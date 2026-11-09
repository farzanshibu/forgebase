# Addendum: message queues, pub/sub and long-running tasks

_Human request, 2026-11-09: "this should include a queue system, i.e. publish and subscribe for queues, and long background tasks done properly."_

## What already exists (do not rebuild)

- **FB-61 durable jobs** (`crates/forgebase-jobs`): enqueue, delay, priority, idempotency key, cron, retries with backoff, dead-letter queue, leases with heartbeats, crash re-claim, cancellation, external workers over HTTP.
- **FB-31 KV** (`crates/forgebase-kv`): Redis-style pub/sub and streams. Pub/sub there is in-memory and lost on restart. Streams have no consumer groups.
- **FB-40 realtime** (`crates/forgebase-realtime`): transactional outbox, event bus, WebSocket and SSE subscriptions with authz filtering, cursors and replay.

## A. Durable topics and queues (FB-63)

A message broker in the style of Google Pub/Sub, SQS plus SNS, or NATS JetStream, stored in the system database.

1. **Topics.** Create, list, delete, with retention (time and size), a message size limit, and an optional schema (JSON Schema) that publish validates against.
2. **Publish.** Single and batch publish with attributes (string map), an ordering key, and a dedup id. A repeated dedup id inside the dedup window is accepted once. Publish inside a database transaction through the FB-40 outbox, so a message is sent only if the transaction commits.
3. **Subscriptions.** Many per topic, each with its own cursor, so every subscription gets every message (fan-out), while consumers inside one subscription share its messages (competing consumers, a work queue). Options: attribute filter, ack deadline (visibility timeout), max delivery attempts, retry backoff, a dead-letter topic, ordering on or off, start position (latest, earliest, a timestamp).
4. **Pull delivery.** REST pull with max messages and long polling; streaming pull over WebSocket or SSE with flow control (max in-flight messages and bytes). Consumers ack, nack (redeliver after backoff), or extend the deadline.
5. **Push delivery.** To an HTTPS webhook (signed, retried with backoff, SSRF-safe through the FB-25 HTTPS client) or to an FB-60 function.
6. **Ordering.** Messages with the same ordering key reach one subscription in publish order, one at a time.
7. **Guarantees.** At-least-once delivery. Redelivery after a missed ack deadline or a crash. A message that exceeds max attempts goes to the dead-letter topic with its delivery history.
8. **Replay.** Seek a subscription to a timestamp or a snapshot, inside retention.
9. **Ops.** Per topic and subscription metrics: backlog, oldest unacked age, publish and ack rates, dead-letter count. Metered through FB-24 (`queue.messages`, `queue.bytes`), authorized through the data core (new queue Actions), REST and OpenAPI as an `ApiExtension` at `/api/v1/queues`.

## B. Long-running tasks (FB-64)

Extends `crates/forgebase-jobs`; does not replace it.

1. **Progress.** A handler or external worker reports percent, stage and message. It is stored on the job and streamed live to clients through an FB-40 realtime channel per job.
2. **Checkpoints.** A handler saves a checkpoint (JSON, size-limited). After a crash, retry or restart, the next attempt starts from the last checkpoint.
3. **Long runs.** Per-job timeouts from seconds to days, automatic lease renewal while the worker is alive, and cooperative cancellation that the handler can observe.
4. **Queues as first-class settings.** Named queues with their own concurrency limit, rate limit (per second or minute), pause and resume, and priority.
5. **Workflows.** Chains, fan-out and fan-in (a job waits for its parents), child jobs, and results passed from parent to child. A failed step fails or compensates its workflow as configured.
6. **Results.** A job result is stored (small results inline, large ones in FB-30 storage) and can be fetched later.
7. **Visibility.** Job and workflow history, timing per attempt, and REST and OpenAPI for all of the above.

## C. Studio, SDK and CLI (FB-65)

1. Studio: a Queues view (topics, subscriptions, backlog, dead letters, message browser, replay) and a Pub/Sub inspector; a task view with live progress, checkpoints, workflow graph, cancel and retry.
2. `sdk/js`: `queues.publish`, `queues.subscribe` (streaming pull with ack), `jobs.onProgress`, workflow helpers.
3. CLI: `forgebase queues publish|pull|tail` and `forgebase jobs watch <id>`.

## Acceptance

- FB-63: fan-out to two subscriptions, competing consumers share a subscription without duplicates under normal operation, redelivery after a missed deadline, dead-letter after max attempts, ordering per key, dedup window, publish-in-transaction rollback sends nothing, replay by timestamp, push retry, restart survival.
- FB-64: progress streams to a subscriber, resume from checkpoint after a killed worker, a day-long timeout does not expire while heartbeats continue, queue concurrency and rate limit hold under load, fan-in waits for all parents, cancellation reaches a running handler.
- FB-65: build, lint and tests for every new screen; SDK and CLI tests.
