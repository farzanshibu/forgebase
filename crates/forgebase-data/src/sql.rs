//! SQL editor backend: raw parameterised SQL with statement classification,
//! batches in one transaction, and schema introspection (ADR-0003 §6).
//!
//! * One statement per call ([`DataService::execute_sql`]) or a batch in one
//!   all-or-nothing transaction ([`DataService::execute_sql_batch`]).
//! * The tokenizer rejects what the editor never runs (transaction control,
//!   `ATTACH`, `VACUUM`, writable pragmas, temporary objects, extension
//!   loading). The engine then decides read vs write by compiling the
//!   statement; reads run on the read pool, whose connections cannot write.
//! * `_forgebase_*` system tables can be read by administrators but only the
//!   system principal may change them.
//! * DDL is recorded in the schema history, and DDL that would break a
//!   collection (dropping its table, renaming a column) is rolled back.

use std::time::Instant;

use forgebase_db::{Rows, Statement, TxMode, Value};
use forgebase_query::{StatementInfo, StatementKind, analyze_statement, split_statements};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::catalog::CatalogSnapshot;
use crate::ddl::{record_history, verify_collections};
use crate::records::sql_to_json;
use crate::{AccessContext, Action, DataError, DataService, MAX_SQL_ROWS, finish};

/// One statement with positional parameters (`?` / `?NNN`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SqlStatement {
    /// SQL text (one statement).
    pub sql: String,
    /// Parameter values (strings, numbers, booleans, null).
    #[serde(default)]
    pub params: Vec<Json>,
}

/// Result of one statement.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SqlResult {
    /// Classification.
    pub kind: StatementKind,
    /// Column names (when rows are returned).
    pub columns: Vec<String>,
    /// Rows.
    pub rows: Vec<Vec<Json>>,
    /// Rows changed (writes).
    pub rows_affected: u64,
    /// Rowid of the last insert (writes).
    pub last_insert_id: Option<i64>,
    /// More rows existed than were returned.
    pub truncated: bool,
    /// Wall time in milliseconds.
    pub duration_ms: f64,
}

/// A column of a table or view.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ColumnInfo {
    /// Name.
    pub name: String,
    /// Declared type.
    pub declared_type: String,
    /// NOT NULL.
    pub not_null: bool,
    /// Default expression.
    pub default: Option<String>,
    /// Part of the primary key.
    pub primary_key: bool,
}

/// A table or view.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TableInfo {
    /// Name.
    pub name: String,
    /// `table` or `view`.
    pub kind: String,
    /// Collection that owns it, if any.
    pub collection: Option<String>,
    /// Columns.
    pub columns: Vec<ColumnInfo>,
    /// Index names.
    pub indexes: Vec<String>,
    /// DDL.
    pub sql: Option<String>,
}

fn bind_params(params: &[Json]) -> Result<Vec<Value>, DataError> {
    params
        .iter()
        .map(|p| {
            Ok(match p {
                Json::Null => Value::Null,
                Json::Bool(b) => Value::from(*b),
                Json::Number(n) => n
                    .as_i64()
                    .map_or_else(|| Value::Real(n.as_f64().unwrap_or(0.0)), Value::Integer),
                Json::String(s) => Value::Text(s.clone()),
                other => Value::Text(other.to_string()),
            })
        })
        .collect()
}

struct Checked {
    sql: String,
    info: StatementInfo,
    params: Vec<Value>,
    kind: StatementKind,
}

fn result_from(kind: StatementKind, rows: Rows, started: Instant) -> SqlResult {
    let truncated = rows.rows.len() > MAX_SQL_ROWS;
    SqlResult {
        kind,
        columns: rows.columns,
        rows: rows
            .rows
            .iter()
            .take(MAX_SQL_ROWS)
            .map(|r| r.iter().map(sql_to_json).collect())
            .collect(),
        rows_affected: 0,
        last_insert_id: None,
        truncated,
        duration_ms: started.elapsed().as_secs_f64() * 1000.0,
    }
}

impl DataService {
    /// Splits, checks and classifies one statement.
    async fn check_sql(&self, ctx: &AccessContext, s: &SqlStatement) -> Result<Checked, DataError> {
        let parts = split_statements(&s.sql)?;
        let [sql] = parts.as_slice() else {
            return Err(DataError::invalid(
                "multiple_statements",
                "send exactly one statement per entry (use a batch for several)",
            ));
        };
        let info = analyze_statement(sql)?;
        let params = bind_params(&s.params)?;
        let mut kind = info.kind;
        if kind == StatementKind::Read {
            // The engine has the final word: e.g. `WITH ... DELETE` writes.
            let probe = Statement::new(sql.clone(), params.clone()).uncached();
            if self.inner.backend.statement_writes(&probe).await? {
                kind = StatementKind::Write;
            }
        }
        let action = if kind == StatementKind::Read {
            Action::ReadSql
        } else {
            Action::WriteSql
        };
        self.authorize(ctx, action, None)?;
        if info.touches_system_tables && kind != StatementKind::Read && !ctx.is_system() {
            return Err(DataError::Forbidden(
                "ForgeBase system tables are read-only here".into(),
            ));
        }
        Ok(Checked {
            sql: sql.clone(),
            info,
            params,
            kind,
        })
    }

    /// Runs one statement. Reads return at most [`MAX_SQL_ROWS`] rows.
    pub async fn execute_sql(&self, ctx: &AccessContext, statement: SqlStatement) -> Result<SqlResult, DataError> {
        let c = self.check_sql(ctx, &statement).await?;
        if c.kind == StatementKind::Read {
            let started = Instant::now();
            let sql = if matches!(c.info.keyword.as_str(), "SELECT" | "WITH" | "VALUES") {
                format!("SELECT * FROM ({}) LIMIT {}", c.sql, MAX_SQL_ROWS + 1)
            } else {
                c.sql.clone()
            };
            let rows = self
                .inner
                .backend
                .fetch_all(&Statement::new(sql, c.params).uncached())
                .await?;
            return Ok(result_from(c.kind, rows, started));
        }
        Ok(self.execute_sql_batch_checked(ctx, vec![c]).await?.remove(0))
    }

    /// Runs statements in one write transaction: all succeed or none.
    pub async fn execute_sql_batch(
        &self,
        ctx: &AccessContext,
        statements: Vec<SqlStatement>,
    ) -> Result<Vec<SqlResult>, DataError> {
        if statements.is_empty() || statements.len() > 1000 {
            return Err(DataError::invalid("invalid_batch", "send 1 to 1000 statements"));
        }
        let mut checked = Vec::with_capacity(statements.len());
        for s in &statements {
            checked.push(self.check_sql(ctx, s).await?);
        }
        self.execute_sql_batch_checked(ctx, checked).await
    }

    async fn execute_sql_batch_checked(
        &self,
        ctx: &AccessContext,
        checked: Vec<Checked>,
    ) -> Result<Vec<SqlResult>, DataError> {
        let schema = checked.iter().any(|c| c.kind == StatementKind::Schema);
        let _guard = if schema {
            Some(self.inner.schema_lock.lock().await)
        } else {
            None
        };
        let d = self.inner.backend.dialect();
        let mut tx = self.begin(TxMode::Normal).await?;
        let result = async {
            let mut out = Vec::with_capacity(checked.len());
            let mut ddl = Vec::new();
            for c in &checked {
                let started = Instant::now();
                let stmt = Statement::new(c.sql.clone(), c.params.clone()).uncached();
                let returns_rows = c.kind == StatementKind::Read || c.info.identifiers.iter().any(|w| w == "returning");
                if returns_rows {
                    let rows = tx.fetch_all(&stmt).await?;
                    out.push(result_from(c.kind, rows, started));
                } else {
                    let done = tx.execute(&stmt).await?;
                    out.push(SqlResult {
                        kind: c.kind,
                        columns: vec![],
                        rows: vec![],
                        rows_affected: done.rows_affected,
                        last_insert_id: (c.info.keyword == "INSERT" || c.info.keyword == "REPLACE")
                            .then_some(done.last_insert_id),
                        truncated: false,
                        duration_ms: started.elapsed().as_secs_f64() * 1000.0,
                    });
                }
                if c.kind == StatementKind::Schema {
                    ddl.push(c.sql.clone());
                }
            }
            if !ddl.is_empty() {
                let cat = CatalogSnapshot::load_tx(&mut *tx).await?;
                verify_collections(d, &mut *tx, &cat).await?;
                record_history(&mut *tx, None, "sql", &ddl, None, None, &ctx.actor()).await?;
            }
            Ok(out)
        }
        .await;
        finish(tx, result).await
    }

    /// The plan of a SQL statement (not executed).
    pub async fn explain_sql(
        &self,
        ctx: &AccessContext,
        statement: SqlStatement,
    ) -> Result<crate::QueryPlan, DataError> {
        let c = self.check_sql(ctx, &statement).await?;
        let steps = self
            .inner
            .backend
            .explain(&Statement::new(c.sql.clone(), c.params).uncached())
            .await?;
        Ok(crate::QueryPlan::from_steps(c.sql, steps))
    }

    /// Tables and views (schema introspection). System tables are omitted.
    pub async fn list_tables(&self, ctx: &AccessContext) -> Result<Vec<TableInfo>, DataError> {
        self.authorize(ctx, Action::ReadSchema, None)?;
        let rows = self
            .inner
            .backend
            .fetch_all(&Statement::new(
                "SELECT name FROM sqlite_master WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\'
                   AND name NOT LIKE '\\_forgebase\\_%' ESCAPE '\\' ORDER BY name",
                vec![],
            ))
            .await?;
        let mut out = Vec::new();
        for r in &rows.rows {
            if let Some(n) = r[0].as_str() {
                out.push(self.table_info(n).await?);
            }
        }
        Ok(out)
    }

    /// One table or view.
    pub async fn describe_table(&self, ctx: &AccessContext, name: &str) -> Result<TableInfo, DataError> {
        self.authorize(ctx, Action::ReadSchema, None)?;
        self.table_info(name).await
    }

    async fn table_info(&self, name: &str) -> Result<TableInfo, DataError> {
        let b = self.inner.backend.as_ref();
        let d = b.dialect();
        let master = b
            .fetch_all(&Statement::new(
                "SELECT name, type, sql FROM sqlite_master WHERE type IN ('table', 'view') AND lower(name) = lower(?1)",
                vec![name.into()],
            ))
            .await?;
        let Some(m) = master.rows.first() else {
            return Err(DataError::CollectionNotFound(name.to_owned()));
        };
        let real = m[0].as_str().unwrap_or(name).to_owned();
        let cols = b
            .fetch_all(&Statement::raw(format!("PRAGMA table_info({})", d.quote_ident(&real))))
            .await?;
        let idx = |c: &str| cols.column(c);
        let columns = match (idx("name"), idx("type"), idx("notnull"), idx("dflt_value"), idx("pk")) {
            (Some(n), Some(t), Some(nn), Some(df), Some(pk)) => cols
                .rows
                .iter()
                .map(|r| ColumnInfo {
                    name: r[n].as_str().unwrap_or_default().to_owned(),
                    declared_type: r[t].as_str().unwrap_or_default().to_owned(),
                    not_null: r[nn].as_i64() == Some(1),
                    default: r[df].as_str().map(str::to_owned),
                    primary_key: r[pk].as_i64().unwrap_or(0) > 0,
                })
                .collect(),
            _ => vec![],
        };
        let indexes = b
            .fetch_all(&Statement::new(
                "SELECT name FROM sqlite_master WHERE type = 'index' AND lower(tbl_name) = lower(?1) ORDER BY name",
                vec![real.clone().into()],
            ))
            .await?
            .rows
            .iter()
            .filter_map(|r| r[0].as_str().map(str::to_owned))
            .collect();
        Ok(TableInfo {
            collection: self.catalog().get(&real).map(|c| c.name.clone()),
            name: real,
            kind: m[1].as_str().unwrap_or_default().to_owned(),
            columns,
            indexes,
            sql: m[2].as_str().map(str::to_owned),
        })
    }
}
