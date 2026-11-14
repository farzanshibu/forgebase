//! Black-box tests of project as code (ADR-0037): `forgebase new`,
//! `forgebase project pull|plan|apply|drift|validate` and `forgebase deploy`
//! against a real `forgebase serve` (SQLite; PostgreSQL with
//! `FORGEBASE_TEST_BACKEND=postgres`, see docs/qa.md).
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../../../tests/integration/common/mod.rs"]
mod common;

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;

use common::{free_port, startup_timeout, wait_healthy};
use serde_json::{Value, json};

const ADMIN_EMAIL: &str = "root@example.com";
const ADMIN_PASSWORD: &str = "root password 1";
const SLACK: &str = "https://hooks.slack.com/services/T000/B000/very-secret-token";

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// A server with an administrator, and a scratch directory for app projects.
struct Fb {
    tmp: tempfile::TempDir,
    child: Child,
    addr: String,
    token: String,
}

impl Fb {
    fn start() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("home")).unwrap();
        let out = bare(&root)
            .args(common::backend_args(&root, &["init", "srv"]))
            .output()
            .unwrap();
        assert!(out.status.success(), "init: {}", text(&out.stderr));
        let srv = root.join("srv");
        let mut create = bare(&srv)
            .args(["admin", "create", "--email", ADMIN_EMAIL, "--password-stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        create
            .stdin
            .take()
            .unwrap()
            .write_all(format!("{ADMIN_PASSWORD}\n").as_bytes())
            .unwrap();
        let out = create.wait_with_output().unwrap();
        assert!(out.status.success(), "admin create: {}", text(&out.stderr));
        let port = free_port();
        let child = bare(&srv)
            .args(["serve", "--port", &port.to_string()])
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(root.join("serve.log")).unwrap())
            .spawn()
            .unwrap();
        let addr = format!("127.0.0.1:{port}");
        assert!(wait_healthy(&addr, startup_timeout()), "server did not become healthy");
        let (status, body) = call(
            &addr,
            "POST",
            "/api/auth/admins/login",
            None,
            Some(json!({"email": ADMIN_EMAIL, "password": ADMIN_PASSWORD})),
        );
        assert_eq!(status, 200, "{body}");
        let token = body["access_token"].as_str().unwrap().to_owned();
        Self {
            tmp,
            child,
            addr,
            token,
        }
    }

    fn root(&self) -> PathBuf {
        self.tmp.path().canonicalize().unwrap()
    }

    /// `forgebase <args>` in the scratch directory, connected to the server.
    fn cli(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        self.cli_stdin(args, env, None)
    }

    fn cli_stdin(&self, args: &[&str], env: &[(&str, &str)], stdin: Option<&str>) -> Output {
        let mut cmd = bare(&self.root());
        cmd.env("FORGEBASE_URL", format!("http://{}", self.addr))
            .env("FORGEBASE_TOKEN", &self.token);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd
            .args(args)
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

    fn ok(&self, args: &[&str], env: &[(&str, &str)]) -> String {
        let o = self.cli(args, env);
        assert_eq!(
            o.status.code(),
            Some(0),
            "forgebase {args:?}\nstdout: {}\nstderr: {}",
            text(&o.stdout),
            text(&o.stderr)
        );
        text(&o.stdout)
    }

    /// Runs `args` and asserts the exit code; returns stdout and stderr.
    fn code(&self, want: i32, args: &[&str], env: &[(&str, &str)]) -> (String, String) {
        let o = self.cli(args, env);
        assert_eq!(
            o.status.code(),
            Some(want),
            "forgebase {args:?}\nstdout: {}\nstderr: {}",
            text(&o.stdout),
            text(&o.stderr)
        );
        (text(&o.stdout), text(&o.stderr))
    }

    fn api(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        call(&self.addr, method, path, Some(&self.token), body)
    }

    fn get(&self, path: &str) -> Value {
        let (s, v) = self.api("GET", path, None);
        assert_eq!(s, 200, "GET {path}: {v}");
        v
    }
}

impl Drop for Fb {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A `forgebase` command with no inherited ForgeBase variables and an empty HOME.
fn bare(cwd: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_forgebase"));
    cmd.current_dir(cwd);
    for (k, _) in std::env::vars_os() {
        let k = k.to_string_lossy().into_owned();
        if (k.starts_with("FORGEBASE_") && k != "FORGEBASE_PG_ENGINE_DIR") || k.starts_with("FB_") {
            cmd.env_remove(k);
        }
    }
    let home = cwd
        .ancestors()
        .find(|a| a.join("home").is_dir())
        .map(|a| a.join("home"));
    if let Some(h) = home {
        cmd.env("HOME", h);
    }
    cmd.env("NO_COLOR", "1");
    common::backend_env(&mut cmd);
    cmd
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

fn edit(path: &Path, f: impl FnOnce(&mut Value)) {
    let mut v: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    f(&mut v);
    std::fs::write(path, serde_json::to_string_pretty(&v).unwrap()).unwrap();
}

/// Every file under `dir` (relative path -> bytes), skipping the project file.
fn tree(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_owned()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else if p.file_name().is_some_and(|n| n != "forgebase.project.toml") {
                out.insert(
                    p.strip_prefix(dir).unwrap().display().to_string(),
                    std::fs::read(&p).unwrap(),
                );
            }
        }
    }
    out
}

fn names(v: &Value) -> Vec<String> {
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["name"].as_str().unwrap().to_owned())
        .collect()
}

/// new → apply on an empty server; pull → plan is empty; edits converge;
/// drift; optimistic concurrency; secrets never reach the disk.
#[test]
fn new_apply_pull_round_trip_drift_and_concurrency() {
    let fb = Fb::start();
    let root = fb.root();
    let env = [("SLACK_WEBHOOK_URL", SLACK)];
    fb.ok(&["new", "shop", "--template", "functions"], &[]);
    let dir = "shop/forgebase";
    assert!(root.join("shop/.github/workflows/forgebase.yml").is_file());
    assert!(root.join("shop/.env.example").is_file());
    assert!(root.join("shop/forgebase/seed/orders.jsonl").is_file());
    assert!(root.join("shop/forgebase/.schemas/collection.schema.json").is_file());
    fb.ok(&["project", "validate", "--dir", dir], &[]);

    // Empty server: everything is a create; exit code 2.
    let (out, _) = fb.code(2, &["project", "plan", "--dir", dir, "--json"], &[]);
    let plan: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(plan["summary"]["create"], 5, "{out}");
    assert_eq!(plan["summary"]["update"], 0, "{out}");

    // Without --auto-approve and no terminal: refused, nothing changed.
    let (_, err) = fb.code(1, &["project", "apply", "--dir", dir], &env);
    assert!(err.contains("--auto-approve"), "{err}");
    assert_eq!(fb.api("GET", "/api/v1/collections/orders", None).0, 404);

    let out = fb.ok(&["project", "apply", "--dir", dir, "--auto-approve"], &env);
    assert!(out.contains("Apply complete: 5 changed"), "{out}");
    let orders = fb.get("/api/v1/collections/orders");
    assert_eq!(orders["fields"].as_array().unwrap().len(), 4, "{orders}");
    let fns = names(&fb.get("/api/v1/functions"));
    assert!(
        fns.contains(&"hello".to_owned()) && fns.contains(&"order-created".to_owned()),
        "{fns:?}"
    );
    let secrets = names(&fb.get("/api/v1/functions/order-created/secrets"));
    assert_eq!(secrets, vec!["SLACK_WEBHOOK_URL"]);
    assert!(fb.get("/api/v1/queues/topics/order-events")["source"]["collection"].is_string());
    assert_eq!(
        fb.get("/api/v1/queues/subscriptions/order-events-audit")["ack_deadline_ms"],
        30000
    );
    let idx = fb.get("/api/v1/indexes/orders_status");
    assert_eq!(idx["origin"], "manual", "{idx}");
    let rules = fb.get("/api/v1/policies/orders");
    assert_eq!(rules["list_rule"], "@request.auth.id != \"\"", "{rules}");

    // Converged: exit 0, and a second apply has nothing to do.
    let (out, _) = fb.code(0, &["project", "plan", "--dir", dir], &[]);
    assert!(out.contains("No changes"), "{out}");
    let out = fb.ok(&["project", "apply", "--dir", dir, "--auto-approve"], &env);
    assert!(out.contains("No changes"), "{out}");
    // Secret values cannot be compared: --update-secrets sends them again.
    let out = fb.ok(
        &["project", "apply", "--dir", dir, "--auto-approve", "--update-secrets"],
        &[("SLACK_WEBHOOK_URL", "https://hooks.slack.com/services/rotated")],
    );
    assert!(out.contains("sent 1 secret(s) of function order-created"), "{out}");

    // pull → plan: no changes; pulling twice gives the same bytes; no secret on disk.
    fb.ok(&["project", "pull", "--dir", "pulled"], &[]);
    fb.code(0, &["project", "plan", "--dir", "pulled"], &[]);
    fb.ok(&["project", "pull", "--dir", "pulled2"], &[]);
    assert_eq!(tree(&root.join("pulled")), tree(&root.join("pulled2")));
    let mut all = Vec::new();
    for (_, bytes) in tree(&root.join("pulled")) {
        all.extend(bytes);
    }
    assert!(
        !text(&all).contains("very-secret-token"),
        "a secret reached the pulled files"
    );
    let ftoml = std::fs::read_to_string(root.join("pulled/functions/order-created/function.toml")).unwrap();
    assert!(
        ftoml.contains("SLACK_WEBHOOK_URL = \"ORDER_CREATED_SLACK_WEBHOOK_URL\""),
        "{ftoml}"
    );
    // An existing project is not overwritten without --force.
    let (_, err) = fb.code(1, &["project", "pull", "--dir", "pulled"], &[]);
    assert!(err.contains("--force"), "{err}");

    // Edit a field: the plan shows the update, apply converges.
    let orders_file = root.join(dir).join("collections/orders.json");
    edit(&orders_file, |v| {
        v["fields"][2]["values"] = json!(["pending", "paid", "shipped", "cancelled"]);
        v["fields"][1]["max"] = json!(100000);
    });
    let (out, _) = fb.code(2, &["project", "plan", "--dir", dir], &[]);
    assert!(out.contains("~ collection orders  [update]"), "{out}");
    assert!(out.contains("fields.status.values"), "{out}");
    assert!(out.contains("schema version v1 -> v2"), "{out}");
    fb.ok(&["project", "apply", "--dir", dir, "--auto-approve"], &env);
    fb.code(0, &["project", "plan", "--dir", dir], &[]);
    let status = &fb.get("/api/v1/collections/orders")["fields"][2];
    assert_eq!(status["values"].as_array().unwrap().len(), 4, "{status}");

    // Out-of-band change: drift exits 2 and names it; the project wins on the next apply.
    let (s, v) = fb.api(
        "PATCH",
        "/api/v1/queues/topics/order-events",
        Some(json!({"labels": {"app": "changed-in-dashboard"}})),
    );
    assert_eq!(s, 200, "{v}");
    let (out, _) = fb.code(2, &["project", "drift", "--dir", dir], &[]);
    assert!(
        out.contains("topic order-events: differs on the server") && out.contains("labels.app"),
        "{out}"
    );
    let (out, _) = fb.code(2, &["project", "drift", "--dir", dir, "--json"], &[]);
    assert_eq!(serde_json::from_str::<Value>(&out).unwrap()["drift"], true);
    // A resource created outside the project is drift too.
    let (s, _) = fb.api("POST", "/api/v1/storage/buckets", Some(json!({"name": "clickops"})));
    assert_eq!(s, 201);
    let (out, _) = fb.code(2, &["project", "drift", "--dir", dir], &[]);
    assert!(out.contains("bucket clickops: only on the server"), "{out}");
    fb.ok(&["project", "apply", "--dir", dir, "--auto-approve"], &env);
    assert_eq!(fb.get("/api/v1/queues/topics/order-events")["labels"]["app"], "orders");
    // Without --prune, the extra bucket stays (and plan still exits 0).
    let (out, _) = fb.code(0, &["project", "plan", "--dir", dir], &[]);
    assert!(
        out.contains("Not in the project") && out.contains("bucket clickops"),
        "{out}"
    );

    // Optimistic concurrency: a saved plan fails if the server changed since.
    let sub_file = root.join(dir).join("queues/subscriptions/order-events-audit.json");
    edit(&sub_file, |v| v["ack_deadline_ms"] = json!(45000));
    let plan_file = root.join("plan.json");
    let plan_arg = plan_file.display().to_string();
    fb.code(2, &["project", "plan", "--dir", dir, "--out", &plan_arg], &[]);
    let (s, v) = fb.api(
        "PATCH",
        "/api/v1/queues/subscriptions/order-events-audit",
        Some(json!({"labels": {"app": "someone-else"}})),
    );
    assert_eq!(s, 200, "{v}");
    let (_, err) = fb.code(1, &["project", "apply", "--dir", dir, "--plan", &plan_arg], &env);
    assert!(err.contains("changed_since_plan"), "{err}");
    assert_eq!(
        fb.get("/api/v1/queues/subscriptions/order-events-audit")["ack_deadline_ms"],
        30000
    );
    // A fresh plan applies, and applying the same plan again is a no-op (resumable).
    fb.code(2, &["project", "plan", "--dir", dir, "--out", &plan_arg], &[]);
    let out = fb.ok(&["project", "apply", "--dir", dir, "--plan", &plan_arg], &env);
    assert!(out.contains("Apply complete"), "{out}");
    let sub = fb.get("/api/v1/queues/subscriptions/order-events-audit");
    assert_eq!(
        (sub["ack_deadline_ms"].clone(), sub["labels"]["app"].clone()),
        (json!(45000), json!("orders"))
    );
    let out = fb.ok(&["project", "apply", "--dir", dir, "--plan", &plan_arg], &env);
    assert!(out.contains("No changes"), "{out}");
    // A plan made before a project edit is refused.
    fb.code(0, &["project", "plan", "--dir", dir, "--out", &plan_arg], &[]);
    edit(&sub_file, |v| v["ack_deadline_ms"] = json!(50000));
    let (_, err) = fb.code(1, &["project", "apply", "--dir", dir, "--plan", &plan_arg], &env);
    assert!(err.contains("not in the saved plan"), "{err}");

    // A topic's record source cannot change: the topic is replaced, and its subscription with it.
    let topic_file = root.join(dir).join("queues/topics/order-events.json");
    edit(&topic_file, |v| v["source"]["ops"] = json!(["create"]));
    let (out, _) = fb.code(2, &["project", "plan", "--dir", dir], &[]);
    assert!(out.contains("-/+ topic order-events  [replace, destructive]"), "{out}");
    assert!(
        out.contains("-/+ subscription order-events-audit  [replace, destructive]"),
        "{out}"
    );
    fb.ok(
        &[
            "project",
            "apply",
            "--dir",
            dir,
            "--auto-approve",
            "--allow-destructive",
        ],
        &env,
    );
    assert_eq!(
        fb.get("/api/v1/queues/topics/order-events")["source"]["ops"],
        json!(["create"])
    );
    assert_eq!(
        fb.get("/api/v1/queues/subscriptions/order-events-audit")["ack_deadline_ms"],
        50000
    );
    fb.code(0, &["project", "plan", "--dir", dir], &[]);
}

/// Field type changes and collection removal are destructive: deletions only
/// with --prune, and both only with --allow-destructive.
#[test]
fn destructive_changes_need_prune_and_allow_destructive() {
    let fb = Fb::start();
    let root = fb.root();
    fb.ok(&["new", "todo", "--template", "basic"], &[]);
    let dir = "todo/forgebase";
    fb.ok(&["project", "apply", "--dir", dir, "--auto-approve"], &[]);
    let (s, _) = fb.api(
        "POST",
        "/api/v1/collections/todos/records",
        Some(json!({"title": "keep me", "notes": "n"})),
    );
    assert_eq!(s, 201);

    // A field type change: destructive, with a data migration.
    let file = root.join(dir).join("collections/todos.json");
    edit(&file, |v| v["fields"][3] = json!({"name": "notes", "type": "json"}));
    let (out, _) = fb.code(2, &["project", "plan", "--dir", dir], &[]);
    assert!(
        out.contains("[update, destructive]") && out.contains("changes the type of field \"notes\""),
        "{out}"
    );
    assert!(out.contains("data migration"), "{out}");
    let (_, err) = fb.code(1, &["project", "apply", "--dir", dir, "--auto-approve"], &[]);
    assert!(err.contains("destructive_change"), "{err}");
    assert_eq!(fb.get("/api/v1/collections/todos")["fields"][3]["type"], "text");
    // Put it back; drop the field instead (also destructive).
    edit(&file, |v| {
        let fields = v["fields"].as_array_mut().unwrap();
        fields.truncate(3);
    });
    let (out, _) = fb.code(2, &["project", "plan", "--dir", dir, "--json"], &[]);
    let plan: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(plan["summary"]["destructive"], 1, "{out}");
    fb.ok(
        &[
            "project",
            "apply",
            "--dir",
            dir,
            "--auto-approve",
            "--allow-destructive",
        ],
        &[],
    );
    assert_eq!(
        fb.get("/api/v1/collections/todos")["fields"].as_array().unwrap().len(),
        3
    );

    // Remove the collection file: not a change without --prune.
    std::fs::remove_file(&file).unwrap();
    let (out, _) = fb.code(0, &["project", "plan", "--dir", dir], &[]);
    assert!(out.contains("No changes") && out.contains("collection todos"), "{out}");
    let (out, _) = fb.code(2, &["project", "plan", "--dir", dir, "--prune"], &[]);
    assert!(out.contains("- collection todos  [delete, destructive]"), "{out}");
    let (_, err) = fb.code(1, &["project", "apply", "--dir", dir, "--prune", "--auto-approve"], &[]);
    assert!(
        err.contains("destructive_change") && err.contains("collection todos"),
        "{err}"
    );
    assert_eq!(fb.api("GET", "/api/v1/collections/todos", None).0, 200);
    // Bucket deletion is destructive too.
    std::fs::remove_file(root.join(dir).join("storage/buckets/attachments.json")).unwrap();
    let out = fb.ok(
        &[
            "project",
            "apply",
            "--dir",
            dir,
            "--prune",
            "--allow-destructive",
            "--auto-approve",
        ],
        &[],
    );
    assert!(
        out.contains("deleted collection todos") && out.contains("deleted bucket attachments"),
        "{out}"
    );
    assert_eq!(fb.api("GET", "/api/v1/collections/todos", None).0, 404);
    assert_eq!(fb.api("GET", "/api/v1/storage/buckets/attachments", None).0, 404);
    fb.code(0, &["project", "plan", "--dir", dir, "--prune"], &[]);
}

/// One invalid function fails alone; the next run converges. Also: project
/// errors exit 1 with file:line, `deploy` runs builds, administrator sign-in.
#[test]
fn partial_failure_converges_and_deploy_builds() {
    let fb = Fb::start();
    let root = fb.root();
    fb.ok(&["new", "app", "--template", "basic"], &[]);
    let dir = "app/forgebase";
    let fdir = root.join(dir).join("functions/broken");
    std::fs::create_dir_all(&fdir).unwrap();
    std::fs::write(
        fdir.join("function.toml"),
        "access = \"public\"\ncapabilities = [\"log\"]\n",
    )
    .unwrap();
    std::fs::write(
        fdir.join("index.ts"),
        "serve(async (req) => { return new Response(\"x\" ;\n",
    )
    .unwrap();
    let (out, _) = fb.code(1, &["project", "apply", "--dir", dir, "--auto-approve"], &[]);
    assert!(out.contains("FAILED function broken"), "{out}");
    assert!(out.contains("Apply incomplete: 3 changed"), "{out}");
    assert_eq!(fb.api("GET", "/api/v1/collections/todos", None).0, 200);
    assert_eq!(fb.api("GET", "/api/v1/functions/hello", None).0, 200);
    // Fix it: the re-run only deploys that function.
    std::fs::write(
        fdir.join("index.ts"),
        "import { serve } from \"forgebase\";\nserve(async () => new Response(\"fixed\"));\n",
    )
    .unwrap();
    let (out, _) = fb.code(2, &["project", "plan", "--dir", dir], &[]);
    assert!(out.contains("Plan: 1 to create, 0 to update"), "{out}");
    let out = fb.ok(&["project", "apply", "--dir", dir, "--auto-approve"], &[]);
    assert!(out.contains("created function broken"), "{out}");
    fb.code(0, &["project", "plan", "--dir", dir], &[]);

    // A project error: exit 1, every problem with file and line, nothing contacted.
    let coll = root.join(dir).join("collections/todos.json");
    let good = std::fs::read_to_string(&coll).unwrap();
    std::fs::write(&coll, good.replacen("\"max_length\": 200", "\"maxlength\": 200", 1)).unwrap();
    let (_, err) = fb.code(1, &["project", "plan", "--dir", dir], &[]);
    assert!(
        err.contains("collections/todos.json:") && err.contains("unknown key \"maxlength\""),
        "{err}"
    );
    std::fs::write(&coll, good).unwrap();

    // deploy runs a function's build command, then applies.
    let built = root.join(dir).join("functions/built");
    std::fs::create_dir_all(&built).unwrap();
    std::fs::write(
        built.join("function.toml"),
        "access = \"public\"\nentry = \"dist/index.js\"\nbuild = \"mkdir -p dist && cp src.js dist/index.js\"\ncapabilities = [\"log\"]\n",
    )
    .unwrap();
    std::fs::write(
        built.join("src.js"),
        "import { serve } from \"forgebase\";\nserve(async () => new Response(\"built\"));\n",
    )
    .unwrap();
    let out = fb.ok(&["deploy", "--dir", dir, "--auto-approve"], &[]);
    assert!(
        out.contains("building function built") && out.contains("created function built"),
        "{out}"
    );
    let (s, v) = call(&fb.addr, "POST", "/api/v1/functions/built/invoke", None, None);
    assert_eq!(s, 200, "{v}");

    // Administrator sign-in instead of a token.
    let o = {
        let mut cmd = bare(&root);
        cmd.env("FORGEBASE_URL", format!("http://{}", fb.addr));
        let mut child = cmd
            .args([
                "project",
                "plan",
                "--dir",
                dir,
                "--admin-email",
                ADMIN_EMAIL,
                "--password-stdin",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(format!("{ADMIN_PASSWORD}\n").as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    };
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    // No credential at all: the server refuses with a hint.
    let mut cmd = bare(&root);
    let o = cmd
        .env("FORGEBASE_URL", format!("http://{}", fb.addr))
        .args(["project", "plan", "--dir", dir])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o.stderr).contains("FORGEBASE_TOKEN"), "{}", text(&o.stderr));
}

/// Relations, hooks, buckets with ACLs, engine rules and an OAuth client:
/// applied in dependency order, round-tripped by pull, issued secrets go only
/// to --secrets-out.
#[test]
fn chat_template_with_rules_buckets_and_oauth_clients() {
    let fb = Fb::start();
    let root = fb.root();
    fb.ok(&["new", "chat", "--template", "realtime-chat"], &[]);
    let dir = root.join("chat/forgebase");
    std::fs::create_dir_all(dir.join("oauth-clients")).unwrap();
    std::fs::write(
        dir.join("oauth-clients/web.json"),
        r#"{"type": "confidential", "redirect_uris": ["https://chat.example.com/callback"], "secret_env": "WEB_CLIENT_SECRET"}"#,
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("storage/buckets")).unwrap();
    std::fs::write(
        dir.join("storage/buckets/avatars.json"),
        r#"{"visibility": "public", "acl": [{"grantee": "authenticated", "permission": "write", "prefix": "u/"}]}"#,
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("policies")).unwrap();
    std::fs::write(
        dir.join("policies/bucket.json"),
        r#"{"rules": {"avatars": {"view": "", "list": null}}}"#,
    )
    .unwrap();
    let d = dir.display().to_string();
    let secrets = root.join("issued.secrets.env");
    // Secrets may not be exported into the project.
    let inside = dir.join("x.env").display().to_string();
    let (_, err) = fb.code(
        1,
        &[
            "project",
            "apply",
            "--dir",
            &d,
            "--auto-approve",
            "--secrets-out",
            &inside,
        ],
        &[],
    );
    assert!(err.contains("inside the project directory"), "{err}");
    let s = secrets.display().to_string();
    let (out, err) = {
        let o = fb.cli(
            &["project", "apply", "--dir", &d, "--auto-approve", "--secrets-out", &s],
            &[],
        );
        assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
        (text(&o.stdout), text(&o.stderr))
    };
    // rooms before messages (messages.room is a relation to rooms).
    let rooms_at = out.find("created collection rooms").unwrap();
    let messages_at = out.find("created collection messages").unwrap();
    assert!(rooms_at < messages_at, "{out}");
    assert!(!err.contains("not saved"), "{err}");
    let exported = std::fs::read_to_string(&secrets).unwrap();
    let secret = exported
        .lines()
        .find_map(|l| l.strip_prefix("WEB_CLIENT_SECRET="))
        .expect("the client secret is exported under secret_env")
        .to_owned();
    assert!(secret.len() > 10);
    let msg = fb.get("/api/v1/collections/messages");
    let rooms = fb.get("/api/v1/collections/rooms");
    let room_field = msg["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "room")
        .unwrap();
    assert_eq!(room_field["collection_id"], rooms["id"]);
    assert_eq!(
        fb.get("/api/v1/storage/buckets/avatars/acl")["entries"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let rules = fb.get("/api/v1/policies/resources/bucket/avatars");
    assert_eq!(rules["rules"]["view"], "", "{rules}");
    let clients = fb.get("/api/auth/clients?realm=default");
    assert!(
        clients["items"].as_array().unwrap().iter().any(|c| c["name"] == "web"),
        "{clients}"
    );

    // The before-create hook runs: a blocked word is refused.
    let (s, v) = fb.api(
        "POST",
        "/api/v1/collections/rooms/records",
        Some(json!({"title": "general"})),
    );
    assert_eq!(s, 201, "{v}");
    let room = v["id"].as_str().unwrap().to_owned();
    let (s, v) = fb.api(
        "POST",
        "/api/v1/collections/messages/records",
        Some(json!({"room": room, "author": "a", "body": "buy spam now"})),
    );
    assert_eq!(s, 400, "{v}");

    fb.code(0, &["project", "plan", "--dir", &d], &[]);
    // Round trip, and no secret on disk.
    fb.ok(&["project", "pull", "--dir", "chat-pulled"], &[]);
    fb.code(0, &["project", "plan", "--dir", "chat-pulled"], &[]);
    let mut all = Vec::new();
    for (_, bytes) in tree(&root.join("chat-pulled")) {
        all.extend(bytes);
    }
    assert!(
        !text(&all).contains(&secret),
        "the client secret reached the pulled files"
    );
    // pull --force mirrors deletions; a re-plan of the original project is still clean.
    let (s, _) = fb.api("DELETE", "/api/v1/storage/buckets/avatars?force=true", None);
    assert_eq!(s, 204);
    fb.ok(&["project", "pull", "--dir", "chat-pulled", "--force"], &[]);
    assert!(!root.join("chat-pulled/storage/buckets/avatars.json").exists());
    let (out, _) = fb.code(2, &["project", "plan", "--dir", &d], &[]);
    assert!(out.contains("+ bucket avatars"), "{out}");
}

/// A WebAssembly function round-trips through pull; `--env` targets a
/// ForgeBase project environment.
#[test]
fn webassembly_functions_and_environments() {
    let fb = Fb::start();
    let root = fb.root();
    let dir = root.join("app");
    let wasm =
        wat::parse_str(r#"(module (memory (export "memory") 1) (func (export "handle") (result i32) (i32.const 0)))"#)
            .unwrap();
    std::fs::create_dir_all(dir.join("functions/tiny")).unwrap();
    std::fs::write(dir.join("functions/tiny/module.wasm"), &wasm).unwrap();
    std::fs::write(
        dir.join("functions/tiny/function.toml"),
        "access = \"public\"\n[limits]\nfuel = 100000\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("storage/buckets")).unwrap();
    std::fs::write(dir.join("storage/buckets/files.json"), "{}").unwrap();
    std::fs::write(
        dir.join("forgebase.project.toml"),
        "format = 1\nname = \"app\"\nmanage = [\"functions\", \"storage\"]\n\n[environments.staging]\nproject = \"shop\"\n\n[environments.qa]\nproject = \"shop\"\nenvironment = \"nope\"\n",
    )
    .unwrap();
    let d = dir.display().to_string();
    let out = fb.ok(&["project", "apply", "--dir", &d, "--auto-approve"], &[]);
    assert!(out.contains("created function tiny"), "{out}");
    let v = fb.get("/api/v1/functions/tiny/versions");
    assert_eq!(v["items"][0]["language"], "wasm", "{v}");
    fb.code(0, &["project", "plan", "--dir", &d], &[]);
    // pull downloads the module: the pulled project plans clean, byte for byte.
    fb.ok(&["project", "pull", "--dir", "pulled"], &[]);
    assert_eq!(
        std::fs::read(root.join("pulled/functions/tiny/module.wasm")).unwrap(),
        wasm
    );
    fb.code(0, &["project", "plan", "--dir", "pulled"], &[]);
    // A changed module is a new version.
    let wasm2 = wat::parse_str(r#"(module (memory (export "memory") 1) (func (export "handle") (result i32) (i32.const 1) (drop) (i32.const 0)))"#).unwrap();
    std::fs::write(dir.join("functions/tiny/module.wasm"), &wasm2).unwrap();
    let (out, _) = fb.code(2, &["project", "plan", "--dir", &d], &[]);
    assert!(
        out.contains("code.sha256") && out.contains("deploys a new version"),
        "{out}"
    );
    fb.ok(&["project", "apply", "--dir", &d, "--auto-approve"], &[]);
    assert_eq!(fb.get("/api/v1/functions/tiny")["active_version"], 2);

    // --env: the project's environment is resolved; unknown ones are refused.
    let (s, org) = fb.api("POST", "/api/v1/orgs", Some(json!({"slug": "acme", "name": "Acme"})));
    assert_eq!(s, 201, "{org}");
    let org = org["id"].as_str().unwrap().to_owned();
    let (s, p) = fb.api(
        "POST",
        &format!("/api/v1/orgs/{org}/projects"),
        Some(json!({"slug": "shop", "name": "Shop"})),
    );
    assert_eq!(s, 201, "{p}");
    let out = fb.ok(
        &["project", "apply", "--dir", &d, "--env", "staging", "--auto-approve"],
        &[],
    );
    assert!(out.contains("Apply complete") || out.contains("No changes"), "{out}");
    fb.code(0, &["project", "plan", "--dir", &d, "--env", "staging"], &[]);
    let (_, err) = fb.code(1, &["project", "plan", "--dir", &d, "--env", "qa"], &[]);
    assert!(err.contains("has no environment \"nope\""), "{err}");
    let (_, err) = fb.code(1, &["project", "plan", "--dir", &d, "--env", "unknown"], &[]);
    assert!(err.contains("no [environments.unknown] table"), "{err}");
}

/// A TLS-terminating proxy in front of the server (certificate of the
/// forgebase-auth test CA, for `localhost`); returns its port.
fn tls_proxy(upstream: String) -> u16 {
    use rustls::pki_types::pem::PemObject;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../forgebase-auth/tests/fixtures");
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(fixtures.join("tls-server.pem"))
        .unwrap()
        .map(|c| c.unwrap())
        .collect();
    let key = PrivateKeyDer::from_pem_file(fixtures.join("tls-server.key")).unwrap();
    let cfg =
        rustls::ServerConfig::builder_with_provider(std::sync::Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(std::sync::Arc::new(cfg));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            loop {
                let Ok((tcp, _)) = listener.accept().await else { return };
                let acceptor = acceptor.clone();
                let upstream = upstream.clone();
                tokio::spawn(async move {
                    let Ok(mut tls) = acceptor.accept(tcp).await else {
                        return;
                    };
                    let Ok(mut up) = tokio::net::TcpStream::connect(&upstream).await else {
                        return;
                    };
                    let _ = tokio::io::copy_bidirectional(&mut tls, &mut up).await;
                });
            }
        });
    });
    port
}

/// `--server https://...` with `--ca-file`: plan and apply through TLS; an
/// untrusted certificate is refused.
#[test]
fn apply_over_https_with_a_private_ca() {
    let fb = Fb::start();
    let port = tls_proxy(fb.addr.clone());
    fb.ok(&["new", "web", "--template", "basic"], &[]);
    let ca = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../forgebase-auth/tests/fixtures/tls-ca.pem")
        .display()
        .to_string();
    let url = format!("https://localhost:{port}");
    let https = [("FORGEBASE_URL", url.as_str())];
    let (out, _) = fb.code(
        2,
        &["project", "plan", "--dir", "web/forgebase", "--ca-file", &ca],
        &https,
    );
    assert!(out.contains(&format!("against https://localhost:{port}")), "{out}");
    fb.ok(
        &[
            "project",
            "apply",
            "--dir",
            "web/forgebase",
            "--ca-file",
            &ca,
            "--auto-approve",
        ],
        &https,
    );
    assert_eq!(fb.api("GET", "/api/v1/collections/todos", None).0, 200);
    fb.code(
        0,
        &["project", "plan", "--dir", "web/forgebase"],
        &[("FORGEBASE_URL", url.as_str()), ("FORGEBASE_CA_FILE", ca.as_str())],
    );
    // Without the CA the certificate is not trusted: no fallback to plain HTTP.
    let (_, err) = fb.code(1, &["project", "plan", "--dir", "web/forgebase"], &https);
    assert!(
        err.contains("cannot reach the server") && err.to_lowercase().contains("certificate"),
        "{err}"
    );
}
