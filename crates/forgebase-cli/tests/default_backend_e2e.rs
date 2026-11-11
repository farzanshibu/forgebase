//! Black-box tests of the default backend (FB-177, ADR-0029 §6): the
//! standard release (`bundled-postgres`) initialises PostgreSQL by default,
//! the `-lite` build initialises SQLite and says so, and an existing SQLite
//! project never switches on its own: `serve` keeps it, `status` and
//! `doctor` show the one-line notice (standard release only).
//!
//! Which half runs follows the build: `cargo test -p forgebase-cli` is the
//! lite half (no embedded engine); `--features bundled-postgres` with
//! `FORGEBASE_PG_ENGINE_ARCHIVE` is the standard half.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Output, Stdio};

use serde_json::Value;

const BUNDLED: bool = cfg!(feature = "bundled-postgres");
const NOTICE: &str = "forgebase db convert --to postgres";

fn forgebase(cwd: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_forgebase"));
    cmd.current_dir(cwd);
    for (k, _) in std::env::vars_os() {
        // No engine directory either: the lite half must not find an engine,
        // and the standard half runs on the embedded one.
        if k.to_string_lossy().starts_with("FORGEBASE_") {
            cmd.env_remove(k);
        }
    }
    cmd.env("NO_COLOR", "1").env("HOME", cwd);
    cmd
}

fn run(cwd: &Path, args: &[&str]) -> Output {
    forgebase(cwd).args(args).stdin(Stdio::null()).output().unwrap()
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
    tempfile::Builder::new().prefix("fb177-").tempdir_in(base).unwrap()
}

fn marker(app: &Path) -> String {
    std::fs::read_to_string(app.join("forgebase-data/forgebase.backend")).unwrap()
}

/// The build says what its default is: `--version` for the lite build,
/// `version --json` for both.
#[test]
fn the_build_names_its_default() {
    let tmp = tempdir();
    let v = json(&run(tmp.path(), &["version", "--json"]));
    assert_eq!(v["bundled_postgres"], BUNDLED);
    assert_eq!(v["default_backend"], if BUNDLED { "postgres" } else { "sqlite" });
    let text = ok(&run(tmp.path(), &["--version"]));
    assert_eq!(text.contains("lite: SQLite only"), !BUNDLED, "{text}");
}

/// `init` with no flag (no terminal, so no question) takes the build's default.
#[test]
fn init_without_a_flag_takes_the_builds_default() {
    let tmp = tempdir();
    let text = ok(&run(tmp.path(), &["init", "app", "--no-admin"]));
    let app = tmp.path().join("app");
    let toml = std::fs::read_to_string(app.join("forgebase.toml")).unwrap();
    if BUNDLED {
        assert!(toml.contains("\nbackend = \"postgres\""), "{toml}");
        assert_eq!(marker(&app), "postgres-managed\n");
        assert!(text.contains("PostgreSQL") && text.contains("(bundled)"), "{text}");
        assert!(!app.join("forgebase-data/forgebase.db").exists());
    } else {
        assert!(toml.contains("\nbackend = \"sqlite\""), "{toml}");
        assert_eq!(marker(&app), "sqlite\n");
        assert!(text.contains("forgebase.db"), "{text}");
        // Never a silent engine: the lite build has no PostgreSQL to start.
        assert!(!app.join("forgebase-data/postgres").exists());
    }
    // The closing message names the next step.
    assert!(text.contains("Next: cd app && forgebase serve"), "{text}");
}

/// `--database sqlite` gives a SQLite project in either build, and
/// `--database postgres` in the lite build is refused with the way out.
#[test]
fn init_database_flag_decides() {
    let tmp = tempdir();
    let text = ok(&run(tmp.path(), &["init", "s", "--database", "sqlite", "--no-admin"]));
    assert!(text.contains("forgebase.db"), "{text}");
    assert_eq!(marker(&tmp.path().join("s")), "sqlite\n");

    if !BUNDLED {
        let out = run(tmp.path(), &["init", "p", "--database", "postgres", "--no-admin"]);
        assert!(!out.status.success());
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("no bundled PostgreSQL"), "{err}");
        assert!(err.contains("--database sqlite"), "{err}");
        assert!(
            !tmp.path().join("p/forgebase.toml").exists(),
            "a refused init writes nothing"
        );
    }
}

/// An existing SQLite project (a `forgebase.db`, no marker: made before the
/// marker existed) stays SQLite: `serve` opens it, `status` and `doctor` say
/// so and, where PostgreSQL is the default, show the one-line notice.
#[test]
fn an_existing_sqlite_project_is_left_alone() {
    let tmp = tempdir();
    ok(&run(tmp.path(), &["init", "app", "--database", "sqlite", "--no-admin"]));
    let app = tmp.path().join("app");
    // A project from before the marker and before the config named a backend.
    std::fs::remove_file(app.join("forgebase-data/forgebase.backend")).unwrap();
    std::fs::remove_file(app.join("forgebase.toml")).unwrap();

    let s = json(&run(&app, &["status", "--json"]));
    assert_eq!(s["database"]["backend"], "sqlite", "{s}");
    assert_eq!(s["database"]["exists"], true);
    assert_eq!(
        s["database"]["notice"].as_str().is_some_and(|n| n.contains(NOTICE)),
        BUNDLED,
        "{s}"
    );
    let text = ok(&run(&app, &["status"]));
    assert_eq!(text.contains(NOTICE), BUNDLED, "{text}");

    let d = json(&run(&app, &["doctor", "--json"]));
    let backend = d["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "backend")
        .unwrap_or_else(|| panic!("{d}"));
    assert_eq!(backend["status"], "ok", "{d}");
    let detail = backend["detail"].as_str().unwrap();
    assert!(detail.contains("SQLite") || detail.contains("sqlite"), "{detail}");
    assert_eq!(detail.contains(NOTICE), BUNDLED, "{detail}");

    // serve: still SQLite, same file.
    let mut child = forgebase(&app)
        .args(["serve", "--port", "0"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap());
    let mut database = None;
    let mut line = String::new();
    while lines.read_line(&mut line).unwrap() > 0 {
        if let Some(rest) = line.trim().strip_prefix("database:") {
            database = Some(rest.trim().to_owned());
            break;
        }
        line.clear();
    }
    child.kill().unwrap();
    child.wait().unwrap();
    let database = database.expect("serve printed no database line");
    assert!(
        database.starts_with("SQLite ") && database.ends_with("forgebase.db"),
        "{database}"
    );
    assert!(!app.join("forgebase-data/postgres").exists(), "no engine was created");
}
