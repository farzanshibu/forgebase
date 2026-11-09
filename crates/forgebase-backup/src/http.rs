//! `/api/v1/system/backups`: snapshots over HTTP (administrators only).
//!
//! Restore is deliberately not an HTTP operation: it replaces the files the
//! running server has open. Use `forgebase restore` with the server stopped.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use forgebase_api::http::ApiJson;
use forgebase_api::principal::Access;
use forgebase_api::{ApiError, ApiExtension};
use forgebase_core::ErrorCode;
use forgebase_data::AccessContext;
use serde::{Deserialize, Serialize};
use utoipa::{OpenApi, ToSchema};

use crate::backup::BackupOptions;
use crate::error::BackupError;
use crate::manifest::Manifest;
use crate::snapshots::Retention;
use crate::verify::{VerifyReport, verify};
use crate::{BackupService, Snapshot};

type ApiResult = Result<Response, ApiError>;
type Body<T> = Result<ApiJson<T>, ApiError>;

fn api(e: BackupError) -> ApiError {
    ApiError::from_error(&e)
}

fn guard(ctx: &AccessContext) -> Result<(), ApiError> {
    if ctx.is_privileged() {
        Ok(())
    } else if matches!(ctx.principal, forgebase_data::access::Principal::Anonymous) {
        Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "unauthenticated",
            "authentication required",
        ))
    } else {
        Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "backups are for administrators",
        ))
    }
}

/// The routes, relative to `/api/v1`, with their state applied.
pub fn router<S: Clone + Send + Sync + 'static>(service: BackupService) -> Router<S> {
    Router::<BackupService>::new()
        .route("/system/backups", get(list).post(create))
        .route("/system/backups/prune", post(prune))
        .route("/system/backups/{id}", get(get_one).delete(delete_one))
        .route("/system/backups/{id}/verify", post(verify_one))
        .with_state(service)
}

/// The routes and OpenAPI fragment, ready for `RouterOptions::extensions`.
pub fn extension(service: BackupService) -> ApiExtension {
    ApiExtension {
        routes: router(service),
        openapi: BackupDoc::openapi(),
    }
}

#[derive(OpenApi)]
#[openapi(
    paths(list, create, get_one, delete_one, verify_one, prune),
    components(schemas(
        SnapshotView, SnapshotList, CreateSnapshot, PruneResult, Manifest, VerifyReport, Retention,
        crate::manifest::FileEntry, crate::manifest::FileKind, crate::manifest::KeyInfo,
    )),
    tags((name = "backups", description = "Point-in-time snapshots of the data directory, with retention and verification"))
)]
struct BackupDoc;

/// A snapshot.
#[derive(Debug, Serialize, ToSchema)]
pub struct SnapshotView {
    /// Snapshot id (its UTC creation time).
    pub id: String,
    /// Manifest with every file's size and SHA-256.
    pub manifest: Manifest,
}

impl From<Snapshot> for SnapshotView {
    fn from(s: Snapshot) -> Self {
        Self {
            id: s.id,
            manifest: s.manifest,
        }
    }
}

/// Snapshots, newest first.
#[derive(Debug, Serialize, ToSchema)]
pub struct SnapshotList {
    /// The snapshots.
    pub items: Vec<SnapshotView>,
}

/// Body of `POST /system/backups`.
#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct CreateSnapshot {
    /// Free-form label.
    #[serde(default)]
    pub label: Option<String>,
    /// Include derived index files (default true).
    #[serde(default)]
    pub include_indexes: Option<bool>,
    /// Retention applied after the snapshot is taken.
    #[serde(default)]
    pub retention: Option<Retention>,
}

/// Snapshots removed by a prune.
#[derive(Debug, Serialize, ToSchema)]
pub struct PruneResult {
    /// Ids of the deleted snapshots.
    pub deleted: Vec<String>,
}

/// Lists snapshots.
#[utoipa::path(get, path = "/api/v1/system/backups", tag = "backups",
    responses((status = 200, description = "Snapshots", body = SnapshotList)))]
async fn list(State(s): State<BackupService>, Access(ctx): Access) -> ApiResult {
    guard(&ctx)?;
    let items = s
        .store()
        .list()
        .map_err(api)?
        .into_iter()
        .map(SnapshotView::from)
        .collect();
    Ok(Json(SnapshotList { items }).into_response())
}

/// Takes a snapshot now (online, consistent) and applies the retention.
#[utoipa::path(post, path = "/api/v1/system/backups", tag = "backups", request_body = CreateSnapshot,
    responses((status = 201, description = "The new snapshot", body = Manifest)))]
async fn create(State(s): State<BackupService>, Access(ctx): Access, body: Body<CreateSnapshot>) -> ApiResult {
    guard(&ctx)?;
    let req = body?.0;
    let opts = BackupOptions {
        label: req.label,
        include_indexes: req.include_indexes.unwrap_or(true),
        ..BackupOptions::default()
    };
    let m = s.snapshot(opts, req.retention).await.map_err(api)?;
    Ok((StatusCode::CREATED, Json(m)).into_response())
}

/// One snapshot.
#[utoipa::path(get, path = "/api/v1/system/backups/{id}", tag = "backups",
    params(("id" = String, Path, description = "Snapshot id")),
    responses((status = 200, description = "The snapshot", body = SnapshotView), (status = 404, description = "No such snapshot")))]
async fn get_one(State(s): State<BackupService>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult {
    guard(&ctx)?;
    Ok(Json(SnapshotView::from(s.store().get(&id).map_err(api)?)).into_response())
}

/// Deletes a snapshot.
#[utoipa::path(delete, path = "/api/v1/system/backups/{id}", tag = "backups",
    params(("id" = String, Path, description = "Snapshot id")),
    responses((status = 204, description = "Deleted"), (status = 404, description = "No such snapshot")))]
async fn delete_one(State(s): State<BackupService>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult {
    guard(&ctx)?;
    s.store().delete(&id).await.map_err(api)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Verifies a snapshot: manifest, every file's hash, database integrity.
#[utoipa::path(post, path = "/api/v1/system/backups/{id}/verify", tag = "backups",
    params(("id" = String, Path, description = "Snapshot id")),
    responses((status = 200, description = "Verification report", body = VerifyReport), (status = 404, description = "No such snapshot")))]
async fn verify_one(State(s): State<BackupService>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult {
    guard(&ctx)?;
    let snap = s.store().get(&id).map_err(api)?;
    Ok(Json(verify(&snap.path).await).into_response())
}

/// Applies a retention policy now.
#[utoipa::path(post, path = "/api/v1/system/backups/prune", tag = "backups", request_body = Retention,
    responses((status = 200, description = "Deleted snapshots", body = PruneResult)))]
async fn prune(State(s): State<BackupService>, Access(ctx): Access, body: Body<Retention>) -> ApiResult {
    guard(&ctx)?;
    let deleted = s.store().prune(body?.0).await.map_err(api)?;
    Ok(Json(PruneResult { deleted }).into_response())
}

#[allow(dead_code)]
fn _error_code_in_scope(e: &BackupError) -> &'static str {
    e.code()
}
