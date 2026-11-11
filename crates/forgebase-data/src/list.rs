//! Record queries: filter, sort, projection, offset and cursor pagination,
//! counts, full-text search and aggregation (ADR-0003 §5).

use std::collections::HashMap;

use base64::Engine;
use forgebase_db::{Statement, Value};
use forgebase_query::{
    Compiler, Expr, FieldPath, ScalarType, SortKey, SqlBuilder, SqlFlavor, parse_filter, parse_sort, query_label,
};
use forgebase_schema::Collection;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json};

use crate::indexes::{IndexDefinition, IndexKind};
use crate::records::{Record, project, row_to_record, sql_to_json};
use crate::{AccessContext, Action, DEFAULT_PER_PAGE, DataError, DataService, MAX_PER_PAGE, Predicate};

/// A record listing request.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ListQuery {
    /// Filter expression.
    pub filter: Option<String>,
    /// Values for `{:name}` parameters in the filter.
    pub params: HashMap<String, Json>,
    /// Sort keys, e.g. `-created,name`.
    pub sort: Option<String>,
    /// Paths to return (all when `None`).
    pub fields: Option<Vec<String>>,
    /// Relation paths to expand.
    pub expand: Vec<String>,
    /// Full-text query (needs a full-text index on the collection).
    pub search: Option<String>,
    /// 1-based page (offset pagination). Ignored with `cursor`.
    pub page: Option<u32>,
    /// Page size (default 30, at most 500).
    pub per_page: Option<u32>,
    /// Continue after the last record of a previous page (keyset pagination).
    pub cursor: Option<String>,
    /// Do not count all matching records.
    pub skip_total: bool,
}

/// A page of records.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ListResult {
    /// Records.
    pub items: Vec<Record>,
    /// Page number (offset pagination).
    pub page: Option<u32>,
    /// Page size.
    pub per_page: u32,
    /// Matching records (unless skipped or in cursor mode).
    pub total_items: Option<u64>,
    /// Pages (with `total_items`).
    pub total_pages: Option<u64>,
    /// Cursor for the next page; `None` on the last page.
    pub next_cursor: Option<String>,
}

/// Aggregate function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AggregateFunction {
    /// Number of rows (or of non-null values of `field`).
    Count,
    /// Sum.
    Sum,
    /// Average.
    Avg,
    /// Minimum.
    Min,
    /// Maximum.
    Max,
}

/// One aggregate column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AggregateSpec {
    /// Function.
    pub function: AggregateFunction,
    /// Field path (required except for `count`).
    #[serde(default)]
    pub field: Option<String>,
    /// Output key (default `function` or `function_field`).
    #[serde(default)]
    pub alias: Option<String>,
}

/// An aggregation request.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AggregateQuery {
    /// Filter expression.
    pub filter: Option<String>,
    /// Filter parameters.
    pub params: HashMap<String, Json>,
    /// Group-by paths.
    pub group_by: Vec<String>,
    /// Aggregates (default: count).
    pub aggregates: Vec<AggregateSpec>,
    /// Most groups returned (default 1000, at most 10 000).
    pub limit: Option<u32>,
}

pub(crate) struct BuiltList {
    pub(crate) select: Statement,
    pub(crate) count: Statement,
    pub(crate) per_page: u32,
    pub(crate) page: Option<u32>,
    pub(crate) sort_keys: usize,
    pub(crate) sort_sig: String,
    /// The statements read relation targets ([`SqlBuilder::follows_relations`]).
    pub(crate) follows_relations: bool,
}

#[derive(Serialize, Deserialize)]
struct Cursor {
    s: String,
    v: Vec<Json>,
    id: String,
}

fn json_to_value(j: &Json) -> Result<Value, DataError> {
    Ok(match j {
        Json::Null => Value::Null,
        Json::Bool(b) => Value::from(*b),
        Json::Number(n) => n
            .as_i64()
            .map_or_else(|| Value::Real(n.as_f64().unwrap_or(0.0)), Value::Integer),
        Json::String(s) => Value::Text(s.clone()),
        _ => return Err(DataError::invalid("invalid_cursor", "malformed cursor")),
    })
}

/// Sanitises a full-text query: every word is quoted, so FTS5 operators in
/// user input are matched as text. PostgreSQL's `websearch_to_tsquery`
/// never fails on user input, so there the words are passed as they are.
fn fts_query(text: &str, flavor: SqlFlavor) -> Option<String> {
    let terms: Vec<String> = text
        .split_whitespace()
        .take(32)
        .map(|w| match flavor {
            SqlFlavor::Sqlite => format!("\"{}\"", w.replace('"', "\"\"")),
            SqlFlavor::Postgres => w.to_owned(),
        })
        .collect();
    (!terms.is_empty()).then(|| terms.join(" "))
}

/// A value read back from an expression of type `ty`: PostgreSQL returns
/// `jsonb` as JSON text (SQLite returns the scalar itself), and a `numeric`
/// sum that should be an integer (SQLite sums integers as integers) as a
/// float.
fn typed_json(flavor: SqlFlavor, ty: ScalarType, v: &Value) -> Json {
    match (flavor, ty, v) {
        (SqlFlavor::Postgres, ScalarType::Json, Value::Text(s)) => serde_json::from_str(s).unwrap_or(Json::Null),
        (SqlFlavor::Postgres, ScalarType::Integer, Value::Real(f)) if f.fract() == 0.0 && f.abs() < 9.0e15 => {
            Json::from(*f as i64)
        }
        _ => sql_to_json(v),
    }
}

impl DataService {
    pub(crate) fn build_list(
        &self,
        ctx: &AccessContext,
        coll: &Collection,
        q: &ListQuery,
        pred: Option<&Predicate>,
        fulltext: Option<&IndexDefinition>,
    ) -> Result<BuiltList, DataError> {
        let d = self.inner.backend.dialect();
        let cat = self.catalog();
        let mut opts = self.compile_options(ctx);
        opts.params = q.params.clone();
        opts.field_guard = self.field_guard(ctx, coll);
        let compiler = Compiler::new(&*cat, &opts);
        let mut b = SqlBuilder::new(d);
        let mut wheres: Vec<String> = Vec::new();
        if let Some(t) = self.tenant_clause(ctx, &mut b, coll, "t") {
            wheres.push(t);
        }
        let filter: Option<Expr> = match &q.filter {
            Some(f) => parse_filter(f)?,
            None => None,
        };
        if let Some(e) = &filter {
            wheres.push(compiler.filter(&mut b, coll, "t", e)?);
        }
        if let Some(p) = pred {
            wheres.push(self.compile_predicate(ctx, &mut b, coll, p)?);
        }
        if let Some(text) = q.search.as_deref().and_then(|s| fts_query(s, SqlFlavor::of(d))) {
            let fts = fulltext.ok_or_else(|| {
                DataError::invalid("no_fulltext_index", format!("{} has no full-text index", coll.name))
            })?;
            // Matching text in a field the caller cannot read would leak it.
            if let Some(guard) = &opts.field_guard {
                let blocked = fts.keys.iter().any(|k| {
                    let root = k.path.split(['.', ':']).next().unwrap_or_default();
                    coll.field(root).is_some_and(|f| !guard.readable(coll, f))
                });
                if blocked {
                    return Err(DataError::invalid(
                        "no_fulltext_index",
                        format!("{} has no full-text index you can search", coll.name),
                    ));
                }
            }
            // FB-144 (minimal shared edit): the index manager renders the
            // match (FTS5 on SQLite, the GIN-indexed tsvector on PostgreSQL).
            let p = b.bind(text);
            wheres.push(crate::indexes::fulltext_match(d, &cat, coll, fts, "t", &p)?);
        }

        let mut sort: Vec<SortKey> = parse_sort(q.sort.as_deref().unwrap_or(""))?;
        let ends_with_id = sort
            .last()
            .is_some_and(|k| k.path.to_string().eq_ignore_ascii_case("id"));
        if !ends_with_id {
            sort.push(SortKey {
                path: FieldPath::parse("id")?,
                desc: false,
            });
        }
        let exprs: Vec<String> = sort
            .iter()
            .map(|k| compiler.scalar(&mut b, coll, "t", &k.path))
            .collect::<Result<_, _>>()?;
        let sort_sig = sort
            .iter()
            .map(|k| format!("{}{}", if k.desc { "-" } else { "" }, k.path))
            .collect::<Vec<_>>()
            .join(",");

        let where_count = if wheres.is_empty() {
            String::new()
        } else {
            format!(
                " WHERE {}",
                wheres
                    .iter()
                    .map(|w| format!("({w})"))
                    .collect::<Vec<_>>()
                    .join(" AND ")
            )
        };
        let table = d.quote_ident(&coll.name);
        let label = query_label(
            &coll.name,
            filter.as_ref(),
            &sort[..sort.len() - usize::from(!ends_with_id)],
        );
        let count = Statement::new(
            format!("SELECT count(*) FROM {table} AS t{where_count}"),
            b.params().to_vec(),
        )
        .with_label(label.clone());

        let per_page = q.per_page.unwrap_or(DEFAULT_PER_PAGE).clamp(1, MAX_PER_PAGE);
        let mut page = None;
        let mut keyset = None;
        if let Some(c) = q.cursor.as_deref().filter(|c| !c.is_empty()) {
            let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(c)
                .map_err(|_| DataError::invalid("invalid_cursor", "malformed cursor"))?;
            let cur: Cursor =
                serde_json::from_slice(&bytes).map_err(|_| DataError::invalid("invalid_cursor", "malformed cursor"))?;
            if cur.s != sort_sig || cur.v.len() + 1 != exprs.len() {
                return Err(DataError::invalid(
                    "invalid_cursor",
                    "the cursor belongs to a different sort order",
                ));
            }
            let mut values: Vec<Value> = cur.v.iter().map(json_to_value).collect::<Result<_, _>>()?;
            values.push(Value::Text(cur.id));
            keyset = Some(self.keyset(&mut b, &exprs, &sort, &values));
        } else {
            page = Some(q.page.unwrap_or(1).max(1));
        }
        let mut all_wheres = wheres;
        if let Some(k) = keyset {
            all_wheres.push(k);
        }
        let where_sel = if all_wheres.is_empty() {
            String::new()
        } else {
            format!(
                " WHERE {}",
                all_wheres
                    .iter()
                    .map(|w| format!("({w})"))
                    .collect::<Vec<_>>()
                    .join(" AND ")
            )
        };
        let extra: Vec<String> = exprs
            .iter()
            .enumerate()
            .map(|(i, e)| format!("{e} AS \"__k{i}\""))
            .collect();
        let order: Vec<String> = exprs
            .iter()
            .zip(&sort)
            .map(|(e, k)| format!("{e} {}", d.order_nulls(k.desc)))
            .collect();
        let limit = b.bind(i64::from(per_page) + 1);
        let offset = b.bind(i64::from(page.map_or(0, |p| (p - 1) * per_page)));
        let follows_relations = b.follows_relations();
        let select = Statement::new(
            format!(
                "SELECT t.*, {} FROM {table} AS t{where_sel} ORDER BY {} LIMIT {limit} OFFSET {offset}",
                extra.join(", "),
                order.join(", ")
            ),
            b.into_params(),
        )
        .with_label(label);
        Ok(BuiltList {
            select,
            count,
            per_page,
            page,
            sort_keys: exprs.len(),
            sort_sig,
            follows_relations,
        })
    }

    /// Rows strictly after `values` in the sort order. NULLs sort first
    /// ascending and last descending on every engine (`Dialect::order_nulls`).
    /// On PostgreSQL a JSON key is `jsonb`; its cursor value is the JSON text
    /// the previous page returned, which binds back as `jsonb`.
    fn keyset(&self, b: &mut SqlBuilder<'_>, exprs: &[String], sort: &[SortKey], values: &[Value]) -> String {
        let is = b.dialect().null_safe_eq();
        let mut clause = "1 = 0".to_owned();
        for i in (0..exprs.len()).rev() {
            let (e, desc, v) = (&exprs[i], sort[i].desc, &values[i]);
            let after = match (desc, v.is_null()) {
                (false, false) => format!("{e} > {}", b.bind(v.clone())),
                (false, true) => format!("{e} IS NOT NULL"),
                (true, false) => format!("({e} < {} OR {e} IS NULL)", b.bind(v.clone())),
                (true, true) => "1 = 0".to_owned(),
            };
            clause = if i == exprs.len() - 1 {
                after
            } else {
                format!("{after} OR ({e} {is} {} AND ({clause}))", b.bind(v.clone()))
            };
        }
        clause
    }

    /// Lists records.
    pub async fn list_records(
        &self,
        ctx: &AccessContext,
        collection: &str,
        q: &ListQuery,
    ) -> Result<ListResult, DataError> {
        let (coll, pred) = self.resolve(ctx, Action::List, collection)?;
        let fts = self.fulltext_index(&coll, q).await?;
        let built = self.build_list(ctx, &coll, q, pred.as_ref(), fts.as_ref())?;
        let rows = self
            .read_rows(ctx, Action::List, &coll, &built.select, built.follows_relations)
            .await?;
        let more = rows.rows.len() > built.per_page as usize;
        let mut items: Vec<Record> = Vec::with_capacity(rows.rows.len().min(built.per_page as usize));
        let mut last_keys: Option<Vec<Json>> = None;
        let mask = self.mask(ctx, &coll);
        for row in rows.rows.iter().take(built.per_page as usize) {
            items.push(row_to_record(&coll, &rows.columns, row, &mask));
            let start = rows.columns.len() - built.sort_keys;
            last_keys = Some(row[start..].iter().map(sql_to_json).collect());
        }
        let next_cursor = match (more, last_keys) {
            (true, Some(mut keys)) => {
                let id = keys
                    .pop()
                    .and_then(|j| j.as_str().map(str::to_owned))
                    .unwrap_or_default();
                let cur = Cursor {
                    s: built.sort_sig.clone(),
                    v: keys,
                    id,
                };
                Some(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(&cur)?))
            }
            _ => None,
        };
        let (total_items, total_pages) = if q.skip_total || built.page.is_none() {
            (None, None)
        } else {
            let n = self
                .read_rows(ctx, Action::List, &coll, &built.count, built.follows_relations)
                .await?
                .scalar()
                .and_then(Value::as_i64)
                .unwrap_or(0)
                .max(0) as u64;
            (Some(n), Some(n.div_ceil(u64::from(built.per_page))))
        };
        if !q.expand.is_empty() {
            self.expand(ctx, &coll, &mut items, &q.expand, 0).await?;
        }
        if let Some(fields) = &q.fields {
            items = items.iter().map(|r| project(r, fields)).collect();
        }
        Ok(ListResult {
            items,
            page: built.page,
            per_page: built.per_page,
            total_items,
            total_pages,
            next_cursor,
        })
    }

    /// Counts records matching a filter.
    pub async fn count_records(
        &self,
        ctx: &AccessContext,
        collection: &str,
        filter: Option<&str>,
        params: HashMap<String, Json>,
    ) -> Result<u64, DataError> {
        let (coll, pred) = self.resolve(ctx, Action::List, collection)?;
        let q = ListQuery {
            filter: filter.map(str::to_owned),
            params,
            ..ListQuery::default()
        };
        let built = self.build_list(ctx, &coll, &q, pred.as_ref(), None)?;
        let rows = self
            .read_rows(ctx, Action::List, &coll, &built.count, built.follows_relations)
            .await?;
        Ok(rows.scalar().and_then(Value::as_i64).unwrap_or(0).max(0) as u64)
    }

    /// Groups and aggregates records. Each result row has one key per
    /// group-by path and one per aggregate.
    pub async fn aggregate(
        &self,
        ctx: &AccessContext,
        collection: &str,
        q: &AggregateQuery,
    ) -> Result<Vec<Record>, DataError> {
        let (coll, pred) = self.resolve(ctx, Action::List, collection)?;
        let d = self.inner.backend.dialect();
        let cat = self.catalog();
        let mut opts = self.compile_options(ctx);
        opts.params = q.params.clone();
        opts.field_guard = self.field_guard(ctx, &coll);
        let compiler = Compiler::new(&*cat, &opts);
        let mut b = SqlBuilder::new(d);
        if q.group_by.len() > 8 || q.aggregates.len() > 16 {
            return Err(DataError::invalid(
                "invalid_aggregate",
                "at most 8 group keys and 16 aggregates",
            ));
        }
        let flavor = SqlFlavor::of(d);
        let pg = flavor == SqlFlavor::Postgres;
        let mut select: Vec<String> = Vec::new();
        let mut groups: Vec<String> = Vec::new();
        let mut keys: Vec<String> = Vec::new();
        // Type of each output column (JSON values come back as JSON text on
        // PostgreSQL).
        let mut types: Vec<ScalarType> = Vec::new();
        for (i, g) in q.group_by.iter().enumerate() {
            let (e, ty) = compiler.scalar_typed(&mut b, &coll, "t", &FieldPath::parse(g)?)?;
            select.push(format!("{e} AS \"__g{i}\""));
            groups.push(e);
            keys.push(g.clone());
            types.push(ty);
        }
        let specs = if q.aggregates.is_empty() {
            vec![AggregateSpec {
                function: AggregateFunction::Count,
                field: None,
                alias: None,
            }]
        } else {
            q.aggregates.clone()
        };
        for (i, a) in specs.iter().enumerate() {
            let f = match a.function {
                AggregateFunction::Count => "count",
                AggregateFunction::Sum => "sum",
                AggregateFunction::Avg => "avg",
                AggregateFunction::Min => "min",
                AggregateFunction::Max => "max",
            };
            let (arg, ty) = match &a.field {
                Some(p) => compiler.scalar_typed(&mut b, &coll, "t", &FieldPath::parse(p)?)?,
                None if a.function == AggregateFunction::Count => ("*".to_owned(), ScalarType::Integer),
                None => return Err(DataError::invalid("invalid_aggregate", format!("{f} needs a field"))),
            };
            // PostgreSQL: `jsonb` has no sum or average and no min/max
            // aggregate; integer sums are `numeric`. Keep SQLite's results:
            // numbers inside JSON are summed as numbers, JSON min/max use
            // `jsonb` order, and integer sums stay integers.
            let (expr, out_ty) = match (pg, a.function, ty) {
                (true, AggregateFunction::Sum | AggregateFunction::Avg, ScalarType::Json) => (
                    format!("{f}(CASE WHEN jsonb_typeof({arg}) = 'number' THEN ({arg} #>> '{{}}')::numeric END)"),
                    // A whole sum reads as an integer, as SQLite sums integers.
                    if a.function == AggregateFunction::Sum {
                        ScalarType::Integer
                    } else {
                        ScalarType::Real
                    },
                ),
                (true, AggregateFunction::Min | AggregateFunction::Max, ScalarType::Json) => (
                    format!(
                        "(array_agg({arg} ORDER BY {arg} {}) FILTER (WHERE {arg} IS NOT NULL))[1]",
                        if a.function == AggregateFunction::Min {
                            "ASC"
                        } else {
                            "DESC"
                        }
                    ),
                    ScalarType::Json,
                ),
                (true, AggregateFunction::Sum, ScalarType::Integer | ScalarType::Bool) => {
                    (format!("CAST(sum({arg}) AS bigint)"), ScalarType::Integer)
                }
                (_, AggregateFunction::Count, _) => (format!("{f}({arg})"), ScalarType::Integer),
                _ => (format!("{f}({arg})"), ty),
            };
            types.push(out_ty);
            select.push(format!("{expr} AS \"__a{i}\""));
            keys.push(a.alias.clone().unwrap_or_else(|| match &a.field {
                Some(p) => format!("{f}_{}", p.replace(['.', ':'], "_")),
                None => f.to_owned(),
            }));
        }
        let mut wheres = Vec::new();
        if let Some(t) = self.tenant_clause(ctx, &mut b, &coll, "t") {
            wheres.push(t);
        }
        if let Some(e) = q.filter.as_deref().map(parse_filter).transpose()?.flatten() {
            wheres.push(compiler.filter(&mut b, &coll, "t", &e)?);
        }
        if let Some(p) = &pred {
            wheres.push(self.compile_predicate(ctx, &mut b, &coll, p)?);
        }
        let mut sql = format!("SELECT {} FROM {} AS t", select.join(", "), d.quote_ident(&coll.name));
        if !wheres.is_empty() {
            sql.push_str(&format!(
                " WHERE {}",
                wheres
                    .iter()
                    .map(|w| format!("({w})"))
                    .collect::<Vec<_>>()
                    .join(" AND ")
            ));
        }
        if !groups.is_empty() {
            let order: Vec<String> = groups.iter().map(|g| format!("{g} {}", d.order_nulls(false))).collect();
            sql.push_str(&format!(
                " GROUP BY {} ORDER BY {}",
                groups.join(", "),
                order.join(", ")
            ));
        }
        let limit = q.limit.unwrap_or(1000).clamp(1, 10_000);
        sql.push_str(&format!(" LIMIT {}", b.bind(i64::from(limit))));
        let follows = b.follows_relations();
        let rows = self
            .read_rows(ctx, Action::List, &coll, &Statement::new(sql, b.into_params()), follows)
            .await?;
        Ok(rows
            .rows
            .iter()
            .map(|r| {
                let mut m = Map::new();
                for ((k, v), ty) in keys.iter().zip(r).zip(&types) {
                    m.insert(k.clone(), typed_json(flavor, *ty, v));
                }
                m
            })
            .collect())
    }

    /// The full-text index of a collection, when the query searches.
    pub(crate) async fn fulltext_index(
        &self,
        coll: &Collection,
        q: &ListQuery,
    ) -> Result<Option<IndexDefinition>, DataError> {
        if q.search.as_deref().is_none_or(|s| s.trim().is_empty()) {
            return Ok(None);
        }
        let rows = self
            .inner
            .backend
            .fetch_all(&Statement::new(
                "SELECT definition FROM _forgebase_indexes WHERE collection_id = ?1 AND kind = 'fulltext' ORDER BY name LIMIT 1",
                vec![coll.id.clone().into()],
            ))
            .await?;
        Ok(match rows.scalar().and_then(Value::as_str) {
            Some(s) => Some(serde_json::from_str::<IndexDefinition>(s)?).filter(|d| d.kind == IndexKind::Fulltext),
            None => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fulltext_queries_are_quoted() {
        assert_eq!(
            fts_query("laptop OR \"x\" NEAR(a b)", SqlFlavor::Sqlite).unwrap(),
            "\"laptop\" \"OR\" \"\"\"x\"\"\" \"NEAR(a\" \"b)\""
        );
        assert_eq!(fts_query("   ", SqlFlavor::Sqlite), None);
        assert_eq!(
            fts_query("  laptop  -bag \"x y\" ", SqlFlavor::Postgres).unwrap(),
            "laptop -bag \"x y\""
        );
    }
}
