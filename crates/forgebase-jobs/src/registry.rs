//! Job handlers: a Rust trait and a registry keyed by job `kind`.
//!
//! Wasm functions (FB-60) will plug in later by registering a handler that
//! calls into the runtime; nothing here knows about them.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;

use forgebase_data::AccessContext;
use forgebase_db::BoxFuture;
use serde_json::Value as Json;
use tokio::sync::watch;

use crate::JobError;
use crate::access::{EnqueueGuard, KindAccess, RecordedContext};
use crate::model::{ClaimedJob, Job, NewJob};
use crate::service::JobService;
use crate::tasks::{ParentResult, ProgressReport};

/// Why a handler failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandlerError {
    /// Try again later (with backoff) while attempts remain.
    Retry(String),
    /// Do not retry: the job goes straight to the dead-letter queue.
    Fatal(String),
}

impl HandlerError {
    /// A retryable failure.
    pub fn retry(message: impl Into<String>) -> Self {
        HandlerError::Retry(message.into())
    }

    /// A permanent failure.
    pub fn fatal(message: impl Into<String>) -> Self {
        HandlerError::Fatal(message.into())
    }
}

impl<E: std::error::Error> From<E> for HandlerError {
    /// Any error is retryable by default; use [`HandlerError::fatal`] to give up.
    fn from(e: E) -> Self {
        HandlerError::Retry(e.to_string())
    }
}

/// Observes the cancellation of a running job: it fires when the job is
/// cancelled, timed out or its lease was lost. Cheap to clone; hand it to
/// the parts of a handler that should stop.
#[derive(Debug, Clone)]
pub struct CancelToken {
    rx: watch::Receiver<bool>,
}

impl CancelToken {
    /// Whether the job should stop now.
    pub fn is_cancelled(&self) -> bool {
        *self.rx.borrow()
    }

    /// Resolves when the job should stop. Never resolves otherwise; use it
    /// in `tokio::select!` beside the real work.
    pub async fn cancelled(&self) {
        let mut rx = self.rx.clone();
        loop {
            if *rx.borrow_and_update() {
                return;
            }
            if rx.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }
}

/// The connection of a context to the queue (absent in detached contexts).
#[derive(Debug, Clone)]
struct Live {
    service: JobService,
    lease: String,
    cancel: Arc<watch::Sender<bool>>,
}

/// What a running handler gets.
#[derive(Debug, Clone)]
pub struct JobContext {
    job: Job,
    cancel: watch::Receiver<bool>,
    checkpoint: Option<Json>,
    parents: Vec<ParentResult>,
    live: Option<Live>,
}

impl JobContext {
    pub(crate) fn new(
        claimed: ClaimedJob,
        service: JobService,
        cancel_tx: Arc<watch::Sender<bool>>,
        cancel: watch::Receiver<bool>,
    ) -> Self {
        Self {
            job: claimed.job,
            cancel,
            checkpoint: claimed.checkpoint,
            parents: claimed.parents,
            live: Some(Live {
                service,
                lease: claimed.lease_id,
                cancel: cancel_tx,
            }),
        }
    }

    /// A context that is never cancelled and not connected to a queue (for
    /// calling handlers in tests): progress and checkpoints are accepted and
    /// dropped.
    pub fn detached(job: Job) -> Self {
        let (tx, rx) = watch::channel(false);
        // A closed channel simply never reports cancellation.
        drop(tx);
        Self {
            job,
            cancel: rx,
            checkpoint: None,
            parents: Vec::new(),
            live: None,
        }
    }

    /// Same detached context, resuming from `checkpoint`.
    #[must_use]
    pub fn with_checkpoint(mut self, checkpoint: Json) -> Self {
        self.checkpoint = Some(checkpoint);
        self
    }

    /// The job.
    pub fn job(&self) -> &Job {
        &self.job
    }

    /// Who enqueued the job (FB-142): the recorded context of the caller
    /// whose request created it, a schedule's creator for scheduled jobs, the
    /// parent's for child jobs; `None` when the system enqueued it (or the
    /// job predates FB-142). Roles and attributes are not part of it: the
    /// policy engine resolves them again when the handler acts as it.
    pub fn enqueuer(&self) -> Option<AccessContext> {
        RecordedContext::decode(self.job.context.as_deref())
            .ok()
            .flatten()
            .and_then(|r| r.context())
    }

    /// The job's payload.
    pub fn payload(&self) -> &Json {
        &self.job.payload
    }

    /// 1 for the first attempt.
    pub fn attempt(&self) -> i64 {
        self.job.attempts
    }

    /// Whether a failure now would dead-letter the job.
    pub fn is_last_attempt(&self) -> bool {
        self.job.attempts >= self.job.max_attempts
    }

    /// Whether the job was cancelled, timed out or its lease was lost: stop working.
    pub fn is_cancelled(&self) -> bool {
        *self.cancel.borrow()
    }

    /// Resolves when the job is cancelled, timed out or its lease was lost.
    /// Never resolves otherwise; use it in `tokio::select!` beside the real work.
    pub async fn cancelled(&self) {
        self.cancel_token().cancelled().await;
    }

    /// A cancellation token for this job.
    pub fn cancel_token(&self) -> CancelToken {
        CancelToken {
            rx: self.cancel.clone(),
        }
    }

    /// The last checkpoint saved by an earlier attempt (after a crash,
    /// timeout, retry or restart): resume from it. `None` on a fresh start.
    pub fn checkpoint(&self) -> Option<&Json> {
        self.checkpoint.as_ref()
    }

    /// The parents (dependencies) of the job with their results.
    pub fn parents(&self) -> &[ParentResult] {
        &self.parents
    }

    /// The parents' results by step name (or job id for jobs outside a
    /// workflow), including results kept in object storage.
    pub async fn parent_results(&self) -> Result<BTreeMap<String, Json>, JobError> {
        let mut out = BTreeMap::new();
        for p in &self.parents {
            let key = p.step.clone().unwrap_or_else(|| p.id.clone());
            let value = if p.result_stored {
                match &self.live {
                    Some(l) => l
                        .service
                        .result(&AccessContext::system(), &p.id)
                        .await?
                        .unwrap_or(Json::Null),
                    None => Json::Null,
                }
            } else {
                p.result.clone().unwrap_or(Json::Null)
            };
            out.insert(key, value);
        }
        Ok(out)
    }

    /// Reports progress (stored on the job and streamed on `jobs:<id>`).
    /// Also renews the lease. Learning that the job was cancelled fires the
    /// cancel token.
    pub async fn progress(&self, report: ProgressReport) -> Result<(), JobError> {
        let Some(l) = &self.live else {
            return report.validate();
        };
        let hb = l
            .service
            .progress(&AccessContext::system(), &self.job.id, &l.lease, report)
            .await;
        self.observe(hb)
    }

    /// Saves a checkpoint; the next attempt starts from it. Also renews the lease.
    pub async fn save_checkpoint(&self, checkpoint: Json) -> Result<(), JobError> {
        let Some(l) = &self.live else {
            return Ok(());
        };
        let hb = l
            .service
            .save_checkpoint(&AccessContext::system(), &self.job.id, &l.lease, &checkpoint)
            .await;
        self.observe(hb)
    }

    /// Enqueues a child job: it joins this job's workflow and is cancelled
    /// with this job. To continue after children, enqueue a job that
    /// `depends_on` them.
    pub async fn spawn_child(&self, child: NewJob) -> Result<Job, JobError> {
        let Some(l) = &self.live else {
            return Err(JobError::state("detached", "a detached context cannot spawn jobs"));
        };
        let mut jobs = l
            .service
            .spawn_children(&AccessContext::system(), &self.job.id, &l.lease, vec![child])
            .await?;
        jobs.pop().ok_or_else(|| JobError::Internal("no child job".into()))
    }

    fn observe(&self, hb: Result<crate::model::Heartbeat, JobError>) -> Result<(), JobError> {
        let Some(l) = &self.live else { return Ok(()) };
        match hb {
            Ok(hb) => {
                if hb.cancel_requested {
                    let _ = l.cancel.send(true);
                }
                Ok(())
            }
            Err(e) => {
                if matches!(e, JobError::LeaseLost(_)) {
                    let _ = l.cancel.send(true);
                }
                Err(e)
            }
        }
    }
}

/// A job handler. Implemented for `async fn(JobContext) -> Result<Json, HandlerError>`
/// closures; implement it by hand for stateful handlers.
pub trait JobHandler: Send + Sync + 'static {
    /// Runs the job. The returned JSON is stored as the job's result.
    fn handle(&self, ctx: JobContext) -> BoxFuture<'static, Result<Json, HandlerError>>;
}

impl<F, Fut> JobHandler for F
where
    F: Fn(JobContext) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Json, HandlerError>> + Send + 'static,
{
    fn handle(&self, ctx: JobContext) -> BoxFuture<'static, Result<Json, HandlerError>> {
        Box::pin(self(ctx))
    }
}

/// Handlers by job kind. The in-process worker pool claims only the kinds
/// registered here, so other kinds stay for external workers.
///
/// Each kind has a [`KindAccess`] (FB-142): [`JobRegistry::register`] makes
/// it reserved (only the system and privileged callers may enqueue it),
/// [`JobRegistry::register_public`] open to every caller with
/// `engine:enqueue_job`, and [`JobRegistry::register_guarded`] open to the
/// callers a guard admits. The worker pool hands the access to its
/// [`JobService`] when it starts ([`JobService::install_kinds`]).
#[derive(Clone, Default)]
pub struct JobRegistry {
    handlers: BTreeMap<String, Arc<dyn JobHandler>>,
    access: BTreeMap<String, KindAccess>,
}

impl std::fmt::Debug for JobRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.handlers.keys()).finish()
    }
}

impl JobRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `handler` for `kind` (replacing an earlier one) as a
    /// reserved kind: only the system and privileged callers may enqueue it.
    #[must_use]
    pub fn register(self, kind: impl Into<String>, handler: impl JobHandler) -> Self {
        self.register_with(kind, handler, KindAccess::Reserved)
    }

    /// Registers `handler` for `kind` as a public kind: every caller allowed
    /// to enqueue jobs may enqueue it. The handler acts for whoever that is.
    #[must_use]
    pub fn register_public(self, kind: impl Into<String>, handler: impl JobHandler) -> Self {
        self.register_with(kind, handler, KindAccess::Public)
    }

    /// Registers `handler` for `kind` with `guard` checking every
    /// non-privileged enqueuer and payload before the job is stored.
    #[must_use]
    pub fn register_guarded(
        self,
        kind: impl Into<String>,
        handler: impl JobHandler,
        guard: impl EnqueueGuard,
    ) -> Self {
        self.register_with(kind, handler, KindAccess::Guarded(Arc::new(guard)))
    }

    /// Registers `handler` for `kind` with `access`.
    #[must_use]
    pub fn register_with(mut self, kind: impl Into<String>, handler: impl JobHandler, access: KindAccess) -> Self {
        let kind = kind.into();
        self.handlers.insert(kind.clone(), Arc::new(handler));
        self.access.insert(kind, access);
        self
    }

    /// Who may enqueue `kind` (`None`: not registered here).
    pub fn access(&self, kind: &str) -> Option<&KindAccess> {
        self.access.get(kind)
    }

    /// Every registered kind with its access.
    pub fn accesses(&self) -> impl Iterator<Item = (&String, &KindAccess)> {
        self.access.iter()
    }

    /// The handler for `kind`.
    pub fn get(&self, kind: &str) -> Option<Arc<dyn JobHandler>> {
        self.handlers.get(kind).cloned()
    }

    /// Registered kinds, sorted.
    pub fn kinds(&self) -> Vec<String> {
        self.handlers.keys().cloned().collect()
    }

    /// No handlers.
    pub fn is_empty(&self) -> bool {
        self.handlers.is_empty()
    }
}
