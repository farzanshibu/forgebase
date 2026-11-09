//! Typed errors. Every failure the SDK returns is an [`Error`].

use std::fmt;
use std::time::Duration;

use serde_json::Value;

/// What kind of failure an [`ApiError`] is, derived from the HTTP status and the server error code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorKind {
    /// 400 / 422: the request was malformed or failed validation (`details.fields` has per-field errors).
    Validation,
    /// 401: missing, expired or invalid credentials.
    Authentication,
    /// 403: the credentials are fine but the action is not allowed (policy, scope, quota-gated feature).
    Permission,
    /// 404.
    NotFound,
    /// 409 (unique violation, version mismatch) and 412.
    Conflict,
    /// 429 from rate limiting. Already retried `max_retries` times when returned.
    RateLimit,
    /// 429 because a quota is exhausted. Never retried: waiting will not help.
    QuotaExceeded,
    /// 5xx. 503 is retried before this is returned.
    Server,
    /// Any other non-2xx status.
    Other,
}

/// The server answered with a non-2xx status: the standard `{"error": {...}}` envelope, decoded.
#[derive(Debug, Clone)]
pub struct ApiError {
    /// HTTP status.
    pub status: u16,
    /// Stable machine-readable server error code (`validation_failed`, `not_found`, ...).
    pub code: String,
    /// Human-readable message.
    pub message: String,
    /// Server request id (`request_id` in the envelope or the `x-request-id` header), for support.
    pub request_id: Option<String>,
    /// Extra structured detail (`fields` for validation errors, the decision for quota errors).
    pub details: Option<Value>,
    /// How long the server asked the client to wait (`Retry-After`).
    pub retry_after: Option<Duration>,
    /// Parsed response body, when there was one.
    pub body: Option<Value>,
}

impl ApiError {
    /// The failure class.
    #[must_use]
    pub fn kind(&self) -> ErrorKind {
        match self.status {
            429 => {
                let c = self.code.to_ascii_lowercase();
                if c.contains("quota") || c.contains("limit_exceeded") || c.contains("usage_limit") {
                    ErrorKind::QuotaExceeded
                } else {
                    ErrorKind::RateLimit
                }
            }
            401 => ErrorKind::Authentication,
            403 => ErrorKind::Permission,
            404 => ErrorKind::NotFound,
            409 | 412 => ErrorKind::Conflict,
            400 | 422 => ErrorKind::Validation,
            s if s >= 500 => ErrorKind::Server,
            _ => ErrorKind::Other,
        }
    }

    /// Per-field validation errors (`details.fields`): `(field, code, message)`.
    #[must_use]
    pub fn field_errors(&self) -> Vec<(String, String, String)> {
        let Some(Value::Object(f)) = self.details.as_ref().and_then(|d| d.get("fields")) else {
            return Vec::new();
        };
        f.iter()
            .map(|(k, v)| {
                let code = v.get("code").and_then(Value::as_str).unwrap_or("invalid");
                let msg = v.get("message").and_then(Value::as_str).unwrap_or("");
                (k.clone(), code.to_owned(), msg.to_owned())
            })
            .collect()
    }

    /// Decodes a failed response.
    pub(crate) fn from_response(
        status: u16,
        body: Option<Value>,
        request_id_header: Option<String>,
        retry_after: Option<Duration>,
    ) -> Self {
        let env = body.as_ref().and_then(|b| b.get("error"));
        let text = |k: &str| env.and_then(|e| e.get(k)).and_then(Value::as_str).map(str::to_owned);
        Self {
            status,
            code: text("code").unwrap_or_else(|| "http_error".into()),
            message: text("message").unwrap_or_else(|| format!("HTTP {status}")),
            request_id: text("request_id").or(request_id_header),
            details: env.and_then(|e| e.get("details")).filter(|d| !d.is_null()).cloned(),
            retry_after,
            body,
        }
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({}): {}", self.status, self.code, self.message)?;
        if let Some(id) = &self.request_id {
            write!(f, " [request {id}]")?;
        }
        Ok(())
    }
}

impl std::error::Error for ApiError {}

/// Everything the SDK can fail with.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The server answered with an error status.
    #[error("{0}")]
    Api(Box<ApiError>),
    /// No HTTP response (DNS, refused connection, TLS, timeout).
    #[error("network error: {0}")]
    Network(String),
    /// A 2xx response that is not what the SDK expected.
    #[error("invalid response: {0}")]
    InvalidResponse(String),
    /// WebSocket / SSE failure (`code` is the server's realtime error code or an SDK one such as `closed`).
    #[error("realtime error {code}: {message}")]
    Realtime {
        /// Stable code.
        code: String,
        /// Human-readable message.
        message: String,
    },
    /// The client was configured wrongly (bad URL, missing credentials for a call).
    #[error("invalid configuration: {0}")]
    Config(String),
}

/// `Result` with the SDK [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// The API error, when the server answered with one.
    #[must_use]
    pub fn api(&self) -> Option<&ApiError> {
        match self {
            Self::Api(a) => Some(a.as_ref()),
            _ => None,
        }
    }

    /// HTTP status, `None` when there was no response.
    #[must_use]
    pub fn status(&self) -> Option<u16> {
        self.api().map(|a| a.status)
    }

    /// Server error code (`validation_failed`, ...) or the SDK's own (`network_error`, `invalid_response`).
    #[must_use]
    pub fn code(&self) -> &str {
        match self {
            Self::Api(a) => &a.code,
            Self::Network(_) => "network_error",
            Self::InvalidResponse(_) => "invalid_response",
            Self::Realtime { code, .. } => code,
            Self::Config(_) => "config_error",
        }
    }

    /// The failure class of an API error.
    #[must_use]
    pub fn kind(&self) -> Option<ErrorKind> {
        self.api().map(ApiError::kind)
    }

    /// Server request id, for support.
    #[must_use]
    pub fn request_id(&self) -> Option<&str> {
        self.api().and_then(|a| a.request_id.as_deref())
    }

    /// True for a 404.
    #[must_use]
    pub fn is_not_found(&self) -> bool {
        self.kind() == Some(ErrorKind::NotFound)
    }

    /// True for a 401.
    #[must_use]
    pub fn is_unauthenticated(&self) -> bool {
        self.kind() == Some(ErrorKind::Authentication)
    }

    /// True for a 403.
    #[must_use]
    pub fn is_forbidden(&self) -> bool {
        self.kind() == Some(ErrorKind::Permission)
    }

    /// True for a 409 / 412.
    #[must_use]
    pub fn is_conflict(&self) -> bool {
        self.kind() == Some(ErrorKind::Conflict)
    }

    /// True for a 400 / 422.
    #[must_use]
    pub fn is_validation(&self) -> bool {
        self.kind() == Some(ErrorKind::Validation)
    }

    /// True when a quota is exhausted.
    #[must_use]
    pub fn is_quota_exceeded(&self) -> bool {
        self.kind() == Some(ErrorKind::QuotaExceeded)
    }

    /// True for rate limiting.
    #[must_use]
    pub fn is_rate_limited(&self) -> bool {
        self.kind() == Some(ErrorKind::RateLimit)
    }
}

impl From<ApiError> for Error {
    fn from(e: ApiError) -> Self {
        Self::Api(Box::new(e))
    }
}

/// Parses `Retry-After` (delta-seconds only; HTTP-dates are not used by the server).
pub(crate) fn parse_retry_after(v: Option<&str>) -> Option<Duration> {
    let s = v?.trim();
    s.parse::<f64>()
        .ok()
        .filter(|n| n.is_finite() && *n >= 0.0)
        .map(Duration::from_secs_f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn err(status: u16, body: Value) -> ApiError {
        ApiError::from_response(status, Some(body), None, None)
    }

    #[test]
    fn classifies_statuses() {
        let k = |s, c: &str| err(s, json!({"error": {"code": c, "message": "m"}})).kind();
        assert_eq!(k(400, "validation_failed"), ErrorKind::Validation);
        assert_eq!(k(401, "unauthenticated"), ErrorKind::Authentication);
        assert_eq!(k(403, "forbidden"), ErrorKind::Permission);
        assert_eq!(k(404, "not_found"), ErrorKind::NotFound);
        assert_eq!(k(409, "conflict"), ErrorKind::Conflict);
        assert_eq!(k(412, "version_mismatch"), ErrorKind::Conflict);
        assert_eq!(k(429, "rate_limited"), ErrorKind::RateLimit);
        assert_eq!(k(429, "quota_exceeded"), ErrorKind::QuotaExceeded);
        assert_eq!(k(500, "internal"), ErrorKind::Server);
        assert_eq!(k(418, "teapot"), ErrorKind::Other);
    }

    #[test]
    fn decodes_envelope_and_fields() {
        let e = err(
            400,
            json!({"error": {"code": "validation_failed", "message": "bad", "request_id": "r1",
                "details": {"fields": {"title": {"code": "required", "message": "is required"}}}}}),
        );
        assert_eq!(e.code, "validation_failed");
        assert_eq!(e.request_id.as_deref(), Some("r1"));
        assert_eq!(
            e.field_errors(),
            vec![("title".into(), "required".into(), "is required".into())]
        );
        assert!(e.to_string().contains("[request r1]"));
    }

    #[test]
    fn falls_back_without_envelope() {
        let e = ApiError::from_response(502, None, Some("hdr".into()), None);
        assert_eq!(e.code, "http_error");
        assert_eq!(e.message, "HTTP 502");
        assert_eq!(e.request_id.as_deref(), Some("hdr"));
    }

    #[test]
    fn parses_retry_after() {
        assert_eq!(parse_retry_after(Some("2")), Some(Duration::from_secs(2)));
        assert_eq!(parse_retry_after(Some("nope")), None);
        assert_eq!(parse_retry_after(None), None);
    }
}
