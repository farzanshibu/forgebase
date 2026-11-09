//! Small helpers over the storage [`Backend`]: parameterised statements,
//! typed row access and write transactions. All SQL of this crate goes
//! through here with bound parameters.

use std::sync::Arc;

use forgebase_data::db::{Backend, ExecResult, Rows, Statement, TxMode, Value, WriteTx};

use crate::AuthError;

/// Current Unix time in seconds.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// RFC 3339 rendering of a Unix time.
pub fn rfc3339(ts: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp(ts)
        .map(forgebase_core::format_rfc3339)
        .unwrap_or_default()
}

/// A statement with bound parameters.
pub fn stmt(sql: &str, params: Vec<Value>) -> Statement {
    Statement::new(sql, params)
}

/// One row of a result.
#[derive(Debug, Clone, Copy)]
pub struct Row<'a> {
    cols: &'a [String],
    vals: &'a [Value],
}

impl<'a> Row<'a> {
    fn get(&self, col: &str) -> &'a Value {
        self.cols
            .iter()
            .position(|c| c == col)
            .and_then(|i| self.vals.get(i))
            .unwrap_or(&Value::Null)
    }

    /// Text column (empty string when NULL).
    pub fn str(&self, col: &str) -> String {
        self.opt_str(col).unwrap_or_default()
    }

    /// Nullable text column.
    pub fn opt_str(&self, col: &str) -> Option<String> {
        match self.get(col) {
            Value::Text(s) => Some(s.clone()),
            Value::Integer(i) => Some(i.to_string()),
            _ => None,
        }
    }

    /// Integer column (0 when NULL).
    pub fn i64(&self, col: &str) -> i64 {
        self.opt_i64(col).unwrap_or(0)
    }

    /// Nullable integer column.
    pub fn opt_i64(&self, col: &str) -> Option<i64> {
        match self.get(col) {
            Value::Integer(i) => Some(*i),
            Value::Real(f) => Some(*f as i64),
            _ => None,
        }
    }

    /// Blob column.
    pub fn blob(&self, col: &str) -> Vec<u8> {
        match self.get(col) {
            Value::Blob(b) => b.clone(),
            Value::Text(s) => s.as_bytes().to_vec(),
            _ => Vec::new(),
        }
    }
}

/// Iterates rows.
pub fn rows(r: &Rows) -> impl Iterator<Item = Row<'_>> {
    r.rows.iter().map(|v| Row {
        cols: &r.columns,
        vals: v,
    })
}

/// First row, if any.
pub fn first(r: &Rows) -> Option<Row<'_>> {
    rows(r).next()
}

/// Read on the read pool.
pub async fn read(db: &Arc<dyn Backend>, sql: &str, params: Vec<Value>) -> Result<Rows, AuthError> {
    Ok(db.fetch_all(&stmt(sql, params)).await?)
}

/// A write transaction that rolls back unless committed.
pub struct Tx {
    inner: Option<Box<dyn WriteTx>>,
}

impl Tx {
    /// Starts a write transaction on the single writer.
    pub async fn begin(db: &Arc<dyn Backend>) -> Result<Self, AuthError> {
        Ok(Self {
            inner: Some(db.begin(TxMode::Normal).await?),
        })
    }

    fn tx(&mut self) -> Result<&mut Box<dyn WriteTx>, AuthError> {
        self.inner
            .as_mut()
            .ok_or_else(|| AuthError::Internal("transaction already finished".into()))
    }

    /// Executes a statement.
    pub async fn exec(&mut self, sql: &str, params: Vec<Value>) -> Result<ExecResult, AuthError> {
        let s = stmt(sql, params);
        Ok(self.tx()?.execute(&s).await?)
    }

    /// Reads inside the transaction.
    pub async fn read(&mut self, sql: &str, params: Vec<Value>) -> Result<Rows, AuthError> {
        let s = stmt(sql, params);
        Ok(self.tx()?.fetch_all(&s).await?)
    }

    /// Commits; durable when it returns.
    pub async fn commit(mut self) -> Result<(), AuthError> {
        match self.inner.take() {
            Some(t) => Ok(t.commit().await?),
            None => Err(AuthError::Internal("transaction already finished".into())),
        }
    }
}

impl Drop for Tx {
    fn drop(&mut self) {
        // Dropping the WriteTx rolls back (ADR-0003 §2).
        self.inner.take();
    }
}

/// `Value::Text` helper.
pub fn t(s: impl Into<String>) -> Value {
    Value::Text(s.into())
}

/// `Value::Integer` helper.
pub fn i(v: i64) -> Value {
    Value::Integer(v)
}

/// Nullable text.
pub fn ot(s: Option<impl Into<String>>) -> Value {
    s.map_or(Value::Null, |s| Value::Text(s.into()))
}
