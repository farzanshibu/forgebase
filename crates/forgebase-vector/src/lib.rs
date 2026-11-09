//! ForgeBase vector database (FB-50, ADR-0012, SPEC §6).
//!
//! | Piece | Where |
//! |---|---|
//! | Vector collections per tenant, vector fields (dimension, metric `cosine`/`l2`/`dot`, index element type) | [`VectorService::create_collection`], [`VectorService::add_field`] |
//! | Canonical vectors + metadata + text in SQLite, validated (dimension, finite, non-zero cosine) | [`VectorService::upsert`], [`VectorService::get_item`] |
//! | USearch HNSW index per field, derived and rebuildable, behind an engine trait | [`AnnEngine`], [`AnnIndex`], [`Usearch`] |
//! | Index files with manifest + checksum; rebuilt when missing, corrupt, stale or from another engine version | [`VectorService::start_background`], [`VectorService::rebuild`] |
//! | kNN with metadata pre/post filters (the data core's filter language), exact search for small sets | [`VectorService::search`] |
//! | Hybrid search: FTS5 keyword + vector, Reciprocal Rank Fusion | [`VectorService::hybrid`] |
//! | Recall check, index statistics, Prometheus metrics | [`VectorService::recall`], [`VectorService::stats`] |
//! | REST under `/api/v1/vector` with OpenAPI | [`router`], [`extension`], [`openapi`] |
//!
//! SQLite holds the only copy of every vector; the files under
//! `<data_dir>/indexes/vector/` can be deleted at any time and are rebuilt
//! from it (SPEC §4.5, §27 #23). Every operation asks the data core's
//! authorizer first ([`forgebase_data::Action::VectorRead`],
//! [`forgebase_data::Action::VectorWrite`],
//! [`forgebase_data::Action::VectorAdmin`]) and stays inside the caller's
//! tenant. This crate contains no authentication or policy code and generates
//! no embeddings.

mod engine;
mod error;
mod filter;
mod http;
mod index;
mod metric;
pub mod model;
mod service;
mod store;

pub use engine::{AnnEngine, AnnIndex, EngineStats, IndexParams, Usearch};
pub use error::VectorError;
pub use http::{MAX_BODY, RebuildResponse, extension, openapi, router};
pub use service::{DEFAULT_CONNECTIVITY, DEFAULT_EXPANSION_ADD, DEFAULT_EXPANSION_SEARCH, VectorConfig, VectorService};
