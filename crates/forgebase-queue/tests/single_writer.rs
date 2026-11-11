//! FB-146: subscription cursors rely on "sequence order is commit order"
//! (ADR-0022, ADR-0028 §7.3). These tests pin it on both backends:
//! the database has exactly one writer connection, a cursor that tails a
//! topic while many publishers commit never skips a message, and on
//! PostgreSQL nothing else writes to the database except the two writers
//! ADR-0029 allows (pg_cron's `forgebase.fire_schedule()` and logical
//! replication apply workers). They fail if a second writer is configured.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use common::writers::{Writer, census, classify, writer_pid};
use common::{admin, harness};
use forgebase_db::{Backend, BackendKind, Database, Statement, Value};
use forgebase_queue::NewMessage;
use serde_json::json;

const PUBLISHERS: usize = 8;
const EACH: usize = 25;

/// Message `seq`s of `topic` after `after`, in order (what a cursor reads).
async fn after(db: &Database, topic: &str, after: i64) -> Vec<i64> {
    let rows = db
        .fetch_all(&Statement::new(
            "SELECT seq FROM _forgebase_queue_messages WHERE topic = ?1 AND seq > ?2 ORDER BY seq",
            vec![topic.into(), after.into()],
        ))
        .await
        .unwrap();
    rows.rows.iter().map(|r| r[0].as_i64().unwrap()).collect()
}

#[test]
fn only_pg_cron_fire_schedule_and_replication_apply_may_write_besides_forgebase() {
    let fire = "SELECT forgebase.fire_schedule('sch-1')";
    assert_eq!(classify("pg_cron", "", fire), Writer::PgCronFireSchedule);
    assert_eq!(classify("client backend", "pg_cron", fire), Writer::PgCronFireSchedule);
    for t in [
        "logical replication apply worker",
        "logical replication parallel worker",
        "logical replication tablesync worker",
    ] {
        assert_eq!(classify(t, "", ""), Writer::ReplicationApply, "{t}");
    }
    assert!(classify("autovacuum worker", "", "autovacuum: ANALYZE x").allowed());
    // pg_cron running anything but the fire function, or a second client.
    for (t, app, q) in [
        ("pg_cron", "", "UPDATE _forgebase_jobs SET status = 'dead'"),
        (
            "pg_cron",
            "",
            "SELECT forgebase.fire_schedule('a'); DELETE FROM _forgebase_jobs",
        ),
        ("client backend", "pg_cron", "SELECT 1"),
        (
            "client backend",
            "forgebase",
            "INSERT INTO _forgebase_queue_messages ...",
        ),
        ("client backend", "psql", fire),
        ("background worker", "", ""),
    ] {
        assert!(!classify(t, app, q).allowed(), "{t} {app}: {q}");
    }
}

#[tokio::test]
async fn one_writer_keeps_message_sequence_order_equal_to_commit_order() {
    let h = harness().await;
    assert_eq!(
        h.db.pool_stats().writer.max,
        1,
        "the database has exactly one writer connection (ADR-0028 §7.3)"
    );
    h.topic("t").await;
    let writer = match h.db.kind() {
        BackendKind::Postgres => Some(writer_pid(&h.db).await),
        BackendKind::Sqlite => None,
    };

    let done = Arc::new(AtomicBool::new(false));
    // A cursor tailing the topic: what it reads must be every committed
    // message, each once, in order.
    let tail = {
        let (db, done) = (h.db.clone(), done.clone());
        tokio::spawn(async move {
            let (mut cursor, mut seen) = (0_i64, Vec::new());
            loop {
                let last_round = done.load(Ordering::SeqCst);
                for seq in after(&db, "t", cursor).await {
                    assert!(seq > cursor, "cursor order");
                    cursor = seq;
                    seen.push(seq);
                }
                if last_round {
                    return seen;
                }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
    };
    // Who else holds a transaction id while the publishers run.
    let sampler = {
        let (db, done) = (h.db.clone(), done.clone());
        tokio::spawn(async move {
            let mut others = Vec::new();
            if let Some(pid) = writer {
                while !done.load(Ordering::SeqCst) {
                    others.extend(census(&db, pid).await);
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                }
            }
            others
        })
    };
    let publishers: Vec<_> = (0..PUBLISHERS)
        .map(|p| {
            let svc = h.svc.clone();
            tokio::spawn(async move {
                for i in 0..EACH {
                    svc.publish(&admin(), "t", vec![NewMessage::new(json!({"p": p, "i": i}))])
                        .await
                        .unwrap();
                }
            })
        })
        .collect();
    for p in publishers {
        p.await.unwrap();
    }
    done.store(true, Ordering::SeqCst);
    let seen = tail.await.unwrap();
    let others = sampler.await.unwrap();

    let committed = after(&h.db, "t", 0).await;
    assert_eq!(committed.len(), PUBLISHERS * EACH);
    assert_eq!(
        seen, committed,
        "the tailing cursor read every committed message once, in sequence order"
    );
    let second: BTreeSet<String> = others
        .into_iter()
        .filter(|(_, w)| !w.allowed())
        .map(|(pid, w)| format!("{pid}: {w:?}"))
        .collect();
    assert!(second.is_empty(), "second writers: {second:?}");
}

/// The census sees a second writer: a second database handle (its own writer
/// connection) writing next to the first. PostgreSQL only.
#[tokio::test]
async fn a_second_writer_is_detected() {
    let h = harness().await;
    if h.db.kind() != BackendKind::Postgres {
        eprintln!("PostgreSQL only: SQLite serializes every writer on its file lock");
        return;
    }
    let pid = writer_pid(&h.db).await;
    let second = h.dir.open().await;
    assert_ne!(writer_pid(&second).await, pid);
    let mut tx = second.begin(forgebase_db::TxMode::Normal).await.unwrap();
    tx.execute(&Statement::new(
        "INSERT INTO _forgebase_queue_rates (scope, name, metric, minute, count) VALUES ('t', 'x', 'm', 1, 1)",
        Vec::<Value>::new(),
    ))
    .await
    .unwrap();
    let found = census(&h.db, pid).await;
    tx.rollback().await.unwrap();
    assert!(
        found.iter().any(|(_, w)| !w.allowed()),
        "an open write transaction of another connection is a second writer: {found:?}"
    );
    second.close().await;
}
