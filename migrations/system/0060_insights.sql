-- ADR-0036: operations insights.
--
-- Statement statistics: one row per (flush bucket, statement fingerprint,
-- backend). The fingerprint is a hash of the statement text with every value
-- removed; `statement` is that reduced text (never a literal). Written every
-- `[observability] flush_interval_secs` from memory; at most
-- `query_stats_top_n` rows per bucket, the busiest by `total_us`. `sketch`
-- is the latency sketch as sparse JSON `[[bucket, count], ...]`
-- (forgebase_core::sketch); sketches of several buckets add up, which is how a
-- time range gets its p50/p95/p99. Pruned after `[observability] retention_days`.
CREATE TABLE _forgebase_stmt_stats (
    bucket_start INTEGER NOT NULL,
    bucket_ms    INTEGER NOT NULL,
    fingerprint  TEXT    NOT NULL,
    backend      TEXT    NOT NULL,
    area         TEXT    NOT NULL,
    statement    TEXT    NOT NULL,
    calls        INTEGER NOT NULL,
    errors       INTEGER NOT NULL DEFAULT 0,
    writes       INTEGER NOT NULL DEFAULT 0,
    slow         INTEGER NOT NULL DEFAULT 0,
    total_us     INTEGER NOT NULL,
    max_us       INTEGER NOT NULL,
    min_us       INTEGER NOT NULL DEFAULT 0,
    rows         INTEGER NOT NULL DEFAULT 0,
    sketch       TEXT    NOT NULL DEFAULT '[]',
    PRIMARY KEY (bucket_start, fingerprint, backend)
) STRICT;
CREATE INDEX _forgebase_stmt_stats_fingerprint ON _forgebase_stmt_stats (fingerprint, bucket_start);

-- Individual statements at or above `[observability] slow_query_ms`, sampled
-- (at most 200 per flush interval) with the reduced text. `at` is Unix ms.
CREATE TABLE _forgebase_slow_statements (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    at          INTEGER NOT NULL,
    fingerprint TEXT    NOT NULL,
    backend     TEXT    NOT NULL,
    area        TEXT    NOT NULL,
    statement   TEXT    NOT NULL,
    duration_us INTEGER NOT NULL,
    rows        INTEGER NOT NULL DEFAULT 0,
    error       INTEGER NOT NULL DEFAULT 0 CHECK (error IN (0, 1)),
    write       INTEGER NOT NULL DEFAULT 0 CHECK (write IN (0, 1))
) STRICT;
CREATE INDEX _forgebase_slow_statements_at ON _forgebase_slow_statements (at);
CREATE INDEX _forgebase_slow_statements_fingerprint ON _forgebase_slow_statements (fingerprint, at);

-- Function statistics per (flush bucket, function, version): invocations,
-- errors, timeouts, cold starts, duration sketch, fuel and memory use against
-- their limits, and the host-call breakdown (`host`: JSON object
-- `{"db.get": {"count", "total_us", "max_us", "errors"}, ...}`).
CREATE TABLE _forgebase_fn_stats (
    bucket_start INTEGER NOT NULL,
    bucket_ms    INTEGER NOT NULL,
    function     TEXT    NOT NULL,
    version      INTEGER NOT NULL,
    invocations  INTEGER NOT NULL,
    errors       INTEGER NOT NULL DEFAULT 0,
    timeouts     INTEGER NOT NULL DEFAULT 0,
    cold_starts  INTEGER NOT NULL DEFAULT 0,
    total_us     INTEGER NOT NULL,
    max_us       INTEGER NOT NULL,
    fuel_total   INTEGER NOT NULL DEFAULT 0,
    fuel_max     INTEGER NOT NULL DEFAULT 0,
    fuel_limit   INTEGER NOT NULL DEFAULT 0,
    memory_max   INTEGER NOT NULL DEFAULT 0,
    memory_limit INTEGER NOT NULL DEFAULT 0,
    host_calls   INTEGER NOT NULL DEFAULT 0,
    sketch       TEXT    NOT NULL DEFAULT '[]',
    host         TEXT    NOT NULL DEFAULT '{}',
    PRIMARY KEY (bucket_start, function, version)
) STRICT;
CREATE INDEX _forgebase_fn_stats_function ON _forgebase_fn_stats (function, bucket_start);

-- Per-invocation profile (host-call timeline, fuel, memory, cold start) of an
-- execution, next to its log row; kept as long as the log row.
ALTER TABLE _forgebase_function_logs ADD COLUMN profile TEXT;

-- Daily per-tenant usage numbers that no other table keeps by day: resource
-- snapshots (`storage.bytes`, `kv.bytes`, ...: the last value of the day) and
-- counters of work done (`functions.invocations`, `queue.messages`, ...).
-- `scope` is `env:<environment id>` (a tenant), or empty for data that
-- belongs to no tenant. `day` is a UTC date `YYYY-MM-DD`.
CREATE TABLE _forgebase_tenant_usage_daily (
    day        TEXT    NOT NULL,
    scope      TEXT    NOT NULL,
    metric     TEXT    NOT NULL,
    value      INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (day, scope, metric)
) STRICT;
CREATE INDEX _forgebase_tenant_usage_daily_metric ON _forgebase_tenant_usage_daily (metric, day);
