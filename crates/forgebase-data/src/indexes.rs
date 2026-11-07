//! Index manager (ADR-0003 §8): definitions, DDL, inspection, rebuild,
//! statistics, query plans, slow queries and recommendations.
//!
//! Indexes use the engine's native structures (SPEC §4.5): B-tree indexes
//! (single, composite, unique, partial, expression, JSON-path) and FTS5
//! full-text tables kept in sync by triggers.

use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

use forgebase_db::{Dialect, PlanStep, SlowQuery, Statement, TxMode, Value, WriteTx};
use forgebase_query::{CompileOptions, Compiler, FieldPath, SqlBuilder, parse_filter};
use forgebase_schema::{Collection, FieldKind};
use serde::{Deserialize, Serialize};

use crate::catalog::CatalogSnapshot;
use crate::ddl::{DdlLog, record_history};
use crate::{AccessContext, Action, DataError, DataService, ListQuery, finish};

/// Prefix of FTS5 tables and their triggers.
pub(crate) const FTS_PREFIX: &str = "_forgebase_fts_";

/// Index kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexKind {
    /// B-tree index (single, composite, unique, partial, expression, JSON path).
    #[default]
    Btree,
    /// FTS5 full-text index.
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
    let ok = name.len() <= 63
        && name.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    let lower = name.to_ascii_lowercase();
    if !ok || lower.starts_with("auto_") || lower.starts_with("sqlite_") {
        return Err(DataError::invalid(
            "invalid_index_name",
            "index names use letters, digits and underscores, start with a letter, and may not start with auto_ or sqlite_",
        ));
    }
    Ok(())
}

/// The automatic indexes of a collection: a unique index per unique field and
/// an index per single relation field (fast joins and cascades).
pub(crate) fn auto_indexes(coll: &Collection) -> Vec<IndexDefinition> {
    let mut out = Vec::new();
    if coll.is_view() {
        return out;
    }
    for f in &coll.fields {
        let single_relation = matches!(f.kind, FieldKind::Relation { max_select: 1, .. });
        if f.unique || single_relation {
            out.push(IndexDefinition {
                name: format!("auto_{}_{}_{}", coll.id, f.id, if f.unique { "u" } else { "r" }),
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

fn fts_table(name: &str) -> String {
    format!("{FTS_PREFIX}{name}")
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
    let table = q(&coll.name);
    match def.kind {
        IndexKind::Btree => {
            let exprs = key_exprs(d, cat, coll, def, "")?;
            let keys: Vec<String> = exprs
                .iter()
                .zip(&def.keys)
                .map(|(e, k)| format!("{e}{}", if k.desc { " DESC" } else { "" }))
                .collect();
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
            let fts = q(&fts_table(&def.name));
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

/// Triggers that keep a full-text index in sync with its table.
pub(crate) fn fts_trigger_sql(
    d: &dyn Dialect,
    cat: &CatalogSnapshot,
    coll: &Collection,
    def: &IndexDefinition,
) -> Result<Vec<String>, DataError> {
    let q = |s: &str| d.quote_ident(s);
    let table = q(&coll.name);
    let fts_name = fts_table(&def.name);
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

/// DDL dropping an index (and its full-text table).
pub(crate) fn drop_index_sql(d: &dyn Dialect, def: &IndexDefinition) -> Vec<String> {
    let q = |s: &str| d.quote_ident(s);
    match def.kind {
        IndexKind::Btree => vec![format!("DROP INDEX IF EXISTS {}", q(&def.name))],
        IndexKind::Fulltext => {
            let fts = fts_table(&def.name);
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
        let coll = self.collection(&def.collection)?;
        self.authorize(ctx, Action::ManageIndexes, Some(&coll))?;
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
            if crate::ddl::object_exists(&mut *tx, &def.name).await?
                || crate::ddl::object_exists(&mut *tx, &fts_table(&def.name)).await?
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
        let coll = collection.map(|c| self.collection(c)).transpose()?;
        self.authorize(ctx, Action::ReadSchema, coll.as_ref())?;
        let b = self.inner.backend.as_ref();
        let mut names = Vec::new();
        let rows = b
            .fetch_all(&Statement::new(
                "SELECT name FROM sqlite_master WHERE type = 'index' AND (?1 IS NULL OR lower(tbl_name) = lower(?1))
                   AND tbl_name NOT LIKE '\\_forgebase\\_%' ESCAPE '\\'
                 UNION SELECT name FROM _forgebase_indexes WHERE ?2 IS NULL OR collection_id = ?2
                 ORDER BY 1",
                vec![
                    coll.as_ref().map(|c| c.name.clone()).into(),
                    coll.as_ref().map(|c| c.id.clone()).into(),
                ],
            ))
            .await?;
        for r in rows.rows {
            if let Some(n) = r[0].as_str() {
                names.push(n.to_owned());
            }
        }
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
            let fts = fts_table(&def.name);
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
                },
            });
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
            },
        })
    }

    async fn stat1(&self, index: &str) -> Option<String> {
        let b = self.inner.backend.as_ref();
        let rows = b
            .fetch_all(&Statement::new(
                "SELECT stat FROM sqlite_stat1 WHERE idx = ?1",
                vec![index.into()],
            ))
            .await
            .ok()?; // sqlite_stat1 exists only after the first ANALYZE
        rows.scalar().and_then(Value::as_str).map(str::to_owned)
    }

    async fn object_size(&self, name: &str) -> Option<i64> {
        let rows = self
            .inner
            .backend
            .fetch_all(&Statement::new(
                "SELECT sum(pgsize) FROM dbstat WHERE name = ?1 OR name LIKE ?2 ESCAPE '\\'",
                vec![
                    name.into(),
                    format!("{}\\_%", self.inner.backend.dialect().escape_like(name)).into(),
                ],
            ))
            .await
            .ok()?;
        rows.scalar().and_then(Value::as_i64)
    }

    /// Rebuilds an index from its table (`REINDEX`, or a full-text rebuild)
    /// and refreshes its statistics.
    pub async fn rebuild_index(&self, ctx: &AccessContext, name: &str) -> Result<IndexInfo, DataError> {
        let _guard = self.inner.schema_lock.lock().await;
        self.authorize(ctx, Action::ManageIndexes, None)?;
        let info = self.index_info(name).await?;
        let d = self.inner.backend.dialect();
        let mut tx = self.begin(TxMode::Normal).await?;
        let result = async {
            match info.definition.as_ref().filter(|d| d.kind == IndexKind::Fulltext) {
                Some(def) => {
                    let coll = self.collection(&def.collection)?;
                    let cat = self.catalog();
                    let fts = d.quote_ident(&fts_table(&def.name));
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
        let coll = collection.map(|c| self.collection(c)).transpose()?;
        self.authorize(ctx, Action::ManageIndexes, coll.as_ref())?;
        let d = self.inner.backend.dialect();
        let sql = match &coll {
            Some(c) => format!("ANALYZE {}", d.quote_ident(&c.name)),
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
        let coll = self.collection(collection)?;
        let pred = self.authorize(ctx, Action::List, Some(&coll))?;
        let fts = self.fulltext_index(&coll, q).await?;
        let built = self.build_list(ctx, &coll, q, pred.as_ref(), fts.as_ref())?;
        let steps = self.inner.backend.explain(&built.select).await?;
        Ok(QueryPlan::from_steps(built.select.sql.clone(), steps))
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
                let probe = Statement::new(sq.sql.clone(), vec![Value::Null; sq.param_count]);
                if let Ok(steps) = self.inner.backend.explain(&probe).await
                    && QueryPlan::from_steps(sq.sql.clone(), steps)
                        .full_scans
                        .iter()
                        .any(|t| t == "t")
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
    fn index_names() {
        assert!(validate_index_name("idx_products_price").is_ok());
        assert!(validate_index_name("auto_x").is_err());
        assert!(validate_index_name("x\"; DROP").is_err());
    }
}
