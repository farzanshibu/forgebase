//! The JSON error envelope.
//!
//! Every error response, from a handler, from routing (404/405), from a
//! timeout or from a caught panic, has the same shape:
//!
//! ```json
//! {"error":{"code":"snake_case","message":"…","request_id":"…"}}
//! ```
//!
//! Handlers return [`ApiError`]. Its `IntoResponse` writes the body with an
//! empty `request_id` and stores the error in the response extensions; the
//! request-context middleware ([`crate::context`]) re-renders it with the
//! request id, and converts any other non-JSON error response it sees.

use std::borrow::Cow;

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use forgebase_core::{ErrorCode, ErrorKind};
use serde::Serialize;

/// An error returned to an HTTP client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    status: StatusCode,
    code: Cow<'static, str>,
    message: String,
    details: Option<serde_json::Value>,
}

/// The `error` object of the envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct ErrorDetail {
    /// Stable machine-readable code.
    #[schema(value_type = String)]
    pub code: Cow<'static, str>,
    /// Human-readable message.
    pub message: String,
    /// Structured context, e.g. per-field validation errors
    /// (`{"fields": {"name": {"code": "required", "message": "..."}}}`).
    /// Additive and optional (ADR-0001 §6).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<Object>)]
    pub details: Option<serde_json::Value>,
    /// Id of the request (also in the `x-request-id` response header).
    pub request_id: String,
}

/// The whole error body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct ErrorEnvelope {
    /// The error.
    pub error: ErrorDetail,
}

impl ApiError {
    /// An error with an explicit status, code and message.
    pub fn new(status: StatusCode, code: impl Into<Cow<'static, str>>, message: impl Into<String>) -> Self {
        Self {
            status,
            code: code.into(),
            message: message.into(),
            details: None,
        }
    }

    /// Same error with structured `details`.
    #[must_use]
    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }

    /// `400` with an explicit code.
    pub fn bad_request(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, message)
    }

    /// The structured details, if any.
    pub fn details(&self) -> Option<&serde_json::Value> {
        self.details.as_ref()
    }

    /// `404 not_found`.
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", message)
    }

    /// `500 internal_error` with a generic message.
    pub fn internal() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "internal server error",
        )
    }

    /// Converts a typed ForgeBase error. Internal errors are logged in full
    /// and shown to the client only as a generic message.
    pub fn from_error<E: ErrorCode>(err: &E) -> Self {
        let kind = err.kind();
        let status = status_for(kind);
        if kind.is_client_visible() {
            if status.is_server_error() {
                tracing::warn!(code = err.code(), error = %err, "request failed");
            }
            Self::new(status, err.code(), err.to_string())
        } else {
            tracing::error!(code = err.code(), error = %err, "internal error");
            Self::new(status, err.code(), "internal server error")
        }
    }

    /// HTTP status.
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// Error code.
    pub fn code(&self) -> &str {
        &self.code
    }

    /// Message.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The error envelope for a given request id.
    pub fn envelope(&self, request_id: &str) -> ErrorEnvelope {
        ErrorEnvelope {
            error: ErrorDetail {
                code: self.code.clone(),
                message: self.message.clone(),
                details: self.details.clone(),
                request_id: request_id.to_owned(),
            },
        }
    }

    /// Generic error for a bare status produced outside a handler
    /// (routing, timeouts, panics).
    pub fn from_status(status: StatusCode) -> Self {
        let code = match status {
            StatusCode::BAD_REQUEST => "bad_request",
            StatusCode::UNAUTHORIZED => "unauthenticated",
            StatusCode::FORBIDDEN => "forbidden",
            StatusCode::NOT_FOUND => "not_found",
            StatusCode::METHOD_NOT_ALLOWED => "method_not_allowed",
            StatusCode::REQUEST_TIMEOUT => "request_timeout",
            StatusCode::PAYLOAD_TOO_LARGE => "payload_too_large",
            StatusCode::UNSUPPORTED_MEDIA_TYPE => "unsupported_media_type",
            StatusCode::UNPROCESSABLE_ENTITY => "unprocessable_entity",
            StatusCode::TOO_MANY_REQUESTS => "rate_limited",
            StatusCode::SERVICE_UNAVAILABLE => "unavailable",
            s if s.is_server_error() => "internal_error",
            _ => "request_error",
        };
        let message = if status.is_server_error() && status != StatusCode::SERVICE_UNAVAILABLE {
            "internal server error".to_owned()
        } else {
            status.canonical_reason().unwrap_or("request failed").to_lowercase()
        };
        Self::new(status, code, message)
    }
}

/// HTTP status for an error kind.
pub fn status_for(kind: ErrorKind) -> StatusCode {
    match kind {
        ErrorKind::InvalidInput => StatusCode::BAD_REQUEST,
        ErrorKind::Unauthenticated => StatusCode::UNAUTHORIZED,
        ErrorKind::Forbidden => StatusCode::FORBIDDEN,
        ErrorKind::NotFound => StatusCode::NOT_FOUND,
        ErrorKind::Conflict => StatusCode::CONFLICT,
        ErrorKind::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
        ErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

impl From<forgebase_data::DataError> for ApiError {
    /// The DataError → HTTP mapping. Status comes from the error's
    /// [`ErrorKind`] (ADR-0001 §7): validation, bad filters and bad SQL are
    /// 400, `Unauthenticated` 401, `Forbidden` 403, missing things 404,
    /// duplicates, version and schema conflicts 409, a missing password
    /// hasher 503, storage and catalog failures 500 (message hidden).
    /// Validation errors carry per-field details.
    fn from(e: forgebase_data::DataError) -> Self {
        let mut api = Self::from_error(&e);
        if let forgebase_data::DataError::Validation(v) = &e {
            api = api.with_details(serde_json::json!({ "fields": v.fields }));
        }
        api
    }
}

impl From<forgebase_db::DbError> for ApiError {
    fn from(e: forgebase_db::DbError) -> Self {
        Self::from_error(&e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut res = (self.status, Json(self.envelope(""))).into_response();
        res.extensions_mut().insert(self);
        res
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Boom;

    impl std::fmt::Display for Boom {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("secret sql detail")
        }
    }
    impl std::error::Error for Boom {}
    impl ErrorCode for Boom {
        fn code(&self) -> &'static str {
            "boom"
        }
        fn kind(&self) -> ErrorKind {
            ErrorKind::Internal
        }
    }

    #[test]
    fn internal_messages_are_not_exposed() {
        let e = ApiError::from_error(&Boom);
        assert_eq!(e.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(e.code(), "boom");
        assert_eq!(e.message(), "internal server error");
    }

    #[test]
    fn statuses_map_to_codes() {
        assert_eq!(
            ApiError::from_status(StatusCode::METHOD_NOT_ALLOWED).code(),
            "method_not_allowed"
        );
        assert_eq!(
            ApiError::from_status(StatusCode::INTERNAL_SERVER_ERROR).message(),
            "internal server error"
        );
        assert_eq!(status_for(ErrorKind::Unavailable), StatusCode::SERVICE_UNAVAILABLE);
    }
}
