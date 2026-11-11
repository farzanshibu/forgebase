//! Test harness: the real router (`router_with`) over a real SQLite database,
//! driven in-process. Identity is injected by a test layer that reads
//! `x-test-user` (`admin`, `user:<id>`) and `x-test-tenant`, standing in for
//! the FB-20 auth layer. No header means no identity (anonymous).
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use axum::middleware::{self, Next};
use forgebase_api::{AppState, RouterOptions};
use forgebase_core::DataLayout;
use forgebase_data::{AccessContext, Authorizer, DefaultAuthorizer};
use forgebase_db::Database;
use forgebase_db::testing::{TestDb, test_backend};
use forgebase_kv::{KvConfig, KvService};
use serde_json::{Value as Json, json};
use tower::ServiceExt;

pub struct Harness {
    pub app: Router,
    pub kv: KvService,
    pub db: Database,
    pub clock: Arc<AtomicU64>,
    pub tmp: tempfile::TempDir,
    pub test: TestDb,
}

pub struct Resp {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub bytes: Vec<u8>,
}

impl Resp {
    pub fn json(&self) -> Json {
        serde_json::from_slice(&self.bytes)
            .unwrap_or_else(|e| panic!("not JSON ({e}): {}", String::from_utf8_lossy(&self.bytes)))
    }

    pub fn expect(self, status: StatusCode) -> Self {
        assert_eq!(self.status, status, "body: {}", String::from_utf8_lossy(&self.bytes));
        self
    }

    pub fn code(&self) -> String {
        self.json()["error"]["code"].as_str().unwrap_or_default().to_owned()
    }
}

/// A migrated database on the backend `FORGEBASE_TEST_BACKEND` selects.
pub async fn test_db() -> TestDb {
    test_backend().await
}

impl Harness {
    pub async fn new() -> Self {
        Self::with(Arc::new(DefaultAuthorizer), KvConfig::default()).await
    }

    pub async fn with(auth: Arc<dyn Authorizer>, cfg: KvConfig) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let test = test_db().await;
        let db = test.database().clone();
        let layout = DataLayout::new(tmp.path().join("data"));
        layout.ensure().unwrap();
        let state = AppState::with_authorizer(db.clone(), layout, Arc::clone(&auth))
            .await
            .unwrap();
        let clock = Arc::new(AtomicU64::new(1_000_000));
        let c = Arc::clone(&clock);
        let kv = KvService::open_with_clock(
            cfg,
            auth,
            Some(Arc::new(db.clone())),
            Arc::new(move || c.load(Ordering::SeqCst)),
        )
        .await
        .unwrap();
        let app = forgebase_api::router_with(
            state,
            RouterOptions::default(),
            forgebase_kv::http::router(kv.clone(), 4 << 20),
            Some(forgebase_kv::http::openapi()),
        )
        .layer(middleware::from_fn(|mut req: Request<Body>, next: Next| async move {
            let user = req
                .headers()
                .get("x-test-user")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            let tenant = req
                .headers()
                .get("x-test-tenant")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            let ctx = match user.as_deref() {
                Some("admin") => Some(AccessContext::admin("root")),
                Some(u) if u.starts_with("user:") => Some(AccessContext::user("users", &u[5..])),
                _ => None,
            };
            if let Some(mut ctx) = ctx {
                ctx.tenant = tenant;
                req.extensions_mut().insert(ctx);
            }
            next.run(req).await
        }));
        Self {
            app,
            kv,
            db,
            clock,
            tmp,
            test,
        }
    }

    pub fn advance(&self, ms: u64) {
        self.clock.fetch_add(ms, Ordering::SeqCst);
    }

    pub async fn raw(
        &self,
        method: Method,
        uri: &str,
        headers: &[(&str, &str)],
        body: Option<Json>,
    ) -> axum::response::Response {
        let mut req = Request::builder().method(method).uri(uri);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let body = match body {
            Some(b) => {
                req = req.header("content-type", "application/json");
                Body::from(b.to_string())
            }
            None => Body::empty(),
        };
        self.app.clone().oneshot(req.body(body).unwrap()).await.unwrap()
    }

    pub async fn send(&self, method: Method, uri: &str, headers: &[(&str, &str)], body: Option<Json>) -> Resp {
        let res = self.raw(method, uri, headers, body).await;
        let (parts, body) = res.into_parts();
        let bytes = axum::body::to_bytes(body, 64 << 20).await.unwrap().to_vec();
        Resp {
            status: parts.status,
            headers: parts.headers,
            bytes,
        }
    }

    /// As the administrator of the default tenant.
    pub async fn admin(&self, method: Method, uri: &str, body: Option<Json>) -> Resp {
        self.send(method, uri, &[("x-test-user", "admin")], body).await
    }

    pub async fn get(&self, uri: &str) -> Resp {
        self.admin(Method::GET, uri, None).await
    }

    pub async fn post(&self, uri: &str, body: Json) -> Resp {
        self.admin(Method::POST, uri, Some(body)).await
    }

    pub async fn put(&self, uri: &str, body: Json) -> Resp {
        self.admin(Method::PUT, uri, Some(body)).await
    }

    pub async fn delete(&self, uri: &str) -> Resp {
        self.admin(Method::DELETE, uri, None).await
    }

    /// Runs a command through `exec` and returns its `result`.
    pub async fn exec(&self, ns: &str, cmd: &[&str]) -> Json {
        self.post(&format!("/api/v1/kv/{ns}/exec"), json!({ "command": cmd }))
            .await
            .expect(StatusCode::OK)
            .json()["result"]
            .clone()
    }
}
