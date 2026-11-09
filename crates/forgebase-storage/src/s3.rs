//! An S3-compatible driver: SigV4-signed requests over the FB-25 outbound
//! client ([`forgebase_auth::social::HttpClient`], `https://` through rustls
//! or plain `http://` for a loopback mock or a TLS-terminating proxy).
//!
//! Reads page through the blob with ranged `GET`s of at most `read_chunk`
//! bytes, so memory stays bounded; writes of more than `part_size` bytes use
//! a multipart upload (`CreateMultipartUpload`, `UploadPart`,
//! `CompleteMultipartUpload`), smaller ones a single `PutObject`. A failing
//! stream aborts the multipart upload, so nothing is left under the key.
//!
//! Path-style addressing (`<endpoint>/<bucket>/<key>`) is used; every
//! S3-compatible service accepts it.

use std::sync::Arc;

use axum::body::Bytes;
use forgebase_auth::social::{HttpClient, HttpRequest, HttpResponse};
use futures_util::StreamExt;

use crate::driver::{BoxFuture, ByteStream, DriverObject, StorageDriver};
use crate::error::{Result, StorageError};
use crate::sigv4::{Credentials, SignInput, encode, sign};
use crate::util::sha256_hex;

/// Settings of an S3-compatible store.
#[derive(Clone)]
pub struct S3Config {
    /// Service endpoint, e.g. `https://s3.eu-central-1.amazonaws.com` or a
    /// MinIO URL.
    pub endpoint: String,
    /// Region used for signing.
    pub region: String,
    /// Bucket holding the blobs.
    pub bucket: String,
    /// Access key id.
    pub access_key: String,
    /// Secret access key.
    pub secret_key: String,
    /// Session token of temporary credentials.
    pub session_token: Option<String>,
    /// Key prefix inside the bucket (no leading slash).
    pub prefix: String,
    /// Size of one multipart part (S3 requires at least 5 MiB, except for
    /// the last part).
    pub part_size: usize,
    /// Size of one ranged read.
    pub read_chunk: usize,
}

impl std::fmt::Debug for S3Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Config")
            .field("endpoint", &self.endpoint)
            .field("region", &self.region)
            .field("bucket", &self.bucket)
            .field("prefix", &self.prefix)
            .finish_non_exhaustive()
    }
}

impl S3Config {
    /// Defaults for everything but the endpoint, bucket and credentials.
    pub fn new(
        endpoint: impl Into<String>,
        bucket: impl Into<String>,
        access_key: impl Into<String>,
        secret_key: impl Into<String>,
    ) -> Self {
        Self {
            endpoint: endpoint.into(),
            region: "us-east-1".into(),
            bucket: bucket.into(),
            access_key: access_key.into(),
            secret_key: secret_key.into(),
            session_token: None,
            prefix: String::new(),
            part_size: 8 * 1024 * 1024,
            read_chunk: 4 * 1024 * 1024,
        }
    }
}

/// The S3 driver.
#[derive(Clone)]
pub struct S3Driver {
    inner: Arc<Inner>,
}

struct Inner {
    cfg: S3Config,
    creds: Credentials,
    client: Arc<dyn HttpClient>,
    scheme: String,
    host: String,
    base_path: String,
    clock: Arc<dyn Fn() -> i64 + Send + Sync>,
}

impl std::fmt::Debug for S3Driver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Driver")
            .field("cfg", &self.inner.cfg)
            .finish_non_exhaustive()
    }
}

struct Query(Vec<(String, String)>);

impl S3Driver {
    /// A driver talking to `cfg.endpoint` through `client`.
    pub fn new(cfg: S3Config, client: Arc<dyn HttpClient>) -> Result<Self> {
        Self::with_clock(
            cfg,
            client,
            Arc::new(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
            }),
        )
    }

    /// Like [`S3Driver::new`] with an injectable clock (epoch seconds).
    pub fn with_clock(
        cfg: S3Config,
        client: Arc<dyn HttpClient>,
        clock: Arc<dyn Fn() -> i64 + Send + Sync>,
    ) -> Result<Self> {
        let url = url::Url::parse(&cfg.endpoint)
            .map_err(|_| StorageError::invalid("invalid_s3_endpoint", "the S3 endpoint is not a URL"))?;
        let scheme = url.scheme().to_owned();
        if scheme != "http" && scheme != "https" {
            return Err(StorageError::invalid(
                "invalid_s3_endpoint",
                "the S3 endpoint must be http or https",
            ));
        }
        let host = match (url.host_str(), url.port()) {
            (Some(h), Some(p)) => format!("{h}:{p}"),
            (Some(h), None) => h.to_owned(),
            _ => {
                return Err(StorageError::invalid(
                    "invalid_s3_endpoint",
                    "the S3 endpoint has no host",
                ));
            }
        };
        let base_path = url.path().trim_end_matches('/').to_owned();
        let creds = Credentials {
            access_key: cfg.access_key.clone(),
            secret_key: cfg.secret_key.clone(),
            session_token: cfg.session_token.clone(),
            region: cfg.region.clone(),
            service: "s3".into(),
        };
        Ok(Self {
            inner: Arc::new(Inner {
                cfg,
                creds,
                client,
                scheme,
                host,
                base_path,
                clock,
            }),
        })
    }

    fn object_path(&self, key: &str) -> String {
        let c = &self.inner.cfg;
        let mut p = format!("{}/{}", self.inner.base_path, c.bucket);
        if !c.prefix.is_empty() {
            p.push('/');
            p.push_str(c.prefix.trim_matches('/'));
        }
        if !key.is_empty() {
            p.push('/');
            p.push_str(key);
        }
        p
    }

    fn bucket_key_path(&self, key: &str) -> String {
        let c = &self.inner.cfg;
        let prefix = c.prefix.trim_matches('/');
        if prefix.is_empty() {
            format!("/{}/{key}", c.bucket)
        } else {
            format!("/{}/{prefix}/{key}", c.bucket)
        }
    }

    async fn call(
        &self,
        method: &'static str,
        path: &str,
        query: Query,
        extra: Vec<(String, String)>,
        body: Vec<u8>,
    ) -> Result<HttpResponse> {
        let i = &self.inner;
        let payload = sha256_hex(&body);
        let signed = sign(
            &i.creds,
            &SignInput {
                method,
                host: &i.host,
                path,
                query: &query.0,
                headers: &extra,
                payload_sha256: &payload,
                now: (i.clock)(),
            },
        );
        let mut url = format!("{}://{}{}", i.scheme, i.host, encode(path, true));
        if !query.0.is_empty() {
            let q = query
                .0
                .iter()
                .map(|(k, v)| format!("{}={}", encode(k, false), encode(v, false)))
                .collect::<Vec<_>>()
                .join("&");
            url.push('?');
            url.push_str(&q);
        }
        let mut req = HttpRequest {
            method,
            url,
            headers: Vec::new(),
            body,
        };
        for (k, v) in extra.into_iter().chain(signed) {
            req = req.header(&k, v);
        }
        i.client
            .send(req)
            .await
            .map_err(|e| StorageError::Driver(format!("S3 request failed: {e}")))
    }

    fn check(resp: HttpResponse, what: &str) -> Result<HttpResponse> {
        if (200..300).contains(&resp.status) {
            Ok(resp)
        } else {
            Err(StorageError::Driver(format!(
                "S3 {what} returned {}: {}",
                resp.status,
                tag(&String::from_utf8_lossy(&resp.body), "Code").unwrap_or_default()
            )))
        }
    }

    async fn put_single(&self, key: &str, body: Vec<u8>) -> Result<()> {
        let r = self
            .call("PUT", &self.object_path(key), Query(vec![]), vec![], body)
            .await?;
        Self::check(r, "PutObject").map(|_| ())
    }

    async fn multipart(&self, key: &str, first: Vec<u8>, mut body: ByteStream) -> Result<u64> {
        let path = self.object_path(key);
        let r = self
            .call(
                "POST",
                &path,
                Query(vec![("uploads".into(), String::new())]),
                vec![],
                vec![],
            )
            .await?;
        let r = Self::check(r, "CreateMultipartUpload")?;
        let upload_id = tag(&String::from_utf8_lossy(&r.body), "UploadId")
            .ok_or_else(|| StorageError::Driver("S3 CreateMultipartUpload returned no UploadId".into()))?;
        let result = self.upload_parts(&path, &upload_id, first, &mut body).await;
        match result {
            Ok(total) => Ok(total),
            Err(e) => {
                let _ = self
                    .call(
                        "DELETE",
                        &path,
                        Query(vec![("uploadId".into(), upload_id)]),
                        vec![],
                        vec![],
                    )
                    .await;
                Err(e)
            }
        }
    }

    async fn upload_parts(&self, path: &str, upload_id: &str, first: Vec<u8>, body: &mut ByteStream) -> Result<u64> {
        let part_size = self.inner.cfg.part_size;
        let mut total = 0u64;
        let mut etags: Vec<(usize, String)> = Vec::new();
        let mut buf = first;
        let mut done = false;
        loop {
            while !done && buf.len() <= part_size {
                match body.next().await {
                    Some(chunk) => buf.extend_from_slice(&chunk?),
                    None => done = true,
                }
            }
            let rest = if buf.len() > part_size {
                buf.split_off(part_size)
            } else {
                Vec::new()
            };
            let n = etags.len() + 1;
            total += buf.len() as u64;
            let q = Query(vec![
                ("partNumber".into(), n.to_string()),
                ("uploadId".into(), upload_id.to_owned()),
            ]);
            let r = self.call("PUT", path, q, vec![], std::mem::take(&mut buf)).await?;
            let r = Self::check(r, "UploadPart")?;
            let etag = r
                .header("etag")
                .ok_or_else(|| StorageError::Driver("S3 UploadPart returned no ETag".into()))?
                .to_owned();
            etags.push((n, etag));
            buf = rest;
            if done && buf.is_empty() {
                break;
            }
        }
        let mut xml = String::from("<CompleteMultipartUpload>");
        for (n, etag) in &etags {
            xml.push_str(&format!(
                "<Part><PartNumber>{n}</PartNumber><ETag>{}</ETag></Part>",
                xml_escape(etag)
            ));
        }
        xml.push_str("</CompleteMultipartUpload>");
        let r = self
            .call(
                "POST",
                path,
                Query(vec![("uploadId".into(), upload_id.to_owned())]),
                vec![],
                xml.into_bytes(),
            )
            .await?;
        let r = Self::check(r, "CompleteMultipartUpload")?;
        // S3 can answer 200 with an error document.
        if String::from_utf8_lossy(&r.body).contains("<Error>") {
            return Err(StorageError::Driver("S3 CompleteMultipartUpload failed".into()));
        }
        Ok(total)
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Text of the first `<name>...</name>` in `xml`.
fn tag(xml: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(xml_unescape(&xml[start..end]))
}

fn parse_modified(s: &str) -> i64 {
    time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339)
        .map_or(0, |t| i64::try_from(t.unix_timestamp_nanos() / 1_000_000).unwrap_or(0))
}

impl StorageDriver for S3Driver {
    fn name(&self) -> &'static str {
        "s3"
    }

    fn put(&self, key: &str, mut body: ByteStream) -> BoxFuture<'_, Result<u64>> {
        let key = key.to_owned();
        Box::pin(async move {
            let part_size = self.inner.cfg.part_size.max(1);
            let mut buf: Vec<u8> = Vec::new();
            while buf.len() <= part_size {
                match body.next().await {
                    Some(chunk) => buf.extend_from_slice(&chunk?),
                    None => {
                        let n = buf.len() as u64;
                        self.put_single(&key, buf).await?;
                        return Ok(n);
                    }
                }
            }
            self.multipart(&key, buf, body).await
        })
    }

    fn get(&self, key: &str, range: Option<(u64, u64)>) -> BoxFuture<'_, Result<ByteStream>> {
        let key = key.to_owned();
        Box::pin(async move {
            let head = self
                .head(&key)
                .await?
                .ok_or_else(|| StorageError::NotFound("stored blob".into()))?;
            let (start, end) = range.map_or((0, head.size), |(s, e)| (s.min(head.size), e.min(head.size)));
            let chunk = self.inner.cfg.read_chunk.max(1) as u64;
            let this = self.clone();
            let stream = futures_util::stream::unfold((this, key, start), move |(this, key, pos)| async move {
                if pos >= end {
                    return None;
                }
                let to = (pos + chunk).min(end);
                let range = format!("bytes={pos}-{}", to - 1);
                let path = this.object_path(&key);
                let r = this
                    .call("GET", &path, Query(vec![]), vec![("range".into(), range)], vec![])
                    .await
                    .and_then(|r| Self::check(r, "GetObject"));
                match r {
                    Ok(resp) if resp.body.len() as u64 == to - pos => {
                        Some((Ok(Bytes::from(resp.body)), (this, key, to)))
                    }
                    Ok(_) => Some((
                        Err(std::io::Error::other("S3 returned an unexpected number of bytes")),
                        (this, key, end),
                    )),
                    Err(e) => Some((Err(std::io::Error::other(e.to_string())), (this, key, end))),
                }
            });
            Ok(Box::pin(stream) as ByteStream)
        })
    }

    fn head(&self, key: &str) -> BoxFuture<'_, Result<Option<DriverObject>>> {
        let key = key.to_owned();
        Box::pin(async move {
            let r = self
                .call("HEAD", &self.object_path(&key), Query(vec![]), vec![], vec![])
                .await?;
            if r.status == 404 {
                return Ok(None);
            }
            let r = Self::check(r, "HeadObject")?;
            let size = r
                .header("content-length")
                .and_then(|v| v.parse::<u64>().ok())
                .ok_or_else(|| StorageError::Driver("S3 HeadObject returned no Content-Length".into()))?;
            Ok(Some(DriverObject {
                key,
                size,
                modified_ms: 0,
            }))
        })
    }

    fn delete(&self, key: &str) -> BoxFuture<'_, Result<()>> {
        let key = key.to_owned();
        Box::pin(async move {
            let r = self
                .call("DELETE", &self.object_path(&key), Query(vec![]), vec![], vec![])
                .await?;
            if r.status == 404 {
                return Ok(());
            }
            Self::check(r, "DeleteObject").map(|_| ())
        })
    }

    fn copy(&self, from: &str, to: &str) -> BoxFuture<'_, Result<()>> {
        let (from, to) = (from.to_owned(), to.to_owned());
        Box::pin(async move {
            let source = encode(&self.bucket_key_path(&from), true);
            let r = self
                .call(
                    "PUT",
                    &self.object_path(&to),
                    Query(vec![]),
                    vec![("x-amz-copy-source".into(), source)],
                    vec![],
                )
                .await?;
            if r.status == 404 {
                return Err(StorageError::NotFound("stored blob".into()));
            }
            let r = Self::check(r, "CopyObject")?;
            if String::from_utf8_lossy(&r.body).contains("<Error>") {
                return Err(StorageError::Driver("S3 CopyObject failed".into()));
            }
            Ok(())
        })
    }

    fn list(&self, prefix: &str) -> BoxFuture<'_, Result<Vec<DriverObject>>> {
        let prefix = prefix.to_owned();
        Box::pin(async move {
            let key_prefix = self.inner.cfg.prefix.trim_matches('/').to_owned();
            let full_prefix = if key_prefix.is_empty() {
                prefix.clone()
            } else {
                format!("{key_prefix}/{prefix}")
            };
            let bucket_path = format!("{}/{}", self.inner.base_path, self.inner.cfg.bucket);
            let mut out = Vec::new();
            let mut token: Option<String> = None;
            loop {
                let mut q = vec![
                    ("list-type".to_owned(), "2".to_owned()),
                    ("prefix".to_owned(), full_prefix.clone()),
                ];
                if let Some(t) = &token {
                    q.push(("continuation-token".into(), t.clone()));
                }
                let r = self.call("GET", &bucket_path, Query(q), vec![], vec![]).await?;
                let r = Self::check(r, "ListObjectsV2")?;
                let xml = String::from_utf8_lossy(&r.body).into_owned();
                for block in xml.split("<Contents>").skip(1) {
                    let block = block.split("</Contents>").next().unwrap_or("");
                    let (Some(k), Some(size)) = (tag(block, "Key"), tag(block, "Size")) else {
                        continue;
                    };
                    let key = if key_prefix.is_empty() {
                        k
                    } else {
                        k.strip_prefix(&format!("{key_prefix}/")).unwrap_or(&k).to_owned()
                    };
                    out.push(DriverObject {
                        key,
                        size: size.parse().unwrap_or(0),
                        modified_ms: tag(block, "LastModified").map_or(0, |m| parse_modified(&m)),
                    });
                }
                if tag(&xml, "IsTruncated").as_deref() == Some("true") {
                    token = tag(&xml, "NextContinuationToken");
                    if token.is_none() {
                        break;
                    }
                } else {
                    break;
                }
            }
            out.sort_by(|a, b| a.key.cmp(&b.key));
            Ok(out)
        })
    }

    fn ping(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            let bucket_path = format!("{}/{}", self.inner.base_path, self.inner.cfg.bucket);
            let r = self
                .call(
                    "GET",
                    &bucket_path,
                    Query(vec![("list-type".into(), "2".into()), ("max-keys".into(), "1".into())]),
                    vec![],
                    vec![],
                )
                .await?;
            Self::check(r, "ListObjectsV2").map(|_| ())
        })
    }
}
