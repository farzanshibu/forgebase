//! Live tests for durable topics and queues (FB-63) through the SDK.

mod common;

use std::time::Duration;

use common::Server;
use forgebase_sdk::{NewMessage, NewSubscription, NewTopic, PullOptions, SeekTo, StreamOptions};
use serde_json::json;

#[tokio::test(flavor = "multi_thread")]
async fn publish_pull_ack_replay_and_dead_letters() {
    let Some(s) = Server::start() else { return };
    let q = s.admin().await.queues();

    q.create_topic(&NewTopic::new("orders")).await.expect("topic");
    let mut sub = NewSubscription::new("billing", "orders");
    sub.start = Some(json!("earliest"));
    q.create_subscription(&sub).await.expect("subscription");

    let first = q
        .publish(
            "orders",
            &NewMessage::new(json!({"id": 1})).attr("kind", "rush").dedup_id("o-1"),
        )
        .await
        .expect("publish");
    assert!(!first.duplicate);
    let again = q
        .publish("orders", &NewMessage::new(json!({"id": 1})).dedup_id("o-1"))
        .await
        .expect("publish again");
    assert!(again.duplicate);
    assert_eq!(again.message_id, first.message_id);
    q.publish_batch(
        "orders",
        &[NewMessage::new(json!({"id": 2})), NewMessage::new(json!({"id": 3}))],
    )
    .await
    .expect("batch");

    let page = q.messages("orders", None, Some(10)).await.expect("log");
    assert_eq!(page.items.len(), 3);

    let got = q
        .pull(
            "billing",
            &PullOptions {
                max_messages: Some(10),
                wait_ms: Some(1000),
                ..PullOptions::default()
            },
        )
        .await
        .expect("pull");
    assert_eq!(got.len(), 3);
    assert_eq!(got[0].message.attributes.get("kind").map(String::as_str), Some("rush"));
    let ids: Vec<String> = got.iter().map(|m| m.ack_id.clone()).collect();
    assert_eq!(q.ack("billing", &ids).await.expect("ack").count, 3);
    assert!(
        q.pull("billing", &PullOptions::default())
            .await
            .expect("empty")
            .is_empty()
    );

    // Replay from the start puts everything back.
    let seek = q
        .seek("billing", &SeekTo::Time("2000-01-01T00:00:00Z".into()))
        .await
        .expect("seek");
    assert_eq!(seek.restored, 0); // a time seek moves the cursor; only snapshots restore ack state
    let again = q
        .pull("billing", &PullOptions::default())
        .await
        .expect("pull after seek");
    assert_eq!(again.len(), 3);
    q.nack("billing", &[again[0].ack_id.clone()]).await.expect("nack");
    assert!(
        q.dead_letters("billing", None, Some(10))
            .await
            .expect("dead letters")
            .is_empty()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn streaming_pull_delivers_and_acks() {
    let Some(s) = Server::start() else { return };
    let q = s.admin().await.queues();
    q.create_topic(&NewTopic::new("events")).await.expect("topic");
    q.create_subscription(&NewSubscription::new("worker", "events"))
        .await
        .expect("subscription");

    let mut stream = q.subscribe("worker", &StreamOptions::default()).await.expect("stream");
    q.publish("events", &NewMessage::new(json!({"n": 7})))
        .await
        .expect("publish");
    let m = tokio::time::timeout(Duration::from_secs(10), stream.next())
        .await
        .expect("delivery in time")
        .expect("stream")
        .expect("a message");
    assert_eq!(m.message.data["n"], 7);
    assert_eq!(stream.ack(&m).await.expect("ack").count, 1);
    let backlog = q.subscription_metrics("worker").await.expect("metrics");
    assert_eq!(backlog["backlog_messages"], 0);
}
