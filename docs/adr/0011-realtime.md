# ADR-0011: Realtime engine

- Status: accepted
- Date: 2026-11-09
- Work package: FB-40 (MDF-28)
- Scope: transactional outbox, dispatcher and event bus, WebSocket and SSE
  endpoints, subscription and per-event authorisation, cursors and replay,
  channels and presence, heartbeats, limits, backpressure, cross-instance
  fan-out, message protocol.
- Builds on: ADR-0001 (one writer, error envelope), ADR-0003 §7
  (`AccessContext`, `Authorizer`, `Decision::Filter` compiled into SQL),
  ADR-0004 (credentials, sessions, revocation, scopes).

## Context

SPEC §9 asks for database, collection, record and document subscriptions,
channels, presence, reconnection with replay, subscription authorisation,
limits, heartbeats and backpressure, with a transactional outbox and
cursor-based reconnection. FB-21 (policy engine) runs in parallel: realtime
must sit behind the existing `Authorizer` so the policy engine plugs in
without changes here.

## 1. Crate and wiring

New crate `forgebase-realtime` (depends on data, db, auth, core; axum with
`ws`). It never imports `sqlx`: it talks to the database through the data
service's `Backend`.

```text
forgebase-server ──► forgebase-api ──► forgebase-realtime ──► forgebase-auth ──► forgebase-data ──► …
```

- `forgebase-server` starts `RealtimeService::start(auth.data(), Some(auth), config, fanout)`
  after the auth service (same single `DataService`), passes it to
  `AppState::with_realtime`, and calls `RealtimeService::shutdown()` before
  the HTTP graceful shutdown (upgraded sockets and SSE bodies are not
  in-flight requests and would otherwise hold it open).
- `forgebase-api::router` mounts `forgebase_realtime::router` with
  `nest_service("/api/v1/realtime", …)` inside the auth layer (like the graph
  router) and merges its OpenAPI fragment. Without a realtime service the
  prefix answers `503 realtime_unavailable`.
- Data-core hook (the only change outside the crate, listed in §9).

## 2. Transactional outbox

- Table `_forgebase_realtime_outbox` (migration `0011_realtime.sql`):
  `seq INTEGER PRIMARY KEY AUTOINCREMENT, op, collection_id, collection,
  record_id, tenant, actor, snapshot, created_at`.
- `DataService::set_change_hook(Arc<dyn ChangeHook>)` (once per process).
  Every record created, updated or deleted through the data service calls
  `ChangeHook::statements(&RecordChange)` **inside the write transaction**
  and executes the returned statements there: the outbox insert commits or
  rolls back with the change (tested: a batch whose second record is invalid
  leaves no outbox rows). After a commit with changes,
  `ChangeHook::committed()` wakes the dispatcher.
- Covered writes: `create_record(s)`, `update_record`, `delete_record`,
  cascade deletes of multi-relations, and the update of records whose
  multi-relation array lost the deleted id.
- `snapshot` is the whole row (`{"c": [columns], "v": [values]}`, blobs as
  `{"$b": base64}`) after the change, or before it for deletes. Password
  columns are never written to the outbox.
- **Cursor = `seq`.** `AUTOINCREMENT` never reuses a value (even after
  pruning); with SQLite's single writer, seq order is commit order and a
  reader never sees seq *n+1* committed before *n*, so "all events after
  cursor N" has no gaps. (A PostgreSQL backend needs a commit-ordered
  sequence, e.g. an outbox written under an advisory lock or a
  `pg_current_xact_id` watermark; noted for that work package.)
- **Retention**: a pruner deletes events older than `retention` (24 h) or
  beyond the newest `max_retained_events` (1 000 000), never the newest one.
  A cursor older than the retained window gets `reset` (`cursor_expired`), not
  a silent gap; a cursor ahead of the head gets `reset` (`cursor_invalid`).

## 3. Dispatcher and bus

One dispatcher task tails the outbox by `seq` (page 500), woken by
`committed()` and polling every `poll_interval` (1 s) as a fallback (writes
from another process). Each committed event is published once as
`Arc<Event>` on a bounded `tokio::sync::broadcast` bus (`bus_capacity` 4096).
On start the dispatcher begins at the current head: history is served by
replay, not re-dispatched.

A connection that falls behind the bus (`Lagged`) does not lose events: it
catches up from the outbox (§4) and continues. The bus never blocks the
dispatcher.

## 4. Subscriptions, replay, delivery

Subscription targets: a collection (optionally with a filter-language
`filter` and `params`), one record (`collection` + `record`), or a custom
`channel`.

- **Subscribe-time authorisation**: collection → `Authorizer` with
  `Action::List`, record → `Action::View` (through
  `DataService::subscription_target`, which also compiles the filter with the
  caller's options: hidden fields only for privileged callers). Channels →
  `Authorizer::authorize_channel(ctx, Action::ChannelSubscribe, name)`.
  Scoped API keys / PATs also need `collections:read:<name>` (and
  `realtime:write` to publish or join presence). Non-privileged callers get
  the same `forbidden` for a missing collection as for a forbidden one
  (no existence probing).
- **Per-event authorisation (no leaks)**: before *every* record event is
  sent, `DataService::view_snapshot(ctx, action, collection_id, snapshot,
  filter)` asks the authorizer again with the subscriber's **current**
  context. `Allow` → deliver; `Deny` → withhold; `Filter(predicate)` → the
  predicate *and* the subscription filter are evaluated by SQLite against the
  event's own snapshot as a one-row derived table (`SELECT 1 FROM (SELECT
  CAST(?1 AS TEXT) AS "id", …) AS t WHERE <predicate> AND <filter>`), with the
  same compiler as list queries and the table's column affinities. The
  delivered `data` is built by the same `row_to_record` as REST reads (no
  passwords; hidden fields only for privileged callers). This is the single
  place FB-21's field-level rules plug into for realtime. Rule changes,
  ownership changes and tenant moves therefore take effect on the next event,
  including replayed ones (tested mid-stream). As defence in depth,
  non-privileged subscribers never receive events of another tenant.
  Results are cached per event and connection for subscriptions with the
  same action and filter.
- **Replay without gaps or duplicates**: each connection subscribes to the bus
  *before* it reads the outbox. A subscription with `cursor` replays outbox
  events `cursor < seq ≤ head` (one merged pass in cursor order for several
  subscriptions), then live events; every subscription keeps `last_seq` and
  skips any event with `seq ≤ last_seq`, so events read from both the outbox
  and the bus are delivered once. A subscription without a cursor starts at
  the head. Replay is limited to `max_replay` (10 000) outbox events; further
  behind → `reset` (`too_far_behind`). Tested with writes continuing during
  reconnect: the union of both connections equals all changes, in commit
  order, each exactly once.
- Deletes carry the last visible state of the record (checked against the
  subscriber's current rule like any other event). An update that moves a
  record out of a subscriber's view is withheld (the subscriber is not told
  that it changed).

## 5. Message protocol

WebSocket `GET /api/v1/realtime/ws`: one JSON object per text frame. Unknown
fields are ignored; every client message may carry `ref`, echoed in its reply.

Client → server:

| `type` | Fields | Reply |
|---|---|---|
| `subscribe` | `id` (1–64 `[A-Za-z0-9_.:-]`), one of `collection` (+ `record` or `filter`/`params`) or `channel`; optional `cursor` | replayed events, then `subscribed` `{id, cursor, replayed?, reset?}`; or `error` |
| `unsubscribe` | `id` | `unsubscribed` |
| `publish` | `channel`, `data` | `ack` (rate limited: `publish_per_second`) |
| `presence.join` | `channel` (must be subscribed), `meta` (≤ 1 KiB JSON) | `presence.state` `{channel, members}` |
| `presence.leave` | `channel` | `presence.left` |
| `ping` | — | `pong` |
| `auth` | `token` (refreshed access token, same principal) | `authenticated` |

Server → client:

```json
{"type":"welcome","connection_id":"…","actor":"user:users/u1","cursor":120,"heartbeat_ms":25000,"limits":{…}}
{"type":"record.created","id":"<subscription>","cursor":121,"collection":"products","record_id":"prod_123","tenant_id":null,"timestamp":"2026-11-09T10:00:00.000Z","data":{…}}
{"type":"record.updated", …} {"type":"record.deleted", …}
{"type":"message","id":"<subscription>","channel":"room:1","from":"user:users/u1","data":…,"timestamp":"…"}
{"type":"presence.join"|"presence.leave","id":"<subscription>","channel":"…","member":{"connection_id","actor","meta","joined_at","instance"}}
{"type":"reset","id":"<subscription>","reason":"cursor_expired"|"cursor_invalid"|"too_far_behind","cursor":<head>}
{"type":"error","ref":…,"id":"<subscription>?","code":"forbidden","message":"…"}
```

Record events follow the SPEC §9 shape (`type`, `collection`, `record_id`,
`tenant_id`, `timestamp`, `data`) plus `id` (subscription) and `cursor`.
Clients store the highest `cursor` per subscription and resubscribe with it.

**SSE** `GET /api/v1/realtime/sse?subscriptions=<JSON array of subscribe
objects>[&cursor=N]`: the same server messages, `event:` = `type`, `data:` =
JSON, `id:` = cursor on record events, so `EventSource` reconnects with
`Last-Event-ID` and replays (the header wins over `cursor`). Every
subscription is authorised before the stream starts (denial = HTTP 401/403).
Heartbeats are `: ping` comments. A server-side close ends the stream with
`event: close` `{"code", "reason"}`. SSE is receive-only: publish with
`POST /api/v1/realtime/channels/{channel}/messages`; presence is listed by
`GET /api/v1/realtime/channels/{channel}/presence`. `GET
/api/v1/realtime/stats` (administrators) returns the counters. All REST and
SSE routes are in the OpenAPI document; the WebSocket route is documented as
an upgrade (`101`).

**Authentication**: the auth layer's credential (bearer token, API key, PAT
or session cookie) like every `/api/v1` route; browsers that cannot set
headers on `WebSocket`/`EventSource` pass `access_token` in the query string
(request spans log the path only, ADR-0001 §9). Cookie-authenticated
connections must send a same-origin `Origin` (cross-site WebSocket hijacking).

**Close codes**: `1000` normal, `1001` server shutting down, `1003` binary
frame, `1009` message above `max_message_bytes`, `4001` credential revoked or
expired, `4002` heartbeat timeout, `4008` slow consumer, `4400` repeated
invalid messages. The server keeps the socket open up to 5 s after sending a
close frame so the client can read the code (closing handshake).

## 6. Connection lifecycle and limits

| Setting | Default | Effect |
|---|---|---|
| `max_connections` | 10 000 | per instance → `429 too_many_connections` at upgrade |
| `max_connections_per_principal` | 32 | per actor (anonymous: per client IP) → `429` |
| `max_subscriptions_per_connection` | 100 | → `error too_many_subscriptions` |
| `max_message_bytes` | 64 KiB | WebSocket frame/message limit (→ `1009`), publish body |
| `outbound_queue` | 1024 messages | bounded per-connection queue |
| `send_timeout` | 10 s | a full queue (or a socket write) blocked longer → close `4008` |
| `heartbeat_interval` | 25 s | WebSocket ping frames, SSE comments |
| `idle_timeout` | 70 s | no frame or pong from the client → `4002` |
| `revalidate_interval` | 10 s | credential re-check → `4001` when revoked/expired |
| `publish_per_second` | 50 | token bucket per connection → `error rate_limited` |
| `retention` / `max_retained_events` | 24 h / 1 000 000 | outbox pruning |

- **Backpressure**: every connection has one bounded queue drained by a writer
  task. Producing into a full queue waits at most `send_timeout`; past it, or
  when one socket write blocks that long, the connection is closed with
  `4008 slow_consumer` (counted once). Memory per connection is therefore
  bounded by `outbound_queue` messages; other connections are unaffected
  (tested: a reading client receives every event while a non-reading one is
  dropped).
- **Revocation**: each connection keeps its credential and re-verifies it with
  the auth service every `revalidate_interval` (access token signature,
  expiry and session row; cookie session; API key/PAT). Logout, session
  revocation, password reset, key revocation or account deletion close the
  socket with `4001` within that interval (tested with a real logout, for
  WebSocket and SSE). Access tokens expire after 15 minutes; long-lived
  clients send `{"type":"auth","token":…}` with a refreshed token.
- **Shutdown** closes every connection with `1001`.

## 7. Channels and presence

Channels are ephemeral pub/sub: delivered at most once to the connections
subscribed at that moment, never stored or replayed, re-authorised per
message (`ChannelSubscribe`). Presence (`presence.join` / `leave`, removed on
disconnect) lists `connection_id`, `actor`, `meta`, `joined_at` and
`instance`; joining needs `ChannelPublish`. Channel names: 1–128
`[A-Za-z0-9_.:-]`. The default authorizer allows channels to administrators
only (fail closed); FB-21 overrides `authorize_channel` with rules per channel
name.

## 8. Cross-instance fan-out

Record events need none: every instance tails the shared database's outbox.
Channel messages and presence are in memory, so instances exchange them
through `trait FanOut { publish(payload), subscribe() -> Receiver }`
(optional; `None` for one instance). `RedisFanOut` speaks Redis/Valkey RESP2
`PUBLISH`/`SUBSCRIBE` over plain TCP with optional `AUTH`, reconnecting with
backoff (no client library, no Redis in tests: it is unit-tested for URL
parsing; FB-32's RESP listener can serve as an integration target later).
Remote presence entries are removed by the remote instance's `leave`; members
of an instance that crashes stay listed until restart (known limitation).

## 9. Data-core changes (forgebase-data, kept small)

- `changes.rs` (new): `ChangeOp`, `RowSnapshot`, `RecordChange`,
  `trait ChangeHook`, `DataService::{set_change_hook, subscription_target,
  view_snapshot}`.
- `lib.rs`: `change_hook: OnceLock<…>` in the service; `authorize_channel`.
- `records.rs`: calls the hook in `load_after_write` (create/update), in
  `delete_in_tx` (delete, multi-relation array updates) and
  `changes_committed()` after commit — 5 call sites.
- `access.rs`: `Action::ChannelSubscribe`, `Action::ChannelPublish` appended;
  `Authorizer::authorize_channel` with a default method (no change needed in
  existing authorizers).

## 10. Metrics

`forgebase_realtime_connections` (gauge),
`forgebase_realtime_events_dispatched_total`,
`forgebase_realtime_events_delivered_total`,
`forgebase_realtime_slow_consumers_total`, `forgebase_realtime_revoked_total`;
`GET /api/v1/realtime/stats` adds subscriptions, withheld events, rejected
connections and the head cursor.

## Consequences and limitations

- Writes that bypass the data service produce no events: raw SQL editor
  writes, rows changed by engine foreign-key cascades (single relations
  `ON DELETE CASCADE/SET NULL`), and auth-internal tables. Schema changes
  produce no events; events of a dropped collection are withheld.
- Per-event checks with a `Filter` decision cost one small read per event and
  distinct (action, filter) per connection; `Allow`/`Deny` cost none.
  Measured in FB-81.
- Realtime limits are not yet in `forgebase.toml` (defaults from
  `RealtimeConfig::default()`); a `[realtime]` section needs a
  `forgebase-core` config change (follow-up).
- One dispatcher per process; a PostgreSQL backend needs a commit-ordered
  cursor (§2).
