//! Black-box tests of authentication in the `forgebase` binary (FB-20):
//! `forgebase admin create`, then sign-in and rejection over real HTTP,
//! and persistence across a restart (SPEC §27 #7, #8, #21).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;

use common::{free_port, wait_healthy};
use serde_json::{Value, json};

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

fn run(cwd: &Path, args: &[&str]) -> Output {
    forgebase(cwd).args(args).output().unwrap()
}

fn run_stdin(cwd: &Path, args: &[&str], input: &str) -> Output {
    let mut child = forgebase(cwd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
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
            wait_healthy(&s.addr, Duration::from_secs(20)),
            "server did not become healthy"
        );
        s
    }

    fn stop(mut self) {
        let mut child = self.child.take().unwrap();
        #[cfg(unix)]
        {
            Command::new("kill")
                .args(["-TERM", &child.id().to_string()])
                .status()
                .unwrap();
        }
        #[cfg(not(unix))]
        child.kill().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while child.try_wait().unwrap().is_none() {
            assert!(std::time::Instant::now() < deadline, "serve did not stop");
            std::thread::sleep(Duration::from_millis(50));
        }
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
fn call(addr: &str, method: &str, path: &str, bearer: Option<&str>, body: Option<Value>) -> (u16, String, Value) {
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
    (status, h.to_lowercase(), serde_json::from_str(b).unwrap_or(Value::Null))
}

#[test]
fn admin_bootstrap_login_and_rejection_over_http() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    assert!(run(&root, &["init", "app"]).status.success());
    let project = root.join("app");

    let out = run(&project, &["admin", "list"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(text(&out.stdout).contains("no administrators"));

    let out = run_stdin(
        &project,
        &["admin", "create", "--email", "Root@Example.com", "--password-stdin"],
        "root password 1\n",
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(text(&out.stdout).contains("created administrator root@example.com"));
    let out = run_stdin(
        &project,
        &["admin", "create", "--email", "root@example.com", "--password-stdin"],
        "root password 1\n",
    );
    assert!(!out.status.success());
    assert!(text(&out.stderr).contains("already exists"), "{}", text(&out.stderr));
    let out = run_stdin(
        &project,
        &["admin", "create", "--email", "x@example.com", "--password-stdin"],
        "short\n",
    );
    assert!(!out.status.success());
    assert!(text(&out.stderr).contains("at least 10"), "{}", text(&out.stderr));
    let out = run(&project, &["admin", "create", "--email", "x@example.com"]);
    assert!(!out.status.success());

    // The master key exists and is private.
    let key = project.join("forgebase-data/auth/master.key");
    assert!(key.is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&key).unwrap().permissions().mode() & 0o777, 0o600);
    }

    let port = free_port();
    let server = Serve::start(&project, port);
    let addr = server.addr.clone();

    // #8: anonymous and bad credentials are rejected with the envelope.
    let (s, h, j) = call(&addr, "GET", "/api/auth/me", None, None);
    assert_eq!(s, 401, "{j}");
    assert_eq!(j["error"]["code"], "unauthenticated");
    assert!(h.contains("x-request-id"));
    assert!(j["error"]["request_id"].as_str().is_some_and(|r| !r.is_empty()));
    let (s, _, j) = call(&addr, "GET", "/api/auth/me", Some("not-a-token"), None);
    assert_eq!((s, j["error"]["code"].as_str()), (401, Some("invalid_token")));
    let (s, _, j) = call(
        &addr,
        "POST",
        "/api/auth/admins/login",
        None,
        Some(json!({"email": "root@example.com", "password": "wrong password"})),
    );
    assert_eq!((s, j["error"]["code"].as_str()), (401, Some("invalid_credentials")));

    // #7: the bootstrapped administrator signs in.
    let (s, _, j) = call(
        &addr,
        "POST",
        "/api/auth/admins/login",
        None,
        Some(json!({"email": "root@example.com", "password": "root password 1"})),
    );
    assert_eq!(s, 200, "{j}");
    let admin = j["access_token"].as_str().unwrap().to_owned();
    let (s, _, j) = call(&addr, "GET", "/api/auth/me", Some(&admin), None);
    assert_eq!(s, 200, "{j}");
    assert_eq!(j["subject"]["type"], "admin");
    assert_eq!(j["admin"]["email"], "root@example.com");

    // End users in the default `users` collection.
    let (s, _, j) = call(
        &addr,
        "POST",
        "/api/auth/signup",
        None,
        Some(json!({"email": "user@example.com", "password": "user password"})),
    );
    assert_eq!(s, 201, "{j}");
    let (s, _, j) = call(
        &addr,
        "POST",
        "/api/auth/login",
        None,
        Some(json!({"email": "user@example.com", "password": "user password"})),
    );
    assert_eq!(s, 200, "{j}");
    let user = j["access_token"].as_str().unwrap().to_owned();
    let refresh = j["refresh_token"].as_str().unwrap().to_owned();
    let (s, _, _) = call(&addr, "GET", "/api/auth/audit", Some(&user), None);
    assert_eq!(s, 403);
    let (s, _, j) = call(&addr, "GET", "/api/auth/audit?event=login", Some(&admin), None);
    assert_eq!(s, 200, "{j}");
    assert!(
        j["items"].as_array().unwrap().iter().any(|e| e["ip"] == "127.0.0.1"),
        "client IP recorded: {j}"
    );

    // The CLI works while the server runs.
    let out = run(&project, &["admin", "list", "--json"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let list: Value = serde_json::from_str(&text(&out.stdout)).unwrap();
    assert!(list["admins"][0]["last_login_at"].is_string());

    // Restart: sessions, keys, users and admins persist.
    server.stop();
    let server = Serve::start(&project, port);
    let (s, _, j) = call(&server.addr, "GET", "/api/auth/me", Some(&admin), None);
    assert_eq!(s, 200, "{j}");
    let (s, _, j) = call(
        &server.addr,
        "POST",
        "/api/auth/refresh",
        None,
        Some(json!({"refresh_token": refresh})),
    );
    assert_eq!(s, 200, "{j}");
    let (s, _, _) = call(
        &server.addr,
        "GET",
        "/api/auth/me",
        Some(j["access_token"].as_str().unwrap()),
        None,
    );
    assert_eq!(s, 200);
    let _ = user;

    // Password recovery from the CLI ends the administrator's sessions.
    let out = run_stdin(
        &project,
        &[
            "admin",
            "reset-password",
            "--email",
            "root@example.com",
            "--password-stdin",
        ],
        "new root password\n",
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    let (s, _, j) = call(&server.addr, "GET", "/api/auth/me", Some(&admin), None);
    assert_eq!((s, j["error"]["message"].as_str()), (401, Some("session revoked")));
    server.stop();
}
