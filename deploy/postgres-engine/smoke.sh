#!/bin/sh
# Smoke test of an extracted (and, on Linux, relocated) engine directory
# (ADR-0028 §2.2, ADR-0029 §1.3). POSIX sh: runs on macOS, glibc distros,
# Alpine and anything else with /bin/sh.
#
#   deploy/postgres-engine/smoke.sh <engine dir> <work dir>
#
# initdb; start with the FB-132 preload settings; CREATE EXTENSION for every
# shipped extension; an HNSW vector query; pg_trgm; pgmq send/read; a pg_cron
# job that fires; TLS library check; pg_dump/pg_restore; pg_basebackup,
# pg_verifybackup and a hot standby started from the backup; stop. Prints
# idle memory (RSS) of the primary.
#
# <work dir> must be short: the Unix socket path is limited to ~103 bytes.
# Env: PORT (default 54329), SMOKE_SKIP_STANDBY=1.
set -eu
E=$(cd "$1" && pwd)
mkdir -p "$2"
W=$(cd "$2" && pwd)
BIN=$E/bin
PORT=${PORT:-54329}
PORT2=$((PORT + 1))
DATA=$W/data
SOCK=$W/s
LOG=$W/primary.log
mkdir -p "$SOCK"
rm -rf "$DATA" "$W/standby" "$W/dump"

say() { printf '\n--- %s\n' "$*"; }
q() { "$BIN/psql" -X -q -A -t -v ON_ERROR_STOP=1 -h "$SOCK" -p "$PORT" -U postgres -d postgres "$@"; }
fail() { echo "SMOKE FAIL: $*" >&2; [ -f "$LOG" ] && tail -30 "$LOG" >&2; stop_all; exit 1; }
stop_all() {
  [ -f "$W/standby/postmaster.pid" ] && "$BIN/pg_ctl" -D "$W/standby" -m fast -w stop >/dev/null 2>&1 || true
  [ -f "$DATA/postmaster.pid" ] && "$BIN/pg_ctl" -D "$DATA" -m fast -w stop >/dev/null 2>&1 || true
}
trap 'stop_all' INT TERM

say "programs run"
for p in "$BIN"/*; do
  case "$(basename "$p")" in *.dll) continue ;; esac
  out=$("$p" --version 2>&1) || fail "$(basename "$p") --version: $out"
  echo "  $(basename "$p"): $out"
done

say "initdb"
TZ=UTC "$BIN/initdb" -D "$DATA" -U postgres --auth=trust --encoding=UTF8 \
  --locale-provider=builtin --builtin-locale=C.UTF-8 --locale=C --no-instructions >"$W/initdb.log" 2>&1 \
  || { cat "$W/initdb.log"; fail initdb; }
cat >>"$DATA/postgresql.conf" <<EOF
listen_addresses = ''
port = $PORT
unix_socket_directories = '$SOCK'
shared_preload_libraries = 'pg_stat_statements,pg_cron'
cron.database_name = 'postgres'
cron.use_background_workers = on
max_worker_processes = 16
wal_level = logical
max_wal_senders = 16
max_replication_slots = 16
max_prepared_transactions = 16
shared_buffers = 128MB
fsync = off
EOF

say "start"
"$BIN/pg_ctl" -D "$DATA" -l "$LOG" -w -t 60 start >/dev/null || fail "start"

say "extensions"
for ext in vector pg_trgm pgmq pg_cron citext pgcrypto unaccent fuzzystrmatch btree_gin btree_gist \
           hstore ltree intarray tablefunc pg_stat_statements amcheck; do
  q -c "CREATE EXTENSION \"$ext\"" || fail "CREATE EXTENSION $ext"
done
q -c "SELECT extname || ' ' || extversion FROM pg_extension ORDER BY 1" | sed 's/^/  /'

say "vector (HNSW)"
q <<'SQL' || fail "vector"
CREATE TABLE items (id bigint PRIMARY KEY, embedding vector(3));
INSERT INTO items VALUES (1, '[1,2,3]'), (2, '[4,5,6]'), (3, '[7,8,9]');
CREATE INDEX ON items USING hnsw (embedding vector_l2_ops);
SQL
got=$(q -c "SET enable_seqscan = off; SELECT id FROM items ORDER BY embedding <-> '[3,1,2]' LIMIT 1")
[ "$got" = 1 ] || fail "vector query returned '$got'"
plan=$(q -c "SET enable_seqscan = off; EXPLAIN SELECT id FROM items ORDER BY embedding <-> '[3,1,2]' LIMIT 1")
echo "$plan" | grep -q items_embedding_idx || fail "HNSW index not used: $plan"
echo "  nearest = $got, plan uses the HNSW index"

say "pg_trgm, pgcrypto, unaccent"
[ "$(q -c "SELECT similarity('forgebase', 'forgebas') > 0.5")" = t ] || fail "pg_trgm"
q -c "SELECT encode(digest('x', 'sha256'), 'hex')" | grep -q '^2d711642' || fail "pgcrypto"
[ "$(q -c "SELECT unaccent('Crème')")" = Creme ] || fail "unaccent"

say "pgmq"
q -c "SELECT pgmq.create('smoke')" >/dev/null || fail "pgmq.create"
q -c "SELECT pgmq.send('smoke', '{\"a\": 1}')" >/dev/null || fail "pgmq.send"
[ "$(q -c "SELECT message->>'a' FROM pgmq.read('smoke', 30, 1)")" = 1 ] || fail "pgmq.read"

say "pg_cron (a job must fire within 70 s)"
q -c "CREATE TABLE cron_hits (at timestamptz)"
q -c "SELECT cron.schedule('smoke', '2 seconds', 'INSERT INTO cron_hits VALUES (now())')" >/dev/null || fail "cron.schedule"
i=0
until [ "$(q -c 'SELECT count(*) > 0 FROM cron_hits')" = t ]; do
  i=$((i + 1)); [ $i -le 70 ] || fail "pg_cron job did not fire"
  sleep 1
done
echo "  fired after ~${i}s"
q -c "SELECT cron.unschedule('smoke')" >/dev/null

say "TLS library and settings"
q -c "SHOW ssl_library" | sed 's/^/  ssl_library = /'
q -c "SELECT current_setting('server_version'), current_setting('data_checksums')" | sed 's/^/  /'

say "idle memory"
sleep 5
pm=$(head -1 "$DATA/postmaster.pid")
# ps -o rss is in KiB on macOS and Linux (procps and busybox).
rss=$(ps -A -o pid= -o ppid= -o rss= 2>/dev/null | awk -v pm="$pm" '$1 == pm || $2 == pm { s += $3; n++ } END { print s " " n }') || rss=""
if [ -n "$rss" ]; then
  set -- $rss
  echo "  idle RSS: $1 KiB in $2 processes (postmaster $(ps -o rss= -p "$pm" | tr -d ' ') KiB)"
  echo "IDLE_RSS_KIB=$1" >"$W/measurements"
fi

say "pg_dump / pg_restore"
"$BIN/pg_dump" -h "$SOCK" -p "$PORT" -U postgres -d postgres -Fc -f "$W/dump" || fail "pg_dump"
"$BIN/pg_restore" -l "$W/dump" | grep -q 'TABLE public items' || fail "pg_restore -l"

if [ "${SMOKE_SKIP_STANDBY:-0}" != 1 ]; then
  say "pg_basebackup, pg_verifybackup, hot standby"
  "$BIN/pg_basebackup" -h "$SOCK" -p "$PORT" -U postgres -D "$W/standby" -R -X stream -c fast \
    || fail "pg_basebackup"
  "$BIN/pg_verifybackup" "$W/standby" >/dev/null || fail "pg_verifybackup"
  "$BIN/pg_ctl" -D "$W/standby" -l "$W/standby.log" -o "-p $PORT2" -w -t 60 start >/dev/null \
    || { tail -20 "$W/standby.log"; fail "standby start"; }
  q -c "INSERT INTO items VALUES (4, '[1,1,1]')"
  i=0
  until [ "$("$BIN/psql" -X -A -t -h "$SOCK" -p "$PORT2" -U postgres -d postgres -c 'SELECT count(*) FROM items')" = 4 ]; do
    i=$((i + 1)); [ $i -le 30 ] || fail "standby did not replay"
    sleep 1
  done
  [ "$("$BIN/psql" -X -A -t -h "$SOCK" -p "$PORT2" -U postgres -d postgres -c 'SELECT pg_is_in_recovery()')" = t ] \
    || fail "standby is not in recovery"
  echo "  standby replays and answers vector queries: $("$BIN/psql" -X -A -t -h "$SOCK" -p "$PORT2" -U postgres -d postgres \
    -c "SELECT id FROM items ORDER BY embedding <-> '[1,1,1]' LIMIT 1")"
  "$BIN/pg_ctl" -D "$W/standby" -m fast -w stop >/dev/null || fail "standby stop"
fi

say "stop"
"$BIN/pg_ctl" -D "$DATA" -m fast -w stop >/dev/null || fail "stop"
# Background workers log FATAL "... due to administrator command" on a fast
# shutdown; anything else at FATAL or PANIC is a failure.
if grep -E 'PANIC|FATAL' "$LOG" | grep -v 'due to administrator command'; then
  fail "the log has PANIC/FATAL lines"
fi
echo
echo "SMOKE OK"
