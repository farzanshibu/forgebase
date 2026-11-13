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
