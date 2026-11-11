//! Types of the long-running task features (FB-64, ADR-0023): progress,
//! checkpoints, attempts, named queue settings, workflows and live updates.

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use utoipa::ToSchema;

use crate::JobError;
use crate::model::{JobStatus, NewJob, check_name};

/// Largest checkpoint (serialised JSON).
pub const MAX_CHECKPOINT_BYTES: usize = 256 * 1024;
/// Results up to this size (serialised JSON) are stored on the job row; larger
/// ones go to the attached [`crate::ResultStore`] (object storage).
pub const MAX_INLINE_RESULT_BYTES: usize = 256 * 1024;
/// Largest result kept in the result store.
pub const MAX_RESULT_BYTES: usize = 16 * 1024 * 1024;
/// Shortest per-attempt timeout.
pub const MIN_TIMEOUT_MS: i64 = 1_000;
/// Longest per-attempt timeout (30 days).
pub const MAX_TIMEOUT_MS: i64 = 30 * 86_400_000;
/// Most steps of one workflow.
pub const MAX_WORKFLOW_STEPS: usize = 500;
/// Most parents (dependencies) of one job.
pub const MAX_DEPENDENCIES: usize = 500;
/// Most child jobs one call spawns.
pub const MAX_CHILDREN: usize = 100;

pub(crate) fn check_timeout(what: &str, ms: i64) -> Result<(), JobError> {
    if (MIN_TIMEOUT_MS..=MAX_TIMEOUT_MS).contains(&ms) {
        Ok(())
    } else {
        Err(JobError::invalid(
            "invalid_job",
            format!("{what} must be between {MIN_TIMEOUT_MS} ms (1 s) and {MAX_TIMEOUT_MS} ms (30 days)"),
        ))
    }
}

/// The latest progress report of a job.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Progress {
    /// 0 to 100.
    pub percent: Option<f64>,
    /// Short name of the current phase, e.g. `download`.
    pub stage: Option<String>,
    /// Human-readable detail.
    pub message: Option<String>,
    /// When it was reported (RFC 3339).
    pub updated_at: String,
}

/// A progress report. Fields left out keep their previous value.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ProgressReport {
    /// 0 to 100.
    #[serde(default)]
    pub percent: Option<f64>,
    /// Phase name (at most 128 characters).
    #[serde(default)]
    pub stage: Option<String>,
    /// Detail (at most 2000 characters).
    #[serde(default)]
    pub message: Option<String>,
}

impl ProgressReport {
    /// A report of `percent` only.
    pub fn percent(percent: f64) -> Self {
        Self {
            percent: Some(percent),
            ..Self::default()
        }
    }

    /// Same report with a stage.
    #[must_use]
    pub fn stage(mut self, stage: impl Into<String>) -> Self {
        self.stage = Some(stage.into());
        self
    }

    /// Same report with a message.
    #[must_use]
    pub fn message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }

    pub(crate) fn validate(&self) -> Result<(), JobError> {
        if let Some(p) = self.percent
            && !(p.is_finite() && (0.0..=100.0).contains(&p))
        {
            return Err(JobError::invalid(
                "invalid_progress",
                "percent must be between 0 and 100",
            ));
        }
        if self.stage.as_ref().is_some_and(|s| s.chars().count() > 128) {
            return Err(JobError::invalid(
                "invalid_progress",
                "stage must be at most 128 characters",
            ));
        }
        if self.message.as_ref().is_some_and(|s| s.chars().count() > 2000) {
            return Err(JobError::invalid(
                "invalid_progress",
                "message must be at most 2000 characters",
            ));
        }
        Ok(())
    }
}

/// The checkpoint of a job.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Checkpoint {
    /// Job id.
    pub job_id: String,
    /// The saved state (`null` when none was saved).
    #[schema(value_type = Option<Object>)]
    pub checkpoint: Option<Json>,
    /// When it was saved (RFC 3339).
    pub saved_at: Option<String>,
}

/// One attempt of a job.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Attempt {
    /// Attempt number (1 for the first; a graceful release does not count).
    pub attempt: i64,
    /// Worker that ran it.
    pub worker_id: Option<String>,
    /// Claim time (RFC 3339).
    pub started_at: String,
    /// End time (RFC 3339); `null` while running.
    pub finished_at: Option<String>,
    /// Run time in milliseconds (so far, while running).
    pub duration_ms: i64,
    /// `succeeded`, `failed`, `retry`, `timed_out`, `lease_expired`,
    /// `released` or `cancelled`; `null` while running.
    pub outcome: Option<String>,
    /// Error of a failed attempt.
    pub error: Option<String>,
    /// The attempt started with a checkpoint of an earlier one.
    pub from_checkpoint: bool,
}

/// A parent (dependency) of a claimed job, with its result.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct ParentResult {
    /// Parent job id.
    pub id: String,
    /// Step name inside its workflow.
    pub step: Option<String>,
    /// Its state (`succeeded` for dependencies; a compensated step may differ).
    pub status: JobStatus,
    /// Its result when stored inline; `null` when it lives in object storage
    /// (`result_stored`) — fetch it with `GET /api/v1/jobs/{id}/result`.
    #[schema(value_type = Option<Object>)]
    pub result: Option<Json>,
    /// The result is in object storage.
    pub result_stored: bool,
}

/// Window of a queue rate limit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum RatePeriod {
    /// Claims per second.
    #[default]
    Second,
    /// Claims per minute.
    Minute,
}

impl RatePeriod {
    pub(crate) fn ms(self) -> i64 {
        match self {
            RatePeriod::Second => 1_000,
            RatePeriod::Minute => 60_000,
        }
    }

    pub(crate) fn from_ms(ms: i64) -> Self {
        if ms >= 60_000 {
            RatePeriod::Minute
        } else {
            RatePeriod::Second
        }
    }
}

/// Settings of a named queue (`PUT /api/v1/jobs/queues/{name}`). Limits left
/// out are removed; `paused` left out keeps its current value.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct QueueSettings {
    /// Most jobs of this queue running at once, over every worker (none: no limit).
    #[serde(default)]
    pub concurrency: Option<i64>,
    /// Most jobs started per `rate_period` (none: no limit).
    #[serde(default)]
    pub rate_limit: Option<i64>,
    /// `second` (default) or `minute`.
    #[serde(default)]
    pub rate_period: Option<RatePeriod>,
    /// Paused queues hand out no jobs (running jobs continue).
    #[serde(default)]
    pub paused: Option<bool>,
    /// Priority of jobs enqueued without one.
    #[serde(default)]
    pub default_priority: Option<i64>,
    /// Per-attempt timeout of jobs enqueued without one, in ms.
    #[serde(default)]
    pub default_timeout_ms: Option<i64>,
}

impl QueueSettings {
    pub(crate) fn validate(&self) -> Result<(), JobError> {
        let bad = |m: &str| Err(JobError::invalid("invalid_queue", m.to_owned()));
        if self.concurrency.is_some_and(|c| !(1..=100_000).contains(&c)) {
            return bad("concurrency must be between 1 and 100000");
        }
        if self.rate_limit.is_some_and(|c| !(1..=1_000_000).contains(&c)) {
            return bad("rate_limit must be between 1 and 1000000");
        }
        if self.default_priority.is_some_and(|p| !(-1000..=1000).contains(&p)) {
            return bad("default_priority must be between -1000 and 1000");
        }
        if let Some(t) = self.default_timeout_ms {
            check_timeout("default_timeout_ms", t).map_err(|_| {
                JobError::invalid(
                    "invalid_queue",
                    format!("default_timeout_ms must be between {MIN_TIMEOUT_MS} and {MAX_TIMEOUT_MS}"),
                )
            })?;
        }
        Ok(())
    }
}

/// A named queue: its settings and current counts.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct QueueInfo {
    /// Queue name.
    pub name: String,
    /// Tenant whose jobs these settings govern (FB-146); `null`: the jobs
    /// without a tenant.
    pub tenant: Option<String>,
    /// Settings were saved for it (otherwise it only has jobs).
    pub configured: bool,
    /// Most jobs running at once over every worker.
    pub concurrency: Option<i64>,
    /// Most jobs started per `rate_period`.
    pub rate_limit: Option<i64>,
    /// `second` or `minute`.
    pub rate_period: RatePeriod,
    /// Hands out no jobs while paused.
    pub paused: bool,
    /// Priority of jobs enqueued without one.
    pub default_priority: Option<i64>,
    /// Per-attempt timeout of jobs enqueued without one, in ms.
    pub default_timeout_ms: Option<i64>,
    /// Waiting jobs that may run (due or delayed).
    pub queued: i64,
    /// Waiting for parents (dependencies) to succeed.
    pub waiting: i64,
    /// Leased by a worker.
    pub running: i64,
    /// Finished successfully (until purged).
    pub succeeded: i64,
    /// In the dead-letter queue.
    pub dead: i64,
    /// Cancelled (until purged).
    pub cancelled: i64,
    /// Who saved the settings first (FB-146); `null`: not configured, or
    /// configured before FB-146.
    pub created_by: Option<String>,
    /// Created (RFC 3339) when configured.
    pub created_at: Option<String>,
    /// Last settings change (RFC 3339) when configured.
    pub updated_at: Option<String>,
}

/// What a failed workflow step does to its workflow.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum FailurePolicy {
    /// Cancel the rest; the workflow ends `failed`.
    #[default]
    Fail,
    /// Cancel the rest, then run the `compensate` job of every succeeded step,
    /// newest first; the workflow ends `compensated` (or `failed` if a
    /// compensation fails).
    Compensate,
}

impl FailurePolicy {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            FailurePolicy::Fail => "fail",
            FailurePolicy::Compensate => "compensate",
        }
    }

    pub(crate) fn parse(s: &str) -> Self {
        if s == "compensate" {
            FailurePolicy::Compensate
        } else {
            FailurePolicy::Fail
        }
    }
}

/// State of a workflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum WorkflowStatus {
    /// Steps are waiting or running.
    Running,
    /// Every step succeeded.
    Succeeded,
    /// A step failed (or a compensation failed).
    Failed,
    /// A step failed; compensations are running.
    Compensating,
    /// A step failed and every compensation succeeded.
    Compensated,
    /// Cancelled by a caller.
    Cancelled,
}

impl WorkflowStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            WorkflowStatus::Running => "running",
            WorkflowStatus::Succeeded => "succeeded",
            WorkflowStatus::Failed => "failed",
            WorkflowStatus::Compensating => "compensating",
            WorkflowStatus::Compensated => "compensated",
            WorkflowStatus::Cancelled => "cancelled",
        }
    }

    pub(crate) fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "running" => WorkflowStatus::Running,
            "succeeded" => WorkflowStatus::Succeeded,
            "failed" => WorkflowStatus::Failed,
            "compensating" => WorkflowStatus::Compensating,
            "compensated" => WorkflowStatus::Compensated,
            "cancelled" => WorkflowStatus::Cancelled,
            _ => return None,
        })
    }

    /// Finished for good.
    pub fn is_terminal(self) -> bool {
        !matches!(self, WorkflowStatus::Running | WorkflowStatus::Compensating)
    }
}

/// The job that undoes a step (saga compensation).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Compensation {
    /// Handler name.
    pub kind: String,
    /// Handler input (default `null`).
    #[serde(default)]
    #[schema(value_type = Object)]
    pub payload: Json,
    /// Queue (default: the step's queue).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue: Option<String>,
}

/// One step of a workflow to create.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewStep {
    /// Unique name inside the workflow.
    pub name: String,
    /// Handler name.
    pub kind: String,
    /// Handler input (default `null`).
    #[serde(default)]
    #[schema(value_type = Object)]
    pub payload: Json,
    /// Names of steps that must succeed first (fan-in: all of them).
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// Queue (default `default`).
    #[serde(default)]
    pub queue: Option<String>,
    /// Higher runs first.
    #[serde(default)]
    pub priority: Option<i64>,
    /// Attempts before the step is dead-lettered (default 5).
    #[serde(default)]
    pub max_attempts: Option<i64>,
    /// First retry delay in ms.
    #[serde(default)]
    pub backoff_base_ms: Option<i64>,
    /// Cap of the retry delay in ms.
    #[serde(default)]
    pub backoff_max_ms: Option<i64>,
    /// Lease length without a heartbeat, in ms.
    #[serde(default)]
    pub visibility_timeout_ms: Option<i64>,
    /// Longest run of one attempt, in ms (1 s to 30 days).
    #[serde(default)]
    pub timeout_ms: Option<i64>,
    /// Run when the workflow fails after this step succeeded (`on_failure: compensate`).
    #[serde(default)]
    pub compensate: Option<Compensation>,
}

impl NewStep {
    /// A step running `kind` with `payload`.
    pub fn new(name: impl Into<String>, kind: impl Into<String>, payload: Json) -> Self {
        Self {
            name: name.into(),
            kind: kind.into(),
            payload,
            ..Self::default()
        }
    }

    /// Same step, after `steps` succeeded.
    #[must_use]
    pub fn after<I, S>(mut self, steps: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.depends_on.extend(steps.into_iter().map(Into::into));
        self
    }

    /// Same step with a compensation.
    #[must_use]
    pub fn compensate(mut self, kind: impl Into<String>, payload: Json) -> Self {
        self.compensate = Some(Compensation {
            kind: kind.into(),
            payload,
            queue: None,
        });
        self
    }

    pub(crate) fn job(&self) -> NewJob {
        NewJob {
            kind: self.kind.clone(),
            payload: self.payload.clone(),
            queue: self.queue.clone(),
            priority: self.priority,
            max_attempts: self.max_attempts,
            backoff_base_ms: self.backoff_base_ms,
            backoff_max_ms: self.backoff_max_ms,
            visibility_timeout_ms: self.visibility_timeout_ms,
            timeout_ms: self.timeout_ms,
            ..NewJob::default()
        }
    }
}

/// A workflow to create: a graph of steps. A chain is steps that each depend
/// on the one before; fan-out is several steps depending on one; fan-in is
/// one step depending on several (it waits for all).
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewWorkflow {
    /// Display name.
    pub name: String,
    /// `fail` (default) or `compensate`.
    #[serde(default)]
    pub on_failure: Option<FailurePolicy>,
    /// The steps (1 to 500).
    pub steps: Vec<NewStep>,
}

impl NewWorkflow {
    /// A workflow of `steps`.
    pub fn new(name: impl Into<String>, steps: Vec<NewStep>) -> Self {
        Self {
            name: name.into(),
            on_failure: None,
            steps,
        }
    }

    /// Same workflow with a failure policy.
    #[must_use]
    pub fn on_failure(mut self, policy: FailurePolicy) -> Self {
        self.on_failure = Some(policy);
        self
    }

    /// Checks names and returns the steps in an order where every step comes
    /// after its dependencies (a cycle is an error).
    pub(crate) fn order(&self) -> Result<Vec<usize>, JobError> {
        if self.name.is_empty() || self.name.chars().count() > 128 {
            return Err(JobError::invalid("invalid_workflow", "name must be 1-128 characters"));
        }
        if self.steps.is_empty() || self.steps.len() > MAX_WORKFLOW_STEPS {
            return Err(JobError::invalid(
                "invalid_workflow",
                format!("a workflow has 1 to {MAX_WORKFLOW_STEPS} steps"),
            ));
        }
        let mut index = std::collections::HashMap::new();
        for (i, s) in self.steps.iter().enumerate() {
            check_name("step name", &s.name, 128).map_err(|e| JobError::invalid("invalid_workflow", e.to_string()))?;
            if index.insert(s.name.as_str(), i).is_some() {
                return Err(JobError::invalid(
                    "invalid_workflow",
                    format!("step {:?} appears twice", s.name),
                ));
            }
        }
        let mut indegree = vec![0usize; self.steps.len()];
        let mut children: Vec<Vec<usize>> = vec![Vec::new(); self.steps.len()];
        for (i, s) in self.steps.iter().enumerate() {
            let mut seen = std::collections::HashSet::new();
            for d in &s.depends_on {
                let Some(&p) = index.get(d.as_str()) else {
                    return Err(JobError::invalid(
                        "invalid_workflow",
                        format!("step {:?} depends on unknown step {d:?}", s.name),
                    ));
                };
                if seen.insert(p) {
                    indegree[i] += 1;
                    children[p].push(i);
                }
            }
        }
        let mut ready: Vec<usize> = (0..self.steps.len()).filter(|i| indegree[*i] == 0).rev().collect();
        let mut order = Vec::with_capacity(self.steps.len());
        while let Some(i) = ready.pop() {
            order.push(i);
            for &c in children[i].iter().rev() {
                indegree[c] -= 1;
                if indegree[c] == 0 {
                    ready.push(c);
                }
            }
        }
        if order.len() != self.steps.len() {
            return Err(JobError::invalid(
                "invalid_workflow",
                "the steps' dependencies form a cycle",
            ));
        }
        Ok(order)
    }
}

/// A workflow.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Workflow {
    /// Workflow id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// State.
    pub status: WorkflowStatus,
    /// What a failed step does.
    pub on_failure: FailurePolicy,
    /// The job whose failure failed the workflow.
    pub failed_job_id: Option<String>,
    /// Why it failed.
    pub error: Option<String>,
    /// Who created it.
    pub created_by: String,
    /// Tenant of the creator (FB-142). `null`: none.
    pub tenant: Option<String>,
    /// Creation time (RFC 3339).
    pub created_at: String,
    /// Last change (RFC 3339).
    pub updated_at: String,
    /// End time (RFC 3339).
    pub finished_at: Option<String>,
}

/// A job of a workflow and the jobs it waits for.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct WorkflowStep {
    /// Step name (child jobs spawned by a step have none unless given).
    pub name: Option<String>,
    /// `step` or `compensation`.
    pub role: String,
    /// The job.
    pub job: crate::model::Job,
    /// Ids of the jobs it waits for.
    pub depends_on: Vec<String>,
}

/// A workflow with its jobs (the graph) and their timings.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct WorkflowDetail {
    /// The workflow.
    #[serde(flatten)]
    pub workflow: Workflow,
    /// Its jobs in creation order: steps, child jobs and compensations.
    pub steps: Vec<WorkflowStep>,
}

/// A page of workflows, newest first.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct WorkflowPage {
    /// The workflows.
    pub items: Vec<Workflow>,
    /// All workflows matching the filter.
    pub total: i64,
}

/// A live update of a job, published on the realtime channel `jobs:<job id>`
/// (and on `jobs:<workflow id>` for workflow jobs).
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct JobUpdate {
    /// `progress` (a progress report or checkpoint) or `status` (a state change).
    pub event: String,
    /// Job id.
    pub job_id: String,
    /// Queue.
    pub queue: String,
    /// Handler name.
    pub kind: String,
    /// State.
    pub status: JobStatus,
    /// Claims so far.
    pub attempts: i64,
    /// Latest progress.
    pub progress: Option<Progress>,
    /// A checkpoint is saved.
    pub has_checkpoint: bool,
    /// Cancellation was requested.
    pub cancel_requested: bool,
    /// Parents not finished yet.
    pub waiting_for: i64,
    /// Workflow.
    pub workflow_id: Option<String>,
    /// Step name.
    pub step: Option<String>,
    /// Last failure (first 500 characters).
    pub last_error: Option<String>,
    /// Last change (RFC 3339).
    pub updated_at: String,
    /// End time (RFC 3339).
    pub finished_at: Option<String>,
}

/// A live update of a workflow, published on `jobs:<workflow id>`.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct WorkflowUpdate {
    /// Always `workflow`.
    pub event: String,
    /// The workflow.
    pub workflow: Workflow,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn workflow_order_is_topological_and_rejects_cycles() {
        let wf = NewWorkflow::new(
            "w",
            vec![
                NewStep::new("join", "k", json!(null)).after(["a", "b"]),
                NewStep::new("a", "k", json!(null)).after(["root"]),
                NewStep::new("b", "k", json!(null)).after(["root"]),
                NewStep::new("root", "k", json!(null)),
            ],
        );
        let order = wf.order().unwrap();
        let pos = |name: &str| order.iter().position(|i| wf.steps[*i].name == name).unwrap();
        assert!(pos("root") < pos("a") && pos("root") < pos("b"));
        assert!(pos("a") < pos("join") && pos("b") < pos("join"));

        let cyclic = NewWorkflow::new(
            "w",
            vec![
                NewStep::new("a", "k", json!(null)).after(["b"]),
                NewStep::new("b", "k", json!(null)).after(["a"]),
            ],
        );
        assert!(cyclic.order().is_err());
        let unknown = NewWorkflow::new("w", vec![NewStep::new("a", "k", json!(null)).after(["zz"])]);
        assert!(unknown.order().is_err());
        let dup = NewWorkflow::new(
            "w",
            vec![NewStep::new("a", "k", json!(null)), NewStep::new("a", "k", json!(null))],
        );
        assert!(dup.order().is_err());
        assert!(NewWorkflow::new("w", vec![]).order().is_err());
    }

    #[test]
    fn progress_and_queue_validation() {
        assert!(ProgressReport::percent(50.0).stage("x").validate().is_ok());
        assert!(ProgressReport::percent(101.0).validate().is_err());
        assert!(ProgressReport::percent(f64::NAN).validate().is_err());
        assert!(ProgressReport::default().stage("s".repeat(129)).validate().is_err());
        assert!(
            QueueSettings {
                concurrency: Some(0),
                ..QueueSettings::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            QueueSettings {
                default_timeout_ms: Some(10),
                ..QueueSettings::default()
            }
            .validate()
            .is_err()
        );
        assert!(check_timeout("t", 86_400_000).is_ok());
        assert!(check_timeout("t", MAX_TIMEOUT_MS + 1).is_err());
        assert_eq!(RatePeriod::from_ms(RatePeriod::Minute.ms()), RatePeriod::Minute);
    }
}
