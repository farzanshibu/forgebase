//! Errors of the credit service.

use forgebase_core::{ErrorCode, ErrorKind};
use forgebase_db::DbError;
use forgebase_entitlements::EntitlementError;
use forgebase_policy::PolicyError;

/// Errors of [`crate::CreditService`].
#[derive(Debug, thiserror::Error)]
pub enum CreditError {
    /// Storage failure.
    #[error(transparent)]
    Db(#[from] DbError),
    /// The authorization pipeline refused the caller.
    #[error(transparent)]
    Policy(#[from] PolicyError),
    /// Reading the usage feed failed.
    #[error(transparent)]
    Usage(#[from] EntitlementError),
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
    /// request, a refund larger than what is left to refund).
    #[error("{message}")]
    Conflict {
        /// Stable code.
        code: &'static str,
        /// Explanation.
        message: String,
    },
    /// The balance does not cover the debit (HTTP 402).
    #[error("{subject} has {balance} credits, {requested} needed")]
    InsufficientCredits {
        /// Subject.
        subject: String,
        /// Balance now.
        balance: i64,
        /// Credits asked for.
        requested: i64,
    },
    /// Bug or corrupt data.
    #[error("internal error: {0}")]
    Internal(String),
}

impl CreditError {
    pub(crate) fn invalid(code: &'static str, message: impl Into<String>) -> Self {
        CreditError::Invalid {
            code,
            message: message.into(),
        }
    }

    pub(crate) fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        CreditError::Conflict {
            code,
            message: message.into(),
        }
    }
}

impl From<serde_json::Error> for CreditError {
    fn from(e: serde_json::Error) -> Self {
        CreditError::Internal(format!("json: {e}"))
    }
}

impl ErrorCode for CreditError {
    fn code(&self) -> &'static str {
        match self {
            CreditError::Db(e) => e.code(),
            CreditError::Policy(e) => e.code(),
            CreditError::Usage(e) => e.code(),
            CreditError::Unauthenticated => "unauthenticated",
            CreditError::Forbidden(_) => "forbidden",
            CreditError::Invalid { code, .. } | CreditError::Conflict { code, .. } => code,
            CreditError::NotFound(_) => "not_found",
            CreditError::InsufficientCredits { .. } => "insufficient_credits",
            CreditError::Internal(_) => "internal_error",
        }
    }

    fn kind(&self) -> ErrorKind {
        match self {
            CreditError::Db(e) => e.kind(),
            CreditError::Policy(e) => e.kind(),
            CreditError::Usage(e) => e.kind(),
            CreditError::Unauthenticated => ErrorKind::Unauthenticated,
            CreditError::Forbidden(_) => ErrorKind::Forbidden,
            CreditError::Invalid { .. } => ErrorKind::InvalidInput,
            CreditError::NotFound(_) => ErrorKind::NotFound,
            CreditError::Conflict { .. } | CreditError::InsufficientCredits { .. } => ErrorKind::Conflict,
            CreditError::Internal(_) => ErrorKind::Internal,
        }
    }
}

/// Result alias.
pub type Result<T, E = CreditError> = std::result::Result<T, E>;
