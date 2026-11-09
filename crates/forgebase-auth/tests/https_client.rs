//! Outbound HTTPS client (FB-25, ADR-0015 §2): rustls + ring, verified
//! certificates, installed by default in the auth service.
#![cfg(feature = "https")]
#![allow(clippy::unwrap_used, missing_docs)]

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use forgebase_auth::net::{HttpsClient, TlsSettings};
use forgebase_auth::social::{HttpClient, HttpRequest};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

const CA: &[u8] = include_bytes!("fixtures/tls-ca.pem");
const SERVER_CERT: &[u8] = include_bytes!("fixtures/tls-server.pem");
const SERVER_KEY: &[u8] = include_bytes!("fixtures/tls-server.key");
const WRONG_CERT: &[u8] = include_bytes!("fixtures/tls-wrongname.pem");
const WRONG_KEY: &[u8] = include_bytes!("fixtures/tls-wrongname.key");

/// An HTTPS server answering every request with `body`; when `linger` the
/// connection stays open after the response (a server ignoring
/// `Connection: close`). Sends each request head it receives.
async fn tls_server(
    cert: &[u8],
    key: &[u8],
    body: &'static str,
    linger: bool,
) -> (u16, mpsc::UnboundedReceiver<String>) {
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(cert).map(|c| c.unwrap()).collect();
    let key = PrivateKeyDer::from_pem_slice(key).unwrap();
    let cfg = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(cfg));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else { return };
            let acceptor = acceptor.clone();
            let tx = tx.clone();
            tokio::spawn(async move {
                let Ok(mut s) = acceptor.accept(tcp).await else { return };
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    match s.read(&mut chunk).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                }
                let _ = tx.send(String::from_utf8_lossy(&buf).into_owned());
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                let _ = s.write_all(resp.as_bytes()).await;
                let _ = s.flush().await;
                if linger {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                } else {
                    let _ = s.shutdown().await;
                }
            });
        }
    });
    (port, rx)
}

fn trusting_test_ca() -> HttpsClient {
    HttpsClient::new(&TlsSettings::new().with_pem(CA).unwrap()).unwrap()
}

#[tokio::test]
async fn https_request_with_a_trusted_certificate_succeeds() {
    let (port, mut seen) = tls_server(SERVER_CERT, SERVER_KEY, r#"{"ok":true}"#, false).await;
    let client = trusting_test_ca();
    for host in ["127.0.0.1", "localhost"] {
        let r = client
            .send(HttpRequest::post_form(
                format!("https://{host}:{port}/token?x=1"),
                &[("code", "abc")],
            ))
            .await
            .unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(r.json().unwrap()["ok"], true);
        let head = seen.recv().await.unwrap();
        assert!(head.starts_with("POST /token?x=1 HTTP/1.1\r\n"), "{head}");
        assert!(head.contains(&format!("Host: {host}:{port}\r\n")), "{head}");
    }
}

#[tokio::test]
async fn unknown_issuer_and_wrong_name_are_refused() {
    let (port, _seen) = tls_server(SERVER_CERT, SERVER_KEY, "{}", false).await;
    // webpki-roots only: the test CA is not trusted.
    let plain_roots = HttpsClient::new(&TlsSettings::new()).unwrap();
    let e = plain_roots
        .send(HttpRequest::get(format!("https://127.0.0.1:{port}/")))
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("TLS handshake") && e.contains("UnknownIssuer"), "{e}");

    let (port, _seen) = tls_server(WRONG_CERT, WRONG_KEY, "{}", false).await;
    let e = trusting_test_ca()
        .send(HttpRequest::get(format!("https://127.0.0.1:{port}/")))
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("TLS handshake") && e.contains("not valid for name"), "{e}");
}

#[tokio::test]
async fn a_server_that_keeps_the_connection_open_does_not_stall_the_client() {
    let (port, _seen) = tls_server(SERVER_CERT, SERVER_KEY, r#"{"a":1}"#, true).await;
    let start = Instant::now();
    let r = trusting_test_ca()
        .with_timeout(Duration::from_secs(10))
        .send(HttpRequest::get(format!("https://127.0.0.1:{port}/")))
        .await
        .unwrap();
    assert_eq!(r.body, br#"{"a":1}"#);
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "framed by Content-Length, not by close"
    );
}

#[tokio::test]
async fn plain_http_still_works_for_loopback_mocks() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 2048];
        let _ = s.read(&mut buf).await.unwrap();
        s.write_all(b"HTTP/1.1 201 Created\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{}\r\n0\r\n\r\n")
            .await
            .unwrap();
    });
    let r = trusting_test_ca()
        .send(HttpRequest::get(format!("http://127.0.0.1:{port}/")))
        .await
        .unwrap();
    assert_eq!((r.status, r.body.as_slice()), (201, b"{}".as_slice()));
}

#[tokio::test]
async fn timeouts_are_errors() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (_s, _) = listener.accept().await.unwrap();
        tokio::time::sleep(Duration::from_secs(30)).await;
    });
    let e = trusting_test_ca()
        .with_timeout(Duration::from_millis(300))
        .send(HttpRequest::get(format!("https://127.0.0.1:{port}/")))
        .await
        .unwrap_err();
    assert!(e.to_string().contains("timed out"), "{e}");
}

#[tokio::test]
async fn the_auth_service_installs_the_https_client_by_default() {
    let (port, _seen) = tls_server(SERVER_CERT, SERVER_KEY, r#"{"issuer":"x"}"#, false).await;
    let ca_dir = tempfile::tempdir().unwrap();
    let ca = ca_dir.path().join("ca.pem");
    std::fs::write(&ca, CA).unwrap();
    let h = common::harness(|cfg| {
        cfg.tls_ca_file = Some(ca);
        cfg.allow_private_outbound = true;
    })
    .await;
    let auth = &h.auth;
    assert!(format!("{:?}", auth.http_client()).contains("HttpsClient"));
    let r = auth
        .http_client()
        .send(HttpRequest::get(format!(
            "https://localhost:{port}/.well-known/openid-configuration"
        )))
        .await
        .unwrap();
    assert_eq!(r.json().unwrap()["issuer"], "x");
}

/// FB-80: by default the auth service's client refuses loopback, private,
/// link-local and metadata addresses before connecting (SSRF through an
/// OAuth/OIDC discovery document, SMS gateway or Turnstile URL).
#[tokio::test]
async fn the_default_auth_client_refuses_private_addresses() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let accepted = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = accepted.clone();
    tokio::spawn(async move {
        while let Ok((_s, _)) = listener.accept().await {
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    });
    let h = common::harness(|_| {}).await;
    let client = h.auth.http_client();
    assert!(format!("{client:?}").contains("public_only: true"));
    for url in [
        format!("http://127.0.0.1:{port}/token"),
        format!("http://localhost:{port}/.well-known/openid-configuration"),
        format!("https://[::1]:{port}/jwks"),
        format!("http://[::ffff:127.0.0.1]:{port}/"),
        "http://169.254.169.254/latest/meta-data/".to_owned(),
        "http://10.0.0.1/".to_owned(),
        "http://192.168.1.1/".to_owned(),
        "http://0.0.0.0/".to_owned(),
    ] {
        let err = client.send(HttpRequest::get(url.clone())).await.unwrap_err().to_string();
        assert!(err.contains("refused"), "{url}: {err}");
    }
    assert_eq!(accepted.load(std::sync::atomic::Ordering::SeqCst), 0, "never connected");
    // Storage drivers and tests build their own client without the guard.
    let open = HttpsClient::new(&TlsSettings::new()).unwrap();
    assert!(format!("{open:?}").contains("public_only: false"));
}

/// Real-world check against public HTTPS endpoints (network required):
/// `cargo test -p forgebase-auth --test https_client -- --ignored`.
#[tokio::test]
#[ignore = "needs internet access"]
async fn real_identity_provider_discovery_over_https() {
    let client = HttpsClient::new(&TlsSettings::new()).unwrap();
    for url in [
        "https://accounts.google.com/.well-known/openid-configuration",
        "https://challenges.cloudflare.com/turnstile/v0/siteverify",
    ] {
        let r = client.send(HttpRequest::get(url)).await.unwrap();
        assert!(r.status < 500, "{url}: {}", r.status);
    }
}
