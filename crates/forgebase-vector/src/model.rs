//! Request and response types of the vector API.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json};
use utoipa::ToSchema;

use crate::error::VectorError;

/// Most dimensions of a vector field.
pub const MAX_DIMENSION: u32 = 4096;
/// Most vector fields per collection.
pub const MAX_FIELDS: usize = 16;
/// Most items in one upsert or delete request.
pub const MAX_BATCH: usize = 1000;
/// Largest `k` of a search.
pub const MAX_K: u32 = 1000;
/// Largest metadata document of an item, serialised.
pub const MAX_METADATA_BYTES: usize = 64 * 1024;
/// Longest item text (keyword search), in bytes.
pub const MAX_TEXT_BYTES: usize = 1024 * 1024;
/// Most queries of one recall check.
pub const MAX_RECALL_SAMPLES: u32 = 1000;

/// Distance function of a vector field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Metric {
    /// Cosine distance, `1 - cos(a, b)`; `score` is the cosine similarity.
    Cosine,
    /// Euclidean distance; `score` is `1 / (1 + distance)`.
    L2,
    /// Inner product, distance `1 - a·b`; `score` is the dot product.
    Dot,
}

impl Metric {
    /// Stored name.
    pub fn as_str(self) -> &'static str {
        match self {
            Metric::Cosine => "cosine",
            Metric::L2 => "l2",
            Metric::Dot => "dot",
        }
    }

    /// Parses a stored name.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "cosine" => Metric::Cosine,
            "l2" => Metric::L2,
            "dot" => Metric::Dot,
            _ => return None,
        })
    }
}

/// Element type of the ANN index. Canonical vectors are always stored as
/// float32; a smaller type makes the derived index smaller and faster at some
/// cost in precision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Quantization {
    /// 32-bit floats (exact copy of the canonical vectors).
    #[default]
    F32,
    /// 16-bit floats.
    F16,
    /// 8-bit integers.
    I8,
}

impl Quantization {
    /// Stored name.
    pub fn as_str(self) -> &'static str {
        match self {
            Quantization::F32 => "f32",
            Quantization::F16 => "f16",
            Quantization::I8 => "i8",
        }
    }

    /// Parses a stored name.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "f32" => Quantization::F32,
            "f16" => Quantization::F16,
            "i8" => Quantization::I8,
            _ => return None,
        })
    }
}

/// Definition of a vector field, as sent when creating it.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct FieldSpec {
    /// `[A-Za-z][A-Za-z0-9_]{0,62}`.
    pub name: String,
    /// Number of components, 1 to 4096.
    pub dimension: u32,
    /// Distance function.
    pub metric: Metric,
    /// Element type of the ANN index (default `f32`).
    #[serde(default)]
    pub quantization: Option<Quantization>,
    /// HNSW links per node (default 16, 2 to 128).
    #[serde(default)]
    pub connectivity: Option<u32>,
    /// HNSW candidate list size while inserting (default 128).
    #[serde(default)]
    pub expansion_add: Option<u32>,
    /// HNSW candidate list size while searching (default 128). Higher means
    /// better recall and slower searches.
    #[serde(default)]
    pub expansion_search: Option<u32>,
}

/// A vector field of a collection.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct VectorField {
    /// Name.
    pub name: String,
    /// Number of components.
    pub dimension: u32,
    /// Distance function.
    pub metric: Metric,
    /// Element type of the ANN index.
    pub quantization: Quantization,
    /// HNSW links per node.
    pub connectivity: u32,
    /// HNSW candidate list size while inserting.
    pub expansion_add: u32,
    /// HNSW candidate list size while searching.
    pub expansion_search: u32,
    /// Creation time.
    pub created: String,
}

/// A new collection.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewCollection {
    /// `[A-Za-z][A-Za-z0-9_]{0,62}`, unique in the tenant.
    pub name: String,
    /// Free text.
    #[serde(default)]
    pub description: String,
    /// Vector fields (at most 16; more can be added later).
    #[serde(default)]
    pub fields: Vec<FieldSpec>,
    /// Searches over at most this many candidate vectors are exact (brute
    /// force) instead of approximate. Default from the server configuration.
    #[serde(default)]
    pub exact_threshold: Option<u32>,
}

/// A vector collection.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct VectorCollection {
    /// Name.
    pub name: String,
    /// Free text.
    pub description: String,
    /// Vector fields.
    pub fields: Vec<VectorField>,
    /// See [`NewCollection::exact_threshold`].
    pub exact_threshold: u32,
    /// Number of items.
    pub items: u64,
    /// Creation time.
    pub created: String,
    /// Last change of the definition or the data.
    pub updated: String,
}

/// Collections of the caller's tenant.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CollectionList {
    /// By name.
    pub items: Vec<VectorCollection>,
}

/// Vector fields of a collection.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FieldList {
    /// By name.
    pub items: Vec<VectorField>,
}

/// One item to insert or replace.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpsertItem {
    /// 1 to 64 of `[A-Za-z0-9_-]`; generated when absent. An existing item
    /// with this id is replaced completely.
    #[serde(default)]
    pub id: Option<String>,
    /// Vector per field name. Fields left out have no vector for this item.
    #[serde(default)]
    pub vectors: BTreeMap<String, Vec<f64>>,
    /// JSON object, filterable with the filter language (`category = 'news'`).
    #[serde(default)]
    #[schema(value_type = Option<Object>)]
    pub metadata: Option<Map<String, Json>>,
    /// Text for keyword search (the full-text side of hybrid search).
    #[serde(default)]
    pub text: Option<String>,
}

/// Body of an upsert.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpsertRequest {
    /// 1 to 1000 items, written in one transaction.
    pub items: Vec<UpsertItem>,
}

/// Outcome of an upsert.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct UpsertResult {
    /// Ids in request order.
    pub ids: Vec<String>,
    /// Items that did not exist before.
    pub created: u64,
    /// Items replaced.
    pub updated: u64,
}

/// A stored item.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Item {
    /// Id.
    pub id: String,
    /// Canonical vectors per field.
    pub vectors: BTreeMap<String, Vec<f32>>,
    /// Metadata object.
    #[schema(value_type = Object)]
    pub metadata: Map<String, Json>,
    /// Keyword-search text.
    pub text: Option<String>,
    /// Incremented by every replace.
    pub version: i64,
    /// Creation time.
    pub created: String,
    /// Last replace.
    pub updated: String,
}

/// Body of a batch delete.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DeleteRequest {
    /// 1 to 1000 ids; unknown ids are ignored.
    pub ids: Vec<String>,
}

/// Outcome of a delete.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DeleteResult {
    /// Items removed.
    pub deleted: u64,
}

/// When a metadata filter is applied relative to the vector search.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum FilterMode {
    /// `pre`, falling back to `post` when the filter matches very many items.
    #[default]
    Auto,
    /// Find the matching items first and search only among them (exact when
    /// few match). Never misses a match that is among the true nearest.
    Pre,
    /// Search first, then drop non-matching results, widening the search
    /// until `k` results match or the index is exhausted.
    Post,
}

/// How a search was answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Strategy {
    /// HNSW search over the whole collection.
    Ann,
    /// HNSW search restricted to the items that match the filter.
    FilteredAnn,
    /// HNSW search with the filter applied to the results.
    PostFilter,
    /// Brute force over every candidate (small sets, `exact: true`).
    Exact,
    /// Brute force because the index is not available (being rebuilt).
    ExactFallback,
}

/// A similarity search.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchRequest {
    /// Vector field; may be omitted when the collection has exactly one.
    #[serde(default)]
    pub field: Option<String>,
    /// Query vector.
    pub vector: Vec<f64>,
    /// Number of results, 1 to 1000 (default 10).
    #[serde(default = "default_k")]
    pub k: u32,
    /// Metadata filter in the filter language, e.g.
    /// `category = 'news' && year >= 2020`.
    #[serde(default)]
    pub filter: Option<String>,
    /// Values for `{:name}` parameters of the filter.
    #[serde(default)]
    #[schema(value_type = Object)]
    pub params: HashMap<String, Json>,
    /// When the filter is applied.
    #[serde(default)]
    pub filter_mode: FilterMode,
    /// Brute force instead of the ANN index (ground truth, small sets).
    #[serde(default)]
    pub exact: bool,
    /// Return each hit's vector.
    #[serde(default)]
    pub include_vectors: bool,
    /// Return each hit's metadata (default `true`).
    #[serde(default = "yes")]
    pub include_metadata: bool,
    /// Return each hit's text.
    #[serde(default)]
    pub include_text: bool,
}

fn default_k() -> u32 {
    10
}

fn yes() -> bool {
    true
}

/// One search result.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SearchHit {
    /// Item id.
    pub id: String,
    /// Distance in the field's metric (lower is closer).
    pub distance: f32,
    /// Similarity (higher is closer): cosine similarity, dot product, or
    /// `1 / (1 + euclidean distance)`.
    pub score: f32,
    /// Metadata, unless `include_metadata` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<Object>)]
    pub metadata: Option<Map<String, Json>>,
    /// Text, when `include_text` is set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Vector, when `include_vectors` is set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vector: Option<Vec<f32>>,
}

/// Search results.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SearchResponse {
    /// Closest first.
    pub hits: Vec<SearchHit>,
    /// How the search was answered.
    pub strategy: Strategy,
    /// Items matching the filter, when the filter was evaluated first.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidates: Option<u64>,
    /// Server-side time in milliseconds.
    pub took_ms: f64,
}

/// Weights of the two rankings in hybrid search.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HybridWeights {
    /// Weight of the vector ranking (default 1).
    #[serde(default = "one")]
    pub vector: f64,
    /// Weight of the keyword ranking (default 1).
    #[serde(default = "one")]
    pub text: f64,
}

fn one() -> f64 {
    1.0
}

impl Default for HybridWeights {
    fn default() -> Self {
        Self { vector: 1.0, text: 1.0 }
    }
}

/// How the words of a keyword query combine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum TextOperator {
    /// Any word may match; items matching more words rank higher.
    #[default]
    Or,
    /// Every word must match.
    And,
}

/// A hybrid search: keyword (FTS5, BM25) and vector rankings fused with
/// Reciprocal Rank Fusion: `score = Σ weight / (rrf_k + rank)`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HybridRequest {
    /// Vector field; may be omitted when the collection has exactly one.
    #[serde(default)]
    pub field: Option<String>,
    /// Query vector (optional: keyword-only without it).
    #[serde(default)]
    pub vector: Option<Vec<f64>>,
    /// Keyword query; every word is matched literally (optional:
    /// vector-only without it).
    #[serde(default)]
    pub text: Option<String>,
    /// How the words combine (default `or`).
    #[serde(default)]
    pub text_operator: TextOperator,
    /// Number of results, 1 to 1000 (default 10).
    #[serde(default = "default_k")]
    pub k: u32,
    /// Candidates taken from each ranking before fusion (default
    /// `max(5 k, 50)`, at most 1000).
    #[serde(default)]
    pub candidates: Option<u32>,
    /// RRF constant (default 60).
    #[serde(default)]
    pub rrf_k: Option<f64>,
    /// Ranking weights (default 1 and 1).
    #[serde(default)]
    pub weights: Option<HybridWeights>,
    /// Metadata filter, applied to both rankings.
    #[serde(default)]
    pub filter: Option<String>,
    /// Values for `{:name}` parameters of the filter.
    #[serde(default)]
    #[schema(value_type = Object)]
    pub params: HashMap<String, Json>,
    /// When the filter is applied on the vector side.
    #[serde(default)]
    pub filter_mode: FilterMode,
    /// Return each hit's vector.
    #[serde(default)]
    pub include_vectors: bool,
    /// Return each hit's metadata (default `true`).
    #[serde(default = "yes")]
    pub include_metadata: bool,
    /// Return each hit's text.
    #[serde(default)]
    pub include_text: bool,
}

/// One hybrid search result.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HybridHit {
    /// Item id.
    pub id: String,
    /// Fused RRF score (higher is better).
    pub score: f64,
    /// 1-based rank in the vector ranking.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vector_rank: Option<u32>,
    /// Distance in the vector ranking.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vector_distance: Option<f32>,
    /// 1-based rank in the keyword ranking.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_rank: Option<u32>,
    /// BM25 relevance in the keyword ranking (higher is better).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_score: Option<f64>,
    /// Metadata, unless `include_metadata` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<Object>)]
    pub metadata: Option<Map<String, Json>>,
    /// Text, when `include_text` is set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Vector, when `include_vectors` is set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vector: Option<Vec<f32>>,
}

/// Hybrid search results.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HybridResponse {
    /// Best first.
    pub hits: Vec<HybridHit>,
    /// How the vector side was answered (absent without a query vector).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vector_strategy: Option<Strategy>,
    /// Candidates from the vector ranking.
    pub vector_candidates: u64,
    /// Candidates from the keyword ranking.
    pub text_candidates: u64,
    /// Server-side time in milliseconds.
    pub took_ms: f64,
}

/// Body of a rebuild request.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RebuildRequest {
    /// Only this field (default: every field of the collection).
    #[serde(default)]
    pub field: Option<String>,
}

/// Life cycle of a derived index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum IndexState {
    /// Not loaded since start; loaded on first use or by the warm-up task.
    Unloaded,
    /// Serving searches.
    Ready,
    /// Being rebuilt from the canonical vectors (see `progress`). Searches are
    /// answered from the previous index if there is one, else exactly.
    Building,
    /// The last rebuild failed (see `reason`); searches are exact.
    Failed,
}

/// Progress of a rebuild.
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
pub struct Progress {
    /// Vectors added so far.
    pub done: u64,
    /// Vectors to add.
    pub total: u64,
}

/// The last completed rebuild.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RebuildInfo {
    /// Why it ran (`missing`, `corrupt`, `stale`, `version_mismatch`,
    /// `params_changed`, `manual`, `apply_failed`).
    pub reason: String,
    /// Detail of the reason.
    pub detail: String,
    /// Vectors indexed.
    pub vectors: u64,
    /// Wall-clock duration.
    pub duration_ms: u64,
    /// Completion time.
    pub finished: String,
}

/// Result of a recall check: ANN results compared with exact results.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RecallReport {
    /// Vector field.
    pub field: String,
    /// Results compared per query.
    pub k: u32,
    /// Queries run (stored vectors used as queries).
    pub samples: u32,
    /// Mean of |ANN ∩ exact| / k, from 0 to 1.
    pub recall: f64,
    /// Total time of the ANN searches.
    pub ann_ms: f64,
    /// Total time of the exact searches.
    pub exact_ms: f64,
    /// When it ran.
    pub measured: String,
}

/// Body of a recall check.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RecallRequest {
    /// Vector field; may be omitted when the collection has exactly one.
    #[serde(default)]
    pub field: Option<String>,
    /// Number of queries (default 100, at most 1000).
    #[serde(default)]
    pub samples: Option<u32>,
    /// Results per query (default 10, at most 100).
    #[serde(default)]
    pub k: Option<u32>,
}

/// State and metrics of one field's ANN index.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct IndexStatus {
    /// Vector field.
    pub field: String,
    /// Life cycle state.
    pub state: IndexState,
    /// `ann` when searches use an index, `exact` when they scan.
    pub serving: String,
    /// Why the index is not ready, or why the running rebuild started.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Progress of the running rebuild.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<Progress>,
    /// Canonical vectors stored for the field.
    pub canonical_vectors: u64,
    /// Vectors in the serving index.
    pub indexed_vectors: u64,
    /// Memory used by the serving index.
    pub memory_bytes: u64,
    /// Size of the index file when saved.
    pub serialized_bytes: u64,
    /// Reserved slots of the serving index.
    pub capacity: u64,
    /// ANN engine.
    pub engine: String,
    /// ANN engine version.
    pub engine_version: String,
    /// Element type of the index.
    pub quantization: Quantization,
    /// HNSW links per node.
    pub connectivity: u32,
    /// HNSW candidate list size while searching.
    pub expansion_search: u32,
    /// The index has changes not yet written to its file.
    pub dirty: bool,
    /// When the file was last written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_saved: Option<String>,
    /// The last completed rebuild.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_rebuild: Option<RebuildInfo>,
    /// The last recall check.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_recall: Option<RecallReport>,
    /// Searches since start.
    pub searches: u64,
    /// Searches answered by the ANN index since start.
    pub ann_searches: u64,
    /// Searches answered by brute force since start.
    pub exact_searches: u64,
    /// Mean search latency since start, in milliseconds.
    pub mean_search_ms: f64,
}

/// Statistics of one collection.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CollectionStats {
    /// Collection.
    pub collection: String,
    /// Items.
    pub items: u64,
    /// Items with keyword-search text.
    pub items_with_text: u64,
    /// Committed changes since creation.
    pub change_seq: i64,
    /// One entry per vector field.
    pub indexes: Vec<IndexStatus>,
}

/// Statistics of the caller's tenant.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct VectorStats {
    /// Collections.
    pub collections: u64,
    /// Vector fields (one ANN index each).
    pub indexes: u64,
    /// Indexes currently being rebuilt.
    pub building: u64,
    /// Vectors in loaded indexes.
    pub indexed_vectors: u64,
    /// Memory used by loaded indexes.
    pub memory_bytes: u64,
}

/// Checks a collection or field name: `[A-Za-z][A-Za-z0-9_]{0,62}`.
pub(crate) fn check_name(kind: &str, name: &str) -> Result<(), VectorError> {
    let mut chars = name.chars();
    let ok = name.len() <= 63
        && chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    if ok {
        Ok(())
    } else {
        Err(VectorError::invalid(
            "invalid_name",
            format!("{kind} name {name:?} must match [A-Za-z][A-Za-z0-9_]{{0,62}}"),
        ))
    }
}

/// Checks an item id: 1 to 64 of `[A-Za-z0-9_-]`.
pub(crate) fn check_id(id: &str) -> Result<(), VectorError> {
    let ok = !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if ok {
        Ok(())
    } else {
        Err(VectorError::invalid(
            "invalid_id",
            format!("item id {id:?} must be 1 to 64 of [A-Za-z0-9_-]"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_ids_are_checked() {
        assert!(check_name("collection", "docs_1").is_ok());
        for bad in ["", "1docs", "_x", "a-b", "a b", "ä", &"a".repeat(64)] {
            assert!(check_name("collection", bad).is_err(), "{bad:?}");
        }
        assert!(check_id("A-b_9").is_ok());
        for bad in ["", "a/b", "../x", "a b", &"a".repeat(65)] {
            assert!(check_id(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn enums_round_trip_their_stored_names() {
        for m in [Metric::Cosine, Metric::L2, Metric::Dot] {
            assert_eq!(Metric::parse(m.as_str()), Some(m));
        }
        for q in [Quantization::F32, Quantization::F16, Quantization::I8] {
            assert_eq!(Quantization::parse(q.as_str()), Some(q));
        }
        assert_eq!(Metric::parse("hamming"), None);
    }
}
