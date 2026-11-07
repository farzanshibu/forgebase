//! SQL dialects.
//!
//! Everything that differs between SQL engines and that code above
//! `forgebase-db` has to emit goes through [`Dialect`]: identifier quoting,
//! placeholders, JSON access, column types, `LIKE` escaping and literal
//! rendering for index definitions. A PostgreSQL backend adds a second
//! implementation; callers do not change (ADR-0003 §2).

use std::fmt;

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
}
