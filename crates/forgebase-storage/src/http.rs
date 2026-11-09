//! REST under `/api/v1/storage` (mounted as an `ApiExtension`).
//!
//! Uploads and downloads stream: the request body goes to the driver chunk
//! by chunk and a download is a stream of the blob (or of one range).
//! Quota refusals are `429 quota_exceeded`, a checksum that does not match
//! `422 checksum_mismatch`, a too-large object `413`, an unsatisfiable range
//! `416`, a resumable chunk at the wrong offset `409 offset_mismatch` with the
//! server's offset in `details` and in the `Upload-Offset` header.
//! Downloads carry `Content-Security-Policy: sandbox` and `nosniff`, and
//! active content types (HTML, SVG, XML) are served as attachments, so a
//! stored file cannot run script in the instance's origin.

use std::collections::BTreeMap;
use std::io;

use axum::Json;
use axum::Router;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use forgebase_api::http::{ApiJson, Query};
use forgebase_api::principal::Access;
use forgebase_api::{ApiError, ApiExtension};
use forgebase_data::Action;
use futures_util::StreamExt;
use serde::Serialize;
use serde_json::json;
use utoipa::{OpenApi, ToSchema};

use crate::driver::ByteStream;
use crate::error::StorageError;
use crate::model::{
    Acl, AclEntry, Bucket, BucketInput, BucketPatch, CompleteInput, CompletePart, DeleteResult, ObjectInfo, ObjectPage,
    PartInfo, Permission, ReconcileReport, SignRequest, SignedMethod, SignedUrl, StorageStats, Upload, UploadInput,
    Visibility,
};
use crate::objects::{PutRequest, RangeSpec};
use crate::service::StorageService;
use crate::signed::SignedParams;
use crate::util::parse_sha256;

type ApiResult = Result<Response, ApiError>;

const META_PREFIX: &str = "x-forgebase-meta-";
const H_CHECKSUM: &str = "x-forgebase-checksum-sha256";
const H_VERSION: &str = "x-forgebase-version-id";
const H_OFFSET: &str = "upload-offset";

/// The HTTP form of a storage error.
pub fn api(e: StorageError) -> ApiError {
    if let StorageError::Entitlement(inner) = e {
        // The quota engine's mapping: 429 with the decision, 403 for a feature outside the plan.
        return forgebase_entitlements::http::api(inner);
    }
    match &e {
        StorageError::TooLarge { limit } => {
            ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, "object_too_large", e.to_string())
                .with_details(json!({"limit": limit}))
        }
        StorageError::RangeNotSatisfiable { size } => ApiError::new(
            StatusCode::RANGE_NOT_SATISFIABLE,
            "range_not_satisfiable",
            e.to_string(),
        )
        .with_details(json!({"size": size})),
        StorageError::ChecksumMismatch { expected, actual } => {
            ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "checksum_mismatch", e.to_string())
                .with_details(json!({"expected": expected, "actual": actual}))
        }
        StorageError::OffsetMismatch { server_offset } => {
            ApiError::new(StatusCode::CONFLICT, "offset_mismatch", e.to_string())
                .with_details(json!({"upload_offset": server_offset}))
        }
        _ => ApiError::from_error(&e),
    }
}

fn ok<T: Serialize>(v: T) -> ApiResult {
    Ok(Json(v).into_response())
}

fn created<T: Serialize>(v: T) -> ApiResult {
    Ok((StatusCode::CREATED, Json(v)).into_response())
}

fn body_stream(body: Body) -> ByteStream {
    Box::pin(body.into_data_stream().map(|c| c.map_err(io::Error::other)))
}

fn header_str<'a>(h: &'a HeaderMap, name: &str) -> Option<&'a str> {
    h.get(name).and_then(|v| v.to_str().ok())
}

fn content_length(h: &HeaderMap) -> Option<u64> {
    header_str(h, "content-length").and_then(|v| v.parse().ok())
}

fn checksum(h: &HeaderMap) -> Result<Option<String>, ApiError> {
    header_str(h, H_CHECKSUM)
        .map(|v| parse_sha256(v).map_err(api))
        .transpose()
}

fn metadata(h: &HeaderMap) -> BTreeMap<String, String> {
    h.iter()
        .filter_map(|(k, v)| {
            let name = k.as_str().strip_prefix(META_PREFIX)?;
            Some((name.to_owned(), v.to_str().ok()?.to_owned()))
        })
        .collect()
}

/// Parses a single `Range: bytes=...`; `None` for no header, an error for
/// anything that is not one satisfiable-looking range.
fn parse_range(h: &HeaderMap) -> Result<Option<RangeSpec>, ApiError> {
    let Some(v) = header_str(h, "range") else {
        return Ok(None);
    };
    let bad = || {
        ApiError::new(
            StatusCode::RANGE_NOT_SATISFIABLE,
            "range_not_satisfiable",
            "unsupported Range header",
        )
    };
    let spec = v.trim().strip_prefix("bytes=").ok_or_else(bad)?;
    if spec.contains(',') {
        return Err(bad());
    }
    let (a, b) = spec.split_once('-').ok_or_else(bad)?;
    let num = |s: &str| s.trim().parse::<u64>().map_err(|_| bad());
    Ok(Some(match (a.trim().is_empty(), b.trim().is_empty()) {
        (false, true) => RangeSpec::From(num(a)?),
        (false, false) => RangeSpec::Bounded(num(a)?, num(b)?),
        (true, false) => RangeSpec::Suffix(num(b)?),
        (true, true) => return Err(bad()),
    }))
}

fn disposition(name: &str) -> String {
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() && !matches!(c, '"' | '\\' | ';') {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("attachment; filename=\"{safe}\"")
}

fn is_active(content_type: &str) -> bool {
    let ct = content_type.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    matches!(
        ct.as_str(),
        "text/html" | "application/xhtml+xml" | "image/svg+xml" | "text/xml" | "application/xml" | "text/javascript"
    )
}

fn object_response(
    method: &Method,
    read: crate::objects::ObjectRead,
    headers: &HeaderMap,
    download: Option<&str>,
) -> ApiResult {
    let info = &read.info;
    let etag = format!("\"{}\"", info.checksum_sha256.as_deref().unwrap_or_default());
    if header_str(headers, "if-none-match").is_some_and(|v| v == etag) && read.range.is_none() {
        return Ok((StatusCode::NOT_MODIFIED, [(header::ETAG, etag)]).into_response());
    }
    let (status, len) = match read.range {
        Some((a, b)) => (StatusCode::PARTIAL_CONTENT, b - a + 1),
        None => (StatusCode::OK, read.size),
    };
    let mut res = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, info.content_type.as_str())
        .header(header::CONTENT_LENGTH, len)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::ETAG, etag)
        .header(header::CONTENT_SECURITY_POLICY, "sandbox")
        .header(HeaderName::from_static("x-content-type-options"), "nosniff")
        .header(HeaderName::from_static(H_VERSION), info.version_id.as_str());
    if let Some(sum) = &info.checksum_sha256 {
        res = res.header(HeaderName::from_static(H_CHECKSUM), sum.as_str());
    }
    if let Some((a, b)) = read.range {
        res = res.header(header::CONTENT_RANGE, format!("bytes {a}-{b}/{}", read.size));
    }
    if let Some(name) = download {
        res = res.header(header::CONTENT_DISPOSITION, disposition(name));
    } else if is_active(&info.content_type) {
        res = res.header(
            header::CONTENT_DISPOSITION,
            disposition(info.key.rsplit('/').next().unwrap_or("download")),
        );
    }
    for (k, v) in &info.metadata {
        if let (Ok(n), Ok(v)) = (
            HeaderName::from_bytes(format!("{META_PREFIX}{k}").as_bytes()),
            HeaderValue::from_str(v),
        ) {
            res = res.header(n, v);
        }
    }
    let body = if method == Method::HEAD {
        Body::empty()
    } else {
        Body::from_stream(read.stream)
    };
    res.body(body).map_err(|_| ApiError::internal())
}

fn upload_headers(res: &mut Response, offset: u64) {
    if let Ok(v) = HeaderValue::from_str(&offset.to_string()) {
        res.headers_mut().insert(HeaderName::from_static(H_OFFSET), v);
    }
}

/// A list.
#[derive(Serialize, ToSchema)]
pub struct Items<T> {
    /// Entries.
    pub items: Vec<T>,
}

/// The routes, relative to `/api/v1`, with their state applied.
pub fn router<S: Clone + Send + Sync + 'static>(svc: StorageService) -> Router<S> {
    Router::<StorageService>::new()
        .route("/storage/stats", get(stats))
        .route("/storage/maintenance/reconcile", post(reconcile))
        .route("/storage/buckets", get(list_buckets).post(create_bucket))
        .route(
            "/storage/buckets/{bucket}",
            get(get_bucket).patch(patch_bucket).delete(delete_bucket),
        )
        .route("/storage/buckets/{bucket}/acl", get(get_acl).put(put_acl))
        .route("/storage/buckets/{bucket}/objects", get(list_objects))
        .route(
            "/storage/buckets/{bucket}/objects/{*key}",
            put(put_object).get(get_object).head(get_object).delete(delete_object),
        )
        .route("/storage/buckets/{bucket}/copy", post(copy_object))
        .route("/storage/buckets/{bucket}/move", post(move_object))
        .route("/storage/buckets/{bucket}/versions/{*key}", get(list_versions))
        .route("/storage/buckets/{bucket}/restore", post(restore_version))
        .route("/storage/buckets/{bucket}/sign", post(sign))
        .route("/storage/buckets/{bucket}/uploads", post(create_upload))
        .route(
            "/storage/uploads/{id}",
            get(get_upload).patch(append_chunk).delete(abort_upload),
        )
        .route("/storage/uploads/{id}/parts/{n}", put(upload_part))
        .route("/storage/uploads/{id}/complete", post(complete_upload))
        .route("/storage/signed/{bucket}/{*key}", get(signed_get).put(signed_put))
        .with_state(svc)
}

/// The routes and OpenAPI fragment, ready for `RouterOptions::extensions`.
pub fn extension(svc: StorageService) -> ApiExtension {
    ApiExtension {
        routes: router(svc),
        openapi: openapi(),
    }
}

/// OpenAPI paths and schemas.
pub fn openapi() -> utoipa::openapi::OpenApi {
    StorageDoc::openapi()
}

#[derive(OpenApi)]
#[openapi(
    paths(
        stats, reconcile, list_buckets, create_bucket, get_bucket, patch_bucket, delete_bucket, get_acl, put_acl,
        list_objects, put_object, get_object, delete_object, copy_object, move_object, list_versions,
        restore_version, sign, create_upload, get_upload, append_chunk, abort_upload, upload_part, complete_upload,
        signed_get, signed_put,
    ),
    components(schemas(
        Bucket, BucketInput, BucketPatch, Visibility, Permission, Acl, AclEntry, ObjectInfo, ObjectPage, DeleteResult,
        Upload, UploadInput, PartInfo, CompleteInput, CompletePart, SignRequest, SignedUrl, SignedMethod,
        ReconcileReport, StorageStats, CopyBody, RestoreBody, ItemsBuckets,
    )),
    tags((name = "storage", description = "Buckets, objects, uploads, signed URLs, quotas and maintenance")),
)]
struct StorageDoc;

/// Buckets.
#[derive(Serialize, ToSchema)]
pub struct ItemsBuckets {
    /// Entries.
    pub items: Vec<Bucket>,
}

/// Copy or move target.
#[derive(serde::Deserialize, ToSchema)]
pub struct CopyBody {
    /// Source key.
    pub key: String,
    /// Source version (copy only).
    #[serde(default)]
    pub version_id: Option<String>,
    /// Destination bucket (default: the same).
    #[serde(default)]
    pub dest_bucket: Option<String>,
    /// Destination key.
    pub dest_key: String,
}

/// Version to restore.
#[derive(serde::Deserialize, ToSchema)]
pub struct RestoreBody {
    /// Key.
    pub key: String,
    /// Version id.
    pub version_id: String,
}

/// Instance totals (storage admins).
#[utoipa::path(get, path = "/api/v1/storage/stats", tag = "storage",
    responses((status = 200, body = StorageStats)))]
async fn stats(State(s): State<StorageService>, Access(ctx): Access) -> ApiResult {
    ok(s.stats(&ctx).await.map_err(api)?)
}

/// Runs the reconciler now (storage admins).
#[utoipa::path(post, path = "/api/v1/storage/maintenance/reconcile", tag = "storage",
    responses((status = 200, body = ReconcileReport)))]
async fn reconcile(State(s): State<StorageService>, Access(ctx): Access) -> ApiResult {
    ok(s.reconcile(&ctx).await.map_err(api)?)
}

/// Buckets the caller may read.
#[utoipa::path(get, path = "/api/v1/storage/buckets", tag = "storage",
    responses((status = 200, body = ItemsBuckets)))]
async fn list_buckets(State(s): State<StorageService>, Access(ctx): Access) -> ApiResult {
    ok(Items {
        items: s.list_buckets(&ctx).await.map_err(api)?,
    })
}

/// Creates a bucket.
#[utoipa::path(post, path = "/api/v1/storage/buckets", tag = "storage", request_body = BucketInput,
    responses((status = 201, body = Bucket)))]
async fn create_bucket(
    State(s): State<StorageService>,
    Access(ctx): Access,
    b: Result<ApiJson<BucketInput>, ApiError>,
) -> ApiResult {
    created(s.create_bucket(&ctx, b?.0).await.map_err(api)?)
}

/// One bucket.
#[utoipa::path(get, path = "/api/v1/storage/buckets/{bucket}", tag = "storage",
    responses((status = 200, body = Bucket)))]
async fn get_bucket(State(s): State<StorageService>, Access(ctx): Access, Path(b): Path<String>) -> ApiResult {
    ok(s.get_bucket(&ctx, &b).await.map_err(api)?)
}

/// Changes a bucket's policy.
#[utoipa::path(patch, path = "/api/v1/storage/buckets/{bucket}", tag = "storage", request_body = BucketPatch,
    responses((status = 200, body = Bucket)))]
async fn patch_bucket(
    State(s): State<StorageService>,
    Access(ctx): Access,
    Path(b): Path<String>,
    body: Result<ApiJson<BucketPatch>, ApiError>,
) -> ApiResult {
    ok(s.update_bucket(&ctx, &b, body?.0).await.map_err(api)?)
}

/// Deletes a bucket (`?force=true` removes its objects too).
#[utoipa::path(delete, path = "/api/v1/storage/buckets/{bucket}", tag = "storage",
    responses((status = 204, description = "Deleted")))]
async fn delete_bucket(
    State(s): State<StorageService>,
    Access(ctx): Access,
    Path(b): Path<String>,
    q: Query,
) -> ApiResult {
    s.delete_bucket(&ctx, &b, q.bool("force")?).await.map_err(api)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// The ACL of a bucket.
#[utoipa::path(get, path = "/api/v1/storage/buckets/{bucket}/acl", tag = "storage",
    responses((status = 200, body = Acl)))]
async fn get_acl(State(s): State<StorageService>, Access(ctx): Access, Path(b): Path<String>) -> ApiResult {
    ok(s.get_acl(&ctx, &b).await.map_err(api)?)
}

/// Replaces the ACL of a bucket.
#[utoipa::path(put, path = "/api/v1/storage/buckets/{bucket}/acl", tag = "storage", request_body = Acl,
    responses((status = 200, body = Acl)))]
async fn put_acl(
    State(s): State<StorageService>,
    Access(ctx): Access,
    Path(b): Path<String>,
    body: Result<ApiJson<Acl>, ApiError>,
) -> ApiResult {
    ok(s.set_acl(&ctx, &b, body?.0).await.map_err(api)?)
}

/// Lists objects (`prefix`, `delimiter`, `limit`, `cursor`).
#[utoipa::path(get, path = "/api/v1/storage/buckets/{bucket}/objects", tag = "storage",
    responses((status = 200, body = ObjectPage)))]
async fn list_objects(
    State(s): State<StorageService>,
    Access(ctx): Access,
    Path(b): Path<String>,
    q: Query,
) -> ApiResult {
    let page = s
        .list_objects(
            &ctx,
            &b,
            q.get("prefix").unwrap_or(""),
            q.get("delimiter"),
            q.u32("limit")?.unwrap_or(100),
            q.get("cursor"),
        )
        .await
        .map_err(api)?;
    ok(page)
}

/// Uploads an object: the request body is the content, streamed. Headers:
/// `Content-Type`, `X-ForgeBase-Checksum-SHA256` (rejects a mismatch),
/// `X-ForgeBase-Meta-<name>` (user metadata).
#[utoipa::path(put, path = "/api/v1/storage/buckets/{bucket}/objects/{key}", tag = "storage",
    responses((status = 201, body = ObjectInfo), (status = 422, description = "Checksum mismatch"),
              (status = 429, description = "Storage quota exceeded")))]
async fn put_object(
    State(s): State<StorageService>,
    Access(ctx): Access,
    Path((bucket, key)): Path<(String, String)>,
    headers: HeaderMap,
    body: Body,
) -> ApiResult {
    let info = s
        .put_object(
            &ctx,
            PutRequest {
                bucket,
                key,
                content_type: header_str(&headers, "content-type").map(str::to_owned),
                metadata: metadata(&headers),
                expected_sha256: checksum(&headers)?,
                content_length: content_length(&headers),
                body: body_stream(body),
            },
        )
        .await
        .map_err(api)?;
    created(info)
}

/// Downloads (GET) or inspects (HEAD) an object; supports `Range`, `ETag`,
/// `?version=` and `?download=<file name>`.
#[utoipa::path(get, path = "/api/v1/storage/buckets/{bucket}/objects/{key}", tag = "storage",
    responses((status = 200, description = "The object"), (status = 206, description = "A range"),
              (status = 416, description = "Range not satisfiable")))]
async fn get_object(
    State(s): State<StorageService>,
    Access(ctx): Access,
    method: Method,
    Path((bucket, key)): Path<(String, String)>,
    headers: HeaderMap,
    q: Query,
) -> ApiResult {
    let range = parse_range(&headers)?;
    let read = s
        .get_object(&ctx, &bucket, &key, q.get("version"), range)
        .await
        .map_err(api)?;
    object_response(&method, read, &headers, q.get("download"))
}

/// Deletes an object, or one version with `?version=`.
#[utoipa::path(delete, path = "/api/v1/storage/buckets/{bucket}/objects/{key}", tag = "storage",
    responses((status = 200, body = DeleteResult)))]
async fn delete_object(
    State(s): State<StorageService>,
    Access(ctx): Access,
    Path((bucket, key)): Path<(String, String)>,
    q: Query,
) -> ApiResult {
    ok(s.delete_object(&ctx, &bucket, &key, q.get("version"))
        .await
        .map_err(api)?)
}

/// Copies an object (no bytes move).
#[utoipa::path(post, path = "/api/v1/storage/buckets/{bucket}/copy", tag = "storage", request_body = CopyBody,
    responses((status = 201, body = ObjectInfo)))]
async fn copy_object(
    State(s): State<StorageService>,
    Access(ctx): Access,
    Path(bucket): Path<String>,
    body: Result<ApiJson<CopyBody>, ApiError>,
) -> ApiResult {
    let b = body?.0;
    let dest = b.dest_bucket.as_deref().unwrap_or(&bucket);
    created(
        s.copy_object(&ctx, &bucket, &b.key, b.version_id.as_deref(), dest, &b.dest_key)
            .await
            .map_err(api)?,
    )
}

/// Moves (renames) an object.
#[utoipa::path(post, path = "/api/v1/storage/buckets/{bucket}/move", tag = "storage", request_body = CopyBody,
    responses((status = 200, body = ObjectInfo)))]
async fn move_object(
    State(s): State<StorageService>,
    Access(ctx): Access,
    Path(bucket): Path<String>,
    body: Result<ApiJson<CopyBody>, ApiError>,
) -> ApiResult {
    let b = body?.0;
    let dest = b.dest_bucket.as_deref().unwrap_or(&bucket);
    ok(s.move_object(&ctx, &bucket, &b.key, dest, &b.dest_key)
        .await
        .map_err(api)?)
}

/// Every version of a key, newest first.
#[utoipa::path(get, path = "/api/v1/storage/buckets/{bucket}/versions/{key}", tag = "storage",
    responses((status = 200, body = ObjectInfo)))]
async fn list_versions(
    State(s): State<StorageService>,
    Access(ctx): Access,
    Path((bucket, key)): Path<(String, String)>,
) -> ApiResult {
    ok(Items {
        items: s.list_versions(&ctx, &bucket, &key).await.map_err(api)?,
    })
}

/// Restores an earlier version as a new current version.
#[utoipa::path(post, path = "/api/v1/storage/buckets/{bucket}/restore", tag = "storage", request_body = RestoreBody,
    responses((status = 201, body = ObjectInfo)))]
async fn restore_version(
    State(s): State<StorageService>,
    Access(ctx): Access,
    Path(bucket): Path<String>,
    body: Result<ApiJson<RestoreBody>, ApiError>,
) -> ApiResult {
    let b = body?.0;
    created(
        s.restore_version(&ctx, &bucket, &b.key, &b.version_id)
            .await
            .map_err(api)?,
    )
}

/// Makes a signed URL.
#[utoipa::path(post, path = "/api/v1/storage/buckets/{bucket}/sign", tag = "storage", request_body = SignRequest,
    responses((status = 200, body = SignedUrl)))]
async fn sign(
    State(s): State<StorageService>,
    Access(ctx): Access,
    Path(bucket): Path<String>,
    body: Result<ApiJson<SignRequest>, ApiError>,
) -> ApiResult {
    ok(s.sign_url(&ctx, &bucket, body?.0).await.map_err(api)?)
}

/// Opens a multipart or resumable upload session.
#[utoipa::path(post, path = "/api/v1/storage/buckets/{bucket}/uploads", tag = "storage", request_body = UploadInput,
    responses((status = 201, body = Upload)))]
async fn create_upload(
    State(s): State<StorageService>,
    Access(ctx): Access,
    Path(bucket): Path<String>,
    body: Result<ApiJson<UploadInput>, ApiError>,
) -> ApiResult {
    let up = s.create_upload(&ctx, &bucket, body?.0).await.map_err(api)?;
    let mut res = (StatusCode::CREATED, Json(&up)).into_response();
    upload_headers(&mut res, up.received_bytes);
    Ok(res)
}

/// The state of a session; `Upload-Offset` is where a resumable client continues.
#[utoipa::path(get, path = "/api/v1/storage/uploads/{id}", tag = "storage",
    responses((status = 200, body = Upload)))]
async fn get_upload(State(s): State<StorageService>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult {
    let up = s.get_upload(&ctx, &id).await.map_err(api)?;
    let mut res = Json(&up).into_response();
    upload_headers(&mut res, up.received_bytes);
    Ok(res)
}

/// Appends a chunk to a resumable upload at `Upload-Offset`.
#[utoipa::path(patch, path = "/api/v1/storage/uploads/{id}", tag = "storage",
    responses((status = 200, body = Upload), (status = 201, body = ObjectInfo, description = "Completed"),
              (status = 409, description = "Wrong offset")))]
async fn append_chunk(
    State(s): State<StorageService>,
    Access(ctx): Access,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> ApiResult {
    let offset = header_str(&headers, H_OFFSET)
        .and_then(|v| v.parse::<u64>().ok())
        .ok_or_else(|| ApiError::bad_request("missing_offset", "send the Upload-Offset header"))?;
    let out = s
        .append_chunk(&ctx, &id, offset, checksum(&headers)?.as_deref(), body_stream(body))
        .await
        .map_err(api)?;
    let mut res = match out.object {
        Some(o) => (StatusCode::CREATED, Json(o)).into_response(),
        None => Json(&out.upload).into_response(),
    };
    upload_headers(&mut res, out.upload.received_bytes);
    Ok(res)
}

/// Cancels a session.
#[utoipa::path(delete, path = "/api/v1/storage/uploads/{id}", tag = "storage",
    responses((status = 204, description = "Aborted")))]
async fn abort_upload(State(s): State<StorageService>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult {
    s.abort_upload(&ctx, &id).await.map_err(api)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Uploads one numbered part of a multipart session.
#[utoipa::path(put, path = "/api/v1/storage/uploads/{id}/parts/{n}", tag = "storage",
    responses((status = 200, body = PartInfo)))]
async fn upload_part(
    State(s): State<StorageService>,
    Access(ctx): Access,
    Path((id, n)): Path<(String, u32)>,
    headers: HeaderMap,
    body: Body,
) -> ApiResult {
    ok(
        s.upload_part(&ctx, &id, n, checksum(&headers)?.as_deref(), body_stream(body))
            .await
            .map_err(api)?,
    )
}

/// Completes a multipart session.
#[utoipa::path(post, path = "/api/v1/storage/uploads/{id}/complete", tag = "storage", request_body = CompleteInput,
    responses((status = 201, body = ObjectInfo)))]
async fn complete_upload(
    State(s): State<StorageService>,
    Access(ctx): Access,
    Path(id): Path<String>,
    body: Option<Json<CompleteInput>>,
) -> ApiResult {
    created(
        s.complete_upload(&ctx, &id, body.map(|b| b.0).unwrap_or_default())
            .await
            .map_err(api)?,
    )
}

fn signed_params(method: SignedMethod, bucket: &str, key: &str, q: &Query) -> Result<SignedParams, ApiError> {
    let mut map = BTreeMap::new();
    for name in ["expires", "version", "download", "ct", "sub", "sig"] {
        if let Some(v) = q.get(name) {
            map.insert(name.to_owned(), v.to_owned());
        }
    }
    SignedParams::from_request(method, bucket, key, &map).map_err(api)
}

/// Downloads through a signed URL (no credentials needed).
#[utoipa::path(get, path = "/api/v1/storage/signed/{bucket}/{key}", tag = "storage",
    responses((status = 200, description = "The object"), (status = 403, description = "Forged or expired")))]
async fn signed_get(
    State(s): State<StorageService>,
    method: Method,
    Path((bucket, key)): Path<(String, String)>,
    headers: HeaderMap,
    q: Query,
) -> ApiResult {
    let p = signed_params(SignedMethod::Get, &bucket, &key, &q)?;
    let range = parse_range(&headers)?;
    let read = s.get_signed(&p, range).await.map_err(api)?;
    object_response(&method, read, &headers, p.download.as_deref())
}

/// Uploads through a signed URL (no credentials needed).
#[utoipa::path(put, path = "/api/v1/storage/signed/{bucket}/{key}", tag = "storage",
    responses((status = 201, body = ObjectInfo)))]
async fn signed_put(
    State(s): State<StorageService>,
    Path((bucket, key)): Path<(String, String)>,
    headers: HeaderMap,
    q: Query,
    body: Body,
) -> ApiResult {
    let p = signed_params(SignedMethod::Put, &bucket, &key, &q)?;
    let info = s
        .put_signed(
            &p,
            header_str(&headers, "content-type").map(str::to_owned),
            metadata(&headers),
            checksum(&headers)?,
            content_length(&headers),
            body_stream(body),
        )
        .await
        .map_err(api)?;
    created(info)
}

impl StorageService {
    /// Totals over the instance (needs the `storage_admin` permission).
    pub async fn stats(&self, ctx: &forgebase_data::AccessContext) -> crate::Result<StorageStats> {
        self.authorize_platform(ctx, Action::StorageAdmin)?;
        let rows = self
            .read(
                "SELECT (SELECT COUNT(*) FROM _forgebase_storage_buckets), \
                        (SELECT COUNT(*) FROM _forgebase_storage_objects WHERE is_latest = 1 AND delete_marker = 0), \
                        (SELECT COUNT(*) FROM _forgebase_storage_objects), \
                        (SELECT COALESCE(SUM(size), 0) FROM _forgebase_storage_objects WHERE is_latest = 1 AND delete_marker = 0), \
                        (SELECT COALESCE(SUM(size), 0) FROM _forgebase_storage_blobs WHERE refcount > 0), \
                        (SELECT COUNT(*) FROM _forgebase_storage_uploads)",
                vec![],
            )
            .await?;
        let r = rows
            .rows
            .first()
            .ok_or_else(|| StorageError::Internal("no statistics".into()))?;
        let n = |i: usize| crate::service::uint(r, i);
        Ok(StorageStats {
            buckets: n(0),
            objects: n(1),
            versions: n(2),
            logical_bytes: n(3),
            stored_bytes: n(4),
            open_uploads: n(5),
        })
    }
}
