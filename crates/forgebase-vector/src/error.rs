//! Vector errors and their HTTP mapping (ADR-0001 §7).

use forgebase_core::{ErrorCode, ErrorKind};
use forgebase_data::DataError;
use forgebase_data::query::QueryError;
use forgebase_db::{DbError, RouterError};

/// Errors of the vector service.
#[derive(Debug, thiserror::Error)]
pub enum VectorError {
    /// The caller must authenticate.
    #[error("authentication required")]
    Unauthenticated,
    /// The caller is not allowed to do this.
    #[error("forbidden: {0}")]
    Forbidden(String),
    /// No such collection in the caller's tenant.
    #[error("vector collection {0:?} not found")]
    CollectionNotFound(String),
    /// A collection with this name exists in the caller's tenant.
    #[error("vector collection {0:?} already exists")]
    CollectionExists(String),
    /// No such vector field.
    #[error("vector field {0:?} not found")]
    FieldNotFound(String),
    /// A vector field with this name exists.
    #[error("vector field {0:?} already exists")]
    FieldExists(String),
    /// No such item.
    #[error("item {0:?} not found")]
    ItemNotFound(String),
    /// A vector has the wrong number of components for its field.
    #[error("{field}: expected {expected} dimensions, got {got}")]
    DimensionMismatch {
        /// Vector field.
        field: String,
        /// Dimension of the field.
        expected: usize,
        /// Components received.
        got: usize,
    },
    /// The request is malformed or exceeds a limit.
    #[error("{message}")]
    Invalid {
        /// Stable code.
        code: &'static str,
        /// Explanation.
        message: String,
    },
    /// The metadata filter does not parse or compile.
    #[error(transparent)]
    Filter(#[from] QueryError),
    /// The ANN index is being built and the collection is too large for an
    /// exact scan in the meantime. Retry later.
    #[error("the index of {0} is not ready ({1}); retry when it is rebuilt")]
    IndexNotReady(String, String),
    /// Storage failure.
    #[error(transparent)]
    Db(#[from] DbError),
    /// The tenant's database set cannot take the call (FB-172, ADR-0029
    /// §3.5, §3.8): `409 tenant_moving` (a move's freeze outlasted the
    /// timeout, or a schema change for a tenant being moved), `503
    /// shard_unavailable` / `shard_migrating`.
    #[error(transparent)]
    Router(#[from] RouterError),
    /// The ANN engine failed.
    #[error("vector index error: {0}")]
    Index(String),
    /// Index file input/output failed.
    #[error("vector index file error: {0}")]
    Io(String),
    /// Stored data could not be decoded (corruption or a bug).
    #[error("internal vector error: {0}")]
    Internal(String),
}

impl VectorError {
    pub(crate) fn invalid(code: &'static str, message: impl Into<String>) -> Self {
        VectorError::Invalid {
            code,
            message: message.into(),
        }
    }
}

impl From<DataError> for VectorError {
    fn from(e: DataError) -> Self {
        match e {
            DataError::Unauthenticated => VectorError::Unauthenticated,
            DataError::Forbidden(m) => VectorError::Forbidden(m),
            DataError::Db(e) => VectorError::Db(e),
            DataError::Query(e) => VectorError::Filter(e),
            other => VectorError::Internal(other.to_string()),
        }
    }
}

impl From<std::io::Error> for VectorError {
    fn from(e: std::io::Error) -> Self {
        VectorError::Io(e.to_string())
    }
}

impl ErrorCode for VectorError {
    fn code(&self) -> &'static str {
        match self {
            VectorError::Unauthenticated => "unauthenticated",
            VectorError::Forbidden(_) => "forbidden",
            VectorError::CollectionNotFound(_) => "vector_collection_not_found",
            VectorError::CollectionExists(_) => "vector_collection_exists",
            VectorError::FieldNotFound(_) => "vector_field_not_found",
            VectorError::FieldExists(_) => "vector_field_exists",
            VectorError::ItemNotFound(_) => "vector_item_not_found",
            VectorError::DimensionMismatch { .. } => "vector_dimension_mismatch",
            VectorError::Invalid { code, .. } => code,
            VectorError::Filter(e) => e.code(),
            VectorError::IndexNotReady(..) => "vector_index_not_ready",
            VectorError::Db(e) => e.code(),
            VectorError::Router(e) => e.code(),
            VectorError::Index(_) => "vector_index_error",
            VectorError::Io(_) => "vector_index_io_error",
            VectorError::Internal(_) => "internal_error",
        }
    }

    fn kind(&self) -> ErrorKind {
        match self {
            VectorError::Unauthenticated => ErrorKind::Unauthenticated,
            VectorError::Forbidden(_) => ErrorKind::Forbidden,
            VectorError::CollectionNotFound(_) | VectorError::FieldNotFound(_) | VectorError::ItemNotFound(_) => {
                ErrorKind::NotFound
            }
            VectorError::CollectionExists(_) | VectorError::FieldExists(_) => ErrorKind::Conflict,
            VectorError::DimensionMismatch { .. } | VectorError::Invalid { .. } => ErrorKind::InvalidInput,
            VectorError::Filter(e) => e.kind(),
            VectorError::IndexNotReady(..) => ErrorKind::Unavailable,
            VectorError::Db(e) => e.kind(),
            VectorError::Router(e) => e.kind(),
            VectorError::Index(_) | VectorError::Io(_) | VectorError::Internal(_) => ErrorKind::Internal,
        }
    }
}
