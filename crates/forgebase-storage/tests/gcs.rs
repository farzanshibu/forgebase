//! The Google Cloud Storage driver against a mock of the JSON API, the OAuth
//! token endpoint and the GCE metadata server (no network): JWT exchange and
//! refresh, metadata-server auth, resumable uploads, retries, paging, V4
//! signed URLs against Google's published test vectors, the storage
//! service on top, and its native signed downloads fetched from the mock.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use common::{Env, admin, bytes_of, chunked, put, read_all};
use forgebase_auth::net::{HttpsClient, TlsSettings};
use forgebase_core::SignedUrlsMode;
use forgebase_storage::cloud::RsaSigner;
use forgebase_storage::driver::collect;
use forgebase_storage::gcs::{V4Request, v4_signed_url, v4_signed_url_with};
use forgebase_storage::{
    GcsAuth, GcsConfig, GcsDriver, PresignGet, ServiceAccount, SignRequest, SignedMode, StorageDriver, StorageError,
    body_of,
};
use serde_json::Value as Json;
use support::gcs_mock::{BUCKET, GcsMock, SA_JSON};
use support::{chunks_of, conformance, pattern};

const NOW: i64 = 1_794_225_600;
const CHUNK: usize = 256 * 1024;

fn client() -> Arc<HttpsClient> {
    Arc::new(HttpsClient::new(&TlsSettings::new()).unwrap().with_max_body(2 << 20))
}

fn sa_for(mock: &GcsMock) -> ServiceAccount {
    let mut v: Json = serde_json::from_str(SA_JSON).unwrap();
    v["token_uri"] = Json::from(format!("{}/token", mock.url()));
    ServiceAccount::from_json(&v.to_string()).unwrap()
}

fn config(mock: &GcsMock, auth: GcsAuth, part: usize) -> GcsConfig {
    let mut cfg = GcsConfig::new(BUCKET, auth);
    cfg.endpoint = mock.url();
    cfg.prefix = "fb".into();
    cfg.part_size = part;
    cfg.read_chunk = 100_000;
    cfg.retry.base_delay = std::time::Duration::from_millis(1);
    cfg
}

fn driver(mock: &GcsMock, part: usize) -> GcsDriver {
    GcsDriver::with_clock(
        config(mock, GcsAuth::ServiceAccount(sa_for(mock)), part),
        client(),
        Arc::new(|| NOW),
    )
    .unwrap()
}

fn clocked(mock: &GcsMock, auth: GcsAuth) -> (GcsDriver, Arc<AtomicI64>) {
    let now = Arc::new(AtomicI64::new(NOW));
    let n = now.clone();
    let d = GcsDriver::with_clock(
        config(mock, auth, CHUNK),
        client(),
        Arc::new(move || n.load(Ordering::SeqCst)),
    )
    .unwrap();
    (d, now)
}

#[tokio::test]
async fn conformance_with_a_service_account() {
    let mock = GcsMock::start(3).await;
    let d = driver(&mock, CHUNK);
    conformance::run(&d, "gcs", 700_000).await;
    assert!(mock.bad().is_empty(), "{:?}", mock.bad());
    // Everything lives under the configured prefix; failed uploads left nothing.
    assert!(mock.names().iter().all(|n| n.starts_with("fb/")), "{:?}", mock.names());
    assert!(
        mock.st.lock().unwrap().cancelled >= 1,
        "a failing resumable upload is cancelled"
    );
    assert_eq!(
        mock.st.lock().unwrap().tokens_issued,
        1,
        "one token served the whole run"
    );
}

#[tokio::test]
async fn conformance_without_credentials_on_an_emulator() {
    let mock = GcsMock::start(0).await;
    mock.st.lock().unwrap().require_auth = false;
    let d = GcsDriver::new(config(&mock, GcsAuth::None, CHUNK), client()).unwrap();
    conformance::run(&d, "gcs", 600_000).await;
    assert_eq!(mock.st.lock().unwrap().tokens_issued, 0);
    assert!(mock.bad().is_empty());
}

#[tokio::test]
async fn the_jwt_grant_is_exchanged_and_the_token_refreshed_before_it_expires() {
    let mock = GcsMock::start(0).await;
    let (d, now) = clocked(&mock, GcsAuth::ServiceAccount(sa_for(&mock)));
    d.put("blobs/aa/one", body_of(vec![1; 10])).await.unwrap();
    d.put("blobs/aa/two", body_of(vec![2; 10])).await.unwrap();
    {
        let s = mock.st.lock().unwrap();
        assert_eq!(s.tokens_issued, 1, "the token is cached");
        let c = &s.assertions[0];
        assert_eq!(
            c["iss"],
            "test-iam-credentials@dummy-project-id.iam.gserviceaccount.com"
        );
        assert_eq!(c["scope"], "https://www.googleapis.com/auth/devstorage.read_write");
        assert_eq!(c["aud"], format!("{}/token", mock.url()));
        assert_eq!((c["iat"].as_i64(), c["exp"].as_i64()), (Some(NOW), Some(NOW + 3600)));
        assert!(s.bad.is_empty(), "{:?}", s.bad);
    }
    // Two minutes before expiry the driver asks for a new token.
    now.store(NOW + 3600 - 119, Ordering::SeqCst);
    d.head("blobs/aa/one").await.unwrap().unwrap();
    assert_eq!(mock.st.lock().unwrap().tokens_issued, 2, "refreshed ahead of expiry");
    d.head("blobs/aa/one").await.unwrap().unwrap();
    assert_eq!(mock.st.lock().unwrap().tokens_issued, 2);
    assert_eq!(
        mock.st.lock().unwrap().assertions[1]["iat"].as_i64(),
        Some(NOW + 3600 - 119)
    );

    // A token the service no longer accepts (401) is replaced and the call retried.
    mock.st.lock().unwrap().accepted.clear();
    assert!(d.head("blobs/aa/two").await.unwrap().is_some());
    assert_eq!(mock.st.lock().unwrap().tokens_issued, 3);
}

#[tokio::test]
async fn a_broken_private_key_is_refused_at_construction() {
    let mock = GcsMock::start(0).await;
    let mut sa = sa_for(&mock);
    let other = "-----BEGIN PRIVATE KEY-----\n".to_owned() + &"x".repeat(8) + "\n-----END PRIVATE KEY-----\n";
    sa.private_key = other;
    let err = GcsDriver::new(config(&mock, GcsAuth::ServiceAccount(sa), CHUNK), client()).unwrap_err();
    assert!(
        matches!(
            err,
            StorageError::Invalid {
                code: "invalid_gcs_key",
                ..
            }
        ),
        "{err:?}"
    );
    assert!(ServiceAccount::from_json("{}").is_err());
    assert!(ServiceAccount::from_json("nope").is_err());
}

#[tokio::test]
async fn the_metadata_server_provides_workload_identity_tokens() {
    let mock = GcsMock::start(0).await;
    let (d, now) = clocked(&mock, GcsAuth::Metadata { endpoint: mock.url() });
    d.put("blobs/aa/wi", body_of(vec![9; 100])).await.unwrap();
    assert_eq!(
        collect(d.get("blobs/aa/wi", None).await.unwrap()).await.unwrap(),
        vec![9; 100]
    );
    assert_eq!(mock.st.lock().unwrap().tokens_issued, 1);
    assert_eq!(
        mock.server
            .count("GET /computeMetadata/v1/instance/service-accounts/default/token"),
        1
    );
    assert_eq!(mock.server.count("POST /token"), 0, "no JWT exchange");
    now.store(NOW + 4000, Ordering::SeqCst);
    d.head("blobs/aa/wi").await.unwrap().unwrap();
    assert_eq!(mock.st.lock().unwrap().tokens_issued, 2, "metadata tokens refresh too");
    // Workload identity has no private key to sign URLs with.
    assert!(matches!(
        d.signed_url("GET", "blobs/aa/wi", 60),
        Err(StorageError::Invalid {
            code: "gcs_signing_unavailable",
            ..
        })
    ));
}

#[tokio::test]
async fn an_explicit_access_token_is_sent_as_is() {
    let mock = GcsMock::start(0).await;
    mock.st.lock().unwrap().accepted.insert("static-token".into());
    let d = GcsDriver::new(
        config(&mock, GcsAuth::AccessToken("static-token".into()), CHUNK),
        client(),
    )
    .unwrap();
    d.put("blobs/aa/x", body_of(vec![1; 3])).await.unwrap();
    let wrong = GcsDriver::new(config(&mock, GcsAuth::AccessToken("wrong".into()), CHUNK), client()).unwrap();
    let err = wrong.put("blobs/aa/y", body_of(vec![1; 3])).await.unwrap_err();
    assert!(matches!(&err, StorageError::Driver(m) if m.contains("401")), "{err:?}");
}

#[tokio::test]
async fn transient_failures_are_retried_with_backoff() {
    let mock = GcsMock::start(0).await;
    let d = driver(&mock, CHUNK);
    d.put("blobs/aa/r", body_of(vec![5; 50])).await.unwrap();

    // 503, 503, then success.
    mock.fail("GET /storage/v1/b/test-bucket/o/", 503, 2);
    let before = mock.server.count("GET /storage/v1/b/test-bucket/o/");
    assert_eq!(d.head("blobs/aa/r").await.unwrap().unwrap().size, 50);
    assert_eq!(
        mock.server.count("GET /storage/v1/b/test-bucket/o/") - before,
        3,
        "two failures, one success"
    );

    // 429 as well, honouring a Retry-After of 0.
    mock.fail("GET /storage/v1/b/test-bucket/o/", 429, 1);
    assert!(d.head("blobs/aa/r").await.unwrap().is_some());

    // A 404 is an answer, not a failure.
    let before = mock.server.count("GET /storage/v1/b/test-bucket/o/");
    assert_eq!(d.head("blobs/aa/none").await.unwrap(), None);
    assert_eq!(mock.server.count("GET /storage/v1/b/test-bucket/o/") - before, 1);

    // Out of attempts: the last answer is reported.
    mock.fail("GET /storage/v1/b/test-bucket/o/", 503, 99);
    let err = d.head("blobs/aa/r").await.unwrap_err();
    assert!(matches!(&err, StorageError::Driver(m) if m.contains("503")), "{err:?}");
}

#[tokio::test]
async fn a_failed_resumable_chunk_resumes_from_what_the_service_kept() {
    let mock = GcsMock::start(0).await;
    // 512 KiB chunks; the service keeps only 256 KiB of the first, and one
    // chunk gets a 503.
    let d = driver(&mock, 2 * CHUNK);
    mock.st.lock().unwrap().partial_once = true;
    mock.fail("PUT /session/", 503, 1);
    let data = pattern(1_300_000);
    d.put("blobs/bb/resumed", chunks_of(data.clone(), 65_537))
        .await
        .unwrap();
    assert_eq!(mock.object("fb/blobs/bb/resumed").unwrap(), data);
    assert!(mock.bad().is_empty(), "{:?}", mock.bad());
    assert!(mock.server.count("PUT /session/") >= 4, "{:?}", mock.server.lines());

    // A body that is an exact multiple of the chunk size.
    let exact = pattern(4 * CHUNK);
    d.put("blobs/bb/exact", chunks_of(exact.clone(), 100_000))
        .await
        .unwrap();
    assert_eq!(mock.object("fb/blobs/bb/exact").unwrap(), exact);
    assert!(mock.bad().is_empty(), "{:?}", mock.bad());
}

#[tokio::test]
async fn small_bodies_are_one_request_and_big_ones_a_resumable_upload() {
    let mock = GcsMock::start(0).await;
    let d = driver(&mock, CHUNK);
    d.put("blobs/aa/small", chunks_of(pattern(CHUNK), 4096)).await.unwrap();
    assert_eq!(
        mock.server
            .count("POST /upload/storage/v1/b/test-bucket/o?uploadType&name"),
        1
    );
    assert_eq!(mock.server.count("PUT /session/"), 0);
    d.put("blobs/bb/big", chunks_of(pattern(CHUNK * 2 + 5), 50_000))
        .await
        .unwrap();
    assert_eq!(mock.server.count("PUT /session/"), 3, "two full chunks and the rest");
    // Reads page through ranged GETs of at most read_chunk bytes.
    let all = collect(d.get("blobs/bb/big", None).await.unwrap()).await.unwrap();
    assert_eq!(all.len(), CHUNK * 2 + 5);
    let gets = mock.server.lines().iter().filter(|l| l.contains("alt")).count();
    assert_eq!(gets, 6, "524293 bytes in reads of 100000: {:?}", mock.server.lines());
}

#[tokio::test]
async fn compose_joins_objects_on_the_server() {
    let mock = GcsMock::start(0).await;
    let d = driver(&mock, CHUNK);
    d.put("staging/p1", body_of(b"hello ".to_vec())).await.unwrap();
    d.put("staging/p2", body_of(b"cloud ".to_vec())).await.unwrap();
    d.put("staging/p3", body_of(b"storage".to_vec())).await.unwrap();
    d.compose(&["staging/p1", "staging/p2", "staging/p3"], "blobs/aa/joined")
        .await
        .unwrap();
    assert_eq!(
        collect(d.get("blobs/aa/joined", None).await.unwrap()).await.unwrap(),
        b"hello cloud storage"
    );
    assert!(matches!(
        d.compose(&["staging/nope"], "blobs/x").await,
        Err(StorageError::NotFound(_))
    ));
    assert!(d.compose(&[], "blobs/x").await.is_err());
    let many: Vec<String> = (0..33).map(|i| format!("staging/p{i}")).collect();
    let refs: Vec<&str> = many.iter().map(String::as_str).collect();
    assert!(d.compose(&refs, "blobs/x").await.is_err(), "at most 32 sources");
}

#[tokio::test]
async fn listing_pages_and_folds_by_delimiter() {
    let mock = GcsMock::start(2).await;
    let d = driver(&mock, CHUNK);
    for k in ["a/1", "a/2", "a/3", "b/1", "b/2", "top"] {
        d.put(k, body_of(k.as_bytes().to_vec())).await.unwrap();
    }
    let all = d.list("").await.unwrap();
    assert_eq!(all.len(), 6);
    assert!(
        mock.server.count("GET /storage/v1/b/test-bucket/o?") >= 3,
        "paged: {:?}",
        mock.server.lines()
    );
    assert_eq!(all[0].key, "a/1", "the prefix is stripped");
    assert_eq!(all[0].modified_ms, 1_794_225_600_000);
    let (objects, prefixes) = d.list_with_delimiter("", "/").await.unwrap();
    assert_eq!(objects.iter().map(|o| o.key.as_str()).collect::<Vec<_>>(), ["top"]);
    assert_eq!(prefixes, ["a/", "b/"]);
    let (objects, prefixes) = d.list_with_delimiter("a/", "/").await.unwrap();
    assert_eq!(objects.len(), 3);
    assert!(prefixes.is_empty());
}

/// Google's published V4 signing test vectors (googleapis/conformance-tests,
/// `storage/v1/v4_signatures.json`) signed with Google's dummy service key,
/// including the two with extra query parameters (encoding and ordering),
/// which the `response-*` overrides of native downloads rely on.
#[test]
fn v4_signed_urls_match_googles_published_vectors() {
    let sa = ServiceAccount::from_json(SA_JSON).unwrap();
    let signer = RsaSigner::from_pem(&sa.private_key).unwrap();
    let vectors: Vec<Json> = serde_json::from_str(include_str!("fixtures/gcs_v4_vectors.json")).unwrap();
    assert!(vectors.len() >= 7);
    let with_query = vectors.iter().filter(|v| v.get("queryParameters").is_some()).count();
    assert_eq!(with_query, 2);
    for v in &vectors {
        let extra: Vec<(String, String)> = v
            .get("queryParameters")
            .and_then(Json::as_object)
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_owned()))
                    .collect()
            })
            .unwrap_or_default();
        let ts = time::OffsetDateTime::parse(
            v["timestamp"].as_str().unwrap(),
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap()
        .unix_timestamp();
        let r = V4Request {
            client_email: &sa.client_email,
            base: "https://storage.googleapis.com",
            host: "storage.googleapis.com",
            bucket: v["bucket"].as_str().unwrap(),
            object: v["object"].as_str().unwrap(),
            method: v["method"].as_str().unwrap(),
            expires_secs: v["expiration"].as_u64().unwrap(),
            now: ts,
        };
        let url = v4_signed_url_with(&signer, &r, &extra).unwrap();
        assert_eq!(url, v["expectedUrl"].as_str().unwrap(), "{}", v["description"]);
        if extra.is_empty() {
            assert_eq!(v4_signed_url(&signer, &r).unwrap(), url);
        }
    }
}

#[test]
fn driver_signed_urls_use_the_prefix_and_the_endpoint_host() {
    let sa = ServiceAccount::from_json(SA_JSON).unwrap();
    let mut cfg = GcsConfig::new("test-bucket", GcsAuth::ServiceAccount(sa));
    cfg.prefix = "fb/".into();
    let d = GcsDriver::with_clock(cfg, client(), Arc::new(|| 1_549_011_600)).unwrap();
    let url = d.signed_url("GET", "blobs/aa/some file", 600).unwrap();
    assert!(
        url.starts_with(
            "https://storage.googleapis.com/test-bucket/fb/blobs/aa/some%20file?X-Goog-Algorithm=GOOG4-RSA-SHA256&"
        ),
        "{url}"
    );
    assert!(
        url.contains("X-Goog-Date=20190201T090000Z&X-Goog-Expires=600&X-Goog-SignedHeaders=host&X-Goog-Signature=")
    );
    assert!(d.signed_url("GET", "k", 0).is_err());
    assert!(d.signed_url("GET", "k", 604_801).is_err(), "more than seven days");
    assert!(d.signed_url("GET", "k", 604_800).is_ok());
}

#[tokio::test]
async fn invalid_settings_are_refused() {
    let mock = GcsMock::start(0).await;
    let mut cfg = config(&mock, GcsAuth::None, CHUNK);
    cfg.endpoint = "ftp://x".into();
    assert!(GcsDriver::new(cfg, client()).is_err());
    let mut cfg = config(&mock, GcsAuth::None, CHUNK);
    cfg.bucket = " ".into();
    assert!(GcsDriver::new(cfg, client()).is_err());
    assert!(!format!("{:?}", config(&mock, GcsAuth::AccessToken("sekrit".into()), CHUNK)).contains("sekrit"));
    assert!(!format!("{:?}", GcsAuth::ServiceAccount(sa_for(&mock))).contains("PRIVATE"));
}

#[tokio::test]
async fn the_storage_service_runs_on_the_gcs_driver() {
    let mock = GcsMock::start(3).await;
    let driver: Arc<dyn StorageDriver> = Arc::new(self::driver(&mock, CHUNK));
    let env = Env::with(|_| {}, Some(driver)).await;
    env.bucket("gcsb", true).await;
    let data = bytes_of(700_000);
    let info = env
        .storage
        .put_object(&admin(), put("gcsb", "dir/file.bin", &data))
        .await
        .unwrap();
    env.storage
        .put_object(&admin(), put("gcsb", "dir/same.bin", &data))
        .await
        .unwrap();
    // Deduplicated: one blob in the bucket, uploaded through a resumable session.
    let names = mock.names();
    assert_eq!(names.len(), 1, "{names:?}");
    assert!(names[0].starts_with("fb/blobs/") && names[0].ends_with(&info.checksum_sha256.clone().unwrap()));
    assert!(mock.server.count("PUT /session/") >= 3);

    let r = env
        .storage
        .get_object(&admin(), "gcsb", "dir/file.bin", None, None)
        .await
        .unwrap();
    assert_eq!(read_all(r).await, data);
    let r = env
        .storage
        .get_object(
            &admin(),
            "gcsb",
            "dir/file.bin",
            None,
            Some(forgebase_storage::RangeSpec::Bounded(250_000, 450_000)),
        )
        .await
        .unwrap();
    assert_eq!(read_all(r).await, data[250_000..=450_000]);

    // A multipart upload session over staging, completed into the same blob.
    let up = env
        .storage
        .create_upload(
            &admin(),
            "gcsb",
            forgebase_storage::UploadInput {
                key: "assembled".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    env.storage
        .upload_part(&admin(), &up.id, 1, None, chunked(data[..300_000].to_vec(), 50_000))
        .await
        .unwrap();
    env.storage
        .upload_part(&admin(), &up.id, 2, None, chunked(data[300_000..].to_vec(), 50_000))
        .await
        .unwrap();
    let done = env
        .storage
        .complete_upload(&admin(), &up.id, Default::default())
        .await
        .unwrap();
    assert_eq!(done.checksum_sha256, info.checksum_sha256);
    assert_eq!(mock.names().len(), 1, "same content, same blob");

    // The reconciler removes orphans and stale staging from the bucket.
    env.driver.put("blobs/ab/orphan", body_of(vec![1; 5])).await.unwrap();
    env.driver
        .put("staging/single/stale", body_of(vec![1; 5]))
        .await
        .unwrap();
    let report = env.storage.reconcile(&admin()).await.unwrap();
    assert_eq!(report.staging_deleted, 1);
    assert!(
        !mock
            .names()
            .iter()
            .any(|k| k.ends_with("blobs/ab/orphan") || k.contains("staging/"))
    );
    assert!(mock.bad().is_empty(), "{:?}", mock.bad());
}

#[test]
fn credentials_are_picked_from_the_environment_in_a_fixed_order() {
    use forgebase_storage::gcs::auth_from_env;
    let env_of = |pairs: &'static [(&'static str, &'static str)]| {
        move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| (*v).to_owned())
    };
    let key = Some(SA_JSON.to_owned());
    // A key wins over a token and over everything else.
    let e = env_of(&[("FORGEBASE_STORAGE_GCS_ACCESS_TOKEN", "t")]);
    assert!(matches!(
        auth_from_env(&e, key, true).unwrap(),
        GcsAuth::ServiceAccount(_)
    ));
    assert!(
        auth_from_env(&e, Some("{}".into()), false).is_err(),
        "a broken key is an error, not a fallback"
    );
    assert!(matches!(auth_from_env(&e, None, false).unwrap(), GcsAuth::AccessToken(t) if t == "t"));
    // A custom endpoint without credentials is an emulator.
    let none = env_of(&[]);
    assert!(matches!(auth_from_env(&none, None, true).unwrap(), GcsAuth::None));
    // Otherwise: the metadata server, at GCE_METADATA_HOST when set.
    assert!(matches!(
        auth_from_env(&none, None, false).unwrap(),
        GcsAuth::Metadata { endpoint } if endpoint == "http://metadata.google.internal"
    ));
    let host = env_of(&[("GCE_METADATA_HOST", "169.254.169.254")]);
    assert!(matches!(
        auth_from_env(&host, None, false).unwrap(),
        GcsAuth::Metadata { endpoint } if endpoint == "http://169.254.169.254"
    ));
}

async fn fetch(url: &str) -> forgebase_auth::social::HttpResponse {
    use forgebase_auth::social::HttpClient as _;
    client()
        .send(forgebase_auth::social::HttpRequest {
            method: "GET",
            url: url.to_owned(),
            headers: Vec::new(),
            body: Vec::new(),
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn presigned_downloads_verify_and_are_scoped_to_one_object() {
    let mock = GcsMock::start(0).await;
    let d = driver(&mock, CHUNK);
    d.put("blobs/aa/one", body_of(b"first".to_vec())).await.unwrap();
    d.put("blobs/bb/two", body_of(b"second".to_vec())).await.unwrap();
    assert_eq!(d.presign_support(), Ok(604_800));
    let url = d
        .presign_get(&PresignGet {
            key: "blobs/aa/one",
            now: NOW,
            expires_secs: 600,
            content_type: Some("text/plain"),
            content_disposition: Some("attachment; filename=\"one.txt\""),
        })
        .unwrap();
    assert!(
        url.starts_with(&format!(
            "{}/{BUCKET}/fb/blobs/aa/one?X-Goog-Algorithm=GOOG4-RSA-SHA256&",
            mock.url()
        )),
        "{url}"
    );
    assert!(url.contains("&X-Goog-Date=20261109T120000Z&X-Goog-Expires=600&X-Goog-SignedHeaders=host&"));
    assert!(url.contains(
        "&response-content-disposition=attachment%3B%20filename%3D%22one.txt%22&response-content-type=text%2Fplain&X-Goog-Signature="
    ));
    let r = fetch(&url).await;
    assert_eq!((r.status, r.body.as_slice()), (200, &b"first"[..]));
    assert_eq!(r.header("content-type"), Some("text/plain"));
    assert_eq!(
        r.header("content-disposition"),
        Some("attachment; filename=\"one.txt\"")
    );
    for forged in [
        url.replace("fb/blobs/aa/one", "fb/blobs/bb/two"),
        url.replace("X-Goog-Expires=600", "X-Goog-Expires=6000"),
        url.replace("text%2Fplain", "text%2Fhtml"),
    ] {
        assert_eq!(fetch(&forged).await.status, 403, "{forged}");
    }
    assert_eq!(mock.st.lock().unwrap().refused.len(), 3);
}

#[tokio::test]
async fn native_signed_downloads_are_served_by_gcs() {
    let mock = GcsMock::start(0).await;
    let driver: Arc<dyn StorageDriver> = Arc::new(self::driver(&mock, CHUNK));
    let env = Env::with(
        |s| {
            s.signed_urls = SignedUrlsMode::Native;
            s.max_signed_ttl_secs = 30 * 86_400;
        },
        Some(driver),
    )
    .await;
    env.bucket("n", false).await;
    let data = bytes_of(5000);
    let info = env
        .storage
        .put_object(
            &admin(),
            forgebase_storage::PutRequest {
                content_type: Some("application/octet-stream".into()),
                ..put("n", "dir/data.bin", &data)
            },
        )
        .await
        .unwrap();
    let sha = info.checksum_sha256.unwrap();
    let sign = |expires_in: u64| SignRequest {
        key: "dir/data.bin".into(),
        method: None,
        expires_in: Some(expires_in),
        version_id: None,
        download_as: None,
        content_type: None,
    };
    let signed = env.storage.sign_url(&admin(), "n", sign(120)).await.unwrap();
    assert_eq!(signed.mode, SignedMode::Native);
    assert!(
        signed
            .url
            .starts_with(&format!("{}/{BUCKET}/fb/blobs/{}/{sha}?", mock.url(), &sha[..2])),
        "{}",
        signed.url
    );
    let r = fetch(&signed.url).await;
    assert_eq!((r.status, &r.body), (200, &data));
    assert_eq!(r.header("content-type"), Some("application/octet-stream"));
    assert_eq!(
        r.header("content-disposition"),
        None,
        "inert content is served inline, as by the proxy"
    );
    // Capped at the seven days GCS allows.
    let long = env.storage.sign_url(&admin(), "n", sign(10 * 86_400)).await.unwrap();
    assert!(long.url.contains("&X-Goog-Expires=604800&"), "{}", long.url);
    assert_eq!(fetch(&long.url).await.status, 200);
    assert!(mock.st.lock().unwrap().refused.is_empty());
    assert!(mock.bad().is_empty(), "{:?}", mock.bad());
}

#[tokio::test]
async fn workload_identity_cannot_sign_so_auto_uses_the_proxy() {
    let mock = GcsMock::start(0).await;
    let wi = || -> Arc<dyn StorageDriver> {
        Arc::new(
            GcsDriver::with_clock(
                config(&mock, GcsAuth::Metadata { endpoint: mock.url() }, CHUNK),
                client(),
                Arc::new(|| NOW),
            )
            .unwrap(),
        )
    };
    let why = wi().presign_support().unwrap_err();
    assert!(why.contains("workload identity"), "{why}");
    let req = || SignRequest {
        key: "k".into(),
        method: None,
        expires_in: None,
        version_id: None,
        download_as: None,
        content_type: None,
    };
    let env = Env::with(|s| s.signed_urls = SignedUrlsMode::Auto, Some(wi())).await;
    env.bucket("b", false).await;
    env.storage
        .put_object(&admin(), put("b", "k", b"proxied"))
        .await
        .unwrap();
    let signed = env.storage.sign_url(&admin(), "b", req()).await.unwrap();
    assert_eq!(signed.mode, SignedMode::Proxy);
    let (s, _, body) = env
        .req(
            None,
            axum::http::Method::GET,
            &signed.url,
            &[],
            axum::body::Body::empty(),
        )
        .await;
    assert_eq!((s.as_u16(), body.as_slice()), (200, &b"proxied"[..]));

    let env = Env::with(|s| s.signed_urls = SignedUrlsMode::Native, Some(wi())).await;
    env.bucket("b", false).await;
    env.storage.put_object(&admin(), put("b", "k", b"x")).await.unwrap();
    match env.storage.sign_url(&admin(), "b", req()).await {
        Err(StorageError::Conflict {
            code: "native_signing_unavailable",
            message,
        }) => assert!(
            message.contains("gcs driver") && message.contains("workload identity"),
            "{message}"
        ),
        other => panic!("{other:?}"),
    }
    // Fixed tokens and emulators without credentials cannot sign either.
    for auth in [GcsAuth::AccessToken("t".into()), GcsAuth::None] {
        let d = GcsDriver::new(config(&mock, auth, CHUNK), client()).unwrap();
        assert!(d.presign_support().is_err());
    }
}
