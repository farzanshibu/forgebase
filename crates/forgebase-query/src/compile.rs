//! Compiles filter expressions to parameterised SQL (ADR-0003 §5).
//!
//! Safety rules, enforced here and tested in `tests`:
//!
//! * Values never reach SQL text: literals, `{:params}` and `@now` become
//!   bound parameters ([`SqlBuilder::bind`]).
//! * Identifiers never come from input: a path is resolved against the
//!   collection schema and the *schema's* column name is quoted by the
//!   dialect. Unknown names are errors.
//! * JSON path segments are validated identifiers or integers and are
//!   rendered as SQL literals so JSON-path expression indexes match.
//! * `~` escapes `%`, `_` and `\` in the value, so it is a literal substring
//!   match.
//!
//! The only exception to the first rule is [`CompileOptions::inline_literals`],
//! used for index definitions (SQLite cannot bind parameters in `CREATE INDEX`):
//! numbers are formatted by Rust and strings are quoted by the dialect.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use forgebase_db::{Dialect, JsonPathSeg, QueryLabel, Value};
use forgebase_schema::{Collection, Field, FieldKind, SchemaMode, Storage};
use serde_json::Value as Json;

use crate::QueryError;
use crate::filter::{Compare, Expr, FieldPath, Literal, Modifier, Op, Operand, PathSeg, SortKey};

/// Accumulates bound parameters and unique aliases for one statement.
#[derive(Debug)]
pub struct SqlBuilder<'d> {
    dialect: &'d dyn Dialect,
    params: Vec<Value>,
    aliases: u32,
    lookups: bool,
}

impl<'d> SqlBuilder<'d> {
    /// A builder for one statement.
    pub fn new(dialect: &'d dyn Dialect) -> Self {
        Self {
            dialect,
            params: Vec::new(),
            aliases: 0,
            lookups: false,
        }
    }

    /// Whether the statement reads another collection's table (a relation
    /// path in a filter, sort key or rule). Native row-level security would
    /// apply that table's policies to the lookup, so such reads stay on the
    /// server's session (FB-152).
    pub fn follows_relations(&self) -> bool {
        self.lookups
    }

    /// The dialect.
    pub fn dialect(&self) -> &'d dyn Dialect {
        self.dialect
    }

    /// Binds a value and returns its placeholder.
    pub fn bind(&mut self, v: impl Into<Value>) -> String {
        self.params.push(v.into());
        self.dialect.placeholder(self.params.len())
    }

    /// A fresh alias (`j1`, `r2`, ...).
    pub fn alias(&mut self, prefix: &str) -> String {
        self.aliases += 1;
        format!("{prefix}{}", self.aliases)
    }

    /// Quotes a schema identifier.
    pub fn ident(&self, name: &str) -> String {
        self.dialect.quote_ident(name)
    }

    /// Parameters bound so far.
    pub fn params(&self) -> &[Value] {
        &self.params
    }

    /// Consumes the builder.
    pub fn into_params(self) -> Vec<Value> {
        self.params
    }
}

/// Looks up collections by id (relation targets).
pub trait Catalog: Sync {
    /// Collection with this id.
    fn collection_by_id(&self, id: &str) -> Option<&Collection>;
}

impl Catalog for HashMap<String, Collection> {
    fn collection_by_id(&self, id: &str) -> Option<&Collection> {
        self.get(id)
    }
}

/// Field-level read rules (FB-21): decides whether the caller may reference a
/// field in a filter, sort key, group-by key, aggregate or relation path.
/// A field it refuses is reported exactly like an unknown field, so its
/// existence does not leak.
pub trait FieldGuard: Send + Sync + fmt::Debug {
    /// Whether `field` of `collection` may be referenced.
    fn readable(&self, collection: &Collection, field: &Field) -> bool;
}

/// Compilation options.
#[derive(Debug, Clone)]
pub struct CompileOptions {
    /// Hidden fields may be referenced (administrators, system).
    pub allow_hidden: bool,
    /// Field-level read rules of the caller, checked on every field a path
    /// touches, relation targets included. `None`: no rule beyond `hidden`.
    pub field_guard: Option<Arc<dyn FieldGuard>>,
    /// Maximum relation hops in one path.
    pub max_relation_depth: usize,
    /// Render literals into SQL text (index definitions only). Parameters,
    /// `@now`, relations and multi-value fields are rejected in this mode.
    pub inline_literals: bool,
    /// Values for `{:name}` parameters.
    pub params: HashMap<String, Json>,
    /// Value of `@now`.
    pub now: String,
    /// Native render mode (FB-152): request references and `@now` render
    /// through this, literals are inlined and relation paths read through
    /// `SECURITY DEFINER` functions ([`crate::native`]).
    pub native: Option<Arc<dyn crate::native::NativeRender>>,
}

impl Default for CompileOptions {
    fn default() -> Self {
        Self {
            allow_hidden: false,
            field_guard: None,
            max_relation_depth: 3,
            inline_literals: false,
            params: HashMap::new(),
            now: forgebase_schema::now_timestamp(),
            native: None,
        }
    }
}

/// SQL type of a compiled expression. SQLite is dynamically typed, so only
/// the PostgreSQL rendering looks at it: JSON values are `jsonb` there and
/// compare with `jsonb` constants; text, numbers and booleans compare with
/// parameters the server types from the column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalarType {
    /// Text (`text`).
    Text,
    /// 64-bit integer (`bigint`).
    Integer,
    /// Float (`double precision`).
    Real,
    /// Boolean stored as 0/1 (`smallint`).
    Bool,
    /// A JSON value (`jsonb` on PostgreSQL; JSON NULL reads as SQL NULL).
    Json,
}

impl ScalarType {
    fn of_storage(s: Storage) -> Self {
        match s {
            Storage::Text => ScalarType::Text,
            Storage::Integer => ScalarType::Integer,
            Storage::Real => ScalarType::Real,
            Storage::Bool => ScalarType::Bool,
            Storage::Json => ScalarType::Json,
        }
    }
}

/// A resolved operand.
#[derive(Debug, Clone)]
enum Res {
    /// One value per row. `json`: the value is a JSON document or array
    /// (`:length` counts its elements).
    Scalar { sql: String, json: bool, ty: ScalarType },
    /// Several values per row: `SELECT .. FROM <from>` yields them as `<value>`.
    Multi {
        from: Vec<String>,
        value: String,
        /// The array expression, when the value is the bare element.
        array: Option<String>,
        ty: ScalarType,
    },
}

impl Res {
    fn ty(&self) -> ScalarType {
        match self {
            Res::Scalar { ty, .. } | Res::Multi { ty, .. } => *ty,
        }
    }
}

/// Whether the dialect is PostgreSQL's (typed comparisons, `jsonb`).
fn is_pg(d: &dyn Dialect) -> bool {
    d.name() == "postgres"
}

/// `path` inside the JSON value `base`. SQLite: `json_extract(base, '$.a.b')`.
/// PostgreSQL: `nullif((base)::jsonb->'a'->'b', 'null'::jsonb)`, a `jsonb`
/// value (typed, so numbers compare and sort as numbers; JSON `null` and a
/// missing key are both SQL NULL, as `json_extract` returns them). The cast is
/// a no-op on `jsonb` columns and accepts JSON held in `text` (`_data`).
fn json_get(d: &dyn Dialect, base: &str, path: &[JsonPathSeg]) -> String {
    if !is_pg(d) {
        return d.json_extract(base, path);
    }
    let mut out = format!("({base})::jsonb");
    for seg in path {
        match seg {
            JsonPathSeg::Key(k) => {
                // Defence in depth: keys are validated identifiers.
                assert!(
                    forgebase_db::dialect::is_safe_ident(k),
                    "json path keys are validated identifiers"
                );
                out.push_str(&format!("->'{k}'"));
            }
            JsonPathSeg::Index(i) => out.push_str(&format!("->{i}")),
        }
    }
    format!("nullif({out}, 'null'::jsonb)")
}

/// `sql` as text (PostgreSQL; SQLite converts implicitly).
fn as_text(d: &dyn Dialect, sql: &str, ty: ScalarType) -> String {
    if !is_pg(d) {
        return sql.to_owned();
    }
    match ty {
        ScalarType::Text => sql.to_owned(),
        ScalarType::Json => format!("({sql} #>> '{{}}')"),
        _ => format!("CAST({sql} AS text)"),
    }
}

/// Makes two path expressions comparable on PostgreSQL: a JSON value against
/// anything else compares as `jsonb`, text against a number as text.
fn coerce_pair(d: &dyn Dialect, a: (String, ScalarType), z: (String, ScalarType)) -> (String, String) {
    let ((a, ta), (z, tz)) = (a, z);
    if !is_pg(d) || ta == tz {
        return (a, z);
    }
    match (ta, tz) {
        (ScalarType::Json, _) => (a, format!("to_jsonb({z})")),
        (_, ScalarType::Json) => (format!("to_jsonb({a})"), z),
        (ScalarType::Text, _) => (a, format!("CAST({z} AS text)")),
        (_, ScalarType::Text) => (format!("CAST({a} AS text)"), z),
        _ => (a, z),
    }
}

/// How a row is reached: directly through an alias, or by id lookup in a
/// related table (a correlated scalar subquery).
#[derive(Debug, Clone)]
pub(crate) enum RowRef {
    Alias(String),
    Lookup {
        table: String,
        alias: String,
        id_expr: String,
    },
}

impl RowRef {
    fn col(&self, d: &dyn Dialect, name: &str) -> String {
        match self {
            RowRef::Alias(a) if a.is_empty() => d.quote_ident(name),
            RowRef::Alias(a) => format!("{a}.{}", d.quote_ident(name)),
            RowRef::Lookup { table, alias, id_expr } => format!(
                "(SELECT {alias}.{} FROM {table} AS {alias} WHERE {alias}.{} = {id_expr})",
                d.quote_ident(name),
                d.quote_ident("id")
            ),
        }
    }
}

/// Compiles filters, sort keys and field paths for one root collection.
pub struct Compiler<'a> {
    catalog: &'a dyn Catalog,
    opts: &'a CompileOptions,
}

fn json_path(segs: &[PathSeg]) -> Result<Vec<JsonPathSeg>, QueryError> {
    segs.iter()
        .map(|s| match s {
            PathSeg::Name(n) => JsonPathSeg::key(n).ok_or_else(|| QueryError::UnknownField(n.clone())),
            PathSeg::Index(i) => Ok(JsonPathSeg::Index(*i)),
        })
        .collect()
}

impl<'a> Compiler<'a> {
    /// A compiler.
    pub fn new(catalog: &'a dyn Catalog, opts: &'a CompileOptions) -> Self {
        Self { catalog, opts }
    }

    pub(crate) fn catalog(&self) -> &'a dyn Catalog {
        self.catalog
    }

    pub(crate) fn opts(&self) -> &'a CompileOptions {
        self.opts
    }

    /// Compiles `expr` against `root`, whose row is available as `alias`.
    /// Returns a boolean SQL expression; parameters go to `b`.
    pub fn filter(
        &self,
        b: &mut SqlBuilder<'_>,
        root: &Collection,
        alias: &str,
        expr: &Expr,
    ) -> Result<String, QueryError> {
        let row = RowRef::Alias(alias.to_owned());
        self.expr(b, root, &row, expr)
    }

    /// Compiles a path that must yield one value per row (sort keys, index
    /// keys, group-by keys, aggregate arguments).
    pub fn scalar(
        &self,
        b: &mut SqlBuilder<'_>,
        root: &Collection,
        alias: &str,
        path: &FieldPath,
    ) -> Result<String, QueryError> {
        self.scalar_typed(b, root, alias, path).map(|(sql, _)| sql)
    }

    /// [`Compiler::scalar`] with the expression's type. On PostgreSQL a
    /// [`ScalarType::Json`] expression is `jsonb`: it sorts by JSON value
    /// and comes back as JSON text.
    pub fn scalar_typed(
        &self,
        b: &mut SqlBuilder<'_>,
        root: &Collection,
        alias: &str,
        path: &FieldPath,
    ) -> Result<(String, ScalarType), QueryError> {
        let row = RowRef::Alias(alias.to_owned());
        match self.path(b, root, &row, path, 0)? {
            Res::Scalar { sql, ty, .. } => Ok((sql, ty)),
            Res::Multi { .. } => Err(QueryError::InvalidOperation(format!(
                "{path} holds several values; use {path}:length or a single-value field"
            ))),
        }
    }

    /// `ORDER BY` items for sort keys (without the keyword). NULLs sort
    /// first ascending and last descending on every engine
    /// ([`Dialect::order_nulls`]), which keyset cursors rely on.
    pub fn order_by(
        &self,
        b: &mut SqlBuilder<'_>,
        root: &Collection,
        alias: &str,
        keys: &[SortKey],
    ) -> Result<Vec<String>, QueryError> {
        keys.iter()
            .map(|k| {
                let e = self.scalar(b, root, alias, &k.path)?;
                Ok(format!("{e} {}", b.dialect().order_nulls(k.desc)))
            })
            .collect()
    }

    fn expr(&self, b: &mut SqlBuilder<'_>, root: &Collection, row: &RowRef, e: &Expr) -> Result<String, QueryError> {
        Ok(match e {
            Expr::Const(true) => "1 = 1".to_owned(),
            Expr::Const(false) => "1 = 0".to_owned(),
            Expr::Not(inner) => format!("NOT ({})", self.expr(b, root, row, inner)?),
            Expr::And(items) | Expr::Or(items) => {
                let sep = if matches!(e, Expr::And(_)) { " AND " } else { " OR " };
                let parts = items
                    .iter()
                    .map(|i| self.expr(b, root, row, i).map(|s| format!("({s})")))
                    .collect::<Result<Vec<_>, _>>()?;
                if parts.is_empty() {
                    return Ok(if matches!(e, Expr::And(_)) { "1 = 1" } else { "1 = 0" }.to_owned());
                }
                parts.join(sep)
            }
            Expr::Compare(c) => self.compare(b, root, row, c)?,
        })
    }

    /// The value of a non-path operand (not yet bound).
    fn constant(&self, o: &Operand) -> Result<Value, QueryError> {
        Ok(match self.constant_json(o)? {
            Json::Null => Value::Null,
            Json::Bool(v) => Value::from(v),
            Json::Number(n) => n
                .as_i64()
                .map(Value::Integer)
                .or_else(|| n.as_f64().map(Value::Real))
                .unwrap_or(Value::Null),
            Json::String(s) => Value::Text(s),
            _ => return Err(QueryError::InvalidOperation("expected a value".into())),
        })
    }

    /// The value of a non-path operand as JSON (literals keep their kind:
    /// `true` stays a boolean, for comparisons with JSON values).
    fn constant_json(&self, o: &Operand) -> Result<Json, QueryError> {
        Ok(match o {
            Operand::Literal(l) => match l {
                Literal::Null => Json::Null,
                Literal::Bool(v) => Json::Bool(*v),
                Literal::Int(i) => Json::from(*i),
                Literal::Float(f) => serde_json::Number::from_f64(*f)
                    .map(Json::Number)
                    .ok_or_else(|| QueryError::InvalidOperation("numbers must be finite".into()))?,
                Literal::Text(s) => Json::String(s.clone()),
            },
            Operand::Param(name) => {
                if self.opts.inline_literals {
                    return Err(QueryError::InvalidOperation("parameters are not allowed here".into()));
                }
                // Native mode: a request reference is rendered by
                // `render_against`; here it only must not look like NULL.
                if self.opts.native.as_ref().is_some_and(|n| n.request(name).is_some()) {
                    return Ok(Json::String(String::new()));
                }
                match self.opts.params.get(name) {
                    None => return Err(QueryError::MissingParam(name.clone())),
                    Some(v @ (Json::Null | Json::Bool(_) | Json::Number(_) | Json::String(_))) => v.clone(),
                    Some(_) => {
                        return Err(QueryError::InvalidOperation(format!(
                            "parameter {name} must be a string, number, boolean or null"
                        )));
                    }
                }
            }
            Operand::Now => {
                if self.opts.inline_literals {
                    return Err(QueryError::InvalidOperation(
                        "@now is not allowed in index definitions".into(),
                    ));
                }
                Json::String(self.opts.now.clone())
            }
            Operand::Path(_) => return Err(QueryError::InvalidOperation("expected a value".into())),
        })
    }

    fn render(&self, b: &mut SqlBuilder<'_>, v: &Value) -> Result<String, QueryError> {
        if !self.opts.inline_literals && self.opts.native.is_none() {
            return Ok(b.bind(v.clone()));
        }
        Ok(match v {
            Value::Null => "NULL".to_owned(),
            Value::Integer(i) => i.to_string(),
            Value::Real(f) => format!("{f:?}"),
            Value::Text(s) => {
                if s.contains('\0') {
                    return Err(QueryError::InvalidOperation("strings cannot contain NUL".into()));
                }
                b.dialect().string_literal(s)
            }
            Value::Blob(_) => return Err(QueryError::InvalidOperation("blobs are not allowed here".into())),
        })
    }

    /// A constant compared with an expression of type `ty`. PostgreSQL:
    /// against a JSON value the constant is a `jsonb` value
    /// (`CAST(? AS jsonb)` with the constant's JSON text bound); a fraction
    /// against an integer column is cast to `double precision`; anything else
    /// is a parameter the server types from the other side.
    fn render_against(&self, b: &mut SqlBuilder<'_>, o: &Operand, ty: ScalarType) -> Result<String, QueryError> {
        if let Some(sql) = self
            .opts
            .native
            .as_ref()
            .and_then(|n| crate::native::render_against(n.as_ref(), o, ty))
        {
            return Ok(sql);
        }
        let d = b.dialect();
        if is_pg(d) && ty == ScalarType::Json {
            let text = self.constant_json(o)?.to_string();
            return Ok(if self.opts.inline_literals || self.opts.native.is_some() {
                format!("{}::jsonb", self.render(b, &Value::Text(text))?)
            } else {
                format!("CAST({} AS jsonb)", b.bind(text))
            });
        }
        let v = self.constant(o)?;
        let sql = self.render(b, &v)?;
        Ok(match (&v, ty) {
            (Value::Real(_), ScalarType::Integer | ScalarType::Bool) if is_pg(d) => {
                format!("CAST({sql} AS double precision)")
            }
            _ => sql,
        })
    }

    fn operand(&self, b: &mut SqlBuilder<'_>, root: &Collection, row: &RowRef, o: &Operand) -> Result<Res, QueryError> {
        match o {
            Operand::Path(p) => self.path(b, root, row, p, 0),
            other => {
                let v = self.constant(other)?;
                let ty = match v {
                    Value::Integer(_) => ScalarType::Integer,
                    Value::Real(_) => ScalarType::Real,
                    _ => ScalarType::Text,
                };
                Ok(Res::Scalar {
                    sql: self.render(b, &v)?,
                    json: false,
                    ty,
                })
            }
        }
    }

    fn compare(
        &self,
        b: &mut SqlBuilder<'_>,
        root: &Collection,
        row: &RowRef,
        c: &Compare,
    ) -> Result<String, QueryError> {
        match &self.opts.native {
            Some(n) => self.native_compare(n, b, root, row, c),
            None => self.compare_plain(b, root, row, c),
        }
    }

    /// The comparison without native dispatch.
    pub(crate) fn compare_plain(
        &self,
        b: &mut SqlBuilder<'_>,
        root: &Collection,
        row: &RowRef,
        c: &Compare,
    ) -> Result<String, QueryError> {
        let d = b.dialect();
        // Normalise so a path is on the left when only one side is a path.
        let (left, op, right) = match (&c.left, &c.right) {
            (l, Operand::Path(_)) if !matches!(l, Operand::Path(_)) => {
                if matches!(c.op, Op::Like | Op::NotLike) {
                    return Err(QueryError::InvalidOperation("put the field on the left of ~".into()));
                }
                (&c.right, c.op.flipped(), &c.left)
            }
            _ => (&c.left, c.op, &c.right),
        };
        let l = self.operand(b, root, row, left)?;

        if matches!(op, Op::Like | Op::NotLike)
            && let Some(pattern) = self
                .opts
                .native
                .as_ref()
                .and_then(|n| crate::native::like_pattern(n.as_ref(), right))
        {
            let ty = l.ty();
            let test = |v: &str| {
                let v = as_text(d, v, ty);
                match op {
                    Op::Like => d.like_ci(&v, &pattern),
                    _ => format!("NOT ({})", d.like_ci(&format!("coalesce({v}, '')"), &pattern)),
                }
            };
            return Ok(match l {
                Res::Scalar { sql, .. } => test(&sql),
                Res::Multi { from, value, .. } => self.quantified(c.any, &from, &test(&value)),
            });
        }
        if matches!(op, Op::Like | Op::NotLike) {
            let text = match right {
                Operand::Literal(Literal::Text(s)) => s.clone(),
                Operand::Param(name) => match self.opts.params.get(name) {
                    Some(Json::String(s)) => s.clone(),
                    Some(_) => {
                        return Err(QueryError::InvalidOperation(format!(
                            "parameter {name} must be a string for ~"
                        )));
                    }
                    None => return Err(QueryError::MissingParam(name.clone())),
                },
                _ => {
                    return Err(QueryError::InvalidOperation(
                        "~ needs a string value on the right".into(),
                    ));
                }
            };
            let pattern = self.render(b, &Value::Text(format!("%{}%", d.escape_like(&text))))?;
            let ty = l.ty();
            // Case-insensitive on every engine: `LIKE` (ASCII) on SQLite,
            // `ILIKE` (Unicode) on PostgreSQL.
            let test = |v: &str| {
                let v = as_text(d, v, ty);
                match op {
                    Op::Like => d.like_ci(&v, &pattern),
                    _ => format!("NOT ({})", d.like_ci(&format!("coalesce({v}, '')"), &pattern)),
                }
            };
            return Ok(match l {
                Res::Scalar { sql, .. } => test(&sql),
                Res::Multi { from, value, .. } => self.quantified(c.any, &from, &test(&value)),
            });
        }

        let right_is_null = !matches!(right, Operand::Path(_)) && self.constant(right)?.is_null();
        if right_is_null {
            let sql = match op {
                Op::Eq => "IS NULL",
                Op::Ne => "IS NOT NULL",
                _ => {
                    return Err(QueryError::InvalidOperation(
                        "null can only be compared with = or !=".into(),
                    ));
                }
            };
            return Ok(match l {
                Res::Scalar { sql: s, .. } => format!("{s} {sql}"),
                Res::Multi { from, value, .. } => self.quantified(c.any, &from, &format!("{value} {sql}")),
            });
        }
        // Against a non-null constant, `=` and `IS` agree; `=` is what every
        // planner matches to an index. Path-to-path equality stays NULL-safe.
        let constant = !matches!(right, Operand::Path(_));
        let r = if constant {
            Res::Scalar {
                sql: self.render_against(b, right, l.ty())?,
                json: false,
                ty: l.ty(),
            }
        } else {
            self.operand(b, root, row, right)?
        };
        let sym = match op {
            Op::Eq if constant => "=",
            Op::Eq => d.null_safe_eq(),
            Op::Ne => d.null_safe_ne(),
            Op::Gt => ">",
            Op::Ge => ">=",
            Op::Lt => "<",
            Op::Le => "<=",
            Op::Like | Op::NotLike => unreachable!("handled above"),
        };
        match (l, r) {
            (Res::Scalar { sql: a, ty: ta, .. }, Res::Scalar { sql: z, ty: tz, .. }) => {
                let (a, z) = coerce_pair(d, (a, ta), (z, tz));
                Ok(format!("{a} {sym} {z}"))
            }
            (
                Res::Multi {
                    from, value, ty: ta, ..
                },
                Res::Scalar { sql: z, ty: tz, .. },
            ) => {
                let (value, z) = coerce_pair(d, (value, ta), (z, tz));
                Ok(self.quantified(c.any, &from, &format!("{value} {sym} {z}")))
            }
            (
                Res::Scalar { sql: a, ty: ta, .. },
                Res::Multi {
                    from, value, ty: tz, ..
                },
            ) => {
                let flipped = match op.flipped() {
                    Op::Gt => ">",
                    Op::Ge => ">=",
                    Op::Lt => "<",
                    Op::Le => "<=",
                    _ => sym,
                };
                let (value, a) = coerce_pair(d, (value, tz), (a, ta));
                Ok(self.quantified(c.any, &from, &format!("{value} {flipped} {a}")))
            }
            (Res::Multi { .. }, Res::Multi { .. }) => Err(QueryError::InvalidOperation(
                "cannot compare two multi-value fields".into(),
            )),
        }
    }

    /// `any`: some element satisfies `cond`; otherwise every element does.
    fn quantified(&self, any: bool, from: &[String], cond: &str) -> String {
        let from = from.join(", ");
        if any {
            format!("EXISTS (SELECT 1 FROM {from} WHERE {cond})")
        } else {
            format!("NOT EXISTS (SELECT 1 FROM {from} WHERE ({cond}) IS NOT TRUE)")
        }
    }

    fn path(
        &self,
        b: &mut SqlBuilder<'_>,
        coll: &Collection,
        row: &RowRef,
        p: &FieldPath,
        depth: usize,
    ) -> Result<Res, QueryError> {
        let res = self.segments(b, coll, row, &p.segments, depth)?;
        let d = b.dialect();
        Ok(match p.modifier {
            None => res,
            Some(Modifier::Length) => match res {
                Res::Multi { array: Some(a), .. } => Res::Scalar {
                    sql: d.json_array_length(&a),
                    json: false,
                    ty: ScalarType::Integer,
                },
                Res::Multi { .. } => {
                    return Err(QueryError::InvalidOperation(format!("{p}: :length needs an array")));
                }
                Res::Scalar { sql, json: true, .. } => Res::Scalar {
                    sql: d.json_array_length(&sql),
                    json: false,
                    ty: ScalarType::Integer,
                },
                Res::Scalar { sql, json: false, ty } => Res::Scalar {
                    sql: format!("length({})", as_text(d, &sql, ty)),
                    json: false,
                    ty: ScalarType::Integer,
                },
            },
            Some(Modifier::Lower) => match res {
                Res::Scalar { sql, ty, .. } => Res::Scalar {
                    sql: format!("lower({})", as_text(d, &sql, ty)),
                    json: false,
                    ty: ScalarType::Text,
                },
                Res::Multi { from, value, ty, .. } => Res::Multi {
                    from,
                    value: format!("lower({})", as_text(d, &value, ty)),
                    array: None,
                    ty: ScalarType::Text,
                },
            },
        })
    }

    fn segments(
        &self,
        b: &mut SqlBuilder<'_>,
        coll: &Collection,
        row: &RowRef,
        segs: &[PathSeg],
        depth: usize,
    ) -> Result<Res, QueryError> {
        let d = b.dialect();
        let Some(PathSeg::Name(name)) = segs.first() else {
            return Err(QueryError::UnknownField(String::new()));
        };
        let rest = &segs[1..];
        let lower = name.to_ascii_lowercase();
        let unknown = || QueryError::UnknownField(name.clone());

        if forgebase_schema::SYSTEM_FIELDS.contains(&lower.as_str()) {
            if !rest.is_empty() {
                return Err(unknown());
            }
            return Ok(Res::Scalar {
                sql: row.col(d, &lower),
                json: false,
                ty: if lower == "_version" {
                    ScalarType::Integer
                } else {
                    ScalarType::Text
                },
            });
        }

        let Some(field) = coll.field(name) else {
            // Flexible collections keep undeclared keys in the `_data` document.
            if coll.schema_mode == SchemaMode::Flexible && !lower.starts_with('_') && lower != "expand" {
                let path = json_path(segs)?;
                return Ok(Res::Scalar {
                    sql: json_get(d, &row.col(d, "_data"), &path),
                    json: true,
                    ty: ScalarType::Json,
                });
            }
            return Err(unknown());
        };
        if !field.kind.is_queryable()
            || (field.hidden && !self.opts.allow_hidden)
            || self.opts.field_guard.as_ref().is_some_and(|g| !g.readable(coll, field))
        {
            return Err(unknown());
        }
        let col = row.col(d, &field.name);
        let multi_restricted = |what: &str| {
            Err(QueryError::InvalidOperation(format!(
                "{name}: {what} cannot be used in index definitions"
            )))
        };

        match &field.kind {
            FieldKind::Relation {
                collection_id,
                max_select,
                ..
            } => {
                if rest.is_empty() && *max_select <= 1 {
                    return Ok(Res::Scalar {
                        sql: col,
                        json: false,
                        ty: ScalarType::Text,
                    });
                }
                if self.opts.inline_literals {
                    return multi_restricted("relations");
                }
                let (from, elem) = if *max_select > 1 {
                    let alias = b.alias("j");
                    let (f, v) = d.json_array_elements(&col, &alias);
                    (Some(f), v)
                } else {
                    (None, col.clone())
                };
                if rest.is_empty() {
                    return Ok(Res::Multi {
                        from: from.into_iter().collect(),
                        value: elem,
                        array: Some(col),
                        ty: ScalarType::Text,
                    });
                }
                if depth + 1 > self.opts.max_relation_depth {
                    return Err(QueryError::TooComplex(format!(
                        "paths may cross at most {} relations",
                        self.opts.max_relation_depth
                    )));
                }
                let target = self
                    .catalog
                    .collection_by_id(collection_id)
                    .ok_or_else(|| QueryError::UnknownField(format!("{name} (target collection missing)")))?;
                if let Some(n) = &self.opts.native {
                    // Native policies read the target with full visibility
                    // through a SECURITY DEFINER function (rls-coverage §3.4).
                    let mut inner_b = SqlBuilder::new(d);
                    let inner = self.segments(&mut inner_b, target, &RowRef::Alias("r".into()), rest, depth + 1)?;
                    let Res::Scalar { sql, json, ty } = inner else {
                        return Err(QueryError::InvalidOperation(format!(
                            "{name}: a native policy cannot follow a relation to several values"
                        )));
                    };
                    if !inner_b.params().is_empty() {
                        return Err(QueryError::InvalidOperation(format!(
                            "{name}: values in relation paths cannot be used in a native policy"
                        )));
                    }
                    let call = format!("{}({elem})", n.relation(&target.name, "r", &sql, ty)?);
                    return Ok(match from {
                        None => Res::Scalar { sql: call, json, ty },
                        Some(f) => Res::Multi {
                            from: vec![f],
                            value: call,
                            array: None,
                            ty,
                        },
                    });
                }
                b.lookups = true;
                let next = RowRef::Lookup {
                    table: d.quote_ident(&target.name),
                    alias: b.alias("r"),
                    id_expr: elem,
                };
                let inner = self.segments(b, target, &next, rest, depth + 1)?;
                Ok(match (from, inner) {
                    (None, r) => r,
                    (Some(f), Res::Scalar { sql, ty, .. }) => Res::Multi {
                        from: vec![f],
                        value: sql,
                        array: None,
                        ty,
                    },
                    (
                        Some(f),
                        Res::Multi {
                            from: more, value, ty, ..
                        },
                    ) => {
                        let mut all = vec![f];
                        all.extend(more);
                        Res::Multi {
                            from: all,
                            value,
                            array: None,
                            ty,
                        }
                    }
                })
            }
            FieldKind::Json { .. } => {
                if rest.is_empty() {
                    Ok(Res::Scalar {
                        sql: col,
                        json: true,
                        ty: ScalarType::Json,
                    })
                } else {
                    Ok(Res::Scalar {
                        sql: json_get(d, &col, &json_path(rest)?),
                        json: true,
                        ty: ScalarType::Json,
                    })
                }
            }
            FieldKind::Geo => match rest {
                [PathSeg::Name(k)] if k == "lon" || k == "lat" => Ok(Res::Scalar {
                    sql: json_get(d, &col, &json_path(rest)?),
                    json: false,
                    ty: ScalarType::Json,
                }),
                [] => Ok(Res::Scalar {
                    sql: col,
                    json: true,
                    ty: ScalarType::Json,
                }),
                _ => Err(QueryError::UnknownField(format!("{name}: geo has only lon and lat"))),
            },
            kind if kind.is_multi() => {
                if self.opts.inline_literals && rest.is_empty() {
                    return multi_restricted("multi-value fields (use :length)");
                }
                let alias = b.alias("j");
                // Array items are any JSON value: on PostgreSQL iterate them
                // as `jsonb` so they compare by type. Select and file values
                // are strings.
                let (from, elem, ty) = if is_pg(d) && matches!(kind, FieldKind::Array { .. }) {
                    (
                        format!(
                            "jsonb_array_elements(CASE WHEN jsonb_typeof(({col})::jsonb) = 'array' THEN ({col})::jsonb END) AS {alias}(value)"
                        ),
                        format!("{alias}.value"),
                        ScalarType::Json,
                    )
                } else {
                    let (f, v) = d.json_array_elements(&col, &alias);
                    (f, v, ScalarType::Text)
                };
                if rest.is_empty() {
                    let value = if ty == ScalarType::Json {
                        format!("nullif({elem}, 'null'::jsonb)")
                    } else {
                        elem
                    };
                    return Ok(Res::Multi {
                        from: vec![from],
                        value,
                        array: Some(col),
                        ty,
                    });
                }
                if self.opts.inline_literals {
                    return multi_restricted("multi-value fields");
                }
                match kind {
                    FieldKind::Array { .. } => Ok(Res::Multi {
                        from: vec![from],
                        value: json_get(d, &elem, &json_path(rest)?),
                        array: None,
                        ty: ScalarType::Json,
                    }),
                    _ => Err(QueryError::UnknownField(format!("{name} has no sub-fields"))),
                }
            }
            kind => {
                if rest.is_empty() {
                    Ok(Res::Scalar {
                        sql: col,
                        json: false,
                        ty: ScalarType::of_storage(kind.storage()),
                    })
                } else {
                    Err(QueryError::UnknownField(format!("{name} has no sub-fields")))
                }
            }
        }
    }
}

/// Equality and range keys used by a filter (for slow-query labels and index
/// recommendations): paths compared with a constant at the top level of AND
/// chains.
pub fn filter_keys(expr: &Expr) -> (Vec<String>, Vec<String>) {
    fn walk(e: &Expr, eq: &mut Vec<String>, range: &mut Vec<String>) {
        match e {
            Expr::And(items) => items.iter().for_each(|i| walk(i, eq, range)),
            Expr::Compare(c) => {
                let (path, op) = match (&c.left, &c.right) {
                    (Operand::Path(p), r) if !matches!(r, Operand::Path(_)) => (p, c.op),
                    (l, Operand::Path(p)) if !matches!(l, Operand::Path(_)) => (p, c.op.flipped()),
                    _ => return,
                };
                if c.any || path.modifier.is_some() {
                    return;
                }
                let s = path.to_string();
                let target = match op {
                    Op::Eq => eq,
                    o if o.is_range() => range,
                    _ => return,
                };
                if !target.contains(&s) {
                    target.push(s);
                }
            }
            _ => {}
        }
    }
    let (mut eq, mut range) = (Vec::new(), Vec::new());
    walk(expr, &mut eq, &mut range);
    (eq, range)
}

/// A slow-query label for a filtered, sorted read of `collection`.
pub fn query_label(collection: &str, filter: Option<&Expr>, sort: &[SortKey]) -> QueryLabel {
    let (equality_keys, range_keys) = filter.map(filter_keys).unwrap_or_default();
    QueryLabel {
        collection: collection.to_owned(),
        equality_keys,
        range_keys,
        sort_keys: sort
            .iter()
            .map(|k| format!("{}{}", if k.desc { "-" } else { "" }, k.path))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::parse_filter;
    use forgebase_db::{PgDialect, SqliteDialect};
    use forgebase_schema::{Field, FieldKind};

    fn catalog() -> HashMap<String, Collection> {
        let mut users = Collection::base("users")
            .with_field(Field::new(
                "name",
                FieldKind::Text {
                    min_length: None,
                    max_length: None,
                },
            ))
            .with_field(Field {
                hidden: true,
                ..Field::new(
                    "secret",
                    FieldKind::Text {
                        min_length: None,
                        max_length: None,
                    },
                )
            });
        users.id = "u".into();
        let mut posts = Collection::base("posts")
            .with_field(Field::new(
                "title",
                FieldKind::Text {
                    min_length: None,
                    max_length: None,
                },
            ))
            .with_field(Field::new(
                "author",
                FieldKind::Relation {
                    collection_id: "u".into(),
                    max_select: 1,
                    cascade_delete: false,
                },
            ))
            .with_field(Field::new(
                "tags",
                FieldKind::Select {
                    values: vec!["a".into(), "b".into()],
                    max_select: 2,
                },
            ))
            .with_field(Field::new("meta", FieldKind::Json { max_size: None }))
            .with_field(Field::new("pw", FieldKind::Password { min_length: 8 }));
        posts.id = "p".into();
        let mut docs = Collection::documents("docs");
        docs.id = "d".into();
        HashMap::from([("u".into(), users), ("p".into(), posts), ("d".into(), docs)])
    }

    fn compile(coll: &str, filter: &str, opts: &CompileOptions) -> Result<(String, Vec<Value>), QueryError> {
        compile_with(&SqliteDialect, coll, filter, opts)
    }

    fn compile_with(
        d: &dyn Dialect,
        coll: &str,
        filter: &str,
        opts: &CompileOptions,
    ) -> Result<(String, Vec<Value>), QueryError> {
        let cat = catalog();
        let root = cat.get(coll).unwrap().clone();
        let expr = parse_filter(filter)?.unwrap();
        let mut b = SqlBuilder::new(d);
        let sql = Compiler::new(&cat, opts).filter(&mut b, &root, "t", &expr)?;
        Ok((sql, b.into_params()))
    }

    fn pg(coll: &str, filter: &str) -> (String, Vec<Value>) {
        compile_with(&PgDialect, coll, filter, &CompileOptions::default()).unwrap()
    }

    #[test]
    fn postgres_json_paths_are_typed_jsonb() {
        let (sql, params) = pg("p", "meta.a.b > 5 && meta.c = 'x' && meta.f = true");
        assert_eq!(
            sql,
            "(nullif((t.\"meta\")::jsonb->'a'->'b', 'null'::jsonb) > CAST($1 AS jsonb)) AND \
             (nullif((t.\"meta\")::jsonb->'c', 'null'::jsonb) = CAST($2 AS jsonb)) AND \
             (nullif((t.\"meta\")::jsonb->'f', 'null'::jsonb) = CAST($3 AS jsonb))"
        );
        assert_eq!(
            params,
            vec![
                Value::Text("5".into()),
                Value::Text("\"x\"".into()),
                Value::Text("true".into())
            ]
        );
        let (sql, _) = pg("d", "metadata.specs.0.ram >= 16 && metadata.brand = null");
        assert_eq!(
            sql,
            "(nullif((t.\"_data\")::jsonb->'metadata'->'specs'->0->'ram', 'null'::jsonb) >= CAST($1 AS jsonb)) AND \
             (nullif((t.\"_data\")::jsonb->'metadata'->'brand', 'null'::jsonb) IS NULL)"
        );
    }

    #[test]
    fn postgres_like_is_ilike_on_text() {
        let (sql, params) = pg("p", "title ~ 'A%' && meta.k ~ 'x' && title !~ 'y'");
        assert_eq!(
            sql,
            "(t.\"title\" ILIKE $1 ESCAPE '\\') AND \
             ((nullif((t.\"meta\")::jsonb->'k', 'null'::jsonb) #>> '{}') ILIKE $2 ESCAPE '\\') AND \
             (NOT (coalesce(t.\"title\", '') ILIKE $3 ESCAPE '\\'))"
        );
        assert_eq!(params[0], Value::Text("%A\\%%".into()));
        let (sql, _) = compile("p", "title !~ 'y'", &CompileOptions::default()).unwrap();
        assert_eq!(sql, "NOT (coalesce(t.\"title\", '') LIKE ?1 ESCAPE '\\')");
    }

    #[test]
    fn postgres_multi_values_and_relations() {
        let (sql, _) = pg("p", "tags ?= 'a' && author.name = 'ann' && tags:length > 1");
        assert_eq!(
            sql,
            "(EXISTS (SELECT 1 FROM jsonb_array_elements_text((t.\"tags\")::jsonb) AS j1(value) WHERE j1.value = $1)) AND \
             ((SELECT r2.\"name\" FROM \"users\" AS r2 WHERE r2.\"id\" = t.\"author\") = $2) AND \
             (coalesce(CASE WHEN jsonb_typeof((t.\"tags\")::jsonb) = 'array' THEN jsonb_array_length((t.\"tags\")::jsonb) END, 0) > $3)"
        );
        // Path to path: a JSON value against text compares as jsonb.
        let (sql, _) = pg("p", "meta.t = title && title:lower = 'x'");
        assert!(
            sql.starts_with(
                "(nullif((t.\"meta\")::jsonb->'t', 'null'::jsonb) IS NOT DISTINCT FROM to_jsonb(t.\"title\"))"
            ),
            "{sql}"
        );
        assert!(sql.ends_with("(lower(t.\"title\") = $1)"), "{sql}");
    }

    #[test]
    fn postgres_inline_literals_for_indexes() {
        let o = CompileOptions {
            inline_literals: true,
            ..CompileOptions::default()
        };
        let (sql, params) = compile_with(&PgDialect, "p", r"title = 'it\'s' && meta.n > 2.5", &o).unwrap();
        assert!(params.is_empty());
        assert_eq!(
            sql,
            "(t.\"title\" = 'it''s') AND (nullif((t.\"meta\")::jsonb->'n', 'null'::jsonb) > '2.5'::jsonb)"
        );
    }

    #[test]
    fn order_by_keeps_sqlite_null_order() {
        let cat = catalog();
        let root = cat.get("p").unwrap().clone();
        let keys = crate::filter::parse_sort("-title,meta.n").unwrap();
        let o = CompileOptions::default();
        for (d, want) in [
            (
                &SqliteDialect as &dyn Dialect,
                vec!["t.\"title\" DESC", "json_extract(t.\"meta\", '$.n') ASC"],
            ),
            (
                &PgDialect,
                vec![
                    "t.\"title\" DESC NULLS LAST",
                    "nullif((t.\"meta\")::jsonb->'n', 'null'::jsonb) ASC NULLS FIRST",
                ],
            ),
        ] {
            let mut b = SqlBuilder::new(d);
            assert_eq!(
                Compiler::new(&cat, &o).order_by(&mut b, &root, "t", &keys).unwrap(),
                want
            );
        }
    }

    #[test]
    fn values_are_always_bound() {
        let o = CompileOptions::default();
        let (sql, params) = compile("p", r"title = 'x\' OR 1=1 --' && meta.a.b > 5", &o).unwrap();
        assert_eq!(sql, "(t.\"title\" = ?1) AND (json_extract(t.\"meta\", '$.a.b') > ?2)");
        assert_eq!(params, vec![Value::Text("x' OR 1=1 --".into()), Value::Integer(5)]);
    }

    #[test]
    fn like_escapes_wildcards() {
        let o = CompileOptions::default();
        let (sql, params) = compile("p", "title ~ '100%_x'", &o).unwrap();
        assert_eq!(sql, "t.\"title\" LIKE ?1 ESCAPE '\\'");
        assert_eq!(params, vec![Value::Text("%100\\%\\_x%".into())]);
    }

    #[test]
    fn relations_and_multi_values() {
        let o = CompileOptions::default();
        let (sql, _) = compile("p", "author.name = 'ann'", &o).unwrap();
        assert_eq!(
            sql,
            "(SELECT r1.\"name\" FROM \"users\" AS r1 WHERE r1.\"id\" = t.\"author\") = ?1"
        );
        let (sql, _) = compile("p", "tags ?= 'a'", &o).unwrap();
        assert_eq!(
            sql,
            "EXISTS (SELECT 1 FROM json_each(t.\"tags\") AS j1 WHERE j1.value = ?1)"
        );
        let (sql, _) = compile("p", "tags = 'a'", &o).unwrap();
        assert!(sql.starts_with("NOT EXISTS"), "{sql}");
        let (sql, _) = compile("p", "tags:length > 1", &o).unwrap();
        assert_eq!(sql, "coalesce(json_array_length(t.\"tags\"), 0) > ?1");
    }

    #[test]
    fn documents_use_json_paths() {
        let o = CompileOptions::default();
        let (sql, _) = compile("d", "metadata.specifications.ram >= 16", &o).unwrap();
        assert_eq!(sql, "json_extract(t.\"_data\", '$.metadata.specifications.ram') >= ?1");
    }

    #[test]
    fn unknown_hidden_and_secret_fields_are_rejected() {
        let o = CompileOptions::default();
        for f in [
            "nope = 1",
            "secret = 'x'",
            "pw = 'x'",
            "author.secret = 'x'",
            "_data = 1",
            "title.x = 1",
        ] {
            assert!(compile("p", f, &o).is_err(), "{f}");
        }
        let admin = CompileOptions {
            allow_hidden: true,
            ..CompileOptions::default()
        };
        assert!(compile("u", "secret = 'x'", &admin).is_ok());
        assert!(compile("d", "_data = 1", &o).is_err());
    }

    #[test]
    fn params_and_nulls() {
        let mut o = CompileOptions::default();
        o.params.insert("who".into(), serde_json::json!("x' --"));
        let (sql, params) = compile("p", "title = {:who} && author != null", &o).unwrap();
        assert_eq!(sql, "(t.\"title\" = ?1) AND (t.\"author\" IS NOT NULL)");
        assert_eq!(params, vec![Value::Text("x' --".into())]);
        assert!(matches!(
            compile("p", "title = {:missing}", &o),
            Err(QueryError::MissingParam(_))
        ));
        assert!(compile("p", "title > null", &o).is_err());
    }

    #[test]
    fn inline_mode_for_index_definitions() {
        let o = CompileOptions {
            inline_literals: true,
            ..CompileOptions::default()
        };
        let (sql, params) = compile("p", r"title = 'it\'s' && meta.n > 2.5", &o).unwrap();
        assert!(params.is_empty());
        assert_eq!(
            sql,
            "(t.\"title\" = 'it''s') AND (json_extract(t.\"meta\", '$.n') > 2.5)"
        );
        assert!(compile("p", "author.name = 'x'", &o).is_err());
        assert!(compile("p", "tags ?= 'a'", &o).is_err());
        assert!(compile("p", "created < @now", &o).is_err());
    }

    #[test]
    fn keys_for_labels() {
        let e = parse_filter("a = 1 && b > 2 && 3 < c && (d = 1 || e = 2)")
            .unwrap()
            .unwrap();
        let (eq, range) = filter_keys(&e);
        assert_eq!(eq, vec!["a"]);
        assert_eq!(range, vec!["b", "c"]);
    }
}
