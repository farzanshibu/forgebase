//! Job handlers: a Rust trait and a registry keyed by job `kind`.
//!
//! Wasm functions (FB-60) will plug in later by registering a handler that
//! calls into the runtime; nothing here knows about them.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;

use forgebase_db::BoxFuture;
use serde_json::Value as Json;
use tokio::sync::watch;

use crate::model::Job;

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

/// What a running handler gets.
#[derive(Debug, Clone)]
pub struct JobContext {
    job: Job,
    cancel: watch::Receiver<bool>,
}

impl JobContext {
    pub(crate) fn new(job: Job, cancel: watch::Receiver<bool>) -> Self {
        Self { job, cancel }
    }

    /// A context that is never cancelled (for calling handlers in tests).
    pub fn detached(job: Job) -> Self {
        let (tx, rx) = watch::channel(false);
        // Keep the sender alive inside the receiver's lifetime is not needed:
        // a closed channel simply never reports cancellation.
        drop(tx);
        Self { job, cancel: rx }
    }

    /// The job.
    pub fn job(&self) -> &Job {
        &self.job
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

    /// Whether the job was cancelled (or its lease was lost): stop working.
    pub fn is_cancelled(&self) -> bool {
        *self.cancel.borrow()
    }

    /// Resolves when the job is cancelled or its lease was lost. Never resolves
    /// otherwise; use it in `tokio::select!` beside the real work.
    pub async fn cancelled(&self) {
        let mut rx = self.cancel.clone();
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
#[derive(Clone, Default)]
pub struct JobRegistry {
    handlers: BTreeMap<String, Arc<dyn JobHandler>>,
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

    /// Registers `handler` for `kind` (replacing an earlier one).
    #[must_use]
    pub fn register(mut self, kind: impl Into<String>, handler: impl JobHandler) -> Self {
        self.handlers.insert(kind.into(), Arc::new(handler));
        self
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
