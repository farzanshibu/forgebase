//! Black-box tests of the developer workflow (ADR-0038): `forgebase dev`
//! (emulator, seed, watch, persist, reset), `forgebase gen types` and the
//! function debugging commands, against the real binary (SQLite; PostgreSQL
//! with `FORGEBASE_TEST_BACKEND=postgres`, see docs/qa.md).
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../../../tests/integration/common/mod.rs"]
mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// A `forgebase` command with no inherited ForgeBase variables, an empty HOME and a private TMPDIR.
fn bare(cwd: &Path, tmp: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_forgebase"));
    cmd.current_dir(cwd);
    for (k, _) in std::env::vars_os() {
        let k = k.to_string_lossy().into_owned();
        if (k.starts_with("FORGEBASE_") && k != "FORGEBASE_PG_ENGINE_DIR") || k.starts_with("FB_") {
            cmd.env_remove(k);
        }
    }
    cmd.env_remove("SLACK_WEBHOOK_URL");
    std::fs::create_dir_all(tmp).unwrap();
    cmd.env("HOME", tmp).env("TMPDIR", tmp).env("NO_COLOR", "1");
    common::backend_env(&mut cmd);
    cmd
}

/// The `--database` flag that picks this run's backend.
fn engine() -> &'static str {
    if common::postgres() { "postgres" } else { "sqlite" }
}

/// A scratch directory with an app scaffolded by `forgebase new`.
struct App {
    root: tempfile::TempDir,
}

impl App {
    fn new(template: &str) -> Self {
        // Short path: the bundled PostgreSQL's Unix socket path has a length limit.
        let root = tempfile::Builder::new().prefix("fbd").tempdir_in(short_tmp()).unwrap();
        let app = Self { root };
        let out = bare(&app.root(), &app.tmp())
            .args(["new", "app", "--template", template])
            .output()
            .unwrap();
        assert!(out.status.success(), "new: {}", text(&out.stderr));
        app
    }

    fn root(&self) -> PathBuf {
        self.root.path().canonicalize().unwrap()
    }

    /// The app directory (holds `forgebase/`).
    fn dir(&self) -> PathBuf {
        self.root().join("app")
    }

    fn project(&self) -> PathBuf {
        self.dir().join("forgebase")
    }

    fn tmp(&self) -> PathBuf {
        self.root().join("t")
    }

    /// `forgebase <args>` in the app directory.
    fn cli(&self, args: &[&str]) -> Output {
        bare(&self.dir(), &self.tmp()).args(args).output().unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let o = self.cli(args);
        assert_eq!(
            o.status.code(),
            Some(0),
            "forgebase {args:?}\nstdout: {}\nstderr: {}",
            text(&o.stdout),
            text(&o.stderr)
        );
        text(&o.stdout)
    }

    /// Starts `forgebase dev --port 0 --json` with extra arguments.
    fn dev(&self, extra: &[&str]) -> Dev {
        Dev::start(self, extra)
    }
}

fn short_tmp() -> PathBuf {
    let p = PathBuf::from("/tmp/fbdev");
    if cfg!(unix) {
        std::fs::create_dir_all(&p).unwrap();
        p
    } else {
        std::env::temp_dir()
    }
}

/// A running `forgebase dev --json`: stdout lines arrive on a channel, stderr goes to a file.
struct Dev {
    child: Option<Child>,
    lines: Receiver<String>,
    log: PathBuf,
    ready: Value,
}

impl Dev {
    fn start(app: &App, extra: &[&str]) -> Self {
        let log = app.root().join("dev.log");
        let mut args = vec!["dev", "--port", "0", "--json", "--database", engine()];
        args.extend_from_slice(extra);
        let mut child = bare(&app.dir(), &app.tmp())
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(std::fs::File::create(&log).unwrap())
            .spawn()
            .unwrap();
        let out = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for l in BufReader::new(out).lines().map_while(Result::ok) {
                if tx.send(l).is_err() {
                    break;
                }
            }
        });
        let mut dev = Self {
            child: Some(child),
            lines: rx,
            log,
            ready: Value::Null,
        };
        let ready = dev.wait_for(common::startup_timeout(), |v| v["event"] == "ready");
        dev.ready = ready;
        dev
    }

    /// The next JSON line that satisfies `pred`; panics on timeout (with the server's log).
    fn wait_for(&mut self, timeout: Duration, pred: impl Fn(&Value) -> bool) -> Value {
        let end = Instant::now() + timeout;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left.max(Duration::from_millis(1))) {
                Ok(l) => {
                    if let Ok(v) = serde_json::from_str::<Value>(&l)
                        && pred(&v)
                    {
                        return v;
                    }
                }
                Err(_) => panic!(
                    "timed out waiting for a line from forgebase dev\nstderr:\n{}",
                    std::fs::read_to_string(&self.log).unwrap_or_default()
                ),
            }
            if Instant::now() > end {
                panic!(
                    "timed out\nstderr:\n{}",
                    std::fs::read_to_string(&self.log).unwrap_or_default()
                );
            }
        }
    }

    fn url(&self) -> String {
        self.ready["url"].as_str().unwrap().to_owned()
    }

    fn addr(&self) -> String {
        self.url().trim_start_matches("http://").to_owned()
    }

    fn key(&self) -> String {
        self.ready["api_key"].as_str().unwrap().to_owned()
    }

    fn data_dir(&self) -> PathBuf {
        PathBuf::from(self.ready["data_dir"].as_str().unwrap())
    }

    fn api(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        call(&self.addr(), method, path, Some(&self.key()), body)
    }

    /// Ctrl-C: asks for a clean shutdown and returns the exit code.
    fn interrupt(&mut self) -> Option<i32> {
        let mut child = self.child.take().unwrap();
        let _ = Command::new("kill")
            .args(["-INT", &child.id().to_string()])
            .status()
            .unwrap();
        let end = Instant::now() + Duration::from_secs(60);
        loop {
            if let Some(s) = child.try_wait().unwrap() {
                return s.code();
            }
            if Instant::now() > end {
                let _ = child.kill();
                panic!("forgebase dev did not stop after SIGINT");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for Dev {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

fn call(addr: &str, method: &str, path: &str, bearer: Option<&str>, body: Option<Value>) -> (u16, Value) {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
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
    } else if method != "GET" {
        head.push_str("Content-Length: 0\r\n");
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
    let chunked = h.to_ascii_lowercase().contains("transfer-encoding: chunked");
    let b = if chunked { dechunk(b) } else { b.to_owned() };
    (status, serde_json::from_str(&b).unwrap_or(Value::Null))
}

fn dechunk(mut raw: &str) -> String {
    let mut out = String::new();
    while let Some((size, rest)) = raw.split_once("\r\n") {
        let n = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
        if n == 0 || rest.len() < n {
            break;
        }
        out.push_str(&rest[..n]);
        raw = rest[n..].trim_start_matches("\r\n");
    }
    out
}

fn titles(v: &Value) -> Vec<String> {
    let mut t: Vec<String> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["title"].as_str().unwrap().to_owned())
        .collect();
    t.sort();
    t
}

fn append(path: &Path, text: &str) {
    let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    f.write_all(text.as_bytes()).unwrap();
}

// ---------------------------------------------------------------------------
// forgebase dev

#[test]
fn dev_applies_the_project_seeds_serves_and_deletes_its_directory_on_exit() {
    let app = App::new("basic");
    let mut dev = app.dev(&["--seed"]);
    assert_eq!(dev.ready["applied"], 3, "{}", dev.ready);
    assert_eq!(dev.ready["persist"], false);
    assert_eq!(dev.ready["seed"]["collections"]["todos"]["created"], 3);
    assert_eq!(dev.ready["seed"]["errors"], json!([]));
    assert!(dev.key().starts_with("fbk_"));
    let data_dir = dev.data_dir();
    assert!(data_dir.is_dir(), "the ephemeral data directory exists while it runs");
    assert!(
        data_dir.starts_with(app.tmp()),
        "it lives in the temp directory, {data_dir:?}"
    );

    // The project's collection serves the seeded records with the dev key.
    let (s, page) = dev.api("GET", "/api/v1/collections/todos/records", None);
    assert_eq!(s, 200, "{page}");
    assert_eq!(
        titles(&page),
        [
            "Deploy from CI",
            "Model the app's collections",
            "Read the ForgeBase docs"
        ]
    );
    // The administrator can sign in with the printed credentials.
    let admin = &dev.ready["admin"];
    let (s, login) = call(
        &dev.addr(),
        "POST",
        "/api/auth/admins/login",
        None,
        Some(json!({"email": admin["email"], "password": admin["password"]})),
    );
    assert_eq!(s, 200, "{login}");
    // The project's function and bucket exist.
    assert_eq!(dev.api("GET", "/api/v1/functions/hello", None).0, 200);

    // `dev seed` finds the running emulator and is idempotent.
    for _ in 0..2 {
        let out = app.ok(&["dev", "seed"]);
        assert!(out.contains("0 created, 0 updated, 3 unchanged"), "{out}");
    }
    let (_, page) = dev.api("GET", "/api/v1/collections/todos/records", None);
    assert_eq!(page["items"].as_array().unwrap().len(), 3, "no copies");

    // Lines with an id are upserted by it: new, changed, then unchanged.
    let seed = app.project().join("seed/todos.jsonl");
    append(&seed, "{\"id\": \"fixed1\", \"title\": \"Pinned\", \"done\": false}\n");
    let out = app.ok(&["dev", "seed"]);
    assert!(out.contains("1 created, 0 updated, 3 unchanged"), "{out}");
    let changed = std::fs::read_to_string(&seed)
        .unwrap()
        .replace("Pinned", "Pinned again");
    std::fs::write(&seed, changed).unwrap();
    let out = app.ok(&["dev", "seed"]);
    assert!(out.contains("0 created, 1 updated, 3 unchanged"), "{out}");
    let out = app.ok(&["dev", "seed"]);
    assert!(out.contains("0 created, 0 updated, 4 unchanged"), "{out}");
    assert_eq!(
        dev.api("GET", "/api/v1/collections/todos/records/fixed1", None).1["title"],
        "Pinned again"
    );

    // Ctrl-C stops everything and deletes the data directory, and the registry entry.
    assert_eq!(dev.interrupt(), Some(0));
    assert!(!data_dir.exists(), "{data_dir:?} was deleted");
    let leftovers: Vec<_> = std::fs::read_dir(app.tmp())
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("fbdev-"))
        .collect();
    assert!(leftovers.is_empty(), "left behind: {leftovers:?}");
    let registry = app.tmp().join("forgebase-dev");
    let entries = std::fs::read_dir(&registry).map(|r| r.count()).unwrap_or(0);
    assert_eq!(entries, 0, "the registry entry is removed");
    let out = app.cli(&["dev", "seed"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("no running dev emulator"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn dev_starts_a_project_whose_function_secret_variable_is_not_set() {
    // The `functions` template reads SLACK_WEBHOOK_URL; the emulator gives the secret a placeholder instead of failing.
    let app = App::new("functions");
    let mut dev = app.dev(&["--seed"]);
    assert_eq!(dev.ready["applied"], 5, "{}", dev.ready);
    let (s, secrets) = dev.api("GET", "/api/v1/functions/order-created/secrets", None);
    assert_eq!(s, 200, "{secrets}");
    assert!(secrets.to_string().contains("SLACK_WEBHOOK_URL"), "{secrets}");
    let log = std::fs::read_to_string(&dev.log).unwrap();
    assert!(log.contains("SLACK_WEBHOOK_URL is not set"), "{log}");
    assert!(log.contains("placeholder"), "{log}");
    assert_eq!(dev.interrupt(), Some(0));
}

#[test]
fn dev_reports_seed_problems_with_file_and_line() {
    let app = App::new("basic");
    let seed = app.project().join("seed/todos.jsonl");
    // Line 4 is valid JSON the collection rejects (title is required).
    append(&seed, "{\"done\": true}\n");
    let out = app.cli(&["dev", "--port", "0", "--database", engine(), "--seed"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    let err = text(&out.stderr) + &text(&out.stdout);
    assert!(err.contains("seed/todos.jsonl:4:"), "{err}");
    assert!(err.contains("title"), "{err}");
    assert!(
        err.contains("seed todos: 3 created"),
        "the good lines still load\n{err}"
    );
    // A line that is not JSON stops the start before anything runs, with its line number.
    append(&seed, "not json\n");
    let out = app.cli(&["dev", "--port", "0", "--database", engine(), "--seed"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("seed/todos.jsonl:5: invalid JSON"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn dev_watch_applies_a_new_collection_and_survives_a_broken_file() {
    let app = App::new("basic");
    let mut dev = app.dev(&["--watch"]);
    assert_eq!(dev.api("GET", "/api/v1/collections/notes", None).0, 404);

    // A new collection file is picked up.
    let notes = app.project().join("collections/notes.json");
    std::fs::write(
        &notes,
        json!({"type": "base", "fields": [{"name": "text", "type": "text", "required": true}]}).to_string(),
    )
    .unwrap();
    let ev = dev.wait_for(Duration::from_secs(60), |v| v["event"] == "reapplied");
    assert_eq!(ev["applied"], 1, "{ev}");
    assert_eq!(ev["failed"], json!([]));
    assert!(
        ev["changed"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c == "collections/notes.json"),
        "{ev}"
    );
    assert_eq!(dev.api("GET", "/api/v1/collections/notes", None).0, 200);
    let (s, rec) = dev.api("POST", "/api/v1/collections/notes/records", Some(json!({"text": "hi"})));
    assert_eq!(s, 201, "{rec}");

    // A broken file does not stop the emulator; fixing it applies the change.
    std::fs::write(&notes, "{ not json").unwrap();
    let ev = dev.wait_for(Duration::from_secs(60), |v| v["event"] == "reapplied");
    assert_eq!(ev["applied"], 0);
    assert!(
        ev["failed"][0].as_str().unwrap().contains("notes.json"),
        "the error names the file: {ev}"
    );
    assert_eq!(
        dev.api("GET", "/api/v1/collections/notes", None).0,
        200,
        "still serving"
    );

    // A destructive change (dropping a field with data) is allowed in dev, and said so.
    std::fs::write(
        &notes,
        json!({"type": "base", "fields": [{"name": "body", "type": "text"}]}).to_string(),
    )
    .unwrap();
    let ev = dev.wait_for(Duration::from_secs(60), |v| v["event"] == "reapplied");
    assert_eq!(ev["applied"], 1, "{ev}");
    assert_eq!(ev["failed"], json!([]), "{ev}");
    let (_, coll) = dev.api("GET", "/api/v1/collections/notes", None);
    let fields: Vec<&str> = coll["fields"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["system"] != true)
        .map(|f| f["name"].as_str().unwrap())
        .collect();
    assert_eq!(fields, ["body"]);
    let log = std::fs::read_to_string(&dev.log).unwrap();
    assert!(log.contains("destructive changes are allowed"), "{log}");

    // Removing the file removes the collection (the emulator mirrors the project).
    std::fs::remove_file(&notes).unwrap();
    dev.wait_for(Duration::from_secs(60), |v| v["event"] == "reapplied");
    assert_eq!(dev.api("GET", "/api/v1/collections/notes", None).0, 404);
    assert_eq!(dev.interrupt(), Some(0));
}

#[test]
fn dev_persist_keeps_data_and_credentials_and_reset_wipes() {
    let app = App::new("basic");
    let keep = app.root().join("keep");
    let keep_s = keep.display().to_string();
    let mut dev = app.dev(&["--persist", &keep_s, "--seed"]);
    assert_eq!(dev.ready["persist"], true);
    let key = dev.key();
    let password = dev.ready["admin"]["password"].as_str().unwrap().to_owned();
    let (s, _) = dev.api(
        "POST",
        "/api/v1/collections/todos/records",
        Some(json!({"id": "mine", "title": "Mine"})),
    );
    assert_eq!(s, 201);
    assert_eq!(dev.interrupt(), Some(0));
    assert!(
        keep.join("dev-credentials.json").is_file(),
        "the credentials stay with the data"
    );

    // Restart: same administrator and key, the data is still there, seeds are not duplicated.
    let mut dev = app.dev(&["--persist", &keep_s, "--seed"]);
    assert_eq!(dev.key(), key);
    assert_eq!(dev.ready["admin"]["password"], password);
    assert_eq!(
        dev.ready["seed"]["collections"]["todos"]["unchanged"], 3,
        "{}",
        dev.ready
    );
    assert_eq!(dev.ready["applied"], 0, "the project is already applied");
    let (_, page) = dev.api("GET", "/api/v1/collections/todos/records", None);
    assert_eq!(page["items"].as_array().unwrap().len(), 4);

    // `dev reset` on the running emulator: everything but the fixtures goes.
    let out = app.ok(&["dev", "reset"]);
    assert!(out.contains("reset: deleted 4 record(s)"), "{out}");
    assert!(out.contains("3 created"), "{out}");
    let (_, page) = dev.api("GET", "/api/v1/collections/todos/records", None);
    assert_eq!(
        titles(&page),
        [
            "Deploy from CI",
            "Model the app's collections",
            "Read the ForgeBase docs"
        ]
    );
    assert_eq!(dev.api("GET", "/api/v1/collections/todos/records/mine", None).0, 404);
    assert_eq!(dev.interrupt(), Some(0));

    // With no emulator running, `reset --persist` wipes the directory (and refuses anything else).
    let out = app.ok(&["dev", "reset", "--persist", &keep_s]);
    assert!(out.contains("wiped"), "{out}");
    assert_eq!(std::fs::read_dir(&keep).unwrap().count(), 0);
    let other = app.root().join("other");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("precious"), "x").unwrap();
    let o = app.cli(&["dev", "reset", "--persist", &other.display().to_string()]);
    assert_eq!(o.status.code(), Some(1));
    assert!(other.join("precious").exists());
}

// ---------------------------------------------------------------------------
// forgebase gen types

/// A project that uses every field type, a keyword, flexible and view collections, and an auth collection.
fn kitchen_sink(app: &App) {
    let c = app.project().join("collections");
    let put = |name: &str, v: Value| std::fs::write(c.join(format!("{name}.json")), v.to_string()).unwrap();
    put(
        "profiles",
        json!({"type": "auth", "fields": [{"name": "display_name", "type": "text", "required": true}]}),
    );
    put(
        "everything",
        json!({"type": "base", "fields": [
            {"name": "title", "type": "text", "required": true, "max_length": 100},
            {"name": "type", "type": "text"},
            {"name": "createdBy", "type": "relation", "collection": "profiles"},
            {"name": "peers", "type": "relation", "collection": "everything", "max_select": 5},
            {"name": "contact", "type": "email"},
            {"name": "site", "type": "url"},
            {"name": "when", "type": "date"},
            {"name": "count", "type": "number", "integer": true, "required": true, "default": 0},
            {"name": "ratio", "type": "number"},
            {"name": "flag", "type": "bool"},
            {"name": "status", "type": "select", "values": ["draft", "in-progress", "done"], "required": true, "default": "draft"},
            {"name": "labels", "type": "select", "values": ["a", "b"], "max_select": 2},
            {"name": "payload", "type": "json"},
            {"name": "names", "type": "array", "items": "text"},
            {"name": "scores", "type": "array", "items": "number"},
            {"name": "avatar", "type": "file"},
            {"name": "gallery", "type": "file", "max_select": 4},
            {"name": "secret", "type": "password"},
            {"name": "where", "type": "geo"},
            {"name": "embedding", "type": "vector", "dimensions": 3},
            {"name": "internal", "type": "text", "hidden": true}
        ]}),
    );
    put(
        "loose",
        json!({"type": "base", "schema_mode": "flexible", "fields": [{"name": "kind", "type": "text"}]}),
    );
}

#[test]
fn gen_types_is_deterministic_and_check_detects_staleness() {
    let app = App::new("realtime-chat");
    let out = app.root().join("generated");
    let ts = out.join("fb.ts").display().to_string();
    let rs = out.join("fb.rs").display().to_string();
    let first = app.ok(&["gen", "types", "--lang", "ts", "--out", &ts]);
    assert!(first.contains("wrote") && first.contains("3 collections"), "{first}");
    let bytes = std::fs::read(&ts).unwrap();
    // Same output again, from stdout and from a second run; nothing rewritten.
    let stdout = app.ok(&["gen", "types", "--lang", "ts"]);
    assert_eq!(stdout.as_bytes(), bytes.as_slice(), "stdout and the file agree");
    let again = app.ok(&["gen", "types", "--lang", "ts", "--out", &ts]);
    assert!(again.contains("up to date"), "{again}");
    assert_eq!(std::fs::read(&ts).unwrap(), bytes);
    app.ok(&["gen", "types", "--lang", "rust", "--out", &rs]);

    // --check: fresh passes; an edited file, a missing file and a changed project fail with exit code 2.
    let args = ["gen", "types", "--lang", "ts", "--out", &ts, "--check"];
    assert!(app.ok(&args).contains("up to date"));
    append(Path::new(&ts), "// edited\n");
    let o = app.cli(&args);
    assert_eq!(o.status.code(), Some(2), "{}", text(&o.stderr));
    assert!(text(&o.stderr).contains("differs"), "{}", text(&o.stderr));
    std::fs::write(&ts, &bytes).unwrap();
    assert_eq!(app.cli(&args).status.code(), Some(0));
    let rooms = app.project().join("collections/rooms.json");
    let mut v: Value = serde_json::from_str(&std::fs::read_to_string(&rooms).unwrap()).unwrap();
    v["fields"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name": "archived", "type": "bool"}));
    std::fs::write(&rooms, v.to_string()).unwrap();
    assert_eq!(
        app.cli(&args).status.code(),
        Some(2),
        "a project change makes the file stale"
    );
    let o = app.cli(&[
        "gen",
        "types",
        "--lang",
        "ts",
        "--out",
        &out.join("missing.ts").display().to_string(),
        "--check",
    ]);
    assert_eq!(o.status.code(), Some(2));
    assert!(text(&o.stderr).contains("is missing"), "{}", text(&o.stderr));
    // --check without a file is a usage error.
    assert_eq!(
        app.cli(&["gen", "types", "--lang", "ts", "--check"]).status.code(),
        Some(2)
    );
}

#[test]
fn gen_types_from_a_server_matches_the_project() {
    let app = App::new("basic");
    let from_project = app.ok(&["gen", "types", "--lang", "ts"]);
    let mut dev = app.dev(&[]);
    let server = app.ok(&[
        "gen",
        "types",
        "--lang",
        "ts",
        "--url",
        &dev.url(),
        "--token",
        &dev.key(),
    ]);
    assert_eq!(
        server, from_project,
        "a live server and its project produce the same types"
    );
    assert_eq!(dev.interrupt(), Some(0));
}

fn find_tsc() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("FORGEBASE_TSC")
        .map(PathBuf::from)
        .filter(|p| p.is_file())
    {
        return Some(p);
    }
    let local = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sdk/js/node_modules/.bin/tsc");
    if local.is_file() {
        return Some(local);
    }
    let out = Command::new("sh").args(["-c", "command -v tsc"]).output().ok()?;
    let p = text(&out.stdout).trim().to_owned();
    (out.status.success() && !p.is_empty()).then(|| PathBuf::from(p))
}

#[test]
fn generated_typescript_compiles_against_the_sdk_types() {
    let Some(tsc) = find_tsc() else {
        eprintln!("skipped: no TypeScript compiler (set FORGEBASE_TSC or run `npm ci` in sdk/js)");
        return;
    };
    let app = App::new("basic");
    kitchen_sink(&app);
    let dir = app.root().join("ts");
    std::fs::create_dir_all(&dir).unwrap();
    let types = dir.join("fb.ts").display().to_string();
    app.ok(&["gen", "types", "--lang", "ts", "--out", &types]);
    let sdk = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../sdk/js/src/index.ts")
        .canonicalize()
        .unwrap();
    std::fs::write(
        dir.join("use.ts"),
        r#"import type { ForgeBase } from "@forgebase/sdk";
import type { Collections, CollectionInputs, Everything, EverythingCreate, EverythingStatus, Loose, Profiles, ProfilesCreate, Todos, TodosCreate } from "./fb";
import { collectionNames } from "./fb";

declare const fb: ForgeBase;
const everything = fb.collection<Everything>("everything");
const todos = fb.collection<Todos>("todos");
const loose = fb.collection<Loose>("loose");
const profiles = fb.collection<Profiles>("profiles");

export async function use() {
  const created = await todos.create({ title: "x" } satisfies TodosCreate);
  const title: string = created.title;
  const done: boolean | null = created.done;
  const version: number = created._version;
  const page = await everything.list({ filter: "count > 1" });
  const first: Everything | undefined = page.items[0];
  const status: EverythingStatus | undefined = first?.status;
  const labels: ("a" | "b")[] | null | undefined = first?.labels;
  const where: { lat: number; lon: number } | null | undefined = first?.where;
  const embedding: number[] | null | undefined = first?.embedding;
  const peers: string[] | null | undefined = first?.peers;
  const internal: string | null | undefined = first?.internal;
  const kind: unknown = (await loose.get("x")).whatever;
  await profiles.create({ email: "a@b.c", password: "secret123", display_name: "A" } satisfies ProfilesCreate);
  const input = { title: "t", status: "in-progress" } satisfies EverythingCreate;
  const byName: Collections["everything"] = {} as Everything;
  const inputs: CollectionInputs["profiles"] = {} as ProfilesCreate;
  const names: readonly string[] = collectionNames;
  // @ts-expect-error status is a closed set of values
  const bad: EverythingCreate = { title: "t", status: "nope" };
  return [title, done, version, status, labels, where, embedding, peers, internal, kind, input, byName, inputs, names, bad];
}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("tsconfig.json"),
        json!({"compilerOptions": {
            "target": "ES2022", "module": "ESNext", "moduleResolution": "Bundler",
            "lib": ["ES2022", "DOM", "DOM.Iterable"], "types": [], "strict": true, "noEmit": true,
            "skipLibCheck": true, "verbatimModuleSyntax": true, "isolatedModules": true,
            "noUncheckedIndexedAccess": true, "baseUrl": ".",
            "paths": {"@forgebase/sdk": [sdk.display().to_string()]}
        }, "files": ["use.ts"]})
        .to_string(),
    )
    .unwrap();
    let o = Command::new(&tsc)
        .args(["-p", "tsconfig.json"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "tsc failed:\n{}{}\n--- fb.ts ---\n{}",
        text(&o.stdout),
        text(&o.stderr),
        std::fs::read_to_string(dir.join("fb.ts")).unwrap()
    );
}

#[test]
fn generated_rust_compiles_and_round_trips_records() {
    let app = App::new("basic");
    kitchen_sink(&app);
    let krate = app.root().join("typed");
    std::fs::create_dir_all(krate.join("src")).unwrap();
    app.ok(&[
        "gen",
        "types",
        "--lang",
        "rust",
        "--out",
        &krate.join("src/types.rs").display().to_string(),
    ]);
    std::fs::write(
        krate.join("Cargo.toml"),
        "[package]\nname = \"typed\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n[dependencies]\nserde = { version = \"1\", features = [\"derive\"] }\nserde_json = \"1\"\n\n[workspace]\n",
    )
    .unwrap();
    std::fs::write(
        krate.join("src/lib.rs"),
        r#"#![deny(warnings)]
pub mod types;

#[cfg(test)]
mod tests {
    use super::types::*;
    use serde_json::json;

    #[test]
    fn records_read_and_inputs_write() {
        let rec: Everything = serde_json::from_value(json!({
            "id": "a1", "created": "2026-01-01T00:00:00Z", "updated": "2026-01-01T00:00:00Z", "_version": 3,
            "title": "T", "type": null, "createdBy": "u1", "peers": ["x", "y"], "contact": null, "site": null,
            "when": "2026-01-01T00:00:00Z", "count": 7, "ratio": 1.5, "flag": true, "status": "in-progress",
            "labels": ["a", "b"], "payload": {"k": [1, 2]}, "names": ["n"], "scores": [1.5], "avatar": null,
            "gallery": ["g.png"], "where": {"lat": 1.0, "lon": 2.0}, "embedding": [0.5, 0.25, 0.0]
        })).unwrap();
        assert_eq!(rec.version, 3);
        assert_eq!(rec.created_by.as_deref(), Some("u1"));
        assert_eq!(rec.status, EverythingStatus::InProgress);
        assert_eq!(rec.r#type, None);
        assert_eq!(rec.count, 7);
        assert_eq!(rec.internal, None, "hidden and absent");
        assert_eq!(rec.labels, Some(vec![EverythingLabels::A, EverythingLabels::B]));
        assert_eq!(Everything::COLLECTION, "everything");
        let back = serde_json::to_value(&rec).unwrap();
        assert_eq!(back["createdBy"], "u1");
        assert_eq!(back["_version"], 3);

        let create = EverythingCreate {
            id: None,
            title: "new".into(),
            r#type: None,
            created_by: Some("u1".into()),
            peers: None, contact: None, site: None, when: None,
            count: None, ratio: None, flag: Some(true), status: Some(EverythingStatus::Draft),
            labels: None, payload: None, names: None, scores: None, avatar: None, gallery: None,
            secret: Some("pw".into()), r#where: Some(GeoPoint { lat: 1.0, lon: 2.0 }), embedding: None, internal: None,
        };
        let v = serde_json::to_value(&create).unwrap();
        assert_eq!(v, json!({"title": "new", "createdBy": "u1", "flag": true, "status": "draft", "secret": "pw", "where": {"lat": 1.0, "lon": 2.0}}));
        let made = EverythingCreate::new("short".into());
        assert_eq!(serde_json::to_value(&made).unwrap(), json!({"title": "short"}));
        let made = LooseCreate::new();
        assert_eq!(serde_json::to_value(&made).unwrap(), json!({}));
        let patch = EverythingUpdate { flag: Some(false), ..Default::default() };
        assert_eq!(serde_json::to_value(&patch).unwrap(), json!({"flag": false}));

        // Flexible collections keep the fields they do not declare.
        let loose: Loose = serde_json::from_value(json!({
            "id": "l", "created": "c", "updated": "u", "_version": 1, "kind": "k", "extra_field": 5
        })).unwrap();
        assert_eq!(loose.extra["extra_field"], 5);
        let user: Profiles = serde_json::from_value(json!({
            "id": "p", "created": "c", "updated": "u", "_version": 1,
            "email": "a@b.c", "email_visibility": false, "verified": true, "display_name": "A"
        })).unwrap();
        assert!(user.verified);
        assert!(COLLECTION_NAMES.contains(&"profiles"));
    }
}
"#,
    )
    .unwrap();
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut cmd = Command::new(cargo);
    cmd.args(["test", "--offline", "--quiet"]).current_dir(&krate);
    if std::env::var_os("CARGO_TARGET_DIR").is_none() {
        cmd.env("CARGO_TARGET_DIR", app.root().join("target"));
    }
    let o = cmd.output().unwrap();
    assert!(
        o.status.success(),
        "cargo test of the generated types failed:\n{}{}\n--- types.rs ---\n{}",
        text(&o.stdout),
        text(&o.stderr),
        std::fs::read_to_string(krate.join("src/types.rs")).unwrap()
    );
}

// ---------------------------------------------------------------------------
// forgebase functions invoke / logs

fn trace_id_of(out: &str) -> String {
    let line = out
        .lines()
        .find(|l| l.starts_with("trace id:"))
        .unwrap_or_else(|| panic!("no trace id line:\n{out}"));
    line.trim_start_matches("trace id:").trim().to_owned()
}

#[test]
fn functions_invoke_shows_status_logs_and_a_trace_id() {
    let app = App::new("basic");
    let mut dev = app.dev(&[]);
    let (url, key) = (dev.url(), dev.key());
    let out = app.ok(&[
        "functions",
        "invoke",
        "hello",
        "--server",
        &url,
        "--token",
        &key,
        "--path",
        "?name=cave",
        "--trace",
    ]);
    assert!(out.contains("status:    200"), "{out}");
    assert!(out.contains("\"message\": \"Hello, cave!\""), "{out}");
    assert!(out.contains("[info] greeting cave"), "the function's log lines\n{out}");
    let trace = trace_id_of(&out);
    assert!(
        trace.len() == 32 && trace.bytes().all(|b| b.is_ascii_hexdigit()),
        "a trace id: {trace}"
    );
    assert!(out.contains("execution: #1 v1 http ok"), "{out}");
    // --trace degrades to the logs and the trace id when the server has no insights API.
    assert!(out.contains("trace:"), "{out}");

    // The same call as JSON, with a body from a file.
    let body = app.root().join("body.json");
    std::fs::write(&body, "{\"a\": 1}").unwrap();
    let json_out = app.ok(&[
        "functions",
        "invoke",
        "hello",
        "--server",
        &url,
        "--token",
        &key,
        "--data",
        &format!("@{}", body.display()),
        "--json",
    ]);
    let v: Value = serde_json::from_str(json_out.trim()).unwrap();
    assert_eq!(v["status"], 200);
    assert_eq!(v["ok"], true);
    assert_eq!(v["execution"]["logs"][0]["message"], "greeting world");
    assert_eq!(v["execution"]["trace_id"], v["trace_id"]);
    assert_eq!(v["body"]["message"], "Hello, world!");

    // A function that does not exist fails with a non-zero exit code and the server's error.
    let o = app.cli(&["functions", "invoke", "nosuch", "--server", &url, "--token", &key]);
    assert_eq!(o.status.code(), Some(1));
    let t = text(&o.stdout);
    assert!(t.contains("status:    404") && t.contains("function_not_found"), "{t}");
    assert!(t.contains("the function did not run"), "{t}");
    assert_eq!(dev.interrupt(), Some(0));
}

#[test]
fn functions_logs_follow_receives_a_line_written_after_it_started() {
    let app = App::new("basic");
    let mut dev = app.dev(&[]);
    let (url, key) = (dev.url(), dev.key());
    // One run before following, so the first poll has something to skip past.
    app.ok(&[
        "functions",
        "invoke",
        "hello",
        "--server",
        &url,
        "--token",
        &key,
        "--path",
        "?name=before",
    ]);
    let mut follow = bare(&app.dir(), &app.tmp())
        .args([
            "functions",
            "logs",
            "hello",
            "--follow",
            "--interval",
            "0.1",
            "--server",
            &url,
            "--token",
            &key,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let out = follow.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for l in BufReader::new(out).lines().map_while(Result::ok) {
            if tx.send(l).is_err() {
                break;
            }
        }
    });
    // The initial page includes the earlier run: the follower is up once it printed that.
    let mut seen = Vec::new();
    let end = Instant::now() + Duration::from_secs(30);
    while !seen.iter().any(|l: &String| l.contains("greeting before")) {
        assert!(
            Instant::now() < end,
            "the follower never printed the first page: {seen:?}"
        );
        if let Ok(l) = rx.recv_timeout(Duration::from_millis(200)) {
            seen.push(l);
        }
    }
    assert!(!seen.iter().any(|l| l.contains("greeting after")));
    // A run that happens after the follower started shows up as it arrives.
    app.ok(&[
        "functions",
        "invoke",
        "hello",
        "--server",
        &url,
        "--token",
        &key,
        "--path",
        "?name=after",
    ]);
    let end = Instant::now() + Duration::from_secs(30);
    let mut got = false;
    while Instant::now() < end && !got {
        if let Ok(l) = rx.recv_timeout(Duration::from_millis(200)) {
            got = l.contains("[info] greeting after");
            seen.push(l);
        }
    }
    let _ = follow.kill();
    let _ = follow.wait();
    assert!(got, "the new log line was streamed: {seen:?}");
    // Each execution line carries the trace id.
    assert!(seen.iter().any(|l| l.contains(" trace ")), "{seen:?}");
    assert_eq!(dev.interrupt(), Some(0));
}
