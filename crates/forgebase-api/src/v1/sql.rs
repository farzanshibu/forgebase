//! SQL editor endpoints. Authorisation (`ReadSql` / `WriteSql`) and statement
//! classification are the data core's (ADR-0003 §6); the default authorizer
//! allows administrators only.

use axum::Json;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use forgebase_data::SqlStatement;
use serde::Deserialize;
use serde_json::json;

use super::ApiResult;
use super::schemas::{
    QueryPlanSchema, SqlBatchBody, SqlBatchResult, SqlResultSchema, SqlStatementBody, TableInfoSchema, TableList,
};
use crate::AppState;
use crate::http::ApiJson;
use crate::principal::Access;

/// Runs one parameterised SQL statement. Reads return at most 10 000 rows
/// (`truncated`); transaction control, `ATTACH`, `VACUUM` and writable pragmas are refused.
#[utoipa::path(
    post, path = "/api/v1/sql", tag = "sql",
    request_body = SqlStatementBody,
    responses((status = 200, description = "Result", body = SqlResultSchema))
)]
pub async fn execute(
    State(s): State<AppState>,
    Access(ctx): Access,
    ApiJson(stmt): ApiJson<SqlStatement>,
) -> ApiResult<Response> {
    Ok(Json(s.data().execute_sql(&ctx, stmt).await?).into_response())
}

/// Body of `POST /sql/batch`.
#[derive(Deserialize)]
pub struct BatchRequest {
    statements: Vec<SqlStatement>,
}

/// Runs 1 to 1000 statements in one all-or-nothing transaction.
#[utoipa::path(
    post, path = "/api/v1/sql/batch", tag = "sql",
    request_body = SqlBatchBody,
    responses((status = 200, description = "One result per statement", body = SqlBatchResult))
)]
pub async fn batch(
    State(s): State<AppState>,
    Access(ctx): Access,
    ApiJson(b): ApiJson<BatchRequest>,
) -> ApiResult<Response> {
    let items = s.data().execute_sql_batch(&ctx, b.statements).await?;
    Ok(Json(json!({ "items": items })).into_response())
}

/// The query plan of a statement (`EXPLAIN QUERY PLAN`), without running it.
#[utoipa::path(
    post, path = "/api/v1/sql/explain", tag = "sql",
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

/// One table or view.
#[utoipa::path(
    get, path = "/api/v1/sql/tables/{name}", tag = "sql",
    params(("name" = String, Path, description = "Table or view name")),
    responses((status = 200, description = "The table", body = TableInfoSchema))
)]
pub async fn table(State(s): State<AppState>, Access(ctx): Access, Path(name): Path<String>) -> ApiResult<Response> {
    Ok(Json(s.data().describe_table(&ctx, &name).await?).into_response())
}
