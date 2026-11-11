//! The pgvector engine (ADR-0029 §1.4, FB-162): SQL over the `vec` column
//! of `_forgebase_vector_values` and one partial HNSW index per (collection,
//! field). Nothing here keeps state; the service decides when to call it.
//!
//! * **Storage.** `vec <schema>.vector` (no fixed dimension) holds the
//!   canonical vector; the portable `vector bytea` stays `NULL`. pgvector
//!   stores `float4`, so the stored vector is the returned vector. Values
//!   travel as pgvector's text form (`[1,0.5]`): Rust prints the shortest
//!   decimal that reads back as the same `f32`, and pgvector prints the same.
//! * **Index.** `fbv_<collection id>_<hash>`: `USING hnsw ((CAST(vec AS
//!   T(dim))) T_<metric>_ops) WITH (m, ef_construction) WHERE collection_id
//!   = <id> AND field = '<field>'`, with T = `vector` up to 2 000
//!   dimensions, `halfvec` up to 4 000 (and for `f16`/`i8` fields); wider
//!   fields have exact search only. The hash covers the field's name,
//!   creation time and build parameters, so a re-created field never meets
//!   a leftover index. Searches repeat the cast and the predicate (as
//!   literals, so even a generic plan matches the partial index).
//! * **Distances** (ADR-0012): cosine `<=>`, L2 `<->`, dot `1 + (a <#> b)`;
//!   every returned distance is computed on the stored `float4` vector,
//!   also when the index orders by `halfvec`.
//! * **Settings per search** (`hnsw.ef_search`, `hnsw.iterative_scan`,
//!   `hnsw.max_scan_tuples`): `set_config(…, true)` in a one-row subquery
//!   the index scan is laterally joined to, so they are set before the scan
//!   starts; each read is its own `READ ONLY` transaction (FB-131), which
//!   ends them.

use std::collections::HashMap;
use std::fmt::Write as _;

use forgebase_data::query::SqlBuilder;
use forgebase_db::{Backend, DbError, Feature, Statement, TxMode, Value, WriteTx};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::error::VectorError;
use crate::filter::Filter;
use crate::metric::{self, Scored};
use crate::model::{Metric, Quantization, VectorEngine, VectorField};
use crate::store::ITEMS;

type Result<T> = std::result::Result<T, VectorError>;

/// Prefix of ForgeBase's HNSW indexes on `_forgebase_vector_values`.
pub(crate) const INDEX_PREFIX: &str = "fbv_";
/// pgvector indexes `vector` up to this many dimensions.
pub(crate) const MAX_VECTOR_DIMS: u32 = 2000;
/// pgvector indexes `halfvec` up to this many dimensions.
pub(crate) const MAX_HALFVEC_DIMS: u32 = 4000;
/// Oldest pgvector the engine uses (`halfvec`).
const MIN_VERSION: (u32, u32, u32) = (0, 7, 0);
/// First pgvector with iterative index scans.
const ITERATIVE_VERSION: (u32, u32, u32) = (0, 8, 0);
/// pgvector's largest `hnsw.ef_search`.
pub(crate) const MAX_EF_SEARCH: u32 = 1000;
/// pgvector's default `hnsw.max_scan_tuples`.
pub(crate) const DEFAULT_MAX_SCAN_TUPLES: u64 = 20_000;

/// The pgvector installation the engine uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pgv {
    /// Schema of the extension's types and operators, quoted.
    pub schema: String,
    /// `extversion`.
    pub version: String,
    /// pgvector 0.8+: `hnsw.iterative_scan` and `hnsw.max_scan_tuples`.
    pub iterative: bool,
}

fn ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

fn lit(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn parse_version(v: &str) -> (u32, u32, u32) {
    let mut it = v
        .split(|c: char| !c.is_ascii_digit())
        .map(|p| p.parse::<u32>().unwrap_or(0));
    (it.next().unwrap_or(0), it.next().unwrap_or(0), it.next().unwrap_or(0))
}

fn bad(what: &str) -> VectorError {
    VectorError::Internal(format!("unexpected stored value in {what}"))
}

fn int(v: &Value, what: &str) -> Result<i64> {
    v.as_i64().ok_or_else(|| bad(what))
}

fn key(v: &Value) -> Result<u64> {
    u64::try_from(int(v, "key")?).map_err(|_| bad("key"))
}

fn real(v: &Value) -> Result<f64> {
    match v {
        Value::Real(f) => Ok(*f),
        #[allow(clippy::cast_precision_loss)]
        Value::Integer(i) => Ok(*i as f64),
        _ => Err(bad("distance")),
    }
}

fn int_of(n: u64) -> Value {
    Value::Integer(i64::try_from(n).unwrap_or(i64::MAX))
}

fn keys_json(keys: &[u64]) -> Value {
    Value::Text(serde_json::to_string(keys).unwrap_or_else(|_| "[]".into()))
}

/// pgvector's text form of `v`.
pub(crate) fn text_of(v: &[f32]) -> String {
    let mut s = String::with_capacity(v.len() * 10 + 2);
    s.push('[');
    for (i, x) in v.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        let _ = write!(s, "{x}");
    }
    s.push(']');
    s
}

/// Parses pgvector's text form.
pub(crate) fn parse_text(t: &str) -> Result<Vec<f32>> {
    let inner = t
        .trim()
        .strip_prefix('[')
        .and_then(|r| r.strip_suffix(']'))
        .ok_or_else(|| bad("vec"))?;
    if inner.trim().is_empty() {
        return Ok(Vec::new());
    }
    inner
        .split(',')
        .map(|p| p.trim().parse::<f32>().map_err(|_| bad("vec")))
        .collect()
}

/// The stored vector of a `CAST(vec AS text)` column in the portable byte
/// encoding (what the service decodes).
pub(crate) fn bytes_of(v: &Value) -> Result<Vec<u8>> {
    let t = v.as_str().ok_or_else(|| bad("vec"))?;
    Ok(metric::encode(&parse_text(t)?))
}

impl Pgv {
    /// `<schema>.<name>`.
    pub(crate) fn ty(&self, name: &str) -> String {
        format!("{}.{name}", self.schema)
    }

    fn op(&self, metric: Metric) -> String {
        let o = match metric {
            Metric::Cosine => "<=>",
            Metric::L2 => "<->",
            Metric::Dot => "<#>",
        };
        format!("OPERATOR({}.{o})", self.schema)
    }

    /// A bound text parameter as a `vector`.
    pub(crate) fn vector_param(&self, p: &str) -> String {
        format!("CAST(CAST({p} AS text) AS {})", self.ty("vector"))
    }

    /// The public distance (ADR-0012) between two `vector` expressions.
    fn distance(&self, metric: Metric, a: &str, b: &str) -> String {
        match metric {
            Metric::Dot => format!("(1 + ({a} {} {b}))", self.op(metric)),
            _ => format!("({a} {} {b})", self.op(metric)),
        }
    }
}

/// The internal (raw) distance of a public one: L2 is compared squared,
/// like the portable path.
fn raw(metric: Metric, public: f64) -> f32 {
    #[allow(clippy::cast_possible_truncation)]
    let p = public as f32;
    match metric {
        Metric::L2 => p * p,
        Metric::Cosine | Metric::Dot => p,
    }
}

// ----------------------------------------------------------------- layout

/// The usable pgvector of this database, or why there is none: the
/// extension installed (0.7+), the `vec` column of the right type and the
/// portable `vector` column nullable (FB-161's DDL version 1).
pub(crate) async fn probe(db: &dyn Backend) -> Result<std::result::Result<Pgv, Vec<String>>> {
    if db.dialect().name() != "postgres" {
        return Ok(Err(vec!["pgvector needs PostgreSQL".into()]));
    }
    let rows = db
        .fetch_all(&Statement::raw(
            "SELECT e.extversion::text, n.nspname::text FROM pg_extension e \
             JOIN pg_namespace n ON n.oid = e.extnamespace WHERE e.extname = 'vector'",
        ))
        .await?;
    let Some(r) = rows.rows.first() else {
        return Ok(Err(vec![
            "the vector extension (pgvector) is not installed in this database".into(),
        ]));
    };
    let version = r[0].as_str().unwrap_or_default().to_owned();
    let schema_name = r[1].as_str().unwrap_or_default().to_owned();
    let v = parse_version(&version);
    let mut problems = Vec::new();
    if v < MIN_VERSION {
        problems.push(format!("pgvector {version} is older than 0.7.0 (halfvec)"));
    }
    let cols = columns(db).await?;
    match cols.get("vec") {
        None => problems.push("_forgebase_vector_values has no column vec".into()),
        Some((ty, _)) if ty != "vector" && !ty.ends_with(".vector") => {
            problems.push(format!("_forgebase_vector_values.vec is {ty}, not vector"));
        }
        Some(_) => {}
    }
    if cols.get("vector").is_some_and(|(_, not_null)| *not_null) {
        problems.push("_forgebase_vector_values.vector is NOT NULL".into());
    }
    if !problems.is_empty() {
        return Ok(Err(problems));
    }
    Ok(Ok(Pgv {
        schema: ident(&schema_name),
        version,
        iterative: v >= ITERATIVE_VERSION,
    }))
}

/// `name -> (format_type, not null)` of the vector columns.
async fn columns(db: &dyn Backend) -> Result<HashMap<String, (String, bool)>> {
    let rows = db
        .fetch_all(&Statement::raw(
            "SELECT a.attname::text, format_type(a.atttypid, a.atttypmod), CAST(a.attnotnull AS int) \
             FROM pg_attribute a WHERE a.attrelid = CAST('_forgebase_vector_values' AS regclass) \
             AND a.attname IN ('vec', 'vector') AND a.attnum > 0 AND NOT a.attisdropped",
        ))
        .await?;
    Ok(rows
        .rows
        .iter()
        .map(|r| {
            (
                r[0].as_str().unwrap_or_default().to_owned(),
                (r[1].as_str().unwrap_or_default().to_owned(), r[2].as_i64() == Some(1)),
            )
        })
        .collect())
}

/// Whether the `vec` column exists (FB-161's DDL ran).
pub(crate) async fn has_vec_column(db: &dyn Backend) -> Result<bool> {
    if db.dialect().name() != "postgres" {
        return Ok(false);
    }
    Ok(columns(db).await?.contains_key("vec"))
}

/// `_forgebase_meta` `ext.vector.engine` (FB-161's pin).
pub(crate) async fn stored_engine(db: &dyn Backend) -> Result<Option<String>> {
    let rows = db
        .fetch_all(&Statement::new(
            "SELECT value FROM _forgebase_meta WHERE key = ?1",
            vec![Feature::Vector.engine_key().into()],
        ))
        .await?;
    Ok(rows.rows.first().and_then(|r| r[0].as_str().map(str::to_owned)))
}

/// Records the engine the vectors are stored with.
async fn write_pin(tx: &mut dyn WriteTx, to: VectorEngine) -> Result<()> {
    tx.execute(&Statement::new(
        "INSERT INTO _forgebase_meta (key, value, updated_at) VALUES (?1, ?2, ?3) \
         ON CONFLICT (key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        vec![
            Feature::Vector.engine_key().into(),
            match to {
                VectorEngine::Pgvector => "extension",
                VectorEngine::Portable => "portable",
            }
            .into(),
            forgebase_core::now_rfc3339().into(),
        ],
    ))
    .await?;
    Ok(())
}

/// Records the engine alone (a conversion with nothing to move).
pub(crate) async fn record_engine(db: &dyn Backend, to: VectorEngine) -> Result<()> {
    let mut tx = db.begin(TxMode::Normal).await?;
    match write_pin(&mut *tx, to).await {
        Ok(()) => Ok(tx.commit().await?),
        Err(e) => {
            let _ = tx.rollback().await;
            Err(e)
        }
    }
}

// ----------------------------------------------------------------- indexes

/// Element type of a field's index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Elem {
    Vector,
    Halfvec,
}

impl Elem {
    fn name(self) -> &'static str {
        match self {
            Elem::Vector => "vector",
            Elem::Halfvec => "halfvec",
        }
    }
}

/// The HNSW index of one field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IndexSpec {
    pub name: String,
    pub cid: i64,
    pub field: String,
    pub dimension: u32,
    pub metric: Metric,
    /// `None`: wider than pgvector indexes (exact search only).
    pub elem: Option<Elem>,
    pub m: u32,
    pub ef_construction: u32,
}

/// The element type for a field (ADR-0029 §1.4): `f32` up to 2 000
/// dimensions as `vector`, wider `f32` and every `f16`/`i8` field (pgvector
/// has no 8-bit type) up to 4 000 as `halfvec`, nothing above.
pub(crate) fn elem_of(dimension: u32, q: Quantization) -> Option<Elem> {
    match q {
        Quantization::F32 if dimension <= MAX_VECTOR_DIMS => Some(Elem::Vector),
        _ if dimension <= MAX_HALFVEC_DIMS => Some(Elem::Halfvec),
        _ => None,
    }
}

/// The index of field `f` of collection `cid`.
pub(crate) fn spec(cid: i64, f: &VectorField) -> IndexSpec {
    // pgvector: 2 <= m <= 100, 4 <= ef_construction <= 1000, ef_construction >= 2 m.
    let m = f.connectivity.clamp(2, 100);
    let ef_construction = f.expansion_add.clamp(2 * m, 1000);
    let mut h = Sha256::new();
    h.update(format!(
        "{}\0{}\0{}\0{}\0{}\0{m}\0{ef_construction}",
        f.name,
        f.created,
        f.dimension,
        f.metric.as_str(),
        f.quantization.as_str()
    ));
    let hash: String = h.finalize().iter().take(6).map(|b| format!("{b:02x}")).collect();
    IndexSpec {
        name: format!("{INDEX_PREFIX}{cid}_{hash}"),
        cid,
        field: f.name.clone(),
        dimension: f.dimension,
        metric: f.metric,
        elem: elem_of(f.dimension, f.quantization),
        m,
        ef_construction,
    }
}

impl IndexSpec {
    /// The indexed expression over `col`.
    fn cast(&self, pgv: &Pgv, col: &str) -> Option<String> {
        self.elem
            .map(|e| format!("CAST({col} AS {}({}))", pgv.ty(e.name()), self.dimension))
    }

    fn ops(&self, pgv: &Pgv) -> Option<String> {
        let m = match self.metric {
            Metric::Cosine => "cosine",
            Metric::L2 => "l2",
            Metric::Dot => "ip",
        };
        self.elem.map(|e| pgv.ty(&format!("{}_{m}_ops", e.name())))
    }

    /// The partial predicate, with literals (the planner proves it).
    fn predicate(&self, alias: &str) -> String {
        format!(
            "{alias}collection_id = {} AND {alias}field = {}",
            self.cid,
            lit(&self.field)
        )
    }

    /// `CREATE INDEX [CONCURRENTLY] IF NOT EXISTS …`, or `None` for a field
    /// pgvector cannot index.
    pub(crate) fn create_sql(&self, pgv: &Pgv, concurrently: bool) -> Option<String> {
        Some(format!(
            "CREATE INDEX {}IF NOT EXISTS {} ON _forgebase_vector_values USING hnsw (({}) {}) \
             WITH (m = {}, ef_construction = {}) WHERE {}",
            if concurrently { "CONCURRENTLY " } else { "" },
            ident(&self.name),
            self.cast(pgv, "vec")?,
            self.ops(pgv)?,
            self.m,
            self.ef_construction,
            self.predicate("")
        ))
    }
}

/// `DROP INDEX [CONCURRENTLY] IF EXISTS <name>`.
pub(crate) fn drop_sql(name: &str, concurrently: bool) -> String {
    format!(
        "DROP INDEX {}IF EXISTS {}",
        if concurrently { "CONCURRENTLY " } else { "" },
        ident(name)
    )
}

/// `REINDEX INDEX CONCURRENTLY <name>`.
pub(crate) fn reindex_sql(name: &str) -> String {
    format!("REINDEX INDEX CONCURRENTLY {}", ident(name))
}

/// One of ForgeBase's indexes on `_forgebase_vector_values`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IndexInfo {
    pub valid: bool,
    pub bytes: u64,
}

/// Every `fbv_` index on the values table, by name.
pub(crate) async fn indexes(db: &dyn Backend) -> Result<HashMap<String, IndexInfo>> {
    let rows = db
        .fetch_all(&Statement::raw(
            "SELECT c.relname::text, CAST(x.indisvalid AS int), pg_relation_size(c.oid) \
             FROM pg_index x JOIN pg_class c ON c.oid = x.indexrelid \
             WHERE x.indrelid = CAST('_forgebase_vector_values' AS regclass) AND c.relname LIKE 'fbv\\_%'",
        ))
        .await?;
    Ok(rows
        .rows
        .iter()
        .map(|r| {
            (
                r[0].as_str().unwrap_or_default().to_owned(),
                IndexInfo {
                    valid: r[1].as_i64() == Some(1),
                    bytes: r[2].as_i64().and_then(|n| u64::try_from(n).ok()).unwrap_or(0),
                },
            )
        })
        .collect())
}

/// Runs DDL that must not run in a transaction on the maintenance
/// connection; where the backend has none, its plain form (`plain`) in a
/// schema-change transaction on the writer (writes wait meanwhile).
pub(crate) async fn maintenance(db: &dyn Backend, statements: Vec<String>, plain: Vec<String>) -> Result<()> {
    match db.maintenance(&statements).await {
        Err(DbError::Unsupported(why)) => {
            tracing::warn!(%why, "no maintenance connection; building vector indexes on the writer");
            let mut tx = db.begin(TxMode::SchemaChange).await?;
            for sql in &plain {
                if let Err(e) = tx.execute(&Statement::raw(sql.clone())).await {
                    let _ = tx.rollback().await;
                    return Err(e.into());
                }
            }
            tx.commit().await?;
            Ok(())
        }
        other => Ok(other?),
    }
}

/// Builds the index of `spec` (none for fields too wide): `CREATE INDEX
/// CONCURRENTLY` on the maintenance connection. An invalid leftover of the
/// same name (a build that failed) is dropped first.
pub(crate) async fn build(db: &dyn Backend, pgv: &Pgv, spec: &IndexSpec, application: Option<&str>) -> Result<()> {
    let (Some(create), Some(plain)) = (spec.create_sql(pgv, true), spec.create_sql(pgv, false)) else {
        return Ok(());
    };
    let existing = indexes(db).await?.remove(&spec.name);
    let mut statements = Vec::new();
    if let Some(app) = application {
        statements.push(format!("SET application_name = {}", lit(app)));
    }
    let mut fallback = Vec::new();
    match existing {
        Some(i) if i.valid => {
            statements.push(reindex_sql(&spec.name));
            fallback.push(format!("REINDEX INDEX {}", ident(&spec.name)));
        }
        Some(_) => {
            statements.push(drop_sql(&spec.name, true));
            statements.push(create);
            fallback.push(drop_sql(&spec.name, false));
            fallback.push(plain);
        }
        None => {
            statements.push(create);
            fallback.push(plain);
        }
    }
    maintenance(db, statements, fallback).await
}

/// Drops indexes by name, concurrently.
pub(crate) async fn drop_indexes(db: &dyn Backend, names: &[String]) -> Result<()> {
    for name in names {
        maintenance(db, vec![drop_sql(name, true)], vec![drop_sql(name, false)]).await?;
    }
    Ok(())
}

/// Tuples a running build tagged `application` has added to its index
/// (`pg_stat_progress_create_index`). Read on the writer: the server role
/// sees its own sessions' progress, a separate reader role may not.
pub(crate) async fn build_progress(db: &dyn Backend, application: &str) -> Result<Option<u64>> {
    let mut tx = db.begin(TxMode::Normal).await?;
    let r = tx
        .fetch_all(&Statement::new(
            "SELECT p.tuples_done FROM pg_stat_progress_create_index p \
             JOIN pg_stat_activity a ON a.pid = p.pid WHERE a.application_name = ?1",
            vec![application.into()],
        ))
        .await;
    let _ = tx.rollback().await;
    Ok(r?
        .rows
        .first()
        .and_then(|r| r[0].as_i64())
        .and_then(|n| u64::try_from(n).ok()))
}

// ----------------------------------------------------------------- search

fn filter_join(filter: Option<&Filter>, b: &mut SqlBuilder<'_>) -> Result<(String, String)> {
    match filter {
        None => Ok((String::new(), String::new())),
        Some(f) => {
            let cond = f.compile(b, "i")?;
            Ok((
                format!(" JOIN {ITEMS} i ON i.collection_id = v.collection_id AND i.key = v.key"),
                format!(" AND ({cond})"),
            ))
        }
    }
}

fn scored(rows: &forgebase_db::Rows, metric: Metric) -> Result<Vec<Scored>> {
    let mut out: Vec<Scored> = rows
        .rows
        .iter()
        .map(|r| {
            Ok(Scored {
                key: key(&r[0])?,
                dist: raw(metric, real(&r[1])?),
            })
        })
        .collect::<Result<_>>()?;
    // relaxed_order may return neighbours slightly out of order.
    out.sort();
    Ok(out)
}

/// Vectors of `field` (of items matching `filter`), counted up to `limit`.
pub(crate) async fn count_upto(
    db: &dyn Backend,
    cid: i64,
    field: &str,
    filter: Option<&Filter>,
    limit: u64,
) -> Result<u64> {
    let mut b = SqlBuilder::new(db.dialect());
    let p_cid = b.bind(cid);
    let p_field = b.bind(field);
    let (join, cond) = filter_join(filter, &mut b)?;
    let p_limit = b.bind(int_of(limit));
    let rows = db
        .fetch_all(&Statement::new(
            format!(
                "SELECT count(*) FROM (SELECT 1 FROM _forgebase_vector_values v{join} \
                 WHERE v.collection_id = {p_cid} AND v.field = {p_field}{cond} LIMIT {p_limit}) t"
            ),
            b.into_params(),
        ))
        .await?;
    Ok(rows
        .scalar()
        .and_then(Value::as_i64)
        .and_then(|n| u64::try_from(n).ok())
        .unwrap_or(0))
}

/// Exact top `k`: ordered by the distance on the uncast column, which no
/// index matches.
pub(crate) async fn exact(
    db: &dyn Backend,
    pgv: &Pgv,
    cid: i64,
    f: &VectorField,
    q: &[f32],
    k: usize,
    filter: Option<&Filter>,
) -> Result<Vec<Scored>> {
    let mut b = SqlBuilder::new(db.dialect());
    let p_q = b.bind(text_of(q));
    let p_cid = b.bind(cid);
    let p_field = b.bind(f.name.as_str());
    let (join, cond) = filter_join(filter, &mut b)?;
    let p_k = b.bind(int_of(k as u64));
    let d = pgv.distance(f.metric, "v.vec", &pgv.vector_param(&p_q));
    let rows = db
        .fetch_all(&Statement::new(
            format!(
                "SELECT v.key, {d} AS d FROM _forgebase_vector_values v{join} \
                 WHERE v.collection_id = {p_cid} AND v.field = {p_field}{cond} ORDER BY d, v.key LIMIT {p_k}"
            ),
            b.into_params(),
        ))
        .await?;
    scored(&rows, f.metric)
}

/// Settings of one index scan.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Scan {
    /// `hnsw.ef_search`.
    pub ef_search: u32,
    /// `hnsw.iterative_scan = relaxed_order` (pgvector 0.8+).
    pub iterative: bool,
    /// `hnsw.max_scan_tuples` of an iterative scan.
    pub max_scan_tuples: u64,
}

/// The `set_config` subquery `cfg` and the condition that ties the scan to
/// it (so the settings are made before the index scan starts).
fn settings(pgv: &Pgv, scan: Scan, b: &mut SqlBuilder<'_>) -> (String, String) {
    let ef = b.bind(scan.ef_search.clamp(1, MAX_EF_SEARCH).to_string());
    let mut cols = vec![format!("set_config('hnsw.ef_search', {ef}, true) AS ef")];
    if pgv.iterative {
        let it = b.bind(if scan.iterative { "relaxed_order" } else { "off" });
        let mst = b.bind(scan.max_scan_tuples.max(1).to_string());
        cols.push(format!("set_config('hnsw.iterative_scan', {it}, true) AS it"));
        cols.push(format!("set_config('hnsw.max_scan_tuples', {mst}, true) AS mst"));
    }
    let tie = cols
        .iter()
        .map(|c| format!("cfg.{} IS NOT NULL", c.rsplit(' ').next().unwrap_or("ef")))
        .collect::<Vec<_>>()
        .join(" AND ");
    (format!("(SELECT {}) cfg", cols.join(", ")), tie)
}

/// The ANN statement of [`ann`] (also for plan checks).
#[allow(clippy::too_many_arguments)]
pub(crate) fn ann_statement(
    db: &dyn Backend,
    pgv: &Pgv,
    spec: &IndexSpec,
    q: &[f32],
    k: usize,
    filter: Option<&Filter>,
    scan: Scan,
) -> Result<Option<Statement>> {
    let mut b = SqlBuilder::new(db.dialect());
    let (cfg, tie) = settings(pgv, scan, &mut b);
    let p_q = b.bind(text_of(q));
    let Some(cast_col) = spec.cast(pgv, "v.vec") else {
        return Ok(None);
    };
    let Some(cast_q) = spec.cast(pgv, &format!("CAST({p_q} AS text)")) else {
        return Ok(None);
    };
    let (join, cond) = filter_join(filter, &mut b)?;
    let p_k = b.bind(int_of(k as u64));
    let d = pgv.distance(spec.metric, "v.vec", &pgv.vector_param(&p_q));
    let order = format!("{cast_col} {} {cast_q}", pgv.op(spec.metric));
    Ok(Some(Statement::new(
        format!(
            "SELECT s.key, s.d FROM {cfg} CROSS JOIN LATERAL (\
             SELECT v.key, {d} AS d FROM _forgebase_vector_values v{join} \
             WHERE {} AND {tie}{cond} ORDER BY {order} LIMIT {p_k}) s",
            spec.predicate("v.")
        ),
        b.into_params(),
    )))
}

/// Top `k` through the field's HNSW index, the filter in the same `WHERE`.
pub(crate) async fn ann(
    db: &dyn Backend,
    pgv: &Pgv,
    spec: &IndexSpec,
    q: &[f32],
    k: usize,
    filter: Option<&Filter>,
    scan: Scan,
) -> Result<Vec<Scored>> {
    let Some(stmt) = ann_statement(db, pgv, spec, q, k, filter, scan)? else {
        return Ok(Vec::new());
    };
    scored(&db.fetch_all(&stmt).await?, spec.metric)
}

/// Keys of every vector of a field in key order (recall samples).
pub(crate) async fn keys(db: &dyn Backend, cid: i64, field: &str) -> Result<Vec<u64>> {
    let rows = db
        .fetch_all(&Statement::new(
            "SELECT key FROM _forgebase_vector_values WHERE collection_id = ?1 AND field = ?2 ORDER BY key",
            vec![cid.into(), field.into()],
        ))
        .await?;
    rows.rows.iter().map(|r| key(&r[0])).collect()
}

/// The ANN side of a recall check: for every sample (a stored vector used
/// as the query) its top `k` through the index, as `(sample, key)`.
pub(crate) async fn recall_ann(
    db: &dyn Backend,
    pgv: &Pgv,
    spec: &IndexSpec,
    samples: &[u64],
    k: usize,
    ef_search: u32,
) -> Result<Vec<(u64, u64)>> {
    let mut b = SqlBuilder::new(db.dialect());
    let scan = Scan {
        ef_search,
        iterative: false,
        max_scan_tuples: DEFAULT_MAX_SCAN_TUPLES,
    };
    let (cfg, tie) = settings(pgv, scan, &mut b);
    let p_samples = b.bind(keys_json(samples));
    let p_k = b.bind(int_of(k as u64));
    let (Some(cast_w), Some(cast_s)) = (spec.cast(pgv, "w.vec"), spec.cast(pgv, "v.vec")) else {
        return Ok(Vec::new());
    };
    let rows = db
        .fetch_all(&Statement::new(
            format!(
                "SELECT q.key, a.key FROM {cfg} \
                 CROSS JOIN LATERAL (SELECT v.key, {cast_s} AS qv FROM _forgebase_vector_values v \
                   WHERE {} AND {tie} AND v.key = ANY (SELECT CAST(jsonb_array_elements_text(CAST({p_samples} AS jsonb)) AS bigint))) q \
                 CROSS JOIN LATERAL (SELECT w.key FROM _forgebase_vector_values w WHERE {} \
                   ORDER BY {cast_w} {} q.qv LIMIT {p_k}) a",
                spec.predicate("v."),
                spec.predicate("w."),
                pgv.op(spec.metric)
            ),
            b.into_params(),
        ))
        .await?;
    rows.rows.iter().map(|r| Ok((key(&r[0])?, key(&r[1])?))).collect()
}

/// The exact side of a recall check and the comparison, in SQL: per
/// sample, the exact top `k` on the uncast column, its `k`-th distance, and
/// how many of the ANN results (`ann`, from [`recall_ann`]) are at least as
/// close (equal distances tie, whichever key won). Returns the mean recall.
pub(crate) async fn recall_exact(
    db: &dyn Backend,
    pgv: &Pgv,
    cid: i64,
    f: &VectorField,
    samples: &[u64],
    ann: &[(u64, u64)],
    k: usize,
) -> Result<f64> {
    let mut b = SqlBuilder::new(db.dialect());
    let p_cid = b.bind(cid);
    let p_field = b.bind(f.name.as_str());
    let p_samples = b.bind(keys_json(samples));
    let pairs: Vec<serde_json::Value> = ann.iter().map(|(s, k)| json!({"s": s, "k": k})).collect();
    let p_ann = b.bind(Value::Text(serde_json::Value::Array(pairs).to_string()));
    let p_k = b.bind(int_of(k as u64));
    let d_exact = pgv.distance(f.metric, "w.vec", "q.vec");
    let d_hit = pgv.distance(f.metric, "h.vec", "q.vec");
    let rows = db
        .fetch_all(&Statement::new(
            format!(
                "SELECT avg(CAST(least(hit, n) AS float8) / greatest(n, 1)) FROM (\
                   SELECT e.n, (SELECT count(*) FROM jsonb_to_recordset(CAST({p_ann} AS jsonb)) AS x(s bigint, k bigint) \
                     JOIN _forgebase_vector_values h ON h.collection_id = {p_cid} AND h.field = {p_field} AND h.key = x.k \
                     WHERE x.s = q.key AND {d_hit} <= e.kth + 1e-5 * greatest(abs(e.kth), 1)) AS hit \
                   FROM _forgebase_vector_values q \
                   CROSS JOIN LATERAL (SELECT count(*) AS n, max(t.d) AS kth FROM (\
                     SELECT {d_exact} AS d FROM _forgebase_vector_values w \
                     WHERE w.collection_id = {p_cid} AND w.field = {p_field} ORDER BY d LIMIT {p_k}) t) e \
                   WHERE q.collection_id = {p_cid} AND q.field = {p_field} \
                   AND q.key = ANY (SELECT CAST(jsonb_array_elements_text(CAST({p_samples} AS jsonb)) AS bigint))) r"
            ),
            b.into_params(),
        ))
        .await?;
    Ok(rows.rows.first().map(|r| real(&r[0])).transpose()?.unwrap_or(0.0))
}

// ------------------------------------------------------------- conversion

/// Most `f32` values moved per conversion batch (bounds the statement).
const BATCH_FLOATS: usize = 2_000_000;

/// Moves up to `limit` vectors to `to`'s column in one transaction (bytea
/// -> `vec`, or back). With `pin`, a batch that moves fewer than `limit`
/// (the last one) also records `to` as the stored engine, in the same
/// transaction (FB-161). Returns how many moved and whether it was the
/// last batch.
pub(crate) async fn move_batch(
    db: &dyn Backend,
    pgv: &Pgv,
    to: VectorEngine,
    limit: usize,
    pin: bool,
) -> Result<(u64, bool)> {
    let mut tx = db.begin(TxMode::Normal).await?;
    let r = async {
        let (select, set) = match to {
            VectorEngine::Pgvector => (
                "SELECT collection_id, field, key, vector FROM _forgebase_vector_values \
                 WHERE vector IS NOT NULL ORDER BY collection_id, field, key LIMIT ?1 FOR UPDATE",
                format!("vec = CAST(x.v AS {}), vector = NULL", pgv.ty("vector")),
            ),
            VectorEngine::Portable => (
                "SELECT collection_id, field, key, CAST(vec AS text) FROM _forgebase_vector_values \
                 WHERE vec IS NOT NULL ORDER BY collection_id, field, key LIMIT ?1 FOR UPDATE",
                "vector = decode(x.v, 'hex'), vec = NULL".to_owned(),
            ),
        };
        let rows = tx.fetch_all(&Statement::new(select, vec![int_of(limit as u64)])).await?;
        let mut batch = Vec::with_capacity(rows.rows.len());
        for r in &rows.rows {
            let v = match (to, &r[3]) {
                (VectorEngine::Pgvector, Value::Blob(bytes)) => {
                    let (chunks, _) = bytes.as_chunks::<4>();
                    let floats: Vec<f32> = chunks.iter().map(|c| f32::from_le_bytes(*c)).collect();
                    text_of(&floats)
                }
                (VectorEngine::Portable, t) => {
                    let floats = parse_text(t.as_str().ok_or_else(|| bad("vec"))?)?;
                    metric::encode(&floats).iter().fold(String::new(), |mut s, b| {
                        let _ = write!(s, "{b:02x}");
                        s
                    })
                }
                _ => return Err(bad("vector")),
            };
            batch.push(json!({"c": int(&r[0], "collection_id")?, "f": r[1].as_str().unwrap_or_default(), "k": int(&r[2], "key")?, "v": v}));
        }
        let n = batch.len() as u64;
        if n > 0 {
            tx.execute(&Statement::new(
                format!(
                    "UPDATE _forgebase_vector_values t SET {set} \
                     FROM jsonb_to_recordset(CAST(?1 AS jsonb)) AS x(c bigint, f text, k bigint, v text) \
                     WHERE t.collection_id = x.c AND t.field = x.f AND t.key = x.k"
                ),
                vec![Value::Text(serde_json::Value::Array(batch).to_string())],
            ))
            .await?;
        }
        let last = (n as usize) < limit;
        if pin && last {
            write_pin(&mut *tx, to).await?;
        }
        Ok((n, last))
    }
    .await;
    match r {
        Ok(v) => {
            tx.commit().await?;
            Ok(v)
        }
        Err(e) => {
            let _ = tx.rollback().await;
            Err(e)
        }
    }
}

/// Rows per batch for vectors of up to `max_dim` dimensions.
pub(crate) fn batch_rows(configured: usize, max_dim: u32) -> usize {
    configured.clamp(1, (BATCH_FLOATS / max_dim.max(1) as usize).max(1))
}

/// Moves every vector to `to`'s column; with `pin`, the last batch records
/// the engine. Returns the vectors moved.
pub(crate) async fn move_all(db: &dyn Backend, pgv: &Pgv, to: VectorEngine, batch: usize, pin: bool) -> Result<u64> {
    let mut moved = 0;
    loop {
        let (n, last) = move_batch(db, pgv, to, batch, pin).await?;
        moved += n;
        if last {
            return Ok(moved);
        }
    }
}

/// The largest dimension of any field (sizes the conversion batches).
pub(crate) async fn max_dimension(db: &dyn Backend) -> Result<u32> {
    let rows = db
        .fetch_all(&Statement::raw(
            "SELECT COALESCE(MAX(dimension), 1) FROM _forgebase_vector_fields",
        ))
        .await?;
    Ok(rows
        .scalar()
        .and_then(Value::as_i64)
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(dim: u32, q: Quantization, metric: Metric) -> VectorField {
        VectorField {
            name: "emb".into(),
            dimension: dim,
            metric,
            quantization: q,
            connectivity: 16,
            expansion_add: 128,
            expansion_search: 128,
            created: "2026-11-11T00:00:00Z".into(),
        }
    }

    fn pgv() -> Pgv {
        Pgv {
            schema: "\"forgebase\"".into(),
            version: "0.8.7".into(),
            iterative: true,
        }
    }

    #[test]
    fn element_types_follow_the_dimension_limits() {
        assert_eq!(elem_of(2000, Quantization::F32), Some(Elem::Vector));
        assert_eq!(elem_of(2001, Quantization::F32), Some(Elem::Halfvec));
        assert_eq!(elem_of(4000, Quantization::F32), Some(Elem::Halfvec));
        assert_eq!(elem_of(4001, Quantization::F32), None);
        assert_eq!(elem_of(8, Quantization::F16), Some(Elem::Halfvec));
        assert_eq!(elem_of(8, Quantization::I8), Some(Elem::Halfvec));
        assert_eq!(elem_of(4096, Quantization::I8), None);
    }

    #[test]
    fn index_ddl_names_the_cast_metric_and_predicate() {
        let s = spec(7, &field(16, Quantization::F32, Metric::Cosine));
        assert!(
            s.name.starts_with("fbv_7_") && s.name.len() == "fbv_7_".len() + 12,
            "{}",
            s.name
        );
        let sql = s.create_sql(&pgv(), true).unwrap();
        assert_eq!(
            sql,
            format!(
                "CREATE INDEX CONCURRENTLY IF NOT EXISTS \"{}\" ON _forgebase_vector_values USING hnsw \
                 ((CAST(vec AS \"forgebase\".vector(16))) \"forgebase\".vector_cosine_ops) \
                 WITH (m = 16, ef_construction = 128) WHERE collection_id = 7 AND field = 'emb'",
                s.name
            )
        );
        let wide = spec(7, &field(3000, Quantization::F32, Metric::Dot));
        assert!(
            wide.create_sql(&pgv(), false)
                .unwrap()
                .contains("halfvec(3000))) \"forgebase\".halfvec_ip_ops")
        );
        let l2 = spec(7, &field(8, Quantization::I8, Metric::L2));
        assert!(l2.create_sql(&pgv(), false).unwrap().contains("halfvec_l2_ops"));
        assert!(
            spec(7, &field(4096, Quantization::F32, Metric::L2))
                .create_sql(&pgv(), true)
                .is_none()
        );
        // Another incarnation of the field (or other parameters): another name.
        let mut again = field(16, Quantization::F32, Metric::Cosine);
        again.created = "2026-11-12T00:00:00Z".into();
        assert_ne!(spec(7, &again).name, s.name);
    }

    #[test]
    fn hnsw_parameters_are_clamped_to_pgvector_limits() {
        let mut f = field(8, Quantization::F32, Metric::L2);
        f.connectivity = 128;
        f.expansion_add = 8;
        let s = spec(1, &f);
        assert_eq!((s.m, s.ef_construction), (100, 200));
        f.connectivity = 4;
        f.expansion_add = 4096;
        let s = spec(1, &f);
        assert_eq!((s.m, s.ef_construction), (4, 1000));
    }

    #[test]
    fn vectors_round_trip_through_the_text_form() {
        let v = vec![0.1f32, -2.5, 1e-7, 3.402_823_5e38, 1.0 / 3.0];
        assert_eq!(parse_text(&text_of(&v)).unwrap(), v);
        assert_eq!(parse_text("[1, 2,3]").unwrap(), vec![1.0, 2.0, 3.0]);
        assert!(parse_text("1,2").is_err());
    }

    #[test]
    fn versions_compare_numerically() {
        assert!(parse_version("0.8.7") >= ITERATIVE_VERSION);
        assert!(parse_version("0.10.0") > parse_version("0.8.7"));
        assert!(parse_version("0.6.2") < MIN_VERSION);
    }
}
