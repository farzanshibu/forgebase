//! FB-178: the WAL cost of `wal_level = logical` (ADR-0029 §1.3, risk table)
//! against `wal_level = replica`, for the same write load on two clusters of
//! the same engine. The numbers go to the ADR-0029 addendum.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use forgebase_db::shard_testing::TestCluster;
use forgebase_db::{Backend, Database, Statement, TxMode};
use forgebase_topology_tests::*;
use serde_json::json;

async fn lsn(db: &Database) -> String {
    db.fetch_all(&Statement::raw("SELECT pg_current_wal_insert_lsn()::text"))
        .await
        .unwrap()
        .rows[0][0]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn diff(db: &Database, from: &str, to: &str) -> i64 {
    db.fetch_all(&Statement::raw(&format!(
        "SELECT CAST(pg_wal_lsn_diff('{to}', '{from}') AS BIGINT)"
    )))
    .await
    .unwrap()
    .rows[0][0]
        .as_i64()
        .unwrap()
}

async fn run(db: &Database, sql: &str) {
    let mut tx = db.begin(TxMode::Normal).await.unwrap();
    tx.execute(&Statement::raw(sql)).await.unwrap();
    tx.commit().await.unwrap();
}

/// The load: ForgeBase-shaped rows (text key, tenant, JSON body, a counter,
/// two secondary indexes); inserts, updates of the indexed counter and the
/// body, and deletes, in small transactions like the API makes them.
async fn load(db: &Database) -> (i64, i64, i64) {
    run(
        db,
        "CREATE TABLE fb178_wal (id BIGINT PRIMARY KEY, tenant TEXT NOT NULL, body JSONB NOT NULL, n INT NOT NULL, \
         created_at TEXT NOT NULL); CREATE INDEX fb178_wal_tenant ON fb178_wal (tenant, id); \
         CREATE INDEX fb178_wal_n ON fb178_wal (n)",
    )
    .await;
    let a = lsn(db).await;
    for b in 0..400 {
        run(
            db,
            &format!(
                "INSERT INTO fb178_wal SELECT g, 't' || (g % 50), \
                 jsonb_build_object('title', md5(g::text), 'text', repeat(md5((g * 7)::text), 4), 'tags', jsonb_build_array('a', 'b', g % 9)), \
                 g % 1000, '2026-01-01T00:00:00Z' FROM generate_series({}, {}) g",
                b * 100 + 1,
                b * 100 + 100
            ),
        )
        .await;
    }
    let b = lsn(db).await;
    for k in 0..200 {
        run(
            db,
            &format!(
                "UPDATE fb178_wal SET n = n + 1, body = jsonb_set(body, '{{title}}', to_jsonb(md5((id + 1)::text))) \
                 WHERE id BETWEEN {} AND {}",
                k * 100 + 1,
                k * 100 + 100
            ),
        )
        .await;
    }
    let c = lsn(db).await;
    for k in 0..100 {
        run(
            db,
            &format!("DELETE FROM fb178_wal WHERE id BETWEEN {} AND {}", k * 100 + 1, k * 100 + 100),
        )
        .await;
    }
    let d = lsn(db).await;
    (diff(db, &a, &b).await, diff(db, &b, &c).await, diff(db, &c, &d).await)
}

#[tokio::test]
async fn logical_decoding_adds_a_small_share_of_wal_volume() {
    if !enabled() {
        return;
    }
    let mut results = Vec::new();
    for level in ["replica", "logical", "replica", "logical"] {
        let cluster = TestCluster::start(&format!("wal-{level}"), &[("wal_level", level)])
            .await
            .expect("an engine");
        let db = cluster.database("forgebase").await;
        let shown = db
            .fetch_all(&Statement::raw("SELECT current_setting('wal_level')"))
            .await
            .unwrap();
        assert_eq!(shown.rows[0][0].as_str(), Some(level));
        let (ins, upd, del) = load(&db).await;
        eprintln!("{level}: insert {ins} update {upd} delete {del}");
        results.push((level, ins, upd, del));
    }
    // Best (smallest) run per level: the load is deterministic, the engine's
    // background WAL (checkpointer, autovacuum) is not.
    let best = |level: &str| {
        results
            .iter()
            .filter(|r| r.0 == level)
            .map(|r| (r.1, r.2, r.3))
            .reduce(|a, b| (a.0.min(b.0), a.1.min(b.1), a.2.min(b.2)))
            .unwrap()
    };
    let (r, l) = (best("replica"), best("logical"));
    let pct = |a: i64, b: i64| (b as f64 - a as f64) * 100.0 / a as f64;
    let total = |x: (i64, i64, i64)| x.0 + x.1 + x.2;
    let out = json!({
        "load": "40000 inserts, 20000 updates (indexed counter and JSON body), 10000 deletes in 700 transactions of 100 rows; \
                 text key, JSON body, two secondary indexes",
        "replica_bytes": {"insert": r.0, "update": r.1, "delete": r.2, "total": total(r)},
        "logical_bytes": {"insert": l.0, "update": l.1, "delete": l.2, "total": total(l)},
        "overhead_percent": {
            "insert": pct(r.0, l.0), "update": pct(r.1, l.1), "delete": pct(r.2, l.2), "total": pct(total(r) , total(l)),
        },
    });
    record("wal_level", &out);
    assert!(total(l) >= total(r), "logical never writes less WAL than replica: {out}");
    assert!(
        pct(total(r), total(l)) < 50.0,
        "logical decoding should cost well under 50% more WAL for row changes: {out}"
    );
}
