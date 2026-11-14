# forgebase-sdk (Rust)

Async Rust client for [ForgeBase](../../README.md): authentication, collections and records,
realtime subscriptions (WebSocket with SSE fallback), object storage, key-value, server functions,
jobs, queues with webhook tooling, billing and entitlements (with their admin sides), credits,
vector search, the property graph with openCypher, the SQL editor, indexes, documents, access
control, organizations, identity administration, customization, alerts and SLOs, operations
insights, instance administration and webhook signature verification. It covers the same surface
as the TypeScript SDK in [`sdk/js`](../js) and adds the roadmap APIs (webhook tooling, billing
webhook inspector, alerts, insights, mail providers and login presets).

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
# forgebase-sdk = { git = "https://github.com/farzanshibu/forgebase", package = "forgebase-sdk" }
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

More in [`examples/`](examples): `quickstart`, `realtime`, `storage`, `kv_jobs_entitlements`,
`vector_graph_sql`, `webhook_verify`. They read `FORGEBASE_URL` and `FORGEBASE_API_KEY` (or email
and password) from the environment.

## Modules

Every accessor returns a cheap handle (`Clone + Send + Sync`); methods are `async` and return
`Result<T, Error>`. Request and response bodies are typed: the structs of [`generated`](src/generated.rs)
where the OpenAPI document describes them, hand-written structs (with builder helpers) elsewhere.
Optional fields are `Option`s and stay off the wire when unset.

| Accessor | Module | Covers |
|---|---|---|
| `auth()` | `auth` | sign-up and sign-in flows, MFA, tokens, sessions, OAuth |
| `collections()`, `collection(name)` | `records` | collection definitions, records (typed or JSON), import / export |
| `documents()` | `documents` | the record operations of document collections, `If-Match`, versions |
| `storage()`, `kv(ns)`, `functions()`, `jobs()`, `realtime()` | `storage`, `kv`, `functions`, `jobs`, `realtime` | objects, key-value, server functions, background jobs, subscriptions |
| `queues()` | `queues`, `queue_webhooks` | topics, publish, pull, ack, replay, streaming pull; **webhook tooling**: attempts, test send, redrive, retry, secret rotation, pause / resume, event catalog |
| `billing()` | `billing`, `billing_admin` | store receipts; customers, portal, subscriptions, orders, invoices, credit notes, catalog (products, prices, coupons, tax rates), providers, settings, dunning, stats, usage meters, maintenance; **webhook inspector**: list, get, replay, reverify |
| `entitlements()` | `entitlements`, `entitlements_admin` | `has`, quotas, reservations, leases; plans, features, add-ons, usage packs, quota policies, overrides, enterprise limits, trials, gauges, usage events and alerts |
| `credits()` | `credits` | balance, lots, ledger, top-up, grant, spend, refund, reversal, rates, stats |
| `vector()` | `vector` | collections and fields, upsert, similarity and hybrid search, recall, rebuild |
| `graph()` | `graph` | nodes, edges, neighbours, traversal, shortest path, read-only openCypher (`engine`: `age` or `portable`) |
| `sql()`, `indexes()` | `sql`, `indexes` | run, batch, explain, tables; indexes, planner statistics, slow-query log |
| `access()` | `access` | permission checks, ACLs, per-principal overrides, collection rules, engine resource rules |
| `orgs()` | `orgs` | organizations, projects and environments, members, invitations, teams, roles, role sets, realm bindings, suspensions |
| `identity()` | `identity` | API keys, service accounts, OAuth clients, realms, login providers (presets), SMS gateways, users, impersonation, audit, passkeys, phone, LDAP |
| `customize()` | `customize` | branding, custom domains, email templates, **mail provider and test send**, agreements, hosted views |
| `system()` | `system` | info, **capabilities**, features and config, backups, database logins, extensions, topology |
| `alerts()`, `slos()` | `alerts` | **rules, alerts, history, silences, notifiers and test send, notifications; SLOs and status** |
| `insights()` | `insights` | **statements, regressions, slow queries, usage, functions, invocations, audit search and streamed CSV / JSONL export** |
| `webhooks::verify`, `construct_event` | `webhooks` | verify `X-Forgebase-Signature` and parse the push envelope |

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

## Vector, graph and SQL

```rust
use forgebase_sdk::generated::Metric;
use forgebase_sdk::vector::{FieldSpec, HybridOptions, NewVectorCollection, SearchOptions, UpsertItem};

let vector = fb.vector();
vector.create(&NewVectorCollection::new("docs").field(FieldSpec::new("emb", 3, Metric::Cosine))).await?;
let docs = vector.collection("docs");
docs.upsert(&[UpsertItem::new().id("a").vector("emb", &[1.0, 0.0, 0.0]).metadata(json!({"year": 2020})).text("hello")]).await?;
let hits = docs.search(&SearchOptions::new(&[1.0, 0.0, 0.0]).k(5).filter("year >= {:min}", Some(json!({"min": 2019})))).await?;
let fused = docs.hybrid(&HybridOptions::new("hello world", &[1.0, 0.0, 0.0]).k(5)).await?; // BM25 + vector, RRF

use forgebase_sdk::graph::{CypherOptions, NewEdge, NewNode};
let g = fb.graph();
g.nodes().create(&NewNode::new("Person", json!({"name": "Ada"})).id("ada")).await?;
g.nodes().create(&NewNode::new("Person", json!({"name": "Bob"})).id("bob")).await?;
g.edges().create(&NewEdge::new("KNOWS", "ada", "bob")).await?;
let r = g.cypher("MATCH (a:Person)-[:KNOWS]->(b) RETURN a.name, b.name", &CypherOptions::default()).await?;
println!("{} rows from the {} engine", r.rows.len(), r.engine);   // "age" or "portable"

use forgebase_sdk::sql::SqlOptions;
let rows = fb.sql().query("SELECT id, title FROM posts WHERE views > ?", &[json!(10)], &SqlOptions::default()).await?;
let plan = fb.sql().explain("SELECT * FROM posts WHERE id = ?", &[json!(1)]).await?;
```

`fb.system().capabilities()` says which engine runs each feature (`opencypher` is `portable` on
SQLite and on PostgreSQL without Apache AGE). Only the first openCypher query of a tenant on AGE can
answer `409 graph_materialising`; retry after the job finishes.

## Webhooks

Verify a push delivery in your handler: pass the **raw** body bytes and the header value.

```rust
use forgebase_sdk::webhooks::{self, DEFAULT_TOLERANCE};

// axum: async fn hook(headers: HeaderMap, body: Bytes) -> StatusCode
let sig = headers.get(webhooks::SIGNATURE_HEADER).and_then(|v| v.to_str().ok());
match webhooks::construct_event::<serde_json::Value>(&secret, sig, &body, DEFAULT_TOLERANCE) {
    Ok(event) => { /* event.message.data, event.delivery_attempt */ StatusCode::OK }
    Err(e) => { eprintln!("{e}"); StatusCode::BAD_REQUEST }   // e.reason: signature_mismatch, ...
}
```

`verify(secret, header, body, tolerance)` checks only the signature and the timestamp. While a secret
rotates the header carries several `v1=` values (`t=<ts>,v1=<new>,v1=<old>`) and a request passes when
any of them matches; `verify_any` / `construct_event_any` accept a list of secrets on the receiving
side. Comparison is constant time, the timestamp must be within `tolerance` (five minutes by
default) of the clock, and the body is parsed only after the signature checks out. `sign` builds a
header for tests of your own handler.

Operate a push subscription with `fb.queues().webhooks("orders-push")`:

```rust
use forgebase_sdk::queue_webhooks::{AttemptsQuery, Redrive};
use forgebase_sdk::queues::PushTarget;

let q = fb.queues();
let mut sub = NewSubscription::new("orders-push", "orders");
sub.push = Some(PushTarget::webhook("https://hooks.example.com/orders"));
let secret = q.create_subscription(&sub).await?.push_secret;   // shown once

let w = q.webhooks("orders-push");
let attempt = w.test(None).await?;                              // synthetic signed event, not a message
let log = w.attempts(&AttemptsQuery::default()).await?;         // status, latency, error class, headers, body
w.redrive(&Redrive::all()).await?;                              // dead letters -> delivered again, once each
let rotated = w.rotate_secret(Some(3_600_000)).await?;          // new secret once; old one valid for an hour
w.pause(Some("maintenance")).await?;  w.resume().await?;
let catalog = q.catalog().await?;                               // topics with description, schema, example
```

Billing provider webhooks are inspected with `fb.billing().webhook_deliveries()`: `list`, `get`,
`replay` (process a verified delivery again) and `reverify` (verify a refused one against the current
secrets).

## Alerts, SLOs and insights

```rust
let alerts = fb.alerts();
let rule: AlertRuleInput = serde_json::from_value(json!({
    "name": "slow-api", "expr": "http_request_p95_seconds", "op": ">", "threshold": 1.0, "for": "5m", "severity": "warning"}))?;
let rule = alerts.rules().create(&rule).await?;
alerts.rules().update(&rule.id, &AlertRulePatch { enabled: Some(false), ..Default::default() }).await?;
let open = alerts.active(Some("firing"), None).await?;
let history = alerts.history(&AlertHistoryQuery { limit: Some(50), ..Default::default() }).await?;
alerts.notifiers().test("ops-slack").await?;                    // sends a test notification now
let status = fb.slos().status("api-availability").await?;       // SLI, error budget, burn rates

use forgebase_sdk::insights::{AuditQuery, ExportFormat, TimeRange};
let i = fb.insights();
let status = i.status().await?;
let regressions = i.regressions(&Default::default()).await?;
let mut csv = i.export_audit(&AuditQuery { range: TimeRange::new("2026-01-01", "2026-02-01"), ..Default::default() },
                             ExportFormat::Csv, Some(100_000)).await?;
while let Some(line) = csv.line().await? { /* streamed: nothing is buffered */ }
```

## Administration

```rust
// Access: roles, rules, ACLs, overrides
fb.access().policies().put("posts", &CollectionPolicy { list_rule: Some(String::new()), ..Default::default() }).await?; // "" = public
fb.access().acl().put(&NewAcl::new("record", "posts/1", "team:t1", "records:update", Effect::Allow)).await?;
let me = fb.access().me(None).await?;   let ok = fb.access().can("records:create").await?;

// Organizations
let org = fb.orgs().create(&NewOrg::new("acme", "Acme")).await?;
fb.orgs().members().set(&org.id, &MemberInput::new("user:users/u1", &["editor"])).await?;
let invite = fb.orgs().invitations().create(&org.id, "ada@example.com", &["viewer"]).await?;   // invite.token: once

// Identity: realms, login provider presets, OAuth clients, service accounts and keys
fb.identity().realms().providers()
    .put("default", "github", &RealmProviderConfig::preset(ProviderPreset::Github, "client-id", "client-secret")).await?;
let client = fb.identity().clients().create(&ClientInput::new("Web").redirect_uri("https://app/cb")).await?; // client_secret: once
let sa = fb.identity().service_accounts().create(&NewServiceAccount::new("ci")).await?;
let key = fb.identity().service_accounts().keys().create(&sa.id, &NewApiKey::new("deploy", &["records:read"])).await?;

// Billing and entitlements administration
fb.billing().catalog().products().put("pro", &product).await?;
fb.billing().subscriptions().create(&subscribe_input).await?;
fb.entitlements().plans().put("pro", &plan).await?;   fb.credits().top_up(&CreditInput::new("org:1", 100, "k1")).await?;

// Customization: branding and the mail provider, with a test send
fb.customize().email().put_provider(&MailProviderConfig::new("postmark").set("server_token", "..."), None).await?;
fb.customize().email().test_provider("me@example.com", None).await?;   // 409 mail_test_failed carries the reason

// Instance: capabilities, backups
let caps = fb.system().capabilities().await?;
let snap = fb.system().backups().create(&Default::default()).await?;
```

Secrets (`client_secret`, API keys, signing secrets, provider credentials) are write-only: the server
returns them once, on creation, and masks them afterwards.

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

`openapi/openapi.json` is `GET /api/openapi.json` of a running server, written as sorted, compact JSON
with a trailing newline (the same bytes as `sdk/js/openapi/openapi.json`):

```sh
curl -s http://127.0.0.1:8090/api/openapi.json |
  python3 -c 'import json,sys; sys.stdout.write(json.dumps(json.load(sys.stdin), separators=(",", ":"), sort_keys=True, ensure_ascii=False) + "\n")' \
  > openapi/openapi.json
```

Where the server's schema is narrower than what it sends, `TYPE_OVERRIDES` in the script widens one
field (today `SloStatus.burn_rates`, whose values are `null` without traffic).

## Known gaps

* Message queues and topics (FB-63) are covered by `queues()` (see Queues above). Job progress,
  checkpoints, attempts, results, job queues and workflows (FB-64) are covered by `jobs()`:
  `report_progress`, `checkpoint`, `watch`, `queues`, `create_workflow`, ...
* Collection definitions are passed as JSON: the OpenAPI request schema is the full `CollectionSchema`
  (with server-assigned `id`, `version`, ...). `storage().delete_object` returns the generated `DeleteResult`.
* No cookie sessions (a browser feature) and no OAuth redirect helper beyond `oauth_start` / `oauth_complete`.
* The identity browser ceremonies stay with the caller: passkey calls take and return the JSON the
  platform credential API produces, and OIDC protocol endpoints (authorize, token, ...) are left to OIDC
  libraries; `identity().oidc()` serves the issuer and discovery URLs.
* Types the OpenAPI document leaves as plain objects (access, orgs and identity bodies, mail provider state)
  are written by hand after the server's JSON; compare them with `/api/openapi.json` after a server upgrade.
* Database topology agent calls (`/api/system/database/agent/*`, used by `forgebase node join`) are not wrapped.
* The audit export streams the response body; a CSV record with an embedded line break spans several
  `line()` results (use `chunk()` and a CSV parser for general CSV).
* Credits, billing and entitlements times are RFC 3339 strings; there is no `chrono` / `time` dependency.

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

Tests: `tests/mock_*.rs` run every module against a scripted HTTP server (`tests/common/mock.rs`) and
check the request each call sends (method, path, query, body) and the response it decodes; response
bodies of generated models are built from the OpenAPI document (`tests/common/sample.rs`). `tests/webhooks.rs`
runs the vectors of `sdk/js/test/webhooks.test.ts`. The live tests (`live_*.rs`) start the real server binary
(`cargo build -p forgebase-cli` in the repository root, then `FB_LIVE_BIN=$PWD/../../target/debug/forgebase`);
`live_admin.rs` covers vector, graph and openCypher, SQL, webhook tooling, the billing webhook inspector,
alerts and SLOs, insights and capabilities, and `live_platform.rs` identity, customization, billing and
entitlements admin, backups, indexes and documents. Without `FB_LIVE_BIN` they pass vacuously.

Dependencies beyond HTTP and TLS: `hmac` and `sha2` (RustCrypto, MIT OR Apache-2.0) for webhook
signatures.

Live tests (`tests/live_*.rs`) start the binary on a free port with a temporary data directory, sign in
as a bootstrapped administrator and exercise every area against real persistence. Without
`FB_LIVE_BIN` they print a notice and pass vacuously.

## Licence

MIT OR Apache-2.0, like the rest of ForgeBase.
