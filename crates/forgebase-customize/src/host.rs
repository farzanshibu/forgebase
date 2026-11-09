//! Host-header routing: a request whose `Host` is a verified custom domain of
//! a realm acts in that realm.
//!
//! [`route`] is a middleware for the outermost layer of the server. For a
//! verified host it **sets** `X-ForgeBase-Realm` (replacing whatever the
//! client sent, so a custom domain cannot be used to reach another realm) and
//! adds a [`ResolvedHost`] request extension; the tenancy and auth layers then
//! treat the request as a request for that realm. For any other host the
//! request passes unchanged (the platform host serves every realm through the
//! header, as before).
//!
//! TLS is not terminated by ForgeBase: the reverse proxy in front holds the
//! certificates of the custom hosts and forwards the original `Host` (or
//! `X-Forwarded-Host`, honoured only with `trust_forwarded_host`).

use axum::extract::{Request, State};
use axum::http::{HeaderName, HeaderValue, header};
use axum::middleware::Next;
use axum::response::Response;

use crate::service::CustomizeService;

/// Set on requests that arrived on a verified custom domain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedHost {
    /// The realm of the domain.
    pub realm: String,
    /// The matched host.
    pub host: String,
}

const REALM_HEADER: HeaderName = HeaderName::from_static("x-forgebase-realm");
const FORWARDED_HOST: HeaderName = HeaderName::from_static("x-forwarded-host");

/// Lower-cases a `Host` value and removes the port and a trailing dot.
pub fn host_of(value: &str) -> String {
    let v = value.trim();
    // `[::1]:8080` style hosts are never custom domains.
    let v = if v.starts_with('[') {
        v
    } else {
        v.rsplit_once(':').map_or(v, |(h, _)| h)
    };
    v.trim_end_matches('.').to_ascii_lowercase()
}

/// The middleware; install with `axum::middleware::from_fn_with_state(svc, route)`.
pub async fn route(State(svc): State<CustomizeService>, mut req: Request, next: Next) -> Response {
    let raw = if svc.config().trust_forwarded_host {
        req.headers()
            .get(&FORWARDED_HOST)
            .or_else(|| req.headers().get(header::HOST))
    } else {
        req.headers().get(header::HOST)
    };
    // With several forwarded hosts the first is the client-facing one.
    let host = raw
        .and_then(|v| v.to_str().ok())
        .map(|v| host_of(v.split(',').next().unwrap_or_default()));
    if let Some(host) = host
        && let Some(realm) = svc.realm_for_host(&host)
        && let Ok(v) = HeaderValue::from_str(&realm)
    {
        req.headers_mut().insert(REALM_HEADER, v);
        req.extensions_mut().insert(ResolvedHost { realm, host });
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::host_of;

    #[test]
    fn host_normalization() {
        assert_eq!(host_of("Login.Example.com:8443"), "login.example.com");
        assert_eq!(host_of("login.example.com."), "login.example.com");
        assert_eq!(host_of("[::1]:80"), "[::1]:80");
    }
}
