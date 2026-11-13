//! Errors of the queue service.

use forgebase_core::{ErrorCode, ErrorKind};
use forgebase_data::DataError;
use forgebase_db::{DbError, RouterError};
use forgebase_entitlements::EntitlementError;

/// Errors of [`crate::QueueService`].
#[derive(Debug, thiserror::Error)]
pub enum QueueError {
    /// Authorisation failed (anonymous, or not allowed).
    #[error(transparent)]
    Access(#[from] DataError),
    /// Storage failure.
    #[error(transparent)]
    Db(#[from] DbError),
    /// The database set of the topic or subscription cannot serve the call
    /// now (FB-172): `tenant_moving` (409), `shard_unavailable` /
    /// `shard_migrating` (503).
    #[error(transparent)]
    Router(#[from] RouterError),
    /// The quota engine refused the usage (`quota_exceeded`, ...).
    #[error(transparent)]
    Entitlement(#[from] EntitlementError),
    /// The request is not acceptable.
    #[error("{message}")]
    Invalid {
        /// Stable code.
        code: &'static str,
        /// Explanation.
        message: String,
    },
    /// No such topic.
    #[error("topic {0:?} not found")]
    TopicNotFound(String),
    /// No such subscription.
    #[error("subscription {0:?} not found")]
    SubscriptionNotFound(String),
    /// No such snapshot.
    #[error("snapshot {0:?} not found")]
    SnapshotNotFound(String),
    /// No such push attempt of the subscription.
    #[error("push attempt {0} not found")]
    AttemptNotFound(i64),
    /// The name is taken.
    #[error("{kind} {name:?} already exists")]
    Exists {
        /// `topic`, `subscription` or `snapshot`.
        kind: &'static str,
        /// The name.
        name: String,
    },
    /// The operation conflicts with the current state.
    #[error("{message}")]
    Conflict {
        /// Stable code.
        code: &'static str,
        /// Explanation.
        message: String,
    },
    /// A message is larger than its topic allows.
    #[error("{0}")]
    TooLarge(String),
    /// Too many calls of a rate-limited operation (test sends); HTTP 429.
    #[error("{message}")]
    RateLimited {
        /// Explanation.
        message: String,
        /// Seconds until a call is accepted again.
        retry_after_secs: u64,
    },
    /// Stored data could not be decoded (corrupt row or bug).
    #[error("internal queue error: {0}")]
    Internal(String),
}

impl QueueError {
    pub(crate) fn invalid(code: &'static str, message: impl Into<String>) -> Self {
        QueueError::Invalid {
            code,
            message: message.into(),
        }
    }

    pub(crate) fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        QueueError::Conflict {
            code,
            message: message.into(),
        }
    }
}

impl ErrorCode for QueueError {
    fn code(&self) -> &'static str {
        match self {
            QueueError::Access(e) => e.code(),
            QueueError::Db(e) => e.code(),
            QueueError::Router(e) => e.code(),
            QueueError::Entitlement(e) => e.code(),
            QueueError::Invalid { code, .. } | QueueError::Conflict { code, .. } => code,
            QueueError::TopicNotFound(_) => "topic_not_found",
            QueueError::SubscriptionNotFound(_) => "subscription_not_found",
            QueueError::SnapshotNotFound(_) => "snapshot_not_found",
            QueueError::AttemptNotFound(_) => "attempt_not_found",
            QueueError::Exists { kind, .. } => match *kind {
                "topic" => "topic_exists",
                "subscription" => "subscription_exists",
                _ => "snapshot_exists",
            },
            QueueError::TooLarge(_) => "message_too_large",
            QueueError::RateLimited { .. } => "rate_limited",
            QueueError::Internal(_) => "internal_error",
        }
    }

    fn kind(&self) -> ErrorKind {
        match self {
            QueueError::Access(e) => e.kind(),
            QueueError::Db(e) => e.kind(),
            QueueError::Router(e) => e.kind(),
            QueueError::Entitlement(e) => e.kind(),
            // The HTTP layer answers `413` for TooLarge and `429` for RateLimited.
            QueueError::Invalid { .. } | QueueError::TooLarge(_) | QueueError::RateLimited { .. } => {
                ErrorKind::InvalidInput
            }
            QueueError::TopicNotFound(_)
            | QueueError::SubscriptionNotFound(_)
            | QueueError::SnapshotNotFound(_)
            | QueueError::AttemptNotFound(_) => ErrorKind::NotFound,
            QueueError::Exists { .. } | QueueError::Conflict { .. } => ErrorKind::Conflict,
            QueueError::Internal(_) => ErrorKind::Internal,
        }
    }
}
