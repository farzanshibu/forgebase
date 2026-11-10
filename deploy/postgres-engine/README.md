# Bundled PostgreSQL engine build (FB-130)

Builds the PostgreSQL engine archives that `forgebase-pgengine` embeds
(ADR-0028 §2.2–§3.1, ADR-0029 §1.3). CI (`.github/workflows/postgres-engine.yml`)
and local runs use the same scripts. Everything they build is pinned in
`/engine.lock`: versions, upstream URLs and SHA-256 of every source, the
programs and contrib modules shipped, and the SHA-256 of each finished archive.

| File | What it does |
|---|---|
| `engine.py` | `get` (read engine.lock), `fetch` (download + verify), `licences`, `pack`, `extract`, `lock-fragment` |
| `build-unix.sh` | macOS and Linux (musl) build of one target, resumable (stamps in `<work>/stamps`) |
| `build-linux.sh` | runs `build-unix.sh` inside the pinned Alpine image (native architecture) |
| `build-windows.ps1` | Windows build: MSVC, meson, `Makefile.win` for pgvector and pg_cron |
| `smoke.sh`, `smoke-windows.ps1` | smoke test of an extracted engine |
| `distros.sh` | Linux archive on Debian 12, Ubuntu 22.04, UBI 9, Alpine 3.20 and distroless |
| `extensions/` | hook for one more extension with patches (Apache AGE, FB-160) |

## Local build (macOS)

Needs Xcode command line tools and Python 3.11+. Keep the work directory out
of the repository and the socket directory of the smoke test short (Unix
sockets are limited to ~103 bytes).

```sh
deploy/postgres-engine/build-unix.sh aarch64-apple-darwin ~/fb-engine/work ~/fb-engine/out
python3 deploy/postgres-engine/engine.py extract \
  ~/fb-engine/out/forgebase-pg18.6-r1-aarch64-apple-darwin.tar.zst /tmp/fbe/engine
deploy/postgres-engine/smoke.sh /tmp/fbe/engine /tmp/fbe/w
```

`FORGEBASE_PG_ENGINE_DIR=/tmp/fbe/engine` then runs the `forgebase-pgengine`
integration tests against it, and `FORGEBASE_PG_ENGINE_ARCHIVE=<archive>`
with `--features bundled-postgres` embeds it (its SHA-256 must be in
`engine.lock`).

Linux engines: `deploy/postgres-engine/build-linux.sh x86_64-unknown-linux-musl <work> <out>`
on an x86_64 host with Docker (aarch64 on an arm64 host).

## What the archive holds

`forgebase-pg<version>-<build>-<target>.tar.zst`: `forgebase-engine.json`
(format 1: every file with size, SHA-256 and mode; links; the Linux `loader`;
`features` with the extension versions and `graph_cypher`), then `bin/`
(the programs in `contents.programs`), `lib/` (libpq, OpenSSL, server
modules, contrib, pgvector, pg_cron; on Linux also `ld-musl-<arch>.so.1`),
`share/` (catalog files, time zones, text search data, extension SQL,
`licenses/`). The packer is deterministic (sorted entries, mtime 0, uid 0,
the pinned zstd at level 19 in single-thread mode), and the build sees its
work directory through `/tmp/forgebase-engine-<target>`, so the same sources
and toolchain give the same archive bytes wherever the work directory is.

Relocation: macOS binaries name libraries as `@loader_path/../lib/…` and are
ad-hoc signed again; Linux executables carry the 255-byte placeholder
interpreter `/forgebase-pg-loader-xxx…` (rewritten in place at extraction)
and `RUNPATH=$ORIGIN/../lib`; Windows DLLs sit next to the executables.
PostgreSQL finds `share/` and `lib/` relative to its executable.

## Adding an extension (FB-160)

See `extensions/README.md`.
