//! [`Backend`] implementation for the embedded SQLite database.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use sqlx::pool::PoolConnection;
use sqlx::sqlite::{SqliteArguments, SqliteRow};
use sqlx::{AssertSqlSafe, Column, Row, Sqlite, TypeInfo, ValueRef};

use crate::backend::{Backend, BoxFuture, ExecResult, PlanStep, Rows, SlowQuery, Statement, TxMode, Value, WriteTx};
use crate::dialect::{Dialect, SqliteDialect};
use crate::{Database, DbError};

/// Default slow-query threshold.
pub const DEFAULT_SLOW_QUERY_THRESHOLD: Duration = Duration::from_millis(200);
/// Entries kept in the slow-query ring buffer.
pub const SLOW_QUERY_LOG_CAPACITY: usize = 256;

static SQLITE: SqliteDialect = SqliteDialect;

/// Bounded in-memory slow-query log shared by all connections of a database.
#[derive(Debug)]
pub(crate) struct SlowLog {
    threshold_us: AtomicU64,
    entries: Mutex<VecDeque<SlowQuery>>,
}

impl Default for SlowLog {
    fn default() -> Self {
        Self {
            threshold_us: AtomicU64::new(DEFAULT_SLOW_QUERY_THRESHOLD.as_micros() as u64),
            entries: Mutex::new(VecDeque::with_capacity(SLOW_QUERY_LOG_CAPACITY)),
        }
    }
}

impl SlowLog {
    fn record(&self, stmt: &Statement, elapsed: Duration, rows: u64) {
        if elapsed.as_micros() as u64 >= self.threshold_us.load(Ordering::Relaxed) {
            let entry = SlowQuery {
                sql: stmt.sql.clone(),
                param_count: stmt.params.len(),
                duration_ms: elapsed.as_secs_f64() * 1000.0,
                at: forgebase_core::now_rfc3339(),
                rows,
                label: stmt.label.clone(),
            };
            tracing::warn!(duration_ms = entry.duration_ms, sql = %entry.sql, "slow query");
            let mut log = self.entries.lock().unwrap_or_else(|p| p.into_inner());
            if log.len() == SLOW_QUERY_LOG_CAPACITY {
                log.pop_front();
            }
            log.push_back(entry);
        }
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

impl Backend for Database {
    fn dialect(&self) -> &dyn Dialect {
        &SQLITE
    }

    fn fetch_all<'a>(&'a self, stmt: &'a Statement) -> BoxFuture<'a, Result<Rows, DbError>> {
        Box::pin(async move {
            let started = Instant::now();
            let rows = bind(stmt)
                .fetch_all(&self.inner.reader)
                .await
                .map_err(|e| self.classify(e))?;
            let rows = decode_rows(rows).map_err(|e| self.classify(e))?;
            self.inner.slow.record(stmt, started.elapsed(), rows.len() as u64);
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
        let log = self.inner.slow.entries.lock().unwrap_or_else(|p| p.into_inner());
        log.iter().cloned().collect()
    }

    fn clear_slow_queries(&self) {
        self.inner
            .slow
            .entries
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
    }

    fn slow_query_threshold(&self) -> Duration {
        Duration::from_micros(self.inner.slow.threshold_us.load(Ordering::Relaxed))
    }

    fn set_slow_query_threshold(&self, threshold: Duration) {
        self.inner
            .slow
            .threshold_us
            .store(threshold.as_micros() as u64, Ordering::Relaxed);
    }
}

/// A write transaction on the single writer connection.
struct SqliteTx {
    /// `None` once committed or rolled back.
    conn: Option<PoolConnection<Sqlite>>,
    mode: TxMode,
    db: Database,
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
            sqlx::query(sql).execute(&mut *conn).await.map_err(|e| db.classify(e))?;
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
            let done = bind(stmt)
                .execute(&mut **self.conn()?)
                .await
                .map_err(|e| db.classify(e))?;
            db.inner.slow.record(stmt, started.elapsed(), done.rows_affected());
            Ok(ExecResult {
                rows_affected: done.rows_affected(),
                last_insert_id: done.last_insert_rowid(),
            })
        })
    }

    fn fetch_all<'a>(&'a mut self, stmt: &'a Statement) -> BoxFuture<'a, Result<Rows, DbError>> {
        Box::pin(async move {
            let db = self.db.clone();
            let started = Instant::now();
            let rows = bind(stmt)
                .fetch_all(&mut **self.conn()?)
                .await
                .map_err(|e| db.classify(e))?;
            let rows = decode_rows(rows).map_err(|e| db.classify(e))?;
            db.inner.slow.record(stmt, started.elapsed(), rows.len() as u64);
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
