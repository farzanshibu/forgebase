//! `forgebase backup`, `restore`, `export` and `import` (FB-83).
//!
//! * `backup` and `export` only read: they open the database read-only, so
//!   they work while the server runs and never take the data-directory lock.
//! * `restore` and `import` change files or schema the server holds in
//!   memory, so they take the lock and refuse while a server runs.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{Context, bail};
use clap::{Args, Subcommand};
use forgebase_backup::restore::{self, RestoreOptions};
use forgebase_backup::transfer::{self, ExportOptions, Format, ImportOptions, OnConflict};
use forgebase_backup::{BackupOptions, Retention, SnapshotStore, backup};
use forgebase_core::{Config, DataLayout};
use forgebase_data::{DataService, DefaultAuthorizer};
use forgebase_db::Database;
use forgebase_server::database::{self, Access, Create, PrimaryDatabase};
use forgebase_server::{DataDirLock, ServerError};
use serde_json::json;

use crate::GlobalArgs;
use crate::commands::quiet_logging;

fn setup(g: &GlobalArgs) -> anyhow::Result<(Config, DataLayout)> {
    quiet_logging(g)?;
    let config = g.load()?.config;
    let layout = DataLayout::new(&config.data_dir);
    Ok((config, layout))
}

fn lock(layout: &DataLayout, what: &str) -> anyhow::Result<DataDirLock> {
    DataDirLock::acquire(layout).map_err(|e| match e {
        ServerError::Locked { .. } => anyhow::anyhow!("{e}; stop the server before {what}"),
        other => other.into(),
    })
}

/// The database for a read-only command, on any backend (FB-148): the
/// running server's engine, or the bundled engine started for the command.
/// `close()` stops an engine it started.
async fn open_read_only(config: &Config, layout: &DataLayout) -> anyhow::Result<PrimaryDatabase> {
    database::open_attached(config, false, layout, Access::Read)
        .await
        .with_context(|| {
            format!(
                "cannot open the database in {} (run `forgebase init` first)",
                layout.root().display()
            )
        })
}

async fn data_service(db: &Database) -> anyhow::Result<DataService> {
    let status = db.migration_status().await?;
    if !status.pending.is_empty() {
        bail!(
            "the database is at schema v{} but this build expects v{}: run `forgebase migrate` first",
            status.current_version,
            status.latest_version
        );
    }
    Ok(DataService::new(Arc::new(db.clone()), Arc::new(DefaultAuthorizer)).await?)
}

fn print_json(v: &serde_json::Value) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

fn retention(keep_last: Option<usize>, max_age_days: Option<u32>) -> Option<Retention> {
    (keep_last.is_some() || max_age_days.is_some()).then_some(Retention {
        keep_last,
        max_age_days,
    })
}

// ------------------------------------------------------------------ backup

/// `forgebase backup`.
#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct BackupCli {
    #[command(subcommand)]
    command: Option<BackupCommand>,
    #[command(flatten)]
    create: CreateArgs,
}

#[derive(Debug, Args)]
struct CreateArgs {
    /// Write the backup to this new directory. Without it a snapshot is
    /// written to `<data dir>/backups/snapshots/<UTC time>`.
    dest: Option<PathBuf>,
    /// Free-form label stored in the manifest.
    #[arg(long)]
    label: Option<String>,
    /// Leave the derived index files out (they are rebuilt after a restore).
    #[arg(long)]
    no_indexes: bool,
    /// Also copy auth/master.key into the backup. Off by default: keep the key
    /// apart from the data it protects.
    #[arg(long)]
    include_master_key: bool,
    /// Snapshots: always keep the newest N (deletes the rest after the backup).
    #[arg(long, value_name = "N")]
    keep_last: Option<usize>,
    /// Snapshots: delete snapshots older than this many days (beyond --keep-last).
    #[arg(long, value_name = "DAYS")]
    max_age_days: Option<u32>,
    /// Print JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Subcommand)]
enum BackupCommand {
    /// List snapshots, newest first.
    List {
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Check a backup: manifest, every file's SHA-256 and the database's integrity.
    Verify {
        /// Backup directory, snapshot id, `latest`, or an RFC 3339 time.
        target: String,
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Delete snapshots by retention policy.
    Prune {
        /// Always keep the newest N.
        #[arg(long, value_name = "N")]
        keep_last: Option<usize>,
        /// Delete snapshots older than this many days (beyond --keep-last).
        #[arg(long, value_name = "DAYS")]
        max_age_days: Option<u32>,
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Delete one snapshot.
    Delete {
        /// Snapshot id.
        id: String,
    },
}

fn resolve_backup(store: &SnapshotStore, spec: &str) -> anyhow::Result<PathBuf> {
    let as_path = Path::new(spec);
    if as_path.join(forgebase_backup::manifest::MANIFEST).is_file() {
        return Ok(as_path.to_path_buf());
    }
    match store.resolve(spec) {
        Ok(s) => Ok(s.path),
        Err(e) if as_path.is_dir() => bail!(
            "{} is not a ForgeBase backup (no MANIFEST.json); {e}",
            as_path.display()
        ),
        Err(e) => Err(e.into()),
    }
}

/// Runs `forgebase backup`.
pub async fn run_backup(g: &GlobalArgs, cli: BackupCli) -> anyhow::Result<ExitCode> {
    let (config, layout) = setup(g)?;
    let store = SnapshotStore::new(&layout);
    match cli.command {
        None => create(&config, &layout, &store, cli.create).await,
        Some(BackupCommand::List { json: as_json }) => {
            let all = store.list()?;
            if as_json {
                print_json(&json!({ "dir": store.dir(), "snapshots": all.iter().map(|s| json!({
                    "id": s.id, "created": s.manifest.created, "label": s.manifest.label,
                    "schema_version": s.manifest.schema_version, "bytes": s.manifest.total_bytes(),
                    "files": s.manifest.files.len(), "master_key_included": s.manifest.master_key.included,
                })).collect::<Vec<_>>() }))?;
            } else if all.is_empty() {
                println!("no snapshots in {}", store.dir().display());
            } else {
                for s in &all {
                    println!(
                        "{}  {}  schema v{}  {} in {} files{}",
                        s.id,
                        s.manifest.created,
                        s.manifest.schema_version,
                        human_bytes(s.manifest.total_bytes()),
                        s.manifest.files.len(),
                        s.manifest
                            .label
                            .as_deref()
                            .map(|l| format!("  [{l}]"))
                            .unwrap_or_default()
                    );
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Some(BackupCommand::Verify { target, json: as_json }) => {
            let path = resolve_backup(&store, &target)?;
            let report = forgebase_backup::verify(&path).await;
            if as_json {
                print_json(&serde_json::to_value(&report)?)?;
            } else {
                println!(
                    "{}: {} file(s), {} checked",
                    path.display(),
                    report.files_checked,
                    human_bytes(report.bytes_checked)
                );
                for w in &report.warnings {
                    println!("[warn] {w}");
                }
                for p in &report.problems {
                    println!("[FAIL] {p}");
                }
                println!(
                    "{}",
                    if report.ok {
                        "backup is valid"
                    } else {
                        "backup is NOT valid"
                    }
                );
            }
            Ok(if report.ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
        Some(BackupCommand::Prune {
            keep_last,
            max_age_days,
            json: as_json,
        }) => {
            let Some(r) = retention(keep_last, max_age_days) else {
                bail!("give --keep-last and/or --max-age-days");
            };
            let gone = store.prune(r).await?;
            if as_json {
                print_json(&json!({ "deleted": gone }))?;
            } else {
                println!(
                    "deleted {} snapshot(s){}",
                    gone.len(),
                    if gone.is_empty() {
                        String::new()
                    } else {
                        format!(": {}", gone.join(", "))
                    }
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        Some(BackupCommand::Delete { id }) => {
            store.delete(&id).await?;
            println!("deleted {id}");
            Ok(ExitCode::SUCCESS)
        }
    }
}

async fn create(
    config: &Config,
    layout: &DataLayout,
    store: &SnapshotStore,
    a: CreateArgs,
) -> anyhow::Result<ExitCode> {
    let db = open_read_only(config, layout).await?;
    let opts = BackupOptions {
        include_indexes: !a.no_indexes,
        include_master_key: a.include_master_key,
        label: a.label,
        ..BackupOptions::default()
    };
    let result = match &a.dest {
        Some(dest) => {
            if a.keep_last.is_some() || a.max_age_days.is_some() {
                bail!("--keep-last and --max-age-days apply to snapshots; omit the destination to take a snapshot");
            }
            let abs = std::path::absolute(dest)?;
            backup::create(&db, layout, &abs, opts).await.map(|m| (abs, m))
        }
        None => store
            .create(&db, layout, opts, retention(a.keep_last, a.max_age_days))
            .await
            .map(|m| (store.dir().join(&m.id), m)),
    };
    db.close().await;
    let (path, m) = result?;
    if a.json {
        print_json(&json!({ "path": path, "manifest": m }))?;
    } else {
        println!(
            "backup written to {}\n  {} file(s), {}, schema v{}, master key {}",
            path.display(),
            m.files.len(),
            human_bytes(m.total_bytes()),
            m.schema_version,
            if m.master_key.included {
                "included"
            } else {
                "not included (keep auth/master.key safe separately)"
            }
        );
    }
    Ok(ExitCode::SUCCESS)
}

// ----------------------------------------------------------------- restore

/// `forgebase restore`.
#[derive(Debug, Args)]
pub struct RestoreCli {
    /// Backup directory, snapshot id, `latest`, or an RFC 3339 time (restores
    /// the newest snapshot taken at or before it).
    source: Option<String>,
    /// Replace an existing database; the current data is kept in backups/pre-restore-*.
    #[arg(long)]
    force: bool,
    /// The original auth/master.key, when the backup does not contain it.
    #[arg(long, value_name = "FILE")]
    master_key: Option<PathBuf>,
    /// Go on without the master key (sealed secrets become unreadable).
    #[arg(long)]
    allow_missing_key: bool,
    /// Do not restore index files; they are rebuilt at startup.
    #[arg(long)]
    no_indexes: bool,
    /// Only roll back an interrupted restore, then stop.
    #[arg(long)]
    recover: bool,
    /// Print JSON.
    #[arg(long)]
    json: bool,
}

/// Runs `forgebase restore`.
pub async fn run_restore(g: &GlobalArgs, cli: RestoreCli) -> anyhow::Result<ExitCode> {
    let (_config, layout) = setup(g)?;
    layout
        .ensure()
        .with_context(|| format!("cannot create {}", layout.root().display()))?;
    let _lock = lock(&layout, "restoring")?;
    if cli.recover {
        let rolled = restore::recover(&layout)?;
        println!(
            "{}",
            if rolled {
                "rolled back an interrupted restore"
            } else {
                "no interrupted restore"
            }
        );
        return Ok(ExitCode::SUCCESS);
    }
    let Some(source) = cli.source else {
        bail!("name a backup directory, a snapshot id, `latest` or a time (or use --recover)");
    };
    let store = SnapshotStore::new(&layout);
    let path = resolve_backup(&store, &source)?;
    let report = restore::restore(
        &path,
        &layout,
        RestoreOptions {
            force: cli.force,
            master_key: cli.master_key,
            allow_missing_key: cli.allow_missing_key,
            skip_indexes: cli.no_indexes,
            crash_after: None,
        },
    )
    .await?;
    if cli.json {
        print_json(&json!({ "source": path, "report": report }))?;
    } else {
        println!(
            "restored {} into {}\n  {} file(s), {}, schema v{}; master key: {}",
            path.display(),
            layout.root().display(),
            report.files,
            human_bytes(report.bytes),
            report.schema_version,
            report.master_key
        );
        if let Some(prev) = &report.previous_data {
            println!("  previous data kept in {}", prev.display());
        }
        for w in &report.warnings {
            println!("  note: {w}");
        }
        println!("start the server with `forgebase serve`; run `forgebase doctor` to check the result");
    }
    Ok(ExitCode::SUCCESS)
}

// ------------------------------------------------------------ export/import

/// `forgebase export`.
#[derive(Debug, Subcommand)]
pub enum ExportCommand {
    /// Export collections (schema and records, ids preserved) to a new bundle directory.
    Collections {
        /// New directory for the bundle.
        dest: PathBuf,
        /// Only this collection (repeatable); default: all non-system collections.
        #[arg(long = "collection", short = 'c')]
        collections: Vec<String>,
        /// Also write the effective configuration as config.toml.
        #[arg(long)]
        with_config: bool,
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Export the records of one collection to a file.
    Records {
        /// Collection name.
        collection: String,
        /// New file.
        dest: PathBuf,
        /// json, ndjson or csv.
        #[arg(long, default_value = "ndjson")]
        format: Format,
    },
    /// Print the effective configuration as TOML (or write it to FILE).
    Config {
        /// New file; standard output when omitted.
        file: Option<PathBuf>,
    },
}

/// `forgebase import`.
#[derive(Debug, Subcommand)]
pub enum ImportCommand {
    /// Import a bundle made by `export collections`. Stop the server first.
    Collections {
        /// Bundle directory.
        src: PathBuf,
        /// What to do with existing collections and records: fail or skip.
        #[arg(long, default_value = "fail")]
        on_conflict: ConflictArg,
        /// Only this collection (repeatable).
        #[arg(long = "collection", short = 'c')]
        collections: Vec<String>,
        /// Check the bundle and report, change nothing.
        #[arg(long)]
        dry_run: bool,
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Import records from a json or ndjson file into an existing collection.
    Records {
        /// Collection name.
        collection: String,
        /// File.
        src: PathBuf,
        /// json or ndjson.
        #[arg(long, default_value = "ndjson")]
        format: Format,
        /// What to do with existing record ids: fail or skip.
        #[arg(long, default_value = "fail")]
        on_conflict: ConflictArg,
    },
    /// Validate a configuration file and install it as the project's config file.
    Config {
        /// TOML file made by `export config`.
        file: PathBuf,
        /// Replace an existing config file.
        #[arg(long)]
        force: bool,
    },
}

/// `fail` or `skip`.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum ConflictArg {
    /// Stop at the first existing collection or record.
    Fail,
    /// Reuse existing collections and skip existing record ids.
    Skip,
}

impl From<ConflictArg> for OnConflict {
    fn from(c: ConflictArg) -> Self {
        match c {
            ConflictArg::Fail => OnConflict::Fail,
            ConflictArg::Skip => OnConflict::Skip,
        }
    }
}

/// Runs `forgebase export`.
pub async fn run_export(g: &GlobalArgs, cmd: ExportCommand) -> anyhow::Result<ExitCode> {
    let (config, layout) = setup(g)?;
    match cmd {
        ExportCommand::Config { file } => {
            let toml = transfer::config_to_toml(&config)?;
            match file {
                Some(f) => {
                    write_new(&f, &toml)?;
                    println!("configuration written to {}", f.display());
                }
                None => print!("{toml}"),
            }
            Ok(ExitCode::SUCCESS)
        }
        ExportCommand::Collections {
            dest,
            collections,
            with_config,
            json: as_json,
        } => {
            let db = open_read_only(&config, &layout).await?;
            let result = async {
                let data = data_service(&db).await?;
                let config_toml = if with_config {
                    Some(transfer::config_to_toml(&config)?)
                } else {
                    None
                };
                let opts = ExportOptions {
                    collections: (!collections.is_empty()).then_some(collections),
                    config_toml,
                };
                Ok::<_, anyhow::Error>(transfer::export_bundle(&data, &std::path::absolute(&dest)?, opts).await?)
            }
            .await;
            db.close().await;
            let report = result?;
            if as_json {
                print_json(&serde_json::to_value(&report)?)?;
            } else {
                for c in &report.bundle.collections {
                    println!("{:<24} {:<5} {} record(s)", c.name, c.kind, c.records);
                }
                for w in &report.warnings {
                    println!("[warn] {w}");
                }
                println!("bundle written to {}", dest.display());
            }
            Ok(ExitCode::SUCCESS)
        }
        ExportCommand::Records {
            collection,
            dest,
            format,
        } => {
            let db = open_read_only(&config, &layout).await?;
            let result = async {
                let data = data_service(&db).await?;
                Ok::<_, anyhow::Error>(
                    transfer::export_records(&data, &collection, &std::path::absolute(&dest)?, format).await?,
                )
            }
            .await;
            db.close().await;
            println!("{} record(s) written to {}", result?, dest.display());
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn write_new(path: &Path, text: &str) -> anyhow::Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("cannot create {} (it must not exist)", path.display()))?;
    f.write_all(text.as_bytes())?;
    Ok(())
}

/// Runs `forgebase import`.
pub async fn run_import(g: &GlobalArgs, cmd: ImportCommand) -> anyhow::Result<ExitCode> {
    let (config, layout) = setup(g)?;
    match cmd {
        ImportCommand::Config { file, force } => {
            let validated = transfer::validate_config_file(&std::path::absolute(&file)?)?;
            let target = match &g.config {
                Some(p) => std::path::absolute(p)?,
                None => std::env::current_dir()?.join(forgebase_core::config::DEFAULT_CONFIG_FILE),
            };
            if target.exists() && !force {
                bail!("{} exists; use --force to replace it", target.display());
            }
            std::fs::copy(&file, &target).with_context(|| format!("cannot write {}", target.display()))?;
            println!(
                "{} is valid and installed as {} (port {}, data directory comes from the environment or flags)",
                file.display(),
                target.display(),
                validated.server.port
            );
            Ok(ExitCode::SUCCESS)
        }
        ImportCommand::Collections {
            src,
            on_conflict,
            collections,
            dry_run,
            json: as_json,
        } => {
            let _lock = lock(&layout, "importing")?;
            let db = open_read_write(&config, &layout).await?;
            let result = async {
                let data = data_service(&db).await?;
                let opts = ImportOptions {
                    on_conflict: on_conflict.into(),
                    collections: (!collections.is_empty()).then_some(collections),
                    dry_run,
                };
                Ok::<_, anyhow::Error>(transfer::import_bundle(&data, &std::path::absolute(&src)?, opts).await?)
            }
            .await;
            let _ = db.checkpoint().await;
            db.close().await;
            let report = result?;
            if as_json {
                print_json(&serde_json::to_value(&report)?)?;
            } else {
                for c in &report.collections {
                    println!(
                        "{:<24} {}  {} record(s), {} skipped",
                        c.name,
                        if c.created { "created" } else { "existing" },
                        c.records,
                        c.skipped
                    );
                }
                if report.dry_run {
                    println!("dry run: nothing was changed");
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        ImportCommand::Records {
            collection,
            src,
            format,
            on_conflict,
        } => {
            let _lock = lock(&layout, "importing")?;
            let db = open_read_write(&config, &layout).await?;
            let result = async {
                let data = data_service(&db).await?;
                Ok::<_, anyhow::Error>(
                    transfer::import_records_file(
                        &data,
                        &collection,
                        &std::path::absolute(&src)?,
                        format,
                        on_conflict.into(),
                    )
                    .await?,
                )
            }
            .await;
            let _ = db.checkpoint().await;
            db.close().await;
            let (n, skipped) = result?;
            println!("{n} record(s) imported, {skipped} skipped");
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// The database for a writing command; the caller holds the lock (FB-148:
/// the bundled engine is started here and stopped by `close()`).
async fn open_read_write(config: &Config, layout: &DataLayout) -> anyhow::Result<PrimaryDatabase> {
    database::open_owned(config, false, layout, Create::No)
        .await
        .with_context(|| {
            format!(
                "cannot open the database in {} (run `forgebase init` first)",
                layout.root().display()
            )
        })
}
