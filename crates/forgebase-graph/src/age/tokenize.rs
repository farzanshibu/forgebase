//! The openCypher tokenizer of `POST /graph/cypher` (ADR-0029 §1.5,
//! FB-163): defence in depth, not the boundary. The boundary is the
//! engine: the query runs in a `READ ONLY` transaction, as `forgebase_rls`
//! for non-privileged callers, on a graph that holds one tenant's data.
//!
//! The tokenizer
//!
//! * refuses write clauses (`CREATE`, `MERGE`, `SET`, `DELETE`, `DETACH`,
//!   `REMOVE`), `CALL`, `LOAD` and `FOREACH`;
//! * refuses schema-qualified function calls (`pg_catalog.f(…)`: AGE would
//!   call the PostgreSQL function);
//! * refuses the request's random dollar-quote tag (the query is sent to the
//!   engine inside `$<tag>$ … $<tag>$`) and any `$$`;
//! * names and counts the items of the final `RETURN`, which AGE needs as
//!   its `AS (c1 agtype, …)` column list.
//!
//! Keywords are matched case-insensitively outside strings, comments and
//! backquoted names; a keyword used as a property (`n.set`) or a map key
//! (`{create: 1}`) is not a clause.

/// Most `RETURN` items (columns).
pub const MAX_COLUMNS: usize = 100;

/// Words that start a clause that writes or calls out.
const REFUSED: &[&str] = &["create", "merge", "set", "delete", "detach", "remove", "call", "load", "foreach"];

/// What the tokenizer found wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// Stable code (`cypher_read_only`, `cypher_invalid`).
    pub code: &'static str,
    /// Explanation.
    pub message: String,
}

fn refuse(code: &'static str, message: impl Into<String>) -> Refusal {
    Refusal {
        code,
        message: message.into(),
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    /// A plain word (identifier or keyword), with its source span.
    Word(String),
    /// A backquoted name.
    Quoted,
    Str,
    Num,
    /// `$name` or `$1`.
    Param,
    Punct(char),
    /// `$$` or `$<tag>$`: never valid Cypher.
    Dollar(String),
}

#[derive(Debug, Clone)]
struct Token {
    tok: Tok,
    start: usize,
    end: usize,
}

fn lex(q: &str) -> Result<Vec<Token>, Refusal> {
    let b: Vec<(usize, char)> = q.char_indices().collect();
    let at = |i: usize| b.get(i).map(|&(_, c)| c);
    let pos = |i: usize| b.get(i).map_or(q.len(), |&(p, _)| p);
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(c) = at(i) {
        let start = pos(i);
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '/' && at(i + 1) == Some('/') {
            while at(i).is_some_and(|c| c != '\n') {
                i += 1;
            }
            continue;
        }
        if c == '/' && at(i + 1) == Some('*') {
            i += 2;
            loop {
                match at(i) {
                    None => return Err(refuse("cypher_invalid", "unterminated comment")),
                    Some('*') if at(i + 1) == Some('/') => {
                        i += 2;
                        break;
                    }
                    Some(_) => i += 1,
                }
            }
            continue;
        }
        if c == '\'' || c == '"' {
            i += 1;
            loop {
                match at(i) {
                    None => return Err(refuse("cypher_invalid", "unterminated string")),
                    Some('\\') => i += 2,
                    Some(d) if d == c => {
                        i += 1;
                        break;
                    }
                    Some(_) => i += 1,
                }
            }
            out.push(Token {
                tok: Tok::Str,
                start,
                end: pos(i),
            });
            continue;
        }
        if c == '`' {
            i += 1;
            loop {
                match at(i) {
                    None => return Err(refuse("cypher_invalid", "unterminated backquoted name")),
                    Some('`') if at(i + 1) == Some('`') => i += 2,
                    Some('`') => {
                        i += 1;
                        break;
                    }
                    Some(_) => i += 1,
                }
            }
            out.push(Token {
                tok: Tok::Quoted,
                start,
                end: pos(i),
            });
            continue;
        }
        if c == '$' {
            let mut j = i + 1;
            while at(j).is_some_and(|c| c.is_alphanumeric() || c == '_') {
                j += 1;
            }
            if at(j) == Some('$') {
                let tag: String = b[i + 1..j].iter().map(|&(_, c)| c).collect();
                out.push(Token {
                    tok: Tok::Dollar(tag),
                    start,
                    end: pos(j + 1),
                });
                i = j + 1;
                continue;
            }
            if j == i + 1 {
                return Err(refuse("cypher_invalid", "a parameter needs a name after $"));
            }
            out.push(Token {
                tok: Tok::Param,
                start,
                end: pos(j),
            });
            i = j;
            continue;
        }
        if c.is_ascii_digit() {
            let mut j = i;
            while at(j).is_some_and(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_') {
                // `1..3` (a range) is two numbers and `..`.
                if at(j) == Some('.') && at(j + 1) == Some('.') {
                    break;
                }
                j += 1;
            }
            out.push(Token {
                tok: Tok::Num,
                start,
                end: pos(j),
            });
            i = j;
            continue;
        }
        if c.is_alphabetic() || c == '_' {
            let mut j = i;
            while at(j).is_some_and(|c| c.is_alphanumeric() || c == '_') {
                j += 1;
            }
            out.push(Token {
                tok: Tok::Word(q[start..pos(j)].to_ascii_lowercase()),
                start,
                end: pos(j),
            });
            i = j;
            continue;
        }
        out.push(Token {
            tok: Tok::Punct(c),
            start,
            end: pos(i + 1),
        });
        i += 1;
    }
    Ok(out)
}

/// A checked query: its `RETURN` column names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    /// One name per `RETURN` item: its alias, else its text.
    pub columns: Vec<String>,
}

fn word(t: &Token) -> Option<&str> {
    match &t.tok {
        Tok::Word(w) => Some(w),
        _ => None,
    }
}

/// Checks `query` (see the module documentation). `tag` is the request's
/// dollar-quote tag.
pub fn check(query: &str, tag: &str) -> Result<Checked, Refusal> {
    if query.contains(&format!("${tag}$")) {
        return Err(refuse("cypher_invalid", "the query contains a reserved dollar-quote tag"));
    }
    let toks = lex(query)?;
    let mut depth = 0i32;
    let mut last_return: Option<usize> = None;
    for (i, t) in toks.iter().enumerate() {
        match &t.tok {
            Tok::Dollar(_) => {
                return Err(refuse("cypher_invalid", "dollar-quoted text is not allowed"));
            }
            Tok::Punct('(' | '[' | '{') => depth += 1,
            Tok::Punct(')' | ']' | '}') => depth -= 1,
            Tok::Punct(';') => {
                return Err(refuse("cypher_invalid", "send one query, without ';'"));
            }
            Tok::Word(w) => {
                let after_dot = i > 0 && toks[i - 1].tok == Tok::Punct('.');
                let next = toks.get(i + 1).map(|t| &t.tok);
                let map_key = next == Some(&Tok::Punct(':'));
                if !after_dot && !map_key && REFUSED.contains(&w.as_str()) {
                    let msg = if w == "call" || w == "load" {
                        format!("{} is not allowed in openCypher queries", w.to_uppercase())
                    } else {
                        format!(
                            "openCypher queries are read-only: {} is not allowed",
                            w.to_uppercase()
                        )
                    };
                    return Err(refuse("cypher_read_only", msg));
                }
                // `schema.function(`: a PostgreSQL function call.
                if !after_dot
                    && next == Some(&Tok::Punct('.'))
                    && toks.get(i + 2).and_then(word).is_some()
                    && toks.get(i + 3).map(|t| &t.tok) == Some(&Tok::Punct('('))
                {
                    return Err(refuse(
                        "cypher_invalid",
                        format!("schema-qualified function calls are not allowed ({w}.…)"),
                    ));
                }
                if w == "return" && depth == 0 && !after_dot && !map_key {
                    last_return = Some(i);
                }
            }
            _ => {}
        }
    }
    if depth != 0 {
        return Err(refuse("cypher_invalid", "unbalanced brackets"));
    }
    let Some(r) = last_return else {
        return Err(refuse("cypher_invalid", "the query needs a RETURN clause"));
    };
    Ok(Checked {
        columns: return_items(query, &toks[r + 1..])?,
    })
}

/// Names of the items after `RETURN` (up to `ORDER BY`, `SKIP`, `LIMIT` or
/// the end).
fn return_items(query: &str, toks: &[Token]) -> Result<Vec<String>, Refusal> {
    let mut items: Vec<(usize, usize, Option<String>)> = Vec::new();
    let mut depth = 0i32;
    let mut start: Option<usize> = None;
    let mut end = 0usize;
    let mut alias: Option<String> = None;
    let mut i = 0;
    if toks.first().and_then(word) == Some("distinct") {
        i = 1;
    }
    let close = |items: &mut Vec<(usize, usize, Option<String>)>, start: &mut Option<usize>, end: usize, alias: &mut Option<String>| {
        if let Some(s) = start.take() {
            items.push((s, end, alias.take()));
        }
    };
    while i < toks.len() {
        let t = &toks[i];
        if depth == 0 {
            match word(t) {
                Some("order" | "skip" | "limit") => break,
                Some("as") if start.is_some() => {
                    let name = toks.get(i + 1).ok_or_else(|| refuse("cypher_invalid", "AS needs a name"))?;
                    let text = &query[name.start..name.end];
                    alias = Some(text.trim_matches('`').replace("``", "`"));
                    end = toks[i].start;
                    i += 2;
                    continue;
                }
                _ => {}
            }
            if t.tok == Tok::Punct(',') {
                close(&mut items, &mut start, end, &mut alias);
                i += 1;
                continue;
            }
            if t.tok == Tok::Punct('*') && start.is_none() {
                return Err(refuse("cypher_invalid", "RETURN * is not supported: name the columns"));
            }
        }
        match t.tok {
            Tok::Punct('(' | '[' | '{') => depth += 1,
            Tok::Punct(')' | ']' | '}') => depth -= 1,
            _ => {}
        }
        if start.is_none() {
            start = Some(t.start);
        }
        if alias.is_none() {
            end = t.end;
        }
        i += 1;
    }
    close(&mut items, &mut start, end, &mut alias);
    if items.is_empty() {
        return Err(refuse("cypher_invalid", "RETURN needs at least one item"));
    }
    if items.len() > MAX_COLUMNS {
        return Err(refuse(
            "cypher_invalid",
            format!("RETURN has more than {MAX_COLUMNS} items"),
        ));
    }
    Ok(items
        .into_iter()
        .map(|(s, e, alias)| alias.unwrap_or_else(|| query[s..e].split_whitespace().collect::<Vec<_>>().join(" ")))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TAG: &str = "fb0123456789abcdef";

    fn cols(q: &str) -> Vec<String> {
        check(q, TAG).unwrap().columns
    }

    fn code(q: &str) -> &'static str {
        check(q, TAG).unwrap_err().code
    }

    #[test]
    fn return_items_are_named_and_counted() {
        assert_eq!(cols("MATCH (n) RETURN n"), ["n"]);
        assert_eq!(cols("MATCH (n) RETURN n.name AS name, count(*)"), ["name", "count(*)"]);
        assert_eq!(cols("MATCH (n) RETURN DISTINCT n.a, {k: [1, 2]} AS m ORDER BY n.a LIMIT 3"), ["n.a", "m"]);
        assert_eq!(cols("MATCH (a)-[*1..3]->(b) RETURN b.__id AS `the id`"), ["the id"]);
        assert_eq!(cols("MATCH (n) WITH n RETURN  n.x  +  1"), ["n.x + 1"]);
        // The last RETURN decides (UNION).
        assert_eq!(cols("MATCH (n:A) RETURN n.x AS x UNION MATCH (n:B) RETURN n.y AS x"), ["x"]);
        // Keywords as properties and map keys are not clauses.
        assert_eq!(cols("MATCH (n) WHERE n.set = 1 RETURN {create: n.delete} AS m"), ["m"]);
        assert_eq!(cols("MATCH (n) RETURN 'CREATE (x)' AS s, \"SET\" AS t // DELETE\n"), ["s", "t"]);
        assert_eq!(cols("MATCH (n) WHERE n.name = $name RETURN n /* MERGE */"), ["n"]);
    }

    #[test]
    fn write_clauses_and_calls_are_refused() {
        for q in [
            "CREATE (n:X) RETURN n",
            "MATCH (n) SET n.a = 1 RETURN n",
            "MATCH (n) DETACH DELETE n RETURN 1",
            "MATCH (n) DELETE n RETURN 1",
            "MERGE (n:X {a: 1}) RETURN n",
            "MATCH (n) REMOVE n.a RETURN n",
            "match (n) create (m) return m",
            "CALL db.labels() YIELD label RETURN label",
            "MATCH (n) FOREACH (x IN [1] | SET n.a = x) RETURN n",
        ] {
            assert_eq!(code(q), "cypher_read_only", "{q}");
        }
        assert_eq!(code("RETURN pg_catalog.pg_read_file('/etc/passwd')"), "cypher_invalid");
        assert_eq!(code("MATCH (n) RETURN n; MATCH (m) RETURN m"), "cypher_invalid");
        assert_eq!(code("RETURN $$x$$"), "cypher_invalid");
        assert_eq!(code(&format!("RETURN 1 AS ${TAG}$")), "cypher_invalid");
        assert_eq!(code("MATCH (n) RETURN *"), "cypher_invalid");
        assert_eq!(code("MATCH (n)"), "cypher_invalid");
        assert_eq!(code("MATCH (n RETURN n"), "cypher_invalid");
        assert_eq!(code("RETURN 'open"), "cypher_invalid");
        // A property call on a variable is not schema-qualified.
        assert_eq!(cols("MATCH (n) RETURN toUpper(n.name) AS u"), ["u"]);
    }

    #[test]
    fn many_columns_are_refused() {
        let items: Vec<String> = (0..=MAX_COLUMNS).map(|i| format!("{i} AS c{i}")).collect();
        assert_eq!(code(&format!("RETURN {}", items.join(", "))), "cypher_invalid");
    }
}
