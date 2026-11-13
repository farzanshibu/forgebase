//! A tiny HTTP/1.1 server for mail API mocks: records each request and
//! answers from a closure.
#![allow(dead_code, clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Debug, Clone)]
pub struct Seen {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Seen {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    /// A form field of an `application/x-www-form-urlencoded` body.
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).unwrap_or(serde_json::Value::Null)
    }

    pub fn form(&self, name: &str) -> Option<String> {
        url::form_urlencoded::parse(self.body.as_bytes())
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.into_owned())
    }
}

pub struct MockHttp {
    pub url: String,
    pub seen: Arc<Mutex<Vec<Seen>>>,
}

impl MockHttp {
    pub fn requests(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

pub async fn start(handler: impl Fn(&Seen) -> (u16, String) + Send + Sync + 'static) -> MockHttp {
    start_ext(move |r| {
        let (status, body) = handler(r);
        (status, Vec::new(), body)
    })
    .await
}

/// Like [`start`], with response headers (`Retry-After`, ...).
pub async fn start_ext(
    handler: impl Fn(&Seen) -> (u16, Vec<(String, String)>, String) + Send + Sync + 'static,
) -> MockHttp {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let handler = Arc::new(handler);
    let s2 = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = listener.accept().await else {
                return;
            };
            let (seen, handler) = (s2.clone(), handler.clone());
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let head_end = loop {
                    if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break p;
                    }
                    match s.read(&mut chunk).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                };
                let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
                let mut lines = head.split("\r\n");
                let mut first = lines.next().unwrap_or("").split(' ');
                let method = first.next().unwrap_or("").to_owned();
                let path = first.next().unwrap_or("").to_owned();
                let headers: Vec<(String, String)> = lines
                    .filter_map(|l| l.split_once(':'))
                    .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
                    .collect();
                let len: usize = headers
                    .iter()
                    .find(|(k, _)| k == "content-length")
                    .and_then(|(_, v)| v.parse().ok())
                    .unwrap_or(0);
                while buf.len() < head_end + 4 + len {
                    match s.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                }
                let body = String::from_utf8_lossy(&buf[head_end + 4..]).into_owned();
                let req = Seen {
                    method,
                    path,
                    headers,
                    body,
                };
                let (status, extra, out) = handler(&req);
                seen.lock().unwrap().push(req);
                let extra: String = extra.iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect();
                let resp = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{out}",
                    out.len()
                );
                let _ = s.write_all(resp.as_bytes()).await;
                let _ = s.shutdown().await;
            });
        }
    });
    MockHttp { url, seen }
}
