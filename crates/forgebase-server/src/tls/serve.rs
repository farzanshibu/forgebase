//! Serving HTTPS, and the plain HTTP listener next to it.
//!
//! **HTTPS.** The TCP accept loop never waits on a handshake: each one runs
//! in its own task (at most [`MAX_HANDSHAKES`] at a time, each bounded by
//! [`HANDSHAKE_TIMEOUT`]), so a slow or silent client cannot stall others.
//! A finished handshake is served by hyper (HTTP/1.1 or HTTP/2, as ALPN
//! chose) with the same router, connection info and graceful shutdown as the
//! plain listener. HTTP/2 does not advertise the extended CONNECT protocol
//! (RFC 8441), so browsers open WebSockets (realtime) over HTTP/1.1, which
//! the realtime routes expect. A connection that negotiated `acme-tls/1`
//! (an ACME `tls-alpn-01` validation) is closed right after its handshake.
//!
//! **Plain HTTP** (`[tls] redirect_http`, or the `http-01` challenge):
//! answers `GET /.well-known/acme-challenge/<token>` from the certificate
//! store and redirects everything else to HTTPS with `308`. The target is
//! `[server] public_url` when it is an `https://` URL; otherwise the request's
//! host (one of the ACME domains in acme mode, else any well-formed host name)
//! with the HTTPS port, so the redirect never points at a host the operator
//! did not name.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{Path as AxPath, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use forgebase_observability::names;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder;
use hyper_util::service::TowerToHyperService;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::task::JoinSet;
use tokio_rustls::TlsAcceptor;
use tower::ServiceExt as _;

use super::certs::{ACME_TLS_ALPN, CertStore};

/// Most TLS handshakes in progress; further TCP connections are closed.
pub const MAX_HANDSHAKES: usize = 1024;
/// Longest TLS handshake.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Serves `app` over TLS on `listener` until `stop` fires, then waits for
/// open connections to finish (the caller bounds that wait).
///
/// # Errors
/// Never in practice: accept errors are logged and retried.
pub async fn serve_tls(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    app: Router,
    mut stop: oneshot::Receiver<()>,
) -> std::io::Result<()> {
    let graceful = hyper_util::server::graceful::GracefulShutdown::new();
    let builder = Builder::new(TokioExecutor::new());
    let make = app.into_make_service_with_connect_info::<SocketAddr>();
    let mut handshakes: JoinSet<Option<(tokio_rustls::server::TlsStream<TcpStream>, SocketAddr)>> = JoinSet::new();
    loop {
        tokio::select! {
            biased;
            _ = &mut stop => break,
            Some(done) = handshakes.join_next(), if !handshakes.is_empty() => {
                let Ok(Some((tls, peer))) = done else { continue };
                if tls.get_ref().1.alpn_protocol() == Some(ACME_TLS_ALPN) {
                    // tls-alpn-01: the validator only needed the handshake.
                    continue;
                }
                let Ok(svc) = make.clone().oneshot(peer).await;
                let conn = builder.serve_connection_with_upgrades(TokioIo::new(tls), TowerToHyperService::new(svc));
                let conn = graceful.watch(conn.into_owned());
                tokio::spawn(async move {
                    if let Err(e) = conn.await {
                        tracing::debug!(%peer, error = %e, "https connection ended with an error");
                    }
                });
            }
            r = listener.accept() => match r {
                Ok((tcp, peer)) => {
                    if handshakes.len() >= MAX_HANDSHAKES {
                        metrics::counter!(names::TLS_HANDSHAKE_FAILURES_TOTAL, "listener" => "https").increment(1);
                        continue;
                    }
                    let _ = tcp.set_nodelay(true);
                    let acceptor = acceptor.clone();
                    handshakes.spawn(async move {
                        match tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(tcp)).await {
                            Ok(Ok(tls)) => Some((tls, peer)),
                            Ok(Err(e)) => {
                                metrics::counter!(names::TLS_HANDSHAKE_FAILURES_TOTAL, "listener" => "https").increment(1);
                                tracing::debug!(%peer, error = %e, "TLS handshake failed");
                                None
                            }
                            Err(_) => {
                                metrics::counter!(names::TLS_HANDSHAKE_FAILURES_TOTAL, "listener" => "https").increment(1);
                                tracing::debug!(%peer, "TLS handshake timed out");
                                None
                            }
                        }
                    });
                }
                Err(e) => {
                    // Typically too many open files: back off instead of spinning.
                    tracing::warn!(error = %e, "HTTPS accept failed");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            },
        }
    }
    drop(listener);
    handshakes.abort_all();
    graceful.shutdown().await;
    Ok(())
}

/// Where the plain listener redirects to.
#[derive(Debug, Clone)]
pub struct RedirectTarget {
    /// `[server] public_url` when it is `https://`.
    pub public_url: Option<String>,
    /// HTTPS port as clients reach it (omitted from the URL when 443).
    pub https_port: u16,
    /// The ACME domains; empty in files mode.
    pub domains: Vec<String>,
}

/// The host name of a `Host` header value, lower-cased, without the port,
/// when it is a well-formed DNS name or IP literal.
fn host_of(value: &str) -> Option<String> {
    let v = value.trim();
    let host = if let Some(rest) = v.strip_prefix('[') {
        let (h, _) = rest.split_once(']')?;
        h.parse::<std::net::Ipv6Addr>().ok()?;
        return Some(format!("[{}]", h.to_ascii_lowercase()));
    } else {
        v.rsplit_once(':').map_or(
            v,
            |(h, port)| {
                if port.bytes().all(|b| b.is_ascii_digit()) { h } else { v }
            },
        )
    };
    let ok = !host.is_empty()
        && host.len() <= 253
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
        && !host.starts_with(['-', '.']);
    ok.then(|| host.to_ascii_lowercase())
}

impl RedirectTarget {
    /// The HTTPS URL for a request with `host` and `path_and_query`, or
    /// `None` when there is nothing sensible to redirect to.
    pub fn location(&self, host: Option<&str>, path_and_query: &str) -> Option<String> {
        let path = if path_and_query.starts_with('/') {
            path_and_query
        } else {
            "/"
        };
        if let Some(base) = &self.public_url {
            return Some(format!("{}{path}", base.trim_end_matches('/')));
        }
        let asked = host.and_then(host_of);
        let host = if self.domains.is_empty() {
            asked?
        } else {
            match asked {
                Some(h) if self.domains.contains(&h) => h,
                _ => self.domains[0].clone(),
            }
        };
        let port = if self.https_port == 443 {
            String::new()
        } else {
            format!(":{}", self.https_port)
        };
        Some(format!("https://{host}{port}{path}"))
    }
}

#[derive(Clone)]
struct PlainState {
    store: Arc<CertStore>,
    target: Arc<RedirectTarget>,
}

async fn challenge(State(s): State<PlainState>, AxPath(token): AxPath<String>) -> Response {
    match s.store.http_challenge(&token) {
        Some(answer) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"))],
            answer,
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "no such ACME challenge").into_response(),
    }
}

async fn redirect(State(s): State<PlainState>, req: Request) -> Response {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .or_else(|| req.uri().authority().map(axum::http::uri::Authority::as_str));
    let pq = req
        .uri()
        .path_and_query()
        .map_or("/", axum::http::uri::PathAndQuery::as_str);
    match s.target.location(host, pq).and_then(|l| HeaderValue::from_str(&l).ok()) {
        Some(location) => (
            StatusCode::PERMANENT_REDIRECT,
            [
                (header::LOCATION, location),
                (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
            ],
        )
            .into_response(),
        None => (StatusCode::BAD_REQUEST, "this server only answers over HTTPS").into_response(),
    }
}

/// The plain HTTP router: ACME `http-01` answers and redirects to HTTPS.
pub fn plain_router(store: Arc<CertStore>, target: RedirectTarget) -> Router {
    Router::new()
        .route("/.well-known/acme-challenge/{token}", axum::routing::get(challenge))
        .fallback(redirect)
        .with_state(PlainState {
            store,
            target: Arc::new(target),
        })
}

/// Serves the plain router on `listener` until `stop` fires.
pub async fn serve_plain(listener: TcpListener, router: Router, stop: oneshot::Receiver<()>) -> std::io::Result<()> {
    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            let _ = stop.await;
        })
        .await
}

/// A layer function adding `Strict-Transport-Security` to every response.
pub fn hsts_value(max_age: u64, include_subdomains: bool) -> Option<HeaderValue> {
    if max_age == 0 {
        return None;
    }
    let v = if include_subdomains {
        format!("max-age={max_age}; includeSubDomains")
    } else {
        format!("max-age={max_age}")
    };
    HeaderValue::from_str(&v).ok()
}

/// Adds the HSTS header to `app`'s responses (unless a handler set one).
pub fn with_hsts(app: Router, value: HeaderValue) -> Router {
    app.layer(axum::middleware::map_response(move |mut res: Response| {
        let value = value.clone();
        async move {
            res.headers_mut()
                .entry(header::STRICT_TRANSPORT_SECURITY)
                .or_insert(value);
            Ok::<_, Infallible>(res)
        }
    }))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn redirect_targets_never_leave_the_named_hosts() {
        let files = RedirectTarget {
            public_url: None,
            https_port: 8443,
            domains: vec![],
        };
        assert_eq!(
            files.location(Some("app.example.com:80"), "/a?b=1").as_deref(),
            Some("https://app.example.com:8443/a?b=1")
        );
        assert_eq!(files.location(Some("evil.com/x"), "/").as_deref(), None);
        assert_eq!(files.location(None, "/"), None);
        assert_eq!(
            files.location(Some("[::1]:80"), "/").as_deref(),
            Some("https://[::1]:8443/")
        );

        let acme = RedirectTarget {
            public_url: None,
            https_port: 443,
            domains: vec!["a.example.com".into(), "b.example.com".into()],
        };
        assert_eq!(
            acme.location(Some("B.example.com"), "/x").as_deref(),
            Some("https://b.example.com/x")
        );
        assert_eq!(
            acme.location(Some("evil.com"), "/x").as_deref(),
            Some("https://a.example.com/x")
        );

        let public = RedirectTarget {
            public_url: Some("https://fb.example.com/".into()),
            https_port: 443,
            domains: vec![],
        };
        assert_eq!(
            public.location(Some("evil.com"), "/p").as_deref(),
            Some("https://fb.example.com/p")
        );
    }

    #[test]
    fn hsts_header_values() {
        assert_eq!(hsts_value(0, true), None);
        assert_eq!(hsts_value(31_536_000, false).unwrap(), "max-age=31536000");
        assert_eq!(hsts_value(60, true).unwrap(), "max-age=60; includeSubDomains");
    }
}
