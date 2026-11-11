//! The real auth layer (FB-20) in front of the FB-11 routes: credentials
//! become the request's AccessContext; anonymous callers and users are denied
//! by the default authorizer, administrators and admin API keys are allowed,
//! and API-key scopes are enforced before any handler runs.
#![allow(clippy::unwrap_used, missing_docs)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use forgebase_api::{AppState, RouterOptions};
use forgebase_auth::{Argon2Config, AuthConfig, AuthService, ClientInfo};
use forgebase_core::DataLayout;
use serde_json::{Value, json};
use tower::ServiceExt;

async fn app() -> (axum::Router, AuthService, forgebase_db::testing::TestDb) {
    // FORGEBASE_TEST_BACKEND picks SQLite (default) or PostgreSQL (FB-149).
    let tmp = forgebase_db::testing::test_backend().await;
    let layout = DataLayout::new(tmp.data_dir().join("data"));
    layout.ensure().unwrap();
    let db = tmp.database().clone();
    let mut acfg = AuthConfig::for_data_dir(layout.root());
    acfg.argon2 = Argon2Config::insecure_fast_for_tests();
    let auth = AuthService::open_with_data(
        Arc::new(db.clone()),
        Arc::new(forgebase_auth::data::DefaultAuthorizer),
        layout.root(),
        acfg,
        None,
    )
    .await
    .unwrap();
    let state = AppState::from_data(db, layout, auth.data().clone()).with_auth(auth.clone());
    (forgebase_api::router(state, RouterOptions::default()), auth, tmp)
}

async fn call(
    app: &axum::Router,
    method: &str,
    path: &str,
    auth: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value, axum::http::HeaderMap) {
    let mut b = Request::builder().method(method).uri(path);
    if let Some(t) = auth {
        b = b.header("authorization", format!("Bearer {t}"));
    }
    let req = match body {
        Some(v) => b
            .header("content-type", "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    };
    let res = app.clone().oneshot(req).await.unwrap();
    let (status, headers) = (res.status(), res.headers().clone());
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null), headers)
}

#[tokio::test]
async fn credentials_become_the_access_context_of_data_routes() {
    let (app, auth, _tmp) = app().await;

    // Anonymous: the data API answers 401 with the envelope and request id.
    let (s, j, h) = call(&app, "GET", "/api/v1/collections", None, None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "{j}");
    assert_eq!(j["error"]["request_id"], h["x-request-id"].to_str().unwrap());
    // Auth errors from the layer carry the same envelope and headers.
    let (s, j, h) = call(&app, "GET", "/api/v1/collections", Some("forged"), None).await;
    assert_eq!(
        (s, j["error"]["code"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("invalid_token"))
    );
    assert_eq!(j["error"]["request_id"], h["x-request-id"].to_str().unwrap());
    assert_eq!(h["x-content-type-options"], "nosniff");

    // A user is authenticated but the default authorizer denies them.
    let (s, j, _) = call(
        &app,
        "POST",
        "/api/auth/signup",
        None,
        Some(json!({"email": "u@example.com", "password": "user password"})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{j}");
    let (_, j, _) = call(
        &app,
        "POST",
        "/api/auth/login",
        None,
        Some(json!({"email": "u@example.com", "password": "user password"})),
    )
    .await;
    let user = j["access_token"].as_str().unwrap().to_owned();
    let (s, j, _) = call(&app, "GET", "/api/v1/collections", Some(&user), None).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{j}");

    // An administrator reaches the data API; passwords written through it are Argon2id.
    auth.create_admin("root@example.com", "root password 1", "test", &ClientInfo::default())
        .await
        .unwrap();
    let (_, j, _) = call(
        &app,
        "POST",
        "/api/auth/admins/login",
        None,
        Some(json!({"email": "root@example.com", "password": "root password 1"})),
    )
    .await;
    let admin = j["access_token"].as_str().unwrap().to_owned();
    let (s, j, _) = call(&app, "GET", "/api/v1/collections", Some(&admin), None).await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert!(
        j.to_string().contains("\"users\""),
        "default auth collection exists: {j}"
    );
    let (s, j, _) = call(
        &app,
        "POST",
        "/api/v1/collections/users/records",
        Some(&admin),
        Some(json!({"email": "made@example.com", "password": "admin made pw"})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{j}");
    assert!(j.get("password").is_none());
    let (_, j, _) = call(
        &app,
        "POST",
        "/api/auth/login",
        None,
        Some(json!({"email": "made@example.com", "password": "admin made pw"})),
    )
    .await;
    assert!(
        j["access_token"].is_string(),
        "a user created over the data API can sign in: {j}"
    );

    // Admin API key with a read scope: reads pass, writes are refused by scope.
    let (_, sa, _) = call(
        &app,
        "POST",
        "/api/auth/service-accounts",
        Some(&admin),
        Some(json!({"name": "etl", "role": "admin"})),
    )
    .await;
    let (_, k, _) = call(
        &app,
        "POST",
        &format!("/api/auth/service-accounts/{}/keys", sa["id"].as_str().unwrap()),
        Some(&admin),
        Some(json!({"name": "ro", "scopes": ["collections:read"]})),
    )
    .await;
    let key = k["key"].as_str().unwrap();
    let (s, j, _) = call(&app, "GET", "/api/v1/collections/users/records", Some(key), None).await;
    assert_eq!(s, StatusCode::OK, "{j}");
    let (s, j, _) = call(
        &app,
        "POST",
        "/api/v1/collections/users/records",
        Some(key),
        Some(json!({})),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{j}");
    assert!(j["error"]["message"].as_str().unwrap().contains("insufficient_scope"));
}

#[tokio::test]
async fn openapi_documents_the_security_schemes() {
    let (app, _auth, _tmp) = app().await;
    let (s, doc, _) = call(&app, "GET", "/api/openapi.json", None, None).await;
    assert_eq!(s, StatusCode::OK);
    let schemes = &doc["components"]["securitySchemes"];
    assert_eq!(schemes["bearer"]["scheme"], "bearer");
    assert_eq!(schemes["api_key"]["in"], "header");
    assert_eq!(schemes["api_key"]["name"], "X-API-Key");
    assert_eq!(schemes["session_cookie"]["in"], "cookie");
    let op = &doc["paths"]["/api/v1/collections"]["get"];
    assert!(
        op["security"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r.get("bearer").is_some())
    );
}

/// FB-22: the social login and passkey routes are in the OpenAPI document
/// and every documented operation is routed.
#[tokio::test]
async fn openapi_documents_social_login_and_passkeys() {
    let (app, _auth, _tmp) = app().await;
    let (status, doc, _) = call(&app, "GET", "/api/openapi.json", None, None).await;
    assert_eq!(status, StatusCode::OK);
    let mut n = 0;
    for (path, item) in doc["paths"].as_object().unwrap() {
        if !(path.starts_with("/api/auth/oauth/") || path.starts_with("/api/auth/webauthn/")) {
            continue;
        }
        for method in item.as_object().unwrap().keys() {
            n += 1;
            let uri = path.replace("{provider}", "nope").replace("{id}", "x1");
            let (status, body, _) = call(&app, &method.to_uppercase(), &uri, None, Some(json!({}))).await;
            let unrouted = status == StatusCode::METHOD_NOT_ALLOWED
                || (status == StatusCode::NOT_FOUND && body["error"]["message"] == "no route for this path");
            assert!(!unrouted, "{method} {path} documented but not routed");
        }
    }
    assert_eq!(n, 16, "all 16 operations are documented");
}
