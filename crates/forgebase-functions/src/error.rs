//! Errors of the functions engine.

use axum::http::StatusCode;
use forgebase_core::{ErrorCode, ErrorKind};
use forgebase_data::DataError;
use forgebase_db::DbError;

/// Why one execution of a function did not complete normally.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExecError {
    /// The function used all of its fuel (CPU budget).
    #[error("the function used all of its fuel ({0} units): look for an endless loop, or raise limits.fuel (at most 20000000000) and deploy again")]
    FuelExhausted(u64),
    /// The function tried to grow its memory beyond the limit, or its
    /// initial memory is larger than the limit.
    #[error("the function exceeded its memory limit of {0} bytes: keep less in memory, or raise limits.memory_bytes (at most 536870912) and deploy again")]
    MemoryLimit(u64),
    /// The function ran longer than its wall-clock limit.
    #[error("the function exceeded its time limit of {0} ms: make it finish sooner (fewer or faster awaits), or raise limits.timeout_ms (at most 60000) and deploy again")]
    Timeout(u64),
    /// The module trapped (unreachable, out-of-bounds access, stack overflow, ...).
    #[error("the function trapped: {0}")]
    Trap(String),
    /// `handle` returned a non-zero status.
    #[error("the function reported failure (status {status}){}", message.as_ref().map(|m| format!(": {m}")).unwrap_or_default())]
    Failed {
        /// The non-zero status.
        status: i32,
        /// The function's output, when it is short UTF-8 text.
        message: Option<String>,
    },
    /// The function's output is not what its trigger expects.
    #[error("the function returned an invalid response: {0}")]
    BadOutput(String),
    /// A JavaScript exception nobody caught (the trace points at the source).
    #[error("the function threw an uncaught exception (catch it, or fix the line shown): {0}")]
    Uncaught(String),
}

impl ExecError {
    /// Stable code.
    pub fn code(&self) -> &'static str {
        match self {
            ExecError::FuelExhausted(_) => "function_fuel_exhausted",
            ExecError::MemoryLimit(_) => "function_memory_limit",
            ExecError::Timeout(_) => "function_timeout",
            ExecError::Trap(_) => "function_trap",
            ExecError::Failed { .. } | ExecError::Uncaught(_) => "function_failed",
            ExecError::BadOutput(_) => "function_bad_output",
        }
    }

    /// Short status recorded in execution logs.
    pub fn status(&self) -> &'static str {
        match self {
            ExecError::FuelExhausted(_) => "fuel_exhausted",
            ExecError::MemoryLimit(_) => "memory_limit",
            ExecError::Timeout(_) => "timeout",
            ExecError::Trap(_) => "trap",
            ExecError::Failed { .. } | ExecError::Uncaught(_) => "failed",
            ExecError::BadOutput(_) => "bad_output",
        }
    }
}

/// Errors of the functions engine.
#[derive(Debug, thiserror::Error)]
pub enum FunctionError {
    /// Authorisation or data-core failure.
    #[error(transparent)]
    Data(#[from] DataError),
    /// Storage failure.
    #[error(transparent)]
    Db(#[from] DbError),
    /// The request is well-formed but not acceptable.
    #[error("{message}")]
    Invalid {
        /// Stable code.
        code: &'static str,
        /// Explanation.
        message: String,
    },
    /// No such function.
    #[error("function {0:?} not found")]
    NotFound(String),
    /// No such version.
    #[error("function {name:?} has no version {version}")]
    VersionNotFound {
        /// Function name.
        name: String,
        /// Requested version.
        version: i64,
    },
    /// No such secret.
    #[error("secret {0:?} not found")]
    SecretNotFound(String),
    /// The function (or the engine) is running as many executions as allowed.
    #[error("{0}")]
    Busy(String),
    /// An execution failed.
    #[error(transparent)]
    Execution(#[from] ExecError),
    /// A bug or unexpected failure.
    #[error("internal functions error: {0}")]
    Internal(String),
    /// JavaScript or TypeScript that does not compile (`400 function_compile_failed`).
    #[error("{}", compile_message(.0))]
    Compile(Vec<crate::compile::CompileDiagnostic>),
    /// The code uses services whose capabilities are not granted (`400 capability_not_granted`).
    #[error("{}", compile_message(.0))]
    Unmet(Vec<crate::compile::CompileDiagnostic>),
    /// A WebAssembly version has no source (`404`).
    #[error("version {version} of function {name:?} is WebAssembly and has no source")]
    NoSource {
        /// Function.
        name: String,
        /// Version.
        version: i64,
    },
    /// The caller may not do this (`403`).
    #[error("{message}")]
    Forbidden {
        /// Error code.
        code: &'static str,
        /// Message.
        message: String,
    },
}

fn compile_message(errors: &[crate::compile::CompileDiagnostic]) -> String {
    match errors.first() {
        Some(e) => {
            let more = if errors.len() > 1 {
                format!(" (and {} more)", errors.len() - 1)
            } else {
                String::new()
            };
            format!("line {}, column {}: {}{more}", e.line, e.column, e.message)
        }
        None => "the source does not compile".into(),
    }
}

impl FunctionError {
    pub(crate) fn invalid(code: &'static str, message: impl Into<String>) -> Self {
        FunctionError::Invalid {
            code,
            message: message.into(),
        }
    }

    /// HTTP status for this error. Execution failures are the function's
    /// fault, not the caller's: `502` (`504` for timeouts).
    pub fn http_status(&self) -> StatusCode {
        match self {
            FunctionError::Execution(ExecError::Timeout(_)) => StatusCode::GATEWAY_TIMEOUT,
            FunctionError::Execution(_) => StatusCode::BAD_GATEWAY,
            FunctionError::Busy(_) => StatusCode::TOO_MANY_REQUESTS,
            other => match other.kind() {
                ErrorKind::InvalidInput => StatusCode::BAD_REQUEST,
                ErrorKind::Unauthenticated => StatusCode::UNAUTHORIZED,
                ErrorKind::Forbidden => StatusCode::FORBIDDEN,
                ErrorKind::NotFound => StatusCode::NOT_FOUND,
                ErrorKind::Conflict => StatusCode::CONFLICT,
                ErrorKind::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
                ErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
            },
        }
    }
}

impl ErrorCode for FunctionError {
    fn code(&self) -> &'static str {
        match self {
            FunctionError::Data(e) => e.code(),
            FunctionError::Db(e) => e.code(),
            FunctionError::Invalid { code, .. } => code,
            FunctionError::NotFound(_) => "function_not_found",
            FunctionError::VersionNotFound { .. } => "function_version_not_found",
            FunctionError::SecretNotFound(_) => "secret_not_found",
            FunctionError::Busy(_) => "function_busy",
            FunctionError::Execution(e) => e.code(),
            FunctionError::Internal(_) => "internal_error",
            FunctionError::Compile(_) => "function_compile_failed",
            FunctionError::Unmet(_) => "capability_not_granted",
            FunctionError::NoSource { .. } => "function_source_not_found",
            FunctionError::Forbidden { code, .. } => code,
        }
    }

    fn kind(&self) -> ErrorKind {
        match self {
            FunctionError::Data(e) => e.kind(),
            FunctionError::Db(e) => e.kind(),
            FunctionError::Invalid { .. } | FunctionError::Compile(_) | FunctionError::Unmet(_) => {
                ErrorKind::InvalidInput
            }
            FunctionError::NoSource { .. } => ErrorKind::NotFound,
            FunctionError::Forbidden { .. } => ErrorKind::Forbidden,
            FunctionError::NotFound(_) | FunctionError::VersionNotFound { .. } | FunctionError::SecretNotFound(_) => {
                ErrorKind::NotFound
            }
            FunctionError::Busy(_) | FunctionError::Execution(_) => ErrorKind::Unavailable,
            FunctionError::Internal(_) => ErrorKind::Internal,
        }
    }
}

/// Result alias.
pub type Result<T, E = FunctionError> = std::result::Result<T, E>;
