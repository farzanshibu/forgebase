# Changelog

All notable changes to the ForgeBase Swift SDK. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[Semantic Versioning](https://semver.org/) and track the server's `/api/v1` compatibility rules (additive changes only within a major version).

## [0.1.0] - 2026-11-14

First release. Package `ForgeBase` (Swift 5.9+; iOS 15, macOS 12, tvOS 15, watchOS 8, visionOS 1), no third-party dependencies.

### Added

- **Core**: `ForgeBaseClient` / `ForgeBaseConfiguration`; `URLSession` transport with retries (429/503, idempotent 502/504 and network
  failures), `Retry-After`, exponential backoff with full jitter; the server's error format mapped to a typed `ForgeBaseError` (`kind`, `status`,
  `code`, `requestId`, `details`, per-field validation errors); read-your-writes commit tokens and `.strong` consistency; `JSON` value type.
- **Auth**: password sign-up / sign-in, MFA step, magic link, OTP, password reset and email verification, refresh-token rotation with a single
  shared refresh and one retry after `401`, sign-out, personal access tokens, sessions, `stateChanges()`; pluggable `TokenStore`
  (`InMemoryTokenStore`, `KeychainTokenStore`); OAuth with an `ASWebAuthenticationSession` helper (`signInWithBrowser`) and a pluggable
  `BrowserAuthenticator`.
- **Records**: list / get / create / update / replace / delete / count, filters with typed parameters, sorting, expand, fields, search, offset and
  cursor pagination (`pages`, `iterate`, `all`), optimistic versions (`If-Match`), export / import, collection definitions.
- **Storage**: buckets, objects (put from `Data` or a file, download to memory or a file with ranges), copy / move / versions / restore, signed
  URLs and a signed-URL `URLRequest` for background `URLSession`s, multipart and resumable uploads with progress and resume.
- **Realtime**: `URLSessionWebSocketTask` client with subscriptions to collections, records and channels, presence, publish, heartbeat, automatic
  reconnect with cursor resume, token rotation, connection events; pluggable `WebSocketFactory`.
- **Services**: functions invoke, key-value (strings, hashes, lists, sets, sorted sets, streams, commands, pub/sub), queues (publish, pull, ack,
  streaming pull with automatic ack), jobs (enqueue, inspect, wait, live updates), entitlements (checks, quotas, reservations, leases), billing
  (customer, portal, subscriptions, invoices, StoreKit / Play / Creem verification), credits reads, vector search, graph reads, SQL, webhook
  verification.
- 140+ offline tests; an example command-line target.

### Not included

Server and operator administration APIs (identity, organizations, access policies, billing catalog and settings, entitlement and credit
administration, function deployment, vector and graph writes, system), SSE realtime fallback, passkeys. See the README.
