//! [`JobService`]: every job operation, each one transactional.
//!
//! All state lives in SQLite (`_forgebase_jobs` and friends, migration 0007).
//! The database has one writer, so a claim, a completion or a firing is atomic
//! without further locking, and anything committed survives a crash or restart.
//!
//! Public methods take an [`AccessContext`] and ask the
//! [`Authorizer`](forgebase_data::Authorizer) of the data core first
//! ([`Action::ReadJobs`], [`Action::EnqueueJob`], [`Action::ManageJobs`],
//! [`Action::WorkJobs`]). In-process machinery (the worker pool) uses
//! [`AccessContext::system`].

use std::collections::BTreeMap;
use std::sync::Arc;

use forgebase_data::{AccessContext, Action, DataService};
use forgebase_db::{Backend, Rows, Statement, TxMode, Value, WriteTx};
use serde_json::Value as Json;
use tokio::sync::Notify;

use crate::clock::{Clock, SystemClock};
use crate::cron::Cron;
use crate::error::JobError;
use crate::model::{
    ClaimRequest, ClaimedJob, Heartbeat, Job, JobEvent, JobFilter, JobPage, JobStats, JobStatus, MAX_CLAIM, MAX_PAGE,
    NewJob, NewSchedule, NewWorker, Policy, QueueStats, Resolved, Schedule, SchedulePatch, WORKER_ONLINE_MS, Worker,
    backoff_ms, check_name, check_payload, iso, jitter,
};

type Result<T> = std::result::Result<T, JobError>;

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
}

const JOB_COLS: &str = "id, queue, kind, payload, priority, status, run_at, attempts, max_attempts, backoff_base_ms, \
    backoff_max_ms, visibility_timeout_ms, idempotency_key, schedule_id, worker_id, lease_id, visibility_until, \
    cancel_requested, result, last_error, created_by, created_at, updated_at, started_at, finished_at";

const SCHEDULE_COLS: &str = "id, name, cron, queue, kind, payload, priority, max_attempts, backoff_base_ms, \
    backoff_max_ms, visibility_timeout_ms, enabled, next_run_at, last_run_at, last_job_id, created_by, created_at, \
    updated_at";

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
            }),
        }
    }

    /// Current time in Unix epoch milliseconds, from this service's clock.
    pub fn now(&self) -> i64 {
        self.inner.clock.now_ms()
    }

    fn backend(&self) -> &dyn Backend {
        self.inner.data.backend().as_ref()
    }

    fn allow(&self, ctx: &AccessContext, action: Action) -> Result<()> {
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

    async fn begin(&self) -> Result<Box<dyn WriteTx>> {
        Ok(self.backend().begin(TxMode::Normal).await?)
    }

    // ---------------------------------------------------------------- enqueue

    /// Enqueues a job. With an idempotency key that already exists in the
    /// queue, nothing is inserted and the first job is returned with
    /// `created == false`.
    pub async fn enqueue(&self, ctx: &AccessContext, new: NewJob) -> Result<(Job, bool)> {
        self.allow(ctx, Action::EnqueueJob)?;
        let now = self.now();
        let resolved = new.resolve(now)?;
        let actor = ctx.actor();
        let mut tx = self.begin().await?;
        let r = insert_job(&mut *tx, &resolved, None, &actor, now).await;
        let (job, created) = finish(tx, r).await?;
        if created {
            self.wake();
        }
        Ok((job, created))
    }

    // ------------------------------------------------------------------ reads

    /// One job.
    pub async fn get(&self, ctx: &AccessContext, id: &str) -> Result<Job> {
        self.allow(ctx, Action::ReadJobs)?;
        fetch_job(self.backend(), id)
            .await?
            .ok_or_else(|| JobError::NotFound(id.to_owned()))
    }

    /// Jobs, newest first.
    pub async fn list(&self, ctx: &AccessContext, filter: JobFilter) -> Result<JobPage> {
        self.allow(ctx, Action::ReadJobs)?;
        let mut where_sql = String::from("1 = 1");
        let mut params: Vec<Value> = Vec::new();
        if let Some(status) = filter.status {
            params.push(status.as_str().into());
            where_sql.push_str(&format!(" AND status = ?{}", params.len()));
        }
        if let Some(queue) = &filter.queue {
            params.push(queue.as_str().into());
            where_sql.push_str(&format!(" AND queue = ?{}", params.len()));
        }
        if let Some(kind) = &filter.kind {
            params.push(kind.as_str().into());
            where_sql.push_str(&format!(" AND kind = ?{}", params.len()));
        }
        let total = self
            .backend()
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
            .backend()
            .fetch_all(&Statement::new(
                format!(
                    "SELECT {JOB_COLS} FROM _forgebase_jobs WHERE {where_sql} \
                     ORDER BY created_at DESC, rowid DESC LIMIT ?{} OFFSET ?{}",
                    params.len() - 1,
                    params.len()
                ),
                params,
            ))
            .await?;
        let items = (0..rows.len())
            .map(|i| job_from(&Row::new(&rows, i)))
            .collect::<Result<_>>()?;
        Ok(JobPage { items, total })
    }

    /// The history of a job, oldest first.
    pub async fn events(&self, ctx: &AccessContext, id: &str) -> Result<Vec<JobEvent>> {
        self.allow(ctx, Action::ReadJobs)?;
        if fetch_job(self.backend(), id).await?.is_none() {
            return Err(JobError::NotFound(id.to_owned()));
        }
        let rows = self
            .backend()
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

    /// Counts per state and queue.
    pub async fn stats(&self, ctx: &AccessContext) -> Result<JobStats> {
        self.allow(ctx, Action::ReadJobs)?;
        let rows = self
            .backend()
            .fetch_all(&Statement::raw(
                "SELECT queue, status, COUNT(*) AS n FROM _forgebase_jobs GROUP BY queue, status ORDER BY queue",
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
    /// `cancel_requested`: its worker learns it from the next heartbeat and
    /// should stop and report `fail`; the job then ends `cancelled`.
    pub async fn cancel(&self, ctx: &AccessContext, id: &str) -> Result<Job> {
        self.allow(ctx, Action::ManageJobs)?;
        let mut tx = self.begin().await?;
        let r = cancel_tx(&mut *tx, id, self.now()).await;
        finish(tx, r).await
    }

    /// Moves a dead or cancelled job back to the queue with a fresh attempt count.
    pub async fn retry(&self, ctx: &AccessContext, id: &str) -> Result<Job> {
        self.allow(ctx, Action::ManageJobs)?;
        let mut tx = self.begin().await?;
        let r = retry_tx(&mut *tx, id, self.now()).await;
        let job = finish(tx, r).await?;
        self.wake();
        Ok(job)
    }

    /// Deletes a finished job and its history.
    pub async fn delete(&self, ctx: &AccessContext, id: &str) -> Result<()> {
        self.allow(ctx, Action::ManageJobs)?;
        let mut tx = self.begin().await?;
        let r = delete_tx(&mut *tx, id).await;
        finish(tx, r).await
    }

    // --------------------------------------------------------------- workers

    /// Registers (or updates) a worker and returns it.
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
        let mut tx = self.begin().await?;
        let r = async {
            tx.execute(&Statement::new(
                "INSERT INTO _forgebase_job_workers (id, name, queues, kinds, concurrency, registered_at, last_seen_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6) \
                 ON CONFLICT (id) DO UPDATE SET name = excluded.name, queues = excluded.queues, \
                 kinds = excluded.kinds, concurrency = excluded.concurrency, last_seen_at = excluded.last_seen_at",
                vec![
                    id.as_str().into(),
                    new.name.clone().into(),
                    serde_json::to_string(&new.queues).unwrap_or_else(|_| "[]".into()).into(),
                    serde_json::to_string(&new.kinds).unwrap_or_else(|_| "[]".into()).into(),
                    concurrency.into(),
                    now.into(),
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
                "SELECT id, name, queues, kinds, concurrency, registered_at, last_seen_at \
                 FROM _forgebase_job_workers WHERE id = ?1",
                vec![id.into()],
            ))
            .await?;
        if rows.is_empty() {
            return Err(JobError::Internal(format!("worker {id} vanished")));
        }
        Ok(worker_from(&Row::new(&rows, 0), self.now()))
    }

    /// Workers, most recently seen first.
    pub async fn workers(&self, ctx: &AccessContext) -> Result<Vec<Worker>> {
        self.allow(ctx, Action::ReadJobs)?;
        let rows = self
            .backend()
            .fetch_all(&Statement::raw(
                "SELECT id, name, queues, kinds, concurrency, registered_at, last_seen_at \
                 FROM _forgebase_job_workers ORDER BY last_seen_at DESC, id LIMIT 500",
            ))
            .await?;
        let now = self.now();
        Ok((0..rows.len()).map(|i| worker_from(&Row::new(&rows, i), now)).collect())
    }

    /// Claims up to `req.max` due jobs, highest priority first, then oldest
    /// due time. Each returned job is `running` with a fresh lease that
    /// expires after the job's visibility timeout unless the worker sends
    /// heartbeats. Expired leases of other workers are re-queued first.
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
        // Cheap read first: an idle poll must not take the single writer.
        if !self.has_claimable(&req, now).await? {
            self.touch_worker(&req.worker_id, now).await?;
            return Ok(Vec::new());
        }
        let mut tx = self.begin().await?;
        let r = claim_tx(&mut *tx, &req, max, now).await;
        finish(tx, r).await
    }

    async fn has_claimable(&self, req: &ClaimRequest, now: i64) -> Result<bool> {
        let (filter, mut params) = claim_filter(&req.queues, &req.kinds, 1);
        params.insert(0, now.into());
        let sql = format!(
            "SELECT \
               EXISTS (SELECT 1 FROM _forgebase_jobs WHERE status = 'running' AND visibility_until <= ?1) \
               OR EXISTS (SELECT 1 FROM _forgebase_jobs WHERE status = 'queued' AND run_at <= ?1 {filter})"
        );
        Ok(self
            .backend()
            .fetch_all(&Statement::new(sql, params))
            .await?
            .scalar()
            .and_then(Value::as_i64)
            == Some(1))
    }

    async fn touch_worker(&self, id: &str, now: i64) -> Result<()> {
        let mut tx = self.begin().await?;
        let r = touch_worker_tx(&mut *tx, id, now).await;
        finish(tx, r).await
    }

    /// Extends the lease and tells the worker whether the job was cancelled.
    pub async fn heartbeat(&self, ctx: &AccessContext, id: &str, lease_id: &str) -> Result<Heartbeat> {
        self.allow(ctx, Action::WorkJobs)?;
        let now = self.now();
        let mut tx = self.begin().await?;
        let r = heartbeat_tx(&mut *tx, id, lease_id, now).await;
        finish(tx, r).await
    }

    /// Marks a running job succeeded. Fails with `lease_lost` if the lease is
    /// not the job's current one (expired and re-claimed, or already finished),
    /// which is what makes completion happen once.
    pub async fn complete(&self, ctx: &AccessContext, id: &str, lease_id: &str, result: Option<Json>) -> Result<Job> {
        self.allow(ctx, Action::WorkJobs)?;
        let result_text = match &result {
            Some(v) => Some(check_payload(v)?),
            None => None,
        };
        let mut tx = self.begin().await?;
        let r = complete_tx(&mut *tx, id, lease_id, result_text, self.now()).await;
        finish(tx, r).await
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
        let mut tx = self.begin().await?;
        let r = fail_tx(&mut *tx, id, lease_id, error, retryable, self.now()).await;
        let job = finish(tx, r).await?;
        if job.status == JobStatus::Queued {
            self.wake();
        }
        Ok(job)
    }

    /// Gives a job back without counting the attempt (graceful shutdown).
    pub async fn release(&self, ctx: &AccessContext, id: &str, lease_id: &str) -> Result<Job> {
        self.allow(ctx, Action::WorkJobs)?;
        let mut tx = self.begin().await?;
        let r = release_tx(&mut *tx, id, lease_id, self.now()).await;
        let job = finish(tx, r).await?;
        self.wake();
        Ok(job)
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
                 updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?15)",
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
                    ctx.actor().into(),
                    now.into(),
                ],
            ))
            .await?;
            fetch_schedule_tx(&mut *tx, &id).await
        }
        .await;
        finish(tx, r).await
    }

    /// Changes a schedule. A new cron expression recomputes the next firing.
    pub async fn update_schedule(&self, ctx: &AccessContext, id: &str, patch: SchedulePatch) -> Result<Schedule> {
        self.allow(ctx, Action::ManageJobs)?;
        let now = self.now();
        let mut tx = self.begin().await?;
        let r = async {
            let mut s = fetch_schedule_tx(&mut *tx, id).await?;
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
        finish(tx, r).await
    }

    /// Deletes a schedule (jobs it already enqueued stay).
    pub async fn delete_schedule(&self, ctx: &AccessContext, id: &str) -> Result<()> {
        self.allow(ctx, Action::ManageJobs)?;
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
        finish(tx, r).await
    }

    /// One schedule.
    pub async fn schedule(&self, ctx: &AccessContext, id: &str) -> Result<Schedule> {
        self.allow(ctx, Action::ReadJobs)?;
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

    /// All schedules by name.
    pub async fn schedules(&self, ctx: &AccessContext) -> Result<Vec<Schedule>> {
        self.allow(ctx, Action::ReadJobs)?;
        let rows = self
            .backend()
            .fetch_all(&Statement::raw(format!(
                "SELECT {SCHEDULE_COLS} FROM _forgebase_job_schedules ORDER BY name LIMIT 1000"
            )))
            .await?;
        (0..rows.len()).map(|i| schedule_from(&Row::new(&rows, i))).collect()
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
        let mut tx = self.begin().await?;
        let r = fire_tx(&mut *tx, now).await;
        let n = finish(tx, r).await?;
        if n > 0 {
            self.wake();
        }
        Ok(n)
    }

    /// Re-queues (or dead-letters) running jobs whose lease expired. Claims do
    /// this too; this is for idle systems. Returns the number of jobs touched.
    pub async fn reap_expired(&self, ctx: &AccessContext) -> Result<usize> {
        self.allow(ctx, Action::ManageJobs)?;
        let now = self.now();
        let expired = self
            .backend()
            .fetch_all(&Statement::new(
                "SELECT 1 FROM _forgebase_jobs WHERE status = 'running' AND visibility_until <= ?1 LIMIT 1",
                vec![now.into()],
            ))
            .await?;
        if expired.is_empty() {
            return Ok(0);
        }
        let mut tx = self.begin().await?;
        let r = reap_tx(&mut *tx, now).await;
        let n = finish(tx, r).await?;
        if n > 0 {
            self.wake();
        }
        Ok(n)
    }

    /// Deletes succeeded and cancelled jobs that finished before
    /// `older_than_ms` (epoch ms), with their history. The dead-letter queue is
    /// never purged automatically. Returns the number deleted.
    pub async fn purge_finished(&self, ctx: &AccessContext, older_than_ms: i64) -> Result<u64> {
        self.allow(ctx, Action::ManageJobs)?;
        let mut tx = self.begin().await?;
        let r = async {
            Ok::<_, JobError>(
                tx.execute(&Statement::new(
                    "DELETE FROM _forgebase_jobs WHERE status IN ('succeeded', 'cancelled') AND finished_at < ?1",
                    vec![older_than_ms.into()],
                ))
                .await?
                .rows_affected,
            )
        }
        .await;
        finish(tx, r).await
    }
}

/// Commits on success, rolls back on error.
async fn finish<T>(tx: Box<dyn WriteTx>, result: Result<T>) -> Result<T> {
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

struct Row<'a> {
    rows: &'a Rows,
    i: usize,
}

impl<'a> Row<'a> {
    fn new(rows: &'a Rows, i: usize) -> Self {
        Self { rows, i }
    }

    fn value(&self, col: &str) -> &Value {
        static NULL: Value = Value::Null;
        self.rows
            .column(col)
            .and_then(|c| self.rows.rows.get(self.i).and_then(|r| r.get(c)))
            .unwrap_or(&NULL)
    }

    fn text(&self, col: &str) -> String {
        self.value(col).as_str().unwrap_or_default().to_owned()
    }

    fn opt_text(&self, col: &str) -> Option<String> {
        self.value(col).as_str().map(str::to_owned)
    }

    fn int(&self, col: &str) -> i64 {
        self.value(col).as_i64().unwrap_or(0)
    }

    fn opt_int(&self, col: &str) -> Option<i64> {
        self.value(col).as_i64()
    }

    fn json(&self, col: &str) -> Result<Json> {
        serde_json::from_str(self.value(col).as_str().unwrap_or("null"))
            .map_err(|e| JobError::Internal(format!("column {col} is not JSON: {e}")))
    }
}

fn job_from(r: &Row<'_>) -> Result<Job> {
    let status = r.text("status");
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
        result: if r.value("result").is_null() {
            None
        } else {
            Some(r.json("result")?)
        },
        last_error: r.opt_text("last_error"),
        created_by: r.text("created_by"),
        created_at: iso(r.int("created_at")),
        updated_at: iso(r.int("updated_at")),
        started_at: r.opt_int("started_at").map(iso),
        finished_at: r.opt_int("finished_at").map(iso),
    })
}

fn schedule_from(r: &Row<'_>) -> Result<Schedule> {
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
        next_run_at: iso(r.int("next_run_at")),
        last_run_at: r.opt_int("last_run_at").map(iso),
        last_job_id: r.opt_text("last_job_id"),
        created_by: r.text("created_by"),
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
    }
}

async fn fetch_job(backend: &dyn Backend, id: &str) -> Result<Option<Job>> {
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

async fn fetch_job_tx(tx: &mut dyn WriteTx, id: &str) -> Result<Job> {
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

async fn event(
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

// ---------------------------------------------------------------------------
// Transactions

async fn insert_job(
    tx: &mut dyn WriteTx,
    r: &Resolved,
    schedule_id: Option<&str>,
    actor: &str,
    now: i64,
) -> Result<(Job, bool)> {
    if let Some(key) = &r.idempotency_key {
        let rows = tx
            .fetch_all(&Statement::new(
                "SELECT id FROM _forgebase_jobs WHERE queue = ?1 AND idempotency_key = ?2",
                vec![r.queue.as_str().into(), key.as_str().into()],
            ))
            .await?;
        if !rows.is_empty() {
            let id = Row::new(&rows, 0).text("id");
            return Ok((fetch_job_tx(tx, &id).await?, false));
        }
    }
    let id = format!("job-{}", uuid::Uuid::new_v4().simple());
    tx.execute(&Statement::new(
        "INSERT INTO _forgebase_jobs (id, queue, kind, payload, priority, status, run_at, max_attempts, \
         backoff_base_ms, backoff_max_ms, visibility_timeout_ms, idempotency_key, schedule_id, created_by, \
         created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, 'queued', ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?14)",
        vec![
            id.as_str().into(),
            r.queue.as_str().into(),
            r.kind.as_str().into(),
            r.payload.as_str().into(),
            r.policy.priority.into(),
            r.run_at.into(),
            r.policy.max_attempts.into(),
            r.policy.backoff_base_ms.into(),
            r.policy.backoff_max_ms.into(),
            r.policy.visibility_timeout_ms.into(),
            r.idempotency_key.clone().into(),
            schedule_id.map(str::to_owned).into(),
            actor.into(),
            now.into(),
        ],
    ))
    .await?;
    event(tx, &id, now, "enqueued", 0, None, None).await?;
    Ok((fetch_job_tx(tx, &id).await?, true))
}

/// `AND queue IN (...) AND kind IN (...)` with placeholders numbered after `taken`.
fn claim_filter(queues: &[String], kinds: &[String], taken: usize) -> (String, Vec<Value>) {
    let mut sql = String::new();
    let mut params: Vec<Value> = Vec::new();
    for (col, list) in [("queue", queues), ("kind", kinds)] {
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
        sql.push_str(&format!(" AND {col} IN ({})", marks.join(", ")));
    }
    (sql, params)
}

async fn touch_worker_tx(tx: &mut dyn WriteTx, id: &str, now: i64) -> Result<()> {
    tx.execute(&Statement::new(
        "INSERT INTO _forgebase_job_workers (id, registered_at, last_seen_at) VALUES (?1, ?2, ?2) \
         ON CONFLICT (id) DO UPDATE SET last_seen_at = excluded.last_seen_at",
        vec![id.into(), now.into()],
    ))
    .await?;
    Ok(())
}

async fn claim_tx(tx: &mut dyn WriteTx, req: &ClaimRequest, max: i64, now: i64) -> Result<Vec<ClaimedJob>> {
    touch_worker_tx(tx, &req.worker_id, now).await?;
    reap_tx(tx, now).await?;
    let (filter, mut params) = claim_filter(&req.queues, &req.kinds, 2);
    params.insert(0, now.into());
    params.insert(1, max.into());
    let rows = tx
        .fetch_all(&Statement::new(
            format!(
                "SELECT id FROM _forgebase_jobs WHERE status = 'queued' AND run_at <= ?1 {filter} \
                 ORDER BY priority DESC, run_at, rowid LIMIT ?2"
            ),
            params,
        ))
        .await?;
    let mut claimed = Vec::new();
    for i in 0..rows.len() {
        let id = Row::new(&rows, i).text("id");
        let lease = format!("lease-{}", uuid::Uuid::new_v4().simple());
        tx.execute(&Statement::new(
            "UPDATE _forgebase_jobs SET status = 'running', attempts = attempts + 1, lease_id = ?2, worker_id = ?3, \
             visibility_until = ?4 + visibility_timeout_ms, heartbeat_at = ?4, started_at = ?4, updated_at = ?4 \
             WHERE id = ?1 AND status = 'queued'",
            vec![
                id.as_str().into(),
                lease.as_str().into(),
                req.worker_id.as_str().into(),
                now.into(),
            ],
        ))
        .await?;
        let job = fetch_job_tx(tx, &id).await?;
        event(tx, &id, now, "claimed", job.attempts, Some(&req.worker_id), None).await?;
        claimed.push(ClaimedJob { lease_id: lease, job });
    }
    Ok(claimed)
}

/// Ends the leases that expired: cancelled if cancellation was requested,
/// dead-lettered when attempts are used up, otherwise queued again at once.
async fn reap_tx(tx: &mut dyn WriteTx, now: i64) -> Result<usize> {
    let rows = tx
        .fetch_all(&Statement::new(
            "SELECT id, attempts, max_attempts, cancel_requested, worker_id FROM _forgebase_jobs \
             WHERE status = 'running' AND visibility_until <= ?1",
            vec![now.into()],
        ))
        .await?;
    for i in 0..rows.len() {
        let r = Row::new(&rows, i);
        let (id, attempts, worker) = (r.text("id"), r.int("attempts"), r.opt_text("worker_id"));
        let (status, ev, finished) = if r.int("cancel_requested") == 1 {
            ("cancelled", "cancelled", true)
        } else if attempts >= r.int("max_attempts") {
            ("dead", "dead", true)
        } else {
            ("queued", "lease_expired", false)
        };
        tx.execute(&Statement::new(
            "UPDATE _forgebase_jobs SET status = ?2, lease_id = NULL, visibility_until = NULL, run_at = ?3, \
             last_error = 'lease expired: the worker stopped sending heartbeats', \
             finished_at = CASE WHEN ?4 = 1 THEN ?3 ELSE NULL END, updated_at = ?3 WHERE id = ?1",
            vec![
                id.as_str().into(),
                status.into(),
                now.into(),
                i64::from(finished).into(),
            ],
        ))
        .await?;
        event(
            tx,
            &id,
            now,
            ev,
            attempts,
            worker.as_deref(),
            Some("lease expired: the worker stopped sending heartbeats"),
        )
        .await?;
    }
    Ok(rows.len())
}

/// The running job whose lease is `lease`, or `lease_lost` / `not found`.
async fn leased(tx: &mut dyn WriteTx, id: &str, lease: &str) -> Result<Job> {
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

async fn heartbeat_tx(tx: &mut dyn WriteTx, id: &str, lease: &str, now: i64) -> Result<Heartbeat> {
    let job = leased(tx, id, lease).await?;
    let until = now + job.visibility_timeout_ms;
    tx.execute(&Statement::new(
        "UPDATE _forgebase_jobs SET visibility_until = ?2, heartbeat_at = ?3, updated_at = ?3 WHERE id = ?1",
        vec![id.into(), until.into(), now.into()],
    ))
    .await?;
    if let Some(w) = &job.worker_id {
        touch_worker_tx(tx, w, now).await?;
    }
    Ok(Heartbeat {
        visibility_until: iso(until),
        cancel_requested: job.cancel_requested,
    })
}

async fn complete_tx(tx: &mut dyn WriteTx, id: &str, lease: &str, result: Option<String>, now: i64) -> Result<Job> {
    let job = leased(tx, id, lease).await?;
    tx.execute(&Statement::new(
        "UPDATE _forgebase_jobs SET status = 'succeeded', result = ?2, lease_id = NULL, visibility_until = NULL, \
         last_error = NULL, finished_at = ?3, updated_at = ?3 WHERE id = ?1",
        vec![id.into(), result.into(), now.into()],
    ))
    .await?;
    event(tx, id, now, "succeeded", job.attempts, job.worker_id.as_deref(), None).await?;
    fetch_job_tx(tx, id).await
}

async fn fail_tx(tx: &mut dyn WriteTx, id: &str, lease: &str, error: &str, retryable: bool, now: i64) -> Result<Job> {
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
         last_error = ?4, finished_at = CASE WHEN ?2 = 'queued' THEN NULL ELSE ?5 END, updated_at = ?5 WHERE id = ?1",
        vec![
            id.into(),
            status.into(),
            run_at.into(),
            error.as_str().into(),
            now.into(),
        ],
    ))
    .await?;
    let message = if status == "queued" {
        format!("{error} (retry at {})", iso(run_at))
    } else {
        error
    };
    event(tx, id, now, ev, job.attempts, job.worker_id.as_deref(), Some(&message)).await?;
    fetch_job_tx(tx, id).await
}

async fn release_tx(tx: &mut dyn WriteTx, id: &str, lease: &str, now: i64) -> Result<Job> {
    let job = leased(tx, id, lease).await?;
    let (status, ev) = if job.cancel_requested {
        ("cancelled", "cancelled")
    } else {
        ("queued", "released")
    };
    tx.execute(&Statement::new(
        "UPDATE _forgebase_jobs SET status = ?2, attempts = MAX(attempts - 1, 0), run_at = ?3, lease_id = NULL, \
         visibility_until = NULL, finished_at = CASE WHEN ?2 = 'queued' THEN NULL ELSE ?3 END, updated_at = ?3 \
         WHERE id = ?1",
        vec![id.into(), status.into(), now.into()],
    ))
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
    fetch_job_tx(tx, id).await
}

async fn cancel_tx(tx: &mut dyn WriteTx, id: &str, now: i64) -> Result<Job> {
    let job = fetch_job_tx(tx, id).await?;
    match job.status {
        JobStatus::Queued => {
            tx.execute(&Statement::new(
                "UPDATE _forgebase_jobs SET status = 'cancelled', finished_at = ?2, updated_at = ?2 WHERE id = ?1",
                vec![id.into(), now.into()],
            ))
            .await?;
            event(tx, id, now, "cancelled", job.attempts, None, None).await?;
        }
        JobStatus::Running => {
            tx.execute(&Statement::new(
                "UPDATE _forgebase_jobs SET cancel_requested = 1, updated_at = ?2 WHERE id = ?1",
                vec![id.into(), now.into()],
            ))
            .await?;
            if !job.cancel_requested {
                event(
                    tx,
                    id,
                    now,
                    "cancel_requested",
                    job.attempts,
                    job.worker_id.as_deref(),
                    None,
                )
                .await?;
            }
        }
        _ => {
            return Err(JobError::state(
                "job_finished",
                format!("job is already {}", job.status.as_str()),
            ));
        }
    }
    fetch_job_tx(tx, id).await
}

async fn retry_tx(tx: &mut dyn WriteTx, id: &str, now: i64) -> Result<Job> {
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
    tx.execute(&Statement::new(
        "UPDATE _forgebase_jobs SET status = 'queued', attempts = 0, run_at = ?2, cancel_requested = 0, \
         finished_at = NULL, updated_at = ?2 WHERE id = ?1",
        vec![id.into(), now.into()],
    ))
    .await?;
    event(tx, id, now, "requeued", 0, None, None).await?;
    fetch_job_tx(tx, id).await
}

async fn delete_tx(tx: &mut dyn WriteTx, id: &str) -> Result<()> {
    let job = fetch_job_tx(tx, id).await?;
    if !job.status.is_terminal() {
        return Err(JobError::state(
            "job_active",
            format!("job is {}; cancel it first", job.status.as_str()),
        ));
    }
    tx.execute(&Statement::new(
        "DELETE FROM _forgebase_jobs WHERE id = ?1",
        vec![id.into()],
    ))
    .await?;
    Ok(())
}

async fn fire_tx(tx: &mut dyn WriteTx, now: i64) -> Result<usize> {
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
            run_at: now,
            idempotency_key: Some(format!("schedule:{}:{due}", s.id)),
        };
        let (job, created) = insert_job(tx, &resolved, Some(&s.id), &format!("schedule:{}", s.name), now).await?;
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
