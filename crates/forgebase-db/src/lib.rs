//! The ForgeBase primary database.
//!
//! [`Database`] is a facade over two engines (ADR-0028 §7.2), chosen by
//! [`DbOptions::target`]:
//!
//! * **SQLite** (default, ADR-0001): one file in WAL mode.
//!   * **One writer.** A pool with exactly one connection serialises every
//!     write inside the process, so writers queue in Rust instead of spinning
//!     on `SQLITE_BUSY`. `busy_timeout` still covers other processes (CLI tools).
//!   * **A read pool.** `read_pool_size` connections opened with
//!     `PRAGMA query_only = ON`; WAL lets them read a consistent snapshot while
//!     the writer commits.
//!   * **Pragmas on every connection:** `foreign_keys = ON`, `busy_timeout`,
//!     `synchronous` (default `FULL`: a commit is fsynced before it is
//!     reported). The writer also sets `journal_mode = WAL` and the open fails
//!     if WAL cannot be enabled.
//! * **PostgreSQL** (ADR-0028 §7.3): the same single writer (one connection,
//!   so sequence order is commit order), a read pool whose statements run in
//!   `READ ONLY` transactions, `?N` placeholders translated to `$N`, parameters
//!   bound by the types the server describes.
//!
//! Engine specifics stay inside this crate: other crates use [`Database`]
//! methods or the [`Backend`] trait (ADR-0003), never `sqlx` pools.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use forgebase_core::{ErrorCode, ErrorKind, Synchronous};
use forgebase_migrations::{AppliedMigration, MigrationError, MigrationStatus};
use serde::Serialize;

pub use forgebase_migrations as migrations;

pub mod backend;
pub mod dialect;
mod pg;
pub mod placement;
pub mod router;
pub mod routing;
mod slow_log;
mod sqlite;
mod sqlite_backend;
#[cfg(feature = "testing")]
pub mod testing;
pub mod topology;

pub use backend::{
    Backend, BoxFuture, ExecResult, PlanStep, QueryLabel, Rows, RunAs, SlowQuery, Statement, TxMode, Value, WriteTx,
};
pub use dialect::{ColumnType, Dialect, JsonPathSeg, PG_FULLTEXT_SETUP, PG_UNACCENT_FN, PgDialect, SqliteDialect};
pub use pg::extensions::{
    EXTENSION_DDL, EXTENSION_SCHEMA, ExtensionDdl, ExtensionError, ExtensionReport, ExtensionSetup, Feature,
    FeatureState, FeatureStatus,
};
pub use pg::listen::Listener;
pub use pg::placeholders::{PlaceholderError, Translated, translate as translate_placeholders};
pub use router::{DbRouter, RouterError, RouterOptions, SetConnector, SetHandle, TenantRef};
pub use routing::{
    CommitToken, Consistency, Lsn, PrimaryOnly, ReadAfter, ReadIntent, ReadRequest, ReadRouter, ReadRouting,
    ReadTarget, ReplicaOptions, ReplicaSet,
};
pub use slow_log::{DEFAULT_SLOW_QUERY_THRESHOLD, SLOW_QUERY_LOG_CAPACITY};
pub use sqlite::sqlite_library_version;

use pg::PgDatabase;
use sqlite::SqliteDatabase;

/// How long a caller waits for the writer (or a reader) before giving up.
pub(crate) const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(30);

/// Which engine a [`Database`] runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    /// Embedded SQLite.
    Sqlite,
    /// PostgreSQL (bundled or external).
    Postgres,
}

impl BackendKind {
    /// `sqlite` or `postgres` (the `[database] backend` value).
    pub fn as_str(self) -> &'static str {
        match self {
            BackendKind::Sqlite => "sqlite",
            BackendKind::Postgres => "postgres",
        }
    }
}

impl fmt::Display for BackendKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A secret string (a connection URL with a password). `Debug` and
/// `Display` never show it.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretString(String);

impl SecretString {
    /// Wraps a secret.
    pub fn new(secret: impl Into<String>) -> Self {
        Self(secret.into())
    }

    /// The secret itself.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl From<String> for SecretString {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for SecretString {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretString(<redacted>)")
    }
}

/// Where the database lives (ADR-0028 §7.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DbTarget {
    /// A SQLite file.
    Sqlite {
        /// Database file.
        path: PathBuf,
    },
    /// A PostgreSQL database.
    Postgres {
        /// Connection URL (`postgres://user:pass@host/db`, or with
        /// `?host=/socket/dir` for a Unix socket).
        url: SecretString,
        /// `true` for ForgeBase's bundled engine (`forgebase-pgengine`),
        /// `false` for an external server.
        managed: bool,
    },
}

impl DbTarget {
    /// A SQLite file.
    pub fn sqlite(path: impl Into<PathBuf>) -> Self {
        Self::Sqlite { path: path.into() }
    }

    /// A PostgreSQL database.
    pub fn postgres(url: impl Into<SecretString>, managed: bool) -> Self {
        Self::Postgres {
            url: url.into(),
            managed,
        }
    }

    /// The engine.
    pub fn kind(&self) -> BackendKind {
        match self {
            DbTarget::Sqlite { .. } => BackendKind::Sqlite,
            DbTarget::Postgres { .. } => BackendKind::Postgres,
        }
    }
}

/// Options for [`Database::open`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbOptions {
    /// Engine and location.
    pub target: DbTarget,
    /// SQLite `synchronous` mode (SQLite only).
    pub synchronous: Synchronous,
    /// SQLite busy timeout (SQLite only).
    pub busy_timeout: Duration,
    /// Number of read connections.
    pub read_pool_size: u32,
    /// Create the database if it does not exist. When `false`, a missing
    /// database is [`DbError::NotFound`] and nothing is created.
    pub create_if_missing: bool,
    /// Open every connection read-only (diagnostics). Implies no creation,
    /// no journal-mode change and no `PRAGMA optimize` on close.
    pub read_only: bool,
}

impl DbOptions {
    /// Options from the database section of the configuration, for the
    /// SQLite file at `path`.
    pub fn from_config(config: &forgebase_core::config::DatabaseConfig, path: impl Into<PathBuf>) -> Self {
        Self::from_config_target(config, DbTarget::sqlite(path))
    }

    /// Options from the database section of the configuration, for `target`.
    pub fn from_config_target(config: &forgebase_core::config::DatabaseConfig, target: DbTarget) -> Self {
        Self {
            target,
            synchronous: config.synchronous,
            busy_timeout: Duration::from_millis(config.busy_timeout_ms),
            read_pool_size: config.read_pool_size,
            create_if_missing: true,
            read_only: false,
        }
    }

    /// Same options, with `create_if_missing` set.
    pub fn create_if_missing(mut self, create: bool) -> Self {
        self.create_if_missing = create;
        self
    }

    /// Same options, opened read-only for diagnostics.
    pub fn read_only(mut self) -> Self {
        self.read_only = true;
        self.create_if_missing = false;
        self
    }

    /// Same options, for another target.
    pub fn with_target(mut self, target: DbTarget) -> Self {
        self.target = target;
        self
    }
}

/// Handle to the primary database. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Database {
    inner: Arc<Inner>,
}

#[derive(Debug)]
enum Inner {
    Sqlite(SqliteDatabase),
    Postgres(PgDatabase),
}

/// Takes the pre-migration snapshot of a PostgreSQL database (FB-150).
///
/// `forgebase-db` has no engine tools: the server installs a provider with
/// [`Database::set_pre_migration`] (it runs `pg_basebackup`), and
/// [`Database::migrate`] calls it before it applies anything to a database
/// that already has applied migrations. A failure stops the migration.
pub trait PreMigrationSnapshot: Send + Sync + fmt::Debug {
    /// Writes a snapshot of the database at schema version `from_version`
    /// somewhere below `backups_dir`; returns where.
    fn take<'a>(
        &'a self,
        backups_dir: &'a Path,
        from_version: i64,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<PathBuf, DbError>> + Send + 'a>>;
}

/// Result of [`Database::migrate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrateReport {
    /// Migrations applied by this call.
    pub applied: Vec<AppliedMigration>,
    /// Snapshot written before applying them, if any.
    pub backup: Option<PathBuf>,
}

/// Identity of this ForgeBase instance, written once at initialisation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InstanceInfo {
    /// Random UUID identifying this data directory.
    pub instance_id: String,
    /// RFC 3339 time of first initialisation.
    pub initialized_at: String,
}

/// Connections of one pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolUsage {
    /// Connections open (idle plus in use).
    pub open: u32,
    /// Open connections waiting in the pool.
    pub idle: u32,
    /// Open connections checked out.
    pub in_use: u32,
    /// Configured maximum.
    pub max: u32,
}

/// Usage of both database pools.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolStats {
    /// The single-connection writer pool.
    pub writer: PoolUsage,
    /// The read pool.
    pub reader: PoolUsage,
}

/// File and engine statistics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DatabaseInfo {
    /// Engine.
    pub backend: BackendKind,
    /// Database file (SQLite); empty on PostgreSQL.
    pub path: PathBuf,
    /// Size of the database in bytes (`pg_database_size` on PostgreSQL).
    pub size_bytes: u64,
    /// Size of the write-ahead log in bytes (0 on PostgreSQL when the role
    /// may not list the WAL directory).
    pub wal_size_bytes: u64,
    /// Current journal mode (expected `wal`; always `wal` on PostgreSQL).
    pub journal_mode: String,
    /// Highest applied system migration.
    pub schema_version: i64,
    /// Page size in bytes (PostgreSQL: `block_size`).
    pub page_size: i64,
    /// Number of pages (PostgreSQL: size / block size).
    pub page_count: i64,
    /// Number of unused pages (always 0 on PostgreSQL).
    pub freelist_count: i64,
    /// Version of the linked SQLite library (empty on PostgreSQL).
    pub sqlite_version: String,
    /// Engine version (SQLite library version, or PostgreSQL `server_version`).
    pub server_version: String,
    /// Server connections to this database (PostgreSQL only).
    pub connections: Option<u32>,
    /// Per accelerated feature, the engine it runs on (ADR-0029 §1.1), as
    /// [`Database::setup_extensions`] left it; empty on SQLite and before
    /// the setup ran.
    pub features: Vec<FeatureStatus>,
}

/// Effective connection settings, read back from SQLite.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EffectivePragmas {
    /// `journal_mode`.
    pub journal_mode: String,
    /// `foreign_keys` (1 = on).
    pub foreign_keys: i64,
    /// `synchronous` (1 = NORMAL, 2 = FULL).
    pub synchronous: i64,
    /// `busy_timeout` in ms.
    pub busy_timeout_ms: i64,
    /// `query_only` (1 on read connections).
    pub query_only: i64,
}

impl Database {
    /// Opens (and with `create_if_missing`, creates) the database.
    /// Does not run migrations; call [`Database::migrate`].
    pub async fn open(opts: DbOptions) -> Result<Self, DbError> {
        let inner = match &opts.target {
            DbTarget::Sqlite { path } => Inner::Sqlite(SqliteDatabase::open(path, &opts).await?),
            DbTarget::Postgres { url, managed } => Inner::Postgres(PgDatabase::open(url, *managed, &opts).await?),
        };
        Ok(Self { inner: Arc::new(inner) })
    }

    /// The engine.
    pub fn kind(&self) -> BackendKind {
        match &*self.inner {
            Inner::Sqlite(_) => BackendKind::Sqlite,
            Inner::Postgres(_) => BackendKind::Postgres,
        }
    }

    fn backend(&self) -> &dyn Backend {
        match &*self.inner {
            Inner::Sqlite(db) => db,
            Inner::Postgres(db) => db,
        }
    }

    #[cfg(test)]
    pub(crate) fn sqlite(&self) -> Option<&SqliteDatabase> {
        match &*self.inner {
            Inner::Sqlite(db) => Some(db),
            Inner::Postgres(_) => None,
        }
    }

    /// Database file path (SQLite); an empty path on PostgreSQL.
    pub fn path(&self) -> &Path {
        match &*self.inner {
            Inner::Sqlite(db) => db.path(),
            Inner::Postgres(_) => Path::new(""),
        }
    }

    /// Where the database lives, for messages: the file path, or the
    /// PostgreSQL URL without its password.
    pub fn location(&self) -> String {
        match &*self.inner {
            Inner::Sqlite(db) => db.path().display().to_string(),
            Inner::Postgres(db) => db.location().to_owned(),
        }
    }

    /// Installs the provider of the pre-migration snapshot on PostgreSQL
    /// (managed mode); SQLite writes its own and ignores it.
    pub fn set_pre_migration(&self, hook: Arc<dyn PreMigrationSnapshot>) {
        if let Inner::Postgres(db) = &*self.inner {
            db.set_pre_migration(hook);
        }
    }

    /// Applies pending system migrations.
    ///
    /// SQLite: when the database already has applied migrations and
    /// `backups_dir` is given, a consistent snapshot (`VACUUM INTO`) is
    /// written there first: that snapshot is the rollback path for
    /// forward-only migrations. PostgreSQL: the baseline and its twins
    /// (`forgebase_migrations::postgres`), then the session user's default
    /// `search_path` is made `forgebase, public` if it is not; the
    /// snapshot comes from the provider of [`Database::set_pre_migration`]
    /// (`backup` is `None` when none is installed, as on an external server).
    pub async fn migrate(&self, backups_dir: Option<&Path>) -> Result<MigrateReport, DbError> {
        match &*self.inner {
            Inner::Sqlite(db) => db.migrate(backups_dir).await,
            Inner::Postgres(db) => db.migrate(backups_dir).await,
        }
    }

    /// Applied / pending migrations, read without changing anything.
    pub async fn migration_status(&self) -> Result<MigrationStatus, DbError> {
        match &*self.inner {
            Inner::Sqlite(db) => db.migration_status().await,
            Inner::Postgres(db) => db.migration_status().await,
        }
    }

    /// Highest applied system migration (0 for a fresh database).
    pub async fn schema_version(&self) -> Result<i64, DbError> {
        match &*self.inner {
            Inner::Sqlite(db) => db.schema_version().await,
            Inner::Postgres(db) => db.schema_version().await,
        }
    }

    /// Cheap liveness probe: a read round-trip and an open writer.
    pub async fn ping(&self) -> Result<(), DbError> {
        match &*self.inner {
            Inner::Sqlite(db) => db.ping().await,
            Inner::Postgres(db) => db.ping().await,
        }
    }

    /// Connections of the writer and reader pools right now.
    pub fn pool_stats(&self) -> PoolStats {
        match &*self.inner {
            Inner::Sqlite(db) => db.pool_stats(),
            Inner::Postgres(db) => db.pool_stats(),
        }
    }

    /// Sizes and engine statistics.
    pub async fn info(&self) -> Result<DatabaseInfo, DbError> {
        match &*self.inner {
            Inner::Sqlite(db) => db.info().await,
            Inner::Postgres(db) => db.info().await,
        }
    }

    /// Sets up the PostgreSQL extensions the accelerated features use
    /// (ADR-0029 §1.1): detects
    /// what the server offers, decides each feature's engine from its mode
    /// and its stored data, creates the extensions in use (superuser-only
    /// ones through [`ExtensionSetup::admin_url`]), grants their schemas,
    /// and applies their versioned DDL ([`EXTENSION_DDL`], recorded as
    /// `ext.<feature>.schema`). Run it after [`Database::migrate`], once per
    /// start; the result is kept for [`Database::feature_active`] and
    /// [`Database::info`]. SQLite: an empty report, nothing to do.
    ///
    /// # Errors
    /// [`ExtensionError::Refused`] when a feature is `on` without a usable
    /// extension, or the configured mode contradicts the engine the
    /// database stores that feature's data with (all problems at once).
    pub async fn setup_extensions(&self, setup: &ExtensionSetup) -> Result<ExtensionReport, ExtensionError> {
        match &*self.inner {
            Inner::Sqlite(_) => Ok(ExtensionReport::default()),
            Inner::Postgres(db) => db.setup_extensions(setup).await,
        }
    }

    /// What [`Database::setup_extensions`] would decide, without creating,
    /// granting or recording anything (one-off commands, diagnostics).
    /// SQLite: an empty report.
    pub async fn detect_extensions(
        &self,
        modes: &forgebase_core::config::PostgresExtensions,
    ) -> Result<ExtensionReport, DbError> {
        match &*self.inner {
            Inner::Sqlite(_) => Ok(ExtensionReport::default()),
            Inner::Postgres(db) => db.detect_extensions(modes).await,
        }
    }

    /// The report of the latest [`Database::setup_extensions`] on this
    /// handle; `None` on SQLite and before it ran.
    pub fn extensions(&self) -> Option<Arc<ExtensionReport>> {
        match &*self.inner {
            Inner::Sqlite(_) => None,
            Inner::Postgres(db) => db.extension_report(),
        }
    }

    /// Whether `feature` runs on its PostgreSQL extension: how the
    /// accelerated engines choose their path. No query; `false` on SQLite
    /// and until [`Database::setup_extensions`] ran.
    pub fn feature_active(&self, feature: Feature) -> bool {
        self.extensions().is_some_and(|r| r.is_active(feature))
    }

    /// Checks the database for damage and returns the problems found (empty
    /// = ok). SQLite: `PRAGMA integrity_check` (corruption severe enough to
    /// make the check fail is returned as a problem too). PostgreSQL:
    /// `amcheck` (`bt_index_check` on every B-tree index) plus a
    /// foreign-key check.
    pub async fn integrity_check(&self) -> Result<Vec<String>, DbError> {
        match &*self.inner {
            Inner::Sqlite(db) => db.integrity_check().await,
            Inner::Postgres(db) => db.integrity_check().await,
        }
    }

    /// Number of rows violating foreign keys.
    pub async fn foreign_key_violations(&self) -> Result<usize, DbError> {
        match &*self.inner {
            Inner::Sqlite(db) => db.foreign_key_violations().await,
            Inner::Postgres(db) => db.foreign_key_violations().await,
        }
    }

    /// Effective settings of the writer (`false`) or a reader (`true`)
    /// connection. SQLite only ([`DbError::Unsupported`] on PostgreSQL).
    pub async fn effective_pragmas(&self, reader: bool) -> Result<EffectivePragmas, DbError> {
        match &*self.inner {
            Inner::Sqlite(db) => db.effective_pragmas(reader).await,
            Inner::Postgres(_) => Err(DbError::unsupported("effective_pragmas", BackendKind::Postgres)),
        }
    }

    /// SQLite: writes the WAL back into the main file and truncates it
    /// (`PRAGMA wal_checkpoint(TRUNCATE)`), leaving one self-contained file.
    /// PostgreSQL: `CHECKPOINT` on the bundled engine, nothing on an external
    /// server.
    pub async fn checkpoint(&self) -> Result<(), DbError> {
        match &*self.inner {
            Inner::Sqlite(db) => db.checkpoint().await,
            Inner::Postgres(db) => db.checkpoint().await,
        }
    }

    /// Writes a consistent, self-contained copy of the database to `target`
    /// (`VACUUM INTO`, FB-83). SQLite only: PostgreSQL backups go through
    /// `forgebase-backup`'s `pg_basebackup` path ([`DbError::Unsupported`]).
    pub async fn snapshot_to(&self, target: &Path) -> Result<(), DbError> {
        match &*self.inner {
            Inner::Sqlite(db) => db.snapshot_to(target).await,
            Inner::Postgres(_) => Err(DbError::unsupported("snapshot_to", BackendKind::Postgres)),
        }
    }

    /// Listens on a PostgreSQL notification `channel` (`NOTIFY`) on a
    /// connection of its own (FB-165).
    ///
    /// # Errors
    /// `Unsupported` on SQLite; the connection failed.
    pub async fn listen(&self, channel: &str) -> Result<Listener, DbError> {
        match &*self.inner {
            Inner::Sqlite(_) => Err(DbError::unsupported("LISTEN", BackendKind::Sqlite)),
            Inner::Postgres(db) => db.listen(channel).await,
        }
    }

    /// Closes both pools, waiting for in-flight queries.
    pub async fn close(&self) {
        match &*self.inner {
            Inner::Sqlite(db) => db.close().await,
            Inner::Postgres(db) => db.close().await,
        }
    }

    /// Reads a system metadata value.
    pub async fn meta_get(&self, key: &str) -> Result<Option<String>, DbError> {
        match &*self.inner {
            Inner::Sqlite(db) => db.meta_get(key).await,
            Inner::Postgres(db) => db.meta_get(key).await,
        }
    }

    /// Inserts or replaces a system metadata value. Durable when this returns.
    pub async fn meta_set(&self, key: &str, value: &str) -> Result<(), DbError> {
        match &*self.inner {
            Inner::Sqlite(db) => db.meta_set(key, value).await,
            Inner::Postgres(db) => db.meta_set(key, value).await,
        }
    }

    /// Deletes a system metadata value. Returns whether it existed.
    pub async fn meta_delete(&self, key: &str) -> Result<bool, DbError> {
        match &*self.inner {
            Inner::Sqlite(db) => db.meta_delete(key).await,
            Inner::Postgres(db) => db.meta_delete(key).await,
        }
    }

    /// All system metadata, ordered by key.
    pub async fn meta_all(&self) -> Result<Vec<(String, String)>, DbError> {
        match &*self.inner {
            Inner::Sqlite(db) => db.meta_all().await,
            Inner::Postgres(db) => db.meta_all().await,
        }
    }

    /// WAL position after the latest commit made through this handle
    /// (PostgreSQL; `None` on SQLite or before the first commit). A read that
    /// must see those writes on a replica waits until it has replayed this far.
    pub fn last_commit_lsn(&self) -> Option<Lsn> {
        match &*self.inner {
            Inner::Sqlite(_) => None,
            Inner::Postgres(db) => db.last_commit_lsn(),
        }
    }

    /// The primary's current WAL insert position (PostgreSQL
    /// `pg_current_wal_insert_lsn()`),
    /// at or after every commit that has returned; `None` on SQLite.
    pub async fn current_wal_lsn(&self) -> Result<Option<Lsn>, DbError> {
        match &*self.inner {
            Inner::Sqlite(_) => Ok(None),
            Inner::Postgres(db) => db.current_wal_lsn().await.map(Some),
        }
    }

    /// PostgreSQL `server_version_num` (`180003`); `None` on SQLite.
    pub fn server_version_num(&self) -> Option<u32> {
        match &*self.inner {
            Inner::Sqlite(_) => None,
            Inner::Postgres(db) => Some(db.server_version_num()),
        }
    }

    /// Waits for this standby to replay `lsn` (`WAIT FOR LSN`, PostgreSQL
    /// 19 and newer); `Unsupported` elsewhere.
    pub(crate) async fn wait_for_lsn(&self, lsn: Lsn, timeout: Duration) -> Result<routing::LsnWait, DbError> {
        match &*self.inner {
            Inner::Sqlite(_) => Ok(routing::LsnWait::Unsupported),
            Inner::Postgres(db) => db.wait_for_lsn(lsn, timeout).await,
        }
    }

    /// The administrator URL [`Backend::admin_batch`] logs in with
    /// (PostgreSQL; the bundled engine's `forgebase_super`). `None`: the
    /// writer's own role. Ignored on SQLite.
    pub fn set_admin_url(&self, url: Option<SecretString>) {
        if let Inner::Postgres(db) = &*self.inner {
            db.set_admin_url(url);
        }
    }

    /// Writes the instance id and initialisation time on first call and
    /// returns them on every call. Atomic across processes.
    pub async fn ensure_instance(&self) -> Result<InstanceInfo, DbError> {
        match &*self.inner {
            Inner::Sqlite(db) => db.ensure_instance().await,
            Inner::Postgres(db) => db.ensure_instance().await,
        }
    }
}

impl Backend for Database {
    fn dialect(&self) -> &dyn Dialect {
        self.backend().dialect()
    }

    fn feature_active(&self, feature: Feature) -> bool {
        Database::feature_active(self, feature)
    }

    fn fetch_all<'a>(&'a self, stmt: &'a Statement) -> BoxFuture<'a, Result<Rows, DbError>> {
        self.backend().fetch_all(stmt)
    }

    fn begin(&self, mode: TxMode) -> BoxFuture<'_, Result<Box<dyn WriteTx>, DbError>> {
        self.backend().begin(mode)
    }

    fn begin_as<'a>(&'a self, mode: TxMode, run_as: &'a RunAs) -> BoxFuture<'a, Result<Box<dyn WriteTx>, DbError>> {
        self.backend().begin_as(mode, run_as)
    }

    fn fetch_all_as<'a>(&'a self, stmt: &'a Statement, run_as: &'a RunAs) -> BoxFuture<'a, Result<Rows, DbError>> {
        self.backend().fetch_all_as(stmt, run_as)
    }

    fn admin_batch<'a>(&'a self, statements: &'a [String]) -> BoxFuture<'a, Result<(), DbError>> {
        self.backend().admin_batch(statements)
    }

    fn explain<'a>(&'a self, stmt: &'a Statement) -> BoxFuture<'a, Result<Vec<PlanStep>, DbError>> {
        self.backend().explain(stmt)
    }

    fn statement_writes<'a>(&'a self, stmt: &'a Statement) -> BoxFuture<'a, Result<bool, DbError>> {
        self.backend().statement_writes(stmt)
    }

    fn slow_queries(&self) -> Vec<SlowQuery> {
        self.backend().slow_queries()
    }

    fn clear_slow_queries(&self) {
        self.backend().clear_slow_queries();
    }

    fn slow_query_threshold(&self) -> Duration {
        self.backend().slow_query_threshold()
    }

    fn set_slow_query_threshold(&self, threshold: Duration) {
        self.backend().set_slow_query_threshold(threshold);
    }

    fn maintenance<'a>(&'a self, statements: &'a [String]) -> BoxFuture<'a, Result<(), DbError>> {
        self.backend().maintenance(statements)
    }
}

/// Which constraint a statement violated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConstraintKind {
    /// UNIQUE or PRIMARY KEY.
    Unique,
    /// FOREIGN KEY.
    ForeignKey,
    /// CHECK.
    Check,
    /// NOT NULL.
    NotNull,
    /// Any other constraint (e.g. a trigger's RAISE).
    Other,
}

/// Database errors.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    /// The database does not exist and creation was not requested.
    #[error("database file {0} does not exist (run `forgebase init`)")]
    NotFound(PathBuf),
    /// The database is damaged (or the file is not a SQLite database).
    #[error("database file {path} is corrupt or not a SQLite database: {message}")]
    Corrupt {
        /// File path (PostgreSQL: the URL without its password).
        path: PathBuf,
        /// Engine message.
        message: String,
    },
    /// WAL mode could not be enabled (e.g. unsupported filesystem).
    #[error("database could not be switched to WAL mode (journal_mode is {0:?})")]
    NotWal(String),
    /// The `-wal` file next to the database was written with a different page
    /// size than the database header says (mitigation for RUSTSEC-2026-0459,
    /// see ADR-0002). Opening would let SQLite replay a mismatched WAL.
    #[error(
        "refusing to open {path}: its write-ahead log {wal} has page size {wal_page_size} but the database header says {db_page_size} (damaged or mismatched files; restore a backup or move the -wal file away)"
    )]
    WalPageSizeMismatch {
        /// Database file.
        path: PathBuf,
        /// The `-wal` file.
        wal: PathBuf,
        /// Page size in the database header.
        db_page_size: u32,
        /// Page size in the WAL header.
        wal_page_size: u32,
    },
    /// Lock contention outlasted the busy timeout (PostgreSQL: serialization
    /// failure, deadlock or lock timeout).
    #[error("database is busy: {0}")]
    Busy(String),
    /// The database has been closed.
    #[error("database is closed")]
    Closed,
    /// The database server cannot be reached or is shutting down
    /// (PostgreSQL SQLSTATE classes 08 and 57P0x).
    #[error("database is unavailable: {0}")]
    Unavailable(String),
    /// The operation does not exist on this backend; callers branch on
    /// [`Database::kind`].
    #[error("{0}")]
    Unsupported(String),
    /// A migration problem.
    #[error(transparent)]
    Migration(#[from] MigrationError),
    /// A filesystem error.
    #[error("i/o error at {path}: {source}")]
    Io {
        /// Path involved.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
    /// A constraint was violated; nothing was written.
    #[error("constraint violated: {message}")]
    Constraint {
        /// Which constraint.
        kind: ConstraintKind,
        /// Engine message (`UNIQUE constraint failed: table.column, …` on
        /// both engines for unique constraints).
        message: String,
    },
    /// A statement tried to write through a read-only connection.
    #[error("statement is not read-only: {0}")]
    ReadOnly(String),
    /// The statement is invalid (syntax error, unknown table or column, type mismatch).
    #[error("invalid statement: {0}")]
    Statement(String),
    /// Any other driver error.
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
}

impl DbError {
    /// `operation` is not available on `backend`.
    pub fn unsupported(operation: &str, backend: BackendKind) -> Self {
        DbError::Unsupported(format!("{operation} is not supported on the {backend} backend"))
    }
}

impl ErrorCode for DbError {
    fn code(&self) -> &'static str {
        match self {
            DbError::NotFound(_) => "database_not_found",
            DbError::Corrupt { .. } => "database_corrupt",
            DbError::NotWal(_) => "database_not_wal",
            DbError::WalPageSizeMismatch { .. } => "database_wal_mismatch",
            DbError::Busy(_) => "database_busy",
            DbError::Closed => "database_closed",
            DbError::Unavailable(_) => "database_unavailable",
            DbError::Unsupported(_) => "database_unsupported",
            DbError::Migration(m) => m.code(),
            DbError::Io { .. } => "io_error",
            DbError::Constraint { kind, .. } => match kind {
                ConstraintKind::Unique => "unique_violation",
                ConstraintKind::ForeignKey => "foreign_key_violation",
                ConstraintKind::Check => "check_violation",
                ConstraintKind::NotNull => "not_null_violation",
                ConstraintKind::Other => "constraint_violation",
            },
            DbError::ReadOnly(_) => "statement_not_read_only",
            DbError::Statement(_) => "invalid_statement",
            DbError::Sqlx(_) => "database_error",
        }
    }

    fn kind(&self) -> ErrorKind {
        match self {
            DbError::NotFound(_)
            | DbError::Corrupt { .. }
            | DbError::Busy(_)
            | DbError::Closed
            | DbError::Unavailable(_) => ErrorKind::Unavailable,
            DbError::Migration(m) => m.kind(),
            DbError::Constraint { .. } => ErrorKind::Conflict,
            DbError::ReadOnly(_) | DbError::Statement(_) | DbError::Unsupported(_) => ErrorKind::InvalidInput,
            DbError::NotWal(_) | DbError::WalPageSizeMismatch { .. } | DbError::Io { .. } | DbError::Sqlx(_) => {
                ErrorKind::Internal
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_and_targets_do_not_leak_in_debug() {
        let t = DbTarget::postgres("postgres://u:hunter2@h/db", true);
        assert!(!format!("{t:?}").contains("hunter2"));
        assert_eq!(t.kind(), BackendKind::Postgres);
        assert_eq!(DbTarget::sqlite("/x").kind(), BackendKind::Sqlite);
        assert_eq!(BackendKind::Postgres.to_string(), "postgres");
    }

    #[test]
    fn new_error_variants_have_codes_and_kinds() {
        let e = DbError::Unavailable("connection refused".into());
        assert_eq!((e.code(), e.kind()), ("database_unavailable", ErrorKind::Unavailable));
        let e = DbError::unsupported("snapshot_to", BackendKind::Postgres);
        assert_eq!(e.code(), "database_unsupported");
        assert_eq!(e.to_string(), "snapshot_to is not supported on the postgres backend");
    }
}
