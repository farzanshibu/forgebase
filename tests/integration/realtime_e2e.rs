//! Black-box realtime test of the `forgebase` binary (FB-40): SPEC §27 #16
//! and #17 over a real WebSocket, graceful shutdown with open sockets, and
//! replay of the persisted outbox after a restart (#21).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;

use common::{free_port, startup_timeout, wait_healthy};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

fn forgebase(cwd: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_forgebase"));
    cmd.current_dir(cwd);
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("FORGEBASE_") {
            cmd.env_remove(k);
        }
    }
    cmd.env("NO_COLOR", "1");
    cmd
}

fn run(cwd: &Path, args: &[&str], input: Option<&str>) -> Output {
    let mut child = forgebase(cwd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(i) = input {
        child.stdin.take().unwrap().write_all(i.as_bytes()).unwrap();
    }
    child.wait_with_output().unwrap()
}

struct Serve {
    child: Option<Child>,
    addr: String,
}

impl Serve {
    fn start(cwd: &Path, port: u16) -> Self {
        let child = forgebase(cwd)
            .args(["serve", "--port", &port.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let s = Self {
            child: Some(child),
            addr: format!("127.0.0.1:{port}"),
        };
        assert!(
            wait_healthy(&s.addr, startup_timeout()),
            "server did not become healthy"
        );
        s
    }

    /// SIGTERM, then waits for exit; returns how long the shutdown took.
    fn stop(mut self) -> Duration {
        let mut child = self.child.take().unwrap();
        let start = std::time::Instant::now();
        #[cfg(unix)]
        Command::new("kill")
            .args(["-TERM", &child.id().to_string()])
            .status()
            .unwrap();
        #[cfg(not(unix))]
        child.kill().unwrap();
        while child.try_wait().unwrap().is_none() {
            assert!(start.elapsed() < Duration::from_secs(20), "serve did not stop");
            std::thread::sleep(Duration::from_millis(50));
        }
        start.elapsed()
    }
}

impl Drop for Serve {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// One HTTP/1.1 request with an optional bearer token and JSON body.
fn call(addr: &str, method: &str, path: &str, bearer: Option<&str>, body: Option<Value>) -> (u16, Value) {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let body = body.map(|b| b.to_string()).unwrap_or_default();
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
    if let Some(t) = bearer {
        head.push_str(&format!("Authorization: Bearer {t}\r\n"));
    }
    if !body.is_empty() {
        head.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            body.len()
        ));
    }
    head.push_str("\r\n");
    s.write_all(head.as_bytes()).unwrap();
    s.write_all(body.as_bytes()).unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).unwrap();
    let t = String::from_utf8_lossy(&buf).into_owned();
    let (h, b) = t.split_once("\r\n\r\n").unwrap_or((&t, ""));
    let status = h
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    (status, serde_json::from_str(b).unwrap_or(Value::Null))
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(addr: &str, token: &str) -> Ws {
    let mut req = format!("ws://{addr}/api/v1/realtime/ws").into_client_request().unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    let (ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    ws
}

/// Next JSON message, or `Err(close code)`.
async fn next(ws: &mut Ws) -> Result<Value, Option<u16>> {
    loop {
        let m = tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .expect("no message within 10 s");
        match m {
            Some(Ok(Message::Text(t))) => return Ok(serde_json::from_str(t.as_str()).unwrap()),
            Some(Ok(Message::Close(f))) => return Err(f.map(|f| u16::from(f.code))),
            Some(Ok(_)) => continue,
            Some(Err(_)) | None => return Err(None),
        }
    }
}

async fn send(ws: &mut Ws, v: Value) {
    ws.send(Message::Text(v.to_string().into())).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn realtime_events_over_the_binary() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    assert!(run(&root, &["init", "app"], None).status.success());
    let project = root.join("app");
    let out = run(
        &project,
        &["admin", "create", "--email", "root@example.com", "--password-stdin"],
        Some("root password 1\n"),
    );
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let port = free_port();
    let server = Serve::start(&project, port);
    let addr = server.addr.clone();
    let (s, j) = call(
        &addr,
        "POST",
        "/api/auth/admins/login",
        None,
        Some(json!({"email": "root@example.com", "password": "root password 1"})),
    );
    assert_eq!(s, 200, "{j}");
    let token = j["access_token"].as_str().unwrap().to_owned();
    let (s, j) = call(
        &addr,
        "POST",
        "/api/v1/collections",
        Some(&token),
        Some(json!({"name": "notes", "fields": [{"name": "text", "type": "text"}]})),
    );
    assert_eq!(s, 201, "{j}");

    // Anonymous callers cannot subscribe (default authorizer fails closed).
    let (s, j) = call(&addr, "GET", "/api/v1/realtime/stats", None, None);
    assert_eq!(s, 401, "{j}");

    // #16: subscribe.
    let mut ws = connect(&addr, &token).await;
    assert_eq!(next(&mut ws).await.unwrap()["type"], "welcome");
    send(&mut ws, json!({"type": "subscribe", "id": "n", "collection": "notes"})).await;
    let ok = next(&mut ws).await.unwrap();
    assert_eq!(ok["type"], "subscribed", "{ok}");

    // #17: a committed change made over REST arrives.
    let (s, rec) = call(
        &addr,
        "POST",
        "/api/v1/collections/notes/records",
        Some(&token),
        Some(json!({"text": "hello"})),
    );
    assert_eq!(s, 201, "{rec}");
    let e = next(&mut ws).await.unwrap();
    assert_eq!(e["type"], "record.created", "{e}");
    assert_eq!(e["record_id"], rec["id"]);
    assert_eq!(e["data"]["text"], "hello");
    let cursor = e["cursor"].as_i64().unwrap();
    let (s, stats) = call(&addr, "GET", "/api/v1/realtime/stats", Some(&token), None);
    assert_eq!(s, 200, "{stats}");
    assert_eq!(stats["connections"], 1);
    assert!(stats["events_delivered"].as_u64().unwrap() >= 1);

    // Graceful shutdown with a socket open: the client gets 1001 and the
    // server stops promptly.
    let stopping = std::thread::spawn(move || server.stop());
    assert_eq!(next(&mut ws).await.err(), Some(Some(1001)));
    let took = stopping.join().unwrap();
    assert!(took < Duration::from_secs(10), "shutdown took {took:?}");

    // #21: the outbox survives a restart; replay from before the event.
    let server = Serve::start(&project, port);
    let mut ws = connect(&server.addr, &token).await;
    assert_eq!(next(&mut ws).await.unwrap()["type"], "welcome");
    send(
        &mut ws,
        json!({"type": "subscribe", "id": "n", "collection": "notes", "cursor": cursor - 1}),
    )
    .await;
    let e = next(&mut ws).await.unwrap();
    assert_eq!(e["type"], "record.created", "{e}");
    assert_eq!(e["record_id"], rec["id"]);
    assert_eq!(e["cursor"], cursor);
    let ok = next(&mut ws).await.unwrap();
    assert_eq!(
        (ok["type"].as_str(), ok["replayed"].as_u64()),
        (Some("subscribed"), Some(1))
    );
    drop(server);
}
