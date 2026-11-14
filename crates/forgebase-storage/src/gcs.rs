//! A native Google Cloud Storage driver over the JSON API (ADR-0040).
//!
//! * **Auth**: a service-account JSON key (RS256 JWT exchanged for an OAuth
//!   access token, cached and refreshed shortly before it expires), the
//!   GCE / GKE metadata server (workload identity), an explicit access token,
//!   or none at all (emulators such as `fake-gcs-server`).
//! * **Writes** of at most one chunk (`part_size`, rounded up to a multiple
//!   of 256 KiB) are a single `uploadType=media` request; larger ones are a
//!   *resumable upload*, one chunk per request, so memory stays bounded by
//!   the chunk size. A failing stream cancels the upload session, so nothing
//!   is left under the key. [`GcsDriver::compose`] concatenates objects
//!   server side (up to 32 sources), the GCS counterpart of an S3 multipart
//!   completion.
//! * **Reads** page through the blob with ranged `GET`s of at most
//!   `read_chunk` bytes.
//! * **Retries** with exponential backoff on `408`, `429` and `5xx` (and on
//!   transport errors); a chunk that failed is re-sent from the offset the
//!   service reports.
//! * [`GcsDriver::signed_url`] makes V4 signed URLs (`GOOG4-RSA-SHA256`) with
//!   the service-account key.
//!
//! ForgeBase keeps its own versions, deduplication and signed URLs above the
//! driver (ADR-0018), so the driver stores plain, unversioned objects and
//! never reads or writes object generations.

use std::sync::Arc;

use axum::body::Bytes;
use forgebase_auth::social::{HttpClient, HttpRequest, HttpResponse};
use futures_util::StreamExt;
use serde_json::{Value as Json, json};

use crate::cloud::{
    Clock, RetryPolicy, RsaSigner, Token, TokenCache, b64url, clean_prefix, error_summary, join_key, parse_rfc3339_ms,
    parse_token, system_clock, with_retry,
};
use crate::driver::{BoxFuture, ByteStream, DriverObject, StorageDriver};
use crate::error::{Result, StorageError};
use crate::sigv4::{encode, stamps};
use crate::util::{hex, sha256_hex};

/// The OAuth scope of object reads and writes.
pub const GCS_SCOPE: &str = "https://www.googleapis.com/auth/devstorage.read_write";
/// Default JSON API endpoint.
pub const GCS_ENDPOINT: &str = "https://storage.googleapis.com";
/// Default metadata server (GCE, GKE workload identity).
pub const GCS_METADATA_ENDPOINT: &str = "http://metadata.google.internal";
const DEFAULT_TOKEN_URI: &str = "https://oauth2.googleapis.com/token";
/// Resumable chunks are multiples of this (a GCS rule).
const CHUNK_UNIT: usize = 256 * 1024;
/// GCS refuses V4 signed URLs valid for more than seven days.
const MAX_SIGNED_SECS: u64 = 604_800;

/// A service-account key (the fields of Google's JSON key file that matter).
#[derive(Clone)]
pub struct ServiceAccount {
    /// `client_email`.
    pub client_email: String,
    /// `private_key`: PKCS#8 PEM.
    pub private_key: String,
    /// `private_key_id`, sent as the JWT `kid`.
    pub private_key_id: Option<String>,
    /// `token_uri`: where the JWT is exchanged.
    pub token_uri: String,
}

impl std::fmt::Debug for ServiceAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceAccount")
            .field("client_email", &self.client_email)
            .finish_non_exhaustive()
    }
}

impl ServiceAccount {
    /// Parses the contents of a service-account JSON key file.
    pub fn from_json(text: &str) -> Result<Self> {
        let v: Json = serde_json::from_str(text)
            .map_err(|_| StorageError::invalid("invalid_gcs_key", "the service-account key is not JSON"))?;
        let field = |name: &str| v.get(name).and_then(Json::as_str).map(str::to_owned);
        let client_email = field("client_email").filter(|s| !s.is_empty()).ok_or_else(|| {
            StorageError::invalid("invalid_gcs_key", "the service-account key has no client_email")
        })?;
        let private_key = field("private_key")
            .filter(|s| !s.is_empty())
            .ok_or_else(|| StorageError::invalid("invalid_gcs_key", "the service-account key has no private_key"))?;
        Ok(Self {
            client_email,
            private_key,
            private_key_id: field("private_key_id").filter(|s| !s.is_empty()),
            token_uri: field("token_uri")
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| DEFAULT_TOKEN_URI.to_owned()),
        })
    }
}

/// How the driver authenticates.
#[derive(Clone)]
pub enum GcsAuth {
    /// A service-account key: JWT bearer grant, token cached and refreshed.
    ServiceAccount(ServiceAccount),
    /// The metadata server of GCE / GKE (workload identity).
    Metadata {
        /// Base URL, normally [`GCS_METADATA_ENDPOINT`].
        endpoint: String,
    },
    /// A fixed access token (tests, short-lived tooling).
    AccessToken(String),
    /// No credentials (emulators).
    None,
}

impl std::fmt::Debug for GcsAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ServiceAccount(sa) => write!(f, "ServiceAccount({})", sa.client_email),
            Self::Metadata { endpoint } => write!(f, "Metadata({endpoint})"),
            Self::AccessToken(_) => f.write_str("AccessToken(..)"),
            Self::None => f.write_str("None"),
        }
    }
}

/// Settings of a GCS store.
#[derive(Clone)]
pub struct GcsConfig {
    /// JSON API endpoint ([`GCS_ENDPOINT`], or an emulator).
    pub endpoint: String,
    /// Bucket holding the blobs.
    pub bucket: String,
    /// Object name prefix inside the bucket (no leading slash).
    pub prefix: String,
    /// Credentials.
    pub auth: GcsAuth,
    /// Upload chunk size (rounded up to a multiple of 256 KiB).
    pub part_size: usize,
    /// Size of one ranged read.
    pub read_chunk: usize,
    /// Retry behaviour.
    pub retry: RetryPolicy,
}

impl std::fmt::Debug for GcsConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GcsConfig")
            .field("endpoint", &self.endpoint)
            .field("bucket", &self.bucket)
            .field("prefix", &self.prefix)
            .field("auth", &self.auth)
            .finish_non_exhaustive()
    }
}

impl GcsConfig {
    /// Defaults for everything but the bucket and credentials.
    pub fn new(bucket: impl Into<String>, auth: GcsAuth) -> Self {
        Self {
            endpoint: GCS_ENDPOINT.into(),
            bucket: bucket.into(),
            prefix: String::new(),
            auth,
            part_size: 8 * 1024 * 1024,
            read_chunk: 4 * 1024 * 1024,
            retry: RetryPolicy::default(),
        }
    }
}

/// The GCS driver.
#[derive(Clone)]
pub struct GcsDriver {
    inner: Arc<Inner>,
}

struct Inner {
    cfg: GcsConfig,
    client: Arc<dyn HttpClient>,
    base: String,
    host: String,
    prefix: String,
    chunk: usize,
    signer: Option<RsaSigner>,
    tokens: TokenCache,
    clock: Clock,
}

impl std::fmt::Debug for GcsDriver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GcsDriver")
            .field("cfg", &self.inner.cfg)
            .finish_non_exhaustive()
    }
}

/// One listing page.
struct ListPage {
    objects: Vec<DriverObject>,
    prefixes: Vec<String>,
}

/// What an upload chunk request left behind.
enum Chunk {
    /// The upload is not complete; this many bytes are persisted in all.
    Persisted(u64),
    /// The object exists.
    Done,
}

fn bad(what: &str, r: &HttpResponse) -> StorageError {
    StorageError::Driver(format!("GCS {what} returned {}: {}", r.status, error_summary(&r.body)))
}

impl GcsDriver {
    /// A driver talking to `cfg.endpoint` through `client`.
    pub fn new(cfg: GcsConfig, client: Arc<dyn HttpClient>) -> Result<Self> {
        Self::with_clock(cfg, client, system_clock())
    }

    /// Like [`GcsDriver::new`] with an injectable clock (epoch seconds).
    pub fn with_clock(cfg: GcsConfig, client: Arc<dyn HttpClient>, clock: Clock) -> Result<Self> {
        let url = url::Url::parse(&cfg.endpoint)
            .map_err(|_| StorageError::invalid("invalid_gcs_endpoint", "the GCS endpoint is not a URL"))?;
        if url.scheme() != "http" && url.scheme() != "https" {
            return Err(StorageError::invalid(
                "invalid_gcs_endpoint",
                "the GCS endpoint must be http or https",
            ));
        }
        let host = match (url.host_str(), url.port()) {
            (Some(h), Some(p)) => format!("{h}:{p}"),
            (Some(h), None) => h.to_owned(),
            _ => {
                return Err(StorageError::invalid(
                    "invalid_gcs_endpoint",
                    "the GCS endpoint has no host",
                ));
            }
        };
        if cfg.bucket.trim().is_empty() {
            return Err(StorageError::invalid("invalid_gcs_bucket", "the GCS bucket is empty"));
        }
        let signer = match &cfg.auth {
            GcsAuth::ServiceAccount(sa) => Some(RsaSigner::from_pem(&sa.private_key)?),
            _ => None,
        };
        let chunk = cfg.part_size.max(1).div_ceil(CHUNK_UNIT) * CHUNK_UNIT;
        let base = format!("{}://{host}{}", url.scheme(), url.path().trim_end_matches('/'));
        let prefix = clean_prefix(&cfg.prefix);
        Ok(Self {
            inner: Arc::new(Inner {
                cfg,
                client,
                base,
                host,
                prefix,
                chunk,
                signer,
                tokens: TokenCache::default(),
                clock,
            }),
        })
    }

    fn full_key(&self, key: &str) -> String {
        join_key(&self.inner.prefix, key)
    }

    fn bucket_url(&self) -> String {
        format!("{}/storage/v1/b/{}", self.inner.base, encode(&self.inner.cfg.bucket, false))
    }

    fn object_url(&self, key: &str) -> String {
        format!("{}/o/{}", self.bucket_url(), encode(&self.full_key(key), false))
    }

    fn upload_url(&self, kind: &str, key: &str) -> String {
        format!(
            "{}/upload/storage/v1/b/{}/o?uploadType={kind}&name={}",
            self.inner.base,
            encode(&self.inner.cfg.bucket, false),
            encode(&self.full_key(key), false)
        )
    }

    fn now(&self) -> i64 {
        (self.inner.clock)()
    }

    /// A bearer token, or `None` for unauthenticated stores.
    async fn token(&self) -> Result<Option<String>> {
        let i = &self.inner;
        let now = self.now();
        match &i.cfg.auth {
            GcsAuth::None => Ok(None),
            GcsAuth::AccessToken(t) => Ok(Some(t.clone())),
            GcsAuth::ServiceAccount(sa) => i
                .tokens
                .get(now, || self.exchange_jwt(sa, now))
                .await
                .map(Some),
            GcsAuth::Metadata { endpoint } => i
                .tokens
                .get(now, || self.metadata_token(endpoint, now))
                .await
                .map(Some),
        }
    }

    /// The JWT bearer grant of a service-account key.
    async fn exchange_jwt(&self, sa: &ServiceAccount, now: i64) -> Result<Token> {
        let signer = self
            .inner
            .signer
            .as_ref()
            .ok_or_else(|| StorageError::Driver("no service-account signer".into()))?;
        let mut header = json!({ "alg": "RS256", "typ": "JWT" });
        if let Some(kid) = &sa.private_key_id {
            header["kid"] = json!(kid);
        }
        let claims = json!({
            "iss": sa.client_email,
            "scope": GCS_SCOPE,
            "aud": sa.token_uri,
            "iat": now,
            "exp": now + 3600,
        });
        let input = format!("{}.{}", b64url(header.to_string().as_bytes()), b64url(claims.to_string().as_bytes()));
        let jwt = format!("{input}.{}", b64url(&signer.sign(input.as_bytes())?));
        let req = HttpRequest::post_form(
            &sa.token_uri,
            &[("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"), ("assertion", &jwt)],
        );
        let resp = with_retry(&self.inner.cfg.retry, || async {
            self.inner
                .client
                .send(req.clone())
                .await
                .map_err(|e| StorageError::Driver(format!("GCS token request failed: {e}")))
        })
        .await?;
        parse_token(&resp, "the Google token endpoint", now)
    }

    /// A token from the metadata server.
    async fn metadata_token(&self, endpoint: &str, now: i64) -> Result<Token> {
        let url = format!(
            "{}/computeMetadata/v1/instance/service-accounts/default/token",
            endpoint.trim_end_matches('/')
        );
        let resp = with_retry(&self.inner.cfg.retry, || async {
            self.inner
                .client
                .send(HttpRequest::get(&url).header("metadata-flavor", "Google"))
                .await
                .map_err(|e| StorageError::Driver(format!("GCS metadata server request failed: {e}")))
        })
        .await?;
        parse_token(&resp, "the GCE metadata server", now)
    }

    /// One authorised request with retries; a `401` drops the cached token
    /// and tries once more with a fresh one.
    async fn call(
        &self,
        method: &'static str,
        url: &str,
        headers: &[(&str, String)],
        body: Vec<u8>,
    ) -> Result<HttpResponse> {
        let mut renewed = false;
        loop {
            let token = self.token().await?;
            let resp = with_retry(&self.inner.cfg.retry, || async {
                self.send_once(method, url, headers, &body, token.as_deref()).await
            })
            .await?;
            if resp.status == 401 && !renewed && token.is_some() {
                renewed = true;
                self.inner.tokens.invalidate().await;
                continue;
            }
            return Ok(resp);
        }
    }

    async fn send_once(
        &self,
        method: &'static str,
        url: &str,
        headers: &[(&str, String)],
        body: &[u8],
        token: Option<&str>,
    ) -> Result<HttpResponse> {
        let mut req = HttpRequest {
            method,
            url: url.to_owned(),
            headers: Vec::new(),
            body: body.to_vec(),
        };
        if let Some(t) = token {
            req = req.header("authorization", format!("Bearer {t}"));
        }
        for (k, v) in headers {
            req = req.header(k, v.clone());
        }
        self.inner
            .client
            .send(req)
            .await
            .map_err(|e| StorageError::Driver(format!("GCS request failed: {e}")))
    }

    async fn put_media(&self, key: &str, body: Vec<u8>) -> Result<()> {
        let r = self
            .call(
                "POST",
                &self.upload_url("media", key),
                &[("content-type", "application/octet-stream".into())],
                body,
            )
            .await?;
        if r.status == 200 { Ok(()) } else { Err(bad("object upload", &r)) }
    }

    /// Starts a resumable session and returns its URI.
    async fn start_resumable(&self, key: &str) -> Result<String> {
        let meta = json!({ "name": self.full_key(key) }).to_string().into_bytes();
        let r = self
            .call(
                "POST",
                &self.upload_url("resumable", key),
                &[
                    ("content-type", "application/json; charset=UTF-8".into()),
                    ("x-upload-content-type", "application/octet-stream".into()),
                ],
                meta,
            )
            .await?;
        if r.status != 200 {
            return Err(bad("resumable upload start", &r));
        }
        r.header("location")
            .map(str::to_owned)
            .ok_or_else(|| StorageError::Driver("GCS resumable upload start returned no Location".into()))
    }

    /// Bytes persisted according to a `308` answer's `Range: bytes=0-N`.
    fn persisted(r: &HttpResponse) -> u64 {
        r.header("range")
            .and_then(|v| v.rsplit('-').next())
            .and_then(|n| n.trim().parse::<u64>().ok())
            .map_or(0, |last| last + 1)
    }

    /// Asks the session how much it has.
    async fn session_status(&self, session: &str, total: Option<u64>) -> Result<Chunk> {
        let range = format!("bytes */{}", total.map_or("*".to_owned(), |t| t.to_string()));
        let r = self
            .call("PUT", session, &[("content-range", range)], Vec::new())
            .await?;
        match r.status {
            200 | 201 => Ok(Chunk::Done),
            308 => Ok(Chunk::Persisted(Self::persisted(&r))),
            _ => Err(bad("resumable upload status", &r)),
        }
    }

    /// Sends bytes `offset..offset+data.len()`; `total` is known for the final
    /// chunk. A chunk that fails transiently is followed by a status query,
    /// and the answer says what to send next.
    async fn put_chunk(&self, session: &str, offset: u64, data: &[u8], total: Option<u64>) -> Result<Chunk> {
        let range = if data.is_empty() {
            format!("bytes */{}", total.unwrap_or(offset))
        } else {
            format!(
                "bytes {offset}-{}/{}",
                offset + data.len() as u64 - 1,
                total.map_or("*".to_owned(), |t| t.to_string())
            )
        };
        let policy = self.inner.cfg.retry;
        let token = self.token().await?;
        let sent = self
            .send_once("PUT", session, &[("content-range", range)], data, token.as_deref())
            .await;
        match sent {
            Ok(r) if matches!(r.status, 200 | 201) => Ok(Chunk::Done),
            Ok(r) if r.status == 308 => Ok(Chunk::Persisted(Self::persisted(&r))),
            Ok(r) if !crate::cloud::retryable_status(r.status) => Err(bad("resumable upload chunk", &r)),
            other => {
                if policy.max_attempts <= 1 {
                    return Err(match other {
                        Ok(r) => bad("resumable upload chunk", &r),
                        Err(e) => e,
                    });
                }
                tokio::time::sleep(policy.delay(1, other.as_ref().ok().and_then(crate::cloud::retry_after))).await;
                // What did the service keep? (This call retries by itself.)
                self.session_status(session, total).await
            }
        }
    }

    async fn upload_resumable(&self, session: &str, first: Vec<u8>, body: &mut ByteStream) -> Result<u64> {
        let chunk = self.inner.chunk;
        let mut buf = first;
        let mut offset = 0u64;
        let mut done = false;
        let mut stalled = 0u32;
        loop {
            while !done && buf.len() <= chunk {
                match body.next().await {
                    Some(c) => buf.extend_from_slice(&c?),
                    None => done = true,
                }
            }
            let is_final = done && buf.len() <= chunk;
            let (send_len, total) = if is_final {
                (buf.len(), Some(offset + buf.len() as u64))
            } else {
                (chunk, None)
            };
            let outcome = self.put_chunk(session, offset, &buf[..send_len], total).await?;
            match outcome {
                Chunk::Done if is_final => return Ok(offset + send_len as u64),
                Chunk::Done => {
                    return Err(StorageError::Driver(
                        "GCS completed a resumable upload before the last chunk".into(),
                    ));
                }
                Chunk::Persisted(p) => {
                    let end = offset + send_len as u64;
                    if p < offset || p > end {
                        return Err(StorageError::Driver(format!(
                            "GCS reports {p} bytes of a resumable upload at offset {offset}"
                        )));
                    }
                    let kept = usize::try_from(p - offset).unwrap_or(0);
                    stalled = if kept == 0 { stalled + 1 } else { 0 };
                    if stalled > self.inner.cfg.retry.max_attempts.max(1) {
                        return Err(StorageError::Driver("GCS resumable upload makes no progress".into()));
                    }
                    buf.drain(..kept);
                    offset = p;
                }
            }
        }
    }

    async fn resumable(&self, key: &str, first: Vec<u8>, mut body: ByteStream) -> Result<u64> {
        let session = self.start_resumable(key).await?;
        match self.upload_resumable(&session, first, &mut body).await {
            Ok(n) => Ok(n),
            Err(e) => {
                // 499 is the answer to a cancelled session; nothing to do on failure.
                let _ = self.call("DELETE", &session, &[], Vec::new()).await;
                Err(e)
            }
        }
    }

    /// Concatenates up to 32 objects into `to` on the server (the GCS
    /// counterpart of completing a multipart upload). Keys are driver keys.
    pub async fn compose(&self, sources: &[&str], to: &str) -> Result<()> {
        if sources.is_empty() || sources.len() > 32 {
            return Err(StorageError::invalid(
                "invalid_compose",
                "GCS composes between 1 and 32 objects at a time",
            ));
        }
        let body = json!({
            "sourceObjects": sources.iter().map(|s| json!({ "name": self.full_key(s) })).collect::<Vec<_>>(),
            "destination": { "contentType": "application/octet-stream" },
        })
        .to_string()
        .into_bytes();
        let r = self
            .call(
                "POST",
                &format!("{}/compose", self.object_url(to)),
                &[("content-type", "application/json".into())],
                body,
            )
            .await?;
        match r.status {
            200 => Ok(()),
            404 => Err(StorageError::NotFound("stored blob".into())),
            _ => Err(bad("compose", &r)),
        }
    }

    async fn list_page(&self, prefix: &str, delimiter: Option<&str>, token: Option<&str>) -> Result<(ListPage, Option<String>)> {
        let mut url = format!(
            "{}/o?maxResults=1000&prefix={}&fields={}",
            self.bucket_url(),
            encode(&self.full_key(prefix), false),
            encode("items(name,size,updated),prefixes,nextPageToken", false)
        );
        if let Some(d) = delimiter {
            url.push_str(&format!("&delimiter={}", encode(d, false)));
        }
        if let Some(t) = token {
            url.push_str(&format!("&pageToken={}", encode(t, false)));
        }
        let r = self.call("GET", &url, &[], Vec::new()).await?;
        if r.status != 200 {
            return Err(bad("list", &r));
        }
        let v: Json = serde_json::from_slice(&r.body)
            .map_err(|_| StorageError::Driver("GCS list returned a body that is not JSON".into()))?;
        let strip = |name: &str| -> String {
            if self.inner.prefix.is_empty() {
                name.to_owned()
            } else {
                name.strip_prefix(&format!("{}/", self.inner.prefix)).unwrap_or(name).to_owned()
            }
        };
        let mut page = ListPage {
            objects: Vec::new(),
            prefixes: Vec::new(),
        };
        for it in v.get("items").and_then(Json::as_array).into_iter().flatten() {
            let Some(name) = it.get("name").and_then(Json::as_str) else { continue };
            page.objects.push(DriverObject {
                key: strip(name),
                size: json_u64(it.get("size")),
                modified_ms: it.get("updated").and_then(Json::as_str).map_or(0, parse_rfc3339_ms),
            });
        }
        for p in v.get("prefixes").and_then(Json::as_array).into_iter().flatten() {
            if let Some(p) = p.as_str() {
                page.prefixes.push(strip(p));
            }
        }
        let next = v.get("nextPageToken").and_then(Json::as_str).map(str::to_owned);
        Ok((page, next))
    }

    /// Lists the blobs under `prefix` and, with a delimiter, the "folders"
    /// directly below it (`prefixes`, each ending in the delimiter).
    pub async fn list_with_delimiter(&self, prefix: &str, delimiter: &str) -> Result<(Vec<DriverObject>, Vec<String>)> {
        let (mut objects, mut prefixes) = (Vec::new(), Vec::new());
        let mut token = None;
        loop {
            let (page, next) = self.list_page(prefix, Some(delimiter), token.as_deref()).await?;
            objects.extend(page.objects);
            prefixes.extend(page.prefixes);
            match next {
                Some(t) => token = Some(t),
                None => break,
            }
        }
        objects.sort_by(|a, b| a.key.cmp(&b.key));
        prefixes.sort();
        Ok((objects, prefixes))
    }

    /// A V4 signed URL (`GOOG4-RSA-SHA256`) for `method` on `key`, valid for
    /// `expires_secs` (at most seven days). Needs a service-account key;
    /// workload identity and bare tokens cannot sign.
    pub fn signed_url(&self, method: &str, key: &str, expires_secs: u64) -> Result<String> {
        let (GcsAuth::ServiceAccount(sa), Some(signer)) = (&self.inner.cfg.auth, &self.inner.signer) else {
            return Err(StorageError::invalid(
                "gcs_signing_unavailable",
                "GCS signed URLs need a service-account key",
            ));
        };
        if !(1..=MAX_SIGNED_SECS).contains(&expires_secs) {
            return Err(StorageError::invalid(
                "invalid_expiry",
                "GCS signed URLs are valid for 1 second to 7 days",
            ));
        }
        v4_signed_url(
            signer,
            &sa.client_email,
            &self.inner.base,
            &self.inner.host,
            &self.inner.cfg.bucket,
            &self.full_key(key),
            method,
            expires_secs,
            self.now(),
        )
    }
}

fn json_u64(v: Option<&Json>) -> u64 {
    v.and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
        .unwrap_or(0)
}

/// Builds a V4 signed URL; split out so a published test vector can drive it.
#[allow(clippy::too_many_arguments)]
pub fn v4_signed_url(
    signer: &RsaSigner,
    client_email: &str,
    base: &str,
    host: &str,
    bucket: &str,
    object: &str,
    method: &str,
    expires_secs: u64,
    now: i64,
) -> Result<String> {
    let (date, stamp) = stamps(now);
    let scope = format!("{date}/auto/storage/goog4_request");
    let mut query = [
        ("X-Goog-Algorithm", "GOOG4-RSA-SHA256".to_owned()),
        ("X-Goog-Credential", format!("{client_email}/{scope}")),
        ("X-Goog-Date", stamp.clone()),
        ("X-Goog-Expires", expires_secs.to_string()),
        ("X-Goog-SignedHeaders", "host".to_owned()),
    ];
    query.sort_by(|a, b| a.0.cmp(b.0));
    let canonical_query = query
        .iter()
        .map(|(k, v)| format!("{}={}", encode(k, false), encode(v, false)))
        .collect::<Vec<_>>()
        .join("&");
    let path = format!("/{bucket}/{}", object);
    let encoded_path = encode(&path, true);
    let canonical = format!("{method}\n{encoded_path}\n{canonical_query}\nhost:{host}\n\nhost\nUNSIGNED-PAYLOAD");
    let to_sign = format!("GOOG4-RSA-SHA256\n{stamp}\n{scope}\n{}", sha256_hex(canonical.as_bytes()));
    let signature = hex(&signer.sign(to_sign.as_bytes())?);
    Ok(format!("{base}{encoded_path}?{canonical_query}&X-Goog-Signature={signature}"))
}

impl StorageDriver for GcsDriver {
    fn name(&self) -> &'static str {
        "gcs"
    }

    fn put(&self, key: &str, mut body: ByteStream) -> BoxFuture<'_, Result<u64>> {
        let key = key.to_owned();
        Box::pin(async move {
            let chunk = self.inner.chunk;
            let mut buf: Vec<u8> = Vec::new();
            while buf.len() <= chunk {
                match body.next().await {
                    Some(c) => buf.extend_from_slice(&c?),
                    None => {
                        let n = buf.len() as u64;
                        self.put_media(&key, buf).await?;
                        return Ok(n);
                    }
                }
            }
            self.resumable(&key, buf, body).await
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
                let url = format!("{}?alt=media", this.object_url(&key));
                let r = this
                    .call("GET", &url, &[("range", format!("bytes={pos}-{}", to - 1))], Vec::new())
                    .await;
                match r {
                    Ok(resp) if matches!(resp.status, 200 | 206) && resp.body.len() as u64 == to - pos => {
                        Some((Ok(Bytes::from(resp.body)), (this, key, to)))
                    }
                    Ok(resp) if matches!(resp.status, 200 | 206) => Some((
                        Err(std::io::Error::other("GCS returned an unexpected number of bytes")),
                        (this, key, end),
                    )),
                    Ok(resp) => Some((Err(std::io::Error::other(bad("object read", &resp).to_string())), (this, key, end))),
                    Err(e) => Some((Err(std::io::Error::other(e.to_string())), (this, key, end))),
                }
            });
            Ok(Box::pin(stream) as ByteStream)
        })
    }

    fn head(&self, key: &str) -> BoxFuture<'_, Result<Option<DriverObject>>> {
        let key = key.to_owned();
        Box::pin(async move {
            let url = format!("{}?fields=name,size,updated", self.object_url(&key));
            let r = self.call("GET", &url, &[], Vec::new()).await?;
            if r.status == 404 {
                return Ok(None);
            }
            if r.status != 200 {
                return Err(bad("object metadata", &r));
            }
            let v: Json = serde_json::from_slice(&r.body)
                .map_err(|_| StorageError::Driver("GCS object metadata is not JSON".into()))?;
            Ok(Some(DriverObject {
                key,
                size: json_u64(v.get("size")),
                modified_ms: v.get("updated").and_then(Json::as_str).map_or(0, parse_rfc3339_ms),
            }))
        })
    }

    fn delete(&self, key: &str) -> BoxFuture<'_, Result<()>> {
        let key = key.to_owned();
        Box::pin(async move {
            let r = self.call("DELETE", &self.object_url(&key), &[], Vec::new()).await?;
            if matches!(r.status, 200 | 204 | 404) { Ok(()) } else { Err(bad("object delete", &r)) }
        })
    }

    fn copy(&self, from: &str, to: &str) -> BoxFuture<'_, Result<()>> {
        let (from, to) = (from.to_owned(), to.to_owned());
        Box::pin(async move {
            let bucket = encode(&self.inner.cfg.bucket, false);
            let base = format!(
                "{}/rewriteTo/b/{bucket}/o/{}",
                self.object_url(&from),
                encode(&self.full_key(&to), false)
            );
            let mut token: Option<String> = None;
            // A rewrite of a large object takes several calls.
            for _ in 0..100_000 {
                let url = match &token {
                    Some(t) => format!("{base}?rewriteToken={}", encode(t, false)),
                    None => base.clone(),
                };
                let r = self.call("POST", &url, &[("content-length", "0".into())], Vec::new()).await?;
                match r.status {
                    404 => return Err(StorageError::NotFound("stored blob".into())),
                    200 => {}
                    _ => return Err(bad("object rewrite", &r)),
                }
                let v: Json = serde_json::from_slice(&r.body)
                    .map_err(|_| StorageError::Driver("GCS rewrite returned a body that is not JSON".into()))?;
                if v.get("done").and_then(Json::as_bool).unwrap_or(false) {
                    return Ok(());
                }
                token = Some(
                    v.get("rewriteToken")
                        .and_then(Json::as_str)
                        .ok_or_else(|| StorageError::Driver("GCS rewrite returned no token".into()))?
                        .to_owned(),
                );
            }
            Err(StorageError::Driver("GCS rewrite did not finish".into()))
        })
    }

    fn list(&self, prefix: &str) -> BoxFuture<'_, Result<Vec<DriverObject>>> {
        let prefix = prefix.to_owned();
        Box::pin(async move {
            let mut out = Vec::new();
            let mut token: Option<String> = None;
            loop {
                let (page, next) = self.list_page(&prefix, None, token.as_deref()).await?;
                out.extend(page.objects);
                match next {
                    Some(t) => token = Some(t),
                    None => break,
                }
            }
            out.sort_by(|a, b| a.key.cmp(&b.key));
            Ok(out)
        })
    }

    fn ping(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            let url = format!("{}/o?maxResults=1&fields=nextPageToken", self.bucket_url());
            let r = self.call("GET", &url, &[], Vec::new()).await?;
            if r.status == 200 { Ok(()) } else { Err(bad("list", &r)) }
        })
    }
}
