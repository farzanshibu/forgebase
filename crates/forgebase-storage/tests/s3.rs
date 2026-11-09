//! The S3 driver against a local mock S3 server (no network): SigV4 headers
//! are recomputed and compared on every request, bodies are checked against
//! their declared payload hash, multipart uploads and ranged reads are
//! observed, and the whole storage service runs on top of it.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use common::*;
use forgebase_auth::net::{HttpsClient, TlsSettings};
use forgebase_storage::sigv4::{Credentials, SignInput, sign};
use forgebase_storage::util::sha256_hex;
use forgebase_storage::{ByteStream, S3Config, S3Driver, StorageDriver, StorageError, body_of};
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
    let expected = sign(
        &Credentials {
            access_key: ACCESS.into(),
            secret_key: SECRET.into(),
            session_token: None,
            region: "us-east-1".into(),
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
    let mut reply: (u16, Vec<(String, String)>, Vec<u8>) = (200, vec![], vec![]);
    {
        let mut s = st.lock().unwrap();
        s.log.push(format!(
            "{method} {path}?{}",
            query.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join("&")
        ));
        if *want != auth || sha256_hex(&body) != payload {
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
