//! Errors with stable codes.

use forgebase_core::{ErrorCode, ErrorKind};

/// What went wrong.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AlertError {
    /// No credentials.
    #[error("sign in as a platform administrator")]
    Unauthenticated,
    /// Not a platform administrator.
    #[error("platform administrators only")]
    Forbidden,
    /// Bad input: `(code, message)`.
    #[error("{1}")]
    Invalid(&'static str, String),
    /// Unknown object: `(code, message)`.
    #[error("{1}")]
    NotFound(&'static str, String),
    /// A state conflict: `(code, message)`.
    #[error("{1}")]
    Conflict(&'static str, String),
    /// Too many requests.
    #[error("{0}")]
    RateLimited(String),
    /// Database or other internal failure.
    #[error("{0}")]
    Internal(String),
}

impl AlertError {
    /// `400` with `code`.
    pub fn invalid(code: &'static str, message: impl Into<String>) -> Self {
        Self::Invalid(code, message.into())
    }

    /// `404` with `code`.
    pub fn not_found(code: &'static str, message: impl Into<String>) -> Self {
        Self::NotFound(code, message.into())
    }

    /// `409` with `code`.
    pub fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        Self::Conflict(code, message.into())
    }
}

impl ErrorCode for AlertError {
    fn code(&self) -> &'static str {
        match self {
            AlertError::Unauthenticated => "unauthenticated",
            AlertError::Forbidden => "forbidden",
            AlertError::Invalid(c, _) | AlertError::NotFound(c, _) | AlertError::Conflict(c, _) => c,
            AlertError::RateLimited(_) => "rate_limited",
            AlertError::Internal(_) => "internal",
        }
    }

    fn kind(&self) -> ErrorKind {
        match self {
            AlertError::Unauthenticated => ErrorKind::Unauthenticated,
            AlertError::Forbidden => ErrorKind::Forbidden,
            AlertError::Invalid(..) => ErrorKind::InvalidInput,
            AlertError::NotFound(..) => ErrorKind::NotFound,
            AlertError::Conflict(..) => ErrorKind::Conflict,
            // The HTTP layer answers 429 for this code.
            AlertError::RateLimited(_) => ErrorKind::Conflict,
            AlertError::Internal(_) => ErrorKind::Internal,
        }
    }
}

impl From<forgebase_db::DbError> for AlertError {
    fn from(e: forgebase_db::DbError) -> Self {
        AlertError::Internal(e.to_string())
    }
}

/// Result alias.
pub type Result<T> = std::result::Result<T, AlertError>;
