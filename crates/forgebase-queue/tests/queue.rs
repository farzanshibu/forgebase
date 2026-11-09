//! Acceptance tests of FB-63 (addendum §Acceptance) against real SQLite:
//! fan-out, competing consumers, redelivery, dead letters, ordering, dedup,
//! transactional publish, replay, restart survival, plus filters, schema,
//! retention, metrics, quotas and authorisation.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{admin, datas, harness, ids, user};
use forgebase_core::ErrorCode;
use forgebase_data::AccessContext;
use forgebase_db::TxMode;
use forgebase_queue::{
    NewMessage, NewSnapshot, NewSubscription, NewTopic, PullRequest, QueueError, RecordSource, SeekRequest,
    StartPosition, TopicPatch, iso,
};
use serde_json::json;

fn sub(name: &str, topic: &str) -> NewSubscription {
    NewSubscription {
        name: name.into(),
        topic: topic.into(),
        ..NewSubscription::default()
    }
}

#[tokio::test]
async fn fan_out_reaches_every_subscription() {
    let h = harness().await;
    h.topic("orders").await;
    h.sub("billing", "orders").await;
    h.sub("shipping", "orders").await;
    for i in 0..3 {
        h.publish("orders", json!({"n": i})).await;
    }
    let a = h.pull("billing", 10).await;
    let b = h.pull("shipping", 10).await;
    assert_eq!(datas(&a), vec![json!({"n": 0}), json!({"n": 1}), json!({"n": 2})]);
    assert_eq!(ids(&a), ids(&b), "each subscription gets every message");
    // Acking on one subscription does not touch the other.
    h.ack("billing", &a).await;
    assert!(h.pull("billing", 10).await.is_empty());
    // The other subscription's leases expire; after the retry backoff they come back.
    h.clock.advance(Duration::from_secs(60));
    h.svc.run_maintenance().await.unwrap();
    h.clock.advance(Duration::from_secs(2));
    let again = h.pull("shipping", 10).await;
    assert_eq!(ids(&again), ids(&b), "unacked messages of the other subscription come back");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn competing_consumers_share_a_subscription_without_duplicates() {
    let h = Arc::new(harness().await);
    h.topic("work").await;
    h.sub("workers", "work").await;
    for batch in 0..3 {
        let msgs = (0..100).map(|i| NewMessage::new(json!({"job": batch * 100 + i}))).collect();
        h.svc.publish(&admin(), "work", msgs).await.unwrap();
    }
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let mut tasks = Vec::new();
    for w in 0..8 {
        let (h, seen) = (h.clone(), seen.clone());
        tasks.push(tokio::spawn(async move {
            let consumer = format!("worker-{w}");
            loop {
                let got = h
                    .svc
                    .pull(
                        &admin(),
                        "workers",
                        PullRequest {
                            max_messages: Some(7),
                            consumer: Some(consumer.clone()),
                            ..PullRequest::default()
                        },
                    )
                    .await
                    .unwrap();
                if got.is_empty() {
                    return;
                }
                seen.lock().unwrap().extend(ids(&got));
                h.ack("workers", &got).await;
            }
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    let seen = seen.lock().unwrap().clone();
    let unique: BTreeSet<_> = seen.iter().cloned().collect();
    assert_eq!(seen.len(), 300, "every message delivered once");
    assert_eq!(unique.len(), 300, "no duplicates");
    let m = h.svc.subscription_metrics(&admin(), "workers").await.unwrap();
    assert_eq!((m.backlog_messages, m.acked_total, m.delivered_total), (0, 300, 300));
}

#[tokio::test]
async fn a_missed_ack_deadline_redelivers() {
    let h = harness().await;
    h.topic("t").await;
    h.sub_with(NewSubscription {
        ack_deadline_ms: Some(2_000),
        backoff_base_ms: Some(1_000),
        ..sub("s", "t")
    })
    .await;
    let id = h.publish("t", json!("hello")).await;
    let first = h.pull("s", 10).await;
    assert_eq!((first.len(), first[0].delivery_attempt), (1, 1));
    assert!(h.pull("s", 10).await.is_empty(), "leased: not handed out twice");
    h.clock.advance(Duration::from_millis(2_001));
    assert!(h.pull("s", 10).await.is_empty(), "expired, waiting for the retry backoff");
    h.clock.advance(Duration::from_millis(1_000));
    let second = h.pull("s", 10).await;
    assert_eq!(second.len(), 1);
    assert_eq!((second[0].message.id.as_str(), second[0].delivery_attempt), (id.as_str(), 2));
    // The first delivery's ack id is no longer current.
    let stale = h.svc.ack(&admin(), "s", &[first[0].ack_id.clone()]).await.unwrap();
    assert_eq!((stale.count, stale.unknown.len()), (0, 1));
    h.ack("s", &second).await;

    // A late ack (deadline passed, not yet redelivered) still counts.
    h.publish("t", json!("late")).await;
    let d = h.pull("s", 10).await;
    h.clock.advance(Duration::from_secs(5));
    h.svc.run_maintenance().await.unwrap();
    h.ack("s", &d).await;
    h.clock.advance(Duration::from_secs(60));
    assert!(h.pull("s", 10).await.is_empty());
    let m = h.svc.subscription_metrics(&admin(), "s").await.unwrap();
    assert_eq!(m.expired_total, 2);
}

#[tokio::test]
async fn messages_are_dead_lettered_after_max_attempts_with_their_history() {
    let h = harness().await;
    h.topic("t").await;
    h.topic("t-dead").await;
    h.sub("dead-reader", "t-dead").await;
    h.sub_with(NewSubscription {
        max_delivery_attempts: Some(3),
        backoff_base_ms: Some(0),
        dead_letter_topic: Some("t-dead".into()),
        ..sub("s", "t")
    })
    .await;
    let id = h
        .svc
        .publish(&admin(), "t", vec![NewMessage::new(json!({"x": 1})).attr("kind", "bad")])
        .await
        .unwrap()[0]
        .message_id
        .clone();
    for attempt in 1..=3 {
        let got = h.pull("s", 10).await;
        assert_eq!(got.len(), 1, "attempt {attempt}");
        assert_eq!(got[0].delivery_attempt, attempt);
        let r = h.svc.nack(&admin(), "s", &[got[0].ack_id.clone()]).await.unwrap();
        assert_eq!(r.count, 1);
    }
    h.clock.advance(Duration::from_secs(60));
    assert!(h.pull("s", 10).await.is_empty(), "dead-lettered, not redelivered");

    let dl = h.svc.dead_letters(&admin(), "s", None, 10).await.unwrap().items;
    assert_eq!(dl.len(), 1);
    assert_eq!((dl[0].message_id.as_str(), dl[0].attempts), (id.as_str(), 3));
    assert_eq!(dl[0].history.len(), 3);
    assert!(dl[0].history.iter().all(|a| a.outcome.as_deref() == Some("nacked")));
    assert_eq!(dl[0].dead_letter_topic.as_deref(), Some("t-dead"));

    let copy = h.pull("dead-reader", 10).await;
    assert_eq!(copy.len(), 1);
    let m = &copy[0].message;
    assert_eq!(m.data, json!({"x": 1}));
    assert_eq!(m.attributes["kind"], "bad");
    assert_eq!(m.attributes["forgebase.dead_letter.subscription"], "s");
    assert_eq!(m.attributes["forgebase.dead_letter.message_id"], id);
    assert_eq!(m.delivery_history.as_ref().map(Vec::len), Some(3));
    assert_eq!(Some(m.id.clone()), dl[0].forwarded_message_id);

    let metrics = h.svc.subscription_metrics(&admin(), "s").await.unwrap();
    assert_eq!((metrics.dead_lettered_total, metrics.dead_letters, metrics.nacked_total), (1, 1, 3));

    // Missed deadlines count too.
    h.sub_with(NewSubscription {
        max_delivery_attempts: Some(2),
        ack_deadline_ms: Some(1_000),
        backoff_base_ms: Some(0),
        ..sub("slow", "t")
    })
    .await;
    h.publish("t", json!("slow")).await;
    for _ in 0..2 {
        assert_eq!(h.pull("slow", 10).await.len(), 1);
        h.clock.advance(Duration::from_millis(1_001));
    }
    assert!(h.pull("slow", 10).await.is_empty());
    let dl = h.svc.dead_letters(&admin(), "slow", None, 10).await.unwrap().items;
    assert_eq!(dl.len(), 1);
    assert!(dl[0].reason.contains("expired"), "{}", dl[0].reason);
    assert!(dl[0].history.iter().all(|a| a.outcome.as_deref() == Some("expired")));
}

#[tokio::test]
async fn ordering_keys_deliver_in_publish_order_one_at_a_time() {
    let h = harness().await;
    h.topic("t").await;
    h.sub_with(NewSubscription {
        ordering: true,
        backoff_base_ms: Some(0),
        ..sub("ordered", "t")
    })
    .await;
    h.sub("unordered", "t").await;
    let plan = [("A", 1), ("B", 1), ("A", 2), ("B", 2), ("A", 3), ("C", 1)];
    let msgs = plan
        .iter()
        .map(|(k, n)| NewMessage::new(json!(format!("{k}{n}"))).ordering_key(k))
        .collect();
    h.svc.publish(&admin(), "t", msgs).await.unwrap();

    assert_eq!(h.pull("unordered", 10).await.len(), 6, "ordering off: all at once");

    let first = h.pull("ordered", 10).await;
    assert_eq!(datas(&first), vec![json!("A1"), json!("B1"), json!("C1")], "one per key");
    assert!(h.pull("ordered", 10).await.is_empty(), "the heads are outstanding");
    // Ack A1: A2 is next. Nack B1: B1 comes back before B2.
    h.ack("ordered", &first[0..1]).await;
    h.svc.nack(&admin(), "ordered", &[first[1].ack_id.clone()]).await.unwrap();
    let second = h.pull("ordered", 10).await;
    assert_eq!(datas(&second), vec![json!("B1"), json!("A2")]);
    h.ack("ordered", &second).await;
    h.ack("ordered", &first[2..3]).await;
    let third = h.pull("ordered", 10).await;
    assert_eq!(datas(&third), vec![json!("B2"), json!("A3")]);
    h.ack("ordered", &third).await;
    assert!(h.pull("ordered", 10).await.is_empty());
}

#[tokio::test]
async fn a_dedup_id_is_accepted_once_inside_the_window() {
    let h = harness().await;
    h.svc
        .create_topic(
            &admin(),
            NewTopic {
                name: "t".into(),
                dedup_window_ms: Some(60_000),
                ..NewTopic::default()
            },
        )
        .await
        .unwrap();
    h.sub("s", "t").await;
    let first = h
        .svc
        .publish(&admin(), "t", vec![NewMessage::new(json!(1)).dedup_id("x")])
        .await
        .unwrap();
    assert!(!first[0].duplicate);
    let again = h
        .svc
        .publish(
            &admin(),
            "t",
            vec![
                NewMessage::new(json!(2)).dedup_id("x"),
                NewMessage::new(json!(3)).dedup_id("y"),
                NewMessage::new(json!(4)).dedup_id("y"),
            ],
        )
        .await
        .unwrap();
    assert!(again[0].duplicate);
    assert_eq!(again[0].message_id, first[0].message_id);
    assert_eq!(again[0].seq, first[0].seq);
    assert!(!again[1].duplicate);
    assert!(again[2].duplicate, "a repeat inside one batch is a duplicate too");
    h.clock.advance(Duration::from_millis(60_001));
    let later = h
        .svc
        .publish(&admin(), "t", vec![NewMessage::new(json!(5)).dedup_id("x")])
        .await
        .unwrap();
    assert!(!later[0].duplicate, "outside the window it is new");
    assert_eq!(datas(&h.pull("s", 10).await), vec![json!(1), json!(3), json!(5)]);
    let t = h.svc.topic(&admin(), "t").await.unwrap();
    assert_eq!(t.published_total, 3);
}

#[tokio::test]
async fn publish_inside_a_transaction_is_sent_only_on_commit() {
    let h = harness().await;
    h.topic("t").await;
    h.sub("s", "t").await;
    let backend = h.data.backend().clone();

    let mut tx = backend.begin(TxMode::Normal).await.unwrap();
    h.svc
        .publish_in_tx(&mut *tx, &admin(), "t", vec![NewMessage::new(json!("rolled back"))])
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    h.svc.committed();
    h.svc.run_maintenance().await.unwrap();
    assert!(h.pull("s", 10).await.is_empty(), "a rolled-back publish sends nothing");
    assert_eq!(h.svc.topic_metrics(&admin(), "t").await.unwrap().messages, 0);

    let mut tx = backend.begin(TxMode::Normal).await.unwrap();
    h.svc
        .publish_in_tx(&mut *tx, &admin(), "t", vec![NewMessage::new(json!("committed"))])
        .await
        .unwrap();
    tx.commit().await.unwrap();
    h.svc.committed();
    assert_eq!(datas(&h.pull("s", 10).await), vec![json!("committed")]);
}

#[tokio::test]
async fn record_changes_are_relayed_from_the_realtime_outbox_once() {
    let h = harness().await;
    h.data
        .create_collection(
            &admin(),
            serde_json::from_value(json!({
                "name": "orders",
                "fields": [{"name": "code", "type": "text", "unique": true}]
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    h.data.create_record(&admin(), "orders", json!({"code": "before"})).await.unwrap();
    h.svc
        .create_topic(
            &admin(),
            NewTopic {
                name: "order-events".into(),
                source: Some(RecordSource {
                    collection: "orders".into(),
                    ops: vec!["create".into()],
                }),
                ..NewTopic::default()
            },
        )
        .await
        .unwrap();
    h.sub("s", "order-events").await;
    // Only an administrator may expose a collection's changes as a topic.
    let err = h
        .svc
        .create_topic(
            &user("u1"),
            NewTopic {
                name: "x".into(),
                source: Some(RecordSource {
                    collection: "orders".into(),
                    ops: vec![],
                }),
                ..NewTopic::default()
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, QueueError::Access(_)), "{err:?}");

    let rec = h.data.create_record(&admin(), "orders", json!({"code": "a1"})).await.unwrap();
    // A transaction that rolls back (duplicate code) leaves no outbox row and no message.
    assert!(
        h.data
            .create_records(&admin(), "orders", vec![json!({"code": "b1"}), json!({"code": "b1"})])
            .await
            .is_err()
    );
    let report = h.svc.run_maintenance().await.unwrap();
    assert_eq!(report.relayed, 1);
    assert_eq!(h.svc.run_maintenance().await.unwrap().relayed, 0, "relayed once");
    let got = h.pull("s", 10).await;
    assert_eq!(got.len(), 1, "the change made before the topic existed is not relayed");
    let m = &got[0].message;
    assert_eq!(m.data["op"], "create");
    assert_eq!(m.data["record"]["code"], "a1");
    assert_eq!(m.attributes["forgebase.collection"], "orders");
    let rid = rec["id"].as_str().unwrap();
    assert_eq!(m.attributes["forgebase.record_id"], rid);
    assert_eq!(m.ordering_key.as_deref(), Some(rid));
    // A source topic does not take publishes.
    let err = h.svc.publish(&admin(), "order-events", vec![NewMessage::new(json!(1))]).await.unwrap_err();
    assert_eq!(err.code(), "topic_has_source");
}

#[tokio::test]
async fn seek_replays_by_timestamp_and_snapshot() {
    let h = harness().await;
    h.topic("t").await;
    h.sub_with(NewSubscription {
        start: Some(StartPosition::Earliest),
        ..sub("s", "t")
    })
    .await;
    let t0 = h.svc.now();
    h.publish("t", json!(1)).await;
    h.clock.advance(Duration::from_secs(10));
    let t1 = h.svc.now();
    h.publish("t", json!(2)).await;
    h.publish("t", json!(3)).await;
    let all = h.pull("s", 10).await;
    h.ack("s", &all).await;
    assert!(h.pull("s", 10).await.is_empty());

    let r = h
        .svc
        .seek(
            &admin(),
            "s",
            SeekRequest {
                time: Some(iso(t1)),
                ..SeekRequest::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(r.restored, 0);
    let replay = h.pull("s", 10).await;
    assert_eq!(datas(&replay), vec![json!(2), json!(3)], "published from t1 on");
    h.ack("s", &replay).await;
    h.svc
        .seek(
            &admin(),
            "s",
            SeekRequest {
                time: Some(iso(t0)),
                ..SeekRequest::default()
            },
        )
        .await
        .unwrap();
    let replay = h.pull("s", 10).await;
    assert_eq!(datas(&replay), vec![json!(1), json!(2), json!(3)]);
    h.ack("s", &replay).await;

    // Snapshot: 4 acked, 5 outstanding.
    h.publish("t", json!(4)).await;
    h.publish("t", json!(5)).await;
    let got = h.pull("s", 10).await;
    h.ack("s", &got[0..1]).await;
    let snap = h
        .svc
        .create_snapshot(
            &admin(),
            NewSnapshot {
                name: "before-5".into(),
                subscription: "s".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(snap.unacked, 1);
    h.ack("s", &got[1..2]).await;
    h.publish("t", json!(6)).await;
    let r = h
        .svc
        .seek(
            &admin(),
            "s",
            SeekRequest {
                snapshot: Some("before-5".into()),
                ..SeekRequest::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(r.restored, 1);
    assert_eq!(datas(&h.pull("s", 10).await), vec![json!(5), json!(6)]);
    // Starting at a timestamp works for new subscriptions too.
    h.sub_with(NewSubscription {
        start: Some(StartPosition::Time(iso(t1))),
        ..sub("late", "t")
    })
    .await;
    assert_eq!(h.pull("late", 10).await.len(), 5, "2..6");
}

#[tokio::test]
async fn queued_and_leased_messages_survive_a_restart() {
    let h = harness().await;
    h.topic("t").await;
    h.sub_with(NewSubscription {
        ack_deadline_ms: Some(5_000),
        backoff_base_ms: Some(0),
        ..sub("s", "t")
    })
    .await;
    let a = h.publish("t", json!("a")).await;
    let b = h.publish("t", json!("b")).await;
    let leased = h.pull("s", 1).await;
    assert_eq!(ids(&leased), vec![a.clone()]);

    let h = h.restart().await;
    assert_eq!(h.svc.topics(&admin()).await.unwrap().len(), 1);
    let fresh = h.pull("s", 10).await;
    assert_eq!(ids(&fresh), vec![b], "never-delivered message is still queued");
    h.ack("s", &fresh).await;
    h.clock.advance(Duration::from_millis(5_001));
    let again = h.pull("s", 10).await;
    assert_eq!(ids(&again), vec![a], "the crashed consumer's lease expired: redelivered");
    assert_eq!(again[0].delivery_attempt, 2);
}

#[tokio::test]
async fn filters_select_messages_by_attribute() {
    let h = harness().await;
    h.topic("t").await;
    h.sub_with(NewSubscription {
        filter: Some(json!({"type": "order", "region": ["eu", "us"]})),
        ..sub("eu-us-orders", "t")
    })
    .await;
    let msgs = vec![
        NewMessage::new(json!(1)).attr("type", "order").attr("region", "eu"),
        NewMessage::new(json!(2)).attr("type", "order").attr("region", "asia"),
        NewMessage::new(json!(3)).attr("type", "refund").attr("region", "us"),
        NewMessage::new(json!(4)),
        NewMessage::new(json!(5)).attr("type", "order").attr("region", "us"),
    ];
    h.svc.publish(&admin(), "t", msgs).await.unwrap();
    h.svc.run_maintenance().await.unwrap();
    let m = h.svc.subscription_metrics(&admin(), "eu-us-orders").await.unwrap();
    assert_eq!(m.backlog_messages, 2, "filtered-out messages are not owed");
    assert_eq!(datas(&h.pull("eu-us-orders", 10).await), vec![json!(1), json!(5)]);
    let bad = h
        .svc
        .create_subscription(
            &admin(),
            NewSubscription {
                filter: Some(json!({"type": {"regex": "x"}})),
                ..sub("bad", "t")
            },
        )
        .await
        .unwrap_err();
    assert_eq!(bad.code(), "invalid_filter");
}

#[tokio::test]
async fn schema_size_and_attribute_rules_are_enforced_at_publish() {
    let h = harness().await;
    h.svc
        .create_topic(
            &admin(),
            NewTopic {
                name: "typed".into(),
                max_message_bytes: Some(200),
                schema: Some(json!({
                    "type": "object",
                    "required": ["id"],
                    "properties": {"id": {"type": "string"}, "qty": {"type": "integer", "minimum": 1}}
                })),
                ..NewTopic::default()
            },
        )
        .await
        .unwrap();
    let ok = h.svc.publish(&admin(), "typed", vec![NewMessage::new(json!({"id": "a", "qty": 2}))]).await;
    assert!(ok.is_ok());
    let err = h
        .svc
        .publish(&admin(), "typed", vec![NewMessage::new(json!({"id": 1}))])
        .await
        .unwrap_err();
    assert_eq!(err.code(), "schema_violation");
    assert!(err.to_string().contains("/id"), "{err}");
    let big = json!({"id": "x".repeat(300)});
    let err = h.svc.publish(&admin(), "typed", vec![NewMessage::new(big)]).await.unwrap_err();
    assert_eq!(err.code(), "message_too_large");
    let err = h
        .svc
        .publish(&admin(), "typed", vec![NewMessage::new(json!({"id": "a"})).attr("forgebase.op", "x")])
        .await
        .unwrap_err();
    assert_eq!(err.code(), "invalid_message");
    // A failing message rejects the whole batch.
    let err = h
        .svc
        .publish(
            &admin(),
            "typed",
            vec![NewMessage::new(json!({"id": "fine"})), NewMessage::new(json!({"qty": 1}))],
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "schema_violation");
    assert_eq!(h.svc.topic(&admin(), "typed").await.unwrap().published_total, 1);
    // Unsupported schema keywords are refused up front.
    let err = h
        .svc
        .update_topic(
            &admin(),
            "typed",
            TopicPatch {
                schema: Some(json!({"type": "string", "pattern": "^a"})),
                ..TopicPatch::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "invalid_schema");
}

#[tokio::test]
async fn retention_drops_old_and_excess_messages() {
    let h = harness().await;
    h.svc
        .create_topic(
            &admin(),
            NewTopic {
                name: "short".into(),
                retention_ms: Some(60_000),
                ..NewTopic::default()
            },
        )
        .await
        .unwrap();
    h.sub_with(NewSubscription {
        start: Some(StartPosition::Earliest),
        ..sub("s", "short")
    })
    .await;
    h.publish("short", json!("old")).await;
    h.clock.advance(Duration::from_secs(61));
    h.publish("short", json!("new")).await;
    let r = h.svc.run_maintenance().await.unwrap();
    assert_eq!(r.pruned, 1);
    assert_eq!(datas(&h.pull("s", 10).await), vec![json!("new")], "unacked but past retention: gone");

    h.svc
        .create_topic(
            &admin(),
            NewTopic {
                name: "small".into(),
                retention_bytes: Some(1024),
                ..NewTopic::default()
            },
        )
        .await
        .unwrap();
    let msgs = (0..20).map(|i| NewMessage::new(json!({"i": i, "pad": "x".repeat(80)}))).collect();
    h.svc.publish(&admin(), "small", msgs).await.unwrap();
    h.svc.run_maintenance().await.unwrap();
    let m = h.svc.topic_metrics(&admin(), "small").await.unwrap();
    assert!(m.bytes <= 1024 && m.messages >= 10, "{m:?}");
    let page = h.svc.messages(&admin(), "small", 0, 100).await.unwrap();
    assert_eq!(page.items.last().unwrap().data["i"], 19, "the newest are kept");
}

#[tokio::test]
async fn metrics_report_backlog_age_and_rates() {
    let h = harness().await;
    h.topic("t").await;
    h.sub("s", "t").await;
    for i in 0..5 {
        h.publish("t", json!(i)).await;
    }
    h.clock.advance(Duration::from_secs(3));
    let got = h.pull("s", 2).await;
    h.ack("s", &got[0..1]).await;
    let m = h.svc.subscription_metrics(&admin(), "s").await.unwrap();
    assert_eq!((m.backlog_messages, m.in_flight, m.ready, m.acked_total), (4, 1, 3, 1));
    assert!(m.oldest_unacked_age_ms.unwrap() >= 3_000);
    assert_eq!(m.acked_last_minute, 1);
    assert!(m.ack_rate_per_sec > 0.0);
    let t = h.svc.topic_metrics(&admin(), "t").await.unwrap();
    assert_eq!((t.messages, t.published_total, t.subscriptions), (5, 5, 1));
    assert_eq!(t.published_last_minute, 5);
    assert!(t.publish_rate_per_sec > 0.0);
}

#[tokio::test]
async fn publishes_are_metered_and_quotas_enforced() {
    let h = harness().await;
    h.topic("t").await;
    h.sub("s", "t").await;
    let who = "user:users/u1";
    h.grant(who, "engine:queue_publish").await;
    h.quota(who, &[("queue.messages", 3), ("queue.bytes", 1_000_000)]).await;
    let u = user("u1");
    h.svc
        .publish(&u, "t", vec![NewMessage::new(json!(1)), NewMessage::new(json!(2))])
        .await
        .unwrap();
    let err = h
        .svc
        .publish(&u, "t", vec![NewMessage::new(json!(3)), NewMessage::new(json!(4))])
        .await
        .unwrap_err();
    assert_eq!(err.code(), "quota_exceeded");
    assert_eq!(h.svc.topic(&admin(), "t").await.unwrap().published_total, 2, "a refusal publishes nothing");
    // Duplicates do not use quota.
    h.svc
        .publish(&u, "t", vec![NewMessage::new(json!(3)).dedup_id("d")])
        .await
        .unwrap();
    let dup = h
        .svc
        .publish(&u, "t", vec![NewMessage::new(json!(3)).dedup_id("d")])
        .await
        .unwrap();
    assert!(dup[0].duplicate);
    let err = h.svc.publish(&u, "t", vec![NewMessage::new(json!(5))]).await.unwrap_err();
    assert_eq!(err.code(), "quota_exceeded");
    // The transactional path checks the quota too.
    let backend = h.data.backend().clone();
    let mut tx = backend.begin(TxMode::Normal).await.unwrap();
    let err = h
        .svc
        .publish_in_tx(&mut *tx, &u, "t", vec![NewMessage::new(json!(6))])
        .await
        .unwrap_err();
    tx.rollback().await.unwrap();
    assert_eq!(err.code(), "quota_exceeded");
    // Administrators have no billing subject and are not metered.
    h.publish("t", json!("admin")).await;
    assert_eq!(h.pull("s", 10).await.len(), 4);
}

#[tokio::test]
async fn transactional_publishes_are_metered_after_commit() {
    let h = harness().await;
    h.topic("t").await;
    let who = "user:users/u2";
    h.grant(who, "engine:queue_publish").await;
    h.quota(who, &[("queue.messages", 1), ("queue.bytes", 1_000_000)]).await;
    let u = user("u2");
    let backend = h.data.backend().clone();
    let mut tx = backend.begin(TxMode::Normal).await.unwrap();
    h.svc
        .publish_in_tx(&mut *tx, &u, "t", vec![NewMessage::new(json!(1))])
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(h.svc.meter_pending().await.unwrap(), 1);
    assert_eq!(h.svc.meter_pending().await.unwrap(), 0, "metered once");
    let err = h.svc.publish(&u, "t", vec![NewMessage::new(json!(2))]).await.unwrap_err();
    assert_eq!(err.code(), "quota_exceeded", "the metered usage counts");
}

#[tokio::test]
async fn every_operation_is_authorised() {
    let h = harness().await;
    h.topic("t").await;
    h.sub("s", "t").await;
    let anon = AccessContext::anonymous();
    let err = h.svc.publish(&anon, "t", vec![NewMessage::new(json!(1))]).await.unwrap_err();
    assert_eq!(err.code(), "unauthenticated");
    let u = user("u3");
    let err = h.svc.publish(&u, "t", vec![NewMessage::new(json!(1))]).await.unwrap_err();
    assert_eq!(err.code(), "forbidden");
    h.grant("user:users/u3", "engine:queue_publish").await;
    h.grant("user:users/u3", "engine:queue_consume").await;
    h.svc.publish(&u, "t", vec![NewMessage::new(json!(1))]).await.unwrap();
    let got = h.svc.pull(&u, "s", PullRequest::default()).await.unwrap();
    assert_eq!(got.len(), 1);
    h.svc.ack(&u, "s", &[got[0].ack_id.clone()]).await.unwrap();
    for err in [
        h.svc.topics(&u).await.map(|_| ()).unwrap_err(),
        h.svc.create_topic(&u, NewTopic { name: "x".into(), ..NewTopic::default() }).await.map(|_| ()).unwrap_err(),
        h.svc.delete_subscription(&u, "s").await.unwrap_err(),
        h.svc.seek(&u, "s", SeekRequest { time: Some(iso(0)), ..SeekRequest::default() }).await.map(|_| ()).unwrap_err(),
    ] {
        assert_eq!(err.code(), "forbidden");
    }
}

#[tokio::test]
async fn long_poll_waits_for_a_publish() {
    let h = Arc::new(harness().await);
    h.topic("t").await;
    h.sub("s", "t").await;
    let waiter = {
        let h = h.clone();
        tokio::spawn(async move {
            let started = tokio::time::Instant::now();
            let got = h
                .svc
                .pull(
                    &admin(),
                    "s",
                    PullRequest {
                        wait_ms: Some(5_000),
                        ..PullRequest::default()
                    },
                )
                .await
                .unwrap();
            (got, started.elapsed())
        })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    h.publish("t", json!("wake")).await;
    let (got, waited) = waiter.await.unwrap();
    assert_eq!(datas(&got), vec![json!("wake")]);
    assert!(waited < Duration::from_secs(4), "woken by the publish, not the timeout ({waited:?})");
    // Nothing to deliver: returns after the wait.
    let started = tokio::time::Instant::now();
    let none = h
        .svc
        .pull(
            &admin(),
            "s",
            PullRequest {
                wait_ms: Some(200),
                ..PullRequest::default()
            },
        )
        .await
        .unwrap();
    assert!(none.is_empty());
    assert!(started.elapsed() >= Duration::from_millis(200));
}

#[tokio::test]
async fn deleting_a_topic_with_subscriptions_needs_force() {
    let h = harness().await;
    h.topic("t").await;
    h.sub("s", "t").await;
    h.publish("t", json!(1)).await;
    let err = h.svc.delete_topic(&admin(), "t", false).await.unwrap_err();
    assert_eq!(err.code(), "topic_has_subscriptions");
    h.svc.delete_topic(&admin(), "t", true).await.unwrap();
    assert_eq!(h.svc.subscription(&admin(), "s").await.unwrap_err().code(), "subscription_not_found");
    assert!(h.svc.topics(&admin()).await.unwrap().is_empty());
}
