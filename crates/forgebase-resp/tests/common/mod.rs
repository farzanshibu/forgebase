//! Shared test harness: an in-process listener over the real KV engine, a
//! token-table authenticator, and a tiny raw RESP client for protocol-level
//! assertions. Tests bind `127.0.0.1:0`.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use forgebase_auth::Scope;
use forgebase_data::{AccessContext, DefaultAuthorizer};
use forgebase_kv::{KvConfig, KvService};
use forgebase_resp::{AuthFailure, Authenticator, BoxFuture, Identity, LocalKv, RespConfig, RespHandle, start};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

pub const T: Duration = Duration::from_secs(5);

/// Token table: `adm` (administrator, default tenant), `ta`/`tb` (administrators
/// of tenants alpha/beta), `usr` (plain user: the default authorizer denies
/// it everything), `ro` (`kv:read` only), `cacheonly` (`kv:read` and
/// `kv:write:cache`).
#[derive(Debug)]
pub struct TokenAuth {
    ids: HashMap<&'static str, Identity>,
}

impl TokenAuth {
    pub fn new() -> Self {
        fn tenant(id: &str, t: &str) -> AccessContext {
            let mut c = AccessContext::admin(id);
            c.tenant = Some(t.into());
            c
        }
        let scopes = |v: &[&str]| Some(v.iter().map(|s| Scope::parse(s).unwrap()).collect::<Vec<_>>());
        let mut ids = HashMap::new();
        ids.insert("adm", Identity::new(AccessContext::admin("a1"), "a1", None, "adm"));
        ids.insert("ta", Identity::new(tenant("a2", "alpha"), "a2", None, "ta"));
        ids.insert("tb", Identity::new(tenant("a3", "beta"), "a3", None, "tb"));
        ids.insert(
            "usr",
            Identity::new(AccessContext::user("users", "u1"), "u1", None, "usr"),
        );
        ids.insert(
            "ro",
            Identity::new(AccessContext::admin("a4"), "a4", scopes(&["kv:read"]), "ro"),
        );
        ids.insert(
            "cacheonly",
            Identity::new(
                AccessContext::admin("a5"),
                "a5",
                scopes(&["kv:read", "kv:write:cache"]),
                "cacheonly",
            ),
        );
        Self { ids }
    }
}

impl Authenticator for TokenAuth {
    fn authenticate<'a>(
        &'a self,
        username: Option<&'a str>,
        secret: &'a str,
    ) -> BoxFuture<'a, Result<Identity, AuthFailure>> {
        Box::pin(async move {
            let id = self.ids.get(secret).ok_or(AuthFailure::Invalid)?;
            match username {
                None | Some("" | "default") => Ok(id.clone()),
                Some(u) if u == id.subject_id => Ok(id.clone()),
                Some(_) => Err(AuthFailure::Invalid),
            }
        })
    }
}

pub struct Server {
    pub handle: RespHandle,
    pub kv: KvService,
    pub addr: SocketAddr,
}

pub fn test_cfg() -> RespConfig {
    RespConfig {
        port: 0,
        auth_failure_delay: Duration::ZERO,
        shutdown_grace: Duration::from_secs(2),
        ..RespConfig::default()
    }
}

pub async fn server() -> Server {
    server_with(|_| {}, KvConfig::default()).await
}

pub async fn server_with(tweak: impl FnOnce(&mut RespConfig), kv_cfg: KvConfig) -> Server {
    let kv = KvService::open(
        KvConfig {
            flush_interval: Duration::ZERO,
            ..kv_cfg
        },
        Arc::new(DefaultAuthorizer),
        None,
    )
    .await
    .unwrap();
    kv.start_background();
    let mut cfg = test_cfg();
    tweak(&mut cfg);
    let handle = start(cfg, Arc::new(LocalKv(kv.clone())), Arc::new(TokenAuth::new()))
        .await
        .expect("bind 127.0.0.1:0 (if this is 'Operation not permitted' the sandbox blocks local ports)");
    let addr = handle.local_addr();
    Server { handle, kv, addr }
}

impl Server {
    pub fn url(&self, token: &str, db: u32) -> String {
        format!("redis://:{token}@{}/{db}", self.addr)
    }

    pub fn url3(&self, token: &str, db: u32) -> String {
        format!("redis://:{token}@{}/{db}?protocol=resp3", self.addr)
    }

    pub async fn conn(&self, token: &str) -> redis::aio::MultiplexedConnection {
        redis::Client::open(self.url(token, 0))
            .unwrap()
            .get_multiplexed_async_connection()
            .await
            .unwrap()
    }

    pub async fn raw(&self) -> Raw {
        Raw::connect(self.addr).await
    }

    /// A raw connection already authenticated with `token`.
    pub async fn raw_as(&self, token: &str) -> Raw {
        let mut r = self.raw().await;
        assert_eq!(r.cmd(&["AUTH", token]).await, Val::Simple("OK".into()));
        r
    }
}

/// A parsed RESP reply (RESP2 and RESP3 types).
#[derive(Debug, Clone, PartialEq)]
pub enum Val {
    Simple(String),
    Err(String),
    Int(i64),
    Bulk(Vec<u8>),
    Null,
    Arr(Vec<Val>),
    Map(Vec<(Val, Val)>),
    Set(Vec<Val>),
    Push(Vec<Val>),
}

impl Val {
    pub fn bulk(s: &str) -> Val {
        Val::Bulk(s.as_bytes().to_vec())
    }

    pub fn ok() -> Val {
        Val::Simple("OK".into())
    }

    pub fn is_err_starting(&self, prefix: &str) -> bool {
        matches!(self, Val::Err(e) if e.starts_with(prefix))
    }

    pub fn err_text(&self) -> String {
        match self {
            Val::Err(e) => e.clone(),
            other => panic!("expected an error, got {other:?}"),
        }
    }

    pub fn as_array(&self) -> &[Val] {
        match self {
            Val::Arr(v) | Val::Set(v) | Val::Push(v) => v,
            other => panic!("expected an array, got {other:?}"),
        }
    }
}

pub struct Raw {
    pub s: TcpStream,
    buf: Vec<u8>,
}

/// Wire encoding of a command.
pub fn encode(parts: &[&str]) -> Vec<u8> {
    let mut out = format!("*{}\r\n", parts.len()).into_bytes();
    for p in parts {
        out.extend_from_slice(format!("${}\r\n{p}\r\n", p.len()).as_bytes());
    }
    out
}

impl Raw {
    pub async fn connect(addr: SocketAddr) -> Raw {
        let s = timeout(T, TcpStream::connect(addr)).await.unwrap().unwrap();
        Raw { s, buf: Vec::new() }
    }

    pub async fn send(&mut self, parts: &[&str]) {
        self.send_raw(&encode(parts)).await;
    }

    pub async fn send_raw(&mut self, bytes: &[u8]) {
        timeout(T, self.s.write_all(bytes)).await.unwrap().unwrap();
    }

    pub async fn cmd(&mut self, parts: &[&str]) -> Val {
        self.send(parts).await;
        self.read().await
    }

    /// Next reply; panics on EOF or timeout.
    pub async fn read(&mut self) -> Val {
        self.try_read().await.expect("connection closed")
    }

    /// Next reply, `None` on EOF.
    pub async fn try_read(&mut self) -> Option<Val> {
        timeout(T, self.parse()).await.expect("timed out waiting for a reply")
    }

    /// Whether the peer closed the connection (waits up to `d`).
    pub async fn closed_within(&mut self, d: Duration) -> bool {
        let mut b = [0u8; 1];
        loop {
            match timeout(d, self.s.read(&mut b)).await {
                Ok(Ok(0) | Err(_)) => return true,
                Ok(Ok(_)) => {}
                Err(_) => return false,
            }
        }
    }

    async fn fill(&mut self) -> bool {
        let mut chunk = [0u8; 16 * 1024];
        match self.s.read(&mut chunk).await {
            Ok(0) | Err(_) => false,
            Ok(n) => {
                self.buf.extend_from_slice(&chunk[..n]);
                true
            }
        }
    }

    async fn line(&mut self) -> Option<String> {
        loop {
            if let Some(i) = self.buf.windows(2).position(|w| w == b"\r\n") {
                let l = String::from_utf8_lossy(&self.buf[..i]).into_owned();
                self.buf.drain(..i + 2);
                return Some(l);
            }
            if !self.fill().await {
                return None;
            }
        }
    }

    async fn exact(&mut self, n: usize) -> Option<Vec<u8>> {
        while self.buf.len() < n + 2 {
            if !self.fill().await {
                return None;
            }
        }
        let v = self.buf[..n].to_vec();
        self.buf.drain(..n + 2);
        Some(v)
    }

    fn parse(&mut self) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<Val>> + Send + '_>> {
        Box::pin(async move {
            let l = self.line().await?;
            let (t, rest) = l.split_at(1);
            Some(match t {
                "+" => Val::Simple(rest.into()),
                "-" => Val::Err(rest.into()),
                ":" => Val::Int(rest.parse().unwrap()),
                "_" => Val::Null,
                "$" => {
                    let n: i64 = rest.parse().unwrap();
                    if n < 0 {
                        Val::Null
                    } else {
                        Val::Bulk(self.exact(usize::try_from(n).unwrap()).await?)
                    }
                }
                "*" | "~" | ">" => {
                    let n: i64 = rest.parse().unwrap();
                    if n < 0 {
                        return Some(Val::Null);
                    }
                    let mut v = Vec::new();
                    for _ in 0..n {
                        v.push(self.parse().await?);
                    }
                    match t {
                        "*" => Val::Arr(v),
                        "~" => Val::Set(v),
                        _ => Val::Push(v),
                    }
                }
                "%" => {
                    let n: usize = rest.parse().unwrap();
                    let mut v = Vec::new();
                    for _ in 0..n {
                        let k = self.parse().await?;
                        let val = self.parse().await?;
                        v.push((k, val));
                    }
                    Val::Map(v)
                }
                other => panic!("unexpected reply type {other:?} in {l:?}"),
            })
        })
    }
}

/// Polls `f` until it returns `Some` (or fails after `secs`).
pub async fn wait_for<T2, F: std::future::Future<Output = Option<T2>>>(secs: u64, mut f: impl FnMut() -> F) -> T2 {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    loop {
        if let Some(v) = f().await {
            return v;
        }
        assert!(tokio::time::Instant::now() < deadline, "timed out");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
