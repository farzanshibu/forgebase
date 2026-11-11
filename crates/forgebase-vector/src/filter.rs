//! Metadata filters: the data core's filter language (ADR-0003 §5) compiled
//! against the items table, plus the caller's collection rule (FB-139).
//!
//! The items table uses the data core's column names (`id`, `created`,
//! `updated`, `_version`, `_data`), so it is described to the compiler as a
//! flexible collection without declared fields: `category = 'news'`
//! compiles to `json_extract(i."_data", '$.category') = ?`. Paths that start
//! with `_` (other than `_version`) are refused by the compiler, so a filter
//! can never name `collection_id`, `key` or anything outside the metadata.
//! Every value is a bound parameter.
//!
//! A collection rule (`vector_collection` resource rules, FB-134) is a
//! predicate over the fields of [`item_schema`] (`id`, `metadata`); the
//! statements read items through [`crate::store::ITEMS`], a derived table
//! that exposes `_data` as `metadata` too, and the rule is ANDed into the
//! same `WHERE` as the caller's filter: a search pre-filter, never a
//! post-filter.

use std::collections::{BTreeSet, HashMap};

use forgebase_data::query::{CompileOptions, Compiler, Expr, PathSeg, SqlBuilder, parse_filter};
use forgebase_data::schema::{Collection, SchemaMode};
use forgebase_data::{Predicate, ResourceKind, ResourceSchema};
use forgebase_db::Dialect;
use serde_json::Value as Json;

use crate::error::VectorError;

/// The item fields collection rules may read: `id` (text) and `metadata`
/// (JSON; `metadata.team = @request.auth.team`). Registered with the
/// authorizer when the engine opens.
pub fn item_schema() -> ResourceSchema {
    ResourceKind::VectorCollection.default_schema()
}

/// A parsed, validated filter with its parameters, and the caller's rule.
#[derive(Debug, Clone)]
pub(crate) struct Filter {
    user: Option<(Expr, HashMap<String, Json>)>,
    rule: Option<Predicate>,
}

fn root() -> Collection {
    let mut c = Collection::base("_forgebase_vector_items");
    c.schema_mode = SchemaMode::Flexible;
    c
}

impl Filter {
    /// Parses `text` (`None` or blank = no filter) and checks that it
    /// compiles, so a bad filter fails before any work is done.
    pub(crate) fn parse(
        dialect: &dyn Dialect,
        text: Option<&str>,
        params: &HashMap<String, Json>,
    ) -> Result<Option<Self>, VectorError> {
        let Some(text) = text.filter(|t| !t.trim().is_empty()) else {
            return Ok(None);
        };
        let Some(expr) = parse_filter(text)? else {
            return Ok(None);
        };
        let f = Filter {
            user: Some((expr, params.clone())),
            rule: None,
        };
        let mut b = SqlBuilder::new(dialect);
        f.compile(&mut b, "i")?;
        Ok(Some(f))
    }

    /// `filter` restricted by the caller's collection rule (`None`: no rule).
    pub(crate) fn restrict(
        dialect: &dyn Dialect,
        filter: Option<Self>,
        rule: Option<Predicate>,
    ) -> Result<Option<Self>, VectorError> {
        let Some(rule) = rule else {
            return Ok(filter);
        };
        let f = Filter {
            user: filter.and_then(|f| f.user),
            rule: Some(rule),
        };
        let mut b = SqlBuilder::new(dialect);
        f.compile(&mut b, "i")?;
        Ok(Some(f))
    }

    /// Top-level metadata keys the caller's own filter reads (lower case).
    pub(crate) fn user_keys(&self) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        if let Some((expr, _)) = &self.user {
            let mut e = expr.clone();
            e.for_each_path_mut(&mut |p| {
                if let Some(PathSeg::Name(n)) = p.segments.first() {
                    out.insert(n.to_ascii_lowercase());
                }
            });
        }
        out
    }

    /// The boolean SQL expression for rows of [`crate::store::ITEMS`]
    /// aliased `alias`; parameters go to `b`.
    pub(crate) fn compile(&self, b: &mut SqlBuilder<'_>, alias: &str) -> Result<String, VectorError> {
        let mut parts = Vec::with_capacity(2);
        if let Some((expr, params)) = &self.user {
            let catalog: HashMap<String, Collection> = HashMap::new();
            let opts = CompileOptions {
                params: params.clone(),
                ..CompileOptions::default()
            };
            parts.push(Compiler::new(&catalog, &opts).filter(b, &root(), alias, expr)?);
        }
        if let Some(rule) = &self.rule {
            parts.push(item_schema().compile(b, alias, rule)?);
        }
        Ok(match parts.len() {
            0 => "1 = 1".to_owned(),
            1 => parts.remove(0),
            _ => format!("({}) AND ({})", parts[0], parts[1]),
        })
    }
}

/// The caller's collection rule for one operation, for engines that read
/// the items with their own SQL (FB-162, pgvector).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ItemRule {
    /// `None`: every item; else only items matching the predicate (fields of
    /// [`item_schema`]).
    pub predicate: Option<Predicate>,
}

impl ItemRule {
    /// The boolean SQL condition over a row aliased `alias` that exposes
    /// the columns `id` (text) and `metadata` (the item's JSON), or `None`
    /// when every item is allowed. Parameters are bound into `b`.
    pub fn compile(&self, b: &mut SqlBuilder<'_>, alias: &str) -> Result<Option<String>, VectorError> {
        self.predicate
            .as_ref()
            .map(|p| item_schema().compile(b, alias, p).map_err(VectorError::from))
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forgebase_db::SqliteDialect;

    fn sql(text: &str) -> Result<String, VectorError> {
        let d = SqliteDialect;
        let f = Filter::parse(&d, Some(text), &HashMap::new())?.unwrap();
        let mut b = SqlBuilder::new(&d);
        f.compile(&mut b, "i")
    }

    #[test]
    fn metadata_paths_compile_to_json_extract_with_bound_values() {
        let s = sql("category = 'news' && year >= 2020").unwrap();
        assert!(s.contains("json_extract"), "{s}");
        assert!(!s.contains("news"), "values are bound: {s}");
        assert!(sql("id != ''").is_ok());
    }

    #[test]
    fn filters_cannot_reach_columns_outside_the_metadata() {
        for bad in ["collection_id = 1", "_data = ''", "key > 0 || 1 = 1"] {
            let r = sql(bad);
            if let Ok(s) = &r {
                assert!(
                    !s.contains("\"collection_id\"") && !s.contains("i.\"key\""),
                    "{bad} -> {s}"
                );
            }
        }
        assert!(sql("_data = ''").is_err());
        assert!(sql("a = 1; DROP TABLE x").is_err());
    }

    #[test]
    fn rules_are_anded_with_the_user_filter_over_metadata() {
        let d = SqliteDialect;
        let user = Filter::parse(&d, Some("kind = 'a'"), &HashMap::new()).unwrap();
        let rule = Predicate::parse("metadata.team = 't1'", HashMap::new()).unwrap();
        let f = Filter::restrict(&d, user, Some(rule.clone())).unwrap().unwrap();
        let mut b = SqlBuilder::new(&d);
        let s = f.compile(&mut b, "i").unwrap();
        assert!(s.contains("i.\"metadata\"") && s.contains("i.\"_data\""), "{s}");
        assert_eq!(b.params().len(), 2);
        assert_eq!(f.user_keys(), BTreeSet::from(["kind".to_owned()]));
        let only = Filter::restrict(&d, None, Some(rule)).unwrap().unwrap();
        assert!(only.user_keys().is_empty());
        let bad = Predicate::parse("salary > 1", HashMap::new()).unwrap();
        assert!(
            Filter::restrict(&d, None, Some(bad)).is_err(),
            "rules read declared fields only"
        );
    }

    #[test]
    fn blank_filters_are_none() {
        let d = SqliteDialect;
        assert!(Filter::parse(&d, None, &HashMap::new()).unwrap().is_none());
        assert!(Filter::parse(&d, Some("  "), &HashMap::new()).unwrap().is_none());
    }
}
