//! [`VectorService`]: collections, items, similarity and hybrid search,
//! index rebuilds and recall checks.
//!
//! Consistency model (ADR-0012 §3): SQLite is the source of truth. A write
//! takes the collection's write lock, commits to SQLite, then applies the same
//! change to the in-memory ANN index; the lock makes "committed" and
//! "applied" advance together, so an index saved under it reflects exactly
//! the change sequence it records. If applying fails, the index is dropped and
//! rebuilt; searches meanwhile are exact. A rebuild reads the canonical
//! vectors without blocking writes: writes made while it runs are recorded
//! and replayed from SQLite before the new index replaces the old one.
//!
//! Every public method takes an [`AccessContext`], asks the data core's
//! authorizer ([`Action::VectorRead`], [`Action::VectorWrite`],
//! [`Action::VectorAdmin`]) and works inside the caller's tenant only.
//!
//! Access (FB-139, rls-coverage G9). Calls on one collection ask
//! `authorize_resource` for the `vector_collection` resource with the
//! operation of the call (searches `list`, item and definition reads `view`,
//! upserts `create` for a new item and `update` for an existing one,
//! deletions `delete`; administration only by permission). A rule that comes
//! back as a predicate over the item fields (`id`, `metadata`) is compiled
//! into every statement that reads the items, so searches are pre-filtered;
//! writes check the stored row (update, delete) and the written row (create,
//! update) before they commit. Callers without a tenant work in the shared
//! `''` scope only when privileged; anyone else gets `tenant_required`
//! (after authorisation, so a denied caller learns nothing). A collection
//! with a `source_collection` (items keyed by record id) re-checks every hit
//! against that collection's View rule as the caller, drops the metadata keys
//! the caller may not read there and refuses filters on them.
//!
//! Storage engines (ADR-0029 §1.4, FB-162). On PostgreSQL whose
//! `_forgebase_meta` `ext.vector.engine` is `extension` (FB-161's pin) and
//! whose pgvector passes the layout check ([`crate::pg::probe`]), the
//! canonical vectors live in the `vec` column and every field has a partial
//! HNSW index in PostgreSQL: no index files, no in-memory index, no save
//! loop, no warm-up. Anything else (SQLite, PostgreSQL without pgvector, a
//! failed layout check, which logs a warning) uses the portable engine
//! above. The engine is resolved when the service opens, and stray vectors
//! of an interrupted conversion are moved to it then (reconcile); every
//! operation holds the engine's read lock, [`VectorService::convert`] its
//! write lock.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once, RwLock};
use std::time::{Duration, Instant};

use forgebase_data::query::QueryError;
use forgebase_data::{
    AccessContext, Action, DataError, DataService, GetOptions, Predicate, Principal, ResourceKind, ResourceOp,
    ResourceRef,
};
use forgebase_db::{Backend, TxMode, WriteTx};
use serde_json::Map;
use tokio::task::JoinHandle;

use crate::engine::{AnnEngine, AnnIndex, Usearch};
use crate::error::VectorError;
use crate::filter::{Filter, ItemRule, item_schema};
use crate::index::FieldIndex;
use crate::metric::{self, Scored, TopK};
use crate::model::*;
use forgebase_db::{ReadIntent, ReadVia};

use crate::pg::{self, Pgv};
use crate::store::{self, CollectionRow};

type Result<T> = std::result::Result<T, VectorError>;

/// Collection option naming the data collection the items were copied from
/// (item id = record id), FB-139.
pub const SOURCE_COLLECTION_OPTION: &str = "source_collection";

/// Default HNSW links per node.
pub const DEFAULT_CONNECTIVITY: u32 = 16;
/// Default HNSW insertion candidate list.
pub const DEFAULT_EXPANSION_ADD: u32 = 128;
/// Default HNSW search candidate list.
pub const DEFAULT_EXPANSION_SEARCH: u32 = 128;

/// Engine settings.
#[derive(Debug, Clone)]
pub struct VectorConfig {
    /// Default of [`NewCollection::exact_threshold`]: searches over at most
    /// this many candidates are exact.
    pub exact_threshold: u32,
    /// Most vectors an exact search scans (explicit `exact`, fallback while an
    /// index is rebuilt, recall checks). Above it: `vector_index_not_ready`.
    pub max_exact_scan: u64,
    /// Most matching items a pre-filter collects; above it the filter is
    /// applied after the search instead.
    pub max_prefilter: usize,
    /// How often changed indexes are written to disk (also at shutdown and
    /// after each rebuild).
    pub save_interval: Duration,
    /// Vectors read from SQLite per batch while rebuilding or scanning.
    pub batch: usize,
}

impl Default for VectorConfig {
    fn default() -> Self {
        Self {
            exact_threshold: 1000,
            max_exact_scan: 200_000,
            max_prefilter: 500_000,
            save_interval: Duration::from_secs(60),
            batch: 2000,
        }
    }
}

/// The vector engine. Cheap to clone.
#[derive(Debug, Clone)]
pub struct VectorService {
    inner: Arc<Inner>,
}

/// Where the vectors live (FB-162).
#[derive(Debug, Clone)]
enum Storage {
    Portable,
    Pgvector(Arc<Pgv>),
}

impl Storage {
    fn pg(&self) -> Option<&Pgv> {
        match self {
            Storage::Portable => None,
            Storage::Pgvector(p) => Some(p),
        }
    }

    fn kind(&self) -> VectorEngine {
        match self {
            Storage::Portable => VectorEngine::Portable,
            Storage::Pgvector(_) => VectorEngine::Pgvector,
        }
    }
}

#[derive(Debug)]
struct Inner {
    data: DataService,
    engine: Arc<dyn AnnEngine>,
    root: PathBuf,
    config: VectorConfig,
    collections: RwLock<HashMap<(String, String), Arc<Coll>>>,
    /// The storage engine: read-locked by every operation for its whole
    /// run, write-locked by a conversion.
    storage: tokio::sync::RwLock<Storage>,
    /// Serialises creating and dropping collections.
    catalog: tokio::sync::Mutex<()>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    stopping: AtomicBool,
}

#[derive(Debug)]
struct Coll {
    row: CollectionRow,
    exact_threshold: u32,
    /// Held across commit + index apply, saves, loads and rebuild hand-over.
    write: tokio::sync::Mutex<()>,
    fields: RwLock<BTreeMap<String, Arc<FieldIndex>>>,
    dropped: AtomicBool,
}

impl Coll {
    fn fields(&self) -> Vec<Arc<FieldIndex>> {
        self.fields
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .values()
            .cloned()
            .collect()
    }

    fn field(&self, name: &str) -> Result<Arc<FieldIndex>> {
        self.fields
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .get(name)
            .cloned()
            .ok_or_else(|| VectorError::FieldNotFound(name.to_owned()))
    }

    /// `name`, or the only field when `name` is absent.
    fn pick(&self, name: Option<&str>) -> Result<Arc<FieldIndex>> {
        if let Some(n) = name {
            return self.field(n);
        }
        let fields = self.fields.read().unwrap_or_else(|p| p.into_inner());
        match fields.len() {
            1 => Ok(fields
                .values()
                .next()
                .cloned()
                .ok_or_else(|| VectorError::FieldNotFound(String::new()))?),
            0 => Err(VectorError::invalid(
                "no_vector_field",
                "the collection has no vector field",
            )),
            _ => Err(VectorError::invalid(
                "field_required",
                "the collection has several vector fields; name one in `field`",
            )),
        }
    }

    fn definition(&self, items: u64, updated: String) -> VectorCollection {
        VectorCollection {
            name: self.row.name.clone(),
            description: self.row.description.clone(),
            fields: self.fields().iter().map(|f| f.def.clone()).collect(),
            exact_threshold: self.exact_threshold,
            source_collection: self.source().map(str::to_owned),
            items,
            created: self.row.created.clone(),
            updated,
        }
    }

    fn source(&self) -> Option<&str> {
        self.row
            .options
            .get(SOURCE_COLLECTION_OPTION)
            .and_then(serde_json::Value::as_str)
    }
}

/// The record re-check of a collection with a `source_collection` (FB-139).
#[derive(Debug)]
struct SourceCheck {
    collection: String,
    /// Lower-case record fields the caller may not read.
    unreadable: BTreeSet<String>,
    /// Item id -> whether the caller may view the record.
    seen: HashMap<String, bool>,
}

impl SourceCheck {
    /// Refuses a filter on metadata keys the caller may not read in the
    /// source collection (the same error as for an unknown field).
    fn check_filter(&self, filter: Option<&Filter>) -> Result<()> {
        if let Some(k) = filter.and_then(|f| f.user_keys().into_iter().find(|k| self.unreadable.contains(k))) {
            return Err(VectorError::Filter(QueryError::UnknownField(k)));
        }
        Ok(())
    }

    fn strip(&self, metadata: &mut Map<String, serde_json::Value>) {
        if !self.unreadable.is_empty() {
            metadata.retain(|k, _| !self.unreadable.contains(&k.to_ascii_lowercase()));
        }
    }
}

fn now() -> String {
    forgebase_data::schema::now_timestamp()
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn strategy_name(s: Strategy) -> &'static str {
    match s {
        Strategy::Ann => "ann",
        Strategy::FilteredAnn => "filtered_ann",
        Strategy::PostFilter => "post_filter",
        Strategy::Exact => "exact",
        Strategy::ExactFallback => "exact_fallback",
    }
}

fn describe_metrics() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        metrics::describe_counter!("forgebase_vector_searches_total", "Vector searches, by strategy.");
        metrics::describe_histogram!(
            "forgebase_vector_search_duration_seconds",
            metrics::Unit::Seconds,
            "Vector search latency."
        );
        metrics::describe_counter!("forgebase_vector_upserts_total", "Vector items upserted.");
        metrics::describe_counter!("forgebase_vector_deletes_total", "Vector items deleted.");
        metrics::describe_counter!("forgebase_vector_rebuilds_total", "ANN index rebuilds, by reason.");
        metrics::describe_gauge!("forgebase_vector_rebuilds_running", "ANN index rebuilds in progress.");
    });
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| VectorError::Internal(format!("blocking task failed: {e}")))?
}

async fn finish<T>(tx: Box<dyn WriteTx>, r: Result<T>) -> Result<T> {
    match r {
        Ok(v) => {
            tx.commit().await?;
            Ok(v)
        }
        Err(e) => {
            if let Err(rb) = tx.rollback().await {
                tracing::warn!(error = %rb, "rollback failed; writer connection discarded");
            }
            Err(e)
        }
    }
}

fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// The FTS5 query for keyword text: every word quoted (operators in the input
/// are literal), joined with `OR` or `AND`.
pub(crate) fn match_expr(text: &str, op: TextOperator) -> Result<String> {
    if text.len() > 1024 {
        return Err(VectorError::invalid(
            "invalid_text_query",
            "text query is longer than 1024 bytes",
        ));
    }
    let words: Vec<String> = text
        .split_whitespace()
        .filter(|w| w.chars().any(char::is_alphanumeric))
        .map(|w| format!("\"{}\"", w.replace('"', "\"\"")))
        .collect();
    if words.is_empty() {
        return Err(VectorError::invalid("invalid_text_query", "text query has no words"));
    }
    if words.len() > 64 {
        return Err(VectorError::invalid(
            "invalid_text_query",
            "text query has more than 64 words",
        ));
    }
    Ok(words.join(match op {
        TextOperator::Or => " OR ",
        TextOperator::And => " AND ",
    }))
}

/// The keyword query for the backend: FTS5 syntax on SQLite, a
/// `websearch_to_tsquery` string on PostgreSQL (quoted words, `or` between
/// them for [`TextOperator::Or`], a space for [`TextOperator::And`]).
pub(crate) fn match_query(dialect: &dyn forgebase_db::Dialect, text: &str, op: TextOperator) -> Result<String> {
    let fts5 = match_expr(text, op)?;
    if dialect.name() != "postgres" {
        return Ok(fts5);
    }
    let words: Vec<String> = text
        .split_whitespace()
        .filter(|w| w.chars().any(char::is_alphanumeric))
        .map(|w| format!("\"{}\"", w.replace('"', " ").trim()))
        .collect();
    Ok(words.join(match op {
        TextOperator::Or => " or ",
        TextOperator::And => " ",
    }))
}

/// Reciprocal Rank Fusion of two rankings (keys best first).
pub(crate) fn rrf(vector: &[u64], text: &[u64], rrf_k: f64, w: HybridWeights) -> Vec<(u64, f64)> {
    let mut scores: HashMap<u64, f64> = HashMap::new();
    for (rank, k) in vector.iter().enumerate() {
        #[allow(clippy::cast_precision_loss)]
        let r = (rank + 1) as f64;
        *scores.entry(*k).or_default() += w.vector / (rrf_k + r);
    }
    for (rank, k) in text.iter().enumerate() {
        #[allow(clippy::cast_precision_loss)]
        let r = (rank + 1) as f64;
        *scores.entry(*k).or_default() += w.text / (rrf_k + r);
    }
    let mut out: Vec<(u64, f64)> = scores.into_iter().collect();
    out.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    out
}

/// The caller's decision for one write operation.
#[derive(Debug)]
enum Gate {
    Allow,
    Filter(Box<Filter>),
    Deny,
}

struct Knn {
    hits: Vec<Scored>,
    strategy: Strategy,
    candidates: Option<u64>,
}

impl VectorService {
    /// Opens the engine on a migrated database. `indexes_dir` is the data
    /// directory's `indexes/`; files go to its `vector/` subdirectory.
    /// Indexes are loaded lazily (first use) or by [`Self::start_background`].
    ///
    /// On PostgreSQL it first resolves the storage engine (FB-162): the
    /// pinned `ext.vector.engine` and pgvector's layout decide; vectors an
    /// interrupted conversion left in the other column are moved to the
    /// engine in use, and on pgvector missing or invalid HNSW indexes are
    /// built in the background (searches are exact meanwhile) and indexes
    /// of dropped fields removed.
    pub async fn open(data: DataService, indexes_dir: PathBuf, config: VectorConfig) -> Result<Self> {
        describe_metrics();
        data.register_resource_schema(item_schema());
        let root = indexes_dir.join("vector");
        std::fs::create_dir_all(&root)?;
        let backend = Arc::clone(data.backend());
        let storage = Self::resolve(backend.as_ref(), config.batch).await?;
        let pg_indexes = match &storage {
            Storage::Pgvector(_) => pg::indexes(backend.as_ref()).await?,
            Storage::Portable => HashMap::new(),
        };
        let rows = store::load_all(backend.as_ref()).await?;
        let mut collections = HashMap::new();
        for (row, fields) in rows {
            let dir = root.join(row.id.to_string());
            let exact_threshold = row
                .options
                .get("exact_threshold")
                .and_then(serde_json::Value::as_u64)
                .and_then(|n| u32::try_from(n).ok())
                .unwrap_or(config.exact_threshold);
            let map = fields
                .into_iter()
                .map(|f| {
                    let idx = match &storage {
                        Storage::Portable => FieldIndex::unloaded(f, &dir),
                        Storage::Pgvector(_) => {
                            let valid = pg_indexes.get(&pg::spec(row.id, &f).name).is_some_and(|i| i.valid);
                            FieldIndex::on_postgres(f, &dir, valid)
                        }
                    };
                    (idx.def.name.clone(), Arc::new(idx))
                })
                .collect();
            collections.insert(
                (row.tenant.clone(), row.name.clone()),
                Arc::new(Coll {
                    row,
                    exact_threshold,
                    write: tokio::sync::Mutex::new(()),
                    fields: RwLock::new(map),
                    dropped: AtomicBool::new(false),
                }),
            );
        }
        let svc = Self {
            inner: Arc::new(Inner {
                data,
                engine: Arc::new(Usearch),
                root,
                config,
                collections: RwLock::new(collections),
                storage: tokio::sync::RwLock::new(storage.clone()),
                catalog: tokio::sync::Mutex::new(()),
                tasks: Mutex::new(Vec::new()),
                stopping: AtomicBool::new(false),
            }),
        };
        match &storage {
            Storage::Pgvector(_) => svc.repair_indexes(&pg_indexes),
            Storage::Portable if pg::has_vec_column(backend.as_ref()).await? => {
                // HNSW indexes of a pgvector past (a conversion to portable
                // that stopped before dropping them) only slow writes down.
                let names: Vec<String> = pg::indexes(backend.as_ref()).await?.into_keys().collect();
                if !names.is_empty() {
                    let db = Arc::clone(&backend);
                    svc.track(tokio::spawn(async move {
                        if let Err(e) = pg::drop_indexes(db.as_ref(), &names).await {
                            tracing::warn!(error = %e, "dropping leftover pgvector indexes failed");
                        }
                    }));
                }
            }
            Storage::Portable => {}
        }
        Ok(svc)
    }

    /// The storage engine of this database (FB-162). See the module docs.
    async fn resolve(db: &dyn Backend, batch: usize) -> Result<Storage> {
        if db.dialect().name() != "postgres" {
            return Ok(Storage::Portable);
        }
        let pinned = pg::stored_engine(db).await?.as_deref() == Some("extension");
        let probe = pg::probe(db).await?;
        let rows = pg::batch_rows(batch, pg::max_dimension(db).await?);
        if !pinned {
            // Vectors a conversion to pgvector left in `vec` before its last
            // batch (which records the engine) go back.
            if let Ok(pgv) = &probe {
                let n = pg::move_all(db, pgv, VectorEngine::Portable, rows, false).await?;
                if n > 0 {
                    tracing::info!(
                        vectors = n,
                        "vectors of an unfinished conversion moved back to the portable engine"
                    );
                }
            }
            return Ok(Storage::Portable);
        }
        match probe {
            Err(problems) => {
                tracing::warn!(
                    ?problems,
                    "this database keeps its vectors in pgvector, but pgvector cannot be used as FB-162 expects; \
                     vectors use the portable engine"
                );
                Ok(Storage::Portable)
            }
            Ok(pgv) => {
                // Vectors written portably under a pgvector pin (databases set
                // up between FB-161 and FB-162, an unfinished conversion back).
                let n = pg::move_all(db, &pgv, VectorEngine::Pgvector, rows, false).await?;
                if n > 0 {
                    tracing::info!(vectors = n, "portable vectors moved to pgvector");
                }
                Ok(Storage::Pgvector(Arc::new(pgv)))
            }
        }
    }

    /// pgvector: builds every missing or invalid index in the background and
    /// drops `fbv_` indexes no field owns (`existing`: what the database has).
    fn repair_indexes(&self, existing: &HashMap<String, pg::IndexInfo>) {
        let mut wanted = HashSet::new();
        for coll in self.all_collections() {
            for f in coll.fields() {
                let spec = pg::spec(coll.row.id, &f.def);
                wanted.insert(spec.name.clone());
                if spec.elem.is_some() && !f.pg_valid.load(Ordering::Relaxed) {
                    let detail = if existing.contains_key(&spec.name) {
                        "the index is invalid (an interrupted build)"
                    } else {
                        "no HNSW index"
                    };
                    self.start_pg_build(&coll, &f, "missing", detail.into());
                }
            }
        }
        let orphans: Vec<String> = existing.keys().filter(|n| !wanted.contains(*n)).cloned().collect();
        if !orphans.is_empty() {
            let db = Arc::clone(self.inner.data.backend());
            self.track(tokio::spawn(async move {
                if let Err(e) = pg::drop_indexes(db.as_ref(), &orphans).await {
                    tracing::warn!(error = %e, "dropping pgvector indexes of dropped fields failed");
                }
            }));
        }
    }

    /// The storage engine in use (FB-162).
    pub async fn storage_engine(&self) -> VectorEngine {
        self.inner.storage.read().await.kind()
    }

    fn db(&self) -> &dyn Backend {
        self.inner.data.backend().as_ref()
    }

    /// The reads of one §2.5 call site (ADR-0029): through the replica
    /// router when `replica`, else the primary. The portable engine's
    /// in-process index follows the primary, so searches over it keep the
    /// store reads there too (FB-169).
    fn rdb(&self, what: &'static str, replica: bool) -> ReadVia<'_> {
        let intent = if replica {
            ReadIntent::replica(what)
        } else {
            ReadIntent::primary(what)
        };
        ReadVia::new(self.db(), intent)
    }

    fn engine(&self) -> Arc<dyn AnnEngine> {
        Arc::clone(&self.inner.engine)
    }

    /// Asks the authorizer for an action over every collection (listing,
    /// tenant statistics): only the plain `engine:vector_<action>` key
    /// grants those.
    pub fn authorize(&self, ctx: &AccessContext, action: Action) -> Result<()> {
        Ok(self.inner.data.authorize_action(ctx, action)?)
    }

    /// Asks the authorizer about one collection without doing anything (the
    /// HTTP layer calls it before reading a body): refused only when the
    /// caller may do nothing of `op` there (a rule that restricts items
    /// passes).
    pub fn authorize_collection(&self, ctx: &AccessContext, action: Action, name: &str, op: ResourceOp) -> Result<()> {
        self.rule(ctx, action, name, op).map(|_| ())
    }

    /// The caller's collection rule for `op` (FB-139): `predicate: None` for
    /// every item, else only items matching it (fields of
    /// [`item_schema`]). Denied callers get the error. For engines that read
    /// items with their own SQL (FB-162, pgvector); compile with
    /// [`ItemRule::compile`].
    pub fn item_rule(&self, ctx: &AccessContext, action: Action, name: &str, op: ResourceOp) -> Result<ItemRule> {
        Ok(ItemRule {
            predicate: self.rule(ctx, action, name, op)?,
        })
    }

    fn rule(&self, ctx: &AccessContext, action: Action, name: &str, op: ResourceOp) -> Result<Option<Predicate>> {
        let r = ResourceRef::new(ResourceKind::VectorCollection, name, op);
        Ok(self.inner.data.authorize_resource(ctx, action, &r)?)
    }

    /// A rule as a filter of the items table (`None`: every item).
    fn rule_filter(&self, rule: Option<Predicate>) -> Result<Option<Filter>> {
        Filter::restrict(self.db().dialect(), None, rule)
    }

    /// Calls that are not about single items (administration, index
    /// statistics) need the permission or an unconditional rule: a rule that
    /// restricts items cannot apply to them.
    fn allow(&self, ctx: &AccessContext, action: Action, name: &str, op: ResourceOp) -> Result<()> {
        match self.rule(ctx, action, name, op)? {
            None => Ok(()),
            Some(_) => Err(Self::denied(
                ctx,
                format!("{action:?} on {name} needs unrestricted access"),
            )),
        }
    }

    /// A write rule as a gate: an error becomes `Deny` (raised only for the
    /// items that need it), a predicate a filter of the items table.
    fn gate(&self, rule: Result<Option<Predicate>>) -> Result<Gate> {
        Ok(match rule {
            Ok(None) => Gate::Allow,
            Ok(Some(p)) => match self.rule_filter(Some(p))? {
                Some(f) => Gate::Filter(Box::new(f)),
                None => Gate::Allow,
            },
            Err(VectorError::Forbidden(_) | VectorError::Unauthenticated) => Gate::Deny,
            Err(e) => return Err(e),
        })
    }

    fn denied(ctx: &AccessContext, msg: String) -> VectorError {
        if ctx.principal == Principal::Anonymous {
            VectorError::Unauthenticated
        } else {
            VectorError::Forbidden(msg)
        }
    }

    /// The caller's tenant scope: its tenant, or the shared `''` scope for
    /// privileged callers without one; anyone else `tenant_required`
    /// (FB-139). Asked after authorisation.
    fn scope(ctx: &AccessContext) -> Result<String> {
        match &ctx.tenant {
            Some(t) => Ok(t.clone()),
            None if ctx.is_privileged() => Ok(String::new()),
            None => Err(VectorError::invalid(
                "tenant_required",
                "select a tenant (X-ForgeBase-Tenant header, or a tenant-bound key)",
            )),
        }
    }

    fn coll(&self, ctx: &AccessContext, name: &str) -> Result<Arc<Coll>> {
        let tenant = Self::scope(ctx)?;
        self.inner
            .collections
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .get(&(tenant, name.to_owned()))
            .cloned()
            .ok_or_else(|| VectorError::CollectionNotFound(name.to_owned()))
    }

    /// The record re-check for `coll`, unless the caller is the system or
    /// the collection has no `source_collection`.
    fn source_check(&self, ctx: &AccessContext, coll: &Coll) -> Result<Option<SourceCheck>> {
        let Some(src) = coll.source().filter(|_| !ctx.is_system()) else {
            return Ok(None);
        };
        let unreadable = match self.inner.data.unreadable_fields(ctx, src) {
            Ok(u) => u,
            // A dropped source collection hides every item (`visible` fails).
            Err(DataError::CollectionNotFound(_)) => BTreeSet::new(),
            Err(e) => return Err(e.into()),
        };
        Ok(Some(SourceCheck {
            collection: src.to_owned(),
            unreadable,
            seen: HashMap::new(),
        }))
    }

    /// Whether the caller may view record `id` of the source collection
    /// (View rule, tenant), fail closed.
    async fn visible(&self, ctx: &AccessContext, sc: &mut SourceCheck, id: &str) -> Result<bool> {
        if let Some(v) = sc.seen.get(id) {
            return Ok(*v);
        }
        let v = match self
            .inner
            .data
            .get_record(ctx, &sc.collection, id, &GetOptions::default())
            .await
        {
            Ok(_) => true,
            Err(DataError::Db(e)) => return Err(VectorError::Db(e)),
            Err(_) => false,
        };
        sc.seen.insert(id.to_owned(), v);
        Ok(v)
    }

    fn all_collections(&self) -> Vec<Arc<Coll>> {
        self.inner
            .collections
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .values()
            .cloned()
            .collect()
    }

    fn track(&self, h: JoinHandle<()>) {
        let mut tasks = self.inner.tasks.lock().unwrap_or_else(|p| p.into_inner());
        tasks.retain(|t| !t.is_finished());
        tasks.push(h);
    }

    // ------------------------------------------------------------ background

    /// Starts the warm-up (loads or rebuilds every index) and the periodic
    /// saver. Call once, inside a Tokio runtime.
    pub fn start_background(&self) {
        let svc = self.clone();
        self.track(tokio::spawn(async move {
            for coll in svc.all_collections() {
                if svc.inner.stopping.load(Ordering::Relaxed) {
                    return;
                }
                // pgvector's indexes live in PostgreSQL: nothing to load.
                let storage = svc.inner.storage.read().await;
                if storage.pg().is_some() {
                    return;
                }
                let _w = coll.write.lock().await;
                if let Err(e) = svc.ensure_loaded(&coll).await {
                    tracing::warn!(collection = %coll.row.name, error = %e, "vector index warm-up failed");
                }
            }
        }));
        let svc = self.clone();
        self.track(tokio::spawn(async move {
            let mut tick = tokio::time::interval(svc.inner.config.save_interval);
            tick.tick().await;
            loop {
                tick.tick().await;
                if svc.inner.stopping.load(Ordering::Relaxed) {
                    return;
                }
                svc.save_all().await;
            }
        }));
    }

    /// Writes every changed index to disk. Returns the number of files
    /// written.
    pub async fn save_all(&self) -> usize {
        let mut n = 0;
        for coll in self.all_collections() {
            let _w = coll.write.lock().await;
            match self.save_locked(&coll).await {
                Ok(k) => n += k,
                Err(e) => tracing::warn!(collection = %coll.row.name, error = %e, "saving vector index failed"),
            }
        }
        n
    }

    async fn save_locked(&self, coll: &Arc<Coll>) -> Result<usize> {
        if coll.dropped.load(Ordering::Relaxed) {
            return Ok(0);
        }
        let (seq, _) = store::change_seq(self.db(), coll.row.id).await?;
        let fields = coll.fields();
        let engine = self.engine();
        blocking(move || {
            let mut n = 0;
            for f in fields {
                if f.save(engine.as_ref(), seq)? {
                    n += 1;
                }
            }
            Ok(n)
        })
        .await
    }

    /// Stops background work and rebuilds, then saves every index. Call
    /// before closing the database.
    pub async fn shutdown(&self) -> usize {
        self.inner.stopping.store(true, Ordering::Relaxed);
        let tasks: Vec<JoinHandle<()>> =
            std::mem::take(&mut *self.inner.tasks.lock().unwrap_or_else(|p| p.into_inner()));
        for t in tasks {
            t.abort();
            let _ = t.await;
        }
        self.save_all().await
    }

    // ---------------------------------------------------- loading, rebuilding

    /// Loads every unloaded index of `coll` from disk, or starts a rebuild
    /// when its file is unusable. Caller holds `coll.write`.
    async fn ensure_loaded(&self, coll: &Arc<Coll>) -> Result<()> {
        let unloaded: Vec<Arc<FieldIndex>> = coll
            .fields()
            .into_iter()
            .filter(|f| f.status().state == IndexState::Unloaded)
            .collect();
        if unloaded.is_empty() {
            return Ok(());
        }
        let (seq, _) = store::change_seq(self.db(), coll.row.id).await?;
        for f in unloaded {
            let count = store::count_vectors(self.db(), coll.row.id, &f.def.name).await?;
            let engine = self.engine();
            let fi = Arc::clone(&f);
            let loaded = blocking(move || Ok(fi.load(engine.as_ref(), seq, count))).await?;
            match loaded {
                Ok(()) => tracing::debug!(collection = %coll.row.name, field = %f.def.name, "vector index loaded"),
                Err(u) => {
                    tracing::info!(
                        collection = %coll.row.name, field = %f.def.name, reason = u.reason, detail = %u.detail,
                        "vector index file not usable; rebuilding from the canonical vectors"
                    );
                    self.start_rebuild_locked(coll, &f, u.reason, u.detail);
                }
            }
        }
        Ok(())
    }

    /// Registers and spawns a rebuild. Caller holds `coll.write`, so every
    /// write after this point is recorded for the hand-over.
    fn start_rebuild_locked(&self, coll: &Arc<Coll>, f: &Arc<FieldIndex>, reason: &'static str, detail: String) {
        let generation = {
            let mut st = f.status();
            if st.state == IndexState::Building {
                return;
            }
            st.state = IndexState::Building;
            st.reason = Some(format!("{reason}: {detail}"));
            st.pending = Some(HashSet::new());
            st.generation += 1;
            st.generation
        };
        f.done.store(0, Ordering::Relaxed);
        f.total.store(0, Ordering::Relaxed);
        metrics::counter!("forgebase_vector_rebuilds_total", "reason" => reason).increment(1);
        metrics::gauge!("forgebase_vector_rebuilds_running").increment(1.0);
        let svc = self.clone();
        let coll = Arc::clone(coll);
        let f = Arc::clone(f);
        self.track(tokio::spawn(async move {
            let started = Instant::now();
            let r = svc.run_rebuild(&coll, &f, generation, reason, &detail, started).await;
            metrics::gauge!("forgebase_vector_rebuilds_running").decrement(1.0);
            if let Err(e) = r {
                tracing::error!(collection = %coll.row.name, field = %f.def.name, error = %e, "vector index rebuild failed");
                let mut st = f.status();
                if st.generation == generation {
                    st.pending = None;
                    if f.live().is_some() {
                        st.state = IndexState::Ready;
                        st.reason = Some(format!("last rebuild failed: {e}"));
                    } else {
                        st.state = IndexState::Failed;
                        st.reason = Some(format!("rebuild failed: {e}"));
                    }
                }
            }
        }));
    }

    fn superseded(&self, coll: &Coll, f: &Arc<FieldIndex>, generation: u64) -> bool {
        self.inner.stopping.load(Ordering::Relaxed)
            || coll.dropped.load(Ordering::Relaxed)
            || f.status().generation != generation
            || coll.field(&f.def.name).map(|cur| !Arc::ptr_eq(&cur, f)).unwrap_or(true)
    }

    async fn run_rebuild(
        &self,
        coll: &Arc<Coll>,
        f: &Arc<FieldIndex>,
        generation: u64,
        reason: &'static str,
        detail: &str,
        started: Instant,
    ) -> Result<()> {
        let cid = coll.row.id;
        let total = store::count_vectors(self.db(), cid, &f.def.name).await?;
        f.total.store(total, Ordering::Relaxed);
        let engine = self.engine();
        let params = f.params;
        let fresh: Arc<dyn AnnIndex> = blocking(move || {
            let idx = engine.create(&params)?;
            idx.reserve(usize::try_from(total).unwrap_or(usize::MAX).saturating_add(64))?;
            Ok(Arc::from(idx))
        })
        .await?;
        let mut after = 0u64;
        loop {
            if self.superseded(coll, f, generation) {
                return Ok(());
            }
            let page = store::scan(self.db(), cid, &f.def.name, None, after, self.inner.config.batch).await?;
            let Some(last) = page.last().map(|(k, _)| *k) else {
                break;
            };
            after = last;
            let n = page.len() as u64;
            let idx = Arc::clone(&fresh);
            let dim = f.def.dimension;
            blocking(move || {
                for (k, bytes) in page {
                    idx.upsert(k, &metric::decode(&bytes, dim)?)?;
                }
                Ok(())
            })
            .await?;
            f.done.fetch_add(n, Ordering::Relaxed);
        }

        // Hand-over: no write runs while the lock is held, so the replayed
        // state is exactly the committed one.
        let _w = coll.write.lock().await;
        if self.superseded(coll, f, generation) {
            return Ok(());
        }
        let pending: Vec<u64> = f.status().pending.take().unwrap_or_default().into_iter().collect();
        let current = store::vectors_by_keys(self.db(), cid, &f.def.name, &pending, None).await?;
        let (seq, _) = store::change_seq(self.db(), cid).await?;
        let idx = Arc::clone(&fresh);
        let dim = f.def.dimension;
        blocking(move || {
            for k in pending {
                match current.get(&k) {
                    Some(bytes) => idx.upsert(k, &metric::decode(bytes, dim)?)?,
                    None => idx.remove(k)?,
                }
            }
            Ok(())
        })
        .await?;
        let vectors = fresh.len() as u64;
        f.set_live(Some(fresh));
        {
            let mut st = f.status();
            st.state = IndexState::Ready;
            st.reason = None;
            st.dirty = true;
            st.manifest = None;
            st.last_rebuild = Some(RebuildInfo {
                reason: reason.to_owned(),
                detail: detail.to_owned(),
                vectors,
                duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                finished: now(),
            });
        }
        f.done.store(vectors, Ordering::Relaxed);
        f.total.store(vectors, Ordering::Relaxed);
        let engine = self.engine();
        let fi = Arc::clone(f);
        if let Err(e) = blocking(move || fi.save(engine.as_ref(), seq)).await {
            tracing::warn!(collection = %coll.row.name, field = %f.def.name, error = %e, "saving rebuilt vector index failed; it stays in memory");
        }
        tracing::info!(collection = %coll.row.name, field = %f.def.name, vectors, reason, "vector index rebuilt");
        Ok(())
    }

    /// pgvector: (re)builds the HNSW index of `f` in the background on the
    /// maintenance connection (`REINDEX INDEX CONCURRENTLY` for a valid
    /// index, else `CREATE INDEX CONCURRENTLY`); the old index serves
    /// meanwhile. Progress comes from `pg_stat_progress_create_index`. A
    /// field that is already building is left alone.
    fn start_pg_build(&self, coll: &Arc<Coll>, f: &Arc<FieldIndex>, reason: &'static str, detail: String) {
        let generation = {
            let mut st = f.status();
            if st.state == IndexState::Building {
                return;
            }
            st.state = IndexState::Building;
            st.reason = Some(format!("{reason}: {detail}"));
            st.generation += 1;
            st.generation
        };
        f.done.store(0, Ordering::Relaxed);
        f.total.store(0, Ordering::Relaxed);
        metrics::counter!("forgebase_vector_rebuilds_total", "reason" => reason).increment(1);
        metrics::gauge!("forgebase_vector_rebuilds_running").increment(1.0);
        let svc = self.clone();
        let coll = Arc::clone(coll);
        let f = Arc::clone(f);
        self.track(tokio::spawn(async move {
            let started = Instant::now();
            let r = svc.run_pg_build(&coll, &f, generation, reason, &detail, started).await;
            metrics::gauge!("forgebase_vector_rebuilds_running").decrement(1.0);
            if let Err(e) = r {
                tracing::error!(collection = %coll.row.name, field = %f.def.name, error = %e, "pgvector index build failed");
                let mut st = f.status();
                if st.generation == generation {
                    if f.pg_valid.load(Ordering::Relaxed) {
                        st.state = IndexState::Ready;
                        st.reason = Some(format!("last rebuild failed: {e}"));
                    } else {
                        st.state = IndexState::Failed;
                        st.reason = Some(format!("index build failed: {e}"));
                    }
                }
            }
        }));
    }

    async fn run_pg_build(
        &self,
        coll: &Arc<Coll>,
        f: &Arc<FieldIndex>,
        generation: u64,
        reason: &'static str,
        detail: &str,
        started: Instant,
    ) -> Result<()> {
        // A conversion waits for running builds.
        let storage = self.inner.storage.read().await;
        let Some(pgv) = storage.pg() else {
            return Ok(());
        };
        if self.superseded(coll, f, generation) {
            return Ok(());
        }
        let db = self.db();
        let spec = pg::spec(coll.row.id, &f.def);
        let total = store::count_vectors(db, coll.row.id, &f.def.name).await?;
        f.total.store(total, Ordering::Relaxed);
        let application = format!("forgebase vector build {}", spec.name);
        let build = pg::build(db, pgv, &spec, Some(&application));
        tokio::pin!(build);
        let mut tick = tokio::time::interval(Duration::from_millis(200));
        let r = loop {
            tokio::select! {
                r = &mut build => break r,
                _ = tick.tick() => {
                    if let Ok(Some(n)) = pg::build_progress(db, &application).await {
                        f.done.store(n.min(total), Ordering::Relaxed);
                    }
                }
            }
        };
        r?;
        let info = pg::indexes(db).await?.remove(&spec.name);
        let valid = info.as_ref().is_some_and(|i| i.valid);
        f.pg_valid.store(valid, Ordering::Relaxed);
        f.pg_bytes.store(info.map(|i| i.bytes).unwrap_or(0), Ordering::Relaxed);
        f.pg_indexed.store(total, Ordering::Relaxed);
        if self.superseded(coll, f, generation) {
            return Ok(());
        }
        {
            let mut st = f.status();
            st.state = if valid { IndexState::Ready } else { IndexState::Failed };
            st.reason = (!valid).then(|| "the index build left an invalid index".to_owned());
            st.last_rebuild = Some(RebuildInfo {
                reason: reason.to_owned(),
                detail: detail.to_owned(),
                vectors: total,
                duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                finished: now(),
            });
        }
        f.done.store(total, Ordering::Relaxed);
        tracing::info!(collection = %coll.row.name, field = %f.def.name, index = %spec.name, vectors = total, reason, "pgvector index built");
        Ok(())
    }

    /// pgvector: builds the index of a new field now (it has no vectors; the
    /// build still scans the table, concurrently with writes). A failure
    /// leaves the field searchable exactly, with the reason.
    async fn build_new_field(&self, pgv: &Pgv, cid: i64, f: &FieldIndex) {
        let spec = pg::spec(cid, &f.def);
        if spec.elem.is_none() {
            return;
        }
        match pg::build(self.db(), pgv, &spec, None).await {
            Ok(()) => f.pg_valid.store(true, Ordering::Relaxed),
            Err(e) => {
                tracing::error!(field = %f.def.name, error = %e, "creating the pgvector index failed; searches are exact");
                let mut st = f.status();
                st.state = IndexState::Failed;
                st.reason = Some(format!("index build failed: {e}"));
            }
        }
    }

    /// pgvector: drops the indexes of fields that are gone (left for the
    /// next start's clean-up when it fails).
    async fn drop_field_indexes(&self, cid: i64, fields: &[VectorField]) {
        let names: Vec<String> = fields.iter().map(|f| pg::spec(cid, f).name).collect();
        if let Err(e) = pg::drop_indexes(self.db(), &names).await {
            tracing::warn!(error = %e, "dropping pgvector indexes failed; the next start drops them");
        }
    }

    /// The serving index of `f`, loading it first if needed.
    async fn serving(&self, coll: &Arc<Coll>, f: &Arc<FieldIndex>) -> Result<Option<Arc<dyn AnnIndex>>> {
        if f.status().state == IndexState::Unloaded {
            let _w = coll.write.lock().await;
            self.ensure_loaded(coll).await?;
        }
        Ok(f.live())
    }

    /// Applies committed changes to every field index; on failure the index
    /// is dropped and rebuilt. Caller holds `coll.write`.
    async fn apply(&self, coll: &Arc<Coll>, changes: Vec<(u64, BTreeMap<String, Vec<f32>>)>) {
        for f in coll.fields() {
            let name = f.def.name.clone();
            let changes: Vec<(u64, Option<Vec<f32>>)> =
                changes.iter().map(|(k, vs)| (*k, vs.get(&name).cloned())).collect();
            let fi = Arc::clone(&f);
            let r = blocking(move || {
                let refs: Vec<(u64, Option<&[f32]>)> = changes.iter().map(|(k, v)| (*k, v.as_deref())).collect();
                fi.apply(&refs)
            })
            .await;
            if let Err(e) = r {
                tracing::error!(collection = %coll.row.name, field = %name, error = %e, "applying a change to the vector index failed; rebuilding it");
                f.set_live(None);
                {
                    let mut st = f.status();
                    st.state = IndexState::Failed;
                }
                self.start_rebuild_locked(coll, &f, "apply_failed", e.to_string());
            }
        }
    }

    // ------------------------------------------------------------ collections

    /// Collections of the caller's tenant (the plain `engine:vector_read`
    /// grants the listing). Collections the caller may not read at all are
    /// left out; `items` counts what the caller's list rule shows.
    pub async fn list_collections(&self, ctx: &AccessContext) -> Result<Vec<VectorCollection>> {
        self.authorize(ctx, Action::VectorRead)?;
        let tenant = Self::scope(ctx)?;
        let mut colls: Vec<Arc<Coll>> = self
            .all_collections()
            .into_iter()
            .filter(|c| c.row.tenant == tenant)
            .collect();
        colls.sort_by(|a, b| a.row.name.cmp(&b.row.name));
        let mut out = Vec::with_capacity(colls.len());
        for c in colls {
            let rule = match self.rule(ctx, Action::VectorRead, &c.row.name, ResourceOp::List) {
                Ok(r) => r,
                Err(VectorError::Forbidden(_) | VectorError::Unauthenticated) => continue,
                Err(e) => return Err(e),
            };
            let filter = self.rule_filter(rule)?;
            out.push(self.describe(&c, filter.as_ref(), "vector.list").await?);
        }
        Ok(out)
    }

    async fn describe(&self, c: &Coll, filter: Option<&Filter>, what: &'static str) -> Result<VectorCollection> {
        let db = self.rdb(what, true);
        let (items, _) = store::count_items(&db, c.row.id, filter).await?;
        let (_, updated) = store::change_seq(&db, c.row.id).await?;
        Ok(c.definition(items, updated))
    }

    /// One collection (`items`: what the caller's view rule shows).
    pub async fn get_collection(&self, ctx: &AccessContext, name: &str) -> Result<VectorCollection> {
        let rule = self.rule(ctx, Action::VectorRead, name, ResourceOp::View)?;
        let c = self.coll(ctx, name)?;
        let filter = self.rule_filter(rule)?;
        self.describe(&c, filter.as_ref(), "vector.get_collection").await
    }

    fn field_def(&self, spec: &FieldSpec) -> Result<VectorField> {
        check_name("field", &spec.name)?;
        if spec.dimension == 0 || spec.dimension > MAX_DIMENSION {
            return Err(VectorError::invalid(
                "invalid_dimension",
                format!("dimension must be 1 to {MAX_DIMENSION}"),
            ));
        }
        let range = |v: Option<u32>, default: u32, lo: u32, hi: u32, what: &str| -> Result<u32> {
            let v = v.unwrap_or(default);
            if (lo..=hi).contains(&v) {
                Ok(v)
            } else {
                Err(VectorError::invalid(
                    "invalid_field",
                    format!("{what} must be {lo} to {hi}"),
                ))
            }
        };
        Ok(VectorField {
            name: spec.name.clone(),
            dimension: spec.dimension,
            metric: spec.metric,
            quantization: spec.quantization.unwrap_or_default(),
            connectivity: range(spec.connectivity, DEFAULT_CONNECTIVITY, 2, 128, "connectivity")?,
            expansion_add: range(spec.expansion_add, DEFAULT_EXPANSION_ADD, 8, 4096, "expansion_add")?,
            expansion_search: range(
                spec.expansion_search,
                DEFAULT_EXPANSION_SEARCH,
                8,
                4096,
                "expansion_search",
            )?,
            created: now(),
        })
    }

    /// Creates a collection in the caller's tenant.
    pub async fn create_collection(&self, ctx: &AccessContext, new: NewCollection) -> Result<VectorCollection> {
        self.allow(ctx, Action::VectorAdmin, &new.name, ResourceOp::Create)?;
        let tenant = Self::scope(ctx)?;
        check_name("collection", &new.name)?;
        if let Some(src) = &new.source_collection
            && self
                .inner
                .data
                .get_collection(&AccessContext::system(), src)
                .await
                .is_err()
        {
            return Err(VectorError::invalid(
                "unknown_source_collection",
                format!("source_collection {src:?} is not a collection"),
            ));
        }
        if new.description.len() > 1024 {
            return Err(VectorError::invalid(
                "invalid_request",
                "description is longer than 1024 bytes",
            ));
        }
        if new.fields.len() > MAX_FIELDS {
            return Err(VectorError::invalid(
                "too_many_fields",
                format!("at most {MAX_FIELDS} vector fields"),
            ));
        }
        let mut defs = Vec::with_capacity(new.fields.len());
        let mut seen = HashSet::new();
        for spec in &new.fields {
            let d = self.field_def(spec)?;
            if !seen.insert(d.name.to_ascii_lowercase()) {
                return Err(VectorError::FieldExists(d.name));
            }
            defs.push(d);
        }
        let exact_threshold = new.exact_threshold.unwrap_or(self.inner.config.exact_threshold);
        let storage = self.inner.storage.read().await;
        let _cat = self.inner.catalog.lock().await;
        if self.coll(ctx, &new.name).is_ok() {
            return Err(VectorError::CollectionExists(new.name));
        }
        let ts = now();
        let mut options = Map::new();
        options.insert("exact_threshold".into(), exact_threshold.into());
        if let Some(src) = &new.source_collection {
            options.insert(SOURCE_COLLECTION_OPTION.into(), src.clone().into());
        }
        let mut tx = self.db().begin(TxMode::Normal).await?;
        let r = async {
            let id = store::insert_collection(&mut *tx, &tenant, &new.name, &new.description, &options, &ts).await?;
            for d in &defs {
                store::insert_field(&mut *tx, id, d).await?;
            }
            Ok(id)
        }
        .await;
        let id = finish(tx, r).await?;
        let dir = self.inner.root.join(id.to_string());
        let mut fields = BTreeMap::new();
        for d in defs {
            let idx = match storage.pg() {
                None => FieldIndex::empty(d, &dir, self.inner.engine.as_ref())?,
                Some(pgv) => {
                    let idx = FieldIndex::on_postgres(d, &dir, false);
                    self.build_new_field(pgv, id, &idx).await;
                    idx
                }
            };
            fields.insert(idx.def.name.clone(), Arc::new(idx));
        }
        let coll = Arc::new(Coll {
            row: CollectionRow {
                id,
                tenant: tenant.clone(),
                name: new.name.clone(),
                description: new.description,
                options,
                created: ts.clone(),
            },
            exact_threshold,
            write: tokio::sync::Mutex::new(()),
            fields: RwLock::new(fields),
            dropped: AtomicBool::new(false),
        });
        self.inner
            .collections
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .insert((tenant, new.name), Arc::clone(&coll));
        Ok(coll.definition(0, ts))
    }

    /// Drops a collection, its items, vectors and index files.
    pub async fn delete_collection(&self, ctx: &AccessContext, name: &str) -> Result<()> {
        self.allow(ctx, Action::VectorAdmin, name, ResourceOp::Delete)?;
        let storage = self.inner.storage.read().await;
        let _cat = self.inner.catalog.lock().await;
        let coll = self.coll(ctx, name)?;
        let _w = coll.write.lock().await;
        let mut tx = self.db().begin(TxMode::Normal).await?;
        let r = store::delete_collection(&mut *tx, coll.row.id).await;
        finish(tx, r).await?;
        coll.dropped.store(true, Ordering::Relaxed);
        if storage.pg().is_some() {
            let defs: Vec<VectorField> = coll.fields().iter().map(|f| f.def.clone()).collect();
            self.drop_field_indexes(coll.row.id, &defs).await;
        }
        self.inner
            .collections
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&(coll.row.tenant.clone(), coll.row.name.clone()));
        let dir = self.inner.root.join(coll.row.id.to_string());
        let _ = blocking(move || {
            if let Err(e) = std::fs::remove_dir_all(&dir)
                && e.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(path = %dir.display(), error = %e, "could not delete vector index directory");
            }
            Ok(())
        })
        .await;
        Ok(())
    }

    /// Vector fields of a collection.
    pub async fn list_fields(&self, ctx: &AccessContext, name: &str) -> Result<Vec<VectorField>> {
        self.rule(ctx, Action::VectorRead, name, ResourceOp::View)?;
        Ok(self.coll(ctx, name)?.fields().iter().map(|f| f.def.clone()).collect())
    }

    /// Adds a vector field (existing items have no vector for it).
    pub async fn add_field(&self, ctx: &AccessContext, name: &str, spec: FieldSpec) -> Result<VectorField> {
        self.allow(ctx, Action::VectorAdmin, name, ResourceOp::Update)?;
        let coll = self.coll(ctx, name)?;
        let def = self.field_def(&spec)?;
        let storage = self.inner.storage.read().await;
        let _w = coll.write.lock().await;
        {
            let fields = coll.fields.read().unwrap_or_else(|p| p.into_inner());
            if fields.len() >= MAX_FIELDS {
                return Err(VectorError::invalid(
                    "too_many_fields",
                    format!("at most {MAX_FIELDS} vector fields"),
                ));
            }
            if fields.keys().any(|k| k.eq_ignore_ascii_case(&def.name)) {
                return Err(VectorError::FieldExists(def.name));
            }
        }
        if storage.pg().is_none() {
            self.ensure_loaded(&coll).await?;
        }
        let mut tx = self.db().begin(TxMode::Normal).await?;
        let r = async {
            store::insert_field(&mut *tx, coll.row.id, &def).await?;
            store::bump(&mut *tx, coll.row.id, &now()).await
        }
        .await;
        finish(tx, r).await?;
        let dir = self.inner.root.join(coll.row.id.to_string());
        let idx = Arc::new(match storage.pg() {
            None => FieldIndex::empty(def.clone(), &dir, self.inner.engine.as_ref())?,
            Some(pgv) => {
                let idx = FieldIndex::on_postgres(def.clone(), &dir, false);
                self.build_new_field(pgv, coll.row.id, &idx).await;
                idx
            }
        });
        coll.fields
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .insert(def.name.clone(), idx);
        Ok(def)
    }

    /// Drops a vector field, its vectors and its index files.
    pub async fn delete_field(&self, ctx: &AccessContext, name: &str, field: &str) -> Result<()> {
        self.allow(ctx, Action::VectorAdmin, name, ResourceOp::Update)?;
        let coll = self.coll(ctx, name)?;
        let storage = self.inner.storage.read().await;
        let _w = coll.write.lock().await;
        let f = coll.field(field)?;
        if storage.pg().is_none() {
            self.ensure_loaded(&coll).await?;
        }
        let mut tx = self.db().begin(TxMode::Normal).await?;
        let r = async {
            store::delete_field(&mut *tx, coll.row.id, &f.def.name).await?;
            store::bump(&mut *tx, coll.row.id, &now()).await
        }
        .await;
        finish(tx, r).await?;
        coll.fields
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&f.def.name);
        f.status().generation += 1;
        if storage.pg().is_some() {
            self.drop_field_indexes(coll.row.id, std::slice::from_ref(&f.def)).await;
        }
        let fi = Arc::clone(&f);
        let _ = blocking(move || {
            fi.remove_files();
            Ok(())
        })
        .await;
        Ok(())
    }

    // ------------------------------------------------------------------ items

    /// Inserts or replaces items in one transaction. A new item needs the
    /// `create` rule, an existing one the `update` rule: a restricting rule
    /// must hold for the stored item (update) and for the written one.
    pub async fn upsert(&self, ctx: &AccessContext, name: &str, items: Vec<UpsertItem>) -> Result<UpsertResult> {
        let create = self.rule(ctx, Action::VectorWrite, name, ResourceOp::Create);
        let update = self.rule(ctx, Action::VectorWrite, name, ResourceOp::Update);
        let (create, update) = match (create, update) {
            (Err(e), Err(_)) => return Err(e),
            (c, u) => (self.gate(c)?, self.gate(u)?),
        };
        let coll = self.coll(ctx, name)?;
        if items.is_empty() || items.len() > MAX_BATCH {
            return Err(VectorError::invalid(
                "invalid_batch",
                format!("send 1 to {MAX_BATCH} items"),
            ));
        }
        struct Prepared {
            id: String,
            vectors: BTreeMap<String, Vec<f32>>,
            metadata: Map<String, serde_json::Value>,
            text: Option<String>,
        }
        let mut prepared = Vec::with_capacity(items.len());
        let mut ids = HashSet::new();
        for item in items {
            let id = match item.id {
                Some(id) => {
                    check_id(&id)?;
                    id
                }
                None => new_id(),
            };
            if !ids.insert(id.clone()) {
                return Err(VectorError::invalid(
                    "duplicate_id",
                    format!("item {id:?} appears twice"),
                ));
            }
            let mut vectors = BTreeMap::new();
            for (field, v) in item.vectors {
                let f = coll.field(&field).map_err(|_| {
                    VectorError::invalid(
                        "unknown_vector_field",
                        format!("the collection has no vector field {field:?}"),
                    )
                })?;
                vectors.insert(field, metric::validate(&f.def.name, f.def.dimension, f.def.metric, &v)?);
            }
            let metadata = item.metadata.unwrap_or_default();
            if serde_json::to_string(&metadata).map(|s| s.len()).unwrap_or(usize::MAX) > MAX_METADATA_BYTES {
                return Err(VectorError::invalid(
                    "metadata_too_large",
                    format!("metadata of {id:?} is larger than {MAX_METADATA_BYTES} bytes"),
                ));
            }
            if item.text.as_ref().is_some_and(|t| t.len() > MAX_TEXT_BYTES) {
                return Err(VectorError::invalid(
                    "text_too_large",
                    format!("text of {id:?} is longer than {MAX_TEXT_BYTES} bytes"),
                ));
            }
            prepared.push(Prepared {
                id,
                vectors,
                metadata,
                text: item.text,
            });
        }

        let storage = self.inner.storage.read().await;
        let pgv = storage.pg();
        let _w = coll.write.lock().await;
        if coll.dropped.load(Ordering::Relaxed) {
            return Err(VectorError::CollectionNotFound(name.to_owned()));
        }
        if pgv.is_none() {
            self.ensure_loaded(&coll).await?;
        }
        let ts = now();
        let cid = coll.row.id;
        let dialect = self.db().dialect();
        let unrestricted = matches!((&create, &update), (Gate::Allow, Gate::Allow));
        let mut tx = self.db().begin(TxMode::Normal).await?;
        let r = async {
            let mut keys = Vec::with_capacity(prepared.len());
            let mut created = 0u64;
            for p in &prepared {
                let existing = if unrestricted {
                    None
                } else {
                    store::item_key_tx(&mut *tx, cid, &p.id).await?
                };
                let (gate, op) = match existing {
                    Some(_) => (&update, ResourceOp::Update),
                    None => (&create, ResourceOp::Create),
                };
                let check = match gate {
                    Gate::Deny => {
                        return Err(Self::denied(ctx, format!("{op} of items in {name} is not allowed")));
                    }
                    Gate::Allow => None,
                    Gate::Filter(f) => Some(&**f),
                };
                if let (Some(f), Some(k)) = (check, existing)
                    && !store::matches_tx(&mut *tx, dialect, cid, k, f).await?
                {
                    return Err(Self::denied(ctx, format!("item {:?} may not be updated", p.id)));
                }
                let (k, version) =
                    store::upsert_item(&mut *tx, cid, &p.id, &p.metadata, p.text.as_deref(), &ts).await?;
                if let Some(f) = check
                    && !store::matches_tx(&mut *tx, dialect, cid, k, f).await?
                {
                    return Err(Self::denied(
                        ctx,
                        format!("item {:?} does not satisfy the {op} rule of {name}", p.id),
                    ));
                }
                if version == 1 {
                    created += 1;
                }
                let vectors: Vec<(&str, &[f32])> = p.vectors.iter().map(|(f, v)| (f.as_str(), v.as_slice())).collect();
                store::replace_vectors(&mut *tx, cid, k, &vectors, pgv).await?;
                keys.push(k);
            }
            store::bump(&mut *tx, cid, &ts).await?;
            Ok((keys, created))
        }
        .await;
        let (keys, created) = finish(tx, r).await?;
        let total = prepared.len() as u64;
        let ids: Vec<String> = prepared.iter().map(|p| p.id.clone()).collect();
        if pgv.is_none() {
            let changes = keys.into_iter().zip(prepared.into_iter().map(|p| p.vectors)).collect();
            self.apply(&coll, changes).await;
        }
        metrics::counter!("forgebase_vector_upserts_total").increment(total);
        Ok(UpsertResult {
            ids,
            created,
            updated: total - created,
        })
    }

    /// One item with its vectors.
    pub async fn get_item(&self, ctx: &AccessContext, name: &str, id: &str) -> Result<Item> {
        let rule = self.rule(ctx, Action::VectorRead, name, ResourceOp::View)?;
        let coll = self.coll(ctx, name)?;
        check_id(id).map_err(|_| VectorError::ItemNotFound(id.to_owned()))?;
        let filter = self.rule_filter(rule)?;
        let storage = self.inner.storage.read().await;
        let db = self.rdb("vector.get", true);
        let mut row = store::get_item(&db, coll.row.id, id, filter.as_ref())
            .await?
            .ok_or_else(|| VectorError::ItemNotFound(id.to_owned()))?;
        if let Some(mut sc) = self.source_check(ctx, &coll)? {
            if !self.visible(ctx, &mut sc, id).await? {
                return Err(VectorError::ItemNotFound(id.to_owned()));
            }
            sc.strip(&mut row.metadata);
        }
        let mut vectors = BTreeMap::new();
        for (field, bytes) in store::item_vectors(&db, coll.row.id, row.key, storage.pg()).await? {
            if let Ok(f) = coll.field(&field) {
                vectors.insert(field, metric::decode(&bytes, f.def.dimension)?);
            }
        }
        Ok(Item {
            id: row.id,
            vectors,
            metadata: row.metadata,
            text: row.text,
            version: row.version,
            created: row.created,
            updated: row.updated,
        })
    }

    /// Deletes items; unknown ids are ignored. Returns how many were removed.
    /// Items the caller's `delete` rule does not allow are left alone like
    /// unknown ids.
    pub async fn delete_items(&self, ctx: &AccessContext, name: &str, ids: Vec<String>) -> Result<u64> {
        let rule = self.rule(ctx, Action::VectorWrite, name, ResourceOp::Delete)?;
        let coll = self.coll(ctx, name)?;
        let filter = self.rule_filter(rule)?;
        if ids.is_empty() || ids.len() > MAX_BATCH {
            return Err(VectorError::invalid(
                "invalid_batch",
                format!("send 1 to {MAX_BATCH} ids"),
            ));
        }
        for id in &ids {
            check_id(id)?;
        }
        let storage = self.inner.storage.read().await;
        let _w = coll.write.lock().await;
        if storage.pg().is_none() {
            self.ensure_loaded(&coll).await?;
        }
        let cid = coll.row.id;
        let dialect = self.db().dialect();
        let mut tx = self.db().begin(TxMode::Normal).await?;
        let r = async {
            let keys = store::delete_items(&mut *tx, dialect, cid, &ids, filter.as_ref()).await?;
            if !keys.is_empty() {
                store::bump(&mut *tx, cid, &now()).await?;
            }
            Ok(keys)
        }
        .await;
        let keys = finish(tx, r).await?;
        let n = keys.len() as u64;
        if n > 0 && storage.pg().is_none() {
            self.apply(&coll, keys.into_iter().map(|k| (k, BTreeMap::new())).collect())
                .await;
        }
        metrics::counter!("forgebase_vector_deletes_total").increment(n);
        Ok(n)
    }

    /// Deletes one item.
    pub async fn delete_item(&self, ctx: &AccessContext, name: &str, id: &str) -> Result<()> {
        if self
            .delete_items(ctx, name, vec![id.to_owned()])
            .await
            .map_err(|e| match e {
                VectorError::Invalid { code: "invalid_id", .. } => VectorError::ItemNotFound(id.to_owned()),
                other => other,
            })?
            == 0
        {
            return Err(VectorError::ItemNotFound(id.to_owned()));
        }
        Ok(())
    }

    // ----------------------------------------------------------------- search

    async fn count_for_exact(&self, cid: i64, f: &FieldIndex, filter: Option<&Filter>) -> Result<u64> {
        let cap = self.inner.config.max_exact_scan;
        match filter {
            None => store::count_vectors(self.db(), cid, &f.def.name).await,
            Some(flt) => Ok(store::matching_keys(
                self.db(),
                cid,
                &f.def.name,
                flt,
                usize::try_from(cap + 1).unwrap_or(usize::MAX),
            )
            .await?
            .len() as u64),
        }
    }

    async fn exact_scan(
        &self,
        cid: i64,
        f: &FieldIndex,
        q: &[f32],
        k: usize,
        filter: Option<&Filter>,
    ) -> Result<Vec<Scored>> {
        let mut top = TopK::new(k);
        let mut after = 0u64;
        loop {
            let page = store::scan(self.db(), cid, &f.def.name, filter, after, self.inner.config.batch).await?;
            let Some(last) = page.last().map(|(k, _)| *k) else {
                break;
            };
            after = last;
            for (key, bytes) in page {
                let v = metric::decode(&bytes, f.def.dimension)?;
                top.push(key, metric::distance(f.def.metric, q, &v));
            }
        }
        Ok(top.into_sorted())
    }

    async fn exact_keys(&self, cid: i64, f: &FieldIndex, q: &[f32], k: usize, keys: &[u64]) -> Result<Vec<Scored>> {
        let mut top = TopK::new(k);
        for chunk in keys.chunks(self.inner.config.batch.max(1)) {
            for (key, bytes) in store::vectors_by_keys(self.db(), cid, &f.def.name, chunk, None).await? {
                let v = metric::decode(&bytes, f.def.dimension)?;
                top.push(key, metric::distance(f.def.metric, q, &v));
            }
        }
        Ok(top.into_sorted())
    }

    async fn ann(
        ix: Arc<dyn AnnIndex>,
        q: Vec<f32>,
        k: usize,
        allow: Option<Arc<HashSet<u64>>>,
    ) -> Result<Vec<Scored>> {
        blocking(move || {
            let hits = match allow {
                None => ix.search(&q, k, None)?,
                Some(set) => {
                    let pred = move |key: u64| set.contains(&key);
                    ix.search(&q, k, Some(&pred))?
                }
            };
            Ok(hits.into_iter().map(|(key, dist)| Scored { dist, key }).collect())
        })
        .await
    }

    async fn post_filter(
        &self,
        cid: i64,
        ix: Arc<dyn AnnIndex>,
        q: &[f32],
        k: usize,
        filter: &Filter,
    ) -> Result<Vec<Scored>> {
        let size = ix.len().max(1);
        let mut want = (k * 4).max(64).min(size);
        for _ in 0..6 {
            let cand = Self::ann(Arc::clone(&ix), q.to_vec(), want, None).await?;
            let keys: Vec<u64> = cand.iter().map(|s| s.key).collect();
            let ok = store::verify(self.db(), cid, &keys, filter).await?;
            let hits: Vec<Scored> = cand.into_iter().filter(|s| ok.contains(&s.key)).take(k).collect();
            if hits.len() >= k || want >= size {
                return Ok(hits);
            }
            want = (want * 4).min(size);
        }
        let cand = Self::ann(Arc::clone(&ix), q.to_vec(), want, None).await?;
        let keys: Vec<u64> = cand.iter().map(|s| s.key).collect();
        let ok = store::verify(self.db(), cid, &keys, filter).await?;
        Ok(cand.into_iter().filter(|s| ok.contains(&s.key)).take(k).collect())
    }

    /// The core kNN: chooses exact, ANN, filtered ANN or post-filtering.
    #[allow(clippy::too_many_arguments)]
    async fn knn(
        &self,
        pgv: Option<&Pgv>,
        coll: &Arc<Coll>,
        f: &Arc<FieldIndex>,
        q: &[f32],
        k: usize,
        filter: Option<&Filter>,
        mode: FilterMode,
        exact: bool,
        what: &'static str,
    ) -> Result<Knn> {
        if let Some(pgv) = pgv {
            return self.knn_pg(pgv, coll, f, q, k, filter, mode, exact, what).await;
        }
        let cid = coll.row.id;
        let cfg = &self.inner.config;
        let threshold = coll.exact_threshold as usize;
        if exact {
            let n = self.count_for_exact(cid, f, filter).await?;
            if n > cfg.max_exact_scan {
                return Err(VectorError::invalid(
                    "exact_too_large",
                    format!("an exact search scans at most {} vectors", cfg.max_exact_scan),
                ));
            }
            return Ok(Knn {
                hits: self.exact_scan(cid, f, q, k, filter).await?,
                strategy: Strategy::Exact,
                candidates: filter.map(|_| n),
            });
        }
        let Some(ix) = self.serving(coll, f).await? else {
            let n = self.count_for_exact(cid, f, filter).await?;
            if n > cfg.max_exact_scan {
                let reason = f.status().reason.clone().unwrap_or_else(|| "rebuilding".into());
                return Err(VectorError::IndexNotReady(
                    format!("{}.{}", coll.row.name, f.def.name),
                    reason,
                ));
            }
            return Ok(Knn {
                hits: self.exact_scan(cid, f, q, k, filter).await?,
                strategy: Strategy::ExactFallback,
                candidates: filter.map(|_| n),
            });
        };
        let Some(flt) = filter else {
            if ix.len() <= threshold {
                return Ok(Knn {
                    hits: self.exact_scan(cid, f, q, k, None).await?,
                    strategy: Strategy::Exact,
                    candidates: None,
                });
            }
            return Ok(Knn {
                hits: Self::ann(ix, q.to_vec(), k, None).await?,
                strategy: Strategy::Ann,
                candidates: None,
            });
        };
        if mode != FilterMode::Post {
            let keys = store::matching_keys(self.db(), cid, &f.def.name, flt, cfg.max_prefilter + 1).await?;
            if keys.len() <= cfg.max_prefilter {
                let n = keys.len() as u64;
                if keys.len() <= threshold {
                    return Ok(Knn {
                        hits: self.exact_keys(cid, f, q, k, &keys).await?,
                        strategy: Strategy::Exact,
                        candidates: Some(n),
                    });
                }
                let set = Arc::new(keys.iter().copied().collect::<HashSet<u64>>());
                let hits = Self::ann(Arc::clone(&ix), q.to_vec(), k, Some(set)).await?;
                if hits.len() >= k.min(keys.len()) {
                    return Ok(Knn {
                        hits,
                        strategy: Strategy::FilteredAnn,
                        candidates: Some(n),
                    });
                }
                // The graph walk ran out of matching candidates: exact over
                // the matching set, so a selective filter never loses hits.
                if n <= cfg.max_exact_scan {
                    return Ok(Knn {
                        hits: self.exact_keys(cid, f, q, k, &keys).await?,
                        strategy: Strategy::Exact,
                        candidates: Some(n),
                    });
                }
                return Ok(Knn {
                    hits,
                    strategy: Strategy::FilteredAnn,
                    candidates: Some(n),
                });
            }
        }
        Ok(Knn {
            hits: self.post_filter(cid, ix, q, k, flt).await?,
            strategy: Strategy::PostFilter,
            candidates: None,
        })
    }

    /// The kNN on pgvector (ADR-0029 §1.4). Same strategies and rules as
    /// the portable path: `exact` (explicit, small sets, small filtered
    /// sets) orders by the distance on the uncast column; `ann` uses the
    /// field's HNSW index with `hnsw.ef_search` = max(`expansion_search`,
    /// `k`); `filtered_ann` (pre/auto) and `post_filter` run an iterative
    /// scan (`relaxed_order`, re-sorted) with the compiled filter in the same
    /// `WHERE`. When fewer than `k` come back, pre/auto search the matching
    /// set exactly if it is small enough, and both widen the scan
    /// (ADR-0012's over-fetch loop: `hnsw.max_scan_tuples`, or `ef_search`
    /// before pgvector 0.8) until `k` match or the whole field was scanned.
    /// `candidates` is reported where it was counted (exact strategies).
    #[allow(clippy::too_many_arguments)]
    async fn knn_pg(
        &self,
        pgv: &Pgv,
        coll: &Coll,
        f: &FieldIndex,
        q: &[f32],
        k: usize,
        filter: Option<&Filter>,
        mode: FilterMode,
        exact: bool,
        what: &'static str,
    ) -> Result<Knn> {
        let via = self.rdb(what, true);
        let db: &dyn Backend = &via;
        let cid = coll.row.id;
        let name = f.def.name.as_str();
        let cfg = &self.inner.config;
        let threshold = u64::from(coll.exact_threshold);
        let spec = pg::spec(cid, &f.def);
        let exact_knn = |strategy: Strategy, candidates: Option<u64>| async move {
            Ok::<_, VectorError>(Knn {
                hits: pg::exact(db, pgv, cid, &f.def, q, k, filter).await?,
                strategy,
                candidates,
            })
        };
        if exact {
            let n = pg::count_upto(db, cid, name, filter, cfg.max_exact_scan + 1).await?;
            if n > cfg.max_exact_scan {
                return Err(VectorError::invalid(
                    "exact_too_large",
                    format!("an exact search scans at most {} vectors", cfg.max_exact_scan),
                ));
            }
            return exact_knn(Strategy::Exact, filter.map(|_| n)).await;
        }
        if spec.elem.is_none() || !f.pg_valid.load(Ordering::Relaxed) {
            let n = pg::count_upto(db, cid, name, filter, cfg.max_exact_scan + 1).await?;
            if n > cfg.max_exact_scan {
                if spec.elem.is_none() {
                    return Err(VectorError::invalid(
                        "exact_too_large",
                        format!(
                            "fields wider than {} dimensions have exact search only on pgvector, over at most {} vectors",
                            pg::MAX_HALFVEC_DIMS,
                            cfg.max_exact_scan
                        ),
                    ));
                }
                let reason = f.status().reason.clone().unwrap_or_else(|| "building".into());
                return Err(VectorError::IndexNotReady(
                    format!("{}.{}", coll.row.name, f.def.name),
                    reason,
                ));
            }
            let strategy = if spec.elem.is_none() {
                Strategy::Exact
            } else {
                Strategy::ExactFallback
            };
            return exact_knn(strategy, filter.map(|_| n)).await;
        }
        let ef_search = f.def.expansion_search.max(u32::try_from(k).unwrap_or(u32::MAX));
        let Some(flt) = filter else {
            if pg::count_upto(db, cid, name, None, threshold + 1).await? <= threshold {
                return exact_knn(Strategy::Exact, None).await;
            }
            // Without an iterative scan pgvector returns up to ef_search >= k rows.
            let scan = pg::Scan {
                ef_search,
                iterative: false,
                max_scan_tuples: pg::DEFAULT_MAX_SCAN_TUPLES,
            };
            return Ok(Knn {
                hits: pg::ann(db, pgv, &spec, q, k, None, scan).await?,
                strategy: Strategy::Ann,
                candidates: None,
            });
        };
        if mode != FilterMode::Post {
            let m = pg::count_upto(db, cid, name, Some(flt), threshold + 1).await?;
            if m <= threshold {
                return exact_knn(Strategy::Exact, Some(m)).await;
            }
        }
        let strategy = if mode == FilterMode::Post {
            Strategy::PostFilter
        } else {
            Strategy::FilteredAnn
        };
        let mut scan = pg::Scan {
            ef_search,
            iterative: pgv.iterative,
            max_scan_tuples: pg::DEFAULT_MAX_SCAN_TUPLES,
        };
        let mut hits = pg::ann(db, pgv, &spec, q, k, Some(flt), scan).await?;
        if hits.len() >= k {
            return Ok(Knn {
                hits,
                strategy,
                candidates: None,
            });
        }
        if mode != FilterMode::Post {
            // The scan ran out of matching candidates: exact over the
            // matching set, so a selective filter never loses hits.
            let n = pg::count_upto(db, cid, name, Some(flt), cfg.max_exact_scan + 1).await?;
            if n <= cfg.max_exact_scan {
                return exact_knn(Strategy::Exact, Some(n)).await;
            }
        }
        let total = store::count_vectors(db, cid, name).await?.max(1);
        for _ in 0..8 {
            let exhausted = if scan.iterative {
                scan.max_scan_tuples >= total
            } else {
                scan.ef_search >= pg::MAX_EF_SEARCH
            };
            if hits.len() >= k || exhausted {
                break;
            }
            if scan.iterative {
                scan.max_scan_tuples = scan.max_scan_tuples.saturating_mul(4).min(total);
            } else {
                scan.ef_search = scan.ef_search.saturating_mul(4).min(pg::MAX_EF_SEARCH);
            }
            hits = pg::ann(db, pgv, &spec, q, k, Some(flt), scan).await?;
        }
        Ok(Knn {
            hits,
            strategy,
            candidates: None,
        })
    }

    fn record(f: &FieldIndex, strategy: Strategy, took: Duration) {
        let c = &f.counters;
        c.searches.fetch_add(1, Ordering::Relaxed);
        match strategy {
            Strategy::Exact | Strategy::ExactFallback => c.exact.fetch_add(1, Ordering::Relaxed),
            _ => c.ann.fetch_add(1, Ordering::Relaxed),
        };
        c.micros
            .fetch_add(u64::try_from(took.as_micros()).unwrap_or(u64::MAX), Ordering::Relaxed);
        metrics::counter!("forgebase_vector_searches_total", "strategy" => strategy_name(strategy)).increment(1);
        metrics::histogram!("forgebase_vector_search_duration_seconds").record(took.as_secs_f64());
    }

    fn check_k(k: u32) -> Result<usize> {
        if k == 0 || k > MAX_K {
            return Err(VectorError::invalid("invalid_k", format!("k must be 1 to {MAX_K}")));
        }
        Ok(k as usize)
    }

    /// Similarity search (SPEC §27 #13). The caller's list rule is a
    /// pre-filter; with a `source_collection`, hits whose record the caller
    /// may not view are dropped and more candidates are searched to refill
    /// `k` (up to [`MAX_K`]).
    pub async fn search(&self, ctx: &AccessContext, name: &str, req: SearchRequest) -> Result<SearchResponse> {
        let rule = self.rule(ctx, Action::VectorRead, name, ResourceOp::List)?;
        let started = Instant::now();
        let coll = self.coll(ctx, name)?;
        let f = coll.pick(req.field.as_deref())?;
        let k = Self::check_k(req.k)?;
        let q = metric::validate(&f.def.name, f.def.dimension, f.def.metric, &req.vector)?;
        let dialect = self.db().dialect();
        let filter = Filter::parse(dialect, req.filter.as_deref(), &req.params)?;
        let mut source = self.source_check(ctx, &coll)?;
        if let Some(sc) = &source {
            sc.check_filter(filter.as_ref())?;
        }
        let filter = Filter::restrict(dialect, filter, rule)?;
        let storage = self.inner.storage.read().await;
        let pgv = storage.pg();
        let mut want = k;
        let (knn, mut items, mut vectors, kept) = loop {
            let knn = self
                .knn(
                    pgv,
                    &coll,
                    &f,
                    &q,
                    want,
                    filter.as_ref(),
                    req.filter_mode,
                    req.exact,
                    "vector.search",
                )
                .await?;
            let keys: Vec<u64> = knn.hits.iter().map(|s| s.key).collect();
            let (items, vectors) = self
                .hydrate(pgv, &coll, &f, &keys, req.include_vectors, "vector.search")
                .await?;
            let Some(sc) = source.as_mut() else {
                break (knn, items, vectors, keys);
            };
            let mut kept = Vec::with_capacity(k);
            for key in &keys {
                if kept.len() == k {
                    break;
                }
                if let Some(item) = items.get(key)
                    && self.visible(ctx, sc, &item.id).await?
                {
                    kept.push(*key);
                }
            }
            if kept.len() >= k || knn.hits.len() < want || want >= MAX_K as usize {
                break (knn, items, vectors, kept);
            }
            want = (want * 4).min(MAX_K as usize);
        };
        let kept: HashSet<u64> = kept.into_iter().collect();
        let hits = knn
            .hits
            .iter()
            .filter(|s| kept.contains(&s.key))
            .filter_map(|s| {
                let item = items.remove(&s.key)?;
                let distance = metric::public_distance(f.def.metric, s.dist);
                let mut metadata = item.metadata;
                if let Some(sc) = &source {
                    sc.strip(&mut metadata);
                }
                Some(SearchHit {
                    id: item.id,
                    distance,
                    score: metric::score(f.def.metric, distance),
                    metadata: req.include_metadata.then_some(metadata),
                    text: if req.include_text { item.text } else { None },
                    vector: vectors.remove(&s.key),
                })
            })
            .collect();
        let took = started.elapsed();
        Self::record(&f, knn.strategy, took);
        Ok(SearchResponse {
            hits,
            strategy: knn.strategy,
            candidates: knn.candidates,
            took_ms: ms(took),
        })
    }

    async fn hydrate(
        &self,
        pgv: Option<&Pgv>,
        coll: &Coll,
        f: &FieldIndex,
        keys: &[u64],
        with_vectors: bool,
        what: &'static str,
    ) -> Result<(HashMap<u64, store::ItemRow>, HashMap<u64, Vec<f32>>)> {
        let db = self.rdb(what, pgv.is_some());
        let items = store::items_by_keys(&db, coll.row.id, keys).await?;
        let mut vectors = HashMap::new();
        if with_vectors {
            for (k, bytes) in store::vectors_by_keys(&db, coll.row.id, &f.def.name, keys, pgv).await? {
                vectors.insert(k, metric::decode(&bytes, f.def.dimension)?);
            }
        }
        Ok((items, vectors))
    }

    /// Hybrid search: FTS5 keyword ranking and vector ranking fused with RRF.
    /// The caller's list rule filters both rankings; with a
    /// `source_collection`, fused hits whose record the caller may not view
    /// are skipped (within the `candidates` pool).
    pub async fn hybrid(&self, ctx: &AccessContext, name: &str, req: HybridRequest) -> Result<HybridResponse> {
        let rule = self.rule(ctx, Action::VectorRead, name, ResourceOp::List)?;
        let started = Instant::now();
        let coll = self.coll(ctx, name)?;
        let k = Self::check_k(req.k)?;
        let candidates = req.candidates.unwrap_or_else(|| (req.k * 5).max(50)).min(MAX_K);
        if candidates == 0 {
            return Err(VectorError::invalid(
                "invalid_candidates",
                "candidates must be at least 1",
            ));
        }
        let candidates = (candidates as usize).max(k);
        let rrf_k = req.rrf_k.unwrap_or(60.0);
        if !rrf_k.is_finite() || rrf_k <= 0.0 || rrf_k > 1e6 {
            return Err(VectorError::invalid("invalid_rrf_k", "rrf_k must be a positive number"));
        }
        let weights = req.weights.unwrap_or_default();
        let ok = |w: f64| w.is_finite() && (0.0..=1e6).contains(&w);
        if !ok(weights.vector) || !ok(weights.text) || weights.vector + weights.text == 0.0 {
            return Err(VectorError::invalid(
                "invalid_weights",
                "weights must be non-negative and not both zero",
            ));
        }
        if req.vector.is_none() && req.text.as_deref().is_none_or(|t| t.trim().is_empty()) {
            return Err(VectorError::invalid(
                "invalid_request",
                "send a vector, a text, or both",
            ));
        }
        let dialect = self.db().dialect();
        let filter = Filter::parse(dialect, req.filter.as_deref(), &req.params)?;
        let mut source = self.source_check(ctx, &coll)?;
        if let Some(sc) = &source {
            sc.check_filter(filter.as_ref())?;
        }
        let filter = Filter::restrict(dialect, filter, rule)?;
        let field = match (&req.vector, req.include_vectors) {
            (Some(_), _) | (None, true) => Some(coll.pick(req.field.as_deref())?),
            (None, false) => req.field.as_deref().map(|n| coll.field(n)).transpose()?,
        };

        let storage = self.inner.storage.read().await;
        let pgv = storage.pg();
        let mut vector_strategy = None;
        let mut by_vector: Vec<(u64, f32)> = Vec::new();
        if let (Some(v), Some(f)) = (&req.vector, &field) {
            let q = metric::validate(&f.def.name, f.def.dimension, f.def.metric, v)?;
            let knn = self
                .knn(
                    pgv,
                    &coll,
                    f,
                    &q,
                    candidates,
                    filter.as_ref(),
                    req.filter_mode,
                    false,
                    "vector.hybrid",
                )
                .await?;
            Self::record(f, knn.strategy, started.elapsed());
            vector_strategy = Some(knn.strategy);
            by_vector = knn
                .hits
                .iter()
                .map(|s| (s.key, metric::public_distance(f.def.metric, s.dist)))
                .collect();
        }
        let mut by_text: Vec<(u64, f64)> = Vec::new();
        if let Some(t) = req.text.as_deref().filter(|t| !t.trim().is_empty()) {
            let expr = match_query(self.db().dialect(), t, req.text_operator)?;
            let db = self.rdb("vector.hybrid", pgv.is_some() || req.vector.is_none());
            by_text = store::keyword(&db, coll.row.id, &expr, filter.as_ref(), candidates).await?;
        }

        let vk: Vec<u64> = by_vector.iter().map(|(k, _)| *k).collect();
        let tk: Vec<u64> = by_text.iter().map(|(k, _)| *k).collect();
        let mut fused: Vec<(u64, f64)> = rrf(&vk, &tk, rrf_k, weights);
        if source.is_none() {
            fused.truncate(k);
        }
        let vrank: HashMap<u64, (u32, f32)> = by_vector
            .iter()
            .enumerate()
            .map(|(i, (key, d))| (*key, (u32::try_from(i + 1).unwrap_or(u32::MAX), *d)))
            .collect();
        let trank: HashMap<u64, (u32, f64)> = by_text
            .iter()
            .enumerate()
            .map(|(i, (key, s))| (*key, (u32::try_from(i + 1).unwrap_or(u32::MAX), *s)))
            .collect();
        let keys: Vec<u64> = fused.iter().map(|(k, _)| *k).collect();
        let (mut items, mut vectors) = match &field {
            Some(f) => {
                self.hydrate(pgv, &coll, f, &keys, req.include_vectors, "vector.hybrid")
                    .await?
            }
            None => (
                store::items_by_keys(&self.rdb("vector.hybrid", true), coll.row.id, &keys).await?,
                HashMap::new(),
            ),
        };
        if let Some(sc) = source.as_mut() {
            let mut kept = Vec::with_capacity(k);
            for (key, score) in fused {
                if kept.len() == k {
                    break;
                }
                let Some(id) = items.get(&key).map(|i| i.id.clone()) else {
                    continue;
                };
                if self.visible(ctx, sc, &id).await? {
                    if let Some(item) = items.get_mut(&key) {
                        sc.strip(&mut item.metadata);
                    }
                    kept.push((key, score));
                }
            }
            fused = kept;
        }
        let hits = fused
            .iter()
            .filter_map(|(key, score)| {
                let item = items.get(key)?;
                Some(HybridHit {
                    id: item.id.clone(),
                    score: *score,
                    vector_rank: vrank.get(key).map(|r| r.0),
                    vector_distance: vrank.get(key).map(|r| r.1),
                    text_rank: trank.get(key).map(|r| r.0),
                    text_score: trank.get(key).map(|r| r.1),
                    metadata: req.include_metadata.then(|| item.metadata.clone()),
                    text: if req.include_text { item.text.clone() } else { None },
                    vector: vectors.remove(key),
                })
            })
            .collect();
        Ok(HybridResponse {
            hits,
            vector_strategy,
            vector_candidates: by_vector.len() as u64,
            text_candidates: by_text.len() as u64,
            took_ms: ms(started.elapsed()),
        })
    }

    // -------------------------------------------------- rebuild, stats, recall

    /// Starts background rebuilds (one field or all) and returns their status.
    /// A field that is already rebuilding is left alone.
    pub async fn rebuild(&self, ctx: &AccessContext, name: &str, field: Option<&str>) -> Result<Vec<IndexStatus>> {
        self.allow(ctx, Action::VectorAdmin, name, ResourceOp::Update)?;
        let coll = self.coll(ctx, name)?;
        let targets = match field {
            Some(n) => vec![coll.field(n)?],
            None => coll.fields(),
        };
        let storage = self.inner.storage.read().await;
        if let Some(pgv) = storage.pg() {
            let mut out = Vec::with_capacity(targets.len());
            for f in &targets {
                if pg::spec(coll.row.id, &f.def).elem.is_some() {
                    self.start_pg_build(&coll, f, "manual", "requested by an administrator".into());
                }
            }
            let info = pg::indexes(self.db()).await?;
            for f in &targets {
                out.push(self.pg_index_status(pgv, &coll, f, &info).await?);
            }
            return Ok(out);
        }
        {
            let _w = coll.write.lock().await;
            // A file that loads keeps serving while the rebuild runs; one that
            // does not starts its own rebuild here.
            self.ensure_loaded(&coll).await?;
            for f in &targets {
                self.start_rebuild_locked(&coll, f, "manual", "requested by an administrator".into());
            }
        }
        let mut out = Vec::with_capacity(targets.len());
        for f in &targets {
            out.push(self.index_status(&coll, f).await?);
        }
        Ok(out)
    }

    /// [`IndexStatus`] of a pgvector field (`info`: the `fbv_` indexes).
    async fn pg_index_status(
        &self,
        pgv: &Pgv,
        coll: &Coll,
        f: &FieldIndex,
        info: &HashMap<String, pg::IndexInfo>,
    ) -> Result<IndexStatus> {
        let canonical = store::count_vectors(self.db(), coll.row.id, &f.def.name).await?;
        let spec = pg::spec(coll.row.id, &f.def);
        let index = info.get(&spec.name);
        let valid = index.is_some_and(|i| i.valid);
        let bytes = index.map(|i| i.bytes).unwrap_or(0);
        let indexed = if valid { canonical } else { 0 };
        f.pg_bytes.store(bytes, Ordering::Relaxed);
        f.pg_indexed.store(indexed, Ordering::Relaxed);
        let st = f.status();
        let searches = f.counters.searches.load(Ordering::Relaxed);
        #[allow(clippy::cast_precision_loss)]
        let mean = if searches == 0 {
            0.0
        } else {
            f.counters.micros.load(Ordering::Relaxed) as f64 / searches as f64 / 1000.0
        };
        Ok(IndexStatus {
            field: f.def.name.clone(),
            state: st.state,
            serving: if valid { "ann" } else { "exact" }.to_owned(),
            reason: st.reason.clone().or_else(|| {
                spec.elem.is_none().then(|| {
                    format!(
                        "pgvector indexes at most {} dimensions: exact search",
                        pg::MAX_HALFVEC_DIMS
                    )
                })
            }),
            progress: (st.state == IndexState::Building).then(|| f.progress()),
            canonical_vectors: canonical,
            indexed_vectors: indexed,
            memory_bytes: bytes,
            serialized_bytes: bytes,
            capacity: 0,
            engine: "pgvector".to_owned(),
            engine_version: pgv.version.clone(),
            quantization: f.def.quantization,
            connectivity: spec.m,
            expansion_search: f.def.expansion_search.min(pg::MAX_EF_SEARCH),
            dirty: false,
            last_saved: None,
            last_rebuild: st.last_rebuild.clone(),
            last_recall: st.last_recall.clone(),
            searches,
            ann_searches: f.counters.ann.load(Ordering::Relaxed),
            exact_searches: f.counters.exact.load(Ordering::Relaxed),
            mean_search_ms: mean,
        })
    }

    async fn index_status(&self, coll: &Coll, f: &FieldIndex) -> Result<IndexStatus> {
        let canonical = store::count_vectors(self.db(), coll.row.id, &f.def.name).await?;
        let live = f.live();
        let es = live.as_ref().map(|l| l.stats()).unwrap_or_default();
        let st = f.status();
        let searches = f.counters.searches.load(Ordering::Relaxed);
        #[allow(clippy::cast_precision_loss)]
        let mean = if searches == 0 {
            0.0
        } else {
            f.counters.micros.load(Ordering::Relaxed) as f64 / searches as f64 / 1000.0
        };
        Ok(IndexStatus {
            field: f.def.name.clone(),
            state: st.state,
            serving: if live.is_some() { "ann" } else { "exact" }.to_owned(),
            reason: st.reason.clone(),
            progress: (st.state == IndexState::Building).then(|| f.progress()),
            canonical_vectors: canonical,
            indexed_vectors: es.len,
            memory_bytes: es.memory_bytes,
            serialized_bytes: es.serialized_bytes,
            capacity: es.capacity,
            engine: self.inner.engine.name().to_owned(),
            engine_version: self.inner.engine.version(),
            quantization: f.def.quantization,
            connectivity: f.def.connectivity,
            expansion_search: f.def.expansion_search,
            dirty: st.dirty,
            last_saved: st.last_saved.clone(),
            last_rebuild: st.last_rebuild.clone(),
            last_recall: st.last_recall.clone(),
            searches,
            ann_searches: f.counters.ann.load(Ordering::Relaxed),
            exact_searches: f.counters.exact.load(Ordering::Relaxed),
            mean_search_ms: mean,
        })
    }

    /// Statistics and index state of a collection. They count every item,
    /// so a caller whose view rule restricts items is refused.
    pub async fn stats(&self, ctx: &AccessContext, name: &str) -> Result<CollectionStats> {
        self.allow(ctx, Action::VectorRead, name, ResourceOp::View)?;
        let coll = self.coll(ctx, name)?;
        let db = self.rdb("vector.stats", true);
        let (items, with_text) = store::count_items(&db, coll.row.id, None).await?;
        let (seq, _) = store::change_seq(&db, coll.row.id).await?;
        let storage = self.inner.storage.read().await;
        let mut indexes = Vec::new();
        match storage.pg() {
            None => {
                for f in coll.fields() {
                    indexes.push(self.index_status(&coll, &f).await?);
                }
            }
            Some(pgv) => {
                let info = pg::indexes(self.db()).await?;
                for f in coll.fields() {
                    indexes.push(self.pg_index_status(pgv, &coll, &f, &info).await?);
                }
            }
        }
        Ok(CollectionStats {
            collection: coll.row.name.clone(),
            items,
            items_with_text: with_text,
            change_seq: seq,
            indexes,
        })
    }

    /// Totals over the caller's tenant.
    pub async fn tenant_stats(&self, ctx: &AccessContext) -> Result<VectorStats> {
        self.authorize(ctx, Action::VectorRead)?;
        let tenant = Self::scope(ctx)?;
        Ok(Self::sum_stats(
            self.all_collections().into_iter().filter(|c| c.row.tenant == tenant),
        ))
    }

    /// Totals over every tenant, for server metrics. No authorisation: it
    /// reveals counts only and is meant for the process itself.
    pub fn totals(&self) -> VectorStats {
        Self::sum_stats(self.all_collections().into_iter())
    }

    fn sum_stats(collections: impl Iterator<Item = Arc<Coll>>) -> VectorStats {
        let mut s = VectorStats {
            collections: 0,
            indexes: 0,
            building: 0,
            indexed_vectors: 0,
            memory_bytes: 0,
        };
        for c in collections {
            s.collections += 1;
            for f in c.fields() {
                s.indexes += 1;
                if f.status().state == IndexState::Building {
                    s.building += 1;
                }
                if let Some(l) = f.live() {
                    let es = l.stats();
                    s.indexed_vectors += es.len;
                    s.memory_bytes += es.memory_bytes;
                } else if f.pg_valid.load(Ordering::Relaxed) {
                    // pgvector: as last seen by a stats call or a build.
                    s.indexed_vectors += f.pg_indexed.load(Ordering::Relaxed);
                    s.memory_bytes += f.pg_bytes.load(Ordering::Relaxed);
                }
            }
        }
        s
    }

    /// Waits until every index of the collection serves from an ANN index
    /// (loaded or rebuilt), or `timeout` passes. Returns whether it is ready.
    pub async fn wait_ready(&self, ctx: &AccessContext, name: &str, timeout: Duration) -> Result<bool> {
        self.rule(ctx, Action::VectorRead, name, ResourceOp::View)?;
        let coll = self.coll(ctx, name)?;
        let deadline = Instant::now() + timeout;
        loop {
            if self.inner.storage.read().await.pg().is_none() {
                let _w = coll.write.lock().await;
                self.ensure_loaded(&coll).await?;
            }
            let ready = coll.fields().iter().all(|f| f.status().state == IndexState::Ready);
            if ready {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// pgvector recall check: both orders come from SQL (the HNSW index, and
    /// the distance on the uncast column), and the comparison runs in SQL
    /// too ([`pg::recall_exact`]).
    async fn pg_recall(&self, pgv: &Pgv, coll: &Coll, f: &FieldIndex, samples: u32, k: u32) -> Result<RecallReport> {
        let db = self.db();
        let cid = coll.row.id;
        let spec = pg::spec(cid, &f.def);
        if spec.elem.is_none() || !f.pg_valid.load(Ordering::Relaxed) {
            let reason = f.status().reason.clone().unwrap_or_else(|| "building".into());
            return Err(VectorError::IndexNotReady(
                format!("{}.{}", coll.row.name, f.def.name),
                reason,
            ));
        }
        let keys = pg::keys(db, cid, &f.def.name).await?;
        let n = keys.len();
        if n == 0 {
            return Err(VectorError::invalid("no_vectors", "the field has no vectors"));
        }
        if n as u64 > self.inner.config.max_exact_scan {
            return Err(VectorError::invalid(
                "exact_too_large",
                format!(
                    "a recall check scans at most {} vectors",
                    self.inner.config.max_exact_scan
                ),
            ));
        }
        let count = (samples as usize).min(n);
        let picked: Vec<u64> = (0..count).map(|s| keys[s * n / count]).collect();
        let k = k as usize;
        let ef_search = f.def.expansion_search.max(u32::try_from(k).unwrap_or(u32::MAX));
        let t = Instant::now();
        let ann = pg::recall_ann(db, pgv, &spec, &picked, k, ef_search).await?;
        let ann_t = t.elapsed();
        let t = Instant::now();
        let recall = pg::recall_exact(db, pgv, cid, &f.def, &picked, &ann, k).await?;
        let exact_t = t.elapsed();
        Ok(RecallReport {
            field: f.def.name.clone(),
            k: u32::try_from(k).unwrap_or(u32::MAX),
            samples: u32::try_from(count).unwrap_or(u32::MAX),
            recall,
            ann_ms: ms(ann_t),
            exact_ms: ms(exact_t),
            measured: now(),
        })
    }

    /// Recall check: stored vectors as queries, ANN results compared with
    /// exact results.
    pub async fn recall(&self, ctx: &AccessContext, name: &str, req: RecallRequest) -> Result<RecallReport> {
        self.allow(ctx, Action::VectorAdmin, name, ResourceOp::View)?;
        let coll = self.coll(ctx, name)?;
        let f = coll.pick(req.field.as_deref())?;
        let samples = req.samples.unwrap_or(100);
        let k = req.k.unwrap_or(10);
        if samples == 0 || samples > MAX_RECALL_SAMPLES || k == 0 || k > 100 {
            return Err(VectorError::invalid(
                "invalid_request",
                format!("samples must be 1 to {MAX_RECALL_SAMPLES}, k 1 to 100"),
            ));
        }
        let storage = self.inner.storage.read().await;
        if let Some(pgv) = storage.pg() {
            let report = self.pg_recall(pgv, &coll, &f, samples, k).await?;
            f.status().last_recall = Some(report.clone());
            return Ok(report);
        }
        let ix = self.serving(&coll, &f).await?.ok_or_else(|| {
            VectorError::IndexNotReady(format!("{}.{}", coll.row.name, f.def.name), "rebuilding".into())
        })?;
        let n = store::count_vectors(self.db(), coll.row.id, &f.def.name).await?;
        if n == 0 {
            return Err(VectorError::invalid("no_vectors", "the field has no vectors"));
        }
        if n > self.inner.config.max_exact_scan {
            return Err(VectorError::invalid(
                "exact_too_large",
                format!(
                    "a recall check scans at most {} vectors",
                    self.inner.config.max_exact_scan
                ),
            ));
        }
        let mut all: Vec<(u64, Vec<f32>)> = Vec::with_capacity(usize::try_from(n).unwrap_or(0));
        let mut after = 0u64;
        loop {
            let page = store::scan(
                self.db(),
                coll.row.id,
                &f.def.name,
                None,
                after,
                self.inner.config.batch,
            )
            .await?;
            let Some(last) = page.last().map(|(k, _)| *k) else {
                break;
            };
            after = last;
            for (key, bytes) in page {
                all.push((key, metric::decode(&bytes, f.def.dimension)?));
            }
        }
        let metric_kind = f.def.metric;
        let field = f.def.name.clone();
        let report = blocking(move || {
            let n = all.len();
            let by_key: HashMap<u64, &Vec<f32>> = all.iter().map(|(k, v)| (*k, v)).collect();
            let samples = (samples as usize).min(n);
            let k = k as usize;
            let (mut ann_t, mut exact_t, mut sum) = (Duration::ZERO, Duration::ZERO, 0.0f64);
            for s in 0..samples {
                let q = &all[s * n / samples].1;
                let t = Instant::now();
                let ann: Vec<u64> = ix.search(q, k, None)?.into_iter().map(|(key, _)| key).collect();
                ann_t += t.elapsed();
                let t = Instant::now();
                let mut top = TopK::new(k);
                for (key, v) in &all {
                    top.push(*key, metric::distance(metric_kind, q, v));
                }
                let exact = top.into_sorted();
                exact_t += t.elapsed();
                // An ANN result is correct when it is at least as close as the
                // k-th true neighbour (equal distances tie, whichever key wins).
                let kth = exact.last().map_or(f32::INFINITY, |e| e.dist);
                let tol = 1e-5 * kth.abs().max(1.0);
                let hit = ann
                    .iter()
                    .filter_map(|key| by_key.get(key))
                    .filter(|v| metric::distance(metric_kind, q, v) <= kth + tol)
                    .count()
                    .min(exact.len());
                #[allow(clippy::cast_precision_loss)]
                {
                    sum += hit as f64 / exact.len().max(1) as f64;
                }
            }
            #[allow(clippy::cast_precision_loss)]
            let recall = sum / samples.max(1) as f64;
            Ok(RecallReport {
                field,
                k: u32::try_from(k).unwrap_or(u32::MAX),
                samples: u32::try_from(samples).unwrap_or(u32::MAX),
                recall,
                ann_ms: ms(ann_t),
                exact_ms: ms(exact_t),
                measured: now(),
            })
        })
        .await?;
        f.status().last_recall = Some(report.clone());
        Ok(report)
    }

    /// The query plan PostgreSQL makes for the index scan of a search on
    /// pgvector (FB-162), one line per plan node: the ANN statement of
    /// `req` (with its filter: the iterative scan) whether or not the
    /// search would pick it. Administrators only; `400 explain_unsupported`
    /// on the portable engine.
    pub async fn explain_search(&self, ctx: &AccessContext, name: &str, req: SearchRequest) -> Result<Vec<String>> {
        self.allow(ctx, Action::VectorAdmin, name, ResourceOp::View)?;
        let coll = self.coll(ctx, name)?;
        let f = coll.pick(req.field.as_deref())?;
        let k = Self::check_k(req.k)?;
        let q = metric::validate(&f.def.name, f.def.dimension, f.def.metric, &req.vector)?;
        let filter = Filter::parse(self.db().dialect(), req.filter.as_deref(), &req.params)?;
        let storage = self.inner.storage.read().await;
        let Some(pgv) = storage.pg() else {
            return Err(VectorError::invalid(
                "explain_unsupported",
                "search plans exist on pgvector only",
            ));
        };
        let spec = pg::spec(coll.row.id, &f.def);
        let scan = pg::Scan {
            ef_search: f.def.expansion_search.max(u32::try_from(k).unwrap_or(u32::MAX)),
            iterative: pgv.iterative && filter.is_some(),
            max_scan_tuples: pg::DEFAULT_MAX_SCAN_TUPLES,
        };
        let Some(stmt) = pg::ann_statement(self.db(), pgv, &spec, &q, k, filter.as_ref(), scan)? else {
            return Err(VectorError::invalid(
                "explain_unsupported",
                format!("{} has no HNSW index (exact search only)", f.def.name),
            ));
        };
        Ok(self.db().explain(&stmt).await?.into_iter().map(|p| p.detail).collect())
    }

    // ------------------------------------------------------------- convert

    /// `vector.convert` (ADR-0029 §1.4): moves every canonical vector to
    /// `to`'s storage. To pgvector: `vector` bytea -> `vec` in batches, then
    /// every field's HNSW index is built (concurrently) and the index files
    /// are deleted. To portable: `vec` -> bytea, the HNSW indexes are
    /// dropped and USearch rebuilds the in-memory indexes in the background.
    /// The last batch's transaction records `ext.vector.engine` (FB-161):
    /// a conversion that stops earlier leaves the database on its old
    /// engine, and the next start moves the converted vectors back. Holds
    /// the engine's write lock: every other vector operation waits.
    /// Instance-wide (no tenant), `engine:vector_admin`.
    ///
    /// # Errors
    /// `400 pgvector_unavailable` when converting to pgvector without a
    /// usable pgvector (with the layout problems), database errors.
    pub async fn convert(&self, ctx: &AccessContext, to: VectorEngine) -> Result<ConvertReport> {
        self.authorize(ctx, Action::VectorAdmin)?;
        if ctx.tenant.is_some() {
            return Err(VectorError::invalid(
                "invalid_request",
                "vector.convert is instance-wide; call it without a tenant",
            ));
        }
        let db = self.db();
        if db.dialect().name() != "postgres" {
            return match to {
                VectorEngine::Portable => Ok(ConvertReport {
                    from: VectorEngine::Portable,
                    to,
                    vectors: 0,
                    indexes: 0,
                }),
                VectorEngine::Pgvector => Err(VectorError::invalid(
                    "pgvector_unavailable",
                    "pgvector needs PostgreSQL",
                )),
            };
        }
        let probe = pg::probe(db).await?;
        if let (VectorEngine::Pgvector, Err(problems)) = (to, &probe) {
            return Err(VectorError::invalid("pgvector_unavailable", problems.join("; ")));
        }
        let mut storage = self.inner.storage.write().await;
        let from = storage.kind();
        let colls = self.all_collections();
        // Portable rebuilds stop at their next check (their field is replaced).
        for c in &colls {
            let _w = c.write.lock().await;
            for f in c.fields() {
                f.status().generation += 1;
            }
        }
        let batch = pg::batch_rows(self.inner.config.batch, pg::max_dimension(db).await?);
        let report = match (to, probe) {
            (VectorEngine::Pgvector, Ok(pgv)) => {
                let pgv = Arc::new(pgv);
                let vectors = pg::move_all(db, &pgv, VectorEngine::Pgvector, batch, true).await?;
                *storage = Storage::Pgvector(Arc::clone(&pgv));
                let existing = pg::indexes(db).await?;
                let mut indexes = 0;
                for c in &colls {
                    let _w = c.write.lock().await;
                    let dir = self.inner.root.join(c.row.id.to_string());
                    let mut fields = BTreeMap::new();
                    for f in c.fields() {
                        let spec = pg::spec(c.row.id, &f.def);
                        let mut valid = existing.get(&spec.name).is_some_and(|i| i.valid);
                        if spec.elem.is_some() && !valid {
                            pg::build(db, &pgv, &spec, None).await?;
                            indexes += 1;
                            valid = true;
                        }
                        fields.insert(
                            f.def.name.clone(),
                            Arc::new(FieldIndex::on_postgres(f.def.clone(), &dir, valid)),
                        );
                    }
                    *c.fields.write().unwrap_or_else(|p| p.into_inner()) = fields;
                    let _ = blocking(move || {
                        if let Err(e) = std::fs::remove_dir_all(&dir)
                            && e.kind() != std::io::ErrorKind::NotFound
                        {
                            tracing::warn!(path = %dir.display(), error = %e, "could not delete vector index files");
                        }
                        Ok(())
                    })
                    .await;
                }
                ConvertReport {
                    from,
                    to,
                    vectors,
                    indexes,
                }
            }
            (VectorEngine::Pgvector, Err(_)) => {
                return Err(VectorError::Internal("pgvector probe changed".into()));
            }
            (VectorEngine::Portable, probe) => {
                let vectors = match &probe {
                    Ok(pgv) => pg::move_all(db, pgv, VectorEngine::Portable, batch, true).await?,
                    Err(problems) if pg::has_vec_column(db).await? => {
                        return Err(VectorError::invalid(
                            "pgvector_unavailable",
                            format!(
                                "the vectors in column vec need pgvector to be moved: {}",
                                problems.join("; ")
                            ),
                        ));
                    }
                    Err(_) => {
                        pg::record_engine(db, VectorEngine::Portable).await?;
                        0
                    }
                };
                let names: Vec<String> = match &probe {
                    Ok(_) => pg::indexes(db).await?.into_keys().collect(),
                    Err(_) => Vec::new(),
                };
                pg::drop_indexes(db, &names).await?;
                *storage = Storage::Portable;
                for c in &colls {
                    let _w = c.write.lock().await;
                    let dir = self.inner.root.join(c.row.id.to_string());
                    let fields = c
                        .fields()
                        .into_iter()
                        .map(|f| (f.def.name.clone(), Arc::new(FieldIndex::unloaded(f.def.clone(), &dir))))
                        .collect();
                    *c.fields.write().unwrap_or_else(|p| p.into_inner()) = fields;
                    // USearch rebuilds from the canonical vectors (files of an
                    // older portable past are stale or match exactly).
                    self.ensure_loaded(c).await?;
                }
                ConvertReport {
                    from,
                    to,
                    vectors,
                    indexes: names.len() as u64,
                }
            }
        };
        drop(storage);
        tracing::info!(
            from = report.from.as_str(),
            to = report.to.as_str(),
            vectors = report.vectors,
            indexes = report.indexes,
            "vector storage converted"
        );
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyword_queries_quote_every_word() {
        assert_eq!(match_expr("rust db", TextOperator::Or).unwrap(), "\"rust\" OR \"db\"");
        assert_eq!(match_expr("a \"b", TextOperator::And).unwrap(), "\"a\" AND \"\"\"b\"");
        assert_eq!(
            match_expr("NEAR(x y) OR -z *", TextOperator::Or).unwrap(),
            "\"NEAR(x\" OR \"y)\" OR \"OR\" OR \"-z\""
        );
        assert!(match_expr("  - * ", TextOperator::Or).is_err());
    }

    #[test]
    fn rrf_rewards_agreement_and_respects_weights() {
        let fused = rrf(&[1, 2, 3], &[3, 4], 60.0, HybridWeights::default());
        assert_eq!(fused[0].0, 3, "in both lists");
        assert!((fused[0].1 - (1.0 / 63.0 + 1.0 / 61.0)).abs() < 1e-12);
        let text_only = rrf(&[1, 2], &[2, 9], 60.0, HybridWeights { vector: 0.0, text: 1.0 });
        assert_eq!(text_only[0].0, 2);
        assert_eq!(text_only[1].0, 9);
    }
}
