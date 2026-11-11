//! Black-box tests of `forgebase backup`, `backup verify`, `restore` and
//! `db dump` on the backend `FORGEBASE_TEST_BACKEND` selects (FB-150,
//! ADR-0028 §5). PostgreSQL runs need `FORGEBASE_PG_ENGINE_DIR`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::Value;

fn pg() -> Option<PathBuf> {
    if std::env::var("FORGEBASE_TEST_BACKEND").as_deref() != Ok("postgres") {
        return None;
    }
    std::env::var_os("FORGEBASE_PG_ENGINE_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn forgebase(cwd: &Path, engine: Option<&Path>) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_forgebase"));
    cmd.current_dir(cwd);
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("FORGEBASE_") {
            cmd.env_remove(k);
        }
    }
    if let Some(e) = engine {
        cmd.env("FORGEBASE_PG_ENGINE_DIR", e);
    }
    cmd.env("NO_COLOR", "1");
    cmd
}

fn run(cwd: &Path, engine: Option<&Path>, args: &[&str]) -> Output {
    forgebase(cwd, engine).args(args).stdin(Stdio::null()).output().unwrap()
}

fn ok(out: &Output) -> String {
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "exit {:?}\nstdout:\n{stdout}\nstderr:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

fn json(out: &Output) -> Value {
    serde_json::from_str(&ok(out)).unwrap()
}

fn tempdir() -> tempfile::TempDir {
    // Short paths: the engine's Unix socket path is limited to 103 bytes.
    let base = std::env::var_os("FORGEBASE_TEST_PG_TMP").unwrap_or_else(|| std::env::temp_dir().into_os_string());
    tempfile::Builder::new().prefix("fb150c-").tempdir_in(base).unwrap()
}

fn admin(cwd: &Path, e: Option<&Path>, email: &str) {
    let mut child = forgebase(cwd, e)
        .args(["admin", "create", "--email", email, "--password-stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        child.stdin.take().unwrap().write_all(b"a long password 1\n").unwrap();
    }
    ok(&child.wait_with_output().unwrap());
}

fn init(tmp: &Path, e: Option<&Path>) -> PathBuf {
    let backend = if e.is_some() { "postgres" } else { "sqlite" };
    ok(&run(tmp, e, &["init", "app", "--database", backend, "--no-admin"]));
    tmp.join("app")
}

#[test]
fn backup_verify_and_restore_round_trip() {
    let e = pg();
    let e = e.as_deref();
    let tmp = tempdir();
    let app = init(tmp.path(), e);
    let data = app.join("forgebase-data");
    admin(&app, e, "first@example.com");

    // A snapshot, then one at a destination.
    let made = json(&run(&app, e, &["backup", "--label", "before", "--json"]));
    let engine = &made["manifest"]["engine"];
    assert_eq!(engine["kind"], if e.is_some() { "postgres" } else { "sqlite" });
    if e.is_some() {
        assert_eq!(engine["mode"], "managed");
        assert_eq!(engine["method"], "basebackup");
        assert_eq!(made["manifest"]["format"], 2);
    }
    assert!(data.join("tmp").read_dir().unwrap().next().is_none(), "no passfile or staging is left");
    let dest = tmp.path().join("bk");
    ok(&run(&app, e, &["backup", dest.to_str().unwrap()]));
    assert!(dest.join("MANIFEST.json").is_file());

    let listed = json(&run(&app, e, &["backup", "list", "--json"]));
    assert_eq!(listed["snapshots"].as_array().unwrap().len(), 1);
    let text = ok(&run(&app, e, &["backup", "verify", "latest"]));
    assert!(text.contains("backup is valid"), "{text}");
    let v = json(&run(&app, e, &["backup", "verify", dest.to_str().unwrap(), "--json"]));
    assert_eq!(v["ok"], true, "{v}");
    if e.is_some() {
        assert!(
            !v["warnings"].to_string().contains("pg_verifybackup was not run"),
            "the CLI passes the engine's tools: {v}"
        );
    }
    let d = json(&run(&app, e, &["doctor", "--json", "--deep"]));
    let snap = d["checks"].as_array().unwrap().iter().find(|c| c["name"] == "snapshots").unwrap();
    assert_eq!(snap["status"], "ok", "{d}");
    assert!(snap["detail"].as_str().unwrap().contains("verified"), "{d}");

    // Change the data, then put the backup back.
    admin(&app, e, "second@example.com");
    assert!(ok(&run(&app, e, &["admin", "list"])).contains("second@example.com"));
    let out = run(&app, e, &["restore", dest.to_str().unwrap()]);
    assert!(!out.status.success(), "needs --force over an existing database");
    assert!(String::from_utf8_lossy(&out.stderr).contains("--force"));
    let text = ok(&run(&app, e, &["restore", dest.to_str().unwrap(), "--force"]));
    assert!(text.contains("restored"), "{text}");
    let list = ok(&run(&app, e, &["admin", "list"]));
    assert!(list.contains("first@example.com") && !list.contains("second@example.com"), "{list}");
    assert!(
        std::fs::read_dir(data.join("backups")).unwrap().flatten().any(|e| e
            .file_name()
            .to_string_lossy()
            .starts_with("pre-restore-")),
        "the replaced data is kept"
    );
    assert_eq!(
        std::fs::read_to_string(data.join("forgebase.backend")).unwrap(),
        if e.is_some() { "postgres-managed\n" } else { "sqlite\n" }
    );
    ok(&run(&app, e, &["doctor"]));
}

#[test]
fn a_physical_backup_restores_into_an_empty_project_and_db_dump_writes_a_portable_dump() {
    let Some(engine) = pg() else {
        eprintln!("skipped: PostgreSQL only");
        return;
    };
    let e = Some(engine.as_path());
    let tmp = tempdir();
    let app = init(tmp.path(), e);
    admin(&app, e, "first@example.com");
    let dest = tmp.path().join("bk");
    ok(&run(&app, e, &["backup", dest.to_str().unwrap()]));

    // db dump: custom format by default, plain with --plain, never over a file.
    let dump = json(&run(&app, e, &["db", "dump", "--json"]));
    let path = PathBuf::from(dump["path"].as_str().unwrap());
    assert_eq!(&std::fs::read(&path).unwrap()[..5], b"PGDMP");
    assert!(dump["bytes"].as_u64().unwrap() > 10_000);
    let sql = tmp.path().join("dump.sql");
    ok(&run(&app, e, &["db", "dump", sql.to_str().unwrap(), "--plain", "--schema-only"]));
    let text = std::fs::read_to_string(&sql).unwrap();
    assert!(text.contains("CREATE TABLE") && !text.contains("OWNER TO") && !text.contains("GRANT "), "portable");
    assert!(!run(&app, e, &["db", "dump", sql.to_str().unwrap()]).status.success());

    // A second, empty project takes the backup: engine, marker and rows come along.
    let other = tmp.path().join("other");
    std::fs::create_dir_all(&other).unwrap();
    let text = ok(&run(&other, e, &["restore", dest.to_str().unwrap()]));
    assert!(text.contains("postgres") && text.contains("WAL"), "{text}");
    assert_eq!(
        std::fs::read_to_string(other.join("forgebase-data/forgebase.backend")).unwrap(),
        "postgres-managed\n"
    );
    let list = ok(&run(&other, e, &["admin", "list"]));
    assert!(list.contains("first@example.com"), "{list}");
}

#[test]
fn sqlite_projects_have_no_db_dump_and_do_not_restore_a_postgres_backup() {
    let tmp = tempfile::tempdir().unwrap();
    let app = init(tmp.path(), None);
    let out = run(&app, None, &["db", "dump"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("SQLite"));
}
