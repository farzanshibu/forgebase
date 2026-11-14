#!/bin/sh
# Usage: run-topology.sh <forgebase binary> <fb-load binary> <empty work dir> [concurrency] [duration] [replicas] [shards] [tenants]
# Sustained mixed load against a bundled-PostgreSQL primary with local read replicas and, optionally,
# shards (ADR-0029). Linux CI runs it (.github/workflows/bench.yml); it also runs on a laptop with
# an engine that has pg_basebackup (FORGEBASE_PG_ENGINE_DIR), with everything on one machine.
#
#   1. init a project on the bundled PostgreSQL, start the server;
#   2. fb-load --scenario topology-setup: adds the replicas and the shards (through
#      /api/system/database/*), waits for them to be in the read rotation;
#   3. restarts the server (it takes replicas up when it opens its router);
#   4. fb-load --scenario topology: writers and readers for <duration> seconds, with the lag of
#      every replica sampled once a second, the share of reads each node served, and the server's
#      CPU, memory, fds and disk.
#
# Environment: JSON_OUT (report file), LABEL, PORT (default 18092), TENANTS (default: 3 per shard,
# 0 without shards), WRITE_PCT (share of workers that write, default 30), SHARD_ROOT (short path
# for shard sockets, default /tmp/fbs.XXXXXX; Unix sockets are limited to 103 bytes),
# FORGEBASE_PG_ENGINE_DIR (the engine for the shards' initdb/pg_ctl and for a binary built
# without the embedded engine).
set -eu
FB=$1; LOAD=$2; DIR=$3; C=${4:-16}; D=${5:-30}; REPLICAS=${6:-2}; SHARDS=${7:-0}
PORT=${PORT:-18092}
WRITE_PCT=${WRITE_PCT:-30}
TENANTS=${TENANTS:-$((SHARDS * 3))}
LABEL=${LABEL:-topology-${REPLICAS}r-${SHARDS}s}
mkdir -p "$DIR"; cd "$DIR"
"$FB" init app --database postgres --no-admin >/dev/null
cd app
echo "root password 1" | "$FB" admin create --email root@example.com --password-stdin >/dev/null

PID=""
SHARD_PIDS=""
start_server() {
  "$FB" serve --port "$PORT" >>../server.log 2>&1 &
  PID=$!
  until curl -sf "localhost:$PORT/health/ready" >/dev/null; do
    kill -0 "$PID" 2>/dev/null || { echo "server exited; see $DIR/server.log" >&2; exit 1; }
    sleep 0.3
  done
}
stop_server() { kill "$PID" 2>/dev/null || true; wait "$PID" 2>/dev/null || true; }
SHARD_ROOT=${SHARD_ROOT:-}
cleanup() {
  stop_server
  if [ "$SHARDS" -gt 0 ] && [ -d "$SHARD_ROOT" ]; then
    for d in "$SHARD_ROOT"/s*/data; do
      [ -d "$d" ] && "$ENGINE/bin/pg_ctl" stop -m immediate -D "$d" >/dev/null 2>&1 || true
    done
    rm -rf "$SHARD_ROOT"
  fi
}
trap cleanup EXIT

SHARD_URLS=""
if [ "$SHARDS" -gt 0 ]; then
  ENGINE=${FORGEBASE_PG_ENGINE_DIR:?set FORGEBASE_PG_ENGINE_DIR to the PostgreSQL engine for the shards}
  SHARD_ROOT=${SHARD_ROOT:-$(mktemp -d /tmp/fbs.XXXXXX)}
  mkdir -p "$SHARD_ROOT"
  i=1
  while [ "$i" -le "$SHARDS" ]; do
    S="$SHARD_ROOT/s$i"; mkdir -p "$S"
    "$ENGINE/bin/initdb" -U postgres -A trust -E UTF8 --locale=C --no-instructions -D "$S/data" >/dev/null
    cat >>"$S/data/postgresql.conf" <<CONF
listen_addresses = ''
unix_socket_directories = '$S'
port = 5432
max_connections = 120
max_prepared_transactions = 16
wal_level = logical
max_wal_senders = 16
max_replication_slots = 16
max_logical_replication_workers = 8
wal_sender_timeout = 0
CONF
    "$ENGINE/bin/pg_ctl" start -w -t 60 -D "$S/data" -l "$S/postgres.log" >/dev/null
    "$ENGINE/bin/psql" -h "$S" -p 5432 -U postgres -d postgres -qc "CREATE DATABASE forgebase" >/dev/null
    URL="postgres://postgres@localhost/forgebase?host=$S&port=5432"
    SHARD_URLS="${SHARD_URLS:+$SHARD_URLS,}$URL"
    i=$((i + 1))
  done
fi

start_server
if [ "$REPLICAS" -gt 0 ] || [ "$SHARDS" -gt 0 ]; then
  "$LOAD" --addr "127.0.0.1:$PORT" --scenario topology-setup --replicas "$REPLICAS" --shard-urls "$SHARD_URLS"
  stop_server
  start_server
fi
JSON_ARG=""
if [ -n "${JSON_OUT:-}" ]; then JSON_ARG="--json $JSON_OUT"; fi
# shellcheck disable=SC2086
"$LOAD" --addr "127.0.0.1:$PORT" --scenario topology --concurrency "$C" --duration "$D" \
  --write-pct "$WRITE_PCT" --tenants "$TENANTS" --pid "$PID" --data-dir "$PWD/forgebase-data" \
  --backend postgres --label "$LABEL" $JSON_ARG
