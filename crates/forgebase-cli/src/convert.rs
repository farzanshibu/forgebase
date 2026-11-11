//! `forgebase db convert --to postgres|sqlite` (FB-151, ADR-0028 §6.4): moves
//! a stopped project between SQLite and PostgreSQL with a verified
//! table-level copy (`forgebase-convert`).
//!
//! Steps: take the data-directory lock (the server must be stopped); take a
//! snapshot (`backups/`); migrate the source to this build's newest
//! version; create the target (the bundled engine's `initdb`, an empty
//! external database, or a new `forgebase.db`) and migrate it; `--to
//! sqlite` first brings extension data to portable rows; copy and verify;
//! then write the backend marker and `[database] backend`, and keep the old
//! database under a new name (`forgebase.db.pre-postgres`,
//! `postgres/data.pre-sqlite`), never deleted. A failed run removes the
//! target it made and leaves the source as it was.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, anyhow, bail};
use clap::{Args, ValueEnum};
use forgebase_backup::{BackupOptions, SnapshotStore};
use forgebase_core::config::{DatabaseBackend, PostgresMode};
use forgebase_core::{Config, DataLayout, LoadedConfig};
use forgebase_db::{Database, DbOptions, DbTarget, SecretString};
use forgebase_pgengine::{Engine, EngineHandle, Role};
use forgebase_server::database::{self, BackendMarker, Create, PrimaryDatabase};
use forgebase_server::{DataDirLock, ServerError};
use serde_json::json;

use crate::GlobalArgs;

/// The backend to convert to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum To {
    /// PostgreSQL: the bundled engine, or the external server when
    /// `[database.postgres] mode = "external"` (URL in FORGEBASE_DATABASE_URL).
    Postgres,
    /// SQLite: one file, `forgebase.db`.
    Sqlite,
}

/// `forgebase db convert`.
#[derive(Debug, Args)]
pub struct ConvertCli {
    /// The backend to move the project to.
    #[arg(long, value_enum)]
    to: To,
    /// Skip the snapshot taken before converting (the old database is kept
    /// either way).
    #[arg(long)]
    no_backup: bool,
    /// `--to sqlite`: refuse when vectors, queue deliveries or schedules are
    /// held by pgvector, pgmq or pg_cron, instead of converting them to
    /// portable rows in the source first.
    #[arg(long)]
    no_prepare: bool,
    /// Print the report as JSON.
    #[arg(long)]
    json: bool,
}

/// The target this run created, removed again when the conversion fails.
enum Made {
    Sqlite(PathBuf),
    Managed { engine: Box<EngineHandle>, pgdata: PathBuf },
    External,
}

/// Runs `forgebase db convert`.
pub async fn run(g: &GlobalArgs, args: ConvertCli) -> anyhow::Result<ExitCode> {
    let loaded = g.load()?;
    let config = &loaded.config;
    let layout = DataLayout::new(&config.data_dir);
    let no_database = || anyhow!("no database in {} (run `forgebase init`)", layout.root().display());
    if !layout.root().is_dir() {
        return Err(no_database());
    }
    let lock = DataDirLock::acquire(&layout).map_err(|e| match e {
        ServerError::Locked { .. } => anyhow!("{e}; stop the server before converting the database"),
        other => other.into(),
    })?;
    let Some(from) = database::existing(&layout)? else {
        return Err(no_database());
    };
    let to = match args.to {
        To::Sqlite => BackendMarker::Sqlite,
        To::Postgres if config.database.postgres.mode == PostgresMode::External => BackendMarker::PostgresExternal,
        To::Postgres => BackendMarker::PostgresManaged,
    };
    if from.backend() == to.backend() {
        bail!("{} already holds {}", layout.root().display(), from.describe());
    }
    let say = |line: &str| {
        if !args.json {
            eprintln!("{line}");
        }
    };

    // Where the old database goes; decided before anything changes.
    let keep = match from {
        BackendMarker::Sqlite => Some(free_name(&layout.root().join("forgebase.db.pre-postgres"))),
        BackendMarker::PostgresManaged => Some(free_name(
            &database::engine_layout(&layout).postgres_dir().join("data.pre-sqlite"),
        )),
        BackendMarker::PostgresExternal => None,
    };
    if from == BackendMarker::PostgresManaged {
        // Replicas and shard sets live under postgres/nodes (ADR-0029); SQLite holds one set.
        forgebase_backup::pg::ensure_single_set(layout.root())
            .map_err(|_| anyhow!("{} holds replicas or shard sets: remove them before converting to SQLite (SQLite holds one database set)", database::engine_layout(&layout).postgres_dir().join("nodes").display()))?;
    }
    preflight_target(config, &layout, to).await?;

    // 1-2. The source: opened (the bundled engine started), backed up, migrated.
    let source = database::open_owned(config, false, &layout, Create::No).await?;
    let result = convert_open(config, &layout, &source, to, &args, say).await;
    let (report, prepared, made) = match result {
        Ok(v) => v,
        Err(e) => {
            source.close().await;
            drop(lock);
            return Err(e);
        }
    };
    // Close both sides before files move.
    let source_url_hint = from;
    source.close().await;
    if let Made::Managed { engine, .. } = &made
        && let Err(e) = engine.stop().await
    {
        tracing::warn!(error = %e, "stopping the new PostgreSQL failed");
    }

    // 7. Marker, configuration, the old database kept.
    let kept = match (source_url_hint, keep) {
        (BackendMarker::Sqlite, Some(dest)) => {
            let db = layout.database_path();
            for suffix in ["", "-wal", "-shm"] {
                let from = PathBuf::from(format!("{}{suffix}", db.display()));
                if from.exists() {
                    let to = PathBuf::from(format!("{}{suffix}", dest.display()));
                    std::fs::rename(&from, &to)
                        .with_context(|| format!("cannot rename {} to {}", from.display(), to.display()))?;
                }
            }
            Some(dest)
        }
        (BackendMarker::PostgresManaged, Some(dest)) => {
            let pgdata = database::engine_layout(&layout).pgdata();
            std::fs::rename(&pgdata, &dest)
                .with_context(|| format!("cannot rename {} to {}", pgdata.display(), dest.display()))?;
            Some(dest)
        }
        _ => None,
    };
    database::write_marker(&layout, to)?;
    let config_note = write_backend(&loaded, to)?;
    drop(lock);

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "from": from.as_str(),
                "to": to.as_str(),
                "kept": kept,
                "config": config_note,
                "prepared": prepared,
                "report": report,
            }))?
        );
        return Ok(ExitCode::SUCCESS);
    }
    println!(
        "converted {} to {}: {} table(s), {} row(s), schema v{}; every table's row count and SHA-256 match",
        from.label(),
        to.label(),
        report.tables.len(),
        report.total_rows(),
        report.schema_version
    );
    println!(
        "  {} collection(s), {} managed index(es) recreated; {} identity sequence(s) set",
        report.collections, report.indexes, report.sequences
    );
    if let Some(p) = &prepared {
        if let Some(v) = p.vectors {
            println!("  vectors moved from pgvector to portable rows first: {v}");
        }
        if let Some(d) = p.deliveries {
            println!("  queue deliveries moved from pgmq to portable rows first: {d}");
        }
        if p.cron_released {
            println!("  schedules handed back from pg_cron to the ForgeBase tick");
        }
    }
    if let Some(k) = &kept {
        println!("  the old database is kept at {} (never deleted)", k.display());
    }
    println!("  {config_note}");
    for n in &report.notes {
        println!("  note: {n}");
    }
    if !report.manual.is_empty() {
        println!(
            "  {} object(s) made with raw SQL were not recreated (their SQL may not parse on {}); recreate them by hand:",
            report.manual.len(),
            to.label()
        );
        for m in &report.manual {
            println!("    {} {} on {}: {}", m.kind, m.name, m.table, m.sql);
        }
    }
    if to == BackendMarker::PostgresManaged || to == BackendMarker::PostgresExternal {
        println!(
            "  vectors, queue deliveries and schedules are portable rows; with [database.postgres.extensions] \
             in auto mode the next start moves them to pgvector, pgmq and pg_cron where available"
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// The steps that need the open source; on failure the target this run
/// made is removed.
async fn convert_open(
    config: &Config,
    layout: &DataLayout,
    source: &PrimaryDatabase,
    to: BackendMarker,
    args: &ConvertCli,
    say: impl Fn(&str) + Send + Sync,
) -> anyhow::Result<(
    forgebase_convert::ConvertReport,
    Option<forgebase_convert::Prepared>,
    Made,
)> {
    if !args.no_backup {
        say("taking a snapshot of the source");
        let opts = BackupOptions {
            label: Some("before db convert".into()),
            pg: database::pg_source(
                config,
                layout,
                source.marker(),
                source.engine().map(AsRef::as_ref),
                source.attached(),
            ),
            ..BackupOptions::default()
        };
        let m = SnapshotStore::new(layout)
            .create(source, layout, opts, None)
            .await
            .context("the snapshot before converting failed (use --no-backup to skip it)")?;
        say(&format!("snapshot {} written", m.id));
    }
    let report = source.migrate(Some(&layout.backups_dir())).await?;
    if !report.applied.is_empty() {
        say(&format!(
            "source migrated: {} migration(s) applied",
            report.applied.len()
        ));
    }
    let prepared = if to == BackendMarker::Sqlite && !args.no_prepare {
        let p = forgebase_convert::prepare_portable(source, &layout.indexes_dir()).await?;
        Some(p)
    } else {
        None
    };
    let source_conn = match source.marker() {
        BackendMarker::Sqlite => forgebase_convert::Conn::Sqlite(layout.database_path()),
        BackendMarker::PostgresManaged => {
            let engine = source
                .engine()
                .ok_or_else(|| anyhow!("the bundled PostgreSQL did not start for the conversion"))?;
            forgebase_convert::Conn::Postgres(SecretString::new(engine.url(Role::Super).expose()))
        }
        BackendMarker::PostgresExternal => forgebase_convert::Conn::Postgres(SecretString::new(external_url(config)?)),
    };

    say(&format!("creating the {} database", to.label()));
    let (target, target_conn, made) = open_target(config, layout, to).await?;
    let progress = |line: &str| say(line);
    let result = async {
        target.migrate(None).await?;
        let r = forgebase_convert::convert(
            forgebase_convert::Endpoint {
                db: source,
                conn: &source_conn,
            },
            forgebase_convert::Endpoint {
                db: &target,
                conn: &target_conn,
            },
            &progress,
        )
        .await?;
        anyhow::Ok(r)
    }
    .await;
    target.close().await;
    match result {
        Ok(r) => Ok((r, prepared, made)),
        Err(e) => {
            discard_target(made, config).await;
            Err(e)
        }
    }
}

/// `path`, or `path.<UTC time>` when `path` exists.
fn free_name(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let stamp = time::OffsetDateTime::now_utc().unix_timestamp();
    PathBuf::from(format!("{}.{stamp}", path.display()))
}

fn external_url(config: &Config) -> anyhow::Result<&str> {
    config
        .database
        .postgres
        .url
        .as_ref()
        .map(|u| u.expose())
        .ok_or_else(|| {
            anyhow!(
                "database.postgres.mode is external, but no server is named: set FORGEBASE_DATABASE_URL to an empty \
             database"
            )
        })
}

/// Refuses a target that is not empty before anything is created.
async fn preflight_target(config: &Config, layout: &DataLayout, to: BackendMarker) -> anyhow::Result<()> {
    match to {
        BackendMarker::Sqlite => {
            let path = layout.database_path();
            if path.exists() {
                bail!(
                    "{} already exists; move it away before converting to SQLite",
                    path.display()
                );
            }
        }
        BackendMarker::PostgresManaged => {
            if !database::bundled_engine_available(&config.database) {
                bail!("{}", database::NO_BUNDLED_ENGINE);
            }
            let pgdata = database::engine_layout(layout).pgdata();
            if pgdata.exists() {
                bail!(
                    "{} already exists; move it away before converting to PostgreSQL",
                    pgdata.display()
                );
            }
        }
        BackendMarker::PostgresExternal => {
            let url = external_url(config)?;
            let db = Database::open(
                DbOptions::from_config_target(&config.database, DbTarget::postgres(url, false)).create_if_missing(true),
            )
            .await?;
            let rows = forgebase_db::Backend::fetch_all(
                &db,
                &forgebase_db::Statement::raw(
                    "SELECT count(*)::bigint FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
                     WHERE n.nspname IN ('public', 'forgebase') AND c.relkind IN ('r', 'p', 'v', 'm')",
                ),
            )
            .await;
            db.close().await;
            let n = rows?.scalar().and_then(forgebase_db::Value::as_i64).unwrap_or(0);
            if n > 0 {
                bail!(
                    "the external database already has {n} table(s) or view(s) in schemas public and forgebase; \
                     convert into an empty database"
                );
            }
        }
    }
    Ok(())
}

/// Creates (managed: `initdb` and start) and opens the target.
async fn open_target(
    config: &Config,
    layout: &DataLayout,
    to: BackendMarker,
) -> anyhow::Result<(Database, forgebase_convert::Conn, Made)> {
    match to {
        BackendMarker::Sqlite => {
            let path = layout.database_path();
            let db = Database::open(DbOptions::from_config(&config.database, &path).create_if_missing(true)).await?;
            Ok((db, forgebase_convert::Conn::Sqlite(path.clone()), Made::Sqlite(path)))
        }
        BackendMarker::PostgresManaged => {
            let el = database::engine_layout(layout);
            let engine = Engine::prepare(&el, &database::engine_config(&config.database)?).await?;
            let opts = DbOptions::from_config_target(
                &config.database,
                DbTarget::postgres(engine.url(Role::Server).expose(), true),
            );
            let conn = forgebase_convert::Conn::Postgres(SecretString::new(engine.url(Role::Super).expose()));
            let made = Made::Managed {
                engine: Box::new(engine),
                pgdata: el.pgdata(),
            };
            match Database::open(opts).await {
                Ok(db) => Ok((db, conn, made)),
                Err(e) => {
                    discard_target(made, config).await;
                    Err(e.into())
                }
            }
        }
        BackendMarker::PostgresExternal => {
            let url = external_url(config)?;
            let db = Database::open(DbOptions::from_config_target(
                &config.database,
                DbTarget::postgres(url, false),
            ))
            .await?;
            Ok((
                db,
                forgebase_convert::Conn::Postgres(SecretString::new(url)),
                Made::External,
            ))
        }
    }
}

/// Removes a target this run made (it was empty before).
async fn discard_target(made: Made, config: &Config) {
    match made {
        Made::Sqlite(path) => {
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
            }
        }
        Made::Managed { engine, pgdata } => {
            let _ = engine.stop().await;
            let _ = std::fs::remove_dir_all(&pgdata);
        }
        Made::External => {
            // The database had no tables in public and forgebase before
            // (preflight): drop what this run made there.
            let Ok(url) = external_url(config) else {
                return;
            };
            let Ok(db) = Database::open(DbOptions::from_config_target(
                &config.database,
                DbTarget::postgres(url, false),
            ))
            .await
            else {
                return;
            };
            let sql = "DO $fb$ DECLARE r record; BEGIN \
                FOR r IN SELECT c.relname, c.relkind FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
                WHERE n.nspname = 'public' AND c.relkind IN ('r', 'p', 'v', 'm') LOOP \
                EXECUTE format(CASE WHEN r.relkind IN ('v', 'm') THEN 'DROP VIEW IF EXISTS public.%I CASCADE' \
                ELSE 'DROP TABLE IF EXISTS public.%I CASCADE' END, r.relname); END LOOP; \
                DROP SCHEMA IF EXISTS forgebase CASCADE; END $fb$";
            if let Ok(mut tx) = forgebase_db::Backend::begin(&db, forgebase_db::TxMode::Normal).await {
                let ok = tx.execute(&forgebase_db::Statement::raw(sql)).await.is_ok();
                let _ = if ok { tx.commit().await } else { tx.rollback().await };
            }
            db.close().await;
        }
    }
}

/// Sets `[database] backend` in the configuration file, keeping comments.
fn write_backend(loaded: &LoadedConfig, to: BackendMarker) -> anyhow::Result<String> {
    let backend = match to.backend() {
        DatabaseBackend::Sqlite => "sqlite",
        DatabaseBackend::Postgres => "postgres",
    };
    let env_overrides = std::env::var("FORGEBASE_DATABASE_BACKEND")
        .ok()
        .filter(|v| !v.trim().is_empty() && v.trim() != backend);
    let mut note = match &loaded.file {
        Some(file) => {
            let text = std::fs::read_to_string(file).with_context(|| format!("cannot read {}", file.display()))?;
            let updated =
                forgebase_core::features::set_keys(&text, &[("database.backend", toml_edit::Value::from(backend))])
                    .map_err(|e| anyhow!("cannot update {}: {e}", file.display()))?;
            forgebase_core::features::write_atomic(file, &updated)
                .with_context(|| format!("cannot write {}", file.display()))?;
            format!("{} now says [database] backend = \"{backend}\"", file.display())
        }
        None => format!(
            "no configuration file: the backend marker forgebase.backend says {}",
            to.as_str()
        ),
    };
    if let Some(v) = env_overrides {
        note.push_str(&format!(
            "; FORGEBASE_DATABASE_BACKEND={v} in the environment contradicts it: change it to {backend}"
        ));
    }
    Ok(note)
}
