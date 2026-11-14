//! A native Azure Blob Storage driver over the REST API (ADR-0040).
//!
//! * **Auth**: the storage account's shared key (`SharedKey` request
//!   signing, HMAC-SHA256), a SAS token, Microsoft Entra ID client
//!   credentials, or a managed identity (IMDS, or the App Service / Container
//!   Apps identity endpoint); Entra tokens are cached and refreshed shortly
//!   before they expire.
//! * **Block blobs**. A write of at most `part_size` bytes is one `Put Blob`;
//!   a larger one is `Put Block` per part followed by `Put Block List`, so
//!   memory stays bounded by the part size and a failing stream commits
//!   nothing (uncommitted blocks are never visible and Azure discards them).
//! * **Reads** page through the blob with ranged `GET`s (`x-ms-range`) of at
//!   most `read_chunk` bytes.
//! * **Retries** with exponential backoff on `408`, `429`, `5xx` (`ServerBusy`
//!   is a `503`) and transport errors; every attempt is signed afresh.
//! * Azurite and other emulators: give the account URL as the endpoint
//!   (`http://127.0.0.1:10000/devstoreaccount1`).
//! * [`AzureDriver::signed_url`] makes service SAS URLs with the account key.
//!   User-delegation SAS (Entra identities) is not implemented.
//!
//! ForgeBase keeps its own versions, deduplication and signed URLs above the
//! driver (ADR-0018), so the driver stores plain block blobs.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use forgebase_auth::social::{HttpClient, HttpRequest, HttpResponse};
use futures_util::StreamExt;
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

use crate::cloud::{
    Clock, RetryPolicy, Token, TokenCache, b64, b64_decode, clean_prefix, error_summary, http_date, iso_z, join_key,
    parse_http_date_ms, parse_token, system_clock, with_retry, xml_blocks, xml_escape, xml_tag,
};
use crate::driver::{BoxFuture, ByteStream, DriverObject, StorageDriver};
use crate::error::{Result, StorageError};
use crate::sigv4::encode;

/// The REST API version every request asks for.
pub const AZURE_API_VERSION: &str = "2021-08-06";
/// The OAuth resource of Blob Storage.
pub const AZURE_RESOURCE: &str = "https://storage.azure.com/";
/// Default Microsoft Entra authority.
pub const AZURE_AUTHORITY: &str = "https://login.microsoftonline.com";
/// Default IMDS token endpoint (Azure VMs, AKS).
pub const AZURE_IMDS_ENDPOINT: &str = "http://169.254.169.254/metadata/identity/oauth2/token";

type HmacSha256 = Hmac<Sha256>;

/// How the driver authenticates.
#[derive(Clone)]
pub enum AzureAuth {
    /// The storage account key (base64, as the portal shows it).
    SharedKey(Vec<u8>),
    /// A SAS token (`sv=...&sig=...`, with or without the leading `?`).
    Sas(String),
    /// Microsoft Entra ID client credentials.
    ClientSecret {
        /// Directory (tenant) id.
        tenant: String,
        /// Application (client) id.
        client_id: String,
        /// Client secret.
        client_secret: String,
        /// Authority base URL, normally [`AZURE_AUTHORITY`].
        authority: String,
    },
    /// A managed identity.
    ManagedIdentity {
        /// Token endpoint: [`AZURE_IMDS_ENDPOINT`], or `IDENTITY_ENDPOINT`.
        endpoint: String,
        /// `IDENTITY_HEADER` of App Service / Container Apps (selects that
        /// protocol); `None` talks to IMDS.
        identity_header: Option<String>,
        /// Client id of a user-assigned identity.
        client_id: Option<String>,
    },
    /// A fixed bearer token (tests).
    AccessToken(String),
}

impl AzureAuth {
    /// Shared-key auth from the base64 account key.
    pub fn shared_key(account_key: &str) -> Result<Self> {
        b64_decode(account_key)
            .filter(|k| !k.is_empty())
            .map(Self::SharedKey)
            .ok_or_else(|| StorageError::invalid("invalid_azure_key", "the account key is not base64"))
    }
}

impl std::fmt::Debug for AzureAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::SharedKey(_) => "SharedKey(..)",
            Self::Sas(_) => "Sas(..)",
            Self::ClientSecret { .. } => "ClientSecret(..)",
            Self::ManagedIdentity { .. } => "ManagedIdentity",
            Self::AccessToken(_) => "AccessToken(..)",
        })
    }
}

/// Picks the credentials from environment variables (`env` looks one up;
/// empty values count as unset), in this order: the account key
/// (`FORGEBASE_STORAGE_AZURE_ACCOUNT_KEY`), a SAS token (`..._SAS_TOKEN`),
/// Microsoft Entra client credentials (`..._TENANT_ID`, `..._CLIENT_ID`,
/// `..._CLIENT_SECRET`), else the managed identity of the host: the App Service
/// / Container Apps endpoint (`IDENTITY_ENDPOINT`, `IDENTITY_HEADER`) or IMDS;
/// `..._CLIENT_ID` alone picks a user-assigned identity.
pub fn auth_from_env(env: &dyn Fn(&str) -> Option<String>) -> Result<AzureAuth> {
    if let Some(key) = env("FORGEBASE_STORAGE_AZURE_ACCOUNT_KEY") {
        return AzureAuth::shared_key(&key);
    }
    if let Some(sas) = env("FORGEBASE_STORAGE_AZURE_SAS_TOKEN") {
        return Ok(AzureAuth::Sas(sas));
    }
    if let (Some(tenant), Some(client_id), Some(client_secret)) = (
        env("FORGEBASE_STORAGE_AZURE_TENANT_ID"),
        env("FORGEBASE_STORAGE_AZURE_CLIENT_ID"),
        env("FORGEBASE_STORAGE_AZURE_CLIENT_SECRET"),
    ) {
        return Ok(AzureAuth::ClientSecret {
            tenant,
            client_id,
            client_secret,
            authority: AZURE_AUTHORITY.to_owned(),
        });
    }
    Ok(AzureAuth::ManagedIdentity {
        endpoint: env("IDENTITY_ENDPOINT").unwrap_or_else(|| AZURE_IMDS_ENDPOINT.to_owned()),
        identity_header: env("IDENTITY_HEADER"),
        client_id: env("FORGEBASE_STORAGE_AZURE_CLIENT_ID"),
    })
}

/// Settings of an Azure Blob store.
#[derive(Clone)]
pub struct AzureConfig {
    /// Storage account name.
    pub account: String,
    /// Container holding the blobs.
    pub container: String,
    /// Account URL. `None`: `https://<account>.blob.core.windows.net`. For
    /// Azurite: `http://127.0.0.1:10000/<account>`.
    pub endpoint: Option<String>,
    /// Blob name prefix inside the container (no leading slash).
    pub prefix: String,
    /// Credentials.
    pub auth: AzureAuth,
    /// Size of one block (and the threshold for block uploads).
    pub part_size: usize,
    /// Size of one ranged read.
    pub read_chunk: usize,
    /// Retry behaviour.
    pub retry: RetryPolicy,
}

impl std::fmt::Debug for AzureConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AzureConfig")
            .field("account", &self.account)
            .field("container", &self.container)
            .field("endpoint", &self.endpoint)
            .field("prefix", &self.prefix)
            .field("auth", &self.auth)
            .finish_non_exhaustive()
    }
}

impl AzureConfig {
    /// Defaults for everything but the account, container and credentials.
    pub fn new(account: impl Into<String>, container: impl Into<String>, auth: AzureAuth) -> Self {
        Self {
            account: account.into(),
            container: container.into(),
            endpoint: None,
            prefix: String::new(),
            auth,
            part_size: 8 * 1024 * 1024,
            read_chunk: 4 * 1024 * 1024,
            retry: RetryPolicy::default(),
        }
    }
}

/// The Azure Blob driver.
#[derive(Clone)]
pub struct AzureDriver {
    inner: Arc<Inner>,
}

struct Inner {
    cfg: AzureConfig,
    client: Arc<dyn HttpClient>,
    /// `scheme://host[:port]`.
    origin: String,
    /// Path of the account URL (empty, or `/<account>` for Azurite).
    base_path: String,
    prefix: String,
    tokens: TokenCache,
    clock: Clock,
}

impl std::fmt::Debug for AzureDriver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AzureDriver")
            .field("cfg", &self.inner.cfg)
            .finish_non_exhaustive()
    }
}

fn bad(what: &str, r: &HttpResponse) -> StorageError {
    // A HEAD answer has no body: the error code travels in a header.
    let detail = r
        .header("x-ms-error-code")
        .map_or_else(|| error_summary(&r.body), str::to_owned);
    StorageError::Driver(format!("Azure {what} returned {}: {detail}", r.status))
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    #[allow(clippy::expect_used)]
    let mut m = <HmacSha256 as KeyInit>::new_from_slice(key).expect("HMAC accepts any key length");
    m.update(data);
    m.finalize().into_bytes().to_vec()
}

/// The `SharedKey` string to sign (Microsoft: *Authorize with Shared Key*,
/// version 2009-09-19 and later). `headers` are the request's headers with
/// lower-case names; `path` is the URL path as sent (encoded); `query` holds
/// the decoded parameters.
pub fn shared_key_string_to_sign(
    method: &str,
    account: &str,
    path: &str,
    query: &[(String, String)],
    headers: &[(String, String)],
    content_length: usize,
) -> String {
    let h = |name: &str| {
        headers
            .iter()
            .find(|(k, _)| k == name)
            .map_or("", |(_, v)| v.as_str())
            .trim()
            .to_owned()
    };
    let mut xms: Vec<(String, String)> = headers
        .iter()
        .filter(|(k, _)| k.starts_with("x-ms-"))
        .map(|(k, v)| (k.clone(), v.trim().to_owned()))
        .collect();
    xms.sort();
    let canonical_headers: String = xms.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let mut params: Vec<(String, Vec<String>)> = Vec::new();
    for (k, v) in query {
        let k = k.to_ascii_lowercase();
        match params.iter_mut().find(|(n, _)| *n == k) {
            Some((_, vs)) => vs.push(v.clone()),
            None => params.push((k, vec![v.clone()])),
        }
    }
    params.sort_by(|a, b| a.0.cmp(&b.0));
    let mut resource = format!("/{account}{path}");
    for (k, mut vs) in params {
        vs.sort();
        resource.push_str(&format!("\n{k}:{}", vs.join(",")));
    }
    let length = if content_length == 0 {
        String::new()
    } else {
        content_length.to_string()
    };
    format!(
        "{method}\n{}\n{}\n{length}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{canonical_headers}{resource}",
        h("content-encoding"),
        h("content-language"),
        h("content-md5"),
        h("content-type"),
        h("date"),
        h("if-modified-since"),
        h("if-match"),
        h("if-none-match"),
        h("if-unmodified-since"),
        h("range"),
    )
}

/// `SharedKey <account>:<signature>`.
pub fn shared_key_authorization(account: &str, key: &[u8], string_to_sign: &str) -> String {
    format!("SharedKey {account}:{}", b64(&hmac(key, string_to_sign.as_bytes())))
}

/// What a service SAS grants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SasPermission {
    /// `r`: read the blob.
    Read,
    /// `cw`: create or overwrite the blob.
    Write,
    /// `d`: delete the blob.
    Delete,
}

impl SasPermission {
    fn letters(self) -> &'static str {
        match self {
            Self::Read => "r",
            Self::Write => "cw",
            Self::Delete => "d",
        }
    }
}

/// What a service SAS is made for.
#[derive(Debug, Clone, Copy)]
pub struct SasParams<'a> {
    /// Storage account name.
    pub account: &'a str,
    /// Container name.
    pub container: &'a str,
    /// Blob name (prefix included).
    pub blob: &'a str,
    /// What the URL allows.
    pub permission: SasPermission,
    /// Start of validity, epoch seconds (`None`: immediately).
    pub start: Option<i64>,
    /// End of validity, epoch seconds.
    pub expiry: i64,
    /// Adds `spr=https`.
    pub https_only: bool,
}

/// The query string of a service SAS for one blob (version 2021-08-06
/// string-to-sign), signed with the account key.
pub fn service_sas(key: &[u8], p: &SasParams<'_>) -> String {
    let sp = p.permission.letters();
    let st = p.start.map(iso_z).unwrap_or_default();
    let se = iso_z(p.expiry);
    let spr = if p.https_only { "https" } else { "" };
    let resource = format!("/blob/{}/{}/{}", p.account, p.container, p.blob);
    // sp, st, se, canonicalized resource, si, sip, spr, sv, sr, snapshot time,
    // encryption scope, rscc, rscd, rsce, rscl, rsct.
    let to_sign = format!("{sp}\n{st}\n{se}\n{resource}\n\n\n{spr}\n{AZURE_API_VERSION}\nb\n\n\n\n\n\n\n");
    let sig = b64(&hmac(key, to_sign.as_bytes()));
    let mut q = vec![
        ("sv", AZURE_API_VERSION.to_owned()),
        ("sr", "b".to_owned()),
        ("sp", sp.to_owned()),
    ];
    if p.start.is_some() {
        q.push(("st", st));
    }
    q.push(("se", se));
    if p.https_only {
        q.push(("spr", "https".to_owned()));
    }
    q.push(("sig", sig));
    q.iter()
        .map(|(k, v)| format!("{k}={}", encode(v, false)))
        .collect::<Vec<_>>()
        .join("&")
}

impl AzureDriver {
    /// A driver talking to the account through `client`.
    pub fn new(cfg: AzureConfig, client: Arc<dyn HttpClient>) -> Result<Self> {
        Self::with_clock(cfg, client, system_clock())
    }

    /// Like [`AzureDriver::new`] with an injectable clock (epoch seconds).
    pub fn with_clock(cfg: AzureConfig, client: Arc<dyn HttpClient>, clock: Clock) -> Result<Self> {
        if cfg.account.trim().is_empty() || cfg.container.trim().is_empty() {
            return Err(StorageError::invalid(
                "invalid_azure_config",
                "the Azure account and container must be set",
            ));
        }
        let endpoint = cfg
            .endpoint
            .clone()
            .filter(|e| !e.trim().is_empty())
            .unwrap_or_else(|| format!("https://{}.blob.core.windows.net", cfg.account));
        let url = url::Url::parse(&endpoint)
            .map_err(|_| StorageError::invalid("invalid_azure_endpoint", "the Azure endpoint is not a URL"))?;
        if url.scheme() != "http" && url.scheme() != "https" {
            return Err(StorageError::invalid(
                "invalid_azure_endpoint",
                "the Azure endpoint must be http or https",
            ));
        }
        let host = match (url.host_str(), url.port()) {
            (Some(h), Some(p)) => format!("{h}:{p}"),
            (Some(h), None) => h.to_owned(),
            _ => {
                return Err(StorageError::invalid(
                    "invalid_azure_endpoint",
                    "the Azure endpoint has no host",
                ));
            }
        };
        let prefix = clean_prefix(&cfg.prefix);
        Ok(Self {
            inner: Arc::new(Inner {
                origin: format!("{}://{host}", url.scheme()),
                base_path: url.path().trim_end_matches('/').to_owned(),
                client,
                prefix,
                tokens: TokenCache::default(),
                clock,
                cfg,
            }),
        })
    }

    fn now(&self) -> i64 {
        (self.inner.clock)()
    }

    fn blob_name(&self, key: &str) -> String {
        join_key(&self.inner.prefix, key)
    }

    /// Path below the account URL: `/<container>/<blob>`.
    fn blob_path(&self, key: &str) -> String {
        format!("/{}/{}", self.inner.cfg.container, self.blob_name(key))
    }

    fn container_path(&self) -> String {
        format!("/{}", self.inner.cfg.container)
    }

    async fn bearer(&self) -> Result<Option<String>> {
        let now = self.now();
        match &self.inner.cfg.auth {
            AzureAuth::SharedKey(_) | AzureAuth::Sas(_) => Ok(None),
            AzureAuth::AccessToken(t) => Ok(Some(t.clone())),
            AzureAuth::ClientSecret {
                tenant,
                client_id,
                client_secret,
                authority,
            } => self
                .inner
                .tokens
                .get(now, || {
                    self.client_credentials(authority, tenant, client_id, client_secret, now)
                })
                .await
                .map(Some),
            AzureAuth::ManagedIdentity {
                endpoint,
                identity_header,
                client_id,
            } => self
                .inner
                .tokens
                .get(now, || {
                    self.managed_identity(endpoint, identity_header.as_deref(), client_id.as_deref(), now)
                })
                .await
                .map(Some),
        }
    }

    async fn client_credentials(
        &self,
        authority: &str,
        tenant: &str,
        client_id: &str,
        client_secret: &str,
        now: i64,
    ) -> Result<Token> {
        let url = format!("{}/{tenant}/oauth2/v2.0/token", authority.trim_end_matches('/'));
        let req = HttpRequest::post_form(
            url,
            &[
                ("grant_type", "client_credentials"),
                ("client_id", client_id),
                ("client_secret", client_secret),
                ("scope", "https://storage.azure.com/.default"),
            ],
        );
        let resp = with_retry(&self.inner.cfg.retry, || async {
            self.inner
                .client
                .send(req.clone())
                .await
                .map_err(|e| StorageError::Driver(format!("Azure token request failed: {e}")))
        })
        .await?;
        parse_token(&resp, "the Microsoft Entra token endpoint", now)
    }

    async fn managed_identity(
        &self,
        endpoint: &str,
        identity_header: Option<&str>,
        client_id: Option<&str>,
        now: i64,
    ) -> Result<Token> {
        let (api, id_param) = if identity_header.is_some() {
            ("2019-08-01", "client_id")
        } else {
            ("2018-02-01", "client_id")
        };
        let mut url = format!(
            "{}?api-version={api}&resource={}",
            endpoint.trim_end_matches('/'),
            encode(AZURE_RESOURCE, false)
        );
        if let Some(id) = client_id {
            url.push_str(&format!("&{id_param}={}", encode(id, false)));
        }
        let req = match identity_header {
            Some(h) => HttpRequest::get(&url).header("x-identity-header", h),
            None => HttpRequest::get(&url).header("metadata", "true"),
        };
        let resp = with_retry(&self.inner.cfg.retry, || async {
            self.inner
                .client
                .send(req.clone())
                .await
                .map_err(|e| StorageError::Driver(format!("Azure managed identity request failed: {e}")))
        })
        .await?;
        parse_token(&resp, "the Azure managed identity endpoint", now)
    }

    /// One authorised request with retries. `path` is below the account URL
    /// (not yet encoded); `query` holds decoded parameters.
    async fn call(
        &self,
        method: &'static str,
        path: &str,
        query: &[(&str, String)],
        headers: &[(&str, String)],
        body: Vec<u8>,
    ) -> Result<HttpResponse> {
        let mut renewed = false;
        loop {
            let token = self.bearer().await?;
            let resp = with_retry(&self.inner.cfg.retry, || async {
                self.send_once(method, path, query, headers, &body, token.as_deref())
                    .await
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
        path: &str,
        query: &[(&str, String)],
        headers: &[(&str, String)],
        body: &[u8],
        token: Option<&str>,
    ) -> Result<HttpResponse> {
        let i = &self.inner;
        let full_path = encode(&format!("{}{path}", i.base_path), true);
        let query: Vec<(String, String)> = query.iter().map(|(k, v)| ((*k).to_owned(), v.clone())).collect();
        let mut all: Vec<(String, String)> = headers
            .iter()
            .map(|(k, v)| (k.to_ascii_lowercase(), v.clone()))
            .collect();
        all.push(("x-ms-version".into(), AZURE_API_VERSION.into()));
        all.push(("x-ms-date".into(), http_date(self.now())));
        match &i.cfg.auth {
            AzureAuth::SharedKey(key) => {
                let sts = shared_key_string_to_sign(method, &i.cfg.account, &full_path, &query, &all, body.len());
                all.push((
                    "authorization".into(),
                    shared_key_authorization(&i.cfg.account, key, &sts),
                ));
            }
            _ => {
                if let Some(t) = token {
                    all.push(("authorization".into(), format!("Bearer {t}")));
                }
            }
        }
        let mut url = format!("{}{full_path}", i.origin);
        let mut q: Vec<String> = query
            .iter()
            .map(|(k, v)| format!("{}={}", encode(k, false), encode(v, false)))
            .collect();
        if let AzureAuth::Sas(sas) = &i.cfg.auth {
            q.push(sas.trim_start_matches('?').to_owned());
        }
        if !q.is_empty() {
            url.push('?');
            url.push_str(&q.join("&"));
        }
        let mut req = HttpRequest {
            method,
            url,
            headers: Vec::new(),
            body: body.to_vec(),
        };
        // The shared client defaults to `Accept: application/json`; Blob answers are XML or bytes.
        req = req.header("accept", "*/*");
        for (k, v) in all {
            req = req.header(&k, v);
        }
        i.client
            .send(req)
            .await
            .map_err(|e| StorageError::Driver(format!("Azure request failed: {e}")))
    }

    async fn put_blob(&self, key: &str, body: Vec<u8>) -> Result<()> {
        let r = self
            .call(
                "PUT",
                &self.blob_path(key),
                &[],
                &[
                    ("x-ms-blob-type", "BlockBlob".into()),
                    ("content-type", "application/octet-stream".into()),
                ],
                body,
            )
            .await?;
        if r.status == 201 {
            Ok(())
        } else {
            Err(bad("Put Blob", &r))
        }
    }

    async fn put_blocks(&self, key: &str, first: Vec<u8>, mut body: ByteStream) -> Result<u64> {
        let part = self.inner.cfg.part_size.max(1);
        let path = self.blob_path(key);
        let mut ids: Vec<String> = Vec::new();
        let mut total = 0u64;
        let mut buf = first;
        let mut done = false;
        loop {
            while !done && buf.len() <= part {
                match body.next().await {
                    Some(c) => buf.extend_from_slice(&c?),
                    None => done = true,
                }
            }
            let rest = if buf.len() > part {
                buf.split_off(part)
            } else {
                Vec::new()
            };
            if ids.len() >= 50_000 {
                return Err(StorageError::Driver(
                    "Azure block blobs have at most 50000 blocks".into(),
                ));
            }
            // Block ids must all have the same length.
            let id = b64(format!("{:08}", ids.len()).as_bytes());
            total += buf.len() as u64;
            let r = self
                .call(
                    "PUT",
                    &path,
                    &[("comp", "block".into()), ("blockid", id.clone())],
                    &[("content-type", "application/octet-stream".into())],
                    std::mem::take(&mut buf),
                )
                .await?;
            if r.status != 201 {
                return Err(bad("Put Block", &r));
            }
            ids.push(id);
            buf = rest;
            if done && buf.is_empty() {
                break;
            }
        }
        let mut xml = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?><BlockList>");
        for id in &ids {
            xml.push_str(&format!("<Latest>{}</Latest>", xml_escape(id)));
        }
        xml.push_str("</BlockList>");
        let r = self
            .call(
                "PUT",
                &path,
                &[("comp", "blocklist".into())],
                &[("content-type", "application/xml".into())],
                xml.into_bytes(),
            )
            .await?;
        if r.status == 201 {
            Ok(total)
        } else {
            Err(bad("Put Block List", &r))
        }
    }

    async fn list_page(
        &self,
        prefix: &str,
        delimiter: Option<&str>,
        marker: Option<&str>,
    ) -> Result<(Vec<DriverObject>, Vec<String>, Option<String>)> {
        let mut q: Vec<(&str, String)> = vec![
            ("restype", "container".into()),
            ("comp", "list".into()),
            ("prefix", self.blob_name(prefix)),
            ("maxresults", "1000".into()),
        ];
        if let Some(d) = delimiter {
            q.push(("delimiter", d.to_owned()));
        }
        if let Some(m) = marker {
            q.push(("marker", m.to_owned()));
        }
        let r = self.call("GET", &self.container_path(), &q, &[], Vec::new()).await?;
        if r.status != 200 {
            return Err(bad("List Blobs", &r));
        }
        let xml = String::from_utf8_lossy(&r.body).into_owned();
        let strip = |name: String| -> String {
            if self.inner.prefix.is_empty() {
                name
            } else {
                name.strip_prefix(&format!("{}/", self.inner.prefix))
                    .map_or(name.clone(), str::to_owned)
            }
        };
        let mut objects = Vec::new();
        for b in xml_blocks(&xml, "Blob") {
            let Some(name) = xml_tag(b, "Name") else { continue };
            objects.push(DriverObject {
                key: strip(name),
                size: xml_tag(b, "Content-Length").and_then(|v| v.parse().ok()).unwrap_or(0),
                modified_ms: xml_tag(b, "Last-Modified").map_or(0, |m| parse_http_date_ms(&m)),
            });
        }
        let prefixes = xml_blocks(&xml, "BlobPrefix")
            .into_iter()
            .filter_map(|b| xml_tag(b, "Name"))
            .map(strip)
            .collect();
        let next = xml_tag(&xml, "NextMarker").filter(|m| !m.is_empty());
        Ok((objects, prefixes, next))
    }

    /// Lists the blobs under `prefix` and, with a delimiter, the "folders"
    /// directly below it (each ending in the delimiter).
    pub async fn list_with_delimiter(&self, prefix: &str, delimiter: &str) -> Result<(Vec<DriverObject>, Vec<String>)> {
        let (mut objects, mut prefixes) = (Vec::new(), Vec::new());
        let mut marker: Option<String> = None;
        loop {
            let (o, p, next) = self.list_page(prefix, Some(delimiter), marker.as_deref()).await?;
            objects.extend(o);
            prefixes.extend(p);
            match next {
                Some(m) => marker = Some(m),
                None => break,
            }
        }
        objects.sort_by(|a, b| a.key.cmp(&b.key));
        prefixes.sort();
        Ok((objects, prefixes))
    }

    /// A service SAS URL for `key`, valid for `expires_secs` from now. Needs
    /// the account key: SAS tokens and Entra identities cannot sign (a
    /// user-delegation SAS is not implemented).
    pub fn signed_url(&self, permission: SasPermission, key: &str, expires_secs: u64) -> Result<String> {
        let AzureAuth::SharedKey(account_key) = &self.inner.cfg.auth else {
            return Err(StorageError::invalid(
                "azure_signing_unavailable",
                "Azure signed URLs need the storage account key",
            ));
        };
        if expires_secs == 0 {
            return Err(StorageError::invalid("invalid_expiry", "the expiry must be positive"));
        }
        let now = self.now();
        let secs = i64::try_from(expires_secs).unwrap_or(i64::MAX / 4);
        let blob = self.blob_name(key);
        let sas = service_sas(
            account_key,
            &SasParams {
                account: &self.inner.cfg.account,
                container: &self.inner.cfg.container,
                blob: &blob,
                permission,
                start: Some(now - 300),
                expiry: now.saturating_add(secs),
                https_only: self.inner.origin.starts_with("https://"),
            },
        );
        let path = encode(&format!("{}{}", self.inner.base_path, self.blob_path(key)), true);
        Ok(format!("{}{path}?{sas}", self.inner.origin))
    }

    /// Waits until an asynchronous copy of `to` finished.
    async fn await_copy(&self, to: &str) -> Result<()> {
        let mut wait = Duration::from_millis(100);
        for _ in 0..1000 {
            let r = self.call("HEAD", &self.blob_path(to), &[], &[], Vec::new()).await?;
            match r.header("x-ms-copy-status") {
                Some("pending") => {
                    tokio::time::sleep(wait).await;
                    wait = (wait * 2).min(Duration::from_secs(5));
                }
                Some("failed" | "aborted") => {
                    return Err(StorageError::Driver("Azure copy failed".into()));
                }
                _ if r.status == 200 => return Ok(()),
                _ => return Err(bad("Get Blob Properties", &r)),
            }
        }
        Err(StorageError::Driver("Azure copy did not finish".into()))
    }
}

impl StorageDriver for AzureDriver {
    fn name(&self) -> &'static str {
        "azure"
    }

    fn put(&self, key: &str, mut body: ByteStream) -> BoxFuture<'_, Result<u64>> {
        let key = key.to_owned();
        Box::pin(async move {
            let part = self.inner.cfg.part_size.max(1);
            let mut buf: Vec<u8> = Vec::new();
            while buf.len() <= part {
                match body.next().await {
                    Some(c) => buf.extend_from_slice(&c?),
                    None => {
                        let n = buf.len() as u64;
                        self.put_blob(&key, buf).await?;
                        return Ok(n);
                    }
                }
            }
            self.put_blocks(&key, buf, body).await
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
                let r = this
                    .call(
                        "GET",
                        &this.blob_path(&key),
                        &[],
                        &[("x-ms-range", format!("bytes={pos}-{}", to - 1))],
                        Vec::new(),
                    )
                    .await;
                match r {
                    Ok(resp) if matches!(resp.status, 200 | 206) && resp.body.len() as u64 == to - pos => {
                        Some((Ok(Bytes::from(resp.body)), (this, key, to)))
                    }
                    Ok(resp) if matches!(resp.status, 200 | 206) => Some((
                        Err(std::io::Error::other("Azure returned an unexpected number of bytes")),
                        (this, key, end),
                    )),
                    Ok(resp) => Some((
                        Err(std::io::Error::other(bad("Get Blob", &resp).to_string())),
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
            let r = self.call("HEAD", &self.blob_path(&key), &[], &[], Vec::new()).await?;
            if r.status == 404 {
                return Ok(None);
            }
            if r.status != 200 {
                return Err(bad("Get Blob Properties", &r));
            }
            let size = r
                .header("content-length")
                .and_then(|v| v.parse::<u64>().ok())
                .ok_or_else(|| StorageError::Driver("Azure Get Blob Properties returned no Content-Length".into()))?;
            Ok(Some(DriverObject {
                key,
                size,
                modified_ms: r.header("last-modified").map_or(0, parse_http_date_ms),
            }))
        })
    }

    fn delete(&self, key: &str) -> BoxFuture<'_, Result<()>> {
        let key = key.to_owned();
        Box::pin(async move {
            let r = self
                .call(
                    "DELETE",
                    &self.blob_path(&key),
                    &[],
                    &[("x-ms-delete-snapshots", "include".into())],
                    Vec::new(),
                )
                .await?;
            if matches!(r.status, 200 | 202 | 404) {
                Ok(())
            } else {
                Err(bad("Delete Blob", &r))
            }
        })
    }

    fn copy(&self, from: &str, to: &str) -> BoxFuture<'_, Result<()>> {
        let (from, to) = (from.to_owned(), to.to_owned());
        Box::pin(async move {
            let i = &self.inner;
            let mut source = format!(
                "{}{}",
                i.origin,
                encode(&format!("{}{}", i.base_path, self.blob_path(&from)), true)
            );
            if let AzureAuth::Sas(sas) = &i.cfg.auth {
                source.push('?');
                source.push_str(sas.trim_start_matches('?'));
            }
            let r = self
                .call(
                    "PUT",
                    &self.blob_path(&to),
                    &[],
                    &[("x-ms-copy-source", source)],
                    Vec::new(),
                )
                .await?;
            match r.status {
                404 => return Err(StorageError::NotFound("stored blob".into())),
                202 | 201 => {}
                _ => return Err(bad("Copy Blob", &r)),
            }
            match r.header("x-ms-copy-status") {
                Some("pending") => self.await_copy(&to).await,
                Some("failed" | "aborted") => Err(StorageError::Driver("Azure copy failed".into())),
                _ => Ok(()),
            }
        })
    }

    fn list(&self, prefix: &str) -> BoxFuture<'_, Result<Vec<DriverObject>>> {
        let prefix = prefix.to_owned();
        Box::pin(async move {
            let mut out = Vec::new();
            let mut marker: Option<String> = None;
            loop {
                let (objects, _, next) = self.list_page(&prefix, None, marker.as_deref()).await?;
                out.extend(objects);
                match next {
                    Some(m) => marker = Some(m),
                    None => break,
                }
            }
            out.sort_by(|a, b| a.key.cmp(&b.key));
            Ok(out)
        })
    }

    fn ping(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            let q = [
                ("restype", "container".to_owned()),
                ("comp", "list".to_owned()),
                ("maxresults", "1".to_owned()),
            ];
            let r = self.call("GET", &self.container_path(), &q, &[], Vec::new()).await?;
            if r.status == 200 {
                Ok(())
            } else {
                Err(bad("List Blobs", &r))
            }
        })
    }
}
