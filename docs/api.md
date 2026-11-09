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
| `GET /metrics` | Prometheus text format. | Stable |
| `GET /api/openapi.json` | OpenAPI 3 document. | Stable |
| `/api/system/*` | Instance information and management. Changes are additive. | No version prefix |
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
