//! One HTTP/1.1 request/response exchange over an already connected stream.
//!
//! Requests always send `Connection: close` and `Accept-Encoding: identity`.
//! Responses are framed by `Transfer-Encoding: chunked`, `Content-Length`, or
//! the end of the stream, and capped (64 KiB of headers, `max_body` of body).
//! Redirects are returned, not followed.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::AuthError;
use crate::social::http_client::{HttpRequest, HttpResponse};

const MAX_HEAD: usize = 64 * 1024;

fn bad() -> AuthError {
    AuthError::Provider("malformed response from the remote server".into())
}

fn io_err(_: std::io::Error) -> AuthError {
    AuthError::Provider("connection to the remote server failed".into())
}

/// The request head (request line and headers) for `req` sent to `url`.
pub(crate) fn request_head(req: &HttpRequest, url: &url::Url) -> Result<String, AuthError> {
    let host = url
        .host_str()
        .ok_or_else(|| AuthError::Provider("URL has no host".into()))?;
    let host_header = match url.port() {
        Some(p) => format!("{host}:{p}"),
        None => host.to_owned(),
    };
    let mut target = url.path().to_owned();
    if target.is_empty() {
        target.push('/');
    }
    if let Some(q) = url.query() {
        target.push('?');
        target.push_str(q);
    }
    if !matches!(req.method, "GET" | "POST" | "PUT" | "DELETE" | "PATCH" | "HEAD") {
        return Err(AuthError::Provider("unsupported HTTP method".into()));
    }
    let mut head = format!(
        "{} {target} HTTP/1.1\r\nHost: {host_header}\r\nConnection: close\r\nUser-Agent: forgebase\r\nAccept-Encoding: identity\r\n",
        req.method
    );
    if !req.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("accept")) {
        head.push_str("Accept: application/json\r\n");
    }
    for (k, v) in &req.headers {
        let k_ok = !k.is_empty() && k.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b));
        if !k_ok || v.contains(['\r', '\n', '\0']) {
            return Err(AuthError::Provider("bad header".into()));
        }
        if k.eq_ignore_ascii_case("content-length") || k.eq_ignore_ascii_case("host") {
            continue;
        }
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    if !req.body.is_empty() || matches!(req.method, "POST" | "PUT" | "PATCH") {
        head.push_str(&format!("Content-Length: {}\r\n", req.body.len()));
    }
    head.push_str("\r\n");
    Ok(head)
}

/// Sends `req` on `stream` and reads the response.
pub(crate) async fn exchange<S>(
    stream: &mut S,
    req: &HttpRequest,
    url: &url::Url,
    max_body: usize,
) -> Result<HttpResponse, AuthError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let head = request_head(req, url)?;
    let mut out = head.into_bytes();
    out.extend_from_slice(&req.body);
    stream.write_all(&out).await.map_err(io_err)?;
    stream.flush().await.map_err(io_err)?;
    read_response(stream, max_body, req.method == "HEAD").await
}

/// Buffered reader over a stream.
struct Buf<'a, R> {
    r: &'a mut R,
    buf: Vec<u8>,
    eof: bool,
}

impl<R: AsyncRead + Unpin> Buf<'_, R> {
    /// Reads more bytes; `false` at the end of the stream. A TLS peer that
    /// closes without `close_notify` counts as the end of the stream.
    async fn fill(&mut self) -> Result<bool, AuthError> {
        if self.eof {
            return Ok(false);
        }
        let mut chunk = [0u8; 8192];
        match self.r.read(&mut chunk).await {
            Ok(0) => {
                self.eof = true;
                Ok(false)
            }
            Ok(n) => {
                self.buf.extend_from_slice(&chunk[..n]);
                Ok(true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                self.eof = true;
                Ok(false)
            }
            Err(e) => Err(io_err(e)),
        }
    }

    /// Bytes up to (not including) `pat`, which is consumed.
    async fn until(&mut self, pat: &[u8], limit: usize) -> Result<Vec<u8>, AuthError> {
        loop {
            if let Some(p) = self.buf.windows(pat.len()).position(|w| w == pat) {
                let out = self.buf[..p].to_vec();
                self.buf.drain(..p + pat.len());
                return Ok(out);
            }
            if self.buf.len() > limit {
                return Err(bad());
            }
            if !self.fill().await? {
                return Err(bad());
            }
        }
    }

    /// Exactly `n` bytes.
    async fn exact(&mut self, n: usize) -> Result<Vec<u8>, AuthError> {
        while self.buf.len() < n {
            if !self.fill().await? {
                return Err(bad());
            }
        }
        Ok(self.buf.drain(..n).collect())
    }

    /// Everything up to the end of the stream.
    async fn rest(&mut self, limit: usize) -> Result<Vec<u8>, AuthError> {
        while self.fill().await? {
            if self.buf.len() > limit {
                return Err(too_large());
            }
        }
        if self.buf.len() > limit {
            return Err(too_large());
        }
        Ok(std::mem::take(&mut self.buf))
    }
}

fn too_large() -> AuthError {
    AuthError::Provider("response from the remote server is too large".into())
}

/// Reads one response (skipping `1xx` interim responses).
pub(crate) async fn read_response<R: AsyncRead + Unpin>(
    r: &mut R,
    max_body: usize,
    head_request: bool,
) -> Result<HttpResponse, AuthError> {
    let mut b = Buf {
        r,
        buf: Vec::new(),
        eof: false,
    };
    loop {
        let head = b.until(b"\r\n\r\n", MAX_HEAD).await?;
        let head = std::str::from_utf8(&head).map_err(|_| bad())?;
        let mut lines = head.split("\r\n");
        let status_line = lines.next().ok_or_else(bad)?;
        if !status_line.starts_with("HTTP/1.") {
            return Err(bad());
        }
        let status: u16 = status_line
            .split(' ')
            .nth(1)
            .and_then(|s| s.parse().ok())
            .filter(|s| (100..600).contains(s))
            .ok_or_else(bad)?;
        let headers: Vec<(String, String)> = lines
            .filter_map(|l| l.split_once(':'))
            .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
            .collect();
        if (100..200).contains(&status) {
            continue;
        }
        let get = |name: &str| headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str());
        let body = if head_request || status == 204 || status == 304 {
            Vec::new()
        } else if get("transfer-encoding").is_some_and(|v| v.to_ascii_lowercase().contains("chunked")) {
            let mut out = Vec::new();
            loop {
                let line = b.until(b"\r\n", 1024).await?;
                let line = std::str::from_utf8(&line).map_err(|_| bad())?;
                let size = usize::from_str_radix(line.split(';').next().unwrap_or("").trim(), 16).map_err(|_| bad())?;
                if size == 0 {
                    // Trailers (usually none) end with an empty line.
                    loop {
                        if b.until(b"\r\n", MAX_HEAD).await?.is_empty() {
                            break;
                        }
                    }
                    break;
                }
                if out.len() + size > max_body {
                    return Err(too_large());
                }
                out.extend(b.exact(size).await?);
                if !b.exact(2).await?.eq(b"\r\n") {
                    return Err(bad());
                }
            }
            out
        } else if let Some(len) = get("content-length") {
            let len: usize = len.parse().map_err(|_| bad())?;
            if len > max_body {
                return Err(too_large());
            }
            b.exact(len).await?
        } else {
            b.rest(max_body).await?
        };
        return Ok(HttpResponse { status, headers, body });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn parse(raw: &[u8], max: usize) -> Result<HttpResponse, AuthError> {
        let mut r = raw;
        read_response(&mut r, max, false).await
    }

    #[tokio::test]
    async fn frames_by_length_chunks_and_close() {
        let r = parse(
            b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\n\r\n{\"a\":1}trailing-junk",
            100,
        )
        .await
        .unwrap();
        assert_eq!((r.status, r.body.as_slice()), (200, b"{\"a\":1}".as_slice()));
        let r = parse(
            b"HTTP/1.1 201 X\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2;ext=1\r\nde\r\n0\r\nX-T: 1\r\n\r\n",
            100,
        )
        .await
        .unwrap();
        assert_eq!((r.status, r.body.as_slice()), (201, b"abcde".as_slice()));
        let r = parse(b"HTTP/1.0 200 OK\r\n\r\nto-the-end", 100).await.unwrap();
        assert_eq!(r.body, b"to-the-end");
        let r = parse(b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 204 No Content\r\n\r\n", 100)
            .await
            .unwrap();
        assert_eq!(r.status, 204);
    }

    #[tokio::test]
    async fn caps_and_garbage_are_errors() {
        assert!(
            parse(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n0123456789", 5)
                .await
                .is_err()
        );
        assert!(parse(b"HTTP/1.1 200 OK\r\n\r\n0123456789", 5).await.is_err());
        assert!(
            parse(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n9\r\n012345678\r\n0\r\n\r\n",
                5
            )
            .await
            .is_err()
        );
        assert!(parse(b"garbage", 5).await.is_err());
        assert!(
            parse(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nshort", 100)
                .await
                .is_err()
        );
        assert!(parse(b"SSH-2.0-x\r\n\r\n", 100).await.is_err());
    }

    #[test]
    fn request_head_is_well_formed_and_rejects_injection() {
        let url = url::Url::parse("https://example.com/token?x=1").unwrap();
        let req = HttpRequest::post_form("https://example.com/token?x=1", &[("a", "b")]).header("Accept", "text/plain");
        let h = request_head(&req, &url).unwrap();
        assert!(h.starts_with("POST /token?x=1 HTTP/1.1\r\nHost: example.com\r\n"));
        assert!(h.contains("Content-Length: 3\r\n"));
        assert!(!h.contains("application/json"));
        let bad = HttpRequest::get("https://example.com/").header("x", "a\r\nInjected: 1");
        assert!(request_head(&bad, &url).is_err());
        let url = url::Url::parse("http://127.0.0.1:8080").unwrap();
        let h = request_head(&HttpRequest::get("http://127.0.0.1:8080"), &url).unwrap();
        assert!(h.starts_with("GET / HTTP/1.1\r\nHost: 127.0.0.1:8080\r\n"));
    }
}
