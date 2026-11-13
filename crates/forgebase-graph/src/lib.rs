//! The ForgeBase graph model (FB-51, SPEC §7) on SQLite and PostgreSQL.
//!
//! * **Model**: nodes (id, label, JSON properties) and typed edges (type,
//!   source, target, directed or undirected, JSON properties) in two indexed
//!   relational tables (`migrations/system/0006_graph.sql`), indexed for both
//!   directions. Rows are tenant-scoped.
//! * **Queries** ([`GraphService`]): create / change / delete nodes and edges;
//!   neighbours (in, out, both, by edge type); multi-hop traversal with a
//!   recursive CTE (depth-limited, cycle-safe, filterable); shortest path
//!   (breadth-first, bounded). Every value is a bound parameter.
//! * **Access**: every call builds on the data core's
//!   [`AccessContext`](forgebase_data::AccessContext) and asks the
//!   [`Authorizer`](forgebase_data::Authorizer) first
//!   ([`Action::GraphRead`](forgebase_data::Action) /
//!   [`Action::GraphWrite`](forgebase_data::Action)); the default authorizer
//!   fails closed. Label and edge-type rules (FB-139, [`GraphAccess`]) are
//!   compiled into every statement: a walk never crosses a hidden edge or
//!   enters a hidden node. Callers without a tenant: privileged only
//!   (`tenant_required`).
//! * **REST** ([`router`], [`openapi`]): mounted at `/api/v1/graph`.
//! * **openCypher** (ADR-0029 §1.5, FB-163; ADR-0034): `POST /graph/cypher`
//!   runs read-only queries, on PostgreSQL with Apache AGE on a derived
//!   per-tenant AGE graph ([`age`]) that every write keeps in step, and
//!   everywhere else on the portable engine ([`portable`]), which compiles a
//!   subset of openCypher to SQL over the canonical tables; label and
//!   edge-type rules apply inside the query on both.
//! * **Shards** ([`GraphService::attach_router`]): every call runs on its
//!   tenant's database set.
//!
//! Limits: depth 1 to 10, results 1 to 1000, at most 20 edge types and 8
//! property conditions per query, properties up to 64 KiB, shortest-path
//! searches up to 100 000 nodes.

mod access;
pub mod age;
mod cypher;
mod error;
mod http;
mod model;
pub mod portable;
mod routed;
mod service;

pub use access::{GraphAccess, Rules};
pub use cypher::{
    CypherDropped, CypherQuery, CypherResult, CypherStatus, DEFAULT_LIMIT, DEFAULT_TIMEOUT_MS, MATERIALISE_JOB,
    MAX_LIMIT, MAX_QUERY_BYTES, MAX_TIMEOUT_MS, MaterialiseJobs, MaterialiseReport,
};
pub use error::GraphError;
pub use http::{openapi, router};
pub use model::*;
pub use service::GraphService;
