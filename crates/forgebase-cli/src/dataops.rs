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
use forgebase_backup::manifest::Manifest;
use forgebase_backup::restore::{self, RestoreOptions};
use forgebase_backup::topology::{self, ClusterBackupOptions, ClusterRestoreOptions, NodeSource, SetSource};
use forgebase_backup::transfer::{self, ExportOptions, Format, ImportOptions, OnConflict};
use forgebase_backup::{BackupOptions, DumpPlan, PgSource, Retention, SnapshotStore, backup};
use forgebase_core::{BackupSource, Config, DataLayout};
use forgebase_data::{DataService, DefaultAuthorizer};
use forgebase_db::Database;
use forgebase_server::database::{self, Access, BackendMarker, Create, PrimaryDatabase};
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

/// The engine's tools without a connection: `verify` and `restore` read a
/// backup and never start the cluster. The version comes from the engine's
/// own `postgres --version`.
async fn pg_tools(config: &Config, layout: &DataLayout) -> Option<PgSource> {
    let root = database::engine_root(&config.database, layout)?;
    let bin = root
        .join("bin")
        .join(format!("postgres{}", std::env::consts::EXE_SUFFIX));
    let out = tokio::process::Command::new(&bin)
        .arg("--version")
        .output()
        .await
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let version = forgebase_pgengine::PgVersion::parse(&text).map(|v| v.text);
    Some(PgSource::tools_only(Some(root), version.unwrap_or_default()))
}

/// The tools and the way in for the database `primary` (None on SQLite).
fn pg_source(config: &Config, layout: &DataLayout, primary: &PrimaryDatabase) -> Option<PgSource> {
    database::pg_source(
        config,
        layout,
        primary.marker(),
        primary.engine().map(|e| &**e),
        primary.attached(),
    )
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
    /// Back up every set of a cluster with replicas or shards (PostgreSQL on
    /// the bundled engine): one backup per set under the destination, with a
    /// combined manifest holding the placement epoch. Without a destination
    /// it goes to `<data dir>/backups/clusters/<UTC time>`.
    #[arg(long)]
    cluster: bool,
    /// With --cluster: copy each set from its `primary` or from a healthy
    /// `replica` (default: `[backup] source`). A set without a healthy
    /// replica is copied from its primary and the manifest says so.
    #[arg(long, value_name = "primary|replica")]
    source: Option<BackupSource>,
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
    if as_path.join(forgebase_backup::manifest::MANIFEST).is_file() || topology::is_cluster_backup(as_path) {
        return Ok(as_path.to_path_buf());
    }
    match store.resolve(spec) {
        Ok(s) => Ok(s.path),
        Err(e) if as_path.is_dir() => bail!(
            "{} is not a ForgeBase backup (no MANIFEST.json or CLUSTER.json); {e}",
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
                    "engine": s.manifest.engine_info(),
                    "files": s.manifest.files.len(), "master_key_included": s.manifest.master_key.included,
                })).collect::<Vec<_>>() }))?;
            } else if all.is_empty() {
                println!("no snapshots in {}", store.dir().display());
            } else {
                for s in &all {
                    println!(
                        "{}  {}  {}  schema v{}  {} in {} files{}",
                        s.id,
                        s.manifest.created,
                        s.manifest.engine_info().describe(),
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
            if topology::is_cluster_backup(&path) {
                return verify_cluster_cli(&config, &layout, &path, as_json).await;
            }
            let report = forgebase_backup::verify_with(
                &path,
                &forgebase_backup::VerifyOptions {
                    pg: pg_tools(&config, &layout).await,
                    tmp: Some(layout.tmp_dir()),
                },
            )
            .await;
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
    if a.source.is_some() && !a.cluster {
        bail!("--source applies to --cluster");
    }
    let db = open_read_only(config, layout).await?;
    if a.cluster {
        let out = cluster_backup(config, layout, &db, &a).await;
        db.close().await;
        return out;
    }
    let opts = BackupOptions {
        include_indexes: !a.no_indexes,
        include_master_key: a.include_master_key,
        label: a.label,
        pg: pg_source(config, layout, &db),
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
            "backup written to {}\n  {}, {} file(s), {}, schema v{}, master key {}",
            path.display(),
            m.engine_info().describe(),
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

// --------------------------------------------------------- cluster backup

/// A data directory relative to the data directory root: `postgres/data`
/// or `postgres/nodes/<name>/data`.
fn node_data_dir(name: Option<&str>) -> String {
    match name {
        None | Some("primary") => "postgres/data".to_owned(),
        Some(n) => format!("postgres/nodes/{n}/data"),
    }
}

/// How the tools reach a node of this host that is running: its own socket,
/// as `role`.
fn local_node_source(
    layout: &DataLayout,
    base: &PgSource,
    secrets: &forgebase_pgengine::RoleSecrets,
    name: &str,
    role: forgebase_pgengine::Role,
) -> anyhow::Result<PgSource> {
    use forgebase_backup::{Password, pg};
    let data = node_data_dir(Some(name));
    let (socket, port) = pg::postmaster_socket(&layout.root().join(&data)).with_context(|| {
        format!("node {name} is not running here (no live postmaster in {data}); start the server so its nodes run")
    })?;
    let password = secrets
        .password(role)
        .with_context(|| format!("roles.secret has no password for {}", role.name()))?;
    Ok(PgSource::managed(
        base.engine_root.clone(),
        base.version.clone(),
        socket,
        port,
        role.name(),
        Password::new(password),
        database::engine_layout(layout).postgres_dir(),
    ))
}

/// `forgebase backup --cluster`: every set of the topology, each from its
/// primary or a healthy replica.
async fn cluster_backup(
    config: &Config,
    layout: &DataLayout,
    primary: &PrimaryDatabase,
    a: &CreateArgs,
) -> anyhow::Result<ExitCode> {
    use forgebase_db::topology::{self as dbtopo, NodeKind, NodeRole, NodeState, SetState};
    use forgebase_pgengine::Role;

    if a.keep_last.is_some() || a.max_age_days.is_some() {
        bail!("--keep-last and --max-age-days apply to snapshots; a cluster backup is not a snapshot");
    }
    let Some(control_pg) = pg_source(config, layout, primary).filter(PgSource::is_managed) else {
        bail!(
            "--cluster backs up PostgreSQL on the bundled engine; SQLite and external servers have one set: use \
             `forgebase backup` (or pg_dump on the external server's own tools)"
        );
    };
    let source = a.source.unwrap_or(config.backup.source);
    let secrets = forgebase_pgengine::RoleSecrets::load(&database::engine_layout(layout).secrets_file())
        .ok()
        .flatten()
        .context("roles.secret is missing: the bundled engine has not been started in this data directory")?;
    let topo = dbtopo::load(primary.db()).await?;

    let mut sets = Vec::new();
    let mut opened: Vec<Database> = Vec::new();
    for set in topo.sets.iter().filter(|s| s.state != SetState::Retired) {
        let nodes: Vec<&dbtopo::DbNode> = topo
            .nodes_of(&set.id)
            .filter(|n| n.state != NodeState::Retired)
            .collect();
        let row = nodes.iter().find(|n| n.role == NodeRole::Primary);
        let (src, data_dir, db) = if set.is_control() {
            if let Some(r) = row
                && r.kind != NodeKind::Local
            {
                bail!("the control primary {} is not a local node", r.name);
            }
            let name = row.map(|r| r.name.as_str());
            (control_pg.clone(), node_data_dir(name), primary.db().clone())
        } else {
            let Some(r) = row else {
                bail!("set {} has no primary node", set.name);
            };
            if r.kind != NodeKind::Local {
                bail!(
                    "the primary {} of set {} runs on another host ({}): back it up there (pg_basebackup on that \
                     host, or `forgebase backup` of its own data directory) - a cluster backup copies the sets \
                     this host runs",
                    r.name,
                    set.name,
                    r.kind
                );
            }
            let src = local_node_source(layout, &control_pg, &secrets, &r.name, Role::Super)?;
            let url = shard_url(&src, &secrets)?;
            let mut opts = forgebase_db::DbOptions::from_config_target(
                &config.database,
                forgebase_db::DbTarget::postgres(url, true),
            );
            opts.read_pool_size = 1;
            let db = Database::open(opts.read_only())
                .await
                .with_context(|| format!("cannot open the primary {} of set {}", r.name, set.name))?;
            opened.push(db.clone());
            (src, node_data_dir(Some(&r.name)), db)
        };
        let primary_name = row.map_or_else(|| "primary".to_owned(), |r| r.name.clone());
        let mut replicas = Vec::new();
        if source == BackupSource::Replica {
            for n in nodes
                .iter()
                .filter(|n| n.role == NodeRole::Replica && n.kind == NodeKind::Local && n.state == NodeState::Streaming)
            {
                match local_node_source(layout, &control_pg, &secrets, &n.name, Role::Replicator) {
                    Ok(pg) => replicas.push(NodeSource {
                        name: n.name.clone(),
                        pg,
                        data_dir: None,
                    }),
                    Err(e) => tracing::warn!(node = %n.name, error = %e, "replica not usable as a backup source"),
                }
            }
        }
        sets.push(SetSource {
            set: set.clone(),
            db,
            primary: NodeSource {
                name: primary_name,
                pg: src,
                data_dir: Some(data_dir),
            },
            replicas,
        });
    }

    let dest = match &a.dest {
        Some(d) => std::path::absolute(d)?,
        None => layout
            .backups_dir()
            .join("clusters")
            .join(forgebase_core::now_rfc3339().replace([':', '.'], "-")),
    };
    let result = topology::create_cluster(
        primary.db(),
        layout,
        &dest,
        ClusterBackupOptions {
            base: BackupOptions {
                include_indexes: !a.no_indexes,
                include_master_key: a.include_master_key,
                label: a.label.clone(),
                ..BackupOptions::default()
            },
            source,
            sets,
        },
    )
    .await;
    for db in opened {
        db.close().await;
    }
    let m = result?;
    if a.json {
        print_json(&json!({ "path": dest, "manifest": m }))?;
    } else {
        println!(
            "cluster backup written to {}\n  {} set(s), {} file(s), {}, topology epoch {}, source {}",
            dest.display(),
            m.sets.len(),
            m.total_files(),
            human_bytes(m.total_bytes()),
            m.topology_epoch,
            m.source
        );
        for e in &m.sets {
            println!(
                "  {:<12} {:<9} from {} ({}), timeline {}, end LSN {}, schema v{}",
                e.name,
                e.kind.as_str(),
                e.node,
                e.role.as_str(),
                e.timeline.map_or_else(|| "-".to_owned(), |t| t.to_string()),
                e.end_lsn.as_deref().unwrap_or("-"),
                e.schema_version
            );
        }
        for w in &m.warnings {
            println!("  [warn] {w}");
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// The URL that opens a shard primary's `forgebase` database as the
/// superuser over its socket (for reading its schema version).
fn shard_url(src: &PgSource, secrets: &forgebase_pgengine::RoleSecrets) -> anyhow::Result<String> {
    use forgebase_backup::PgEndpoint;
    let PgEndpoint::Managed {
        socket_dir, port, user, ..
    } = &src.endpoint
    else {
        bail!("not a bundled node");
    };
    let password = secrets
        .password(forgebase_pgengine::Role::Super)
        .context("roles.secret has no superuser password")?;
    let enc = |v: &str| {
        v.bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => char::from(b).to_string(),
                _ => format!("%{b:02X}"),
            })
            .collect::<String>()
    };
    Ok(format!(
        "postgres://{}:{}@localhost/forgebase?host={}&port={port}",
        enc(user),
        enc(password),
        enc(&socket_dir.display().to_string())
    ))
}

async fn verify_cluster_cli(
    config: &Config,
    layout: &DataLayout,
    path: &Path,
    as_json: bool,
) -> anyhow::Result<ExitCode> {
    let report = topology::verify_cluster(
        path,
        &forgebase_backup::VerifyOptions {
            pg: pg_tools(config, layout).await,
            tmp: Some(layout.tmp_dir()),
        },
    )
    .await;
    if as_json {
        print_json(&serde_json::to_value(&report)?)?;
    } else {
        println!(
            "{}: {} set(s), {} file(s), {} checked",
            path.display(),
            report.sets.len(),
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
                "cluster backup is valid"
            } else {
                "cluster backup is NOT valid"
            }
        );
    }
    Ok(if report.ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
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
    let (config, layout) = setup(g)?;
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
    let Some(source) = cli.source.clone() else {
        bail!("name a backup directory, a snapshot id, `latest` or a time (or use --recover)");
    };
    let store = SnapshotStore::new(&layout);
    let path = resolve_backup(&store, &source)?;
    if topology::is_cluster_backup(&path) {
        return restore_cluster_cli(&config, &layout, &path, cli).await;
    }
    let manifest = Manifest::read(&path)?;
    let engine = manifest.engine_info();
    let target = restore_marker(&config, &layout, &engine)?;
    let pg = if engine.is_postgres() && engine.method == forgebase_backup::manifest::Method::Dump {
        // A logical backup loads into the external server named by the configuration.
        database::pg_source(&config, &layout, BackendMarker::PostgresExternal, None, None)
    } else if engine.is_postgres() {
        pg_tools(&config, &layout).await
    } else {
        None
    };
    let report = restore::restore(
        &path,
        &layout,
        RestoreOptions {
            force: cli.force,
            master_key: cli.master_key,
            allow_missing_key: cli.allow_missing_key,
            skip_indexes: cli.no_indexes,
            pg,
            crash_after: None,
        },
    )
    .await?;
    // The marker follows the data (ADR-0028 §6.3); a physical restore into an
    // empty directory is what writes it first.
    database::write_marker(&layout, target)?;
    if cli.json {
        print_json(&json!({ "source": path, "report": report }))?;
    } else {
        println!(
            "restored {} into {}\n  {}, {} file(s), {}, schema v{}; master key: {}",
            path.display(),
            layout.root().display(),
            report.engine,
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
        if engine.is_postgres() && engine.method == forgebase_backup::manifest::Method::Basebackup {
            println!("  PostgreSQL replays its WAL when the server starts, so the first start takes a moment longer");
        }
        println!("start the server with `forgebase serve`; run `forgebase doctor` to check the result");
    }
    Ok(ExitCode::SUCCESS)
}

/// `forgebase restore` of a cluster backup: every set, offline.
async fn restore_cluster_cli(
    config: &Config,
    layout: &DataLayout,
    path: &Path,
    cli: RestoreCli,
) -> anyhow::Result<ExitCode> {
    let cm = topology::ClusterManifest::read(path)?;
    let control = cm.control().context("the cluster backup has no control set")?;
    let engine = Manifest::read(&path.join(&control.dir))?.engine_info();
    let target = restore_marker(config, layout, &engine)?;
    let report = topology::restore_cluster(
        path,
        layout,
        ClusterRestoreOptions {
            base: RestoreOptions {
                force: cli.force,
                master_key: cli.master_key,
                allow_missing_key: cli.allow_missing_key,
                skip_indexes: cli.no_indexes,
                pg: pg_tools(config, layout).await,
                crash_after: None,
            },
            ..ClusterRestoreOptions::default()
        },
    )
    .await?;
    database::write_marker(layout, target)?;
    if cli.json {
        print_json(&json!({ "source": path, "report": report }))?;
    } else {
        println!(
            "restored cluster backup {} into {}\n  {} set(s), {} file(s), {}, topology epoch {}; master key: {}",
            path.display(),
            layout.root().display(),
            report.sets.len(),
            report.files,
            human_bytes(report.bytes),
            report.topology_epoch,
            report.master_key
        );
        for s in &report.sets {
            println!(
                "  {:<12} schema v{} -> {}",
                s.name,
                s.schema_version,
                s.data_dir.as_deref().unwrap_or("(loaded into its server)")
            );
        }
        if let Some(prev) = &report.previous_data {
            println!("  previous data kept in {}", prev.display());
        }
        for r in &report.moved_aside {
            println!("  moved aside (stale after the restore): {r}");
        }
        for r in &report.replicas {
            println!("  replica {} ({}) needs a rebuild", r.node, r.kind);
        }
        for w in &report.warnings {
            println!("  note: {w}");
        }
        println!(
            "start the server with `forgebase serve`; replicas are rebuilt and every set is brought to the \
             schema head once the restored control database runs ({} lists the work)",
            report.pending_file.display()
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// The backend marker a restore of a backup made by `engine` leaves behind,
/// after checking that it does not change the backend of this data directory
/// (that is `forgebase db convert`'s job, FB-151).
fn restore_marker(
    config: &Config,
    layout: &DataLayout,
    engine: &forgebase_backup::manifest::EngineInfo,
) -> anyhow::Result<BackendMarker> {
    let wanted = match (engine.kind.as_str(), engine.mode.as_deref()) {
        ("postgres", Some("external")) => BackendMarker::PostgresExternal,
        ("postgres", _) => BackendMarker::PostgresManaged,
        _ => BackendMarker::Sqlite,
    };
    if wanted == BackendMarker::PostgresManaged && !database::bundled_engine_available(&config.database) {
        bail!("{}", database::NO_BUNDLED_ENGINE);
    }
    if wanted == BackendMarker::PostgresExternal && config.database.postgres.url.is_none() {
        bail!(
            "the backup is a logical backup of an external PostgreSQL server: set FORGEBASE_DATABASE_URL to the \
             server to load it into, and keep database.postgres.mode = \"external\""
        );
    }
    if let Some(have) = database::read_marker(layout)?
        && have != wanted
    {
        bail!(
            "this data directory is {} but the backup is {}: a restore does not change the backend; move the \
             project with `forgebase db convert` (FB-151), or restore into an empty data directory",
            have.describe(),
            wanted.describe()
        );
    }
    Ok(wanted)
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
            let _ = database::checkpoint(&db).await;
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
            let _ = database::checkpoint(&db).await;
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

// -------------------------------------------------------------------- dump

/// `forgebase db dump`.
#[derive(Debug, Args)]
pub struct DumpCli {
    /// Destination file (default: `<data dir>/backups/dumps/forgebase-<UTC time>.dump`).
    dest: Option<PathBuf>,
    /// Write plain SQL instead of the custom format.
    #[arg(long)]
    plain: bool,
    /// Only the schema, no rows.
    #[arg(long)]
    schema_only: bool,
    /// Print JSON.
    #[arg(long)]
    json: bool,
}

/// Runs `forgebase db dump`: `pg_dump` of the whole database, without
/// ownership and privileges, so any PostgreSQL can load it. Works while the
/// server runs.
pub async fn run_db_dump(g: &GlobalArgs, cli: DumpCli) -> anyhow::Result<ExitCode> {
    let (config, layout) = setup(g)?;
    let primary = open_read_only(&config, &layout).await?;
    let result = dump(&config, &layout, &primary, &cli).await;
    primary.close().await;
    let (path, size) = result?;
    if cli.json {
        print_json(&json!({ "path": path, "bytes": size, "format": if cli.plain { "plain" } else { "custom" } }))?;
    } else {
        println!(
            "dump written to {} ({})\n  load it with {}",
            path.display(),
            human_bytes(size),
            if cli.plain {
                "psql -f"
            } else {
                "pg_restore -d <database>"
            }
        );
    }
    Ok(ExitCode::SUCCESS)
}

async fn dump(
    config: &Config,
    layout: &DataLayout,
    primary: &PrimaryDatabase,
    cli: &DumpCli,
) -> anyhow::Result<(PathBuf, u64)> {
    let Some(src) = pg_source(config, layout, primary) else {
        bail!(
            "this project uses SQLite: `forgebase backup` copies it, and `sqlite3 {} .dump` gives plain SQL",
            layout.database_path().display()
        );
    };
    let dest = match &cli.dest {
        Some(d) => std::path::absolute(d)?,
        None => {
            let stamp = forgebase_core::now_rfc3339().replace([':', '-'], "");
            layout
                .backups_dir()
                .join("dumps")
                .join(format!("forgebase-{stamp}.{}", if cli.plain { "sql" } else { "dump" }))
        }
    };
    if dest.exists() {
        bail!("{} already exists", dest.display());
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
    }
    src.dump(
        &dest,
        &layout.tmp_dir(),
        &DumpPlan {
            forgebase_schema_only: false,
            plain: cli.plain,
            portable: true,
            schema_only: cli.schema_only,
        },
    )
    .await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o600))?;
    }
    let size = std::fs::metadata(&dest)?.len();
    Ok((dest, size))
}
