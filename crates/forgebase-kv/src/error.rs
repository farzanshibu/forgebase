//! Errors of the KV engine.

use forgebase_core::{ErrorCode, ErrorKind};

/// A failed KV operation.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum KvError {
    /// The key holds another data type than the command needs.
    #[error("the key holds a different kind of value")]
    WrongType,
    /// A value that must be an integer is not.
    #[error("value is not an integer or out of range")]
    NotInteger,
    /// A value that must be a number is not.
    #[error("value is not a valid number")]
    NotFloat,
    /// The result would overflow.
    #[error("increment or decrement would overflow")]
    Overflow,
    /// Bad command syntax or argument.
    #[error("{0}")]
    Syntax(String),
    /// No such command.
    #[error("unknown command '{0}'")]
    UnknownCommand(String),
    /// Wrong number of arguments.
    #[error("wrong number of arguments for '{0}'")]
    WrongArity(String),
    /// Key rejected (empty, too long).
    #[error("{0}")]
    InvalidKey(String),
    /// Namespace name rejected.
    #[error("{0}")]
    InvalidNamespace(String),
    /// A value exceeds the size limit.
    #[error("value larger than the {0} byte limit")]
    TooLarge(usize),
    /// `KEYS` matched more keys than allowed.
    #[error("more than {0} keys match; use SCAN")]
    TooManyKeys(usize),
    /// A list index is outside the list, or the key does not exist for `LSET`.
    #[error("{0}")]
    OutOfRange(String),
    /// Over the memory budget and nothing may be evicted.
    #[error("memory limit reached and the eviction policy does not allow freeing space")]
    OutOfMemory,
    /// Too many live pub/sub channels.
    #[error("too many pub/sub channels")]
    TooManyChannels,
    /// The caller has no credentials.
    #[error("authentication required")]
    Unauthenticated,
    /// The caller is not allowed to do this.
    #[error("not allowed")]
    Forbidden,
    /// A non-privileged caller without a tenant named a tenant namespace
    /// (FB-138): select a tenant, or use a `shared.<name>` namespace.
    #[error("{0}")]
    TenantRequired(String),
    /// The SQLite snapshot failed.
    #[error("kv storage error: {0}")]
    Storage(String),
}

impl ErrorCode for KvError {
    fn code(&self) -> &'static str {
        match self {
            KvError::WrongType => "kv_wrong_type",
            KvError::NotInteger => "kv_not_integer",
            KvError::NotFloat => "kv_not_number",
            KvError::Overflow => "kv_overflow",
            KvError::Syntax(_) => "kv_syntax",
            KvError::UnknownCommand(_) => "kv_unknown_command",
            KvError::WrongArity(_) => "kv_wrong_arity",
            KvError::InvalidKey(_) => "kv_invalid_key",
            KvError::InvalidNamespace(_) => "kv_invalid_namespace",
            KvError::TooLarge(_) => "kv_value_too_large",
            KvError::TooManyKeys(_) => "kv_too_many_keys",
            KvError::OutOfRange(_) => "kv_out_of_range",
            KvError::OutOfMemory => "kv_out_of_memory",
            KvError::TooManyChannels => "kv_too_many_channels",
            KvError::Unauthenticated => "unauthenticated",
            KvError::Forbidden => "forbidden",
            KvError::TenantRequired(_) => "tenant_required",
            KvError::Storage(_) => "kv_storage_error",
        }
    }

    fn kind(&self) -> ErrorKind {
        match self {
            KvError::WrongType => ErrorKind::Conflict,
            KvError::NotInteger
            | KvError::NotFloat
            | KvError::Overflow
            | KvError::Syntax(_)
            | KvError::UnknownCommand(_)
            | KvError::WrongArity(_)
            | KvError::InvalidKey(_)
            | KvError::InvalidNamespace(_)
            | KvError::TooLarge(_)
            | KvError::TooManyKeys(_)
            | KvError::OutOfRange(_)
            | KvError::TenantRequired(_) => ErrorKind::InvalidInput,
            KvError::OutOfMemory | KvError::TooManyChannels => ErrorKind::Unavailable,
            KvError::Unauthenticated => ErrorKind::Unauthenticated,
            KvError::Forbidden => ErrorKind::Forbidden,
            KvError::Storage(_) => ErrorKind::Internal,
        }
    }
}

impl From<forgebase_db::DbError> for KvError {
    fn from(e: forgebase_db::DbError) -> Self {
        KvError::Storage(e.to_string())
    }
}

impl From<KvError> for forgebase_api::ApiError {
    fn from(e: KvError) -> Self {
        forgebase_api::ApiError::from_error(&e)
    }
}

/// Result alias.
pub type KvResult<T> = Result<T, KvError>;
