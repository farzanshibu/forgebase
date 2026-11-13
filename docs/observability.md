# Observability

## Insights

Insights answer four operator questions from inside the server, for administrators, under
`/api/v1/insights` ([ADR-0036](adr/0036-ops-insights.md)): *which statements are slow, and did one get
slower*; *which tenant uses what, and what would it cost*; *what does a function spend its time on*; *who did
what, from where, and what happened around it*. Every route needs the `ReadInsights` action (statements,
usage, functions) or `ReadAudit` (audit): platform administrators and admin service accounts have both;
anyone else needs the permission `engine:read_insights` / `engine:read_audit`. Anonymous callers get `401`,
signed-in callers without the permission `403`. Every list is paged and bounded.

Times in query strings are RFC 3339 (`2026-11-14T09:00:00Z`) or a date (`2026-11-14`, UTC midnight); `to` is
exclusive. Durations in answers are milliseconds with three decimals.

### Configuration

```toml
[observability]
slow_query_ms = 200          # FORGEBASE_OBSERVABILITY_SLOW_QUERY_MS (0 = every statement)
query_stats = true           # aggregate statements by shape
flush_interval_secs = 60     # statistics in memory are written this often
query_stats_top_n = 200      # shapes written per flush; the rest in one summary row
retention_days = 14          # statistics, slow statements, function profiles (daily tenant usage: 400)
function_profiling = true    # time every function host call
```

Memory is bounded: at most 4000 statement shapes (further ones are counted together), 200 slow statements per
flush interval, 64 timeline entries per function execution. The cost of statistics is measured by a test: a few
microseconds per statement on one thread, no contention across threads, about 3 % on a point lookup
([ADR-0036 §2](adr/0036-ops-insights.md)).

### Statements: slow log and regressions

A statement is reduced to its *shape*: values, placeholders and comments removed, `IN (?, ?, ?)` shortened.
`select name from users where id = ? and name = ?` is one shape however often it runs with other values. The
text shown anywhere never holds a value. Each shape has an area (`data`, `auth`, `jobs`, `queue`, `kv`,
`storage`, `billing`, `functions`, ...) taken from the first system table it names.

| Route | Answers |
|---|---|
| `GET /insights/status` | State: enabled, threshold, flush interval, shapes in memory, rows stored. |
| `POST /insights/flush` | Writes the statistics in memory now. `{statements, slow_statements, slow_dropped, functions}`. |
| `GET /insights/queries` | Statistics per shape over `from`/`to` (default last hour): `{items: [{fingerprint, statement, area, backend, calls, errors, error_rate, writes, slow, rows, total_ms, mean_ms, min_ms, max_ms, p50_ms, p95_ms, p99_ms}], total, from, to, limit, offset}`. Filters `area`, `backend`, `q` (text); `sort` = `total_time` (default), `calls`, `mean`, `max`, `p50`, `p95`, `p99`, `errors`, `rows`, `slow`; `limit` ≤ 200. |
| `GET /insights/queries/{fingerprint}` | One shape: the totals above plus `series: [{at, seconds, calls, errors, mean_ms, p95_ms, max_ms}]`. `404` when the range holds nothing of it. |
| `GET /insights/queries/regressions` | Shapes whose recent p95 grew: `{recent_from, recent_to, baseline_from, baseline_to, min_ratio, min_calls, min_delta_ms, items: [{kind, fingerprint, statement, area, backend, recent, baseline, ratio, delta_ms, extra_ms}]}`. `recent`/`baseline` are `{calls, errors, error_rate, mean_ms, p50_ms, p95_ms, p99_ms}`. Parameters: `window` (3600 s), `baseline` (86400 s), `until`, `min_calls` (20), `min_ratio` (2), `min_delta_ms` (1), `area`. `kind` is `latency` or `errors`. |
| `GET /insights/slow-queries` | Individual statements over the threshold, newest first: `{items: [{id, at, fingerprint, statement, area, backend, duration_ms, rows, error, write}], next_cursor}`. Filters `fingerprint`, `area`, `min_ms`, `q`, `from`/`to` (default 24 h); `cursor`, `limit` ≤ 500. |

Percentiles come from a latency sketch and are within about ±11 % of the true value. Prometheus:
`forgebase_db_slow_statements_total{area}` (every slow statement), `forgebase_db_statement_latency_seconds{area,quantile}`,
`forgebase_db_statement_calls{area}`, `forgebase_db_statement_errors{area}` (last flushed interval),
`forgebase_db_regressions` (latency regressions found by the last check).

### Usage and cost per tenant

Tenants are environments, grouped by `group_by=environment|project|org|realm`. Metrics (see
`GET /insights/usage/metrics`): resource snapshots `storage.bytes`, `storage.objects`, `kv.keys`, `kv.bytes`,
`vector.items` (last value of the period, with `peak`); work counters `queue.messages`, `queue.bytes`,
`jobs.created`, `functions.invocations`, `functions.compute_ms`, `functions.fuel` (summed); and
`usage.<feature>` for every feature metered through the entitlement engine, summed from the usage events of the
organization or principal that was billed. Those events are not attributed to an environment: listing
environments shows them as rows of their own with `attributed: false`; `group_by=org` adds them to the
organization's environments.

| Route | Answers |
|---|---|
| `GET /insights/usage` | `{from, to, group_by, sort, total, limit, offset, currency, cost_available, estimate: true, items: [{level, id, name, realm, org, project, attributed, metrics: {<metric>: {kind, value, peak}}, cost}]}`. Period: `period=day\|month` with `date` (`YYYY-MM-DD`, or `YYYY-MM`), or `from`/`to` (at most 400 days); default this month so far. Filters `metric` (comma list), `realm`, `org`, `project`; `sort` = a metric key or `cost`, largest first; `currency` (ISO 4217); `limit` ≤ 500, `offset`. |
| `GET /insights/usage/top` | The top `n` (≤ 100) by `metric` (a key or `cost`); same period and `group_by`. |
| `GET /insights/usage/series?scope=env:<id>` | Daily values: `{scope, metrics: {<metric>: [[day, value], ...]}}`. |
| `GET /insights/usage/metrics` | The built-in metrics: key, kind (`counter`/`gauge`), unit, description, source. |
| `POST /insights/usage/rollup?day=` | Recomputes the day's resource and work numbers from the live tables (the server does today and yesterday every 15 minutes). |

**The cost is an estimate** (`"estimate": true`; every `cost` object repeats it): the period's quantity of each
counter priced with the metered price of the same feature in the billing catalog (per unit, graduated or
volume, in `currency`) as if the period were one billing period, plus the credits the credit rate of that
feature would charge: `cost: {estimate, currency, amount_minor, amount, credits, lines: [{metric, quantity, price, amount_minor, credits}]}`.
Gauges are not priced. With billing off, no price in the currency and no rate there is no `cost`.

### Functions

| Route | Answers |
|---|---|
| `GET /insights/functions` | Per function version over `from`/`to` (default last hour): `{items: [{function, version, invocations, errors, error_rate, timeouts, cold_starts, mean_ms, p50_ms, p95_ms, p99_ms, max_ms, fuel_mean, fuel_max, fuel_limit, fuel_utilisation, memory_max_bytes, memory_limit_bytes, host_calls_per_invocation, host: [{op, count, errors, total_ms, mean_ms, max_ms, share}]}]}`. Filters `function`, `version`. |
| `GET /insights/functions/{name}` | `{function, versions: [...as above], series: [{at, invocations, errors, cold_starts, mean_ms, p95_ms}]}`. |
| `GET /insights/functions/invocations` | Recent executions, newest first: the execution log row (`id, function, version, trigger, status, duration_ms, fuel_used, error, logs, request_id, created_at`) plus `profile`; `next_cursor`. Filters `function`, `version`, `status`, `trigger`, `request_id`, `errors=true`, `min_duration_ms`; `cursor`, `limit` ≤ 500. |
| `GET /insights/functions/invocations/{id}` | One of them. |

`profile` is `{cold_start, prepare_us, duration_us, fuel_used, fuel_limit, memory_peak_bytes, memory_limit_bytes, tenant, host_calls, host: {<op>: {count, total_us, max_us, errors}}, timeline: [{op, at_us, dur_us, ok}], timeline_dropped}`.
`timeline` holds the first 64 host calls. A **cold start** is an execution that had to load its module or
script into the cache (after a restart, an eviction, or the first run); `prepare_us` is that time. Fuel is
WebAssembly fuel, roughly instructions. Invocation detail is kept as long as the execution log
(`log_retention` rows per function); aggregates for `retention_days`.

### Audit investigation

| Route | Answers |
|---|---|
| `GET /insights/audit` | Search, newest first: `{items: [{id, at, event, outcome, actor, resource_type, collection, resource_id, ip, user_agent, request_id, impersonator, detail}], next_cursor}`. Filters: `actor` (exact: `user:<collection>/<id>`, `admin:<id>`, `service_account:<id>`, `anonymous`), `actor_type` (`user`, `admin`, `service_account`, `api_key`, `anonymous`), `action` (prefix: `login`, `api_key.`), `resource_type`, `collection`, `resource_id`, `outcome` (`success`/`failure`), `ip`, `request_id`, `from`/`to`, `q` (text in the details, event, resource id and user agent; case-insensitive, no wildcards), `order=asc\|desc`; `cursor`, `limit` ≤ 1000. |
| `GET /insights/audit/{id}` | One entry. |
| `GET /insights/audit/{id}/related` | `{event, same_request: [...], same_actor: [...]}`: the entries of the same request, and of the same actor within `window_secs` (default 300, at most 3600) either side. |
| `GET /insights/audit/summary` | `{total, outcomes, events, actors, ips}` with the top counts in `from`/`to`. |
| `GET /insights/audit/export` | The same filters as a download, streamed in pages: `format=csv` (default) or `jsonl`, oldest first, `max_rows` (default 100 000, at most 1 000 000). CSV cells that start like a formula are prefixed with `'`. |

The audit trail searched here is the authentication and administration log (`_forgebase_auth_audit`, also at
`GET /api/auth/audit`). Authorization denials, entitlement and billing audit logs have their own
endpoints.

### Limits to know

- Statistics are per process and per flush interval; a crash loses the interval in memory.
- Queue messages and jobs are counted from rows still retained; key-value figures are those of the engine's
  last write-behind flush; gauges are snapshots (15 minutes), not integrals.
- A statement shape is a reduced text, not a parsed query: bulk inserts with a varying number of rows are
  separate shapes (bounded as above).
