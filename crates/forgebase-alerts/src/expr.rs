//! Alert expressions: a PromQL subset (ADR-0035 §3).
//!
//! ```text
//! expr     = term { ("+" | "-") term }
//! term     = unary { ("*" | "/" | "%") unary }
//! unary    = "-" unary | primary
//! primary  = number | "(" expr ")" | aggregation | call | selector
//! aggregation = ("sum" | "avg" | "min" | "max" | "count") [grouping] "(" expr ")" [grouping]
//! grouping = ("by" | "without") "(" [label { "," label }] ")"
//! call     = rate | increase | delta | avg_over_time | min_over_time | max_over_time "(" range ")"
//!          | histogram_quantile "(" number "," expr ")"
//!          | abs | absent "(" expr ")" | clamp_min | clamp_max "(" expr "," number ")"
//!          | time "(" ")"
//! selector = metric [ "{" matcher { "," matcher } "}" ]        (an instant vector)
//! range    = selector "[" duration "]"                          (5m, 30s, 1h, 1d)
//! matcher  = label ("=" | "!=" | "=~" | "!~") string            ('…' or "…")
//! ```
//!
//! Metric names may contain `:`; `forgebase:*` names are the built-in
//! signals (queues, disk, backups, SLOs). Binary operators between two
//! vectors match samples with the same labels (metric name dropped).
//! Arithmetic drops the metric name, as in Prometheus.

use std::collections::BTreeSet;
use std::fmt;

use regex::Regex;

use crate::samples::{History, Labels, RangeSeries, Sample, Snapshot};

/// A parse or type error, with what to write instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExprError(pub String);

impl fmt::Display for ExprError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ExprError {}

fn err<T>(msg: impl Into<String>) -> Result<T, ExprError> {
    Err(ExprError(msg.into()))
}

/// A label matcher.
#[derive(Debug, Clone)]
pub struct Matcher {
    /// Label name.
    pub label: String,
    /// Operator.
    pub op: MatchOp,
    /// Value or pattern.
    pub value: String,
    re: Option<Regex>,
}

/// Matcher operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchOp {
    /// `=`
    Eq,
    /// `!=`
    Ne,
    /// `=~` (anchored regular expression)
    Re,
    /// `!~`
    NotRe,
}

impl Matcher {
    /// A matcher; regular expressions are anchored like Prometheus's.
    pub fn new(label: &str, op: MatchOp, value: &str) -> Result<Self, ExprError> {
        let re = match op {
            MatchOp::Re | MatchOp::NotRe => Some(
                Regex::new(&format!("^(?:{value})$"))
                    .map_err(|e| ExprError(format!("{label}=~{value:?} is not a valid regular expression: {e}")))?,
            ),
            _ => None,
        };
        Ok(Self {
            label: label.to_owned(),
            op,
            value: value.to_owned(),
            re,
        })
    }

    /// Whether `labels` match (a missing label is the empty string).
    pub fn matches(&self, labels: &Labels) -> bool {
        let v = labels.get(&self.label).map_or("", String::as_str);
        match self.op {
            MatchOp::Eq => v == self.value,
            MatchOp::Ne => v != self.value,
            MatchOp::Re => self.re.as_ref().is_some_and(|r| r.is_match(v)),
            MatchOp::NotRe => !self.re.as_ref().is_some_and(|r| r.is_match(v)),
        }
    }
}

/// An instant-vector selector.
#[derive(Debug, Clone)]
pub struct Selector {
    /// Metric name.
    pub name: String,
    /// Matchers on the other labels.
    pub matchers: Vec<Matcher>,
}

/// Aggregation operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggOp {
    /// Sum.
    Sum,
    /// Average.
    Avg,
    /// Minimum.
    Min,
    /// Maximum.
    Max,
    /// Number of series.
    Count,
}

/// Range function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeFn {
    /// Per-second increase of a counter (resets handled).
    Rate,
    /// Increase of a counter over the window.
    Increase,
    /// Last minus first value (gauges).
    Delta,
    /// Average of the points.
    AvgOverTime,
    /// Minimum of the points.
    MinOverTime,
    /// Maximum of the points.
    MaxOverTime,
}

/// Binary operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `/`
    Div,
    /// `%`
    Mod,
}

/// A parsed expression.
#[derive(Debug, Clone)]
pub enum Expr {
    /// A number.
    Number(f64),
    /// An instant vector.
    Select(Selector),
    /// A range function over a selector and a window.
    Range(RangeFn, Selector, i64),
    /// An aggregation; `by` (true) or `without` (false) these labels; `None`: everything.
    Agg(AggOp, Option<(bool, Vec<String>)>, Box<Expr>),
    /// `histogram_quantile(q, buckets)`.
    Quantile(f64, Box<Expr>),
    /// `abs(x)`.
    Abs(Box<Expr>),
    /// `absent(x)`: 1 when `x` is empty.
    Absent(Box<Expr>),
    /// `clamp_min(x, n)` (`true`) or `clamp_max(x, n)`.
    Clamp(bool, Box<Expr>, f64),
    /// `time()`: the evaluation time in Unix seconds.
    Time,
    /// Negation.
    Neg(Box<Expr>),
    /// Binary arithmetic.
    Bin(BinOp, Box<Expr>, Box<Expr>),
}

// ---------------------------------------------------------------- lexer

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Num(f64),
    Str(String),
    Dur(i64),
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Eq,
    Ne,
    Re,
    NotRe,
}

/// Parses `5m`, `30s`, `1h`, `2d`, `1w`, `500ms`, or combinations (`1h30m`).
pub fn parse_duration(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if s == "0" {
        return Some(0);
    }
    let b = s.as_bytes();
    let mut i = 0;
    let mut total: i64 = 0;
    while i < b.len() {
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if start == i {
            return None;
        }
        let n: i64 = s[start..i].parse().ok()?;
        let unit_start = i;
        while i < b.len() && b[i].is_ascii_alphabetic() {
            i += 1;
        }
        let ms = match &s[unit_start..i] {
            "ms" => 1,
            "s" => 1_000,
            "m" => 60_000,
            "h" => 3_600_000,
            "d" => 86_400_000,
            "w" => 7 * 86_400_000,
            _ => return None,
        };
        total = total.checked_add(n.checked_mul(ms)?)?;
    }
    Some(total)
}

/// `90000` → `1m30s` (for messages and API answers).
pub fn format_duration(ms: i64) -> String {
    if ms == 0 {
        return "0s".into();
    }
    let mut out = String::new();
    let mut rest = ms;
    for (unit, n) in [("d", 86_400_000), ("h", 3_600_000), ("m", 60_000), ("s", 1_000), ("ms", 1)] {
        if rest >= n {
            out.push_str(&format!("{}{unit}", rest / n));
            rest %= n;
        }
    }
    out
}

fn lex(src: &str) -> Result<Vec<Tok>, ExprError> {
    let b = src.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    let mut in_brackets = false;
    while i < b.len() {
        let c = b[i];
        match c {
            b' ' | b'\t' | b'\n' | b'\r' => i += 1,
            b'(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            b')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            b'{' => {
                out.push(Tok::LBrace);
                i += 1;
            }
            b'}' => {
                out.push(Tok::RBrace);
                i += 1;
            }
            b'[' => {
                out.push(Tok::LBracket);
                in_brackets = true;
                i += 1;
            }
            b']' => {
                out.push(Tok::RBracket);
                in_brackets = false;
                i += 1;
            }
            b',' => {
                out.push(Tok::Comma);
                i += 1;
            }
            b'+' => {
                out.push(Tok::Plus);
                i += 1;
            }
            b'-' => {
                out.push(Tok::Minus);
                i += 1;
            }
            b'*' => {
                out.push(Tok::Star);
                i += 1;
            }
            b'/' => {
                out.push(Tok::Slash);
                i += 1;
            }
            b'%' => {
                out.push(Tok::Percent);
                i += 1;
            }
            b'=' => {
                if b.get(i + 1) == Some(&b'~') {
                    out.push(Tok::Re);
                    i += 2;
                } else if b.get(i + 1) == Some(&b'=') {
                    return err("== is not an operator here: the comparison is the rule's op and threshold");
                } else {
                    out.push(Tok::Eq);
                    i += 1;
                }
            }
            b'!' => match b.get(i + 1) {
                Some(b'=') => {
                    out.push(Tok::Ne);
                    i += 2;
                }
                Some(b'~') => {
                    out.push(Tok::NotRe);
                    i += 2;
                }
                _ => return err("expected != or !~"),
            },
            b'"' | b'\'' => {
                let quote = c;
                i += 1;
                let mut s = String::new();
                loop {
                    let Some(&ch) = b.get(i) else {
                        return err("a string is not closed");
                    };
                    if ch == quote {
                        i += 1;
                        break;
                    }
                    if ch == b'\\' {
                        match b.get(i + 1) {
                            Some(b'n') => s.push('\n'),
                            Some(b't') => s.push('\t'),
                            Some(&e) => s.push(char::from(e)),
                            None => return err("a string is not closed"),
                        }
                        i += 2;
                        continue;
                    }
                    let rest = &src[i..];
                    let chr = rest.chars().next().unwrap_or('\0');
                    s.push(chr);
                    i += chr.len_utf8();
                }
                out.push(Tok::Str(s));
            }
            c if c.is_ascii_digit() || c == b'.' => {
                let start = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'.' || b[i] == b'_') {
                    // An exponent sign: 1e-3.
                    i += 1;
                    if i < b.len() && (b[i] == b'-' || b[i] == b'+') && matches!(b[i - 1], b'e' | b'E') && !in_brackets {
                        i += 1;
                    }
                }
                let text = src[start..i].replace('_', "");
                if in_brackets {
                    let d = parse_duration(&text)
                        .ok_or_else(|| ExprError(format!("{text:?} is not a duration such as 5m or 1h")))?;
                    out.push(Tok::Dur(d));
                } else if let Ok(n) = text.parse::<f64>() {
                    out.push(Tok::Num(n));
                } else if let Some(d) = parse_duration(&text) {
                    out.push(Tok::Dur(d));
                } else {
                    return err(format!("{text:?} is not a number"));
                }
            }
            c if c.is_ascii_alphabetic() || c == b'_' || c == b':' => {
                let start = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b':') {
                    i += 1;
                }
                out.push(Tok::Ident(src[start..i].to_owned()));
            }
            other => return err(format!("unexpected character {:?}", char::from(other))),
        }
    }
    Ok(out)
}

// --------------------------------------------------------------- parser

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
    depth: usize,
}

const MAX_DEPTH: usize = 64;

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        self.pos += 1;
        t
    }

    fn expect(&mut self, t: &Tok, what: &str) -> Result<(), ExprError> {
        match self.next() {
            Some(ref x) if x == t => Ok(()),
            Some(x) => err(format!("expected {what}, found {}", show(&x))),
            None => err(format!("expected {what} at the end")),
        }
    }

    fn expr(&mut self) -> Result<Expr, ExprError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return err("the expression is nested too deeply");
        }
        let mut left = self.term()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Plus) => BinOp::Add,
                Some(Tok::Minus) => BinOp::Sub,
                _ => break,
            };
            self.pos += 1;
            let right = self.term()?;
            left = Expr::Bin(op, Box::new(left), Box::new(right));
        }
        self.depth -= 1;
        Ok(left)
    }

    fn term(&mut self) -> Result<Expr, ExprError> {
        let mut left = self.unary()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Star) => BinOp::Mul,
                Some(Tok::Slash) => BinOp::Div,
                Some(Tok::Percent) => BinOp::Mod,
                _ => break,
            };
            self.pos += 1;
            let right = self.unary()?;
            left = Expr::Bin(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<Expr, ExprError> {
        if self.peek() == Some(&Tok::Minus) {
            self.pos += 1;
            return Ok(Expr::Neg(Box::new(self.unary()?)));
        }
        self.primary()
    }

    fn number(&mut self, what: &str) -> Result<f64, ExprError> {
        let neg = if self.peek() == Some(&Tok::Minus) {
            self.pos += 1;
            true
        } else {
            false
        };
        match self.next() {
            Some(Tok::Num(n)) => Ok(if neg { -n } else { n }),
            _ => err(format!("{what} must be a number")),
        }
    }

    fn grouping(&mut self) -> Result<Option<(bool, Vec<String>)>, ExprError> {
        let by = match self.peek() {
            Some(Tok::Ident(w)) if w == "by" => true,
            Some(Tok::Ident(w)) if w == "without" => false,
            _ => return Ok(None),
        };
        self.pos += 1;
        self.expect(&Tok::LParen, "( after by/without")?;
        let mut labels = Vec::new();
        loop {
            match self.next() {
                Some(Tok::RParen) => break,
                Some(Tok::Ident(l)) => {
                    labels.push(l);
                    match self.next() {
                        Some(Tok::Comma) => {}
                        Some(Tok::RParen) => break,
                        _ => return err("expected , or ) in a label list"),
                    }
                }
                _ => return err("expected a label name in by (…)"),
            }
        }
        Ok(Some((by, labels)))
    }

    fn selector(&mut self, name: String) -> Result<Selector, ExprError> {
        let mut matchers = Vec::new();
        if self.peek() == Some(&Tok::LBrace) {
            self.pos += 1;
            loop {
                match self.next() {
                    Some(Tok::RBrace) => break,
                    Some(Tok::Ident(label)) => {
                        let op = match self.next() {
                            Some(Tok::Eq) => MatchOp::Eq,
                            Some(Tok::Ne) => MatchOp::Ne,
                            Some(Tok::Re) => MatchOp::Re,
                            Some(Tok::NotRe) => MatchOp::NotRe,
                            _ => return err(format!("expected =, !=, =~ or !~ after {label}")),
                        };
                        let value = match self.next() {
                            Some(Tok::Str(v)) => v,
                            _ => return err(format!("the value of {label} must be a quoted string")),
                        };
                        matchers.push(Matcher::new(&label, op, &value)?);
                        match self.next() {
                            Some(Tok::Comma) => {}
                            Some(Tok::RBrace) => break,
                            _ => return err("expected , or } in a selector"),
                        }
                    }
                    _ => return err("expected a label name in {…}"),
                }
            }
        }
        Ok(Selector { name, matchers })
    }

    fn range_arg(&mut self, f: &str) -> Result<(Selector, i64), ExprError> {
        let name = match self.next() {
            Some(Tok::Ident(n)) => n,
            _ => return err(format!("{f}() needs a metric with a range, such as {f}(metric[5m])")),
        };
        let sel = self.selector(name)?;
        if self.next() != Some(Tok::LBracket) {
            return err(format!("{f}() needs a range, such as {f}({}[5m])", sel.name));
        }
        let d = match self.next() {
            Some(Tok::Dur(d)) if d > 0 => d,
            _ => return err("a range must be a duration such as [5m]"),
        };
        if d > 7 * 86_400_000 {
            return err("a range is at most 7d");
        }
        self.expect(&Tok::RBracket, "]")?;
        Ok((sel, d))
    }

    fn primary(&mut self) -> Result<Expr, ExprError> {
        match self.next() {
            Some(Tok::Num(n)) => Ok(Expr::Number(n)),
            Some(Tok::LParen) => {
                let e = self.expr()?;
                self.expect(&Tok::RParen, ")")?;
                Ok(e)
            }
            Some(Tok::Ident(id)) => {
                let agg = match id.as_str() {
                    "sum" => Some(AggOp::Sum),
                    "avg" => Some(AggOp::Avg),
                    "min" => Some(AggOp::Min),
                    "max" => Some(AggOp::Max),
                    "count" => Some(AggOp::Count),
                    _ => None,
                };
                if let Some(op) = agg
                    && matches!(self.peek(), Some(Tok::LParen) | Some(Tok::Ident(_)))
                {
                    let mut grouping = self.grouping()?;
                    self.expect(&Tok::LParen, "(")?;
                    let inner = self.expr()?;
                    self.expect(&Tok::RParen, ")")?;
                    if grouping.is_none() {
                        grouping = self.grouping()?;
                    }
                    return Ok(Expr::Agg(op, grouping, Box::new(inner)));
                }
                if self.peek() == Some(&Tok::LParen) {
                    self.pos += 1;
                    let e = match id.as_str() {
                        "rate" | "increase" | "delta" | "avg_over_time" | "min_over_time" | "max_over_time" => {
                            let f = match id.as_str() {
                                "rate" => RangeFn::Rate,
                                "increase" => RangeFn::Increase,
                                "delta" => RangeFn::Delta,
                                "avg_over_time" => RangeFn::AvgOverTime,
                                "min_over_time" => RangeFn::MinOverTime,
                                _ => RangeFn::MaxOverTime,
                            };
                            let (sel, d) = self.range_arg(&id)?;
                            Expr::Range(f, sel, d)
                        }
                        "histogram_quantile" => {
                            let q = self.number("the quantile")?;
                            if !(0.0..=1.0).contains(&q) {
                                return err("the quantile must be between 0 and 1, such as 0.95");
                            }
                            self.expect(&Tok::Comma, ",")?;
                            Expr::Quantile(q, Box::new(self.expr()?))
                        }
                        "abs" => Expr::Abs(Box::new(self.expr()?)),
                        "absent" => Expr::Absent(Box::new(self.expr()?)),
                        "clamp_min" | "clamp_max" => {
                            let e = self.expr()?;
                            self.expect(&Tok::Comma, ",")?;
                            let n = self.number("the bound")?;
                            Expr::Clamp(id == "clamp_min", Box::new(e), n)
                        }
                        "time" => Expr::Time,
                        other => {
                            return err(format!(
                                "unknown function {other}(); use rate, increase, delta, avg_over_time, \
                                 min_over_time, max_over_time, histogram_quantile, abs, absent, clamp_min, \
                                 clamp_max, time or an aggregation (sum, avg, min, max, count)"
                            ));
                        }
                    };
                    self.expect(&Tok::RParen, ")")?;
                    return Ok(e);
                }
                if self.peek() == Some(&Tok::LBracket) {
                    return err(format!(
                        "{id}[…] is a range: wrap it in a function such as rate({id}[5m])"
                    ));
                }
                Ok(Expr::Select(self.selector(id)?))
            }
            Some(t) => err(format!("unexpected {}", show(&t))),
            None => err("the expression is empty"),
        }
    }
}

fn show(t: &Tok) -> String {
    match t {
        Tok::Ident(s) => s.clone(),
        Tok::Num(n) => n.to_string(),
        Tok::Str(s) => format!("{s:?}"),
        Tok::Dur(d) => format_duration(*d),
        Tok::LParen => "(".into(),
        Tok::RParen => ")".into(),
        Tok::LBrace => "{".into(),
        Tok::RBrace => "}".into(),
        Tok::LBracket => "[".into(),
        Tok::RBracket => "]".into(),
        Tok::Comma => ",".into(),
        Tok::Plus => "+".into(),
        Tok::Minus => "-".into(),
        Tok::Star => "*".into(),
        Tok::Slash => "/".into(),
        Tok::Percent => "%".into(),
        Tok::Eq => "=".into(),
        Tok::Ne => "!=".into(),
        Tok::Re => "=~".into(),
        Tok::NotRe => "!~".into(),
    }
}

/// Parses an expression.
pub fn parse(src: &str) -> Result<Expr, ExprError> {
    if src.len() > 4096 {
        return err("an expression is at most 4096 characters");
    }
    let toks = lex(src)?;
    let mut p = Parser { toks, pos: 0, depth: 0 };
    let e = p.expr()?;
    if let Some(t) = p.peek() {
        return err(format!("unexpected {} after the expression", show(t)));
    }
    Ok(e)
}

impl Expr {
    /// Metric names read through a range function, and the longest range.
    pub fn ranges(&self, out: &mut BTreeSet<String>, longest: &mut i64) {
        match self {
            Expr::Range(_, s, d) => {
                out.insert(s.name.clone());
                *longest = (*longest).max(*d);
            }
            Expr::Agg(_, _, e) | Expr::Quantile(_, e) | Expr::Abs(e) | Expr::Absent(e) | Expr::Neg(e) => {
                e.ranges(out, longest)
            }
            Expr::Clamp(_, e, _) => e.ranges(out, longest),
            Expr::Bin(_, a, b) => {
                a.ranges(out, longest);
                b.ranges(out, longest);
            }
            Expr::Number(_) | Expr::Select(_) | Expr::Time => {}
        }
    }

    /// Every metric name the expression reads.
    pub fn names(&self, out: &mut BTreeSet<String>) {
        match self {
            Expr::Range(_, s, _) | Expr::Select(s) => {
                out.insert(s.name.clone());
            }
            Expr::Agg(_, _, e) | Expr::Quantile(_, e) | Expr::Abs(e) | Expr::Absent(e) | Expr::Neg(e) => e.names(out),
            Expr::Clamp(_, e, _) => e.names(out),
            Expr::Bin(_, a, b) => {
                a.names(out);
                b.names(out);
            }
            Expr::Number(_) | Expr::Time => {}
        }
    }
}

// ------------------------------------------------------------ evaluation

/// What an expression evaluates to.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// A number.
    Scalar(f64),
    /// Samples.
    Vector(Vec<Sample>),
}

impl Value {
    /// The samples; a scalar is one sample without labels.
    pub fn into_samples(self) -> Vec<Sample> {
        match self {
            Value::Scalar(v) => vec![Sample {
                labels: Labels::new(),
                value: v,
            }],
            Value::Vector(v) => v,
        }
    }
}

/// Where an evaluation reads from.
pub struct Context<'a> {
    /// The current values.
    pub now: &'a Snapshot,
    /// Past values (range functions).
    pub history: &'a History,
}

fn select<'a>(snap: &'a Snapshot, sel: &'a Selector) -> impl Iterator<Item = &'a Sample> + 'a {
    snap.series
        .get(&sel.name)
        .into_iter()
        .flatten()
        .filter(move |s| sel.matchers.iter().all(|m| m.matches(&s.labels)))
}

fn without_name(mut l: Labels) -> Labels {
    l.remove("__name__");
    l
}

/// Increase of a counter over the points, treating a drop as a reset.
fn counter_increase(r: &RangeSeries) -> Option<(f64, f64)> {
    let mut pts = r.points.clone();
    if let Some(born) = r.born_after {
        pts.insert(0, (born, 0.0));
    }
    if pts.len() < 2 {
        return None;
    }
    let mut inc = 0.0;
    for w in pts.windows(2) {
        let (a, b) = (w[0].1, w[1].1);
        inc += if b >= a { b - a } else { b };
    }
    let span = (pts[pts.len() - 1].0 - pts[0].0) as f64 / 1000.0;
    Some((inc, span))
}

fn range_value(f: RangeFn, r: &RangeSeries, window_ms: i64) -> Option<f64> {
    let vals = || r.points.iter().map(|p| p.1);
    match f {
        RangeFn::Rate => {
            let (inc, span) = counter_increase(r)?;
            // Per second over the covered span (at least one second).
            Some(inc / span.max(1.0))
        }
        RangeFn::Increase => {
            // The increase seen over the window's points; not extrapolated
            // (ADR-0035 §3), so a count alert never sees more than happened.
            let _ = window_ms;
            counter_increase(r).map(|(inc, _)| inc)
        }
        RangeFn::Delta => {
            if r.points.len() < 2 {
                return None;
            }
            Some(r.points[r.points.len() - 1].1 - r.points[0].1)
        }
        RangeFn::AvgOverTime => {
            let n = r.points.len() as f64;
            Some(vals().sum::<f64>() / n)
        }
        RangeFn::MinOverTime => vals().fold(None, |m: Option<f64>, v| Some(m.map_or(v, |m| m.min(v)))),
        RangeFn::MaxOverTime => vals().fold(None, |m: Option<f64>, v| Some(m.map_or(v, |m| m.max(v)))),
    }
}

fn bucket_quantile(q: f64, mut buckets: Vec<(f64, f64)>) -> f64 {
    buckets.sort_by(|a, b| a.0.total_cmp(&b.0));
    let Some(&(top, total)) = buckets.last() else {
        return f64::NAN;
    };
    if !top.is_infinite() || total <= 0.0 {
        return f64::NAN;
    }
    let rank = q * total;
    let mut prev_bound = 0.0;
    let mut prev_count = 0.0;
    for (i, &(bound, count)) in buckets.iter().enumerate() {
        if count >= rank {
            if bound.is_infinite() {
                // In the +Inf bucket: the highest finite bound.
                return if i == 0 { f64::NAN } else { buckets[i - 1].0 };
            }
            if count == prev_count {
                return bound;
            }
            let start = if i == 0 && bound <= 0.0 { bound } else { prev_bound };
            return start + (bound - start) * (rank - prev_count) / (count - prev_count);
        }
        prev_bound = bound;
        prev_count = count;
    }
    f64::NAN
}

fn arith(op: BinOp, a: f64, b: f64) -> f64 {
    match op {
        BinOp::Add => a + b,
        BinOp::Sub => a - b,
        BinOp::Mul => a * b,
        BinOp::Div => a / b,
        BinOp::Mod => a % b,
    }
}

/// Evaluates `e`.
pub fn eval(e: &Expr, cx: &Context<'_>) -> Value {
    match e {
        Expr::Number(n) => Value::Scalar(*n),
        Expr::Time => Value::Scalar(cx.now.at_ms as f64 / 1000.0),
        Expr::Select(sel) => Value::Vector(select(cx.now, sel).cloned().collect()),
        Expr::Range(f, sel, d) => Value::Vector(
            cx.history
                .range(&sel.name, cx.now.at_ms, *d)
                .into_iter()
                .filter(|r| sel.matchers.iter().all(|m| m.matches(&r.labels)))
                .filter_map(|r| {
                    range_value(*f, &r, *d).map(|v| Sample {
                        labels: without_name(r.labels),
                        value: v,
                    })
                })
                .collect(),
        ),
        Expr::Agg(op, grouping, inner) => {
            let samples = eval(inner, cx).into_samples();
            let mut groups: std::collections::BTreeMap<Labels, Vec<f64>> = std::collections::BTreeMap::new();
            for s in samples {
                let key: Labels = match grouping {
                    None => Labels::new(),
                    Some((true, keep)) => s
                        .labels
                        .iter()
                        .filter(|(k, _)| keep.contains(k))
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                    Some((false, drop)) => s
                        .labels
                        .iter()
                        .filter(|(k, _)| !drop.contains(k) && k.as_str() != "__name__")
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                };
                groups.entry(key).or_default().push(s.value);
            }
            Value::Vector(
                groups
                    .into_iter()
                    .map(|(labels, vals)| {
                        let n = vals.len() as f64;
                        let value = match op {
                            AggOp::Sum => vals.iter().sum(),
                            AggOp::Avg => vals.iter().sum::<f64>() / n,
                            AggOp::Min => vals.iter().copied().fold(f64::INFINITY, f64::min),
                            AggOp::Max => vals.iter().copied().fold(f64::NEG_INFINITY, f64::max),
                            AggOp::Count => n,
                        };
                        Sample { labels, value }
                    })
                    .collect(),
            )
        }
        Expr::Quantile(q, inner) => {
            let mut groups: std::collections::BTreeMap<Labels, Vec<(f64, f64)>> = std::collections::BTreeMap::new();
            for s in eval(inner, cx).into_samples() {
                let Some(le) = s.labels.get("le").and_then(|v| match v.as_str() {
                    "+Inf" | "Inf" => Some(f64::INFINITY),
                    other => other.parse::<f64>().ok(),
                }) else {
                    continue;
                };
                let mut key = without_name(s.labels);
                key.remove("le");
                groups.entry(key).or_default().push((le, s.value));
            }
            Value::Vector(
                groups
                    .into_iter()
                    .map(|(labels, b)| Sample {
                        labels,
                        value: bucket_quantile(*q, b),
                    })
                    .collect(),
            )
        }
        Expr::Abs(inner) => map(eval(inner, cx), f64::abs),
        Expr::Clamp(min, inner, n) => {
            let n = *n;
            if *min {
                map(eval(inner, cx), move |v| v.max(n))
            } else {
                map(eval(inner, cx), move |v| v.min(n))
            }
        }
        Expr::Neg(inner) => map(eval(inner, cx), |v| -v),
        Expr::Absent(inner) => {
            let empty = match eval(inner, cx) {
                Value::Scalar(_) => false,
                Value::Vector(v) => v.is_empty(),
            };
            if !empty {
                return Value::Vector(Vec::new());
            }
            // Labels from the equality matchers of a plain selector.
            let labels: Labels = match inner.as_ref() {
                Expr::Select(s) | Expr::Range(_, s, _) => s
                    .matchers
                    .iter()
                    .filter(|m| m.op == MatchOp::Eq)
                    .map(|m| (m.label.clone(), m.value.clone()))
                    .collect(),
                _ => Labels::new(),
            };
            Value::Vector(vec![Sample { labels, value: 1.0 }])
        }
        Expr::Bin(op, a, b) => match (eval(a, cx), eval(b, cx)) {
            (Value::Scalar(x), Value::Scalar(y)) => Value::Scalar(arith(*op, x, y)),
            (Value::Vector(v), Value::Scalar(y)) => Value::Vector(
                v.into_iter()
                    .map(|s| Sample {
                        labels: without_name(s.labels),
                        value: arith(*op, s.value, y),
                    })
                    .collect(),
            ),
            (Value::Scalar(x), Value::Vector(v)) => Value::Vector(
                v.into_iter()
                    .map(|s| Sample {
                        labels: without_name(s.labels),
                        value: arith(*op, x, s.value),
                    })
                    .collect(),
            ),
            (Value::Vector(l), Value::Vector(r)) => {
                let right: std::collections::HashMap<Labels, f64> =
                    r.into_iter().map(|s| (without_name(s.labels), s.value)).collect();
                Value::Vector(
                    l.into_iter()
                        .filter_map(|s| {
                            let labels = without_name(s.labels);
                            right.get(&labels).map(|y| Sample {
                                value: arith(*op, s.value, *y),
                                labels,
                            })
                        })
                        .collect(),
                )
            }
        },
    }
}

fn map(v: Value, f: impl Fn(f64) -> f64) -> Value {
    match v {
        Value::Scalar(x) => Value::Scalar(f(x)),
        Value::Vector(v) => Value::Vector(
            v.into_iter()
                .map(|s| Sample {
                    labels: without_name(s.labels),
                    value: f(s.value),
                })
                .collect(),
        ),
    }
}

/// Comparison of a rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum CmpOp {
    /// `>`
    #[serde(rename = ">")]
    Gt,
    /// `>=`
    #[serde(rename = ">=")]
    Ge,
    /// `<`
    #[serde(rename = "<")]
    Lt,
    /// `<=`
    #[serde(rename = "<=")]
    Le,
    /// `==`
    #[serde(rename = "==")]
    Eq,
    /// `!=`
    #[serde(rename = "!=")]
    Ne,
}

impl CmpOp {
    /// Parses `>`, `>=`, `<`, `<=`, `==`, `!=`.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim() {
            ">" => CmpOp::Gt,
            ">=" => CmpOp::Ge,
            "<" => CmpOp::Lt,
            "<=" => CmpOp::Le,
            "==" => CmpOp::Eq,
            "!=" => CmpOp::Ne,
            _ => return None,
        })
    }

    /// The operator text.
    pub fn as_str(self) -> &'static str {
        match self {
            CmpOp::Gt => ">",
            CmpOp::Ge => ">=",
            CmpOp::Lt => "<",
            CmpOp::Le => "<=",
            CmpOp::Eq => "==",
            CmpOp::Ne => "!=",
        }
    }

    /// Whether `v op threshold` holds (never for NaN).
    pub fn holds(self, v: f64, threshold: f64) -> bool {
        if v.is_nan() {
            return false;
        }
        match self {
            CmpOp::Gt => v > threshold,
            CmpOp::Ge => v >= threshold,
            CmpOp::Lt => v < threshold,
            CmpOp::Le => v <= threshold,
            CmpOp::Eq => v == threshold,
            CmpOp::Ne => v != threshold,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Series<'a> = (&'a str, &'a [(&'a str, &'a str)], f64);

    fn snap(at: i64, series: &[Series<'_>]) -> Snapshot {
        let mut s = Snapshot::new(at);
        for (n, l, v) in series {
            s.push(n, l, *v);
        }
        s
    }

    fn run(src: &str, now: &Snapshot, h: &History) -> Vec<Sample> {
        let e = parse(src).unwrap_or_else(|e| panic!("{src}: {e}"));
        eval(&e, &Context { now, history: h }).into_samples()
    }

    #[test]
    fn parse_errors_say_what_to_write() {
        for (src, needle) in [
            ("rate(x)", "range"),
            ("x[5m]", "rate(x[5m])"),
            ("nope(x)", "unknown function"),
            ("x{a=1}", "quoted string"),
            ("sum(x", ")"),
            ("histogram_quantile(2, x)", "between 0 and 1"),
            ("x{a=~\"(\"}", "regular expression"),
            ("", "empty"),
            ("x > 1", "unexpected"),
        ] {
            let e = parse(src).expect_err(src);
            assert!(e.0.contains(needle), "{src}: {e}");
        }
        assert!(parse("sum by (route) (rate(forgebase_http_requests_total{status=~'5..'}[5m]))").is_ok());
        assert!(parse("forgebase:queue_backlog{subscription!=\"a\"} / 2 + -1").is_ok());
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("5m"), Some(300_000));
        assert_eq!(parse_duration("1h30m"), Some(5_400_000));
        assert_eq!(parse_duration("500ms"), Some(500));
        assert_eq!(parse_duration("0"), Some(0));
        assert_eq!(parse_duration("5"), None);
        assert_eq!(parse_duration("5x"), None);
        assert_eq!(format_duration(5_400_000), "1h30m");
    }

    #[test]
    fn instant_aggregation_and_ratio() {
        let req = "forgebase_http_requests_total";
        let now = snap(
            0,
            &[
                (req, &[("route", "/a"), ("status", "200")], 90.0),
                (req, &[("route", "/a"), ("status", "500")], 10.0),
                (req, &[("route", "/b"), ("status", "200")], 50.0),
            ],
        );
        let h = History::default();
        let r = run(
            &format!("sum by (route) ({req}{{status=~\"5..\"}}) / sum by (route) ({req})"),
            &now,
            &h,
        );
        assert_eq!(r.len(), 1, "only /a has errors: {r:?}");
        assert_eq!(r[0].labels["route"], "/a");
        assert!((r[0].value - 0.1).abs() < 1e-9);
        let r = run(&format!("count({req})"), &now, &h);
        assert_eq!(r[0].value, 3.0);
        let r = run(&format!("max without (status) ({req}) * 2"), &now, &h);
        assert_eq!(r.len(), 2);
        let r = run("absent(nothing{a=\"x\"})", &now, &h);
        assert_eq!(r[0].labels["a"], "x");
        assert!(run(&format!("absent({req})"), &now, &h).is_empty());
    }

    #[test]
    fn rate_increase_and_resets() {
        let mut h = History::default();
        h.configure(["c".to_owned()].into_iter().collect(), 3_600_000);
        let mut last = Snapshot::new(0);
        for (t, v) in [(0, 0.0), (10_000, 10.0), (20_000, 20.0), (30_000, 5.0), (40_000, 15.0)] {
            last = snap(t, &[("c", &[("k", "1")], v)]);
            h.record(&last);
        }
        // 10 + 10 + 5 (reset) + 10 = 35 over 40 s.
        let r = run("rate(c[1m])", &last, &h);
        assert!((r[0].value - 35.0 / 40.0).abs() < 1e-9, "{r:?}");
        let r = run("increase(c[1m])", &last, &h);
        assert_eq!(r[0].value, 35.0);
        // (0, 40s] leaves out the first point: 5 (reset) + 10 + 10.
        let r = run("increase(c[40s])", &last, &h);
        assert_eq!(r[0].value, 25.0);
        let r = run("delta(c[1m])", &last, &h);
        assert_eq!(r[0].value, 15.0);
        let r = run("max_over_time(c[1m])", &last, &h);
        assert_eq!(r[0].value, 20.0);
    }

    #[test]
    fn histogram_quantile_interpolates() {
        let b = "d_bucket";
        let now = snap(
            0,
            &[
                (b, &[("le", "0.1")], 50.0),
                (b, &[("le", "0.5")], 90.0),
                (b, &[("le", "1")], 100.0),
                (b, &[("le", "+Inf")], 100.0),
            ],
        );
        let h = History::default();
        let r = run("histogram_quantile(0.95, sum by (le) (d_bucket))", &now, &h);
        assert!((r[0].value - 0.75).abs() < 1e-9, "{r:?}");
        let r = run("histogram_quantile(0.5, d_bucket)", &now, &h);
        assert!((r[0].value - 0.1).abs() < 1e-9, "{r:?}");
    }

    #[test]
    fn comparisons_skip_nan() {
        assert!(CmpOp::Gt.holds(2.0, 1.0));
        assert!(!CmpOp::Gt.holds(f64::NAN, 1.0));
        assert!(CmpOp::Lt.holds(f64::NEG_INFINITY, 0.0));
        assert_eq!(CmpOp::parse(">="), Some(CmpOp::Ge));
    }
}
