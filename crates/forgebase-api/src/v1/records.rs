//! Records and documents: CRUD, queries, counts, aggregation, versions.
//!
//! The `/documents/{collection}/...` routes are the same handlers as
//! `/collections/{collection}/records/...`: a document is a record of a
//! collection with `schema_mode: flexible`. `PATCH` is an RFC 7386 JSON merge
//! patch (`application/json` or `application/merge-patch+json`), `PUT`
//! replaces the document.

use axum::Json as AxumJson;
use axum::extract::{Path, State};
use axum::http::header::{ETAG, HeaderValue, LOCATION};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use forgebase_data::{AggregateQuery, ListQuery, Record, UpdateOptions};
use serde_json::{Value as Json, json};

use super::ApiResult;
use super::schemas::{
    AggregateBody, CountResponse, ItemsResponse, ListQueryBody, ListResponse, QueryPlanSchema, RecordVersions,
};
use crate::AppState;
use crate::error::ApiError;
use crate::http::{ApiJson, Query, if_match_version};
use crate::principal::Access;

const QUERY_PARAMS_DOC: &str = "Query string: `filter`, `p.<name>` (filter parameters), `sort` (`-created,name`), \
    `fields`, `expand`, `search`, `page`, `per_page` (max 500), `cursor`, `skip_total`.";

/// A record response with its version as `ETag`.
pub(crate) fn record_response(status: StatusCode, rec: Record, location: Option<String>) -> Response {
    let version = rec.get("_version").and_then(Json::as_i64);
    let mut res = (status, AxumJson(rec)).into_response();
    if let Some(v) = version
        && let Ok(h) = HeaderValue::from_str(&format!("\"{v}\""))
    {
        res.headers_mut().insert(ETAG, h);
    }
    if let Some(l) = location
        && let Ok(h) = HeaderValue::from_str(&l)
    {
        res.headers_mut().insert(LOCATION, h);
    }
    res
}

/// Lists records.
#[utoipa::path(
    get, path = "/api/v1/collections/{collection}/records", operation_id = "records_list", tag = "records",
    params(("collection" = String, Path, description = "Collection name or id")),
    description = QUERY_PARAMS_DOC,
    responses((status = 200, description = "A page of records", body = ListResponse))
)]
pub async fn list(
    State(s): State<AppState>,
    Access(ctx): Access,
    Path(c): Path<String>,
    q: Query,
) -> ApiResult<Response> {
    let result = s.data().list_records(&ctx, &c, &q.list_query()?).await?;
    Ok(AxumJson(result).into_response())
}

/// Lists records with a typed JSON query (typed `params`, long filters).
#[utoipa::path(
    post, path = "/api/v1/collections/{collection}/records/query", tag = "records",
    params(("collection" = String, Path, description = "Collection name or id")),
    request_body = ListQueryBody,
    responses((status = 200, description = "A page of records", body = ListResponse))
)]
pub async fn query(
    State(s): State<AppState>,
    Access(ctx): Access,
    Path(c): Path<String>,
    ApiJson(q): ApiJson<ListQuery>,
) -> ApiResult<Response> {
    Ok(AxumJson(s.data().list_records(&ctx, &c, &q).await?).into_response())
}

/// Counts records matching a filter.
#[utoipa::path(
    get, path = "/api/v1/collections/{collection}/records/count", tag = "records",
    params(("collection" = String, Path, description = "Collection name or id")),
    description = "Query string: `filter` and `p.<name>` parameters.",
    responses((status = 200, description = "Number of matching records", body = CountResponse))
)]
pub async fn count(
    State(s): State<AppState>,
    Access(ctx): Access,
    Path(c): Path<String>,
    q: Query,
) -> ApiResult<Response> {
    let n = s
        .data()
        .count_records(&ctx, &c, q.get("filter"), q.filter_params())
        .await?;
    Ok(AxumJson(json!({ "count": n })).into_response())
}

/// Groups and aggregates records (`count`, `sum`, `avg`, `min`, `max`).
#[utoipa::path(
    post, path = "/api/v1/collections/{collection}/aggregate", tag = "records",
    params(("collection" = String, Path, description = "Collection name or id")),
    request_body = AggregateBody,
    responses((status = 200, description = "One row per group", body = ItemsResponse))
)]
pub async fn aggregate(
    State(s): State<AppState>,
    Access(ctx): Access,
    Path(c): Path<String>,
    ApiJson(q): ApiJson<AggregateQuery>,
) -> ApiResult<Response> {
    let items = s.data().aggregate(&ctx, &c, &q).await?;
    Ok(AxumJson(json!({ "items": items })).into_response())
}

/// The query plan of a record listing: which indexes it uses, which tables it scans.
#[utoipa::path(
    post, path = "/api/v1/collections/{collection}/records/explain", operation_id = "records_explain", tag = "indexes",
    params(("collection" = String, Path, description = "Collection name or id")),
    request_body = ListQueryBody,
    responses((status = 200, description = "Query plan", body = QueryPlanSchema))
)]
pub async fn explain(
    State(s): State<AppState>,
    Access(ctx): Access,
    Path(c): Path<String>,
    ApiJson(q): ApiJson<ListQuery>,
) -> ApiResult<Response> {
    Ok(AxumJson(s.data().explain_list(&ctx, &c, &q).await?).into_response())
}

/// Creates a record, or (with a JSON array, at most 1000) several records in
/// one all-or-nothing transaction.
#[utoipa::path(
    post, path = "/api/v1/collections/{collection}/records", operation_id = "records_create", tag = "records",
    params(("collection" = String, Path, description = "Collection name or id")),
    request_body(content = Object, description = "One record object, or an array of record objects"),
    responses(
        (status = 201, description = "The record (ETag = version), or `{items}` for an array", body = Object),
        (status = 400, description = "Validation failed (`error.details.fields`)"),
    )
)]
pub async fn create(
    State(s): State<AppState>,
    Access(ctx): Access,
    Path(c): Path<String>,
    ApiJson(body): ApiJson<Json>,
) -> ApiResult<Response> {
    match body {
        Json::Array(items) => {
            let recs = s.data().create_records(&ctx, &c, items).await?;
            Ok((StatusCode::CREATED, AxumJson(json!({ "items": recs }))).into_response())
        }
        obj @ Json::Object(_) => {
            let rec = s.data().create_record(&ctx, &c, obj).await?;
            let id = rec.get("id").and_then(Json::as_str).unwrap_or_default();
            let location = format!("/api/v1/collections/{c}/records/{id}");
            Ok(record_response(StatusCode::CREATED, rec, Some(location)))
        }
        _ => Err(ApiError::bad_request(
            "invalid_body",
            "send a JSON object, or an array of objects",
        )),
    }
}

/// Reads one record.
#[utoipa::path(
    get, path = "/api/v1/collections/{collection}/records/{id}", operation_id = "records_get", tag = "records",
    params(
        ("collection" = String, Path, description = "Collection name or id"),
        ("id" = String, Path, description = "Record id"),
    ),
    description = "Query string: `fields`, `expand`.",
    responses((status = 200, description = "The record (ETag = version)", body = Object))
)]
pub async fn get(
    State(s): State<AppState>,
    Access(ctx): Access,
    Path((c, id)): Path<(String, String)>,
    q: Query,
) -> ApiResult<Response> {
    let fields = q.list("fields");
    let opts = forgebase_data::GetOptions {
        fields: (!fields.is_empty()).then_some(fields),
        expand: q.list("expand"),
    };
    let rec = s.data().get_record(&ctx, &c, &id, &opts).await?;
    Ok(record_response(StatusCode::OK, rec, None))
}

async fn write(
    s: AppState,
    ctx: forgebase_data::AccessContext,
    (c, id): (String, String),
    headers: HeaderMap,
    body: Json,
    replace: bool,
) -> ApiResult<Response> {
    if !body.is_object() {
        return Err(ApiError::bad_request("invalid_body", "send a JSON object"));
    }
    let opts = UpdateOptions {
        expected_version: if_match_version(&headers)?,
        replace,
    };
    let rec = s.data().update_record(&ctx, &c, &id, body, opts).await?;
    Ok(record_response(StatusCode::OK, rec, None))
}

/// Updates a record with a JSON merge patch (RFC 7386): objects merge, `null`
/// removes a key, arrays and scalars replace. The merged document is validated.
/// `If-Match: "<version>"` makes it conditional (409 `version_conflict`).
#[utoipa::path(
    patch, path = "/api/v1/collections/{collection}/records/{id}", tag = "records",
    params(
        ("collection" = String, Path, description = "Collection name or id"),
        ("id" = String, Path, description = "Record id"),
        ("If-Match" = Option<String>, Header, description = "Expected record version, e.g. \"3\""),
    ),
    request_body(content = Object, description = "Merge patch"),
    responses((status = 200, description = "The updated record", body = Object), (status = 409, description = "Version conflict"))
)]
pub async fn patch(
    State(s): State<AppState>,
    Access(ctx): Access,
    Path(p): Path<(String, String)>,
    headers: HeaderMap,
    ApiJson(body): ApiJson<Json>,
) -> ApiResult<Response> {
    write(s, ctx, p, headers, body, false).await
}

/// Replaces a record's whole document (missing fields get their defaults).
#[utoipa::path(
    put, path = "/api/v1/collections/{collection}/records/{id}", tag = "records",
    params(
        ("collection" = String, Path, description = "Collection name or id"),
        ("id" = String, Path, description = "Record id"),
        ("If-Match" = Option<String>, Header, description = "Expected record version, e.g. \"3\""),
    ),
    request_body(content = Object, description = "The new document"),
    responses((status = 200, description = "The updated record", body = Object), (status = 409, description = "Version conflict"))
)]
pub async fn put(
    State(s): State<AppState>,
    Access(ctx): Access,
    Path(p): Path<(String, String)>,
    headers: HeaderMap,
    ApiJson(body): ApiJson<Json>,
) -> ApiResult<Response> {
    write(s, ctx, p, headers, body, true).await
}

/// Deletes a record. `If-Match: "<version>"` makes it conditional.
#[utoipa::path(
    delete, path = "/api/v1/collections/{collection}/records/{id}", operation_id = "records_delete", tag = "records",
    params(
        ("collection" = String, Path, description = "Collection name or id"),
        ("id" = String, Path, description = "Record id"),
        ("If-Match" = Option<String>, Header, description = "Expected record version, e.g. \"3\""),
    ),
    responses((status = 204, description = "Deleted"), (status = 409, description = "Version conflict or still referenced"))
)]
pub async fn delete(
    State(s): State<AppState>,
    Access(ctx): Access,
    Path((c, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    s.data()
        .delete_record(&ctx, &c, &id, if_match_version(&headers)?)
        .await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Stored versions of a record (collections with `versioned: true`), oldest first.
#[utoipa::path(
    get, path = "/api/v1/collections/{collection}/records/{id}/versions", operation_id = "records_versions", tag = "records",
    params(
        ("collection" = String, Path, description = "Collection name or id"),
        ("id" = String, Path, description = "Record id"),
    ),
    responses((status = 200, description = "Versions, including a delete tombstone", body = RecordVersions))
)]
pub async fn versions(
    State(s): State<AppState>,
    Access(ctx): Access,
    Path((c, id)): Path<(String, String)>,
) -> ApiResult<Response> {
    let items = s.data().record_versions(&ctx, &c, &id).await?;
    Ok(AxumJson(json!({ "items": items })).into_response())
}

// ---- documents: the same operations under /documents ----

/// Creates a document (see `POST /collections/{collection}/records`).
#[utoipa::path(
    post, path = "/api/v1/documents/{collection}", tag = "documents",
    params(("collection" = String, Path, description = "Document collection name or id")),
    request_body(content = Object, description = "One document, or an array of documents"),
    responses((status = 201, description = "The document (ETag = version)", body = Object))
)]
pub async fn doc_create(
    state: State<AppState>,
    access: Access,
    path: Path<String>,
    body: ApiJson<Json>,
) -> ApiResult<Response> {
    create(state, access, path, body).await
}

/// Reads a document.
#[utoipa::path(
    get, path = "/api/v1/documents/{collection}/{id}", tag = "documents",
    params(
        ("collection" = String, Path, description = "Document collection name or id"),
        ("id" = String, Path, description = "Document id"),
    ),
    responses((status = 200, description = "The document", body = Object))
)]
pub async fn doc_get(
    state: State<AppState>,
    access: Access,
    path: Path<(String, String)>,
    q: Query,
) -> ApiResult<Response> {
    get(state, access, path, q).await
}

/// Merge-patches a document (RFC 7386; accepts `application/merge-patch+json`).
#[utoipa::path(
    patch, path = "/api/v1/documents/{collection}/{id}", tag = "documents",
    params(
        ("collection" = String, Path, description = "Document collection name or id"),
        ("id" = String, Path, description = "Document id"),
        ("If-Match" = Option<String>, Header, description = "Expected document version, e.g. \"3\""),
    ),
    request_body(content = Object, description = "Merge patch"),
    responses((status = 200, description = "The merged document", body = Object))
)]
pub async fn doc_patch(
    state: State<AppState>,
    access: Access,
    path: Path<(String, String)>,
    headers: HeaderMap,
    body: ApiJson<Json>,
) -> ApiResult<Response> {
    patch(state, access, path, headers, body).await
}

/// Replaces a document.
#[utoipa::path(
    put, path = "/api/v1/documents/{collection}/{id}", tag = "documents",
    params(
        ("collection" = String, Path, description = "Document collection name or id"),
        ("id" = String, Path, description = "Document id"),
        ("If-Match" = Option<String>, Header, description = "Expected document version, e.g. \"3\""),
    ),
    request_body(content = Object, description = "The new document"),
    responses((status = 200, description = "The stored document", body = Object))
)]
pub async fn doc_put(
    state: State<AppState>,
    access: Access,
    path: Path<(String, String)>,
    headers: HeaderMap,
    body: ApiJson<Json>,
) -> ApiResult<Response> {
    put(state, access, path, headers, body).await
}

/// Deletes a document.
#[utoipa::path(
    delete, path = "/api/v1/documents/{collection}/{id}", tag = "documents",
    params(
        ("collection" = String, Path, description = "Document collection name or id"),
        ("id" = String, Path, description = "Document id"),
        ("If-Match" = Option<String>, Header, description = "Expected document version, e.g. \"3\""),
    ),
    responses((status = 204, description = "Deleted"))
)]
pub async fn doc_delete(
    state: State<AppState>,
    access: Access,
    path: Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    delete(state, access, path, headers).await
}

/// Stored versions of a document (collections with `versioned: true`).
#[utoipa::path(
    get, path = "/api/v1/documents/{collection}/{id}/versions", tag = "documents",
    params(
        ("collection" = String, Path, description = "Document collection name or id"),
        ("id" = String, Path, description = "Document id"),
    ),
    responses((status = 200, description = "Versions, oldest first", body = RecordVersions))
)]
pub async fn doc_versions(state: State<AppState>, access: Access, path: Path<(String, String)>) -> ApiResult<Response> {
    versions(state, access, path).await
}
