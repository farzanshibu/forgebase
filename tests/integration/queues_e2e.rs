//! Black-box tests of `forgebase queues publish|pull|tail` and `forgebase jobs watch` (FB-65)
//! against a real `forgebase serve`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;

use common::{free_port, startup_timeout, wait_healthy};
use serde_json::{Value, json};

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn forgebase(cwd: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_forgebase"));
    cmd.current_dir(cwd);
    for (k, _) in std::env::vars_os() {
        let k = k.to_string_lossy().into_owned();
        if k.starts_with("FORGEBASE_") || k.starts_with("FB_") {
            cmd.env_remove(k);
        }
    }
    cmd.env("NO_COLOR", "1");
    common::backend_env(&mut cmd);
    cmd
}

fn run(cwd: &Path, args: &[&str]) -> Output {
    forgebase(cwd).args(common::backend_args(cwd, args)).output().unwrap()
}

struct Serve {
    child: Child,
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
            child,
            addr: format!("127.0.0.1:{port}"),
        };
        assert!(
            wait_healthy(&s.addr, startup_timeout()),
            "server did not become healthy"
        );
        s
    }
}

impl Drop for Serve {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

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
    let t = text(&buf);
    let (h, b) = t.split_once("\r\n\r\n").unwrap_or((&t, ""));
    let status = h
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    (status, serde_json::from_str(b).unwrap_or(Value::Null))
}

fn ok(o: &Output) -> String {
    assert!(
        o.status.success(),
        "stderr: {}\nstdout: {}",
        text(&o.stderr),
        text(&o.stdout)
    );
    text(&o.stdout)
}

#[test]
fn queues_and_jobs_through_the_cli() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    assert!(run(&root, &["init", "app"]).status.success());
    let project = root.join("app");
    let mut create = forgebase(&project)
        .args(["admin", "create", "--email", "root@example.com", "--password-stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    create.stdin.take().unwrap().write_all(b"root password 1\n").unwrap();
    assert!(create.wait_with_output().unwrap().status.success());

    let server = Serve::start(&project, free_port());
    let addr = server.addr.clone();
    let url = format!("http://{addr}");
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
        "/api/v1/queues/topics",
        Some(&token),
        Some(json!({"name": "orders"})),
    );
    assert_eq!(s, 201, "{j}");
    let (s, j) = call(
        &addr,
        "POST",
        "/api/v1/queues/subscriptions",
        Some(&token),
        Some(json!({"name": "billing", "topic": "orders", "start": "earliest"})),
    );
    assert_eq!(s, 201, "{j}");

    let q = |args: &[&str]| {
        let mut a = vec!["queues", "--server", &url, "--token", &token];
        a.extend_from_slice(args);
        run(&project, &a)
    };

    // Publish: data from the argument, with attributes; a plain string with --text.
    let out = ok(&q(&[
        "publish",
        "orders",
        r#"{"order":42}"#,
        "--attr",
        "kind=rush",
        "--json",
    ]));
    let v: Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(v["results"][0]["duplicate"], false, "{out}");
    ok(&q(&["publish", "orders", "hello", "--text"]));

    // Tail shows the retained messages, oldest first, as NDJSON.
    let out = ok(&q(&["tail", "orders", "--json"]));
    let rows: Vec<Value> = out.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(rows.len(), 2, "{out}");
    assert_eq!(rows[0]["data"]["order"], 42);
    assert_eq!(rows[1]["data"], "hello");

    // Pull leases the messages; --ack settles them, so a second pull is empty.
    let out = ok(&q(&["pull", "billing", "-n", "5", "--ack", "--json"]));
    assert_eq!(out.lines().count(), 2, "{out}");
    let out = ok(&q(&["pull", "billing", "--json"]));
    assert_eq!(out.trim(), "", "{out}");

    // Errors carry the server's code.
    let o = q(&["publish", "nope", "1"]);
    assert!(!o.status.success());
    assert!(text(&o.stderr).contains("not_found"), "{}", text(&o.stderr));

    // jobs watch: a job that is already finished prints its state and exits 0; a dead one exits 1.
    let (s, job) = call(
        &addr,
        "POST",
        "/api/v1/jobs",
        Some(&token),
        Some(json!({"kind": "noop", "max_attempts": 1})),
    );
    assert!(s == 200 || s == 201, "{job}");
    let id = job["id"].as_str().unwrap().to_owned();
    let (s, _) = call(&addr, "POST", &format!("/api/v1/jobs/{id}/cancel"), Some(&token), None);
    assert!(s == 200 || s == 204);
    let o = run(
        &project,
        &[
            "jobs",
            "--server",
            &url,
            "--token",
            &token,
            "watch",
            &id,
            "--interval",
            "0.2",
            "--timeout",
            "20",
        ],
    );
    assert_eq!(o.status.code(), Some(1), "{}{}", text(&o.stdout), text(&o.stderr));
    assert!(text(&o.stdout).contains("cancelled"), "{}", text(&o.stdout));
}
