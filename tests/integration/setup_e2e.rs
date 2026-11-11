//! Black-box tests of first-run setup in the `forgebase` binary (FB-90,
//! `docs/requirements/first-run-setup.md` §1–2): the setup link on
//! `forgebase serve`, `GET /api/setup/status`, `POST /api/setup/admin`
//! (token required, wrong, right; 409 after setup; rate limit), the token
//! never reaching the log, `forgebase status` and `forgebase init`'s
//! administrator flags.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use common::{startup_timeout, wait_healthy};
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
    common::backend_env(&mut cmd);
    cmd
}

fn run(cwd: &Path, args: &[&str]) -> Output {
    forgebase(cwd).args(common::backend_args(cwd, args)).output().unwrap()
}

fn run_stdin(cwd: &Path, args: &[&str], input: &str) -> Output {
    let mut child = forgebase(cwd)
        .args(common::backend_args(cwd, args))
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

/// A `forgebase serve --port 0` child: stdout read line by line, stderr
/// (the log) written to a file.
struct Serve {
    child: Option<Child>,
    addr: String,
    lines: mpsc::Receiver<String>,
    stdout: Vec<String>,
}

impl Serve {
    fn start(cwd: &Path, log: &Path) -> Self {
        let log = std::fs::File::create(log).unwrap();
        let mut child = forgebase(cwd)
            // Debug logging: the token must stay out of the log at any level
            // the project's own crates log at.
            .args(["serve", "--port", "0", "--log-level", "debug"])
            .stdout(Stdio::piped())
            .stderr(Stdio::from(log))
            .spawn()
            .unwrap();
        let out = child.stdout.take().unwrap();
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut s = Self {
            child: Some(child),
            addr: String::new(),
            lines,
            stdout: Vec::new(),
        };
        let line = s
            .line_containing("listening on http://", startup_timeout())
            .expect("no listening line");
        s.addr = line[line.find("http://").unwrap() + 7..].trim().to_owned();
        assert!(
            wait_healthy(&s.addr, startup_timeout()),
            "server did not become healthy"
        );
        s
    }

    /// The next stdout line containing `needle`, waiting up to `timeout`.
    fn line_containing(&mut self, needle: &str, timeout: Duration) -> Option<String> {
        if let Some(l) = self.stdout.iter().find(|l| l.contains(needle)) {
            return Some(l.clone());
        }
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            let line = self.lines.recv_timeout(left).ok()?;
            self.stdout.push(line.clone());
            if line.contains(needle) {
                return Some(line);
            }
        }
    }

    /// The setup token from the `First run:` line, if one is printed.
    fn setup_token(&mut self, wait: Duration) -> Option<String> {
        let line = self.line_containing("First run: create the administrator at ", wait)?;
        let url = line.split(" at ").nth(1).unwrap().trim();
        assert!(
            url.starts_with(&format!("http://{}/admin/setup?token=", self.addr)),
            "setup link uses the bound address: {line}"
        );
        Some(url.split("token=").nth(1).unwrap().to_owned())
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

/// One HTTP/1.1 request; returns status, head and JSON body.
fn call(addr: &str, method: &str, path: &str, headers: &[(&str, &str)], body: Option<Value>) -> (u16, String, Value) {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    let body = body.map(|b| b.to_string()).unwrap_or_default();
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
    for (k, v) in headers {
        head.push_str(&format!("{k}: {v}\r\n"));
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
    (status, h.to_owned(), serde_json::from_str(b).unwrap_or(Value::Null))
}

fn setup(addr: &str, body: Value) -> (u16, String, Value) {
    call(addr, "POST", "/api/setup/admin", &[], Some(body))
}

/// `name=value` of a `Set-Cookie` header.
fn cookie(head: &str, name: &str) -> Option<String> {
    head.lines()
        .filter_map(|l| l.split_once(": ").filter(|(k, _)| k.eq_ignore_ascii_case("set-cookie")))
        .map(|(_, v)| v)
        .find(|c| c.starts_with(&format!("{name}=")))
        .map(|c| c.split(';').next().unwrap().to_owned())
}

#[test]
fn serve_prints_a_setup_link_and_the_api_creates_the_first_admin_once() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let out = run(&root, &["init", "app", "--no-admin"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let init = text(&out.stdout);
    assert!(init.contains("admin:     not created (--no-admin)"), "{init}");
    assert!(
        init.contains("the first `forgebase serve` prints a one-time setup link"),
        "{init}"
    );
    let project = root.join("app");

    // `forgebase status`: setup pending.
    let out = run(&project, &["status"]);
    assert!(
        text(&out.stdout).contains("  setup:     pending (no administrator)"),
        "{}",
        text(&out.stdout)
    );
    let out = run(&project, &["status", "--json"]);
    let j: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(j["setup_required"], true, "{j}");

    let log = root.join("serve.log");
    let mut server = Serve::start(&project, &log);
    let addr = server.addr.clone();
    let token = server.setup_token(startup_timeout()).expect("no setup link on stdout");
    // >= 128 bits, URL-safe.
    assert!(token.len() >= 22, "{token}");
    assert!(
        token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
        "{token}"
    );

    // Status: setup required, no authentication needed.
    let (s, _, j) = call(&addr, "GET", "/api/setup/status", &[], None);
    assert_eq!((s, j), (200, json!({"setup_required": true})));

    // The dashboard page is fetched with the token in the query string.
    let (s, _, _) = call(&addr, "GET", &format!("/admin/setup?token={token}"), &[], None);
    assert_ne!(s, 0);

    // Not a request object: 400 validation_failed (not counted as an attempt).
    let (s, _, j) = setup(&addr, json!("not an object"));
    assert_eq!(
        (s, j["error"]["code"].as_str()),
        (400, Some("validation_failed")),
        "{j}"
    );

    // Token required.
    let (s, h, j) = setup(
        &addr,
        json!({"email": "root@example.com", "password": "root password 1"}),
    );
    assert_eq!(
        (s, j["error"]["code"].as_str()),
        (403, Some("invalid_setup_token")),
        "{j}"
    );
    assert!(j["error"]["request_id"].as_str().is_some_and(|r| !r.is_empty()), "{j}");
    assert!(h.to_lowercase().contains("cache-control: no-store"), "{h}");
    // Wrong token, including one that shares all but the last character
    // (compared in constant time on digests, so no prefix shortcut).
    for wrong in [
        "wrong".to_owned(),
        token[..token.len() - 1].to_owned(),
        format!("{token}x"),
    ] {
        let (s, _, j) = setup(
            &addr,
            json!({"token": wrong, "email": "root@example.com", "password": "root password 1"}),
        );
        assert_eq!(
            (s, j["error"]["code"].as_str()),
            (403, Some("invalid_setup_token")),
            "{j}"
        );
    }
    // Right token, weak password: 400, and the token stays valid.
    let (s, _, j) = setup(
        &addr,
        json!({"token": token, "email": "root@example.com", "password": "short"}),
    );
    assert_eq!(
        (s, j["error"]["code"].as_str()),
        (400, Some("validation_failed")),
        "{j}"
    );
    let (s, _, j) = setup(
        &addr,
        json!({"token": token, "email": "not-an-email", "password": "root password 1"}),
    );
    assert_eq!(
        (s, j["error"]["code"].as_str()),
        (400, Some("validation_failed")),
        "{j}"
    );

    // Right token, twice at once: exactly one administrator is created (201,
    // signed in with a session cookie like the admin login); the other 409.
    let racers: Vec<_> = ["Root@Example.com", "other@example.com"]
        .into_iter()
        .map(|email| {
            let (addr, token) = (addr.clone(), token.clone());
            std::thread::spawn(move || {
                setup(
                    &addr,
                    json!({"token": token, "email": email, "password": "root password 1"}),
                )
            })
        })
        .collect();
    let mut results: Vec<_> = racers.into_iter().map(|t| t.join().unwrap()).collect();
    results.sort_by_key(|r| r.0);
    assert_eq!(results[1].0, 409, "{}", results[1].2);
    assert_eq!(results[1].2["error"]["code"], "setup_complete");
    let (s, h, j) = results.swap_remove(0);
    assert_eq!(s, 201, "{j}");
    assert_eq!(j["subject"]["type"], "admin", "{j}");
    assert!(j["session_id"].as_str().is_some(), "{j}");
    assert!(j.get("access_token").is_none(), "cookie mode by default: {j}");
    let session = cookie(&h, "fb_session").expect("session cookie");
    assert!(cookie(&h, "fb_csrf").is_some(), "{h}");
    assert!(cookie(&h, "fb_admin_device").is_some(), "{h}");
    let (s, _, me) = call(&addr, "GET", "/api/auth/me", &[("Cookie", &session)], None);
    assert_eq!(s, 200, "{me}");
    assert_eq!(me["subject"]["type"], "admin", "{me}");

    // 409 once an administrator exists, whatever the token.
    for t in [token.as_str(), "wrong"] {
        let (s, _, j) = setup(
            &addr,
            json!({"token": t, "email": "second@example.com", "password": "root password 2"}),
        );
        assert_eq!((s, j["error"]["code"].as_str()), (409, Some("setup_complete")), "{j}");
    }
    let (s, _, j) = call(&addr, "GET", "/api/setup/status", &[], None);
    assert_eq!((s, j), (200, json!({"setup_required": false})));

    // The administrator signs in with the admin login.
    let out = run(&project, &["admin", "list", "--json"]);
    let admins: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(admins["admins"].as_array().unwrap().len(), 1, "{admins}");
    let email = admins["admins"][0]["email"].as_str().unwrap().to_owned();
    let (s, _, j) = call(
        &addr,
        "POST",
        "/api/auth/admins/login",
        &[],
        Some(json!({"email": email, "password": "root password 1"})),
    );
    assert_eq!(s, 200, "{j}");

    // Both endpoints are documented.
    let (_, _, doc) = call(&addr, "GET", "/api/openapi.json", &[], None);
    assert!(doc["paths"]["/api/setup/status"]["get"].is_object());
    assert!(doc["paths"]["/api/setup/admin"]["post"]["responses"]["409"].is_object());

    let out = run(&project, &["status"]);
    assert!(
        text(&out.stdout).contains("  setup:     complete"),
        "{}",
        text(&out.stdout)
    );
    server.stop();

    // The token went to standard output only, never to the log.
    let logged = std::fs::read_to_string(&log).unwrap();
    assert!(logged.contains("setup mode"), "the log was captured: {logged}");
    assert!(!logged.contains(&token), "setup token in the log file");

    // With an administrator, serve prints no setup link.
    let mut again = Serve::start(&project, &root.join("serve2.log"));
    assert!(again.line_containing("  data dir:", startup_timeout()).is_some());
    assert!(
        again.setup_token(Duration::from_secs(2)).is_none(),
        "{:?}",
        again.stdout
    );
    again.stop();
}

#[test]
fn a_restart_issues_a_new_token_and_attempts_are_rate_limited() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    // No terminal: init cannot prompt and says so.
    let out = run(&root, &["init", "app"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        text(&out.stdout).contains("admin:     not created (no terminal to prompt on)"),
        "{}",
        text(&out.stdout)
    );
    let project = root.join("app");
    assert!(
        !project.join("forgebase-data/auth/master.key").exists(),
        "init without an administrator leaves the master key to the first start"
    );

    let mut first = Serve::start(&project, &root.join("a.log"));
    let old = first.setup_token(startup_timeout()).unwrap();
    first.stop();
    let mut server = Serve::start(&project, &root.join("b.log"));
    let token = server.setup_token(startup_timeout()).unwrap();
    assert_ne!(old, token, "a restart issues a new token");
    let addr = server.addr.clone();

    // The old token is dead; every attempt counts against the address.
    let body = |t: &str| json!({"token": t, "email": "root@example.com", "password": "root password 1"});
    let (s, _, j) = setup(&addr, body(&old));
    assert_eq!(
        (s, j["error"]["code"].as_str()),
        (403, Some("invalid_setup_token")),
        "{j}"
    );
    for _ in 1..10 {
        let (s, _, _) = setup(&addr, body("guess"));
        assert_eq!(s, 403);
    }
    // The 11th attempt in the minute is refused before the token is checked.
    let (s, h, j) = setup(&addr, body(&token));
    assert_eq!(s, 429, "{j}");
    assert_eq!(j["error"]["code"], "too_many_requests", "{j}");
    assert!(h.to_lowercase().contains("retry-after: "), "{h}");
    let (_, _, j) = call(&addr, "GET", "/api/setup/status", &[], None);
    assert_eq!(j["setup_required"], true, "rate limited attempt created nothing");
    server.stop();
}

#[test]
fn init_creates_the_admin_non_interactively() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();

    // Flag rules.
    let out = run(&root, &["init", "app", "--admin-email", "root@example.com"]);
    assert!(!out.status.success(), "--admin-email needs --admin-password-stdin");
    let out = run(
        &root,
        &[
            "init",
            "app",
            "--admin-email",
            "root@example.com",
            "--admin-password-stdin",
            "--no-admin",
        ],
    );
    assert!(!out.status.success(), "--no-admin conflicts with --admin-email");
    // A weak password is refused before the project is created.
    let out = run_stdin(
        &root,
        &[
            "init",
            "app",
            "--admin-email",
            "root@example.com",
            "--admin-password-stdin",
        ],
        "short\n",
    );
    assert!(!out.status.success());
    assert!(text(&out.stderr).contains("at least 10"), "{}", text(&out.stderr));
    assert!(!root.join("app").exists(), "nothing written on a refused password");

    let args = [
        "init",
        "app",
        "--admin-email",
        "Root@Example.com",
        "--admin-password-stdin",
    ];
    let out = run_stdin(&root, &args, "root password 1\n");
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        text(&out.stdout).contains("  admin:     root@example.com (created)"),
        "{}",
        text(&out.stdout)
    );
    let project = root.join("app");
    let out = run(&project, &["admin", "list", "--json"]);
    let j: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(j["admins"][0]["email"], "root@example.com", "{j}");
    assert_eq!(j["admins"].as_array().unwrap().len(), 1);
    let out = run(&project, &["status"]);
    assert!(
        text(&out.stdout).contains("  setup:     complete"),
        "{}",
        text(&out.stdout)
    );

    // Re-running init keeps the administrator and creates no second one.
    let out = run_stdin(&root, &args, "root password 1\n");
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        text(&out.stdout).contains("  admin:     already set up (kept)"),
        "{}",
        text(&out.stdout)
    );
    let out = run(&project, &["admin", "list", "--json"]);
    let j: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(j["admins"].as_array().unwrap().len(), 1);

    // The administrator signs in on the first serve, which prints no setup link.
    let mut server = Serve::start(&project, &root.join("serve.log"));
    assert!(server.line_containing("  data dir:", startup_timeout()).is_some());
    assert!(
        server.setup_token(Duration::from_secs(2)).is_none(),
        "{:?}",
        server.stdout
    );
    let (s, _, j) = call(
        &server.addr,
        "POST",
        "/api/auth/admins/login",
        &[],
        Some(json!({"email": "root@example.com", "password": "root password 1"})),
    );
    assert_eq!(s, 200, "{j}");
    server.stop();
}
