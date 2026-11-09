//! The ForgeBase HTTP API.
//!
//! [`router`] builds the complete Axum application for a running instance:
//!
//! | Path | Purpose |
//! |---|---|
//! | `GET /health` | liveness |
//! | `GET /health/ready` | readiness (database + storage) |
//! | `GET /metrics` | Prometheus metrics |
//! | `GET /api/system/info` | instance information |
//! | `/api/auth/*` | authentication (forgebase-auth, ADR-0004) |
//! | `/api/v1/realtime/*` | WebSocket, SSE, channels (forgebase-realtime, ADR-0011) |
//! | `/admin/` | embedded admin dashboard |
//!
//! Versioning: management endpoints live under `/api/system/*`. Data APIs
//! added from Phase 2 live under `/api/v1/*`; a breaking change gets a new
//! prefix (`/api/v2/*`) while the old one is kept for a deprecation period.
//! See ADR-0001.
//!
//! Middleware, outermost first: request context (request id, error
//! envelope, security headers) → tracing span → metrics → panic catcher →
//! request timeout → authentication (credential → `AccessContext`
//! extension) → handler.

pub mod admin;
pub mod context;
pub mod error;
pub mod http;
pub mod openapi;
pub mod principal;
pub mod routes;
pub mod v1;

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware;
use axum::routing::get;
use forgebase_auth::AuthService;
use forgebase_core::DataLayout;
use forgebase_data::{Authorizer, DataError, DataService, DefaultAuthorizer};
use forgebase_db::Database;
use metrics_exporter_prometheus::PrometheusHandle;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::{DefaultOnFailure, DefaultOnResponse, TraceLayer};
use tracing::Level;

pub use context::{RequestId, X_REQUEST_ID};
pub use error::ApiError;
pub use principal::Access;

/// Shared state of the HTTP layer. Cheap to clone.
#[derive(Debug, Clone)]
pub struct AppState {
    inner: Arc<StateInner>,
}

#[derive(Debug, Clone)]
struct StateInner {
    db: Database,
    data: DataService,
    limits: Limits,
    layout: DataLayout,
    started_at: String,
    started: Instant,
    metrics: &'static PrometheusHandle,
    auth: Option<AuthService>,
    realtime: Option<forgebase_realtime::RealtimeService>,
}

/// Size limits the handlers enforce themselves (see [`RouterOptions`]).
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Largest import body in bytes.
    pub max_import_bytes: u64,
}

impl AppState {
    /// State for an open, migrated database in `layout`, with the fail-closed
    /// [`DefaultAuthorizer`] (administrators and the system only).
    pub async fn new(db: Database, layout: DataLayout) -> Result<Self, DataError> {
        Self::with_authorizer(db, layout, Arc::new(DefaultAuthorizer)).await
    }

    /// Same with a custom [`Authorizer`] (the policy engine, or a test double).
    pub async fn with_authorizer(
        db: Database,
        layout: DataLayout,
        authorizer: Arc<dyn Authorizer>,
    ) -> Result<Self, DataError> {
        let data = DataService::new(Arc::new(db.clone()), authorizer).await?;
        Ok(Self {
            inner: Arc::new(StateInner {
                db,
                data,
                limits: Limits {
                    max_import_bytes: RouterOptions::default().max_import_bytes,
                },
                layout,
                started_at: forgebase_core::now_rfc3339(),
                started: Instant::now(),
                metrics: forgebase_observability::metrics_handle(),
                auth: None,
                realtime: None,
            }),
        })
    }

    /// The data-core service every `/api/v1` handler calls.
    pub fn data(&self) -> &DataService {
        &self.inner.data
    }

    /// Limits enforced inside handlers.
    pub fn limits(&self) -> Limits {
        self.inner.limits
    }

    fn with_limits(&self, limits: Limits) -> Self {
        // A new state sharing everything but the limits.
        let mut inner = (*self.inner).clone();
        inner.limits = limits;
        Self { inner: Arc::new(inner) }
    }

    /// State on an existing data service (the one the auth service built,
    /// with the Argon2id hasher for `password` fields).
    pub fn from_data(db: Database, layout: DataLayout, data: DataService) -> Self {
        Self {
            inner: Arc::new(StateInner {
                db,
                data,
                limits: Limits {
                    max_import_bytes: RouterOptions::default().max_import_bytes,
                },
                layout,
                started_at: forgebase_core::now_rfc3339(),
                started: Instant::now(),
                metrics: forgebase_observability::metrics_handle(),
                auth: None,
                realtime: None,
            }),
        }
    }

    /// Same state with authentication: installs the auth layer and mounts
    /// `/api/auth/*`. Without it every request is anonymous. The data service
    /// becomes the auth service's (one catalog, one password hasher).
    pub fn with_auth(mut self, auth: AuthService) -> Self {
        let inner = Arc::make_mut(&mut self.inner);
        inner.data = auth.data().clone();
        inner.auth = Some(auth);
        self
    }

    /// The auth service, when configured.
    pub fn auth(&self) -> Option<&AuthService> {
        self.inner.auth.as_ref()
    }

    /// Same state with realtime (FB-40): mounts `/api/v1/realtime/*`. The
    /// service must run on this state's data service.
    pub fn with_realtime(mut self, realtime: forgebase_realtime::RealtimeService) -> Self {
        Arc::make_mut(&mut self.inner).realtime = Some(realtime);
        self
    }

    /// The realtime service, when configured.
    pub fn realtime(&self) -> Option<&forgebase_realtime::RealtimeService> {
        self.inner.realtime.as_ref()
    }

    /// The primary database.
    pub fn db(&self) -> &Database {
        &self.inner.db
    }

    /// The data directory layout.
    pub fn layout(&self) -> &DataLayout {
        &self.inner.layout
    }

    /// RFC 3339 time the state was created (server start).
    pub fn started_at(&self) -> &str {
        &self.inner.started_at
    }

    /// Whole seconds since start.
    pub fn uptime_secs(&self) -> u64 {
        self.inner.started.elapsed().as_secs()
    }

    /// Prometheus handle.
    pub fn metrics(&self) -> &'static PrometheusHandle {
        self.inner.metrics
    }
}

/// Router options.
#[derive(Debug, Clone)]
pub struct RouterOptions {
    /// Per-request timeout; exceeding it returns `408 request_timeout`.
    /// It bounds the whole handler, including reading an import body.
    pub request_timeout: Duration,
    /// Largest JSON request body of `/api/v1` (413 `payload_too_large` above).
    pub max_body_bytes: u64,
    /// Largest import body (streamed, so far above the JSON limit).
    pub max_import_bytes: u64,
    /// Origins allowed by CORS: exact origins such as `https://app.example.com`,
    /// or `*` for any. Empty (the default) sends no CORS headers, so browsers
    /// only allow same-origin use.
    pub cors_allowed_origins: Vec<String>,
}

impl Default for RouterOptions {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(30),
            max_body_bytes: 4 * 1024 * 1024,
            max_import_bytes: 256 * 1024 * 1024,
            cors_allowed_origins: Vec::new(),
        }
    }
}

fn cors_layer(origins: &[String]) -> Option<CorsLayer> {
    use axum::http::{HeaderName, Method, header};
    if origins.is_empty() {
        return None;
    }
    let allow = if origins.iter().any(|o| o == "*") {
        AllowOrigin::any()
    } else {
        AllowOrigin::list(origins.iter().filter_map(|o| o.parse().ok()))
    };
    Some(
        CorsLayer::new()
            .allow_origin(allow)
            .allow_methods([
                Method::GET,
                Method::POST,
                Method::PUT,
                Method::PATCH,
                Method::DELETE,
                Method::OPTIONS,
            ])
            .allow_headers([
                header::AUTHORIZATION,
                header::CONTENT_TYPE,
                header::IF_MATCH,
                HeaderName::from_static("x-request-id"),
                HeaderName::from_static(forgebase_auth::http::API_KEY_HEADER),
                HeaderName::from_static(forgebase_auth::http::CSRF_HEADER),
            ])
            .expose_headers([header::ETAG, header::LOCATION, HeaderName::from_static("x-request-id")])
            .max_age(Duration::from_secs(600)),
    )
}

/// Builds the application router.
///
/// `/api/v1/*` handlers read the request's `AccessContext`. When the state has
/// an [`AuthService`] ([`AppState::with_auth`]), its layer runs just outside
/// [`principal::install_anonymous`] (inside request ids, CORS and the timeout,
/// so auth errors keep the envelope) and stores the caller's context;
/// otherwise [`principal::install_anonymous`] stores an anonymous context and
/// the default authorizer denies the request.
pub fn router(state: AppState, opts: RouterOptions) -> Router {
    let auth = state.auth().cloned();
    let state = state.with_limits(Limits {
        max_import_bytes: opts.max_import_bytes,
    });
    let app = Router::new()
        .route("/health", get(routes::health))
        .route("/health/ready", get(routes::ready))
        .route("/metrics", get(routes::metrics))
        .route("/api/system/info", get(routes::system_info))
        .route("/api/openapi.json", get(openapi::serve))
        .nest("/api/v1", v1::router(&opts))
        .nest_service(
            "/api/v1/graph",
            forgebase_graph::router(forgebase_graph::GraphService::new(state.data().clone())),
        )
        .nest_service("/api/v1/realtime", realtime_router(state.realtime()))
        .route("/admin", get(admin::redirect))
        .route("/admin/", get(admin::index))
        .route("/admin/{*path}", get(admin::asset))
        .fallback(routes::not_found)
        .with_state(state)
        .layer(middleware::from_fn(principal::hide_collection_existence))
        .layer(middleware::from_fn(principal::install_anonymous));
    let app = with_auth_layer(app, auth)
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            opts.request_timeout,
        ))
        .layer(CatchPanicLayer::new())
        .layer(middleware::from_fn(context::track_metrics))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|req: &Request| {
                    let request_id = req.extensions().get::<RequestId>().map_or("-", RequestId::as_str);
                    // Path only: query strings may carry tokens.
                    tracing::info_span!(
                        "http",
                        method = %req.method(),
                        path = %req.uri().path(),
                        request_id = %request_id,
                    )
                })
                .on_response(DefaultOnResponse::new().level(Level::INFO))
                .on_failure(DefaultOnFailure::new().level(Level::ERROR)),
        );
    let app = match cors_layer(&opts.cors_allowed_origins) {
        Some(cors) => app.layer(cors),
        None => app,
    };
    app.layer(middleware::from_fn(context::request_context))
}

/// `/api/v1/realtime/*`: the realtime router, or `503 realtime_unavailable`
/// when the state has no realtime service.
fn realtime_router(rt: Option<&forgebase_realtime::RealtimeService>) -> Router {
    match rt {
        Some(rt) => forgebase_realtime::router(rt.clone()),
        None => Router::new().fallback(|| async {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "realtime_unavailable",
                "realtime is not running on this server",
            )
        }),
    }
}

/// Mounts `/api/auth/*` and installs the authentication layer, which must run
/// before (outside) [`principal::install_anonymous`].
fn with_auth_layer(app: Router, auth: Option<AuthService>) -> Router {
    match auth {
        Some(auth) => app
            .merge(forgebase_auth::http::router(auth.clone()))
            .layer(middleware::from_fn_with_state(auth, forgebase_auth::http::authenticate)),
        None => app,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Method, header};
    use tower::ServiceExt;

    async fn app() -> (Router, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().join("data"));
        layout.ensure().unwrap();
        let cfg = forgebase_core::Config::defaults(tmp.path());
        let db = Database::open(forgebase_db::DbOptions::from_config(
            &cfg.database,
            layout.database_path(),
        ))
        .await
        .unwrap();
        db.migrate(Some(&layout.backups_dir())).await.unwrap();
        (
            router(AppState::new(db, layout).await.unwrap(), RouterOptions::default()),
            tmp,
        )
    }

    async fn call(
        app: &Router,
        method: Method,
        uri: &str,
        headers: &[(&str, &str)],
    ) -> (StatusCode, header::HeaderMap, serde_json::Value, String) {
        let mut req = axum::http::Request::builder().method(method).uri(uri);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let res = app.clone().oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
        let status = res.status();
        let headers = res.headers().clone();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, headers, json, text)
    }

    #[tokio::test]
    async fn health_ready_and_info_follow_the_contract() {
        let (app, tmp) = app().await;
        let (s, h, j, _) = call(&app, Method::GET, "/health", &[]).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(j, serde_json::json!({"status": "ok"}));
        assert!(h.contains_key("x-request-id"));
        assert_eq!(h["x-content-type-options"], "nosniff");

        let (s, _, j, _) = call(&app, Method::GET, "/health/ready", &[]).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(
            j,
            serde_json::json!({"status": "ready", "checks": {"database": "ok", "storage": "ok"}})
        );

        let (s, _, j, _) = call(&app, Method::GET, "/api/system/info", &[]).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(j["name"], "forgebase");
        assert_eq!(j["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(j["mode"], "embedded");
        assert_eq!(j["data_dir"], tmp.path().join("data").display().to_string());
        assert!(j["started_at"].as_str().unwrap().ends_with('Z'));
        assert!(j["uptime_secs"].is_u64());
        assert_eq!(j["database"]["journal_mode"], "wal");
        assert_eq!(
            j["database"]["schema_version"],
            forgebase_db::migrations::Migrator::system().latest_version()
        );
        assert!(j["database"]["size_bytes"].as_u64().unwrap() > 0);
        assert!(j["database"]["path"].as_str().unwrap().ends_with("forgebase.db"));
    }

    #[tokio::test]
    async fn readiness_fails_when_storage_is_gone() {
        let (app, tmp) = app().await;
        std::fs::remove_dir_all(tmp.path().join("data/storage")).unwrap();
        let (s, _, j, _) = call(&app, Method::GET, "/health/ready", &[]).await;
        assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(j["status"], "not_ready");
        assert_eq!(j["checks"]["database"], "ok");
        assert_eq!(j["checks"]["storage"], "error");
        assert!(j["errors"]["storage"].as_str().unwrap().contains("not writable"));
    }

    #[tokio::test]
    async fn errors_use_the_envelope_with_the_request_id() {
        let (app, _tmp) = app().await;
        let (s, h, j, _) = call(&app, Method::GET, "/nope", &[("x-request-id", "client-id-1")]).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        assert_eq!(h["x-request-id"], "client-id-1");
        assert_eq!(j["error"]["code"], "not_found");
        assert_eq!(j["error"]["request_id"], "client-id-1");

        let (s, h, j, _) = call(&app, Method::POST, "/health", &[("x-request-id", "bad id\twith tab")]).await;
        assert_eq!(s, StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(h[header::CONTENT_TYPE], "application/json");
        assert_eq!(j["error"]["code"], "method_not_allowed");
        let rid = j["error"]["request_id"].as_str().unwrap();
        assert_ne!(rid, "bad id\twith tab", "unsafe ids are replaced");
        assert_eq!(h["x-request-id"], rid);
    }

    #[tokio::test]
    async fn metrics_are_prometheus_text() {
        let (app, _tmp) = app().await;
        call(&app, Method::GET, "/health", &[]).await;
        let (s, h, _, text) = call(&app, Method::GET, "/metrics", &[]).await;
        assert_eq!(s, StatusCode::OK);
        assert!(
            h[header::CONTENT_TYPE]
                .to_str()
                .unwrap()
                .starts_with("text/plain; version=0.0.4")
        );
        assert!(text.contains("forgebase_http_requests_total"), "{text}");
        assert!(text.contains("route=\"/health\""), "{text}");
        assert!(text.contains("forgebase_db_size_bytes"), "{text}");
        assert!(
            text.contains("forgebase_http_request_duration_seconds_bucket"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn admin_is_served_with_or_without_a_dashboard_build() {
        let (app, _tmp) = app().await;
        let (s, h, _, _) = call(&app, Method::GET, "/admin", &[]).await;
        assert_eq!(s, StatusCode::PERMANENT_REDIRECT);
        assert_eq!(h[header::LOCATION], "/admin/");

        let (s, h, _, body) = call(&app, Method::GET, "/admin/", &[]).await;
        assert_eq!(s, StatusCode::OK);
        assert!(h[header::CONTENT_TYPE].to_str().unwrap().starts_with("text/html"));
        assert!(body.contains("<html") || body.contains("<!doctype") || body.contains("<!DOCTYPE"));

        let (s, _, _, spa) = call(&app, Method::GET, "/admin/system/overview", &[]).await;
        assert_eq!(s, StatusCode::OK, "client-side routes get index.html");
        assert_eq!(spa, body);

        let (s, _, j, _) = call(&app, Method::GET, "/admin/assets/missing-file.js", &[]).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        assert_eq!(j["error"]["code"], "not_found");

        if admin::dashboard_embedded() {
            let (_, h, _, _) = call(&app, Method::GET, "/admin/", &[]).await;
            let etag = h[header::ETAG].to_str().unwrap().to_owned();
            let (s, _, _, _) = call(&app, Method::GET, "/admin/", &[("if-none-match", &etag)]).await;
            assert_eq!(s, StatusCode::NOT_MODIFIED);
        } else {
            assert!(body.contains("dashboard not built"));
        }
    }
}
