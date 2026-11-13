//! Errors of the insights service.

use forgebase_core::{ErrorCode, ErrorKind};
use forgebase_data::DataError;
use forgebase_db::DbError;

/// Errors of [`crate::InsightsService`].
#[derive(Debug, thiserror::Error)]
pub enum InsightsError {
    /// Storage failure.
    #[error(transparent)]
    Db(#[from] DbError),
    /// Reading the audit trail failed.
    #[error(transparent)]
    Auth(#[from] forgebase_auth::AuthError),
    /// The caller may not read insights.
    #[error(transparent)]
    Access(#[from] DataError),
    /// Bad input.
    #[error("{message}")]
    Invalid {
        /// Stable code.
        code: &'static str,
        /// Explanation.
        message: String,
    },
    /// No such object.
    #[error("{0} not found")]
    NotFound(String),
    /// A module the request needs is switched off.
    #[error("{0}")]
    Unavailable(String),
    /// Bug or corrupt data.
    #[error("internal error: {0}")]
    Internal(String),
}

impl InsightsError {
    /// A bad-input error.
    pub fn invalid(code: &'static str, message: impl Into<String>) -> Self {
        Self::Invalid {
            code,
            message: message.into(),
        }
    }
}

impl ErrorCode for InsightsError {
    fn code(&self) -> &'static str {
        match self {
            Self::Db(e) => e.code(),
            Self::Auth(e) => e.code(),
            Self::Access(e) => e.code(),
            Self::Invalid { code, .. } => code,
            Self::NotFound(_) => "not_found",
            Self::Unavailable(_) => "unavailable",
            Self::Internal(_) => "internal_error",
        }
    }

    fn kind(&self) -> ErrorKind {
        match self {
            Self::Db(e) => e.kind(),
            Self::Auth(e) => e.kind(),
            Self::Access(e) => e.kind(),
            Self::Invalid { .. } => ErrorKind::InvalidInput,
            Self::NotFound(_) => ErrorKind::NotFound,
            Self::Unavailable(_) => ErrorKind::Unavailable,
            Self::Internal(_) => ErrorKind::Internal,
        }
    }
}

/// Result of insights operations.
pub type Result<T> = std::result::Result<T, InsightsError>;
