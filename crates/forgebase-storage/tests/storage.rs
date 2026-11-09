//! Object storage against real persistence: SQLite metadata, the local
//! driver and the real HTTP router. Scenarios #10 and #11 of SPEC §27 plus
//! ranges, resumable uploads, checksums, deduplication, versions, signed
//! URLs, quotas, access control and the reconciler.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::body::Body;
use axum::http::{Method, StatusCode};
use common::*;
use forgebase_data::AccessContext;
use forgebase_storage::{
    Acl, AclEntry, BucketInput, BucketPatch, CompleteInput, CompletePart, Permission, RangeSpec, SignRequest,
    SignedMethod, StorageError, UploadInput, Visibility,
};
use serde_json::json;

fn up_input(key: &str, kind: &str, size: Option<u64>) -> UploadInput {
    UploadInput {
        key: key.into(),
        kind: Some(kind.into()),
        size,
        ..UploadInput::default()
    }
}

// ---- scenario #10: upload and retrieve an object ------------------------

#[tokio::test]
async fn scenario_10_upload_and_retrieve_an_object_over_http() {
    let env = Env::new().await;
    let (s, b) = env
        .json(
            Some(admin()),
            Method::POST,
            "/api/v1/storage/buckets",
            json!({"name": "photos"}),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    assert_eq!(b["visibility"], "private");

    let data = bytes_of(300_000);
    let digest = sha(&data);
    let (s, _, body) = env
        .req(
            Some(admin()),
            Method::PUT,
            "/api/v1/storage/buckets/photos/objects/2026/cat.bin",
            &[
                ("content-type", "application/x-test"),
                ("x-forgebase-checksum-sha256", &digest),
                ("x-forgebase-meta-owner", "alice"),
            ],
            Body::from(data.clone()),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{}", String::from_utf8_lossy(&body));
    let info: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(info["size"], 300_000);
    assert_eq!(info["checksum_sha256"], digest.as_str());
    assert_eq!(info["metadata"]["owner"], "alice");

    let (s, h, got) = env
        .req(
            Some(admin()),
            Method::GET,
            "/api/v1/storage/buckets/photos/objects/2026/cat.bin",
            &[],
            Body::empty(),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(got, data);
    assert_eq!(h["content-type"], "application/x-test");
    assert_eq!(h["content-length"], "300000");
    assert_eq!(h["accept-ranges"], "bytes");
    assert_eq!(h["etag"], format!("\"{digest}\"").as_str());
    assert_eq!(h["x-forgebase-meta-owner"], "alice");
    assert_eq!(h["content-security-policy"], "sandbox");

    let (s, h, got) = env
        .req(
            Some(admin()),
            Method::HEAD,
            "/api/v1/storage/buckets/photos/objects/2026/cat.bin",
            &[],
            Body::empty(),
        )
        .await;
    assert_eq!(
        (s, got.len(), h["content-length"].to_str().unwrap()),
        (StatusCode::OK, 0, "300000")
    );

    let (s, list) = env
        .json(
            Some(admin()),
            Method::GET,
            "/api/v1/storage/buckets/photos/objects?prefix=2026/&delimiter=/",
            json!(null),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(list["objects"][0]["key"], "2026/cat.bin");

    // The bytes are on disk once, under the content hash.
    let blobs = env.driver.list("blobs/").await.unwrap();
    assert_eq!(blobs.len(), 1);
    assert!(blobs[0].key.ends_with(&digest));

    // Anonymous callers are refused (401), the object does not leak.
    let (s, _, _) = env
        .req(
            None,
            Method::GET,
            "/api/v1/storage/buckets/photos/objects/2026/cat.bin",
            &[],
            Body::empty(),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    let (s, del) = env
        .json(
            Some(admin()),
            Method::DELETE,
            "/api/v1/storage/buckets/photos/objects/2026/cat.bin",
            json!(null),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{del}");
    let (s, _, _) = env
        .req(
            Some(admin()),
            Method::GET,
            "/api/v1/storage/buckets/photos/objects/2026/cat.bin",
            &[],
            Body::empty(),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

// ---- scenario #11: signed download URL -----------------------------------

#[tokio::test]
async fn scenario_11_signed_download_url() {
    let env = Env::new().await;
    env.bucket("docs", false).await;
    let data = b"top secret report".to_vec();
    env.storage
        .put_object(&admin(), put("docs", "reports/q3 2026.txt", &data))
        .await
        .unwrap();

    let (s, signed) = env
        .json(
            Some(admin()),
            Method::POST,
            "/api/v1/storage/buckets/docs/sign",
            json!({"key": "reports/q3 2026.txt", "expires_in": 60, "download_as": "q3.txt"}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{signed}");
    let url = signed["url"].as_str().unwrap().to_owned();
    assert!(
        url.starts_with("/api/v1/storage/signed/docs/reports/q3%202026.txt?expires="),
        "{url}"
    );

    // No credentials at all: the signature is the authorisation.
    let (s, h, body) = env.req(None, Method::GET, &url, &[], Body::empty()).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body, data);
    assert_eq!(h["content-disposition"], "attachment; filename=\"q3.txt\"");

    // Without the signature the same path is refused.
    let (s, _, _) = env
        .req(
            None,
            Method::GET,
            "/api/v1/storage/buckets/docs/objects/reports/q3%202026.txt",
            &[],
            Body::empty(),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

// ---- ranges ---------------------------------------------------------------

#[tokio::test]
async fn range_reads_serve_partial_content() {
    let env = Env::new().await;
    env.bucket("r", false).await;
    let data = bytes_of(100_000);
    env.storage.put_object(&admin(), put("r", "blob", &data)).await.unwrap();
    let uri = "/api/v1/storage/buckets/r/objects/blob";
    let get = |range: &'static str| {
        let env = &env;
        async move {
            env.req(Some(admin()), Method::GET, uri, &[("range", range)], Body::empty())
                .await
        }
    };
    let (s, h, b) = get("bytes=10-19").await;
    assert_eq!(s, StatusCode::PARTIAL_CONTENT);
    assert_eq!(b, data[10..20]);
    assert_eq!(h["content-range"], "bytes 10-19/100000");
    assert_eq!(h["content-length"], "10");

    let (s, h, b) = get("bytes=99990-").await;
    assert_eq!((s, b.as_slice()), (StatusCode::PARTIAL_CONTENT, &data[99_990..]));
    assert_eq!(h["content-range"], "bytes 99990-99999/100000");

    let (s, _, b) = get("bytes=-5").await;
    assert_eq!((s, b.as_slice()), (StatusCode::PARTIAL_CONTENT, &data[99_995..]));

    // An end past the object is clamped.
    let (s, _, b) = get("bytes=99998-500000").await;
    assert_eq!((s, b.len()), (StatusCode::PARTIAL_CONTENT, 2));

    let (s, _, _) = get("bytes=100000-").await;
    assert_eq!(s, StatusCode::RANGE_NOT_SATISFIABLE);
    let (s, _, _) = get("bytes=0-1,5-6").await;
    assert_eq!(s, StatusCode::RANGE_NOT_SATISFIABLE);

    // A range across driver read chunks, through the service.
    let r = env
        .storage
        .get_object(&admin(), "r", "blob", None, Some(RangeSpec::Bounded(65_000, 70_000)))
        .await
        .unwrap();
    assert_eq!(r.range, Some((65_000, 70_000)));
    assert_eq!(read_all(r).await, data[65_000..=70_000]);
}

// ---- resumable upload after an interruption -------------------------------

#[tokio::test]
async fn resumable_upload_continues_after_interruption() {
    let env = Env::new().await;
    env.bucket("big", false).await;
    let data = bytes_of(50_000);
    let up = env
        .storage
        .create_upload(&admin(), "big", up_input("movie.bin", "resumable", Some(50_000)))
        .await
        .unwrap();

    let a = env
        .storage
        .append_chunk(&admin(), &up.id, 0, None, chunked(data[..20_000].to_vec(), 4096))
        .await
        .unwrap();
    assert_eq!(a.upload.received_bytes, 20_000);
    assert!(a.object.is_none());

    // The connection drops in the middle of the next chunk.
    let err = env
        .storage
        .append_chunk(
            &admin(),
            &up.id,
            20_000,
            None,
            interrupted(data[20_000..30_000].to_vec()),
        )
        .await;
    assert!(
        matches!(
            err,
            Err(StorageError::Invalid {
                code: "upload_interrupted",
                ..
            })
        ),
        "{err:?}"
    );
    // Nothing of the broken chunk counts, and nothing is left staged for it.
    assert_eq!(
        env.storage.get_upload(&admin(), &up.id).await.unwrap().received_bytes,
        20_000
    );
    assert_eq!(env.driver.list(&format!("staging/{}/", up.id)).await.unwrap().len(), 1);

    // A client that guessed wrong learns where the server is.
    let err = env
        .storage
        .append_chunk(&admin(), &up.id, 30_000, None, chunked(vec![1; 10], 10))
        .await;
    assert!(
        matches!(err, Err(StorageError::OffsetMismatch { server_offset: 20_000 })),
        "{err:?}"
    );

    // Resume at the server's offset: the last chunk completes the object.
    let done = env
        .storage
        .append_chunk(&admin(), &up.id, 20_000, None, chunked(data[20_000..].to_vec(), 7000))
        .await
        .unwrap();
    let object = done.object.expect("declared size reached");
    assert_eq!(object.size, 50_000);
    assert_eq!(object.checksum_sha256.as_deref(), Some(sha(&data).as_str()));
    let r = env
        .storage
        .get_object(&admin(), "big", "movie.bin", None, None)
        .await
        .unwrap();
    assert_eq!(read_all(r).await, data);
    // Session and staged chunks are gone.
    assert!(env.storage.get_upload(&admin(), &up.id).await.is_err());
    assert!(env.driver.list("staging/").await.unwrap().is_empty());
}

#[tokio::test]
async fn resumable_upload_over_http() {
    let env = Env::new().await;
    env.bucket("h", false).await;
    let data = bytes_of(9_000);
    let (s, up) = env
        .json(
            Some(admin()),
            Method::POST,
            "/api/v1/storage/buckets/h/uploads",
            json!({"key": "f.bin", "kind": "resumable", "size": 9000}),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{up}");
    let id = up["id"].as_str().unwrap();
    let uri = format!("/api/v1/storage/uploads/{id}");
    let (s, h, _) = env
        .req(
            Some(admin()),
            Method::PATCH,
            &uri,
            &[("upload-offset", "0")],
            Body::from(data[..4000].to_vec()),
        )
        .await;
    assert_eq!((s, h["upload-offset"].to_str().unwrap()), (StatusCode::OK, "4000"));
    let (s, _, b) = env
        .req(
            Some(admin()),
            Method::PATCH,
            &uri,
            &[("upload-offset", "0")],
            Body::from(vec![0u8; 5]),
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let err: serde_json::Value = serde_json::from_slice(&b).unwrap();
    assert_eq!(err["error"]["code"], "offset_mismatch");
    assert_eq!(err["error"]["details"]["upload_offset"], 4000);
    let (s, _, _) = env
        .req(
            Some(admin()),
            Method::PATCH,
            &uri,
            &[("upload-offset", "4000")],
            Body::from(data[4000..].to_vec()),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    let (_, _, got) = env
        .req(
            Some(admin()),
            Method::GET,
            "/api/v1/storage/buckets/h/objects/f.bin",
            &[],
            Body::empty(),
        )
        .await;
    assert_eq!(got, data);
}

// ---- multipart ---------------------------------------------------------------

#[tokio::test]
async fn multipart_parts_in_any_order_with_part_checksums() {
    let env = Env::new().await;
    env.bucket("m", false).await;
    let data = bytes_of(30_000);
    let up = env
        .storage
        .create_upload(&admin(), "m", up_input("big.bin", "multipart", None))
        .await
        .unwrap();
    let p = |n: usize| data[(n - 1) * 10_000..n * 10_000].to_vec();
    // Out of order, and part 2 first sent wrong, then replaced.
    env.storage
        .upload_part(&admin(), &up.id, 3, None, chunked(p(3), 3000))
        .await
        .unwrap();
    let bad = env
        .storage
        .upload_part(&admin(), &up.id, 2, Some(&sha(b"nope")), chunked(p(2), 3000))
        .await;
    assert!(matches!(bad, Err(StorageError::ChecksumMismatch { .. })));
    env.storage
        .upload_part(&admin(), &up.id, 2, Some(&sha(&p(2))), chunked(p(2), 3000))
        .await
        .unwrap();
    env.storage
        .upload_part(&admin(), &up.id, 1, None, chunked(vec![9; 10_000], 3000))
        .await
        .unwrap();
    env.storage
        .upload_part(&admin(), &up.id, 1, None, chunked(p(1), 3000))
        .await
        .unwrap(); // replaces
    let state = env.storage.get_upload(&admin(), &up.id).await.unwrap();
    assert_eq!(state.parts.len(), 3);
    assert_eq!(state.received_bytes, 30_000);

    // A wrong whole-object checksum fails the completion and keeps the session.
    let bad = env
        .storage
        .complete_upload(
            &admin(),
            &up.id,
            CompleteInput {
                parts: None,
                checksum_sha256: Some(sha(b"x")),
            },
        )
        .await;
    assert!(matches!(bad, Err(StorageError::ChecksumMismatch { .. })));
    let info = env
        .storage
        .complete_upload(
            &admin(),
            &up.id,
            CompleteInput {
                parts: Some(
                    vec![1, 2, 3]
                        .into_iter()
                        .map(|n| CompletePart {
                            part_number: n,
                            checksum_sha256: Some(sha(&p(n as usize))),
                        })
                        .collect(),
                ),
                checksum_sha256: Some(sha(&data)),
            },
        )
        .await
        .unwrap();
    assert_eq!(info.size, 30_000);
    let r = env
        .storage
        .get_object(&admin(), "m", "big.bin", None, None)
        .await
        .unwrap();
    assert_eq!(read_all(r).await, data);
    assert!(env.driver.list("staging/").await.unwrap().is_empty());
}

// ---- checksums -------------------------------------------------------------

#[tokio::test]
async fn checksum_mismatch_is_rejected_and_leaves_nothing() {
    let env = Env::new().await;
    env.bucket("c", false).await;
    let data = bytes_of(5000);
    let wrong = sha(b"something else");
    let (s, _, b) = env
        .req(
            Some(admin()),
            Method::PUT,
            "/api/v1/storage/buckets/c/objects/a.bin",
            &[("x-forgebase-checksum-sha256", &wrong)],
            Body::from(data.clone()),
        )
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let err: serde_json::Value = serde_json::from_slice(&b).unwrap();
    assert_eq!(err["error"]["code"], "checksum_mismatch");
    assert_eq!(err["error"]["details"]["actual"], sha(&data).as_str());
    assert!(
        env.driver.list("").await.unwrap().is_empty(),
        "no staged file or blob remains"
    );
    assert!(env.storage.head_object(&admin(), "c", "a.bin", None).await.is_err());
    // A malformed checksum header is a 400, not a silent skip.
    let (s, _, _) = env
        .req(
            Some(admin()),
            Method::PUT,
            "/api/v1/storage/buckets/c/objects/a.bin",
            &[("x-forgebase-checksum-sha256", "xyz")],
            Body::from(data),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

// ---- deduplication ------------------------------------------------------------

#[tokio::test]
async fn identical_content_is_stored_once_until_unreferenced() {
    let env = Env::new().await;
    env.bucket("d", false).await;
    env.bucket("e", false).await;
    let data = bytes_of(70_000);
    env.storage.put_object(&admin(), put("d", "one", &data)).await.unwrap();
    env.storage.put_object(&admin(), put("d", "two", &data)).await.unwrap();
    env.storage
        .put_object(&admin(), put("e", "three", &data))
        .await
        .unwrap();
    assert_eq!(env.driver.list("blobs/").await.unwrap().len(), 1);
    assert!(env.driver.list("staging/").await.unwrap().is_empty());
    // Copy and move touch metadata only.
    env.storage
        .copy_object(&admin(), "d", "one", None, "e", "copy")
        .await
        .unwrap();
    env.storage
        .move_object(&admin(), "d", "two", "d", "renamed")
        .await
        .unwrap();
    assert_eq!(env.driver.list("blobs/").await.unwrap().len(), 1);
    assert!(env.storage.head_object(&admin(), "d", "two", None).await.is_err());
    assert_eq!(
        env.storage
            .head_object(&admin(), "d", "renamed", None)
            .await
            .unwrap()
            .size,
        70_000
    );

    let refs = |sql: &str| {
        let db = env.db.clone();
        let sql = sql.to_owned();
        async move {
            use forgebase_db::Backend;
            db.fetch_all(&forgebase_db::Statement::new(&sql, vec![]))
                .await
                .unwrap()
                .scalar()
                .and_then(forgebase_db::Value::as_i64)
                .unwrap()
        }
    };
    assert_eq!(refs("SELECT refcount FROM _forgebase_storage_blobs").await, 4);
    for (b, k) in [("d", "one"), ("d", "renamed"), ("e", "three")] {
        env.storage.delete_object(&admin(), b, k, None).await.unwrap();
    }
    // Still referenced by the copy: the reconciler keeps it.
    let r = env.storage.reconcile(&admin()).await.unwrap();
    assert_eq!(r.unreferenced_blobs_deleted, 0);
    assert_eq!(env.driver.list("blobs/").await.unwrap().len(), 1);
    env.storage.delete_object(&admin(), "e", "copy", None).await.unwrap();
    let r = env.storage.reconcile(&admin()).await.unwrap();
    assert_eq!(r.unreferenced_blobs_deleted, 1);
    assert!(env.driver.list("blobs/").await.unwrap().is_empty());
    assert_eq!(refs("SELECT COUNT(*) FROM _forgebase_storage_blobs").await, 0);
}

// ---- versions --------------------------------------------------------------------

#[tokio::test]
async fn versions_can_be_listed_deleted_and_restored() {
    let env = Env::new().await;
    env.bucket("v", true).await;
    let v1 = env
        .storage
        .put_object(&admin(), put("v", "doc", b"first"))
        .await
        .unwrap();
    let v2 = env
        .storage
        .put_object(&admin(), put("v", "doc", b"second"))
        .await
        .unwrap();
    assert_ne!(v1.version_id, v2.version_id);
    let cur = env.storage.get_object(&admin(), "v", "doc", None, None).await.unwrap();
    assert_eq!(read_all(cur).await, b"second");
    let old = env
        .storage
        .get_object(&admin(), "v", "doc", Some(&v1.version_id), None)
        .await
        .unwrap();
    assert_eq!(read_all(old).await, b"first");

    // A delete adds a marker; the content stays recoverable.
    let del = env.storage.delete_object(&admin(), "v", "doc", None).await.unwrap();
    assert!(del.delete_marker);
    assert!(matches!(
        env.storage.get_object(&admin(), "v", "doc", None, None).await,
        Err(StorageError::NotFound(_))
    ));
    assert!(
        env.storage
            .list_objects(&admin(), "v", "", None, 10, None)
            .await
            .unwrap()
            .objects
            .is_empty()
    );
    let versions = env.storage.list_versions(&admin(), "v", "doc").await.unwrap();
    assert_eq!(versions.len(), 3);
    assert!(versions[0].delete_marker && versions[0].is_latest);

    // Restore the first version: it becomes a new current version.
    let restored = env
        .storage
        .restore_version(&admin(), "v", "doc", &v1.version_id)
        .await
        .unwrap();
    assert!(restored.is_latest);
    assert_ne!(restored.version_id, v1.version_id);
    let cur = env.storage.get_object(&admin(), "v", "doc", None, None).await.unwrap();
    assert_eq!(read_all(cur).await, b"first");
    assert_eq!(env.storage.list_versions(&admin(), "v", "doc").await.unwrap().len(), 4);

    // Removing the current version for good promotes the newest remaining one.
    env.storage
        .delete_object(&admin(), "v", "doc", Some(&restored.version_id))
        .await
        .unwrap();
    let list = env.storage.list_versions(&admin(), "v", "doc").await.unwrap();
    assert_eq!(list.len(), 3);
    assert!(list[0].is_latest && list[0].delete_marker);

    // Same flow over HTTP.
    let (s, b) = env
        .json(
            Some(admin()),
            Method::POST,
            "/api/v1/storage/buckets/v/restore",
            json!({"key": "doc", "version_id": v2.version_id}),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    let (_, _, got) = env
        .req(
            Some(admin()),
            Method::GET,
            "/api/v1/storage/buckets/v/objects/doc",
            &[],
            Body::empty(),
        )
        .await;
    assert_eq!(got, b"second");
    let uri = format!("/api/v1/storage/buckets/v/objects/doc?version={}", v1.version_id);
    let (_, _, got) = env.req(Some(admin()), Method::GET, &uri, &[], Body::empty()).await;
    assert_eq!(got, b"first");
}

#[tokio::test]
async fn unversioned_overwrite_replaces_and_frees_the_old_blob() {
    let env = Env::new().await;
    env.bucket("u", false).await;
    env.storage
        .put_object(&admin(), put("u", "k", b"old content"))
        .await
        .unwrap();
    env.storage
        .put_object(&admin(), put("u", "k", b"new content"))
        .await
        .unwrap();
    assert_eq!(env.storage.list_versions(&admin(), "u", "k").await.unwrap().len(), 1);
    let r = env.storage.reconcile(&admin()).await.unwrap();
    assert_eq!(r.unreferenced_blobs_deleted, 1);
    assert_eq!(env.driver.list("blobs/").await.unwrap().len(), 1);
}

// ---- signed URLs ----------------------------------------------------------------

#[tokio::test]
async fn expired_and_forged_signed_urls_are_refused() {
    let env = Env::new().await;
    env.bucket("s", false).await;
    env.bucket("other", false).await;
    env.storage
        .put_object(&admin(), put("s", "a.txt", b"alpha"))
        .await
        .unwrap();
    env.storage
        .put_object(&admin(), put("s", "b.txt", b"beta"))
        .await
        .unwrap();
    let signed = env
        .storage
        .sign_url(
            &admin(),
            "s",
            SignRequest {
                key: "a.txt".into(),
                method: None,
                expires_in: Some(60),
                version_id: None,
                download_as: None,
                content_type: None,
            },
        )
        .await
        .unwrap();
    let get = |url: String| {
        let env = &env;
        async move { env.req(None, Method::GET, &url, &[], Body::empty()).await }
    };
    let (s, _, b) = get(signed.url.clone()).await;
    assert_eq!((s, b.as_slice()), (StatusCode::OK, &b"alpha"[..]));

    let code = |b: &[u8]| {
        serde_json::from_slice::<serde_json::Value>(b).unwrap()["error"]["code"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    // Another key under the same signature.
    let (s, _, b) = get(signed.url.replace("a.txt", "b.txt")).await;
    assert_eq!((s, code(&b).as_str()), (StatusCode::FORBIDDEN, "signature_invalid"));
    // Another bucket.
    let (s, _, b) = get(signed.url.replace("/signed/s/", "/signed/other/")).await;
    assert_eq!((s, code(&b).as_str()), (StatusCode::FORBIDDEN, "signature_invalid"));
    // A later expiry.
    let later = signed.url.replacen("expires=", "expires=9", 1);
    let (s, _, b) = get(later).await;
    assert_eq!((s, code(&b).as_str()), (StatusCode::FORBIDDEN, "signature_invalid"));
    // A tampered, a missing and a malformed signature.
    let (_, sig) = signed.url.rsplit_once("sig=").unwrap();
    let flipped = format!(
        "{}{}",
        &signed.url[..signed.url.len() - 1],
        if sig.ends_with('0') { '1' } else { '0' }
    );
    let (s, _, _) = get(flipped).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _, _) = get(signed.url.split("&sig=").next().unwrap().to_owned()).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _, _) = get(format!("{}zz", signed.url)).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // A download signature is not an upload signature.
    let (s, _, _) = env.req(None, Method::PUT, &signed.url, &[], Body::from("evil")).await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // Past the expiry the genuine URL stops working.
    env.clock.advance(61_000);
    let (s, _, b) = get(signed.url.clone()).await;
    assert_eq!((s, code(&b).as_str()), (StatusCode::FORBIDDEN, "url_expired"));

    // Limits on the lifetime, and signing needs access to the object.
    let too_long = env
        .storage
        .sign_url(
            &admin(),
            "s",
            SignRequest {
                key: "a.txt".into(),
                method: None,
                expires_in: Some(10_000_000),
                version_id: None,
                download_as: None,
                content_type: None,
            },
        )
        .await;
    assert!(matches!(
        too_long,
        Err(StorageError::Invalid {
            code: "invalid_expiry",
            ..
        })
    ));
    let denied = env
        .storage
        .sign_url(
            &user("mallory"),
            "s",
            SignRequest {
                key: "a.txt".into(),
                method: None,
                expires_in: None,
                version_id: None,
                download_as: None,
                content_type: None,
            },
        )
        .await;
    assert!(matches!(denied, Err(StorageError::Forbidden(_))), "{denied:?}");
}

#[tokio::test]
async fn signed_upload_url_accepts_one_key_and_bills_the_signer() {
    let env = Env::new().await;
    env.bucket("up", false).await;
    let signed = env
        .storage
        .sign_url(
            &admin(),
            "up",
            SignRequest {
                key: "in/new.txt".into(),
                method: Some(SignedMethod::Put),
                expires_in: Some(120),
                version_id: None,
                download_as: None,
                content_type: Some("text/plain".into()),
            },
        )
        .await
        .unwrap();
    let (s, _, _) = env
        .req(
            None,
            Method::PUT,
            &signed.url,
            &[("content-type", "image/png")],
            Body::from("x"),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "content type is part of the signature");
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
    let r = env
        .storage
        .get_object(&admin(), "up", "in/new.txt", None, None)
        .await
        .unwrap();
    assert_eq!(read_all(r).await, b"hello");
}

// ---- quota --------------------------------------------------------------------------

#[tokio::test]
async fn storage_quota_exceeded_is_429_and_deletes_give_it_back() {
    let env = Env::new().await;
    env.bucket("q", false).await;
    env.storage
        .set_acl(
            &admin(),
            "q",
            Acl {
                entries: vec![AclEntry {
                    prefix: String::new(),
                    grantee: "user:people/alice".to_owned(),
                    permission: Permission::Write,
                }],
            },
        )
        .await
        .unwrap();
    env.quota(&pk("alice"), 100).await;
    let alice = user("alice");
    let first = env.storage.put_object(&alice, put("q", "a", &[1; 60])).await.unwrap();

    let (s, _, b) = env
        .req(
            Some(alice.clone()),
            Method::PUT,
            "/api/v1/storage/buckets/q/objects/b",
            &[],
            Body::from(vec![2u8; 60]),
        )
        .await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "{}", String::from_utf8_lossy(&b));
    let err: serde_json::Value = serde_json::from_slice(&b).unwrap();
    assert_eq!(err["error"]["code"], "quota_exceeded");
    // The refused upload left nothing behind.
    assert!(env.storage.head_object(&admin(), "q", "b", None).await.is_err());
    assert_eq!(env.driver.list("blobs/").await.unwrap().len(), 1);
    assert!(env.driver.list("staging/").await.unwrap().is_empty());

    // 40 more bytes still fit; a copy counts too.
    env.storage.put_object(&alice, put("q", "c", &[3; 40])).await.unwrap();
    let over = env.storage.copy_object(&alice, "q", "a", None, "q", "a2").await;
    assert!(matches!(over, Err(StorageError::Entitlement(_))), "{over:?}");

    // Deleting an object frees its bytes.
    env.storage.delete_object(&alice, "q", "a", None).await.unwrap();
    env.storage.put_object(&alice, put("q", "b", &[2; 60])).await.unwrap();
    // An overwrite is charged before the replaced bytes are given back, so
    // at the limit it needs headroom (documented in ADR-0018)...
    let at_limit = env.storage.put_object(&alice, put("q", "b", &[4; 10])).await;
    assert!(matches!(at_limit, Err(StorageError::Entitlement(_))), "{at_limit:?}");
    // ...and once it went through, the replaced bytes are released.
    env.storage.delete_object(&alice, "q", "c", None).await.unwrap(); // 60 left
    env.storage.put_object(&alice, put("q", "b", &[4; 10])).await.unwrap(); // 10 now
    env.storage.put_object(&alice, put("q", "d", &[5; 90])).await.unwrap();
    let _ = first;

    // Another subject has its own counter (and no plan: no limit applies).
    env.storage
        .put_object(&admin(), put("q", "big-admin", &[6; 500]))
        .await
        .unwrap();
}

// ---- access control ---------------------------------------------------------------------

#[tokio::test]
async fn bucket_visibility_acl_and_limits_are_enforced() {
    let env = Env::new().await;
    env.bucket("priv", false).await;
    env.storage
        .put_object(&admin(), put("priv", "docs/a.txt", b"a"))
        .await
        .unwrap();
    env.storage
        .put_object(&admin(), put("priv", "other/b.txt", b"b"))
        .await
        .unwrap();
    let alice = user("alice");
    let anon = AccessContext::anonymous();

    // Private: anonymous is unauthenticated, a signed-in stranger is forbidden.
    assert!(matches!(
        env.storage.get_object(&anon, "priv", "docs/a.txt", None, None).await,
        Err(StorageError::Unauthenticated)
    ));
    assert!(matches!(
        env.storage.get_object(&alice, "priv", "docs/a.txt", None, None).await,
        Err(StorageError::Forbidden(_))
    ));
    assert!(env.storage.list_buckets(&alice).await.unwrap().is_empty());

    // An ACL grant on one prefix.
    env.storage
        .set_acl(
            &admin(),
            "priv",
            Acl {
                entries: vec![
                    AclEntry {
                        prefix: "docs/".into(),
                        grantee: pk("alice"),
                        permission: Permission::Read,
                    },
                    AclEntry {
                        prefix: "inbox/".into(),
                        grantee: "role:uploader".into(),
                        permission: Permission::Write,
                    },
                ],
            },
        )
        .await
        .unwrap();
    assert!(
        env.storage
            .get_object(&alice, "priv", "docs/a.txt", None, None)
            .await
            .is_ok()
    );
    assert!(matches!(
        env.storage.get_object(&alice, "priv", "other/b.txt", None, None).await,
        Err(StorageError::Forbidden(_))
    ));
    assert!(matches!(
        env.storage.put_object(&alice, put("priv", "docs/x", b"x")).await,
        Err(StorageError::Forbidden(_))
    ));
    assert!(
        env.storage
            .list_objects(&alice, "priv", "docs/", None, 10, None)
            .await
            .is_ok()
    );
    assert!(
        env.storage
            .list_objects(&alice, "priv", "", None, 10, None)
            .await
            .is_err()
    );
    assert_eq!(env.storage.list_buckets(&alice).await.unwrap().len(), 1);
    // Role-based grant.
    let mut uploader = user("bob");
    uploader.roles = vec!["uploader".into()];
    assert!(
        env.storage
            .put_object(&uploader, put("priv", "inbox/f", b"f"))
            .await
            .is_ok()
    );
    assert!(
        env.storage
            .put_object(&uploader, put("priv", "docs/f", b"f"))
            .await
            .is_err()
    );
    // Only admins of the bucket change its ACL.
    assert!(env.storage.set_acl(&alice, "priv", Acl::default()).await.is_err());

    // Authenticated and public visibility.
    env.storage
        .update_bucket(
            &admin(),
            "priv",
            BucketPatch {
                visibility: Some(Visibility::Authenticated),
                ..BucketPatch::default()
            },
        )
        .await
        .unwrap();
    assert!(
        env.storage
            .get_object(&user("carol"), "priv", "other/b.txt", None, None)
            .await
            .is_ok()
    );
    assert!(matches!(
        env.storage.get_object(&anon, "priv", "other/b.txt", None, None).await,
        Err(StorageError::Unauthenticated)
    ));
    env.storage
        .update_bucket(
            &admin(),
            "priv",
            BucketPatch {
                visibility: Some(Visibility::Public),
                ..BucketPatch::default()
            },
        )
        .await
        .unwrap();
    let r = env
        .storage
        .get_object(&anon, "priv", "other/b.txt", None, None)
        .await
        .unwrap();
    assert_eq!(read_all(r).await, b"b");
    // Public read never means public write or delete.
    assert!(env.storage.put_object(&anon, put("priv", "evil", b"x")).await.is_err());
    assert!(
        env.storage
            .delete_object(&user("carol"), "priv", "other/b.txt", None)
            .await
            .is_err()
    );

    // Without any grant a signed-in user cannot create buckets.
    let denied = env
        .storage
        .create_bucket(
            &alice,
            BucketInput {
                name: "mine".into(),
                ..BucketInput::default()
            },
        )
        .await;
    assert!(matches!(denied, Err(StorageError::Forbidden(_))), "{denied:?}");
}

#[tokio::test]
async fn bucket_limits_and_content_types_are_checked() {
    let env = Env::new().await;
    env.storage
        .create_bucket(
            &admin(),
            BucketInput {
                name: "img".into(),
                max_object_bytes: Some(1000),
                allowed_content_types: Some(vec!["image/*".into(), "application/pdf".into()]),
                ..BucketInput::default()
            },
        )
        .await
        .unwrap();
    let png = [b"\x89PNG\r\n\x1a\n".as_slice(), &[0u8; 100]].concat();
    let ok = env
        .storage
        .put_object(&admin(), put("img", "a.png", &png))
        .await
        .unwrap();
    assert_eq!(ok.content_type, "image/png", "detected from the bytes");
    let bad = env.storage.put_object(&admin(), put("img", "a.txt", b"hello")).await;
    assert!(matches!(
        bad,
        Err(StorageError::Invalid {
            code: "content_type_not_allowed",
            ..
        })
    ));
    let (s, _, _) = env
        .req(
            Some(admin()),
            Method::PUT,
            "/api/v1/storage/buckets/img/objects/big.png",
            &[("content-type", "image/png")],
            Body::from(vec![0u8; 2000]),
        )
        .await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
    // Streaming past the limit without a Content-Length is caught too.
    let mut req = put("img", "big2.png", b"");
    req.content_type = Some("image/png".into());
    req.body = chunked(vec![0u8; 2000], 300);
    assert!(matches!(
        env.storage.put_object(&admin(), req).await,
        Err(StorageError::TooLarge { limit: 1000 })
    ));
    assert!(env.driver.list("staging/").await.unwrap().is_empty());
}

// ---- reconciler ------------------------------------------------------------------------------

#[tokio::test]
async fn reconciler_cleans_orphans_staging_and_incomplete_uploads() {
    let env = Env::new().await;
    env.bucket("rec", false).await;
    let keep = bytes_of(2000);
    env.storage
        .put_object(&admin(), put("rec", "keep", &keep))
        .await
        .unwrap();

    // An open session with a part, expired by the clock.
    let up = env
        .storage
        .create_upload(&admin(), "rec", up_input("half", "multipart", None))
        .await
        .unwrap();
    env.storage
        .upload_part(&admin(), &up.id, 1, None, chunked(vec![7; 3000], 1000))
        .await
        .unwrap();
    // A live session that must survive.
    let live = env
        .storage
        .create_upload(&admin(), "rec", up_input("live", "multipart", None))
        .await
        .unwrap();
    env.storage
        .upload_part(&admin(), &live.id, 1, None, chunked(vec![8; 100], 100))
        .await
        .unwrap();

    // A crash between placing a blob and committing metadata; a crash while staging.
    let orphan = format!("blobs/ab/{}", "ab".repeat(32));
    env.driver
        .put(&orphan, forgebase_storage::body_of(vec![1; 10]))
        .await
        .unwrap();
    env.driver
        .put("staging/single/abandoned", forgebase_storage::body_of(vec![2; 10]))
        .await
        .unwrap();
    // Metadata that names a blob the store lost.
    let lost = env
        .storage
        .put_object(&admin(), put("rec", "lost", b"will vanish"))
        .await
        .unwrap();
    env.driver
        .delete(&format!(
            "blobs/{}/{}",
            &lost.checksum_sha256.clone().unwrap()[..2],
            lost.checksum_sha256.clone().unwrap()
        ))
        .await
        .unwrap();

    // Nothing is stale yet when the grace period is long.
    let strict = Env::with(|s| s.reconcile_grace_secs = 3600, None).await;
    strict
        .driver
        .put("staging/single/fresh", forgebase_storage::body_of(vec![1]))
        .await
        .unwrap();
    let r = strict.storage.reconcile(&admin()).await.unwrap();
    assert_eq!(
        (r.staging_deleted, r.orphan_blobs_deleted),
        (0, 0),
        "inside the grace period nothing is touched"
    );

    env.clock.advance(25 * 3600 * 1000 - 3000); // `half` expired (24 h), `live` too: re-open below
    let live_state = env.storage.get_upload(&admin(), &live.id).await;
    assert!(live_state.is_err(), "past the session lifetime a session is refused");
    let report = env.storage.reconcile(&admin()).await.unwrap();
    assert_eq!(report.uploads_aborted, 2);
    assert_eq!(report.orphan_blobs_deleted, 1);
    assert!(report.staging_deleted >= 1);
    assert_eq!(report.missing_blobs, vec![lost.checksum_sha256.unwrap()]);
    assert!(env.driver.list("staging/").await.unwrap().is_empty());
    assert!(env.driver.head(&orphan).await.unwrap().is_none());
    // Referenced data is untouched.
    let r = env
        .storage
        .get_object(&admin(), "rec", "keep", None, None)
        .await
        .unwrap();
    assert_eq!(read_all(r).await, keep);
    // A second run finds nothing new.
    let again = env.storage.reconcile(&admin()).await.unwrap();
    assert_eq!(
        (
            again.uploads_aborted,
            again.staging_deleted,
            again.orphan_blobs_deleted,
            again.unreferenced_blobs_deleted
        ),
        (0, 0, 0, 0)
    );
    // Only storage admins may run it.
    assert!(env.storage.reconcile(&user("alice")).await.is_err());
    let (s, _) = env
        .json(
            Some(admin()),
            Method::POST,
            "/api/v1/storage/maintenance/reconcile",
            json!(null),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn aborting_an_upload_discards_its_parts() {
    let env = Env::new().await;
    env.bucket("ab", false).await;
    let up = env
        .storage
        .create_upload(&admin(), "ab", up_input("x", "multipart", None))
        .await
        .unwrap();
    env.storage
        .upload_part(&admin(), &up.id, 1, None, chunked(vec![1; 500], 100))
        .await
        .unwrap();
    assert_eq!(env.driver.list("staging/").await.unwrap().len(), 1);
    env.storage.abort_upload(&admin(), &up.id).await.unwrap();
    assert!(env.driver.list("staging/").await.unwrap().is_empty());
    assert!(env.storage.get_upload(&admin(), &up.id).await.is_err());
    // Another user cannot touch someone else's session.
    env.storage
        .set_acl(
            &admin(),
            "ab",
            Acl {
                entries: vec![AclEntry {
                    prefix: String::new(),
                    grantee: "authenticated".into(),
                    permission: Permission::Write,
                }],
            },
        )
        .await
        .unwrap();
    let mine = env
        .storage
        .create_upload(&user("alice"), "ab", up_input("y", "multipart", None))
        .await
        .unwrap();
    assert!(matches!(
        env.storage.get_upload(&user("bob"), &mine.id).await,
        Err(StorageError::NotFound(_))
    ));
}

#[tokio::test]
async fn deleting_a_bucket_needs_force_and_releases_everything() {
    let env = Env::new().await;
    env.bucket("gone", true).await;
    env.storage
        .put_object(&admin(), put("gone", "a", b"aaaa"))
        .await
        .unwrap();
    env.storage
        .put_object(&admin(), put("gone", "a", b"bbbb"))
        .await
        .unwrap();
    let up = env
        .storage
        .create_upload(&admin(), "gone", up_input("p", "multipart", None))
        .await
        .unwrap();
    env.storage
        .upload_part(&admin(), &up.id, 1, None, chunked(vec![1; 10], 10))
        .await
        .unwrap();
    let refused = env.storage.delete_bucket(&admin(), "gone", false).await;
    assert!(matches!(
        refused,
        Err(StorageError::Conflict {
            code: "bucket_not_empty",
            ..
        })
    ));
    env.storage.delete_bucket(&admin(), "gone", true).await.unwrap();
    assert!(env.storage.get_bucket(&admin(), "gone").await.is_err());
    assert!(env.driver.list("staging/").await.unwrap().is_empty());
    let r = env.storage.reconcile(&admin()).await.unwrap();
    assert_eq!(r.unreferenced_blobs_deleted, 2);
}

#[tokio::test]
async fn listing_pages_and_groups_by_delimiter() {
    let env = Env::new().await;
    env.bucket("l", false).await;
    for k in [
        "a.txt",
        "docs/1.txt",
        "docs/2.txt",
        "docs/sub/3.txt",
        "img/x.png",
        "z.txt",
    ] {
        env.storage
            .put_object(&admin(), put("l", k, k.as_bytes()))
            .await
            .unwrap();
    }
    let page = env
        .storage
        .list_objects(&admin(), "l", "", Some("/"), 100, None)
        .await
        .unwrap();
    let keys: Vec<_> = page.objects.iter().map(|o| o.key.as_str()).collect();
    assert_eq!(keys, ["a.txt", "z.txt"]);
    assert_eq!(page.prefixes, ["docs/", "img/"]);
    assert!(page.next_cursor.is_none());

    // Walk the whole bucket two entries at a time.
    let mut seen = Vec::new();
    let mut cursor = None;
    loop {
        let p = env
            .storage
            .list_objects(&admin(), "l", "", None, 2, cursor.as_deref())
            .await
            .unwrap();
        seen.extend(p.objects.iter().map(|o| o.key.clone()));
        match p.next_cursor {
            Some(c) => cursor = Some(c),
            None => break,
        }
    }
    assert_eq!(
        seen,
        [
            "a.txt",
            "docs/1.txt",
            "docs/2.txt",
            "docs/sub/3.txt",
            "img/x.png",
            "z.txt"
        ]
    );
    // Same with groups: a.txt, docs/, img/, z.txt in pages of two.
    let p1 = env
        .storage
        .list_objects(&admin(), "l", "", Some("/"), 2, None)
        .await
        .unwrap();
    assert_eq!((p1.objects.len(), p1.prefixes.clone()), (1, vec!["docs/".to_owned()]));
    let p2 = env
        .storage
        .list_objects(&admin(), "l", "", Some("/"), 2, p1.next_cursor.as_deref())
        .await
        .unwrap();
    assert_eq!(
        (
            p2.objects.iter().map(|o| o.key.as_str()).collect::<Vec<_>>(),
            p2.prefixes
        ),
        (vec!["z.txt"], vec!["img/".to_owned()])
    );
    let sub = env
        .storage
        .list_objects(&admin(), "l", "docs/", Some("/"), 100, None)
        .await
        .unwrap();
    assert_eq!(sub.objects.len(), 2);
    assert_eq!(sub.prefixes, ["docs/sub/"]);
}

#[tokio::test]
async fn openapi_documents_the_storage_api() {
    let doc = serde_json::to_value(forgebase_storage::openapi()).unwrap();
    let paths = doc["paths"].as_object().unwrap();
    for p in [
        "/api/v1/storage/buckets",
        "/api/v1/storage/buckets/{bucket}/objects/{key}",
        "/api/v1/storage/buckets/{bucket}/sign",
        "/api/v1/storage/uploads/{id}",
        "/api/v1/storage/signed/{bucket}/{key}",
        "/api/v1/storage/maintenance/reconcile",
    ] {
        assert!(paths.contains_key(p), "missing {p}");
    }
}

// ---- FIX-4: bucket usage and object metadata ----------------------------

#[tokio::test]
async fn bucket_usage_and_object_metadata_endpoints() {
    let env = Env::new().await;
    let (s, _) = env
        .json(
            Some(admin()),
            Method::POST,
            "/api/v1/storage/buckets",
            json!({"name": "docs", "versioning": true}),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    let put = |key: &'static str, n: usize| {
        let env = &env;
        async move {
            let (s, _, b) = env
                .req(
                    Some(admin()),
                    Method::PUT,
                    &format!("/api/v1/storage/buckets/docs/objects/{key}"),
                    &[("content-type", "text/plain"), ("x-forgebase-meta-owner", "bob")],
                    Body::from(bytes_of(n)),
                )
                .await;
            assert_eq!(s, StatusCode::CREATED, "{}", String::from_utf8_lossy(&b));
        }
    };
    put("a.txt", 100).await;
    put("a.txt", 50).await; // second version of a.txt
    put("b.txt", 10).await;

    let (s, u) = env
        .json(
            Some(admin()),
            Method::GET,
            "/api/v1/storage/buckets/docs/usage",
            json!(null),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{u}");
    assert_eq!(
        u,
        json!({"bucket": "docs", "objects": 2, "versions": 3, "logical_bytes": 60, "version_bytes": 160})
    );
    // a delete marker leaves the version count up and the current objects down
    env.json(
        Some(admin()),
        Method::DELETE,
        "/api/v1/storage/buckets/docs/objects/b.txt",
        json!(null),
    )
    .await;
    let (_, u) = env
        .json(
            Some(admin()),
            Method::GET,
            "/api/v1/storage/buckets/docs/usage",
            json!(null),
        )
        .await;
    assert_eq!(
        (u["objects"].clone(), u["logical_bytes"].clone()),
        (json!(1), json!(50))
    );
    // unknown bucket, and other users cannot read usage
    let (s, _) = env
        .json(
            Some(admin()),
            Method::GET,
            "/api/v1/storage/buckets/nope/usage",
            json!(null),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = env
        .json(
            Some(user("eve")),
            Method::GET,
            "/api/v1/storage/buckets/docs/usage",
            json!(null),
        )
        .await;
    assert!(s == StatusCode::FORBIDDEN || s == StatusCode::NOT_FOUND, "{s}");

    // JSON metadata
    let (s, m) = env
        .json(
            Some(admin()),
            Method::GET,
            "/api/v1/storage/buckets/docs/meta/a.txt",
            json!(null),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{m}");
    assert_eq!(m["key"], "a.txt");
    assert_eq!(m["size"], 50);
    assert_eq!(m["content_type"], "text/plain");
    assert_eq!(m["metadata"]["owner"], "bob");
    assert_eq!(m["is_latest"], true);
    // an older version by id
    let (_, vs) = env
        .json(
            Some(admin()),
            Method::GET,
            "/api/v1/storage/buckets/docs/versions/a.txt",
            json!(null),
        )
        .await;
    let old = vs["items"][1]["version_id"].as_str().unwrap().to_owned();
    let (s, m) = env
        .json(
            Some(admin()),
            Method::GET,
            &format!("/api/v1/storage/buckets/docs/meta/a.txt?version={old}"),
            json!(null),
        )
        .await;
    assert_eq!((s, m["size"].clone()), (StatusCode::OK, json!(100)));
    // deleted and missing objects are 404
    let (s, _) = env
        .json(
            Some(admin()),
            Method::GET,
            "/api/v1/storage/buckets/docs/meta/b.txt",
            json!(null),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // HEAD: headers only
    let (s, h, body) = env
        .req(
            Some(admin()),
            Method::HEAD,
            "/api/v1/storage/buckets/docs/meta/a.txt",
            &[],
            Body::empty(),
        )
        .await;
    assert_eq!((s, body.len()), (StatusCode::OK, 0));
    assert_eq!(h["content-length"], "50");
    assert_eq!(h["content-type"], "text/plain");
    assert_eq!(h["x-forgebase-meta-owner"], "bob");
    assert!(h.contains_key("x-forgebase-version-id"));
    let (s, _, _) = env
        .req(
            Some(admin()),
            Method::HEAD,
            "/api/v1/storage/buckets/docs/meta/zzz",
            &[],
            Body::empty(),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // anonymous callers are refused
    let (s, _, _) = env
        .req(
            None,
            Method::GET,
            "/api/v1/storage/buckets/docs/meta/a.txt",
            &[],
            Body::empty(),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}
