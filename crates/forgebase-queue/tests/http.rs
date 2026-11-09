//! The HTTP surface: REST round trip, OpenAPI, WebSocket and SSE streaming
//! pull with flow control, and push delivery to signed webhooks (with retries
//! and SSRF protection) and to server functions. Streaming and webhook tests
//! bind `127.0.0.1:0`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::{BTreeSet, HashMap};
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::Request;
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use axum::{Router, routing::post};
use base64::Engine as _;
use common::{H, admin, harness, harness_with, settings, user};
use forgebase_api::{AppState, RouterOptions};
use forgebase_core::{DataLayout, ErrorCode};
use forgebase_db::BoxFuture;
use forgebase_functions::fetch::{Connector, Resolver};
use forgebase_functions::{DeployRequest, FetchConfig, Fetcher, FunctionService, FunctionsConfig, LimitsInput};
use forgebase_queue::{
    NewMessage, NewSubscription, NewTopic, PullRequest, PushConfig, PushKind, QueueSettings, push,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value as Json, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tower::ServiceExt;

/// The API router with the queue extension; `x-test-as: admin | <user id>`
/// sets the caller.
fn app(h: &H) -> Router {
    let layout = DataLayout::new(h.dir.path().join("data"));
    let state = AppState::from_data(h.db.clone(), layout, h.data.clone());
    forgebase_api::router(
        state,
        RouterOptions {
            extensions: vec![forgebase_queue::extension(h.svc.clone())],
            ..RouterOptions::default()
        },
    )
    .layer(axum::middleware::from_fn(as_caller))
}

async fn as_caller(mut req: Request, next: Next) -> Response {
    if let Some(v) = req.headers().get("x-test-as").and_then(|v| v.to_str().ok()) {
        let ctx = if v == "admin" { admin() } else { user(v) };
        req.extensions_mut().insert(ctx);
    }
    next.run(req).await
}

async fn call(app: &Router, who: Option<&str>, method: Method, uri: &str, body: Option<Json>) -> (StatusCode, Json) {
    let mut b = axum::http::Request::builder().method(method).uri(uri);
    if let Some(w) = who {
        b = b.header("x-test-as", w);
    }
    let req = match body {
        Some(j) => b
            .header("content-type", "application/json")
            .body(Body::from(j.to_string()))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    };
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 24).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Json::Null))
}

async fn serve(app: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
            .await
            .unwrap();
    });
    addr
}

#[tokio::test]
async fn rest_round_trip() {
    let h = harness().await;
    let app = app(&h);
    let a = Some("admin");
    let (s, _) = call(&app, None, Method::GET, "/api/v1/queues/topics", None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _) = call(&app, Some("nobody"), Method::POST, "/api/v1/queues/topics", Some(json!({"bad": 1}))).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "authorised before the body is looked at");

    let (s, t) = call(&app, a, Method::POST, "/api/v1/queues/topics", Some(json!({"name": "orders", "max_message_bytes": 128}))).await;
    assert_eq!(s, StatusCode::CREATED, "{t}");
    let (s, _) = call(&app, a, Method::POST, "/api/v1/queues/topics", Some(json!({"name": "orders"}))).await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, sub) = call(
        &app,
        a,
        Method::POST,
        "/api/v1/queues/subscriptions",
        Some(json!({"name": "billing", "topic": "orders", "ack_deadline_ms": 30000})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{sub}");
    assert!(sub.get("push_secret").is_none());

    let (s, p) = call(
        &app,
        a,
        Method::POST,
        "/api/v1/queues/topics/orders/publish",
        Some(json!({"messages": [
            {"data": {"id": 1}, "attributes": {"type": "new"}, "ordering_key": "c1"},
            {"data": {"id": 2}, "dedup_id": "two"},
            {"data": {"id": 2}, "dedup_id": "two"}
        ]})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{p}");
    assert_eq!(p["results"].as_array().unwrap().len(), 3);
    assert_eq!(p["results"][2]["duplicate"], true);
    let (s, e) = call(
        &app,
        a,
        Method::POST,
        "/api/v1/queues/topics/orders/publish",
        Some(json!({"messages": [{"data": "x".repeat(200)}]})),
    )
    .await;
    assert_eq!((s, e["error"]["code"].as_str()), (StatusCode::PAYLOAD_TOO_LARGE, Some("message_too_large")));

    let (s, pulled) = call(
        &app,
        a,
        Method::POST,
        "/api/v1/queues/subscriptions/billing/pull",
        Some(json!({"max_messages": 10})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let msgs = pulled["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[0]["message"]["attributes"]["type"], "new");
    assert_eq!(msgs[0]["delivery_attempt"], 1);
    let ack_ids: Vec<Json> = msgs.iter().map(|m| m["ack_id"].clone()).collect();
    let (s, r) = call(
        &app,
        a,
        Method::POST,
        "/api/v1/queues/subscriptions/billing/modify-deadline",
        Some(json!({"ack_ids": [ack_ids[0]], "ack_deadline_ms": 60000})),
    )
    .await;
    assert_eq!((s, r["count"].as_i64()), (StatusCode::OK, Some(1)));
    let (s, r) = call(
        &app,
        a,
        Method::POST,
        "/api/v1/queues/subscriptions/billing/ack",
        Some(json!({"ack_ids": ack_ids})),
    )
    .await;
    assert_eq!((s, r["count"].as_i64()), (StatusCode::OK, Some(2)));

    let (s, m) = call(&app, a, Method::GET, "/api/v1/queues/subscriptions/billing/metrics", None).await;
    assert_eq!((s, m["acked_total"].as_i64(), m["backlog_messages"].as_i64()), (StatusCode::OK, Some(2), Some(0)));
    let (s, m) = call(&app, a, Method::GET, "/api/v1/queues/topics/orders/metrics", None).await;
    assert_eq!((s, m["published_total"].as_i64()), (StatusCode::OK, Some(2)));
    let (s, page) = call(&app, a, Method::GET, "/api/v1/queues/topics/orders/messages?limit=1", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    assert!(page["next_after"].is_i64());
    let (s, list) = call(&app, a, Method::GET, "/api/v1/queues/subscriptions?topic=orders", None).await;
    assert_eq!((s, list["items"].as_array().unwrap().len()), (StatusCode::OK, 1));

    let (s, snap) = call(
        &app,
        a,
        Method::POST,
        "/api/v1/queues/snapshots",
        Some(json!({"name": "s1", "subscription": "billing"})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{snap}");
    let (s, r) = call(
        &app,
        a,
        Method::POST,
        "/api/v1/queues/subscriptions/billing/seek",
        Some(json!({"time": "2000-01-01T00:00:00Z"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{r}");
    let (s, _) = call(&app, a, Method::POST, "/api/v1/queues/subscriptions/billing/seek", Some(json!({}))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = call(&app, a, Method::DELETE, "/api/v1/queues/topics/orders", None).await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _) = call(&app, a, Method::DELETE, "/api/v1/queues/topics/orders?force=true", None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = call(&app, a, Method::GET, "/api/v1/queues/subscriptions/billing", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[test]
fn openapi_operation_ids_are_unique_and_prefixed() {
    let doc = serde_json::to_value(forgebase_queue::openapi()).unwrap();
    let mut ids = BTreeSet::new();
    let mut n = 0;
    for (path, item) in doc["paths"].as_object().unwrap() {
        assert!(path.starts_with("/api/v1/queues/"), "{path}");
        for (_, op) in item.as_object().unwrap() {
            let Some(id) = op.get("operationId").and_then(Json::as_str) else { continue };
            assert!(id.starts_with("queues_"), "{id}");
            assert!(ids.insert(id.to_owned()), "duplicate {id}");
            n += 1;
        }
    }
    assert_eq!(n, 26);
}

async fn next_frame(ws: &mut (impl StreamExt<Item = Result<WsMessage, tokio_tungstenite::tungstenite::Error>> + Unpin)) -> Json {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("frame in time")
            .unwrap()
            .unwrap();
        if let WsMessage::Text(t) = msg {
            return serde_json::from_str(&t).unwrap();
        }
    }
}

#[tokio::test]
async fn websocket_streaming_pull_honours_flow_control() {
    let h = harness().await;
    h.topic("t").await;
    h.sub("s", "t").await;
    for i in 1..=5 {
        h.publish("t", json!(i)).await;
    }
    let addr = serve(app(&h)).await;
    let mut req = tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(format!(
        "ws://{addr}/api/v1/queues/subscriptions/s/stream?max_messages=2"
    ))
    .unwrap();
    req.headers_mut().insert("x-test-as", "admin".parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();

    let f = next_frame(&mut ws).await;
    assert_eq!(f["type"], "messages");
    let first: Vec<Json> = f["messages"].as_array().unwrap().clone();
    assert_eq!(first.iter().map(|m| m["message"]["data"].clone()).collect::<Vec<_>>(), vec![json!(1), json!(2)]);
    // The window is full: nothing more until something is acknowledged.
    assert!(tokio::time::timeout(Duration::from_millis(300), ws.next()).await.is_err());
    ws.send(WsMessage::Text(
        json!({"type": "ack", "ack_ids": [first[0]["ack_id"]]}).to_string().into(),
    ))
    .await
    .unwrap();
    let mut got_result = false;
    let mut more = Vec::new();
    while more.is_empty() {
        let f = next_frame(&mut ws).await;
        match f["type"].as_str() {
            Some("result") => {
                assert_eq!((f["op"].as_str(), f["result"]["count"].as_i64()), (Some("ack"), Some(1)));
                got_result = true;
            }
            Some("messages") => more = f["messages"].as_array().unwrap().clone(),
            other => panic!("unexpected frame {other:?}"),
        }
    }
    assert!(got_result);
    assert_eq!(more.len(), 1);
    assert_eq!(more[0]["message"]["data"], 3);
    // Bad frames are answered, the stream stays open.
    ws.send(WsMessage::Text("{\"type\":\"jump\"}".into())).await.unwrap();
    assert_eq!(next_frame(&mut ws).await["type"], "error");
    ws.close(None).await.unwrap();
    drop(ws);

    // Closing released the held leases (2 and 3) without counting the attempt.
    let got = common::wait_for(Duration::from_secs(5), || async {
        let got = h.pull("s", 10).await;
        (got.len() == 4).then_some(got)
    })
    .await;
    let data: Vec<Json> = common::datas(&got);
    assert_eq!(data, vec![json!(2), json!(3), json!(4), json!(5)]);
    assert_eq!(got[0].delivery_attempt, 1);
}

/// Reads `data:` lines of `message` events from a raw SSE response.
async fn sse_events(stream: &mut TcpStream, buf: &mut String, want: usize, timeout: Duration) -> Vec<Json> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let events: Vec<Json> = buf
            .split("\n\n")
            .filter(|block| block.contains("event: message"))
            .filter_map(|block| {
                block
                    .lines()
                    .find_map(|l| l.strip_prefix("data: "))
                    .and_then(|d| serde_json::from_str(d).ok())
            })
            .collect();
        if events.len() >= want || tokio::time::Instant::now() >= deadline {
            return events;
        }
        let mut chunk = [0u8; 8192];
        match tokio::time::timeout_at(deadline, stream.read(&mut chunk)).await {
            Ok(Ok(0)) | Err(_) => return events,
            Ok(Ok(n)) => buf.push_str(&String::from_utf8_lossy(&chunk[..n])),
            Ok(Err(e)) => panic!("{e}"),
        }
    }
}

#[tokio::test]
async fn sse_streaming_pull_with_rest_acks() {
    let h = harness().await;
    h.topic("t").await;
    h.sub("s", "t").await;
    h.publish("t", json!("a")).await;
    h.publish("t", json!("b")).await;
    let addr = serve(app(&h)).await;
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(
            b"GET /api/v1/queues/subscriptions/s/events?max_messages=1 HTTP/1.1\r\nHost: x\r\nx-test-as: admin\r\nAccept: text/event-stream\r\n\r\n",
        )
        .await
        .unwrap();
    let mut buf = String::new();
    let first = sse_events(&mut stream, &mut buf, 1, Duration::from_secs(5)).await;
    assert!(buf.starts_with("HTTP/1.1 200"), "{buf}");
    assert_eq!(first.len(), 1);
    assert_eq!(first[0]["message"]["data"], "a");
    let again = sse_events(&mut stream, &mut buf, 2, Duration::from_millis(300)).await;
    assert_eq!(again.len(), 1, "window of one: b waits for the ack");
    let ack = h.svc.ack(&admin(), "s", &[first[0]["ack_id"].as_str().unwrap().to_owned()]).await.unwrap();
    assert_eq!(ack.count, 1);
    let both = sse_events(&mut stream, &mut buf, 2, Duration::from_secs(5)).await;
    assert_eq!(both.len(), 2);
    assert_eq!(both[1]["message"]["data"], "b");
    // Server shutdown ends open streams.
    h.svc.close_streams();
    let mut rest = Vec::new();
    let ended = tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut rest)).await;
    assert!(ended.is_ok(), "the stream ended");
}

// ------------------------------------------------------------------- push

/// Maps host names to addresses (a fake DNS).
#[derive(Debug)]
struct MapResolver(HashMap<String, Vec<IpAddr>>);

impl Resolver for MapResolver {
    fn resolve<'a>(&'a self, host: &'a str, _port: u16) -> BoxFuture<'a, io::Result<Vec<IpAddr>>> {
        let r = self
            .0
            .get(host)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such host"));
        Box::pin(async move { r })
    }
}

fn public() -> IpAddr {
    "93.184.216.34".parse().unwrap()
}

/// Routes the public test address to the local mock.
#[derive(Debug)]
struct Route(SocketAddr);

impl Connector for Route {
    fn connect(&self, addr: SocketAddr) -> BoxFuture<'_, io::Result<TcpStream>> {
        let target = if addr.ip() == public() { self.0 } else { addr };
        Box::pin(TcpStream::connect(target))
    }
}

#[derive(Default)]
struct Hook {
    calls: Mutex<Vec<(String, Vec<u8>)>>,
    fail_first: usize,
}

async fn mock_webhook(fail_first: usize) -> (SocketAddr, Arc<Hook>) {
    let hook = Arc::new(Hook {
        fail_first,
        ..Hook::default()
    });
    let state = hook.clone();
    let app = Router::new().route(
        "/hook",
        post(move |headers: axum::http::HeaderMap, body: axum::body::Bytes| {
            let state = state.clone();
            async move {
                let sig = headers
                    .get(push::SIGNATURE_HEADER)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default()
                    .to_owned();
                let mut calls = state.calls.lock().unwrap();
                calls.push((sig, body.to_vec()));
                if calls.len() <= state.fail_first {
                    StatusCode::INTERNAL_SERVER_ERROR
                } else {
                    StatusCode::NO_CONTENT
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (addr, hook)
}

fn fetcher(mock: SocketAddr) -> Fetcher {
    let dns = MapResolver(
        [
            ("hooks.test".to_owned(), vec![public()]),
            ("internal.test".to_owned(), vec!["10.1.2.3".parse().unwrap()]),
        ]
        .into_iter()
        .collect(),
    );
    Fetcher::with(
        &forgebase_auth::net::TlsSettings::new(),
        FetchConfig::default(),
        Arc::new(dns),
        Arc::new(Route(mock)),
    )
    .unwrap()
}

fn push_sub(name: &str, kind: PushKind, endpoint: &str, attempts: Option<i64>) -> NewSubscription {
    NewSubscription {
        name: name.into(),
        topic: "t".into(),
        backoff_base_ms: Some(0),
        max_delivery_attempts: attempts,
        dead_letter_topic: Some("dead".into()),
        push: Some(PushConfig {
            kind,
            endpoint: endpoint.into(),
        }),
        ..NewSubscription::default()
    }
}

#[tokio::test]
async fn webhook_push_is_signed_and_retried_until_it_succeeds() {
    let h = harness().await;
    let (mock, hook) = mock_webhook(2).await;
    h.svc.set_fetcher(fetcher(mock));
    h.topic("t").await;
    h.topic("dead").await;
    let sub = h
        .sub_with(push_sub("hook", PushKind::Webhook, "http://hooks.test/hook", Some(5)))
        .await;
    let secret = sub.push_secret.clone().expect("secret shown once on create");
    assert!(secret.starts_with("whsec_"));
    assert!(h.svc.subscription(&admin(), "hook").await.unwrap().push_secret.is_none());
    let id = h.publish("t", json!({"order": 7})).await;

    // Pull is refused on a push subscription.
    let err = h.svc.pull(&admin(), "hook", PullRequest::default()).await.unwrap_err();
    assert_eq!(err.code(), "push_subscription");

    for round in 1..=3 {
        let ok = h.svc.deliver_push().await.unwrap();
        assert_eq!(ok, usize::from(round == 3), "round {round}");
    }
    assert_eq!(h.svc.deliver_push().await.unwrap(), 0, "acked: not sent again");
    let calls = hook.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 3);
    let now = h.svc.now() / 1000;
    for (i, (sig, body)) in calls.iter().enumerate() {
        assert!(push::verify(&secret, sig, body, now, 300), "signature of call {i}");
        let b: Json = serde_json::from_slice(body).unwrap();
        assert_eq!(b["delivery_attempt"], i + 1);
        assert_eq!(b["message"]["id"], id);
        assert_eq!(b["message"]["data"]["order"], 7);
    }
    let m = h.svc.subscription_metrics(&admin(), "hook").await.unwrap();
    assert_eq!((m.acked_total, m.nacked_total, m.backlog_messages), (1, 2, 0));

    // The background task delivers on its own.
    let (mock2, hook2) = mock_webhook(0).await;
    let h2 = harness().await;
    h2.svc.set_fetcher(fetcher(mock2));
    h2.topic("t").await;
    h2.topic("dead").await;
    h2.sub_with(push_sub("bg", PushKind::Webhook, "http://hooks.test/hook", None)).await;
    h2.svc.start_background();
    h2.publish("t", json!("bg")).await;
    common::wait_for(Duration::from_secs(5), || async { (hook2.calls.lock().unwrap().len() == 1).then_some(()) }).await;
    h2.svc.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn webhook_push_is_ssrf_safe() {
    let h = harness().await;
    let (mock, hook) = mock_webhook(0).await;
    h.svc.set_fetcher(fetcher(mock));
    h.topic("t").await;
    h.topic("dead").await;
    // Literal internal addresses are refused when configured.
    for url in ["http://127.0.0.1:9/x", "http://169.254.169.254/latest", "http://[::1]/x", "http://10.0.0.1/x"] {
        let err = h
            .svc
            .create_subscription(&admin(), push_sub("bad", PushKind::Webhook, url, None))
            .await
            .unwrap_err();
        assert_eq!(err.code(), "push_endpoint_blocked", "{url}");
    }
    let err = h
        .svc
        .create_subscription(&admin(), push_sub("bad", PushKind::Webhook, "ftp://hooks.test/x", None))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "invalid_push_endpoint");
    // A name that resolves to an internal address is refused at delivery.
    h.sub_with(push_sub("internal", PushKind::Webhook, "http://internal.test/hook", Some(1)))
        .await;
    h.publish("t", json!("secret")).await;
    assert_eq!(h.svc.deliver_push().await.unwrap(), 0);
    assert!(hook.calls.lock().unwrap().is_empty(), "nothing reached any server");
    let dl = h.svc.dead_letters(&admin(), "internal", None, 10).await.unwrap().items;
    assert_eq!(dl.len(), 1);
    let err = dl[0].history[0].error.clone().unwrap_or_default();
    assert!(err.contains("blocked"), "{err}");

    // https is required unless the operator allows http.
    let strict = harness_with(QueueSettings {
        allow_http_push: false,
        ..settings()
    })
    .await;
    strict.topic("t").await;
    strict.topic("dead").await;
    let err = strict
        .svc
        .create_subscription(&admin(), push_sub("plain", PushKind::Webhook, "http://hooks.test/hook", None))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "invalid_push_endpoint");
    strict
        .svc
        .create_subscription(&admin(), push_sub("tls", PushKind::Webhook, "https://hooks.test/hook", None))
        .await
        .unwrap();
}

fn wasm(text: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(wat::parse_str(text).unwrap())
}

fn deploy(name: &str, body: &str) -> DeployRequest {
    DeployRequest {
        name: name.into(),
        wasm: wasm(&format!(
            "(module (memory (export \"memory\") 1) (func (export \"handle\") (result i32) {body}))"
        )),
        description: None,
        access: None,
        capabilities: Vec::new(),
        fetch_hosts: Vec::new(),
        limits: LimitsInput::default(),
        env: Default::default(),
        hooks: Vec::new(),
        activate: true,
    }
}

#[tokio::test]
async fn push_to_a_function_acks_on_success_and_dead_letters_failures() {
    let h = harness().await;
    let functions = FunctionService::open(h.data.clone(), None, h.keyring.clone(), FunctionsConfig::default())
        .await
        .unwrap();
    functions.deploy(&admin(), deploy("ok-fn", "(i32.const 0)")).await.unwrap();
    functions.deploy(&admin(), deploy("bad-fn", "unreachable")).await.unwrap();
    h.svc.attach_functions(functions);
    h.topic("t").await;
    h.topic("dead").await;
    h.sub("dead-reader", "dead").await;
    h.sub_with(push_sub("to-ok", PushKind::Function, "ok-fn", Some(2))).await;
    h.sub_with(push_sub("to-bad", PushKind::Function, "bad-fn", Some(2))).await;
    h.publish("t", json!({"n": 1})).await;

    assert_eq!(h.svc.deliver_push().await.unwrap(), 1, "ok-fn acks, bad-fn fails");
    assert_eq!(h.svc.deliver_push().await.unwrap(), 0, "bad-fn fails again");
    let ok = h.svc.subscription_metrics(&admin(), "to-ok").await.unwrap();
    assert_eq!((ok.acked_total, ok.backlog_messages), (1, 0));
    let bad = h.svc.dead_letters(&admin(), "to-bad", None, 10).await.unwrap().items;
    assert_eq!(bad.len(), 1);
    assert_eq!(bad[0].history.len(), 2);
    assert!(bad[0].history.iter().all(|a| a.outcome.as_deref() == Some("failed") && a.error.is_some()));
    let copy = h.pull("dead-reader", 10).await;
    assert_eq!(copy.len(), 1);
    assert_eq!(copy[0].message.attributes["forgebase.dead_letter.subscription"], "to-bad");
}

#[tokio::test]
async fn the_push_secret_survives_a_restart() {
    let h = harness().await;
    let (mock, hook) = mock_webhook(0).await;
    h.topic("t").await;
    h.topic("dead").await;
    let secret = h
        .sub_with(push_sub("hook", PushKind::Webhook, "http://hooks.test/hook", None))
        .await
        .push_secret
        .unwrap();
    h.publish("t", json!("after restart")).await;
    let h = h.restart().await;
    h.svc.set_fetcher(fetcher(mock));
    assert_eq!(h.svc.deliver_push().await.unwrap(), 1);
    let calls = hook.calls.lock().unwrap().clone();
    assert!(push::verify(&secret, &calls[0].0, &calls[0].1, h.svc.now() / 1000, 300));
    let _ = NewTopic::default();
    let _ = NewMessage::new(json!(null));
}
