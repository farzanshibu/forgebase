//! `init`, `serve`, `migrate`, `status` and `version`.

use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, bail};
use forgebase_core::config::{
    BUNDLED_POSTGRES, CONFIG_TEMPLATE, DEFAULT_BACKEND, DatabaseBackend, ENV_DATABASE_URL, PostgresMode,
    PostgresOverrides, normalize,
};
use forgebase_core::{DataLayout, LoadedConfig, build};
use forgebase_server::database::{self, Access, BackendMarker, Create, PrimaryDatabase};
use forgebase_server::{DataDirLock, ServerError};
use serde_json::json;

use crate::GlobalArgs;
use crate::probe;

/// Logging for one-shot commands: quiet unless asked.
pub(crate) fn quiet_logging(g: &GlobalArgs) -> anyhow::Result<()> {
    // The bundled engine's own log (target forgebase::postgres) stays out of
    // one-off commands: a failure to start is reported with its last lines.
    // So does sqlx's slow-statement warning, which prints the whole statement
    // (the PostgreSQL baseline migration is one).
    let level = g
        .log_level
        .as_deref()
        .unwrap_or("warn,forgebase::postgres=off,sqlx::query=error");
    forgebase_observability::init_logging(level, g.log_format.unwrap_or(forgebase_core::LogFormat::Pretty))?;
    Ok(())
}

/// Whether the loaded configuration states the backend (FB-148): the
/// backend marker wins over a default, never over an explicit setting.
pub(crate) fn explicit_backend(loaded: &LoadedConfig) -> bool {
    database::backend_is_explicit(&loaded.sources)
}

/// Opens the data directory's database for a one-off command (see
/// [`database::open_attached`]): the running server's engine, or the bundled
/// engine started for the command.
pub(crate) async fn open_attached(loaded: &LoadedConfig, access: Access) -> anyhow::Result<PrimaryDatabase> {
    let layout = DataLayout::new(&loaded.config.data_dir);
    database::open_attached(&loaded.config, explicit_backend(loaded), &layout, access)
        .await
        .map_err(|e| match e {
            ServerError::NotInitialized(_) => {
                anyhow::anyhow!("no database in {} (run `forgebase init`)", layout.root().display())
            }
            other => other.into(),
        })
}

/// `--database` and `--database-url` of `forgebase init` (ADR-0028 §6.2,
/// ADR-0029 §6).
#[derive(Debug, Clone, Default, clap::Args)]
pub struct InitDatabaseArgs {
    /// Database engine: postgres (the bundled PostgreSQL server; the default
    /// of the standard release) or sqlite (one file; the default of the lite
    /// release). On a terminal without this flag, init asks.
    #[arg(long, value_name = "ENGINE", value_parser = ["postgres", "sqlite"])]
    pub database: Option<String>,
    /// Use an existing PostgreSQL server instead of the bundled one. The URL
    /// is written to the project's .env (FORGEBASE_DATABASE_URL), not to
    /// forgebase.toml.
    #[arg(long, value_name = "URL", conflicts_with = "database")]
    pub database_url: Option<String>,
}

/// The backend `init` sets up.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Choice {
    Sqlite,
    Managed,
    External(String),
}

impl Choice {
    fn marker(&self) -> BackendMarker {
        match self {
            Choice::Sqlite => BackendMarker::Sqlite,
            Choice::Managed => BackendMarker::PostgresManaged,
            Choice::External(_) => BackendMarker::PostgresExternal,
        }
    }

    fn from_marker(m: BackendMarker) -> Self {
        match m {
            BackendMarker::Sqlite => Choice::Sqlite,
            BackendMarker::PostgresManaged => Choice::Managed,
            // The URL comes from the environment or .env, as before.
            BackendMarker::PostgresExternal => Choice::External(String::new()),
        }
    }

    /// The layer that makes this run use the choice.
    fn overrides(&self) -> (Option<DatabaseBackend>, Option<PostgresOverrides>) {
        match self {
            Choice::Sqlite => (Some(DatabaseBackend::Sqlite), None),
            Choice::Managed => (
                Some(DatabaseBackend::Postgres),
                Some(PostgresOverrides {
                    mode: Some(PostgresMode::Managed),
                    ..PostgresOverrides::default()
                }),
            ),
            Choice::External(url) => (
                Some(DatabaseBackend::Postgres),
                Some(PostgresOverrides {
                    mode: Some(PostgresMode::External),
                    url: (!url.is_empty()).then(|| url.clone()),
                    ..PostgresOverrides::default()
                }),
            ),
        }
    }
}

/// The terminal question of `init` (ADR-0029 §6). `None` = the default.
pub fn prompt_database(input: &mut dyn BufRead, out: &mut dyn Write) -> anyhow::Result<DatabaseBackend> {
    let (first, second) = if DEFAULT_BACKEND == DatabaseBackend::Postgres {
        (
            ("PostgreSQL (default, bundled server)", DatabaseBackend::Postgres),
            ("SQLite (one file)", DatabaseBackend::Sqlite),
        )
    } else {
        (
            ("SQLite (default, one file)", DatabaseBackend::Sqlite),
            ("PostgreSQL (bundled server)", DatabaseBackend::Postgres),
        )
    };
    for _ in 0..3 {
        write!(out, "Database: [1] {} [2] {} ", first.0, second.0)?;
        out.flush()?;
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            return Ok(first.1);
        }
        match line.trim().to_ascii_lowercase().as_str() {
            "" | "1" => return Ok(first.1),
            "2" => return Ok(second.1),
            "postgres" | "postgresql" => return Ok(DatabaseBackend::Postgres),
            "sqlite" => return Ok(DatabaseBackend::Sqlite),
            _ => writeln!(out, "Enter 1 or 2.")?,
        }
    }
    bail!("no database chosen")
}

/// The config file `init` writes: the template with `[database] backend`
/// (and, for an external server, `mode`) set.
fn config_text(choice: &Choice) -> String {
    let backend = match choice {
        Choice::Sqlite => "sqlite",
        Choice::Managed | Choice::External(_) => "postgres",
    };
    let mut text = CONFIG_TEMPLATE.replacen(
        "# backend = \"postgres\"      # FORGEBASE_DATABASE_BACKEND",
        &format!(
            "{:<27} # FORGEBASE_DATABASE_BACKEND",
            format!("backend = \"{backend}\"")
        ),
        1,
    );
    if matches!(choice, Choice::External(_)) {
        text = text
            .replacen("# [database.postgres]\n", "[database.postgres]\n", 1)
            .replacen(
                "# mode = \"managed\"          # FORGEBASE_DATABASE_POSTGRES_MODE",
                "mode = \"external\"          # FORGEBASE_DATABASE_POSTGRES_MODE (URL in .env)",
                1,
            );
    }
    text
}

/// Adds `FORGEBASE_DATABASE_URL` to `dir/.env` (created 0600). Refuses a
/// different URL already there.
fn write_dotenv_url(dir: &Path, url: &str) -> anyhow::Result<std::path::PathBuf> {
    let path = dir.join(crate::dotenv::DOTENV_FILE);
    let existing = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    };
    let vars = dotenvy::from_read_iter(existing.as_bytes()).collect::<Result<Vec<_>, _>>();
    if let Ok(vars) = vars
        && let Some((_, old)) = vars.iter().find(|(k, _)| k == ENV_DATABASE_URL)
    {
        if old == url {
            return Ok(path);
        }
        bail!(
            "{} already sets {ENV_DATABASE_URL} to another server; edit it there",
            path.display()
        );
    }
    let quoted = if url.contains('\'') {
        format!("\"{}\"", url.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        format!("'{url}'")
    };
    let mut text = existing;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&format!(
        "# External PostgreSQL server (forgebase init --database-url). Keep this file private.\n{ENV_DATABASE_URL}={quoted}\n"
    ));
    forgebase_core::features::write_atomic(&path, &text).with_context(|| format!("cannot write {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("cannot chmod {}", path.display()))?;
    }
    Ok(path)
}

/// `forgebase init [DIR]`.
pub async fn init(
    g: &GlobalArgs,
    dir: &Path,
    admin: &crate::admin::InitAdminArgs,
    dbargs: &InitDatabaseArgs,
) -> anyhow::Result<ExitCode> {
    quiet_logging(g)?;
    // `--admin-password-stdin`: read and check it before touching the disk.
    let preset = crate::admin::preset_admin(admin)?;
    let cwd = std::env::current_dir()?;
    let project = normalize(&cwd.join(dir));

    let config_path = match &g.config {
        Some(p) => normalize(&cwd.join(p)),
        None => project.join(forgebase_core::config::DEFAULT_CONFIG_FILE),
    };
    // What exists already decides, unless a flag says otherwise.
    let mut probe_cli = g.overrides();
    probe_cli.config_file = Some(config_path.clone());
    let existing_config = if config_path.is_file() {
        Some(g.load_with(probe_cli)?)
    } else {
        None
    };
    let data_dir = match &existing_config {
        Some(loaded) => loaded.config.data_dir.clone(),
        None => g.data_dir.as_ref().map_or_else(
            || project.join(forgebase_core::config::DEFAULT_DATA_DIR),
            |d| normalize(&cwd.join(d)),
        ),
    };
    let on_disk = database::existing(&DataLayout::new(&data_dir))?;
    let configured = existing_config
        .as_ref()
        .filter(|l| explicit_backend(l))
        .map(|l| BackendMarker::of(&l.config.database));
    let choice = match (&dbargs.database, &dbargs.database_url) {
        (_, Some(url)) => Choice::External(url.clone()),
        (Some(d), None) if d == "sqlite" => Choice::Sqlite,
        (Some(_), None) => Choice::Managed,
        (None, None) => match on_disk.or(configured) {
            Some(m) => Choice::from_marker(m),
            None if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() => {
                let stdin = std::io::stdin();
                match prompt_database(&mut stdin.lock(), &mut std::io::stdout())? {
                    DatabaseBackend::Sqlite => Choice::Sqlite,
                    DatabaseBackend::Postgres => Choice::Managed,
                }
            }
            None => match DEFAULT_BACKEND {
                DatabaseBackend::Sqlite => Choice::Sqlite,
                DatabaseBackend::Postgres => Choice::Managed,
            },
        },
    };
    if let Some(found) = on_disk
        && found != choice.marker()
    {
        bail!(
            "{} already holds {}; init does not change the database engine (use forgebase db convert)",
            data_dir.display(),
            found.describe()
        );
    }
    if let Some(found) = configured
        && found != choice.marker()
    {
        bail!(
            "{} sets the database to {}; edit it or drop the --database flag",
            config_path.display(),
            found.as_str()
        );
    }
    if choice == Choice::Managed && !BUNDLED_POSTGRES {
        // A development build may still run an engine from FORGEBASE_PG_ENGINE_DIR.
        let probe = existing_config.as_ref().map_or_else(
            || forgebase_core::Config::defaults(&project).database,
            |l| l.config.database.clone(),
        );
        if !database::bundled_engine_available(&probe) {
            bail!("{}", database::NO_BUNDLED_ENGINE);
        }
    }

    std::fs::create_dir_all(&project).with_context(|| format!("cannot create {}", project.display()))?;
    let created_config = !config_path.exists();
    if created_config {
        write_private(&config_path, &config_text(&choice))
            .with_context(|| format!("cannot write {}", config_path.display()))?;
    }
    let dotenv = match &choice {
        Choice::External(url) if !url.is_empty() => Some(write_dotenv_url(&project, url)?),
        _ => None,
    };

    let mut cli = g.overrides();
    cli.config_file = Some(config_path.clone());
    let (backend, postgres) = choice.overrides();
    cli.database_backend = backend;
    cli.postgres = postgres;
    if let Choice::External(url) = &choice
        && !url.is_empty()
    {
        cli.secrets_set.push(ENV_DATABASE_URL.to_owned());
    }
    let config = g.load_with(cli)?.config;
    let layout = DataLayout::new(&config.data_dir);
    layout
        .ensure()
        .with_context(|| format!("cannot create data directory {}", layout.root().display()))?;
    let _lock = DataDirLock::acquire(&layout).map_err(running_hint)?;

    let primary = match database::open_owned(&config, true, &layout, Create::Yes).await {
        Ok(p) => p,
        Err(ServerError::Engine(forgebase_pgengine::EngineError::Root)) => {
            bail!("PostgreSQL cannot run as root: run as an unprivileged user, or use --database sqlite")
        }
        Err(e) => return Err(e.into()),
    };
    let db = primary.db().clone();
    let result = async {
        let report = db.migrate(Some(&layout.backups_dir())).await?;
        let instance = db.ensure_instance().await?;
        let version = db.schema_version().await?;
        database::checkpoint(&db).await?;
        // FB-90: the first administrator (may prompt on a terminal), while
        // the engine still runs.
        let admin = crate::admin::init_admin(&config, &db, admin, preset).await?;
        anyhow::Ok((report, instance, version, admin))
    }
    .await;
    let where_ = database_line(&primary, &layout);
    primary.close().await;
    let (report, instance, version, admin) = result?;

    println!("Initialized ForgeBase project in {}", project.display());
    println!(
        "  config:    {} ({})",
        config_path.display(),
        if created_config { "created" } else { "existing, kept" }
    );
    println!("  data dir:  {}", layout.root().display());
    println!(
        "  database:  {where_} (schema v{version}, {} migration(s) applied now)",
        report.applied.len()
    );
    if let Some(p) = &dotenv {
        println!("  server:    URL saved in {} ({ENV_DATABASE_URL})", p.display());
    }
    println!("  instance:  {}", instance.instance_id);
    println!("  admin:     {}", admin.describe());
    println!();
    let rel = dir.display().to_string();
    if rel == "." {
        println!("Next: forgebase serve");
    } else {
        println!("Next: cd {rel} && forgebase serve");
    }
    Ok(ExitCode::SUCCESS)
}

/// One line naming the database: the SQLite file, the bundled engine and its
/// cluster, or the external server.
pub(crate) fn database_line(primary: &PrimaryDatabase, layout: &DataLayout) -> String {
    match primary.marker() {
        BackendMarker::Sqlite => layout.database_path().display().to_string(),
        BackendMarker::PostgresManaged => {
            let version = primary
                .engine()
                .map(|e| format!(" {}", e.version().text))
                .unwrap_or_default();
            format!(
                "PostgreSQL{version} (bundled) in {}",
                database::engine_layout(layout).pgdata().display()
            )
        }
        BackendMarker::PostgresExternal => format!("PostgreSQL (external) at {}", primary.db().location()),
    }
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

pub(crate) fn running_hint(e: ServerError) -> anyhow::Error {
    match e {
        ServerError::Locked { .. } => anyhow::anyhow!("{e}; stop the server first (it applies migrations at startup)"),
        other => other.into(),
    }
}

/// `forgebase serve`.
pub async fn serve(g: &GlobalArgs) -> anyhow::Result<ExitCode> {
    let loaded = g.load()?;
    let config = &loaded.config;
    forgebase_observability::init_logging(&config.log.level, config.log.format)?;
    match &loaded.file {
        Some(f) => tracing::info!(config = %f.display(), "configuration loaded"),
        None => tracing::info!("no config file; using defaults, environment and flags"),
    }
    // FB-91: with the loader's layers, so the feature registry knows each setting's source.
    let server = forgebase_server::start_loaded(&loaded).await?;
    let addr = server.local_addr();
    println!("ForgeBase {} listening on http://{addr}", build::VERSION);
    println!("  admin:     http://{addr}/admin/");
    println!("  health:    http://{addr}/health");
    if let Some(resp) = server.resp_addr() {
        println!("  redis:     redis://{resp} (RESP2/RESP3; AUTH with an API key or personal access token)");
    }
    println!("  data dir:  {}", config.data_dir.display());
    match (server.backend(), server.engine()) {
        (BackendMarker::PostgresManaged, Some(e)) => println!(
            "  database:  PostgreSQL {} (bundled; socket {}{})",
            e.version().text,
            e.socket_dir().map_or_else(String::new, |d| d.display().to_string()),
            e.tcp_addr().map(|a| format!(", TCP {a}")).unwrap_or_default()
        ),
        (BackendMarker::PostgresExternal, _) => {
            println!("  database:  PostgreSQL (external) at {}", server.db().location())
        }
        _ => println!(
            "  database:  SQLite {}",
            DataLayout::new(&config.data_dir).database_path().display()
        ),
    }
    if let Some(token) = server.setup_token() {
        // FB-90: standard output only, never the log.
        println!();
        // The configured public URL when set (FB-91 server.public_url), otherwise the bound address.
        let base = config
            .server
            .public_url
            .clone()
            .unwrap_or_else(|| format!("http://{addr}"));
        println!("First run: create the administrator at {}", setup_link(&base, token));
    }
    server.run_until_signal().await?;
    Ok(ExitCode::SUCCESS)
}

/// The first-run setup link: the dashboard's setup page with the token.
fn setup_link(base: &str, token: &str) -> String {
    format!("{}/admin/setup?token={token}", base.trim_end_matches('/'))
}

/// `forgebase migrate [--status]`.
pub async fn migrate(g: &GlobalArgs, status_only: bool) -> anyhow::Result<ExitCode> {
    quiet_logging(g)?;
    let loaded = g.load()?;
    let config = &loaded.config;
    let layout = DataLayout::new(&config.data_dir);
    if !database::is_initialized(&layout) {
        bail!("no database in {} (run `forgebase init`)", layout.root().display());
    }

    if status_only {
        let primary = open_attached(&loaded, Access::Read).await?;
        let status = primary.db().migration_status().await;
        primary.close().await;
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
    let primary = database::open_owned(config, explicit_backend(&loaded), &layout, Create::No).await?;
    let db = primary.db().clone();
    let result = async {
        let report = db.migrate(Some(&layout.backups_dir())).await?;
        let version = db.schema_version().await?;
        database::checkpoint(&db).await?;
        anyhow::Ok((report, version))
    }
    .await;
    primary.close().await;
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

/// The one-line notice for SQLite projects once PostgreSQL is the default
/// (ADR-0029 §6).
pub(crate) fn sqlite_notice(marker: Option<BackendMarker>) -> Option<&'static str> {
    (marker == Some(BackendMarker::Sqlite) && DEFAULT_BACKEND == DatabaseBackend::Postgres).then_some(
        "this project uses SQLite; PostgreSQL is the default now: `forgebase db convert --to postgres` moves it",
    )
}

/// `forgebase status [--json]`.
#[allow(clippy::too_many_lines)]
pub async fn status(g: &GlobalArgs, as_json: bool) -> anyhow::Result<ExitCode> {
    quiet_logging(g)?;
    let loaded = g.load()?;
    let config = &loaded.config;
    let layout = DataLayout::new(&config.data_dir);

    // Before opening: the bundled engine may be started under the lock below.
    let owner = DataDirLock::probe(&layout);
    let marker = database::resolve(config, explicit_backend(&loaded), &layout);
    let initialized = database::is_initialized(&layout);
    let mut database = json!({
        "backend": marker.as_ref().ok().map(|m| m.backend().as_str()),
        "mode": marker.as_ref().ok().and_then(|m| m.mode()).map(PostgresMode::as_str),
        "path": layout.database_path(),
        "exists": initialized,
    });
    let mut instance_id = None;
    let mut setup_required: Option<bool> = None;
    let mut where_ = None;
    if let Err(e) = &marker {
        database["error"] = json!(e.to_string());
    } else if initialized {
        match open_attached(&loaded, Access::Read).await {
            Ok(primary) => {
                where_ = Some(database_line(&primary, &layout));
                let db = primary.db();
                if primary.marker() != BackendMarker::Sqlite {
                    database["path"] = match primary.marker() {
                        BackendMarker::PostgresManaged => json!(database::engine_layout(&layout).pgdata()),
                        _ => json!(db.location()),
                    };
                }
                if let Some((dir, port)) = primary.socket() {
                    database["socket_dir"] = json!(dir);
                    database["port"] = json!(port);
                    database["engine_started_by"] = json!(if primary.engine().is_some() {
                        "this command"
                    } else {
                        "the server"
                    });
                }
                if let Some(e) = primary.engine() {
                    database["engine_version"] = json!(e.version().text);
                }
                if let Ok(info) = db.info().await {
                    database["size_bytes"] = json!(info.size_bytes);
                    database["wal_size_bytes"] = json!(info.wal_size_bytes);
                    database["journal_mode"] = json!(info.journal_mode);
                    database["server_version"] = json!(info.server_version);
                    if let Some(c) = info.connections {
                        database["connections"] = json!(c);
                    }
                }
                if let Ok(s) = db.migration_status().await {
                    database["schema_version"] = json!(s.current_version);
                    database["latest_schema_version"] = json!(s.latest_version);
                    database["pending_migrations"] = json!(s.pending.len());
                    database["migration_problems"] = json!(s.problems.len());
                }
                instance_id = db.meta_get("instance_id").await.ok().flatten();
                // FB-90: setup mode while no administrator exists.
                let backend: std::sync::Arc<dyn forgebase_db::Backend> = std::sync::Arc::new(db.clone());
                setup_required = forgebase_auth::setup::admin_exists(&backend)
                    .await
                    .ok()
                    .map(|exists| !exists);
                primary.close().await;
            }
            Err(e) => database["error"] = json!(format!("{e:#}")),
        }
    }
    let notice = sqlite_notice(marker.as_ref().ok().copied().filter(|_| initialized));
    if let Some(n) = notice {
        database["notice"] = json!(n);
    }

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
        "bundled_postgres": BUNDLED_POSTGRES,
        "config_file": loaded.file,
        "data_dir": layout.root(),
        "data_dir_exists": layout.root().is_dir(),
        "instance_id": instance_id,
        "database": database,
        "setup_required": setup_required,
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
    if let Some(err) = out["database"]["error"].as_str() {
        println!("  database:  ERROR {err}");
    } else if !initialized {
        println!("  database:  not initialized (run `forgebase init`)");
    } else {
        let d = &out["database"];
        let pending = d["pending_migrations"].as_u64().unwrap_or(0);
        let detail = if d["backend"] == "sqlite" {
            format!(
                "{} bytes, {}",
                d["size_bytes"],
                d["journal_mode"].as_str().unwrap_or("?")
            )
        } else {
            format!("{} bytes", d["size_bytes"])
        };
        println!(
            "  database:  {} ({detail}, schema v{} of v{}{})",
            where_.unwrap_or_default(),
            d["schema_version"],
            d["latest_schema_version"],
            if pending > 0 {
                format!(", {pending} pending")
            } else {
                String::new()
            }
        );
        if let Some(dir) = d["socket_dir"].as_str() {
            println!(
                "  engine:    socket {dir} (port {}), started by {}",
                d["port"],
                d["engine_started_by"].as_str().unwrap_or("?")
            );
        }
    }
    if let Some(n) = notice {
        println!("  notice:    {n}");
    }
    if let Some(id) = &out["instance_id"].as_str() {
        println!("  instance:  {id}");
    }
    match setup_required {
        Some(true) => println!("  setup:     pending (no administrator)"),
        Some(false) => println!("  setup:     complete"),
        None => {}
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
                "bundled_postgres": BUNDLED_POSTGRES,
                "default_backend": DEFAULT_BACKEND.as_str(),
                "dashboard_embedded": dashboard,
            })
        );
    } else {
        println!(
            "{} {} ({}/{}, sqlite {}, {}, dashboard {})",
            build::NAME,
            build::VERSION,
            build::OS,
            build::ARCH,
            sqlite,
            if BUNDLED_POSTGRES {
                "bundled PostgreSQL"
            } else {
                "lite: no bundled PostgreSQL"
            },
            if dashboard { "embedded" } else { "not embedded" }
        );
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use forgebase_core::config::ConfigOverrides;

    #[test]
    fn config_text_sets_the_backend_and_parses() {
        for (choice, backend) in [
            (Choice::Sqlite, DatabaseBackend::Sqlite),
            (Choice::Managed, DatabaseBackend::Postgres),
            (
                Choice::External("postgres://u:p@h/db".into()),
                DatabaseBackend::Postgres,
            ),
        ] {
            let text = config_text(&choice);
            let tmp = tempfile::tempdir().unwrap();
            std::fs::write(tmp.path().join("forgebase.toml"), &text).unwrap();
            let loaded = forgebase_core::config::ConfigLoader::new(tmp.path()).load().unwrap();
            assert_eq!(loaded.config.database.backend, backend, "{text}");
            assert!(explicit_backend(&loaded));
            let mode = loaded.config.database.postgres.mode;
            assert_eq!(mode == PostgresMode::External, matches!(choice, Choice::External(_)));
            assert!(!text.contains("postgres://"), "the URL never goes to forgebase.toml");
        }
    }

    #[test]
    fn prompt_offers_the_default_first() {
        let mut out = Vec::new();
        let got = prompt_database(&mut "\n".as_bytes(), &mut out).unwrap();
        assert_eq!(got, DEFAULT_BACKEND);
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("Database: [1] "), "{text}");
        assert!(text.contains("(default"), "{text}");
        let mut out = Vec::new();
        assert_eq!(
            prompt_database(&mut "x\nsqlite\n".as_bytes(), &mut out).unwrap(),
            DatabaseBackend::Sqlite
        );
        assert!(prompt_database(&mut "x\ny\nz\n".as_bytes(), &mut Vec::new()).is_err());
    }

    #[test]
    fn dotenv_gets_the_url_once() {
        let tmp = tempfile::tempdir().unwrap();
        let url = "postgres://forgebase:s#cr'et@db.example.com/forgebase";
        let path = write_dotenv_url(tmp.path(), url).unwrap();
        write_dotenv_url(tmp.path(), url).unwrap();
        let vars: Vec<(String, String)> = dotenvy::from_path_iter(&path).unwrap().map(Result::unwrap).collect();
        assert_eq!(vars, [(ENV_DATABASE_URL.to_owned(), url.to_owned())]);
        assert!(write_dotenv_url(tmp.path(), "postgres://other/db").is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn overrides_of_a_choice_are_explicit() {
        let mut cli = ConfigOverrides::default();
        let (b, p) = Choice::Managed.overrides();
        cli.database_backend = b;
        cli.postgres = p;
        let keys = cli.keys();
        assert!(keys.contains(&"database.backend") && keys.contains(&"database.postgres.mode"));
    }
}
