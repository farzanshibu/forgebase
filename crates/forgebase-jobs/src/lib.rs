//! ForgeBase durable jobs (FB-61, ADR-0007).
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
//!
//! Every operation asks the data core's authorizer first
//! ([`forgebase_data::Action::ReadJobs`] and friends); the pool acts as the
//! system principal. This crate contains no authentication or policy code.

mod clock;
mod cron;
mod error;
mod model;
mod pool;
mod registry;
mod routes;
mod service;

pub use clock::{Clock, ManualClock, SystemClock};
pub use cron::{Cron, CronError};
pub use error::JobError;
pub use model::{
    ClaimRequest, ClaimedJob, Heartbeat, Job, JobEvent, JobFilter, JobPage, JobStats, JobStatus, NewJob, NewSchedule,
    NewWorker, QueueStats, Schedule, SchedulePatch, Worker, backoff_ms, iso,
};
pub use pool::{PoolConfig, PoolHandle, start_pool};
pub use registry::{HandlerError, JobContext, JobHandler, JobRegistry};
pub use routes::{extension, openapi, router};
pub use service::{JobService, now_ms};
