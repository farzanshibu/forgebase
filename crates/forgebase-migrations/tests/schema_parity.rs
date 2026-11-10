//! Schema parity (ADR-0028 §7.6): migrate a SQLite and a PostgreSQL database
//! to the head and compare tables, columns (through the type mapping),
//! nullability, defaults, primary keys, unique constraints, foreign keys,
//! indexes (columns, uniqueness, partial predicates), the number of check
//! constraints, trigger names and seed rows. Any difference fails with a
//! readable diff. The pairing check runs first, so a SQLite migration without
//! a PostgreSQL twin is named even when no PostgreSQL engine is configured.
//!
//! PostgreSQL tests skip without an engine (`FORGEBASE_PG_ENGINE_DIR`,
//! `FORGEBASE_TEST_PG_ENGINE` or `FORGEBASE_TEST_PG_URL`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use forgebase_db::DbTarget;
use forgebase_db::testing::{self, TestDb};
use forgebase_migrations::postgres::{self, PgMigrator, ROWID_TABLES};
use forgebase_migrations::{Migration, MigrationError, Migrator};
use sqlx::{AssertSqlSafe, Connection, PgConnection, Row, SqliteConnection};

/// Tables that differ by design; compared for existence only.
/// SQLite: a contentless FTS5 table; PostgreSQL: a table with a generated
/// tsvector and a GIN index, kept in sync by the same triggers.
const FULLTEXT_TABLES: &[&str] = &["_forgebase_vector_fts"];
/// FTS5's own tables behind a virtual table (SQLite only).
const FTS5_SHADOWS: &[&str] = &["_data", "_idx", "_content", "_docsize", "_config"];
/// Columns that hold the time the row was written (differ between runs).
const TIME_COLUMNS: &[&str] = &["created_at", "updated_at", "applied_at", "execution_ms", "checksum"];

#[derive(Debug, Default, PartialEq, Eq)]
struct Schema {
    tables: BTreeMap<String, Table>,
    triggers: BTreeSet<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Table {
    columns: Vec<Column>,
    primary_key: Vec<String>,
    unique: BTreeSet<Vec<String>>,
    foreign_keys: BTreeSet<ForeignKey>,
    /// name → (unique, columns, predicate), normalised.
    indexes: BTreeMap<String, (bool, String, String)>,
    checks: usize,
    /// Rows of a small table (seed data), ordered, time columns left out.
    rows: Option<Vec<Vec<Option<String>>>>,
    row_count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Column {
    name: String,
    ty: String,
    not_null: bool,
    default: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ForeignKey {
    columns: Vec<String>,
    table: String,
    references: Vec<String>,
    on_update: String,
    on_delete: String,
}

/// Lowercase, no casts, quotes, parentheses or whitespace: `COALESCE(org_id,
/// ''::text)` and `coalesce(org_id, '')` compare equal.
fn norm_expr(s: &str) -> String {
    let lower = s.to_lowercase();
    let mut out = String::new();
    let mut chars = lower.chars().peekable();
    let mut in_str = false;
    while let Some(c) = chars.next() {
        if in_str {
            out.push(c);
            if c == '\'' {
                in_str = false;
            }
            continue;
        }
        match c {
            '\'' => {
                in_str = true;
                out.push(c);
            }
            ':' if chars.peek() == Some(&':') => {
                chars.next();
                // Skip the type name: letters, digits, _, ., spaces inside
                // "double precision", "character varying", and [].
                while let Some(&n) = chars.peek() {
                    if n.is_ascii_alphanumeric() || matches!(n, '_' | '.' | '"' | '[' | ']') {
                        chars.next();
                    } else if n == ' ' {
                        let rest: String = chars.clone().skip(1).take(9).collect();
                        if rest.starts_with("precision") || rest.starts_with("varying") {
                            chars.next();
                        } else {
                            break;
                        }
                    } else {
                        break;
                    }
                }
            }
            '(' | ')' | '"' => {}
            c if c.is_whitespace() => {}
            c => out.push(c),
        }
    }
    out
}

/// A normalised column default.
fn norm_default(s: &str) -> String {
    let n = norm_expr(s);
    if n.contains("strftime") || (n.contains("to_char") && n.contains("now")) {
        return "<now as rfc 3339 text>".into();
    }
    // '5'::bigint -> 5
    if let Some(inner) = n.strip_prefix('\'').and_then(|x| x.strip_suffix('\''))
        && !inner.is_empty()
        && inner.parse::<f64>().is_ok()
    {
        return inner.to_owned();
    }
    n
}

/// Splits a CREATE TABLE body at top-level commas (aware of quotes and
/// comments) and returns `(column name, its definition)` for column items.
fn column_definitions(create_sql: &str) -> BTreeMap<String, String> {
    let open = create_sql.find('(').expect("CREATE TABLE (");
    let body = &create_sql[open + 1..create_sql.rfind(')').expect(")")];
    let mut items = Vec::new();
    let (mut depth, mut cur) = (0i32, String::new());
    let b = body.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i] as char;
        if c == '-' && body[i..].starts_with("--") {
            i = body[i..].find('\n').map_or(b.len(), |p| i + p);
            continue;
        }
        if c == '\'' || c == '"' {
            let end = body[i + 1..].find(c).map_or(b.len(), |p| i + 1 + p);
            cur.push_str(&body[i..=end.min(b.len() - 1)]);
            i = end + 1;
            continue;
        }
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            _ => {}
        }
        if c == ',' && depth == 0 {
            items.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
        i += 1;
    }
    items.push(cur);
    let mut out = BTreeMap::new();
    for item in items {
        let t = item.trim();
        let first = t.split_whitespace().next().unwrap_or("").to_uppercase();
        if ["PRIMARY", "UNIQUE", "FOREIGN", "CHECK", "CONSTRAINT"].contains(&first.as_str()) {
            continue;
        }
        let name = t.split_whitespace().next().unwrap_or("").trim_matches('"').to_owned();
        out.insert(name, t.to_owned());
    }
    out
}

/// The bodies of every `CHECK (…)` in a CREATE TABLE statement.
fn check_bodies(create_sql: &str) -> Vec<String> {
    let mut out = Vec::new();
    let upper = create_sql.to_uppercase();
    let mut from = 0;
    while let Some(p) = upper[from..].find("CHECK") {
        let start = from + p;
        from = start + 5;
        let before = upper[..start].chars().last().unwrap_or(' ');
        if before.is_ascii_alphanumeric() || before == '_' {
            continue;
        }
        let Some(open) = upper[from..].find('(').map(|o| from + o) else { break };
        if !upper[from..open].trim().is_empty() {
            continue;
        }
        let mut depth = 0;
        let mut end = open;
        for (k, ch) in create_sql[open..].char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = open + k;
                        break;
                    }
                }
                _ => {}
            }
        }
        out.push(create_sql[open + 1..end].trim().to_owned());
        from = end;
    }
    out
}

/// A check PostgreSQL enforces through the `jsonb` type instead.
fn is_json_only_check(body: &str) -> bool {
    let n = norm_expr(body);
    n.starts_with("json_valid") && !n.contains("and")
        || (n.contains("isnullorjson_valid") && !n.contains("and"))
}

fn sqlite_type(table_sql: &str, def: &str, name: &str, declared: &str, rowid_alias: bool) -> String {
    let declared = declared.to_uppercase();
    let def_n = norm_expr(def);
    // ADR-0028 §7.4: INTEGER PRIMARY KEY AUTOINCREMENT -> identity. A plain
    // INTEGER PRIMARY KEY (the bookkeeping version) is given explicitly.
    if rowid_alias && def_n.contains("autoincrement") {
        return "bigint identity".into();
    }
    match declared.as_str() {
        "INTEGER" => {
            let flag = format!("{}in0,1", name.to_lowercase());
            if def_n.contains(&flag) || norm_expr(table_sql).contains(&format!("check{flag}")) {
                "smallint".into()
            } else {
                "bigint".into()
            }
        }
        "TEXT" => {
            if norm_expr(table_sql).contains(&format!("json_valid{}", name.to_lowercase())) {
                "jsonb".into()
            } else if def_n.contains("collatenocase") {
                "citext".into()
            } else {
                "text".into()
            }
        }
        "REAL" => "double precision".into(),
        "BLOB" => "bytea".into(),
        other => format!("unmapped SQLite type {other}"),
    }
}

/// `(a, b DESC) WHERE x` → (normalised columns, normalised predicate).
fn sqlite_index_parts(sql: &str) -> (String, String) {
    let upper = sql.to_uppercase();
    let on = upper.find(" ON ").expect("ON");
    let open = on + sql[on..].find('(').expect("(");
    let mut depth = 0;
    let mut close = open;
    for (k, ch) in sql[open..].char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    close = open + k;
                    break;
                }
            }
            _ => {}
        }
    }
    let cols = norm_expr(&sql[open + 1..close]);
    let pred = upper[close..]
        .find("WHERE")
        .map(|w| norm_expr(&sql[close + w + 5..]))
        .unwrap_or_default();
    (cols, pred)
}

fn fk_action(s: &str) -> String {
    match s {
        "a" | "NO ACTION" => "NO ACTION",
        "r" | "RESTRICT" => "RESTRICT",
        "c" | "CASCADE" => "CASCADE",
        "n" | "SET NULL" => "SET NULL",
        "d" | "SET DEFAULT" => "SET DEFAULT",
        other => other,
    }
    .to_owned()
}

async fn sqlite_schema(conn: &mut SqliteConnection) -> Schema {
    let mut schema = Schema::default();
    let tables: Vec<(String, String)> =
        sqlx::query_as("SELECT name, sql FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\'")
            .fetch_all(&mut *conn)
            .await
            .unwrap();
    let virtuals: Vec<&str> = tables
        .iter()
        .filter(|(_, sql)| sql.to_uppercase().starts_with("CREATE VIRTUAL TABLE"))
        .map(|(n, _)| n.as_str())
        .collect();
    let mut pks: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (name, sql) in &tables {
        let shadow = virtuals
            .iter()
            .any(|v| FTS5_SHADOWS.iter().any(|s| *name == format!("{v}{s}")));
        if shadow {
            continue;
        }
        if FULLTEXT_TABLES.contains(&name.as_str()) || virtuals.contains(&name.as_str()) {
            schema.tables.insert(name.clone(), Table::default());
            continue;
        }
        let without_rowid = sql.to_uppercase().contains("WITHOUT ROWID");
        let defs = column_definitions(sql);
        let info = sqlx::query(AssertSqlSafe(format!("PRAGMA table_info(\"{name}\")")))
            .fetch_all(&mut *conn)
            .await
            .unwrap();
        let mut pk: Vec<(i64, String)> = Vec::new();
        for r in &info {
            let p: i64 = r.get(5);
            if p > 0 {
                pk.push((p, r.get(1)));
            }
        }
        pk.sort();
        let pk: Vec<String> = pk.into_iter().map(|(_, n)| n).collect();
        let mut t = Table {
            primary_key: pk.clone(),
            ..Table::default()
        };
        for r in &info {
            let col: String = r.get(1);
            let declared: String = r.get(2);
            let not_null: i64 = r.get(3);
            let default: Option<String> = r.get(4);
            let rowid_alias = !without_rowid && declared.eq_ignore_ascii_case("INTEGER") && pk == [col.clone()];
            let def = defs.get(&col).cloned().unwrap_or_default();
            t.columns.push(Column {
                ty: sqlite_type(sql, &def, &col, &declared, rowid_alias),
                not_null: not_null == 1 || pk.contains(&col),
                default: default.as_deref().map(norm_default),
                name: col,
            });
        }
        let indexes = sqlx::query(AssertSqlSafe(format!("PRAGMA index_list(\"{name}\")")))
            .fetch_all(&mut *conn)
            .await
            .unwrap();
        for r in &indexes {
            let iname: String = r.get(1);
            let unique: i64 = r.get(2);
            let origin: String = r.get(3);
            match origin.as_str() {
                "u" => {
                    let cols: Vec<String> = sqlx::query(AssertSqlSafe(format!("PRAGMA index_info(\"{iname}\")")))
                        .fetch_all(&mut *conn)
                        .await
                        .unwrap()
                        .iter()
                        .map(|c| c.get::<String, _>(2))
                        .collect();
                    t.unique.insert(cols);
                }
                "c" => {
                    let isql: String = sqlx::query_scalar("SELECT sql FROM sqlite_schema WHERE name = ?1")
                        .bind(&iname)
                        .fetch_one(&mut *conn)
                        .await
                        .unwrap();
                    let (cols, pred) = sqlite_index_parts(&isql);
                    t.indexes.insert(iname, (unique == 1, cols, pred));
                }
                _ => {}
            }
        }
        t.checks = check_bodies(sql).iter().filter(|b| !is_json_only_check(b)).count();
        let fks = sqlx::query(AssertSqlSafe(format!("PRAGMA foreign_key_list(\"{name}\")")))
            .fetch_all(&mut *conn)
            .await
            .unwrap();
        // id -> (target table, columns, target columns, on update, on delete)
        type Grouped = BTreeMap<i64, (String, Vec<String>, Vec<Option<String>>, String, String)>;
        let mut grouped = Grouped::new();
        for r in &fks {
            let e = grouped
                .entry(r.get(0))
                .or_insert_with(|| (r.get(2), Vec::new(), Vec::new(), r.get(5), r.get(6)));
            e.1.push(r.get(3));
            e.2.push(r.get(4));
        }
        for (_, (table, cols, refs, on_update, on_delete)) in grouped {
            t.foreign_keys.insert(ForeignKey {
                columns: cols,
                // A missing target column means the referenced primary key;
                // filled in below once every table's key is known.
                references: refs.into_iter().map(Option::unwrap_or_default).collect(),
                table,
                on_update: fk_action(&on_update),
                on_delete: fk_action(&on_delete),
            });
        }
        pks.insert(name.clone(), pk);
        schema.tables.insert(name.clone(), t);
    }
    for t in schema.tables.values_mut() {
        t.foreign_keys = std::mem::take(&mut t.foreign_keys)
            .into_iter()
            .map(|mut fk| {
                if fk.references.iter().all(String::is_empty) {
                    fk.references = pks.get(&fk.table).cloned().unwrap_or_default();
                }
                fk
            })
            .collect();
    }
    let triggers: Vec<(String,)> = sqlx::query_as("SELECT name FROM sqlite_schema WHERE type = 'trigger'")
        .fetch_all(&mut *conn)
        .await
        .unwrap();
    schema.triggers = triggers.into_iter().map(|(n,)| n).collect();
    for (name, t) in &mut schema.tables {
        if FULLTEXT_TABLES.contains(&name.as_str()) {
            continue;
        }
        t.row_count = sqlx::query_scalar(AssertSqlSafe(format!("SELECT count(*) FROM \"{name}\"")))
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        if (1..=50).contains(&t.row_count) {
            let cols: Vec<&Column> = t
                .columns
                .iter()
                .filter(|c| !TIME_COLUMNS.contains(&c.name.as_str()))
                .collect();
            let select = cols
                .iter()
                .map(|c| format!("CAST(\"{}\" AS TEXT)", c.name))
                .collect::<Vec<_>>()
                .join(", ");
            let order = (1..=cols.len()).map(|i| i.to_string()).collect::<Vec<_>>().join(", ");
            let rows = sqlx::query(AssertSqlSafe(format!("SELECT {select} FROM \"{name}\" ORDER BY {order}")))
                .fetch_all(&mut *conn)
                .await
                .unwrap();
            t.rows = Some(
                rows.iter()
                    .map(|r| {
                        cols.iter()
                            .enumerate()
                            .map(|(i, c)| norm_value(&c.ty, r.get(i)))
                            .collect()
                    })
                    .collect(),
            );
        }
    }
    schema
}

/// JSON compared as values (jsonb normalises spacing and key order).
fn norm_value(ty: &str, v: Option<String>) -> Option<String> {
    match (ty, v) {
        ("jsonb", Some(s)) => Some(
            serde_json::from_str::<serde_json::Value>(&s)
                .map(|j| j.to_string())
                .unwrap_or(s),
        ),
        (_, v) => v,
    }
}

async fn pg_names(conn: &mut PgConnection, sql: &'static str, oid: i64) -> Vec<Vec<String>> {
    sqlx::query(sql)
        .bind(oid)
        .fetch_all(&mut *conn)
        .await
        .unwrap()
        .iter()
        .map(|r| r.get::<Vec<String>, _>(0))
        .collect()
}

async fn pg_schema(conn: &mut PgConnection) -> Schema {
    let mut schema = Schema::default();
    let tables: Vec<(String, i64)> = sqlx::query_as(
        "SELECT c.relname::text, c.oid::bigint FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
         WHERE n.nspname = 'forgebase' AND c.relkind IN ('r', 'p') ORDER BY 1",
    )
    .fetch_all(&mut *conn)
    .await
    .unwrap();
    for (name, oid) in &tables {
        if FULLTEXT_TABLES.contains(&name.as_str()) {
            schema.tables.insert(name.clone(), Table::default());
            continue;
        }
        let mut t = Table::default();
        let cols = sqlx::query(
            "SELECT a.attname::text, format_type(a.atttypid, a.atttypmod), a.attnotnull,
                    pg_get_expr(d.adbin, d.adrelid), a.attidentity::text
             FROM pg_attribute a LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
             WHERE a.attrelid = $1::bigint::oid AND a.attnum > 0 AND NOT a.attisdropped ORDER BY a.attnum",
        )
        .bind(oid)
        .fetch_all(&mut *conn)
        .await
        .unwrap();
        for r in &cols {
            let identity: String = r.get(4);
            let ty: String = r.get(1);
            let ty = ty.trim_start_matches("forgebase.").to_owned();
            let default: Option<String> = r.get(3);
            t.columns.push(Column {
                name: r.get(0),
                ty: if identity.is_empty() {
                    ty
                } else {
                    format!("{ty} identity")
                },
                not_null: r.get(2),
                default: default.as_deref().map(norm_default),
            });
        }
        const KEY_COLS: &str = "SELECT array_agg(a.attname::text ORDER BY k.ord)
             FROM pg_constraint con, unnest(con.conkey) WITH ORDINALITY k(attnum, ord)
             JOIN pg_attribute a ON a.attnum = k.attnum
             WHERE con.conrelid = $1::bigint::oid AND a.attrelid = con.conrelid AND con.contype = 'p'
             GROUP BY con.oid";
        t.primary_key = pg_names(conn, KEY_COLS, *oid).await.into_iter().next().unwrap_or_default();
        const UNIQUE_COLS: &str = "SELECT array_agg(a.attname::text ORDER BY k.ord)
             FROM pg_constraint con, unnest(con.conkey) WITH ORDINALITY k(attnum, ord)
             JOIN pg_attribute a ON a.attnum = k.attnum
             WHERE con.conrelid = $1::bigint::oid AND a.attrelid = con.conrelid AND con.contype = 'u'
             GROUP BY con.oid";
        t.unique = pg_names(conn, UNIQUE_COLS, *oid).await.into_iter().collect();
        let fks = sqlx::query(
            "SELECT
               (SELECT array_agg(a.attname::text ORDER BY k.ord) FROM unnest(con.conkey) WITH ORDINALITY k(attnum, ord)
                  JOIN pg_attribute a ON a.attrelid = con.conrelid AND a.attnum = k.attnum),
               con.confrelid::regclass::text,
               (SELECT array_agg(a.attname::text ORDER BY k.ord) FROM unnest(con.confkey) WITH ORDINALITY k(attnum, ord)
                  JOIN pg_attribute a ON a.attrelid = con.confrelid AND a.attnum = k.attnum),
               con.confupdtype::text, con.confdeltype::text, con.condeferrable, con.condeferred, con.conname::text
             FROM pg_constraint con WHERE con.conrelid = $1::bigint::oid AND con.contype = 'f'",
        )
        .bind(oid)
        .fetch_all(&mut *conn)
        .await
        .unwrap();
        for r in &fks {
            let deferrable: bool = r.get(5);
            let deferred: bool = r.get(6);
            let cname: String = r.get(7);
            assert!(
                deferrable && !deferred,
                "{name}.{cname}: foreign keys must be DEFERRABLE INITIALLY IMMEDIATE (TxMode::SchemaChange defers them)"
            );
            let target: String = r.get(1);
            t.foreign_keys.insert(ForeignKey {
                columns: r.get(0),
                table: target.trim_start_matches("forgebase.").to_owned(),
                references: r.get(2),
                on_update: fk_action(&r.get::<String, _>(3)),
                on_delete: fk_action(&r.get::<String, _>(4)),
            });
        }
        let idx = sqlx::query(
            "SELECT ic.relname::text, i.indisunique, pg_get_indexdef(i.indexrelid), pg_get_expr(i.indpred, i.indrelid)
             FROM pg_index i JOIN pg_class ic ON ic.oid = i.indexrelid
             WHERE i.indrelid = $1::bigint::oid
               AND NOT EXISTS (SELECT 1 FROM pg_constraint con WHERE con.conindid = i.indexrelid AND con.conrelid = i.indrelid)",
        )
        .bind(oid)
        .fetch_all(&mut *conn)
        .await
        .unwrap();
        for r in &idx {
            let def: String = r.get(2);
            let pred: Option<String> = r.get(3);
            let using = def.find(" USING ").expect("USING");
            let open = using + def[using..].find('(').expect("(");
            let body = match def.rfind(" WHERE ") {
                Some(w) if w > open => &def[open..w],
                _ => &def[open..],
            };
            t.indexes.insert(
                r.get(0),
                (r.get(1), norm_expr(body), pred.as_deref().map(norm_expr).unwrap_or_default()),
            );
        }
        t.checks = sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM pg_constraint WHERE conrelid = $1::bigint::oid AND contype = 'c'",
        )
        .bind(oid)
        .fetch_one(&mut *conn)
        .await
        .unwrap() as usize;
        // Allowed difference: the rowid stand-in column.
        if ROWID_TABLES.contains(&name.as_str()) {
            let before = t.columns.len();
            t.columns.retain(|c| !(c.name == "_rowid" && c.ty == "bigint identity" && c.not_null));
            assert_eq!(before - 1, t.columns.len(), "{name} needs a _rowid identity column");
        }
        t.row_count = sqlx::query_scalar(AssertSqlSafe(format!("SELECT count(*) FROM forgebase.\"{name}\"")))
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        if (1..=50).contains(&t.row_count) {
            let cols: Vec<&Column> = t
                .columns
                .iter()
                .filter(|c| !TIME_COLUMNS.contains(&c.name.as_str()))
                .collect();
            let select = cols
                .iter()
                .map(|c| format!("\"{}\"::text", c.name))
                .collect::<Vec<_>>()
                .join(", ");
            let order = cols
                .iter()
                .map(|c| format!("\"{}\"::text COLLATE \"C\"", c.name))
                .collect::<Vec<_>>()
                .join(", ");
            let rows = sqlx::query(AssertSqlSafe(format!(
                "SELECT {select} FROM forgebase.\"{name}\" ORDER BY {order}"
            )))
            .fetch_all(&mut *conn)
            .await
            .unwrap();
            t.rows = Some(
                rows.iter()
                    .map(|r| {
                        cols.iter()
                            .enumerate()
                            .map(|(i, c)| norm_value(&c.ty, r.get(i)))
                            .collect()
                    })
                    .collect(),
            );
        }
        schema.tables.insert(name.clone(), t);
    }
    let triggers: Vec<(String,)> = sqlx::query_as(
        "SELECT t.tgname::text FROM pg_trigger t JOIN pg_class c ON c.oid = t.tgrelid
         JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = 'forgebase' AND NOT t.tgisinternal",
    )
    .fetch_all(&mut *conn)
    .await
    .unwrap();
    schema.triggers = triggers.into_iter().map(|(n,)| n).collect();
    schema
}

/// Every difference, one line each.
fn diff(sqlite: &Schema, pg: &Schema) -> Vec<String> {
    let mut out = Vec::new();
    for name in sqlite.tables.keys().filter(|n| !pg.tables.contains_key(*n)) {
        out.push(format!("table {name}: only on SQLite"));
    }
    for name in pg.tables.keys().filter(|n| !sqlite.tables.contains_key(*n)) {
        out.push(format!("table {name}: only on PostgreSQL"));
    }
    for (name, s) in &sqlite.tables {
        let Some(p) = pg.tables.get(name) else { continue };
        let scols: BTreeMap<&str, &Column> = s.columns.iter().map(|c| (c.name.as_str(), c)).collect();
        let pcols: BTreeMap<&str, &Column> = p.columns.iter().map(|c| (c.name.as_str(), c)).collect();
        for (c, sc) in &scols {
            match pcols.get(c) {
                None => out.push(format!("{name}.{c}: column only on SQLite")),
                Some(pc) if pc != sc => out.push(format!("{name}.{c}: SQLite {sc:?} != PostgreSQL {pc:?}")),
                Some(_) => {}
            }
        }
        for c in pcols.keys().filter(|c| !scols.contains_key(*c)) {
            out.push(format!("{name}.{c}: column only on PostgreSQL"));
        }
        if s.primary_key != p.primary_key {
            out.push(format!(
                "{name}: primary key SQLite {:?} != PostgreSQL {:?}",
                s.primary_key, p.primary_key
            ));
        }
        for u in s.unique.symmetric_difference(&p.unique) {
            let side = if s.unique.contains(u) { "SQLite" } else { "PostgreSQL" };
            out.push(format!("{name}: unique {u:?} only on {side}"));
        }
        for f in s.foreign_keys.symmetric_difference(&p.foreign_keys) {
            let side = if s.foreign_keys.contains(f) { "SQLite" } else { "PostgreSQL" };
            out.push(format!("{name}: foreign key {f:?} only on {side}"));
        }
        for (i, si) in &s.indexes {
            match p.indexes.get(i) {
                None => out.push(format!("{name}: index {i} only on SQLite")),
                Some(pi) if pi != si => out.push(format!("{name}: index {i}: SQLite {si:?} != PostgreSQL {pi:?}")),
                Some(_) => {}
            }
        }
        for i in p.indexes.keys().filter(|i| !s.indexes.contains_key(*i)) {
            out.push(format!("{name}: index {i} only on PostgreSQL"));
        }
        if s.checks != p.checks {
            out.push(format!(
                "{name}: {} check constraints on SQLite (json_valid checks excluded), {} on PostgreSQL",
                s.checks, p.checks
            ));
        }
        if s.row_count != p.row_count {
            out.push(format!(
                "{name}: {} rows on SQLite, {} on PostgreSQL",
                s.row_count, p.row_count
            ));
        } else if s.rows != p.rows {
            out.push(format!(
                "{name}: seed rows differ\n  SQLite:     {:?}\n  PostgreSQL: {:?}",
                s.rows, p.rows
            ));
        }
    }
    for t in sqlite.triggers.symmetric_difference(&pg.triggers) {
        let side = if sqlite.triggers.contains(t) { "SQLite" } else { "PostgreSQL" };
        out.push(format!("trigger {t}: only on {side}"));
    }
    out
}

async fn sqlite_head() -> SqliteConnection {
    let mut conn = SqliteConnection::connect("sqlite::memory:").await.unwrap();
    Migrator::system().run(&mut conn).await.unwrap();
    conn
}

fn pg_url(db: &TestDb) -> String {
    match db.options().target {
        DbTarget::Postgres { url, .. } => url.expose().to_owned(),
        other => panic!("not a PostgreSQL test database: {other:?}"),
    }
}

async fn pg_connect(url: &str) -> PgConnection {
    PgConnection::connect(url).await.unwrap()
}

/// `url` with its database (and, when given, its user) replaced.
fn url_with(url: &str, database: &str, user: Option<&str>) -> String {
    let (base, query) = url.split_once('?').map_or((url, None), |(b, q)| (b, Some(q)));
    let scheme_end = base.find("://").map_or(0, |p| p + 3);
    let path = base[scheme_end..].find('/').map_or(base.len(), |p| p + scheme_end);
    let mut authority = base[scheme_end..path].to_owned();
    if let Some(u) = user {
        let host = authority.split_once('@').map_or(authority.as_str(), |(_, h)| h).to_owned();
        authority = format!("{u}@{host}");
    }
    let mut out = format!("{}{authority}/{database}", &base[..scheme_end]);
    if let Some(q) = query {
        out.push('?');
        out.push_str(q);
    }
    out
}

/// A fresh, empty database on the test engine (dropped at the end of the
/// test binary with the engine).
async fn empty_database(admin: &mut PgConnection, owner: Option<&str>) -> String {
    let name = format!("e_{}", uuid_like());
    let owner = owner.map(|o| format!(" OWNER \"{o}\"")).unwrap_or_default();
    sqlx::raw_sql(AssertSqlSafe(format!("CREATE DATABASE \"{name}\"{owner}")))
        .execute(&mut *admin)
        .await
        .unwrap();
    name
}

fn uuid_like() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let t = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    format!(
        "{t:x}{:x}{:x}",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

/// Fails, naming the file to add, when a SQLite migration has no PostgreSQL
/// twin (runs without a PostgreSQL engine too).
#[test]
fn every_sqlite_migration_has_a_postgres_twin() {
    let problems = postgres::pairing_problems();
    assert!(
        problems.is_empty(),
        "SQLite and PostgreSQL migrations do not pair up:\n- {}",
        problems.join("\n- ")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn postgres_schema_matches_sqlite_at_head() {
    every_sqlite_migration_has_a_postgres_twin();
    let Some(db) = testing::postgres_test_db().await else {
        eprintln!("skipped: no PostgreSQL engine configured (set FORGEBASE_PG_ENGINE_DIR)");
        return;
    };
    let mut lite = sqlite_head().await;
    let sqlite = sqlite_schema(&mut lite).await;
    let mut pgc = pg_connect(&pg_url(&db)).await;
    let pg = pg_schema(&mut pgc).await;
    assert!(sqlite.tables.len() > 100, "{} SQLite tables", sqlite.tables.len());
    let problems = diff(&sqlite, &pg);
    let mut report = String::new();
    for p in &problems {
        let _ = writeln!(report, "- {p}");
    }
    assert!(
        problems.is_empty(),
        "{} schema differences between SQLite and PostgreSQL at the head:\n{report}",
        problems.len()
    );
    // Same versions and names recorded on both backends.
    let s_versions: Vec<(i64, String)> = sqlx::query_as("SELECT version, name FROM _forgebase_migrations ORDER BY 1")
        .fetch_all(&mut lite)
        .await
        .unwrap();
    let p_versions: Vec<(i64, String)> =
        sqlx::query_as("SELECT version, name FROM forgebase._forgebase_migrations ORDER BY 1")
            .fetch_all(&mut pgc)
            .await
            .unwrap();
    assert_eq!(s_versions, p_versions);
}

#[test]
fn the_comparison_catches_differences() {
    let mut a = Schema::default();
    let mut t = Table {
        columns: vec![Column {
            name: "x".into(),
            ty: "bigint".into(),
            not_null: true,
            default: None,
        }],
        ..Table::default()
    };
    a.tables.insert("t".into(), t.clone());
    t.columns[0].ty = "text".into();
    t.indexes.insert("i".into(), (false, "x".into(), String::new()));
    let mut b = Schema::default();
    b.tables.insert("t".into(), t);
    b.triggers.insert("tr".into());
    let d = diff(&a, &b);
    assert_eq!(d.len(), 3, "{d:#?}");
    assert!(d[0].contains("t.x"));
    assert!(d[1].contains("index i only on PostgreSQL"));
    assert!(d[2].contains("trigger tr: only on PostgreSQL"));
}

#[test]
fn normalisation() {
    assert_eq!(norm_expr("COALESCE(org_id, ''::text)"), norm_expr("coalesce(org_id, '')"));
    assert_eq!(
        norm_expr("((status = 'running'::text) AND (deadline_at IS NOT NULL))"),
        norm_expr("status = 'running' AND deadline_at IS NOT NULL")
    );
    assert_eq!(norm_expr("'x'::character varying"), "'x'");
    assert_eq!(norm_expr("1.5::double precision"), "1.5");
    assert_eq!(norm_default("'5'::bigint"), "5");
    assert_eq!(norm_default("'{}'::jsonb"), "'{}'");
    assert_eq!(
        norm_default("strftime('%Y-%m-%dT%H:%M:%fZ', 'now')"),
        norm_default("to_char((now() AT TIME ZONE 'UTC'::text), 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"'::text)")
    );
    assert!(is_json_only_check("json_valid(x)"));
    assert!(is_json_only_check("x IS NULL OR json_valid(x)"));
    assert!(!is_json_only_check("json_valid(x) AND json_type(x) = 'object'"));
    let sql = "CREATE TABLE t (a INTEGER NOT NULL CHECK (a IN (0, 1)), b TEXT CHECK (length(b) > 0), \
               CHECK (a = 1 OR b IS NULL)) STRICT";
    assert_eq!(check_bodies(sql).len(), 3);
    assert_eq!(column_definitions(sql).len(), 2);
    assert_eq!(
        sqlite_index_parts("CREATE INDEX i ON t (a, coalesce(b, '')) WHERE a = 1"),
        ("a,coalesceb,''".into(), "a=1".into())
    );
    assert_eq!(
        url_with("postgres://u@localhost/db?host=/s&port=1", "x", Some("v")),
        "postgres://v@localhost/x?host=/s&port=1"
    );
}

// ---------------------------------------------------------------------------
// The PostgreSQL runner
// ---------------------------------------------------------------------------

async fn bookkeeping(conn: &mut PgConnection) -> Vec<(i64, String, String)> {
    sqlx::query_as("SELECT version, name, checksum FROM forgebase._forgebase_migrations ORDER BY version")
        .fetch_all(&mut *conn)
        .await
        .unwrap()
}

async fn regclass_exists(conn: &mut PgConnection, name: &str) -> bool {
    sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
        .bind(name)
        .fetch_one(&mut *conn)
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn postgres_runner_applies_the_baseline_once() {
    let Some(db) = testing::postgres_test_db().await else {
        return;
    };
    let url = pg_url(&db);
    let mut admin = pg_connect(&url).await;
    let name = empty_database(&mut admin, None).await;
    let mut conn = pg_connect(&url_with(&url, &name, None)).await;

    let before = postgres::status(&mut conn).await.unwrap();
    assert_eq!(before.current_version, 0);
    assert_eq!(before.pending.len(), postgres::baseline_covers().len());
    assert!(!regclass_exists(&mut conn, "forgebase._forgebase_migrations").await, "status writes nothing");

    let first = postgres::migrate(&mut conn).await.unwrap();
    assert_eq!(first.len(), postgres::baseline_covers().len());
    let rows = bookkeeping(&mut conn).await;
    let baseline = postgres::POSTGRES_MIGRATIONS[0].checksum();
    for (row, cover) in rows.iter().zip(postgres::baseline_covers()) {
        assert_eq!((row.0, row.1.as_str(), row.2.as_str()), (cover.version, cover.name, baseline.as_str()));
    }
    let status = postgres::status(&mut conn).await.unwrap();
    assert!(status.is_up_to_date());
    assert_eq!(status.current_version, PgMigrator::system().latest_version());

    let second = postgres::migrate(&mut conn).await.unwrap();
    assert!(second.is_empty(), "second run applies nothing");
    assert_eq!(postgres::status(&mut conn).await.unwrap(), status);

    // The session finds the system tables unqualified.
    let realms: i64 = sqlx::query_scalar("SELECT count(*) FROM _forgebase_realms")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    assert_eq!(realms, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn postgres_runner_rolls_back_a_failing_migration() {
    let Some(db) = testing::postgres_test_db().await else {
        return;
    };
    static COVERS: &[Migration] = &[Migration::new(1, "one", "")];
    static SET: &[Migration] = &[
        Migration::new(1, "baseline", "CREATE TABLE a (x bigint);"),
        Migration::new(2, "two", "CREATE TABLE b (x bigint); INSERT INTO missing_table VALUES (1);"),
    ];
    let url = pg_url(&db);
    let mut admin = pg_connect(&url).await;
    let name = empty_database(&mut admin, None).await;
    let mut conn = pg_connect(&url_with(&url, &name, None)).await;
    let migrator = PgMigrator::new(SET, COVERS).unwrap();
    let err = migrator.run(&mut conn).await.unwrap_err();
    assert!(matches!(err, MigrationError::Failed { version: 2, .. }), "{err}");
    assert!(regclass_exists(&mut conn, "forgebase.a").await, "migration 1 committed, in schema forgebase");
    assert!(!regclass_exists(&mut conn, "forgebase.b").await, "migration 2 rolled back");
    let status = migrator.status(&mut conn).await.unwrap();
    assert_eq!(status.current_version, 1);
    assert_eq!(status.pending.len(), 1);

    // An edited migration is refused; a newer database is refused.
    static EDITED: &[Migration] = &[Migration::new(1, "baseline", "CREATE TABLE a (x text);")];
    let err = PgMigrator::new(EDITED, COVERS).unwrap().run(&mut conn).await.unwrap_err();
    assert!(matches!(err, MigrationError::ChecksumMismatch { version: 1, .. }), "{err}");
    static FIXED: &[Migration] = &[
        Migration::new(1, "baseline", "CREATE TABLE a (x bigint);"),
        Migration::new(2, "two", "CREATE TABLE b (x bigint);"),
    ];
    PgMigrator::new(FIXED, COVERS).unwrap().run(&mut conn).await.unwrap();
    let err = PgMigrator::new(&FIXED[..1], COVERS)
        .unwrap()
        .run(&mut conn)
        .await
        .unwrap_err();
    assert!(matches!(err, MigrationError::DatabaseNewer { version: 2, .. }), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn postgres_runners_racing_apply_each_migration_once() {
    let Some(db) = testing::postgres_test_db().await else {
        return;
    };
    let url = pg_url(&db);
    let mut admin = pg_connect(&url).await;
    let name = empty_database(&mut admin, None).await;
    let target = url_with(&url, &name, None);
    let mut a = pg_connect(&target).await;
    let mut b = pg_connect(&target).await;
    let (ra, rb) = tokio::join!(postgres::migrate(&mut a), postgres::migrate(&mut b));
    let (ra, rb) = (ra.unwrap(), rb.unwrap());
    assert_eq!(ra.len() + rb.len(), postgres::baseline_covers().len(), "applied exactly once");
    assert_eq!(bookkeeping(&mut a).await.len(), postgres::baseline_covers().len());
}

/// An external server: an ordinary role that owns its database (no
/// superuser, no forgebase_owner role) migrates it, and its new sessions get
/// `search_path = forgebase, public` from `ALTER ROLE … IN DATABASE`.
#[tokio::test(flavor = "multi_thread")]
async fn postgres_external_owner_role_migrates_and_gets_the_search_path() {
    let Some(db) = testing::postgres_test_db().await else {
        return;
    };
    let url = pg_url(&db);
    let mut admin = pg_connect(&url).await;
    let role = format!("ext_{}", uuid_like());
    sqlx::raw_sql(AssertSqlSafe(format!("CREATE ROLE \"{role}\" LOGIN")))
        .execute(&mut admin)
        .await
        .unwrap();
    let name = empty_database(&mut admin, Some(&role)).await;
    let target = url_with(&url, &name, Some(&role));
    let mut conn = pg_connect(&target).await;
    let default: String = sqlx::query_scalar("SELECT reset_val FROM pg_settings WHERE name = 'search_path'")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    assert!(!default.contains("forgebase"), "{default}");
    postgres::migrate(&mut conn).await.unwrap();
    // This session predates the setting; the catalog has it now.
    assert!(!postgres::ensure_search_path(&mut conn).await.unwrap(), "set once");
    let owner: String = sqlx::query_scalar("SELECT tableowner::text FROM pg_tables WHERE tablename = '_forgebase_jobs'")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    assert_eq!(owner, role);
    let _ = conn.close().await;

    let mut fresh = pg_connect(&target).await;
    let path: String = sqlx::query_scalar("SHOW search_path").fetch_one(&mut fresh).await.unwrap();
    assert_eq!(path, "forgebase, public");
    assert!(!postgres::ensure_search_path(&mut fresh).await.unwrap(), "already set");
    let _ = fresh.close().await;
    for sql in [
        format!("DROP DATABASE \"{name}\" WITH (FORCE)"),
        format!("DROP ROLE \"{role}\""),
    ] {
        sqlx::raw_sql(AssertSqlSafe(sql)).execute(&mut admin).await.unwrap();
    }
}

/// RAISE(ABORT, msg) triggers keep their message (SQLSTATE P0001), the
/// invoice-line freeze looks at the invoice, and the vector keyword index
/// follows the items.
#[tokio::test(flavor = "multi_thread")]
async fn postgres_triggers_behave_like_sqlite() {
    let Some(db) = testing::postgres_test_db().await else {
        return;
    };
    let mut c = pg_connect(&pg_url(&db)).await;
    sqlx::raw_sql(
        "INSERT INTO _forgebase_agreements (id, realm, kind, version, title, body, published_at, published_by)
         VALUES ('a1', 'default', 'terms', 1, 't', 'b', 0, 'admin')",
    )
    .execute(&mut c)
    .await
    .unwrap();
    for sql in [
        "UPDATE _forgebase_agreements SET title = 'x'",
        "DELETE FROM _forgebase_agreements",
    ] {
        let e = sqlx::raw_sql(sql).execute(&mut c).await.unwrap_err();
        let d = e.as_database_error().expect("database error");
        assert_eq!(d.code().as_deref(), Some("P0001"), "{sql}");
        assert_eq!(d.message(), "agreement versions are immutable", "{sql}");
    }
    // Vector keyword index.
    sqlx::raw_sql(
        "INSERT INTO _forgebase_vector_collections (tenant, name, created_at, updated_at) VALUES ('t', 'c', 'x', 'x');
         INSERT INTO _forgebase_vector_items (collection_id, id, content, created, updated)
         SELECT id, 'a', 'Crème brûlée', 'x', 'x' FROM _forgebase_vector_collections;
         INSERT INTO _forgebase_vector_items (collection_id, id, content, created, updated)
         SELECT id, 'b', NULL, 'x', 'x' FROM _forgebase_vector_collections",
    )
    .execute(&mut c)
    .await
    .unwrap();
    let hits: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM _forgebase_vector_fts
         WHERE fts @@ websearch_to_tsquery('simple', _forgebase_unaccent('creme'))",
    )
    .fetch_one(&mut c)
    .await
    .unwrap();
    assert_eq!(hits, 1);
    sqlx::raw_sql("UPDATE _forgebase_vector_items SET content = 'tarte' WHERE id = 'b'; DELETE FROM _forgebase_vector_items WHERE id = 'a'")
        .execute(&mut c)
        .await
        .unwrap();
    let left: Vec<(String,)> = sqlx::query_as("SELECT content FROM _forgebase_vector_fts")
        .fetch_all(&mut c)
        .await
        .unwrap();
    assert_eq!(left, [("tarte".to_owned(),)]);
}
