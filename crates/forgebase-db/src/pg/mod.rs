//! [`Backend`] implementation for PostgreSQL (ADR-0028 §7.3).
//!
//! * **One writer** (`max_connections = 1`): every write transaction queues
//!   for the same connection, as on SQLite, so sequence order is commit order
//!   (realtime and queue cursors, ADR-0011/0022) and read-modify-write code
//!   stays correct. `TxMode::Normal` is `BEGIN ISOLATION LEVEL READ COMMITTED`;
//!   `TxMode::SchemaChange` adds `SET CONSTRAINTS ALL DEFERRED`, so foreign
//!   keys (created `DEFERRABLE`) are checked at `COMMIT`.
//! * **A read pool.** Each read is one statement in its own
//!   `BEGIN READ ONLY … ROLLBACK`: a write fails with `25006`
//!   ([`DbError::ReadOnly`]) and anything the statement changed in the session
//!   (`SET`, `set_config`) is undone. Reads go through the extended protocol,
//!   which carries exactly one statement.
//! * **Statements**: placeholders are translated ([`placeholders`]), the
//!   statement is prepared (cached per connection by sqlx), and every value is
//!   converted to the parameter type the server reported ([`types`]).
//! * **Schema changes**: a committed DDL statement bumps a generation number;
//!   each pooled connection keeps the number it last saw in the session
//!   setting `forgebase.schema_gen` and clears its statement cache when it is
//!   behind (the cached plans' result columns may have changed).

pub(crate) mod error;
pub(crate) mod extensions;
pub(crate) mod placeholders;
pub(crate) mod twophase;
pub(crate) mod types;

use std::collections::HashMap;
use std::path::Path;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use forgebase_migrations::postgres::{self as pg_migrations, PgMigrator};
use forgebase_migrations::{MigrationError, MigrationStatus};
use sqlx::pool::PoolConnection;
use sqlx::postgres::types::Oid;
use sqlx::postgres::{PgArguments, PgConnectOptions, PgPool, PgPoolOptions, PgRow, PgTypeInfo};
use sqlx::{Arguments, AssertSqlSafe, Column, Connection, Executor, Postgres, Row, SqlSafeStr, Statement as _};

use crate::backend::{
    Backend, BoxFuture, ExecResult, PlanStep, Rows, RunAs, SlowQuery, Statement, TxMode, Value, WriteTx,
};
use crate::dialect::{Dialect, PgDialect};
use crate::routing::{Lsn, LsnWait, PrimaryOnly, ReadRequest, ReadRouter, ReadTarget};
use crate::slow_log::SlowLog;
use crate::sqlite_backend::changes_schema;
use crate::{
    ACQUIRE_TIMEOUT, BackendKind, DatabaseInfo, DbError, DbOptions, InstanceInfo, MigrateReport, PoolStats, PoolUsage,
    SecretString,
};

use placeholders::Translated;
use types::PgParam;

static PG: PgDialect = PgDialect;

/// Prepared statements kept per connection.
const STATEMENT_CACHE: usize = 512;
/// Translated SQL strings kept (shared by all connections).
const SQL_CACHE: usize = 4096;
/// Oldest server major version accepted (ADR-0028 decision 1).
const MIN_SERVER_VERSION: u32 = 160_000;
/// First server major version with `WAIT FOR LSN` (ADR-0029 §2.6).
const WAIT_FOR_LSN_VERSION: u32 = 190_000;

/// Handle to an open PostgreSQL database. Cheap to clone.
#[derive(Debug, Clone)]
pub(crate) struct PgDatabase {
    inner: Arc<PgInner>,
}

#[derive(Debug)]
struct PgInner {
    writer: PgPool,
    reader: ReadPools,
    /// WAL position after the latest commit (0 = none yet).
    last_commit_lsn: AtomicU64,
    /// `server_version_num` at open.
    server_version: u32,
    /// `WAIT FOR LSN` failed to parse once: the engine lacks it.
    no_wait_for: std::sync::atomic::AtomicBool,
    /// URL without the password, for messages.
    location: String,
    managed: bool,
    read_only: bool,
    slow: SlowLog,
    /// Bumped around every committed schema change.
    schema_gen: AtomicU64,
    /// Generation the writer connection's statement cache matches.
    writer_gen: AtomicU64,
    sql: Mutex<HashMap<String, Arc<Translated>>>,
    /// Parameters the server cannot infer, per translated SQL text.
    indeterminate: Mutex<HashMap<String, Arc<[usize]>>>,
    /// The latest [`extensions`] setup of this handle.
    extensions: RwLock<Option<Arc<extensions::ExtensionReport>>>,
}

/// A failure while running a statement: the driver's (the server may have
/// aborted the transaction) or ours (nothing was sent).
enum RunError {
    Driver(sqlx::Error),
    Local(DbError),
}

impl From<sqlx::Error> for RunError {
    fn from(e: sqlx::Error) -> Self {
        RunError::Driver(e)
    }
}

impl From<DbError> for RunError {
    fn from(e: DbError) -> Self {
        RunError::Local(e)
    }
}

/// `postgres://user@host:port/db` (or `…?host=/socket`), never the password.
fn describe_target(o: &PgConnectOptions) -> String {
    let db = o.get_database().unwrap_or(o.get_username());
    match o.get_socket() {
        Some(dir) => format!("postgres://{}@/{db}?host={}", o.get_username(), dir.display()),
        None => format!("postgres://{}@{}:{}/{db}", o.get_username(), o.get_host(), o.get_port()),
    }
}

fn base_options(url: &SecretString) -> Result<PgConnectOptions, DbError> {
    let opts = PgConnectOptions::from_str(url.expose())
        .map_err(|e| DbError::Unavailable(format!("invalid PostgreSQL URL: {e}")))?;
    Ok(opts
        .application_name("forgebase")
        .statement_cache_capacity(STATEMENT_CACHE)
        .options([
            ("DateStyle", "ISO"),
            ("TimeZone", "UTC"),
            ("IntervalStyle", "postgres"),
            ("extra_float_digits", "3"),
        ]))
}

impl PgDatabase {
    pub(crate) async fn open(url: &SecretString, managed: bool, opts: &DbOptions) -> Result<Self, DbError> {
        let base = base_options(url)?;
        let location = describe_target(&base);
        let writer_opts = if opts.read_only {
            base.clone().options([("default_transaction_read_only", "on")])
        } else {
            base.clone()
        };
        // A direct connection first: a missing database or an unreachable
        // server fails at once (a pool would retry until its timeout).
        let probe = match sqlx::PgConnection::connect_with(&writer_opts).await {
            Ok(conn) => conn,
            Err(e) if error::sqlstate(&e).as_deref() == Some("3D000") && opts.create_if_missing && !opts.read_only => {
                create_database(&base, &location).await?;
                sqlx::PgConnection::connect_with(&writer_opts)
                    .await
                    .map_err(|e| error::classify(e, &location))?
            }
            Err(e) => return Err(error::classify(e, &location)),
        };
        let version = probe.server_version_num().unwrap_or(0);
        let _ = probe.close().await;
        if version < MIN_SERVER_VERSION {
            return Err(DbError::Unavailable(format!(
                "{location}: PostgreSQL 16 or newer is required (server reports version number {version})"
            )));
        }
        let writer = PgPoolOptions::new()
            .max_connections(1)
            .min_connections(1)
            .acquire_timeout(ACQUIRE_TIMEOUT)
            .idle_timeout(None)
            .max_lifetime(None)
            .connect_with(writer_opts)
            .await
            .map_err(|e| error::classify(e, &location))?;
        let reader = PgPoolOptions::new()
            .max_connections(opts.read_pool_size.max(1))
            .min_connections(0)
            .acquire_timeout(ACQUIRE_TIMEOUT)
            .connect_with(base.options([("default_transaction_read_only", "on")]))
            .await
            .map_err(|e| error::classify(e, &location))?;
        tracing::debug!(%location, managed, readers = opts.read_pool_size, "postgres database opened");
        Ok(Self {
            inner: Arc::new(PgInner {
                writer,
                reader: ReadPools {
                    primary: reader,
                    replicas: Vec::new(),
                    router: Arc::new(PrimaryOnly),
                },
                last_commit_lsn: AtomicU64::new(0),
                server_version: version,
                no_wait_for: std::sync::atomic::AtomicBool::new(false),
                location,
                managed,
                read_only: opts.read_only,
                slow: SlowLog::default(),
                schema_gen: AtomicU64::new(1),
                writer_gen: AtomicU64::new(0),
                sql: Mutex::new(HashMap::new()),
                indeterminate: Mutex::new(HashMap::new()),
                extensions: RwLock::new(None),
            }),
        })
    }

    /// The report of the latest [`extensions`] setup, if it ran.
    pub(crate) fn extension_report(&self) -> Option<Arc<extensions::ExtensionReport>> {
        self.inner.extensions.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    fn store_extensions(&self, report: extensions::ExtensionReport) {
        *self.inner.extensions.write().unwrap_or_else(|p| p.into_inner()) = Some(Arc::new(report));
    }

    pub(crate) fn location(&self) -> &str {
        &self.inner.location
    }

    fn classify(&self, e: sqlx::Error) -> DbError {
        error::classify(e, &self.inner.location)
    }

    fn run_error(&self, e: RunError) -> DbError {
        match e {
            RunError::Driver(e) => self.classify(e),
            RunError::Local(e) => e,
        }
    }

    fn bump_schema_generation(&self) {
        self.inner.schema_gen.fetch_add(1, Ordering::AcqRel);
    }

    /// `?N` → `$N`, cached per SQL text.
    fn translate(&self, sql: &str) -> Result<Arc<Translated>, DbError> {
        if let Some(t) = self.inner.sql.lock().unwrap_or_else(|p| p.into_inner()).get(sql) {
            return Ok(Arc::clone(t));
        }
        let t = Arc::new(placeholders::translate(sql).map_err(|e| DbError::Statement(e.to_string()))?);
        let mut cache = self.inner.sql.lock().unwrap_or_else(|p| p.into_inner());
        if cache.len() >= SQL_CACHE {
            cache.clear();
        }
        cache.insert(sql.to_owned(), Arc::clone(&t));
        Ok(t)
    }

    /// Runs one statement inside a `READ ONLY` transaction on a reader.
    async fn read(&self, stmt: &Statement) -> Result<Rows, DbError> {
        self.read_with(stmt, None).await
    }

    /// [`PgDatabase::read`], optionally as `run_as` (FB-152): the read-only
    /// transaction switches role and sets the claims before the statement;
    /// the `ROLLBACK` that ends it undoes both.
    async fn read_with(&self, stmt: &Statement, run_as: Option<&RunAs>) -> Result<Rows, DbError> {
        if let Some(r) = run_as {
            r.check()?;
        }
        let started = Instant::now();
        let pool = self.inner.reader.pick(stmt);
        let mut conn = pool.acquire().await.map_err(|e| self.classify(e))?;
        let gen_now = self.inner.schema_gen.load(Ordering::Acquire);
        let mut result = self.read_once(&mut conn, stmt, gen_now, run_as).await;
        // The rollback in read_once ended the aborted transaction.
        if matches!(&result, Err(RunError::Driver(e)) if error::is_stale_plan(e)) {
            if conn.clear_cached_statements().await.is_ok() {
                result = self.read_once(&mut conn, stmt, gen_now, run_as).await;
            }
        } else if matches!(&result, Err(RunError::Driver(e)) if error::is_lost_unnamed_statement(e)) {
            result = self.read_once(&mut conn, stmt, gen_now, run_as).await;
        }
        let rows = result.map_err(|e| self.run_error(e))?;
        self.inner
            .slow
            .record(stmt, started.elapsed(), rows.len() as u64, false);
        Ok(rows)
    }

    async fn read_once(
        &self,
        conn: &mut PoolConnection<Postgres>,
        stmt: &Statement,
        gen_now: u64,
        run_as: Option<&RunAs>,
    ) -> Result<Rows, RunError> {
        let seen: Option<String> =
            sqlx::raw_sql("BEGIN READ ONLY; SELECT current_setting('forgebase.schema_gen', true)")
                .fetch_optional(&mut **conn)
                .await?
                .and_then(|row: PgRow| row.try_get::<Option<String>, _>(0).ok().flatten());
        let behind = seen.as_deref() != Some(gen_now.to_string().as_str());
        let result = async {
            if behind {
                conn.clear_cached_statements().await?;
            }
            if let Some(r) = run_as {
                set_run_as(conn, r).await?;
            }
            fetch(self, conn, stmt).await
        }
        .await;
        // ROLLBACK also undoes any session change the statement made; the
        // generation is recorded after it, outside the transaction.
        let end = if behind && result.is_ok() {
            format!("ROLLBACK; SET forgebase.schema_gen = '{gen_now}'")
        } else {
            "ROLLBACK".to_owned()
        };
        if sqlx::raw_sql(AssertSqlSafe(end)).execute(&mut **conn).await.is_err() {
            conn.close_on_drop();
        }
        result
    }

    /// Applies pending system migrations ([`forgebase_migrations::postgres`])
    /// on the writer connection, then makes sure of the session user's
    /// `search_path`.
    ///
    /// The pre-migration snapshot (ADR-0028 §7.6: `pg_basebackup`, managed
    /// mode) needs the engine's tools, which this crate does not have: it is
    /// skipped with a warning and `backup` is `None`.
    pub(crate) async fn migrate(&self, backups_dir: Option<&Path>) -> Result<MigrateReport, DbError> {
        let migrator = PgMigrator::system();
        let mut conn = self.inner.writer.acquire().await.map_err(|e| self.classify(e))?;
        let status = migrator.status(&mut conn).await?;
        if let Some(problem) = status.problems.into_iter().next() {
            return Err(MigrationError::from(problem).into());
        }
        let applied = if status.pending.is_empty() {
            Vec::new()
        } else {
            if backups_dir.is_some() && status.current_version > 0 {
                tracing::warn!(
                    location = %self.inner.location,
                    managed = self.inner.managed,
                    "no pre-migration snapshot on PostgreSQL yet; take a backup before upgrading"
                );
            }
            self.bump_schema_generation();
            let applied = migrator.run(&mut conn).await;
            self.bump_schema_generation();
            applied?
        };
        if pg_migrations::ensure_search_path(&mut conn).await? {
            // Idle readers opened before the change still have the old
            // search_path: close them, new ones get the new default.
            while let Some(reader) = self.inner.reader.primary.try_acquire() {
                let _ = reader.close().await;
            }
        }
        Ok(MigrateReport { applied, backup: None })
    }

    /// Applied / pending migrations, read without changing anything.
    pub(crate) async fn migration_status(&self) -> Result<MigrationStatus, DbError> {
        let mut conn = self
            .inner
            .reader
            .primary
            .acquire()
            .await
            .map_err(|e| self.classify(e))?;
        Ok(PgMigrator::system().status(&mut conn).await?)
    }

    pub(crate) async fn schema_version(&self) -> Result<i64, DbError> {
        let exists = self
            .read(&Statement::raw(
                "SELECT to_regclass('_forgebase_migrations') IS NOT NULL",
            ))
            .await?;
        if exists.scalar().and_then(Value::as_i64) != Some(1) {
            return Ok(0);
        }
        let rows = self
            .read(&Statement::raw(
                "SELECT coalesce(max(version), 0)::bigint FROM _forgebase_migrations",
            ))
            .await?;
        Ok(rows.scalar().and_then(Value::as_i64).unwrap_or(0))
    }

    pub(crate) async fn ping(&self) -> Result<(), DbError> {
        if self.inner.writer.is_closed() {
            return Err(DbError::Closed);
        }
        let rows = self.read(&Statement::raw("SELECT 1")).await?;
        debug_assert_eq!(rows.scalar(), Some(&Value::Integer(1)));
        Ok(())
    }

    pub(crate) fn pool_stats(&self) -> PoolStats {
        let one = |p: &PgPool| {
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
            reader: one(&self.inner.reader.primary),
        }
    }

    pub(crate) async fn info(&self) -> Result<DatabaseInfo, DbError> {
        let rows = self
            .read(&Statement::raw(
                "SELECT pg_database_size(current_database())::bigint, current_setting('block_size')::bigint, \
                 current_setting('server_version'), \
                 (SELECT count(*) FROM pg_stat_activity WHERE datname = current_database())::bigint",
            ))
            .await?;
        let row = rows.rows.first().cloned().unwrap_or_default();
        let int = |i: usize| row.get(i).and_then(Value::as_i64).unwrap_or(0);
        let (size, block) = (int(0), int(1).max(1));
        // pg_ls_waldir needs pg_monitor (or superuser): 0 when not allowed.
        let wal = self
            .read(&Statement::raw(
                "SELECT coalesce(sum(size), 0)::bigint FROM pg_ls_waldir()",
            ))
            .await
            .ok()
            .and_then(|r| r.scalar().and_then(Value::as_i64))
            .unwrap_or(0);
        Ok(DatabaseInfo {
            backend: BackendKind::Postgres,
            path: Default::default(),
            size_bytes: u64::try_from(size).unwrap_or(0),
            wal_size_bytes: u64::try_from(wal).unwrap_or(0),
            journal_mode: "wal".into(),
            schema_version: self.schema_version().await?,
            page_size: block,
            page_count: size / block,
            freelist_count: 0,
            sqlite_version: String::new(),
            server_version: row.get(2).and_then(Value::as_str).unwrap_or_default().to_owned(),
            connections: u32::try_from(int(3)).ok(),
            features: self.extension_report().map(|r| r.features.clone()).unwrap_or_default(),
        })
    }

    /// `bt_index_check` on every valid B-tree index (amcheck), then the
    /// foreign-key check.
    pub(crate) async fn integrity_check(&self) -> Result<Vec<String>, DbError> {
        let installed = self
            .read(&Statement::raw(
                "SELECT count(*) FROM pg_extension WHERE extname = 'amcheck'",
            ))
            .await?;
        if installed.scalar().and_then(Value::as_i64) != Some(1) {
            if self.inner.read_only {
                return Err(DbError::Unsupported(
                    "the amcheck extension is not installed in this database".into(),
                ));
            }
            let mut tx = self.begin_tx(TxMode::Normal).await?;
            tx.execute(&Statement::raw("CREATE EXTENSION IF NOT EXISTS amcheck"))
                .await
                .map_err(|e| DbError::Unsupported(format!("the amcheck extension is not available: {e}")))?;
            Box::new(tx).commit().await?;
        }
        let indexes = self
            .read(&Statement::raw(
                "SELECT c.oid::regclass::text FROM pg_index i JOIN pg_class c ON c.oid = i.indexrelid \
                 JOIN pg_am am ON am.oid = c.relam JOIN pg_namespace n ON n.oid = c.relnamespace \
                 WHERE am.amname = 'btree' AND c.relpersistence <> 't' AND i.indisvalid AND i.indisready \
                 AND n.nspname NOT IN ('pg_catalog', 'information_schema') AND n.nspname NOT LIKE 'pg\\_toast%' \
                 ORDER BY 1",
            ))
            .await?;
        let mut problems = Vec::new();
        for row in &indexes.rows {
            let Some(name) = row.first().and_then(Value::as_str) else {
                continue;
            };
            let check = Statement::new("SELECT bt_index_check(?1::regclass)::text", vec![name.into()]).uncached();
            match self.read(&check).await {
                Ok(_) => {}
                Err(DbError::Corrupt { message, .. }) => problems.push(format!("index {name}: {message}")),
                // Dropped while we were looking.
                Err(DbError::Statement(_)) => {}
                Err(e) => return Err(e),
            }
        }
        let violations = self.foreign_key_violations().await?;
        if violations > 0 {
            problems.push(format!("{violations} row(s) violate foreign keys"));
        }
        Ok(problems)
    }

    pub(crate) async fn foreign_key_violations(&self) -> Result<usize, DbError> {
        let fks = self
            .read(&Statement::raw(
                "SELECT con.conrelid::regclass::text, con.confrelid::regclass::text, \
                 (SELECT json_agg(a.attname ORDER BY k.ord) FROM unnest(con.conkey) WITH ORDINALITY k(attnum, ord) \
                    JOIN pg_attribute a ON a.attrelid = con.conrelid AND a.attnum = k.attnum), \
                 (SELECT json_agg(a.attname ORDER BY k.ord) FROM unnest(con.confkey) WITH ORDINALITY k(attnum, ord) \
                    JOIN pg_attribute a ON a.attrelid = con.confrelid AND a.attnum = k.attnum) \
                 FROM pg_constraint con JOIN pg_namespace n ON n.oid = con.connamespace \
                 WHERE con.contype = 'f' AND n.nspname NOT IN ('pg_catalog', 'information_schema')",
            ))
            .await?;
        let mut total = 0usize;
        for row in &fks.rows {
            let text = |i: usize| row.get(i).and_then(Value::as_str).unwrap_or_default();
            let cols = |i: usize| serde_json::from_str::<Vec<String>>(text(i)).unwrap_or_default();
            let (child_cols, parent_cols) = (cols(2), cols(3));
            if child_cols.is_empty() || child_cols.len() != parent_cols.len() {
                continue;
            }
            let q = |c: &str| PG.quote_ident(c);
            let not_null: Vec<String> = child_cols.iter().map(|c| format!("c.{} IS NOT NULL", q(c))).collect();
            let join: Vec<String> = child_cols
                .iter()
                .zip(&parent_cols)
                .map(|(c, p)| format!("p.{} = c.{}", q(p), q(c)))
                .collect();
            let sql = format!(
                "SELECT count(*)::bigint FROM {} c WHERE {} AND NOT EXISTS (SELECT 1 FROM {} p WHERE {})",
                text(0),
                not_null.join(" AND "),
                text(1),
                join.join(" AND ")
            );
            let rows = self.read(&Statement::raw(sql)).await?;
            total += usize::try_from(rows.scalar().and_then(Value::as_i64).unwrap_or(0)).unwrap_or(0);
        }
        Ok(total)
    }

    /// `CHECKPOINT` on the bundled engine; nothing on an external server.
    pub(crate) async fn checkpoint(&self) -> Result<(), DbError> {
        if !self.inner.managed || self.inner.read_only {
            return Ok(());
        }
        let mut conn = self.inner.writer.acquire().await.map_err(|e| self.classify(e))?;
        sqlx::raw_sql("CHECKPOINT")
            .execute(&mut *conn)
            .await
            .map_err(|e| self.classify(e))?;
        Ok(())
    }

    pub(crate) async fn close(&self) {
        self.inner.reader.close().await;
        self.inner.writer.close().await;
    }

    pub(crate) async fn meta_get(&self, key: &str) -> Result<Option<String>, DbError> {
        let rows = self
            .read(&Statement::new(
                "SELECT value FROM _forgebase_meta WHERE key = ?1",
                vec![key.into()],
            ))
            .await?;
        Ok(rows.scalar().and_then(Value::as_str).map(str::to_owned))
    }

    async fn write_one(&self, stmt: Statement) -> Result<ExecResult, DbError> {
        let mut tx = Box::new(self.begin_tx(TxMode::Normal).await?);
        let done = tx.execute(&stmt).await?;
        tx.commit().await?;
        Ok(done)
    }

    pub(crate) async fn meta_set(&self, key: &str, value: &str) -> Result<(), DbError> {
        self.write_one(Statement::new(
            "INSERT INTO _forgebase_meta (key, value, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            vec![key.into(), value.into(), forgebase_core::now_rfc3339().into()],
        ))
        .await?;
        Ok(())
    }

    pub(crate) async fn meta_delete(&self, key: &str) -> Result<bool, DbError> {
        let done = self
            .write_one(Statement::new(
                "DELETE FROM _forgebase_meta WHERE key = ?1",
                vec![key.into()],
            ))
            .await?;
        Ok(done.rows_affected > 0)
    }

    pub(crate) async fn meta_all(&self) -> Result<Vec<(String, String)>, DbError> {
        let rows = self
            .read(&Statement::raw("SELECT key, value FROM _forgebase_meta ORDER BY key"))
            .await?;
        Ok(rows
            .rows
            .into_iter()
            .map(|r| {
                let s = |i: usize| r.get(i).and_then(Value::as_str).unwrap_or_default().to_owned();
                (s(0), s(1))
            })
            .collect())
    }

    pub(crate) async fn ensure_instance(&self) -> Result<InstanceInfo, DbError> {
        let mut tx = Box::new(self.begin_tx(TxMode::Normal).await?);
        let now = forgebase_core::now_rfc3339();
        for (key, value) in [
            ("instance_id", uuid::Uuid::new_v4().to_string()),
            ("initialized_at", now.clone()),
        ] {
            tx.execute(&Statement::new(
                "INSERT INTO _forgebase_meta (key, value, updated_at) VALUES (?1, ?2, ?3) ON CONFLICT (key) DO NOTHING",
                vec![key.into(), value.into(), now.clone().into()],
            ))
            .await?;
        }
        let rows = tx
            .fetch_all(&Statement::raw(
                "SELECT key, value FROM _forgebase_meta WHERE key IN ('instance_id', 'initialized_at')",
            ))
            .await?;
        tx.commit().await?;
        let get = |k: &str| {
            rows.rows
                .iter()
                .find(|r| r.first().and_then(Value::as_str) == Some(k))
                .and_then(|r| r.get(1).and_then(Value::as_str))
                .unwrap_or_default()
                .to_owned()
        };
        Ok(InstanceInfo {
            instance_id: get("instance_id"),
            initialized_at: get("initialized_at"),
        })
    }

    async fn begin_tx(&self, mode: TxMode) -> Result<PgTx, DbError> {
        let mut conn = self.inner.writer.acquire().await.map_err(|e| self.classify(e))?;
        let gen_now = self.inner.schema_gen.load(Ordering::Acquire);
        if self.inner.writer_gen.load(Ordering::Acquire) != gen_now {
            conn.clear_cached_statements().await.map_err(|e| self.classify(e))?;
            self.inner.writer_gen.store(gen_now, Ordering::Release);
        }
        let begin = match (self.inner.read_only, mode) {
            (true, _) => "BEGIN READ ONLY",
            (false, TxMode::Normal) => "BEGIN ISOLATION LEVEL READ COMMITTED",
            (false, TxMode::SchemaChange) => "BEGIN ISOLATION LEVEL READ COMMITTED; SET CONSTRAINTS ALL DEFERRED",
        };
        if let Err(e) = sqlx::raw_sql(begin).execute(&mut *conn).await {
            conn.close_on_drop();
            return Err(self.classify(e));
        }
        Ok(PgTx {
            conn: Some(conn),
            db: self.clone(),
            mode,
            ddl: false,
            failed: false,
            run_as: None,
        })
    }

    /// [`PgDatabase::begin_tx`] with every statement run as `run_as`.
    async fn begin_tx_as(&self, mode: TxMode, run_as: &RunAs) -> Result<PgTx, DbError> {
        run_as.check()?;
        let mut tx = self.begin_tx(mode).await?;
        let conn = tx.conn()?;
        if let Err(e) = set_run_as(conn, run_as).await {
            tx.failed = true;
            return Err(self.classify(e));
        }
        tx.run_as = Some(run_as.clone());
        Ok(tx)
    }
}

/// Switches the transaction to `run_as`: `SET LOCAL ROLE` and the claims,
/// in one round trip (`set_config(.., true)` is transaction-local). Both
/// values are bound parameters, never SQL text.
async fn set_run_as(conn: &mut sqlx::PgConnection, run_as: &RunAs) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT set_config('forgebase.claims', $1, true), set_config('role', $2, true)")
        .bind(run_as.claims.as_str())
        .bind(run_as.role.as_str())
        .execute(conn)
        .await
        .map(|_| ())
}

/// Back to the transaction's own identity after one statement ran as
/// another: its `run_as` (from [`Backend::begin_as`]) or the session user.
async fn restore_run_as(conn: &mut sqlx::PgConnection, tx_level: Option<&RunAs>) -> Result<(), sqlx::Error> {
    match tx_level {
        Some(r) => set_run_as(conn, r).await,
        None => sqlx::query("SELECT set_config('role', 'none', true)")
            .execute(conn)
            .await
            .map(|_| ()),
    }
}

/// The read targets: the primary's read pool, later replicas (ADR-0029),
/// and the router that picks one per statement.
#[derive(Debug)]
struct ReadPools {
    primary: PgPool,
    replicas: Vec<PgPool>,
    router: Arc<dyn ReadRouter>,
}

impl ReadPools {
    fn pick(&self, stmt: &Statement) -> &PgPool {
        let request = ReadRequest {
            statement: stmt,
            replicas: self.replicas.len(),
        };
        match self.router.route(&request) {
            ReadTarget::Replica(n) => self.replicas.get(n).unwrap_or(&self.primary),
            ReadTarget::Primary => &self.primary,
        }
    }

    async fn close(&self) {
        for pool in &self.replicas {
            pool.close().await;
        }
        self.primary.close().await;
    }
}

/// `COMMIT` and, in the same round trip, the WAL position after it (for
/// read-your-writes on replicas). A failure of the position query alone
/// (`42501` not allowed, `55000` a standby) leaves the commit standing.
async fn commit_with_lsn(db: &PgDatabase, conn: &mut sqlx::PgConnection) -> Result<(), DbError> {
    match sqlx::raw_sql("COMMIT; SELECT pg_current_wal_insert_lsn()::text")
        .fetch_optional(&mut *conn)
        .await
    {
        Ok(row) => {
            let lsn = row
                .and_then(|r: PgRow| r.try_get::<String, _>(0).ok())
                .and_then(|t| t.parse::<Lsn>().ok());
            if let Some(lsn) = lsn {
                db.inner.last_commit_lsn.fetch_max(lsn.0, Ordering::AcqRel);
            }
            Ok(())
        }
        Err(e) if matches!(error::sqlstate(&e).as_deref(), Some("42501" | "55000")) => Ok(()),
        Err(e) => Err(db.classify(e)),
    }
}

impl PgDatabase {
    /// WAL position after the latest commit through this handle.
    pub(crate) fn last_commit_lsn(&self) -> Option<Lsn> {
        match self.inner.last_commit_lsn.load(Ordering::Acquire) {
            0 => None,
            n => Some(Lsn(n)),
        }
    }

    /// The primary's current WAL insert position (`pg_current_wal_insert_lsn()`), at or
    /// after every commit that has returned.
    pub(crate) async fn current_wal_lsn(&self) -> Result<Lsn, DbError> {
        let mut conn = self
            .inner
            .reader
            .primary
            .acquire()
            .await
            .map_err(|e| self.classify(e))?;
        let text: String = sqlx::query_scalar("SELECT pg_current_wal_insert_lsn()::text")
            .fetch_one(&mut *conn)
            .await
            .map_err(|e| self.classify(e))?;
        text.parse().map_err(DbError::Statement)
    }
}

impl PgDatabase {
    /// `server_version_num` of the server (`180003`).
    pub(crate) fn server_version_num(&self) -> u32 {
        self.inner.server_version
    }

    /// Waits until this server (a standby) has replayed `lsn`, at most
    /// `timeout`, with `WAIT FOR LSN` (PostgreSQL 19 and newer; outside a
    /// transaction block, on a read connection). `Unsupported` on older
    /// engines and once the command failed to parse.
    pub(crate) async fn wait_for_lsn(&self, lsn: Lsn, timeout: Duration) -> Result<LsnWait, DbError> {
        if self.inner.server_version < WAIT_FOR_LSN_VERSION || self.inner.no_wait_for.load(Ordering::Acquire) {
            return Ok(LsnWait::Unsupported);
        }
        let mut conn = self
            .inner
            .reader
            .primary
            .acquire()
            .await
            .map_err(|e| self.classify(e))?;
        let ms = timeout.as_millis().max(1);
        // The LSN is formatted from an integer: no user text in the SQL.
        let sql = format!("WAIT FOR LSN '{lsn}' WITH (TIMEOUT '{ms}ms', NO_THROW)");
        match sqlx::raw_sql(AssertSqlSafe(sql)).fetch_optional(&mut *conn).await {
            Ok(row) => {
                let status = row.and_then(|r: PgRow| r.try_get::<String, _>(0).ok());
                Ok(match status.as_deref() {
                    None | Some("success") => LsnWait::Reached,
                    Some(_) => LsnWait::TimedOut,
                })
            }
            Err(e) if matches!(error::sqlstate(&e).as_deref(), Some("42601" | "42704" | "0A000")) => {
                self.inner.no_wait_for.store(true, Ordering::Release);
                Ok(LsnWait::Unsupported)
            }
            Err(e) => Err(self.classify(e)),
        }
    }
}

/// Connects to the server's `postgres` database and creates the target database.
async fn create_database(base: &PgConnectOptions, location: &str) -> Result<(), DbError> {
    let name = base.get_database().unwrap_or(base.get_username()).to_owned();
    let mut admin = sqlx::PgConnection::connect_with(&base.clone().database("postgres"))
        .await
        .map_err(|e| error::classify(e, location))?;
    let sql = format!("CREATE DATABASE {}", PG.quote_ident(&name));
    let created = sqlx::raw_sql(AssertSqlSafe(sql)).execute(&mut admin).await;
    let _ = admin.close().await;
    match created {
        Ok(_) => {
            tracing::info!(%location, "created PostgreSQL database");
            Ok(())
        }
        // Created by someone else in the meantime.
        Err(e) if error::sqlstate(&e).as_deref() == Some("42P04") => Ok(()),
        Err(e) => Err(error::classify(e, location)),
    }
}

/// Decodes rows; `columns` come from the prepared statement (so an empty
/// result still names its columns) or, without one, from the first row.
fn decode_rows(rows: &[PgRow], columns: Option<Vec<String>>) -> Result<Rows, DbError> {
    let columns = columns.unwrap_or_else(|| {
        rows.first()
            .map(|r| r.columns().iter().map(|c| c.name().to_owned()).collect())
            .unwrap_or_default()
    });
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let mut values = Vec::with_capacity(row.len());
        for i in 0..row.len() {
            let raw = row.try_get_raw(i).map_err(|e| DbError::Statement(e.to_string()))?;
            values.push(types::decode(raw)?);
        }
        out.push(values);
    }
    Ok(Rows { columns, rows: out })
}

/// A statement ready to run: translated SQL, and its arguments bound by the
/// parameter types the server reported.
struct Prepared {
    sql: String,
    args: Option<PgArguments>,
    columns: Option<Vec<String>>,
}

/// The type a parameter the server cannot infer (`SELECT ?1`) is declared
/// with, from the kind of the value bound this time (as SQLite types it).
fn kind_oid(v: &Value) -> u32 {
    match v {
        Value::Integer(_) => types::oid::INT8,
        Value::Real(_) => types::oid::FLOAT8,
        Value::Blob(_) => types::oid::BYTEA,
        Value::Null | Value::Text(_) => types::oid::TEXT,
    }
}

/// `$N` from "could not determine data type of parameter $N" (42P18).
fn indeterminate_param(e: &sqlx::Error) -> Option<usize> {
    let sqlx::Error::Database(db) = e else {
        return None;
    };
    if db.code().as_deref() != Some("42P18") {
        return None;
    }
    let msg = db.message();
    let tail = msg.strip_prefix("could not determine data type of parameter $")?;
    tail.parse().ok()
}

async fn prepare(db: &PgDatabase, conn: &mut sqlx::PgConnection, stmt: &Statement) -> Result<Prepared, RunError> {
    let tr = db.translate(&stmt.sql)?;
    let n = tr.max_param.max(stmt.params.len());
    if n == 0 {
        return Ok(Prepared {
            sql: tr.sql.clone(),
            args: None,
            columns: None,
        });
    }
    // Parameters the server cannot infer, learned on first sight of the SQL.
    let known = db
        .inner
        .indeterminate
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&tr.sql)
        .cloned();
    let first_sight = known.is_none();
    let mut indeterminate: Vec<usize> = known.map(|k| k.to_vec()).unwrap_or_default();
    let prepared = loop {
        // Unused numbers (SQLite allows gaps) are declared text; indeterminate
        // ones get the bound value's type, and the SQL a comment naming those
        // types so each combination is its own cached statement.
        let mut sql = tr.sql.clone();
        let mut tag = String::new();
        let declared: Vec<PgTypeInfo> = (1..=n)
            .map(|i| {
                let value = stmt.params.get(i - 1).unwrap_or(&Value::Null);
                let oid = if indeterminate.contains(&i) {
                    let o = kind_oid(value);
                    tag.push_str(&format!(" {i}:{o}"));
                    o
                } else if tr.used.contains(&i) {
                    0
                } else {
                    types::oid::TEXT
                };
                PgTypeInfo::with_oid(Oid(oid))
            })
            .collect();
        if !tag.is_empty() {
            sql.push_str(&format!("\n/* forgebase types:{tag} */"));
        }
        // A failed prepare aborts the transaction, so the first attempt for
        // an SQL text runs under a savepoint.
        if first_sight {
            sqlx::raw_sql("SAVEPOINT forgebase_prepare").execute(&mut *conn).await?;
        }
        match conn
            .prepare_with(AssertSqlSafe(sql.clone()).into_sql_str(), &declared)
            .await
        {
            Ok(p) => {
                if first_sight {
                    sqlx::raw_sql("RELEASE SAVEPOINT forgebase_prepare")
                        .execute(&mut *conn)
                        .await?;
                    let mut cache = db.inner.indeterminate.lock().unwrap_or_else(|p| p.into_inner());
                    if cache.len() >= SQL_CACHE {
                        cache.clear();
                    }
                    cache.insert(tr.sql.clone(), indeterminate.clone().into());
                }
                break (p, sql);
            }
            Err(e) => {
                if first_sight {
                    sqlx::raw_sql("ROLLBACK TO SAVEPOINT forgebase_prepare; RELEASE SAVEPOINT forgebase_prepare")
                        .execute(&mut *conn)
                        .await?;
                }
                match indeterminate_param(&e) {
                    Some(i) if first_sight && i <= n && !indeterminate.contains(&i) => indeterminate.push(i),
                    _ => return Err(e.into()),
                }
            }
        }
    };
    let (prepared, sql) = prepared;
    let param_types: Vec<PgTypeInfo> = match prepared.parameters() {
        Some(sqlx::Either::Left(types)) => types.to_vec(),
        _ => return Err(DbError::Statement("the server did not describe the parameters".into()).into()),
    };
    let mut args = PgArguments::default();
    for (i, ty) in param_types.iter().enumerate() {
        let value = stmt.params.get(i).unwrap_or(&Value::Null);
        let param = PgParam::new(value, ty, i + 1)?;
        Arguments::add(&mut args, param).map_err(|e| DbError::Statement(format!("parameter ${}: {e}", i + 1)))?;
    }
    let columns = Some(prepared.columns().iter().map(|c| c.name().to_owned()).collect());
    Ok(Prepared {
        sql,
        args: Some(args),
        columns,
    })
}

async fn fetch(db: &PgDatabase, conn: &mut sqlx::PgConnection, stmt: &Statement) -> Result<Rows, RunError> {
    let p = prepare(db, conn, stmt).await?;
    let rows = match p.args {
        Some(args) => {
            sqlx::query_with(AssertSqlSafe(p.sql), args)
                .persistent(stmt.cache)
                .fetch_all(&mut *conn)
                .await?
        }
        None => {
            sqlx::query(AssertSqlSafe(p.sql))
                .persistent(stmt.cache)
                .fetch_all(&mut *conn)
                .await?
        }
    };
    Ok(decode_rows(&rows, p.columns)?)
}

/// Whether [`fetch`] runs `stmt` as the unnamed prepared statement: uncached
/// and without parameters (with parameters, [`prepare`] has already prepared
/// and cached a named one, which sqlx then reuses).
fn runs_unnamed(db: &PgDatabase, stmt: &Statement) -> Result<bool, DbError> {
    Ok(!stmt.cache && stmt.params.is_empty() && db.translate(&stmt.sql)?.max_param == 0)
}

/// [`fetch`] of an unnamed statement inside a write transaction. sqlx can
/// lose the unnamed statement on a connection's first sight of a result type
/// ([`error::is_lost_unnamed_statement`]); that error would abort the
/// transaction, so the statement runs under a savepoint and once more after
/// rolling back to it (the type is cached by then). Reads retry the same way
/// in [`PgDatabase::read`], without a savepoint (each read is its own
/// transaction).
async fn fetch_unnamed_in_tx(
    db: &PgDatabase,
    conn: &mut sqlx::PgConnection,
    stmt: &Statement,
) -> Result<Rows, RunError> {
    sqlx::raw_sql("SAVEPOINT forgebase_unnamed").execute(&mut *conn).await?;
    let mut result = fetch(db, conn, stmt).await;
    if matches!(&result, Err(RunError::Driver(e)) if error::is_lost_unnamed_statement(e)) {
        sqlx::raw_sql("ROLLBACK TO SAVEPOINT forgebase_unnamed")
            .execute(&mut *conn)
            .await?;
        result = fetch(db, conn, stmt).await;
    }
    let rows = result?;
    sqlx::raw_sql("RELEASE SAVEPOINT forgebase_unnamed")
        .execute(&mut *conn)
        .await?;
    Ok(rows)
}

async fn execute(db: &PgDatabase, conn: &mut sqlx::PgConnection, stmt: &Statement) -> Result<u64, RunError> {
    if stmt.params.is_empty() && !stmt.cache {
        // One-off statements without parameters (DDL, scripts) use the simple
        // protocol, which, like SQLite, accepts several statements.
        let tr = db.translate(&stmt.sql)?;
        if tr.max_param == 0 {
            let done = sqlx::raw_sql(AssertSqlSafe(stmt.sql.clone()))
                .execute(&mut *conn)
                .await?;
            return Ok(done.rows_affected());
        }
    }
    let p = prepare(db, conn, stmt).await?;
    let done = match p.args {
        Some(args) => {
            sqlx::query_with(AssertSqlSafe(p.sql), args)
                .persistent(stmt.cache)
                .execute(&mut *conn)
                .await?
        }
        None => {
            sqlx::query(AssertSqlSafe(p.sql))
                .persistent(stmt.cache)
                .execute(&mut *conn)
                .await?
        }
    };
    Ok(done.rows_affected())
}

/// First keyword of a statement, upper-cased (comments, whitespace and
/// opening parentheses skipped).
fn leading_keyword(sql: &str) -> String {
    let mut rest = sql;
    loop {
        rest = rest.trim_start_matches(|c: char| c.is_whitespace() || c == '(');
        if let Some(r) = rest.strip_prefix("--") {
            rest = r.split_once('\n').map_or("", |(_, tail)| tail);
        } else if let Some(r) = rest.strip_prefix("/*") {
            rest = r.split_once("*/").map_or("", |(_, tail)| tail);
        } else {
            break;
        }
    }
    rest.chars()
        .take_while(char::is_ascii_alphabetic)
        .collect::<String>()
        .to_ascii_uppercase()
}

/// Flattens an `EXPLAIN (FORMAT JSON)` plan into steps, depth first.
fn plan_steps(plan: &serde_json::Value, parent: i64, out: &mut Vec<PlanStep>) {
    let id = i64::try_from(out.len()).unwrap_or(i64::MAX) + 1;
    let field = |k: &str| plan.get(k).and_then(serde_json::Value::as_str);
    let mut detail = field("Node Type").unwrap_or("?").to_owned();
    if let Some(op) = field("Operation") {
        detail = format!("{detail} ({op})");
    }
    if let Some(index) = field("Index Name") {
        detail.push_str(" using ");
        detail.push_str(index);
    }
    if let Some(rel) = field("Relation Name") {
        detail.push_str(" on ");
        detail.push_str(rel);
        if let Some(alias) = field("Alias").filter(|a| *a != rel) {
            detail.push(' ');
            detail.push_str(alias);
        }
    }
    if let Some(cond) = field("Index Cond").or_else(|| field("Filter")) {
        detail.push_str(&format!(" ({cond})"));
    }
    out.push(PlanStep { id, parent, detail });
    if let Some(children) = plan.get("Plans").and_then(serde_json::Value::as_array) {
        for child in children {
            plan_steps(child, id, out);
        }
    }
}

fn plan_writes(plan: &serde_json::Value) -> bool {
    let node = plan
        .get("Node Type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    matches!(node, "ModifyTable" | "LockRows")
        || plan
            .get("Plans")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|c| c.iter().any(plan_writes))
}

impl PgDatabase {
    async fn explain_json(&self, stmt: &Statement) -> Result<serde_json::Value, DbError> {
        let mut explain = stmt.clone();
        explain.sql = format!("{}{}", PG.explain_prefix(), stmt.sql);
        explain.cache = false;
        explain.label = None;
        let rows = self.read(&explain).await?;
        let text = rows.scalar().and_then(Value::as_str).unwrap_or("[]");
        let plan: serde_json::Value =
            serde_json::from_str(text).map_err(|e| DbError::Statement(format!("unreadable query plan: {e}")))?;
        Ok(plan
            .get(0)
            .and_then(|p| p.get("Plan"))
            .cloned()
            .unwrap_or(serde_json::Value::Null))
    }
}

impl Backend for PgDatabase {
    fn dialect(&self) -> &dyn Dialect {
        &PG
    }

    fn fetch_all<'a>(&'a self, stmt: &'a Statement) -> BoxFuture<'a, Result<Rows, DbError>> {
        Box::pin(self.read(stmt))
    }

    fn begin_as<'a>(&'a self, mode: TxMode, run_as: &'a RunAs) -> BoxFuture<'a, Result<Box<dyn WriteTx>, DbError>> {
        Box::pin(async move { Ok(Box::new(self.begin_tx_as(mode, run_as).await?) as Box<dyn WriteTx>) })
    }

    fn fetch_all_as<'a>(&'a self, stmt: &'a Statement, run_as: &'a RunAs) -> BoxFuture<'a, Result<Rows, DbError>> {
        Box::pin(self.read_with(stmt, Some(run_as)))
    }

    fn begin(&self, mode: TxMode) -> BoxFuture<'_, Result<Box<dyn WriteTx>, DbError>> {
        Box::pin(async move { Ok(Box::new(self.begin_tx(mode).await?) as Box<dyn WriteTx>) })
    }

    fn explain<'a>(&'a self, stmt: &'a Statement) -> BoxFuture<'a, Result<Vec<PlanStep>, DbError>> {
        Box::pin(async move {
            let plan = self.explain_json(stmt).await?;
            let mut steps = Vec::new();
            if !plan.is_null() {
                plan_steps(&plan, 0, &mut steps);
            }
            Ok(steps)
        })
    }

    fn statement_writes<'a>(&'a self, stmt: &'a Statement) -> BoxFuture<'a, Result<bool, DbError>> {
        Box::pin(async move {
            match leading_keyword(&stmt.sql).as_str() {
                "SELECT" | "WITH" | "VALUES" | "TABLE" => Ok(plan_writes(&self.explain_json(stmt).await?)),
                "SHOW" => Ok(false),
                "EXPLAIN" => Ok(stmt.sql.to_ascii_uppercase().contains("ANALYZE")),
                // DDL, DML and utility statements write (or change the session).
                _ => Ok(true),
            }
        })
    }

    fn slow_queries(&self) -> Vec<SlowQuery> {
        self.inner.slow.entries()
    }

    fn clear_slow_queries(&self) {
        self.inner.slow.clear();
    }

    fn slow_query_threshold(&self) -> Duration {
        self.inner.slow.threshold()
    }

    fn set_slow_query_threshold(&self, threshold: Duration) {
        self.inner.slow.set_threshold(threshold);
    }
}

/// A write transaction on the single writer connection.
struct PgTx {
    /// `None` once committed or rolled back.
    conn: Option<PoolConnection<Postgres>>,
    db: PgDatabase,
    mode: TxMode,
    /// A statement changed the schema: readers must drop cached statements
    /// after the commit.
    ddl: bool,
    /// A statement failed on the server, which aborts a PostgreSQL
    /// transaction; `commit` then rolls back and reports it.
    failed: bool,
    /// Identity of the whole transaction ([`Backend::begin_as`]).
    run_as: Option<RunAs>,
}

impl PgTx {
    fn conn(&mut self) -> Result<&mut PoolConnection<Postgres>, DbError> {
        self.conn.as_mut().ok_or(DbError::Closed)
    }

    fn note<T>(&mut self, r: Result<T, RunError>) -> Result<T, DbError> {
        r.map_err(|e| {
            if matches!(e, RunError::Driver(_)) {
                self.failed = true;
            }
            self.db.run_error(e)
        })
    }

    async fn finish(mut self, commit: bool) -> Result<(), DbError> {
        let Some(mut conn) = self.conn.take() else {
            return Err(DbError::Closed);
        };
        let db = self.db.clone();
        if commit && self.failed {
            if sqlx::raw_sql("ROLLBACK").execute(&mut *conn).await.is_err() {
                conn.close_on_drop();
            }
            return Err(DbError::Statement(
                "the transaction was rolled back because an earlier statement in it failed".into(),
            ));
        }
        let refresh = commit && (self.mode == TxMode::SchemaChange || self.ddl);
        if refresh {
            db.bump_schema_generation();
        }
        let result = if commit {
            commit_with_lsn(&db, &mut conn).await
        } else {
            sqlx::raw_sql("ROLLBACK")
                .execute(&mut *conn)
                .await
                .map(|_| ())
                .map_err(|e| db.classify(e))
        };
        if refresh {
            db.bump_schema_generation();
            // The writer cleared its own cache after the DDL statement.
            db.inner
                .writer_gen
                .store(db.inner.schema_gen.load(Ordering::Acquire), Ordering::Release);
        }
        if result.is_err() {
            // A failed COMMIT ends the transaction on the server; make sure.
            if sqlx::raw_sql("ROLLBACK").execute(&mut *conn).await.is_err() {
                conn.close_on_drop();
            }
        }
        result
    }
}

impl Drop for PgTx {
    fn drop(&mut self) {
        let Some(mut conn) = self.conn.take() else {
            return;
        };
        // Dropped mid-transaction (error path or cancelled future): roll back
        // before the writer goes back to the pool, or discard it.
        match tokio::runtime::Handle::try_current() {
            Ok(rt) => {
                rt.spawn(async move {
                    if sqlx::raw_sql("ROLLBACK").execute(&mut *conn).await.is_err() {
                        conn.close_on_drop();
                    }
                });
            }
            Err(_) => conn.close_on_drop(),
        }
    }
}

impl PgTx {
    /// Switches role and claims to `run_as` for the next statement.
    async fn switch_to(&mut self, run_as: &RunAs) -> Result<(), DbError> {
        run_as.check()?;
        let db = self.db.clone();
        let r = set_run_as(self.conn()?, run_as).await;
        r.map_err(|e| {
            self.failed = true;
            db.classify(e)
        })
    }

    /// Back to the transaction's own identity (skipped once a statement
    /// failed: the transaction is aborted and only rolls back).
    async fn switch_back(&mut self) -> Result<(), DbError> {
        if self.failed {
            return Ok(());
        }
        let db = self.db.clone();
        let tx_level = self.run_as.clone();
        let r = restore_run_as(self.conn()?, tx_level.as_ref()).await;
        r.map_err(|e| {
            self.failed = true;
            db.classify(e)
        })
    }
}

impl WriteTx for PgTx {
    fn execute_as<'a>(
        &'a mut self,
        stmt: &'a Statement,
        run_as: &'a RunAs,
    ) -> BoxFuture<'a, Result<ExecResult, DbError>> {
        Box::pin(async move {
            self.switch_to(run_as).await?;
            let out = self.execute(stmt).await;
            self.switch_back().await?;
            out
        })
    }

    fn fetch_all_as<'a>(&'a mut self, stmt: &'a Statement, run_as: &'a RunAs) -> BoxFuture<'a, Result<Rows, DbError>> {
        Box::pin(async move {
            self.switch_to(run_as).await?;
            let out = self.fetch_all(stmt).await;
            self.switch_back().await?;
            out
        })
    }

    fn execute<'a>(&'a mut self, stmt: &'a Statement) -> BoxFuture<'a, Result<ExecResult, DbError>> {
        Box::pin(async move {
            let started = Instant::now();
            let db = self.db.clone();
            let ddl = changes_schema(&stmt.sql);
            let conn = self.conn()?;
            let r = execute(&db, conn, stmt).await;
            let rows = self.note(r)?;
            if ddl {
                self.ddl = true;
                let conn = self.conn()?;
                let cleared = conn.clear_cached_statements().await;
                cleared.map_err(|e| db.classify(e))?;
            }
            db.inner.slow.record(stmt, started.elapsed(), rows, true);
            Ok(ExecResult {
                rows_affected: rows,
                // PostgreSQL has no rowid: use `Dialect::returning_id`.
                last_insert_id: 0,
            })
        })
    }

    fn fetch_all<'a>(&'a mut self, stmt: &'a Statement) -> BoxFuture<'a, Result<Rows, DbError>> {
        Box::pin(async move {
            let started = Instant::now();
            let db = self.db.clone();
            let unnamed = runs_unnamed(&db, stmt)?;
            let conn = self.conn()?;
            let r = if unnamed {
                fetch_unnamed_in_tx(&db, conn, stmt).await
            } else {
                fetch(&db, conn, stmt).await
            };
            let rows = self.note(r)?;
            db.inner.slow.record(stmt, started.elapsed(), rows.len() as u64, true);
            Ok(rows)
        })
    }

    fn commit(self: Box<Self>) -> BoxFuture<'static, Result<(), DbError>> {
        Box::pin((*self).finish(true))
    }

    fn rollback(self: Box<Self>) -> BoxFuture<'static, Result<(), DbError>> {
        Box::pin((*self).finish(false))
    }

    fn prepare(self: Box<Self>, gid: String) -> BoxFuture<'static, Result<(), DbError>> {
        Box::pin((*self).finish_prepare(gid))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leading_keywords() {
        assert_eq!(leading_keyword("  -- c\n/* x */ (select 1)"), "SELECT");
        assert_eq!(
            leading_keyword("with x as (delete from t returning *) select * from x"),
            "WITH"
        );
        assert_eq!(leading_keyword("CREATE TABLE t (a int)"), "CREATE");
        assert_eq!(leading_keyword(""), "");
    }

    #[test]
    fn plan_flattening_and_write_detection() {
        let plan: serde_json::Value = serde_json::from_str(
            r#"{"Node Type":"Nested Loop","Plans":[
                {"Node Type":"Index Scan","Index Name":"idx_a","Relation Name":"t","Alias":"x","Index Cond":"(a = 1)"},
                {"Node Type":"Seq Scan","Relation Name":"u","Alias":"u"}]}"#,
        )
        .unwrap();
        let mut steps = Vec::new();
        plan_steps(&plan, 0, &mut steps);
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[1].detail, "Index Scan using idx_a on t x ((a = 1))");
        assert_eq!((steps[1].parent, steps[2].parent), (1, 1));
        assert!(!plan_writes(&plan));
        let cte: serde_json::Value = serde_json::from_str(
            r#"{"Node Type":"CTE Scan","Plans":[{"Node Type":"ModifyTable","Operation":"Delete"}]}"#,
        )
        .unwrap();
        assert!(plan_writes(&cte));
    }

    #[test]
    fn locations_never_show_the_password() {
        let o = base_options(&SecretString::new("postgres://fb:hunter2@db.example:6543/app")).unwrap();
        assert_eq!(describe_target(&o), "postgres://fb@db.example:6543/app");
        let s = base_options(&SecretString::new("postgres://fb:pw@localhost/app?host=/tmp/sock")).unwrap();
        assert!(!describe_target(&s).contains("pw"));
    }
}
