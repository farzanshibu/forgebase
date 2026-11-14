//! A scripted HTTP server for hermetic tests: replies are consumed in order (the last one
//! repeats) and every request is recorded.
#![allow(dead_code, missing_docs)]

use std::sync::{Arc, Mutex};

use forgebase_sdk::{ForgeBase, RetryPolicy};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// One scripted reply.
#[derive(Clone)]
pub struct Reply {
    pub status: u16,
    pub content_type: &'static str,
    pub body: String,
}

/// 200 with a JSON body.
pub fn ok(v: Value) -> Reply {
    Reply {
        status: 200,
        content_type: "application/json",
        body: v.to_string(),
    }
}

/// A JSON body with a given status (201, 202, ...).
pub fn status(code: u16, v: Value) -> Reply {
    Reply {
        status: code,
        content_type: "application/json",
        body: v.to_string(),
    }
}

/// 204 with no body.
pub fn no_content() -> Reply {
    Reply {
        status: 204,
        content_type: "application/json",
        body: String::new(),
    }
}

/// 200 with a text body.
pub fn text(content_type: &'static str, body: &str) -> Reply {
    Reply {
        status: 200,
        content_type,
        body: body.to_owned(),
    }
}

/// The standard error envelope.
pub fn err(code: u16, name: &str) -> Reply {
    status(
        code,
        json!({"error": {"code": name, "message": name, "request_id": "req-1"}}),
    )
}

#[derive(Debug, Clone)]
pub struct Seen {
    pub method: String,
    /// Path and query string, as sent.
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Seen {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// The JSON body (`Null` when empty).
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }

    /// The path without its query string.
    pub fn route(&self) -> &str {
        self.path.split('?').next().unwrap_or("")
    }

    /// The query string decoded into pairs.
    pub fn query(&self) -> Vec<(String, String)> {
        let q = self.path.split_once('?').map_or("", |(_, q)| q);
        url::form_urlencoded::parse(q.as_bytes())
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect()
    }

    /// One query parameter.
    pub fn q(&self, name: &str) -> Option<String> {
        self.query().into_iter().find(|(k, _)| k == name).map(|(_, v)| v)
    }
}

pub struct Mock {
    pub base: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Mock {
    pub async fn start(script: Vec<Reply>) -> Self {
        let l = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let base = format!("http://{}", l.local_addr().expect("addr"));
        let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
        let (s2, script) = (seen.clone(), Arc::new(script));
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = l.accept().await else { return };
                let (seen, script) = (s2.clone(), script.clone());
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    let head_end = loop {
                        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            break i + 4;
                        }
                        match sock.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                    };
                    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
                    let mut lines = head.lines();
                    let mut first = lines.next().unwrap_or_default().split(' ');
                    let (method, path) = (
                        first.next().unwrap_or_default().to_owned(),
                        first.next().unwrap_or_default().to_owned(),
                    );
                    let headers: Vec<(String, String)> = lines
                        .filter_map(|l| {
                            l.split_once(':')
                                .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
                        })
                        .collect();
                    let len = headers
                        .iter()
                        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                        .and_then(|(_, v)| v.parse::<usize>().ok())
                        .unwrap_or(0);
                    while buf.len() < head_end + len {
                        match sock.read(&mut chunk).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                    }
                    let body = String::from_utf8_lossy(&buf[head_end.min(buf.len())..]).into_owned();
                    let n = {
                        let mut g = seen.lock().expect("lock");
                        g.push(Seen {
                            method,
                            path,
                            headers,
                            body,
                        });
                        g.len()
                    };
                    let r = script[(n - 1).min(script.len() - 1)].clone();
                    let out = format!(
                        "HTTP/1.1 {} X\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        r.status,
                        r.content_type,
                        r.body.len(),
                        r.body
                    );
                    let _ = sock.write_all(out.as_bytes()).await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        Self { base, seen }
    }

    pub fn calls(&self) -> Vec<Seen> {
        self.seen.lock().expect("lock").clone()
    }

    /// The only request seen.
    pub fn only(&self) -> Seen {
        let c = self.calls();
        assert_eq!(c.len(), 1, "expected exactly one request, saw {c:?}");
        c.into_iter().next().expect("one call")
    }

    /// The last request seen.
    pub fn last(&self) -> Seen {
        self.calls().pop().expect("at least one request")
    }

    pub fn client(&self) -> ForgeBase {
        ForgeBase::builder(&self.base)
            .retry(RetryPolicy::none())
            .api_key("fb_test")
            .build()
            .expect("client")
    }
}

/// A client against a server that answers `reply` to everything.
pub async fn serve(reply: Reply) -> (Mock, ForgeBase) {
    let m = Mock::start(vec![reply]).await;
    let fb = m.client();
    (m, fb)
}
