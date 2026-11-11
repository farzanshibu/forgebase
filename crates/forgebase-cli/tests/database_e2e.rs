//! Black-box tests of the database commands of the `forgebase` binary
//! (FB-148, ADR-0028 §6, ADR-0029 §6): `init --database`, `--database-url`,
//! the backend marker, `status`, `doctor`, `migrate --status`, `admin` and
//! `db status` / `db psql` on the bundled PostgreSQL.
//!
//! The PostgreSQL tests run with `FORGEBASE_TEST_BACKEND=postgres` and an
//! engine in `FORGEBASE_PG_ENGINE_DIR`, and skip otherwise.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::Value;

fn engine_dir() -> Option<PathBuf> {
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
    tempfile::Builder::new().prefix("fb148c-").tempdir_in(base).unwrap()
}

#[test]
fn sqlite_init_writes_the_backend_and_refuses_to_switch() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run(tmp.path(), None, &["init", "app", "--database", "sqlite", "--no-admin"]);
    let text = ok(&out);
    assert!(text.contains("forgebase.db"), "{text}");
    let app = tmp.path().join("app");
    let toml = std::fs::read_to_string(app.join("forgebase.toml")).unwrap();
    assert!(toml.contains("\nbackend = \"sqlite\""), "{toml}");
    assert_eq!(
        std::fs::read_to_string(app.join("forgebase-data/forgebase.backend")).unwrap(),
        "sqlite\n"
    );

    // init again keeps it; asking for the other engine is refused.
    ok(&run(tmp.path(), None, &["init", "app", "--no-admin"]));
    let out = run(
        tmp.path(),
        None,
        &["init", "app", "--database", "postgres", "--no-admin"],
    );
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("already holds a sqlite database"), "{err}");

    // An explicit setting that contradicts the marker stops every command.
    let out = forgebase(&app, None)
        .args(["status"])
        .env("FORGEBASE_DATABASE_BACKEND", "postgres")
        .output()
        .unwrap();
    let stdout = ok(&out);
    assert!(stdout.contains("forgebase db convert --to postgres"), "{stdout}");

    let s = json(&run(&app, None, &["status", "--json"]));
    assert_eq!(s["database"]["backend"], "sqlite");
    assert_eq!(s["database"]["exists"], true);
    let d = json(&run(&app, None, &["db", "status", "--json"]));
    assert_eq!(d["backend"], "sqlite");
    assert_eq!(d["marker"], "sqlite");
    let out = run(&app, None, &["db", "psql", "-c", "select 1"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("sqlite3"));
    // The unknown-variable guard lets the test and engine variables through.
    let out = forgebase(&app, None)
        .args(["status"])
        .env("FORGEBASE_TEST_BACKEND", "postgres")
        .env("FORGEBASE_PG_ENGINE_DIR", "/nowhere")
        .output()
        .unwrap();
    ok(&out);
}

#[test]
fn database_url_goes_to_dotenv_not_the_config() {
    let tmp = tempfile::tempdir().unwrap();
    // Nothing listens there: init fails after writing the files, quickly.
    let url = "postgres://forgebase:s3cret@127.0.0.1:1/forgebase";
    let out = run(tmp.path(), None, &["init", "app", "--database-url", url, "--no-admin"]);
    assert!(!out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    let app = tmp.path().join("app");
    let toml = std::fs::read_to_string(app.join("forgebase.toml")).unwrap();
    assert!(!toml.contains("s3cret"), "{toml}");
    assert!(toml.contains("\nmode = \"external\""), "{toml}");
    let env = std::fs::read_to_string(app.join(".env")).unwrap();
    assert!(env.contains("FORGEBASE_DATABASE_URL='postgres://forgebase:s3cret@127.0.0.1:1/forgebase'"));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!err.contains("s3cret"), "the password stays out of messages: {err}");
}

#[test]
fn version_names_the_build() {
    let tmp = tempfile::tempdir().unwrap();
    let v = json(&run(tmp.path(), None, &["version", "--json"]));
    let bundled = v["bundled_postgres"].as_bool().unwrap();
    assert_eq!(v["default_backend"], if bundled { "postgres" } else { "sqlite" });
    let text = ok(&run(tmp.path(), None, &["--version"]));
    assert_eq!(text.contains("lite"), !bundled, "{text}");
}

#[test]
fn bundled_postgres_project_lifecycle() {
    let Some(engine) = engine_dir() else {
        eprintln!("skipped: FORGEBASE_TEST_BACKEND=postgres and FORGEBASE_PG_ENGINE_DIR are needed");
        return;
    };
    let tmp = tempdir();
    let e = Some(engine.as_path());
    let text = ok(&run(
        tmp.path(),
        e,
        &["init", "app", "--database", "postgres", "--no-admin"],
    ));
    assert!(text.contains("PostgreSQL") && text.contains("(bundled)"), "{text}");
    let app = tmp.path().join("app");
    let data = app.join("forgebase-data");
    assert_eq!(
        std::fs::read_to_string(data.join("forgebase.backend")).unwrap(),
        "postgres-managed\n"
    );
    assert!(data.join("postgres/data/PG_VERSION").is_file());
    assert!(
        !data.join("postgres/data/postmaster.pid").exists(),
        "init stops the engine"
    );
    assert!(!data.join("forgebase.db").exists());

    let s = json(&run(&app, e, &["status", "--json"]));
    assert_eq!(s["database"]["backend"], "postgres");
    assert_eq!(s["database"]["mode"], "managed");
    assert_eq!(s["database"]["engine_started_by"], "this command");
    assert_eq!(s["database"]["schema_version"], s["database"]["latest_schema_version"]);
    assert_eq!(s["setup_required"], true);
    assert!(
        !data.join("postgres/data/postmaster.pid").exists(),
        "status stops what it started"
    );

    let text = ok(&run(&app, e, &["migrate", "--status"]));
    assert!(text.contains("(up to date)"), "{text}");

    let mut admin = forgebase(&app, e)
        .args(["admin", "create", "--email", "root@example.com", "--password-stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        admin.stdin.take().unwrap().write_all(b"a long password 1\n").unwrap();
    }
    ok(&admin.wait_with_output().unwrap());
    let list = ok(&run(&app, e, &["admin", "list"]));
    assert!(list.contains("root@example.com"), "{list}");

    let d = json(&run(&app, e, &["doctor", "--json"]));
    let checks = d["checks"].as_array().unwrap();
    let check = |name: &str| {
        checks
            .iter()
            .find(|c| c["name"] == name)
            .cloned()
            .unwrap_or(Value::Null)
    };
    for name in ["backend", "engine", "cluster", "roles", "database", "migrations"] {
        assert_eq!(check(name)["status"], "ok", "{name}: {d}");
    }
    assert!(check("database")["detail"].as_str().unwrap().contains("amcheck"), "{d}");

    let out = run(&app, e, &["db", "psql", "-tA", "-c", "select current_user || ':' || 1"]);
    assert_eq!(ok(&out).trim(), "forgebase_server:1");
    let out = run(
        &app,
        e,
        &["db", "psql", "--read-only", "-tA", "-c", "select current_user"],
    );
    assert_eq!(ok(&out).trim(), "forgebase_reader");
    let out = run(&app, e, &["db", "psql", "--read-only", "-c", "create table x (a int)"]);
    assert!(!out.status.success(), "the reader cannot write");
    assert!(
        std::fs::read_dir(data.join("tmp")).unwrap().next().is_none(),
        "the passfile is removed"
    );

    let st = json(&run(&app, e, &["db", "status", "--json"]));
    assert_eq!(st["backend"], "postgres");
    assert_eq!(st["engine"]["running"], false);
    assert_eq!(st["listen"], Value::Null);
    assert_eq!(st["extensions"].as_array().unwrap().len(), 5);
    assert!(st["cluster_bytes_on_disk"].as_u64().unwrap() > 1_000_000);

    // An explicit SQLite setting is refused on this directory.
    let out = forgebase(&app, e)
        .args(["migrate"])
        .env("FORGEBASE_DATABASE_BACKEND", "sqlite")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("holds a postgres database"), "{err}");
}
