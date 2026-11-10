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
    // Feature registry and configuration (FB-91, ADR-0026).
    ("get", "/api/system/features"),
    ("patch", "/api/system/config"),
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
    // Policy and tenancy (FB-21, ADR-0008 §10).
    ("get", "/api/v1/realms"),
    ("post", "/api/v1/realms"),
    ("get", "/api/v1/orgs"),
    ("post", "/api/v1/orgs"),
    ("get", "/api/v1/orgs/{org}"),
    ("patch", "/api/v1/orgs/{org}"),
    ("delete", "/api/v1/orgs/{org}"),
    ("get", "/api/v1/orgs/{org}/projects"),
    ("post", "/api/v1/orgs/{org}/projects"),
    ("get", "/api/v1/projects"),
    ("post", "/api/v1/projects"),
    ("get", "/api/v1/projects/{project}"),
    ("delete", "/api/v1/projects/{project}"),
    ("post", "/api/v1/projects/{project}/environments"),
    ("get", "/api/v1/orgs/{org}/members"),
    ("put", "/api/v1/orgs/{org}/members"),
    ("post", "/api/v1/orgs/{org}/members/remove"),
    ("post", "/api/v1/orgs/{org}/members/status"),
    ("get", "/api/v1/orgs/{org}/teams"),
    ("post", "/api/v1/orgs/{org}/teams"),
    ("delete", "/api/v1/teams/{team}"),
    ("post", "/api/v1/teams/{team}/members"),
    ("post", "/api/v1/teams/{team}/members/remove"),
    ("get", "/api/v1/orgs/{org}/roles"),
    ("put", "/api/v1/orgs/{org}/roles"),
    ("delete", "/api/v1/orgs/{org}/roles/{key}"),
    ("put", "/api/v1/orgs/{org}/role-sets"),
    ("get", "/api/v1/roles"),
    ("put", "/api/v1/roles"),
    ("delete", "/api/v1/roles/{key}"),
    ("put", "/api/v1/role-sets"),
    ("put", "/api/v1/realm-bindings"),
    ("post", "/api/v1/overrides"),
    ("delete", "/api/v1/overrides/{id}"),
    ("get", "/api/v1/acl"),
    ("post", "/api/v1/acl"),
    ("delete", "/api/v1/acl/{id}"),
    ("post", "/api/v1/suspensions"),
    ("post", "/api/v1/suspensions/lift"),
    ("get", "/api/v1/policies/{collection}"),
    ("put", "/api/v1/policies/{collection}"),
    ("delete", "/api/v1/policies/{collection}"),
    ("post", "/api/v1/orgs/{org}/invitations"),
    ("post", "/api/v1/invitations/accept"),
    ("get", "/api/v1/authz/me"),
    ("post", "/api/v1/authz/check"),
    ("get", "/api/v1/authz/audit"),
    ("get", "/api/v1/graph/nodes"),
    ("post", "/api/v1/graph/nodes"),
    ("get", "/api/v1/graph/nodes/{id}"),
    ("put", "/api/v1/graph/nodes/{id}"),
    ("patch", "/api/v1/graph/nodes/{id}"),
    ("delete", "/api/v1/graph/nodes/{id}"),
    ("get", "/api/v1/graph/nodes/{id}/neighbors"),
    ("get", "/api/v1/graph/edges"),
    ("post", "/api/v1/graph/edges"),
    ("get", "/api/v1/graph/edges/{id}"),
    ("patch", "/api/v1/graph/edges/{id}"),
    ("delete", "/api/v1/graph/edges/{id}"),
    ("post", "/api/v1/graph/traverse"),
    ("post", "/api/v1/graph/shortest-path"),
    ("get", "/api/v1/graph/stats"),
    ("get", "/api/v1/realtime/ws"),
    ("get", "/api/v1/realtime/sse"),
    ("post", "/api/v1/realtime/channels/{channel}/messages"),
    ("get", "/api/v1/realtime/channels/{channel}/presence"),
    ("get", "/api/v1/realtime/stats"),
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
    let api = Api::with_policy_engine(None).await;
    let doc = api.get("/api/openapi.json").await.json();
    for (method, path) in operations(&doc) {
        let uri = path
            .replace("{collection}", "c1")
            .replace("{id}", "r1")
            .replace("{name}", "n1")
            .replace("{org}", "o1")
            .replace("{project}", "p1")
            .replace("{team}", "t1")
            .replace("{key}", "k1");
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

/// Every operation of a document has a unique `operationId`.
fn assert_unique_operation_ids(doc: &Json) {
    let mut seen = std::collections::BTreeMap::new();
    for (path, item) in doc["paths"].as_object().unwrap() {
        for (method, op) in item.as_object().unwrap() {
            let id = op["operationId"]
                .as_str()
                .unwrap_or_else(|| panic!("{method} {path} has no operationId"));
            if let Some(first) = seen.insert(id.to_owned(), format!("{method} {path}")) {
                panic!("duplicate operationId `{id}`: {first} and {method} {path}");
            }
        }
    }
}

/// The document served with every crate fragment that `forgebase-api` itself
/// depends on (auth, policy, graph, realtime) has unique operationIds, and every
/// `/api/auth/*` operation is documented with a request/response schema.
#[test]
fn operation_ids_are_unique_and_auth_is_documented() {
    let auth = forgebase_auth::openapi();
    let doc = serde_json::to_value(forgebase_api::openapi::document_with([&auth])).unwrap();
    assert_unique_operation_ids(&doc);

    for (m, p) in [
        ("post", "/api/auth/signup"),
        ("post", "/api/auth/login"),
        ("post", "/api/auth/refresh"),
        ("post", "/api/auth/logout"),
        ("get", "/api/auth/me"),
        ("post", "/api/auth/mfa/verify"),
        ("get", "/api/auth/sessions"),
        ("delete", "/api/auth/sessions/{id}"),
        ("post", "/api/auth/tokens"),
        ("post", "/api/auth/impersonate"),
        ("get", "/api/auth/audit"),
        ("post", "/api/auth/keys/rotate"),
        ("post", "/api/auth/oauth/{provider}/start"),
        ("post", "/api/auth/webauthn/login/verify"),
        ("get", "/api/auth/realm"),
        ("post", "/api/auth/phone/request"),
        ("post", "/api/auth/account/delete"),
    ] {
        assert!(doc["paths"][p][m].is_object(), "missing in OpenAPI: {m} {p}");
    }
    let login = &doc["paths"]["/api/auth/login"]["post"];
    assert!(login["requestBody"]["content"]["application/json"]["schema"]["$ref"].is_string());
    assert!(login["responses"]["200"]["content"]["application/json"]["schema"].is_object());
    assert!(login["responses"]["400"].is_object());
}
