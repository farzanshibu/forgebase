//! Vector search (`/api/v1/vector`): collections with one or more vector fields, upsert,
//! k-nearest-neighbour search with metadata filters, keyword + vector hybrid search and index
//! maintenance. `client.vector()`.
//!
//! ```no_run
//! # async fn run(fb: forgebase_sdk::ForgeBase) -> forgebase_sdk::Result<()> {
//! use forgebase_sdk::generated::Metric;
//! use forgebase_sdk::vector::{FieldSpec, NewVectorCollection, SearchOptions, UpsertItem};
//! let vector = fb.vector();
//! vector
//!     .create(&NewVectorCollection::new("docs").field(FieldSpec::new("emb", 3, Metric::Cosine)))
//!     .await?;
//! let docs = vector.collection("docs");
//! docs.upsert(&[UpsertItem::new().id("a").vector("emb", &[1.0, 0.0, 0.0])]).await?;
//! let hits = docs.search(&SearchOptions::new(&[1.0, 0.0, 0.0]).k(3)).await?;
//! # let _ = hits; Ok(()) }
//! ```

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::Serialize;
use serde_json::{Value, json};

use crate::error::{Error, Result};
use crate::generated::{
    CollectionStats, FilterMode, HybridResponse, IndexStatus, Item, Metric, Quantization, RecallReport, SearchResponse,
    TextOperator, UpsertResult, VectorCollection, VectorField, VectorStats,
};
use crate::http::{Req, Transport, seg};

const V: &str = "/api/v1/vector";

/// A vector field to create.
#[derive(Debug, Clone, Serialize)]
pub struct FieldSpec {
    /// `[A-Za-z][A-Za-z0-9_]{0,62}`.
    pub name: String,
    /// Components, 1 to 4096.
    pub dimension: u32,
    /// Distance function. `cosine`: `score` is the cosine similarity; `l2`: `1 / (1 + distance)`; `dot`: the dot product.
    pub metric: Metric,
    /// Element type of the ANN index (default `f32`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quantization: Option<Quantization>,
    /// HNSW links per node (default 16, 2 to 128).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connectivity: Option<u32>,
    /// HNSW candidate list size while inserting (default 128).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expansion_add: Option<u32>,
    /// HNSW candidate list size while searching (default 128): higher is better recall, slower.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expansion_search: Option<u32>,
}

impl FieldSpec {
    /// A field with server defaults for the index.
    #[must_use]
    pub fn new(name: impl Into<String>, dimension: u32, metric: Metric) -> Self {
        Self {
            name: name.into(),
            dimension,
            metric,
            quantization: None,
            connectivity: None,
            expansion_add: None,
            expansion_search: None,
        }
    }
}

/// A collection to create.
#[derive(Debug, Clone, Default, Serialize)]
pub struct NewVectorCollection {
    /// `[A-Za-z][A-Za-z0-9_]{0,62}`, unique per tenant.
    pub name: String,
    /// Description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Up to 16 vector fields (more can be added later with [`VectorFieldsApi::add`]).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<FieldSpec>,
    /// Searches over at most this many candidates are exact (brute force) instead of approximate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exact_threshold: Option<u32>,
    /// Data collection the items are copied from (item id = record id). Searches then re-check
    /// every item against that collection's View rule as the caller. Set at creation only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_collection: Option<String>,
}

impl NewVectorCollection {
    /// A collection named `name`.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Self::default()
        }
    }

    /// Adds a vector field.
    #[must_use]
    pub fn field(mut self, f: FieldSpec) -> Self {
        self.fields.push(f);
        self
    }
}

/// One item to insert or replace.
#[derive(Debug, Clone, Default, Serialize)]
pub struct UpsertItem {
    /// 1 to 64 of `[A-Za-z0-9_-]`; generated when absent. An existing item with this id is replaced completely.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Vector per field name; fields left out have no vector for this item.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub vectors: BTreeMap<String, Vec<f32>>,
    /// Filterable JSON object (up to 64 KiB).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    /// Text for the keyword side of hybrid search.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

impl UpsertItem {
    /// An empty item.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the id.
    #[must_use]
    pub fn id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Sets the vector of `field`.
    #[must_use]
    pub fn vector(mut self, field: impl Into<String>, v: &[f32]) -> Self {
        self.vectors.insert(field.into(), v.to_vec());
        self
    }

    /// Sets the metadata object.
    #[must_use]
    pub fn metadata(mut self, m: Value) -> Self {
        self.metadata = Some(m);
        self
    }

    /// Sets the keyword text.
    #[must_use]
    pub fn text(mut self, t: impl Into<String>) -> Self {
        self.text = Some(t.into());
        self
    }
}

/// Options of a k-nearest-neighbour search.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SearchOptions {
    /// Query vector.
    pub vector: Vec<f32>,
    /// Vector field; may be left out when the collection has exactly one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// Number of results, 1 to 1000 (default 10).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub k: Option<u32>,
    /// Metadata filter, e.g. `category = 'news' && year >= {:min}`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    /// Values of the `{:name}` placeholders of `filter`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
    /// `auto` (default), `pre` (filter first, exact when few match) or `post` (search first, widen until `k` match).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter_mode: Option<FilterMode>,
    /// Brute force instead of the ANN index (ground truth, small sets).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exact: Option<bool>,
    /// Return each hit's vector.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_vectors: Option<bool>,
    /// Return each hit's metadata (server default `true`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_metadata: Option<bool>,
    /// Return each hit's text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_text: Option<bool>,
}

impl SearchOptions {
    /// Searches around `vector`.
    #[must_use]
    pub fn new(vector: &[f32]) -> Self {
        Self {
            vector: vector.to_vec(),
            ..Self::default()
        }
    }

    /// Number of results.
    #[must_use]
    pub fn k(mut self, k: u32) -> Self {
        self.k = Some(k);
        self
    }

    /// Vector field.
    #[must_use]
    pub fn field(mut self, field: impl Into<String>) -> Self {
        self.field = Some(field.into());
        self
    }

    /// Metadata filter with `{:name}` placeholders bound from `params`.
    #[must_use]
    pub fn filter(mut self, filter: impl Into<String>, params: Option<Value>) -> Self {
        self.filter = Some(filter.into());
        self.params = params;
        self
    }

    /// Exact (brute-force) search.
    #[must_use]
    pub fn exact(mut self) -> Self {
        self.exact = Some(true);
        self
    }
}

/// Weights of the two rankings of a hybrid search (default 1 and 1).
#[derive(Debug, Clone, Default, Serialize)]
pub struct HybridWeights {
    /// Weight of the vector ranking.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vector: Option<f64>,
    /// Weight of the keyword ranking.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<f64>,
}

/// Options of a hybrid (keyword + vector) search.
#[derive(Debug, Clone, Default, Serialize)]
pub struct HybridOptions {
    /// Query vector; without it the search is keyword-only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vector: Option<Vec<f32>>,
    /// Keyword query (every word matched literally); without it the search is vector-only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// `or` (default, more matching words rank higher) or `and`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_operator: Option<TextOperator>,
    /// Vector field; may be left out when the collection has exactly one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// Number of results, 1 to 1000 (default 10).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub k: Option<u32>,
    /// Candidates taken from each ranking before fusion (default `max(5k, 50)`, at most 1000).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidates: Option<u32>,
    /// Reciprocal Rank Fusion constant (default 60): `score = Σ weight / (rrf_k + rank)`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rrf_k: Option<u32>,
    /// Weights of the two rankings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weights: Option<HybridWeights>,
    /// Metadata filter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    /// Values of the `{:name}` placeholders of `filter`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
    /// When the filter applies.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter_mode: Option<FilterMode>,
    /// Return each hit's vector.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_vectors: Option<bool>,
    /// Return each hit's metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_metadata: Option<bool>,
    /// Return each hit's text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_text: Option<bool>,
}

impl HybridOptions {
    /// Fuses a keyword query with a vector query.
    #[must_use]
    pub fn new(text: impl Into<String>, vector: &[f32]) -> Self {
        Self {
            text: Some(text.into()),
            vector: Some(vector.to_vec()),
            ..Self::default()
        }
    }

    /// Keyword-only search.
    #[must_use]
    pub fn text_only(text: impl Into<String>) -> Self {
        Self {
            text: Some(text.into()),
            ..Self::default()
        }
    }

    /// Number of results.
    #[must_use]
    pub fn k(mut self, k: u32) -> Self {
        self.k = Some(k);
        self
    }
}

/// Options of a recall measurement.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RecallOptions {
    /// Vector field; may be left out when the collection has exactly one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// Queries (stored vectors), default 100, at most 1000.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub samples: Option<u32>,
    /// Results per query, default 10, at most 100.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub k: Option<u32>,
}

/// Vector search: `client.vector()`.
#[derive(Clone)]
pub struct VectorApi {
    pub(crate) t: Arc<Transport>,
}

impl VectorApi {
    /// A collection handle: `fb.vector().collection("docs").search(..)`.
    #[must_use]
    pub fn collection(&self, name: &str) -> VectorCollectionApi {
        VectorCollectionApi {
            t: self.t.clone(),
            name: name.to_owned(),
        }
    }

    /// Collections of the tenant, by name.
    pub async fn list(&self) -> Result<Vec<VectorCollection>> {
        self.t.items(Req::get(format!("{V}/collections"))).await
    }

    /// Creates a collection with its vector fields. A taken name is a conflict.
    pub async fn create(&self, input: &NewVectorCollection) -> Result<VectorCollection> {
        self.t.json(Req::post(format!("{V}/collections")).json(input)?).await
    }

    /// Totals over the tenant: collections, indexes, indexes building, memory.
    pub async fn stats(&self) -> Result<VectorStats> {
        self.t.json(Req::get(format!("{V}/stats"))).await
    }
}

/// One vector collection: `client.vector().collection("docs")`.
#[derive(Clone)]
pub struct VectorCollectionApi {
    t: Arc<Transport>,
    name: String,
}

impl VectorCollectionApi {
    fn base(&self) -> String {
        format!("{V}/collections/{}", seg(&self.name))
    }

    /// Collection name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Fields of this collection.
    #[must_use]
    pub fn fields(&self) -> VectorFieldsApi {
        VectorFieldsApi {
            t: self.t.clone(),
            base: format!("{}/fields", self.base()),
        }
    }

    /// The collection with its fields and item count.
    pub async fn get(&self) -> Result<VectorCollection> {
        self.t.json(Req::get(self.base())).await
    }

    /// Drops the collection with all its items, vectors and index files.
    pub async fn drop_collection(&self) -> Result<()> {
        self.t.unit(Req::delete(self.base())).await
    }

    /// Inserts or replaces 1 to 1000 items in one transaction. The result lists the ids in request order.
    pub async fn upsert(&self, items: &[UpsertItem]) -> Result<UpsertResult> {
        self.t
            .json(Req::post(format!("{}/upsert", self.base())).json_raw(json!({"items": items})))
            .await
    }

    /// One item with its canonical vectors.
    pub async fn get_item(&self, id: &str) -> Result<Item> {
        self.t
            .json(Req::get(format!("{}/items/{}", self.base(), seg(id))))
            .await
    }

    /// Deletes one item (a missing id is a not-found error).
    pub async fn delete_item(&self, id: &str) -> Result<()> {
        self.t
            .unit(Req::delete(format!("{}/items/{}", self.base(), seg(id))))
            .await
    }

    /// Deletes 1 to 1000 items by id; unknown ids are ignored. Returns how many were deleted.
    pub async fn delete_items(&self, ids: &[&str]) -> Result<u64> {
        let v = self
            .t
            .value(Req::post(format!("{}/delete", self.base())).json_raw(json!({"ids": ids})))
            .await?;
        Ok(v.get("deleted").and_then(Value::as_u64).unwrap_or(0))
    }

    /// k-nearest-neighbour search with an optional metadata filter; closest first.
    pub async fn search(&self, o: &SearchOptions) -> Result<SearchResponse> {
        self.t
            .json(Req::post(format!("{}/search", self.base())).json(o)?.idempotent())
            .await
    }

    /// Hybrid search: keyword (BM25) and vector rankings fused with Reciprocal Rank Fusion; best first.
    pub async fn hybrid(&self, o: &HybridOptions) -> Result<HybridResponse> {
        self.t
            .json(Req::post(format!("{}/hybrid", self.base())).json(o)?.idempotent())
            .await
    }

    /// Item counts and the state, progress and metrics of each field's index.
    pub async fn stats(&self) -> Result<CollectionStats> {
        self.t.json(Req::get(format!("{}/stats", self.base()))).await
    }

    /// Rebuilds ANN indexes from the canonical vectors in the background; follow `progress` in [`stats`](Self::stats).
    pub async fn rebuild(&self, field: Option<&str>) -> Result<Vec<IndexStatus>> {
        let v = self
            .t
            .value(Req::post(format!("{}/rebuild", self.base())).json_value(json!({"field": field})))
            .await?;
        serde_json::from_value(v.get("indexes").cloned().unwrap_or_else(|| Value::Array(Vec::new())))
            .map_err(|e| Error::InvalidResponse(format!("rebuild: {e}")))
    }

    /// Measures recall@k of the ANN index against exact search, using stored vectors as queries.
    pub async fn recall(&self, o: &RecallOptions) -> Result<RecallReport> {
        self.t
            .json(Req::post(format!("{}/recall", self.base())).json(o)?.idempotent())
            .await
    }
}

/// Vector fields of one collection: `client.vector().collection("docs").fields()`.
#[derive(Clone)]
pub struct VectorFieldsApi {
    t: Arc<Transport>,
    base: String,
}

impl VectorFieldsApi {
    /// The collection's fields.
    pub async fn list(&self) -> Result<Vec<VectorField>> {
        self.t.items(Req::get(self.base.clone())).await
    }

    /// Adds a field. Existing items have no vector for it until upserted again.
    pub async fn add(&self, spec: &FieldSpec) -> Result<VectorField> {
        self.t.json(Req::post(self.base.clone()).json(spec)?).await
    }

    /// Drops a field, its vectors and its index.
    pub async fn drop_field(&self, field: &str) -> Result<()> {
        self.t.unit(Req::delete(format!("{}/{}", self.base, seg(field)))).await
    }
}
