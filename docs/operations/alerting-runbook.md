# Alerting runbook

One section per built-in alert rule of `forgebase-alerts` ([ADR-0035](../adr/0035-tracing-alerting-slos.md),
[observability.md §2](../observability.md#2-alerts)): what it means, what to look at first, how to fix it. A section's
heading is the rule's **id**, which is also what the alert's `runbook_url` points at:

> Every built-in rule's `runbook_url` is `…/docs/observability.md#<rule id>`, and `observability.md` has a heading
> with that id (§2.3) giving the definition and a one-paragraph hint. **This page is the long form**: the same ids,
> in the same order, with the checks and fixes. SLO burn-rate rules (`slo:<name>:<tier>`) link to
> [observability.md#3-slos](../observability.md#3-slos); their runbook is the last section here. To make the alerts
> link here instead, set `runbook_url` on your own copies of the rules (§2.4 of observability.md) or point your
> notifier's template at this page.

All commands assume an administrator session (`TOKEN` is a bearer access token or a service-account key with
`system:read`), the server at `localhost:8090`, and the data directory `forgebase-data`:

```sh
alias fbapi='curl -s -H "Authorization: Bearer $TOKEN" -H "content-type: application/json"'
```

How to read any alert: `fbapi localhost:8090/api/v1/alerts` lists what is active, `GET /api/v1/alerts/{id}` its history,
and the alert's labels (`subscription`, `queue`, `provider`, …) say where to look. A rule can be switched off with
`PATCH /api/v1/alerts/rules/{id} {"enabled": false}` or `[alerts] disabled_rules`; silence while you work with
`POST /api/v1/alerts/silences`. A signal that cannot be read (the database is down) makes the rules that read it
**skip**: they neither fire nor resolve, so an outage can hide other alerts: check `/health/ready` first.

| Rule id | Alert name | Severity | Fires when |
|---|---|---|---|
| [`http_5xx_ratio_high`](#http_5xx_ratio_high) | HttpServerErrorsHigh | warning | more than 5% of requests answer 5xx over 5 minutes, for 5 minutes |
| [`http_latency_p95_high`](#http_latency_p95_high) | HttpLatencyHigh | warning | p95 latency above 1 s for 10 minutes |
| [`queue_backlog_high`](#queue_backlog_high) | QueueBacklogHigh | warning | a subscription has more than 10 000 unacknowledged messages for 15 minutes |
| [`queue_oldest_unacked_high`](#queue_oldest_unacked_high) | QueueMessagesStuck | warning | the oldest unacknowledged message is over 1 hour old for 10 minutes |
| [`queue_dead_letters_growing`](#queue_dead_letters_growing) | QueueDeadLettersGrowing | warning | any message was dead-lettered in the last 15 minutes |
| [`queue_push_paused`](#queue_push_paused) | QueuePushPaused | critical | a push subscription is paused |
| [`billing_webhook_signature_invalid`](#billing_webhook_signature_invalid) | BillingWebhookSignaturesInvalid | warning | more than 5 webhooks of one provider failed the signature check in 15 minutes |
| [`job_failure_ratio_high`](#job_failure_ratio_high) | JobFailuresHigh | warning | more than 25% of a queue's job attempts failed over 15 minutes, for 15 minutes |
| [`jobs_dead_lettered`](#jobs_dead_lettered) | JobsDeadLettered | warning | jobs of a queue went to the dead-letter queue in the last 15 minutes |
| [`disk_free_low`](#disk_free_low) | DiskSpaceLow | warning | under 10% of the data directory's disk is free for 5 minutes |
| [`disk_free_critical`](#disk_free_critical) | DiskSpaceCritical | critical | under 5% free for 5 minutes |
| [`backup_stale`](#backup_stale) | BackupStale | warning | the newest snapshot is more than 26 hours old |
| [`tls_cert_expiring`](#tls_cert_expiring) | TlsCertificateExpiring | warning | a certificate expires within 14 days |
| [`tls_cert_expiry_critical`](#tls_cert_expiry_critical) | TlsCertificateExpiringSoon | critical | a certificate expires within 3 days |
| [`slo:<name>:<tier>`](#slo-burn-rate-rules-slonametier) | SloBurnRateHigh | critical or warning | an SLO burns its error budget too fast |

---

## `http_5xx_ratio_high`

**Meaning.** More than 5% of HTTP requests answered with a 5xx status for five minutes (the ratio is over all routes;
requests that never get a response, because a proxy gave up, are not counted).

**First checks.**
1. `GET /health/ready`: which check fails (`database`, `storage`, …)?
2. Which routes: `fbapi localhost:8090/metrics | grep 'forgebase_http_requests_total{.*status="5'` (sorted by `route`).
3. The server log, filtered on the failing `request_id` (every 5xx response carries `X-Request-Id`); with tracing on, the trace id
   in `traceparent`. The server log's `error` lines name the cause (a database error, a panic caught, a function trap).
4. Did something change: a deploy, a migration (`GET /api/system/info` shows the schema version), a new function version, a
   full disk (see `disk_free_critical`)?

**Fixes.**
- Database unreachable or restarting: [HA runbook](ha-failover.md); the bundled engine is restarted with backoff, and `503
  database_unavailable` clears when it is back. A wedged primary may need a promote.
- One function or route: roll back the function (`forgebase functions deploy` of the previous version), disable the route's feature in
  the registry if it is optional.
- Disk full: free space (`disk_free_critical`).
- After a bad deploy: [upgrades.md](upgrades.md#rollback).

## `http_latency_p95_high`

**Meaning.** The 95th percentile of request latency is over one second for ten minutes. It is the latency of
requests ForgeBase answered, across all routes, so one slow bulk route can lift it.

**First checks.**
1. The slow routes: the histogram `forgebase_http_request_duration_seconds_bucket{route=…}`; compare `rate()` per route.
2. The database: `forgebase_db_query_duration_seconds` (`mode` read or write) and `forgebase_db_pool_connections_in_use` against the pool size
   (a pool at its limit queues requests); the **slow-query log** (queries over the threshold, replica reads included).
3. The host: CPU and memory (`process_cpu_seconds_total`, `process_resident_memory_bytes`), the disk (`disk_free_*`, I/O wait),
   vacuum or `pg_basebackup` running at the same time (a backup or a **drill** on the same volume competes for I/O: move `--work-dir`).
4. Functions: slow ones show in their execution logs.

**Fixes.** Add the missing index (the collection's index screen, `index_builds`); raise `[database] read_pool_size` for read-heavy loads; move
heavy reads to a replica (`[database.replicas] read_routing`); fix or time-box the slow function; schedule backups and drills off peak.

## `queue_backlog_high`

**Meaning.** A subscription has over 10 000 messages unacknowledged for fifteen minutes: its consumers (pull) or its push endpoint do not keep up.
The labels name the `subscription` and `topic`.

**First checks.**
1. `fbapi localhost:8090/api/v1/queues/subscriptions/<sub>/metrics`: backlog, in-flight, oldest age, ack rate.
2. Pull subscription: are consumers running and **acknowledging**? (`ack_deadline_ms` expiring means redelivery, not progress.)
3. Push subscription: `fbapi localhost:8090/api/v1/queues/subscriptions/<sub>/attempts`: status, latency and the (truncated) response body of the endpoint.
4. Is publishing simply faster than consuming right now (a bulk import)?

**Fixes.** Add consumers or raise `[queue] push_concurrency`; fix the endpoint; lengthen `ack_deadline_ms` for slow handlers; for a poison message, let it
dead-letter (`max_delivery_attempts`) and `redrive` it later; if the backlog is stale data, `seek` the subscription forward (it drops what
you skip: only for data you may lose).

## `queue_oldest_unacked_high`

**Meaning.** The oldest unacknowledged message of a subscription is over an hour old for ten minutes: something is **stuck**, which a short
backlog can hide.

**First checks.** `…/subscriptions/<sub>/metrics` for `oldest_unacked`; then the dead letters and attempts as above; a message with an ordering key
blocks the messages behind it while it is being retried (`ordering = true` subscriptions deliver one per key).

**Fixes.** Find why that one message does not complete (a handler crash, a payload the consumer cannot parse); fix the consumer and let it retry (or let it dead-letter at `max_delivery_attempts`); `retry` forces a push attempt now (`POST …/subscriptions/<sub>/retry`).

## `queue_dead_letters_growing`

**Meaning.** At least one message ran out of delivery attempts in the last 15 minutes and was dead-lettered. The rule is edge-like: it fires while the
15-minute `increase` of `forgebase:queue_dead_lettered_total` is above zero.

**First checks.** `fbapi localhost:8090/api/v1/queues/subscriptions/<sub>/dead-letters`: payload, delivery history, last error. Look for a pattern (one
tenant, one message kind, one deploy).

**Fixes.** Fix the consumer or endpoint, then `POST …/subscriptions/<sub>/redrive` to put the dead letters back. Raise `max_delivery_attempts` or the
backoff if the downstream is flaky rather than broken. Delete dead letters you have decided to drop (they expire with
`[queue] dead_letter_retention_days`).

## `queue_push_paused`

**Meaning.** A push subscription is **paused**, by an administrator or by the circuit breaker after repeated failures (`pause_after_failures`). **Nothing is
delivered** until it is resumed; messages accumulate (up to the topic's retention: do not leave it).

**First checks.**
1. `…/subscriptions/<sub>/attempts`: why the breaker opened (4xx/5xx, timeout, `push_endpoint_blocked`, a changed certificate, a function error).
2. `POST …/subscriptions/<sub>/test` sends a signed test delivery now (10 a minute) and records it.
3. Is the signature failing at the receiver? The `X-Forgebase-Signature` secret was rotated (`secret/rotate`) and the consumer still has the old one.

**Fixes.** Repair the endpoint (deploy, DNS, certificate, secret), `test`, then `POST …/subscriptions/<sub>/resume`. Redrive dead letters made in the
meantime. Retention: if the pause outlived the topic's `retention`, expired messages are gone: seek or republish from the source.

## `billing_webhook_signature_invalid`

**Meaning.** More than five webhooks from one **provider** (the `provider` label) failed signature verification in fifteen minutes. Either the signing
secret stored in ForgeBase is not the provider's current one (the usual case after a rotation), or someone is posting forged requests.

**First checks.**
1. The inspector: `fbapi localhost:8090/api/v1/billing/webhook-deliveries` (outcome `signature_invalid`, source address, headers kept, body kept
   within the budget).
2. Did the provider's webhook secret change recently? Compare with the provider dashboard.
3. Source addresses: the provider's published ranges, or something else?

**Fixes.** Update the secret in ForgeBase's billing provider settings, then **re-verify** the held deliveries
(`POST …/billing/webhook-deliveries/{id}/reverify`) and **replay** the ones that should have been applied (`…/replay`), so no event is lost.
If the requests are forged, they were already rejected (nothing is applied on a failed signature): block the source, keep the alert as a canary, and rate-limit at your proxy.

## `job_failure_ratio_high`

**Meaning.** More than a quarter of a queue's job attempts failed (`failed` or `dead` outcomes of `forgebase_jobs_completed_total`) over fifteen minutes, for fifteen
minutes. The `queue` label says which.

**First checks.**
1. `fbapi 'localhost:8090/api/v1/jobs?status=queued'` and `…/jobs?status=dead`, `GET /api/v1/jobs/{id}/attempts` for the error text and timing.
2. What the failing kind has in common (a function, `storage.reconcile`, a billing job, `system.backup_drill`).
3. A dependency: the database, an upstream API, disk space.

**Fixes.** Fix the dependency or handler; retries with backoff continue meanwhile. Pause the queue if it is making things worse
(`POST /api/v1/jobs/queues/{name}/pause`, `…/resume` after). A **drill** job that fails is a finished job with `outcome: "failed"`
in its result: read the report ([disaster-recovery.md §5](disaster-recovery.md#5-drills)), a failed *job* means the child process could not run.

## `jobs_dead_lettered`

**Meaning.** Jobs of a queue exhausted their attempts and went to the dead-letter queue in the last 15 minutes.

**First checks.** `GET /api/v1/jobs?status=dead` (the dead-letter queue), `GET /api/v1/jobs/{id}/attempts` and `…/events` for each; the cause is usually
the same for a burst.

**Fixes.** Fix the cause, then `POST /api/v1/jobs/{id}/retry` per job (or from the Jobs screen). Delete what you decide to drop. If it is a scheduled
job that always dies, disable or fix the schedule (`PATCH /api/v1/jobs/schedules/{id}`).

## `disk_free_low`

**Meaning.** Less than 10% of the filesystem that holds the data directory is free, for five minutes.

**First checks.**
1. What is big: `du -xh --max-depth=2 forgebase-data | sort -h | tail`. Usual suspects: `backups/snapshots/` and `backups/pre-restore-*` and
   `backups/pre-migrate-*` (**never auto-deleted**), `tmp/` (leftover restore or **drill scratch directories**, `tmp/drill-*`), `storage/`, `postgres/data` and
   its WAL (`pg_wal`: a stuck replica's slot keeps WAL up to the 8 GB cap), SQLite's `-wal`, logs if you log to the same disk.
2. `forgebase doctor` (it names leftover staging and the WAL size).
3. Growth rate: is this a trend (storage, tables) or an event (a runaway job, a backup)?

**Fixes.**
- Prune: `forgebase backup prune --keep-last 14 --max-age-days 30`; delete `backups/pre-restore-*` and `backups/pre-migrate-*` you no longer need
  (after you have checked the restored data); remove `tmp/drill-*` of a killed drill (they are swept after six hours).
- Move snapshots **off-site** and keep fewer locally ([disaster-recovery.md §3](disaster-recovery.md#3-strategy)).
- Run drills with `--work-dir` on another volume.
- Delete data you can: expired storage versions, the queue's old topics, old audit rows; run the storage reconciler (`storage.reconcile`).
- Grow the volume. On PostgreSQL, a replica that is far behind holds WAL: rebuild or remove it.

## `disk_free_critical`

**Meaning.** Less than 5% is free for five minutes. **Writes fail soon**: PostgreSQL and SQLite stop accepting writes when the disk is full, a snapshot or a
drill cannot complete, and a full volume on PostgreSQL can end in a crash that needs recovery.

**First checks and fixes.** As `disk_free_low`, **now**: free space first (stop a drill or backup that is writing; delete `tmp/drill-*`, old
`backups/pre-restore-*`), then diagnose. If the server is already failing writes, stop it cleanly (`systemctl stop forgebase`, so PostgreSQL shuts down with a
checkpoint) before you free space, rather than letting it crash. Do not delete anything under `postgres/data` or `storage/` by hand.

## `backup_stale`

**Meaning.** The newest **snapshot** in `backups/snapshots/` is more than 26 hours old. Two limits of what the rule sees, worth knowing:

- It reads only snapshots of this data directory (`forgebase backup` without a destination, the REST snapshot, the scheduled one). A backup you wrote to
  an explicit directory or only to an off-site store does not count.
- With **no snapshot at all** the signal is absent and the rule does **not** fire. Add your own rule for that:
  `absent(forgebase:backup_last_success_timestamp_seconds)` (op `>`, threshold `0`), or check the schedule exists.

**First checks.**
1. `forgebase backup list`: the newest snapshot and its time. `ls -l forgebase-data/backups/snapshots`; a `*.partial` directory is a backup that did not finish.
2. The scheduler: `systemctl list-timers`, `crontab -l`, the Kubernetes CronJob (`kubectl get cronjob,job -n forgebase`; its last job's logs).
3. Run one by hand and read the error: `forgebase backup --label manual`. Common: disk full (`disk_free_*`), the bundled engine not running (the CLI starts it
   under the lock, so a running server must be the one that holds it), `pg_basebackup` refused, permissions on `backups/`.
4. Does `forgebase backup verify latest` pass? A snapshot whose manifest is unreadable is not listed, so it does not reset the age.

**Fixes.** Repair the schedule, take a snapshot now, then **verify and drill** it ([disaster-recovery.md §5](disaster-recovery.md#5-drills)). If your interval is
longer than 26 hours by design, add a custom rule with your own threshold and disable this one. Related: add a rule on the drill metrics:

```toml
[[alerts.rules]]
name = "BackupDrillStale"
expr = "time() - forgebase_backup_last_drill_success_timestamp_seconds"
op = ">"
threshold = 691200            # 8 days: weekly drill plus a day of slack
for = "0s"
severity = "warning"
annotations = { summary = "No restore drill has passed for {{ $value | duration }}", runbook_url = "https://github.com/farzanshibu/forgebase/blob/main/docs/operations/disaster-recovery.md#5-drills" }

[[alerts.rules]]
name = "BackupDrillFailed"
expr = "forgebase_backup_last_drill_ok"
op = "=="
threshold = 0
for = "0s"
severity = "critical"
annotations = { summary = "The last restore drill failed: this backup may not restore", runbook_url = "https://github.com/farzanshibu/forgebase/blob/main/docs/operations/disaster-recovery.md#5-drills" }
```

(The drill gauges are set when `/metrics` is scraped, so they are as fresh as your last scrape.)

## `tls_cert_expiring`

**Meaning.** A TLS certificate expires within 14 days. The rule reads the gauge named by `[alerts] tls_cert_expiry_metric` (default
`forgebase_tls_certificate_expiry_timestamp_seconds`, a Unix time; it stays quiet while that gauge does not exist). Native TLS
(`[tls] mode = files|acme`, ADR-0033) exports that gauge next to `forgebase_tls_certificate_expiry_seconds` (seconds remaining), so the default works
with native TLS. With TLS at a proxy, point the rule at your proxy's or an exporter's timestamp (`probe_ssl_earliest_cert_expiry`); `forgebase doctor`
(its `tls` check warns under 14 days) and the `tls` entry of `GET /api/system/info` show the same expiry.

**First checks.**
1. `forgebase doctor`, `fbapi localhost:8090/api/system/info | jq .tls`: expiry, issuer, **last renewal error and the next attempt**.
2. ACME: `[tls.acme]` domains and challenges; can the CA reach port 443 (`tls-alpn-01`) or `http_port` as 80 (`http-01`)? Rate limits (a restore or a drill that
   lost `tls/` asks for a new certificate: **`[backup] include_tls`** keeps it)? Is another server answering for the domain (ACME runs on one server per domain)?
3. Files mode: did the platform (cert-manager, your PKI) renew the file, and does ForgeBase reload it (`reload_interval_secs`, or `SIGHUP`)?
4. Behind a proxy: the proxy owns the certificate, not ForgeBase.

**Fixes.** ACME: fix reachability or the account, restart if the state is damaged (the old certificate keeps being served meanwhile; failures retry after 2
minutes, doubling to 6 hours); with a restore that dropped `tls/`, wait out the CA's rate limit or restore a backup that has it. Files: install the renewed
files and `kill -HUP` (or wait `reload_interval_secs`). Check the chain after: `openssl s_client -connect host:443 -servername host </dev/null | openssl x509 -noout -dates`.

## `tls_cert_expiry_critical`

**Meaning.** The same, within **3 days**: clients refuse the connection once it expires. Same checks as `tls_cert_expiring`. If renewal cannot be fixed in time,
install a certificate by hand (`[tls] mode = "files"` with a certificate from any CA, or terminate TLS at a proxy with a valid one) rather than let it lapse.

---

## SLO burn-rate rules (`slo:<name>:<tier>`)

**Meaning.** Alert name `SloBurnRateHigh`, labels `slo` and `tier`. An SLO's error budget (`1 − objective` of requests over its window) is being spent faster
than a tier allows, with **both** windows over the threshold (ADR-0035 §5):

| Tier | Windows (long / short) | Spends this much budget | Severity | Reading |
|---|---|---|---|---|
| `page_fast` | 1 h / 5 min | 2% in an hour | critical | an outage now: act immediately |
| `page_slow` | 6 h / 30 min | 5% in six hours | critical | a significant degradation that is not stopping |
| `ticket` | 3 d / 6 h | 10% in three days | warning | a slow leak: schedule the fix |

**First checks.**
1. `GET /api/v1/slos/<id>/status`: SLI, budget remaining, burn rates per window, which tiers fire.
2. What the SLO measures (`route_prefix`/`route_regex`, `availability` or `latency`, `latency_threshold_ms`): look at those routes' 5xx or slow answers, as in
   [`http_5xx_ratio_high`](#http_5xx_ratio_high) and [`http_latency_p95_high`](#http_latency_p95_high), restricted to the SLO's routes.
3. When it started (the alert's history) and what changed then.

**Fixes.** Fix the cause as for those two rules. A tier that fires from a **bad objective** (too tight for what the service does) is a design problem: change the objective
or window with `PUT /api/v1/slos/{id}` after agreeing it, not by disabling the alert. `"alerting": false` on the SLO removes its rules; each rule can be switched off like any other.

---

## Adding your own

A rule you add yourself should carry a `runbook_url`. Copy the shape of the drill rules above; `POST /api/v1/alerts/query` evaluates an expression on the spot so you can
see its value before you commit to a threshold.
