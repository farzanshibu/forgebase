//! The openCypher subset of ForgeBase's portable graph engine (ADR-0034):
//! a tokenizer ([`lexer`]) and a recursive-descent parser ([`parser`]) that
//! turn one read-only query into an [`ast::Query`]. No I/O; the graph crate
//! compiles the tree to SQL over the canonical graph tables.
//!
//! The subset:
//!
//! * clauses: one or more `MATCH pattern, … [WHERE expr]`, then one
//!   `RETURN [DISTINCT] item [AS name], … [ORDER BY expr [ASC|DESC], …]
//!   [SKIP n] [LIMIT n]`; a query may also be a bare `RETURN`;
//! * patterns: node patterns `(v:Label {key: expr, …})`, relationship
//!   patterns `-[r:TYPE|OTHER {key: expr}]->`, `<-[…]-`, `-[…]-` (and the
//!   short forms `-->`, `<--`, `--`), bounded variable-length relationships
//!   `-[*min..max]->` (`*n`, `*..max`; at most [`MAX_PATH_DEPTH`]);
//! * expressions: literals (`null`, booleans, integers, floats, strings,
//!   lists), parameters (`$name`), variables, property access (`n.a.b`),
//!   comparisons (`=`, `<>`, `<`, `<=`, `>`, `>=`), `AND`, `OR`, `NOT`,
//!   `IN`, `IS [NOT] NULL`, `STARTS WITH`, `ENDS WITH`, `CONTAINS`, unary
//!   minus on numbers, function calls (the graph crate decides which
//!   functions exist: `count`, `collect`, `min`, `max`, `sum`, `avg`, …).
//!
//! Errors carry a stable code and the position (line and column):
//!
//! * `cypher_invalid`: not openCypher (syntax);
//! * `cypher_read_only`: a clause that writes or calls out (`CREATE`,
//!   `MERGE`, `SET`, `DELETE`, `DETACH`, `REMOVE`, `FOREACH`, `CALL`,
//!   `LOAD`), as the Apache AGE path answers;
//! * `cypher_unsupported`: valid openCypher outside the subset (`WITH`,
//!   `OPTIONAL MATCH`, `UNWIND`, `UNION`, `CASE`, arithmetic, path
//!   variables, unbounded `*`, …), naming the construct;
//! * `cypher_too_complex`: a limit ([`MAX_PATH_DEPTH`], [`MAX_ELEMENTS`],
//!   [`MAX_NESTING`], [`MAX_RETURN_ITEMS`]).

pub mod ast;
pub mod lexer;
pub mod parser;

use std::fmt;

pub use ast::*;
pub use parser::parse;

/// Deepest variable-length relationship (`*..10`).
pub const MAX_PATH_DEPTH: u32 = 10;
/// Most node and relationship patterns in one query.
pub const MAX_ELEMENTS: usize = 32;
/// Deepest expression nesting.
pub const MAX_NESTING: usize = 64;
/// Most `RETURN` items (as the Apache AGE path).
pub const MAX_RETURN_ITEMS: usize = 100;
/// Longest query in bytes (as the Apache AGE path).
pub const MAX_QUERY_BYTES: usize = 64 * 1024;

/// Why a query was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CypherError {
    /// Stable code: `cypher_invalid`, `cypher_read_only`,
    /// `cypher_unsupported` or `cypher_too_complex`.
    pub code: &'static str,
    /// What is wrong, with the position.
    pub message: String,
    /// Byte offset in the query.
    pub offset: usize,
    /// 1-based line.
    pub line: usize,
    /// 1-based column (characters).
    pub column: usize,
}

impl CypherError {
    /// An error at byte `offset` of `query`.
    pub fn at(code: &'static str, query: &str, offset: usize, what: impl fmt::Display) -> Self {
        let offset = offset.min(query.len());
        let before = query.get(..offset).unwrap_or(query);
        let line = before.matches('\n').count() + 1;
        let column = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
        Self {
            code,
            message: format!("{what} (line {line}, column {column})"),
            offset,
            line,
            column,
        }
    }

    /// `cypher_unsupported`: `construct` is valid openCypher outside the
    /// portable subset.
    pub fn unsupported(query: &str, offset: usize, construct: impl fmt::Display) -> Self {
        Self::at(
            "cypher_unsupported",
            query,
            offset,
            format_args!("{construct} is not supported by the portable openCypher engine"),
        )
    }

    /// `cypher_invalid`: a syntax error.
    pub fn invalid(query: &str, offset: usize, what: impl fmt::Display) -> Self {
        Self::at("cypher_invalid", query, offset, what)
    }

    /// `cypher_too_complex`: a limit.
    pub fn too_complex(query: &str, offset: usize, what: impl fmt::Display) -> Self {
        Self::at("cypher_too_complex", query, offset, what)
    }
}

impl fmt::Display for CypherError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CypherError {}
