//! Tenant moves, shard drains and rebalancing in the server (ADR-0029
//! §3.8–§3.9, FB-174).
//!
//! * [`Moves`] implements the engine's [`MoveHooks`]: how a target reaches
//!   a source (`source_conninfo`), the administrative connection per set
//!   (`admin`: the managed engine's superuser over its socket, local shard
//!   nodes the same; external nodes use their own URL), and the services
//!   that take part: KV writes the tenant's dirty keys back at the freeze,
//!   pg_cron schedules are reconciled at the freeze and after the switch,
//!   the queue relays drain before the barrier, queue settings follow the
//!   tenant, direct-login roles are recreated on the target.
//! * Reserved jobs: [`kinds::TENANT_MOVE`] runs one move and schedules its
//!   cleanup after the retention ([`kinds::TENANT_CLEANUP`]);
//!   [`kinds::SHARD_DRAIN`] and [`kinds::REBALANCE`] run plans one move at
//!   a time. They report progress and honour cancel before each switch.
//! * An hourly loop refreshes the per-tenant size estimates the plans use.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use forgebase_auth::crypto::Keyring;
use forgebase_core::config::DatabaseConfig;
use forgebase_data::AccessContext;
use forgebase_db::moves::{self, MoveControl, MoveError, MoveHooks, MoveOptions, MoveRecord, RebalancePlan};
use forgebase_db::topology::{CONTROL_SET_ID, DbSet, NodeKind, NodeRole};
use forgebase_db::{Backend, BoxFuture, Database, DbOptions, DbRouter, DbTarget, SecretString, SetHandle, Statement, Value};
use forgebase_jobs::{HandlerError, Job, JobContext, JobRegistry, JobService, NewJob, ProgressReport};
use forgebase_kv::KvService;
use forgebase_pgengine::shard::LocalShards;
use forgebase_queue::QueueService;
use serde_json::{Value as Json, json};

use crate::shards::open_node_url;

/// Job kinds (reserved, on control).
pub mod kinds {
    /// One tenant move (`{move}`).
    pub const TENANT_MOVE: &str = "database.tenant.move";
    /// The cleanup of a switched move after the retention (`{move}`).
    pub const TENANT_CLEANUP: &str = "database.tenant.cleanup";
    /// Move every tenant off a draining shard (`{set}`).
    pub const SHARD_DRAIN: &str = "database.shard.drain";
    /// Run a rebalance plan (`{plan_id}`).
    pub const REBALANCE: &str = "database.rebalance";
}

/// Queue of topology operations (FB-175's operations list reads it).
pub const OPERATIONS_QUEUE: &str = forgebase_api::system_database::OPERATIONS_QUEUE;

/// How often tenant sizes are refreshed.
pub const STATS_INTERVAL: Duration = Duration::from_secs(3600);

/// What [`Moves`] needs from the server.
#[derive(Clone)]
pub struct MovesConfig {
    /// The router.
    pub router: DbRouter,
    /// Jobs (operations run as reserved jobs).
    pub jobs: JobService,
    /// KV, when on.
    pub kv: Option<KvService>,
    /// Queues, when on.
    pub queue: Option<QueueService>,
    /// Unseals node URLs.
    pub keyring: Keyring,
    /// Pool settings for administrative connections.
    pub database: DatabaseConfig,
    /// How targets reach control and how the engine administers it: the
    /// managed engine's superuser URL, or the configured URL of an external
    /// server.
    pub control_url: Option<SecretString>,
    /// Shard primaries on this host.
    pub shards: Option<Arc<LocalShards>>,
    /// Move tuning (`[database.sharding]`).
    pub options: MoveOptions,
}

/// The server side of tenant moves. Cheap to clone.
#[derive(Clone)]
pub struct Moves {
    inner: Arc<Inner>,
}

struct Inner {
    cfg: MovesConfig,
    admins: tokio::sync::Mutex<HashMap<String, Database>>,
    stats: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(t) = self.stats.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take() {
            t.abort();
        }
    }
}

/// Move tuning from `[database.sharding]`.
pub fn options_from(config: &forgebase_core::Config) -> MoveOptions {
    let s = &config.database.topology.sharding;
    MoveOptions {
        freeze_timeout: Duration::from_millis(s.move_freeze_timeout_ms.max(100)),
        source_retention: Duration::from_secs(s.move_source_retention_hours.saturating_mul(3600)),
        ..MoveOptions::default()
    }
}

impl std::fmt::Debug for Moves {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Moves").finish_non_exhaustive()
    }
}

fn hook(e: impl std::fmt::Display) -> MoveError {
    MoveError::Hook(e.to_string())
}

impl Moves {
    /// Moves over `cfg`.
    pub fn new(cfg: MovesConfig) -> Self {
        Self {
            inner: Arc::new(Inner {
                cfg,
                admins: tokio::sync::Mutex::new(HashMap::new()),
                stats: std::sync::Mutex::new(None),
            }),
        }
    }

    fn need_postgres(&self) -> Result<(), MoveError> {
        if self.router().control().kind() == forgebase_db::BackendKind::Postgres {
            Ok(())
        } else {
            Err(MoveError::PostgresRequired)
        }
    }

    /// The router.
    pub fn router(&self) -> &DbRouter {
        &self.inner.cfg.router
    }

    /// Move tuning.
    pub fn options(&self) -> &MoveOptions {
        &self.inner.cfg.options
    }

    /// The URL of `set`'s primary with administrative rights, when this
    /// server knows one: control's configured URL, a local shard node's
    /// superuser socket, or an external node's sealed URL.
    async fn primary_url(&self, set: &DbSet, admin: bool) -> Result<Option<String>, MoveError> {
        if set.is_control() {
            return Ok(self.inner.cfg.control_url.as_ref().map(|u| u.expose().to_owned()));
        }
        let topo = self.router().topology();
        let Some(node) = topo.primary_of(&set.id) else {
            return Err(MoveError::Hook(format!("{} has no primary", set.name)));
        };
        match node.kind {
            NodeKind::Local => {
                let shards = self
                    .inner
                    .cfg
                    .shards
                    .as_ref()
                    .ok_or_else(|| MoveError::Hook("no local shard nodes on this server".into()))?;
                let h = shards
                    .get(&node.name)
                    .await
                    .ok_or_else(|| MoveError::Hook(format!("local shard node {} is not running", node.name)))?;
                let role = if admin {
                    forgebase_pgengine::Role::Super
                } else {
                    forgebase_pgengine::Role::Server
                };
                Ok(Some(h.url(role).expose().to_owned()))
            }
            NodeKind::Remote | NodeKind::External => match node.url_sealed.as_deref() {
                Some(sealed) => Ok(Some(open_node_url(&self.inner.cfg.keyring, &node.id, sealed).map_err(hook)?)),
                None if admin => Ok(None),
                None => Err(MoveError::Hook(format!("node {} has no connection URL", node.name))),
            },
        }
    }

    /// Enqueues the cleanup of a switched move at its due time.
    async fn schedule_cleanup(&self, rec: &MoveRecord) -> Result<(), MoveError> {
        let Some(due) = moves::cleanup_due(rec, self.options()) else {
            return Ok(());
        };
        let mut job = NewJob::new(kinds::TENANT_CLEANUP, json!({ "move": rec.id, "tenant": rec.tenant }));
        job.queue = Some(OPERATIONS_QUEUE.to_owned());
        job.run_at = Some(due);
        job.max_attempts = Some(10);
        job.idempotency_key = Some(format!("cleanup:{}", rec.id));
        self.inner
            .cfg
            .jobs
            .enqueue(&AccessContext::system(), job)
            .await
            .map_err(hook)?;
        Ok(())
    }

    /// Starts a move of `tenant` to set `to`: the move row, then its job.
    /// Returns the move and the job.
    ///
    /// # Errors
    /// The engine's refusals ([`moves::begin`]) and job errors.
    pub async fn start_move(
        &self,
        ctx: &AccessContext,
        tenant: &str,
        to: &str,
        created_by: Option<String>,
    ) -> Result<(MoveRecord, Job), MoveError> {
        let rec = moves::begin(
            self.router(),
            self,
            self.options(),
            moves::BeginMove {
                tenant: tenant.to_owned(),
                to: to.to_owned(),
                created_by,
                ..moves::BeginMove::default()
            },
        )
        .await?;
        let job = self.enqueue_move(ctx, &rec).await?;
        Ok((rec, job))
    }

    /// Starts the move back of switched move `id` ("Undo move").
    ///
    /// # Errors
    /// The move is not switched, or the engine's refusals.
    pub async fn undo_move(&self, ctx: &AccessContext, id: &str, created_by: Option<String>) -> Result<(MoveRecord, Job), MoveError> {
        let rec = moves::undo(self.router(), self, self.options(), id, created_by).await?;
        let job = self.enqueue_move(ctx, &rec).await?;
        Ok((rec, job))
    }

    async fn enqueue_move(&self, ctx: &AccessContext, rec: &MoveRecord) -> Result<Job, MoveError> {
        let mut job = NewJob::new(
            kinds::TENANT_MOVE,
            json!({ "move": rec.id, "tenant": rec.tenant, "set": rec.from_set, "to": rec.to_set }),
        );
        job.queue = Some(OPERATIONS_QUEUE.to_owned());
        job.max_attempts = Some(5);
        let control = self.router().control();
        let (job, _) = match self.inner.cfg.jobs.enqueue(ctx, job).await {
            Ok(j) => j,
            Err(e) => {
                let _ = moves::abort(self.router(), self, self.options(), &rec.id, "its job could not be queued").await;
                return Err(hook(e));
            }
        };
        moves::set_job(&control, &rec.id, &job.id).await?;
        Ok(job)
    }

    /// A rebalance plan over fresh size estimates, stored for
    /// [`Moves::start_rebalance`].
    ///
    /// # Errors
    /// Database errors.
    pub async fn plan(&self, options: &moves::PlanOptions) -> Result<RebalancePlan, MoveError> {
        self.need_postgres()?;
        moves::rebalance::refresh_stats(self.router()).await?;
        let loads = moves::rebalance::tenant_loads(self.router()).await?;
        let plan = moves::plan_rebalance(&self.router().topology().sets, &loads, options);
        moves::rebalance::save_plan(&self.router().control(), &plan).await?;
        Ok(plan)
    }

    /// Runs stored plan `plan_id` as job [`kinds::REBALANCE`].
    ///
    /// # Errors
    /// Unknown or expired plan, job errors.
    pub async fn start_rebalance(&self, ctx: &AccessContext, plan_id: &str) -> Result<Job, MoveError> {
        let plan = moves::rebalance::load_plan(&self.router().control(), plan_id).await?;
        let mut job = NewJob::new(kinds::REBALANCE, json!({ "plan_id": plan.id, "moves": plan.moves.len() }));
        job.queue = Some(OPERATIONS_QUEUE.to_owned());
        job.max_attempts = Some(3);
        let (job, _) = self.inner.cfg.jobs.enqueue(ctx, job).await.map_err(hook)?;
        Ok(job)
    }

    /// Marks shard `set` draining and runs the drain as job
    /// [`kinds::SHARD_DRAIN`].
    ///
    /// # Errors
    /// Control, an unknown set, job errors.
    pub async fn start_drain(&self, ctx: &AccessContext, set: &str) -> Result<(RebalancePlan, Job), MoveError> {
        self.need_postgres()?;
        moves::rebalance::refresh_stats(self.router()).await?;
        let plan = moves::drain_set(self.router(), set).await?;
        let mut job = NewJob::new(kinds::SHARD_DRAIN, json!({ "set": set, "plan_id": plan.id }));
        job.queue = Some(OPERATIONS_QUEUE.to_owned());
        job.max_attempts = Some(3);
        let (job, _) = self.inner.cfg.jobs.enqueue(ctx, job).await.map_err(hook)?;
        Ok((plan, job))
    }

    /// Refreshes size estimates every [`STATS_INTERVAL`] while the cluster
    /// is sharded, until the last clone of this service is dropped.
    pub fn spawn_stats(&self) {
        let weak = Arc::downgrade(&self.inner);
        let task = tokio::spawn(async move {
            let mut tick = tokio::time::interval(STATS_INTERVAL);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                let Some(inner) = weak.upgrade() else { return };
                let me = Moves { inner };
                if !me.router().is_sharded() {
                    continue;
                }
                match moves::rebalance::refresh_stats(me.router()).await {
                    Ok(n) => tracing::debug!(tenants = n, "tenant size estimates refreshed"),
                    Err(e) => tracing::warn!(error = %e, "tenant size estimates not refreshed"),
                }
            }
        });
        if let Some(old) = self
            .inner
            .stats
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .replace(task)
        {
            old.abort();
        }
    }

    async fn run_move_job(&self, ctx: &JobContext) -> Result<Json, HandlerError> {
        let id = payload_str(ctx, "move")?;
        let control = JobProgress(ctx);
        match moves::run(self.router(), self, &control, self.options(), id).await {
            Ok(rec) => {
                if rec.is_switched() {
                    self.schedule_cleanup(&rec).await.map_err(|e| HandlerError::retry(e.to_string()))?;
                }
                Ok(move_result(&rec))
            }
            Err(e) if e.is_transient() => Err(HandlerError::retry(e.to_string())),
            Err(e) => Err(HandlerError::fatal(format!("{e} ({})", forgebase_core::ErrorCode::code(&e)))),
        }
    }

    async fn run_cleanup_job(&self, ctx: &JobContext) -> Result<Json, HandlerError> {
        let id = payload_str(ctx, "move")?;
        match moves::cleanup(self.router(), self, self.options(), id).await {
            Ok(rec) => Ok(move_result(&rec)),
            Err(e) if e.is_transient() => Err(HandlerError::retry(e.to_string())),
            Err(e) => Err(HandlerError::fatal(e.to_string())),
        }
    }

    async fn run_plan_job(&self, ctx: &JobContext, plan: RebalancePlan) -> Result<Json, HandlerError> {
        let control = JobProgress(ctx);
        let out = moves::run_plan(self.router(), self, &control, self.options(), &plan, None)
            .await
            .map_err(|e| HandlerError::retry(e.to_string()))?;
        for m in out.moves.iter().filter(|m| m.is_switched()) {
            if let Err(e) = self.schedule_cleanup(m).await {
                tracing::warn!(move_id = %m.id, error = %e, "move cleanup not scheduled");
            }
        }
        let result = json!({
            "plan_id": plan.id,
            "moved": out.moves.iter().filter(|m| m.is_switched()).count(),
            "moves": out.moves.iter().map(move_result).collect::<Vec<_>>(),
            "skipped": out.skipped,
            "failed": out.failed.as_ref().map(|(t, e)| json!({ "tenant": t, "error": e })),
        });
        match &out.failed {
            Some((t, e)) => Err(HandlerError::fatal(format!("the move of {t} failed: {e}; later moves did not run"))),
            None if ctx.is_cancelled() => Err(HandlerError::fatal("cancelled between moves".to_owned())),
            None => Ok(result),
        }
    }

    async fn run_rebalance_job(&self, ctx: &JobContext) -> Result<Json, HandlerError> {
        let id = payload_str(ctx, "plan_id")?;
        let plan = moves::rebalance::load_plan(&self.router().control(), id)
            .await
            .map_err(|e| HandlerError::fatal(e.to_string()))?;
        self.run_plan_job(ctx, plan).await
    }

    async fn run_drain_job(&self, ctx: &JobContext) -> Result<Json, HandlerError> {
        let set = payload_str(ctx, "set")?;
        // A fresh plan: placements may have changed since it was queued.
        let plan = moves::drain_set(self.router(), set)
            .await
            .map_err(|e| HandlerError::fatal(e.to_string()))?;
        self.run_plan_job(ctx, plan).await
    }
}

fn move_result(rec: &MoveRecord) -> Json {
    json!({
        "move": rec.id,
        "tenant": rec.tenant,
        "from": rec.from_set,
        "to": rec.to_set,
        "state": rec.state.as_str(),
        "freeze_ms": rec.detail.freeze_ms,
        "rows": rec.detail.rows.values().sum::<u64>(),
        "error": rec.error,
    })
}

fn payload_str<'a>(ctx: &'a JobContext, key: &str) -> Result<&'a str, HandlerError> {
    ctx.payload()
        .get(key)
        .and_then(Json::as_str)
        .ok_or_else(|| HandlerError::fatal(format!("bad payload: no {key}")))
}

/// Job progress and cancel for the engine.
struct JobProgress<'a>(&'a JobContext);

impl MoveControl for JobProgress<'_> {
    fn report<'a>(&'a self, stage: &'a str, percent: f64, message: String) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let r = self
                .0
                .progress(ProgressReport {
                    percent: Some(percent.clamp(0.0, 100.0)),
                    stage: Some(stage.to_owned()),
                    message: Some(message),
                })
                .await;
            if let Err(e) = r {
                tracing::debug!(error = %e, "move progress not stored");
            }
        })
    }

    fn cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

impl MoveHooks for Moves {
    fn source_conninfo<'a>(&'a self, source: &'a DbSet) -> BoxFuture<'a, Result<SecretString, MoveError>> {
        Box::pin(async move {
            match self.primary_url(source, true).await? {
                Some(url) => Ok(SecretString::new(url)),
                None => Err(MoveError::Hook(format!(
                    "no connection URL for the primary of {}; targets cannot subscribe to it",
                    source.name
                ))),
            }
        })
    }

    fn admin<'a>(&'a self, set: &'a DbSet) -> BoxFuture<'a, Result<Option<Database>, MoveError>> {
        Box::pin(async move {
            if let Some(db) = self.inner.admins.lock().await.get(&set.id) {
                return Ok(Some(db.clone()));
            }
            // External nodes: the router's pool (the operator's role).
            let managed = set.is_control() && self.inner.cfg.control_url.is_some()
                || self
                    .router()
                    .topology()
                    .primary_of(&set.id)
                    .is_some_and(|n| n.kind == NodeKind::Local && n.role == NodeRole::Primary);
            if !managed {
                return Ok(None);
            }
            let Some(url) = self.primary_url(set, true).await? else {
                return Ok(None);
            };
            let mut opts = DbOptions::from_config_target(&self.inner.cfg.database, DbTarget::postgres(url, false));
            opts.read_pool_size = 2;
            let db = Database::open(opts).await.map_err(MoveError::Db)?;
            self.inner.admins.lock().await.insert(set.id.clone(), db.clone());
            Ok(Some(db))
        })
    }

    fn prepare_target<'a>(
        &'a self,
        tenant: &'a str,
        source: &'a SetHandle,
        target: &'a SetHandle,
    ) -> BoxFuture<'a, Result<(), MoveError>> {
        Box::pin(async move {
            // The tenant's direct-login roles (FB-153), same SCRAM verifier.
            let control = self.router().control();
            let exists = control
                .fetch_all(&Statement::raw(
                    "SELECT count(*) FROM pg_catalog.pg_tables WHERE schemaname = 'forgebase' \
                     AND tablename = '_forgebase_db_logins'",
                ))
                .await?;
            if exists.scalar().and_then(Value::as_i64).unwrap_or(0) == 0 {
                return Ok(());
            }
            let rows = control
                .fetch_all(&Statement::new(
                    "SELECT role_name FROM _forgebase_db_logins WHERE tenant = ?1",
                    vec![tenant.into()],
                ))
                .await?;
            for r in &rows.rows {
                let Some(role) = r.first().and_then(Value::as_str) else { continue };
                forgebase_policy::native::copy_login_role(source.database(), target.database(), role, None)
                    .await
                    .map_err(hook)?;
            }
            Ok(())
        })
    }

    fn frozen<'a>(&'a self, tenant: &'a str) -> BoxFuture<'a, Result<(), MoveError>> {
        Box::pin(async move {
            if let Some(kv) = &self.inner.cfg.kv {
                kv.flush_tenant(tenant).await.map_err(hook)?;
            }
            self.inner.cfg.jobs.sync_pgcron_now().await.map_err(hook)?;
            Ok(())
        })
    }

    fn drain<'a>(&'a self, tenant: &'a str, source: &'a SetHandle) -> BoxFuture<'a, Result<(), MoveError>> {
        let _ = (tenant, source);
        Box::pin(async move {
            let Some(queue) = &self.inner.cfg.queue else {
                return Ok(());
            };
            // Every set's relay, control's cursor of each set included, until
            // nothing is left (pages of 200).
            for _ in 0..100 {
                if queue.relay_outbox().await.map_err(hook)? == 0 {
                    break;
                }
            }
            Ok(())
        })
    }

    fn switched<'a>(
        &'a self,
        tenant: &'a str,
        source: &'a SetHandle,
        target: &'a SetHandle,
    ) -> BoxFuture<'a, Result<(), MoveError>> {
        Box::pin(async move {
            let jobs = &self.inner.cfg.jobs;
            let copied = jobs.copy_queue_settings(tenant).await.map_err(hook)?;
            jobs.sync_pgcron_now().await.map_err(hook)?;
            tracing::info!(
                tenant,
                from = %source.id(),
                to = %target.id(),
                queue_settings = copied,
                "tenant switched"
            );
            Ok(())
        })
    }

    fn unfrozen<'a>(&'a self, tenant: &'a str) -> BoxFuture<'a, Result<(), MoveError>> {
        let _ = tenant;
        Box::pin(async move {
            self.inner.cfg.jobs.sync_pgcron_now().await.map_err(hook)?;
            Ok(())
        })
    }
}

/// Registers the reserved move, cleanup, drain and rebalance handlers.
pub fn register_jobs(registry: JobRegistry, moves: Moves) -> JobRegistry {
    let (m1, m2, m3, m4) = (moves.clone(), moves.clone(), moves.clone(), moves);
    registry
        .register(kinds::TENANT_MOVE, move |ctx: JobContext| {
            let m = m1.clone();
            async move { m.run_move_job(&ctx).await }
        })
        .register(kinds::TENANT_CLEANUP, move |ctx: JobContext| {
            let m = m2.clone();
            async move { m.run_cleanup_job(&ctx).await }
        })
        .register(kinds::SHARD_DRAIN, move |ctx: JobContext| {
            let m = m3.clone();
            async move { m.run_drain_job(&ctx).await }
        })
        .register(kinds::REBALANCE, move |ctx: JobContext| {
            let m = m4.clone();
            async move { m.run_rebalance_job(&ctx).await }
        })
}

/// The URL targets use to reach control, from the configuration: an
/// external server's URL (the managed engine's comes from its handle).
pub fn external_control_url(config: &forgebase_core::Config) -> Option<SecretString> {
    config
        .database
        .postgres
        .url
        .as_ref()
        .map(|u| SecretString::new(u.expose().to_owned()))
}

/// Whether set `id` is control.
pub fn is_control(id: &str) -> bool {
    id == CONTROL_SET_ID
}
