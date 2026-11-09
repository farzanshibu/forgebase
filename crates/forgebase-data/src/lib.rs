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
pub mod collections;
mod ddl;
mod error;
mod guard;
pub mod indexes;
pub mod list;
pub mod records;
pub mod sql;

use std::sync::{Arc, RwLock};

use forgebase_db::{Backend, TxMode, WriteTx};
use forgebase_query::CompileOptions;
use forgebase_schema::Collection;

pub use access::{
    AccessContext, Action, Authorizer, Decision, DefaultAuthorizer, FieldAccess, Predicate, Principal,
    READS_TENANT_DATA_OPTION, SecretHasher, TENANT_COLUMN, TENANT_SCOPED_OPTION, is_tenant_scoped, reads_tenant_data,
};
pub use collections::SchemaChange;
pub use error::DataError;
pub use indexes::{IndexDefinition, IndexInfo, IndexKey, IndexKind, IndexRecommendation, QueryPlan};
pub use list::{AggregateFunction, AggregateQuery, AggregateSpec, ListQuery, ListResult};
pub use records::{GetOptions, Record, RecordVersion, UpdateOptions};
pub use sql::{ColumnInfo, SqlResult, SqlStatement, TableInfo};

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
            }),
        })
    }

    /// The storage backend.
    pub fn backend(&self) -> &Arc<dyn Backend> {
        &self.inner.backend
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
    pub(crate) fn authorize(
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

    pub(crate) fn compile_options(&self, ctx: &AccessContext) -> CompileOptions {
        CompileOptions {
            allow_hidden: ctx.is_privileged(),
            ..CompileOptions::default()
        }
    }

    pub(crate) async fn begin(&self, mode: TxMode) -> Result<Box<dyn WriteTx>, DataError> {
        Ok(self.inner.backend.begin(mode).await?)
    }
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
