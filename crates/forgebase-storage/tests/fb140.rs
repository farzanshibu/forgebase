//! FB-140: the sealed URL signing key and its migration (rls-coverage G11,
//! G15), and bucket rules on the paths the server test does not reach
//! (multipart and resumable uploads, restores, moves, tenant buckets).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;

use common::{Env, admin, put, user};
use forgebase_auth::crypto::Keyring;
use forgebase_data::AccessContext;
use forgebase_db::{Backend, Statement, TxMode, Value};
use forgebase_entitlements::Clock;
use forgebase_storage::{
    BucketInput, CompleteInput, SignRequest, SignedMethod, SignedParams, StorageError, StorageService,
    StorageSettings, UploadInput, Visibility, body_of,
};
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

const KEYRING: [u8; 32] = [9u8; 32];

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// A URL of the format before FB-140, signed with `key`.
fn v1_params(key: &[u8], bucket: &str, object: &str, expires: i64) -> SignedParams {
    let msg = format!("GET\n{bucket}\n{object}\n{expires}\n\n\n\n");
    let mut m = <Hmac<Sha256> as KeyInit>::new_from_slice(key).unwrap();
    m.update(msg.as_bytes());
    let sig = hex(&m.finalize().into_bytes());
    let q: BTreeMap<String, String> = [("expires".to_owned(), expires.to_string()), ("sig".to_owned(), sig)].into();
    SignedParams::from_request(SignedMethod::Get, bucket, object, &q).unwrap()
}

async fn settings(env: &Env) -> Vec<(String, String)> {
    let rows = env
        .db
        .fetch_all(&Statement::new(
            "SELECT name, value FROM _forgebase_storage_settings ORDER BY name",
            vec![],
        ))
        .await
        .unwrap();
    rows.rows
        .iter()
        .map(|r| (r[0].as_str().unwrap().to_owned(), r[1].as_str().unwrap().to_owned()))
        .collect()
}

async fn exec(env: &Env, sql: &str, params: Vec<Value>) {
    let mut tx = env.db.begin(TxMode::Normal).await.unwrap();
    tx.execute(&Statement::new(sql, params)).await.unwrap();
    tx.commit().await.unwrap();
}

async fn reopen(env: &Env, keyring: Keyring) -> Result<StorageService, StorageError> {
    StorageService::open(
        Arc::new(env.db.clone()),
        env.data.clone(),
        env.driver.clone(),
        Arc::new(env.clock.clone()),
        StorageSettings::default(),
        keyring,
    )
    .await
}

#[tokio::test]
async fn a_plain_key_is_sealed_unchanged_and_old_urls_keep_working() {
    let env = Env::new().await;
    env.bucket("docs", false).await;
    env.storage
        .put_object(&admin(), put("docs", "r.txt", b"report"))
        .await
        .unwrap();
    // The state before FB-140: a plain hex key and URLs signed with it.
    let old_key = [42u8; 32];
    exec(&env, "DELETE FROM _forgebase_storage_settings", vec![]).await;
    exec(
        &env,
        "INSERT INTO _forgebase_storage_settings (name, value) VALUES ('signing_key', ?)",
        vec![hex(&old_key).into()],
    )
    .await;
    let expires = env.clock.now_ms() / 1000 + 600;
    let old_url = v1_params(&old_key, "docs", "r.txt", expires);

    let s = reopen(&env, Keyring::from_master(&KEYRING)).await.unwrap();
    // Migrated: sealed, the plain row gone, nothing in the table is the key.
    let rows = settings(&env).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].0, "signing_key_sealed");
    assert!(!rows[0].1.contains(&hex(&old_key)), "{rows:?}");
    // The URL issued before the migration still works.
    let read = s.get_signed(&old_url, None).await.unwrap();
    assert_eq!(read.size, 6);
    // A database copy alone cannot forge: the sealed value is not the key.
    let sealed = forgebase_storage::util::unhex(&rows[0].1).unwrap();
    assert!(matches!(
        s.get_signed(&v1_params(&sealed, "docs", "r.txt", expires), None).await,
        Err(StorageError::SignatureInvalid)
    ));
    // New URLs use format 2 under the same key; both verify.
    let url = s
        .sign_url(
            &admin(),
            "docs",
            SignRequest {
                key: "r.txt".into(),
                method: None,
                expires_in: Some(60),
                version_id: None,
                download_as: None,
                content_type: None,
            },
        )
        .await
        .unwrap();
    assert!(url.url.contains("?v=2&"), "{}", url.url);

    // Another master key opens nothing and overwrites nothing.
    let err = reopen(&env, Keyring::from_master(&[1u8; 32])).await.unwrap_err();
    assert!(err.to_string().contains("master key"), "{err}");
    assert_eq!(settings(&env).await, rows);
    let again = reopen(&env, Keyring::from_master(&KEYRING)).await.unwrap();
    assert!(again.get_signed(&old_url, None).await.is_ok());

    // Rotation revokes every URL, old and new format alike.
    assert!(matches!(
        again.rotate_signing_key(&user("alice")).await,
        Err(StorageError::Forbidden(_))
    ));
    again.rotate_signing_key(&admin()).await.unwrap();
    assert!(matches!(
        again.get_signed(&old_url, None).await,
        Err(StorageError::SignatureInvalid)
    ));
    assert_ne!(settings(&env).await, rows);
}

async fn rules(env: &Env, bucket: &str, set: &[(&str, &str)]) {
    let input = forgebase_policy::store::ResourceRulesInput {
        rules: set.iter().map(|(a, r)| ((*a).to_owned(), Some((*r).to_owned()))).collect(),
    };
    env.policy
        .put_resource_rules(&admin(), "bucket", bucket, &input)
        .await
        .unwrap();
}

fn upload(key: &str) -> UploadInput {
    UploadInput {
        key: key.into(),
        kind: Some("resumable".into()),
        content_type: Some("text/plain".into()),
        metadata: BTreeMap::new(),
        size: None,
        checksum_sha256: None,
    }
}

#[tokio::test]
async fn bucket_rules_apply_to_uploads_restores_and_moves() {
    let env = Env::new().await;
    env.bucket("home", true).await;
    let own = "owner = @request.auth.id";
    rules(
        &env,
        "home",
        &[
            ("read", own),
            ("create", "folders.0 = @request.auth.id && size <= 10"),
            ("update", own),
            ("delete", own),
        ],
    )
    .await;
    let (alice, bob) = (user("alice"), user("bob"));

    // A resumable upload is checked when it completes: the size rule holds
    // on the assembled object.
    let up = env.storage.create_upload(&alice, "home", upload("alice/big.txt")).await.unwrap();
    env.storage
        .append_chunk(&alice, &up.id, 0, None, body_of(vec![b'x'; 20]))
        .await
        .unwrap();
    let r = env.storage.complete_upload(&alice, &up.id, CompleteInput::default()).await;
    assert!(matches!(r, Err(StorageError::Forbidden(_))), "{r:?}");
    let up = env.storage.create_upload(&alice, "home", upload("alice/ok.txt")).await.unwrap();
    env.storage
        .append_chunk(&alice, &up.id, 0, None, body_of(b"small".to_vec()))
        .await
        .unwrap();
    let done = env.storage.complete_upload(&alice, &up.id, CompleteInput::default()).await;
    let info = match done {
        Ok(i) => i,
        Err(e) => panic!("{e}"),
    };
    assert_eq!(info.created_by, "user:people/alice");
    // In someone else's folder: refused when the upload completes (rules
    // over the written version need the assembled object).
    let up = env.storage.create_upload(&bob, "home", upload("alice/x.txt")).await.unwrap();
    env.storage
        .append_chunk(&bob, &up.id, 0, None, body_of(b"x".to_vec()))
        .await
        .unwrap();
    assert!(matches!(
        env.storage.complete_upload(&bob, &up.id, CompleteInput::default()).await,
        Err(StorageError::Forbidden(_))
    ));
    // ... and a session onto someone else's existing object at once.
    assert!(matches!(
        env.storage.create_upload(&bob, "home", upload("alice/ok.txt")).await,
        Err(StorageError::Forbidden(_))
    ));

    // Restoring needs view on the version restored.
    env.storage
        .put_object(&alice, put("home", "alice/ok.txt", b"v2"))
        .await
        .unwrap();
    let first = env
        .storage
        .list_versions(&alice, "home", "alice/ok.txt")
        .await
        .unwrap()
        .last()
        .unwrap()
        .version_id
        .clone();
    assert!(matches!(
        env.storage.restore_version(&bob, "home", "alice/ok.txt", &first).await,
        Err(StorageError::NotFound(_))
    ));
    env.storage
        .restore_version(&alice, "home", "alice/ok.txt", &first)
        .await
        .unwrap();

    // Moving someone else's object: refused before anything is copied.
    assert!(matches!(
        env.storage
            .move_object(&bob, "home", "alice/ok.txt", "home", "bob/ok.txt")
            .await,
        Err(StorageError::NotFound(_))
    ));
    assert!(matches!(
        env.storage.head_object(&admin(), "home", "bob/ok.txt", None).await,
        Err(StorageError::NotFound(_))
    ));
    // A delete marker of one's own object.
    let del = env.storage.delete_object(&alice, "home", "alice/ok.txt", None).await.unwrap();
    assert!(del.delete_marker);
}

#[tokio::test]
async fn tenant_buckets_resolve_in_their_tenant() {
    let env = Env::new().await;
    let ta = AccessContext::admin("root").with_tenant("tenant-a");
    let tb = AccessContext::admin("root").with_tenant("tenant-b");
    let input = |n: &str| BucketInput {
        name: n.into(),
        visibility: Visibility::Private,
        versioning: false,
        max_object_bytes: None,
        allowed_content_types: None,
    };
    assert_eq!(env.storage.create_bucket(&ta, input("files")).await.unwrap().tenant.as_deref(), Some("tenant-a"));
    env.storage.create_bucket(&tb, input("files")).await.unwrap();
    assert!(matches!(
        env.storage.create_bucket(&ta, input("files")).await,
        Err(StorageError::Conflict { .. })
    ));
    env.storage.put_object(&ta, put("files", "a.txt", b"a")).await.unwrap();
    assert!(matches!(
        env.storage.head_object(&tb, "files", "a.txt", None).await,
        Err(StorageError::NotFound(_))
    ));
    // Without a tenant, neither is visible; their stored names are not names.
    assert!(env.storage.list_buckets(&admin()).await.unwrap().is_empty());
    for name in ["files", "tenant-a/files"] {
        assert!(matches!(
            env.storage.get_bucket(&admin(), name).await,
            Err(StorageError::NotFound(_))
        ));
    }
    // An instance-wide bucket of the same name is hidden in tenant a only.
    env.storage.create_bucket(&admin(), input("files")).await.unwrap();
    env.storage.put_object(&admin(), put("files", "g.txt", b"g")).await.unwrap();
    let names: Vec<_> = env.storage.list_buckets(&ta).await.unwrap().into_iter().map(|b| b.tenant).collect();
    assert_eq!(names, vec![Some("tenant-a".to_owned())]);
    assert!(env.storage.head_object(&ta, "files", "g.txt", None).await.is_err());
    assert!(env.storage.head_object(&admin(), "files", "g.txt", None).await.is_ok());
    // Deleting a tenant bucket leaves the others.
    env.storage.delete_bucket(&ta, "files", true).await.unwrap();
    assert!(env.storage.head_object(&admin(), "files", "g.txt", None).await.is_ok());
    assert!(env.storage.get_bucket(&tb, "files").await.is_ok());
}
