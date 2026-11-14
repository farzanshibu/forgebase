//! SQL editor (`/api/v1/sql`): `client.sql()`. Administrators only by default (the data core's
//! authorizer decides). Parameters are positional (`?` / `?1`); values are JSON strings, numbers,
//! booleans or null.

use std::sync::Arc;

use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::error::Result;
use crate::generated::{QueryPlanSchema, SqlBatchResult, SqlResultSchema, TableInfoSchema, TableList};
use crate::http::{Req, Transport, seg};

/// Result of one statement (rows are arrays in `columns` order; at most 10 000 rows, see `truncated`).
pub type SqlResult = SqlResultSchema;
/// A table or view with its columns and indexes.
pub type TableInfo = TableInfoSchema;
/// The query plan of a statement.
pub type QueryPlan = QueryPlanSchema;

/// One statement of a batch.
#[derive(Debug, Clone, Serialize)]
pub struct SqlStatement {
    /// The statement.
    pub sql: String,
    /// Positional parameters.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<Value>,
}

impl SqlStatement {
    /// A statement with parameters.
    #[must_use]
    pub fn new(sql: impl Into<String>, params: Vec<Value>) -> Self {
        Self {
            sql: sql.into(),
            params,
        }
    }
}

impl From<&str> for SqlStatement {
    fn from(sql: &str) -> Self {
        Self::new(sql, Vec::new())
    }
}

/// Run as a principal (PostgreSQL): row and field rules apply, DDL is refused, audited as `sql.run_as`.
#[derive(Debug, Clone, Serialize)]
pub struct SqlRunAs {
    /// `user:<collection>/<id>` or `service:<id>`.
    pub principal: String,
    /// The tenant to act in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
}

/// Options of a SQL call.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SqlOptions {
    /// Return secret columns unmasked and allow statements that name them (audited as `sql.show_secrets`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_secrets: Option<bool>,
    /// Read on a replica that has replayed your own writes (ignored without replicas; writes and batches stay on the primary).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_from_replica: Option<bool>,
    /// Number of the database set a read runs on (`0` or absent: control).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set: Option<u32>,
    /// Run as this principal would.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_as: Option<SqlRunAs>,
}

/// SQL editor: `client.sql()`.
#[derive(Clone)]
pub struct SqlApi {
    pub(crate) t: Arc<Transport>,
}

const S: &str = "/api/v1/sql";

impl SqlApi {
    /// Runs one parameterised statement. Transaction control, `ATTACH`, `VACUUM` and writable
    /// pragmas are refused.
    pub async fn execute(&self, sql: &str, params: &[Value], o: &SqlOptions) -> Result<SqlResult> {
        let mut body = serde_json::to_value(o).unwrap_or_else(|_| json!({}));
        if let Value::Object(m) = &mut body {
            m.insert("sql".into(), json!(sql));
            m.insert("params".into(), json!(params));
        }
        self.t.json(Req::post(S).json_raw(body)).await
    }

    /// [`execute`](Self::execute), then one JSON object per row keyed by column name (the last of
    /// two equal names wins).
    pub async fn query(&self, sql: &str, params: &[Value], o: &SqlOptions) -> Result<Vec<Map<String, Value>>> {
        Ok(rows_as_objects(&self.execute(sql, params, o).await?))
    }

    /// Runs 1 to 1000 statements in one all-or-nothing transaction; one result per statement.
    /// `run_as` and `show_secrets` apply to the whole batch.
    pub async fn batch(&self, statements: &[SqlStatement], o: &SqlOptions) -> Result<Vec<SqlResult>> {
        let mut body = serde_json::to_value(o).unwrap_or_else(|_| json!({}));
        if let Value::Object(m) = &mut body {
            m.remove("read_from_replica");
            m.remove("set");
            m.insert("statements".into(), json!(statements));
        }
        let r: SqlBatchResult = self.t.json(Req::post(format!("{S}/batch")).json_raw(body)).await?;
        Ok(r.items)
    }

    /// The query plan (`EXPLAIN QUERY PLAN`) of a statement, without running it.
    pub async fn explain(&self, sql: &str, params: &[Value]) -> Result<QueryPlan> {
        self.t
            .json(
                Req::post(format!("{S}/explain"))
                    .json_raw(json!({"sql": sql, "params": params}))
                    .idempotent(),
            )
            .await
    }

    /// Tables and views (without ForgeBase system tables) with columns and indexes.
    pub async fn tables(&self) -> Result<Vec<TableInfo>> {
        let r: TableList = self.t.json(Req::get(format!("{S}/tables"))).await?;
        Ok(r.items)
    }

    /// One table or view. A system table is a not-found error unless you are an administrator.
    pub async fn table(&self, name: &str) -> Result<TableInfo> {
        self.t.json(Req::get(format!("{S}/tables/{}", seg(name)))).await
    }
}

/// Turns a result into one object per row, keyed by column name (the last of two equal names wins).
#[must_use]
pub fn rows_as_objects(r: &SqlResult) -> Vec<Map<String, Value>> {
    r.rows
        .iter()
        .map(|row| r.columns.iter().zip(row).map(|(c, v)| (c.clone(), v.clone())).collect())
        .collect()
}
