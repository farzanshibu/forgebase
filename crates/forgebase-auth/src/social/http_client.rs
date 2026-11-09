//! Outbound HTTP for provider calls (token exchange, userinfo, JWKS,
//! discovery), behind a trait so production can plug a TLS-capable client and
//! tests an in-process one.
//!
//! The auth service installs [`crate::net::HttpsClient`] (rustls + ring,
//! feature `https`, on by default; ADR-0015 §2) at start. [`PlainHttpClient`]
//! speaks HTTP/1.1 over plain TCP only and refuses `https://` with a clear
//! error instead of silently downgrading (builds without the `https`
//! feature, or a TLS-terminating egress proxy).

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use tokio::net::TcpStream;

use crate::AuthError;

/// Boxed future returned by [`HttpClient::send`].
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// An outbound request.
#[derive(Debug, Clone)]
pub struct HttpRequest {
    /// `GET` or `POST`.
    pub method: &'static str,
    /// Absolute URL.
    pub url: String,
    /// Extra headers.
    pub headers: Vec<(String, String)>,
    /// Body (for `POST`).
    pub body: Vec<u8>,
}

impl HttpRequest {
    /// A GET request.
    pub fn get(url: impl Into<String>) -> Self {
        Self {
            method: "GET",
            url: url.into(),
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    /// A form-encoded POST.
    pub fn post_form(url: impl Into<String>, fields: &[(&str, &str)]) -> Self {
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(fields)
            .finish()
            .into_bytes();
        Self {
            method: "POST",
            url: url.into(),
            headers: vec![("content-type".into(), "application/x-www-form-urlencoded".into())],
            body,
        }
    }

    /// Adds a header.
    pub fn header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.to_ascii_lowercase(), value.into()));
        self
    }
}

/// A response.
#[derive(Debug, Clone)]
pub struct HttpResponse {
    /// Status code.
    pub status: u16,
    /// Lower-cased header names.
    pub headers: Vec<(String, String)>,
    /// Body.
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// First header value with this (lower-case) name.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    /// Body as JSON (providers differ in content type, so it is not checked).
    pub fn json(&self) -> Result<serde_json::Value, AuthError> {
        serde_json::from_slice(&self.body).map_err(|_| AuthError::Provider("provider returned a non-JSON body".into()))
    }
}

/// Sends provider requests.
pub trait HttpClient: Send + Sync + std::fmt::Debug {
    /// Performs the request; non-2xx statuses are returned, not errors.
    fn send(&self, req: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, AuthError>>;
}

/// Plain `http://` client (see the module docs).
#[derive(Debug, Clone)]
pub struct PlainHttpClient {
    timeout: Duration,
    max_body: usize,
}

impl Default for PlainHttpClient {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(10),
            max_body: 1 << 20,
        }
    }
}

impl HttpClient for PlainHttpClient {
    fn send(&self, req: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, AuthError>> {
        Box::pin(async move {
            tokio::time::timeout(self.timeout, self.exchange(req))
                .await
                .map_err(|_| AuthError::Provider("provider request timed out".into()))?
        })
    }
}

impl PlainHttpClient {
    async fn exchange(&self, req: HttpRequest) -> Result<HttpResponse, AuthError> {
        let url = url::Url::parse(&req.url).map_err(|_| AuthError::Provider("bad provider URL".into()))?;
        if url.scheme() != "http" {
            return Err(AuthError::Provider(
                "https endpoints need a TLS-capable HttpClient (build with the `https` feature, ADR-0015)".into(),
            ));
        }
        let (host, port) = crate::net::host_port(&url, 80)?;
        let mut stream = TcpStream::connect((host.as_str(), port))
            .await
            .map_err(|_| AuthError::Provider("could not reach the provider".into()))?;
        crate::net::http1::exchange(&mut stream, &req, &url, self.max_body).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn https_is_refused_not_downgraded() {
        let e = PlainHttpClient::default()
            .send(HttpRequest::get("https://example.com/"))
            .await
            .unwrap_err();
        assert!(e.to_string().contains("TLS"));
    }
}
