//! Filter language: lexer, parser and syntax tree (ADR-0003 §5).
//!
//! ```text
//! filter     := or
//! or         := and ( "||" and )*
//! and        := unary ( "&&" unary )*
//! unary      := "!" unary | "(" or ")" | comparison
//! comparison := operand op operand
//! op         := ["?"] ( "=" | "!=" | ">" | ">=" | "<" | "<=" | "~" | "!~" )
//! operand    := path | string | number | "true" | "false" | "null" | "{:" name "}" | "@now"
//! path       := ident ( "." ( ident | digits ) )* [ ":" ( "length" | "lower" ) ]
//! ident      := [A-Za-z_][A-Za-z0-9_]*
//! string     := '...' | "..."      (backslash escapes \' \" \\ \n \t)
//! number     := -?digits[.digits][(e|E)[+-]digits]
//! ```
//!
//! `?`-prefixed operators mean "any element matches" on multi-value fields;
//! the plain operators require every element to match. `~` is "contains"
//! (case-insensitive for ASCII, the value is matched literally: `%` and `_`
//! have no special meaning). Inputs are bounded: 4 KiB, 512 tokens, nesting
//! depth 32.

use std::fmt;

use crate::QueryError;

/// Maximum filter length in bytes.
pub const MAX_FILTER_LEN: usize = 4096;
/// Maximum number of tokens.
pub const MAX_TOKENS: usize = 512;
/// Maximum nesting depth of parentheses and `!`.
pub const MAX_DEPTH: usize = 32;

/// A boolean filter expression.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// All must hold.
    And(Vec<Expr>),
    /// At least one must hold.
    Or(Vec<Expr>),
    /// Negation.
    Not(Box<Expr>),
    /// A comparison.
    Compare(Compare),
    /// A constant (used by authorisation predicates: `false` denies all rows).
    Const(bool),
}

/// `left op right`.
#[derive(Debug, Clone, PartialEq)]
pub struct Compare {
    /// Left operand.
    pub left: Operand,
    /// Operator.
    pub op: Op,
    /// `?` prefix: any element of a multi-value operand matches.
    pub any: bool,
    /// Right operand.
    pub right: Operand,
}

/// Comparison operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// `=` (NULL-safe).
    Eq,
    /// `!=` (NULL-safe).
    Ne,
    /// `>`.
    Gt,
    /// `>=`.
    Ge,
    /// `<`.
    Lt,
    /// `<=`.
    Le,
    /// `~` contains.
    Like,
    /// `!~` does not contain.
    NotLike,
}

impl Op {
    /// Whether this is a range operator.
    pub fn is_range(self) -> bool {
        matches!(self, Op::Gt | Op::Ge | Op::Lt | Op::Le)
    }

    /// The operator with its operands swapped (`a < b` == `b > a`).
    pub fn flipped(self) -> Op {
        match self {
            Op::Gt => Op::Lt,
            Op::Ge => Op::Le,
            Op::Lt => Op::Gt,
            Op::Le => Op::Ge,
            other => other,
        }
    }
}

/// An operand.
#[derive(Debug, Clone, PartialEq)]
pub enum Operand {
    /// A constant.
    Literal(Literal),
    /// A field path.
    Path(FieldPath),
    /// A named parameter `{:name}`, bound from the caller's parameter map.
    Param(String),
    /// `@now`: the current timestamp.
    Now,
}

/// A constant.
#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    /// `null`.
    Null,
    /// `true` / `false`.
    Bool(bool),
    /// Integer.
    Int(i64),
    /// Float.
    Float(f64),
    /// String.
    Text(String),
}

/// A dotted path into a record: `author.name`, `metadata.specs.ram`, `tags.0`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FieldPath {
    /// Segments; the first is always a name.
    pub segments: Vec<PathSeg>,
    /// Optional modifier.
    pub modifier: Option<Modifier>,
}

/// One path segment.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PathSeg {
    /// A name (`[A-Za-z_][A-Za-z0-9_]*`).
    Name(String),
    /// An array index.
    Index(u32),
}

/// Path modifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Modifier {
    /// `:length`: number of elements (arrays) or characters (text).
    Length,
    /// `:lower`: lower-cased text.
    Lower,
}

impl fmt::Display for FieldPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, s) in self.segments.iter().enumerate() {
            if i > 0 {
                f.write_str(".")?;
            }
            match s {
                PathSeg::Name(n) => f.write_str(n)?,
                PathSeg::Index(n) => write!(f, "{n}")?,
            }
        }
        match self.modifier {
            Some(Modifier::Length) => f.write_str(":length"),
            Some(Modifier::Lower) => f.write_str(":lower"),
            None => Ok(()),
        }
    }
}

impl fmt::Display for Literal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Literal::Null => f.write_str("null"),
            Literal::Bool(b) => write!(f, "{b}"),
            Literal::Int(i) => write!(f, "{i}"),
            Literal::Float(x) => write!(f, "{x:?}"),
            Literal::Text(s) => {
                f.write_str("'")?;
                for c in s.chars() {
                    match c {
                        '\\' => f.write_str("\\\\")?,
                        '\'' => f.write_str("\\'")?,
                        '\n' => f.write_str("\\n")?,
                        '\t' => f.write_str("\\t")?,
                        other => write!(f, "{other}")?,
                    }
                }
                f.write_str("'")
            }
        }
    }
}

impl fmt::Display for Operand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Operand::Literal(l) => write!(f, "{l}"),
            Operand::Path(p) => write!(f, "{p}"),
            Operand::Param(n) => write!(f, "{{:{n}}}"),
            Operand::Now => f.write_str("@now"),
        }
    }
}

impl fmt::Display for Op {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Op::Eq => "=",
            Op::Ne => "!=",
            Op::Gt => ">",
            Op::Ge => ">=",
            Op::Lt => "<",
            Op::Le => "<=",
            Op::Like => "~",
            Op::NotLike => "!~",
        })
    }
}

/// Renders the expression back to filter syntax (parses to the same tree).
impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Expr::Const(true) => f.write_str("1 = 1"),
            Expr::Const(false) => f.write_str("1 = 0"),
            Expr::Not(e) => write!(f, "!({e})"),
            Expr::And(items) | Expr::Or(items) => {
                let sep = if matches!(self, Expr::And(_)) { " && " } else { " || " };
                for (i, e) in items.iter().enumerate() {
                    if i > 0 {
                        f.write_str(sep)?;
                    }
                    write!(f, "({e})")?;
                }
                Ok(())
            }
            Expr::Compare(c) => write!(f, "{} {}{} {}", c.left, if c.any { "?" } else { "" }, c.op, c.right),
        }
    }
}

impl Expr {
    /// Calls `f` on every field path in the expression (mutable), e.g. to
    /// follow a field rename.
    pub fn for_each_path_mut(&mut self, f: &mut dyn FnMut(&mut FieldPath)) {
        match self {
            Expr::And(items) | Expr::Or(items) => items.iter_mut().for_each(|e| e.for_each_path_mut(f)),
            Expr::Not(e) => e.for_each_path_mut(f),
            Expr::Compare(c) => {
                for o in [&mut c.left, &mut c.right] {
                    if let Operand::Path(p) = o {
                        f(p);
                    }
                }
            }
            Expr::Const(_) => {}
        }
    }
}

impl FieldPath {
    /// Parses a standalone path (sort keys, index keys, projections).
    pub fn parse(s: &str) -> Result<Self, QueryError> {
        let tokens = lex(s)?;
        match tokens.as_slice() {
            [
                Tok {
                    kind: TokKind::Path(p), ..
                },
            ] => Ok(p.clone()),
            _ => Err(QueryError::syntax(0, format!("{s:?} is not a field path"))),
        }
    }

    /// First segment's name.
    pub fn root(&self) -> &str {
        match self.segments.first() {
            Some(PathSeg::Name(n)) => n,
            _ => "",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum TokKind {
    LParen,
    RParen,
    And,
    Or,
    Not,
    Op(Op, bool),
    Operand(Operand),
    Path(FieldPath),
}

#[derive(Debug, Clone, PartialEq)]
struct Tok {
    kind: TokKind,
    pos: usize,
}

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_'
}
fn is_ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

fn lex(input: &str) -> Result<Vec<Tok>, QueryError> {
    if input.len() > MAX_FILTER_LEN {
        return Err(QueryError::TooComplex(format!(
            "filter is longer than {MAX_FILTER_LEN} bytes"
        )));
    }
    let b = input.as_bytes();
    let mut i = 0;
    let mut out: Vec<Tok> = Vec::new();
    while i < b.len() {
        if out.len() > MAX_TOKENS {
            return Err(QueryError::TooComplex(format!(
                "filter has more than {MAX_TOKENS} tokens"
            )));
        }
        let c = b[i];
        let pos = i;
        let push = |out: &mut Vec<Tok>, kind| out.push(Tok { kind, pos });
        match c {
            b' ' | b'\t' | b'\n' | b'\r' => i += 1,
            b'(' => {
                push(&mut out, TokKind::LParen);
                i += 1;
            }
            b')' => {
                push(&mut out, TokKind::RParen);
                i += 1;
            }
            b'&' if b.get(i + 1) == Some(&b'&') => {
                push(&mut out, TokKind::And);
                i += 2;
            }
            b'|' if b.get(i + 1) == Some(&b'|') => {
                push(&mut out, TokKind::Or);
                i += 2;
            }
            b'?' | b'=' | b'!' | b'>' | b'<' | b'~' => {
                let any = c == b'?';
                let j = if any { i + 1 } else { i };
                let (op, len) = match (b.get(j), b.get(j + 1)) {
                    (Some(b'!'), Some(b'=')) => (Some(Op::Ne), 2),
                    (Some(b'!'), Some(b'~')) => (Some(Op::NotLike), 2),
                    (Some(b'>'), Some(b'=')) => (Some(Op::Ge), 2),
                    (Some(b'<'), Some(b'=')) => (Some(Op::Le), 2),
                    (Some(b'='), _) => (Some(Op::Eq), 1),
                    (Some(b'>'), _) => (Some(Op::Gt), 1),
                    (Some(b'<'), _) => (Some(Op::Lt), 1),
                    (Some(b'~'), _) => (Some(Op::Like), 1),
                    (Some(b'!'), _) if !any => (None, 1),
                    _ => return Err(QueryError::syntax(pos, "unknown operator")),
                };
                match op {
                    Some(op) => push(&mut out, TokKind::Op(op, any)),
                    None => push(&mut out, TokKind::Not),
                }
                i = j + len;
            }
            b'\'' | b'"' => {
                let quote = c;
                let mut s = String::new();
                let mut j = i + 1;
                let mut closed = false;
                // Work on chars so multi-byte UTF-8 stays intact.
                let rest = &input[j..];
                let mut chars = rest.char_indices();
                while let Some((k, ch)) = chars.next() {
                    if ch == '\\' {
                        match chars.next() {
                            Some((_, 'n')) => s.push('\n'),
                            Some((_, 't')) => s.push('\t'),
                            Some((_, other)) => s.push(other),
                            None => break,
                        }
                    } else if ch as u32 == u32::from(quote) {
                        j += k + 1;
                        closed = true;
                        break;
                    } else {
                        s.push(ch);
                    }
                }
                if !closed {
                    return Err(QueryError::syntax(pos, "unterminated string"));
                }
                if s.contains('\0') {
                    return Err(QueryError::syntax(pos, "strings cannot contain NUL"));
                }
                push(&mut out, TokKind::Operand(Operand::Literal(Literal::Text(s))));
                i = j;
            }
            b'-' | b'0'..=b'9' => {
                let mut j = i + usize::from(c == b'-');
                let digits_start = j;
                while j < b.len() && b[j].is_ascii_digit() {
                    j += 1;
                }
                if j == digits_start {
                    return Err(QueryError::syntax(pos, "expected a number"));
                }
                let mut float = false;
                if j < b.len() && b[j] == b'.' && b.get(j + 1).is_some_and(u8::is_ascii_digit) {
                    float = true;
                    j += 1;
                    while j < b.len() && b[j].is_ascii_digit() {
                        j += 1;
                    }
                }
                if j < b.len() && (b[j] == b'e' || b[j] == b'E') {
                    let mut k = j + 1;
                    if k < b.len() && (b[k] == b'+' || b[k] == b'-') {
                        k += 1;
                    }
                    if k < b.len() && b[k].is_ascii_digit() {
                        float = true;
                        j = k;
                        while j < b.len() && b[j].is_ascii_digit() {
                            j += 1;
                        }
                    }
                }
                if j < b.len() && is_ident(b[j]) {
                    return Err(QueryError::syntax(pos, "malformed number"));
                }
                let text = &input[i..j];
                let lit = if float {
                    let f: f64 = text.parse().map_err(|_| QueryError::syntax(pos, "malformed number"))?;
                    if !f.is_finite() {
                        return Err(QueryError::syntax(pos, "number out of range"));
                    }
                    Literal::Float(f)
                } else {
                    match text.parse::<i64>() {
                        Ok(n) => Literal::Int(n),
                        Err(_) => return Err(QueryError::syntax(pos, "integer out of range")),
                    }
                };
                push(&mut out, TokKind::Operand(Operand::Literal(lit)));
                i = j;
            }
            b'{' => {
                if b.get(i + 1) != Some(&b':') {
                    return Err(QueryError::syntax(pos, "expected {:name}"));
                }
                let start = i + 2;
                let mut j = start;
                while j < b.len() && is_ident(b[j]) {
                    j += 1;
                }
                if j == start || !is_ident_start(b[start]) || b.get(j) != Some(&b'}') {
                    return Err(QueryError::syntax(pos, "expected {:name}"));
                }
                push(&mut out, TokKind::Operand(Operand::Param(input[start..j].to_owned())));
                i = j + 1;
            }
            b'@' => {
                let start = i + 1;
                let mut j = start;
                while j < b.len() && is_ident(b[j]) {
                    j += 1;
                }
                match &input[start..j] {
                    "now" => push(&mut out, TokKind::Operand(Operand::Now)),
                    other => return Err(QueryError::syntax(pos, format!("unknown macro @{other}"))),
                }
                i = j;
            }
            c if is_ident_start(c) => {
                let mut segments = Vec::new();
                let mut j = i;
                loop {
                    let start = j;
                    if j < b.len() && is_ident_start(b[j]) {
                        while j < b.len() && is_ident(b[j]) {
                            j += 1;
                        }
                        let name = &input[start..j];
                        if name.len() > 64 {
                            return Err(QueryError::syntax(start, "name too long"));
                        }
                        segments.push(PathSeg::Name(name.to_owned()));
                    } else if !segments.is_empty() && j < b.len() && b[j].is_ascii_digit() {
                        while j < b.len() && b[j].is_ascii_digit() {
                            j += 1;
                        }
                        let n = input[start..j]
                            .parse::<u32>()
                            .map_err(|_| QueryError::syntax(start, "index out of range"))?;
                        segments.push(PathSeg::Index(n));
                    } else {
                        return Err(QueryError::syntax(start, "expected a name after '.'"));
                    }
                    if j < b.len() && b[j] == b'.' {
                        j += 1;
                        continue;
                    }
                    break;
                }
                if segments.len() > 16 {
                    return Err(QueryError::TooComplex("path has more than 16 segments".into()));
                }
                let mut modifier = None;
                if j < b.len() && b[j] == b':' {
                    let start = j + 1;
                    let mut k = start;
                    while k < b.len() && is_ident(b[k]) {
                        k += 1;
                    }
                    modifier = Some(match &input[start..k] {
                        "length" => Modifier::Length,
                        "lower" => Modifier::Lower,
                        other => return Err(QueryError::syntax(j, format!("unknown modifier :{other}"))),
                    });
                    j = k;
                }
                let kind = if segments.len() == 1 && modifier.is_none() {
                    match &segments[0] {
                        PathSeg::Name(n) if n == "true" => TokKind::Operand(Operand::Literal(Literal::Bool(true))),
                        PathSeg::Name(n) if n == "false" => TokKind::Operand(Operand::Literal(Literal::Bool(false))),
                        PathSeg::Name(n) if n == "null" => TokKind::Operand(Operand::Literal(Literal::Null)),
                        _ => TokKind::Path(FieldPath { segments, modifier }),
                    }
                } else {
                    TokKind::Path(FieldPath { segments, modifier })
                };
                push(&mut out, kind);
                i = j;
            }
            _ => {
                let ch = input[i..].chars().next().unwrap_or('?');
                return Err(QueryError::syntax(pos, format!("unexpected character {ch:?}")));
            }
        }
    }
    Ok(out)
}

/// Parses a filter. An empty or blank filter is `Ok(None)`.
pub fn parse_filter(input: &str) -> Result<Option<Expr>, QueryError> {
    let tokens = lex(input)?;
    if tokens.is_empty() {
        return Ok(None);
    }
    let mut p = Parser { tokens, i: 0, depth: 0 };
    let e = p.or()?;
    if let Some(t) = p.tokens.get(p.i) {
        return Err(QueryError::syntax(t.pos, "unexpected token after expression"));
    }
    Ok(Some(e))
}

struct Parser {
    tokens: Vec<Tok>,
    i: usize,
    depth: usize,
}

impl Parser {
    fn peek(&self) -> Option<&TokKind> {
        self.tokens.get(self.i).map(|t| &t.kind)
    }

    fn pos(&self) -> usize {
        self.tokens
            .get(self.i)
            .map_or_else(|| self.tokens.last().map_or(0, |t| t.pos), |t| t.pos)
    }

    fn enter(&mut self) -> Result<(), QueryError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(QueryError::TooComplex(format!("filter nests deeper than {MAX_DEPTH}")));
        }
        Ok(())
    }

    fn or(&mut self) -> Result<Expr, QueryError> {
        let mut items = vec![self.and()?];
        while self.peek() == Some(&TokKind::Or) {
            self.i += 1;
            items.push(self.and()?);
        }
        Ok(if items.len() == 1 {
            items.remove(0)
        } else {
            Expr::Or(items)
        })
    }

    fn and(&mut self) -> Result<Expr, QueryError> {
        let mut items = vec![self.unary()?];
        while self.peek() == Some(&TokKind::And) {
            self.i += 1;
            items.push(self.unary()?);
        }
        Ok(if items.len() == 1 {
            items.remove(0)
        } else {
            Expr::And(items)
        })
    }

    fn unary(&mut self) -> Result<Expr, QueryError> {
        match self.peek() {
            Some(TokKind::Not) => {
                self.i += 1;
                self.enter()?;
                let e = self.unary()?;
                self.depth -= 1;
                Ok(Expr::Not(Box::new(e)))
            }
            Some(TokKind::LParen) => {
                self.i += 1;
                self.enter()?;
                let e = self.or()?;
                self.depth -= 1;
                if self.peek() != Some(&TokKind::RParen) {
                    return Err(QueryError::syntax(self.pos(), "expected ')'"));
                }
                self.i += 1;
                Ok(e)
            }
            _ => self.comparison(),
        }
    }

    fn operand(&mut self) -> Result<Operand, QueryError> {
        let pos = self.pos();
        match self.tokens.get(self.i).map(|t| t.kind.clone()) {
            Some(TokKind::Operand(o)) => {
                self.i += 1;
                Ok(o)
            }
            Some(TokKind::Path(p)) => {
                self.i += 1;
                Ok(Operand::Path(p))
            }
            _ => Err(QueryError::syntax(pos, "expected a field, value or parameter")),
        }
    }

    fn comparison(&mut self) -> Result<Expr, QueryError> {
        let left = self.operand()?;
        let pos = self.pos();
        let (op, any) = match self.peek() {
            Some(TokKind::Op(op, any)) => (*op, *any),
            _ => return Err(QueryError::syntax(pos, "expected a comparison operator")),
        };
        self.i += 1;
        let right = self.operand()?;
        Ok(Expr::Compare(Compare { left, op, any, right }))
    }
}

/// Direction of a sort key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SortKey {
    /// Field path.
    pub path: FieldPath,
    /// Descending.
    pub desc: bool,
}

/// Parses `-created,name,+price` into sort keys (at most 8).
pub fn parse_sort(input: &str) -> Result<Vec<SortKey>, QueryError> {
    let mut keys = Vec::new();
    for part in input.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (desc, rest) = match part.as_bytes()[0] {
            b'-' => (true, &part[1..]),
            b'+' => (false, &part[1..]),
            _ => (false, part),
        };
        keys.push(SortKey {
            path: FieldPath::parse(rest.trim())?,
            desc,
        });
    }
    if keys.len() > 8 {
        return Err(QueryError::TooComplex("at most 8 sort keys".into()));
    }
    Ok(keys)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(s: &str) -> Operand {
        Operand::Path(FieldPath::parse(s).unwrap())
    }

    #[test]
    fn parses_precedence_and_operators() {
        let e = parse_filter("a = 1 || b ?~ 'x' && !(c.d >= -2.5e1)").unwrap().unwrap();
        let Expr::Or(items) = e else { panic!("or") };
        assert_eq!(items.len(), 2);
        let Expr::And(and) = &items[1] else { panic!("and") };
        assert_eq!(
            and[0],
            Expr::Compare(Compare {
                left: path("b"),
                op: Op::Like,
                any: true,
                right: Operand::Literal(Literal::Text("x".into())),
            })
        );
        assert!(matches!(&and[1], Expr::Not(_)));
    }

    #[test]
    fn literals_params_macros() {
        let e = parse_filter(r#"a != null && b = true && c = "q\"uote" && d = {:p1} && e < @now && f = 7"#)
            .unwrap()
            .unwrap();
        let Expr::And(items) = e else { panic!() };
        assert_eq!(items.len(), 6);
        let Expr::Compare(c) = &items[2] else { panic!() };
        assert_eq!(c.right, Operand::Literal(Literal::Text("q\"uote".into())));
        let Expr::Compare(c) = &items[3] else { panic!() };
        assert_eq!(c.right, Operand::Param("p1".into()));
    }

    #[test]
    fn paths_with_indexes_and_modifiers() {
        let p = FieldPath::parse("tags.0.name:lower").unwrap();
        assert_eq!(p.segments.len(), 3);
        assert_eq!(p.segments[1], PathSeg::Index(0));
        assert_eq!(p.modifier, Some(Modifier::Lower));
        assert_eq!(p.to_string(), "tags.0.name:lower");
        assert!(FieldPath::parse("0abc").is_err());
    }

    #[test]
    fn rejects_garbage_and_injection_shapes() {
        for bad in [
            "name = 'x'; DROP TABLE t; --",
            "name = 'x' OR 1=1",
            "name = 'unterminated",
            "a.\"b\" = 1",
            "a = ",
            "(a = 1",
            "a = 1)",
            "a == 1",
            "a = 1 --comment",
            "a = 1 /* x */",
            "a = @request.auth.id",
            "a = {:bad name}",
            "a = 12abc",
            "a = 99999999999999999999",
            "a.b..c = 1",
            "a:unknown = 1",
            "a = `x`",
            "a = [1]",
        ] {
            assert!(parse_filter(bad).is_err(), "{bad} should not parse");
        }
    }

    #[test]
    fn limits_are_enforced() {
        let deep = format!("{}a = 1{}", "(".repeat(40), ")".repeat(40));
        assert!(matches!(parse_filter(&deep), Err(QueryError::TooComplex(_))));
        let long = "a = 1 && ".repeat(600) + "a = 1";
        assert!(matches!(parse_filter(&long), Err(QueryError::TooComplex(_))));
        assert_eq!(parse_filter("   ").unwrap(), None);
    }

    #[test]
    fn display_round_trips() {
        for src in [
            r"a = 1 || b ?~ 'x\'y' && !(c.d >= -2.5)",
            "tags:length > 2 && t != null && u = {:p} && v < @now && w = true",
        ] {
            let e = parse_filter(src).unwrap().unwrap();
            let again = parse_filter(&e.to_string()).unwrap().unwrap();
            assert_eq!(e, again, "{}", e);
        }
    }

    #[test]
    fn sort_keys() {
        let keys = parse_sort("-created, name ,+metadata.price").unwrap();
        assert_eq!(keys.len(), 3);
        assert!(keys[0].desc);
        assert!(!keys[2].desc);
        assert!(parse_sort("name; DROP TABLE x").is_err());
    }
}
