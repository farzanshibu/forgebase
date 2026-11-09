//! SPEC §27 scenario #18, "Execute a sandboxed server function", over the
//! real `/api/v1` router and middleware: an administrator deploys a module
//! through the REST API, sets a secret and grants the function's identity
//! data and KV permissions through the policy engine; an anonymous caller
//! invokes the public HTTP function, which writes a record as its own
//! identity, increments a KV counter, reads the secret and logs; the record
//! is persisted, the execution is in the log, and the OpenAPI document lists
//! the routes. Also: access modes, management authorisation, rollback and
//! error statuses over HTTP.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::middleware::{self, Next};
use common::{READ_INPUT, Wat, admin, constant, harness};
use forgebase_api::{AppState, RouterOptions};
use forgebase_core::DataLayout;
use forgebase_data::AccessContext;
use serde_json::{Value as Json, json};
use tower::ServiceExt;

/// The `/api/v1` application with the functions extension. A request with
/// `x-test-as: admin` or `x-test-as: user` gets that identity (as the auth
/// layer would); others are anonymous.
fn app(h: &common::H) -> Router {
    let layout = DataLayout::new(h.dir.path().join("data"));
    let state = AppState::from_data(h.db.clone(), layout, h.data.clone());
    let app = forgebase_api::router(
        state,
        RouterOptions {
            extensions: vec![forgebase_functions::extension(h.svc.clone())],
            ..RouterOptions::default()
        },
    );
    app.layer(middleware::from_fn(|mut req: Request<Body>, next: Next| async move {
        let who = req.headers().get("x-test-as").and_then(|v| v.to_str().ok()).map(str::to_owned);
        match who.as_deref() {
            Some("admin") => {
                req.extensions_mut().insert(admin());
            }
            Some("user") => {
                req.extensions_mut().insert(AccessContext::user("users", "u1"));
            }
            _ => {}
        }
        next.run(req).await
    }))
}

struct Res {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: Vec<u8>,
}

impl Res {
    fn json(&self) -> Json {
        serde_json::from_slice(&self.body).unwrap_or(Json::Null)
    }
}

async fn call(app: &Router, method: Method, uri: &str, who: Option<&str>, body: Option<Json>) -> Res {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(w) = who {
        req = req.header("x-test-as", w);
    }
    let body = match body {
        Some(b) => {
            req = req.header("content-type", "application/json");
            Body::from(b.to_string())
        }
        None => Body::empty(),
    };
    let res = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let body = axum::body::to_bytes(res.into_body(), 1 << 22).await.unwrap().to_vec();
    Res { status, headers, body }
}

/// The scenario's guest: stores its whole HTTP input as a `notes`
/// document, increments the KV counter `calls`, reads the secret `GREETING`,
/// logs, and answers `201 {"note": <record>, "kv": <counter>}` with the
/// secret in `x-greeting`.
fn notes_module() -> String {
    let mut w = Wat::new(&["db_create", "kv", "secret_get", "log"]);
    let pre = w.s(r#"{"collection":"notes","data":"#);
    let close = w.s("}");
    let err_pre = w.s(r#"{"status":403,"body":"#);
    let incr = w.s(r#"{"op":"incr","key":"calls"}"#);
    let secret = w.s("GREETING");
    let msg = w.s("note created");
    let o1 = w.s(r#"{"status":201,"headers":{"x-greeting":""#);
    let o2 = w.s(r#""},"body":{"note":"#);
    let o3 = w.s(r#","kv":"#);
    let o4 = w.s("}}");
    let (args, res, aux, out) = (common::ARGS, common::RES, common::AUX, common::OUT);
    let kvres = 0x58000;
    w.wasm(&format!(
        "{READ_INPUT}
         (local.set $p (call $copy (i32.const {args}) {pre}))
         (local.set $p (call $copy (local.get $p) (i32.const 65536) (local.get $n)))
         (local.set $p (call $copy (local.get $p) {close}))
         (local.set $r (call $db_create (i32.const {args}) (i32.sub (local.get $p) (i32.const {args}))))
         (if (i32.lt_s (local.get $r) (i32.const 0)) (then
           (local.set $r (i32.sub (i32.const 0) (local.get $r)))
           (drop (call $result_read (i32.const {res}) (local.get $r)))
           (local.set $p (call $copy (i32.const {out}) {err_pre}))
           (local.set $p (call $copy (local.get $p) (i32.const {res}) (local.get $r)))
           (local.set $p (call $copy (local.get $p) {close}))
           (call $emit (i32.const {out}) (i32.sub (local.get $p) (i32.const {out})))
           (return (i32.const 0))))
         (drop (call $result_read (i32.const {res}) (local.get $r)))
         (local.set $k (local.get $r))
         (local.set $q (call $kv {incr}))
         (if (i32.lt_s (local.get $q) (i32.const 0)) (then (return (i32.const 7))))
         (drop (call $result_read (i32.const {kvres}) (local.get $q)))
         (local.set $r (call $secret_get {secret}))
         (if (i32.lt_s (local.get $r) (i32.const 0)) (then (return (i32.const 8))))
         (drop (call $result_read (i32.const {aux}) (local.get $r)))
         (drop (call $log (i32.const 1) {msg}))
         (local.set $p (call $copy (i32.const {out}) {o1}))
         (local.set $p (call $copy (local.get $p) (i32.const {aux}) (local.get $r)))
         (local.set $p (call $copy (local.get $p) {o2}))
         (local.set $p (call $copy (local.get $p) (i32.const {res}) (local.get $k)))
         (local.set $p (call $copy (local.get $p) {o3}))
         (local.set $p (call $copy (local.get $p) (i32.const {kvres}) (local.get $q)))
         (local.set $p (call $copy (local.get $p) {o4}))
         (call $emit (i32.const {out}) (i32.sub (local.get $p) (i32.const {out})))
         (i32.const 0)"
    ))
}

#[tokio::test]
async fn scenario_18_execute_a_sandboxed_server_function() {
    let h = harness().await;
    h.collection(json!({"name": "notes", "schema_mode": "flexible", "fields": [{"name": "trigger", "type": "text"}]}))
        .await;
    let app = app(&h);

    // Deploy over REST as an administrator.
    let deploy = json!({
        "name": "notes",
        "wasm": notes_module(),
        "description": "stores notes",
        "access": "public",
        "capabilities": ["db.write", "kv", "secrets", "log"],
        "limits": {"fuel": 50_000_000, "memory_bytes": 4_194_304, "timeout_ms": 2_000},
    });
    let r = call(&app, Method::POST, "/api/v1/functions", None, Some(deploy.clone())).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "anonymous cannot deploy");
    let r = call(&app, Method::POST, "/api/v1/functions", Some("user"), Some(deploy.clone())).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "users cannot deploy");
    let r = call(&app, Method::POST, "/api/v1/functions", Some("admin"), Some(deploy)).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", String::from_utf8_lossy(&r.body));
    let d = r.json();
    assert_eq!(d["version"]["version"], 1);
    assert_eq!(d["function"]["principal"], "service:function.notes");
    assert_eq!(d["version"]["limits"]["fuel"], 50_000_000);

    let r = call(
        &app,
        Method::PUT,
        "/api/v1/functions/notes/secrets/GREETING",
        Some("admin"),
        Some(json!({"value": "hi-from-a-secret"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", String::from_utf8_lossy(&r.body));

    // Without permissions the function's identity is denied by the policy engine.
    let r = call(
        &app,
        Method::POST,
        "/api/v1/functions/notes/invoke?x=1",
        None,
        Some(json!({"title": "hello"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", String::from_utf8_lossy(&r.body));
    assert_eq!(r.json()["body"]["code"], "forbidden");

    h.grant("notes", "data:notes:write").await;
    h.grant("notes", "data:notes:read").await;
    h.grant("notes", "engine:kv_read").await;
    h.grant("notes", "engine:kv_write").await;

    // Anonymous invocation of the public HTTP function.
    let mut req = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/functions/notes/invoke?x=1")
        .header("content-type", "application/json")
        .header("authorization", "Bearer not-for-functions");
    req = req.header("x-trace", "abc");
    let res = app
        .clone()
        .oneshot(req.body(Body::from(json!({"title": "hello"}).to_string())).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    assert_eq!(res.headers().get("x-greeting").unwrap(), "hi-from-a-secret");
    assert_eq!(res.headers().get("content-type").unwrap(), "application/json");
    let body: Json = serde_json::from_slice(&axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap()).unwrap();
    assert_eq!(body["kv"]["value"], 1);
    let note = &body["note"];
    assert_eq!(note["trigger"], "http");
    assert_eq!(note["method"], "POST");
    assert_eq!(note["body"]["title"], "hello");
    assert_eq!(note["query"]["x"], "1");
    assert_eq!(note["headers"]["x-trace"], "abc");
    assert!(note["headers"].get("authorization").is_none(), "credentials are not passed to functions");
    assert_eq!(note["auth"]["type"], "anonymous");

    // Persisted: readable through the data core.
    let id = note["id"].as_str().unwrap();
    let stored = h.data.get_record(&admin(), "notes", id, &Default::default()).await.unwrap();
    assert_eq!(stored["body"]["title"], "hello");

    // Second call: the KV counter persisted between executions.
    let r = call(&app, Method::POST, "/api/v1/functions/notes/invoke", None, Some(json!({"title": "again"}))).await;
    assert_eq!(r.status, StatusCode::CREATED);
    assert_eq!(r.json()["kv"]["value"], 2);
    assert_eq!(h.data.list_records(&admin(), "notes", &Default::default()).await.unwrap().items.len(), 2);

    // Execution log with the function's log line.
    let r = call(&app, Method::GET, "/api/v1/functions/notes/logs", Some("admin"), None).await;
    assert_eq!(r.status, StatusCode::OK);
    let items = r.json()["items"].as_array().unwrap().clone();
    assert_eq!(items.len(), 3);
    assert_eq!(items[0]["status"], "ok");
    assert_eq!(items[0]["trigger"], "http");
    assert_eq!(items[0]["logs"][0]["message"], "note created");
    assert!(items[0]["fuel_used"].as_i64().unwrap() > 0);
    let r = call(&app, Method::GET, "/api/v1/functions/notes/logs", None, None).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "logs are not public");

    // Secrets are listed without values.
    let r = call(&app, Method::GET, "/api/v1/functions/notes/secrets", Some("admin"), None).await;
    assert_eq!(r.json()["items"][0]["name"], "GREETING");
    assert!(!String::from_utf8_lossy(&r.body).contains("hi-from"));

    // Documented.
    let r = call(&app, Method::GET, "/api/openapi.json", None, None).await;
    let doc = String::from_utf8_lossy(&r.body).to_string();
    for p in [
        "/api/v1/functions",
        "/api/v1/functions/{name}/invoke",
        "/api/v1/functions/{name}/rollback",
        "/api/v1/functions/{name}/secrets/{key}",
    ] {
        assert!(doc.contains(p), "{p} not documented");
    }
}

#[tokio::test]
async fn access_modes_rollback_and_error_statuses_over_http() {
    let h = harness().await;
    let app = app(&h);
    let deploy = |name: &str, wasm: String, access: &str| {
        json!({"name": name, "wasm": wasm, "access": access, "limits": {"fuel": 3_000_000, "timeout_ms": 1_000}})
    };
    for (name, access) in [("open", "public"), ("members", "authenticated"), ("staff", "permission")] {
        let body = deploy(name, constant(r#"{"body":"v1"}"#), access);
        assert_eq!(
            call(&app, Method::POST, "/api/v1/functions", Some("admin"), Some(body)).await.status,
            StatusCode::CREATED
        );
    }
    let get = |name: &str, who: Option<&'static str>| {
        let app = app.clone();
        let uri = format!("/api/v1/functions/{name}/invoke");
        async move { call(&app, Method::GET, &uri, who, None).await }
    };
    let r = get("open", None).await;
    assert_eq!((r.status, r.body.as_slice()), (StatusCode::OK, b"v1".as_slice()));
    assert_eq!(r.headers.get("content-type").unwrap(), "text/plain; charset=utf-8");
    assert_eq!(get("members", None).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(get("members", Some("user")).await.status, StatusCode::OK);
    assert_eq!(get("staff", Some("user")).await.status, StatusCode::FORBIDDEN);
    assert_eq!(get("staff", Some("admin")).await.status, StatusCode::OK);
    assert_eq!(get("missing", None).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(get("missing", Some("user")).await.status, StatusCode::NOT_FOUND);

    // v2 spins: fuel exhaustion is a 502 with its code; rollback over REST restores v1.
    let w = Wat::new(&[]);
    let body = deploy("open", w.wasm("(loop $l (br $l)) (i32.const 0)"), "public");
    assert_eq!(
        call(&app, Method::POST, "/api/v1/functions", Some("admin"), Some(body)).await.status,
        StatusCode::CREATED
    );
    let r = get("open", None).await;
    assert_eq!(r.status, StatusCode::BAD_GATEWAY);
    assert_eq!(r.json()["error"]["code"], "function_fuel_exhausted", "{}", String::from_utf8_lossy(&r.body));
    let r = call(&app, Method::POST, "/api/v1/functions/open/rollback", Some("user"), None).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = call(&app, Method::POST, "/api/v1/functions/open/rollback", Some("admin"), None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", String::from_utf8_lossy(&r.body));
    assert_eq!(r.json()["active_version"], 1);
    assert_eq!(get("open", None).await.body, b"v1");
    let r = call(&app, Method::GET, "/api/v1/functions/open/versions", Some("admin"), None).await;
    assert_eq!(r.json()["items"].as_array().unwrap().len(), 2);

    // Timeout is a 504; invalid output a 502.
    let w = Wat::new(&[]);
    let body = json!({"name": "slow", "wasm": w.wasm("(loop $l (br $l)) (i32.const 0)"), "access": "public",
        "limits": {"fuel": 20_000_000_000u64, "timeout_ms": 100}});
    call(&app, Method::POST, "/api/v1/functions", Some("admin"), Some(body)).await;
    let r = get("slow", None).await;
    assert_eq!(r.status, StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(r.json()["error"]["code"], "function_timeout");
    let body = deploy("weird", constant("not json"), "public");
    call(&app, Method::POST, "/api/v1/functions", Some("admin"), Some(body)).await;
    let r = get("weird", None).await;
    assert_eq!(r.status, StatusCode::BAD_GATEWAY);
    assert_eq!(r.json()["error"]["code"], "function_bad_output");

    // Invalid deploys are 400 with their code.
    let r = call(
        &app,
        Method::POST,
        "/api/v1/functions",
        Some("admin"),
        Some(json!({"name": "bad", "wasm": "AGFzbQEAAAA="})),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.json()["error"]["code"], "invalid_module");

    // Delete.
    assert_eq!(
        call(&app, Method::DELETE, "/api/v1/functions/weird", Some("admin"), None).await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(get("weird", Some("admin")).await.status, StatusCode::NOT_FOUND);
    let list = call(&app, Method::GET, "/api/v1/functions", Some("admin"), None).await.json();
    assert_eq!(list["items"].as_array().unwrap().len(), 4);
}

#[tokio::test]
async fn http_input_carries_method_and_raw_bodies() {
    let h = harness().await;
    let app = app(&h);
    // Echo: answers {"body": <its whole input>}.
    let mut w = Wat::new(&[]);
    let pre = w.s(r#"{"body":"#);
    let close = w.s("}");
    let out = common::OUT;
    let body = format!(
        "{READ_INPUT}
         (local.set $p (call $copy (i32.const {out}) {pre}))
         (local.set $p (call $copy (local.get $p) (i32.const 65536) (local.get $n)))
         (local.set $p (call $copy (local.get $p) {close}))
         (call $emit (i32.const {out}) (i32.sub (local.get $p) (i32.const {out})))
         (i32.const 0)"
    );
    let d = json!({"name": "echo", "wasm": w.wasm(&body), "access": "public"});
    call(&app, Method::POST, "/api/v1/functions", Some("admin"), Some(d)).await;
    let req = Request::builder()
        .method(Method::PUT)
        .uri("/api/v1/functions/echo/invoke")
        .header("content-type", "text/plain")
        .header("cookie", "session=secret")
        .body(Body::from("plain text"))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v: Json = serde_json::from_slice(&axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap()).unwrap();
    assert_eq!(v["method"], "PUT");
    assert_eq!(v["body"], "plain text");
    assert!(v["headers"].get("cookie").is_none());
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/functions/echo/invoke")
        .body(Body::from(vec![0xff, 0xfe, 0x00]))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let v: Json = serde_json::from_slice(&axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap()).unwrap();
    assert_eq!(v["body_base64"], "//4A");
    // Bodies above 1 MiB are refused before the function runs.
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/functions/echo/invoke")
        .body(Body::from(vec![b'a'; 2 * 1024 * 1024]))
        .unwrap();
    assert_eq!(app.clone().oneshot(req).await.unwrap().status(), StatusCode::PAYLOAD_TOO_LARGE);
}
