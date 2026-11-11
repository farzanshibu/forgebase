#!/bin/sh
# Apache AGE post-install hook (FB-160), run by build-unix.sh after
# `make install` of AGE into the staged engine.
# Env from build-unix.sh: STAGED (staged install root), OS, TARGET, DLL
# (so|dylib), EXT_DIR (the patched AGE source tree).
#
# 1. Every patch in patches/ is pinned in engine.lock ([[sources.age.patches]],
#    same order, same SHA-256), so a changed or unlisted patch fails the build
#    before anything is packed.
# 2. The licence and NOTICE kept in THIRD_PARTY_LICENSES/postgres/age/ are the
#    upstream files byte for byte; the NOTICE ships as
#    share/licenses/age-NOTICE.txt (age.txt is copied by engine.py licences).
# 3. lib/plugins/age.<DLL> -> ../age.<DLL>: non-superuser roles may preload
#    only from $libdir/plugins/ (ForgeBase sets session_preload_libraries =
#    '$libdir/plugins/age' per role, ADR-0029 §1.2). A link, not a copy:
#    PostgreSQL recognises an already loaded library by inode, so
#    '$libdir/age' (the extension's module_pathname) and
#    '$libdir/plugins/age' are the same loaded module; two copies would be
#    loaded twice and install AGE's hooks twice in one backend.
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)
: "${STAGED:?}" "${DLL:?}" "${EXT_DIR:?}"

"${PYTHON:-python3}" - "$ROOT/engine.lock" "$HERE/patches" <<'PY'
import hashlib, sys, tomllib
from pathlib import Path
lock = tomllib.load(open(sys.argv[1], "rb"))
pinned = lock["sources"]["age"].get("patches", [])
on_disk = sorted(p.name for p in Path(sys.argv[2]).glob("*.patch"))
names = [p["file"] for p in pinned]
if names != on_disk:
    sys.exit(f"age patches: engine.lock lists {names}, patches/ holds {on_disk}")
for p in pinned:
    got = hashlib.sha256((Path(sys.argv[2]) / p["file"]).read_bytes()).hexdigest()
    if got != p["sha256"]:
        sys.exit(f"age patch {p['file']}: SHA-256 {got} is not the pinned {p['sha256']}")
    print(f"patch ok {p['file']} ({p['upstream_pr']}, {p['upstream_commit'][:12]})")
PY

LIC=$ROOT/THIRD_PARTY_LICENSES/postgres/age
cmp "$EXT_DIR/LICENSE" "$LIC/age.txt" || { echo "age: THIRD_PARTY_LICENSES/postgres/age/age.txt differs from upstream LICENSE" >&2; exit 1; }
cmp "$EXT_DIR/NOTICE" "$LIC/NOTICE" || { echo "age: THIRD_PARTY_LICENSES/postgres/age/NOTICE differs from upstream NOTICE" >&2; exit 1; }
mkdir -p "$STAGED/share/licenses"
cp "$LIC/NOTICE" "$STAGED/share/licenses/age-NOTICE.txt"

[ -f "$STAGED/lib/age.$DLL" ] || { echo "age: $STAGED/lib/age.$DLL was not installed" >&2; exit 1; }
mkdir -p "$STAGED/lib/plugins"
rm -f "$STAGED/lib/plugins/age.$DLL"
ln -s "../age.$DLL" "$STAGED/lib/plugins/age.$DLL"
ls -l "$STAGED/lib/age.$DLL" "$STAGED/lib/plugins/age.$DLL"
