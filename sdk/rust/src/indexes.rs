//! Index management, planner statistics and the slow-query log (`/api/v1/indexes`,
//! `/api/v1/slow-queries`): `client.indexes()`.

use std::sync::Arc;

use serde::Serialize;
use serde_json::json;

use crate::error::Result;
use crate::generated::{
    IndexInfoSchema, IndexList, IndexRecommendationSchema, IndexRecommendations, IndexStatsSchema, SlowQueryLog,
    ThresholdBody,
};
use crate::http::{Req, Transport, seg};

/// An index with its statistics.
pub type IndexInfo = IndexInfoSchema;
/// Planner statistics of one index.
pub type IndexStats = IndexStatsSchema;
/// An index suggested by the slow-query log.
pub type IndexRecommendation = IndexRecommendationSchema;

/// One key of an index.
#[derive(Debug, Clone, Serialize)]
pub struct IndexKey {
    /// Field path in filter syntax (`price`, `metadata.brand`, `title:lower`).
    pub path: String,
    /// Descending order.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub desc: Option<bool>,
}

impl IndexKey {
    /// An ascending key.
    #[must_use]
    pub fn asc(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            desc: None,
        }
    }

    /// A descending key.
    #[must_use]
    pub fn desc(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            desc: Some(true),
        }
    }
}

/// Kind of an index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum IndexKind {
    /// B-tree (default).
    Btree,
    /// Full-text.
    Fulltext,
    /// Trigram (fuzzy search; portable on every engine, ADR-0034).
    Trigram,
}

/// An index to create.
#[derive(Debug, Clone, Serialize)]
pub struct NewIndex {
    /// Collection name or id.
    pub collection: String,
    /// Keys in order. For a full-text index, the fields to index.
    pub keys: Vec<IndexKey>,
    /// Index name (generated when absent).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Unique.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unique: Option<bool>,
    /// Partial index condition, in filter syntax.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partial: Option<String>,
    /// Kind.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<IndexKind>,
}

impl NewIndex {
    /// A B-tree index over `keys`.
    #[must_use]
    pub fn new(collection: impl Into<String>, keys: Vec<IndexKey>) -> Self {
        Self {
            collection: collection.into(),
            keys,
            name: None,
            unique: None,
            partial: None,
            kind: None,
        }
    }

    /// Sets the name.
    #[must_use]
    pub fn name(mut self, n: impl Into<String>) -> Self {
        self.name = Some(n.into());
        self
    }

    /// Makes it unique.
    #[must_use]
    pub fn unique(mut self) -> Self {
        self.unique = Some(true);
        self
    }

    /// Sets the kind.
    #[must_use]
    pub fn kind(mut self, k: IndexKind) -> Self {
        self.kind = Some(k);
        self
    }
}

/// Indexes and the slow-query log: `client.indexes()`.
#[derive(Clone)]
pub struct IndexesApi {
    pub(crate) t: Arc<Transport>,
}

const I: &str = "/api/v1/indexes";

impl IndexesApi {
    /// Indexes with their statistics; `collection` limits the list to one collection.
    pub async fn list(&self, collection: Option<&str>) -> Result<Vec<IndexInfo>> {
        let r: IndexList = self.t.json(Req::get(I).q_opt("collection", collection)).await?;
        Ok(r.items)
    }

    /// Creates an index. A taken name or duplicate values for a unique index are a conflict.
    pub async fn create(&self, input: &NewIndex) -> Result<IndexInfo> {
        self.t.json(Req::post(I).json(input)?).await
    }

    /// One index.
    pub async fn get(&self, name: &str) -> Result<IndexInfo> {
        self.t.json(Req::get(format!("{I}/{}", seg(name)))).await
    }

    /// Drops a manual index (schema-derived ones follow their field).
    pub async fn drop_index(&self, name: &str) -> Result<()> {
        self.t.unit(Req::delete(format!("{I}/{}", seg(name)))).await
    }

    /// `REINDEX` + `ANALYZE`, or re-populates a full-text index.
    pub async fn rebuild(&self, name: &str) -> Result<IndexInfo> {
        self.t
            .json(Req::post(format!("{I}/{}/rebuild", seg(name))).json_raw(json!({})))
            .await
    }

    /// Planner statistics, estimated rows and size on disk of one index.
    pub async fn stats(&self, name: &str) -> Result<IndexStats> {
        self.t.json(Req::get(format!("{I}/{}/stats", seg(name)))).await
    }

    /// Indexes suggested by the slow-query log.
    pub async fn recommendations(&self) -> Result<Vec<IndexRecommendation>> {
        let r: IndexRecommendations = self.t.json(Req::get(format!("{I}/recommendations"))).await?;
        Ok(r.items)
    }

    /// Refreshes planner statistics (`ANALYZE`), of one collection or of everything.
    pub async fn analyze(&self, collection: Option<&str>) -> Result<()> {
        self.t
            .unit(Req::post(format!("{I}/analyze")).json_value(json!({"collection": collection})))
            .await
    }

    /// Recent statements slower than the threshold (SQL text only, never parameter values).
    pub async fn slow_queries(&self) -> Result<SlowQueryLog> {
        self.t.json(Req::get("/api/v1/slow-queries")).await
    }

    /// Empties the slow-query log.
    pub async fn clear_slow_queries(&self) -> Result<()> {
        self.t.unit(Req::delete("/api/v1/slow-queries")).await
    }

    /// Sets the slow-query threshold in milliseconds (`0` logs every statement); returns the new value.
    pub async fn set_slow_query_threshold(&self, threshold_ms: i64) -> Result<i64> {
        let r: ThresholdBody = self
            .t
            .json(Req::put("/api/v1/slow-queries/threshold").json_raw(json!({"threshold_ms": threshold_ms})))
            .await?;
        Ok(r.threshold_ms)
    }
}
