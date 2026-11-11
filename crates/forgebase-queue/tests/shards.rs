//! Queues on database sets (FB-172, ADR-0029 §3.5).
//!
//! One set (SQLite and PostgreSQL): a router changes nothing. Several sets
//! (PostgreSQL clusters from `forgebase_db::shard_testing`, skipped without
//! an engine): a tenant's topics, subscriptions and snapshots live on its
//! set and instance-wide ones on control (a tenant's subscription to an
//! instance-wide topic with them); names resolve as on one set; listings
//! merge both; maintenance runs per set and skips moving tenants; an
//! instance-wide record-source topic relays every set's outbox with a
//! cursor per set on control.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{admin, harness, real_now, settings};
use forgebase_auth::crypto::Keyring;
use forgebase_data::{AccessContext, DataService, DefaultAuthorizer};
use forgebase_db::shard_testing::Sharded;
use forgebase_db::topology::PlacementState;
use forgebase_db::{Backend, Database, DbRouter, RouterOptions, Statement, TxMode, Value, shards};
use forgebase_jobs::ManualClock;
use forgebase_queue::{
    NewMessage, NewSnapshot, NewSubscription, NewTopic, PullRequest, QueueService, RecordSource,
};
use serde_json::json;

fn tenant(t: &str) -> AccessContext {
    admin().with_tenant(t)
}

fn msg(v: serde_json::Value) -> NewMessage {
    NewMessage {
        data: v,
        ..NewMessage::default()
    }
}

fn topic(name: &str) -> NewTopic {
    NewTopic {
        name: name.into(),
        ..NewTopic::default()
    }
}

fn sub(name: &str, topic: &str) -> NewSubscription {
    NewSubscription {
        name: name.into(),
        topic: topic.into(),
        ..NewSubscription::default()
    }
}

fn pull(n: i64) -> PullRequest {
    PullRequest {
        max_messages: Some(n),
        ..PullRequest::default()
    }
}

async fn count(db: &Database, sql: &str) -> i64 {
    db.fetch_all(&Statement::raw(sql))
        .await
        .unwrap()
        .rows
        .first()
        .and_then(|r| r.first())
        .and_then(Value::as_i64)
        .unwrap_or(-1)
}

async fn exec(db: &Database, sql: &str) {
    let mut tx = db.begin(TxMode::Normal).await.unwrap();
    tx.execute(&Statement::raw(sql)).await.unwrap();
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn one_set_with_a_router_behaves_as_before() {
    let h = harness().await;
    h.svc.attach_router(DbRouter::single(h.db.clone()));
    h.svc.create_topic(&tenant("ta"), topic("orders")).await.unwrap();
    h.svc.create_topic(&admin(), topic("events")).await.unwrap();
    h.svc.create_subscription(&tenant("ta"), sub("work", "orders")).await.unwrap();
    h.svc.create_subscription(&tenant("ta"), sub("all", "events")).await.unwrap();
    h.svc
        .publish(&tenant("ta"), "orders", vec![msg(json!({"n": 1}))])
        .await
        .unwrap();
    h.svc.publish(&admin(), "events", vec![msg(json!({"n": 2}))]).await.unwrap();
    h.svc.run_maintenance().await.unwrap();
    let got = h.svc.pull(&tenant("ta"), "work", pull(5)).await.unwrap();
    assert_eq!(got.len(), 1);
    h.svc
        .ack(&tenant("ta"), "work", &[got[0].ack_id.clone()])
        .await
        .unwrap();
    assert_eq!(h.svc.pull(&tenant("ta"), "all", pull(5)).await.unwrap().len(), 1);
    assert_eq!(h.svc.topics(&tenant("ta")).await.unwrap().len(), 2);
    assert_eq!(h.svc.subscriptions(&tenant("ta"), None).await.unwrap().len(), 2);
    let b = h.svc.backlog().await.unwrap();
    assert_eq!(b.in_flight, 1);
}

#[tokio::test]
async fn queues_live_with_their_topic_on_the_tenant_set() {
    let Some(mut sh) = Sharded::start().await else {
        eprintln!("skipped: no PostgreSQL engine");
        return;
    };
    let (set, s1) = sh.provision("s1").await;
    shards::activate_shard(
        &sh.router,
        &set,
        &s1,
        shards::ShardSetup {
            backups_dir: None,
            replay: None,
            control_conninfo: None,
        },
    )
    .await
    .unwrap();
    let router = DbRouter::open(
        sh.control.clone(),
        RouterOptions {
            connector: Some(sh.connector.clone()),
            freeze_timeout: Duration::from_millis(200),
            ..RouterOptions::default()
        },
    )
    .await
    .unwrap();
    router.place("tb", &set.id, PlacementState::Active).await.unwrap();
    router.place("tc", &set.id, PlacementState::Active).await.unwrap();
    let control = sh.control.clone();
    let data = DataService::new(Arc::new(control.clone()), Arc::new(DefaultAuthorizer))
        .await
        .unwrap();
    let clock = ManualClock::new(real_now());
    let svc = QueueService::with_clock(
        data,
        Keyring::from_master(b"queue-tests-master-key-0123456789"),
        settings(),
        Arc::new(clock.clone()),
    );
    svc.attach_router(router.clone());

    // Topics: a tenant's on its set, instance-wide ones on control.
    svc.create_topic(&tenant("tb"), topic("orders")).await.unwrap();
    svc.create_topic(&admin(), topic("events")).await.unwrap();
    let topics = "SELECT count(*) FROM _forgebase_queue_topics";
    assert_eq!((count(&s1, topics).await, count(&control, topics).await), (1, 1));
    assert_eq!(
        count(&s1, "SELECT count(*) FROM _forgebase_queue_topics WHERE name = 'tb/orders'").await,
        1
    );

    // Subscriptions live with their topic.
    svc.create_subscription(&tenant("tb"), sub("work", "orders")).await.unwrap();
    svc.create_subscription(&tenant("tb"), sub("all", "events")).await.unwrap();
    let subs_of = |db: &Database, name: &str| {
        let (db, sql) = (
            db.clone(),
            format!("SELECT count(*) FROM _forgebase_queue_subscriptions WHERE name = '{name}'"),
        );
        async move { count(&db, &sql).await }
    };
    assert_eq!((subs_of(&s1, "tb/work").await, subs_of(&control, "tb/work").await), (1, 0));
    assert_eq!((subs_of(&s1, "tb/all").await, subs_of(&control, "tb/all").await), (0, 1));

    // Publishing and pulling resolve names as on one set.
    svc.publish(&tenant("tb"), "orders", vec![msg(json!({"n": 1}))])
        .await
        .unwrap();
    svc.publish(&tenant("tb"), "events", vec![msg(json!({"n": 2}))])
        .await
        .unwrap();
    let messages = "SELECT count(*) FROM _forgebase_queue_messages";
    assert_eq!((count(&s1, messages).await, count(&control, messages).await), (1, 1));
    svc.run_maintenance().await.unwrap();
    let work = svc.pull(&tenant("tb"), "work", pull(5)).await.unwrap();
    assert_eq!(work.len(), 1);
    assert_eq!(work[0].message.data, json!({"n": 1}));
    svc.ack(&tenant("tb"), "work", &[work[0].ack_id.clone()]).await.unwrap();
    let all = svc.pull(&tenant("tb"), "all", pull(5)).await.unwrap();
    assert_eq!(all.len(), 1);
    svc.ack(&tenant("tb"), "all", &[all[0].ack_id.clone()]).await.unwrap();

    // Listings of a tenant caller merge its set and control.
    let names: Vec<String> = svc
        .topics(&tenant("tb"))
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.name)
        .collect();
    assert_eq!(names, vec!["events".to_owned(), "orders".to_owned()]);
    assert_eq!(svc.subscriptions(&tenant("tb"), None).await.unwrap().len(), 2);
    assert_eq!(svc.subscriptions(&tenant("tb"), Some("orders")).await.unwrap().len(), 1);
    assert_eq!(svc.topic(&tenant("tb"), "orders").await.unwrap().name, "orders");
    assert_eq!(svc.subscription(&tenant("tb"), "all").await.unwrap().topic, "events");

    // Snapshots: with their subscription.
    svc.create_snapshot(
        &tenant("tb"),
        NewSnapshot {
            name: "snap".into(),
            subscription: "work".into(),
        },
    )
    .await
    .unwrap();
    let snaps = "SELECT count(*) FROM _forgebase_queue_snapshots";
    assert_eq!((count(&s1, snaps).await, count(&control, snaps).await), (1, 0));
    assert_eq!(svc.snapshots(&tenant("tb")).await.unwrap().len(), 1);
    svc.snapshot(&tenant("tb"), "snap").await.unwrap();
    svc.delete_snapshot(&tenant("tb"), "snap").await.unwrap();

    // Backlog and metrics add up every set.
    svc.publish(&tenant("tb"), "orders", vec![msg(json!({"n": 3}))])
        .await
        .unwrap();
    svc.publish(&admin(), "events", vec![msg(json!({"n": 4}))]).await.unwrap();
    svc.run_maintenance().await.unwrap();
    let b = svc.backlog().await.unwrap();
    assert_eq!(b.ready, 2, "{b:?}");
    svc.topic_metrics(&tenant("tb"), "orders").await.unwrap();
    svc.subscription_metrics(&tenant("tb"), "work").await.unwrap();

    // A tenant moving away from s1 (its placement names control; its rows
    // on s1 are a copy): s1's maintenance leaves its leases alone.
    svc.create_topic(&tenant("tc"), topic("jobs")).await.unwrap();
    svc.create_subscription(
        &tenant("tc"),
        NewSubscription {
            ack_deadline_ms: Some(10_000),
            ..sub("w", "jobs")
        },
    )
    .await
    .unwrap();
    svc.publish(&tenant("tc"), "jobs", vec![msg(json!({"n": 5}))]).await.unwrap();
    svc.run_maintenance().await.unwrap();
    assert_eq!(svc.pull(&tenant("tc"), "w", pull(1)).await.unwrap().len(), 1);
    router.place("tc", "control", PlacementState::Moving).await.unwrap();
    clock.advance(Duration::from_secs(60));
    svc.run_maintenance().await.unwrap();
    let leased = "SELECT count(*) FROM _forgebase_queue_deliveries WHERE subscription = 'tc/w' AND state = 'leased'";
    assert_eq!(count(&s1, leased).await, 1, "an excluded tenant's lease is not reaped");
    router.place("tc", &set.id, PlacementState::Active).await.unwrap();
    svc.run_maintenance().await.unwrap();
    assert_eq!(count(&s1, leased).await, 0, "reaped once it is back");

    // A frozen tenant: calls wait for the freeze, then 409 tenant_moving.
    router.place("tb", &set.id, PlacementState::Frozen).await.unwrap();
    let err = svc
        .publish(&tenant("tb"), "orders", vec![msg(json!({"n": 6}))])
        .await
        .unwrap_err();
    assert_eq!(forgebase_core::ErrorCode::code(&err), "tenant_moving");
    router.place("tb", &set.id, PlacementState::Active).await.unwrap();

    // Record changes: an instance-wide source topic on control relays every
    // set's outbox; a cursor per set lives on control.
    svc.create_topic(
        &admin(),
        NewTopic {
            source: Some(RecordSource {
                collection: "posts".into(),
                ops: Vec::new(),
            }),
            ..topic("changes")
        },
    )
    .await
    .unwrap();
    svc.relay_outbox().await.unwrap();
    let key = format!(
        "SELECT count(*) FROM _forgebase_queue_state WHERE key = 'outbox_cursor:{}'",
        set.id
    );
    assert_eq!((count(&control, &key).await, count(&s1, &key).await), (1, 0));
    exec(
        &s1,
        "INSERT INTO _forgebase_realtime_outbox (op, collection_id, collection, record_id, tenant, actor, snapshot, \
         created_at) VALUES ('create', 'c1', 'posts', 'r1', 'tb', 'user:1', '{\"id\": \"r1\"}', \
         '2026-11-11T00:00:00Z')",
    )
    .await;
    assert_eq!(svc.relay_outbox().await.unwrap(), 1);
    assert_eq!(svc.relay_outbox().await.unwrap(), 0, "the cursor moved in the same transaction");
    let relayed = svc.messages(&admin(), "changes", 0, 10).await.unwrap();
    assert_eq!(relayed.items.len(), 1);
    assert_eq!(relayed.items[0].data["record_id"], "r1");
    assert_eq!(relayed.items[0].data["tenant"], "tb");
    drop(sh);
}
