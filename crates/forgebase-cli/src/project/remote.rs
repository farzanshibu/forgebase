//! Talking to the server: connection (plain HTTP or HTTPS with verified
//! certificates), credentials, tenancy headers, and reading the server's
//! state into canonical specs.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use forgebase_auth::net::{HttpsClient, TlsSettings};
use forgebase_auth::social::http_client::{HttpClient, HttpRequest};
use serde_json::{Value, json};

use super::layout::{POLICY_KINDS, Target};
use super::model::{self, Actual, Key, Kind};
use crate::GlobalArgs;

/// Largest response the CLI reads (a pulled WebAssembly module, a long list).
const MAX_BODY: usize = 96 << 20;
/// Time limit of one request (a deploy compiles TypeScript on the server).
const TIMEOUT: Duration = Duration::from_secs(120);

/// How to reach and authenticate to the server (flags and environment).
#[derive(Debug, Clone, Default)]
pub struct Connection {
    /// `--server`.
    pub server: Option<String>,
    /// `--token`.
    pub token: Option<String>,
    /// `--admin-email`.
    pub admin_email: Option<String>,
    /// Password read with `--password-stdin`.
    pub admin_password: Option<String>,
    /// `--ca-file`.
    pub ca_file: Option<PathBuf>,
    /// `--realm`.
    pub realm: Option<String>,
}

fn env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

/// A server answer that was not 2xx.
#[derive(Debug, Clone)]
pub struct ApiFailure {
    /// HTTP status.
    pub status: u16,
    /// Stable error code (`error.code`).
    pub code: String,
    /// Message.
    pub message: String,
}

impl std::fmt::Display for ApiFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {} (HTTP {})", self.code, self.message, self.status)?;
        match self.status {
            401 => f.write_str(
                " (set FORGEBASE_TOKEN to an API key or personal access token, pass --token, or sign in with --admin-email)",
            ),
            403 => f.write_str(" (the credential needs administrator rights; policies need a super-administrator)"),
            _ => Ok(()),
        }
    }
}

impl std::error::Error for ApiFailure {}

/// Whether an error is a server `404`.
pub fn is_not_found(e: &anyhow::Error) -> bool {
    e.downcast_ref::<ApiFailure>().is_some_and(|f| f.status == 404)
}

/// A connected server.
pub struct Remote {
    http: HttpsClient,
    /// Base URL without a trailing slash.
    pub base: String,
    token: Option<String>,
    headers: Vec<(String, String)>,
    /// Realm of OAuth clients.
    pub realm: String,
}

impl std::fmt::Debug for Remote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Remote")
            .field("base", &self.base)
            .field("realm", &self.realm)
            .finish_non_exhaustive()
    }
}

/// `http://` / `https://` base URL (a bare `host:port` means `http://`).
pub fn normalize_base(url: &str) -> anyhow::Result<String> {
    let url = url.trim();
    let full = if url.contains("://") {
        url.to_owned()
    } else {
        format!("http://{url}")
    };
    let parsed = url::Url::parse(&full).with_context(|| format!("{url:?} is not a URL"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        bail!("{url:?}: only http:// and https:// servers are supported");
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        bail!("{url:?}: put credentials in FORGEBASE_TOKEN, not in the URL");
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        bail!("{url:?}: the server URL has no query or fragment");
    }
    Ok(full.trim_end_matches('/').to_owned())
}

impl Remote {
    /// Connects using flags, then `FORGEBASE_URL`/`FB_URL`, then the project
    /// target, then the local config's bind address; signs in when asked.
    pub async fn connect(c: &Connection, target: &Target, g: &GlobalArgs) -> anyhow::Result<Self> {
        let url = c
            .server
            .clone()
            .or_else(|| env("FORGEBASE_URL"))
            .or_else(|| env("FB_URL"))
            .or_else(|| target.url.clone());
        let base = match url {
            Some(u) => normalize_base(&u)?,
            None => {
                let cfg = g.load()?.config;
                format!("http://{}", crate::probe::dial_addr(&cfg.server.host, cfg.server.port))
            }
        };
        let mut tls = TlsSettings::new();
        if let Some(ca) = c
            .ca_file
            .clone()
            .or_else(|| env("FORGEBASE_CA_FILE").map(PathBuf::from))
        {
            tls = tls
                .with_pem_file(&ca)
                .map_err(|e| anyhow!("--ca-file {}: {e}", ca.display()))?;
        }
        let http = HttpsClient::new(&tls)
            .map_err(|e| anyhow!("TLS setup: {e}"))?
            .with_timeout(TIMEOUT)
            .with_max_body(MAX_BODY);
        let realm = c
            .realm
            .clone()
            .or_else(|| target.realm.clone())
            .unwrap_or_else(|| "default".into());
        let mut remote = Self {
            http,
            base,
            token: None,
            headers: Vec::new(),
            realm,
        };
        remote.token = c
            .token
            .clone()
            .or_else(|| env("FORGEBASE_TOKEN"))
            .or_else(|| env("FB_TOKEN"));
        let email = c.admin_email.clone().or_else(|| env("FORGEBASE_ADMIN_EMAIL"));
        if remote.token.is_none()
            && let Some(email) = email
        {
            let password = c
                .admin_password
                .clone()
                .or_else(|| std::env::var("FORGEBASE_ADMIN_PASSWORD").ok())
                .ok_or_else(|| {
                    anyhow!("--admin-email needs the password: --password-stdin or FORGEBASE_ADMIN_PASSWORD")
                })?;
            let out = remote
                .call(
                    "POST",
                    "/api/auth/admins/login",
                    Some(&json!({"email": email, "password": password})),
                )
                .await
                .context("administrator sign-in")?;
            remote.token = Some(
                out["access_token"]
                    .as_str()
                    .ok_or_else(|| anyhow!("the sign-in answer has no access token"))?
                    .to_owned(),
            );
        }
        if c.realm.is_some() || target.realm.is_some() {
            remote.headers.push(("X-Forgebase-Realm".into(), remote.realm.clone()));
        }
        if let (Some(project), Some(env_name)) = (&target.project, &target.environment) {
            remote.select_environment(project, env_name).await?;
        }
        Ok(remote)
    }

    /// Adds the organization and tenant headers of a project environment.
    async fn select_environment(&mut self, project: &str, env_name: &str) -> anyhow::Result<()> {
        let p = match self
            .call("GET", &format!("/api/v1/projects/{}", enc(project)), None)
            .await
        {
            Ok(p) => p,
            // Not an id (or not one the caller may read by id): look the slug up.
            Err(e)
                if e.downcast_ref::<ApiFailure>()
                    .is_some_and(|f| matches!(f.status, 403 | 404)) =>
            {
                self.project_by_slug(project)
                    .await?
                    .ok_or_else(|| anyhow!("no ForgeBase project {project:?} (id or slug) on {}", self.base))?
            }
            Err(e) => return Err(e),
        };
        let p = if p["environments"].is_array() {
            p
        } else {
            let id = p["id"].as_str().unwrap_or(project).to_owned();
            self.call("GET", &format!("/api/v1/projects/{}", enc(&id)), None)
                .await?
        };
        let envs = p["environments"].as_array().cloned().unwrap_or_default();
        let e = envs
            .iter()
            .find(|e| e["name"].as_str() == Some(env_name))
            .ok_or_else(|| {
                let names: Vec<&str> = envs.iter().filter_map(|e| e["name"].as_str()).collect();
                anyhow!(
                    "project {project:?} has no environment {env_name:?} (it has: {})",
                    names.join(", ")
                )
            })?;
        if let Some(org) = p["org_id"].as_str() {
            self.headers.push(("X-Forgebase-Org".into(), org.to_owned()));
        }
        let id = e["id"].as_str().ok_or_else(|| anyhow!("environment without an id"))?;
        self.headers.push(("X-Forgebase-Tenant".into(), id.to_owned()));
        Ok(())
    }

    /// A project by slug: the caller's personal projects, then each organization's.
    async fn project_by_slug(&self, slug: &str) -> anyhow::Result<Option<Value>> {
        let list_of = |v: &Value| {
            v.as_array()
                .or_else(|| v["items"].as_array())
                .cloned()
                .unwrap_or_default()
        };
        let find = |items: Vec<Value>| items.into_iter().find(|p| p["slug"].as_str() == Some(slug));
        if let Ok(v) = self.call("GET", "/api/v1/projects", None).await
            && let Some(p) = find(list_of(&v))
        {
            return Ok(Some(p));
        }
        let orgs = list_of(&self.call("GET", "/api/v1/orgs", None).await?);
        for o in orgs {
            let Some(id) = o["id"].as_str() else { continue };
            let v = self
                .call("GET", &format!("/api/v1/orgs/{}/projects", enc(id)), None)
                .await?;
            if let Some(p) = find(list_of(&v)) {
                return Ok(Some(p));
            }
        }
        Ok(None)
    }

    /// Whether a credential is configured.
    pub fn has_credential(&self) -> bool {
        self.token.is_some()
    }

    /// One request; non-2xx answers become [`ApiFailure`] errors.
    pub async fn call(&self, method: &'static str, path: &str, body: Option<&Value>) -> anyhow::Result<Value> {
        let mut req = HttpRequest {
            method,
            url: format!("{}{path}", self.base),
            headers: vec![
                ("Accept".into(), "application/json".into()),
                (
                    "User-Agent".into(),
                    concat!("forgebase-cli/", env!("CARGO_PKG_VERSION")).into(),
                ),
            ],
            body: Vec::new(),
        };
        if let Some(t) = &self.token {
            req.headers.push(("Authorization".into(), format!("Bearer {t}")));
        }
        req.headers.extend(self.headers.iter().cloned());
        if let Some(b) = body {
            req.headers.push(("Content-Type".into(), "application/json".into()));
            req.body = serde_json::to_vec(b)?;
        }
        let reply = self
            .http
            .send(req)
            .await
            .map_err(|e| anyhow!("cannot reach the server at {}: {e}", self.base))?;
        let parsed: Value = if reply.body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&reply.body).unwrap_or(Value::Null)
        };
        if (200..300).contains(&reply.status) {
            return Ok(parsed);
        }
        let err = &parsed["error"];
        Err(ApiFailure {
            status: reply.status,
            code: err["code"].as_str().unwrap_or("error").to_owned(),
            message: err["message"]
                .as_str()
                .unwrap_or("the server refused the request")
                .to_owned(),
        }
        .into())
    }

    /// GET returning `None` on 404.
    pub async fn get_opt(&self, path: &str) -> anyhow::Result<Option<Value>> {
        match self.call("GET", path, None).await {
            Ok(v) => Ok(Some(v)),
            Err(e) if is_not_found(&e) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Raw bytes of a GET (WebAssembly modules).
    pub async fn get_bytes(&self, path: &str) -> anyhow::Result<Option<Vec<u8>>> {
        let mut req = HttpRequest::get(format!("{}{path}", self.base));
        if let Some(t) = &self.token {
            req.headers.push(("Authorization".into(), format!("Bearer {t}")));
        }
        req.headers.extend(self.headers.iter().cloned());
        let reply = self
            .http
            .send(req)
            .await
            .map_err(|e| anyhow!("cannot reach the server at {}: {e}", self.base))?;
        match reply.status {
            200 => Ok(Some(reply.body)),
            404 => Ok(None),
            s => bail!("GET {path}: HTTP {s}"),
        }
    }

    fn items(v: &Value) -> Vec<Value> {
        v["items"].as_array().cloned().unwrap_or_default()
    }

    // -----------------------------------------------------------------------
    // Reading state

    /// Every resource of `kinds` on the server.
    pub async fn fetch_all(&self, kinds: &BTreeSet<Kind>) -> anyhow::Result<BTreeMap<Key, Actual>> {
        let mut out = BTreeMap::new();
        let mut add = |list: Vec<Actual>| {
            for a in list {
                out.insert((a.kind, a.name.clone()), a);
            }
        };
        if kinds.contains(&Kind::Collection) {
            add(self.collections(None).await?);
        }
        if kinds.contains(&Kind::Bucket) {
            add(self.buckets(None).await?);
        }
        if kinds.contains(&Kind::Topic) {
            add(self.topics(None).await?);
        }
        if kinds.contains(&Kind::Function) {
            add(self.functions(None).await?);
        }
        if kinds.contains(&Kind::Subscription) {
            add(self.subscriptions(None).await?);
        }
        if kinds.contains(&Kind::Policy) {
            add(self.policies(None).await?);
        }
        if kinds.contains(&Kind::OauthClient) {
            add(self.oauth_clients(None).await?);
        }
        for a in out.values_mut() {
            model::canon_numbers(&mut a.spec);
        }
        Ok(out)
    }

    /// One resource (`None`: the server does not have it).
    pub async fn fetch_one(&self, kind: Kind, name: &str) -> anyhow::Result<Option<Actual>> {
        let one = Some(name);
        let list = match kind {
            Kind::Collection => self.collections(one).await?,
            Kind::Bucket => self.buckets(one).await?,
            Kind::Topic => self.topics(one).await?,
            Kind::Function => self.functions(one).await?,
            Kind::Subscription => self.subscriptions(one).await?,
            Kind::Policy => self.policies(one).await?,
            Kind::OauthClient => self.oauth_clients(one).await?,
        };
        Ok(list.into_iter().find(|a| a.name == name).map(|mut a| {
            model::canon_numbers(&mut a.spec);
            a
        }))
    }

    /// Collection id -> name of every collection (system ones included).
    pub async fn collection_names(&self) -> anyhow::Result<HashMap<String, String>> {
        let list = self.call("GET", "/api/v1/collections", None).await?;
        Ok(Self::items(&list)
            .iter()
            .filter_map(|c| Some((c["id"].as_str()?.to_owned(), c["name"].as_str()?.to_owned())))
            .collect())
    }

    async fn collections(&self, only: Option<&str>) -> anyhow::Result<Vec<Actual>> {
        let list = Self::items(&self.call("GET", "/api/v1/collections", None).await?);
        let names: HashMap<String, String> = list
            .iter()
            .filter_map(|c| Some((c["id"].as_str()?.to_owned(), c["name"].as_str()?.to_owned())))
            .collect();
        let wanted: Vec<&Value> = list
            .iter()
            .filter(|c| !c["system"].as_bool().unwrap_or(false))
            .filter(|c| only.is_none_or(|n| c["name"].as_str() == Some(n)))
            .collect();
        if wanted.is_empty() {
            return Ok(Vec::new());
        }
        let indexes = Self::items(&self.call("GET", "/api/v1/indexes", None).await?);
        let mut out = Vec::new();
        for c in wanted {
            let name = c["name"].as_str().unwrap_or_default().to_owned();
            let own: Vec<Value> = indexes
                .iter()
                .filter(|i| i["origin"].as_str() == Some("manual"))
                .filter(|i| i["collection"].as_str() == Some(name.as_str()) || i["collection"] == c["id"])
                .map(|i| {
                    let mut d = i["definition"].clone();
                    if d.is_object() {
                        d["name"] = i["name"].clone();
                        if d.get("kind").is_none_or(Value::is_null) {
                            d["kind"] = i["kind"].clone();
                        }
                    }
                    d
                })
                .filter(Value::is_object)
                .collect();
            let rules = self
                .call("GET", &format!("/api/v1/policies/{}", enc(&name)), None)
                .await?;
            out.push(Actual {
                kind: Kind::Collection,
                spec: model::collection_spec_from_server(c, &names, &own, Some(&rules)),
                revision: format!("v{}", c["version"].as_i64().unwrap_or(0)),
                raw: c.clone(),
                name,
            });
        }
        Ok(out)
    }

    async fn buckets(&self, only: Option<&str>) -> anyhow::Result<Vec<Actual>> {
        let list = match only {
            Some(n) => self
                .get_opt(&format!("/api/v1/storage/buckets/{}", enc(n)))
                .await?
                .into_iter()
                .collect(),
            None => Self::items(&self.call("GET", "/api/v1/storage/buckets", None).await?),
        };
        let mut out = Vec::new();
        for b in list {
            let name = b["name"].as_str().unwrap_or_default().to_owned();
            let acl = self
                .call("GET", &format!("/api/v1/storage/buckets/{}/acl", enc(&name)), None)
                .await?;
            out.push(Actual {
                kind: Kind::Bucket,
                spec: model::bucket_spec(&b, Some(&acl["entries"])),
                revision: b["updated_at"].as_str().unwrap_or_default().to_owned(),
                raw: b,
                name,
            });
        }
        Ok(out)
    }

    async fn topics(&self, only: Option<&str>) -> anyhow::Result<Vec<Actual>> {
        let list = match only {
            Some(n) => self
                .get_opt(&format!("/api/v1/queues/topics/{}", enc(n)))
                .await?
                .into_iter()
                .collect(),
            None => Self::items(&self.call("GET", "/api/v1/queues/topics", None).await?),
        };
        let names = if list.iter().any(|t| t["source"].is_object()) {
            self.collection_names().await?
        } else {
            HashMap::new()
        };
        Ok(list
            .into_iter()
            .map(|mut t| {
                // A record source may name its collection by id.
                if let Some(c) = t["source"]["collection"].as_str()
                    && let Some(n) = names.get(c)
                {
                    t["source"]["collection"] = json!(n);
                }
                Actual {
                    kind: Kind::Topic,
                    name: t["name"].as_str().unwrap_or_default().to_owned(),
                    spec: model::topic_spec(&t, false),
                    revision: t["updated_at"].as_str().unwrap_or_default().to_owned(),
                    raw: t,
                }
            })
            .collect())
    }

    async fn subscriptions(&self, only: Option<&str>) -> anyhow::Result<Vec<Actual>> {
        let list = match only {
            Some(n) => self
                .get_opt(&format!("/api/v1/queues/subscriptions/{}", enc(n)))
                .await?
                .into_iter()
                .collect(),
            None => Self::items(&self.call("GET", "/api/v1/queues/subscriptions", None).await?),
        };
        Ok(list
            .into_iter()
            .map(|s| Actual {
                kind: Kind::Subscription,
                name: s["name"].as_str().unwrap_or_default().to_owned(),
                spec: model::subscription_spec(&s, false),
                revision: s["updated_at"].as_str().unwrap_or_default().to_owned(),
                raw: s,
            })
            .collect())
    }

    async fn functions(&self, only: Option<&str>) -> anyhow::Result<Vec<Actual>> {
        let list = match only {
            Some(n) => self
                .get_opt(&format!("/api/v1/functions/{}", enc(n)))
                .await?
                .into_iter()
                .collect(),
            None => Self::items(&self.call("GET", "/api/v1/functions", None).await?),
        };
        let mut out = Vec::new();
        for f in list {
            let name = f["name"].as_str().unwrap_or_default().to_owned();
            let path = format!("/api/v1/functions/{}", enc(&name));
            let versions = Self::items(&self.call("GET", &format!("{path}/versions"), None).await?);
            let active = f["active_version"].as_i64().unwrap_or(0);
            let v = versions
                .iter()
                .find(|v| v["version"].as_i64() == Some(active))
                .or_else(|| versions.iter().find(|v| v["active"].as_bool() == Some(true)))
                .cloned()
                .unwrap_or(Value::Null);
            let secrets: BTreeSet<String> = Self::items(&self.call("GET", &format!("{path}/secrets"), None).await?)
                .iter()
                .filter_map(|s| s["name"].as_str().map(str::to_owned))
                .collect();
            let kv = self.call("GET", &format!("{path}/kv-rules"), None).await?;
            let language = v["language"].as_str().unwrap_or("wasm").to_owned();
            let sha = v["sha256"].as_str().unwrap_or_default().to_owned();
            let spec = model::function_spec(
                f["description"].as_str().unwrap_or_default(),
                f["access"].as_str().unwrap_or("permission"),
                &v,
                (&language, &sha),
                &secrets,
                Some(&kv["items"]),
            );
            let mut raw = f.clone();
            raw["active"] = v;
            out.push(Actual {
                kind: Kind::Function,
                spec,
                revision: format!(
                    "v{}/{}/{}",
                    f["latest_version"].as_i64().unwrap_or(0),
                    active,
                    f["updated_at"].as_str().unwrap_or_default()
                ),
                raw,
                name,
            });
        }
        Ok(out)
    }

    async fn policies(&self, only: Option<&str>) -> anyhow::Result<Vec<Actual>> {
        let kinds: Vec<&str> = match only {
            Some(n) => vec![n.split_once('/').map_or(n, |(k, _)| k)],
            None => POLICY_KINDS.to_vec(),
        };
        let mut out = Vec::new();
        for k in kinds {
            let list = match self
                .call("GET", &format!("/api/v1/policies/resources/{}", enc(k)), None)
                .await
            {
                Ok(v) => Self::items(&v),
                // A server that does not know a kind yet.
                Err(e)
                    if e.downcast_ref::<ApiFailure>()
                        .is_some_and(|f| f.status == 400 || f.status == 404) =>
                {
                    continue;
                }
                Err(e) => return Err(e),
            };
            for item in list {
                let name = format!("{k}/{}", item["name"].as_str().unwrap_or_default());
                out.push(Actual {
                    kind: Kind::Policy,
                    spec: model::policy_spec(&item["rules"]),
                    revision: String::new(),
                    raw: item,
                    name,
                });
            }
        }
        Ok(out)
    }

    async fn oauth_clients(&self, only: Option<&str>) -> anyhow::Result<Vec<Actual>> {
        let list = Self::items(
            &self
                .call("GET", &format!("/api/auth/clients?realm={}", enc(&self.realm)), None)
                .await?,
        );
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for c in list {
            let name = c["name"].as_str().unwrap_or_default().to_owned();
            if only.is_some_and(|n| n != name) {
                continue;
            }
            if !seen.insert(name.clone()) {
                bail!(
                    "two OAuth clients in realm {:?} are named {name:?}; rename one on the server (the project identifies clients by name)",
                    self.realm
                );
            }
            out.push(Actual {
                kind: Kind::OauthClient,
                spec: model::oauth_spec(&c, false),
                revision: c["updated_at"].as_str().unwrap_or_default().to_owned(),
                raw: c,
                name,
            });
        }
        Ok(out)
    }
}

/// Percent-encodes one path segment or query value.
pub fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_urls_are_checked() {
        assert_eq!(
            normalize_base("http://127.0.0.1:8090/").unwrap(),
            "http://127.0.0.1:8090"
        );
        assert_eq!(normalize_base("127.0.0.1:9").unwrap(), "http://127.0.0.1:9");
        assert_eq!(
            normalize_base("https://api.example.com/fb").unwrap(),
            "https://api.example.com/fb"
        );
        assert!(normalize_base("ftp://x").is_err());
        assert!(normalize_base("https://u:p@x").is_err());
        assert!(normalize_base("https://x/?a=1").is_err());
    }

    #[test]
    fn failures_carry_code_and_hint() {
        let f = ApiFailure {
            status: 401,
            code: "unauthenticated".into(),
            message: "authentication required".into(),
        };
        let text = f.to_string();
        assert!(
            text.starts_with("unauthenticated: authentication required (HTTP 401)") && text.contains("FORGEBASE_TOKEN")
        );
        assert!(is_not_found(&anyhow::Error::from(ApiFailure {
            status: 404,
            code: "not_found".into(),
            message: String::new()
        })));
    }

    #[tokio::test]
    async fn https_uses_verified_tls() {
        // A plain-HTTP listener behind an https:// URL: the handshake must fail, never fall back.
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            while let Ok((mut s, _)) = l.accept().await {
                let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}").await;
            }
        });
        let c = Connection {
            server: Some(format!("https://localhost:{}", addr.port())),
            token: Some("t".into()),
            ..Connection::default()
        };
        let r = Remote::connect(&c, &Target::default(), &GlobalArgs::default())
            .await
            .unwrap();
        let err = r.call("GET", "/health", None).await.unwrap_err().to_string();
        assert!(err.contains("TLS") || err.contains("handshake"), "{err}");
    }
}
