//! `forgebase backup drill`: proves that a backup restores (ADR-0041).
//!
//! The drill never touches the live data directory. It
//!
//! 1. restores the backup into a scratch directory (verified: manifest, every
//!    hash, `pg_verifybackup` on PostgreSQL),
//! 2. starts a **quiescent** server on it (`forgebase_server::start_quiescent`:
//!    a loopback port, no jobs, cron, push delivery, webhooks, alerts or TLS
//!    renewal),
//! 3. runs the checks: `doctor`, schema version, record and storage counts
//!    against the manifest, a sample read of records and of storage blobs, the
//!    master key, the sealed `tls/` state,
//! 4. stops the server and deletes the scratch directory (unless `--keep`),
//! 5. reports the measured recovery time (RTO: restore, start and checks) and
//!    the recovery point (RPO: the backup's age), records the result in the
//!    history (`<data_dir>/backups/drills/`) and exits non-zero on any failure.
//!
//! Supported: SQLite backups and physical backups of the bundled PostgreSQL.
//! A logical (`pg_dump`) backup of an external server needs a scratch server
//! to load into, which the drill does not own: it is refused with the manual
//! procedure (docs/operations/disaster-recovery.md).

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::Args;
use forgebase_backup::drill::{
    CheckStatus, DrillCheck, DrillCounts, DrillOutcome, DrillPhases, DrillReport, DrillStore,
};
use forgebase_backup::manifest::{FileKind, Manifest, Method};
use forgebase_backup::restore::{self, RestoreOptions};
use forgebase_backup::{BackupError, SnapshotStore, backup, stats, topology};
use forgebase_core::config::tls_config::TlsConfig;
use forgebase_core::{Config, ConfigOrigin, ConfigSources, DataLayout, LoadedConfig};
use forgebase_data::{AccessContext, DataService, DefaultAuthorizer, ListQuery};
use forgebase_server::database;

use crate::dataops;

/// Records read per collection unless `--sample` says otherwise.
const DEFAULT_SAMPLE: u32 = 5;
/// Blob files read and hashed at most.
const BLOB_SAMPLE: i64 = 20;
/// Scratch directories left by a killed drill are removed after this long.
const STALE_AFTER: Duration = Duration::from_secs(6 * 3600);
const MARKER: &str = ".forgebase-drill";

/// Arguments of `forgebase backup drill`.
#[derive(Debug, Args)]
pub struct DrillArgs {
    /// Backup directory, snapshot id, `latest`, or an RFC 3339 time (the
    /// newest snapshot taken at or before it) [default: latest].
    target: Option<String>,
    /// Print the report as JSON.
    #[arg(long)]
    json: bool,
    /// Keep the scratch directory (its path is in the report) instead of deleting it.
    #[arg(long)]
    keep: bool,
    /// Records read back per collection.
    #[arg(long, value_name = "N", default_value_t = DEFAULT_SAMPLE)]
    sample: u32,
    /// Where the scratch directory is made [default: `<data dir>/tmp`]. It
    /// needs about the size of the backup free.
    #[arg(long, value_name = "DIR")]
    work_dir: Option<PathBuf>,
    /// The auth master key to restore with when the backup does not contain
    /// it: point it at your escrow copy to prove that the escrow works
    /// [default: the live data directory's key].
    #[arg(long, value_name = "FILE")]
    master_key: Option<PathBuf>,
    /// Do not record the result in the drill history.
    #[arg(long)]
    no_record: bool,
    /// Who runs the drill, for the history (`cli`, `api` or `schedule`).
    #[arg(long, value_name = "WHO", default_value = "cli", hide = true)]
    trigger: String,
}

/// Collects the checks of one drill.
struct Checks(Vec<DrillCheck>);

impl Checks {
    fn push(&mut self, name: &str, status: CheckStatus, detail: impl Into<String>) {
        self.0.push(DrillCheck {
            name: name.to_owned(),
            status,
            detail: detail.into(),
        });
    }
    fn ok(&mut self, name: &str, detail: impl Into<String>) {
        self.push(name, CheckStatus::Ok, detail);
    }
    fn warn(&mut self, name: &str, detail: impl Into<String>) {
        self.push(name, CheckStatus::Warn, detail);
    }
    fn fail(&mut self, name: &str, detail: impl Into<String>) {
        self.push(name, CheckStatus::Fail, detail);
    }
    fn skip(&mut self, name: &str, detail: impl Into<String>) {
        self.push(name, CheckStatus::Skip, detail);
    }
    fn failed(&self) -> bool {
        self.0.iter().any(|c| c.status == CheckStatus::Fail)
    }
}

fn secs_since(rfc3339: &str) -> i64 {
    use time::format_description::well_known::Rfc3339;
    time::OffsetDateTime::parse(rfc3339, &Rfc3339)
        .map_or(0, |t| (time::OffsetDateTime::now_utc() - t).whole_seconds().max(0))
}

fn ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// The configuration the drill's server runs with: the live one, pointed at
/// the scratch directory, with everything that reaches outside turned off
/// (an environment variable naming an external database, a public URL, TLS,
/// RESP, replicas, S3 storage).
fn scratch_config(live: &Config, scratch: &Path, engine: &forgebase_backup::manifest::EngineInfo) -> Config {
    use forgebase_core::config::{DatabaseBackend, PostgresMode};
    let mut c = live.clone();
    c.data_dir = scratch.to_path_buf();
    c.server.host = "127.0.0.1".to_owned();
    c.server.port = 0;
    c.server.public_url = None;
    c.resp.enabled = false;
    c.tls = TlsConfig::default();
    c.database.topology = forgebase_core::config::topology_config::TopologyConfig::default();
    c.database.postgres.url = None;
    c.database.postgres.mode = PostgresMode::Managed;
    c.database.postgres.listen = String::new();
    c.database.backend = if engine.is_postgres() {
        DatabaseBackend::Postgres
    } else {
        DatabaseBackend::Sqlite
    };
    c.storage.driver = forgebase_core::StorageDriverKind::Local;
    c.mcp.enabled = false;
    c.backup.drill_schedule = None;
    c
}

/// The directory to drill. Unlike `restore`, a drill takes a directory that
/// has no readable manifest too (it reports why it cannot be restored), and
/// `latest` is the newest snapshot *directory*: a damaged newest backup is
/// the one a restore would need, so it is the one to fail on, not an older
/// good one.
fn target_dir(store: &SnapshotStore, spec: &str) -> anyhow::Result<PathBuf> {
    // A snapshot of this data directory wins over a directory of the same name
    // in the working directory (the drill API passes snapshot ids).
    if let Some(dir) = store.path_of(spec).ok().filter(|p| p.is_dir()) {
        return Ok(dir);
    }
    if spec == "latest" {
        let newest = std::fs::read_dir(store.dir())
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| !n.starts_with('.') && !n.ends_with(".partial"))
            .max();
        if let Some(name) = newest {
            return Ok(store.dir().join(name));
        }
    } else if Path::new(spec).is_dir() && !Path::new(spec).join(forgebase_backup::manifest::MANIFEST).is_file() {
        return Ok(PathBuf::from(spec));
    }
    dataops::resolve_backup(store, spec)
}

/// Removes scratch directories of killed drills under `parent`.
fn sweep_stale(parent: &Path) {
    let Ok(rd) = std::fs::read_dir(parent) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let stale = e.file_name().to_string_lossy().starts_with("drill-")
            && p.join(MARKER).is_file()
            && e.metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > STALE_AFTER);
        if stale {
            let _ = std::fs::remove_dir_all(&p);
        }
    }
}

/// `forgebase backup drill`.
pub(crate) async fn run(
    config: &forgebase_core::Config,
    layout: &DataLayout,
    store: &SnapshotStore,
    args: DrillArgs,
) -> anyhow::Result<ExitCode> {
    let started_at = forgebase_core::now_rfc3339();
    let drill_id = format!(
        "{}-{:04x}",
        started_at.replace([':', '-', '.'], ""),
        std::process::id() & 0xffff
    );
    let t0 = Instant::now();

    let spec = args.target.clone().unwrap_or_else(|| "latest".to_owned());
    let path = target_dir(store, &spec)?;
    let mut checks = Checks(Vec::new());
    let mut counts = DrillCounts::default();
    let mut phases = DrillPhases::default();
    let mut kept = None;
    let mut backup_id = path
        .file_name()
        .map_or_else(|| spec.clone(), |n| n.to_string_lossy().into_owned());
    let mut backup_created = String::new();
    let mut engine_text = String::from("unknown");
    let mut schema_version = 0;

    'drill: {
        if topology::is_cluster_backup(&path) {
            checks.fail(
                "backup",
                "a cluster backup (replicas and shards) cannot be drilled yet: drill the control set's own \
                 backup, or restore it by hand (docs/operations/ha-failover.md)",
            );
            break 'drill;
        }
        let manifest = match Manifest::read(&path) {
            Ok(m) => m,
            Err(BackupError::Verify(p)) => {
                checks.fail("verify", p.join("; "));
                break 'drill;
            }
            Err(e) => {
                checks.fail("verify", e.to_string());
                break 'drill;
            }
        };
        backup_id.clone_from(&manifest.id);
        backup_created.clone_from(&manifest.created);
        let engine = manifest.engine_info();
        engine_text = engine.describe();
        schema_version = manifest.schema_version;
        if manifest.database_only {
            checks.fail(
                "backup",
                "this is a database-only snapshot (taken before a migration): it has no storage or key to test",
            );
            break 'drill;
        }
        if engine.is_postgres() && engine.method == Method::Dump {
            checks.fail(
                "backup",
                "a logical backup of an external PostgreSQL server needs a scratch server to load into, which \
                 the drill does not run: restore the dump into a scratch database and run `forgebase doctor` \
                 there (docs/operations/disaster-recovery.md#external-postgresql)",
            );
            break 'drill;
        }
        let tools = if engine.is_postgres() {
            if !database::bundled_engine_available(&config.database) {
                checks.fail("backup", database::NO_BUNDLED_ENGINE);
                break 'drill;
            }
            dataops::pg_tools(config, layout).await
        } else {
            None
        };

        // Scratch directory, with room for the restored copy.
        let parent = args.work_dir.clone().unwrap_or_else(|| layout.tmp_dir());
        if let Err(e) = std::fs::create_dir_all(&parent) {
            checks.fail("scratch", format!("cannot create {}: {e}", parent.display()));
            break 'drill;
        }
        sweep_stale(&parent);
        let need = manifest.total_bytes();
        let want = need.saturating_add(need / 5).saturating_add(256 * 1024 * 1024);
        if let Ok(free) = fs4::available_space(&parent)
            && free < want
        {
            checks.fail(
                "scratch",
                format!(
                    "{} has {} free, the drill needs about {} (the backup is {}): free space or use --work-dir on \
                     a larger volume",
                    parent.display(),
                    dataops::human_bytes(free),
                    dataops::human_bytes(want),
                    dataops::human_bytes(need)
                ),
            );
            break 'drill;
        }
        let scratch = parent.join(format!("drill-{drill_id}"));
        if let Err(e) = make_scratch(&scratch, &drill_id) {
            checks.fail("scratch", format!("cannot create {}: {e}", scratch.display()));
            break 'drill;
        }

        drill_in(
            &args,
            config,
            layout,
            &manifest,
            &path,
            &scratch,
            tools,
            &mut checks,
            &mut counts,
            &mut phases,
            t0,
        )
        .await;

        if args.keep {
            kept = Some(scratch.display().to_string());
        } else if let Err(e) = std::fs::remove_dir_all(&scratch) {
            checks.warn("cleanup", format!("{} could not be removed: {e}", scratch.display()));
        }
    }

    let rto = t0.elapsed();
    let failed = checks.failed();
    let problems: Vec<String> = checks
        .0
        .iter()
        .filter(|c| c.status == CheckStatus::Fail)
        .map(|c| format!("{}: {}", c.name, c.detail))
        .collect();
    let report = DrillReport {
        id: drill_id,
        backup_id,
        rpo_seconds: if backup_created.is_empty() {
            0
        } else {
            secs_since(&backup_created)
        },
        backup_created,
        engine: engine_text,
        schema_version,
        started_at,
        finished_at: forgebase_core::now_rfc3339(),
        outcome: if failed {
            DrillOutcome::Failed
        } else {
            DrillOutcome::Passed
        },
        rto_seconds: (rto.as_secs_f64() * 1000.0).round() / 1000.0,
        phases,
        counts,
        checks: checks.0,
        problems,
        trigger: args.trigger.clone(),
        kept,
    };
    if !args.no_record
        && let Err(e) = DrillStore::new(layout).record(&report)
    {
        eprintln!("warning: the drill result could not be recorded: {e}");
    }
    if args.json {
        dataops::print_json(&serde_json::to_value(&report)?)?;
    } else {
        print_report(&report);
    }
    Ok(if report.passed() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

#[allow(clippy::too_many_arguments)]
async fn drill_in(
    args: &DrillArgs,
    live: &Config,
    live_layout: &DataLayout,
    manifest: &Manifest,
    backup_path: &Path,
    scratch: &Path,
    tools: Option<forgebase_backup::PgSource>,
    checks: &mut Checks,
    counts: &mut DrillCounts,
    phases: &mut DrillPhases,
    t0: Instant,
) {
    let engine = manifest.engine_info();
    let cfg = scratch_config(live, scratch, &engine);
    let layout = DataLayout::new(scratch);

    // ---- restore (verifies first) -------------------------------------
    let key = args.master_key.clone().or_else(|| {
        backup::master_key_path(live_layout)
            .is_file()
            .then(|| backup::master_key_path(live_layout))
    });
    let marker = match dataops::restore_marker(&cfg, &layout, &engine) {
        Ok(m) => m,
        Err(e) => {
            checks.fail("restore", format!("{e:#}"));
            return;
        }
    };
    let restored = restore::restore(
        backup_path,
        &layout,
        RestoreOptions {
            force: false,
            master_key: if manifest.master_key.included {
                None
            } else {
                key.clone()
            },
            allow_missing_key: manifest.master_key.fingerprint.is_none(),
            skip_indexes: false,
            pg: tools,
            crash_after: None,
            fail_swap: false,
        },
    )
    .await;
    phases.restore_ms = ms(t0.elapsed());
    match restored {
        Ok(r) => {
            if let Err(e) = database::write_marker(&layout, marker) {
                checks.fail("restore", format!("the backend marker could not be written: {e}"));
                return;
            }
            checks.ok(
                "restore",
                format!(
                    "{} restored: {} file(s), {} (hashes verified)",
                    r.engine,
                    r.files,
                    dataops::human_bytes(r.bytes)
                ),
            );
            for w in &r.warnings {
                // The key has a check of its own below.
                if !w.starts_with("the master key is not part of this backup") {
                    checks.warn("restore", w.clone());
                }
            }
            let key_status = if manifest.master_key.included {
                ("master_key", CheckStatus::Ok, "restored from the backup".to_owned())
            } else if manifest.master_key.fingerprint.is_none() {
                (
                    "master_key",
                    CheckStatus::Warn,
                    "the data directory had no master key when the backup was taken".to_owned(),
                )
            } else {
                let from = args
                    .master_key
                    .as_ref()
                    .map_or("the live data directory's key".to_owned(), |p| p.display().to_string());
                (
                    "master_key",
                    CheckStatus::Ok,
                    format!("the backup holds no key; {from} matches its fingerprint"),
                )
            };
            checks.push(key_status.0, key_status.1, key_status.2);
        }
        Err(BackupError::Verify(problems)) => {
            checks.fail("verify", problems.join("; "));
            return;
        }
        Err(e) => {
            // The most useful message when the key is the problem (not escrowed, wrong key).
            let name = if matches!(e, BackupError::Invalid(ref m) if m.contains("master key")) {
                "master_key"
            } else {
                "restore"
            };
            checks.fail(name, e.to_string());
            return;
        }
    }

    // ---- start a quiescent server on the restored data -----------------
    let t_start = Instant::now();
    let server = match forgebase_server::start_quiescent(&cfg).await {
        Ok(s) => s,
        Err(e) => {
            phases.start_ms = ms(t_start.elapsed());
            checks.fail("server", format!("the restored data does not start: {e}"));
            return;
        }
    };
    phases.start_ms = ms(t_start.elapsed());
    let addr = server.local_addr();
    let t_checks = Instant::now();

    let health = crate::probe::get(&addr.to_string(), "/health", Duration::from_secs(5)).await;
    match health {
        Ok((200, _)) => checks.ok(
            "server",
            format!(
                "quiescent server answered on {addr} after {:.1}s ({} migration(s) applied at start)",
                phases.start_ms as f64 / 1000.0,
                server.startup_migrations().applied.len()
            ),
        ),
        Ok((status, _)) => checks.fail("server", format!("/health answered {status}")),
        Err(e) => checks.fail("server", format!("/health did not answer: {e}")),
    }

    // ---- schema --------------------------------------------------------
    match server.db().migration_status().await {
        Ok(s) if s.problems.is_empty() && s.pending.is_empty() => checks.ok(
            "schema",
            if s.current_version == manifest.schema_version {
                format!("schema v{} as in the backup", s.current_version)
            } else {
                format!(
                    "backup at schema v{}, migrated to v{} at start",
                    manifest.schema_version, s.current_version
                )
            },
        ),
        Ok(s) => checks.fail(
            "schema",
            format!(
                "v{} with pending {:?}, problems {:?}",
                s.current_version, s.pending, s.problems
            ),
        ),
        Err(e) => checks.fail("schema", format!("migration state unreadable: {e}")),
    }

    // ---- doctor (attaches to the running quiescent server) ----------------
    let loaded = LoadedConfig {
        config: cfg.clone(),
        file: None,
        sources: ConfigSources::default(),
        origin: ConfigOrigin::embedded(&cfg),
    };
    let mut doctor_fail = 0;
    let mut doctor_warn = 0;
    let mut doctor_total = 0;
    for (name, status, detail) in crate::doctor::instance_checks(&loaded).await {
        doctor_total += 1;
        match status {
            CheckStatus::Fail => {
                doctor_fail += 1;
                checks.fail(&format!("doctor:{name}"), detail);
            }
            CheckStatus::Warn => {
                doctor_warn += 1;
                // Facts about the scratch copy, not about the backup.
                if !matches!(name.as_str(), "snapshots" | "disk") {
                    checks.warn(&format!("doctor:{name}"), detail);
                }
            }
            _ => {}
        }
    }
    if doctor_fail == 0 {
        checks.ok(
            "doctor",
            format!("{doctor_total} check(s), {doctor_warn} warning(s), none failed"),
        );
    }

    // ---- counts against the manifest -------------------------------------
    let db = server.db().clone();
    match stats::collect(&db).await {
        Ok(got) => {
            counts.collections = got.collections.len() as u64;
            counts.records = got.collections.values().sum();
            counts.storage_objects = got.storage_objects;
            match &manifest.stats {
                Some(want) => {
                    let diffs = stats::compare(want, &got);
                    if diffs.is_empty() {
                        checks.ok(
                            "counts",
                            format!(
                                "{} collection(s), {} record(s), {} storage object(s): all as recorded in the manifest",
                                counts.collections, counts.records, counts.storage_objects
                            ),
                        );
                    } else {
                        for d in diffs {
                            checks.fail("counts", d);
                        }
                    }
                }
                None => checks.warn(
                    "counts",
                    format!(
                        "this backup carries no counts (made before ADR-0041): {} collection(s), {} record(s), {} \
                         storage object(s) found, nothing to compare with",
                        counts.collections, counts.records, counts.storage_objects
                    ),
                ),
            }
            sample_records(&db, &got, args.sample.clamp(1, 1000), checks, counts).await;
        }
        Err(e) => checks.fail("counts", format!("the restored data cannot be counted: {e}")),
    }

    // ---- storage blobs ----------------------------------------------------
    sample_blobs(&db, &layout, &cfg, checks, counts).await;

    // ---- master key and tls state --------------------------------------------
    match (
        backup::key_fingerprint(&backup::master_key_path(&layout)),
        &manifest.master_key.fingerprint,
    ) {
        (Ok(Some(got)), Some(want)) if &got == want => {
            checks.ok("master_key", format!("restored key has the backup's fingerprint {got}"));
        }
        (Ok(Some(got)), Some(want)) => checks.fail(
            "master_key",
            format!("the restored key has fingerprint {got}, the backup expects {want}"),
        ),
        (Ok(None) | Err(_), Some(_)) => checks.fail("master_key", "the restored data directory has no master key"),
        (_, None) => {}
    }
    if manifest.includes_tls() {
        let n = manifest.files.iter().filter(|f| f.kind == FileKind::Tls).count();
        let present = count_files(&layout.root().join(backup::TLS_DIR));
        if present >= n {
            checks.ok("tls", format!("the sealed tls/ state came back ({n} file(s))"));
        } else {
            checks.fail("tls", format!("{n} tls file(s) in the backup, {present} restored"));
        }
    } else if live.tls.mode == forgebase_core::config::tls_config::TlsMode::Acme {
        checks.warn(
            "tls",
            "the server uses ACME but this backup has no tls/ state: a restore would request a new certificate \
             (set [backup] include_tls = true)",
        );
    } else {
        checks.skip("tls", "no tls/ state in this backup (native ACME TLS is off)");
    }
    phases.checks_ms = ms(t_checks.elapsed());

    if let Err(e) = server.shutdown().await {
        checks.warn("server", format!("the drill server did not stop cleanly: {e}"));
    }
}

/// Reads `n` records of every collection through the data service, as the
/// system principal, and each one's page back by id.
async fn sample_records(
    db: &forgebase_db::Database,
    got: &stats::Counts,
    n: u32,
    checks: &mut Checks,
    counts: &mut DrillCounts,
) {
    let data = match DataService::new(std::sync::Arc::new(db.clone()), std::sync::Arc::new(DefaultAuthorizer)).await {
        Ok(d) => d,
        Err(e) => {
            checks.fail("records", format!("the data service does not open: {e}"));
            return;
        }
    };
    let ctx = AccessContext::system();
    let mut failures = Vec::new();
    for (name, &total) in &got.collections {
        if total == 0 {
            continue;
        }
        let q = ListQuery {
            per_page: Some(n),
            skip_total: true,
            ..ListQuery::default()
        };
        match data.list_records(&ctx, name, &q).await {
            Ok(page) => {
                if page.items.is_empty() {
                    failures.push(format!("{name}: {total} record(s) counted but a listing returned none"));
                }
                for rec in &page.items {
                    let rec_id = rec.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_owned();
                    match data
                        .get_record(&ctx, name, &rec_id, &forgebase_data::GetOptions::default())
                        .await
                    {
                        Ok(_) => counts.records_sampled += 1,
                        Err(e) => failures.push(format!("{name}/{rec_id}: {e}")),
                    }
                }
            }
            Err(e) => failures.push(format!("{name}: {e}")),
        }
    }
    if failures.is_empty() {
        checks.ok(
            "records",
            format!(
                "{} record(s) read back from {} collection(s)",
                counts.records_sampled,
                got.collections.values().filter(|&&c| c > 0).count()
            ),
        );
    } else {
        for f in failures.into_iter().take(10) {
            checks.fail("records", f);
        }
    }
}

/// Reads up to [`BLOB_SAMPLE`] blob files named by the database, hashes them
/// and compares size and SHA-256 with the row.
async fn sample_blobs(
    db: &forgebase_db::Database,
    layout: &DataLayout,
    cfg: &Config,
    checks: &mut Checks,
    counts: &mut DrillCounts,
) {
    use forgebase_db::{Backend, Statement, Value};
    use sha2::{Digest, Sha256};
    if cfg.storage.driver != forgebase_core::StorageDriverKind::Local {
        checks.skip(
            "blobs",
            "storage is not on the local disk: its objects are not part of the backup",
        );
        return;
    }
    let sql = format!(
        "SELECT sha256, size FROM _forgebase_storage_blobs WHERE refcount > 0 ORDER BY sha256 LIMIT {BLOB_SAMPLE}"
    );
    let rows = match db.fetch_all(&Statement::new(sql, vec![])).await {
        Ok(r) => r,
        Err(e) => {
            checks.fail("blobs", format!("the blob table cannot be read: {e}"));
            return;
        }
    };
    if rows.rows.is_empty() {
        checks.skip("blobs", "no storage blobs in this backup");
        return;
    }
    let mut problems = Vec::new();
    for row in &rows.rows {
        let (Some(sha), size) = (row.first().and_then(Value::as_str), row.get(1).and_then(Value::as_i64)) else {
            continue;
        };
        if sha.len() < 2 {
            continue;
        }
        let file = layout.storage_dir().join("blobs").join(&sha[..2]).join(sha);
        let sha = sha.to_owned();
        let read = tokio::task::spawn_blocking(move || {
            std::fs::read(&file).map(|b| {
                (
                    b.len(),
                    Sha256::digest(&b)
                        .iter()
                        .map(|x| format!("{x:02x}"))
                        .collect::<String>(),
                )
            })
        })
        .await;
        match read {
            Ok(Ok((len, got))) => {
                if got != sha {
                    problems.push(format!("blob {sha} is corrupt (its content hashes to {got})"));
                } else if size.is_some_and(|s| i64::try_from(len).ok() != Some(s)) {
                    problems.push(format!("blob {sha} has {len} byte(s), the database says {size:?}"));
                } else {
                    counts.blobs_sampled += 1;
                }
            }
            Ok(Err(e)) => problems.push(format!("blob {sha} cannot be read: {e}")),
            Err(e) => problems.push(format!("blob {sha}: {e}")),
        }
    }
    if problems.is_empty() {
        checks.ok(
            "blobs",
            format!(
                "{} blob(s) read and hashed: size and SHA-256 match",
                counts.blobs_sampled
            ),
        );
    } else {
        for p in problems.into_iter().take(10) {
            checks.fail("blobs", p);
        }
    }
}

fn human_duration(secs: i64) -> String {
    let (d, h, m, s) = (secs / 86_400, secs % 86_400 / 3600, secs % 3600 / 60, secs % 60);
    match (d, h, m) {
        (0, 0, 0) => format!("{s}s"),
        (0, 0, _) => format!("{m}m {s}s"),
        (0, _, _) => format!("{h}h {m}m"),
        _ => format!("{d}d {h}h"),
    }
}

fn print_report(r: &DrillReport) {
    println!(
        "restore drill of {} ({}, schema v{}, taken {})",
        r.backup_id, r.engine, r.schema_version, r.backup_created
    );
    for c in &r.checks {
        let tag = match c.status {
            CheckStatus::Ok => "[ok]  ",
            CheckStatus::Warn => "[warn]",
            CheckStatus::Fail => "[FAIL]",
            CheckStatus::Skip => "[skip]",
        };
        println!("{tag} {:<16} {}", c.name, c.detail);
    }
    println!(
        "\nRTO (restore + start + checks): {:.1}s  (restore {:.1}s, start {:.1}s, checks {:.1}s)",
        r.rto_seconds,
        r.phases.restore_ms as f64 / 1000.0,
        r.phases.start_ms as f64 / 1000.0,
        r.phases.checks_ms as f64 / 1000.0
    );
    println!(
        "RPO (age of the backup):        {}  (a restore loses what was written since)",
        human_duration(r.rpo_seconds)
    );
    if let Some(k) = &r.kept {
        println!("scratch directory kept: {k}");
    }
    println!(
        "{}",
        if r.passed() {
            "drill PASSED: this backup restores"
        } else {
            "drill FAILED: do not rely on this backup"
        }
    );
}

/// The scratch directory, owner-only like a data directory, with its marker.
fn make_scratch(scratch: &Path, drill_id: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(scratch)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(scratch, std::fs::Permissions::from_mode(0o700))?;
    }
    std::fs::write(scratch.join(MARKER), drill_id)
}

/// Files below `dir`, recursively.
fn count_files(dir: &Path) -> usize {
    let Ok(rd) = std::fs::read_dir(dir) else { return 0 };
    rd.flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => count_files(&e.path()),
            Ok(t) if t.is_file() => 1,
            _ => 0,
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use forgebase_backup::manifest::EngineInfo;
    use forgebase_core::config::tls_config::TlsMode;

    #[test]
    fn the_scratch_server_cannot_reach_what_the_live_one_does() {
        let mut live = Config::defaults(Path::new("/srv/live"));
        live.server.host = "0.0.0.0".into();
        live.server.port = 8090;
        live.server.public_url = Some("https://example.com".into());
        live.resp.enabled = true;
        live.tls.mode = TlsMode::Acme;
        live.mcp.enabled = true;
        live.storage.driver = forgebase_core::StorageDriverKind::S3;
        live.backup.drill_schedule = Some("0 3 * * *".into());
        live.database.postgres.mode = forgebase_core::config::PostgresMode::External;
        let c = scratch_config(&live, Path::new("/tmp/scratch"), &EngineInfo::sqlite());
        assert_eq!(c.data_dir, Path::new("/tmp/scratch"));
        assert_eq!((c.server.host.as_str(), c.server.port), ("127.0.0.1", 0));
        assert!(c.server.public_url.is_none() && !c.resp.enabled && !c.mcp.enabled);
        assert_eq!(c.tls.mode, TlsMode::Off);
        assert_eq!(c.storage.driver, forgebase_core::StorageDriverKind::Local);
        assert!(c.database.postgres.url.is_none() && c.backup.drill_schedule.is_none());
        assert_eq!(c.database.backend, forgebase_core::config::DatabaseBackend::Sqlite);
        assert_eq!(c.database.postgres.mode, forgebase_core::config::PostgresMode::Managed);
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!(human_duration(0), "0s");
        assert_eq!(human_duration(59), "59s");
        assert_eq!(human_duration(125), "2m 5s");
        assert_eq!(human_duration(7_980), "2h 13m");
        assert_eq!(human_duration(200_000), "2d 7h");
    }

    #[test]
    fn latest_is_the_newest_directory_even_when_its_manifest_is_damaged() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(tmp.path());
        let store = SnapshotStore::new(&layout);
        for id in [
            "20261101T000000Z",
            "20261102T000000Z",
            "20261103T000000Z.partial",
            ".hidden",
        ] {
            std::fs::create_dir_all(store.dir().join(id)).unwrap();
        }
        // Neither has a manifest: `SnapshotStore::resolve` would find nothing; the drill drills the newest.
        let newest = target_dir(&store, "latest").unwrap();
        assert_eq!(newest.file_name().unwrap(), "20261102T000000Z");
        // A snapshot id wins over a same-named directory elsewhere; a plain directory is taken as it is.
        assert_eq!(
            target_dir(&store, "20261101T000000Z").unwrap(),
            store.dir().join("20261101T000000Z")
        );
        let other = tmp.path().join("copied-backup");
        std::fs::create_dir_all(&other).unwrap();
        assert_eq!(target_dir(&store, other.to_str().unwrap()).unwrap(), other);
        assert!(target_dir(&store, "no-such-thing").is_err());
    }

    #[test]
    fn a_scratch_directory_is_private_and_marked() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("drill-x");
        make_scratch(&dir, "x").unwrap();
        assert_eq!(std::fs::read_to_string(dir.join(MARKER)).unwrap(), "x");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        }
        assert_eq!(count_files(&dir), 1);
        // Only old marked directories are swept.
        sweep_stale(tmp.path());
        assert!(dir.exists(), "a fresh scratch directory is left alone");
    }
}
