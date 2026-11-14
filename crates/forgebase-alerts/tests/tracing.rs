//! Trace context end to end (ADR-0035 §1-2), exported over OTLP/HTTP JSON
//! to an in-process collector: an HTTP request with a caller's
//! `traceparent` → a JavaScript function → `ctx.queues.publish` → push
//! delivery → the webhook receiver gets a `traceparent` of the same trace,
//! with the right parent/child links between the server, function,
//! producer, consumer and client spans; and a job continues the trace of
//! the span that enqueued it. One test: the subscriber and exporter are
//! process-wide.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{Method, Request, StatusCode};
use common::admin;
use forgebase_auth::crypto::Keyring;
use forgebase_core::config::telemetry_config::{ObservabilityConfig, OtlpProtocol};
use forgebase_core::{DataLayout, LogFormat};
use forgebase_data::schema::Collection;
use forgebase_data::{AccessContext, Action, Authorizer, DataService, Decision};
use forgebase_db::{Backend, BoxFuture};
use forgebase_functions::{
    DeployRequest, FunctionService, FunctionsConfig, QueueMessage, QueuePublisher, ServiceError,
};
use forgebase_queue::{NewMessage, NewSubscription, NewTopic, PushConfig, PushKind, QueueService, QueueSettings};
use serde_json::{Value as Json, json};
use tokio::net::TcpListener;
use tower::ServiceExt;

/// Lets everyone do everything (the function's identity publishes).
#[derive(Debug)]
struct AllowAll;

impl Authorizer for AllowAll {
    fn authorize(&self, _: &AccessContext, _: Action, _: Option<&Collection>) -> Decision {
        Decision::Allow
    }
}

/// `ctx.queues.publish` through the queue engine, as the server wires it.
#[derive(Debug)]
struct Publisher(QueueService);

impl QueuePublisher for Publisher {
    fn publish<'a>(&'a self, ctx: &'a AccessContext, msg: QueueMessage) -> BoxFuture<'a, Result<Json, ServiceError>> {
        Box::pin(async move {
            let mut m = NewMessage::new(msg.data);
            m.attributes = msg.attributes;
            let out = self
                .0
                .publish(ctx, &msg.topic, vec![m])
                .await
                .map_err(|e| ServiceError::new("publish_failed", e.to_string()))?;
            Ok(serde_json::to_value(&out[0]).unwrap())
        })
    }
}

/// An OTLP/HTTP collector: keeps every JSON body posted to `/v1/traces`.
async fn collector() -> (SocketAddr, Arc<Mutex<Vec<Json>>>) {
    let got = Arc::new(Mutex::new(Vec::new()));
    async fn traces(State(got): State<Arc<Mutex<Vec<Json>>>>, body: Bytes) -> StatusCode {
        got.lock()
            .unwrap()
            .push(serde_json::from_slice(&body).unwrap_or(Json::Null));
        StatusCode::OK
    }
    let app = Router::new()
        .route("/v1/traces", axum::routing::post(traces))
        .with_state(got.clone());
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(l, app).await;
    });
    (addr, got)
}

#[derive(Debug, Clone)]
struct Span {
    trace: String,
    id: String,
    parent: String,
    name: String,
    kind: i64,
    attrs: HashMap<String, Json>,
    status: i64,
}

fn spans(bodies: &[Json]) -> Vec<Span> {
    let mut out = Vec::new();
    for b in bodies {
        for rs in b["resourceSpans"].as_array().into_iter().flatten() {
            assert_eq!(rs["resource"]["attributes"][0]["key"], "service.name");
            for ss in rs["scopeSpans"].as_array().into_iter().flatten() {
                for s in ss["spans"].as_array().into_iter().flatten() {
                    out.push(Span {
                        trace: s["traceId"].as_str().unwrap_or_default().to_owned(),
                        id: s["spanId"].as_str().unwrap_or_default().to_owned(),
                        parent: s["parentSpanId"].as_str().unwrap_or_default().to_owned(),
                        name: s["name"].as_str().unwrap_or_default().to_owned(),
                        kind: s["kind"].as_i64().unwrap_or_default(),
                        attrs: s["attributes"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .map(|a| (a["key"].as_str().unwrap_or_default().to_owned(), a["value"].clone()))
                            .collect(),
                        status: s["status"]["code"].as_i64().unwrap_or_default(),
                    });
                }
            }
        }
    }
    out
}

fn find<'a>(all: &'a [Span], name: &str) -> &'a Span {
    all.iter().find(|s| s.name == name).unwrap_or_else(|| {
        panic!(
            "no span {name:?} in {:#?}",
            all.iter().map(|s| &s.name).collect::<Vec<_>>()
        )
    })
}

const CALLER_TRACE: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
const CALLER_SPAN: &str = "00f067aa0ba902b7";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_request_is_traced_through_a_function_a_queue_and_a_push_webhook() {
    let (otlp, got) = collector().await;
    let mut obs = ObservabilityConfig::default();
    obs.otlp.enabled = true;
    obs.otlp.endpoint = format!("http://{otlp}");
    obs.otlp.protocol = OtlpProtocol::HttpJson;
    obs.otlp.service_name = "forgebase-test".into();
    forgebase_observability::init_telemetry("warn", LogFormat::Json, &obs, false).unwrap();
    assert!(forgebase_observability::trace::propagation_enabled());

    let dir = tempfile::tempdir().unwrap();
    let test = forgebase_db::testing::test_backend().await;
    let db = test.database().clone();
    let backend: Arc<dyn Backend> = Arc::new(db.clone());
    let data = DataService::new(backend, Arc::new(AllowAll)).await.unwrap();
    let keyring = Keyring::load_or_create(&dir.path().join("master.key")).unwrap();
    let (hook_addr, receiver) = common::receiver().await;
    let queue = QueueService::new(
        data.clone(),
        keyring.clone(),
        QueueSettings {
            allow_http_push: true,
            ..QueueSettings::default()
        },
    );
    queue.set_fetcher(common::fetcher(hook_addr));
    let fns = FunctionService::open(data.clone(), None, keyring, FunctionsConfig::default())
        .await
        .unwrap();
    fns.attach_services(forgebase_functions::Services {
        queue: Some(Arc::new(Publisher(queue.clone()))),
        ..Default::default()
    });
    let sys = AccessContext::system();
    queue
        .create_topic(
            &sys,
            NewTopic {
                name: "orders".into(),
                ..NewTopic::default()
            },
        )
        .await
        .unwrap();
    queue
        .create_subscription(
            &sys,
            NewSubscription {
                name: "hook".into(),
                topic: "orders".into(),
                push: Some(PushConfig {
                    kind: PushKind::Webhook,
                    endpoint: "http://hooks.test/push".into(),
                }),
                ..NewSubscription::default()
            },
        )
        .await
        .unwrap();
    let src = r#"import { serve } from "forgebase";
serve(async (req, ctx) => {
  const r = await ctx.queues.publish("orders", { order: 42 });
  return Response.json({ ok: true, id: r.id });
});"#;
    let deploy: DeployRequest = serde_json::from_value(json!({
        "name": "checkout", "source": src, "language": "javascript", "access": "public",
        "capabilities": ["queues.publish"],
    }))
    .unwrap();
    fns.deploy(&admin(), deploy).await.unwrap();

    let layout = DataLayout::new(dir.path().join("data"));
    layout.ensure().unwrap();
    let app = forgebase_api::router(
        forgebase_api::AppState::from_data(db.clone(), layout, data.clone()),
        forgebase_api::RouterOptions {
            extensions: vec![forgebase_functions::extension(fns.clone())],
            request_timeout: Duration::from_secs(300),
            ..forgebase_api::RouterOptions::default()
        },
    );
    // A direct loopback caller: trusted, so its sampled flag is kept.
    let peer = SocketAddr::from(([127, 0, 0, 1], 40000));
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/functions/checkout/invoke")
        .header("content-type", "application/json")
        .header("traceparent", format!("00-{CALLER_TRACE}-{CALLER_SPAN}-01"))
        .header("tracestate", "vendor=abc")
        .extension(axum::extract::ConnectInfo(peer))
        .body(Body::from("{}"))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let back = res
        .headers()
        .get("traceparent")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .expect("a traceparent response header");
    let body = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert!(back.starts_with(&format!("00-{CALLER_TRACE}-")), "{back}");
    assert!(back.ends_with("-01"));

    // Push delivery: the receiver gets the same trace.
    assert_eq!(queue.deliver_push().await.unwrap(), 1);
    let calls = receiver.on("/push");
    assert_eq!(calls.len(), 1);
    let tp = calls[0].header("traceparent").to_owned();
    assert!(
        tp.starts_with(&format!("00-{CALLER_TRACE}-")),
        "the webhook continues the trace: {tp}"
    );
    assert_eq!(calls[0].header("tracestate"), "vendor=abc");
    let delivered = calls[0].json();
    let attr_tp = delivered["message"]["attributes"]["traceparent"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        attr_tp.contains(CALLER_TRACE),
        "the message carries the publish's trace: {delivered}"
    );

    // The execution log has the trace id.
    let logs = fns.logs(&admin(), "checkout", 5).await.unwrap();
    assert_eq!(logs[0].trace_id.as_deref(), Some(CALLER_TRACE));

    // Jobs: the run continues the enqueuing span's trace.
    let jobs = forgebase_jobs::JobService::new(data.clone());
    let seen: Arc<Mutex<Option<forgebase_observability::trace::SpanContext>>> = Arc::default();
    let seen2 = seen.clone();
    let registry = forgebase_jobs::JobRegistry::new().register("traced", move |_ctx: forgebase_jobs::JobContext| {
        let seen = seen2.clone();
        async move {
            *seen.lock().unwrap() = forgebase_observability::trace::current();
            Ok(Json::Null)
        }
    });
    let parent = tracing::info_span!("enqueue", otel.name = "enqueue job");
    let parent_ctx = forgebase_observability::trace::span_context(&parent).unwrap();
    let job = {
        use tracing::Instrument as _;
        jobs.enqueue(&sys, forgebase_jobs::NewJob::new("traced", json!({})))
            .instrument(parent.clone())
            .await
            .unwrap()
            .0
    };
    drop(parent);
    assert_eq!(job.traceparent.as_deref(), Some(parent_ctx.traceparent().as_str()));
    let pool = forgebase_jobs::start_pool(jobs.clone(), registry, forgebase_jobs::PoolConfig::default());
    let mut ran = None;
    for _ in 0..200 {
        if let Some(c) = seen.lock().unwrap().clone() {
            ran = Some(c);
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    pool.shutdown().await;
    let ran = ran.expect("the job ran");
    assert_eq!(ran.trace_id, parent_ctx.trace_id, "same trace");

    // Export: every span reaches the collector with the right links.
    let flushed = tokio::task::spawn_blocking(|| forgebase_observability::otlp::flush(Duration::from_secs(10)))
        .await
        .unwrap();
    assert!(flushed);
    let all = spans(&got.lock().unwrap());
    let server = find(&all, "POST /api/v1/functions/{name}/invoke");
    assert_eq!(
        (server.trace.as_str(), server.parent.as_str(), server.kind),
        (CALLER_TRACE, CALLER_SPAN, 2)
    );
    assert_eq!(
        server.attrs["http.route"]["stringValue"],
        "/api/v1/functions/{name}/invoke"
    );
    assert_eq!(server.attrs["http.response.status_code"]["intValue"], "200");
    assert!(!server.attrs.contains_key("path"), "no raw path is exported");
    assert!(server.attrs.contains_key("forgebase.request_id"));
    assert!(back.contains(&server.id), "the response names the server span");
    let function = find(&all, "function checkout");
    assert_eq!((function.parent.as_str(), function.kind), (server.id.as_str(), 1));
    let publish = find(&all, "publish orders");
    assert_eq!((publish.parent.as_str(), publish.kind), (function.id.as_str(), 4));
    assert!(attr_tp.contains(&publish.id), "the attribute is the producer span");
    let deliver = find(&all, "deliver hook");
    assert_eq!((deliver.parent.as_str(), deliver.kind), (publish.id.as_str(), 5));
    assert_eq!(deliver.attrs["messaging.destination.name"]["stringValue"], "orders");
    let client = all
        .iter()
        .find(|s| s.kind == 3 && s.parent == deliver.id)
        .expect("a client span under the delivery");
    assert_eq!(client.name, "POST");
    assert_eq!(client.attrs["server.address"]["stringValue"], "hooks.test");
    assert!(tp.contains(&client.id), "the webhook's parent is the client span");
    for s in [server, function, publish, deliver, client] {
        assert_eq!(s.trace, CALLER_TRACE, "{}", s.name);
        assert_eq!(s.status, 0, "{} ok", s.name);
    }
    let run = find(&all, "job traced");
    assert_eq!(run.parent, parent_ctx.span_id_hex());
    assert_eq!(run.kind, 5);
    assert_eq!(run.trace, parent_ctx.trace_id_hex());
    fns.shutdown();
}
