//! Black-box tests of the `forgebase` binary: `init` then `serve` from an
//! empty folder, restart persistence, `status`, `migrate`, `doctor`, `version`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;

use common::{file_len, free_port, get_json, wait_healthy};
use forgebase_db::{Database, DbOptions};

/// `forgebase` with a clean environment (no inherited `FORGEBASE_*`).
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

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// A running `forgebase serve`, killed on drop if the test fails early.
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
        let addr = format!("127.0.0.1:{port}");
        let serve = Self {
            child: Some(child),
            addr,
        };
        assert!(
            wait_healthy(&serve.addr, Duration::from_secs(20)),
            "server did not become healthy"
        );
        serve
    }

    /// SIGTERM and wait for a clean exit (graceful shutdown).
    #[cfg(unix)]
    fn stop(mut self) {
        let mut child = self.child.take().unwrap();
        let ok = Command::new("kill")
            .args(["-TERM", &child.id().to_string()])
            .status()
            .unwrap()
            .success();
        assert!(ok, "kill -TERM failed");
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success(), "serve exited with {status}");
                return;
            }
            assert!(std::time::Instant::now() < deadline, "serve did not stop after SIGTERM");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[cfg(not(unix))]
    fn stop(mut self) {
        let mut child = self.child.take().unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
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

fn db_options(path: PathBuf) -> DbOptions {
    let cfg = forgebase_core::Config::defaults(Path::new("."));
    DbOptions::from_config(&cfg.database, path).create_if_missing(false)
}

fn doctor_json(cwd: &Path) -> (bool, serde_json::Value) {
    let out = run(cwd, &["doctor", "--json"]);
    let report: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("doctor --json is not JSON ({e}): {}", stdout(&out)));
    (out.status.success(), report)
}

fn check<'a>(report: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("no {name} check in {report}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn init_then_serve_from_an_empty_folder_and_survive_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();

    // init from an empty folder
    let out = run(&root, &["init", "my-project"]);
    assert!(out.status.success(), "init failed: {}", stderr(&out));
    assert!(stdout(&out).contains("cd my-project && forgebase serve"));
    let project = root.join("my-project");
    let data = project.join("forgebase-data");
    let db_path = data.join("forgebase.db");
    assert!(project.join("forgebase.toml").is_file());
    for dir in ["storage", "indexes", "backups", "tmp"] {
        assert!(data.join(dir).is_dir(), "{dir} missing");
    }
    assert!(db_path.is_file());

    // init again is idempotent
    let out = run(&root, &["init", "my-project"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("existing, kept"));
    assert!(stdout(&out).contains("0 migration(s) applied now"));

    // serve on a random free port, using ./forgebase.toml from the project dir
    let port = free_port();
    let server = Serve::start(&project, port);
    let addr = server.addr.clone();

    let (s, health) = get_json(&addr, "/health");
    assert_eq!((s, health), (200, serde_json::json!({"status": "ok"})));
    let (s, ready) = get_json(&addr, "/health/ready");
    assert_eq!(s, 200, "{ready}");
    assert_eq!(ready["status"], "ready");
    let (s, info) = get_json(&addr, "/api/system/info");
    assert_eq!(s, 200);
    assert_eq!(info["name"], "forgebase");
    assert_eq!(info["mode"], "embedded");
    assert_eq!(info["data_dir"], data.display().to_string());
    assert_eq!(info["database"]["path"], db_path.display().to_string());
    assert_eq!(info["database"]["journal_mode"], "wal");
    assert_eq!(
        info["database"]["schema_version"],
        forgebase_db::migrations::Migrator::system().latest_version()
    );
    let (s, err) = get_json(&addr, "/api/does-not-exist");
    assert_eq!(s, 404);
    assert_eq!(err["error"]["code"], "not_found");
    assert!(!err["error"]["request_id"].as_str().unwrap().is_empty());

    // write through the db layer from another process (WAL allows it)
    let db = Database::open(db_options(db_path.clone())).await.unwrap();
    let instance_id = db
        .meta_get("instance_id")
        .await
        .unwrap()
        .expect("instance id written by init");
    db.meta_set("e2e.row", "persisted").await.unwrap();
    db.close().await;

    // status sees the running server
    let out = run(&project, &["status", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let status: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(status["server"]["running"], true);
    assert_eq!(status["server"]["healthy"], true);
    assert_eq!(
        status["database"]["schema_version"],
        forgebase_db::migrations::Migrator::system().latest_version()
    );
    assert_eq!(status["instance_id"], instance_id.as_str());

    // migrate refuses while the server holds the data directory
    let out = run(&project, &["migrate"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("in use by another ForgeBase process"),
        "{}",
        stderr(&out)
    );

    // doctor is green while running
    let (ok, report) = doctor_json(&project);
    assert!(ok, "{report}");

    // graceful stop (SIGTERM) checkpoints the WAL
    server.stop();
    assert_eq!(file_len(&data.join("forgebase.db-wal")), 0);
    let (s, _) = (common::http("GET", &addr, "/health").is_err(), ());
    assert!(s, "port released");

    // restart: same instance, row still there, schema unchanged
    let server = Serve::start(&project, port);
    let (s, info) = get_json(&server.addr, "/api/system/info");
    assert_eq!(s, 200);
    assert_eq!(
        info["database"]["schema_version"],
        forgebase_db::migrations::Migrator::system().latest_version()
    );
    server.stop();

    let db = Database::open(db_options(db_path.clone())).await.unwrap();
    assert_eq!(db.meta_get("e2e.row").await.unwrap().as_deref(), Some("persisted"));
    assert_eq!(db.meta_get("instance_id").await.unwrap(), Some(instance_id));
    db.close().await;

    // migrate is idempotent once stopped
    let out = run(&project, &["migrate"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("already up to date"));
    let out = run(&project, &["migrate", "--status"]);
    assert!(out.status.success());
    assert!(stdout(&out).contains("applied  0001_initial"));
}

#[test]
fn serve_flags_override_config_and_a_second_server_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    // No init: serve creates the data directory itself, at --data-dir.
    let port = free_port();
    let child = forgebase(&root)
        .args(["serve", "--port", &port.to_string(), "--data-dir", "custom-data"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let server = Serve {
        child: Some(child),
        addr: format!("127.0.0.1:{port}"),
    };
    assert!(wait_healthy(&server.addr, Duration::from_secs(20)));
    let (_, info) = get_json(&server.addr, "/api/system/info");
    assert_eq!(info["data_dir"], root.join("custom-data").display().to_string());

    let other = free_port();
    let out = run(
        &root,
        &["serve", "--port", &other.to_string(), "--data-dir", "custom-data"],
    );
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("in use by another ForgeBase process"),
        "{}",
        stderr(&out)
    );
    server.stop();
}

#[test]
fn the_resp_listener_is_off_unless_asked_and_serve_flags_turn_it_on() {
    use std::io::{Read, Write};
    let ping = |addr: &str| -> Option<String> {
        let mut s = std::net::TcpStream::connect(addr).ok()?;
        s.set_read_timeout(Some(Duration::from_secs(10))).ok()?;
        s.write_all(b"PING\r\n").ok()?;
        let mut buf = [0u8; 16];
        let n = s.read(&mut buf).ok()?;
        Some(String::from_utf8_lossy(&buf[..n]).into_owned())
    };
    // default: nothing listens on the RESP port
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let (port, resp_port) = (free_port(), free_port());
    let off = Serve::start(&root, port);
    assert!(
        ping(&format!("127.0.0.1:{resp_port}")).is_none(),
        "RESP is off by default"
    );
    off.stop();
    // --resp --resp-port: PING works before AUTH, data commands need AUTH
    let child = forgebase(&root)
        .args([
            "serve",
            "--port",
            &port.to_string(),
            "--resp",
            "--resp-port",
            &resp_port.to_string(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let on = Serve {
        child: Some(child),
        addr: format!("127.0.0.1:{port}"),
    };
    assert!(wait_healthy(&on.addr, Duration::from_secs(20)));
    let addr = format!("127.0.0.1:{resp_port}");
    assert_eq!(ping(&addr).as_deref(), Some("+PONG\r\n"));
    let mut s = std::net::TcpStream::connect(&addr).unwrap();
    s.write_all(b"GET k\r\n").unwrap();
    let mut buf = [0u8; 64];
    let n = s.read(&mut buf).unwrap();
    assert!(String::from_utf8_lossy(&buf[..n]).starts_with("-NOAUTH"));
    on.stop();
    // environment variables work too
    let child = forgebase(&root)
        .env("FORGEBASE_RESP_ENABLED", "true")
        .env("FORGEBASE_RESP_PORT", resp_port.to_string())
        .args(["serve", "--port", &port.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let env_on = Serve {
        child: Some(child),
        addr: format!("127.0.0.1:{port}"),
    };
    assert!(wait_healthy(&env_on.addr, Duration::from_secs(20)));
    assert_eq!(ping(&addr).as_deref(), Some("+PONG\r\n"));
    env_on.stop();
}

#[test]
fn doctor_flags_a_missing_database() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    assert!(run(root, &["init"]).status.success());
    std::fs::remove_file(root.join("forgebase-data/forgebase.db")).unwrap();
    let (ok, report) = doctor_json(root);
    assert!(!ok, "doctor must fail: {report}");
    let db = check(&report, "database");
    assert_eq!(db["status"], "fail");
    assert!(db["detail"].as_str().unwrap().contains("missing"), "{db}");
    let out = run(root, &["doctor"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stdout(&out).contains("[FAIL] database"), "{}", stdout(&out));
}

#[test]
fn doctor_flags_an_uninitialized_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let (ok, report) = doctor_json(tmp.path());
    assert!(!ok);
    assert_eq!(check(&report, "data_dir")["status"], "fail");
    assert_eq!(check(&report, "database")["status"], "fail");
    assert!(
        !tmp.path().join("forgebase-data").exists(),
        "doctor must not create anything"
    );
}

#[test]
fn doctor_flags_a_file_that_is_not_a_database() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    assert!(run(root, &["init"]).status.success());
    std::fs::write(root.join("forgebase-data/forgebase.db"), vec![0x42; 16 * 1024]).unwrap();
    let (ok, report) = doctor_json(root);
    assert!(!ok, "{report}");
    let db = check(&report, "database");
    assert_eq!(db["status"], "fail");
    assert!(db["detail"].as_str().unwrap().contains("corrupt"), "{db}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn doctor_flags_damaged_pages() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    assert!(run(root, &["init"]).status.success());
    let path = root.join("forgebase-data/forgebase.db");

    let db = Database::open(db_options(path.clone())).await.unwrap();
    for i in 0..300 {
        db.meta_set(&format!("row-{i:04}"), &"v".repeat(200)).await.unwrap();
    }
    let page = db.info().await.unwrap().page_size as usize;
    db.checkpoint().await.unwrap();
    db.close().await;

    let mut bytes = std::fs::read(&path).unwrap();
    for b in &mut bytes[page..] {
        *b = 0xff;
    }
    std::fs::write(&path, &bytes).unwrap();

    let (ok, report) = doctor_json(root);
    assert!(!ok, "{report}");
    assert_eq!(check(&report, "database")["status"], "fail", "{report}");
}

#[test]
fn version_and_help() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run(tmp.path(), &["version"]);
    assert!(out.status.success());
    assert!(stdout(&out).starts_with(&format!("forgebase {}", env!("CARGO_PKG_VERSION"))));
    let out = run(tmp.path(), &["version", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["name"], "forgebase");
    assert!(!v["sqlite"].as_str().unwrap().is_empty());
    let out = run(tmp.path(), &["--help"]);
    for cmd in ["init", "serve", "migrate", "status", "doctor", "version"] {
        assert!(stdout(&out).contains(cmd), "help lists {cmd}");
    }
}

#[test]
fn bad_configuration_is_reported() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("forgebase.toml"), "[server]\nprot = 1\n").unwrap();
    let out = run(tmp.path(), &["status"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("invalid config file"), "{}", stderr(&out));

    let out = forgebase(tmp.path())
        .env("FORGEBASE_PORT", "not-a-port")
        .args(["--config", "/nonexistent.toml", "status"])
        .output()
        .unwrap();
    assert!(!out.status.success());
}
