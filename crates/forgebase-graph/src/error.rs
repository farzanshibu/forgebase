//! Graph errors and their HTTP mapping.

use forgebase_core::{ErrorCode, ErrorKind};
use forgebase_data::DataError;
use forgebase_db::DbError;

/// Errors of the graph service.
#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    /// The caller must authenticate.
    #[error("authentication required")]
    Unauthenticated,
    /// The caller is not allowed to do this.
    #[error("forbidden: {0}")]
    Forbidden(String),
    /// No such node (or not visible to the caller's tenant).
    #[error("node {0:?} not found")]
    NodeNotFound(String),
    /// No such edge (or not visible to the caller's tenant).
    #[error("edge {0:?} not found")]
    EdgeNotFound(String),
    /// A node with this id exists.
    #[error("node {0:?} already exists")]
    NodeExists(String),
    /// An edge with this id exists.
    #[error("edge {0:?} already exists")]
    EdgeExists(String),
    /// The request is malformed or exceeds a limit.
    #[error("{message}")]
    Invalid {
        /// Stable code.
        code: &'static str,
        /// Explanation.
        message: String,
    },
    /// The caller's graph rules cannot be applied to an openCypher query
    /// (FB-163: they select too many rows to pass along).
    #[error("{0}")]
    RulesUnsupported(String),
    /// The derived Apache AGE graph is not in use here (FB-163): its
    /// operations (`drop`, materialisation) do not exist; `POST
    /// /graph/cypher` answers on the portable engine (ADR-0034).
    #[error("the Apache AGE openCypher graph is not in use on this server (queries run on the portable engine)")]
    CypherUnsupported,
    /// The tenant's derived graph is being built (FB-163); `job_id` is the
    /// `graph.materialise` job, when the jobs service runs it.
    #[error("the openCypher graph of this tenant is being built; retry when job {} is done", .job_id.as_deref().unwrap_or("(in process)"))]
    Materialising {
        /// The job.
        job_id: Option<String>,
    },
    /// Shard routing (ADR-0029 §3.5).
    #[error(transparent)]
    Router(#[from] forgebase_db::RouterError),
    /// Storage failure.
    #[error(transparent)]
    Db(#[from] DbError),
    /// Stored data could not be decoded (corruption or a bug).
    #[error("internal graph error: {0}")]
    Internal(String),
}

impl GraphError {
    pub(crate) fn invalid(code: &'static str, message: impl Into<String>) -> Self {
        GraphError::Invalid {
            code,
            message: message.into(),
        }
    }
}

impl From<DataError> for GraphError {
    fn from(e: DataError) -> Self {
        match e {
            DataError::Unauthenticated => GraphError::Unauthenticated,
            DataError::Forbidden(m) => GraphError::Forbidden(m),
            DataError::Db(e) => GraphError::Db(e),
            other => GraphError::Internal(other.to_string()),
        }
    }
}

impl From<forgebase_data::query::QueryError> for GraphError {
    /// A rule that does not compile cannot be applied: fail closed.
    fn from(e: forgebase_data::query::QueryError) -> Self {
        GraphError::Forbidden(format!("a graph rule cannot be applied: {e}"))
    }
}

impl ErrorCode for GraphError {
    fn code(&self) -> &'static str {
        match self {
            GraphError::Unauthenticated => "unauthenticated",
            GraphError::Forbidden(_) => "forbidden",
            GraphError::NodeNotFound(_) => "node_not_found",
            GraphError::EdgeNotFound(_) => "edge_not_found",
            GraphError::NodeExists(_) => "node_exists",
            GraphError::EdgeExists(_) => "edge_exists",
            GraphError::Invalid { code, .. } => code,
            GraphError::RulesUnsupported(_) => "cypher_rules_unsupported",
            GraphError::CypherUnsupported => "cypher_unsupported",
            GraphError::Materialising { .. } => "graph_materialising",
            GraphError::Router(e) => e.code(),
            GraphError::Db(e) => e.code(),
            GraphError::Internal(_) => "internal_error",
        }
    }

    fn kind(&self) -> ErrorKind {
        match self {
            GraphError::Unauthenticated => ErrorKind::Unauthenticated,
            GraphError::Forbidden(_) => ErrorKind::Forbidden,
            GraphError::NodeNotFound(_) | GraphError::EdgeNotFound(_) => ErrorKind::NotFound,
            GraphError::NodeExists(_) | GraphError::EdgeExists(_) => ErrorKind::Conflict,
            GraphError::Invalid { .. } | GraphError::CypherUnsupported => ErrorKind::InvalidInput,
            GraphError::RulesUnsupported(_) => ErrorKind::Forbidden,
            GraphError::Materialising { .. } => ErrorKind::Conflict,
            GraphError::Router(e) => e.kind(),
            GraphError::Db(e) => e.kind(),
            GraphError::Internal(_) => ErrorKind::Internal,
        }
    }
}
