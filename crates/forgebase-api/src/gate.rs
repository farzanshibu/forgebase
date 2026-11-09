//! Request gates: synchronous admission checks that run on every request
//! after authentication and tenancy and before the handler (plan-based API
//! rate limits of FB-24, ADR-0010).
//!
//! A gate sees the request's [`AccessContext`] (with realm, organization and
//! tenant resolved) and the path. It passes the request, optionally adding
//! response headers (`X-RateLimit-*`), or rejects it with an [`ApiError`]
//! (rendered in the error envelope) and headers (`Retry-After`).

use std::fmt;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{HeaderName, HeaderValue};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use forgebase_data::AccessContext;

use crate::error::ApiError;

/// Response headers a gate adds.
pub type GateHeaders = Vec<(HeaderName, HeaderValue)>;

/// A gate's answer.
#[derive(Debug)]
pub enum GateDecision {
    /// Continue; add these headers to the response.
    Pass(GateHeaders),
    /// Stop with this error and headers.
    Reject(ApiError, GateHeaders),
}

/// An admission check on the request path. Must not block: answer from
/// memory.
pub trait RequestGate: Send + Sync + fmt::Debug {
    /// Decides for one request.
    fn admit(&self, ctx: &AccessContext, path: &str) -> GateDecision;
}

/// The middleware running every gate in order.
pub(crate) async fn run(State(gates): State<Arc<[Arc<dyn RequestGate>]>>, req: Request, next: Next) -> Response {
    let ctx = req
        .extensions()
        .get::<AccessContext>()
        .cloned()
        .unwrap_or_else(AccessContext::anonymous);
    let path = req.uri().path().to_owned();
    let mut extra: GateHeaders = Vec::new();
    for gate in gates.iter() {
        match gate.admit(&ctx, &path) {
            GateDecision::Pass(h) => extra.extend(h),
            GateDecision::Reject(err, h) => {
                let mut res = err.into_response();
                for (k, v) in h {
                    res.headers_mut().insert(k, v);
                }
                return res;
            }
        }
    }
    let mut res = next.run(req).await;
    for (k, v) in extra {
        res.headers_mut().insert(k, v);
    }
    res
}
