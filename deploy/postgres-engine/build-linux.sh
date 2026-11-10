#!/usr/bin/env bash
# Builds a Linux (musl) engine inside the pinned Alpine image, on a host with
# Docker whose architecture matches the target (no emulation).
#
#   deploy/postgres-engine/build-linux.sh <x86_64|aarch64>-unknown-linux-musl <work dir> <out dir>
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
TARGET=${1:?target}
WORK=$(mkdir -p "$2" && cd "$2" && pwd)
OUT=$(mkdir -p "$3" && cd "$3" && pwd)
IMAGE=$(python3 "$HERE/engine.py" --lock "$ROOT/engine.lock" get engine.alpine_image)
# Build tools only (PostgreSQL 17+ tarballs need perl, bison and flex); every library the engine links is built from a pinned
# source except musl itself and ncurses (static, for libedit), which come
# from the image and are listed under [bundled] in engine.lock.
PACKAGES="bash build-base perl bison flex linux-headers ncurses-dev ncurses-static python3 file patch"
docker run --rm \
  -v "$ROOT:/src:ro" -v "$WORK:/work" -v "$OUT:/out" \
  -e JOBS="${JOBS:-$(nproc)}" -e FB_ENGINE_SOURCES=/work/src -e FB_ENGINE_STEPS="${FB_ENGINE_STEPS:-}" \
  "$IMAGE" sh -euc "
    apk add --no-cache $PACKAGES >/dev/null
    apk info -v musl ncurses-static
    /src/deploy/postgres-engine/build-unix.sh $TARGET /work/build /out
    chown -R $(id -u):$(id -g) /work /out"
