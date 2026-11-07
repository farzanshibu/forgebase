//! `init`, `serve`, `migrate`, `status` and `version`.

use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, bail};
use forgebase_core::config::{CONFIG_TEMPLATE, normalize};
use forgebase_core::{DataLayout, build};
use forgebase_db::{Database, DbOptions};
use forgebase_server::{DataDirLock, ServerError};
use serde_json::json;

use crate::GlobalArgs;
use crate::probe;

/// Logging for one-shot commands: quiet unless asked.
fn quiet_logging(g: &GlobalArgs) -> anyhow::Result<()> {
    let level = g.log_level.as_deref().unwrap_or("warn");
    forgebase_observability::init_logging(level, g.log_format.unwrap_or(forgebase_core::LogFormat::Pretty))?;
    Ok(())
}

/// `forgebase init [DIR]`.
pub async fn init(g: &GlobalArgs, dir: &Path) -> anyhow::Result<ExitCode> {
    quiet_logging(g)?;
    let cwd = std::env::current_dir()?;
    let project = normalize(&cwd.join(dir));
    std::fs::create_dir_all(&project).with_context(|| format!("cannot create {}", project.display()))?;

    let config_path = match &g.config {
        Some(p) => normalize(&cwd.join(p)),
        None => project.join(forgebase_core::config::DEFAULT_CONFIG_FILE),
    };
    let created_config = !config_path.exists();
    if created_config {
        write_private(&config_path, CONFIG_TEMPLATE)
            .with_context(|| format!("cannot write {}", config_path.display()))?;
    }

    let mut cli = g.overrides();
    cli.config_file = Some(config_path.clone());
    let config = g.load_with(cli)?.config;
    let layout = DataLayout::new(&config.data_dir);
    layout
        .ensure()
        .with_context(|| format!("cannot create data directory {}", layout.root().display()))?;
    let _lock = DataDirLock::acquire(&layout).map_err(running_hint)?;

    let db = Database::open(DbOptions::from_config(&config.database, layout.database_path())).await?;
    let report = db.migrate(Some(&layout.backups_dir())).await;
    let result = async {
        let report = report?;
        let instance = db.ensure_instance().await?;
        let version = db.schema_version().await?;
        db.checkpoint().await?;
        anyhow::Ok((report, instance, version))
    }
    .await;
    db.close().await;
    let (report, instance, version) = result?;

    println!("Initialized ForgeBase project in {}", project.display());
    println!(
        "  config:    {} ({})",
        config_path.display(),
        if created_config { "created" } else { "existing, kept" }
    );
    println!("  data dir:  {}", layout.root().display());
    println!(
        "  database:  {} (schema v{version}, {} migration(s) applied now)",
        layout.database_path().display(),
        report.applied.len()
    );
    println!("  instance:  {}", instance.instance_id);
    println!();
    let rel = dir.display().to_string();
    if rel == "." {
        println!("Next: forgebase serve");
    } else {
        println!("Next: cd {rel} && forgebase serve");
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(unix)]
fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(contents.as_bytes())?;
    f.sync_all()
}

#[cfg(not(unix))]
fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    std::fs::write(path, contents)
}

fn running_hint(e: ServerError) -> anyhow::Error {
    match e {
        ServerError::Locked { .. } => anyhow::anyhow!("{e}; stop the server first (it applies migrations at startup)"),
        other => other.into(),
    }
}

/// `forgebase serve`.
pub async fn serve(g: &GlobalArgs) -> anyhow::Result<ExitCode> {
    let loaded = g.load()?;
    let config = loaded.config;
    forgebase_observability::init_logging(&config.log.level, config.log.format)?;
    match &loaded.file {
        Some(f) => tracing::info!(config = %f.display(), "configuration loaded"),
        None => tracing::info!("no config file; using defaults, environment and flags"),
    }
    let server = forgebase_server::start(&config).await?;
    let addr = server.local_addr();
    println!("ForgeBase {} listening on http://{addr}", build::VERSION);
    println!("  admin:     http://{addr}/admin/");
    println!("  health:    http://{addr}/health");
    println!("  data dir:  {}", config.data_dir.display());
    server.run_until_signal().await?;
    Ok(ExitCode::SUCCESS)
}

/// `forgebase migrate [--status]`.
pub async fn migrate(g: &GlobalArgs, status_only: bool) -> anyhow::Result<ExitCode> {
    quiet_logging(g)?;
    let config = g.load()?.config;
    let layout = DataLayout::new(&config.data_dir);
    if !layout.is_initialized() {
        bail!(
            "no database at {} (run `forgebase init`)",
            layout.database_path().display()
        );
    }

    if status_only {
        let db = Database::open(DbOptions::from_config(&config.database, layout.database_path()).read_only()).await?;
        let status = db.migration_status().await;
        db.close().await;
        let status = status?;
        for a in &status.applied {
            println!(
                "applied  {:04}_{}  {}  {}ms",
                a.version, a.name, a.applied_at, a.execution_ms
            );
        }
        for p in &status.pending {
            println!("pending  {:04}_{}", p.version, p.name);
        }
        for p in &status.problems {
            println!("problem  {p:?}");
        }
        println!(
            "schema v{} of v{}{}",
            status.current_version,
            status.latest_version,
            if status.is_up_to_date() { " (up to date)" } else { "" }
        );
        return Ok(if status.problems.is_empty() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        });
    }

    let _lock = DataDirLock::acquire(&layout).map_err(running_hint)?;
    let db = Database::open(DbOptions::from_config(&config.database, layout.database_path()).create_if_missing(false))
        .await?;
    let result = async {
        let report = db.migrate(Some(&layout.backups_dir())).await?;
        let version = db.schema_version().await?;
        db.checkpoint().await?;
        anyhow::Ok((report, version))
    }
    .await;
    db.close().await;
    let (report, version) = result?;
    if let Some(b) = &report.backup {
        println!("snapshot  {}", b.display());
    }
    for a in &report.applied {
        println!("applied   {:04}_{} ({}ms)", a.version, a.name, a.execution_ms);
    }
    if report.applied.is_empty() {
        println!("schema v{version}: already up to date");
    } else {
        println!("schema v{version}");
    }
    Ok(ExitCode::SUCCESS)
}

/// `forgebase status [--json]`.
pub async fn status(g: &GlobalArgs, as_json: bool) -> anyhow::Result<ExitCode> {
    quiet_logging(g)?;
    let loaded = g.load()?;
    let config = &loaded.config;
    let layout = DataLayout::new(&config.data_dir);

    let mut database = json!({
        "path": layout.database_path(),
        "exists": layout.is_initialized(),
    });
    let mut instance_id = None;
    if layout.is_initialized() {
        match Database::open(DbOptions::from_config(&config.database, layout.database_path()).read_only()).await {
            Ok(db) => {
                if let Ok(info) = db.info().await {
                    database["size_bytes"] = json!(info.size_bytes);
                    database["wal_size_bytes"] = json!(info.wal_size_bytes);
                    database["journal_mode"] = json!(info.journal_mode);
                }
                if let Ok(s) = db.migration_status().await {
                    database["schema_version"] = json!(s.current_version);
                    database["latest_schema_version"] = json!(s.latest_version);
                    database["pending_migrations"] = json!(s.pending.len());
                    database["migration_problems"] = json!(s.problems.len());
                }
                instance_id = db.meta_get("instance_id").await.ok().flatten();
                db.close().await;
            }
            Err(e) => database["error"] = json!(e.to_string()),
        }
    }

    let owner = DataDirLock::probe(&layout);
    let dial = match &owner {
        Some(Some(o)) => o.addr.clone(),
        _ => probe::dial_addr(&config.server.host, config.server.port),
    };
    let healthy = probe::is_forgebase(&dial).await;
    let server = json!({
        "running": owner.is_some(),
        "pid": owner.as_ref().and_then(|o| o.as_ref().map(|o| o.pid)),
        "address": dial,
        "healthy": healthy,
    });

    let out = json!({
        "version": build::VERSION,
        "config_file": loaded.file,
        "data_dir": layout.root(),
        "data_dir_exists": layout.root().is_dir(),
        "instance_id": instance_id,
        "database": database,
        "server": server,
    });
    if as_json {
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(ExitCode::SUCCESS);
    }

    println!("ForgeBase {}", build::VERSION);
    println!(
        "  config:    {}",
        loaded
            .file
            .as_ref()
            .map_or("none (defaults, environment, flags)".to_owned(), |f| f
                .display()
                .to_string())
    );
    println!(
        "  data dir:  {}{}",
        layout.root().display(),
        if layout.root().is_dir() { "" } else { " (missing)" }
    );
    if !layout.is_initialized() {
        println!("  database:  not initialized (run `forgebase init`)");
    } else if let Some(err) = out["database"]["error"].as_str() {
        println!("  database:  ERROR {err}");
    } else {
        let d = &out["database"];
        let pending = d["pending_migrations"].as_u64().unwrap_or(0);
        println!(
            "  database:  {} ({} bytes, {}, schema v{} of v{}{})",
            layout.database_path().display(),
            d["size_bytes"],
            d["journal_mode"].as_str().unwrap_or("?"),
            d["schema_version"],
            d["latest_schema_version"],
            if pending > 0 {
                format!(", {pending} pending")
            } else {
                String::new()
            }
        );
    }
    if let Some(id) = &out["instance_id"].as_str() {
        println!("  instance:  {id}");
    }
    match (&owner, healthy) {
        (Some(o), true) => println!(
            "  server:    running at http://{dial}{} (healthy)",
            o.as_ref().map(|o| format!(", pid {}", o.pid)).unwrap_or_default()
        ),
        (Some(_), false) => println!("  server:    running (data dir locked) but http://{dial}/health does not answer"),
        (None, true) => println!("  server:    another ForgeBase answers at http://{dial} (different data dir)"),
        (None, false) => println!("  server:    stopped"),
    }
    Ok(ExitCode::SUCCESS)
}

/// `forgebase version [--json]`.
pub async fn version(as_json: bool) -> anyhow::Result<ExitCode> {
    let sqlite = forgebase_db::sqlite_library_version()
        .await
        .unwrap_or_else(|_| "unknown".into());
    let dashboard = forgebase_api::admin::dashboard_embedded();
    if as_json {
        println!(
            "{}",
            json!({
                "name": build::NAME,
                "version": build::VERSION,
                "os": build::OS,
                "arch": build::ARCH,
                "sqlite": sqlite,
                "dashboard_embedded": dashboard,
            })
        );
    } else {
        println!(
            "{} {} ({}/{}, sqlite {}, dashboard {})",
            build::NAME,
            build::VERSION,
            build::OS,
            build::ARCH,
            sqlite,
            if dashboard { "embedded" } else { "not embedded" }
        );
    }
    Ok(ExitCode::SUCCESS)
}
