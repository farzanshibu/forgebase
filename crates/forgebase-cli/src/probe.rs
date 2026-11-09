//! A minimal HTTP/1.1 GET used to check whether a ForgeBase server answers.
//! Plain TCP, no TLS, `Connection: close`: enough for `/health` on loopback
//! without pulling an HTTP client into the binary.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Address to dial for a bind host: wildcard binds are probed on loopback.
pub fn dial_addr(host: &str, port: u16) -> String {
    let host = match host {
        "0.0.0.0" | "" => "127.0.0.1",
        "::" | "[::]" => "::1",
        h => h,
    };
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// `GET path` on `addr`; returns the status code and body.
pub async fn get(addr: &str, path: &str, timeout: Duration) -> std::io::Result<(u16, String)> {
    tokio::time::timeout(timeout, async {
        let mut stream = TcpStream::connect(addr).await?;
        let req = format!(
            "GET {path} HTTP/1.1\r\nHost: {addr}\r\nUser-Agent: forgebase-cli\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(req.as_bytes()).await?;
        let mut buf = Vec::new();
        stream.take(1 << 20).read_to_end(&mut buf).await?;
        let text = String::from_utf8_lossy(&buf);
        let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
        let status = head
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse::<u16>().ok())
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "not an HTTP response"))?;
        Ok((status, body.to_owned()))
    })
    .await
    .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "timed out"))?
}

/// A buffered HTTP response.
#[derive(Debug)]
pub struct Reply {
    /// Status code.
    pub status: u16,
    /// Decoded body (chunked encoding removed).
    pub body: Vec<u8>,
}

/// Largest response the CLI reads (a runaway server must not exhaust memory).
const MAX_REPLY: u64 = 64 << 20;

/// One request with an optional JSON body, `Connection: close`. Plain HTTP only.
pub async fn request(
    addr: &str,
    method: &str,
    path: &str,
    bearer: Option<&str>,
    json_body: Option<&[u8]>,
    timeout: Duration,
) -> std::io::Result<Reply> {
    tokio::time::timeout(timeout, async {
        let mut stream = TcpStream::connect(addr).await?;
        let mut head = format!(
            "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nUser-Agent: forgebase-cli\r\nAccept: application/json\r\nConnection: close\r\n"
        );
        if let Some(t) = bearer {
            head.push_str(&format!("Authorization: Bearer {t}\r\n"));
        }
        if let Some(b) = json_body {
            head.push_str(&format!("Content-Type: application/json\r\nContent-Length: {}\r\n", b.len()));
        } else if method != "GET" {
            head.push_str("Content-Length: 0\r\n");
        }
        head.push_str("\r\n");
        stream.write_all(head.as_bytes()).await?;
        if let Some(b) = json_body {
            stream.write_all(b).await?;
        }
        let mut buf = Vec::new();
        stream.take(MAX_REPLY).read_to_end(&mut buf).await?;
        parse_reply(&buf)
    })
    .await
    .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "timed out"))?
}

fn bad(msg: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, msg.to_owned())
}

fn parse_reply(buf: &[u8]) -> std::io::Result<Reply> {
    let split = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| bad("not an HTTP response"))?;
    let head = String::from_utf8_lossy(&buf[..split]);
    let raw = &buf[split + 4..];
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| bad("not an HTTP response"))?;
    let chunked = lines.any(|l| {
        let l = l.to_ascii_lowercase();
        l.starts_with("transfer-encoding:") && l.contains("chunked")
    });
    let body = if chunked { dechunk(raw)? } else { raw.to_vec() };
    Ok(Reply { status, body })
}

fn dechunk(mut raw: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let eol = raw
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or_else(|| bad("truncated chunked body"))?;
        let size_text = String::from_utf8_lossy(&raw[..eol]);
        let size_text = size_text.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_text, 16).map_err(|_| bad("bad chunk size"))?;
        raw = &raw[eol + 2..];
        if size == 0 {
            return Ok(out);
        }
        if raw.len() < size + 2 {
            return Err(bad("truncated chunked body"));
        }
        out.extend_from_slice(&raw[..size]);
        raw = &raw[size + 2..];
    }
}

/// Whether a ForgeBase `/health` answers `{"status":"ok"}` on `addr`.
pub async fn is_forgebase(addr: &str) -> bool {
    match get(addr, "/health", Duration::from_secs(2)).await {
        Ok((200, body)) => serde_json::from_str::<serde_json::Value>(&body)
            .is_ok_and(|v| v.get("status").and_then(|s| s.as_str()) == Some("ok")),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_content_length_and_chunked_replies() {
        let r = parse_reply(b"HTTP/1.1 201 Created\r\nContent-Length: 2\r\n\r\n{}").unwrap();
        assert_eq!((r.status, r.body.as_slice()), (201, &b"{}"[..]));
        let r =
            parse_reply(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2;x=1\r\nde\r\n0\r\n\r\n")
                .unwrap();
        assert_eq!(r.body, b"abcde");
        assert!(parse_reply(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nab").is_err());
        assert!(parse_reply(b"garbage").is_err());
    }

    #[test]
    fn wildcard_hosts_dial_loopback() {
        assert_eq!(dial_addr("0.0.0.0", 1), "127.0.0.1:1");
        assert_eq!(dial_addr("::", 2), "[::1]:2");
        assert_eq!(dial_addr("localhost", 3), "localhost:3");
        assert_eq!(dial_addr("fe80::1", 4), "[fe80::1]:4");
    }
}
