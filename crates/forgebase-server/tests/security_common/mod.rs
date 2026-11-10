//! Shared harness of the FB-80 bypass suite: a real server on `127.0.0.1:0`,
//! a raw HTTP/1.1 client (so headers such as `X-Forwarded-For` and odd paths
//! go out exactly as written), and identities created through a second auth
//! service on the same database with fast Argon2 parameters (the hash
//! carries its parameters, so the server verifies them quickly too).
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use forgebase_auth::{Argon2Config, AuthConfig, AuthService, ClientInfo};
use forgebase_core::Config;
use forgebase_data::DefaultAuthorizer;
use forgebase_server::RunningServer;
use serde_json::{Value as Json, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const ADMIN_PASSWORD: &str = "a-long-admin-password-1";
pub const USER_PASSWORD: &str = "correct-horse-battery-9";

/// One HTTP response.
#[derive(Debug, Clone)]
pub struct Resp {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Resp {
    pub fn json(&self) -> Json {
        serde_json::from_slice(&self.body).unwrap_or(Json::Null)
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// Everything the client received, for "never appears" checks.
    pub fn raw(&self) -> String {
        let mut s = String::new();
        for (k, v) in &self.headers {
            s.push_str(&format!("{k}: {v}\n"));
        }
        s.push_str(&self.text());
        s
    }

    pub fn code(&self) -> String {
        self.json()["error"]["code"].as_str().unwrap_or_default().to_owned()
    }
}

/// Sends one request over a fresh connection.
pub async fn send(addr: SocketAddr, method: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> Resp {
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: forgebase\r\nConnection: close\r\n");
    let mut has_type = false;
    for (k, v) in headers {
        has_type |= k.eq_ignore_ascii_case("content-type");
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    if !body.is_empty() && !has_type {
        req.push_str("Content-Type: application/json\r\n");
    }
    req.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    s.write_all(req.as_bytes()).await.unwrap();
    // The server may answer (413) and close before reading a large body.
    let _ = s.write_all(body).await;
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out).await;
    parse(&out)
}

fn parse(raw: &[u8]) -> Resp {
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap_or(raw.len());
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let mut rest = raw.get(split + 4..).unwrap_or_default().to_vec();
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|l| l.split(' ').nth(1))
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    if headers
        .iter()
        .any(|(k, v)| k == "transfer-encoding" && v.eq_ignore_ascii_case("chunked"))
    {
        rest = dechunk(&rest);
    }
    Resp {
        status,
        headers,
        body: rest,
    }
}

fn dechunk(mut b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(nl) = b.windows(2).position(|w| w == b"\r\n") {
        let size = usize::from_str_radix(String::from_utf8_lossy(&b[..nl]).trim(), 16).unwrap_or(0);
        if size == 0 {
            break;
        }
        let start = nl + 2;
        out.extend_from_slice(&b[start..start + size]);
        b = &b[start + size + 2..];
    }
    out
}

pub fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

/// A started server plus a fast-hashing auth service on its database.
pub struct Env {
    pub server: RunningServer,
    pub auth: AuthService,
    pub addr: SocketAddr,
    pub dir: tempfile::TempDir,
}

pub fn config(dir: &Path) -> Config {
    let mut cfg = Config::defaults(dir);
    cfg.server.host = "127.0.0.1".into();
    cfg.server.port = 0;
    cfg.server.shutdown_timeout_secs = 5;
    cfg.queue.poll_interval_ms = 50;
    cfg
}

impl Env {
    pub async fn new() -> Self {
        Self::with(|_| {}).await
    }

    pub async fn with(f: impl FnOnce(&mut Config)) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = config(dir.path());
        f(&mut cfg);
        let server = forgebase_server::start(&cfg).await.unwrap();
        let mut acfg = AuthConfig::for_data_dir(&cfg.data_dir);
        acfg.argon2 = Argon2Config::insecure_fast_for_tests();
        let auth = AuthService::open_with_data(
            Arc::new(server.database().clone()),
            Arc::new(DefaultAuthorizer),
            &cfg.data_dir,
            acfg,
            None,
        )
        .await
        .unwrap();
        let addr = server.local_addr();
        Self {
            server,
            auth,
            addr,
            dir,
        }
    }

    pub async fn req(&self, method: &str, path: &str, headers: &[(&str, &str)], body: Option<Json>) -> Resp {
        let bytes = body.map(|b| serde_json::to_vec(&b).unwrap()).unwrap_or_default();
        send(self.addr, method, path, headers, &bytes).await
    }

    /// `method path` with a bearer token and extra headers.
    pub async fn as_(&self, token: &str, method: &str, path: &str, extra: &[(&str, &str)], body: Option<Json>) -> Resp {
        let auth = bearer(token);
        let mut h = vec![("Authorization", auth.as_str())];
        h.extend_from_slice(extra);
        self.req(method, path, &h, body).await
    }

    /// Creates an administrator and signs in over HTTP.
    pub async fn admin(&self, email: &str) -> String {
        self.auth
            .create_admin(email, ADMIN_PASSWORD, "test", &ClientInfo::default())
            .await
            .unwrap();
        let r = self
            .req(
                "POST",
                "/api/auth/admins/login",
                &[],
                Some(json!({"email": email, "password": ADMIN_PASSWORD})),
            )
            .await;
        assert_eq!(r.status, 200, "admin login: {}", r.text());
        r.json()["access_token"].as_str().unwrap().to_owned()
    }

    /// Creates a user in the default realm and signs in over HTTP; returns
    /// (access token, principal key `user:<collection>/<id>`).
    pub async fn user(&self, email: &str) -> (String, String) {
        let v = self
            .auth
            .signup(&ClientInfo::default(), None, email, USER_PASSWORD, None)
            .await
            .unwrap();
        let r = self
            .req(
                "POST",
                "/api/auth/login",
                &[],
                Some(json!({"email": email, "password": USER_PASSWORD})),
            )
            .await;
        assert_eq!(r.status, 200, "login: {}", r.text());
        let j = r.json();
        let id = v["id"].as_str().unwrap().to_owned();
        let coll = j["user"]["collection_name"]
            .as_str()
            .or_else(|| j["user"]["collection"].as_str())
            .unwrap_or("users")
            .to_owned();
        (
            j["access_token"].as_str().unwrap().to_owned(),
            format!("user:{coll}/{id}"),
        )
    }

    /// An admin service account's API key with `scopes`.
    pub async fn admin_key(&self, admin: &str, scopes: &[&str]) -> String {
        let sa = self
            .as_(
                admin,
                "POST",
                "/api/auth/service-accounts",
                &[],
                Some(json!({"name": format!("sa-{}", scopes.join("-").replace([':', '*'], "_")), "role": "admin"})),
            )
            .await;
        assert_eq!(sa.status, 201, "service account: {}", sa.text());
        let id = sa.json()["id"].as_str().unwrap().to_owned();
        let k = self
            .as_(
                admin,
                "POST",
                &format!("/api/auth/service-accounts/{id}/keys"),
                &[],
                Some(json!({"name": "k", "scopes": scopes})),
            )
            .await;
        assert_eq!(k.status, 201, "key: {}", k.text());
        k.json()["key"].as_str().unwrap().to_owned()
    }

    pub async fn shutdown(self) {
        self.server.shutdown().await.unwrap();
    }
}

/// A loopback listener that counts the connections it accepts (an SSRF
/// target standing in for an internal service).
pub struct Canary {
    pub port: u16,
    hits: Arc<std::sync::atomic::AtomicUsize>,
}

impl Canary {
    pub async fn start() -> Self {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let h = hits.clone();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                h.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let _ = s
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                    .await;
            }
        });
        Self { port, hits }
    }

    pub fn hits(&self) -> usize {
        self.hits.load(std::sync::atomic::Ordering::SeqCst)
    }
}
