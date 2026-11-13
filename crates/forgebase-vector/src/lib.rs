//! ForgeBase vector database (FB-50, ADR-0012, SPEC §6).
//!
//! | Piece | Where |
//! |---|---|
//! | Vector collections per tenant, vector fields (dimension, metric `cosine`/`l2`/`dot`, index element type) | [`VectorService::create_collection`], [`VectorService::add_field`] |
//! | Canonical vectors + metadata + text in SQLite, validated (dimension, finite, non-zero cosine) | [`VectorService::upsert`], [`VectorService::get_item`] |
//! | USearch HNSW index per field, derived and rebuildable, behind an engine trait | [`AnnEngine`], [`AnnIndex`], [`Usearch`] |
//! | Index files with manifest + checksum; rebuilt when missing, corrupt, stale or from another engine version | [`VectorService::start_background`], [`VectorService::rebuild`] |
//! | kNN with metadata pre/post filters (the data core's filter language), exact search for small sets | [`VectorService::search`] |
//! | Hybrid search: full-text keyword (FTS5 on SQLite, `tsvector` on PostgreSQL) + vector, Reciprocal Rank Fusion | [`VectorService::hybrid`] |
//! | Recall check, index statistics, Prometheus metrics | [`VectorService::recall`], [`VectorService::stats`] |
//! | PostgreSQL with pgvector: `vec` column, partial HNSW index per field, iterative scans, concurrent rebuilds, recall in SQL; `vector.convert` both ways (FB-162) | [`VectorService::storage_engine`], [`VectorService::convert`], [`register_jobs`] |
//! | Database sets: a tenant's vectors on its set, an engine per set, in-process indexes loaded from every set and following a moved tenant, `409 tenant_moving` (FB-172) | [`VectorService::attach_router`], [`VectorService::open_routed`], [`VectorService::storage_engines`], [`VectorService::convert_set`] |
//! | REST under `/api/v1/vector` with OpenAPI | [`router`], [`extension`], [`openapi`] |
//!
//! SQLite holds the only copy of every vector; the files under
//! `<data_dir>/indexes/vector/` can be deleted at any time and are rebuilt
//! from it (SPEC §4.5, §27 #23). Every operation asks the data core's
//! authorizer first ([`forgebase_data::Action::VectorRead`],
//! [`forgebase_data::Action::VectorWrite`],
//! [`forgebase_data::Action::VectorAdmin`]) and stays inside the caller's
//! tenant. Collection rules (`vector_collection` resource rules over
//! [`item_schema`]) pre-filter searches and guard writes; a
//! `source_collection` re-checks hits against the records' View rule
//! (FB-139). This crate contains no authentication or policy code and
//! generates no embeddings.

mod engine;
mod error;
mod filter;
mod http;
mod index;
mod jobs;
mod metric;
pub mod model;
mod pg;
mod service;
mod store;

pub use engine::{AnnEngine, AnnIndex, EngineStats, IndexParams, Usearch};
pub use error::VectorError;
pub use filter::{ItemRule, item_schema};
pub use http::{MAX_BODY, RebuildResponse, extension, openapi, router};
pub use jobs::{CONVERT_JOB, register_jobs};
pub use service::{
    DEFAULT_CONNECTIVITY, DEFAULT_EXPANSION_ADD, DEFAULT_EXPANSION_SEARCH, SOURCE_COLLECTION_OPTION, VectorConfig,
    VectorService,
};
