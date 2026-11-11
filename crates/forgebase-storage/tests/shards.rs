//! Storage over database sets (ADR-0029 §3.2, §3.5, FB-172): a tenant
//! bucket and its objects, ACL entries, uploads and parts live on the
//! tenant's set, instance-wide buckets on control; signed URLs find the set
//! from the tenant they carry; blob reference counts are per set while the
//! blob files are shared, so the reconciler keeps a file any set still
//! references and leaves the copies of a moving tenant alone; a frozen
//! tenant's calls fail with `409 tenant_moving` after the freeze timeout.
//!
//! The single-set test runs on SQLite and PostgreSQL; the multi-set test
//! needs a PostgreSQL engine (`FORGEBASE_PG_ENGINE_DIR` or
//! `FORGEBASE_TEST_PG_ENGINE`) and skips without one.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use common::{Env, admin, put, read_all};
use forgebase_core::ErrorCode;
use forgebase_data::{AccessContext, DataService, DefaultAuthorizer};
use forgebase_db::shard_testing::Sharded;
use forgebase_db::topology::PlacementState;
use forgebase_db::{Backend, Database, DbRouter, RouterError, RouterOptions, Statement, TxMode, Value, shards};
use forgebase_entitlements::ManualClock;
use forgebase_storage::{
    AclEntry, BucketInput, CompleteInput, LocalDriver, Permission, SignRequest, SignedMethod, SignedParams,
    StorageDriver, StorageError, StorageService, StorageSettings, UploadInput, body_of,
};

fn bucket(name: &str) -> BucketInput {
    BucketInput {
        name: name.into(),
        ..BucketInput::default()
    }
}

fn sign(key: &str, method: SignedMethod) -> SignRequest {
    SignRequest {
        key: key.into(),
        method: Some(method),
        expires_in: Some(600),
        version_id: None,
        download_as: None,
        content_type: None,
    }
}

/// The values of a signed URL as the HTTP route reads them.
fn params_of(url: &str, method: SignedMethod) -> SignedParams {
    let (path, query) = url.split_once('?').unwrap();
    let rest = path.strip_prefix("/api/v1/storage/signed/").unwrap();
    let (bucket, key) = rest.split_once('/').unwrap();
    let q: BTreeMap<String, String> = url::form_urlencoded::parse(query.as_bytes()).into_owned().collect();
    SignedParams::from_request(method, bucket, key, &q).unwrap()
}

fn sha(data: &[u8]) -> String {
    forgebase_storage::util::sha256_hex(data)
}

fn blob_key(sha: &str) -> String {
    format!("blobs/{}/{sha}", &sha[..2])
}

async fn strings(db: &Database, sql: &str) -> Vec<String> {
    db.fetch_all(&Statement::new(sql, vec![]))
        .await
        .unwrap()
        .rows
        .iter()
        .map(|r| r[0].as_str().unwrap().to_owned())
        .collect()
}

async fn count(db: &Database, sql: &str, params: Vec<Value>) -> i64 {
    db.fetch_all(&Statement::new(sql, params))
        .await
        .unwrap()
        .scalar()
        .and_then(Value::as_i64)
        .unwrap()
}

async fn exec(db: &Database, sql: &str, params: Vec<Value>) {
    let mut tx = db.begin(TxMode::Normal).await.unwrap();
    tx.execute(&Statement::new(sql, params)).await.unwrap();
    tx.commit().await.unwrap();
}

/// A storage service over control (the router is attached by the caller).
async fn open(control: &Database, driver: &Arc<dyn StorageDriver>, clock: &ManualClock) -> StorageService {
    let backend: Arc<dyn Backend> = Arc::new(control.clone());
    let data = DataService::new(backend.clone(), Arc::new(DefaultAuthorizer))
        .await
        .unwrap();
    StorageService::open(
        backend,
        data,
        driver.clone(),
        Arc::new(clock.clone()),
        StorageSettings {
            reconcile_grace_secs: 0,
            ..StorageSettings::default()
        },
        forgebase_auth::crypto::Keyring::from_master(&[9u8; 32]),
    )
    .await
    .unwrap()
}

async fn has_file(driver: &Arc<dyn StorageDriver>, key: &str) -> bool {
    driver.head(key).await.unwrap().is_some()
}

#[tokio::test]
async fn one_set_router_behaves_as_before() {
    let env = Env::new().await;
    env.storage.attach_router(DbRouter::single(env.db.clone()));
    let s = &env.storage;
    let ta = admin().with_tenant("tenant-a");

    s.create_bucket(&admin(), bucket("shared")).await.unwrap();
    s.create_bucket(&ta, bucket("files")).await.unwrap();
    assert!(matches!(
        s.create_bucket(&ta, bucket("shared")).await,
        Err(StorageError::Conflict {
            code: "bucket_exists",
            ..
        })
    ));
    s.put_object(&ta, put("files", "a.txt", b"aye")).await.unwrap();
    s.put_object(&ta, put("shared", "w.txt", b"wide")).await.unwrap();
    assert_eq!(
        strings(&env.db, "SELECT name FROM _forgebase_storage_buckets ORDER BY name").await,
        ["shared", "tenant-a/files"]
    );
    let names: Vec<_> = s
        .list_buckets(&ta)
        .await
        .unwrap()
        .into_iter()
        .map(|b| (b.name, b.tenant))
        .collect();
    assert_eq!(
        names,
        [("files".into(), Some("tenant-a".into())), ("shared".into(), None)]
    );
    let read = s.get_object(&ta, "files", "a.txt", None, None).await.unwrap();
    assert_eq!(read_all(read).await, b"aye");

    // A signed URL of the tenant bucket, used without any context.
    let url = s
        .sign_url(&ta, "files", sign("a.txt", SignedMethod::Get))
        .await
        .unwrap();
    let got = s
        .get_signed(&params_of(&url.url, SignedMethod::Get), None)
        .await
        .unwrap();
    assert_eq!(read_all(got).await, b"aye");

    // A multipart session.
    let up = s
        .create_upload(
            &ta,
            "files",
            UploadInput {
                key: "m.bin".into(),
                ..UploadInput::default()
            },
        )
        .await
        .unwrap();
    s.upload_part(&ta, &up.id, 1, None, body_of(b"part".to_vec()))
        .await
        .unwrap();
    assert_eq!(s.get_upload(&admin(), &up.id).await.unwrap().parts.len(), 1);
    let done = s.complete_upload(&ta, &up.id, CompleteInput::default()).await.unwrap();
    assert_eq!(done.size, 4);

    let stats = s.stats(&admin()).await.unwrap();
    assert_eq!((stats.scope.as_str(), stats.buckets, stats.objects), ("engine", 2, 3));
    let stats = s.stats(&ta).await.unwrap();
    assert_eq!((stats.buckets, stats.objects), (1, 2));

    // The reconciler collects what nothing references any more.
    s.delete_object(&ta, "files", "a.txt", None).await.unwrap();
    let report = s.reconcile_now().await.unwrap();
    assert_eq!(report.unreferenced_blobs_deleted, 1, "{report:?}");
    assert!(!has_file(&env.driver, &blob_key(&sha(b"aye"))).await);
    s.delete_bucket(&ta, "files", true).await.unwrap();
    assert_eq!(
        strings(&env.db, "SELECT name FROM _forgebase_storage_buckets ORDER BY name").await,
        ["shared"]
    );
}

#[tokio::test]
async fn tenant_buckets_live_on_the_tenants_set() {
    let Some(mut sh) = Sharded::start().await else {
        eprintln!("skipped: no PostgreSQL engine configured");
        return;
    };
    let (set, shard) = sh.provision("s1").await;
    shards::activate_shard(
        &sh.router,
        &set,
        &shard,
        shards::ShardSetup {
            backups_dir: None,
            replay: None,
            control_conninfo: None,
        },
    )
    .await
    .unwrap();
    // tenant-b on the shard; tenant-a stays on control (no placement row).
    sh.router
        .place("tenant-b", &set.id, PlacementState::Active)
        .await
        .unwrap();
    let control = sh.control.clone();

    let tmp = tempfile::tempdir().unwrap();
    let driver: Arc<dyn StorageDriver> = Arc::new(LocalDriver::new(tmp.path().join("storage")));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap())
        .unwrap();
    let clock = ManualClock::at(now);
    let s = open(&control, &driver, &clock).await;
    s.attach_router(sh.router.clone());
    let root = admin();
    let ta = admin().with_tenant("tenant-a");
    let tb = admin().with_tenant("tenant-b");

    // --- Buckets: tenant b's on the shard, the others on control.
    s.create_bucket(&root, bucket("shared")).await.unwrap();
    s.create_bucket(&ta, bucket("files")).await.unwrap();
    s.create_bucket(&tb, bucket("files")).await.unwrap();
    assert!(
        matches!(
            s.create_bucket(&tb, bucket("shared")).await,
            Err(StorageError::Conflict {
                code: "bucket_exists",
                ..
            })
        ),
        "an instance-wide name on control is taken for a tenant on a shard too"
    );
    let buckets = "SELECT name FROM _forgebase_storage_buckets ORDER BY name";
    assert_eq!(strings(&control, buckets).await, ["shared", "tenant-a/files"]);
    assert_eq!(strings(&shard, buckets).await, ["tenant-b/files"]);

    // --- Objects: on the set of their bucket.
    s.put_object(&tb, put("files", "b.txt", b"bee")).await.unwrap();
    s.put_object(&ta, put("files", "a.txt", b"aye")).await.unwrap();
    s.put_object(&tb, put("shared", "from-b.txt", b"from b")).await.unwrap();
    let objects = "SELECT bucket || ':' || key FROM _forgebase_storage_objects ORDER BY bucket, key";
    assert_eq!(
        strings(&control, objects).await,
        ["shared:from-b.txt", "tenant-a/files:a.txt"]
    );
    assert_eq!(strings(&shard, objects).await, ["tenant-b/files:b.txt"]);
    let blobs = "SELECT sha256 FROM _forgebase_storage_blobs ORDER BY sha256";
    assert_eq!(strings(&shard, blobs).await, [sha(b"bee")]);
    let mut on_control = vec![sha(b"aye"), sha(b"from b")];
    on_control.sort();
    assert_eq!(strings(&control, blobs).await, on_control);

    // --- Listing and reading are isolated per tenant.
    let listed = |ctx: AccessContext| {
        let s = s.clone();
        async move {
            s.list_buckets(&ctx)
                .await
                .unwrap()
                .into_iter()
                .map(|b| (b.name, b.tenant))
                .collect::<Vec<_>>()
        }
    };
    assert_eq!(
        listed(tb.clone()).await,
        [("files".into(), Some("tenant-b".into())), ("shared".into(), None)]
    );
    assert_eq!(
        listed(ta.clone()).await,
        [("files".into(), Some("tenant-a".into())), ("shared".into(), None)]
    );
    assert_eq!(listed(root.clone()).await, [("shared".into(), None)]);
    let keys = |page: forgebase_storage::ObjectPage| page.objects.into_iter().map(|o| o.key).collect::<Vec<_>>();
    assert_eq!(
        keys(s.list_objects(&tb, "files", "", None, 100, None).await.unwrap()),
        ["b.txt"]
    );
    assert_eq!(
        keys(s.list_objects(&ta, "files", "", None, 100, None).await.unwrap()),
        ["a.txt"]
    );
    let got = s.get_object(&tb, "files", "b.txt", None, None).await.unwrap();
    assert_eq!(read_all(got).await, b"bee");
    assert!(matches!(
        s.get_object(&tb, "files", "a.txt", None, None).await,
        Err(StorageError::NotFound(_))
    ));
    assert_eq!(s.head_object(&tb, "files", "b.txt", None).await.unwrap().size, 3);
    assert_eq!(s.list_versions(&tb, "files", "b.txt").await.unwrap().len(), 1);
    assert_eq!(s.bucket_usage(&tb, "files").await.unwrap().objects, 1);
    assert_eq!(
        s.get_bucket(&tb, "files").await.unwrap().tenant.as_deref(),
        Some("tenant-b")
    );

    // --- Statistics: per tenant from its set; the engine summed over sets.
    let st = s.stats(&tb).await.unwrap();
    assert_eq!((st.buckets, st.objects, st.logical_bytes), (1, 1, 3));
    let st = s.stats(&ta).await.unwrap();
    assert_eq!((st.buckets, st.objects, st.logical_bytes), (1, 1, 3));
    let st = s.stats(&root).await.unwrap();
    assert_eq!(
        (
            st.scope.as_str(),
            st.buckets,
            st.objects,
            st.logical_bytes,
            st.stored_bytes
        ),
        ("engine", 3, 3, 12, 12)
    );

    // --- ACL entries on the bucket's set.
    s.set_acl(
        &tb,
        "files",
        forgebase_storage::Acl {
            entries: vec![AclEntry {
                prefix: "pub/".into(),
                grantee: "public".into(),
                permission: Permission::Read,
            }],
        },
    )
    .await
    .unwrap();
    let acl = "SELECT COUNT(*) FROM _forgebase_storage_acl";
    assert_eq!(
        (count(&shard, acl, vec![]).await, count(&control, acl, vec![]).await),
        (1, 0)
    );
    assert_eq!(s.get_acl(&tb, "files").await.unwrap().entries.len(), 1);

    // --- Upload sessions and parts on the bucket's set.
    let up = s
        .create_upload(
            &tb,
            "files",
            UploadInput {
                key: "big.bin".into(),
                ..UploadInput::default()
            },
        )
        .await
        .unwrap();
    s.upload_part(&tb, &up.id, 1, None, body_of(b"part one ".to_vec()))
        .await
        .unwrap();
    s.upload_part(&tb, &up.id, 2, None, body_of(b"part two".to_vec()))
        .await
        .unwrap();
    let uploads = "SELECT COUNT(*) FROM _forgebase_storage_uploads";
    let parts = "SELECT COUNT(*) FROM _forgebase_storage_parts";
    assert_eq!(
        (
            count(&shard, uploads, vec![]).await,
            count(&control, uploads, vec![]).await
        ),
        (1, 0)
    );
    assert_eq!(count(&shard, parts, vec![]).await, 2);
    // An administrator without a tenant finds the session on its set.
    assert_eq!(s.get_upload(&root, &up.id).await.unwrap().parts.len(), 2);
    let done = s.complete_upload(&tb, &up.id, CompleteInput::default()).await.unwrap();
    assert_eq!(done.size, 17);
    assert_eq!(count(&shard, uploads, vec![]).await, 0);
    let big = count(
        &shard,
        "SELECT COUNT(*) FROM _forgebase_storage_objects WHERE bucket = 'tenant-b/files' AND key = 'big.bin'",
        vec![],
    )
    .await;
    assert_eq!(big, 1);

    // --- Signed URLs: used without a tenant context, they find the set
    // from the tenant they carry.
    let url = s
        .sign_url(&tb, "files", sign("b.txt", SignedMethod::Get))
        .await
        .unwrap();
    assert!(url.url.contains("t=tenant-b"), "{}", url.url);
    let got = s
        .get_signed(&params_of(&url.url, SignedMethod::Get), None)
        .await
        .unwrap();
    assert_eq!(read_all(got).await, b"bee");
    let url = s
        .sign_url(&tb, "files", sign("signed.txt", SignedMethod::Put))
        .await
        .unwrap();
    s.put_signed(
        &params_of(&url.url, SignedMethod::Put),
        Some("text/plain".into()),
        BTreeMap::new(),
        None,
        None,
        body_of(b"via url".to_vec()),
    )
    .await
    .unwrap();
    let signed = count(
        &shard,
        "SELECT COUNT(*) FROM _forgebase_storage_objects WHERE bucket = 'tenant-b/files' AND key = 'signed.txt'",
        vec![],
    )
    .await;
    assert_eq!(signed, 1);

    // --- Shared blob files: the same content on two sets, deleted on one,
    // stays; deleted on both, goes.
    s.put_object(&ta, put("files", "same.txt", b"same content"))
        .await
        .unwrap();
    s.put_object(&tb, put("files", "same.txt", b"same content"))
        .await
        .unwrap();
    let same = blob_key(&sha(b"same content"));
    let st = s.stats(&root).await.unwrap();
    assert_eq!(
        st.stored_bytes,
        3 + 3 + 6 + 17 + 7 + 12,
        "a blob held by two sets counts once: {st:?}"
    );
    s.delete_object(&ta, "files", "same.txt", None).await.unwrap();
    let report = s.reconcile_now().await.unwrap();
    assert!(
        has_file(&driver, &same).await,
        "the shard still references it: {report:?}"
    );
    assert_eq!(report.unreferenced_blobs_deleted, 0, "{report:?}");
    assert!(report.missing_blobs.is_empty(), "{report:?}");
    s.delete_object(&tb, "files", "same.txt", None).await.unwrap();
    let report = s.reconcile_now().await.unwrap();
    assert!(!has_file(&driver, &same).await, "{report:?}");
    assert_eq!(report.unreferenced_blobs_deleted, 1, "{report:?}");
    let row = "SELECT COUNT(*) FROM _forgebase_storage_blobs WHERE sha256 = ?";
    assert_eq!(count(&control, row, vec![sha(b"same content").into()]).await, 0);
    assert_eq!(count(&shard, row, vec![sha(b"same content").into()]).await, 0);

    // A copy across sets references the blob on the target's set.
    s.copy_object(&tb, "shared", "from-b.txt", None, "files", "copied.txt")
        .await
        .unwrap();
    s.delete_object(&root, "shared", "from-b.txt", None).await.unwrap();
    s.reconcile_now().await.unwrap();
    assert!(has_file(&driver, &blob_key(&sha(b"from b"))).await);
    let got = s.get_object(&tb, "files", "copied.txt", None, None).await.unwrap();
    assert_eq!(read_all(got).await, b"from b");

    // --- A moving tenant is left alone where it is a partial copy. Tenant
    // b moves from the shard to control: control holds a copy of its bucket,
    // an object whose blob count is not rebuilt yet (zero) and an expired
    // session.
    let copied = b"copied content";
    let copied_sha = sha(copied);
    driver
        .put(&blob_key(&copied_sha), body_of(copied.to_vec()))
        .await
        .unwrap();
    exec(
        &control,
        "INSERT INTO _forgebase_storage_buckets (name, tenant, owner, created_at, updated_at) \
         VALUES ('tenant-b/files', 'tenant-b', 'admin:root', 0, 0)",
        vec![],
    )
    .await;
    exec(
        &control,
        "INSERT INTO _forgebase_storage_blobs (sha256, size, refcount, created_at, zero_since) VALUES (?1, 14, 0, 0, 0)",
        vec![copied_sha.as_str().into()],
    )
    .await;
    exec(
        &control,
        "INSERT INTO _forgebase_storage_objects (id, bucket, key, blob_sha, size, quota_subject, created_by, created_at) \
         VALUES ('copy-1', 'tenant-b/files', 'copy.bin', ?1, 14, 'x', 'admin:root', 0)",
        vec![copied_sha.as_str().into()],
    )
    .await;
    exec(
        &control,
        "INSERT INTO _forgebase_storage_uploads (id, bucket, key, kind, created_by, quota_subject, created_at, expires_at) \
         VALUES ('copy-up', 'tenant-b/files', 'u.bin', 'multipart', 'admin:root', 'x', 0, 1)",
        vec![],
    )
    .await;
    // Sessions that expire: tenant a's on control, tenant b's on the shard.
    for ctx in [&ta, &tb] {
        s.create_upload(
            ctx,
            "files",
            UploadInput {
                key: "late.bin".into(),
                ..UploadInput::default()
            },
        )
        .await
        .unwrap();
    }
    clock.advance(25 * 3600 * 1000);
    sh.router
        .place("tenant-b", &set.id, PlacementState::Moving)
        .await
        .unwrap();
    let report = s.reconcile_now().await.unwrap();
    assert_eq!(
        report.uploads_aborted, 2,
        "the source keeps serving tenant b: {report:?}"
    );
    let copy_up = "SELECT COUNT(*) FROM _forgebase_storage_uploads WHERE id = 'copy-up'";
    assert_eq!(
        count(&control, copy_up, vec![]).await,
        1,
        "the target's copy is skipped"
    );
    assert_eq!(count(&shard, uploads, vec![]).await, 0);
    assert!(
        has_file(&driver, &blob_key(&copied_sha)).await,
        "the copy's blob stays: {report:?}"
    );
    assert_eq!(count(&control, row, vec![copied_sha.as_str().into()]).await, 1);
    // Moving tenants are still served from the source.
    assert_eq!(
        keys(s.list_objects(&tb, "files", "", None, 100, None).await.unwrap()).len(),
        4
    );

    // The move is aborted: the copy goes, then the blob nobody holds.
    exec(
        &control,
        "DELETE FROM _forgebase_storage_buckets WHERE name = 'tenant-b/files'",
        vec![],
    )
    .await;
    sh.router
        .place("tenant-b", &set.id, PlacementState::Active)
        .await
        .unwrap();
    let report = s.reconcile_now().await.unwrap();
    assert!(!has_file(&driver, &blob_key(&copied_sha)).await, "{report:?}");
    assert_eq!(count(&control, row, vec![copied_sha.as_str().into()]).await, 0);

    // --- A frozen tenant waits for the switch, then gets 409 tenant_moving.
    let short = DbRouter::open(
        control.clone(),
        RouterOptions {
            connector: Some(sh.connector.clone()),
            freeze_timeout: Duration::from_millis(200),
            ..RouterOptions::default()
        },
    )
    .await
    .unwrap();
    let frozen = open(&control, &driver, &clock).await;
    frozen.attach_router(short.clone());
    let url = frozen
        .sign_url(&tb, "files", sign("b.txt", SignedMethod::Get))
        .await
        .unwrap();
    short.place("tenant-b", &set.id, PlacementState::Frozen).await.unwrap();
    let started = std::time::Instant::now();
    let err = frozen.list_buckets(&tb).await.unwrap_err();
    assert!(started.elapsed() >= Duration::from_millis(150), "it waited");
    assert!(
        matches!(err, StorageError::Router(RouterError::TenantMoving(_))),
        "{err:?}"
    );
    assert_eq!(err.code(), "tenant_moving");
    assert_eq!(forgebase_storage::http::api(err).status().as_u16(), 409);
    let err = frozen
        .get_signed(&params_of(&url.url, SignedMethod::Get), None)
        .await
        .unwrap_err();
    assert_eq!(forgebase_storage::http::api(err).status().as_u16(), 409);
    // Tenant a and instance-wide buckets do not wait.
    assert_eq!(frozen.list_buckets(&ta).await.unwrap().len(), 2);
    short.place("tenant-b", &set.id, PlacementState::Active).await.unwrap();
    assert_eq!(frozen.list_buckets(&tb).await.unwrap().len(), 2);
}
