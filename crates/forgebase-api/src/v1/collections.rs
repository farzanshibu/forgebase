//! Collections: CRUD, schema (JSON Schema of a record) and schema history.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::http::header::LOCATION;
use axum::response::{IntoResponse, Response};
use forgebase_data::schema::Collection;
use serde_json::json;

use super::schemas::{CollectionList, CollectionSchema, CollectionSchemaResponse, SchemaHistory};
use super::{ApiResult, jsonschema};
use crate::AppState;
use crate::http::ApiJson;
use crate::principal::Access;

/// Lists collections.
#[utoipa::path(
    get, path = "/api/v1/collections", tag = "collections",
    responses((status = 200, description = "Every collection definition", body = CollectionList))
)]
pub async fn list(State(s): State<AppState>, Access(ctx): Access) -> ApiResult<Response> {
    let items = s.data().list_collections(&ctx).await?;
    Ok(Json(json!({ "items": items })).into_response())
}

/// Creates a collection (its table, automatic indexes and history entry, atomically).
#[utoipa::path(
    post, path = "/api/v1/collections", tag = "collections",
    request_body = CollectionSchema,
    responses(
        (status = 201, description = "Created", body = CollectionSchema),
        (status = 409, description = "Name already taken"),
    )
)]
pub async fn create(
    State(s): State<AppState>,
    Access(ctx): Access,
    ApiJson(def): ApiJson<Collection>,
) -> ApiResult<Response> {
    let created = s.data().create_collection(&ctx, def).await?;
    let location = format!("/api/v1/collections/{}", created.name);
    let mut res = (StatusCode::CREATED, Json(created)).into_response();
    if let Ok(v) = location.parse() {
        res.headers_mut().insert(LOCATION, v);
    }
    Ok(res)
}

/// One collection by name or id.
#[utoipa::path(
    get, path = "/api/v1/collections/{collection}", tag = "collections",
    params(("collection" = String, Path, description = "Collection name or id")),
    responses((status = 200, description = "The definition", body = CollectionSchema))
)]
pub async fn get(State(s): State<AppState>, Access(ctx): Access, Path(c): Path<String>) -> ApiResult<Response> {
    Ok(Json(s.data().get_collection(&ctx, &c).await?).into_response())
}

/// Replaces a collection's definition: rename, add / rename / change / drop
/// fields (matched by field `id`), change options. Applied as transactional DDL.
#[utoipa::path(
    put, path = "/api/v1/collections/{collection}", tag = "collections",
    params(("collection" = String, Path, description = "Collection name or id")),
    request_body = CollectionSchema,
    responses(
        (status = 200, description = "Updated", body = CollectionSchema),
        (status = 409, description = "Existing data does not fit the new definition"),
    )
)]
pub async fn update(
    State(s): State<AppState>,
    Access(ctx): Access,
    Path(c): Path<String>,
    ApiJson(def): ApiJson<Collection>,
) -> ApiResult<Response> {
    Ok(Json(s.data().update_collection(&ctx, &c, def).await?).into_response())
}

/// Drops a collection and its records (refused while relations point to it).
#[utoipa::path(
    delete, path = "/api/v1/collections/{collection}", tag = "collections",
    params(("collection" = String, Path, description = "Collection name or id")),
    responses((status = 204, description = "Dropped"), (status = 409, description = "Referenced by a relation field"))
)]
pub async fn delete(State(s): State<AppState>, Access(ctx): Access, Path(c): Path<String>) -> ApiResult<Response> {
    s.data().delete_collection(&ctx, &c).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// The collection definition plus a JSON Schema describing its records.
#[utoipa::path(
    get, path = "/api/v1/collections/{collection}/schema", tag = "collections",
    params(("collection" = String, Path, description = "Collection name or id")),
    responses((status = 200, description = "Definition and JSON Schema", body = CollectionSchemaResponse))
)]
pub async fn schema(State(s): State<AppState>, Access(ctx): Access, Path(c): Path<String>) -> ApiResult<Response> {
    let coll = s.data().get_collection(&ctx, &c).await?;
    let json_schema = jsonschema::record_schema(&coll, ctx.is_privileged());
    Ok(Json(json!({ "collection": coll, "json_schema": json_schema })).into_response())
}

/// Append-only history of the collection's schema changes.
#[utoipa::path(
    get, path = "/api/v1/collections/{collection}/history", tag = "collections",
    params(("collection" = String, Path, description = "Collection name or id")),
    responses((status = 200, description = "Changes, oldest first", body = SchemaHistory))
)]
pub async fn history(State(s): State<AppState>, Access(ctx): Access, Path(c): Path<String>) -> ApiResult<Response> {
    let items = s.data().schema_history(&ctx, Some(&c)).await?;
    Ok(Json(json!({ "items": items })).into_response())
}
