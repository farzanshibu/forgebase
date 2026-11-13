# ForgeBase SDKs

This page says which SDKs exist, what they cover, and where their full
documentation is. The JavaScript SDK's own README in
[`sdk/js/README.md`](../sdk/js/README.md) is the detailed reference. This page
does not repeat it.

## 1. Status

| SDK | Location | Status |
|---|---|---|
| JavaScript / TypeScript (`@forgebase/sdk`) | [`sdk/js`](../sdk/js) | Version 0.1.0. Covers auth, collections and records, storage, KV, functions, vector, graph, SQL, realtime, queues, jobs, billing, entitlements, credits, webhook verification, and the organization, access, identity, customization and system administration APIs. |
| Rust (`forgebase-sdk`) | [`sdk/rust`](../sdk/rust) | Version 0.1.0 (FB-82), its own Cargo workspace. Covers auth, records, KV, storage, realtime, functions, jobs, queues, billing and entitlements. See [sdk/rust/README.md](../sdk/rust/README.md). |
| Dart / Flutter (`forgebase`) | [`sdk/dart`](../sdk/dart) | Version 0.1.0 (requested after the 2026-11-10 skip). Pure Dart, so it runs in Flutter and on servers. Covers auth, records, realtime, storage, KV, functions, queues, jobs, entitlements and store billing. See [§8](#8-dart--flutter-sdk) and [sdk/dart/README.md](../sdk/dart/README.md). |
| Kotlin (Android and JVM, `dev.forgebase:forgebase`) | [`sdk/kotlin`](../sdk/kotlin) | Version 0.1.0, a pure Kotlin/JVM Gradle library (Android needs no plugin). Covers auth with a pluggable token store and Custom Tabs sign-in, records, storage (multipart and resumable), realtime with resume, functions, KV, queues, jobs, entitlements, billing customer and store verification, vector, graph, SQL reads and webhook verification. Administration APIs are left to the JS and Rust SDKs. See [sdk/kotlin/README.md](../sdk/kotlin/README.md). |
| Swift (iOS, macOS, tvOS, watchOS, visionOS; product `ForgeBase`) | [`sdk/swift`](../sdk/swift) | Version 0.1.0, Swift Package Manager, no third-party dependencies. Covers auth with a Keychain token store and `ASWebAuthenticationSession` sign-in, records, storage (multipart and resumable), realtime with resume, functions, KV, queues, jobs, entitlements, billing, credits, vector, graph, SQL reads and webhook verification. Administration APIs are left to the JS and Rust SDKs. See [§10](#10-swift-sdk) and [sdk/swift/README.md](../sdk/swift/README.md). |

## 2. JavaScript / TypeScript SDK

### 2.1 Install and create a client

```sh
npm install @forgebase/sdk
```

```ts
import { ForgeBase } from "@forgebase/sdk";

const fb = new ForgeBase("http://localhost:8090");
```

The package has no runtime dependencies. It needs `fetch`, which Node 20+,
browsers, Deno and Bun provide. It ships ESM, CJS and type declarations.

### 2.2 What it covers

| Area | Entry point | Details |
|---|---|---|
| Authentication | `fb.auth` | Sign-up and sign-in, MFA, magic links, one-time codes, OAuth and OIDC redirect flows, sessions in memory or a storage adapter, HttpOnly cookie mode, API keys for servers. See [sdk/js/README.md#authentication](../sdk/js/README.md#authentication). |
| Collections and records | `fb.collection<T>(name)` | Create, read, update with optimistic concurrency, replace, delete, list with filter, sort, expand and fields, and iterators over pages. See [§3](#3-collections-and-records). |
| Realtime | `fb.realtime` | One connection for every subscription. WebSocket with SSE fallback. Automatic resume from the last cursor. Channels, presence and publish. See [sdk/js/README.md#realtime](../sdk/js/README.md#realtime). |
| Entitlements and quotas | `fb.entitlements` | See [sdk/js/README.md#entitlements-and-quotas](../sdk/js/README.md#entitlements-and-quotas). |
| Queues and pub/sub | `fb.queues` | Topics, subscriptions, publish (attributes, ordering key, dedup id), pull, ack, nack, deadlines, streaming pull with automatic reconnect, seek and snapshots, dead letters. See [§3.1](#31-queues). |
| Jobs and long tasks | `fb.jobs` | Enqueue, progress, wait, workflows. See [sdk/js/README.md#jobs-and-long-tasks](../sdk/js/README.md#jobs-and-long-tasks). |
| Storage | `fb.storage` | Buckets, objects, multipart and resumable uploads, signed URLs, versions, ACLs. See [sdk/js/README.md#storage](../sdk/js/README.md#storage). |
| Documents | `fb.documents` | Document form of the record operations with merge patch. |
| Key-value | `fb.kv.ns(name)` | Strings, hashes, lists, sets, sorted sets, streams, TTLs, pub/sub, batch and raw commands. See [sdk/js/README.md#key-value-store](../sdk/js/README.md#key-value-store). |
| SQL and indexes | `fb.sql`, `fb.indexes` | Parameterised SQL, batch, explain, tables; collection indexes, recommendations and the slow-query log. |
| Graph | `fb.graph` | Nodes, edges, neighbours, traversal, shortest path, read-only openCypher. |
| Vector search | `fb.vector` | Collections, fields, upsert, similarity and hybrid search, recall, rebuild. |
| Functions | `fb.functions` | Deploy, invoke (JSON or raw `Response`), versions, rollback, logs, secrets, KV rules. |
| Credits | `fb.credits` | Balance, ledger, top-ups, grants, spend, refunds, reversals, rates, usage, stats. |
| Billing | `fb.billing` | Customers, subscriptions, orders, invoices, portal, catalog, providers, settings and store purchases. |
| Organizations and access | `fb.orgs`, `fb.access` | Orgs, projects, teams, members, invitations, roles; permission checks, ACLs, collection and resource rules. |
| Identity and customization | `fb.identity`, `fb.customize` | API keys, tokens, service accounts, OAuth clients, realms, user administration; domains, branding, email, agreements. |
| System | `fb.system` | Features, config, database topology, backups, DB logins, extensions ([§3.2](#32-backups-and-operational-endpoints)). |
| Webhook verification | `verifyWebhookSignature`, `constructWebhookEvent` | Verifies push-subscription webhooks on any Web Crypto runtime ([§3.3](#33-verifying-push-webhooks)). |
| Raw requests | `fb.request(path, options)` | For routes the SDK does not wrap yet. |

### 2.3 Retries and errors

- `429` and `503` are retried for every method. `502`, `504` and network errors
  are retried only for idempotent methods. A `Retry-After` header is honoured.
- Every failure is a `ForgeBaseError` with `status`, `code`, `message`,
  `requestId`, `details` and `retryAfter`. Subclasses include `ValidationError`,
  `AuthenticationError`, `PermissionError`, `NotFoundError`, `ConflictError`,
  `RateLimitError`, `QuotaExceededError`, `ServerError`, `NetworkError`,
  `RealtimeError` and `WebhookVerificationError`.

See [sdk/js/README.md#retries](../sdk/js/README.md#retries) and
[#errors](../sdk/js/README.md#errors). The error envelope itself is in
[api.md §6](api.md#6-errors).

## 3. Collections and records

```ts
const posts = fb.collection<{ id: string; title: string; views: number }>("posts");

const p = await posts.create({ title: "Hello" });
await posts.update(p.id, { views: 1 }, { version: p._version }); // If-Match; ConflictError if stale
await posts.delete(p.id);

const page = await posts.list({
  filter: 'status = "live" && views > {:min}',
  params: { min: 10 },
  sort: ["-created", "title"],
  page: 2,
  perPage: 50,
});
```

The filter language, sort keys and pagination are described in
[database.md §6](database.md#6-query-language) and
[api.md §7](api.md#7-lists-and-pagination). The SDK's `list` options map onto
those query parameters.

### 3.1 Queues

Both SDKs wrap `/api/v1/queues` ([api.md §11.1](api.md#111-queues-apiv1queues)).

```ts
await fb.queues.createTopic("orders");
await fb.queues.createSubscription("billing", "orders", { ackDeadlineMs: 30_000, maxDeliveryAttempts: 5 });
await fb.queues.publish("orders", { id: 1 }, { attributes: { kind: "rush" }, orderingKey: "u1", dedupId: "o-1" });

// Streaming pull: resolve = ack, throw = nack (redelivered after the backoff).
const stream = fb.queues.subscribe<{ id: number }>("billing", async (d) => charge(d.data.id), { maxMessages: 50 });
stream.unsubscribe();

// Manual pull and ack
for (const m of await fb.queues.pull("billing", { maxMessages: 10, waitMs: 2000 })) {
  await fb.queues.ack("billing", [m.ack_id]);
}
await fb.queues.seek("billing", { time: new Date(Date.now() - 3_600_000) }); // replay the last hour
```

```rust
let q = fb.queues();
q.create_topic(&NewTopic::new("orders")).await?;
q.create_subscription(&NewSubscription::new("billing", "orders")).await?;
q.publish("orders", &NewMessage::new(json!({"id": 1})).attr("kind", "rush").dedup_id("o-1")).await?;
for m in q.pull("billing", &PullOptions { max_messages: Some(10), wait_ms: Some(2000), ..Default::default() }).await? {
    q.ack("billing", &[m.ack_id]).await?;
}
```

A repeated `dedupId` inside the topic's dedup window is stored once; the
repeat's result has `duplicate: true`. Unacknowledged messages come back after the
ack deadline; after `maxDeliveryAttempts` they move to the dead letters
(`fb.queues.deadLetters(...)`).

### 3.2 Backups and operational endpoints

The JavaScript SDK wraps the backup routes (FB-83) as `fb.system.backups`; the
Rust SDK does not, and neither wraps `/metrics`. They are administrator
operations; call them with an administrator session or an admin service-account
key:

```ts
const snap = await fb.system.backups.create({ label: "before-migration", retention: { keepLast: 7 } });
await fb.system.backups.verify(snap.id);
```

Restore has no HTTP route: stop the server and run `forgebase restore`
([api.md §11.2](api.md#112-backups-apiv1systembackups)). `/metrics` and
`/api/system/*` follow `[server] ops_access` (default `loopback`): from another
host, only administrators can read them, and a scoped key needs `system:read`
([api.md §11.3](api.md#113-operational-endpoints-ops_access)).

### 3.3 Verifying push webhooks

A push subscription with a webhook endpoint receives `{subscription,
delivery_attempt, message}` signed with `X-Forgebase-Signature:
t=<unix s>,v1=<hex HMAC-SHA256(secret, "<t>.<raw body>")>`
([ADR-0022 §13](adr/0022-queues-pubsub.md)). The JavaScript SDK verifies it with
Web Crypto, so it runs on Node 18+, Deno, Bun and edge runtimes:

```ts
import { constructWebhookEvent, WebhookVerificationError } from "@forgebase/sdk/webhooks";

const event = await constructWebhookEvent({
  secret: process.env.FORGEBASE_WEBHOOK_SECRET!, // or [newSecret, oldSecret] while rotating
  header: request.headers.get("x-forgebase-signature"),
  body: await request.text(),                    // the raw body, never re-serialised JSON
  toleranceSeconds: 300,
});
```

A header may carry several `v1=` values during a secret rotation; the check
passes when any matches. The comparison is constant time and timestamps further
than `toleranceSeconds` from now (either direction) are rejected. Failures throw
`WebhookVerificationError` with a `reason`. Express, Next.js and Hono examples are
in [sdk/js/README.md#webhook-verification](../sdk/js/README.md#webhook-verification).

## 4. Types

The SDK's route types are generated from the server's OpenAPI document. The
checked-in copy is `sdk/js/openapi/openapi.json`, and the generated file is
`sdk/js/src/generated/openapi.ts`:

```sh
npm run generate                                 # from the checked-in document
npm run generate -- --url http://127.0.0.1:8090  # refresh the document from a running server
npm run generate:check                           # CI: fails when the generated file is stale
```

Use the types directly:

```ts
import type { components, paths } from "@forgebase/sdk";
```

## 5. Development and tests

```sh
cd sdk/js
npm install
npm run typecheck && npm run lint && npm test && npm run build
```

`npm test` runs the unit and HTTP tests and skips the live tests. The live tests
start the real server binary, seed an admin, and drive the SDK against it:

```sh
cargo build -p forgebase-cli                       # from the repository root
cd sdk/js
FB_LIVE_BIN=$PWD/../../target/debug/forgebase npm run test:live
```

`prepublishOnly` runs typecheck, lint, tests and build before a publish.

## 6. Known limitations

These are listed in [sdk/js/README.md#known-limitations](../sdk/js/README.md#known-limitations).
In short:

- The OpenAPI document leaves most `/api/auth/*`, policy, orgs and customization
  bodies as plain objects and omits query parameters. Those types in the SDK are
  written by hand from the server handlers. Compare the live
  `GET /api/openapi.json` with the SDK's types after a server upgrade.
- Passkeys are wrapped as option and verify calls; the browser credential APIs
  stay with the caller. OIDC protocol endpoints are left to OIDC libraries.
- Request bodies cannot be `ReadableStream`s (use the multipart or resumable
  upload helpers), and a non-2xx answer from a function is always raised as a
  typed error.
- Webhook-endpoint tooling on the server (attempt log, test send, redrive,
  secret rotation, health, event catalog) is not wrapped yet.

## 7. Versioning

The SDK follows the API's versioning rules. Within `/api/v1` the server only adds
fields, endpoints and optional parameters. Clients must ignore unknown fields
([api.md §2](api.md#2-versioning)). A breaking change to the server means a new
`/api/v2` path and a new SDK major version.

## 8. Dart / Flutter SDK

The `forgebase` package in [`sdk/dart`](../sdk/dart) is pure Dart: no Flutter
dependency, no `dart:io` or `dart:html`, so one package serves iOS, Android, web,
desktop and server-side Dart. It needs Dart 3.3 or newer and depends on
`package:http`, `package:web_socket_channel` and `package:crypto`. The detailed
reference is [sdk/dart/README.md](../sdk/dart/README.md).

```yaml
dependencies:
  forgebase:
    git:
      url: https://github.com/farzanshibu/forgebase
      path: sdk/dart
```

```dart
import 'package:forgebase/forgebase.dart';

final fb = ForgeBase('http://localhost:8090', storage: SecureTokenStorage());
await fb.auth.signIn(email: 'ada@example.com', password: '...');
final page = await fb.collection('posts').list(const ListOptions(sort: '-created'));
```

### 8.1 What it covers

| Area | Entry point | Notes |
|---|---|---|
| Authentication | `fb.auth` | Same flows as the JS SDK except cookie mode. Automatic refresh with rotation, shared across concurrent calls. The session lives in a `TokenStorage`: in memory by default, or your adapter ([README](../sdk/dart/README.md#token-storage) shows `flutter_secure_storage`). |
| Collections and records | `fb.collection(name)`, `fb.collectionOf<T>` | Filters with typed params, pagination as streams, optimistic `version:` (`If-Match`), export and import. |
| Realtime | `fb.realtime` | WebSocket only. Reconnects with backoff and resumes each subscription from its last cursor. Channels, presence, publish, `watch` as a `Stream`. |
| Storage | `fb.storage` | Buckets, objects, ranges, streaming download, signed URLs, multipart and resumable uploads. |
| Key-value | `fb.kv(namespace)` | Strings, bytes, counters, hashes, lists, sets, sorted sets, streams, raw commands, pub/sub. |
| Functions | `fb.functions` | Invoke (JSON or raw), deploy, logs, versions, secrets. |
| Queues and jobs | `fb.queues`, `fb.jobs` | Same as the JS SDK: streaming pull with reconnect, workflows, progress over realtime. |
| Entitlements, billing | `fb.entitlements`, `fb.billing.store` | Quota calls and App Store / Google Play / Creem purchase verification. |
| Raw requests | `fb.request(path, ...)` | For routes the SDK does not wrap yet. |

Retries, consistency tokens and the error mapping behave as in
[§2.3](#23-retries-and-errors). The exception classes use Dart names
(`ForgeBaseException`, `ValidationException`, `NotFoundException`, ...); the
README has the table.

### 8.2 Differences from the JavaScript SDK

- No cookie mode (bearer sessions only) and no SSE fallback for realtime.
- No `AbortSignal`: cancel a stream by cancelling its subscription.
- Server objects are `Map<String, dynamic>`; typed classes cover sessions, pages,
  storage objects, jobs, queue messages and realtime events. They are written by
  hand against the OpenAPI document and covered by tests. There is no generated
  types file.
- Vector, graph, SQL, credits and organization-admin routes are not wrapped; use
  `fb.request`.

### 8.3 Development and tests

```sh
cd sdk/dart
dart pub get
dart analyze --fatal-infos && dart format --set-exit-if-changed . && dart test
```

`dart test` uses `package:http`'s `MockClient` and a fake `WebSocketChannel`, and
skips `test/live_test.dart`. Set `FORGEBASE_LIVE_URL`,
`FORGEBASE_LIVE_ADMIN_EMAIL` and `FORGEBASE_LIVE_ADMIN_PASSWORD` to run the live
test against a server. CI runs analysis, the format check and the unit tests in
the `sdk-dart` job.
## 9. Kotlin SDK (Android and JVM)

[`sdk/kotlin`](../sdk/kotlin) is one Kotlin/JVM library (Kotlin 2, JVM 17, explicit API mode) built on OkHttp,
kotlinx.coroutines and kotlinx.serialization. It follows the module layout of the JavaScript SDK
(`fb.auth`, `fb.collection(...)`, `fb.storage`, `fb.realtime`, `fb.functions`, `fb.kv`, `fb.queues`, `fb.jobs`,
`fb.entitlements`, `fb.billing`, `fb.vector`, `fb.graph`, `fb.sql`, `fb.credits`) with suspend functions and `Flow`s.

```kotlin
val fb = ForgeBase("https://api.example.com", ForgeBaseOptions(tokenStore = myEncryptedStore))
fb.auth.signIn("ada@example.com", "password")
val posts = fb.collection("posts").list(ListOptions(filter = "views > {:min}", params = mapOf("min" to 100)))
fb.realtime.subscribe(SubscribeOptions(collection = "posts")).collect { event -> /* resumes after a reconnect */ }
```

- **Auth**: refresh-token rotation is single-flight, a `401` is retried once after a refresh, and the session is kept in a
  pluggable `TokenStore` (the README shows an `EncryptedSharedPreferences` adapter). Social sign-in is `oauth.start()` (open the
  URL in a Custom Tab) then `oauth.completeFromUrl()` on the App Link redirect; the server holds the PKCE verifier.
- **Realtime**: WebSocket only. Each subscription resumes from its last cursor after a reconnect.
- **Webhooks**: `Webhooks.constructEvent(secrets, header, rawBody)` for JVM servers, with the same test vectors as the JS SDK.
- **Tests**: `cd sdk/kotlin && ./gradlew test` (MockWebServer for HTTP, WebSocket and SSE; no server needed). CI runs it on JDK 17.
- **Left out**: administration APIs (function deploy, bucket ACLs, billing catalog, organizations, identity admin, system),
  cookie sessions, the realtime SSE fallback and OpenAPI code generation. `fb.request(path)` reaches any route.

## 10. Swift SDK

Native Swift package `ForgeBase` in [`sdk/swift`](../sdk/swift), version 0.1.0, for iOS 15, macOS 12, tvOS 15, watchOS 8 and
visionOS 1 (Swift 5.9+). The reference is [`sdk/swift/README.md`](../sdk/swift/README.md). It has no third-party dependencies: `URLSession`,
`URLSessionWebSocketTask`, `CryptoKit`, `Security` (Keychain) and `AuthenticationServices`.

```swift
import ForgeBase

let fb = ForgeBaseClient(baseURL: URL(string: "http://localhost:8080")!,
                         tokenStore: KeychainTokenStore(service: "com.example.app.forgebase"))
try await fb.auth.signIn(email: "ada@example.com", password: "...")
let posts = try await fb.collection("posts").list(ListOptions(filter: "views > {:min}", params: ["min": 100], sort: ["-views"]))
let sub = try await fb.realtime.subscribe(.collection("posts"))
for await event in sub.events { /* .record, .message, .presence, .reset */ }
```

It mirrors the method structure and behaviour of the JavaScript SDK (retries and `Retry-After`, error mapping, refresh-token rotation with one
shared refresh and a retry after `401`, commit tokens, realtime resume from the last cursor) with Swift types: `Codable` records, `AsyncStream`
for realtime events, one `ForgeBaseError` whose `kind` replaces the JavaScript error classes.

| Area | Swift API |
|---|---|
| Auth | `fb.auth`: sign-up, password / MFA / magic link / OTP sign-in, reset and verification flows, refresh, sign-out, PATs, sessions, `stateChanges()`; OAuth with `fb.auth.oauth.signInWithBrowser` (`ASWebAuthenticationSession`); `TokenStore` (`InMemoryTokenStore`, `KeychainTokenStore`) |
| Records | `fb.collection("posts", as: Post.self)`: list / pages / iterate / get / create / update / replace / delete / count / export / import, `version:` for `If-Match`; `fb.collections` |
| Storage | `fb.storage`: buckets, `from(bucket)` objects (data or file bodies, ranges, download to file), signed URLs (also as a `URLRequest` for background sessions), multipart and resumable uploads |
| Realtime | `fb.realtime`: collection, record and channel subscriptions, presence, publish, reconnect with cursor resume |
| Services | `fb.functions` (invoke), `fb.kv`, `fb.queues` (publish, pull, ack, streaming pull), `fb.jobs`, `fb.entitlements`, `fb.billing` (customer, portal, subscriptions, invoices, StoreKit / Play / Creem verification), `fb.credits` (reads), `fb.vector` (search), `fb.graph` (reads), `fb.sql`; `Webhook` verification |

Administration APIs (identity and organization management, access policies, billing catalog and settings, entitlement and credit
administration, function deployment, vector and graph writes, `system`) are left to the JavaScript and Rust SDKs; `fb.request(...)` reaches any
route. The OAuth helper relies on the server doing PKCE with the provider: the app only returns `code` and `state`.

```sh
cd sdk/swift
swift build -Xswiftc -warnings-as-errors && swift test   # offline: URLProtocol mock and a fake WebSocket
```
