# ForgeBase Kotlin SDK

Kotlin SDK for [ForgeBase](../../README.md) that runs on the JVM (servers, desktop, tests) and on Android. It is one pure
Kotlin/JVM library, so there is no Android Gradle Plugin in the build: add it to an Android app like any other dependency.

- Coroutines first: every call is a cancellable `suspend` function; subscriptions are `Flow`s.
- Three dependencies: `kotlinx-coroutines-core`, `kotlinx-serialization-json` and OkHttp (HTTP and WebSocket).
- Kotlin 2, JVM target 17, Kotlin *explicit API* mode, no `java.net.http`, no `java.time` or `java.util.Base64`
  (so Android API 21+ works without desugaring; this repository tests the JVM build, not a device).
- Typed exceptions (`NotFoundException`, `ConflictException`, `QuotaExceededException`, ...), retries with backoff,
  single-flight token refresh, read-your-writes commit tokens.
- Licence: MIT OR Apache-2.0.

It mirrors the structure and behaviour of the [JavaScript SDK](../js/README.md). What is deliberately left out
(administration APIs) is listed under [Not covered](#not-covered).

## Install

The artifact is `dev.forgebase:forgebase`. It is not on Maven Central yet. Build it from this repository:

```sh
cd sdk/kotlin
./gradlew publishToMavenLocal      # then: mavenLocal() + implementation("dev.forgebase:forgebase:0.1.0")
```

or include it in your build (`includeBuild("path/to/forgebase/sdk/kotlin")` in `settings.gradle.kts`, then
`implementation("dev.forgebase:forgebase")`).

Android needs `<uses-permission android:name="android.permission.INTERNET" />`. Use `https://` URLs (cleartext HTTP to a
local server needs a network security config).

## Quick start

```kotlin
import dev.forgebase.*

val fb = ForgeBase("https://api.example.com")            // or ForgeBaseOptions(apiKey = "fb_...") for servers

when (val r = fb.auth.signIn("ada@example.com", "correct horse")) {
    is Session -> println("signed in as ${r.user}")
    is MfaChallenge -> fb.auth.verifyMfa(r.mfaToken, totpCode)
}

@Serializable data class Post(val id: String, val title: String, val views: Int = 0)

val posts = fb.collection("posts")
val page = posts.listAs<Post>(ListOptions(filter = "views > {:min}", params = mapOf("min" to 100), sort = listOf("-views"), perPage = 20))
val created = posts.create(mapOf("title" to "Hello"))             // Row = JsonObject
```

`Row` is a `JsonObject`; use `row.decode<Post>()`, `getAs<Post>(id)`, `listAs<Post>()` or `createFrom(post)` for typed
access with kotlinx.serialization (`ForgeJson` ignores unknown fields). Call `fb.close()` when you are done (stops the realtime
connection and background work). Everything below is on that one client.

### Options

```kotlin
ForgeBase(
    "https://api.example.com",
    ForgeBaseOptions(
        apiKey = null,                       // X-API-Key: API keys, service accounts, personal access tokens
        accessToken = null,                  // static bearer token (no refresh)
        tokenStore = InMemoryTokenStore(),   // where the session lives (see "Keeping the session")
        autoRefresh = true, refreshSkewMs = 30_000,
        retry = RetryOptions(maxRetries = 3, baseDelayMs = 250, maxDelayMs = 10_000),
        headers = mapOf("X-Forgebase-Org" to "acme"),
        consistency = Consistency.SESSION,   // STRONG reads every call from the primary
        httpClient = myOkHttpClient,         // proxies, pinning, interceptors; the SDK derives its own from it
        reconnect = ReconnectOptions(),      // realtime
    ),
)
```

## Authentication

Every sign-in stores the session in the client, refreshes the access token before it expires (**one** refresh request for any
number of concurrent callers), and retries a request once after a `401`. A refresh token the server rejects ends the session;
a transient failure keeps it.

```kotlin
fb.auth.signUp("ada@example.com", "password")                         // creates the user, no session
val r = fb.auth.signIn("ada@example.com", "password", SignInOptions(deviceName = "Pixel 9"))
fb.auth.requestMagicLink(email); fb.auth.confirmMagicLink(tokenFromLink)
fb.auth.requestOtp(email);       fb.auth.verifyOtp(email, "123456")
fb.auth.requestPasswordReset(email); fb.auth.confirmPasswordReset(token, newPassword)
fb.auth.me(); fb.auth.listSessions(); fb.auth.signOut()

fb.auth.sessionFlow.collect { session -> /* StateFlow<Session?>: drive your UI */ }
fb.auth.events.collect { e -> /* SIGNED_IN, TOKEN_REFRESHED, SIGNED_OUT */ }
```

### Keeping the session: `TokenStore`

The default store is in memory. Implement `TokenStore` to survive process restarts. The stored value is a JSON string with the
access and refresh tokens, so encrypt it. `EncryptedSharedPreferences` (androidx.security) adapter:

```kotlin
class EncryptedPrefsTokenStore(context: Context) : TokenStore {
    private val prefs = EncryptedSharedPreferences.create(
        context, "forgebase_session",
        MasterKey.Builder(context).setKeyScheme(MasterKey.KeyScheme.AES256_GCM).build(),
        EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
        EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM,
    )
    override suspend fun load(): String? = withContext(Dispatchers.IO) { prefs.getString("session", null) }
    override suspend fun save(value: String) = withContext(Dispatchers.IO) { prefs.edit().putString("session", value).apply() }
    override suspend fun clear() = withContext(Dispatchers.IO) { prefs.edit().remove("session").apply() }
}

val fb = ForgeBase(url, ForgeBaseOptions(tokenStore = EncryptedPrefsTokenStore(applicationContext)))
```

A Jetpack DataStore adapter is the same three functions over `dataStore.data.first()[KEY]` and `dataStore.edit { ... }`; wrap the
value with a Keystore-backed AEAD (Tink's `AndroidKeysetManager`) before writing, because DataStore itself does not encrypt.
A store that throws never breaks a request: the SDK treats an unreadable or failing store as "signed out" / "not saved".

### Social sign-in with Custom Tabs (and PKCE)

The server runs the authorization-code flow with **PKCE (`S256`)**, `state` and `nonce` against the provider (ADR-0009): it
generates and keeps the verifier, bound to the single-use `state`. The app only opens the URL and hands the `code` and `state`
back, so the SDK needs no verifier of its own.

1. In the server's provider configuration set the provider's `redirect_uri` to an HTTPS **App Link** of your app
   (`https://app.example.com/oauth/callback`, verified with Digital Asset Links) and register the same URL with the provider.
2. Start the flow and open the URL in a Custom Tab:

```kotlin
val start = fb.auth.oauth.start("google", SignInOptions(deviceName = Build.MODEL))
CustomTabsIntent.Builder().build().launchUrl(activity, Uri.parse(start.authorizationUrl))
```

3. The provider redirects to your App Link; the activity (`launchMode="singleTask"`, intent filter with
   `android:autoVerify="true"`) receives it and finishes the flow:

```kotlin
override fun onNewIntent(intent: Intent) {
    super.onNewIntent(intent)
    val url = intent.dataString ?: return
    lifecycleScope.launch {
        when (val r = fb.auth.oauth.completeFromUrl("google", url)) {   // checks ?error=, takes ?code= and ?state=
            is Session -> showHome()
            is MfaChallenge -> askForTotp(r)
        }
    }
}
```

`fb.auth.oauth.complete(provider, code, state)` takes the parts directly; `link()`, `identities()` and `unlink()` manage linked
accounts. For apps that use ForgeBase **as** the identity provider (ADR-0019, public clients need PKCE), `Pkce.generate()` gives a
verifier/challenge pair: send `challenge` with `code_challenge_method=S256` and `verifier` as `code_verifier` at the token endpoint.

## Records

```kotlin
val posts = fb.collection("posts")
posts.list(ListOptions(filter = "status = \"active\"", sort = listOf("-created"), expand = listOf("author"), perPage = 50))
posts.iterate(ListOptions(filter = "published = true")).collect { row -> /* every record, page by page (Flow) */ }
posts.pages().collect { page -> page.items; page.nextCursor }
posts.first("slug = \"hello\""); posts.count("views > 10")
posts.get(id); posts.create(mapOf(...)); posts.update(id, mapOf("title" to "New"), version = row["_version"]!!.jsonPrimitive.long)
posts.replace(id, data); posts.delete(id)
posts.export(TransferFormat.CSV); posts.import(TransferFormat.NDJSON, text)
```

With `params` the typed JSON query endpoint is used (`{:name}` placeholders are bound server side, never concatenated). With
`version`, the write sends `If-Match` and a stale record fails with `ConflictException` (optimistic concurrency). Paging follows
`nextCursor` when the server gives one, else page numbers.

## Storage

```kotlin
val avatars = fb.storage.from("avatars")
avatars.put("u1/me.png", bytes, PutOptions(contentType = "image/png", metadata = mapOf("owner" to "u1"), checksumSha256 = sha256Hex(bytes)))
avatars.put("u1/big.mov", File(path))                      // streamed from disk
avatars.bytes("u1/me.png"); avatars.text("note.txt"); avatars.downloadTo("u1/me.png", File(out))
avatars.download("video.mp4", DownloadOptions(range = ByteRange.Range(0, 1_048_575)))   // raw Response, 206
avatars.createSignedUrl("u1/me.png", expiresIn = 600)      // fb.storage.signed.resolve(url) → absolute URL
avatars.createSignedUploadUrl("u1/new.png"); fb.storage.signed.upload(signed, bytes)   // no credentials sent
avatars.iterate(ListObjectsOptions(prefix = "u1/")).collect { obj -> }
avatars.meta(key); avatars.copy(key, dest); avatars.move(key, dest); avatars.versions(key); avatars.restore(key, versionId); avatars.delete(key)

// Large files: numbered parts of 8 MiB (any order), aborted if a part fails.
fb.storage.uploads.upload("videos", "trip.mov", File(path), contentType = "video/quicktime") { p -> progress(p.uploadedBytes, p.totalBytes) }

// Resumable: sequential offsets; ask the server where it stopped and continue.
val session = fb.storage.uploads.create("videos", "trip.mov", CreateUploadOptions(kind = "resumable", size = file.length()))
fb.storage.uploads.resume(session.id, UploadSource.file(file)) { p -> ... }   // call again after a failure
```

`UploadSource` has `bytes`, `string` and `file` factories and reads byte ranges, so retries and parallel parts never share a
stream. `download()` returns the OkHttp `Response`: close it (or use `readText()` / `readBytes()` / `downloadTo`).

## Realtime

One WebSocket carries every subscription. It reconnects with jittered backoff and **resumes each subscription from its last
cursor** (events missed while offline are replayed; if the cursor is too old you get a `RealtimeEvent.Reset` and reload).

```kotlin
fb.realtime.subscribe(SubscribeOptions(collection = "posts", filter = "published = true"))
    .collect { e ->
        when (e) {
            is RealtimeEvent.Record -> println("${e.type} ${e.recordId} ${e.data}")
            is RealtimeEvent.Message -> println("${e.channel}: ${e.data}")
            is RealtimeEvent.Presence -> println("${e.member.actor} ${if (e.joined) "joined" else "left"}")
            is RealtimeEvent.Reset -> reloadFromScratch()
        }
    }                                                    // cancelling the collection unsubscribes

fb.realtime.subscribe(SubscribeOptions(channel = "room:42")).launchIn(scope)
fb.realtime.publish("room:42", mapOf("text" to "hi"))
val members = fb.realtime.joinPresence("room:42", meta = mapOf("name" to "Ada")); fb.realtime.leavePresence("room:42")
fb.realtime.presence("room:42")                           // REST
fb.realtime.state.collect { }                             // IDLE, CONNECTING, OPEN, RECONNECTING, CLOSED
fb.realtime.connectionEvents.collect { }                  // Open, Closed, Reconnecting, Error
```

Credentials go in the handshake headers (never the URL). An access token refreshed while connected is pushed over the socket; a
`4001` close refreshes and reconnects. A refused subscription fails its flow (`RealtimeException` / `PermissionException`). The
push-style `subscribe(options) { event -> }` overload calls the handler on a background thread. Server-sent events are not used as a
fallback: WebSocket is the only transport.

## Functions, KV, queues, jobs

```kotlin
val out = fb.functions.invoke("hello", mapOf("name" to "Ada"), InvokeOptions(path = "greet/en"))   // JsonElement
val typed = fb.functions.invokeAs<Reply>("hello", mapOf("name" to "Ada"))

val cache = fb.kv.ns("cache")
cache.set("k", "v", KvSetOptions(ttlMs = 60_000)); cache.text("k"); cache.incr("hits"); cache.hashSet("h", mapOf("a" to 1))
cache.zsetAdd("board", listOf("ada" to 10.0)); cache.exec(listOf("ZRANGE", "board", 0, -1)); cache.keys(KvScanOptions(match = "user:*")).collect { }
cache.subscribe("events").collect { m -> m.message.toText() }          // SSE with reconnect

fb.queues.publish("orders", mapOf("id" to 42), PublishOptions(attributes = mapOf("kind" to "rush"), dedupId = "order-42"))
fb.queues.pull("worker", PullOptions(maxMessages = 10, waitMs = 5_000)).forEach { m -> process(m.message.data); fb.queues.ack("worker", listOf(m.ackId)) }
fb.queues.subscribe("worker") { d -> handle(d.decode<Order>()) }       // ack on return, nack on throw; suspends until cancelled

val job = fb.jobs.enqueue("report.render", mapOf("id" to 7), EnqueueOptions(idempotencyKey = "report-7"))
fb.jobs.progress(job.id).collect { u -> u.progress?.percent }          // stored state first, then live updates
fb.jobs.await(job.id)                                                  // polls; throws JobFailedException if dead / cancelled
```

Deploying and managing functions (versions, secrets, KV rules), topic/subscription retention policy beyond the basics,
job workflows' builder and queue pause/resume are in the JavaScript SDK; the raw JSON shapes of these routes are available
through `fb.request(...)` and the `createWorkflow` / `queues` calls that take and return JSON.

## Entitlements, billing, credits

```kotlin
fb.entitlements.hasFeature("export")                                  // plan / feature / permission checks
fb.entitlements.checkQuota(QuotaRequest("api_calls", units = 5))
fb.entitlements.consume(QuotaRequest("api_calls"), idempotencyKey = "req-123")           // QuotaExceededException past a hard limit
fb.entitlements.withReservation(QuotaRequest("export"), "job-9") { /* work; commit on success, release on throw */ }

fb.billing.customers.get(); fb.billing.portal.get(); fb.billing.portal.paymentMethod(returnUrl = "myapp://billing")   // HostedPage.url → Custom Tab
fb.billing.subscriptions(); fb.billing.invoices()
fb.billing.store.verify("google_play", StoreReceipt(purchaseToken = token, productId = "pro_month"))   // Play Billing: verify, then the plan applies
fb.credits.balance()
```

## Vector, graph, SQL

```kotlin
fb.vector.collection("docs").search(embedding, VectorQuery(k = 5, filter = "year >= {:y}", params = mapOf("y" to 2020)))
fb.vector.collection("docs").hybrid(text = "kotlin coroutines", vector = embedding)
fb.graph.traverse("n1", maxDepth = 2); fb.graph.shortestPath("a", "b"); fb.graph.cypher("MATCH (n) RETURN n LIMIT 5")
fb.sql.query("select id, name from users where id > ?", listOf(10))   // List<JsonObject>
```

## Verifying push webhooks (JVM servers)

Push subscriptions POST `{subscription, delivery_attempt, message}` with
`X-Forgebase-Signature: t=<unix>,v1=<hex HMAC-SHA256(secret, "<t>.<body>")>`. Verify the **raw body bytes** before parsing.
The check is constant time, accepts several `v1` values (secret rotation) and several secrets, and has a 300 s tolerance.

```kotlin
// Ktor
post("/hooks/orders") {
    val body = call.receive<ByteArray>()
    val event = try {
        Webhooks.constructEvent(listOf(newSecret, oldSecret), call.request.header(Webhooks.SIGNATURE_HEADER), body)
    } catch (e: WebhookVerificationException) {
        return@post call.respond(HttpStatusCode.BadRequest, e.reason.wire)      // 4xx: not counted as delivered
    }
    handle(event.message.data); call.respond(HttpStatusCode.OK)
}
```

`Webhooks.verify(...)` returns a boolean; `Webhooks.sign(secret, body)` builds the header for tests of your own handler. The
vectors are the same as `sdk/js/test/webhooks.test.ts`. In Spring, take the body as `ByteArray` (`@RequestBody`) rather than a
deserialised object.

## Errors, retries and consistency

Every failure is a `ForgeBaseException` with `status`, `code`, `requestId`, `details`, `retryAfter`, `body`:

| Exception | When |
|---|---|
| `ValidationException` (`.fields`) | 400, 422 |
| `AuthenticationException` / `PermissionException` / `NotFoundException` | 401 / 403 / 404 |
| `ConflictException` | 409, 412 (unique violation, stale `version`, `offset_mismatch`) |
| `RateLimitException` | 429, already retried `maxRetries` times |
| `QuotaExceededException` | 429 with a quota code; never retried |
| `ServerException` | 5xx (503 is retried first) |
| `NetworkException`, `InvalidResponseException`, `RealtimeException`, `WebhookVerificationException`, `JobFailedException` | no response, unexpected body, WebSocket, webhook, job |

429 and 503 are retried (honouring `Retry-After` up to `maxRetryAfterMs`); 502 / 504 and network failures are retried for
idempotent methods only. `CancellationException` is never wrapped.

With read replicas the SDK keeps the highest commit position per set from `X-ForgeBase-Commit` and sends it back as
`X-ForgeBase-Read-After`, so a read sees your own writes. `ListOptions(consistency = Consistency.STRONG)` or
`ForgeBaseOptions(consistency = ...)` read the primary. Servers without replicas send nothing and the SDK sends nothing.

### Escape hatch

```kotlin
fb.request("/api/v1/kv/_info")                                   // JsonElement, with credentials, retries and error mapping
fb.transport.send("/api/v1/some/stream", headers = mapOf("Accept" to "text/event-stream"), streaming = true)  // raw Response
```

## Android notes

- Run the suspend functions from `viewModelScope` / `lifecycleScope`; they switch to OkHttp's own threads. Collect realtime
  `Flow`s with `repeatOnLifecycle(STARTED)` and call `fb.realtime.close()` / `fb.close()` when the owner goes away (the realtime
  client reconnects by itself after a network change).
- Create one `ForgeBase` per process (an `Application`-scoped singleton or your DI graph). Pass your app's `OkHttpClient` to share its
  connection pool, certificate pinning and interceptors.
- R8/ProGuard: kotlinx.serialization and OkHttp ship their own consumer rules. Your own `@Serializable` classes are covered by
  kotlinx.serialization's rules.
- Do not ship an API key (`fb_...`) or service-account key in an app: use user sign-in. Keys are for servers.

## Not covered

Server and organisation administration is left to the JavaScript and Rust SDKs, the CLI and the dashboard: function deploy /
versions / secrets, storage bucket ACLs and maintenance, queue administration beyond create/delete/metrics, the billing catalog,
providers, settings and operator views, entitlement catalog (plans, features, add-ons, overrides), credits writes (top-up, grant,
spend, refund), organizations, projects, teams and roles, access rules, identity administration (API keys, service accounts, OAuth
clients, realms), customization, system administration, SQL and vector schema changes, passkeys, and the document and index APIs.
`fb.request(...)` reaches any of those routes. Also not implemented: cookie sessions (browsers only), the server-sent-events
fallback for realtime, and code generation from the OpenAPI document (the typed models cover the commonly used shapes; everything
else is `JsonObject` / `JsonElement`).

## Development

```sh
cd sdk/kotlin
./gradlew test            # MockWebServer suites for HTTP, WebSocket and SSE, plus the webhook vectors
./gradlew build
./gradlew publishToMavenLocal
```

JDK 17 is required. Builds run with `allWarningsAsErrors` and explicit API mode, so the public surface is deliberate. When
`FORGEBASE_SDK_BUILD_DIR` is set, the build output goes to `$FORGEBASE_SDK_BUILD_DIR/forgebase` instead of `build/` (useful on
machines with little disk).
