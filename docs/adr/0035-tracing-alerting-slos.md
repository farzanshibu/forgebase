# ADR-0035: Tracing export, alerting and SLOs

- Status: accepted
- Date: 2026-11-14
- Scope: `crates/forgebase-observability` (`trace.rs`, `otlp.rs`, `otlp_encode.rs`, `http_client.rs`, the
  JSON log format, metric names, `HttpObserver`), the new crate `crates/forgebase-alerts`, instrumentation in
  `forgebase-api` (server span, `traceparent` response header, HTTP observers), `forgebase-functions`
  (function span, fetch client span and header injection, `trace_id` on execution logs),
  `forgebase-queue` (publish and delivery spans, the `traceparent` message attribute), `forgebase-jobs` (stored
  trace context, run span, `forgebase_jobs_completed_total`), `forgebase-core` (`[observability]`,
  `[alerts]`), migrations `0058_alerts_slos` and `0059_trace_context` (both backends), server and CLI wiring.
- Builds on: ADR-0007 (jobs), ADR-0017 (functions, the SSRF-safe fetcher), ADR-0022 (queues), ADR-0031
  (push attempt log, health, circuit breaker; its signature scheme), ADR-0032 (billing webhook metrics),
  ADR-0020 (email templates and SMTP), ADR-0026 (feature registry).
- Docs: [observability.md](../observability.md).

## Context

ForgeBase had Prometheus metrics and logs with request ids. It had no way to follow one request through a
function, a queue and a webhook; nothing watched the metrics and told anyone; and there was no notion of a
service level objective. Operators had to run Prometheus, Alertmanager and a tracing pipeline next to a
single-binary backend to get there, and still could not see queue- or backup-level health without exporters.

## Decisions

### 1. OTLP export, hand-written

`[observability.otlp]` (off by default) exports spans and, with `export_logs`, log events over **OTLP/HTTP**,
protobuf (`application/x-protobuf`, the OTLP default) or JSON. Metrics stay on `/metrics`. No gRPC.

The encoder (`otlp_encode.rs`, about 300 lines) writes the few OTLP 1.x messages needed by hand, and a 120-line
HTTP/1.1 client (rustls with the webpki roots plus an optional `ca_file`) posts them. We did **not** take the
`opentelemetry`, `opentelemetry_sdk`, `opentelemetry-otlp` and `tracing-opentelemetry` crates:

- `opentelemetry-otlp`'s `http-proto` pulls `opentelemetry-proto` with `tonic` and `prost` even without
  gRPC, plus an HTTP client (reqwest or hyper-util) we would have to replace anyway;
- the SDK's span model would have to be bridged to `tracing` and to our propagation points in five crates,
  each then depending on the OpenTelemetry API;
- "zero overhead when off" is simpler to guarantee when the layer is ours;
- the protocol surface used is small and stable (OTLP 1.0 is frozen), and both encodings are unit-tested
  against the schema.

So no new dependency for export (rustls, tokio-rustls, webpki-roots, url and serde_json were in the tree).
The collector address is operator configuration, so unlike the fetcher this client may reach loopback and
private addresses. Secret headers come from the environment (`env:NAME` values, `FORGEBASE_OTLP_HEADERS`).

The exporter is one thread with a current-thread Tokio runtime fed by a bounded queue (8192 items, batches of
512 every 2 s); a full queue drops and counts (`forgebase_otlp_dropped_total`), a failed batch is retried twice
(connection errors, 429, 502-504) and then dropped and counted; `serve` flushes it for up to 5 s at shutdown.
Its own warnings use the target `forgebase_observability::otlp`, which log export skips (no feedback loop).

### 2. Trace context

A `tracing` layer (`ContextLayer`) gives every span a W3C trace context: from a remote parent given as the
field `otel.parent` (a `traceparent` value), else from the parent span, else a new trace whose sampling is
decided by the **parent-based ratio sampler** (`sampling_ratio`, deterministic on the trace id). Span names,
kinds and status come from `otel.name`, `otel.kind`, `otel.status_code` (the `tracing-opentelemetry`
convention). **Only fields with a dot in their name become span attributes**; plain fields stay in local
logs, so adding a log field never leaks it to a collector.

Without export the layer is installed only for JSON logs (ids only: nothing recorded); with pretty logs and no
export nothing changes at all. With export, the layer has its own filter: spans at `INFO` and above always
(tracing does not depend on `log.level`), events at `log.level` when logs are exported.

JSON log lines carry `trace_id` and `span_id` as their first keys (the existing JSON format otherwise
unchanged). Propagation, only while export is on:

- **HTTP in.** The server span is `<METHOD> <route template>` (never the raw path). A caller's `traceparent`
  is its parent per `[observability] trust_incoming_trace`: `auto` (default) lets a *trusted* caller (behind
  `[auth] trust_forwarded_for = true`, i.e. a proxy you declared, or a direct loopback client without
  forwarding headers, the same test as `ops_access`) decide sampling; an untrusted caller keeps its trace id
  (frontend traces still join) but the ratio sampler decides, so outside clients cannot force every request to
  be exported. `always` and `never` are the obvious alternatives.
- **HTTP out.** Responses carry the server span's `traceparent`. The SSRF-safe fetcher makes a client span
  (host only, never the URL) and adds its `traceparent` unless the caller set one: function `fetch`, push
  webhooks and notifiers all get it.
- **Queues.** A publish is a producer span; each message gets the attributes `traceparent`/`tracestate`
  (unless set by the publisher), stored with it. Push delivery is a consumer span continuing it, so the webhook
  receiver sees the original trace id. Transactional publishes take the current span's context.
- **Jobs.** `_forgebase_jobs.traceparent` (0059) stores the enqueuing span's context; the run is a consumer
  span continuing it; the job JSON shows it for external workers.
- **Functions.** A run is a span `function <name>`; `_forgebase_function_logs.trace_id` (0059) puts its
  trace id on every execution log. `ctx.trace` in JavaScript is a follow-up: the prelude is compiled into the
  prebuilt QuickJS engine (`forgebase-js.wasm`), which has to be rebuilt to read a new input field.

### 3. Alerting engine (`forgebase-alerts`)

A rule is an expression, a comparison and threshold, a `for` duration, a severity, labels and annotations.
Expressions are a **PromQL subset** (selectors with `= != =~ !~`, `rate`, `increase`, `delta`,
`*_over_time`, `sum/avg/min/max/count by|without`, `histogram_quantile`, `abs`, `absent`, `clamp_*`,
`time()`, arithmetic with label matching). The comparison is the rule's, not the expression's. The engine
keeps its own short history of the series that range functions read (for the longest range used), so it needs
no Prometheus server. `increase` is not extrapolated and a counter born between rounds counts from zero.

Inputs: this process's Prometheus registry, and **signals** (`forgebase:*` series) collected each round only
when a rule reads them: queue backlog, oldest unacknowledged age, dead-lettered total and records, push paused
(through `QueueService`, so every database set is covered), the data directory's disk space (statvfs), the
newest backup snapshot, SLO burn rates, budgets and SLIs. Billing `signature_invalid`, HTTP 5xx ratio and p95
use the existing counters and histograms (the billing counter also counts deliveries the inspector budget did
not store). Job failures got a counter, `forgebase_jobs_completed_total{queue,outcome}`, in
`complete`/`fail`, so external workers are counted too. A signal that cannot be collected skips the rules
reading it: their alerts neither fire nor resolve on missing data.

States: an alert (rule id + label set, hashed into a `fingerprint`) is `pending` while the condition holds
for less than `for`, then `firing`, then `resolved`; at most one open row per fingerprint (a partial unique
index), every transition in `_forgebase_alert_events`, all of a round in one transaction. Resolved alerts are
history, pruned after `history_retention_days`.

The **built-in pack** (14 rules, each switchable by id through the API or `[alerts] disabled_rules`): 5xx
ratio, p95 latency, queue backlog, stuck messages, dead-letter growth, paused push subscription, billing
signature failures, job failure ratio, dead jobs, disk low and critical, stale backup, and two certificate
expiry rules on a configurable gauge (`tls_cert_expiry_metric`, a Unix expiry time) so the TLS work plugs in
by name. Custom rules come from `[[alerts.rules]]` (validated at start; an invalid one stops the server) and
the API.

### 4. Notifications

Notifiers (`_forgebase_alert_notifiers`): signed **webhook** (the ADR-0031 `X-Forgebase-Signature` scheme
with a per-notifier `whsec_` secret), **Slack** incoming webhook, **email** through `forgebase-customize`
(its SMTP provider, template `alert_notification` created on first use), **PagerDuty** Events v2 (trigger and
resolve, one dedup key per notifier and group), and a **queue topic** (publish with a dedup id per delivery).
Secrets (signing secret, Slack URL, routing key) are sealed with the keyring (AAD `alerts:notifier:<id>`) and
never listed. HTTP goes through the SSRF-safe fetcher; `https://` only unless `allow_http`.

Grouping and dedup: per notifier, firing non-silenced alerts that match its matchers and minimum severity are
grouped by `group_by` labels; `_forgebase_alert_groups` remembers the set of fingerprints each group was last
sent with. A notification is queued when a group appears, its set changes, its `repeat_interval` passes, or
it resolves. Silences (matchers and a window) leave alerts out of groups; an alert that leaves a group because
it was silenced is not reported resolved. Inhibition is not implemented.

Delivery: `_forgebase_alert_notifications` is an outbox written in the round's transactions and drained after
each round; failures retry after 30 s doubling to an hour, eight attempts, then `failed`; outcomes are listed
and counted. A test send is recorded and not retried.

### 5. SLOs

Availability (non-5xx share) and latency (share of non-5xx requests within a threshold) objectives over route
templates (prefix or anchored regex, optional methods). Counting is an `HttpObserver` that the API calls once
per answered request (route template, status, latency); it adds to in-memory per-SLO per-minute counts,
flushed every round (and at shutdown) into `_forgebase_slo_minutes` with an additive upsert. Per-SLO
aggregates (not per route) keep the table bounded: at most `window / 1 min` rows per SLO, pruned after the
window. A new SLO starts without history; latency thresholds are exact (not snapped to histogram buckets).

Error budget remaining is `1 - bad / ((1 - objective) × total)`; the burn rate of a window is
`(bad / total) / (1 - objective)`. Burn-rate alerts are generated rules per SLO following the Google SRE
workbook: `page_fast` (1h and 5m windows, 2% of the budget), `page_slow` (6h/30m, 5%), `ticket` (3d/6h, 10%),
threshold `share × window / long window` (14.4, 6 and 1 for 30 days); the expression takes the minimum of the
two windows' burn rates, so both must exceed it.

### 6. API and configuration

`/api/v1/alerts/*` and `/api/v1/slos/*` (28 operations, `operationId` prefixes `alerts_`, `slos_`), platform
administrators only (`AccessContext::is_privileged`, as the billing inspector; checked in a route layer before
any body is read). A new feature `alerts` (switch `alerts.enabled`, routes gated by the registry) and an
always-on `observability` feature hold the settings. Error codes: `alert_rule_not_found`, `alert_not_found`,
`silence_not_found`, `notifier_not_found`, `slo_not_found` (404); `alert_rule_exists`, `notifier_exists`,
`slo_exists`, `rule_read_only`, `rule_disabled_by_config` (409); `invalid_alert_rule`, `invalid_name`,
`invalid_matcher`, `invalid_silence`, `invalid_notifier`, `invalid_slo`, `invalid_expression`,
`invalid_query` (400); `rate_limited` (429).

## Consequences

- With export on, every request costs a span (ids, a few attributes, one queue push); sampled-out spans cost
  ids only. Off, JSON logging costs one id pair per span; pretty logging nothing.
- Messages published while export is on are about 70 bytes larger (`traceparent`), which counts toward
  `max_message_bytes`. Consumers see the attribute.
- The engine evaluates in the server process: one replica (ForgeBase's model). Range functions need
  `range` minutes of uptime after a start.
- Gauges refreshed only by `/metrics` scrapes (`forgebase_jobs`, …) are as fresh as the last scrape; the
  signals cover the important ones.
- Dashboards for all of this and SDK wrappers are follow-ups.

## Rollback

0058 only adds tables; 0059 only adds nullable columns. Stop the server and restore the pre-migration backup,
or drop the `_forgebase_alert_*`, `_forgebase_slo*` tables, the two columns and the version rows 58-59. Older
servers ignore the `traceparent` message attribute and job column.
