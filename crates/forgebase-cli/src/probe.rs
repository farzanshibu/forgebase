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
    fn wildcard_hosts_dial_loopback() {
        assert_eq!(dial_addr("0.0.0.0", 1), "127.0.0.1:1");
        assert_eq!(dial_addr("::", 2), "[::1]:2");
        assert_eq!(dial_addr("localhost", 3), "localhost:3");
        assert_eq!(dial_addr("fe80::1", 4), "[fe80::1]:4");
    }
}
