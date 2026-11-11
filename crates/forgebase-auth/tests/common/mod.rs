//! Shared test harness: in-process router over a real SQLite database.

#![allow(dead_code, clippy::unwrap_used, missing_docs)]

pub mod idp;
pub mod ldap;
pub mod mock_http;
pub mod rsa_key;

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use forgebase_auth::data::DefaultAuthorizer;
use forgebase_auth::{Argon2Config, AuthConfig, AuthService, DevMailer, MailKind};
use forgebase_db::Database;
use serde_json::{Value, json};
use tower::ServiceExt;

pub const PW: &str = "correct horse battery";

pub struct H {
    pub app: Router,
    pub auth: AuthService,
    pub mail: DevMailer,
    pub db: Database,
    pub dir: forgebase_db::testing::TestDb,
}

pub struct Resp {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub json: Value,
}

pub async fn harness(tweak: impl FnOnce(&mut AuthConfig)) -> H {
    let dir = forgebase_db::testing::test_backend().await;
    let db = dir.database().clone();
    let mut cfg = AuthConfig::for_data_dir(dir.data_dir());
    cfg.argon2 = Argon2Config::insecure_fast_for_tests();
    cfg.trust_forwarded_for = true;
    cfg.cookie_secure = false;
    cfg.ip_attempts_per_minute = 100_000;
    // Mock providers, gateways and Turnstile listen on loopback; the SSRF
    // guard (FB-80) is tested with this turned off in https_client.rs.
    cfg.allow_private_outbound = true;
    tweak(&mut cfg);
    let auth = AuthService::open_with_data(
        Arc::new(db.clone()),
        Arc::new(DefaultAuthorizer),
        dir.data_dir(),
        cfg,
        None,
    )
    .await
    .unwrap();
    let app =
        Router::new()
            .merge(forgebase_auth::http::router(auth.clone()))
            .layer(axum::middleware::from_fn_with_state(
                auth.clone(),
                forgebase_auth::http::authenticate,
            ));
    H {
        app,
        auth,
        mail: DevMailer::new(dir.data_dir().join("mail")),
        db,
        dir,
    }
}

impl H {
    pub async fn req(&self, method: Method, path: &str, bearer: Option<&str>, body: Option<Value>) -> Resp {
        self.req_raw(method, path, bearer, body.map(|v| ("application/json", v.to_string())))
            .await
    }

    pub async fn req_raw(
        &self,
        method: Method,
        path: &str,
        bearer: Option<&str>,
        body: Option<(&str, String)>,
    ) -> Resp {
        self.req_with(method, path, bearer, body, &[]).await
    }

    /// A JSON request with extra headers (realm, Turnstile token, ...).
    pub async fn send(
        &self,
        method: Method,
        path: &str,
        bearer: Option<&str>,
        body: Option<Value>,
        headers: &[(&str, &str)],
    ) -> Resp {
        self.req_with(
            method,
            path,
            bearer,
            body.map(|v| ("application/json", v.to_string())),
            headers,
        )
        .await
    }

    pub async fn req_with(
        &self,
        method: Method,
        path: &str,
        bearer: Option<&str>,
        body: Option<(&str, String)>,
        headers: &[(&str, &str)],
    ) -> Resp {
        let mut b = Request::builder()
            .method(method)
            .uri(path)
            .header("x-request-id", "test-req")
            .header("x-forwarded-for", "10.0.0.1");
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        if let Some(t) = bearer {
            b = b.header("authorization", format!("Bearer {t}"));
        }
        let req = match body {
            Some((ct, v)) => b.header("content-type", ct).body(Body::from(v)).unwrap(),
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

    pub async fn post(&self, path: &str, bearer: Option<&str>, body: Value) -> Resp {
        self.req(Method::POST, path, bearer, Some(body)).await
    }

    pub async fn get(&self, path: &str, bearer: Option<&str>) -> Resp {
        self.req(Method::GET, path, bearer, None).await
    }

    pub async fn delete(&self, path: &str, bearer: Option<&str>) -> Resp {
        self.req(Method::DELETE, path, bearer, None).await
    }

    /// Signs up and verifies an email, returns `(user id, access token)`.
    pub async fn verified_user(&self, email: &str) -> (String, String) {
        let r = self
            .post("/api/auth/signup", None, json!({"email": email, "password": PW}))
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
        let id = r.json["user"]["id"].as_str().unwrap().to_owned();
        let mail = self.mail.last(email, MailKind::VerifyEmail).unwrap();
        let r = self
            .post("/api/auth/verify-email/confirm", None, json!({"token": mail.secret}))
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.json);
        (id, self.password_login(email).await)
    }

    pub async fn password_login(&self, email: &str) -> String {
        let r = self
            .post("/api/auth/login", None, json!({"email": email, "password": PW}))
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.json);
        r.json["access_token"].as_str().unwrap().to_owned()
    }

    /// Signs up without verifying.
    pub async fn unverified_user(&self, email: &str) -> String {
        let r = self
            .post("/api/auth/signup", None, json!({"email": email, "password": PW}))
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
        r.json["user"]["id"].as_str().unwrap().to_owned()
    }
}

impl H {
    /// Creates the administrator `root@example.com` and returns an access token.
    pub async fn root_token(&self) -> String {
        self.auth
            .create_admin(
                "root@example.com",
                "admin password 1",
                "test",
                &forgebase_auth::ClientInfo::default(),
            )
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

    /// Runs one SQL statement on the database.
    pub async fn sql(&self, sql: &str) {
        use forgebase_db::Backend;
        let mut tx = self.db.begin(forgebase_db::TxMode::Normal).await.unwrap();
        tx.execute(&forgebase_db::Statement::raw(sql)).await.unwrap();
        tx.commit().await.unwrap();
    }

    /// One integer from a query.
    pub async fn count(&self, sql: &str) -> i64 {
        use forgebase_db::Backend;
        let r = self.db.fetch_all(&forgebase_db::Statement::raw(sql)).await.unwrap();
        r.rows[0][0].as_i64().unwrap()
    }
}

pub fn assert_error(r: &Resp, status: StatusCode, code: &str) {
    assert_eq!(r.status, status, "{}", r.json);
    assert_eq!(r.json["error"]["code"], code, "{}", r.json);
}
