//! The exporter's HTTP/1.1 client: one `POST` per connection
//! (`Connection: close`), `http://` or `https://` (rustls with the
//! webpki-roots CA bundle plus an optional extra PEM). Only the status line
//! of the answer is read.
//!
//! The collector address is the operator's configuration, so unlike the
//! SSRF-safe client of functions and webhooks it may be on loopback or a
//! private network (a collector next to the server is the usual setup).

use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

/// A rustls client config with the public roots and `extra_pem`'s.
pub(crate) fn tls_config(extra_pem: Option<&[u8]>) -> Result<Arc<rustls::ClientConfig>, String> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if let Some(pem) = extra_pem {
        let mut n = 0;
        for c in CertificateDer::pem_slice_iter(pem) {
            let c = c.map_err(|e| format!("observability.otlp.ca_file: {e}"))?;
            roots
                .add(c)
                .map_err(|e| format!("observability.otlp.ca_file: unusable certificate: {e}"))?;
            n += 1;
        }
        if n == 0 {
            return Err("observability.otlp.ca_file holds no PEM certificate".into());
        }
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut cfg = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("TLS setup: {e}"))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(cfg))
}

/// `POST`s `body` to `url`; returns the status code.
pub(crate) async fn post(
    url: &str,
    headers: &[(String, String)],
    content_type: &str,
    body: &[u8],
    timeout: Duration,
    tls: &Arc<rustls::ClientConfig>,
) -> Result<u16, String> {
    let u = url::Url::parse(url).map_err(|e| format!("{url}: {e}"))?;
    let host = u.host_str().ok_or_else(|| format!("{url} has no host"))?.to_owned();
    let port = u.port_or_known_default().ok_or_else(|| format!("{url} has no port"))?;
    let mut path = u.path().to_owned();
    if let Some(q) = u.query() {
        path.push('?');
        path.push_str(q);
    }
    let host_header = match u.port() {
        Some(p) => format!("{host}:{p}"),
        None => host.clone(),
    };
    let mut head = format!(
        "POST {path} HTTP/1.1\r\nHost: {host_header}\r\nUser-Agent: forgebase/{}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n",
        forgebase_core::build::VERSION,
        body.len()
    );
    for (k, v) in headers {
        if k.contains(['\r', '\n', ':']) || v.contains(['\r', '\n']) {
            return Err(format!("bad header {k:?}"));
        }
        head.push_str(k);
        head.push_str(": ");
        head.push_str(v);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    let work = async {
        let connect_host = host.trim_start_matches('[').trim_end_matches(']').to_owned();
        let tcp = TcpStream::connect((connect_host.as_str(), port))
            .await
            .map_err(|e| format!("connect {host_header}: {e}"))?;
        let _ = tcp.set_nodelay(true);
        match u.scheme() {
            "http" => exchange(tcp, &head, body).await,
            "https" => {
                let name = ServerName::try_from(connect_host.clone())
                    .map_err(|_| format!("{host} is not a valid TLS server name"))?;
                let s = tokio_rustls::TlsConnector::from(tls.clone())
                    .connect(name, tcp)
                    .await
                    .map_err(|e| format!("TLS handshake with {host}: {e}"))?;
                exchange(s, &head, body).await
            }
            other => Err(format!("the {other} scheme is not supported")),
        }
    };
    tokio::time::timeout(timeout, work)
        .await
        .map_err(|_| format!("{url}: timed out"))?
}

async fn exchange<S: AsyncRead + AsyncWrite + Unpin>(mut s: S, head: &str, body: &[u8]) -> Result<u16, String> {
    s.write_all(head.as_bytes()).await.map_err(|e| e.to_string())?;
    s.write_all(body).await.map_err(|e| e.to_string())?;
    s.flush().await.map_err(|e| e.to_string())?;
    let mut buf = Vec::with_capacity(256);
    let mut chunk = [0u8; 512];
    while !buf.windows(2).any(|w| w == b"\r\n") {
        let n = s.read(&mut chunk).await.map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("the collector closed the connection without an answer".into());
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > 8192 {
            return Err("the collector's status line is too long".into());
        }
    }
    let line = String::from_utf8_lossy(&buf);
    let status = line
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse::<u16>().ok())
        .ok_or_else(|| "the collector's answer is not HTTP".to_owned())?;
    // Drain a little so the peer does not see a reset before its write.
    let _ = tokio::time::timeout(Duration::from_millis(50), s.read(&mut chunk)).await;
    Ok(status)
}
