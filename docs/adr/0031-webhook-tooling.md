# ADR-0031: Webhook delivery tooling

- Status: accepted
- Date: 2026-11-14
- Scope: `crates/forgebase-queue` (`push.rs`, `tooling.rs`, `catalog.rs`, HTTP routes),
  migrations `0050_queue_push_attempts`, `0051_queue_push_health`,
  `0052_queue_redrive_catalog`, `[queue]` settings, `crates/forgebase-cli/src/queues.rs`
  (`forgebase queues listen|test|redrive|retry|rotate-secret|pause|resume|attempts|catalog`).
- Builds on: ADR-0022 (topics, subscriptions, push delivery §13, dead letters §5),
  ADR-0029 §1.6 (pgmq deliveries), ADR-0017 (the SSRF-safe fetcher), ADR-0008 (audit log).
- HTTP reference: [api.md §11.1](../api.md#webhook-delivery-tooling).

## Context

ADR-0022 §13 pushes signed JSON to webhooks and retries with backoff, an attempt
limit and a dead-letter topic. Operating it was blind: a delivery history kept only
attempt, time, consumer, outcome and error; there was no way to check an endpoint
without publishing a real message, to deliver dead letters again, to change a
signing secret without breaking receivers, to stop hammering a dead endpoint, to
discover what can be subscribed to, or to receive a topic on a laptop. Hosted
webhook services (Stripe, Svix, GitHub) all offer these; this ADR adds them.

## Decisions

### 1. Push attempt log

`_forgebase_queue_push_attempts` (0050) gets one row per webhook or function call:
message id and delivery attempt, `test` flag, endpoint, start time, latency, outcome
(`success`, `failure`), HTTP status, an **error class** (`timeout`, `dns`,
`connect`, `tls`, `ssrf_refused`, `http_status`, `redirect`, `protocol`,
`invalid_request`, `config`, `function`; derived from the fetcher's error), the
request headers sent with every signature masked (`t=<ts>,v1=<redacted>`),
allowlisted response headers (`content-type`, `retry-after`, request ids, …;
cookies are never stored) and the response body cut to `[queue]
push_response_body_bytes` (default 4 KiB, lossy UTF-8, with the full size and a
`truncated` flag). The row is written **in the transaction of the delivery's ack or
nack**, together with the endpoint health (§5), so the log never disagrees with the
delivery state. Retention: the newest `push_attempts_per_subscription` (1000) per
subscription and at most `push_attempts_retention_secs` (7 days), pruned by the
existing maintenance pass. The table is placed with its subscription on sharded
deployments (`COPY`, like dead letters). Routes: list (newest first; filters
outcome, message id, time window, test; `before` paging) and get one.

### 2. Test send

`POST …/test` sends a synthetic event now: the delivery envelope with
`"type": "forgebase.test"`, a message `test-<uuid>` (`seq` 0, attribute
`forgebase.test`), the same signed headers plus `X-Forgebase-Test: 1`; a function
endpoint is invoked with it. It is not a message: nothing is published, no cursor,
delivery, total, rate, health counter or dead letter changes; only the attempt log
keeps it (`test = 1`, excluded from health). It is limited per subscription to
`push_test_sends_per_minute` (10; in memory, sliding minute; `429 rate_limited`).
It works on a paused subscription, so an endpoint can be checked before resuming.

### 3. Redrive and retry now

`POST …/redrive` takes `ids` (1-1000) or `all` (optionally `since`, an attribute
`filter` in the subscription filter syntax, and `limit`). In one write transaction
each record is first marked (`UPDATE … SET redriven_at, redriven_by WHERE
redriven_at IS NULL`); only the call whose update changed the row re-delivers it, so
a record is never redriven twice, also by concurrent callers (one writer on SQLite;
the row lock and re-check of the predicate under PostgreSQL's read committed). The
message is owed again with a **fresh attempt counter**:

- still retained: a delivery row (or pgmq entry) for its `seq`, unless one exists, or
  unless the subscription's cursor has not reached it (it will be read anyway);
- dropped by retention: stored again on the topic **for that subscription only**
  (`_forgebase_queue_messages.only_subscription`, 0052) with the same id, data,
  attributes and ordering key, a new `seq` and publish time, publisher
  `system:queue`; materialisation, metrics and topic listings of other
  subscriptions skip it. A record-change message is delivered as it was stored in
  the dead letter (the owner's view when it was dead-lettered).

Each redrive is written to the audit log (`queue.dead_letters.redrive`).
`POST …/retry` makes deliveries that wait for a backoff ready now (all, or by
message id); leased deliveries are untouched.

### 4. Zero-downtime secret rotation

`POST …/secret/rotate` (`grace_ms`, default 24 h, at most 7 days) stores a new
sealed secret (keyring AAD `queue:push:<name>` as before) and keeps the old one
sealed in `push_secret_previous` until `push_secret_previous_until` (0051). While
the window lasts `X-Forgebase-Signature` is `t=<ts>,v1=<new>,v1=<old>`; afterwards
only `v1=<new>`. **Receivers must accept a request when any `v1` matches** their
secret; `push::verify` does. A receiver that only reads the last `v1` keeps working
with the old secret during the window. `…/secret/expire-previous` ends the window
now; maintenance clears expired previous secrets, and signing checks the deadline
itself. A rotation inside a window replaces the older secret. A changed endpoint
drops the previous secret. Rotations are audited.

### 5. Endpoint health and the circuit breaker

Per push subscription (0051): `consecutive_failures`, `last_success_at`,
`last_failure_at`, `last_error`, updated with each attempt. The subscription metrics
add `push`: success rate and nearest-rank p50/p95/p99 latency over the attempts of
the last hour (newest 1000, test sends excluded) and the pause state. Prometheus
(`forgebase_observability::names`): `forgebase_queue_push_attempts_total`
(`subscription`, `outcome`), `forgebase_queue_push_latency_seconds` (histogram),
`forgebase_queue_push_consecutive_failures`, `forgebase_queue_push_paused`,
`forgebase_queue_push_auto_paused_total`,
`forgebase_queue_push_last_success_timestamp_seconds`,
`forgebase_queue_redriven_total`; the gauges are refreshed every maintenance round.

`pause_after_failures` (1-1000, default off) opens the breaker: the failure that
reaches it sets `paused_at` and `paused_reason` (`auto: N consecutive failed
deliveries (last: …)`), emits the counter, a `warn` log with
`event = "queue.push.auto_paused"` and an audit event, and releases the rest of the
claimed batch without counting an attempt. A paused subscription is skipped by the
push rounds; its messages keep accumulating and **none is dead-lettered while it is
paused** (the triggering message and expired leases are retried instead). `…/pause`
pauses by hand; `…/resume` clears the pause and the failure count, makes every
delayed delivery ready and wakes the dispatcher, which delivers the backlog in
sequence order (per ordering key on ordered subscriptions).

### 6. Event catalog

`GET /queues/catalog` lists every topic the caller may read (as `GET /topics`
filters them) with `description`, `data_schema` (the topic schema), and an
`example`: the topic's `example` (new topic field, checked against the schema on
create and change), else one derived from the schema (`const`, `examples`,
`default`, `enum`, then `type` with bounds), else none (`example_source` says
which). A record-source topic lists its `event_types` (`record.created`,
`record.updated`, `record.deleted`, as configured) with the filter that selects each
(`{"forgebase.op": "create"}`), the change message schema whose `record` is built
from the collection's fields (hidden and password fields left out; a plain object
when the caller may not read the collection's schema), and a derived example.

### 7. Subscription expiry and `forgebase queues listen`

A subscription may have `ttl_ms` (60 s to 31 days; a change with `ttl_ms` extends
it, `0` removes it): maintenance deletes it after `expires_at`. `forgebase queues
listen <topic> --forward-to http://localhost:…` creates `listen-<random>` (pull,
optional `--filter`, `ttl_ms` = `--ttl`, default 10 min, renewed every third),
prints a session secret, consumes it over the WebSocket streaming pull, signs each
delivery with the session secret in the same header format, POSTs it to the local
URL (plain `http://`, any host: it runs on the developer's machine), acks on `2xx`,
nacks otherwise (1 s backoff), prints one line per delivery, and deletes the
subscription on Ctrl-C. A crashed session's subscription expires on its own. The
other commands (`test`, `redrive`, `retry`, `rotate-secret`, `pause`, `resume`,
`attempts`, `catalog`) are thin calls of the routes.

### 8. HTTP

Ten operations, `operationId` prefixed `queues_`: `queues_list_push_attempts`,
`queues_get_push_attempt`, `queues_test_push`, `queues_redrive`,
`queues_retry_now`, `queues_rotate_secret`, `queues_expire_previous_secret`,
`queues_pause`, `queues_resume`, `queues_event_catalog`. Authorisation runs before
the body is read: `QueueRead` for reads, `QueueAdmin` for everything else, on the
subscription (its owner holds every permission). New error codes:
`attempt_not_found` (404), `not_push_subscription`, `not_webhook_subscription` (409),
`rate_limited` (429), `invalid_redrive`, `invalid_example`, `invalid_description` (400).

## Consequences

- Every push attempt costs one more row in the transaction that already acks or
  nacks it; push throughput is bounded by the endpoint, not by this write.
- The error class of fetch failures is read from the fetcher's error text
  (`forgebase-functions` has no typed causes); a changed message falls back to
  `protocol`.
- The test-send limit is per process (lost on restart, not shared by a cluster).
- A redriven copy of an expired message has a new `seq` and publish time; a seek of
  the subscription to an earlier time delivers it again, like any message.

## Rollback

The three migrations only add a table and nullable (or defaulted) columns. To roll
back, stop the server and restore the pre-migration backup, or drop
`_forgebase_queue_push_attempts`, the added columns and the version rows 50-52.
Older servers ignore the columns; receivers that verify only the last `v1` keep
working with the current secret.
