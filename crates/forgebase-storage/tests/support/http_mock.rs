//! A tiny HTTP/1.1 server for driver tests: every request goes to a handler
//! closure, every response closes the connection.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// A parsed request.
#[derive(Debug, Clone)]
pub struct Req {
    pub method: String,
    /// The path as sent (still percent-encoded).
    pub raw_path: String,
    /// The decoded path.
    pub path: String,
    /// The raw query string.
    pub raw_query: String,
    /// Decoded query parameters, in order.
    pub query: Vec<(String, String)>,
    /// Lower-case header names.
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

impl Req {
    pub fn q(&self, name: &str) -> Option<String> {
        self.query.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
    }

    pub fn h(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }
}

/// A response.
#[derive(Debug, Clone, Default)]
pub struct Resp {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Resp {
    pub fn new(status: u16) -> Self {
        Self {
            status,
            ..Self::default()
        }
    }

    pub fn body(mut self, b: impl Into<Vec<u8>>) -> Self {
        self.body = b.into();
        self
    }

    pub fn header(mut self, k: &str, v: impl Into<String>) -> Self {
        self.headers.push((k.to_owned(), v.into()));
        self
    }

    pub fn json(status: u16, v: &serde_json::Value) -> Self {
        Self::new(status)
            .header("Content-Type", "application/json")
            .body(v.to_string())
    }
}

pub type Handler = Arc<dyn Fn(&Req) -> Resp + Send + Sync>;

/// A running mock.
pub struct Server {
    pub addr: SocketAddr,
    /// `METHOD path?query-names` of every request, in order.
    pub log: Arc<Mutex<Vec<String>>>,
}

impl Server {
    pub async fn start(handler: Handler) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let log = Arc::new(Mutex::new(Vec::new()));
        let l2 = log.clone();
        tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else { return };
                let (h, l) = (handler.clone(), l2.clone());
                tokio::spawn(async move {
                    let _ = serve(sock, h, l).await;
                });
            }
        });
        Self { addr, log }
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Requests whose log line starts with `prefix`.
    pub fn count(&self, prefix: &str) -> usize {
        self.log.lock().unwrap().iter().filter(|l| l.starts_with(prefix)).count()
    }

    pub fn lines(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }
}

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 3 <= b.len() {
            if let Some(v) = std::str::from_utf8(&b[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())
            {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

async fn serve(mut sock: tokio::net::TcpStream, handler: Handler, log: Arc<Mutex<Vec<String>>>) -> std::io::Result<()> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 16384];
    let head_end = loop {
        let n = sock.read(&mut chunk).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap().split(' ');
    let (method, target) = (first.next().unwrap().to_owned(), first.next().unwrap().to_owned());
    let headers: HashMap<String, String> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    let len: usize = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    while buf.len() < head_end + len {
        let n = sock.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let body = buf[head_end..(head_end + len).min(buf.len())].to_vec();
    let (raw_path, raw_query) = target.split_once('?').unwrap_or((&target, ""));
    let query: Vec<(String, String)> = url::form_urlencoded::parse(raw_query.as_bytes()).into_owned().collect();
    let req = Req {
        method: method.clone(),
        raw_path: raw_path.to_owned(),
        path: pct_decode(raw_path),
        raw_query: raw_query.to_owned(),
        query,
        headers,
        body,
    };
    log.lock().unwrap().push(format!(
        "{method} {}?{}",
        req.path,
        req.query.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join("&")
    ));
    let resp = handler(&req);
    let custom_len = resp.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-length"));
    let mut out = format!("HTTP/1.1 {} X\r\nConnection: close\r\n", resp.status);
    if !custom_len {
        out.push_str(&format!("Content-Length: {}\r\n", resp.body.len()));
    }
    for (k, v) in &resp.headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str("\r\n");
    sock.write_all(out.as_bytes()).await?;
    if method != "HEAD" {
        sock.write_all(&resp.body).await?;
    }
    sock.shutdown().await
}
