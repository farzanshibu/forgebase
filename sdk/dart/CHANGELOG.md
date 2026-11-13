# Changelog

All notable changes to the Dart / Flutter SDK are listed here. The SDK follows
[semantic versioning](https://semver.org); before 1.0 a minor version may change
the API.

## 0.1.0

First release. Pure Dart, for Flutter (iOS, Android, web, desktop) and servers.

- Transport: JSON requests, retries for 429 / 503 (and idempotent 502 / 504 and
  network errors) with exponential backoff, full jitter and `Retry-After`;
  quota 429s are never retried; typed exceptions from the error envelope;
  request timeout; commit tokens (`X-ForgeBase-Read-After`) and strong
  consistency.
- Auth: sign-up, sign-in, administrator sign-in, MFA, magic link, one-time
  code, password reset, email verification, OAuth start / complete, personal
  access tokens, sessions. Automatic refresh with rotation, one shared refresh
  for concurrent callers, retry once after a 401, `AuthChange` events.
- Pluggable `TokenStorage` (in memory by default) and `CallbackTokenStorage`.
- Records: list with filter, params, sort, expand, fields, search, offset and
  cursor pagination; `pages` / `iterate` / `all`; get, create, update, replace,
  delete with optimistic `If-Match` versions; count, export, import; collection
  definitions; typed rows with `collectionOf`.
- Realtime over WebSocket: subscriptions to collections, records and channels,
  cursor tracking, reconnect with backoff and resume, heartbeats, token
  rotation over the open socket, close code 4001 handling, publish, presence,
  `watch` as a `Stream`.
- Storage: buckets, objects (put, get, range, stream, metadata, list, copy,
  move, versions, restore, delete), signed URLs (sign, download, upload),
  multipart and resumable uploads with progress.
- Key-value: strings with TTL, bytes, counters, hashes, lists, sets, sorted
  sets, streams, `scan` / `keys`, `exec` / `batch`, pub/sub over SSE.
- Functions: invoke (JSON and raw), deploy, list, logs, versions, rollback,
  secrets.
- Queues: topics, subscriptions, publish with dedup ids, pull, ack, nack,
  deadlines, seek, snapshots, dead letters, streaming pull with reconnect.
- Jobs: enqueue, get, list, cancel, retry, progress over realtime, `wait`,
  queues, workflows with a validating builder.
- Entitlements and quotas, and store billing (App Store, Google Play, Creem).
