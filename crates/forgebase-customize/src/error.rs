//! Errors of the customization service.

use forgebase_core::{ErrorCode, ErrorKind};
use forgebase_db::DbError;
use forgebase_policy::PolicyError;

/// Errors of [`crate::CustomizeService`].
#[derive(Debug, thiserror::Error)]
pub enum CustomizeError {
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
    /// Conflicts with stored state.
    #[error("{message}")]
    Conflict {
        /// Stable code.
        code: &'static str,
        /// Explanation.
        message: String,
    },
    /// Bug or corrupt data.
    #[error("internal error: {0}")]
    Internal(String),
}

impl CustomizeError {
    pub(crate) fn invalid(code: &'static str, message: impl Into<String>) -> Self {
        CustomizeError::Invalid {
            code,
            message: message.into(),
        }
    }

    pub(crate) fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        CustomizeError::Conflict {
            code,
            message: message.into(),
        }
    }
}

impl From<serde_json::Error> for CustomizeError {
    fn from(e: serde_json::Error) -> Self {
        CustomizeError::Internal(format!("json: {e}"))
    }
}

impl ErrorCode for CustomizeError {
    fn code(&self) -> &'static str {
        match self {
            CustomizeError::Db(e) => e.code(),
            CustomizeError::Policy(e) => e.code(),
            CustomizeError::Unauthenticated => "unauthenticated",
            CustomizeError::Forbidden(_) => "forbidden",
            CustomizeError::Invalid { code, .. } | CustomizeError::Conflict { code, .. } => code,
            CustomizeError::NotFound(_) => "not_found",
            CustomizeError::Internal(_) => "internal_error",
        }
    }

    fn kind(&self) -> ErrorKind {
        match self {
            CustomizeError::Db(e) => e.kind(),
            CustomizeError::Policy(e) => e.kind(),
            CustomizeError::Unauthenticated => ErrorKind::Unauthenticated,
            CustomizeError::Forbidden(_) => ErrorKind::Forbidden,
            CustomizeError::Invalid { .. } => ErrorKind::InvalidInput,
            CustomizeError::NotFound(_) => ErrorKind::NotFound,
            CustomizeError::Conflict { .. } => ErrorKind::Conflict,
            CustomizeError::Internal(_) => ErrorKind::Internal,
        }
    }
}

/// Result alias.
pub type Result<T, E = CustomizeError> = std::result::Result<T, E>;
