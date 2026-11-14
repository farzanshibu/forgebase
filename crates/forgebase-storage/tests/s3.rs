//! The S3 driver against a local mock S3 server (no network): SigV4 headers
//! are recomputed and compared on every request, bodies are checked against
//! their declared payload hash, multipart uploads and ranged reads are
//! observed, and the whole storage service runs on top of it. Pre-signed
//! URLs (query-string SigV4) are checked against AWS's documented example and
//! verified by the mock with code written from AWS's documentation, and the
//! service's native signed downloads are fetched from the mock.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod support;

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use common::*;
use forgebase_auth::net::{HttpsClient, TlsSettings};
use forgebase_core::SignedUrlsMode;
use forgebase_storage::s3::{MAX_PRESIGNED_SECS, PresignInput, presigned_url};
use forgebase_storage::sigv4::{Credentials, SignInput, sign};
use forgebase_storage::util::sha256_hex;
use forgebase_storage::{
    ByteStream, PresignGet, S3Config, S3Driver, SignRequest, SignedMode, StorageDriver, StorageError, body_of,
};
use hmac::{Hmac, KeyInit, Mac};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const NOW: i64 = 1_794_225_600;
const ACCESS: &str = "AKIATESTKEY";
const SECRET: &str = "secret/with+chars";

#[derive(Default)]
struct Store {
    objects: BTreeMap<String, Vec<u8>>,
    uploads: HashMap<String, BTreeMap<u32, Vec<u8>>>,
    log: Vec<String>,
    bad_signatures: Vec<String>,
    list_page: usize,
}

struct Mock {
    addr: std::net::SocketAddr,
    store: Arc<Mutex<Store>>,
}

impl Mock {
    async fn start(list_page: usize) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let store = Arc::new(Mutex::new(Store {
            list_page,
            ..Store::default()
        }));
        let shared = store.clone();
        tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else { return };
                let st = shared.clone();
                tokio::spawn(async move {
                    let _ = serve(sock, st, addr).await;
                });
            }
        });
        Self { addr, store }
    }

    fn driver(&self, part_size: usize, read_chunk: usize, prefix: &str) -> S3Driver {
        let mut cfg = S3Config::new(format!("http://{}", self.addr), "blobs-bucket", ACCESS, SECRET);
        cfg.part_size = part_size;
        cfg.read_chunk = read_chunk;
        cfg.prefix = prefix.into();
        let client = HttpsClient::new(&TlsSettings::new()).unwrap().with_max_body(1 << 20);
        S3Driver::with_clock(cfg, Arc::new(client), Arc::new(|| NOW)).unwrap()
    }

    fn count(&self, what: &str) -> usize {
        self.store
            .lock()
            .unwrap()
            .log
            .iter()
            .filter(|l| l.starts_with(what))
            .count()
    }

    fn keys(&self) -> Vec<String> {
        self.store.lock().unwrap().objects.keys().cloned().collect()
    }
}

fn pct_decode(s: &str) -> String {
    url::form_urlencoded::parse(format!("x={}", s.replace('+', "%2B")).as_bytes())
        .next()
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default()
}

/// RFC 3986 encoding as SigV4 wants it (unreserved characters kept).
fn uri_encode(s: &str, keep_slash: bool) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => char::from(b).to_string(),
            b'/' if keep_slash => "/".to_owned(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn mac(key: &[u8], data: &str) -> Vec<u8> {
    let mut m = <Hmac<sha2::Sha256> as KeyInit>::new_from_slice(key).unwrap();
    m.update(data.as_bytes());
    m.finalize().into_bytes().to_vec()
}

/// Verifies a pre-signed request as S3 does ("Authenticating Requests: Using
/// Query Parameters"), written from the documentation rather than the driver:
/// the canonical query is every parameter but `X-Amz-Signature`, re-encoded
/// and sorted; the only signed header is `host`; the payload is unsigned.
/// Returns why it is refused.
fn verify_presigned(
    method: &str,
    host: &str,
    path: &str,
    query: &[(String, String)],
    (access, secret): (&str, &str),
) -> Result<(), String> {
    let get = |n: &str| query.iter().find(|(k, _)| k == n).map(|(_, v)| v.clone());
    if get("X-Amz-Algorithm").as_deref() != Some("AWS4-HMAC-SHA256") {
        return Err("algorithm".into());
    }
    if get("X-Amz-SignedHeaders").as_deref() != Some("host") {
        return Err("signed headers".into());
    }
    let expires: u64 = get("X-Amz-Expires").and_then(|e| e.parse().ok()).ok_or("expires")?;
    if !(1..=604_800).contains(&expires) {
        return Err("expires out of range".into());
    }
    let credential = get("X-Amz-Credential").ok_or("credential")?;
    let scope: Vec<&str> = credential.split('/').collect();
    if scope.len() != 5 || scope[0] != access || scope[3] != "s3" || scope[4] != "aws4_request" {
        return Err(format!("credential {credential}"));
    }
    let date = get("X-Amz-Date").ok_or("date")?;
    if !date.starts_with(scope[1]) {
        return Err("date and scope disagree".into());
    }
    let mut pairs: Vec<String> = query
        .iter()
        .filter(|(k, _)| k != "X-Amz-Signature")
        .map(|(k, v)| format!("{}={}", uri_encode(k, false), uri_encode(v, false)))
        .collect();
    pairs.sort();
    let canonical = format!(
        "{method}\n{}\n{}\nhost:{host}\n\nhost\nUNSIGNED-PAYLOAD",
        uri_encode(path, true),
        pairs.join("&")
    );
    let digest = sha256_hex(canonical.as_bytes());
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{date}\n{}/{}/s3/aws4_request\n{digest}",
        scope[1], scope[2]
    );
    let mut k = mac(format!("AWS4{secret}").as_bytes(), scope[1]);
    for part in [scope[2], "s3", "aws4_request"] {
        k = mac(&k, part);
    }
    let want = forgebase_storage::util::hex(&mac(&k, &to_sign));
    if get("X-Amz-Signature").as_deref() == Some(want.as_str()) {
        Ok(())
    } else {
        Err("SignatureDoesNotMatch".into())
    }
}

async fn serve(
    mut sock: tokio::net::TcpStream,
    st: Arc<Mutex<Store>>,
    addr: std::net::SocketAddr,
) -> std::io::Result<()> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let head_end = loop {
        let n = sock.read(&mut chunk).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap().split(' ');
    let (method, target) = (first.next().unwrap().to_owned(), first.next().unwrap().to_owned());
    let headers: HashMap<String, String> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    let len: usize = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    while buf.len() < head_end + len {
        let n = sock.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let body = buf[head_end..head_end + len].to_vec();
    let (raw_path, raw_query) = target.split_once('?').unwrap_or((&target, ""));
    let path = pct_decode(raw_path);
    let query: Vec<(String, String)> = url::form_urlencoded::parse(raw_query.as_bytes()).into_owned().collect();
    let q = |name: &str| query.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());

    // Verify the signature by recomputing it from what arrived.
    let auth = headers.get("authorization").cloned().unwrap_or_default();
    let signed_names: Vec<String> = auth
        .split("SignedHeaders=")
        .nth(1)
        .and_then(|s| s.split(',').next())
        .map(|s| s.split(';').map(str::to_owned).collect())
        .unwrap_or_default();
    let extra: Vec<(String, String)> = signed_names
        .iter()
        .filter(|n| {
            !matches!(
                n.as_str(),
                "host" | "x-amz-date" | "x-amz-content-sha256" | "x-amz-security-token"
            )
        })
        .map(|n| (n.clone(), headers.get(n).cloned().unwrap_or_default()))
        .collect();
    let payload = headers.get("x-amz-content-sha256").cloned().unwrap_or_default();
    // The region is part of the credential scope the client signed with.
    let region = auth
        .split("Credential=")
        .nth(1)
        .and_then(|c| c.split('/').nth(2))
        .unwrap_or("us-east-1")
        .to_owned();
    let expected = sign(
        &Credentials {
            access_key: ACCESS.into(),
            secret_key: SECRET.into(),
            session_token: None,
            region,
            service: "s3".into(),
        },
        &SignInput {
            method: &method,
            host: &addr.to_string(),
            path: &path,
            query: &query,
            headers: &extra,
            payload_sha256: &payload,
            now: NOW,
        },
    );
    let want = &expected.iter().find(|(k, _)| k == "authorization").unwrap().1;
    let presigned = q("X-Amz-Signature").is_some();
    let verdict = if presigned {
        if headers.contains_key("authorization") {
            Err("both header and query authentication".to_owned())
        } else {
            verify_presigned(&method, &addr.to_string(), &path, &query, (ACCESS, SECRET))
        }
    } else if *want == auth && sha256_hex(&body) == payload {
        Ok(())
    } else {
        Err("SignatureDoesNotMatch".to_owned())
    };
    let mut reply: (u16, Vec<(String, String)>, Vec<u8>) = (200, vec![], vec![]);
    {
        let mut s = st.lock().unwrap();
        s.log.push(format!(
            "{method} {path}?{}",
            query.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join("&")
        ));
        if let Err(why) = verdict {
            s.log.push(format!("refused {why}"));
            s.bad_signatures.push(format!("{method} {target}"));
            reply = (
                403,
                vec![],
                b"<Error><Code>SignatureDoesNotMatch</Code></Error>".to_vec(),
            );
        } else {
            let rel = path.strip_prefix("/blobs-bucket/").map(str::to_owned);
            match (method.as_str(), rel) {
                ("GET", _) if q("list-type").is_some() => {
                    let prefix = q("prefix").unwrap_or_default();
                    let after = q("continuation-token").unwrap_or_default();
                    let matching: Vec<(&String, &Vec<u8>)> = s
                        .objects
                        .iter()
                        .filter(|(k, _)| k.starts_with(&prefix) && k.as_str() > after.as_str())
                        .collect();
                    let page = if s.list_page == 0 { matching.len() } else { s.list_page };
                    let mut xml = String::from("<ListBucketResult>");
                    for (k, v) in matching.iter().take(page) {
                        xml.push_str(&format!("<Contents><Key>{}</Key><Size>{}</Size><LastModified>2026-11-09T12:00:00.000Z</LastModified></Contents>", k.replace('&', "&amp;"), v.len()));
                    }
                    if matching.len() > page {
                        xml.push_str(&format!(
                            "<IsTruncated>true</IsTruncated><NextContinuationToken>{}</NextContinuationToken>",
                            matching[page - 1].0
                        ));
                    } else {
                        xml.push_str("<IsTruncated>false</IsTruncated>");
                    }
                    xml.push_str("</ListBucketResult>");
                    reply.2 = xml.into_bytes();
                }
                ("POST", Some(key)) if q("uploads").is_some() => {
                    let _ = key;
                    let id = format!("upload-{}", s.uploads.len() + 1);
                    s.uploads.insert(id.clone(), BTreeMap::new());
                    reply.2 = format!(
                        "<InitiateMultipartUploadResult><UploadId>{id}</UploadId></InitiateMultipartUploadResult>"
                    )
                    .into_bytes();
                }
                ("POST", Some(key)) => {
                    let id = q("uploadId").unwrap();
                    let parts = s.uploads.remove(&id).unwrap_or_default();
                    let xml = String::from_utf8_lossy(&body).into_owned();
                    let listed = xml.matches("<PartNumber>").count();
                    assert_eq!(listed, parts.len(), "complete names every part");
                    s.objects.insert(key, parts.into_values().flatten().collect());
                    reply.2 = b"<CompleteMultipartUploadResult><ETag>\"done\"</ETag></CompleteMultipartUploadResult>"
                        .to_vec();
                }
                ("PUT", Some(key)) if headers.contains_key("x-amz-copy-source") => {
                    let src = pct_decode(headers["x-amz-copy-source"].trim_start_matches('/'))
                        .trim_start_matches("blobs-bucket/")
                        .to_owned();
                    match s.objects.get(&src).cloned() {
                        Some(v) => {
                            s.objects.insert(key, v);
                            reply.2 = b"<CopyObjectResult><ETag>\"c\"</ETag></CopyObjectResult>".to_vec();
                        }
                        None => reply = (404, vec![], b"<Error><Code>NoSuchKey</Code></Error>".to_vec()),
                    }
                }
                ("PUT", Some(_)) if q("partNumber").is_some() => {
                    let id = q("uploadId").unwrap();
                    let n: u32 = q("partNumber").unwrap().parse().unwrap();
                    s.uploads.get_mut(&id).unwrap().insert(n, body.clone());
                    reply.1.push(("ETag".into(), format!("\"part-{n}\"")));
                }
                ("PUT", Some(key)) => {
                    s.objects.insert(key, body.clone());
                }
                ("DELETE", Some(_)) if q("uploadId").is_some() => {
                    s.uploads.remove(&q("uploadId").unwrap());
                    reply.0 = 204;
                }
                ("DELETE", Some(key)) => {
                    s.objects.remove(&key);
                    reply.0 = 204;
                }
                ("HEAD", Some(key)) => match s.objects.get(&key) {
                    Some(v) => reply.1.push(("Content-Length".into(), v.len().to_string())),
                    None => reply.0 = 404,
                },
                ("GET", Some(key)) => match s.objects.get(&key) {
                    Some(v) => {
                        // Signed response overrides (pre-signed URLs only, as on S3).
                        for (param, header) in [
                            ("response-content-type", "Content-Type"),
                            ("response-content-disposition", "Content-Disposition"),
                        ] {
                            if let Some(value) = q(param).filter(|_| presigned) {
                                reply.1.push((header.into(), value));
                            }
                        }
                        let range = headers
                            .get("range")
                            .and_then(|r| r.strip_prefix("bytes="))
                            .and_then(|r| r.split_once('-'));
                        match range {
                            Some((a, b)) => {
                                let (a, b): (usize, usize) = (a.parse().unwrap(), b.parse().unwrap());
                                reply.0 = 206;
                                reply.2 = v[a..=b.min(v.len() - 1)].to_vec();
                            }
                            None => reply.2 = v.clone(),
                        }
                    }
                    None => reply = (404, vec![], b"<Error><Code>NoSuchKey</Code></Error>".to_vec()),
                },
                _ => reply = (400, vec![], b"<Error><Code>Unsupported</Code></Error>".to_vec()),
            }
        }
    }
    let custom_len = reply.1.iter().any(|(k, _)| k == "Content-Length");
    let mut out = format!("HTTP/1.1 {} X\r\nConnection: close\r\n", reply.0);
    if !custom_len {
        out.push_str(&format!("Content-Length: {}\r\n", reply.2.len()));
    }
    for (k, v) in &reply.1 {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str("\r\n");
    sock.write_all(out.as_bytes()).await?;
    if method != "HEAD" {
        sock.write_all(&reply.2).await?;
    }
    sock.shutdown().await
}

fn stream_of(items: Vec<std::io::Result<Vec<u8>>>) -> ByteStream {
    Box::pin(futures_util::stream::iter(
        items.into_iter().map(|r| r.map(axum::body::Bytes::from)),
    ))
}

#[tokio::test]
async fn driver_round_trip_with_signed_requests_multipart_and_ranges() {
    let mock = Mock::start(2).await;
    let d = mock.driver(1000, 700, "fb");
    let small = bytes_of(400);
    assert_eq!(d.put("blobs/aa/small", body_of(small.clone())).await.unwrap(), 400);
    assert_eq!(mock.count("PUT /blobs-bucket/fb/blobs/aa/small"), 1);
    assert_eq!(mock.count("POST"), 0, "small bodies are one PutObject");

    let big = bytes_of(3500);
    let n = d.put("blobs/bb/big", chunked(big.clone(), 333)).await.unwrap();
    assert_eq!(n, 3500);
    assert_eq!(mock.count("POST /blobs-bucket/fb/blobs/bb/big?uploads"), 1);
    assert_eq!(
        mock.count("PUT /blobs-bucket/fb/blobs/bb/big?partNumber&uploadId"),
        4,
        "3 full parts + the rest"
    );
    assert_eq!(mock.count("POST /blobs-bucket/fb/blobs/bb/big?uploadId"), 1);
    assert_eq!(mock.store.lock().unwrap().objects["fb/blobs/bb/big"], big);

    // Full read pages through ranged GETs of at most read_chunk bytes.
    let before = mock.count("GET /blobs-bucket/fb/blobs/bb/big");
    let all = forgebase_storage::driver::collect(d.get("blobs/bb/big", None).await.unwrap())
        .await
        .unwrap();
    assert_eq!(all, big);
    assert_eq!(
        mock.count("GET /blobs-bucket/fb/blobs/bb/big") - before,
        5,
        "3500 / 700"
    );
    let part = forgebase_storage::driver::collect(d.get("blobs/bb/big", Some((1000, 1500))).await.unwrap())
        .await
        .unwrap();
    assert_eq!(part, big[1000..1500]);

    let head = d.head("blobs/bb/big").await.unwrap().unwrap();
    assert_eq!(head.size, 3500);
    assert!(d.head("blobs/none").await.unwrap().is_none());
    assert!(matches!(
        d.get("blobs/none", None).await,
        Err(StorageError::NotFound(_))
    ));

    d.copy("blobs/aa/small", "blobs/cc/copy").await.unwrap();
    d.rename("blobs/cc/copy", "blobs/dd/moved").await.unwrap();
    assert!(d.head("blobs/cc/copy").await.unwrap().is_none());
    assert_eq!(d.head("blobs/dd/moved").await.unwrap().unwrap().size, 400);

    // Listing strips the prefix, filters, and follows continuation tokens (page of 2).
    let listed: Vec<String> = d.list("blobs/").await.unwrap().into_iter().map(|o| o.key).collect();
    assert_eq!(listed, ["blobs/aa/small", "blobs/bb/big", "blobs/dd/moved"]);
    assert!(d.list("blobs/").await.unwrap().iter().all(|o| o.modified_ms > 0));

    d.delete("blobs/dd/moved").await.unwrap();
    d.delete("blobs/dd/moved").await.unwrap(); // idempotent
    d.ping().await.unwrap();
    assert!(
        mock.store.lock().unwrap().bad_signatures.is_empty(),
        "{:?}",
        mock.store.lock().unwrap().bad_signatures
    );
}

#[tokio::test]
async fn a_failing_stream_aborts_the_multipart_upload() {
    let mock = Mock::start(0).await;
    let d = mock.driver(1000, 700, "");
    let mut items: Vec<std::io::Result<Vec<u8>>> = vec![Ok(vec![1; 1500]), Ok(vec![2; 1500])];
    items.push(Err(std::io::Error::new(std::io::ErrorKind::ConnectionReset, "reset")));
    assert!(d.put("blobs/xx/failed", stream_of(items)).await.is_err());
    assert_eq!(
        mock.count("DELETE /blobs-bucket/blobs/xx/failed?uploadId"),
        1,
        "the upload is aborted"
    );
    assert!(mock.keys().is_empty());
    assert!(mock.store.lock().unwrap().uploads.is_empty());
}

#[tokio::test]
async fn a_wrong_secret_is_refused_by_the_service() {
    let mock = Mock::start(0).await;
    let mut cfg = S3Config::new(
        format!("http://{}", mock.addr),
        "blobs-bucket",
        ACCESS,
        "not-the-secret",
    );
    cfg.part_size = 1000;
    let client = HttpsClient::new(&TlsSettings::new()).unwrap();
    let d = S3Driver::with_clock(cfg, Arc::new(client), Arc::new(|| NOW)).unwrap();
    let err = d.put("blobs/aa/x", body_of(vec![1; 10])).await;
    assert!(
        matches!(err, Err(StorageError::Driver(m)) if m.contains("403")),
        "SignatureDoesNotMatch"
    );
}

#[tokio::test]
async fn the_storage_service_runs_on_the_s3_driver() {
    let mock = Mock::start(3).await;
    let driver: Arc<dyn StorageDriver> = Arc::new(mock.driver(2000, 1500, "fb"));
    let env = Env::with(|_| {}, Some(driver)).await;
    env.bucket("s3b", true).await;
    let data = bytes_of(7000);
    let info = env
        .storage
        .put_object(&admin(), put("s3b", "dir/file.bin", &data))
        .await
        .unwrap();
    env.storage
        .put_object(&admin(), put("s3b", "dir/same.bin", &data))
        .await
        .unwrap();
    // Deduplicated: one blob in the bucket, uploaded as a multipart upload.
    let keys = mock.keys();
    assert_eq!(keys.len(), 1, "{keys:?}");
    assert!(keys[0].starts_with("fb/blobs/") && keys[0].ends_with(&info.checksum_sha256.clone().unwrap()));
    assert!(mock.count("POST") >= 2);

    let r = env
        .storage
        .get_object(&admin(), "s3b", "dir/file.bin", None, None)
        .await
        .unwrap();
    assert_eq!(read_all(r).await, data);
    let r = env
        .storage
        .get_object(
            &admin(),
            "s3b",
            "dir/file.bin",
            None,
            Some(forgebase_storage::RangeSpec::Bounded(1900, 4100)),
        )
        .await
        .unwrap();
    assert_eq!(read_all(r).await, data[1900..=4100]);

    // Multipart session over S3 staging, completed into the same blob.
    let up = env
        .storage
        .create_upload(
            &admin(),
            "s3b",
            forgebase_storage::UploadInput {
                key: "assembled".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    env.storage
        .upload_part(&admin(), &up.id, 1, None, chunked(data[..3000].to_vec(), 500))
        .await
        .unwrap();
    env.storage
        .upload_part(&admin(), &up.id, 2, None, chunked(data[3000..].to_vec(), 500))
        .await
        .unwrap();
    let done = env
        .storage
        .complete_upload(&admin(), &up.id, Default::default())
        .await
        .unwrap();
    assert_eq!(done.checksum_sha256, info.checksum_sha256);
    assert_eq!(mock.keys().len(), 1, "same content, same blob");

    // Reconciler removes an orphan blob and stale staging from the S3 bucket.
    env.driver.put("blobs/ab/orphan", body_of(vec![1; 5])).await.unwrap();
    env.driver
        .put("staging/single/stale", body_of(vec![1; 5]))
        .await
        .unwrap();
    let report = env.storage.reconcile(&admin()).await.unwrap();
    assert_eq!(report.staging_deleted, 1);
    assert!(report.orphan_blobs_deleted <= 1);
    // The orphan key is not 64 hex characters of a known blob: it is unknown to the metadata.
    assert!(
        !mock
            .keys()
            .iter()
            .any(|k| k.ends_with("blobs/ab/orphan") || k.contains("staging/"))
    );
    assert!(mock.store.lock().unwrap().bad_signatures.is_empty());
}

#[tokio::test]
async fn conformance_on_the_s3_driver() {
    let mock = Mock::start(3).await;
    let d = mock.driver(100_000, 100_000, "fb");
    support::conformance::run(&d, "s3", 350_000).await;
    assert!(mock.store.lock().unwrap().bad_signatures.is_empty());
    assert!(
        mock.store.lock().unwrap().uploads.is_empty(),
        "failed uploads are aborted"
    );
}

/// Google Cloud Storage through its S3 interoperability API (HMAC keys): the
/// signing region is `auto`, the endpoint is typed with a trailing slash and
/// the prefix with stray slashes.
#[tokio::test]
async fn gcs_interoperability_settings_work_on_the_s3_driver() {
    let mock = Mock::start(2).await;
    let mut cfg = S3Config::new(format!("http://{}/", mock.addr), "blobs-bucket", ACCESS, SECRET);
    cfg.region = "auto".into();
    cfg.prefix = "/fb/".into();
    cfg.part_size = 100_000;
    cfg.read_chunk = 100_000;
    let client = HttpsClient::new(&TlsSettings::new()).unwrap().with_max_body(1 << 20);
    let d = S3Driver::with_clock(cfg, Arc::new(client), Arc::new(|| NOW)).unwrap();
    support::conformance::run(&d, "s3", 250_000).await;
    assert!(mock.store.lock().unwrap().bad_signatures.is_empty());
    assert!(mock.keys().iter().all(|k| k.starts_with("fb/")), "{:?}", mock.keys());
}

fn get_of(url: &str) -> forgebase_auth::social::HttpRequest {
    forgebase_auth::social::HttpRequest {
        method: "GET",
        url: url.to_owned(),
        headers: Vec::new(),
        body: Vec::new(),
    }
}

async fn fetch(url: &str) -> forgebase_auth::social::HttpResponse {
    use forgebase_auth::social::HttpClient as _;
    let client = HttpsClient::new(&TlsSettings::new()).unwrap().with_max_body(1 << 20);
    client.send(get_of(url)).await.unwrap()
}

fn query_of(url: &str) -> Vec<(String, String)> {
    url::Url::parse(url).unwrap().query_pairs().into_owned().collect()
}

/// AWS's documented pre-signed URL ("Authenticating Requests: Using Query
/// Parameters (AWS Signature Version 4)": GET `test.txt`, 86400 seconds).
#[test]
fn presigned_urls_match_awss_documented_example() {
    let c = Credentials {
        access_key: "AKIAIOSFODNN7EXAMPLE".into(),
        secret_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(),
        session_token: None,
        region: "us-east-1".into(),
        service: "s3".into(),
    };
    let url = presigned_url(
        &c,
        &PresignInput {
            method: "GET",
            scheme: "https",
            host: "examplebucket.s3.amazonaws.com",
            path: "/test.txt",
            query: &[],
            expires_secs: 86_400,
            now: 1_369_353_600,
        },
    );
    assert_eq!(
        url,
        "https://examplebucket.s3.amazonaws.com/test.txt?X-Amz-Algorithm=AWS4-HMAC-SHA256\
         &X-Amz-Credential=AKIAIOSFODNN7EXAMPLE%2F20130524%2Fus-east-1%2Fs3%2Faws4_request\
         &X-Amz-Date=20130524T000000Z&X-Amz-Expires=86400&X-Amz-SignedHeaders=host\
         &X-Amz-Signature=aeeed9bbccd4d02ee5c0109b86d86835f995330da4c265957d157751f604d404"
    );
    // The mock's verifier, written separately from the documentation, agrees.
    let keys = ("AKIAIOSFODNN7EXAMPLE", "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY");
    let q = query_of(&url);
    assert_eq!(
        verify_presigned("GET", "examplebucket.s3.amazonaws.com", "/test.txt", &q, keys),
        Ok(())
    );
    assert!(verify_presigned("PUT", "examplebucket.s3.amazonaws.com", "/test.txt", &q, keys).is_err());
    assert!(verify_presigned("GET", "examplebucket.s3.amazonaws.com", "/other.txt", &q, keys).is_err());
}

#[tokio::test]
async fn driver_presigned_urls_are_scoped_to_one_key_and_bounded() {
    let mock = Mock::start(0).await;
    let d = mock.driver(1000, 700, "fb");
    d.put("blobs/aa/one", body_of(b"first".to_vec())).await.unwrap();
    d.put("blobs/bb/two", body_of(b"second".to_vec())).await.unwrap();
    let url = d.signed_url("GET", "blobs/aa/one", 600).unwrap();
    assert!(
        url.starts_with(&format!(
            "http://{}/blobs-bucket/fb/blobs/aa/one?X-Amz-Algorithm=AWS4-HMAC-SHA256\
             &X-Amz-Credential={ACCESS}%2F20261109%2Fus-east-1%2Fs3%2Faws4_request\
             &X-Amz-Date=20261109T120000Z&X-Amz-Expires=600&X-Amz-SignedHeaders=host&X-Amz-Signature=",
            mock.addr
        )),
        "{url}"
    );
    let r = fetch(&url).await;
    assert_eq!((r.status, r.body.as_slice()), (200, &b"first"[..]));
    // One object, one method, one lifetime: any change breaks the signature.
    for forged in [
        url.replace("blobs/aa/one", "blobs/bb/two"),
        url.replace("X-Amz-Expires=600", "X-Amz-Expires=6000"),
        format!("{url}&response-content-type=text%2Fhtml"),
    ] {
        assert_eq!(fetch(&forged).await.status, 403, "{forged}");
    }
    // A PUT URL uploads, and is not a GET URL.
    let put_url = d.signed_url("PUT", "blobs/cc/three", 60).unwrap();
    let mut req = get_of(&put_url);
    req.method = "PUT";
    req.body = b"third".to_vec();
    use forgebase_auth::social::HttpClient as _;
    let client = HttpsClient::new(&TlsSettings::new()).unwrap();
    assert_eq!(client.send(req).await.unwrap().status, 200);
    assert_eq!(mock.store.lock().unwrap().objects["fb/blobs/cc/three"], b"third");
    assert_eq!(fetch(&put_url).await.status, 403);
    let refused = mock.store.lock().unwrap().bad_signatures.len();
    assert_eq!(refused, 4, "exactly the forged requests were refused");

    // Lifetimes: 1 second to 7 days.
    for bad in [0, MAX_PRESIGNED_SECS + 1] {
        assert!(matches!(
            d.signed_url("GET", "k", bad),
            Err(StorageError::Invalid {
                code: "invalid_expiry",
                ..
            })
        ));
    }
    assert!(d.signed_url("GET", "k", MAX_PRESIGNED_SECS).is_ok());
    assert_eq!(d.presign_support(), Ok(MAX_PRESIGNED_SECS));

    // Response overrides are signed and served.
    let url = d
        .presign_get(&PresignGet {
            key: "blobs/aa/one",
            now: NOW,
            expires_secs: 60,
            content_type: Some("text/plain; charset=utf-8"),
            content_disposition: Some("attachment; filename=\"one.txt\""),
        })
        .unwrap();
    assert!(url.contains("&response-content-disposition=attachment%3B%20filename%3D%22one.txt%22"));
    let r = fetch(&url).await;
    assert_eq!(r.status, 200);
    assert_eq!(r.header("content-type"), Some("text/plain; charset=utf-8"));
    assert_eq!(
        r.header("content-disposition"),
        Some("attachment; filename=\"one.txt\"")
    );
    let swapped = url.replace("text%2Fplain", "text%2Fhtml");
    assert_eq!(fetch(&swapped).await.status, 403);

    // Temporary credentials sign their session token into the URL.
    let mut cfg = S3Config::new(format!("http://{}", mock.addr), "blobs-bucket", ACCESS, SECRET);
    cfg.prefix = "fb".into();
    cfg.session_token = Some("tok/en+1".into());
    let temp = S3Driver::with_clock(
        cfg,
        Arc::new(HttpsClient::new(&TlsSettings::new()).unwrap()),
        Arc::new(|| NOW),
    )
    .unwrap();
    let url = temp.signed_url("GET", "blobs/aa/one", 60).unwrap();
    assert!(url.contains("&X-Amz-Security-Token=tok%2Fen%2B1&"), "{url}");
    assert_eq!(fetch(&url).await.status, 200);
    // Without keys there is nothing to sign with.
    let cfg = S3Config::new(format!("http://{}", mock.addr), "blobs-bucket", "", "");
    let anon = S3Driver::new(cfg, Arc::new(HttpsClient::new(&TlsSettings::new()).unwrap())).unwrap();
    assert!(anon.presign_support().is_err());
    assert!(matches!(
        anon.signed_url("GET", "k", 60),
        Err(StorageError::Invalid {
            code: "s3_signing_unavailable",
            ..
        })
    ));
}

#[tokio::test]
async fn native_signed_downloads_are_served_by_s3() {
    let mock = Mock::start(0).await;
    let driver: Arc<dyn StorageDriver> = Arc::new(mock.driver(2000, 1500, "fb"));
    let env = Env::with(
        |s| {
            s.signed_urls = SignedUrlsMode::Native;
            s.max_signed_ttl_secs = 30 * 86_400;
        },
        Some(driver),
    )
    .await;
    env.bucket("n", false).await;
    let data = b"hello from the bucket".to_vec();
    let info = env
        .storage
        .put_object(
            &admin(),
            forgebase_storage::PutRequest {
                content_type: Some("text/plain".into()),
                ..put("n", "docs/hello.txt", &data)
            },
        )
        .await
        .unwrap();
    let sha = info.checksum_sha256.clone().unwrap();
    let sign = |expires_in: u64| SignRequest {
        key: "docs/hello.txt".into(),
        method: None,
        expires_in: Some(expires_in),
        version_id: None,
        download_as: Some("hello.txt".into()),
        content_type: None,
    };
    let signed = env.storage.sign_url(&admin(), "n", sign(300)).await.unwrap();
    assert_eq!(signed.mode, SignedMode::Native);
    let blob_url = format!("http://{}/blobs-bucket/fb/blobs/{}/{sha}?", mock.addr, &sha[..2]);
    assert!(signed.url.starts_with(&blob_url), "{}", signed.url);
    assert!(signed.url.contains("&X-Amz-Expires=300&"));
    let gets_before = mock.count("GET");
    let r = fetch(&signed.url).await;
    assert_eq!((r.status, &r.body), (200, &data));
    assert_eq!(r.header("content-type"), Some("text/plain"));
    assert_eq!(
        r.header("content-disposition"),
        Some("attachment; filename=\"hello.txt\"")
    );
    assert_eq!(
        mock.count("GET") - gets_before,
        1,
        "one request, straight to the bucket"
    );
    // Longer than S3 allows: capped at seven days, and reported so.
    let long = env.storage.sign_url(&admin(), "n", sign(20 * 86_400)).await.unwrap();
    assert!(long.url.contains("&X-Amz-Expires=604800&"), "{}", long.url);
    let at = time::OffsetDateTime::parse(&long.expires_at, &time::format_description::well_known::Rfc3339)
        .unwrap()
        .unix_timestamp();
    let q = query_of(&long.url);
    let date = &q.iter().find(|(k, _)| k == "X-Amz-Date").unwrap().1;
    assert_eq!(
        &forgebase_storage::sigv4::stamps(at - 604_800).1,
        date,
        "expires_at is the signing time plus seven days"
    );
    assert_eq!(fetch(&long.url).await.status, 200);
    // Signing needs read access, as for a proxy URL.
    assert!(matches!(
        env.storage.sign_url(&user("mallory"), "n", sign(60)).await,
        Err(StorageError::Forbidden(_))
    ));
    assert!(mock.store.lock().unwrap().bad_signatures.is_empty());
}
