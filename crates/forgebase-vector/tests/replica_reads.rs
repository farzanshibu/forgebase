//! Replica reads in the vector service (ADR-0029 §2.5, FB-169): get, list,
//! statistics, hybrid keyword search and, on pgvector, kNN search read a
//! lagging standby when the request allows it. Writes, DDL, index builds and
//! the portable engine's kNN (its in-process index follows the primary)
//! read the primary.
//!
//! Needs a PostgreSQL engine (`FORGEBASE_PG_ENGINE_DIR` or
//! `FORGEBASE_TEST_PG_ENGINE`); without one each test prints a skip. With
//! pgvector in the engine the search tests exercise pgvector, else the
//! portable engine (both are checked: the expected side differs).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use common::{admin, create, field, item, query};
use forgebase_core::config::{ExtensionMode, PostgresExtensions};
use forgebase_data::{AccessContext, DataService, DefaultAuthorizer};
use forgebase_db::ExtensionSetup;
use forgebase_db::routing::testing::RoutedCluster;
use forgebase_db::routing::{Consistency, ReadAfter, RequestScope, scope};
use forgebase_vector::model::*;
use forgebase_vector::{VectorConfig, VectorError, VectorService};
use serde_json::json;

struct Env {
    rc: RoutedCluster,
    svc: VectorService,
    ctx: AccessContext,
    pgvector: bool,
    _dir: tempfile::TempDir,
}

fn text_item(id: &str, v: &[f32], text: &str) -> UpsertItem {
    UpsertItem {
        text: Some(text.into()),
        ..item(id, "v", v, json!({}))
    }
}

/// Collection `c` (field `v`, three dimensions) with item `i1` replayed on
/// the standby; then replay is paused and `i2` is written.
async fn env() -> Option<Env> {
    let Some(rc) = RoutedCluster::start().await else {
        eprintln!("skipped: no PostgreSQL engine configured");
        return None;
    };
    let dir = tempfile::tempdir().unwrap();
    // The server's start-up extension setup: pgvector when the engine has it.
    let modes = PostgresExtensions {
        vector: if std::env::var("FORGEBASE_TEST_PGVECTOR").as_deref() == Ok("off") {
            ExtensionMode::Off
        } else {
            ExtensionMode::Auto
        },
        graph: ExtensionMode::Off,
        queue: ExtensionMode::Off,
        cron: ExtensionMode::Off,
        trigram: ExtensionMode::Off,
    };
    rc.handle
        .setup_extensions(&ExtensionSetup {
            modes,
            ..ExtensionSetup::default()
        })
        .await
        .unwrap();
    let data = DataService::new(rc.backend(), Arc::new(DefaultAuthorizer)).await.unwrap();
    let layout = forgebase_core::DataLayout::new(dir.path().join("data"));
    layout.ensure().unwrap();
    let svc = VectorService::open(data, layout.indexes_dir(), VectorConfig::default())
        .await
        .unwrap();
    let pgvector = svc.storage_engine().await == VectorEngine::Pgvector;
    let ctx = admin();
    create(&svc, &ctx, "c", vec![field("v", 3, Metric::Cosine)], Some(10_000)).await;
    svc.upsert(&ctx, "c", vec![text_item("i1", &[1.0, 0.0, 0.0], "alpha one")])
        .await
        .unwrap();
    rc.sync().await;
    rc.lag().await;
    svc.upsert(&ctx, "c", vec![text_item("i2", &[0.0, 1.0, 0.0], "alpha two")])
        .await
        .unwrap();
    Some(Env {
        rc,
        svc,
        ctx,
        pgvector,
        _dir: dir,
    })
}

fn hybrid_text(text: &str) -> HybridRequest {
    serde_json::from_value(json!({"text": text})).unwrap()
}

#[tokio::test]
async fn vector_reads_use_a_lagging_replica_when_allowed() {
    let Some(e) = env().await else { return };
    let (svc, ctx, rc) = (&e.svc, &e.ctx, &e.rc);

    // get
    let before = rc.replica_reads();
    assert!(svc.get_item(ctx, "c", "i1").await.is_ok());
    assert!(matches!(
        svc.get_item(ctx, "c", "i2").await,
        Err(VectorError::ItemNotFound(_))
    ));
    assert!(rc.replica_reads() >= before + 2, "get read the lagging replica");

    // list (collections with their item counts)
    let before = rc.replica_reads();
    let list = svc.list_collections(ctx).await.unwrap();
    assert_eq!(list[0].items, 1);
    assert!(rc.replica_reads() > before);
    let before = rc.replica_reads();
    assert_eq!(svc.get_collection(ctx, "c").await.unwrap().items, 1);
    assert!(rc.replica_reads() > before);

    // stats
    let before = rc.replica_reads();
    assert_eq!(svc.stats(ctx, "c").await.unwrap().items, 1);
    assert!(rc.replica_reads() > before);

    // hybrid, keyword only: store reads, on every engine
    let before = rc.replica_reads();
    let hits = svc.hybrid(ctx, "c", hybrid_text("alpha")).await.unwrap().hits;
    assert_eq!(hits.iter().map(|h| h.id.as_str()).collect::<Vec<_>>(), ["i1"]);
    assert!(rc.replica_reads() > before);
    assert_eq!(rc.fallbacks(), 0);
}

#[tokio::test]
async fn knn_search_follows_the_engine() {
    let Some(e) = env().await else { return };
    let (svc, ctx, rc) = (&e.svc, &e.ctx, &e.rc);
    let q = query("v", &[0.0, 1.0, 0.0], 5);

    let before = rc.replica_reads();
    let hits = svc.search(ctx, "c", q.clone()).await.unwrap().hits;
    let ids: Vec<_> = hits.iter().map(|h| h.id.as_str()).collect();
    if e.pgvector {
        assert_eq!(ids, ["i1"], "pgvector: the search read the lagging replica");
        assert!(rc.replica_reads() > before);
    } else {
        // The in-process index follows the primary: so do its store reads.
        assert_eq!(ids, ["i2", "i1"], "portable: the primary");
        assert_eq!(rc.replica_reads(), before);
    }

    // hybrid with a vector: the same rule
    let before = rc.replica_reads();
    let h: HybridRequest = serde_json::from_value(json!({"vector": [0.0, 1.0, 0.0], "text": "alpha"})).unwrap();
    let hits = svc.hybrid(ctx, "c", h).await.unwrap().hits;
    if e.pgvector {
        assert_eq!(hits.len(), 1);
        assert!(rc.replica_reads() > before);
    } else {
        assert_eq!(hits.len(), 2);
        assert_eq!(rc.replica_reads(), before);
    }

    // Strong consistency: the primary, whatever the engine.
    let strong = RequestScope::new(Consistency::Strong, ReadAfter::new());
    let before = rc.replica_reads();
    let hits = scope::scoped(strong, svc.search(ctx, "c", q)).await.unwrap().hits;
    assert_eq!(hits.len(), 2);
    assert_eq!(rc.replica_reads(), before);
}

#[tokio::test]
async fn writes_ddl_and_builds_never_read_a_replica() {
    let Some(e) = env().await else { return };
    let (svc, ctx, rc) = (&e.svc, &e.ctx, &e.rc);
    let before = rc.replica_reads();

    svc.upsert(ctx, "c", vec![text_item("i3", &[0.0, 0.0, 1.0], "gamma")])
        .await
        .unwrap();
    assert_eq!(svc.delete_item(ctx, "c", "i3").await.ok(), Some(()));
    create(svc, ctx, "d", vec![field("w", 3, Metric::Cosine)], None).await;
    svc.rebuild(ctx, "c", None).await.unwrap();
    svc.delete_collection(ctx, "d").await.unwrap();
    assert_eq!(rc.replica_reads(), before, "writes, DDL and rebuilds read the primary");
    assert_eq!(rc.fallbacks(), 0);
}

#[tokio::test]
async fn a_requests_own_writes_are_read_back() {
    let Some(e) = env().await else { return };
    let (svc, ctx) = (&e.svc, &e.ctx);
    let own = RequestScope::new(Consistency::Session, ReadAfter::new());
    let got = scope::scoped(own, async {
        svc.upsert(ctx, "c", vec![text_item("i4", &[0.0, 0.0, 1.0], "delta")])
            .await
            .unwrap();
        svc.get_item(ctx, "c", "i4").await
    })
    .await;
    assert!(got.is_ok(), "read-your-writes");
}
