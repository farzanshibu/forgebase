//! Managed replicas and the topology service in the server (FB-175: the
//! FB-168 wiring and `/api/system/database/*`).
//!
//! * [`Replicas`] (one per server): the [`LocalReplicas`] of this host,
//!   started after the primary ([`Replicas::open`]) and stopped before it
//!   ([`Replicas::stop`]); the control primary's engine handle (it changes
//!   on a promote); the background loops (lost slots, lag history,
//!   `[[database.nodes]]` seeding, `pg_hba.conf` peers).
//! * [`ReplicaConnector`]: the router's [`SetConnector`]. Local replicas
//!   are opened over their socket as `forgebase_reader`
//!   (`DbOptions::read_only`); remote and external nodes, and shard
//!   primaries, from their sealed URL (FB-171's [`NodeConnector`]).
//! * The control primary at start is the `ready` primary of
//!   `<data_dir>/topology.json` ([`control_layout`]): after a promote it is
//!   `postgres/nodes/<name>`.
//! * [`TopologyOps`] for the admin API: reads, synchronous changes, and the
//!   long operations as reserved jobs on queue
//!   [`api::OPERATIONS_QUEUE`] ([`register_jobs`]).
//! * A promote of **control's** primary needs a restart of this server:
//!   its database handle cannot switch pools while it runs, so the
//!   topology reports `restart_required` and the job finishes on the next
//!   start (its checkpoint was replicated before the fence). Shard sets
//!   switch through the router.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Duration;

use forgebase_api::system_database::{self as api, OpsFuture, TopologyError, TopologyOps, kinds};
use forgebase_auth::crypto::Keyring;
use forgebase_core::config::topology_config::{DeclaredRole, ReplicasConfig};
use forgebase_data::{AccessContext, DataService};
use forgebase_db::topology::{
    self, CONTROL_SET_ID, DbNode, DbSet, NodeKind, NodeOrigin, NodeRole, NodeState, SetKind, SetState,
};
use forgebase_db::{
    Backend, BackendKind, BoxFuture, Database, DbError, DbOptions, DbRouter, DbTarget, RouterError, SetConnector,
    Statement, TxMode, Value,
};
use forgebase_jobs::{HandlerError, Job, JobContext, JobFilter, JobRegistry, JobService, JobStatus, NewJob};
use forgebase_pgengine::replication::service::{self, ServiceError};
use forgebase_pgengine::shard::LocalShards;
use forgebase_pgengine::standby::{LocalReplicas, OldPrimary, SslMode};
use forgebase_pgengine::{EngineConfig, EngineHandle, Layout, Role, RoleSecrets, Secret};
use serde_json::{Value as Json, json};
use tokio::task::JoinHandle;

use crate::ServerError;
use crate::shards::{NodeConnector, open_node_url, seal_node_url};

/// How often lost slots are checked and config nodes refreshed.
const HEALTH_PERIOD: Duration = Duration::from_secs(60);
/// How often lag is sampled for the history (the minute keeps the highest).
const LAG_SAMPLE_PERIOD: Duration = Duration::from_secs(10);
/// Minutes of lag history kept per node.
const LAG_MINUTES: usize = 60;
/// How long a remote promote waits for the agent.
const REMOTE_PROMOTE_TIMEOUT: Duration = Duration::from_secs(120);
/// Stages after which an operation cannot be cancelled.
const POINT_OF_NO_RETURN: &[&str] = &["fencing", "promoting", "switching", "activating", "cleanup"];

/// How long a remote shard's add waits for its primary to join (FB-174).
const REMOTE_PRIMARY_WAIT: Duration = Duration::from_secs(3600);

// ---------------------------------------------------------------------------
// Start-up helpers (used by `database.rs`).
// ---------------------------------------------------------------------------

/// The control primary's cluster layout: the `ready` primary of control in
/// `<data_dir>/topology.json` when it is a local node other than the
/// original (`postgres/nodes/<name>` after a promote), else
/// `postgres/data`.
pub fn control_layout(data_dir: &Path) -> Layout {
    let original = Layout::primary(data_dir);
    let Ok(Some(file)) = topology::read_topology_file(&data_dir.join(topology::TOPOLOGY_FILE)) else {
        return original;
    };
    let Some(p) = file
        .topology
        .nodes
        .iter()
        .find(|n| n.set_id == CONTROL_SET_ID && n.role == NodeRole::Primary && n.state == NodeState::Ready)
    else {
        return original;
    };
    if p.kind != NodeKind::Local || p.name == forgebase_pgengine::standby::OLD_PRIMARY {
        return original;
    }
    match Layout::node(data_dir, &p.name, forgebase_pgengine::NodeKind::Primary) {
        Ok(l) if l.pgdata().join("PG_VERSION").is_file() => l,
        _ => {
            tracing::warn!(node = %p.name, "topology.json names a control primary without data; using postgres/data");
            original
        }
    }
}

/// `[database.replicas] slot_wal_keep_size` as the engine's slot limits.
pub fn slot_limits(cfg: &ReplicasConfig) -> forgebase_pgengine::replication::SlotLimits {
    forgebase_pgengine::replication::SlotLimits {
        wal_keep_size: cfg.slot_wal_keep_size.clone().into(),
        ..forgebase_pgengine::replication::SlotLimits::DEFAULT
    }
}

/// `[database.replicas]` and `[database.sharding]` as router options.
fn router_options(config: &forgebase_core::Config) -> forgebase_db::RouterOptions {
    use forgebase_core::config::topology_config::{ReadRouting as R, TenantPlacement as P};
    let r = &config.database.topology.replicas;
    let s = &config.database.topology.sharding;
    forgebase_db::RouterOptions {
        policy: forgebase_db::router::PlacementPolicy {
            mode: match s.placement {
                P::LeastLoaded => forgebase_db::router::PlacementMode::LeastLoaded,
                P::Control => forgebase_db::router::PlacementMode::Control,
            },
            project_affinity: s.project_affinity,
        },
        freeze_timeout: Duration::from_millis(s.move_freeze_timeout_ms),
        replicas: forgebase_db::ReplicaOptions {
            read_routing: match r.read_routing {
                R::Primary => forgebase_db::ReadRouting::Primary,
                R::Replicas => forgebase_db::ReadRouting::Replicas,
            },
            read_your_writes: r.read_your_writes,
            lsn_wait: Duration::from_millis(r.lsn_wait_ms),
            max_lag: Duration::from_millis(r.max_lag_ms),
            max_lag_bytes: r.max_lag_bytes,
            health_interval: Duration::from_millis(r.health_interval_ms),
            ..forgebase_db::ReplicaOptions::default()
        },
        ..forgebase_db::RouterOptions::default()
    }
}

// ---------------------------------------------------------------------------
// The service object.
// ---------------------------------------------------------------------------

/// Managed replicas, the router hook-up and the topology service of one server.
pub struct Replicas {
    data_dir: PathBuf,
    config: forgebase_core::Config,
    keyring: Keyring,
    local: Arc<LocalReplicas>,
    /// Shard primaries on this host (FB-174).
    shards: Arc<LocalShards>,
    /// Tenant moves, drains and rebalancing (FB-174).
    moves: OnceLock<crate::moves::Moves>,
    primary: RwLock<Option<Arc<EngineHandle>>>,
    promoted: tokio::sync::Mutex<Vec<Arc<EngineHandle>>>,
    router: OnceLock<DbRouter>,
    jobs: OnceLock<JobService>,
    data: OnceLock<DataService>,
    lag: Mutex<HashMap<String, VecDeque<api::LagSample>>>,
    restart_required: Mutex<Option<String>>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    changes: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for Replicas {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Replicas")
            .field("data_dir", &self.data_dir)
            .finish_non_exhaustive()
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Replicas {
    /// Loads the keyring and, with the bundled engine running as `engine`,
    /// starts every provisioned local replica (a failure is logged; the
    /// replica stays out of rotation).
    ///
    /// # Errors
    /// The keyring cannot be read or created; a bad engine configuration.
    pub async fn open(
        config: &forgebase_core::Config,
        data_dir: &Path,
        engine: Option<Arc<EngineHandle>>,
    ) -> Result<Arc<Self>, ServerError> {
        let path = forgebase_auth::AuthConfig::for_data_dir(data_dir).master_key_path;
        let keyring = tokio::task::spawn_blocking(move || Keyring::load_or_create(&path))
            .await
            .map_err(|e| ServerError::Backend(e.to_string()))?
            .map_err(|e| ServerError::Backend(e.to_string()))?;
        let engine_cfg: EngineConfig = if config.database.backend == forgebase_core::config::DatabaseBackend::Postgres {
            crate::database::engine_config(&config.database)?
        } else {
            EngineConfig::default()
        };
        let local = Arc::new(LocalReplicas::new(data_dir, &engine_cfg));
        let shards = Arc::new(LocalShards::new(data_dir, &engine_cfg));
        if engine.is_some() {
            // Shard primaries first: their local replicas follow them.
            for (name, set, r) in shards.start_existing().await {
                match r {
                    Ok(()) => tracing::info!(node = %name, set = %set, "local shard primary started"),
                    Err(e) => tracing::error!(node = %name, set = %set, error = %e, "local shard primary did not start"),
                }
            }
            for (name, r) in local.start_existing().await {
                match r {
                    Ok(()) => tracing::info!(replica = %name, "local replica started"),
                    Err(e) => tracing::error!(replica = %name, error = %e, "local replica did not start"),
                }
            }
        }
        Ok(Arc::new(Self {
            data_dir: data_dir.to_path_buf(),
            config: config.clone(),
            keyring,
            local,
            shards,
            moves: OnceLock::new(),
            primary: RwLock::new(engine),
            promoted: tokio::sync::Mutex::new(Vec::new()),
            router: OnceLock::new(),
            jobs: OnceLock::new(),
            data: OnceLock::new(),
            lag: Mutex::new(HashMap::new()),
            restart_required: Mutex::new(None),
            tasks: Mutex::new(Vec::new()),
            changes: tokio::sync::Mutex::new(()),
        }))
    }

    /// The keyring node URLs are sealed with.
    pub fn keyring(&self) -> &Keyring {
        &self.keyring
    }

    /// The local replicas of this host.
    pub fn local(&self) -> &LocalReplicas {
        &self.local
    }

    /// The shard primaries of this host (FB-174).
    pub fn shards(&self) -> &Arc<LocalShards> {
        &self.shards
    }

    /// Hands over the move service (FB-174; after [`Replicas::attach`]).
    pub fn attach_moves(&self, moves: crate::moves::Moves) {
        let _ = self.moves.set(moves);
    }

    fn moves(&self) -> Result<&crate::moves::Moves, TopologyError> {
        self.moves
            .get()
            .ok_or_else(|| TopologyError::new(503, api::codes::TOPOLOGY_UNAVAILABLE, "the server is starting"))
    }

    /// How the engine reaches control as an administrator, and how targets
    /// reach it: the managed engine's superuser over its socket, or the
    /// configured URL of an external server.
    pub fn control_admin_url(&self) -> Option<forgebase_db::SecretString> {
        match self.primary() {
            Some(h) => Some(forgebase_db::SecretString::new(h.url(Role::Super).expose().to_owned())),
            None => crate::moves::external_control_url(&self.config),
        }
    }

    /// The running local primary of `set` (control's engine, or a local
    /// shard node).
    async fn set_primary(&self, set: &str) -> Result<Arc<EngineHandle>, TopologyError> {
        if set == CONTROL_SET_ID {
            return self.need_primary();
        }
        let control = self.control()?;
        let topo = topology::load(&control).await.map_err(db_err)?;
        let node = topo
            .primary_of(set)
            .filter(|n| n.kind == NodeKind::Local)
            .ok_or_else(|| {
                TopologyError::new(
                    400,
                    "node_unsupported",
                    format!("{set}'s primary is not on this server: add an external replica of it"),
                )
            })?;
        self.shards.get(&node.name).await.ok_or_else(|| {
            TopologyError::new(
                503,
                api::codes::NODE_UNREACHABLE,
                format!("local shard primary {} is not running", node.name),
            )
        })
    }

    /// The control primary's engine, when this server runs it.
    pub fn primary(&self) -> Option<Arc<EngineHandle>> {
        self.primary
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// The router over control `db` with this server's connector, the
    /// `[database.replicas]` / `[database.sharding]` options and
    /// `topology.json`. SQLite has one fixed set.
    ///
    /// # Errors
    /// The topology cannot be read.
    pub async fn open_router(self: &Arc<Self>, db: &Database) -> Result<DbRouter, ServerError> {
        if db.kind() != BackendKind::Postgres {
            return Ok(DbRouter::single(db.clone()));
        }
        let mut options = router_options(&self.config);
        options.connector = Some(Arc::new(ReplicaConnector {
            inner: NodeConnector::new(self.keyring.clone(), self.config.database.clone()),
            replicas: self.clone(),
        }));
        options.topology_file = Some(self.data_dir.join(topology::TOPOLOGY_FILE));
        DbRouter::open(db.clone(), options)
            .await
            .map_err(|e| ServerError::Backend(format!("cannot open the database topology: {e}")))
    }

    /// Hands over the router, the job service and the data service, seeds
    /// `[[database.nodes]]` and starts the background loops (PostgreSQL).
    pub async fn attach(self: &Arc<Self>, router: DbRouter, jobs: JobService, data: DataService) {
        let pg = router.control().database().kind() == BackendKind::Postgres;
        let _ = self.router.set(router);
        let _ = self.jobs.set(jobs);
        let _ = self.data.set(data);
        if !pg {
            return;
        }
        if let Err(e) = self.seed_config_nodes().await {
            tracing::warn!(error = %e.message, "[[database.nodes]] not seeded");
        }
        if let Err(e) = self.apply_hba_peers().await {
            tracing::warn!(error = %e.message, "replication peers not applied to pg_hba.conf");
        }
        let health = {
            let me = Arc::downgrade(self);
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(HEALTH_PERIOD);
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    tick.tick().await;
                    let Some(me) = me.upgrade() else { return };
                    me.health_round().await;
                }
            })
        };
        let lag = {
            let me = Arc::downgrade(self);
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(LAG_SAMPLE_PERIOD);
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    tick.tick().await;
                    let Some(me) = me.upgrade() else { return };
                    me.sample_lag();
                }
            })
        };
        lock(&self.tasks).extend([health, lag]);
    }

    /// Stops the loops, every local replica and a promoted primary (before
    /// the server stops its original engine).
    pub async fn stop(&self) {
        for t in lock(&self.tasks).drain(..) {
            t.abort();
        }
        self.local.stop_all().await;
        self.shards.stop_all().await;
        for h in self.promoted.lock().await.drain(..) {
            if let Err(e) = h.stop().await {
                tracing::warn!(error = %e, "stopping the promoted primary failed");
            }
        }
    }

    /// The topology service for the admin API (PostgreSQL only).
    pub fn ops(self: &Arc<Self>) -> Option<Arc<dyn TopologyOps>> {
        (self.config.database.backend == forgebase_core::config::DatabaseBackend::Postgres)
            .then(|| self.clone() as Arc<dyn TopologyOps>)
    }

    fn router(&self) -> Result<&DbRouter, TopologyError> {
        self.router
            .get()
            .ok_or_else(|| TopologyError::new(503, api::codes::TOPOLOGY_UNAVAILABLE, "the server is starting"))
    }

    fn jobs(&self) -> Result<&JobService, TopologyError> {
        self.jobs
            .get()
            .ok_or_else(|| TopologyError::new(503, api::codes::TOPOLOGY_UNAVAILABLE, "the server is starting"))
    }

    fn control(&self) -> Result<Database, TopologyError> {
        let db = self.router()?.control().database().clone();
        if db.kind() != BackendKind::Postgres {
            return Err(TopologyError::postgres_required());
        }
        Ok(db)
    }

    fn need_primary(&self) -> Result<Arc<EngineHandle>, TopologyError> {
        self.primary().ok_or_else(|| {
            TopologyError::new(
                400,
                "node_unsupported",
                "managed replicas need the bundled PostgreSQL engine ([database.postgres] mode = \"managed\"); \
                 with an external server, add external replicas",
            )
        })
    }

    fn topology_file(&self) -> PathBuf {
        self.data_dir.join(topology::TOPOLOGY_FILE)
    }

    async fn reload(&self) {
        if let Ok(r) = self.router()
            && let Err(e) = r.reload().await
        {
            tracing::warn!(error = %e, "router reload after a topology change failed");
        }
    }

    // -- background ---------------------------------------------------------

    async fn health_round(&self) {
        let (Ok(control), Some(primary)) = (self.control(), self.primary()) else {
            return;
        };
        let has_local = !self.local.names().await.is_empty();
        if has_local || self.has_remote().await {
            match service::mark_lost_slots(&control, CONTROL_SET_ID, &primary).await {
                Ok(changed) if !changed.is_empty() => {
                    tracing::warn!(nodes = ?changed, "replicas lost their replication slot and need a rebuild");
                    self.reload().await;
                }
                Ok(_) => {}
                Err(e) => tracing::debug!(error = %e, "slot check failed"),
            }
        }
    }

    async fn has_remote(&self) -> bool {
        self.router
            .get()
            .is_some_and(|r| r.topology().nodes.iter().any(|n| n.kind == NodeKind::Remote))
    }

    fn sample_lag(&self) {
        let Some(router) = self.router.get() else { return };
        let minute = {
            let now = forgebase_core::now_rfc3339();
            format!("{}:00Z", now.get(..16).unwrap_or(&now))
        };
        let mut lag = lock(&self.lag);
        for set in router.sets() {
            let Some(rs) = set.replicas() else { continue };
            for st in rs.status() {
                let q = lag.entry(st.node_id.clone()).or_default();
                match q.back_mut() {
                    Some(last) if last.at == minute => {
                        last.lag_ms = last.lag_ms.max(st.lag_ms);
                        last.lag_bytes = last.lag_bytes.max(st.lag_bytes);
                    }
                    _ => {
                        q.push_back(api::LagSample {
                            at: minute.clone(),
                            lag_ms: st.lag_ms,
                            lag_bytes: st.lag_bytes,
                        });
                        while q.len() > LAG_MINUTES {
                            q.pop_front();
                        }
                    }
                }
            }
        }
    }

    /// `[[database.nodes]]`: external nodes from the file, `origin =
    /// config`. A declared node whose variable is unset is skipped with a
    /// warning; a config node no longer declared is retired.
    async fn seed_config_nodes(&self) -> Result<(), TopologyError> {
        let control = self.control()?;
        let decls = &self.config.database.topology.nodes;
        let topo = topology::load(&control).await.map_err(db_err)?;
        let mut changed = false;
        for d in decls {
            let Some(set) = topo
                .sets
                .iter()
                .find(|s| s.id == d.set || s.name.eq_ignore_ascii_case(&d.set))
            else {
                tracing::warn!(node = %d.name, set = %d.set, "[[database.nodes]] names an unknown set; skipped");
                continue;
            };
            let Ok(url) = std::env::var(d.url_env.trim()) else {
                tracing::warn!(node = %d.name, var = %d.url_env, "[[database.nodes]] url_env is not set; skipped");
                continue;
            };
            let role = match d.role {
                DeclaredRole::Replica => NodeRole::Replica,
                DeclaredRole::Primary => NodeRole::Primary,
            };
            let existing = topo
                .nodes
                .iter()
                .find(|n| n.name.eq_ignore_ascii_case(&d.name) && n.state != NodeState::Retired);
            let id = match existing {
                Some(n) if n.origin != NodeOrigin::Config => {
                    tracing::warn!(node = %d.name, "[[database.nodes]] name is used by a node added from the API; skipped");
                    continue;
                }
                Some(n) => n.id.clone(),
                None => {
                    let state = if role == NodeRole::Primary {
                        NodeState::Ready
                    } else {
                        NodeState::CatchingUp
                    };
                    topology::insert_node(
                        &control,
                        &topology::NewNode {
                            set_id: set.id.clone(),
                            name: d.name.clone(),
                            kind: NodeKind::External,
                            role,
                            url_sealed: None,
                            state,
                            weight: d.weight,
                            origin: NodeOrigin::Config,
                        },
                    )
                    .await
                    .map_err(db_err)?
                    .id
                }
            };
            let sealed = seal_node_url(&self.keyring, &id, url.trim()).map_err(TopologyError::internal)?;
            let same = existing.is_some_and(|n| {
                n.weight == d.weight
                    && n.url_sealed
                        .as_deref()
                        .and_then(|s| open_node_url(&self.keyring, &n.id, s).ok())
                        .is_some_and(|u| u == url.trim())
            });
            if !same {
                set_node_url(&control, &id, Some(&sealed), Some(d.weight)).await?;
                changed = true;
            }
        }
        for n in topo
            .nodes
            .iter()
            .filter(|n| n.origin == NodeOrigin::Config && n.state != NodeState::Retired)
        {
            if !decls.iter().any(|d| d.name.eq_ignore_ascii_case(&n.name)) {
                retire_node(&control, n).await?;
                changed = true;
            }
        }
        if changed {
            self.reload().await;
        }
        Ok(())
    }

    /// Writes the agents' and the server's addresses into the primary's
    /// `pg_hba.conf` (FB-168: remote replicas clone and stream over TLS).
    async fn apply_hba_peers(&self) -> Result<bool, TopologyError> {
        let (Some(primary), Ok(control)) = (self.primary(), self.control()) else {
            return Ok(false);
        };
        let rows = control
            .fetch_all(&Statement::raw(
                "SELECT a.address FROM _forgebase_db_node_agents a JOIN _forgebase_db_nodes n ON n.id = a.node_id \
                 WHERE n.state <> 'retired' AND a.address IS NOT NULL",
            ))
            .await
            .map_err(db_err)?;
        let agents: Vec<IpAddr> = rows
            .rows
            .iter()
            .filter_map(|r| r.first().and_then(Value::as_str).and_then(|a| a.parse().ok()))
            .collect();
        let readers: Vec<IpAddr> = primary
            .tcp_addr()
            .map(|a| a.ip())
            .filter(|ip| !ip.is_unspecified())
            .into_iter()
            .collect();
        forgebase_pgengine::hba::apply_peers(&primary, &forgebase_pgengine::replication::HbaInput { agents, readers })
            .await
            .map_err(|e| TopologyError::internal(e.to_string()))
    }

    // -- operations ---------------------------------------------------------

    async fn active_ops(&self) -> Result<Vec<Job>, TopologyError> {
        let jobs = self.jobs()?;
        let mut out = Vec::new();
        for status in [JobStatus::Queued, JobStatus::Running] {
            let page = jobs
                .list(
                    &AccessContext::system(),
                    JobFilter {
                        status: Some(status),
                        queue: Some(api::OPERATIONS_QUEUE.to_owned()),
                        limit: Some(200),
                        ..JobFilter::default()
                    },
                )
                .await
                .map_err(job_err)?;
            out.extend(page.items);
        }
        Ok(out)
    }

    /// One topology change per set at a time: refuses when an operation on
    /// `set` is queued or running, else enqueues `kind`.
    async fn enqueue(
        &self,
        ctx: &AccessContext,
        kind: &str,
        set: &str,
        node: Option<&str>,
        mut payload: Json,
    ) -> Result<api::Operation, TopologyError> {
        let _one = self.changes.lock().await;
        if let Some(busy) = self
            .active_ops()
            .await?
            .into_iter()
            .find(|j| j.payload.get("set").and_then(Json::as_str) == Some(set))
        {
            return Err(TopologyError::new(
                409,
                api::codes::OPERATION_IN_PROGRESS,
                format!(
                    "operation {} ({}) is already changing {set}; wait for it or cancel it",
                    busy.id, busy.kind
                ),
            ));
        }
        if let Some(o) = payload.as_object_mut() {
            o.insert("set".into(), json!(set));
            if let Some(n) = node {
                o.insert("node".into(), json!(n));
            }
        }
        let mut job = NewJob::new(kind, payload);
        job.queue = Some(api::OPERATIONS_QUEUE.to_owned());
        job.max_attempts = Some(3);
        let (job, _) = self.jobs()?.enqueue(ctx, job).await.map_err(job_err)?;
        Ok(operation_of(&job))
    }

    async fn node_row(&self, id: &str) -> Result<DbNode, TopologyError> {
        let control = self.control()?;
        topology::load(&control)
            .await
            .map_err(db_err)?
            .nodes
            .into_iter()
            .find(|n| n.id == id && n.state != NodeState::Retired)
            .ok_or_else(|| TopologyError::new(404, "node_not_found", format!("node {id} not found")))
    }

    fn statuses(&self) -> HashMap<String, Live> {
        let mut out = HashMap::new();
        if let Some(r) = self.router.get() {
            for set in r.sets() {
                if let Some(rs) = set.replicas() {
                    for st in rs.status() {
                        out.insert(
                            st.node_id.clone(),
                            Live {
                                state: st.state,
                                in_rotation: st.in_rotation,
                                lag_ms: st.lag_ms,
                                lag_bytes: st.lag_bytes,
                                replay_lsn: st.replay_lsn.map(|l| l.to_string()),
                                last_error: st.last_error.clone(),
                            },
                        );
                    }
                }
            }
        }
        out
    }

    async fn node_infos(&self, control: &Database) -> Result<Vec<api::NodeInfo>, TopologyError> {
        let views = service::nodes(control, None).await.map_err(svc_err)?;
        let st = self.statuses();
        Ok(views
            .into_iter()
            .map(|v| {
                let s = st.get(&v.id).cloned();
                node_info(v, s.as_ref())
            })
            .collect())
    }
}

// ---------------------------------------------------------------------------
// Conversions and small helpers.
// ---------------------------------------------------------------------------

fn db_err(e: DbError) -> TopologyError {
    let status = forgebase_api::error::status_for(forgebase_core::ErrorCode::kind(&e)).as_u16();
    TopologyError::new(status, forgebase_core::ErrorCode::code(&e), e.to_string())
}

fn svc_err(e: ServiceError) -> TopologyError {
    match e {
        ServiceError::Db(d) => db_err(d),
        e => TopologyError::new(e.status(), e.code(), e.to_string()),
    }
}

fn router_err(e: RouterError) -> TopologyError {
    match e {
        RouterError::TenantMoving(t) => TopologyError::new(
            409,
            api::codes::TENANT_MOVING,
            format!("tenant {t} is being moved; wait for the move to finish"),
        ),
        RouterError::PostgresRequired => TopologyError::postgres_required(),
        RouterError::Unsupported(m) => TopologyError::new(400, api::codes::SHARDING_UNSUPPORTED, m),
        RouterError::UnknownSet(s) => TopologyError::new(404, "shard_not_found", format!("set {s} not found")),
        RouterError::Db(d) => db_err(d),
        other => TopologyError::internal(other.to_string()),
    }
}

/// A move-engine error as an API error (FB-174).
fn move_err(e: forgebase_db::moves::MoveError) -> TopologyError {
    use forgebase_core::{ErrorCode, ErrorKind};
    if let forgebase_db::moves::MoveError::Router(r) = e {
        return router_err(r);
    }
    let status = match e.kind() {
        ErrorKind::NotFound => 404,
        ErrorKind::Conflict => 409,
        ErrorKind::InvalidInput => 400,
        ErrorKind::Unavailable => 503,
        _ => 500,
    };
    TopologyError::new(status, e.code(), e.to_string())
}

fn plan_view(p: &forgebase_db::moves::RebalancePlan) -> api::RebalancePlanView {
    let loads = |v: &[forgebase_db::moves::SetScore]| {
        v.iter()
            .map(|s| api::SetLoadInfo {
                set_id: s.set_id.clone(),
                name: s.name.clone(),
                weight: s.weight,
                tenants: s.tenants,
                bytes: s.bytes,
                score: s.score,
            })
            .collect()
    };
    let expires = forgebase_db::moves::rebalance::expires_at(p);
    api::RebalancePlanView {
        id: p.id.clone(),
        created_at: p.created_at.clone(),
        expires_at: expires,
        moves: p
            .moves
            .iter()
            .map(|m| api::PlannedMoveInfo {
                tenant: m.tenant.clone(),
                project: m.project.clone(),
                from: m.from.clone(),
                to: m.to.clone(),
                rows: m.rows,
                bytes: m.bytes,
                est_freeze_ms: m.est_freeze_ms,
            })
            .collect(),
        before: loads(&p.before),
        after: loads(&p.after),
        spread_before: p.spread_before,
        spread_after: p.spread_after,
        est_freeze_ms: p.moves.iter().map(|m| m.est_freeze_ms).sum(),
    }
}

fn job_err(e: forgebase_jobs::JobError) -> TopologyError {
    let status = forgebase_api::error::status_for(forgebase_core::ErrorCode::kind(&e)).as_u16();
    TopologyError::new(status, forgebase_core::ErrorCode::code(&e), e.to_string())
}

fn operation_of(j: &Job) -> api::Operation {
    let progress = j.progress.as_ref().map(|p| api::OperationProgress {
        percent: p.percent,
        stage: p.stage.clone(),
        message: p.message.clone(),
    });
    let stage = progress.as_ref().and_then(|p| p.stage.clone()).unwrap_or_default();
    let cancellable = cancellable(j.status, j.cancel_requested, &stage);
    api::Operation {
        id: j.id.clone(),
        kind: j.kind.clone(),
        status: j.status.as_str().to_owned(),
        set: j.payload.get("set").and_then(Json::as_str).map(str::to_owned),
        node: j.payload.get("node").and_then(Json::as_str).map(str::to_owned),
        progress,
        cancel_requested: j.cancel_requested,
        cancellable,
        result: j.result.clone(),
        last_error: j.last_error.clone(),
        created_by: j.created_by.clone(),
        created_at: j.created_at.clone(),
        started_at: j.started_at.clone(),
        finished_at: j.finished_at.clone(),
    }
}

/// The router's live view of a replica in rotation.
#[derive(Debug, Clone)]
struct Live {
    state: NodeState,
    in_rotation: bool,
    lag_ms: u64,
    lag_bytes: u64,
    replay_lsn: Option<String>,
    last_error: Option<String>,
}

fn cancellable(status: JobStatus, cancel_requested: bool, stage: &str) -> bool {
    match status {
        JobStatus::Queued => true,
        JobStatus::Running => !cancel_requested && !POINT_OF_NO_RETURN.contains(&stage),
        _ => false,
    }
}

fn node_info(v: service::NodeView, st: Option<&Live>) -> api::NodeInfo {
    api::NodeInfo {
        id: v.id,
        set: v.set_id,
        name: v.name,
        kind: v.kind.as_str().to_owned(),
        role: v.role.as_str().to_owned(),
        // The router's health sample is newer than the row for a replica in rotation.
        state: st.map_or_else(|| v.state.as_str().to_owned(), |s| s.state.as_str().to_owned()),
        weight: v.weight,
        engine_version: v.engine_version,
        system_identifier: v.system_identifier,
        timeline: v.timeline,
        origin: v.origin.as_str().to_owned(),
        in_rotation: st.map(|s| s.in_rotation),
        lag_ms: st.map(|s| s.lag_ms),
        lag_bytes: st.map(|s| s.lag_bytes),
        replay_lsn: st.and_then(|s| s.replay_lsn.clone()),
        last_error: st.and_then(|s| s.last_error.clone()),
        agent: v.agent.and_then(|a| serde_json::to_value(a).ok()),
        created_at: v.created_at,
        updated_at: v.updated_at,
    }
}

fn set_info(s: &DbSet, tenants: u64, est_bytes: u64) -> api::SetInfo {
    api::SetInfo {
        id: s.id.clone(),
        name: s.name.clone(),
        kind: s.kind.as_str().to_owned(),
        number: s.number,
        state: s.state.as_str().to_owned(),
        weight: s.weight,
        hosts_tenants: s.hosts_tenants,
        tenants,
        est_bytes,
    }
}

async fn set_node_url(
    control: &Database,
    id: &str,
    sealed: Option<&str>,
    weight: Option<u32>,
) -> Result<(), TopologyError> {
    let mut tx = control.begin(TxMode::Normal).await.map_err(db_err)?;
    tx.execute(&Statement::new(
        "UPDATE _forgebase_db_nodes SET url_sealed = COALESCE(CAST(?2 AS TEXT), url_sealed), \
         weight = COALESCE(CAST(?3 AS BIGINT), weight), updated_at = ?4 WHERE id = ?1",
        vec![
            id.into(),
            sealed.map(str::to_owned).into(),
            weight.map(i64::from).into(),
            forgebase_core::now_rfc3339().into(),
        ],
    ))
    .await
    .map_err(db_err)?;
    topology::bump_epoch(tx.as_mut()).await.map_err(db_err)?;
    tx.commit().await.map_err(db_err)
}

/// Retires a node row (its name is freed: `<name>~<id>`).
async fn retire_node(control: &Database, n: &DbNode) -> Result<(), TopologyError> {
    let mut tx = control.begin(TxMode::Normal).await.map_err(db_err)?;
    tx.execute(&Statement::new(
        "UPDATE _forgebase_db_nodes SET state = 'retired', name = ?2, updated_at = ?3 WHERE id = ?1",
        vec![
            n.id.clone().into(),
            format!("{}~{}", n.name, n.id).into(),
            forgebase_core::now_rfc3339().into(),
        ],
    ))
    .await
    .map_err(db_err)?;
    topology::bump_epoch(tx.as_mut()).await.map_err(db_err)?;
    tx.commit().await.map_err(db_err)
}

fn check_confirm(confirm: &str, name: &str) -> Result<(), TopologyError> {
    if confirm == name {
        Ok(())
    } else {
        Err(TopologyError::new(
            400,
            "confirm_mismatch",
            format!("type the name {name:?} to confirm"),
        ))
    }
}

fn reader_password(data_dir: &Path) -> Result<Secret, TopologyError> {
    let layout = Layout::primary(data_dir);
    RoleSecrets::load(&layout.secrets_file())
        .map_err(|e| TopologyError::internal(e.to_string()))?
        .and_then(|s| s.password(Role::Reader).map(|p| Secret::new(p.to_owned())))
        .ok_or_else(|| TopologyError::internal("roles.secret has no forgebase_reader password"))
}

/// The TLS mode remote replicas use: `verify-full` with an operator
/// certificate, `require` with the self-signed one.
fn replication_sslmode(config: &forgebase_core::Config) -> SslMode {
    if config.database.postgres.tls_cert.is_empty() {
        SslMode::Require
    } else {
        SslMode::VerifyFull
    }
}

// ---------------------------------------------------------------------------
// The router's connector.
// ---------------------------------------------------------------------------

/// Opens local replicas over their socket, everything else from its
/// sealed URL ([`NodeConnector`]).
#[derive(Clone)]
pub struct ReplicaConnector {
    inner: NodeConnector,
    replicas: Arc<Replicas>,
}

impl std::fmt::Debug for ReplicaConnector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReplicaConnector").finish_non_exhaustive()
    }
}

impl SetConnector for ReplicaConnector {
    fn connect<'a>(&'a self, set: &'a DbSet, primary: &'a DbNode) -> BoxFuture<'a, Result<Database, DbError>> {
        Box::pin(async move {
            if primary.kind != NodeKind::Local {
                return self.inner.connect(set, primary).await;
            }
            // FB-174: a shard primary on this server, over its socket.
            let h = self.replicas.shards.get(&primary.name).await.ok_or_else(|| {
                DbError::Unavailable(format!("local shard primary {} is not running on this server", primary.name))
            })?;
            let cfg = &self.replicas.config.database;
            let mut opts = DbOptions::from_config_target(cfg, DbTarget::postgres(h.url(Role::Server).expose(), true));
            opts.read_pool_size = cfg.postgres.pool_size;
            Database::open(opts).await
        })
    }

    fn connect_replica<'a>(&'a self, set: &'a DbSet, node: &'a DbNode) -> BoxFuture<'a, Result<Database, DbError>> {
        Box::pin(async move {
            if node.kind != NodeKind::Local {
                return self.inner.connect_replica(set, node).await;
            }
            let h = self.replicas.local.get(&node.name).await.ok_or_else(|| {
                DbError::Unsupported(format!("local replica {} is not running on this server", node.name))
            })?;
            let cfg = &self.replicas.config.database;
            let mut opts = DbOptions::from_config_target(cfg, DbTarget::postgres(h.url(Role::Reader).expose(), true));
            opts.read_pool_size = cfg.postgres.pool_size;
            Database::open(opts.read_only()).await
        })
    }
}

// ---------------------------------------------------------------------------
// Promote: the server's side of the pools.
// ---------------------------------------------------------------------------

struct Pools<'a> {
    replicas: &'a Replicas,
}

impl service::PoolSwitch for Pools<'_> {
    fn fence<'a>(&'a self, set_id: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            // Control's pools are the server's own database: the old
            // primary's stop is the fence, and the server restarts onto the
            // new one. A shard's pools close here.
            if set_id != CONTROL_SET_ID
                && let Ok(r) = self.replicas.router()
                && let Some(h) = r.set(set_id)
            {
                h.database().close().await;
            }
        })
    }

    fn repoint<'a>(
        &'a self,
        set_id: &'a str,
        new_primary: &'a DbNode,
        handle: Option<Arc<EngineHandle>>,
    ) -> BoxFuture<'a, service::ServiceResult<Database>> {
        Box::pin(async move {
            let cfg = &self.replicas.config.database;
            if set_id == CONTROL_SET_ID {
                let url = match &handle {
                    Some(h) => h.url(Role::Server).expose().to_owned(),
                    None => {
                        let sealed = new_primary.url_sealed.as_deref().ok_or_else(|| {
                            ServiceError::Unsupported(format!("{} has no connection URL", new_primary.name))
                        })?;
                        open_node_url(&self.replicas.keyring, &new_primary.id, sealed).map_err(ServiceError::Invalid)?
                    }
                };
                let opts = DbOptions::from_config_target(cfg, DbTarget::postgres(url, handle.is_some()));
                let db = Database::open(opts).await?;
                *lock(&self.replicas.restart_required) = Some(format!(
                    "control's primary moved to {}: restart the server to move its connections",
                    new_primary.name
                ));
                tracing::warn!(node = %new_primary.name, "control's primary was promoted: restart the server");
                return Ok(db);
            }
            // FB-174: a shard's new primary: the router reopens the set (its
            // primary node changed) and the set's pools follow.
            if let Ok(r) = self.replicas.router() {
                r.reload().await.map_err(|e| ServiceError::Invalid(e.to_string()))?;
                return match r.set(set_id) {
                    Some(h) => Ok(h.database().clone()),
                    None => Err(ServiceError::Invalid(format!("set {set_id} did not reopen"))),
                };
            }
            Err(ServiceError::Invalid("the router is not running".into()))
        })
    }
}

// ---------------------------------------------------------------------------
// TopologyOps.
// ---------------------------------------------------------------------------

impl TopologyOps for Replicas {
    fn topology(&self) -> OpsFuture<'_, api::TopologyView> {
        Box::pin(async move {
            let control = self.control()?;
            let topo = topology::load(&control).await.map_err(db_err)?;
            let placements = topology::load_placements(&control).await.map_err(db_err)?;
            let mut counts: HashMap<&str, (u64, u64)> = HashMap::new();
            for p in &placements {
                let e = counts.entry(p.set_id.as_str()).or_default();
                e.0 += 1;
                e.1 += p.est_bytes;
            }
            let sets = topo
                .sets
                .iter()
                .filter(|s| s.state != SetState::Retired)
                .map(|s| {
                    let (n, b) = counts.get(s.id.as_str()).copied().unwrap_or_default();
                    set_info(s, n, b)
                })
                .collect();
            let nodes = self.node_infos(&control).await?;
            let operations = self.active_ops().await?.iter().map(operation_of).collect();
            Ok(api::TopologyView {
                backend: "postgres".into(),
                engine_version: self.primary().map(|h| h.version().text.clone()),
                epoch: topo.epoch,
                sets,
                nodes,
                operations,
                restart_required: lock(&self.restart_required).clone(),
            })
        })
    }

    fn node<'a>(&'a self, id: &'a str) -> OpsFuture<'a, api::NodeDetail> {
        Box::pin(async move {
            let control = self.control()?;
            let v = service::node_view(&control, id).await.map_err(svc_err)?;
            let st = self.statuses().get(id).cloned();
            let lag_history = lock(&self.lag)
                .get(id)
                .map(|q| q.iter().cloned().collect())
                .unwrap_or_default();
            Ok(api::NodeDetail {
                node: node_info(v, st.as_ref()),
                lag_history,
            })
        })
    }

    fn add_node<'a>(&'a self, ctx: &'a AccessContext, req: api::AddNode) -> OpsFuture<'a, api::Added> {
        Box::pin(async move {
            let control = self.control()?;
            let topo = topology::load(&control).await.map_err(db_err)?;
            if topo.set(&req.set).is_none_or(|s| s.state == SetState::Retired) {
                return Err(TopologyError::new(
                    404,
                    "shard_not_found",
                    format!("set {} not found", req.set),
                ));
            }
            if topo
                .nodes
                .iter()
                .any(|n| n.state != NodeState::Retired && n.name.eq_ignore_ascii_case(&req.name))
            {
                return Err(TopologyError::new(
                    409,
                    "node_exists",
                    format!("a node named {} already exists", req.name),
                ));
            }
            let weight = req.weight.unwrap_or(1);
            if req.kind == "local" {
                self.need_primary()?;
                // FB-174: a shard's local replicas follow its local primary.
                self.set_primary(&req.set).await?;
                let op = self
                    .enqueue(
                        ctx,
                        kinds::NODE_ADD,
                        &req.set,
                        None,
                        json!({ "name": req.name, "weight": weight }),
                    )
                    .await?;
                return Ok(api::Added {
                    operation: Some(op),
                    node: None,
                });
            }
            // External: check that the server answers and is a copy of the
            // set's primary before it is stored.
            let url = req.url.clone().unwrap_or_default();
            let primary_db = self
                .router()?
                .set(&req.set)
                .map(|h| h.database().clone())
                .ok_or_else(|| TopologyError::new(404, "shard_not_found", format!("set {} is not open", req.set)))?;
            check_external_replica(&primary_db, &self.config, &url).await?;
            let node = topology::insert_node(
                &control,
                &topology::NewNode {
                    set_id: req.set.clone(),
                    name: req.name.clone(),
                    kind: NodeKind::External,
                    role: NodeRole::Replica,
                    url_sealed: None,
                    state: NodeState::CatchingUp,
                    weight,
                    origin: NodeOrigin::Api,
                },
            )
            .await
            .map_err(db_err)?;
            let sealed = seal_node_url(&self.keyring, &node.id, &url).map_err(TopologyError::internal)?;
            set_node_url(&control, &node.id, Some(&sealed), None).await?;
            self.reload().await;
            let v = service::node_view(&control, &node.id).await.map_err(svc_err)?;
            let st = self.statuses().get(&node.id).cloned();
            Ok(api::Added {
                operation: None,
                node: Some(node_info(v, st.as_ref())),
            })
        })
    }

    fn update_node<'a>(&'a self, id: &'a str, req: api::UpdateNode) -> OpsFuture<'a, api::NodeInfo> {
        Box::pin(async move {
            let control = self.control()?;
            let n = self.node_row(id).await?;
            if n.origin == NodeOrigin::Config {
                return Err(TopologyError::new(
                    409,
                    "node_from_config",
                    format!("{} is declared in forgebase.toml; change its weight there", n.name),
                ));
            }
            set_node_url(&control, id, None, Some(req.weight)).await?;
            self.reload().await;
            let v = service::node_view(&control, id).await.map_err(svc_err)?;
            let st = self.statuses().get(id).cloned();
            Ok(node_info(v, st.as_ref()))
        })
    }

    fn remove_node<'a>(&'a self, id: &'a str, confirm: &'a str) -> OpsFuture<'a, api::Removed> {
        Box::pin(async move {
            let control = self.control()?;
            let n = self.node_row(id).await?;
            if n.origin == NodeOrigin::Config {
                check_confirm(confirm, &n.name)?;
                return Err(TopologyError::new(
                    409,
                    "node_from_config",
                    format!("{} is declared in forgebase.toml; remove it there", n.name),
                ));
            }
            let _one = self.changes.lock().await;
            // FB-174: a shard's replica has its slot on the shard's primary.
            let primary = if n.set_id == CONTROL_SET_ID {
                self.primary()
            } else {
                self.set_primary(&n.set_id).await.ok()
            };
            // Close its pool first: the router drops it on reload.
            service::remove_replica(&control, &self.local, primary.as_deref(), id, confirm)
                .await
                .map_err(svc_err)?;
            self.reload().await;
            if n.kind == NodeKind::Remote
                && let Err(e) = self.apply_hba_peers().await
            {
                tracing::warn!(error = %e.message, "pg_hba.conf peers not updated after a removal");
            }
            Ok(api::Removed { removed: id.to_owned() })
        })
    }

    fn promote_check<'a>(&'a self, id: &'a str) -> OpsFuture<'a, api::PromoteCheck> {
        Box::pin(async move {
            let n = self.node_row(id).await?;
            if n.role != NodeRole::Replica {
                return Err(TopologyError::new(
                    409,
                    "node_state",
                    format!("{} is already the primary", n.name),
                ));
            }
            if n.kind != NodeKind::Local {
                // Remote: the agent's last report against the primary.
                let control = self.control()?;
                let v = service::node_view(&control, id).await.map_err(svc_err)?;
                let replayed = v.agent.as_ref().and_then(|a| a.replay_lsn);
                let primary_lsn = control.current_wal_lsn().await.ok().flatten();
                return Ok(api::PromoteCheck {
                    replayed: replayed.map(|l| l.to_string()),
                    primary_lsn: primary_lsn.map(|l| l.to_string()),
                    behind_bytes: match (primary_lsn, replayed) {
                        (Some(p), Some(r)) => Some(p.0.saturating_sub(r.0)),
                        _ => None,
                    },
                    primary_reachable: primary_lsn.is_some(),
                });
            }
            let primary = self.primary();
            let c = service::promote_check(&self.local, primary.as_deref(), &n.name)
                .await
                .map_err(svc_err)?;
            Ok(api::PromoteCheck {
                replayed: c.replayed.map(|l| l.to_string()),
                primary_lsn: c.primary_lsn.map(|l| l.to_string()),
                behind_bytes: c.behind_bytes,
                primary_reachable: c.primary_reachable,
            })
        })
    }

    fn promote<'a>(
        &'a self,
        ctx: &'a AccessContext,
        id: &'a str,
        req: api::PromoteBody,
    ) -> OpsFuture<'a, api::Operation> {
        Box::pin(async move {
            let n = self.node_row(id).await?;
            check_confirm(&req.confirm, &n.name)?;
            if n.role != NodeRole::Replica {
                return Err(TopologyError::new(
                    409,
                    "node_state",
                    format!("{} is already the primary", n.name),
                ));
            }
            match n.kind {
                NodeKind::External => {
                    return Err(TopologyError::new(
                        400,
                        "node_unsupported",
                        format!(
                            "{} is an external replica: promote it on its own server, then add it as a primary",
                            n.name
                        ),
                    ));
                }
                NodeKind::Local | NodeKind::Remote => {
                    self.need_primary()?;
                }
            }
            if !matches!(
                n.state,
                NodeState::Streaming | NodeState::CatchingUp | NodeState::Lagging | NodeState::Unreachable
            ) {
                return Err(TopologyError::new(
                    409,
                    "node_state",
                    format!(
                        "{} is {}: only a replica that follows the primary can be promoted",
                        n.name, n.state
                    ),
                ));
            }
            self.enqueue(
                ctx,
                kinds::PROMOTE,
                &n.set_id,
                Some(id),
                json!({ "confirm": req.confirm, "accept_data_loss": req.accept_data_loss }),
            )
            .await
        })
    }

    fn rebuild<'a>(&'a self, ctx: &'a AccessContext, id: &'a str) -> OpsFuture<'a, api::Operation> {
        Box::pin(async move {
            let n = self.node_row(id).await?;
            if n.role != NodeRole::Replica {
                return Err(TopologyError::new(
                    400,
                    "node_unsupported",
                    format!("{} is the primary; promote another node first", n.name),
                ));
            }
            if n.kind == NodeKind::External {
                return Err(TopologyError::new(
                    400,
                    "node_unsupported",
                    format!("{} is an external replica: ForgeBase does not rebuild it", n.name),
                ));
            }
            if n.kind == NodeKind::Local {
                self.need_primary()?;
            }
            self.enqueue(ctx, kinds::NODE_REBUILD, &n.set_id, Some(id), json!({}))
                .await
        })
    }

    fn join_token<'a>(
        &'a self,
        ctx: &'a AccessContext,
        req: api::JoinTokenBody,
        server_url: &'a str,
    ) -> OpsFuture<'a, api::JoinToken> {
        Box::pin(async move {
            let control = self.control()?;
            let primary = self.need_primary()?;
            if primary.tcp_addr().is_none() {
                return Err(TopologyError::new(
                    400,
                    "node_unsupported",
                    "replicas on other hosts reach the primary over TCP: set [database.postgres] listen (for example 0.0.0.0:5433) and restart",
                ));
            }
            let role = req
                .role
                .parse::<NodeRole>()
                .map_err(|_| TopologyError::invalid(format!("role {:?} must be replica", req.role)))?;
            let issued = service::issue_join_token(
                &control,
                &service::JoinTokenRequest {
                    set_id: req.set.clone(),
                    role,
                },
                Some(&ctx.actor()),
                server_url,
            )
            .await
            .map_err(svc_err)?;
            Ok(api::JoinToken {
                id: issued.id,
                token: issued.token,
                set: issued.set_id,
                role: issued.role.as_str().to_owned(),
                expires_at: issued.expires_at,
                command: issued.command,
            })
        })
    }

    fn add_shard<'a>(&'a self, ctx: &'a AccessContext, req: api::AddShard) -> OpsFuture<'a, api::Operation> {
        Box::pin(async move {
            let control = self.control()?;
            let topo = topology::load(&control).await.map_err(db_err)?;
            if topo
                .sets
                .iter()
                .any(|s| s.state != SetState::Retired && s.name.eq_ignore_ascii_case(req.name.trim()))
            {
                return Err(TopologyError::new(
                    409,
                    "shard_exists",
                    format!("a shard named {} exists", req.name),
                ));
            }
            if topo
                .nodes
                .iter()
                .any(|n| n.state != NodeState::Retired && n.name.eq_ignore_ascii_case(&req.node.name))
            {
                return Err(TopologyError::new(
                    409,
                    "node_exists",
                    format!("a node named {} already exists", req.node.name),
                ));
            }
            // FB-174: `local` (a cluster on this server), `remote` (another
            // host joins with a primary token), `external` (a server you run).
            let kind = req.node.kind.as_str();
            let url = req.node.url.clone().unwrap_or_default();
            match kind {
                "local" => {
                    self.need_primary()?;
                    forgebase_pgengine::Layout::node(&self.data_dir, &req.node.name, forgebase_pgengine::NodeKind::Primary)
                        .map_err(|e| TopologyError::invalid(e.to_string()))?;
                }
                "remote" => {
                    self.need_primary()?;
                }
                _ => check_reachable(&self.config, &url).await?,
            }
            let set = topology::insert_set(
                &control,
                &topology::NewSet {
                    name: req.name.trim().to_owned(),
                    weight: req.weight.unwrap_or(1),
                },
            )
            .await
            .map_err(db_err)?;
            if kind == "remote" {
                // The node row comes with `forgebase node join`
                // (POST join-tokens {set, role: primary}); the job waits for it.
                return self
                    .enqueue(
                        ctx,
                        kinds::SHARD_ADD,
                        &set.id,
                        None,
                        json!({ "remote": req.node.name }),
                    )
                    .await;
            }
            let node = topology::insert_node(
                &control,
                &topology::NewNode {
                    set_id: set.id.clone(),
                    name: req.node.name.clone(),
                    kind: if kind == "local" { NodeKind::Local } else { NodeKind::External },
                    role: NodeRole::Primary,
                    url_sealed: None,
                    state: if kind == "local" { NodeState::Provisioning } else { NodeState::Ready },
                    weight: 1,
                    origin: NodeOrigin::Api,
                },
            )
            .await
            .map_err(db_err)?;
            if kind != "local" {
                let sealed = seal_node_url(&self.keyring, &node.id, &url).map_err(TopologyError::internal)?;
                set_node_url(&control, &node.id, Some(&sealed), None).await?;
            }
            self.enqueue(ctx, kinds::SHARD_ADD, &set.id, Some(&node.id), json!({}))
                .await
        })
    }

    fn update_shard<'a>(&'a self, id: &'a str, req: api::UpdateShard) -> OpsFuture<'a, api::SetInfo> {
        Box::pin(async move {
            let control = self.control()?;
            let topo = topology::load(&control).await.map_err(db_err)?;
            let set = topo
                .set(id)
                .filter(|s| s.state != SetState::Retired)
                .ok_or_else(|| TopologyError::new(404, "shard_not_found", format!("set {id} not found")))?;
            let state = match req.state.as_deref() {
                Some("active") => Some(SetState::Active),
                Some("draining") => Some(SetState::Draining),
                _ => None,
            };
            if state.is_some() && set.kind == SetKind::Control {
                return Err(TopologyError::invalid(
                    "control cannot be drained; set hosts_tenants = false to keep new tenants off it",
                ));
            }
            if state.is_some() && matches!(set.state, SetState::Provisioning | SetState::Migrating) {
                return Err(TopologyError::new(
                    409,
                    api::codes::OPERATION_IN_PROGRESS,
                    format!("{} is {}; wait for it to become active", set.name, set.state),
                ));
            }
            // FB-174: draining moves every tenant away (operation
            // `database.shard.drain`, one move at a time).
            if state == Some(SetState::Draining) && set.state == SetState::Active {
                self.enqueue(&AccessContext::system(), kinds::SHARD_DRAIN, id, None, json!({}))
                    .await?;
            }
            let updated = topology::update_set(&control, id, state, req.weight, req.hosts_tenants)
                .await
                .map_err(db_err)?
                .ok_or_else(|| TopologyError::new(404, "shard_not_found", format!("set {id} not found")))?;
            self.reload().await;
            let counted = topology::load_placements(&control)
                .await
                .map_err(db_err)?
                .into_iter()
                .filter(|p| p.set_id == id)
                .fold((0u64, 0u64), |(n, b), p| (n + 1, b + p.est_bytes));
            Ok(set_info(&updated, counted.0, counted.1))
        })
    }

    fn remove_shard<'a>(&'a self, id: &'a str, confirm: &'a str) -> OpsFuture<'a, api::Removed> {
        Box::pin(async move {
            let control = self.control()?;
            let topo = topology::load(&control).await.map_err(db_err)?;
            let set = topo
                .set(id)
                .filter(|s| s.state != SetState::Retired)
                .cloned()
                .ok_or_else(|| TopologyError::new(404, "shard_not_found", format!("set {id} not found")))?;
            if set.kind == SetKind::Control {
                return Err(TopologyError::invalid("control cannot be removed"));
            }
            check_confirm(confirm, &set.name)?;
            let _one = self.changes.lock().await;
            if self
                .active_ops()
                .await?
                .iter()
                .any(|j| j.payload.get("set").and_then(Json::as_str) == Some(id))
            {
                return Err(TopologyError::new(
                    409,
                    api::codes::OPERATION_IN_PROGRESS,
                    format!("an operation is changing {}", set.name),
                ));
            }
            let placed = topology::load_placements(&control)
                .await
                .map_err(db_err)?
                .into_iter()
                .filter(|p| p.set_id == id)
                .count();
            if placed > 0 {
                return Err(TopologyError::new(
                    409,
                    api::codes::SHARD_NOT_EMPTY,
                    format!(
                        "{} still holds {placed} tenants: drain it and move them first",
                        set.name
                    ),
                ));
            }
            // FB-174: no move may still use it (source rows kept until cleanup).
            if let Err(e) = forgebase_db::moves::check_removable(self.router()?, id).await {
                return Err(move_err(e));
            }
            if let Some(h) = self.router()?.set(id) {
                let _ = forgebase_db::shards::unsubscribe_from_control(h.database(), &control, set.number).await;
            }
            for n in topo.nodes_of(id).filter(|n| n.state != NodeState::Retired) {
                retire_node(&control, n).await?;
                // FB-174: a local shard node's data stays under
                // postgres/retired/ until DELETE retired/{id}.
                if n.kind == NodeKind::Local {
                    if n.role == NodeRole::Primary {
                        match self.shards.retire(&n.name).await {
                            Ok(r) => tracing::info!(node = %n.name, path = %r.path.display(), "shard data retired"),
                            Err(e) => tracing::warn!(node = %n.name, error = %e, "shard data not retired"),
                        }
                    } else if let Err(e) = self.local.remove(None, &n.name).await {
                        tracing::warn!(node = %n.name, error = %e, "local replica of a removed shard not deleted");
                    }
                }
            }
            topology::update_set(&control, id, Some(SetState::Retired), None, Some(false))
                .await
                .map_err(db_err)?;
            if let Some(h) = self.router()?.set(id) {
                h.database().close().await;
            }
            self.reload().await;
            Ok(api::Removed { removed: id.to_owned() })
        })
    }

    fn placements(&self, q: api::PlacementQuery) -> OpsFuture<'_, api::PlacementPage> {
        Box::pin(async move {
            let control = self.control()?;
            let placed: BTreeMap<String, topology::Placement> = topology::load_placements(&control)
                .await
                .map_err(db_err)?
                .into_iter()
                .map(|p| (p.tenant.clone(), p))
                .collect();
            let envs = control
                .fetch_all(&Statement::raw(
                    "SELECT e.id, e.project_id, p.org_id FROM _forgebase_environments e \
                     LEFT JOIN _forgebase_projects p ON p.id = e.project_id",
                ))
                .await
                .map_err(db_err)?;
            let text = |r: &[Value], i: usize| r.get(i).and_then(Value::as_str).map(str::to_owned);
            let mut all: BTreeMap<String, api::PlacementInfo> = BTreeMap::new();
            for r in &envs.rows {
                let Some(id) = text(r, 0) else { continue };
                all.insert(
                    id.clone(),
                    api::PlacementInfo {
                        tenant: id,
                        project: text(r, 1),
                        organization: text(r, 2),
                        set: CONTROL_SET_ID.into(),
                        state: "active".into(),
                        est_rows: 0,
                        est_bytes: 0,
                        moved_at: None,
                    },
                );
            }
            for (t, p) in placed {
                let e = all.entry(t.clone()).or_insert_with(|| api::PlacementInfo {
                    tenant: t,
                    project: None,
                    organization: None,
                    set: String::new(),
                    state: String::new(),
                    est_rows: 0,
                    est_bytes: 0,
                    moved_at: None,
                });
                e.set = p.set_id;
                e.state = p.state.as_str().to_owned();
                e.est_rows = p.est_rows;
                e.est_bytes = p.est_bytes;
                e.moved_at = p.moved_at;
            }
            let limit = usize::try_from(q.limit.unwrap_or(50)).unwrap_or(50);
            let mut items: Vec<api::PlacementInfo> = all
                .into_values()
                .filter(|p| q.set.as_deref().is_none_or(|s| p.set == s))
                .filter(|p| q.project.as_deref().is_none_or(|s| p.project.as_deref() == Some(s)))
                .filter(|p| q.q.as_deref().is_none_or(|s| p.tenant.contains(s)))
                .filter(|p| q.cursor.as_deref().is_none_or(|c| p.tenant.as_str() > c))
                .take(limit + 1)
                .collect();
            let next_cursor = (items.len() > limit).then(|| {
                items.truncate(limit);
                items.last().map(|p| p.tenant.clone()).unwrap_or_default()
            });
            Ok(api::PlacementPage { items, next_cursor })
        })
    }

    fn move_tenant<'a>(&'a self, ctx: &'a AccessContext, tenant: &'a str, req: api::MoveBody) -> OpsFuture<'a, api::Operation> {
        Box::pin(async move {
            let router = self.router()?;
            let moves = self.moves()?;
            let from = router
                .placement(tenant)
                .map_or_else(|| CONTROL_SET_ID.to_owned(), |p| p.set_id);
            // One topology change per set: neither set may be busy.
            let _one = self.changes.lock().await;
            if let Some(busy) = self.active_ops().await?.into_iter().find(|j| {
                [j.payload.get("set"), j.payload.get("to")]
                    .into_iter()
                    .flatten()
                    .filter_map(Json::as_str)
                    .any(|s| s == from || s == req.to)
            }) {
                return Err(TopologyError::new(
                    409,
                    api::codes::OPERATION_IN_PROGRESS,
                    format!("operation {} ({}) is changing one of the two sets; wait for it or cancel it", busy.id, busy.kind),
                ));
            }
            let (_, job) = moves
                .start_move(ctx, tenant, &req.to, ctx.principal.id().map(str::to_owned))
                .await
                .map_err(move_err)?;
            Ok(operation_of(&job))
        })
    }

    fn rebalance_plan(&self, req: api::RebalancePlanBody) -> OpsFuture<'_, api::RebalancePlanView> {
        Box::pin(async move {
            let plan = self
                .moves()?
                .plan(&forgebase_db::moves::PlanOptions {
                    sets: req.sets,
                    max_moves: req.max_moves,
                })
                .await
                .map_err(move_err)?;
            Ok(plan_view(&plan))
        })
    }

    fn rebalance<'a>(&'a self, ctx: &'a AccessContext, req: api::RebalanceBody) -> OpsFuture<'a, api::Operation> {
        Box::pin(async move {
            let moves = self.moves()?;
            let _one = self.changes.lock().await;
            if let Some(busy) = self.active_ops().await?.into_iter().next() {
                return Err(TopologyError::new(
                    409,
                    api::codes::OPERATION_IN_PROGRESS,
                    format!("operation {} ({}) is running; a rebalance waits for every other change", busy.id, busy.kind),
                ));
            }
            let job = moves.start_rebalance(ctx, &req.plan_id).await.map_err(move_err)?;
            Ok(operation_of(&job))
        })
    }

    fn retired_data(&self) -> OpsFuture<'_, api::RetiredList> {
        Box::pin(async move {
            Ok(api::RetiredList {
                items: self
                    .shards
                    .retired()
                    .into_iter()
                    .map(|r| api::RetiredInfo {
                        id: r.id,
                        name: r.name,
                        set_id: r.set_id,
                        bytes: r.bytes,
                    })
                    .collect(),
            })
        })
    }

    fn delete_retired<'a>(&'a self, id: &'a str, confirm: &'a str) -> OpsFuture<'a, api::Removed> {
        Box::pin(async move {
            let found = self
                .shards
                .retired()
                .into_iter()
                .find(|r| r.id == id)
                .ok_or_else(|| TopologyError::new(404, "retired_not_found", format!("no retired shard data {id}")))?;
            check_confirm(confirm, &found.name)?;
            let shards = self.shards.clone();
            let id_owned = id.to_owned();
            tokio::task::spawn_blocking(move || shards.delete_retired(&id_owned))
                .await
                .map_err(|e| TopologyError::internal(e.to_string()))?
                .map_err(|e| TopologyError::internal(e.to_string()))?;
            Ok(api::Removed { removed: id.to_owned() })
        })
    }

    fn operations<'a>(&'a self, ctx: &'a AccessContext, q: api::OperationQuery) -> OpsFuture<'a, api::OperationList> {
        Box::pin(async move {
            if q.active {
                let mut items: Vec<api::Operation> = self.active_ops().await?.iter().map(operation_of).collect();
                items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
                return Ok(api::OperationList { items });
            }
            let page = self
                .jobs()?
                .list(
                    ctx,
                    JobFilter {
                        queue: Some(api::OPERATIONS_QUEUE.to_owned()),
                        limit: Some(i64::from(q.limit.unwrap_or(50))),
                        ..JobFilter::default()
                    },
                )
                .await
                .map_err(job_err)?;
            Ok(api::OperationList {
                items: page.items.iter().map(operation_of).collect(),
            })
        })
    }

    fn operation<'a>(&'a self, ctx: &'a AccessContext, id: &'a str) -> OpsFuture<'a, api::Operation> {
        Box::pin(async move {
            let job = self.jobs()?.get(ctx, id).await.map_err(|e| match e {
                forgebase_jobs::JobError::NotFound(_) => TopologyError::new(
                    404,
                    api::codes::OPERATION_NOT_FOUND,
                    format!("operation {id} not found"),
                ),
                e => job_err(e),
            })?;
            if job.queue != api::OPERATIONS_QUEUE {
                return Err(TopologyError::new(
                    404,
                    api::codes::OPERATION_NOT_FOUND,
                    format!("operation {id} not found"),
                ));
            }
            Ok(operation_of(&job))
        })
    }

    fn cancel_operation<'a>(&'a self, ctx: &'a AccessContext, id: &'a str) -> OpsFuture<'a, api::Operation> {
        Box::pin(async move {
            let op = self.operation(ctx, id).await?;
            if !op.cancellable {
                return Err(TopologyError::new(
                    409,
                    api::codes::NOT_CANCELLABLE,
                    format!("operation {id} is {} and cannot be cancelled now", op.status),
                ));
            }
            let job = self.jobs()?.cancel(ctx, id).await.map_err(job_err)?;
            Ok(operation_of(&job))
        })
    }

    fn agent_join(&self, body: Json, peer: Option<IpAddr>) -> OpsFuture<'_, Json> {
        Box::pin(async move {
            let control = self.control()?;
            let primary = self.need_primary()?;
            let req: service::JoinRequest =
                serde_json::from_value(body).map_err(|e| TopologyError::invalid(format!("bad join request: {e}")))?;
            let source = service::ReplicationSource::from_handle(&primary, None, replication_sslmode(&self.config))
                .map_err(svc_err)?;
            let mut grant = service::redeem_join(&control, &source, &req, peer)
                .await
                .map_err(svc_err)?;
            if grant.role == NodeRole::Primary {
                // FB-174: a shard primary on another host. Same roles as every
                // node (the secrets go once over HTTPS); the server writes to it
                // as forgebase_server at its listen address.
                let listen = req.listen.as_deref().map(str::trim).filter(|l| !l.is_empty()).ok_or_else(|| {
                    TopologyError::invalid("a shard primary must listen on TCP: run forgebase node join with --listen")
                })?;
                let file = Layout::primary(&self.data_dir).secrets_file();
                let text = std::fs::read_to_string(&file).map_err(|e| TopologyError::internal(e.to_string()))?;
                let pw = RoleSecrets::load(&file)
                    .map_err(|e| TopologyError::internal(e.to_string()))?
                    .and_then(|s| s.password(Role::Server).map(|p| Secret::new(p.to_owned())))
                    .ok_or_else(|| TopologyError::internal("roles.secret has no forgebase_server password"))?;
                let url = service::server_url(listen, &pw, source.sslmode);
                let sealed =
                    seal_node_url(&self.keyring, &grant.node_id, url.expose()).map_err(TopologyError::internal)?;
                set_node_url(&control, &grant.node_id, Some(&sealed), None).await?;
                grant.secrets = Some(text);
                if let Err(e) = self.apply_hba_peers().await {
                    tracing::warn!(error = %e.message, "pg_hba.conf peers not updated after a join");
                }
                return serde_json::to_value(&grant).map_err(|e| TopologyError::internal(e.to_string()));
            }
            // The server's read pool on the replica: the reader's URL, sealed.
            if let Some(listen) = req.listen.as_deref().filter(|l| !l.trim().is_empty()) {
                let pw = reader_password(&self.data_dir)?;
                let url = service::reader_url(listen.trim(), &pw, source.sslmode);
                let sealed =
                    seal_node_url(&self.keyring, &grant.node_id, url.expose()).map_err(TopologyError::internal)?;
                set_node_url(&control, &grant.node_id, Some(&sealed), None).await?;
            }
            if let Err(e) = self.apply_hba_peers().await {
                tracing::warn!(error = %e.message, "pg_hba.conf peers not updated after a join");
            }
            self.reload().await;
            serde_json::to_value(&grant).map_err(|e| TopologyError::internal(e.to_string()))
        })
    }

    fn agent_commands<'a>(
        &'a self,
        id: &'a str,
        key: &'a str,
        wait_ms: Option<u64>,
        resend: bool,
    ) -> OpsFuture<'a, Json> {
        Box::pin(async move {
            let control = self.control()?;
            let wait = Duration::from_millis(wait_ms.unwrap_or(0));
            let batch = service::poll_commands(&control, id, key, wait, resend)
                .await
                .map_err(svc_err)?;
            serde_json::to_value(&batch).map_err(|e| TopologyError::internal(e.to_string()))
        })
    }

    fn agent_heartbeat<'a>(&'a self, id: &'a str, key: &'a str, body: Json) -> OpsFuture<'a, Json> {
        Box::pin(async move {
            let control = self.control()?;
            let hb: service::Heartbeat =
                serde_json::from_value(body).map_err(|e| TopologyError::invalid(format!("bad heartbeat: {e}")))?;
            let row = self.node_row(id).await.ok();
            let before = row.as_ref().map(|n| n.state);
            let reply = service::heartbeat(&control, id, key, &hb).await.map_err(svc_err)?;
            // FB-174: a remote shard primary that runs is ready; the waiting
            // `database.shard.add` activates it.
            if let Some(n) = &row
                && n.role == NodeRole::Primary
                && n.kind == NodeKind::Remote
                && n.state == NodeState::Provisioning
                && hb.state == service::AgentState::Primary
            {
                topology::update_node_state(&control, id, NodeState::Ready)
                    .await
                    .map_err(db_err)?;
            }
            if before != Some(reply.node_state) {
                self.reload().await;
            }
            serde_json::to_value(&reply).map_err(|e| TopologyError::internal(e.to_string()))
        })
    }
}

/// An external replica must answer and be a copy of the set's primary
/// (same `system_identifier`, in recovery).
async fn check_external_replica(
    primary: &Database,
    config: &forgebase_core::Config,
    url: &str,
) -> Result<(), TopologyError> {
    let db = open_probe(config, url).await?;
    let ident = "SELECT CAST(system_identifier AS TEXT) FROM pg_control_system()";
    let theirs = db.fetch_all(&Statement::raw(ident)).await;
    let recovery = db
        .fetch_all(&Statement::raw("SELECT CAST(pg_is_in_recovery() AS INT)"))
        .await;
    db.close().await;
    let ours = primary.fetch_all(&Statement::raw(ident)).await.map_err(db_err)?;
    let text = |r: &forgebase_db::Rows| {
        r.rows
            .first()
            .and_then(|r| r.first())
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    let theirs = theirs.map_err(|e| {
        TopologyError::new(
            400,
            api::codes::NODE_MISMATCH,
            format!("the server at that URL does not let ForgeBase read its identity: {e}"),
        )
    })?;
    if text(&theirs) != text(&ours) {
        return Err(TopologyError::new(
            400,
            api::codes::NODE_MISMATCH,
            "the server at that URL is not a copy of this set's primary (its system identifier differs)",
        ));
    }
    let in_recovery = recovery
        .ok()
        .and_then(|r| r.rows.first().and_then(|r| r.first()).and_then(Value::as_i64))
        .is_some_and(|v| v == 1);
    if !in_recovery {
        return Err(TopologyError::new(
            400,
            api::codes::NODE_MISMATCH,
            "the server at that URL is not a replica (it accepts writes)",
        ));
    }
    Ok(())
}

async fn open_probe(config: &forgebase_core::Config, url: &str) -> Result<Database, TopologyError> {
    let mut opts = DbOptions::from_config_target(&config.database, DbTarget::postgres(url.to_owned(), false));
    opts.read_pool_size = 1;
    match tokio::time::timeout(Duration::from_secs(10), Database::open(opts.read_only())).await {
        Ok(Ok(db)) => Ok(db),
        Ok(Err(e)) => Err(TopologyError::new(
            503,
            api::codes::NODE_UNREACHABLE,
            format!("cannot connect to the server at that URL: {e}"),
        )),
        Err(_) => Err(TopologyError::new(
            503,
            api::codes::NODE_UNREACHABLE,
            "the server at that URL did not answer within 10 seconds",
        )),
    }
}

async fn check_reachable(config: &forgebase_core::Config, url: &str) -> Result<(), TopologyError> {
    let db = open_probe(config, url).await?;
    let r = db.fetch_all(&Statement::raw("SELECT 1")).await;
    db.close().await;
    r.map(|_| ()).map_err(|e| {
        TopologyError::new(
            503,
            api::codes::NODE_UNREACHABLE,
            format!("the server at that URL failed a query: {e}"),
        )
    })
}

// ---------------------------------------------------------------------------
// Jobs.
// ---------------------------------------------------------------------------

fn stage(stage: &str, percent: f64, message: impl Into<String>) -> forgebase_jobs::ProgressReport {
    forgebase_jobs::ProgressReport {
        percent: Some(percent),
        stage: Some(stage.to_owned()),
        message: Some(message.into()),
    }
}

async fn report(ctx: &JobContext, p: forgebase_jobs::ProgressReport) {
    if let Err(e) = ctx.progress(p).await {
        tracing::debug!(error = %e, "operation progress not stored");
    }
}

fn cancelled(ctx: &JobContext, before: &str) -> Result<(), HandlerError> {
    if ctx.is_cancelled() {
        Err(HandlerError::fatal(format!(
            "cancelled before {before}; nothing was changed"
        )))
    } else {
        Ok(())
    }
}

fn fail(e: TopologyError) -> HandlerError {
    HandlerError::fatal(format!("{} ({})", e.message, e.code))
}

fn payload_str<'a>(ctx: &'a JobContext, key: &str) -> Result<&'a str, HandlerError> {
    ctx.payload()
        .get(key)
        .and_then(Json::as_str)
        .ok_or_else(|| HandlerError::fatal(format!("bad payload: no {key}")))
}

/// Registers the reserved `database.*` handlers.
pub fn register_jobs(registry: JobRegistry, replicas: Arc<Replicas>) -> JobRegistry {
    let r1 = replicas.clone();
    let r2 = replicas.clone();
    let r3 = replicas.clone();
    let r4 = replicas;
    registry
        .register(kinds::NODE_ADD, move |ctx: JobContext| {
            let r = r1.clone();
            async move { r.run_node_add(&ctx).await }
        })
        .register(kinds::NODE_REBUILD, move |ctx: JobContext| {
            let r = r2.clone();
            async move { r.run_rebuild(&ctx).await }
        })
        .register(kinds::PROMOTE, move |ctx: JobContext| {
            let r = r3.clone();
            async move { r.run_promote(&ctx).await }
        })
        .register(kinds::SHARD_ADD, move |ctx: JobContext| {
            let r = r4.clone();
            async move { r.run_shard_add(&ctx).await }
        })
}

impl Replicas {
    async fn run_node_add(&self, ctx: &JobContext) -> Result<Json, HandlerError> {
        let set = payload_str(ctx, "set")?.to_owned();
        let name = payload_str(ctx, "name")?.to_owned();
        let weight = ctx
            .payload()
            .get("weight")
            .and_then(Json::as_u64)
            .and_then(|w| u32::try_from(w).ok())
            .unwrap_or(1);
        cancelled(ctx, "the clone")?;
        let control = self.control().map_err(fail)?;
        let primary = self.set_primary(&set).await.map_err(fail)?;
        // A retry after a lost lease: the row exists; rebuild it instead.
        let existing = topology::load(&control)
            .await
            .map_err(|e| HandlerError::retry(e.to_string()))?
            .nodes
            .into_iter()
            .find(|n| n.name == name && n.state != NodeState::Retired);
        report(
            ctx,
            stage(
                "cloning",
                10.0,
                format!("copying the primary's data into replica {name}"),
            ),
        )
        .await;
        let view = match existing {
            Some(n) if ctx.checkpoint().is_some() => {
                service::rebuild_replica(&control, &self.local, Some(&primary), &n.id)
                    .await
                    .map_err(|e| fail(svc_err(e)))?
            }
            Some(n) => return Err(HandlerError::fatal(format!("a node named {} already exists", n.name))),
            None => {
                let _ = ctx.save_checkpoint(json!({ "stage": "cloning", "name": name })).await;
                service::add_local_replica(
                    &control,
                    &self.local,
                    &primary,
                    &service::AddLocalReplica {
                        set_id: set,
                        name: name.clone(),
                        weight,
                    },
                )
                .await
                .map_err(|e| fail(svc_err(e)))?
            }
        };
        report(
            ctx,
            stage("starting", 90.0, format!("replica {name} started; catching up")),
        )
        .await;
        self.reload().await;
        report(ctx, stage("done", 100.0, format!("replica {name} is catching up"))).await;
        Ok(json!({ "node": view.id, "name": view.name, "state": view.state.as_str() }))
    }

    async fn run_rebuild(&self, ctx: &JobContext) -> Result<Json, HandlerError> {
        let id = payload_str(ctx, "node")?.to_owned();
        cancelled(ctx, "the rebuild")?;
        let control = self.control().map_err(fail)?;
        let n = self.node_row(&id).await.map_err(fail)?;
        report(ctx, stage("rebuilding", 10.0, format!("rebuilding {}", n.name))).await;
        if n.kind == NodeKind::Local && n.state == NodeState::NeedsRebuild {
            let primary = self.need_primary().map_err(fail)?;
            let (view, how) = service::rejoin_local(&control, &self.local, &primary, &id)
                .await
                .map_err(|e| fail(svc_err(e)))?;
            self.reload().await;
            report(ctx, stage("done", 100.0, format!("{} rejoined ({how:?})", n.name))).await;
            return Ok(json!({ "node": view.id, "state": view.state.as_str(), "how": format!("{how:?}") }));
        }
        let primary = if n.set_id == CONTROL_SET_ID {
            self.primary()
        } else {
            self.set_primary(&n.set_id).await.ok()
        };
        let view = service::rebuild_replica(&control, &self.local, primary.as_deref(), &id)
            .await
            .map_err(|e| fail(svc_err(e)))?;
        self.reload().await;
        report(ctx, stage("done", 100.0, format!("{} rebuilt", n.name))).await;
        Ok(json!({ "node": view.id, "state": view.state.as_str() }))
    }

    async fn run_promote(&self, ctx: &JobContext) -> Result<Json, HandlerError> {
        let id = payload_str(ctx, "node")?.to_owned();
        let confirm = payload_str(ctx, "confirm")?.to_owned();
        let accept_data_loss = ctx
            .payload()
            .get("accept_data_loss")
            .and_then(Json::as_bool)
            .unwrap_or(false);
        let control = self.control().map_err(fail)?;
        let n = topology::load(&control)
            .await
            .map_err(|e| HandlerError::retry(e.to_string()))?
            .nodes
            .into_iter()
            .find(|n| n.id == id)
            .ok_or_else(|| HandlerError::fatal(format!("node {id} not found")))?;
        // The attempt after a control promote (the server restarted onto
        // the new primary): the checkpoint was replicated before the fence.
        if ctx.checkpoint().is_some() {
            if n.role == NodeRole::Primary {
                return Ok(json!({ "node": id, "promoted": true, "resumed": true }));
            }
            return Err(HandlerError::fatal(format!(
                "the promote of {} was interrupted; check the topology, then promote again",
                n.name
            )));
        }
        cancelled(ctx, "the fence")?;
        let primary = self.need_primary().map_err(fail)?;
        report(ctx, stage("fencing", 20.0, "stopping writes on the old primary")).await;
        ctx.save_checkpoint(json!({ "stage": "fencing", "node": id }))
            .await
            .map_err(|e| HandlerError::retry(e.to_string()))?;
        let req = service::PromoteRequest {
            confirm,
            accept_data_loss,
        };
        let pools = Pools { replicas: self };
        let file = self.topology_file();
        let out = match n.kind {
            NodeKind::Local => {
                let (outcome, handle) = service::promote_local(
                    &control,
                    &self.local,
                    OldPrimary::Running(&primary),
                    &id,
                    &req,
                    &pools,
                    Some(&file),
                )
                .await
                .map_err(|e| fail(svc_err(e)))?;
                if n.set_id == CONTROL_SET_ID {
                    *self.primary.write().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(handle.clone());
                    self.promoted.lock().await.push(handle);
                }
                serde_json::to_value(&outcome).unwrap_or(Json::Null)
            }
            _ => {
                let outcome = service::promote_remote(
                    &control,
                    OldPrimary::Running(&primary),
                    &id,
                    &req,
                    &pools,
                    Some(&file),
                    REMOTE_PROMOTE_TIMEOUT,
                )
                .await
                .map_err(|e| fail(svc_err(e)))?;
                serde_json::to_value(&outcome).unwrap_or(Json::Null)
            }
        };
        report(ctx, stage("done", 100.0, format!("{} is the primary", n.name))).await;
        let restart = lock(&self.restart_required).clone();
        Ok(json!({ "node": id, "promoted": true, "outcome": out, "restart_required": restart }))
    }

    /// FB-174: waits for a remote shard's primary to join and run
    /// (`forgebase node join` with a primary token), cancellable.
    async fn wait_remote_primary(&self, ctx: &JobContext, control: &Database, set_id: &str) -> Result<String, HandlerError> {
        let started = std::time::Instant::now();
        loop {
            if ctx.is_cancelled() {
                return Err(HandlerError::fatal(
                    "cancelled while waiting for the shard's primary to join; remove the shard with DELETE shards/{id}",
                ));
            }
            let topo = topology::load(control)
                .await
                .map_err(|e| HandlerError::retry(e.to_string()))?;
            if let Some(n) = topo.primary_of(set_id)
                && n.state == NodeState::Ready
                && n.url_sealed.is_some()
            {
                return Ok(n.id.clone());
            }
            if started.elapsed() > REMOTE_PRIMARY_WAIT {
                return Err(HandlerError::fatal(
                    "the shard's primary did not join within an hour; make a new primary join token and run forgebase node join",
                ));
            }
            report(
                ctx,
                stage(
                    "waiting",
                    2.0,
                    "waiting for the primary: POST /api/system/database/join-tokens {set, role: \"primary\"}, then run the command on its host",
                ),
            )
            .await;
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    /// How a new shard reaches control for the `fb_control` subscription:
    /// control's superuser socket for a shard on this host, an external
    /// control's configured URL otherwise; `None` when there is no such way.
    fn control_conninfo_for(&self, node: &DbNode) -> Option<forgebase_db::SecretString> {
        if node.kind == NodeKind::Local {
            return self.control_admin_url();
        }
        crate::moves::external_control_url(&self.config)
    }

    async fn run_shard_add(&self, ctx: &JobContext) -> Result<Json, HandlerError> {
        let set_id = payload_str(ctx, "set")?.to_owned();
        cancelled(ctx, "the shard was activated")?;
        let control = self.control().map_err(fail)?;
        let router = self.router().map_err(fail)?;
        let node_id = match ctx.payload().get("node").and_then(Json::as_str) {
            Some(n) => n.to_owned(),
            None => self.wait_remote_primary(ctx, &control, &set_id).await?,
        };
        let topo = topology::load(&control)
            .await
            .map_err(|e| HandlerError::retry(e.to_string()))?;
        let set = topo
            .set(&set_id)
            .cloned()
            .ok_or_else(|| HandlerError::fatal(format!("set {set_id} not found")))?;
        let node = topo
            .nodes
            .iter()
            .find(|n| n.id == node_id)
            .cloned()
            .ok_or_else(|| HandlerError::fatal(format!("node {node_id} not found")))?;
        // FB-174: a shard primary on this server: initdb and bootstrap.
        if node.kind == NodeKind::Local && self.shards.get(&node.name).await.is_none() {
            report(
                ctx,
                stage("provisioning", 5.0, format!("creating the cluster of {} on this server", node.name)),
            )
            .await;
            self.shards
                .add(&node.name, &set.id)
                .await
                .map_err(|e| HandlerError::retry(e.to_string()))?;
            topology::update_node_state(&control, &node.id, NodeState::Ready)
                .await
                .map_err(|e| HandlerError::retry(e.to_string()))?;
        }
        report(ctx, stage("connecting", 10.0, format!("connecting to {}", node.name))).await;
        let connector = ReplicaConnector {
            inner: NodeConnector::new(self.keyring.clone(), self.config.database.clone()),
            replicas: Arc::new(self.shallow()),
        };
        let db = connector
            .connect(&set, &node)
            .await
            .map_err(|e| HandlerError::retry(e.to_string()))?;
        report(
            ctx,
            stage("activating", 30.0, "migrating the shard and creating the collections"),
        )
        .await;
        // FB-174: before the first shard, control's tenants get placement
        // rows (their load counts) and relations to shared collections lose
        // their SQL foreign keys (FB-171).
        let first = !topo
            .sets
            .iter()
            .any(|s| s.kind == SetKind::Shard && s.id != set.id && s.state != SetState::Retired);
        if first {
            if let Some(data) = self.data.get() {
                match data.cross_shard_offenders().await {
                    Ok(v) if !v.is_empty() => tracing::warn!(
                        rules = v.len(),
                        "rules follow relations to shared collections; they answer relation_crosses_shards once sharded"
                    ),
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "cross-shard rule check failed"),
                }
                data.drop_cross_placement_keys()
                    .await
                    .map_err(|e| HandlerError::retry(e.to_string()))?;
            }
            forgebase_db::moves::rebalance::ensure_control_placements(router)
                .await
                .map_err(|e| HandlerError::retry(e.to_string()))?;
        }
        let backups = DataLayoutDirs::backups(&self.data_dir);
        let replay = self.data.get().map(|d| d as &dyn forgebase_db::shards::CatalogReplay);
        let conninfo = self.control_conninfo_for(&node);
        if conninfo.is_none() {
            tracing::warn!(
                shard = %set.name,
                "no way for the shard to reach control: fb_control (record ACLs, direct logins) is not replicated to it"
            );
        }
        let setup = forgebase_db::shards::ShardSetup {
            backups_dir: Some(&backups),
            replay,
            control_conninfo: conninfo.as_ref(),
        };
        let activated = forgebase_db::shards::activate_shard(router, &set, &db, setup).await;
        db.close().await;
        let set = activated.map_err(|e| fail(router_err(e)))?;
        report(ctx, stage("done", 100.0, format!("{} is active", set.name))).await;
        Ok(json!({ "set": set.id, "state": set.state.as_str() }))
    }

    /// A copy that shares the local replicas and the handles but no loops
    /// (for a connector built inside a job).
    fn shallow(&self) -> Self {
        Self {
            data_dir: self.data_dir.clone(),
            config: self.config.clone(),
            keyring: self.keyring.clone(),
            local: self.local.clone(),
            shards: self.shards.clone(),
            moves: OnceLock::new(),
            primary: RwLock::new(self.primary()),
            promoted: tokio::sync::Mutex::new(Vec::new()),
            router: OnceLock::new(),
            jobs: OnceLock::new(),
            data: OnceLock::new(),
            lag: Mutex::new(HashMap::new()),
            restart_required: Mutex::new(None),
            tasks: Mutex::new(Vec::new()),
            changes: tokio::sync::Mutex::new(()),
        }
    }
}

/// Paths of the data directory this module needs.
struct DataLayoutDirs;

impl DataLayoutDirs {
    fn backups(data_dir: &Path) -> PathBuf {
        forgebase_core::DataLayout::new(data_dir).backups_dir()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_layout_follows_topology_json() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        assert_eq!(control_layout(root).pgdata(), Layout::primary(root).pgdata());
        let mut topo = topology::Topology::single();
        topo.epoch = 3;
        topo.nodes.push(DbNode {
            id: "n1".into(),
            set_id: CONTROL_SET_ID.into(),
            name: "r1".into(),
            kind: NodeKind::Local,
            role: NodeRole::Primary,
            url_sealed: None,
            state: NodeState::Ready,
            weight: 1,
            engine_version: None,
            system_identifier: None,
            timeline: Some(2),
            origin: NodeOrigin::Api,
            created_at: String::new(),
            updated_at: String::new(),
        });
        topology::write_topology_file(&root.join(topology::TOPOLOGY_FILE), &topo).unwrap();
        // No data there yet: the original stays.
        assert_eq!(control_layout(root).pgdata(), Layout::primary(root).pgdata());
        let promoted = Layout::node(root, "r1", forgebase_pgengine::NodeKind::Primary).unwrap();
        std::fs::create_dir_all(promoted.pgdata()).unwrap();
        std::fs::write(promoted.pgdata().join("PG_VERSION"), "18\n").unwrap();
        assert_eq!(control_layout(root).pgdata(), promoted.pgdata());
    }

    #[test]
    fn operations_are_cancellable_until_their_point_of_no_return() {
        assert!(cancellable(JobStatus::Queued, false, ""));
        assert!(cancellable(JobStatus::Running, false, "cloning"));
        assert!(!cancellable(JobStatus::Running, true, "cloning"), "already asked");
        for s in POINT_OF_NO_RETURN {
            assert!(!cancellable(JobStatus::Running, false, s), "{s}");
        }
        assert!(!cancellable(JobStatus::Succeeded, false, ""));
    }

    #[test]
    fn config_maps_to_router_options() {
        let mut c = forgebase_core::Config::defaults(Path::new("/srv"));
        c.database.topology.replicas.max_lag_ms = 750;
        c.database.topology.replicas.read_routing = forgebase_core::config::topology_config::ReadRouting::Primary;
        c.database.topology.sharding.placement = forgebase_core::config::topology_config::TenantPlacement::Control;
        let o = router_options(&c);
        assert_eq!(o.replicas.max_lag, Duration::from_millis(750));
        assert_eq!(o.replicas.read_routing, forgebase_db::ReadRouting::Primary);
        assert_eq!(o.policy.mode, forgebase_db::router::PlacementMode::Control);
        c.database.topology.replicas.slot_wal_keep_size = "2GB".into();
        assert_eq!(slot_limits(&c.database.topology.replicas).wal_keep_size, "2GB");
    }
}
