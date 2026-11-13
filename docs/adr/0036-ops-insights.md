# ADR-0036: Operations insights for the dashboard

- Status: accepted
- Date: 2026-11-14
- Scope: `crates/forgebase-insights` (new), `crates/forgebase-db` (`stmtstats.rs`, hooks in `slow_log.rs`
  and the six statement paths), `crates/forgebase-functions` (`profile.rs`, hooks at the host-call boundary),
  `crates/forgebase-auth` (`service/audit_search.rs`), `crates/forgebase-core` (`sketch.rs`,
  `observability_config.rs`, the settings registry), `crates/forgebase-data` (`Action::ReadInsights`,
  `Action::ReadAudit`), `crates/forgebase-server` (wiring), migrations `0060_insights` and `0061_audit_search`
  (SQLite and PostgreSQL).
- Builds on: ADR-0001 (one writer), ADR-0008 (tenancy tree, actions), ADR-0010 (usage events), ADR-0016
  (credit rates), ADR-0017 and ADR-0027 (functions), ADR-0021 (billing prices), ADR-0028 (both backends),
  ADR-0029 (placement registry).
- Not in this ADR: trace export and alert rules (ADR-0035). Everything here is exposed so alert rules can read
  it: Prometheus gauges and counters (§9).

## Context

The dashboard shows what exists but not how it behaves. An operator could not answer: which statements are
slow, and did one get slower since yesterday; which tenant uses what and what would it cost; what a function
spends its time on; who did what, from where, and what else happened around it. The pieces exist: the
slow-query ring buffer in `forgebase-db` (256 entries of raw SQL text, memory only), usage events, execution
logs, the auth audit log. This ADR adds the measuring, the bounded storage and one admin API over them.

## Decisions

### 1. Fingerprints

Every statement a backend runs (reads, writes, inside or outside a transaction, SQLite and PostgreSQL) is
reduced to a *fingerprint* by one scan of its text (`forgebase_db::stmtstats`): comments and whitespace go,
keywords and bare identifiers are lower-cased, quoted identifiers stay, **every literal becomes `?`** (strings
with `''` and backslash escapes, `E'..'`, `X'..'`, dollar-quoted strings, numbers with sign, exponent and hex,
`?`, `?N`, `$N`, `:name`), and a list of placeholders (`IN (?, ?, ?)`) becomes one. Spacing comes from the
tokens, not from the original whitespace, so `a=1` and `a = 1` agree. The id is the 64-bit hash of that text
(16 hex digits, salted per backend). `VALUES (?, ?), (?, ?)` with a different row count is another
fingerprint; the table is bounded (§2), so a bulk loader cannot grow it.

The scan hashes while it reduces and allocates nothing; the reduced *text* is built once per fingerprint, on
its first statement. The text stored and shown, in the statistics and in the slow log, is that reduced text, so
a secret in a literal never reaches the database or the API.

### 2. Aggregation on the hot path

`StatementStats::record` is called from `SlowLog::record` (and `record_error` for failed statements), which
every statement path already called. It does: one scan; a read lock on one of 16 shard maps; relaxed atomic
additions (calls, errors, total, rows, max, min) and one into a latency sketch. No write lock, no database
access, no allocation after the first sight of a fingerprint. In memory at most 4000 fingerprints (further
statements count in one `<other statements>` entry); a fingerprint idle for five drains is dropped. The
statement's area (`data`, `jobs`, `queue`, `billing`, ...) comes from the first `_forgebase_*` table in its
reduced text, else `data` for a labelled data-core statement, else `other`. Statements on the insight tables
themselves are not counted.

**Latency sketch** (`forgebase_core::sketch`): a log-linear histogram, four buckets per power of two, 160
buckets up to 2^40 µs. A bucket spans at most 25 % of its lower bound, so a quantile read at the bucket
midpoint is within about ±11 %; sketches merge by adding counts, so a time range gets its p50/p95/p99 by adding
the stored buckets. It is 1.3 KB in memory and sparse JSON (`[[bucket, count], ...]`) on disk.

**Measured overhead** (debug build, Apple silicon; `crates/forgebase-db/tests/stmtstats.rs` asserts the
bounds): 4.1 µs per statement for a 230-byte statement on one thread, 0.9 µs per statement on eight threads
sharing one fingerprint (no contention collapse); end to end, a SQLite point lookup (about 38 µs) costs 1.1 µs
more with the statistics on, about 3 %. The test fails above 25 µs (single thread), 4× the single-thread cost
(eight threads) or 40 µs added per statement end to end. A release build is several times faster.

### 3. Storage, flush, retention

A background task (`InsightsService::start_background`) drains the statistics every
`[observability] flush_interval_secs` (60) and writes one bucket to `_forgebase_stmt_stats` (migration 0060):
the busiest `query_stats_top_n` (200) fingerprints by total time, and one summary row for the rest. Nothing is
written per statement. `bucket_start` is the start of the interval flushed (not aligned to a clock), so a
bucket is never written twice. Buckets older than two hours are merged into hourly buckets once an hour
(`compact`), so reading days of history reads a bounded number of rows (at most 200 × (120 + 24 × days)).
Rows older than `retention_days` (14) are deleted every tenth flush. The last interval is written at
shutdown. A flush that fails loses that interval's statistics (they are statistics, not data); the next
interval is unaffected.

Reads merge the stored buckets in the range with what is still in memory (the interval since the last
flush), so the newest statements show without waiting for a flush. `POST /insights/flush` writes now.

### 4. Slow statements

A statement at or above `[observability] slow_query_ms` (200; `0` records all) is kept as an individual entry:
time, fingerprint, area, backend, duration, rows, whether it failed and wrote, and the *reduced text*. At most
200 are kept per flush interval (a budget reset at every drain; the rest only count, and the count of dropped
ones is returned by the flush). They are written to `_forgebase_slow_statements`, deleted after
`retention_days`, and never more than the newest 20 000. The counter
`forgebase_db_slow_statements_total{area}` counts every one, kept or not. The older in-memory ring
(`Backend::slow_queries`, raw text) is unchanged; the threshold it uses is now the configured one.

### 5. Regressions

`GET /insights/queries/regressions` compares a recent window (default 1 hour ending now) with the baseline
window before it (default 24 hours), per fingerprint: it is reported when both windows have at least
`min_calls` (20) runs, the recent p95 is at least `min_ratio` (2) times the baseline p95 and at least
`min_delta_ms` (1) larger. A second kind, `errors`, reports a fingerprint whose error rate rose by at least
5 percentage points to at least 5 % with 3 or more errors. The list is sorted by the extra time the recent
runs cost (`(p95 growth) × calls`). Windows are assigned by bucket start, so a bucket straddling the edge
belongs to the window it starts in. The count of latency regressions is the gauge `forgebase_db_regressions`
(set whenever the check runs).

### 6. Per-tenant usage and cost

**Tenants** follow ADR-0008: realm → organization → project → environment. `group_by` is `environment`
(default), `project`, `org` or `realm`.

**Sources, nothing duplicated.** Metering events (ADR-0010) are summed straight from
`_forgebase_usage_events` for the period (migration 0061 adds an index on `at`, so no rollup of events is
kept), keyed by the billed subject. Everything with no per-day history of its own goes into
`_forgebase_tenant_usage_daily (day, scope, metric, value)` (migration 0060): the **resource snapshots**
`storage.bytes`, `storage.objects`, `kv.keys`, `kv.bytes`, `vector.items` (last value of the day) and the
**work counters** `queue.messages`, `queue.bytes`, `jobs.created` (rows created that day, still retained), and
`functions.invocations`, `functions.compute_ms`, `functions.fuel` (added by each flush from the function
profiler, per tenant of the caller). `rollup_day` recomputes the day from the live tables on every set
(control and shards) and replaces its values; the server does it for today and yesterday every 15 minutes, and
`POST /insights/usage/rollup` does it on demand. Daily rows are kept 400 days.

**Periods:** `period=day|month` with `date`, or `from`/`to` (at most 400 days). Counters are summed over the
period; gauges show the value of the last day with data and the peak.

**Attribution.** Resource numbers belong to environments and add up to their project, organization (or
`user:<owner>` of a personal project) and realm. Metering events belong to the billed subject: `org:<id>` (so
they appear at organization level and above) or a principal. When listing environments or projects they show as
rows of their own with `attributed: false` rather than being spread over environments.

**Cost: an estimate, marked as one** (`"estimate": true`, in every response and cost object). For each counter
metric, the metered price of the same feature in the billing catalog (`usage.<feature>` → feature;
the built-in work counters → their own name) is applied to the period's quantity with the price's scheme
(per unit, graduated or volume) in the requested currency (default: the currency of most metered prices), as
if the period were one billing period; and the credits the credit rate of that feature would charge. Gauges
are not priced. Without billing, or without a price for the currency, a line has no amount; with neither
prices nor rates there is no `cost`. The estimate prices the catalog as it is now; it is not an invoice.

Top-N is `sort=<metric or cost>` with `limit`, or `/usage/top?metric=&n=`.

### 7. Function profiling

The host-call boundary (`call` for WebAssembly functions, `js_call` for JavaScript ones) reads the clock before
and after each operation and adds its duration to a per-operation counter and, for the first 64 calls, to a
timeline (operation, offset, duration, ok). Operation names come from a fixed list (`db.get`, `kv`, `fetch`,
`storage.put`, ...; anything else is `other`), so a function cannot grow the key set. The memory limiter
records the largest memory the execution asked for. Whether the execution was a **cold start** is whether its
module or script had to be loaded into the cache (`prepare_us` is that time). At the end `FnProfiler::record`
folds the result into an aggregate per `(function, version)` — invocations, errors, timeouts, cold starts,
duration sketch, fuel total/max against the fuel limit, peak memory against its limit, host calls and the
per-operation breakdown — and a counter per tenant. The task of §3 drains it into `_forgebase_fn_stats`.

The profile of each execution, as JSON, is stored in the new `profile` column of its row in
`_forgebase_function_logs` (the same row that holds outcome, trigger, request id and log lines), so
per-invocation detail has exactly the log's retention (`log_retention` rows per function, 1000) and costs no
extra write. `[observability] function_profiling = false` turns the timing off (the aggregates and the
column stay empty). Cost: two clock readings per host call and a short mutex hold per execution.

### 8. Audit investigation

`AuthService::audit_search` (in `forgebase-auth`, which owns the table) filters `_forgebase_auth_audit` by
actor (exact, or by kind: `user`, `admin`, `service_account`, `api_key`, `anonymous`), action (prefix),
resource type, collection and id, outcome, client address, request id, time range `[from, to)` and free text
(case-insensitive, `%` and `_` are literal) in the details, event, resource id and user agent. Every value is a
bound parameter. Listing is by row id, newest or oldest first, with an id cursor (`next_cursor`): stable under
concurrent inserts. `audit_related` returns the entries of the same request and of the same actor within a
window (default ±5 minutes, at most one hour). `audit_summary` counts by outcome, event, actor and address.
Migration 0061 adds indexes on actor, client address, request id and time.

**Export** (`/insights/audit/export?format=csv|jsonl`) streams pages of 1000 rows through the response body,
oldest first, so a large export never sits in memory; `max_rows` defaults to 100 000, at most 1 000 000. In
CSV a cell that starts with `=`, `+`, `-`, `@`, tab or CR (a user agent or a detail an attacker chose) is
prefixed with `'` so a spreadsheet does not run it.

Realm administrators who hold `engine:read_audit` see only their realm's rows; platform administrators see all.

### 9. API, authorization, metrics

One prefix, `/api/v1/insights`, operation ids `insights_*`, tag `insights`, in the OpenAPI document. Every
handler first asks the data core's authorizer for **`Action::ReadInsights`** (statements, usage, functions) or
**`Action::ReadAudit`**: platform administrators and admin service accounts pass; anyone else needs the
permission `engine:read_insights` / `engine:read_audit`; anonymous callers get `401`, others `403 forbidden`.
Every list is paged and bounded (page size caps, range caps, `max_rows`).

Prometheus (for alert rules): `forgebase_db_slow_statements_total{area}`;
`forgebase_db_statement_latency_seconds{area,quantile}`, `forgebase_db_statement_calls{area}` and
`forgebase_db_statement_errors{area}` for the last flushed interval; `forgebase_db_regressions`;
`forgebase_function_cold_starts_total{function}` and `forgebase_function_fuel_utilisation_ratio{function}`.

### 10. Configuration

`[observability]`: `slow_query_ms` (200), `query_stats` (true), `flush_interval_secs` (60),
`query_stats_top_n` (200), `retention_days` (14), `function_profiling` (true); env
`FORGEBASE_OBSERVABILITY_<KEY>`; all in the settings registry. The section is a separate module
(`observability_config.rs`) so it merges next to other `[observability]` keys.

## Consequences

- Both backends: the schema is the same (parity test), statements use `?N`, `CAST`, `ON CONFLICT` only.
- On a sharded server the flush writes to control; tenant resource numbers are read from every set.
- The statistics are per process. Several servers on one PostgreSQL (not supported today: ADR-0001 §3) would
  each write their own buckets; reads add them up.
- Statement text is reduced, not parsed: an exotic dialect construct may fingerprint to more shapes than
  expected; the 4000-entry bound and the top-N keep the cost fixed.

## Limitations and follow-ups

- Queue messages and jobs of a day are counted from rows still retained; a topic with a retention under a day
  undercounts. KV numbers are those of the last write-behind flush of the engine.
- Gauges are snapshots from the 15-minute rollup, not integrals (no GB-hours).
- Dashboard pages and SDK wrappers (list in the work report).
