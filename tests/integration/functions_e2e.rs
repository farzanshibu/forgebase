//! Black-box tests of `forgebase functions deploy|logs` (FB-62) against a real
//! `forgebase serve`: deploy a hand-assembled WebAssembly module, invoke it,
//! read its logs (also with `--follow`), and the failure messages.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use common::{free_port, startup_timeout, wait_healthy};
use serde_json::{Value, json};

/// `(module (memory (export "memory") 1) (func (export "handle") (result i32) i32.const 0))`
const HANDLE_WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, // magic, version
    0x01, 0x05, 0x01, 0x60, 0x00, 0x01, 0x7f, // type () -> i32
    0x03, 0x02, 0x01, 0x00, // one function of type 0
    0x05, 0x03, 0x01, 0x00, 0x01, // memory, 1 page
    0x07, 0x13, 0x02, 0x06, b'm', b'e', b'm', b'o', b'r', b'y', 0x02, 0x00, 0x06, b'h', b'a', b'n', b'd', b'l', b'e',
    0x00, 0x00, // exports
    0x0a, 0x06, 0x01, 0x04, 0x00, 0x41, 0x00, 0x0b, // body: i32.const 0
];

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
fn deploy_invoke_and_read_logs_through_the_cli() {
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

    let wasm = project.join("hello.wasm");
    std::fs::write(&wasm, HANDLE_WASM).unwrap();
    let wasm_s = wasm.to_str().unwrap();

    let port = free_port();
    let server = Serve::start(&project, port);
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

    // No credential: the server's answer and the hint reach the user.
    let o = run(&project, &["functions", "--server", &url, "deploy", "hello", wasm_s]);
    assert!(!o.status.success());
    let err = text(&o.stderr);
    assert!(
        err.contains("unauthenticated") && err.contains("--token") && err.contains("HTTP 401"),
        "{err}"
    );

    // Deploy v1 (token and URL from flags), v2 inactive (token and URL from the environment).
    let out = ok(&run(
        &project,
        &[
            "functions",
            "--server",
            &url,
            "--token",
            &token,
            "deploy",
            "hello",
            wasm_s,
            "--capability",
            "log",
            "--description",
            "says hi",
            "--env",
            "MODE=test",
            "--timeout-ms",
            "2000",
        ],
    ));
    assert!(out.contains("deployed hello v1") && out.contains("active"), "{out}");
    let out = ok(&forgebase(&project)
        .args(["functions", "deploy", "hello", wasm_s, "--no-activate"])
        .env("FB_URL", &url)
        .env("FB_TOKEN", &token)
        .output()
        .unwrap());
    assert!(
        out.contains("deployed hello v2") && out.contains("not active (v1 still serves)"),
        "{out}"
    );

    // The server agrees with what the CLI said.
    let (s, j) = call(&addr, "GET", "/api/v1/functions/hello", Some(&token), None);
    assert_eq!(s, 200, "{j}");
    assert_eq!(
        (j["active_version"].as_i64(), j["latest_version"].as_i64()),
        (Some(1), Some(2))
    );
    assert_eq!(j["description"], "says hi");
    let (_, j) = call(&addr, "GET", "/api/v1/functions/hello/versions", Some(&token), None);
    assert_eq!(j["items"][1]["limits"]["timeout_ms"], 2000);
    assert_eq!(j["items"][1]["env"]["MODE"], "test");

    // A module the server rejects reports the server's code.
    let bad = project.join("empty.wasm");
    std::fs::write(&bad, b"\0asm\x01\0\0\0").unwrap();
    let o = run(
        &project,
        &[
            "functions",
            "--server",
            &url,
            "--token",
            &token,
            "deploy",
            "broken",
            bad.to_str().unwrap(),
        ],
    );
    assert!(!o.status.success());
    assert!(text(&o.stderr).contains("invalid_module"), "{}", text(&o.stderr));

    // Logs: nothing yet, then one execution per invocation.
    let logs = |extra: &[&str]| {
        let mut args = vec!["functions", "--server", &url, "--token", &token, "logs", "hello"];
        args.extend_from_slice(extra);
        ok(&run(&project, &args))
    };
    assert_eq!(logs(&[]).trim(), "");
    let (s, _) = call(
        &addr,
        "POST",
        "/api/v1/functions/hello/invoke",
        Some(&token),
        Some(json!({})),
    );
    assert!(s == 200 || s == 204, "invoke answered {s}");
    let out = logs(&[]);
    assert!(out.contains("ok") && out.contains("v1 http"), "{out}");
    let json_out = logs(&["--json", "-n", "5"]);
    let first: Value = serde_json::from_str(json_out.lines().next().unwrap()).unwrap();
    assert_eq!(
        (first["function"].as_str(), first["version"].as_i64()),
        (Some("hello"), Some(1))
    );
    let seen_id = first["id"].as_i64().unwrap();

    // --follow: the existing line first, then the next invocation's, then Ctrl-C (kill).
    let mut child = forgebase(&project)
        .args([
            "functions",
            "--server",
            &url,
            "--token",
            &token,
            "logs",
            "hello",
            "--follow",
            "--interval",
            "0.2",
            "--json",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });
    let line = rx.recv_timeout(Duration::from_secs(10)).expect("initial line");
    assert_eq!(
        serde_json::from_str::<Value>(&line).unwrap()["id"].as_i64(),
        Some(seen_id)
    );
    let (s, _) = call(
        &addr,
        "POST",
        "/api/v1/functions/hello/invoke",
        Some(&token),
        Some(json!({})),
    );
    assert!(s == 200 || s == 204, "invoke answered {s}");
    let line = rx.recv_timeout(Duration::from_secs(10)).expect("followed line");
    assert!(serde_json::from_str::<Value>(&line).unwrap()["id"].as_i64().unwrap() > seen_id);
    child.kill().unwrap();
    child.wait().unwrap();

    // Without --server the configured bind address is used.
    let o = run(
        &project,
        &[
            "--port",
            &port.to_string(),
            "functions",
            "--token",
            &token,
            "logs",
            "hello",
            "-n",
            "1",
        ],
    );
    assert!(ok(&o).contains("v1 http"));

    // Unknown function: the server's 404 is shown.
    let o = run(
        &project,
        &["functions", "--server", &url, "--token", &token, "logs", "nope"],
    );
    assert!(!o.status.success());
    assert!(text(&o.stderr).contains("HTTP 404"), "{}", text(&o.stderr));
}

#[test]
fn new_writes_a_typescript_function_that_deploys_and_runs_as_is() {
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

    // `functions new` needs no server.
    let out = ok(&run(&project, &["functions", "new", "hello"]));
    assert!(out.contains("index.ts") && out.contains("forgebase.d.ts"), "{out}");
    let index = project.join("functions/hello/index.ts");
    assert!(index.is_file() && project.join("functions/hello/forgebase.d.ts").is_file());
    let o = run(&project, &["functions", "new", "hello"]);
    assert!(
        !o.status.success() && text(&o.stderr).contains("--force"),
        "{}",
        text(&o.stderr)
    );
    let out = ok(&run(&project, &["functions", "new", "plain", "--js"]));
    assert!(out.contains("index.js"), "{out}");

    let port = free_port();
    let server = Serve::start(&project, port);
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

    // The template deploys and runs unchanged.
    let index_s = index.to_str().unwrap();
    let out = ok(&run(
        &project,
        &[
            "functions",
            "--server",
            &url,
            "--token",
            &token,
            "deploy",
            "hello",
            index_s,
            "--access",
            "public",
            "--capability",
            "log",
        ],
    ));
    assert!(
        out.contains("deployed hello v1 (typescript,") && out.contains("active"),
        "{out}"
    );
    let (s, j) = call(&addr, "GET", "/api/v1/functions/hello/invoke?name=you", None, None);
    assert_eq!(s, 200, "{j}");
    assert_eq!(j, json!({"message": "Hello, you!", "caller": null}));
    let (s, j) = call(&addr, "GET", "/api/v1/functions/hello/versions", Some(&token), None);
    assert_eq!(s, 200);
    assert_eq!(j["items"][0]["runtime"], "js");
    let logs = ok(&run(
        &project,
        &["functions", "--server", &url, "--token", &token, "logs", "hello"],
    ));
    assert!(logs.contains("greeting you"), "{logs}");

    // The JavaScript template too.
    let js = project.join("functions/plain/index.js");
    let out = ok(&run(
        &project,
        &[
            "functions",
            "--server",
            &url,
            "--token",
            &token,
            "deploy",
            "plain",
            js.to_str().unwrap(),
            "--access",
            "public",
            "--capability",
            "log",
        ],
    ));
    assert!(out.contains("(javascript,"), "{out}");
    let (s, j) = call(&addr, "GET", "/api/v1/functions/plain/invoke", None, None);
    assert_eq!((s, j["message"].clone()), (200, json!("Hello, world!")));

    // A compile error names the line and column.
    let broken = project.join("broken.ts");
    std::fs::write(
        &broken,
        "import { serve } from \"forgebase\";\nserve(() => {\n  return new Response(;\n});\n",
    )
    .unwrap();
    let o = run(
        &project,
        &[
            "functions",
            "--server",
            &url,
            "--token",
            &token,
            "deploy",
            "broken",
            broken.to_str().unwrap(),
        ],
    );
    assert!(!o.status.success());
    let err = text(&o.stderr);
    assert!(
        err.contains("function_compile_failed") && err.contains("line 3"),
        "{err}"
    );
}

#[test]
fn deploy_refuses_a_missing_or_non_wasm_file_before_connecting() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    // Nothing listens on this port: the file check must fail first.
    let port = free_port().to_string();
    let o = run(&root, &["--port", &port, "functions", "deploy", "hello", "nope.wasm"]);
    assert!(!o.status.success());
    assert!(text(&o.stderr).contains("cannot read nope.wasm"), "{}", text(&o.stderr));
    std::fs::write(root.join("a.wat"), "(module)").unwrap();
    let o = run(&root, &["--port", &port, "functions", "deploy", "hello", "a.wat"]);
    assert!(
        text(&o.stderr).contains("not a WebAssembly binary"),
        "{}",
        text(&o.stderr)
    );
    let o = run(&root, &["--port", &port, "functions", "--token", "t", "logs", "hello"]);
    assert!(
        text(&o.stderr).contains("cannot reach the server"),
        "{}",
        text(&o.stderr)
    );
}
