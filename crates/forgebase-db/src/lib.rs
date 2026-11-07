//! The ForgeBase primary database.
//!
//! Embedded mode uses one SQLite file in WAL mode:
//!
//! * **One writer.** A pool with exactly one connection serialises every write
//!   inside the process, so writers queue in Rust instead of spinning on
//!   `SQLITE_BUSY`. `busy_timeout` still covers other processes (CLI tools).
//! * **A read pool.** `read_pool_size` connections opened with
//!   `PRAGMA query_only = ON`; WAL lets them read a consistent snapshot while
//!   the writer commits.
//! * **Pragmas on every connection:** `foreign_keys = ON`, `busy_timeout`,
//!   `synchronous` (default `FULL`: a commit is fsynced before it is reported,
//!   see ADR-0001). The writer also sets `journal_mode = WAL` and the open fails
//!   if WAL cannot be enabled.
//!
//! SQLite specifics stay inside this crate: other crates use [`Database`]
//! methods, never `sqlx` pools. Phase 2 (FB-10) puts a backend trait in front
//! of this so a PostgreSQL implementation can follow.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use forgebase_core::{ErrorCode, ErrorKind, Synchronous};
use forgebase_migrations::{AppliedMigration, MigrationError, MigrationStatus, Migrator};
use serde::Serialize;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Row, SqlitePool};

pub use forgebase_migrations as migrations;

/// How long a caller waits for the writer (or a reader) before giving up.
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(30);

/// Options for [`Database::open`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbOptions {
    /// Database file.
    pub path: PathBuf,
    /// SQLite `synchronous` mode.
    pub synchronous: Synchronous,
    /// SQLite busy timeout.
    pub busy_timeout: Duration,
    /// Number of read connections.
    pub read_pool_size: u32,
    /// Create the file if it does not exist. When `false`, a missing file is
    /// [`DbError::NotFound`] and nothing is created.
    pub create_if_missing: bool,
    /// Open every connection read-only (diagnostics). Implies no creation,
    /// no journal-mode change and no `PRAGMA optimize` on close.
    pub read_only: bool,
}

impl DbOptions {
    /// Options from the database section of the configuration.
    pub fn from_config(config: &forgebase_core::config::DatabaseConfig, path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
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
}

/// Handle to the primary database. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Database {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    writer: SqlitePool,
    reader: SqlitePool,
    path: PathBuf,
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

/// File and engine statistics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DatabaseInfo {
    /// Database file.
    pub path: PathBuf,
    /// Size of the main database file in bytes.
    pub size_bytes: u64,
    /// Size of the write-ahead log in bytes.
    pub wal_size_bytes: u64,
    /// Current journal mode (expected `wal`).
    pub journal_mode: String,
    /// Highest applied system migration.
    pub schema_version: i64,
    /// Page size in bytes.
    pub page_size: i64,
    /// Number of pages.
    pub page_count: i64,
    /// Number of unused pages.
    pub freelist_count: i64,
    /// Version of the linked SQLite library.
    pub sqlite_version: String,
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
        if !opts.create_if_missing && !opts.path.is_file() {
            return Err(DbError::NotFound(opts.path));
        }
        if opts.create_if_missing && !opts.read_only && !opts.path.exists() {
            // Pre-create the file owner-only; SQLite gives -wal/-shm the same mode.
            create_private_file(&opts.path).map_err(|source| DbError::Io {
                path: opts.path.clone(),
                source,
            })?;
        }
        let synchronous = match opts.synchronous {
            Synchronous::Full => SqliteSynchronous::Full,
            Synchronous::Normal => SqliteSynchronous::Normal,
        };
        let base = SqliteConnectOptions::new()
            .filename(&opts.path)
            .foreign_keys(true)
            .busy_timeout(opts.busy_timeout)
            .synchronous(synchronous);

        let writer_opts = if opts.read_only {
            base.clone().read_only(true)
        } else {
            base.clone()
                .create_if_missing(opts.create_if_missing)
                .journal_mode(SqliteJournalMode::Wal)
                .optimize_on_close(true, 400u32)
        };
        let writer = SqlitePoolOptions::new()
            .max_connections(1)
            .min_connections(1)
            .acquire_timeout(ACQUIRE_TIMEOUT)
            .idle_timeout(None)
            .max_lifetime(None)
            .connect_with(writer_opts)
            .await
            .map_err(|e| classify(e, &opts.path))?;

        let journal_mode: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(&writer)
            .await
            .map_err(|e| classify(e, &opts.path))?;
        if !opts.read_only && !journal_mode.eq_ignore_ascii_case("wal") {
            writer.close().await;
            return Err(DbError::NotWal(journal_mode));
        }

        let reader_opts = base.read_only(opts.read_only).pragma("query_only", "ON");
        let reader = SqlitePoolOptions::new()
            .max_connections(opts.read_pool_size.max(1))
            .min_connections(0)
            .acquire_timeout(ACQUIRE_TIMEOUT)
            .connect_with(reader_opts)
            .await
            .map_err(|e| classify(e, &opts.path))?;

        tracing::debug!(path = %opts.path.display(), synchronous = %opts.synchronous, readers = opts.read_pool_size, "database opened");
        Ok(Self {
            inner: Arc::new(Inner {
                writer,
                reader,
                path: opts.path,
            }),
        })
    }

    /// Database file path.
    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /// Applies pending system migrations.
    ///
    /// When the database already has applied migrations and `backups_dir` is
    /// given, a consistent snapshot (`VACUUM INTO`) is written there first: that
    /// snapshot is the rollback path for forward-only migrations.
    pub async fn migrate(&self, backups_dir: Option<&Path>) -> Result<MigrateReport, DbError> {
        self.migrate_with(Migrator::system(), backups_dir).await
    }

    async fn migrate_with(&self, migrator: Migrator<'_>, backups_dir: Option<&Path>) -> Result<MigrateReport, DbError> {
        let mut conn = self.inner.writer.acquire().await.map_err(|e| self.classify(e))?;
        let status = migrator.status(&mut conn).await?;
        if let Some(problem) = status.problems.into_iter().next() {
            return Err(MigrationError::from(problem).into());
        }
        if status.pending.is_empty() {
            return Ok(MigrateReport {
                applied: Vec::new(),
                backup: None,
            });
        }

        let mut backup = None;
        if let (Some(dir), true) = (backups_dir, status.current_version > 0) {
            let stamp = forgebase_core::now_rfc3339().replace([':', '-'], "");
            let target = dir.join(format!("pre-migrate-v{}-{stamp}.db", status.current_version));
            std::fs::create_dir_all(dir).map_err(|source| DbError::Io {
                path: dir.to_path_buf(),
                source,
            })?;
            sqlx::query("VACUUM INTO ?1")
                .bind(target.to_string_lossy().into_owned())
                .execute(&mut *conn)
                .await
                .map_err(|e| self.classify(e))?;
            tracing::info!(backup = %target.display(), "wrote pre-migration snapshot");
            backup = Some(target);
        }

        let applied = migrator.run(&mut conn).await?;
        Ok(MigrateReport { applied, backup })
    }

    /// Applied / pending migrations, read without changing anything.
    pub async fn migration_status(&self) -> Result<MigrationStatus, DbError> {
        let mut conn = self.inner.reader.acquire().await.map_err(|e| self.classify(e))?;
        Ok(Migrator::system().status(&mut conn).await?)
    }

    /// Highest applied system migration (0 for a fresh file).
    pub async fn schema_version(&self) -> Result<i64, DbError> {
        Ok(self.migration_status().await?.current_version)
    }

    /// Cheap liveness probe: a read round-trip and an open writer.
    pub async fn ping(&self) -> Result<(), DbError> {
        if self.inner.writer.is_closed() {
            return Err(DbError::Closed);
        }
        let one: i64 = sqlx::query_scalar("SELECT 1")
            .fetch_one(&self.inner.reader)
            .await
            .map_err(|e| self.classify(e))?;
        debug_assert_eq!(one, 1);
        Ok(())
    }

    /// File sizes and engine statistics.
    pub async fn info(&self) -> Result<DatabaseInfo, DbError> {
        let r = &self.inner.reader;
        let c = |e| self.classify(e);
        let journal_mode: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(r)
            .await
            .map_err(c)?;
        let page_size: i64 = sqlx::query_scalar("PRAGMA page_size").fetch_one(r).await.map_err(c)?;
        let page_count: i64 = sqlx::query_scalar("PRAGMA page_count").fetch_one(r).await.map_err(c)?;
        let freelist_count: i64 = sqlx::query_scalar("PRAGMA freelist_count")
            .fetch_one(r)
            .await
            .map_err(c)?;
        let sqlite_version: String = sqlx::query_scalar("SELECT sqlite_version()")
            .fetch_one(r)
            .await
            .map_err(c)?;
        let schema_version = self.schema_version().await?;
        let size = |p: &Path| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        let mut wal = self.inner.path.clone().into_os_string();
        wal.push("-wal");
        Ok(DatabaseInfo {
            path: self.inner.path.clone(),
            size_bytes: size(&self.inner.path),
            wal_size_bytes: size(Path::new(&wal)),
            journal_mode: journal_mode.to_ascii_lowercase(),
            schema_version,
            page_size,
            page_count,
            freelist_count,
            sqlite_version,
        })
    }

    /// Runs `PRAGMA integrity_check`. Returns the problems found (empty = ok).
    /// Corruption severe enough to make the check itself fail is returned as
    /// a problem too.
    pub async fn integrity_check(&self) -> Result<Vec<String>, DbError> {
        let rows = sqlx::query("PRAGMA integrity_check")
            .fetch_all(&self.inner.reader)
            .await;
        match rows {
            Ok(rows) => {
                let mut problems = Vec::new();
                for row in rows {
                    let line: String = row.try_get(0).map_err(|e| self.classify(e))?;
                    if line != "ok" {
                        problems.push(line);
                    }
                }
                Ok(problems)
            }
            Err(e) => match self.classify(e) {
                DbError::Corrupt { message, .. } => Ok(vec![message]),
                other => Err(other),
            },
        }
    }

    /// Number of rows violating foreign keys (`PRAGMA foreign_key_check`).
    pub async fn foreign_key_violations(&self) -> Result<usize, DbError> {
        Ok(sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(&self.inner.reader)
            .await
            .map_err(|e| self.classify(e))?
            .len())
    }

    /// Effective settings of the writer (`false`) or a reader (`true`) connection.
    pub async fn effective_pragmas(&self, reader: bool) -> Result<EffectivePragmas, DbError> {
        let pool = if reader { &self.inner.reader } else { &self.inner.writer };
        let mut conn = pool.acquire().await.map_err(|e| self.classify(e))?;
        let c = |e| self.classify(e);
        Ok(EffectivePragmas {
            journal_mode: sqlx::query_scalar("PRAGMA journal_mode")
                .fetch_one(&mut *conn)
                .await
                .map_err(c)?,
            foreign_keys: sqlx::query_scalar("PRAGMA foreign_keys")
                .fetch_one(&mut *conn)
                .await
                .map_err(c)?,
            synchronous: sqlx::query_scalar("PRAGMA synchronous")
                .fetch_one(&mut *conn)
                .await
                .map_err(c)?,
            busy_timeout_ms: sqlx::query_scalar("PRAGMA busy_timeout")
                .fetch_one(&mut *conn)
                .await
                .map_err(c)?,
            query_only: sqlx::query_scalar("PRAGMA query_only")
                .fetch_one(&mut *conn)
                .await
                .map_err(c)?,
        })
    }

    /// Writes the WAL back into the main file and truncates it
    /// (`PRAGMA wal_checkpoint(TRUNCATE)`), leaving one self-contained file.
    pub async fn checkpoint(&self) -> Result<(), DbError> {
        let row = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
            .fetch_one(&self.inner.writer)
            .await
            .map_err(|e| self.classify(e))?;
        let busy: i64 = row.try_get(0).map_err(|e| self.classify(e))?;
        if busy != 0 {
            tracing::warn!("wal checkpoint could not complete: readers still active");
        }
        Ok(())
    }

    /// Closes both pools, waiting for in-flight queries.
    pub async fn close(&self) {
        self.inner.reader.close().await;
        self.inner.writer.close().await;
    }

    /// Reads a system metadata value.
    pub async fn meta_get(&self, key: &str) -> Result<Option<String>, DbError> {
        sqlx::query_scalar("SELECT value FROM _forgebase_meta WHERE key = ?1")
            .bind(key.to_owned())
            .fetch_optional(&self.inner.reader)
            .await
            .map_err(|e| self.classify(e))
    }

    /// Inserts or replaces a system metadata value. Durable when this returns.
    pub async fn meta_set(&self, key: &str, value: &str) -> Result<(), DbError> {
        sqlx::query(
            "INSERT INTO _forgebase_meta (key, value, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        )
        .bind(key.to_owned())
        .bind(value.to_owned())
        .bind(forgebase_core::now_rfc3339())
        .execute(&self.inner.writer)
        .await
        .map_err(|e| self.classify(e))?;
        Ok(())
    }

    /// Deletes a system metadata value. Returns whether it existed.
    pub async fn meta_delete(&self, key: &str) -> Result<bool, DbError> {
        let done = sqlx::query("DELETE FROM _forgebase_meta WHERE key = ?1")
            .bind(key.to_owned())
            .execute(&self.inner.writer)
            .await
            .map_err(|e| self.classify(e))?;
        Ok(done.rows_affected() > 0)
    }

    /// All system metadata, ordered by key.
    pub async fn meta_all(&self) -> Result<Vec<(String, String)>, DbError> {
        sqlx::query_as("SELECT key, value FROM _forgebase_meta ORDER BY key")
            .fetch_all(&self.inner.reader)
            .await
            .map_err(|e| self.classify(e))
    }

    /// Writes the instance id and initialisation time on first call and
    /// returns them on every call. Atomic across processes.
    pub async fn ensure_instance(&self) -> Result<InstanceInfo, DbError> {
        let mut tx = self
            .inner
            .writer
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|e| self.classify(e))?;
        let now = forgebase_core::now_rfc3339();
        for (key, value) in [
            ("instance_id", uuid::Uuid::new_v4().to_string()),
            ("initialized_at", now.clone()),
        ] {
            sqlx::query(
                "INSERT INTO _forgebase_meta (key, value, updated_at) VALUES (?1, ?2, ?3) ON CONFLICT (key) DO NOTHING",
            )
            .bind(key)
            .bind(value)
            .bind(now.clone())
            .execute(&mut *tx)
            .await
            .map_err(|e| self.classify(e))?;
        }
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT key, value FROM _forgebase_meta WHERE key IN ('instance_id', 'initialized_at')")
                .fetch_all(&mut *tx)
                .await
                .map_err(|e| self.classify(e))?;
        tx.commit().await.map_err(|e| self.classify(e))?;
        let get = |k: &str| {
            rows.iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.clone())
                .unwrap_or_default()
        };
        Ok(InstanceInfo {
            instance_id: get("instance_id"),
            initialized_at: get("initialized_at"),
        })
    }

    fn classify(&self, e: sqlx::Error) -> DbError {
        classify(e, &self.inner.path)
    }
}

/// Maps a driver error onto [`DbError`], recognising corruption and contention.
/// Creates an empty file readable and writable by the owner only (Unix).
/// SQLite treats an empty file as a new database.
fn create_private_file(path: &Path) -> std::io::Result<()> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    match o.open(path) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}

/// Version of the SQLite library linked into this binary.
pub async fn sqlite_library_version() -> Result<String, DbError> {
    use sqlx::Connection;
    let mut conn = sqlx::SqliteConnection::connect("sqlite::memory:").await?;
    let v: String = sqlx::query_scalar("SELECT sqlite_version()")
        .fetch_one(&mut conn)
        .await?;
    conn.close().await?;
    Ok(v)
}

fn classify(e: sqlx::Error, path: &Path) -> DbError {
    match &e {
        sqlx::Error::Database(db) => {
            let primary = db.code().and_then(|c| c.parse::<i32>().ok()).map(|c| c & 0xff);
            match primary {
                // SQLITE_CORRUPT, SQLITE_NOTADB
                Some(11) | Some(26) => DbError::Corrupt {
                    path: path.to_path_buf(),
                    message: db.message().to_owned(),
                },
                // SQLITE_BUSY, SQLITE_LOCKED
                Some(5) | Some(6) => DbError::Busy(db.message().to_owned()),
                _ => DbError::Sqlx(e),
            }
        }
        sqlx::Error::PoolTimedOut => DbError::Busy("timed out waiting for a database connection".into()),
        sqlx::Error::PoolClosed => DbError::Closed,
        _ => DbError::Sqlx(e),
    }
}

/// Database errors.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    /// The database file does not exist and creation was not requested.
    #[error("database file {0} does not exist (run `forgebase init`)")]
    NotFound(PathBuf),
    /// The file is not a SQLite database or is damaged.
    #[error("database file {path} is corrupt or not a SQLite database: {message}")]
    Corrupt {
        /// File path.
        path: PathBuf,
        /// SQLite message.
        message: String,
    },
    /// WAL mode could not be enabled (e.g. unsupported filesystem).
    #[error("database could not be switched to WAL mode (journal_mode is {0:?})")]
    NotWal(String),
    /// Lock contention outlasted the busy timeout.
    #[error("database is busy: {0}")]
    Busy(String),
    /// The database has been closed.
    #[error("database is closed")]
    Closed,
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
    /// Any other driver error.
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
}

impl ErrorCode for DbError {
    fn code(&self) -> &'static str {
        match self {
            DbError::NotFound(_) => "database_not_found",
            DbError::Corrupt { .. } => "database_corrupt",
            DbError::NotWal(_) => "database_not_wal",
            DbError::Busy(_) => "database_busy",
            DbError::Closed => "database_closed",
            DbError::Migration(m) => m.code(),
            DbError::Io { .. } => "io_error",
            DbError::Sqlx(_) => "database_error",
        }
    }

    fn kind(&self) -> ErrorKind {
        match self {
            DbError::NotFound(_) | DbError::Corrupt { .. } | DbError::Busy(_) | DbError::Closed => {
                ErrorKind::Unavailable
            }
            DbError::Migration(m) => m.kind(),
            DbError::NotWal(_) | DbError::Io { .. } | DbError::Sqlx(_) => ErrorKind::Internal,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forgebase_core::config::DatabaseConfig;

    fn opts(path: &Path) -> DbOptions {
        DbOptions::from_config(
            &DatabaseConfig {
                synchronous: Synchronous::Full,
                busy_timeout_ms: 5000,
                read_pool_size: 4,
            },
            path,
        )
    }

    async fn fresh(dir: &Path) -> Database {
        let db = Database::open(opts(&dir.join("forgebase.db"))).await.unwrap();
        db.migrate(Some(&dir.join("backups"))).await.unwrap();
        db
    }

    #[tokio::test]
    async fn pragmas_are_applied_to_writer_and_readers() {
        let tmp = tempfile::tempdir().unwrap();
        let db = fresh(tmp.path()).await;
        let w = db.effective_pragmas(false).await.unwrap();
        assert_eq!(w.journal_mode, "wal");
        assert_eq!(w.foreign_keys, 1);
        assert_eq!(w.synchronous, 2, "FULL");
        assert_eq!(w.busy_timeout_ms, 5000);
        assert_eq!(w.query_only, 0);
        let r = db.effective_pragmas(true).await.unwrap();
        assert_eq!(r.foreign_keys, 1);
        assert_eq!(r.query_only, 1, "readers cannot write");
        db.close().await;

        let mut normal = opts(&tmp.path().join("forgebase.db"));
        normal.synchronous = Synchronous::Normal;
        let db = Database::open(normal).await.unwrap();
        assert_eq!(db.effective_pragmas(false).await.unwrap().synchronous, 1, "NORMAL");
        db.close().await;
    }

    #[tokio::test]
    async fn writes_survive_close_and_reopen() {
        let tmp = tempfile::tempdir().unwrap();
        let db = fresh(tmp.path()).await;
        let instance = db.ensure_instance().await.unwrap();
        assert_eq!(instance.instance_id.len(), 36);
        db.meta_set("greeting", "hello").await.unwrap();
        db.meta_set("greeting", "hello again").await.unwrap();
        db.checkpoint().await.unwrap();
        db.close().await;

        let db = Database::open(opts(&tmp.path().join("forgebase.db")).create_if_missing(false))
            .await
            .unwrap();
        assert_eq!(db.meta_get("greeting").await.unwrap().as_deref(), Some("hello again"));
        assert_eq!(db.ensure_instance().await.unwrap(), instance, "instance id is stable");
        assert!(db.meta_delete("greeting").await.unwrap());
        assert_eq!(db.meta_get("greeting").await.unwrap(), None);
        let info = db.info().await.unwrap();
        assert_eq!(info.journal_mode, "wal");
        assert_eq!(info.schema_version, Migrator::system().latest_version());
        assert!(info.size_bytes > 0);
        assert!(db.integrity_check().await.unwrap().is_empty());
        assert_eq!(db.foreign_key_violations().await.unwrap(), 0);
        db.close().await;
    }

    #[tokio::test]
    async fn migrate_is_idempotent_and_needs_no_backup_when_current() {
        let tmp = tempfile::tempdir().unwrap();
        let db = fresh(tmp.path()).await;
        let again = db.migrate(Some(&tmp.path().join("backups"))).await.unwrap();
        assert!(again.applied.is_empty());
        assert_eq!(again.backup, None);
        assert!(db.migration_status().await.unwrap().is_up_to_date());
        db.close().await;
    }

    #[tokio::test]
    async fn existing_database_is_snapshotted_before_new_migrations() {
        use forgebase_migrations::Migration;
        static V1: &[Migration] = &[Migration::new(
            1,
            "one",
            "CREATE TABLE a (x INTEGER); INSERT INTO a VALUES (7);",
        )];
        static V2: &[Migration] = &[
            Migration::new(1, "one", "CREATE TABLE a (x INTEGER); INSERT INTO a VALUES (7);"),
            Migration::new(2, "two", "DROP TABLE a;"),
        ];
        let tmp = tempfile::tempdir().unwrap();
        let backups = tmp.path().join("backups");
        let db = Database::open(opts(&tmp.path().join("forgebase.db"))).await.unwrap();
        let first = db
            .migrate_with(Migrator::new(V1).unwrap(), Some(&backups))
            .await
            .unwrap();
        assert_eq!(first.backup, None, "fresh database needs no snapshot");
        let second = db
            .migrate_with(Migrator::new(V2).unwrap(), Some(&backups))
            .await
            .unwrap();
        assert_eq!(second.applied.len(), 1);
        let snapshot = second.backup.expect("snapshot written");
        db.close().await;

        let snap = Database::open(opts(&snapshot).create_if_missing(false)).await.unwrap();
        let x: i64 = sqlx::query_scalar("SELECT x FROM a")
            .fetch_one(&snap.inner.reader)
            .await
            .unwrap();
        assert_eq!(x, 7, "snapshot holds the pre-migration data");
        snap.close().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn new_database_files_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let db = fresh(tmp.path()).await;
        db.meta_set("k", "v").await.unwrap();
        for name in ["forgebase.db", "forgebase.db-wal"] {
            let mode = std::fs::metadata(tmp.path().join(name)).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "{name}");
        }
        db.close().await;
    }

    #[tokio::test]
    async fn readers_cannot_write() {
        let tmp = tempfile::tempdir().unwrap();
        let db = fresh(tmp.path()).await;
        let err = sqlx::query("INSERT INTO _forgebase_meta (key, value) VALUES ('x', 'y')")
            .execute(&db.inner.reader)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("readonly")
                || err.to_string().contains("read-only")
                || err.to_string().contains("query_only"),
            "{err}"
        );
        db.close().await;
    }

    #[tokio::test]
    async fn concurrent_reads_during_writes() {
        let tmp = tempfile::tempdir().unwrap();
        let db = fresh(tmp.path()).await;
        let writer = {
            let db = db.clone();
            tokio::spawn(async move {
                for i in 0..200 {
                    db.meta_set(&format!("k{i:03}"), &i.to_string()).await.unwrap();
                }
            })
        };
        let readers: Vec<_> = (0..4)
            .map(|_| {
                let db = db.clone();
                tokio::spawn(async move {
                    let mut last = 0;
                    for _ in 0..100 {
                        let n = db.meta_all().await.unwrap().len();
                        assert!(n >= last, "a reader never sees rows disappear");
                        last = n;
                    }
                })
            })
            .collect();
        writer.await.unwrap();
        for r in readers {
            r.await.unwrap();
        }
        assert_eq!(db.meta_all().await.unwrap().len(), 200);
        db.close().await;
    }

    #[tokio::test]
    async fn read_only_open_cannot_write() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("forgebase.db");
        let db = fresh(tmp.path()).await;
        db.meta_set("k", "v").await.unwrap();
        db.checkpoint().await.unwrap();
        db.close().await;
        let ro = Database::open(opts(&path).read_only()).await.unwrap();
        assert_eq!(ro.meta_get("k").await.unwrap().as_deref(), Some("v"));
        assert!(ro.meta_set("k", "w").await.is_err());
        assert!(ro.integrity_check().await.unwrap().is_empty());
        ro.close().await;
        assert!(!sqlite_library_version().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn missing_file_is_not_created_without_permission() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("forgebase.db");
        let err = Database::open(opts(&path).create_if_missing(false)).await.unwrap_err();
        assert_eq!(err.code(), "database_not_found");
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn garbage_file_is_reported_corrupt() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("forgebase.db");
        std::fs::write(&path, vec![0x5a; 8192]).unwrap();
        let err = Database::open(opts(&path).create_if_missing(false)).await.unwrap_err();
        assert_eq!(err.code(), "database_corrupt", "{err}");
    }

    #[tokio::test]
    async fn damaged_pages_fail_the_integrity_check() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("forgebase.db");
        let db = fresh(tmp.path()).await;
        for i in 0..300 {
            db.meta_set(&format!("key-{i:04}"), &"x".repeat(200)).await.unwrap();
        }
        let page_size = db.info().await.unwrap().page_size as usize;
        db.checkpoint().await.unwrap();
        db.close().await;

        let mut bytes = std::fs::read(&path).unwrap();
        assert!(bytes.len() > page_size * 4);
        // Overwrite every page after the schema page with junk.
        for b in &mut bytes[page_size..] {
            *b = 0xff;
        }
        std::fs::write(&path, &bytes).unwrap();

        match Database::open(opts(&path).create_if_missing(false)).await {
            Err(e) => assert_eq!(e.code(), "database_corrupt", "{e}"),
            Ok(db) => {
                let problems = db.integrity_check().await.unwrap();
                assert!(!problems.is_empty(), "integrity_check must report the damage");
                db.close().await;
            }
        }
    }
}
