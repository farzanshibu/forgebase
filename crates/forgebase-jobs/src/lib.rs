//! ForgeBase durable jobs (FB-61, ADR-0007) and long-running tasks (FB-64, ADR-0023).
//!
//! A job is a row in the system database. Because every state change is a
//! committed transaction, a queued job survives a restart, and a job a worker
//! was running when the process died is handed out again once its lease
//! (visibility timeout) expires. A job completes once: `complete` presents the
//! lease token, and a second claimant invalidates the first lease.
//!
//! | Piece | Where |
//! |---|---|
//! | Enqueue, delay (`run_at`/`delay_ms`), priority, idempotency key | [`JobService::enqueue`] |
//! | Retries with exponential backoff and jitter, dead-letter queue | [`JobService::fail`], [`backoff_ms`] |
//! | Leases, heartbeats, re-claiming crashed workers' jobs | [`JobService::claim`], [`JobService::heartbeat`] |
//! | Cancellation (queued at once, running cooperatively) | [`JobService::cancel`] |
//! | Cron schedules (UTC, 5 or 6 fields) | [`Cron`], [`JobService::create_schedule`] |
//! | Handlers as a Rust trait registry | [`JobRegistry`], [`JobHandler`] |
//! | In-process worker pool with graceful shutdown | [`start_pool`] |
//! | External workers over HTTP, OpenAPI fragment | [`router`], [`extension`], [`openapi`] |
//! | Progress, streamed on realtime channel `jobs:<id>` | [`JobService::progress`], [`JobContext::progress`], [`JobService::attach_realtime`] |
//! | Who may watch a job channel | [`JobChannelAuthorizer`] |
//! | Checkpoints and resume | [`JobService::save_checkpoint`], [`JobContext::checkpoint`] |
//! | Per-job timeouts (seconds to days), lease renewal, cancel token | [`NewJob::timeout`], [`PoolConfig::heartbeat_interval`], [`CancelToken`] |
//! | Named queues: concurrency, rate limit, pause, defaults | [`JobService::set_queue`], [`QueueSettings`] |
//! | Workflows: chains, fan-out/in, child jobs, fail or compensate | [`JobService::create_workflow`], [`NewJob::depends_on`], [`JobContext::spawn_child`] |
//! | Large results in object storage | [`ResultStore`], [`JobService::result`] |
//! | Attempt history with timings | [`JobService::attempts`] |
//!
//! Every operation asks the data core's authorizer first
//! ([`forgebase_data::Action::ReadJobs`] and friends); the pool acts as the
//! system principal. This crate contains no authentication or policy code.
//!
//! Tenancy (FB-142, [`JobAccess`]): jobs carry their enqueuer's tenant and
//! recorded context; non-privileged callers see only their own jobs of their
//! tenant (every job of it with [`READ_ALL_KEY`]); kinds are reserved,
//! public or guarded ([`KindAccess`]).

mod access;
mod clock;
mod cron;
mod error;
mod flow;
mod hooks;
mod model;
mod pool;
mod queues;
mod registry;
mod routes;
mod service;
mod tasks;

pub use access::{
    EnqueueGuard, JobAccess, KindAccess, PermissionCheck, READ_ALL_KEY, RESERVED_PREFIXES, RecordedContext,
};
pub use clock::{Clock, ManualClock, SystemClock};
pub use cron::{Cron, CronError};
pub use error::JobError;
pub use hooks::{
    CHANNEL_PREFIX, JobChannelAuthorizer, JobEventSink, MemoryResultStore, MemorySink, RealtimeSink, ResultStore,
    StoreError, channel,
};
pub use model::{
    ClaimRequest, ClaimedJob, Heartbeat, Job, JobEvent, JobFilter, JobPage, JobStats, JobStatus, NewJob, NewSchedule,
    NewWorker, QueueStats, Schedule, SchedulePatch, Worker, backoff_ms, iso,
};
pub use pool::{PoolConfig, PoolHandle, start_pool};
pub use registry::{CancelToken, HandlerError, JobContext, JobHandler, JobRegistry};
pub use routes::{extension, openapi, router};
pub use service::{JobService, now_ms};
pub use tasks::{
    Attempt, Checkpoint, Compensation, FailurePolicy, JobUpdate, MAX_CHECKPOINT_BYTES, MAX_INLINE_RESULT_BYTES,
    MAX_RESULT_BYTES, MAX_TIMEOUT_MS, MIN_TIMEOUT_MS, NewStep, NewWorkflow, ParentResult, Progress, ProgressReport,
    QueueInfo, QueueSettings, RatePeriod, Workflow, WorkflowDetail, WorkflowPage, WorkflowStatus, WorkflowStep,
    WorkflowUpdate,
};
