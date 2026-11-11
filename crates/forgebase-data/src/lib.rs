//! ForgeBase data core (FB-10, ADR-0003).
//!
//! [`DataService`] is the service API the REST layer (FB-11), the dashboard
//! and the SDKs build on. It owns:
//!
//! * **Collections** ([`collections`]): base, auth (schema only), view and
//!   system collections; every SPEC §4.3 field type; create / alter / drop as
//!   transactional DDL with a schema history.
//! * **Records and documents** ([`records`]): validated records, flexible JSON
//!   documents, merge-patch updates, versioning, automatic timestamps,
//!   relations with expand, bulk insert.
//! * **Queries** ([`list`]): the filter language compiled to parameterised SQL,
//!   sorting, projection, offset and cursor pagination, counts, aggregation,
//!   full-text search.
//! * **Indexes** ([`indexes`]): B-tree, composite, unique, partial,
//!   expression, JSON-path and FTS5 indexes; automatic indexes from the schema;
//!   inspection, rebuild, statistics, query plans, the slow-query log and
//!   index recommendations.
//! * **SQL editor backend** ([`sql`]): raw parameterised SQL with statement
//!   classification; reads run on connections that cannot write.
//!
//! Every call takes an [`AccessContext`] and asks the [`Authorizer`] first;
//! tenant scoping and field-level rules are enforced here too (ADR-0008).

pub mod access;
mod catalog;
pub mod changes;
pub mod collections;
mod ddl;
mod error;
mod guard;
pub mod indexes;
pub mod list;
pub mod native;
pub mod records;
pub mod sql;

use std::sync::{Arc, OnceLock, RwLock};

use forgebase_db::{Backend, DbError, ExecResult, Rows, RunAs, Statement, TxMode, WriteTx};
use forgebase_query::CompileOptions;
use forgebase_schema::Collection;

pub use access::{
    AccessContext, Action, Authorizer, DEFAULT_REALM, Decision, DefaultAuthorizer, FieldAccess, Predicate, Principal,
    READS_TENANT_DATA_OPTION, ResourceField, ResourceFieldType, ResourceKind, ResourceOp, ResourceRef, ResourceSchema,
    SecretHasher, SubjectRef, TENANT_COLUMN, TENANT_SCOPED_OPTION, is_tenant_scoped, reads_tenant_data,
};
pub use changes::{ChangeHook, ChangeOp, GuardRejection, PendingWrite, RecordChange, RowSnapshot, WriteGuard};
pub use collections::{IndexBuild, SchemaChange};
pub use error::DataError;
pub use indexes::{
    IndexDefinition, IndexInfo, IndexKey, IndexKind, IndexOrigin, IndexRecommendation, IndexStats, QueryPlan,
};
pub use list::{AggregateFunction, AggregateQuery, AggregateSpec, ListQuery, ListResult};
pub use native::NativePolicies;
pub use records::{GetOptions, Record, RecordVersion, UpdateOptions};
pub use sql::{ColumnInfo, SqlOptions, SqlResult, SqlStatement, TableInfo};

pub use forgebase_db as db;
pub use forgebase_query as query;
pub use forgebase_schema as schema;

use catalog::CatalogSnapshot;

/// Default page size.
pub const DEFAULT_PER_PAGE: u32 = 30;
/// Largest page size.
pub const MAX_PER_PAGE: u32 = 500;
/// Most rows a read-only SQL editor statement returns.
pub const MAX_SQL_ROWS: usize = 10_000;
/// Most records in one bulk insert.
pub const MAX_BATCH: usize = 1_000;

/// The data-core service. Cheap to clone.
#[derive(Debug, Clone)]
pub struct DataService {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    backend: Arc<dyn Backend>,
    authorizer: Arc<dyn Authorizer>,
    hasher: Option<Arc<dyn SecretHasher>>,
    catalog: RwLock<Arc<CatalogSnapshot>>,
    /// Serialises schema changes and catalog reloads in this process (the
    /// single writer already serialises them in the database).
    schema_lock: tokio::sync::Mutex<()>,
    /// Transactional outbox hook (FB-40), installed once at startup.
    change_hook: OnceLock<Arc<dyn changes::ChangeHook>>,
    /// Before-write hook (server functions, FB-60), installed once at startup.
    write_guard: OnceLock<Arc<dyn changes::WriteGuard>>,
    /// Native row-level security generator (FB-152, PostgreSQL), installed
    /// once at startup.
    native: OnceLock<Arc<dyn native::NativePolicies>>,
    /// The database sets (FB-173, ADR-0029 §3.7): once sharded, schema
    /// changes run on control and every shard as one two-phase commit.
    shards: OnceLock<forgebase_db::DbRouter>,
}

impl DataService {
    /// Builds the service on a migrated database and loads the catalog.
    pub async fn new(backend: Arc<dyn Backend>, authorizer: Arc<dyn Authorizer>) -> Result<Self, DataError> {
        Self::with_hasher(backend, authorizer, None).await
    }

    /// Same as [`DataService::new`] with a hasher for password fields.
    pub async fn with_hasher(
        backend: Arc<dyn Backend>,
        authorizer: Arc<dyn Authorizer>,
        hasher: Option<Arc<dyn SecretHasher>>,
    ) -> Result<Self, DataError> {
        let snapshot = CatalogSnapshot::load(backend.as_ref()).await?;
        Ok(Self {
            inner: Arc::new(Inner {
                backend,
                authorizer,
                hasher,
                catalog: RwLock::new(Arc::new(snapshot)),
                schema_lock: tokio::sync::Mutex::new(()),
                change_hook: OnceLock::new(),
                write_guard: OnceLock::new(),
                native: OnceLock::new(),
                shards: OnceLock::new(),
            }),
        })
    }

    /// The storage backend.
    pub fn backend(&self) -> &Arc<dyn Backend> {
        &self.inner.backend
    }

    /// The authorizer, for engines that ask about their own resources
    /// (`Authorizer::authorize_resource`, FB-134; object storage, FB-140).
    pub fn authorizer(&self) -> &Arc<dyn Authorizer> {
        &self.inner.authorizer
    }

    /// Re-reads collection definitions (after another process changed them).
    pub async fn reload_catalog(&self) -> Result<(), DataError> {
        let _guard = self.inner.schema_lock.lock().await;
        self.reload_locked().await
    }

    async fn reload_locked(&self) -> Result<(), DataError> {
        let snapshot = CatalogSnapshot::load(self.inner.backend.as_ref()).await?;
        *self.inner.catalog.write().unwrap_or_else(|p| p.into_inner()) = Arc::new(snapshot);
        Ok(())
    }

    pub(crate) fn catalog(&self) -> Arc<CatalogSnapshot> {
        self.inner.catalog.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub(crate) fn collection(&self, name_or_id: &str) -> Result<Collection, DataError> {
        self.catalog()
            .get(name_or_id)
            .cloned()
            .ok_or_else(|| DataError::CollectionNotFound(name_or_id.to_owned()))
    }

    /// Asks the authorizer. `Ok(Some(p))` restricts rows to predicate `p`.
    /// Public so feature crates (graph, FB-51) authorise through the same hook.
    pub fn authorize(
        &self,
        ctx: &AccessContext,
        action: Action,
        coll: Option<&Collection>,
    ) -> Result<Option<Predicate>, DataError> {
        if ctx.is_system() {
            return Ok(None);
        }
        match self.inner.authorizer.authorize(ctx, action, coll) {
            Decision::Allow => Ok(None),
            Decision::Filter(p) => Ok(Some(p)),
            Decision::Deny => {
                if ctx.principal == Principal::Anonymous {
                    Err(DataError::Unauthenticated)
                } else {
                    Err(DataError::Forbidden(format!(
                        "{action:?}{} is not allowed",
                        coll.map(|c| format!(" on {}", c.name)).unwrap_or_default()
                    )))
                }
            }
        }
    }

    /// Asks the authorizer about realtime channel `channel` (FB-40). Channels
    /// have no row-level rules: a `Decision::Filter` is a denial.
    pub fn authorize_channel(&self, ctx: &AccessContext, action: Action, channel: &str) -> Result<(), DataError> {
        if ctx.is_system() {
            return Ok(());
        }
        match self.inner.authorizer.authorize_channel(ctx, action, channel) {
            Decision::Allow => Ok(()),
            _ if ctx.principal == Principal::Anonymous => Err(DataError::Unauthenticated),
            _ => Err(DataError::Forbidden(format!(
                "{action:?} on channel {channel} is not allowed"
            ))),
        }
    }

    /// Authorises an action that is not about one collection (jobs, FB-61).
    ///
    /// The system is always allowed; everybody else goes through the
    /// [`Authorizer`] with no collection. Such actions have no rows to filter,
    /// so a [`Decision::Filter`] answer is treated as a denial (fail closed).
    pub fn authorize_action(&self, ctx: &AccessContext, action: Action) -> Result<(), DataError> {
        match self.authorize(ctx, action, None)? {
            None => Ok(()),
            Some(_) => Err(DataError::Forbidden(format!("{action:?} is not allowed"))),
        }
    }

    /// Asks the authorizer about one engine resource (FB-134; used by the
    /// vector and graph engines, FB-139). The system is always allowed;
    /// `Ok(Some(p))` restricts the resource's items to predicate `p` over the
    /// fields of the kind's [`ResourceSchema`](access::ResourceSchema).
    pub fn authorize_resource(
        &self,
        ctx: &AccessContext,
        action: Action,
        resource: &access::ResourceRef,
    ) -> Result<Option<Predicate>, DataError> {
        if ctx.is_system() {
            return Ok(None);
        }
        match self.inner.authorizer.authorize_resource(ctx, action, resource) {
            Decision::Allow => Ok(None),
            Decision::Filter(p) => Ok(Some(p)),
            _ if ctx.principal == Principal::Anonymous => Err(DataError::Unauthenticated),
            _ => Err(DataError::Forbidden(format!("{action:?} on {resource} is not allowed"))),
        }
    }

    /// Tells the authorizer which item fields resource rules of
    /// `schema.kind` may read (FB-134).
    pub fn register_resource_schema(&self, schema: access::ResourceSchema) {
        self.inner.authorizer.register_resource_schema(schema);
    }

    pub(crate) fn compile_options(&self, ctx: &AccessContext) -> CompileOptions {
        CompileOptions {
            allow_hidden: ctx.is_privileged(),
            ..CompileOptions::default()
        }
    }

    pub(crate) async fn begin(&self, mode: TxMode) -> Result<Box<dyn WriteTx>, DataError> {
        Ok(self.inner.backend.begin(mode).await?)
    }

    /// Installs the native row-level security generator (FB-152), called
    /// in every schema transaction from then on. Once; `false` if one was
    /// installed already.
    pub fn set_native_policies(&self, hook: Arc<dyn native::NativePolicies>) -> bool {
        self.inner.native.set(hook).is_ok()
    }

    pub(crate) fn native_policies(&self) -> Option<&Arc<dyn native::NativePolicies>> {
        self.inner.native.get()
    }

    /// Who `ctx`'s statements for `action` on `coll` run as (`enforce` mode,
    /// FB-152); `None`: as the server. Privileged callers and views never
    /// run under the RLS role.
    pub(crate) fn run_as(&self, ctx: &AccessContext, action: Action, coll: &Collection) -> Option<RunAs> {
        if ctx.is_system() || ctx.is_privileged() || coll.is_view() {
            return None;
        }
        self.inner.authorizer.native_session(ctx, action, coll)
    }

    /// Reads rows of `coll` for `ctx`: as the RLS role in `enforce` mode,
    /// unless the statement follows relation paths (the lookups would see
    /// the target table's policies; the data core reads targets with full
    /// visibility, rls-coverage §3.4).
    ///
    /// ADR-0029 §2.5: record reads may go to a replica (`what` names the call
    /// site: `records.list`, `records.view`, `records.aggregate`). A read as
    /// the RLS role stays on the primary: the role switch needs the writer's
    /// session.
    pub(crate) async fn read_rows(
        &self,
        ctx: &AccessContext,
        action: Action,
        coll: &Collection,
        stmt: &Statement,
        follows_relations: bool,
        what: &'static str,
    ) -> Result<Rows, DataError> {
        let run_as = if follows_relations {
            None
        } else {
            self.run_as(ctx, action, coll)
        };
        Ok(match run_as {
            Some(r) => self.inner.backend.fetch_all_as(stmt, &r).await?,
            None => self.fetch_replica(stmt, what).await?,
        })
    }

    /// A plain read that may be served by a replica (ADR-0029 §2.5).
    pub(crate) async fn fetch_replica(&self, stmt: &Statement, what: &'static str) -> Result<Rows, DbError> {
        self.inner
            .backend
            .fetch_routed(stmt, &forgebase_db::ReadIntent::replica(what))
            .await
    }
}

/// Runs a record write, as `run_as` when given. Under native row-level
/// security a refused row is an error, never a silent no-op: a policy
/// violation (SQLSTATE 42501) and an update or delete that reaches no row
/// the data core had found are [`DataError::Forbidden`].
pub(crate) async fn execute_maybe_as(
    tx: &mut dyn WriteTx,
    stmt: &Statement,
    run_as: Option<&RunAs>,
    expect_row: bool,
    on_err: impl FnOnce(DbError) -> DataError,
) -> Result<ExecResult, DataError> {
    let Some(r) = run_as else {
        return tx.execute(stmt).await.map_err(on_err);
    };
    let done = tx.execute_as(stmt, r).await.map_err(|e| match e {
        DbError::Statement(m) if m.contains("row-level security") => {
            DataError::Forbidden(format!("native row-level security refused the write: {m}"))
        }
        other => on_err(other),
    })?;
    if expect_row && done.rows_affected == 0 {
        return Err(DataError::Forbidden(
            "native row-level security refused the write".into(),
        ));
    }
    Ok(done)
}

/// Commits on success, rolls back on error.
pub(crate) async fn finish<T>(tx: Box<dyn WriteTx>, result: Result<T, DataError>) -> Result<T, DataError> {
    match result {
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
