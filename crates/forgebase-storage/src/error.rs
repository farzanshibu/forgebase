//! Errors of the storage service.

use forgebase_core::{ErrorCode, ErrorKind};
use forgebase_db::{DbError, RouterError};
use forgebase_entitlements::EntitlementError;

/// Errors of [`crate::StorageService`] and the drivers.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// Metadata store failure.
    #[error(transparent)]
    Db(#[from] DbError),
    /// The bucket's database set cannot serve the call (ADR-0029 §3.5,
    /// FB-172): its tenant is being moved (`tenant_moving`, 409), or the set
    /// is unavailable or migrating (`shard_unavailable`, `shard_migrating`,
    /// 503).
    #[error(transparent)]
    Router(#[from] RouterError),
    /// The quota engine refused (quota exceeded, feature not entitled).
    #[error(transparent)]
    Entitlement(EntitlementError),
    /// A driver (filesystem, S3) failed.
    #[error("storage driver: {0}")]
    Driver(String),
    /// No credentials where they are required.
    #[error("authentication required")]
    Unauthenticated,
    /// The caller may not do this.
    #[error("{0}")]
    Forbidden(String),
    /// A signed URL is forged, tampered with or for another resource.
    #[error("the signature is not valid")]
    SignatureInvalid,
    /// A signed URL is past its expiry.
    #[error("the signed URL has expired")]
    SignatureExpired,
    /// No such bucket, object, version or upload (or not visible).
    #[error("{0} not found")]
    NotFound(String),
    /// Bad input.
    #[error("{message}")]
    Invalid {
        /// Stable code.
        code: &'static str,
        /// Explanation.
        message: String,
    },
    /// Conflicts with stored state.
    #[error("{message}")]
    Conflict {
        /// Stable code.
        code: &'static str,
        /// Explanation.
        message: String,
    },
    /// The received bytes do not hash to the checksum the client declared.
    #[error("checksum mismatch: expected {expected}, received {actual}")]
    ChecksumMismatch {
        /// Declared SHA-256 (hex).
        expected: String,
        /// SHA-256 of the received bytes (hex).
        actual: String,
    },
    /// The object (or a part) is larger than the limit.
    #[error("object is larger than the {limit} byte limit")]
    TooLarge {
        /// The limit in force.
        limit: u64,
    },
    /// A range request outside the object.
    #[error("range not satisfiable for an object of {size} bytes")]
    RangeNotSatisfiable {
        /// Object size.
        size: u64,
    },
    /// A resumable upload chunk does not start where the server is.
    #[error("upload offset mismatch: the server is at {server_offset}")]
    OffsetMismatch {
        /// Bytes the server has.
        server_offset: u64,
    },
    /// Bug or unexpected failure.
    #[error("internal error: {0}")]
    Internal(String),
}

impl StorageError {
    /// Bad input with a stable code.
    pub fn invalid(code: &'static str, message: impl Into<String>) -> Self {
        Self::Invalid {
            code,
            message: message.into(),
        }
    }

    /// Conflict with a stable code.
    pub fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        Self::Conflict {
            code,
            message: message.into(),
        }
    }

    /// A driver failure.
    pub fn driver(e: impl std::fmt::Display) -> Self {
        Self::Driver(e.to_string())
    }
}

impl From<EntitlementError> for StorageError {
    fn from(e: EntitlementError) -> Self {
        Self::Entitlement(e)
    }
}

impl From<std::io::Error> for StorageError {
    fn from(e: std::io::Error) -> Self {
        Self::Driver(e.to_string())
    }
}

impl ErrorCode for StorageError {
    fn code(&self) -> &'static str {
        match self {
            StorageError::Db(e) => e.code(),
            StorageError::Router(e) => e.code(),
            StorageError::Entitlement(e) => e.code(),
            StorageError::Driver(_) => "storage_unavailable",
            StorageError::Unauthenticated => "unauthenticated",
            StorageError::Forbidden(_) => "forbidden",
            StorageError::SignatureInvalid => "signature_invalid",
            StorageError::SignatureExpired => "url_expired",
            StorageError::NotFound(_) => "not_found",
            StorageError::Invalid { code, .. } | StorageError::Conflict { code, .. } => code,
            StorageError::ChecksumMismatch { .. } => "checksum_mismatch",
            StorageError::TooLarge { .. } => "object_too_large",
            StorageError::RangeNotSatisfiable { .. } => "range_not_satisfiable",
            StorageError::OffsetMismatch { .. } => "offset_mismatch",
            StorageError::Internal(_) => "internal_error",
        }
    }

    fn kind(&self) -> ErrorKind {
        match self {
            StorageError::Db(e) => e.kind(),
            StorageError::Router(e) => e.kind(),
            StorageError::Entitlement(e) => e.kind(),
            StorageError::Driver(_) => ErrorKind::Unavailable,
            StorageError::Unauthenticated => ErrorKind::Unauthenticated,
            StorageError::Forbidden(_) | StorageError::SignatureInvalid | StorageError::SignatureExpired => {
                ErrorKind::Forbidden
            }
            StorageError::NotFound(_) => ErrorKind::NotFound,
            StorageError::Invalid { .. }
            | StorageError::ChecksumMismatch { .. }
            | StorageError::TooLarge { .. }
            | StorageError::RangeNotSatisfiable { .. } => ErrorKind::InvalidInput,
            StorageError::Conflict { .. } | StorageError::OffsetMismatch { .. } => ErrorKind::Conflict,
            StorageError::Internal(_) => ErrorKind::Internal,
        }
    }
}

/// Result alias.
pub type Result<T, E = StorageError> = std::result::Result<T, E>;
