//! Job, schedule and worker types, validation and the retry policy.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use time::OffsetDateTime;
use utoipa::ToSchema;

use crate::JobError;
use crate::tasks::{MAX_DEPENDENCIES, ParentResult, Progress, check_timeout};

/// Default attempts before a job is dead-lettered.
pub const DEFAULT_MAX_ATTEMPTS: i64 = 5;
/// Default first retry delay.
pub const DEFAULT_BACKOFF_BASE_MS: i64 = 1_000;
/// Default cap of the retry delay (one hour).
pub const DEFAULT_BACKOFF_MAX_MS: i64 = 3_600_000;
/// Default time a worker may hold a job without a heartbeat.
pub const DEFAULT_VISIBILITY_TIMEOUT_MS: i64 = 60_000;
/// Largest payload (serialised JSON) of one job.
pub const MAX_PAYLOAD_BYTES: usize = 256 * 1024;
/// Most jobs one claim call returns.
pub const MAX_CLAIM: i64 = 50;
/// Largest page of a listing.
pub const MAX_PAGE: i64 = 200;
/// A worker counts as online if it was seen this recently.
pub const WORKER_ONLINE_MS: i64 = 60_000;

/// Lifecycle state of a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    /// Waiting for its `run_at` and for a worker (also while waiting to retry).
    Queued,
    /// A worker holds the lease.
    Running,
    /// Finished successfully.
    Succeeded,
    /// Dead-letter queue: attempts exhausted or failed for good. `retry` re-queues it.
    Dead,
    /// Cancelled before it finished.
    Cancelled,
}

impl JobStatus {
    /// The stored text.
    pub fn as_str(self) -> &'static str {
        match self {
            JobStatus::Queued => "queued",
            JobStatus::Running => "running",
            JobStatus::Succeeded => "succeeded",
            JobStatus::Dead => "dead",
            JobStatus::Cancelled => "cancelled",
        }
    }

    pub(crate) fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "queued" => JobStatus::Queued,
            "running" => JobStatus::Running,
            "succeeded" => JobStatus::Succeeded,
            "dead" => JobStatus::Dead,
            "cancelled" => JobStatus::Cancelled,
            _ => return None,
        })
    }

    /// Finished for good (until retried).
    pub fn is_terminal(self) -> bool {
        matches!(self, JobStatus::Succeeded | JobStatus::Dead | JobStatus::Cancelled)
    }
}

/// A job. The lease token is never part of it (see [`ClaimedJob`]).
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Job {
    /// Job id.
    pub id: String,
    /// Queue name.
    pub queue: String,
    /// Handler name.
    pub kind: String,
    /// Handler input.
    #[schema(value_type = Object)]
    pub payload: Json,
    /// Higher runs first.
    pub priority: i64,
    /// State.
    pub status: JobStatus,
    /// Earliest start (RFC 3339); moves forward while waiting to retry.
    pub run_at: String,
    /// Claims so far (a lost lease counts).
    pub attempts: i64,
    /// Attempts before the job is dead-lettered.
    pub max_attempts: i64,
    /// First retry delay in milliseconds; doubles per attempt up to `backoff_max_ms`.
    pub backoff_base_ms: i64,
    /// Cap of the retry delay in milliseconds.
    pub backoff_max_ms: i64,
    /// Time in milliseconds a worker may hold the job without a heartbeat.
    pub visibility_timeout_ms: i64,
    /// Deduplication key, unique per queue.
    pub idempotency_key: Option<String>,
    /// Schedule that enqueued the job.
    pub schedule_id: Option<String>,
    /// Worker that holds (or held last) the job.
    pub worker_id: Option<String>,
    /// While running: when the lease expires without a heartbeat (RFC 3339).
    pub visibility_until: Option<String>,
    /// Cancellation was requested while running; workers should stop and `fail`.
    pub cancel_requested: bool,
    /// Handler output of a succeeded job.
    #[schema(value_type = Option<Object>)]
    pub result: Option<Json>,
    /// Message of the last failure.
    pub last_error: Option<String>,
    /// Who enqueued it.
    pub created_by: String,
    /// Tenant (project environment) of the enqueuer (FB-142); `null`: none.
    pub tenant: Option<String>,
    /// The enqueuer's recorded context (JSON, [`crate::RecordedContext`]);
    /// not part of the API. `None`: the system.
    #[serde(skip)]
    #[schema(ignore)]
    pub(crate) context: Option<String>,
    /// W3C `traceparent` of the span that enqueued the job (ADR-0035): the
    /// run continues that trace, and an external worker may too. Absent
    /// while trace propagation is off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub traceparent: Option<String>,
    /// Creation time (RFC 3339).
    pub created_at: String,
    /// Last change (RFC 3339).
    pub updated_at: String,
    /// Start of the latest attempt (RFC 3339).
    pub started_at: Option<String>,
    /// When it reached a terminal state (RFC 3339).
    pub finished_at: Option<String>,
    /// Latest progress report.
    pub progress: Option<Progress>,
    /// A checkpoint is saved (`GET /api/v1/jobs/{id}/checkpoint`).
    pub has_checkpoint: bool,
    /// When the checkpoint was saved (RFC 3339).
    pub checkpoint_at: Option<String>,
    /// Longest run of one attempt in ms (`null`: no limit).
    pub timeout_ms: Option<i64>,
    /// While running with a timeout: when the attempt times out (RFC 3339).
    pub deadline_at: Option<String>,
    /// Last heartbeat of the current attempt (RFC 3339).
    pub heartbeat_at: Option<String>,
    /// Workflow the job belongs to.
    pub workflow_id: Option<String>,
    /// Step name inside the workflow.
    pub step: Option<String>,
    /// The job that spawned this one (child jobs).
    pub parent_id: Option<String>,
    /// The step this job compensates (saga compensation).
    pub compensates: Option<String>,
    /// Parents (dependencies) that have not succeeded yet; the job is not
    /// claimed before this is 0.
    pub waiting_for: i64,
    /// The result is kept in object storage (`GET /api/v1/jobs/{id}/result`);
    /// `result` is `null` then.
    pub result_stored: bool,
    /// Size of the stored result in bytes.
    pub result_size: Option<i64>,
}

/// A claimed job with the lease that fences it.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct ClaimedJob {
    /// Secret token: `heartbeat`, `complete` and `fail` must present it.
    pub lease_id: String,
    /// The job (already `running`).
    pub job: Job,
    /// The last checkpoint of an earlier attempt: resume from it.
    #[schema(value_type = Option<Object>)]
    pub checkpoint: Option<Json>,
    /// The parents (dependencies) with their results.
    pub parents: Vec<ParentResult>,
}

/// Retry and visibility settings shared by jobs and schedules.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Policy {
    pub priority: i64,
    pub max_attempts: i64,
    pub backoff_base_ms: i64,
    pub backoff_max_ms: i64,
    pub visibility_timeout_ms: i64,
}

impl Policy {
    pub(crate) fn resolve(
        priority: Option<i64>,
        max_attempts: Option<i64>,
        backoff_base_ms: Option<i64>,
        backoff_max_ms: Option<i64>,
        visibility_timeout_ms: Option<i64>,
    ) -> Result<Self, JobError> {
        let p = Policy {
            priority: priority.unwrap_or(0),
            max_attempts: max_attempts.unwrap_or(DEFAULT_MAX_ATTEMPTS),
            backoff_base_ms: backoff_base_ms.unwrap_or(DEFAULT_BACKOFF_BASE_MS),
            backoff_max_ms: backoff_max_ms.unwrap_or(DEFAULT_BACKOFF_MAX_MS),
            visibility_timeout_ms: visibility_timeout_ms.unwrap_or(DEFAULT_VISIBILITY_TIMEOUT_MS),
        };
        range("priority", p.priority, -1000, 1000)?;
        range("max_attempts", p.max_attempts, 1, 100)?;
        range("backoff_base_ms", p.backoff_base_ms, 0, 86_400_000)?;
        range("backoff_max_ms", p.backoff_max_ms, 0, 7 * 86_400_000)?;
        range("visibility_timeout_ms", p.visibility_timeout_ms, 1, 86_400_000)?;
        if p.backoff_max_ms < p.backoff_base_ms {
            return Err(JobError::invalid(
                "invalid_retry_policy",
                "backoff_max_ms must be at least backoff_base_ms",
            ));
        }
        Ok(p)
    }
}

fn range(name: &str, v: i64, min: i64, max: i64) -> Result<(), JobError> {
    if (min..=max).contains(&v) {
        Ok(())
    } else {
        Err(JobError::invalid(
            "invalid_job",
            format!("{name} must be between {min} and {max}"),
        ))
    }
}

/// Checks a queue, kind, worker or schedule name: `[A-Za-z0-9._:-]`, at most `max` characters.
pub(crate) fn check_name(what: &str, value: &str, max: usize) -> Result<(), JobError> {
    let ok = !value.is_empty()
        && value.len() <= max
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'));
    if ok {
        Ok(())
    } else {
        Err(JobError::invalid(
            "invalid_job",
            format!("{what} must be 1-{max} characters of letters, digits, '.', '_', ':' or '-'"),
        ))
    }
}

pub(crate) fn check_payload(payload: &Json) -> Result<String, JobError> {
    let text = serde_json::to_string(payload).map_err(|e| JobError::invalid("invalid_job", e.to_string()))?;
    if text.len() > MAX_PAYLOAD_BYTES {
        return Err(JobError::invalid(
            "payload_too_large",
            format!("payload is larger than {MAX_PAYLOAD_BYTES} bytes"),
        ));
    }
    Ok(text)
}

/// A job to enqueue.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewJob {
    /// Handler name.
    pub kind: String,
    /// Handler input (default `null`).
    #[serde(default)]
    #[schema(value_type = Object)]
    pub payload: Json,
    /// Queue (default `default`).
    #[serde(default)]
    pub queue: Option<String>,
    /// Higher runs first (default 0, range -1000..=1000).
    #[serde(default)]
    pub priority: Option<i64>,
    /// Earliest start, RFC 3339. Mutually exclusive with `delay_ms`.
    #[serde(default)]
    pub run_at: Option<String>,
    /// Earliest start as a delay from now in milliseconds.
    #[serde(default)]
    pub delay_ms: Option<i64>,
    /// Attempts before dead-lettering (default 5).
    #[serde(default)]
    pub max_attempts: Option<i64>,
    /// First retry delay in ms (default 1000); doubles per attempt.
    #[serde(default)]
    pub backoff_base_ms: Option<i64>,
    /// Cap of the retry delay in ms (default 1 hour).
    #[serde(default)]
    pub backoff_max_ms: Option<i64>,
    /// Time in ms a worker may hold the job without a heartbeat (default 60000).
    #[serde(default)]
    pub visibility_timeout_ms: Option<i64>,
    /// Enqueueing the same key twice in a queue returns the first job.
    #[serde(default)]
    pub idempotency_key: Option<String>,
    /// Longest run of one attempt in ms (1 s to 30 days; default: the queue's,
    /// else none). A timed-out attempt is retried like a failure.
    #[serde(default)]
    pub timeout_ms: Option<i64>,
    /// Ids of jobs that must succeed first; the job waits for all of them.
    #[serde(default)]
    pub depends_on: Vec<String>,
}

impl NewJob {
    /// A job of `kind` with `payload` and default settings.
    pub fn new(kind: impl Into<String>, payload: Json) -> Self {
        Self {
            kind: kind.into(),
            payload,
            ..Self::default()
        }
    }

    /// Same job, on `queue`.
    #[must_use]
    pub fn queue(mut self, queue: impl Into<String>) -> Self {
        self.queue = Some(queue.into());
        self
    }

    /// Same job, starting after `delay`.
    #[must_use]
    pub fn delay(mut self, delay: Duration) -> Self {
        self.delay_ms = Some(i64::try_from(delay.as_millis()).unwrap_or(i64::MAX));
        self
    }

    /// Same job with a priority.
    #[must_use]
    pub fn priority(mut self, priority: i64) -> Self {
        self.priority = Some(priority);
        self
    }

    /// Same job with an idempotency key.
    #[must_use]
    pub fn idempotency_key(mut self, key: impl Into<String>) -> Self {
        self.idempotency_key = Some(key.into());
        self
    }

    /// Same job with a retry policy.
    #[must_use]
    pub fn retries(mut self, max_attempts: i64, backoff_base: Duration, backoff_max: Duration) -> Self {
        self.max_attempts = Some(max_attempts);
        self.backoff_base_ms = Some(i64::try_from(backoff_base.as_millis()).unwrap_or(i64::MAX));
        self.backoff_max_ms = Some(i64::try_from(backoff_max.as_millis()).unwrap_or(i64::MAX));
        self
    }

    /// Same job with a visibility timeout.
    #[must_use]
    pub fn visibility_timeout(mut self, timeout: Duration) -> Self {
        self.visibility_timeout_ms = Some(i64::try_from(timeout.as_millis()).unwrap_or(i64::MAX));
        self
    }

    /// Same job with a per-attempt timeout.
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout_ms = Some(i64::try_from(timeout.as_millis()).unwrap_or(i64::MAX));
        self
    }

    /// Same job, running after the jobs `ids` succeeded.
    #[must_use]
    pub fn depends_on<I, S>(mut self, ids: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.depends_on.extend(ids.into_iter().map(Into::into));
        self
    }
}

/// A validated [`NewJob`].
#[derive(Debug, Clone, Default)]
pub(crate) struct Resolved {
    pub queue: String,
    pub kind: String,
    pub payload: String,
    pub policy: Policy,
    /// The caller chose the priority (otherwise the queue default applies).
    pub priority_given: bool,
    pub run_at: i64,
    pub idempotency_key: Option<String>,
    pub timeout_ms: Option<i64>,
    pub depends_on: Vec<String>,
    pub workflow_id: Option<String>,
    pub step: Option<String>,
    pub parent_id: Option<String>,
    pub compensates: Option<String>,
    /// Serialised [`crate::tasks::Compensation`].
    pub compensate: Option<String>,
}

impl NewJob {
    pub(crate) fn resolve(&self, now: i64) -> Result<Resolved, JobError> {
        let queue = self.queue.clone().unwrap_or_else(|| "default".to_owned());
        check_name("queue", &queue, 64)?;
        check_name("kind", &self.kind, 128)?;
        if let Some(key) = &self.idempotency_key
            && (key.is_empty() || key.len() > 256)
        {
            return Err(JobError::invalid(
                "invalid_job",
                "idempotency_key must be 1-256 characters",
            ));
        }
        let run_at = match (&self.run_at, self.delay_ms) {
            (Some(_), Some(_)) => {
                return Err(JobError::invalid(
                    "invalid_job",
                    "give either run_at or delay_ms, not both",
                ));
            }
            (Some(text), None) => parse_rfc3339_ms(text)?,
            (None, Some(delay)) => {
                range("delay_ms", delay, 0, 366 * 86_400_000)?;
                now + delay
            }
            (None, None) => now,
        };
        Ok(Resolved {
            queue,
            kind: self.kind.clone(),
            payload: check_payload(&self.payload)?,
            policy: Policy::resolve(
                self.priority,
                self.max_attempts,
                self.backoff_base_ms,
                self.backoff_max_ms,
                self.visibility_timeout_ms,
            )?,
            priority_given: self.priority.is_some(),
            run_at,
            idempotency_key: self.idempotency_key.clone(),
            timeout_ms: match self.timeout_ms {
                Some(t) => {
                    check_timeout("timeout_ms", t)?;
                    Some(t)
                }
                None => None,
            },
            depends_on: {
                if self.depends_on.len() > MAX_DEPENDENCIES {
                    return Err(JobError::invalid(
                        "invalid_job",
                        format!("at most {MAX_DEPENDENCIES} dependencies"),
                    ));
                }
                let mut ids: Vec<String> = Vec::with_capacity(self.depends_on.len());
                for id in &self.depends_on {
                    check_name("depends_on id", id, 64)?;
                    if !ids.contains(id) {
                        ids.push(id.clone());
                    }
                }
                ids
            },
            workflow_id: None,
            step: None,
            parent_id: None,
            compensates: None,
            compensate: None,
        })
    }
}

pub(crate) fn parse_rfc3339_ms(text: &str) -> Result<i64, JobError> {
    let t = OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339)
        .map_err(|_| JobError::invalid("invalid_job", format!("run_at {text:?} is not an RFC 3339 timestamp")))?;
    i64::try_from(t.unix_timestamp_nanos() / 1_000_000)
        .map_err(|_| JobError::invalid("invalid_job", "run_at is out of range"))
}

/// Epoch milliseconds to RFC 3339 (UTC, millisecond precision).
pub fn iso(ms: i64) -> String {
    let t = OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000).unwrap_or(OffsetDateTime::UNIX_EPOCH);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute(),
        t.second(),
        t.millisecond()
    )
}

/// The delay before retrying after `attempt` failed attempts (1 for the
/// first failure): `base * 2^(attempt-1)` capped at `max`, then reduced by a
/// random 0-25% (`jitter` in `[0, 1)`) so retries of many jobs that failed
/// together do not all fire at once. Never exceeds `max`.
pub fn backoff_ms(base: i64, max: i64, attempt: i64, jitter: f64) -> i64 {
    let exp = u32::try_from((attempt - 1).clamp(0, 40)).unwrap_or(40);
    let raw = base.saturating_mul(1_i64 << exp).min(max).max(0);
    let jitter = jitter.clamp(0.0, 1.0);
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    let jittered = (raw as f64 * (1.0 - 0.25 * jitter)) as i64;
    jittered.clamp(0, max)
}

/// A uniform random number in `[0, 1)`.
pub(crate) fn jitter() -> f64 {
    let bits = (uuid::Uuid::new_v4().as_u128() >> 75) as u64; // 53 random bits
    #[allow(clippy::cast_precision_loss)]
    {
        bits as f64 / (1u64 << 53) as f64
    }
}

/// One entry of a job's history.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct JobEvent {
    /// Sequence number.
    pub id: i64,
    /// When (RFC 3339).
    pub at: String,
    /// `enqueued`, `claimed`, `succeeded`, `retry_scheduled`, `dead`,
    /// `cancel_requested`, `cancelled`, `lease_expired`, `released`, `requeued`.
    pub event: String,
    /// Attempt number at the time.
    pub attempt: i64,
    /// Worker involved.
    pub worker_id: Option<String>,
    /// Detail, e.g. the error message.
    pub message: Option<String>,
}

/// Filter of [`crate::JobService::list`].
#[derive(Debug, Clone, Default)]
pub struct JobFilter {
    /// Only this state (`dead` lists the dead-letter queue).
    pub status: Option<JobStatus>,
    /// Only this queue.
    pub queue: Option<String>,
    /// Only this handler.
    pub kind: Option<String>,
    /// Page size (default 50, at most 200).
    pub limit: Option<i64>,
    /// Rows to skip.
    pub offset: Option<i64>,
    /// Only jobs of this workflow.
    pub workflow_id: Option<String>,
    /// Only children of this job.
    pub parent_id: Option<String>,
}

/// A page of jobs, newest first.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct JobPage {
    /// The jobs.
    pub items: Vec<Job>,
    /// All jobs matching the filter.
    pub total: i64,
}

/// Counts for one queue.
#[derive(Debug, Clone, Default, Serialize, ToSchema)]
pub struct QueueStats {
    /// Queue name.
    pub queue: String,
    /// Waiting (including delayed and waiting to retry).
    pub queued: i64,
    /// Leased by a worker.
    pub running: i64,
    /// Finished successfully (until purged).
    pub succeeded: i64,
    /// In the dead-letter queue.
    pub dead: i64,
    /// Cancelled (until purged).
    pub cancelled: i64,
}

/// Queue statistics.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct JobStats {
    /// Counts over all queues by state.
    pub totals: BTreeMap<String, i64>,
    /// Counts per queue.
    pub queues: Vec<QueueStats>,
}

/// A recurring job definition.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Schedule {
    /// Schedule id.
    pub id: String,
    /// Unique name.
    pub name: String,
    /// Cron expression (UTC; 5 fields, or 6 with leading seconds).
    pub cron: String,
    /// Queue of the jobs.
    pub queue: String,
    /// Handler name.
    pub kind: String,
    /// Handler input.
    #[schema(value_type = Object)]
    pub payload: Json,
    /// Priority of the jobs.
    pub priority: i64,
    /// Attempts per job.
    pub max_attempts: i64,
    /// First retry delay in ms.
    pub backoff_base_ms: i64,
    /// Cap of the retry delay in ms.
    pub backoff_max_ms: i64,
    /// Visibility timeout of the jobs in ms.
    pub visibility_timeout_ms: i64,
    /// Fires only while enabled.
    pub enabled: bool,
    /// Next firing (RFC 3339). For a schedule pg_cron fires it is computed
    /// from the expression when read.
    pub next_run_at: String,
    /// What fires it: `forgebase` (the server's tick) or `pg_cron` (FB-165;
    /// PostgreSQL with the extension, expressions it reads like ForgeBase).
    pub fired_by: String,
    /// Last firing (RFC 3339).
    pub last_run_at: Option<String>,
    /// Job of the last firing.
    pub last_job_id: Option<String>,
    /// Who created it.
    pub created_by: String,
    /// Tenant of the creator (FB-142); its jobs run in it. `null`: none.
    pub tenant: Option<String>,
    /// Creation time (RFC 3339).
    pub created_at: String,
    /// Last change (RFC 3339).
    pub updated_at: String,
}

/// A schedule to create.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewSchedule {
    /// Unique name.
    pub name: String,
    /// Cron expression (UTC; 5 fields, or 6 with leading seconds).
    pub cron: String,
    /// Handler name.
    pub kind: String,
    /// Queue (default `default`).
    #[serde(default)]
    pub queue: Option<String>,
    /// Handler input (default `null`).
    #[serde(default)]
    #[schema(value_type = Object)]
    pub payload: Json,
    /// Priority (default 0).
    #[serde(default)]
    pub priority: Option<i64>,
    /// Attempts per job (default 5).
    #[serde(default)]
    pub max_attempts: Option<i64>,
    /// First retry delay in ms.
    #[serde(default)]
    pub backoff_base_ms: Option<i64>,
    /// Cap of the retry delay in ms.
    #[serde(default)]
    pub backoff_max_ms: Option<i64>,
    /// Visibility timeout in ms.
    #[serde(default)]
    pub visibility_timeout_ms: Option<i64>,
    /// Start enabled (default true).
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// Changes to a schedule.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SchedulePatch {
    /// New cron expression.
    #[serde(default)]
    pub cron: Option<String>,
    /// Enable or disable.
    #[serde(default)]
    pub enabled: Option<bool>,
    /// New payload.
    #[serde(default)]
    #[schema(value_type = Option<Object>)]
    pub payload: Option<Json>,
    /// New priority.
    #[serde(default)]
    pub priority: Option<i64>,
}

/// A worker that registered or claimed jobs.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Worker {
    /// Worker id.
    pub id: String,
    /// Display name.
    pub name: Option<String>,
    /// Queues it works (empty: all).
    pub queues: Vec<String>,
    /// Handlers it runs (empty: all).
    pub kinds: Vec<String>,
    /// Jobs it runs at once.
    pub concurrency: i64,
    /// First seen (RFC 3339).
    pub registered_at: String,
    /// Last claim, heartbeat or registration (RFC 3339).
    pub last_seen_at: String,
    /// Seen in the last minute.
    pub online: bool,
    /// Tenant of the caller that registered it (FB-146); `null`: none.
    pub tenant: Option<String>,
    /// Who registered it (FB-146); `null`: registered before FB-146.
    pub created_by: Option<String>,
}

/// A worker to register.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewWorker {
    /// Id to use; generated when absent.
    #[serde(default)]
    pub id: Option<String>,
    /// Display name.
    #[serde(default)]
    pub name: Option<String>,
    /// Queues it works (empty: all).
    #[serde(default)]
    pub queues: Vec<String>,
    /// Handlers it runs (empty: all).
    #[serde(default)]
    pub kinds: Vec<String>,
    /// Jobs it runs at once (default 1).
    #[serde(default)]
    pub concurrency: Option<i64>,
}

/// A claim request.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ClaimRequest {
    /// The claiming worker (any id of `[A-Za-z0-9._:-]`, at most 128 characters).
    pub worker_id: String,
    /// Only these queues (empty: all).
    #[serde(default)]
    pub queues: Vec<String>,
    /// Only these handlers (empty: all).
    #[serde(default)]
    pub kinds: Vec<String>,
    /// At most this many jobs (default 1, at most 50).
    #[serde(default)]
    pub max: Option<i64>,
}

/// Answer of a heartbeat.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Heartbeat {
    /// New lease expiry (RFC 3339).
    pub visibility_until: String,
    /// The job was cancelled: stop and call `fail`.
    pub cancel_requested: bool,
    /// When the attempt times out (RFC 3339), if the job has a timeout.
    pub deadline_at: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_caps_and_jitters_downwards() {
        // No jitter: base * 2^(attempt-1).
        assert_eq!(backoff_ms(1000, 3_600_000, 1, 0.0), 1000);
        assert_eq!(backoff_ms(1000, 3_600_000, 2, 0.0), 2000);
        assert_eq!(backoff_ms(1000, 3_600_000, 5, 0.0), 16_000);
        // Capped.
        assert_eq!(backoff_ms(1000, 10_000, 10, 0.0), 10_000);
        assert_eq!(backoff_ms(1000, 10_000, 1000, 0.0), 10_000);
        // Full jitter removes at most 25%.
        assert_eq!(backoff_ms(1000, 10_000, 3, 0.999_999), 3000);
        for i in 0..200 {
            let d = backoff_ms(1000, 10_000, 3, f64::from(i) / 200.0);
            assert!((3000..=4000).contains(&d), "{d}");
        }
        assert_eq!(backoff_ms(0, 0, 3, 0.5), 0);
        let j = jitter();
        assert!((0.0..1.0).contains(&j));
    }

    #[test]
    fn validation() {
        let now = 1_000_000;
        let ok = NewJob::new("send_email", serde_json::json!({"to": "a@b.c"}))
            .delay(Duration::from_secs(2))
            .resolve(now)
            .unwrap();
        assert_eq!(ok.run_at, now + 2000);
        assert_eq!(ok.queue, "default");
        for bad in [
            NewJob::new("", Json::Null),
            NewJob::new("has space", Json::Null),
            NewJob::new("k", Json::Null).queue("a/b"),
            NewJob::new("k", Json::Null).priority(5000),
            NewJob::new("k", Json::Null).retries(0, Duration::ZERO, Duration::ZERO),
            NewJob::new("k", Json::Null).retries(3, Duration::from_secs(10), Duration::from_secs(1)),
            NewJob::new("k", Json::String("x".repeat(MAX_PAYLOAD_BYTES))),
            NewJob {
                run_at: Some("2030-01-01T00:00:00Z".into()),
                delay_ms: Some(5),
                ..NewJob::new("k", Json::Null)
            },
            NewJob {
                run_at: Some("tomorrow".into()),
                ..NewJob::new("k", Json::Null)
            },
        ] {
            assert!(bad.resolve(now).is_err(), "{bad:?}");
        }
        assert_eq!(
            NewJob {
                run_at: Some("1970-01-01T00:00:01Z".into()),
                ..NewJob::new("k", Json::Null)
            }
            .resolve(now)
            .unwrap()
            .run_at,
            1000
        );
        assert_eq!(iso(0), "1970-01-01T00:00:00.000Z");
    }
}
