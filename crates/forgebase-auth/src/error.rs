//! Errors of the auth service.
//!
//! Messages are written for clients and never contain secrets. Credential
//! failures are deliberately vague (`invalid_credentials`) so they do not
//! reveal whether an account exists.

use forgebase_core::{ErrorCode, ErrorKind};
use forgebase_data::DataError;
use forgebase_data::db::DbError;

/// Everything that can go wrong in authentication.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AuthError {
    /// Wrong email/password, unknown account, or an unusable one-time code.
    #[error("invalid credentials")]
    InvalidCredentials,
    /// Missing, malformed, expired, revoked or forged access token or key.
    #[error("{0}")]
    InvalidToken(&'static str),
    /// The endpoint needs an authenticated caller.
    #[error("authentication required")]
    Unauthenticated,
    /// Authenticated, but not allowed (role, scope, CSRF).
    #[error("{0}")]
    Forbidden(String),
    /// A second factor is needed; the client continues with the challenge.
    #[error("multi-factor authentication required")]
    MfaRequired,
    /// Too many attempts; retry after the given number of seconds.
    #[error("too many attempts, retry in {retry_after} seconds")]
    RateLimited {
        /// Seconds until the next attempt is allowed.
        retry_after: u64,
    },
    /// The account must verify its email first.
    #[error("email address not verified")]
    EmailNotVerified,
    /// Bad request data.
    #[error("{0}")]
    InvalidInput(String),
    /// The email is already registered.
    #[error("an account with this email already exists")]
    EmailTaken,
    /// Name conflict (service account, admin).
    #[error("{0}")]
    Conflict(String),
    /// Unknown resource (session, key, service account, collection).
    #[error("{0}")]
    NotFound(String),
    /// Mail delivery failed.
    #[error("could not send mail: {0}")]
    Mail(String),
    /// Error from the data core (validation of user records, storage).
    #[error(transparent)]
    Data(#[from] DataError),
    /// Database failure.
    #[error(transparent)]
    Db(#[from] DbError),
    /// Bug or unexpected failure; the message is logged, never returned.
    #[error("{0}")]
    Internal(String),
}

impl ErrorCode for AuthError {
    fn code(&self) -> &'static str {
        match self {
            Self::InvalidCredentials => "invalid_credentials",
            Self::InvalidToken(_) => "invalid_token",
            Self::Unauthenticated => "unauthenticated",
            Self::Forbidden(_) => "forbidden",
            Self::MfaRequired => "mfa_required",
            Self::RateLimited { .. } => "too_many_requests",
            Self::EmailNotVerified => "email_not_verified",
            Self::InvalidInput(_) => "invalid_input",
            Self::EmailTaken => "email_taken",
            Self::Conflict(_) => "conflict",
            Self::NotFound(_) => "not_found",
            Self::Mail(_) => "mail_failed",
            Self::Data(e) => e.code(),
            Self::Db(e) => e.code(),
            Self::Internal(_) => "internal",
        }
    }

    fn kind(&self) -> ErrorKind {
        match self {
            Self::InvalidCredentials | Self::InvalidToken(_) | Self::Unauthenticated | Self::MfaRequired => {
                ErrorKind::Unauthenticated
            }
            Self::Forbidden(_) | Self::EmailNotVerified => ErrorKind::Forbidden,
            Self::RateLimited { .. } | Self::Mail(_) => ErrorKind::Unavailable,
            Self::InvalidInput(_) => ErrorKind::InvalidInput,
            Self::EmailTaken | Self::Conflict(_) => ErrorKind::Conflict,
            Self::NotFound(_) => ErrorKind::NotFound,
            Self::Data(e) => e.kind(),
            Self::Db(e) => e.kind(),
            Self::Internal(_) => ErrorKind::Internal,
        }
    }
}

impl AuthError {
    /// Shorthand for [`AuthError::InvalidInput`].
    pub fn invalid(msg: impl Into<String>) -> Self {
        Self::InvalidInput(msg.into())
    }
}
