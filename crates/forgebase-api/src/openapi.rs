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
use utoipa::openapi::security::{
    ApiKey, ApiKeyValue, HttpAuthScheme, HttpBuilder, SecurityRequirement, SecurityScheme,
};
use utoipa::openapi::{ContentBuilder, Ref, RefOr, ResponseBuilder};

use crate::error::{ErrorDetail, ErrorEnvelope};
use crate::v1::{collections, db_logins, extensions, indexes, records, schemas, sql, transfer};
use forgebase_policy::http::docs as pd;

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

/// The capability matrix (ADR-0034): per feature, the engine it runs on
/// (a PostgreSQL extension or the portable engine), its state (`active`,
/// `portable`, `off`, `native`, `unavailable`), mode and limits.
#[utoipa::path(
    get, path = "/api/system/capabilities", tag = "system",
    responses((status = 200, description = "Every feature of this server", body = crate::capabilities::Capabilities))
)]
#[allow(dead_code)]
fn doc_system_capabilities() {}

/// The feature registry: every module, whether it is on, and its typed
/// settings with value, default and source (administrators only; FB-91).
#[utoipa::path(
    get, path = "/api/system/features", tag = "system",
    responses(
        (status = 200, description = "Modules and settings; secrets have `value: null`", body = crate::features::SystemFeatures),
        (status = 401, description = "No credentials", body = ErrorEnvelope),
        (status = 403, description = "Not an administrator", body = ErrorEnvelope),
    )
)]
#[allow(dead_code)]
fn doc_system_features() {}

/// Write settings to the config file: every key is validated first, then all
/// are written in one atomic, comment-preserving replace (administrators
/// only; FB-91). Changes apply after a restart.
#[utoipa::path(
    patch, path = "/api/system/config", tag = "system",
    request_body = crate::features::SystemConfigPatch,
    responses(
        (status = 200, description = "Written; `restart_required` says whether a restart is needed", body = crate::features::SystemConfigPatched),
        (status = 400, description = "`validation_failed`, `details: {\"<key>\": \"reason\"}`", body = ErrorEnvelope),
        (status = 401, description = "No credentials", body = ErrorEnvelope),
        (status = 403, description = "Not an administrator", body = ErrorEnvelope),
        (status = 409, description = "`setting_overridden`: set by an environment variable or flag (`details` per key)", body = ErrorEnvelope),
    )
)]
#[allow(dead_code)]
fn doc_system_config() {}

/// MCP server (ADR-0030): one JSON-RPC 2.0 message of the Model Context
/// Protocol (Streamable HTTP transport, JSON responses). Off unless
/// `[mcp] enabled = true`. Authenticate like any API call; a scoped
/// credential needs `mcp:write` (or `mcp:*`), and each tool call is then
/// checked against its own scopes and rules.
#[utoipa::path(
    post, path = "/api/mcp", tag = "mcp",
    request_body(content = Object, description = "A JSON-RPC 2.0 request or notification: `initialize`, `ping`, `tools/list`, `tools/call`"),
    responses(
        (status = 200, description = "The JSON-RPC response", body = Object),
        (status = 202, description = "A notification or response was accepted"),
        (status = 400, description = "Not JSON, a batch, or an unsupported `MCP-Protocol-Version`", body = Object),
        (status = 401, description = "No credentials (`WWW-Authenticate: Bearer`)", body = ErrorEnvelope),
        (status = 403, description = "`origin_not_allowed`, or a scoped credential without `mcp`", body = ErrorEnvelope),
        (status = 404, description = "`feature_disabled`: `[mcp] enabled = false`", body = ErrorEnvelope),
    )
)]
#[allow(dead_code)]
fn doc_mcp() {}

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
            Errors are `{\"error\":{\"code\",\"message\",\"request_id\",\"details?\"}}`. Authenticate with \
            `Authorization: Bearer <access token | API key | personal access token>`, `X-API-Key`, or the \
            `fb_session` cookie (+ `X-CSRF-Token` on writes); see `/api/auth/*` and ADR-0004. Requests without \
            credentials are anonymous and the authorizer decides; the `/api/v1` data API is versioned additively \
            (ADR-0001 §6).",
        license(name = "MIT OR Apache-2.0"),
    ),
    paths(
        doc_health, doc_ready, doc_metrics, doc_system_info, doc_system_capabilities, doc_system_features, doc_system_config, doc_openapi, doc_mcp,
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
        db_logins::list, db_logins::create, db_logins::get, db_logins::rotate, db_logins::delete,
        extensions::list, extensions::create, extensions::drop,
        pd::list_realms, pd::create_realm, pd::list_orgs, pd::create_org, pd::get_org, pd::update_org,
        pd::delete_org, pd::list_org_projects, pd::create_org_project, pd::list_personal_projects,
        pd::create_personal_project, pd::get_project, pd::delete_project, pd::create_environment,
        pd::list_members, pd::put_member, pd::remove_member, pd::member_status, pd::list_teams,
        pd::create_team, pd::delete_team, pd::add_team_member, pd::remove_team_member, pd::list_org_roles,
        pd::put_org_role, pd::delete_org_role, pd::put_org_role_set, pd::list_realm_roles, pd::put_realm_role,
        pd::delete_realm_role, pd::put_realm_role_set, pd::put_realm_binding, pd::add_override,
        pd::delete_override, pd::list_acl, pd::put_acl, pd::delete_acl, pd::suspend, pd::unsuspend,
        pd::get_policy, pd::put_policy, pd::delete_policy, pd::invite, pd::accept, pd::me, pd::check,
        pd::audit, pd::list_resource_rules, pd::get_resource_rules, pd::put_resource_rules,
        pd::delete_resource_rules, pd::check_resource,
    ),
    components(schemas(
        ErrorEnvelope, ErrorDetail,
        crate::features::SystemFeatures, crate::features::SystemFeature, crate::features::SystemSetting,
        crate::features::SystemConfigPatch, crate::features::SystemConfigPatched,
        crate::capabilities::Capabilities, crate::capabilities::Capability,
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
        sql::RunAsBody, db_logins::DbLoginList, db_logins::RotateBody,
        forgebase_policy::logins::LoginSpec, forgebase_policy::logins::DbLogin,
        forgebase_policy::logins::LoginSecret,
        extensions::ExtensionInfo, extensions::ExtensionList, extensions::CreateExtension,
    )),
    tags(
        (name = "system", description = "Health, metrics and instance information"),
        (name = "mcp", description = "Model Context Protocol server for AI agents (ADR-0030)"),
        (name = "collections", description = "Collection definitions and their schema"),
        (name = "records", description = "Records: CRUD, filter, sort, expand, projection, pagination, count, aggregate"),
        (name = "documents", description = "JSON documents: merge patch and versions"),
        (name = "indexes", description = "Index management, statistics, query plans, slow queries"),
        (name = "sql", description = "SQL editor"),
        (name = "import-export", description = "Streamed import and export (JSON, NDJSON, CSV)"),
        (name = "tenancy", description = "Realms, organizations, projects, environments (tenants), members, teams, invitations (ADR-0008)"),
        (name = "authorization", description = "Roles, role sets, overrides, resource ACL, suspensions, collection rules, checks and audit (ADR-0008)"),
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

/// Security schemes of the auth layer (FB-20, ADR-0004).
fn add_security(doc: &mut utoipa::openapi::OpenApi) {
    let components = doc.components.get_or_insert_with(Default::default);
    components.add_security_scheme(
        "bearer",
        SecurityScheme::Http(
            HttpBuilder::new()
                .scheme(HttpAuthScheme::Bearer)
                .bearer_format("access token (HS256 JWT), API key (fbk_…) or personal access token (fbp_…)")
                .description(Some(
                    "`Authorization: Bearer <token>`. Access tokens come from `/api/auth/login` and \
                     `/api/auth/refresh`; API keys and PATs carry scopes (`<area>:read|write[:<name>]`).",
                ))
                .build(),
        ),
    );
    components.add_security_scheme(
        "api_key",
        SecurityScheme::ApiKey(ApiKey::Header(ApiKeyValue::with_description(
            "X-API-Key",
            "Service-account API key or personal access token (alternative to the bearer header).",
        ))),
    );
    components.add_security_scheme(
        "session_cookie",
        SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::with_description(
            "fb_session",
            "Browser session cookie from `/api/auth/login` with `cookie: true`; unsafe methods also need `X-CSRF-Token`.",
        ))),
    );
}

/// Any one credential, or none (anonymous; the authorizer decides).
fn api_security() -> Vec<SecurityRequirement> {
    vec![
        SecurityRequirement::new("bearer", Vec::<String>::new()),
        SecurityRequirement::new("api_key", Vec::<String>::new()),
        SecurityRequirement::new("session_cookie", Vec::<String>::new()),
        SecurityRequirement::default(),
    ]
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
    doc.merge(forgebase_graph::openapi());
    // FB-175: `/api/system/database/*`.
    doc.merge(crate::system_database::openapi());
    for ext in extensions {
        doc.merge(ext.clone());
    }
    doc.merge(forgebase_realtime::openapi());
    add_security(&mut doc);
    for (path, item) in &mut doc.paths.paths {
        if !(path.starts_with("/api/v1/") || path.starts_with("/api/auth/")) {
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
            if path.starts_with("/api/v1/") {
                op.security = Some(api_security());
            }
        }
    }
    doc
}

/// `GET /api/openapi.json`.
pub async fn serve(doc: std::sync::Arc<utoipa::openapi::OpenApi>) -> Response {
    Json(&*doc).into_response()
}
