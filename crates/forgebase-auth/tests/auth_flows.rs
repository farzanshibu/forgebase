//! End-to-end tests of the auth core over HTTP (in-process router, real
//! SQLite database in a temp dir, dev mailer on disk).
//!
//! SPEC §27 #7 (authenticate a user) and #8 (reject unauthorized requests),
//! plus refresh-token reuse, lockout, expiry and revocation, MFA, API-key
//! scopes, cookie/CSRF mode and secret hygiene.

#![allow(clippy::unwrap_used, missing_docs)]

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{Extension, State};
use axum::http::{HeaderMap, Method, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use forgebase_auth::crypto::{base32_decode, hotp};
use forgebase_auth::data::{AccessContext, DataService, DefaultAuthorizer};
use forgebase_auth::{Argon2Config, AuthConfig, AuthService, ClientInfo, DevMailer, MailKind};
use forgebase_core::ErrorCode;
use forgebase_db::{Database, DbOptions};
use serde_json::{Value, json};
use tower::ServiceExt;

const PW: &str = "correct horse battery";

struct H {
    app: Router,
    auth: AuthService,
    mail: DevMailer,
    db: Database,
    dir: tempfile::TempDir,
}

async fn probe(State(data): State<DataService>, Extension(ctx): Extension<AccessContext>) -> Response {
    // A data-layer call with the request's context: the default authorizer
    // fails closed (anonymous → 401, users → 403, administrators → 200).
    match data.list_collections(&ctx).await {
        Ok(c) => (
            StatusCode::OK,
            axum::Json(json!({"principal": format!("{:?}", ctx.principal), "collections": c.len()})),
        )
            .into_response(),
        Err(e) => {
            let status = match e.kind() {
                forgebase_core::ErrorKind::Unauthenticated => StatusCode::UNAUTHORIZED,
                forgebase_core::ErrorKind::Forbidden => StatusCode::FORBIDDEN,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            (status, axum::Json(json!({"error": {"code": e.code()}}))).into_response()
        }
    }
}

fn test_config(dir: &std::path::Path) -> AuthConfig {
    let mut cfg = AuthConfig::for_data_dir(dir);
    cfg.argon2 = Argon2Config::insecure_fast_for_tests();
    cfg.trust_forwarded_for = true;
    cfg.cookie_secure = false;
    cfg
}

async fn open_db(dir: &std::path::Path) -> Database {
    let cfg = forgebase_core::Config::defaults(dir);
    let db = Database::open(DbOptions::from_config(&cfg.database, dir.join("forgebase.db")))
        .await
        .unwrap();
    db.migrate(None).await.unwrap();
    db
}

async fn harness_in(dir: tempfile::TempDir, tweak: impl FnOnce(&mut AuthConfig)) -> H {
    let db = open_db(dir.path()).await;
    let mut cfg = test_config(dir.path());
    tweak(&mut cfg);
    let auth = AuthService::open_with_data(Arc::new(db.clone()), Arc::new(DefaultAuthorizer), dir.path(), cfg, None)
        .await
        .unwrap();
    let probe_routes = Router::new()
        .route("/api/v1/collections/{name}/records", get(probe).post(probe))
        .with_state(auth.data().clone());
    let app = Router::new()
        .merge(forgebase_auth::http::router(auth.clone()))
        .merge(probe_routes)
        .layer(axum::middleware::from_fn_with_state(
            auth.clone(),
            forgebase_auth::http::authenticate,
        ));
    H {
        app,
        auth,
        mail: DevMailer::new(dir.path().join("mail")),
        db,
        dir,
    }
}

async fn harness(tweak: impl FnOnce(&mut AuthConfig)) -> H {
    harness_in(tempfile::tempdir().unwrap(), tweak).await
}

struct Resp {
    status: StatusCode,
    headers: HeaderMap,
    json: Value,
}

impl H {
    async fn req(
        &self,
        method: Method,
        path: &str,
        bearer: Option<&str>,
        body: Option<Value>,
        headers: &[(&str, &str)],
    ) -> Resp {
        let mut b = Request::builder()
            .method(method)
            .uri(path)
            .header("x-request-id", "test-req");
        if !headers.iter().any(|(k, _)| *k == "x-forwarded-for") {
            b = b.header("x-forwarded-for", "10.0.0.1");
        }
        if let Some(t) = bearer {
            b = b.header("authorization", format!("Bearer {t}"));
        }
        for (k, v) in headers {
            b = b.header(*k, *v);
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
        let headers = res.headers().clone();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
        Resp {
            status,
            headers,
            json: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        }
    }

    async fn post(&self, path: &str, bearer: Option<&str>, body: Value) -> Resp {
        self.req(Method::POST, path, bearer, Some(body), &[]).await
    }

    async fn get(&self, path: &str, bearer: Option<&str>) -> Resp {
        self.req(Method::GET, path, bearer, None, &[]).await
    }

    async fn signup(&self, email: &str) -> Value {
        let r = self
            .post("/api/auth/signup", None, json!({"email": email, "password": PW}))
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
        r.json["user"].clone()
    }

    async fn login(&self, email: &str) -> Value {
        let r = self
            .post("/api/auth/login", None, json!({"email": email, "password": PW}))
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.json);
        r.json
    }

    async fn admin_token(&self) -> String {
        self.auth
            .create_admin("root@example.com", "admin password 1", "test", &ClientInfo::default())
            .await
            .unwrap();
        let r = self
            .post(
                "/api/auth/admins/login",
                None,
                json!({"email": "root@example.com", "password": "admin password 1"}),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.json);
        r.json["access_token"].as_str().unwrap().to_owned()
    }
}

fn s(v: &Value) -> &str {
    v.as_str().unwrap()
}

fn assert_error(r: &Resp, status: StatusCode, code: &str) {
    assert_eq!(r.status, status, "{}", r.json);
    assert_eq!(r.json["error"]["code"], code, "{}", r.json);
    assert_eq!(r.json["error"]["request_id"], "test-req");
}

// ---------------------------------------------------------------------------
// SPEC §27 #7 — authenticate a user
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scenario_7_authenticate_a_user() {
    let h = harness(|_| {}).await;
    let user = h.signup("Alice@Example.com").await;
    assert_eq!(user["email"], "alice@example.com", "emails are normalised");
    assert_eq!(user["verified"], false);
    assert!(
        user.get("password").is_none() && user.get("token_key").is_none(),
        "{user}"
    );

    // Verification mail → confirm → verified persisted in the user record.
    let mail = h.mail.last("alice@example.com", MailKind::VerifyEmail).unwrap();
    assert!(mail.link.as_deref().unwrap().contains(&mail.secret));
    let r = h
        .post("/api/auth/verify-email/confirm", None, json!({"token": mail.secret}))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["user"]["verified"], true);
    let r = h
        .post("/api/auth/verify-email/confirm", None, json!({"token": mail.secret}))
        .await;
    assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_token");

    let login = h.login("ALICE@example.com").await;
    assert_eq!(login["token_type"], "Bearer");
    assert_eq!(login["expires_in"], 900);
    assert_eq!(login["user"]["id"], user["id"]);
    let access = s(&login["access_token"]).to_owned();

    let me = h.get("/api/auth/me", Some(&access)).await;
    assert_eq!(me.status, StatusCode::OK, "{}", me.json);
    assert_eq!(
        me.json["subject"],
        json!({"type": "user", "collection": "users", "id": user["id"]})
    );
    assert_eq!(me.json["credential"], "access_token");
    assert_eq!(me.json["user"]["email"], "alice@example.com");

    // The data layer receives the user's AccessContext.
    let p = h.get("/api/v1/collections/users/records", Some(&access)).await;
    assert_eq!(
        p.status,
        StatusCode::FORBIDDEN,
        "default authorizer denies users: {}",
        p.json
    );

    // Sessions and login history.
    let sessions = h.get("/api/auth/sessions", Some(&access)).await;
    assert_eq!(sessions.json["items"].as_array().unwrap().len(), 1);
    assert_eq!(sessions.json["items"][0]["current"], true);
    let hist = h.get("/api/auth/login-history", Some(&access)).await;
    assert_eq!(hist.json["items"][0]["event"], "login");
    assert_eq!(hist.json["items"][0]["ip"], "10.0.0.1");

    // Restart: a new service on the same data directory accepts the token
    // (signing keys and master key are persisted) and the user persists.
    let dir = h.dir;
    h.db.close().await;
    drop(h.app);
    drop(h.auth);
    let h2 = harness_in(dir, |_| {}).await;
    let me = h2.get("/api/auth/me", Some(&access)).await;
    assert_eq!(me.status, StatusCode::OK, "{}", me.json);
    assert_eq!(me.json["user"]["verified"], true);
    h2.login("alice@example.com").await;
}

#[tokio::test]
async fn signup_validation() {
    let h = harness(|_| {}).await;
    let r = h
        .post(
            "/api/auth/signup",
            None,
            json!({"email": "a@x.io", "password": "short"}),
        )
        .await;
    assert_error(&r, StatusCode::BAD_REQUEST, "invalid_input");
    let r = h
        .post("/api/auth/signup", None, json!({"email": "nope", "password": PW}))
        .await;
    assert_error(&r, StatusCode::BAD_REQUEST, "invalid_input");
    let r = h
        .post(
            "/api/auth/signup",
            None,
            json!({"email": "a@x.io", "password": PW, "data": {"verified": true}}),
        )
        .await;
    assert_error(&r, StatusCode::BAD_REQUEST, "invalid_input");
    h.signup("a@x.io").await;
    let r = h
        .post("/api/auth/signup", None, json!({"email": "A@X.io", "password": PW}))
        .await;
    assert_error(&r, StatusCode::CONFLICT, "email_taken");
    let r = h
        .post(
            "/api/auth/signup",
            None,
            json!({"email": "b@x.io", "password": PW, "collection": "nope"}),
        )
        .await;
    assert_error(&r, StatusCode::NOT_FOUND, "not_found");
    let r = h.req(Method::POST, "/api/auth/signup", None, None, &[]).await;
    assert_error(&r, StatusCode::BAD_REQUEST, "invalid_input");
}

// ---------------------------------------------------------------------------
// SPEC §27 #8 — reject unauthorized requests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scenario_8_reject_unauthorized_requests() {
    let h = harness(|_| {}).await;
    h.signup("bob@example.com").await;

    // No credentials: protected auth endpoints 401, data layer 401.
    let r = h.get("/api/auth/me", None).await;
    assert_error(&r, StatusCode::UNAUTHORIZED, "unauthenticated");
    assert_eq!(r.headers["www-authenticate"], "Bearer");
    let p = h.get("/api/v1/collections/users/records", None).await;
    assert_eq!(p.status, StatusCode::UNAUTHORIZED, "{}", p.json);

    // Garbage, malformed and forged credentials are rejected, never downgraded to anonymous.
    for bad in [
        "garbage",
        "a.b.c",
        "fbk_short",
        "fbp_aaaaaaaaaaaaaaaa_wrongsecretwrongsecret",
    ] {
        let r = h.get("/api/v1/collections/users/records", Some(bad)).await;
        assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_token");
        assert!(
            r.headers["www-authenticate"]
                .to_str()
                .unwrap()
                .contains("invalid_token")
        );
    }
    let r = h
        .req(
            Method::GET,
            "/api/auth/me",
            None,
            None,
            &[("authorization", "Basic YTpi")],
        )
        .await;
    assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_token");

    let login = h.login("bob@example.com").await;
    let token = s(&login["access_token"]).to_owned();
    // Forged payload with the original signature.
    let parts: Vec<&str> = token.split('.').collect();
    let forged_payload = forgebase_auth::crypto::b64url(
        br#"{"iss":"forgebase","typ":"access","st":"admin","sub":"x","sid":"y","iat":0,"exp":99999999999,"jti":"j","amr":[]}"#,
    );
    let forged = format!("{}.{}.{}", parts[0], forged_payload, parts[2]);
    let r = h.get("/api/auth/me", Some(&forged)).await;
    assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_token");
    // Signature flipped.
    let mut sig = parts[2].to_owned();
    let last = sig.pop().unwrap();
    sig.push(if last == 'A' { 'B' } else { 'A' });
    let r = h
        .get("/api/auth/me", Some(&format!("{}.{}.{sig}", parts[0], parts[1])))
        .await;
    assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_token");

    // Wrong password and unknown account: identical answers.
    let wrong = h
        .post(
            "/api/auth/login",
            None,
            json!({"email": "bob@example.com", "password": "wrong password"}),
        )
        .await;
    let unknown = h
        .post(
            "/api/auth/login",
            None,
            json!({"email": "nobody@example.com", "password": "wrong password"}),
        )
        .await;
    assert_error(&wrong, StatusCode::UNAUTHORIZED, "invalid_credentials");
    assert_eq!(wrong.json["error"]["message"], unknown.json["error"]["message"]);
    assert_eq!(wrong.status, unknown.status);

    // A user cannot use administrator endpoints.
    for path in ["/api/auth/service-accounts", "/api/auth/audit", "/api/auth/admins"] {
        let r = h.get(path, Some(&token)).await;
        assert_error(&r, StatusCode::FORBIDDEN, "forbidden");
    }
    // An administrator can, and the data layer accepts them.
    let admin = h.admin_token().await;
    let r = h.get("/api/auth/audit?event=login&limit=5", Some(&admin)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert!(r.json["items"].as_array().unwrap().len() <= 5);
    let p = h.get("/api/v1/collections/users/records", Some(&admin)).await;
    assert_eq!(p.status, StatusCode::OK, "{}", p.json);
    assert!(s(&p.json["principal"]).starts_with("Admin"));

    // Failed attempts are audited.
    let audit = h.get("/api/auth/audit?event=login", Some(&admin)).await;
    assert!(
        audit.json["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["outcome"] == "failure" && e["detail"]["reason"] == "invalid_credentials")
    );
}

// ---------------------------------------------------------------------------
// Refresh tokens
// ---------------------------------------------------------------------------

#[tokio::test]
async fn refresh_rotates_and_reuse_revokes_the_family() {
    let h = harness(|_| {}).await;
    h.signup("carol@example.com").await;
    let login = h.login("carol@example.com").await;
    let r1 = s(&login["refresh_token"]).to_owned();

    let a = h.post("/api/auth/refresh", None, json!({"refresh_token": r1})).await;
    assert_eq!(a.status, StatusCode::OK, "{}", a.json);
    let r2 = s(&a.json["refresh_token"]).to_owned();
    let access2 = s(&a.json["access_token"]).to_owned();
    assert_ne!(r1, r2);
    assert_eq!(a.json["session_id"], login["session_id"]);
    assert_eq!(h.get("/api/auth/me", Some(&access2)).await.status, StatusCode::OK);

    // Replaying r1 (already rotated) = theft: the whole session is revoked.
    let replay = h.post("/api/auth/refresh", None, json!({"refresh_token": r1})).await;
    assert_error(&replay, StatusCode::UNAUTHORIZED, "invalid_token");
    assert!(s(&replay.json["error"]["message"]).contains("reuse"));
    // The legitimate newest token and its access token die with it.
    let r = h.post("/api/auth/refresh", None, json!({"refresh_token": r2})).await;
    assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_token");
    let r = h.get("/api/auth/me", Some(&access2)).await;
    assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_token");
    assert_eq!(r.json["error"]["message"], "session revoked");

    // Unknown refresh tokens are rejected.
    let r = h
        .post("/api/auth/refresh", None, json!({"refresh_token": "nope"}))
        .await;
    assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_token");

    let admin = h.admin_token().await;
    let audit = h
        .get("/api/auth/audit?event=refresh.reuse_detected", Some(&admin))
        .await;
    assert_eq!(audit.json["items"].as_array().unwrap().len(), 1, "{}", audit.json);
}

// ---------------------------------------------------------------------------
// Lockout and rate limits
// ---------------------------------------------------------------------------

#[tokio::test]
async fn lockout_after_n_failures() {
    let h = harness(|c| c.lockout_threshold = 5).await;
    h.signup("dave@example.com").await;
    let bad = json!({"email": "dave@example.com", "password": "not the password"});
    for n in 0..5 {
        // Different IPs: the lock is per account, not per address.
        let ip = format!("10.1.0.{n}");
        let r = h
            .req(
                Method::POST,
                "/api/auth/login",
                None,
                Some(bad.clone()),
                &[("x-forwarded-for", &ip)],
            )
            .await;
        assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_credentials");
    }
    // Now even the right password is refused until the lock expires.
    let r = h
        .post(
            "/api/auth/login",
            None,
            json!({"email": "dave@example.com", "password": PW}),
        )
        .await;
    assert_error(&r, StatusCode::TOO_MANY_REQUESTS, "too_many_requests");
    let retry: u64 = r.headers["retry-after"].to_str().unwrap().parse().unwrap();
    assert!(retry > 800 && retry <= 900, "{retry}");

    // Unknown accounts lock the same way (no enumeration through lockout).
    let ghost = json!({"email": "ghost@example.com", "password": "x"});
    for _ in 0..5 {
        h.post("/api/auth/login", None, ghost.clone()).await;
    }
    let r = h.post("/api/auth/login", None, ghost).await;
    assert_error(&r, StatusCode::TOO_MANY_REQUESTS, "too_many_requests");

    // The lock is persisted (survives a restart).
    let dir = h.dir;
    h.db.close().await;
    let h2 = harness_in(dir, |_| {}).await;
    let r = h2
        .post(
            "/api/auth/login",
            None,
            json!({"email": "dave@example.com", "password": PW}),
        )
        .await;
    assert_error(&r, StatusCode::TOO_MANY_REQUESTS, "too_many_requests");

    // A password reset (mailbox control) clears it.
    let r = h2
        .post(
            "/api/auth/password-reset/request",
            None,
            json!({"email": "dave@example.com"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED);
    let mail = h2.mail.last("dave@example.com", MailKind::PasswordReset).unwrap();
    let r = h2
        .post(
            "/api/auth/password-reset/confirm",
            None,
            json!({"token": mail.secret, "password": "brand new password"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.json);
    let r = h2
        .post(
            "/api/auth/login",
            None,
            json!({"email": "dave@example.com", "password": "brand new password"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
}

#[tokio::test]
async fn per_ip_rate_limit() {
    let h = harness(|c| c.ip_attempts_per_minute = 3).await;
    let body = json!({"email": "x@example.com", "password": "whatever"});
    for _ in 0..3 {
        let r = h.post("/api/auth/login", None, body.clone()).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    }
    let r = h.post("/api/auth/login", None, body.clone()).await;
    assert_error(&r, StatusCode::TOO_MANY_REQUESTS, "too_many_requests");
    assert!(r.headers.contains_key("retry-after"));
    // Another client address is not affected.
    let r = h
        .req(
            Method::POST,
            "/api/auth/login",
            None,
            Some(body),
            &[("x-forwarded-for", "10.9.9.9")],
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------------------------
// Expiry and revocation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn expired_and_revoked_tokens_are_rejected() {
    let h = harness(|c| c.access_token_ttl = Duration::from_secs(1)).await;
    h.signup("erin@example.com").await;
    let a = h.login("erin@example.com").await;
    let b = h.login("erin@example.com").await;
    let (ta, tb) = (s(&a["access_token"]).to_owned(), s(&b["access_token"]).to_owned());
    assert_eq!(h.get("/api/auth/me", Some(&ta)).await.status, StatusCode::OK);

    tokio::time::sleep(Duration::from_millis(2100)).await;
    let r = h.get("/api/auth/me", Some(&ta)).await;
    assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_token");
    assert_eq!(r.json["error"]["message"], "access token expired");

    // Fresh tokens through refresh.
    let ra = h
        .post("/api/auth/refresh", None, json!({"refresh_token": a["refresh_token"]}))
        .await;
    let rb = h
        .post("/api/auth/refresh", None, json!({"refresh_token": b["refresh_token"]}))
        .await;
    let (ta, tb2) = (
        s(&ra.json["access_token"]).to_owned(),
        s(&rb.json["access_token"]).to_owned(),
    );
    let _ = tb;

    // Session A revokes session B from the device list.
    let r = h
        .req(
            Method::DELETE,
            &format!("/api/auth/sessions/{}", s(&b["session_id"])),
            Some(&ta),
            None,
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.json);
    let r = h.get("/api/auth/me", Some(&tb2)).await;
    assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_token");
    assert_eq!(r.json["error"]["message"], "session revoked");
    let r = h
        .post(
            "/api/auth/refresh",
            None,
            json!({"refresh_token": rb.json["refresh_token"]}),
        )
        .await;
    assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_token");

    // Logout revokes the current session and its refresh token.
    let r = h.post("/api/auth/logout", Some(&ta), json!({})).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(h.get("/api/auth/me", Some(&ta)).await.status, StatusCode::UNAUTHORIZED);
    let r = h
        .post(
            "/api/auth/refresh",
            None,
            json!({"refresh_token": ra.json["refresh_token"]}),
        )
        .await;
    assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_token");

    // A user cannot revoke someone else's session.
    h.signup("frank@example.com").await;
    let f = h.login("frank@example.com").await;
    let g = h.login("erin@example.com").await;
    let r = h
        .req(
            Method::DELETE,
            &format!("/api/auth/sessions/{}", s(&f["session_id"])),
            Some(s(&g["access_token"])),
            None,
            &[],
        )
        .await;
    assert_error(&r, StatusCode::NOT_FOUND, "not_found");

    // Expired one-time links are rejected.
    h.post(
        "/api/auth/magic-link/request",
        None,
        json!({"email": "erin@example.com"}),
    )
    .await;
    let mail = h.mail.last("erin@example.com", MailKind::MagicLink).unwrap();
    expire_one_time_tokens(&h).await;
    let r = h
        .post("/api/auth/magic-link/confirm", None, json!({"token": mail.secret}))
        .await;
    assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_token");
}

/// Expires every one-time token through the storage backend.
async fn expire_one_time_tokens(h: &H) {
    use forgebase_db::{Backend, Statement, TxMode};
    let mut tx = h.db.begin(TxMode::Normal).await.unwrap();
    tx.execute(&Statement::raw("UPDATE _forgebase_auth_one_time SET expires_at = 0"))
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

// ---------------------------------------------------------------------------
// MFA
// ---------------------------------------------------------------------------

fn totp(secret_b32: &str, offset_steps: i64) -> String {
    let secret = base32_decode(secret_b32).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    hotp(&secret, (now / 30 + offset_steps) as u64)
}

#[tokio::test]
async fn mfa_is_required_when_enabled() {
    let h = harness(|_| {}).await;
    h.signup("gina@example.com").await;
    let login = h.login("gina@example.com").await;
    let token = s(&login["access_token"]).to_owned();

    let setup = h.post("/api/auth/mfa/totp/setup", Some(&token), json!({})).await;
    assert_eq!(setup.status, StatusCode::OK, "{}", setup.json);
    let secret = s(&setup.json["secret"]).to_owned();
    assert!(s(&setup.json["otpauth_url"]).starts_with("otpauth://totp/ForgeBase:"));
    // Not enabled until confirmed: login still issues tokens directly.
    assert!(h.login("gina@example.com").await["access_token"].is_string());

    let bad = h
        .post("/api/auth/mfa/totp/confirm", Some(&token), json!({"code": "000000"}))
        .await;
    assert_error(&bad, StatusCode::UNAUTHORIZED, "invalid_credentials");
    let ok = h
        .post(
            "/api/auth/mfa/totp/confirm",
            Some(&token),
            json!({"code": totp(&secret, 0)}),
        )
        .await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.json);
    let codes: Vec<String> = serde_json::from_value(ok.json["recovery_codes"].clone()).unwrap();
    assert_eq!(codes.len(), 10);

    // Password alone no longer signs in.
    let first = h
        .post(
            "/api/auth/login",
            None,
            json!({"email": "gina@example.com", "password": PW}),
        )
        .await;
    assert_eq!(first.status, StatusCode::OK);
    assert_eq!(first.json["mfa_required"], true);
    assert!(first.json.get("access_token").is_none() && first.json.get("refresh_token").is_none());
    let challenge = s(&first.json["mfa_token"]).to_owned();
    // The challenge is not a credential.
    assert_eq!(
        h.get("/api/auth/me", Some(&challenge)).await.status,
        StatusCode::UNAUTHORIZED
    );

    let r = h
        .post(
            "/api/auth/mfa/verify",
            None,
            json!({"mfa_token": challenge, "code": "123456"}),
        )
        .await;
    assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_credentials");
    // The next step's code (skew window) completes sign-in.
    let code = totp(&secret, 1);
    let r = h
        .post(
            "/api/auth/mfa/verify",
            None,
            json!({"mfa_token": challenge, "code": code}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let mfa_token = s(&r.json["access_token"]).to_owned();
    let me = h.get("/api/auth/me", Some(&mfa_token)).await;
    assert_eq!(me.json["mfa"], true);
    assert_eq!(me.json["mfa_enabled"], true);
    // The challenge is single use.
    let r = h
        .post(
            "/api/auth/mfa/verify",
            None,
            json!({"mfa_token": challenge, "code": code}),
        )
        .await;
    assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_token");

    // Replaying the same TOTP code on a new challenge fails.
    let second = h
        .post(
            "/api/auth/login",
            None,
            json!({"email": "gina@example.com", "password": PW}),
        )
        .await;
    let c2 = s(&second.json["mfa_token"]).to_owned();
    let r = h
        .post("/api/auth/mfa/verify", None, json!({"mfa_token": c2, "code": code}))
        .await;
    assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_credentials");
    // A recovery code works once.
    let r = h
        .post("/api/auth/mfa/verify", None, json!({"mfa_token": c2, "code": codes[0]}))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let third = h
        .post(
            "/api/auth/login",
            None,
            json!({"email": "gina@example.com", "password": PW}),
        )
        .await;
    let r = h
        .post(
            "/api/auth/mfa/verify",
            None,
            json!({"mfa_token": third.json["mfa_token"], "code": codes[0]}),
        )
        .await;
    assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_credentials");

    // Magic links are a first factor only: MFA still applies.
    h.post(
        "/api/auth/magic-link/request",
        None,
        json!({"email": "gina@example.com"}),
    )
    .await;
    let mail = h.mail.last("gina@example.com", MailKind::MagicLink).unwrap();
    let r = h
        .post("/api/auth/magic-link/confirm", None, json!({"token": mail.secret}))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["mfa_required"], true);

    // Disabling needs a valid second factor.
    let r = h
        .post(
            "/api/auth/mfa/totp/disable",
            Some(&mfa_token),
            json!({"code": "000000"}),
        )
        .await;
    assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_credentials");
    let r = h
        .post(
            "/api/auth/mfa/totp/disable",
            Some(&mfa_token),
            json!({"code": codes[1]}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert!(h.login("gina@example.com").await["access_token"].is_string());
}

// ---------------------------------------------------------------------------
// API keys, service accounts, PATs, scopes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn api_key_scopes_are_enforced() {
    let h = harness(|_| {}).await;
    let admin = h.admin_token().await;
    let sa = h
        .post(
            "/api/auth/service-accounts",
            Some(&admin),
            json!({"name": "ci", "role": "admin"}),
        )
        .await;
    assert_eq!(sa.status, StatusCode::CREATED, "{}", sa.json);
    let sa_id = s(&sa.json["id"]).to_owned();
    let created = h
        .post(
            &format!("/api/auth/service-accounts/{sa_id}/keys"),
            Some(&admin),
            json!({"name": "reader", "scopes": ["collections:read:users"]}),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.json);
    let key = s(&created.json["key"]).to_owned();
    assert!(key.starts_with("fbk_"));
    let key_id = s(&created.json["id"]).to_owned();

    // In scope: allowed (and the admin role reaches the data layer).
    let r = h.get("/api/v1/collections/users/records", Some(&key)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let r = h
        .req(
            Method::GET,
            "/api/v1/collections/users/records",
            None,
            None,
            &[("x-api-key", &key)],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "X-API-Key header: {}", r.json);
    // Out of scope: other collection, write access, auth management.
    for (m, p) in [
        (Method::GET, "/api/v1/collections/posts/records"),
        (Method::POST, "/api/v1/collections/users/records"),
        (Method::GET, "/api/auth/service-accounts"),
        (Method::GET, "/api/auth/me"),
    ] {
        let r = h.req(m.clone(), p, Some(&key), Some(json!({})), &[]).await;
        assert_error(&r, StatusCode::FORBIDDEN, "forbidden");
        assert!(
            s(&r.json["error"]["message"]).contains("insufficient_scope"),
            "{m} {p}: {}",
            r.json
        );
    }

    // Listing never shows the secret.
    let list = h
        .get(&format!("/api/auth/service-accounts/{sa_id}/keys"), Some(&admin))
        .await;
    let text = list.json.to_string();
    assert!(!text.contains(&key[20..]), "secret leaked in listing");
    assert!(list.json["items"][0].get("key").is_none() && list.json["items"][0].get("secret_hash").is_none());

    // Invalid scope strings are refused.
    let r = h
        .post(
            &format!("/api/auth/service-accounts/{sa_id}/keys"),
            Some(&admin),
            json!({"name": "bad", "scopes": ["everything"]}),
        )
        .await;
    assert_error(&r, StatusCode::BAD_REQUEST, "invalid_input");

    // Revoked keys stop working immediately.
    let r = h
        .req(
            Method::DELETE,
            &format!("/api/auth/api-keys/{key_id}"),
            Some(&admin),
            None,
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.json);
    let r = h.get("/api/v1/collections/users/records", Some(&key)).await;
    assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_token");
    assert_eq!(r.json["error"]["message"], "API key revoked");

    // Personal access tokens: scoped to the owner, cannot escalate.
    h.signup("hank@example.com").await;
    let user = s(&h.login("hank@example.com").await["access_token"]).to_owned();
    let pat = h
        .post(
            "/api/auth/tokens",
            Some(&user),
            json!({"name": "cli", "scopes": ["auth:read", "auth:write"], "expires_in_days": 30}),
        )
        .await;
    assert_eq!(pat.status, StatusCode::CREATED, "{}", pat.json);
    let pat_key = s(&pat.json["key"]).to_owned();
    assert!(pat_key.starts_with("fbp_"));
    let me = h.get("/api/auth/me", Some(&pat_key)).await;
    assert_eq!(me.status, StatusCode::OK, "{}", me.json);
    assert_eq!(me.json["credential"], "personal_access_token");
    assert_eq!(me.json["scopes"], json!(["auth:read", "auth:write"]));
    // The PAT may mint narrower tokens but not broader ones.
    let r = h
        .post(
            "/api/auth/tokens",
            Some(&pat_key),
            json!({"name": "narrow", "scopes": ["auth:read"]}),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    let r = h
        .post(
            "/api/auth/tokens",
            Some(&pat_key),
            json!({"name": "wide", "scopes": ["*"]}),
        )
        .await;
    assert_error(&r, StatusCode::FORBIDDEN, "forbidden");
    // Data access needs a data scope.
    let r = h.get("/api/v1/collections/users/records", Some(&pat_key)).await;
    assert_error(&r, StatusCode::FORBIDDEN, "forbidden");
    // The owner revokes it.
    let r = h
        .req(
            Method::DELETE,
            &format!("/api/auth/tokens/{}", s(&pat.json["id"])),
            Some(&user),
            None,
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.json);
    assert_eq!(
        h.get("/api/auth/me", Some(&pat_key)).await.status,
        StatusCode::UNAUTHORIZED
    );

    // Deleting the service account kills its keys.
    let k2 = h
        .post(
            &format!("/api/auth/service-accounts/{sa_id}/keys"),
            Some(&admin),
            json!({"name": "k2", "scopes": ["*"]}),
        )
        .await;
    let k2 = s(&k2.json["key"]).to_owned();
    assert_eq!(h.get("/api/auth/me", Some(&k2)).await.status, StatusCode::OK);
    let r = h
        .req(
            Method::DELETE,
            &format!("/api/auth/service-accounts/{sa_id}"),
            Some(&admin),
            None,
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(h.get("/api/auth/me", Some(&k2)).await.status, StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------------------------
// Cookie mode and CSRF
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cookie_sessions_require_csrf_for_writes() {
    let h = harness(|_| {}).await;
    h.signup("ivy@example.com").await;
    let r = h
        .post(
            "/api/auth/login",
            None,
            json!({"email": "ivy@example.com", "password": PW, "cookie": true, "device_name": "Laptop"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert!(r.json.get("access_token").is_none() && r.json.get("refresh_token").is_none());
    let csrf = s(&r.json["csrf_token"]).to_owned();
    let cookies: Vec<String> = r
        .headers
        .get_all("set-cookie")
        .iter()
        .map(|v| v.to_str().unwrap().to_owned())
        .collect();
    let session = cookies.iter().find(|c| c.starts_with("fb_session=")).unwrap();
    assert!(
        session.contains("HttpOnly") && session.contains("SameSite=Lax"),
        "{session}"
    );
    let value = session.split(';').next().unwrap().to_owned();

    let me = h
        .req(Method::GET, "/api/auth/me", None, None, &[("cookie", &value)])
        .await;
    assert_eq!(me.status, StatusCode::OK, "{}", me.json);
    assert_eq!(me.json["credential"], "session_cookie");
    let sessions = h
        .req(Method::GET, "/api/auth/sessions", None, None, &[("cookie", &value)])
        .await;
    assert_eq!(sessions.json["items"][0]["device_name"], "Laptop");
    assert_eq!(sessions.json["items"][0]["cookie"], true);

    // Unsafe method without the CSRF header: refused.
    let r = h
        .req(
            Method::POST,
            "/api/auth/logout",
            None,
            Some(json!({})),
            &[("cookie", &value)],
        )
        .await;
    assert_error(&r, StatusCode::FORBIDDEN, "forbidden");
    let r = h
        .req(
            Method::POST,
            "/api/auth/logout",
            None,
            Some(json!({})),
            &[("cookie", &value), ("x-csrf-token", "wrong")],
        )
        .await;
    assert_error(&r, StatusCode::FORBIDDEN, "forbidden");
    let r = h
        .req(
            Method::POST,
            "/api/auth/logout",
            None,
            Some(json!({})),
            &[("cookie", &value), ("x-csrf-token", &csrf)],
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.json);

    // The stale cookie is ignored (anonymous) and cleared, so login still works.
    let r = h
        .req(Method::GET, "/api/auth/me", None, None, &[("cookie", &value)])
        .await;
    assert_error(&r, StatusCode::UNAUTHORIZED, "unauthenticated");
    assert!(
        r.headers
            .get_all("set-cookie")
            .iter()
            .any(|v| v.to_str().unwrap().starts_with("fb_session=;"))
    );
}

// ---------------------------------------------------------------------------
// Email OTP, magic link, reset, password change
// ---------------------------------------------------------------------------

#[tokio::test]
async fn passwordless_and_recovery_flows() {
    let h = harness(|c| c.otp_max_attempts = 3).await;
    h.signup("jack@example.com").await;

    // Requests answer the same for unknown accounts and send nothing.
    for path in [
        "/api/auth/otp/request",
        "/api/auth/magic-link/request",
        "/api/auth/password-reset/request",
    ] {
        let a = h.post(path, None, json!({"email": "nobody@example.com"})).await;
        let b = h.post(path, None, json!({"email": "jack@example.com"})).await;
        assert_eq!(a.status, StatusCode::ACCEPTED);
        assert_eq!(a.json, b.json);
    }
    assert!(h.mail.read_all().iter().all(|m| m.to != "nobody@example.com"));

    // Email OTP: wrong codes burn the code after 3 attempts.
    h.post("/api/auth/otp/request", None, json!({"email": "jack@example.com"}))
        .await;
    let code = h.mail.last("jack@example.com", MailKind::EmailOtp).unwrap().secret;
    assert_eq!(code.len(), 6);
    let wrong = if code == "000000" { "111111" } else { "000000" };
    for _ in 0..3 {
        let r = h
            .post(
                "/api/auth/otp/verify",
                None,
                json!({"email": "jack@example.com", "code": wrong}),
            )
            .await;
        assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_credentials");
    }
    let r = h
        .post(
            "/api/auth/otp/verify",
            None,
            json!({"email": "jack@example.com", "code": code}),
        )
        .await;
    assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_credentials");
    // A fresh code works once.
    h.post("/api/auth/otp/request", None, json!({"email": "jack@example.com"}))
        .await;
    let code = h.mail.last("jack@example.com", MailKind::EmailOtp).unwrap().secret;
    let r = h
        .post(
            "/api/auth/otp/verify",
            None,
            json!({"email": "jack@example.com", "code": code}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["user"]["verified"], true, "OTP proves the mailbox");
    let otp_access = s(&r.json["access_token"]).to_owned();
    let r = h
        .post(
            "/api/auth/otp/verify",
            None,
            json!({"email": "jack@example.com", "code": code}),
        )
        .await;
    assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_credentials");

    // Magic link: single use.
    h.post(
        "/api/auth/magic-link/request",
        None,
        json!({"email": "jack@example.com"}),
    )
    .await;
    let link = h.mail.last("jack@example.com", MailKind::MagicLink).unwrap().secret;
    let r = h
        .post("/api/auth/magic-link/confirm", None, json!({"token": link}))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let r = h
        .post("/api/auth/magic-link/confirm", None, json!({"token": link}))
        .await;
    assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_token");

    // Password reset: weak password keeps the token usable; success revokes sessions.
    h.post(
        "/api/auth/password-reset/request",
        None,
        json!({"email": "jack@example.com"}),
    )
    .await;
    let reset = h.mail.last("jack@example.com", MailKind::PasswordReset).unwrap().secret;
    let r = h
        .post(
            "/api/auth/password-reset/confirm",
            None,
            json!({"token": reset, "password": "short"}),
        )
        .await;
    assert_error(&r, StatusCode::BAD_REQUEST, "invalid_input");
    let r = h
        .post(
            "/api/auth/password-reset/confirm",
            None,
            json!({"token": reset, "password": "another long password"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.json);
    assert_eq!(
        h.get("/api/auth/me", Some(&otp_access)).await.status,
        StatusCode::UNAUTHORIZED
    );
    let old = h
        .post(
            "/api/auth/login",
            None,
            json!({"email": "jack@example.com", "password": PW}),
        )
        .await;
    assert_error(&old, StatusCode::UNAUTHORIZED, "invalid_credentials");
    let new = h
        .post(
            "/api/auth/login",
            None,
            json!({"email": "jack@example.com", "password": "another long password"}),
        )
        .await;
    assert_eq!(new.status, StatusCode::OK);

    // Password change: needs the current password, keeps this session, ends others.
    let t1 = s(&new.json["access_token"]).to_owned();
    let other = h
        .post(
            "/api/auth/login",
            None,
            json!({"email": "jack@example.com", "password": "another long password"}),
        )
        .await;
    let t2 = s(&other.json["access_token"]).to_owned();
    let r = h
        .post(
            "/api/auth/password/change",
            Some(&t1),
            json!({"current_password": "nope", "new_password": "third long password"}),
        )
        .await;
    assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_credentials");
    let r = h
        .post(
            "/api/auth/password/change",
            Some(&t1),
            json!({"current_password": "another long password", "new_password": "third long password"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.json);
    assert_eq!(h.get("/api/auth/me", Some(&t1)).await.status, StatusCode::OK);
    assert_eq!(h.get("/api/auth/me", Some(&t2)).await.status, StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------------------------
// Secret hygiene
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

static CAPTURE: std::sync::OnceLock<Capture> = std::sync::OnceLock::new();

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Capture;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::test]
async fn no_secret_is_logged_stored_or_returned_after_creation() {
    // Global subscriber: every test in this binary logs into the capture,
    // which only widens what is checked.
    let capture = CAPTURE.get_or_init(|| {
        let capture = Capture::default();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_writer(capture.clone())
            .with_ansi(false)
            .finish();
        tracing::subscriber::set_global_default(subscriber).unwrap();
        capture
    });

    let h = harness(|_| {}).await;
    let mut secrets: Vec<String> = vec![
        PW.to_owned(),
        "admin password 1".to_owned(),
        "a new long password".to_owned(),
    ];
    h.signup("kim@example.com").await;
    let verify = h.mail.last("kim@example.com", MailKind::VerifyEmail).unwrap().secret;
    secrets.push(verify);
    let login = h.login("kim@example.com").await;
    let access = s(&login["access_token"]).to_owned();
    secrets.push(s(&login["refresh_token"]).to_owned());
    let refreshed = h
        .post(
            "/api/auth/refresh",
            None,
            json!({"refresh_token": login["refresh_token"]}),
        )
        .await;
    secrets.push(s(&refreshed.json["refresh_token"]).to_owned());
    secrets.push(s(&refreshed.json["access_token"]).to_owned());
    secrets.push(access.clone());
    // Wrong password attempt (the attempted password must not be logged either).
    h.post(
        "/api/auth/login",
        None,
        json!({"email": "kim@example.com", "password": "wrong guess 12345"}),
    )
    .await;
    secrets.push("wrong guess 12345".into());

    let pat = h
        .post(
            "/api/auth/tokens",
            Some(&access),
            json!({"name": "t", "scopes": ["auth:read"]}),
        )
        .await;
    let pat_key = s(&pat.json["key"]).to_owned();
    secrets.push(pat_key.split('_').skip(2).collect::<Vec<_>>().join("_"));
    secrets.push(pat_key.clone());
    h.get("/api/auth/me", Some(&pat_key)).await;

    let admin = h.admin_token().await;
    secrets.push(admin.clone());
    let sa = h
        .post("/api/auth/service-accounts", Some(&admin), json!({"name": "svc"}))
        .await;
    let key = h
        .post(
            &format!("/api/auth/service-accounts/{}/keys", s(&sa.json["id"])),
            Some(&admin),
            json!({"name": "k", "scopes": ["*"]}),
        )
        .await;
    secrets.push(s(&key.json["key"]).to_owned());

    let setup = h.post("/api/auth/mfa/totp/setup", Some(&access), json!({})).await;
    let totp_secret = s(&setup.json["secret"]).to_owned();
    secrets.push(totp_secret.clone());
    let confirm = h
        .post(
            "/api/auth/mfa/totp/confirm",
            Some(&access),
            json!({"code": totp(&totp_secret, 0)}),
        )
        .await;
    for c in confirm.json["recovery_codes"].as_array().unwrap() {
        secrets.push(s(c).replace('-', ""));
        secrets.push(s(c).to_owned());
    }
    let mfa = h
        .post(
            "/api/auth/login",
            None,
            json!({"email": "kim@example.com", "password": PW}),
        )
        .await;
    secrets.push(s(&mfa.json["mfa_token"]).to_owned());

    h.post("/api/auth/otp/request", None, json!({"email": "kim@example.com"}))
        .await;
    let code = h.mail.last("kim@example.com", MailKind::EmailOtp).unwrap().secret;
    h.post(
        "/api/auth/password-reset/request",
        None,
        json!({"email": "kim@example.com"}),
    )
    .await;
    let reset = h.mail.last("kim@example.com", MailKind::PasswordReset).unwrap().secret;
    secrets.push(reset.clone());
    h.post(
        "/api/auth/password-reset/confirm",
        None,
        json!({"token": reset, "password": "a new long password"}),
    )
    .await;

    // 1. Logs: nothing secret, even at TRACE.
    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(!logs.is_empty(), "the capture works");
    for sec in &secrets {
        assert!(!logs.contains(sec.as_str()), "secret {sec:?} found in logs");
    }
    assert!(!logs.contains(&format!("code={code}")) && !logs.contains(&format!("\"{code}\"")));

    // 2. Database: no plaintext anywhere (every text and blob cell of every table).
    use forgebase_db::{Backend, Statement, Value as V};
    let tables =
        h.db.fetch_all(&Statement::raw("SELECT name FROM sqlite_master WHERE type = 'table'"))
            .await
            .unwrap();
    let mut dump = String::new();
    for row in &tables.rows {
        let V::Text(name) = &row[0] else { continue };
        let rows =
            h.db.fetch_all(&Statement::raw(format!("SELECT * FROM \"{name}\"")))
                .await
                .unwrap();
        for r in rows.rows {
            for v in r {
                match v {
                    V::Text(t) => dump.push_str(&t),
                    V::Blob(b) => dump.push_str(&String::from_utf8_lossy(&b)),
                    _ => {}
                }
                dump.push('\n');
            }
        }
    }
    for sec in &secrets {
        assert!(!dump.contains(sec.as_str()), "secret {sec:?} stored in plaintext");
    }
    assert!(!dump.lines().any(|l| l == code), "OTP code stored in plaintext");
    assert!(dump.contains("$argon2id$"), "passwords are Argon2id hashes");

    // 3. API responses after creation: listings and audit never carry secrets.
    let admin2 = s(&h
        .post(
            "/api/auth/admins/login",
            None,
            json!({"email": "root@example.com", "password": "admin password 1"}),
        )
        .await
        .json["access_token"])
    .to_owned();
    let mut listed = String::new();
    for path in [
        "/api/auth/audit?limit=500",
        "/api/auth/service-accounts",
        "/api/auth/admins",
    ] {
        listed.push_str(&h.get(path, Some(&admin2)).await.json.to_string());
    }
    listed.push_str(
        &h.get(
            &format!("/api/auth/service-accounts/{}/keys", s(&sa.json["id"])),
            Some(&admin2),
        )
        .await
        .json
        .to_string(),
    );
    for sec in &secrets {
        assert!(!listed.contains(sec.as_str()), "secret {sec:?} returned after creation");
    }
    assert!(!listed.contains("password_hash") && !listed.contains("secret_hash") && !listed.contains("$argon2"));
}

#[tokio::test]
async fn signing_key_rotation_keeps_old_tokens_valid() {
    let h = harness(|_| {}).await;
    h.signup("lee@example.com").await;
    let before = s(&h.login("lee@example.com").await["access_token"]).to_owned();
    let admin = h.admin_token().await;
    let r = h.post("/api/auth/keys/rotate", Some(&admin), json!({})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let after = s(&h.login("lee@example.com").await["access_token"]).to_owned();
    let kid = |t: &str| forgebase_auth::crypto::jwt_header(t).unwrap().kid;
    assert_ne!(kid(&before), kid(&after));
    assert_eq!(kid(&after), s(&r.json["active_kid"]));
    assert_eq!(h.get("/api/auth/me", Some(&before)).await.status, StatusCode::OK);
    assert_eq!(h.get("/api/auth/me", Some(&after)).await.status, StatusCode::OK);
    // Signing keys are encrypted at rest.
    use forgebase_db::{Backend, Statement};
    let rows =
        h.db.fetch_all(&Statement::raw("SELECT length(secret_enc) FROM _forgebase_auth_keys"))
            .await
            .unwrap();
    assert_eq!(rows.rows.len(), 2);
    // A different master key cannot open them: the service refuses to start.
    let dir = h.dir;
    h.db.close().await;
    std::fs::write(
        dir.path().join("auth/master.key"),
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\n",
    )
    .unwrap();
    let db = open_db(dir.path()).await;
    let err = AuthService::open_with_data(
        Arc::new(db.clone()),
        Arc::new(DefaultAuthorizer),
        dir.path(),
        test_config(dir.path()),
        None,
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("master key"), "{err}");
}
