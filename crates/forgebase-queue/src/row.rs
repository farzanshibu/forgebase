//! Typed access to result rows.

use std::collections::BTreeMap;

use forgebase_db::{Rows, Value};
use serde_json::Value as Json;

use crate::error::QueueError;

/// Row `i` of `rows`, read by column name.
pub(crate) struct Row<'a> {
    rows: &'a Rows,
    i: usize,
}

impl<'a> Row<'a> {
    pub(crate) fn new(rows: &'a Rows, i: usize) -> Self {
        Self { rows, i }
    }

    /// Every row of `rows`.
    pub(crate) fn all(rows: &'a Rows) -> impl Iterator<Item = Row<'a>> {
        (0..rows.rows.len()).map(move |i| Row::new(rows, i))
    }

    fn value(&self, col: &str) -> &Value {
        static NULL: Value = Value::Null;
        self.rows
            .column(col)
            .and_then(|c| self.rows.rows.get(self.i).and_then(|r| r.get(c)))
            .unwrap_or(&NULL)
    }

    pub(crate) fn text(&self, col: &str) -> String {
        self.value(col).as_str().unwrap_or_default().to_owned()
    }

    pub(crate) fn opt_text(&self, col: &str) -> Option<String> {
        self.value(col).as_str().map(str::to_owned)
    }

    pub(crate) fn int(&self, col: &str) -> i64 {
        self.value(col).as_i64().unwrap_or(0)
    }

    pub(crate) fn opt_int(&self, col: &str) -> Option<i64> {
        self.value(col).as_i64()
    }

    pub(crate) fn blob(&self, col: &str) -> Option<Vec<u8>> {
        match self.value(col) {
            Value::Blob(b) => Some(b.clone()),
            _ => None,
        }
    }

    pub(crate) fn json(&self, col: &str) -> Result<Json, QueueError> {
        serde_json::from_str(self.value(col).as_str().unwrap_or("null"))
            .map_err(|e| QueueError::Internal(format!("column {col} is not JSON: {e}")))
    }

    pub(crate) fn opt_json(&self, col: &str) -> Result<Option<Json>, QueueError> {
        match self.value(col).as_str() {
            None => Ok(None),
            Some(t) => serde_json::from_str(t)
                .map(Some)
                .map_err(|e| QueueError::Internal(format!("column {col} is not JSON: {e}"))),
        }
    }

    pub(crate) fn map(&self, col: &str) -> Result<BTreeMap<String, String>, QueueError> {
        serde_json::from_str(self.value(col).as_str().unwrap_or("{}"))
            .map_err(|e| QueueError::Internal(format!("column {col} is not a string map: {e}")))
    }
}

/// The first column of the first row as an integer.
pub(crate) fn scalar(rows: &Rows) -> i64 {
    rows.scalar().and_then(Value::as_i64).unwrap_or(0)
}
