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

use forgebase_db::{Dialect, JsonPathSeg, QueryLabel, Value};
use forgebase_schema::{Collection, FieldKind, SchemaMode};
use serde_json::Value as Json;

use crate::QueryError;
use crate::filter::{Compare, Expr, FieldPath, Literal, Modifier, Op, Operand, PathSeg, SortKey};

/// Accumulates bound parameters and unique aliases for one statement.
#[derive(Debug)]
pub struct SqlBuilder<'d> {
    dialect: &'d dyn Dialect,
    params: Vec<Value>,
    aliases: u32,
}

impl<'d> SqlBuilder<'d> {
    /// A builder for one statement.
    pub fn new(dialect: &'d dyn Dialect) -> Self {
        Self {
            dialect,
            params: Vec::new(),
            aliases: 0,
        }
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
pub trait Catalog {
    /// Collection with this id.
    fn collection_by_id(&self, id: &str) -> Option<&Collection>;
}

impl Catalog for HashMap<String, Collection> {
    fn collection_by_id(&self, id: &str) -> Option<&Collection> {
        self.get(id)
    }
}

/// Compilation options.
#[derive(Debug, Clone)]
pub struct CompileOptions {
    /// Hidden fields may be referenced (administrators, system).
    pub allow_hidden: bool,
    /// Maximum relation hops in one path.
    pub max_relation_depth: usize,
    /// Render literals into SQL text (index definitions only). Parameters,
    /// `@now`, relations and multi-value fields are rejected in this mode.
    pub inline_literals: bool,
    /// Values for `{:name}` parameters.
    pub params: HashMap<String, Json>,
    /// Value of `@now`.
    pub now: String,
}

impl Default for CompileOptions {
    fn default() -> Self {
        Self {
            allow_hidden: false,
            max_relation_depth: 3,
            inline_literals: false,
            params: HashMap::new(),
            now: forgebase_schema::now_timestamp(),
        }
    }
}

/// A resolved operand.
#[derive(Debug, Clone)]
enum Res {
    /// One value per row.
    Scalar { sql: String, json: bool },
    /// Several values per row: `SELECT .. FROM <from>` yields them as `<value>`.
    Multi {
        from: Vec<String>,
        value: String,
        /// The array expression, when the value is the bare element.
        array: Option<String>,
    },
}

/// How a row is reached: directly through an alias, or by id lookup in a
/// related table (a correlated scalar subquery).
#[derive(Debug, Clone)]
enum RowRef {
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
        let row = RowRef::Alias(alias.to_owned());
        match self.path(b, root, &row, path, 0)? {
            Res::Scalar { sql, .. } => Ok(sql),
            Res::Multi { .. } => Err(QueryError::InvalidOperation(format!(
                "{path} holds several values; use {path}:length or a single-value field"
            ))),
        }
    }

    /// `ORDER BY` items for sort keys (without the keyword).
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
                Ok(format!("{e} {}", if k.desc { "DESC" } else { "ASC" }))
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
        Ok(match o {
            Operand::Literal(l) => match l {
                Literal::Null => Value::Null,
                Literal::Bool(v) => Value::from(*v),
                Literal::Int(i) => Value::Integer(*i),
                Literal::Float(f) => Value::Real(*f),
                Literal::Text(s) => Value::Text(s.clone()),
            },
            Operand::Param(name) => {
                if self.opts.inline_literals {
                    return Err(QueryError::InvalidOperation("parameters are not allowed here".into()));
                }
                match self.opts.params.get(name) {
                    None => return Err(QueryError::MissingParam(name.clone())),
                    Some(Json::Null) => Value::Null,
                    Some(Json::Bool(v)) => Value::from(*v),
                    Some(Json::Number(n)) => n
                        .as_i64()
                        .map(Value::Integer)
                        .or_else(|| n.as_f64().map(Value::Real))
                        .unwrap_or(Value::Null),
                    Some(Json::String(s)) => Value::Text(s.clone()),
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
                Value::Text(self.opts.now.clone())
            }
            Operand::Path(_) => return Err(QueryError::InvalidOperation("expected a value".into())),
        })
    }

    fn render(&self, b: &mut SqlBuilder<'_>, v: &Value) -> Result<String, QueryError> {
        if !self.opts.inline_literals {
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

    fn operand(&self, b: &mut SqlBuilder<'_>, root: &Collection, row: &RowRef, o: &Operand) -> Result<Res, QueryError> {
        match o {
            Operand::Path(p) => self.path(b, root, row, p, 0),
            other => {
                let v = self.constant(other)?;
                Ok(Res::Scalar {
                    sql: self.render(b, &v)?,
                    json: false,
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
            let esc = d.like_escape_clause();
            let test = |v: &str| match op {
                Op::Like => format!("{v} LIKE {pattern} {esc}"),
                _ => format!("coalesce({v}, '') NOT LIKE {pattern} {esc}"),
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
        let r = self.operand(b, root, row, right)?;
        // Against a non-null constant, `=` and `IS` agree; `=` is what every
        // planner matches to an index. Path-to-path equality stays NULL-safe.
        let constant = !matches!(right, Operand::Path(_));
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
            (Res::Scalar { sql: a, .. }, Res::Scalar { sql: z, .. }) => Ok(format!("{a} {sym} {z}")),
            (Res::Multi { from, value, .. }, Res::Scalar { sql: z, .. }) => {
                Ok(self.quantified(c.any, &from, &format!("{value} {sym} {z}")))
            }
            (Res::Scalar { sql: a, .. }, Res::Multi { from, value, .. }) => {
                let flipped = match op.flipped() {
                    Op::Gt => ">",
                    Op::Ge => ">=",
                    Op::Lt => "<",
                    Op::Le => "<=",
                    _ => sym,
                };
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
                },
                Res::Multi { .. } => {
                    return Err(QueryError::InvalidOperation(format!("{p}: :length needs an array")));
                }
                Res::Scalar { sql, json: true } => Res::Scalar {
                    sql: d.json_array_length(&sql),
                    json: false,
                },
                Res::Scalar { sql, json: false } => Res::Scalar {
                    sql: format!("length({sql})"),
                    json: false,
                },
            },
            Some(Modifier::Lower) => match res {
                Res::Scalar { sql, .. } => Res::Scalar {
                    sql: format!("lower({sql})"),
                    json: false,
                },
                Res::Multi { from, value, .. } => Res::Multi {
                    from,
                    value: format!("lower({value})"),
                    array: None,
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
            });
        }

        let Some(field) = coll.field(name) else {
            // Flexible collections keep undeclared keys in the `_data` document.
            if coll.schema_mode == SchemaMode::Flexible && !lower.starts_with('_') && lower != "expand" {
                let path = json_path(segs)?;
                return Ok(Res::Scalar {
                    sql: d.json_extract(&row.col(d, "_data"), &path),
                    json: true,
                });
            }
            return Err(unknown());
        };
        if !field.kind.is_queryable() || (field.hidden && !self.opts.allow_hidden) {
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
                    return Ok(Res::Scalar { sql: col, json: false });
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
                let next = RowRef::Lookup {
                    table: d.quote_ident(&target.name),
                    alias: b.alias("r"),
                    id_expr: elem,
                };
                let inner = self.segments(b, target, &next, rest, depth + 1)?;
                Ok(match (from, inner) {
                    (None, r) => r,
                    (Some(f), Res::Scalar { sql, .. }) => Res::Multi {
                        from: vec![f],
                        value: sql,
                        array: None,
                    },
                    (Some(f), Res::Multi { from: more, value, .. }) => {
                        let mut all = vec![f];
                        all.extend(more);
                        Res::Multi {
                            from: all,
                            value,
                            array: None,
                        }
                    }
                })
            }
            FieldKind::Json { .. } => {
                if rest.is_empty() {
                    Ok(Res::Scalar { sql: col, json: true })
                } else {
                    Ok(Res::Scalar {
                        sql: d.json_extract(&col, &json_path(rest)?),
                        json: true,
                    })
                }
            }
            FieldKind::Geo => match rest {
                [PathSeg::Name(k)] if k == "lon" || k == "lat" => Ok(Res::Scalar {
                    sql: d.json_extract(&col, &json_path(rest)?),
                    json: false,
                }),
                [] => Ok(Res::Scalar { sql: col, json: true }),
                _ => Err(QueryError::UnknownField(format!("{name}: geo has only lon and lat"))),
            },
            kind if kind.is_multi() => {
                if self.opts.inline_literals && rest.is_empty() {
                    return multi_restricted("multi-value fields (use :length)");
                }
                let alias = b.alias("j");
                let (from, elem) = d.json_array_elements(&col, &alias);
                if rest.is_empty() {
                    return Ok(Res::Multi {
                        from: vec![from],
                        value: elem,
                        array: Some(col),
                    });
                }
                if self.opts.inline_literals {
                    return multi_restricted("multi-value fields");
                }
                match kind {
                    FieldKind::Array { .. } => Ok(Res::Multi {
                        from: vec![from],
                        value: d.json_extract(&elem, &json_path(rest)?),
                        array: None,
                    }),
                    _ => Err(QueryError::UnknownField(format!("{name} has no sub-fields"))),
                }
            }
            _ => {
                if rest.is_empty() {
                    Ok(Res::Scalar { sql: col, json: false })
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
    use forgebase_db::SqliteDialect;
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
        let cat = catalog();
        let root = cat.get(coll).unwrap().clone();
        let expr = parse_filter(filter)?.unwrap();
        let d = SqliteDialect;
        let mut b = SqlBuilder::new(&d);
        let sql = Compiler::new(&cat, opts).filter(&mut b, &root, "t", &expr)?;
        Ok((sql, b.into_params()))
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
