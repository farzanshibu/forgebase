//! FB-164 (ADR-0029 §1.6): subscription deliveries on pgmq. One queue per
//! subscription, created and dropped with it; the pgmq layout check with
//! its portable fallback; `queue.convert` both ways with leases, attempts
//! and history kept. The ADR-0022 guarantees themselves are `queue.rs` and
//! `http.rs`, which run on whichever engine the test database offers
//! (pgmq on the bundled engine, portable on SQLite and on servers without
//! pgmq).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use common::{admin, harness, ids, on_pgmq, user};
use forgebase_db::{Backend, Statement};
use forgebase_queue::{CONVERT_JOB, DeliveryEngine, NewSubscription, register_jobs};
use serde_json::json;

async fn count(h: &common::H, sql: &str) -> i64 {
    let rows = h.db.fetch_all(&Statement::raw(sql)).await.unwrap();
    rows.scalar().and_then(forgebase_db::Value::as_i64).unwrap_or(0)
}

async fn queues(h: &common::H) -> i64 {
    count(h, "SELECT COUNT(*) FROM pgmq.meta WHERE queue_name LIKE 'fbs\\_%'").await
}

async fn meta(h: &common::H) -> Option<String> {
    h.db.meta_get("ext.queue.engine").await.unwrap()
}

/// The table of the only ForgeBase queue.
async fn only_table(h: &common::H) -> String {
    let rows = h
        .db
        .fetch_all(&Statement::raw(
            "SELECT queue_name FROM pgmq.meta WHERE queue_name LIKE 'fbs\\_%'",
        ))
        .await
        .unwrap();
    assert_eq!(rows.rows.len(), 1);
    format!("pgmq.q_{}", rows.rows[0][0].as_str().unwrap())
}

#[tokio::test]
async fn each_subscription_owns_a_pgmq_queue_of_its_deliveries() {
    let h = harness().await;
    if !on_pgmq(&h).await {
        return;
    }
    h.topic("t").await;
    h.sub("s", "t").await;
    assert_eq!(queues(&h).await, 1, "created with the subscription");
    let t = only_table(&h).await;
    for i in 0..3 {
        h.publish("t", json!(i)).await;
    }
    let got = h.pull("s", 2).await;
    assert_eq!(got.len(), 2);
    // Owed deliveries are pgmq entries: two leased (read once), one ready.
    assert_eq!(count(&h, &format!("SELECT COUNT(*) FROM {t}")).await, 3);
    assert_eq!(
        count(&h, &format!("SELECT COUNT(*) FROM {t} WHERE message->>'consumer' IS NOT NULL AND read_ct = 1")).await,
        2
    );
    assert_eq!(count(&h, "SELECT COUNT(*) FROM _forgebase_queue_deliveries").await, 0);
    h.ack("s", &got).await;
    assert_eq!(count(&h, &format!("SELECT COUNT(*) FROM {t}")).await, 1);

    h.svc.delete_subscription(&admin(), "s").await.unwrap();
    assert_eq!(queues(&h).await, 0, "dropped with the subscription");
    h.sub("s2", "t").await;
    h.sub("s3", "t").await;
    assert_eq!(queues(&h).await, 2);
    h.svc.delete_topic(&admin(), "t", true).await.unwrap();
    assert_eq!(queues(&h).await, 0, "dropped with the topic");
}

#[tokio::test]
async fn convert_moves_owed_deliveries_both_ways() {
    let h = harness().await;
    let start = h.svc.delivery_engine().await.unwrap();
    h.topic("t").await;
    h.sub_with(NewSubscription {
        name: "s".into(),
        topic: "t".into(),
        ack_deadline_ms: Some(10_000),
        backoff_base_ms: Some(0),
        ..NewSubscription::default()
    })
    .await;
    let mut published = Vec::new();
    for i in 0..5 {
        published.push(h.publish("t", json!(i)).await);
    }
    let leased = h.pull("s", 2).await;
    assert_eq!(ids(&leased), published[..2].to_vec());
    // An ordering subscription converts too (its key index comes along).
    h.sub_with(NewSubscription {
        name: "o".into(),
        topic: "t".into(),
        ordering: true,
        start: Some(forgebase_queue::StartPosition::Earliest),
        ..NewSubscription::default()
    })
    .await;
    assert_eq!(h.pull("o", 10).await.len(), 5);

    if start == DeliveryEngine::Portable && !pgmq_available(&h).await {
        // Nothing to convert to: refused, and portable to portable is a no-op.
        let err = h.svc.convert(&admin(), DeliveryEngine::Pgmq).await.unwrap_err();
        assert_eq!(err.code(), "pgmq_unavailable", "{err}");
        let r = h.svc.convert(&admin(), DeliveryEngine::Portable).await.unwrap();
        assert_eq!((r.from, r.to, r.deliveries), (DeliveryEngine::Portable, DeliveryEngine::Portable, 0));
        return;
    }
    // Tenants and non-admins may not convert.
    assert!(h.svc.convert(&user("u1"), DeliveryEngine::Portable).await.is_err());

    // To the other engine and back: owed deliveries, leases, attempts and
    // history move with them.
    let other = match start {
        DeliveryEngine::Pgmq => DeliveryEngine::Portable,
        DeliveryEngine::Portable => DeliveryEngine::Pgmq,
    };
    let r = h.svc.convert(&admin(), other).await.unwrap();
    assert_eq!((r.from, r.to, r.subscriptions, r.deliveries), (start, other, 2, 10), "{r:?}");
    assert_eq!(h.svc.delivery_engine().await.unwrap(), other);
    assert_eq!(meta(&h).await.as_deref(), Some(stored(other)));
    // A lease handed out before the conversion is still acked.
    h.ack("s", &leased[..1]).await;
    let rest = h.pull("s", 10).await;
    assert_eq!(ids(&rest), published[2..].to_vec(), "leased one stays leased");

    let r = h.svc.convert(&admin(), start).await.unwrap();
    assert_eq!((r.from, r.to, r.subscriptions, r.deliveries), (other, start, 2, 9), "{r:?}");
    assert_eq!(meta(&h).await.as_deref(), Some(stored(start)));
    h.ack("s", &rest).await;
    // The second early lease expires and is redelivered as attempt 2, with
    // its history carried across both conversions.
    h.clock.advance(Duration::from_millis(10_001));
    let again = h.pull("s", 10).await;
    assert_eq!(ids(&again), vec![published[1].clone()]);
    assert_eq!(again[0].delivery_attempt, 2);
    h.ack("s", &again).await;
    let m = h.svc.subscription_metrics(&admin(), "s").await.unwrap();
    assert_eq!((m.backlog_messages, m.in_flight), (0, 0));

    // The next start follows the stored engine.
    let h = h.restart().await;
    assert_eq!(h.svc.delivery_engine().await.unwrap(), start);
}

fn stored(e: DeliveryEngine) -> &'static str {
    match e {
        DeliveryEngine::Pgmq => "extension",
        DeliveryEngine::Portable => "portable",
    }
}

async fn pgmq_available(h: &common::H) -> bool {
    h.db.dialect().name() == "postgres"
        && count(h, "SELECT COUNT(*) FROM pg_extension WHERE extname = 'pgmq'").await > 0
}

#[tokio::test]
async fn a_queue_table_layout_mismatch_falls_back_to_portable() {
    let h = harness().await;
    if !on_pgmq(&h).await {
        return;
    }
    h.topic("t").await;
    h.sub("s", "t").await;
    let t = only_table(&h).await;
    let mut tx = h.db.begin(forgebase_db::TxMode::Normal).await.unwrap();
    tx.execute(&Statement::raw(format!("ALTER TABLE {t} RENAME COLUMN vt TO vt_renamed")))
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let h = h.restart().await;
    assert_eq!(
        h.svc.delivery_engine().await.unwrap(),
        DeliveryEngine::Portable,
        "the layout check refuses pgmq"
    );
    assert_eq!(meta(&h).await.as_deref(), Some("extension"), "the stored engine is kept");
    let id = h.publish("t", json!("x")).await;
    let got = h.pull("s", 10).await;
    assert_eq!(ids(&got), vec![id], "the portable engine delivers");
    h.ack("s", &got).await;
    // pgmq cannot be converted to while a queue table does not fit.
    let err = h.svc.convert(&admin(), DeliveryEngine::Pgmq).await.unwrap_err();
    assert_eq!(err.code(), "pgmq_unavailable");
    assert!(err.to_string().contains("no column vt"), "{err}");
}

#[tokio::test]
async fn the_convert_job_is_reserved() {
    let h = harness().await;
    let reg = register_jobs(forgebase_jobs::JobRegistry::default(), h.svc.clone());
    assert_eq!(CONVERT_JOB, "queue.convert");
    assert!(
        matches!(reg.access(CONVERT_JOB), Some(forgebase_jobs::KindAccess::Reserved)),
        "only privileged callers may enqueue it"
    );
}
