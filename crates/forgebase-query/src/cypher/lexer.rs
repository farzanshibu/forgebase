//! The openCypher tokenizer: words, backquoted names, strings, numbers,
//! parameters and punctuation, with byte spans. Comments (`//`, `/* */`)
//! and whitespace are skipped.

use super::CypherError;
use super::ast::Span;

/// A token kind.
#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    /// An identifier or keyword, as written.
    Word(String),
    /// A backquoted name, unquoted.
    Quoted(String),
    /// A string literal, unescaped.
    Str(String),
    /// An integer literal.
    Int(i64),
    /// A float literal.
    Float(f64),
    /// `$name` or `$0`.
    Param(String),
    /// One-character punctuation: `( ) [ ] { } , . : | ; * + - / % ^ = < > !`.
    Punct(char),
    /// `<=`.
    Le,
    /// `>=`.
    Ge,
    /// `<>` (or `!=`).
    Ne,
    /// `=~`.
    RegexMatch,
    /// `..`.
    DotDot,
    /// End of input.
    Eof,
}

/// A token and where it is.
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    /// The kind.
    pub tok: Tok,
    /// Its bytes.
    pub span: Span,
}

impl Token {
    /// Whether this is the keyword `kw` (case-insensitive; not backquoted).
    pub fn is_kw(&self, kw: &str) -> bool {
        matches!(&self.tok, Tok::Word(w) if w.eq_ignore_ascii_case(kw))
    }

    /// Whether this is punctuation `c`.
    pub fn is(&self, c: char) -> bool {
        self.tok == Tok::Punct(c)
    }
}

/// Tokenizes `q`; the last token is [`Tok::Eof`].
pub fn lex(q: &str) -> Result<Vec<Token>, CypherError> {
    let chars: Vec<(usize, char)> = q.char_indices().collect();
    let at = |i: usize| chars.get(i).map(|&(_, c)| c);
    let pos = |i: usize| chars.get(i).map_or(q.len(), |&(p, _)| p);
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
                    None => return Err(CypherError::invalid(q, start, "unterminated comment")),
                    Some('*') if at(i + 1) == Some('/') => {
                        i += 2;
                        break;
                    }
                    Some(_) => i += 1,
                }
            }
            continue;
        }
        let push = |out: &mut Vec<Token>, tok: Tok, end: usize| {
            out.push(Token {
                tok,
                span: Span::new(start, end),
            });
        };
        // Strings.
        if c == '\'' || c == '"' {
            let mut s = String::new();
            i += 1;
            loop {
                match at(i) {
                    None => return Err(CypherError::invalid(q, start, "unterminated string")),
                    Some(d) if d == c => {
                        i += 1;
                        break;
                    }
                    Some('\\') => {
                        let esc = at(i + 1).ok_or_else(|| CypherError::invalid(q, start, "unterminated string"))?;
                        i += 2;
                        match esc {
                            '\\' => s.push('\\'),
                            '\'' => s.push('\''),
                            '"' => s.push('"'),
                            'n' => s.push('\n'),
                            't' => s.push('\t'),
                            'r' => s.push('\r'),
                            'b' => s.push('\u{8}'),
                            'f' => s.push('\u{c}'),
                            'u' | 'U' => {
                                let n = if esc == 'u' { 4 } else { 8 };
                                let hex: String = (0..n).filter_map(|k| at(i + k)).collect();
                                let cp = (hex.chars().count() == n)
                                    .then(|| u32::from_str_radix(&hex, 16).ok())
                                    .flatten()
                                    .and_then(char::from_u32)
                                    .ok_or_else(|| CypherError::invalid(q, pos(i - 2), "invalid \\u escape"))?;
                                s.push(cp);
                                i += n;
                            }
                            other => {
                                return Err(CypherError::invalid(
                                    q,
                                    pos(i - 2),
                                    format_args!("unknown escape \\{other}"),
                                ));
                            }
                        }
                    }
                    Some(d) => {
                        s.push(d);
                        i += 1;
                    }
                }
            }
            push(&mut out, Tok::Str(s), pos(i));
            continue;
        }
        // Backquoted names.
        if c == '`' {
            let mut s = String::new();
            i += 1;
            loop {
                match at(i) {
                    None => return Err(CypherError::invalid(q, start, "unterminated backquoted name")),
                    Some('`') if at(i + 1) == Some('`') => {
                        s.push('`');
                        i += 2;
                    }
                    Some('`') => {
                        i += 1;
                        break;
                    }
                    Some(d) => {
                        s.push(d);
                        i += 1;
                    }
                }
            }
            if s.is_empty() {
                return Err(CypherError::invalid(q, start, "empty backquoted name"));
            }
            push(&mut out, Tok::Quoted(s), pos(i));
            continue;
        }
        // Parameters.
        if c == '$' {
            let mut j = i + 1;
            if at(j) == Some('`') {
                return Err(CypherError::unsupported(q, start, "a backquoted parameter name"));
            }
            while at(j).is_some_and(|c| c.is_alphanumeric() || c == '_') {
                j += 1;
            }
            if j == i + 1 {
                return Err(CypherError::invalid(q, start, "a parameter needs a name ($name)"));
            }
            let name: String = chars[i + 1..j].iter().map(|&(_, c)| c).collect();
            i = j;
            push(&mut out, Tok::Param(name), pos(i));
            continue;
        }
        // Numbers.
        if c.is_ascii_digit() {
            let mut j = i;
            while at(j).is_some_and(|c| c.is_ascii_digit()) {
                j += 1;
            }
            if at(j).is_some_and(|c| c == 'x' || c == 'X' || c == 'o') && j == i + 1 && c == '0' {
                return Err(CypherError::unsupported(q, start, "a hexadecimal or octal number"));
            }
            let mut float = false;
            if at(j) == Some('.') && at(j + 1).is_some_and(|c| c.is_ascii_digit()) {
                float = true;
                j += 1;
                while at(j).is_some_and(|c| c.is_ascii_digit()) {
                    j += 1;
                }
            }
            if at(j).is_some_and(|c| c == 'e' || c == 'E') {
                let mut k = j + 1;
                if at(k).is_some_and(|c| c == '+' || c == '-') {
                    k += 1;
                }
                if at(k).is_some_and(|c| c.is_ascii_digit()) {
                    float = true;
                    j = k;
                    while at(j).is_some_and(|c| c.is_ascii_digit()) {
                        j += 1;
                    }
                }
            }
            if at(j).is_some_and(|c| c.is_alphabetic() || c == '_') {
                return Err(CypherError::invalid(q, start, "a number runs into a name"));
            }
            let text = &q[start..pos(j)];
            let tok = if float {
                Tok::Float(
                    text.parse()
                        .map_err(|_| CypherError::invalid(q, start, "invalid number"))?,
                )
            } else {
                Tok::Int(
                    text.parse()
                        .map_err(|_| CypherError::invalid(q, start, "integer out of range"))?,
                )
            };
            i = j;
            push(&mut out, tok, pos(i));
            continue;
        }
        // Words.
        if c.is_alphabetic() || c == '_' {
            let mut j = i;
            while at(j).is_some_and(|c| c.is_alphanumeric() || c == '_') {
                j += 1;
            }
            let w = q[start..pos(j)].to_owned();
            i = j;
            push(&mut out, Tok::Word(w), pos(i));
            continue;
        }
        // Two-character operators.
        let two = |a: char, b: char| c == a && at(i + 1) == Some(b);
        let tok2 = if two('<', '=') {
            Some(Tok::Le)
        } else if two('>', '=') {
            Some(Tok::Ge)
        } else if two('<', '>') || two('!', '=') {
            Some(Tok::Ne)
        } else if two('=', '~') {
            Some(Tok::RegexMatch)
        } else if two('.', '.') {
            Some(Tok::DotDot)
        } else {
            None
        };
        if let Some(t) = tok2 {
            i += 2;
            push(&mut out, t, pos(i));
            continue;
        }
        if "()[]{},.:|;*+-/%^=<>!".contains(c) {
            i += 1;
            push(&mut out, Tok::Punct(c), pos(i));
            continue;
        }
        return Err(CypherError::invalid(
            q,
            start,
            format_args!("unexpected character {c:?}"),
        ));
    }
    out.push(Token {
        tok: Tok::Eof,
        span: Span::new(q.len(), q.len()),
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(q: &str) -> Vec<Tok> {
        lex(q).unwrap().into_iter().map(|t| t.tok).collect()
    }

    #[test]
    fn tokens() {
        assert_eq!(
            kinds("MATCH (n:`My Label` {a: 'x\\'y'})-[*1..3]->() // c\nRETURN n.a <> $p, 1.5e2"),
            vec![
                Tok::Word("MATCH".into()),
                Tok::Punct('('),
                Tok::Word("n".into()),
                Tok::Punct(':'),
                Tok::Quoted("My Label".into()),
                Tok::Punct('{'),
                Tok::Word("a".into()),
                Tok::Punct(':'),
                Tok::Str("x'y".into()),
                Tok::Punct('}'),
                Tok::Punct(')'),
                Tok::Punct('-'),
                Tok::Punct('['),
                Tok::Punct('*'),
                Tok::Int(1),
                Tok::DotDot,
                Tok::Int(3),
                Tok::Punct(']'),
                Tok::Punct('-'),
                Tok::Punct('>'),
                Tok::Punct('('),
                Tok::Punct(')'),
                Tok::Word("RETURN".into()),
                Tok::Word("n".into()),
                Tok::Punct('.'),
                Tok::Word("a".into()),
                Tok::Ne,
                Tok::Param("p".into()),
                Tok::Punct(','),
                Tok::Float(150.0),
                Tok::Eof,
            ]
        );
        assert_eq!(kinds("'\\u00e9'"), vec![Tok::Str("é".into()), Tok::Eof]);
        assert_eq!(kinds("/* x */ 7"), vec![Tok::Int(7), Tok::Eof]);
    }

    #[test]
    fn errors_have_positions() {
        let e = lex("RETURN\n  'abc").unwrap_err();
        assert_eq!((e.code, e.line, e.column), ("cypher_invalid", 2, 3));
        assert_eq!(lex("RETURN #").unwrap_err().column, 8);
        assert_eq!(lex("RETURN 99999999999999999999").unwrap_err().code, "cypher_invalid");
    }
}
