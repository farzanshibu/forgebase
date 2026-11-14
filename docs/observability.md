# Observability: tracing, alerts, SLOs and insights

ForgeBase exports metrics for Prometheus on `/metrics`
([deployment.md §5.1](deployment.md#51-metrics-metrics)). This page covers the
rest of [ADR-0035](adr/0035-tracing-alerting-slos.md):

1. **Tracing**: W3C trace context end to end, trace ids in JSON logs, and
   export of traces (and optionally logs) to an OpenTelemetry collector.
2. **Alerts**: rules over the server's metrics and health signals, a built-in
   rule pack, silences, and notifiers (signed webhook, Slack, email,
   PagerDuty, a queue topic).
3. **SLOs**: availability and latency objectives with error budgets and
   multi-window burn-rate alerts.

Everything here is administrator-only and documented in the OpenAPI
document (`operationId` prefixes `alerts_` and `slos_`).

## 1. Tracing

### 1.1 What you get without configuration

With `log.format = "json"` every log line carries `trace_id` and `span_id`
(the first two keys), so the lines of one request group together even
without a tracing backend. Responses always carry `x-request-id`.

Nothing is exported, no header is added to responses or outbound requests,
and no trace attribute is added to queue messages until
`[observability.otlp] enabled = true`.

### 1.2 Turning on export

```toml
[observability.otlp]
enabled = true
endpoint = "http://127.0.0.1:4318"   # /v1/traces and /v1/logs are appended
protocol = "http/protobuf"          # or "http/json"
service_name = "forgebase"
sampling_ratio = 1.0                # share of new traces recorded (parent-based)
export_traces = true
export_logs = false                 # log lines can hold personal data
# headers = { "x-honeycomb-team" = "env:HONEYCOMB_API_KEY" }
# resource_attributes = { "deployment.environment.name" = "production" }
```

Every key has an environment variable (`FORGEBASE_OTLP_ENABLED`,
`FORGEBASE_OTLP_ENDPOINT`, `FORGEBASE_OTLP_PROTOCOL`,
`FORGEBASE_OTLP_SAMPLING_RATIO`, …). Header values written as `env:NAME` are
read from the environment variable `NAME` at start; `FORGEBASE_OTLP_HEADERS`
(`name=value,name2=value2`, values may be percent-encoded) adds secret
headers without writing them anywhere. `ca_file` adds a PEM certificate
authority for a collector with a private certificate. A change needs a
restart.

The exporter runs on its own thread: spans and log records go into a bounded
queue (8192 items; when full, new items are dropped and counted in
`forgebase_otlp_dropped_total{reason="queue_full"}`), and are sent in batches
of up to 512 every two seconds. A failed request is retried twice on
connection errors and `429`, `502`, `503`, `504`, then dropped
(`forgebase_otlp_dropped_total{reason="export_failed"}`, one warning a
minute). `forgebase_otlp_exported_total{signal}` counts what arrived. On
shutdown the queue is flushed for up to five seconds. Only OTLP/HTTP is
supported (no gRPC): use port 4318.

### 1.3 Backends

| Backend | `endpoint` | Notes |
|---|---|---|
| OpenTelemetry Collector | `http://127.0.0.1:4318` | The `otlp` receiver with `protocols: http`. Recommended in front of everything else: batching, tail sampling, fan-out. |
| Grafana Tempo | `http://tempo:4318` | Enable the `otlp` receiver's `http` protocol in the distributor. Logs: send them to Loki through a collector. |
| Jaeger (v2 or 1.35+) | `http://jaeger:4318` | OTLP is on by default (`COLLECTOR_OTLP_ENABLED=true` on 1.x). Jaeger ignores logs. |
| Honeycomb | `https://api.honeycomb.io` | `headers = { "x-honeycomb-team" = "env:HONEYCOMB_API_KEY" }`; the dataset follows `service_name`. |
| Grafana Cloud | the OTLP gateway URL of your stack | `FORGEBASE_OTLP_HEADERS=authorization=Basic%20<base64 instance:token>` |

### 1.4 Spans

| Span | Kind | Name | Attributes |
|---|---|---|---|
| HTTP request | server | `GET /api/v1/collections/{name}/records` (the route template, never the raw path) | `http.request.method`, `http.route`, `http.response.status_code`, `forgebase.request_id` |
| Function run | internal | `function <name>` | `faas.name`, `faas.trigger` |
| Outbound request (function `fetch`, push webhooks, notifiers) | client | `POST` | `http.request.method`, `server.address` (host only), `http.response.status_code` |
| Queue publish | producer | `publish <topic>` | `messaging.system`, `messaging.destination.name`, `messaging.batch.message_count` |
| Push delivery | consumer | `deliver <subscription>` | `messaging.destination.name`, `messaging.destination.subscription.name`, `messaging.message.id`, `forgebase.delivery_attempt` |
| Job run | consumer | `job <kind>` | `messaging.destination.name` (queue), `forgebase.job.id`, `forgebase.job.kind`, `forgebase.job.attempt` |

A 5xx answer, a failed function, delivery or job sets the span status to
error. Only span fields whose name contains a dot are exported; plain log
fields (`path`, `request_id`, `error`, …) stay in the local log.

### 1.5 How context travels

* **Into the server.** A request's `traceparent` (and `tracestate`) becomes
  the parent of its server span according to
  `[observability] trust_incoming_trace`:
  * `auto` (default): a *trusted* caller continues its trace with its own
    sampling decision. Trusted means `[auth] trust_forwarded_for = true` (you
    declared a proxy you run in front, which usually starts the trace) or a
    direct loopback client without forwarding headers. Any other caller keeps
    its trace id (your frontend's traces still join up), but ForgeBase's
    sampler decides whether the trace is recorded, so an outside client
    cannot make you export every request.
  * `always`: every caller decides. `never`: the header is ignored.
* **Out of the server.** Responses carry the server span's `traceparent`
  (CORS exposes it). Outbound requests through the SSRF-safe client (function
  `fetch`, push webhooks, webhook/Slack/PagerDuty notifiers) carry the
  client span's `traceparent`, unless the caller set one.
* **Queues.** A publish stamps each message with the attributes
  `traceparent` (and `tracestate`), unless the publisher set them; they count
  toward the message size. Push delivery continues that trace, so the webhook
  receiver gets a `traceparent` of the same trace. Pull and streaming
  consumers read the attribute and continue it themselves.
* **Jobs.** A job stores the `traceparent` of the span that enqueued it
  (`traceparent` in the job JSON); its run is a child of it. External
  workers can continue it the same way.
* **Functions.** Every execution log (`GET /api/v1/functions/{name}/logs`)
  has the run's `trace_id`, so a function's log lines join its trace; its
  `fetch` calls carry the `traceparent` of their client span.

## 2. Alerts

The engine evaluates every enabled rule every
`[alerts] evaluation_interval_secs` (30 by default, sooner after a change
through the API). A rule's expression yields samples; each sample that passes
the comparison with the threshold is an **alert** with the sample's labels,
the rule's labels, `alertname` and `severity`. An alert is `pending` until
the condition has held for the rule's `for`, then `firing`; when the
condition no longer holds it is `resolved` (a pending alert that clears is
resolved without having fired). Every change is kept
(`GET /api/v1/alerts/{id}` lists them) for `history_retention_days`.

When a signal cannot be read (the database is down, say), the rules that
read it are skipped: their alerts neither fire nor resolve on missing data.

### 2.1 Expressions

A subset of PromQL:

```text
sum(rate(forgebase_http_requests_total{status=~"5.."}[5m])) / sum(rate(forgebase_http_requests_total[5m]))
histogram_quantile(0.95, sum by (le) (rate(forgebase_http_request_duration_seconds_bucket{route=~"/api/v1/functions/.*"}[5m])))
forgebase:queue_backlog{subscription="orders"}
increase(forgebase:queue_dead_lettered_total[15m])
forgebase_tls_certificate_expiry_timestamp_seconds - time()
```

* Selectors with `=`, `!=`, `=~`, `!~` (anchored regular expressions).
* Range functions over `metric[5m]`: `rate`, `increase`, `delta`,
  `avg_over_time`, `min_over_time`, `max_over_time` (ranges up to 7 days).
  `rate` is per second over the points in the window; `increase` is not
  extrapolated (it never reports more than happened). A counter that appears
  between two rounds counts from 0.
* Aggregations `sum`, `avg`, `min`, `max`, `count` with `by (…)` or
  `without (…)`.
* `histogram_quantile(q, …)`, `abs`, `absent`, `clamp_min`, `clamp_max`,
  `time()`.
* Arithmetic `+ - * / %` between numbers and vectors; two vectors match on
  identical labels (the metric name is dropped).

The comparison is not part of the expression: it is the rule's `op` (`>`,
`>=`, `<`, `<=`, `==`, `!=`) and `threshold`. NaN never fires (a ratio with
no traffic is NaN). `POST /api/v1/alerts/query` evaluates an expression now,
for trying one out.

Range functions read the engine's own history of each series, kept for the
longest range any rule uses; after a restart a `[15m]` rule sees 15 minutes
of data only after 15 minutes. Gauges that ForgeBase refreshes only when
`/metrics` is scraped (`forgebase_jobs`, `forgebase_queue_deliveries`, …)
are as fresh as the last scrape: prefer the signals below.

### 2.2 Signals

| Series | Labels | Meaning |
|---|---|---|
| `forgebase:queue_backlog` | `subscription`, `topic` | Unacknowledged messages |
| `forgebase:queue_oldest_unacked_seconds` | `subscription`, `topic` | Age of the oldest one |
| `forgebase:queue_dead_lettered_total` | `subscription`, `topic` | Messages dead-lettered (counter: use `increase`) |
| `forgebase:queue_dead_letters` | `subscription`, `topic` | Dead-letter records kept |
| `forgebase:queue_push_paused` | `subscription`, `topic` | 1 while a push subscription is paused |
| `forgebase:data_dir_free_bytes`, `forgebase:data_dir_total_bytes`, `forgebase:data_dir_free_ratio` | | The data directory's disk |
| `forgebase:backup_last_success_timestamp_seconds`, `forgebase:backup_last_success_age_seconds` | | The newest snapshot in `backups/` (absent when there is none) |
| `forgebase:slo_burn_rate` | `slo`, `window` | Burn rate over `5m`, `30m`, `1h`, `6h`, `3d` |
| `forgebase:slo_error_budget_remaining`, `forgebase:slo_sli` | `slo` | Budget left, SLI over the window |

Everything on `/metrics` is available too: `forgebase_http_requests_total`,
`forgebase_billing_webhook_deliveries_total`, `forgebase_jobs_completed_total`
(job attempts by `queue` and `outcome`: `succeeded`, `failed`, `dead`,
`cancelled`), `forgebase_queue_push_*` and the rest.

### 2.3 Built-in rules

Each can be switched off with `PATCH /api/v1/alerts/rules/{id}
{"enabled": false}` or listed in `[alerts] disabled_rules` (the API cannot
switch those on). Each rule has a runbook (meaning, first checks, fixes) under
the same id in [operations/alerting-runbook.md](operations/alerting-runbook.md).
The restore drill (`forgebase backup drill`, ADR-0041) exports
`forgebase_backup_last_drill_success_timestamp_seconds`; the runbook shows
custom rules on it.

#### `http_5xx_ratio_high`

`HttpServerErrorsHigh`, warning: more than 5% of requests answered 5xx over
5 minutes, for 5 minutes. Find failing requests by `request_id` in the log
(or by trace id in your tracing backend).

#### `http_latency_p95_high`

`HttpLatencyHigh`, warning: the 95th percentile of request latency is above
1 s for 10 minutes. Check `forgebase_db_query_duration_seconds` and slow
functions.

#### `queue_backlog_high`

`QueueBacklogHigh`, warning: a subscription has more than 10 000
unacknowledged messages for 15 minutes. Add consumers or check that they
acknowledge.

#### `queue_oldest_unacked_high`

`QueueMessagesStuck`, warning: the oldest unacknowledged message is more than
an hour old for 10 minutes.

#### `queue_dead_letters_growing`

`QueueDeadLettersGrowing`, warning: messages were dead-lettered in the last 15
minutes. Inspect them and redrive (ADR-0031).

#### `queue_push_paused`

`QueuePushPaused`, critical: a push subscription is paused (by hand or by the
circuit breaker). Nothing is delivered until it is resumed. Check the push
attempt log, fix the endpoint, then resume.

#### `billing_webhook_signature_invalid`

`BillingWebhookSignaturesInvalid`, warning: more than 5 deliveries of one
provider failed the signature check in 15 minutes. Usually a wrong or rotated
webhook secret; otherwise forged requests. See the billing webhook inspector
and re-verify after fixing the secret (ADR-0032).

#### `job_failure_ratio_high`

`JobFailuresHigh`, warning: more than a quarter of the job attempts of a
queue failed over 15 minutes, for 15 minutes.

#### `jobs_dead_lettered`

`JobsDeadLettered`, warning: jobs of a queue went to the dead-letter queue in
the last 15 minutes. Retry them once fixed.

#### `disk_free_low`

`DiskSpaceLow`, warning: less than 10% of the data directory's disk is free
for 5 minutes.

#### `disk_free_critical`

`DiskSpaceCritical`, critical: less than 5% free for 5 minutes.

#### `backup_stale`

`BackupStale`, warning: the newest snapshot is more than 26 hours old. Check
the backup schedule (`forgebase backup`).

#### `tls_cert_expiring`

`TlsCertificateExpiring`, warning: a certificate expires within 14 days. The
rule reads the gauge named by `[alerts] tls_cert_expiry_metric` (default
`forgebase_tls_certificate_expiry_timestamp_seconds`, the Unix time the
certificate expires; point it at any exporter's equivalent, such as
`probe_ssl_earliest_cert_expiry`). It stays quiet while the gauge does not
exist.

#### `tls_cert_expiry_critical`

`TlsCertificateExpiringSoon`, critical: within 3 days.

### 2.4 Custom rules

In the file (read-only through the API, `source: "config"`; an invalid rule
stops the server from starting with the reason):

```toml
[[alerts.rules]]
name = "CheckoutSlow"
expr = "histogram_quantile(0.95, sum by (le) (rate(forgebase_http_request_duration_seconds_bucket{route=\"/api/v1/functions/checkout/invoke\"}[5m])))"
op = ">"
threshold = 0.5
for = "10m"
severity = "critical"
labels = { team = "payments" }
annotations = { summary = "Checkout p95 is {{ $value }} s", runbook_url = "https://wiki.example.com/checkout" }
```

Or through the API: `POST /api/v1/alerts/rules` with the same fields
(`for` as a duration string). Annotations expand `{{ $labels.<name> }}`,
`{{ $value }}`, `{{ $value | percent }}` and `{{ $value | duration }}`.

### 2.5 Notifiers

`POST /api/v1/alerts/notifiers`:

| `kind` | Fields | What is sent |
|---|---|---|
| `webhook` | `url` | `POST` of the JSON below with `X-Forgebase-Signature: t=<unix s>,v1=<hex HMAC-SHA256(secret, "<t>.<body>")>` (the ADR-0031 scheme; verify like a push webhook), `X-Forgebase-Event: alert.firing`, `alert.resolved` or `alert.test`, `X-Forgebase-Delivery`. The `signing_secret` (`whsec_…`) is in the create answer only. |
| `slack` | `url` (the incoming-webhook URL; stored sealed, listed as its host) | `{"text": …}` with one line per alert. |
| `email` | `to` (1-20 addresses) | Through the server's mail settings and the template `alert_notification` (created on first use, with variables `subject`, `text`, `status`; edit it like any template). |
| `pagerduty` | `routing_key` (sealed), `url` (default `https://events.pagerduty.com/v2/enqueue`) | Events API v2: `trigger` while the group fires, `resolve` when it resolves, one `dedup_key` per notifier and group. |
| `queue` | `topic` | One message per notification on that ForgeBase topic (`data` is the JSON below; attributes `status`, `alertname`, `severity`), to fan out yourself. |

Routing and grouping: `matchers` (all must match), `min_severity`,
`group_by` (default `["alertname"]`). A notifier is told about a group when
it first fires, when its set of firing alerts changes, every
`repeat_interval` (default `4h`) while it keeps firing, and (with
`send_resolved`, the default; PagerDuty always) when it resolves. Silenced
alerts are left out of groups.

```json
{"version": "1", "status": "firing", "notifier": "ops", "group_key": "{alertname=\"QueuePushPaused\"}",
 "group_labels": {"alertname": "QueuePushPaused"},
 "alerts": [{"status": "firing", "id": "alr-…", "fingerprint": "…", "rule_id": "queue_push_paused",
             "name": "QueuePushPaused", "severity": "critical",
             "labels": {"alertname": "QueuePushPaused", "severity": "critical", "subscription": "orders", "topic": "events"},
             "annotations": {"summary": "Push delivery of orders is paused", "runbook_url": "…"},
             "value": 1, "starts_at": "2026-11-14T10:00:00.000Z", "ends_at": null}],
 "created_at": "2026-11-14T10:00:00.000Z", "external_url": "https://forgebase.example.com/admin/"}
```

Notifications go through a delivery outbox (`GET
/api/v1/alerts/notifications`): a failed attempt is retried after 30 s,
doubling up to an hour, eight attempts in all, then `failed`
(`forgebase_alert_notifications_total{notifier_type,outcome}` counts them;
the notifier shows `last_error`). `POST /api/v1/alerts/notifiers/{id}/test`
sends a test notification at once (10 per notifier and minute) and records
it. Webhook, Slack and PagerDuty URLs must be public `https://` addresses
(`[alerts] allow_http = true` permits `http://`); requests go through the
SSRF-safe client, so private and metadata addresses are refused.

### 2.6 Silences

`POST /api/v1/alerts/silences`
`{"matchers": [{"label": "subscription", "value": "orders"}], "duration": "2h", "comment": "endpoint maintenance"}`
mutes notifications of matching alerts (they still fire and are listed with
`silenced_by`). `DELETE` ends a silence now; ended silences stay listed with
`?all=true` until the retention removes them.

## 3. SLOs

```json
POST /api/v1/slos
{"name": "records-api", "kind": "availability", "objective": 0.999, "window": "30d",
 "route_prefix": "/api/v1/collections"}

{"name": "checkout-latency", "kind": "latency", "objective": 0.99, "window": "28d",
 "route_regex": "/api/v1/functions/checkout/.*", "methods": ["POST"], "latency_threshold_ms": 300}
```

* **Availability**: good = not 5xx. **Latency**: good = answered within
  `latency_threshold_ms`; 5xx answers are left out (they count against
  availability).
* Routes are matched on the route template (as in `forgebase_http_requests_total`):
  `route_prefix`, or an anchored `route_regex`, or every route. Unmatched
  paths (404s of unknown routes) never count.
* Counts are kept per SLO and minute in the database (`_forgebase_slo_minutes`,
  pruned after the window; at most 90 days × 1440 rows per SLO), written
  every evaluation round and at shutdown. A new SLO starts with no history; a
  changed one keeps its counts.

`GET /api/v1/slos/{id}/status`:

```json
{"slo": {…}, "total": 120000, "good": 119950, "bad": 50, "sli": 0.99958,
 "error_budget": {"allowed": 120.0, "consumed": 50, "remaining_ratio": 0.583},
 "burn_rates": {"5m": 0.0, "30m": 1.2, "1h": 0.8, "6h": 0.4, "3d": 0.41},
 "alerts": [{"tier": "page_fast", "long_window": "1h", "short_window": "5m", "burn_rate_threshold": 14.4,
             "severity": "critical", "firing": false, "rule_id": "slo:records-api:page_fast"}, …]}
```

* Error budget: `allowed = (1 - objective) × total`; `remaining_ratio = 1 - bad / allowed`
  (negative when overspent, 1 without traffic).
* Burn rate over a window: `(bad / total) / (1 - objective)`; 1 spends the
  budget exactly over the SLO window.
* Burn-rate alerts (Google SRE workbook, multi-window multi-burn-rate): a tier
  fires when **both** its long and short windows burn faster than its
  threshold, which is the rate that spends a share of the budget in the long
  window: `share × window / long`.

| Tier | Long / short | Budget share | Threshold for 30 days | Severity |
|---|---|---|---|---|
| `page_fast` | 1h / 5m | 2% | 14.4 | critical |
| `page_slow` | 6h / 30m | 5% | 6 | critical |
| `ticket` | 3d / 6h | 10% | 1 | warning |

  Tiers whose long window is longer than the SLO window are left out. The
  rules are `slo:<name>:<tier>` (`alertname` `SloBurnRateHigh`, labels `slo`,
  `tier`); `"alerting": false` on the SLO removes them; each can be switched
  off like a built-in rule.
* Prometheus: `forgebase_slo_sli_ratio`, `forgebase_slo_objective_ratio`,
  `forgebase_slo_error_budget_remaining_ratio` (label `slo`) and
  `forgebase_slo_burn_rate` (labels `slo`, `window`).

## 4. What leaves the box

See [security.md §4.15](security.md#415-tracing-export-and-alert-notifications-adr-0035).

## 5. Insights

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
