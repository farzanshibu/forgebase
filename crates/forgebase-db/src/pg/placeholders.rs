//! Placeholder translation for PostgreSQL (ADR-0028 §7.3, decision 7).
//!
//! ForgeBase SQL is written with SQLite placeholders: `?N` (about 3 000 call
//! sites) and a few bare `?`. PostgreSQL only understands `$N`. This lexer
//! rewrites them while skipping everything that is not code: string literals
//! (`'…'` with `''`, `E'…'` with backslash escapes), quoted identifiers
//! (`"…"`), dollar-quoted bodies (`$$…$$`, `$tag$…$tag$`) and comments (`--`
//! to the end of the line, nested `/* … */`).
//!
//! Numbering follows SQLite: `?N` is parameter `N`; a bare `?` is one more
//! than the largest parameter number assigned so far. A `$N` already in the
//! text is kept and counts as assigned.

use std::collections::BTreeSet;

/// Highest parameter number PostgreSQL accepts in one statement.
pub const MAX_PARAMS: usize = 65_535;

/// SQL with `$N` placeholders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Translated {
    /// Rewritten SQL text.
    pub sql: String,
    /// Highest parameter number referenced (0 when there is none).
    pub max_param: usize,
    /// Every parameter number referenced, ascending.
    pub used: BTreeSet<usize>,
}

/// Why a statement's placeholders cannot be translated.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlaceholderError {
    /// `?0` or a number above [`MAX_PARAMS`].
    #[error("parameter number {0} is out of range (1 to 65535)")]
    OutOfRange(String),
}

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c >= 0x80
}

fn is_ident_char(c: u8) -> bool {
    is_ident_start(c) || c.is_ascii_digit() || c == b'$'
}

/// Rewrites `?N` and `?` to `$N`.
pub fn translate(sql: &str) -> Result<Translated, PlaceholderError> {
    let b = sql.as_bytes();
    let mut out = String::with_capacity(sql.len() + 8);
    let mut used = BTreeSet::new();
    let mut max_param = 0usize;
    // Start of the not-yet-copied part of `sql`.
    let mut copied = 0usize;
    let mut i = 0usize;

    let number = |text: &str, used: &mut BTreeSet<usize>, max: &mut usize| -> Result<usize, PlaceholderError> {
        let n: usize = text
            .parse()
            .ok()
            .filter(|n| (1..=MAX_PARAMS).contains(n))
            .ok_or_else(|| PlaceholderError::OutOfRange(text.to_owned()))?;
        used.insert(n);
        *max = (*max).max(n);
        Ok(n)
    };

    while i < b.len() {
        let c = b[i];
        match c {
            b'\'' => i = skip_quoted(b, i + 1, b'\'', false),
            b'"' => i = skip_quoted(b, i + 1, b'"', false),
            b'-' if b.get(i + 1) == Some(&b'-') => {
                i = memchr_from(b, i + 2, b'\n').map_or(b.len(), |p| p + 1);
            }
            b'/' if b.get(i + 1) == Some(&b'*') => i = skip_block_comment(b, i + 2),
            b'$' => {
                if b.get(i + 1).is_some_and(u8::is_ascii_digit) {
                    // Positional parameter already in PostgreSQL form.
                    let end = digits_end(b, i + 1);
                    number(&sql[i + 1..end], &mut used, &mut max_param)?;
                    i = end;
                } else if let Some(tag_end) = dollar_tag_end(b, i) {
                    let tag = &b[i..=tag_end];
                    i = find_sub(b, tag_end + 1, tag).map_or(b.len(), |p| p + tag.len());
                } else {
                    i += 1;
                }
            }
            b'?' => {
                out.push_str(&sql[copied..i]);
                let end = digits_end(b, i + 1);
                let n = if end > i + 1 {
                    number(&sql[i + 1..end], &mut used, &mut max_param)?
                } else {
                    let n = max_param + 1;
                    number(&n.to_string(), &mut used, &mut max_param)?
                };
                out.push('$');
                out.push_str(&n.to_string());
                i = end;
                copied = i;
            }
            c if is_ident_start(c) => {
                // `E'…'` / `e'…'` escape strings: backslash escapes are active.
                if (c == b'E' || c == b'e') && b.get(i + 1) == Some(&b'\'') {
                    i = skip_quoted(b, i + 2, b'\'', true);
                } else {
                    // A whole identifier or keyword, so `a$1` and `x?` inside
                    // names are never taken apart (`?` is not an identifier
                    // character, so it still ends the word).
                    i += 1;
                    while i < b.len() && is_ident_char(b[i]) {
                        i += 1;
                    }
                }
            }
            _ => i += 1,
        }
    }
    out.push_str(&sql[copied..]);
    Ok(Translated {
        sql: out,
        max_param,
        used,
    })
}

/// Position after the closing `quote` of a literal whose body starts at `i`.
/// A doubled quote is an escaped quote; with `backslash`, `\x` escapes `x`.
/// An unterminated literal runs to the end (the server reports it).
fn skip_quoted(b: &[u8], mut i: usize, quote: u8, backslash: bool) -> usize {
    while i < b.len() {
        let c = b[i];
        if backslash && c == b'\\' {
            i += 2;
        } else if c == quote {
            if b.get(i + 1) == Some(&quote) {
                i += 2;
            } else {
                return i + 1;
            }
        } else {
            i += 1;
        }
    }
    b.len()
}

/// Position after a (possibly nested) block comment whose body starts at `i`.
fn skip_block_comment(b: &[u8], mut i: usize) -> usize {
    let mut depth = 1usize;
    while i < b.len() {
        if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
            depth += 1;
            i += 2;
        } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
            depth -= 1;
            i += 2;
            if depth == 0 {
                return i;
            }
        } else {
            i += 1;
        }
    }
    b.len()
}

/// If a dollar-quote opening tag (`$$` or `$tag$`) starts at `i`, the index of
/// its closing `$`.
fn dollar_tag_end(b: &[u8], i: usize) -> Option<usize> {
    let mut j = i + 1;
    if b.get(j) == Some(&b'$') {
        return Some(j);
    }
    if !b.get(j).copied().is_some_and(is_ident_start) {
        return None;
    }
    while j < b.len() && (is_ident_start(b[j]) || b[j].is_ascii_digit()) {
        j += 1;
    }
    (b.get(j) == Some(&b'$')).then_some(j)
}

fn digits_end(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    i
}

fn memchr_from(b: &[u8], from: usize, needle: u8) -> Option<usize> {
    b.get(from..)?.iter().position(|&c| c == needle).map(|p| p + from)
}

fn find_sub(b: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    b.get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(sql: &str) -> String {
        translate(sql).unwrap().sql
    }

    #[test]
    fn numbered_and_bare_placeholders() {
        assert_eq!(
            t("SELECT * FROM a WHERE x = ?1 AND y = ?2"),
            "SELECT * FROM a WHERE x = $1 AND y = $2"
        );
        assert_eq!(t("VALUES (?, ?, ?)"), "VALUES ($1, $2, $3)");
        // SQLite: a bare ? is one more than the largest number so far.
        assert_eq!(t("?3, ?, ?1, ?"), "$3, $4, $1, $5");
        assert_eq!(t("?2 ?2 ?2"), "$2 $2 $2");
        assert_eq!(t("x=?1"), "x=$1");
        assert_eq!(t("?12)"), "$12)");
        let tr = translate("?3 ?1").unwrap();
        assert_eq!(tr.max_param, 3);
        assert_eq!(tr.used.into_iter().collect::<Vec<_>>(), vec![1, 3]);
    }

    #[test]
    fn strings_identifiers_and_comments_are_left_alone() {
        assert_eq!(t("SELECT '?1', ?1"), "SELECT '?1', $1");
        assert_eq!(t("SELECT 'it''s ?', ?"), "SELECT 'it''s ?', $1");
        assert_eq!(t(r"SELECT E'\' ?1', ?2"), r"SELECT E'\' ?1', $2");
        assert_eq!(t(r"SELECT e'a\\', ?"), r"SELECT e'a\\', $1");
        // A plain string does not treat backslash as an escape.
        assert_eq!(t(r"SELECT 'a\', ?"), r"SELECT 'a\', $1");
        assert_eq!(
            t(r#"SELECT "we?ird" FROM "t""?" WHERE c = ?"#),
            r#"SELECT "we?ird" FROM "t""?" WHERE c = $1"#
        );
        assert_eq!(t("SELECT 1 -- what ?1\n, ?1"), "SELECT 1 -- what ?1\n, $1");
        assert_eq!(t("SELECT 1 -- trailing ?"), "SELECT 1 -- trailing ?");
        assert_eq!(
            t("/* a /* nested ? */ still ?1 */ ?2"),
            "/* a /* nested ? */ still ?1 */ $2"
        );
        assert_eq!(t("SELECT 6 / ?1"), "SELECT 6 / $1");
        assert_eq!(t("SELECT 6 - ?1"), "SELECT 6 - $1");
    }

    #[test]
    fn dollar_quotes_and_native_parameters() {
        let f = "CREATE FUNCTION f() RETURNS text AS $$ SELECT '?1' || ? $$ LANGUAGE sql";
        assert_eq!(t(f), f);
        let tagged = "DO $body$ BEGIN PERFORM ?1; $x$ inner $x$; END $body$; SELECT ?1";
        assert_eq!(
            t(tagged),
            "DO $body$ BEGIN PERFORM ?1; $x$ inner $x$; END $body$; SELECT $1"
        );
        assert_eq!(t("SELECT $1, ?"), "SELECT $1, $2");
        assert_eq!(
            t("SELECT a$1, ?"),
            "SELECT a$1, $1",
            "$ inside an identifier is not a parameter"
        );
        // A lone $ that opens nothing.
        assert_eq!(t("SELECT '$' || ?1, $ 1"), "SELECT '$' || $1, $ 1");
    }

    #[test]
    fn unterminated_and_unicode_input_does_not_panic() {
        assert_eq!(t("SELECT 'open ?1"), "SELECT 'open ?1");
        assert_eq!(t("SELECT \"open ?1"), "SELECT \"open ?1");
        assert_eq!(t("/* open ?1"), "/* open ?1");
        assert_eq!(t("$$ open ?1"), "$$ open ?1");
        assert_eq!(t("SELECT 'é', ?1, ünï?"), "SELECT 'é', $1, ünï$2");
        assert_eq!(t(""), "");
        assert_eq!(t("?"), "$1");
    }

    #[test]
    fn out_of_range_numbers_are_refused() {
        assert!(matches!(translate("?0"), Err(PlaceholderError::OutOfRange(_))));
        assert!(matches!(translate("?65536"), Err(PlaceholderError::OutOfRange(_))));
        assert!(matches!(
            translate("?99999999999999999999999"),
            Err(PlaceholderError::OutOfRange(_))
        ));
        assert_eq!(t("?65535"), "$65535");
    }
}
