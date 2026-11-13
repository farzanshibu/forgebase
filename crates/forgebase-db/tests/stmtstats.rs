//! Statement fingerprints, statistics and the slow-statement capture on a real
//! database (ADR-0036 §1). SQLite always; PostgreSQL with
//! `FORGEBASE_TEST_BACKEND=postgres FORGEBASE_PG_ENGINE_DIR=...`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use forgebase_db::stmtstats::{StatementStats, fingerprint_id};
use forgebase_db::testing::test_backend;
use forgebase_db::{Backend, Database, Statement, TxMode};

/// The statistics are process-wide: one test at a time drains them.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn ddl(db: &Database, sql: &str) {
    let mut tx = db.begin(TxMode::SchemaChange).await.unwrap();
    tx.execute(&Statement::raw(sql)).await.unwrap();
    tx.commit().await.unwrap();
}

async fn run(db: &Database, sql: &str) {
    db.fetch_all(&Statement::raw(sql)).await.unwrap();
}

/// What the process-wide statistics hold for statements shaped like `sql`.
fn live(db: &Database, sql: &str) -> Option<forgebase_db::stmtstats::StatementAggregate> {
    let backend = if db.kind() == forgebase_db::BackendKind::Postgres { "postgres" } else { "sqlite" };
    let id = fingerprint_id(backend, sql);
    StatementStats::global().peek().into_iter().find(|a| a.fingerprint == id)
}

#[tokio::test]
async fn queries_that_differ_in_literals_are_one_fingerprint() {
    let _serial = SERIAL.lock().await;
    let t = test_backend().await;
    let db = t.database();
    ddl(db, "CREATE TABLE fp_items (id bigint PRIMARY KEY, name text)").await;
    for i in 0..5 {
        run(db, &format!("SELECT name FROM fp_items WHERE id = {i} AND name = 'n{i}'")).await;
    }
    run(db, "select name from fp_items where id=7 and name='x' -- hand written").await;
    db.fetch_all(&Statement::new(
        "SELECT name FROM fp_items WHERE id = ?1 AND name = ?2",
        vec![1.into(), "a".into()],
    ))
    .await
    .unwrap();
    let a = live(db, "SELECT name FROM fp_items WHERE id = 1 AND name = 'a'").expect("one fingerprint");
    assert_eq!(a.calls, 7, "{a:?}");
    assert_eq!(a.text, "select name from fp_items where id = ? and name = ?");
    assert_eq!(a.errors, 0);
    assert_eq!(a.sketch.count(), 7);
    assert!(a.total_us >= a.max_us && a.max_us >= a.min_us);
    // Another shape is another row.
    run(db, "SELECT id FROM fp_items WHERE id = 1").await;
    assert_eq!(live(db, "SELECT id FROM fp_items WHERE id = 9").unwrap().calls, 1);
}

#[tokio::test]
async fn failed_statements_count_as_errors() {
    let _serial = SERIAL.lock().await;
    let t = test_backend().await;
    let db = t.database();
    let r = db
        .fetch_all(&Statement::raw("SELECT * FROM fp_no_such_table WHERE id = 1"))
        .await;
    assert!(r.is_err());
    let a = live(db, "SELECT * FROM fp_no_such_table WHERE id = 2").expect("recorded");
    assert_eq!((a.calls, a.errors), (1, 1), "{a:?}");
}

/// The one test that drains and toggles the shared collector.
#[tokio::test]
async fn slow_statements_keep_no_literal_values_and_the_overhead_is_small() {
    let _serial = SERIAL.lock().await;
    let t = test_backend().await;
    let db = t.database();
    let stats = StatementStats::global();
    ddl(db, "CREATE TABLE fp_secrets (id bigint PRIMARY KEY, token text)").await;
    // -- Slow capture ------------------------------------------------------
    stats.drain();
    db.set_slow_query_threshold(Duration::ZERO);
    run(db, "SELECT id FROM fp_secrets WHERE token = 'hunter2-API-KEY' AND id > 12345").await;
    db.set_slow_query_threshold(Duration::from_secs(3600));
    run(db, "SELECT id FROM fp_secrets WHERE token = 'fast-one' AND id > 7").await;
    let d = stats.drain();
    let mine: Vec<_> = d.slow.iter().filter(|s| s.text.contains("fp_secrets")).collect();
    assert_eq!(mine.len(), 1, "only the statement over the threshold is listed: {:?}", d.slow);
    let s = mine[0];
    assert_eq!(s.text, "select id from fp_secrets where token = ? and id > ?");
    assert!(!s.text.contains("hunter2") && !s.text.contains("12345"));
    assert!(s.duration_us > 0 || s.rows == 0);
    assert!(
        d.aggregates.iter().all(|a| !a.text.contains("hunter2") && !a.text.contains("fast-one")),
        "no aggregate text holds a value"
    );
    let a = d.aggregates.iter().find(|a| a.text.contains("fp_secrets")).unwrap();
    assert_eq!((a.calls, a.slow), (2, 1));

    // -- Overhead ----------------------------------------------------------
    // (1) The hot path alone: a 200-byte statement, one thread and eight.
    let sql = "SELECT a.id, a.name, b.total FROM accounts a JOIN balances b ON b.account_id = a.id \
               WHERE a.region = 'eu-west-1' AND b.total > 1000 AND a.status IN ('open', 'held', 'new') ORDER BY b.total DESC LIMIT 50";
    let local = Arc::new(StatementStats::new());
    let n = 50_000u32;
    let t0 = Instant::now();
    for _ in 0..n {
        local.record("sqlite", sql, None, Duration::from_micros(120), 3, false, false, false);
    }
    let single = t0.elapsed() / n;
    let t0 = Instant::now();
    std::thread::scope(|sc| {
        for _ in 0..8 {
            let l = local.clone();
            sc.spawn(move || {
                for _ in 0..n {
                    l.record("sqlite", sql, None, Duration::from_micros(120), 3, false, false, false);
                }
            });
        }
    });
    let per_call_8 = t0.elapsed() / (8 * n);
    eprintln!("statement statistics: {single:?}/statement on one thread, {per_call_8:?}/statement on eight (debug build)");
    // Stated bound (unoptimised test build; release is several times lower):
    // 25 microseconds per statement on one thread, and eight threads sharing
    // one fingerprint do not serialise (per statement no worse than 4x).
    assert!(single < Duration::from_micros(25), "{single:?}");
    assert!(per_call_8 < single * 4 + Duration::from_micros(5), "{per_call_8:?} vs {single:?}");
    assert_eq!(local.peek()[0].calls, u64::from(9 * n));

    // (2) End to end: point lookups through the database with the statistics
    // on and off. Best of three rounds each, to ignore scheduling noise.
    for i in 0..50 {
        db.fetch_all(&Statement::new(
            "SELECT token FROM fp_secrets WHERE id = ?1",
            vec![i.into()],
        ))
        .await
        .unwrap();
    }
    let lookups = 400;
    let timed = |on: bool| stats.set_enabled(on);
    let mut off = Duration::MAX;
    let mut on = Duration::MAX;
    for _ in 0..3 {
        timed(false);
        let t0 = Instant::now();
        for i in 0..lookups {
            db.fetch_all(&Statement::new(
                "SELECT token FROM fp_secrets WHERE id = ?1",
                vec![i64::from(i).into()],
            ))
            .await
            .unwrap();
        }
        off = off.min(t0.elapsed());
        timed(true);
        let t0 = Instant::now();
        for i in 0..lookups {
            db.fetch_all(&Statement::new(
                "SELECT token FROM fp_secrets WHERE id = ?1",
                vec![i64::from(i).into()],
            ))
            .await
            .unwrap();
        }
        on = on.min(t0.elapsed());
    }
    let extra = on.saturating_sub(off) / lookups as u32;
    eprintln!("point lookup: {off:?} off, {on:?} on for {lookups}: {extra:?} extra per statement");
    assert!(extra < Duration::from_micros(40), "extra per statement: {extra:?}");
}
