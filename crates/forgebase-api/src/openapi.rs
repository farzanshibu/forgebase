//! The OpenAPI 3 document, served at `GET /api/openapi.json`.
//!
//! Every route of the server is listed. Responses that every `/api/v1`
//! operation can produce (`400`, `401`, `403`, and `404` for operations with
//! path parameters) are added to each operation by [`document`] instead of
//! being repeated in every annotation. A test walks the document and checks
//! that each operation is routed.

use axum::Json;
use axum::response::{IntoResponse, Response};
use utoipa::OpenApi;
use utoipa::openapi::path::Operation;
use utoipa::openapi::{ContentBuilder, Ref, RefOr, ResponseBuilder};

use crate::error::{ErrorDetail, ErrorEnvelope};
use crate::v1::{collections, indexes, records, schemas, sql, transfer};

/// Liveness probe.
#[utoipa::path(
    get, path = "/health", tag = "system",
    responses((status = 200, description = "`{\"status\":\"ok\"}`", body = Object))
)]
#[allow(dead_code)]
fn doc_health() {}

/// Readiness probe.
#[utoipa::path(
    get, path = "/health/ready", tag = "system",
    responses(
        (status = 200, description = "Database answers, fully migrated, storage writable", body = Object),
        (status = 503, description = "A check failed; `errors` names it", body = Object),
    )
)]
#[allow(dead_code)]
fn doc_ready() {}

/// Prometheus metrics.
#[utoipa::path(
    get, path = "/metrics", tag = "system",
    responses((status = 200, description = "Prometheus text exposition format", content_type = "text/plain", body = String))
)]
#[allow(dead_code)]
fn doc_metrics() {}

/// Instance information.
#[utoipa::path(
    get, path = "/api/system/info", tag = "system",
    responses((status = 200, description = "Name, version, data dir, uptime, database summary", body = Object))
)]
#[allow(dead_code)]
fn doc_system_info() {}

/// This document.
#[utoipa::path(
    get, path = "/api/openapi.json", tag = "system",
    responses((status = 200, description = "The OpenAPI 3 document", body = Object))
)]
#[allow(dead_code)]
fn doc_openapi() {}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "ForgeBase API",
        description = "REST API over ForgeBase's SQL, collection and document engine. \
            Errors are `{\"error\":{\"code\",\"message\",\"request_id\",\"details?\"}}`. Until authentication ships \
            (FB-20) every `/api/v1` request is anonymous and is denied with `401`; the `/api/v1` data API is versioned \
            additively (ADR-0001 §6).",
        license(name = "MIT OR Apache-2.0"),
    ),
    paths(
        doc_health, doc_ready, doc_metrics, doc_system_info, doc_openapi,
        collections::list, collections::create, collections::get, collections::update, collections::delete,
        collections::schema, collections::history,
        records::list, records::query, records::count, records::aggregate, records::explain, records::create,
        records::get, records::patch, records::put, records::delete, records::versions,
        records::doc_create, records::doc_get, records::doc_patch, records::doc_put, records::doc_delete,
        records::doc_versions,
        indexes::list, indexes::create, indexes::recommendations, indexes::analyze, indexes::get, indexes::drop,
        indexes::rebuild, indexes::stats, indexes::slow_queries, indexes::clear_slow_queries,
        indexes::set_threshold,
        sql::execute, sql::batch, sql::explain, sql::tables, sql::table,
        transfer::export, transfer::import,
    ),
    components(schemas(
        ErrorEnvelope, ErrorDetail,
        schemas::FieldSchema, schemas::CollectionSchema, schemas::CollectionList, schemas::CollectionSchemaResponse,
        schemas::SchemaChangeSchema, schemas::SchemaHistory, schemas::ListResponse, schemas::ItemsResponse,
        schemas::CountResponse, schemas::ListQueryBody, schemas::AggregateSpecBody, schemas::AggregateBody,
        schemas::RecordVersionSchema, schemas::RecordVersions, schemas::IndexKeySchema,
        schemas::IndexDefinitionSchema, schemas::IndexStatsSchema, schemas::IndexInfoSchema, schemas::IndexList,
        schemas::PlanStepSchema, schemas::QueryPlanSchema, schemas::IndexRecommendationSchema,
        schemas::IndexRecommendations, schemas::AnalyzeBody, schemas::SqlStatementBody, schemas::SqlBatchBody,
        schemas::SqlResultSchema, schemas::SqlBatchResult, schemas::ColumnInfoSchema, schemas::TableInfoSchema,
        schemas::TableList, schemas::SlowQueryLabelSchema, schemas::SlowQuerySchema, schemas::SlowQueryLog,
        schemas::ThresholdBody, schemas::ImportResult,
    )),
    tags(
        (name = "system", description = "Health, metrics and instance information"),
        (name = "collections", description = "Collection definitions and their schema"),
        (name = "records", description = "Records: CRUD, filter, sort, expand, projection, pagination, count, aggregate"),
        (name = "documents", description = "JSON documents: merge patch and versions"),
        (name = "indexes", description = "Index management, statistics, query plans, slow queries"),
        (name = "sql", description = "SQL editor"),
        (name = "import-export", description = "Streamed import and export (JSON, NDJSON, CSV)"),
    ),
)]
struct ApiDoc;

fn error_response(description: &str) -> RefOr<utoipa::openapi::Response> {
    RefOr::T(
        ResponseBuilder::new()
            .description(description)
            .content(
                "application/json",
                ContentBuilder::new()
                    .schema(Some(Ref::from_schema_name("ErrorEnvelope")))
                    .build(),
            )
            .build(),
    )
}

fn add_errors(op: &mut Operation, with_path_params: bool) {
    let mut codes = vec![
        (
            "400",
            "Invalid request (error.code says why; validation errors list `details.fields`)",
        ),
        ("401", "Authentication required (anonymous callers are denied)"),
        ("403", "Not allowed"),
    ];
    if with_path_params {
        codes.push((
            "404",
            "No such collection, record or index (or not visible to the caller)",
        ));
    }
    for (code, description) in codes {
        op.responses
            .responses
            .entry(code.to_owned())
            .or_insert_with(|| error_response(description));
    }
}

/// The complete OpenAPI document of the core API.
pub fn document() -> utoipa::openapi::OpenApi {
    document_with(std::iter::empty())
}

/// The document with the fragments of feature crates merged in
/// ([`crate::ApiExtension`]).
pub fn document_with<'a>(
    extensions: impl IntoIterator<Item = &'a utoipa::openapi::OpenApi>,
) -> utoipa::openapi::OpenApi {
    let mut doc = ApiDoc::openapi();
    for ext in extensions {
        doc.merge(ext.clone());
    }
    for (path, item) in &mut doc.paths.paths {
        if !path.starts_with("/api/v1/") {
            continue;
        }
        let params = path.contains('{');
        for op in [
            &mut item.get,
            &mut item.post,
            &mut item.put,
            &mut item.patch,
            &mut item.delete,
        ]
        .into_iter()
        .flatten()
        {
            add_errors(op, params);
        }
    }
    doc
}

/// `GET /api/openapi.json`.
pub async fn serve(doc: std::sync::Arc<utoipa::openapi::OpenApi>) -> Response {
    Json(&*doc).into_response()
}
