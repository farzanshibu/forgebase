# Changelog

All notable changes to the Kotlin SDK. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the SDK
follows [semantic versioning](https://semver.org/). Within `/api/v1` the server only adds fields, so the SDK ignores unknown ones.

## [0.1.0] - unreleased

First release.

### Added

- Core: `ForgeBase` client and options (API key, access token, token store, headers, consistency, your own `OkHttpClient`); a
  suspend transport on OkHttp with retries and jittered backoff (`Retry-After`, 429/503, idempotent 502/504 and network
  failures), typed exceptions for the server error envelope, commit tokens (`X-ForgeBase-Read-After`) and strong reads.
- Auth: sign-up, password sign-in, MFA step, magic link, one-time codes, password reset, email verification, sessions, personal
  access tokens; single-flight refresh-token rotation with a retry after `401`; pluggable `TokenStore` (in-memory default);
  `StateFlow` / `SharedFlow` of the session and auth events; social sign-in (`oauth.start` / `complete` / `completeFromUrl`,
  link, unlink) for Custom Tabs; `Pkce` helper for apps using ForgeBase as the identity provider.
- Records: list with filters, sort, expand, fields, cursor and page paging as `Flow`s, typed queries with `{:name}` params,
  CRUD, optimistic concurrency with `If-Match`, count, export and import; collection definitions.
- Storage: buckets, put / download (range, to file) / copy / move / versions / restore, signed download and upload URLs,
  multipart upload with progress and abort on failure, resumable uploads with `resume`, SHA-256 checksums.
- Realtime: OkHttp WebSocket with `Flow` subscriptions to collections, records and channels, presence, publish, reconnect with
  resume from the last cursor, heartbeat, token refresh over the socket, connection state and events.
- Functions invoke, KV (strings, hashes, lists, sets, sorted sets, streams, generic commands, pub/sub over SSE), queues (publish,
  pull, ack, nack, deadlines, streaming pull with auto-ack), jobs (enqueue, list, cancel, retry, results, live progress, await,
  workflows as JSON), entitlements and quotas (checks, consume, reservations, leases), billing customer, portal, subscriptions,
  invoices and store purchase verification, credits reads, vector search and hybrid search, graph traversal and openCypher, SQL reads.
- Webhook signature verification (`Webhooks.verify` / `constructEvent` / `sign`): constant time, secret rotation, tolerance, same
  vectors as the JavaScript SDK.
- Documentation: README with Android guidance (`EncryptedSharedPreferences` adapter, Custom Tabs flow), section in `docs/sdk.md`,
  CI job.

### Not included

Administration APIs (see the README), cookie sessions, the realtime SSE fallback and OpenAPI code generation.
