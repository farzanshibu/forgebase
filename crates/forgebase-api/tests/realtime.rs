//! `/api/v1/realtime` mounted in the full router (FB-40): the SSE stream
//! outlives the request timeout, admin stats, and `503` without realtime.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::middleware::{self, Next};
use forgebase_api::{AppState, RouterOptions};
use forgebase_core::DataLayout;
use forgebase_data::{AccessContext, DefaultAuthorizer};
use forgebase_realtime::{RealtimeConfig, RealtimeService};
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn app(with_realtime: bool) -> (Router, forgebase_db::testing::TestDb) {
    // FORGEBASE_TEST_BACKEND picks SQLite (default) or PostgreSQL (FB-149).
    let tmp = forgebase_db::testing::test_backend().await;
    let layout = DataLayout::new(tmp.data_dir().join("data"));
    layout.ensure().unwrap();
    let db = tmp.database().clone();
    let mut state = AppState::with_authorizer(db, layout, Arc::new(DefaultAuthorizer))
        .await
        .unwrap();
    if with_realtime {
        let rt = RealtimeService::start(
            state.data().clone(),
            None,
            RealtimeConfig {
                heartbeat_interval: Duration::from_millis(100),
                ..RealtimeConfig::default()
            },
            None,
        )
        .await
        .unwrap();
        state = state.with_realtime(rt);
    }
    let app = forgebase_api::router(
        state,
        RouterOptions {
            request_timeout: Duration::from_millis(300),
            ..RouterOptions::default()
        },
    )
    .layer(middleware::from_fn(|mut req: Request<Body>, next: Next| async move {
        if req.headers().contains_key("x-test-admin") {
            req.extensions_mut().insert(AccessContext::admin("root"));
        }
        next.run(req).await
    }));
    (app, tmp)
}

fn get(uri: &str, admin: bool) -> Request<Body> {
    let mut b = Request::builder().uri(uri);
    if admin {
        b = b.header("x-test-admin", "1");
    }
    b.body(Body::empty()).unwrap()
}

#[tokio::test]
async fn sse_stream_outlives_the_request_timeout() {
    let (app, _tmp) = app(true).await;
    let res = app
        .clone()
        .oneshot(get("/api/v1/realtime/sse?subscriptions=%5B%5D", true))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()["content-type"], "text/event-stream");
    let mut body = res.into_body();
    let mut text = String::new();
    let start = std::time::Instant::now();
    // Read for three request timeouts: welcome, then heartbeat comments.
    while start.elapsed() < Duration::from_millis(900) {
        let frame = tokio::time::timeout(Duration::from_secs(2), body.frame())
            .await
            .expect("stream stalled")
            .expect("stream ended early")
            .unwrap();
        if let Ok(b) = frame.into_data() {
            text.push_str(&String::from_utf8_lossy(&b));
        }
    }
    assert!(text.contains("event: welcome"), "{text}");
    assert!(text.matches(": ping").count() >= 3, "{text}");

    let res = app.clone().oneshot(get("/api/v1/realtime/stats", true)).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let stats: serde_json::Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(stats["connections"], 1);

    let res = app.oneshot(get("/api/v1/realtime/stats", false)).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn realtime_routes_answer_503_without_the_service() {
    let (app, _tmp) = app(false).await;
    let res = app.oneshot(get("/api/v1/realtime/stats", true)).await.unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body: serde_json::Value = serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["error"]["code"], "realtime_unavailable");
}
