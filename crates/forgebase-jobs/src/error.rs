//! Errors of the job queue.

use forgebase_core::{ErrorCode, ErrorKind};
use forgebase_data::DataError;
use forgebase_db::{DbError, RouterError};

/// Errors of [`crate::JobService`].
#[derive(Debug, thiserror::Error)]
pub enum JobError {
    /// Authorisation failed (anonymous, or not allowed).
    #[error(transparent)]
    Access(#[from] DataError),
    /// Storage failure.
    #[error(transparent)]
    Db(#[from] DbError),
    /// The tenant's database set cannot serve the call now (ADR-0029 §3.5,
    /// FB-172): `tenant_moving` (409), `shard_unavailable` /
    /// `shard_migrating` (503).
    #[error(transparent)]
    Router(#[from] RouterError),
    /// The request is not acceptable.
    #[error("{message}")]
    Invalid {
        /// Stable code.
        code: &'static str,
        /// Explanation.
        message: String,
    },
    /// No such job.
    #[error("job {0:?} not found")]
    NotFound(String),
    /// No such schedule.
    #[error("schedule {0:?} not found")]
    ScheduleNotFound(String),
    /// A schedule with this name exists.
    #[error("schedule {0:?} already exists")]
    ScheduleExists(String),
    /// The lease is not (or no longer) held by the caller: the job finished,
    /// was claimed again after the visibility timeout, or the token is wrong.
    #[error("lease on job {0:?} is not held (expired, reassigned or job finished)")]
    LeaseLost(String),
    /// The job is in a state that does not allow the operation.
    #[error("{message}")]
    State {
        /// Stable code.
        code: &'static str,
        /// Explanation.
        message: String,
    },
    /// No such workflow.
    #[error("workflow {0:?} not found")]
    WorkflowNotFound(String),
    /// The result store (object storage) failed.
    #[error("job result store: {0}")]
    Store(String),
    /// Stored data could not be decoded (corrupt row or bug).
    #[error("internal job error: {0}")]
    Internal(String),
}

impl JobError {
    pub(crate) fn invalid(code: &'static str, message: impl Into<String>) -> Self {
        JobError::Invalid {
            code,
            message: message.into(),
        }
    }

    pub(crate) fn state(code: &'static str, message: impl Into<String>) -> Self {
        JobError::State {
            code,
            message: message.into(),
        }
    }
}

impl ErrorCode for JobError {
    fn code(&self) -> &'static str {
        match self {
            JobError::Access(e) => e.code(),
            JobError::Db(e) => e.code(),
            JobError::Router(e) => e.code(),
            JobError::Invalid { code, .. } | JobError::State { code, .. } => code,
            JobError::NotFound(_) => "job_not_found",
            JobError::ScheduleNotFound(_) => "schedule_not_found",
            JobError::ScheduleExists(_) => "schedule_exists",
            JobError::LeaseLost(_) => "lease_lost",
            JobError::WorkflowNotFound(_) => "workflow_not_found",
            JobError::Store(_) => "result_store_error",
            JobError::Internal(_) => "internal_error",
        }
    }

    fn kind(&self) -> ErrorKind {
        match self {
            JobError::Access(e) => e.kind(),
            JobError::Db(e) => e.kind(),
            JobError::Router(e) => e.kind(),
            JobError::Invalid { .. } => ErrorKind::InvalidInput,
            JobError::NotFound(_) | JobError::ScheduleNotFound(_) | JobError::WorkflowNotFound(_) => {
                ErrorKind::NotFound
            }
            JobError::ScheduleExists(_) | JobError::LeaseLost(_) | JobError::State { .. } => ErrorKind::Conflict,
            JobError::Store(_) | JobError::Internal(_) => ErrorKind::Internal,
        }
    }
}
