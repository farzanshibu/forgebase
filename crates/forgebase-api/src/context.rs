//! Per-request middleware: request ids, the error envelope, security
//! headers and HTTP metrics.

use std::sync::Arc;
use std::time::Instant;

use axum::Json;
use axum::extract::{MatchedPath, Request, State};
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

/// HTTP observers told about every answered request (the SLO engine,
/// ADR-0035 §5).
pub type Observers = Arc<[Arc<dyn forgebase_observability::HttpObserver>]>;

/// Records request count, latency and in-flight gauge, labelled by the
/// matched route template (never the raw path, to bound cardinality), and
/// tells the [`Observers`].
pub async fn track_metrics(State(observers): State<Observers>, req: Request, next: Next) -> Response {
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|p| p.as_str().to_owned())
        .unwrap_or_else(|| "<unmatched>".to_owned());
    let method = req.method().as_str().to_owned();
    let start = Instant::now();
    let _in_flight = InFlight::enter();
    let res = next.run(req).await;
    let code = res.status().as_u16();
    let elapsed = start.elapsed().as_secs_f64();
    for o in observers.iter() {
        o.observe(&method, &route, code, elapsed);
    }
    let status = code.to_string();
    metrics::counter!(names::HTTP_REQUESTS_TOTAL, "method" => method.clone(), "route" => route.clone(), "status" => status)
        .increment(1);
    metrics::histogram!(names::HTTP_REQUEST_DURATION_SECONDS, "method" => method, "route" => route)
        .record(elapsed);
    res
}

/// The server span of a request (ADR-0035 §2): named `<METHOD> <route
/// template>` (never the raw path), kind `server`, with the caller's
/// `traceparent` as parent when [`forgebase_observability::trace::accept_incoming`]
/// accepts it. A caller may decide the sampling when it is trusted: behind
/// `[auth] trust_forwarded_for`, or a direct loopback client.
pub fn server_span(req: &Request) -> tracing::Span {
    use forgebase_observability::trace;
    let request_id = req.extensions().get::<RequestId>().map_or("-", RequestId::as_str);
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map_or("<unmatched>", |p| p.as_str());
    let method = req.method().as_str();
    let header = |n: &str| req.headers().get(n).and_then(|v| v.to_str().ok());
    let parent = trace::accept_incoming(
        header(trace::TRACEPARENT),
        header(trace::TRACESTATE),
        trace::caller_trusted(crate::routes::direct_loopback(req)),
    );
    let (parent, state) = match &parent {
        Some(p) => (p.traceparent(), p.trace_state.clone().unwrap_or_default()),
        None => (String::new(), String::new()),
    };
    // Path only in the local log: query strings may carry tokens. Exported
    // attributes are the dotted fields (no raw path).
    tracing::info_span!(
        "http",
        method = %method,
        path = %req.uri().path(),
        request_id = %request_id,
        otel.name = %format_args!("{method} {route}"),
        otel.kind = "server",
        otel.parent = %parent,
        otel.tracestate = %state,
        http.request.method = %method,
        http.route = %route,
        forgebase.request_id = %request_id,
        http.response.status_code = tracing::field::Empty,
        otel.status_code = tracing::field::Empty,
    )
}

/// Records the status of the answered request on its span, then logs it as
/// [`DefaultOnResponse`](tower_http::trace::DefaultOnResponse) does.
#[derive(Debug, Clone)]
pub struct RecordStatus(pub tower_http::trace::DefaultOnResponse);

impl<B> tower_http::trace::OnResponse<B> for RecordStatus {
    fn on_response(self, res: &axum::http::Response<B>, latency: std::time::Duration, span: &tracing::Span) {
        span.record("http.response.status_code", res.status().as_u16());
        if res.status().is_server_error() {
            span.record("otel.status_code", "error");
        }
        self.0.on_response(res, latency, span);
    }
}

/// Adds the server span's `traceparent` to the response while trace
/// propagation is on (`[observability.otlp] enabled`).
pub async fn trace_header(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    if forgebase_observability::trace::propagation_enabled()
        && let Some(c) = forgebase_observability::trace::current()
        && let Ok(v) = HeaderValue::from_str(&c.traceparent())
    {
        res.headers_mut()
            .insert(HeaderName::from_static(forgebase_observability::trace::TRACEPARENT), v);
    }
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
