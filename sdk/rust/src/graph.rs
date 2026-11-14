//! Property graph (`/api/v1/graph`): nodes, typed edges, neighbours, traversal, shortest path and
//! read-only openCypher. `client.graph()`.
//!
//! `properties` are JSON objects (at most 64 KiB). openCypher runs on Apache AGE or, where AGE is
//! not in use, on the portable engine (ADR-0034); [`CypherResult::engine`] says which answered.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::Serialize;
use serde_json::{Value, json};

use crate::error::Result;
use crate::generated::{
    CypherDropped, CypherResult, Direction, Edge, EdgePage, GraphStats, NeighborPage, Node, NodeDeleted, NodePage,
    PathResult, TraverseResult,
};
use crate::http::{Req, Transport, seg, wire_str};

const G: &str = "/api/v1/graph";

/// A node to create.
#[derive(Debug, Clone, Default, Serialize)]
pub struct NewNode {
    /// 1 to 128 of `A-Z a-z 0-9 _ . : @ -`; generated when absent. An existing id is a conflict.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Kind, up to 64 characters (`User`, `Product`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Properties.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<Value>,
}

impl NewNode {
    /// A node of kind `label` with `properties`.
    #[must_use]
    pub fn new(label: impl Into<String>, properties: Value) -> Self {
        Self {
            id: None,
            label: Some(label.into()),
            properties: Some(properties),
        }
    }

    /// Sets the id.
    #[must_use]
    pub fn id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }
}

/// A change to a node ([`GraphNodesApi::replace`] / [`GraphNodesApi::patch`]).
#[derive(Debug, Clone, Default, Serialize)]
pub struct NodeChange {
    /// New label (unchanged when absent).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// `replace`: the new properties. `patch`: a JSON merge patch (RFC 7396): `null` removes a key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<Value>,
}

/// Options of [`GraphNodesApi::list`].
#[derive(Debug, Clone, Default)]
pub struct ListNodes {
    /// Only nodes with this label.
    pub label: Option<String>,
    /// Id of the last node of the previous page.
    pub after: Option<String>,
    /// 1 to 1000 (default 100).
    pub limit: Option<u32>,
}

/// Options of [`GraphNodesApi::neighbors`].
#[derive(Debug, Clone, Default)]
pub struct NeighborsOptions {
    /// `out` (default), `in` or `both`.
    pub direction: Option<Direction>,
    /// Follow only these edge types.
    pub types: Vec<String>,
    /// 1 to 1000 (default 100).
    pub limit: Option<u32>,
    /// Rows to skip: pass the previous page's `next_after`.
    pub offset: Option<u32>,
}

/// An edge to create.
#[derive(Debug, Clone, Serialize)]
pub struct NewEdge {
    /// Generated when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Relationship type, 1 to 64 of `A-Z a-z 0-9 _ . : -`.
    #[serde(rename = "type")]
    pub edge_type: String,
    /// Source node id; the node must exist.
    pub source: String,
    /// Target node id; the node must exist.
    pub target: String,
    /// `false` for an undirected relationship (default `true`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub directed: Option<bool>,
    /// Properties.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<Value>,
}

impl NewEdge {
    /// A directed edge `source -[edge_type]-> target`.
    #[must_use]
    pub fn new(edge_type: impl Into<String>, source: impl Into<String>, target: impl Into<String>) -> Self {
        Self {
            id: None,
            edge_type: edge_type.into(),
            source: source.into(),
            target: target.into(),
            directed: None,
            properties: None,
        }
    }

    /// Sets the properties.
    #[must_use]
    pub fn properties(mut self, p: Value) -> Self {
        self.properties = Some(p);
        self
    }
}

/// Options of [`GraphEdgesApi::list`].
#[derive(Debug, Clone, Default)]
pub struct ListEdges {
    /// Relationship type.
    pub edge_type: Option<String>,
    /// Source node.
    pub source: Option<String>,
    /// Target node.
    pub target: Option<String>,
    /// Id of the last edge of the previous page.
    pub after: Option<String>,
    /// 1 to 1000 (default 100).
    pub limit: Option<u32>,
}

/// Filter on the nodes a traversal returns (not on the walk): label and property equality, all must match.
#[derive(Debug, Clone, Default, Serialize)]
pub struct NodeFilter {
    /// Label.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Up to 8 equality conditions; values are strings, numbers or booleans.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub properties: BTreeMap<String, Value>,
}

/// Options of [`GraphApi::traverse`].
#[derive(Debug, Clone, Default, Serialize)]
pub struct TraverseOptions {
    /// Id of the start node.
    pub start: String,
    /// `out` (default), `in` or `both`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direction: Option<Direction>,
    /// Follow only these edge types (at most 20); empty = all.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub edge_types: Vec<String>,
    /// Hops, 1 to 10 (default 3).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<u32>,
    /// Most nodes returned, 1 to 1000 (default 100).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Filter on the returned nodes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter: Option<NodeFilter>,
    /// Return the start node too, at depth 0 (default `false`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_start: Option<bool>,
    /// Return the edges between the returned nodes (default `true`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_edges: Option<bool>,
}

impl TraverseOptions {
    /// Traverses from `start` with server defaults.
    #[must_use]
    pub fn new(start: impl Into<String>) -> Self {
        Self {
            start: start.into(),
            ..Self::default()
        }
    }

    /// Hops to walk.
    #[must_use]
    pub fn max_depth(mut self, hops: u32) -> Self {
        self.max_depth = Some(hops);
        self
    }

    /// Edge direction.
    #[must_use]
    pub fn direction(mut self, d: Direction) -> Self {
        self.direction = Some(d);
        self
    }
}

/// Options of [`GraphApi::shortest_path`].
#[derive(Debug, Clone, Serialize)]
pub struct ShortestPathOptions {
    /// Start node.
    pub from: String,
    /// End node.
    pub to: String,
    /// `out` (default), `in` or `both`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direction: Option<Direction>,
    /// Follow only these edge types.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub edge_types: Vec<String>,
    /// Longest path in hops, 1 to 10 (default 6).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<u32>,
}

impl ShortestPathOptions {
    /// The shortest path `from` to `to`.
    #[must_use]
    pub fn new(from: impl Into<String>, to: impl Into<String>) -> Self {
        Self {
            from: from.into(),
            to: to.into(),
            direction: None,
            edge_types: Vec::new(),
            max_depth: None,
        }
    }
}

/// Options of [`GraphApi::cypher`].
#[derive(Debug, Clone, Default)]
pub struct CypherOptions {
    /// Values of the `$name` parameters of the query.
    pub params: Option<Value>,
    /// Most rows returned, 1 to 1000 (default 100).
    pub limit: Option<u32>,
    /// Statement timeout, 1 to 30 000 ms (default 10 000).
    pub timeout_ms: Option<u32>,
}

/// Graph helpers: `client.graph()`.
#[derive(Clone)]
pub struct GraphApi {
    pub(crate) t: Arc<Transport>,
}

impl GraphApi {
    /// Nodes.
    #[must_use]
    pub fn nodes(&self) -> GraphNodesApi {
        GraphNodesApi { t: self.t.clone() }
    }

    /// Edges.
    #[must_use]
    pub fn edges(&self) -> GraphEdgesApi {
        GraphEdgesApi { t: self.t.clone() }
    }

    /// Multi-hop traversal: the nodes reachable from `start` within `max_depth` hops (at most 10)
    /// with their fewest-hops depth, and the edges between them. Cycles are safe.
    pub async fn traverse(&self, o: &TraverseOptions) -> Result<TraverseResult> {
        self.t
            .json(Req::post(format!("{G}/traverse")).json(o)?.idempotent())
            .await
    }

    /// Shortest path by fewest hops (unweighted, breadth-first). `found: false` when no path exists within `max_depth`.
    pub async fn shortest_path(&self, o: &ShortestPathOptions) -> Result<PathResult> {
        self.t
            .json(Req::post(format!("{G}/shortest-path")).json(o)?.idempotent())
            .await
    }

    /// Node and edge counts, edges per type, and the state of the openCypher graph.
    pub async fn stats(&self) -> Result<GraphStats> {
        self.t.json(Req::get(format!("{G}/stats"))).await
    }

    /// Read-only openCypher over the tenant's graph. One query with a final `RETURN` (no
    /// `RETURN *`); parameters are `$name`. Needs `engine:graph_cypher`.
    ///
    /// On Apache AGE the first query of a tenant builds the derived graph: that call fails with a
    /// conflict whose code is `graph_materialising`; retry after it finishes
    /// (`stats().cypher.state == "ready"`). The portable engine reads the canonical tables and
    /// never waits. [`CypherResult::engine`] is `age` or `portable`.
    pub async fn cypher(&self, query: &str, o: &CypherOptions) -> Result<CypherResult> {
        let body = json!({"query": query, "params": o.params, "limit": o.limit, "timeout_ms": o.timeout_ms});
        self.t
            .json(Req::post(format!("{G}/cypher")).json_value(body).idempotent())
            .await
    }

    /// Drops the derived openCypher graph (rebuilt on next use; the REST graph is untouched).
    pub async fn drop_cypher(&self) -> Result<CypherDropped> {
        self.t
            .json(Req::post(format!("{G}/cypher/drop")).json_raw(json!({})))
            .await
    }
}

/// Nodes: `client.graph().nodes()`.
#[derive(Clone)]
pub struct GraphNodesApi {
    t: Arc<Transport>,
}

impl GraphNodesApi {
    /// Creates a node.
    pub async fn create(&self, input: &NewNode) -> Result<Node> {
        self.t.json(Req::post(format!("{G}/nodes")).json(input)?).await
    }

    /// One page of nodes by id, optionally of one label.
    pub async fn list(&self, o: &ListNodes) -> Result<NodePage> {
        self.t
            .json(
                Req::get(format!("{G}/nodes"))
                    .q_opt("label", o.label.as_deref())
                    .q_opt("after", o.after.as_deref())
                    .q_opt("limit", o.limit),
            )
            .await
    }

    /// Every node matching `o`, page after page, collected.
    pub async fn list_all(&self, o: &ListNodes) -> Result<Vec<Node>> {
        let mut o = o.clone();
        let mut out = Vec::new();
        loop {
            let page = self.list(&o).await?;
            out.extend(page.items);
            match page.next_after {
                Some(a) if !a.is_empty() => o.after = Some(a),
                _ => return Ok(out),
            }
        }
    }

    /// One node. A missing id is a not-found error.
    pub async fn get(&self, id: &str) -> Result<Node> {
        self.t.json(Req::get(format!("{G}/nodes/{}", seg(id)))).await
    }

    /// `PUT`: replaces the properties (and the label when given).
    pub async fn replace(&self, id: &str, change: &NodeChange) -> Result<Node> {
        self.t
            .json(Req::put(format!("{G}/nodes/{}", seg(id))).json(change)?)
            .await
    }

    /// `PATCH`: merges the properties (RFC 7396: `null` removes a key).
    pub async fn patch(&self, id: &str, change: &NodeChange) -> Result<Node> {
        self.t
            .json(Req::patch(format!("{G}/nodes/{}", seg(id))).json(change)?)
            .await
    }

    /// Deletes a node and every edge that touches it.
    pub async fn delete(&self, id: &str) -> Result<NodeDeleted> {
        self.t.json(Req::delete(format!("{G}/nodes/{}", seg(id)))).await
    }

    /// Nodes one hop away with the connecting edge. `next_after` is the next `offset`.
    pub async fn neighbors(&self, id: &str, o: &NeighborsOptions) -> Result<NeighborPage> {
        let types = (!o.types.is_empty()).then(|| o.types.join(","));
        self.t
            .json(
                Req::get(format!("{G}/nodes/{}/neighbors", seg(id)))
                    .q_opt("direction", o.direction.as_ref().and_then(wire_str))
                    .q_opt("type", types)
                    .q_opt("limit", o.limit)
                    .q_opt("offset", o.offset),
            )
            .await
    }
}

/// Edges: `client.graph().edges()`.
#[derive(Clone)]
pub struct GraphEdgesApi {
    t: Arc<Transport>,
}

impl GraphEdgesApi {
    /// Creates an edge between two existing nodes (a missing endpoint is a not-found error).
    pub async fn create(&self, input: &NewEdge) -> Result<Edge> {
        self.t.json(Req::post(format!("{G}/edges")).json(input)?).await
    }

    /// One page of edges by id, filtered by type and endpoints.
    pub async fn list(&self, o: &ListEdges) -> Result<EdgePage> {
        self.t
            .json(
                Req::get(format!("{G}/edges"))
                    .q_opt("type", o.edge_type.as_deref())
                    .q_opt("source", o.source.as_deref())
                    .q_opt("target", o.target.as_deref())
                    .q_opt("after", o.after.as_deref())
                    .q_opt("limit", o.limit),
            )
            .await
    }

    /// One edge.
    pub async fn get(&self, id: &str) -> Result<Edge> {
        self.t.json(Req::get(format!("{G}/edges/{}", seg(id)))).await
    }

    /// Merges properties (RFC 7396: `null` removes a key). Type and endpoints cannot change.
    pub async fn patch(&self, id: &str, properties: &Value) -> Result<Edge> {
        self.t
            .json(Req::patch(format!("{G}/edges/{}", seg(id))).json_raw(json!({"properties": properties})))
            .await
    }

    /// Deletes an edge.
    pub async fn delete(&self, id: &str) -> Result<()> {
        self.t.unit(Req::delete(format!("{G}/edges/{}", seg(id)))).await
    }
}
