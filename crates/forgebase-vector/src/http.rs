//! `/api/v1/vector`: collections and their vector fields, upsert, retrieval,
//! similarity and hybrid search, rebuilds, statistics and recall checks.
//!
//! Handlers only translate HTTP. Each one authorises first (anonymous callers
//! get `401`, callers without permission `403`) and only then reads the body,
//! so a caller who may not use an endpoint learns nothing about its input.

use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use forgebase_api::http::ApiJson;
use forgebase_api::principal::Access;
use forgebase_api::{ApiError, ApiExtension};
use forgebase_data::{AccessContext, Action, ResourceOp};
use serde::Serialize;
use tower_http::limit::RequestBodyLimitLayer;
use utoipa::{OpenApi, ToSchema};

use crate::error::VectorError;
use crate::model::*;
use crate::service::VectorService;

type ApiResult = Result<Response, ApiError>;
type Body<T> = Result<ApiJson<T>, ApiError>;

/// Largest JSON body (a batch of 1000 items of a few hundred dimensions).
pub const MAX_BODY: usize = 16 * 1024 * 1024;

fn api(e: VectorError) -> ApiError {
    ApiError::from_error(&e)
}

/// Checks that the caller may do `op` on collection `name` at all before the
/// body is read (FB-139: per collection, so resource keys and rules count).
fn body<T>(
    s: &VectorService,
    ctx: &AccessContext,
    action: Action,
    name: &str,
    op: ResourceOp,
    body: Body<T>,
) -> Result<T, ApiError> {
    s.authorize_collection(ctx, action, name, op).map_err(api)?;
    Ok(body?.0)
}

fn ok<T: Serialize>(v: T) -> ApiResult {
    Ok(Json(v).into_response())
}

/// The routes, relative to `/api/v1`, with their state applied.
pub fn router<S: Clone + Send + Sync + 'static>(service: VectorService) -> Router<S> {
    Router::<VectorService>::new()
        .route("/vector/stats", get(tenant_stats))
        .route("/vector/collections", get(list_collections).post(create_collection))
        .route(
            "/vector/collections/{name}",
            get(get_collection).delete(delete_collection),
        )
        .route("/vector/collections/{name}/fields", get(list_fields).post(add_field))
        .route("/vector/collections/{name}/fields/{field}", delete(delete_field))
        .route("/vector/collections/{name}/upsert", post(upsert))
        .route("/vector/collections/{name}/delete", post(delete_items))
        .route(
            "/vector/collections/{name}/items/{id}",
            get(get_item).delete(delete_item),
        )
        .route("/vector/collections/{name}/search", post(search))
        .route("/vector/collections/{name}/hybrid", post(hybrid))
        .route("/vector/collections/{name}/rebuild", post(rebuild))
        .route("/vector/collections/{name}/stats", get(stats))
        .route("/vector/collections/{name}/recall", post(recall))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .layer(RequestBodyLimitLayer::new(MAX_BODY))
        .with_state(service)
}

/// The routes and OpenAPI fragment, ready for `RouterOptions::extensions`.
pub fn extension(service: VectorService) -> ApiExtension {
    ApiExtension {
        routes: router(service),
        openapi: openapi(),
    }
}

/// OpenAPI paths and schemas of the vector API.
pub fn openapi() -> utoipa::openapi::OpenApi {
    VectorDoc::openapi()
}

#[derive(OpenApi)]
#[openapi(
    paths(
        tenant_stats, list_collections, create_collection, get_collection, delete_collection, list_fields,
        add_field, delete_field, upsert, delete_items, get_item, delete_item, search, hybrid, rebuild, stats,
        recall,
    ),
    components(schemas(
        Metric, Quantization, FieldSpec, VectorField, NewCollection, VectorCollection, VectorCollectionList, FieldList,
        UpsertItem, UpsertRequest, UpsertResult, Item, DeleteRequest, DeleteResult, FilterMode, Strategy,
        SearchRequest, SearchHit, SearchResponse, HybridWeights, TextOperator, HybridRequest, HybridHit,
        HybridResponse, RebuildRequest, RebuildResponse, IndexState, Progress, RebuildInfo, RecallReport,
        RecallRequest, IndexStatus, CollectionStats, VectorStats,
    )),
    tags((name = "vector", description = "Vector collections, similarity search (kNN with metadata filters) and hybrid keyword + vector search")),
)]
struct VectorDoc;

/// Indexes being rebuilt.
#[derive(Serialize, ToSchema)]
pub struct RebuildResponse {
    /// One entry per field.
    pub indexes: Vec<IndexStatus>,
}

/// Totals over the caller's tenant.
#[utoipa::path(get, path = "/api/v1/vector/stats", tag = "vector",
    responses((status = 200, description = "Statistics", body = VectorStats)))]
async fn tenant_stats(State(s): State<VectorService>, Access(ctx): Access) -> ApiResult {
    ok(s.tenant_stats(&ctx).await.map_err(api)?)
}

/// Collections of the caller's tenant.
#[utoipa::path(get, path = "/api/v1/vector/collections", tag = "vector",
    responses((status = 200, description = "Collections", body = VectorCollectionList)))]
async fn list_collections(State(s): State<VectorService>, Access(ctx): Access) -> ApiResult {
    ok(VectorCollectionList {
        items: s.list_collections(&ctx).await.map_err(api)?,
    })
}

/// Creates a collection with its vector fields.
#[utoipa::path(post, path = "/api/v1/vector/collections", tag = "vector", request_body = NewCollection,
    responses((status = 201, description = "Created", body = VectorCollection),
              (status = 409, description = "The name is taken")))]
async fn create_collection(State(s): State<VectorService>, Access(ctx): Access, new: Body<NewCollection>) -> ApiResult {
    // The name is in the body, so the service checks it; administration is
    // granted by permissions only, which anonymous callers never hold.
    if ctx.principal == forgebase_data::Principal::Anonymous {
        return Err(api(VectorError::Unauthenticated));
    }
    let new = new?.0;
    let c = s.create_collection(&ctx, new).await.map_err(api)?;
    Ok((StatusCode::CREATED, Json(c)).into_response())
}

/// One collection.
#[utoipa::path(get, path = "/api/v1/vector/collections/{name}", tag = "vector",
    params(("name" = String, Path, description = "Collection")),
    responses((status = 200, description = "The collection", body = VectorCollection)))]
async fn get_collection(State(s): State<VectorService>, Access(ctx): Access, Path(name): Path<String>) -> ApiResult {
    ok(s.get_collection(&ctx, &name).await.map_err(api)?)
}

/// Drops a collection with all its items, vectors and index files.
#[utoipa::path(delete, path = "/api/v1/vector/collections/{name}", tag = "vector",
    params(("name" = String, Path, description = "Collection")),
    responses((status = 204, description = "Dropped")))]
async fn delete_collection(State(s): State<VectorService>, Access(ctx): Access, Path(name): Path<String>) -> ApiResult {
    s.delete_collection(&ctx, &name).await.map_err(api)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Vector fields of a collection.
#[utoipa::path(get, path = "/api/v1/vector/collections/{name}/fields", tag = "vector",
    params(("name" = String, Path, description = "Collection")),
    responses((status = 200, description = "Fields", body = FieldList)))]
async fn list_fields(State(s): State<VectorService>, Access(ctx): Access, Path(name): Path<String>) -> ApiResult {
    ok(FieldList {
        items: s.list_fields(&ctx, &name).await.map_err(api)?,
    })
}

/// Adds a vector field.
#[utoipa::path(post, path = "/api/v1/vector/collections/{name}/fields", tag = "vector", request_body = FieldSpec,
    params(("name" = String, Path, description = "Collection")),
    responses((status = 201, description = "Added", body = VectorField)))]
async fn add_field(
    State(s): State<VectorService>,
    Access(ctx): Access,
    Path(name): Path<String>,
    spec: Body<FieldSpec>,
) -> ApiResult {
    let spec = body(&s, &ctx, Action::VectorAdmin, &name, ResourceOp::Update, spec)?;
    let f = s.add_field(&ctx, &name, spec).await.map_err(api)?;
    Ok((StatusCode::CREATED, Json(f)).into_response())
}

/// Drops a vector field, its vectors and its index.
#[utoipa::path(delete, path = "/api/v1/vector/collections/{name}/fields/{field}", tag = "vector",
    params(("name" = String, Path, description = "Collection"), ("field" = String, Path, description = "Vector field")),
    responses((status = 204, description = "Dropped")))]
async fn delete_field(
    State(s): State<VectorService>,
    Access(ctx): Access,
    Path((name, field)): Path<(String, String)>,
) -> ApiResult {
    s.delete_field(&ctx, &name, &field).await.map_err(api)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Inserts or replaces items (vectors, metadata, text) in one transaction.
#[utoipa::path(post, path = "/api/v1/vector/collections/{name}/upsert", tag = "vector", request_body = UpsertRequest,
    params(("name" = String, Path, description = "Collection")),
    responses((status = 200, description = "Stored", body = UpsertResult),
              (status = 400, description = "Wrong dimension, non-finite value, unknown field, bad id")))]
async fn upsert(
    State(s): State<VectorService>,
    Access(ctx): Access,
    Path(name): Path<String>,
    req: Body<UpsertRequest>,
) -> ApiResult {
    // New items need `create`, existing ones `update`: either lets the body in.
    if let Err(e) = s.authorize_collection(&ctx, Action::VectorWrite, &name, ResourceOp::Create) {
        s.authorize_collection(&ctx, Action::VectorWrite, &name, ResourceOp::Update)
            .map_err(|_| api(e))?;
    }
    let req = req?.0;
    ok(s.upsert(&ctx, &name, req.items).await.map_err(api)?)
}

/// Deletes items by id.
#[utoipa::path(post, path = "/api/v1/vector/collections/{name}/delete", tag = "vector", request_body = DeleteRequest,
    params(("name" = String, Path, description = "Collection")),
    responses((status = 200, description = "Deleted", body = DeleteResult)))]
async fn delete_items(
    State(s): State<VectorService>,
    Access(ctx): Access,
    Path(name): Path<String>,
    req: Body<DeleteRequest>,
) -> ApiResult {
    let req = body(&s, &ctx, Action::VectorWrite, &name, ResourceOp::Delete, req)?;
    ok(DeleteResult {
        deleted: s.delete_items(&ctx, &name, req.ids).await.map_err(api)?,
    })
}

/// One item with its canonical vectors.
#[utoipa::path(get, path = "/api/v1/vector/collections/{name}/items/{id}", tag = "vector",
    params(("name" = String, Path, description = "Collection"), ("id" = String, Path, description = "Item")),
    responses((status = 200, description = "The item", body = Item)))]
async fn get_item(
    State(s): State<VectorService>,
    Access(ctx): Access,
    Path((name, id)): Path<(String, String)>,
) -> ApiResult {
    ok(s.get_item(&ctx, &name, &id).await.map_err(api)?)
}

/// Deletes one item.
#[utoipa::path(delete, path = "/api/v1/vector/collections/{name}/items/{id}", tag = "vector",
    params(("name" = String, Path, description = "Collection"), ("id" = String, Path, description = "Item")),
    responses((status = 204, description = "Deleted")))]
async fn delete_item(
    State(s): State<VectorService>,
    Access(ctx): Access,
    Path((name, id)): Path<(String, String)>,
) -> ApiResult {
    s.delete_item(&ctx, &name, &id).await.map_err(api)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// k-nearest-neighbour search with an optional metadata filter.
#[utoipa::path(post, path = "/api/v1/vector/collections/{name}/search", tag = "vector", request_body = SearchRequest,
    params(("name" = String, Path, description = "Collection")),
    responses((status = 200, description = "Closest items first", body = SearchResponse),
              (status = 503, description = "Index rebuilding and the collection is too large to scan")))]
async fn search(
    State(s): State<VectorService>,
    Access(ctx): Access,
    Path(name): Path<String>,
    req: Body<SearchRequest>,
) -> ApiResult {
    let req = body(&s, &ctx, Action::VectorRead, &name, ResourceOp::List, req)?;
    ok(s.search(&ctx, &name, req).await.map_err(api)?)
}

/// Hybrid search: keyword (FTS5/BM25 on SQLite, full-text search on PostgreSQL) and vector rankings fused with Reciprocal Rank Fusion.
#[utoipa::path(post, path = "/api/v1/vector/collections/{name}/hybrid", tag = "vector", request_body = HybridRequest,
    params(("name" = String, Path, description = "Collection")),
    responses((status = 200, description = "Best items first", body = HybridResponse)))]
async fn hybrid(
    State(s): State<VectorService>,
    Access(ctx): Access,
    Path(name): Path<String>,
    req: Body<HybridRequest>,
) -> ApiResult {
    let req = body(&s, &ctx, Action::VectorRead, &name, ResourceOp::List, req)?;
    ok(s.hybrid(&ctx, &name, req).await.map_err(api)?)
}

/// Rebuilds ANN indexes from the canonical vectors, in the background.
#[utoipa::path(post, path = "/api/v1/vector/collections/{name}/rebuild", operation_id = "vector_rebuild", tag = "vector", request_body = RebuildRequest,
    params(("name" = String, Path, description = "Collection")),
    responses((status = 202, description = "Rebuild started; follow `progress` in the stats", body = RebuildResponse)))]
async fn rebuild(
    State(s): State<VectorService>,
    Access(ctx): Access,
    Path(name): Path<String>,
    req: Body<RebuildRequest>,
) -> ApiResult {
    let req = body(&s, &ctx, Action::VectorAdmin, &name, ResourceOp::Update, req)?;
    let indexes = s.rebuild(&ctx, &name, req.field.as_deref()).await.map_err(api)?;
    Ok((StatusCode::ACCEPTED, Json(RebuildResponse { indexes })).into_response())
}

/// Collection statistics with each index's state, progress and metrics.
#[utoipa::path(get, path = "/api/v1/vector/collections/{name}/stats", operation_id = "vector_stats", tag = "vector",
    params(("name" = String, Path, description = "Collection")),
    responses((status = 200, description = "Statistics", body = CollectionStats)))]
async fn stats(State(s): State<VectorService>, Access(ctx): Access, Path(name): Path<String>) -> ApiResult {
    ok(s.stats(&ctx, &name).await.map_err(api)?)
}

/// Measures recall@k of the ANN index against exact search.
#[utoipa::path(post, path = "/api/v1/vector/collections/{name}/recall", tag = "vector", request_body = RecallRequest,
    params(("name" = String, Path, description = "Collection")),
    responses((status = 200, description = "Recall", body = RecallReport)))]
async fn recall(
    State(s): State<VectorService>,
    Access(ctx): Access,
    Path(name): Path<String>,
    req: Body<RecallRequest>,
) -> ApiResult {
    let req = body(&s, &ctx, Action::VectorAdmin, &name, ResourceOp::View, req)?;
    ok(s.recall(&ctx, &name, req).await.map_err(api)?)
}
