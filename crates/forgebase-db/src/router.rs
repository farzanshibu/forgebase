//! The set router (ADR-0029 §3.4, §3.5).
//!
//! [`DbRouter`] maps a tenant to the set that holds its rows and hands out a
//! [`SetHandle`], which implements [`Backend`] on that set's primary and,
//! when the set has replicas, routes [`Backend::fetch_routed`] reads to them
//! ([`ReplicaSet`], FB-167):
//!
//! * [`DbRouter::control`]: the control set (identity, policy, catalog,
//!   billing, topology; every table of placement class `control`);
//! * [`DbRouter::for_tenant`]: the tenant's set, from the placement cache;
//!   no tenant, or a tenant without a placement row, is control;
//! * [`DbRouter::sets`]: every routable set, control first (per-set loops,
//!   fan-out reads).
//!
//! **One set** (SQLite, or PostgreSQL without shards) behaves exactly as
//! before the router existed: every call returns the one [`Database`] the
//! server opened, nothing is read or written for routing, and
//! [`DbRouter::place_new_tenant`] writes no row.
//!
//! **Placement cache.** The router keeps every placement row in memory and
//! the topology epoch next to it. One server process per cluster (decision
//! 19), so the cache is updated by the same process that writes the rows:
//! [`DbRouter::place`] writes control and updates the cache after the
//! commit; changes made by the store functions in [`crate::topology`]
//! (adding sets or nodes) are picked up by [`DbRouter::reload`]. A cached
//! row is only ever replaced by one with a higher epoch, so a reload that
//! read before a concurrent [`DbRouter::place`] cannot undo it. Every change
//! is published on [`DbRouter::subscribe`] and rewrites `topology.json`
//! when a path is configured.
//!
//! **Frozen tenants.** During a move's switch the tenant's placement is
//! `frozen`: [`DbRouter::for_tenant`] waits (it does not refuse) until the
//! placement changes or the freeze timeout passes ([`RouterError::TenantMoving`]).

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use forgebase_core::{ErrorCode, ErrorKind};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::backend::{Backend, BoxFuture, PlanStep, Rows, RunAs, SlowQuery, Statement, TxMode, Value, WriteTx};
use crate::dialect::Dialect;
use crate::routing::{ReadIntent, ReadRouting, ReplicaNode, ReplicaOptions, ReplicaSet, check_replica};
use crate::topology::{
    self, CONTROL_SET_ID, DbNode, DbSet, NodeRole, NodeState, Placement, PlacementState, SetState, Topology,
};
use crate::{BackendKind, Database, DbError};

/// How long a request for a frozen tenant waits by default (ADR-0029 §3.8
/// `move_freeze_timeout_ms`).
pub const DEFAULT_FREEZE_TIMEOUT: Duration = Duration::from_secs(10);

/// The tenant a call is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TenantRef<'a> {
    /// No tenant (instance-wide rows): control.
    Instance,
    /// A tenant (environment id).
    Tenant(&'a str),
}

impl<'a> TenantRef<'a> {
    /// `None` and `""` are [`TenantRef::Instance`], as rows whose tenant is
    /// `NULL` or `''` live on control.
    pub fn new(tenant: Option<&'a str>) -> Self {
        match tenant {
            Some(t) if !t.is_empty() => TenantRef::Tenant(t),
            _ => TenantRef::Instance,
        }
    }

    /// The tenant id, if any.
    pub fn tenant(&self) -> Option<&'a str> {
        match self {
            TenantRef::Instance => None,
            TenantRef::Tenant(t) => Some(t),
        }
    }
}

impl<'a> From<Option<&'a str>> for TenantRef<'a> {
    fn from(tenant: Option<&'a str>) -> Self {
        TenantRef::new(tenant)
    }
}

impl<'a> From<&'a str> for TenantRef<'a> {
    fn from(tenant: &'a str) -> Self {
        TenantRef::new(Some(tenant))
    }
}

/// One set: its description and its primary [`Database`]. Implements
/// [`Backend`] (reads and the writer on the primary) and derefs to the
/// [`Database`]. Cheap to clone.
#[derive(Clone)]
pub struct SetHandle {
    set: Arc<DbSet>,
    db: Database,
    replicas: Option<ReplicaSet>,
}

impl SetHandle {
    /// A handle over `db` for `set`.
    pub fn new(set: DbSet, db: Database) -> Self {
        Self {
            set: Arc::new(set),
            db,
            replicas: None,
        }
    }

    /// Same handle, routing replica reads over `replicas` (an empty set
    /// routes nothing). Its commits then record read-your-writes tokens.
    #[must_use]
    pub fn with_replicas(mut self, replicas: ReplicaSet) -> Self {
        self.replicas = (!replicas.is_empty()).then_some(replicas);
        self
    }

    /// The set's replicas, when it has any.
    pub fn replicas(&self) -> Option<&ReplicaSet> {
        self.replicas.as_ref()
    }

    /// Set id.
    pub fn id(&self) -> &str {
        &self.set.id
    }

    /// Set number *k* (0 for control).
    pub fn number(&self) -> u16 {
        self.set.number
    }

    /// Set name.
    pub fn name(&self) -> &str {
        &self.set.name
    }

    /// The set as the topology described it when this handle was made.
    pub fn set(&self) -> &DbSet {
        &self.set
    }

    /// Whether this is the control set.
    pub fn is_control(&self) -> bool {
        self.set.is_control()
    }

    /// The set's primary database.
    pub fn database(&self) -> &Database {
        &self.db
    }
}

impl fmt::Debug for SetHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SetHandle")
            .field("id", &self.set.id)
            .field("number", &self.set.number)
            .field("location", &self.db.location())
            .field("replicas", &self.replicas.as_ref().map_or(0, ReplicaSet::len))
            .finish()
    }
}

impl Deref for SetHandle {
    type Target = Database;

    fn deref(&self) -> &Database {
        &self.db
    }
}

impl Backend for SetHandle {
    fn dialect(&self) -> &dyn Dialect {
        self.db.dialect()
    }

    fn feature_active(&self, feature: crate::Feature) -> bool {
        self.db.feature_active(feature)
    }

    fn fetch_all<'a>(&'a self, stmt: &'a Statement) -> BoxFuture<'a, Result<Rows, DbError>> {
        self.db.fetch_all(stmt)
    }

    fn fetch_routed<'a>(&'a self, stmt: &'a Statement, intent: &'a ReadIntent) -> BoxFuture<'a, Result<Rows, DbError>> {
        match &self.replicas {
            None => self.db.fetch_all(stmt),
            Some(replicas) => Box::pin(replicas.fetch(stmt, intent)),
        }
    }

    fn begin(&self, mode: TxMode) -> BoxFuture<'_, Result<Box<dyn WriteTx>, DbError>> {
        match &self.replicas {
            None => self.db.begin(mode),
            Some(replicas) => Box::pin(async move {
                let tx = self.db.begin(mode).await?;
                Ok(replicas.wrap_tx(tx))
            }),
        }
    }

    fn begin_as<'a>(&'a self, mode: TxMode, run_as: &'a RunAs) -> BoxFuture<'a, Result<Box<dyn WriteTx>, DbError>> {
        self.db.begin_as(mode, run_as)
    }

    fn fetch_all_as<'a>(&'a self, stmt: &'a Statement, run_as: &'a RunAs) -> BoxFuture<'a, Result<Rows, DbError>> {
        self.db.fetch_all_as(stmt, run_as)
    }

    fn explain<'a>(&'a self, stmt: &'a Statement) -> BoxFuture<'a, Result<Vec<PlanStep>, DbError>> {
        self.db.explain(stmt)
    }

    fn statement_writes<'a>(&'a self, stmt: &'a Statement) -> BoxFuture<'a, Result<bool, DbError>> {
        self.db.statement_writes(stmt)
    }

    fn slow_queries(&self) -> Vec<SlowQuery> {
        self.db.slow_queries()
    }

    fn clear_slow_queries(&self) {
        self.db.clear_slow_queries();
    }

    fn slow_query_threshold(&self) -> Duration {
        self.db.slow_query_threshold()
    }

    fn set_slow_query_threshold(&self, threshold: Duration) {
        self.db.set_slow_query_threshold(threshold);
    }
}

/// Opens the primary [`Database`] of a set other than control. The server
/// (FB-171) implements it: it unseals `primary.url_sealed` with the
/// keyring, or finds a local node's socket, and opens with its options.
pub trait SetConnector: Send + Sync + fmt::Debug {
    /// Opens `set`'s primary.
    fn connect<'a>(&'a self, set: &'a DbSet, primary: &'a DbNode) -> BoxFuture<'a, Result<Database, DbError>>;

    /// Opens a replica of `set` (control included) read-only
    /// (`DbOptions::read_only`). The default refuses: a connector that
    /// cannot open replicas leaves every read on the primary.
    fn connect_replica<'a>(&'a self, set: &'a DbSet, node: &'a DbNode) -> BoxFuture<'a, Result<Database, DbError>> {
        let _ = set;
        Box::pin(async move {
            Err(DbError::Unsupported(format!(
                "the set connector cannot open replica {}",
                node.name
            )))
        })
    }
}

/// Where new tenants go (`[database.sharding] placement`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlacementMode {
    /// The active set with the lowest load score (estimated bytes ÷ weight).
    #[default]
    LeastLoaded,
    /// Control, always.
    Control,
}

/// Placement of new tenants (`[database.sharding]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlacementPolicy {
    /// Least loaded or control.
    pub mode: PlacementMode,
    /// New environments of a project go to the set of its existing ones.
    pub project_affinity: bool,
}

impl Default for PlacementPolicy {
    fn default() -> Self {
        Self {
            mode: PlacementMode::LeastLoaded,
            project_affinity: true,
        }
    }
}

/// Options of [`DbRouter::open`].
#[derive(Debug, Clone)]
pub struct RouterOptions {
    /// Opens sets other than control (required once the topology has one).
    pub connector: Option<Arc<dyn SetConnector>>,
    /// `<data_dir>/topology.json`, rewritten after every change.
    pub topology_file: Option<PathBuf>,
    /// Placement of new tenants.
    pub policy: PlacementPolicy,
    /// How long a request for a frozen tenant waits.
    pub freeze_timeout: Duration,
    /// Replica routing (`[database.replicas]`).
    pub replicas: ReplicaOptions,
}

impl Default for RouterOptions {
    fn default() -> Self {
        Self {
            connector: None,
            topology_file: None,
            policy: PlacementPolicy::default(),
            freeze_timeout: DEFAULT_FREEZE_TIMEOUT,
            replicas: ReplicaOptions::default(),
        }
    }
}

/// Replica states the router opens: a replica being provisioned, promoted,
/// rebuilt or retired is not routed to.
fn routable_replica(state: NodeState) -> bool {
    matches!(
        state,
        NodeState::Ready | NodeState::CatchingUp | NodeState::Streaming | NodeState::Lagging | NodeState::Unreachable
    )
}

/// Load of one set, for placement and rebalance plans.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetLoad {
    /// Set id.
    pub set_id: String,
    /// Set number.
    pub number: u16,
    /// Weight (≥ 1).
    pub weight: u32,
    /// Sum of the estimated bytes of its placed tenants.
    pub est_bytes: u64,
    /// Number of placement rows on it (tenants without a row are not counted).
    pub tenants: u64,
}

/// The set with the lowest load score among `loads`: estimated bytes ÷
/// weight, then tenants ÷ weight, then the lower number. `None` when empty.
pub fn least_loaded(loads: &[SetLoad]) -> Option<&SetLoad> {
    loads.iter().min_by(|a, b| {
        let w = |l: &SetLoad| u128::from(l.weight.max(1));
        (u128::from(a.est_bytes) * w(b))
            .cmp(&(u128::from(b.est_bytes) * w(a)))
            .then((u128::from(a.tenants) * w(b)).cmp(&(u128::from(b.tenants) * w(a))))
            .then(a.number.cmp(&b.number))
    })
}

/// Routing errors.
#[derive(Debug, thiserror::Error)]
pub enum RouterError {
    /// A database error.
    #[error(transparent)]
    Db(#[from] DbError),
    /// The set does not exist.
    #[error("database set {0:?} does not exist")]
    UnknownSet(String),
    /// The set exists but cannot serve requests (provisioning, retired, no
    /// primary, or not opened).
    #[error("database set {0:?} is not available")]
    SetUnavailable(String),
    /// The set takes no new tenants (draining, or `hosts_tenants` off).
    #[error("database set {0:?} takes no new tenants")]
    SetClosed(String),
    /// The tenant is frozen by a move for longer than the freeze timeout.
    #[error("tenant {0:?} is being moved to another database set; retry shortly")]
    TenantMoving(String),
    /// The router was built for one fixed set ([`DbRouter::single`]).
    #[error("this server runs on one database set; placements cannot change")]
    SingleSet,
    /// Shards need PostgreSQL.
    #[error("database shards need PostgreSQL")]
    PostgresRequired,
    /// The topology has shards but the router was opened without a
    /// [`SetConnector`].
    #[error("the database topology has shards but no way to connect to them")]
    NoConnector,
}

impl ErrorCode for RouterError {
    fn code(&self) -> &'static str {
        match self {
            RouterError::Db(e) => e.code(),
            RouterError::UnknownSet(_) => "shard_not_found",
            RouterError::SetUnavailable(_) => "shard_unavailable",
            RouterError::SetClosed(_) => "shard_closed",
            RouterError::TenantMoving(_) => "tenant_moving",
            RouterError::SingleSet => "sharding_unsupported",
            RouterError::PostgresRequired => "postgres_required",
            RouterError::NoConnector => "shard_connector_missing",
        }
    }

    fn kind(&self) -> ErrorKind {
        match self {
            RouterError::Db(e) => e.kind(),
            RouterError::UnknownSet(_) => ErrorKind::NotFound,
            RouterError::SetUnavailable(_) => ErrorKind::Unavailable,
            RouterError::SetClosed(_) | RouterError::SingleSet | RouterError::PostgresRequired => {
                ErrorKind::InvalidInput
            }
            RouterError::TenantMoving(_) => ErrorKind::Conflict,
            RouterError::NoConnector => ErrorKind::Internal,
        }
    }
}

/// Routes calls to database sets. Cheap to clone.
#[derive(Debug, Clone)]
pub struct DbRouter {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    /// Built by [`DbRouter::single`]: no topology tables behind it.
    fixed: bool,
    control_db: Database,
    state: RwLock<State>,
    changes: watch::Sender<u64>,
    options: RouterOptions,
}

#[derive(Debug)]
struct State {
    topology: Topology,
    /// Open, routable sets by id (control included).
    handles: BTreeMap<String, SetHandle>,
    placements: HashMap<String, Placement>,
    written_epoch: Option<u64>,
}

impl State {
    fn new(control: &Database, topology: Topology) -> Self {
        let mut handles = BTreeMap::new();
        handles.insert(
            CONTROL_SET_ID.to_owned(),
            SetHandle::new(topology.control(), control.clone()),
        );
        Self {
            topology,
            handles,
            placements: HashMap::new(),
            written_epoch: None,
        }
    }

    fn control(&self) -> SetHandle {
        self.handles
            .get(CONTROL_SET_ID)
            .cloned()
            .expect("the control set always has a handle")
    }

    fn route(&self, tenant: &str) -> Result<SetHandle, RouterError> {
        match self.placements.get(tenant) {
            None => Ok(self.control()),
            Some(p) if p.state == PlacementState::Frozen => Err(RouterError::TenantMoving(tenant.to_owned())),
            Some(p) => self
                .handles
                .get(&p.set_id)
                .cloned()
                .ok_or_else(|| RouterError::SetUnavailable(p.set_id.clone())),
        }
    }

    /// Keeps the higher epoch of a cached and an incoming row.
    fn apply(&mut self, p: Placement) -> bool {
        match self.placements.get(&p.tenant) {
            Some(old) if old.epoch >= p.epoch => false,
            _ => {
                self.placements.insert(p.tenant.clone(), p);
                true
            }
        }
    }
}

impl DbRouter {
    /// A router over one fixed set: `db` is control and hosts every tenant.
    /// Reads and writes nothing for routing (works before migrations, on
    /// SQLite and on PostgreSQL); placements cannot change.
    pub fn single(db: Database) -> Self {
        Self::build(db, Topology::single(), RouterOptions::default(), true)
    }

    /// Reads the topology and every placement from `control` (migrated to
    /// head), opens the other routable sets through
    /// `options.connector`, and writes `topology.json` when a path is given.
    /// With only the control set this behaves as [`DbRouter::single`].
    pub async fn open(control: Database, options: RouterOptions) -> Result<Self, RouterError> {
        let router = Self::build(control, Topology::single(), options, false);
        router.reload().await?;
        Ok(router)
    }

    fn build(control: Database, topology: Topology, options: RouterOptions, fixed: bool) -> Self {
        let epoch = topology.epoch;
        Self {
            inner: Arc::new(Inner {
                fixed,
                state: RwLock::new(State::new(&control, topology)),
                control_db: control,
                changes: watch::channel(epoch).0,
                options,
            }),
        }
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, State> {
        self.inner.state.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, State> {
        self.inner.state.write().unwrap_or_else(|e| e.into_inner())
    }

    /// The control set.
    pub fn control(&self) -> SetHandle {
        self.read().control()
    }

    /// Every routable, open set, control first, then by number.
    pub fn sets(&self) -> Vec<SetHandle> {
        let mut sets: Vec<SetHandle> = self.read().handles.values().cloned().collect();
        sets.sort_by_key(SetHandle::number);
        sets
    }

    /// An open set by id.
    pub fn set(&self, id: &str) -> Option<SetHandle> {
        self.read().handles.get(id).cloned()
    }

    /// An open set by number (e.g. `cursor >> 40`, [`topology::set_number_of`]).
    pub fn set_by_number(&self, number: u16) -> Option<SetHandle> {
        self.read().handles.values().find(|h| h.number() == number).cloned()
    }

    /// Whether more than one set is in use.
    pub fn is_sharded(&self) -> bool {
        self.read().handles.len() > 1
    }

    /// The topology epoch the cache is at.
    pub fn epoch(&self) -> u64 {
        self.read().topology.epoch
    }

    /// Sets and nodes as last loaded.
    pub fn topology(&self) -> Topology {
        self.read().topology.clone()
    }

    /// The cached placement row of a tenant (`None`: control, no row).
    pub fn placement(&self, tenant: &str) -> Option<Placement> {
        self.read().placements.get(tenant).cloned()
    }

    /// Every cached placement row.
    pub fn placements(&self) -> Vec<Placement> {
        let mut all: Vec<Placement> = self.read().placements.values().cloned().collect();
        all.sort_by(|a, b| a.tenant.cmp(&b.tenant));
        all
    }

    /// Notified with the topology epoch after every change the router sees.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.inner.changes.subscribe()
    }

    /// Path of `topology.json`, when configured.
    pub fn topology_file(&self) -> Option<&Path> {
        self.inner.options.topology_file.as_deref()
    }

    /// The placement policy for new tenants.
    pub fn policy(&self) -> PlacementPolicy {
        self.inner.options.policy
    }

    /// The tenant's set without waiting: a frozen tenant is
    /// [`RouterError::TenantMoving`]. For background loops that skip
    /// moving tenants.
    pub fn try_for_tenant(&self, tenant: TenantRef<'_>) -> Result<SetHandle, RouterError> {
        match tenant {
            TenantRef::Instance => Ok(self.control()),
            TenantRef::Tenant(t) => self.read().route(t),
        }
    }

    /// The tenant's set. A frozen tenant waits until its placement changes,
    /// at most the freeze timeout, then fails with
    /// [`RouterError::TenantMoving`].
    pub async fn for_tenant(&self, tenant: TenantRef<'_>) -> Result<SetHandle, RouterError> {
        let TenantRef::Tenant(t) = tenant else {
            return Ok(self.control());
        };
        let deadline = tokio::time::Instant::now() + self.inner.options.freeze_timeout;
        let mut changes = self.inner.changes.subscribe();
        loop {
            match self.read().route(t) {
                Err(RouterError::TenantMoving(_)) => {}
                other => return other,
            }
            match tokio::time::timeout_at(deadline, changes.changed()).await {
                Ok(Ok(())) => continue,
                Ok(Err(_)) | Err(_) => return Err(RouterError::TenantMoving(t.to_owned())),
            }
        }
    }

    /// Re-reads sets, nodes and placements from control, opens new sets,
    /// drops retired ones, publishes the epoch and rewrites `topology.json`
    /// when it changed. A no-op for [`DbRouter::single`].
    pub async fn reload(&self) -> Result<(), RouterError> {
        if self.inner.fixed {
            return Ok(());
        }
        let control = &self.inner.control_db;
        let topology = topology::load(control).await?;
        let placements = topology::load_placements(control).await?;
        let others: Vec<&DbSet> = topology
            .sets
            .iter()
            .filter(|s| !s.is_control() && s.is_routable())
            .collect();
        if control.kind() == BackendKind::Sqlite && !others.is_empty() {
            return Err(RouterError::PostgresRequired);
        }
        // Open new sets outside the lock; keep the databases of known ones.
        let known: HashMap<String, Database> = self
            .read()
            .handles
            .iter()
            .map(|(id, h)| (id.clone(), h.db.clone()))
            .collect();
        let mut handles = BTreeMap::new();
        handles.insert(
            CONTROL_SET_ID.to_owned(),
            SetHandle::new(topology.control(), control.clone()),
        );
        for set in others {
            let db = match known.get(&set.id) {
                Some(db) => db.clone(),
                None => {
                    let connector = self.inner.options.connector.as_ref().ok_or(RouterError::NoConnector)?;
                    let primary = topology
                        .primary_of(&set.id)
                        .ok_or_else(|| RouterError::SetUnavailable(set.id.clone()))?;
                    connector.connect(set, primary).await?
                }
            };
            handles.insert(set.id.clone(), SetHandle::new(set.clone(), db));
        }
        // Replicas per set (control included); a set keeps its replica set
        // while its nodes are the same.
        let known_replicas: HashMap<String, ReplicaSet> = self
            .read()
            .handles
            .iter()
            .filter_map(|(id, h)| h.replicas.clone().map(|r| (id.clone(), r)))
            .collect();
        for handle in handles.values_mut() {
            if let Some(replicas) = self
                .replicas_for(&topology, handle, known_replicas.get(handle.id()))
                .await
            {
                *handle = handle.clone().with_replicas(replicas);
            }
        }
        let epoch = {
            let mut state = self.write();
            let epoch = state.topology.epoch.max(topology.epoch);
            state.topology = topology;
            state.topology.epoch = epoch;
            state.handles = handles;
            let cached = std::mem::take(&mut state.placements);
            for p in placements {
                state.apply(p);
            }
            for p in cached.into_values() {
                state.apply(p);
            }
            epoch
        };
        self.changed(epoch)
    }

    /// The replica set of `handle`'s set: its replica nodes in a routable
    /// state, opened through the connector and checked
    /// ([`check_replica`]). A node that cannot be opened or fails the check
    /// is left out with a warning: a missing replica never fails routing.
    async fn replicas_for(
        &self,
        topology: &Topology,
        handle: &SetHandle,
        old: Option<&ReplicaSet>,
    ) -> Option<ReplicaSet> {
        let options = self.inner.options.replicas;
        if options.read_routing == ReadRouting::Primary || handle.db.kind() != BackendKind::Postgres {
            return None;
        }
        let nodes: Vec<&DbNode> = topology
            .nodes_of(handle.id())
            .filter(|n| n.role == NodeRole::Replica && routable_replica(n.state))
            .collect();
        if nodes.is_empty() {
            return None;
        }
        let mut ids: Vec<String> = nodes.iter().map(|n| n.id.clone()).collect();
        ids.sort();
        if let Some(old) = old
            && old.node_ids() == ids
        {
            return Some(old.clone());
        }
        let open: HashMap<String, Database> = old
            .map(ReplicaSet::node_databases)
            .unwrap_or_default()
            .into_iter()
            .collect();
        let mut replicas = Vec::new();
        for node in nodes {
            let db = match open.get(&node.id) {
                Some(db) => db.clone(),
                None => {
                    let Some(connector) = self.inner.options.connector.as_ref() else {
                        tracing::warn!(set = %handle.id(), node = %node.name, "no set connector: replica not used");
                        continue;
                    };
                    let db = match connector.connect_replica(handle.set(), node).await {
                        Ok(db) => db,
                        Err(e) => {
                            tracing::warn!(set = %handle.id(), node = %node.name, error = %e, "replica not opened");
                            continue;
                        }
                    };
                    if let Err(e) = check_replica(&handle.db, &db, &node.name).await {
                        tracing::error!(set = %handle.id(), node = %node.name, error = %e, "replica refused");
                        db.close().await;
                        continue;
                    }
                    db
                }
            };
            replicas.push(ReplicaNode { node: node.clone(), db });
        }
        if replicas.is_empty() {
            return None;
        }
        Some(
            ReplicaSet::open_after(
                handle.id(),
                handle.db.clone(),
                replicas,
                options,
                Some(self.inner.control_db.clone()),
                old,
            )
            .await,
        )
    }

    /// Places `tenant` on set `set_id` with `state` (moves and their
    /// freezes, FB-174; administrators). Writes control (epoch + 1 when the
    /// row changes), then the cache, the notification and `topology.json`.
    pub async fn place(&self, tenant: &str, set_id: &str, state: PlacementState) -> Result<Placement, RouterError> {
        if self.inner.fixed {
            return Err(RouterError::SingleSet);
        }
        if !self.read().handles.contains_key(set_id) {
            return Err(match self.read().topology.set(set_id) {
                None => RouterError::UnknownSet(set_id.to_owned()),
                Some(_) => RouterError::SetUnavailable(set_id.to_owned()),
            });
        }
        let control = &self.inner.control_db;
        let mut tx = control.begin(TxMode::Normal).await?;
        let (placement, changed) = topology::upsert_placement(tx.as_mut(), tenant, set_id, state).await?;
        let epoch = if changed {
            Some(topology::bump_epoch(tx.as_mut()).await?)
        } else {
            None
        };
        tx.commit().await?;
        let epoch = {
            let mut s = self.write();
            s.apply(placement.clone());
            if let Some(e) = epoch {
                s.topology.epoch = s.topology.epoch.max(e);
            }
            s.topology.epoch
        };
        self.changed(epoch)?;
        Ok(placement)
    }

    /// Takes a placement row read elsewhere (e.g. after a move's flip made
    /// by another code path) into the cache if its epoch is newer. Returns
    /// whether the cache changed.
    pub fn apply_placement(&self, placement: Placement) -> bool {
        let applied = self.write().apply(placement);
        if applied {
            self.inner.changes.send_modify(|_| {});
        }
        applied
    }

    /// Loads of the sets that take new tenants.
    pub fn set_loads(&self) -> Vec<SetLoad> {
        let state = self.read();
        let mut loads: Vec<SetLoad> = state
            .handles
            .values()
            .filter(|h| h.set.takes_new_tenants())
            .map(|h| SetLoad {
                set_id: h.id().to_owned(),
                number: h.number(),
                weight: h.set.weight,
                est_bytes: 0,
                tenants: 0,
            })
            .collect();
        for p in state.placements.values() {
            if let Some(l) = loads.iter_mut().find(|l| l.set_id == p.set_id) {
                l.est_bytes = l.est_bytes.saturating_add(p.est_bytes);
                l.tenants += 1;
            }
        }
        loads.sort_by_key(|l| l.number);
        loads
    }

    /// Chooses and records the set of a new tenant (ADR-0029 §3.4) and
    /// returns it. A tenant that already has a placement keeps it.
    ///
    /// * `requested`: an administrator's choice (`POST environments` with
    ///   `shard`); the set must be active.
    /// * Otherwise, with project affinity, the set of the project's other
    ///   environments (`project_id`); then the least-loaded set (or control,
    ///   per the policy's mode).
    ///
    /// With one set nothing is written and control is returned.
    pub async fn place_new_tenant(
        &self,
        tenant: &str,
        project_id: Option<&str>,
        requested: Option<&str>,
    ) -> Result<SetHandle, RouterError> {
        if self.read().placements.contains_key(tenant) {
            return self.for_tenant(TenantRef::Tenant(tenant)).await;
        }
        if !self.is_sharded() {
            return match requested {
                Some(id) if id != CONTROL_SET_ID => Err(RouterError::UnknownSet(id.to_owned())),
                _ => Ok(self.control()),
            };
        }
        let target = match requested {
            Some(id) => {
                let set = self.set(id).ok_or_else(|| match self.read().topology.set(id) {
                    None => RouterError::UnknownSet(id.to_owned()),
                    Some(_) => RouterError::SetUnavailable(id.to_owned()),
                })?;
                if set.set().state != SetState::Active {
                    return Err(RouterError::SetClosed(id.to_owned()));
                }
                set.id().to_owned()
            }
            None => self.choose_set(tenant, project_id).await?,
        };
        self.place(tenant, &target, PlacementState::Active).await?;
        self.for_tenant(TenantRef::Tenant(tenant)).await
    }

    async fn choose_set(&self, tenant: &str, project_id: Option<&str>) -> Result<String, RouterError> {
        let policy = self.inner.options.policy;
        if policy.mode == PlacementMode::Control {
            return Ok(CONTROL_SET_ID.to_owned());
        }
        if let (true, Some(project)) = (policy.project_affinity, project_id) {
            let siblings = self
                .inner
                .control_db
                .fetch_all(&Statement::new(
                    "SELECT id FROM _forgebase_environments WHERE project_id = ?1 AND id <> ?2 ORDER BY created_at, id",
                    vec![project.into(), tenant.into()],
                ))
                .await?;
            if let Some(sibling) = siblings.rows.first().and_then(|r| r.first()).and_then(Value::as_str) {
                let set_id = self
                    .placement(sibling)
                    .map_or_else(|| CONTROL_SET_ID.to_owned(), |p| p.set_id);
                if self.set(&set_id).is_some_and(|h| h.set().takes_new_tenants()) {
                    return Ok(set_id);
                }
            }
        }
        let loads = self.set_loads();
        least_loaded(&loads)
            .map(|l| l.set_id.clone())
            .ok_or_else(|| RouterError::SetClosed("every set".into()))
    }

    fn changed(&self, epoch: u64) -> Result<(), RouterError> {
        self.inner.changes.send_replace(epoch);
        let Some(path) = self.inner.options.topology_file.as_deref() else {
            return Ok(());
        };
        let topology = {
            let state = self.read();
            if state.written_epoch == Some(epoch) {
                return Ok(());
            }
            state.topology.clone()
        };
        topology::write_topology_file(path, &topology)?;
        self.write().written_epoch = Some(epoch);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(id: &str, number: u16, weight: u32, est_bytes: u64, tenants: u64) -> SetLoad {
        SetLoad {
            set_id: id.into(),
            number,
            weight,
            est_bytes,
            tenants,
        }
    }

    #[test]
    fn tenant_refs() {
        assert_eq!(TenantRef::new(None), TenantRef::Instance);
        assert_eq!(TenantRef::from(""), TenantRef::Instance);
        assert_eq!(TenantRef::from("env1"), TenantRef::Tenant("env1"));
        assert_eq!(TenantRef::from(Some("env1")).tenant(), Some("env1"));
    }

    #[test]
    fn least_loaded_divides_by_weight_then_breaks_ties() {
        assert!(least_loaded(&[]).is_none());
        // 100 / 1 vs 150 / 2: the heavier set wins.
        let loads = [load("control", 0, 1, 100, 1), load("a", 1, 2, 150, 1)];
        assert_eq!(least_loaded(&loads).unwrap().set_id, "a");
        // Equal bytes: fewer tenants per weight.
        let loads = [
            load("control", 0, 1, 0, 3),
            load("a", 1, 1, 0, 1),
            load("b", 2, 1, 0, 2),
        ];
        assert_eq!(least_loaded(&loads).unwrap().set_id, "a");
        // Full tie: lowest number.
        let loads = [load("b", 2, 1, 0, 0), load("a", 1, 1, 0, 0)];
        assert_eq!(least_loaded(&loads).unwrap().set_id, "a");
        // No overflow at the extremes.
        let loads = [load("a", 1, 10_000, u64::MAX, 0), load("b", 2, 1, u64::MAX / 2, 0)];
        assert_eq!(least_loaded(&loads).unwrap().set_id, "a");
    }

    #[test]
    fn errors_have_codes() {
        assert_eq!(RouterError::TenantMoving("t".into()).code(), "tenant_moving");
        assert_eq!(RouterError::TenantMoving("t".into()).kind(), ErrorKind::Conflict);
        assert_eq!(RouterError::UnknownSet("s".into()).kind(), ErrorKind::NotFound);
        assert_eq!(RouterError::PostgresRequired.code(), "postgres_required");
        let db: RouterError = DbError::Closed.into();
        assert_eq!(db.code(), "database_closed");
    }
}
