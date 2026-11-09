# forgebase-sdk (Rust)

Async Rust client for [ForgeBase](../../README.md): authentication, collections and records,
realtime subscriptions (WebSocket with SSE fallback), object storage, key-value, server functions,
jobs and entitlements. It mirrors the TypeScript SDK in [`sdk/js`](../js).

* `reqwest` + `rustls` (the `ring` provider): **no OpenSSL**, no system TLS library.
* Typed errors with the server's error codes, status and request id.
* Automatic refresh of expired or rejected access tokens (rotating refresh tokens, single flight).
* Retries with exponential backoff and jitter for transient failures, `Retry-After` honoured.
* Realtime that reconnects and **resumes from the last cursor**: no lost and no duplicated events.
* Wire models generated from the server's OpenAPI document.

Requires Rust 1.97+ (edition 2024) and a Tokio runtime.

## Install

The crate is not published yet. Depend on it by path or git:

```toml
[dependencies]
forgebase-sdk = { path = "sdk/rust" }          # inside this repository
# forgebase-sdk = { git = "https://github.com/forgebase/forgebase", package = "forgebase-sdk" }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
serde_json = "1"
```

## Quick start

```rust
use forgebase_sdk::{ForgeBase, ListOptions, SignInOptions};
use serde_json::json;

#[tokio::main]
async fn main() -> forgebase_sdk::Result<()> {
    let fb = ForgeBase::new("http://localhost:8090")?;
    fb.auth().sign_in("me@example.com", "a long passphrase", &SignInOptions::default()).await?;

    let posts = fb.collection("posts");
    let created = posts.create(&json!({"title": "hello"})).await?;
    let page = posts.list(&ListOptions::new().filter(r#"title = "hello""#).sort("-created")).await?;
    println!("{} record(s), first id {}", page.items.len(), created["id"]);
    Ok(())
}
```

More in [`examples/`](examples): `quickstart`, `realtime`, `storage`, `kv_jobs_entitlements`.
They read `FORGEBASE_URL` and `FORGEBASE_API_KEY` (or email and password) from the environment.

## Client and configuration

```rust
use std::time::Duration;
use forgebase_sdk::{FileStore, ForgeBase, RetryPolicy};

let fb = ForgeBase::builder("https://forgebase.example.com")
    .api_key("fb_...")                                   // or .access_token(..); or sign in (below)
    .timeout(Duration::from_secs(30))
    .retry(RetryPolicy { max_retries: 5, ..RetryPolicy::default() })
    .session_store(FileStore::new("session.json"))       // keep the session across restarts
    .header("X-Trace", "abc")
    .build()?;
```

`ForgeBase` is cheap to clone; clones share credentials, the session and the realtime connection.
`fb.request("GET", "/api/v1/...", None)` is an escape hatch for endpoints without a helper.

## Authentication

| Call | |
|---|---|
| `auth().sign_up(email, password, collection, data)` | creates a user (does not sign in) |
| `auth().sign_in(..)`, `sign_in_admin(..)` | password sign-in → `SignInResult::Session` or `MfaRequired` |
| `auth().verify_mfa(mfa_token, code)` | second step: TOTP or recovery code |
| `auth().request_magic_link(..)`, `confirm_magic_link(token, ..)` | passwordless link |
| `auth().request_otp(..)`, `verify_otp(email, code, ..)` | emailed one-time code |
| `auth().refresh()`, `sign_out()`, `me()`, `watch()` | session helpers |
| `auth().mfa_totp_setup()`, `mfa_totp_confirm(code)`, `mfa_totp_disable(code)`, `mfa_recovery_codes(code)` | TOTP enrolment |
| `auth().create_token(..)`, `list_tokens()`, `revoke_token(id)` | personal access tokens (API keys) |
| `auth().list_api_keys()`, `revoke_api_key(id)`, `create_service_account(..)`, `create_service_account_key(..)` | API keys and service accounts |
| `auth().request_password_reset(..)`, `confirm_password_reset(..)`, `change_password(..)`, email verification | account flows |
| `auth().oauth_providers()`, `oauth_start(..)`, `oauth_complete(..)` | social / OIDC sign-in |

The session is stored in the client; the access token is refreshed **before** it expires and **after** a
401, once, with a single shared refresh for concurrent callers. A revoked refresh token ends the session
(`auth().watch()` reports it). API keys (`fb_...`) are sent as `X-API-Key` and never refreshed.

```rust
use forgebase_sdk::SignInResult;
match fb.auth().sign_in(email, password, &Default::default()).await? {
    SignInResult::Session(s) => println!("hello {}", s.session_id),
    SignInResult::MfaRequired(c) => { fb.auth().verify_mfa(&c.mfa_token, &code_from_user).await?; }
    _ => {}
}
```

## Collections and records

```rust
// Definitions are JSON (see "Known gaps").
fb.collections().create(&json!({"name": "posts", "type": "base",
    "fields": [{"name": "title", "type": "text", "required": true}]})).await?;

#[derive(serde::Deserialize)] struct Post { id: String, title: String }
let posts = fb.collection("posts").typed::<Post>();       // or untyped serde_json::Value

let p = posts.create(&json!({"title": "hi"})).await?;
posts.update(&p.id, &json!({"title": "hello"}), WriteOptions { version: Some(3) }).await?; // If-Match
let page = posts.list(&ListOptions::new()
    .filter("views > {:min}").params(json!({"min": 10}))  // typed, injection-safe parameters
    .sort("-created").per_page(50)).await?;
let mut all = posts.iter(ListOptions::new().per_page(200));  // lazy, follows cursors/pages
while let Some(post) = all.next().await? { /* ... */ }
```

Also: `get`, `get_expanded`, `replace` (PUT), `delete`, `count`, `first`, `all`, `aggregate`,
`export` / `import` (`json`, `ndjson`, `csv`).

## Realtime

```rust
let rt = fb.realtime();
let mut sub = rt.subscribe(SubscribeOptions::collection("posts").filter("views > 0")).await?;
while let Some(event) = sub.next().await {
    match event {
        RealtimeEvent::Record(r) => println!("{} {}", r.kind, r.record_id),
        RealtimeEvent::Reset(r) => { /* replay impossible: reload state, continue from r.cursor */ }
        RealtimeEvent::Error { code, .. } => { eprintln!("subscription ended: {code}"); break; }
        _ => {}
    }
}
let cursor = sub.cursor(); // persist it; later: SubscribeOptions::collection("posts").from_cursor(cursor)
```

One connection (a background task) carries every subscription. It uses WebSocket and falls back to
Server-Sent Events when WebSocket cannot connect (`RealtimeConfig::transport` forces one). It sends
heartbeats, reconnects with backoff and resubscribes every subscription **from its last cursor**.
Channels and presence: `publish(channel, data)`, `SubscribeOptions::channel(..)`, `join_presence`,
`leave_presence` (WebSocket only), `presence(channel)` (REST). Dropping a `Subscription` unsubscribes.

## Storage

```rust
let st = fb.storage();
st.create_bucket(&BucketInput { name: "docs".into(), ..Default::default() }).await?;
st.put_object("docs", "a/b.txt", "hello", &PutOptions::default()).await?;
let bytes = st.get_object("docs", "a/b.txt", None).await?;
let tail  = st.get_object_range("docs", "a/b.txt", 3, None).await?;
let mut s = st.get_object_stream("docs", "big.bin").await?;       // chunked download
let link  = st.sign("docs", &SignRequest { key: "a/b.txt".into(), method: Some(SignedMethod::GET),
                    expires_in: Some(60), ..Default::default() }).await?;
let bytes = anonymous_client.storage().download_signed(&link).await?; // no credentials needed
st.upload_multipart("docs", "big.bin", &data, 8 << 20, None).await?;  // parts + complete, aborts on failure
// resumable: create_upload, append_chunk(id, offset, bytes), get_upload(id) to find the offset
```

Also: `list_objects` (prefix, delimiter, cursor), `copy_object`, `move_object`, `delete_object`,
`delete_object_version`, `list_versions`, `upload_signed`.

## Queues

```rust
use forgebase_sdk::{NewMessage, NewSubscription, NewTopic, PullOptions, SeekTo, StreamOptions};
use serde_json::json;

let q = fb.queues();
q.create_topic(&NewTopic::new("orders")).await?;
q.create_subscription(&NewSubscription::new("billing", "orders")).await?;
q.publish("orders", &NewMessage::new(json!({"id": 1})).attr("kind", "rush").dedup_id("o-1")).await?;

// Pull and ack
for m in q.pull("billing", &PullOptions { max_messages: Some(10), wait_ms: Some(2000), ..Default::default() }).await? {
    q.ack("billing", &[m.ack_id]).await?;
}

// Streaming pull (server-sent events); settle each delivery, `None` means the server closed the stream
let mut stream = q.subscribe("billing", &StreamOptions::default()).await?;
while let Some(m) = stream.next().await? {
    stream.ack(&m).await?; // or stream.nack(&m) to redeliver after the backoff
}
q.seek("billing", &SeekTo::Time("2026-01-01T00:00:00Z".into())).await?; // replay
```

## KV, functions, jobs, entitlements

```rust
let kv = fb.kv("cache");
kv.set("k", "v", &SetOptions { ttl_ms: Some(60_000), ..Default::default() }).await?;
kv.get_string("k").await?;  kv.incr("hits", Some(1)).await?;  kv.scan("0", Some("user:*"), None).await?;
// hashes, lists, sets, sorted sets, streams, pub/sub, exec(["INCRBY", "n", "2"])

let out = fb.functions().invoke("hello", &json!({"name": "ada"})).await?;
fb.functions().deploy(DeployRequest { name: "hello".into(), ..Default::default() }, &wasm_bytes).await?;

let jobs = fb.jobs();
let job = jobs.enqueue(&NewJob { kind: "email".into(), idempotency_key: Some("welcome-1".into()), ..Default::default() }).await?;
let done = jobs.wait(&job.id, Duration::from_millis(500), Duration::from_secs(60)).await?;
// worker side: claim, heartbeat, complete, fail

let ent = fb.entitlements();
if ent.has(HasQuery::Feature("ai_calls".into()), None).await? {
    ent.with_reservation(&MeteredRequest::new("ai_calls", "req-42"), |_| async { Ok(()) }).await?;
}
ent.check_quota(&QuotaRequest::new("ai_calls")).await?;   // consume, reserve, commit, leases, usage
```

## Errors

Every failure is an `Error`: `Api(ApiError)` (server answered), `Network`, `InvalidResponse`,
`Realtime`, `Config`. For an API error `e.kind()` is one of `Validation` (400/422),
`Authentication` (401), `Permission` (403), `NotFound` (404), `Conflict` (409/412), `RateLimit` (429),
`QuotaExceeded` (429, a quota code), `Server` (5xx), `Other`; `e.code()` is the server's stable code
(`validation_failed`, ...), `e.request_id()` is for support, and `ApiError::field_errors()` lists
per-field validation errors. Helpers: `is_not_found()`, `is_conflict()`, `is_quota_exceeded()`, ...

## Retries

Retried with backoff (`RetryPolicy`: 3 retries, 250 ms base, 10 s cap, full jitter): `429` and `503`
for every method; `502`, `504` and network errors only for idempotent calls (`GET`, `HEAD`, `PUT`,
`DELETE`, and calls carrying an idempotency key such as `jobs().enqueue` with a key, `consume`,
`reserve`). `Retry-After` is honoured up to `max_retry_after` (default 60 s; longer fails at once). A `429`
caused by an exhausted **quota** is never retried.

## Generated models

`src/generated.rs` holds structs and enums for every schema of the OpenAPI document, produced by
[`scripts/generate_models.py`](scripts/generate_models.py) from [`openapi/openapi.json`](openapi/openapi.json) (a copy of the server's `/api/openapi.json`). The
client code, auth, retries, realtime and the ergonomic API are hand-written; the generated models are
the typed responses (`Job`, `ObjectInfo`, `Entitlements`, ...). Regenerate after the API changes:

```sh
python3 scripts/generate_models.py openapi/openapi.json src/generated.rs
git diff --exit-code src/generated.rs     # drift check
```

## Known gaps

* **Message queues and topics** (FB-63: publish, pull, ack) are not on the server yet, so the SDK has
  no such API. Job progress, checkpoints, attempts, results, job queues and workflows (FB-64) are covered
  by `jobs()`: `report_progress`, `checkpoint`, `watch`, `queues`, `create_workflow`, ...
* Two schemas share a name in the OpenAPI document (`NewCollection`, `DeleteResult`), so the generated
  model is the wrong one for those endpoints: collection definitions are passed as JSON and
  `storage().delete_object` returns a hand-written `DeleteOutcome`.
* No cookie sessions (a browser feature) and no OAuth redirect helper beyond `oauth_start` / `oauth_complete`.
* Not covered by helpers (use `fb.request`): policies, orgs and teams, plans and billing admin, vector,
  graph, SQL, credits, customization.

## Build and test

The crate is its own Cargo workspace (`sdk/rust/Cargo.toml`) so the server workspace does not pull the
HTTP client stack. From `sdk/rust`:

```sh
export CARGO_INCREMENTAL=0                      # smaller target/ on a shared disk
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test                                      # unit + scripted-server tests (hermetic)
FB_LIVE_BIN=/path/to/target/debug/forgebase cargo test   # + live tests against the real server binary
cargo deny --config ../../deny.toml check
```

Live tests (`tests/live_*.rs`) start the binary on a free port with a temporary data directory, sign in
as a bootstrapped administrator and exercise every area against real persistence. Without
`FB_LIVE_BIN` they print a notice and pass vacuously.

## Licence

MIT OR Apache-2.0, like the rest of ForgeBase.
