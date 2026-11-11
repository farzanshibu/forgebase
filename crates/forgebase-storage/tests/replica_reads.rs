//! Replica reads in the storage service (ADR-0029 §2.5, FB-169): bucket and
//! object metadata reads (listings, versions, head, usage, statistics) read a
//! lagging standby when the request allows it. Authorisation state (the
//! bucket row, ACLs), object content, writes and signed-URL issue read the
//! primary.
//!
//! Needs a PostgreSQL engine (`FORGEBASE_PG_ENGINE_DIR` or
//! `FORGEBASE_TEST_PG_ENGINE`); without one each test prints a skip.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use forgebase_core::DataLayout;
use forgebase_data::{AccessContext, DataService, DefaultAuthorizer};
use forgebase_db::routing::testing::RoutedCluster;
use forgebase_db::routing::{Consistency, ReadAfter, RequestScope, scope};
use forgebase_entitlements::ManualClock;
use forgebase_storage::{
    BucketInput, LocalDriver, PutRequest, StorageDriver, StorageError, StorageService, StorageSettings, body_of,
};

struct Env {
    rc: RoutedCluster,
    storage: StorageService,
    ctx: AccessContext,
    _tmp: tempfile::TempDir,
}

fn put(bucket: &str, key: &str, data: &[u8]) -> PutRequest {
    PutRequest {
        bucket: bucket.into(),
        key: key.into(),
        content_type: None,
        metadata: Default::default(),
        expected_sha256: None,
        content_length: None,
        body: body_of(data.to_vec()),
    }
}

fn bucket(name: &str, versioning: bool) -> BucketInput {
    BucketInput {
        name: name.into(),
        versioning,
        ..BucketInput::default()
    }
}

/// Bucket `docs` (versioned) with `a.txt` (3 bytes) replayed on the standby;
/// then replay is paused and `a.txt` gets a second version (6 bytes), `b.txt`
/// and bucket `extra` are written.
async fn env() -> Option<Env> {
    let Some(rc) = RoutedCluster::start().await else {
        eprintln!("skipped: no PostgreSQL engine configured");
        return None;
    };
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().join("data"));
    layout.ensure().unwrap();
    let backend = rc.backend();
    let data = DataService::new(backend.clone(), Arc::new(DefaultAuthorizer)).await.unwrap();
    let driver: Arc<dyn StorageDriver> = Arc::new(LocalDriver::new(layout.storage_dir()));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap())
        .unwrap();
    let storage = StorageService::open(
        backend,
        data,
        driver,
        Arc::new(ManualClock::at(now)),
        StorageSettings::default(),
        forgebase_auth::crypto::Keyring::from_master(&[9u8; 32]),
    )
    .await
    .unwrap();
    let ctx = AccessContext::admin("root");
    storage.create_bucket(&ctx, bucket("docs", true)).await.unwrap();
    storage.put_object(&ctx, put("docs", "a.txt", b"one")).await.unwrap();
    rc.sync().await;
    rc.lag().await;
    storage.put_object(&ctx, put("docs", "a.txt", b"three!")).await.unwrap();
    storage.put_object(&ctx, put("docs", "b.txt", b"bee")).await.unwrap();
    storage.create_bucket(&ctx, bucket("extra", false)).await.unwrap();
    Some(Env {
        rc,
        storage,
        ctx,
        _tmp: tmp,
    })
}

#[tokio::test]
async fn storage_metadata_reads_use_a_lagging_replica_when_allowed() {
    let Some(e) = env().await else { return };
    let (s, ctx, rc) = (&e.storage, &e.ctx, &e.rc);

    // object listing
    let before = rc.replica_reads();
    let page = s.list_objects(ctx, "docs", "", None, 100, None).await.unwrap();
    let keys: Vec<_> = page.objects.iter().map(|o| o.key.as_str()).collect();
    assert_eq!(keys, ["a.txt"], "listing read the lagging replica");
    assert_eq!(page.objects[0].size, 3);
    assert!(rc.replica_reads() > before);

    // versions
    let before = rc.replica_reads();
    assert_eq!(s.list_versions(ctx, "docs", "a.txt").await.unwrap().len(), 1);
    assert!(rc.replica_reads() > before);

    // head: the replica has the first version and no b.txt
    let before = rc.replica_reads();
    assert_eq!(s.head_object(ctx, "docs", "a.txt", None).await.unwrap().size, 3);
    assert!(matches!(
        s.head_object(ctx, "docs", "b.txt", None).await,
        Err(StorageError::NotFound(_))
    ));
    assert_eq!(rc.replica_reads(), before + 2);

    // bucket listing, usage and statistics
    let before = rc.replica_reads();
    let names: Vec<_> = s.list_buckets(ctx).await.unwrap().into_iter().map(|b| b.name).collect();
    assert_eq!(names, ["docs"], "bucket `extra` is not on the standby yet");
    assert!(rc.replica_reads() > before);
    let before = rc.replica_reads();
    let usage = s.bucket_usage(ctx, "docs").await.unwrap();
    assert_eq!(usage.objects, 1);
    assert!(rc.replica_reads() > before);
    let before = rc.replica_reads();
    let stats = s.stats(ctx).await.unwrap();
    assert_eq!(stats.buckets, 1);
    assert!(rc.replica_reads() > before);
    assert_eq!(rc.fallbacks(), 0);
}

#[tokio::test]
async fn object_content_authorisation_and_writes_read_the_primary() {
    let Some(e) = env().await else { return };
    let (s, ctx, rc) = (&e.storage, &e.ctx, &e.rc);
    let before = rc.replica_reads();

    // The content read finds the newest version, which the standby lacks.
    let read = s.get_object(ctx, "docs", "a.txt", None, None).await.unwrap();
    assert_eq!(read.size, 6);
    // Bucket rows and ACLs decide access: primary (`extra` is not replayed).
    assert!(s.get_bucket(ctx, "extra").await.is_ok());
    assert!(s.get_acl(ctx, "extra").await.is_ok());
    // Writes.
    s.put_object(ctx, put("extra", "c.txt", b"see")).await.unwrap();
    s.delete_object(ctx, "extra", "c.txt", None).await.unwrap();
    assert_eq!(rc.replica_reads(), before, "content, ACLs and writes read the primary");
    assert_eq!(rc.fallbacks(), 0);
}

#[tokio::test]
async fn strong_reads_and_a_requests_own_writes_see_the_primary() {
    let Some(e) = env().await else { return };
    let (s, ctx, rc) = (&e.storage, &e.ctx, &e.rc);

    let strong = RequestScope::new(Consistency::Strong, ReadAfter::new());
    let before = rc.replica_reads();
    let page = scope::scoped(strong, s.list_objects(ctx, "docs", "", None, 100, None))
        .await
        .unwrap();
    assert_eq!(page.objects.len(), 2);
    assert_eq!(rc.replica_reads(), before);

    let own = RequestScope::new(Consistency::Session, ReadAfter::new());
    let head = scope::scoped(own, async {
        s.put_object(ctx, put("docs", "d.txt", b"dee")).await.unwrap();
        s.head_object(ctx, "docs", "d.txt", None).await
    })
    .await;
    assert!(head.is_ok(), "read-your-writes");
}
