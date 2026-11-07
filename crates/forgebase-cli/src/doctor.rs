//! `forgebase doctor`: read-only diagnostics.
//!
//! Checks configuration, data-directory permissions, database integrity
//! (`PRAGMA integrity_check` and `foreign_key_check`), migration status,
//! storage and index directories, free disk space and the HTTP port.
//! Nothing is created or modified; the database is opened read-only.
//! Exit code 1 when any check fails.

use std::path::Path;
use std::process::ExitCode;

use forgebase_core::{Config, DataLayout};
use forgebase_db::{Database, DbOptions};
use forgebase_server::DataDirLock;
use serde::Serialize;

use crate::GlobalArgs;
use crate::probe;

/// Free space below which the disk check fails.
const DISK_FAIL_BYTES: u64 = 100 * 1024 * 1024;
/// Free space below which the disk check warns.
const DISK_WARN_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Level {
    Ok,
    Warn,
    Fail,
}

#[derive(Debug, Serialize)]
struct Check {
    name: &'static str,
    status: Level,
    detail: String,
}

#[derive(Default)]
struct Report {
    checks: Vec<Check>,
}

impl Report {
    fn push(&mut self, name: &'static str, status: Level, detail: impl Into<String>) {
        self.checks.push(Check {
            name,
            status,
            detail: detail.into(),
        });
    }
    fn failed(&self) -> usize {
        self.checks.iter().filter(|c| c.status == Level::Fail).count()
    }
    fn warned(&self) -> usize {
        self.checks.iter().filter(|c| c.status == Level::Warn).count()
    }
}

/// Runs every check and prints the report.
pub async fn run(g: &GlobalArgs, as_json: bool) -> anyhow::Result<ExitCode> {
    let level = g.log_level.as_deref().unwrap_or("error");
    forgebase_observability::init_logging(level, forgebase_core::LogFormat::Pretty)?;

    let mut r = Report::default();
    match g.load() {
        Ok(loaded) => {
            let source = loaded
                .file
                .as_ref()
                .map_or("no config file; defaults, environment and flags".to_owned(), |f| {
                    format!("loaded {}", f.display())
                });
            r.push("config", Level::Ok, source);
            check_instance(&mut r, &loaded.config).await;
        }
        Err(e) => r.push("config", Level::Fail, format!("{e:#}")),
    }

    if as_json {
        let out = serde_json::json!({
            "ok": r.failed() == 0,
            "failed": r.failed(),
            "warnings": r.warned(),
            "checks": r.checks,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        for c in &r.checks {
            let tag = match c.status {
                Level::Ok => "[ok]  ",
                Level::Warn => "[warn]",
                Level::Fail => "[FAIL]",
            };
            println!("{tag} {:<11} {}", c.name, c.detail);
        }
        let passed = r.checks.len() - r.failed() - r.warned();
        println!(
            "\ndoctor: {passed} ok, {} warning(s), {} failed",
            r.warned(),
            r.failed()
        );
    }
    Ok(if r.failed() == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

async fn check_instance(r: &mut Report, config: &Config) {
    let layout = DataLayout::new(&config.data_dir);
    let root = layout.root();

    // Data directory and permissions.
    if !root.is_dir() {
        r.push(
            "data_dir",
            Level::Fail,
            format!("{} does not exist (run `forgebase init`)", root.display()),
        );
    } else {
        match writable(root) {
            Err(e) => r.push(
                "data_dir",
                Level::Fail,
                format!("{} is not writable: {e}", root.display()),
            ),
            Ok(()) => match loose_permissions(root) {
                Some(mode) => r.push(
                    "data_dir",
                    Level::Warn,
                    format!(
                        "{} is accessible by group/others (mode {mode:o}); run chmod 700",
                        root.display()
                    ),
                ),
                None => r.push(
                    "data_dir",
                    Level::Ok,
                    format!("{} (writable, owner-only)", root.display()),
                ),
            },
        }
    }

    // Database integrity and migrations.
    let db_path = layout.database_path();
    if !db_path.is_file() {
        r.push(
            "database",
            Level::Fail,
            format!("{} is missing (run `forgebase init`)", db_path.display()),
        );
        r.push("migrations", Level::Fail, "no database");
    } else {
        check_database(r, config, &layout).await;
    }

    // Storage (canonical blobs) and indexes (derived).
    let storage = layout.storage_dir();
    if !storage.is_dir() {
        r.push(
            "storage",
            Level::Fail,
            format!("{} is missing (run `forgebase init`)", storage.display()),
        );
    } else if let Err(e) = writable(&storage) {
        r.push(
            "storage",
            Level::Fail,
            format!("{} is not writable: {e}", storage.display()),
        );
    } else {
        r.push("storage", Level::Ok, format!("{} writable", storage.display()));
    }
    let indexes = layout.indexes_dir();
    if indexes.is_dir() {
        r.push(
            "indexes",
            Level::Ok,
            format!("{} present (derived, rebuildable)", indexes.display()),
        );
    } else {
        r.push(
            "indexes",
            Level::Warn,
            format!("{} is missing; it is recreated at startup", indexes.display()),
        );
    }

    check_disk(r, root);
    check_port(r, config, &layout).await;
}

async fn check_database(r: &mut Report, config: &Config, layout: &DataLayout) {
    let path = layout.database_path();
    let opts = DbOptions::from_config(&config.database, &path).read_only();
    let db = match Database::open(opts).await {
        Ok(db) => db,
        Err(e) => {
            r.push("database", Level::Fail, e.to_string());
            r.push("migrations", Level::Fail, "database cannot be opened");
            return;
        }
    };

    let integrity = db.integrity_check().await;
    let fk = db.foreign_key_violations().await;
    let info = db.info().await;
    match (integrity, fk, info) {
        (Ok(problems), _, _) if !problems.is_empty() => {
            let shown: Vec<_> = problems.iter().take(5).cloned().collect();
            r.push(
                "database",
                Level::Fail,
                format!(
                    "integrity_check found {} problem(s): {}",
                    problems.len(),
                    shown.join("; ")
                ),
            );
        }
        (Err(e), _, _) => r.push("database", Level::Fail, format!("integrity_check failed: {e}")),
        (Ok(_), Ok(n), _) if n > 0 => r.push("database", Level::Fail, format!("{n} foreign-key violation(s)")),
        (Ok(_), Err(e), _) => r.push("database", Level::Fail, format!("foreign_key_check failed: {e}")),
        (Ok(_), Ok(_), Err(e)) => r.push("database", Level::Fail, e.to_string()),
        (Ok(_), Ok(_), Ok(info)) => {
            let detail = format!(
                "{} ok ({} bytes, wal {} bytes, sqlite {})",
                path.display(),
                info.size_bytes,
                info.wal_size_bytes,
                info.sqlite_version
            );
            if info.journal_mode != "wal" {
                r.push(
                    "database",
                    Level::Warn,
                    format!("{detail}; journal_mode is {}, expected wal", info.journal_mode),
                );
            } else if let Some(mode) = loose_permissions(&path) {
                r.push(
                    "database",
                    Level::Warn,
                    format!("{detail}; file mode {mode:o} lets group/others read it"),
                );
            } else {
                r.push("database", Level::Ok, detail);
            }
        }
    }

    match db.migration_status().await {
        Err(e) => r.push("migrations", Level::Fail, e.to_string()),
        Ok(s) if !s.problems.is_empty() => r.push("migrations", Level::Fail, format!("{:?}", s.problems)),
        Ok(s) if !s.pending.is_empty() => r.push(
            "migrations",
            Level::Warn,
            format!(
                "schema v{} of v{}: {} pending (run `forgebase migrate`; `serve` applies them at startup)",
                s.current_version,
                s.latest_version,
                s.pending.len()
            ),
        ),
        Ok(s) => r.push(
            "migrations",
            Level::Ok,
            format!("schema v{} (latest)", s.current_version),
        ),
    }
    db.close().await;
}

fn check_disk(r: &mut Report, root: &Path) {
    let mut dir = root;
    while !dir.exists() {
        match dir.parent() {
            Some(p) => dir = p,
            None => break,
        }
    }
    match fs4::available_space(dir) {
        Ok(free) => {
            let gib = free as f64 / (1024.0 * 1024.0 * 1024.0);
            let detail = format!("{gib:.1} GiB free on the data directory's filesystem");
            let level = if free < DISK_FAIL_BYTES {
                Level::Fail
            } else if free < DISK_WARN_BYTES {
                Level::Warn
            } else {
                Level::Ok
            };
            r.push("disk", level, detail);
        }
        Err(e) => r.push("disk", Level::Warn, format!("cannot read free space: {e}")),
    }
}

async fn check_port(r: &mut Report, config: &Config, layout: &DataLayout) {
    let bind = format!("{}:{}", config.server.host, config.server.port);
    if let Some(owner) = DataDirLock::probe(layout) {
        let detail = match owner {
            Some(o) => format!("this data directory is served by pid {} on {}", o.pid, o.addr),
            None => "this data directory is in use by a running ForgeBase".to_owned(),
        };
        r.push("port", Level::Ok, detail);
        return;
    }
    if config.server.port == 0 {
        r.push("port", Level::Ok, "port 0: a free port is chosen at startup");
        return;
    }
    match tokio::net::TcpListener::bind((config.server.host.as_str(), config.server.port)).await {
        Ok(listener) => {
            drop(listener);
            r.push("port", Level::Ok, format!("{bind} is free"));
        }
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            let dial = probe::dial_addr(&config.server.host, config.server.port);
            if probe::is_forgebase(&dial).await {
                r.push(
                    "port",
                    Level::Warn,
                    format!("{bind} is used by another ForgeBase instance"),
                );
            } else {
                r.push("port", Level::Fail, format!("{bind} is in use by another process"));
            }
        }
        Err(e) => r.push("port", Level::Fail, format!("cannot bind {bind}: {e}")),
    }
}

fn writable(dir: &Path) -> std::io::Result<()> {
    let probe = dir.join(format!(".doctor-{}", std::process::id()));
    std::fs::write(&probe, b"ok")?;
    std::fs::remove_file(&probe)
}

#[cfg(unix)]
fn loose_permissions(path: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path).ok()?.permissions().mode() & 0o777;
    (mode & 0o077 != 0).then_some(mode)
}

#[cfg(not(unix))]
fn loose_permissions(_path: &Path) -> Option<u32> {
    None
}
