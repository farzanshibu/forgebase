//! SSRF protection of the `fetch` capability: blocked address ranges (after
//! DNS resolution), DNS rebinding, redirects re-checked, host allow-lists,
//! and the same refusals seen from inside a function. A loopback mock server
//! stands in for a public host: the test connector routes the vetted public
//! address 93.184.215.14 to it, so the real checks run on real addresses.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use common::{Wat, harness_with, req};
use forgebase_auth::net::TlsSettings;
use forgebase_db::BoxFuture;
use forgebase_functions::fetch::{Connector, FetchConfig, FetchError, FetchRequest, Fetcher, Resolver};
use forgebase_functions::{FunctionsConfig, Trigger};
use serde_json::{Value as Json, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const PUBLIC: &str = "93.184.215.14";

fn public() -> IpAddr {
    PUBLIC.parse().unwrap()
}

/// Answers by path; see the match below.
async fn mock_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    match s.read(&mut chunk).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                }
                let head = String::from_utf8_lossy(&buf).to_string();
                let path = head.split(' ').nth(1).unwrap_or("/").to_owned();
                let (status, extra, body) = match path.as_str() {
                    "/ok" => ("200 OK", String::new(), "hello".to_owned()),
                    "/redirect-metadata" => (
                        "302 Found",
                        "Location: http://169.254.169.254/latest/meta-data/\r\n".to_owned(),
                        String::new(),
                    ),
                    "/redirect-localhost" => (
                        "302 Found",
                        "Location: http://localhost/ok\r\n".to_owned(),
                        String::new(),
                    ),
                    "/redirect-ten" => (
                        "307 Temporary Redirect",
                        "Location: http://10.0.0.7/\r\n".to_owned(),
                        String::new(),
                    ),
                    "/redirect-public" => ("301 Moved", "Location: /ok\r\n".to_owned(), String::new()),
                    "/redirect-other" => (
                        "302 Found",
                        "Location: http://other.test/echo\r\n".to_owned(),
                        String::new(),
                    ),
                    "/redirect-loop" => ("302 Found", "Location: /redirect-loop\r\n".to_owned(), String::new()),
                    "/echo" => ("200 OK", String::new(), head.to_ascii_lowercase()),
                    _ => ("404 Not Found", String::new(), "no".to_owned()),
                };
                let res = format!(
                    "HTTP/1.1 {status}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = s.write_all(res.as_bytes()).await;
            });
        }
    });
    addr
}

/// Fixed answers; counts lookups.
#[derive(Debug, Default)]
struct MapResolver {
    map: HashMap<String, Vec<IpAddr>>,
    calls: AtomicUsize,
}

impl MapResolver {
    fn with(entries: &[(&str, &[&str])]) -> Self {
        Self {
            map: entries
                .iter()
                .map(|(h, ips)| ((*h).to_owned(), ips.iter().map(|i| i.parse().unwrap()).collect()))
                .collect(),
            calls: AtomicUsize::new(0),
        }
    }
}

impl Resolver for MapResolver {
    fn resolve<'a>(&'a self, host: &'a str, _port: u16) -> BoxFuture<'a, io::Result<Vec<IpAddr>>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let r = self
            .map
            .get(host)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such host"));
        Box::pin(async move { r })
    }
}

/// A rebinding DNS server: a public address for the first lookup of each
/// name, loopback afterwards.
#[derive(Debug, Default)]
struct Rebinding {
    calls: AtomicUsize,
}

impl Resolver for Rebinding {
    fn resolve<'a>(&'a self, _host: &'a str, _port: u16) -> BoxFuture<'a, io::Result<Vec<IpAddr>>> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { Ok(vec![if n == 0 { public() } else { "127.0.0.1".parse().unwrap() }]) })
    }
}

/// Routes the public test address to the mock; records every connection.
#[derive(Debug)]
struct Route {
    mock: SocketAddr,
    seen: Mutex<Vec<SocketAddr>>,
}

impl Connector for Route {
    fn connect(&self, addr: SocketAddr) -> BoxFuture<'_, io::Result<TcpStream>> {
        self.seen.lock().unwrap().push(addr);
        let target = if addr.ip() == public() { self.mock } else { addr };
        Box::pin(TcpStream::connect(target))
    }
}

struct Net {
    fetcher: Fetcher,
    route: Arc<Route>,
    mock: SocketAddr,
}

async fn net(resolver: Arc<dyn Resolver>) -> Net {
    let mock = mock_server().await;
    let route = Arc::new(Route {
        mock,
        seen: Mutex::new(Vec::new()),
    });
    let fetcher = Fetcher::with(&TlsSettings::new(), FetchConfig::default(), resolver, route.clone()).unwrap();
    Net { fetcher, route, mock }
}

fn names() -> Arc<MapResolver> {
    Arc::new(MapResolver::with(&[
        ("public.test", &[PUBLIC]),
        ("other.test", &[PUBLIC]),
        ("localhost", &["127.0.0.1", "::1"]),
        ("internal.test", &["10.1.2.3"]),
        ("metadata.test", &["169.254.169.254"]),
        ("mixed.test", &[PUBLIC, "127.0.0.1"]),
        ("v6loop.test", &["::1"]),
        ("mapped.test", &["::ffff:192.168.0.10"]),
    ]))
}

fn get(url: &str) -> FetchRequest {
    FetchRequest {
        method: "GET".into(),
        url: url.into(),
        headers: Vec::new(),
        body: Vec::new(),
    }
}

fn blocked(r: Result<forgebase_functions::fetch::FetchResponse, FetchError>) -> String {
    match r {
        Err(FetchError::Blocked(m)) => m,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[tokio::test]
async fn private_loopback_link_local_and_metadata_addresses_are_refused() {
    let n = net(names()).await;
    let port = n.mock.port();
    for url in [
        format!("http://127.0.0.1:{port}/ok"),
        format!("http://127.9.9.9:{port}/ok"),
        "http://169.254.169.254/latest/meta-data/".to_owned(),
        "http://10.0.0.5/".to_owned(),
        "http://172.20.1.1/".to_owned(),
        "http://192.168.1.1/".to_owned(),
        format!("http://[::1]:{port}/ok"),
        format!("http://[::ffff:127.0.0.1]:{port}/ok"),
        "http://[fe80::1]/".to_owned(),
        "http://[fd00:ec2::254]/".to_owned(),
        format!("http://0.0.0.0:{port}/ok"),
        format!("http://2130706433:{port}/ok"),
        format!("http://localhost:{port}/ok"),
        "http://internal.test/".to_owned(),
        "http://metadata.test/latest/meta-data/".to_owned(),
        "http://v6loop.test/".to_owned(),
        "http://mapped.test/".to_owned(),
    ] {
        let why = blocked(n.fetcher.fetch(get(&url), &[]).await);
        assert!(!why.is_empty(), "{url}");
    }
    let why = blocked(n.fetcher.fetch(get("http://169.254.169.254/"), &[]).await);
    assert!(why.contains("metadata"), "{why}");
    // One blocked address among several refuses the request.
    let why = blocked(n.fetcher.fetch(get("http://mixed.test/ok"), &[]).await);
    assert!(why.contains("127.0.0.1"), "{why}");
    assert!(n.route.seen.lock().unwrap().is_empty(), "no connection was attempted");

    // Other schemes and credentials in the URL.
    blocked(n.fetcher.fetch(get("file:///etc/passwd"), &[]).await);
    blocked(n.fetcher.fetch(get("gopher://public.test/"), &[]).await);
    blocked(n.fetcher.fetch(get("http://user:pass@public.test/ok"), &[]).await);

    // The public host works.
    let ok = n.fetcher.fetch(get("http://public.test/ok"), &[]).await.unwrap();
    assert_eq!((ok.status, ok.body.as_slice()), (200, b"hello".as_slice()));
}

#[tokio::test]
async fn dns_rebinding_cannot_redirect_the_connection() {
    let resolver = Arc::new(Rebinding::default());
    let n = net(resolver.clone()).await;
    // First lookup is public: the request goes to that vetted address, and
    // the name is not looked up again for the connection.
    let ok = n.fetcher.fetch(get("http://rebind.test/ok"), &[]).await.unwrap();
    assert_eq!(ok.status, 200);
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 1);
    assert_eq!(*n.route.seen.lock().unwrap(), vec![SocketAddr::new(public(), 80)]);
    // Now the name points at loopback: refused before connecting.
    let why = blocked(n.fetcher.fetch(get("http://rebind.test/ok"), &[]).await);
    assert!(why.contains("127.0.0.1"), "{why}");
    assert_eq!(n.route.seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn every_redirect_target_is_checked_again() {
    let n = net(names()).await;
    let why = blocked(n.fetcher.fetch(get("http://public.test/redirect-metadata"), &[]).await);
    assert!(why.contains("169.254.169.254"), "{why}");
    blocked(n.fetcher.fetch(get("http://public.test/redirect-localhost"), &[]).await);
    blocked(n.fetcher.fetch(get("http://public.test/redirect-ten"), &[]).await);
    // Only the public hop was ever connected.
    assert!(n.route.seen.lock().unwrap().iter().all(|a| a.ip() == public()));

    // A redirect to a public target is followed.
    let ok = n
        .fetcher
        .fetch(get("http://public.test/redirect-public"), &[])
        .await
        .unwrap();
    assert_eq!((ok.status, ok.body.as_slice()), (200, b"hello".as_slice()));
    assert!(ok.url.ends_with("/ok"));

    // Redirect loops end.
    assert!(matches!(
        n.fetcher.fetch(get("http://public.test/redirect-loop"), &[]).await,
        Err(FetchError::Failed(m)) if m.contains("redirect")
    ));

    // Credentials do not follow a redirect to another origin.
    let mut r = get("http://public.test/redirect-other");
    r.headers.push(("Authorization".into(), "Bearer top-secret".into()));
    let echoed = n.fetcher.fetch(r, &[]).await.unwrap();
    let text = String::from_utf8(echoed.body).unwrap();
    assert!(text.contains("host: other.test"), "{text}");
    assert!(!text.contains("top-secret"), "{text}");
    // ... but are sent to the origin they were meant for.
    let mut r = get("http://public.test/echo");
    r.headers.push(("Authorization".into(), "Bearer top-secret".into()));
    let text = String::from_utf8(n.fetcher.fetch(r, &[]).await.unwrap().body).unwrap();
    assert!(text.contains("top-secret"));
}

#[tokio::test]
async fn fetch_hosts_allow_list_is_enforced_on_every_hop() {
    let n = net(names()).await;
    let allowed = vec!["public.test".to_owned()];
    assert_eq!(
        n.fetcher
            .fetch(get("http://public.test/ok"), &allowed)
            .await
            .unwrap()
            .status,
        200
    );
    let why = blocked(n.fetcher.fetch(get("http://other.test/ok"), &allowed).await);
    assert!(why.contains("fetch_hosts"), "{why}");
    blocked(
        n.fetcher
            .fetch(get("http://public.test/redirect-other"), &allowed)
            .await,
    );
}

#[tokio::test]
async fn functions_see_the_same_refusals() {
    let n = net(names()).await;
    let h = harness_with(FunctionsConfig {
        fetcher: Some(n.fetcher.clone()),
        ..FunctionsConfig::default()
    })
    .await;
    // {"url": "<env TARGET>"} → fetch → output the result or the error.
    let mut w = Wat::new(&["fetch", "env_get"]);
    let target = w.s("TARGET");
    let pre = w.s(r#"{"url":""#);
    let post = w.s(r#""}"#);
    let body = format!(
        "(local.set $r (call $env_get {target}))
         (drop (call $result_read (i32.const {aux}) (local.get $r)))
         (local.set $p (call $copy (i32.const {args}) {pre}))
         (local.set $p (call $copy (local.get $p) (i32.const {aux}) (local.get $r)))
         (local.set $p (call $copy (local.get $p) {post}))
         (local.set $r (call $fetch (i32.const {args}) (i32.sub (local.get $p) (i32.const {args}))))
         (if (i32.lt_s (local.get $r) (i32.const 0)) (then (local.set $r (i32.sub (i32.const 0) (local.get $r)))))
         (drop (call $result_read (i32.const {res}) (local.get $r)))
         (call $emit (i32.const {res}) (local.get $r))
         (i32.const 0)",
        aux = common::AUX,
        args = common::ARGS,
        res = common::RES
    );
    let wasm = w.wasm(&body);
    let run = |target: &str| {
        let mut r = req("fetcher", &wasm, &["fetch", "env"]);
        r.env.insert("TARGET".into(), target.into());
        r
    };
    for (target, code) in [
        ("http://127.0.0.1/", "fetch_blocked"),
        (
            "http://169.254.169.254/latest/meta-data/iam/security-credentials/",
            "fetch_blocked",
        ),
        ("http://10.0.0.1/", "fetch_blocked"),
        ("http://[::1]/", "fetch_blocked"),
        ("http://public.test/redirect-metadata", "fetch_blocked"),
    ] {
        h.deploy(run(target)).await;
        let out: Json = h
            .svc
            .invoke("fetcher", Trigger::Direct, &json!({}), None)
            .await
            .unwrap()
            .json()
            .unwrap()
            .unwrap();
        assert_eq!(out["code"], code, "{target}: {out}");
    }
    h.deploy(run("http://public.test/ok")).await;
    let out: Json = h
        .svc
        .invoke("fetcher", Trigger::Direct, &json!({}), None)
        .await
        .unwrap()
        .json()
        .unwrap()
        .unwrap();
    assert_eq!(
        (out["status"].as_u64(), out["body"].as_str()),
        (Some(200), Some("hello")),
        "{out}"
    );
}
