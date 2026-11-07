//! The graph API is mounted at `/api/v1/graph` behind the same identity and
//! authorisation rules as the rest of `/api/v1`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::http::StatusCode;
use common::Api;
use serde_json::json;

#[tokio::test]
async fn anonymous_is_denied_and_the_envelope_has_a_request_id() {
    let api = Api::anonymous().await;
    for uri in ["/api/v1/graph/nodes", "/api/v1/graph/nodes/a", "/api/v1/graph/stats"] {
        let r = api.get(uri).await.expect(StatusCode::UNAUTHORIZED);
        let j = r.json();
        assert_eq!(j["error"]["code"], "unauthenticated", "{uri}");
        assert!(!j["error"]["request_id"].as_str().unwrap().is_empty(), "{uri}: {j}");
    }
    api.post("/api/v1/graph/nodes", json!({"id": "a"}))
        .await
        .expect(StatusCode::UNAUTHORIZED);
    api.post("/api/v1/graph/traverse", json!({"start": "a"}))
        .await
        .expect(StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn administrator_round_trip_through_the_server_router() {
    let api = Api::admin().await;
    api.post("/api/v1/graph/nodes", json!({"id": "a", "label": "User"}))
        .await
        .expect(StatusCode::CREATED);
    api.post("/api/v1/graph/nodes", json!({"id": "b"}))
        .await
        .expect(StatusCode::CREATED);
    api.post(
        "/api/v1/graph/edges",
        json!({"type": "FOLLOWS", "source": "a", "target": "b"}),
    )
    .await
    .expect(StatusCode::CREATED);
    let r = api
        .post(
            "/api/v1/graph/traverse",
            json!({"start": "a", "depth": 3, "edge": "FOLLOWS"}),
        )
        .await
        .expect(StatusCode::OK)
        .json();
    assert_eq!(r["nodes"][0]["node"]["id"], "b");
    assert_eq!(r["nodes"][0]["depth"], 1);
    let missing = api.get("/api/v1/graph/nodes/ghost").await.expect(StatusCode::NOT_FOUND);
    assert_eq!(missing.code(), "node_not_found");
    assert!(missing.headers.contains_key("x-request-id"));
    assert_eq!(missing.headers["x-content-type-options"], "nosniff");
}
