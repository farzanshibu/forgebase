#!/bin/sh
# Usage: run-load.sh <forgebase binary> <fb-load binary> <empty work dir> [concurrency] [duration] [database]
# Starts a release server in <work dir>, runs the fb-load scenarios, stops the server.
# database: postgres (the bundled PostgreSQL) or sqlite; default: the build's default.
#
# Environment (all optional):
#   SCENARIOS    comma list for fb-load (default: records,queue,realtime,auth,storage,graph,sql)
#   JSON_OUT     write the run's JSON report here (fb-load --json); CI stores and compares it
#   LABEL        label stored in the JSON (default: <database>)
#   PORT         server port (default 18091)
#   SUBSCRIBERS  realtime subscribers (default 30)
#   LOAD_ARGS    more fb-load arguments, e.g. "--graph-nodes 1000 --sql-orders 5000"
# The server's process tree (the bundled PostgreSQL included) and its data directory are
# sampled once a second during every scenario (CPU, RSS, open fds, disk).
set -eu
FB=$1; LOAD=$2; DIR=$3; C=${4:-16}; D=${5:-10}; DB=${6:-}
PORT=${PORT:-18091}
SCENARIOS=${SCENARIOS:-records,queue,realtime,auth,storage,graph,sql}
SUBSCRIBERS=${SUBSCRIBERS:-30}
LABEL=${LABEL:-${DB:-default}}
mkdir -p "$DIR"; cd "$DIR"
if [ -n "$DB" ]; then "$FB" init app --database "$DB" --no-admin >/dev/null; else "$FB" init app --no-admin >/dev/null; fi
cd app
echo "root password 1" | "$FB" admin create --email root@example.com --password-stdin >/dev/null
"$FB" serve --port "$PORT" >../server.log 2>&1 &
PID=$!
trap 'kill $PID 2>/dev/null || true' EXIT
until curl -sf "localhost:$PORT/health/ready" >/dev/null; do sleep 0.2; done
JSON_ARG=""
if [ -n "${JSON_OUT:-}" ]; then JSON_ARG="--json $JSON_OUT"; fi
# shellcheck disable=SC2086
"$LOAD" --addr "127.0.0.1:$PORT" --scenario "$SCENARIOS" --concurrency "$C" --duration "$D" \
  --subscribers "$SUBSCRIBERS" --pid "$PID" --data-dir "$PWD/forgebase-data" \
  --backend "${DB:-default}" --label "$LABEL" $JSON_ARG ${LOAD_ARGS:-}
