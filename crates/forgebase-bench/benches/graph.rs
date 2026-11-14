//! Graph: traversal at depth 1 to 4, neighbours, shortest path and the
//! openCypher endpoint on a generated graph (ADR-0006, ADR-0034).
//!
//! The graph has `NODES` nodes and `EDGES` edges (10 000 and 50 000 at full
//! scale): a ring `i -> i+1` (so every pair is connected) plus pseudo-random
//! `KNOWS` edges from a fixed seed, about five out-edges per node. It is
//! bulk-loaded into the canonical tables in one transaction; everything
//! measured goes through [`GraphService`].
//!
//! openCypher: first on the portable engine (SQL over the canonical tables,
//! ADR-0034, every backend); then, on PostgreSQL with Apache AGE installed,
//! the derived AGE graph is materialised and the same queries run again as
//! `cypher_age_*`. Without AGE the second half is skipped with a message.
#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use forgebase_bench::{Lcg, Rig, scaled};
use forgebase_data::AccessContext;
use forgebase_db::{Backend, ExtensionSetup, Feature, Statement, TxMode, WriteTx};
use forgebase_graph::{CypherQuery, Direction, GraphError, GraphService, NeighborQuery, PathQuery, TraverseQuery};
use serde_json::json;

const TS: &str = "2026-01-01T00:00:00Z";

async fn edge(tx: &mut dyn WriteTx, e: usize, s: usize, t: usize) {
    tx.execute(&Statement::new(
        "INSERT INTO _forgebase_graph_edges \
         (tenant, id, type, source, target, directed, properties, created_at, updated_at) \
         VALUES ('', ?, 'KNOWS', ?, ?, 1, ?, ?, ?)",
        vec![
            format!("e{e}").into(),
            format!("n{s}").into(),
            format!("n{t}").into(),
            json!({"w": (s + t) % 5}).to_string().into(),
            TS.into(),
            TS.into(),
        ],
    ))
    .await
    .unwrap();
}

async fn load(rig: &Rig, nodes: usize, edges: usize) {
    let mut tx = rig.db.begin(TxMode::Normal).await.unwrap();
    for i in 0..nodes {
        tx.execute(&Statement::new(
            "INSERT INTO _forgebase_graph_nodes (tenant, id, label, properties, created_at, updated_at) \
             VALUES ('', ?, ?, ?, ?, ?)",
            vec![
                format!("n{i}").into(),
                (if i % 10 == 0 { "Hub" } else { "Person" }).into(),
                json!({"n": i, "name": format!("person {i}")}).to_string().into(),
                TS.into(),
                TS.into(),
            ],
        ))
        .await
        .unwrap();
    }
    let mut rng = Lcg::new(42);
    let mut e = 0_usize;
    for i in 0..nodes {
        edge(&mut *tx, e, i, (i + 1) % nodes).await;
        e += 1;
    }
    while e < edges {
        let (s, t) = (rng.below(nodes as u64) as usize, rng.below(nodes as u64) as usize);
        if s != t {
            edge(&mut *tx, e, s, t).await;
            e += 1;
        }
    }
    tx.commit().await.unwrap();
}

fn cq(query: &str, params: serde_json::Value) -> CypherQuery {
    CypherQuery {
        query: query.into(),
        params: Some(params),
        limit: Some(100),
        timeout_ms: Some(30_000),
    }
}

/// Enables Apache AGE when the engine has it and waits for the derived graph.
/// Returns whether `cypher` now answers from AGE.
async fn try_enable_age(rig: &Rig, graph: &GraphService, ctx: &AccessContext) -> bool {
    if rig.db.kind() != forgebase_db::BackendKind::Postgres {
        return false;
    }
    if let Err(e) = rig.db.setup_extensions(&ExtensionSetup::default()).await {
        eprintln!("graph bench: extension setup failed ({e}); AGE half skipped");
        return false;
    }
    if !rig.db.feature_active(Feature::Graph) {
        eprintln!("graph bench: Apache AGE is not in use on this engine; AGE half skipped");
        return false;
    }
    // The first read starts materialising and answers 409 graph_materialising.
    match graph.cypher(ctx, cq("RETURN 1 AS one", json!({}))).await {
        Ok(_) | Err(GraphError::Materialising { .. }) => {}
        Err(e) => {
            eprintln!("graph bench: AGE probe failed ({e}); AGE half skipped");
            return false;
        }
    }
    for _ in 0..1200 {
        match graph.stats(ctx).await.ok().and_then(|s| s.cypher) {
            Some(st) if st.state == "ready" => return graph.cypher_engine() == "age",
            Some(st) if st.state == "failed" => {
                eprintln!(
                    "graph bench: AGE materialisation failed ({:?}); AGE half skipped",
                    st.error
                );
                return false;
            }
            _ => tokio::time::sleep(Duration::from_millis(250)).await,
        }
    }
    eprintln!("graph bench: AGE graph not ready in time; AGE half skipped");
    false
}

fn bench(c: &mut Criterion) {
    let (nodes, edges) = (scaled(10_000, 500), scaled(50_000, 2_500));
    let rt = tokio::runtime::Runtime::new().unwrap();
    let rig = rt.block_on(Rig::open());
    rt.block_on(load(&rig, nodes, edges));
    let graph = GraphService::new(rt.block_on(rig.data()));
    let ctx = AccessContext::admin("root");
    let stats = rt.block_on(graph.stats(&ctx)).unwrap();
    assert_eq!((stats.nodes, stats.edges), (nodes as i64, edges as i64));

    let mut g = c.benchmark_group("graph");
    g.sample_size(20);
    g.throughput(Throughput::Elements(1));

    for depth in 1..=4_u32 {
        let mut i = 0_usize;
        g.bench_function(format!("traverse_depth_{depth}"), |b| {
            b.to_async(&rt).iter(|| {
                i = (i + 7919) % nodes;
                let (graph, ctx, start) = (graph.clone(), ctx.clone(), format!("n{i}"));
                async move {
                    let r = graph
                        .traverse(
                            &ctx,
                            TraverseQuery {
                                start,
                                direction: Direction::Out,
                                edge_types: Default::default(),
                                max_depth: Some(depth),
                                limit: Some(1_000),
                                filter: None,
                                include_start: false,
                                include_edges: Some(false),
                            },
                        )
                        .await
                        .unwrap();
                    assert!(!r.nodes.is_empty());
                }
            });
        });
    }
    let mut i = 0_usize;
    g.bench_function("neighbors_page_50", |b| {
        b.to_async(&rt).iter(|| {
            i = (i + 7919) % nodes;
            let (graph, ctx, id) = (graph.clone(), ctx.clone(), format!("n{i}"));
            async move {
                graph
                    .neighbors(
                        &ctx,
                        &id,
                        NeighborQuery {
                            direction: Direction::Both,
                            limit: Some(50),
                            ..NeighborQuery::default()
                        },
                    )
                    .await
                    .unwrap();
            }
        });
    });
    let mut i = 0_usize;
    g.bench_function("shortest_path_max_depth_8", |b| {
        b.to_async(&rt).iter(|| {
            i = (i + 7919) % nodes;
            let (graph, ctx) = (graph.clone(), ctx.clone());
            let (from, to) = (format!("n{i}"), format!("n{}", (i * 31 + 17) % nodes));
            async move {
                let r = graph
                    .shortest_path(
                        &ctx,
                        PathQuery {
                            from,
                            to,
                            direction: Direction::Out,
                            edge_types: Default::default(),
                            max_depth: Some(8),
                        },
                    )
                    .await
                    .unwrap();
                // Random pairs are connected within a few hops at this density.
                let _ = r.found;
            }
        });
    });

    let queries: [(&str, &str); 3] = [
        ("one_hop", "MATCH ({__id: $id})-[:KNOWS]->(x) RETURN x.__id"),
        (
            "two_hops",
            "MATCH ({__id: $id})-[:KNOWS*1..2]->(x) RETURN DISTINCT x.__id",
        ),
        ("label_count", "MATCH (n:Hub) WHERE n.n >= $min RETURN count(*) AS c"),
    ];
    for (name, q) in queries {
        let mut i = 0_usize;
        g.bench_function(format!("cypher_portable_{name}"), |b| {
            b.to_async(&rt).iter(|| {
                i = (i + 7919) % nodes;
                let (graph, ctx) = (graph.clone(), ctx.clone());
                let params = json!({"id": format!("n{i}"), "min": 0});
                async move {
                    let r = graph.cypher(&ctx, cq(q, params)).await.unwrap();
                    assert_eq!(r.engine, "portable");
                }
            });
        });
    }

    if rt.block_on(try_enable_age(&rig, &graph, &ctx)) {
        for (name, q) in queries {
            let mut i = 0_usize;
            g.bench_function(format!("cypher_age_{name}"), |b| {
                b.to_async(&rt).iter(|| {
                    i = (i + 7919) % nodes;
                    let (graph, ctx) = (graph.clone(), ctx.clone());
                    let params = json!({"id": format!("n{i}"), "min": 0});
                    async move {
                        let r = graph.cypher(&ctx, cq(q, params)).await.unwrap();
                        assert_eq!(r.engine, "age");
                    }
                });
            });
        }
    }
    g.finish();
    drop(graph);
    rt.block_on(rig.close());
}

criterion_group!(benches, bench);
criterion_main!(benches);
