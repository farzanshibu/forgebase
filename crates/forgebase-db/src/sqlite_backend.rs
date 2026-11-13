//! [`Backend`] implementation for the embedded SQLite database.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use sqlx::pool::PoolConnection;
use sqlx::sqlite::{SqliteArguments, SqliteRow};
use sqlx::{AssertSqlSafe, Column, Row, Sqlite, TypeInfo, ValueRef};

use crate::DbError;
use crate::backend::{Backend, BoxFuture, ExecResult, PlanStep, Rows, SlowQuery, Statement, TxMode, Value, WriteTx};
use crate::dialect::{Dialect, SqliteDialect};
use crate::sqlite::SqliteDatabase;

static SQLITE: SqliteDialect = SqliteDialect;

/// Per-connection record of the schema generation last seen, so a reader that
/// is behind a schema change drops its cached statements and reloads the schema.
///
/// Why both: sqlx keeps the column list of a cached statement (stale after
/// `ALTER TABLE ... ADD COLUMN`, then `row.rs` panics with index out of bounds),
/// and SQLite itself prepares against the connection's in-memory schema, which
/// only reloads when a transaction starts. Both are fixed by clearing sqlx's
/// cache and then touching `sqlite_schema` before the connection is handed out.
#[derive(Debug, Default)]
pub(crate) struct SeenGenerations(Mutex<std::collections::HashMap<usize, u64>>);

impl SeenGenerations {
    /// Pool `before_acquire` hook body. Cost when nothing changed: one atomic
    /// load and a short mutex-guarded map lookup, no database round trip.
    pub(crate) async fn refresh_if_behind(
        &self,
        conn: &mut sqlx::SqliteConnection,
        current: u64,
    ) -> Result<(), sqlx::Error> {
        use sqlx::Connection;
        let key = conn.lock_handle().await?.as_raw_handle().as_ptr() as usize;
        let seen = self.0.lock().unwrap_or_else(|p| p.into_inner()).get(&key).copied();
        if seen == Some(current) {
            return Ok(());
        }
        conn.clear_cached_statements().await?;
        // Starts a read transaction, which makes SQLite reload a changed schema.
        sqlx::query("SELECT 1 FROM sqlite_schema LIMIT 1")
            .persistent(false)
            .execute(&mut *conn)
            .await?;
        self.0.lock().unwrap_or_else(|p| p.into_inner()).insert(key, current);
        Ok(())
    }
}

fn bind(stmt: &Statement) -> sqlx::query::Query<'_, Sqlite, SqliteArguments> {
    let mut q = sqlx::query(AssertSqlSafe(stmt.sql.clone())).persistent(stmt.cache);
    for p in &stmt.params {
        q = match p {
            Value::Null => q.bind(Option::<i64>::None),
            Value::Integer(i) => q.bind(*i),
            Value::Real(f) => q.bind(*f),
            Value::Text(s) => q.bind(s.clone()),
            Value::Blob(b) => q.bind(b.clone()),
        };
    }
    q
}

fn decode_rows(rows: Vec<SqliteRow>) -> Result<Rows, sqlx::Error> {
    let columns = rows
        .first()
        .map(|r| r.columns().iter().map(|c| c.name().to_owned()).collect())
        .unwrap_or_default();
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let mut values = Vec::with_capacity(row.len());
        for i in 0..row.len() {
            let raw = row.try_get_raw(i)?;
            let value = if raw.is_null() {
                Value::Null
            } else {
                match raw.type_info().name() {
                    "INTEGER" | "BOOLEAN" => Value::Integer(row.try_get_unchecked::<i64, _>(i)?),
                    "REAL" | "NUMERIC" => Value::Real(row.try_get_unchecked::<f64, _>(i)?),
                    "BLOB" => Value::Blob(row.try_get_unchecked::<Vec<u8>, _>(i)?),
                    _ => Value::Text(row.try_get_unchecked::<String, _>(i)?),
                }
            };
            values.push(value);
        }
        out.push(values);
    }
    Ok(Rows { columns, rows: out })
}

/// Opcodes of SQLite's bytecode that only appear in programs that write.
fn program_writes(rows: &Rows) -> bool {
    let (Some(op), Some(p2)) = (rows.column("opcode"), rows.column("p2")) else {
        return true; // unknown shape: be conservative
    };
    rows.rows.iter().any(|r| {
        let opcode = r[op].as_str().unwrap_or_default();
        match opcode {
            // Transaction P2 != 0 starts a write transaction.
            "Transaction" => r[p2].as_i64().unwrap_or(1) != 0,
            // (Insert/IdxInsert also write ephemeral tables in plain SELECTs, so
            // they are not listed; real-table writes always open a write cursor.)
            "OpenWrite" | "SetCookie" | "CreateBtree" | "Destroy" | "Clear" | "ParseSchema" | "DropTable"
            | "DropIndex" | "DropTrigger" | "VUpdate" | "VCreate" | "VDestroy" | "Vacuum" | "JournalMode" => true,
            _ => false,
        }
    })
}

impl Backend for SqliteDatabase {
    fn dialect(&self) -> &dyn Dialect {
        &SQLITE
    }

    fn fetch_all<'a>(&'a self, stmt: &'a Statement) -> BoxFuture<'a, Result<Rows, DbError>> {
        Box::pin(async move {
            crate::backend::refuse_nul(stmt)?;
            let started = Instant::now();
            let rows = bind(stmt)
                .fetch_all(&self.inner.reader)
                .await
                .map_err(|e| self.classify(e))?;
            let rows = decode_rows(rows).map_err(|e| self.classify(e))?;
            self.inner
                .slow
                .record(stmt, started.elapsed(), rows.len() as u64, false);
            Ok(rows)
        })
    }

    fn begin(&self, mode: TxMode) -> BoxFuture<'_, Result<Box<dyn WriteTx>, DbError>> {
        Box::pin(async move {
            let mut conn = self.inner.writer.acquire().await.map_err(|e| self.classify(e))?;
            if mode == TxMode::SchemaChange {
                // Must happen outside a transaction: inside one it is a no-op.
                sqlx::query("PRAGMA foreign_keys = OFF")
                    .execute(&mut *conn)
                    .await
                    .map_err(|e| self.classify(e))?;
            }
            if let Err(e) = sqlx::query("BEGIN IMMEDIATE").execute(&mut *conn).await {
                if mode == TxMode::SchemaChange {
                    conn.close_on_drop();
                }
                return Err(self.classify(e));
            }
            Ok(Box::new(SqliteTx {
                conn: Some(conn),
                mode,
                db: self.clone(),
                ddl: false,
            }) as Box<dyn WriteTx>)
        })
    }

    fn explain<'a>(&'a self, stmt: &'a Statement) -> BoxFuture<'a, Result<Vec<PlanStep>, DbError>> {
        Box::pin(async move {
            let mut explain = stmt.clone();
            explain.sql = format!("{}{}", SQLITE.explain_prefix(), stmt.sql);
            explain.cache = false;
            explain.label = None;
            let rows = bind(&explain)
                .fetch_all(&self.inner.reader)
                .await
                .map_err(|e| self.classify(e))?;
            let rows = decode_rows(rows).map_err(|e| self.classify(e))?;
            let (id, parent, detail) = (
                rows.column("id").unwrap_or(0),
                rows.column("parent").unwrap_or(1),
                rows.column("detail").unwrap_or(3),
            );
            Ok(rows
                .rows
                .iter()
                .map(|r| PlanStep {
                    id: r.get(id).and_then(Value::as_i64).unwrap_or_default(),
                    parent: r.get(parent).and_then(Value::as_i64).unwrap_or_default(),
                    detail: r.get(detail).and_then(Value::as_str).unwrap_or_default().to_owned(),
                })
                .collect())
        })
    }

    fn statement_writes<'a>(&'a self, stmt: &'a Statement) -> BoxFuture<'a, Result<bool, DbError>> {
        Box::pin(async move {
            let mut explain = stmt.clone();
            explain.sql = format!("EXPLAIN {}", stmt.sql);
            explain.cache = false;
            let rows = bind(&explain)
                .fetch_all(&self.inner.reader)
                .await
                .map_err(|e| self.classify(e))?;
            let rows = decode_rows(rows).map_err(|e| self.classify(e))?;
            Ok(program_writes(&rows))
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
struct SqliteTx {
    /// `None` once committed or rolled back.
    conn: Option<PoolConnection<Sqlite>>,
    mode: TxMode,
    db: SqliteDatabase,
    /// A statement changed the schema (DDL in a `Normal` transaction, such as
    /// `CREATE INDEX`): readers must reload it after the commit (FB-85).
    ddl: bool,
}

/// Whether `sql` changes the schema or the planner's statistics, so pooled
/// readers must reload their in-memory schema once it commits. Leading
/// whitespace and SQL comments are skipped; the first keyword decides.
pub(crate) fn changes_schema(sql: &str) -> bool {
    let mut rest = sql;
    loop {
        rest = rest.trim_start();
        if let Some(r) = rest.strip_prefix("--") {
            rest = r.split_once('\n').map_or("", |(_, tail)| tail);
        } else if let Some(r) = rest.strip_prefix("/*") {
            rest = r.split_once("*/").map_or("", |(_, tail)| tail);
        } else {
            break;
        }
    }
    let word: String = rest
        .chars()
        .take_while(char::is_ascii_alphabetic)
        .collect::<String>()
        .to_ascii_uppercase();
    matches!(word.as_str(), "CREATE" | "DROP" | "ALTER" | "ANALYZE" | "REINDEX")
}

impl SqliteTx {
    fn conn(&mut self) -> Result<&mut PoolConnection<Sqlite>, DbError> {
        self.conn.as_mut().ok_or(DbError::Closed)
    }

    /// Ends the transaction with `sql` and restores connection state.
    async fn finish(mut self: Box<Self>, sql: &'static str) -> Result<(), DbError> {
        let Some(mut conn) = self.conn.take() else {
            return Err(DbError::Closed);
        };
        let db = self.db.clone();
        let result = async {
            if sql == "COMMIT" && self.mode == TxMode::SchemaChange {
                let violations = sqlx::query("PRAGMA foreign_key_check")
                    .fetch_all(&mut *conn)
                    .await
                    .map_err(|e| db.classify(e))?;
                if let Some(first) = violations.first() {
                    let table: String = first.try_get(0).unwrap_or_default();
                    let parent: String = first.try_get(2).unwrap_or_default();
                    return Err(DbError::Constraint {
                        kind: crate::ConstraintKind::ForeignKey,
                        message: format!(
                            "{} row(s) of {table} would reference missing rows in {parent}",
                            violations.len()
                        ),
                    });
                }
            }
            let refresh = sql == "COMMIT" && (self.mode == TxMode::SchemaChange || self.ddl);
            if refresh {
                // Before and after: a reader acquired between the two is
                // refreshed again once the commit is visible.
                db.bump_schema_generation();
            }
            sqlx::query(sql).execute(&mut *conn).await.map_err(|e| db.classify(e))?;
            if refresh {
                db.bump_schema_generation();
            }
            if self.mode == TxMode::SchemaChange || self.ddl {
                // The writer's own statements prepared before the DDL (a
                // `SELECT *` of a rebuilt table) would decode the old
                // columns: readers are refreshed by their pool hook, the
                // writer here.
                use sqlx::Connection as _;
                conn.clear_cached_statements().await.map_err(|e| db.classify(e))?;
            }
            Ok(())
        }
        .await;
        if result.is_err() {
            // Leave nothing half-done on the writer: rollback, or discard the
            // connection (closing rolls back) if even that fails.
            if sqlx::query("ROLLBACK").execute(&mut *conn).await.is_err() {
                conn.close_on_drop();
            }
        }
        if self.mode == TxMode::SchemaChange
            && sqlx::query("PRAGMA foreign_keys = ON")
                .execute(&mut *conn)
                .await
                .is_err()
        {
            conn.close_on_drop();
        }
        result
    }
}

impl Drop for SqliteTx {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.as_mut() {
            // Dropped mid-transaction (error path or cancelled future): discard
            // the connection so the open transaction is rolled back by SQLite
            // and the pool opens a fresh writer with default pragmas.
            conn.close_on_drop();
        }
    }
}

impl WriteTx for SqliteTx {
    fn execute<'a>(&'a mut self, stmt: &'a Statement) -> BoxFuture<'a, Result<ExecResult, DbError>> {
        Box::pin(async move {
            let db = self.db.clone();
            let started = Instant::now();
            crate::backend::refuse_nul(stmt)?;
            if !self.ddl && changes_schema(&stmt.sql) {
                self.ddl = true;
            }
            let done = bind(stmt)
                .execute(&mut **self.conn()?)
                .await
                .map_err(|e| db.classify(e))?;
            db.inner
                .slow
                .record(stmt, started.elapsed(), done.rows_affected(), true);
            Ok(ExecResult {
                rows_affected: done.rows_affected(),
                last_insert_id: done.last_insert_rowid(),
            })
        })
    }

    fn fetch_all<'a>(&'a mut self, stmt: &'a Statement) -> BoxFuture<'a, Result<Rows, DbError>> {
        Box::pin(async move {
            let db = self.db.clone();
            crate::backend::refuse_nul(stmt)?;
            let started = Instant::now();
            let rows = bind(stmt)
                .fetch_all(&mut **self.conn()?)
                .await
                .map_err(|e| db.classify(e))?;
            let rows = decode_rows(rows).map_err(|e| db.classify(e))?;
            db.inner.slow.record(stmt, started.elapsed(), rows.len() as u64, true);
            Ok(rows)
        })
    }

    fn commit(self: Box<Self>) -> BoxFuture<'static, Result<(), DbError>> {
        Box::pin(self.finish("COMMIT"))
    }

    fn rollback(self: Box<Self>) -> BoxFuture<'static, Result<(), DbError>> {
        Box::pin(self.finish("ROLLBACK"))
    }
}
