//! openCypher on the derived Apache AGE graph (ADR-0029 §1.5, FB-163).
//!
//! Needs `FORGEBASE_TEST_BACKEND=postgres` and an engine with AGE
//! (`FORGEBASE_TEST_PG_ENGINE`, e.g. the r1 archive of FB-160); without one
//! each test prints a skip. The feature-off path is `tests/cypher_off.rs`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use forgebase_data::schema::Collection;
use forgebase_data::{
    AccessContext, Action, Authorizer, DataService, Decision, Predicate, Principal, ResourceKind, ResourceRef,
};
use forgebase_db::testing::{TestDb, selected_backend, test_backend};
use forgebase_db::{Backend, BackendKind, BoxFuture, Database, ExtensionSetup, Feature, RunAs, Statement, TxMode};
use forgebase_graph::{
    CypherQuery, CypherResult, Direction, EdgeChange, GraphError, GraphService, MaterialiseJobs, NeighborQuery, NewEdge,
    NewNode, NodeChange, TraverseQuery,
};
use serde_json::{Value as Json, json};
use tower::ServiceExt;

// ---- harness ------------------------------------------------------------------

/// Users: `cy` may read the graph and run openCypher; `reader` may only read
/// the graph; `ruled` may run openCypher under label and edge-type rules:
/// `Secret` nodes and `HIDDEN` edges are hidden, `Doc` nodes need
/// `properties.team = 'a'`, `RATED` edges need `properties.stars >= 3`.
#[derive(Debug)]
struct Policy;

fn user_of(ctx: &AccessContext) -> Option<&str> {
    match &ctx.principal {
        Principal::User { id, .. } => Some(id.as_str()),
        _ => None,
    }
}

impl Authorizer for Policy {
    fn authorize(&self, ctx: &AccessContext, action: Action, _c: Option<&Collection>) -> Decision {
        if ctx.is_privileged() {
            return Decision::Allow;
        }
        match (user_of(ctx), action) {
            (Some("cy" | "ruled"), Action::GraphRead | Action::GraphCypher) => Decision::Allow,
            (Some("reader"), Action::GraphRead) => Decision::Allow,
            _ => Decision::Deny,
        }
    }

    fn authorize_resource(&self, ctx: &AccessContext, action: Action, r: &ResourceRef) -> Decision {
        let base = self.authorize(ctx, action, None);
        if user_of(ctx) != Some("ruled") || !matches!(base, Decision::Allow) {
            return base;
        }
        let pred = |s: &str| Decision::Filter(Predicate::parse(s, HashMap::new()).unwrap());
        match (r.kind, r.name.as_str()) {
            (ResourceKind::GraphLabel, "Secret") | (ResourceKind::GraphEdgeType, "HIDDEN") => Decision::Deny,
            (ResourceKind::GraphLabel, "Doc") => pred("properties.team = 'a'"),
            (ResourceKind::GraphEdgeType, "RATED") => pred("properties.stars >= 3"),
            _ => Decision::Allow,
        }
    }
}

struct Env {
    db: Database,
    graph: GraphService,
    data: DataService,
    _test: TestDb,
}

fn age_plugin() -> bool {
    let Some(engine) = ["FORGEBASE_TEST_PG_ENGINE", "FORGEBASE_PG_ENGINE_DIR"]
        .iter()
        .find_map(|k| std::env::var_os(k).filter(|v| !v.is_empty()))
        .map(PathBuf::from)
    else {
        return false;
    };
    ["so", "dylib"]
        .iter()
        .any(|ext| engine.join("lib/plugins").join(format!("age.{ext}")).exists())
}

/// A migrated PostgreSQL database with the extensions set up and AGE
/// loaded in every session (the bundled engine preloads it per role; the
/// test engine's superuser gets the same setting), or `None` (skip).
async fn env() -> Option<Env> {
    if selected_backend() != BackendKind::Postgres || !age_plugin() {
        eprintln!("skipped: needs FORGEBASE_TEST_BACKEND=postgres and an engine with Apache AGE");
        return None;
    }
    let test = test_backend().await;
    // Once per engine (one per test binary): concurrent ALTER ROLEs collide.
    static PRELOAD: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    PRELOAD
        .get_or_init(|| async {
            let mut tx = test.database().begin(TxMode::Normal).await.unwrap();
            tx.execute(&Statement::raw(
                "ALTER ROLE CURRENT_USER SET session_preload_libraries = '$libdir/plugins/age'",
            ))
            .await
            .unwrap();
            tx.commit().await.unwrap();
        })
        .await;
    // New pools: their sessions load AGE.
    let db = Database::open(test.options()).await.unwrap();
    db.setup_extensions(&ExtensionSetup::default()).await.unwrap();
    assert!(db.feature_active(Feature::Graph), "AGE is installed but not usable");
    let data = DataService::new(Arc::new(db.clone()), Arc::new(Policy)).await.unwrap();
    let graph = GraphService::new(data.clone());
    Some(Env {
        db,
        graph,
        data,
        _test: test,
    })
}

fn admin(tenant: &str) -> AccessContext {
    AccessContext::admin("root").with_tenant(tenant)
}

fn user(id: &str, tenant: &str) -> AccessContext {
    AccessContext::user("users", id).with_tenant(tenant)
}

fn q(query: &str) -> CypherQuery {
    CypherQuery {
        query: query.into(),
        params: None,
        limit: None,
        timeout_ms: None,
    }
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

async fn edge(g: &GraphService, ctx: &AccessContext, id: &str, ty: &str, s: &str, t: &str, props: Json) {
    g.create_edge(
        ctx,
        NewEdge {
            id: Some(id.into()),
            edge_type: ty.into(),
            source: s.into(),
            target: t.into(),
            directed: None,
            properties: Some(props),
        },
    )
    .await
    .unwrap();
}

/// Waits until the tenant of `ctx` has a ready graph.
async fn wait_ready(g: &GraphService, ctx: &AccessContext) {
    for _ in 0..600 {
        let st = g.stats(ctx).await.unwrap().cypher.expect("AGE is on");
        match st.state.as_str() {
            "ready" => return,
            "failed" => panic!("materialisation failed: {:?}", st.error),
            _ => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    }
    panic!("the graph did not become ready");
}

/// The first query starts materialisation (409); waits until ready.
async fn materialise(g: &GraphService, ctx: &AccessContext) {
    match g.cypher(ctx, q("RETURN 1 AS one")).await {
        Err(GraphError::Materialising { .. }) => {}
        other => panic!("expected graph_materialising, got {other:?}"),
    }
    wait_ready(g, ctx).await;
}

async fn cy(g: &GraphService, ctx: &AccessContext, query: &str) -> CypherResult {
    g.cypher(ctx, q(query))
        .await
        .unwrap_or_else(|e| panic!("{query}: {e}"))
}

/// The first column of every row, as strings, sorted.
async fn col(g: &GraphService, ctx: &AccessContext, query: &str) -> Vec<String> {
    let mut v: Vec<String> = cy(g, ctx, query)
        .await
        .rows
        .iter()
        .map(|r| match &r[0] {
            Json::String(s) => s.clone(),
            other => other.to_string(),
        })
        .collect();
    v.sort();
    v
}

async fn count(g: &GraphService, ctx: &AccessContext, query: &str) -> i64 {
    cy(g, ctx, query).await.rows[0][0].as_i64().unwrap()
}

fn code(e: &GraphError) -> &'static str {
    use forgebase_core::ErrorCode;
    e.code()
}

// ---- tests ----------------------------------------------------------------------

#[tokio::test]
async fn materialisation_copies_the_canonical_graph_in_batches() {
    let Some(e) = env().await else { return };
    let g = &e.graph;
    let ctx = admin("t1");
    // 2 500 nodes and a ring of 2 500 edges: three node batches, three edge batches.
    let mut tx = e.db.begin(TxMode::Normal).await.unwrap();
    tx.execute(&Statement::raw(
        "INSERT INTO _forgebase_graph_nodes (tenant, id, label, properties, created_at, updated_at) \
         SELECT 't1', 'n' || i, 'N', jsonb_build_object('i', i), 'x', 'x' FROM generate_series(1, 2500) AS i",
    ))
    .await
    .unwrap();
    tx.execute(&Statement::raw(
        "INSERT INTO _forgebase_graph_edges (tenant, id, type, source, target, directed, properties, created_at, updated_at) \
         SELECT 't1', 'e' || i, 'NEXT', 'n' || i, 'n' || (i % 2500 + 1), 1, '{}', 'x', 'x' FROM generate_series(1, 2500) AS i",
    ))
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let st = g.stats(&ctx).await.unwrap().cypher.unwrap();
    assert_eq!((st.state.as_str(), st.graph.len()), ("none", 20));
    materialise(g, &ctx).await;
    let st = g.stats(&ctx).await.unwrap().cypher.unwrap();
    assert_eq!((st.nodes, st.edges, st.done), (2500, 2500, 5000));
    assert_eq!(count(g, &ctx, "MATCH (n:N) RETURN count(n)").await, 2500);
    assert_eq!(count(g, &ctx, "MATCH ()-[e:NEXT]->() RETURN count(e)").await, 2500);
    let r = cy(g, &ctx, "MATCH (a {__id: 'n7'})-[e]->(b) RETURN a, e, b.i AS i").await;
    assert_eq!(r.columns, ["a", "e", "i"]);
    assert_eq!(r.rows[0][0], json!({"id": "n7", "label": "N", "properties": {"i": 7}}));
    assert_eq!(
        r.rows[0][1],
        json!({"id": "e7", "type": "NEXT", "source": "n7", "target": "n8", "directed": true, "properties": {}})
    );
    assert_eq!(r.rows[0][2], json!(8));
    // A path.
    let p = cy(g, &ctx, "MATCH p = (a {__id: 'n1'})-[*2]->(b) RETURN p").await;
    let p = &p.rows[0][0];
    assert_eq!(p["nodes"].as_array().unwrap().len(), 3);
    assert_eq!(p["edges"][1]["id"], "e2");
}

#[tokio::test]
async fn every_write_updates_the_derived_graph_in_its_transaction() {
    let Some(e) = env().await else { return };
    let g = &e.graph;
    let ctx = admin("t1");
    node(g, &ctx, "a", "Person", json!({"name": "Ann"})).await;
    node(g, &ctx, "b", "Person", json!({"name": "Bob"})).await;
    materialise(g, &ctx).await;

    // Create.
    node(g, &ctx, "c", "Person", json!({"name": "Cid"})).await;
    assert_eq!(
        col(g, &ctx, "MATCH (n:Person) RETURN n.__id").await,
        ["a", "b", "c"]
    );
    edge(g, &ctx, "ac", "KNOWS", "a", "c", json!({"since": 2020})).await;
    assert_eq!(col(g, &ctx, "MATCH (:Person {__id: 'a'})-[:KNOWS]->(b) RETURN b.__id").await, ["c"]);
    // Change properties (merge patch) of a node and an edge.
    g.change_node(
        &ctx,
        "c",
        NodeChange {
            label: None,
            properties: Some(json!({"age": 30})),
        },
        true,
    )
    .await
    .unwrap();
    assert_eq!(
        cy(g, &ctx, "MATCH (n {__id: 'c'}) RETURN n").await.rows[0][0]["properties"],
        json!({"name": "Cid", "age": 30})
    );
    g.change_edge(
        &ctx,
        "ac",
        EdgeChange {
            properties: Some(json!({"since": 2021})),
        },
    )
    .await
    .unwrap();
    assert_eq!(count(g, &ctx, "MATCH ()-[e:KNOWS]->() RETURN e.since").await, 2021);
    // A new label moves the vertex to another table; its edges follow.
    g.change_node(
        &ctx,
        "c",
        NodeChange {
            label: Some("Robot".into()),
            properties: None,
        },
        false,
    )
    .await
    .unwrap();
    assert_eq!(col(g, &ctx, "MATCH (n:Person) RETURN n.__id").await, ["a", "b"]);
    assert_eq!(col(g, &ctx, "MATCH (:Person)-[:KNOWS]->(r:Robot) RETURN r.name").await, ["Cid"]);
    // Undirected edges carry `__undirected`.
    g.create_edge(
        &ctx,
        NewEdge {
            id: Some("bc".into()),
            edge_type: "NEAR".into(),
            source: "b".into(),
            target: "c".into(),
            directed: Some(false),
            properties: None,
        },
    )
    .await
    .unwrap();
    let r = cy(g, &ctx, "MATCH (:Robot)-[e:NEAR]-() RETURN e").await;
    assert_eq!(r.rows[0][0]["directed"], false);
    // A failed write leaves no trace (same transaction).
    assert!(
        g.create_node(
            &ctx,
            NewNode {
                id: Some("a".into()),
                label: Some("Dup".into()),
                properties: None,
            },
        )
        .await
        .is_err()
    );
    assert_eq!(count(g, &ctx, "MATCH (n:Dup) RETURN count(n)").await, 0);
    // Delete an edge, then a node with its edges.
    g.delete_edge(&ctx, "bc").await.unwrap();
    assert_eq!(count(g, &ctx, "MATCH ()-[e:NEAR]-() RETURN count(e)").await, 0);
    assert_eq!(g.delete_node(&ctx, "c").await.unwrap(), 1);
    assert_eq!(count(g, &ctx, "MATCH (n) RETURN count(n)").await, 2);
    assert_eq!(count(g, &ctx, "MATCH ()-[e]->() RETURN count(e)").await, 0);
    // The map holds exactly the remaining rows.
    let m = e
        .db
        .fetch_all(&Statement::raw(
            "SELECT kind, id FROM _forgebase_graph_age_ids WHERE tenant = 't1' ORDER BY kind, id",
        ))
        .await
        .unwrap();
    assert_eq!(m.len(), 2);
}

#[tokio::test]
async fn names_age_cannot_take_use_the_catch_all_labels() {
    let Some(e) = env().await else { return };
    let g = &e.graph;
    let ctx = admin("t1");
    node(g, &ctx, "x", "my-label", json!({})).await;
    node(g, &ctx, "y", "", json!({})).await;
    node(g, &ctx, "z", "Fine", json!({})).await;
    materialise(g, &ctx).await;
    edge(g, &ctx, "xy", "rel-x", "x", "y", json!({})).await;
    let r = cy(g, &ctx, "MATCH (n:_fbnode) RETURN n ORDER BY n.__id").await;
    let labels: Vec<&Json> = r.rows.iter().map(|r| &r[0]["label"]).collect();
    assert_eq!(labels, [&json!("my-label"), &json!("")]);
    let r = cy(g, &ctx, "MATCH ()-[e:_fbedge]->() RETURN e").await;
    assert_eq!(r.rows[0][0]["type"], "rel-x");
    assert_eq!(col(g, &ctx, "MATCH (n:_fbnode {__label: 'my-label'}) RETURN n.__id").await, ["x"]);
}

#[tokio::test]
async fn queries_are_read_only_bounded_and_checked() {
    let Some(e) = env().await else { return };
    let g = &e.graph;
    let ctx = admin("t1");
    node(g, &ctx, "a", "Person", json!({"name": "Ann"})).await;
    materialise(g, &ctx).await;

    // The tokenizer refuses writes and calls (defence in depth).
    for (query, want) in [
        ("CREATE (n:X) RETURN n", "cypher_read_only"),
        ("MATCH (n) SET n.x = 1 RETURN n", "cypher_read_only"),
        ("MATCH (n) DETACH DELETE n RETURN 1", "cypher_read_only"),
        ("RETURN pg_catalog.pg_sleep(1)", "cypher_invalid"),
        ("MATCH (n) RETURN *", "cypher_invalid"),
        ("MATCH (n RETURN n", "cypher_invalid"),
        ("MATCH (n) RETURN n; MATCH (m) RETURN m", "cypher_invalid"),
    ] {
        let err = g.cypher(&ctx, q(query)).await.unwrap_err();
        assert_eq!(code(&err), want, "{query}: {err}");
    }
    // The engine is the boundary: a write that reaches it fails in the
    // READ ONLY transaction.
    let stmt = Statement::new(
        "SELECT r FROM forgebase.graph_cypher(?1::text, ?2::text, NULL, ?3::text, 1, 10) AS r",
        vec![
            forgebase_graph::age::graph_name("t1").into(),
            "CREATE (n:X) RETURN n".into(),
            "fb0123456789abcdef".into(),
        ],
    );
    let err = e.db.fetch_all_as(&stmt, &RunAs::session("{}")).await.unwrap_err();
    assert!(matches!(err, forgebase_db::DbError::ReadOnly(_)), "{err}");
    assert_eq!(count(g, &ctx, "MATCH (n:X) RETURN count(n)").await, 0);

    // Parameters, limits, timeouts, engine errors.
    let r = g
        .cypher(
            &ctx,
            CypherQuery {
                query: "MATCH (n) WHERE n.name = $name RETURN n.__id AS id".into(),
                params: Some(json!({"name": "Ann"})),
                limit: None,
                timeout_ms: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(r.rows, [[json!("a")]]);
    let r = g
        .cypher(
            &ctx,
            CypherQuery {
                query: "UNWIND range(1, 10) AS x RETURN x".into(),
                params: None,
                limit: Some(3),
                timeout_ms: None,
            },
        )
        .await
        .unwrap();
    assert_eq!((r.rows.len(), r.truncated), (3, true));
    let slow = CypherQuery {
        query: "UNWIND range(1, 4000) AS x UNWIND range(1, 4000) AS y RETURN count(*)".into(),
        params: None,
        limit: None,
        timeout_ms: Some(20),
    };
    let err = g.cypher(&ctx, slow).await.unwrap_err();
    assert_eq!(code(&err), "cypher_timeout", "{err:?}");
    assert_eq!(
        code(&g.cypher(&ctx, q("MATCH (n) RETURN nosuchfn(n)")).await.unwrap_err()),
        "cypher_error"
    );
    for bad in [
        CypherQuery {
            limit: Some(1001),
            ..q("RETURN 1 AS x")
        },
        CypherQuery {
            timeout_ms: Some(30_001),
            ..q("RETURN 1 AS x")
        },
        CypherQuery {
            params: Some(json!([1])),
            ..q("RETURN 1 AS x")
        },
        q(""),
    ] {
        assert!(matches!(g.cypher(&ctx, bad).await, Err(GraphError::Invalid { .. })));
    }
}

#[tokio::test]
async fn cypher_needs_its_own_permission() {
    let Some(e) = env().await else { return };
    let g = &e.graph;
    node(g, &admin("t1"), "a", "Person", json!({})).await;
    materialise(g, &admin("t1")).await;
    // graph_read alone is not enough.
    assert!(matches!(
        g.cypher(&user("reader", "t1"), q("MATCH (n) RETURN n")).await,
        Err(GraphError::Forbidden(_))
    ));
    assert!(matches!(
        g.cypher(&AccessContext::anonymous().with_tenant("t1"), q("MATCH (n) RETURN n"))
            .await,
        Err(GraphError::Unauthenticated)
    ));
    assert_eq!(
        code(&g.cypher(&AccessContext::user("users", "cy"), q("RETURN 1 AS x")).await.unwrap_err()),
        "tenant_required"
    );
    assert_eq!(col(g, &user("cy", "t1"), "MATCH (n) RETURN n.__id").await, ["a"]);
    assert!(matches!(
        g.drop_cypher(&user("reader", "t1")).await,
        Err(GraphError::Forbidden(_))
    ));
}

/// Cypher under label and edge-type rules sees exactly what the REST graph
/// API shows the same caller (decision 9a.1: full rules inside Cypher).
#[tokio::test]
async fn label_rules_apply_inside_cypher_as_in_rest() {
    let Some(e) = env().await else { return };
    let g = &e.graph;
    let a = admin("t1");
    for (id, label, props) in [
        ("p1", "Person", json!({})),
        ("p2", "Person", json!({})),
        ("p3", "Person", json!({})),
        ("s1", "Secret", json!({})),
        ("d1", "Doc", json!({"team": "a"})),
        ("d2", "Doc", json!({"team": "b"})),
        ("x1", "odd-label", json!({})),
    ] {
        node(g, &a, id, label, props).await;
    }
    for (id, ty, s, t, props) in [
        ("k1", "KNOWS", "p1", "p2", json!({})),
        ("k2", "KNOWS", "p2", "s1", json!({})),
        ("k3", "KNOWS", "s1", "p3", json!({})),
        ("k4", "KNOWS", "p2", "x1", json!({})),
        ("w1", "WROTE", "p1", "d1", json!({})),
        ("w2", "WROTE", "p1", "d2", json!({})),
        ("h1", "HIDDEN", "p1", "p3", json!({})),
        ("r1", "RATED", "p2", "d1", json!({"stars": 5})),
        ("r2", "RATED", "p3", "d1", json!({"stars": 1})),
    ] {
        edge(g, &a, id, ty, s, t, props).await;
    }
    materialise(g, &a).await;
    let ruled = user("ruled", "t1");

    let rest_nodes: Vec<String> = {
        let mut v: Vec<String> = g
            .list_nodes(&ruled, None, None, Some(1000))
            .await
            .unwrap()
            .items
            .into_iter()
            .map(|n| n.id)
            .collect();
        v.sort();
        v
    };
    assert_eq!(rest_nodes, ["d1", "p1", "p2", "p3", "x1"]);
    assert_eq!(col(g, &ruled, "MATCH (n) RETURN n.__id").await, rest_nodes);
    let rest_edges: Vec<String> = {
        let mut v: Vec<String> = g
            .list_edges(&ruled, None, None, None, None, Some(1000))
            .await
            .unwrap()
            .items
            .into_iter()
            .map(|e| e.id)
            .collect();
        v.sort();
        v
    };
    assert_eq!(rest_edges, ["k1", "k4", "r1", "w1"]);
    assert_eq!(col(g, &ruled, "MATCH ()-[e]->() RETURN e.__id").await, rest_edges);
    // Traversals: the hidden node blocks the walk p2 -> s1 -> p3, the
    // hidden edge p1 -> p3 is not taken.
    let walk: TraverseQuery = serde_json::from_value(json!({"start": "p1", "max_depth": 4})).unwrap();
    let mut rest_walk: Vec<String> = g
        .traverse(&ruled, walk)
        .await
        .unwrap()
        .nodes
        .into_iter()
        .map(|n| n.node.id)
        .filter(|id| id != "p1")
        .collect();
    rest_walk.sort();
    assert_eq!(rest_walk, ["d1", "p2", "x1"]);
    assert_eq!(
        col(g, &ruled, "MATCH ({__id: 'p1'})-[*1..4]->(b) RETURN DISTINCT b.__id").await,
        rest_walk
    );
    // Neighbours, both directions.
    let nb: Vec<String> = {
        let mut v: Vec<String> = g
            .neighbors(
                &ruled,
                "d1",
                NeighborQuery {
                    direction: Direction::Both,
                    ..NeighborQuery::default()
                },
            )
            .await
            .unwrap()
            .items
            .into_iter()
            .map(|n| n.node.id)
            .collect();
        v.sort();
        v.dedup();
        v
    };
    assert_eq!(col(g, &ruled, "MATCH ({__id: 'd1'})-[]-(b) RETURN DISTINCT b.__id").await, nb);
    assert_eq!(count(g, &ruled, "MATCH (n:Secret) RETURN count(n)").await, 0);
    assert_eq!(col(g, &ruled, "MATCH (n:Doc) RETURN n.__id").await, ["d1"]);
    // An unrestricted caller sees everything.
    assert_eq!(count(g, &user("cy", "t1"), "MATCH (n) RETURN count(n)").await, 7);
    assert_eq!(count(g, &a, "MATCH ()-[e]->() RETURN count(e)").await, 9);
}

#[tokio::test]
async fn tenants_get_their_own_graphs() {
    let Some(e) = env().await else { return };
    let g = &e.graph;
    node(g, &admin("t1"), "a", "Person", json!({})).await;
    node(g, &admin("t2"), "b", "Person", json!({})).await;
    materialise(g, &admin("t1")).await;
    materialise(g, &admin("t2")).await;
    assert_eq!(col(g, &admin("t1"), "MATCH (n) RETURN n.__id").await, ["a"]);
    assert_eq!(col(g, &admin("t2"), "MATCH (n) RETURN n.__id").await, ["b"]);
    assert_ne!(
        forgebase_graph::age::graph_name("t1"),
        forgebase_graph::age::graph_name("t2")
    );
    // Writes of one tenant never reach the other's graph.
    node(g, &admin("t1"), "c", "Person", json!({})).await;
    assert_eq!(count(g, &admin("t2"), "MATCH (n) RETURN count(n)").await, 1);
}

#[tokio::test]
async fn drop_frees_the_graph_and_stale_graphs_are_rebuilt() {
    let Some(e) = env().await else { return };
    let g = &e.graph;
    let ctx = admin("t1");
    node(g, &ctx, "a", "Person", json!({})).await;
    materialise(g, &ctx).await;
    assert!(g.drop_cypher(&ctx).await.unwrap().dropped);
    assert!(!g.drop_cypher(&ctx).await.unwrap().dropped);
    assert_eq!(g.stats(&ctx).await.unwrap().cypher.unwrap().state, "none");
    // Writes while dropped are not synced (nothing to sync); the next query rebuilds.
    node(g, &ctx, "b", "Person", json!({})).await;
    materialise(g, &ctx).await;
    assert_eq!(col(g, &ctx, "MATCH (n) RETURN n.__id").await, ["a", "b"]);

    // Stale (after a move): writes stop syncing, the next query rebuilds
    // from the canonical tables.
    assert!(g.mark_cypher_stale("t1").await.unwrap());
    assert_eq!(g.stats(&ctx).await.unwrap().cypher.unwrap().state, "stale");
    node(g, &ctx, "c", "Person", json!({})).await;
    g.delete_node(&ctx, "a").await.unwrap();
    materialise(g, &ctx).await;
    assert_eq!(col(g, &ctx, "MATCH (n) RETURN n.__id").await, ["b", "c"]);
}

#[derive(Debug, Default)]
struct FakeJobs(Mutex<Vec<String>>);

impl MaterialiseJobs for FakeJobs {
    fn enqueue<'a>(&'a self, tenant: &'a str) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let mut v = self.0.lock().unwrap();
            v.push(tenant.to_owned());
            Ok(format!("job-{}", v.len()))
        })
    }
}

#[tokio::test]
async fn the_jobs_service_runs_graph_materialise() {
    let Some(e) = env().await else { return };
    let jobs = Arc::new(FakeJobs::default());
    let g = GraphService::new(e.data.clone()).with_jobs(jobs.clone());
    let ctx = admin("t1");
    node(&g, &ctx, "a", "Person", json!({})).await;
    match g.cypher(&ctx, q("RETURN 1 AS x")).await {
        Err(GraphError::Materialising { job_id }) => assert_eq!(job_id.as_deref(), Some("job-1")),
        other => panic!("{other:?}"),
    }
    // Asked again while it runs: the same job, no second one.
    match g.cypher(&ctx, q("RETURN 1 AS x")).await {
        Err(GraphError::Materialising { job_id }) => assert_eq!(job_id.as_deref(), Some("job-1")),
        other => panic!("{other:?}"),
    }
    assert_eq!(*jobs.0.lock().unwrap(), ["t1"]);
    let st = g.stats(&ctx).await.unwrap().cypher.unwrap();
    assert_eq!((st.state.as_str(), st.job_id.as_deref()), ("materialising", Some("job-1")));
    // The job body (the server registers it as `graph.materialise`).
    let report = g.materialise("t1").await.unwrap();
    assert_eq!((report.state.as_str(), report.copied), ("ready", 1));
    assert_eq!(col(&g, &ctx, "MATCH (n) RETURN n.__id").await, ["a"]);
    // A second run has nothing to do.
    assert_eq!(g.materialise("t1").await.unwrap().copied, 0);
}

#[tokio::test]
async fn the_routes_answer_409_then_rows() {
    let Some(e) = env().await else { return };
    let g = &e.graph;
    let ctx = admin("t1");
    node(g, &ctx, "a", "Person", json!({"name": "Ann"})).await;
    let app = Router::new().nest_service("/api/v1/graph", forgebase_graph::router(g.clone()));
    let call = |path: &'static str, body: Json| {
        let app = app.clone();
        let ctx = ctx.clone();
        async move {
            let mut req = Request::post(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap();
            req.extensions_mut().insert(ctx);
            let res = app.oneshot(req).await.unwrap();
            let status = res.status();
            let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
            (status, serde_json::from_slice::<Json>(&bytes).unwrap())
        }
    };
    let (s, b) = call("/api/v1/graph/cypher", json!({"query": "MATCH (n) RETURN n.name AS name"})).await;
    assert_eq!(s, StatusCode::CONFLICT, "{b}");
    assert_eq!(b["error"]["code"], "graph_materialising");
    assert!(b["error"].as_object().unwrap().contains_key("job_id"), "{b}");
    wait_ready(g, &ctx).await;
    let (s, b) = call("/api/v1/graph/cypher", json!({"query": "MATCH (n) RETURN n.name AS name"})).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b, json!({"columns": ["name"], "rows": [["Ann"]], "truncated": false}));
    let (s, b) = call("/api/v1/graph/cypher", json!({"query": "CREATE (n) RETURN n"})).await;
    assert_eq!((s, b["error"]["code"].as_str()), (StatusCode::BAD_REQUEST, Some("cypher_read_only")));
    let (s, b) = call("/api/v1/graph/cypher/drop", json!({})).await;
    assert_eq!((s, &b), (StatusCode::OK, &json!({"dropped": true})));
    let stats = g.stats(&ctx).await.unwrap();
    assert_eq!(stats.cypher.unwrap().state, "none");
}

/// The cost of rules: label tables with row-level security rebuild AGE's
/// traversal cache on every statement (FB-160 note). Prints the timings.
#[tokio::test]
async fn rules_cost_of_variable_length_traversals_is_measured() {
    let Some(e) = env().await else { return };
    let g = &e.graph;
    let a = admin("t1");
    let mut tx = e.db.begin(TxMode::Normal).await.unwrap();
    tx.execute(&Statement::raw(
        "INSERT INTO _forgebase_graph_nodes (tenant, id, label, properties, created_at, updated_at) \
         SELECT 't1', 'n' || i, CASE WHEN i % 10 = 0 THEN 'Doc' ELSE 'Person' END, \
         jsonb_build_object('team', CASE WHEN i % 20 = 0 THEN 'a' ELSE 'b' END), 'x', 'x' \
         FROM generate_series(1, 5000) AS i",
    ))
    .await
    .unwrap();
    tx.execute(&Statement::raw(
        "INSERT INTO _forgebase_graph_edges (tenant, id, type, source, target, directed, properties, created_at, updated_at) \
         SELECT 't1', 'e' || i, 'KNOWS', 'n' || i, 'n' || (i % 5000 + 1), 1, '{}', 'x', 'x' FROM generate_series(1, 5000) AS i",
    ))
    .await
    .unwrap();
    tx.commit().await.unwrap();
    materialise(g, &a).await;
    let query = "MATCH ({__id: 'n1'})-[*1..12]->(b) RETURN count(DISTINCT b)";
    let mut times = Vec::new();
    for ctx in [user("cy", "t1"), user("ruled", "t1")] {
        cy(g, &ctx, query).await;
        let started = std::time::Instant::now();
        for _ in 0..5 {
            cy(g, &ctx, query).await;
        }
        times.push(started.elapsed() / 5);
    }
    eprintln!(
        "variable-length MATCH over 5000 nodes: unrestricted {:?}, with label rules {:?} per query",
        times[0], times[1]
    );
    // With rules the walk stops at the first hidden Doc (n10 is team b).
    assert_eq!(count(g, &user("cy", "t1"), query).await, 12);
    assert_eq!(count(g, &user("ruled", "t1"), query).await, 8);
}
