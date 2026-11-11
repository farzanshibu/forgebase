//! FB-178 (after FB-174): tenant moves through the API. A tenant moves
//! between shards under write load, a move is cancelled, the server restarts
//! during a move, and a rebalance spreads tenants. Verified by the HTTP
//! records and, for where bytes are, by the shard clusters.
//!
//! Written against the FB-174 routes; the timings in the ADR addendum come
//! from the `FB178_MEASURE move_freeze` lines these tests record.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use forgebase_db::{Backend, Statement};
use forgebase_topology_tests::*;
use serde_json::{Value as Json, json};

struct World {
    t: Topo,
    s1: String,
    s2: String,
    tenant: String,
}

async fn world(shards: usize) -> World {
    let mut t = Topo::bare().await;
    let mut ids = Vec::new();
    for i in 1..=shards {
        ids.push(t.add_shard(&format!("s{i}")).await);
    }
    t.tenant_collection("notes").await;
    let tenant = t.new_tenant().await;
    // Placed by the policy: control takes none, ties go to the lower number.
    for i in 0..200 {
        let r = t
            .write(&tenant, "notes", json!({"body": format!("seed-{i}"), "n": i}))
            .await;
        assert_eq!(r.status, 201, "{}", r.text());
    }
    World {
        s1: ids[0].clone(),
        s2: ids.get(1).cloned().unwrap_or_default(),
        tenant,
        t,
    }
}

async fn start_move(t: &Topo, tenant: &str, to: &str) -> String {
    let r = t
        .adm(
            "POST",
            &format!("{BASE}/placements/{tenant}/move"),
            Some(json!({"to": to, "confirm": tenant})),
        )
        .await;
    assert_eq!(r.status, 202, "move: {}", r.text());
    r.json()["operation"]["id"].as_str().unwrap().to_owned()
}

async fn placement(t: &Topo, tenant: &str) -> Json {
    let r = t
        .adm("GET", &format!("{BASE}/placements?q={tenant}&limit=5"), None)
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    r.json()["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["tenant"] == tenant)
        .cloned()
        .expect("placement listed")
}

async fn rows_on(t: &Topo, shard: &str, tenant: &str) -> i64 {
    let db = t.shard(shard).database("forgebase").await;
    db.fetch_all(&Statement::raw(format!(
        "SELECT count(*) FROM notes WHERE _tenant = '{tenant}'"
    )))
    .await
    .unwrap()
    .rows[0][0]
        .as_i64()
        .unwrap()
}

#[tokio::test]
async fn a_tenant_moves_under_write_load_without_losing_a_record() {
    if !enabled() {
        return;
    }
    let w = world(2).await;
    let stop = Arc::new(AtomicBool::new(false));
    let acked = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let failures = Arc::new(std::sync::Mutex::new(Vec::<u16>::new()));
    let writer = {
        let (stop, acked, failures) = (stop.clone(), acked.clone(), failures.clone());
        let (addr, admin, tenant) = (w.t.addr, w.t.admin.clone(), w.tenant.clone());
        tokio::spawn(async move {
            let mut i = 0;
            while !stop.load(Ordering::Relaxed) {
                let key = format!("live-{i}");
                let auth = format!("Bearer {admin}");
                let body = serde_json::to_vec(&json!({"body": key, "n": i})).unwrap();
                let r = send(
                    addr,
                    "POST",
                    "/api/v1/collections/notes/records",
                    &[("Authorization", &auth), ("X-ForgeBase-Tenant", &tenant)],
                    &body,
                )
                .await;
                if r.status == 201 {
                    acked.lock().unwrap().push(key);
                } else {
                    failures.lock().unwrap().push(r.status);
                }
                i += 1;
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
    };
    tokio::time::sleep(Duration::from_millis(500)).await;
    let op = start_move(&w.t, &w.tenant, &w.s2).await;
    let done = w.t.wait_op(&op, Duration::from_secs(300)).await;
    assert_eq!(done["status"], "succeeded", "{done}");
    stop.store(true, Ordering::Relaxed);
    writer.await.unwrap();

    let acked = acked.lock().unwrap().clone();
    assert!(!acked.is_empty());
    let r = w.t.list(&w.tenant, "notes", &[]).await;
    assert_eq!(r.status, 200);
    let present: Vec<String> = r.json()["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["body"].as_str().unwrap().to_owned())
        .collect();
    for k in acked
        .iter()
        .chain((0..200).map(|i| format!("seed-{i}")).collect::<Vec<_>>().iter())
    {
        assert!(present.contains(k), "{k} survived the move");
    }
    let p = placement(&w.t, &w.tenant).await;
    assert_eq!(p["set"], w.s2.as_str(), "{p}");
    // A write blocked by the freeze waits for the flip; it never fails with anything but a retryable 409.
    for code in failures.lock().unwrap().iter() {
        assert_eq!(*code, 409, "only tenant_moving may refuse a write");
    }
    // The tenant's rows are on s2; s1 keeps its copy until the cleanup job.
    assert!(rows_on(&w.t, "s2", &w.tenant).await >= 200 + acked.len() as i64 - 1);
    let result = &done["result"];
    record(
        "move_freeze",
        &json!({"freeze_ms": result["freeze_ms"], "rows": result["rows"], "live_writes": acked.len(), "result": result}),
    );
    w.t.shutdown().await;
}

#[tokio::test]
async fn a_cancelled_move_leaves_the_tenant_where_it_was() {
    if !enabled() {
        return;
    }
    let w = world(2).await;
    let op = start_move(&w.t, &w.tenant, &w.s2).await;
    let r = w.t.adm("POST", &format!("{BASE}/operations/{op}/cancel"), None).await;
    assert!(matches!(r.status, 200 | 202 | 409), "{}", r.text());
    let done = w.t.wait_op(&op, Duration::from_secs(300)).await;
    let moved = done["status"] == "succeeded";
    let p = placement(&w.t, &w.tenant).await;
    if moved {
        // Past the point of no return (switching, cleanup) a cancel is refused.
        assert_eq!(p["set"], w.s2.as_str());
    } else {
        assert_eq!(done["status"], "cancelled", "{done}");
        assert_eq!(
            (p["set"].as_str(), p["state"].as_str()),
            (Some(w.s1.as_str()), Some("active")),
            "{p}"
        );
        assert_eq!(rows_on(&w.t, "s2", &w.tenant).await, 0, "the target was cleaned");
    }
    let r = w.t.list(&w.tenant, "notes", &[]).await;
    assert_eq!(Topo::count(&r), 200, "every record is still served");
    w.t.shutdown().await;
}

#[tokio::test]
async fn a_move_survives_a_server_restart() {
    if !enabled() {
        return;
    }
    let mut w = world(2).await;
    let op = start_move(&w.t, &w.tenant, &w.s2).await;
    // Let it get going, then restart the server in the middle.
    tokio::time::sleep(Duration::from_millis(700)).await;
    w.t.restart().await;
    let done = w.t.wait_op(&op, Duration::from_secs(300)).await;
    assert!(
        matches!(done["status"].as_str(), Some("succeeded" | "cancelled" | "dead")),
        "{done}"
    );
    let p = placement(&w.t, &w.tenant).await;
    let r = w.t.list(&w.tenant, "notes", &[]).await;
    assert_eq!(Topo::count(&r), 200, "no record lost: {p}");
    if done["status"] == "succeeded" {
        assert_eq!(p["set"], w.s2.as_str());
    } else {
        assert_eq!(
            (p["set"].as_str(), p["state"].as_str()),
            (Some(w.s1.as_str()), Some("active")),
            "{p}"
        );
    }
    w.t.shutdown().await;
}

#[tokio::test]
async fn a_rebalance_spreads_tenants_over_the_shards() {
    if !enabled() {
        return;
    }
    let mut t = Topo::bare().await;
    let s1 = t.add_shard("s1").await;
    let s2 = t.add_shard("s2").await;
    let s3 = t.add_shard("s3").await;
    // Only s1 takes new tenants while they are created, so all six land
    // there through the placement policy; then the others open up.
    t.takes_tenants(&s2, false).await;
    t.takes_tenants(&s3, false).await;
    t.tenant_collection("notes").await;
    let mut tenants = Vec::new();
    for _ in 0..6 {
        let tenant = t.new_tenant().await;
        for i in 0..40 {
            let r = t
                .write(&tenant, "notes", json!({"body": format!("r-{i}"), "n": i}))
                .await;
            assert_eq!(r.status, 201);
        }
        tenants.push(tenant);
    }
    for tenant in &tenants {
        let r = t.adm("GET", &format!("{BASE}/placements?q={tenant}"), None).await;
        assert_eq!(r.status, 200, "{}", r.text());
        assert_eq!(r.json()["items"][0]["set"], s1.as_str(), "placed on s1: {}", r.text());
    }
    t.takes_tenants(&s2, true).await;
    t.takes_tenants(&s3, true).await;
    let r = t
        .adm("POST", &format!("{BASE}/rebalance/plan"), Some(json!({"max_moves": 6})))
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let plan = r.json();
    assert!(
        !plan["moves"].as_array().unwrap().is_empty(),
        "something to move: {plan}"
    );
    assert!(plan["spread_after"].as_f64() < plan["spread_before"].as_f64(), "{plan}");
    let id = plan["id"].as_str().unwrap();
    let r = t
        .adm(
            "POST",
            &format!("{BASE}/rebalance"),
            Some(json!({"plan_id": id, "confirm": id})),
        )
        .await;
    assert_eq!(r.status, 202, "{}", r.text());
    let done = t
        .wait_op(r.json()["operation"]["id"].as_str().unwrap(), Duration::from_secs(600))
        .await;
    assert_eq!(done["status"], "succeeded", "{done}");
    let r = t.adm("GET", &format!("{BASE}/placements?limit=50"), None).await;
    let items = r.json()["items"].as_array().unwrap().clone();
    let on_s1 = items
        .iter()
        .filter(|p| p["set"] == s1.as_str() && tenants.contains(&p["tenant"].as_str().unwrap().to_owned()))
        .count();
    assert!(on_s1 < 6, "tenants left s1: {on_s1}");
    for tenant in &tenants {
        let r = t.list(tenant, "notes", &[]).await;
        assert_eq!(Topo::count(&r), 40, "tenant {tenant} intact after the rebalance");
    }
    t.shutdown().await;
}
