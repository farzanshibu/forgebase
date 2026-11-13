//! The syntax tree of the portable openCypher subset.

/// A byte range of the query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    /// First byte.
    pub start: usize,
    /// One past the last byte.
    pub end: usize,
}

impl Span {
    /// The range `start..end`.
    pub fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    /// From the start of `self` to the end of `other`.
    pub fn to(self, other: Span) -> Span {
        Span::new(self.start, other.end.max(self.start))
    }
}

/// One query.
#[derive(Debug, Clone, PartialEq)]
pub struct Query {
    /// The `MATCH` clauses, in order (none for a bare `RETURN`).
    pub matches: Vec<MatchClause>,
    /// The final `RETURN`.
    pub ret: Return,
}

/// `MATCH pattern, … [WHERE expr]`.
#[derive(Debug, Clone, PartialEq)]
pub struct MatchClause {
    /// Comma-separated patterns.
    pub patterns: Vec<Pattern>,
    /// The `WHERE` condition.
    pub filter: Option<Expr>,
    /// The `MATCH` keyword.
    pub span: Span,
}

/// A chain `(a)-[r]->(b)<-[s]-(c)`.
#[derive(Debug, Clone, PartialEq)]
pub struct Pattern {
    /// The first node.
    pub start: NodePattern,
    /// Each relationship and the node after it.
    pub steps: Vec<(RelPattern, NodePattern)>,
}

/// `(v:Label {key: expr})`.
#[derive(Debug, Clone, PartialEq)]
pub struct NodePattern {
    /// The variable, if named.
    pub var: Option<String>,
    /// Labels (`:A:B`: the node must have both).
    pub labels: Vec<String>,
    /// The property map.
    pub props: Vec<(String, Expr)>,
    /// From `(` to `)`.
    pub span: Span,
}

/// Which way a relationship pattern points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// `-[]->`: from the left node to the right one.
    Right,
    /// `<-[]-`: from the right node to the left one.
    Left,
    /// `-[]-`: either way.
    Both,
}

/// `-[r:TYPE|OTHER *min..max {key: expr}]->`.
#[derive(Debug, Clone, PartialEq)]
pub struct RelPattern {
    /// The variable, if named.
    pub var: Option<String>,
    /// Allowed types (any when empty).
    pub types: Vec<String>,
    /// Direction.
    pub direction: Direction,
    /// `Some((min, max))` for a variable-length relationship.
    pub length: Option<(u32, u32)>,
    /// The property map (each relationship of a variable-length one must
    /// match it).
    pub props: Vec<(String, Expr)>,
    /// From the first to the last character of the relationship.
    pub span: Span,
}

/// `RETURN …`.
#[derive(Debug, Clone, PartialEq)]
pub struct Return {
    /// `RETURN DISTINCT`.
    pub distinct: bool,
    /// The items.
    pub items: Vec<ReturnItem>,
    /// `ORDER BY`.
    pub order: Vec<SortItem>,
    /// `SKIP`.
    pub skip: Option<Expr>,
    /// `LIMIT`.
    pub limit: Option<Expr>,
    /// The `RETURN` keyword.
    pub span: Span,
}

/// `expr [AS name]`.
#[derive(Debug, Clone, PartialEq)]
pub struct ReturnItem {
    /// The expression.
    pub expr: Expr,
    /// The alias.
    pub alias: Option<String>,
    /// The column name: the alias, else the expression's text with its
    /// whitespace collapsed (as the Apache AGE path names columns).
    pub name: String,
}

/// `expr [ASC|DESC]`.
#[derive(Debug, Clone, PartialEq)]
pub struct SortItem {
    /// The key.
    pub expr: Expr,
    /// Descending.
    pub desc: bool,
}

/// A literal value.
#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    /// `null`.
    Null,
    /// `true` / `false`.
    Bool(bool),
    /// An integer.
    Int(i64),
    /// A float.
    Float(f64),
    /// A string.
    Str(String),
}

/// Comparison operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    /// `=`.
    Eq,
    /// `<>`.
    Ne,
    /// `<`.
    Lt,
    /// `<=`.
    Le,
    /// `>`.
    Gt,
    /// `>=`.
    Ge,
}

impl CmpOp {
    /// The SQL operator.
    pub fn sql(self) -> &'static str {
        match self {
            CmpOp::Eq => "=",
            CmpOp::Ne => "<>",
            CmpOp::Lt => "<",
            CmpOp::Le => "<=",
            CmpOp::Gt => ">",
            CmpOp::Ge => ">=",
        }
    }
}

/// String predicates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StringOp {
    /// `STARTS WITH`.
    StartsWith,
    /// `ENDS WITH`.
    EndsWith,
    /// `CONTAINS`.
    Contains,
}

/// An expression with its position.
#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    /// What it is.
    pub kind: ExprKind,
    /// Where it is.
    pub span: Span,
}

/// Expression kinds.
#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    /// A literal.
    Lit(Literal),
    /// `$name`.
    Param(String),
    /// A variable.
    Var(String),
    /// `expr.key`.
    Prop(Box<Expr>, String),
    /// `[a, b, …]`.
    List(Vec<Expr>),
    /// `NOT expr`.
    Not(Box<Expr>),
    /// `a AND b`.
    And(Box<Expr>, Box<Expr>),
    /// `a OR b`.
    Or(Box<Expr>, Box<Expr>),
    /// `a <op> b`.
    Cmp(CmpOp, Box<Expr>, Box<Expr>),
    /// `expr IS NULL` (`negated`: `IS NOT NULL`).
    IsNull {
        /// The operand.
        expr: Box<Expr>,
        /// `IS NOT NULL`.
        negated: bool,
    },
    /// `a IN b`.
    In(Box<Expr>, Box<Expr>),
    /// `a STARTS WITH b`, `ENDS WITH`, `CONTAINS`.
    Str(StringOp, Box<Expr>, Box<Expr>),
    /// `-expr`.
    Neg(Box<Expr>),
    /// `name([DISTINCT] args)`; `count(*)` has `star`.
    Call {
        /// The function name, lowercased.
        name: String,
        /// `DISTINCT`.
        distinct: bool,
        /// Arguments.
        args: Vec<Expr>,
        /// `count(*)`.
        star: bool,
    },
}

impl Expr {
    /// Whether the expression contains an aggregate call (`count`,
    /// `collect`, `min`, `max`, `sum`, `avg`).
    pub fn has_aggregate(&self) -> bool {
        let mut found = false;
        self.walk(&mut |e| {
            if let ExprKind::Call { name, .. } = &e.kind
                && is_aggregate(name)
            {
                found = true;
            }
        });
        found
    }

    /// Calls `f` on this expression and every sub-expression.
    pub fn walk(&self, f: &mut dyn FnMut(&Expr)) {
        f(self);
        match &self.kind {
            ExprKind::Lit(_) | ExprKind::Param(_) | ExprKind::Var(_) => {}
            ExprKind::Prop(e, _) | ExprKind::Not(e) | ExprKind::Neg(e) | ExprKind::IsNull { expr: e, .. } => {
                e.walk(f);
            }
            ExprKind::List(items) | ExprKind::Call { args: items, .. } => {
                for e in items {
                    e.walk(f);
                }
            }
            ExprKind::And(a, b)
            | ExprKind::Or(a, b)
            | ExprKind::Cmp(_, a, b)
            | ExprKind::In(a, b)
            | ExprKind::Str(_, a, b) => {
                a.walk(f);
                b.walk(f);
            }
        }
    }
}

/// Whether `name` (lowercase) is an aggregate function.
pub fn is_aggregate(name: &str) -> bool {
    matches!(name, "count" | "collect" | "min" | "max" | "sum" | "avg")
}
