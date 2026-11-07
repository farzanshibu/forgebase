//! Data-core errors.

use forgebase_core::{ErrorCode, ErrorKind};
use forgebase_db::{ConstraintKind, DbError};
use forgebase_query::QueryError;
use forgebase_schema::{SchemaError, ValidationErrors};

/// Errors of the data-core services.
#[derive(Debug, thiserror::Error)]
pub enum DataError {
    /// Storage failure.
    #[error(transparent)]
    Db(#[from] DbError),
    /// Bad filter, sort or SQL.
    #[error(transparent)]
    Query(#[from] QueryError),
    /// Bad collection definition.
    #[error(transparent)]
    Schema(#[from] SchemaError),
    /// Record values failed validation.
    #[error(transparent)]
    Validation(#[from] ValidationErrors),
    /// No such collection.
    #[error("collection {0:?} not found")]
    CollectionNotFound(String),
    /// No such record (or the caller may not see it).
    #[error("record {id:?} not found in {collection:?}")]
    RecordNotFound {
        /// Collection.
        collection: String,
        /// Record id.
        id: String,
    },
    /// No such index.
    #[error("index {0:?} not found")]
    IndexNotFound(String),
    /// Name already taken.
    #[error("{0} already exists")]
    AlreadyExists(String),
    /// The caller is not allowed to do this.
    #[error("forbidden: {0}")]
    Forbidden(String),
    /// The caller must authenticate.
    #[error("authentication required")]
    Unauthenticated,
    /// Optimistic-concurrency check failed.
    #[error("record version is {actual}, expected {expected}")]
    VersionConflict {
        /// Version the caller expected.
        expected: i64,
        /// Current version.
        actual: i64,
    },
    /// The change conflicts with existing data or schema.
    #[error("{message}")]
    Conflict {
        /// Stable code.
        code: &'static str,
        /// Explanation.
        message: String,
    },
    /// The request is well-formed but not acceptable.
    #[error("{message}")]
    Invalid {
        /// Stable code.
        code: &'static str,
        /// Explanation.
        message: String,
    },
    /// A password field was written but no secret hasher is configured.
    #[error("password fields need a secret hasher (auth is not configured)")]
    HasherUnavailable,
    /// Stored data could not be decoded (corrupt catalog or bug).
    #[error("internal data error: {0}")]
    Internal(String),
}

impl DataError {
    pub(crate) fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        DataError::Conflict {
            code,
            message: message.into(),
        }
    }

    pub(crate) fn invalid(code: &'static str, message: impl Into<String>) -> Self {
        DataError::Invalid {
            code,
            message: message.into(),
        }
    }
}

impl From<serde_json::Error> for DataError {
    fn from(e: serde_json::Error) -> Self {
        DataError::Internal(format!("json: {e}"))
    }
}

impl ErrorCode for DataError {
    fn code(&self) -> &'static str {
        match self {
            DataError::Db(e) => e.code(),
            DataError::Query(e) => e.code(),
            DataError::Schema(e) => e.code(),
            DataError::Validation(e) => e.code(),
            DataError::CollectionNotFound(_) => "collection_not_found",
            DataError::RecordNotFound { .. } => "record_not_found",
            DataError::IndexNotFound(_) => "index_not_found",
            DataError::AlreadyExists(_) => "already_exists",
            DataError::Forbidden(_) => "forbidden",
            DataError::Unauthenticated => "unauthenticated",
            DataError::VersionConflict { .. } => "version_conflict",
            DataError::Conflict { code, .. } | DataError::Invalid { code, .. } => code,
            DataError::HasherUnavailable => "password_hasher_unavailable",
            DataError::Internal(_) => "internal_error",
        }
    }

    fn kind(&self) -> ErrorKind {
        match self {
            DataError::Db(e) => e.kind(),
            DataError::Query(e) => e.kind(),
            DataError::Schema(_) | DataError::Validation(_) | DataError::Invalid { .. } => ErrorKind::InvalidInput,
            DataError::CollectionNotFound(_) | DataError::RecordNotFound { .. } | DataError::IndexNotFound(_) => {
                ErrorKind::NotFound
            }
            DataError::AlreadyExists(_) | DataError::VersionConflict { .. } | DataError::Conflict { .. } => {
                ErrorKind::Conflict
            }
            DataError::Forbidden(_) => ErrorKind::Forbidden,
            DataError::Unauthenticated => ErrorKind::Unauthenticated,
            DataError::HasherUnavailable => ErrorKind::Unavailable,
            DataError::Internal(_) => ErrorKind::Internal,
        }
    }
}

/// Maps a unique-constraint failure on a collection table to field errors
/// (`UNIQUE constraint failed: posts.slug, posts.lang`).
pub(crate) fn unique_fields(table: &str, e: &DbError) -> Option<Vec<String>> {
    let DbError::Constraint {
        kind: ConstraintKind::Unique,
        message,
    } = e
    else {
        return None;
    };
    let list = message.split("failed:").nth(1)?;
    let fields: Vec<String> = list
        .split(',')
        .filter_map(|part| {
            let (t, c) = part.trim().split_once('.')?;
            t.eq_ignore_ascii_case(table).then(|| c.to_owned())
        })
        .collect();
    (!fields.is_empty()).then_some(fields)
}
