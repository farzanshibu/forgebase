//! The Azure Blob driver against a mock of Blob Storage, Microsoft Entra and
//! IMDS (no network): SharedKey signatures (Microsoft's documented string to
//! sign, recomputed independently on every request), SAS, client credentials,
//! managed identity, block uploads, retries, paging and signed URLs, and the
//! storage service's native signed downloads fetched from the mock.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use common::{Env, admin, bytes_of, chunked, put, read_all};
use forgebase_auth::net::{HttpsClient, TlsSettings};
use forgebase_core::SignedUrlsMode;
use forgebase_storage::azure::{
    SasOverrides, SasParams, service_sas, service_sas_with, shared_key_authorization, shared_key_string_to_sign,
};
use forgebase_storage::cloud::{b64_decode, http_date};
use forgebase_storage::driver::collect;
use forgebase_storage::{
    AzureAuth, AzureConfig, AzureDriver, PresignGet, SasPermission, SignRequest, SignedMode, StorageDriver,
    StorageError, body_of,
};
use support::azure_mock::{ACCOUNT, AzureMock, CONTAINER, KEY, Mode, SAS};
use support::{chunks_of, conformance, pattern};

const NOW: i64 = 1_794_225_600;

fn client() -> Arc<HttpsClient> {
    Arc::new(HttpsClient::new(&TlsSettings::new()).unwrap().with_max_body(2 << 20))
}

fn config(mock: &AzureMock, auth: AzureAuth, part: usize) -> AzureConfig {
    let mut cfg = AzureConfig::new(ACCOUNT, CONTAINER, auth);
    cfg.endpoint = Some(mock.endpoint());
    cfg.prefix = "fb".into();
    cfg.part_size = part;
    cfg.read_chunk = 100_000;
    cfg.retry.base_delay = Duration::from_millis(1);
    cfg
}

fn shared_key(mock: &AzureMock, part: usize) -> AzureDriver {
    mock.st.lock().unwrap().expect_date = Some(http_date(NOW));
    AzureDriver::with_clock(
        config(mock, AzureAuth::shared_key(KEY).unwrap(), part),
        client(),
        Arc::new(|| NOW),
    )
    .unwrap()
}

fn clocked(mock: &AzureMock, auth: AzureAuth) -> (AzureDriver, Arc<AtomicI64>) {
    let now = Arc::new(AtomicI64::new(NOW));
    let n = now.clone();
    let d = AzureDriver::with_clock(
        config(mock, auth, 64_000),
        client(),
        Arc::new(move || n.load(Ordering::SeqCst)),
    )
    .unwrap();
    (d, now)
}

fn h(k: &str, v: &str) -> (String, String) {
    (k.to_owned(), v.to_owned())
}

/// Microsoft's *Authorize with Shared Key* example (List Containers).
#[test]
fn shared_key_string_to_sign_matches_microsofts_documented_example() {
    let sts = shared_key_string_to_sign(
        "GET",
        "myaccount",
        "/",
        &[h("comp", "list")],
        &[
            h("x-ms-date", "Fri, 26 Jun 2015 23:39:12 GMT"),
            h("x-ms-version", "2015-02-21"),
        ],
        0,
    );
    assert_eq!(
        sts,
        "GET\n\n\n\n\n\n\n\n\n\n\n\nx-ms-date:Fri, 26 Jun 2015 23:39:12 GMT\nx-ms-version:2015-02-21\n/myaccount/\ncomp:list"
    );
    // HMAC-SHA256 with Azurite's key, recomputed with Python's hmac module.
    let key = b64_decode(KEY).unwrap();
    assert_eq!(
        shared_key_authorization("myaccount", &key, &sts),
        "SharedKey myaccount:50/jBo1FiKZ/fGm1D+cmcfX2dhs7IMHVve2TIXFYWK8="
    );
}

#[test]
fn shared_key_covers_body_headers_and_query_parameters() {
    let sts = shared_key_string_to_sign(
        "PUT",
        "devstoreaccount1",
        "/devstoreaccount1/blobs/fb/blobs/aa/x",
        &[],
        &[
            h("content-type", "application/octet-stream"),
            h("x-ms-version", "2021-08-06"),
            h("x-ms-date", "Fri, 26 Jun 2015 23:39:12 GMT"),
            h("x-ms-blob-type", "BlockBlob"),
            h("authorization", "ignored"),
        ],
        5,
    );
    assert_eq!(
        sts,
        "PUT\n\n\n5\n\napplication/octet-stream\n\n\n\n\n\n\nx-ms-blob-type:BlockBlob\nx-ms-date:Fri, 26 Jun 2015 23:39:12 GMT\nx-ms-version:2021-08-06\n/devstoreaccount1/devstoreaccount1/blobs/fb/blobs/aa/x"
    );
    let key = b64_decode(KEY).unwrap();
    assert_eq!(
        shared_key_authorization("devstoreaccount1", &key, &sts),
        "SharedKey devstoreaccount1:S8pAbpQDD2qtmvr0alttuoWjYHTnRL+OqSHDGmn+RQM="
    );
    // Parameters: lower-cased names, sorted, repeated values joined with commas.
    let sts = shared_key_string_to_sign(
        "GET",
        "a",
        "/c",
        &[
            h("restype", "container"),
            h("Comp", "list"),
            h("prefix", "p/q"),
            h("comp", "x"),
        ],
        &[],
        0,
    );
    assert!(
        sts.ends_with("/a/c\ncomp:list,x\nprefix:p/q\nrestype:container"),
        "{sts}"
    );
}

#[test]
fn service_sas_matches_an_independent_computation() {
    let key = b64_decode(KEY).unwrap();
    let params = |blob, permission, start, expiry, https_only| SasParams {
        account: ACCOUNT,
        container: CONTAINER,
        blob,
        permission,
        start,
        expiry,
        https_only,
    };
    let q = service_sas(
        &key,
        &params("fb/blobs/aa/x", SasPermission::Read, Some(NOW - 300), NOW + 900, false),
    );
    // The string to sign (version 2021-08-06) and its HMAC were computed with Python.
    assert_eq!(
        q,
        "sv=2021-08-06&sr=b&sp=r&st=2026-11-09T11%3A55%3A00Z&se=2026-11-09T12%3A15%3A00Z&sig=wLG5GSx3Ji5TYpSQ3DCC6%2BWgyP8SzMcZ5u2C3J%2Burd0%3D"
    );
    let w = service_sas(&key, &params("b", SasPermission::Write, None, NOW + 60, true));
    assert!(
        w.contains("sp=cw") && w.contains("spr=https") && !w.contains("st="),
        "{w}"
    );
    assert!(service_sas(&key, &params("b", SasPermission::Delete, None, NOW, false)).contains("sp=d&"));
    // The response overrides of a native download are signed (rscd, rsct). The
    // string to sign and its HMAC were computed with Python from Microsoft's
    // field order, which reproduces the vector above when both are empty.
    let o = service_sas_with(
        &key,
        &params("fb/blobs/aa/x", SasPermission::Read, Some(NOW - 300), NOW + 900, false),
        &SasOverrides {
            content_disposition: Some("attachment; filename=\"x.txt\""),
            content_type: Some("text/plain"),
        },
    );
    assert_eq!(
        o,
        "sv=2021-08-06&sr=b&sp=r&st=2026-11-09T11%3A55%3A00Z&se=2026-11-09T12%3A15%3A00Z\
         &rscd=attachment%3B%20filename%3D%22x.txt%22&rsct=text%2Fplain\
         &sig=cJN7Y1A2nmU2TNwm8kH0GXoLa%2FVAFbXgwBrVq0Ci9rs%3D"
    );
    assert_eq!(
        service_sas_with(
            &key,
            &params("fb/blobs/aa/x", SasPermission::Read, Some(NOW - 300), NOW + 900, false),
            &SasOverrides::default()
        ),
        q
    );
}

#[tokio::test]
async fn driver_signed_urls_are_service_sas_urls() {
    let mock = AzureMock::start(Mode::SharedKey, true, 0).await;
    let mut cfg = config(&mock, AzureAuth::shared_key(KEY).unwrap(), 1000);
    cfg.endpoint = Some("http://127.0.0.1:10000/devstoreaccount1".into());
    let d = AzureDriver::with_clock(cfg, client(), Arc::new(|| NOW)).unwrap();
    let url = d.signed_url(SasPermission::Read, "blobs/aa/x", 900).unwrap();
    assert_eq!(
        url,
        "http://127.0.0.1:10000/devstoreaccount1/blobs/fb/blobs/aa/x?sv=2021-08-06&sr=b&sp=r&st=2026-11-09T11%3A55%3A00Z&se=2026-11-09T12%3A15%3A00Z&sig=wLG5GSx3Ji5TYpSQ3DCC6%2BWgyP8SzMcZ5u2C3J%2Burd0%3D"
    );
    assert!(d.signed_url(SasPermission::Read, "k", 0).is_err());
    // A SAS or an Entra identity cannot sign.
    let sas = AzureDriver::new(config(&mock, AzureAuth::Sas(SAS.into()), 1000), client()).unwrap();
    assert!(matches!(
        sas.signed_url(SasPermission::Read, "k", 60),
        Err(StorageError::Invalid {
            code: "azure_signing_unavailable",
            ..
        })
    ));
}

#[tokio::test]
async fn conformance_with_shared_key_on_azure_and_azurite_style_urls() {
    for (path_style, page) in [(false, 3), (true, 0)] {
        let mock = AzureMock::start(Mode::SharedKey, path_style, page).await;
        let d = shared_key(&mock, 64_000);
        conformance::run(&d, "azure", 300_000).await;
        assert!(mock.bad().is_empty(), "path_style={path_style}: {:?}", mock.bad());
        assert!(mock.names().iter().all(|n| n.starts_with("fb/")), "{:?}", mock.names());
        assert!(
            mock.st.lock().unwrap().deleted_with.iter().all(|d| d == "include"),
            "deletes include snapshots"
        );
    }
}

#[tokio::test]
async fn conformance_with_a_sas_token() {
    let mock = AzureMock::start(Mode::Sas, false, 0).await;
    let d = AzureDriver::new(config(&mock, AzureAuth::Sas(format!("?{SAS}")), 64_000), client()).unwrap();
    conformance::run(&d, "azure", 200_000).await;
    assert!(mock.bad().is_empty(), "{:?}", mock.bad());
}

#[tokio::test]
async fn conformance_with_entra_client_credentials_and_token_refresh() {
    let mock = AzureMock::start(Mode::Bearer, false, 0).await;
    let auth = AzureAuth::ClientSecret {
        tenant: "contoso".into(),
        client_id: "app-id".into(),
        client_secret: "shh".into(),
        authority: mock.server.url(),
    };
    let d = AzureDriver::new(config(&mock, auth.clone(), 64_000), client()).unwrap();
    conformance::run(&d, "azure", 150_000).await;
    assert_eq!(mock.st.lock().unwrap().tokens_issued, 1, "one cached token");
    assert!(mock.bad().is_empty(), "{:?}", mock.bad());

    let (d, now) = clocked(&mock, auth);
    d.put("blobs/aa/t", body_of(vec![1; 5])).await.unwrap();
    assert_eq!(
        mock.st.lock().unwrap().tokens_issued,
        2,
        "a second driver, its own token"
    );
    now.store(NOW + 3600 - 100, Ordering::SeqCst);
    d.head("blobs/aa/t").await.unwrap().unwrap();
    assert_eq!(mock.st.lock().unwrap().tokens_issued, 3, "refreshed before expiry");
    // A token the service forgot (401) is replaced.
    mock.st.lock().unwrap().accepted.clear();
    d.head("blobs/aa/t").await.unwrap().unwrap();
    assert_eq!(mock.st.lock().unwrap().tokens_issued, 4);

    let bad = AzureDriver::new(
        config(
            &mock,
            AzureAuth::ClientSecret {
                tenant: "contoso".into(),
                client_id: "app-id".into(),
                client_secret: "wrong".into(),
                authority: mock.server.url(),
            },
            1000,
        ),
        client(),
    )
    .unwrap();
    let err = bad.ping().await.unwrap_err();
    assert!(matches!(&err, StorageError::Driver(m) if m.contains("400")), "{err:?}");
}

#[tokio::test]
async fn managed_identity_uses_imds_or_the_app_service_endpoint() {
    let mock = AzureMock::start(Mode::Bearer, false, 0).await;
    let imds = |header: Option<&str>, id: Option<&str>| AzureAuth::ManagedIdentity {
        endpoint: format!("{}/metadata/identity/oauth2/token", mock.server.url()),
        identity_header: header.map(str::to_owned),
        client_id: id.map(str::to_owned),
    };
    let d = AzureDriver::new(config(&mock, imds(None, Some("uami-1")), 1000), client()).unwrap();
    d.put("blobs/aa/mi", body_of(vec![3; 30])).await.unwrap();
    assert_eq!(
        collect(d.get("blobs/aa/mi", None).await.unwrap()).await.unwrap(),
        vec![3; 30]
    );
    let d = AzureDriver::new(config(&mock, imds(Some("secret-header"), None), 1000), client()).unwrap();
    d.head("blobs/aa/mi").await.unwrap().unwrap();
    let reqs = mock.st.lock().unwrap().token_requests.clone();
    assert_eq!(reqs, ["imds uami-1", "imds "], "{reqs:?}");
}

#[tokio::test]
async fn big_writes_are_blocks_committed_by_a_block_list() {
    let mock = AzureMock::start(Mode::SharedKey, false, 0).await;
    let d = shared_key(&mock, 1000);
    let small = pattern(1000);
    d.put("blobs/aa/small", chunks_of(small.clone(), 333)).await.unwrap();
    assert_eq!(
        mock.server.count("PUT /blobs/fb/blobs/aa/small?"),
        1,
        "one Put Blob: {:?}",
        mock.server.lines()
    );
    assert_eq!(mock.server.count("PUT /blobs/fb/blobs/aa/small?comp"), 0);

    let big = pattern(3500);
    assert_eq!(d.put("blobs/bb/big", chunks_of(big.clone(), 333)).await.unwrap(), 3500);
    assert_eq!(
        mock.server.count("PUT /blobs/fb/blobs/bb/big?comp&blockid"),
        4,
        "3 full blocks + the rest"
    );
    assert_eq!(
        mock.server.count("PUT /blobs/fb/blobs/bb/big?comp"),
        5,
        "and the block list"
    );
    assert_eq!(mock.blob("fb/blobs/bb/big").unwrap(), big);
    assert!(mock.bad().is_empty(), "{:?}", mock.bad());

    // Reads page through x-ms-range requests of at most read_chunk bytes.
    let lines_before = mock.server.lines().len();
    let all = collect(d.get("blobs/bb/big", None).await.unwrap()).await.unwrap();
    assert_eq!(all, big);
    let gets = mock.server.lines()[lines_before..]
        .iter()
        .filter(|l| l.starts_with("GET"))
        .count();
    assert_eq!(gets, 1, "3500 bytes fit one read of 100000");
    let part = collect(d.get("blobs/bb/big", Some((1000, 2500))).await.unwrap())
        .await
        .unwrap();
    assert_eq!(part, big[1000..2500]);
}

#[tokio::test]
async fn transient_failures_are_retried_and_every_attempt_is_signed_afresh() {
    let mock = AzureMock::start(Mode::SharedKey, false, 0).await;
    let d = shared_key(&mock, 1000);
    d.put("blobs/aa/r", body_of(vec![5; 50])).await.unwrap();
    mock.fail("HEAD /blobs/fb/blobs/aa/r", 503, 2);
    let before = mock.server.count("HEAD");
    assert_eq!(d.head("blobs/aa/r").await.unwrap().unwrap().size, 50);
    assert_eq!(mock.server.count("HEAD") - before, 3);
    mock.fail("PUT /blobs/fb/blobs/aa/w", 429, 1);
    mock.fail("PUT /blobs/fb/blobs/aa/w", 500, 1);
    d.put("blobs/aa/w", body_of(vec![1; 10])).await.unwrap();
    assert!(
        mock.bad().is_empty(),
        "retries carry valid signatures: {:?}",
        mock.bad()
    );
    // A 404 is an answer.
    let before = mock.server.count("HEAD");
    assert_eq!(d.head("blobs/aa/none").await.unwrap(), None);
    assert_eq!(mock.server.count("HEAD") - before, 1);
    // Out of attempts.
    mock.fail("HEAD /blobs/fb/blobs/aa/r", 503, 99);
    let err = d.head("blobs/aa/r").await.unwrap_err();
    assert!(
        matches!(&err, StorageError::Driver(m) if m.contains("503") && m.contains("ServerBusy")),
        "{err:?}"
    );
}

#[tokio::test]
async fn asynchronous_copies_are_awaited() {
    let mock = AzureMock::start(Mode::SharedKey, false, 0).await;
    let d = shared_key(&mock, 1000);
    d.put("blobs/aa/src", body_of(vec![8; 40])).await.unwrap();
    mock.st.lock().unwrap().pending_copies = 3;
    d.copy("blobs/aa/src", "blobs/aa/dst").await.unwrap();
    assert_eq!(
        mock.server.count("HEAD /blobs/fb/blobs/aa/dst"),
        4,
        "polled until the copy finished"
    );
    assert_eq!(mock.blob("fb/blobs/aa/dst").unwrap(), vec![8; 40]);
}

#[tokio::test]
async fn listing_pages_and_folds_by_delimiter() {
    let mock = AzureMock::start(Mode::SharedKey, false, 2).await;
    let d = shared_key(&mock, 1000);
    for k in ["a/1", "a/2", "a/3", "b/1", "b/2", "top"] {
        d.put(k, body_of(k.as_bytes().to_vec())).await.unwrap();
    }
    let all = d.list("").await.unwrap();
    assert_eq!(all.len(), 6);
    assert!(
        mock.server.count("GET /blobs?") >= 3,
        "paged with markers: {:?}",
        mock.server.lines()
    );
    assert_eq!(all[0].key, "a/1", "the prefix is stripped");
    assert_eq!(all[0].modified_ms, 1_794_225_600_000);
    let (objects, prefixes) = d.list_with_delimiter("", "/").await.unwrap();
    assert_eq!(objects.iter().map(|o| o.key.as_str()).collect::<Vec<_>>(), ["top"]);
    assert_eq!(prefixes, ["a/", "b/"]);
    assert!(mock.bad().is_empty(), "{:?}", mock.bad());
}

#[tokio::test]
async fn invalid_settings_are_refused() {
    let mock = AzureMock::start(Mode::SharedKey, false, 0).await;
    assert!(AzureAuth::shared_key("not base64!").is_err());
    let mut cfg = config(&mock, AzureAuth::Sas("sv=1".into()), 1000);
    cfg.endpoint = Some("ftp://x".into());
    assert!(AzureDriver::new(cfg, client()).is_err());
    let mut cfg = config(&mock, AzureAuth::Sas("sv=1".into()), 1000);
    cfg.container = String::new();
    assert!(AzureDriver::new(cfg, client()).is_err());
    let cfg = config(&mock, AzureAuth::shared_key(KEY).unwrap(), 1000);
    let dbg = format!("{cfg:?}{:?}", AzureAuth::Sas("sig=sekrit".into()));
    assert!(!dbg.contains("sekrit") && !dbg.contains(KEY), "{dbg}");
    // The default endpoint is the account's blob host.
    let mut cfg = AzureConfig::new("myacct", "c", AzureAuth::Sas("sv=1".into()));
    cfg.endpoint = None;
    assert!(AzureDriver::new(cfg, client()).is_ok());
}

#[tokio::test]
async fn the_storage_service_runs_on_the_azure_driver() {
    let mock = AzureMock::start(Mode::SharedKey, true, 3).await;
    let driver: Arc<dyn StorageDriver> = Arc::new(shared_key(&mock, 100_000));
    let env = Env::with(|_| {}, Some(driver)).await;
    env.bucket("azb", true).await;
    let data = bytes_of(700_000);
    let info = env
        .storage
        .put_object(&admin(), put("azb", "dir/file.bin", &data))
        .await
        .unwrap();
    env.storage
        .put_object(&admin(), put("azb", "dir/same.bin", &data))
        .await
        .unwrap();
    let names = mock.names();
    assert_eq!(names.len(), 1, "{names:?}");
    assert!(names[0].starts_with("fb/blobs/") && names[0].ends_with(&info.checksum_sha256.clone().unwrap()));
    assert!(mock.server.count("PUT /devstoreaccount1/blobs/fb/blobs") >= 1);

    let r = env
        .storage
        .get_object(&admin(), "azb", "dir/file.bin", None, None)
        .await
        .unwrap();
    assert_eq!(read_all(r).await, data);
    let r = env
        .storage
        .get_object(
            &admin(),
            "azb",
            "dir/file.bin",
            None,
            Some(forgebase_storage::RangeSpec::Bounded(250_000, 450_000)),
        )
        .await
        .unwrap();
    assert_eq!(read_all(r).await, data[250_000..=450_000]);

    let up = env
        .storage
        .create_upload(
            &admin(),
            "azb",
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
    use forgebase_storage::azure::auth_from_env;
    let env_of = |pairs: &'static [(&'static str, &'static str)]| {
        move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| (*v).to_owned())
    };
    let all = env_of(&[
        ("FORGEBASE_STORAGE_AZURE_ACCOUNT_KEY", KEY),
        ("FORGEBASE_STORAGE_AZURE_SAS_TOKEN", "sv=1"),
        ("FORGEBASE_STORAGE_AZURE_TENANT_ID", "t"),
        ("FORGEBASE_STORAGE_AZURE_CLIENT_ID", "c"),
        ("FORGEBASE_STORAGE_AZURE_CLIENT_SECRET", "s"),
    ]);
    assert!(matches!(auth_from_env(&all).unwrap(), AzureAuth::SharedKey(_)));
    let bad_key = env_of(&[("FORGEBASE_STORAGE_AZURE_ACCOUNT_KEY", "not base64!")]);
    assert!(auth_from_env(&bad_key).is_err());
    let sas = env_of(&[
        ("FORGEBASE_STORAGE_AZURE_SAS_TOKEN", "sv=1"),
        ("FORGEBASE_STORAGE_AZURE_CLIENT_SECRET", "s"),
    ]);
    assert!(matches!(auth_from_env(&sas).unwrap(), AzureAuth::Sas(t) if t == "sv=1"));
    let entra = env_of(&[
        ("FORGEBASE_STORAGE_AZURE_TENANT_ID", "t"),
        ("FORGEBASE_STORAGE_AZURE_CLIENT_ID", "c"),
        ("FORGEBASE_STORAGE_AZURE_CLIENT_SECRET", "s"),
    ]);
    assert!(matches!(auth_from_env(&entra).unwrap(), AzureAuth::ClientSecret { tenant, .. } if tenant == "t"));
    // A client id alone selects a user-assigned managed identity.
    let mi = env_of(&[("FORGEBASE_STORAGE_AZURE_CLIENT_ID", "c")]);
    assert!(matches!(
        auth_from_env(&mi).unwrap(),
        AzureAuth::ManagedIdentity { endpoint, identity_header: None, client_id: Some(c) }
            if endpoint == "http://169.254.169.254/metadata/identity/oauth2/token" && c == "c"
    ));
    let app_service = env_of(&[
        ("IDENTITY_ENDPOINT", "http://127.0.0.1:42356/msi/token"),
        ("IDENTITY_HEADER", "h"),
    ]);
    assert!(matches!(
        auth_from_env(&app_service).unwrap(),
        AzureAuth::ManagedIdentity { endpoint, identity_header: Some(h), .. }
            if endpoint == "http://127.0.0.1:42356/msi/token" && h == "h"
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

fn sign_req(key: &str, expires_in: Option<u64>) -> SignRequest {
    SignRequest {
        key: key.into(),
        method: None,
        expires_in,
        version_id: None,
        download_as: None,
        content_type: None,
    }
}

#[tokio::test]
async fn presigned_downloads_verify_and_are_scoped_to_one_blob() {
    for path_style in [false, true] {
        let mock = AzureMock::start(Mode::SharedKey, path_style, 0).await;
        let d = shared_key(&mock, 64_000);
        d.put("blobs/aa/one", body_of(b"first".to_vec())).await.unwrap();
        d.put("blobs/bb/two", body_of(b"second".to_vec())).await.unwrap();
        assert_eq!(
            d.presign_support(),
            Ok(u64::MAX),
            "no provider limit; ForgeBase's applies"
        );
        let url = d
            .presign_get(&PresignGet {
                key: "blobs/aa/one",
                now: NOW,
                expires_secs: 900,
                content_type: Some("text/plain"),
                content_disposition: Some("attachment; filename=\"one.txt\""),
            })
            .unwrap();
        assert!(
            url.starts_with(&format!(
                "{}/{CONTAINER}/fb/blobs/aa/one?sv=2021-08-06&sr=b&sp=r&",
                mock.endpoint()
            )),
            "{url}"
        );
        assert!(url.contains("&se=2026-11-09T12%3A15%3A00Z&"));
        let r = fetch(&url).await;
        assert_eq!((r.status, r.body.as_slice()), (200, &b"first"[..]));
        assert_eq!(r.header("content-type"), Some("text/plain"));
        assert_eq!(
            r.header("content-disposition"),
            Some("attachment; filename=\"one.txt\"")
        );
        for forged in [
            url.replace("fb/blobs/aa/one", "fb/blobs/bb/two"),
            url.replace("se=2026-11-09T12%3A15", "se=2027-11-09T12%3A15"),
            url.replace("rsct=text%2Fplain", "rsct=text%2Fhtml"),
            url.replace("sp=r", "sp=rw"),
        ] {
            assert_eq!(fetch(&forged).await.status, 403, "{forged}");
        }
        assert_eq!(mock.st.lock().unwrap().refused.len(), 4);
        assert!(mock.bad().is_empty(), "{:?}", mock.bad());
    }
}

#[tokio::test]
async fn native_signed_downloads_are_served_by_azure() {
    let mock = AzureMock::start(Mode::SharedKey, true, 0).await;
    let driver: Arc<dyn StorageDriver> = Arc::new(shared_key(&mock, 100_000));
    let env = Env::with(|s| s.signed_urls = SignedUrlsMode::Native, Some(driver)).await;
    env.bucket("n", false).await;
    let page = b"<html><script>alert(1)</script></html>".to_vec();
    let info = env
        .storage
        .put_object(
            &admin(),
            forgebase_storage::PutRequest {
                content_type: Some("text/html".into()),
                ..put("n", "site/index.html", &page)
            },
        )
        .await
        .unwrap();
    let sha = info.checksum_sha256.unwrap();
    let signed = env
        .storage
        .sign_url(&admin(), "n", sign_req("site/index.html", Some(600)))
        .await
        .unwrap();
    assert_eq!(signed.mode, SignedMode::Native);
    assert!(
        signed.url.starts_with(&format!(
            "{}/{CONTAINER}/fb/blobs/{}/{sha}?",
            mock.endpoint(),
            &sha[..2]
        )),
        "{}",
        signed.url
    );
    let r = fetch(&signed.url).await;
    assert_eq!((r.status, &r.body), (200, &page));
    assert_eq!(r.header("content-type"), Some("text/html"));
    // Active content is an attachment on the provider's origin too.
    assert_eq!(
        r.header("content-disposition"),
        Some("attachment; filename=\"index.html\"")
    );
    assert!(mock.st.lock().unwrap().refused.is_empty());
    assert!(mock.bad().is_empty(), "{:?}", mock.bad());
}

#[tokio::test]
async fn without_the_account_key_auto_uses_the_proxy_and_native_is_refused() {
    let mock = AzureMock::start(Mode::Sas, false, 0).await;
    let sas = || -> Arc<dyn StorageDriver> {
        Arc::new(AzureDriver::new(config(&mock, AzureAuth::Sas(SAS.into()), 64_000), client()).unwrap())
    };
    let env = Env::with(|s| s.signed_urls = SignedUrlsMode::Auto, Some(sas())).await;
    env.bucket("b", false).await;
    env.storage
        .put_object(&admin(), put("b", "k", b"proxied"))
        .await
        .unwrap();
    let signed = env.storage.sign_url(&admin(), "b", sign_req("k", None)).await.unwrap();
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

    let env = Env::with(|s| s.signed_urls = SignedUrlsMode::Native, Some(sas())).await;
    env.bucket("b", false).await;
    env.storage.put_object(&admin(), put("b", "k", b"x")).await.unwrap();
    match env.storage.sign_url(&admin(), "b", sign_req("k", None)).await {
        Err(StorageError::Conflict {
            code: "native_signing_unavailable",
            message,
        }) => assert!(
            message.contains("azure driver") && message.contains("account key"),
            "{message}"
        ),
        other => panic!("{other:?}"),
    }
    // Entra identities and managed identities cannot sign either.
    for auth in [
        AzureAuth::AccessToken("t".into()),
        AzureAuth::ClientSecret {
            tenant: "t".into(),
            client_id: "c".into(),
            client_secret: "s".into(),
            authority: mock.server.url(),
        },
        AzureAuth::ManagedIdentity {
            endpoint: mock.server.url(),
            identity_header: None,
            client_id: None,
        },
    ] {
        let d = AzureDriver::new(config(&mock, auth, 64_000), client()).unwrap();
        let why = d.presign_support().unwrap_err();
        assert!(why.contains("account key"), "{why}");
    }
    assert!(mock.bad().is_empty(), "{:?}", mock.bad());
}
