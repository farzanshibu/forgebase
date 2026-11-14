//! A scripted HTTP server for CLI unit tests.

use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// A request as the fake server saw it.
#[derive(Debug, Clone)]
pub(crate) struct Seen {
    pub(crate) request_line: String,
    pub(crate) headers: String,
    pub(crate) body: String,
}

/// A scripted answer: status, body and extra response headers.
pub(crate) type Answer = (u16, String, Vec<(String, String)>);

/// Serves `replies` in order (the last repeats) and records each request.
pub(crate) async fn fake_server(replies: Vec<(u16, String)>) -> (String, Arc<Mutex<Vec<Seen>>>) {
    let count = Arc::new(Mutex::new(0_usize));
    fake_server_with(move |_seen| {
        let mut n = count.lock().unwrap();
        let (status, body) = replies.get(*n).or(replies.last()).cloned().unwrap();
        *n += 1;
        (status, body, Vec::new())
    })
    .await
}

/// Serves whatever `answer` returns for each request (it sees the request first) and records each request.
pub(crate) async fn fake_server_with(
    answer: impl Fn(&Seen) -> Answer + Send + Sync + 'static,
) -> (String, Arc<Mutex<Vec<Seen>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = listener.accept().await else {
                return;
            };
            let mut buf = Vec::new();
            let mut tmp = [0_u8; 4096];
            let mut this = None;
            loop {
                let k = s.read(&mut tmp).await.unwrap();
                buf.extend_from_slice(&tmp[..k]);
                let text = String::from_utf8_lossy(&buf).into_owned();
                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                    let want = head
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if body.len() >= want {
                        let mut lines = head.lines();
                        let seen = Seen {
                            request_line: lines.next().unwrap().to_owned(),
                            headers: lines.collect::<Vec<_>>().join("\n"),
                            body: body.to_owned(),
                        };
                        log.lock().unwrap().push(seen.clone());
                        this = Some(seen);
                        break;
                    }
                }
                if k == 0 {
                    break;
                }
            }
            let Some(this) = this else { continue };
            let (status, body, extra) = answer(&this);
            let extra: String = extra.iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect();
            let head = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = s.write_all(head.as_bytes()).await;
            let _ = s.write_all(body.as_bytes()).await;
        }
    });
    (addr, seen)
}
