//! Index manager (ADR-0003 §8): definitions, DDL, inspection, rebuild,
//! statistics, query plans, slow queries and recommendations.
//!
//! Indexes use the engine's native structures (SPEC §4.5): B-tree indexes
//! (single, composite, unique, partial, expression, JSON-path) and full-text
//! indexes. On SQLite a full-text index is an FTS5 table kept in sync by
//! triggers; on PostgreSQL it is a GIN index over
//! `to_tsvector('simple', _forgebase_unaccent(…))` of the indexed fields
//! (an expression index, so no column is added to the table and nothing
//! needs syncing), queried with `websearch_to_tsquery` (ADR-0028 §7.4, FB-144
//! addendum). Statistics come from `sqlite_stat1` / `pg_class` and
//! `pg_stat_user_indexes`, sizes from `dbstat` / `pg_relation_size`.

use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

use forgebase_db::{Dialect, PG_UNACCENT_FN, PlanStep, SlowQuery, Statement, TxMode, Value, WriteTx};
use forgebase_query::{CompileOptions, Compiler, FieldPath, SqlBuilder, parse_filter};
use forgebase_schema::{Collection, FieldKind};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::access::{TENANT_COLUMN, is_tenant_scoped};
use crate::catalog::CatalogSnapshot;
use crate::ddl::{DdlLog, is_pg, pg_regclass, record_history, rel};
use crate::{AccessContext, Action, DataError, DataService, ListQuery, finish};

/// Prefix of FTS5 tables and their triggers (SQLite) and of full-text GIN
/// indexes (PostgreSQL).
pub(crate) const FTS_PREFIX: &str = "_forgebase_fts_";

/// Index kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexKind {
    /// B-tree index (single, composite, unique, partial, expression, JSON path).
    #[default]
    Btree,
    /// Full-text index (FTS5 on SQLite, GIN over a `tsvector` on PostgreSQL).
    Fulltext,
}

impl IndexKind {
    fn as_str(self) -> &'static str {
        match self {
            IndexKind::Btree => "btree",
            IndexKind::Fulltext => "fulltext",
        }
    }
}

/// One key of an index: a field path (`price`, `metadata.brand`,
/// `title:lower`) and its direction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexKey {
    /// Field path in filter syntax.
    pub path: String,
    /// Descending.
    #[serde(default)]
    pub desc: bool,
}

impl IndexKey {
    /// An ascending key.
    pub fn asc(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            desc: false,
        }
    }
}

/// An index to create.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexDefinition {
    /// Index name (generated when empty).
    #[serde(default)]
    pub name: String,
    /// Collection name or id.
    pub collection: String,
    /// Keys, in order. For full-text indexes, the fields to index.
    pub keys: Vec<IndexKey>,
    /// Unique index.
    #[serde(default)]
    pub unique: bool,
    /// Partial index condition, in filter syntax.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partial: Option<String>,
    /// Kind.
    #[serde(default)]
    pub kind: IndexKind,
}

impl IndexDefinition {
    /// A B-tree index on `keys` (ascending).
    pub fn btree(collection: impl Into<String>, keys: &[&str]) -> Self {
        Self {
            name: String::new(),
            collection: collection.into(),
            keys: keys.iter().map(|k| IndexKey::asc(*k)).collect(),
            unique: false,
            partial: None,
            kind: IndexKind::Btree,
        }
    }
}

/// Where an index comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexOrigin {
    /// Derived from the schema (unique fields, relations).
    Auto,
    /// Created through the index manager.
    Manual,
    /// Created with raw SQL (not managed; kept across table rebuilds).
    Sql,
    /// Created by the engine for a PRIMARY KEY or UNIQUE constraint.
    Engine,
}

/// Statistics of one index.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct IndexStats {
    /// Planner statistics (`sqlite_stat1.stat`: rows, then average rows per
    /// key prefix), present after `analyze`.
    pub stat: Option<String>,
    /// Estimated rows in the index (first number of `stat`).
    pub rows_estimate: Option<i64>,
    /// Size on disk in bytes.
    pub size_bytes: Option<i64>,
    /// Index scans since the statistics were last reset
    /// (`pg_stat_user_indexes.idx_scan`; PostgreSQL only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scans: Option<i64>,
}

/// An index as found in the database.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IndexInfo {
    /// Name.
    pub name: String,
    /// Table.
    pub table: String,
    /// Collection, if the table belongs to one.
    pub collection: Option<String>,
    /// Origin.
    pub origin: IndexOrigin,
    /// Kind.
    pub kind: IndexKind,
    /// Unique.
    pub unique: bool,
    /// Partial (has a WHERE clause).
    pub partial: bool,
    /// Indexed columns (`<expr>` for expression keys).
    pub columns: Vec<String>,
    /// Managed definition, if any.
    pub definition: Option<IndexDefinition>,
    /// Engine DDL.
    pub sql: Option<String>,
    /// Statistics.
    pub stats: IndexStats,
}

/// A query plan.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QueryPlan {
    /// The SQL that was planned (parameters are not shown).
    pub sql: String,
    /// Plan steps.
    pub steps: Vec<PlanStep>,
    /// Indexes the plan uses.
    pub indexes_used: Vec<String>,
    /// Tables (or aliases) read in full (`SCAN`, with or without an index).
    pub full_scans: Vec<String>,
}

impl QueryPlan {
    /// Reads SQLite's `EXPLAIN QUERY PLAN` details (`SEARCH t USING INDEX i`,
    /// `SCAN t`).
    pub(crate) fn from_steps(sql: String, steps: Vec<PlanStep>) -> Self {
        let mut indexes_used = Vec::new();
        let mut full_scans = Vec::new();
        for s in &steps {
            let d = &s.detail;
            for marker in ["USING COVERING INDEX ", "USING INDEX "] {
                if let Some(rest) = d.split(marker).nth(1) {
                    let name = rest.split([' ', '(']).next().unwrap_or_default().to_owned();
                    if !name.is_empty() && !indexes_used.contains(&name) {
                        indexes_used.push(name);
                    }
                    break;
                }
            }
            // `SCAN x` reads every row, even `SCAN x USING INDEX i` (a full
            // walk in index order); only `SEARCH` is a lookup.
            if let Some(rest) = d.strip_prefix("SCAN ")
                && !rest.starts_with("CONSTANT")
            {
                let table = rest.split(' ').next().unwrap_or_default().to_owned();
                if !full_scans.contains(&table) {
                    full_scans.push(table);
                }
            }
        }
        Self {
            sql,
            steps,
            indexes_used,
            full_scans,
        }
    }

    /// Reads a PostgreSQL `EXPLAIN (FORMAT JSON)` plan (its `Plan` node).
    ///
    /// A full scan is a `Seq Scan`, or an `Index Scan` / `Index Only Scan`
    /// with no `Index Cond`: a walk of the whole table in index order (what
    /// SQLite calls `SCAN t USING INDEX i`). Steps keep `forgebase-db`'s
    /// wording, with the filter shown apart from the index condition.
    pub(crate) fn from_pg_json(sql: String, plan: Option<&Json>) -> Self {
        fn walk(node: &Json, parent: i64, steps: &mut Vec<PlanStep>, used: &mut Vec<String>, scans: &mut Vec<String>) {
            let id = i64::try_from(steps.len()).unwrap_or(i64::MAX) + 1;
            let field = |k: &str| node.get(k).and_then(Json::as_str);
            let kind = field("Node Type").unwrap_or("?");
            let mut detail = kind.to_owned();
            if let Some(op) = field("Operation") {
                detail.push_str(&format!(" ({op})"));
            }
            if let Some(index) = field("Index Name") {
                detail.push_str(&format!(" using {index}"));
                if !used.iter().any(|u| u == index) {
                    used.push(index.to_owned());
                }
            }
            if let Some(rel) = field("Relation Name") {
                detail.push_str(&format!(" on {rel}"));
                let alias = field("Alias").unwrap_or(rel);
                if alias != rel {
                    detail.push_str(&format!(" {alias}"));
                }
                let whole = kind == "Seq Scan"
                    || (matches!(kind, "Index Scan" | "Index Only Scan") && field("Index Cond").is_none());
                if whole && !scans.iter().any(|s| s == alias) {
                    scans.push(alias.to_owned());
                }
            }
            if let Some(cond) = field("Index Cond") {
                detail.push_str(&format!(" ({cond})"));
            }
            if let Some(filter) = field("Filter") {
                detail.push_str(&format!(" filter {filter}"));
            }
            steps.push(PlanStep { id, parent, detail });
            for child in node.get("Plans").and_then(Json::as_array).into_iter().flatten() {
                walk(child, id, steps, used, scans);
            }
        }
        let (mut steps, mut indexes_used, mut full_scans) = (Vec::new(), Vec::new(), Vec::new());
        if let Some(p) = plan {
            walk(p, 0, &mut steps, &mut indexes_used, &mut full_scans);
        }
        Self {
            sql,
            steps,
            indexes_used,
            full_scans,
        }
    }
}

/// A suggested index derived from slow queries.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IndexRecommendation {
    /// The index to create.
    pub definition: IndexDefinition,
    /// Why.
    pub reason: String,
    /// Slow executions this index would help.
    pub occurrences: usize,
    /// Slowest of them, in milliseconds.
    pub max_duration_ms: f64,
}

fn validate_index_name(name: &str) -> Result<(), DataError> {
    let ok = name.len() <= forgebase_schema::POSTGRES_MAX_IDENT
        && name.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    let lower = name.to_ascii_lowercase();
    if !ok || ["auto_", "sqlite_", "pg_"].iter().any(|p| lower.starts_with(p)) {
        return Err(DataError::invalid(
            "invalid_index_name",
            "index names use letters, digits and underscores (at most 63), start with a letter, and may not start with auto_, sqlite_ or pg_",
        ));
    }
    Ok(())
}

/// Name of a collection's `_tenant` index. Derived names are fitted to the
/// engine's identifier limit (63 bytes on PostgreSQL: a longer name gets a
/// stable hash suffix, see [`Dialect::fit_ident`]).
pub(crate) fn tenant_index_name(d: &dyn Dialect, coll: &Collection) -> String {
    d.fit_ident(&format!("auto_{}_tenant", coll.id))
}

/// The automatic indexes of a collection: a unique index per unique field and
/// an index per single relation field (fast joins and cascades). Names are
/// fitted to the identifier limit.
pub(crate) fn auto_indexes(d: &dyn Dialect, coll: &Collection) -> Vec<IndexDefinition> {
    let mut out = Vec::new();
    if coll.is_view() {
        return out;
    }
    for f in &coll.fields {
        let single_relation = matches!(f.kind, FieldKind::Relation { max_select: 1, .. });
        if f.unique || single_relation {
            out.push(IndexDefinition {
                name: d.fit_ident(&format!("auto_{}_{}_{}", coll.id, f.id, if f.unique { "u" } else { "r" })),
                collection: coll.name.clone(),
                keys: vec![IndexKey::asc(f.name.clone())],
                unique: f.unique,
                partial: None,
                kind: IndexKind::Btree,
            });
        }
    }
    out
}

/// Compiles key expressions of a definition against a row alias ("" for
/// unqualified column names, `new` / `old` in triggers).
fn key_exprs(
    d: &dyn Dialect,
    cat: &CatalogSnapshot,
    coll: &Collection,
    def: &IndexDefinition,
    alias: &str,
) -> Result<Vec<String>, DataError> {
    if def.keys.is_empty() || def.keys.len() > 16 {
        return Err(DataError::invalid("invalid_index", "an index needs 1 to 16 keys"));
    }
    let opts = CompileOptions {
        allow_hidden: true,
        inline_literals: true,
        ..CompileOptions::default()
    };
    let compiler = Compiler::new(cat, &opts);
    let mut b = SqlBuilder::new(d);
    def.keys
        .iter()
        .map(|k| {
            let path = FieldPath::parse(&k.path)?;
            Ok(compiler.scalar(&mut b, coll, alias, &path)?)
        })
        .collect()
}

/// Engine object of full-text index `name`: the FTS5 table on SQLite, the
/// GIN index on PostgreSQL (fitted to 63 bytes: index names may use all 63).
pub(crate) fn fts_table(d: &dyn Dialect, name: &str) -> String {
    d.fit_ident(&format!("{FTS_PREFIX}{name}"))
}

/// PostgreSQL full-text document of a definition over row `alias`:
/// `to_tsvector('simple', _forgebase_unaccent(coalesce(k1::text, '') || ' ' || …))`.
/// The index and the query render it the same way, so the planner matches
/// the expression index.
fn pg_fts_document(
    d: &dyn Dialect,
    cat: &CatalogSnapshot,
    coll: &Collection,
    def: &IndexDefinition,
    alias: &str,
) -> Result<String, DataError> {
    let parts: Vec<String> = key_exprs(d, cat, coll, def, alias)?
        .iter()
        .map(|e| format!("coalesce(({e})::text, '')"))
        .collect();
    Ok(format!(
        "to_tsvector('simple', {PG_UNACCENT_FN}({}))",
        parts.join(" || ' ' || ")
    ))
}

/// Condition: row `alias` of `coll` matches the full-text query bound to
/// `param`, through index `def` (FTS5 `MATCH` on SQLite, `@@` on the
/// indexed `tsvector` expression on PostgreSQL).
pub(crate) fn fulltext_match(
    d: &dyn Dialect,
    cat: &CatalogSnapshot,
    coll: &Collection,
    def: &IndexDefinition,
    alias: &str,
    param: &str,
) -> Result<String, DataError> {
    if is_pg(d) {
        Ok(format!(
            "{} @@ websearch_to_tsquery('simple', {PG_UNACCENT_FN}({param}))",
            pg_fts_document(d, cat, coll, def, alias)?
        ))
    } else {
        Ok(d.fulltext_match(alias, &fts_table(d, &def.name), param))
    }
}

/// DDL creating an index (and, for full-text, its triggers and initial
/// content).
pub(crate) fn create_index_sql(
    d: &dyn Dialect,
    cat: &CatalogSnapshot,
    coll: &Collection,
    def: &IndexDefinition,
) -> Result<Vec<String>, DataError> {
    let q = |s: &str| d.quote_ident(s);
    let table = rel(d, &coll.name);
    let pg = is_pg(d);
    match def.kind {
        IndexKind::Btree => {
            let exprs = key_exprs(d, cat, coll, def, "")?;
            let keys: Vec<String> = exprs
                .iter()
                .zip(&def.keys)
                .map(|(e, k)| {
                    // PostgreSQL takes an expression key in parentheses (a
                    // parenthesised column is still a plain column key).
                    let e = if pg { format!("({e})") } else { e.clone() };
                    format!("{e}{}", if k.desc { " DESC" } else { "" })
                })
                .collect();
            let mut keys = keys;
            if def.unique && is_tenant_scoped(coll) {
                // Uniqueness holds per tenant: other tenants' values neither
                // collide nor leak through "already in use".
                keys.insert(0, q(TENANT_COLUMN));
            }
            let mut sql = format!(
                "CREATE {}INDEX {} ON {table} ({})",
                if def.unique { "UNIQUE " } else { "" },
                q(&def.name),
                keys.join(", ")
            );
            if let Some(p) = def.partial.as_deref().filter(|p| !p.trim().is_empty()) {
                let expr =
                    parse_filter(p)?.ok_or_else(|| DataError::invalid("invalid_index", "empty partial condition"))?;
                let opts = CompileOptions {
                    allow_hidden: true,
                    inline_literals: true,
                    ..CompileOptions::default()
                };
                let mut b = SqlBuilder::new(d);
                let cond = Compiler::new(cat, &opts).filter(&mut b, coll, "", &expr)?;
                sql.push_str(&format!(" WHERE {cond}"));
            }
            Ok(vec![sql])
        }
        IndexKind::Fulltext => {
            if def.unique || def.partial.is_some() {
                return Err(DataError::invalid(
                    "invalid_index",
                    "full-text indexes cannot be unique or partial",
                ));
            }
            if pg {
                return Ok(vec![format!(
                    "CREATE INDEX {} ON {table} USING gin (({}))",
                    q(&fts_table(d, &def.name)),
                    pg_fts_document(d, cat, coll, def, "")?
                )]);
            }
            let fts = q(&fts_table(d, &def.name));
            let cols: Vec<String> = (0..def.keys.len()).map(|i| format!("c{i}")).collect();
            let plain = key_exprs(d, cat, coll, def, "")?;
            let mut out = vec![format!(
                "CREATE VIRTUAL TABLE {fts} USING fts5({}, content='', contentless_delete=1, tokenize='unicode61 remove_diacritics 2')",
                cols.join(", ")
            )];
            out.extend(fts_trigger_sql(d, cat, coll, def)?);
            out.push(format!(
                "INSERT INTO {fts} (rowid, {}) SELECT rowid, {} FROM {table}",
                cols.join(", "),
                plain.join(", ")
            ));
            Ok(out)
        }
    }
}

/// Triggers that keep a full-text index in sync with its table (SQLite;
/// none on PostgreSQL, where the index is an expression index).
pub(crate) fn fts_trigger_sql(
    d: &dyn Dialect,
    cat: &CatalogSnapshot,
    coll: &Collection,
    def: &IndexDefinition,
) -> Result<Vec<String>, DataError> {
    if is_pg(d) {
        return Ok(Vec::new());
    }
    let q = |s: &str| d.quote_ident(s);
    let table = q(&coll.name);
    let fts_name = fts_table(d, &def.name);
    let fts = q(&fts_name);
    let cols: Vec<String> = (0..def.keys.len()).map(|i| format!("c{i}")).collect();
    let new_vals = key_exprs(d, cat, coll, def, "new")?;
    let insert = format!(
        "INSERT INTO {fts} (rowid, {}) VALUES (new.rowid, {});",
        cols.join(", "),
        new_vals.join(", ")
    );
    let delete = format!("DELETE FROM {fts} WHERE rowid = old.rowid;");
    Ok(vec![
        format!(
            "CREATE TRIGGER {} AFTER INSERT ON {table} BEGIN {insert} END",
            q(&format!("{fts_name}_ai"))
        ),
        format!(
            "CREATE TRIGGER {} AFTER DELETE ON {table} BEGIN {delete} END",
            q(&format!("{fts_name}_ad"))
        ),
        format!(
            "CREATE TRIGGER {} AFTER UPDATE ON {table} BEGIN {delete} {insert} END",
            q(&format!("{fts_name}_au"))
        ),
    ])
}

/// DDL dropping an index (and its full-text table). On PostgreSQL the index
/// is named in schema `public`, so a system index of the same name in schema
/// `forgebase` (first on the search path) is never touched.
pub(crate) fn drop_index_sql(d: &dyn Dialect, def: &IndexDefinition) -> Vec<String> {
    let q = |s: &str| d.quote_ident(s);
    match def.kind {
        IndexKind::Btree => vec![format!("DROP INDEX IF EXISTS {}", rel(d, &def.name))],
        IndexKind::Fulltext if is_pg(d) => {
            vec![format!("DROP INDEX IF EXISTS {}", rel(d, &fts_table(d, &def.name)))]
        }
        IndexKind::Fulltext => {
            let fts = fts_table(d, &def.name);
            vec![
                format!("DROP TRIGGER IF EXISTS {}", q(&format!("{fts}_ai"))),
                format!("DROP TRIGGER IF EXISTS {}", q(&format!("{fts}_ad"))),
                format!("DROP TRIGGER IF EXISTS {}", q(&format!("{fts}_au"))),
                format!("DROP TABLE IF EXISTS {}", q(&fts)),
            ]
        }
    }
}

/// Managed index rows of a collection.
pub(crate) async fn managed_indexes(
    tx: &mut dyn WriteTx,
    collection_id: &str,
) -> Result<Vec<(IndexDefinition, String)>, DataError> {
    let rows = tx
        .fetch_all(&Statement::new(
            "SELECT definition, origin FROM _forgebase_indexes WHERE collection_id = ?1 ORDER BY name",
            vec![collection_id.into()],
        ))
        .await?;
    rows.rows
        .iter()
        .map(|r| {
            let def: IndexDefinition = serde_json::from_str(r[0].as_str().unwrap_or("{}"))?;
            Ok((def, r[1].as_str().unwrap_or("manual").to_owned()))
        })
        .collect()
}

/// Inserts a managed index row.
pub(crate) async fn insert_index_row(
    log: &mut DdlLog<'_>,
    coll: &Collection,
    def: &IndexDefinition,
    origin: &str,
) -> Result<(), DataError> {
    log.exec(
        "INSERT INTO _forgebase_indexes (name, collection_id, origin, kind, definition, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        vec![
            def.name.clone().into(),
            coll.id.clone().into(),
            origin.into(),
            def.kind.as_str().into(),
            serde_json::to_string(def)?.into(),
            forgebase_schema::now_timestamp().into(),
        ],
    )
    .await?;
    Ok(())
}

/// Rewrites field names in a definition after field renames.
pub(crate) fn rename_fields(def: &mut IndexDefinition, renames: &BTreeMap<String, String>) -> Result<(), DataError> {
    let rename = |p: &mut FieldPath| {
        if let Some(forgebase_query::PathSeg::Name(n)) = p.segments.first_mut()
            && let Some(new) = renames.get(&n.to_ascii_lowercase())
        {
            *n = new.clone();
        }
    };
    for k in &mut def.keys {
        let mut p = FieldPath::parse(&k.path)?;
        rename(&mut p);
        k.path = p.to_string();
    }
    if let Some(partial) = &def.partial
        && let Some(mut e) = parse_filter(partial)?
    {
        e.for_each_path_mut(&mut |p| rename(p));
        def.partial = Some(e.to_string());
    }
    Ok(())
}

/// Root field names a definition uses.
pub(crate) fn fields_used(def: &IndexDefinition) -> Result<Vec<String>, DataError> {
    let mut out = Vec::new();
    for k in &def.keys {
        out.push(FieldPath::parse(&k.path)?.root().to_ascii_lowercase());
    }
    if let Some(p) = &def.partial
        && let Some(mut e) = parse_filter(p)?
    {
        e.for_each_path_mut(&mut |p| out.push(p.root().to_ascii_lowercase()));
    }
    Ok(out)
}

impl DataService {
    /// Creates an index. Fails (and changes nothing) if, for example, a unique
    /// index meets duplicate values.
    pub async fn create_index(&self, ctx: &AccessContext, mut def: IndexDefinition) -> Result<IndexInfo, DataError> {
        let _guard = self.inner.schema_lock.lock().await;
        let (coll, _) = self.resolve(ctx, Action::ManageIndexes, &def.collection)?;
        if coll.is_view() {
            return Err(DataError::invalid(
                "invalid_index",
                "view collections cannot be indexed",
            ));
        }
        if def.name.is_empty() {
            let keys: Vec<String> = def.keys.iter().map(|k| k.path.replace(['.', ':'], "_")).collect();
            def.name = format!("idx_{}_{}", coll.name, keys.join("_"));
            def.name.truncate(50);
            def.name = format!(
                "{}_{}",
                def.name.trim_end_matches('_'),
                &forgebase_schema::generate_id()[..6]
            );
        }
        validate_index_name(&def.name)?;
        def.collection = coll.name.clone();
        let d = self.inner.backend.dialect();
        let mut tx = self.begin(TxMode::Normal).await?;
        let result = async {
            if crate::ddl::object_exists(d, &mut *tx, &def.name).await?
                || crate::ddl::object_exists(d, &mut *tx, &fts_table(d, &def.name)).await?
            {
                return Err(DataError::AlreadyExists(format!("index {}", def.name)));
            }
            let cat = CatalogSnapshot::load_tx(&mut *tx).await?;
            let mut log = DdlLog::new(&mut *tx);
            for sql in create_index_sql(d, &cat, &coll, &def)? {
                log.run(sql).await.map_err(|e| unique_build_error(e, &def))?;
            }
            insert_index_row(&mut log, &coll, &def, "manual").await?;
            let statements = std::mem::take(&mut log.statements);
            record_history(
                &mut *tx,
                Some(&coll),
                "index_create",
                &statements,
                None,
                None,
                &ctx.actor(),
            )
            .await?;
            Ok(())
        }
        .await;
        finish(tx, result).await?;
        self.index_info(&def.name).await
    }

    /// Drops a manual index. Automatic indexes follow the schema and are
    /// removed by changing the field.
    pub async fn drop_index(&self, ctx: &AccessContext, name: &str) -> Result<(), DataError> {
        let _guard = self.inner.schema_lock.lock().await;
        let d = self.inner.backend.dialect();
        let mut tx = self.begin(TxMode::Normal).await?;
        let result = async {
            let rows = tx
                .fetch_all(&Statement::new(
                    "SELECT definition, origin, collection_id FROM _forgebase_indexes WHERE name = ?1",
                    vec![name.into()],
                ))
                .await?;
            let Some(row) = rows.rows.first() else {
                return Err(DataError::IndexNotFound(name.to_owned()));
            };
            let def: IndexDefinition = serde_json::from_str(row[0].as_str().unwrap_or("{}"))?;
            let coll = self.collection(row[2].as_str().unwrap_or_default())?;
            self.authorize(ctx, Action::ManageIndexes, Some(&coll))?;
            if row[1].as_str() == Some("auto") {
                return Err(DataError::invalid(
                    "auto_index",
                    "automatic indexes follow the schema; change the field instead",
                ));
            }
            let mut log = DdlLog::new(&mut *tx);
            for sql in drop_index_sql(d, &def) {
                log.run(sql).await?;
            }
            log.exec("DELETE FROM _forgebase_indexes WHERE name = ?1", vec![name.into()])
                .await?;
            let statements = std::mem::take(&mut log.statements);
            record_history(
                &mut *tx,
                Some(&coll),
                "index_drop",
                &statements,
                None,
                None,
                &ctx.actor(),
            )
            .await?;
            Ok(())
        }
        .await;
        finish(tx, result).await
    }

    /// Indexes of one collection (or of every table when `None`), with their
    /// statistics.
    pub async fn list_indexes(
        &self,
        ctx: &AccessContext,
        collection: Option<&str>,
    ) -> Result<Vec<IndexInfo>, DataError> {
        let coll = collection
            .map(|c| self.resolve(ctx, Action::ReadSchema, c).map(|r| r.0))
            .transpose()?;
        self.authorize(ctx, Action::ReadSchema, coll.as_ref())?;
        let b = self.inner.backend.as_ref();
        let mut names = Vec::new();
        let engine = if is_pg(b.dialect()) {
            crate::catalog::pg::index_names(coll.as_ref().map(|c| c.name.as_str()))
        } else {
            Statement::new(
                "SELECT name FROM sqlite_master WHERE type = 'index' AND (?1 IS NULL OR lower(tbl_name) = lower(?1))
                   AND tbl_name NOT LIKE '\\_forgebase\\_%' ESCAPE '\\'",
                vec![coll.as_ref().map(|c| c.name.clone()).into()],
            )
        };
        for r in b.fetch_all(&engine).await?.rows {
            if let Some(n) = r[0].as_str() {
                names.push(n.to_owned());
            }
        }
        let managed = b
            .fetch_all(&Statement::new(
                "SELECT name FROM _forgebase_indexes WHERE CAST(?1 AS TEXT) IS NULL OR collection_id = ?1",
                vec![coll.as_ref().map(|c| c.id.clone()).into()],
            ))
            .await?;
        for r in managed.rows {
            if let Some(n) = r[0].as_str() {
                names.push(n.to_owned());
            }
        }
        names.sort();
        names.dedup();
        let mut out = Vec::new();
        for n in names {
            out.push(self.index_info(&n).await?);
        }
        Ok(out)
    }

    /// One index.
    pub async fn get_index(&self, ctx: &AccessContext, name: &str) -> Result<IndexInfo, DataError> {
        self.authorize(ctx, Action::ReadSchema, None)?;
        self.index_info(name).await
    }

    async fn index_info(&self, name: &str) -> Result<IndexInfo, DataError> {
        let b = self.inner.backend.as_ref();
        let d = b.dialect();
        let managed = b
            .fetch_all(&Statement::new(
                "SELECT definition, origin, collection_id FROM _forgebase_indexes WHERE name = ?1",
                vec![name.into()],
            ))
            .await?;
        let (definition, origin, coll_id) = match managed.rows.first() {
            Some(r) => {
                let def: IndexDefinition = serde_json::from_str(r[0].as_str().unwrap_or("{}"))?;
                let origin = if r[1].as_str() == Some("auto") {
                    IndexOrigin::Auto
                } else {
                    IndexOrigin::Manual
                };
                (Some(def), Some(origin), r[2].as_str().map(str::to_owned))
            }
            None => (None, None, None),
        };
        let collection = coll_id
            .as_deref()
            .and_then(|id| self.catalog().get(id).map(|c| c.name.clone()));
        let definition = definition.map(|mut def| {
            if let Some(c) = &collection {
                def.collection = c.clone();
            }
            def
        });
        if let Some(def) = definition.as_ref().filter(|d| d.kind == IndexKind::Fulltext) {
            let fts = fts_table(d, &def.name);
            let size = self.object_size(&fts).await;
            return Ok(IndexInfo {
                name: name.to_owned(),
                table: collection.clone().unwrap_or_default(),
                collection,
                origin: origin.unwrap_or(IndexOrigin::Manual),
                kind: IndexKind::Fulltext,
                unique: false,
                partial: false,
                columns: def.keys.iter().map(|k| k.path.clone()).collect(),
                definition: Some(def.clone()),
                sql: None,
                stats: IndexStats {
                    stat: None,
                    rows_estimate: None,
                    size_bytes: size,
                    scans: None,
                },
            });
        }
        if is_pg(d) {
            return self.pg_index_info(name, definition, origin, collection).await;
        }
        let master = b
            .fetch_all(&Statement::new(
                "SELECT tbl_name, sql FROM sqlite_master WHERE type = 'index' AND name = ?1",
                vec![name.into()],
            ))
            .await?;
        let Some(m) = master.rows.first() else {
            return Err(DataError::IndexNotFound(name.to_owned()));
        };
        let table = m[0].as_str().unwrap_or_default().to_owned();
        let sql = m[1].as_str().map(str::to_owned);
        let list = b
            .fetch_all(&Statement::raw(format!("PRAGMA index_list({})", d.quote_ident(&table))))
            .await?;
        let (mut unique, mut partial, mut engine) = (false, false, false);
        if let (Some(ni), Some(ui), Some(oi), Some(pi)) = (
            list.column("name"),
            list.column("unique"),
            list.column("origin"),
            list.column("partial"),
        ) {
            for r in &list.rows {
                if r[ni].as_str() == Some(name) {
                    unique = r[ui].as_i64() == Some(1);
                    partial = r[pi].as_i64() == Some(1);
                    engine = r[oi].as_str() != Some("c");
                }
            }
        }
        let info = b
            .fetch_all(&Statement::raw(format!("PRAGMA index_xinfo({})", d.quote_ident(name))))
            .await?;
        let mut columns = Vec::new();
        if let (Some(key), Some(cid), Some(cname)) = (info.column("key"), info.column("cid"), info.column("name")) {
            for r in &info.rows {
                if r[key].as_i64() == Some(1) {
                    columns.push(match (r[cid].as_i64(), r[cname].as_str()) {
                        (Some(-2), _) => "<expr>".to_owned(),
                        (_, Some(n)) => n.to_owned(),
                        _ => "rowid".to_owned(),
                    });
                }
            }
        }
        let stat = self.stat1(name).await;
        let collection = collection.or_else(|| self.catalog().get(&table).map(|c| c.name.clone()));
        Ok(IndexInfo {
            name: name.to_owned(),
            table,
            collection,
            origin: origin.unwrap_or(if engine { IndexOrigin::Engine } else { IndexOrigin::Sql }),
            kind: IndexKind::Btree,
            unique,
            partial,
            columns,
            definition,
            sql,
            stats: IndexStats {
                rows_estimate: stat.as_deref().and_then(|s| s.split(' ').next()?.parse().ok()),
                stat,
                size_bytes: self.object_size(name).await,
                scans: None,
            },
        })
    }

    /// [`Self::index_info`] on PostgreSQL, from `pg_index`, `pg_class` and
    /// `pg_stat_user_indexes` (indexes of tables in schema `public`).
    async fn pg_index_info(
        &self,
        name: &str,
        definition: Option<IndexDefinition>,
        origin: Option<IndexOrigin>,
        collection: Option<String>,
    ) -> Result<IndexInfo, DataError> {
        let rows = self
            .inner
            .backend
            .fetch_all(&crate::catalog::pg::index_row(name))
            .await?;
        let Some(r) = rows.rows.first() else {
            return Err(DataError::IndexNotFound(name.to_owned()));
        };
        let col = |c: &str| rows.column(c).and_then(|i| r.get(i));
        let flag = |c: &str| col(c).and_then(Value::as_i64) == Some(1);
        let table = col("tbl").and_then(Value::as_str).unwrap_or_default().to_owned();
        let columns: Vec<String> = col("cols")
            .and_then(Value::as_str)
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        let est = col("est").and_then(Value::as_i64);
        let collection = collection.or_else(|| self.catalog().get(&table).map(|c| c.name.clone()));
        Ok(IndexInfo {
            name: name.to_owned(),
            table,
            collection,
            origin: origin.unwrap_or(if flag("engine") { IndexOrigin::Engine } else { IndexOrigin::Sql }),
            kind: IndexKind::Btree,
            unique: flag("uniq"),
            partial: flag("partial"),
            columns,
            definition,
            sql: col("sql").and_then(Value::as_str).map(str::to_owned),
            stats: IndexStats {
                stat: est.map(|n| n.to_string()),
                rows_estimate: est,
                size_bytes: col("size").and_then(Value::as_i64),
                scans: col("scans").and_then(Value::as_i64),
            },
        })
    }

    async fn stat1(&self, index: &str) -> Option<String> {
        let b = self.inner.backend.as_ref();
        let rows = b.fetch_all(&b.dialect().index_stats(index)).await.ok()?; // sqlite_stat1 exists only after the first ANALYZE
        rows.scalar().and_then(Value::as_str).map(str::to_owned)
    }

    async fn object_size(&self, name: &str) -> Option<i64> {
        let d = self.inner.backend.dialect();
        let stmt = if is_pg(d) {
            crate::catalog::pg::relation_size(&pg_regclass(d, name))
        } else {
            // The FTS5 shadow tables (`<name>_data`, …) count too.
            d.object_size(name)
        };
        let rows = self.inner.backend.fetch_all(&stmt).await.ok()?;
        rows.scalar().and_then(Value::as_i64)
    }

    /// Rebuilds an index from its table (`REINDEX`, or a full-text rebuild)
    /// and refreshes its statistics. On PostgreSQL this is a plain `REINDEX
    /// INDEX` (the concurrent form cannot run in a transaction), which blocks
    /// writes to the table while it runs, then `ANALYZE` of the table.
    pub async fn rebuild_index(&self, ctx: &AccessContext, name: &str) -> Result<IndexInfo, DataError> {
        let _guard = self.inner.schema_lock.lock().await;
        self.authorize(ctx, Action::ManageIndexes, None)?;
        let info = self.index_info(name).await?;
        let d = self.inner.backend.dialect();
        let mut tx = self.begin(TxMode::Normal).await?;
        let result = async {
            if is_pg(d) {
                let engine_name = match info.definition.as_ref().filter(|d| d.kind == IndexKind::Fulltext) {
                    Some(def) => fts_table(d, &def.name),
                    None => name.to_owned(),
                };
                tx.execute(&Statement::raw(format!("REINDEX INDEX {}", rel(d, &engine_name))))
                    .await?;
                tx.execute(&Statement::raw(format!("ANALYZE {}", rel(d, &info.table))))
                    .await?;
                return Ok(());
            }
            match info.definition.as_ref().filter(|d| d.kind == IndexKind::Fulltext) {
                Some(def) => {
                    let coll = self.collection(&def.collection)?;
                    let cat = self.catalog();
                    let fts = d.quote_ident(&fts_table(d, &def.name));
                    let cols: Vec<String> = (0..def.keys.len()).map(|i| format!("c{i}")).collect();
                    let exprs = key_exprs(d, &cat, &coll, def, "")?;
                    tx.execute(&Statement::raw(format!("DELETE FROM {fts}"))).await?;
                    tx.execute(&Statement::raw(format!(
                        "INSERT INTO {fts} (rowid, {}) SELECT rowid, {} FROM {}",
                        cols.join(", "),
                        exprs.join(", "),
                        d.quote_ident(&coll.name)
                    )))
                    .await?;
                }
                None => {
                    tx.execute(&Statement::raw(format!("REINDEX {}", d.quote_ident(name))))
                        .await?;
                    tx.execute(&Statement::raw(format!("ANALYZE {}", d.quote_ident(name))))
                        .await?;
                }
            }
            Ok(())
        }
        .await;
        finish(tx, result).await?;
        self.index_info(name).await
    }

    /// Refreshes planner statistics (`ANALYZE`) for a collection or the
    /// whole database.
    pub async fn analyze(&self, ctx: &AccessContext, collection: Option<&str>) -> Result<(), DataError> {
        let coll = collection
            .map(|c| self.resolve(ctx, Action::ManageIndexes, c).map(|r| r.0))
            .transpose()?;
        self.authorize(ctx, Action::ManageIndexes, coll.as_ref())?;
        let d = self.inner.backend.dialect();
        let sql = match &coll {
            Some(c) => format!("ANALYZE {}", rel(d, &c.name)),
            None => "ANALYZE".to_owned(),
        };
        let mut tx = self.begin(TxMode::Normal).await?;
        let r = tx
            .execute(&Statement::raw(sql))
            .await
            .map(|_| ())
            .map_err(DataError::from);
        finish(tx, r).await
    }

    /// The plan of a record listing (the same SQL `list_records` runs).
    pub async fn explain_list(
        &self,
        ctx: &AccessContext,
        collection: &str,
        q: &ListQuery,
    ) -> Result<QueryPlan, DataError> {
        let (coll, pred) = self.resolve(ctx, Action::List, collection)?;
        let fts = self.fulltext_index(&coll, q).await?;
        let built = self.build_list(ctx, &coll, q, pred.as_ref(), fts.as_ref())?;
        self.plan(&built.select).await
    }

    /// The plan of `stmt`. PostgreSQL's plan is read here from `EXPLAIN
    /// (FORMAT JSON)` ([`QueryPlan::from_pg_json`]): telling a full index walk
    /// from a lookup needs the index condition apart from the filter.
    async fn plan(&self, stmt: &Statement) -> Result<QueryPlan, DataError> {
        let b = self.inner.backend.as_ref();
        let d = b.dialect();
        if !is_pg(d) {
            return Ok(QueryPlan::from_steps(stmt.sql.clone(), b.explain(stmt).await?));
        }
        let mut explain = stmt.clone();
        explain.sql = format!("{}{}", d.explain_prefix(), stmt.sql);
        explain.cache = false;
        explain.label = None; // never a slow query to recommend for
        let rows = b.fetch_all(&explain).await?;
        let text = rows.scalar().and_then(Value::as_str).unwrap_or("[]");
        let json: Json = serde_json::from_str(text)
            .map_err(|e| DataError::Internal(format!("unreadable query plan: {e}")))?;
        Ok(QueryPlan::from_pg_json(
            stmt.sql.clone(),
            json.get(0).and_then(|p| p.get("Plan")),
        ))
    }

    /// Recent slow statements.
    pub fn slow_queries(&self, ctx: &AccessContext) -> Result<Vec<SlowQuery>, DataError> {
        self.authorize(ctx, Action::ReadSchema, None)?;
        Ok(self.inner.backend.slow_queries())
    }

    /// Clears the slow-query log.
    pub fn clear_slow_queries(&self, ctx: &AccessContext) -> Result<(), DataError> {
        self.authorize(ctx, Action::ManageIndexes, None)?;
        self.inner.backend.clear_slow_queries();
        Ok(())
    }

    /// Current slow-query threshold.
    pub fn slow_query_threshold(&self) -> Duration {
        self.inner.backend.slow_query_threshold()
    }

    /// Sets the slow-query threshold.
    pub fn set_slow_query_threshold(&self, ctx: &AccessContext, threshold: Duration) -> Result<(), DataError> {
        self.authorize(ctx, Action::ManageIndexes, None)?;
        self.inner.backend.set_slow_query_threshold(threshold);
        Ok(())
    }

    /// Suggests indexes for slow record queries whose plan scans the whole
    /// table: equality keys first, then one range key, else the sort keys.
    /// Suggestions already covered by an index's leading keys are skipped.
    pub async fn recommend_indexes(&self, ctx: &AccessContext) -> Result<Vec<IndexRecommendation>, DataError> {
        self.authorize(ctx, Action::ReadSchema, None)?;
        let mut by_key: BTreeMap<(String, Vec<String>), IndexRecommendation> = BTreeMap::new();
        let mut planned: HashSet<String> = HashSet::new();
        let mut scans: HashSet<String> = HashSet::new();
        for sq in self.inner.backend.slow_queries() {
            let Some(label) = &sq.label else { continue };
            let Some(coll) = self.catalog().get(&label.collection).cloned() else {
                continue;
            };
            if planned.insert(sq.sql.clone()) {
                // The log keeps no values. PostgreSQL folds `x = NULL` to
                // a constant (no scan in the plan), so it gets a neutral
                // value that converts to every parameter type ForgeBase binds.
                let filler = if is_pg(self.inner.backend.dialect()) {
                    Value::Text("0".into())
                } else {
                    Value::Null
                };
                let probe = Statement::new(sq.sql.clone(), vec![filler; sq.param_count]);
                if let Ok(plan) = self.plan(&probe).await
                    && plan.full_scans.iter().any(|t| t == "t")
                {
                    scans.insert(sq.sql.clone());
                }
            }
            if !scans.contains(&sq.sql) {
                continue;
            }
            let mut keys: Vec<String> = label.equality_keys.clone();
            if let Some(r) = label.range_keys.first() {
                if !keys.contains(r) {
                    keys.push(r.clone());
                }
            } else {
                for s in &label.sort_keys {
                    let s = s.trim_start_matches('-').to_owned();
                    if !keys.contains(&s) {
                        keys.push(s);
                    }
                }
            }
            keys.retain(|k| k != "id");
            if keys.is_empty() {
                continue;
            }
            let entry = by_key
                .entry((coll.name.clone(), keys.clone()))
                .or_insert_with(|| IndexRecommendation {
                    definition: IndexDefinition {
                        name: String::new(),
                        collection: coll.name.clone(),
                        keys: keys.iter().map(|k| IndexKey::asc(k.clone())).collect(),
                        unique: false,
                        partial: None,
                        kind: IndexKind::Btree,
                    },
                    reason: format!(
                        "slow queries on {} filter or sort by {} and scan the whole table",
                        coll.name,
                        keys.join(", ")
                    ),
                    occurrences: 0,
                    max_duration_ms: 0.0,
                });
            entry.occurrences += 1;
            entry.max_duration_ms = entry.max_duration_ms.max(sq.duration_ms);
        }
        let mut out = Vec::new();
        for ((coll, keys), rec) in by_key {
            let existing = self.list_indexes(&AccessContext::system(), Some(&coll)).await?;
            let covered = existing.iter().any(|ix| {
                let ix_keys: Vec<String> = match &ix.definition {
                    Some(d) => d.keys.iter().map(|k| k.path.to_ascii_lowercase()).collect(),
                    None => ix.columns.iter().map(|c| c.to_ascii_lowercase()).collect(),
                };
                ix.partial.eq(&false)
                    && ix_keys.len() >= keys.len()
                    && keys.iter().zip(&ix_keys).all(|(a, b)| a.to_ascii_lowercase() == *b)
            });
            if !covered {
                out.push(rec);
            }
        }
        out.sort_by(|a, b| b.max_duration_ms.total_cmp(&a.max_duration_ms));
        Ok(out)
    }
}

fn unique_build_error(e: DataError, def: &IndexDefinition) -> DataError {
    match e {
        DataError::Db(forgebase_db::DbError::Constraint {
            kind: forgebase_db::ConstraintKind::Unique,
            ..
        }) => DataError::conflict(
            "duplicate_values",
            format!(
                "cannot create unique index {}: existing records have duplicate values",
                def.name
            ),
        ),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_parsing() {
        let steps = vec![
            PlanStep {
                id: 2,
                parent: 0,
                detail: "SEARCH t USING INDEX idx_a (a=? AND b>?)".into(),
            },
            PlanStep {
                id: 3,
                parent: 0,
                detail: "SCAN r1".into(),
            },
            PlanStep {
                id: 4,
                parent: 0,
                detail: "SCAN t USING COVERING INDEX idx_b".into(),
            },
            PlanStep {
                id: 5,
                parent: 0,
                detail: "SCAN CONSTANT ROW".into(),
            },
        ];
        let p = QueryPlan::from_steps(String::new(), steps);
        assert_eq!(p.indexes_used, vec!["idx_a", "idx_b"]);
        assert_eq!(p.full_scans, vec!["r1", "t"]);
    }

    #[test]
    fn postgres_plan_parsing() {
        let plan = serde_json::json!({
            "Node Type": "Limit",
            "Plans": [{
                "Node Type": "Nested Loop",
                "Plans": [
                    {"Node Type": "Index Scan", "Index Name": "items_pkey", "Relation Name": "items",
                     "Alias": "t", "Filter": "(other = '3'::bigint)"},
                    {"Node Type": "Index Scan", "Index Name": "idx_a", "Relation Name": "orders",
                     "Alias": "r1", "Index Cond": "(a = t.id)"},
                    {"Node Type": "Bitmap Heap Scan", "Relation Name": "tags", "Alias": "g",
                     "Plans": [{"Node Type": "Bitmap Index Scan", "Index Name": "idx_g", "Index Cond": "(x = 1)"}]},
                    {"Node Type": "Seq Scan", "Relation Name": "events", "Alias": "events"}
                ]
            }]
        });
        let p = QueryPlan::from_pg_json(String::new(), Some(&plan));
        assert_eq!(p.indexes_used, vec!["items_pkey", "idx_a", "idx_g"]);
        assert_eq!(p.full_scans, vec!["t", "events"], "an index walk with only a filter reads everything");
        assert_eq!(p.steps.len(), 7);
        assert_eq!(p.steps[2].detail, "Index Scan using items_pkey on items t filter (other = '3'::bigint)");
        assert_eq!(p.steps[3].detail, "Index Scan using idx_a on orders r1 ((a = t.id))");
        assert_eq!(p.steps[5].parent, p.steps[4].id);
        assert!(QueryPlan::from_pg_json(String::new(), None).steps.is_empty());
    }

    #[test]
    fn index_names() {
        assert!(validate_index_name("idx_products_price").is_ok());
        assert!(validate_index_name("auto_x").is_err());
        assert!(validate_index_name("pg_x").is_err());
        assert!(validate_index_name(&"a".repeat(63)).is_ok());
        assert!(validate_index_name(&"a".repeat(64)).is_err());
        assert!(validate_index_name("x\"; DROP").is_err());
    }
}
