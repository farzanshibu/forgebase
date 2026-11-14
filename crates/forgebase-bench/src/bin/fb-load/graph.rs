//! `graph`: traversal, shortest path, neighbours and openCypher over
//! `/api/v1/graph`. The graph (`--graph-nodes`, `--graph-edges`; 10 000 and
//! 50 000 by default, the size the Criterion bench uses) is built through the
//! API first (a ring plus random `KNOWS` edges from a fixed seed) and its
//! load rate is reported as `load_nodes` and `load_edges`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use forgebase_bench::Lcg;
use serde_json::{Value, json};

use crate::http::Conn;
use crate::stats::{Stats, emit};
use crate::{Args, run_workers};

const OPS: [&str; 7] = [
    "traverse_depth_1",
    "traverse_depth_2",
    "traverse_depth_3",
    "traverse_depth_4",
    "shortest_path",
    "neighbors_50",
    "cypher_one_hop",
];

/// Runs `make(i)` for every `i` in `0..count`, spread over `workers` tasks;
/// returns the stats and the wall time.
async fn load<F>(addr: &str, token: &str, workers: usize, count: usize, make: F) -> (Stats, f64)
where
    F: Fn(usize) -> (String, Value) + Send + Sync + 'static,
{
    let make = std::sync::Arc::new(make);
    let start = Instant::now();
    let handles: Vec<_> = (0..workers)
        .map(|w| {
            let (addr, token, make) = (addr.to_owned(), token.to_owned(), make.clone());
            tokio::spawn(async move {
                let mut c = Conn::new(&addr);
                let mut st = Stats::default();
                for i in (w..count).step_by(workers) {
                    let (path, body) = make(i);
                    let t = Instant::now();
                    let (s, _) = c.json("POST", &path, Some(&token), Some(&body)).await;
                    st.us.push(t.elapsed().as_micros() as u64);
                    if s != 201 && s != 200 {
                        st.errors += 1;
                    }
                }
                st
            })
        })
        .collect();
    let mut total = Stats::default();
    for h in handles {
        total.merge(h.await.unwrap());
    }
    (total, start.elapsed().as_secs_f64())
}

pub(crate) async fn run(a: &Args, token: &str) {
    let (nodes, edges) = (a.graph_nodes.max(10), a.graph_edges.max(a.graph_nodes));
    let mut c = Conn::new(&a.addr);
    let (s, st) = c.json("GET", "/api/v1/graph/stats", Some(token), None).await;
    assert_eq!(s, 200, "graph stats: {st}");
    if st["nodes"].as_u64().unwrap_or(0) == 0 {
        let (stats, secs) = load(&a.addr, token, a.concurrency.min(16), nodes, |i| {
            (
                "/api/v1/graph/nodes".into(),
                json!({"id": format!("n{i}"), "label": if i % 10 == 0 { "Hub" } else { "Person" }, "properties": {"n": i}}),
            )
        })
        .await;
        stats.report("graph", "load_nodes", secs);
        // The ring makes every pair reachable; the rest is random.
        let mut rng = Lcg::new(42);
        let extra: Vec<(usize, usize)> = (nodes..edges)
            .map(|_| {
                loop {
                    let (s, t) = (rng.below(nodes as u64) as usize, rng.below(nodes as u64) as usize);
                    if s != t {
                        break (s, t);
                    }
                }
            })
            .collect();
        let extra = std::sync::Arc::new(extra);
        let (stats, secs) = load(&a.addr, token, a.concurrency.min(16), edges, move |i| {
            let (s, t) = if i < nodes {
                (i, (i + 1) % nodes)
            } else {
                extra[i - nodes]
            };
            (
                "/api/v1/graph/edges".into(),
                json!({"id": format!("e{i}"), "type": "KNOWS", "source": format!("n{s}"), "target": format!("n{t}")}),
            )
        })
        .await;
        stats.report("graph", "load_edges", secs);
    }
    // The openCypher engine, and a wait for a derived (AGE) graph to be ready.
    let probe = json!({"query": "MATCH ({__id: 'n0'})-[:KNOWS]->(x) RETURN x.__id", "limit": 10});
    let mut engine = String::from("unknown");
    for _ in 0..240 {
        let (s, j) = c.json("POST", "/api/v1/graph/cypher", Some(token), Some(&probe)).await;
        if s == 200 {
            engine = j["engine"].as_str().unwrap_or("unknown").to_owned();
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    println!("graph: openCypher engine {engine}");

    let (addr, token_c) = (a.addr.clone(), token.to_owned());
    let (stats, secs) = run_workers(a.concurrency, a.duration, OPS.len(), move |w, stop: std::sync::Arc<AtomicBool>| {
        let (addr, token) = (addr.clone(), token_c.clone());
        async move {
            let mut c = Conn::new(&addr);
            let mut st: Vec<Stats> = OPS.iter().map(|_| Stats::default()).collect();
            let mut rng = Lcg::new(w as u64 + 11);
            let mut n = 0_usize;
            while !stop.load(Ordering::Relaxed) {
                n += 1;
                let op = n % OPS.len();
                let start = format!("n{}", rng.below(nodes as u64));
                let t = Instant::now();
                let (method, path, body) = match op {
                    0..=3 => (
                        "POST",
                        "/api/v1/graph/traverse".to_owned(),
                        Some(json!({"start": start, "direction": "out", "max_depth": op + 1, "limit": 1000, "include_edges": false})),
                    ),
                    4 => (
                        "POST",
                        "/api/v1/graph/shortest-path".to_owned(),
                        Some(json!({"from": start, "to": format!("n{}", rng.below(nodes as u64)), "direction": "out", "max_depth": 8})),
                    ),
                    5 => ("GET", format!("/api/v1/graph/nodes/{start}/neighbors?direction=both&limit=50"), None),
                    _ => (
                        "POST",
                        "/api/v1/graph/cypher".to_owned(),
                        Some(json!({"query": "MATCH ({__id: $id})-[:KNOWS]->(x) RETURN x.__id", "params": {"id": start}, "limit": 100})),
                    ),
                };
                let (s, _) = c.json(method, &path, Some(&token), body.as_ref()).await;
                st[op].us.push(t.elapsed().as_micros() as u64);
                if s != 200 {
                    st[op].errors += 1;
                }
            }
            st
        }
    })
    .await;
    for (s, op) in stats.into_iter().zip(OPS) {
        s.report_with("graph", op, secs, &[("nodes", json!(nodes)), ("edges", json!(edges))]);
    }
    emit("graph", "cypher_engine", &[("engine", json!(engine))]);
}
