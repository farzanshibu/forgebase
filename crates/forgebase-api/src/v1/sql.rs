//! SQL editor endpoints. Authorisation (`ReadSql` / `WriteSql`) and statement
//! classification are the data core's (ADR-0003 §6); the default authorizer
//! allows administrators only.
//!
//! Secrets (rls-coverage G11): values of `password`, `token_key` and password
//! fields come back as `********` (also as keys of JSON documents, with
//! `secrets_masked: true` on the result), and a statement that names such a
//! column is refused with 403, unless the body sends `"show_secrets": true`. Every use of `show_secrets` is recorded as a
//! `sql.show_secrets` entry of the auth audit log (`GET /api/auth/audit`).
//!
//! PostgreSQL (FB-153): the editor runs as `forgebase_sql_admin`, which has no
//! `SELECT` on secret columns, so `show_secrets` cannot reveal them there
//! (the statement fails with 403). `"run_as": {"principal": "user:users/u1",
//! "tenant": "env1"}` runs the statements as a direct login bound to that
//! user would (row and field rules apply, names resolve to the `api`
//! views); DDL is refused with it; each call is audited as `sql.run_as`.

use axum::Json;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use forgebase_data::{AccessContext, SqlOptions, SqlRunAs, SqlStatement};
use serde::Deserialize;
use serde_json::json;

use super::ApiResult;
use super::schemas::{
    QueryPlanSchema, SqlBatchBody, SqlBatchResult, SqlResultSchema, SqlStatementBody, TableInfoSchema, TableList,
};
use crate::AppState;
use crate::error::ApiError;
use crate::http::ApiJson;
use crate::principal::Access;

/// Runs one parameterised SQL statement. Reads return at most 10 000 rows
/// (`truncated`); transaction control, `ATTACH`, `VACUUM` and writable pragmas are refused.
/// Secret columns are masked unless the body sets `show_secrets` (audited).
#[utoipa::path(
    post, path = "/api/v1/sql", tag = "sql",
    request_body = SqlStatementBody,
    responses((status = 200, description = "Result", body = SqlResultSchema))
)]
pub async fn execute(
    State(s): State<AppState>,
    Access(ctx): Access,
    ApiJson(b): ApiJson<ExecuteRequest>,
) -> ApiResult<Response> {
    let run_as = run_as(&s, &ctx, b.run_as.as_ref()).await?;
    Ok(Json(
        s.data()
            .execute_sql_as(&ctx, b.statement, b.opts, run_as.as_ref())
            .await?,
    )
    .into_response())
}

/// `run_as` of a SQL editor body (FB-153): whose claims the statements run
/// with.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RunAsBody {
    /// Principal key: `user:<collection>/<id>` or `service:<id>`.
    pub principal: String,
    /// The tenant to act in.
    #[serde(default)]
    pub tenant: Option<String>,
}

async fn run_as(s: &AppState, ctx: &AccessContext, body: Option<&RunAsBody>) -> Result<Option<SqlRunAs>, ApiError> {
    let Some(b) = body else { return Ok(None) };
    let policy = s
        .policy()
        .ok_or_else(|| ApiError::bad_request("run_as_unavailable", "run as needs the policy service"))?;
    policy
        .run_as_claims(ctx, &b.principal, b.tenant.as_deref())
        .await
        .map(Some)
        .map_err(|e| ApiError::from_error(&e))
}

/// Body of `POST /sql`: `{sql, params, show_secrets, run_as}`.
#[derive(Deserialize)]
pub struct ExecuteRequest {
    #[serde(flatten)]
    statement: SqlStatement,
    #[serde(flatten)]
    opts: SqlOptions,
    #[serde(default)]
    run_as: Option<RunAsBody>,
}

/// Body of `POST /sql/batch`: `{statements, show_secrets, run_as}`.
#[derive(Deserialize)]
pub struct BatchRequest {
    statements: Vec<SqlStatement>,
    #[serde(flatten)]
    opts: SqlOptions,
    #[serde(default)]
    run_as: Option<RunAsBody>,
}

/// Runs 1 to 1000 statements in one all-or-nothing transaction. With
/// `show_secrets` the audit entry commits with the batch.
#[utoipa::path(
    post, path = "/api/v1/sql/batch", operation_id = "sql_batch", tag = "sql",
    request_body = SqlBatchBody,
    responses((status = 200, description = "One result per statement", body = SqlBatchResult))
)]
pub async fn batch(
    State(s): State<AppState>,
    Access(ctx): Access,
    ApiJson(b): ApiJson<BatchRequest>,
) -> ApiResult<Response> {
    let run_as = run_as(&s, &ctx, b.run_as.as_ref()).await?;
    let items = s
        .data()
        .execute_sql_batch_as(&ctx, b.statements, b.opts, run_as.as_ref())
        .await?;
    Ok(Json(json!({ "items": items })).into_response())
}

/// The query plan of a statement (`EXPLAIN QUERY PLAN`), without running it.
#[utoipa::path(
    post, path = "/api/v1/sql/explain", operation_id = "sql_explain", tag = "sql",
    request_body = SqlStatementBody,
    responses((status = 200, description = "Query plan", body = QueryPlanSchema))
)]
pub async fn explain(
    State(s): State<AppState>,
    Access(ctx): Access,
    ApiJson(stmt): ApiJson<SqlStatement>,
) -> ApiResult<Response> {
    Ok(Json(s.data().explain_sql(&ctx, stmt).await?).into_response())
}

/// Tables and views (without ForgeBase system tables) with columns and indexes.
#[utoipa::path(
    get, path = "/api/v1/sql/tables", tag = "sql",
    responses((status = 200, description = "Tables", body = TableList))
)]
pub async fn tables(State(s): State<AppState>, Access(ctx): Access) -> ApiResult<Response> {
    let items = s.data().list_tables(&ctx).await?;
    Ok(Json(json!({ "items": items })).into_response())
}

/// One table or view. System tables are described to administrators only;
/// anyone else gets 404, as for a missing table.
#[utoipa::path(
    get, path = "/api/v1/sql/tables/{name}", tag = "sql",
    params(("name" = String, Path, description = "Table or view name")),
    responses((status = 200, description = "The table", body = TableInfoSchema))
)]
pub async fn table(State(s): State<AppState>, Access(ctx): Access, Path(name): Path<String>) -> ApiResult<Response> {
    Ok(Json(s.data().describe_table(&ctx, &name).await?).into_response())
}
