//! Lexical analysis of raw SQL for the SQL editor (ADR-0003 §6,
//! ADR-0028 §7.5).
//!
//! This is a tokenizer, not a parser: it splits statements, finds the leading
//! keyword and spots names that must never be touched. The engine has the
//! final word on read vs write (`Backend::statement_writes`), and reads run on
//! connections that cannot write.
//!
//! Two flavours ([`SqlFlavor`]): SQLite's (`[ident]`, `` `ident` ``,
//! `:name`/`@name`/`$name` parameters, trigger bodies with `BEGIN ... END`)
//! and PostgreSQL's (`$tag$ ... $tag$` and `E'...'` strings, nested block
//! comments, `[`/`::`/`@` as operators, `BEGIN ATOMIC ... END` routine bodies,
//! and its own deny list). On PostgreSQL the deny list is defence in depth:
//! the boundary is the role the editor runs as (FB-153).

use forgebase_db::Dialect;

use crate::QueryError;

/// Which engine's SQL the text is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SqlFlavor {
    /// SQLite.
    #[default]
    Sqlite,
    /// PostgreSQL.
    Postgres,
}

impl SqlFlavor {
    /// The flavour of a dialect.
    pub fn of(d: &dyn Dialect) -> Self {
        if d.name() == "postgres" {
            SqlFlavor::Postgres
        } else {
            SqlFlavor::Sqlite
        }
    }
}

/// Maximum SQL text accepted by the editor (1 MiB).
pub const MAX_SQL_LEN: usize = 1 << 20;

/// Token kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    /// Keyword or bare identifier.
    Word,
    /// `"ident"`, `` `ident` `` or `[ident]`.
    QuotedIdent,
    /// `'string'` (also PostgreSQL's `E'string'`).
    String,
    /// PostgreSQL `$tag$ body $tag$` (the body, without the tags).
    DollarString,
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

/// Tokenizes SQLite SQL, skipping whitespace and comments.
pub fn tokenize(sql: &str) -> Result<Vec<Token>, QueryError> {
    tokenize_for(sql, SqlFlavor::Sqlite)
}

/// Tokenizes SQL of `flavor`, skipping whitespace and comments.
pub fn tokenize_for(sql: &str, flavor: SqlFlavor) -> Result<Vec<Token>, QueryError> {
    tokenize_spans(sql, flavor).map(|v| v.into_iter().map(|(t, _)| t).collect())
}

/// The closing `$tag$` of a PostgreSQL dollar quote opening at `i`, if `i`
/// starts one: returns (end of the opening tag, the tag).
fn dollar_tag(sql: &str, i: usize) -> Option<(usize, &str)> {
    let b = sql.as_bytes();
    let mut j = i + 1;
    if b.get(j).is_some_and(u8::is_ascii_digit) {
        return None; // `$1`: a parameter
    }
    while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_' || b[j] >= 0x80) {
        j += 1;
    }
    (b.get(j) == Some(&b'$')).then(|| (j + 1, &sql[i..=j]))
}

fn tokenize_spans(sql: &str, flavor: SqlFlavor) -> Result<Vec<(Token, std::ops::Range<usize>)>, QueryError> {
    let pg = flavor == SqlFlavor::Postgres;
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
            b'/' if pg && b.get(i + 1) == Some(&b'*') => {
                // PostgreSQL block comments nest and must be closed.
                let mut depth = 0usize;
                let mut j = i;
                loop {
                    if j + 1 >= b.len() {
                        return Err(QueryError::syntax(start, "unterminated comment"));
                    }
                    if b[j] == b'/' && b[j + 1] == b'*' {
                        depth += 1;
                        j += 2;
                    } else if b[j] == b'*' && b[j + 1] == b'/' {
                        depth -= 1;
                        j += 2;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        j += 1;
                    }
                }
                i = j;
                continue;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                match sql[i + 2..].find("*/") {
                    Some(end) => i = i + 2 + end + 2,
                    None => i = b.len(), // SQLite accepts an unterminated trailing comment
                }
                continue;
            }
            b'$' if pg && dollar_tag(sql, i).is_some() => {
                let Some((body_start, tag)) = dollar_tag(sql, i) else {
                    unreachable!("checked by the guard")
                };
                let Some(end) = sql[body_start..].find(tag) else {
                    return Err(QueryError::syntax(start, "unterminated dollar-quoted text"));
                };
                let text = sql[body_start..body_start + end].to_owned();
                let j = body_start + end + tag.len();
                out.push((
                    Token {
                        kind: TokenKind::DollarString,
                        text,
                    },
                    start..j,
                ));
                i = j;
            }
            b'e' | b'E' if pg && b.get(i + 1) == Some(&b'\'') => {
                // E'...': backslash escapes.
                let mut j = i + 2;
                let mut text = Vec::new();
                loop {
                    match b.get(j) {
                        None => return Err(QueryError::syntax(start, "unterminated quoted text")),
                        Some(b'\\') => {
                            if let Some(&x) = b.get(j + 1) {
                                text.push(x);
                            }
                            j += 2;
                        }
                        Some(b'\'') if b.get(j + 1) == Some(&b'\'') => {
                            text.push(b'\'');
                            j += 2;
                        }
                        Some(b'\'') => {
                            j += 1;
                            break;
                        }
                        Some(&x) => {
                            text.push(x);
                            j += 1;
                        }
                    }
                }
                let text = String::from_utf8_lossy(&text).into_owned();
                out.push((
                    Token {
                        kind: TokenKind::String,
                        text,
                    },
                    start..j,
                ));
                i = j;
            }
            b'`' | b'[' if pg => {
                // Operators in PostgreSQL (array subscripts), not quotes.
                out.push((tok(TokenKind::Punct, &sql[i..i + 1]), start..i + 1));
                i += 1;
            }
            b':' | b'@' if pg => {
                // `::` casts and `@>`-style operators: PostgreSQL has no
                // named parameters.
                out.push((tok(TokenKind::Punct, &sql[i..i + 1]), start..i + 1));
                i += 1;
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

/// Splits SQLite SQL text into statements (without the trailing `;`).
/// Semicolons inside strings, comments and `CREATE TRIGGER ... BEGIN ... END`
/// bodies do not split. Empty statements are dropped.
pub fn split_statements(sql: &str) -> Result<Vec<String>, QueryError> {
    split_statements_for(sql, SqlFlavor::Sqlite)
}

/// [`split_statements`] for `flavor`. On PostgreSQL, routine bodies are
/// dollar-quoted strings or `BEGIN ATOMIC ... END` blocks; neither splits.
pub fn split_statements_for(sql: &str, flavor: SqlFlavor) -> Result<Vec<String>, QueryError> {
    let tokens = tokenize_spans(sql, flavor)?;
    let mut out = Vec::new();
    let mut stmt_start: Option<usize> = None;
    let mut stmt_tokens: Vec<&Token> = Vec::new();
    // BEGIN/CASE ... END nesting inside a trigger (SQLite) or `BEGIN ATOMIC`
    // routine (PostgreSQL) body.
    let mut depth = 0i32;
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
        let prev_begin = stmt_tokens.last().is_some_and(|t| t.is_word("BEGIN"));
        stmt_tokens.push(tok);
        let opens = match flavor {
            SqlFlavor::Sqlite => is_create_trigger(&stmt_tokens) && tok.is_word("BEGIN"),
            SqlFlavor::Postgres => is_create_routine(&stmt_tokens) && prev_begin && tok.is_word("ATOMIC"),
        };
        if opens || (depth > 0 && tok.is_word("CASE")) {
            depth += 1;
        } else if tok.is_word("END") && depth > 0 {
            depth -= 1;
        }
    }
    if depth != 0 {
        return Err(QueryError::syntax(sql.len(), "unterminated trigger or routine body"));
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

/// `CREATE [OR REPLACE] FUNCTION|PROCEDURE`.
fn is_create_routine(t: &[&Token]) -> bool {
    t.first().is_some_and(|w| w.is_word("CREATE"))
        && t.iter()
            .skip(1)
            .take(3)
            .any(|w| w.is_word("FUNCTION") || w.is_word("PROCEDURE"))
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

/// Classifies one SQLite statement and rejects what the SQL editor never runs:
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

/// [`analyze_statement`] for `flavor`.
pub fn analyze_statement_for(stmt: &str, flavor: SqlFlavor) -> Result<StatementInfo, QueryError> {
    match flavor {
        SqlFlavor::Sqlite => analyze_statement(stmt),
        SqlFlavor::Postgres => analyze_pg(stmt),
    }
}

/// Functions the SQL editor never calls on PostgreSQL: server files and
/// programs, other sessions, configuration, session-level locks, sleeping on
/// the single writer, and functions that run a query given as text (which
/// would hide the rest of this list).
const PG_FORBIDDEN_FUNCTIONS: &[&str] = &[
    "pg_read_file",
    "pg_read_binary_file",
    "pg_ls_dir",
    "pg_stat_file",
    "lo_import",
    "lo_export",
    "pg_terminate_backend",
    "pg_cancel_backend",
    "pg_reload_conf",
    "pg_rotate_logfile",
    "pg_promote",
    "pg_switch_wal",
    "pg_sleep",
    "pg_sleep_for",
    "pg_sleep_until",
    "set_config",
    "query_to_xml",
    "query_to_xmlschema",
    "query_to_xml_and_xmlschema",
    "cursor_to_xml",
    "cursor_to_xmlschema",
    "ts_stat",
    "pg_import_system_collations",
];

/// Function-name prefixes never called on PostgreSQL (`dblink*`, files,
/// replication, backups, session-level advisory locks).
const PG_FORBIDDEN_PREFIXES: &[&str] = &[
    "dblink",
    "pg_ls_",
    "pg_read_",
    "pg_file_",
    "pg_logical_",
    "pg_replication_",
    "pg_create_",
    "pg_drop_replication",
    "pg_backup_",
    "pg_advisory",
    "pg_try_advisory",
];

/// Objects the SQL editor does not create, alter or drop on PostgreSQL:
/// cluster-wide or privileged objects, and schemas (collections live in
/// `public`, ForgeBase's tables in `forgebase`).
const PG_FORBIDDEN_OBJECTS: &[&str] = &[
    "EXTENSION",
    "ROLE",
    "USER",
    "GROUP",
    "DATABASE",
    "TABLESPACE",
    "SCHEMA",
    "SUBSCRIPTION",
    "PUBLICATION",
    "SERVER",
    "FOREIGN",
    "EVENT",
    "LANGUAGE",
    "SYSTEM",
    "DEFAULT",
    "ACCESS",
    "TRANSFORM",
    "OWNED",
    "LARGE",
];

fn pg_function_forbidden(name: &str) -> bool {
    PG_FORBIDDEN_FUNCTIONS.contains(&name) || PG_FORBIDDEN_PREFIXES.iter().any(|p| name.starts_with(p))
}

/// The first forbidden function `tokens` call (a name followed by `(`),
/// including inside dollar-quoted routine bodies.
fn pg_forbidden_call(tokens: &[Token], depth: usize) -> Result<Option<String>, QueryError> {
    for (i, t) in tokens.iter().enumerate() {
        match t.kind {
            TokenKind::Word | TokenKind::QuotedIdent => {
                let lower = t.text.to_ascii_lowercase();
                if pg_function_forbidden(&lower) && tokens.get(i + 1).is_some_and(|n| n.text == "(") {
                    return Ok(Some(lower));
                }
            }
            TokenKind::DollarString if depth < 4 => {
                // A body that does not tokenize is still searched word by word.
                let inner = match tokenize_for(&t.text, SqlFlavor::Postgres) {
                    Ok(inner) => inner,
                    Err(_) => {
                        let lower = t.text.to_ascii_lowercase();
                        let found = lower
                            .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                            .find(|w| pg_function_forbidden(w))
                            .map(str::to_owned);
                        if found.is_some() {
                            return Ok(found);
                        }
                        continue;
                    }
                };
                if let Some(f) = pg_forbidden_call(&inner, depth + 1)? {
                    return Ok(Some(f));
                }
            }
            _ => {}
        }
    }
    Ok(None)
}

/// The object word of `CREATE|ALTER|DROP [OR REPLACE] [modifiers] <object>`.
fn pg_object_word(tokens: &[Token]) -> String {
    const SKIP: &[&str] = &[
        "OR",
        "REPLACE",
        "UNIQUE",
        "UNLOGGED",
        "GLOBAL",
        "LOCAL",
        "TEMP",
        "TEMPORARY",
        "MATERIALIZED",
        "RECURSIVE",
        "TRUSTED",
        "PROCEDURAL",
        "CONSTRAINT",
    ];
    tokens
        .iter()
        .skip(1)
        .take(6)
        .map(|t| t.text.to_ascii_uppercase())
        .find(|w| !SKIP.contains(&w.as_str()))
        .unwrap_or_default()
}

/// Whether two consecutive words `a b` appear in `tokens`.
fn has_words(tokens: &[Token], a: &str, b: &[&str]) -> bool {
    tokens.windows(2).any(|w| {
        w[0].is_word(a)
            && (w[1].kind == TokenKind::Word || w[1].kind == TokenKind::String)
            && b.iter().any(|x| w[1].text.eq_ignore_ascii_case(x))
    })
}

/// A top-level `SELECT ... INTO new_table` (creates a table).
fn pg_select_into(tokens: &[Token]) -> bool {
    let mut depth = 0i32;
    let mut verb = "";
    for t in tokens {
        match t.text.as_str() {
            "(" if t.kind == TokenKind::Punct => depth += 1,
            ")" if t.kind == TokenKind::Punct => depth -= 1,
            _ if depth == 0 && t.kind == TokenKind::Word => {
                let w = t.text.to_ascii_uppercase();
                match w.as_str() {
                    "SELECT" | "INSERT" | "UPDATE" | "DELETE" | "MERGE" => {
                        verb = match w.as_str() {
                            "SELECT" => "SELECT",
                            _ => "DML",
                        };
                    }
                    "INTO" if verb == "SELECT" => return true,
                    _ => {}
                }
            }
            _ => {}
        }
    }
    false
}

/// `EXPLAIN ANALYZE ...` / `EXPLAIN (ANALYZE ...) ...`: runs the statement.
fn pg_explain_analyze(tokens: &[Token]) -> bool {
    let is_analyze = |t: &Token| t.is_word("ANALYZE") || t.is_word("ANALYSE");
    match tokens.get(1) {
        Some(t) if t.text == "(" => tokens.iter().skip(2).take_while(|t| t.text != ")").any(is_analyze),
        _ => tokens
            .iter()
            .skip(1)
            .take_while(|t| t.kind == TokenKind::Word && ["ANALYZE", "ANALYSE", "VERBOSE"].iter().any(|w| t.is_word(w)))
            .any(is_analyze),
    }
}

/// The PostgreSQL deny list of ADR-0028 §7.5 (defence in depth: the
/// editor's role is the boundary, FB-153).
fn analyze_pg(stmt: &str) -> Result<StatementInfo, QueryError> {
    let tokens = tokenize_for(stmt, SqlFlavor::Postgres)?;
    let Some(first) = tokens.first() else {
        return Err(QueryError::NotAllowed("empty statement".into()));
    };
    if first.kind != TokenKind::Word {
        return Err(QueryError::NotAllowed("a statement must start with a keyword".into()));
    }
    let keyword = first.text.to_ascii_uppercase();
    let not_allowed = |why: &str| Err(QueryError::NotAllowed(why.to_owned()));
    let mut kind = match keyword.as_str() {
        "SELECT" | "VALUES" | "WITH" | "TABLE" | "SHOW" => StatementKind::Read,
        "EXPLAIN" => {
            if pg_explain_analyze(&tokens) {
                return not_allowed("EXPLAIN ANALYZE runs the statement; use the query-plan API");
            }
            StatementKind::Read
        }
        "INSERT" | "UPDATE" | "DELETE" | "MERGE" | "TRUNCATE" | "CALL" => StatementKind::Write,
        "CREATE" | "ALTER" | "DROP" | "COMMENT" => StatementKind::Schema,
        "ANALYZE" | "ANALYSE" | "REINDEX" | "REFRESH" => StatementKind::Maintenance,
        "BEGIN" | "START" | "COMMIT" | "END" | "ROLLBACK" | "ABORT" | "SAVEPOINT" | "RELEASE" | "PREPARE" => {
            return not_allowed("transaction control is managed by ForgeBase; send the statements as one batch");
        }
        "SET" | "RESET" | "DISCARD" => {
            return not_allowed("session settings and roles are managed by ForgeBase");
        }
        "COPY" => return not_allowed("COPY is not allowed from the SQL editor; use the import and export APIs"),
        "VACUUM" | "CHECKPOINT" | "CLUSTER" => {
            return not_allowed("maintenance commands run through the maintenance API");
        }
        "LOAD" => return not_allowed("loading libraries is not allowed"),
        "LISTEN" | "NOTIFY" | "UNLISTEN" => return not_allowed("notifications are not available from the SQL editor"),
        "GRANT" | "REVOKE" | "REASSIGN" | "SECURITY" => {
            return not_allowed("privileges and roles are managed by ForgeBase");
        }
        "DO" => return not_allowed("anonymous code blocks are not allowed; create a function"),
        "EXECUTE" | "DEALLOCATE" | "DECLARE" | "FETCH" | "MOVE" | "CLOSE" | "LOCK" | "IMPORT" => {
            return Err(QueryError::NotAllowed(format!(
                "{keyword} is not available from the SQL editor"
            )));
        }
        other => return Err(QueryError::NotAllowed(format!("unsupported statement {other}"))),
    };
    if kind == StatementKind::Read && keyword != "EXPLAIN" && pg_select_into(&tokens) {
        kind = StatementKind::Schema;
    }
    if kind == StatementKind::Schema && keyword != "COMMENT" {
        if tokens
            .iter()
            .take(5)
            .any(|t| t.is_word("TEMP") || t.is_word("TEMPORARY"))
        {
            return not_allowed("temporary objects are not allowed");
        }
        let object = pg_object_word(&tokens);
        if PG_FORBIDDEN_OBJECTS.contains(&object.as_str()) {
            return Err(QueryError::NotAllowed(format!(
                "{keyword} {object} is not available from the SQL editor"
            )));
        }
        if has_words(&tokens, "LANGUAGE", &["c", "internal"]) {
            return not_allowed("functions in LANGUAGE c or internal are not allowed");
        }
        if has_words(&tokens, "SECURITY", &["DEFINER"]) {
            return not_allowed("SECURITY DEFINER functions are not allowed");
        }
        if has_words(&tokens, "OWNER", &["TO"]) {
            return not_allowed("object owners are managed by ForgeBase");
        }
    }
    if let Some(f) = pg_forbidden_call(&tokens, 0)? {
        return Err(QueryError::NotAllowed(format!("function {f} is not allowed")));
    }
    let mut identifiers: Vec<String> = Vec::new();
    let mut touches_system_tables = false;
    for (i, t) in tokens.iter().enumerate() {
        if matches!(t.kind, TokenKind::Word | TokenKind::QuotedIdent) {
            let lower = t.text.to_ascii_lowercase();
            let qualifier = tokens.get(i + 1).is_some_and(|n| n.text == ".");
            let call = tokens.get(i + 1).is_some_and(|n| n.text == "(");
            // `_forgebase*` tables, the `forgebase` schema and the catalogs.
            if lower.starts_with("_forgebase")
                || (qualifier
                    && matches!(
                        lower.as_str(),
                        "forgebase" | "pg_catalog" | "information_schema" | "pg_toast"
                    ))
                || (lower.starts_with("pg_") && !call)
            {
                touches_system_tables = true;
            }
            if !identifiers.contains(&lower) {
                identifiers.push(lower);
            }
        }
    }
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

    fn pg(sql: &str) -> Result<StatementInfo, QueryError> {
        analyze_statement_for(sql, SqlFlavor::Postgres)
    }

    #[test]
    fn postgres_tokenizer_knows_its_quotes() {
        let sql = "CREATE FUNCTION f() RETURNS trigger LANGUAGE plpgsql AS $body$ BEGIN INSERT INTO a VALUES ('x;'); RETURN NEW; END $body$; \
                   SELECT E'it\\'s; fine', $$;$$, a[1], x::text /* a /* nested ; */ comment */; \
                   CREATE PROCEDURE p() LANGUAGE sql BEGIN ATOMIC INSERT INTO a VALUES (1); SELECT CASE WHEN 1 = 1 THEN 1 END; END; SELECT 2";
        let parts = split_statements_for(sql, SqlFlavor::Postgres).unwrap();
        assert_eq!(parts.len(), 4, "{parts:#?}");
        assert!(parts[0].ends_with("$body$"));
        assert!(parts[1].starts_with("SELECT E'") && parts[1].ends_with("comment */"));
        assert!(parts[2].starts_with("CREATE PROCEDURE") && parts[2].ends_with("END"));
        assert_eq!(parts[3], "SELECT 2");
        let toks = tokenize_for("SELECT $1, ?2, $tag$a'b$tag$", SqlFlavor::Postgres).unwrap();
        assert_eq!(toks[1].kind, TokenKind::Param);
        assert_eq!(toks[3].kind, TokenKind::Param);
        assert_eq!(toks[5].kind, TokenKind::DollarString);
        assert_eq!(toks[5].text, "a'b");
        assert!(tokenize_for("SELECT $x$ open", SqlFlavor::Postgres).is_err());
        assert!(tokenize_for("SELECT 1 /* open", SqlFlavor::Postgres).is_err());
    }

    #[test]
    fn postgres_classifies_statements() {
        for (sql, kind) in [
            ("select 1", StatementKind::Read),
            ("TABLE posts", StatementKind::Read),
            ("SHOW server_version", StatementKind::Read),
            ("EXPLAIN SELECT 1", StatementKind::Read),
            ("WITH x AS (SELECT 1) SELECT * FROM x", StatementKind::Read),
            (
                "WITH x AS (SELECT 1) INSERT INTO t SELECT * FROM x",
                StatementKind::Read,
            ),
            ("SELECT a INTO t2 FROM t", StatementKind::Schema),
            ("INSERT INTO t SELECT a FROM u", StatementKind::Write),
            (
                "MERGE INTO t USING u ON t.id = u.id WHEN MATCHED THEN DELETE",
                StatementKind::Write,
            ),
            ("TRUNCATE t", StatementKind::Write),
            ("CREATE TABLE t (x bigint)", StatementKind::Schema),
            (
                "CREATE OR REPLACE FUNCTION f() RETURNS int LANGUAGE sql AS $$ SELECT 1 $$",
                StatementKind::Schema,
            ),
            ("COMMENT ON TABLE t IS 'x'", StatementKind::Schema),
            ("ANALYZE t", StatementKind::Maintenance),
            ("REFRESH MATERIALIZED VIEW v", StatementKind::Maintenance),
        ] {
            assert_eq!(pg(sql).unwrap().kind, kind, "{sql}");
        }
    }

    #[test]
    fn postgres_deny_list() {
        for bad in [
            "BEGIN",
            "START TRANSACTION",
            "COMMIT",
            "ROLLBACK",
            "SAVEPOINT a",
            "PREPARE TRANSACTION 'x'",
            "COPY t TO '/tmp/x'",
            "COPY t FROM PROGRAM 'id'",
            "ALTER SYSTEM SET work_mem = '1GB'",
            "LOAD 'auto_explain'",
            "SET ROLE postgres",
            "SET SESSION AUTHORIZATION postgres",
            "RESET ROLE",
            "SET search_path = public",
            "CREATE EXTENSION dblink",
            "CREATE FUNCTION f() RETURNS int LANGUAGE c AS 'lib', 'f'",
            "CREATE FUNCTION f() RETURNS int LANGUAGE 'internal' AS 'int4in'",
            "CREATE FUNCTION f() RETURNS int LANGUAGE sql SECURITY DEFINER AS $$ SELECT 1 $$",
            "ALTER FUNCTION f() SECURITY DEFINER",
            "ALTER TABLE t OWNER TO postgres",
            "LISTEN x",
            "NOTIFY x",
            "UNLISTEN *",
            "VACUUM",
            "CHECKPOINT",
            "CLUSTER t",
            "SELECT pg_read_file('/etc/passwd')",
            "SELECT pg_catalog.pg_read_binary_file('x')",
            "SELECT \"pg_ls_dir\"('.')",
            "SELECT lo_import('/etc/passwd')",
            "SELECT lo_export(1, '/tmp/x')",
            "SELECT pg_terminate_backend(1)",
            "SELECT pg_cancel_backend(1)",
            "SELECT pg_reload_conf()",
            "SELECT set_config('forgebase.schema_gen', '1', false)",
            "SELECT dblink_connect('x')",
            "SELECT * FROM dblink('x', 'y') AS t(a int)",
            "SELECT pg_advisory_lock(1)",
            "SELECT pg_sleep(10)",
            "SELECT query_to_xml('select 1', true, true, '')",
            "CREATE FUNCTION f() RETURNS text LANGUAGE plpgsql AS $$ BEGIN RETURN pg_read_file('x'); END $$",
            "CREATE FUNCTION f() RETURNS text LANGUAGE plpgsql AS $q$ BEGIN RETURN pg_read_file('x'; END $q$",
            "DO $$ BEGIN PERFORM 1; END $$",
            "GRANT SELECT ON t TO PUBLIC",
            "REVOKE ALL ON t FROM PUBLIC",
            "CREATE ROLE r",
            "ALTER ROLE r SUPERUSER",
            "DROP ROLE r",
            "CREATE SCHEMA s",
            "CREATE DATABASE d",
            "CREATE SERVER s FOREIGN DATA WRAPPER w",
            "CREATE EVENT TRIGGER e ON ddl_command_start EXECUTE FUNCTION f()",
            "CREATE TEMP TABLE x (a int)",
            "CREATE OR REPLACE TEMP VIEW v AS SELECT 1",
            "EXPLAIN ANALYZE DELETE FROM t",
            "EXPLAIN (ANALYZE, FORMAT JSON) DELETE FROM t",
            "PRAGMA table_info(t)",
            "ATTACH DATABASE 'x' AS y",
            "DISCARD ALL",
            "LOCK TABLE t",
            "EXECUTE p",
            "DECLARE c CURSOR FOR SELECT 1",
            "",
        ] {
            assert!(pg(bad).is_err(), "{bad}");
        }
        assert!(pg("SELECT pg_typeof(1), current_setting('server_version')").is_ok());
        assert!(
            pg("SELECT pg_advisory_xact_lock_shared").is_ok(),
            "a column name, not a call"
        );
    }

    #[test]
    fn postgres_spots_system_objects() {
        for s in [
            "DELETE FROM _forgebase_collections",
            "UPDATE \"_FORGEBASE_meta\" SET value = 1",
            "INSERT INTO forgebase.anything VALUES (1)",
            "UPDATE pg_catalog.pg_class SET relname = 'x'",
            "DELETE FROM pg_authid",
            "UPDATE information_schema.tables SET x = 1",
        ] {
            assert!(pg(s).unwrap().touches_system_tables, "{s}");
        }
        for s in [
            "SELECT '_forgebase_x'",
            "SELECT pg_typeof(1)",
            "UPDATE forgebase_notes SET a = 1",
        ] {
            assert!(!pg(s).unwrap().touches_system_tables, "{s}");
        }
    }
}
