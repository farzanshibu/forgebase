//! The ForgeBase graph model (FB-51, SPEC §7) on SQLite.
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
//!   fails closed. Graph data has no row-level policies: a filter decision is a
//!   denial.
//! * **REST** ([`router`], [`openapi`]): mounted at `/api/v1/graph`.
//!
//! Limits: depth 1 to 10, results 1 to 1000, at most 20 edge types and 8
//! property conditions per query, properties up to 64 KiB, shortest-path
//! searches up to 100 000 nodes.

mod error;
mod http;
mod model;
mod service;

pub use error::GraphError;
pub use http::{openapi, router};
pub use model::*;
pub use service::GraphService;
