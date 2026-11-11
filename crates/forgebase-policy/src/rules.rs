//! The rule language (ADR-0008 §6): the data core's filter grammar plus
//! request references.
//!
//! ```text
//! @request.auth.id        caller id (absent for anonymous callers)
//! @request.auth.type      anonymous | user | service | admin
//! @request.auth.collection auth collection of a user
//! @request.auth.authenticated  true | false
//! @request.auth.roles     effective role keys (several values: use ?=)
//! @request.auth.permissions  effective allow patterns (several values)
//! @request.auth.scopes    API-key / PAT scopes (several values)
//! @request.auth.<field>   any other field of the caller's auth record (ABAC)
//! @request.tenant | @request.realm | @request.org
//! ```
//!
//! A rule is parsed once. At request time every request reference gets its
//! value and the rule is partially evaluated in three-valued (SQL) logic:
//! parts that only involve request values and constants are decided in Rust,
//! the rest stays an expression over record fields with request values as
//! bound parameters — the data core compiles it into the same SQL statement.
//! A request value that is absent (or null) makes every comparison it is in
//! *unknown* — except an explicit `= null` / `= ""` presence test — so a rule
//! like `owner = @request.auth.id` can never match rows with an empty owner
//! for an anonymous caller, and negation cannot turn "unknown" into a grant.
//! Unknown at the top is a denial.

use std::collections::HashMap;
use std::fmt;

use forgebase_query::{Compare, Expr, Literal, Op, Operand, parse_filter};
use serde_json::Value as Json;

/// Longest rule text.
pub const MAX_RULE_LEN: usize = 4096;
/// Parameter prefix reserved for request references.
const PARAM_PREFIX: &str = "__rq_";
/// Parameter that is always NULL: renders "unknown" inside SQL.
const NULL_PARAM: &str = "__fb_null";

/// Why a rule is rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid rule: {0}")]
pub struct RuleError(pub String);

/// What a request reference names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestRef {
    /// `@request.auth.<path>`.
    Auth(Vec<String>),
    /// `@request.tenant`.
    Tenant,
    /// `@request.realm`.
    Realm,
    /// `@request.org`.
    Org,
}

impl RequestRef {
    /// Known to hold several values (must be used as `@... ?op x`).
    fn is_multi(&self) -> bool {
        matches!(self, RequestRef::Auth(p) if p.len() == 1 && matches!(p[0].as_str(), "roles" | "permissions" | "scopes"))
    }
}

/// A parsed rule.
#[derive(Debug, Clone)]
pub struct Rule {
    source: String,
    expr: Expr,
    refs: Vec<RequestRef>,
}

/// The value of a request reference for one request.
#[derive(Debug, Clone, PartialEq)]
pub enum RequestValue {
    /// Not available (anonymous caller, no tenant, unknown attribute, null).
    Absent,
    /// One value.
    One(Json),
    /// Several values (roles, scopes, array attributes).
    Many(Vec<Json>),
}

/// Resolves request references.
pub trait RequestValues {
    /// The value of `r`.
    fn value(&self, r: &RequestRef) -> RequestValue;
}

/// Result of partially evaluating a rule.
#[derive(Debug, Clone, PartialEq)]
pub enum Eval {
    /// Holds for every row.
    True,
    /// Holds for no row.
    False,
    /// Cannot be decided (an absent value); treated as false at the top.
    Unknown,
    /// Holds for the rows matching this expression (parameters in `params`).
    Sql(Expr),
}

/// A residual expression with its parameter values.
#[derive(Debug, Clone, PartialEq)]
pub struct Residual {
    /// Expression over record fields.
    pub filter: Expr,
    /// Parameter values.
    pub params: HashMap<String, Json>,
}

impl Residual {
    /// As a data-core predicate.
    pub fn into_predicate(self) -> forgebase_data::Predicate {
        forgebase_data::Predicate {
            filter: self.filter,
            params: self.params,
        }
    }
}

impl fmt::Display for Rule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.source)
    }
}

/// Replaces request references with `{:__rq_N}` placeholders outside string
/// literals and refuses caller parameters (`{:name}`) in rules.
fn substitute(src: &str) -> Result<(String, Vec<RequestRef>), RuleError> {
    let b = src.as_bytes();
    let mut out = String::with_capacity(src.len());
    let mut refs = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'\'' || c == b'"' {
            let start = i;
            i += 1;
            while i < b.len() && b[i] != c {
                if b[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            i = (i + 1).min(b.len());
            out.push_str(&src[start..i]);
            continue;
        }
        if c == b'{' && b.get(i + 1) == Some(&b':') {
            return Err(RuleError("rules cannot use {:parameters}; use @request values".into()));
        }
        if c == b'@' && src[i..].starts_with("@request") {
            let mut j = i + "@request".len();
            let mut segs = Vec::new();
            while j < b.len() && b[j] == b'.' {
                let s = j + 1;
                let mut e = s;
                while e < b.len() && (b[e].is_ascii_alphanumeric() || b[e] == b'_') {
                    e += 1;
                }
                if e == s || b[s].is_ascii_digit() {
                    return Err(RuleError(format!("bad request reference at {i}")));
                }
                segs.push(src[s..e].to_owned());
                j = e;
            }
            let r = match segs.split_first() {
                Some((h, rest)) if h == "auth" && !rest.is_empty() && rest.len() <= 4 => {
                    RequestRef::Auth(rest.to_vec())
                }
                Some((h, [])) if h == "tenant" => RequestRef::Tenant,
                Some((h, [])) if h == "realm" => RequestRef::Realm,
                Some((h, [])) if h == "org" => RequestRef::Org,
                _ => {
                    return Err(RuleError(format!(
                        "unknown request reference @request.{}",
                        segs.join(".")
                    )));
                }
            };
            out.push_str(&format!("{{:{PARAM_PREFIX}{}}}", refs.len()));
            refs.push(r);
            i = j;
            continue;
        }
        let ch = src[i..].chars().next().unwrap_or(' ');
        out.push(ch);
        i += ch.len_utf8();
    }
    Ok((out, refs))
}

fn ref_index(o: &Operand) -> Option<usize> {
    match o {
        Operand::Param(p) => p.strip_prefix(PARAM_PREFIX).and_then(|n| n.parse().ok()),
        _ => None,
    }
}

impl Rule {
    /// Parses a rule. An empty rule is not a rule (callers decide what empty
    /// means: "everyone" for collection rules).
    pub fn parse(src: &str) -> Result<Self, RuleError> {
        if src.len() > MAX_RULE_LEN {
            return Err(RuleError(format!("rules are at most {MAX_RULE_LEN} bytes")));
        }
        let (text, refs) = substitute(src)?;
        let expr = parse_filter(&text)
            .map_err(|e| RuleError(e.to_string()))?
            .ok_or_else(|| RuleError("empty rule".into()))?;
        let rule = Self {
            source: src.to_owned(),
            expr,
            refs,
        };
        rule.check_multi(&rule.expr)?;
        Ok(rule)
    }

    /// The rule text.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// The parsed expression (request references are `{:__rq_N}` params).
    pub fn expr(&self) -> &Expr {
        &self.expr
    }

    /// The request references, by parameter index (`{:__rq_N}` is the
    /// `N`th); native policies render them from the claims (FB-152).
    pub fn refs(&self) -> &[RequestRef] {
        &self.refs
    }

    fn check_multi(&self, e: &Expr) -> Result<(), RuleError> {
        match e {
            Expr::And(v) | Expr::Or(v) => v.iter().try_for_each(|x| self.check_multi(x)),
            Expr::Not(x) => self.check_multi(x),
            Expr::Const(_) => Ok(()),
            Expr::Compare(c) => {
                if let Some(i) = ref_index(&c.right)
                    && self.refs[i].is_multi()
                {
                    return Err(RuleError(
                        "put roles, permissions and scopes on the left: @request.auth.roles ?= 'x'".into(),
                    ));
                }
                if let Some(i) = ref_index(&c.left)
                    && self.refs[i].is_multi()
                    && !c.any
                {
                    return Err(RuleError(
                        "roles, permissions and scopes hold several values: compare with ?= / ?!= etc.".into(),
                    ));
                }
                Ok(())
            }
        }
    }

    /// Whether the rule reads record fields (static rules do not).
    pub fn reads_records(&self) -> bool {
        fn walk(e: &Expr) -> bool {
            match e {
                Expr::And(v) | Expr::Or(v) => v.iter().any(walk),
                Expr::Not(x) => walk(x),
                Expr::Const(_) => false,
                Expr::Compare(c) => matches!(c.left, Operand::Path(_)) || matches!(c.right, Operand::Path(_)),
            }
        }
        walk(&self.expr)
    }

    /// The expression with every request reference bound to a placeholder
    /// value of `sample`, for validating record paths against a schema.
    pub fn sample(&self, sample: &Json) -> Residual {
        let params = (0..self.refs.len())
            .map(|i| (format!("{PARAM_PREFIX}{i}"), sample.clone()))
            .chain(std::iter::once((NULL_PARAM.to_owned(), Json::Null)))
            .collect();
        Residual {
            filter: self.expr.clone(),
            params,
        }
    }

    /// Partially evaluates the rule for one request.
    pub fn evaluate(&self, values: &dyn RequestValues, now: &str) -> (Eval, HashMap<String, Json>) {
        let vals: Vec<RequestValue> = self
            .refs
            .iter()
            .map(|r| match values.value(r) {
                RequestValue::One(Json::Null) => RequestValue::Absent,
                v => v,
            })
            .collect();
        let mut params = HashMap::new();
        let mut ev = Evaluator {
            vals: &vals,
            now,
            params: &mut params,
            next: 0,
        };
        let out = ev.expr(&self.expr);
        if matches!(out, Eval::Sql(_)) {
            params.insert(NULL_PARAM.to_owned(), Json::Null);
        }
        (out, params)
    }
}

/// A scalar in SQLite's comparison model (booleans are integers).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Sv {
    Null,
    Int(i64),
    Real(f64),
    Text(String),
}

impl Sv {
    fn from_json(j: &Json) -> Option<Sv> {
        Some(match j {
            Json::Null => Sv::Null,
            Json::Bool(b) => Sv::Int(i64::from(*b)),
            Json::Number(n) => n.as_i64().map(Sv::Int).or_else(|| n.as_f64().map(Sv::Real))?,
            Json::String(s) => Sv::Text(s.clone()),
            _ => return None,
        })
    }

    fn from_literal(l: &Literal) -> Sv {
        match l {
            Literal::Null => Sv::Null,
            Literal::Bool(b) => Sv::Int(i64::from(*b)),
            Literal::Int(i) => Sv::Int(*i),
            Literal::Float(f) => Sv::Real(*f),
            Literal::Text(s) => Sv::Text(s.clone()),
        }
    }

    fn num(&self) -> Option<f64> {
        match self {
            Sv::Int(i) => Some(*i as f64),
            Sv::Real(r) => Some(*r),
            _ => None,
        }
    }

    /// SQLite ordering of non-null values: numbers before text.
    fn cmp(&self, other: &Sv) -> Option<std::cmp::Ordering> {
        match (self, other) {
            (Sv::Int(a), Sv::Int(b)) => Some(a.cmp(b)),
            (a, b) if a.num().is_some() && b.num().is_some() => a.num()?.partial_cmp(&b.num()?),
            (Sv::Text(a), Sv::Text(b)) => Some(a.as_bytes().cmp(b.as_bytes())),
            (a, Sv::Text(_)) if a.num().is_some() => Some(std::cmp::Ordering::Less),
            (Sv::Text(_), b) if b.num().is_some() => Some(std::cmp::Ordering::Greater),
            _ => None,
        }
    }

    fn text(&self) -> String {
        match self {
            Sv::Null => String::new(),
            Sv::Int(i) => i.to_string(),
            Sv::Real(r) => r.to_string(),
            Sv::Text(s) => s.clone(),
        }
    }
}

fn tri(b: Option<bool>) -> Eval {
    match b {
        Some(true) => Eval::True,
        Some(false) => Eval::False,
        None => Eval::Unknown,
    }
}

/// Static comparison with the data core compiler's semantics for constants:
/// `=` is SQL `=` (unknown with NULL), `!=` is `IS NOT`, `= null` / `!= null`
/// test for NULL, ranges are three-valued, `~` is an ASCII case-insensitive
/// substring test (`!~` treats NULL as `''`).
pub(crate) fn static_compare(l: &Sv, op: Op, r: &Sv) -> Eval {
    if matches!(op, Op::Like | Op::NotLike) {
        let Sv::Text(pat) = r else { return Eval::Unknown };
        let pat = pat.to_ascii_lowercase();
        return match (op, l) {
            (Op::Like, Sv::Null) => Eval::Unknown,
            (Op::Like, v) => tri(Some(v.text().to_ascii_lowercase().contains(&pat))),
            (_, v) => tri(Some(!v.text().to_ascii_lowercase().contains(&pat))),
        };
    }
    if *r == Sv::Null {
        return match op {
            Op::Eq => tri(Some(*l == Sv::Null)),
            Op::Ne => tri(Some(*l != Sv::Null)),
            _ => Eval::Unknown,
        };
    }
    let ord = || l.cmp(r);
    match op {
        Op::Eq if *l == Sv::Null => Eval::Unknown,
        Op::Eq => tri(Some(ord() == Some(std::cmp::Ordering::Equal))),
        Op::Ne => tri(Some(*l == Sv::Null || ord() != Some(std::cmp::Ordering::Equal))),
        _ if *l == Sv::Null => Eval::Unknown,
        Op::Gt => tri(ord().map(|o| o.is_gt())),
        Op::Ge => tri(ord().map(|o| o.is_ge())),
        Op::Lt => tri(ord().map(|o| o.is_lt())),
        Op::Le => tri(ord().map(|o| o.is_le())),
        Op::Like | Op::NotLike => unreachable!("handled above"),
    }
}

fn not(e: Eval) -> Eval {
    match e {
        Eval::True => Eval::False,
        Eval::False => Eval::True,
        Eval::Unknown => Eval::Unknown,
        Eval::Sql(x) => Eval::Sql(Expr::Not(Box::new(x))),
    }
}

fn null_expr() -> Expr {
    Expr::Compare(Compare {
        left: Operand::Param(NULL_PARAM.to_owned()),
        op: Op::Eq,
        any: false,
        right: Operand::Literal(Literal::Int(1)),
    })
}

/// Kleene AND / OR over partial results.
fn combine(and: bool, parts: Vec<Eval>) -> Eval {
    let (absorb, neutral) = if and {
        (Eval::False, Eval::True)
    } else {
        (Eval::True, Eval::False)
    };
    let mut sql = Vec::new();
    let mut unknown = false;
    for p in parts {
        match p {
            p if p == absorb => return absorb,
            p if p == neutral => {}
            Eval::Unknown => unknown = true,
            Eval::Sql(x) => sql.push(x),
            _ => {}
        }
    }
    match (sql.is_empty(), unknown) {
        (true, false) => neutral,
        (true, true) => Eval::Unknown,
        (false, u) => {
            if u {
                sql.push(null_expr());
            }
            if sql.len() == 1 {
                Eval::Sql(sql.remove(0))
            } else if and {
                Eval::Sql(Expr::And(sql))
            } else {
                Eval::Sql(Expr::Or(sql))
            }
        }
    }
}

struct Evaluator<'a> {
    vals: &'a [RequestValue],
    now: &'a str,
    params: &'a mut HashMap<String, Json>,
    next: usize,
}

enum Side {
    Path,
    Value(Sv),
    Absent,
    Many(Vec<Json>),
}

impl Evaluator<'_> {
    fn expr(&mut self, e: &Expr) -> Eval {
        match e {
            Expr::Const(true) => Eval::True,
            Expr::Const(false) => Eval::False,
            Expr::Not(x) => not(self.expr(x)),
            Expr::And(v) => {
                let parts = v.iter().map(|x| self.expr(x)).collect();
                combine(true, parts)
            }
            Expr::Or(v) => {
                let parts = v.iter().map(|x| self.expr(x)).collect();
                combine(false, parts)
            }
            Expr::Compare(c) => self.compare(c),
        }
    }

    fn side(&self, o: &Operand) -> Side {
        match o {
            Operand::Path(_) => Side::Path,
            Operand::Literal(l) => Side::Value(Sv::from_literal(l)),
            Operand::Now => Side::Value(Sv::Text(self.now.to_owned())),
            Operand::Param(_) => match ref_index(o).map(|i| &self.vals[i]) {
                Some(RequestValue::One(j)) => Sv::from_json(j).map_or(Side::Absent, Side::Value),
                Some(RequestValue::Many(v)) => Side::Many(v.clone()),
                _ => Side::Absent,
            },
        }
    }

    /// Binds a request value as a fresh parameter.
    fn bind(&mut self, j: Json) -> Operand {
        let name = format!("__rv_{}", self.next);
        self.next += 1;
        self.params.insert(name.clone(), j);
        Operand::Param(name)
    }

    fn compare(&mut self, c: &Compare) -> Eval {
        let (l, r) = (self.side(&c.left), self.side(&c.right));
        let presence = |op: Op, other: &Side| match (op, other) {
            (Op::Eq, Side::Value(Sv::Null)) => Some(Eval::True),
            (Op::Eq, Side::Value(Sv::Text(t))) if t.is_empty() => Some(Eval::True),
            (Op::Ne, Side::Value(Sv::Null)) => Some(Eval::False),
            (Op::Ne, Side::Value(Sv::Text(t))) if t.is_empty() => Some(Eval::False),
            _ => None,
        };
        match (&l, &r) {
            (Side::Absent, other) => return presence(c.op, other).unwrap_or(Eval::Unknown),
            (other, Side::Absent) => return presence(c.op, other).unwrap_or(Eval::Unknown),
            _ => {}
        }
        if let Side::Many(items) = &l {
            // `?op`: some element satisfies (validated at parse time).
            if !c.any {
                return Eval::Unknown;
            }
            let parts = items
                .iter()
                .map(|item| {
                    self.compare(&Compare {
                        left: Operand::Literal(json_literal(item)),
                        op: c.op,
                        any: false,
                        right: c.right.clone(),
                    })
                })
                .collect();
            return combine(false, parts);
        }
        if matches!(r, Side::Many(_)) {
            return Eval::Unknown;
        }
        if let (Side::Value(a), Side::Value(b)) = (&l, &r) {
            if c.any {
                return Eval::Unknown; // `?` needs several values
            }
            return static_compare(a, c.op, b);
        }
        // At least one record path: keep it for SQL, binding request values.
        let mut out = c.clone();
        for o in [&mut out.left, &mut out.right] {
            if let Some(i) = ref_index(o)
                && let RequestValue::One(j) = &self.vals[i]
            {
                *o = self.bind(j.clone());
            } else if let Operand::Param(_) = o {
                return Eval::Unknown;
            }
        }
        Eval::Sql(Expr::Compare(out))
    }
}

fn json_literal(j: &Json) -> Literal {
    match j {
        Json::Bool(b) => Literal::Bool(*b),
        Json::Number(n) => n
            .as_i64()
            .map(Literal::Int)
            .unwrap_or_else(|| Literal::Float(n.as_f64().unwrap_or(0.0))),
        Json::String(s) => Literal::Text(s.clone()),
        _ => Literal::Null,
    }
}

/// A fixed set of request values (tests, the policy dry-run API).
#[derive(Debug, Clone, Default)]
pub struct StaticValues(pub HashMap<String, RequestValue>);

impl RequestValues for StaticValues {
    fn value(&self, r: &RequestRef) -> RequestValue {
        let key = match r {
            RequestRef::Auth(p) => format!("auth.{}", p.join(".")),
            RequestRef::Tenant => "tenant".into(),
            RequestRef::Realm => "realm".into(),
            RequestRef::Org => "org".into(),
        };
        self.0.get(&key).cloned().unwrap_or(RequestValue::Absent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn vals(pairs: &[(&str, RequestValue)]) -> StaticValues {
        StaticValues(pairs.iter().map(|(k, v)| ((*k).to_owned(), v.clone())).collect())
    }

    fn eval(rule: &str, v: &StaticValues) -> Eval {
        Rule::parse(rule).unwrap().evaluate(v, "2026-01-01T00:00:00.000Z").0
    }

    #[test]
    fn static_rules_decide_without_rows() {
        let anon = vals(&[("auth.type", RequestValue::One(json!("anonymous")))]);
        let user = vals(&[
            ("auth.id", RequestValue::One(json!("u1"))),
            ("auth.type", RequestValue::One(json!("user"))),
            ("auth.roles", RequestValue::Many(vec![json!("editor")])),
        ]);
        assert_eq!(eval("@request.auth.id != ''", &anon), Eval::False);
        assert_eq!(eval("@request.auth.id = ''", &anon), Eval::True);
        assert_eq!(eval("@request.auth.id != ''", &user), Eval::True);
        assert_eq!(eval("@request.auth.roles ?= 'editor'", &user), Eval::True);
        assert_eq!(eval("@request.auth.roles ?= 'admin'", &user), Eval::False);
        assert_eq!(eval("@request.auth.roles ?= 'admin'", &anon), Eval::Unknown);
        assert_eq!(eval("@request.auth.type = 'user' && 1 = 1", &user), Eval::True);
    }

    #[test]
    fn absent_values_never_match_rows_even_negated() {
        let anon = vals(&[]);
        assert_eq!(eval("owner = @request.auth.id", &anon), Eval::Unknown);
        assert_eq!(eval("!(owner = @request.auth.id)", &anon), Eval::Unknown);
        // Unknown AND row-dependent stays row-dependent with a NULL marker.
        let Eval::Sql(e) = eval("!(owner = @request.auth.id && status = 'draft')", &anon) else {
            panic!("expected SQL")
        };
        assert!(e.to_string().contains(NULL_PARAM), "{e}");
    }

    #[test]
    fn residuals_bind_request_values() {
        let user = vals(&[("auth.id", RequestValue::One(json!("u1' OR 1=1 --")))]);
        let r = Rule::parse("owner = @request.auth.id || public = true").unwrap();
        let (e, params) = r.evaluate(&user, "now");
        let Eval::Sql(e) = e else { panic!() };
        let text = e.to_string();
        assert!(!text.contains("OR 1=1"), "values are parameters: {text}");
        assert!(params.values().any(|v| v == &json!("u1' OR 1=1 --")));
    }

    #[test]
    fn substitution_respects_strings_and_refuses_params() {
        let r = Rule::parse("name = '@request.auth.id' && x = \"a\\\"@request\"").unwrap();
        assert!(r.refs.is_empty());
        assert!(Rule::parse("owner = {:id}").is_err());
        assert!(Rule::parse("owner = @request.body.id").is_err());
        assert!(Rule::parse("@request.auth.roles = 'x'").is_err());
        assert!(Rule::parse("'x' ?= @request.auth.roles").is_err());
        assert!(Rule::parse("owner = @request.auth.id; DROP TABLE x").is_err());
        assert!(Rule::parse("").is_err());
    }

    #[test]
    fn static_compare_mirrors_sql() {
        use Sv::*;
        assert_eq!(static_compare(&Int(1), Op::Eq, &Real(1.0)), Eval::True);
        assert_eq!(static_compare(&Int(1), Op::Eq, &Text("1".into())), Eval::False);
        assert_eq!(static_compare(&Int(9), Op::Lt, &Text("1".into())), Eval::True);
        assert_eq!(static_compare(&Null, Op::Eq, &Int(1)), Eval::Unknown);
        assert_eq!(static_compare(&Null, Op::Ne, &Int(1)), Eval::True);
        assert_eq!(static_compare(&Null, Op::NotLike, &Text("a".into())), Eval::True);
        assert_eq!(
            static_compare(&Text("ABC".into()), Op::Like, &Text("b".into())),
            Eval::True
        );
    }
}
