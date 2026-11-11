//! FB-178: lag and read-your-writes. A replica whose replay is paused falls
//! behind: reads that carry the caller's commit token (or ask for strong
//! consistency) never see old data, reads without one may, and past
//! `max_lag_ms` the replica leaves the rotation until it has caught up.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use forgebase_db::{Backend, ReadIntent, Statement};
use forgebase_topology_tests::*;
use serde_json::json;

async fn count(t: &Topo, extra: &[(&str, &str)]) -> usize {
    let r = t
        .as_(&t.admin, "GET", "/api/v1/collections/events/records?perPage=500", extra, None)
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    Topo::count(&r)
}

#[tokio::test]
async fn paused_replicas_never_serve_a_read_older_than_the_callers_writes_and_leave_the_rotation() {
    if !enabled() {
        return;
    }
    let mut t = Topo::bare_with(|c| {
        let r = &mut c.database.topology.replicas;
        r.max_lag_ms = 4_000;
        r.health_interval_ms = 300;
    })
    .await;
    t.add_replica("r1").await;
    t.add_replica("r2").await;
    // The data core takes replicas up when the server opens its router.
    t.restart().await;
    for r in ["r1", "r2"] {
        t.wait_in_rotation(r, Duration::from_secs(120)).await;
    }
    let r = t
        .adm(
            "POST",
            "/api/v1/collections",
            Some(json!({"name": "events", "fields": [{"name": "k", "type": "text"}]})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.text());
    for i in 0..5 {
        let r = t
            .adm("POST", "/api/v1/collections/events/records", Some(json!({"k": format!("a{i}")})))
            .await;
        assert_eq!(r.status, 201);
    }
    let replicas = t.server().router().control().replicas().cloned().expect("replicas");
    let standby_count = async || {
        let rows = replicas
            .fetch(
                &Statement::raw("SELECT CAST(pg_is_in_recovery() AS INT), (SELECT count(*) FROM events)"),
                &ReadIntent::replica("fb178 lag"),
            )
            .await
            .unwrap();
        (rows.rows[0][0].as_i64().unwrap(), rows.rows[0][1].as_i64().unwrap())
    };
    // Both standbys have the five.
    until("the standbys have the first five", Duration::from_secs(30), async || {
        let mut ok = 0;
        for _ in 0..6 {
            let (rec, n) = standby_count().await;
            ok += usize::from(rec == 1 && n == 5);
        }
        ok >= 4
    })
    .await;

    // Pause the replay on both standbys, then write five more.
    for r in ["r1", "r2"] {
        psql(&t.node_data(r), "postgres", "SELECT pg_wal_replay_pause()").expect("pause");
    }
    let mut token = String::new();
    for i in 0..5 {
        let r = t
            .adm("POST", "/api/v1/collections/events/records", Some(json!({"k": format!("b{i}")})))
            .await;
        assert_eq!(r.status, 201);
        token = r.header("x-forgebase-commit").expect("commit token").to_owned();
    }
    assert!(token.starts_with("control/"), "{token}");

    // A read with no token may be old; with the token (or strong) it is not.
    let (in_recovery, n) = standby_count().await;
    assert_eq!((in_recovery, n), (1, 5), "a replica read with no token is behind");
    assert_eq!(
        count(&t, &[("X-ForgeBase-Read-After", &token), ("X-ForgeBase-Consistency", "eventual")]).await,
        10,
        "read-after: the replicas are behind, the read goes to the primary"
    );
    assert_eq!(count(&t, &[("X-ForgeBase-Consistency", "strong")]).await, 10, "strong");
    assert_eq!(count(&t, &[]).await, 10, "the writer's own next read (remembered commit)");

    // Past max_lag_ms both leave the rotation; every HTTP read still works.
    until("both replicas lag out of rotation", Duration::from_secs(90), async || {
        let mut out = 0;
        for r in ["r1", "r2"] {
            if let Some(n) = t.node(r).await
                && n["in_rotation"] != true
            {
                assert_eq!(n["state"], "lagging", "{n}");
                out += 1;
            }
        }
        assert_eq!(count(&t, &[]).await, 10);
        out == 2
    })
    .await;
    let (in_recovery, n) = standby_count().await;
    assert_eq!((in_recovery, n), (0, 10), "with no replica in rotation reads use the primary");
    assert_eq!(count(&t, &[("X-ForgeBase-Read-After", &token)]).await, 10);

    // Resume: they catch up and come back; their reads include every write.
    for r in ["r1", "r2"] {
        psql(&t.node_data(r), "postgres", "SELECT pg_wal_replay_resume()").expect("resume");
    }
    for r in ["r1", "r2"] {
        t.wait_in_rotation(r, Duration::from_secs(120)).await;
    }
    let mut served = 0;
    for _ in 0..12 {
        let (rec, n) = standby_count().await;
        if rec == 1 {
            served += 1;
            assert_eq!(n, 10, "a standby that is back has everything");
        }
    }
    assert!(served > 0, "replicas serve reads again");
    let n1 = t.node("r1").await.unwrap();
    assert!(n1["lag_bytes"].as_u64().is_some(), "{n1}");
    t.shutdown().await;
}
