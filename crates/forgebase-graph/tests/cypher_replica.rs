//! openCypher on a hot standby (ADR-0029 §1.5 "Replicas", FB-163).
//!
//! 1. The formal standby test: `MATCH` and variable-length `MATCH` run on
//!    the standby as `forgebase_rls` (not a superuser, AGE preloaded per
//!    role), with the row-level security of label rules.
//! 2. Cypher reads then route to replicas (`graph.cypher`, FB-169): a
//!    lagging standby serves them, a strong request reads the primary.
//!
//! Needs a PostgreSQL engine with Apache AGE (`FORGEBASE_TEST_PG_ENGINE`);
//! skips without.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use forgebase_data::schema::Collection;
use forgebase_data::{
    AccessContext, Action, Authorizer, DataService, Decision, Predicate, Principal, ResourceKind, ResourceRef,
};
use forgebase_db::routing::testing::RoutedCluster;
use forgebase_db::routing::{Consistency, ReadAfter, ReplicaNode, ReplicaOptions, ReplicaSet, RequestScope, scope};
use forgebase_db::topology::{DbNode, DbSet, NodeKind, NodeOrigin, NodeRole, NodeState};
use forgebase_db::{ExtensionSetup, SetHandle};
use forgebase_graph::{CypherQuery, GraphError, GraphService, NewEdge, NewNode};
use serde_json::{Value as Json, json};

/// `ruled` may not see `Secret` nodes; `cy` sees everything.
#[derive(Debug)]
struct Policy;

impl Authorizer for Policy {
    fn authorize(&self, ctx: &AccessContext, action: Action, _c: Option<&Collection>) -> Decision {
        if ctx.is_privileged() {
            return Decision::Allow;
        }
        match (&ctx.principal, action) {
            (Principal::User { .. }, Action::GraphRead | Action::GraphCypher) => Decision::Allow,
            _ => Decision::Deny,
        }
    }

    fn authorize_resource(&self, ctx: &AccessContext, action: Action, r: &ResourceRef) -> Decision {
        let base = self.authorize(ctx, action, None);
        let ruled = matches!(&ctx.principal, Principal::User { id, .. } if id == "ruled");
        match (ruled, r.kind, r.name.as_str()) {
            (true, ResourceKind::GraphLabel, "Secret") => Decision::Deny,
            (true, ResourceKind::GraphLabel, "Doc") => {
                Decision::Filter(Predicate::parse("properties.team = 'a'", HashMap::new()).unwrap())
            }
            _ => base,
        }
    }
}

fn age_plugin() -> bool {
    ["FORGEBASE_TEST_PG_ENGINE", "FORGEBASE_PG_ENGINE_DIR"]
        .iter()
        .find_map(|k| std::env::var_os(k).filter(|v| !v.is_empty()))
        .map(PathBuf::from)
        .is_some_and(|e| ["so", "dylib"].iter().any(|x| e.join(format!("lib/plugins/age.{x}")).exists()))
}

struct Env {
    rc: RoutedCluster,
    replicas: ReplicaSet,
    graph: GraphService,
}

impl Env {
    fn replica_reads(&self) -> u64 {
        self.replicas.status().iter().map(|s| s.reads).sum()
    }

    async fn sync(&self) {
        self.rc.cluster.wait_caught_up().await.unwrap();
        self.replicas.refresh().await;
    }
}

/// A primary with AGE set up and a standby, both loading AGE in every
/// session; the graph service reads through a replica set over them.
async fn env() -> Option<Env> {
    if !age_plugin() {
        eprintln!("skipped: needs an engine with Apache AGE");
        return None;
    }
    let Some(rc) = RoutedCluster::start().await else {
        eprintln!("skipped: no PostgreSQL engine configured");
        return None;
    };
    rc.cluster
        .primary_sql("ALTER ROLE CURRENT_USER SET session_preload_libraries = '$libdir/plugins/age'")
        .await
        .unwrap();
    rc.cluster.wait_caught_up().await.unwrap();
    // Pools opened after the setting: every session loads AGE.
    let primary = rc.cluster.open_primary().await.unwrap();
    primary.setup_extensions(&ExtensionSetup::default()).await.unwrap();
    rc.cluster.wait_caught_up().await.unwrap();
    let standby = rc.cluster.open_standby().await.unwrap();
    let node = DbNode {
        id: "n-r1".into(),
        set_id: "control".into(),
        name: "r1".into(),
        kind: NodeKind::External,
        role: NodeRole::Replica,
        url_sealed: None,
        state: NodeState::Ready,
        weight: 1,
        engine_version: None,
        system_identifier: None,
        timeline: None,
        origin: NodeOrigin::Api,
        created_at: String::new(),
        updated_at: String::new(),
    };
    let replicas = ReplicaSet::open(
        "control",
        primary.clone(),
        vec![ReplicaNode { node, db: standby }],
        ReplicaOptions {
            health_interval: Duration::ZERO,
            lsn_wait: Duration::from_millis(20),
            ..ReplicaOptions::default()
        },
        None,
    )
    .await;
    let handle = SetHandle::new(DbSet::control(), primary).with_replicas(replicas.clone());
    let data = DataService::new(Arc::new(handle), Arc::new(Policy)).await.unwrap();
    let graph = GraphService::new(data);
    Some(Env { rc, replicas, graph })
}

fn q(query: &str) -> CypherQuery {
    CypherQuery {
        query: query.into(),
        params: None,
        limit: None,
        timeout_ms: None,
    }
}

async fn ids(g: &GraphService, ctx: &AccessContext, query: &str) -> Vec<String> {
    let mut v: Vec<String> = g
        .cypher(ctx, q(query))
        .await
        .unwrap_or_else(|e| panic!("{query}: {e}"))
        .rows
        .iter()
        .map(|r| r[0].as_str().map_or_else(|| r[0].to_string(), str::to_owned))
        .collect();
    v.sort();
    v
}

async fn node(g: &GraphService, ctx: &AccessContext, id: &str, label: &str, props: Json) {
    g.create_node(
        ctx,
        NewNode {
            id: Some(id.into()),
            label: Some(label.into()),
            properties: Some(props),
        },
    )
    .await
    .unwrap();
}

async fn edge(g: &GraphService, ctx: &AccessContext, id: &str, s: &str, t: &str) {
    g.create_edge(
        ctx,
        NewEdge {
            id: Some(id.into()),
            edge_type: "KNOWS".into(),
            source: s.into(),
            target: t.into(),
            directed: None,
            properties: None,
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn cypher_reads_run_on_the_standby_with_rules() {
    let Some(e) = env().await else { return };
    let g = &e.graph;
    let admin = AccessContext::admin("root").with_tenant("t1");
    node(g, &admin, "a", "Person", json!({})).await;
    node(g, &admin, "b", "Person", json!({})).await;
    node(g, &admin, "s", "Secret", json!({})).await;
    node(g, &admin, "d1", "Doc", json!({"team": "a"})).await;
    node(g, &admin, "d2", "Doc", json!({"team": "b"})).await;
    edge(g, &admin, "ab", "a", "b").await;
    edge(g, &admin, "bs", "b", "s").await;
    edge(g, &admin, "sd", "s", "d1").await;
    edge(g, &admin, "bd", "b", "d2").await;
    assert!(matches!(
        g.cypher(&admin, q("RETURN 1 AS x")).await,
        Err(GraphError::Materialising { .. })
    ));
    for _ in 0..600 {
        if g.stats(&admin).await.unwrap().cypher.unwrap().state == "ready" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    e.sync().await;

    // 1. The standby answers MATCH and variable-length MATCH as forgebase_rls.
    let cy = AccessContext::user("users", "cy").with_tenant("t1");
    let ruled = AccessContext::user("users", "ruled").with_tenant("t1");
    let before = e.replica_reads();
    assert_eq!(ids(g, &cy, "MATCH (n) RETURN n.__id").await, ["a", "b", "d1", "d2", "s"]);
    assert_eq!(
        ids(g, &cy, "MATCH ({__id: 'a'})-[*1..3]->(n) RETURN DISTINCT n.__id").await,
        ["b", "d1", "d2", "s"]
    );
    assert_eq!(ids(g, &ruled, "MATCH (n) RETURN n.__id").await, ["a", "b", "d1"]);
    assert_eq!(
        ids(g, &ruled, "MATCH ({__id: 'a'})-[*1..3]->(n) RETURN DISTINCT n.__id").await,
        ["b"],
        "the hidden Secret blocks the walk to d1; d2 fails its rule"
    );
    assert!(e.replica_reads() > before, "the standby served the Cypher reads");

    // 2. A lagging standby serves Cypher reads; a strong request reads the primary.
    e.rc.lag().await;
    node(g, &admin, "c", "Person", json!({})).await;
    let before = e.replica_reads();
    assert_eq!(ids(g, &cy, "MATCH (n:Person) RETURN n.__id").await, ["a", "b"]);
    assert!(e.replica_reads() > before);
    let strong = RequestScope::new(Consistency::Strong, ReadAfter::new());
    let seen = scope::scoped(strong, ids(g, &cy, "MATCH (n:Person) RETURN n.__id")).await;
    assert_eq!(seen, ["a", "b", "c"]);
    e.rc.cluster.resume_replay().await.unwrap();
    e.sync().await;
    assert_eq!(ids(g, &cy, "MATCH (n:Person) RETURN n.__id").await, ["a", "b", "c"]);
}
