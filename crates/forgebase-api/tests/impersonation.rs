//! FB-23 admin impersonation (ADR-0014 §4) with the real auth layer, the real
//! policy engine and the real router: super-admin only, reason required, no
//! admin targets, expiry, immediate revocation, audit marks on both audit
//! logs, and no escalation from inside an impersonated session.
#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use forgebase_api::{AppState, RouterOptions};
use forgebase_auth::{Argon2Config, AuthConfig, AuthService, ClientInfo};
use forgebase_core::DataLayout;
use forgebase_db::{Backend, Database, DbOptions, Statement, TxMode};
use serde_json::{Value, json};
use tower::ServiceExt;

const PW: &str = "correct horse battery";

struct Stack {
    app: Router,
    auth: AuthService,
    db: Database,
    admin_token: String,
    admin_id: String,
    user_id: String,
    _tmp: tempfile::TempDir,
}

fn ci() -> ClientInfo {
    ClientInfo {
        user_agent: Some("test".into()),
        ..ClientInfo::default()
    }
}

async fn stack() -> Stack {
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().join("data"));
    layout.ensure().unwrap();
    let cfg = forgebase_core::Config::defaults(tmp.path());
    let db = Database::open(DbOptions::from_config(&cfg.database, layout.database_path()))
        .await
        .unwrap();
    db.migrate(None).await.unwrap();
    let backend: Arc<dyn Backend> = Arc::new(db.clone());
    let policy = forgebase_policy::PolicyService::open(backend.clone()).await.unwrap();
    let mut acfg = AuthConfig::for_data_dir(layout.root());
    acfg.argon2 = Argon2Config::insecure_fast_for_tests();
    let auth = AuthService::open_with_data(backend, policy.authorizer(), layout.root(), acfg, None)
        .await
        .unwrap();
    policy.attach_data(auth.data().clone());
    auth.ensure_auth_collection("users").await.unwrap();
    let state = AppState::from_data(db.clone(), layout, auth.data().clone())
        .with_auth(auth.clone())
        .with_policy(policy);
    let app = forgebase_api::router(state, RouterOptions::default());

    let admin = auth
        .create_admin("root@example.com", "a very long admin password", "test", &ci())
        .await
        .unwrap();
    let user = auth
        .signup(&ci(), Some("users"), "alice@example.com", PW, None)
        .await
        .unwrap();
    let user_id = user["id"].as_str().unwrap().to_owned();
    let mut s = Stack {
        app,
        auth,
        db,
        admin_token: String::new(),
        admin_id: admin.id,
        user_id,
        _tmp: tmp,
    };
    let (st, j) = s
        .call(
            "POST",
            "/api/auth/admins/login",
            None,
            Some(json!({"email": "root@example.com", "password": "a very long admin password"})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{j}");
    s.admin_token = j["access_token"].as_str().unwrap().to_owned();
    s
}

impl Stack {
    async fn call(&self, method: &str, path: &str, token: Option<&str>, body: Option<Value>) -> (StatusCode, Value) {
        let mut b = Request::builder().method(method).uri(path);
        if let Some(t) = token {
            b = b.header("authorization", format!("Bearer {t}"));
        }
        let req = match body {
            Some(v) => b
                .header("content-type", "application/json")
                .body(Body::from(v.to_string()))
                .unwrap(),
            None => b.body(Body::empty()).unwrap(),
        };
        let res = self.app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    fn target(&self) -> String {
        format!("user:users/{}", self.user_id)
    }

    async fn impersonate(&self, reason: &str, ttl: u32) -> Value {
        let (st, j) = self
            .call(
                "POST",
                "/api/auth/impersonate",
                Some(&self.admin_token),
                Some(json!({"principal": self.target(), "reason": reason, "ttl_minutes": ttl})),
            )
            .await;
        assert_eq!(st, StatusCode::OK, "{j}");
        j
    }
}

#[tokio::test]
async fn only_a_super_admin_session_may_impersonate() {
    let s = stack().await;
    let body = json!({"principal": s.target(), "reason": "support ticket 42"});

    // Anonymous.
    let (st, _) = s.call("POST", "/api/auth/impersonate", None, Some(body.clone())).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);

    // A plain user.
    let (st, j) = s
        .call(
            "POST",
            "/api/auth/login",
            None,
            Some(json!({"collection": "users", "email": "alice@example.com", "password": PW})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{j}");
    let user_token = j["access_token"].as_str().unwrap();
    let (st, j) = s
        .call("POST", "/api/auth/impersonate", Some(user_token), Some(body.clone()))
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{j}");

    // An admin-role service account key is not an interactive admin session.
    let admin_ctx = forgebase_auth::Authenticated {
        subject: forgebase_auth::Subject::Admin {
            id: s.admin_id.clone(),
        },
        credential: forgebase_auth::CredentialKind::AccessToken,
        credential_id: "x".into(),
        scopes: None,
        mfa: true,
        impersonator: None,
    };
    let sa = s
        .auth
        .create_service_account(&admin_ctx, &ci(), "robot", "", "admin")
        .await
        .unwrap();
    let key = s
        .auth
        .create_api_key(&admin_ctx, &ci(), &sa.id, "k", &["auth:*".to_owned()], None)
        .await
        .unwrap();
    let (st, j) = s
        .call("POST", "/api/auth/impersonate", Some(&key.key), Some(body.clone()))
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{j}");

    // The administrator may.
    s.impersonate("support ticket 42", 5).await;
}

#[tokio::test]
async fn reason_target_and_ttl_are_validated() {
    let s = stack().await;
    let post = |body: Value| {
        let s = &s;
        async move {
            s.call("POST", "/api/auth/impersonate", Some(&s.admin_token), Some(body))
                .await
        }
    };
    // Reason required.
    let (st, j) = post(json!({"principal": s.target(), "reason": "  "})).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{j}");
    let (st, _) = post(json!({"principal": s.target()})).await;
    assert!(st.is_client_error());
    // TTL bounds.
    for ttl in [0, 61] {
        let (st, j) = post(json!({"principal": s.target(), "reason": "because", "ttl_minutes": ttl})).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{j}");
    }
    // Never another super-admin, nor a service account.
    for p in [format!("admin:{}", s.admin_id), "service:robot".to_owned()] {
        let (st, j) = post(json!({"principal": p, "reason": "because"})).await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{j}");
        assert!(
            j["error"]["message"].as_str().unwrap().contains("cannot_impersonate_admin"),
            "{j}"
        );
    }
    // Unknown user, malformed principal.
    let (st, _) = post(json!({"principal": "user:users/nobody", "reason": "because"})).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = post(json!({"principal": "alice", "reason": "because"})).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn an_impersonated_session_is_the_user_not_an_admin_and_cannot_escalate() {
    let s = stack().await;
    let g = s.impersonate("debugging checkout", 10).await;
    let tok = g["access_token"].as_str().unwrap();
    assert_eq!(g["impersonator"], s.admin_id.as_str());
    assert!(g.get("refresh_token").is_none());

    // /me shows the user, marked as impersonated.
    let (st, me) = s.call("GET", "/api/auth/me", Some(tok), None).await;
    assert_eq!(st, StatusCode::OK, "{me}");
    assert_eq!(me["subject"]["type"], "user");
    assert_eq!(me["subject"]["id"], s.user_id.as_str());
    assert_eq!(me["impersonator"], s.admin_id.as_str());
    assert_eq!(me["impersonation_reason"], "debugging checkout");

    // Not privileged: admin routes and super-admin policy routes refuse it.
    for (m, p) in [
        ("GET", "/api/auth/admins"),
        ("GET", "/api/auth/audit"),
        ("GET", "/api/v1/collections"),
        ("GET", "/api/v1/realms"),
        ("GET", "/api/v1/policies/users"),
    ] {
        let (st, j) = s.call(m, p, Some(tok), None).await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{m} {p}: {j}");
    }
    // No nested impersonation, no credential changes, no new keys.
    let nested = json!({"principal": s.target(), "reason": "again"});
    let (st, _) = s.call("POST", "/api/auth/impersonate", Some(tok), Some(nested)).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    for (m, p, b) in [
        (
            "POST",
            "/api/auth/password/change",
            json!({"current_password": PW, "new_password": "another long password"}),
        ),
        ("POST", "/api/auth/tokens", json!({"name": "mine", "scopes": ["collections:read"]})),
        ("POST", "/api/auth/mfa/totp/setup", json!({})),
        ("POST", "/api/auth/refresh", json!({"refresh_token": "nope"})),
    ] {
        let (st, j) = s.call(m, p, Some(tok), Some(b)).await;
        assert!(st == StatusCode::FORBIDDEN || st == StatusCode::UNAUTHORIZED, "{p}: {st} {j}");
    }
    let (_, j) = s
        .call(
            "POST",
            "/api/auth/password/change",
            Some(tok),
            Some(json!({"current_password": PW, "new_password": "another long password"})),
        )
        .await;
    assert!(j["error"]["message"].as_str().unwrap().contains("impersonated_session"), "{j}");

    // The user's own password still works (nothing was changed).
    let (st, _) = s
        .call(
            "POST",
            "/api/auth/login",
            None,
            Some(json!({"collection": "users", "email": "alice@example.com", "password": PW})),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
}

#[tokio::test]
async fn expiry_and_end_take_effect_immediately() {
    let s = stack().await;

    // Expiry: the session row is the authority, whatever the token says.
    let g = s.impersonate("expiring", 5).await;
    let tok = g["access_token"].as_str().unwrap().to_owned();
    let sid = g["session_id"].as_str().unwrap().to_owned();
    let (st, _) = s.call("GET", "/api/auth/me", Some(&tok), None).await;
    assert_eq!(st, StatusCode::OK);
    let mut tx = s.db.begin(TxMode::Normal).await.unwrap();
    tx.execute(&Statement::new(
        "UPDATE _forgebase_auth_sessions SET expires_at = 1 WHERE id = ?1",
        vec![sid.clone().into()],
    ))
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let (st, j) = s.call("GET", "/api/auth/me", Some(&tok), None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "{j}");

    // Ended by an administrator.
    let g = s.impersonate("ended by admin", 5).await;
    let tok = g["access_token"].as_str().unwrap().to_owned();
    let sid = g["session_id"].as_str().unwrap().to_owned();
    let (st, _) = s
        .call(
            "POST",
            &format!("/api/auth/impersonations/{sid}/end"),
            Some(&s.admin_token),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (st, _) = s.call("GET", "/api/auth/me", Some(&tok), None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);

    // Ended by the impersonated session itself (the banner's button).
    let g = s.impersonate("self end", 5).await;
    let tok = g["access_token"].as_str().unwrap().to_owned();
    let (st, _) = s.call("POST", "/api/auth/logout", Some(&tok), None).await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (st, _) = s.call("GET", "/api/auth/me", Some(&tok), None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);

    // A user cannot end somebody else's impersonation.
    let g = s.impersonate("kept", 5).await;
    let sid = g["session_id"].as_str().unwrap().to_owned();
    let (_, j) = s
        .call(
            "POST",
            "/api/auth/login",
            None,
            Some(json!({"collection": "users", "email": "alice@example.com", "password": PW})),
        )
        .await;
    let user_tok = j["access_token"].as_str().unwrap();
    let (st, _) = s
        .call("POST", &format!("/api/auth/impersonations/{sid}/end"), Some(user_tok), None)
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    // The administrator's list shows them all, with active flags.
    let (st, j) = s
        .call("GET", "/api/auth/impersonations", Some(&s.admin_token), None)
        .await;
    assert_eq!(st, StatusCode::OK);
    let items = j["items"].as_array().unwrap();
    assert_eq!(items.len(), 4, "{j}");
    assert_eq!(items.iter().filter(|i| i["active"] == true).count(), 1, "{j}");
    assert!(items.iter().all(|i| i["admin_id"] == s.admin_id.as_str()));
    assert!(items.iter().any(|i| i["reason"] == "kept" && i["target"] == s.target().as_str()));
}

#[tokio::test]
async fn every_audit_row_written_while_impersonating_names_the_administrator() {
    let s = stack().await;
    let g = s.impersonate("audit check", 10).await;
    let tok = g["access_token"].as_str().unwrap().to_owned();

    // A denial by the policy engine (authorization audit) ...
    let (st, _) = s.call("GET", "/api/v1/policies/users", Some(&tok), None).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    // ... and an authentication event (authentication audit): the end.
    let (st, _) = s.call("POST", "/api/auth/logout", Some(&tok), None).await;
    assert_eq!(st, StatusCode::NO_CONTENT);

    let (st, j) = s
        .call("GET", "/api/auth/audit?limit=100", Some(&s.admin_token), None)
        .await;
    assert_eq!(st, StatusCode::OK);
    let rows = j["items"].as_array().unwrap();
    let find = |event: &str| rows.iter().find(|r| r["event"] == event).cloned();
    let start = find("impersonation.start").expect("start is audited");
    assert_eq!(start["actor"], format!("admin:{}", s.admin_id));
    assert_eq!(start["detail"]["reason"], "audit check");
    assert_eq!(start["subject_id"], s.user_id.as_str());
    let end = find("impersonation.end").expect("end is audited");
    assert_eq!(end["impersonator"], s.admin_id.as_str());
    assert_eq!(end["actor"], format!("user:users/{}", s.user_id));
    // Ordinary rows have no impersonator.
    let login = rows.iter().find(|r| r["event"] == "login").unwrap();
    assert!(login["impersonator"].is_null());

    // The authorization audit is written in the background.
    let mut mark = Value::Null;
    for _ in 0..60 {
        let (st, j) = s
            .call("GET", "/api/v1/authz/audit?limit=100", Some(&s.admin_token), None)
            .await;
        assert_eq!(st, StatusCode::OK, "{j}");
        if let Some(r) = j
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["principal"] == format!("user:users/{}", s.user_id).as_str())
        {
            mark = r.clone();
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(mark["impersonator"], s.admin_id.as_str(), "{mark}");
    assert_eq!(mark["step"], "permission");
}

#[tokio::test]
async fn admin_user_routes_show_sessions_history_mfa_and_revoke_all() {
    let s = stack().await;
    let (_, j) = s
        .call(
            "POST",
            "/api/auth/login",
            None,
            Some(json!({"collection": "users", "email": "alice@example.com", "password": PW, "device_name": "laptop"})),
        )
        .await;
    let user_tok = j["access_token"].as_str().unwrap().to_owned();
    let base = format!("/api/auth/users/users/{}", s.user_id);

    let (st, d) = s.call("GET", &base, Some(&s.admin_token), None).await;
    assert_eq!(st, StatusCode::OK, "{d}");
    assert_eq!(d["user"]["email"], "alice@example.com");
    assert_eq!(d["mfa_enabled"], false);
    assert_eq!(d["active_sessions"], 1);
    assert!(d["last_login_at"].is_string());

    let (st, l) = s
        .call("GET", &format!("{base}/sessions"), Some(&s.admin_token), None)
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(l["items"][0]["device_name"], "laptop");
    let (st, h) = s
        .call("GET", &format!("{base}/login-history"), Some(&s.admin_token), None)
        .await;
    assert_eq!(st, StatusCode::OK);
    assert!(h["items"].as_array().unwrap().iter().any(|e| e["event"] == "login"));

    // Users cannot read each other's admin view.
    for p in [base.clone(), format!("{base}/sessions"), format!("{base}/login-history")] {
        let (st, _) = s.call("GET", &p, Some(&user_tok), None).await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{p}");
    }
    let (st, _) = s
        .call("POST", &format!("{base}/sessions/revoke"), Some(&user_tok), None)
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    // Revoke all.
    let (st, r) = s
        .call("POST", &format!("{base}/sessions/revoke"), Some(&s.admin_token), None)
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(r["revoked"], 1);
    let (st, _) = s.call("GET", "/api/auth/me", Some(&user_tok), None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);

    // The key list shows every key and PAT.
    let (st, k) = s
        .call("GET", "/api/auth/api-keys", Some(&s.admin_token), None)
        .await;
    assert_eq!(st, StatusCode::OK);
    assert!(k["items"].is_array());
}
