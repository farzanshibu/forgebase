//! FB-178 chaos: kill a replica, promote under writes, two-phase recovery
//! after a crash. Black box over HTTP; process ids, sockets and prepared
//! transactions are only looked at to cause the failure and to verify.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use forgebase_db::{Backend, ReadIntent, Statement, TxMode};
use forgebase_topology_tests::*;
use serde_json::json;

async fn settings_collection(t: &Topo) {
    let r = t
        .adm(
            "POST",
            "/api/v1/collections",
            Some(json!({"name": "events", "fields": [{"name": "k", "type": "text"}]})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.text());
}

async fn put(t: &Topo, k: &str) -> bool {
    t.adm("POST", "/api/v1/collections/events/records", Some(json!({"k": k})))
        .await
        .status
        == 201
}

async fn keys(t: &Topo) -> Vec<String> {
    let r = t.adm("GET", "/api/v1/collections/events/records?perPage=500", None).await;
    assert_eq!(r.status, 200, "{}", r.text());
    r.json()["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["k"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn a_killed_replica_fails_reads_over_to_the_primary_and_comes_back() {
    if !enabled() {
        return;
    }
    let mut t = Topo::start(2, 0).await;
    settings_collection(&t).await;
    for i in 0..20 {
        assert!(put(&t, &format!("before-{i}")).await);
    }
    // The routers of this server: reads on replicas are verified in process
    // (`pg_is_in_recovery`), the HTTP reads are only checked for correctness.
    let replicas = t.server().router().control().replicas().cloned().expect("replicas");
    let on_standby = async |rs: &forgebase_db::ReplicaSet| {
        let rows = rs
            .fetch(
                &Statement::raw("SELECT CAST(pg_is_in_recovery() AS INT), (SELECT count(*) FROM events)"),
                &ReadIntent::replica("fb178"),
            )
            .await
            .unwrap();
        (rows.rows[0][0].as_i64().unwrap(), rows.rows[0][1].as_i64().unwrap())
    };

    // A reader runs through the whole failure: no read may fail.
    let stop = Arc::new(AtomicBool::new(false));
    let reads = Arc::new(AtomicUsize::new(0));
    let failed = Arc::new(AtomicUsize::new(0));
    let reader = {
        let (stop, reads, failed) = (stop.clone(), reads.clone(), failed.clone());
        let (addr, admin) = (t.addr, t.admin.clone());
        tokio::spawn(async move {
            while !stop.load(Ordering::Relaxed) {
                let auth = format!("Bearer {admin}");
                let r = send(
                    addr,
                    "GET",
                    "/api/v1/collections/events/records?perPage=500",
                    &[("Authorization", &auth)],
                    &[],
                )
                .await;
                reads.fetch_add(1, Ordering::Relaxed);
                if r.status != 200 {
                    failed.fetch_add(1, Ordering::Relaxed);
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
    };

    let r1 = t.node("r1").await.unwrap();
    assert_eq!(r1["state"], "streaming");
    let data = t.node_data("r1");
    let old_pid = postmaster_pid(&data).expect("r1 runs");
    kill9(old_pid);

    // In-process reads never fail either: the dead replica's reads retry on
    // the primary (or the other replica) and it leaves the rotation.
    let start = Instant::now();
    let mut left = false;
    while start.elapsed() < Duration::from_secs(60) {
        let (_, n) = on_standby(&replicas).await;
        assert!(n >= 20, "reads keep answering during the failure: {n}");
        if t.node("r1").await.is_some_and(|n| n["in_rotation"] != true) {
            left = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(left, "r1 left the read rotation: {}", t.topology().await);
    // Writes go on while it is gone.
    for i in 0..10 {
        assert!(put(&t, &format!("during-{i}")).await);
    }
    assert_eq!(keys(&t).await.len(), 30);

    // The supervisor starts r1 again; it catches up and returns to rotation.
    t.wait_in_rotation("r1", Duration::from_secs(180)).await;
    let new_pid = postmaster_pid(&data).expect("r1 runs again");
    assert_ne!(new_pid, old_pid, "r1 is a new process");
    for i in 0..10 {
        assert!(put(&t, &format!("after-{i}")).await);
    }
    // Wait until r1 serves reads that include every write (a token of the
    // last commit), then no read through the router is behind.
    until("r1 has replayed everything", Duration::from_secs(60), async || {
        let n = t
            .server()
            .router()
            .control()
            .database()
            .fetch_all(&Statement::raw("SELECT count(*) FROM events"))
            .await
            .unwrap()
            .rows[0][0]
            .as_i64()
            .unwrap();
        let lsn = t.server().router().control().database().current_wal_lsn().await.unwrap();
        let st = replicas.status();
        n == 40 && lsn.is_some_and(|l| st.iter().any(|s| s.name == "r1" && s.replay_lsn.is_some_and(|r| r >= l)))
    })
    .await;
    stop.store(true, Ordering::Relaxed);
    reader.await.unwrap();
    assert!(reads.load(Ordering::Relaxed) > 20, "the reader ran");
    assert_eq!(failed.load(Ordering::Relaxed), 0, "no HTTP read failed while r1 was killed");
    let mut standby_reads = 0;
    for _ in 0..20 {
        let (in_recovery, n) = on_standby(&replicas).await;
        if in_recovery == 1 {
            standby_reads += 1;
            assert_eq!(n, 40, "a standby read sees every write");
        }
    }
    assert!(standby_reads > 0, "replicas serve reads again");
    let r = t.adm("GET", &format!("{BASE}/nodes/{}", r1["id"].as_str().unwrap()), None).await;
    assert_eq!(r.status, 200, "{}", r.text());
    t.shutdown().await;
}

#[tokio::test]
async fn a_promoted_replica_keeps_every_committed_write_and_the_old_primary_rejoins() {
    if !enabled() {
        return;
    }
    let mut t = Topo::start(2, 0).await;
    settings_collection(&t).await;
    for i in 0..25 {
        assert!(put(&t, &format!("pre-{i}")).await);
    }

    // A writer keeps acknowledging writes while the promote runs.
    let stop = Arc::new(AtomicBool::new(false));
    let acked = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let writer = {
        let (stop, acked) = (stop.clone(), acked.clone());
        let (addr, admin) = (t.addr, t.admin.clone());
        tokio::spawn(async move {
            let mut i = 0;
            while !stop.load(Ordering::Relaxed) {
                let key = format!("live-{i}");
                let auth = format!("Bearer {admin}");
                let body = serde_json::to_vec(&json!({"k": key})).unwrap();
                let r = send(
                    addr,
                    "POST",
                    "/api/v1/collections/events/records",
                    &[("Authorization", &auth)],
                    &body,
                )
                .await;
                if r.status == 201 {
                    acked.lock().unwrap().push(key);
                }
                i += 1;
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
    };
    tokio::time::sleep(Duration::from_secs(1)).await;

    let r1 = t.node("r1").await.unwrap();
    let r1_id = r1["id"].as_str().unwrap().to_owned();
    let chk = t.adm("GET", &format!("{BASE}/nodes/{r1_id}/promote-check"), None).await;
    assert_eq!(chk.status, 200, "{}", chk.text());
    let r = t
        .adm(
            "POST",
            &format!("{BASE}/nodes/{r1_id}/promote"),
            Some(json!({"confirm": "r1"})),
        )
        .await;
    assert_eq!(r.status, 202, "{}", r.text());
    let promote_op = r.json()["operation"]["id"].as_str().unwrap().to_owned();
    let file = t.cfg.data_dir.join(forgebase_db::topology::TOPOLOGY_FILE);
    until("r1 is the primary in topology.json", Duration::from_secs(180), async || {
        forgebase_db::topology::read_topology_file(&file)
            .ok()
            .flatten()
            .is_some_and(|f| {
                f.topology
                    .nodes
                    .iter()
                    .any(|n| n.name == "r1" && n.role == forgebase_db::topology::NodeRole::Primary)
            })
    })
    .await;
    stop.store(true, Ordering::Relaxed);
    writer.await.unwrap();
    let acked: Vec<String> = acked.lock().unwrap().clone();
    assert!(!acked.is_empty(), "the writer got acknowledgements");

    // Restart onto the new primary; the promote finishes there.
    t.restart().await;
    assert!(t.primary_pgdata().ends_with("postgres/nodes/r1/data"), "r1 is the primary now");
    let present = keys(&t).await;
    for k in (0..25).map(|i| format!("pre-{i}")).chain(acked.iter().cloned()) {
        assert!(present.contains(&k), "committed write {k} survived the promotion");
    }
    let done = t.wait_op(&promote_op, Duration::from_secs(240)).await;
    assert_eq!(done["status"], "succeeded", "{done}");
    assert!(put(&t, "after-promote").await, "the new primary takes writes");

    // The old primary shows as needing a rebuild, and /rebuild rejoins it.
    let old = t.node("primary").await.expect("the old primary is a node now");
    assert_eq!(old["state"], "needs_rebuild", "{old}");
    let r = t
        .adm("POST", &format!("{BASE}/nodes/{}/rebuild", old["id"].as_str().unwrap()), None)
        .await;
    assert_eq!(r.status, 202, "{}", r.text());
    let done = t.wait_op(r.json()["operation"]["id"].as_str().unwrap(), Duration::from_secs(300)).await;
    assert_eq!(done["status"], "succeeded", "{done}");
    until("the old primary streams from r1", Duration::from_secs(120), async || {
        t.node("primary")
            .await
            .is_some_and(|n| matches!(n["state"].as_str(), Some("streaming" | "catching_up")))
    })
    .await;
    // It has the post-promotion write, too.
    assert!(put(&t, "after-rejoin").await);
    let old_data = t.node_data("primary");
    until("the rejoined node replays the new writes", Duration::from_secs(120), async || {
        psql(&old_data, "forgebase", "SELECT count(*) FROM events WHERE k = 'after-rejoin'").is_ok_and(|s| s == "1")
    })
    .await;
    t.shutdown().await;
}

#[tokio::test]
async fn prepared_transactions_resolve_after_a_crash_in_the_middle_of_a_cross_shard_commit() {
    if !enabled() {
        return;
    }
    use forgebase_db::twophase::{node_gid, participants};
    let mut t = Topo::bare().await;
    t.add_shard("s1").await;
    t.add_shard("s2").await;
    let router = t.server().router().clone();
    let parts = participants(&router);
    assert_eq!(parts.len(), 3, "control and two shards");
    let decided = "fb_chaos0decided0001";
    let undecided = "fb_chaos0undecided02";
    // Two transactions are prepared on every set, the way a schema change does:
    // `decided` has its journal row (the coordinator died after the decision),
    // `undecided` does not (it died before).
    for (gid, table) in [(decided, "fb178_decided"), (undecided, "fb178_undecided")] {
        for p in &parts {
            let mut tx = p.db.begin(TxMode::Normal).await.unwrap();
            tx.execute(&Statement::raw(&format!("CREATE TABLE {table} (id INT)"))).await.unwrap();
            tx.prepare(node_gid(gid, p.number)).await.unwrap();
        }
    }
    let nodes: Vec<String> = parts.iter().map(|p| format!("\"{}\"", p.set_id)).collect();
    {
        let mut tx = router.control().begin(TxMode::Normal).await.unwrap();
        tx.execute(&Statement::raw(&format!(
            "INSERT INTO _forgebase_xact_journal (gid, nodes, state, purpose, created_at) \
             VALUES ('{decided}', CAST('[{}]' AS jsonb), 'committing', 'fb178 chaos', '2026-01-01T00:00:00Z')",
            nodes.join(",")
        )))
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }
    for p in &parts {
        let rows = p.db.prepared_transactions().await.unwrap();
        assert_eq!(rows.len(), 2, "two prepared transactions on {}", p.set_id);
    }
    drop(parts);
    drop(router);

    // The crash: the primary and a shard die without a shutdown checkpoint.
    kill9(postmaster_pid(&t.primary_pgdata()).expect("primary runs"));
    t.shards.iter().find(|(n, _)| n == "s1").unwrap().1.crash();
    t.crash().await;
    t.shards.iter().find(|(n, _)| n == "s1").unwrap().1.restart().await;
    t.start_again().await;

    // Recovery at start commits the decided one everywhere and rolls the
    // undecided one back everywhere (after its grace period).
    let started = Instant::now();
    loop {
        let router = t.server().router().clone();
        let mut open = 0;
        for p in participants(&router) {
            open += p.db.prepared_transactions().await.unwrap().len();
        }
        if open == 0 {
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(240), "{open} transactions still prepared");
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let router = t.server().router().clone();
    for p in participants(&router) {
        let exists = |table: &'static str| {
            let db = p.db.clone();
            async move {
                db.fetch_all(&Statement::raw(&format!(
                    "SELECT count(*) FROM information_schema.tables WHERE table_name = '{table}'"
                )))
                .await
                .unwrap()
                .rows[0][0]
                    .as_i64()
                    .unwrap()
            }
        };
        assert_eq!(exists("fb178_decided").await, 1, "the decided transaction committed on {}", p.set_id);
        assert_eq!(exists("fb178_undecided").await, 0, "the undecided one rolled back on {}", p.set_id);
    }
    assert_eq!(
        t.control_scalar(&format!("SELECT count(*) FROM _forgebase_xact_journal WHERE gid = '{decided}' AND state = 'committed'"))
            .await,
        1,
        "the journal row is closed"
    );
    t.shutdown().await;
}
