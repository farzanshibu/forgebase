//! PostgreSQL extensions (FB-153, rls-coverage §3.6):
//! `/api/v1/system/extensions`. `CREATE EXTENSION` runs only here (the SQL
//! editor refuses `CREATE|ALTER|DROP EXTENSION`), for administrators, as the
//! database administrator (`forgebase_super` on the bundled engine), and
//! only for the allow list of ADR-0028 §1. Extensions that back a ForgeBase
//! feature (pgvector, Apache AGE, pgmq, pg_cron, pg_trgm: FB-161) follow
//! `[database.postgres.extensions]` and are listed, never created or dropped
//! here; `citext` and `unaccent` belong to the schema. Changes are audited
//! (`db.extension.create`, `db.extension.drop`). 400
//! `extensions_unavailable` on SQLite.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use forgebase_data::AccessContext;
use forgebase_db::{Backend, BackendKind, Feature, Statement, Value};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::ApiResult;
use crate::AppState;
use crate::error::ApiError;
use crate::http::ApiJson;
use crate::principal::Access;

/// Extensions the API may create (ADR-0028 §1): the shipped contrib list.
/// pgvector is in the engine too but belongs to the vector feature.
pub const ALLOWED: &[&str] = &[
    "btree_gin",
    "btree_gist",
    "citext",
    "fuzzystrmatch",
    "hstore",
    "intarray",
    "ltree",
    "pgcrypto",
    "pg_stat_statements",
    "tablefunc",
    "unaccent",
    "amcheck",
];

/// Extensions the schema itself creates (baseline migration).
const SCHEMA_OWNED: &[&str] = &["citext", "unaccent"];

/// Schema user extensions go to.
const USER_SCHEMA: &str = "public";

fn feature_of(name: &str) -> Option<Feature> {
    Feature::ALL.into_iter().find(|f| f.extension() == name)
}

/// One extension of the allow list or of a feature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct ExtensionInfo {
    /// `CREATE EXTENSION` name.
    pub name: String,
    /// Who manages it: `api` (this endpoint), `schema` (ForgeBase's own
    /// migrations), or `feature:<name>` (`[database.postgres.extensions]`).
    pub managed_by: String,
    /// The engine ships it (`pg_available_extensions`).
    pub available: bool,
    /// Version the engine offers.
    pub default_version: Option<String>,
    /// Installed version.
    pub installed_version: Option<String>,
    /// Schema it lives in, when installed.
    pub schema: Option<String>,
    /// The engine's description.
    pub comment: Option<String>,
}

/// `{items}` of `GET /system/extensions`.
#[derive(Serialize, utoipa::ToSchema)]
pub struct ExtensionList {
    /// The allow list and the feature extensions, by name.
    pub items: Vec<ExtensionInfo>,
}

/// Body of `POST /system/extensions`.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateExtension {
    /// An allowed extension name.
    pub name: String,
}

fn require(s: &AppState, ctx: &AccessContext) -> ApiResult<()> {
    if s.db().kind() != BackendKind::Postgres {
        return Err(ApiError::bad_request(
            "extensions_unavailable",
            "extensions need the PostgreSQL backend",
        ));
    }
    if !ctx.is_privileged() {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "extensions are managed by administrators",
        ));
    }
    Ok(())
}

async fn inventory(db: &dyn Backend) -> ApiResult<Vec<ExtensionInfo>> {
    let rows = db
        .fetch_all(&Statement::new(
            "SELECT a.name, a.default_version, e.extversion, n.nspname, a.comment \
             FROM pg_catalog.pg_available_extensions a \
             LEFT JOIN pg_catalog.pg_extension e ON e.extname = a.name \
             LEFT JOIN pg_catalog.pg_namespace n ON n.oid = e.extnamespace",
            vec![],
        ))
        .await?;
    let text = |r: &[Value], i: usize| r.get(i).and_then(Value::as_str).map(str::to_owned);
    let mut names: Vec<&str> = ALLOWED.to_vec();
    names.extend(Feature::ALL.iter().map(|f| f.extension()));
    names.sort_unstable();
    names.dedup();
    Ok(names
        .into_iter()
        .map(|name| {
            let row = rows.rows.iter().find(|r| text(r, 0).as_deref() == Some(name));
            let managed_by = match feature_of(name) {
                Some(f) => format!("feature:{}", f.name()),
                None if SCHEMA_OWNED.contains(&name) => "schema".to_owned(),
                None => "api".to_owned(),
            };
            ExtensionInfo {
                name: name.to_owned(),
                managed_by,
                available: row.is_some(),
                default_version: row.and_then(|r| text(r, 1)),
                installed_version: row.and_then(|r| text(r, 2)),
                schema: row.and_then(|r| text(r, 3)),
                comment: row.and_then(|r| text(r, 4)),
            }
        })
        .collect())
}

async fn audit(s: &AppState, ctx: &AccessContext, event: &str, name: &str) -> ApiResult<()> {
    if let Some(p) = s.policy() {
        p.audit_event(ctx, event, json!({ "extension": name }))
            .await
            .map_err(|e| ApiError::from_error(&e))?;
    }
    Ok(())
}

/// The allow list and the feature extensions, with what the engine offers
/// and what is installed.
#[utoipa::path(
    get, path = "/api/v1/system/extensions", operation_id = "extensions_list", tag = "system",
    responses((status = 200, description = "Extensions", body = ExtensionList))
)]
pub async fn list(State(s): State<AppState>, Access(ctx): Access) -> ApiResult<Response> {
    require(&s, &ctx)?;
    let items = inventory(s.db()).await?;
    Ok(Json(json!({ "items": items })).into_response())
}

fn refuse_managed(name: &str, verb: &str) -> ApiResult<()> {
    if let Some(f) = feature_of(name) {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "extension_managed",
            format!(
                "{name} backs the {} feature: set [database.postgres.extensions] {} instead of {verb}ing it here",
                f.name(),
                f.name()
            ),
        ));
    }
    if !ALLOWED.contains(&name) {
        return Err(ApiError::bad_request(
            "extension_not_allowed",
            format!("{name} is not on the allow list ({})", ALLOWED.join(", ")),
        ));
    }
    Ok(())
}

/// Creates an allowed extension in schema `public`, as the database
/// administrator. Idempotent.
#[utoipa::path(
    post, path = "/api/v1/system/extensions", operation_id = "extensions_create", tag = "system",
    request_body = CreateExtension,
    responses((status = 201, description = "Installed", body = ExtensionInfo))
)]
pub async fn create(
    State(s): State<AppState>,
    Access(ctx): Access,
    ApiJson(body): ApiJson<CreateExtension>,
) -> ApiResult<Response> {
    require(&s, &ctx)?;
    let name = body.name.trim().to_ascii_lowercase();
    refuse_managed(&name, "creat")?;
    let before = inventory(s.db()).await?;
    let info = before.iter().find(|e| e.name == name);
    if !info.is_some_and(|e| e.available) {
        return Err(ApiError::bad_request(
            "extension_unavailable",
            format!("this PostgreSQL engine does not ship {name}"),
        ));
    }
    if info.is_some_and(|e| e.installed_version.is_none()) {
        // `name` is one of ALLOWED: a plain identifier.
        s.db()
            .admin_batch(&[format!(
                "CREATE EXTENSION IF NOT EXISTS \"{name}\" WITH SCHEMA {USER_SCHEMA}"
            )])
            .await?;
        audit(&s, &ctx, "db.extension.create", &name).await?;
    }
    let after = inventory(s.db()).await?;
    let out = after
        .into_iter()
        .find(|e| e.name == name)
        .ok_or_else(ApiError::internal)?;
    Ok((StatusCode::CREATED, Json(out)).into_response())
}

/// Drops an extension this endpoint manages (`RESTRICT`: refused while
/// objects use it).
#[utoipa::path(
    delete, path = "/api/v1/system/extensions/{name}", operation_id = "extensions_drop", tag = "system",
    params(("name" = String, Path, description = "Extension name")),
    responses((status = 204, description = "Dropped"))
)]
pub async fn drop(State(s): State<AppState>, Access(ctx): Access, Path(name): Path<String>) -> ApiResult<Response> {
    require(&s, &ctx)?;
    let name = name.trim().to_ascii_lowercase();
    refuse_managed(&name, "dropp")?;
    if SCHEMA_OWNED.contains(&name.as_str()) || name == "amcheck" {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "extension_managed",
            format!("ForgeBase itself uses {name}"),
        ));
    }
    s.db()
        .admin_batch(&[format!("DROP EXTENSION IF EXISTS \"{name}\" RESTRICT")])
        .await?;
    audit(&s, &ctx, "db.extension.drop", &name).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
