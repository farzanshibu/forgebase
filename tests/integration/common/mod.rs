//! Helpers shared by the integration tests.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::time::{Duration, Instant};

/// A free TCP port on loopback (bound then released).
pub fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// Blocking HTTP/1.1 request; returns (status, headers, body).
pub fn http(method: &str, addr: &str, path: &str) -> std::io::Result<(u16, String, String)> {
    let mut s = TcpStream::connect(addr)?;
    s.set_read_timeout(Some(Duration::from_secs(10)))?;
    s.set_write_timeout(Some(Duration::from_secs(10)))?;
    write!(
        s,
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"
    )?;
    let mut buf = Vec::new();
    s.read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    Ok((status, head.to_owned(), body.to_owned()))
}

/// GET returning status and parsed JSON body (Null if not JSON).
pub fn get_json(addr: &str, path: &str) -> (u16, serde_json::Value) {
    let (status, _, body) = http("GET", addr, path).unwrap();
    (status, serde_json::from_str(&body).unwrap_or(serde_json::Value::Null))
}

/// Polls `/health` until it answers 200 or the timeout passes.
pub fn wait_healthy(addr: &str, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if let Ok((200, _, _)) = http("GET", addr, "/health") {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// Size of a file, 0 if missing.
pub fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}
