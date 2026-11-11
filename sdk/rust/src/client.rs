//! The client entry point.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::auth::{AuthApi, AuthState, MemoryStore, SessionStore};
use crate::billing::BillingApi;
use crate::consistency::{CommitTokens, Consistency};
use crate::entitlements::EntitlementsApi;
use crate::error::{Error, Result};
use crate::functions::FunctionsApi;
use crate::http::{Req, RetryPolicy, Transport, seg};
use crate::jobs::JobsApi;
use crate::kv::Kv;
use crate::queues::QueuesApi;
use crate::realtime::{Realtime, RealtimeConfig};
use crate::records::{CollectionsApi, Records};
use crate::storage::StorageApi;

/// Builds a [`ForgeBase`] client.
///
/// ```no_run
/// # async fn run() -> forgebase_sdk::Result<()> {
/// use forgebase_sdk::ForgeBase;
/// let fb = ForgeBase::builder("http://localhost:8090")
///     .api_key("fb_...")
///     .build()?;
/// # let _ = fb; Ok(()) }
/// ```
pub struct ClientBuilder {
    base_url: String,
    api_key: Option<String>,
    access_token: Option<String>,
    retry: RetryPolicy,
    timeout: Duration,
    connect_timeout: Duration,
    user_agent: String,
    store: Option<Arc<dyn SessionStore>>,
    auto_refresh: bool,
    refresh_skew: Duration,
    headers: Vec<(String, String)>,
    realtime: RealtimeConfig,
    http: Option<reqwest::Client>,
    consistency: Consistency,
}

impl ClientBuilder {
    /// Read replicas: [`Consistency::Session`] (default) reads your own writes through the commit
    /// tokens the client tracks; [`Consistency::Strong`] reads every call from the primary. For
    /// some calls only, use [`ForgeBase::with_consistency`].
    #[must_use]
    pub fn consistency(mut self, consistency: Consistency) -> Self {
        self.consistency = consistency;
        self
    }

    /// Authenticates every request with an API key, service-account key or personal access token.
    #[must_use]
    pub fn api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into());
        self
    }

    /// Authenticates every request with a fixed bearer token (no refresh).
    #[must_use]
    pub fn access_token(mut self, token: impl Into<String>) -> Self {
        self.access_token = Some(token.into());
        self
    }

    /// Retry policy for transient failures ([`RetryPolicy::none`] disables retries).
    #[must_use]
    pub fn retry(mut self, policy: RetryPolicy) -> Self {
        self.retry = policy;
        self
    }

    /// Per-request timeout (default 30 s).
    #[must_use]
    pub fn timeout(mut self, d: Duration) -> Self {
        self.timeout = d;
        self
    }

    /// Connection timeout (default 10 s).
    #[must_use]
    pub fn connect_timeout(mut self, d: Duration) -> Self {
        self.connect_timeout = d;
        self
    }

    /// `User-Agent` header (default `forgebase-sdk-rust/<version>`).
    #[must_use]
    pub fn user_agent(mut self, ua: impl Into<String>) -> Self {
        self.user_agent = ua.into();
        self
    }

    /// Where the session is kept (default: memory). See [`FileStore`](crate::FileStore).
    #[must_use]
    pub fn session_store(mut self, store: impl SessionStore + 'static) -> Self {
        self.store = Some(Arc::new(store));
        self
    }

    /// Refresh the access token automatically (default true).
    #[must_use]
    pub fn auto_refresh(mut self, on: bool) -> Self {
        self.auto_refresh = on;
        self
    }

    /// Refresh the access token this long before it expires (default 30 s).
    #[must_use]
    pub fn refresh_skew(mut self, d: Duration) -> Self {
        self.refresh_skew = d;
        self
    }

    /// Adds a header to every request.
    #[must_use]
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Realtime connection settings.
    #[must_use]
    pub fn realtime(mut self, cfg: RealtimeConfig) -> Self {
        self.realtime = cfg;
        self
    }

    /// Uses your own `reqwest::Client` (proxy, custom roots, ...). Timeouts and user agent are then yours to set.
    #[must_use]
    pub fn http_client(mut self, client: reqwest::Client) -> Self {
        self.http = Some(client);
        self
    }

    /// Builds the client.
    pub fn build(self) -> Result<ForgeBase> {
        let base = self.base_url.trim_end_matches('/').to_owned();
        let parsed = url::Url::parse(&base).map_err(|e| Error::Config(format!("bad base URL {base:?}: {e}")))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(Error::Config(format!(
                "base URL must be http or https, got {:?}",
                parsed.scheme()
            )));
        }
        // rustls needs a process-wide crypto provider; the SDK ships `ring`. Ignore "already installed".
        let _ = rustls::crypto::ring::default_provider().install_default();
        let http = match self.http {
            Some(c) => c,
            None => reqwest::Client::builder()
                .timeout(self.timeout)
                .connect_timeout(self.connect_timeout)
                .user_agent(self.user_agent)
                .build()
                .map_err(|e| Error::Config(format!("cannot build the HTTP client: {e}")))?,
        };
        let store = self.store.unwrap_or_else(|| Arc::new(MemoryStore::default()));
        let skew = u64::try_from(self.refresh_skew.as_millis()).unwrap_or(30_000);
        let auth = Arc::new(AuthState::new(
            self.api_key,
            self.access_token,
            store,
            self.auto_refresh,
            skew,
        ));
        let t = Arc::new(Transport {
            base,
            http,
            retry: self.retry,
            auth,
            extra_headers: self.headers,
            tokens: Arc::new(CommitTokens::default()),
            consistency: self.consistency,
        });
        Ok(ForgeBase {
            realtime: Realtime::new(t.clone(), self.realtime),
            t,
        })
    }
}

/// The ForgeBase client. Cheap to clone; clones share credentials and the realtime connection.
#[derive(Clone)]
pub struct ForgeBase {
    pub(crate) t: Arc<Transport>,
    realtime: Realtime,
}

impl ForgeBase {
    /// A client for `base_url` with default settings and no credentials.
    pub fn new(base_url: impl Into<String>) -> Result<Self> {
        Self::builder(base_url).build()
    }

    /// Starts building a client.
    #[must_use]
    pub fn builder(base_url: impl Into<String>) -> ClientBuilder {
        ClientBuilder {
            base_url: base_url.into(),
            api_key: None,
            access_token: None,
            retry: RetryPolicy::default(),
            timeout: Duration::from_secs(30),
            connect_timeout: Duration::from_secs(10),
            user_agent: format!("forgebase-sdk-rust/{}", env!("CARGO_PKG_VERSION")),
            store: None,
            auto_refresh: true,
            refresh_skew: Duration::from_secs(30),
            headers: Vec::new(),
            realtime: RealtimeConfig::default(),
            http: None,
            consistency: Consistency::Session,
        }
    }

    /// A client sharing this one's credentials, commit tokens and realtime connection, whose
    /// calls use `consistency`: `fb.with_consistency(Consistency::Strong).collection("posts")`.
    #[must_use]
    pub fn with_consistency(&self, consistency: Consistency) -> Self {
        let t = &self.t;
        Self {
            t: Arc::new(Transport {
                base: t.base.clone(),
                http: t.http.clone(),
                retry: t.retry.clone(),
                auth: t.auth.clone(),
                extra_headers: t.extra_headers.clone(),
                tokens: t.tokens.clone(),
                consistency,
            }),
            realtime: self.realtime.clone(),
        }
    }

    /// Commit tokens of this client's writes, sent as `X-ForgeBase-Read-After`. Realtime events
    /// that carry a position can feed it ([`CommitTokens::note`]).
    #[must_use]
    pub fn commit_tokens(&self) -> Arc<CommitTokens> {
        self.t.tokens.clone()
    }

    /// Sign-up, sign-in, MFA, tokens and sessions.
    #[must_use]
    pub fn auth(&self) -> AuthApi {
        AuthApi { t: self.t.clone() }
    }

    /// Collection definitions.
    #[must_use]
    pub fn collections(&self) -> CollectionsApi {
        CollectionsApi { t: self.t.clone() }
    }

    /// Records of one collection, as JSON objects. Use [`Records::typed`] for your own structs.
    #[must_use]
    pub fn collection(&self, name: &str) -> Records<Value> {
        Records::new(self.t.clone(), name)
    }

    /// Object storage.
    #[must_use]
    pub fn storage(&self) -> StorageApi {
        StorageApi { t: self.t.clone() }
    }

    /// A key-value namespace.
    #[must_use]
    pub fn kv(&self, namespace: &str) -> Kv {
        Kv {
            t: self.t.clone(),
            ns: namespace.to_owned(),
        }
    }

    /// Server functions.
    #[must_use]
    pub fn functions(&self) -> FunctionsApi {
        FunctionsApi { t: self.t.clone() }
    }

    /// Background jobs.
    #[must_use]
    pub fn jobs(&self) -> JobsApi {
        JobsApi {
            t: self.t.clone(),
            realtime: self.realtime.clone(),
        }
    }

    /// Durable topics and queues: publish, pull, ack, replay and streaming pull.
    #[must_use]
    pub fn queues(&self) -> QueuesApi {
        QueuesApi { t: self.t.clone() }
    }

    /// Billing helpers (store receipts).
    #[must_use]
    pub fn billing(&self) -> BillingApi {
        BillingApi { t: self.t.clone() }
    }

    /// Entitlements and quotas.
    #[must_use]
    pub fn entitlements(&self) -> EntitlementsApi {
        EntitlementsApi { t: self.t.clone() }
    }

    /// Realtime subscriptions (WebSocket with SSE fallback).
    #[must_use]
    pub fn realtime(&self) -> Realtime {
        self.realtime.clone()
    }

    /// True when `/health` answers 200.
    pub async fn health(&self) -> bool {
        self.t
            .unit(Req::get("/health").anonymous().retry(RetryPolicy::none()))
            .await
            .is_ok()
    }

    /// Escape hatch for endpoints the SDK has no helper for: `method` is `GET`, `POST`, ... and `path`
    /// starts with `/` (it is joined to the base URL as is, so encode path segments yourself).
    /// The body is JSON; the response is JSON (`Null` when empty). Auth, refresh and retries apply.
    pub async fn request(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value> {
        let m = reqwest::Method::from_bytes(method.to_ascii_uppercase().as_bytes())
            .map_err(|_| Error::Config(format!("bad HTTP method {method:?}")))?;
        let mut r = Req::new(m, path);
        if let Some(b) = body {
            r = r.json_raw(b);
        }
        self.t.value(r).await
    }

    /// Encodes a path segment (helper for [`request`](Self::request)).
    #[must_use]
    pub fn encode_segment(segment: &str) -> String {
        seg(segment)
    }
}

impl Req {
    pub(crate) fn retry(mut self, p: RetryPolicy) -> Self {
        self.retry = Some(p);
        self
    }
}
