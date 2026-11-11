//! In-memory snapshot of collection definitions.

use std::collections::HashMap;

use forgebase_db::{Backend, Statement, Value, WriteTx};
use forgebase_schema::Collection;

use crate::DataError;

/// Collection definitions by id, with a case-insensitive name index.
#[derive(Debug, Default, Clone)]
pub(crate) struct CatalogSnapshot {
    by_id: HashMap<String, Collection>,
    by_name: HashMap<String, String>,
}

const SELECT_DEFINITIONS: &str = "SELECT definition FROM _forgebase_collections ORDER BY name";

impl CatalogSnapshot {
    pub(crate) async fn load(backend: &dyn Backend) -> Result<Self, DataError> {
        let rows = backend.fetch_all(&Statement::new(SELECT_DEFINITIONS, vec![])).await?;
        Self::from_rows(rows.rows)
    }

    /// Loads the catalog as seen inside a write transaction.
    pub(crate) async fn load_tx(tx: &mut dyn WriteTx) -> Result<Self, DataError> {
        let rows = tx.fetch_all(&Statement::new(SELECT_DEFINITIONS, vec![])).await?;
        Self::from_rows(rows.rows)
    }

    fn from_rows(rows: Vec<Vec<Value>>) -> Result<Self, DataError> {
        let mut s = Self::default();
        for row in rows {
            let text = row.first().and_then(Value::as_str).unwrap_or("{}");
            let c: Collection = serde_json::from_str(text)
                .map_err(|e| DataError::Internal(format!("collection definition does not decode: {e}")))?;
            s.insert(c);
        }
        Ok(s)
    }

    pub(crate) fn insert(&mut self, c: Collection) {
        self.by_name.insert(c.name.to_ascii_lowercase(), c.id.clone());
        self.by_id.insert(c.id.clone(), c);
    }

    /// By id, or by name (case-insensitive).
    pub(crate) fn get(&self, name_or_id: &str) -> Option<&Collection> {
        self.by_id.get(name_or_id).or_else(|| {
            self.by_name
                .get(&name_or_id.to_ascii_lowercase())
                .and_then(|id| self.by_id.get(id))
        })
    }

    /// All collections, ordered by name.
    pub(crate) fn all(&self) -> Vec<&Collection> {
        let mut v: Vec<&Collection> = self.by_id.values().collect();
        v.sort_by_key(|c| c.name.to_ascii_lowercase());
        v
    }
}

impl forgebase_query::Catalog for CatalogSnapshot {
    fn collection_by_id(&self, id: &str) -> Option<&Collection> {
        self.by_id.get(id)
    }
}

/// PostgreSQL catalog introspection for collection objects (ADR-0028 §7.4).
///
/// Everything here reads `pg_catalog` for objects in schema `public` (named
/// by a `public."name"` text that `to_regclass` parses), never through the
/// search path, where system objects in schema `forgebase` come first.
/// Results use the column names of the SQLite statements they replace.
pub(crate) mod pg {
    use forgebase_db::{Statement, Value, WriteTx};

    use crate::DataError;

    /// Columns of a relation: `cid, name, type, notnull, dflt_value, pk`
    /// (as `PRAGMA table_info`), in column order.
    pub(crate) fn columns(regclass: &str) -> Statement {
        Statement::new(
            "SELECT a.attnum - 1 AS cid, a.attname::text AS name, format_type(a.atttypid, a.atttypmod) AS type, \
             a.attnotnull::int AS notnull, pg_get_expr(d.adbin, d.adrelid) AS dflt_value, \
             coalesce((SELECT k.ord FROM unnest(pk.indkey::int2[]) WITH ORDINALITY k(attnum, ord) WHERE k.attnum = a.attnum), 0)::bigint AS pk \
             FROM pg_catalog.pg_attribute a \
             LEFT JOIN pg_catalog.pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
             LEFT JOIN pg_catalog.pg_index pk ON pk.indrelid = a.attrelid AND pk.indisprimary \
             WHERE a.attrelid = to_regclass(?1) AND a.attnum > 0 AND NOT a.attisdropped \
             ORDER BY a.attnum",
            vec![regclass.into()],
        )
    }

    /// A view that depends on a table (directly or through other views).
    #[derive(Debug, Clone)]
    pub(crate) struct DependentView {
        /// `schema."name"`, ready for DDL.
        pub(crate) qualified: String,
        /// Bare name.
        pub(crate) name: String,
        /// `pg_get_viewdef`, without the trailing semicolon.
        pub(crate) query: String,
        /// `reloptions` (`security_invoker=true`), comma-separated.
        pub(crate) options: Option<String>,
    }

    impl DependentView {
        /// `CREATE VIEW` restoring the view as it was.
        pub(crate) fn create_sql(&self) -> String {
            match self.options.as_deref().filter(|o| !o.is_empty()) {
                Some(o) => format!("CREATE VIEW {} WITH ({o}) AS {}", self.qualified, self.query),
                None => format!("CREATE VIEW {} AS {}", self.qualified, self.query),
            }
        }
    }

    /// Views reading `regclass` (any schema), with the views reading them,
    /// in creation order (a view after every view it reads). With `columns`,
    /// only views that read one of those columns start the walk.
    pub(crate) async fn dependent_views(
        tx: &mut dyn WriteTx,
        regclass: &str,
        columns: Option<&[String]>,
    ) -> Result<Vec<DependentView>, DataError> {
        let cols: Value = match columns {
            Some(c) => Value::Text(serde_json::to_string(c)?),
            None => Value::Null,
        };
        let rows = tx
            .fetch_all(&Statement::new(
                "WITH RECURSIVE deps(oid, depth) AS ( \
                   SELECT r.ev_class, 1 FROM pg_catalog.pg_depend dep \
                   JOIN pg_catalog.pg_rewrite r ON r.oid = dep.objid \
                   WHERE dep.classid = 'pg_catalog.pg_rewrite'::regclass AND dep.refclassid = 'pg_catalog.pg_class'::regclass \
                     AND dep.refobjid = to_regclass(?1) AND r.ev_class <> dep.refobjid \
                     AND (CAST(?2 AS jsonb) IS NULL OR dep.refobjsubid IN ( \
                       SELECT a.attnum FROM pg_catalog.pg_attribute a WHERE a.attrelid = to_regclass(?1) \
                         AND a.attname::text IN (SELECT jsonb_array_elements_text(CAST(?2 AS jsonb))))) \
                   UNION \
                   SELECT r.ev_class, deps.depth + 1 FROM deps \
                   JOIN pg_catalog.pg_depend dep ON dep.refobjid = deps.oid \
                     AND dep.refclassid = 'pg_catalog.pg_class'::regclass AND dep.classid = 'pg_catalog.pg_rewrite'::regclass \
                   JOIN pg_catalog.pg_rewrite r ON r.oid = dep.objid \
                   WHERE r.ev_class <> deps.oid AND deps.depth < 64 \
                 ) \
                 SELECT quote_ident(n.nspname) || '.' || quote_ident(c.relname), c.relname::text, \
                   pg_get_viewdef(c.oid), array_to_string(c.reloptions, ', '), max(deps.depth) AS depth \
                 FROM deps JOIN pg_catalog.pg_class c ON c.oid = deps.oid \
                 JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
                 WHERE c.relkind = 'v' \
                 GROUP BY n.nspname, c.relname, c.oid, c.reloptions ORDER BY depth, c.relname",
                vec![regclass.into(), cols],
            ))
            .await?;
        Ok(rows
            .rows
            .iter()
            .map(|r| {
                let text = |i: usize| r.get(i).and_then(Value::as_str).unwrap_or_default().to_owned();
                DependentView {
                    qualified: text(0),
                    name: text(1),
                    query: text(2).trim().trim_end_matches(';').to_owned(),
                    options: r.get(3).and_then(Value::as_str).map(str::to_owned),
                }
            })
            .collect())
    }

    /// Names of the `CHECK` and `FOREIGN KEY` constraints on exactly one
    /// column of `regclass`.
    pub(crate) async fn column_constraints(
        tx: &mut dyn WriteTx,
        regclass: &str,
        column: &str,
    ) -> Result<Vec<String>, DataError> {
        let rows = tx
            .fetch_all(&Statement::new(
                "SELECT con.conname::text FROM pg_catalog.pg_constraint con \
                 JOIN pg_catalog.pg_attribute a ON a.attrelid = con.conrelid AND a.attnum = con.conkey[1] \
                 WHERE con.conrelid = to_regclass(?1) AND con.contype IN ('c', 'f') \
                   AND cardinality(con.conkey) = 1 AND a.attname::text = ?2 ORDER BY 1",
                vec![regclass.into(), column.into()],
            ))
            .await?;
        Ok(rows
            .rows
            .iter()
            .filter_map(|r| r.first().and_then(Value::as_str).map(str::to_owned))
            .collect())
    }

    /// Names of the indexes on tables in schema `public` (one table, matched
    /// ignoring case, when `table` is given), except ForgeBase's own derived
    /// objects (`_forgebase_…`, the full-text indexes).
    pub(crate) fn index_names(table: Option<&str>) -> Statement {
        Statement::new(
            "SELECT c.relname::text FROM pg_catalog.pg_index i \
             JOIN pg_catalog.pg_class c ON c.oid = i.indexrelid \
             JOIN pg_catalog.pg_class t ON t.oid = i.indrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = 'public' AND (CAST(?1 AS text) IS NULL OR lower(t.relname::text) = lower(?1)) \
               AND c.relname::text NOT LIKE '\\_forgebase\\_%' ESCAPE '\\'",
            vec![table.into()],
        )
    }

    /// One index in schema `public`: `tbl, sql, uniq, partial, engine (0/1:
    /// primary key or unique constraint), cols (JSON array, `<expr>` for
    /// expression keys), est (rows, NULL before statistics), size (bytes),
    /// scans (`pg_stat_user_indexes.idx_scan`)`.
    pub(crate) fn index_row(name: &str) -> Statement {
        Statement::new(
            "SELECT t.relname::text AS tbl, pg_get_indexdef(i.indexrelid) AS sql, i.indisunique::int AS uniq, \
               (i.indpred IS NOT NULL)::int AS partial, \
               (i.indisprimary OR EXISTS (SELECT 1 FROM pg_catalog.pg_constraint con \
                  WHERE con.conindid = i.indexrelid AND con.conrelid = i.indrelid AND con.contype IN ('p', 'u')))::int AS engine, \
               (SELECT json_agg(CASE WHEN k.attnum = 0 THEN '<expr>' ELSE a.attname::text END ORDER BY k.ord)::text \
                  FROM unnest(i.indkey::int2[]) WITH ORDINALITY k(attnum, ord) \
                  LEFT JOIN pg_catalog.pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = k.attnum \
                  WHERE k.ord <= i.indnkeyatts) AS cols, \
               CASE WHEN c.reltuples >= 0 THEN c.reltuples::bigint END AS est, \
               pg_relation_size(c.oid) AS size, \
               (SELECT s.idx_scan FROM pg_catalog.pg_stat_user_indexes s WHERE s.indexrelid = i.indexrelid) AS scans \
             FROM pg_catalog.pg_index i \
             JOIN pg_catalog.pg_class c ON c.oid = i.indexrelid \
             JOIN pg_catalog.pg_class t ON t.oid = i.indrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = 'public' AND c.relname::text = ?1",
            vec![name.into()],
        )
    }

    /// Bytes used by a relation in schema `public` (NULL if it is missing).
    pub(crate) fn relation_size(regclass: &str) -> Statement {
        Statement::new("SELECT pg_total_relation_size(to_regclass(?1))", vec![regclass.into()])
    }
}
