//! Black-box tests of `forgebase backup drill`, the drill REST API and the
//! sealed TLS state in backups (ADR-0041), on the backend
//! `FORGEBASE_TEST_BACKEND` selects (SQLite by default; `postgres` with
//! `FORGEBASE_PG_ENGINE_DIR` runs the bundled engine).
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../../../tests/integration/common/mod.rs"]
mod common;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
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
    forgebase(cwd)
        .args(common::backend_args(cwd, args))
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn ok(o: &Output) -> String {
    assert!(
        o.status.success(),
        "exit {:?}\nstdout: {}\nstderr: {}",
        o.status,
        text(&o.stdout),
        text(&o.stderr)
    );
    text(&o.stdout)
}

fn json_of(o: &Output) -> Value {
    serde_json::from_str(&ok(o)).unwrap()
}

/// Short paths: the engine's Unix socket path is limited to 103 bytes.
fn tempdir() -> tempfile::TempDir {
    let base = std::env::var_os("FORGEBASE_TEST_PG_TMP").unwrap_or_else(|| std::env::temp_dir().into_os_string());
    tempfile::Builder::new().prefix("fbdr-").tempdir_in(base).unwrap()
}

fn create_admin(app: &Path) {
    let mut create = forgebase(app)
        .args(["admin", "create", "--email", "root@example.com", "--password-stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    create.stdin.take().unwrap().write_all(b"root password 1\n").unwrap();
    ok(&create.wait_with_output().unwrap());
}

/// An export bundle with `n` books, imported with `forgebase import`.
fn import_books(app: &Path, n: usize) {
    let dir = app.join("bundle");
    std::fs::create_dir_all(dir.join("collections")).unwrap();
    let coll = json!({
        "name": "books", "type": "base",
        "fields": [
            {"name": "title", "type": "text"},
            {"name": "pages", "type": "number", "integer": true}
        ]
    });
    std::fs::write(
        dir.join("collections/books.schema.json"),
        serde_json::to_vec(&coll).unwrap(),
    )
    .unwrap();
    let lines: String = (0..n)
        .map(|i| {
            format!(
                "{}\n",
                json!({"id": format!("book{i:010}"), "title": format!("Book {i}"), "pages": i * 10})
            )
        })
        .collect();
    std::fs::write(dir.join("collections/books.ndjson"), lines).unwrap();
    let bundle = json!({
        "format": 1, "created": "2026-01-01T00:00:00Z", "forgebase_version": "test",
        "collections": [{"name": "books", "id": "", "type": "base", "records": n}], "config": false
    });
    std::fs::write(dir.join("bundle.json"), bundle.to_string()).unwrap();
    ok(&run(app, &["import", "collections", dir.to_str().unwrap()]));
}

/// A blob the way the storage service writes one: the file under
/// `storage/blobs/<2 hex>/<sha256>` and its row. Written with SQL, as the
/// project's own superuser on PostgreSQL.
fn add_blob(app: &Path, body: &[u8]) -> (String, PathBuf) {
    use sha2::{Digest, Sha256};
    let sha: String = Sha256::digest(body).iter().map(|b| format!("{b:02x}")).collect();
    let file = app.join("forgebase-data/storage/blobs").join(&sha[..2]).join(&sha);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, body).unwrap();
    let table = if common::postgres() {
        "forgebase._forgebase_storage_blobs"
    } else {
        "_forgebase_storage_blobs"
    };
    let sql = format!(
        "INSERT INTO {table} (sha256, size, refcount, created_at) VALUES ('{sha}', {}, 1, 0)",
        body.len()
    );
    if common::postgres() {
        ok(&run(app, &["db", "psql", "--superuser", "-c", &sql]));
    } else {
        sqlite_exec(&app.join("forgebase-data/forgebase.db"), &sql);
    }
    (sha, file)
}

fn sqlite_exec(db: &Path, sql: &str) {
    let db = db.to_path_buf();
    let sql = sql.to_owned();
    // A private runtime: the test itself is synchronous.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async move {
        use forgebase_db::{Backend, TxMode};
        let cfg = forgebase_core::Config::defaults(db.parent().unwrap());
        let opts = forgebase_db::DbOptions::from_config(&cfg.database, db).create_if_missing(false);
        let database = forgebase_db::Database::open(opts).await.unwrap();
        let mut tx = database.begin(TxMode::Normal).await.unwrap();
        tx.execute(&forgebase_db::Statement::raw(sql)).await.unwrap();
        tx.commit().await.unwrap();
        database.close().await;
    });
}

/// `init`, books, an administrator (so the master key exists) and a blob.
fn project(tmp: &Path) -> (PathBuf, String) {
    ok(&run(tmp, &["init", "app", "--no-admin"]));
    let app = tmp.join("app");
    create_admin(&app);
    import_books(&app, 3);
    let (sha, _) = add_blob(&app, b"the quick brown fox");
    (app, sha)
}

fn check<'a>(report: &'a Value, name: &str) -> &'a Value {
    report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("no {name} check in {report:#}"))
}

fn snapshot_dir(app: &Path) -> PathBuf {
    let snaps = app.join("forgebase-data/backups/snapshots");
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(snaps)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("MANIFEST.json").is_file())
        .collect();
    dirs.sort();
    dirs.pop().expect("a snapshot")
}

fn tmp_is_empty(app: &Path) -> bool {
    std::fs::read_dir(app.join("forgebase-data/tmp"))
        .unwrap()
        .flatten()
        .all(|e| !e.file_name().to_string_lossy().starts_with("drill-"))
}

/// A drill of a fresh backup passes with the counts of the manifest, reports
/// RTO and RPO, records its history, leaves the live data alone and cleans up.
#[test]
fn a_backup_drill_passes_with_correct_counts() {
    let tmp = tempdir();
    let (app, _sha) = project(tmp.path());
    let live = std::fs::metadata(app.join("forgebase-data/auth/master.key"))
        .unwrap()
        .len();
    let made = json_of(&run(&app, &["backup", "--json"]));
    let stats = &made["manifest"]["stats"];
    assert_eq!(stats["collections"]["books"]["min"], 3, "{made:#}");
    assert_eq!(stats["collections"]["books"]["max"], 3);
    assert_eq!(stats["storage_blobs"]["min"], 1);

    let report = json_of(&run(&app, &["backup", "drill", "--json"]));
    assert_eq!(report["outcome"], "passed", "{report:#}");
    assert_eq!(
        report["engine"].as_str().unwrap().split(' ').next().unwrap(),
        if common::postgres() { "postgres" } else { "sqlite" }
    );
    for name in [
        "restore",
        "server",
        "schema",
        "doctor",
        "counts",
        "records",
        "blobs",
        "master_key",
    ] {
        let c = check(&report, name);
        assert!(c["status"] == "ok" || c["status"] == "warn", "{name}: {c}");
    }
    assert_eq!(check(&report, "counts")["status"], "ok");
    assert!(
        check(&report, "counts")["detail"]
            .as_str()
            .unwrap()
            .contains("as recorded"),
        "{report:#}"
    );
    assert!(report["counts"]["records"].as_u64().unwrap() >= 3, "{report:#}");
    assert!(report["counts"]["records_sampled"].as_u64().unwrap() >= 3);
    assert_eq!(report["counts"]["blobs_sampled"], 1);
    // The measured recovery time and the backup's age.
    assert!(report["rto_seconds"].as_f64().unwrap() > 0.0);
    let p = &report["phases"];
    assert!(p["restore_ms"].as_u64().unwrap() > 0 && p["start_ms"].as_u64().unwrap() > 0);
    assert!(report["rpo_seconds"].as_i64().unwrap() >= 0);
    assert!(report["backup_created"].as_str().unwrap().contains('T'));

    // History on disk, scratch deleted, live data untouched.
    let hist = app.join("forgebase-data/backups/drills");
    assert_eq!(std::fs::read_dir(&hist).unwrap().count(), 1);
    assert!(tmp_is_empty(&app), "the scratch directory is deleted");
    assert_eq!(
        std::fs::metadata(app.join("forgebase-data/auth/master.key"))
            .unwrap()
            .len(),
        live
    );

    // The human report.
    let human = ok(&run(&app, &["backup", "drill", "latest"]));
    assert!(human.contains("RTO") && human.contains("RPO"), "{human}");
    assert!(human.contains("drill PASSED"), "{human}");
}

/// A corrupt blob in the backup fails the drill with the file named.
#[test]
fn a_corrupt_blob_fails_the_drill_with_a_clear_reason() {
    let tmp = tempdir();
    let (app, sha) = project(tmp.path());
    ok(&run(&app, &["backup"]));
    let snap = snapshot_dir(&app);
    let blob = snap.join("storage/blobs").join(&sha[..2]).join(&sha);
    std::fs::write(&blob, b"the quick brown f0x").unwrap();

    let out = run(&app, &["backup", "drill", "--json"]);
    assert!(!out.status.success(), "a corrupt backup must fail the drill");
    let report: Value = serde_json::from_str(&text(&out.stdout)).unwrap();
    assert_eq!(report["outcome"], "failed");
    let reason = report["problems"].to_string();
    assert!(reason.contains(&sha) && reason.contains("corrupt"), "{reason}");
    assert!(tmp_is_empty(&app));
    // The failure is on record.
    let hist = std::fs::read_dir(app.join("forgebase-data/backups/drills"))
        .unwrap()
        .count();
    assert_eq!(hist, 1);
}

/// A damaged or missing manifest fails the drill, never restores.
#[test]
fn a_corrupt_manifest_fails_the_drill() {
    let tmp = tempdir();
    let (app, _) = project(tmp.path());
    ok(&run(&app, &["backup"]));
    let snap = snapshot_dir(&app);
    let manifest = snap.join("MANIFEST.json");
    let good = std::fs::read(&manifest).unwrap();

    std::fs::write(&manifest, &good[..good.len() / 2]).unwrap();
    let out = run(&app, &["backup", "drill", "--json"]);
    assert!(!out.status.success());
    let r: Value = serde_json::from_str(&text(&out.stdout)).unwrap();
    assert_eq!(r["outcome"], "failed");
    assert!(r["problems"].to_string().contains("MANIFEST.json is invalid"), "{r:#}");

    std::fs::remove_file(&manifest).unwrap();
    let out = run(&app, &["backup", "drill", snap.to_str().unwrap(), "--json"]);
    assert!(!out.status.success());
    let r: Value = serde_json::from_str(&text(&out.stdout)).unwrap();
    assert!(r["problems"].to_string().contains("MANIFEST.json is missing"), "{r:#}");
}

/// The counts recorded in the manifest are what the drill holds the restored
/// data to: a manifest that promises more records fails it.
#[test]
fn counts_that_disagree_with_the_manifest_fail_the_drill() {
    let tmp = tempdir();
    let (app, _) = project(tmp.path());
    ok(&run(&app, &["backup"]));
    let manifest = snapshot_dir(&app).join("MANIFEST.json");
    let mut m: Value = serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
    m["stats"]["collections"]["books"] = json!({"min": 5, "max": 5});
    std::fs::write(&manifest, serde_json::to_vec_pretty(&m).unwrap()).unwrap();

    let out = run(&app, &["backup", "drill", "--json"]);
    assert!(!out.status.success());
    let r: Value = serde_json::from_str(&text(&out.stdout)).unwrap();
    assert_eq!(check(&r, "counts")["status"], "fail", "{r:#}");
    assert!(
        check(&r, "counts")["detail"]
            .as_str()
            .unwrap()
            .contains("collection books has 3 record(s), the backup recorded 5"),
        "{r:#}"
    );
}

/// A blob the database names but the files do not have fails the drill.
#[test]
fn a_missing_blob_fails_the_drill() {
    let tmp = tempdir();
    let (app, sha) = project(tmp.path());
    // The file is gone before the backup: the manifest does not list it.
    std::fs::remove_file(app.join("forgebase-data/storage/blobs").join(&sha[..2]).join(&sha)).unwrap();
    ok(&run(&app, &["backup"]));
    let out = run(&app, &["backup", "drill", "--json"]);
    assert!(!out.status.success());
    let r: Value = serde_json::from_str(&text(&out.stdout)).unwrap();
    assert_eq!(check(&r, "blobs")["status"], "fail", "{r:#}");
    assert!(check(&r, "blobs")["detail"].as_str().unwrap().contains(&sha));
}

/// The backup holds no master key: the drill restores with the live one, with
/// an escrow copy named by `--master-key`, and refuses a wrong key.
#[test]
fn the_drill_proves_the_master_key_escrow() {
    let tmp = tempdir();
    let (app, _) = project(tmp.path());
    ok(&run(&app, &["backup"]));
    let live_key = app.join("forgebase-data/auth/master.key");

    let r = json_of(&run(&app, &["backup", "drill", "--json"]));
    assert_eq!(r["outcome"], "passed", "{r:#}");
    assert!(
        check(&r, "master_key")["detail"]
            .as_str()
            .unwrap()
            .contains("matches its fingerprint")
    );

    let escrow = tmp.path().join("escrow.key");
    std::fs::copy(&live_key, &escrow).unwrap();
    let r = json_of(&run(
        &app,
        &["backup", "drill", "--json", "--master-key", escrow.to_str().unwrap()],
    ));
    assert_eq!(r["outcome"], "passed", "{r:#}");
    assert!(
        check(&r, "master_key")["detail"]
            .as_str()
            .unwrap()
            .contains("escrow.key")
    );

    let wrong = tmp.path().join("wrong.key");
    std::fs::write(&wrong, b"not the key of this project, 32 bytes of it").unwrap();
    let out = run(
        &app,
        &["backup", "drill", "--json", "--master-key", wrong.to_str().unwrap()],
    );
    assert!(!out.status.success());
    let r: Value = serde_json::from_str(&text(&out.stdout)).unwrap();
    assert!(r["problems"].to_string().contains("does not match the backup"), "{r:#}");

    // With the key in the backup the drill needs none from outside.
    ok(&run(&app, &["backup", "--include-master-key"]));
    let r = json_of(&run(&app, &["backup", "drill", "--json"]));
    assert_eq!(check(&r, "master_key")["status"], "ok");
}

#[test]
fn keep_leaves_the_scratch_directory_for_inspection() {
    let tmp = tempdir();
    let (app, _) = project(tmp.path());
    ok(&run(&app, &["backup"]));
    let r = json_of(&run(&app, &["backup", "drill", "--json", "--keep", "--no-record"]));
    assert_eq!(r["outcome"], "passed");
    let kept = PathBuf::from(r["kept"].as_str().unwrap());
    assert!(
        kept.join("forgebase-data").exists() || kept.join("forgebase.db").exists() || kept.join("postgres").exists()
    );
    assert!(
        !app.join("forgebase-data/backups/drills").exists(),
        "--no-record writes no history"
    );
}

/// `[backup] include_tls` puts the sealed `tls/` directory into the backup
/// and a restore brings it back; the drill checks it.
#[test]
fn include_tls_backs_up_and_restores_the_sealed_tls_state() {
    let tmp = tempdir();
    let (app, _) = project(tmp.path());
    let state = app.join("forgebase-data/tls/acme/0123456789abcdef");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(state.join("account.sealed"), b"sealed-account").unwrap();
    std::fs::write(state.join("cert.sealed"), b"sealed-cert").unwrap();
    std::fs::write(state.join("status.json"), b"{}").unwrap();

    // Off unless asked (TLS mode is not acme here).
    let plain = json_of(&run(&app, &["backup", "--json"]));
    assert!(!plain["manifest"]["files"].to_string().contains("\"tls\""));

    let mut cmd = forgebase(&app);
    let out = cmd
        .env("FORGEBASE_BACKUP_INCLUDE_TLS", "true")
        .args(common::backend_args(&app, &["backup", "--json"]))
        .output()
        .unwrap();
    let made = json_of(&out);
    let files = made["manifest"]["files"].as_array().unwrap();
    let tls: Vec<&Value> = files.iter().filter(|f| f["kind"] == "tls").collect();
    assert_eq!(tls.len(), 3, "{made:#}");
    assert!(tls.iter().any(|f| f["path"] == "tls/acme/0123456789abcdef/cert.sealed"));
    let snap = PathBuf::from(made["path"].as_str().unwrap());
    assert!(snap.join("tls/acme/0123456789abcdef/account.sealed").is_file());

    // The drill finds it restored.
    let r = json_of(&run(&app, &["backup", "drill", snap.to_str().unwrap(), "--json"]));
    assert_eq!(check(&r, "tls")["status"], "ok", "{r:#}");

    // A restore into another data directory brings it back.
    let other = tmp.path().join("other");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::copy(app.join("forgebase-data/auth/master.key"), tmp.path().join("m.key")).unwrap();
    let out = forgebase(&other)
        .args(common::backend_args(
            &other,
            &[
                "restore",
                snap.to_str().unwrap(),
                "--master-key",
                tmp.path().join("m.key").to_str().unwrap(),
                "--data-dir",
                other.join("data").to_str().unwrap(),
            ],
        ))
        .output()
        .unwrap();
    ok(&out);
    let restored = other.join("data/tls/acme/0123456789abcdef");
    assert_eq!(std::fs::read(restored.join("cert.sealed")).unwrap(), b"sealed-cert");
    assert_eq!(
        std::fs::read(restored.join("account.sealed")).unwrap(),
        b"sealed-account"
    );
}

// ------------------------------------------------------------------ the API

struct Serve {
    child: Child,
    addr: String,
}

impl Serve {
    fn start(cwd: &Path) -> Self {
        let port = free_port();
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

fn metrics(addr: &str) -> String {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(s, "GET /metrics HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n").unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).unwrap();
    text(&buf)
}

/// `POST /system/backups/{id}/drill` runs a drill as a job on a live server,
/// `GET .../drills` returns the history with RTO and RPO, and the metrics say when
/// the last drill passed.
#[test]
fn the_api_runs_a_drill_and_keeps_the_history() {
    let tmp = tempdir();
    let (app, _) = project(tmp.path());
    let server = Serve::start(&app);
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
    let t = Some(token.as_str());

    // A snapshot through the API, then a drill of it.
    let (s, m) = call(
        &addr,
        "POST",
        "/api/v1/system/backups",
        t,
        Some(json!({"label": "api"})),
    );
    assert_eq!(s, 201, "{m}");
    let id = m["id"].as_str().unwrap().to_owned();

    // Not an administrator: refused.
    let (s, _) = call(&addr, "POST", &format!("/api/v1/system/backups/{id}/drill"), None, None);
    assert_eq!(s, 401);
    let (s, _) = call(&addr, "POST", "/api/v1/system/backups/nope/drill", t, None);
    assert_eq!(s, 404);

    let (s, started) = call(&addr, "POST", &format!("/api/v1/system/backups/{id}/drill"), t, None);
    assert_eq!(s, 202, "{started}");
    assert_eq!(started["backup_id"], id);
    let job = started["job_id"].as_str().unwrap().to_owned();

    // The job runs the drill; its result is the report.
    let deadline = std::time::Instant::now() + Duration::from_secs(240);
    let result = loop {
        let (s, j) = call(&addr, "GET", &format!("/api/v1/jobs/{job}"), t, None);
        assert_eq!(s, 200, "{j}");
        match j["status"].as_str().unwrap() {
            "succeeded" => break j,
            "dead" | "cancelled" => panic!("the drill job failed: {j:#}"),
            _ => {}
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the drill job did not finish: {j:#}"
        );
        std::thread::sleep(Duration::from_millis(500));
    };
    assert_eq!(result["result"]["outcome"], "passed", "{result:#}");
    assert_eq!(result["result"]["trigger"], "api");

    let (s, hist) = call(&addr, "GET", &format!("/api/v1/system/backups/{id}/drills"), t, None);
    assert_eq!(s, 200, "{hist}");
    assert_eq!(hist["backup_id"], id);
    let items = hist["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{hist:#}");
    assert_eq!(items[0]["outcome"], "passed");
    assert!(items[0]["rto_seconds"].as_f64().unwrap() > 0.0);
    assert!(items[0]["rpo_seconds"].as_i64().unwrap() >= 0);
    let (s, latest) = call(&addr, "GET", "/api/v1/system/backups/latest/drills", t, None);
    assert_eq!(s, 200);
    assert_eq!(latest["items"].as_array().unwrap().len(), 1);

    // Prometheus.
    let m = metrics(&addr);
    let line = m
        .lines()
        .find(|l| l.starts_with("forgebase_backup_last_drill_success_timestamp_seconds"))
        .unwrap_or_else(|| panic!("no drill gauge in:\n{m}"));
    let ts: f64 = line.split_whitespace().last().unwrap().parse().unwrap();
    assert!(ts > 1.7e9, "{line}");
    assert!(
        m.contains("forgebase_backup_drill_duration_seconds{phase=\"total\"}"),
        "{m}"
    );
    assert!(m.contains("forgebase_backup_last_drill_ok 1"), "{m}");
}
