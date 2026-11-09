//! Errors of the entitlement service.

use forgebase_core::{ErrorCode, ErrorKind};
use forgebase_db::DbError;
use forgebase_policy::PolicyError;

use crate::model::Decision;

/// Errors of [`crate::EntitlementService`].
#[derive(Debug, thiserror::Error)]
pub enum EntitlementError {
    /// Storage failure.
    #[error(transparent)]
    Db(#[from] DbError),
    /// The authorization pipeline refused the caller.
    #[error(transparent)]
    Policy(#[from] PolicyError),
    /// No credentials where a caller is required.
    #[error("authentication required")]
    Unauthenticated,
    /// The caller may not do this.
    #[error("{0}")]
    Forbidden(String),
    /// Bad input.
    #[error("{message}")]
    Invalid {
        /// Stable code.
        code: &'static str,
        /// Explanation.
        message: String,
    },
    /// No such object (or not visible to the caller).
    #[error("{0} not found")]
    NotFound(String),
    /// Conflicts with stored state (an idempotency key reused for another
    /// request, a duplicate key).
    #[error("{message}")]
    Conflict {
        /// Stable code.
        code: &'static str,
        /// Explanation.
        message: String,
    },
    /// The feature is not in the subject's plan (entitlement).
    #[error("feature {feature} is not included in the plan of {subject}")]
    FeatureDisabled {
        /// Feature key.
        feature: String,
        /// Subject.
        subject: String,
    },
    /// Not enough quota left (HTTP 429).
    #[error("quota exhausted for {}", .0.feature)]
    QuotaExceeded(Box<Decision>),
    /// Bug or corrupt data.
    #[error("internal error: {0}")]
    Internal(String),
}

impl EntitlementError {
    pub(crate) fn invalid(code: &'static str, message: impl Into<String>) -> Self {
        EntitlementError::Invalid {
            code,
            message: message.into(),
        }
    }

    pub(crate) fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        EntitlementError::Conflict {
            code,
            message: message.into(),
        }
    }
}

impl From<serde_json::Error> for EntitlementError {
    fn from(e: serde_json::Error) -> Self {
        EntitlementError::Internal(format!("json: {e}"))
    }
}

impl ErrorCode for EntitlementError {
    fn code(&self) -> &'static str {
        match self {
            EntitlementError::Db(e) => e.code(),
            EntitlementError::Policy(e) => e.code(),
            EntitlementError::Unauthenticated => "unauthenticated",
            EntitlementError::Forbidden(_) => "forbidden",
            EntitlementError::Invalid { code, .. } | EntitlementError::Conflict { code, .. } => code,
            EntitlementError::NotFound(_) => "not_found",
            EntitlementError::FeatureDisabled { .. } => "feature_not_entitled",
            EntitlementError::QuotaExceeded(_) => "quota_exceeded",
            EntitlementError::Internal(_) => "internal_error",
        }
    }

    fn kind(&self) -> ErrorKind {
        match self {
            EntitlementError::Db(e) => e.kind(),
            EntitlementError::Policy(e) => e.kind(),
            EntitlementError::Unauthenticated => ErrorKind::Unauthenticated,
            EntitlementError::Forbidden(_)
            | EntitlementError::FeatureDisabled { .. }
            | EntitlementError::QuotaExceeded(_) => ErrorKind::Forbidden,
            EntitlementError::Invalid { .. } => ErrorKind::InvalidInput,
            EntitlementError::NotFound(_) => ErrorKind::NotFound,
            EntitlementError::Conflict { .. } => ErrorKind::Conflict,
            EntitlementError::Internal(_) => ErrorKind::Internal,
        }
    }
}

/// Result alias.
pub type Result<T, E = EntitlementError> = std::result::Result<T, E>;
