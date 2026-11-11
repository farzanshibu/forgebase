//! [`JobService`]: every job operation, each one transactional.
//!
//! All state lives in SQLite (`_forgebase_jobs` and friends, migrations 0007
//! and 0022). The database has one writer, so a claim, a completion or a
//! firing is atomic without further locking, and anything committed survives
//! a crash or restart.
//!
//! Public methods take an [`AccessContext`] and ask the
//! [`Authorizer`](forgebase_data::Authorizer) of the data core first
//! ([`Action::ReadJobs`], [`Action::EnqueueJob`], [`Action::ManageJobs`],
//! [`Action::WorkJobs`]). In-process machinery (the worker pool) uses
//! [`AccessContext::system`].
//!
//! Side effects that must wait for the commit (live updates, waking the pool,
//! telling an in-process handler it was cancelled, deleting stored results)
//! are collected in an [`Fx`] and applied after the transaction committed.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, RwLock};

use forgebase_data::{AccessContext, Action, DataService};
use forgebase_db::{Backend, ReadVia, Rows, Statement, TxMode, Value, WriteTx};
use serde_json::Value as Json;
use tokio::sync::{Notify, watch};

use crate::access::{JobAccess, KindAccess, Owner, RecordedContext, Scope};
use crate::clock::{Clock, SystemClock};
use crate::cron::Cron;
use crate::error::JobError;
use crate::hooks::{JobEventSink, RealtimeSink, ResultStore, channel};
use crate::model::{
    ClaimRequest, ClaimedJob, Heartbeat, Job, JobEvent, JobFilter, JobPage, JobStats, JobStatus, MAX_CLAIM, MAX_PAGE,
    NewJob, NewSchedule, NewWorker, Policy, QueueStats, Resolved, Schedule, SchedulePatch, WORKER_ONLINE_MS, Worker,
    backoff_ms, check_name, check_payload, iso, jitter,
};
use crate::tasks::{
    Attempt, Checkpoint, JobUpdate, MAX_CHECKPOINT_BYTES, MAX_CHILDREN, MAX_INLINE_RESULT_BYTES, MAX_RESULT_BYTES,
    ParentResult, Progress, ProgressReport, WorkflowUpdate,
};

pub(crate) type Result<T> = std::result::Result<T, JobError>;

/// Current wall-clock time in Unix epoch milliseconds.
pub fn now_ms() -> i64 {
    SystemClock.now_ms()
}

/// The job queue. Cheap to clone.
#[derive(Debug, Clone)]
pub struct JobService {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    data: DataService,
    clock: Arc<dyn Clock>,
    wake: Notify,
    sink: RwLock<Option<Arc<dyn JobEventSink>>>,
    store: RwLock<Option<Arc<dyn ResultStore>>>,
    /// Cancellation signals of the handlers running in this process.
    local: Mutex<HashMap<String, Arc<watch::Sender<bool>>>>,
    /// Tenancy, kinds and channel owners (FB-142).
    access: RwLock<JobAccess>,
    /// Schedules run on pg_cron ([`crate::attach_pgcron`], FB-165).
    pgcron: std::sync::atomic::AtomicBool,
}

/// Who creates a job: recorded in `created_by`, `tenant` and `context`.
#[derive(Debug, Clone, Default)]
pub(crate) struct Origin {
    pub actor: String,
    pub tenant: Option<String>,
    pub context: Option<String>,
}

impl Origin {
    /// The caller itself.
    pub(crate) fn of(ctx: &AccessContext) -> Self {
        Self {
            actor: ctx.actor(),
            tenant: ctx.tenant.clone(),
            context: RecordedContext::encode(ctx),
        }
    }

    /// A job created for `parent` by `ctx` (child jobs): the parent's tenant;
    /// the parent's creator and context when the system (an in-process
    /// handler) spawns it, else the caller's.
    pub(crate) fn child(ctx: &AccessContext, parent: &Job) -> Self {
        if ctx.is_system() {
            Self {
                actor: parent.created_by.clone(),
                tenant: parent.tenant.clone(),
                context: parent.context.clone(),
            }
        } else {
            Self {
                tenant: parent.tenant.clone(),
                ..Self::of(ctx)
            }
        }
    }
}

pub(crate) const JOB_COLS: &str = "id, queue, kind, payload, priority, status, run_at, attempts, max_attempts, \
    backoff_base_ms, backoff_max_ms, visibility_timeout_ms, idempotency_key, schedule_id, worker_id, lease_id, \
    visibility_until, cancel_requested, result, last_error, created_by, created_at, updated_at, started_at, \
    finished_at, progress_percent, progress_stage, progress_message, progress_at, checkpoint IS NOT NULL AS \
    has_checkpoint, checkpoint_at, timeout_ms, deadline_at, heartbeat_at, workflow_id, step, parent_id, compensates, \
    waiting_for, result_ref, result_size, tenant, context";

pub(crate) const SCHEDULE_COLS: &str = "id, name, cron, queue, kind, payload, priority, max_attempts, backoff_base_ms, \
    backoff_max_ms, visibility_timeout_ms, enabled, next_run_at, last_run_at, last_job_id, created_by, created_at, \
    updated_at, tenant, context";

/// Effects of a transaction, applied after it committed, and what the
/// transaction's SQL needs to know about the engine.
#[derive(Debug)]
pub(crate) struct Fx {
    /// Column that orders job and workflow rows by insertion
    /// ([`forgebase_db::Dialect::row_order_col`]): SQLite's `rowid`, the
    /// `_rowid` identity column on PostgreSQL. Ties of `created_at` and
    /// `finished_at` break on it.
    pub row_order: &'static str,
    /// Jobs to publish: (id, `status` or `progress`).
    pub jobs: Vec<(String, &'static str)>,
    /// Workflows whose state changed.
    pub workflows: BTreeSet<String>,
    /// Running jobs whose handler should stop.
    pub cancel: Vec<String>,
    /// New claimable work.
    pub wake: bool,
    /// Result-store keys to delete.
    pub drop_objects: Vec<String>,
}

impl Fx {
    pub(crate) fn new(row_order: &'static str) -> Self {
        Self {
            row_order,
            jobs: Vec::new(),
            workflows: BTreeSet::new(),
            cancel: Vec::new(),
            wake: false,
            drop_objects: Vec::new(),
        }
    }

    pub(crate) fn job(&mut self, id: &str, event: &'static str) {
        if !self.jobs.iter().any(|(i, e)| i == id && *e == event) {
            self.jobs.push((id.to_owned(), event));
        }
    }
}

impl JobService {
    /// A job service over the data core's database and authorizer.
    pub fn new(data: DataService) -> Self {
        Self::with_clock(data, Arc::new(SystemClock))
    }

    /// A job service that reads time from `clock` (tests drive it by hand).
    pub fn with_clock(data: DataService, clock: Arc<dyn Clock>) -> Self {
        Self {
            inner: Arc::new(Inner {
                data,
                clock,
                wake: Notify::new(),
                sink: RwLock::new(None),
                store: RwLock::new(None),
                local: Mutex::new(HashMap::new()),
                access: RwLock::new(JobAccess::new()),
                pgcron: std::sync::atomic::AtomicBool::new(false),
            }),
        }
    }

    /// Shares `access` with this service (FB-142): the server hands the same
    /// state to its [`crate::JobChannelAuthorizer`], so job channels follow
    /// the visibility of the jobs. Kinds installed earlier are kept.
    pub fn attach_access(&self, access: JobAccess) {
        let mut cur = self.inner.access.write().unwrap_or_else(|p| p.into_inner());
        let kinds: Vec<(String, KindAccess)> = cur.kinds_raw();
        access.set_kinds(kinds);
        *cur = access;
    }

    /// The tenancy state of this service (permission check, kinds, owners).
    pub fn access(&self) -> JobAccess {
        self.inner.access.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Records who may enqueue each kind of `registry` ([`KindAccess`]).
    /// [`crate::start_pool`] calls it; call it yourself when the pool runs
    /// elsewhere.
    pub fn install_kinds(&self, registry: &crate::JobRegistry) {
        self.access()
            .set_kinds(registry.accesses().map(|(k, a)| (k.clone(), a.clone())));
    }

    /// Answers [`crate::READ_ALL_KEY`] with `check` (the policy engine).
    pub fn attach_permissions(&self, check: crate::PermissionCheck) {
        self.access().attach_permissions(check);
    }

    /// Fails with `not found` unless `ctx` may see job `id` (FB-142).
    pub(crate) async fn visible(&self, ctx: &AccessContext, id: &str) -> Result<()> {
        let scope = self.access().scope(ctx);
        if scope == Scope::All {
            return Ok(());
        }
        let rows = self
            .backend()
            .fetch_all(&Statement::new(
                "SELECT j.tenant, j.created_by, s.created_by AS schedule_creator FROM _forgebase_jobs j \
                 LEFT JOIN _forgebase_job_schedules s ON s.id = j.schedule_id WHERE j.id = ?1",
                vec![id.into()],
            ))
            .await?;
        if rows.is_empty() {
            return Err(JobError::NotFound(id.to_owned()));
        }
        let r = Row::new(&rows, 0);
        let owner = Owner {
            tenant: r.opt_text("tenant"),
            created_by: r.text("created_by"),
        };
        if !scope.sees(&owner, r.opt_text("schedule_creator").as_deref()) {
            return Err(JobError::NotFound(id.to_owned()));
        }
        self.access().remember(id, owner);
        Ok(())
    }

    /// Remembers the owners of `jobs` for job channel checks.
    pub(crate) fn remember_jobs<'a>(&self, jobs: impl IntoIterator<Item = &'a Job>) {
        let access = self.access();
        for j in jobs {
            access.remember(
                &j.id,
                Owner {
                    tenant: j.tenant.clone(),
                    created_by: j.created_by.clone(),
                },
            );
        }
    }

    /// Publishes live job and workflow updates to `sink` from now on.
    pub fn attach_events(&self, sink: Arc<dyn JobEventSink>) {
        *self.inner.sink.write().unwrap_or_else(|p| p.into_inner()) = Some(sink);
    }

    /// Publishes live updates on FB-40 realtime channels `jobs:<id>`.
    pub fn attach_realtime(&self, realtime: forgebase_realtime::RealtimeService) {
        self.attach_events(Arc::new(RealtimeSink(realtime)));
    }

    /// Keeps results larger than [`MAX_INLINE_RESULT_BYTES`] in `store`.
    /// Without one, such results are rejected (`payload_too_large`).
    pub fn attach_result_store(&self, store: Arc<dyn ResultStore>) {
        *self.inner.store.write().unwrap_or_else(|p| p.into_inner()) = Some(store);
    }

    fn sink(&self) -> Option<Arc<dyn JobEventSink>> {
        self.inner.sink.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub(crate) fn store(&self) -> Option<Arc<dyn ResultStore>> {
        self.inner.store.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Current time in Unix epoch milliseconds, from this service's clock.
    pub fn now(&self) -> i64 {
        self.inner.clock.now_ms()
    }

    /// The reads of one §2.5 call site (ADR-0029, FB-169): job list, get,
    /// events, attempts and stats may be served by a replica. Claims,
    /// heartbeats, the result and checkpoint reads (the work in flight) and
    /// every write keep [`Self::backend`].
    pub(crate) fn reader(&self, what: &'static str) -> ReadVia<'_> {
        ReadVia::replica(self.backend(), what)
    }

    pub(crate) fn backend(&self) -> &dyn Backend {
        self.inner.data.backend().as_ref()
    }

    /// Column that orders job and workflow rows by insertion
    /// (`rowid` / `_rowid`, ADR-0028 §7.4).
    pub(crate) fn row_order(&self) -> &'static str {
        self.backend().dialect().row_order_col()
    }

    /// Effects of a new transaction.
    pub(crate) fn fx(&self) -> Fx {
        Fx::new(self.row_order())
    }

    pub(crate) fn allow(&self, ctx: &AccessContext, action: Action) -> Result<()> {
        Ok(self.inner.data.authorize_action(ctx, action)?)
    }

    /// Asks the authorizer without doing anything. The HTTP layer calls it
    /// before it looks at a request body, so a caller who may not use an
    /// endpoint learns nothing about its input format.
    pub fn authorize(&self, ctx: &AccessContext, action: Action) -> Result<()> {
        self.allow(ctx, action)
    }

    /// Resolves when new work may be available (a job was enqueued, retried or
    /// released in this process). The worker pool waits on it between polls.
    pub async fn work_available(&self) {
        self.inner.wake.notified().await;
    }

    fn wake(&self) {
        self.inner.wake.notify_one();
    }

    pub(crate) fn pgcron_flag(&self) -> bool {
        self.inner.pgcron.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub(crate) fn set_pgcron(&self, on: bool) {
        self.inner.pgcron.store(on, std::sync::atomic::Ordering::Relaxed);
    }

    pub(crate) fn wake_pool(&self) {
        self.wake();
    }

    pub(crate) fn register_local(&self, id: &str, cancel: Arc<watch::Sender<bool>>) {
        self.inner
            .local
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(id.to_owned(), cancel);
    }

    pub(crate) fn unregister_local(&self, id: &str) {
        self.inner.local.lock().unwrap_or_else(|p| p.into_inner()).remove(id);
    }

    pub(crate) async fn begin(&self) -> Result<Box<dyn WriteTx>> {
        Ok(self.backend().begin(TxMode::Normal).await?)
    }

    /// Commits (or rolls back) and, after a commit, applies the effects.
    pub(crate) async fn commit<T>(&self, tx: Box<dyn WriteTx>, result: Result<T>, fx: Fx) -> Result<T> {
        let v = finish(tx, result).await?;
        self.apply(fx).await;
        Ok(v)
    }

    async fn apply(&self, fx: Fx) {
        if fx.wake {
            self.wake();
        }
        if !fx.cancel.is_empty() {
            let local = self.inner.local.lock().unwrap_or_else(|p| p.into_inner());
            for id in &fx.cancel {
                if let Some(s) = local.get(id) {
                    let _ = s.send(true);
                }
            }
        }
        if let Some(sink) = self.sink() {
            for (id, event) in &fx.jobs {
                match fetch_job(self.backend(), id).await {
                    Ok(Some(job)) => {
                        self.remember_jobs([&job]);
                        let data = serde_json::to_value(JobUpdate::of(&job, event)).unwrap_or(Json::Null);
                        if let Some(w) = &job.workflow_id {
                            sink.publish(channel(w), data.clone()).await;
                        }
                        sink.publish(channel(id), data).await;
                    }
                    Ok(None) => {}
                    Err(e) => tracing::debug!(job = %id, error = %e, "job update not published"),
                }
            }
            for id in &fx.workflows {
                if let Ok(Some(workflow)) = crate::flow::fetch_workflow(self.backend(), id).await {
                    self.access().remember(
                        id,
                        Owner {
                            tenant: workflow.tenant.clone(),
                            created_by: workflow.created_by.clone(),
                        },
                    );
                    let data = serde_json::to_value(WorkflowUpdate {
                        event: "workflow".into(),
                        workflow,
                    })
                    .unwrap_or(Json::Null);
                    sink.publish(channel(id), data).await;
                }
            }
        }
        if !fx.drop_objects.is_empty()
            && let Some(store) = self.store()
        {
            for key in fx.drop_objects {
                if let Err(e) = store.delete(key.clone()).await {
                    tracing::warn!(%key, error = %e, "stored job result not deleted");
                }
            }
        }
    }

    // ---------------------------------------------------------------- enqueue

    /// Enqueues a job. With an idempotency key that already exists in the
    /// queue, nothing is inserted and the first job is returned with
    /// `created == false`. With `depends_on`, the job waits until every
    /// listed job succeeded (and is cancelled if one of them fails).
    ///
    /// The job records the caller's tenant and context (FB-142); a kind the
    /// caller may not enqueue ([`KindAccess`]) is refused (`403`). With an
    /// idempotency key, only a job the caller may see is returned; another
    /// tenant's key never matches, and another caller's in the same tenant
    /// is a conflict.
    pub async fn enqueue(&self, ctx: &AccessContext, new: NewJob) -> Result<(Job, bool)> {
        self.allow(ctx, Action::EnqueueJob)?;
        let now = self.now();
        let resolved = new.resolve(now)?;
        self.access().admit(ctx, &new.kind, &new.payload)?;
        let origin = Origin::of(ctx);
        let scope = self.access().scope(ctx);
        let mut fx = self.fx();
        let mut tx = self.begin().await?;
        let r = async {
            let (job, created) = insert_job(&mut *tx, &resolved, None, &origin, now, &mut fx).await?;
            if !created && !scope.sees(&owner_of(&job), None) {
                return Err(JobError::state(
                    "idempotency_key_in_use",
                    "another caller's job in this queue has this idempotency key",
                ));
            }
            Ok((job, created))
        }
        .await;
        self.commit(tx, r, fx).await
    }

    // ------------------------------------------------------------------ reads

    /// One job.
    pub async fn get(&self, ctx: &AccessContext, id: &str) -> Result<Job> {
        self.allow(ctx, Action::ReadJobs)?;
        self.visible(ctx, id).await?;
        fetch_job(&self.reader("jobs.get"), id)
            .await?
            .ok_or_else(|| JobError::NotFound(id.to_owned()))
    }

    /// Jobs, newest first.
    pub async fn list(&self, ctx: &AccessContext, filter: JobFilter) -> Result<JobPage> {
        self.allow(ctx, Action::ReadJobs)?;
        let mut params: Vec<Value> = Vec::new();
        let mut where_sql = self.access().scope(ctx).sql("", true, &mut params);
        if let Some(status) = filter.status {
            params.push(status.as_str().into());
            where_sql.push_str(&format!(" AND status = ?{}", params.len()));
        }
        for (col, v) in [
            ("queue", &filter.queue),
            ("kind", &filter.kind),
            ("workflow_id", &filter.workflow_id),
            ("parent_id", &filter.parent_id),
        ] {
            if let Some(v) = v {
                params.push(v.as_str().into());
                where_sql.push_str(&format!(" AND {col} = ?{}", params.len()));
            }
        }
        let total = self
            .reader("jobs.list")
            .fetch_all(&Statement::new(
                format!("SELECT COUNT(*) FROM _forgebase_jobs WHERE {where_sql}"),
                params.clone(),
            ))
            .await?
            .scalar()
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let limit = filter.limit.unwrap_or(50).clamp(1, MAX_PAGE);
        let offset = filter.offset.unwrap_or(0).max(0);
        params.push(limit.into());
        params.push(offset.into());
        let rows = self
            .reader("jobs.list")
            .fetch_all(&Statement::new(
                format!(
                    "SELECT {JOB_COLS} FROM _forgebase_jobs WHERE {where_sql} \
                     ORDER BY created_at DESC, {ord} DESC LIMIT ?{} OFFSET ?{}",
                    params.len() - 1,
                    params.len(),
                    ord = self.row_order()
                ),
                params,
            ))
            .await?;
        let items: Vec<Job> = (0..rows.len())
            .map(|i| job_from(&Row::new(&rows, i)))
            .collect::<Result<_>>()?;
        self.remember_jobs(&items);
        Ok(JobPage { items, total })
    }

    /// The history of a job, oldest first.
    pub async fn events(&self, ctx: &AccessContext, id: &str) -> Result<Vec<JobEvent>> {
        self.allow(ctx, Action::ReadJobs)?;
        self.visible(ctx, id).await?;
        let rows = self
            .reader("jobs.events")
            .fetch_all(&Statement::new(
                "SELECT id, at, event, attempt, worker_id, message FROM _forgebase_job_events \
                 WHERE job_id = ?1 ORDER BY id",
                vec![id.into()],
            ))
            .await?;
        Ok((0..rows.len())
            .map(|i| {
                let r = Row::new(&rows, i);
                JobEvent {
                    id: r.int("id"),
                    at: iso(r.int("at")),
                    event: r.text("event"),
                    attempt: r.int("attempt"),
                    worker_id: r.opt_text("worker_id"),
                    message: r.opt_text("message"),
                }
            })
            .collect())
    }

    /// The attempts of a job with their timings, oldest first.
    pub async fn attempts(&self, ctx: &AccessContext, id: &str) -> Result<Vec<Attempt>> {
        self.allow(ctx, Action::ReadJobs)?;
        self.visible(ctx, id).await?;
        let rows = self
            .reader("jobs.attempts")
            .fetch_all(&Statement::new(
                "SELECT attempt, worker_id, started_at, finished_at, outcome, error, from_checkpoint \
                 FROM _forgebase_job_attempts WHERE job_id = ?1 ORDER BY id",
                vec![id.into()],
            ))
            .await?;
        let now = self.now();
        Ok((0..rows.len())
            .map(|i| {
                let r = Row::new(&rows, i);
                let started = r.int("started_at");
                let finished = r.opt_int("finished_at");
                Attempt {
                    attempt: r.int("attempt"),
                    worker_id: r.opt_text("worker_id"),
                    started_at: iso(started),
                    finished_at: finished.map(iso),
                    duration_ms: (finished.unwrap_or(now) - started).max(0),
                    outcome: r.opt_text("outcome"),
                    error: r.opt_text("error"),
                    from_checkpoint: r.int("from_checkpoint") == 1,
                }
            })
            .collect())
    }

    /// The last checkpoint of a job.
    pub async fn checkpoint(&self, ctx: &AccessContext, id: &str) -> Result<Checkpoint> {
        self.allow(ctx, Action::ReadJobs)?;
        self.visible(ctx, id).await?;
        let rows = self
            .backend()
            .fetch_all(&Statement::new(
                "SELECT checkpoint, checkpoint_at FROM _forgebase_jobs WHERE id = ?1",
                vec![id.into()],
            ))
            .await?;
        if rows.is_empty() {
            return Err(JobError::NotFound(id.to_owned()));
        }
        let r = Row::new(&rows, 0);
        Ok(Checkpoint {
            job_id: id.to_owned(),
            checkpoint: r.opt_json("checkpoint")?,
            saved_at: r.opt_int("checkpoint_at").map(iso),
        })
    }

    /// The result of a succeeded job, read from the result store when it is
    /// kept there. `None` when the handler returned none.
    pub async fn result(&self, ctx: &AccessContext, id: &str) -> Result<Option<Json>> {
        self.allow(ctx, Action::ReadJobs)?;
        self.visible(ctx, id).await?;
        let rows = self
            .backend()
            .fetch_all(&Statement::new(
                "SELECT status, result, result_ref FROM _forgebase_jobs WHERE id = ?1",
                vec![id.into()],
            ))
            .await?;
        if rows.is_empty() {
            return Err(JobError::NotFound(id.to_owned()));
        }
        let r = Row::new(&rows, 0);
        if r.text("status") != "succeeded" {
            return Err(JobError::state(
                "job_not_succeeded",
                format!("job is {}; only succeeded jobs have a result", r.text("status")),
            ));
        }
        self.load_result(id, &r).await
    }

    /// Inline result or the stored one.
    pub(crate) async fn load_result(&self, id: &str, r: &Row<'_>) -> Result<Option<Json>> {
        if let Some(key) = r.opt_text("result_ref") {
            let store = self
                .store()
                .ok_or_else(|| JobError::Store("the result is stored but no result store is attached".into()))?;
            let bytes = store
                .get(key.clone())
                .await
                .map_err(JobError::Store)?
                .ok_or_else(|| JobError::Store(format!("stored result of job {id} is missing ({key})")))?;
            return serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| JobError::Internal(format!("stored result of job {id} is not JSON: {e}")));
        }
        r.opt_json("result")
    }

    /// Counts per state and queue, of the jobs the caller may see.
    pub async fn stats(&self, ctx: &AccessContext) -> Result<JobStats> {
        self.allow(ctx, Action::ReadJobs)?;
        let mut params = Vec::new();
        let scope = self.access().scope(ctx).sql("", true, &mut params);
        let rows = self
            .reader("jobs.stats")
            .fetch_all(&Statement::new(
                format!(
                    "SELECT queue, status, COUNT(*) AS n FROM _forgebase_jobs WHERE {scope} \
                     GROUP BY queue, status ORDER BY queue"
                ),
                params,
            ))
            .await?;
        let mut totals: BTreeMap<String, i64> = ["queued", "running", "succeeded", "dead", "cancelled"]
            .iter()
            .map(|s| ((*s).to_owned(), 0))
            .collect();
        let mut queues: Vec<QueueStats> = Vec::new();
        for i in 0..rows.len() {
            let r = Row::new(&rows, i);
            let (queue, status, n) = (r.text("queue"), r.text("status"), r.int("n"));
            if queues.last().is_none_or(|q| q.queue != queue) {
                queues.push(QueueStats {
                    queue,
                    ..QueueStats::default()
                });
            }
            let q = queues.last_mut().ok_or_else(|| JobError::Internal("no queue".into()))?;
            match status.as_str() {
                "queued" => q.queued = n,
                "running" => q.running = n,
                "succeeded" => q.succeeded = n,
                "dead" => q.dead = n,
                "cancelled" => q.cancelled = n,
                other => return Err(JobError::Internal(format!("unknown job status {other:?}"))),
            }
            *totals.entry(status).or_insert(0) += n;
        }
        Ok(JobStats { totals, queues })
    }

    // ----------------------------------------------------------- management

    /// Cancels a job. A queued job is cancelled at once. A running job gets
    /// `cancel_requested`: an in-process handler is told at once (its cancel
    /// token fires), an external worker learns it from its next heartbeat;
    /// either should stop and report `fail`; the job then ends `cancelled`.
    /// Unfinished child jobs are cancelled too, and jobs that depend on a
    /// cancelled job are cancelled with it.
    pub async fn cancel(&self, ctx: &AccessContext, id: &str) -> Result<Job> {
        self.allow(ctx, Action::ManageJobs)?;
        self.visible(ctx, id).await?;
        let now = self.now();
        let mut fx = self.fx();
        let mut tx = self.begin().await?;
        let r = async {
            cancel_tx(&mut *tx, id, now, &mut fx, true).await?;
            fetch_job_tx(&mut *tx, id).await
        }
        .await;
        self.commit(tx, r, fx).await
    }

    /// Moves a dead or cancelled job back to the queue with a fresh attempt
    /// count. Its checkpoint is kept, so the next attempt resumes from it.
    pub async fn retry(&self, ctx: &AccessContext, id: &str) -> Result<Job> {
        self.allow(ctx, Action::ManageJobs)?;
        self.visible(ctx, id).await?;
        let mut fx = self.fx();
        let mut tx = self.begin().await?;
        let r = retry_tx(&mut *tx, id, self.now(), &mut fx).await;
        self.commit(tx, r, fx).await
    }

    /// Deletes a finished job and its history (and its stored result).
    pub async fn delete(&self, ctx: &AccessContext, id: &str) -> Result<()> {
        self.allow(ctx, Action::ManageJobs)?;
        self.visible(ctx, id).await?;
        let mut fx = self.fx();
        let mut tx = self.begin().await?;
        let r = delete_tx(&mut *tx, id, &mut fx).await;
        self.commit(tx, r, fx).await
    }

    // --------------------------------------------------------------- workers

    /// Registers (or updates) a worker and returns it. A worker belongs to
    /// the tenant and principal that registered it first (FB-146); others
    /// may not take its id over (`409 worker_id_taken`), except privileged
    /// callers and, in the same tenant, holders of `engine:read_jobs:all`.
    pub async fn register_worker(&self, ctx: &AccessContext, new: NewWorker) -> Result<Worker> {
        self.allow(ctx, Action::WorkJobs)?;
        let id = match new.id {
            Some(id) => {
                check_name("worker id", &id, 128)?;
                id
            }
            None => format!("w-{}", uuid::Uuid::new_v4().simple()),
        };
        for q in &new.queues {
            check_name("queue", q, 64)?;
        }
        for k in &new.kinds {
            check_name("kind", k, 128)?;
        }
        if let Some(name) = &new.name
            && name.len() > 128
        {
            return Err(JobError::invalid(
                "invalid_worker",
                "name must be at most 128 characters",
            ));
        }
        let concurrency = new.concurrency.unwrap_or(1);
        if !(1..=10_000).contains(&concurrency) {
            return Err(JobError::invalid(
                "invalid_worker",
                "concurrency must be between 1 and 10000",
            ));
        }
        let now = self.now();
        let scope = self.access().scope(ctx);
        let mut tx = self.begin().await?;
        let r = async {
            if worker_owner_tx(&mut *tx, &id).await?.is_some_and(|o| !scope.sees(&o, None)) {
                return Err(JobError::state(
                    "worker_id_taken",
                    format!("worker id {id:?} belongs to another tenant or user"),
                ));
            }
            // The owner columns keep their first value on an update.
            tx.execute(&Statement::new(
                "INSERT INTO _forgebase_job_workers (id, name, queues, kinds, concurrency, registered_at, last_seen_at, \
                 tenant, created_by) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, ?7, ?8) \
                 ON CONFLICT (id) DO UPDATE SET name = excluded.name, queues = excluded.queues, \
                 kinds = excluded.kinds, concurrency = excluded.concurrency, last_seen_at = excluded.last_seen_at",
                vec![
                    id.as_str().into(),
                    new.name.clone().into(),
                    serde_json::to_string(&new.queues).unwrap_or_else(|_| "[]".into()).into(),
                    serde_json::to_string(&new.kinds).unwrap_or_else(|_| "[]".into()).into(),
                    concurrency.into(),
                    now.into(),
                    ctx.tenant.clone().into(),
                    ctx.actor().into(),
                ],
            ))
            .await?;
            Ok::<_, JobError>(())
        }
        .await;
        finish(tx, r).await?;
        self.worker(&id).await
    }

    async fn worker(&self, id: &str) -> Result<Worker> {
        let rows = self
            .backend()
            .fetch_all(&Statement::new(
                "SELECT id, name, queues, kinds, concurrency, registered_at, last_seen_at, tenant, created_by \
                 FROM _forgebase_job_workers WHERE id = ?1",
                vec![id.into()],
            ))
            .await?;
        if rows.is_empty() {
            return Err(JobError::Internal(format!("worker {id} vanished")));
        }
        Ok(worker_from(&Row::new(&rows, 0), self.now()))
    }

    /// Workers the caller may see (FB-146: of its tenant, and its own unless
    /// it holds `engine:read_jobs:all`), most recently seen first.
    pub async fn workers(&self, ctx: &AccessContext) -> Result<Vec<Worker>> {
        self.allow(ctx, Action::ReadJobs)?;
        let mut params = Vec::new();
        let cond = self.access().scope(ctx).sql("", false, &mut params);
        let rows = self
            .backend()
            .fetch_all(&Statement::new(
                format!(
                    "SELECT id, name, queues, kinds, concurrency, registered_at, last_seen_at, tenant, created_by \
                     FROM _forgebase_job_workers WHERE {cond} ORDER BY last_seen_at DESC, id LIMIT 500"
                ),
                params,
            ))
            .await?;
        let now = self.now();
        Ok((0..rows.len()).map(|i| worker_from(&Row::new(&rows, i), now)).collect())
    }

    /// Claims up to `req.max` due jobs, highest priority first, then oldest
    /// due time. Each returned job is `running` with a fresh lease that
    /// expires after the job's visibility timeout unless the worker sends
    /// heartbeats. Expired leases and timed-out attempts of other workers are
    /// dealt with first. Paused queues, queues at their concurrency limit or
    /// rate limit, and jobs still waiting for parents are skipped.
    pub async fn claim(&self, ctx: &AccessContext, req: ClaimRequest) -> Result<Vec<ClaimedJob>> {
        self.allow(ctx, Action::WorkJobs)?;
        check_name("worker_id", &req.worker_id, 128)?;
        for q in &req.queues {
            check_name("queue", q, 64)?;
        }
        for k in &req.kinds {
            check_name("kind", k, 128)?;
        }
        let max = req.max.unwrap_or(1);
        if !(1..=MAX_CLAIM).contains(&max) {
            return Err(JobError::invalid(
                "invalid_claim",
                format!("max must be between 1 and {MAX_CLAIM}"),
            ));
        }
        let now = self.now();
        let limit = self.worker_limit(ctx);
        let toucher = self.toucher(ctx);
        // Cheap read first: an idle poll must not take the single writer.
        if !self.has_claimable(&req, limit.as_ref(), now).await? {
            self.touch_worker(&req.worker_id, &toucher, now).await?;
            return Ok(Vec::new());
        }
        let mut fx = self.fx();
        let mut tx = self.begin().await?;
        let r = claim_tx(&mut *tx, &req, limit.as_ref(), &toucher, max, now, &mut fx).await;
        self.commit(tx, r, fx).await
    }

    /// What a non-privileged worker may claim (FB-142): jobs of its own
    /// tenant (none: jobs without a tenant) whose kind is public.
    fn worker_limit(&self, ctx: &AccessContext) -> Option<WorkerLimit> {
        if ctx.is_privileged() {
            return None;
        }
        let access = self.access();
        Some(WorkerLimit {
            tenant: ctx.tenant.clone(),
            excluded_kinds: access.unclaimable(),
            public_with_prefix: access.public_with_prefix(),
        })
    }

    async fn has_claimable(&self, req: &ClaimRequest, limit: Option<&WorkerLimit>, now: i64) -> Result<bool> {
        let blocked = self.backend().fetch_all(&blocked_queues_stmt(now)).await?;
        let blocked: Vec<String> = (0..blocked.len()).map(|i| Row::new(&blocked, i).text("name")).collect();
        let (filter, mut params) = claim_filter(&req.queues, &req.kinds, &blocked, limit, 1);
        params.insert(0, now.into());
        let sql = format!(
            "SELECT \
               EXISTS (SELECT 1 FROM _forgebase_jobs WHERE status = 'running' AND visibility_until <= ?1) \
               OR EXISTS (SELECT 1 FROM _forgebase_jobs WHERE status = 'running' AND deadline_at <= ?1) \
               OR EXISTS (SELECT 1 FROM _forgebase_jobs WHERE status = 'queued' AND waiting_for = 0 \
                          AND run_at <= ?1 {filter})"
        );
        Ok(self
            .backend()
            .fetch_all(&Statement::new(sql, params))
            .await?
            .scalar()
            .and_then(Value::as_i64)
            == Some(1))
    }

    fn toucher(&self, ctx: &AccessContext) -> Toucher {
        Toucher {
            scope: self.access().scope(ctx),
            owner: Owner {
                tenant: ctx.tenant.clone(),
                created_by: ctx.actor(),
            },
        }
    }

    async fn touch_worker(&self, id: &str, toucher: &Toucher, now: i64) -> Result<()> {
        let mut tx = self.begin().await?;
        let r = touch_worker_tx(&mut *tx, id, toucher, now).await;
        finish(tx, r).await
    }

    /// Extends the lease and tells the worker whether the job was cancelled.
    pub async fn heartbeat(&self, ctx: &AccessContext, id: &str, lease_id: &str) -> Result<Heartbeat> {
        self.allow(ctx, Action::WorkJobs)?;
        let now = self.now();
        let mut tx = self.begin().await?;
        let r = heartbeat_tx(&mut *tx, id, lease_id, &self.toucher(ctx), now).await;
        finish(tx, r).await
    }

    /// Reports progress of a running job (percent, stage, message; fields
    /// left out keep their value). Stored on the job, published live on
    /// `jobs:<id>`, and counts as a heartbeat.
    pub async fn progress(
        &self,
        ctx: &AccessContext,
        id: &str,
        lease_id: &str,
        report: ProgressReport,
    ) -> Result<Heartbeat> {
        self.allow(ctx, Action::WorkJobs)?;
        report.validate()?;
        let now = self.now();
        let mut fx = self.fx();
        let mut tx = self.begin().await?;
        let r = async {
            let hb = heartbeat_tx(&mut *tx, id, lease_id, &self.toucher(ctx), now).await?;
            tx.execute(&Statement::new(
                "UPDATE _forgebase_jobs SET progress_percent = COALESCE(?2, progress_percent), \
                 progress_stage = COALESCE(?3, progress_stage), progress_message = COALESCE(?4, progress_message), \
                 progress_at = ?5 WHERE id = ?1",
                vec![
                    id.into(),
                    report.percent.into(),
                    report.stage.clone().into(),
                    report.message.clone().into(),
                    now.into(),
                ],
            ))
            .await?;
            fx.job(id, "progress");
            Ok(hb)
        }
        .await;
        self.commit(tx, r, fx).await
    }

    /// Saves the checkpoint of a running job (JSON, at most
    /// [`MAX_CHECKPOINT_BYTES`]). The next attempt (after a crash, timeout,
    /// retry or restart) gets it. Counts as a heartbeat.
    pub async fn save_checkpoint(
        &self,
        ctx: &AccessContext,
        id: &str,
        lease_id: &str,
        checkpoint: &Json,
    ) -> Result<Heartbeat> {
        self.allow(ctx, Action::WorkJobs)?;
        let text =
            serde_json::to_string(checkpoint).map_err(|e| JobError::invalid("invalid_checkpoint", e.to_string()))?;
        if text.len() > MAX_CHECKPOINT_BYTES {
            return Err(JobError::invalid(
                "checkpoint_too_large",
                format!("checkpoint is larger than {MAX_CHECKPOINT_BYTES} bytes"),
            ));
        }
        let now = self.now();
        let mut fx = self.fx();
        let mut tx = self.begin().await?;
        let r = async {
            let hb = heartbeat_tx(&mut *tx, id, lease_id, &self.toucher(ctx), now).await?;
            tx.execute(&Statement::new(
                "UPDATE _forgebase_jobs SET checkpoint = ?2, checkpoint_at = ?3 WHERE id = ?1",
                vec![id.into(), text.as_str().into(), now.into()],
            ))
            .await?;
            fx.job(id, "progress");
            Ok(hb)
        }
        .await;
        self.commit(tx, r, fx).await
    }

    /// Enqueues child jobs of a running job (the lease proves the caller runs
    /// it). Children join the parent's workflow, so the workflow finishes
    /// only after them, and are cancelled when the parent is cancelled.
    pub async fn spawn_children(
        &self,
        ctx: &AccessContext,
        id: &str,
        lease_id: &str,
        children: Vec<NewJob>,
    ) -> Result<Vec<Job>> {
        self.allow(ctx, Action::WorkJobs)?;
        if children.is_empty() || children.len() > MAX_CHILDREN {
            return Err(JobError::invalid(
                "invalid_job",
                format!("spawn 1 to {MAX_CHILDREN} child jobs at once"),
            ));
        }
        let now = self.now();
        let mut resolved = Vec::with_capacity(children.len());
        let access = self.access();
        for c in &children {
            resolved.push(c.resolve(now)?);
            access.admit(ctx, &c.kind, &c.payload)?;
        }
        let mut fx = self.fx();
        let mut tx = self.begin().await?;
        let r = async {
            let parent = leased(&mut *tx, id, lease_id).await?;
            // FB-142: children belong to the parent's tenant.
            let origin = Origin::child(ctx, &parent);
            let mut out = Vec::with_capacity(resolved.len());
            for mut r in resolved {
                r.parent_id = Some(parent.id.clone());
                r.workflow_id.clone_from(&parent.workflow_id);
                out.push(insert_job(&mut *tx, &r, None, &origin, now, &mut fx).await?.0);
            }
            Ok(out)
        }
        .await;
        self.commit(tx, r, fx).await
    }

    /// Marks a running job succeeded. Fails with `lease_lost` if the lease is
    /// not the job's current one (expired and re-claimed, or already finished),
    /// which is what makes completion happen once. A result larger than
    /// [`MAX_INLINE_RESULT_BYTES`] is kept in the result store (up to
    /// [`MAX_RESULT_BYTES`]).
    pub async fn complete(&self, ctx: &AccessContext, id: &str, lease_id: &str, result: Option<Json>) -> Result<Job> {
        self.allow(ctx, Action::WorkJobs)?;
        let text = match &result {
            Some(v) => Some(serde_json::to_string(v).map_err(|e| JobError::invalid("invalid_job", e.to_string()))?),
            None => None,
        };
        let (inline, stored) = match text {
            Some(t) if t.len() > MAX_INLINE_RESULT_BYTES => {
                let Some(store) = self.store() else {
                    return Err(JobError::invalid(
                        "payload_too_large",
                        format!("payload is larger than {MAX_INLINE_RESULT_BYTES} bytes"),
                    ));
                };
                if t.len() > MAX_RESULT_BYTES {
                    return Err(JobError::invalid(
                        "payload_too_large",
                        format!("result is larger than {MAX_RESULT_BYTES} bytes"),
                    ));
                }
                // Upload outside the transaction, under a key of this lease,
                // after checking the lease (a stale worker writes nothing).
                self.check_lease(id, lease_id).await?;
                let key = format!("jobs/{id}/{lease_id}.json");
                let size = i64::try_from(t.len()).unwrap_or(i64::MAX);
                store.put(key.clone(), t.into_bytes()).await.map_err(JobError::Store)?;
                (None, Some((key, size)))
            }
            other => (other, None),
        };
        let now = self.now();
        let mut fx = self.fx();
        let mut tx = self.begin().await?;
        let r = complete_tx(&mut *tx, id, lease_id, inline, stored.clone(), now, &mut fx).await;
        if r.is_err()
            && let (Some((key, _)), Some(store)) = (&stored, self.store())
        {
            let _ = store.delete(key.clone()).await;
        }
        self.commit(tx, r, fx).await
    }

    async fn check_lease(&self, id: &str, lease: &str) -> Result<()> {
        let rows = self
            .backend()
            .fetch_all(&Statement::new(
                "SELECT status, lease_id FROM _forgebase_jobs WHERE id = ?1",
                vec![id.into()],
            ))
            .await?;
        if rows.is_empty() {
            return Err(JobError::NotFound(id.to_owned()));
        }
        let r = Row::new(&rows, 0);
        if r.text("status") == "running" && r.opt_text("lease_id").as_deref() == Some(lease) {
            Ok(())
        } else {
            Err(JobError::LeaseLost(id.to_owned()))
        }
    }

    /// Reports a failed attempt. A retryable failure re-queues the job after an
    /// exponential backoff with jitter while attempts remain; otherwise the job
    /// goes to the dead-letter queue (`dead`). A job whose cancellation was
    /// requested ends `cancelled`.
    pub async fn fail(
        &self,
        ctx: &AccessContext,
        id: &str,
        lease_id: &str,
        error: &str,
        retryable: bool,
    ) -> Result<Job> {
        self.allow(ctx, Action::WorkJobs)?;
        let mut fx = self.fx();
        let mut tx = self.begin().await?;
        let r = fail_tx(&mut *tx, id, lease_id, error, retryable, self.now(), &mut fx).await;
        self.commit(tx, r, fx).await
    }

    /// Gives a job back without counting the attempt (graceful shutdown).
    pub async fn release(&self, ctx: &AccessContext, id: &str, lease_id: &str) -> Result<Job> {
        self.allow(ctx, Action::WorkJobs)?;
        let mut fx = self.fx();
        let mut tx = self.begin().await?;
        let r = release_tx(&mut *tx, id, lease_id, self.now(), &mut fx).await;
        self.commit(tx, r, fx).await
    }

    // -------------------------------------------------------------- schedules

    /// Creates a recurring job.
    pub async fn create_schedule(&self, ctx: &AccessContext, new: NewSchedule) -> Result<Schedule> {
        self.allow(ctx, Action::ManageJobs)?;
        check_name("name", &new.name, 128)?;
        check_name("kind", &new.kind, 128)?;
        let queue = new.queue.clone().unwrap_or_else(|| "default".to_owned());
        check_name("queue", &queue, 64)?;
        let cron = Cron::parse(&new.cron).map_err(|e| JobError::invalid("invalid_cron", e.to_string()))?;
        let policy = Policy::resolve(
            new.priority,
            new.max_attempts,
            new.backoff_base_ms,
            new.backoff_max_ms,
            new.visibility_timeout_ms,
        )?;
        let payload = check_payload(&new.payload)?;
        self.access().admit(ctx, &new.kind, &new.payload)?;
        let origin = Origin::of(ctx);
        let now = self.now();
        let next = cron
            .next_after(now)
            .ok_or_else(|| JobError::invalid("invalid_cron", "the expression never fires"))?;
        let id = format!("sch-{}", uuid::Uuid::new_v4().simple());
        let mut tx = self.begin().await?;
        let r = async {
            let exists = tx
                .fetch_all(&Statement::new(
                    "SELECT 1 FROM _forgebase_job_schedules WHERE name = ?1",
                    vec![new.name.as_str().into()],
                ))
                .await?;
            if !exists.is_empty() {
                return Err(JobError::ScheduleExists(new.name.clone()));
            }
            tx.execute(&Statement::new(
                "INSERT INTO _forgebase_job_schedules (id, name, cron, queue, kind, payload, priority, max_attempts, \
                 backoff_base_ms, backoff_max_ms, visibility_timeout_ms, enabled, next_run_at, created_by, created_at, \
                 updated_at, tenant, context) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?15, ?16, ?17)",
                vec![
                    id.as_str().into(),
                    new.name.as_str().into(),
                    new.cron.trim().into(),
                    queue.as_str().into(),
                    new.kind.as_str().into(),
                    payload.into(),
                    policy.priority.into(),
                    policy.max_attempts.into(),
                    policy.backoff_base_ms.into(),
                    policy.backoff_max_ms.into(),
                    policy.visibility_timeout_ms.into(),
                    i64::from(new.enabled.unwrap_or(true)).into(),
                    next.into(),
                    origin.actor.as_str().into(),
                    now.into(),
                    origin.tenant.clone().into(),
                    origin.context.clone().into(),
                ],
            ))
            .await?;
            fetch_schedule_tx(&mut *tx, &id).await
        }
        .await;
        let s = finish(tx, r).await?;
        self.pgcron_after_write(&s.id).await;
        self.schedule_fresh(&s).await
    }

    /// Changes a schedule. A new cron expression recomputes the next firing.
    pub async fn update_schedule(&self, ctx: &AccessContext, id: &str, patch: SchedulePatch) -> Result<Schedule> {
        self.allow(ctx, Action::ManageJobs)?;
        self.schedule_visible(ctx, id).await?;
        let now = self.now();
        let access = self.access();
        let mut tx = self.begin().await?;
        let r = async {
            let mut s = fetch_schedule_tx(&mut *tx, id).await?;
            // FB-142: a new payload is the updater's: admitted for it and run
            // with its context.
            let mut context = None;
            if let Some(p) = &patch.payload {
                access.admit(ctx, &s.kind, p)?;
                context = Some(RecordedContext::encode(ctx));
            }
            if let Some(expr) = &patch.cron {
                let cron = Cron::parse(expr).map_err(|e| JobError::invalid("invalid_cron", e.to_string()))?;
                let next = cron
                    .next_after(now)
                    .ok_or_else(|| JobError::invalid("invalid_cron", "the expression never fires"))?;
                s.cron = expr.trim().to_owned();
                s.next_run_at = iso(next);
            }
            if let Some(enabled) = patch.enabled {
                s.enabled = enabled;
                if enabled && patch.cron.is_none() {
                    // Re-enabling must not fire for the time it was off.
                    let cron = Cron::parse(&s.cron).map_err(|e| JobError::Internal(e.to_string()))?;
                    s.next_run_at = iso(cron.next_after(now).unwrap_or(now));
                }
            }
            if let Some(p) = &patch.payload {
                s.payload = p.clone();
            }
            if let Some(p) = patch.priority {
                s.priority = Policy::resolve(Some(p), None, None, None, None)?.priority;
            }
            if let Some(c) = context {
                tx.execute(&Statement::new(
                    "UPDATE _forgebase_job_schedules SET context = ?2 WHERE id = ?1",
                    vec![id.into(), c.into()],
                ))
                .await?;
            }
            tx.execute(&Statement::new(
                "UPDATE _forgebase_job_schedules SET cron = ?2, enabled = ?3, payload = ?4, priority = ?5, \
                 next_run_at = ?6, updated_at = ?7 WHERE id = ?1",
                vec![
                    id.into(),
                    s.cron.as_str().into(),
                    i64::from(s.enabled).into(),
                    check_payload(&s.payload)?.into(),
                    s.priority.into(),
                    crate::model::parse_rfc3339_ms(&s.next_run_at)?.into(),
                    now.into(),
                ],
            ))
            .await?;
            fetch_schedule_tx(&mut *tx, id).await
        }
        .await;
        let s = finish(tx, r).await?;
        self.pgcron_after_write(id).await;
        self.schedule_fresh(&s).await
    }

    /// Deletes a schedule (jobs it already enqueued stay).
    pub async fn delete_schedule(&self, ctx: &AccessContext, id: &str) -> Result<()> {
        self.allow(ctx, Action::ManageJobs)?;
        self.schedule_visible(ctx, id).await?;
        let mut tx = self.begin().await?;
        let r = async {
            let n = tx
                .execute(&Statement::new(
                    "DELETE FROM _forgebase_job_schedules WHERE id = ?1",
                    vec![id.into()],
                ))
                .await?
                .rows_affected;
            if n == 0 {
                return Err(JobError::ScheduleNotFound(id.to_owned()));
            }
            Ok(())
        }
        .await;
        finish(tx, r).await?;
        self.pgcron_after_write(id).await;
        Ok(())
    }

    /// One schedule.
    pub async fn schedule(&self, ctx: &AccessContext, id: &str) -> Result<Schedule> {
        self.allow(ctx, Action::ReadJobs)?;
        self.schedule_visible(ctx, id).await?;
        let rows = self
            .backend()
            .fetch_all(&Statement::new(
                format!("SELECT {SCHEDULE_COLS} FROM _forgebase_job_schedules WHERE id = ?1"),
                vec![id.into()],
            ))
            .await?;
        if rows.is_empty() {
            return Err(JobError::ScheduleNotFound(id.to_owned()));
        }
        schedule_from(&Row::new(&rows, 0))
    }

    /// The schedules the caller may see, by name.
    pub async fn schedules(&self, ctx: &AccessContext) -> Result<Vec<Schedule>> {
        self.allow(ctx, Action::ReadJobs)?;
        let mut params = Vec::new();
        let scope = self.access().scope(ctx).sql("", false, &mut params);
        let rows = self
            .backend()
            .fetch_all(&Statement::new(
                format!("SELECT {SCHEDULE_COLS} FROM _forgebase_job_schedules WHERE {scope} ORDER BY name LIMIT 1000"),
                params,
            ))
            .await?;
        (0..rows.len()).map(|i| schedule_from(&Row::new(&rows, i))).collect()
    }

    /// Fails with `schedule not found` unless `ctx` may see schedule `id`.
    async fn schedule_visible(&self, ctx: &AccessContext, id: &str) -> Result<()> {
        let scope = self.access().scope(ctx);
        if scope == Scope::All {
            return Ok(());
        }
        let rows = self
            .backend()
            .fetch_all(&Statement::new(
                "SELECT tenant, created_by FROM _forgebase_job_schedules WHERE id = ?1",
                vec![id.into()],
            ))
            .await?;
        let visible = !rows.is_empty() && {
            let r = Row::new(&rows, 0);
            scope.sees(
                &Owner {
                    tenant: r.opt_text("tenant"),
                    created_by: r.text("created_by"),
                },
                None,
            )
        };
        if visible {
            Ok(())
        } else {
            Err(JobError::ScheduleNotFound(id.to_owned()))
        }
    }

    // ------------------------------------------------------------ maintenance

    /// Enqueues one job for every enabled schedule that is due and moves its
    /// next firing past now. Runs in one transaction; the job's idempotency
    /// key `schedule:<id>:<due time>` makes a firing happen once even if two
    /// processes race. Firings missed while the server was down collapse into
    /// one. Returns the number of jobs enqueued.
    pub async fn fire_due_schedules(&self, ctx: &AccessContext) -> Result<usize> {
        self.allow(ctx, Action::ManageJobs)?;
        let now = self.now();
        let due = self
            .backend()
            .fetch_all(&Statement::new(
                "SELECT 1 FROM _forgebase_job_schedules WHERE enabled = 1 AND next_run_at <= ?1 LIMIT 1",
                vec![now.into()],
            ))
            .await?;
        if due.is_empty() {
            return Ok(0);
        }
        let mut fx = self.fx();
        let mut tx = self.begin().await?;
        let r = fire_tx(&mut *tx, now, &mut fx).await;
        self.commit(tx, r, fx).await
    }

    /// Re-queues (or dead-letters) running jobs whose lease expired, and
    /// retries (or dead-letters) attempts that ran past their timeout. Claims
    /// do this too; this is for idle systems. Returns the number of jobs
    /// touched.
    pub async fn reap_expired(&self, ctx: &AccessContext) -> Result<usize> {
        self.allow(ctx, Action::ManageJobs)?;
        let now = self.now();
        let expired = self
            .backend()
            .fetch_all(&Statement::new(
                "SELECT 1 FROM _forgebase_jobs WHERE status = 'running' AND visibility_until <= ?1 \
                 UNION ALL SELECT 1 FROM _forgebase_jobs WHERE status = 'running' AND deadline_at <= ?1 LIMIT 1",
                vec![now.into()],
            ))
            .await?;
        if expired.is_empty() {
            return Ok(0);
        }
        let mut fx = self.fx();
        let mut tx = self.begin().await?;
        let r = reap_tx(&mut *tx, now, &mut fx).await;
        self.commit(tx, r, fx).await
    }

    /// Deletes succeeded and cancelled jobs that finished before
    /// `older_than_ms` (epoch ms), with their history and stored results,
    /// and workflows that ended `succeeded`, `compensated` or `cancelled`
    /// before then. The dead-letter queue and failed workflows are never
    /// purged automatically. Returns the number of jobs deleted.
    pub async fn purge_finished(&self, ctx: &AccessContext, older_than_ms: i64) -> Result<u64> {
        self.allow(ctx, Action::ManageJobs)?;
        let mut fx = self.fx();
        let mut tx = self.begin().await?;
        let r = async {
            let refs = tx
                .fetch_all(&Statement::new(
                    "SELECT result_ref FROM _forgebase_jobs WHERE status IN ('succeeded', 'cancelled') \
                     AND finished_at < ?1 AND result_ref IS NOT NULL",
                    vec![older_than_ms.into()],
                ))
                .await?;
            for i in 0..refs.len() {
                fx.drop_objects.push(Row::new(&refs, i).text("result_ref"));
            }
            let n = tx
                .execute(&Statement::new(
                    "DELETE FROM _forgebase_jobs WHERE status IN ('succeeded', 'cancelled') AND finished_at < ?1",
                    vec![older_than_ms.into()],
                ))
                .await?
                .rows_affected;
            tx.execute(&Statement::new(
                "DELETE FROM _forgebase_job_workflows WHERE status IN ('succeeded', 'compensated', 'cancelled') \
                 AND finished_at < ?1",
                vec![older_than_ms.into()],
            ))
            .await?;
            Ok::<_, JobError>(n)
        }
        .await;
        self.commit(tx, r, fx).await
    }
}

/// Commits on success, rolls back on error.
pub(crate) async fn finish<T>(tx: Box<dyn WriteTx>, result: Result<T>) -> Result<T> {
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

// ---------------------------------------------------------------------------
// Rows

pub(crate) struct Row<'a> {
    rows: &'a Rows,
    i: usize,
}

impl<'a> Row<'a> {
    pub(crate) fn new(rows: &'a Rows, i: usize) -> Self {
        Self { rows, i }
    }

    pub(crate) fn value(&self, col: &str) -> &Value {
        static NULL: Value = Value::Null;
        self.rows
            .column(col)
            .and_then(|c| self.rows.rows.get(self.i).and_then(|r| r.get(c)))
            .unwrap_or(&NULL)
    }

    pub(crate) fn text(&self, col: &str) -> String {
        self.value(col).as_str().unwrap_or_default().to_owned()
    }

    pub(crate) fn opt_text(&self, col: &str) -> Option<String> {
        self.value(col).as_str().map(str::to_owned)
    }

    pub(crate) fn int(&self, col: &str) -> i64 {
        self.value(col).as_i64().unwrap_or(0)
    }

    pub(crate) fn opt_int(&self, col: &str) -> Option<i64> {
        self.value(col).as_i64()
    }

    fn opt_float(&self, col: &str) -> Option<f64> {
        match self.value(col) {
            Value::Real(f) => Some(*f),
            Value::Integer(i) =>
            {
                #[allow(clippy::cast_precision_loss)]
                Some(*i as f64)
            }
            _ => None,
        }
    }

    pub(crate) fn json(&self, col: &str) -> Result<Json> {
        serde_json::from_str(self.value(col).as_str().unwrap_or("null"))
            .map_err(|e| JobError::Internal(format!("column {col} is not JSON: {e}")))
    }

    pub(crate) fn opt_json(&self, col: &str) -> Result<Option<Json>> {
        if self.value(col).is_null() {
            Ok(None)
        } else {
            self.json(col).map(Some)
        }
    }
}

pub(crate) fn job_from(r: &Row<'_>) -> Result<Job> {
    let status = r.text("status");
    let progress = r.opt_int("progress_at").map(|at| Progress {
        percent: r.opt_float("progress_percent"),
        stage: r.opt_text("progress_stage"),
        message: r.opt_text("progress_message"),
        updated_at: iso(at),
    });
    Ok(Job {
        id: r.text("id"),
        queue: r.text("queue"),
        kind: r.text("kind"),
        payload: r.json("payload")?,
        priority: r.int("priority"),
        status: JobStatus::parse(&status)
            .ok_or_else(|| JobError::Internal(format!("unknown job status {status:?}")))?,
        run_at: iso(r.int("run_at")),
        attempts: r.int("attempts"),
        max_attempts: r.int("max_attempts"),
        backoff_base_ms: r.int("backoff_base_ms"),
        backoff_max_ms: r.int("backoff_max_ms"),
        visibility_timeout_ms: r.int("visibility_timeout_ms"),
        idempotency_key: r.opt_text("idempotency_key"),
        schedule_id: r.opt_text("schedule_id"),
        worker_id: r.opt_text("worker_id"),
        visibility_until: r.opt_int("visibility_until").map(iso),
        cancel_requested: r.int("cancel_requested") == 1,
        result: r.opt_json("result")?,
        last_error: r.opt_text("last_error"),
        created_by: r.text("created_by"),
        tenant: r.opt_text("tenant"),
        context: r.opt_text("context"),
        created_at: iso(r.int("created_at")),
        updated_at: iso(r.int("updated_at")),
        started_at: r.opt_int("started_at").map(iso),
        finished_at: r.opt_int("finished_at").map(iso),
        progress,
        has_checkpoint: r.int("has_checkpoint") == 1,
        checkpoint_at: r.opt_int("checkpoint_at").map(iso),
        timeout_ms: r.opt_int("timeout_ms"),
        deadline_at: r.opt_int("deadline_at").map(iso),
        heartbeat_at: r.opt_int("heartbeat_at").map(iso),
        workflow_id: r.opt_text("workflow_id"),
        step: r.opt_text("step"),
        parent_id: r.opt_text("parent_id"),
        compensates: r.opt_text("compensates"),
        waiting_for: r.int("waiting_for"),
        result_stored: r.opt_text("result_ref").is_some(),
        result_size: r.opt_int("result_size"),
    })
}

impl JobUpdate {
    pub(crate) fn of(job: &Job, event: &str) -> Self {
        JobUpdate {
            event: event.to_owned(),
            job_id: job.id.clone(),
            queue: job.queue.clone(),
            kind: job.kind.clone(),
            status: job.status,
            attempts: job.attempts,
            progress: job.progress.clone(),
            has_checkpoint: job.has_checkpoint,
            cancel_requested: job.cancel_requested,
            waiting_for: job.waiting_for,
            workflow_id: job.workflow_id.clone(),
            step: job.step.clone(),
            last_error: job.last_error.as_ref().map(|e| e.chars().take(500).collect()),
            updated_at: job.updated_at.clone(),
            finished_at: job.finished_at.clone(),
        }
    }
}

/// FB-165: a schedule that pg_cron fires carries [`crate::pgcron::OWNED`] in
/// `next_run_at`; its next firing is computed from the expression.
fn fired_by_of(r: &Row<'_>) -> &'static str {
    if r.int("next_run_at") == crate::pgcron::OWNED {
        "pg_cron"
    } else {
        "forgebase"
    }
}

fn next_run_at_of(r: &Row<'_>) -> String {
    let at = r.int("next_run_at");
    if at == crate::pgcron::OWNED {
        let next = Cron::parse(&r.text("cron")).ok().and_then(|c| c.next_after(now_ms()));
        return iso(next.unwrap_or(at));
    }
    iso(at)
}

pub(crate) fn schedule_from(r: &Row<'_>) -> Result<Schedule> {
    Ok(Schedule {
        id: r.text("id"),
        name: r.text("name"),
        cron: r.text("cron"),
        queue: r.text("queue"),
        kind: r.text("kind"),
        payload: r.json("payload")?,
        priority: r.int("priority"),
        max_attempts: r.int("max_attempts"),
        backoff_base_ms: r.int("backoff_base_ms"),
        backoff_max_ms: r.int("backoff_max_ms"),
        visibility_timeout_ms: r.int("visibility_timeout_ms"),
        enabled: r.int("enabled") == 1,
        next_run_at: next_run_at_of(r),
        fired_by: fired_by_of(r).to_owned(),
        last_run_at: r.opt_int("last_run_at").map(iso),
        last_job_id: r.opt_text("last_job_id"),
        created_by: r.text("created_by"),
        tenant: r.opt_text("tenant"),
        created_at: iso(r.int("created_at")),
        updated_at: iso(r.int("updated_at")),
    })
}

fn worker_from(r: &Row<'_>, now: i64) -> Worker {
    let list =
        |col: &str| serde_json::from_str::<Vec<String>>(r.value(col).as_str().unwrap_or("[]")).unwrap_or_default();
    let seen = r.int("last_seen_at");
    Worker {
        id: r.text("id"),
        name: r.opt_text("name"),
        queues: list("queues"),
        kinds: list("kinds"),
        concurrency: r.int("concurrency"),
        registered_at: iso(r.int("registered_at")),
        last_seen_at: iso(seen),
        online: now - seen <= WORKER_ONLINE_MS,
        tenant: r.opt_text("tenant"),
        created_by: r.opt_text("created_by"),
    }
}

pub(crate) async fn fetch_job(backend: &dyn Backend, id: &str) -> Result<Option<Job>> {
    let rows = backend
        .fetch_all(&Statement::new(
            format!("SELECT {JOB_COLS} FROM _forgebase_jobs WHERE id = ?1"),
            vec![id.into()],
        ))
        .await?;
    if rows.is_empty() {
        return Ok(None);
    }
    job_from(&Row::new(&rows, 0)).map(Some)
}

pub(crate) async fn fetch_job_tx(tx: &mut dyn WriteTx, id: &str) -> Result<Job> {
    let rows = tx
        .fetch_all(&Statement::new(
            format!("SELECT {JOB_COLS} FROM _forgebase_jobs WHERE id = ?1"),
            vec![id.into()],
        ))
        .await?;
    if rows.is_empty() {
        return Err(JobError::NotFound(id.to_owned()));
    }
    job_from(&Row::new(&rows, 0))
}

async fn fetch_schedule_tx(tx: &mut dyn WriteTx, id: &str) -> Result<Schedule> {
    let rows = tx
        .fetch_all(&Statement::new(
            format!("SELECT {SCHEDULE_COLS} FROM _forgebase_job_schedules WHERE id = ?1"),
            vec![id.into()],
        ))
        .await?;
    if rows.is_empty() {
        return Err(JobError::ScheduleNotFound(id.to_owned()));
    }
    schedule_from(&Row::new(&rows, 0))
}

pub(crate) async fn event(
    tx: &mut dyn WriteTx,
    job_id: &str,
    now: i64,
    event: &str,
    attempt: i64,
    worker: Option<&str>,
    message: Option<&str>,
) -> Result<()> {
    tx.execute(&Statement::new(
        "INSERT INTO _forgebase_job_events (job_id, at, event, attempt, worker_id, message) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        vec![
            job_id.into(),
            now.into(),
            event.into(),
            attempt.into(),
            worker.map(str::to_owned).into(),
            message.map(|m| m.chars().take(2000).collect::<String>()).into(),
        ],
    ))
    .await?;
    Ok(())
}

/// Closes the open attempt row of a job.
pub(crate) async fn close_attempt(
    tx: &mut dyn WriteTx,
    job_id: &str,
    now: i64,
    outcome: &str,
    error: Option<&str>,
) -> Result<()> {
    tx.execute(&Statement::new(
        "UPDATE _forgebase_job_attempts SET finished_at = ?2, outcome = ?3, error = ?4 \
         WHERE job_id = ?1 AND finished_at IS NULL",
        vec![
            job_id.into(),
            now.into(),
            outcome.into(),
            error.map(|m| m.chars().take(2000).collect::<String>()).into(),
        ],
    ))
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Transactions

/// Inserts a job (or returns the one with its idempotency key). Applies the
/// queue's default priority and timeout, counts the parents that have not
/// succeeded yet and records the dependencies.
pub(crate) async fn insert_job(
    tx: &mut dyn WriteTx,
    r: &Resolved,
    schedule_id: Option<&str>,
    origin: &Origin,
    now: i64,
    fx: &mut Fx,
) -> Result<(Job, bool)> {
    if let Some(key) = &r.idempotency_key {
        // FB-142: keys are per tenant.
        let rows = tx
            .fetch_all(&Statement::new(
                "SELECT id FROM _forgebase_jobs WHERE queue = ?1 AND idempotency_key = ?2 \
                 AND COALESCE(tenant, '') = ?3",
                vec![
                    r.queue.as_str().into(),
                    key.as_str().into(),
                    origin.tenant.clone().unwrap_or_default().into(),
                ],
            ))
            .await?;
        if !rows.is_empty() {
            let id = Row::new(&rows, 0).text("id");
            return Ok((fetch_job_tx(tx, &id).await?, false));
        }
    }
    let defaults = tx
        .fetch_all(&Statement::new(
            "SELECT default_priority, default_timeout_ms FROM _forgebase_job_queues WHERE name = ?1",
            vec![crate::queues::queue_key(origin.tenant.as_deref(), &r.queue).into()],
        ))
        .await?;
    let (mut priority, mut timeout_ms) = (r.policy.priority, r.timeout_ms);
    if !defaults.is_empty() {
        let d = Row::new(&defaults, 0);
        if !r.priority_given
            && let Some(p) = d.opt_int("default_priority")
        {
            priority = p;
        }
        if timeout_ms.is_none() {
            timeout_ms = d.opt_int("default_timeout_ms");
        }
    }
    let mut waiting = 0_i64;
    for parent in &r.depends_on {
        let rows = tx
            .fetch_all(&Statement::new(
                "SELECT status FROM _forgebase_jobs WHERE id = ?1",
                vec![parent.as_str().into()],
            ))
            .await?;
        if rows.is_empty() {
            return Err(JobError::invalid(
                "unknown_dependency",
                format!("depends_on: job {parent:?} does not exist"),
            ));
        }
        match Row::new(&rows, 0).text("status").as_str() {
            "succeeded" => {}
            "dead" | "cancelled" => {
                return Err(JobError::state(
                    "dependency_failed",
                    format!("depends_on: job {parent:?} did not succeed"),
                ));
            }
            _ => waiting += 1,
        }
    }
    let id = format!("job-{}", uuid::Uuid::new_v4().simple());
    tx.execute(&Statement::new(
        "INSERT INTO _forgebase_jobs (id, queue, kind, payload, priority, status, run_at, max_attempts, \
         backoff_base_ms, backoff_max_ms, visibility_timeout_ms, idempotency_key, schedule_id, created_by, \
         created_at, updated_at, timeout_ms, workflow_id, step, parent_id, compensates, compensate, waiting_for, \
         tenant, context) \
         VALUES (?1, ?2, ?3, ?4, ?5, 'queued', ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?14, ?15, ?16, ?17, ?18, \
         ?19, ?20, ?21, ?22, ?23)",
        vec![
            id.as_str().into(),
            r.queue.as_str().into(),
            r.kind.as_str().into(),
            r.payload.as_str().into(),
            priority.into(),
            r.run_at.into(),
            r.policy.max_attempts.into(),
            r.policy.backoff_base_ms.into(),
            r.policy.backoff_max_ms.into(),
            r.policy.visibility_timeout_ms.into(),
            r.idempotency_key.clone().into(),
            schedule_id.map(str::to_owned).into(),
            origin.actor.as_str().into(),
            now.into(),
            timeout_ms.into(),
            r.workflow_id.clone().into(),
            r.step.clone().into(),
            r.parent_id.clone().into(),
            r.compensates.clone().into(),
            r.compensate.clone().into(),
            waiting.into(),
            origin.tenant.clone().into(),
            origin.context.clone().into(),
        ],
    ))
    .await?;
    for parent in &r.depends_on {
        tx.execute(&Statement::new(
            "INSERT INTO _forgebase_job_deps (job_id, parent_id) VALUES (?1, ?2) ON CONFLICT DO NOTHING",
            vec![id.as_str().into(), parent.as_str().into()],
        ))
        .await?;
    }
    event(tx, &id, now, "enqueued", 0, None, None).await?;
    fx.job(&id, "status");
    if waiting == 0 {
        fx.wake = true;
    }
    Ok((fetch_job_tx(tx, &id).await?, true))
}

/// The tenant and creator of a job.
pub(crate) fn owner_of(job: &Job) -> Owner {
    Owner {
        tenant: job.tenant.clone(),
        created_by: job.created_by.clone(),
    }
}

/// Queue settings (stored names, [`crate::queues::queue_key`]) that hand out
/// nothing right now: paused, at their concurrency limit, or out of rate
/// budget in the current window.
fn blocked_queues_stmt(now: i64) -> Statement {
    Statement::new(
        format!(
            "SELECT name FROM _forgebase_job_queues q WHERE paused = 1 \
             OR (rate_limit IS NOT NULL AND window_start + rate_period_ms > ?1 AND window_count >= rate_limit) \
             OR (concurrency IS NOT NULL AND concurrency <= \
                 (SELECT COUNT(*) FROM _forgebase_jobs r WHERE r.status = 'running' AND {} = q.name))",
            crate::queues::job_queue_key("r.")
        ),
        vec![now.into()],
    )
}

/// What a non-privileged worker may claim (FB-142).
#[derive(Debug, Clone)]
pub(crate) struct WorkerLimit {
    /// Only jobs of this tenant (`None`: jobs without a tenant).
    tenant: Option<String>,
    /// Registered reserved and guarded kinds.
    excluded_kinds: Vec<String>,
    /// Public kinds that carry a reserved prefix.
    public_with_prefix: Vec<String>,
}

/// `AND queue IN (...) AND kind IN (...)`, `AND <settings name> NOT IN
/// (...)` for the `excluded` queue settings (stored names), and a worker's
/// tenant and kind limits, with placeholders numbered after `taken`.
fn claim_filter(
    queues: &[String],
    kinds: &[String],
    excluded: &[String],
    limit: Option<&WorkerLimit>,
    taken: usize,
) -> (String, Vec<Value>) {
    let mut sql = String::new();
    let mut params: Vec<Value> = Vec::new();
    if let Some(l) = limit {
        match &l.tenant {
            Some(t) => {
                params.push(t.as_str().into());
                sql.push_str(&format!(" AND tenant = ?{}", taken + params.len()));
            }
            None => sql.push_str(" AND tenant IS NULL"),
        }
        if !l.excluded_kinds.is_empty() {
            let marks: Vec<String> = l
                .excluded_kinds
                .iter()
                .map(|k| {
                    params.push(k.as_str().into());
                    format!("?{}", taken + params.len())
                })
                .collect();
            sql.push_str(&format!(" AND kind NOT IN ({})", marks.join(", ")));
        }
        // Prefixes are constants of `[a-z.]`: no LIKE wildcards to escape.
        let mut prefixes: Vec<String> = crate::access::RESERVED_PREFIXES
            .iter()
            .map(|p| format!("kind LIKE '{p}%'"))
            .collect();
        prefixes.sort();
        let public: Vec<String> = l
            .public_with_prefix
            .iter()
            .map(|k| {
                params.push(k.as_str().into());
                format!("?{}", taken + params.len())
            })
            .collect();
        if public.is_empty() {
            sql.push_str(&format!(" AND NOT ({})", prefixes.join(" OR ")));
        } else {
            sql.push_str(&format!(
                " AND (kind IN ({}) OR NOT ({}))",
                public.join(", "),
                prefixes.join(" OR ")
            ));
        }
    }
    let settings_key = crate::queues::job_queue_key("");
    for (col, list, op) in [
        ("queue", queues, "IN"),
        ("kind", kinds, "IN"),
        (settings_key.as_str(), excluded, "NOT IN"),
    ] {
        if list.is_empty() {
            continue;
        }
        let marks: Vec<String> = list
            .iter()
            .map(|v| {
                params.push(v.as_str().into());
                format!("?{}", taken + params.len())
            })
            .collect();
        sql.push_str(&format!(" AND {col} {op} ({})", marks.join(", ")));
    }
    (sql, params)
}

/// Who claims as a worker: what it may see and who it is (FB-146).
pub(crate) struct Toucher {
    scope: Scope,
    owner: Owner,
}

/// The tenant and principal that registered worker `id`, if it exists.
async fn worker_owner_tx(tx: &mut dyn WriteTx, id: &str) -> Result<Option<Owner>> {
    let rows = tx
        .fetch_all(&Statement::new(
            "SELECT tenant, created_by FROM _forgebase_job_workers WHERE id = ?1",
            vec![id.into()],
        ))
        .await?;
    Ok((!rows.is_empty()).then(|| {
        let r = Row::new(&rows, 0);
        Owner {
            tenant: r.opt_text("tenant"),
            created_by: r.opt_text("created_by").unwrap_or_default(),
        }
    }))
}

/// Records that worker `id` was seen (creating it for the claimer). The
/// worker of another tenant or user is left alone: a claim never changes a
/// row the claimer may not see.
async fn touch_worker_tx(tx: &mut dyn WriteTx, id: &str, toucher: &Toucher, now: i64) -> Result<()> {
    match worker_owner_tx(tx, id).await? {
        Some(owner) if !toucher.scope.sees(&owner, None) => {}
        Some(_) => {
            tx.execute(&Statement::new(
                "UPDATE _forgebase_job_workers SET last_seen_at = ?2 WHERE id = ?1",
                vec![id.into(), now.into()],
            ))
            .await?;
        }
        None => {
            tx.execute(&Statement::new(
                "INSERT INTO _forgebase_job_workers (id, registered_at, last_seen_at, tenant, created_by) \
                 VALUES (?1, ?2, ?2, ?3, ?4)",
                vec![
                    id.into(),
                    now.into(),
                    toucher.owner.tenant.clone().into(),
                    toucher.owner.created_by.as_str().into(),
                ],
            ))
            .await?;
        }
    }
    Ok(())
}

/// What a queue may still hand out in this claim.
#[derive(Debug, Clone, Copy)]
struct Budget {
    /// Concurrency left (`None`: unlimited).
    running_left: Option<i64>,
    /// Rate budget left in the window (`None`: unlimited).
    rate_left: Option<i64>,
    /// Rate window to store back: (start, claims), if rate-limited.
    window: Option<(i64, i64)>,
}

impl Budget {
    fn open(&self) -> bool {
        self.running_left.is_none_or(|n| n > 0) && self.rate_left.is_none_or(|n| n > 0)
    }

    fn take(&mut self) {
        if let Some(n) = &mut self.running_left {
            *n -= 1;
        }
        if let Some(n) = &mut self.rate_left {
            *n -= 1;
        }
        if let Some((_, c)) = &mut self.window {
            *c += 1;
        }
    }
}

/// The budget of the queue settings stored as `key` (none: unlimited).
async fn budget(tx: &mut dyn WriteTx, key: &str, now: i64) -> Result<Budget> {
    let rows = tx
        .fetch_all(&Statement::new(
            format!(
                "SELECT paused, concurrency, rate_limit, rate_period_ms, window_start, window_count, \
                 (SELECT COUNT(*) FROM _forgebase_jobs WHERE status = 'running' AND {} = ?1) AS running \
                 FROM _forgebase_job_queues WHERE name = ?1",
                crate::queues::job_queue_key("")
            ),
            vec![key.into()],
        ))
        .await?;
    if rows.is_empty() {
        return Ok(Budget {
            running_left: None,
            rate_left: None,
            window: None,
        });
    }
    let r = Row::new(&rows, 0);
    if r.int("paused") == 1 {
        return Ok(Budget {
            running_left: Some(0),
            rate_left: None,
            window: None,
        });
    }
    let running_left = r.opt_int("concurrency").map(|c| c - r.int("running"));
    let (rate_left, window) = match r.opt_int("rate_limit") {
        None => (None, None),
        Some(limit) => {
            let period = r.int("rate_period_ms").max(1);
            let (start, count) = if now >= r.int("window_start") + period {
                (now - now.rem_euclid(period), 0)
            } else {
                (r.int("window_start"), r.int("window_count"))
            };
            (Some(limit - count), Some((start, count)))
        }
    };
    Ok(Budget {
        running_left,
        rate_left,
        window,
    })
}

async fn claim_tx(
    tx: &mut dyn WriteTx,
    req: &ClaimRequest,
    limit: Option<&WorkerLimit>,
    toucher: &Toucher,
    max: i64,
    now: i64,
    fx: &mut Fx,
) -> Result<Vec<ClaimedJob>> {
    touch_worker_tx(tx, &req.worker_id, toucher, now).await?;
    reap_tx(tx, now, fx).await?;
    let blocked = tx.fetch_all(&blocked_queues_stmt(now)).await?;
    let mut excluded: Vec<String> = (0..blocked.len()).map(|i| Row::new(&blocked, i).text("name")).collect();
    let mut budgets: HashMap<String, Budget> = HashMap::new();
    let mut claimed = Vec::new();
    // Each round either claims something or excludes a saturated queue, so a
    // queue at its limit cannot starve the others.
    for _round in 0..16 {
        let want = max - i64::try_from(claimed.len()).unwrap_or(i64::MAX);
        if want <= 0 {
            break;
        }
        let (filter, mut params) = claim_filter(&req.queues, &req.kinds, &excluded, limit, 2);
        params.insert(0, now.into());
        params.insert(1, want.into());
        let rows = tx
            .fetch_all(&Statement::new(
                format!(
                    "SELECT id, queue, tenant FROM _forgebase_jobs WHERE status = 'queued' AND waiting_for = 0 \
                     AND run_at <= ?1 {filter} ORDER BY priority DESC, run_at, {ord} LIMIT ?2",
                    ord = fx.row_order
                ),
                params,
            ))
            .await?;
        if rows.is_empty() {
            break;
        }
        let mut skipped = false;
        for i in 0..rows.len() {
            let row = Row::new(&rows, i);
            let id = row.text("id");
            let queue = crate::queues::queue_key(row.opt_text("tenant").as_deref(), &row.text("queue"));
            if excluded.contains(&queue) {
                skipped = true;
                continue;
            }
            if !budgets.contains_key(&queue) {
                let b = budget(tx, &queue, now).await?;
                budgets.insert(queue.clone(), b);
            }
            let Some(b) = budgets.get_mut(&queue) else { continue };
            if !b.open() {
                excluded.push(queue);
                skipped = true;
                continue;
            }
            b.take();
            claimed.push(claim_one(tx, &id, &req.worker_id, now, fx).await?);
        }
        if !skipped {
            break;
        }
    }
    for (queue, b) in &budgets {
        if let Some((start, count)) = b.window {
            tx.execute(&Statement::new(
                "UPDATE _forgebase_job_queues SET window_start = ?2, window_count = ?3 WHERE name = ?1",
                vec![queue.as_str().into(), start.into(), count.into()],
            ))
            .await?;
        }
    }
    Ok(claimed)
}

async fn claim_one(tx: &mut dyn WriteTx, id: &str, worker: &str, now: i64, fx: &mut Fx) -> Result<ClaimedJob> {
    let lease = format!("lease-{}", uuid::Uuid::new_v4().simple());
    tx.execute(&Statement::new(
        "UPDATE _forgebase_jobs SET status = 'running', attempts = attempts + 1, lease_id = ?2, worker_id = ?3, \
         visibility_until = ?4 + visibility_timeout_ms, heartbeat_at = ?4, started_at = ?4, updated_at = ?4, \
         deadline_at = CASE WHEN timeout_ms IS NULL THEN NULL ELSE ?4 + timeout_ms END \
         WHERE id = ?1 AND status = 'queued'",
        vec![id.into(), lease.as_str().into(), worker.into(), now.into()],
    ))
    .await?;
    let job = fetch_job_tx(tx, id).await?;
    tx.execute(&Statement::new(
        "INSERT INTO _forgebase_job_attempts (job_id, attempt, worker_id, started_at, from_checkpoint) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        vec![
            id.into(),
            job.attempts.into(),
            worker.into(),
            now.into(),
            i64::from(job.has_checkpoint).into(),
        ],
    ))
    .await?;
    event(tx, id, now, "claimed", job.attempts, Some(worker), None).await?;
    let checkpoint = {
        let rows = tx
            .fetch_all(&Statement::new(
                "SELECT checkpoint FROM _forgebase_jobs WHERE id = ?1",
                vec![id.into()],
            ))
            .await?;
        if rows.is_empty() {
            None
        } else {
            Row::new(&rows, 0).opt_json("checkpoint")?
        }
    };
    let parents = parents_tx(tx, &job, fx.row_order).await?;
    fx.job(id, "status");
    Ok(ClaimedJob {
        lease_id: lease,
        job,
        checkpoint,
        parents,
    })
}

/// The parents of `job` with their results; a compensation job also gets
/// the step it compensates.
async fn parents_tx(tx: &mut dyn WriteTx, job: &Job, row_order: &str) -> Result<Vec<ParentResult>> {
    let rows = tx
        .fetch_all(&Statement::new(
            format!(
                "SELECT j.id, j.step, j.status, j.result, j.result_ref FROM _forgebase_jobs j \
                 WHERE j.id IN (SELECT parent_id FROM _forgebase_job_deps WHERE job_id = ?1) OR j.id = ?2 \
                 ORDER BY j.created_at, j.{row_order}"
            ),
            vec![job.id.as_str().into(), job.compensates.clone().into()],
        ))
        .await?;
    (0..rows.len())
        .map(|i| {
            let r = Row::new(&rows, i);
            let status = r.text("status");
            Ok(ParentResult {
                id: r.text("id"),
                step: r.opt_text("step"),
                status: JobStatus::parse(&status)
                    .ok_or_else(|| JobError::Internal(format!("unknown job status {status:?}")))?,
                result: r.opt_json("result")?,
                result_stored: r.opt_text("result_ref").is_some(),
            })
        })
        .collect()
}

/// Ends the leases that expired and the attempts that ran past their
/// deadline. Expired lease: cancelled if cancellation was requested,
/// dead-lettered when attempts are used up, otherwise queued again at once.
/// Timeout: like a retryable failure (backoff), or dead when attempts are
/// used up.
pub(crate) async fn reap_tx(tx: &mut dyn WriteTx, now: i64, fx: &mut Fx) -> Result<usize> {
    let rows = tx
        .fetch_all(&Statement::new(
            "SELECT id, attempts, max_attempts, cancel_requested, worker_id, timeout_ms, backoff_base_ms, \
             backoff_max_ms, deadline_at IS NOT NULL AND deadline_at <= ?1 AS timed_out FROM _forgebase_jobs \
             WHERE status = 'running' AND (visibility_until <= ?1 OR deadline_at <= ?1)",
            vec![now.into()],
        ))
        .await?;
    for i in 0..rows.len() {
        let r = Row::new(&rows, i);
        let (id, attempts, worker) = (r.text("id"), r.int("attempts"), r.opt_text("worker_id"));
        let timed_out = r.int("timed_out") == 1;
        let message = if timed_out {
            format!("timed out after {} ms", r.int("timeout_ms"))
        } else {
            "lease expired: the worker stopped sending heartbeats".to_owned()
        };
        let (status, ev, run_at) = if r.int("cancel_requested") == 1 {
            ("cancelled", "cancelled", now)
        } else if attempts >= r.int("max_attempts") {
            ("dead", "dead", now)
        } else if timed_out {
            let delay = backoff_ms(r.int("backoff_base_ms"), r.int("backoff_max_ms"), attempts, jitter());
            ("queued", "retry_scheduled", now + delay)
        } else {
            ("queued", "lease_expired", now)
        };
        let finished = status != "queued";
        tx.execute(&Statement::new(
            "UPDATE _forgebase_jobs SET status = ?2, lease_id = NULL, visibility_until = NULL, deadline_at = NULL, \
             run_at = ?3, last_error = ?5, finished_at = CASE WHEN ?4 = 1 THEN CAST(?6 AS BIGINT) ELSE NULL END, updated_at = ?6 \
             WHERE id = ?1",
            vec![
                id.as_str().into(),
                status.into(),
                run_at.into(),
                i64::from(finished).into(),
                message.as_str().into(),
                now.into(),
            ],
        ))
        .await?;
        let outcome = if timed_out { "timed_out" } else { "lease_expired" };
        close_attempt(tx, &id, now, outcome, Some(&message)).await?;
        let detail = if status == "queued" && timed_out {
            format!("{message} (retry at {})", iso(run_at))
        } else {
            message.clone()
        };
        event(tx, &id, now, ev, attempts, worker.as_deref(), Some(&detail)).await?;
        fx.job(&id, "status");
        fx.cancel.push(id.clone());
        if finished {
            let st = if status == "dead" {
                JobStatus::Dead
            } else {
                JobStatus::Cancelled
            };
            crate::flow::on_terminal(tx, &id, st, now, fx).await?;
        } else {
            fx.wake = true;
        }
    }
    Ok(rows.len())
}

/// The running job whose lease is `lease`, or `lease_lost` / `not found`.
pub(crate) async fn leased(tx: &mut dyn WriteTx, id: &str, lease: &str) -> Result<Job> {
    let rows = tx
        .fetch_all(&Statement::new(
            format!("SELECT {JOB_COLS} FROM _forgebase_jobs WHERE id = ?1"),
            vec![id.into()],
        ))
        .await?;
    if rows.is_empty() {
        return Err(JobError::NotFound(id.to_owned()));
    }
    let held = Row::new(&rows, 0).opt_text("lease_id");
    let job = job_from(&Row::new(&rows, 0))?;
    // Compare in constant shape; the token is random, so timing is not a concern.
    if job.status == JobStatus::Running && held.as_deref() == Some(lease) {
        Ok(job)
    } else {
        Err(JobError::LeaseLost(id.to_owned()))
    }
}

async fn heartbeat_tx(tx: &mut dyn WriteTx, id: &str, lease: &str, toucher: &Toucher, now: i64) -> Result<Heartbeat> {
    let job = leased(tx, id, lease).await?;
    let until = now + job.visibility_timeout_ms;
    tx.execute(&Statement::new(
        "UPDATE _forgebase_jobs SET visibility_until = ?2, heartbeat_at = ?3, updated_at = ?3 WHERE id = ?1",
        vec![id.into(), until.into(), now.into()],
    ))
    .await?;
    if let Some(w) = &job.worker_id {
        touch_worker_tx(tx, w, toucher, now).await?;
    }
    Ok(Heartbeat {
        visibility_until: iso(until),
        cancel_requested: job.cancel_requested,
        deadline_at: job.deadline_at,
    })
}

async fn complete_tx(
    tx: &mut dyn WriteTx,
    id: &str,
    lease: &str,
    result: Option<String>,
    stored: Option<(String, i64)>,
    now: i64,
    fx: &mut Fx,
) -> Result<Job> {
    let job = leased(tx, id, lease).await?;
    let (result_ref, result_size) = match stored {
        Some((key, size)) => (Some(key), Some(size)),
        None => (None, None),
    };
    tx.execute(&Statement::new(
        "UPDATE _forgebase_jobs SET status = 'succeeded', result = ?2, result_ref = ?4, result_size = ?5, \
         lease_id = NULL, visibility_until = NULL, deadline_at = NULL, last_error = NULL, finished_at = ?3, \
         updated_at = ?3 WHERE id = ?1",
        vec![
            id.into(),
            result.into(),
            now.into(),
            result_ref.into(),
            result_size.into(),
        ],
    ))
    .await?;
    close_attempt(tx, id, now, "succeeded", None).await?;
    event(tx, id, now, "succeeded", job.attempts, job.worker_id.as_deref(), None).await?;
    fx.job(id, "status");
    crate::flow::on_terminal(tx, id, JobStatus::Succeeded, now, fx).await?;
    fetch_job_tx(tx, id).await
}

async fn fail_tx(
    tx: &mut dyn WriteTx,
    id: &str,
    lease: &str,
    error: &str,
    retryable: bool,
    now: i64,
    fx: &mut Fx,
) -> Result<Job> {
    let job = leased(tx, id, lease).await?;
    let (status, ev, run_at) = if job.cancel_requested {
        ("cancelled", "cancelled", now)
    } else if !retryable || job.attempts >= job.max_attempts {
        ("dead", "dead", now)
    } else {
        let delay = backoff_ms(job.backoff_base_ms, job.backoff_max_ms, job.attempts, jitter());
        ("queued", "retry_scheduled", now + delay)
    };
    let error: String = error.chars().take(4000).collect();
    tx.execute(&Statement::new(
        "UPDATE _forgebase_jobs SET status = ?2, run_at = ?3, lease_id = NULL, visibility_until = NULL, \
         deadline_at = NULL, last_error = ?4, finished_at = CASE WHEN ?2 = 'queued' THEN NULL ELSE CAST(?5 AS BIGINT) END, \
         updated_at = ?5 WHERE id = ?1",
        vec![
            id.into(),
            status.into(),
            run_at.into(),
            error.as_str().into(),
            now.into(),
        ],
    ))
    .await?;
    let outcome = match status {
        "cancelled" => "cancelled",
        "dead" => "failed",
        _ => "retry",
    };
    close_attempt(tx, id, now, outcome, Some(&error)).await?;
    let message = if status == "queued" {
        format!("{error} (retry at {})", iso(run_at))
    } else {
        error
    };
    event(tx, id, now, ev, job.attempts, job.worker_id.as_deref(), Some(&message)).await?;
    fx.job(id, "status");
    match status {
        "queued" => fx.wake = true,
        "dead" => crate::flow::on_terminal(tx, id, JobStatus::Dead, now, fx).await?,
        _ => crate::flow::on_terminal(tx, id, JobStatus::Cancelled, now, fx).await?,
    }
    fetch_job_tx(tx, id).await
}

async fn release_tx(tx: &mut dyn WriteTx, id: &str, lease: &str, now: i64, fx: &mut Fx) -> Result<Job> {
    let job = leased(tx, id, lease).await?;
    let (status, ev) = if job.cancel_requested {
        ("cancelled", "cancelled")
    } else {
        ("queued", "released")
    };
    tx.execute(&Statement::new(
        "UPDATE _forgebase_jobs SET status = ?2, attempts = CASE WHEN attempts > 0 THEN attempts - 1 ELSE 0 END, run_at = ?3, lease_id = NULL, \
         visibility_until = NULL, deadline_at = NULL, finished_at = CASE WHEN ?2 = 'queued' THEN NULL ELSE CAST(?3 AS BIGINT) END, \
         updated_at = ?3 WHERE id = ?1",
        vec![id.into(), status.into(), now.into()],
    ))
    .await?;
    close_attempt(
        tx,
        id,
        now,
        if status == "queued" { "released" } else { "cancelled" },
        None,
    )
    .await?;
    event(
        tx,
        id,
        now,
        ev,
        job.attempts,
        job.worker_id.as_deref(),
        Some("worker shut down"),
    )
    .await?;
    fx.job(id, "status");
    if status == "queued" {
        fx.wake = true;
    } else {
        crate::flow::on_terminal(tx, id, JobStatus::Cancelled, now, fx).await?;
    }
    fetch_job_tx(tx, id).await
}

/// Cancels `id` (queued: at once; running: requested) and, with
/// `children`, its unfinished child jobs. A job that already finished is an
/// error only for the job asked for.
pub(crate) async fn cancel_tx(tx: &mut dyn WriteTx, id: &str, now: i64, fx: &mut Fx, strict: bool) -> Result<()> {
    let mut work = vec![(id.to_owned(), strict)];
    while let Some((id, strict)) = work.pop() {
        let job = fetch_job_tx(tx, &id).await?;
        match job.status {
            JobStatus::Queued => {
                tx.execute(&Statement::new(
                    "UPDATE _forgebase_jobs SET status = 'cancelled', finished_at = ?2, updated_at = ?2 WHERE id = ?1",
                    vec![id.as_str().into(), now.into()],
                ))
                .await?;
                event(tx, &id, now, "cancelled", job.attempts, None, None).await?;
                fx.job(&id, "status");
                // Boxed: on_terminal can cancel workflow jobs, which lands here again.
                Box::pin(crate::flow::on_terminal(tx, &id, JobStatus::Cancelled, now, fx)).await?;
            }
            JobStatus::Running => {
                tx.execute(&Statement::new(
                    "UPDATE _forgebase_jobs SET cancel_requested = 1, updated_at = ?2 WHERE id = ?1",
                    vec![id.as_str().into(), now.into()],
                ))
                .await?;
                if !job.cancel_requested {
                    event(
                        tx,
                        &id,
                        now,
                        "cancel_requested",
                        job.attempts,
                        job.worker_id.as_deref(),
                        None,
                    )
                    .await?;
                    fx.job(&id, "status");
                }
                fx.cancel.push(id.clone());
            }
            _ if strict => {
                return Err(JobError::state(
                    "job_finished",
                    format!("job is already {}", job.status.as_str()),
                ));
            }
            _ => continue,
        }
        let children = tx
            .fetch_all(&Statement::new(
                "SELECT id FROM _forgebase_jobs WHERE parent_id = ?1 AND status IN ('queued', 'running')",
                vec![id.as_str().into()],
            ))
            .await?;
        for i in 0..children.len() {
            work.push((Row::new(&children, i).text("id"), false));
        }
    }
    Ok(())
}

async fn retry_tx(tx: &mut dyn WriteTx, id: &str, now: i64, fx: &mut Fx) -> Result<Job> {
    let job = fetch_job_tx(tx, id).await?;
    if !matches!(job.status, JobStatus::Dead | JobStatus::Cancelled) {
        return Err(JobError::state(
            "job_not_retryable",
            format!(
                "only dead or cancelled jobs can be retried; this one is {}",
                job.status.as_str()
            ),
        ));
    }
    // Parents decide whether it may run again.
    let parents = tx
        .fetch_all(&Statement::new(
            "SELECT j.status FROM _forgebase_job_deps d JOIN _forgebase_jobs j ON j.id = d.parent_id \
             WHERE d.job_id = ?1",
            vec![id.into()],
        ))
        .await?;
    let mut waiting = 0_i64;
    for i in 0..parents.len() {
        match Row::new(&parents, i).text("status").as_str() {
            "succeeded" => {}
            "dead" | "cancelled" => {
                return Err(JobError::state(
                    "dependency_failed",
                    "a job this one depends on did not succeed; retry that one first",
                ));
            }
            _ => waiting += 1,
        }
    }
    tx.execute(&Statement::new(
        "UPDATE _forgebase_jobs SET status = 'queued', attempts = 0, run_at = ?2, cancel_requested = 0, \
         finished_at = NULL, waiting_for = ?3, updated_at = ?2 WHERE id = ?1",
        vec![id.into(), now.into(), waiting.into()],
    ))
    .await?;
    event(tx, id, now, "requeued", 0, None, None).await?;
    fx.job(id, "status");
    fx.wake = true;
    fetch_job_tx(tx, id).await
}

async fn delete_tx(tx: &mut dyn WriteTx, id: &str, fx: &mut Fx) -> Result<()> {
    let rows = tx
        .fetch_all(&Statement::new(
            "SELECT status, result_ref FROM _forgebase_jobs WHERE id = ?1",
            vec![id.into()],
        ))
        .await?;
    if rows.is_empty() {
        return Err(JobError::NotFound(id.to_owned()));
    }
    let r = Row::new(&rows, 0);
    let status = JobStatus::parse(&r.text("status")).unwrap_or(JobStatus::Running);
    if !status.is_terminal() {
        return Err(JobError::state(
            "job_active",
            format!("job is {}; cancel it first", status.as_str()),
        ));
    }
    if let Some(key) = r.opt_text("result_ref") {
        fx.drop_objects.push(key);
    }
    tx.execute(&Statement::new(
        "DELETE FROM _forgebase_jobs WHERE id = ?1",
        vec![id.into()],
    ))
    .await?;
    Ok(())
}

async fn fire_tx(tx: &mut dyn WriteTx, now: i64, fx: &mut Fx) -> Result<usize> {
    let rows = tx
        .fetch_all(&Statement::new(
            format!(
                "SELECT {SCHEDULE_COLS} FROM _forgebase_job_schedules WHERE enabled = 1 AND next_run_at <= ?1 \
                 ORDER BY next_run_at LIMIT 100"
            ),
            vec![now.into()],
        ))
        .await?;
    let mut fired = 0;
    for i in 0..rows.len() {
        let r = Row::new(&rows, i);
        let s = schedule_from(&r)?;
        let due = r.int("next_run_at");
        let policy = Policy {
            priority: s.priority,
            max_attempts: s.max_attempts,
            backoff_base_ms: s.backoff_base_ms,
            backoff_max_ms: s.backoff_max_ms,
            visibility_timeout_ms: s.visibility_timeout_ms,
        };
        let resolved = Resolved {
            queue: s.queue.clone(),
            kind: s.kind.clone(),
            payload: serde_json::to_string(&s.payload).unwrap_or_else(|_| "null".into()),
            policy,
            priority_given: true,
            run_at: now,
            idempotency_key: Some(format!("schedule:{}:{due}", s.id)),
            ..Resolved::default()
        };
        // FB-142: the schedule's tenant and its creator's recorded context.
        let origin = Origin {
            actor: format!("schedule:{}", s.name),
            tenant: s.tenant.clone(),
            context: r.opt_text("context"),
        };
        let (job, created) = insert_job(tx, &resolved, Some(&s.id), &origin, now, fx).await?;
        if created {
            fired += 1;
        }
        let next = Cron::parse(&s.cron).ok().and_then(|c| c.next_after(now));
        tx.execute(&Statement::new(
            "UPDATE _forgebase_job_schedules SET last_run_at = ?2, last_job_id = ?3, next_run_at = COALESCE(?4, next_run_at), \
             enabled = CASE WHEN ?4 IS NULL THEN 0 ELSE enabled END, updated_at = ?2 WHERE id = ?1",
            vec![s.id.as_str().into(), now.into(), job.id.as_str().into(), next.into()],
        ))
        .await?;
        if next.is_none() {
            tracing::warn!(schedule = %s.name, "cron expression no longer fires; schedule disabled");
        }
    }
    Ok(fired)
}
