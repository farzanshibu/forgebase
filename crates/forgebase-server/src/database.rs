//! The primary database of a data directory, on any backend (ADR-0028 §6,
//! ADR-0029 §6, FB-148): SQLite, the bundled PostgreSQL engine
//! (`forgebase-pgengine`) or an external PostgreSQL server.
//!
//! - [`resolve`] decides the backend before anything is opened: the backend
//!   marker `<data_dir>/forgebase.backend` wins over the default, an explicit
//!   configuration that contradicts it is refused, a directory without a
//!   marker that holds `forgebase.db` is SQLite, an empty one takes the
//!   configuration.
//! - [`open_owned`] opens it for the process that holds the data-directory
//!   lock (server, `init`, `migrate`): the bundled engine is started first and
//!   stopped by [`PrimaryDatabase::close`].
//! - [`open_attached`] opens it for diagnostics and one-off commands
//!   (`status`, `doctor`, `admin`, `export`, `db`): it connects to the engine
//!   of a running server through its Unix socket, or, when no server runs,
//!   takes the lock and starts the engine for the duration of the command.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use forgebase_core::config::{DatabaseBackend, DatabaseConfig, ExtensionMode, PostgresMode};
use forgebase_core::{Config, DataLayout};
use forgebase_db::{Database, DbOptions, DbTarget};
use forgebase_pgengine::{Engine, EngineConfig, EngineHandle, EngineSource, Layout, Role, RoleSecrets};

use crate::{DataDirLock, ServerError};

/// File name of the backend marker in the data directory (ADR-0028 §6.3).
pub const BACKEND_MARKER: &str = "forgebase.backend";

/// What a data directory holds: the contents of the backend marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendMarker {
    /// `forgebase.db`.
    Sqlite,
    /// The bundled engine's cluster in `postgres/`.
    PostgresManaged,
    /// A database on an external server.
    PostgresExternal,
}

impl BackendMarker {
    /// The marker file's contents.
    pub fn as_str(self) -> &'static str {
        match self {
            BackendMarker::Sqlite => "sqlite",
            BackendMarker::PostgresManaged => "postgres-managed",
            BackendMarker::PostgresExternal => "postgres-external",
        }
    }

    /// Parses the marker file's contents.
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim() {
            "sqlite" => Some(BackendMarker::Sqlite),
            "postgres-managed" => Some(BackendMarker::PostgresManaged),
            "postgres-external" => Some(BackendMarker::PostgresExternal),
            _ => None,
        }
    }

    /// The backend the configuration asks for.
    pub fn of(config: &DatabaseConfig) -> Self {
        match (config.backend, config.postgres.mode) {
            (DatabaseBackend::Sqlite, _) => BackendMarker::Sqlite,
            (DatabaseBackend::Postgres, PostgresMode::Managed) => BackendMarker::PostgresManaged,
            (DatabaseBackend::Postgres, PostgresMode::External) => BackendMarker::PostgresExternal,
        }
    }

    /// `[database] backend` of this marker.
    pub fn backend(self) -> DatabaseBackend {
        match self {
            BackendMarker::Sqlite => DatabaseBackend::Sqlite,
            BackendMarker::PostgresManaged | BackendMarker::PostgresExternal => DatabaseBackend::Postgres,
        }
    }

    /// `[database.postgres] mode` of this marker (`None` for SQLite).
    pub fn mode(self) -> Option<PostgresMode> {
        match self {
            BackendMarker::Sqlite => None,
            BackendMarker::PostgresManaged => Some(PostgresMode::Managed),
            BackendMarker::PostgresExternal => Some(PostgresMode::External),
        }
    }

    /// Words for messages: "a sqlite database".
    pub fn describe(self) -> &'static str {
        match self {
            BackendMarker::Sqlite => "a sqlite database",
            BackendMarker::PostgresManaged => "a postgres database (the bundled server)",
            BackendMarker::PostgresExternal => "a postgres database on an external server",
        }
    }

    /// Words for status lines: "PostgreSQL (bundled)".
    pub fn label(self) -> &'static str {
        match self {
            BackendMarker::Sqlite => "SQLite",
            BackendMarker::PostgresManaged => "PostgreSQL (bundled)",
            BackendMarker::PostgresExternal => "PostgreSQL (external)",
        }
    }
}

/// `<data_dir>/forgebase.backend`.
pub fn marker_path(layout: &DataLayout) -> PathBuf {
    layout.root().join(BACKEND_MARKER)
}

/// The bundled engine's cluster layout of a data directory.
pub fn engine_layout(layout: &DataLayout) -> Layout {
    Layout::primary(layout.root())
}

/// Reads the backend marker (`None` when there is none).
///
/// # Errors
/// An unreadable marker, or one with unknown contents.
pub fn read_marker(layout: &DataLayout) -> Result<Option<BackendMarker>, ServerError> {
    let path = marker_path(layout);
    match std::fs::read_to_string(&path) {
        Ok(text) => BackendMarker::parse(&text).map(Some).ok_or_else(|| {
            ServerError::Backend(format!(
                "{} holds {:?}; expected sqlite, postgres-managed or postgres-external",
                path.display(),
                text.trim()
            ))
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(ServerError::Io { path, source }),
    }
}

/// Writes the backend marker atomically (`init`, the first start of an
/// empty data directory and `db convert` write it).
///
/// # Errors
/// File-system errors.
pub fn write_marker(layout: &DataLayout, marker: BackendMarker) -> Result<(), ServerError> {
    let path = marker_path(layout);
    forgebase_core::features::write_atomic(&path, &format!("{}\n", marker.as_str()))
        .map_err(|source| ServerError::Io { path, source })
}

/// What the data directory holds: the marker, else what is on disk
/// (`forgebase.db` = SQLite, a cluster in `postgres/data` = the bundled
/// engine), else `None` (an empty directory).
///
/// # Errors
/// An unreadable or unknown marker.
pub fn existing(layout: &DataLayout) -> Result<Option<BackendMarker>, ServerError> {
    if let Some(m) = read_marker(layout)? {
        return Ok(Some(m));
    }
    if layout.database_path().is_file() {
        return Ok(Some(BackendMarker::Sqlite));
    }
    if engine_layout(layout).pgdata().join("PG_VERSION").is_file() {
        return Ok(Some(BackendMarker::PostgresManaged));
    }
    Ok(None)
}

/// Whether the data directory holds a database on any backend.
pub fn is_initialized(layout: &DataLayout) -> bool {
    matches!(existing(layout), Ok(Some(_)) | Err(_))
}

/// Decides the backend of the data directory (ADR-0028 §6.3, ADR-0029 §6).
///
/// `explicit` says whether `[database] backend` (or `mode`) was set by the
/// file, the environment or a flag, rather than being the build's default.
/// An existing directory keeps its backend when the configuration only has
/// the default (existing SQLite projects never switch on their own); an
/// explicit configuration that contradicts it is refused.
///
/// # Errors
/// [`ServerError::BackendMismatch`], or an unreadable marker.
pub fn resolve(config: &Config, explicit: bool, layout: &DataLayout) -> Result<BackendMarker, ServerError> {
    let configured = BackendMarker::of(&config.database);
    match existing(layout)? {
        None => Ok(configured),
        Some(found) if found == configured || !explicit => Ok(found),
        Some(found) => Err(ServerError::BackendMismatch {
            configured: configured.as_str(),
            found,
            data_dir: layout.root().to_path_buf(),
        }),
    }
}

/// Whether `[database] backend` or `[database.postgres] mode` came from a
/// layer other than the defaults.
pub fn backend_is_explicit(sources: &forgebase_core::ConfigSources) -> bool {
    use forgebase_core::config::Source;
    ["database.backend", "database.postgres.mode"]
        .iter()
        .any(|k| sources.get(k) != Source::Default)
}

/// The bundled engine's settings from `[database.postgres]`.
///
/// # Errors
/// An invalid `listen` address.
pub fn engine_config(config: &DatabaseConfig) -> Result<EngineConfig, ServerError> {
    let pg = &config.postgres;
    let source = if pg.engine_dir.trim().is_empty() {
        EngineSource::Auto
    } else {
        EngineSource::Dir(PathBuf::from(&pg.engine_dir))
    };
    let mode = |m: ExtensionMode| match m {
        ExtensionMode::Auto => forgebase_pgengine::ExtensionMode::Auto,
        ExtensionMode::On => forgebase_pgengine::ExtensionMode::On,
        ExtensionMode::Off => forgebase_pgengine::ExtensionMode::Off,
    };
    Ok(EngineConfig {
        source,
        listen: EngineConfig::parse_listen(&pg.listen)?,
        settings: pg.settings.clone(),
        cron: mode(pg.extensions.cron),
        graph: mode(pg.extensions.graph),
        ..EngineConfig::default()
    })
}

/// Whether this binary can run the bundled engine without configuration:
/// it embeds one, or `FORGEBASE_PG_ENGINE_DIR` names one (development).
pub fn bundled_engine_available(config: &DatabaseConfig) -> bool {
    forgebase_pgengine::HAS_EMBEDDED
        || !config.postgres.engine_dir.trim().is_empty()
        || std::env::var_os(forgebase_pgengine::ENGINE_DIR_ENV).is_some_and(|v| !v.is_empty())
}

/// The message for `--database postgres` in a build without an engine.
pub const NO_BUNDLED_ENGINE: &str =
    "this build has no bundled PostgreSQL; use --database-url, --database sqlite or the standard release";

/// The primary database, with the engine this process started (if any).
#[derive(Debug)]
pub struct PrimaryDatabase {
    db: Database,
    marker: BackendMarker,
    engine: Option<Arc<EngineHandle>>,
    attached: Option<AttachedEngine>,
    lock: Option<DataDirLock>,
}

impl PrimaryDatabase {
    /// The database handle (cheap to clone).
    pub fn db(&self) -> &Database {
        &self.db
    }

    /// The backend.
    pub fn marker(&self) -> BackendMarker {
        self.marker
    }

    /// The bundled engine this process runs, if it started one.
    pub fn engine(&self) -> Option<&Arc<EngineHandle>> {
        self.engine.as_ref()
    }

    /// The engine of the running server this process connected to.
    pub fn attached(&self) -> Option<&AttachedEngine> {
        self.attached.as_ref()
    }

    /// Socket directory and port of the bundled engine, whoever runs it.
    pub fn socket(&self) -> Option<(PathBuf, u16)> {
        if let Some(e) = &self.engine {
            return e.socket_dir().map(|d| (d.to_path_buf(), e.port()));
        }
        self.attached.as_ref().map(|a| (a.socket_dir.clone(), a.port))
    }

    /// Closes the pools, then stops the engine this process started and
    /// releases the lock it took.
    pub async fn close(self) {
        self.db.close().await;
        if let Some(engine) = &self.engine
            && let Err(e) = engine.stop().await
        {
            tracing::warn!(error = %e, "stopping PostgreSQL failed");
        }
        drop(self.lock);
    }

    /// Folds the SQLite WAL into the database file. Nothing on PostgreSQL:
    /// the engine's fast shutdown checkpoints, and `forgebase_server` is not
    /// allowed `CHECKPOINT` (no `pg_checkpoint` membership).
    ///
    /// # Errors
    /// SQLite checkpoint errors.
    pub async fn checkpoint(&self) -> Result<(), forgebase_db::DbError> {
        checkpoint(&self.db).await
    }

    pub(crate) fn into_parts(self) -> (Database, Option<Arc<EngineHandle>>) {
        (self.db, self.engine)
    }
}

impl std::ops::Deref for PrimaryDatabase {
    type Target = Database;

    fn deref(&self) -> &Database {
        &self.db
    }
}

/// [`Database::checkpoint`] on SQLite only (see [`PrimaryDatabase::checkpoint`]).
///
/// # Errors
/// SQLite checkpoint errors.
pub async fn checkpoint(db: &Database) -> Result<(), forgebase_db::DbError> {
    if db.kind() == forgebase_db::BackendKind::Sqlite {
        db.checkpoint().await
    } else {
        Ok(())
    }
}

/// How [`open_owned`] treats a data directory without a database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Create {
    /// Create it (`init`, `serve`).
    Yes,
    /// Refuse (`migrate`): [`ServerError::NotInitialized`].
    No,
}

/// Opens the primary database for the process holding the data-directory
/// lock, starting the bundled engine first (ADR-0028 §4.3). Writes the
/// backend marker when the directory had none.
///
/// # Errors
/// Backend mismatch, engine and database errors.
pub async fn open_owned(
    config: &Config,
    explicit: bool,
    layout: &DataLayout,
    create: Create,
) -> Result<PrimaryDatabase, ServerError> {
    let marker = resolve(config, explicit, layout)?;
    let had_marker = read_marker(layout)?.is_some();
    if create == Create::No && existing(layout)?.is_none() {
        return Err(ServerError::NotInitialized(layout.root().to_path_buf()));
    }
    if had_marker {
        check_contents(layout, marker)?;
    }
    let primary = match marker {
        BackendMarker::Sqlite => {
            let opts = DbOptions::from_config(&config.database, layout.database_path())
                .create_if_missing(create == Create::Yes);
            PrimaryDatabase {
                db: Database::open(opts).await?,
                marker,
                engine: None,
                attached: None,
                lock: None,
            }
        }
        BackendMarker::PostgresManaged => {
            let engine = start_engine(config, layout).await?;
            let opts = pg_options(&config.database, engine.url(Role::Server).expose(), true);
            match Database::open(opts).await {
                Ok(db) => PrimaryDatabase {
                    db,
                    marker,
                    engine: Some(Arc::new(engine)),
                    attached: None,
                    lock: None,
                },
                Err(e) => {
                    let _ = engine.stop().await;
                    return Err(e.into());
                }
            }
        }
        BackendMarker::PostgresExternal => {
            let url = external_url(&config.database)?;
            let opts = pg_options(&config.database, url, false).create_if_missing(create == Create::Yes);
            PrimaryDatabase {
                db: Database::open(opts).await?,
                marker,
                engine: None,
                attached: None,
                lock: None,
            }
        }
    };
    if !had_marker && let Err(e) = write_marker(layout, marker) {
        primary.close().await;
        return Err(e);
    }
    Ok(primary)
}

/// A marked data directory must still hold its database: never create an
/// empty one in place of a deleted file or cluster (ADR-0028 §6.3).
fn check_contents(layout: &DataLayout, marker: BackendMarker) -> Result<(), ServerError> {
    let missing = match marker {
        BackendMarker::Sqlite => (!layout.database_path().is_file()).then(|| layout.database_path()),
        BackendMarker::PostgresManaged => {
            let pgdata = engine_layout(layout).pgdata();
            (!pgdata.join("PG_VERSION").is_file()).then_some(pgdata)
        }
        BackendMarker::PostgresExternal => None,
    };
    match missing {
        Some(path) => Err(ServerError::Backend(format!(
            "{} says this data directory holds {}, but {} is missing; restore it from a backup \
             (forgebase restore), or remove {} to start an empty project",
            marker_path(layout).display(),
            marker.describe(),
            path.display(),
            BACKEND_MARKER
        ))),
        None => Ok(()),
    }
}

/// Starts the bundled engine of `layout` (the caller holds the lock).
async fn start_engine(config: &Config, layout: &DataLayout) -> Result<EngineHandle, ServerError> {
    if !bundled_engine_available(&config.database) {
        return Err(ServerError::Backend(NO_BUNDLED_ENGINE.to_owned()));
    }
    let cfg = engine_config(&config.database)?;
    let started = std::time::Instant::now();
    let engine = Engine::prepare(&engine_layout(layout), &cfg).await?;
    tracing::info!(
        version = %engine.version().text,
        engine = %engine.engine().root.display(),
        elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        "bundled PostgreSQL started"
    );
    Ok(engine)
}

fn pg_options(config: &DatabaseConfig, url: &str, managed: bool) -> DbOptions {
    let mut opts = DbOptions::from_config_target(config, DbTarget::postgres(url, managed));
    opts.read_pool_size = config.postgres.pool_size;
    opts
}

fn external_url(config: &DatabaseConfig) -> Result<&str, ServerError> {
    config.postgres.url.as_ref().map(|u| u.expose()).ok_or_else(|| {
        ServerError::Backend(
            "database.postgres.mode is external, but no server is named: set FORGEBASE_DATABASE_URL, \
             such as postgres://forgebase:secret@db.example.com/forgebase"
                .to_owned(),
        )
    })
}

/// Which login a one-off command uses on PostgreSQL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Read-only (`forgebase_reader`; SQLite opened read-only).
    Read,
    /// The server's own login (`forgebase_server`): reads and writes.
    Write,
    /// The bundled engine's superuser (`forgebase_super`; `doctor`'s
    /// `amcheck`). On SQLite and external servers the same as [`Access::Write`].
    Super,
}

impl Access {
    fn role(self) -> Role {
        match self {
            Access::Read => Role::Reader,
            Access::Write => Role::Server,
            Access::Super => Role::Super,
        }
    }
}

/// Opens the database of a data directory that may be served right now
/// (`status`, `doctor`, `admin`, `export`, `db`):
/// - SQLite: the file, read-only for [`Access::Read`];
/// - bundled engine: the running server's engine through its socket, or,
///   when no server runs, the engine started under the lock for the
///   command's duration;
/// - external: the configured server.
///
/// # Errors
/// [`ServerError::NotInitialized`] for a directory without a database;
/// engine and database errors.
pub async fn open_attached(
    config: &Config,
    explicit: bool,
    layout: &DataLayout,
    access: Access,
) -> Result<PrimaryDatabase, ServerError> {
    let marker = resolve(config, explicit, layout)?;
    if existing(layout)?.is_none() {
        return Err(ServerError::NotInitialized(layout.root().to_path_buf()));
    }
    if read_marker(layout)?.is_some() {
        check_contents(layout, marker)?;
    }
    match marker {
        BackendMarker::Sqlite => {
            let opts = DbOptions::from_config(&config.database, layout.database_path());
            let opts = if access == Access::Read {
                opts.read_only()
            } else {
                opts.create_if_missing(false)
            };
            Ok(PrimaryDatabase {
                db: Database::open(opts).await?,
                marker,
                engine: None,
                attached: None,
                lock: None,
            })
        }
        BackendMarker::PostgresExternal => {
            let url = external_url(&config.database)?;
            let mut opts = pg_options(&config.database, url, false).create_if_missing(false);
            if access == Access::Read {
                opts = opts.read_only();
            }
            Ok(PrimaryDatabase {
                db: Database::open(opts).await?,
                marker,
                engine: None,
                attached: None,
                lock: None,
            })
        }
        BackendMarker::PostgresManaged => {
            let role = access.role();
            match DataDirLock::acquire(layout) {
                Ok(lock) => {
                    let engine = start_engine(config, layout).await?;
                    let mut opts = pg_options(&config.database, engine.url(role).expose(), true);
                    opts.read_pool_size = 2;
                    match Database::open(opts).await {
                        Ok(db) => Ok(PrimaryDatabase {
                            db,
                            marker,
                            engine: Some(Arc::new(engine)),
                            attached: None,
                            lock: Some(lock),
                        }),
                        Err(e) => {
                            let _ = engine.stop().await;
                            Err(e.into())
                        }
                    }
                }
                Err(ServerError::Locked { .. }) => {
                    let attached = attach(layout).await?;
                    let url = attached.url(role)?;
                    let mut opts = pg_options(&config.database, &url, true);
                    opts.read_pool_size = 2;
                    Ok(PrimaryDatabase {
                        db: Database::open(opts).await?,
                        marker,
                        engine: None,
                        attached: Some(attached),
                        lock: None,
                    })
                }
                Err(e) => Err(e),
            }
        }
    }
}

/// The bundled engine of a running server, found through `postmaster.pid`.
#[derive(Debug, Clone)]
pub struct AttachedEngine {
    /// Postmaster pid.
    pub pid: u32,
    /// Unix socket directory.
    pub socket_dir: PathBuf,
    /// Port (the socket's name).
    pub port: u16,
    secrets_file: PathBuf,
}

impl AttachedEngine {
    /// Connection URL of `role` for the `forgebase` database, through the socket.
    ///
    /// # Errors
    /// `roles.secret` is missing, unreadable or not private.
    pub fn url(&self, role: Role) -> Result<String, ServerError> {
        let secrets = RoleSecrets::load(&self.secrets_file)?.ok_or_else(|| {
            ServerError::Backend(format!(
                "{} is missing, so ForgeBase cannot log in to the running PostgreSQL",
                self.secrets_file.display()
            ))
        })?;
        let pw = secrets.password(role).unwrap_or_default();
        Ok(socket_url(
            role.name(),
            pw,
            forgebase_pgengine::DATABASE,
            &self.socket_dir,
            self.port,
        ))
    }
}

/// `postgres://role:pw@localhost/db?host=<dir>&port=<port>`.
pub fn socket_url(role: &str, password: &str, database: &str, dir: &Path, port: u16) -> String {
    format!(
        "postgres://{}:{}@localhost/{}?host={}&port={port}",
        pct(role, b""),
        pct(password, b""),
        pct(database, b""),
        pct(&dir.display().to_string(), b"/")
    )
}

fn pct(s: &str, keep: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) || keep.contains(&b) {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// What `<pgdata>/postmaster.pid` says about the engine of a data directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostmasterInfo {
    /// Postmaster pid.
    pub pid: u32,
    /// Port.
    pub port: u16,
    /// First Unix socket directory (empty when the engine listens on TCP only).
    pub socket_dir: Option<PathBuf>,
    /// `starting`, `ready`, `stopping`, `standby` (line 8).
    pub status: String,
}

/// Reads `postmaster.pid` of the bundled engine (`None` when it is not
/// running or the file is malformed). The pid is checked to be alive.
pub fn postmaster(layout: &DataLayout) -> Option<PostmasterInfo> {
    let text = std::fs::read_to_string(engine_layout(layout).pgdata().join("postmaster.pid")).ok()?;
    let lines: Vec<&str> = text.lines().collect();
    let pid: u32 = lines.first()?.trim().parse().ok()?;
    let port: u16 = lines.get(3)?.trim().parse().ok()?;
    let socket_dir = lines
        .get(4)
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(PathBuf::from);
    let status = lines.get(7).map_or("starting", |l| l.trim()).to_owned();
    pid_alive(pid).then_some(PostmasterInfo {
        pid,
        port,
        socket_dir,
        status,
    })
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(not(unix))]
fn pid_alive(_pid: u32) -> bool {
    true
}

/// Finds the engine of a running server and waits up to 10 s for it to be
/// ready (a server that is still starting).
async fn attach(layout: &DataLayout) -> Result<AttachedEngine, ServerError> {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        match postmaster(layout) {
            Some(info) if info.status == "ready" => {
                let socket_dir = info.socket_dir.ok_or_else(|| {
                    ServerError::Backend("the running PostgreSQL has no Unix socket to connect to".into())
                })?;
                return Ok(AttachedEngine {
                    pid: info.pid,
                    socket_dir,
                    port: info.port,
                    secrets_file: engine_layout(layout).secrets_file(),
                });
            }
            _ if std::time::Instant::now() >= deadline => {
                return Err(ServerError::Backend(format!(
                    "the data directory {} is in use by a running ForgeBase, but its PostgreSQL is not ready",
                    layout.root().display()
                )));
            }
            _ => tokio::time::sleep(Duration::from_millis(200)).await,
        }
    }
}

/// The engine directory of a data directory's cluster, for programs such as
/// `psql` (`forgebase db psql`): `engine_dir`, else `FORGEBASE_PG_ENGINE_DIR`
/// when the binary embeds no engine, else the newest extracted engine of the
/// cluster's major in `postgres/engine/`.
pub fn engine_root(config: &DatabaseConfig, layout: &DataLayout) -> Option<PathBuf> {
    let pg = &config.postgres;
    if !pg.engine_dir.trim().is_empty() {
        return Some(PathBuf::from(&pg.engine_dir));
    }
    if !forgebase_pgengine::HAS_EMBEDDED
        && let Some(dir) = std::env::var_os(forgebase_pgengine::ENGINE_DIR_ENV).filter(|v| !v.is_empty())
    {
        return Some(PathBuf::from(dir));
    }
    let el = engine_layout(layout);
    let major = std::fs::read_to_string(el.pgdata().join("PG_VERSION")).ok();
    let major = major.as_deref().map(str::trim);
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(el.engines_dir())
        .ok()?
        .flatten()
        .filter(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            !name.ends_with(".partial")
                && e.path().join(forgebase_pgengine::archive::SHA_FILE).is_file()
                && major.is_none_or(|m| name.starts_with(&format!("{m}.")))
        })
        .map(|e| {
            let t = e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
            (t, e.path())
        })
        .collect();
    found.sort();
    found.pop().map(|(_, p)| p)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn config(dir: &Path) -> Config {
        Config::defaults(dir)
    }

    #[test]
    fn marker_round_trips() {
        for m in [
            BackendMarker::Sqlite,
            BackendMarker::PostgresManaged,
            BackendMarker::PostgresExternal,
        ] {
            assert_eq!(BackendMarker::parse(m.as_str()), Some(m));
            assert_eq!(BackendMarker::parse(&format!("{}\n", m.as_str())), Some(m));
        }
        assert_eq!(BackendMarker::parse("postgres"), None);
    }

    #[test]
    fn marker_wins_over_the_default_and_refuses_an_explicit_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        let mut c = config(tmp.path());
        let layout = DataLayout::new(&c.data_dir);
        layout.ensure().unwrap();
        c.database.backend = DatabaseBackend::Postgres;
        // Empty directory: the configuration decides.
        assert_eq!(resolve(&c, true, &layout).unwrap(), BackendMarker::PostgresManaged);
        assert!(!is_initialized(&layout));
        // A SQLite file without a marker: an existing project stays SQLite.
        std::fs::write(layout.database_path(), b"").unwrap();
        assert_eq!(resolve(&c, false, &layout).unwrap(), BackendMarker::Sqlite);
        let err = resolve(&c, true, &layout).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "database.backend is postgres, but the data directory {} holds a sqlite database; run forgebase db convert --to postgres, or set [database] backend = \"sqlite\"",
                layout.root().display()
            )
        );
        assert_eq!(forgebase_core::ErrorCode::code(&err), "backend_mismatch");
        // The marker wins over what is on disk.
        write_marker(&layout, BackendMarker::PostgresManaged).unwrap();
        assert_eq!(read_marker(&layout).unwrap(), Some(BackendMarker::PostgresManaged));
        assert_eq!(resolve(&c, true, &layout).unwrap(), BackendMarker::PostgresManaged);
        c.database.backend = DatabaseBackend::Sqlite;
        assert!(resolve(&c, true, &layout).is_err());
        assert_eq!(resolve(&c, false, &layout).unwrap(), BackendMarker::PostgresManaged);
        std::fs::write(marker_path(&layout), "mysql\n").unwrap();
        assert!(read_marker(&layout).is_err());
    }

    #[test]
    fn a_marked_directory_must_keep_its_database() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(tmp.path());
        layout.ensure().unwrap();
        write_marker(&layout, BackendMarker::Sqlite).unwrap();
        let err = check_contents(&layout, BackendMarker::Sqlite).unwrap_err();
        assert!(err.to_string().contains("forgebase.db is missing"), "{err}");
        std::fs::write(layout.database_path(), b"").unwrap();
        check_contents(&layout, BackendMarker::Sqlite).unwrap();
        let err = check_contents(&layout, BackendMarker::PostgresManaged).unwrap_err();
        assert!(err.to_string().contains("postgres/data is missing"), "{err}");
        check_contents(&layout, BackendMarker::PostgresExternal).unwrap();
    }

    #[test]
    fn engine_config_maps_the_postgres_section() {
        let mut c = config(Path::new("/srv/fb")).database;
        c.postgres.listen = "127.0.0.1:5433".into();
        c.postgres.engine_dir = "/opt/pg".into();
        c.postgres.extensions.cron = ExtensionMode::Off;
        c.postgres.extensions.graph = ExtensionMode::On;
        c.postgres.settings.insert("work_mem".into(), "16MB".into());
        let e = engine_config(&c).unwrap();
        assert_eq!(e.graph, forgebase_pgengine::ExtensionMode::On);
        assert_eq!(e.listen, Some("127.0.0.1:5433".parse().unwrap()));
        assert_eq!(e.source, EngineSource::Dir("/opt/pg".into()));
        assert_eq!(e.cron, forgebase_pgengine::ExtensionMode::Off);
        assert_eq!(e.settings.get("work_mem").map(String::as_str), Some("16MB"));
        c.postgres.listen = "0.0.0.0:5433".into();
        assert!(engine_config(&c).is_err());
    }

    #[test]
    fn socket_urls_are_encoded() {
        let url = socket_url(
            "forgebase_reader",
            "p@ss/w:rd",
            "forgebase",
            Path::new("/tmp/a b"),
            5432,
        );
        assert_eq!(
            url,
            "postgres://forgebase_reader:p%40ss%2Fw%3Ard@localhost/forgebase?host=/tmp/a%20b&port=5432"
        );
    }

    #[test]
    fn postmaster_pid_is_parsed() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(tmp.path());
        let pgdata = engine_layout(&layout).pgdata();
        std::fs::create_dir_all(&pgdata).unwrap();
        assert_eq!(postmaster(&layout), None);
        let me = std::process::id();
        std::fs::write(
            pgdata.join("postmaster.pid"),
            format!("{me}\n/x\n1700000000\n5432\n/tmp/sock\n\n  1234 5678\nready   \n"),
        )
        .unwrap();
        let info = postmaster(&layout).unwrap();
        assert_eq!(info.pid, me);
        assert_eq!(info.port, 5432);
        assert_eq!(info.socket_dir, Some(PathBuf::from("/tmp/sock")));
        assert_eq!(info.status, "ready");
    }
}
