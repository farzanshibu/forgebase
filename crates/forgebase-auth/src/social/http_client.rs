//! Outbound HTTP for provider calls (token exchange, userinfo, JWKS,
//! discovery), behind a trait so production can plug a TLS-capable client and
//! tests an in-process one.
//!
//! The built-in [`PlainHttpClient`] speaks HTTP/1.1 over a plain TCP stream
//! with timeouts and a response size cap. It does **not** do TLS: ForgeBase
//! adds no TLS stack to the tree without a decision (ADR-0009 §9), so real
//! identity providers need either a TLS terminating egress proxy or an
//! `HttpClient` implementation backed by a TLS library. `https://` URLs fail
//! with a clear error instead of silently downgrading.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
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
                "https provider endpoints need a TLS-capable HttpClient (see ADR-0009)".into(),
            ));
        }
        let host = url
            .host_str()
            .ok_or_else(|| AuthError::Provider("provider URL has no host".into()))?;
        let port = url.port_or_known_default().unwrap_or(80);
        let mut target = url.path().to_owned();
        if let Some(q) = url.query() {
            target.push('?');
            target.push_str(q);
        }
        let mut head = format!(
            "{} {target} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\nUser-Agent: forgebase-auth\r\nAccept: application/json\r\n",
            req.method
        );
        for (k, v) in &req.headers {
            if k.contains(['\r', '\n']) || v.contains(['\r', '\n']) {
                return Err(AuthError::Provider("bad header".into()));
            }
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        if req.method == "POST" {
            head.push_str(&format!("content-length: {}\r\n", req.body.len()));
        }
        head.push_str("\r\n");

        let mut stream = TcpStream::connect((host, port))
            .await
            .map_err(|_| AuthError::Provider("could not reach the provider".into()))?;
        let io_err = |_| AuthError::Provider("provider connection failed".into());
        stream.write_all(head.as_bytes()).await.map_err(io_err)?;
        stream.write_all(&req.body).await.map_err(io_err)?;
        let mut raw = Vec::new();
        (&mut stream)
            .take(self.max_body as u64 + 16 * 1024)
            .read_to_end(&mut raw)
            .await
            .map_err(io_err)?;
        parse_response(&raw, self.max_body)
    }
}

fn parse_response(raw: &[u8], max_body: usize) -> Result<HttpResponse, AuthError> {
    let bad = || AuthError::Provider("malformed provider response".into());
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").ok_or_else(bad)?;
    let head = std::str::from_utf8(&raw[..split]).map_err(|_| bad())?;
    let mut lines = head.split("\r\n");
    let status: u16 = lines
        .next()
        .and_then(|l| l.split(' ').nth(1))
        .and_then(|s| s.parse().ok())
        .ok_or_else(bad)?;
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    let body = &raw[split + 4..];
    let chunked = headers
        .iter()
        .any(|(k, v)| k == "transfer-encoding" && v.to_ascii_lowercase().contains("chunked"));
    let body = if chunked {
        dechunk(body).ok_or_else(bad)?
    } else {
        body.to_vec()
    };
    if body.len() > max_body {
        return Err(AuthError::Provider("provider response too large".into()));
    }
    Ok(HttpResponse { status, headers, body })
}

fn dechunk(mut b: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let eol = b.windows(2).position(|w| w == b"\r\n")?;
        let size = usize::from_str_radix(std::str::from_utf8(&b[..eol]).ok()?.split(';').next()?.trim(), 16).ok()?;
        b = &b[eol + 2..];
        if size == 0 {
            return Some(out);
        }
        if b.len() < size + 2 {
            return None;
        }
        out.extend_from_slice(&b[..size]);
        b = &b[size + 2..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_content_length_and_chunked() {
        let r = parse_response(b"HTTP/1.1 200 OK\r\nContent-Type: a/b\r\n\r\n{\"a\":1}", 100).unwrap();
        assert_eq!((r.status, r.body.as_slice()), (200, b"{\"a\":1}".as_slice()));
        let r = parse_response(
            b"HTTP/1.1 201 X\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n",
            100,
        )
        .unwrap();
        assert_eq!((r.status, r.body.as_slice()), (201, b"abcde".as_slice()));
        assert!(parse_response(b"HTTP/1.1 200 OK\r\n\r\n0123456789", 5).is_err());
        assert!(parse_response(b"garbage", 5).is_err());
    }

    #[tokio::test]
    async fn https_is_refused_not_downgraded() {
        let e = PlainHttpClient::default()
            .send(HttpRequest::get("https://example.com/"))
            .await
            .unwrap_err();
        assert!(e.to_string().contains("TLS"));
    }
}
