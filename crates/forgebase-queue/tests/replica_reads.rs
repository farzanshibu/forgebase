//! Replica reads in the queue (ADR-0029 §2.5, FB-169): the message browser,
//! dead letters, topic and subscription metrics and the server-wide backlog
//! read a lagging standby when the request allows it. Pulls (claims), acks,
//! nacks, deadline changes, publishes and every write read the primary.
//!
//! Needs a PostgreSQL engine (`FORGEBASE_PG_ENGINE_DIR` or
//! `FORGEBASE_TEST_PG_ENGINE`); without one each test prints a skip.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use forgebase_auth::crypto::Keyring;
use forgebase_data::{AccessContext, DataService, DefaultAuthorizer};
use forgebase_db::routing::testing::RoutedCluster;
use forgebase_db::routing::{Consistency, ReadAfter, RequestScope, scope};
use forgebase_queue::{NewMessage, NewSubscription, NewTopic, PullRequest, QueueService, QueueSettings};
use serde_json::json;

struct Env {
    rc: RoutedCluster,
    q: QueueService,
    ctx: AccessContext,
}

fn msg(n: i64) -> Vec<NewMessage> {
    vec![NewMessage::new(json!({ "n": n }))]
}

/// Topic `t` and subscription `s` with message 1 (fanned out into the
/// subscription) replayed on the standby; then replay is paused and message 2
/// is published and fanned out: the standby holds one message, the primary two.
async fn env() -> Option<Env> {
    let Some(rc) = RoutedCluster::start().await else {
        eprintln!("skipped: no PostgreSQL engine configured");
        return None;
    };
    let ctx = AccessContext::admin("root");
    let data = DataService::new(rc.backend(), Arc::new(DefaultAuthorizer))
        .await
        .unwrap();
    let q = QueueService::with_clock(
        data,
        Keyring::from_master(b"queue-tests-master-key-0123456789"),
        QueueSettings {
            // The tests drive fan-out by hand.
            poll_interval: Duration::from_secs(3600),
            ..QueueSettings::default()
        },
        Arc::new(forgebase_jobs::SystemClock),
    );
    q.create_topic(
        &ctx,
        NewTopic {
            name: "t".into(),
            ..NewTopic::default()
        },
    )
    .await
    .unwrap();
    q.create_subscription(
        &ctx,
        NewSubscription {
            name: "s".into(),
            topic: "t".into(),
            ..NewSubscription::default()
        },
    )
    .await
    .unwrap();
    q.publish(&ctx, "t", msg(1)).await.unwrap();
    q.run_maintenance().await.unwrap();
    rc.sync().await;
    rc.lag().await;
    q.publish(&ctx, "t", msg(2)).await.unwrap();
    q.run_maintenance().await.unwrap();
    Some(Env { rc, q, ctx })
}

#[tokio::test]
async fn queue_browser_and_metrics_use_a_lagging_replica_when_allowed() {
    let Some(e) = env().await else { return };
    let (q, ctx, rc) = (&e.q, &e.ctx, &e.rc);

    // message browser
    let before = rc.replica_reads();
    let page = q.messages(ctx, "t", 0, 100).await.unwrap();
    assert_eq!(page.items.len(), 1, "the browser read the lagging replica");
    assert!(rc.replica_reads() > before);

    // topic metrics (messages count and rates)
    let before = rc.replica_reads();
    assert_eq!(q.topic_metrics(ctx, "t").await.unwrap().messages, 1);
    assert!(rc.replica_reads() >= before + 2);

    // subscription metrics
    let before = rc.replica_reads();
    let m = q.subscription_metrics(ctx, "s").await.unwrap();
    assert_eq!(m.backlog_messages, 1);
    assert!(rc.replica_reads() >= before + 2);

    // dead letters
    let before = rc.replica_reads();
    assert!(q.dead_letters(ctx, "s", None, 10).await.unwrap().items.is_empty());
    assert!(rc.replica_reads() > before);

    // server-wide backlog
    let before = rc.replica_reads();
    assert_eq!(q.backlog().await.unwrap().messages, 1);
    assert!(rc.replica_reads() > before);
    assert_eq!(rc.fallbacks(), 0);
}

#[tokio::test]
async fn claims_acks_and_writes_read_the_primary() {
    let Some(e) = env().await else { return };
    let (q, ctx, rc) = (&e.q, &e.ctx, &e.rc);
    let before = rc.replica_reads();

    // A pull claims from the primary: it finds both messages.
    let got = q
        .pull(
            ctx,
            "s",
            PullRequest {
                max_messages: Some(10),
                ..PullRequest::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(got.len(), 2, "claims see the primary");
    let ids: Vec<String> = got.iter().map(|m| m.ack_id.clone()).collect();
    assert_eq!(q.ack(ctx, "s", &ids[..1]).await.unwrap().count, 1);
    q.nack(ctx, "s", &ids[1..]).await.unwrap();
    q.publish(ctx, "t", msg(3)).await.unwrap();
    q.run_maintenance().await.unwrap();
    assert_eq!(
        rc.replica_reads(),
        before,
        "claims, acks, nacks and publishes read the primary"
    );
    assert_eq!(rc.fallbacks(), 0);
}

#[tokio::test]
async fn strong_reads_and_a_requests_own_writes_see_the_primary() {
    let Some(e) = env().await else { return };
    let (q, ctx, rc) = (&e.q, &e.ctx, &e.rc);

    let strong = RequestScope::new(Consistency::Strong, ReadAfter::new());
    let before = rc.replica_reads();
    let page = scope::scoped(strong, q.messages(ctx, "t", 0, 100)).await.unwrap();
    assert_eq!(page.items.len(), 2);
    assert_eq!(rc.replica_reads(), before);

    let own = RequestScope::new(Consistency::Session, ReadAfter::new());
    let page = scope::scoped(own, async {
        q.publish(ctx, "t", msg(9)).await.unwrap();
        q.messages(ctx, "t", 0, 100).await.unwrap()
    })
    .await;
    assert_eq!(page.items.len(), 3, "read-your-writes");
}
