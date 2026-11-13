# ForgeBase HTTP API

This page sets the conventions every ForgeBase endpoint follows. It does not
list every endpoint. The live OpenAPI document is the reference for paths,
request bodies and response schemas:

```text
GET http://127.0.0.1:8090/api/openapi.json
```

Decisions behind these conventions: [ADR-0001 §6–7](adr/0001-architecture.md)
(paths, versioning, error model), [ADR-0004](adr/0004-auth.md) (credentials),
[ADR-0008 §10](adr/0008-policy.md) (tenancy headers).

## 1. Base paths

| Path | Purpose | Stability |
|---|---|---|
| `GET /health` | Liveness: the process answers HTTP. | Stable |
| `GET /health/ready` | Readiness: database and storage answer. `200` when ready. | Stable |
| `GET /metrics` | Prometheus text format. Access follows `[server] ops_access` ([§11.3](#113-operational-endpoints-ops_access)). | Stable |
| `GET /api/openapi.json` | OpenAPI 3 document. | Stable |
| `/api/system/*` | Instance information and management. Changes are additive. Access follows `[server] ops_access`. | No version prefix |
| `/api/auth/*` | Sign-in, sessions, tokens, MFA, OAuth, OIDC, passkeys, API keys, service accounts. | Unversioned; see ADR-0004 §12 |
| `/api/v1/*` | Data and product APIs: collections, KV, storage, jobs, billing, and so on. | Versioned |
| `/api/v1/realtime/*` | WebSocket (`/ws`), Server-Sent Events (`/sse`), channels and presence. | Versioned |
| `/admin/` | Embedded admin dashboard (single-page app). | n/a |

Example: `GET /api/system/info` returns the instance information; `GET /health`
is the probe a load balancer should use.

## 2. Versioning

Inside `/api/v1`, changes are additive only: new fields, new endpoints, new
optional parameters. Clients must ignore unknown fields. A breaking change gets
`/api/v2/*`. The old version stays served for at least one minor release and
returns a `Deprecation` header (ADR-0001 §6).

## 3. Authentication

Send one of these with each request. Without any, the request is anonymous and
the authorizer decides what an anonymous caller may do.

| Credential | How to send | Lifetime |
|---|---|---|
| Access token (JWT) | `Authorization: Bearer <access token>` | 15 minutes; checked against its session on every use, so revocation is immediate |
| API key | `X-API-Key: <key>`, or `Authorization: Bearer <key>` | Optional expiry, 1–3650 days |
| Personal access token (PAT) | `Authorization: Bearer <pat>` | Optional expiry |
| Session cookie | Cookie `fb_session`. On writes, also `X-CSRF-Token: <token>` | Session lifetime, with idle timeout |

Obtain tokens from `POST /api/auth/login`. Refresh tokens rotate on every use.
Reusing a rotated refresh token revokes the whole session (ADR-0004 §3).

API keys and PATs carry **scopes**: `*`, `<area>:*`, `<area>:read`,
`<area>:write`, or with a name after the area. `write` does not imply `read`.
A request outside its scope returns `403` with code `insufficient_scope`
(ADR-0004 §10).

## 4. Tenancy headers

Operations that act inside an organization or environment take these headers.
The server checks membership and fills in the caller's realm, organization,
tenant and roles before the handler runs.

| Header | Meaning |
|---|---|
| `X-Forgebase-Realm` | Realm (identity and policy boundary). |
| `X-Forgebase-Org` | Organization the request acts in. |
| `X-Forgebase-Tenant` | Tenant (environment) the request acts in. |

The authorizer checks each request against the pipeline in
[architecture.md §4](architecture.md#4-how-a-request-flows). A denial returns
`403` with `details.step` naming the step that refused.

## 5. Request ids

Every response carries `x-request-id`. A client may send its own. It is kept
only if it is 1–128 characters of `A-Z a-z 0-9 . _ : -`; otherwise the server
generates a UUID v4 (ADR-0001 §7). Quote this id when you report a problem:
it appears in the server logs for that request.

## 6. Errors

Every error, including routing `404`/`405`, timeouts (`408`) and caught panics
(`500`), has this shape:

```json
{
  "error": {
    "code": "invalid_body",
    "message": "human-readable text",
    "request_id": "4f3c…",
    "details": { "fields": { "title": { "code": "required", "message": "…" } } }
  }
}
```

- `code` is a stable snake_case value. Codes are part of the API and are not
  renamed once released. `details` is present only when there is structured
  context, such as per-field validation errors.
- The status comes from the error's kind: `400` invalid input, `401`
  unauthenticated, `403` forbidden, `404` not found, `409` conflict, `503`
  unavailable, `500` internal. `internal` errors reach the client only as
  `internal server error`.
- Common body errors: `400 invalid_json`, `400 invalid_body` (wrong shape),
  `413 payload_too_large`, `415 unsupported_media_type`.
- Concurrency: `409` when a version check fails. Jobs return `409 lease_lost`
  when a worker presents an expired lease.
- Rate limits return `429` with a `Retry-After` header.

## 7. Lists and pagination

List endpoints share these query parameters:

| Parameter | Meaning |
|---|---|
| `filter` | Filter expression (the query language, see [database.md](database.md#6-query-language)). |
| `p.<name>` | Values for `{:name}` placeholders in `filter`, so values never go into the expression text. |
| `sort` | Comma-separated keys; prefix `-` for descending. |
| `fields` | Comma-separated fields to return. |
| `expand` | Relations to include. |
| `search` | Free-text search where the collection supports it. |
| `page`, `per_page` | Page-number pagination. |
| `cursor` | Cursor pagination. Use the `next_cursor` from the previous response. |
| `skip_total` | `true` skips the total count, which is costly on large tables. |

A list response has this shape:

```json
{
  "items": [ { "...": "..." } ],
  "page": 1,
  "per_page": 50,
  "total_items": 1234,
  "total_pages": 25,
  "next_cursor": "opaque-string"
}
```

`next_cursor` is absent on the last page. Prefer cursors for large or
changing data; page numbers can skip or repeat items when rows change between
requests. `total_items` and `total_pages` may be absent when `skip_total` is set.

## 8. Concurrency

Updates to records use optimistic concurrency. Read the record's version
(`_version`) and send it with the write; a mismatch returns `409`. The JS SDK
sends this as `If-Match` (see [sdk.md](sdk.md#3-collections-and-records)).

## 9. Bodies and limits

- JSON bodies must be `application/json` or a `+json` type such as
  `application/merge-patch+json`.
- Handlers enforce size limits. The import route is streamed and accepts far
  larger bodies than the JSON routes.
- Each request has a timeout (`request_timeout_secs`, default 30). A request
  past the timeout returns `408`.
- CORS: allowed request headers are `Authorization`, `Content-Type`, `If-Match`,
  `x-request-id`, `X-API-Key` and `X-CSRF-Token`. Exposed response headers are
  `ETag`, `Location` and `x-request-id`.

## 10. OpenAPI

`GET /api/openapi.json` is the authoritative description of every path,
parameter and schema on the running server. Generate clients from it. The SDK's
types are generated from it (see [sdk.md](sdk.md#4-types)). This page does not copy
it, so it cannot drift from the server.

Security schemes in the document: `bearer`, `api_key` (`X-API-Key`) and
`session_cookie`. Each `/api/v1` operation accepts any one of them, or none.

## 11. Queues, backups and operational access

These groups are in the OpenAPI document like every other route. This section
gives the shape of each group and the rules that the schema alone does not
show.

### 11.1 Queues (`/api/v1/queues`)

Durable topics with independent subscriptions (FB-63, FB-65,
[ADR-0022](adr/0022-queues-pubsub.md)). The routes exist when `[queue] enabled = true`
(the default). Messages are kept per topic for its retention; each subscription
has its own cursor, so a slow consumer does not hold back others.

| Route | Purpose |
|---|---|
| `GET`/`POST /queues/topics` | List or create topics (retention, message size limit, dedup window). |
| `GET`/`PATCH`/`DELETE /queues/topics/{topic}` | Read, change or delete a topic. A topic with subscriptions answers `409 topic_has_subscriptions` unless `?force=true`, which deletes them too. |
| `POST /queues/topics/{topic}/publish` | `{"messages": [{"data": <json>, "attributes": {...}, "ordering_key": "...", "dedup_id": "..."}]}`. Each result has `message_id`, `seq` and `duplicate`. A repeated `dedup_id` inside the dedup window is accepted once. |
| `GET /queues/topics/{topic}/messages` | Messages in order, from `after` (a `seq`). |
| `GET`/`POST /queues/subscriptions` | List (optionally `?topic=`) or create subscriptions: pull or push, ack deadline, max delivery attempts, dead-letter topic, filter. |
| `GET`/`PATCH`/`DELETE /queues/subscriptions/{sub}` | Read, change or delete a subscription. |
| `POST /queues/subscriptions/{sub}/pull` | `{"max_messages": n, "wait_ms": ms}` long-polls. Each message has an `ack_id`, `delivery_attempt` and `ack_deadline`. |
| `POST .../ack`, `.../nack`, `.../modify-deadline` | `{"ack_ids": [...]}` (plus the new deadline for modify). An unacknowledged message is redelivered after its deadline; after the last attempt it goes to the dead-letter list. |
| `POST /queues/subscriptions/{sub}/seek` | Move the cursor to a time or to a snapshot. |
| `GET .../stream`, `.../events` | Streaming pull with flow control (`max_messages`, `max_bytes` unacknowledged): `stream` is a WebSocket, `events` is Server-Sent Events. Pull subscriptions only (`409 push_subscription`). |
| `GET .../dead-letters`, `GET .../metrics`, `GET /queues/topics/{topic}/metrics` | Dead letters and counters (backlog, oldest unacked age, deliveries). |
| `GET`/`POST /queues/snapshots`, `DELETE /queues/snapshots/{snapshot}` | Named copies of a subscription's acknowledgement state, for `seek`. |

Permissions are the policy keys `engine:queue_read`, `engine:queue_publish`,
`engine:queue_consume` and `engine:queue_admin`. Push subscriptions call their
endpoint through the same outbound address guard as functions `fetch`
([security.md](security.md)).

#### Webhook delivery tooling

Debugging and operating push subscriptions ([ADR-0031](adr/0031-webhook-tooling.md)).
Reads need `engine:queue_read`, everything else `engine:queue_admin`, on the
subscription (its creator holds both). Bodies marked optional may be empty.

| Route | Purpose |
|---|---|
| `GET .../attempts` | Push attempts, newest first: deliveries and test sends with `status`, `latency_ms`, `error_class` (`timeout`, `dns`, `connect`, `tls`, `ssrf_refused`, `http_status`, `redirect`, `protocol`, `invalid_request`, `config`, `function`), the request headers sent (signature masked), allowlisted response headers and the response body cut to `[queue] push_response_body_bytes` (`response_truncated`, `response_bytes`). Filters `outcome`, `message_id`, `since`, `until`, `test`; paging `before` / `next_before`, `limit`. |
| `GET .../attempts/{id}` | One attempt (`404 attempt_not_found`). |
| `POST .../test` | Optional `{"data": <json>}`. Sends a synthetic signed event now (`"type": "forgebase.test"` in the usual `{subscription, delivery_attempt, message}` envelope, header `X-Forgebase-Test: 1`) and returns the attempt. Not a message: cursors, totals, health and dead letters are untouched. `[queue] push_test_sends_per_minute` per subscription (`429 rate_limited`); pull subscriptions answer `409 not_push_subscription`. |
| `POST .../redrive` | `{"ids": [..]}` or `{"all": true, "since": "<RFC 3339>", "filter": {...}, "limit": n}`. Delivers dead letters to the subscription again with a fresh attempt counter. Each record is redriven once (`redriven_at`, `redriven_by` on the dead letter), also under concurrent calls; the result lists `redriven`, `already_redriven`, `not_found` and `remaining`. Audited (`queue.dead_letters.redrive`). |
| `POST .../retry` | Optional `{"message_ids": [..]}`. Deliveries waiting for a retry backoff become ready now. |
| `POST .../secret/rotate` | Optional `{"grace_ms": n}` (default 24 h, at most 7 days). Returns the new `push_secret` once. During the grace window `X-Forgebase-Signature` carries both signatures, new first: `t=<ts>,v1=<new>,v1=<old>`; afterwards only the new one. Receivers must accept the request when **any** `v1` matches. `409 not_webhook_subscription` for functions. |
| `POST .../secret/expire-previous` | Ends the grace window now. |
| `POST .../pause`, `POST .../resume` | Optional `{"reason": ".."}` on pause. A paused push subscription keeps accumulating messages (none is dead-lettered while paused); resume makes every delayed delivery ready and delivers the backlog in order. `pause_after_failures` on a subscription (1-1000, `0` in a change turns it off) pauses it automatically after that many consecutive failed attempts (`paused_reason` starts with `auto:`). |
| `GET /queues/catalog` | Every topic the caller may read with `description`, `data_schema`, `example` and `example_source` (`topic`, `schema`, `none`); record-source topics list their `event_types` (`record.created`, `record.updated`, `record.deleted`) with the subscription `filter` that selects each, the change message schema (the collection's fields, when the caller may read its schema) and an example. Topics take `description` and `example` (checked against the schema) on create and change. |

`GET .../metrics` of a push subscription adds `push`: success rate and p50/p95/p99
latency of the last hour (newest 1000 attempts, test sends excluded),
`consecutive_failures`, `last_success_at`, `last_failure_at`, `last_error` and the
pause state. Prometheus: `forgebase_queue_push_attempts_total{subscription,outcome}`,
`forgebase_queue_push_latency_seconds`, `forgebase_queue_push_consecutive_failures`,
`forgebase_queue_push_paused`, `forgebase_queue_push_auto_paused_total`,
`forgebase_queue_push_last_success_timestamp_seconds` and
`forgebase_queue_redriven_total`. A subscription created or changed with
`ttl_ms` (60 s to 31 days) is deleted when it runs out; `forgebase queues listen
<topic> --forward-to http://localhost:3000/hook` uses one to forward a topic to a
local endpoint, signed with a session secret.

### 11.2 Backups (`/api/v1/system/backups`)

Snapshots of the database and storage over HTTP (FB-83). Administrators only.

| Route | Purpose |
|---|---|
| `GET /system/backups` | Snapshots, newest first: id, label, created time, size, whether derived index files are included. |
| `POST /system/backups` | Take a snapshot now while the server keeps serving. Body (all optional): `{"label": "nightly", "include_indexes": true, "retention": {"keep_last": 7, "max_age_days": 30}}`. Retention runs after the snapshot is written. |
| `GET`/`DELETE /system/backups/{id}` | Read or delete one snapshot. `latest` names the newest (also for `verify`). |
| `POST /system/backups/{id}/verify` | Check the manifest, every file's size and SHA-256, and that no extra files exist. SQLite: open the copied database read-only (integrity, foreign keys, migration state). PostgreSQL: `pg_verifybackup` (bundled) or `pg_restore --list` (external), and the schema version against this build. |
| `POST /system/backups/prune` | Apply a retention policy (`keep_last`, `max_age_days`). |

Restore is not an HTTP operation, because it replaces files the running server
has open. Stop the server and run `forgebase restore <id|latest> --force`
([deployment.md](deployment.md)). `forgebase backup`, `backup list`,
`backup verify` and `doctor` are the CLI forms of the routes above.

### 11.3 Operational endpoints (`ops_access`)

`/metrics` and `/api/system/*` show the data directory, the database path and
load figures, so FB-80 gates them with `[server] ops_access`
(`FORGEBASE_OPS_ACCESS`, [security.md §5](security.md)):

| Value | Who may read them |
|---|---|
| `loopback` (default) | Administrators and admin service accounts from anywhere, plus unauthenticated requests whose socket peer is a loopback address and that carry no `Forwarded`, `X-Forwarded-For` or `X-Real-IP` header (a Prometheus on the same host). |
| `admin` | Administrators only. |
| `public` | Anyone. Use only behind a proxy that restricts these paths itself. |

A refused anonymous request gets `401`, a signed-in non-administrator `403`.
For `/metrics`, a scoped API key needs the scope `system:read`.
`/health` and `/health/ready` are never gated.

### 11.4 SQL editor (`/api/v1/sql`)

`POST /sql` runs one statement, `POST /sql/batch` 1 to 1000 in one
all-or-nothing transaction, `POST /sql/explain` returns a plan. Values go in
`params`, never in the text.

- **Placeholders.** SQLite takes `?`, `?N` and `$N`. On PostgreSQL a statement
  with values and no `$N` may use `?`/`?N` (translated); a statement that uses
  `$N`, or has no values, is sent as written, so `?`, `?|` and `?&` are the
  `jsonb` operators: `SELECT data ? 'k' FROM t WHERE id = $1`.
- **Secret columns.** Password fields, `password` and `token_key`, and the
  secret columns of ForgeBase's system tables (`*_hash`, `*_enc`, `private_*`,
  and names with a `password`, `secret`, `token` or `sealed` word) are masked
  in results (`"secrets_masked": true`), and a statement that names one is
  refused with `403`. `"show_secrets": true` shows them; each use is audited
  (`sql.show_secrets`). On PostgreSQL the editor's role also has no `SELECT` on
  them, so `SELECT *` of such a table is refused there unless `show_secrets` is
  set. On SQLite this is a guard in front of an administrator's tool, not a
  boundary: a column name built at run time is not seen.

### 11.5 Billing webhook inspector (`/api/v1/billing/webhook-deliveries`)

Every request to `POST /api/v1/billing/webhooks/{provider}` is recorded, verified or not
([ADR-0032](adr/0032-billing-webhook-inspector.md)): provider and account, received time, client address,
allowlisted headers (never `authorization`, cookies or API keys), the raw body (up to 256 KiB), the
verification outcome with a safe reason, the provider event id and type, the processing outcome, the billing
objects it touched and the latency. Unverified deliveries are stored only within a per-provider budget, and the
answer to the provider never changes. Rows are deleted after `[billing] webhook_log_retention_days` (default 30).
Platform administrators only: a signed-in non-administrator gets `403`, an anonymous caller `401`.

| Route | Purpose |
|---|---|
| `GET /billing/webhook-deliveries` | Newest first: `{"items": [...], "total", "limit", "offset"}`. Filters `provider`, `outcome`, `event_type`, `event_id`, `from`, `to` (RFC 3339, `to` exclusive); `limit` 1–500 (default 50), `offset`. No bodies. |
| `GET /billing/webhook-deliveries/{id}` | One delivery with `headers` (object) and `body` (`body_encoding` `utf8`, or `base64` for binary bodies; `null` when the budget dropped it). |
| `POST /billing/webhook-deliveries/{id}/replay` | Processes a verified delivery again through the normal path. The stored body is verified again with the current secrets as of the time it was received; the event dedup decides. |
| `POST /billing/webhook-deliveries/{id}/reverify` | Verifies a refused delivery again with the current secrets (after fixing a webhook secret) and processes it if it now verifies, exactly once. |

`outcome` is the processing outcome of a verified delivery (`applied`, `ignored`, `duplicate`, `failed`) or the
verification outcome of a refused one (`signature_invalid`, `timestamp_out_of_tolerance`, `unknown_provider`,
`malformed`, `rejected`, `error`). `links` holds `webhook_event_id`, `customer_id`, `subscription_id`,
`order_id`, `invoice_id`, `payment_id`, `refund_id` and `store_purchase_id`.

Replay and re-verify answer `{"result", "detail", "delivery"}`. `result` is `applied`, `ignored` or `failed`
(processed now), `already_applied` or `already_ignored` (the event was handled before: nothing changed), or
`unverified` (the stored body does not verify with the current secrets: nothing was processed). Both are written
to the billing audit log. Errors: `409 delivery_not_verified` (replay of a refused delivery: re-verify it),
`409 delivery_already_verified` (re-verify of a verified one: replay it), `409 reverify_in_progress`,
`409 body_not_stored` (dropped by the budget or cut at the size limit) and `409 not_reverifiable` (Google Play:
its push token is not stored; Pub/Sub redelivers unacknowledged pushes).

### 11.6 Alerts and SLOs (`/api/v1/alerts`, `/api/v1/slos`)

The alerting engine and service level objectives of
[ADR-0035](adr/0035-tracing-alerting-slos.md) ([observability.md](observability.md)).
Platform administrators only, checked before a body is read: an anonymous
caller gets `401`, a signed-in non-administrator `403`. `[alerts] enabled = false`
turns both prefixes into `404 feature_disabled`.

| Route | Purpose |
|---|---|
| `GET /alerts` | Open alerts (`pending`, `firing`), most severe first; `state`, `severity` filters. Each lists `silenced_by`. |
| `GET /alerts/history` | Every alert, newest first: `state`, `rule_id`, `from`, `to`, `before` (paging), `limit` (1–500). |
| `GET /alerts/{id}` | One alert with its state changes (`events`). |
| `POST /alerts/query` | Evaluates an expression now: `{"expr": "…"}` → samples. |
| `GET /alerts/rules` · `POST /alerts/rules` | Every rule (built-in, `[[alerts.rules]]`, custom, SLO burn-rate) with `enabled`, `disabled_by_config`, `source`; create a custom rule (`name`, `expr`, `op`, `threshold`, `for`, `severity`, `labels`, `annotations`). |
| `GET`, `PATCH`, `DELETE /alerts/rules/{id}` | One rule; change it (built-in, file and SLO rules: `enabled` only, else `409 rule_read_only`); delete a custom one. |
| `GET`, `POST /alerts/silences` · `GET`, `PUT`, `DELETE /alerts/silences/{id}` | Silences (`?all=true` with expired ones): `matchers` (`label`, `op` `=` `!=` `=~` `!~`, `value`), `starts_at`, `ends_at` or `duration`, `comment`. `DELETE` ends one now. |
| `GET`, `POST /alerts/notifiers` · `GET`, `PUT`, `DELETE /alerts/notifiers/{id}` | Notifiers (`webhook`, `slack`, `email`, `pagerduty`, `queue`; by id or name); secrets are never listed; a webhook's `signing_secret` is in the create answer only. |
| `POST /alerts/notifiers/{id}/test` | Sends a test notification now (10 per notifier and minute, then `429 rate_limited`) and returns its delivery. |
| `GET /alerts/notifications` | Deliveries, newest first: `notifier`, `status` (`pending`, `sent`, `failed`), `limit`. |
| `GET`, `POST /slos` · `GET`, `PUT`, `DELETE /slos/{id}` | SLOs (by id or name): `kind` `availability` or `latency`, `objective`, `window`, `route_prefix` or `route_regex`, `methods`, `latency_threshold_ms`, `alerting`, `labels`. |
| `GET /slos/{id}/status` · `GET /slos/status` | Totals, SLI, `error_budget` (`allowed`, `consumed`, `remaining_ratio`), `burn_rates` per window and the burn-rate tiers. |

Errors: `404 alert_rule_not_found`, `alert_not_found`, `silence_not_found`,
`notifier_not_found`, `slo_not_found`; `409 alert_rule_exists`,
`notifier_exists`, `slo_exists`, `rule_read_only`, `rule_disabled_by_config`;
`400 invalid_alert_rule`, `invalid_name`, `invalid_matcher`, `invalid_silence`,
`invalid_notifier`, `invalid_slo`, `invalid_expression`, `invalid_query`;
`429 rate_limited`.

Trace context: with `[observability.otlp] enabled`, every response carries
`traceparent` (the server span), and a request's `traceparent` continues the
caller's trace under `[observability] trust_incoming_trace`
([observability.md §1.5](observability.md#15-how-context-travels)). Queue
messages published then carry a `traceparent` attribute, and jobs a
`traceparent` field.
