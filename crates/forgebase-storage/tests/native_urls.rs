//! Native ("offloaded") signed download URLs on the storage service
//! (ADR-0040 addendum): the `signed_urls` mode and its per-bucket overrides,
//! the checks that still run before a native URL is issued, the expiry cap,
//! the response-header overrides, the audit record, `auto` falling back to
//! the proxy, `native` failing clearly, uploads staying on the proxy, and
//! the proxy path unchanged. The provider here is a fake signer over the
//! local driver; `tests/s3.rs`, `gcs.rs` and `azure.rs` sign for real and
//! download through their mocks.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Method, StatusCode};
use common::{Env, admin, put, sha, user};
use forgebase_core::SignedUrlsMode;
use forgebase_data::AccessContext;
use forgebase_db::{Backend, Statement};
use forgebase_storage::driver::BoxFuture;
use forgebase_storage::{
    Acl, AclEntry, BucketInput, ByteStream, DriverObject, LocalDriver, Permission, PresignGet, PutRequest, SignRequest,
    SignedMethod, SignedMode, StorageDriver, StorageError, StorageSettings, Visibility,
};
use serde_json::json;

/// What the fake provider was asked to sign.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Presigned {
    key: String,
    now: i64,
    expires_secs: u64,
    content_type: Option<String>,
    content_disposition: Option<String>,
}

/// The local driver plus a fake provider signer.
#[derive(Debug)]
struct Signer {
    local: LocalDriver,
    support: Result<u64, String>,
    issued: Mutex<Vec<Presigned>>,
}

impl Signer {
    fn new(dir: &std::path::Path, support: Result<u64, String>) -> Arc<Self> {
        Arc::new(Self {
            local: LocalDriver::new(dir),
            support,
            issued: Mutex::new(Vec::new()),
        })
    }

    fn issued(&self) -> Vec<Presigned> {
        self.issued.lock().unwrap().clone()
    }
}

impl StorageDriver for Signer {
    fn name(&self) -> &'static str {
        "fake"
    }
    fn put(&self, key: &str, body: ByteStream) -> BoxFuture<'_, forgebase_storage::Result<u64>> {
        self.local.put(key, body)
    }
    fn get(&self, key: &str, range: Option<(u64, u64)>) -> BoxFuture<'_, forgebase_storage::Result<ByteStream>> {
        self.local.get(key, range)
    }
    fn head(&self, key: &str) -> BoxFuture<'_, forgebase_storage::Result<Option<DriverObject>>> {
        self.local.head(key)
    }
    fn delete(&self, key: &str) -> BoxFuture<'_, forgebase_storage::Result<()>> {
        self.local.delete(key)
    }
    fn copy(&self, from: &str, to: &str) -> BoxFuture<'_, forgebase_storage::Result<()>> {
        self.local.copy(from, to)
    }
    fn list(&self, prefix: &str) -> BoxFuture<'_, forgebase_storage::Result<Vec<DriverObject>>> {
        self.local.list(prefix)
    }
    fn ping(&self) -> BoxFuture<'_, forgebase_storage::Result<()>> {
        self.local.ping()
    }
    fn presign_support(&self) -> Result<u64, String> {
        self.support.clone()
    }
    fn presign_get(&self, req: &PresignGet<'_>) -> forgebase_storage::Result<String> {
        self.issued.lock().unwrap().push(Presigned {
            key: req.key.to_owned(),
            now: req.now,
            expires_secs: req.expires_secs,
            content_type: req.content_type.map(str::to_owned),
            content_disposition: req.content_disposition.map(str::to_owned),
        });
        Ok(format!(
            "https://provider.example/{}?expires={}",
            req.key, req.expires_secs
        ))
    }
}

struct Rig {
    env: Env,
    signer: Arc<Signer>,
    _dir: tempfile::TempDir,
}

async fn rig(support: Result<u64, String>, tune: impl FnOnce(&mut StorageSettings)) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let signer = Signer::new(dir.path(), support);
    let env = Env::with(tune, Some(signer.clone() as Arc<dyn StorageDriver>)).await;
    Rig { env, signer, _dir: dir }
}

fn native(s: &mut StorageSettings) {
    s.signed_urls = SignedUrlsMode::Native;
}

fn get(key: &str) -> SignRequest {
    SignRequest {
        key: key.into(),
        method: None,
        expires_in: None,
        version_id: None,
        download_as: None,
        content_type: None,
    }
}

fn typed(bucket: &str, key: &str, data: &[u8], content_type: &str) -> PutRequest {
    PutRequest {
        content_type: Some(content_type.into()),
        ..put(bucket, key, data)
    }
}

fn blob(data: &[u8]) -> String {
    let h = sha(data);
    format!("blobs/{}/{h}", &h[..2])
}

fn now_secs(env: &Env) -> i64 {
    use forgebase_entitlements::Clock as _;
    env.clock.now_ms() / 1000
}

fn epoch_of(rfc3339: &str) -> i64 {
    time::OffsetDateTime::parse(rfc3339, &time::format_description::well_known::Rfc3339)
        .unwrap()
        .unix_timestamp()
}

async fn audit_rows(env: &Env) -> Vec<serde_json::Value> {
    let rows = env
        .db
        .fetch_all(&Statement::new(
            "SELECT actor, CAST(detail AS TEXT) FROM _forgebase_auth_audit WHERE event = ?1 ORDER BY id",
            vec!["storage.signed_url.native".into()],
        ))
        .await
        .unwrap();
    rows.rows
        .iter()
        .map(|r| {
            let detail: serde_json::Value = serde_json::from_str(r[1].as_str().unwrap()).unwrap();
            json!({ "actor": r[0].as_str().unwrap_or_default(), "detail": detail })
        })
        .collect()
}

#[tokio::test]
async fn the_proxy_path_is_the_default_and_unchanged() {
    let r = rig(Ok(604_800), |_| {}).await;
    let env = &r.env;
    env.bucket("s", false).await;
    env.storage
        .put_object(&admin(), put("s", "a.txt", b"alpha"))
        .await
        .unwrap();
    let signed = env.storage.sign_url(&admin(), "s", get("a.txt")).await.unwrap();
    assert_eq!(signed.mode, SignedMode::Proxy);
    assert!(
        signed.url.starts_with("/api/v1/storage/signed/s/a.txt?v=2&expires="),
        "{}",
        signed.url
    );
    assert!(signed.url.contains("&sig="));
    let (s, _, b) = env.req(None, Method::GET, &signed.url, &[], Body::empty()).await;
    assert_eq!((s, b.as_slice()), (StatusCode::OK, &b"alpha"[..]));
    assert!(r.signer.issued().is_empty(), "a driver that can sign is not asked to");
    assert!(
        audit_rows(env).await.is_empty(),
        "proxy issuance is not audited (as before)"
    );
    // The REST answer keeps its shape and says who serves the URL.
    let (s, body) = env
        .json(
            Some(admin()),
            Method::POST,
            "/api/v1/storage/buckets/s/sign",
            json!({"key": "a.txt"}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{body}");
    let mut keys: Vec<&str> = body.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["expires_at", "method", "mode", "url"]);
    assert_eq!(
        (body["mode"].as_str(), body["method"].as_str()),
        (Some("proxy"), Some("GET"))
    );
}

#[tokio::test]
async fn native_urls_name_one_blob_and_carry_the_proxy_headers() {
    let r = rig(Ok(604_800), native).await;
    let env = &r.env;
    env.bucket("s", false).await;
    env.storage
        .put_object(&admin(), typed("s", "docs/a.txt", b"alpha", "text/plain"))
        .await
        .unwrap();
    let signed = env.storage.sign_url(&admin(), "s", get("docs/a.txt")).await.unwrap();
    assert_eq!((signed.mode, signed.method), (SignedMode::Native, SignedMethod::Get));
    assert_eq!(
        signed.url,
        format!("https://provider.example/{}?expires=900", blob(b"alpha"))
    );
    let issued = r.signer.issued();
    assert_eq!(issued.len(), 1);
    let now = now_secs(env);
    assert_eq!(
        issued[0],
        Presigned {
            key: blob(b"alpha"),
            now,
            expires_secs: 900,
            content_type: Some("text/plain".into()),
            content_disposition: None,
        }
    );
    assert_eq!(
        epoch_of(&signed.expires_at),
        now + 900,
        "the provider's expiry is reported"
    );

    // A file name to save as becomes a signed Content-Disposition, made as the proxy makes it.
    let mut req = get("docs/a.txt");
    req.download_as = Some("final report.txt".into());
    env.storage.sign_url(&admin(), "s", req).await.unwrap();
    assert_eq!(
        r.signer.issued()[1].content_disposition.as_deref(),
        Some("attachment; filename=\"final_report.txt\"")
    );
    // Active content is an attachment, as on the proxy path.
    env.storage
        .put_object(&admin(), typed("s", "page.html", b"<script>1</script>", "text/html"))
        .await
        .unwrap();
    env.storage.sign_url(&admin(), "s", get("page.html")).await.unwrap();
    let last = r.signer.issued().pop().unwrap();
    assert_eq!(last.content_type.as_deref(), Some("text/html"));
    assert_eq!(
        last.content_disposition.as_deref(),
        Some("attachment; filename=\"page.html\"")
    );

    // Every native URL is audited, without the URL itself (a bearer credential).
    let rows = audit_rows(env).await;
    assert_eq!(rows.len(), 3, "{rows:?}");
    assert_eq!(rows[0]["actor"], "admin:root");
    let d = &rows[0]["detail"];
    assert_eq!(
        (d["bucket"].as_str(), d["key"].as_str()),
        (Some("s"), Some("docs/a.txt"))
    );
    assert_eq!(d["driver"], "fake");
    assert_eq!(d["expires_at"].as_str(), Some(signed.expires_at.as_str()));
    assert!(!rows[0].to_string().contains("provider.example"));

    // Through REST: same shape, `mode: native`, an absolute URL.
    let (s, body) = env
        .json(
            Some(admin()),
            Method::POST,
            "/api/v1/storage/buckets/s/sign",
            json!({"key": "docs/a.txt", "expires_in": 60}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body["mode"], "native");
    assert_eq!(
        body["url"].as_str().unwrap(),
        format!("https://provider.example/{}?expires=60", blob(b"alpha"))
    );
}

#[tokio::test]
async fn native_urls_are_bound_to_the_version_they_were_issued_for() {
    let r = rig(Ok(604_800), native).await;
    let env = &r.env;
    env.bucket("v", true).await;
    let v1 = env.storage.put_object(&admin(), put("v", "k", b"one")).await.unwrap();
    env.storage.put_object(&admin(), put("v", "k", b"two")).await.unwrap();
    env.storage.sign_url(&admin(), "v", get("k")).await.unwrap();
    assert_eq!(r.signer.issued()[0].key, blob(b"two"), "the latest version");
    let mut old = get("k");
    old.version_id = Some(v1.version_id.clone());
    env.storage.sign_url(&admin(), "v", old).await.unwrap();
    assert_eq!(r.signer.issued()[1].key, blob(b"one"), "a version asked for");
    let mut missing = get("k");
    missing.version_id = Some("no-such-version".into());
    assert!(matches!(
        env.storage.sign_url(&admin(), "v", missing).await,
        Err(StorageError::NotFound(_))
    ));
    // A deleted object (a delete marker) has nothing to sign.
    env.storage.delete_object(&admin(), "v", "k", None).await.unwrap();
    assert!(matches!(
        env.storage.sign_url(&admin(), "v", get("k")).await,
        Err(StorageError::NotFound(_))
    ));
    assert!(matches!(
        env.storage.sign_url(&admin(), "v", get("nothing-here")).await,
        Err(StorageError::NotFound(_))
    ));
    assert_eq!(r.signer.issued().len(), 2);
}

#[tokio::test]
async fn the_expiry_is_capped_at_the_providers_maximum() {
    let r = rig(Ok(3600), |s| {
        native(s);
        s.max_signed_ttl_secs = 30 * 86_400;
    })
    .await;
    let env = &r.env;
    env.bucket("s", false).await;
    env.storage.put_object(&admin(), put("s", "a", b"alpha")).await.unwrap();
    let mut long = get("a");
    long.expires_in = Some(20 * 86_400);
    let signed = env.storage.sign_url(&admin(), "s", long).await.unwrap();
    assert_eq!(
        r.signer.issued()[0].expires_secs,
        3600,
        "never longer than the provider allows"
    );
    assert_eq!(
        epoch_of(&signed.expires_at),
        now_secs(env) + 3600,
        "and reported as such"
    );
    let mut short = get("a");
    short.expires_in = Some(120);
    env.storage.sign_url(&admin(), "s", short).await.unwrap();
    assert_eq!(r.signer.issued()[1].expires_secs, 120, "shorter lifetimes are kept");
    // ForgeBase's own limits still apply first.
    for bad in [0, 30 * 86_400 + 1] {
        let mut req = get("a");
        req.expires_in = Some(bad);
        assert!(
            matches!(
                env.storage.sign_url(&admin(), "s", req).await,
                Err(StorageError::Invalid {
                    code: "invalid_expiry",
                    ..
                })
            ),
            "{bad}"
        );
    }
    assert_eq!(r.signer.issued().len(), 2);
}

#[tokio::test]
async fn access_checks_still_run_before_a_native_url_is_issued() {
    let r = rig(Ok(604_800), native).await;
    let env = &r.env;
    env.bucket("private", false).await;
    env.storage
        .put_object(&admin(), put("private", "a", b"alpha"))
        .await
        .unwrap();
    env.storage
        .put_object(&admin(), put("private", "shared/b", b"beta"))
        .await
        .unwrap();
    // No access: refused, and nothing is signed.
    assert!(matches!(
        env.storage.sign_url(&user("mallory"), "private", get("a")).await,
        Err(StorageError::Forbidden(_))
    ));
    assert!(
        env.storage
            .sign_url(&AccessContext::anonymous(), "private", get("a"))
            .await
            .is_err()
    );
    let (s, body) = env
        .json(
            Some(user("mallory")),
            Method::POST,
            "/api/v1/storage/buckets/private/sign",
            json!({"key": "a"}),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{body}");
    // An ACL grant on a prefix covers that prefix only.
    env.storage
        .set_acl(
            &admin(),
            "private",
            Acl {
                entries: vec![AclEntry {
                    prefix: "shared/".into(),
                    grantee: "user:people/alice".into(),
                    permission: Permission::Read,
                }],
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        env.storage.sign_url(&user("alice"), "private", get("a")).await,
        Err(StorageError::Forbidden(_))
    ));
    let ok = env
        .storage
        .sign_url(&user("alice"), "private", get("shared/b"))
        .await
        .unwrap();
    assert_eq!(ok.mode, SignedMode::Native);
    // Bucket visibility: `authenticated` lets any signed-in caller read, not anonymous ones.
    env.storage
        .create_bucket(
            &admin(),
            BucketInput {
                name: "members".into(),
                visibility: Visibility::Authenticated,
                ..BucketInput::default()
            },
        )
        .await
        .unwrap();
    env.storage
        .put_object(&admin(), put("members", "m", b"mem"))
        .await
        .unwrap();
    assert_eq!(
        env.storage
            .sign_url(&user("bob"), "members", get("m"))
            .await
            .unwrap()
            .mode,
        SignedMode::Native
    );
    assert!(
        env.storage
            .sign_url(&AccessContext::anonymous(), "members", get("m"))
            .await
            .is_err()
    );
    let keys: Vec<String> = r.signer.issued().into_iter().map(|p| p.key).collect();
    assert_eq!(
        keys,
        [blob(b"beta"), blob(b"mem")],
        "only the allowed objects were signed"
    );
}

#[tokio::test]
async fn uploads_stay_on_the_proxy_in_native_mode() {
    let r = rig(Ok(604_800), native).await;
    let env = &r.env;
    env.bucket("up", false).await;
    let signed = env
        .storage
        .sign_url(
            &admin(),
            "up",
            SignRequest {
                key: "in.txt".into(),
                method: Some(SignedMethod::Put),
                expires_in: Some(120),
                version_id: None,
                download_as: None,
                content_type: Some("text/plain".into()),
            },
        )
        .await
        .unwrap();
    assert_eq!(signed.mode, SignedMode::Proxy);
    assert!(signed.url.starts_with("/api/v1/storage/signed/up/in.txt?"));
    let (s, _, b) = env
        .req(
            None,
            Method::PUT,
            &signed.url,
            &[("content-type", "text/plain")],
            Body::from("hello"),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{}", String::from_utf8_lossy(&b));
    assert!(r.signer.issued().is_empty());
    // The bytes went through ForgeBase's checks: a size limit still holds.
    env.storage
        .update_bucket(
            &admin(),
            "up",
            forgebase_storage::BucketPatch {
                max_object_bytes: Some(3),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let mut req = get("big.txt");
    req.method = Some(SignedMethod::Put);
    let signed = env.storage.sign_url(&admin(), "up", req).await.unwrap();
    let (s, _, _) = env
        .req(None, Method::PUT, &signed.url, &[], Body::from("too big"))
        .await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn auto_falls_back_to_the_proxy_and_native_fails_clearly() {
    let why = "the fake has no key".to_owned();
    // auto without signing credentials: proxy URLs, working as always.
    let r = rig(Err(why.clone()), |s| s.signed_urls = SignedUrlsMode::Auto).await;
    let env = &r.env;
    env.bucket("s", false).await;
    env.storage.put_object(&admin(), put("s", "a", b"alpha")).await.unwrap();
    let signed = env.storage.sign_url(&admin(), "s", get("a")).await.unwrap();
    assert_eq!(signed.mode, SignedMode::Proxy);
    let (s, _, b) = env.req(None, Method::GET, &signed.url, &[], Body::empty()).await;
    assert_eq!((s, b.as_slice()), (StatusCode::OK, &b"alpha"[..]));
    assert!(r.signer.issued().is_empty());

    // auto with signing credentials: native.
    let r2 = rig(Ok(604_800), |s| s.signed_urls = SignedUrlsMode::Auto).await;
    r2.env.bucket("s", false).await;
    r2.env
        .storage
        .put_object(&admin(), put("s", "a", b"alpha"))
        .await
        .unwrap();
    assert_eq!(
        r2.env.storage.sign_url(&admin(), "s", get("a")).await.unwrap().mode,
        SignedMode::Native
    );

    // native without signing credentials: a clear refusal (after the access checks).
    let r3 = rig(Err(why.clone()), native).await;
    let env = &r3.env;
    env.bucket("s", false).await;
    env.storage.put_object(&admin(), put("s", "a", b"alpha")).await.unwrap();
    match env.storage.sign_url(&admin(), "s", get("a")).await {
        Err(StorageError::Conflict {
            code: "native_signing_unavailable",
            message,
        }) => assert!(message.contains(&why) && message.contains("fake driver"), "{message}"),
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        env.storage.sign_url(&user("mallory"), "s", get("a")).await,
        Err(StorageError::Forbidden(_))
    ));
    let (s, body) = env
        .json(
            Some(admin()),
            Method::POST,
            "/api/v1/storage/buckets/s/sign",
            json!({"key": "a"}),
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "native_signing_unavailable");

    // The local driver never signs.
    let env = Env::with(native, None).await;
    env.bucket("s", false).await;
    env.storage.put_object(&admin(), put("s", "a", b"alpha")).await.unwrap();
    assert!(matches!(
        env.storage.sign_url(&admin(), "s", get("a")).await,
        Err(StorageError::Conflict {
            code: "native_signing_unavailable",
            ..
        })
    ));
    let env = Env::with(|s| s.signed_urls = SignedUrlsMode::Auto, None).await;
    env.bucket("s", false).await;
    env.storage.put_object(&admin(), put("s", "a", b"alpha")).await.unwrap();
    assert_eq!(
        env.storage.sign_url(&admin(), "s", get("a")).await.unwrap().mode,
        SignedMode::Proxy
    );
}

#[tokio::test]
async fn per_bucket_overrides_pick_the_mode() {
    let r = rig(Ok(604_800), |s| {
        s.signed_urls_buckets = BTreeMap::from([
            ("avatars".to_owned(), SignedUrlsMode::Native),
            ("invoices".to_owned(), SignedUrlsMode::Native),
            ("acme/invoices".to_owned(), SignedUrlsMode::Proxy),
        ]);
    })
    .await;
    let env = &r.env;
    for b in ["avatars", "docs"] {
        env.bucket(b, false).await;
        env.storage
            .put_object(&admin(), put(b, "x", b.as_bytes()))
            .await
            .unwrap();
    }
    assert_eq!(
        env.storage.sign_url(&admin(), "avatars", get("x")).await.unwrap().mode,
        SignedMode::Native
    );
    assert_eq!(
        env.storage.sign_url(&admin(), "docs", get("x")).await.unwrap().mode,
        SignedMode::Proxy,
        "the instance default"
    );
    // A tenant's own entry wins over the bucket name's.
    let m = |t: Option<&str>, b: &str| env.storage.signed_urls_mode(t, b);
    assert_eq!(m(Some("acme"), "invoices"), SignedUrlsMode::Proxy);
    assert_eq!(m(Some("globex"), "invoices"), SignedUrlsMode::Native);
    assert_eq!(m(None, "invoices"), SignedUrlsMode::Native);
    assert_eq!(m(Some("acme"), "docs"), SignedUrlsMode::Proxy);

    // And the other way round: native everywhere but one bucket.
    let r = rig(Ok(604_800), |s| {
        native(s);
        s.signed_urls_buckets = BTreeMap::from([("private-docs".to_owned(), SignedUrlsMode::Proxy)]);
    })
    .await;
    r.env.bucket("private-docs", false).await;
    r.env
        .storage
        .put_object(&admin(), put("private-docs", "x", b"x"))
        .await
        .unwrap();
    assert_eq!(
        r.env
            .storage
            .sign_url(&admin(), "private-docs", get("x"))
            .await
            .unwrap()
            .mode,
        SignedMode::Proxy
    );
}
