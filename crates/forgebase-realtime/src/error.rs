//! Realtime errors and their HTTP mapping.

use forgebase_core::{ErrorCode, ErrorKind};
use forgebase_data::db::DbError;
use forgebase_data::{AccessContext, DataError};

/// Errors of the realtime service.
#[derive(Debug, thiserror::Error)]
pub enum RealtimeError {
    /// The caller must authenticate (or the credential is no longer valid).
    #[error("{0}")]
    Unauthenticated(String),
    /// The caller is not allowed to do this.
    #[error("forbidden: {0}")]
    Forbidden(String),
    /// A connection limit was reached.
    #[error("{0}")]
    TooManyConnections(String),
    /// Too many messages in a short time.
    #[error("{0}")]
    RateLimited(String),
    /// The request or message is malformed or exceeds a limit.
    #[error("{message}")]
    Invalid {
        /// Stable code.
        code: &'static str,
        /// Explanation.
        message: String,
    },
    /// The data core refused or failed.
    #[error(transparent)]
    Data(#[from] DataError),
    /// Storage failure.
    #[error(transparent)]
    Db(#[from] DbError),
    /// The fan-out transport failed.
    #[error("fan-out error: {0}")]
    FanOut(String),
    /// The service is shutting down or not started.
    #[error("realtime is unavailable: {0}")]
    Unavailable(String),
    /// A bug or corrupt state.
    #[error("internal realtime error: {0}")]
    Internal(String),
}

impl RealtimeError {
    pub(crate) fn invalid(code: &'static str, message: impl Into<String>) -> Self {
        RealtimeError::Invalid {
            code,
            message: message.into(),
        }
    }

    /// Maps a data-core error for `ctx`: non-privileged callers never learn
    /// whether a collection exists (like the REST layer).
    pub(crate) fn from_data(e: DataError, ctx: &AccessContext) -> Self {
        match e {
            DataError::CollectionNotFound(_) if !ctx.is_privileged() => {
                if ctx.principal == forgebase_data::Principal::Anonymous {
                    RealtimeError::Unauthenticated("authentication required".into())
                } else {
                    RealtimeError::Forbidden("not allowed".into())
                }
            }
            DataError::Unauthenticated => RealtimeError::Unauthenticated("authentication required".into()),
            DataError::Forbidden(m) => RealtimeError::Forbidden(m),
            other => RealtimeError::Data(other),
        }
    }
}

impl ErrorCode for RealtimeError {
    fn code(&self) -> &'static str {
        match self {
            RealtimeError::Unauthenticated(_) => "unauthenticated",
            RealtimeError::Forbidden(_) => "forbidden",
            RealtimeError::TooManyConnections(_) => "too_many_connections",
            RealtimeError::RateLimited(_) => "rate_limited",
            RealtimeError::Invalid { code, .. } => code,
            RealtimeError::Data(e) => e.code(),
            RealtimeError::Db(e) => e.code(),
            RealtimeError::FanOut(_) => "fanout_error",
            RealtimeError::Unavailable(_) => "unavailable",
            RealtimeError::Internal(_) => "internal_error",
        }
    }

    fn kind(&self) -> ErrorKind {
        match self {
            RealtimeError::Unauthenticated(_) => ErrorKind::Unauthenticated,
            RealtimeError::Forbidden(_) => ErrorKind::Forbidden,
            // 429 is mapped by the HTTP layer from the code.
            RealtimeError::TooManyConnections(_) | RealtimeError::RateLimited(_) => ErrorKind::Unavailable,
            RealtimeError::Invalid { .. } => ErrorKind::InvalidInput,
            RealtimeError::Data(e) => e.kind(),
            RealtimeError::Db(e) => e.kind(),
            RealtimeError::FanOut(_) | RealtimeError::Unavailable(_) => ErrorKind::Unavailable,
            RealtimeError::Internal(_) => ErrorKind::Internal,
        }
    }
}
