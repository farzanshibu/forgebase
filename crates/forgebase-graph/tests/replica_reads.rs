//! Replica reads in the graph service (ADR-0029 §2.5, FB-169): get, list,
//! neighbours, traversal, shortest path and statistics read a lagging
//! standby when the request allows it; writes, and the rule and access
//! lookups that decide what a caller may see, read the primary.
//!
//! Needs a PostgreSQL engine (`FORGEBASE_PG_ENGINE_DIR` or
//! `FORGEBASE_TEST_PG_ENGINE`); without one each test prints a skip.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use forgebase_data::{AccessContext, DataService, DefaultAuthorizer};
use forgebase_db::routing::testing::RoutedCluster;
use forgebase_db::routing::{Consistency, ReadAfter, RequestScope, scope};
use forgebase_graph::{Direction, GraphError, GraphService, NeighborQuery, NewEdge, NewNode, PathQuery, TraverseQuery};
use serde_json::json;

struct Env {
    rc: RoutedCluster,
    graph: GraphService,
    ctx: AccessContext,
}

fn node(id: &str) -> NewNode {
    NewNode {
        id: Some(id.into()),
        label: Some("User".into()),
        properties: None,
    }
}

fn edge(id: &str, source: &str, target: &str) -> NewEdge {
    NewEdge {
        id: Some(id.into()),
        edge_type: "FOLLOWS".into(),
        source: source.into(),
        target: target.into(),
        directed: None,
        properties: None,
    }
}

/// `u1 -e1-> u2` replayed on the standby; then replay is paused and
/// `u3` with `u2 -e2-> u3` is written: the standby lacks `u3` and `e2`.
async fn env() -> Option<Env> {
    let Some(rc) = RoutedCluster::start().await else {
        eprintln!("skipped: no PostgreSQL engine configured");
        return None;
    };
    let ctx = AccessContext::admin("root");
    let data = DataService::new(rc.backend(), Arc::new(DefaultAuthorizer))
        .await
        .unwrap();
    let graph = GraphService::new(data);
    graph.create_node(&ctx, node("u1")).await.unwrap();
    graph.create_node(&ctx, node("u2")).await.unwrap();
    graph.create_edge(&ctx, edge("e1", "u1", "u2")).await.unwrap();
    rc.sync().await;
    rc.lag().await;
    graph.create_node(&ctx, node("u3")).await.unwrap();
    graph.create_edge(&ctx, edge("e2", "u2", "u3")).await.unwrap();
    Some(Env { rc, graph, ctx })
}

fn traverse(start: &str) -> TraverseQuery {
    serde_json::from_value(json!({"start": start, "max_depth": 3})).unwrap()
}

fn path(from: &str, to: &str) -> PathQuery {
    serde_json::from_value(json!({"from": from, "to": to})).unwrap()
}

#[tokio::test]
async fn graph_reads_use_a_lagging_replica_when_allowed() {
    let Some(e) = env().await else { return };
    let (g, ctx, rc) = (&e.graph, &e.ctx, &e.rc);

    // get (node and edge)
    let before = rc.replica_reads();
    assert!(g.get_node(ctx, "u2").await.is_ok());
    assert!(matches!(g.get_node(ctx, "u3").await, Err(GraphError::NodeNotFound(_))));
    assert!(g.get_edge(ctx, "e1").await.is_ok());
    assert!(matches!(g.get_edge(ctx, "e2").await, Err(GraphError::EdgeNotFound(_))));
    assert_eq!(rc.replica_reads(), before + 4);

    // list
    let before = rc.replica_reads();
    assert_eq!(g.list_nodes(ctx, None, None, None).await.unwrap().items.len(), 2);
    assert_eq!(
        g.list_edges(ctx, None, None, None, None, None)
            .await
            .unwrap()
            .items
            .len(),
        1
    );
    assert!(rc.replica_reads() >= before + 2);

    // stats
    let before = rc.replica_reads();
    let stats = g.stats(ctx).await.unwrap();
    assert_eq!((stats.nodes, stats.edges), (2, 1));
    assert!(rc.replica_reads() > before);

    // neighbours
    let before = rc.replica_reads();
    let n = g.neighbors(ctx, "u2", NeighborQuery::default()).await.unwrap();
    assert!(n.items.is_empty(), "e2 is not on the standby yet");
    assert!(rc.replica_reads() > before);

    // traversal
    let before = rc.replica_reads();
    let t = g.traverse(ctx, traverse("u1")).await.unwrap();
    assert_eq!(t.nodes.len(), 1, "u1 reaches u2 only");
    assert!(rc.replica_reads() > before);

    // shortest path: u1 to u2 is there, u1 to u3 is not (u3 is not read yet)
    let before = rc.replica_reads();
    let p = g.shortest_path(ctx, path("u1", "u2")).await.unwrap();
    assert!(p.found);
    assert!(g.shortest_path(ctx, path("u1", "u3")).await.is_err());
    assert!(rc.replica_reads() > before);
    assert_eq!(rc.fallbacks(), 0);
    let _ = Direction::Out;
}

#[tokio::test]
async fn strong_reads_and_a_requests_own_writes_see_the_primary() {
    let Some(e) = env().await else { return };
    let (g, ctx, rc) = (&e.graph, &e.ctx, &e.rc);

    let strong = RequestScope::new(Consistency::Strong, ReadAfter::new());
    let before = rc.replica_reads();
    let stats = scope::scoped(strong, g.stats(ctx)).await.unwrap();
    assert_eq!((stats.nodes, stats.edges), (3, 2));
    assert_eq!(rc.replica_reads(), before);

    let own = RequestScope::new(Consistency::Session, ReadAfter::new());
    let seen = scope::scoped(own, async {
        g.create_node(ctx, node("u4")).await.unwrap();
        g.get_node(ctx, "u4").await
    })
    .await;
    assert!(seen.is_ok(), "read-your-writes");
}

#[tokio::test]
async fn graph_writes_and_rule_lookups_never_read_a_replica() {
    let Some(e) = env().await else { return };
    let (g, ctx, rc) = (&e.graph, &e.ctx, &e.rc);
    let before = rc.replica_reads();
    // Writes read inside their transaction: u3 and e2 exist on the primary only.
    g.create_edge(ctx, edge("e3", "u3", "u1")).await.unwrap();
    g.change_node(ctx, "u3", forgebase_graph::NodeChange::default(), true)
        .await
        .unwrap();
    g.delete_edge(ctx, "e3").await.unwrap();
    g.delete_node(ctx, "u3").await.unwrap();
    assert_eq!(rc.replica_reads(), before, "writes read the primary");
    assert_eq!(rc.fallbacks(), 0);
}
