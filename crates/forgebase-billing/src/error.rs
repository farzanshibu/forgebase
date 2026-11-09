//! Errors of the billing service.

use forgebase_core::{ErrorCode, ErrorKind};
use forgebase_credits::CreditError;
use forgebase_db::DbError;
use forgebase_entitlements::EntitlementError;
use forgebase_policy::PolicyError;

/// Errors of [`crate::BillingService`].
#[derive(Debug, thiserror::Error)]
pub enum BillingError {
    /// Storage failure.
    #[error(transparent)]
    Db(#[from] DbError),
    /// The authorization pipeline refused the caller.
    #[error(transparent)]
    Policy(#[from] PolicyError),
    /// The entitlement sync (FB-24) failed.
    #[error(transparent)]
    Entitlements(#[from] EntitlementError),
    /// The credit ledger (FB-73) failed.
    #[error(transparent)]
    Credits(#[from] CreditError),
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
    /// A webhook whose signature, timestamp or body is not acceptable.
    #[error("{message}")]
    Webhook {
        /// Stable code (`invalid_signature`, `stale_signature`, ...).
        code: &'static str,
        /// Explanation.
        message: String,
    },
    /// The payment provider refused or failed a call.
    #[error("payment provider: {0}")]
    Provider(String),
    /// Billing is not configured (no such provider account).
    #[error("{0}")]
    NotConfigured(String),
    /// Bug or corrupt data.
    #[error("internal error: {0}")]
    Internal(String),
}

impl BillingError {
    pub(crate) fn invalid(code: &'static str, message: impl Into<String>) -> Self {
        BillingError::Invalid {
            code,
            message: message.into(),
        }
    }

    pub(crate) fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        BillingError::Conflict {
            code,
            message: message.into(),
        }
    }

    pub(crate) fn webhook(code: &'static str, message: impl Into<String>) -> Self {
        BillingError::Webhook {
            code,
            message: message.into(),
        }
    }
}

impl From<serde_json::Error> for BillingError {
    fn from(e: serde_json::Error) -> Self {
        BillingError::Internal(format!("json: {e}"))
    }
}

impl From<forgebase_auth::AuthError> for BillingError {
    fn from(e: forgebase_auth::AuthError) -> Self {
        BillingError::Provider(e.to_string())
    }
}

impl ErrorCode for BillingError {
    fn code(&self) -> &'static str {
        match self {
            BillingError::Db(e) => e.code(),
            BillingError::Policy(e) => e.code(),
            BillingError::Entitlements(e) => e.code(),
            BillingError::Credits(e) => e.code(),
            BillingError::Unauthenticated => "unauthenticated",
            BillingError::Forbidden(_) => "forbidden",
            BillingError::Invalid { code, .. }
            | BillingError::Conflict { code, .. }
            | BillingError::Webhook { code, .. } => code,
            BillingError::NotFound(_) => "not_found",
            BillingError::Provider(_) => "provider_error",
            BillingError::NotConfigured(_) => "billing_not_configured",
            BillingError::Internal(_) => "internal_error",
        }
    }

    fn kind(&self) -> ErrorKind {
        match self {
            BillingError::Db(e) => e.kind(),
            BillingError::Policy(e) => e.kind(),
            BillingError::Entitlements(e) => e.kind(),
            BillingError::Credits(e) => e.kind(),
            BillingError::Unauthenticated => ErrorKind::Unauthenticated,
            BillingError::Forbidden(_) => ErrorKind::Forbidden,
            BillingError::Invalid { .. } | BillingError::Webhook { .. } => ErrorKind::InvalidInput,
            BillingError::NotFound(_) | BillingError::NotConfigured(_) => ErrorKind::NotFound,
            BillingError::Conflict { .. } => ErrorKind::Conflict,
            BillingError::Provider(_) => ErrorKind::Unavailable,
            BillingError::Internal(_) => ErrorKind::Internal,
        }
    }
}

/// Result alias.
pub type Result<T, E = BillingError> = std::result::Result<T, E>;
