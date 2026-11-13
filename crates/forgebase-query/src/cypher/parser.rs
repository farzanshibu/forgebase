//! Recursive-descent parser of the portable openCypher subset (see the
//! module documentation of [`super`] for the grammar it accepts and the
//! error codes).

use super::ast::*;
use super::lexer::{Tok, Token, lex};
use super::{CypherError, MAX_ELEMENTS, MAX_NESTING, MAX_PATH_DEPTH, MAX_QUERY_BYTES, MAX_RETURN_ITEMS};

type Result<T> = std::result::Result<T, CypherError>;

/// Clause words that write or call out (answered `cypher_read_only`, as the
/// Apache AGE path answers them).
const WRITES: &[&str] = &["create", "merge", "set", "delete", "detach", "remove", "foreach"];

/// Parses one query of the portable subset.
pub fn parse(q: &str) -> Result<Query> {
    if q.len() > MAX_QUERY_BYTES {
        return Err(CypherError::too_complex(
            q,
            0,
            format_args!("the query is longer than {MAX_QUERY_BYTES} bytes"),
        ));
    }
    let toks = lex(q)?;
    let mut p = Parser {
        q,
        toks,
        i: 0,
        last_end: 0,
        depth: 0,
        elements: 0,
    };
    p.query()
}

struct Parser<'q> {
    q: &'q str,
    toks: Vec<Token>,
    i: usize,
    last_end: usize,
    depth: usize,
    elements: usize,
}

fn describe(t: &Token) -> String {
    match &t.tok {
        Tok::Word(w) => format!("{w:?}"),
        Tok::Quoted(w) => format!("`{w}`"),
        Tok::Str(_) => "a string".into(),
        Tok::Int(_) | Tok::Float(_) => "a number".into(),
        Tok::Param(p) => format!("${p}"),
        Tok::Punct(c) => format!("{c:?}"),
        Tok::Le => "'<='".into(),
        Tok::Ge => "'>='".into(),
        Tok::Ne => "'<>'".into(),
        Tok::RegexMatch => "'=~'".into(),
        Tok::DotDot => "'..'".into(),
        Tok::Eof => "the end of the query".into(),
    }
}

impl Parser<'_> {
    fn peek(&self) -> &Token {
        self.peek_at(0)
    }

    fn peek_at(&self, n: usize) -> &Token {
        let last = self.toks.len() - 1;
        &self.toks[(self.i + n).min(last)]
    }

    fn next(&mut self) -> Token {
        let t = self.peek().clone();
        if t.tok != Tok::Eof {
            self.i += 1;
            self.last_end = t.span.end;
        }
        t
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek().is(c) {
            self.next();
            true
        } else {
            false
        }
    }

    fn eat_tok(&mut self, tok: &Tok) -> bool {
        if &self.peek().tok == tok {
            self.next();
            true
        } else {
            false
        }
    }

    fn eat_kw(&mut self, kw: &str) -> bool {
        if self.peek().is_kw(kw) {
            self.next();
            true
        } else {
            false
        }
    }

    fn here(&self) -> usize {
        self.peek().span.start
    }

    fn expected(&self, what: &str) -> CypherError {
        let t = self.peek();
        CypherError::invalid(
            self.q,
            t.span.start,
            format_args!("expected {what}, found {}", describe(t)),
        )
    }

    fn expect(&mut self, c: char, what: &str) -> Result<Span> {
        if self.peek().is(c) {
            Ok(self.next().span)
        } else {
            Err(self.expected(what))
        }
    }

    fn expect_kw(&mut self, kw: &str) -> Result<Span> {
        if self.peek().is_kw(kw) {
            Ok(self.next().span)
        } else {
            Err(self.expected(kw))
        }
    }

    fn unsupported(&self, at: usize, construct: &str) -> CypherError {
        CypherError::unsupported(self.q, at, construct)
    }

    /// A name: a word or a backquoted name.
    fn name(&mut self, what: &str) -> Result<String> {
        match &self.peek().tok {
            Tok::Word(w) => {
                let w = w.clone();
                self.next();
                Ok(w)
            }
            Tok::Quoted(w) => {
                let w = w.clone();
                self.next();
                Ok(w)
            }
            _ => Err(self.expected(what)),
        }
    }

    fn query(&mut self) -> Result<Query> {
        let mut matches = Vec::new();
        loop {
            let t = self.peek().clone();
            if t.is_kw("match") {
                matches.push(self.match_clause()?);
                continue;
            }
            if t.is_kw("return") {
                let ret = self.return_clause()?;
                let t = self.peek().clone();
                return match &t.tok {
                    Tok::Eof => Ok(Query { matches, ret }),
                    _ if t.is_kw("union") => Err(self.unsupported(t.span.start, "UNION")),
                    _ => Err(self.clause_error(&t)),
                };
            }
            return Err(self.clause_error(&t));
        }
    }

    /// The error for a token where a clause should start.
    fn clause_error(&self, t: &Token) -> CypherError {
        let at = t.span.start;
        if let Tok::Word(w) = &t.tok {
            let lw = w.to_ascii_lowercase();
            if WRITES.contains(&lw.as_str()) {
                return CypherError::at(
                    "cypher_read_only",
                    self.q,
                    at,
                    format_args!("openCypher queries are read-only: {} is not allowed", lw.to_uppercase()),
                );
            }
            if lw == "call" || lw == "load" {
                return CypherError::at(
                    "cypher_read_only",
                    self.q,
                    at,
                    format_args!("{} is not allowed in openCypher queries", lw.to_uppercase()),
                );
            }
            let construct = match lw.as_str() {
                "optional" => Some("OPTIONAL MATCH"),
                "with" => Some("WITH"),
                "unwind" => Some("UNWIND"),
                "union" => Some("UNION"),
                "use" => Some("USE"),
                "show" => Some("SHOW"),
                _ => None,
            };
            if let Some(c) = construct {
                return self.unsupported(at, c);
            }
        }
        match &t.tok {
            Tok::Eof => CypherError::invalid(self.q, at, "the query needs a RETURN clause"),
            Tok::Punct(';') => CypherError::invalid(self.q, at, "send one query, without ';'"),
            _ => CypherError::invalid(
                self.q,
                at,
                format_args!("expected MATCH, WHERE or RETURN, found {}", describe(t)),
            ),
        }
    }

    fn match_clause(&mut self) -> Result<MatchClause> {
        let span = self.next().span;
        let mut patterns = vec![self.pattern()?];
        while self.eat(',') {
            patterns.push(self.pattern()?);
        }
        let filter = if self.eat_kw("where") { Some(self.expr()?) } else { None };
        Ok(MatchClause { patterns, filter, span })
    }

    fn pattern(&mut self) -> Result<Pattern> {
        let t = self.peek().clone();
        if matches!(t.tok, Tok::Word(_) | Tok::Quoted(_)) {
            if self.peek_at(1).is('=') {
                return Err(self.unsupported(t.span.start, "a path variable (p = …)"));
            }
            if t.is_kw("shortestPath") || t.is_kw("allShortestPaths") {
                return Err(self.unsupported(t.span.start, "shortestPath()"));
            }
        }
        let start = self.node_pattern()?;
        let mut steps = Vec::new();
        while self.peek().is('-') || (self.peek().is('<') && self.peek_at(1).is('-')) {
            let rel = self.rel_pattern()?;
            let node = self.node_pattern()?;
            steps.push((rel, node));
        }
        Ok(Pattern { start, steps })
    }

    fn count_element(&mut self, at: usize) -> Result<()> {
        self.elements += 1;
        if self.elements > MAX_ELEMENTS {
            return Err(CypherError::too_complex(
                self.q,
                at,
                format_args!("more than {MAX_ELEMENTS} node and relationship patterns"),
            ));
        }
        Ok(())
    }

    fn node_pattern(&mut self) -> Result<NodePattern> {
        let open = self.expect('(', "'(' (a node pattern)")?;
        self.count_element(open.start)?;
        let var = match &self.peek().tok {
            Tok::Word(_) | Tok::Quoted(_) => Some(self.name("a variable")?),
            _ => None,
        };
        let mut labels = Vec::new();
        while self.eat(':') {
            labels.push(self.name("a label")?);
            if self.peek().is('|') || self.peek().is('&') || self.peek().is('!') {
                return Err(self.unsupported(self.here(), "a label expression (|, &, !)"));
            }
        }
        let props = match &self.peek().tok {
            Tok::Punct('{') => self.map()?,
            Tok::Param(_) => return Err(self.unsupported(self.here(), "a parameter as a property map")),
            _ => Vec::new(),
        };
        if self.peek().is_kw("where") {
            return Err(self.unsupported(self.here(), "WHERE inside a node pattern"));
        }
        let close = self.expect(')', "')' (end of the node pattern)")?;
        Ok(NodePattern {
            var,
            labels,
            props,
            span: open.to(close),
        })
    }

    fn rel_pattern(&mut self) -> Result<RelPattern> {
        let start = self.here();
        self.count_element(start)?;
        let left = self.eat('<');
        self.expect('-', "'-'")?;
        let mut var = None;
        let mut types = Vec::new();
        let mut length = None;
        let mut props = Vec::new();
        if self.eat('[') {
            if matches!(self.peek().tok, Tok::Word(_) | Tok::Quoted(_)) {
                var = Some(self.name("a variable")?);
            }
            if self.eat(':') {
                types.push(self.name("a relationship type")?);
                while self.eat('|') {
                    self.eat(':');
                    types.push(self.name("a relationship type")?);
                }
                if self.peek().is('&') || self.peek().is('!') {
                    return Err(self.unsupported(self.here(), "a type expression (&, !)"));
                }
            }
            if self.peek().is('*') {
                length = Some(self.length()?);
            }
            match &self.peek().tok {
                Tok::Punct('{') => props = self.map()?,
                Tok::Param(_) => return Err(self.unsupported(self.here(), "a parameter as a property map")),
                _ => {}
            }
            if self.peek().is_kw("where") {
                return Err(self.unsupported(self.here(), "WHERE inside a relationship pattern"));
            }
            self.expect(']', "']' (end of the relationship pattern)")?;
        }
        self.expect('-', "'-'")?;
        let right = self.eat('>');
        let direction = match (left, right) {
            (false, true) => Direction::Right,
            (true, false) => Direction::Left,
            _ => Direction::Both,
        };
        Ok(RelPattern {
            var,
            types,
            direction,
            length,
            props,
            span: Span::new(start, self.last_end),
        })
    }

    /// `*`, `*n`, `*min..max`, `*..max`.
    fn length(&mut self) -> Result<(u32, u32)> {
        let star = self.next().span.start;
        let int = |p: &mut Self| -> Result<Option<u32>> {
            match p.peek().tok {
                Tok::Int(n) => {
                    let at = p.here();
                    p.next();
                    u32::try_from(n)
                        .map(Some)
                        .map_err(|_| CypherError::invalid(p.q, at, "path length out of range"))
                }
                _ => Ok(None),
            }
        };
        let (min, max) = match int(self)? {
            Some(n) if self.eat_tok(&Tok::DotDot) => (n, int(self)?),
            Some(n) => (n, Some(n)),
            None if self.eat_tok(&Tok::DotDot) => (1, int(self)?),
            None => (1, None),
        };
        let Some(max) = max else {
            return Err(self.unsupported(
                star,
                &format!("an unbounded variable-length relationship (give an upper bound up to {MAX_PATH_DEPTH}: *1..{MAX_PATH_DEPTH})"),
            ));
        };
        if max > MAX_PATH_DEPTH {
            return Err(CypherError::too_complex(
                self.q,
                star,
                format_args!("variable-length relationships are limited to {MAX_PATH_DEPTH} hops"),
            ));
        }
        if min > max {
            return Err(CypherError::invalid(
                self.q,
                star,
                "the minimum path length exceeds the maximum",
            ));
        }
        Ok((min, max))
    }

    fn map(&mut self) -> Result<Vec<(String, Expr)>> {
        self.expect('{', "'{'")?;
        let mut out: Vec<(String, Expr)> = Vec::new();
        if self.eat('}') {
            return Ok(out);
        }
        loop {
            let at = self.here();
            let key = self.name("a property name")?;
            if out.iter().any(|(k, _)| k == &key) {
                return Err(CypherError::invalid(
                    self.q,
                    at,
                    format_args!("property {key:?} given twice"),
                ));
            }
            self.expect(':', "':'")?;
            let value = self.expr()?;
            out.push((key, value));
            if self.eat(',') {
                continue;
            }
            self.expect('}', "'}' or ','")?;
            return Ok(out);
        }
    }

    fn return_clause(&mut self) -> Result<Return> {
        let span = self.next().span;
        let distinct = self.eat_kw("distinct");
        if self.peek().is('*') {
            return Err(self.unsupported(self.here(), "RETURN * (name the columns)"));
        }
        let mut items: Vec<ReturnItem> = Vec::new();
        loop {
            let start = self.here();
            let expr = self.expr()?;
            let end = self.last_end;
            let alias = if self.eat_kw("as") {
                Some(self.name("a column name")?)
            } else {
                None
            };
            let name = alias
                .clone()
                .unwrap_or_else(|| self.q[start..end].split_whitespace().collect::<Vec<_>>().join(" "));
            if items.iter().any(|i| i.name == name) {
                return Err(CypherError::invalid(
                    self.q,
                    start,
                    format_args!("two RETURN columns are named {name:?}"),
                ));
            }
            items.push(ReturnItem { expr, alias, name });
            if items.len() > MAX_RETURN_ITEMS {
                return Err(CypherError::too_complex(
                    self.q,
                    start,
                    format_args!("RETURN has more than {MAX_RETURN_ITEMS} items"),
                ));
            }
            if !self.eat(',') {
                break;
            }
        }
        let mut order = Vec::new();
        if self.eat_kw("order") {
            self.expect_kw("by")?;
            loop {
                let expr = self.expr()?;
                let desc = if self.eat_kw("desc") || self.eat_kw("descending") {
                    true
                } else {
                    let _ = self.eat_kw("asc") || self.eat_kw("ascending");
                    false
                };
                order.push(SortItem { expr, desc });
                if !self.eat(',') {
                    break;
                }
            }
        }
        let skip = if self.eat_kw("skip") { Some(self.expr()?) } else { None };
        let limit = if self.eat_kw("limit") { Some(self.expr()?) } else { None };
        Ok(Return {
            distinct,
            items,
            order,
            skip,
            limit,
            span,
        })
    }

    // ---- expressions ----------------------------------------------------

    fn enter(&mut self) -> Result<()> {
        self.depth += 1;
        if self.depth > MAX_NESTING {
            return Err(CypherError::too_complex(
                self.q,
                self.here(),
                format_args!("expressions nest deeper than {MAX_NESTING}"),
            ));
        }
        Ok(())
    }

    fn expr(&mut self) -> Result<Expr> {
        self.enter()?;
        let r = self.or();
        self.depth -= 1;
        r
    }

    fn mk(&self, kind: ExprKind, start: usize) -> Expr {
        Expr {
            kind,
            span: Span::new(start, self.last_end.max(start)),
        }
    }

    fn or(&mut self) -> Result<Expr> {
        let start = self.here();
        let mut l = self.xor()?;
        while self.eat_kw("or") {
            let r = self.xor()?;
            l = self.mk(ExprKind::Or(Box::new(l), Box::new(r)), start);
        }
        Ok(l)
    }

    fn xor(&mut self) -> Result<Expr> {
        let l = self.and()?;
        if self.peek().is_kw("xor") {
            return Err(self.unsupported(self.here(), "XOR"));
        }
        Ok(l)
    }

    fn and(&mut self) -> Result<Expr> {
        let start = self.here();
        let mut l = self.not()?;
        while self.eat_kw("and") {
            let r = self.not()?;
            l = self.mk(ExprKind::And(Box::new(l), Box::new(r)), start);
        }
        Ok(l)
    }

    fn not(&mut self) -> Result<Expr> {
        let start = self.here();
        if self.eat_kw("not") {
            self.enter()?;
            let e = self.not();
            self.depth -= 1;
            return Ok(self.mk(ExprKind::Not(Box::new(e?)), start));
        }
        self.comparison()
    }

    fn comparison(&mut self) -> Result<Expr> {
        let start = self.here();
        let mut l = self.additive()?;
        let mut compared = false;
        loop {
            let t = self.peek().clone();
            let op = match &t.tok {
                Tok::Punct('=') => Some(CmpOp::Eq),
                Tok::Ne => Some(CmpOp::Ne),
                Tok::Punct('<') => Some(CmpOp::Lt),
                Tok::Le => Some(CmpOp::Le),
                Tok::Punct('>') => Some(CmpOp::Gt),
                Tok::Ge => Some(CmpOp::Ge),
                _ => None,
            };
            if let Some(op) = op {
                if compared {
                    return Err(self.unsupported(t.span.start, "a chained comparison (a < b < c)"));
                }
                compared = true;
                self.next();
                let r = self.additive()?;
                l = self.mk(ExprKind::Cmp(op, Box::new(l), Box::new(r)), start);
                continue;
            }
            if t.tok == Tok::RegexMatch {
                return Err(self.unsupported(t.span.start, "=~ (regular expressions)"));
            }
            if t.is_kw("is") {
                self.next();
                let negated = self.eat_kw("not");
                self.expect_kw("null")?;
                l = self.mk(
                    ExprKind::IsNull {
                        expr: Box::new(l),
                        negated,
                    },
                    start,
                );
                continue;
            }
            if t.is_kw("in") {
                self.next();
                let r = self.additive()?;
                l = self.mk(ExprKind::In(Box::new(l), Box::new(r)), start);
                continue;
            }
            let sop = if t.is_kw("starts") {
                Some(StringOp::StartsWith)
            } else if t.is_kw("ends") {
                Some(StringOp::EndsWith)
            } else if t.is_kw("contains") {
                Some(StringOp::Contains)
            } else {
                None
            };
            if let Some(sop) = sop {
                self.next();
                if sop != StringOp::Contains {
                    self.expect_kw("with")?;
                }
                let r = self.additive()?;
                l = self.mk(ExprKind::Str(sop, Box::new(l), Box::new(r)), start);
                continue;
            }
            return Ok(l);
        }
    }

    fn additive(&mut self) -> Result<Expr> {
        let l = self.unary()?;
        let t = self.peek();
        if t.is('+') || t.is('-') || t.is('*') || t.is('/') || t.is('%') || t.is('^') {
            return Err(self.unsupported(t.span.start, "arithmetic (+ - * / % ^)"));
        }
        Ok(l)
    }

    fn unary(&mut self) -> Result<Expr> {
        let start = self.here();
        if self.eat('-') {
            self.enter()?;
            let e = self.unary();
            self.depth -= 1;
            let e = e?;
            return Ok(match e.kind {
                ExprKind::Lit(Literal::Int(n)) => self.mk(ExprKind::Lit(Literal::Int(-n)), start),
                ExprKind::Lit(Literal::Float(f)) => self.mk(ExprKind::Lit(Literal::Float(-f)), start),
                _ => self.mk(ExprKind::Neg(Box::new(e)), start),
            });
        }
        if self.eat('+') {
            return self.unary();
        }
        self.postfix()
    }

    fn postfix(&mut self) -> Result<Expr> {
        let start = self.here();
        let mut e = self.atom()?;
        loop {
            if self.peek().is('.') && matches!(self.peek_at(1).tok, Tok::Word(_) | Tok::Quoted(_)) {
                self.next();
                let key = self.name("a property name")?;
                if self.peek().is('(') {
                    return Err(self.unsupported(start, "a namespaced function call"));
                }
                e = self.mk(ExprKind::Prop(Box::new(e), key), start);
                continue;
            }
            if self.peek().is('[') {
                return Err(self.unsupported(self.here(), "list indexing and slicing"));
            }
            if self.peek().is(':') {
                return Err(self.unsupported(self.here(), "a label test (n:Label) in an expression"));
            }
            return Ok(e);
        }
    }

    fn atom(&mut self) -> Result<Expr> {
        let t = self.peek().clone();
        let start = t.span.start;
        let lit = |p: &mut Self, l: Literal| {
            p.next();
            Ok(p.mk(ExprKind::Lit(l), start))
        };
        match &t.tok {
            Tok::Int(n) => return lit(self, Literal::Int(*n)),
            Tok::Float(f) => return lit(self, Literal::Float(*f)),
            Tok::Str(s) => return lit(self, Literal::Str(s.clone())),
            Tok::Param(p) => {
                let p = p.clone();
                self.next();
                return Ok(self.mk(ExprKind::Param(p), start));
            }
            Tok::Punct('(') => {
                self.next();
                let e = self.expr()?;
                self.expect(')', "')'")?;
                if self.peek().is('-') || (self.peek().is('<') && self.peek_at(1).is('-')) {
                    return Err(self.unsupported(start, "a pattern predicate"));
                }
                return Ok(Expr {
                    kind: e.kind,
                    span: Span::new(start, self.last_end),
                });
            }
            Tok::Punct('[') => return self.list(),
            Tok::Punct('{') => return Err(self.unsupported(start, "a map literal")),
            Tok::Quoted(v) => {
                let v = v.clone();
                self.next();
                return Ok(self.mk(ExprKind::Var(v), start));
            }
            Tok::Word(w) => {
                let lw = w.to_ascii_lowercase();
                match lw.as_str() {
                    "true" => return lit(self, Literal::Bool(true)),
                    "false" => return lit(self, Literal::Bool(false)),
                    "null" => return lit(self, Literal::Null),
                    "case" => return Err(self.unsupported(start, "CASE")),
                    "exists" => return Err(self.unsupported(start, "EXISTS")),
                    "count" if self.peek_at(1).is('{') => {
                        return Err(self.unsupported(start, "a COUNT { } subquery"));
                    }
                    "all" | "any" | "none" | "single" if self.peek_at(1).is('(') => {
                        return Err(self.unsupported(start, &format!("the {lw}() list predicate")));
                    }
                    "reduce" if self.peek_at(1).is('(') => return Err(self.unsupported(start, "reduce()")),
                    "shortestpath" | "allshortestpaths" => return Err(self.unsupported(start, "shortestPath()")),
                    "match" | "return" | "where" | "with" | "order" | "skip" | "limit" | "union" | "optional"
                    | "unwind" => {
                        return Err(self.expected("an expression"));
                    }
                    _ => {}
                }
                if self.peek_at(1).is('(') {
                    return self.call(lw, start);
                }
                let w = w.clone();
                self.next();
                return Ok(self.mk(ExprKind::Var(w), start));
            }
            _ => {}
        }
        Err(self.expected("an expression"))
    }

    fn call(&mut self, name: String, start: usize) -> Result<Expr> {
        self.next();
        self.next();
        if name == "count" && self.peek().is('*') {
            self.next();
            self.expect(')', "')'")?;
            return Ok(self.mk(
                ExprKind::Call {
                    name,
                    distinct: false,
                    args: Vec::new(),
                    star: true,
                },
                start,
            ));
        }
        let distinct = self.eat_kw("distinct");
        let mut args = Vec::new();
        if !self.eat(')') {
            loop {
                args.push(self.expr()?);
                if self.eat(',') {
                    continue;
                }
                self.expect(')', "')' or ','")?;
                break;
            }
        }
        Ok(self.mk(
            ExprKind::Call {
                name,
                distinct,
                args,
                star: false,
            },
            start,
        ))
    }

    fn list(&mut self) -> Result<Expr> {
        let start = self.next().span.start;
        if matches!(self.peek().tok, Tok::Word(_) | Tok::Quoted(_)) && self.peek_at(1).is_kw("in") {
            return Err(self.unsupported(start, "a list comprehension"));
        }
        let mut items = Vec::new();
        if !self.eat(']') {
            loop {
                items.push(self.expr()?);
                if self.eat(',') {
                    continue;
                }
                if self.peek().is('|') {
                    return Err(self.unsupported(start, "a pattern or list comprehension"));
                }
                self.expect(']', "']' or ','")?;
                break;
            }
        }
        Ok(self.mk(ExprKind::List(items), start))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(q: &str) -> &'static str {
        parse(q).map(|_| "ok").unwrap_or_else(|e| e.code)
    }

    #[test]
    fn parses_the_subset() {
        let q = parse(
            "MATCH (a:Person {name: $n})-[r:KNOWS|LIKES]->(b), (b)<-[*1..3]-(c:`Odd Label`) \
             WHERE a.age >= 18 AND NOT b.x IS NULL OR c.name STARTS WITH 'J' AND b.tag IN ['x', 'y'] \
             MATCH (c)--(d) \
             RETURN DISTINCT a.name AS name, count(DISTINCT b), collect(c.name) \
             ORDER BY name DESC, count(DISTINCT b) SKIP 1 LIMIT $lim",
        )
        .unwrap();
        assert_eq!(q.matches.len(), 2);
        let m = &q.matches[0];
        assert_eq!(m.patterns.len(), 2);
        assert_eq!(m.patterns[0].start.labels, ["Person"]);
        let (r, b) = &m.patterns[0].steps[0];
        assert_eq!(
            (r.var.as_deref(), r.types.len(), r.direction),
            (Some("r"), 2, Direction::Right)
        );
        assert_eq!(b.var.as_deref(), Some("b"));
        let (v, c) = &m.patterns[1].steps[0];
        assert_eq!((v.length, v.direction), (Some((1, 3)), Direction::Left));
        assert_eq!(c.labels, ["Odd Label"]);
        assert_eq!(q.matches[1].patterns[0].steps[0].0.direction, Direction::Both);
        assert!(q.ret.distinct);
        let names: Vec<&str> = q.ret.items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["name", "count(DISTINCT b)", "collect(c.name)"]);
        assert_eq!(q.ret.order.len(), 2);
        assert!(q.ret.order[0].desc && !q.ret.order[1].desc);
        assert!(matches!(q.ret.limit.as_ref().unwrap().kind, ExprKind::Param(_)));
        // OR binds weaker than AND.
        assert!(matches!(m.filter.as_ref().unwrap().kind, ExprKind::Or(..)));
    }

    #[test]
    fn lengths_and_short_forms() {
        let len = |q: &str| parse(q).unwrap().matches[0].patterns[0].steps[0].0.length;
        assert_eq!(len("MATCH (a)-[*2]->(b) RETURN b"), Some((2, 2)));
        assert_eq!(len("MATCH (a)-[*..4]->(b) RETURN b"), Some((1, 4)));
        assert_eq!(len("MATCH (a)-[*0..1]->(b) RETURN b"), Some((0, 1)));
        assert_eq!(len("MATCH (a)-->(b) RETURN b"), None);
        let dir = |q: &str| parse(q).unwrap().matches[0].patterns[0].steps[0].0.direction;
        assert_eq!(dir("MATCH (a)<--(b) RETURN b"), Direction::Left);
        assert_eq!(dir("MATCH (a)--(b) RETURN b"), Direction::Both);
        assert_eq!(dir("MATCH (a)-[:X]->(b) RETURN b"), Direction::Right);
        assert_eq!(code("RETURN 1 AS one"), "ok");
        assert_eq!(code("RETURN -1.5 AS x, 'a' AS y, [1, 2] AS z, null AS n"), "ok");
    }

    #[test]
    fn writes_are_read_only() {
        for q in [
            "CREATE (n) RETURN n",
            "MATCH (n) SET n.x = 1 RETURN n",
            "MATCH (n) DETACH DELETE n",
            "MATCH (n) WHERE n.a = 1 REMOVE n.a RETURN n",
            "MERGE (n:A) RETURN n",
            "CALL db.labels()",
            "LOAD CSV FROM 'x' AS l RETURN l",
        ] {
            assert_eq!(code(q), "cypher_read_only", "{q}");
        }
        // Keywords as names are fine.
        assert_eq!(code("MATCH (n) WHERE n.set = 1 RETURN n.create AS c"), "ok");
    }

    #[test]
    fn unsupported_constructs_name_themselves() {
        for (q, what) in [
            ("MATCH (n) WITH n RETURN n", "WITH"),
            ("OPTIONAL MATCH (n) RETURN n", "OPTIONAL MATCH"),
            ("UNWIND [1] AS x RETURN x", "UNWIND"),
            ("RETURN 1 AS a UNION RETURN 2 AS a", "UNION"),
            ("MATCH (n) RETURN n.a + 1", "arithmetic"),
            ("MATCH (n) RETURN CASE WHEN true THEN 1 END", "CASE"),
            ("MATCH p = (a)-->(b) RETURN p", "path variable"),
            ("MATCH (a)-[*]->(b) RETURN b", "unbounded"),
            ("MATCH (a)-[*2..]->(b) RETURN b", "unbounded"),
            ("MATCH (n) RETURN *", "RETURN *"),
            ("MATCH (n) WHERE n.a =~ 'x.*' RETURN n", "=~"),
            ("MATCH (n) WHERE (n)-->() RETURN n", "pattern predicate"),
            ("MATCH (n) RETURN [x IN [1] | x]", "list comprehension"),
            ("MATCH (n) RETURN n.list[0]", "indexing"),
            ("MATCH (n) RETURN {a: 1}", "map literal"),
            ("MATCH (n) WHERE true XOR false RETURN n", "XOR"),
            ("MATCH (n) WHERE exists(n.a) RETURN n", "EXISTS"),
            ("MATCH (a:A|B) RETURN a", "label expression"),
            ("MATCH (a {x: 1} WHERE a.y = 2) RETURN a", "WHERE inside"),
            ("MATCH (n) RETURN apoc.x(n)", "namespaced"),
        ] {
            let e = parse(q).unwrap_err();
            assert_eq!(e.code, "cypher_unsupported", "{q}: {e}");
            assert!(e.message.contains(what), "{q}: {e}");
            assert!(e.message.contains("line 1, column"), "{e}");
        }
    }

    #[test]
    fn syntax_errors_and_limits() {
        for q in [
            "MATCH (n RETURN n",
            "MATCH (n) RETURN",
            "MATCH (n)",
            "MATCH (n) RETURN n;",
            "RETURN 1 AS a, 2 AS a",
            "MATCH (a)-[*3..2]->(b) RETURN b",
            "MATCH (n {a: 1, a: 2}) RETURN n",
            "RETURN 'x",
            "FOO",
        ] {
            assert_eq!(code(q), "cypher_invalid", "{q}");
        }
        assert_eq!(code("MATCH (a)-[*1..11]->(b) RETURN b"), "cypher_too_complex");
        let deep = format!("RETURN {}1{}", "(".repeat(100), ")".repeat(100));
        assert_eq!(code(&deep), "cypher_too_complex");
        let many = format!("MATCH {} RETURN 1", vec!["()"; 40].join(", "));
        assert_eq!(code(&many), "cypher_too_complex");
        let e = parse("MATCH (n)\nRETURN n.a AND").unwrap_err();
        assert_eq!((e.line, e.column), (2, 15), "{e}");
    }
}
