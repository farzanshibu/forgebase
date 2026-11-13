//! Row access helpers.

use std::collections::BTreeMap;

use forgebase_db::{Backend, Rows, Statement, TxMode, Value};

use crate::error::{AlertError, Result};

/// Runs `stmts` in one write transaction.
pub(crate) async fn write(db: &dyn Backend, stmts: Vec<Statement>) -> Result<()> {
    if stmts.is_empty() {
        return Ok(());
    }
    let mut tx = db.begin(TxMode::Normal).await?;
    for s in &stmts {
        tx.execute(s).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Reads rows.
pub(crate) async fn read(db: &dyn Backend, sql: &str, params: Vec<Value>) -> Result<Rows> {
    Ok(db.fetch_all(&Statement::new(sql, params)).await?)
}

/// One row of a result.
pub(crate) struct Row<'a> {
    rows: &'a Rows,
    i: usize,
}

impl<'a> Row<'a> {
    /// Every row of `rows`.
    pub(crate) fn all(rows: &'a Rows) -> impl Iterator<Item = Row<'a>> + 'a {
        (0..rows.rows.len()).map(move |i| Row { rows, i })
    }

    fn get(&self, col: &str) -> Option<&Value> {
        let c = self.rows.column(col)?;
        self.rows.rows.get(self.i)?.get(c)
    }

    pub(crate) fn text(&self, col: &str) -> String {
        self.opt_text(col).unwrap_or_default()
    }

    pub(crate) fn opt_text(&self, col: &str) -> Option<String> {
        match self.get(col)? {
            Value::Text(s) => Some(s.clone()),
            Value::Integer(i) => Some(i.to_string()),
            Value::Real(f) => Some(f.to_string()),
            Value::Blob(b) => Some(String::from_utf8_lossy(b).into_owned()),
            Value::Null => None,
        }
    }

    pub(crate) fn int(&self, col: &str) -> i64 {
        self.opt_int(col).unwrap_or(0)
    }

    pub(crate) fn opt_int(&self, col: &str) -> Option<i64> {
        match self.get(col)? {
            Value::Integer(i) => Some(*i),
            Value::Real(f) => Some(*f as i64),
            Value::Text(t) => t.parse().ok(),
            _ => None,
        }
    }

    pub(crate) fn opt_float(&self, col: &str) -> Option<f64> {
        match self.get(col)? {
            Value::Real(f) => Some(*f),
            Value::Integer(i) => Some(*i as f64),
            Value::Text(t) => t.parse().ok(),
            _ => None,
        }
    }

    pub(crate) fn float(&self, col: &str) -> f64 {
        self.opt_float(col).unwrap_or(0.0)
    }

    pub(crate) fn blob(&self, col: &str) -> Option<Vec<u8>> {
        match self.get(col)? {
            Value::Blob(b) => Some(b.clone()),
            Value::Text(t) => Some(t.clone().into_bytes()),
            _ => None,
        }
    }

    pub(crate) fn map(&self, col: &str) -> BTreeMap<String, String> {
        serde_json::from_str(&self.text(col)).unwrap_or_default()
    }

    pub(crate) fn json<T: serde::de::DeserializeOwned + Default>(&self, col: &str) -> T {
        serde_json::from_str(&self.text(col)).unwrap_or_default()
    }
}

/// JSON text of `v`.
pub(crate) fn to_json<T: serde::Serialize>(v: &T) -> Result<String> {
    serde_json::to_string(v).map_err(|e| AlertError::Internal(e.to_string()))
}

/// A random id with a prefix (`al-…`).
pub(crate) fn new_id(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4().simple())
}
