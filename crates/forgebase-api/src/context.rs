//! Per-request middleware: request ids, the error envelope, security
//! headers and HTTP metrics.

use std::sync::Arc;
use std::time::Instant;

use axum::Json;
use axum::extract::{MatchedPath, Request};
use axum::http::header::{CONTENT_TYPE, HeaderName, HeaderValue};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use forgebase_observability::names;

use crate::error::ApiError;

/// Header carrying the request id, in and out.
pub const X_REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

/// The id of the current request, available as a request extension.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestId(Arc<str>);

impl RequestId {
    /// The id as a string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Accepts a client-supplied id only if it is short and made of safe
/// characters, so it cannot be used to inject into logs.
fn is_valid_request_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
}

/// Outermost middleware. Assigns a request id (keeping a valid incoming
/// `x-request-id`), wraps every error response in the JSON envelope, and
/// adds `x-request-id` and baseline security headers to every response.
pub async fn request_context(mut req: Request, next: Next) -> Response {
    let id = req
        .headers()
        .get(&X_REQUEST_ID)
        .and_then(|v| v.to_str().ok())
        .filter(|v| is_valid_request_id(v))
        .map(str::to_owned)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let header = HeaderValue::from_str(&id).unwrap_or_else(|_| HeaderValue::from_static("invalid"));
    req.headers_mut().insert(X_REQUEST_ID, header.clone());
    req.extensions_mut().insert(RequestId(id.as_str().into()));

    let mut res = next.run(req).await;
    res = envelope(res, &id);

    let headers = res.headers_mut();
    headers.insert(X_REQUEST_ID, header);
    headers
        .entry(HeaderName::from_static("x-content-type-options"))
        .or_insert(HeaderValue::from_static("nosniff"));
    headers
        .entry(HeaderName::from_static("x-frame-options"))
        .or_insert(HeaderValue::from_static("DENY"));
    headers
        .entry(HeaderName::from_static("referrer-policy"))
        .or_insert(HeaderValue::from_static("no-referrer"));
    res
}

fn envelope(mut res: Response, request_id: &str) -> Response {
    if let Some(err) = res.extensions_mut().remove::<ApiError>() {
        return rebuild(res, &err, request_id);
    }
    let status = res.status();
    if !(status.is_client_error() || status.is_server_error()) {
        return res;
    }
    let is_json = res
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"));
    if is_json {
        return res;
    }
    let err = ApiError::from_status(status);
    rebuild(res, &err, request_id)
}

fn rebuild(res: Response, err: &ApiError, request_id: &str) -> Response {
    let (mut parts, _) = res.into_parts();
    let body = Json(err.envelope(request_id)).into_response();
    let (body_parts, body) = body.into_parts();
    parts.headers.remove(axum::http::header::CONTENT_LENGTH);
    if let Some(ct) = body_parts.headers.get(CONTENT_TYPE) {
        parts.headers.insert(CONTENT_TYPE, ct.clone());
    }
    Response::from_parts(parts, body)
}

/// Records request count, latency and in-flight gauge, labelled by the
/// matched route template (never the raw path, to bound cardinality).
pub async fn track_metrics(req: Request, next: Next) -> Response {
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|p| p.as_str().to_owned())
        .unwrap_or_else(|| "<unmatched>".to_owned());
    let method = req.method().as_str().to_owned();
    let start = Instant::now();
    let _in_flight = InFlight::enter();
    let res = next.run(req).await;
    let status = res.status().as_u16().to_string();
    metrics::counter!(names::HTTP_REQUESTS_TOTAL, "method" => method.clone(), "route" => route.clone(), "status" => status)
        .increment(1);
    metrics::histogram!(names::HTTP_REQUEST_DURATION_SECONDS, "method" => method, "route" => route)
        .record(start.elapsed().as_secs_f64());
    res
}

/// Keeps the in-flight gauge right even when a request future is dropped
/// (client disconnect, timeout).
struct InFlight(metrics::Gauge);

impl InFlight {
    fn enter() -> Self {
        let g = metrics::gauge!(names::HTTP_REQUESTS_IN_FLIGHT);
        g.increment(1.0);
        Self(g)
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.decrement(1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_id_validation() {
        assert!(is_valid_request_id("abc-123_x.y:z"));
        assert!(!is_valid_request_id(""));
        assert!(!is_valid_request_id("has space"));
        assert!(!is_valid_request_id("new\nline"));
        assert!(!is_valid_request_id(&"a".repeat(129)));
    }
}
