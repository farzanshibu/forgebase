# ForgeBase Swift SDK

Native Swift SDK for [ForgeBase](../../README.md) on iOS, macOS, tvOS, watchOS and visionOS: auth, records, storage, realtime,
functions, key-value, queues, jobs, entitlements, billing, credits, vector search, graph and SQL reads.

- Swift 5.9+, `async`/`await`, `Sendable` throughout (the package builds with complete strict-concurrency checking).
- **No third-party dependencies.** `URLSession` for HTTP, `URLSessionWebSocketTask` for realtime, `CryptoKit` for hashes and webhook HMAC,
  `Security` for the Keychain, `AuthenticationServices` for browser sign-in.
- Platforms: iOS 15, macOS 12, tvOS 15, watchOS 8, visionOS 1.
- Method structure and behaviour mirror the [JavaScript SDK](../js/README.md) (retries, error mapping, token refresh, consistency tokens,
  realtime resume), written the Swift way: typed `Codable` records, `AsyncStream`s for events, one `ForgeBaseError` with a `kind`.
- License: MIT OR Apache-2.0.

## Install

Swift Package Manager, from this repository's `sdk/swift` directory (or the published mirror of it):

```swift
// Package.swift
dependencies: [
    .package(url: "https://github.com/your-org/forgebase-swift.git", from: "0.1.0"),
],
targets: [
    .target(name: "MyApp", dependencies: [.product(name: "ForgeBase", package: "forgebase-swift")]),
]
```

In Xcode: **File ▸ Add Package Dependencies…** and pick the `ForgeBase` library. For a local checkout use
`.package(path: "../forgebase/sdk/swift")`.

## Quick start

```swift
import ForgeBase

let fb = ForgeBaseClient(baseURL: URL(string: "https://api.example.com")!)

// Sign in (the session is kept in the token store and refreshed for you).
let result = try await fb.auth.signIn(email: "ada@example.com", password: "…")
if let challenge = result.challenge {                       // MFA enabled
    try await fb.auth.verifyMFA(mfaToken: challenge.mfaToken, code: "123456")
}

// Records, typed.
struct Post: Codable, Sendable { var id: String?; var title: String; var views: Int? }
let posts = fb.collection("posts", as: Post.self)
let page = try await posts.list(ListOptions(filter: "views > {:min}", params: ["min": 100], sort: ["-views"], perPage: 20))
let created = try await posts.create(Post(id: nil, title: "Hello"))

// Realtime.
let sub = try await fb.realtime.subscribe(.collection("posts"))
for await event in sub.events {
    if case .record(let e) = event { print(e.kind, e.recordId, try e.record(as: Post.self).title) }
}
```

Records default to `JSONObject` (`[String: JSON]`) when you do not give a type: `fb.collection("posts").get(id)["title"]?.stringValue`
or `post.title` through dynamic member lookup.

## Configuration

```swift
var config = ForgeBaseConfiguration(baseURL: url)
config.apiKey = "fb_…"                       // X-API-Key (service account, personal access token)
config.accessToken = "…"                     // or a static bearer token (never refreshed)
config.tokenStore = KeychainTokenStore(service: "com.example.myapp.forgebase")
config.consistency = .session                // .strong: read every call from the primary
config.headers = ["X-Forgebase-Org": "org_1"]
config.retry = RetryPolicy(maxRetries: 3)    // see "Retries and errors"
config.session = URLSession(configuration: …) // pinning, proxies, tests
let fb = ForgeBaseClient(configuration: config)
```

`ForgeBaseClient(baseURL:apiKey:tokenStore:session:)` covers the common case. `fb.request("GET", "/api/v1/kv/_info")` reaches any route
the typed API does not wrap yet.

The client is a `final class` that is `Sendable`: create one and share it across tasks and actors.

## Authentication

Every sign-in stores the session in the token store, refreshes the access token `refreshSkew` (30 s) before it expires (one refresh
request no matter how many calls are in flight), and retries a request once after a `401`. A refresh token the server rejects
(revoked, reused) signs the user out; a network error during refresh keeps the session.

```swift
try await fb.auth.signUp(email: "ada@example.com", password: "…", data: ["name": "Ada"])   // does not sign in
let r = try await fb.auth.signIn(email: …, password: …)                                    // .session or .mfaRequired
try await fb.auth.verifyMFA(mfaToken: …, code: …)
try await fb.auth.requestMagicLink(email: …);  try await fb.auth.confirmMagicLink(token: …)
try await fb.auth.requestOTP(email: …);        try await fb.auth.verifyOTP(email: …, code: …)
try await fb.auth.requestPasswordReset(email: …); try await fb.auth.confirmPasswordReset(token: …, password: …)
let me = try await fb.auth.me()
try await fb.auth.signOut()                      // revokes on the server; forgets locally even if the call fails

for await change in await fb.auth.stateChanges() { /* .signedIn, .tokenRefreshed, .signedOut */ }
```

### Token stores

`TokenStore` is a three-method protocol (`load`, `save`, `clear`). The default `InMemoryTokenStore` forgets the session on relaunch.
`KeychainTokenStore` keeps it in the Keychain (JSON in one generic-password item, readable after first unlock, this device only by
default; set `accessGroup` to share it with extensions). Implement the protocol to store sessions anywhere else.

```swift
let fb = ForgeBaseClient(baseURL: url, tokenStore: KeychainTokenStore(service: "com.example.myapp.forgebase"))
if await fb.auth.session() != nil { /* already signed in */ }
```

### OAuth (Sign in with GitHub, Google, …) with `ASWebAuthenticationSession`

```swift
// 1. On the server, register the app's redirect as the provider redirect URI, e.g. myapp://auth/callback
// 2. In the app (iOS, macOS, visionOS, watchOS; not tvOS):
let result = try await fb.auth.oauth.signInWithBrowser(provider: "github", callbackURLScheme: "myapp")
```

`signInWithBrowser` calls `oauth.start`, shows the provider's page in an `ASWebAuthenticationSession`, reads `code` and `state` from the
redirect and calls `oauth.complete`. Cancelling the sheet throws a `ForgeBaseError` of kind `.oauth` with code `oauth_cancelled`. The pieces
are public if you want to run the flow yourself (`start`, `complete(provider:code:state:)`, `complete(provider:redirectURL:)`), and the browser
is pluggable through `BrowserAuthenticator` (use it in tests, or for `SFSafariViewController` flows).

**PKCE.** ForgeBase is the OAuth client of the provider. It generates the S256 code verifier, keeps it sealed with the one-time `state`, puts the
challenge in the authorization URL, and redeems the code itself, so the app never handles a verifier or a client secret. What protects the
redirect is the single-use, server-bound `state`: always return the `code` and `state` of the same flow to `complete`.

## Records

```swift
let posts = fb.collection("posts", as: Post.self)
try await posts.list(ListOptions(filter: …, params: …, sort: ["-created"], expand: ["author"], fields: […], search: …, page: 2, perPage: 50, skipTotal: true))
for try await post in posts.iterate(ListOptions(filter: "published = true")) { … }     // follows cursors, else page numbers
for try await page in posts.pages(ListOptions(perPage: 100)) { … }
let all = try await posts.all(ListOptions(filter: …))
let one = try await posts.get("id", expand: ["author"])
let first = try await posts.first(filter: "slug = {:s}", params: ["s": "hello"])
try await posts.create(Post(…)); try await posts.update(id, patch); try await posts.replace(id, post); try await posts.delete(id)
let n = try await posts.count(filter: "views > 10")
```

With `params` the typed JSON query endpoint is used (`{:name}` placeholders); without, a plain `GET`.

**Optimistic concurrency.** Every record carries `_version`. Pass it back as `version:` and a stale write throws a `.conflict` error
(HTTP 412) instead of overwriting:

```swift
do { try await posts.update(id, ["title": "New"] as JSONObject, version: post._version) }
catch let e as ForgeBaseError where e.kind == .conflict { /* reload and retry */ }
```

## Storage

```swift
let avatars = fb.storage.from("avatars")
try await avatars.put("u1/me.png", data: png, options: PutOptions(contentType: "image/png", metadata: ["owner": "u1"], checksumSha256: sha256Hex(png)))
try await avatars.put("clip.mov", fileURL: url)                       // streamed from disk, not loaded into memory

let d = try await avatars.download("u1/me.png")                       // Download: data, statusCode, contentType, etag, headers
try await avatars.download("clip.mov", to: destinationURL)            // straight to a file
try await avatars.download("big.bin", DownloadOptions(range: .bytes(start: 0, end: 1023)))   // 206

let link = try await avatars.createSignedURL("u1/me.png", expiresIn: 600)     // anyone with it can GET until it expires
let up = try await avatars.createSignedUploadURL("u1/new.png", contentType: "image/png")
try await fb.storage.signed.upload(up, .data(png))                              // no credentials sent
```

Multipart and resumable uploads handle large files; sources can be `Data` or a file, read one part at a time:

```swift
// Multipart: numbered parts, aborts the session if a part fails.
let obj = try await fb.storage.uploads.upload(bucket: "docs", key: "report.pdf", source: .file(url), partSize: 8 << 20,
                                              onProgress: { print($0.fraction) })

// Resumable: create with the total size, send chunks at the server's offset. After a crash or relaunch,
// keep the session id and call resume again: it asks the server where it stopped.
let session = try await fb.storage.uploads.create(bucket: "docs", key: "movie.mov", CreateUploadOptions(kind: "resumable", size: size))
let done = try await fb.storage.uploads.resume(session.id, source: .file(url))
```

### Background transfers

The SDK's own requests use `async` `URLSession` APIs, which a *background* `URLSession` does not support, and iOS suspends an app that
only holds foreground transfers. For uploads and downloads that must survive suspension, let the server hand you a signed URL and give
the transfer to your own background session:

```swift
let signed = try await fb.storage.from("videos").createSignedUploadURL("clip.mov", contentType: "video/quicktime")
let request = try fb.storage.signed.urlRequest(signed, contentType: "video/quicktime")      // PUT, headers set, no credentials needed
let session = URLSession(configuration: .background(withIdentifier: "com.example.uploads"), delegate: myDelegate, delegateQueue: nil)
session.uploadTask(with: request, fromFile: fileURL).resume()
```

The signed URL carries its own authorisation, so the background task needs no token and a relaunch can pick the transfer up by identifier.
The same works for downloads (`createSignedURL` then `downloadTask`). Pass the finished object's metadata through
`fb.storage.from(…).meta(key)` afterwards if you need it.

## Realtime

One `URLSessionWebSocketTask` carries every subscription. The credential goes in a header (`X-API-Key` / `Authorization`), never in the URL.

```swift
let posts = try await fb.realtime.subscribe(.collection("posts", filter: "published = true"))
let room  = try await fb.realtime.subscribe(.channel("room:42"))
let one   = try await fb.realtime.subscribe(.record(collection: "posts", id: "p1"))

for await event in room.events {
    switch event {
    case .record(let e):   …   // kind (.created/.updated/.deleted), recordId, data, cursor, commit
    case .message(let m):  …   // channel, from, data
    case .presence(let p): …   // .join / .leave, member
    case .reset(let r):    …   // the server could not replay: reload your state, then continue
    }
}

try await fb.realtime.publish(channel: "room:42", data: ["text": "hi"])
let members = try await fb.realtime.joinPresence(channel: "room:42", meta: ["name": "Ada"])   // channel must be subscribed
try await fb.realtime.leavePresence(channel: "room:42")
```

- **Reconnect and resume.** A dropped socket reconnects with exponential backoff and full jitter (`RealtimeConfiguration`). Every
  subscription is re-established from the highest cursor it delivered, so no event is lost or repeated; a subscription the server now refuses
  is dropped and reported. The server's heartbeat is answered, a silent connection is closed after 2.5 intervals (`4002`), a `4001` close
  refreshes the token first, and a refreshed access token is sent to the live socket.
- Leaving the `for await` loop or cancelling its task unsubscribes. `subscription.cursor()` is the resume position: persist it to resume
  across launches (`SubscribeOptions(cursor:)`).
- `connectionEvents()` is a stream of `.open`, `.close`, `.reconnecting`, `.error` for a status indicator.
- Record events with a `commit` position feed the client's consistency tokens, so a read after the event sees the record.
- Realtime uses WebSockets only (the JavaScript SDK's SSE fallback is not needed on Apple platforms). To run without a network,
  pass your own `WebSocketFactory` in `RealtimeConfiguration`.

## Functions, KV, queues, jobs

```swift
// Functions
let out = try await fb.functions.invoke("resize", input: ["key": "u1/me.png", "width": 128])      // JSON in, JSON out
let typed: Thumb = try await fb.functions.invoke("resize", input: ResizeInput(…), as: Thumb.self)
let raw = try await fb.functions.invokeRaw("render", body: data, contentType: "application/json") // status, headers, bytes

// KV
let cache = fb.kv.ns("cache")
try await cache.set("k", "v", KVSetOptions(ttlMs: 60_000));  let v = try await cache.text("k")
for try await key in cache.keys(match: "user:*") { … }
for try await e in cache.subscribe("news") { if case .message(let m) = e { print(m.message.string) } }   // pub/sub over SSE, reconnects

// Queues
try await fb.queues.publish("orders", ["id": 1], PublishOptions(dedupId: "order-1"))     // a dedupId makes the publish retry-safe
let batch = try await fb.queues.pull("worker", PullOptions(maxMessages: 10, waitMs: 5000))
try await fb.queues.ack("worker", ackIds: batch.map(\.ackId))
let stream = fb.queues.subscribe("worker") { delivery in            // acked when the handler returns, nacked when it throws
    try await handle(delivery.payload(as: Order.self))
}
stream.cancel()

// Jobs
let job = try await fb.jobs.enqueue("thumbnail", payload: ["key": "u1/me.png"], EnqueueOptions(idempotencyKey: "thumb-u1"))
for try await update in try await fb.jobs.updates(job.id) { print(update.status as Any, update.progress as Any) }   // stored state, then live
let finished = try await fb.jobs.wait(job.id, timeout: 120)           // polls; throws .jobFailed when it ends dead/cancelled
```

Function invocations are not retried unless you ask (`InvokeOptions(retry: true)`), and neither are un-keyed job enqueues or pulls: those calls
could run twice.

## Entitlements, billing, credits

```swift
if try await fb.entitlements.has(.feature("export")) { … }
let decision = try await fb.entitlements.checkQuota(QuotaRequest(feature: "ai.tokens", units: 500))
try await fb.entitlements.consume(QuotaRequest(feature: "ai.tokens", units: 500, idempotencyKey: UUID().uuidString))   // .quotaExceeded past a hard limit
try await fb.entitlements.withReservation(QuotaRequest(feature: "render", idempotencyKey: id)) { _ in try await render() }  // commit or release

// Customer billing
_ = try await fb.billing.customers.put(BillingCustomerInput(email: "ada@example.com", currency: "usd"))
let portal = try await fb.billing.portal.get()
let page = try await fb.billing.portal.paymentMethod(returnURL: "myapp://billing")          // open page.url in a browser sheet
let checkout = try await fb.billing.subscriptions.create(items: [BillingItem(price: "pro_monthly")], idempotencyKey: UUID().uuidString)

// StoreKit 2: verify the signed transaction server-side, then finish it
for await result in Transaction.updates {
    guard case .verified(let tx) = result else { continue }
    _ = try await fb.billing.store.verify(.appStore, StoreReceipt(signedTransaction: result.jwsRepresentation))
    await tx.finish()
}
```

Credits (`fb.credits`) reads balances, lots and the ledger. Granting credits, rates and the billing catalog are administrator operations.

## Vector, graph, SQL

```swift
let hits = try await fb.vector.collection("docs").search(vector: embedding, VectorQuery(k: 5, filter: "lang = {:l}", params: ["l": "en"]))
let hybrid = try await fb.vector.collection("docs").hybrid(vector: embedding, text: "swift concurrency", VectorQuery(k: 5))
let friends = try await fb.graph.neighbors(of: "n1", direction: .out, edgeType: "KNOWS")
let rows = try await fb.sql.query("select id, name from users where id > ?", params: [0])
let typed = try await fb.sql.query("select id, name from users", as: User.self)
```

## Errors, retries, consistency

Every failure is a `ForgeBaseError`:

| `kind` | When |
|---|---|
| `.validation` | 400 / 422; `error.fields["title"]?.message` has per-field errors |
| `.authentication` / `.permission` / `.notFound` | 401 / 403 / 404 |
| `.conflict` | 409 / 412 (unique violation, stale `version`, wrong upload offset: `details["upload_offset"]`) |
| `.rateLimited` / `.quotaExceeded` | 429 from rate limiting (retried) / an exhausted quota (never retried) |
| `.server` / `.api` | 5xx / any other status |
| `.network` | no response (DNS, refused, timeout); `underlying` has the `URLError` |
| `.invalidResponse` | a 2xx answer the SDK could not read |
| `.realtime`, `.oauth`, `.webhook`, `.jobFailed`, `.invalidRequest` | stream, browser sign-in, webhook, job and argument errors |

`status`, `code` (the server's machine code), `message`, `requestId`, `details`, `retryAfter` and the parsed `body` are on every error.
Cancelling a task throws `CancellationError`, not a `ForgeBaseError`.

**Retries.** `429` and `503` are retried for every method, `502` and `504` for idempotent ones (`GET`, `HEAD`, `PUT`, `DELETE`), and so are network
failures of idempotent calls. The delay is the server's `Retry-After` when sent (a longer wait than `maxRetryAfter` fails at once), else
exponential backoff with full jitter. A quota error is final.

**Read-your-writes with replicas.** The SDK remembers the commit positions the server returns after writes (`X-ForgeBase-Commit`) and sends them
back as `X-ForgeBase-Read-After`, so a replica only answers once it replayed your write. `consistency: .strong` (or `ListOptions(consistency:)`)
reads from the primary. Without replicas the server sends no tokens and nothing extra is sent.

## Webhooks

`Webhook.construct(secrets:header:body:)` verifies the `X-Forgebase-Signature` of a push subscription (HMAC-SHA256 over `"<t>.<raw body>"`,
a 5-minute tolerance, several secrets while rotating) and returns the envelope. It is for server-side Swift or tests; an app does not receive
webhooks. `Webhook.sign` produces headers for tests.

## SwiftUI

```swift
import SwiftUI
import ForgeBase

@MainActor final class PostsModel: ObservableObject {
    @Published var posts: [Post] = []
    let fb: ForgeBaseClient

    init(fb: ForgeBaseClient) { self.fb = fb }

    func load() async {
        do {
            posts = try await fb.collection("posts", as: Post.self).list(ListOptions(sort: ["-created"], perPage: 50)).items
            let sub = try await fb.realtime.subscribe(.collection("posts"))
            for await event in sub.events {                    // ends (and unsubscribes) when the .task is cancelled
                guard case .record(let e) = event, let post = try? e.record(as: Post.self) else { continue }
                switch e.kind {
                case .deleted: posts.removeAll { $0.id == post.id }
                default: if let i = posts.firstIndex(where: { $0.id == post.id }) { posts[i] = post } else { posts.insert(post, at: 0) }
                }
            }
        } catch { print(error) }
    }
}

struct PostsView: View {
    @StateObject var model: PostsModel
    var body: some View {
        List(model.posts, id: \.id) { Text($0.title) }.task { await model.load() }
    }
}
```

A runnable command-line tour is in [`Examples/ForgeBaseExample`](Examples/ForgeBaseExample/Example.swift)
(`FORGEBASE_URL=… FORGEBASE_API_KEY=… swift run ForgeBaseExample list posts`).

## Testing your app

The client takes any `URLSession`, so a `URLProtocol` stub stands in for the server (see `Tests/ForgeBaseTests/TestSupport.swift`, which also
has a fake `WebSocketConnection`):

```swift
let cfg = URLSessionConfiguration.ephemeral
cfg.protocolClasses = [MyStubProtocol.self]
let fb = ForgeBaseClient(configuration: ForgeBaseConfiguration(baseURL: url, apiKey: "test", session: URLSession(configuration: cfg)))
```

## Not covered

The Swift SDK targets what an app does. Server and operator administration is left to the [JavaScript](../js/README.md) and
[Rust](../rust/README.md) SDKs and the CLI: collection schema design beyond the basic CRUD wrappers, identity and organization administration
(API keys, service accounts, OAuth clients, realms, teams, roles, ACLs and policies), billing catalog / providers / settings / operator views,
entitlement catalog and overrides, credit posting and rates, function deployment and secrets, vector collection and field management, graph
writes, backups, features, topology and other `system` APIs. Any of them is reachable with `fb.request(_:_:query:body:)`.

Also: no SSE realtime fallback (WebSocket only), no passkey (WebAuthn) wrappers, cookie sessions are not used (tokens only), and timestamps
in typed values are kept as the server's RFC 3339 strings.

## Development

```sh
cd sdk/swift
swift build -Xswiftc -warnings-as-errors
swift test                       # 140+ offline tests: URLProtocol mock + fake WebSocket, no network
swift run ForgeBaseExample       # CLI tour
```

Build output goes to `.build/` (git-ignored); pass `--scratch-path` to put it elsewhere. Cross-platform check:
`xcodebuild build -scheme ForgeBase -destination 'generic/platform=iOS'` (also `tvOS`, `watchOS`, `visionOS`).
