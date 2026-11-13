//! The embedded SQLite database (ADR-0001): one writer connection, a read
//! pool, WAL mode. Reached through the [`crate::Database`] facade.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use forgebase_core::Synchronous;
use forgebase_migrations::{MigrationError, MigrationStatus, Migrator};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Row, SqlitePool};

use crate::{
    ACQUIRE_TIMEOUT, ConstraintKind, DatabaseInfo, DbError, DbOptions, EffectivePragmas, InstanceInfo, MigrateReport,
    PoolStats, PoolUsage, sqlite_backend,
};

/// Handle to an open SQLite database. Cheap to clone.
#[derive(Debug, Clone)]
pub(crate) struct SqliteDatabase {
    pub(crate) inner: Arc<SqliteInner>,
}

#[derive(Debug)]
pub(crate) struct SqliteInner {
    pub(crate) writer: SqlitePool,
    pub(crate) reader: SqlitePool,
    pub(crate) path: PathBuf,
    pub(crate) slow: crate::slow_log::SlowLog,
    /// Bumped on every committed schema change; readers compare it on acquire.
    pub(crate) schema_gen: Arc<std::sync::atomic::AtomicU64>,
}

impl SqliteDatabase {
    /// Opens (and with `create_if_missing`, creates) the database.
    /// Does not run migrations; call [`Database::migrate`].
    pub(crate) async fn open(path: &Path, opts: &DbOptions) -> Result<Self, DbError> {
        if !opts.create_if_missing && !path.is_file() {
            return Err(DbError::NotFound(path.to_path_buf()));
        }
        check_wal_page_size(path)?;
        if opts.create_if_missing && !opts.read_only && !path.exists() {
            // Pre-create the file owner-only; SQLite gives -wal/-shm the same mode.
            create_private_file(path).map_err(|source| DbError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        }
        let synchronous = match opts.synchronous {
            Synchronous::Full => SqliteSynchronous::Full,
            Synchronous::Normal => SqliteSynchronous::Normal,
        };
        let base = SqliteConnectOptions::new()
            .filename(path)
            .foreign_keys(true)
            .busy_timeout(opts.busy_timeout)
            .synchronous(synchronous)
            // `REGEXP` for the filter language's `~` with a non-ASCII value
            // (`Dialect::regexp_ci`): SQLite's `LIKE` folds ASCII only.
            .with_regexp();

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
            .map_err(|e| classify(e, path))?;

        let journal_mode: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(&writer)
            .await
            .map_err(|e| classify(e, path))?;
        if !opts.read_only && !journal_mode.eq_ignore_ascii_case("wal") {
            writer.close().await;
            return Err(DbError::NotWal(journal_mode));
        }

        let reader_opts = base.read_only(opts.read_only).pragma("query_only", "ON");
        let schema_gen = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let seen = Arc::new(sqlite_backend::SeenGenerations::default());
        let hook_gen = Arc::clone(&schema_gen);
        let reader = SqlitePoolOptions::new()
            .before_acquire(move |conn, _meta| {
                let (seen, hook_gen) = (Arc::clone(&seen), Arc::clone(&hook_gen));
                Box::pin(async move {
                    let current = hook_gen.load(std::sync::atomic::Ordering::Acquire);
                    seen.refresh_if_behind(conn, current).await?;
                    Ok(true)
                })
            })
            .max_connections(opts.read_pool_size.max(1))
            .min_connections(0)
            .acquire_timeout(ACQUIRE_TIMEOUT)
            .connect_with(reader_opts)
            .await
            .map_err(|e| classify(e, path))?;

        tracing::debug!(path = %path.display(), synchronous = %opts.synchronous, readers = opts.read_pool_size, "database opened");
        Ok(Self {
            inner: Arc::new(SqliteInner {
                writer,
                reader,
                path: path.to_path_buf(),
                slow: crate::slow_log::SlowLog::default(),
                schema_gen,
            }),
        })
    }

    pub(crate) fn bump_schema_generation(&self) {
        self.inner.schema_gen.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
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

        self.bump_schema_generation();
        let applied = migrator.run(&mut conn).await;
        self.bump_schema_generation();
        let applied = applied?;
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

    /// Connections of the writer and reader pools right now.
    pub fn pool_stats(&self) -> PoolStats {
        let one = |p: &SqlitePool| {
            let open = p.size();
            let idle = u32::try_from(p.num_idle()).unwrap_or(u32::MAX).min(open);
            PoolUsage {
                open,
                idle,
                in_use: open - idle,
                max: p.options().get_max_connections(),
            }
        };
        PoolStats {
            writer: one(&self.inner.writer),
            reader: one(&self.inner.reader),
        }
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
            backend: crate::BackendKind::Sqlite,
            server_version: sqlite_version.clone(),
            connections: None,
            features: Vec::new(),
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

    /// Writes a consistent, self-contained copy of the database to `target`
    /// (`VACUUM INTO`, FB-83): one transaction snapshot of the committed state,
    /// taken while the server keeps serving (readers are never blocked; in WAL
    /// mode the single writer is only held for its own statement). The target
    /// must not exist. The copy has no WAL and needs no recovery.
    ///
    /// Works on a read-only handle too, so `forgebase backup` can snapshot a
    /// database a running server owns.
    pub async fn snapshot_to(&self, target: &Path) -> Result<(), DbError> {
        if target.exists() {
            return Err(DbError::Io {
                path: target.to_path_buf(),
                source: std::io::Error::new(std::io::ErrorKind::AlreadyExists, "snapshot target exists"),
            });
        }
        sqlx::query("VACUUM INTO ?1")
            .bind(target.to_string_lossy().into_owned())
            .execute(&self.inner.writer)
            .await
            .map_err(|e| self.classify(e))?;
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

    pub(crate) fn classify(&self, e: sqlx::Error) -> DbError {
        classify(e, &self.inner.path)
    }
}

/// Maps a driver error onto [`DbError`], recognising corruption and contention.
/// Refuses to open a database whose sibling `-wal` file was written with a
/// different page size than the database header declares.
///
/// Mitigation for RUSTSEC-2026-0459 (bundled SQLite older than 3.54, held back
/// by `sqlx-sqlite`; see ADR-0002): recovery of a WAL whose page size differs
/// from the database's is what the advisory exploits, so a pair that does not
/// match is rejected before SQLite sees it. A missing, empty or short file, a
/// WAL without a valid header magic (SQLite ignores it) and a database file
/// shorter than its 100-byte header are not checked.
fn check_wal_page_size(path: &Path) -> Result<(), DbError> {
    use std::io::Read;

    let mut wal_name = path.as_os_str().to_owned();
    wal_name.push("-wal");
    let wal = PathBuf::from(wal_name);
    let io = |p: &Path| {
        let p = p.to_path_buf();
        move |source| DbError::Io { path: p, source }
    };

    let mut wal_header = [0u8; 32];
    match std::fs::File::open(&wal) {
        Ok(mut f) => {
            if f.read_exact(&mut wal_header).is_err() {
                return Ok(()); // empty or truncated: SQLite discards it
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(io(&wal)(e)),
    }
    let magic = u32::from_be_bytes([wal_header[0], wal_header[1], wal_header[2], wal_header[3]]);
    if magic != 0x377f_0682 && magic != 0x377f_0683 {
        return Ok(());
    }
    let wal_page_size = u32::from_be_bytes([wal_header[8], wal_header[9], wal_header[10], wal_header[11]]);

    let mut db_header = [0u8; 100];
    match std::fs::File::open(path) {
        Ok(mut f) => {
            if f.read_exact(&mut db_header).is_err() {
                return Ok(()); // new or empty database
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(io(path)(e)),
    }
    // The header stores the page size in two bytes; the value 1 means 65536.
    let db_page_size = match u16::from_be_bytes([db_header[16], db_header[17]]) {
        1 => 65_536,
        n => u32::from(n),
    };
    if db_page_size != wal_page_size {
        return Err(DbError::WalPageSizeMismatch {
            path: path.to_path_buf(),
            wal,
            db_page_size,
            wal_page_size,
        });
    }
    Ok(())
}

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
                // SQLITE_READONLY: a write reached a read connection.
                Some(8) => DbError::ReadOnly(db.message().to_owned()),
                // SQLITE_CONSTRAINT: classify by extended code.
                Some(19) => {
                    let extended = db.code().and_then(|c| c.parse::<i32>().ok()).unwrap_or(19);
                    let kind = match extended {
                        2067 | 1555 => ConstraintKind::Unique,
                        787 => ConstraintKind::ForeignKey,
                        275 => ConstraintKind::Check,
                        1299 => ConstraintKind::NotNull,
                        _ => ConstraintKind::Other,
                    };
                    DbError::Constraint {
                        kind,
                        message: db.message().to_owned(),
                    }
                }
                // SQLITE_ERROR (syntax, unknown table/column, ...), SQLITE_RANGE,
                // SQLITE_MISMATCH, SQLITE_TOOBIG: the statement is at fault.
                Some(1) | Some(25) | Some(20) | Some(18) => DbError::Statement(db.message().to_owned()),
                _ => DbError::Sqlx(e),
            }
        }
        sqlx::Error::PoolTimedOut => DbError::Busy("timed out waiting for a database connection".into()),
        sqlx::Error::PoolClosed => DbError::Closed,
        _ => DbError::Sqlx(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Database, DbOptions};
    use forgebase_core::ErrorCode;
    use forgebase_core::config::DatabaseConfig;
    use forgebase_migrations::Migrator;

    fn opts(path: &Path) -> DbOptions {
        DbOptions::from_config(
            &DatabaseConfig {
                synchronous: Synchronous::Full,
                busy_timeout_ms: 5000,
                read_pool_size: 4,
                ..forgebase_core::config::Config::defaults(Path::new(".")).database
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
            .sqlite()
            .unwrap()
            .migrate_with(Migrator::new(V1).unwrap(), Some(&backups))
            .await
            .unwrap();
        assert_eq!(first.backup, None, "fresh database needs no snapshot");
        let second = db
            .sqlite()
            .unwrap()
            .migrate_with(Migrator::new(V2).unwrap(), Some(&backups))
            .await
            .unwrap();
        assert_eq!(second.applied.len(), 1);
        let snapshot = second.backup.expect("snapshot written");
        db.close().await;

        let snap = Database::open(opts(&snapshot).create_if_missing(false)).await.unwrap();
        let x: i64 = sqlx::query_scalar("SELECT x FROM a")
            .fetch_one(&snap.sqlite().unwrap().inner.reader)
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
            .execute(&db.sqlite().unwrap().inner.reader)
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
    async fn snapshot_to_is_consistent_online_and_read_only() {
        let tmp = tempfile::tempdir().unwrap();
        let db = fresh(tmp.path()).await;
        db.meta_set("k", "v1").await.unwrap();
        let snap = tmp.path().join("snap.db");
        db.snapshot_to(&snap).await.unwrap();
        assert!(db.snapshot_to(&snap).await.is_err(), "existing target is refused");
        db.meta_set("k", "v2").await.unwrap();

        // The copy is self-contained and shows the state at snapshot time.
        let copy = Database::open(opts(&snap).read_only()).await.unwrap();
        assert_eq!(copy.meta_get("k").await.unwrap().as_deref(), Some("v1"));
        assert!(copy.integrity_check().await.unwrap().is_empty());
        copy.close().await;

        // A read-only handle on the live file can snapshot it as well.
        let ro = Database::open(opts(&tmp.path().join("forgebase.db")).read_only())
            .await
            .unwrap();
        let snap2 = tmp.path().join("snap2.db");
        ro.snapshot_to(&snap2).await.unwrap();
        ro.close().await;
        let copy2 = Database::open(opts(&snap2).read_only()).await.unwrap();
        assert_eq!(copy2.meta_get("k").await.unwrap().as_deref(), Some("v2"));
        copy2.close().await;
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

    /// A database/WAL pair with the given page sizes, written by hand: only the
    /// two headers matter to the check.
    fn crafted_pair(dir: &Path, db_page: u16, wal_page: u32, magic: u32) -> PathBuf {
        let path = dir.join("crafted.db");
        let mut db = vec![0u8; 4096];
        db[..16].copy_from_slice(b"SQLite format 3\0");
        db[16..18].copy_from_slice(&db_page.to_be_bytes());
        std::fs::write(&path, db).unwrap();
        let mut wal = vec![0u8; 32];
        wal[..4].copy_from_slice(&magic.to_be_bytes());
        wal[4..8].copy_from_slice(&3_007_000u32.to_be_bytes());
        wal[8..12].copy_from_slice(&wal_page.to_be_bytes());
        std::fs::write(dir.join("crafted.db-wal"), wal).unwrap();
        path
    }

    #[tokio::test]
    async fn a_wal_with_another_page_size_than_the_database_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let path = crafted_pair(tmp.path(), 4096, 8192, 0x377f_0682);
        let Err(err) = Database::open(opts(&path)).await else {
            panic!("a mismatched WAL must be refused");
        };
        match &err {
            DbError::WalPageSizeMismatch {
                db_page_size,
                wal_page_size,
                ..
            } => assert_eq!((*db_page_size, *wal_page_size), (4096, 8192)),
            other => panic!("wrong error: {other}"),
        }
        assert!(err.to_string().contains("page size 8192"), "{err}");
        assert_eq!(ErrorCode::code(&err), "database_wal_mismatch");
        // Also refused read-only, and nothing was modified.
        let ro = DbOptions {
            read_only: true,
            create_if_missing: false,
            ..opts(&path)
        };
        assert!(matches!(
            Database::open(ro).await.err(),
            Some(DbError::WalPageSizeMismatch { .. })
        ));
        assert_eq!(std::fs::metadata(tmp.path().join("crafted.db-wal")).unwrap().len(), 32);
    }

    #[test]
    fn matching_page_sizes_and_unrelated_wal_files_pass_the_check() {
        let tmp = tempfile::tempdir().unwrap();
        let ok = crafted_pair(tmp.path(), 4096, 4096, 0x377f_0683);
        check_wal_page_size(&ok).unwrap();
        // 1 in the header means 65536.
        let big = crafted_pair(tmp.path(), 1, 65_536, 0x377f_0682);
        check_wal_page_size(&big).unwrap();
        // A WAL without the magic number is ignored by SQLite, so it is not judged here.
        let junk = crafted_pair(tmp.path(), 4096, 8192, 0xdead_beef);
        check_wal_page_size(&junk).unwrap();
        // No WAL, an empty WAL and a new database are fine.
        let none = tmp.path().join("none.db");
        check_wal_page_size(&none).unwrap();
        std::fs::write(tmp.path().join("empty.db-wal"), b"").unwrap();
        check_wal_page_size(&tmp.path().join("empty.db")).unwrap();
    }

    #[tokio::test]
    async fn a_real_database_reopens_after_a_clean_shutdown_and_with_a_live_wal() {
        let tmp = tempfile::tempdir().unwrap();
        let db = fresh(tmp.path()).await;
        // While open, the -wal file exists with the real page size: a second open passes the check.
        assert!(tmp.path().join("forgebase.db-wal").exists());
        check_wal_page_size(&tmp.path().join("forgebase.db")).unwrap();
        db.close().await;
        let db = Database::open(opts(&tmp.path().join("forgebase.db"))).await.unwrap();
        db.close().await;
    }
}
