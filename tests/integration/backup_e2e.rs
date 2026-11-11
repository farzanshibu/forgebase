//! Black-box tests of `forgebase backup`, `restore`, `export`, `import` and the
//! full `doctor` against a real `forgebase serve` process.
//!
//! SPEC §27 #21 (restart the server and verify persistence) and #22 (back up
//! and restore the database) run end to end here, including a backup taken
//! while the server is serving and a restore after the data was destroyed.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;

use common::{free_port, startup_timeout, wait_healthy};
use forgebase_data::schema::{Collection, Field, FieldKind};
use serde_json::{Value as Json, json};

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

fn ok(cwd: &Path, args: &[&str]) -> String {
    let o = run(cwd, args);
    assert!(
        o.status.success(),
        "forgebase {args:?} failed: {}\n{}",
        String::from_utf8_lossy(&o.stderr),
        String::from_utf8_lossy(&o.stdout)
    );
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn fails(cwd: &Path, args: &[&str]) -> String {
    let o = run(cwd, args);
    assert!(!o.status.success(), "forgebase {args:?} should have failed");
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stderr),
        String::from_utf8_lossy(&o.stdout)
    )
}

struct Serve {
    child: Option<Child>,
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
        assert!(
            wait_healthy(&format!("127.0.0.1:{port}"), startup_timeout()),
            "server not healthy"
        );
        Self { child: Some(child) }
    }

    fn stop(mut self) {
        let mut child = self.child.take().unwrap();
        assert!(
            Command::new("kill")
                .args(["-TERM", &child.id().to_string()])
                .status()
                .unwrap()
                .success()
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success(), "serve exited with {status}");
                return;
            }
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

/// A hand-made export bundle: `books` with `n` records with fixed ids.
fn write_bundle(dir: &Path, n: usize) {
    std::fs::create_dir_all(dir.join("collections")).unwrap();
    let coll = Collection::base("books")
        .with_field(Field::new(
            "title",
            FieldKind::Text {
                min_length: None,
                max_length: None,
            },
        ))
        .with_field(Field::new(
            "pages",
            FieldKind::Number {
                min: None,
                max: None,
                integer: true,
            },
        ));
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
}

fn project() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    ok(&root, &["init", "proj"]);
    let proj = root.join("proj");
    (tmp, proj)
}

/// Records of `books` as seen through `export records` (works with the server running).
fn books(proj: &Path, tag: &str) -> Vec<Json> {
    let out = proj.join(format!("books-{tag}.ndjson"));
    ok(proj, &["export", "records", "books", out.to_str().unwrap()]);
    let text = std::fs::read_to_string(&out).unwrap();
    text.lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

fn ids(v: &[Json]) -> Vec<String> {
    v.iter().map(|r| r["id"].as_str().unwrap().to_owned()).collect()
}

fn doctor(proj: &Path, args: &[&str]) -> (bool, Json) {
    let mut a = vec!["doctor", "--json"];
    a.extend_from_slice(args);
    let o = run(proj, &a);
    (o.status.success(), serde_json::from_slice(&o.stdout).unwrap())
}

fn check<'a>(report: &'a Json, name: &str) -> &'a Json {
    report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("no {name} check in {report}"))
}

/// SPEC §27 #21: restart the server and verify persistence.
#[test]
fn scenario_21_restart_keeps_collections_and_records() {
    let (_tmp, proj) = project();
    let bundle = proj.join("seed");
    write_bundle(&bundle, 20);
    let out = ok(&proj, &["import", "collections", bundle.to_str().unwrap()]);
    assert!(out.contains("books") && out.contains("created"), "{out}");

    let server = Serve::start(&proj);
    let before = books(&proj, "before");
    assert_eq!(before.len(), 20);
    server.stop();

    let server = Serve::start(&proj);
    let after = books(&proj, "after");
    assert_eq!(ids(&before), ids(&after), "records and their ids survive a restart");
    assert_eq!(before[7]["title"], "Book 7");
    server.stop();
}

/// SPEC §27 #22: back up and restore the database.
#[test]
fn scenario_22_online_backup_then_restore_after_data_loss() {
    let (_tmp, proj) = project();
    let data = proj.join("forgebase-data");
    let bundle = proj.join("seed");
    write_bundle(&bundle, 40);
    ok(&proj, &["import", "collections", bundle.to_str().unwrap()]);

    // Online snapshot while the server is serving (read-only access, no lock needed).
    let server = Serve::start(&proj);
    let out = ok(&proj, &["backup", "--label", "nightly", "--keep-last", "2"]);
    assert!(out.contains("backup written"), "{out}");
    ok(&proj, &["backup", "--keep-last", "2"]);
    ok(&proj, &["backup", "--keep-last", "2"]);
    let list: Json = serde_json::from_str(&ok(&proj, &["backup", "list", "--json"])).unwrap();
    assert_eq!(
        list["snapshots"].as_array().unwrap().len(),
        2,
        "retention keeps the newest two: {list}"
    );
    let latest = list["snapshots"][0]["id"].as_str().unwrap().to_owned();
    let verify = ok(&proj, &["backup", "verify", "latest"]);
    assert!(verify.contains("backup is valid"), "{verify}");

    // An explicit destination, and doctor sees the snapshots.
    let explicit = proj.join("offsite-backup");
    ok(&proj, &["backup", explicit.to_str().unwrap(), "--no-indexes"]);
    assert!(explicit.join("MANIFEST.json").is_file());
    let (healthy, report) = doctor(&proj, &["--deep"]);
    assert_eq!(check(&report, "snapshots")["status"], "ok", "{report}");
    assert!(
        check(&report, "snapshots")["detail"]
            .as_str()
            .unwrap()
            .contains("verified")
    );
    assert!(healthy, "{report}");

    // The expected state, then the server stops and the data is destroyed.
    let expected = books(&proj, "expected");
    assert_eq!(expected.len(), 40);

    // A restore into a data directory a server holds is refused.
    let msg = fails(&proj, &["restore", "latest", "--force"]);
    assert!(msg.contains("stop the server before restoring"), "{msg}");
    server.stop();

    if common::postgres() {
        // The cluster is the database: wipe it and leave junk behind.
        let _ = std::fs::remove_dir_all(data.join("postgres/data"));
        std::fs::create_dir_all(data.join("postgres/data")).unwrap();
        std::fs::write(data.join("postgres/data/PG_VERSION"), b"junk").unwrap();
    } else {
        for f in ["forgebase.db", "forgebase.db-wal", "forgebase.db-shm"] {
            let _ = std::fs::remove_file(data.join(f));
        }
        std::fs::write(data.join("forgebase.db"), b"this is not a database").unwrap();
    }
    let (healthy, report) = doctor(&proj, &[]);
    assert!(!healthy, "doctor must fail on a destroyed database: {report}");
    assert_eq!(check(&report, "database")["status"], "fail");

    // Restore refuses to overwrite without --force, then restores the newest snapshot.
    let msg = fails(&proj, &["restore", "latest"]);
    assert!(msg.contains("--force"), "{msg}");
    let out = ok(&proj, &["restore", "latest", "--force"]);
    assert!(out.contains("previous data kept in"), "{out}");
    assert!(
        data.join("backups").read_dir().unwrap().any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("pre-restore-"))
    );

    let (healthy, report) = doctor(&proj, &[]);
    assert!(healthy, "{report}");
    assert_eq!(check(&report, "database")["status"], "ok", "{report}");
    assert_eq!(check(&report, "master_key")["status"], "ok", "{report}");

    let server = Serve::start(&proj);
    let restored = books(&proj, "restored");
    assert_eq!(
        restored, expected,
        "the restored instance holds exactly the backed-up records"
    );
    server.stop();

    // Restore by id and by point in time work; a missing time is an error.
    ok(&proj, &["restore", &latest, "--force"]);
    ok(&proj, &["restore", "2999-01-01T00:00:00Z", "--force"]);
    let msg = fails(&proj, &["restore", "2000-01-01T00:00:00Z", "--force"]);
    assert!(msg.contains("not found"), "{msg}");

    // Explicit backup directories restore too, and corruption is caught first.
    ok(&proj, &["restore", explicit.to_str().unwrap(), "--force"]);
    let db = if common::postgres() {
        explicit.join("postgres/PG_VERSION")
    } else {
        explicit.join("forgebase.db")
    };
    let mut bytes = std::fs::read(&db).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    std::fs::write(&db, bytes).unwrap();
    let o = run(&proj, &["backup", "verify", explicit.to_str().unwrap()]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stdout).contains("NOT valid"));
    let msg = fails(&proj, &["restore", explicit.to_str().unwrap(), "--force"]);
    assert!(msg.contains("verification failed"), "{msg}");
    // The failed restore did not touch the data.
    assert!(ok(&proj, &["doctor"]).contains("0 failed"));
}

#[test]
fn export_import_round_trip_between_projects_and_config() {
    let (_tmp, a) = project();
    let bundle = a.join("seed");
    write_bundle(&bundle, 15);
    ok(&a, &["import", "collections", bundle.to_str().unwrap()]);

    let out_dir = a.join("bundle-out");
    ok(
        &a,
        &["export", "collections", out_dir.to_str().unwrap(), "--with-config"],
    );
    assert!(out_dir.join("config.toml").is_file());
    assert!(out_dir.join("bundle.json").is_file());

    // A second project receives the bundle, with ids preserved.
    let (_tmp2, b) = project();
    let dry = ok(&b, &["import", "collections", out_dir.to_str().unwrap(), "--dry-run"]);
    assert!(dry.contains("dry run"), "{dry}");
    ok(&b, &["import", "collections", out_dir.to_str().unwrap()]);
    assert_eq!(ids(&books(&a, "a")), ids(&books(&b, "b")));
    // Importing again collides unless asked to skip.
    let msg = fails(&b, &["import", "collections", out_dir.to_str().unwrap()]);
    assert!(msg.contains("already exists"), "{msg}");
    let out = ok(
        &b,
        &[
            "import",
            "collections",
            out_dir.to_str().unwrap(),
            "--on-conflict",
            "skip",
        ],
    );
    assert!(out.contains("15 skipped"), "{out}");

    // Record-level formats.
    let csv = a.join("books.csv");
    ok(
        &a,
        &["export", "records", "books", csv.to_str().unwrap(), "--format", "csv"],
    );
    let text = std::fs::read_to_string(&csv).unwrap();
    assert!(text.starts_with("id,created,updated,title,pages\n"), "{text}");
    assert_eq!(text.lines().count(), 16);
    let json_file = a.join("books.json");
    ok(
        &a,
        &[
            "export",
            "records",
            "books",
            json_file.to_str().unwrap(),
            "--format",
            "json",
        ],
    );
    let arr: Vec<Json> = serde_json::from_str(&std::fs::read_to_string(&json_file).unwrap()).unwrap();
    assert_eq!(arr.len(), 15);
    ok(
        &b,
        &[
            "import",
            "records",
            "books",
            json_file.to_str().unwrap(),
            "--format",
            "json",
            "--on-conflict",
            "skip",
        ],
    );

    // Config export and import validate like the server.
    let cfg = a.join("exported.toml");
    ok(&a, &["export", "config", cfg.to_str().unwrap()]);
    let (_tmp3, c) = project();
    let msg = fails(&c, &["import", "config", cfg.to_str().unwrap()]);
    assert!(msg.contains("--force"), "{msg}");
    ok(&c, &["import", "config", cfg.to_str().unwrap(), "--force"]);
    std::fs::write(&cfg, "[server]\nport = 99999\n").unwrap();
    fails(&c, &["import", "config", cfg.to_str().unwrap(), "--force"]);
}

#[test]
fn doctor_reports_key_snapshots_and_an_interrupted_restore() {
    let (_tmp, proj) = project();
    let (healthy, report) = doctor(&proj, &[]);
    assert!(healthy, "{report}");
    assert_eq!(
        check(&report, "snapshots")["status"],
        "warn",
        "no snapshot yet: {report}"
    );
    assert_eq!(
        check(&report, "master_key")["status"],
        "warn",
        "no key before the first start"
    );

    // Every check has a name, a status and a detail (the --json contract).
    for c in report["checks"].as_array().unwrap() {
        assert!(c["name"].is_string() && c["detail"].is_string());
        assert!(["ok", "warn", "fail"].contains(&c["status"].as_str().unwrap()));
    }
    assert_eq!(report["ok"], true);
    assert!(report["warnings"].as_u64().unwrap() >= 2);

    // A leftover restore journal fails the check and `restore --recover` clears it.
    let data = proj.join("forgebase-data");
    let aside = data.join("backups/pre-restore-x");
    std::fs::create_dir_all(&aside).unwrap();
    std::fs::write(
        data.join(".restore-journal.json"),
        json!({"aside": aside, "staging": data.join("tmp/restore-x"), "items": [], "committed": false}).to_string(),
    )
    .unwrap();
    let (healthy, report) = doctor(&proj, &[]);
    assert!(!healthy);
    assert_eq!(check(&report, "restore")["status"], "fail");
    let out = ok(&proj, &["restore", "--recover"]);
    assert!(out.contains("rolled back"), "{out}");
    assert!(doctor(&proj, &[]).0);
}
