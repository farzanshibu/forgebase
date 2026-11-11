//! Native render mode (FB-152, rls-coverage §3.4): compiles a rule into SQL
//! that PostgreSQL evaluates inside a row-level security policy.
//!
//! The data core partially evaluates a rule in Rust for each request
//! (`forgebase-policy` `rules`): request values are known, so comparisons
//! between them are decided before any SQL runs, and the rest binds them as
//! parameters. A policy is compiled once, so request references stay
//! references, read at run time from the caller's claims:
//!
//! * a reference renders as the `jsonb` SQL that [`NativeRender::request`]
//!   returns (SQL `NULL` when the value is absent, a JSON array when it holds
//!   several values); `@now` as [`NativeRender::now`];
//! * a comparison without any record field (`@request.auth.type = 'user'`)
//!   becomes `forgebase.rule_cmp(l, op, r, any)`, a SQL port of the Rust
//!   static comparison with its presence rules (`= null`, `= ""`) and
//!   three-valued result;
//! * a comparison of a reference with a record field keeps the field
//!   rendering of the normal compiler (so indexes are used) and casts the
//!   reference to the field's type; an absent value is *unknown* (SQL
//!   `NULL`), as in Rust; several values with `?op` are a three-valued OR
//!   over the items;
//! * literals are inlined (a policy has no parameters);
//! * a relation path (`author.team`) reads its target through a
//!   `SECURITY DEFINER` function that [`NativeRender::relation`] defines, so
//!   the lookup sees every row, as the data core's subquery does, instead of
//!   running into the target table's own policies.
//!
//! Anything else (`{:params}`, a relation path to several values) is an
//! error: the caller decides what a rule that cannot be compiled natively
//! becomes.

use std::fmt;
use std::sync::Arc;

use forgebase_db::Dialect;
use forgebase_schema::Collection;

use crate::QueryError;
use crate::compile::{Compiler, RowRef, ScalarType, SqlBuilder};
use crate::filter::{Compare, Literal, Op, Operand};

/// What native mode needs from the caller (the policy generator).
pub trait NativeRender: Send + Sync + fmt::Debug {
    /// `jsonb` SQL of the request reference behind parameter `name` (SQL
    /// `NULL` when absent, a JSON array for several values), or `None` when
    /// `name` is not a request reference.
    fn request(&self, name: &str) -> Option<String>;

    /// Text SQL of `@now`, in ForgeBase's timestamp format.
    fn now(&self) -> String;

    /// Defines a `SECURITY DEFINER` function returning `value_sql` (an
    /// expression over `alias`, a row of collection `target`) for the row
    /// whose id is the function's only argument, and returns the function's
    /// qualified name.
    fn relation(&self, target: &str, alias: &str, value_sql: &str, ty: ScalarType) -> Result<String, QueryError>;
}

/// Name of an operator inside `forgebase.rule_cmp`.
pub fn op_name(op: Op) -> &'static str {
    match op {
        Op::Eq => "=",
        Op::Ne => "!=",
        Op::Gt => ">",
        Op::Ge => ">=",
        Op::Lt => "<",
        Op::Le => "<=",
        Op::Like => "~",
        Op::NotLike => "!~",
    }
}

/// SQL type of a [`ScalarType`] on PostgreSQL.
pub fn pg_type(ty: ScalarType) -> &'static str {
    match ty {
        ScalarType::Text => "text",
        ScalarType::Integer => "bigint",
        ScalarType::Real => "double precision",
        ScalarType::Bool => "smallint",
        ScalarType::Json => "jsonb",
    }
}

enum Side {
    Path,
    Literal,
    Now,
    Request(String),
}

fn side(n: &dyn NativeRender, o: &Operand) -> Result<Side, QueryError> {
    Ok(match o {
        Operand::Path(_) => Side::Path,
        Operand::Literal(_) => Side::Literal,
        Operand::Now => Side::Now,
        Operand::Param(p) => match n.request(p) {
            Some(sql) => Side::Request(sql),
            None => {
                return Err(QueryError::InvalidOperation(format!(
                    "{{:{p}}} cannot be used in a native policy"
                )));
            }
        },
    })
}

/// `jsonb` SQL of a non-path operand (for `forgebase.rule_cmp`).
fn json_of(d: &dyn Dialect, n: &dyn NativeRender, o: &Operand) -> Result<String, QueryError> {
    Ok(match o {
        Operand::Literal(l) => {
            let j = match l {
                Literal::Null => serde_json::Value::Null,
                Literal::Bool(v) => serde_json::Value::Bool(*v),
                Literal::Int(i) => serde_json::Value::from(*i),
                Literal::Float(f) => serde_json::Number::from_f64(*f)
                    .map(serde_json::Value::Number)
                    .ok_or_else(|| QueryError::InvalidOperation("numbers must be finite".into()))?,
                Literal::Text(s) => {
                    if s.contains('\0') {
                        return Err(QueryError::InvalidOperation("strings cannot contain NUL".into()));
                    }
                    serde_json::Value::String(s.clone())
                }
            };
            format!("{}::jsonb", d.string_literal(&j.to_string()))
        }
        Operand::Now => format!("to_jsonb({})", n.now()),
        Operand::Param(p) => n
            .request(p)
            .ok_or_else(|| QueryError::InvalidOperation(format!("{{:{p}}} cannot be used in a native policy")))?,
        Operand::Path(_) => return Err(QueryError::InvalidOperation("expected a value".into())),
    })
}

/// A request value cast to the type of the field it is compared with, the
/// way the data core's bound parameter is typed by the server.
pub(crate) fn typed(json_sql: &str, ty: ScalarType) -> String {
    match ty {
        ScalarType::Json => json_sql.to_owned(),
        ScalarType::Text => format!("forgebase.jtext({json_sql})"),
        ScalarType::Integer | ScalarType::Real | ScalarType::Bool => format!("forgebase.jnum({json_sql})"),
    }
}

/// [`Compiler`]'s rendering of a constant in native mode: a request
/// reference or `@now` against an expression of type `ty`. `None` for
/// ordinary literals.
pub(crate) fn render_against(n: &dyn NativeRender, o: &Operand, ty: ScalarType) -> Option<String> {
    match o {
        Operand::Param(p) => n.request(p).map(|sql| typed(&sql, ty)),
        Operand::Now => Some(match ty {
            ScalarType::Json => format!("to_jsonb({})", n.now()),
            _ => n.now(),
        }),
        _ => None,
    }
}

/// The `~` pattern of a request reference: `'%' || escaped || '%'`.
pub(crate) fn like_pattern(n: &dyn NativeRender, o: &Operand) -> Option<String> {
    match o {
        Operand::Param(p) => n
            .request(p)
            .map(|sql| format!("('%' || forgebase.like_escape(forgebase.jtext({sql})) || '%')")),
        Operand::Now => Some(format!("('%' || forgebase.like_escape({}) || '%')", n.now())),
        _ => None,
    }
}

/// A native operand standing for the items of a request array (the `?op`
/// expansion): renders as `item` (`jsonb`).
#[derive(Debug)]
struct ItemOverlay {
    base: Arc<dyn NativeRender>,
    name: String,
    item: String,
}

impl NativeRender for ItemOverlay {
    fn request(&self, name: &str) -> Option<String> {
        if name == self.name {
            Some(self.item.clone())
        } else {
            self.base.request(name)
        }
    }

    fn now(&self) -> String {
        self.base.now()
    }

    fn relation(&self, target: &str, alias: &str, value_sql: &str, ty: ScalarType) -> Result<String, QueryError> {
        self.base.relation(target, alias, value_sql, ty)
    }
}

impl Compiler<'_> {
    /// [`Compiler`]'s comparison in native mode.
    pub(crate) fn native_compare(
        &self,
        n: &Arc<dyn NativeRender>,
        b: &mut SqlBuilder<'_>,
        root: &Collection,
        row: &RowRef,
        c: &Compare,
    ) -> Result<String, QueryError> {
        let (l, r) = (side(n.as_ref(), &c.left)?, side(n.as_ref(), &c.right)?);
        let d = b.dialect();
        match (&l, &r) {
            (Side::Path, Side::Path | Side::Literal) | (Side::Literal, Side::Path) => {
                self.compare_plain(b, root, row, c)
            }
            // No record field: decided at run time, as Rust decides it.
            (Side::Literal | Side::Now | Side::Request(_), Side::Literal | Side::Now | Side::Request(_)) => {
                Ok(format!(
                    "forgebase.rule_cmp({}, {}, {}, {})",
                    json_of(d, n.as_ref(), &c.left)?,
                    d.string_literal(op_name(c.op)),
                    json_of(d, n.as_ref(), &c.right)?,
                    if c.any { "true" } else { "false" }
                ))
            }
            // `@now` is always present: an ordinary constant.
            (Side::Path, Side::Now) | (Side::Now, Side::Path) => self.compare_plain(b, root, row, c),
            // A value on the right: absent or several values is unknown.
            (Side::Path, Side::Request(sql)) => {
                let one = self.compare_plain(b, root, row, c)?;
                Ok(format!(
                    "(CASE WHEN {sql} IS NULL OR jsonb_typeof({sql}) = 'array' THEN NULL ELSE ({one}) END)"
                ))
            }
            // A value on the left: absent is unknown; several values need
            // `?op` (some item matches, three-valued).
            (Side::Request(sql), Side::Path) => {
                let one = self.compare_plain(b, root, row, c)?;
                let many = if c.any {
                    self.native_items(n, b, root, row, c, sql)?
                } else {
                    "NULL".to_owned()
                };
                Ok(format!(
                    "(CASE WHEN {sql} IS NULL THEN NULL WHEN jsonb_typeof({sql}) = 'array' THEN {many} ELSE ({one}) END)"
                ))
            }
        }
    }

    /// `@request.x ?op field` over several values: true when some item
    /// matches, unknown when none does but one is unknown, else false (an
    /// empty array is false).
    fn native_items(
        &self,
        n: &Arc<dyn NativeRender>,
        b: &mut SqlBuilder<'_>,
        root: &Collection,
        row: &RowRef,
        c: &Compare,
        array_sql: &str,
    ) -> Result<String, QueryError> {
        let Operand::Param(name) = &c.left else {
            return Err(QueryError::InvalidOperation("expected a request value".into()));
        };
        let e = b.alias("e");
        let z = b.alias("z");
        let item = format!("{e}.v");
        let opts = crate::CompileOptions {
            native: Some(Arc::new(ItemOverlay {
                base: n.clone(),
                name: name.clone(),
                item: item.clone(),
            })),
            ..self.opts().clone()
        };
        let inner = Compiler::new(self.catalog(), &opts);
        let one = Compare {
            left: c.left.clone(),
            op: c.op,
            any: false,
            right: c.right.clone(),
        };
        let typed_cmp = inner.compare_plain(b, root, row, &one)?;
        // A null item is a null literal in Rust: `= null` tests presence.
        let null_cmp = inner
            .compare_plain(
                b,
                root,
                row,
                &Compare {
                    left: Operand::Literal(Literal::Null),
                    ..one
                },
            )
            .unwrap_or_else(|_| "NULL".to_owned());
        Ok(format!(
            "(SELECT CASE WHEN bool_or({z}.c) THEN true WHEN bool_or({z}.c IS NULL) THEN NULL ELSE false END \
             FROM jsonb_array_elements({array_sql}) AS {e}(v) \
             CROSS JOIN LATERAL (SELECT (CASE WHEN jsonb_typeof({item}) IN ('null', 'object', 'array') THEN ({null_cmp}) ELSE ({typed_cmp}) END) AS c) AS {z})"
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use forgebase_db::PgDialect;
    use forgebase_schema::{Collection, Field, FieldKind};

    use super::*;
    use crate::{CompileOptions, parse_filter};

    #[derive(Debug, Default)]
    struct Fake {
        defined: Mutex<Vec<String>>,
    }

    impl NativeRender for Fake {
        fn request(&self, name: &str) -> Option<String> {
            name.strip_prefix("__rq_").map(|i| format!("(SELECT forgebase.req('r{i}'))"))
        }
        fn now(&self) -> String {
            "(SELECT forgebase.now_text())".into()
        }
        fn relation(&self, target: &str, alias: &str, value_sql: &str, ty: ScalarType) -> Result<String, QueryError> {
            let name = format!("forgebase.fb_rel_{}", self.defined.lock().unwrap().len());
            self.defined.lock().unwrap().push(format!(
                "{name}(text) RETURNS {} AS SELECT {value_sql} FROM public.\"{target}\" AS {alias}",
                pg_type(ty)
            ));
            Ok(name)
        }
    }

    fn catalog() -> HashMap<String, Collection> {
        let mut users = Collection::base("users");
        users.id = "u".into();
        users.fields = vec![Field::new("team", FieldKind::Text { min_length: None, max_length: None })];
        let mut posts = Collection::base("posts");
        posts.id = "p".into();
        posts.fields = vec![
            Field::new(
                "owner",
                FieldKind::Relation {
                    collection_id: "u".into(),
                    max_select: 1,
                    cascade_delete: false,
                },
            ),
            Field::new(
                "count",
                FieldKind::Number {
                    integer: true,
                    min: None,
                    max: None,
                },
            ),
            Field::new("title", FieldKind::Text { min_length: None, max_length: None }),
        ];
        HashMap::from([("u".into(), users), ("p".into(), posts)])
    }

    fn native(filter: &str) -> (Result<String, QueryError>, Arc<Fake>) {
        let cat = catalog();
        let fake = Arc::new(Fake::default());
        let opts = CompileOptions {
            native: Some(fake.clone()),
            ..CompileOptions::default()
        };
        let mut b = SqlBuilder::new(&PgDialect);
        let expr = parse_filter(filter).unwrap().unwrap();
        let out = Compiler::new(&cat, &opts).filter(&mut b, &cat["p"], "t", &expr);
        assert!(b.params().is_empty(), "native mode binds nothing");
        (out, fake)
    }

    #[test]
    fn request_values_render_as_claims_and_literals_inline() {
        let (sql, _) = native("owner = {:__rq_0} && title = \"a'b\"");
        let sql = sql.unwrap();
        assert!(sql.contains("t.\"owner\" = forgebase.jtext((SELECT forgebase.req('r0')))"), "{sql}");
        assert!(sql.contains("CASE WHEN (SELECT forgebase.req('r0')) IS NULL OR"), "{sql}");
        assert!(sql.contains("t.\"title\" = 'a''b'"), "{sql}");
    }

    #[test]
    fn static_comparisons_go_to_rule_cmp() {
        let (sql, _) = native("{:__rq_1} = 'user' || {:__rq_2} ?= 'admin'");
        let sql = sql.unwrap();
        assert!(
            sql.contains("forgebase.rule_cmp((SELECT forgebase.req('r1')), '=', '\"user\"'::jsonb, false)"),
            "{sql}"
        );
        assert!(sql.contains("'\"admin\"'::jsonb, true)"), "{sql}");
    }

    #[test]
    fn numbers_and_items() {
        let (sql, _) = native("{:__rq_0} ?<= count");
        let sql = sql.unwrap();
        assert!(sql.contains("jsonb_array_elements((SELECT forgebase.req('r0')))"), "{sql}");
        assert!(sql.contains("t.\"count\" >= forgebase.jnum("), "{sql}");
    }

    #[test]
    fn relations_go_through_definer_functions() {
        let (sql, fake) = native("owner.team = 'red'");
        let sql = sql.unwrap();
        assert_eq!(sql, "forgebase.fb_rel_0(t.\"owner\") = 'red'");
        let defined = fake.defined.lock().unwrap();
        assert_eq!(defined.len(), 1);
        assert!(defined[0].contains("SELECT r.\"team\" FROM public.\"users\" AS r"), "{}", defined[0]);
    }

    #[test]
    fn caller_parameters_and_now() {
        let (err, _) = native("title = {:x}");
        assert!(err.is_err());
        let (sql, _) = native("title < @now");
        assert!(sql.unwrap().contains("t.\"title\" < (SELECT forgebase.now_text())"));
    }
}
