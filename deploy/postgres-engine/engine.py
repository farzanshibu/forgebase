#!/usr/bin/env python3
"""Engine build helper shared by CI and local runs (FB-130, ADR-0028 §2.2).

Subcommands:
  get KEY                      print a value of engine.lock (dotted key; lists space-joined)
  fetch --dest DIR --os OS     download the sources OS needs, each checked against its SHA-256
  licences --os OS --into DIR  copy the licence files OS ships; fail if one is missing
  pack SRC OUT --target T      pack an engine directory into a ForgeBase engine archive
  extract ARCHIVE DEST         verify an archive against its manifest and extract it
  lock-fragment ARCHIVE...     print the [archives."<target>"] tables for engine.lock

The archive format is the one crates/forgebase-pgengine reads (ADR-0028,
Addendum FB-132): tar + zstd, `forgebase-engine.json` (format 1) as the first
entry listing every file (path, size, sha256, mode) and every link. `pack` is
deterministic: sorted entries, GNU headers with mtime 0 and uid/gid 0, and
the zstd CLI of the pinned zstd build in single-thread mode, so the same tree
gives the same archive bytes. `extract` applies the same checks as
`forgebase_pgengine::archive::extract` and relocates Linux executables the
same way (an in-place rewrite of the PT_INTERP placeholder).
"""

from __future__ import annotations

import argparse
import hashlib
import io
import json
import os
import shutil
import stat
import struct
import subprocess
import sys
import tarfile
import time
import tomllib
import urllib.request
from pathlib import Path, PurePosixPath

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
MANIFEST_NAME = "forgebase-engine.json"
MANIFEST_FORMAT = 1
PLACEHOLDER_PREFIX = "/forgebase-pg-loader-"
MAX_INTERP_LEN = 254


def die(msg: str) -> None:
    print(f"engine.py: {msg}", file=sys.stderr)
    sys.exit(1)


def load_lock(path: str | None) -> dict:
    p = Path(path or os.environ.get("FORGEBASE_PG_ENGINE_LOCK") or ROOT / "engine.lock")
    with p.open("rb") as f:
        return tomllib.load(f)


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


# ---------------------------------------------------------------- lock values

def cmd_get(args) -> None:
    node = load_lock(args.lock)
    for part in args.key.split("."):
        if not isinstance(node, dict) or part not in node:
            die(f"engine.lock has no {args.key}")
        node = node[part]
    if isinstance(node, list):
        print(" ".join(str(x) for x in node))
    elif isinstance(node, dict):
        die(f"{args.key} is a table")
    else:
        print(node)


def sources_for(lock: dict, os_name: str) -> dict:
    return {k: v for k, v in lock.get("sources", {}).items() if os_name in v.get("os", [])}


def cmd_fetch(args) -> None:
    lock = load_lock(args.lock)
    dest = Path(args.dest)
    dest.mkdir(parents=True, exist_ok=True)
    for name, src in sources_for(lock, args.os).items():
        url, want = src["url"], src["sha256"].lower()
        file = dest / f"{name}-{src['version']}{''.join(PurePosixPath(url).suffixes[-2:])}"
        if file.exists() and sha256_file(file) == want:
            print(f"cached   {file.name}")
            continue
        tmp = file.with_name(file.name + ".part")
        print(f"download {url}")
        req = urllib.request.Request(url, headers={"User-Agent": "forgebase-engine-build"})
        for attempt in range(4):
            try:
                with urllib.request.urlopen(req, timeout=120) as r, tmp.open("wb") as f:
                    shutil.copyfileobj(r, f, 1 << 20)
                break
            except OSError as e:
                if attempt == 3:
                    die(f"download {url}: {e}")
                # Back off: engine-pg18.6-r3 lost an aarch64 build to a network
                # outage that four instant retries all hit.
                wait = 5 * 3 ** attempt
                print(f"  retry in {wait}s after: {e}")
                time.sleep(wait)
        got = sha256_file(tmp)
        if got != want:
            tmp.unlink()
            die(f"{url}: SHA-256 {got} does not match engine.lock ({want})")
        tmp.replace(file)
        print(f"verified {file.name} {got}")
    # The file each source landed in, for the build scripts.
    for name, src in sources_for(lock, args.os).items():
        suffix = "".join(PurePosixPath(src["url"]).suffixes[-2:])
        print(f"SRC_{name.upper()}={dest / (name + '-' + src['version'] + suffix)}", file=sys.stderr)


def cmd_licences(args) -> None:
    lock = load_lock(args.lock)
    lic_dir = ROOT / "THIRD_PARTY_LICENSES" / "postgres"
    out = Path(args.into) if args.into else None
    if out:
        out.mkdir(parents=True, exist_ok=True)
    missing = []
    entries = {**lock.get("sources", {}), **lock.get("bundled", {})}
    for name, e in sorted(entries.items()):
        if args.os != "all" and args.os not in e.get("os", []):
            continue
        f = lic_dir / e.get("licence_file", f"{name}.txt")
        if not f.is_file() or f.stat().st_size < 200:
            missing.append(f"{name}: {f.relative_to(ROOT)}")
            continue
        if out:
            shutil.copyfile(f, out / f.name)
    extra = lic_dir / "README.md"
    if out and extra.is_file():
        shutil.copyfile(extra, out / "README.md")
    if missing:
        die("licence files missing or empty:\n  " + "\n  ".join(missing))
    print(f"licences ok for {args.os}" + (f", copied to {out}" if out else ""))


# ---------------------------------------------------------------- pack

def collect(src: Path, rel: str, out: list) -> None:
    p = src / rel
    if p.is_symlink() or not p.is_dir():
        out.append(rel)
        return
    for child in sorted(os.listdir(p)):
        collect(src, f"{rel}/{child}", out)


def link_stays_inside(key: str, target: str) -> bool:
    if target.startswith(("/", "\\")) or (len(target) > 1 and target[1] == ":"):
        return False
    depth = key.split("/")[:-1]
    for part in target.replace("\\", "/").split("/"):
        if part in ("", "."):
            continue
        if part == "..":
            if not depth:
                return False
            depth.pop()
        else:
            depth.append(part)
    return True


def rel_key(p: str) -> str:
    parts = []
    for part in PurePosixPath(p.replace("\\", "/")).parts:
        if part in ("", "."):
            continue
        if part == ".." or part.startswith("/"):
            raise ValueError(f"path {p!r} must be relative and stay inside")
        parts.append(part)
    if not parts:
        raise ValueError(f"empty path {p!r}")
    return "/".join(parts)


def zstd_cli(explicit: str | None) -> str:
    z = explicit or os.environ.get("FB_ZSTD") or shutil.which("zstd")
    if not z:
        die("no zstd program: pass --zstd (the pinned build's programs/zstd)")
    return z


def cmd_pack(args) -> None:
    src = Path(args.src).resolve()
    items: list[str] = []
    for top in args.paths or ["bin", "lib", "share"]:
        if (src / top).exists():
            collect(src, rel_key(top), items)
    items = sorted(set(items))
    files, links = [], []
    for key in items:
        p = src / key
        if p.is_symlink():
            target = os.readlink(p)
            if not link_stays_inside(key, target):
                die(f"link {key} -> {target} leaves the engine directory")
            links.append({"path": key, "target": target})
        else:
            st = p.stat()
            mode = stat.S_IMODE(st.st_mode) & 0o777 if os.name != "nt" else (0o755 if key.startswith("bin/") or key.endswith((".dll", ".exe")) else 0o644)
            files.append({"path": key, "size": st.st_size, "sha256": sha256_file(p), "mode": mode})
    features = {}
    for kv in args.feature or []:
        k, _, v = kv.partition("=")
        features[k] = {"true": True, "false": False}.get(v, v)
    manifest = {
        "format": MANIFEST_FORMAT,
        "postgres": args.postgres,
        "build": args.build,
        "target": args.target,
    }
    if args.loader:
        if not any(f["path"] == rel_key(args.loader) for f in files):
            die(f"loader {args.loader} is not in the engine")
        manifest["loader"] = rel_key(args.loader)
    manifest["features"] = dict(sorted(features.items()))
    manifest["files"] = files
    manifest["links"] = links
    mbytes = json.dumps(manifest, indent=2).encode()

    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w", format=tarfile.GNU_FORMAT) as tar:
        def add(name: str, data_or_path, size: int, mode: int, link: str | None = None) -> None:
            ti = tarfile.TarInfo(name)
            ti.mtime, ti.uid, ti.gid, ti.uname, ti.gname = 0, 0, 0, "", ""
            ti.mode = mode
            if link is not None:
                ti.type, ti.linkname, ti.size = tarfile.SYMTYPE, link, 0
                tar.addfile(ti)
                return
            ti.size = size
            if isinstance(data_or_path, bytes):
                tar.addfile(ti, io.BytesIO(data_or_path))
            else:
                with open(data_or_path, "rb") as f:
                    tar.addfile(ti, f)
        add(MANIFEST_NAME, mbytes, len(mbytes), 0o644)
        for f in files:
            add(f["path"], src / f["path"], f["size"], f["mode"])
        for l in links:
            add(l["path"], None, 0, 0o777, link=l["target"])
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    tmp = out.with_name(out.name + ".part")
    if out.suffix == ".tar":
        # The uncompressed tar, for a caller that runs zstd itself
        # (build-windows.ps1: Python could not start the zstd CLI there).
        tmp.write_bytes(buf.getvalue())
        tmp.replace(out)
        print(f"tar {out} ({len(buf.getvalue())} bytes, {len(files)} files, {len(links)} links)")
        return
    z = zstd_cli(args.zstd)
    try:
        subprocess.run([z, "-q", "-f", f"-{args.level}", "--single-thread", "-o", str(tmp)],
                       input=buf.getvalue(), check=True)
    except OSError as e:
        die(f"cannot run zstd {z!r} (exists: {os.path.exists(z)}, cwd {os.getcwd()!r}): {e}")
    tmp.replace(out)
    sha = sha256_file(out)
    (out.with_name(out.name + ".sha256")).write_text(f"{sha}  {out.name}\n")
    total = sum(f["size"] for f in files)
    print(f"{sha}  {out}")
    print(f"files {len(files)} links {len(links)} unpacked {total} bytes archive {out.stat().st_size} bytes",
          file=sys.stderr)


# ---------------------------------------------------------------- extract

def cmd_compress(args) -> None:
    """zstd-compress a file with the `zstandard` module (pip). The Windows build
    uses it when the zstd CLI cannot be started (engine-pg18.6-r3..r5)."""
    import zstandard
    src, out = Path(args.src), Path(args.out)
    tmp = out.with_name(out.name + ".part")
    params = zstandard.ZstdCompressionParameters.from_level(args.level, threads=0)
    data = src.read_bytes()
    tmp.write_bytes(zstandard.ZstdCompressor(compression_params=params).compress(data))
    tmp.replace(out)
    print(f"compressed {src} -> {out} ({len(data)} -> {out.stat().st_size} bytes, zstandard {zstandard.__version__})")


def decompress(path: Path, zstd: str | None) -> bytes:
    if path.suffix == ".tar":  # already decompressed (the Windows smoke test)
        return path.read_bytes()
    try:
        import zstandard  # optional (pip): no subprocess
        return zstandard.ZstdDecompressor().stream_reader(path.read_bytes()).read()
    except ImportError:
        pass
    try:
        from compression import zstd as zmod  # Python 3.14+
        return zmod.decompress(path.read_bytes())
    except ImportError:
        return subprocess.run([zstd_cli(zstd), "-dc", "-q", str(path)], check=True,
                              stdout=subprocess.PIPE).stdout


def elf_interp_range(data: bytes):
    """(offset, size) of the PT_INTERP contents of an ELF file, or None."""
    if data[:4] != b"\x7fELF":
        return None
    cls, enc = data[4], data[5]
    e = "<" if enc == 1 else ">"
    if cls == 2:
        phoff, = struct.unpack_from(e + "Q", data, 0x20)
        phentsize, phnum = struct.unpack_from(e + "HH", data, 0x36)
        for i in range(phnum):
            o = phoff + i * phentsize
            p_type, = struct.unpack_from(e + "I", data, o)
            if p_type == 3:
                off, = struct.unpack_from(e + "Q", data, o + 8)
                size, = struct.unpack_from(e + "Q", data, o + 32)
                return off, size
    elif cls == 1:
        phoff, = struct.unpack_from(e + "I", data, 0x1C)
        phentsize, phnum = struct.unpack_from(e + "HH", data, 0x2A)
        for i in range(phnum):
            o = phoff + i * phentsize
            p_type, off, _, _, size = struct.unpack_from(e + "IIIII", data, o)
            if p_type == 3:
                return off, size
    return None


def patch_interp(path: Path, new: str, prefix: str) -> bool:
    data = bytearray(path.read_bytes())
    rng = elf_interp_range(bytes(data[:1 << 16]) if len(data) > 1 << 16 else bytes(data))
    if rng is None:
        return False
    off, size = rng
    seg = bytes(data[off:off + size])
    current = seg.split(b"\0", 1)[0].decode()
    if current == new:
        return False
    if not current.startswith(prefix):
        die(f"{path}: interpreter {current!r} is not the relocatable placeholder ({prefix}...)")
    if len(new) + 1 > size:
        die(f"{path}: loader path {new!r} is {len(new)} bytes, the segment holds {size - 1}")
    data[off:off + size] = new.encode().ljust(size, b"\0")
    mode = path.stat().st_mode
    os.chmod(path, mode | stat.S_IWUSR)
    path.write_bytes(bytes(data))
    os.chmod(path, mode)
    return True


def cmd_extract(args) -> None:
    archive = Path(args.archive)
    raw = decompress(archive, args.zstd)
    dest = Path(args.dest)
    if dest.exists():
        die(f"{dest} exists")
    partial = dest.with_name(dest.name + ".partial")
    shutil.rmtree(partial, ignore_errors=True)
    partial.mkdir(parents=True)
    with tarfile.open(fileobj=io.BytesIO(raw), mode="r:") as tar:
        members = tar.getmembers()
        man_member = next((m for m in members if rel_key(m.name).split("/")[-1] == MANIFEST_NAME
                           and rel_key(m.name).count("/") <= 1), None)
        if man_member is None:
            die(f"{archive} has no {MANIFEST_NAME}")
        mbytes = tar.extractfile(man_member).read()
        manifest = json.loads(mbytes)
        if manifest.get("format") != MANIFEST_FORMAT:
            die(f"manifest format {manifest.get('format')} is not {MANIFEST_FORMAT}")
        key = rel_key(man_member.name)
        prefix = key[: -len(MANIFEST_NAME)]
        files = {rel_key(f["path"]): f for f in manifest["files"]}
        links = {rel_key(l["path"]): l for l in manifest.get("links", [])}
        for l in links.values():
            if not link_stays_inside(rel_key(l["path"]), l["target"]):
                die(f"link {l['path']} -> {l['target']} leaves the engine")
        seen = set()
        for m in members:
            full = rel_key(m.name)
            if prefix:
                if full + "/" == prefix:
                    continue
                if not full.startswith(prefix):
                    die(f"{m.name} lies outside {prefix}")
                full = full[len(prefix):]
            out = partial / full
            if m.isdir():
                out.mkdir(parents=True, exist_ok=True)
                continue
            if m.isfile():
                data = tar.extractfile(m).read()
                if full == MANIFEST_NAME:
                    if data != mbytes:
                        die("the archive holds two different manifests")
                    out.write_bytes(data)
                    continue
                want = files.get(full)
                if want is None:
                    die(f"{full} is not in the manifest")
                if full in seen:
                    die(f"{full} appears twice")
                seen.add(full)
                if len(data) != want["size"] or hashlib.sha256(data).hexdigest() != want["sha256"].lower():
                    die(f"{full} does not match the manifest")
                out.parent.mkdir(parents=True, exist_ok=True)
                out.write_bytes(data)
                os.chmod(out, want["mode"] & 0o777 | 0o200)
            elif m.issym():
                want = links.get(full)
                if want is None or want["target"] != m.linkname:
                    die(f"link {full} -> {m.linkname} is not in the manifest")
                seen.add(full)
                out.parent.mkdir(parents=True, exist_ok=True)
                os.symlink(m.linkname, out)
            else:
                die(f"{full}: entry type {m.type!r} is not allowed")
        missing = (set(files) | set(links)) - seen
        if missing:
            die(f"the archive lacks {sorted(missing)[:5]}")
    loader = manifest.get("loader")
    patched = 0
    if loader:
        loader_abs = args.loader_path or str(dest.resolve() / rel_key(loader))
        if len(loader_abs) > MAX_INTERP_LEN:
            die(f"loader path {loader_abs} is longer than {MAX_INTERP_LEN} bytes")
        placeholder = manifest.get("interp_placeholder") or PLACEHOLDER_PREFIX
        for k in files:
            if k.startswith("bin/") and patch_interp(partial / k, loader_abs, placeholder):
                patched += 1
    for k, f in files.items():
        os.chmod(partial / k, (f["mode"] & 0o777 & ~0o222) | 0o400)
    partial.rename(dest)
    print(f"extracted {len(files)} files, {len(links)} links to {dest}"
          + (f"; relocated {patched} executables to {loader_abs}" if loader else ""))


def cmd_lock_fragment(args) -> None:
    for a in args.archives:
        p = Path(a)
        raw = decompress(p, args.zstd)
        with tarfile.open(fileobj=io.BytesIO(raw), mode="r:") as tar:
            m = next(x for x in tar.getmembers() if x.name.endswith(MANIFEST_NAME))
            target = json.loads(tar.extractfile(m).read())["target"]
        print(f'[archives."{target}"]\nfile = "{p.name}"\nsha256 = "{sha256_file(p)}"\n')


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--lock", help="engine.lock (default: repository root or $FORGEBASE_PG_ENGINE_LOCK)")
    sub = ap.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("get"); s.add_argument("key"); s.set_defaults(fn=cmd_get)
    s = sub.add_parser("fetch"); s.add_argument("--dest", required=True)
    s.add_argument("--os", required=True, choices=["linux", "macos", "windows"]); s.set_defaults(fn=cmd_fetch)
    s = sub.add_parser("licences"); s.add_argument("--os", required=True, choices=["linux", "macos", "windows", "all"])
    s.add_argument("--into"); s.set_defaults(fn=cmd_licences)
    s = sub.add_parser("pack"); s.add_argument("src"); s.add_argument("out")
    s.add_argument("--postgres", required=True); s.add_argument("--build", required=True)
    s.add_argument("--target", required=True); s.add_argument("--loader")
    s.add_argument("--feature", action="append", help="key=value for the manifest's features map")
    s.add_argument("--zstd"); s.add_argument("--level", type=int, default=19)
    s.add_argument("paths", nargs="*"); s.set_defaults(fn=cmd_pack)
    s = sub.add_parser("compress"); s.add_argument("src"); s.add_argument("out")
    s.add_argument("--level", type=int, default=19); s.set_defaults(fn=cmd_compress)
    s = sub.add_parser("extract"); s.add_argument("archive"); s.add_argument("dest")
    s.add_argument("--loader-path", help="absolute loader path to write (default: <dest>/<loader>)")
    s.add_argument("--zstd"); s.set_defaults(fn=cmd_extract)
    s = sub.add_parser("lock-fragment"); s.add_argument("archives", nargs="+"); s.add_argument("--zstd")
    s.set_defaults(fn=cmd_lock_fragment)
    args = ap.parse_args()
    args.fn(args)


if __name__ == "__main__":
    main()
