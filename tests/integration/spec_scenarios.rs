//! FB-85: the 25 minimum scenarios of SPEC §27, end to end, against the
//! `forgebase` binary that cargo just built.
//!
//! Every test starts from nothing: `forgebase init` in a new temporary
//! directory, an administrator created with `forgebase admin create`, then
//! `forgebase serve --port 0` as a child process with an empty `HOME` and no
//! `FORGEBASE_*` variables from the caller. Everything after that goes over
//! real sockets: HTTP/1.1 for the REST API, WebSocket and Server-Sent Events
//! for realtime, and the CLI for backup and restore. Persistence is checked by
//! stopping the process (SIGTERM) and starting it again on the same data
//! directory.
//!
//! The test names are `scenario_NN_<what>` with NN the number in SPEC §27, so
//! `cargo test -p forgebase-cli --test spec_scenarios` runs all 25 and
//! `docs/qa.md` maps each row to one test.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{Value as Json, json};
use sha2::Digest as _;

// ---------------------------------------------------------------------------
// HTTP client: raw HTTP/1.1, one connection per request.
// ---------------------------------------------------------------------------

/// One HTTP response.
struct Resp {
    status: u16,
    /// Header block, lower-cased.
    head: String,
    body: Vec<u8>,
}

impl Resp {
    fn json(&self) -> Json {
        serde_json::from_slice(&self.body).unwrap_or(Json::Null)
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// The `error.code` of an error envelope ("" if none).
    fn code(&self) -> String {
        self.json()["error"]["code"].as_str().unwrap_or_default().to_owned()
    }

    fn header(&self, name: &str) -> Option<String> {
        let name = format!("{}:", name.to_ascii_lowercase());
        self.head
            .lines()
            .find(|l| l.starts_with(&name))
            .map(|l| l[name.len()..].trim().to_owned())
    }

    /// Asserts the status and returns the JSON body.
    fn expect(&self, status: u16) -> Json {
        assert_eq!(self.status, status, "unexpected status; body: {}", self.text());
        self.json()
    }
}

fn dechunk(mut b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let Some(nl) = b.windows(2).position(|w| w == b"\r\n") else {
            return out;
        };
        let size_txt = String::from_utf8_lossy(&b[..nl]);
        let size = usize::from_str_radix(size_txt.split(';').next().unwrap().trim(), 16).unwrap_or(0);
        b = &b[nl + 2..];
        if size == 0 || b.len() < size {
            out.extend_from_slice(&b[..size.min(b.len())]);
            return out;
        }
        out.extend_from_slice(&b[..size]);
        b = b.get(size + 2..).unwrap_or_default();
    }
}

/// Sends one request and reads the whole response.
fn raw(addr: &str, method: &str, path: &str, headers: &[(&str, String)], body: &[u8]) -> Resp {
    let mut s = TcpStream::connect(addr).unwrap_or_else(|e| panic!("connect {addr}: {e}"));
    s.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
    for (k, v) in headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    if !body.is_empty() || matches!(method, "POST" | "PUT" | "PATCH") {
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    head.push_str("\r\n");
    s.write_all(head.as_bytes()).unwrap();
    // The server may answer (413, 401) before reading a large body.
    let _ = s.write_all(body);
    let mut buf = Vec::new();
    let _ = s.read_to_end(&mut buf);
    let split = buf.windows(4).position(|w| w == b"\r\n\r\n").unwrap_or(buf.len());
    let head = String::from_utf8_lossy(&buf[..split]).to_ascii_lowercase();
    let rest = buf.get(split + 4..).unwrap_or_default();
    let status = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let body = if head.contains("transfer-encoding: chunked") {
        dechunk(rest)
    } else {
        rest.to_vec()
    };
    Resp { status, head, body }
}

fn enc(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn sha256_hex(data: &[u8]) -> String {
    sha2::Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

fn hmac_sha256_hex(key: &[u8], msg: &[u8]) -> String {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&sha2::Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = sha2::Sha256::new();
    inner.update(k.iter().map(|b| b ^ 0x36).collect::<Vec<_>>());
    inner.update(msg);
    let mut outer = sha2::Sha256::new();
    outer.update(k.iter().map(|b| b ^ 0x5c).collect::<Vec<_>>());
    outer.update(inner.finalize());
    outer.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn now_secs() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap()
}

fn wait_for<T>(what: &str, timeout: Duration, mut f: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(start.elapsed() < timeout, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

// ---------------------------------------------------------------------------
// One ForgeBase instance: a fresh project and a `forgebase serve` process.
// ---------------------------------------------------------------------------

const ADMIN_EMAIL: &str = "root@example.com";
const ADMIN_PASSWORD: &str = "root password 1";
const USER_PASSWORD: &str = "user password 1";

struct Fb {
    tmp: tempfile::TempDir,
    proj: PathBuf,
    env: Vec<(String, String)>,
    child: Option<Child>,
    addr: String,
    admin: String,
    starts: u32,
}

impl Fb {
    /// A new project with a running server and a signed-in administrator.
    fn start() -> Self {
        Self::start_with(&[])
    }

    /// Same, with extra `FORGEBASE_*` settings for the server process.
    fn start_with(env: &[(&str, &str)]) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("home")).unwrap();
        let mut fb = Self {
            proj: root.join("app"),
            tmp,
            env: env.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect(),
            child: None,
            addr: String::new(),
            admin: String::new(),
            starts: 0,
        };
        let out = fb.cli_in(&root, &["init", "app"], None);
        assert!(out.status.success(), "init: {}", text(&out.stderr));
        assert!(fb.proj.join("forgebase.toml").is_file(), "init wrote the config file");
        let out = fb.cli(
            &["admin", "create", "--email", ADMIN_EMAIL, "--password-stdin"],
            Some(&format!("{ADMIN_PASSWORD}\n")),
        );
        assert!(out.status.success(), "admin create: {}", text(&out.stderr));
        fb.serve();
        fb.admin = fb.admin_login();
        fb
    }

    fn data_dir(&self) -> PathBuf {
        self.proj.join("forgebase-data")
    }

    fn command(&self, cwd: &Path) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_forgebase"));
        cmd.current_dir(cwd);
        for (k, _) in std::env::vars_os() {
            if k.to_string_lossy().starts_with("FORGEBASE_") {
                cmd.env_remove(k);
            }
        }
        // A clean machine: no state in the user's home directory.
        cmd.env("HOME", self.tmp.path().join("home"));
        cmd.env("NO_COLOR", "1");
        common::backend_env(&mut cmd);
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        cmd
    }

    fn cli_in(&self, cwd: &Path, args: &[&str], stdin: Option<&str>) -> Output {
        let mut child = self
            .command(cwd)
            .args(common::backend_args(cwd, args))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut input = child.stdin.take().unwrap();
        if let Some(s) = stdin {
            input.write_all(s.as_bytes()).unwrap();
        }
        drop(input);
        child.wait_with_output().unwrap()
    }

    /// Runs `forgebase <args>` in the project directory.
    fn cli(&self, args: &[&str], stdin: Option<&str>) -> Output {
        self.cli_in(&self.proj, args, stdin)
    }

    /// Runs `forgebase <args>` and asserts it succeeds; returns stdout.
    fn cli_ok(&self, args: &[&str]) -> String {
        let o = self.cli(args, None);
        assert!(
            o.status.success(),
            "forgebase {args:?} failed: {}\n{}",
            text(&o.stderr),
            text(&o.stdout)
        );
        text(&o.stdout)
    }

    fn log_path(&self) -> PathBuf {
        self.tmp.path().join(format!("serve-{}.log", self.starts))
    }

    /// Starts `forgebase serve` on a free port and waits until it is healthy.
    fn serve(&mut self) {
        assert!(self.child.is_none(), "already serving");
        self.starts += 1;
        let log = std::fs::File::create(self.log_path()).unwrap();
        let mut child = self
            .command(&self.proj)
            .args(["serve", "--port", "0"])
            .stdout(Stdio::piped())
            .stderr(log)
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut sent = false;
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if !sent && let Some(at) = line.find("listening on http://") {
                    let _ = tx.send(line[at + "listening on http://".len()..].trim().to_owned());
                    sent = true;
                }
            }
        });
        let addr = rx.recv_timeout(common::startup_timeout()).unwrap_or_else(|_| {
            let _ = child.kill();
            panic!(
                "server did not report its address; log:\n{}",
                std::fs::read_to_string(self.log_path()).unwrap_or_default()
            )
        });
        self.child = Some(child);
        assert!(
            common::wait_healthy(&addr, common::startup_timeout()),
            "server not healthy; log:\n{}",
            std::fs::read_to_string(self.log_path()).unwrap_or_default()
        );
        self.addr = addr;
    }

    /// SIGTERM, then waits for a clean exit.
    fn stop(&mut self) {
        let mut child = self.child.take().expect("not serving");
        assert!(
            Command::new("kill")
                .args(["-TERM", &child.id().to_string()])
                .status()
                .unwrap()
                .success()
        );
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success(), "serve exited with {status}");
                return;
            }
            assert!(Instant::now() < deadline, "serve did not stop within 30 s");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn restart(&mut self) {
        self.stop();
        self.serve();
    }

    fn send(&self, method: &str, path: &str, token: Option<&str>, headers: &[(&str, String)], body: &[u8]) -> Resp {
        let mut h: Vec<(&str, String)> = headers.to_vec();
        if let Some(t) = token {
            h.push(("Authorization", format!("Bearer {t}")));
        }
        raw(&self.addr, method, path, &h, body)
    }

    /// A JSON request with an optional bearer token.
    fn call(&self, method: &str, path: &str, token: Option<&str>, body: Option<Json>) -> Resp {
        let (h, b) = match body {
            Some(b) => (vec![("Content-Type", "application/json".to_owned())], b.to_string()),
            None => (Vec::new(), String::new()),
        };
        self.send(method, path, token, &h, b.as_bytes())
    }

    /// A JSON request as the administrator.
    fn admin(&self, method: &str, path: &str, body: Option<Json>) -> Resp {
        self.call(method, path, Some(&self.admin.clone()), body)
    }

    fn admin_login(&self) -> String {
        let r = self.call(
            "POST",
            "/api/auth/admins/login",
            None,
            Some(json!({"email": ADMIN_EMAIL, "password": ADMIN_PASSWORD})),
        );
        r.expect(200)["access_token"].as_str().unwrap().to_owned()
    }

    /// Signs a user up in the default `users` collection; returns (token, id).
    fn user(&self, email: &str) -> (String, String) {
        let r = self.call(
            "POST",
            "/api/auth/signup",
            None,
            Some(json!({"email": email, "password": USER_PASSWORD})),
        );
        r.expect(201);
        let j = self
            .call(
                "POST",
                "/api/auth/login",
                None,
                Some(json!({"email": email, "password": USER_PASSWORD})),
            )
            .expect(200);
        let token = j["access_token"].as_str().unwrap().to_owned();
        let me = self.call("GET", "/api/auth/me", Some(&token), None).expect(200);
        let id = me["subject"]["id"]
            .as_str()
            .or_else(|| me["user"]["id"].as_str())
            .unwrap_or_else(|| panic!("no user id in {me}"))
            .to_owned();
        (token, id)
    }

    fn collection(&self, def: Json) -> Json {
        self.admin("POST", "/api/v1/collections", Some(def)).expect(201)
    }
}

impl Drop for Fb {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn ids(list: &Json) -> Vec<String> {
    list["items"]
        .as_array()
        .unwrap_or_else(|| panic!("no items in {list}"))
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_owned())
        .collect()
}

/// A WebAssembly module (base64) whose `handle` writes `out` as its response.
fn module_answering(out: &str) -> String {
    let esc: String = out.bytes().map(|b| format!("\\{b:02x}")).collect();
    let wat = format!(
        r#"(module
  (import "forgebase" "output" (func $output (param i32 i32) (result i32)))
  (import "forgebase" "log" (func $log (param i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 16) "{esc}")
  (data (i32.const 4000) "handled")
  (func (export "handle") (result i32)
    (drop (call $log (i32.const 1) (i32.const 4000) (i32.const 7)))
    (drop (call $output (i32.const 16) (i32.const {len})))
    (i32.const 0)))"#,
        len = out.len()
    );
    base64::engine::general_purpose::STANDARD.encode(wat::parse_str(&wat).unwrap())
}

fn wasm_b64(wat: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(wat::parse_str(wat).unwrap())
}

fn deploy_hello(fb: &Fb) {
    let d = fb
        .admin(
            "POST",
            "/api/v1/functions",
            Some(json!({
                "name": "hello",
                "wasm": module_answering(r#"{"status":200,"body":{"hello":"world"}}"#),
                "access": "public",
                "capabilities": ["log"],
            })),
        )
        .expect(201);
    assert_eq!(d["version"]["version"], 1, "{d}");
}

// ---------------------------------------------------------------------------
// 1-6: projects, SQL, documents, records, indexes, plans
// ---------------------------------------------------------------------------

/// #1 Create a project: `forgebase init` makes the instance; an organization
/// and a project with its three environments are created over the API and
/// survive a restart.
#[test]
fn scenario_01_create_a_project() {
    let mut fb = Fb::start();
    if common::postgres() {
        assert!(
            fb.data_dir().join("postgres").is_dir(),
            "init created the PostgreSQL cluster"
        );
    } else {
        assert!(
            fb.data_dir().join("forgebase.db").is_file(),
            "init created the database"
        );
    }
    let (alice, _) = fb.user("alice@example.com");
    let org = fb
        .call(
            "POST",
            "/api/v1/orgs",
            Some(&alice),
            Some(json!({"slug": "acme", "name": "Acme"})),
        )
        .expect(201);
    let org_id = org["id"].as_str().unwrap().to_owned();
    let project = fb
        .call(
            "POST",
            &format!("/api/v1/orgs/{org_id}/projects"),
            Some(&alice),
            Some(json!({"slug": "app", "name": "App"})),
        )
        .expect(201);
    let pid = project["id"].as_str().unwrap().to_owned();
    let envs: Vec<&str> = project["environments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(envs, ["development", "staging", "production"]);
    // The same slug twice in one organization is a conflict.
    let r = fb.call(
        "POST",
        &format!("/api/v1/orgs/{org_id}/projects"),
        Some(&alice),
        Some(json!({"slug": "app", "name": "Again"})),
    );
    assert_eq!(r.status, 409, "{}", r.text());
    // Anonymous callers cannot create organizations.
    let r = fb.call(
        "POST",
        "/api/v1/orgs",
        None,
        Some(json!({"slug": "anon", "name": "Anon"})),
    );
    assert_eq!(r.status, 401, "{}", r.text());

    fb.restart();
    let alice = fb
        .call(
            "POST",
            "/api/auth/login",
            None,
            Some(json!({"email": "alice@example.com", "password": USER_PASSWORD})),
        )
        .expect(200)["access_token"]
        .as_str()
        .unwrap()
        .to_owned();
    let back = fb
        .call("GET", &format!("/api/v1/projects/{pid}"), Some(&alice), None)
        .expect(200);
    assert_eq!(back["slug"], "app");
    assert_eq!(back["environments"].as_array().unwrap().len(), 3);
}

/// #2 Create a SQL table through the SQL endpoint, write and read it, and
/// keep it across a restart.
#[test]
fn scenario_02_create_a_sql_table() {
    let mut fb = Fb::start();
    fb.admin(
        "POST",
        "/api/v1/sql",
        Some(json!({"sql": if common::postgres() {
            "CREATE TABLE invoices (id BIGSERIAL PRIMARY KEY, customer TEXT NOT NULL, total REAL)"
        } else {
            "CREATE TABLE invoices (id INTEGER PRIMARY KEY, customer TEXT NOT NULL, total REAL) STRICT"
        }})),
    )
    .expect(200);
    let ins = fb
        .admin(
            "POST",
            "/api/v1/sql/batch",
            Some(json!({"statements": [
                {"sql": "INSERT INTO invoices (customer, total) VALUES (?1, ?2)", "params": ["acme", 12.5]},
                {"sql": "INSERT INTO invoices (customer, total) VALUES (?1, ?2)", "params": ["globex", 7]},
            ]})),
        )
        .expect(200);
    assert_eq!(ins["items"][0]["rows_affected"], 1, "{ins}");
    // A failing batch is all-or-nothing.
    let r = fb.admin(
        "POST",
        "/api/v1/sql/batch",
        Some(json!({"statements": [
            {"sql": "INSERT INTO invoices (customer, total) VALUES ('initech', 1)"},
            {"sql": "INSERT INTO invoices (customer, total) VALUES (NULL, 1)"},
        ]})),
    );
    assert_eq!(r.status, 409, "{}", r.text());
    let t = fb.admin("GET", "/api/v1/sql/tables/invoices", None).expect(200);
    assert_eq!(t["columns"].as_array().unwrap().len(), 3, "{t}");

    fb.restart();
    let rows = fb
        .admin(
            "POST",
            "/api/v1/sql",
            Some(json!({"sql": "SELECT customer, total FROM invoices ORDER BY customer"})),
        )
        .expect(200);
    assert_eq!(rows["rows"], json!([["acme", 12.5], ["globex", 7.0]]), "{rows}");
}

/// #3 Create a document collection: flexible schema, nested JSON, merge patch
/// with optimistic concurrency, persisted.
#[test]
fn scenario_03_create_a_document_collection() {
    let mut fb = Fb::start();
    let c = fb.collection(json!({"name": "products", "schema_mode": "flexible", "versioned": true,
        "fields": [{"name": "title", "type": "text", "required": true}]}));
    assert_eq!(c["schema_mode"], "flexible", "{c}");
    let doc = fb
        .admin(
            "POST",
            "/api/v1/documents/products",
            Some(json!({"title": "Laptop", "specs": {"ram": 16, "cpu": "m4"}, "tags": ["a", "b"]})),
        )
        .expect(201);
    let id = doc["id"].as_str().unwrap().to_owned();
    // A document without the required field is refused with field details.
    let r = fb.admin("POST", "/api/v1/documents/products", Some(json!({"specs": {}})));
    assert_eq!(r.status, 400, "{}", r.text());
    let patched = fb.send(
        "PATCH",
        &format!("/api/v1/documents/products/{id}"),
        Some(&fb.admin.clone()),
        &[
            ("Content-Type", "application/merge-patch+json".to_owned()),
            ("If-Match", "\"1\"".to_owned()),
        ],
        json!({"specs": {"ram": 32, "cpu": null}}).to_string().as_bytes(),
    );
    assert_eq!(patched.status, 200, "{}", patched.text());
    assert_eq!(patched.json()["specs"], json!({"ram": 32}));
    let stale = fb.send(
        "PATCH",
        &format!("/api/v1/documents/products/{id}"),
        Some(&fb.admin.clone()),
        &[
            ("Content-Type", "application/json".to_owned()),
            ("If-Match", "\"1\"".to_owned()),
        ],
        json!({"title": "Stale"}).to_string().as_bytes(),
    );
    assert_eq!((stale.status, stale.code().as_str()), (409, "version_conflict"));

    fb.restart();
    let back = fb
        .admin("GET", &format!("/api/v1/documents/products/{id}"), None)
        .expect(200);
    assert_eq!(back["title"], "Laptop");
    assert_eq!(back["specs"], json!({"ram": 32}));
    assert_eq!(back["tags"], json!(["a", "b"]));
}

/// #4 Insert and query records: filters with bound parameters, sort,
/// pagination, cursors and projection.
#[test]
fn scenario_04_insert_and_query_records() {
    let fb = Fb::start();
    fb.collection(json!({"name": "books", "fields": [
        {"name": "title", "type": "text", "required": true},
        {"name": "pages", "type": "number"},
        {"name": "genre", "type": "text"}]}));
    let rows: Vec<Json> = (0..60)
        .map(|i| json!({"title": format!("Book {i:02}"), "pages": i * 10, "genre": if i % 3 == 0 { "sf" } else { "crime" }}))
        .collect();
    let created = fb
        .admin("POST", "/api/v1/collections/books/records", Some(json!(rows)))
        .expect(201);
    assert_eq!(created["items"].as_array().map(Vec::len), Some(60), "{created}");
    let one = fb
        .admin(
            "POST",
            "/api/v1/collections/books/records",
            Some(json!({"title": "Single", "pages": 1, "genre": "poetry"})),
        )
        .expect(201);
    let got = fb
        .admin(
            "GET",
            &format!("/api/v1/collections/books/records/{}", one["id"].as_str().unwrap()),
            None,
        )
        .expect(200);
    assert_eq!(got["title"], "Single");

    let filter = enc("genre = {:g} && pages >= {:min}");
    let q = fb
        .admin(
            "GET",
            &format!("/api/v1/collections/books/records?filter={filter}&p.g=sf&p.min=300&sort=-pages&per_page=5&fields=title,pages"),
            None,
        )
        .expect(200);
    assert_eq!(q["total_items"], 10, "sf books with >= 300 pages: {q}");
    assert_eq!(q["items"][0]["title"], "Book 57");
    assert_eq!(q["items"].as_array().unwrap().len(), 5);
    assert!(q["items"][0].get("genre").is_none(), "projection: {q}");
    // Cursor pagination walks every record exactly once.
    let mut seen = Vec::new();
    let mut path = "/api/v1/collections/books/records?per_page=25&sort=title".to_owned();
    loop {
        let page = fb.admin("GET", &path, None).expect(200);
        seen.extend(ids(&page));
        match page["next_cursor"].as_str() {
            Some(c) => {
                path = format!(
                    "/api/v1/collections/books/records?per_page=25&sort=title&cursor={}",
                    enc(c)
                )
            }
            None => break,
        }
    }
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), 61);
    // The JSON query form returns the same answer.
    let q2 = fb
        .admin(
            "POST",
            "/api/v1/collections/books/records/query",
            Some(json!({"filter": "genre = 'sf' && pages >= 300", "sort": "-pages"})),
        )
        .expect(200);
    assert_eq!(q2["total_items"], 10);
    // Validation: a wrong type is refused and nothing is written.
    let r = fb.admin(
        "POST",
        "/api/v1/collections/books/records",
        Some(json!({"title": "Bad", "pages": "many"})),
    );
    assert_eq!(r.status, 400, "{}", r.text());
    let all = fb
        .admin("GET", "/api/v1/collections/books/records?per_page=1", None)
        .expect(200);
    assert_eq!(all["total_items"], 61);
}

fn items_collection(fb: &Fb) {
    fb.collection(json!({"name": "items", "fields": [
        {"name": "category", "type": "text"}, {"name": "price", "type": "number"}, {"name": "note", "type": "text"}]}));
    let rows: Vec<Json> = (0..300)
        .map(|i| json!({"category": format!("c{}", i % 10), "price": i, "note": format!("n{i}")}))
        .collect();
    fb.admin("POST", "/api/v1/collections/items/records", Some(json!(rows[..150])))
        .expect(201);
    fb.admin("POST", "/api/v1/collections/items/records", Some(json!(rows[150..])))
        .expect(201);
}

/// #5 Create a composite index and read it back after a restart.
#[test]
fn scenario_05_create_a_composite_index() {
    let mut fb = Fb::start();
    items_collection(&fb);
    let idx = fb
        .admin(
            "POST",
            "/api/v1/indexes",
            Some(json!({"name": "idx_items_cat_price", "collection": "items",
                "keys": [{"path": "category"}, {"path": "price", "desc": true}]})),
        )
        .expect(201);
    assert_eq!(idx["columns"].as_array().unwrap().len(), 2, "{idx}");
    let r = fb.admin(
        "POST",
        "/api/v1/indexes",
        Some(json!({"name": "idx_items_cat_price", "collection": "items", "keys": [{"path": "note"}]})),
    );
    assert_eq!(r.status, 409, "same name twice: {}", r.text());
    let r = fb.admin(
        "POST",
        "/api/v1/indexes",
        Some(json!({"name": "idx_bad", "collection": "items", "keys": [{"path": "nope"}]})),
    );
    assert_eq!(r.status / 100, 4, "unknown field: {}", r.text());

    fb.restart();
    let one = fb.admin("GET", "/api/v1/indexes/idx_items_cat_price", None).expect(200);
    assert_eq!(one["definition"]["keys"][0]["path"], "category", "{one}");
    assert_eq!(one["definition"]["keys"][1]["desc"], true);
    let list = fb.admin("GET", "/api/v1/indexes?collection=items", None).expect(200);
    assert!(
        list["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["name"] == "idx_items_cat_price"),
        "{list}"
    );
}

/// #6 Validate index usage with query plans: a full scan before the index,
/// the composite index after it, for the record API and for raw SQL.
#[test]
fn scenario_06_validate_index_usage_with_query_plans() {
    let fb = Fb::start();
    items_collection(&fb);
    let plan_query = json!({"filter": "category = 'c3' && price > 100", "sort": "-price"});
    let before = fb
        .admin(
            "POST",
            "/api/v1/collections/items/records/explain",
            Some(plan_query.clone()),
        )
        .expect(200);
    assert!(before["indexes_used"].as_array().unwrap().is_empty(), "{before}");
    assert!(!before["full_scans"].as_array().unwrap().is_empty(), "{before}");
    fb.admin(
        "POST",
        "/api/v1/indexes",
        Some(json!({"name": "idx_items_cat_price", "collection": "items",
            "keys": [{"path": "category"}, {"path": "price", "desc": true}]})),
    )
    .expect(201);
    let r = fb.admin("POST", "/api/v1/indexes/analyze", Some(json!({"collection": "items"})));
    assert_eq!(r.status, 204, "{}", r.text());
    let after = fb
        .admin(
            "POST",
            "/api/v1/collections/items/records/explain",
            Some(plan_query.clone()),
        )
        .expect(200);
    // Plan shape is the engine's choice: SQLite takes the index; the PostgreSQL
    // planner prefers a sequential scan on a table this small.
    if !common::postgres() {
        assert_eq!(after["indexes_used"], json!(["idx_items_cat_price"]), "{after}");
        assert!(after["full_scans"].as_array().unwrap().is_empty(), "{after}");
    }
    let rows = fb
        .admin("POST", "/api/v1/collections/items/records/query", Some(plan_query))
        .expect(200);
    assert_eq!(rows["total_items"], 20);
    assert_eq!(rows["items"][0]["price"], 293.0);
    let sql = fb
        .admin(
            "POST",
            "/api/v1/sql/explain",
            Some(json!({"sql": "SELECT * FROM items WHERE category = ?1 AND price > ?2", "params": ["c3", 100]})),
        )
        .expect(200);
    if !common::postgres() {
        assert_eq!(sql["indexes_used"], json!(["idx_items_cat_price"]), "{sql}");
    }
}

// ---------------------------------------------------------------------------
// 7-9: authentication, rejection, tenant isolation
// ---------------------------------------------------------------------------

/// #7 Authenticate a user: sign-up, sign-in, the session's identity, refresh
/// rotation, sign-out.
#[test]
fn scenario_07_authenticate_a_user() {
    let mut fb = Fb::start();
    fb.call(
        "POST",
        "/api/auth/signup",
        None,
        Some(json!({"email": "ann@example.com", "password": USER_PASSWORD})),
    )
    .expect(201);
    let login = fb
        .call(
            "POST",
            "/api/auth/login",
            None,
            Some(json!({"email": "ANN@example.com", "password": USER_PASSWORD})),
        )
        .expect(200);
    let access = login["access_token"].as_str().unwrap().to_owned();
    let refresh = login["refresh_token"].as_str().unwrap().to_owned();
    let me = fb.call("GET", "/api/auth/me", Some(&access), None).expect(200);
    assert_eq!(me["subject"]["type"], "user", "{me}");
    assert_eq!(me["user"]["email"], "ann@example.com", "{me}");
    // The administrator also authenticates (CLI-bootstrapped account).
    let me = fb.admin("GET", "/api/auth/me", None).expect(200);
    assert_eq!(me["subject"]["type"], "admin");

    // Refresh rotates the token; the session survives a restart.
    fb.restart();
    assert_eq!(fb.call("GET", "/api/auth/me", Some(&access), None).status, 200);
    let rotated = fb
        .call(
            "POST",
            "/api/auth/refresh",
            None,
            Some(json!({"refresh_token": refresh})),
        )
        .expect(200);
    let access2 = rotated["access_token"].as_str().unwrap().to_owned();
    assert_ne!(rotated["refresh_token"].as_str().unwrap(), refresh.as_str());
    assert_eq!(fb.call("GET", "/api/auth/me", Some(&access2), None).status, 200);
    // Reusing the rotated refresh token is theft: the whole session ends.
    let r = fb.call(
        "POST",
        "/api/auth/refresh",
        None,
        Some(json!({"refresh_token": refresh})),
    );
    assert_eq!(r.status, 401, "{}", r.text());
    assert_eq!(fb.call("GET", "/api/auth/me", Some(&access2), None).status, 401);

    // A new sign-in, then sign-out revokes that session at once.
    let t = fb
        .call(
            "POST",
            "/api/auth/login",
            None,
            Some(json!({"email": "ann@example.com", "password": USER_PASSWORD})),
        )
        .expect(200)["access_token"]
        .as_str()
        .unwrap()
        .to_owned();
    let r = fb.call("POST", "/api/auth/logout", Some(&t), Some(json!({})));
    assert!(r.status == 200 || r.status == 204, "logout: {} {}", r.status, r.text());
    assert_eq!(fb.call("GET", "/api/auth/me", Some(&t), None).status, 401);
}

/// #8 Reject unauthorized requests: no credential, a forged or malformed
/// token, wrong password, a user on administrator routes.
#[test]
fn scenario_08_reject_unauthorized_requests() {
    let fb = Fb::start();
    fb.collection(json!({"name": "notes", "fields": [{"name": "text", "type": "text"}]}));
    let r = fb.call("GET", "/api/auth/me", None, None);
    assert_eq!((r.status, r.code().as_str()), (401, "unauthenticated"), "{}", r.text());
    assert!(r.header("x-request-id").is_some_and(|v| !v.is_empty()));
    let r = fb.call("GET", "/api/v1/collections", None, None);
    assert_eq!(r.status, 401, "{}", r.text());
    let r = fb.call("GET", "/api/auth/me", Some("not-a-token"), None);
    assert_eq!((r.status, r.code().as_str()), (401, "invalid_token"));
    // A token signed with alg=none, and the admin's token with its signature altered.
    let b64 = |v: Json| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string());
    let none = format!(
        "{}.{}.",
        b64(json!({"alg": "none", "typ": "JWT"})),
        b64(json!({"sub": "admin", "typ": "admin", "exp": now_secs() + 600}))
    );
    assert_eq!(fb.call("GET", "/api/auth/me", Some(&none), None).status, 401);
    let mut forged = fb.admin.clone();
    let last = forged.pop().unwrap();
    forged.push(if last == 'A' { 'B' } else { 'A' });
    assert_eq!(fb.call("GET", "/api/v1/collections", Some(&forged), None).status, 401);
    let r = fb.call(
        "POST",
        "/api/auth/admins/login",
        None,
        Some(json!({"email": ADMIN_EMAIL, "password": "wrong password"})),
    );
    assert_eq!((r.status, r.code().as_str()), (401, "invalid_credentials"));
    let r = fb.call(
        "POST",
        "/api/auth/login",
        None,
        Some(json!({"email": "nobody@example.com", "password": "whatever 12345"})),
    );
    assert_eq!(r.status, 401, "{}", r.text());
    // A signed-in user is not an administrator.
    let (user, _) = fb.user("user@example.com");
    for (m, p, b) in [
        ("POST", "/api/v1/collections", Some(json!({"name": "x", "fields": []}))),
        ("POST", "/api/v1/sql", Some(json!({"sql": "SELECT 1"}))),
        ("GET", "/api/auth/audit", None),
        ("DELETE", "/api/v1/collections/notes", None),
        ("GET", "/api/v1/functions", None),
    ] {
        let r = fb.call(m, p, Some(&user), b);
        assert_eq!(r.status, 403, "{m} {p} as a user: {}", r.text());
    }
    // Nothing was changed by the refused calls.
    let list = fb.admin("GET", "/api/v1/collections", None).expect(200);
    assert!(list.to_string().contains("\"notes\""), "{list}");
}

/// #9 Verify tenant isolation: a tenant-scoped collection, two organizations;
/// the other tenant's header is refused and a known record id is not found
/// from another tenant.
#[test]
fn scenario_09_verify_tenant_isolation() {
    let fb = Fb::start();
    fb.collection(json!({"name": "tasks", "options": {"tenant_scoped": true},
        "fields": [{"name": "title", "type": "text"}]}));
    let any_user = "@request.auth.id != ''";
    fb.admin(
        "PUT",
        "/api/v1/policies/tasks",
        Some(
            json!({"list_rule": any_user, "view_rule": any_user, "create_rule": any_user,
            "update_rule": any_user, "delete_rule": any_user}),
        ),
    )
    .expect(200);
    let tenant_of = |token: &str, slug: &str| -> String {
        let org = fb
            .call(
                "POST",
                "/api/v1/orgs",
                Some(token),
                Some(json!({"slug": slug, "name": slug})),
            )
            .expect(201);
        let p = fb
            .call(
                "POST",
                &format!("/api/v1/orgs/{}/projects", org["id"].as_str().unwrap()),
                Some(token),
                Some(json!({"slug": "app", "name": "App"})),
            )
            .expect(201);
        p["environments"][0]["id"].as_str().unwrap().to_owned()
    };
    let (alice, _) = fb.user("alice@example.com");
    let (bob, _) = fb.user("bob@example.com");
    let ta = tenant_of(&alice, "acme");
    let tb = tenant_of(&bob, "bobco");
    let as_tenant = |token: &str, tenant: &str, m: &str, p: &str, b: Option<Json>| {
        let mut h = vec![("X-Forgebase-Tenant", tenant.to_owned())];
        let body = b.map(|b| b.to_string()).unwrap_or_default();
        if !body.is_empty() {
            h.push(("Content-Type", "application/json".to_owned()));
        }
        fb.send(m, p, Some(token), &h, body.as_bytes())
    };
    let rec = as_tenant(
        &alice,
        &ta,
        "POST",
        "/api/v1/collections/tasks/records",
        Some(json!({"title": "alice's"})),
    )
    .expect(201);
    let rid = rec["id"].as_str().unwrap().to_owned();
    as_tenant(
        &bob,
        &tb,
        "POST",
        "/api/v1/collections/tasks/records",
        Some(json!({"title": "bob's"})),
    )
    .expect(201);

    // Bob cannot act in alice's tenant: the tenancy step refuses.
    let r = as_tenant(
        &bob,
        &ta,
        "GET",
        &format!("/api/v1/collections/tasks/records/{rid}"),
        None,
    );
    assert_eq!(r.status, 403, "{}", r.text());
    assert_eq!(r.json()["error"]["details"]["step"], "tenant", "{}", r.text());
    // From his own tenant the id does not exist; the list holds only his record.
    let r = as_tenant(
        &bob,
        &tb,
        "GET",
        &format!("/api/v1/collections/tasks/records/{rid}"),
        None,
    );
    assert_eq!(r.status, 404, "{}", r.text());
    let list = as_tenant(&bob, &tb, "GET", "/api/v1/collections/tasks/records", None).expect(200);
    let titles: Vec<&str> = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, ["bob's"]);
    // Updates and deletes across tenants do not happen either.
    let r = as_tenant(
        &bob,
        &tb,
        "PATCH",
        &format!("/api/v1/collections/tasks/records/{rid}"),
        Some(json!({"title": "pwned"})),
    );
    assert_eq!(r.status, 404, "{}", r.text());
    let r = as_tenant(
        &bob,
        &tb,
        "DELETE",
        &format!("/api/v1/collections/tasks/records/{rid}"),
        None,
    );
    assert_eq!(r.status, 404, "{}", r.text());
    let mine = as_tenant(
        &alice,
        &ta,
        "GET",
        &format!("/api/v1/collections/tasks/records/{rid}"),
        None,
    )
    .expect(200);
    assert_eq!(mine["title"], "alice's");
    let list = as_tenant(&alice, &ta, "GET", "/api/v1/collections/tasks/records", None).expect(200);
    assert_eq!(list["items"].as_array().unwrap().len(), 1, "{list}");
}

// ---------------------------------------------------------------------------
// 10-11: object storage
// ---------------------------------------------------------------------------

/// #10 Upload and retrieve an object: bytes, checksum, metadata, range, and
/// persistence across a restart.
#[test]
fn scenario_10_upload_and_retrieve_an_object() {
    let mut fb = Fb::start();
    let b = fb
        .admin("POST", "/api/v1/storage/buckets", Some(json!({"name": "photos"})))
        .expect(201);
    assert_eq!(b["visibility"], "private", "{b}");
    let data: Vec<u8> = (0..300_000u32).map(|i| (i * 7 % 251) as u8).collect();
    let digest = sha256_hex(&data);
    let admin = fb.admin.clone();
    let r = fb.send(
        "PUT",
        "/api/v1/storage/buckets/photos/objects/2026/cat.bin",
        Some(&admin),
        &[
            ("Content-Type", "application/octet-stream".to_owned()),
            ("X-Forgebase-Checksum-Sha256", digest.clone()),
            ("X-Forgebase-Meta-Owner", "alice".to_owned()),
        ],
        &data,
    );
    let info = r.expect(201);
    assert_eq!(info["size"], 300_000);
    assert_eq!(info["checksum_sha256"], digest.as_str());
    // A wrong checksum is refused.
    let r = fb.send(
        "PUT",
        "/api/v1/storage/buckets/photos/objects/bad.bin",
        Some(&admin),
        &[
            ("Content-Type", "application/octet-stream".to_owned()),
            ("X-Forgebase-Checksum-Sha256", sha256_hex(b"other")),
        ],
        &data,
    );
    assert_eq!(r.status / 100, 4, "{}", r.text());

    fb.restart();
    let admin = fb.admin.clone();
    let got = fb.send(
        "GET",
        "/api/v1/storage/buckets/photos/objects/2026/cat.bin",
        Some(&admin),
        &[],
        b"",
    );
    assert_eq!(got.status, 200, "{}", got.text());
    assert_eq!(got.body.len(), data.len());
    assert!(got.body == data, "the bytes came back unchanged");
    assert_eq!(got.header("etag").as_deref(), Some(format!("\"{digest}\"").as_str()));
    assert_eq!(got.header("x-forgebase-meta-owner").as_deref(), Some("alice"));
    let part = fb.send(
        "GET",
        "/api/v1/storage/buckets/photos/objects/2026/cat.bin",
        Some(&admin),
        &[("Range", "bytes=100-199".to_owned())],
        b"",
    );
    assert_eq!(part.status, 206);
    assert!(part.body == data[100..200], "range bytes");
    let list = fb
        .admin("GET", "/api/v1/storage/buckets/photos/objects?prefix=2026/", None)
        .expect(200);
    assert_eq!(list["objects"][0]["key"], "2026/cat.bin", "{list}");
    // Anonymous callers get nothing.
    let r = fb.send(
        "GET",
        "/api/v1/storage/buckets/photos/objects/2026/cat.bin",
        None,
        &[],
        b"",
    );
    assert_eq!(r.status, 401);
    // Delete, then it is gone.
    fb.admin("DELETE", "/api/v1/storage/buckets/photos/objects/2026/cat.bin", None)
        .expect(200);
    let r = fb.send(
        "GET",
        "/api/v1/storage/buckets/photos/objects/2026/cat.bin",
        Some(&admin),
        &[],
        b"",
    );
    assert_eq!(r.status, 404);
}

/// #11 Generate a signed download URL: it works without credentials, a
/// tampered or expired one does not.
#[test]
fn scenario_11_generate_a_signed_download_url() {
    let fb = Fb::start();
    fb.admin("POST", "/api/v1/storage/buckets", Some(json!({"name": "docs"})))
        .expect(201);
    let admin = fb.admin.clone();
    let data = b"top secret report".to_vec();
    fb.send(
        "PUT",
        "/api/v1/storage/buckets/docs/objects/reports/q3%202026.txt",
        Some(&admin),
        &[("Content-Type", "text/plain".to_owned())],
        &data,
    )
    .expect(201);
    let signed = fb
        .admin(
            "POST",
            "/api/v1/storage/buckets/docs/sign",
            Some(json!({"key": "reports/q3 2026.txt", "expires_in": 60, "download_as": "q3.txt"})),
        )
        .expect(200);
    let url = signed["url"].as_str().unwrap().to_owned();
    assert!(url.starts_with("/api/v1/storage/signed/docs/"), "{url}");
    let r = fb.send("GET", &url, None, &[], b"");
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(r.body == data);
    assert_eq!(
        r.header("content-disposition").as_deref(),
        Some("attachment; filename=\"q3.txt\"")
    );
    // The plain object path needs credentials.
    let r = fb.send(
        "GET",
        "/api/v1/storage/buckets/docs/objects/reports/q3%202026.txt",
        None,
        &[],
        b"",
    );
    assert_eq!(r.status, 401);
    // Tampering with the signature or the expiry breaks it.
    let tampered = if url.ends_with('0') {
        format!("{}1", &url[..url.len() - 1])
    } else {
        format!("{}0", &url[..url.len() - 1])
    };
    assert_eq!(fb.send("GET", &tampered, None, &[], b"").status / 100, 4);
    let (path, query) = url.split_once('?').unwrap();
    let longer: String = query
        .split('&')
        .map(|kv| match kv.split_once('=') {
            Some(("expires", v)) => format!("expires={}", v.parse::<i64>().unwrap() + 3600),
            _ => kv.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("&");
    assert_eq!(
        fb.send("GET", &format!("{path}?{longer}"), None, &[], b"").status / 100,
        4
    );
    // A URL that expires in one second stops working.
    let short = fb
        .admin(
            "POST",
            "/api/v1/storage/buckets/docs/sign",
            Some(json!({"key": "reports/q3 2026.txt", "expires_in": 1})),
        )
        .expect(200);
    std::thread::sleep(Duration::from_millis(2_100));
    let r = fb.send("GET", short["url"].as_str().unwrap(), None, &[], b"");
    assert_eq!(r.status / 100, 4, "expired URL answered {}", r.status);
}

// ---------------------------------------------------------------------------
// 12-15: vectors and graphs
// ---------------------------------------------------------------------------

fn vector_kb(fb: &Fb, n: u32) {
    fb.admin(
        "POST",
        "/api/v1/vector/collections",
        Some(json!({"name": "kb", "fields": [{"name": "emb", "dimension": 3, "metric": "l2"}], "exact_threshold": 0})),
    )
    .expect(201);
    let items: Vec<Json> = (0..n)
        .map(|i| json!({"id": format!("p{i}"), "vectors": {"emb": [f64::from(i), 0.0, 0.0]}, "metadata": {"even": i % 2 == 0}}))
        .collect();
    let r = fb
        .admin(
            "POST",
            "/api/v1/vector/collections/kb/upsert",
            Some(json!({"items": items})),
        )
        .expect(200);
    assert_eq!(r["created"], n, "{r}");
}

fn top_ids(fb: &Fb, v: [f64; 3], k: u32) -> Vec<String> {
    let r = fb
        .admin(
            "POST",
            "/api/v1/vector/collections/kb/search",
            Some(json!({"vector": v, "k": k})),
        )
        .expect(200);
    r["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["id"].as_str().unwrap().to_owned())
        .collect()
}

/// #12 Store and retrieve a vector, with metadata, across a restart.
#[test]
fn scenario_12_store_and_retrieve_a_vector() {
    let mut fb = Fb::start();
    vector_kb(&fb, 50);
    let item = fb
        .admin("GET", "/api/v1/vector/collections/kb/items/p7", None)
        .expect(200);
    assert_eq!(item["vectors"]["emb"], json!([7.0, 0.0, 0.0]), "{item}");
    assert_eq!(item["metadata"]["even"], false);
    let r = fb.admin(
        "POST",
        "/api/v1/vector/collections/kb/upsert",
        Some(json!({"items": [{"id": "bad", "vectors": {"emb": [1.0, 2.0]}}]})),
    );
    assert_eq!((r.status, r.code().as_str()), (400, "vector_dimension_mismatch"));
    fb.restart();
    let item = fb
        .admin("GET", "/api/v1/vector/collections/kb/items/p7", None)
        .expect(200);
    assert_eq!(item["vectors"]["emb"], json!([7.0, 0.0, 0.0]));
    let stats = fb.admin("GET", "/api/v1/vector/collections/kb/stats", None).expect(200);
    assert_eq!(stats["items"], 50, "{stats}");
}

/// #13 Execute vector similarity search: nearest neighbours through the ANN
/// index, with a metadata filter.
#[test]
fn scenario_13_execute_vector_similarity_search() {
    let fb = Fb::start();
    vector_kb(&fb, 200);
    assert_eq!(top_ids(&fb, [41.2, 0.0, 0.0], 2), ["p41", "p42"]);
    assert_eq!(top_ids(&fb, [199.9, 0.0, 0.0], 3), ["p199", "p198", "p197"]);
    let r = fb
        .admin(
            "POST",
            "/api/v1/vector/collections/kb/search",
            Some(json!({"vector": [41.2, 0.0, 0.0], "k": 2, "filter": "even = true"})),
        )
        .expect(200);
    let hits: Vec<&str> = r["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["id"].as_str().unwrap())
        .collect();
    assert_eq!(hits, ["p42", "p40"], "{r}");
    let r = fb.admin(
        "POST",
        "/api/v1/vector/collections/kb/search",
        Some(json!({"vector": [1.0, 0.0]})),
    );
    assert_eq!(r.status, 400);
}

fn graph_seed(fb: &Fb) {
    for (id, label) in [
        ("a", "User"),
        ("b", "User"),
        ("c", "User"),
        ("d", "User"),
        ("x", "Product"),
    ] {
        fb.admin(
            "POST",
            "/api/v1/graph/nodes",
            Some(json!({"id": id, "label": label, "properties": {"name": id}})),
        )
        .expect(201);
    }
    for (id, t, s, d) in [
        ("f1", "FOLLOWS", "a", "b"),
        ("f2", "FOLLOWS", "b", "c"),
        ("f3", "FOLLOWS", "c", "d"),
        ("p1", "PURCHASED", "a", "x"),
    ] {
        fb.admin(
            "POST",
            "/api/v1/graph/edges",
            Some(json!({"id": id, "type": t, "source": s, "target": d})),
        )
        .expect(201);
    }
}

/// #14 Create graph nodes and edges; referential integrity; persisted.
#[test]
fn scenario_14_create_graph_nodes_and_edges() {
    let mut fb = Fb::start();
    graph_seed(&fb);
    let r = fb.admin(
        "POST",
        "/api/v1/graph/edges",
        Some(json!({"type": "FOLLOWS", "source": "a", "target": "ghost"})),
    );
    assert_eq!(r.status / 100, 4, "an edge to a missing node: {}", r.text());
    let r = fb.admin("POST", "/api/v1/graph/nodes", Some(json!({"id": "a"})));
    assert_eq!(r.status, 409, "{}", r.text());
    fb.restart();
    let n = fb.admin("GET", "/api/v1/graph/nodes/a", None).expect(200);
    assert_eq!(
        (n["label"].as_str(), n["properties"]["name"].as_str()),
        (Some("User"), Some("a"))
    );
    let e = fb.admin("GET", "/api/v1/graph/edges/f1", None).expect(200);
    assert_eq!((e["source"].as_str(), e["target"].as_str()), (Some("a"), Some("b")));
    let stats = fb.admin("GET", "/api/v1/graph/stats", None).expect(200);
    assert_eq!(
        (stats["nodes"].as_i64(), stats["edges"].as_i64()),
        (Some(5), Some(4)),
        "{stats}"
    );
    // Deleting a node removes its edges.
    fb.admin("DELETE", "/api/v1/graph/nodes/b", None).expect(200);
    assert_eq!(fb.admin("GET", "/api/v1/graph/edges/f1", None).status, 404);
    assert_eq!(fb.admin("GET", "/api/v1/graph/edges/f2", None).status, 404);
}

/// #15 Execute graph traversal: bounded BFS, neighbours, shortest path.
#[test]
fn scenario_15_execute_graph_traversal() {
    let fb = Fb::start();
    graph_seed(&fb);
    let r = fb
        .admin(
            "POST",
            "/api/v1/graph/traverse",
            Some(json!({"start": "a", "edge_types": ["FOLLOWS"], "max_depth": 2})),
        )
        .expect(200);
    let mut got: Vec<(String, i64)> = r["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| {
            (
                n["node"]["id"].as_str().unwrap().to_owned(),
                n["depth"].as_i64().unwrap(),
            )
        })
        .collect();
    got.sort();
    assert_eq!(got, [("b".to_owned(), 1), ("c".to_owned(), 2)], "{r}");
    let r = fb
        .admin("GET", "/api/v1/graph/nodes/a/neighbors?direction=out", None)
        .expect(200);
    let mut n: Vec<&str> = r["items"]
        .as_array()
        .unwrap_or_else(|| panic!("{r}"))
        .iter()
        .map(|i| i["node"]["id"].as_str().unwrap())
        .collect();
    n.sort_unstable();
    assert_eq!(n, ["b", "x"], "{r}");
    let r = fb
        .admin(
            "POST",
            "/api/v1/graph/shortest-path",
            Some(json!({"from": "a", "to": "d"})),
        )
        .expect(200);
    let path: Vec<&str> = r["nodes"]
        .as_array()
        .unwrap_or_else(|| panic!("{r}"))
        .iter()
        .map(|n| n["id"].as_str().or_else(|| n.as_str()).unwrap())
        .collect();
    assert_eq!(path, ["a", "b", "c", "d"], "{r}");
    // Injection through traversal parameters changes nothing.
    let r = fb.admin("POST", "/api/v1/graph/traverse", Some(json!({"start": "a' OR '1'='1"})));
    assert_eq!(r.status, 404);
    let stats = fb.admin("GET", "/api/v1/graph/stats", None).expect(200);
    assert_eq!(stats["nodes"], 5);
}

// ---------------------------------------------------------------------------
// 16-17: realtime
// ---------------------------------------------------------------------------

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn ws_connect(addr: &str, token: &str) -> Ws {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut req = format!("ws://{addr}/api/v1/realtime/ws").into_client_request().unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    tokio_tungstenite::connect_async(req).await.unwrap().0
}

async fn ws_next(ws: &mut Ws) -> Json {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::Message;
    loop {
        match tokio::time::timeout(Duration::from_secs(15), ws.next())
            .await
            .expect("no realtime message within 15 s")
        {
            Some(Ok(Message::Text(t))) => return serde_json::from_str(t.as_str()).unwrap(),
            Some(Ok(_)) => {}
            other => panic!("socket ended: {other:?}"),
        }
    }
}

async fn ws_send(ws: &mut Ws, v: Json) {
    use futures_util::SinkExt;
    ws.send(tokio_tungstenite::tungstenite::Message::Text(v.to_string().into()))
        .await
        .unwrap();
}

/// #16 Subscribe to realtime events over WebSocket and Server-Sent Events;
/// a subscription the caller may not read is refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scenario_16_subscribe_to_realtime_events() {
    let fb = Fb::start();
    fb.collection(json!({"name": "notes", "fields": [{"name": "text", "type": "text"}]}));
    let mut ws = ws_connect(&fb.addr, &fb.admin).await;
    assert_eq!(ws_next(&mut ws).await["type"], "welcome");
    ws_send(&mut ws, json!({"type": "subscribe", "id": "n", "collection": "notes"})).await;
    let ok = ws_next(&mut ws).await;
    assert_eq!(ok["type"], "subscribed", "{ok}");
    let stats = fb.admin("GET", "/api/v1/realtime/stats", None).expect(200);
    assert!(stats["connections"].as_u64().unwrap() >= 1, "{stats}");

    // A user without read access to the collection is refused per subscription.
    let (user, _) = fb.user("eve@example.com");
    let mut evil = ws_connect(&fb.addr, &user).await;
    assert_eq!(ws_next(&mut evil).await["type"], "welcome");
    ws_send(
        &mut evil,
        json!({"type": "subscribe", "id": "s", "collection": "notes"}),
    )
    .await;
    let refused = ws_next(&mut evil).await;
    assert_ne!(refused["type"], "subscribed", "{refused}");

    // SSE: the stream opens with the subscription acknowledged.
    let subs = enc(&json!([{"id": "n", "collection": "notes"}]).to_string());
    let mut s = TcpStream::connect(&fb.addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
    write!(
        s,
        "GET /api/v1/realtime/sse?subscriptions={subs} HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nAccept: text/event-stream\r\n\r\n",
        fb.addr, fb.admin
    )
    .unwrap();
    let mut r = BufReader::new(s);
    let mut status = String::new();
    r.read_line(&mut status).unwrap();
    assert!(status.contains(" 200 "), "{status}");
    let mut seen = String::new();
    while !seen.contains("subscribed") {
        let mut line = String::new();
        assert!(r.read_line(&mut line).unwrap() > 0, "sse ended: {seen}");
        seen.push_str(&line);
    }
}

/// #17 Verify event delivery after a committed change: created, updated and
/// deleted events arrive over WebSocket and SSE in commit order; a
/// rolled-back batch emits nothing; replay from a cursor after a restart.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scenario_17_verify_event_delivery_after_a_committed_change() {
    let mut fb = Fb::start();
    fb.collection(json!({"name": "notes", "fields": [{"name": "text", "type": "text", "required": true}]}));
    let mut ws = ws_connect(&fb.addr, &fb.admin).await;
    assert_eq!(ws_next(&mut ws).await["type"], "welcome");
    ws_send(&mut ws, json!({"type": "subscribe", "id": "n", "collection": "notes"})).await;
    assert_eq!(ws_next(&mut ws).await["type"], "subscribed");

    // SSE subscriber in a thread, collecting `data:` lines.
    let subs = enc(&json!([{"id": "n", "collection": "notes"}]).to_string());
    let (addr, admin) = (fb.addr.clone(), fb.admin.clone());
    let (tx, rx) = mpsc::channel::<Json>();
    std::thread::spawn(move || {
        let mut s = TcpStream::connect(&addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        write!(
            s,
            "GET /api/v1/realtime/sse?subscriptions={subs} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {admin}\r\nAccept: text/event-stream\r\n\r\n"
        )
        .unwrap();
        for line in BufReader::new(s).lines() {
            let Ok(line) = line else { break };
            if let Some(d) = line.strip_prefix("data:")
                && let Ok(v) = serde_json::from_str::<Json>(d.trim())
                && tx.send(v).is_err()
            {
                break;
            }
        }
    });
    let sse_next = |kind: &str| -> Json {
        loop {
            let v = rx
                .recv_timeout(Duration::from_secs(15))
                .expect("no SSE event within 15 s");
            if v["type"] == kind {
                return v;
            }
        }
    };
    // Wait for the SSE subscription to be live before writing.
    sse_next("subscribed");

    // A failing batch (second record invalid) commits nothing and emits nothing.
    let r = fb.admin(
        "POST",
        "/api/v1/collections/notes/records",
        Some(json!([{"text": "ghost"}, {"nope": 1}])),
    );
    assert_eq!(r.status, 400, "{}", r.text());
    let rec = fb
        .admin(
            "POST",
            "/api/v1/collections/notes/records",
            Some(json!({"text": "hello"})),
        )
        .expect(201);
    let id = rec["id"].as_str().unwrap().to_owned();
    let e = ws_next(&mut ws).await;
    assert_eq!(e["type"], "record.created", "{e}");
    assert_eq!(e["record_id"], id.as_str());
    assert_eq!(e["data"]["text"], "hello", "the batch that failed sent no event first");
    let cursor = e["cursor"].as_i64().unwrap();
    // The change is committed when the event arrives: a read sees it.
    fb.admin("GET", &format!("/api/v1/collections/notes/records/{id}"), None)
        .expect(200);
    fb.admin(
        "PATCH",
        &format!("/api/v1/collections/notes/records/{id}"),
        Some(json!({"text": "edited"})),
    )
    .expect(200);
    let e = ws_next(&mut ws).await;
    assert_eq!(
        (e["type"].as_str(), e["data"]["text"].as_str()),
        (Some("record.updated"), Some("edited"))
    );
    fb.admin("DELETE", &format!("/api/v1/collections/notes/records/{id}"), None);
    let e = ws_next(&mut ws).await;
    assert_eq!(e["type"], "record.deleted", "{e}");
    let s1 = sse_next("record.created");
    assert_eq!(s1["record_id"], id.as_str());
    assert_eq!(sse_next("record.updated")["record_id"], id.as_str());
    assert_eq!(sse_next("record.deleted")["record_id"], id.as_str());
    drop(ws);

    // After a restart the persisted outbox replays from a cursor.
    fb.restart();
    let mut ws = ws_connect(&fb.addr, &fb.admin).await;
    assert_eq!(ws_next(&mut ws).await["type"], "welcome");
    ws_send(
        &mut ws,
        json!({"type": "subscribe", "id": "n", "collection": "notes", "cursor": cursor - 1}),
    )
    .await;
    let kinds = [
        ws_next(&mut ws).await["type"].as_str().unwrap().to_owned(),
        ws_next(&mut ws).await["type"].as_str().unwrap().to_owned(),
        ws_next(&mut ws).await["type"].as_str().unwrap().to_owned(),
    ];
    assert_eq!(kinds, ["record.created", "record.updated", "record.deleted"]);
    let ok = ws_next(&mut ws).await;
    assert_eq!(
        (ok["type"].as_str(), ok["replayed"].as_u64()),
        (Some("subscribed"), Some(3)),
        "{ok}"
    );
}

// ---------------------------------------------------------------------------
// 18-20: functions, jobs, billing
// ---------------------------------------------------------------------------

/// #18 Execute a sandboxed server function: deploy WebAssembly over REST,
/// invoke it over HTTP; the sandbox stops an infinite loop, refuses imports
/// outside the host ABI and host calls without the capability.
#[test]
fn scenario_18_execute_a_sandboxed_server_function() {
    let fb = Fb::start();
    // Users cannot deploy.
    let (user, _) = fb.user("dev@example.com");
    let hello = json!({"name": "hello", "wasm": module_answering(r#"{"status":200,"body":{"hello":"world"}}"#),
        "access": "public", "capabilities": ["log"]});
    assert_eq!(
        fb.call("POST", "/api/v1/functions", Some(&user), Some(hello.clone()))
            .status,
        403
    );
    fb.admin("POST", "/api/v1/functions", Some(hello)).expect(201);
    let r = fb.call("POST", "/api/v1/functions/hello/invoke", None, Some(json!({})));
    assert_eq!(r.expect(200), json!({"hello": "world"}));
    let logs = fb.admin("GET", "/api/v1/functions/hello/logs", None).expect(200);
    assert!(logs.to_string().contains("ok"), "the execution is logged: {logs}");

    // An infinite loop is stopped by the fuel limit.
    let spin = wasm_b64(
        r#"(module (memory (export "memory") 1)
             (func (export "handle") (result i32) (loop $l (br $l)) (i32.const 0)))"#,
    );
    fb.admin(
        "POST",
        "/api/v1/functions",
        Some(json!({"name": "spin", "wasm": spin, "access": "public",
            "limits": {"fuel": 2_000_000, "timeout_ms": 20_000}})),
    )
    .expect(201);
    let started = Instant::now();
    let r = fb.call("POST", "/api/v1/functions/spin/invoke", None, Some(json!({})));
    assert_eq!(
        (r.status, r.code().as_str()),
        (502, "function_fuel_exhausted"),
        "{}",
        r.text()
    );
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "fuel, not the clock, stopped it"
    );

    // A module that wants WASI file access is not accepted.
    let wasi = wasm_b64(
        r#"(module
             (import "wasi_snapshot_preview1" "path_open" (func (param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)))
             (memory (export "memory") 1)
             (func (export "handle") (result i32) (i32.const 0)))"#,
    );
    let r = fb.admin(
        "POST",
        "/api/v1/functions",
        Some(json!({"name": "escape", "wasm": wasi, "access": "public"})),
    );
    assert_eq!(r.status, 400, "{}", r.text());

    // A host call the function was not granted is refused (deploy or call).
    let db = wasm_b64(
        r#"(module
             (import "forgebase" "db_create" (func $db (param i32 i32) (result i32)))
             (memory (export "memory") 1)
             (data (i32.const 16) "{\"collection\":\"notes\",\"data\":{}}")
             (func (export "handle") (result i32) (drop (call $db (i32.const 16) (i32.const 32))) (i32.const 0)))"#,
    );
    let r = fb.admin(
        "POST",
        "/api/v1/functions",
        Some(json!({"name": "sneaky", "wasm": db, "access": "public", "capabilities": ["log"]})),
    );
    if r.status == 201 {
        let r = fb.call("POST", "/api/v1/functions/sneaky/invoke", None, Some(json!({})));
        assert!(r.status >= 400, "a call without the capability must fail: {}", r.text());
    } else {
        assert_eq!(r.status, 400, "{}", r.text());
    }
}

/// #19 Schedule and execute a background job: a delayed job and a cron
/// schedule run a function through the durable queue.
#[test]
fn scenario_19_schedule_and_execute_a_background_job() {
    let mut fb = Fb::start();
    deploy_hello(&fb);
    let job = fb
        .admin(
            "POST",
            "/api/v1/jobs",
            Some(
                json!({"kind": "function.invoke", "payload": {"function": "hello", "input": {"n": 1}},
                "delay_ms": 1500, "idempotency_key": "welcome-1"}),
            ),
        )
        .expect(201);
    assert_eq!(job["status"], "queued", "{job}");
    let id = job["id"].as_str().unwrap().to_owned();
    // The same idempotency key returns the first job.
    let again = fb
        .admin(
            "POST",
            "/api/v1/jobs",
            Some(json!({"kind": "function.invoke", "payload": {"function": "hello"}, "idempotency_key": "welcome-1"})),
        )
        .expect(200);
    assert_eq!(again["id"], id.as_str());
    // Not before its time.
    let now = fb.admin("GET", &format!("/api/v1/jobs/{id}"), None).expect(200);
    assert_eq!(now["status"], "queued", "{now}");
    let done = wait_for("the delayed job", Duration::from_secs(60), || {
        let j = fb.admin("GET", &format!("/api/v1/jobs/{id}"), None).json();
        (j["status"] == "succeeded").then_some(j)
    });
    assert_eq!(done["result"]["status"], 200, "{done}");
    assert_eq!(done["result"]["body"]["hello"], "world", "{done}");
    assert_eq!(done["attempts"], 1);
    let ev = fb.admin("GET", &format!("/api/v1/jobs/{id}/events"), None).expect(200);
    let names: Vec<&str> = ev["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["event"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["enqueued", "claimed", "succeeded"]);

    // A schedule every second.
    let sched = fb
        .admin(
            "POST",
            "/api/v1/jobs/schedules",
            Some(json!({"name": "tick", "cron": "* * * * * *", "kind": "function.invoke", "payload": {"function": "hello"}})),
        )
        .expect(201);
    wait_for("two scheduled runs", Duration::from_secs(60), || {
        let l = fb
            .admin("GET", "/api/v1/jobs?kind=function.invoke&status=succeeded", None)
            .json();
        (l["total"].as_i64().unwrap_or(0) >= 3).then_some(())
    });
    // The schedule survives a restart and keeps firing.
    fb.restart();
    let s = fb
        .admin(
            "GET",
            &format!("/api/v1/jobs/schedules/{}", sched["id"].as_str().unwrap()),
            None,
        )
        .expect(200);
    assert!(s["last_run_at"].is_string(), "{s}");
    let before = fb
        .admin("GET", "/api/v1/jobs?kind=function.invoke&status=succeeded", None)
        .json()["total"]
        .as_i64()
        .unwrap();
    wait_for("a run after the restart", Duration::from_secs(60), || {
        let l = fb
            .admin("GET", "/api/v1/jobs?kind=function.invoke&status=succeeded", None)
            .json();
        (l["total"].as_i64().unwrap_or(0) > before).then_some(())
    });
}

/// #20 Process a billing webhook idempotently: a signed Stripe event pays an
/// open invoice once; a forged one is refused; redeliveries (also after a
/// restart) are duplicates and change nothing.
#[test]
fn scenario_20_process_a_billing_webhook_idempotently() {
    const KEY: &str = "sk_test_51fb85scenario20";
    const WHSEC: &str = "whsec_fb85_scenario_20";
    let mut fb = Fb::start();
    fb.admin(
        "PUT",
        "/api/v1/plans/pro",
        Some(json!({"name": "Pro", "active": true, "features": []})),
    )
    .expect(200);
    // The customer pays out of band (manual provider): no outbound call is made.
    fb.admin("PUT", "/api/v1/billing/providers/manual", Some(json!({})))
        .expect(200);
    fb.admin(
        "PUT",
        "/api/v1/billing/providers/stripe",
        Some(json!({"api_key": KEY, "webhook_secret": WHSEC})),
    )
    .expect(200);
    fb.admin(
        "PUT",
        "/api/v1/billing/products/pro",
        Some(json!({"name": "Pro", "kind": "plan", "plan_key": "pro", "active": true})),
    )
    .expect(200);
    fb.admin(
        "PUT",
        "/api/v1/billing/prices/pro-m",
        Some(
            json!({"product": "pro", "type": "recurring", "interval": "month", "currency": "USD",
            "unit_amount": 2900, "active": true}),
        ),
    )
    .expect(200);
    let (ann, ann_id) = fb.user("ann@example.com");
    fb.call(
        "PUT",
        "/api/v1/billing/customer",
        Some(&ann),
        Some(json!({"email": "ann@example.com", "provider": "manual"})),
    )
    .expect(200);
    let c = fb
        .call(
            "POST",
            "/api/v1/billing/subscriptions",
            Some(&ann),
            Some(json!({"items": [{"price": "pro-m"}], "idempotency_key": "s20"})),
        )
        .expect(201);
    let inv = c["invoice"]["id"].as_str().unwrap_or_else(|| panic!("{c}")).to_owned();
    let sub = c["subscription"]["id"].as_str().unwrap().to_owned();
    assert_eq!(c["invoice"]["status"], "open", "{c}");

    let t = now_secs();
    let body = json!({
        "id": "evt_fb85", "object": "event", "type": "payment_intent.succeeded", "created": t, "livemode": false,
        "data": {"object": {"id": "pi_fb85", "amount": 2900, "amount_received": 2900, "currency": "usd",
            "payment_method": "pm_fb85", "metadata": {"forgebase_invoice": inv}}}
    })
    .to_string();
    let signed = |secret: &str| {
        let mut m = format!("{t}.").into_bytes();
        m.extend_from_slice(body.as_bytes());
        format!("t={t},v1={}", hmac_sha256_hex(secret.as_bytes(), &m))
    };
    let deliver = |fb: &Fb, sig: String| {
        fb.send(
            "POST",
            "/api/v1/billing/webhooks/stripe",
            None,
            &[
                ("Content-Type", "application/json".to_owned()),
                ("Stripe-Signature", sig),
            ],
            body.as_bytes(),
        )
    };
    let r = deliver(&fb, signed("whsec_not_the_secret"));
    assert_eq!(
        (r.status, r.code().as_str()),
        (400, "invalid_signature"),
        "{}",
        r.text()
    );
    let r = deliver(&fb, signed(WHSEC));
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(r.text().contains("\"processed\""), "{}", r.text());
    let r = deliver(&fb, signed(WHSEC));
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(r.text().contains("\"duplicate\""), "{}", r.text());

    let check = |fb: &Fb| {
        let i = fb
            .admin("GET", &format!("/api/v1/billing/invoices/{inv}"), None)
            .expect(200);
        assert_eq!(
            (i["status"].as_str(), i["amount_paid"].as_i64()),
            (Some("paid"), Some(2900)),
            "{i}"
        );
        let p = fb
            .admin(
                "GET",
                &format!(
                    "/api/v1/billing/payments?subject={}",
                    enc(&format!("user:users/{ann_id}"))
                ),
                None,
            )
            .expect(200);
        let n = p["items"].as_array().or_else(|| p.as_array()).map(Vec::len);
        assert_eq!(n, Some(1), "one payment: {p}");
        let s = fb
            .admin("GET", &format!("/api/v1/billing/subscriptions/{sub}"), None)
            .expect(200);
        assert_eq!(s["status"], "active", "{s}");
    };
    check(&fb);
    fb.restart();
    let r = deliver(&fb, signed(WHSEC));
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(r.text().contains("\"duplicate\""), "after a restart: {}", r.text());
    check(&fb);
    let ev = fb.admin("GET", "/api/v1/billing/webhook-events", None).expect(200);
    let items = ev["items"].as_array().or_else(|| ev.as_array()).unwrap().clone();
    assert_eq!(items.len(), 1, "{ev}");
    assert_eq!(
        (items[0]["attempts"].as_i64(), items[0]["status"].as_str()),
        (Some(3), Some("processed"))
    );
}

// ---------------------------------------------------------------------------
// 21-25: restart, backup, rebuild, concurrency, malice
// ---------------------------------------------------------------------------

/// #21 Restart the server and verify persistence of every model at once:
/// records, users and sessions, objects, vectors, graph, KV, functions.
#[test]
fn scenario_21_restart_the_server_and_verify_persistence() {
    let mut fb = Fb::start();
    fb.collection(json!({"name": "notes", "fields": [{"name": "text", "type": "text"}]}));
    let rec = fb
        .admin(
            "POST",
            "/api/v1/collections/notes/records",
            Some(json!({"text": "keep me"})),
        )
        .expect(201);
    let (user, _) = fb.user("kept@example.com");
    fb.admin("POST", "/api/v1/storage/buckets", Some(json!({"name": "b"})))
        .expect(201);
    let admin = fb.admin.clone();
    fb.send(
        "PUT",
        "/api/v1/storage/buckets/b/objects/k.txt",
        Some(&admin),
        &[("Content-Type", "text/plain".to_owned())],
        b"object bytes",
    )
    .expect(201);
    vector_kb(&fb, 20);
    graph_seed(&fb);
    let r = fb.admin(
        "PUT",
        "/api/v1/kv/app/strings/greeting",
        Some(json!({"value": "hello"})),
    );
    assert!(r.status / 100 == 2, "{} {}", r.status, r.text());
    deploy_hello(&fb);

    fb.restart();
    let r = fb
        .admin(
            "GET",
            &format!("/api/v1/collections/notes/records/{}", rec["id"].as_str().unwrap()),
            None,
        )
        .expect(200);
    assert_eq!(r["text"], "keep me");
    assert_eq!(
        fb.call("GET", "/api/auth/me", Some(&user), None).status,
        200,
        "the session persisted"
    );
    let o = fb.send(
        "GET",
        "/api/v1/storage/buckets/b/objects/k.txt",
        Some(&fb.admin.clone()),
        &[],
        b"",
    );
    assert_eq!((o.status, o.text().as_str()), (200, "object bytes"));
    assert_eq!(top_ids(&fb, [3.1, 0.0, 0.0], 1), ["p3"]);
    assert_eq!(fb.admin("GET", "/api/v1/graph/edges/f3", None).status, 200);
    let kv = fb.admin("GET", "/api/v1/kv/app/keys/greeting", None).expect(200);
    assert!(kv.to_string().contains("\"hello\""), "{kv}");
    let r = fb.call("POST", "/api/v1/functions/hello/invoke", None, Some(json!({})));
    assert_eq!(r.expect(200)["hello"], "world");
}

/// #22 Back up and restore the database: an online backup through the CLI
/// while serving, data loss, restore, and every model back.
#[test]
fn scenario_22_back_up_and_restore_the_database() {
    // `backup` on PostgreSQL arrives with FB-150 (pg_basebackup); until then the
    // command refuses ("snapshot_to is not supported on the postgres backend").
    if common::postgres() {
        eprintln!("skipped on postgres: backups are FB-150");
        return;
    }
    let mut fb = Fb::start();
    fb.collection(json!({"name": "notes", "fields": [{"name": "text", "type": "text"}]}));
    let rows: Vec<Json> = (0..25).map(|i| json!({"text": format!("note {i}")})).collect();
    fb.admin("POST", "/api/v1/collections/notes/records", Some(json!(rows)))
        .expect(201);
    fb.admin("POST", "/api/v1/storage/buckets", Some(json!({"name": "b"})))
        .expect(201);
    let admin = fb.admin.clone();
    fb.send(
        "PUT",
        "/api/v1/storage/buckets/b/objects/k.txt",
        Some(&admin),
        &[("Content-Type", "text/plain".to_owned())],
        b"object bytes",
    )
    .expect(201);
    vector_kb(&fb, 20);
    let out = fb.cli_ok(&["backup", "--label", "qa"]);
    assert!(out.contains("backup written"), "{out}");
    assert!(fb.cli_ok(&["backup", "verify", "latest"]).contains("backup is valid"));
    // Written after the backup: gone after the restore.
    fb.admin(
        "POST",
        "/api/v1/collections/notes/records",
        Some(json!({"text": "after"})),
    )
    .expect(201);
    let r = fb.cli(&["restore", "latest", "--force"], None);
    assert!(!r.status.success(), "a restore under a running server is refused");
    fb.stop();

    let data = fb.data_dir();
    for f in ["forgebase.db", "forgebase.db-wal", "forgebase.db-shm"] {
        let _ = std::fs::remove_file(data.join(f));
    }
    std::fs::write(data.join("forgebase.db"), b"this is not a database").unwrap();
    let _ = std::fs::remove_dir_all(data.join("storage"));
    let _ = std::fs::remove_dir_all(data.join("indexes"));
    let out = fb.cli_ok(&["restore", "latest", "--force"]);
    assert!(!out.is_empty());
    let doctor = fb.cli(&["doctor", "--json"], None);
    assert!(
        doctor.status.success(),
        "doctor after restore: {}",
        text(&doctor.stdout)
    );

    fb.serve();
    fb.admin = fb.admin_login();
    let list = fb
        .admin("GET", "/api/v1/collections/notes/records?per_page=100", None)
        .expect(200);
    assert_eq!(list["total_items"], 25, "the post-backup write is not there: {list}");
    let o = fb.send(
        "GET",
        "/api/v1/storage/buckets/b/objects/k.txt",
        Some(&fb.admin.clone()),
        &[],
        b"",
    );
    assert_eq!((o.status, o.text().as_str()), (200, "object bytes"));
    let vec_ready = wait_for("the vector index", Duration::from_secs(30), || {
        let s = fb.admin("GET", "/api/v1/vector/collections/kb/stats", None).json();
        (s["indexes"][0]["state"] == "ready").then_some(s)
    });
    assert_eq!(vec_ready["items"], 20);
    assert_eq!(top_ids(&fb, [3.1, 0.0, 0.0], 1), ["p3"]);
}

/// #23 Rebuild derived indexes: vector ANN files deleted offline are rebuilt
/// from the database on start; an online vector rebuild and a data index
/// rebuild keep answers identical.
#[test]
fn scenario_23_rebuild_derived_indexes() {
    let mut fb = Fb::start();
    vector_kb(&fb, 200);
    items_collection(&fb);
    fb.admin(
        "POST",
        "/api/v1/indexes",
        Some(json!({"name": "idx_items_cat", "collection": "items", "keys": [{"path": "category"}]})),
    )
    .expect(201);
    let expected = top_ids(&fb, [41.2, 0.0, 0.0], 3);
    assert_eq!(expected, ["p41", "p42", "p40"]);

    // Online rebuild.
    let r = fb
        .admin(
            "POST",
            "/api/v1/vector/collections/kb/rebuild",
            Some(json!({"field": "emb"})),
        )
        .expect(202);
    assert_eq!(r["indexes"][0]["field"], "emb", "{r}");
    wait_for("the manual rebuild", Duration::from_secs(60), || {
        let s = fb.admin("GET", "/api/v1/vector/collections/kb/stats", None).json();
        (s["indexes"][0]["state"] == "ready" && s["indexes"][0]["last_rebuild"]["reason"] == "manual").then_some(())
    });
    assert_eq!(top_ids(&fb, [41.2, 0.0, 0.0], 3), expected);
    let rebuilt = fb
        .admin("POST", "/api/v1/indexes/idx_items_cat/rebuild", Some(json!({})))
        .expect(200);
    assert_eq!(rebuilt["name"], "idx_items_cat");
    let plan = fb
        .admin(
            "POST",
            "/api/v1/collections/items/records/explain",
            Some(json!({"filter": "category = 'c3'"})),
        )
        .expect(200);
    if !common::postgres() {
        assert_eq!(plan["indexes_used"], json!(["idx_items_cat"]), "{plan}");
    }

    // Offline: the derived files are deleted; the next start rebuilds them.
    fb.stop();
    let files = fb.data_dir().join("indexes/vector");
    assert!(files.is_dir(), "vector index files exist after shutdown");
    std::fs::remove_dir_all(&files).unwrap();
    fb.serve();
    fb.admin = fb.admin_login();
    let s = wait_for("the rebuild after a restart", Duration::from_secs(60), || {
        let s = fb.admin("GET", "/api/v1/vector/collections/kb/stats", None).json();
        (s["indexes"][0]["state"] == "ready").then_some(s)
    });
    assert_eq!(s["indexes"][0]["last_rebuild"]["reason"], "missing", "{s}");
    assert_eq!(s["indexes"][0]["indexed_vectors"], 200);
    assert_eq!(top_ids(&fb, [41.2, 0.0, 0.0], 3), expected);
}

/// #24 Test concurrent read and write workloads: parallel writers and readers
/// over HTTP, no errors, no lost writes, and optimistic concurrency on one
/// record lets exactly one of the racing updates win.
#[test]
fn scenario_24_concurrent_read_and_write_workloads() {
    let fb = Fb::start();
    fb.collection(json!({"name": "events", "fields": [
        {"name": "writer", "type": "number"}, {"name": "seq", "type": "number"}]}));
    let (addr, token) = (fb.addr.clone(), fb.admin.clone());
    const WRITERS: u32 = 8;
    const PER_WRITER: u32 = 40;
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let call = |addr: &str, token: &str, m: &str, p: &str, b: Option<Json>| {
        let body = b.map(|b| b.to_string()).unwrap_or_default();
        raw(
            addr,
            m,
            p,
            &[
                ("Authorization", format!("Bearer {token}")),
                ("Content-Type", "application/json".to_owned()),
            ],
            body.as_bytes(),
        )
    };
    let writers: Vec<_> = (0..WRITERS)
        .map(|w| {
            let (addr, token) = (addr.clone(), token.clone());
            std::thread::spawn(move || {
                let mut ids = Vec::new();
                for seq in 0..PER_WRITER {
                    let r = call(
                        &addr,
                        &token,
                        "POST",
                        "/api/v1/collections/events/records",
                        Some(json!({"writer": w, "seq": seq})),
                    );
                    assert_eq!(r.status, 201, "writer {w} seq {seq}: {}", r.text());
                    ids.push(r.json()["id"].as_str().unwrap().to_owned());
                }
                ids
            })
        })
        .collect();
    let readers: Vec<_> = (0..4)
        .map(|_| {
            let (addr, token, stop) = (addr.clone(), token.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut reads = 0u32;
                let mut last = 0i64;
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    let r = call(
                        &addr,
                        &token,
                        "GET",
                        "/api/v1/collections/events/records?per_page=1",
                        None,
                    );
                    assert_eq!(r.status, 200, "{}", r.text());
                    let n = r.json()["total_items"].as_i64().unwrap();
                    assert!(n >= last, "a reader saw the count go back: {last} -> {n}");
                    last = n;
                    reads += 1;
                }
                reads
            })
        })
        .collect();
    let mut all: Vec<String> = writers.into_iter().flat_map(|h| h.join().unwrap()).collect();
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let reads: u32 = readers.into_iter().map(|h| h.join().unwrap()).sum();
    assert!(reads > 0);
    all.sort();
    all.dedup();
    assert_eq!(all.len(), (WRITERS * PER_WRITER) as usize, "every write got its own id");
    let total = fb
        .admin("GET", "/api/v1/collections/events/records?per_page=1", None)
        .expect(200);
    assert_eq!(total["total_items"], WRITERS * PER_WRITER, "no lost writes");
    for w in 0..WRITERS {
        let r = fb
            .admin(
                "GET",
                &format!(
                    "/api/v1/collections/events/records?per_page=1&filter={}",
                    enc(&format!("writer = {w}"))
                ),
                None,
            )
            .expect(200);
        assert_eq!(r["total_items"], PER_WRITER, "writer {w}");
    }

    // Racing updates with the same version: exactly one wins, the rest get 409.
    fb.collection(json!({"name": "counters", "fields": [{"name": "n", "type": "number"}]}));
    let c = fb
        .admin("POST", "/api/v1/collections/counters/records", Some(json!({"n": 0})))
        .expect(201);
    let cid = c["id"].as_str().unwrap().to_owned();
    let version = c["_version"].as_i64().unwrap_or(1);
    let racers: Vec<_> = (0..8)
        .map(|i| {
            let (addr, token, cid) = (addr.clone(), token.clone(), cid.clone());
            std::thread::spawn(move || {
                raw(
                    &addr,
                    "PATCH",
                    &format!("/api/v1/collections/counters/records/{cid}"),
                    &[
                        ("Authorization", format!("Bearer {token}")),
                        ("Content-Type", "application/json".to_owned()),
                        ("If-Match", format!("\"{version}\"")),
                    ],
                    json!({"n": i + 1}).to_string().as_bytes(),
                )
                .status
            })
        })
        .collect();
    let statuses: Vec<u16> = racers.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(statuses.iter().filter(|s| **s == 200).count(), 1, "{statuses:?}");
    assert_eq!(statuses.iter().filter(|s| **s == 409).count(), 7, "{statuses:?}");
}

/// #25 Test malicious inputs and permission bypass attempts against the
/// running binary: injection through filters, sort and SQL; path traversal in
/// object keys; oversized and malformed bodies; privilege escalation through
/// self-service fields; data still intact afterwards. The in-process FB-80
/// suite (crates/forgebase-server/tests/security_bypass.rs) covers SSRF and
/// the rest of the threat model.
#[test]
fn scenario_25_malicious_inputs_and_permission_bypass_attempts() {
    let fb = Fb::start();
    fb.collection(json!({"name": "secrets", "fields": [
        {"name": "owner", "type": "text"}, {"name": "value", "type": "text"}]}));
    fb.admin(
        "POST",
        "/api/v1/collections/secrets/records",
        Some(json!({"owner": "root", "value": "s3cr3t"})),
    )
    .expect(201);
    let (user, uid) = fb.user("mallory@example.com");

    // Injection through the filter language: refused or harmless, never executed.
    for f in [
        "owner = 'x' OR 1=1",
        "owner = '' ; DROP TABLE secrets; --",
        "owner = 'x') UNION SELECT * FROM _forgebase_admins --",
        "owner ~ '%' && value = (SELECT value FROM secrets)",
    ] {
        let r = fb.admin(
            "GET",
            &format!("/api/v1/collections/secrets/records?filter={}", enc(f)),
            None,
        );
        assert!(
            r.status == 400 || (r.status == 200 && r.json()["items"].as_array().is_some_and(Vec::is_empty)),
            "filter {f:?}: {} {}",
            r.status,
            r.text()
        );
    }
    let r = fb.admin(
        "GET",
        &format!(
            "/api/v1/collections/secrets/records?sort={}",
            enc("owner; DROP TABLE secrets")
        ),
        None,
    );
    assert_eq!(r.status, 400, "{}", r.text());
    // The SQL editor runs one statement and keeps system tables read-only.
    let r = fb.admin(
        "POST",
        "/api/v1/sql",
        Some(json!({"sql": "SELECT 1; DROP TABLE secrets"})),
    );
    assert_eq!((r.status, r.code().as_str()), (400, "multiple_statements"));
    let r = fb.admin(
        "POST",
        "/api/v1/sql",
        Some(json!({"sql": "DELETE FROM _forgebase_collections"})),
    );
    assert_eq!(r.status, 403, "{}", r.text());
    let r = fb.admin(
        "POST",
        "/api/v1/sql",
        Some(json!({"sql": "ATTACH DATABASE '/tmp/x.db' AS x"})),
    );
    assert_eq!(r.status, 400, "{}", r.text());

    // Users get nothing from a collection without rules (fail closed).
    let r = fb.call("GET", "/api/v1/collections/secrets/records", Some(&user), None);
    assert!(r.status == 403 || r.status == 404, "{} {}", r.status, r.text());
    assert!(!r.text().contains("s3cr3t"));
    // Self-service cannot grant roles or verify itself.
    let r = fb.call(
        "PATCH",
        &format!("/api/v1/collections/users/records/{uid}"),
        Some(&user),
        Some(json!({"roles": ["admin"], "verified": true})),
    );
    assert!(
        r.status >= 400,
        "privilege escalation through a profile update: {}",
        r.text()
    );
    let me = fb.call("GET", "/api/auth/me", Some(&user), None).expect(200);
    assert_ne!(me["subject"]["type"], "admin");
    assert_eq!(fb.call("GET", "/api/auth/audit", Some(&user), None).status, 403);

    // Object keys cannot walk out of the bucket: they are opaque names in the
    // database (docs/security.md), so a `..` key is either refused or stored
    // as a plain name, and no file appears outside the content-addressed
    // blob store. The database file is untouched.
    fb.admin("POST", "/api/v1/storage/buckets", Some(json!({"name": "b"})))
        .expect(201);
    let admin = fb.admin.clone();
    let db_len = common::file_len(&fb.data_dir().join("forgebase.db"));
    for (key, name) in [
        ("..%2F..%2Fforgebase.db", "../../forgebase.db"),
        ("%2E%2E/%2E%2E/escape.txt", "../../escape.txt"),
        ("a/../../escape.txt", "a/../../escape.txt"),
        ("..%5C..%5Cescape.txt", "..\\..\\escape.txt"),
    ] {
        let r = fb.send(
            "PUT",
            &format!("/api/v1/storage/buckets/b/objects/{key}"),
            Some(&admin),
            &[("Content-Type", "text/plain".to_owned())],
            b"x",
        );
        match r.status {
            400 | 404 => {}
            201 => {
                assert_eq!(r.json()["key"], name, "stored under its literal name: {}", r.text());
                let back = fb.send(
                    "GET",
                    &format!("/api/v1/storage/buckets/b/objects/{key}"),
                    Some(&admin),
                    &[],
                    b"",
                );
                assert_eq!((back.status, back.body.as_slice()), (200, &b"x"[..]));
            }
            s => panic!("key {key}: {s} {}", r.text()),
        }
    }
    for dir in [fb.data_dir(), fb.proj.clone(), fb.data_dir().join("storage")] {
        assert!(!dir.join("escape.txt").exists(), "{}", dir.display());
    }
    assert!(common::file_len(&fb.data_dir().join("forgebase.db")) >= db_len);
    let ok = fb.admin(
        "POST",
        "/api/v1/sql",
        Some(json!({"sql": "SELECT count(*) FROM _forgebase_collections"})),
    );
    assert_eq!(ok.status, 200, "the database still opens: {}", ok.text());
    // Declared image types must carry image bytes.
    let r = fb.send(
        "PUT",
        "/api/v1/storage/buckets/b/objects/x.png",
        Some(&admin),
        &[("Content-Type", "image/png".to_owned())],
        b"<script>alert(1)</script>",
    );
    assert_eq!(r.status, 400, "{}", r.text());

    // Malformed and oversized bodies.
    let r = fb.send(
        "POST",
        "/api/v1/collections/secrets/records",
        Some(&admin),
        &[("Content-Type", "application/json".to_owned())],
        b"{\"owner\": ",
    );
    assert_eq!((r.status, r.code().as_str()), (400, "invalid_json"));
    let r = fb.send(
        "POST",
        "/api/v1/collections/secrets/records",
        Some(&admin),
        &[("Content-Type", "text/plain".to_owned())],
        b"owner=x",
    );
    assert_eq!(r.status, 415, "{}", r.text());
    let big = format!("{{\"owner\": \"{}\"}}", "a".repeat(40 * 1024 * 1024));
    let r = fb.send(
        "POST",
        "/api/v1/collections/secrets/records",
        Some(&admin),
        &[("Content-Type", "application/json".to_owned())],
        big.as_bytes(),
    );
    assert!(
        r.status == 413 || r.status == 0,
        "a 40 MiB JSON body: {} {}",
        r.status,
        r.text()
    );
    // Request ids from the client are sanitised.
    let r = fb.send(
        "GET",
        "/health",
        None,
        &[("X-Request-Id", "bad<script>id".to_owned())],
        b"",
    );
    let rid = r.header("x-request-id").unwrap_or_default();
    assert!(!rid.is_empty() && !rid.contains('<'), "request id {rid:?}");

    // Everything is still in place.
    let list = fb.admin("GET", "/api/v1/collections/secrets/records", None).expect(200);
    assert_eq!(list["items"][0]["value"], "s3cr3t");
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    let health = fb.call("GET", "/health/ready", None, None);
    assert_eq!(health.status, 200, "{}", health.text());
}
