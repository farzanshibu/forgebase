//! Errors of the backup crate.

use std::path::PathBuf;

use forgebase_core::{ErrorCode, ErrorKind};
use forgebase_data::DataError;
use forgebase_db::DbError;

/// Failure of a backup, restore, snapshot, export or import.
#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    /// The request itself is wrong (bad id, bad option, missing key).
    #[error("{0}")]
    Invalid(String),
    /// No such backup or snapshot.
    #[error("backup {0:?} not found")]
    NotFound(String),
    /// The target already holds data, or a restore was interrupted.
    #[error("{0}")]
    Conflict(String),
    /// The backup does not match its manifest or is not a usable database.
    #[error("backup verification failed: {}", .0.join("; "))]
    Verify(Vec<String>),
    /// File system failure.
    #[error("{}: {source}", path.display())]
    Io {
        /// File or directory.
        path: PathBuf,
        /// Cause.
        source: std::io::Error,
    },
    /// Database failure.
    #[error(transparent)]
    Db(#[from] DbError),
    /// Data service failure (export and import).
    #[error(transparent)]
    Data(#[from] DataError),
    /// A file of an export bundle is malformed.
    #[error("{0}")]
    Format(String),
    /// A task failed unexpectedly.
    #[error("{0}")]
    Internal(String),
}

impl ErrorCode for BackupError {
    fn code(&self) -> &'static str {
        match self {
            Self::Invalid(_) => "backup_invalid",
            Self::NotFound(_) => "backup_not_found",
            Self::Conflict(_) => "backup_conflict",
            Self::Verify(_) => "backup_verification_failed",
            Self::Io { .. } => "backup_io_error",
            Self::Db(e) => e.code(),
            Self::Data(e) => e.code(),
            Self::Format(_) => "backup_bundle_invalid",
            Self::Internal(_) => "internal_error",
        }
    }

    fn kind(&self) -> ErrorKind {
        match self {
            Self::Invalid(_) | Self::Format(_) => ErrorKind::InvalidInput,
            Self::NotFound(_) => ErrorKind::NotFound,
            Self::Conflict(_) => ErrorKind::Conflict,
            Self::Verify(_) => ErrorKind::Conflict,
            Self::Db(e) => e.kind(),
            Self::Data(e) => e.kind(),
            Self::Io { .. } | Self::Internal(_) => ErrorKind::Internal,
        }
    }
}

/// Result alias.
pub type Result<T> = std::result::Result<T, BackupError>;

/// Adds the path to an I/O error.
pub(crate) fn io(path: &std::path::Path) -> impl FnOnce(std::io::Error) -> BackupError + '_ {
    move |source| BackupError::Io {
        path: path.to_path_buf(),
        source,
    }
}
