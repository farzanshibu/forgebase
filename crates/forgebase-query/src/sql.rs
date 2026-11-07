//! Lexical analysis of raw SQL for the SQL editor (ADR-0003 §6).
//!
//! This is a tokenizer, not a parser: it splits statements, finds the leading
//! keyword and spots names that must never be touched. The engine has the
//! final word on read vs write (`Backend::statement_writes`), and reads run on
//! connections that cannot write.

use crate::QueryError;

/// Maximum SQL text accepted by the editor (1 MiB).
pub const MAX_SQL_LEN: usize = 1 << 20;

/// Token kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    /// Keyword or bare identifier.
    Word,
    /// `"ident"`, `` `ident` `` or `[ident]`.
    QuotedIdent,
    /// `'string'`.
    String,
    /// Numeric literal.
    Number,
    /// `?`, `?NNN`, `:name`, `@name`, `$name`.
    Param,
    /// Any other character (`;`, `(`, `=`, ...).
    Punct,
}

/// A token and its text (quotes removed for quoted identifiers).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// Kind.
    pub kind: TokenKind,
    /// Text.
    pub text: String,
}

impl Token {
    fn is_word(&self, w: &str) -> bool {
        self.kind == TokenKind::Word && self.text.eq_ignore_ascii_case(w)
    }
}

/// Tokenizes SQL, skipping whitespace and comments.
pub fn tokenize(sql: &str) -> Result<Vec<Token>, QueryError> {
    tokenize_spans(sql).map(|v| v.into_iter().map(|(t, _)| t).collect())
}

fn tokenize_spans(sql: &str) -> Result<Vec<(Token, std::ops::Range<usize>)>, QueryError> {
    if sql.len() > MAX_SQL_LEN {
        return Err(QueryError::TooComplex("SQL text is larger than 1 MiB".into()));
    }
    let b = sql.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        let start = i;
        let tok = |kind, text: &str| Token {
            kind,
            text: text.to_owned(),
        };
        match c {
            c if c.is_ascii_whitespace() => {
                i += 1;
                continue;
            }
            b'-' if b.get(i + 1) == Some(&b'-') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                match sql[i + 2..].find("*/") {
                    Some(end) => i = i + 2 + end + 2,
                    None => i = b.len(), // SQLite accepts an unterminated trailing comment
                }
                continue;
            }
            b'\'' | b'"' | b'`' | b'[' => {
                let close = if c == b'[' { b']' } else { c };
                let mut j = i + 1;
                let mut text = Vec::new();
                loop {
                    match b.get(j) {
                        None => return Err(QueryError::syntax(start, "unterminated quoted text")),
                        Some(&x) if x == close => {
                            if close != b']' && b.get(j + 1) == Some(&close) {
                                text.push(x);
                                j += 2;
                            } else {
                                j += 1;
                                break;
                            }
                        }
                        Some(&x) => {
                            text.push(x);
                            j += 1;
                        }
                    }
                }
                let text = String::from_utf8_lossy(&text).into_owned();
                let kind = if c == b'\'' {
                    TokenKind::String
                } else {
                    TokenKind::QuotedIdent
                };
                out.push((Token { kind, text }, start..j));
                i = j;
            }
            b'?' | b':' | b'@' | b'$' => {
                let mut j = i + 1;
                while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                    j += 1;
                }
                if c != b'?' && j == i + 1 {
                    out.push((tok(TokenKind::Punct, &sql[i..j]), start..j));
                } else {
                    out.push((tok(TokenKind::Param, &sql[i..j]), start..j));
                }
                i = j;
            }
            c if c.is_ascii_digit() || (c == b'.' && b.get(i + 1).is_some_and(u8::is_ascii_digit)) => {
                let mut j = i;
                while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'.') {
                    j += 1;
                }
                out.push((tok(TokenKind::Number, &sql[i..j]), start..j));
                i = j;
            }
            c if c.is_ascii_alphabetic() || c == b'_' || c >= 0x80 => {
                let mut j = i;
                while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_' || b[j] == b'$' || b[j] >= 0x80) {
                    j += 1;
                }
                out.push((tok(TokenKind::Word, &sql[i..j]), start..j));
                i = j;
            }
            _ => {
                let ch_len = sql[i..].chars().next().map_or(1, char::len_utf8);
                out.push((tok(TokenKind::Punct, &sql[i..i + ch_len]), start..i + ch_len));
                i += ch_len;
            }
        }
    }
    Ok(out)
}

/// Splits SQL text into statements (without the trailing `;`). Semicolons
/// inside strings, comments and `CREATE TRIGGER ... BEGIN ... END` bodies do
/// not split. Empty statements are dropped.
pub fn split_statements(sql: &str) -> Result<Vec<String>, QueryError> {
    let tokens = tokenize_spans(sql)?;
    let mut out = Vec::new();
    let mut stmt_start: Option<usize> = None;
    let mut stmt_tokens: Vec<&Token> = Vec::new();
    let mut depth = 0i32; // BEGIN/CASE ... END nesting inside a trigger body
    for (tok, span) in &tokens {
        if tok.kind == TokenKind::Punct && tok.text == ";" && depth == 0 {
            if let Some(s) = stmt_start.take() {
                out.push(sql[s..span.start].trim().to_owned());
            }
            stmt_tokens.clear();
            continue;
        }
        if stmt_start.is_none() {
            stmt_start = Some(span.start);
        }
        stmt_tokens.push(tok);
        if is_create_trigger(&stmt_tokens) {
            if tok.is_word("BEGIN") || (depth > 0 && tok.is_word("CASE")) {
                depth += 1;
            } else if tok.is_word("END") && depth > 0 {
                depth -= 1;
            }
        }
    }
    if depth != 0 {
        return Err(QueryError::syntax(sql.len(), "unterminated trigger body"));
    }
    if let Some(s) = stmt_start {
        let tail = sql[s..].trim();
        if !tail.is_empty() {
            out.push(tail.to_owned());
        }
    }
    Ok(out)
}

fn is_create_trigger(t: &[&Token]) -> bool {
    let words: Vec<&&Token> = t.iter().take(4).collect();
    words.first().is_some_and(|w| w.is_word("CREATE")) && words.iter().skip(1).take(2).any(|w| w.is_word("TRIGGER"))
}

/// What a statement does, by its leading keyword.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StatementKind {
    /// `SELECT`, `VALUES`, `WITH ...`, `EXPLAIN`, read-only `PRAGMA`.
    Read,
    /// `INSERT`, `UPDATE`, `DELETE`, `REPLACE`.
    Write,
    /// `CREATE`, `ALTER`, `DROP`.
    Schema,
    /// `ANALYZE`, `REINDEX`.
    Maintenance,
}

/// Lexical facts about one statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatementInfo {
    /// Kind by leading keyword (the engine may upgrade `Read` to `Write`).
    pub kind: StatementKind,
    /// Leading keyword, upper-case.
    pub keyword: String,
    /// The statement names a `_forgebase_*` system table.
    pub touches_system_tables: bool,
    /// Names mentioned (identifiers and words), lower-case, deduplicated.
    pub identifiers: Vec<String>,
}

/// Read-only pragmas the SQL editor may run (never with `=`).
const ALLOWED_PRAGMAS: &[&str] = &[
    "table_info",
    "table_xinfo",
    "table_list",
    "index_list",
    "index_info",
    "index_xinfo",
    "foreign_key_list",
    "foreign_key_check",
    "integrity_check",
    "quick_check",
    "compile_options",
    "function_list",
    "collation_list",
    "page_count",
    "page_size",
    "freelist_count",
    "user_version",
    "schema_version",
    "encoding",
    "journal_mode",
];

/// Virtual-table modules user DDL may use.
const ALLOWED_MODULES: &[&str] = &["fts5", "fts4", "fts3", "rtree", "rtree_i32"];

/// Functions that are never allowed.
const FORBIDDEN_FUNCTIONS: &[&str] = &["load_extension", "fts3_tokenizer", "readfile", "writefile", "edit"];

/// Classifies one statement and rejects what the SQL editor never runs:
/// transaction control (the service owns transactions), `ATTACH`/`DETACH`,
/// `VACUUM`, writable or unknown pragmas, temporary objects, unknown
/// virtual-table modules and dangerous functions.
pub fn analyze_statement(stmt: &str) -> Result<StatementInfo, QueryError> {
    let tokens = tokenize(stmt)?;
    let Some(first) = tokens.first() else {
        return Err(QueryError::NotAllowed("empty statement".into()));
    };
    if first.kind != TokenKind::Word {
        return Err(QueryError::NotAllowed("a statement must start with a keyword".into()));
    }
    let keyword = first.text.to_ascii_uppercase();
    let not_allowed = |why: &str| Err(QueryError::NotAllowed(why.to_owned()));
    let kind = match keyword.as_str() {
        "SELECT" | "VALUES" | "WITH" | "EXPLAIN" => StatementKind::Read,
        "INSERT" | "UPDATE" | "DELETE" | "REPLACE" => StatementKind::Write,
        "CREATE" | "ALTER" | "DROP" => StatementKind::Schema,
        "ANALYZE" | "REINDEX" => StatementKind::Maintenance,
        "PRAGMA" => {
            let name = tokens.get(1).map(|t| t.text.to_ascii_lowercase()).unwrap_or_default();
            let name = match tokens.get(2) {
                // PRAGMA main.table_info(...)
                Some(t) if t.text == "." => tokens.get(3).map(|t| t.text.to_ascii_lowercase()).unwrap_or_default(),
                _ => name,
            };
            if !ALLOWED_PRAGMAS.contains(&name.as_str()) || tokens.iter().any(|t| t.text == "=") {
                return not_allowed("only read-only introspection pragmas are allowed");
            }
            StatementKind::Read
        }
        "BEGIN" | "COMMIT" | "END" | "ROLLBACK" | "SAVEPOINT" | "RELEASE" => {
            return not_allowed("transaction control is managed by ForgeBase; send the statements as one batch");
        }
        "ATTACH" | "DETACH" => return not_allowed("attaching databases is not allowed"),
        "VACUUM" => return not_allowed("VACUUM is not allowed from the SQL editor"),
        other => return Err(QueryError::NotAllowed(format!("unsupported statement {other}"))),
    };
    if kind == StatementKind::Schema {
        if tokens
            .iter()
            .take(3)
            .any(|t| t.is_word("TEMP") || t.is_word("TEMPORARY"))
        {
            return not_allowed("temporary objects are not allowed");
        }
        if let Some(pos) = tokens.iter().position(|t| t.is_word("USING"))
            && tokens.iter().take(3).any(|t| t.is_word("VIRTUAL"))
        {
            let module = tokens
                .get(pos + 1)
                .map(|t| t.text.to_ascii_lowercase())
                .unwrap_or_default();
            if !ALLOWED_MODULES.contains(&module.as_str()) {
                return not_allowed("virtual table module not allowed");
            }
        }
    }
    let mut identifiers: Vec<String> = Vec::new();
    for (i, t) in tokens.iter().enumerate() {
        if matches!(t.kind, TokenKind::Word | TokenKind::QuotedIdent) {
            let lower = t.text.to_ascii_lowercase();
            if t.kind == TokenKind::Word
                && FORBIDDEN_FUNCTIONS.contains(&lower.as_str())
                && tokens.get(i + 1).is_some_and(|n| n.text == "(")
            {
                return Err(QueryError::NotAllowed(format!("function {lower} is not allowed")));
            }
            if !identifiers.contains(&lower) {
                identifiers.push(lower);
            }
        }
    }
    let touches_system_tables = identifiers.iter().any(|n| n.starts_with("_forgebase"));
    Ok(StatementInfo {
        kind,
        keyword,
        touches_system_tables,
        identifiers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_respecting_strings_comments_and_triggers() {
        let sql = "SELECT ';' -- ; comment\n; /* ; */ INSERT INTO t VALUES (1);;\
                   CREATE TRIGGER tr AFTER INSERT ON t BEGIN UPDATE t SET x = CASE WHEN 1 THEN 2 END; DELETE FROM u; END; SELECT 2";
        let parts = split_statements(sql).unwrap();
        assert_eq!(parts.len(), 4, "{parts:?}");
        assert_eq!(parts[0], "SELECT ';' -- ; comment");
        assert!(parts[2].starts_with("CREATE TRIGGER") && parts[2].ends_with("END"));
        assert_eq!(parts[3], "SELECT 2");
    }

    #[test]
    fn classifies_statements() {
        assert_eq!(analyze_statement("select 1").unwrap().kind, StatementKind::Read);
        assert_eq!(
            analyze_statement("WITH x AS (SELECT 1) SELECT * FROM x").unwrap().kind,
            StatementKind::Read
        );
        assert_eq!(
            analyze_statement("insert into t values (1)").unwrap().kind,
            StatementKind::Write
        );
        assert_eq!(
            analyze_statement("CREATE TABLE t (x)").unwrap().kind,
            StatementKind::Schema
        );
        assert_eq!(
            analyze_statement("PRAGMA table_info(t)").unwrap().kind,
            StatementKind::Read
        );
        assert_eq!(analyze_statement("ANALYZE").unwrap().kind, StatementKind::Maintenance);
        assert!(analyze_statement("CREATE VIRTUAL TABLE f USING fts5(body)").is_ok());
    }

    #[test]
    fn rejects_dangerous_statements() {
        for bad in [
            "ATTACH DATABASE '/tmp/x.db' AS x",
            "DETACH x",
            "PRAGMA writable_schema = 1",
            "PRAGMA foreign_keys = OFF",
            "PRAGMA database_list",
            "PRAGMA table_info = 1",
            "BEGIN",
            "COMMIT",
            "VACUUM",
            "VACUUM INTO '/tmp/x'",
            "CREATE TEMP TABLE x (a)",
            "CREATE TEMPORARY VIEW v AS SELECT 1",
            "CREATE VIRTUAL TABLE x USING fileio(a)",
            "SELECT load_extension('/tmp/evil')",
            "SELECT fts3_tokenizer('x')",
            "(SELECT 1)",
            "",
        ] {
            assert!(analyze_statement(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn spots_system_tables_in_any_quoting() {
        for s in [
            "DELETE FROM _forgebase_collections",
            "UPDATE \"_FORGEBASE_meta\" SET value = 1",
            "INSERT INTO [_forgebase_indexes] VALUES (1)",
            "SELECT * FROM `_forgebase_schema_history`",
        ] {
            assert!(analyze_statement(s).unwrap().touches_system_tables, "{s}");
        }
        assert!(
            !analyze_statement("SELECT '_forgebase_x'")
                .unwrap()
                .touches_system_tables
        );
    }
}
