//! The storage backend abstraction (ADR-0003 §2).
//!
//! Code above `forgebase-db` talks to the database only through [`Backend`]
//! and [`WriteTx`]: it hands over a [`Statement`] (SQL text rendered through
//! the backend's [`Dialect`] plus bound [`Value`]s) and gets plain [`Rows`]
//! back. No driver types cross this boundary, so a PostgreSQL backend can
//! implement the same traits later without changing callers.
//!
//! Concurrency contract (ADR-0001 §3): [`Backend::fetch_all`] runs on the read
//! pool (consistent snapshot, can never write); [`Backend::begin`] takes the
//! single writer and starts an immediate transaction, so writers queue instead
//! of failing on lock upgrades.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use serde::Serialize;

use crate::DbError;
use crate::dialect::Dialect;
use crate::routing::ReadIntent;

/// A boxed, sendable future (object-safe async trait methods without extra crates).
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A SQL value, independent of the driver.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Value {
    /// SQL NULL.
    Null,
    /// 64-bit integer.
    Integer(i64),
    /// 64-bit float.
    Real(f64),
    /// UTF-8 text.
    Text(String),
    /// Bytes.
    Blob(Vec<u8>),
}

impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Value::Text(v.to_owned())
    }
}
impl From<String> for Value {
    fn from(v: String) -> Self {
        Value::Text(v)
    }
}
impl From<i64> for Value {
    fn from(v: i64) -> Self {
        Value::Integer(v)
    }
}
impl From<f64> for Value {
    fn from(v: f64) -> Self {
        Value::Real(v)
    }
}
impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Value::Integer(i64::from(v))
    }
}
impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(v: Option<T>) -> Self {
        v.map_or(Value::Null, Into::into)
    }
}

impl Value {
    /// Text content, if this is text.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Text(s) => Some(s),
            _ => None,
        }
    }

    /// Integer content (also accepts integral reals).
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Integer(i) => Some(*i),
            Value::Real(f) if f.fract() == 0.0 && f.is_finite() => Some(*f as i64),
            _ => None,
        }
    }

    /// Whether this is NULL.
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }
}

/// What a statement is about, for the slow-query log and index recommendations.
/// Opaque to the backend.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct QueryLabel {
    /// Collection (or table) the statement reads.
    pub collection: String,
    /// Field paths compared for equality in the filter, in order of appearance.
    pub equality_keys: Vec<String>,
    /// Field paths compared with range operators.
    pub range_keys: Vec<String>,
    /// Field paths in the ORDER BY clause.
    pub sort_keys: Vec<String>,
}

/// SQL text plus bound parameters. Placeholders come from
/// [`Dialect::placeholder`]; parameter `n` binds `params[n - 1]`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Statement {
    /// SQL text. Never contains user input except through parameters.
    pub sql: String,
    /// Bound values.
    pub params: Vec<Value>,
    /// Optional description for the slow-query log.
    pub label: Option<QueryLabel>,
    /// Keep the prepared statement in the per-connection cache. `false` for
    /// one-off statements (SQL editor, DDL).
    pub cache: bool,
}

impl Statement {
    /// A statement with parameters, cached.
    pub fn new(sql: impl Into<String>, params: Vec<Value>) -> Self {
        Self {
            sql: sql.into(),
            params,
            label: None,
            cache: true,
        }
    }

    /// A statement without parameters, not cached (DDL and one-offs).
    pub fn raw(sql: impl Into<String>) -> Self {
        Self {
            sql: sql.into(),
            params: Vec::new(),
            label: None,
            cache: false,
        }
    }

    /// Same statement with a slow-query label.
    pub fn with_label(mut self, label: QueryLabel) -> Self {
        self.label = Some(label);
        self
    }

    /// Same statement, not kept in the statement cache.
    pub fn uncached(mut self) -> Self {
        self.cache = false;
        self
    }
}

/// Result rows.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Rows {
    /// Column names, in order.
    pub columns: Vec<String>,
    /// Row values, one `Vec` per row, aligned with `columns`.
    pub rows: Vec<Vec<Value>>,
}

impl Rows {
    /// Index of a column by name.
    pub fn column(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c == name)
    }

    /// Number of rows.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether there are no rows.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// First column of the first row.
    pub fn scalar(&self) -> Option<&Value> {
        self.rows.first().and_then(|r| r.first())
    }
}

/// Outcome of a write statement.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ExecResult {
    /// Rows changed by the statement.
    pub rows_affected: u64,
    /// Row id of the last inserted row (engine specific; 0 if none).
    pub last_insert_id: i64,
}

/// How a write transaction treats foreign keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxMode {
    /// Normal: constraints enforced immediately.
    Normal,
    /// Schema change (table rebuild): foreign-key enforcement is suspended for
    /// the transaction and the whole database is checked with a foreign-key
    /// check before commit; any violation aborts the commit. This is SQLite's
    /// documented procedure for changing a table's definition.
    SchemaChange,
}

/// One step of a query plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlanStep {
    /// Node id.
    pub id: i64,
    /// Parent node id.
    pub parent: i64,
    /// Engine description, e.g. `SEARCH t USING INDEX idx (a=?)`.
    pub detail: String,
}

/// An entry of the slow-query log.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SlowQuery {
    /// SQL text (parameters are not recorded: they may hold user data).
    pub sql: String,
    /// Number of bound parameters.
    pub param_count: usize,
    /// Wall time in milliseconds.
    pub duration_ms: f64,
    /// RFC 3339 time the statement finished.
    pub at: String,
    /// Rows returned (reads) or changed (writes).
    pub rows: u64,
    /// Caller-supplied description.
    pub label: Option<QueryLabel>,
}

/// Who statements run as under PostgreSQL native row-level security
/// (FB-152, rls-coverage §3.3): `SET LOCAL ROLE <role>` plus
/// `set_config('forgebase.claims', <claims>, true)` in the same transaction.
/// SQLite has no roles: every method taking one ignores it there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunAs {
    /// Role switched to (`forgebase_rls`). Lower-case letters, digits and `_`.
    pub role: String,
    /// The caller's claims as JSON text (read back by `forgebase.claims()`).
    pub claims: String,
}

impl RunAs {
    /// Runs as `role` with `claims`.
    pub fn new(role: impl Into<String>, claims: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            claims: claims.into(),
        }
    }

    /// Refuses role names that are not plain identifiers.
    pub fn check(&self) -> Result<(), DbError> {
        let ok = !self.role.is_empty()
            && self.role.len() <= 63
            && self
                .role
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
        if ok {
            Ok(())
        } else {
            Err(DbError::Statement(format!("invalid role name {:?}", self.role)))
        }
    }
}

/// A storage engine: SQLite now, PostgreSQL later.
pub trait Backend: Send + Sync + fmt::Debug {
    /// SQL rendering rules of this engine.
    fn dialect(&self) -> &dyn Dialect;

    /// Runs a read-only statement on the read pool. A statement that tries to
    /// write fails ([`DbError::ReadOnly`]) instead of writing.
    fn fetch_all<'a>(&'a self, stmt: &'a Statement) -> BoxFuture<'a, Result<Rows, DbError>>;

    /// Runs a read-only statement where `intent` allows (ADR-0029 §2.5):
    /// on a set with replicas ([`crate::SetHandle`]), a healthy replica that
    /// has replayed the caller's commits, else the primary. Every other
    /// backend runs [`Backend::fetch_all`]. Only the call sites of the §2.5
    /// table pass [`ReadIntent::replica`].
    fn fetch_routed<'a>(&'a self, stmt: &'a Statement, intent: &'a ReadIntent) -> BoxFuture<'a, Result<Rows, DbError>> {
        let _ = intent;
        self.fetch_all(stmt)
    }

    /// Starts a write transaction on the single writer.
    fn begin(&self, mode: TxMode) -> BoxFuture<'_, Result<Box<dyn WriteTx>, DbError>>;

    /// Starts a write transaction whose statements all run as `run_as`
    /// (PostgreSQL: `SET LOCAL ROLE` + claims for the whole transaction).
    /// Default (SQLite): [`Backend::begin`].
    fn begin_as<'a>(&'a self, mode: TxMode, run_as: &'a RunAs) -> BoxFuture<'a, Result<Box<dyn WriteTx>, DbError>> {
        let _ = run_as;
        self.begin(mode)
    }

    /// [`Backend::fetch_all`] as `run_as` (PostgreSQL: the read-only
    /// transaction switches role and sets the claims first). Default
    /// (SQLite): [`Backend::fetch_all`].
    fn fetch_all_as<'a>(&'a self, stmt: &'a Statement, run_as: &'a RunAs) -> BoxFuture<'a, Result<Rows, DbError>> {
        let _ = run_as;
        self.fetch_all(stmt)
    }

    /// The engine's query plan for a statement (not executed).
    fn explain<'a>(&'a self, stmt: &'a Statement) -> BoxFuture<'a, Result<Vec<PlanStep>, DbError>>;

    /// Asks the engine whether a single statement would write (prepared,
    /// not executed). Errors for statements that do not compile.
    fn statement_writes<'a>(&'a self, stmt: &'a Statement) -> BoxFuture<'a, Result<bool, DbError>>;

    /// Recent statements slower than the threshold, newest last.
    fn slow_queries(&self) -> Vec<SlowQuery>;

    /// Clears the slow-query log.
    fn clear_slow_queries(&self);

    /// Current slow-query threshold.
    fn slow_query_threshold(&self) -> Duration;

    /// Sets the slow-query threshold (`Duration::ZERO` logs every statement).
    fn set_slow_query_threshold(&self, threshold: Duration);

    /// Runs statements that may not run inside a transaction block (`CREATE
    /// INDEX CONCURRENTLY`, `REINDEX … CONCURRENTLY`, FB-162) on a
    /// maintenance connection of their own: not the writer, not a reader,
    /// each statement in autocommit, in order, stopping at the first error;
    /// the connection is closed afterwards. Session settings (`SET …`) apply
    /// to the statements after them. The default refuses with
    /// [`DbError::Unsupported`] (SQLite has no such statements).
    fn maintenance<'a>(&'a self, statements: &'a [String]) -> BoxFuture<'a, Result<(), DbError>> {
        let _ = statements;
        Box::pin(async { Err(DbError::Unsupported("maintenance statements need PostgreSQL".into())) })
    }
}

/// An open write transaction. Dropping it without [`WriteTx::commit`] rolls
/// back (the connection is discarded, which SQLite rolls back atomically).
pub trait WriteTx: Send {
    /// Runs a statement and returns its effect.
    fn execute<'a>(&'a mut self, stmt: &'a Statement) -> BoxFuture<'a, Result<ExecResult, DbError>>;

    /// Runs a statement and returns its rows (reads inside the transaction,
    /// `RETURNING` clauses).
    fn fetch_all<'a>(&'a mut self, stmt: &'a Statement) -> BoxFuture<'a, Result<Rows, DbError>>;

    /// [`WriteTx::execute`] with only this statement run as `run_as`
    /// (PostgreSQL: role and claims switched before it and back after it).
    /// Default (SQLite): [`WriteTx::execute`].
    fn execute_as<'a>(
        &'a mut self,
        stmt: &'a Statement,
        run_as: &'a RunAs,
    ) -> BoxFuture<'a, Result<ExecResult, DbError>> {
        let _ = run_as;
        self.execute(stmt)
    }

    /// [`WriteTx::fetch_all`] with only this statement run as `run_as`.
    /// Default (SQLite): [`WriteTx::fetch_all`].
    fn fetch_all_as<'a>(&'a mut self, stmt: &'a Statement, run_as: &'a RunAs) -> BoxFuture<'a, Result<Rows, DbError>> {
        let _ = run_as;
        self.fetch_all(stmt)
    }

    /// Commits. Durable when this returns `Ok` (ADR-0001 `synchronous`).
    fn commit(self: Box<Self>) -> BoxFuture<'static, Result<(), DbError>>;

    /// Rolls back.
    fn rollback(self: Box<Self>) -> BoxFuture<'static, Result<(), DbError>>;
}
