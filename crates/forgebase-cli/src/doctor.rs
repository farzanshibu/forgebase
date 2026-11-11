//! `forgebase doctor`: read-only diagnostics.
//!
//! Checks configuration, data-directory permissions, database integrity
//! (`PRAGMA integrity_check` and `foreign_key_check`), migration status,
//! storage and index directories, free disk space and the HTTP port, plus
//! (FB-83) the master key, an interrupted restore, snapshots, storage blobs
//! against their metadata (orphans and missing files), vector index files,
//! WAL size and leftover staging files. `--deep` also hashes the newest
//! snapshot. Nothing is created or modified; the database is opened read-only.
//! Exit code 1 when any check fails.
//!
//! On the bundled PostgreSQL (FB-148) it also checks the engine (files
//! against the archive manifest, the `postgres` binary runs), the cluster
//! (`PG_VERSION` against the engine, permissions), `roles.secret` and a
//! postmaster left behind, and runs `amcheck` (`bt_index_check` on every
//! B-tree index) as the cluster's superuser. With no server running it
//! starts the engine under the data-directory lock for the checks and stops
//! it afterwards.

use std::path::Path;
use std::process::ExitCode;

use forgebase_core::{Config, DataLayout, LoadedConfig};
use forgebase_db::Database;
use forgebase_server::DataDirLock;
use forgebase_server::database::{self, Access, BackendMarker, PrimaryDatabase};
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
pub async fn run(g: &GlobalArgs, as_json: bool, deep: bool) -> anyhow::Result<ExitCode> {
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
            check_instance(&mut r, &loaded, deep).await;
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

async fn check_instance(r: &mut Report, loaded: &LoadedConfig, deep: bool) {
    let config = &loaded.config;
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

    // Backend, engine, database integrity and migrations.
    let marker = match database::resolve(config, crate::commands::explicit_backend(loaded), &layout) {
        Ok(m) => Some(m),
        Err(e) => {
            r.push("backend", Level::Fail, e.to_string());
            None
        }
    };
    let initialized = database::is_initialized(&layout);
    if let Some(m) = marker {
        let detail = match crate::commands::sqlite_notice(initialized.then_some(m)) {
            Some(n) => format!("{}; {n}", m.label()),
            None => m.label().to_owned(),
        };
        r.push("backend", Level::Ok, detail);
    }
    if marker == Some(BackendMarker::PostgresManaged) && initialized {
        check_engine(r, config, &layout).await;
    }
    let mut primary: Option<PrimaryDatabase> = None;
    if marker.is_none() {
        r.push("database", Level::Fail, "the backend could not be decided");
        r.push("migrations", Level::Fail, "no database");
    } else if !initialized {
        r.push(
            "database",
            Level::Fail,
            format!("{} holds no database (run `forgebase init`)", root.display()),
        );
        r.push("migrations", Level::Fail, "no database");
    } else {
        match crate::commands::open_attached(loaded, Access::Super).await {
            Ok(p) => {
                check_database(r, &p, &layout).await;
                primary = Some(p);
            }
            Err(e) => {
                r.push("database", Level::Fail, format!("{e:#}"));
                r.push("migrations", Level::Fail, "database cannot be opened");
            }
        }
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
    check_extras(r, config, &layout, primary.as_deref(), deep).await;
    if let Some(p) = primary {
        p.close().await;
    }
    check_port(r, config, &layout).await;
}

/// The bundled engine's files, cluster, passwords and processes.
async fn check_engine(r: &mut Report, config: &Config, layout: &DataLayout) {
    use forgebase_pgengine::{EngineDir, EngineOrigin, MIN_MAJOR, RoleSecrets};
    let el = database::engine_layout(layout);
    let pgdata = el.pgdata();
    let major = std::fs::read_to_string(pgdata.join("PG_VERSION"))
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok());
    match database::engine_root(&config.database, layout) {
        None => r.push(
            "engine",
            Level::Warn,
            format!(
                "no extracted engine in {}; the next start extracts the bundled one",
                el.engines_dir().display()
            ),
        ),
        Some(root) => {
            let extracted = root.starts_with(el.engines_dir());
            let verified = if extracted {
                forgebase_pgengine::archive::verify_extracted(&root)
                    .map(|m| format!(", {} files match the manifest", m.files.len()))
            } else {
                Ok(String::new())
            };
            match (verified, EngineDir::open(&root, EngineOrigin::Dir).await) {
                (Err(e), _) => r.push("engine", Level::Fail, format!("{}: {e}", root.display())),
                (_, Err(e)) => r.push("engine", Level::Fail, e.to_string()),
                (Ok(files), Ok(engine)) => {
                    let v = &engine.version;
                    if v.major < MIN_MAJOR {
                        r.push(
                            "engine",
                            Level::Fail,
                            format!("PostgreSQL {} is older than {MIN_MAJOR}", v.text),
                        );
                    } else if major.is_some_and(|m| m != v.major) {
                        r.push(
                            "engine",
                            Level::Fail,
                            format!(
                                "the cluster was created by PostgreSQL {} and the engine is {}; ForgeBase never upgrades a major on its own",
                                major.unwrap_or_default(),
                                v.text
                            ),
                        );
                    } else {
                        r.push(
                            "engine",
                            Level::Ok,
                            format!("PostgreSQL {} in {}{files}", v.text, root.display()),
                        );
                    }
                }
            }
        }
    }
    match loose_permissions(&pgdata) {
        Some(mode) => r.push(
            "cluster",
            Level::Fail,
            format!(
                "{} has mode {mode:o}; PostgreSQL refuses to start unless it is 0700",
                pgdata.display()
            ),
        ),
        None => r.push(
            "cluster",
            Level::Ok,
            format!(
                "{} (PostgreSQL {}, owner-only)",
                pgdata.display(),
                major.map_or("?".to_owned(), |m| m.to_string())
            ),
        ),
    }
    match RoleSecrets::load(&el.secrets_file()) {
        Ok(Some(_)) => r.push(
            "roles",
            Level::Ok,
            format!("{} present, owner-only", el.secrets_file().display()),
        ),
        Ok(None) => r.push(
            "roles",
            Level::Fail,
            format!(
                "{} is missing: ForgeBase cannot log in to the cluster; restore it from a backup",
                el.secrets_file().display()
            ),
        ),
        Err(e) => r.push("roles", Level::Fail, e.to_string()),
    }
    if DataDirLock::probe(layout).is_none()
        && let Some(pm) = database::postmaster(layout)
    {
        r.push(
            "postmaster",
            Level::Warn,
            format!(
                "a PostgreSQL left behind by a stopped server is running (pid {}); the next start stops it first",
                pm.pid
            ),
        );
    }
}

/// Master key, interrupted restore, snapshots, blobs, vector index files, WAL, staging leftovers.
async fn check_extras(r: &mut Report, config: &Config, layout: &DataLayout, db: Option<&Database>, deep: bool) {
    if !layout.root().is_dir() {
        return;
    }
    check_key(r, layout);
    if forgebase_backup::restore::interrupted(layout) {
        r.push(
            "restore",
            Level::Fail,
            "an earlier restore was interrupted; run `forgebase restore --recover` (the server is stopped)",
        );
    }
    check_snapshots(r, config, layout, deep).await;
    check_pre_migration(r, layout);
    check_staging(r, layout);
    check_index_files(r, layout);
    if let Some(db) = db {
        {
            // PostgreSQL's WAL is bounded by max_wal_size and recycled by checkpoints.
            if let Ok(info) = db.info().await
                && info.backend == forgebase_db::BackendKind::Sqlite
                && info.wal_size_bytes > WAL_WARN_BYTES
            {
                r.push(
                    "wal",
                    Level::Warn,
                    format!(
                        "write-ahead log is {} MiB; a checkpoint runs at clean shutdown (a long-running reader may be holding it)",
                        info.wal_size_bytes / (1024 * 1024)
                    ),
                );
            }
            if config.storage.driver == forgebase_core::StorageDriverKind::Local {
                check_blobs(r, db, layout).await;
            }
            check_two_phase(r, db).await;
        }
    }
}

/// WAL size above which doctor warns.
const WAL_WARN_BYTES: u64 = 256 * 1024 * 1024;
/// Age of the newest snapshot above which doctor warns.
const SNAPSHOT_STALE_DAYS: i64 = 7;

fn check_key(r: &mut Report, layout: &DataLayout) {
    let key = forgebase_backup::backup::master_key_path(layout);
    match std::fs::metadata(&key) {
        Err(_) => r.push(
            "master_key",
            Level::Warn,
            format!(
                "{} is missing: it is created at the first start; on a restored directory restore the original key, or stored secrets stay unreadable",
                key.display()
            ),
        ),
        Ok(m) if m.len() == 0 => r.push("master_key", Level::Fail, format!("{} is empty", key.display())),
        Ok(_) => match loose_permissions(&key) {
            Some(mode) => r.push(
                "master_key",
                Level::Warn,
                format!("{} has mode {mode:o}; run chmod 600", key.display()),
            ),
            None => r.push("master_key", Level::Ok, format!("{} present, owner-only", key.display())),
        },
    }
}

/// The backend a snapshot of this project must come from, `None` when it cannot be told.
fn current_engine(config: &Config, layout: &DataLayout) -> Option<(BackendMarker, Option<u32>)> {
    let marker = database::resolve(config, false, layout).ok()?;
    let major = std::fs::read_to_string(database::engine_layout(layout).pgdata().join("PG_VERSION"))
        .ok()
        .and_then(|t| t.trim().parse().ok());
    Some((marker, major))
}

async fn check_snapshots(r: &mut Report, config: &Config, layout: &DataLayout, deep: bool) {
    let store = forgebase_backup::SnapshotStore::new(layout);
    let all = match store.list() {
        Ok(a) => a,
        Err(e) => {
            r.push(
                "snapshots",
                Level::Warn,
                format!("cannot read {}: {e}", store.dir().display()),
            );
            return;
        }
    };
    let Some(latest) = all.first() else {
        r.push(
            "snapshots",
            Level::Warn,
            "no snapshot yet; run `forgebase backup` (schedule it with cron or a systemd timer)",
        );
        return;
    };
    let age_days =
        time::OffsetDateTime::parse(&latest.manifest.created, &time::format_description::well_known::Rfc3339)
            .map(|t| (time::OffsetDateTime::now_utc() - t).whole_days())
            .ok();
    let mut level = Level::Ok;
    let mut detail = format!(
        "{} snapshot(s); newest {} ({})",
        all.len(),
        latest.id,
        latest.manifest.created
    );
    if age_days.is_some_and(|d| d >= SNAPSHOT_STALE_DAYS) {
        level = Level::Warn;
        detail.push_str(&format!("; older than {SNAPSHOT_STALE_DAYS} days"));
    }
    let engine = latest.manifest.engine_info();
    detail.push_str(&format!("; {}", engine.describe()));
    if let Some((marker, major)) = current_engine(config, layout) {
        let same = match marker {
            BackendMarker::Sqlite => !engine.is_postgres(),
            BackendMarker::PostgresManaged => {
                engine.is_postgres() && engine.method == forgebase_backup::manifest::Method::Basebackup
            }
            BackendMarker::PostgresExternal => {
                engine.is_postgres() && engine.method == forgebase_backup::manifest::Method::Dump
            }
        };
        if !same {
            level = Level::Warn;
            detail.push_str(&format!(
                "; the project is {} now, so a restore of it would be refused (`forgebase db convert`, or take a new backup)",
                marker.describe()
            ));
        } else if let (Some(have), Some(want)) = (major, engine.major)
            && engine.method == forgebase_backup::manifest::Method::Basebackup
            && have != want
        {
            level = Level::Warn;
            detail.push_str(&format!(
                "; it is PostgreSQL {want} but the cluster is {have}: a physical backup restores on the same major only, take a new one"
            ));
        }
    }
    if deep {
        let tools = database::engine_root(&config.database, layout)
            .map(|root| forgebase_backup::PgSource::tools_only(Some(root), String::new()));
        let v = forgebase_backup::verify_with(
            &latest.path,
            &forgebase_backup::VerifyOptions {
                pg: tools,
                tmp: Some(layout.tmp_dir()),
            },
        )
        .await;
        if v.ok {
            detail.push_str(&format!("; verified ({} files)", v.files_checked));
        } else {
            level = Level::Fail;
            detail.push_str(&format!("; verification FAILED: {}", v.problems.join("; ")));
        }
    }
    r.push("snapshots", level, detail);
}

/// Pre-migration snapshots (`backups/pre-migrate-*`): how many, how big the newest is.
fn check_pre_migration(r: &mut Report, layout: &DataLayout) {
    let Ok(rd) = std::fs::read_dir(layout.backups_dir()) else {
        return;
    };
    let mut found: Vec<String> = rd
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("pre-migrate-") && !n.ends_with(".partial"))
        .collect();
    if found.is_empty() {
        return;
    }
    found.sort();
    let newest = found.last().cloned().unwrap_or_default();
    r.push(
        "pre_migration",
        Level::Ok,
        format!(
            "{} pre-migration snapshot(s) in {}; newest {newest} (delete them once the upgrade is verified)",
            found.len(),
            layout.backups_dir().display()
        ),
    );
}

fn check_staging(r: &mut Report, layout: &DataLayout) {
    let mut leftovers = Vec::new();
    for dir in [
        layout.tmp_dir(),
        layout.backups_dir().join("snapshots"),
        layout.backups_dir(),
    ] {
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.starts_with("restore-") || name.starts_with(".pgpass-") || name.ends_with(".partial") {
                    leftovers.push(e.path().display().to_string());
                }
            }
        }
    }
    if !leftovers.is_empty() {
        r.push(
            "staging",
            Level::Warn,
            format!(
                "leftovers of an unfinished backup or restore (safe to delete when stopped): {}",
                leftovers.join(", ")
            ),
        );
    }
}

/// Vector index files: each manifest parses and each `.usearch` file is non-empty.
/// A bad index is derived data: the server rebuilds it, so this only warns.
fn check_index_files(r: &mut Report, layout: &DataLayout) {
    let root = layout.indexes_dir();
    let mut stack = vec![root.clone()];
    let (mut files, mut bad) = (0usize, Vec::new());
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let path = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if path.is_dir() {
                stack.push(path);
            } else if name.ends_with(".manifest.json") {
                files += 1;
                let ok = std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                    .is_some();
                if !ok {
                    bad.push(path.display().to_string());
                }
            } else if name.ends_with(".usearch") {
                files += 1;
                if e.metadata().map(|m| m.len() == 0).unwrap_or(true) {
                    bad.push(path.display().to_string());
                }
            }
        }
    }
    if !root.is_dir() {
        return;
    }
    if bad.is_empty() {
        r.push(
            "index_files",
            Level::Ok,
            format!("{files} vector index file(s) readable"),
        );
    } else {
        r.push(
            "index_files",
            Level::Warn,
            format!(
                "damaged index file(s), rebuilt by the server at startup: {}",
                bad.join(", ")
            ),
        );
    }
}

/// Blobs on disk against `_forgebase_storage_blobs`: files nobody references
/// (orphans, removed by the reconciler) and referenced blobs that are gone.
async fn check_blobs(r: &mut Report, db: &Database, layout: &DataLayout) {
    use forgebase_db::{Backend, Statement};
    let rows = match db
        .fetch_all(&Statement::raw("SELECT sha256, refcount FROM _forgebase_storage_blobs"))
        .await
    {
        Ok(rows) => rows,
        Err(_) => return, // storage tables not created yet
    };
    let mut known: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    for row in &rows.rows {
        if let (Some(sha), Some(rc)) = (row[0].as_str(), row[1].as_i64()) {
            known.insert(sha.to_owned(), rc);
        }
    }
    let root = layout.storage_dir().join("blobs");
    let mut on_disk = std::collections::HashSet::new();
    if let Ok(prefixes) = std::fs::read_dir(&root) {
        for p in prefixes.flatten() {
            if let Ok(files) = std::fs::read_dir(p.path()) {
                for f in files.flatten() {
                    on_disk.insert(f.file_name().to_string_lossy().into_owned());
                }
            }
        }
    }
    let missing = known
        .iter()
        .filter(|(sha, rc)| **rc > 0 && !on_disk.contains(*sha))
        .count();
    let orphans = on_disk.iter().filter(|sha| !known.contains_key(*sha)).count();
    if missing > 0 {
        r.push(
            "storage_blobs",
            Level::Fail,
            format!(
                "{missing} referenced blob(s) are missing from {} (restore from a backup)",
                root.display()
            ),
        );
    } else if orphans > 0 {
        r.push(
            "storage_blobs",
            Level::Warn,
            format!(
                "{orphans} blob file(s) have no metadata; the storage reconciler removes them after its grace period"
            ),
        );
    } else {
        r.push(
            "storage_blobs",
            Level::Ok,
            format!("{} blob(s), all accounted for", known.len()),
        );
    }
}

async fn check_database(r: &mut Report, primary: &PrimaryDatabase, layout: &DataLayout) {
    let path = layout.database_path();
    let db = primary.db();
    let sqlite = primary.marker() == BackendMarker::Sqlite;
    let check_name = if sqlite { "integrity_check" } else { "amcheck" };

    let integrity = db.integrity_check().await;
    let fk = db.foreign_key_violations().await;
    let info = db.info().await;
    match (integrity, fk, info) {
        (Ok(problems), _, _) if !problems.is_empty() => {
            let shown: Vec<_> = problems.iter().take(5).cloned().collect();
            r.push(
                "database",
                Level::Fail,
                format!("{check_name} found {} problem(s): {}", problems.len(), shown.join("; ")),
            );
        }
        (Err(e), _, _) => r.push("database", Level::Fail, format!("{check_name} failed: {e}")),
        (Ok(_), Ok(n), _) if n > 0 => r.push("database", Level::Fail, format!("{n} foreign-key violation(s)")),
        (Ok(_), Err(e), _) => r.push("database", Level::Fail, format!("foreign_key_check failed: {e}")),
        (Ok(_), Ok(_), Err(e)) => r.push("database", Level::Fail, e.to_string()),
        (Ok(_), Ok(_), Ok(info)) if !sqlite => r.push(
            "database",
            Level::Ok,
            format!(
                "{} ok ({} bytes, PostgreSQL {}, amcheck and foreign keys clean)",
                crate::commands::database_line(primary, layout),
                info.size_bytes,
                info.server_version
            ),
        ),
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

/// In-doubt two-phase schema transactions on control (ADR-0029 §3.7,
/// FB-173): prepared parts older than the recovery grace and decided
/// transactions not finished on every set. Shards are reached by the
/// server (it resolves both at start and every minute); doctor reads
/// control, where the journal lives. Fails when the topology has shards but
/// control cannot prepare transactions.
async fn check_two_phase(r: &mut Report, db: &Database) {
    use forgebase_db::twophase::{self, Participant};
    if db.kind() != forgebase_db::BackendKind::Postgres {
        return;
    }
    if let Ok(topology) = forgebase_db::topology::load(db).await
        && topology.is_sharded()
        && db.max_prepared_transactions().await.unwrap_or(0) <= 0
    {
        r.push(
            "two_phase",
            Level::Fail,
            "the cluster has shards but control has max_prepared_transactions = 0; set it above 0 (16) and restart",
        );
        return;
    }
    let nodes = [Participant::new(forgebase_db::topology::CONTROL_SET_ID, 0, db.clone())];
    match twophase::in_doubt(db, &nodes, twophase::RECOVERY_GRACE).await {
        Ok(list) if list.is_empty() => r.push("two_phase", Level::Ok, "no in-doubt schema transactions"),
        Ok(list) => r.push("two_phase", Level::Warn, twophase::describe_in_doubt(&list)),
        Err(e) => r.push(
            "two_phase",
            Level::Warn,
            format!("could not read the transaction journal: {e}"),
        ),
    }
}
