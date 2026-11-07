//! Request and response types of the graph service.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json};
use utoipa::ToSchema;

/// Default and largest traversal depth (hops).
pub const DEFAULT_DEPTH: u32 = 3;
/// Deepest traversal or path search a caller may ask for.
pub const MAX_DEPTH: u32 = 10;
/// Default page / result size.
pub const DEFAULT_LIMIT: u32 = 100;
/// Largest page / result size.
pub const MAX_LIMIT: u32 = 1_000;
/// Most edge types in one filter.
pub const MAX_EDGE_TYPES: usize = 20;
/// Most property equality conditions in one filter.
pub const MAX_FILTERS: usize = 8;
/// Largest `properties` object, in bytes of JSON.
pub const MAX_PROPERTIES_BYTES: usize = 64 * 1024;
/// Most nodes a shortest-path search may visit before it gives up.
pub const MAX_VISITED: usize = 100_000;
/// Most edges returned with a traversal.
pub const MAX_TRAVERSE_EDGES: usize = 5_000;

/// A graph node.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Node {
    /// Node id (unique per tenant).
    pub id: String,
    /// Optional kind, e.g. `User` or `Product`.
    pub label: String,
    /// JSON object of properties.
    #[schema(value_type = Object)]
    pub properties: Json,
    /// RFC 3339 creation time.
    pub created_at: String,
    /// RFC 3339 time of the last change.
    pub updated_at: String,
}

/// A typed edge between two nodes.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Edge {
    /// Edge id (unique per tenant).
    pub id: String,
    /// Relationship type, e.g. `FOLLOWS`.
    #[serde(rename = "type")]
    pub edge_type: String,
    /// Source node id.
    pub source: String,
    /// Target node id.
    pub target: String,
    /// `false` for an undirected relationship (walked both ways).
    pub directed: bool,
    /// JSON object of properties.
    #[schema(value_type = Object)]
    pub properties: Json,
    /// RFC 3339 creation time.
    pub created_at: String,
    /// RFC 3339 time of the last change.
    pub updated_at: String,
}

/// Body of `POST /nodes`.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewNode {
    /// Id; generated when absent. 1 to 128 of `A-Z a-z 0-9 _ . : @ -`.
    pub id: Option<String>,
    /// Kind, up to 64 characters.
    pub label: Option<String>,
    /// Property object (default `{}`), up to 64 KiB.
    #[schema(value_type = Option<Object>)]
    pub properties: Option<Json>,
}

/// Body of `PUT` (replace) and `PATCH` (merge) on a node.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NodeChange {
    /// New label (unchanged when absent).
    pub label: Option<String>,
    /// `PUT`: replaces the properties. `PATCH`: JSON merge patch (RFC 7396;
    /// `null` removes a key).
    #[schema(value_type = Option<Object>)]
    pub properties: Option<Json>,
}

/// Body of `POST /edges`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewEdge {
    /// Id; generated when absent.
    pub id: Option<String>,
    /// Relationship type, 1 to 64 of `A-Z a-z 0-9 _ . : -`.
    #[serde(rename = "type")]
    pub edge_type: String,
    /// Source node id; the node must exist.
    pub source: String,
    /// Target node id; the node must exist.
    pub target: String,
    /// `false` for an undirected relationship (default `true`).
    pub directed: Option<bool>,
    /// Property object (default `{}`).
    #[schema(value_type = Option<Object>)]
    pub properties: Option<Json>,
}

/// Body of `PATCH /edges/{id}`: only properties can change.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EdgeChange {
    /// JSON merge patch (RFC 7396; `null` removes a key).
    #[schema(value_type = Option<Object>)]
    pub properties: Option<Json>,
}

/// Which way edges are followed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    /// From source to target (and across undirected edges).
    #[default]
    Out,
    /// From target to source (and across undirected edges).
    In,
    /// Either way.
    Both,
}

/// Node filter: label and property equality (all must match).
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct NodeFilter {
    /// Only nodes with this label.
    pub label: Option<String>,
    /// Property equality, e.g. `{"country": "IN"}`. Values are strings, numbers
    /// or booleans; keys are `A-Z a-z 0-9 _ -`. At most 8.
    #[schema(value_type = Option<Object>)]
    pub properties: Option<Map<String, Json>>,
}

/// Body of `POST /traverse`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TraverseQuery {
    /// Id of the start node.
    pub start: String,
    /// Direction to follow (default `out`).
    #[serde(default)]
    pub direction: Direction,
    /// Edge types to follow; empty or absent = all (at most 20).
    #[serde(default, alias = "edge")]
    #[schema(value_type = Vec<String>)]
    pub edge_types: EdgeTypes,
    /// Hops, 1 to 10 (default 3).
    #[serde(alias = "depth")]
    pub max_depth: Option<u32>,
    /// Most nodes returned, 1 to 1000 (default 100).
    pub limit: Option<u32>,
    /// Filter applied to the returned nodes (not to the walk).
    pub filter: Option<NodeFilter>,
    /// Return the start node too (depth 0). Default `false`.
    #[serde(default)]
    pub include_start: bool,
    /// Return the edges between the returned nodes (and the start node).
    /// Default `true`.
    pub include_edges: Option<bool>,
}

/// One edge type or several.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum EdgeTypes {
    /// Not given.
    #[default]
    #[serde(skip)]
    None,
    /// One type.
    One(String),
    /// Several.
    Many(Vec<String>),
}

impl EdgeTypes {
    pub(crate) fn into_vec(self) -> Vec<String> {
        match self {
            EdgeTypes::None => Vec::new(),
            EdgeTypes::One(t) => vec![t],
            EdgeTypes::Many(v) => v,
        }
    }
}

/// A node reached by a traversal.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct TraversedNode {
    /// Fewest hops from the start node.
    pub depth: u32,
    /// The node.
    pub node: Node,
}

/// Result of a traversal.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct TraverseResult {
    /// Start node id.
    pub start: String,
    /// Reached nodes, nearest first, then by id.
    pub nodes: Vec<TraversedNode>,
    /// Edges between the returned nodes (and the start node).
    pub edges: Vec<Edge>,
    /// More nodes or edges matched than were returned.
    pub truncated: bool,
}

/// Body of `POST /shortest-path`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PathQuery {
    /// Id of the first node.
    pub from: String,
    /// Id of the last node.
    pub to: String,
    /// Direction to follow (default `out`).
    #[serde(default)]
    pub direction: Direction,
    /// Edge types to follow; empty or absent = all.
    #[serde(default, alias = "edge")]
    #[schema(value_type = Vec<String>)]
    pub edge_types: EdgeTypes,
    /// Longest path in hops, 1 to 10 (default 6).
    pub max_depth: Option<u32>,
}

/// Result of a shortest-path search (unweighted: fewest hops).
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct PathResult {
    /// Whether a path within `max_depth` exists.
    pub found: bool,
    /// Number of edges on the path.
    pub length: Option<u32>,
    /// Nodes from `from` to `to`.
    pub nodes: Vec<Node>,
    /// Edges between consecutive nodes.
    pub edges: Vec<Edge>,
}

/// A neighbour of a node.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Neighbor {
    /// `out` when the queried node is the edge's source, `in` when it is the
    /// target (also for undirected edges).
    pub direction: Direction,
    /// The connecting edge.
    pub edge: Edge,
    /// The node at the other end.
    pub node: Node,
}

/// Parameters of a neighbour query.
#[derive(Debug, Clone, Default)]
pub struct NeighborQuery {
    /// Direction (default `out`).
    pub direction: Direction,
    /// Edge types; empty = all.
    pub edge_types: Vec<String>,
    /// Page size.
    pub limit: Option<u32>,
    /// Rows to skip.
    pub offset: Option<u32>,
}

/// A page of results.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Page<T> {
    /// Items.
    pub items: Vec<T>,
    /// Pass as `after` to get the next page; absent on the last page.
    pub next_after: Option<String>,
}

/// Counts for the admin explorer.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct GraphStats {
    /// Number of nodes.
    pub nodes: i64,
    /// Number of edges.
    pub edges: i64,
    /// Edge count per relationship type.
    pub edge_types: Vec<TypeCount>,
}

/// Edges of one type.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct TypeCount {
    /// Relationship type.
    #[serde(rename = "type")]
    pub edge_type: String,
    /// Number of edges.
    pub count: i64,
}
