//! Named direct PostgreSQL logins (FB-153, rls-coverage §3.5):
//! `/api/v1/system/db-logins`. Administrators only (an administrator's
//! session already passed MFA when MFA is on); `admin` logins need a
//! platform administrator. Every change is audited (`db.login.*` in
//! `GET /api/auth/audit`). The password is in the answer of create and
//! rotate only; ForgeBase keeps its SCRAM verifier in PostgreSQL, never the
//! password. 400 `db_logins_unavailable` on SQLite.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use forgebase_policy::PolicyService;
use forgebase_policy::logins::{DbLogin, LoginSecret, LoginSpec};
use serde::Deserialize;
use serde_json::json;

use super::ApiResult;
use crate::AppState;
use crate::error::ApiError;
use crate::http::ApiJson;
use crate::principal::Access;

fn policy(s: &AppState) -> ApiResult<&PolicyService> {
    s.policy().ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "db_logins_unavailable",
            "direct logins need the policy service",
        )
    })
}

fn err(e: &forgebase_policy::PolicyError) -> ApiError {
    ApiError::from_error(e)
}

/// `{items}` of `GET /system/db-logins`.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub struct DbLoginList {
    /// Logins, by name.
    pub items: Vec<DbLogin>,
}

/// Body of `POST /system/db-logins/{name}/rotate`.
#[derive(Debug, Default, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RotateBody {
    /// New expiry (RFC 3339); absent keeps the current one.
    #[serde(default)]
    pub valid_until: Option<String>,
}

/// Every named login.
#[utoipa::path(
    get, path = "/api/v1/system/db-logins", operation_id = "db_logins_list", tag = "system",
    responses((status = 200, description = "Logins", body = DbLoginList))
)]
pub async fn list(State(s): State<AppState>, Access(ctx): Access) -> ApiResult<Response> {
    let items = policy(&s)?.list_db_logins(&ctx).await.map_err(|e| err(&e))?;
    Ok(Json(json!({ "items": items })).into_response())
}

/// Creates a login bound to one principal and at most one tenant. The
/// password is shown once.
#[utoipa::path(
    post, path = "/api/v1/system/db-logins", operation_id = "db_logins_create", tag = "system",
    request_body = LoginSpec,
    responses((status = 201, description = "The login and its password", body = LoginSecret))
)]
pub async fn create(
    State(s): State<AppState>,
    Access(ctx): Access,
    ApiJson(spec): ApiJson<LoginSpec>,
) -> ApiResult<Response> {
    let out = policy(&s)?.create_db_login(&ctx, spec).await.map_err(|e| err(&e))?;
    Ok((StatusCode::CREATED, Json(out)).into_response())
}

/// One login.
#[utoipa::path(
    get, path = "/api/v1/system/db-logins/{name}", operation_id = "db_logins_get", tag = "system",
    params(("name" = String, Path, description = "Login name")),
    responses((status = 200, description = "The login", body = DbLogin))
)]
pub async fn get(State(s): State<AppState>, Access(ctx): Access, Path(name): Path<String>) -> ApiResult<Response> {
    let login = policy(&s)?.get_db_login(&ctx, &name).await.map_err(|e| err(&e))?;
    Ok(Json(login).into_response())
}

/// A new password (and optionally a new expiry). Open sessions stay.
#[utoipa::path(
    post, path = "/api/v1/system/db-logins/{name}/rotate", operation_id = "db_logins_rotate", tag = "system",
    params(("name" = String, Path, description = "Login name")),
    request_body = RotateBody,
    responses((status = 200, description = "The login and its new password", body = LoginSecret))
)]
pub async fn rotate(
    State(s): State<AppState>,
    Access(ctx): Access,
    Path(name): Path<String>,
    body: axum::body::Bytes,
) -> ApiResult<Response> {
    // The body is optional: no body keeps the expiry.
    let body: RotateBody = if body.iter().all(u8::is_ascii_whitespace) {
        RotateBody::default()
    } else {
        serde_json::from_slice(&body).map_err(|e| ApiError::bad_request("invalid_body", e.to_string()))?
    };
    let out = policy(&s)?
        .rotate_db_login(&ctx, &name, body.valid_until)
        .await
        .map_err(|e| err(&e))?;
    Ok(Json(out).into_response())
}

/// Drops a login: its sessions end and its role is removed.
#[utoipa::path(
    delete, path = "/api/v1/system/db-logins/{name}", operation_id = "db_logins_delete", tag = "system",
    params(("name" = String, Path, description = "Login name")),
    responses((status = 204, description = "Dropped"))
)]
pub async fn delete(State(s): State<AppState>, Access(ctx): Access, Path(name): Path<String>) -> ApiResult<Response> {
    policy(&s)?.delete_db_login(&ctx, &name).await.map_err(|e| err(&e))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
