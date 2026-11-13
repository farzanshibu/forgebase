//! SQL dialects.
//!
//! Everything that differs between SQL engines and that code above
//! `forgebase-db` has to emit goes through [`Dialect`]: identifier quoting,
//! placeholders, JSON access, column types, `LIKE` escaping and literal
//! rendering for index definitions. [`SqliteDialect`] and [`PgDialect`]
//! implement it; callers do not change (ADR-0003 §2, ADR-0028 §7.4).
//!
//! The methods added for PostgreSQL (from [`Dialect::like_ci`] on) have SQLite
//! implementations that return exactly the SQL the code emits today, so the
//! port packages can switch call sites one at a time.

use std::fmt;

use crate::backend::{Statement, Value};

/// One step of a JSON path: an object key or an array index.
///
/// Keys are restricted to `[A-Za-z_][A-Za-z0-9_]*` by [`JsonPathSeg::key`],
/// so a path can be rendered as a SQL literal without escaping concerns.
/// Rendering paths as literals (instead of bound parameters) is what lets the
/// planner match JSON-path expression indexes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum JsonPathSeg {
    /// Object member.
    Key(String),
    /// Array element (0-based).
    Index(u32),
}

impl JsonPathSeg {
    /// A key segment, if `key` is a safe identifier.
    pub fn key(key: &str) -> Option<Self> {
        is_safe_ident(key).then(|| Self::Key(key.to_owned()))
    }
}

/// Whether `s` matches `[A-Za-z_][A-Za-z0-9_]*` and is at most 64 bytes.
pub fn is_safe_ident(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && s.len() <= 64
}

/// Storage class of a column, mapped to a concrete type by the dialect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnType {
    /// UTF-8 text.
    Text,
    /// 64-bit integer.
    Integer,
    /// 64-bit float.
    Real,
    /// Boolean (0/1 in SQLite).
    Boolean,
    /// JSON text (validated by a CHECK constraint).
    Json,
}

/// SQL rendering rules of one engine.
pub trait Dialect: Send + Sync + fmt::Debug {
    /// Engine name (`sqlite`, `postgres`).
    fn name(&self) -> &'static str;

    /// Quotes an identifier. Embedded quote characters are doubled, so any
    /// string is safe here, but callers only pass names validated against the
    /// schema.
    fn quote_ident(&self, ident: &str) -> String {
        format!("\"{}\"", ident.replace('"', "\"\""))
    }

    /// NULL-safe equality operator (`IS` in SQLite, `IS NOT DISTINCT FROM` in PostgreSQL).
    fn null_safe_eq(&self) -> &'static str {
        "IS"
    }

    /// NULL-safe inequality operator.
    fn null_safe_ne(&self) -> &'static str {
        "IS NOT"
    }

    /// Placeholder for the 1-based parameter `index`.
    fn placeholder(&self, index: usize) -> String;

    /// Expression extracting `path` from the JSON text in `expr`.
    fn json_extract(&self, expr: &str, path: &[JsonPathSeg]) -> String;

    /// Table-valued expression iterating the elements of the JSON array in
    /// `expr` under `alias`. Returns the FROM item and the element expression.
    fn json_array_elements(&self, expr: &str, alias: &str) -> (String, String);

    /// Length of the JSON array in `expr` (NULL-safe: 0 for NULL).
    fn json_array_length(&self, expr: &str) -> String;

    /// Expression that is true when `expr` is valid JSON text.
    fn json_valid(&self, expr: &str) -> String;

    /// The `ESCAPE` clause matching [`Dialect::escape_like`].
    fn like_escape_clause(&self) -> &'static str {
        "ESCAPE '\\'"
    }

    /// Escapes `%`, `_` and the escape character itself so `text` matches
    /// literally inside a `LIKE` pattern. The backslash is escaped first.
    fn escape_like(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len() + 2);
        for c in text.chars() {
            if matches!(c, '\\' | '%' | '_') {
                out.push('\\');
            }
            out.push(c);
        }
        out
    }

    /// Renders a string as a SQL literal. Only for statements that cannot take
    /// parameters (index definitions); everything else binds values.
    fn string_literal(&self, s: &str) -> String {
        format!("'{}'", s.replace('\'', "''"))
    }

    /// Concrete column type.
    fn column_type(&self, t: ColumnType) -> &'static str;

    /// Prefix that turns a query into a query-plan request.
    fn explain_prefix(&self) -> &'static str;

    /// Case-insensitive `LIKE` with the [`Dialect::escape_like`] escape:
    /// `expr LIKE pat ESCAPE '\'` (ASCII case folding) on SQLite,
    /// `expr ILIKE pat ESCAPE '\'` on PostgreSQL.
    fn like_ci(&self, expr: &str, pat: &str) -> String;

    /// A Unicode case-insensitive match of `expr` against the regular
    /// expression `pat` (a bound `(?i)…` pattern), where [`Dialect::like_ci`]
    /// folds ASCII only: `expr REGEXP pat` on SQLite (registered on every
    /// ForgeBase connection). `None` where `like_ci` is Unicode already.
    /// Only for bound parameters, never for stored schema (index or view
    /// definitions), which other SQLite connections must be able to read.
    fn regexp_ci(&self, expr: &str, pat: &str) -> Option<String> {
        let _ = (expr, pat);
        None
    }

    /// Sort direction keeping SQLite's NULL order (NULLs first ascending,
    /// last descending), which keyset cursors rely on.
    fn order_nulls(&self, desc: bool) -> &'static str;

    /// `expr` is one of the values of the JSON array bound to `param`
    /// (id lists). On PostgreSQL the elements compare as text: cast `expr`
    /// for non-text columns.
    fn in_json_list(&self, expr: &str, param: &str) -> String;

    /// Aggregate of `expr` into a JSON array (`[]` for no rows).
    fn json_array_agg(&self, expr: &str) -> String;

    /// The JSON array `target` with `value` appended.
    fn json_array_append(&self, target: &str, value: &str) -> String;

    /// The JSON array `target` with member `key` of its last element set to
    /// `value`. `key` must be a safe identifier ([`is_safe_ident`]).
    fn json_set_last(&self, target: &str, key: &str, value: &str) -> String;

    /// `INSERT` that skips rows conflicting with an existing row:
    /// `INSERT OR IGNORE INTO table (cols) source` on SQLite,
    /// `INSERT INTO table (cols) source ON CONFLICT DO NOTHING` on PostgreSQL.
    /// `cols` is the rendered column list, `source` a `VALUES (…)` or `SELECT`.
    fn insert_ignore(&self, table: &str, cols: &str, source: &str) -> String;

    /// Portable upsert (replaces `INSERT OR REPLACE`): inserts `source` into
    /// `cols`, and on a conflict on `key` updates every other column from the
    /// proposed row (or does nothing when every column is part of the key).
    /// Names are quoted. A `SELECT` source needs a `WHERE` clause on SQLite.
    fn upsert(&self, table: &str, cols: &[&str], key: &[&str], source: &str) -> String {
        let q = |c: &&str| self.quote_ident(c);
        let col_list = cols.iter().map(q).collect::<Vec<_>>().join(", ");
        let key_list = key.iter().map(q).collect::<Vec<_>>().join(", ");
        let updates: Vec<String> = cols
            .iter()
            .filter(|c| !key.contains(c))
            .map(|c| format!("{0} = excluded.{0}", self.quote_ident(c)))
            .collect();
        let action = if updates.is_empty() {
            "DO NOTHING".to_owned()
        } else {
            format!("DO UPDATE SET {}", updates.join(", "))
        };
        format!(
            "INSERT INTO {} ({col_list}) {source} ON CONFLICT ({key_list}) {action}",
            self.quote_ident(table)
        )
    }

    /// Clause returning the generated key of an inserted row (replaces
    /// `last_insert_id`, which PostgreSQL does not have).
    fn returning_id(&self, col: &str) -> String {
        format!("RETURNING {}", self.quote_ident(col))
    }

    /// Column that orders rows by insertion: `rowid` on SQLite, `_rowid` (an
    /// identity column the PostgreSQL DDL adds where code orders by `rowid`).
    fn row_order_col(&self) -> &'static str;

    /// Current Unix time in whole seconds.
    fn now_epoch(&self) -> &'static str;

    /// Longest identifier the engine keeps (bytes).
    fn max_ident_len(&self) -> usize;

    /// `name` if it fits [`Dialect::max_ident_len`]; otherwise its first bytes
    /// plus `_` and a 10-hex-digit hash of the whole name, so derived names
    /// (`auto_…`, `_forgebase_fts_…`) stay distinct and stable.
    fn fit_ident(&self, name: &str) -> String {
        let max = self.max_ident_len();
        if name.len() <= max {
            return name.to_owned();
        }
        let mut cut = max - 11;
        while !name.is_char_boundary(cut) {
            cut -= 1;
        }
        format!("{}_{:010x}", &name[..cut], fnv1a(name.as_bytes()) & 0xff_ffff_ffff)
    }

    /// Tables and views: columns `name`, `type` (`table` or `view`) and `sql`
    /// (definition, may be NULL), excluding the engine's own catalog.
    fn introspect_tables(&self) -> Statement;

    /// Columns of `table`: `cid`, `name`, `type`, `notnull` (0/1),
    /// `dflt_value`, `pk` (1-based position in the primary key, else 0),
    /// in column order.
    fn introspect_columns(&self, table: &str) -> Statement;

    /// Indexes of `table`: `seq`, `name`, `unique` (0/1), `origin` (`c`
    /// created by `CREATE INDEX`, `u` unique constraint, `pk` primary key),
    /// `partial` (0/1).
    fn introspect_indexes(&self, table: &str) -> Statement;

    /// Planner statistics of an index: one row, column `stat`, whose first
    /// space-separated number is the estimated row count; no row before the
    /// statistics exist.
    fn index_stats(&self, index: &str) -> Statement;

    /// Bytes used by a table or index (and, on SQLite, its shadow tables):
    /// one row, one column, NULL when unknown.
    fn object_size(&self, name: &str) -> Statement;

    /// Statements creating the full-text index `fts` over `exprs` of `table`
    /// and filling it. `new_exprs` are the same expressions over `new.` (the
    /// SQLite sync triggers use them). Names are unquoted.
    fn fulltext_create(&self, table: &str, fts: &str, exprs: &[String], new_exprs: &[String]) -> Vec<String>;

    /// Statements that keep `fts` in sync with `table` after the table was
    /// rebuilt (SQLite triggers; nothing on PostgreSQL, where the index is a
    /// generated column).
    fn fulltext_sync(&self, table: &str, fts: &str, new_exprs: &[String]) -> Vec<String>;

    /// Statements dropping the full-text index `fts` of `table`.
    fn fulltext_drop(&self, table: &str, fts: &str) -> Vec<String>;

    /// Condition: the row `alias` matches the full-text query bound to `param`.
    fn fulltext_match(&self, alias: &str, fts: &str, param: &str) -> String;

    /// Relevance of row `alias` for the query bound to `param`; **lower is
    /// better** on both engines (SQLite bm25; PostgreSQL negated `ts_rank`,
    /// whose ranks differ from bm25).
    fn fulltext_rank(&self, alias: &str, fts: &str, param: &str) -> String;
}

/// FNV-1a, 64 bit: a stable hash for derived identifiers.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

fn join(items: &[String]) -> String {
    items.join(", ")
}

/// The SQLite dialect (bundled SQLite with JSON1 and FTS5).
#[derive(Debug, Clone, Copy, Default)]
pub struct SqliteDialect;

/// Renders `path` as a SQLite JSON path (`$.a.b[0]`).
fn sqlite_json_path(path: &[JsonPathSeg]) -> String {
    let mut out = String::from("$");
    for seg in path {
        match seg {
            JsonPathSeg::Key(k) => {
                // Keys are validated identifiers; quote anyway for safety.
                debug_assert!(is_safe_ident(k));
                out.push('.');
                out.push_str(k);
            }
            JsonPathSeg::Index(i) => {
                out.push('[');
                out.push_str(&i.to_string());
                out.push(']');
            }
        }
    }
    out
}

impl Dialect for SqliteDialect {
    fn name(&self) -> &'static str {
        "sqlite"
    }

    fn placeholder(&self, index: usize) -> String {
        format!("?{index}")
    }

    fn json_extract(&self, expr: &str, path: &[JsonPathSeg]) -> String {
        let p = sqlite_json_path(path);
        // Defence in depth: never emit a path literal containing a quote.
        assert!(!p.contains('\''), "json path segments are validated identifiers");
        format!("json_extract({expr}, '{p}')")
    }

    fn json_array_elements(&self, expr: &str, alias: &str) -> (String, String) {
        (format!("json_each({expr}) AS {alias}"), format!("{alias}.value"))
    }

    fn json_array_length(&self, expr: &str) -> String {
        format!("coalesce(json_array_length({expr}), 0)")
    }

    fn json_valid(&self, expr: &str) -> String {
        format!("json_valid({expr})")
    }

    fn column_type(&self, t: ColumnType) -> &'static str {
        match t {
            ColumnType::Text | ColumnType::Json => "TEXT",
            ColumnType::Integer | ColumnType::Boolean => "INTEGER",
            ColumnType::Real => "REAL",
        }
    }

    fn explain_prefix(&self) -> &'static str {
        "EXPLAIN QUERY PLAN "
    }

    fn like_ci(&self, expr: &str, pat: &str) -> String {
        format!("{expr} LIKE {pat} {}", self.like_escape_clause())
    }

    fn regexp_ci(&self, expr: &str, pat: &str) -> Option<String> {
        Some(format!("{expr} REGEXP {pat}"))
    }

    fn order_nulls(&self, desc: bool) -> &'static str {
        if desc { "DESC" } else { "ASC" }
    }

    fn in_json_list(&self, expr: &str, param: &str) -> String {
        format!("{expr} IN (SELECT value FROM json_each({param}))")
    }

    fn json_array_agg(&self, expr: &str) -> String {
        format!("json_group_array({expr})")
    }

    fn json_array_append(&self, target: &str, value: &str) -> String {
        format!("json_insert({target}, '$[#]', {value})")
    }

    fn json_set_last(&self, target: &str, key: &str, value: &str) -> String {
        assert!(is_safe_ident(key), "json keys are validated identifiers");
        format!("json_set({target}, '$[#-1].{key}', {value})")
    }

    fn insert_ignore(&self, table: &str, cols: &str, source: &str) -> String {
        format!("INSERT OR IGNORE INTO {table} ({cols}) {source}")
    }

    fn row_order_col(&self) -> &'static str {
        "rowid"
    }

    fn now_epoch(&self) -> &'static str {
        "unixepoch()"
    }

    fn max_ident_len(&self) -> usize {
        1000
    }

    fn introspect_tables(&self) -> Statement {
        Statement::raw(
            "SELECT name, type, sql FROM sqlite_master WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' ORDER BY name",
        )
    }

    fn introspect_columns(&self, table: &str) -> Statement {
        Statement::raw(format!("PRAGMA table_info({})", self.quote_ident(table)))
    }

    fn introspect_indexes(&self, table: &str) -> Statement {
        Statement::raw(format!("PRAGMA index_list({})", self.quote_ident(table)))
    }

    fn index_stats(&self, index: &str) -> Statement {
        Statement::new("SELECT stat FROM sqlite_stat1 WHERE idx = ?1", vec![index.into()])
    }

    fn object_size(&self, name: &str) -> Statement {
        Statement::new(
            "SELECT sum(pgsize) FROM dbstat WHERE name = ?1 OR name LIKE ?2 ESCAPE '\\'",
            vec![name.into(), Value::Text(format!("{}\\_%", self.escape_like(name)))],
        )
    }

    fn fulltext_create(&self, table: &str, fts: &str, exprs: &[String], new_exprs: &[String]) -> Vec<String> {
        let (t, f) = (self.quote_ident(table), self.quote_ident(fts));
        let cols: Vec<String> = (0..exprs.len()).map(|i| format!("c{i}")).collect();
        let mut out = vec![format!(
            "CREATE VIRTUAL TABLE {f} USING fts5({}, content='', contentless_delete=1, tokenize='unicode61 remove_diacritics 2')",
            join(&cols)
        )];
        out.extend(self.fulltext_sync(table, fts, new_exprs));
        out.push(format!(
            "INSERT INTO {f} (rowid, {}) SELECT rowid, {} FROM {t}",
            join(&cols),
            join(exprs)
        ));
        out
    }

    fn fulltext_sync(&self, table: &str, fts: &str, new_exprs: &[String]) -> Vec<String> {
        let (t, f) = (self.quote_ident(table), self.quote_ident(fts));
        let cols: Vec<String> = (0..new_exprs.len()).map(|i| format!("c{i}")).collect();
        let insert = format!(
            "INSERT INTO {f} (rowid, {}) VALUES (new.rowid, {});",
            join(&cols),
            join(new_exprs)
        );
        let delete = format!("DELETE FROM {f} WHERE rowid = old.rowid;");
        vec![
            format!(
                "CREATE TRIGGER {} AFTER INSERT ON {t} BEGIN {insert} END",
                self.quote_ident(&format!("{fts}_ai"))
            ),
            format!(
                "CREATE TRIGGER {} AFTER DELETE ON {t} BEGIN {delete} END",
                self.quote_ident(&format!("{fts}_ad"))
            ),
            format!(
                "CREATE TRIGGER {} AFTER UPDATE ON {t} BEGIN {delete} {insert} END",
                self.quote_ident(&format!("{fts}_au"))
            ),
        ]
    }

    fn fulltext_drop(&self, _table: &str, fts: &str) -> Vec<String> {
        let q = |s: &str| self.quote_ident(s);
        vec![
            format!("DROP TRIGGER IF EXISTS {}", q(&format!("{fts}_ai"))),
            format!("DROP TRIGGER IF EXISTS {}", q(&format!("{fts}_ad"))),
            format!("DROP TRIGGER IF EXISTS {}", q(&format!("{fts}_au"))),
            format!("DROP TABLE IF EXISTS {}", q(fts)),
        ]
    }

    fn fulltext_match(&self, alias: &str, fts: &str, param: &str) -> String {
        let f = self.quote_ident(fts);
        format!("{alias}.rowid IN (SELECT rowid FROM {f} WHERE {f} MATCH {param})")
    }

    fn fulltext_rank(&self, alias: &str, fts: &str, param: &str) -> String {
        let f = self.quote_ident(fts);
        format!("(SELECT bm25({f}) FROM {f} WHERE {f} MATCH {param} AND rowid = {alias}.rowid)")
    }
}

/// The PostgreSQL dialect (PostgreSQL 16 or newer; JSON columns are `jsonb`).
#[derive(Debug, Clone, Copy, Default)]
pub struct PgDialect;

/// Function the PostgreSQL schema provides for full-text indexes: `unaccent`
/// is only `STABLE`, and generated columns need an `IMMUTABLE` expression.
pub const PG_UNACCENT_FN: &str = "_forgebase_unaccent";

/// Statements creating [`PG_UNACCENT_FN`] (for the PostgreSQL baseline
/// migration, FB-133). Needs the `unaccent` extension.
pub const PG_FULLTEXT_SETUP: &[&str] = &[
    "CREATE EXTENSION IF NOT EXISTS unaccent",
    "CREATE OR REPLACE FUNCTION _forgebase_unaccent(text) RETURNS text LANGUAGE sql IMMUTABLE PARALLEL SAFE STRICT AS $$ SELECT unaccent('unaccent'::regdictionary, $1) $$",
];

/// `'{a,b,0}'`: a PostgreSQL text-array path literal.
#[cfg(test)]
fn pg_path_literal(path: &[JsonPathSeg]) -> String {
    let parts: Vec<String> = path
        .iter()
        .map(|seg| match seg {
            JsonPathSeg::Key(k) => {
                debug_assert!(is_safe_ident(k));
                k.clone()
            }
            JsonPathSeg::Index(i) => i.to_string(),
        })
        .collect();
    format!("'{{{}}}'", parts.join(","))
}

impl PgDialect {
    fn tsquery(param: &str) -> String {
        format!("websearch_to_tsquery('simple', {PG_UNACCENT_FN}({param}))")
    }
}

impl Dialect for PgDialect {
    fn name(&self) -> &'static str {
        "postgres"
    }

    fn null_safe_eq(&self) -> &'static str {
        "IS NOT DISTINCT FROM"
    }

    fn null_safe_ne(&self) -> &'static str {
        "IS DISTINCT FROM"
    }

    fn placeholder(&self, index: usize) -> String {
        format!("${index}")
    }

    /// `expr->'a'->>'b'`: text, like the expression indexes on JSON paths
    /// (`((_data->'metadata'->>'brand'))`), so the planner can use them.
    fn json_extract(&self, expr: &str, path: &[JsonPathSeg]) -> String {
        if path.is_empty() {
            return format!("({expr} #>> '{{}}')");
        }
        let mut out = format!("({expr}");
        for (i, seg) in path.iter().enumerate() {
            out.push_str(if i + 1 == path.len() { "->>" } else { "->" });
            match seg {
                JsonPathSeg::Key(k) => {
                    assert!(is_safe_ident(k), "json path segments are validated identifiers");
                    out.push('\'');
                    out.push_str(k);
                    out.push('\'');
                }
                JsonPathSeg::Index(n) => out.push_str(&n.to_string()),
            }
        }
        out.push(')');
        out
    }

    fn json_array_elements(&self, expr: &str, alias: &str) -> (String, String) {
        (
            format!("jsonb_array_elements_text(({expr})::jsonb) AS {alias}(value)"),
            format!("{alias}.value"),
        )
    }

    fn json_array_length(&self, expr: &str) -> String {
        format!(
            "coalesce(CASE WHEN jsonb_typeof(({expr})::jsonb) = 'array' THEN jsonb_array_length(({expr})::jsonb) END, 0)"
        )
    }

    fn json_valid(&self, expr: &str) -> String {
        format!("(({expr})::text IS JSON)")
    }

    fn column_type(&self, t: ColumnType) -> &'static str {
        match t {
            ColumnType::Text => "text",
            ColumnType::Integer => "bigint",
            ColumnType::Real => "double precision",
            ColumnType::Boolean => "smallint",
            ColumnType::Json => "jsonb",
        }
    }

    fn explain_prefix(&self) -> &'static str {
        "EXPLAIN (FORMAT JSON) "
    }

    fn like_ci(&self, expr: &str, pat: &str) -> String {
        format!("{expr} ILIKE {pat} {}", self.like_escape_clause())
    }

    fn order_nulls(&self, desc: bool) -> &'static str {
        if desc { "DESC NULLS LAST" } else { "ASC NULLS FIRST" }
    }

    fn in_json_list(&self, expr: &str, param: &str) -> String {
        format!("{expr} = ANY (SELECT jsonb_array_elements_text(({param})::jsonb))")
    }

    fn json_array_agg(&self, expr: &str) -> String {
        format!("coalesce(jsonb_agg({expr}), '[]'::jsonb)")
    }

    fn json_array_append(&self, target: &str, value: &str) -> String {
        format!("(({target})::jsonb || jsonb_build_array({value}))")
    }

    fn json_set_last(&self, target: &str, key: &str, value: &str) -> String {
        assert!(is_safe_ident(key), "json keys are validated identifiers");
        format!("jsonb_set(({target})::jsonb, '{{-1,{key}}}', jsonb_build_array({value})->0)")
    }

    fn insert_ignore(&self, table: &str, cols: &str, source: &str) -> String {
        format!("INSERT INTO {table} ({cols}) {source} ON CONFLICT DO NOTHING")
    }

    fn row_order_col(&self) -> &'static str {
        "_rowid"
    }

    /// Truncated like SQLite's `unixepoch()` (a plain `::bigint` cast rounds).
    fn now_epoch(&self) -> &'static str {
        "floor(extract(epoch from now()))::bigint"
    }

    fn max_ident_len(&self) -> usize {
        63
    }

    /// `sql`: a view's definition; for a table, a `CREATE TABLE` rebuilt
    /// from the catalog (PostgreSQL keeps no text): columns with type,
    /// `NOT NULL` and default, then the primary key, unique, foreign-key and
    /// check constraints.
    fn introspect_tables(&self) -> Statement {
        Statement::raw(
            "SELECT c.relname AS name, CASE WHEN c.relkind IN ('v', 'm') THEN 'view' ELSE 'table' END AS type, \
             CASE WHEN c.relkind IN ('v', 'm') THEN pg_get_viewdef(c.oid) \
             ELSE 'CREATE TABLE ' || quote_ident(c.relname) || ' (' || concat_ws(', ', \
               (SELECT string_agg(quote_ident(a.attname) || ' ' || format_type(a.atttypid, a.atttypmod) \
                  || CASE WHEN a.attnotnull THEN ' NOT NULL' ELSE '' END \
                  || coalesce(' DEFAULT ' || pg_get_expr(d.adbin, d.adrelid), ''), ', ' ORDER BY a.attnum) \
                FROM pg_catalog.pg_attribute a \
                LEFT JOIN pg_catalog.pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
                WHERE a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped), \
               (SELECT string_agg(pg_get_constraintdef(con.oid), ', ' ORDER BY con.contype DESC, con.conname) \
                FROM pg_catalog.pg_constraint con \
                WHERE con.conrelid = c.oid AND con.contype IN ('p', 'u', 'f', 'c'))) || ')' END AS sql \
             FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE c.relkind IN ('r', 'p', 'v', 'm') AND n.nspname = ANY (current_schemas(false)) \
             ORDER BY c.relname",
        )
    }

    fn introspect_columns(&self, table: &str) -> Statement {
        Statement::new(
            "SELECT a.attnum - 1 AS cid, a.attname::text AS name, format_type(a.atttypid, a.atttypmod) AS type, \
             a.attnotnull::int AS notnull, pg_get_expr(d.adbin, d.adrelid) AS dflt_value, \
             coalesce((SELECT k.ord FROM unnest(pk.indkey) WITH ORDINALITY k(attnum, ord) WHERE k.attnum = a.attnum), 0)::bigint AS pk \
             FROM pg_catalog.pg_attribute a \
             LEFT JOIN pg_catalog.pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
             LEFT JOIN pg_catalog.pg_index pk ON pk.indrelid = a.attrelid AND pk.indisprimary \
             WHERE a.attrelid = to_regclass(quote_ident(?1)) AND a.attnum > 0 AND NOT a.attisdropped \
             ORDER BY a.attnum",
            vec![table.into()],
        )
    }

    fn introspect_indexes(&self, table: &str) -> Statement {
        Statement::new(
            "SELECT (row_number() OVER (ORDER BY c.relname) - 1)::bigint AS seq, c.relname::text AS name, \
             i.indisunique::int AS \"unique\", \
             CASE WHEN i.indisprimary THEN 'pk' WHEN con.contype = 'u' THEN 'u' ELSE 'c' END AS origin, \
             (i.indpred IS NOT NULL)::int AS partial \
             FROM pg_catalog.pg_index i JOIN pg_catalog.pg_class c ON c.oid = i.indexrelid \
             LEFT JOIN pg_catalog.pg_constraint con ON con.conindid = i.indexrelid AND con.contype IN ('u', 'p') \
             WHERE i.indrelid = to_regclass(quote_ident(?1)) ORDER BY c.relname",
            vec![table.into()],
        )
    }

    fn index_stats(&self, index: &str) -> Statement {
        Statement::new(
            "SELECT (greatest(c.reltuples, 0)::bigint)::text AS stat FROM pg_catalog.pg_class c \
             WHERE c.oid = to_regclass(quote_ident(?1)) AND c.reltuples >= 0",
            vec![index.into()],
        )
    }

    fn object_size(&self, name: &str) -> Statement {
        Statement::new(
            "SELECT pg_total_relation_size(to_regclass(quote_ident(?1)))",
            vec![name.into()],
        )
    }

    fn fulltext_create(&self, table: &str, fts: &str, exprs: &[String], _new_exprs: &[String]) -> Vec<String> {
        let (t, f) = (self.quote_ident(table), self.quote_ident(fts));
        let doc: Vec<String> = exprs.iter().map(|e| format!("coalesce(({e})::text, '')")).collect();
        vec![
            format!(
                "ALTER TABLE {t} ADD COLUMN {f} tsvector GENERATED ALWAYS AS (to_tsvector('simple', {PG_UNACCENT_FN}({}))) STORED",
                doc.join(" || ' ' || ")
            ),
            format!(
                "CREATE INDEX {} ON {t} USING gin ({f})",
                self.quote_ident(&self.fit_ident(&format!("{fts}_gin")))
            ),
        ]
    }

    fn fulltext_sync(&self, _table: &str, _fts: &str, _new_exprs: &[String]) -> Vec<String> {
        Vec::new()
    }

    fn fulltext_drop(&self, table: &str, fts: &str) -> Vec<String> {
        vec![format!(
            "ALTER TABLE {} DROP COLUMN IF EXISTS {}",
            self.quote_ident(table),
            self.quote_ident(fts)
        )]
    }

    fn fulltext_match(&self, alias: &str, fts: &str, param: &str) -> String {
        format!("{alias}.{} @@ {}", self.quote_ident(fts), Self::tsquery(param))
    }

    fn fulltext_rank(&self, alias: &str, fts: &str, param: &str) -> String {
        format!(
            "(-ts_rank({alias}.{}, {}))",
            self.quote_ident(fts),
            Self::tsquery(param)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_and_escaping() {
        let d = SqliteDialect;
        assert_eq!(d.quote_ident("a\"b"), "\"a\"\"b\"");
        assert_eq!(d.escape_like(r"50%_\x"), r"50\%\_\\x");
        assert_eq!(d.string_literal("it's"), "'it''s'");
        assert_eq!(d.placeholder(3), "?3");
    }

    #[test]
    fn json_paths_render_as_literals() {
        let d = SqliteDialect;
        let path = [
            JsonPathSeg::key("metadata").unwrap(),
            JsonPathSeg::Index(2),
            JsonPathSeg::key("ram").unwrap(),
        ];
        assert_eq!(
            d.json_extract("t.\"_data\"", &path),
            "json_extract(t.\"_data\", '$.metadata[2].ram')"
        );
        assert!(JsonPathSeg::key("a') OR 1=1 --").is_none());
        assert!(JsonPathSeg::key("1abc").is_none());
        assert!(is_safe_ident("_x9"));
    }

    #[test]
    fn sqlite_methods_render_todays_sql() {
        let d = SqliteDialect;
        assert_eq!(d.like_ci("name", "?1"), r"name LIKE ?1 ESCAPE '\'");
        assert_eq!(d.order_nulls(false), "ASC");
        assert_eq!(d.in_json_list("id", "?2"), "id IN (SELECT value FROM json_each(?2))");
        assert_eq!(d.json_array_agg("x"), "json_group_array(x)");
        assert_eq!(d.json_array_append("h", "?1"), "json_insert(h, '$[#]', ?1)");
        assert_eq!(d.json_set_last("h", "status", "?1"), "json_set(h, '$[#-1].status', ?1)");
        assert_eq!(
            d.insert_ignore("t", "a, b", "VALUES (?1, ?2)"),
            "INSERT OR IGNORE INTO t (a, b) VALUES (?1, ?2)"
        );
        assert_eq!(
            d.upsert("t", &["k", "v"], &["k"], "VALUES (?1, ?2)"),
            "INSERT INTO \"t\" (\"k\", \"v\") VALUES (?1, ?2) ON CONFLICT (\"k\") DO UPDATE SET \"v\" = excluded.\"v\""
        );
        assert_eq!(
            d.upsert("t", &["k"], &["k"], "VALUES (?1)"),
            "INSERT INTO \"t\" (\"k\") VALUES (?1) ON CONFLICT (\"k\") DO NOTHING"
        );
        assert_eq!(d.returning_id("seq"), "RETURNING \"seq\"");
        assert_eq!((d.row_order_col(), d.now_epoch()), ("rowid", "unixepoch()"));
        assert_eq!(d.introspect_columns("Posts").sql, "PRAGMA table_info(\"Posts\")");
        assert_eq!(d.fit_ident("short"), "short");
        let fts = d.fulltext_create("posts", "_forgebase_fts_p", &["a".into()], &["new.a".into()]);
        assert_eq!(fts.len(), 5);
        assert!(fts[0].starts_with("CREATE VIRTUAL TABLE \"_forgebase_fts_p\" USING fts5(c0, content=''"));
        assert_eq!(
            fts[4],
            "INSERT INTO \"_forgebase_fts_p\" (rowid, c0) SELECT rowid, a FROM \"posts\""
        );
        assert_eq!(d.fulltext_drop("posts", "f").len(), 4);
    }

    #[test]
    fn postgres_dialect() {
        let d = PgDialect;
        assert_eq!(d.placeholder(3), "$3");
        assert_eq!(d.null_safe_eq(), "IS NOT DISTINCT FROM");
        let path = [
            JsonPathSeg::key("metadata").unwrap(),
            JsonPathSeg::key("brand").unwrap(),
        ];
        assert_eq!(
            d.json_extract("t.\"_data\"", &path),
            "(t.\"_data\"->'metadata'->>'brand')"
        );
        assert_eq!(d.json_extract("x", &[JsonPathSeg::Index(2)]), "(x->>2)");
        assert_eq!(d.like_ci("name", "$1"), r"name ILIKE $1 ESCAPE '\'");
        assert_eq!(d.order_nulls(true), "DESC NULLS LAST");
        assert_eq!(
            d.insert_ignore("t", "a", "VALUES ($1)"),
            "INSERT INTO t (a) VALUES ($1) ON CONFLICT DO NOTHING"
        );
        assert_eq!(d.column_type(ColumnType::Json), "jsonb");
        assert_eq!(pg_path_literal(&path), "'{metadata,brand}'");
        // Identifiers over 63 bytes get a stable hash suffix.
        let long = format!("auto_{}", "x".repeat(80));
        let fitted = d.fit_ident(&long);
        assert_eq!(fitted.len(), 63);
        assert_eq!(fitted, d.fit_ident(&long));
        assert_ne!(fitted, d.fit_ident(&format!("{long}y")));
        assert!(fitted.starts_with("auto_xxx"));
        let multibyte = "é".repeat(40);
        assert!(d.fit_ident(&multibyte).len() <= 63);
        assert_eq!(d.fulltext_sync("t", "f", &[]), Vec::<String>::new());
        assert!(
            d.fulltext_match("t", "f", "$1")
                .contains("@@ websearch_to_tsquery('simple', _forgebase_unaccent($1))")
        );
    }
}
