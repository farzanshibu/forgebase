#!/usr/bin/env bash
# Linux distro matrix (ADR-0028 §2.2, FB-130 "Decides"): extracts a Linux
# engine archive once, relocates its executables to the path the containers
# see (/opt/fbpg), and runs smoke.sh in Debian 12, Ubuntu 22.04, UBI 9 and
# Alpine 3.20 as an unprivileged user. Distroless has no shell: the cluster
# is created in a Debian container, then postgres and psql run in
# gcr.io/distroless/static-debian12, which has no C library at all.
#
#   deploy/postgres-engine/distros.sh <archive> <work dir>
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
ARCHIVE=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
T=$(mkdir -p "$2" && cd "$2" && pwd)
ARCH=$(uname -m)
E=$T/fbpg
rm -rf "$E" "$T"/w-*
python3 "$HERE/engine.py" extract "$ARCHIVE" "$E" --loader-path "/opt/fbpg/lib/ld-musl-$ARCH.so.1"
cp "$HERE/smoke.sh" "$T/smoke.sh"
uid=$(id -u) gid=$(id -g)
printf 'fbsmoke:x:%s:%s::/tmp:/bin/sh\n' "$uid" "$gid" >"$T/passwd"
printf 'fbsmoke:x:%s:\n' "$gid" >"$T/group"

run() { # <image> <work> <command...>
  local image=$1 w=$2; shift 2
  docker run --rm --user "$uid:$gid" \
    -v "$E:/opt/fbpg:ro" -v "$w:/w" -v "$T/smoke.sh:/smoke.sh:ro" \
    -v "$T/passwd:/etc/passwd:ro" -v "$T/group:/etc/group:ro" "$image" "$@"
}

declare -A IMAGES=(
  [debian-12]=debian:12-slim
  [ubuntu-22.04]=ubuntu:22.04
  [ubi-9]=registry.access.redhat.com/ubi9/ubi-minimal:latest
  [alpine-3.20]=alpine:3.20
)
results=()
for name in debian-12 ubuntu-22.04 ubi-9 alpine-3.20; do
  w=$T/w-$name
  mkdir -p "$w"
  echo "::group::$name"
  if run "${IMAGES[$name]}" "$w" /bin/sh /smoke.sh /opt/fbpg /w; then
    results+=("$name: ok")
  else
    results+=("$name: FAILED")
  fi
  echo "::endgroup::"
done

echo "::group::distroless"
w=$T/w-distroless
mkdir -p "$w/s"
status="distroless: FAILED"
if run debian:12-slim "$w" /opt/fbpg/bin/initdb -D /w/data -U postgres --auth=trust --encoding=UTF8 \
     --locale-provider=builtin --builtin-locale=C.UTF-8 --locale=C --no-instructions; then
  cid=$(docker run -d --user "$uid:$gid" -v "$E:/opt/fbpg:ro" -v "$w:/w" \
    -v "$T/passwd:/etc/passwd:ro" -v "$T/group:/etc/group:ro" \
    gcr.io/distroless/static-debian12:latest /opt/fbpg/bin/postgres -D /w/data \
    -c listen_addresses= -c unix_socket_directories=/w/s -c port=54329 \
    -c shared_preload_libraries=pg_stat_statements,pg_cron -c cron.database_name=postgres)
  ok=0
  for _ in $(seq 1 30); do
    if docker exec "$cid" /opt/fbpg/bin/pg_isready -h /w/s -p 54329 >/dev/null 2>&1; then ok=1; break; fi
    sleep 1
  done
  if [ $ok = 1 ] && docker exec "$cid" /opt/fbpg/bin/psql -X -v ON_ERROR_STOP=1 -h /w/s -p 54329 -U postgres -d postgres \
       -c "CREATE EXTENSION vector" -c "CREATE EXTENSION pg_cron" -c "CREATE EXTENSION pgmq" -c "CREATE EXTENSION pg_trgm" \
       -c "SELECT '[1,2,3]'::vector <-> '[3,2,1]'" -c "SHOW ssl_library"; then
    status="distroless: ok"
  fi
  docker logs "$cid" 2>&1 | tail -20
  docker exec "$cid" /opt/fbpg/bin/pg_ctl -D /w/data -m fast -w stop || true
  docker rm -f "$cid" >/dev/null
fi
results+=("$status")
echo "::endgroup::"

printf '%s\n' "${results[@]}" | tee "$T/distros.txt"
! grep -q FAILED "$T/distros.txt"
