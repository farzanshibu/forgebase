//! The public calls of [`GraphService`]: each resolves the caller's
//! database set first (ADR-0029 §3.5, FB-163; a no-op without shards), then
//! runs on that set.

use forgebase_data::{AccessContext, Action, ResourceOp};

use crate::access::GraphAccess;
use crate::error::GraphError;
use crate::model::*;
use crate::service::GraphService;

type Result<T> = std::result::Result<T, GraphError>;

impl GraphService {
    /// What the caller may see (or change) of its tenant's graph for `op`
    /// (FB-139): the decision of every label and edge type, ready to compile
    /// into SQL. Public so engines that walk the graph with their own SQL
    /// (FB-147, FB-163) apply the same rules and can tell whether any rule
    /// applies ([`GraphAccess::is_unrestricted`]).
    pub async fn access(&self, ctx: &AccessContext, action: Action, op: ResourceOp) -> Result<GraphAccess> {
        self.on(ctx).await?.access_here(ctx, action, op).await
    }

    /// Creates a node. The label's `create` rule must allow it (a
    /// restricting rule must hold for the new node).
    pub async fn create_node(&self, ctx: &AccessContext, new: NewNode) -> Result<Node> {
        self.on(ctx).await?.create_node_here(ctx, new).await
    }

    /// One node (the label's `view` rule applies).
    pub async fn get_node(&self, ctx: &AccessContext, id: &str) -> Result<Node> {
        self.on(ctx).await?.get_node_here(ctx, id).await
    }

    /// Changes a node's label and/or properties (`merge`: RFC 7396 merge
    /// patch of the properties); the `update` rules of the old and the new
    /// label apply.
    pub async fn change_node(&self, ctx: &AccessContext, id: &str, change: NodeChange, merge: bool) -> Result<Node> {
        self.on(ctx).await?.change_node_here(ctx, id, change, merge).await
    }

    /// Deletes a node and every edge that touches it. Returns the number of
    /// edges removed. Needs the label's `delete` rule, and the `delete` rule
    /// of every edge that touches the node.
    pub async fn delete_node(&self, ctx: &AccessContext, id: &str) -> Result<u64> {
        self.on(ctx).await?.delete_node_here(ctx, id).await
    }

    /// Lists nodes ordered by id, optionally of one label.
    pub async fn list_nodes(
        &self,
        ctx: &AccessContext,
        label: Option<&str>,
        after: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Page<Node>> {
        self.on(ctx).await?.list_nodes_here(ctx, label, after, limit).await
    }

    /// Creates an edge between two existing nodes. The type's `create` rule
    /// must allow it.
    pub async fn create_edge(&self, ctx: &AccessContext, new: NewEdge) -> Result<Edge> {
        self.on(ctx).await?.create_edge_here(ctx, new).await
    }

    /// One edge (its type's `view` rule and both nodes visible).
    pub async fn get_edge(&self, ctx: &AccessContext, id: &str) -> Result<Edge> {
        self.on(ctx).await?.get_edge_here(ctx, id).await
    }

    /// Changes an edge's properties (the type's `update` rule applies).
    pub async fn change_edge(&self, ctx: &AccessContext, id: &str, change: EdgeChange) -> Result<Edge> {
        self.on(ctx).await?.change_edge_here(ctx, id, change).await
    }

    /// Deletes an edge. Needs the type's `delete` rule.
    pub async fn delete_edge(&self, ctx: &AccessContext, id: &str) -> Result<()> {
        self.on(ctx).await?.delete_edge_here(ctx, id).await
    }

    /// Lists edges ordered by id; every filter is optional.
    pub async fn list_edges(
        &self,
        ctx: &AccessContext,
        edge_type: Option<&str>,
        source: Option<&str>,
        target: Option<&str>,
        after: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Page<Edge>> {
        self.on(ctx)
            .await?
            .list_edges_here(ctx, edge_type, source, target, after, limit)
            .await
    }

    /// Node and edge counts the caller may see, and the state of the
    /// tenant's openCypher graph where AGE runs (FB-163).
    pub async fn stats(&self, ctx: &AccessContext) -> Result<GraphStats> {
        self.on(ctx).await?.stats_here(ctx).await
    }

    /// Nodes one hop from `id`, with the connecting edge.
    pub async fn neighbors(&self, ctx: &AccessContext, id: &str, q: NeighborQuery) -> Result<Page<Neighbor>> {
        self.on(ctx).await?.neighbors_here(ctx, id, q).await
    }

    /// Multi-hop traversal from `q.start` (recursive CTE, depth-limited,
    /// cycle-safe).
    pub async fn traverse(&self, ctx: &AccessContext, q: TraverseQuery) -> Result<TraverseResult> {
        self.on(ctx).await?.traverse_here(ctx, q).await
    }

    /// Shortest path (fewest hops, breadth-first) between two nodes.
    pub async fn shortest_path(&self, ctx: &AccessContext, q: PathQuery) -> Result<PathResult> {
        self.on(ctx).await?.shortest_path_here(ctx, q).await
    }
}
