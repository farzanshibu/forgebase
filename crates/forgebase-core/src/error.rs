//! The ForgeBase error model.
//!
//! Every crate defines its own explicit error enum (no stringly-typed errors
//! crossing crate boundaries). Each of those enums implements [`ErrorCode`],
//! which gives:
//!
//! * a stable, machine-readable snake_case `code` (part of the public API,
//!   never renamed once released), and
//! * an [`ErrorKind`], which the HTTP layer maps to a status code.
//!
//! The HTTP envelope is `{"error":{"code":"…","message":"…","request_id":"…"}}`.
//! Errors of kind [`ErrorKind::Internal`] never expose their internal message
//! to clients; the details go to the log, keyed by request id.

/// Broad category of an error. Decides the HTTP status and whether the
/// message is safe to show to a client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// The caller sent something invalid (HTTP 400).
    InvalidInput,
    /// The caller is not authenticated (HTTP 401).
    Unauthenticated,
    /// The caller is authenticated but not allowed (HTTP 403).
    Forbidden,
    /// The resource does not exist (HTTP 404).
    NotFound,
    /// The request conflicts with current state (HTTP 409).
    Conflict,
    /// The service or a dependency is temporarily unavailable (HTTP 503).
    Unavailable,
    /// A bug or an unexpected failure (HTTP 500). Message is not shown to clients.
    Internal,
}

impl ErrorKind {
    /// Whether the error message may be shown to an API client verbatim.
    pub fn is_client_visible(self) -> bool {
        !matches!(self, ErrorKind::Internal)
    }
}

/// Implemented by every ForgeBase error type.
pub trait ErrorCode: std::error::Error {
    /// Stable snake_case identifier, e.g. `database_unavailable`.
    fn code(&self) -> &'static str;
    /// Category used to pick the HTTP status.
    fn kind(&self) -> ErrorKind;
}
