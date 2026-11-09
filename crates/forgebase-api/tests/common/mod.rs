//! Test harness: the real router over a real SQLite database in a temp dir,
//! driven in-process with `tower::ServiceExt::oneshot` (no sockets). Identity
//! is injected with a test-only layer standing in for the FB-20 auth layer.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::io;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use axum::middleware::{self, Next};
use forgebase_api::{AppState, RouterOptions};
use forgebase_core::DataLayout;
use forgebase_data::{AccessContext, Authorizer, DefaultAuthorizer};
use forgebase_db::{Database, DbOptions};
use futures_util::stream;
use serde_json::{Value as Json, json};
use tower::ServiceExt;

pub struct Api {
    pub app: Router,
    pub db: Database,
    _tmp: tempfile::TempDir,
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

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes).into_owned()
    }

    /// Asserts the status, printing the body when it differs.
    pub fn expect(self, status: StatusCode) -> Self {
        assert_eq!(self.status, status, "body: {}", self.text());
        self
    }

    pub fn code(&self) -> String {
        self.json()["error"]["code"].as_str().unwrap_or_default().to_owned()
    }
}

pub fn admin() -> AccessContext {
    AccessContext::admin("root")
}

impl Api {
    /// An administrator on the default (fail-closed) authorizer.
    pub async fn admin() -> Self {
        Self::with(Some(admin()), Arc::new(DefaultAuthorizer), RouterOptions::default()).await
    }

    /// No identity layer at all: what the server does until FB-20 lands.
    pub async fn anonymous() -> Self {
        Self::with(None, Arc::new(DefaultAuthorizer), RouterOptions::default()).await
    }

    /// The policy engine (FB-21) as authorizer, its routes and tenancy layer.
    pub async fn with_policy_engine(ctx: Option<AccessContext>) -> Self {
        Self::build(ctx, None, RouterOptions::default()).await
    }

    pub async fn with(ctx: Option<AccessContext>, authorizer: Arc<dyn Authorizer>, opts: RouterOptions) -> Self {
        Self::build(ctx, Some(authorizer), opts).await
    }

    async fn build(ctx: Option<AccessContext>, authorizer: Option<Arc<dyn Authorizer>>, opts: RouterOptions) -> Self {
        let authorizer_is_policy = authorizer.is_none();
        let tmp = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().join("data"));
        layout.ensure().unwrap();
        let cfg = forgebase_core::Config::defaults(tmp.path());
        let db = Database::open(DbOptions::from_config(&cfg.database, layout.database_path()))
            .await
            .unwrap();
        db.migrate(Some(&layout.backups_dir())).await.unwrap();
        let state = match authorizer {
            Some(a) => AppState::with_authorizer(db.clone(), layout, a).await.unwrap(),
            None => {
                let backend: Arc<dyn forgebase_db::Backend> = Arc::new(db.clone());
                let policy = forgebase_policy::PolicyService::open(backend).await.unwrap();
                let state = AppState::with_authorizer(db.clone(), layout, policy.authorizer())
                    .await
                    .unwrap();
                policy.attach_data(state.data().clone());
                state.with_policy(policy)
            }
        };
        let mut app = forgebase_api::router(state, opts);
        let _ = authorizer_is_policy;
        if ctx.is_none() && authorizer_is_policy {
            // Test-only identity: `x-test-as: admin | <user id>` stands in for
            // the auth layer (outside the router's own layers, like it).
            app = app.layer(middleware::from_fn(|mut req: Request<Body>, next: Next| async move {
                let who = req
                    .headers()
                    .get("x-test-as")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned);
                match who.as_deref() {
                    Some("admin") => {
                        req.extensions_mut().insert(AccessContext::admin("root"));
                    }
                    Some(id) => {
                        req.extensions_mut().insert(AccessContext::user("users", id));
                    }
                    None => {}
                }
                next.run(req).await
            }));
        }
        if let Some(ctx) = ctx {
            // Outside the router's own layers, like the auth layer will be.
            app = app.layer(middleware::from_fn(move |mut req: Request<Body>, next: Next| {
                let ctx = ctx.clone();
                async move {
                    req.extensions_mut().insert(ctx);
                    next.run(req).await
                }
            }));
        }
        Self { app, db, _tmp: tmp }
    }

    pub async fn send(&self, method: Method, uri: &str, headers: &[(&str, &str)], body: Body) -> Resp {
        let mut req = Request::builder().method(method).uri(uri);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let res = self.app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
        let (parts, body) = res.into_parts();
        let bytes = axum::body::to_bytes(body, 256 << 20).await.unwrap().to_vec();
        Resp {
            status: parts.status,
            headers: parts.headers,
            bytes,
        }
    }

    pub async fn call(&self, method: Method, uri: &str, body: Option<Json>) -> Resp {
        match body {
            Some(b) => {
                self.send(
                    method,
                    uri,
                    &[("content-type", "application/json")],
                    Body::from(b.to_string()),
                )
                .await
            }
            None => self.send(method, uri, &[], Body::empty()).await,
        }
    }

    pub async fn get(&self, uri: &str) -> Resp {
        self.call(Method::GET, uri, None).await
    }

    pub async fn post(&self, uri: &str, body: Json) -> Resp {
        self.call(Method::POST, uri, Some(body)).await
    }

    pub async fn patch(&self, uri: &str, body: Json) -> Resp {
        self.call(Method::PATCH, uri, Some(body)).await
    }

    pub async fn delete(&self, uri: &str) -> Resp {
        self.call(Method::DELETE, uri, None).await
    }

    /// A body delivered in `chunk`-byte pieces, so parsers see split tokens.
    pub async fn post_chunked(&self, uri: &str, content_type: &str, data: &[u8], chunk: usize) -> Resp {
        let chunks: Vec<Result<Vec<u8>, io::Error>> = data.chunks(chunk).map(|c| Ok(c.to_vec())).collect();
        self.send(
            Method::POST,
            uri,
            &[("content-type", content_type)],
            Body::from_stream(stream::iter(chunks)),
        )
        .await
    }

    /// Creates a collection from JSON and returns its definition.
    pub async fn make_collection(&self, def: Json) -> Json {
        self.post("/api/v1/collections", def)
            .await
            .expect(StatusCode::CREATED)
            .json()
    }

    pub async fn make_record(&self, collection: &str, rec: Json) -> Json {
        self.post(&format!("/api/v1/collections/{collection}/records"), rec)
            .await
            .expect(StatusCode::CREATED)
            .json()
    }
}

pub fn text_field(name: &str) -> Json {
    json!({"name": name, "type": "text"})
}
