# ADR-0022: Durable topics and queues (publish/subscribe)

- Status: accepted
- Date: 2026-11-09
- Work package: FB-63 (MDF-55), addendum `docs/requirements/queues-pubsub-long-tasks.md` §A
- Scope: `crates/forgebase-queue`, migration `0021_queue.sql`, `[queue]` config, `/api/v1/queues`.
- Builds on: ADR-0001 (one writer, error model), ADR-0003 (`Authorizer`), ADR-0007 (jobs: leases,
  backoff), ADR-0010 (quotas), ADR-0011 (realtime outbox), ADR-0015 (outbound HTTPS),
  ADR-0017 (functions, SSRF-safe fetch).

## Context

FB-61 jobs are tasks for workers; FB-31 KV pub/sub is in memory and lost on restart; FB-40 realtime
pushes record changes to browsers. None is a durable message broker with fan-out to independent
consumer groups. FB-63 adds one in the style of Google Pub/Sub, SQS + SNS or NATS JetStream, stored
in the system database so it needs no external service.

## Decisions

1. **Storage** (migration 0021). `_forgebase_queue_topics`, `_forgebase_queue_messages` (one row
   per published message; `seq INTEGER PRIMARY KEY AUTOINCREMENT` is the global sequence),
   `_forgebase_queue_subscriptions` (each with its own `cursor_seq`), `_forgebase_queue_deliveries`
   (one row per message a subscription still owes: `ready` or `leased`), `_forgebase_queue_dedup`,
   `_forgebase_queue_dead_letters`, `_forgebase_queue_snapshots` + `_items`, `_forgebase_queue_rates`
   (per-minute counters), `_forgebase_queue_state` (relay cursor). Times are epoch milliseconds
   (API: RFC 3339). Foreign keys cascade: deleting a topic deletes its messages and subscriptions;
   pruning a message deletes its deliveries.
2. **Fan-out by cursor, delivery rows on demand.** A publish writes one message row, whatever the
   number of subscriptions. Each subscription reads the topic from its cursor in sequence order
   and creates delivery rows for messages that pass its filter ("materialization"; at most 2000 per
   claim, and every subscription each maintenance round). Because SQLite has one writer, sequence
   numbers are assigned in commit order, so a cursor can never pass a message that commits later
   with a lower number. (A future Postgres backend needs a commit-ordered cursor, e.g. an outbox
   table drained by one writer.)
3. **Competing consumers.** A claim is one write transaction: reap expired leases, materialize,
   select ready rows (`available_at <= now`) by sequence, set `state = 'leased'`,
   `lease_id` (random ack id), `lease_until = now + ack_deadline`. Two consumers can never hold
   the same row. An ack deletes the row by `(subscription, lease_id)`.
4. **Redelivery and retries.** A missed deadline or a nack makes the row `ready` again at
   `now + backoff(attempts)` (ADR-0007 formula without jitter, base and cap per subscription;
   base `0` retries at once). A lease id stays valid after expiry until the message is leased
   again, so a late ack still prevents a redelivery; after a nack it is invalid. Deadline changes:
   `modify-deadline` with 1 ms-10 min, `0` = nack. At-least-once overall.
5. **Dead letters.** With `max_delivery_attempts`, the attempt that reaches the limit (nack or
   expiry) moves the message off the subscription: a `_forgebase_queue_dead_letters` record with
   the delivery history (attempt, time, consumer, outcome, error; last 25), and, when
   `dead_letter_topic` is set, a copy published there with `forgebase.dead_letter.*` attributes and
   the history in `delivery_history`. Without a limit messages are retried until retention drops
   them.
6. **Ordering.** On an ordered subscription a row with an ordering key is claimable only if no row
   of the same subscription and key with a lower sequence exists (ready, delayed or leased). So a
   key's messages are delivered in publish order, one at a time; a nacked head is retried before
   its successors; a dead-lettered head unblocks them. Ordering and filters are immutable.
7. **Dedup.** `(topic, dedup_id)` rows expire after the topic's window (default 10 min). A repeat
   inside the window stores nothing and returns the first message id (`duplicate: true`); a repeat
   inside one batch counts too. Duplicates use no quota.
8. **Transactional publish.** `QueueService::publish_in_tx(tx, ...)` writes the messages inside the
   caller's write transaction: they exist exactly when it commits. `committed()` wakes consumers.
   **Record sources** connect topics to the FB-40 transactional outbox: a topic with
   `source: {collection, ops}` receives one message per committed record change (data: op,
   collection, record id, row snapshot, tenant, actor; ordering key = record id; dedup id
   `outbox:<seq>`), relayed from `_forgebase_realtime_outbox` with a cursor that advances in the
   same transaction as the inserts (exactly once). A rolled-back record write leaves no outbox row
   and so no message. Only administrators may create record sources (the messages bypass the
   collection's read rules); source topics take no direct publishes and no schema.
9. **Topics.** Retention by time (1 min-31 days, default 7 days) and optionally by size (oldest
   messages dropped first), applied by maintenance; unacknowledged messages past retention are
   dropped too (as in Pub/Sub). Message size limit (default and cap from `[queue]
   max_message_bytes`, 1 MiB; size = data JSON + attributes + ordering key). Optional JSON Schema
   for `data`: an in-house validator for a documented subset (`type`, `enum`, `const`,
   `properties`, `required`, `additionalProperties`, `items`, length/size/number bounds,
   `allOf`/`anyOf`/`oneOf`/`not`; annotations ignored). Any other keyword (`$ref`, `pattern`, ...)
   is refused when the schema is set, so nothing is silently unenforced. No new dependency.
10. **Filters.** A JSON object of attribute conditions, ANDed: string (equals), array (one of),
    `{eq, ne, in, not_in, prefix, exists}`, and `$any: [filter, ...]` for OR.
11. **Pull.** `POST .../pull` with `max_messages` (1-1000) and `wait_ms` long polling (capped by
    `[queue] max_pull_wait_ms`, default 20 s, below the request timeout). Waiters wake on a
    `watch` channel bumped after commits (publish, nack, ack, seek) and poll every
    `poll_interval` for backoffs that end.
12. **Streaming pull.** WebSocket `GET .../stream` (JSON frames: server `messages`, `result`,
    `error`; client `ack`, `nack`, `modify_deadline`, `flow`) and SSE `GET .../events` (one
    `message` event per delivery; acks via REST). Flow control: each stream is a consumer id
    (`ws-…`, `sse-…`); it is given at most `max_messages` / `max_bytes` minus what it holds in
    unexpired leases (one message always fits when it holds none). A closing WebSocket releases its
    leases without counting the attempt; SSE leases expire with their deadline. The server closes
    all streams before graceful shutdown.
13. **Push.** A push subscription is consumed by the background task (consumer `push`), up to
    `push_batch` messages per round, delivered one after another (ordering holds). Webhooks:
    `POST` JSON `{subscription, delivery_attempt, message}` with
    `X-Forgebase-Signature: t=<unix s>,v1=<hex HMAC-SHA256(secret, "<t>.<body>")>`; the secret
    (`whsec_…`) is generated per subscription, sealed with the data directory's keyring (AAD
    `queue:push:<name>`), shown once on create (or when the endpoint changes). Requests go through
    `forgebase_functions::Fetcher` (FB-25 TLS settings, every resolved address must be public, no
    DNS rebinding, redirects re-checked); literal private IPs are refused at configuration time;
    `https://` only unless `[queue] allow_http_push`. Functions: `FunctionService::invoke(name,
    Trigger::Job, body)`. `2xx` / success acks; anything else nacks with the error recorded, so
    the subscription's backoff, attempt limit and dead-letter topic apply.
14. **Replay.** Seek to a time: delivery rows are dropped and the cursor is set to the highest
    sequence published before it (everything from it on is delivered again, inside retention). A
    snapshot saves the cursor and the set of owed sequences; seeking to it restores both.
    Snapshots expire with the topic's retention.
15. **Metrics.** Per topic: retained messages/bytes, oldest age, totals, last-minute count and
    5-minute publish rate. Per subscription: backlog (owed rows + unread messages), bytes, oldest
    unacked age, ready/delayed/in-flight, delivered/acked/nacked/expired/dead-lettered totals,
    dead-letter records, ack rate. Prometheus counters `forgebase_queue_*_total`.
16. **Metering and quotas (FB-24).** Publishes by billable callers reserve `queue.messages`
    (count) and `queue.bytes` before the transaction and commit the reservation with the actual
    non-duplicate amount after it (release on failure), so a refusal (`429 quota_exceeded`)
    publishes nothing. Transactional publishes dry-run the quota (read pool only, safe inside the
    caller's transaction) and are metered after commit by the maintenance pass (`metered = 0`
    rows, idempotency key per message). Features missing from the catalog are not metered;
    administrators and the system have no billing subject.
17. **Authorisation.** New `Action`s `QueueRead`, `QueuePublish`, `QueueConsume`, `QueueAdmin`
    (policy keys `engine:queue_read`, `engine:queue_publish`, `engine:queue_consume`,
    `engine:queue_admin`). HTTP handlers authorise before reading the body. Streams re-check
    `QueueConsume` before every batch.
18. **HTTP.** `ApiExtension` at `/api/v1/queues`, 26 operations, every `operationId` prefixed
    `queues_`. Errors: `413 message_too_large`, `429 quota_exceeded`, `409 topic_exists`,
    `subscription_exists`, `snapshot_exists`, `topic_has_subscriptions` (delete with
    `force=true`), `topic_has_source`, `push_subscription` (pull or stream of a push
    subscription), `404 topic_not_found`, `subscription_not_found`, `snapshot_not_found`.
19. **Config** `[queue]`: `enabled` (default true), `max_message_bytes`, `default_retention_secs`,
    `max_retention_secs`, `dedup_window_secs`, `max_pull_wait_ms`, `poll_interval_ms`,
    `push_timeout_secs`, `push_concurrency`, `allow_http_push`, `dead_letter_retention_days`.
20. **Lifecycle.** The server opens the service after functions, mounts it, starts its background
    task (maintenance every `poll_interval_ms`, push whenever woken), and on shutdown closes
    streams first, then stops the task before the database closes.

## Consequences

- Every claim takes the single writer; idle polls first do a cheap read. Throughput is bounded by
  SQLite write transactions (thousands of messages per second in batches), which suits an embedded
  backend; a heavy workload should batch publishes and pulls.
- Topics and subscriptions are instance-wide (not tenant-scoped) and authorised per action, not
  per topic; per-topic rules are a follow-up.
- Message `data` is JSON (no raw bytes); binary payloads go base64 in a string.

## Rollback

Migration 0021 only adds tables. To roll back, stop the server, restore the pre-migration backup
the migrator writes, or drop the `_forgebase_queue_*` tables and the version-21 row of the
migrations table.
