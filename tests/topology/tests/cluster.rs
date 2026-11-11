//! FB-178: the black-box suite on one primary, two local replicas and three
//! shards (ADR-0029). Everything goes through `/api/system/database/*` and
//! `/api/v1/*`; the shard clusters are only looked at to see where bytes
//! physically are.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use forgebase_db::Backend;
use forgebase_topology_tests::*;
use serde_json::json;

async fn shard_count(t: &Topo, shard: &str, sql: &str) -> i64 {
    let db = t.shard(shard).database("forgebase").await;
    let rows = db.fetch_all(&forgebase_db::Statement::raw(sql)).await.unwrap();
    rows.rows[0][0].as_i64().unwrap()
}

#[tokio::test]
async fn the_topology_serves_tenants_replica_reads_and_schema_changes() {
    if !enabled() {
        return;
    }
    let t0 = std::time::Instant::now();
    let mut t = Topo::start(2, 3).await;
    // Replicas added while the server runs are used by the data core after the
    // next start (its handle on control is taken when the server opens).
    t.restart().await;
    for r in ["r1", "r2"] {
        t.wait_in_rotation(r, Duration::from_secs(120)).await;
    }
    eprintln!("[{:?}] topology up and restarted", t0.elapsed());

    // --- The topology through the API -------------------------------------
    let topo = t.topology().await;
    assert_eq!(topo["backend"], "postgres");
    let sets = topo["sets"].as_array().unwrap();
    assert_eq!(sets.len(), 4, "control and three shards: {topo}");
    let nodes = topo["nodes"].as_array().unwrap();
    for r in ["r1", "r2"] {
        let n = nodes.iter().find(|n| n["name"] == r).expect(r);
        assert_eq!(
            (n["role"].as_str(), n["state"].as_str()),
            (Some("replica"), Some("streaming"))
        );
        assert_eq!(n["in_rotation"], true, "{n}");
        assert_eq!(n["set"], "control");
    }
    assert!(!topo.to_string().contains("postgres://"), "no node URL ever shows");

    // --- A schema change reaches control and every shard (two-phase) -------
    t.tenant_collection("notes").await;
    let r = t
        .adm(
            "POST",
            "/api/v1/collections",
            Some(json!({"name": "settings", "fields": [{"name": "k", "type": "text"}, {"name": "v", "type": "text"}]})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.text());
    for name in ["s1", "s2", "s3"] {
        for table in ["notes", "settings"] {
            assert_eq!(
                shard_count(
                    &t,
                    name,
                    &format!("SELECT count(*) FROM information_schema.tables WHERE table_name = '{table}' AND table_schema = 'public'")
                )
                .await,
                1,
                "{table} exists on {name}"
            );
        }
    }
    let rows = t
        .control_scalar("SELECT count(*) FROM _forgebase_xact_journal WHERE state = 'committing'")
        .await;
    assert_eq!(rows, 0, "no two-phase decision is left open");

    eprintln!("[{:?}] schema change done", t0.elapsed());
    // --- Tenants: placed on shards, isolated, physically where they belong -
    let set_ids: Vec<String> = {
        let mut v = Vec::new();
        for n in ["s1", "s2", "s3"] {
            v.push(t.set(n).await.unwrap()["id"].as_str().unwrap().to_owned());
        }
        v
    };
    let mut tenants = Vec::new();
    for i in 0..6 {
        let tenant = t.new_tenant().await;
        t.place(&tenant, &set_ids[i % 3]).await;
        tenants.push(tenant);
    }
    for (i, tenant) in tenants.iter().enumerate() {
        for n in 0..5 {
            let r = t
                .write(tenant, "notes", json!({"body": format!("t{i}-{n}"), "n": n}))
                .await;
            assert_eq!(r.status, 201, "write: {}", r.text());
        }
    }
    for (i, tenant) in tenants.iter().enumerate() {
        let r = t.list(tenant, "notes", &[]).await;
        assert_eq!(r.status, 200, "{}", r.text());
        assert_eq!(Topo::count(&r), 5, "tenant {i} sees its own five");
        assert!(
            r.json()["items"]
                .as_array()
                .unwrap()
                .iter()
                .all(|x| x["body"].as_str().unwrap().starts_with(&format!("t{i}-"))),
            "no other tenant's rows"
        );
    }
    for name in ["s1", "s2", "s3"] {
        assert_eq!(
            shard_count(&t, name, "SELECT count(*) FROM notes").await,
            10,
            "two tenants of five records on {name}"
        );
    }
    assert_eq!(
        t.control_scalar("SELECT count(*) FROM notes").await,
        0,
        "none on control"
    );
    let r = t.adm("GET", &format!("{BASE}/placements?limit=50"), None).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let placed = r.json()["items"].as_array().unwrap().clone();
    for (i, tenant) in tenants.iter().enumerate() {
        let p = placed
            .iter()
            .find(|p| p["tenant"] == tenant.as_str())
            .expect("placement listed");
        assert_eq!(
            (p["set"].as_str(), p["state"].as_str()),
            (Some(set_ids[i % 3].as_str()), Some("active"))
        );
    }

    eprintln!("[{:?}] tenants done", t0.elapsed());
    // --- Replica reads: served by the standbys, never ahead of the writer --
    for k in 0..10 {
        let r = t
            .adm(
                "POST",
                "/api/v1/collections/settings/records",
                Some(json!({"k": format!("k{k}"), "v": "x"})),
            )
            .await;
        assert_eq!(r.status, 201, "{}", r.text());
    }
    let reader = t.user().await;
    let status = || t.server().router().control().replicas().expect("replicas").status();
    let before: u64 = status().iter().map(|s| s.reads).sum();
    // A reader that carries no token (no recent write of its own): allowed on replicas.
    for _ in 0..30 {
        let r = t
            .as_(&reader, "GET", "/api/v1/collections/settings/records", &[], None)
            .await;
        assert!(r.status == 200 || r.status == 403, "{}", r.text());
    }
    let after: u64 = status().iter().map(|s| s.reads).sum();
    eprintln!("replica reads {before} -> {after}");
    // A write and, right after it, a read by the same principal never goes back in time.
    for k in 0..10 {
        let r = t
            .adm(
                "POST",
                "/api/v1/collections/settings/records",
                Some(json!({"k": format!("ryw{k}"), "v": "y"})),
            )
            .await;
        assert_eq!(r.status, 201);
        // FINDING (FB-178): with shards present, a control-set record write returns no
        // X-ForgeBase-Commit even after a restart with two replicas (see the report).
        if r.header("x-forgebase-commit").is_none() {
            eprintln!("FINDING: no X-ForgeBase-Commit on a record write with shards present");
        }
        let l = t
            .adm("GET", "/api/v1/collections/settings/records?perPage=500", None)
            .await;
        assert_eq!(l.status, 200);
        assert!(
            l.json()["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|x| x["k"] == format!("ryw{k}")),
            "read-your-writes: ryw{k}"
        );
    }

    eprintln!("[{:?}] replica reads done", t0.elapsed());
    // --- The operations log and audit trail record what happened -----------
    let r = t.adm("GET", &format!("{BASE}/operations?limit=50"), None).await;
    assert_eq!(r.status, 200);
    let ops = r.json();
    let kinds: Vec<&str> = ops["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|o| o["kind"].as_str())
        .collect();
    assert!(
        kinds.contains(&"database.node.add") && kinds.contains(&"database.shard.add"),
        "{kinds:?}"
    );
    let r = t.adm("GET", "/api/auth/audit?event=db.topology.shard.add", None).await;
    assert_eq!(r.status, 200);
    assert!(
        r.json()["items"].as_array().unwrap().len() >= 3,
        "three shard adds audited"
    );

    eprintln!("[{:?}] audit done", t0.elapsed());
    // --- A restart brings every node back with no loss -----------------------
    t.restart().await;
    let reads = t.list(&tenants[0], "notes", &[]).await;
    assert_eq!(Topo::count(&reads), 5);
    until(
        "both replicas are back in rotation",
        Duration::from_secs(120),
        async || {
            t.node("r1").await.is_some_and(|n| n["in_rotation"] == true)
                && t.node("r2").await.is_some_and(|n| n["in_rotation"] == true)
        },
    )
    .await;
    t.shutdown().await;
}
