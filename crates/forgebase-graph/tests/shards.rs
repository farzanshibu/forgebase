//! Per-tenant set resolution in the graph service (ADR-0029 §3.5, FB-163):
//! with a router attached, every call runs on its tenant's database set.
//!
//! Needs a PostgreSQL engine (`FORGEBASE_TEST_PG_ENGINE`); skips without.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::Arc;

use forgebase_data::{AccessContext, DataService, DefaultAuthorizer};
use forgebase_db::topology::{self, DbNode, DbSet, NewNode, NewSet, NodeKind, NodeOrigin, NodeRole, NodeState, PlacementState, SetState};
use forgebase_db::{Backend, BoxFuture, Database, DbError, DbRouter, RouterOptions, SetConnector, Statement, testing};
use forgebase_graph::{GraphService, NewEdge, NewNode as GraphNode};

#[derive(Debug)]
struct Connector(HashMap<String, Database>);

impl SetConnector for Connector {
    fn connect<'a>(&'a self, _set: &'a DbSet, primary: &'a DbNode) -> BoxFuture<'a, Result<Database, DbError>> {
        Box::pin(async move {
            self.0
                .get(&primary.name)
                .cloned()
                .ok_or_else(|| DbError::Unavailable(format!("no database for {}", primary.name)))
        })
    }
}

async fn count(db: &Database, table: &str, tenant: &str) -> i64 {
    db.fetch_all(&Statement::new(
        format!("SELECT count(*) FROM {table} WHERE tenant = ?1"),
        vec![tenant.into()],
    ))
    .await
    .unwrap()
    .scalar()
    .and_then(forgebase_db::Value::as_i64)
    .unwrap()
}

#[tokio::test]
async fn each_call_runs_on_its_tenants_set() {
    let Some(control) = testing::postgres_test_db().await else {
        eprintln!("skipped: no PostgreSQL engine configured");
        return;
    };
    let shard = testing::postgres_test_db().await.unwrap();
    let (control_db, shard_db) = (control.database().clone(), shard.database().clone());
    let set = topology::insert_set(
        &control_db,
        &NewSet {
            name: "s1".into(),
            weight: 1,
        },
    )
    .await
    .unwrap();
    let node = topology::insert_node(
        &control_db,
        &NewNode {
            set_id: set.id.clone(),
            name: "s1-primary".into(),
            kind: NodeKind::External,
            role: NodeRole::Primary,
            url_sealed: Some("sealed".into()),
            state: NodeState::Ready,
            weight: 1,
            origin: NodeOrigin::Api,
        },
    )
    .await
    .unwrap();
    topology::update_set(&control_db, &set.id, Some(SetState::Active), None, None)
        .await
        .unwrap();
    let router = DbRouter::open(
        control_db.clone(),
        RouterOptions {
            connector: Some(Arc::new(Connector(HashMap::from([(node.name.clone(), shard_db.clone())])))),
            ..RouterOptions::default()
        },
    )
    .await
    .unwrap();
    assert!(router.is_sharded());
    router.place("env-a", &set.id, PlacementState::Active).await.unwrap();

    let data = DataService::new(Arc::new(control_db.clone()), Arc::new(DefaultAuthorizer))
        .await
        .unwrap();
    let graph = GraphService::new(data);
    assert!(graph.attach_router(router.clone()));
    assert!(!graph.attach_router(router));

    for tenant in ["env-a", "env-b"] {
        let ctx = AccessContext::admin("root").with_tenant(tenant);
        for id in ["x", "y"] {
            graph
                .create_node(
                    &ctx,
                    GraphNode {
                        id: Some(id.into()),
                        label: Some("Person".into()),
                        properties: None,
                    },
                )
                .await
                .unwrap();
        }
        graph
            .create_edge(
                &ctx,
                NewEdge {
                    id: Some("xy".into()),
                    edge_type: "KNOWS".into(),
                    source: "x".into(),
                    target: "y".into(),
                    directed: None,
                    properties: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(graph.get_node(&ctx, "y").await.unwrap().id, "y");
        assert_eq!(graph.stats(&ctx).await.unwrap().nodes, 2);
        let walk = serde_json::from_value(serde_json::json!({"start": "x", "max_depth": 2})).unwrap();
        assert_eq!(graph.traverse(&ctx, walk).await.unwrap().nodes.len(), 1, "y (the start is not listed)");
    }
    // env-a lives on the shard, env-b on control.
    assert_eq!(count(&shard_db, "_forgebase_graph_nodes", "env-a").await, 2);
    assert_eq!(count(&shard_db, "_forgebase_graph_edges", "env-a").await, 1);
    assert_eq!(count(&control_db, "_forgebase_graph_nodes", "env-a").await, 0);
    assert_eq!(count(&control_db, "_forgebase_graph_nodes", "env-b").await, 2);
    assert_eq!(count(&shard_db, "_forgebase_graph_nodes", "env-b").await, 0);
    // Deleting on the shard.
    let ctx = AccessContext::admin("root").with_tenant("env-a");
    assert_eq!(graph.delete_node(&ctx, "x").await.unwrap(), 1);
    assert_eq!(count(&shard_db, "_forgebase_graph_edges", "env-a").await, 0);
}
