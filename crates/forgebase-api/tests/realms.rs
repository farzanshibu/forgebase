//! FB-25 realm isolation through the whole stack (auth layer, policy tenancy
//! layer, policy engine): identities act in their own realm, and the policy
//! engine refuses a foreign realm. Wired like `forgebase-server`.
#![allow(clippy::unwrap_used, missing_docs)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use forgebase_api::{AppState, RouterOptions};
use forgebase_auth::{Argon2Config, AuthConfig, AuthService, ClientInfo};
use forgebase_core::DataLayout;
use forgebase_db::{Database, DbOptions};
use serde_json::{Value, json};
use tower::ServiceExt;

const PW: &str = "correct horse battery";

async fn app() -> (
    axum::Router,
    AuthService,
    forgebase_policy::PolicyService,
    tempfile::TempDir,
) {
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().join("data"));
    layout.ensure().unwrap();
    let cfg = forgebase_core::Config::defaults(tmp.path());
    let db = Database::open(DbOptions::from_config(&cfg.database, layout.database_path()))
        .await
        .unwrap();
    db.migrate(None).await.unwrap();
    let backend: Arc<dyn forgebase_db::Backend> = Arc::new(db.clone());
    let policy = forgebase_policy::PolicyService::open(backend.clone()).await.unwrap();
    let mut acfg = AuthConfig::for_data_dir(layout.root());
    acfg.argon2 = Argon2Config::insecure_fast_for_tests();
    let auth = AuthService::open_with_data(backend, policy.authorizer(), layout.root(), acfg, None)
        .await
        .unwrap();
    policy.attach_data(auth.data().clone());
    policy.attach_realms(auth.realms().clone()).await.unwrap();
    let p = policy.clone();
    auth.set_change_hook(Arc::new(move || {
        let p = p.clone();
        tokio::spawn(async move {
            let _ = p.reload().await;
        });
    }));
    let state = AppState::from_data(db, layout, auth.data().clone())
        .with_auth(auth.clone())
        .with_policy(policy.clone());
    (
        forgebase_api::router(state, RouterOptions::default()),
        auth,
        policy,
        tmp,
    )
}

async fn call(
    app: &axum::Router,
    method: &str,
    path: &str,
    token: Option<&str>,
    realm: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut b = Request::builder().method(method).uri(path);
    if let Some(t) = token {
        b = b.header("authorization", format!("Bearer {t}"));
    }
    if let Some(r) = realm {
        b = b.header("x-forgebase-realm", r);
    }
    let req = match body {
        Some(v) => b
            .header("content-type", "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    };
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn signup_login(app: &axum::Router, realm: Option<&str>, email: &str) -> (String, String) {
    let (s, j) = call(
        app,
        "POST",
        "/api/auth/signup",
        None,
        realm,
        Some(json!({"email": email, "password": PW})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{j}");
    let (s, j) = call(
        app,
        "POST",
        "/api/auth/login",
        None,
        realm,
        Some(json!({"email": email, "password": PW})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{j}");
    (
        j["user"]["id"].as_str().unwrap().to_owned(),
        j["access_token"].as_str().unwrap().to_owned(),
    )
}

#[tokio::test]
async fn identities_act_in_their_own_realm_and_cross_realm_is_refused() {
    let (app, auth, _policy, _tmp) = app().await;
    auth.create_admin("root@example.com", "admin password 1", "test", &ClientInfo::default())
        .await
        .unwrap();
    let (s, j) = call(
        &app,
        "POST",
        "/api/auth/admins/login",
        None,
        None,
        Some(json!({"email": "root@example.com", "password": "admin password 1"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{j}");
    let admin = j["access_token"].as_str().unwrap().to_owned();

    // The realm through the policy API, its user pool through the auth API.
    let (s, j) = call(
        &app,
        "POST",
        "/api/v1/realms",
        Some(&admin),
        None,
        Some(json!({"id": "acme", "name": "Acme"})),
    )
    .await;
    assert!(s.is_success(), "{s} {j}");
    let (s, j) = call(
        &app,
        "PUT",
        "/api/auth/realms/acme",
        Some(&admin),
        None,
        Some(json!({})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{j}");

    // Anonymous callers may select a realm (its login page).
    let (s, j) = call(&app, "GET", "/api/auth/realm", None, Some("acme"), None).await;
    assert_eq!((s, j["realm"].as_str()), (StatusCode::OK, Some("acme")), "{j}");

    let (_, def_tok) = signup_login(&app, None, "same@example.com").await;
    let (_, acme_tok) = signup_login(&app, Some("acme"), "same@example.com").await;

    // An acme user acts in acme by default: its organization lands there.
    let (s, j) = call(
        &app,
        "POST",
        "/api/v1/orgs",
        Some(&acme_tok),
        None,
        Some(json!({"slug": "acme-co", "name": "Acme Co"})),
    )
    .await;
    assert!(s.is_success(), "{s} {j}");
    assert_eq!(j["realm_id"], "acme", "{j}");
    let (s, j) = call(&app, "GET", "/api/v1/orgs", Some(&acme_tok), None, None).await;
    assert_eq!(s, StatusCode::OK, "{j}");

    // Selecting another realm is refused for both users.
    let (s, j) = call(&app, "GET", "/api/v1/orgs", Some(&acme_tok), Some("default"), None).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{j}");
    let (s, j) = call(&app, "GET", "/api/v1/orgs", Some(&def_tok), Some("acme"), None).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{j}");
    let (s, j) = call(
        &app,
        "POST",
        "/api/v1/orgs",
        Some(&def_tok),
        Some("acme"),
        Some(json!({"slug": "x", "name": "x"})),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{j}");
    // The default user's own realm still works, and sees none of acme's orgs.
    let (s, j) = call(&app, "GET", "/api/v1/orgs", Some(&def_tok), None, None).await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert!(!j.to_string().contains("acme-co"), "{j}");
}

async fn eventually(f: impl Fn() -> bool) -> bool {
    for _ in 0..100 {
        if f() {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    false
}

#[tokio::test]
async fn suspensions_set_through_the_auth_api_reach_the_policy_engine() {
    let (app, auth, policy, _tmp) = app().await;
    auth.create_admin("root@example.com", "admin password 1", "test", &ClientInfo::default())
        .await
        .unwrap();
    let (_, j) = call(
        &app,
        "POST",
        "/api/auth/admins/login",
        None,
        None,
        Some(json!({"email": "root@example.com", "password": "admin password 1"})),
    )
    .await;
    let admin = j["access_token"].as_str().unwrap().to_owned();
    let (uid, _) = signup_login(&app, None, "s@example.com").await;
    let principal = format!("user:users/{uid}");
    let now = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    };
    let (s, j) = call(
        &app,
        "POST",
        &format!("/api/auth/users/users/{uid}/suspend"),
        Some(&admin),
        None,
        Some(json!({"reason": "x"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert!(
        eventually(|| policy.snapshot().is_suspended(&principal, now())).await,
        "policy model reloaded"
    );
    let (s, _) = call(
        &app,
        "POST",
        &format!("/api/auth/users/users/{uid}/unsuspend"),
        Some(&admin),
        None,
        Some(json!({})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(
        eventually(|| !policy.snapshot().is_suspended(&principal, now())).await,
        "lift reached the policy engine"
    );
    let (s, j) = call(
        &app,
        "POST",
        "/api/auth/login",
        None,
        None,
        Some(json!({"email": "s@example.com", "password": PW})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{j}");
    let tok = j["access_token"].as_str().unwrap().to_owned();
    let (s, j) = call(&app, "GET", "/api/v1/orgs", Some(&tok), None, None).await;
    assert_eq!(s, StatusCode::OK, "{j}");
}

/// FIX-4: one call creates the realm (policy engine) and its identity (auth).
#[tokio::test]
async fn one_call_realm_create_with_identity_settings() {
    let (app, auth, policy, _tmp) = app().await;
    auth.create_admin("root@example.com", "admin password 1", "test", &ClientInfo::default())
        .await
        .unwrap();
    let (s, j) = call(
        &app,
        "POST",
        "/api/auth/admins/login",
        None,
        None,
        Some(json!({"email": "root@example.com", "password": "admin password 1"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{j}");
    let admin = j["access_token"].as_str().unwrap().to_owned();

    let body = json!({
        "id": "acme", "name": "Acme Inc",
        "config": {"signup": true, "methods": {"phone": true}, "phone": {"signup": true}}
    });
    let (s, j) = call(&app, "POST", "/api/auth/realms", Some(&admin), None, Some(body.clone())).await;
    assert_eq!(s, StatusCode::CREATED, "{j}");
    assert_eq!(j["realm"], "acme");
    assert_eq!(j["collection"], "acme_users");
    assert_eq!(j["config"]["methods"]["phone"], true);

    // The realm exists for the policy engine and has its identity.
    let (s, j) = call(&app, "GET", "/api/v1/realms", Some(&admin), None, None).await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert!(
        j.as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == "acme" && r["name"] == "Acme Inc")
    );
    let (s, j) = call(&app, "GET", "/api/auth/realms/acme", Some(&admin), None, None).await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(j["config"]["phone"]["signup"], true);
    // The existing list route still works next to the new POST.
    let (s, j) = call(&app, "GET", "/api/auth/realms", Some(&admin), None, None).await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert!(j["items"].as_array().unwrap().iter().any(|r| r["realm"] == "acme"));
    // Users sign up inside the new realm.
    signup_login(&app, Some("acme"), "alice@example.com").await;
    let _ = policy;

    // Duplicates and bad input are refused; nothing is half-written.
    let (s, _) = call(&app, "POST", "/api/auth/realms", Some(&admin), None, Some(body)).await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, j) = call(
        &app,
        "POST",
        "/api/auth/realms",
        Some(&admin),
        None,
        Some(json!({"id": "bad", "config": {"methods": {"ldap": true}}})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{j}");
    let (_, j) = call(&app, "GET", "/api/v1/realms", Some(&admin), None, None).await;
    assert!(
        !j.as_array().unwrap().iter().any(|r| r["id"] == "bad"),
        "validated before the realm was created"
    );

    // Administrators only.
    let (_, user) = signup_login(&app, None, "kim@example.com").await;
    let (s, _) = call(
        &app,
        "POST",
        "/api/auth/realms",
        Some(&user),
        None,
        Some(json!({"id": "evil"})),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = call(
        &app,
        "POST",
        "/api/auth/realms",
        None,
        None,
        Some(json!({"id": "evil"})),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}
