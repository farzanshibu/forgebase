//! Index management, statistics, query plans, the slow-query log.

use std::time::Duration;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use forgebase_data::IndexDefinition;
use serde::Deserialize;
use serde_json::json;

use super::ApiResult;
use super::schemas::{
    AnalyzeBody, IndexDefinitionSchema, IndexInfoSchema, IndexList, IndexRecommendations, IndexStatsSchema,
    SlowQueryLog, ThresholdBody,
};
use crate::AppState;
use crate::http::{ApiJson, Query};
use crate::principal::Access;

/// Lists indexes with their statistics.
#[utoipa::path(
    get, path = "/api/v1/indexes", operation_id = "indexes_list", tag = "indexes",
    description = "Query string: `collection` limits the list to one collection.",
    responses((status = 200, description = "Indexes", body = IndexList))
)]
pub async fn list(State(s): State<AppState>, Access(ctx): Access, q: Query) -> ApiResult<Response> {
    let items = s.data().list_indexes(&ctx, q.get("collection")).await?;
    Ok(Json(json!({ "items": items })).into_response())
}

/// Creates an index: B-tree (single, composite, unique, partial, expression,
/// JSON path) or `fulltext`.
#[utoipa::path(
    post, path = "/api/v1/indexes", operation_id = "indexes_create", tag = "indexes",
    request_body = IndexDefinitionSchema,
    responses((status = 201, description = "Created", body = IndexInfoSchema), (status = 409, description = "Name taken or duplicate values for a unique index"))
)]
pub async fn create(
    State(s): State<AppState>,
    Access(ctx): Access,
    ApiJson(def): ApiJson<IndexDefinition>,
) -> ApiResult<Response> {
    let info = s.data().create_index(&ctx, def).await?;
    Ok((StatusCode::CREATED, Json(info)).into_response())
}

/// Index recommendations derived from the slow-query log.
#[utoipa::path(
    get, path = "/api/v1/indexes/recommendations", tag = "indexes",
    responses((status = 200, description = "Suggested indexes", body = IndexRecommendations))
)]
pub async fn recommendations(State(s): State<AppState>, Access(ctx): Access) -> ApiResult<Response> {
    let items = s.data().recommend_indexes(&ctx).await?;
    Ok(Json(json!({ "items": items })).into_response())
}

/// Body of `POST /indexes/analyze`.
#[derive(Deserialize, Default)]
#[serde(default)]
pub struct AnalyzeRequest {
    collection: Option<String>,
}

/// Refreshes planner statistics (`ANALYZE`).
#[utoipa::path(
    post, path = "/api/v1/indexes/analyze", tag = "indexes",
    request_body = AnalyzeBody,
    responses((status = 204, description = "Statistics refreshed"))
)]
pub async fn analyze(
    State(s): State<AppState>,
    Access(ctx): Access,
    ApiJson(b): ApiJson<AnalyzeRequest>,
) -> ApiResult<Response> {
    s.data().analyze(&ctx, b.collection.as_deref()).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// One index.
#[utoipa::path(
    get, path = "/api/v1/indexes/{name}", operation_id = "indexes_get", tag = "indexes",
    params(("name" = String, Path, description = "Index name")),
    responses((status = 200, description = "The index", body = IndexInfoSchema))
)]
pub async fn get(State(s): State<AppState>, Access(ctx): Access, Path(name): Path<String>) -> ApiResult<Response> {
    Ok(Json(s.data().get_index(&ctx, &name).await?).into_response())
}

/// Drops an index (manual indexes only; schema-derived ones follow their field).
#[utoipa::path(
    delete, path = "/api/v1/indexes/{name}", tag = "indexes",
    params(("name" = String, Path, description = "Index name")),
    responses((status = 204, description = "Dropped"))
)]
pub async fn drop(State(s): State<AppState>, Access(ctx): Access, Path(name): Path<String>) -> ApiResult<Response> {
    s.data().drop_index(&ctx, &name).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Rebuilds an index (`REINDEX` + `ANALYZE`, or re-populates a full-text index).
#[utoipa::path(
    post, path = "/api/v1/indexes/{name}/rebuild", operation_id = "indexes_rebuild", tag = "indexes",
    params(("name" = String, Path, description = "Index name")),
    responses((status = 200, description = "The rebuilt index", body = IndexInfoSchema))
)]
pub async fn rebuild(State(s): State<AppState>, Access(ctx): Access, Path(name): Path<String>) -> ApiResult<Response> {
    Ok(Json(s.data().rebuild_index(&ctx, &name).await?).into_response())
}

/// Statistics of one index: planner statistics, estimated rows, size on disk.
#[utoipa::path(
    get, path = "/api/v1/indexes/{name}/stats", operation_id = "indexes_stats", tag = "indexes",
    params(("name" = String, Path, description = "Index name")),
    responses((status = 200, description = "Index statistics", body = IndexStatsSchema))
)]
pub async fn stats(State(s): State<AppState>, Access(ctx): Access, Path(name): Path<String>) -> ApiResult<Response> {
    Ok(Json(s.data().get_index(&ctx, &name).await?.stats).into_response())
}

/// Recent statements slower than the threshold (SQL text only, never parameter values).
#[utoipa::path(
    get, path = "/api/v1/slow-queries", tag = "indexes",
    responses((status = 200, description = "The slow-query log", body = SlowQueryLog))
)]
pub async fn slow_queries(State(s): State<AppState>, Access(ctx): Access) -> ApiResult<Response> {
    let items = s.data().slow_queries(&ctx)?;
    let threshold_ms = u64::try_from(s.data().slow_query_threshold().as_millis()).unwrap_or(u64::MAX);
    Ok(Json(json!({ "threshold_ms": threshold_ms, "items": items })).into_response())
}

/// Clears the slow-query log.
#[utoipa::path(
    delete, path = "/api/v1/slow-queries", tag = "indexes",
    responses((status = 204, description = "Cleared"))
)]
pub async fn clear_slow_queries(State(s): State<AppState>, Access(ctx): Access) -> ApiResult<Response> {
    s.data().clear_slow_queries(&ctx)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Body of `PUT /slow-queries/threshold`.
#[derive(Deserialize)]
pub struct ThresholdRequest {
    threshold_ms: u64,
}

/// Sets the slow-query threshold (`0` logs every statement).
#[utoipa::path(
    put, path = "/api/v1/slow-queries/threshold", tag = "indexes",
    request_body = ThresholdBody,
    responses((status = 200, description = "The new threshold", body = ThresholdBody))
)]
pub async fn set_threshold(
    State(s): State<AppState>,
    Access(ctx): Access,
    ApiJson(b): ApiJson<ThresholdRequest>,
) -> ApiResult<Response> {
    s.data()
        .set_slow_query_threshold(&ctx, Duration::from_millis(b.threshold_ms))?;
    Ok(Json(json!({ "threshold_ms": b.threshold_ms })).into_response())
}
