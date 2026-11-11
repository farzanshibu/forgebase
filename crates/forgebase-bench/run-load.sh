#!/bin/sh
# Usage: run-load.sh <forgebase binary> <fb-load binary> <empty work dir> [concurrency] [duration] [database]
# Starts a release server in <work dir>, runs every fb-load scenario, stops the server.
# database: postgres (the bundled PostgreSQL) or sqlite; default: the build's default.
set -eu
FB=$1; LOAD=$2; DIR=$3; C=${4:-16}; D=${5:-10}; DB=${6:-}
mkdir -p "$DIR"; cd "$DIR"
if [ -n "$DB" ]; then "$FB" init app --database "$DB" --no-admin >/dev/null; else "$FB" init app --no-admin >/dev/null; fi
cd app
echo "root password 1" | "$FB" admin create --email root@example.com --password-stdin >/dev/null
"$FB" serve --port 18091 >../server.log 2>&1 &
PID=$!
trap 'kill $PID 2>/dev/null || true' EXIT
until curl -sf localhost:18091/health/ready >/dev/null; do sleep 0.2; done
for s in records queue realtime auth; do
  "$LOAD" --addr 127.0.0.1:18091 --scenario $s --concurrency "$C" --duration "$D" --subscribers 30
done
