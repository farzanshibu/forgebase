//! ForgeBase query language (ADR-0003 §5, §6).
//!
//! * [`filter`]: the PocketBase-style filter language (lexer, parser, AST,
//!   sort keys).
//! * [`compile`]: compiles filters, sort keys and field paths against a
//!   collection schema into parameterised SQL through a
//!   [`forgebase_db::Dialect`].
//! * [`sql`]: tokenizer, statement splitter and classifier for raw SQL.
//! * [`cypher`]: the openCypher subset of the portable graph engine
//!   (tokenizer, recursive-descent parser, syntax tree; ADR-0034).
//! * [`trigram`]: `pg_trgm`'s trigram model for the portable fuzzy-search
//!   engine (ADR-0034).
//!
//! No I/O happens here, so everything is unit-tested in isolation, including
//! injection attempts.

pub mod compile;
pub mod cypher;
pub mod filter;
pub mod native;
pub mod sql;
pub mod trigram;
mod trigram_fold;

pub use compile::{Catalog, CompileOptions, Compiler, FieldGuard, ScalarType, SqlBuilder, filter_keys, query_label};
pub use filter::{
    Compare, Expr, FieldPath, Literal, Modifier, Op, Operand, PathSeg, SortKey, parse_filter, parse_sort,
};
pub use native::NativeRender;
pub use sql::{
    SqlFlavor, StatementInfo, StatementKind, analyze_statement, analyze_statement_for, split_statements,
    split_statements_for,
};

use forgebase_core::{ErrorCode, ErrorKind};

/// Filter, sort and SQL-analysis errors. All are the caller's fault.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum QueryError {
    /// Malformed input.
    #[error("syntax error at {position}: {message}")]
    Syntax {
        /// Byte offset.
        position: usize,
        /// What is wrong.
        message: String,
    },
    /// Input exceeds a size or complexity limit.
    #[error("query too complex: {0}")]
    TooComplex(String),
    /// A path does not name a queryable field.
    #[error("unknown field {0:?}")]
    UnknownField(String),
    /// A well-formed but meaningless operation (e.g. `~` on a number).
    #[error("invalid filter: {0}")]
    InvalidOperation(String),
    /// A `{:name}` parameter has no value.
    #[error("missing value for parameter {0:?}")]
    MissingParam(String),
    /// The SQL editor does not run this statement.
    #[error("statement not allowed: {0}")]
    NotAllowed(String),
}

impl QueryError {
    pub(crate) fn syntax(position: usize, message: impl Into<String>) -> Self {
        QueryError::Syntax {
            position,
            message: message.into(),
        }
    }
}

impl ErrorCode for QueryError {
    fn code(&self) -> &'static str {
        match self {
            QueryError::Syntax { .. } => "filter_syntax_error",
            QueryError::TooComplex(_) => "query_too_complex",
            QueryError::UnknownField(_) => "unknown_field",
            QueryError::InvalidOperation(_) => "invalid_filter",
            QueryError::MissingParam(_) => "missing_parameter",
            QueryError::NotAllowed(_) => "statement_not_allowed",
        }
    }

    fn kind(&self) -> ErrorKind {
        ErrorKind::InvalidInput
    }
}
