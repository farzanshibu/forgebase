//! Smoke: the harness builds the topology.
use forgebase_topology_tests::*;

#[tokio::test]
async fn smoke() {
    if !enabled() {
        return;
    }
    let t = Topo::start(2, 3).await;
    let topo = t.topology().await;
    eprintln!("{topo}");
    t.shutdown().await;
}
