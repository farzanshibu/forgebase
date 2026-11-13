//! OpenAPI schemas of the request and response bodies.
//!
//! The data-core types (`Collection`, `IndexInfo`, ...) are plain serde types
//! without OpenAPI derives, so the document describes them with these mirror
//! types. They are documentation only: nothing is (de)serialised through them,
//! and a test checks that real responses carry the documented keys.

#![allow(missing_docs, dead_code)]

use serde_json::Value as Json;
use utoipa::ToSchema;

/// Any JSON object (a record or document).
pub type Object = serde_json::Map<String, Json>;

#[derive(ToSchema)]
pub struct FieldSchema {
    /// Stable id; renames keep it.
    pub id: String,
    pub name: String,
    /// `text`, `number`, `bool`, `date`, `email`, `url`, `select`, `json`,
    /// `array`, `relation`, `file`, `password`, `geo`, `vector`.
    #[schema(rename = "type")]
    pub kind: String,
    pub required: bool,
    pub unique: bool,
    pub hidden: bool,
    pub system: bool,
    #[schema(value_type = Option<Object>)]
    pub default: Option<Json>,
    // Type options sit beside `type` (there is no `options` object; other
    // keys are refused). Each applies to the types named.
    /// `text`: minimum length in characters; `password`: minimum length (default 8).
    pub min_length: Option<u32>,
    /// `text`: maximum length in characters (default 1 000 000).
    pub max_length: Option<u32>,
    /// `number`: minimum value.
    pub min: Option<f64>,
    /// `number`: maximum value.
    pub max: Option<f64>,
    /// `number`: integers only.
    pub integer: Option<bool>,
    /// `select`: the allowed values.
    pub values: Option<Vec<String>>,
    /// `select`, `relation`, `file`: values that may be chosen at once (default 1; above 1 the field holds an array).
    pub max_select: Option<u32>,
    /// `relation`: the target collection's id.
    pub collection_id: Option<String>,
    /// `relation`: delete this record when the referenced record is deleted.
    pub cascade_delete: Option<bool>,
    /// `array`: item type, `text`, `number`, `bool` or `json` (default `json`).
    pub items: Option<String>,
    /// `array`: minimum number of items.
    pub min_items: Option<u32>,
    /// `array`: maximum number of items.
    pub max_items: Option<u32>,
    /// `vector`: number of dimensions (1 to 16 000).
    pub dimensions: Option<u32>,
    /// `json`: maximum serialised size in bytes (default 1 MiB).
    pub max_size: Option<u32>,
}

#[derive(ToSchema)]
pub struct CollectionSchema {
    pub id: String,
    pub name: String,
    /// `base`, `auth` or `view`.
    #[schema(rename = "type")]
    pub kind: String,
    pub system: bool,
    /// `strict` or `flexible` (JSON documents with undeclared keys).
    pub schema_mode: String,
    /// Keep every version of every record.
    pub versioned: bool,
    pub fields: Vec<FieldSchema>,
    pub view_query: Option<String>,
    #[schema(value_type = Object)]
    pub options: Object,
    pub version: i64,
    pub created: String,
    pub updated: String,
}

#[derive(ToSchema)]
pub struct CollectionList {
    pub items: Vec<CollectionSchema>,
}

#[derive(ToSchema)]
pub struct CollectionSchemaResponse {
    pub collection: CollectionSchema,
    /// JSON Schema (draft 2020-12) of a record of this collection.
    #[schema(value_type = Object)]
    pub json_schema: Json,
}

#[derive(ToSchema)]
pub struct SchemaChangeSchema {
    pub id: i64,
    pub collection_id: Option<String>,
    pub collection: Option<String>,
    pub version: Option<i64>,
    pub action: String,
    pub statements: Vec<String>,
    #[schema(value_type = Option<Object>)]
    pub before: Option<Json>,
    #[schema(value_type = Option<Object>)]
    pub after: Option<Json>,
    pub actor: String,
    pub applied_at: String,
}

#[derive(ToSchema)]
pub struct SchemaHistory {
    pub items: Vec<SchemaChangeSchema>,
}

#[derive(ToSchema)]
pub struct ListResponse {
    #[schema(value_type = Vec<Object>)]
    pub items: Vec<Object>,
    pub page: Option<u32>,
    pub per_page: u32,
    pub total_items: Option<u64>,
    pub total_pages: Option<u64>,
    /// Pass as `cursor` to get the next page; absent on the last page.
    pub next_cursor: Option<String>,
}

#[derive(ToSchema)]
pub struct ItemsResponse {
    #[schema(value_type = Vec<Object>)]
    pub items: Vec<Object>,
}

#[derive(ToSchema)]
pub struct CountResponse {
    pub count: u64,
}

#[derive(ToSchema)]
pub struct ListQueryBody {
    pub filter: Option<String>,
    /// Values of `{:name}` parameters, typed.
    #[schema(value_type = Object)]
    pub params: Option<Object>,
    /// e.g. `-created,name`
    pub sort: Option<String>,
    pub fields: Option<Vec<String>>,
    pub expand: Option<Vec<String>>,
    /// Full-text query (needs a full-text index).
    pub search: Option<String>,
    /// Fuzzy text: trigram similarity, best match first (PostgreSQL with
    /// pg_trgm; `400 fuzzy_unsupported` otherwise).
    pub fuzzy: Option<String>,
    /// Fields `fuzzy` compares; each needs a trigram index (default: all
    /// fields of the collection's trigram indexes).
    pub fuzzy_fields: Option<Vec<String>>,
    /// Minimum similarity from 0 to 1 (default 0.3).
    pub fuzzy_threshold: Option<f64>,
    pub page: Option<u32>,
    pub per_page: Option<u32>,
    pub cursor: Option<String>,
    pub skip_total: Option<bool>,
}

#[derive(ToSchema)]
pub struct AggregateSpecBody {
    /// `count`, `sum`, `avg`, `min`, `max`.
    pub function: String,
    pub field: Option<String>,
    pub alias: Option<String>,
}

#[derive(ToSchema)]
pub struct AggregateBody {
    pub filter: Option<String>,
    #[schema(value_type = Object)]
    pub params: Option<Object>,
    pub group_by: Option<Vec<String>>,
    pub aggregates: Option<Vec<AggregateSpecBody>>,
    pub limit: Option<u32>,
}

#[derive(ToSchema)]
pub struct RecordVersionSchema {
    pub version: i64,
    /// `create`, `update` or `delete`.
    pub op: String,
    #[schema(value_type = Option<Object>)]
    pub data: Option<Json>,
    pub changed_at: String,
    pub actor: String,
}

#[derive(ToSchema)]
pub struct RecordVersions {
    pub items: Vec<RecordVersionSchema>,
}

#[derive(ToSchema)]
pub struct IndexKeySchema {
    /// Field path in filter syntax: `price`, `metadata.brand`, `title:lower`.
    pub path: String,
    pub desc: Option<bool>,
}

#[derive(ToSchema)]
pub struct IndexDefinitionSchema {
    pub name: Option<String>,
    pub collection: String,
    pub keys: Vec<IndexKeySchema>,
    pub unique: Option<bool>,
    pub partial: Option<String>,
    /// `btree` (default), `fulltext` or `trigram` (PostgreSQL with pg_trgm;
    /// `400 index_kind_unsupported` elsewhere).
    pub kind: Option<String>,
}

#[derive(ToSchema)]
pub struct IndexStatsSchema {
    pub stat: Option<String>,
    pub rows_estimate: Option<i64>,
    pub size_bytes: Option<i64>,
}

#[derive(ToSchema)]
pub struct IndexInfoSchema {
    pub name: String,
    pub table: String,
    pub collection: Option<String>,
    /// `auto`, `manual`, `sql` or `engine`.
    pub origin: String,
    pub kind: String,
    pub unique: bool,
    pub partial: bool,
    pub columns: Vec<String>,
    pub definition: Option<IndexDefinitionSchema>,
    pub sql: Option<String>,
    pub stats: IndexStatsSchema,
}

#[derive(ToSchema)]
pub struct IndexList {
    pub items: Vec<IndexInfoSchema>,
}

#[derive(ToSchema)]
pub struct PlanStepSchema {
    pub id: i64,
    pub parent: i64,
    pub detail: String,
}

#[derive(ToSchema)]
pub struct QueryPlanSchema {
    pub sql: String,
    pub steps: Vec<PlanStepSchema>,
    pub indexes_used: Vec<String>,
    pub full_scans: Vec<String>,
}

#[derive(ToSchema)]
pub struct IndexRecommendationSchema {
    pub definition: IndexDefinitionSchema,
    pub reason: String,
    pub occurrences: usize,
    pub max_duration_ms: f64,
}

#[derive(ToSchema)]
pub struct IndexRecommendations {
    pub items: Vec<IndexRecommendationSchema>,
}

#[derive(ToSchema)]
pub struct AnalyzeBody {
    /// Collection to analyze; every collection when absent.
    pub collection: Option<String>,
}

#[derive(ToSchema)]
pub struct SqlStatementBody {
    /// One SQL statement; positional parameters `?` / `?1`.
    pub sql: String,
    #[schema(value_type = Option<Vec<Object>>)]
    pub params: Option<Vec<Json>>,
    /// `POST /sql` only (a batch sets it on the batch body): return secret
    /// columns (`password`, `token_key`, password fields) unmasked and allow
    /// statements that name them. Each use is audited (`sql.show_secrets`
    /// in `GET /api/auth/audit`). Default `false`.
    pub show_secrets: Option<bool>,
    /// `POST /sql` only (FB-153, PostgreSQL): run as a direct login bound to
    /// this user would, `{"principal": "user:users/u1", "tenant": "env1"}`.
    /// Audited (`sql.run_as`).
    #[schema(value_type = Option<crate::v1::sql::RunAsBody>)]
    pub run_as: Option<Json>,
}

#[derive(ToSchema)]
pub struct SqlBatchBody {
    /// 1 to 1000 statements, run in one all-or-nothing transaction.
    pub statements: Vec<SqlStatementBody>,
    /// Return secret columns unmasked for the whole batch and allow
    /// statements that name them; audited once per batch. Default `false`.
    pub show_secrets: Option<bool>,
    /// Run the batch as a direct login bound to this user would (FB-153).
    #[schema(value_type = Option<crate::v1::sql::RunAsBody>)]
    pub run_as: Option<Json>,
}

#[derive(ToSchema)]
pub struct SqlResultSchema {
    /// `read`, `write`, `schema` or `maintenance`.
    pub kind: String,
    pub columns: Vec<String>,
    #[schema(value_type = Vec<Vec<Object>>)]
    pub rows: Vec<Vec<Json>>,
    pub rows_affected: u64,
    /// Rowid of the last insert (SQLite; `null` on PostgreSQL: use `RETURNING`).
    pub last_insert_id: Option<i64>,
    pub truncated: bool,
    pub duration_ms: f64,
    /// `true` when secret values in `rows` read `"********"` (send
    /// `show_secrets` to see them); absent otherwise.
    pub secrets_masked: Option<bool>,
}

#[derive(ToSchema)]
pub struct SqlBatchResult {
    pub items: Vec<SqlResultSchema>,
}

#[derive(ToSchema)]
pub struct ColumnInfoSchema {
    pub name: String,
    pub declared_type: String,
    pub not_null: bool,
    pub default: Option<String>,
    pub primary_key: bool,
}

#[derive(ToSchema)]
pub struct TableInfoSchema {
    pub name: String,
    /// `table` or `view`.
    pub kind: String,
    pub collection: Option<String>,
    pub columns: Vec<ColumnInfoSchema>,
    pub indexes: Vec<String>,
    pub sql: Option<String>,
}

#[derive(ToSchema)]
pub struct TableList {
    pub items: Vec<TableInfoSchema>,
}

#[derive(ToSchema)]
pub struct SlowQueryLabelSchema {
    pub collection: String,
    pub equality_keys: Vec<String>,
    pub range_keys: Vec<String>,
    pub sort_keys: Vec<String>,
}

#[derive(ToSchema)]
pub struct SlowQuerySchema {
    /// SQL text; parameter values are never recorded.
    pub sql: String,
    pub param_count: usize,
    pub duration_ms: f64,
    pub at: String,
    pub rows: u64,
    pub label: Option<SlowQueryLabelSchema>,
}

#[derive(ToSchema)]
pub struct SlowQueryLog {
    pub threshold_ms: u64,
    pub items: Vec<SlowQuerySchema>,
}

#[derive(ToSchema)]
pub struct ThresholdBody {
    /// Statements slower than this are logged; `0` logs every statement.
    pub threshold_ms: u64,
}

#[derive(ToSchema)]
pub struct ImportResult {
    /// Records stored.
    pub imported: u64,
    /// `json`, `ndjson` or `csv`.
    pub format: String,
}
