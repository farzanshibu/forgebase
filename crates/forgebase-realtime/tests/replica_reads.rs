//! Realtime on a set with replicas (ADR-0029 §2.5 and §2.6, FB-169): the
//! outbox is always read on the primary (the tail and the replay are in the
//! "always the primary" column), and each page of events carries the
//! primary's commit position (`<set>/<lsn>`) sampled when the page was read,
//! so a client that reads after an event sees the record.
//!
//! Needs a PostgreSQL engine (`FORGEBASE_PG_ENGINE_DIR` or
//! `FORGEBASE_TEST_PG_ENGINE`); without one the test prints a skip. The
//! no-replica case (SQLite or an unrouted PostgreSQL) is checked on the
//! selected backend by the second test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use forgebase_data::schema::Collection;
use forgebase_data::{AccessContext, DataService, DefaultAuthorizer};
use forgebase_db::routing::testing::RoutedCluster;
use forgebase_db::routing::{CommitToken, Lsn};
use forgebase_realtime::{RealtimeConfig, RealtimeService, outbox};
use serde_json::json;

fn quiet() -> RealtimeConfig {
    RealtimeConfig {
        // The tests read the outbox by hand.
        dispatch: false,
        ..RealtimeConfig::default()
    }
}

#[tokio::test]
async fn outbox_pages_carry_the_commit_position_and_are_read_on_the_primary() {
    let Some(rc) = RoutedCluster::start().await else {
        eprintln!("skipped: no PostgreSQL engine configured");
        return;
    };
    let ctx = AccessContext::admin("root");
    let data = DataService::new(rc.backend(), Arc::new(DefaultAuthorizer)).await.unwrap();
    let rt = RealtimeService::start(data.clone(), None, quiet(), None).await.unwrap();
    data.create_collection(&ctx, Collection::base("things")).await.unwrap();
    rc.sync().await;
    rc.lag().await;
    // Written while the standby is behind: only the primary has the event.
    data.create_record(&ctx, "things", json!({"id": "t1"})).await.unwrap();
    let after_commit = rc.cluster.primary_lsn().await.unwrap();

    let before = rc.replica_reads();
    let page = outbox::read_after(&*rc.backend(), 0, 100).await.unwrap();
    assert_eq!(page.len(), 1, "the primary's outbox has the event the standby lacks");
    assert_eq!(rc.replica_reads(), before, "the outbox tail never reads a replica");
    let token: CommitToken = page[0].commit.as_deref().expect("a commit token").parse().unwrap();
    assert_eq!(token.set, "control");
    assert!(
        token.lsn >= Lsn(0) && token.lsn >= after_commit,
        "the sampled position {} is at or after the event's commit {after_commit}",
        token.lsn
    );
    // Every event of one page carries the same token.
    data.create_record(&ctx, "things", json!({"id": "t2"})).await.unwrap();
    let page = outbox::read_after(&*rc.backend(), 0, 100).await.unwrap();
    assert_eq!(page.len(), 2);
    assert_eq!(page[0].commit, page[1].commit);
    assert!(page[0].commit.is_some());
    // An empty page samples nothing.
    assert!(outbox::read_after(&*rc.backend(), 1_000_000, 100).await.unwrap().is_empty());
    rt.shutdown();
}

#[tokio::test]
async fn without_replicas_events_carry_no_token() {
    let test = forgebase_db::testing::test_backend().await;
    let backend: Arc<dyn forgebase_db::Backend> = Arc::new(test.database().clone());
    let ctx = AccessContext::admin("root");
    let data = DataService::new(backend.clone(), Arc::new(DefaultAuthorizer)).await.unwrap();
    let rt = RealtimeService::start(data.clone(), None, quiet(), None).await.unwrap();
    data.create_collection(&ctx, Collection::base("things")).await.unwrap();
    data.create_record(&ctx, "things", json!({"id": "t1"})).await.unwrap();
    let page = outbox::read_after(&*backend, 0, 100).await.unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].commit, None, "nothing to wait for without replicas");
    rt.shutdown();
}
