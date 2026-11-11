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
//! * PostgreSQL (ADR-0028 §7.5, FB-143): the tokenizer uses PostgreSQL's
//!   quoting and deny list ([`forgebase_query::SqlFlavor`]); reads run on the
//!   read pool in `READ ONLY` transactions (the reader role in managed mode);
//!   writes and DDL run on the writer with `search_path = public, forgebase`
//!   for the transaction, so tables the editor creates land in `public`,
//!   never in ForgeBase's schema. `last_insert_id` is `None` there (use
//!   `RETURNING`). Introspection is limited to ForgeBase's search path
//!   (`public` and `forgebase`), never `pg_catalog` or `information_schema`.
//! * Roles on PostgreSQL (FB-153, rls-coverage §3.5, G11): once the server
//!   sets [`DataService::set_sql_editor_role`], every editor statement runs
//!   as `forgebase_sql_admin` (`BYPASSRLS`, no `SELECT` on secret columns,
//!   not a superuser), on the read pool and on the writer alike, except
//!   `CREATE|ALTER|DROP POLICY`, which needs the table owner and runs as the
//!   server. Policies named `fb_*` (generated) and objects in the `api`
//!   schema cannot be changed here. **Run as** ([`SqlRunAs`]): the statements
//!   run as the direct-login groups with a chosen user's claims and names
//!   resolve to the `api` views, so the editor shows exactly what a direct
//!   login bound to that user sees; DDL is refused; each call is audited
//!   (`sql.run_as`).

use std::collections::BTreeSet;
use std::time::Instant;

use forgebase_db::{ColumnType, Rows, Statement, TxMode, Value, WriteTx};
use forgebase_query::{SqlFlavor, StatementInfo, StatementKind, analyze_statement_for, split_statements_for};
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

/// Event recorded in `_forgebase_auth_audit` for each "run as" call.
pub const RUN_AS_EVENT: &str = "sql.run_as";

/// The SQL editor's role on PostgreSQL (FB-153).
pub const SQL_ADMIN_ROLE: &str = "forgebase_sql_admin";
/// Group of every named direct login: reads the `api` views.
pub const LOGIN_GROUP: &str = "forgebase_login";
/// Direct logins that may also write through the `api` views.
pub const LOGIN_WRITE_GROUP: &str = "forgebase_login_write";

/// `search_path` of "run as": collection names resolve to the `api` views.
const RUN_AS_SEARCH_PATH: &str = "api, forgebase";

/// The SQL editor's "run as" (FB-153): the claims of a chosen user, computed
/// by the policy engine (`PolicyService::run_as_claims`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlRunAs {
    /// Principal key, for the audit row.
    pub principal: String,
    /// Tenant the claims are for.
    pub tenant: Option<String>,
    /// Claims JSON (`forgebase.claims()`).
    pub claims: String,
}

impl SqlRunAs {
    fn role(&self, write: bool) -> forgebase_db::RunAs {
        forgebase_db::RunAs::new(if write { LOGIN_WRITE_GROUP } else { LOGIN_GROUP }, self.claims.clone())
            .with_search_path(RUN_AS_SEARCH_PATH)
    }
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
    /// Rowid of the last insert (writes; SQLite only: use `RETURNING` on
    /// PostgreSQL).
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
    /// `CREATE|ALTER|DROP POLICY` (PostgreSQL): runs as the server, which
    /// owns the tables.
    policy: bool,
}

/// Lower-case words and punctuation of `sql` outside string literals (a
/// rough scan for the PostgreSQL guards below; the tokenizer already
/// refused what it cannot split).
fn words(sql: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for ch in sql.chars() {
        if let Some(q) = quote {
            if ch == q {
                quote = None;
                if q == '"' {
                    out.push(std::mem::take(&mut cur));
                }
            } else if q == '"' {
                cur.extend(ch.to_lowercase());
            }
            continue;
        }
        match ch {
            '\'' => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                quote = Some('\'');
            }
            '"' => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                quote = Some('"');
            }
            c if c.is_alphanumeric() || c == '_' || c == '$' => cur.extend(c.to_lowercase()),
            c => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                if c == '.' {
                    out.push(".".into());
                }
            }
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// `Some(policy name)` for `CREATE|ALTER|DROP POLICY [IF EXISTS] <name>`.
fn policy_ddl(w: &[String]) -> Option<String> {
    let verb = w.first()?.as_str();
    if !matches!(verb, "create" | "alter" | "drop") || w.get(1).map(String::as_str) != Some("policy") {
        return None;
    }
    let mut i = 2;
    if w.get(i).map(String::as_str) == Some("if") {
        i += 2;
    }
    Some(w.get(i).cloned().unwrap_or_default())
}

/// The statement names an object of the `api` schema (`api.x`).
fn names_api_schema(w: &[String]) -> bool {
    w.windows(2).any(|p| p[0] == "api" && p[1] == ".")
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
        run_as: bool,
    ) -> Result<Checked, DataError> {
        let flavor = SqlFlavor::of(self.inner.backend.dialect());
        let parts = split_statements_for(&s.sql, flavor)?;
        let [sql] = parts.as_slice() else {
            return Err(DataError::invalid(
                "multiple_statements",
                "send exactly one statement per entry (use a batch for several)",
            ));
        };
        let info = analyze_statement_for(sql, flavor)?;
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
        let mut policy = false;
        if SqlFlavor::of(self.inner.backend.dialect()) == SqlFlavor::Postgres {
            let w = words(sql);
            if let Some(name) = policy_ddl(&w) {
                if name.starts_with("fb_") {
                    return Err(DataError::Forbidden(
                        "policies named fb_* are generated from ForgeBase rules and cannot be changed here".into(),
                    ));
                }
                policy = true;
            }
            if kind != StatementKind::Read && names_api_schema(&w) {
                return Err(DataError::Forbidden(
                    "the api schema holds the views ForgeBase generates for direct logins".into(),
                ));
            }
        } else if run_as {
            return Err(DataError::invalid(
                "run_as_unavailable",
                "run as needs PostgreSQL with native row-level security",
            ));
        }
        if run_as && (kind == StatementKind::Schema || policy) {
            return Err(DataError::invalid(
                "run_as_ddl",
                "run as runs reads and writes only; run DDL without it",
            ));
        }
        Ok(Checked {
            sql: sql.clone(),
            info,
            params,
            kind,
            policy,
        })
    }

    /// Who the SQL editor's statements run as on PostgreSQL (FB-153):
    /// [`SQL_ADMIN_ROLE`] once the server confirmed that role exists and
    /// both pools may switch to it; `None` keeps the pools' own roles (SQLite,
    /// external servers without the role).
    pub fn set_sql_editor_role(&self, role: Option<String>) {
        *self.inner.sql_role.write().unwrap_or_else(|p| p.into_inner()) = role;
    }

    /// The role set by [`DataService::set_sql_editor_role`].
    pub fn sql_editor_role(&self) -> Option<String> {
        self.inner.sql_role.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Who one checked statement runs as.
    fn statement_role(&self, c: &Checked, run_as: Option<&SqlRunAs>) -> Option<forgebase_db::RunAs> {
        if c.policy {
            return None;
        }
        if let Some(r) = run_as {
            return Some(r.role(c.kind != StatementKind::Read));
        }
        self.sql_editor_role().map(|role| forgebase_db::RunAs::new(role, ""))
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
        self.audit_sql(ctx, tx, checked, SHOW_SECRETS_EVENT, None).await?;
        tracing::warn!(actor = %ctx.actor(), statements = checked.len(), "SQL editor: secrets shown");
        Ok(())
    }

    async fn audit_sql(
        &self,
        ctx: &AccessContext,
        tx: &mut dyn WriteTx,
        checked: &[Checked],
        event: &str,
        run_as: Option<&SqlRunAs>,
    ) -> Result<(), DataError> {
        let d = self.inner.backend.dialect();
        let mut detail = json!({
            "statements": checked.len(),
            "sql": checked.iter().take(10).map(|c| clip(&c.sql, 500)).collect::<Vec<_>>(),
        });
        if let Some(r) = run_as {
            detail["run_as"] = json!({ "principal": r.principal, "tenant": r.tenant });
        }
        tx.execute(&Statement::new(
            format!(
                "INSERT INTO _forgebase_auth_audit (at, event, outcome, actor, request_id, detail, impersonator, realm)
                 VALUES ({}, ?1, 'success', ?2, ?3, CAST(?4 AS {}), ?5, ?6)",
                d.now_epoch(),
                d.column_type(ColumnType::Json)
            ),
            vec![
                event.to_owned().into(),
                ctx.actor().into(),
                ctx.request_id.clone().into(),
                detail.to_string().into(),
                ctx.impersonator.clone().into(),
                ctx.realm_id.clone().into(),
            ],
        ))
        .await?;
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
        self.execute_sql_as(ctx, statement, opts, None).await
    }

    /// [`DataService::execute_sql_with`], optionally as another user
    /// ([`SqlRunAs`], PostgreSQL only; audited as `sql.run_as`).
    ///
    /// # Errors
    /// As [`DataService::execute_sql`]; `run_as_ddl` for DDL with `run_as`.
    pub async fn execute_sql_as(
        &self,
        ctx: &AccessContext,
        statement: SqlStatement,
        opts: SqlOptions,
        run_as: Option<&SqlRunAs>,
    ) -> Result<SqlResult, DataError> {
        let c = self
            .check_sql(ctx, &statement, !opts.show_secrets, run_as.is_some())
            .await?;
        if c.kind == StatementKind::Read {
            let started = Instant::now();
            let sql = if matches!(c.info.keyword.as_str(), "SELECT" | "WITH" | "VALUES" | "TABLE") {
                format!("SELECT * FROM ({}) LIMIT {}", c.sql, MAX_SQL_ROWS + 1)
            } else {
                c.sql.clone()
            };
            let stmt = Statement::new(sql, c.params.clone()).uncached();
            let rows = match self.statement_role(&c, run_as) {
                Some(role) => self.inner.backend.fetch_all_as(&stmt, &role).await?,
                None => self.inner.backend.fetch_all(&stmt).await?,
            };
            let mut r = result_from(c.kind, rows, started);
            if opts.show_secrets || run_as.is_some() {
                // Shown only once the use is on record.
                let mut tx = self.begin(TxMode::Normal).await?;
                let checked = [c];
                let mut audited = Ok(());
                if opts.show_secrets {
                    audited = self.audit_show_secrets(ctx, &mut *tx, &checked).await;
                }
                if audited.is_ok() && run_as.is_some() {
                    audited = self.audit_sql(ctx, &mut *tx, &checked, RUN_AS_EVENT, run_as).await;
                }
                finish(tx, audited).await?;
            }
            if !opts.show_secrets {
                self.mask_results(std::slice::from_mut(&mut r));
            }
            return Ok(r);
        }
        Ok(self.execute_sql_batch_checked(ctx, vec![c], opts, run_as).await?.remove(0))
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
        self.execute_sql_batch_as(ctx, statements, opts, None).await
    }

    /// [`DataService::execute_sql_batch_with`], optionally as another user
    /// ([`SqlRunAs`]); the audit row commits with the batch.
    ///
    /// # Errors
    /// As [`DataService::execute_sql_batch`]; `run_as_ddl` for DDL with `run_as`.
    pub async fn execute_sql_batch_as(
        &self,
        ctx: &AccessContext,
        statements: Vec<SqlStatement>,
        opts: SqlOptions,
        run_as: Option<&SqlRunAs>,
    ) -> Result<Vec<SqlResult>, DataError> {
        if statements.is_empty() || statements.len() > 1000 {
            return Err(DataError::invalid("invalid_batch", "send 1 to 1000 statements"));
        }
        let mut checked = Vec::with_capacity(statements.len());
        for s in &statements {
            checked.push(
                self.check_sql(ctx, s, !opts.show_secrets, run_as.is_some())
                    .await?,
            );
        }
        self.execute_sql_batch_checked(ctx, checked, opts, run_as).await
    }

    async fn execute_sql_batch_checked(
        &self,
        ctx: &AccessContext,
        checked: Vec<Checked>,
        opts: SqlOptions,
        run_as: Option<&SqlRunAs>,
    ) -> Result<Vec<SqlResult>, DataError> {
        let schema = checked.iter().any(|c| c.kind == StatementKind::Schema);
        let _guard = if schema {
            Some(self.inner.schema_lock.lock().await)
        } else {
            None
        };
        let d = self.inner.backend.dialect();
        let pg = SqlFlavor::of(d) == SqlFlavor::Postgres;
        let mut tx = self.begin(TxMode::Normal).await?;
        let result = async {
            if pg {
                // Unqualified names the statements create land in `public`;
                // ForgeBase's own tables still resolve (second on the path).
                tx.execute(&Statement::raw(PG_EDITOR_SEARCH_PATH)).await?;
            }
            let mut out = Vec::with_capacity(checked.len());
            let mut ddl = Vec::new();
            for c in &checked {
                let started = Instant::now();
                let stmt = Statement::new(c.sql.clone(), c.params.clone()).uncached();
                let returns_rows = c.kind == StatementKind::Read || c.info.identifiers.iter().any(|w| w == "returning");
                let role = self.statement_role(c, run_as);
                if returns_rows {
                    let rows = match &role {
                        Some(r) => tx.fetch_all_as(&stmt, r).await?,
                        None => tx.fetch_all(&stmt).await?,
                    };
                    out.push(result_from(c.kind, rows, started));
                } else {
                    let done = match &role {
                        Some(r) => tx.execute_as(&stmt, r).await?,
                        None => tx.execute(&stmt).await?,
                    };
                    out.push(SqlResult {
                        kind: c.kind,
                        columns: vec![],
                        rows: vec![],
                        rows_affected: done.rows_affected,
                        last_insert_id: (!pg && (c.info.keyword == "INSERT" || c.info.keyword == "REPLACE"))
                            .then_some(done.last_insert_id),
                        truncated: false,
                        duration_ms: started.elapsed().as_secs_f64() * 1000.0,
                        secrets_masked: false,
                    });
                }
                if pg && role.as_ref().is_some_and(|r| r.search_path.is_some()) {
                    // "Run as" switched to the `api` views for that statement.
                    tx.execute(&Statement::raw(PG_EDITOR_SEARCH_PATH)).await?;
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
            if run_as.is_some() {
                self.audit_sql(ctx, &mut *tx, &checked, RUN_AS_EVENT, run_as).await?;
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
        let c = self.check_sql(ctx, &statement, false, false).await?;
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
        let b = self.inner.backend.as_ref();
        let rows = match SqlFlavor::of(b.dialect()) {
            SqlFlavor::Sqlite => {
                b.fetch_all(&Statement::new(
                    "SELECT name FROM sqlite_master WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\'
                   AND name NOT LIKE '\\_forgebase\\_%' ESCAPE '\\' ORDER BY name",
                    vec![],
                ))
                .await?
            }
            // Tables and views on ForgeBase's search path (`forgebase`,
            // `public`); its own tables are left out by name below.
            SqlFlavor::Postgres => b.fetch_all(&b.dialect().introspect_tables()).await?,
        };
        let name_col = rows.column("name").unwrap_or(0);
        let mut out = Vec::new();
        for r in &rows.rows {
            if let Some(n) = r.get(name_col).and_then(Value::as_str)
                && !is_system_table(n)
            {
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

    /// The table or view `name` (any case; on PostgreSQL an exact match
    /// wins, since quoted names are case-sensitive there): `(name, type, sql)`.
    async fn find_table(&self, name: &str) -> Result<Option<(String, String, Option<String>)>, DataError> {
        let b = self.inner.backend.as_ref();
        let text = |r: &[Value], i: Option<usize>| i.and_then(|i| r.get(i)).and_then(Value::as_str).map(str::to_owned);
        let rows = match SqlFlavor::of(b.dialect()) {
            SqlFlavor::Sqlite => {
                b.fetch_all(&Statement::new(
                    "SELECT name, type, sql FROM sqlite_master WHERE type IN ('table', 'view') AND lower(name) = lower(?1)",
                    vec![name.into()],
                ))
                .await?
            }
            SqlFlavor::Postgres => b.fetch_all(&b.dialect().introspect_tables()).await?,
        };
        let (n, t, q) = (rows.column("name"), rows.column("type"), rows.column("sql"));
        let found = rows
            .rows
            .iter()
            .find(|r| text(r, n).as_deref() == Some(name))
            .or_else(|| {
                rows.rows
                    .iter()
                    .find(|r| text(r, n).is_some_and(|x| x.eq_ignore_ascii_case(name)))
            });
        Ok(found.map(|r| {
            (
                text(r, n).unwrap_or_else(|| name.to_owned()),
                text(r, t).unwrap_or_default(),
                text(r, q),
            )
        }))
    }

    async fn table_info(&self, name: &str) -> Result<TableInfo, DataError> {
        let b = self.inner.backend.as_ref();
        let d = b.dialect();
        let Some((real, kind, sql)) = self.find_table(name).await? else {
            return Err(DataError::CollectionNotFound(name.to_owned()));
        };
        let cols = b.fetch_all(&d.introspect_columns(&real)).await?;
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
        let index_rows = match SqlFlavor::of(d) {
            SqlFlavor::Sqlite => {
                b.fetch_all(&Statement::new(
                    "SELECT name FROM sqlite_master WHERE type = 'index' AND lower(tbl_name) = lower(?1) ORDER BY name",
                    vec![real.clone().into()],
                ))
                .await?
            }
            SqlFlavor::Postgres => b.fetch_all(&d.introspect_indexes(&real)).await?,
        };
        let name_col = index_rows.column("name").unwrap_or(0);
        let mut indexes: Vec<String> = index_rows
            .rows
            .iter()
            .filter_map(|r| r.get(name_col).and_then(Value::as_str).map(str::to_owned))
            .collect();
        indexes.sort();
        Ok(TableInfo {
            collection: self.catalog().get(&real).map(|c| c.name.clone()),
            name: real,
            kind,
            columns,
            indexes,
            sql,
        })
    }
}

/// Set at the start of every SQL editor write transaction on PostgreSQL.
const PG_EDITOR_SEARCH_PATH: &str = "SET LOCAL search_path = public, forgebase";

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
    fn policy_ddl_and_api_names_are_seen() {
        let name = |sql: &str| policy_ddl(&words(sql));
        assert_eq!(name("CREATE POLICY p1 ON posts USING (true)").as_deref(), Some("p1"));
        assert_eq!(name("drop policy if exists \"FB_tenant\" on posts").as_deref(), Some("fb_tenant"));
        assert_eq!(name("ALTER POLICY fb_rule_select ON t RENAME TO x").as_deref(), Some("fb_rule_select"));
        assert_eq!(name("SELECT 'create policy fb_x'"), None);
        assert_eq!(name("CREATE TABLE policy (id int)"), None);
        assert!(names_api_schema(&words("DROP VIEW api.posts")));
        assert!(names_api_schema(&words("update \"api\" . \"posts\" set x = 1")));
        assert!(!names_api_schema(&words("SELECT 'api.posts', api_key FROM t")));
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
