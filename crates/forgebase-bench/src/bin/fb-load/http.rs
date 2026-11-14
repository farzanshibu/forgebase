//! A minimal keep-alive HTTP/1.1 client over a tokio socket: one connection,
//! `Content-Length` and chunked responses, JSON and raw bodies.

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// A request body.
pub(crate) enum Payload<'a> {
    /// No body.
    None,
    /// A JSON document.
    Json(&'a Value),
    /// Raw bytes with their content type.
    Raw(&'a [u8], &'a str),
}

/// A response. `status` is 0 when the request failed (connection error).
#[derive(Debug, Default)]
pub(crate) struct Reply {
    /// Status code.
    pub(crate) status: u16,
    /// Header names (lower case) and values.
    pub(crate) headers: Vec<(String, String)>,
    /// The body, when the caller asked to keep it.
    pub(crate) body: Vec<u8>,
    /// Length of the body as received.
    pub(crate) body_len: usize,
}

impl Reply {
    /// A header's value.
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    /// The body as JSON (`null` when it is not).
    pub(crate) fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

/// One keep-alive HTTP/1.1 connection.
pub(crate) struct Conn {
    addr: String,
    stream: Option<TcpStream>,
    buf: Vec<u8>,
    chunk: Vec<u8>,
}

impl Conn {
    pub(crate) fn new(addr: &str) -> Self {
        Self {
            addr: addr.to_owned(),
            stream: None,
            buf: Vec::with_capacity(8192),
            chunk: vec![0; 64 * 1024],
        }
    }

    /// Sends a request. Reconnects once when the server closed the idle
    /// connection. `keep_body` copies the body into the reply (large downloads
    /// only need their length).
    pub(crate) async fn send(
        &mut self,
        method: &str,
        path: &str,
        token: Option<&str>,
        headers: &[(&str, &str)],
        payload: Payload<'_>,
        keep_body: bool,
    ) -> Reply {
        for attempt in 0..2 {
            if self.stream.is_none() {
                let s = TcpStream::connect(&self.addr).await.expect("connect");
                s.set_nodelay(true).ok();
                self.stream = Some(s);
            }
            match self.try_send(method, path, token, headers, &payload, keep_body).await {
                Ok(r) => return r,
                Err(_) => {
                    self.stream = None;
                    if attempt == 1 {
                        return Reply::default();
                    }
                }
            }
        }
        Reply::default()
    }

    async fn try_send(
        &mut self,
        method: &str,
        path: &str,
        token: Option<&str>,
        headers: &[(&str, &str)],
        payload: &Payload<'_>,
        keep_body: bool,
    ) -> std::io::Result<Reply> {
        let json_text;
        let (body, ctype): (&[u8], Option<&str>) = match payload {
            Payload::None => (&[], None),
            Payload::Json(v) => {
                json_text = v.to_string();
                (json_text.as_bytes(), Some("application/json"))
            }
            Payload::Raw(b, t) => (b, Some(t)),
        };
        let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {}\r\n", self.addr);
        if let Some(t) = token {
            req.push_str(&format!("Authorization: Bearer {t}\r\n"));
        }
        for (k, v) in headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        if let Some(t) = ctype {
            req.push_str(&format!("Content-Type: {t}\r\nContent-Length: {}\r\n", body.len()));
        }
        req.push_str("\r\n");
        let s = self.stream.as_mut().expect("stream");
        s.write_all(req.as_bytes()).await?;
        if !body.is_empty() {
            s.write_all(body).await?;
        }
        self.buf.clear();
        let head_end = loop {
            let n = s.read(&mut self.chunk).await?;
            if n == 0 {
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            self.buf.extend_from_slice(&self.chunk[..n]);
            if let Some(pos) = self.buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos + 4;
            }
        };
        let head = String::from_utf8_lossy(&self.buf[..head_end]).into_owned();
        let mut lines = head.lines();
        let status: u16 = lines
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        let hdrs: Vec<(String, String)> = lines
            .filter_map(|l| l.split_once(':'))
            .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
            .collect();
        let find = |n: &str| hdrs.iter().find(|(k, _)| k == n).map(|(_, v)| v.as_str());
        let chunked = find("transfer-encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked"));
        let content_length = find("content-length")
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(0);
        let no_body = method == "HEAD" || status == 204 || status == 304;
        let close = find("connection").is_some_and(|v| v.eq_ignore_ascii_case("close"));
        let body_bytes: Vec<u8>;
        let body_len;
        if no_body {
            body_bytes = Vec::new();
            body_len = 0;
        } else if chunked {
            while !self.buf[head_end..].ends_with(b"0\r\n\r\n") {
                let n = s.read(&mut self.chunk).await?;
                if n == 0 {
                    return Err(std::io::ErrorKind::UnexpectedEof.into());
                }
                self.buf.extend_from_slice(&self.chunk[..n]);
            }
            body_bytes = dechunk(&self.buf[head_end..]);
            body_len = body_bytes.len();
        } else {
            while self.buf.len() < head_end + content_length {
                let n = s.read(&mut self.chunk).await?;
                if n == 0 {
                    return Err(std::io::ErrorKind::UnexpectedEof.into());
                }
                self.buf.extend_from_slice(&self.chunk[..n]);
            }
            body_len = content_length;
            body_bytes = if keep_body {
                self.buf[head_end..head_end + content_length].to_vec()
            } else {
                Vec::new()
            };
        }
        if close {
            self.stream = None;
        }
        Ok(Reply {
            status,
            headers: hdrs,
            body: if keep_body { body_bytes } else { Vec::new() },
            body_len,
        })
    }

    /// A request with an optional JSON body; the status and the body.
    pub(crate) async fn call(
        &mut self,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: Option<&Value>,
    ) -> (u16, Vec<u8>) {
        let payload = body.map_or(Payload::None, Payload::Json);
        let r = self.send(method, path, token, &[], payload, true).await;
        (r.status, r.body)
    }

    /// [`Conn::call`] with the body parsed as JSON.
    pub(crate) async fn json(
        &mut self,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: Option<&Value>,
    ) -> (u16, Value) {
        let (s, b) = self.call(method, path, token, body).await;
        (s, serde_json::from_slice(&b).unwrap_or(Value::Null))
    }
}

/// Decodes a chunked body (`size\r\ndata\r\n ... 0\r\n\r\n`).
pub(crate) fn dechunk(mut b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(nl) = b.windows(2).position(|w| w == b"\r\n") {
        let size = usize::from_str_radix(
            String::from_utf8_lossy(&b[..nl]).split(';').next().unwrap_or("").trim(),
            16,
        )
        .unwrap_or(0);
        if size == 0 || b.len() < nl + 2 + size {
            break;
        }
        out.extend_from_slice(&b[nl + 2..nl + 2 + size]);
        b = b.get(nl + 2 + size + 2..).unwrap_or_default();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunked_bodies_are_decoded() {
        assert_eq!(dechunk(b"4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n"), b"Wikipedia");
        assert_eq!(dechunk(b"0\r\n\r\n"), b"");
    }

    #[tokio::test]
    async fn round_trip_against_a_local_server() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut b = vec![0; 4096];
            let _ = s.read(&mut b).await.unwrap();
            s.write_all(b"HTTP/1.1 201 Created\r\nContent-Length: 7\r\nX-Test: yes\r\n\r\n{\"a\":1}")
                .await
                .unwrap();
        });
        let mut c = Conn::new(&addr);
        let r = c
            .send(
                "POST",
                "/x",
                Some("t"),
                &[("X-A", "b")],
                Payload::Raw(b"abc", "text/plain"),
                true,
            )
            .await;
        assert_eq!((r.status, r.body_len), (201, 7));
        assert_eq!(r.header("x-test"), Some("yes"));
        assert_eq!(r.json()["a"], 1);
    }
}
