//! Health, readiness, metrics and system-info handlers.
//!
//! Response shapes follow the Phase 1 interface contract (docs/PLAN.md §5).
//! `/metrics` and `/api/system/*` sit behind [`ops_gate`] (`[server]
//! ops_access`, FB-80); `/health*` stays open for load balancers.

use std::net::{IpAddr, SocketAddr};

use axum::Json;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::StatusCode;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, HeaderValue};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use forgebase_core::OpsAccess;
use forgebase_data::{AccessContext, Principal};
use forgebase_observability::names;
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::AppState;
use crate::error::ApiError;

/// `GET /health`: liveness. Answers as long as the process serves HTTP.
pub async fn health() -> Response {
    no_store(Json(json!({ "status": "ok" })).into_response())
}

/// `GET /health/ready`: readiness. `200` when the database answers and is
/// fully migrated and the storage directory is writable; `503` naming the
/// failing check otherwise.
pub async fn ready(State(state): State<AppState>) -> Response {
    let mut checks = Map::new();
    let mut errors = Map::new();

    let database = match state.db().ping().await {
        Ok(()) => match state.db().migration_status().await {
            Ok(s) if s.is_up_to_date() => Ok(()),
            Ok(s) => Err(format!(
                "schema at version {} of {}, {} problem(s)",
                s.current_version,
                s.latest_version,
                s.problems.len()
            )),
            Err(e) => Err(e.to_string()),
        },
        Err(e) => Err(e.to_string()),
    };
    record(&mut checks, &mut errors, "database", database);
    record(&mut checks, &mut errors, "storage", probe_storage(&state).await);

    let ok = errors.is_empty();
    let mut body = Map::new();
    body.insert("status".into(), Value::from(if ok { "ready" } else { "not_ready" }));
    body.insert("checks".into(), Value::Object(checks));
    if !ok {
        let summary = Value::Object(errors.clone());
        tracing::warn!(errors = %summary, "readiness check failed");
        body.insert("errors".into(), Value::Object(errors));
    }
    let status = if ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    no_store((status, Json(Value::Object(body))).into_response())
}

fn record(checks: &mut Map<String, Value>, errors: &mut Map<String, Value>, name: &str, result: Result<(), String>) {
    match result {
        Ok(()) => {
            checks.insert(name.into(), Value::from("ok"));
        }
        Err(message) => {
            checks.insert(name.into(), Value::from("error"));
            errors.insert(name.into(), Value::from(message));
        }
    }
}

/// Writes and removes a small file in `storage/`.
async fn probe_storage(state: &AppState) -> Result<(), String> {
    let dir = state.layout().storage_dir();
    let probe = dir.join(format!(".ready-{}", uuid::Uuid::new_v4()));
    tokio::fs::write(&probe, b"ok")
        .await
        .map_err(|e| format!("storage directory {} is not writable: {e}", dir.display()))?;
    tokio::fs::remove_file(&probe)
        .await
        .map_err(|e| format!("cannot clean up probe file in {}: {e}", dir.display()))
}

/// The database part of the system info.
#[derive(Debug, Serialize)]
struct DatabaseSummary {
    path: String,
    size_bytes: u64,
    journal_mode: String,
    schema_version: i64,
}

/// `GET /api/system/info`.
#[derive(Debug, Serialize)]
pub struct SystemInfo {
    name: &'static str,
    version: &'static str,
    mode: &'static str,
    data_dir: String,
    started_at: String,
    uptime_secs: u64,
    database: DatabaseSummary,
}

/// `GET /api/system/info`: read-only instance information.
pub async fn system_info(State(state): State<AppState>) -> Result<Response, ApiError> {
    let db = state.db().info().await?;
    let info = SystemInfo {
        name: forgebase_core::build::NAME,
        version: forgebase_core::build::VERSION,
        mode: "embedded",
        data_dir: state.layout().root().display().to_string(),
        started_at: state.started_at().to_owned(),
        uptime_secs: state.uptime_secs(),
        database: DatabaseSummary {
            path: db.path.display().to_string(),
            size_bytes: db.size_bytes,
            journal_mode: db.journal_mode,
            schema_version: db.schema_version,
        },
    };
    Ok(no_store(Json(info).into_response()))
}

/// `GET /metrics`: Prometheus text exposition format.
pub async fn metrics(State(state): State<AppState>) -> Response {
    metrics::gauge!(names::UPTIME_SECONDS).set(state.uptime_secs() as f64);
    match state.db().info().await {
        Ok(info) => {
            metrics::gauge!(names::DB_SIZE_BYTES).set(info.size_bytes as f64);
            metrics::gauge!(names::DB_WAL_SIZE_BYTES).set(info.wal_size_bytes as f64);
            metrics::gauge!(names::DB_SCHEMA_VERSION).set(info.schema_version as f64);
        }
        Err(e) => tracing::warn!(error = %e, "could not read database stats for /metrics"),
    }
    let handle = state.metrics();
    handle.run_upkeep();
    (
        StatusCode::OK,
        [
            (
                CONTENT_TYPE,
                HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
            ),
            (CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ],
        handle.render(),
    )
        .into_response()
}

/// Admission of `/metrics` and `/api/system/*` (FB-80): administrators and
/// admin service accounts always; unauthenticated callers only under
/// [`OpsAccess::Public`], or under [`OpsAccess::Loopback`] when the socket
/// peer is a loopback address and the request carries no forwarding header
/// (a reverse proxy on the same host forwards remote clients).
pub async fn ops_gate(State(access): State<OpsAccess>, req: Request, next: Next) -> Response {
    let open = match access {
        OpsAccess::Public => true,
        OpsAccess::Loopback => direct_loopback(&req),
        OpsAccess::Admin => false,
    };
    if !open {
        match req.extensions().get::<AccessContext>() {
            Some(ctx) if ctx.is_privileged() => {}
            Some(ctx) if ctx.principal != Principal::Anonymous => {
                return ApiError::new(
                    StatusCode::FORBIDDEN,
                    "forbidden",
                    "operational endpoints are for administrators",
                )
                .into_response();
            }
            _ => {
                return ApiError::new(
                    StatusCode::UNAUTHORIZED,
                    "unauthenticated",
                    "operational endpoints need an administrator's credentials",
                )
                .into_response();
            }
        }
    }
    next.run(req).await
}

/// Headers a reverse proxy adds; their presence means the socket peer is a
/// proxy, not the client.
const FORWARDING_HEADERS: [&str; 3] = ["forwarded", "x-forwarded-for", "x-real-ip"];

fn direct_loopback(req: &Request) -> bool {
    let peer = req.extensions().get::<ConnectInfo<SocketAddr>>().map(|c| c.0.ip());
    let loopback = match peer {
        Some(IpAddr::V4(a)) => a.is_loopback(),
        Some(IpAddr::V6(a)) => a.is_loopback() || a.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback()),
        None => false,
    };
    loopback && !FORWARDING_HEADERS.iter().any(|h| req.headers().contains_key(*h))
}

/// Fallback for unknown routes.
pub async fn not_found() -> ApiError {
    ApiError::not_found("no route for this path")
}

fn no_store(mut res: Response) -> Response {
    res.headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}
