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
//!   system principal may change them. [`DataService::describe_table`] hides
//!   them (and `sqlite_*`) from callers who are not privileged, like
//!   [`DataService::list_tables`] (rls-coverage G12).
//! * DDL is recorded in the schema history, and DDL that would break a
//!   collection (dropping its table, renaming a column) is rolled back.
//! * Secrets (rls-coverage G11): values of secret columns (`password`,
//!   `token_key` and every password field) are masked in results, also as
//!   keys of JSON documents (record versions, change snapshots), and a
//!   statement that names a secret column is refused, unless the call sets
//!   [`SqlOptions::show_secrets`]. Each use of `show_secrets` is audited
//!   (`sql.show_secrets` in `_forgebase_auth_audit`). On SQLite this is a
//!   guard in front of an administrator's tool, not a boundary (a name built
//!   at run time is not seen); on PostgreSQL the editor's role has no
//!   `SELECT` on secret columns (FB-153).

use std::collections::BTreeSet;
use std::time::Instant;

use forgebase_db::{ColumnType, Rows, Statement, TxMode, Value, WriteTx};
use forgebase_query::{StatementInfo, StatementKind, analyze_statement, split_statements};
use forgebase_schema::FieldKind;
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};

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

/// How a SQL editor call treats secret columns.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SqlOptions {
    /// Return secret columns unmasked and allow statements that name them.
    /// Each use is audited.
    #[serde(default)]
    pub show_secrets: bool,
}

/// What a masked secret value reads as.
pub const SECRET_MASK: &str = "********";

/// Event recorded in `_forgebase_auth_audit` for each use of `show_secrets`.
pub const SHOW_SECRETS_EVENT: &str = "sql.show_secrets";

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
    /// Secret values were masked (send `show_secrets` to see them).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub secrets_masked: bool,
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

/// Whether `name` is a table the SQL editor keeps to privileged callers.
fn is_system_table(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.starts_with("_forgebase") || n.starts_with("sqlite_")
}

/// Lower-case names of the secret columns: `password`, `token_key` and every
/// password field of the catalog.
fn secret_columns(cat: &CatalogSnapshot) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = ["password", "token_key"].map(str::to_owned).into();
    for c in cat.all() {
        for f in c.fields.iter().filter(|f| matches!(f.kind, FieldKind::Password { .. })) {
            out.insert(f.name.to_ascii_lowercase());
        }
    }
    out
}

/// The first secret column `sql` names: any word of the text, so quoted
/// identifiers, JSON paths in string literals (`'$.token_key'`) and aliases
/// count.
fn names_secret(sql: &str, secrets: &BTreeSet<String>) -> Option<String> {
    let lower = sql.to_ascii_lowercase();
    lower
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .find(|w| secrets.contains(*w))
        .map(str::to_owned)
}

/// Masks the secret keys of a JSON document held in a text cell; `None` when
/// the text is not such a document or holds no secret key.
fn mask_json_text(text: &str, secrets: &BTreeSet<String>) -> Option<String> {
    let t = text.trim_start();
    if !(t.starts_with('{') || t.starts_with('[')) {
        return None;
    }
    let lower = t.to_ascii_lowercase();
    if !secrets.iter().any(|s| lower.contains(s.as_str())) {
        return None;
    }
    let mut doc: Json = serde_json::from_str(t).ok()?;
    mask_json(&mut doc, secrets).then(|| doc.to_string())
}

fn mask_json(v: &mut Json, secrets: &BTreeSet<String>) -> bool {
    match v {
        Json::Object(m) => {
            let mut changed = false;
            // A change snapshot: `{"c": [column names], "v": [values]}`.
            let secret_at: Vec<usize> = match m.get("c").and_then(Json::as_array) {
                Some(cols) => cols
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| c.as_str().is_some_and(|c| secrets.contains(&c.to_ascii_lowercase())))
                    .map(|(i, _)| i)
                    .collect(),
                None => Vec::new(),
            };
            if let Some(Json::Array(vals)) = m.get_mut("v") {
                for i in secret_at {
                    if let Some(x) = vals.get_mut(i)
                        && !x.is_null()
                        && *x != Json::String(SECRET_MASK.into())
                    {
                        *x = Json::String(SECRET_MASK.into());
                        changed = true;
                    }
                }
            }
            for (k, x) in m.iter_mut() {
                if secrets.contains(&k.to_ascii_lowercase()) {
                    if !x.is_null() && *x != Json::String(SECRET_MASK.into()) {
                        *x = Json::String(SECRET_MASK.into());
                        changed = true;
                    }
                } else {
                    changed |= mask_json(x, secrets);
                }
            }
            changed
        }
        Json::Array(a) => a.iter_mut().fold(false, |acc, x| mask_json(x, secrets) | acc),
        _ => false,
    }
}

/// Masks secret values in a result: whole columns named like a secret, and
/// secret keys inside JSON documents.
fn mask_result(r: &mut SqlResult, secrets: &BTreeSet<String>) {
    let secret_col: Vec<bool> = r
        .columns
        .iter()
        .map(|c| secrets.contains(&c.to_ascii_lowercase()))
        .collect();
    for row in &mut r.rows {
        for (i, v) in row.iter_mut().enumerate() {
            if secret_col.get(i).copied().unwrap_or(false) {
                if !v.is_null() {
                    *v = Json::String(SECRET_MASK.into());
                    r.secrets_masked = true;
                }
            } else if let Json::String(s) = v
                && let Some(masked) = mask_json_text(s, secrets)
            {
                *s = masked;
                r.secrets_masked = true;
            }
        }
    }
}

/// At most `max` characters of `s`.
fn clip(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
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
        secrets_masked: false,
    }
}

impl DataService {
    /// Splits, checks and classifies one statement. With `guard_secrets`, a
    /// statement that names a secret column is refused.
    async fn check_sql(
        &self,
        ctx: &AccessContext,
        s: &SqlStatement,
        guard_secrets: bool,
    ) -> Result<Checked, DataError> {
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
        if guard_secrets && let Some(col) = names_secret(sql, &secret_columns(&self.catalog())) {
            return Err(DataError::Forbidden(format!(
                "the statement names the secret column {col}: set show_secrets to run it (the use is audited)"
            )));
        }
        Ok(Checked {
            sql: sql.clone(),
            info,
            params,
            kind,
        })
    }

    /// Masks the secrets of `results`.
    fn mask_results(&self, results: &mut [SqlResult]) {
        let secrets = secret_columns(&self.catalog());
        for r in results {
            mask_result(r, &secrets);
        }
    }

    /// Records one use of `show_secrets` for `checked`.
    async fn audit_show_secrets(
        &self,
        ctx: &AccessContext,
        tx: &mut dyn WriteTx,
        checked: &[Checked],
    ) -> Result<(), DataError> {
        let d = self.inner.backend.dialect();
        let detail = json!({
            "statements": checked.len(),
            "sql": checked.iter().take(10).map(|c| clip(&c.sql, 500)).collect::<Vec<_>>(),
        });
        tx.execute(&Statement::new(
            format!(
                "INSERT INTO _forgebase_auth_audit (at, event, outcome, actor, request_id, detail, impersonator, realm)
                 VALUES ({}, ?1, 'success', ?2, ?3, CAST(?4 AS {}), ?5, ?6)",
                d.now_epoch(),
                d.column_type(ColumnType::Json)
            ),
            vec![
                SHOW_SECRETS_EVENT.into(),
                ctx.actor().into(),
                ctx.request_id.clone().into(),
                detail.to_string().into(),
                ctx.impersonator.clone().into(),
                ctx.realm_id.clone().into(),
            ],
        ))
        .await?;
        tracing::warn!(actor = %ctx.actor(), statements = checked.len(), "SQL editor: secrets shown");
        Ok(())
    }

    /// Runs one statement. Reads return at most [`MAX_SQL_ROWS`] rows.
    /// Secret columns are masked ([`SqlOptions`]).
    pub async fn execute_sql(&self, ctx: &AccessContext, statement: SqlStatement) -> Result<SqlResult, DataError> {
        self.execute_sql_with(ctx, statement, SqlOptions::default()).await
    }

    /// [`DataService::execute_sql`] with options.
    pub async fn execute_sql_with(
        &self,
        ctx: &AccessContext,
        statement: SqlStatement,
        opts: SqlOptions,
    ) -> Result<SqlResult, DataError> {
        let c = self.check_sql(ctx, &statement, !opts.show_secrets).await?;
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
                .fetch_all(&Statement::new(sql, c.params.clone()).uncached())
                .await?;
            let mut r = result_from(c.kind, rows, started);
            if opts.show_secrets {
                // Shown only once the use is on record.
                let mut tx = self.begin(TxMode::Normal).await?;
                let audited = self.audit_show_secrets(ctx, &mut *tx, &[c]).await;
                finish(tx, audited).await?;
            } else {
                self.mask_results(std::slice::from_mut(&mut r));
            }
            return Ok(r);
        }
        Ok(self.execute_sql_batch_checked(ctx, vec![c], opts).await?.remove(0))
    }

    /// Runs statements in one write transaction: all succeed or none.
    /// Secret columns are masked ([`SqlOptions`]).
    pub async fn execute_sql_batch(
        &self,
        ctx: &AccessContext,
        statements: Vec<SqlStatement>,
    ) -> Result<Vec<SqlResult>, DataError> {
        self.execute_sql_batch_with(ctx, statements, SqlOptions::default())
            .await
    }

    /// [`DataService::execute_sql_batch`] with options; with `show_secrets`
    /// the audit entry commits with the batch.
    pub async fn execute_sql_batch_with(
        &self,
        ctx: &AccessContext,
        statements: Vec<SqlStatement>,
        opts: SqlOptions,
    ) -> Result<Vec<SqlResult>, DataError> {
        if statements.is_empty() || statements.len() > 1000 {
            return Err(DataError::invalid("invalid_batch", "send 1 to 1000 statements"));
        }
        let mut checked = Vec::with_capacity(statements.len());
        for s in &statements {
            checked.push(self.check_sql(ctx, s, !opts.show_secrets).await?);
        }
        self.execute_sql_batch_checked(ctx, checked, opts).await
    }

    async fn execute_sql_batch_checked(
        &self,
        ctx: &AccessContext,
        checked: Vec<Checked>,
        opts: SqlOptions,
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
                        secrets_masked: false,
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
            if opts.show_secrets {
                self.audit_show_secrets(ctx, &mut *tx, &checked).await?;
            } else {
                self.mask_results(&mut out);
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
        // A plan holds no values: secret columns may be named.
        let c = self.check_sql(ctx, &statement, false).await?;
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

    /// One table or view. Callers who are not privileged get the same
    /// not-found error for a system table as for a missing one (G12).
    pub async fn describe_table(&self, ctx: &AccessContext, name: &str) -> Result<TableInfo, DataError> {
        self.authorize(ctx, Action::ReadSchema, None)?;
        let hidden = |n: &str| !ctx.is_privileged() && is_system_table(n);
        if hidden(name) {
            return Err(DataError::CollectionNotFound(name.to_owned()));
        }
        let info = self.table_info(name).await?;
        if hidden(&info.name) {
            return Err(DataError::CollectionNotFound(name.to_owned()));
        }
        Ok(info)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn secrets() -> BTreeSet<String> {
        ["password", "token_key", "pin"].map(str::to_owned).into()
    }

    #[test]
    fn names_secret_sees_every_spelling() {
        let s = secrets();
        for sql in [
            "SELECT password FROM users",
            "SELECT \"Token_Key\" FROM users",
            "SELECT [pin] AS p FROM accounts",
            "SELECT json_extract(data, '$.token_key') FROM v",
            "SELECT id FROM users WHERE upper(password) LIKE 'X%'",
        ] {
            assert!(names_secret(sql, &s).is_some(), "{sql}");
        }
        for sql in [
            "SELECT * FROM users",
            "SELECT password_hint, pinned FROM t",
            "SELECT 'passwords'",
        ] {
            assert_eq!(names_secret(sql, &s), None, "{sql}");
        }
    }

    #[test]
    fn json_documents_are_masked_by_key_and_snapshot_column() {
        let s = secrets();
        let doc = r#"{"id":"a","token_key":"k1","nested":[{"Password":"p1"}],"keep":"x"}"#;
        let out: Json = serde_json::from_str(&mask_json_text(doc, &s).unwrap()).unwrap();
        assert_eq!(out["token_key"], SECRET_MASK);
        assert_eq!(out["nested"][0]["Password"], SECRET_MASK);
        assert_eq!(out["keep"], "x");
        let snap = r#"{"c":["id","token_key","pin"],"v":["a","k1",null]}"#;
        let out: Json = serde_json::from_str(&mask_json_text(snap, &s).unwrap()).unwrap();
        assert_eq!(out["v"], json!(["a", SECRET_MASK, null]));
        assert_eq!(mask_json_text(r#"{"c":["id"],"v":["a"]}"#, &s), None);
        assert_eq!(mask_json_text("password", &s), None);
        assert_eq!(mask_json_text("{not json password", &s), None);
    }

    #[test]
    fn system_tables_are_recognised_in_any_case() {
        for n in [
            "_forgebase_auth_audit",
            "_FORGEBASE_x",
            "sqlite_sequence",
            "SQLITE_stat1",
        ] {
            assert!(is_system_table(n), "{n}");
        }
        for n in ["users", "forgebase_notes", "my_sqlite_table"] {
            assert!(!is_system_table(n), "{n}");
        }
    }
}
