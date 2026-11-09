//! Metadata filters: the data core's filter language (ADR-0003 §5) compiled
//! against the items table.
//!
//! The items table uses the data core's column names (`id`, `created`,
//! `updated`, `_version`, `_data`), so it is described to the compiler as a
//! flexible collection without declared fields: `category = 'news'`
//! compiles to `json_extract(i."_data", '$.category') = ?`. Paths that start
//! with `_` (other than `_version`) are refused by the compiler, so a filter
//! can never name `collection_id`, `key` or anything outside the metadata.
//! Every value is a bound parameter.

use std::collections::HashMap;

use forgebase_data::query::{CompileOptions, Compiler, Expr, SqlBuilder, parse_filter};
use forgebase_data::schema::{Collection, SchemaMode};
use forgebase_db::Dialect;
use serde_json::Value as Json;

use crate::error::VectorError;

/// A parsed, validated filter with its parameters.
#[derive(Debug, Clone)]
pub(crate) struct Filter {
    expr: Expr,
    params: HashMap<String, Json>,
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
            expr,
            params: params.clone(),
        };
        let mut b = SqlBuilder::new(dialect);
        f.compile(&mut b, "i")?;
        Ok(Some(f))
    }

    /// The boolean SQL expression for rows of the items table aliased
    /// `alias`; parameters go to `b`.
    pub(crate) fn compile(&self, b: &mut SqlBuilder<'_>, alias: &str) -> Result<String, VectorError> {
        let catalog: HashMap<String, Collection> = HashMap::new();
        let opts = CompileOptions {
            params: self.params.clone(),
            ..CompileOptions::default()
        };
        Ok(Compiler::new(&catalog, &opts).filter(b, &root(), alias, &self.expr)?)
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
    fn blank_filters_are_none() {
        let d = SqliteDialect;
        assert!(Filter::parse(&d, None, &HashMap::new()).unwrap().is_none());
        assert!(Filter::parse(&d, Some("  "), &HashMap::new()).unwrap().is_none());
    }
}
