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
//! | `GET /api/system/features` | feature registry and settings (administrators; FB-91) |
//! | `PATCH /api/system/config` | write settings to the config file (administrators; FB-91) |
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
//! envelope, security headers) → feature gate (`404 feature_disabled` for
//! the routes of a switched-off module; FB-91) → tracing span → metrics → panic catcher →
//! request timeout → authentication (credential → `AccessContext`
//! extension) → tenancy (realm, organization, tenant, roles; FB-21) →
//! request gates (plan-based rate limits; FB-24) → handler.

pub mod admin;
pub mod context;
pub mod error;
pub mod features;
pub mod gate;
pub mod http;
pub mod openapi;
pub mod principal;
pub mod routes;
pub mod v1;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware;
use axum::routing::{get, patch};
use forgebase_auth::AuthService;
use forgebase_core::DataLayout;
use forgebase_data::{Authorizer, DataError, DataService, DefaultAuthorizer};
use forgebase_db::Database;
use forgebase_policy::PolicyService;
use metrics_exporter_prometheus::PrometheusHandle;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::{DefaultOnFailure, DefaultOnResponse, TraceLayer};
use tracing::Level;

pub use context::{RequestId, X_REQUEST_ID};
pub use error::ApiError;
pub use gate::{GateDecision, GateHeaders, RequestGate};
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
    policy: Option<PolicyService>,
    realtime: Option<forgebase_realtime::RealtimeService>,
    collectors: Vec<MetricsCollector>,
    database: DatabaseRuntime,
}

/// What `/api/system/info` and `/health/ready` report about the database
/// beyond the [`Database`] handle (FB-148): the PostgreSQL mode, the
/// extension switches and, when this process runs the bundled engine, its
/// state.
#[derive(Clone, Default)]
pub struct DatabaseRuntime {
    /// `managed` or `external` on PostgreSQL; `None` on SQLite.
    pub mode: Option<&'static str>,
    /// Where the data lives, for display: the SQLite file, the bundled
    /// cluster's directory, or the external server's URL without password.
    pub location: Option<String>,
    /// `(feature, PostgreSQL extension, mode)` from `[database.postgres.extensions]`.
    pub extensions: Vec<(&'static str, &'static str, &'static str)>,
    /// The bundled engine's state, when this process runs it.
    pub engine: Option<Arc<dyn Fn() -> EngineReport + Send + Sync>>,
}

impl std::fmt::Debug for DatabaseRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DatabaseRuntime")
            .field("mode", &self.mode)
            .field("location", &self.location)
            .field("extensions", &self.extensions)
            .field("engine", &self.engine.as_ref().map(|e| e()))
            .finish()
    }
}

/// The bundled PostgreSQL engine as the server sees it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct EngineReport {
    /// `ready`, `restarting`, `failed` or `stopped`.
    pub state: &'static str,
    /// PostgreSQL version (`18.6`).
    pub version: String,
    /// Postmaster pid while running.
    pub pid: Option<u32>,
    /// Restarts after crashes since the server started.
    pub restarts: u32,
    /// The last failure, if any.
    pub last_error: Option<String>,
    /// Unix socket directory (`None` on Windows).
    pub socket_dir: Option<String>,
    /// TCP address, when direct TCP logins are on.
    pub tcp: Option<String>,
    /// Start-up warnings (a missing optional module).
    pub warnings: Vec<String>,
}

/// An async hook that refreshes gauges just before `/metrics` renders.
#[derive(Clone)]
pub struct MetricsCollector(Arc<dyn Fn() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>);

impl std::fmt::Debug for MetricsCollector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MetricsCollector")
    }
}

impl MetricsCollector {
    /// Runs the collector once.
    pub(crate) fn run(&self) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        (self.0)()
    }
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
                policy: None,
                realtime: None,
                collectors: Vec::new(),
                database: DatabaseRuntime::default(),
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
                policy: None,
                realtime: None,
                collectors: Vec::new(),
                database: DatabaseRuntime::default(),
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

    /// Same state with the policy engine (FB-21): mounts the policy and
    /// tenancy routes and installs the tenancy layer. The data service must
    /// have been built with [`PolicyService::authorizer`].
    pub fn with_policy(mut self, policy: PolicyService) -> Self {
        Arc::make_mut(&mut self.inner).policy = Some(policy);
        self
    }

    /// The policy service, when configured.
    pub fn policy(&self) -> Option<&PolicyService> {
        self.inner.policy.as_ref()
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

    /// Same state with a collector run on every `/metrics` scrape (each is cut
    /// off after two seconds so a slow subsystem cannot stall the scrape).
    /// Collectors set gauges through the `metrics` macros.
    pub fn with_metrics_collector<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        Arc::make_mut(&mut self.inner)
            .collectors
            .push(MetricsCollector(Arc::new(move || Box::pin(f()))));
        self
    }

    /// Registered metrics collectors.
    pub(crate) fn collectors(&self) -> &[MetricsCollector] {
        &self.inner.collectors
    }

    /// Same state with what the server knows about its database engine
    /// (FB-148): shown by `/api/system/info`, checked by `/health/ready`.
    pub fn with_database_runtime(mut self, runtime: DatabaseRuntime) -> Self {
        Arc::make_mut(&mut self.inner).database = runtime;
        self
    }

    /// What the server knows about its database engine.
    pub fn database_runtime(&self) -> &DatabaseRuntime {
        &self.inner.database
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
    /// Routes and OpenAPI fragments of feature crates built on this crate
    /// (jobs, FB-61), mounted under `/api/v1` inside the same middleware.
    pub extensions: Vec<ApiExtension>,
    /// Admission checks run on every request after authentication and
    /// tenancy, before the handler (plan-based API rate limits, FB-24).
    pub gates: Vec<Arc<dyn RequestGate>>,
    /// Who may read `/metrics` and `/api/system/*` ([`routes::ops_gate`]).
    pub ops_access: forgebase_core::OpsAccess,
    /// The running configuration and its layers (FB-91): serves
    /// `GET /api/system/features` and `PATCH /api/system/config`, and turns
    /// the routes of disabled modules into `404 feature_disabled`. Without
    /// it nothing is gated and both endpoints answer `503`.
    pub system_config: Option<Arc<features::SystemConfig>>,
}

/// Routes of a feature crate that depends on `forgebase-api` (so this crate
/// cannot depend on it), plus the OpenAPI fragment documenting them.
#[derive(Clone)]
pub struct ApiExtension {
    /// Routes relative to `/api/v1`.
    pub routes: Router<AppState>,
    /// Paths and schemas; paths are full (`/api/v1/...`).
    pub openapi: utoipa::openapi::OpenApi,
}

impl std::fmt::Debug for ApiExtension {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiExtension")
            .field("paths", &self.openapi.paths.paths.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl Default for RouterOptions {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(30),
            max_body_bytes: 4 * 1024 * 1024,
            max_import_bytes: 256 * 1024 * 1024,
            cors_allowed_origins: Vec::new(),
            extensions: Vec::new(),
            gates: Vec::new(),
            ops_access: forgebase_core::OpsAccess::default(),
            system_config: None,
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
    router_with(state, opts, Router::new(), None)
}

/// [`router`] plus routes owned by another crate (for example the KV engine's
/// `/api/v1/kv`). `extra` gets the same layers as every other route (request
/// ids, error envelope, anonymous fallback, timeout); `extra_openapi` is
/// merged into `GET /api/openapi.json`.
pub fn router_with(
    state: AppState,
    opts: RouterOptions,
    extra: Router<AppState>,
    extra_openapi: Option<utoipa::openapi::OpenApi>,
) -> Router {
    let auth = state.auth().cloned();
    let policy = state.policy().cloned();
    if let (Some(a), Some(p)) = (&auth, &policy) {
        a.set_realm_creator(realm_creator(p.clone()));
    }
    let state = state.with_limits(Limits {
        max_import_bytes: opts.max_import_bytes,
    });
    let mut v1 = v1::router(&opts);
    for ext in &opts.extensions {
        v1 = v1.merge(ext.routes.clone());
    }
    let auth_doc = auth.as_ref().map(|_| forgebase_auth::openapi());
    let disabled = opts
        .system_config
        .as_ref()
        .map(|c| features::DisabledFeatures::of(c.running()))
        .filter(|d| !d.is_empty());
    let mut doc = openapi::document_with(
        opts.extensions
            .iter()
            .map(|e| &e.openapi)
            .chain(extra_openapi.as_ref())
            .chain(auth_doc.as_ref()),
    );
    // A disabled module is not documented (its routes answer 404).
    if let Some(off) = &disabled {
        doc.paths.paths.retain(|path, _| off.owner(path).is_none());
    }
    let doc = Arc::new(doc);
    let graph_on = opts.system_config.as_ref().is_none_or(|c| c.running().graph.enabled);
    let graph = if graph_on {
        Router::new().nest_service(
            "/api/v1/graph",
            forgebase_graph::router(forgebase_graph::GraphService::new(state.data().clone())),
        )
    } else {
        Router::new()
    };
    let app = Router::new()
        .route("/health", get(routes::health))
        .route("/health/ready", get(routes::ready))
        .merge(
            Router::new()
                .route("/metrics", get(routes::metrics))
                .route("/api/system/info", get(routes::system_info))
                .route_layer(middleware::from_fn_with_state(opts.ops_access, routes::ops_gate)),
        )
        .merge(
            Router::new()
                .route("/api/system/features", get(features::get_features))
                .route("/api/system/config", patch(features::patch_config))
                .with_state(opts.system_config.clone()),
        )
        .route("/api/openapi.json", get(move || openapi::serve(doc.clone())))
        .nest("/api/v1", v1)
        .merge(graph)
        .merge(extra)
        .nest_service("/api/v1/realtime", realtime_router(state.realtime()))
        .route("/admin", get(admin::redirect))
        .route("/admin/", get(admin::index))
        .route("/admin/{*path}", get(admin::asset))
        .fallback(routes::not_found)
        .with_state(state)
        .layer(middleware::from_fn(principal::install_anonymous));
    // Gates run after tenancy (outside them) has completed the context.
    let app = if opts.gates.is_empty() {
        app
    } else {
        let gates: Arc<[Arc<dyn RequestGate>]> = opts.gates.clone().into();
        app.layer(middleware::from_fn_with_state(gates, gate::run))
    };
    let app = with_policy_layer(app, policy);
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
    let app = match disabled {
        Some(off) => app.layer(middleware::from_fn_with_state(off, features::gate)),
        None => app,
    };
    app.layer(middleware::from_fn(context::request_context))
}

/// Lets `POST /api/auth/realms` create the realm record through the policy
/// engine, which owns realms (and reloads its snapshot).
fn realm_creator(policy: PolicyService) -> forgebase_auth::realm::RealmCreator {
    use forgebase_auth::AuthError;
    use forgebase_core::{ErrorCode, ErrorKind};
    Arc::new(move |ctx, id, name| {
        let policy = policy.clone();
        Box::pin(async move {
            policy.create_realm(&ctx, &id, &name).await.map(|_| ()).map_err(|e| {
                let m = e.to_string();
                match e.kind() {
                    ErrorKind::InvalidInput => AuthError::InvalidInput(m),
                    ErrorKind::Unauthenticated => AuthError::Unauthenticated,
                    ErrorKind::Forbidden => AuthError::Forbidden(m),
                    ErrorKind::NotFound => AuthError::NotFound(m),
                    ErrorKind::Conflict => AuthError::Conflict(m),
                    _ => AuthError::Internal(m),
                }
            })
        })
    })
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

/// Mounts the policy and tenancy routes and installs the tenancy layer, which
/// runs after authentication and before [`principal::install_anonymous`].
fn with_policy_layer(app: Router, policy: Option<PolicyService>) -> Router {
    match policy {
        Some(p) => app
            .merge(forgebase_policy::http::router(p.clone()))
            .layer(middleware::from_fn_with_state(p, forgebase_policy::http::tenancy)),
        None => app,
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
        app_with(RouterOptions::default()).await
    }

    async fn app_with(opts: RouterOptions) -> (Router, tempfile::TempDir) {
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
        (router(AppState::new(db, layout).await.unwrap(), opts), tmp)
    }

    async fn call(
        app: &Router,
        method: Method,
        uri: &str,
        headers: &[(&str, &str)],
    ) -> (StatusCode, header::HeaderMap, serde_json::Value, String) {
        // A direct loopback client, as the server's `ConnectInfo` reports it.
        let peer = std::net::SocketAddr::from(([127, 0, 0, 1], 40000));
        let mut req = axum::http::Request::builder()
            .method(method)
            .uri(uri)
            .extension(axum::extract::ConnectInfo(peer));
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

    /// FB-148: the backend, the engine and the feature engines, on the test
    /// backend (`FORGEBASE_TEST_BACKEND=postgres` for PostgreSQL).
    #[tokio::test]
    async fn info_and_readiness_report_the_backend_and_the_engine() {
        let test = forgebase_db::testing::test_backend().await;
        let layout = DataLayout::new(test.data_dir().join("data"));
        layout.ensure().unwrap();
        let pg = test.kind() == forgebase_db::BackendKind::Postgres;
        let extensions = vec![
            ("vector", "vector", "auto"),
            ("graph", "age", "off"),
            ("queue", "pgmq", "auto"),
            ("cron", "pg_cron", "on"),
            ("trigram", "pg_trgm", "auto"),
        ];
        let engine_state = std::sync::Arc::new(std::sync::Mutex::new("ready"));
        let seen = engine_state.clone();
        let runtime = DatabaseRuntime {
            mode: pg.then_some("managed"),
            location: pg.then(|| "/srv/fb/postgres/data".to_owned()),
            extensions: if pg { extensions } else { Vec::new() },
            engine: pg.then(|| {
                let f: Arc<dyn Fn() -> EngineReport + Send + Sync> = Arc::new(move || EngineReport {
                    state: *seen.lock().unwrap(),
                    version: "18.6".into(),
                    pid: Some(4242),
                    restarts: 1,
                    last_error: Some("the postmaster exited unexpectedly".into()),
                    socket_dir: Some("/srv/fb/postgres/run".into()),
                    tcp: None,
                    warnings: Vec::new(),
                });
                f
            }),
        };
        let state = AppState::new(test.database().clone(), layout)
            .await
            .unwrap()
            .with_database_runtime(runtime);
        let app = router(state, RouterOptions::default());

        let (s, _, j, text) = call(&app, Method::GET, "/api/system/info", &[]).await;
        assert_eq!(s, StatusCode::OK, "{text}");
        let d = &j["database"];
        assert_eq!(d["backend"], if pg { "postgres" } else { "sqlite" });
        assert!(d["server_version"].as_str().is_some_and(|v| !v.is_empty()), "{d}");
        if pg {
            assert_eq!(d["mode"], "managed");
            assert_eq!(d["path"], "/srv/fb/postgres/data");
            assert_eq!(d["engine"]["state"], "ready");
            assert_eq!(d["engine"]["version"], "18.6");
            assert_eq!(d["engine"]["pid"], 4242);
            assert!(d["connections"].as_u64().is_some(), "{d}");
            let f = d["features"].as_object().unwrap();
            assert_eq!(
                f.keys().map(String::as_str).collect::<Vec<_>>(),
                ["cron", "graph", "queue", "trigram", "vector"]
            );
            for (name, v) in f {
                assert_eq!(v["engine"], "portable", "{name}: {v}");
            }
            assert_eq!(f["graph"]["state"], "off");
            assert_eq!(f["graph"]["extension"], "age");
            // pg_trgm is a contrib module: every PostgreSQL has it.
            assert!(f["trigram"]["available"].as_str().is_some(), "{f:?}");
            assert!(
                ["available", "installed"].contains(&f["trigram"]["state"].as_str().unwrap()),
                "{f:?}"
            );
        } else {
            assert_eq!(d["mode"], serde_json::Value::Null);
            assert_eq!(d["engine"], serde_json::Value::Null);
            assert_eq!(d["features"], serde_json::json!({}));
        }

        // The readiness body keeps its contract shape (docs/PLAN.md §5).
        let (s, _, j, _) = call(&app, Method::GET, "/health/ready", &[]).await;
        assert_eq!(s, StatusCode::OK, "{j}");
        assert_eq!(
            j,
            serde_json::json!({"status": "ready", "checks": {"database": "ok", "storage": "ok"}})
        );
        if pg {
            // A restarting engine fails readiness (ADR-0028 §4.3).
            *engine_state.lock().unwrap() = "restarting";
            let (s, _, j, _) = call(&app, Method::GET, "/health/ready", &[]).await;
            assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{j}");
            assert_eq!(j["checks"]["database"], "error");
            assert_eq!(
                j["errors"]["database"],
                "the bundled PostgreSQL is restarting: the postmaster exited unexpectedly"
            );
        }
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
    async fn ops_endpoints_refuse_proxied_and_remote_anonymous_callers() {
        let (app, _tmp) = app().await;
        for path in ["/metrics", "/api/system/info"] {
            let (s, _, _, _) = call(&app, Method::GET, path, &[]).await;
            assert_eq!(s, StatusCode::OK, "{path}: direct loopback client");
            for h in ["x-forwarded-for", "forwarded", "x-real-ip"] {
                let (s, _, j, _) = call(&app, Method::GET, path, &[(h, "203.0.113.9")]).await;
                assert_eq!(s, StatusCode::UNAUTHORIZED, "{path} behind a proxy ({h})");
                assert_eq!(j["error"]["code"], "unauthenticated");
            }
            // No socket address at all (an in-process call): not loopback.
            let res = app
                .clone()
                .oneshot(axum::http::Request::get(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "{path} without a peer");
        }
        let (admin_only, _tmp2) = app_with(RouterOptions {
            ops_access: forgebase_core::OpsAccess::Admin,
            ..RouterOptions::default()
        })
        .await;
        let (s, _, _, _) = call(&admin_only, Method::GET, "/metrics", &[]).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "admin mode: loopback is not enough");
        let (s, _, _, _) = call(&app, Method::GET, "/health", &[("x-forwarded-for", "203.0.113.9")]).await;
        assert_eq!(s, StatusCode::OK, "health stays open");
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
