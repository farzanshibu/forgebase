//! Black-box tests of `forgebase db convert` (FB-151, ADR-0028 §6.4). The
//! round trip needs `FORGEBASE_TEST_BACKEND=postgres` and
//! `FORGEBASE_PG_ENGINE_DIR`; the refusals run everywhere.
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

fn fails(out: &Output) -> String {
    assert!(
        !out.status.success(),
        "expected a failure: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn json(out: &Output) -> Value {
    serde_json::from_str(&ok(out)).unwrap()
}

fn tempdir() -> tempfile::TempDir {
    // Short paths: the engine's Unix socket path is limited to 103 bytes.
    let base = std::env::var_os("FORGEBASE_TEST_PG_TMP").unwrap_or_else(|| std::env::temp_dir().into_os_string());
    tempfile::Builder::new().prefix("fb151c-").tempdir_in(base).unwrap()
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

fn marker(data: &Path) -> String {
    std::fs::read_to_string(data.join("forgebase.backend"))
        .unwrap()
        .trim()
        .to_owned()
}

#[test]
fn convert_refuses_the_same_backend_and_a_missing_project() {
    let tmp = tempdir();
    let empty = run(tmp.path(), None, &["db", "convert", "--to", "postgres"]);
    assert!(
        fails(&empty).contains("no database"),
        "{}",
        String::from_utf8_lossy(&empty.stderr)
    );
    ok(&run(
        tmp.path(),
        None,
        &["init", "app", "--database", "sqlite", "--no-admin"],
    ));
    let app = tmp.path().join("app");
    let same = run(&app, None, &["db", "convert", "--to", "sqlite"]);
    assert!(fails(&same).contains("already holds a sqlite database"));
    if pg().is_none() && std::env::var_os("FORGEBASE_PG_ENGINE_DIR").is_none() && !cfg!(feature = "bundled-postgres") {
        let lite = run(&app, None, &["db", "convert", "--to", "postgres"]);
        assert!(fails(&lite).contains("no bundled PostgreSQL"));
        // Nothing changed.
        assert_eq!(marker(&app.join("forgebase-data")), "sqlite");
        assert!(app.join("forgebase-data/forgebase.db").is_file());
    }
}

#[test]
fn convert_round_trip_on_the_bundled_engine() {
    let Some(e) = pg() else {
        eprintln!("skipped: FORGEBASE_TEST_BACKEND=postgres and FORGEBASE_PG_ENGINE_DIR are needed");
        return;
    };
    let e = Some(e.as_path());
    let tmp = tempdir();
    ok(&run(
        tmp.path(),
        e,
        &["init", "app", "--database", "sqlite", "--no-admin"],
    ));
    let app = tmp.path().join("app");
    let data = app.join("forgebase-data");
    admin(&app, e, "first@example.com");

    let to_pg = json(&run(&app, e, &["db", "convert", "--to", "postgres", "--json"]));
    assert_eq!(to_pg["to"], "postgres-managed", "{to_pg}");
    let tables = to_pg["report"]["tables"].as_array().unwrap();
    assert!(
        tables
            .iter()
            .any(|t| t["table"] == "_forgebase_admins" && t["rows"] == 1),
        "{to_pg}"
    );
    assert_eq!(marker(&data), "postgres-managed");
    assert!(data.join("forgebase.db.pre-postgres").is_file(), "the old file is kept");
    assert!(!data.join("forgebase.db").exists());
    let toml = std::fs::read_to_string(app.join("forgebase.toml")).unwrap();
    assert!(toml.contains("backend = \"postgres\""), "{toml}");

    let status = json(&run(&app, e, &["status", "--json"]));
    assert_eq!(status["database"]["backend"], "postgres", "{status}");
    let admins = ok(&run(&app, e, &["admin", "list"]));
    assert!(admins.contains("first@example.com"), "{admins}");
    let snaps = json(&run(&app, e, &["backup", "list", "--json"]));
    assert_eq!(
        snaps["snapshots"].as_array().unwrap().len(),
        1,
        "a snapshot before converting"
    );

    let back = json(&run(&app, e, &["db", "convert", "--to", "sqlite", "--json"]));
    assert_eq!(back["to"], "sqlite", "{back}");
    assert_eq!(marker(&data), "sqlite");
    assert!(data.join("forgebase.db").is_file());
    assert!(
        data.join("postgres/data.pre-sqlite/PG_VERSION").is_file(),
        "the old cluster is kept"
    );
    let toml = std::fs::read_to_string(app.join("forgebase.toml")).unwrap();
    assert!(toml.contains("backend = \"sqlite\""), "{toml}");
    let admins = ok(&run(&app, e, &["admin", "list"]));
    assert!(admins.contains("first@example.com"), "{admins}");
    // Equal hashes both ways.
    let a: Vec<&Value> = to_pg["report"]["tables"].as_array().unwrap().iter().collect();
    let b: Vec<&Value> = back["report"]["tables"].as_array().unwrap().iter().collect();
    assert_eq!(a.len(), b.len());
    for (x, y) in a.iter().zip(&b) {
        assert_eq!(x["sha256"], y["sha256"], "{} differs", x["table"]);
    }
    let d = json(&run(&app, e, &["doctor", "--json"]));
    assert_ne!(d["ok"], false, "doctor after the round trip: {d}");
}
