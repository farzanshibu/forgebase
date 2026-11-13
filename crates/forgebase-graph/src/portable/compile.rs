//! Compiles a parsed openCypher query ([`forgebase_data::query::cypher`])
//! to one SQL `SELECT` over the canonical graph tables, for SQLite and
//! PostgreSQL (ADR-0034).
//!
//! * **Bindings.** A node variable is a row alias of
//!   `_forgebase_graph_nodes`, a relationship variable one of
//!   `_forgebase_graph_edges`; a variable-length relationship is a
//!   recursive CTE (a *walk*) that enumerates paths (`start`, `node`,
//!   `depth`, `path` = the edge ids) from the more constrained end of the
//!   pattern, at most [`MAX_WALK_ROWS`] of them. Every alias filters on the
//!   caller's tenant; every value is a bound parameter.
//! * **Semantics** follow the Apache AGE engine on the derived graph: a
//!   directed pattern follows the stored direction (`-[]->` from source to
//!   target, whether the edge is directed or not; `-[]-` either way), the
//!   fixed-length relationships of a `MATCH` clause are different edges and
//!   a walk never repeats an edge, but (as AGE 1.8) a walk may use an edge
//!   that another relationship of the clause matched; `n.__id`, `r.__id`,
//!   `r.__source` and
//!   `r.__target` read the ForgeBase ids, results come back as JSON in
//!   ForgeBase's shapes.
//! * **Rules** (FB-139): every node alias and every node a walk enters
//!   must pass the caller's label rules, every edge its edge-type rules, as
//!   in the REST API.
//! * **Values.** Each expression is compiled with a static type: SQL
//!   booleans, numbers and text where the type is known (literals,
//!   parameters, ids, aggregates), JSON where it is not (properties: JSON
//!   text on SQLite, `jsonb` on PostgreSQL). Comparisons of values of
//!   different types are `false` for `=`, `true` for `<>` and `null` for
//!   ordering, as openCypher says; `null` propagates.

use std::collections::HashMap;

use forgebase_data::query::SqlBuilder;
use forgebase_data::query::cypher::{
    CmpOp, CypherError, Direction, Expr, ExprKind, Literal, MatchClause, NodePattern, Query, RelPattern, StringOp,
};
use forgebase_db::{Dialect, Statement, Value};
use serde_json::{Map, Value as Json};

use crate::access::GraphAccess;

type Result<T> = std::result::Result<T, CypherError>;

const NODES: &str = "_forgebase_graph_nodes";
const EDGES: &str = "_forgebase_graph_edges";

/// Most paths one variable-length relationship may enumerate; more is
/// `cypher_too_complex`.
pub const MAX_WALK_ROWS: usize = 100_000;

/// The SQL of a query.
#[derive(Debug, Clone)]
pub(crate) struct Compiled {
    /// Column names, one per `RETURN` item.
    pub(crate) columns: Vec<String>,
    /// The query: one JSON text column per item (then hidden sort keys).
    pub(crate) main: Statement,
    /// With variable-length relationships: one row, the number of paths
    /// of each walk (capped at [`MAX_WALK_ROWS`] + 1).
    pub(crate) preflight: Option<Statement>,
    /// Whether the statement fetches the request's limit + 1 rows (more
    /// rows = truncated), not the query's own smaller `LIMIT`.
    pub(crate) detect_truncation: bool,
    /// Columns typed float (`avg`, float literals): PostgreSQL writes a
    /// whole float as `30`, the column says `30.0`.
    pub(crate) floats: Vec<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flavor {
    Sqlite,
    Pg,
}

/// Static type of a compiled expression (see the module docs).
#[derive(Debug, Clone, PartialEq)]
enum Ty {
    Null,
    Bool,
    Int,
    Float,
    /// A number of either kind (sums, JSON numbers read as numbers).
    Num,
    Str,
    /// JSON array.
    List,
    /// JSON object.
    Map,
    /// JSON of any type (a property).
    Any,
    /// A node row alias.
    Node(String),
    /// An edge row alias.
    Rel(String),
    /// A walk alias; `true` when it ran from the right end of its pattern.
    Walk(String, bool),
}

#[derive(Debug, Clone)]
struct C {
    sql: String,
    ty: Ty,
    /// The elements of a list literal or list parameter (for `IN`).
    items: Option<Vec<C>>,
}

impl C {
    fn new(sql: impl Into<String>, ty: Ty) -> Self {
        Self {
            sql: sql.into(),
            ty,
            items: None,
        }
    }
}

#[derive(Debug, Clone)]
enum Binding {
    Node(String),
    Rel(String),
    Walk(String, bool),
}

/// A walk planned in the first pass.
#[derive(Debug, Clone)]
struct Walk {
    /// Outer alias (`w0`).
    alias: String,
    /// Whether it starts at the right node of its pattern.
    reversed: bool,
    min: u32,
}

/// What a relationship of a `MATCH` clause binds, for edge uniqueness.
#[derive(Debug, Clone)]
enum EdgeRef {
    Fixed(String),
    Walk,
}

struct Compiler<'a, 'd> {
    q: &'a str,
    f: Flavor,
    b: SqlBuilder<'d>,
    params: &'a Map<String, Json>,
    acc: &'a GraphAccess,
    tenant: String,
    vars: HashMap<String, Binding>,
    froms: Vec<String>,
    conds: Vec<String>,
    ctes: Vec<String>,
    walks: HashMap<(usize, usize, usize), Walk>,
    counter: usize,
    /// Inside an aggregate's argument.
    in_agg: bool,
}

/// Compiles `query` for tenant `tenant` under the caller's rules `acc`;
/// `limit` is the request's row limit.
pub(crate) fn compile(
    d: &dyn Dialect,
    q: &str,
    query: &Query,
    params: &Map<String, Json>,
    acc: &GraphAccess,
    tenant: &str,
    limit: u32,
) -> Result<Compiled> {
    let f = if d.name() == "postgres" {
        Flavor::Pg
    } else {
        Flavor::Sqlite
    };
    let mut b = SqlBuilder::new(d);
    let tenant = b.bind(tenant.to_owned());
    let mut c = Compiler {
        q,
        f,
        b,
        params,
        acc,
        tenant,
        vars: HashMap::new(),
        froms: Vec::new(),
        conds: Vec::new(),
        ctes: Vec::new(),
        walks: HashMap::new(),
        counter: 0,
        in_agg: false,
    };
    // Pass 1: the walks (their CTEs use only the tenant and constants, so a
    // preflight can count them with the parameters bound so far).
    for (ci, clause) in query.matches.iter().enumerate() {
        for (pi, pattern) in clause.patterns.iter().enumerate() {
            let mut left = &pattern.start;
            for (si, (rel, right)) in pattern.steps.iter().enumerate() {
                if let Some((min, max)) = rel.length {
                    let w = c.plan_walk(clause, rel, left, right, min, max)?;
                    c.walks.insert((ci, pi, si), w);
                }
                left = right;
            }
        }
    }
    let preflight = (!c.ctes.is_empty()).then(|| {
        let counts: Vec<String> = (0..c.ctes.len())
            .map(|j| {
                format!(
                    "(SELECT count(*) FROM (SELECT 1 FROM walk{j} LIMIT {}) AS c{j})",
                    MAX_WALK_ROWS + 1
                )
            })
            .collect();
        Statement::new(
            format!("WITH RECURSIVE {} SELECT {}", c.ctes.join(", "), counts.join(", ")),
            c.b.params().to_vec(),
        )
    });
    // Pass 2: the patterns and conditions.
    for (ci, clause) in query.matches.iter().enumerate() {
        c.clause(ci, clause)?;
    }
    c.finish(query, limit, preflight)
}

impl Compiler<'_, '_> {
    fn next_id(&mut self) -> usize {
        self.counter += 1;
        self.counter
    }

    fn lit_false(&self) -> &'static str {
        match self.f {
            Flavor::Sqlite => "0",
            Flavor::Pg => "FALSE",
        }
    }

    fn lit_true(&self) -> &'static str {
        match self.f {
            Flavor::Sqlite => "1",
            Flavor::Pg => "TRUE",
        }
    }

    fn invalid(&self, at: usize, what: impl std::fmt::Display) -> CypherError {
        CypherError::invalid(self.q, at, what)
    }

    fn unsupported(&self, at: usize, what: &str) -> CypherError {
        CypherError::unsupported(self.q, at, what)
    }

    // ---- patterns --------------------------------------------------------

    /// Pass 1: the CTE of a variable-length relationship between `left`
    /// and `right`, walking from the more constrained end.
    fn plan_walk(
        &mut self,
        clause: &MatchClause,
        rel: &RelPattern,
        left: &NodePattern,
        right: &NodePattern,
        min: u32,
        max: u32,
    ) -> Result<Walk> {
        let score = |n: &NodePattern| 2 * usize::from(!n.props.is_empty()) + usize::from(!n.labels.is_empty());
        let reversed = score(right) > score(left);
        let start = if reversed { right } else { left };
        // Which way the walk steps: along `source → target` (forward) or
        // back, or both.
        let forward = match (rel.direction, reversed) {
            (Direction::Both, _) => None,
            (Direction::Right, false) | (Direction::Left, true) => Some(true),
            (Direction::Right, true) | (Direction::Left, false) => Some(false),
        };
        let j = self.ctes.len();
        let t = self.tenant.clone();

        // Anchor: the start node's candidates, with its labels, constant
        // properties, rules and the WHERE conjuncts about it alone.
        let saved = std::mem::take(&mut self.vars);
        let mut anchor = vec![format!("a.tenant = {t}")];
        for l in &start.labels {
            let p = self.b.bind(l.clone());
            anchor.push(format!("a.label = {p}"));
        }
        for (k, v) in &start.props {
            let cond = self.const_prop_eq(Ty::Node("a".into()), "a", k, v)?;
            anchor.push(cond);
        }
        if let Some(var) = &start.var
            && let Some(w) = &clause.filter
        {
            self.vars.clear();
            self.vars.insert(var.clone(), Binding::Node("a".into()));
            for conjunct in conjuncts(w) {
                if refs_only(conjunct, var) && !conjunct.has_aggregate() {
                    let c = self.expr(conjunct)?;
                    anchor.push(self.as_bool(&c, conjunct.span.start)?);
                }
            }
        }
        if let Some(v) = self
            .acc
            .node_sql(&mut self.b, "a")
            .map_err(|e| self.invalid(rel.span.start, e))?
        {
            anchor.push(v);
        }
        self.vars = saved;

        // Step: one edge on from the walk's node.
        let (join, next) = match forward {
            Some(true) => ("e.source = w.node".to_owned(), "e.target".to_owned()),
            Some(false) => ("e.target = w.node".to_owned(), "e.source".to_owned()),
            None => (
                "(e.source = w.node OR e.target = w.node)".to_owned(),
                "CASE WHEN e.source = w.node THEN e.target ELSE e.source END".to_owned(),
            ),
        };
        let maxp = self.b.bind(i64::from(max));
        let mut step = vec![format!("w.depth < {maxp}")];
        step.push(match self.f {
            Flavor::Sqlite => "instr(w.path, ',' || e.id || ',') = 0".to_owned(),
            Flavor::Pg => "NOT (e.id = ANY (w.path))".to_owned(),
        });
        if !rel.types.is_empty() {
            let list: Vec<String> = rel.types.iter().map(|ty| self.b.bind(ty.clone())).collect();
            step.push(format!("e.type IN ({})", list.join(", ")));
        }
        let saved = std::mem::take(&mut self.vars);
        for (k, v) in &rel.props {
            let cond = self.const_prop_eq(Ty::Rel("e".into()), "e", k, v)?;
            step.push(cond);
        }
        self.vars = saved;
        if let Some(v) = self
            .acc
            .edges
            .compile(&mut self.b, "e")
            .map_err(|e| self.invalid(rel.span.start, e))?
        {
            step.push(v);
        }
        if let Some(v) = self
            .acc
            .node_sql(&mut self.b, "wn")
            .map_err(|e| self.invalid(rel.span.start, e))?
        {
            step.push(format!(
                "EXISTS (SELECT 1 FROM {NODES} AS wn WHERE wn.tenant = e.tenant AND wn.id = {next} AND {v})"
            ));
        }
        let (empty, append) = match self.f {
            Flavor::Sqlite => ("','", "w.path || e.id || ','"),
            Flavor::Pg => ("CAST(ARRAY[] AS text[])", "w.path || e.id"),
        };
        // SQLite stops the recursion at the cap; PostgreSQL produces rows on
        // demand, so the consumer's LIMIT stops it (no LIMIT allowed inside).
        // SQLite's CROSS JOIN keeps the walk queue the outer loop (ADR-0006).
        let (cap, cross) = match self.f {
            Flavor::Sqlite => (format!(" LIMIT {}", MAX_WALK_ROWS + 1), "CROSS JOIN"),
            Flavor::Pg => (String::new(), "JOIN"),
        };
        self.ctes.push(format!(
            "walk{j} (start, node, depth, path) AS (SELECT a.id, a.id, 0, {empty} FROM {NODES} AS a WHERE {} \
             UNION ALL SELECT w.start, {next}, w.depth + 1, {append} FROM walk{j} AS w {cross} {EDGES} AS e \
             ON e.tenant = {t} AND {join} WHERE {}{cap})",
            anchor.join(" AND "),
            step.join(" AND ")
        ));
        Ok(Walk {
            alias: format!("w{j}"),
            reversed,
            min,
        })
    }

    /// `key = value` on row `alias` (a node or an edge) where `value` must
    /// not refer to variables (it is used inside a walk).
    fn const_prop_eq(&mut self, ty: Ty, alias: &str, key: &str, value: &Expr) -> Result<String> {
        let mut vars = Vec::new();
        value.walk(&mut |e| {
            if let ExprKind::Var(v) = &e.kind {
                vars.push((v.clone(), e.span.start));
            }
        });
        if let Some((_, at)) = vars.first() {
            return Err(self.unsupported(
                *at,
                "a variable in the property map of a variable-length relationship or its start node",
            ));
        }
        let base = C::new(alias, ty);
        let prop = self.prop(&base, key, value.span.start)?;
        let v = self.expr(value)?;
        let eq = self.cmp(CmpOp::Eq, &prop, &v)?;
        self.as_bool(&eq, value.span.start)
    }

    /// Pass 2: one `MATCH` clause.
    fn clause(&mut self, ci: usize, clause: &MatchClause) -> Result<()> {
        let mut edges: Vec<EdgeRef> = Vec::new();
        for (pi, pattern) in clause.patterns.iter().enumerate() {
            // Only a node at an end of a chain may go without a row: one
            // in the middle links its two relationships.
            let last = pattern.steps.len();
            let mut left = self.node(&pattern.start, last == 0)?;
            for (si, (rel, right_np)) in pattern.steps.iter().enumerate() {
                let right = self.node(right_np, si + 1 < last)?;
                let walk = self.walks.get(&(ci, pi, si)).cloned();
                edges.push(self.rel(rel, left.as_deref(), right.as_deref(), walk)?);
                left = right;
            }
        }
        // Edge uniqueness within the clause, between fixed-length
        // relationships (a walk keeps its own edges unique; AGE 1.8 does not
        // compare a walk's edges with the clause's other relationships).
        let fixed: Vec<&str> = edges
            .iter()
            .filter_map(|e| match e {
                EdgeRef::Fixed(a) => Some(a.as_str()),
                EdgeRef::Walk => None,
            })
            .collect();
        for (i, x) in fixed.iter().enumerate() {
            for y in &fixed[i + 1..] {
                self.conds.push(format!("{x}.id <> {y}.id"));
            }
        }
        if let Some(w) = &clause.filter {
            if w.has_aggregate() {
                return Err(self.invalid(w.span.start, "aggregate functions are not allowed in WHERE"));
            }
            let c = self.expr(w)?;
            let cond = self.as_bool(&c, w.span.start)?;
            self.conds.push(cond);
        }
        Ok(())
    }

    /// A node pattern: its alias (`None`: an anonymous, unconstrained node
    /// at the end of a chain, which needs no row of its own: the edge's
    /// foreign key says it exists, and no rule restricts it). `linked`: it
    /// must have a row (a lone node, or one between two relationships).
    fn node(&mut self, np: &NodePattern, linked: bool) -> Result<Option<String>> {
        let at = np.span.start;
        let alias = match np.var.as_ref().and_then(|v| self.vars.get(v)) {
            Some(Binding::Node(a)) => a.clone(),
            Some(_) => {
                return Err(self.invalid(
                    at,
                    format_args!(
                        "{} is a relationship, not a node",
                        np.var.as_deref().unwrap_or_default()
                    ),
                ));
            }
            None => {
                if np.var.is_none()
                    && !linked
                    && np.labels.is_empty()
                    && np.props.is_empty()
                    && self.acc.nodes.is_unrestricted()
                {
                    return Ok(None);
                }
                let a = format!("n{}", self.next_id());
                self.froms.push(format!("{NODES} AS {a}"));
                self.conds.push(format!("{a}.tenant = {}", self.tenant));
                if let Some(v) = self.acc.node_sql(&mut self.b, &a).map_err(|e| self.invalid(at, e))? {
                    self.conds.push(v);
                }
                if let Some(v) = &np.var {
                    self.vars.insert(v.clone(), Binding::Node(a.clone()));
                }
                a
            }
        };
        for l in &np.labels {
            let p = self.b.bind(l.clone());
            self.conds.push(format!("{alias}.label = {p}"));
        }
        for (k, v) in &np.props {
            let base = C::new(alias.clone(), Ty::Node(alias.clone()));
            let prop = self.prop(&base, k, v.span.start)?;
            let val = self.expr(v)?;
            let eq = self.cmp(CmpOp::Eq, &prop, &val)?;
            let cond = self.as_bool(&eq, v.span.start)?;
            self.conds.push(cond);
        }
        Ok(Some(alias))
    }

    /// A relationship between nodes `l` and `r` (aliases, `None` when not
    /// materialised).
    fn rel(&mut self, rp: &RelPattern, l: Option<&str>, r: Option<&str>, walk: Option<Walk>) -> Result<EdgeRef> {
        let at = rp.span.start;
        if let Some(v) = &rp.var
            && self.vars.contains_key(v)
        {
            return Err(self.unsupported(at, "a relationship variable bound twice"));
        }
        if let Some(w) = walk {
            let a = w.alias.clone();
            let j = &a[1..];
            self.froms.push(match self.f {
                Flavor::Sqlite => format!("walk{j} AS {a}"),
                Flavor::Pg => format!("(SELECT * FROM walk{j} LIMIT {}) AS {a}", MAX_WALK_ROWS + 1),
            });
            let (start, end) = if w.reversed { (r, l) } else { (l, r) };
            if w.min > 0 {
                let p = self.b.bind(i64::from(w.min));
                self.conds.push(format!("{a}.depth >= {p}"));
            }
            if let Some(s) = start {
                self.conds.push(format!("{a}.start = {s}.id"));
            }
            if let Some(e) = end {
                self.conds.push(format!("{a}.node = {e}.id"));
            }
            if let Some(v) = &rp.var {
                self.vars.insert(v.clone(), Binding::Walk(a.clone(), w.reversed));
            }
            return Ok(EdgeRef::Walk);
        }
        let e = format!("e{}", self.next_id());
        self.froms.push(format!("{EDGES} AS {e}"));
        self.conds.push(format!("{e}.tenant = {}", self.tenant));
        let ends = |s: &str, t: &str| format!("{e}.source = {s}.id AND {e}.target = {t}.id");
        let cond = match (rp.direction, l, r) {
            (Direction::Right, Some(l), Some(r)) => Some(ends(l, r)),
            (Direction::Left, Some(l), Some(r)) => Some(ends(r, l)),
            (Direction::Both, Some(l), Some(r)) => Some(format!("(({}) OR ({}))", ends(l, r), ends(r, l))),
            (Direction::Right, Some(l), None) | (Direction::Left, None, Some(l)) => {
                Some(format!("{e}.source = {l}.id"))
            }
            (Direction::Right, None, Some(r)) | (Direction::Left, Some(r), None) => {
                Some(format!("{e}.target = {r}.id"))
            }
            (Direction::Both, Some(x), None) | (Direction::Both, None, Some(x)) => {
                Some(format!("({e}.source = {x}.id OR {e}.target = {x}.id)"))
            }
            (_, None, None) => None,
        };
        if let Some(c) = cond {
            self.conds.push(c);
        }
        if !rp.types.is_empty() {
            let list: Vec<String> = rp.types.iter().map(|t| self.b.bind(t.clone())).collect();
            self.conds.push(format!("{e}.type IN ({})", list.join(", ")));
        }
        if let Some(v) = self
            .acc
            .edges
            .compile(&mut self.b, &e)
            .map_err(|x| self.invalid(at, x))?
        {
            self.conds.push(v);
        }
        if let Some(v) = &rp.var {
            self.vars.insert(v.clone(), Binding::Rel(e.clone()));
        }
        for (k, v) in &rp.props {
            let base = C::new(e.clone(), Ty::Rel(e.clone()));
            let prop = self.prop(&base, k, v.span.start)?;
            let val = self.expr(v)?;
            let eq = self.cmp(CmpOp::Eq, &prop, &val)?;
            let cond = self.as_bool(&eq, v.span.start)?;
            self.conds.push(cond);
        }
        Ok(EdgeRef::Fixed(e))
    }

    // ---- RETURN ----------------------------------------------------------

    fn finish(mut self, query: &Query, limit: u32, preflight: Option<Statement>) -> Result<Compiled> {
        let ret = &query.ret;
        let aggregating = ret.items.iter().any(|i| i.expr.has_aggregate());
        let mut outs: Vec<C> = Vec::with_capacity(ret.items.len());
        for item in &ret.items {
            outs.push(self.expr(&item.expr)?);
        }
        let mut select: Vec<String> = outs
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let j = self.to_json(c);
                match self.f {
                    Flavor::Sqlite => format!("{j} AS c{i}"),
                    Flavor::Pg => format!("CAST({j} AS text) AS c{i}"),
                }
            })
            .collect();
        let mut group: Vec<String> = Vec::new();
        if aggregating {
            for (item, c) in ret.items.iter().zip(&outs) {
                if item.expr.has_aggregate() {
                    continue;
                }
                match &c.ty {
                    Ty::Null => {}
                    Ty::Node(a) | Ty::Rel(a) => {
                        group.push(format!("{a}.tenant"));
                        group.push(format!("{a}.id"));
                    }
                    Ty::Walk(w, _) => {
                        group.push(format!("{w}.start"));
                        group.push(format!("{w}.node"));
                        group.push(format!("{w}.path"));
                    }
                    _ => group.push(c.sql.clone()),
                }
            }
        }
        // ORDER BY: a returned column (by alias or by its expression), or,
        // without DISTINCT and aggregation, any expression.
        let mut order: Vec<String> = Vec::new();
        for (k, s) in ret.order.iter().enumerate() {
            let by_alias = match &s.expr.kind {
                ExprKind::Var(v) => ret.items.iter().position(|i| i.alias.as_deref() == Some(v.as_str())),
                _ => None,
            };
            let found = by_alias.or_else(|| ret.items.iter().position(|i| same_expr(&i.expr, &s.expr)));
            // After DISTINCT or an aggregation, an expression over returned
            // variables (`RETURN p, count(*) ORDER BY p.name`) depends on
            // the returned columns alone.
            let over_returned = !s.expr.has_aggregate() && {
                let mut ok = true;
                s.expr.walk(&mut |x| {
                    if let ExprKind::Var(v) = &x.kind {
                        ok &= ret
                            .items
                            .iter()
                            .any(|i| matches!(&i.expr.kind, ExprKind::Var(w) if w == v) && i.alias.is_none());
                    }
                });
                ok
            };
            let c = match found {
                Some(i) => outs[i].clone(),
                None if (ret.distinct || aggregating) && over_returned => self.expr(&s.expr)?,
                None if ret.distinct || aggregating => {
                    return Err(self.invalid(
                        s.expr.span.start,
                        "after DISTINCT or an aggregation, ORDER BY may only use returned columns",
                    ));
                }
                None => {
                    if s.expr.has_aggregate() {
                        return Err(self.invalid(s.expr.span.start, "ORDER BY cannot aggregate here"));
                    }
                    self.expr(&s.expr)?
                }
            };
            let dir = if s.desc { "DESC NULLS FIRST" } else { "ASC NULLS LAST" };
            for (n, key) in self.sort_keys(&c).into_iter().enumerate() {
                let name = format!("s{k}_{n}");
                select.push(format!("{key} AS {name}"));
                order.push(format!("{name} {dir}"));
            }
        }
        let skip = ret
            .skip
            .as_ref()
            .map(|e| self.count_value(e, "SKIP"))
            .transpose()?
            .unwrap_or(0);
        let own = ret.limit.as_ref().map(|e| self.count_value(e, "LIMIT")).transpose()?;
        let request = i64::from(limit);
        let (fetch, detect_truncation) = match own {
            Some(l) if l <= request => (l, false),
            _ => (request + 1, true),
        };
        let mut sql = String::new();
        if !self.ctes.is_empty() {
            sql.push_str(&format!("WITH RECURSIVE {} ", self.ctes.join(", ")));
        }
        sql.push_str("SELECT ");
        if ret.distinct {
            sql.push_str("DISTINCT ");
        }
        sql.push_str(&select.join(", "));
        if !self.froms.is_empty() {
            sql.push_str(&format!(" FROM {}", self.froms.join(", ")));
        }
        if !self.conds.is_empty() {
            let conds: Vec<String> = self.conds.iter().map(|c| format!("({c})")).collect();
            sql.push_str(&format!(" WHERE {}", conds.join(" AND ")));
        }
        if !group.is_empty() {
            sql.push_str(&format!(" GROUP BY {}", group.join(", ")));
        }
        if !order.is_empty() {
            sql.push_str(&format!(" ORDER BY {}", order.join(", ")));
        }
        let lim = self.b.bind(fetch);
        let off = self.b.bind(skip);
        sql.push_str(&format!(" LIMIT {lim} OFFSET {off}"));
        Ok(Compiled {
            columns: ret.items.iter().map(|i| i.name.clone()).collect(),
            floats: outs.iter().map(|c| c.ty == Ty::Float).collect(),
            main: Statement::new(sql, self.b.into_params()),
            preflight,
            detect_truncation,
        })
    }

    /// `SKIP` / `LIMIT`: a non-negative integer literal or parameter.
    fn count_value(&self, e: &Expr, what: &str) -> Result<i64> {
        let v = match &e.kind {
            ExprKind::Lit(Literal::Int(n)) => Some(*n),
            ExprKind::Param(p) => self.params.get(p).and_then(Json::as_i64),
            _ => None,
        };
        match v {
            Some(n) if n >= 0 => Ok(n),
            _ => Err(self.invalid(
                e.span.start,
                format_args!("{what} needs a non-negative integer (a literal or a parameter)"),
            )),
        }
    }

    /// SQL keys that order values of `c` as openCypher does (within a type;
    /// across types by type).
    fn sort_keys(&self, c: &C) -> Vec<String> {
        match &c.ty {
            Ty::Null => Vec::new(),
            Ty::Bool | Ty::Int | Ty::Float | Ty::Num | Ty::Str => vec![c.sql.clone()],
            Ty::Node(a) | Ty::Rel(a) => vec![format!("{a}.id")],
            Ty::Walk(w, _) => vec![format!("{w}.path")],
            Ty::List | Ty::Map | Ty::Any => {
                let x = &c.sql;
                match self.f {
                    Flavor::Sqlite => vec![
                        format!(
                            "(CASE json_type({x}) WHEN 'object' THEN 1 WHEN 'array' THEN 4 WHEN 'text' THEN 6 \
                             WHEN 'true' THEN 7 WHEN 'false' THEN 7 WHEN 'integer' THEN 8 WHEN 'real' THEN 8 END)"
                        ),
                        format!("({x} ->> '$')"),
                    ],
                    Flavor::Pg => vec![
                        format!(
                            "(CASE jsonb_typeof({x}) WHEN 'object' THEN 1 WHEN 'array' THEN 4 WHEN 'string' THEN 6 \
                             WHEN 'boolean' THEN 7 WHEN 'number' THEN 8 END)"
                        ),
                        x.clone(),
                    ],
                }
            }
        }
    }

    // ---- expressions -----------------------------------------------------

    fn expr(&mut self, e: &Expr) -> Result<C> {
        let at = e.span.start;
        Ok(match &e.kind {
            ExprKind::Lit(l) => self.literal(l),
            ExprKind::Param(p) => {
                let v = self
                    .params
                    .get(p)
                    .cloned()
                    .ok_or_else(|| self.invalid(at, format_args!("missing parameter ${p}")))?;
                self.json_value(&v)
            }
            ExprKind::Var(v) => match self.vars.get(v) {
                Some(Binding::Node(a)) => C::new(a.clone(), Ty::Node(a.clone())),
                Some(Binding::Rel(a)) => C::new(a.clone(), Ty::Rel(a.clone())),
                Some(Binding::Walk(a, r)) => C::new(a.clone(), Ty::Walk(a.clone(), *r)),
                None => return Err(self.invalid(at, format_args!("variable {v} is not defined"))),
            },
            ExprKind::Prop(base, key) => {
                let b = self.expr(base)?;
                self.prop(&b, key, at)?
            }
            ExprKind::List(items) => {
                let mut cs = Vec::with_capacity(items.len());
                for i in items {
                    cs.push(self.expr(i)?);
                }
                let parts: Vec<String> = cs.iter().map(|c| self.to_json(c)).collect();
                let sql = match self.f {
                    Flavor::Sqlite => format!("json_array({})", parts.join(", ")),
                    Flavor::Pg => format!("jsonb_build_array({})", parts.join(", ")),
                };
                C {
                    sql,
                    ty: Ty::List,
                    items: Some(cs),
                }
            }
            ExprKind::Not(x) => {
                let c = self.expr(x)?;
                let b = self.as_bool(&c, x.span.start)?;
                C::new(format!("(NOT {b})"), Ty::Bool)
            }
            ExprKind::And(a, b) | ExprKind::Or(a, b) => {
                let op = if matches!(e.kind, ExprKind::And(..)) {
                    "AND"
                } else {
                    "OR"
                };
                let ca = self.expr(a)?;
                let cb = self.expr(b)?;
                let sa = self.as_bool(&ca, a.span.start)?;
                let sb = self.as_bool(&cb, b.span.start)?;
                C::new(format!("({sa} {op} {sb})"), Ty::Bool)
            }
            ExprKind::Cmp(op, a, b) => {
                let ca = self.expr(a)?;
                let cb = self.expr(b)?;
                self.cmp(*op, &ca, &cb)?
            }
            ExprKind::IsNull { expr, negated } => {
                let c = self.expr(expr)?;
                let is = match &c.ty {
                    Ty::Node(_) | Ty::Rel(_) | Ty::Walk(..) => self.lit_false().to_owned(),
                    Ty::Null => self.lit_true().to_owned(),
                    _ => format!("({} IS NULL)", c.sql),
                };
                C::new(if *negated { format!("(NOT {is})") } else { is }, Ty::Bool)
            }
            ExprKind::In(a, list) => {
                let ca = self.expr(a)?;
                let cl = self.expr(list)?;
                self.in_list(&ca, &cl, list.span.start)?
            }
            ExprKind::Str(op, a, b) => {
                let ca = self.expr(a)?;
                let cb = self.expr(b)?;
                self.str_op(*op, &ca, &cb)
            }
            ExprKind::Neg(x) => {
                let c = self.expr(x)?;
                match self.num(&c) {
                    Some(n) => C::new(format!("(-{n})"), if c.ty == Ty::Int { Ty::Int } else { Ty::Num }),
                    None => return Err(self.invalid(at, "unary minus needs a number")),
                }
            }
            ExprKind::Call {
                name,
                distinct,
                args,
                star,
            } => self.call(name, *distinct, args, *star, at)?,
        })
    }

    fn literal(&mut self, l: &Literal) -> C {
        match l {
            Literal::Null => C::new("NULL", Ty::Null),
            Literal::Bool(v) => C::new(if *v { self.lit_true() } else { self.lit_false() }, Ty::Bool),
            Literal::Int(n) => {
                let p = self.b.bind(*n);
                C::new(self.typed(&p, "bigint"), Ty::Int)
            }
            Literal::Float(x) => {
                let p = self.b.bind(Value::Real(*x));
                C::new(self.typed(&p, "double precision"), Ty::Float)
            }
            Literal::Str(s) => {
                let p = self.b.bind(s.clone());
                C::new(self.typed(&p, "text"), Ty::Str)
            }
        }
    }

    /// A placeholder with its PostgreSQL type (SQLite needs none).
    fn typed(&self, p: &str, ty: &str) -> String {
        match self.f {
            Flavor::Sqlite => p.to_owned(),
            Flavor::Pg => format!("CAST({p} AS {ty})"),
        }
    }

    /// A parameter's JSON value as a compiled expression.
    fn json_value(&mut self, v: &Json) -> C {
        match v {
            Json::Null => C::new("NULL", Ty::Null),
            Json::Bool(b) => self.literal(&Literal::Bool(*b)),
            Json::Number(n) => match n.as_i64() {
                Some(i) => self.literal(&Literal::Int(i)),
                None => self.literal(&Literal::Float(n.as_f64().unwrap_or(0.0))),
            },
            Json::String(s) => self.literal(&Literal::Str(s.clone())),
            Json::Array(items) => {
                let p = self.b.bind(v.to_string());
                let sql = match self.f {
                    Flavor::Sqlite => format!("json({p})"),
                    Flavor::Pg => format!("CAST({p} AS jsonb)"),
                };
                let cs = items.iter().map(|i| self.json_value(i)).collect();
                C {
                    sql,
                    ty: Ty::List,
                    items: Some(cs),
                }
            }
            Json::Object(_) => {
                let p = self.b.bind(v.to_string());
                let sql = match self.f {
                    Flavor::Sqlite => format!("json({p})"),
                    Flavor::Pg => format!("CAST({p} AS jsonb)"),
                };
                C::new(sql, Ty::Map)
            }
        }
    }

    /// `base.key`.
    fn prop(&mut self, base: &C, key: &str, at: usize) -> Result<C> {
        let column = |a: &str, col: &str| C::new(format!("{a}.{col}"), Ty::Str);
        let json = match &base.ty {
            Ty::Node(a) => {
                if key == "__id" {
                    return Ok(column(a, "id"));
                }
                format!("{a}.properties")
            }
            Ty::Rel(a) => {
                match key {
                    "__id" => return Ok(column(a, "id")),
                    "__source" => return Ok(column(a, "source")),
                    "__target" => return Ok(column(a, "target")),
                    _ => {}
                }
                format!("{a}.properties")
            }
            Ty::Map | Ty::Any => base.sql.clone(),
            Ty::Null => return Ok(C::new("NULL", Ty::Null)),
            Ty::Walk(..) => {
                return Err(self.invalid(at, "a variable-length relationship is a list: it has no properties"));
            }
            _ => {
                return Err(self.invalid(
                    at,
                    format_args!("type mismatch: cannot read property {key:?} of this value"),
                ));
            }
        };
        if key.contains(['"', '\\']) || key.chars().any(char::is_control) {
            return Err(self.unsupported(at, "a property name with quotes, backslashes or control characters"));
        }
        let sql = match self.f {
            Flavor::Sqlite => {
                let p = self.b.bind(format!("$.\"{key}\""));
                format!("nullif({json} -> {p}, 'null')")
            }
            Flavor::Pg => {
                let p = self.b.bind(key.to_owned());
                format!("nullif({json} -> CAST({p} AS text), 'null'::jsonb)")
            }
        };
        Ok(C::new(sql, Ty::Any))
    }

    /// The value as JSON (SQLite: JSON text with the JSON subtype;
    /// PostgreSQL: `jsonb`).
    fn to_json(&self, c: &C) -> String {
        let x = &c.sql;
        match (&c.ty, self.f) {
            (Ty::Null, _) => "NULL".to_owned(),
            (Ty::Bool, Flavor::Sqlite) => {
                format!("(CASE WHEN {x} THEN json('true') WHEN NOT {x} THEN json('false') END)")
            }
            (Ty::Int | Ty::Float | Ty::Num | Ty::Str, Flavor::Sqlite) => format!("json_quote({x})"),
            (Ty::List | Ty::Map | Ty::Any, Flavor::Sqlite) => format!("json({x})"),
            (Ty::Bool | Ty::Int | Ty::Float | Ty::Num | Ty::Str, Flavor::Pg) => format!("to_jsonb({x})"),
            (Ty::List | Ty::Map | Ty::Any, Flavor::Pg) => x.clone(),
            (Ty::Node(a), Flavor::Sqlite) => {
                format!("json_object('id', {a}.id, 'label', {a}.label, 'properties', json({a}.properties))")
            }
            (Ty::Node(a), Flavor::Pg) => {
                format!("jsonb_build_object('id', {a}.id, 'label', {a}.label, 'properties', {a}.properties)")
            }
            (Ty::Rel(a), f) => edge_json(f, a),
            (Ty::Walk(w, rev), Flavor::Sqlite) => {
                let dir = if *rev { "DESC" } else { "ASC" };
                format!(
                    "(SELECT json_group_array(json({}) ORDER BY wj.key {dir}) FROM json_each('[\"' || \
                     replace(substr({w}.path, 2, length({w}.path) - 2), ',', '\",\"') || '\"]') AS wj \
                     JOIN {EDGES} AS we ON we.tenant = {t} AND we.id = wj.value)",
                    edge_json(Flavor::Sqlite, "we"),
                    t = self.tenant
                )
            }
            (Ty::Walk(w, rev), Flavor::Pg) => {
                let dir = if *rev { "DESC" } else { "ASC" };
                format!(
                    "(SELECT coalesce(jsonb_agg({} ORDER BY wj.ord {dir}), '[]'::jsonb) FROM unnest({w}.path) \
                     WITH ORDINALITY AS wj(id, ord) JOIN {EDGES} AS we ON we.tenant = {t} AND we.id = wj.id)",
                    edge_json(Flavor::Pg, "we"),
                    t = self.tenant
                )
            }
        }
    }

    /// A boolean condition from `c`.
    fn as_bool(&self, c: &C, at: usize) -> Result<String> {
        let x = &c.sql;
        Ok(match (&c.ty, self.f) {
            (Ty::Bool, _) => x.clone(),
            (Ty::Null, _) => "NULL".to_owned(),
            (Ty::Any, Flavor::Sqlite) => format!("(CASE json_type({x}) WHEN 'true' THEN 1 WHEN 'false' THEN 0 END)"),
            (Ty::Any, Flavor::Pg) => format!("(CASE WHEN jsonb_typeof({x}) = 'boolean' THEN ({x})::boolean END)"),
            _ => return Err(self.invalid(at, "type mismatch: expected a boolean")),
        })
    }

    /// `c` as an SQL number, `None` when it cannot be one.
    fn num(&self, c: &C) -> Option<String> {
        let x = &c.sql;
        match (&c.ty, self.f) {
            (Ty::Int | Ty::Float | Ty::Num, _) => Some(x.clone()),
            (Ty::Null, _) => Some("NULL".into()),
            (Ty::Any, Flavor::Sqlite) => Some(format!(
                "(CASE WHEN json_type({x}) IN ('integer', 'real') THEN {x} ->> '$' END)"
            )),
            (Ty::Any, Flavor::Pg) => Some(format!(
                "(CASE WHEN jsonb_typeof({x}) = 'number' THEN ({x})::numeric END)"
            )),
            _ => None,
        }
    }

    /// `c` as SQL text, `None` when it cannot be one.
    fn text(&self, c: &C) -> Option<String> {
        let x = &c.sql;
        match (&c.ty, self.f) {
            (Ty::Str, _) => Some(x.clone()),
            (Ty::Null, _) => Some("NULL".into()),
            (Ty::Any, Flavor::Sqlite) => Some(format!("(CASE WHEN json_type({x}) = 'text' THEN {x} ->> '$' END)")),
            (Ty::Any, Flavor::Pg) => Some(format!(
                "(CASE WHEN jsonb_typeof({x}) = 'string' THEN {x} #>> '{{}}' END)"
            )),
            _ => None,
        }
    }

    /// `c` (JSON) as an SQL boolean.
    fn json_bool(&self, x: &str) -> String {
        match self.f {
            Flavor::Sqlite => format!("(CASE json_type({x}) WHEN 'true' THEN 1 WHEN 'false' THEN 0 END)"),
            Flavor::Pg => format!("(CASE WHEN jsonb_typeof({x}) = 'boolean' THEN ({x})::boolean END)"),
        }
    }

    /// The JSON type class of a JSON value, as the engine names it.
    fn class_of(&self, x: &str) -> String {
        match self.f {
            Flavor::Sqlite => format!(
                "(CASE json_type({x}) WHEN 'integer' THEN 'n' WHEN 'real' THEN 'n' WHEN 'text' THEN 's' \
                 WHEN 'true' THEN 'b' WHEN 'false' THEN 'b' ELSE json_type({x}) END)"
            ),
            Flavor::Pg => format!("jsonb_typeof({x})"),
        }
    }

    /// The class name of a static type, in [`Self::class_of`]'s terms.
    fn class_name(&self, ty: &Ty) -> Option<&'static str> {
        Some(match (ty, self.f) {
            (Ty::Int | Ty::Float | Ty::Num, Flavor::Sqlite) => "n",
            (Ty::Int | Ty::Float | Ty::Num, Flavor::Pg) => "number",
            (Ty::Str, Flavor::Sqlite) => "s",
            (Ty::Str, Flavor::Pg) => "string",
            (Ty::Bool, Flavor::Sqlite) => "b",
            (Ty::Bool, Flavor::Pg) => "boolean",
            (Ty::List, _) => "array",
            (Ty::Map, _) => "object",
            _ => return None,
        })
    }

    /// What a comparison of values of different types gives.
    fn mismatch(&self, op: CmpOp) -> &'static str {
        match op {
            CmpOp::Eq => self.lit_false(),
            CmpOp::Ne => self.lit_true(),
            _ => "NULL",
        }
    }

    fn json_eq(&self, op: CmpOp, a: &str, b: &str) -> String {
        match (op, self.f) {
            (CmpOp::Eq | CmpOp::Ne, Flavor::Sqlite) => format!("(json({a}) {} json({b}))", op.sql()),
            (CmpOp::Eq | CmpOp::Ne, Flavor::Pg) => format!("({a} {} {b})", op.sql()),
            _ => "NULL".to_owned(),
        }
    }

    fn cmp(&mut self, op: CmpOp, a: &C, b: &C) -> Result<C> {
        let o = op.sql();
        let bool_c = |s: String| C::new(s, Ty::Bool);
        let scalar = |t: &Ty| matches!(t, Ty::Bool | Ty::Int | Ty::Float | Ty::Num | Ty::Str);
        let same_class = |x: &Ty, y: &Ty| {
            matches!(
                (x, y),
                (Ty::Int | Ty::Float | Ty::Num, Ty::Int | Ty::Float | Ty::Num)
                    | (Ty::Str, Ty::Str)
                    | (Ty::Bool, Ty::Bool)
                    | (Ty::List, Ty::List)
                    | (Ty::Map, Ty::Map)
            )
        };
        Ok(match (&a.ty, &b.ty) {
            (Ty::Null, _) | (_, Ty::Null) => bool_c("NULL".into()),
            (Ty::Node(x), Ty::Node(y)) | (Ty::Rel(x), Ty::Rel(y)) => match op {
                CmpOp::Eq | CmpOp::Ne => bool_c(format!("({x}.id {o} {y}.id)")),
                _ => bool_c("NULL".into()),
            },
            (Ty::Walk(..), _) | (_, Ty::Walk(..)) => {
                return Err(CypherError::unsupported(
                    self.q,
                    0,
                    "comparing variable-length relationships",
                ));
            }
            (Ty::Node(_) | Ty::Rel(_), _) | (_, Ty::Node(_) | Ty::Rel(_)) => bool_c(self.mismatch(op).into()),
            (x, y) if scalar(x) && scalar(y) => {
                if same_class(x, y) {
                    bool_c(format!("({} {o} {})", a.sql, b.sql))
                } else {
                    bool_c(self.mismatch(op).into())
                }
            }
            (Ty::List | Ty::Map, Ty::List | Ty::Map) => {
                if same_class(&a.ty, &b.ty) {
                    bool_c(self.json_eq(op, &a.sql, &b.sql))
                } else {
                    bool_c(self.mismatch(op).into())
                }
            }
            (Ty::Any, Ty::Any) => {
                let (x, y) = (&a.sql, &b.sql);
                let (cx, cy) = (self.class_of(x), self.class_of(y));
                let (n, s) = match self.f {
                    Flavor::Sqlite => ("n", "s"),
                    Flavor::Pg => ("number", "string"),
                };
                let bl = if self.f == Flavor::Sqlite { "b" } else { "boolean" };
                let ca = C::new(x.clone(), Ty::Any);
                let cb = C::new(y.clone(), Ty::Any);
                let (nx, ny) = (self.num(&ca).unwrap_or_default(), self.num(&cb).unwrap_or_default());
                let (sx, sy) = (self.text(&ca).unwrap_or_default(), self.text(&cb).unwrap_or_default());
                let (bx, by) = (self.json_bool(x), self.json_bool(y));
                bool_c(format!(
                    "(CASE WHEN {x} IS NULL OR {y} IS NULL THEN NULL WHEN {cx} <> {cy} THEN {mm} \
                     WHEN {cx} = '{n}' THEN {nx} {o} {ny} WHEN {cx} = '{s}' THEN {sx} {o} {sy} \
                     WHEN {cx} = '{bl}' THEN {bx} {o} {by} ELSE {je} END)",
                    mm = self.mismatch(op),
                    je = self.json_eq(op, x, y)
                ))
            }
            // One side JSON, the other statically typed.
            (Ty::Any, t) | (t, Ty::Any) => {
                let dyn_left = a.ty == Ty::Any;
                let (d, st) = if dyn_left { (a, b) } else { (b, a) };
                let class = self.class_name(t).unwrap_or("?");
                let guard = format!("{} = '{class}'", self.class_of(&d.sql));
                let conv = match t {
                    Ty::Int | Ty::Float | Ty::Num => self.num(d),
                    Ty::Str => self.text(d),
                    Ty::Bool => Some(self.json_bool(&d.sql)),
                    _ => None,
                };
                let test = match conv {
                    Some(cv) if dyn_left => format!("{cv} {o} {}", st.sql),
                    Some(cv) => format!("{} {o} {cv}", st.sql),
                    None => self.json_eq(op, &d.sql, &self.to_json(st)),
                };
                bool_c(format!(
                    "(CASE WHEN {} IS NULL THEN NULL WHEN {guard} THEN {test} ELSE {} END)",
                    d.sql,
                    self.mismatch(op)
                ))
            }
            _ => bool_c(self.mismatch(op).into()),
        })
    }

    fn in_list(&mut self, a: &C, list: &C, at: usize) -> Result<C> {
        if let Some(items) = &list.items {
            if items.is_empty() {
                return Ok(C::new(self.lit_false(), Ty::Bool));
            }
            let mut parts = Vec::with_capacity(items.len());
            for i in items {
                parts.push(self.cmp(CmpOp::Eq, a, i)?.sql);
            }
            return Ok(C::new(format!("({})", parts.join(" OR ")), Ty::Bool));
        }
        let l = &list.sql;
        let j = format!("j{}", self.next_id());
        let (from, elem, guard) = match self.f {
            Flavor::Sqlite => (
                format!("json_each({l}) AS {j}"),
                format!(
                    "(CASE {j}.type WHEN 'null' THEN NULL WHEN 'true' THEN json('true') WHEN 'false' THEN json('false') \
                     WHEN 'text' THEN json_quote({j}.value) WHEN 'integer' THEN json_quote({j}.value) \
                     WHEN 'real' THEN json_quote({j}.value) ELSE json({j}.value) END)"
                ),
                format!("json_type({l}) = 'array'"),
            ),
            Flavor::Pg => (
                format!("jsonb_array_elements({l}) AS {j}(v)"),
                format!("nullif({j}.v, 'null'::jsonb)"),
                format!("jsonb_typeof({l}) = 'array'"),
            ),
        };
        match &list.ty {
            Ty::List | Ty::Any => {}
            Ty::Null => return Ok(C::new("NULL", Ty::Bool)),
            _ => return Err(self.invalid(at, "IN needs a list")),
        }
        let eq = self.cmp(CmpOp::Eq, a, &C::new(elem, Ty::Any))?.sql;
        let null_a = match &a.ty {
            Ty::Node(_) | Ty::Rel(_) => self.lit_false().to_owned(),
            _ => format!("{} IS NULL", a.sql),
        };
        Ok(C::new(
            format!("(CASE WHEN {null_a} THEN NULL WHEN {guard} THEN EXISTS (SELECT 1 FROM {from} WHERE {eq}) END)"),
            Ty::Bool,
        ))
    }

    fn str_op(&self, op: StringOp, a: &C, b: &C) -> C {
        let (Some(x), Some(y)) = (self.text(a), self.text(b)) else {
            return C::new("NULL", Ty::Bool);
        };
        let sql = match (op, self.f) {
            (StringOp::StartsWith, Flavor::Sqlite) => format!("substr({x}, 1, length({y})) = {y}"),
            (StringOp::EndsWith, Flavor::Sqlite) => format!("(length({y}) = 0 OR substr({x}, -length({y})) = {y})"),
            (StringOp::Contains, Flavor::Sqlite) => format!("instr({x}, {y}) > 0"),
            (StringOp::StartsWith, Flavor::Pg) => format!("starts_with({x}, {y})"),
            (StringOp::EndsWith, Flavor::Pg) => format!("right({x}, length({y})) = {y}"),
            (StringOp::Contains, Flavor::Pg) => format!("strpos({x}, {y}) > 0"),
        };
        C::new(
            format!("(CASE WHEN {x} IS NULL OR {y} IS NULL THEN NULL ELSE {sql} END)"),
            Ty::Bool,
        )
    }

    // ---- functions -------------------------------------------------------

    fn call(&mut self, name: &str, distinct: bool, args: &[Expr], star: bool, at: usize) -> Result<C> {
        let arity = |n: usize| -> Result<()> {
            if args.len() == n {
                Ok(())
            } else {
                Err(CypherError::invalid(
                    self.q,
                    at,
                    format_args!("{name}() takes {n} argument{}", if n == 1 { "" } else { "s" }),
                ))
            }
        };
        if forgebase_data::query::cypher::is_aggregate(name) {
            if self.in_agg {
                return Err(self.invalid(at, "aggregate functions cannot be nested"));
            }
            if star {
                return Ok(C::new("count(*)", Ty::Int));
            }
            arity(1)?;
            self.in_agg = true;
            let arg = self.expr(&args[0]);
            self.in_agg = false;
            return self.aggregate(name, distinct, &arg?, at);
        }
        if distinct {
            return Err(self.invalid(
                at,
                format_args!("DISTINCT is only for aggregate functions, not {name}()"),
            ));
        }
        let mut cs = Vec::with_capacity(args.len());
        for a in args {
            cs.push(self.expr(a)?);
        }
        Ok(match name {
            "type" => {
                arity(1)?;
                match &cs[0].ty {
                    Ty::Rel(e) => C::new(format!("{e}.type"), Ty::Str),
                    Ty::Null => C::new("NULL", Ty::Null),
                    _ => return Err(self.invalid(at, "type() needs a relationship")),
                }
            }
            "labels" => {
                arity(1)?;
                match &cs[0].ty {
                    Ty::Node(n) => C::new(
                        match self.f {
                            Flavor::Sqlite => format!("json_array({n}.label)"),
                            Flavor::Pg => format!("jsonb_build_array({n}.label)"),
                        },
                        Ty::List,
                    ),
                    Ty::Null => C::new("NULL", Ty::Null),
                    _ => return Err(self.invalid(at, "labels() needs a node")),
                }
            }
            "tolower" | "toupper" => {
                arity(1)?;
                let f = if name == "tolower" { "lower" } else { "upper" };
                match self.text(&cs[0]) {
                    Some(x) => C::new(format!("{f}({x})"), Ty::Str),
                    None => return Err(self.invalid(at, format_args!("{name}() needs a string"))),
                }
            }
            "size" => {
                arity(1)?;
                let x = cs[0].sql.clone();
                match (&cs[0].ty, self.f) {
                    (Ty::Str, _) => C::new(format!("length({x})"), Ty::Int),
                    (Ty::Null, _) => C::new("NULL", Ty::Null),
                    (Ty::List | Ty::Any, Flavor::Sqlite) => C::new(
                        format!(
                            "(CASE json_type({x}) WHEN 'array' THEN json_array_length({x}) \
                             WHEN 'text' THEN length({x} ->> '$') END)"
                        ),
                        Ty::Int,
                    ),
                    (Ty::List | Ty::Any, Flavor::Pg) => C::new(
                        format!(
                            "(CASE jsonb_typeof({x}) WHEN 'array' THEN jsonb_array_length({x}) \
                             WHEN 'string' THEN length({x} #>> '{{}}') END)"
                        ),
                        Ty::Int,
                    ),
                    (Ty::Walk(w, _), Flavor::Sqlite) => C::new(
                        format!("(length({w}.path) - length(replace({w}.path, ',', '')) - 1)"),
                        Ty::Int,
                    ),
                    (Ty::Walk(w, _), Flavor::Pg) => C::new(format!("cardinality({w}.path)"), Ty::Int),
                    _ => return Err(self.invalid(at, "size() needs a string or a list")),
                }
            }
            "coalesce" => {
                if cs.is_empty() {
                    return Err(self.invalid(at, "coalesce() needs arguments"));
                }
                let first = cs[0].ty.clone();
                let typed = matches!(first, Ty::Str | Ty::Bool | Ty::Int | Ty::Float | Ty::Num)
                    && cs.iter().all(|c| c.ty == first || c.ty == Ty::Null);
                if typed {
                    let parts: Vec<String> = cs.iter().map(|c| c.sql.clone()).collect();
                    C::new(format!("coalesce({})", parts.join(", ")), first)
                } else {
                    let parts: Vec<String> = cs.iter().map(|c| self.to_json(c)).collect();
                    let sql = format!("coalesce({})", parts.join(", "));
                    C::new(
                        match self.f {
                            Flavor::Sqlite => format!("json({sql})"),
                            Flavor::Pg => sql,
                        },
                        Ty::Any,
                    )
                }
            }
            "id" => return Err(self.unsupported(at, "id() (read the ForgeBase id as n.__id)")),
            other => return Err(self.unsupported(at, &format!("the function {other}()"))),
        })
    }

    fn aggregate(&mut self, name: &str, distinct: bool, arg: &C, at: usize) -> Result<C> {
        let dist = if distinct { "DISTINCT " } else { "" };
        // The value aggregated: its identity for nodes and edges, its SQL
        // otherwise (JSON for properties).
        let key = match &arg.ty {
            Ty::Node(a) | Ty::Rel(a) => format!("{a}.id"),
            Ty::Walk(w, _) => format!("{w}.path"),
            _ => arg.sql.clone(),
        };
        let nonnull = match &arg.ty {
            Ty::Node(_) | Ty::Rel(_) | Ty::Walk(..) => None,
            _ => Some(format!("{} IS NOT NULL", arg.sql)),
        };
        Ok(match name {
            "count" => C::new(format!("count({dist}{key})"), Ty::Int),
            "collect" => {
                let j = self.to_json(arg);
                let filter = nonnull.map(|c| format!(" FILTER (WHERE {c})")).unwrap_or_default();
                match self.f {
                    Flavor::Sqlite => C::new(format!("json_group_array({dist}{j}){filter}"), Ty::List),
                    Flavor::Pg => C::new(format!("coalesce(jsonb_agg({dist}{j}){filter}, '[]'::jsonb)"), Ty::List),
                }
            }
            "min" | "max" => match &arg.ty {
                Ty::Int | Ty::Float | Ty::Num | Ty::Str => C::new(format!("{name}({dist}{})", arg.sql), arg.ty.clone()),
                Ty::Bool => C::new(
                    match self.f {
                        Flavor::Sqlite => format!("{name}({dist}{})", arg.sql),
                        Flavor::Pg => format!("bool_{}({})", if name == "min" { "and" } else { "or" }, arg.sql),
                    },
                    Ty::Bool,
                ),
                Ty::Any => {
                    // Numbers when there are any, else strings.
                    let n = self.num(arg).unwrap_or_default();
                    let s = self.text(arg).unwrap_or_default();
                    let (jn, js) = match self.f {
                        Flavor::Sqlite => (format!("json_quote({name}({n}))"), format!("json_quote({name}({s}))")),
                        Flavor::Pg => (format!("to_jsonb({name}({n}))"), format!("to_jsonb({name}({s}))")),
                    };
                    let sql = format!("(CASE WHEN count({n}) > 0 THEN {jn} WHEN count({s}) > 0 THEN {js} END)");
                    C::new(
                        match self.f {
                            Flavor::Sqlite => format!("json({sql})"),
                            Flavor::Pg => sql,
                        },
                        Ty::Any,
                    )
                }
                Ty::Null => C::new("NULL", Ty::Null),
                _ => return Err(self.invalid(at, format_args!("{name}() needs numbers or strings"))),
            },
            "sum" | "avg" => {
                let Some(n) = self.num(arg) else {
                    return Err(self.invalid(at, format_args!("{name}() needs numbers")));
                };
                if name == "sum" {
                    C::new(format!("coalesce(sum({dist}{n}), 0)"), Ty::Num)
                } else {
                    C::new(
                        match self.f {
                            Flavor::Sqlite => format!("avg({dist}{n})"),
                            Flavor::Pg => format!("CAST(avg({dist}{n}) AS double precision)"),
                        },
                        Ty::Float,
                    )
                }
            }
            other => return Err(self.unsupported(at, &format!("the aggregate {other}()"))),
        })
    }
}

/// An edge row as JSON in ForgeBase's shape.
fn edge_json(f: Flavor, a: &str) -> String {
    match f {
        Flavor::Sqlite => format!(
            "json_object('id', {a}.id, 'type', {a}.type, 'source', {a}.source, 'target', {a}.target, \
             'directed', json(CASE WHEN {a}.directed = 1 THEN 'true' ELSE 'false' END), \
             'properties', json({a}.properties))"
        ),
        Flavor::Pg => format!(
            "jsonb_build_object('id', {a}.id, 'type', {a}.type, 'source', {a}.source, 'target', {a}.target, \
             'directed', {a}.directed = 1, 'properties', {a}.properties)"
        ),
    }
}

/// The top-level `AND` conjuncts of `e`.
fn conjuncts(e: &Expr) -> Vec<&Expr> {
    match &e.kind {
        ExprKind::And(a, b) => {
            let mut v = conjuncts(a);
            v.extend(conjuncts(b));
            v
        }
        _ => vec![e],
    }
}

/// Whether `e` refers to variable `var` and no other.
fn refs_only(e: &Expr, var: &str) -> bool {
    let (mut mine, mut other) = (false, false);
    e.walk(&mut |x| {
        if let ExprKind::Var(v) = &x.kind {
            if v == var {
                mine = true;
            } else {
                other = true;
            }
        }
    });
    mine && !other
}

/// Structural equality of expressions (positions ignored).
fn same_expr(a: &Expr, b: &Expr) -> bool {
    use ExprKind as K;
    match (&a.kind, &b.kind) {
        (K::Lit(x), K::Lit(y)) => x == y,
        (K::Param(x), K::Param(y)) | (K::Var(x), K::Var(y)) => x == y,
        (K::Prop(x, k), K::Prop(y, l)) => k == l && same_expr(x, y),
        (K::List(x), K::List(y)) => x.len() == y.len() && x.iter().zip(y).all(|(a, b)| same_expr(a, b)),
        (K::Not(x), K::Not(y)) | (K::Neg(x), K::Neg(y)) => same_expr(x, y),
        (K::And(a1, b1), K::And(a2, b2)) | (K::Or(a1, b1), K::Or(a2, b2)) | (K::In(a1, b1), K::In(a2, b2)) => {
            same_expr(a1, a2) && same_expr(b1, b2)
        }
        (K::Cmp(o1, a1, b1), K::Cmp(o2, a2, b2)) => o1 == o2 && same_expr(a1, a2) && same_expr(b1, b2),
        (K::Str(o1, a1, b1), K::Str(o2, a2, b2)) => o1 == o2 && same_expr(a1, a2) && same_expr(b1, b2),
        (K::IsNull { expr: x, negated: n1 }, K::IsNull { expr: y, negated: n2 }) => n1 == n2 && same_expr(x, y),
        (
            K::Call {
                name: n1,
                distinct: d1,
                args: a1,
                star: s1,
            },
            K::Call {
                name: n2,
                distinct: d2,
                args: a2,
                star: s2,
            },
        ) => {
            n1 == n2 && d1 == d2 && s1 == s2 && a1.len() == a2.len() && a1.iter().zip(a2).all(|(x, y)| same_expr(x, y))
        }
        _ => false,
    }
}
