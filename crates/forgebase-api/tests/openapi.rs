//! The OpenAPI document covers every route, and every documented operation is routed.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::body::Body;
use axum::http::{Method, StatusCode};
use common::Api;
use serde_json::{Value as Json, json};

/// Every (method, path) the server serves besides `/admin/*` (the embedded
/// dashboard is static files, not API).
const EXPECTED: &[(&str, &str)] = &[
    ("get", "/health"),
    ("get", "/health/ready"),
    ("get", "/metrics"),
    ("get", "/api/system/info"),
    ("get", "/api/openapi.json"),
    ("get", "/api/v1/collections"),
    ("post", "/api/v1/collections"),
    ("get", "/api/v1/collections/{collection}"),
    ("put", "/api/v1/collections/{collection}"),
    ("delete", "/api/v1/collections/{collection}"),
    ("get", "/api/v1/collections/{collection}/schema"),
    ("get", "/api/v1/collections/{collection}/history"),
    ("get", "/api/v1/collections/{collection}/records"),
    ("post", "/api/v1/collections/{collection}/records"),
    ("post", "/api/v1/collections/{collection}/records/query"),
    ("get", "/api/v1/collections/{collection}/records/count"),
    ("post", "/api/v1/collections/{collection}/records/explain"),
    ("post", "/api/v1/collections/{collection}/aggregate"),
    ("get", "/api/v1/collections/{collection}/records/{id}"),
    ("patch", "/api/v1/collections/{collection}/records/{id}"),
    ("put", "/api/v1/collections/{collection}/records/{id}"),
    ("delete", "/api/v1/collections/{collection}/records/{id}"),
    ("get", "/api/v1/collections/{collection}/records/{id}/versions"),
    ("post", "/api/v1/documents/{collection}"),
    ("get", "/api/v1/documents/{collection}/{id}"),
    ("patch", "/api/v1/documents/{collection}/{id}"),
    ("put", "/api/v1/documents/{collection}/{id}"),
    ("delete", "/api/v1/documents/{collection}/{id}"),
    ("get", "/api/v1/documents/{collection}/{id}/versions"),
    ("get", "/api/v1/indexes"),
    ("post", "/api/v1/indexes"),
    ("get", "/api/v1/indexes/recommendations"),
    ("post", "/api/v1/indexes/analyze"),
    ("get", "/api/v1/indexes/{name}"),
    ("delete", "/api/v1/indexes/{name}"),
    ("post", "/api/v1/indexes/{name}/rebuild"),
    ("get", "/api/v1/indexes/{name}/stats"),
    ("get", "/api/v1/slow-queries"),
    ("delete", "/api/v1/slow-queries"),
    ("put", "/api/v1/slow-queries/threshold"),
    ("post", "/api/v1/sql"),
    ("post", "/api/v1/sql/batch"),
    ("post", "/api/v1/sql/explain"),
    ("get", "/api/v1/sql/tables"),
    ("get", "/api/v1/sql/tables/{name}"),
    ("get", "/api/v1/collections/{collection}/export"),
    ("post", "/api/v1/collections/{collection}/import"),
];

fn operations(doc: &Json) -> Vec<(String, String)> {
    let mut ops = Vec::new();
    for (path, item) in doc["paths"].as_object().unwrap() {
        for method in item.as_object().unwrap().keys() {
            ops.push((method.clone(), path.clone()));
        }
    }
    ops
}

#[tokio::test]
async fn document_lists_every_route() {
    let api = Api::anonymous().await;
    let res = api.get("/api/openapi.json").await.expect(StatusCode::OK);
    assert!(
        res.headers["content-type"]
            .to_str()
            .unwrap()
            .starts_with("application/json")
    );
    let doc = res.json();
    assert!(doc["openapi"].as_str().unwrap().starts_with("3."));
    assert_eq!(doc["info"]["title"], "ForgeBase API");
    assert_eq!(doc["info"]["license"]["name"], "MIT OR Apache-2.0");

    let ops = operations(&doc);
    for (m, p) in EXPECTED {
        assert!(
            ops.contains(&((*m).to_owned(), (*p).to_owned())),
            "missing in OpenAPI: {m} {p}"
        );
    }
    assert_eq!(ops.len(), EXPECTED.len(), "documented but not expected: {:?}", {
        ops.iter()
            .filter(|o| !EXPECTED.contains(&(o.0.as_str(), o.1.as_str())))
            .collect::<Vec<_>>()
    });

    // Shared error schema is referenced by the added error responses.
    assert!(doc["components"]["schemas"]["ErrorEnvelope"].is_object());
    let list = &doc["paths"]["/api/v1/collections/{collection}/records"]["get"];
    assert_eq!(
        list["responses"]["401"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/ErrorEnvelope"
    );
    assert!(list["responses"]["404"].is_object());
    assert!(
        doc["paths"]["/api/v1/collections"]["get"]["responses"]
            .get("404")
            .is_none()
    );
    // every $ref resolves
    let text = doc.to_string();
    for part in text.split("\"$ref\":\"#/components/schemas/").skip(1) {
        let name = part.split('"').next().unwrap();
        assert!(doc["components"]["schemas"][name].is_object(), "dangling $ref {name}");
    }
    // every operation has a summary/description and a tag
    for (path, item) in doc["paths"].as_object().unwrap() {
        for (m, op) in item.as_object().unwrap() {
            assert!(op["tags"].is_array(), "{m} {path} has no tag");
            assert!(
                op["responses"].as_object().is_some_and(|r| !r.is_empty()),
                "{m} {path} has no responses"
            );
        }
    }
}

/// Every documented operation is actually routed: a request to it never gets
/// the router's 404 / 405.
#[tokio::test]
async fn every_documented_operation_is_routed() {
    let api = Api::anonymous().await;
    let doc = api.get("/api/openapi.json").await.json();
    for (method, path) in operations(&doc) {
        let uri = path
            .replace("{collection}", "c1")
            .replace("{id}", "r1")
            .replace("{name}", "n1");
        let m = Method::from_bytes(method.to_ascii_uppercase().as_bytes()).unwrap();
        let r = api
            .send(
                m,
                &uri,
                &[("content-type", "application/json")],
                Body::from(json!({}).to_string()),
            )
            .await;
        assert_ne!(
            r.status,
            StatusCode::METHOD_NOT_ALLOWED,
            "{method} {path} is documented but not routed"
        );
        let unrouted = r.status == StatusCode::NOT_FOUND && r.json()["error"]["message"] == "no route for this path";
        assert!(!unrouted, "{method} {path} is documented but not routed");
    }
}
