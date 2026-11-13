//! Native TLS through the real server lifecycle (ADR-0033): HTTPS with a
//! certificate from files, hot reload without a restart (contents check and
//! SIGHUP), a key that does not match refusing to start, RESP over TLS, the
//! plain HTTP redirect and `http-01` listener, the `tls-alpn-01` resolver,
//! HSTS, the status entry and `doctor`.
//!
//! SQLite always; the HTTPS test also runs on PostgreSQL with
//! `FORGEBASE_TEST_BACKEND=postgres` and an engine in `FORGEBASE_PG_ENGINE_DIR`
//! (or a `bundled-postgres` build). Binds `127.0.0.1:0`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use forgebase_auth::{Argon2Config, AuthConfig, AuthService, Authenticated, ClientInfo, CredentialKind, Subject};
use forgebase_core::config::tls_config::TlsMode;
use forgebase_core::config::{DatabaseBackend, PostgresMode};
use forgebase_core::{Config, ErrorCode};
use forgebase_data::DefaultAuthorizer;
use forgebase_server::tls::{self, DoctorLevel, x509};
use forgebase_server::{RunningServer, ServerError, start};
use rustls::pki_types::{CertificateDer, ServerName};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// A self-signed certificate for localhost and 127.0.0.1 valid for `days`,
/// written as PEM: `(cert path, key path, cert DER)`.
fn write_pair(dir: &Path, name: &str, days: i64) -> (PathBuf, PathBuf, Vec<u8>) {
    let (cert, key) = x509::self_signed(&["localhost", "127.0.0.1"], now() - 60, now() + days * 86_400, &[]).unwrap();
    let (c, k) = (dir.join(format!("{name}.crt")), dir.join(format!("{name}.key")));
    std::fs::write(&c, x509::pem("CERTIFICATE", &cert)).unwrap();
    std::fs::write(&k, x509::pem("PRIVATE KEY", &key)).unwrap();
    (c, k, cert)
}

fn postgres_wanted() -> bool {
    std::env::var("FORGEBASE_TEST_BACKEND").as_deref() == Ok("postgres")
        && (forgebase_pgengine::HAS_EMBEDDED
            || std::env::var_os(forgebase_pgengine::ENGINE_DIR_ENV).is_some_and(|v| !v.is_empty()))
}

fn config(dir: &Path, cert: &Path, key: &Path) -> Config {
    let mut cfg = Config::defaults(dir);
    cfg.server.host = "127.0.0.1".into();
    cfg.server.port = 0;
    cfg.server.shutdown_timeout_secs = 5;
    cfg.database.backend = DatabaseBackend::Sqlite;
    cfg.tls.mode = TlsMode::Files;
    cfg.tls.cert_file = cert.display().to_string();
    cfg.tls.key_file = key.display().to_string();
    cfg.tls.reload_interval_secs = 1;
    cfg
}

fn postgres(mut cfg: Config) -> Config {
    cfg.database.backend = DatabaseBackend::Postgres;
    cfg.database.postgres.mode = PostgresMode::Managed;
    for (k, v) in [
        ("fsync", "off"),
        ("synchronous_commit", "off"),
        ("full_page_writes", "off"),
    ] {
        cfg.database.postgres.settings.insert(k.into(), v.into());
    }
    cfg
}

fn tempdir() -> tempfile::TempDir {
    // Short paths: the engine's Unix socket path is limited to 103 bytes.
    let base = std::env::var_os("FORGEBASE_TEST_PG_TMP").unwrap_or_else(|| std::env::temp_dir().into_os_string());
    tempfile::Builder::new().prefix("fbtls-").tempdir_in(base).unwrap()
}

fn client(trust: &[&[u8]], alpn: &[&[u8]]) -> Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    for c in trust {
        roots.add(CertificateDer::from(c.to_vec())).unwrap();
    }
    let mut cfg = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    cfg.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
    Arc::new(cfg)
}

async fn connect(addr: SocketAddr, cfg: Arc<rustls::ClientConfig>, sni: &str) -> std::io::Result<TlsStream<TcpStream>> {
    let tcp = TcpStream::connect(addr).await?;
    tokio_rustls::TlsConnector::from(cfg)
        .connect(ServerName::try_from(sni.to_owned()).unwrap(), tcp)
        .await
}

fn serial_of<S>(s: &TlsStream<S>) -> String {
    let certs = s.get_ref().1.peer_certificates().unwrap();
    x509::parse_certificate(certs[0].as_ref()).unwrap().serial
}

/// One HTTP/1.1 exchange on a kept-alive stream: `(status, head, body)`.
async fn request<S: AsyncRead + AsyncWrite + Unpin>(s: &mut S, path: &str, host: &str) -> (u16, String, String) {
    let req = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: keep-alive\r\n\r\n");
    s.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let head_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        let n = s.read(&mut chunk).await.unwrap();
        assert!(n > 0, "connection closed before the response head");
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    let lower = head.to_ascii_lowercase();
    let mut body = buf[head_end..].to_vec();
    if let Some(len) = lower
        .lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .map(|v| v.trim().parse::<usize>().unwrap())
    {
        while body.len() < len {
            let n = s.read(&mut chunk).await.unwrap();
            assert!(n > 0);
            body.extend_from_slice(&chunk[..n]);
        }
    } else if lower.contains("transfer-encoding: chunked") {
        while !body.ends_with(b"0\r\n\r\n") {
            let n = s.read(&mut chunk).await.unwrap();
            assert!(n > 0);
            body.extend_from_slice(&chunk[..n]);
        }
        // Drop the chunk framing: the JSON sits between the first { and the last }.
        let text = String::from_utf8_lossy(&body).to_string();
        let json = text
            .find(['{', '['])
            .and_then(|i| text.rfind(['}', ']']).map(|j| text[i..=j].to_owned()))
            .unwrap_or_default();
        return (status, head, json);
    }
    (status, head, String::from_utf8_lossy(&body).to_string())
}

async fn https_server(cfg: &Config) -> RunningServer {
    start(cfg).await.expect("server starts with TLS")
}

async fn files_mode_serves_https(backend_pg: bool) {
    let tmp = if backend_pg {
        tempdir()
    } else {
        tempfile::tempdir().unwrap()
    };
    let (cert, key, der) = write_pair(tmp.path(), "a", 90);
    let mut cfg = config(tmp.path(), &cert, &key);
    cfg.tls.hsts_max_age_secs = 3600;
    if backend_pg {
        cfg = postgres(cfg);
    }
    let server = https_server(&cfg).await;
    let addr = server.local_addr();
    assert!(server.url().starts_with("https://"), "{}", server.url());

    // A client that trusts the certificate gets HTTPS, with HSTS.
    let mut s = connect(addr, client(&[&der], &[b"http/1.1"]), "localhost")
        .await
        .unwrap();
    assert_eq!(s.get_ref().1.alpn_protocol(), Some(&b"http/1.1"[..]));
    let (status, head, body) = request(&mut s, "/health", "localhost").await;
    assert_eq!(status, 200, "{head}{body}");
    assert!(body.contains("\"ok\""), "{body}");
    assert!(
        head.to_ascii_lowercase()
            .contains("strict-transport-security: max-age=3600"),
        "{head}"
    );

    // ALPN offers h2 first; an IP-address SAN verifies too.
    let s2 = connect(addr, client(&[&der], &[b"h2", b"http/1.1"]), "127.0.0.1")
        .await
        .unwrap();
    assert_eq!(s2.get_ref().1.alpn_protocol(), Some(&b"h2"[..]));
    drop(s2);

    // A client that does not trust it refuses; plain HTTP gets no answer.
    let (_, _, other) = write_pair(tmp.path(), "other", 90);
    assert!(connect(addr, client(&[&other], &[]), "localhost").await.is_err());
    let mut plain = TcpStream::connect(addr).await.unwrap();
    plain
        .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    let mut out = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), plain.read_to_end(&mut out)).await;
    assert!(!String::from_utf8_lossy(&out).contains("200 OK"));

    // The status entry (loopback, no forwarding header: admitted without credentials).
    let (status, _, body) = request(&mut s, "/api/system/info", "localhost").await;
    assert_eq!(status, 200, "{body}");
    let info: serde_json::Value = serde_json::from_str(&body).unwrap();
    let t = &info["tls"];
    assert_eq!(t["mode"], "files", "{info}");
    assert_eq!(t["min_version"], "1.2");
    assert_eq!(t["alpn"], serde_json::json!(["h2", "http/1.1"]));
    assert_eq!(t["certificate"]["names"], serde_json::json!(["localhost", "127.0.0.1"]));
    assert!(t["certificate"]["expires_in_secs"].as_i64().unwrap() > 89 * 86_400);
    assert_eq!(t["cert_file"], cert.display().to_string());
    assert!(t["last_error"].is_null());

    // The expiry gauge is exported.
    let (status, _, metrics) = request(&mut s, "/metrics", "localhost").await;
    assert_eq!(status, 200);
    let line = metrics
        .lines()
        .find(|l| l.starts_with("forgebase_tls_certificate_expiry_seconds "))
        .unwrap_or_else(|| panic!("expiry gauge in:\n{metrics}"));
    let secs: f64 = line.split_whitespace().nth(1).unwrap().parse().unwrap();
    assert!(secs > 89.0 * 86_400.0, "{line}");

    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn files_mode_serves_https_with_a_trusted_self_signed_certificate() {
    files_mode_serves_https(false).await;
}

#[tokio::test]
async fn files_mode_serves_https_on_postgres() {
    if !postgres_wanted() {
        eprintln!("skipped: set FORGEBASE_TEST_BACKEND=postgres and FORGEBASE_PG_ENGINE_DIR");
        return;
    }
    files_mode_serves_https(true).await;
}

/// Waits until a new handshake presents a certificate with `serial`.
async fn wait_for_serial(addr: SocketAddr, trust: &[&[u8]], serial: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(s) = connect(addr, client(trust, &[]), "localhost").await
            && serial_of(&s) == serial
        {
            return;
        }
        assert!(Instant::now() < deadline, "certificate {serial} was not served in time");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test]
async fn hot_reload_swaps_the_certificate_without_a_restart_or_dropping_connections() {
    let tmp = tempfile::tempdir().unwrap();
    let (cert, key, first) = write_pair(tmp.path(), "live", 90);
    let server = https_server(&config(tmp.path(), &cert, &key)).await;
    let addr = server.local_addr();
    let first_serial = x509::parse_certificate(&first).unwrap().serial;

    let mut kept = connect(addr, client(&[&first], &[b"http/1.1"]), "localhost")
        .await
        .unwrap();
    assert_eq!(serial_of(&kept), first_serial);
    assert_eq!(request(&mut kept, "/health", "localhost").await.0, 200);

    // Replace both files: the next handshake gets the new certificate.
    let (c2, k2, second) = write_pair(tmp.path(), "next", 90);
    std::fs::copy(&c2, &cert).unwrap();
    std::fs::copy(&k2, &key).unwrap();
    let second_serial = x509::parse_certificate(&second).unwrap().serial;
    wait_for_serial(addr, &[&first, &second], &second_serial).await;

    // The connection opened before the swap keeps working.
    assert_eq!(request(&mut kept, "/health", "localhost").await.0, 200);

    // A broken pair (a key from another certificate) is refused: the second
    // certificate stays, and the status says why.
    let (_, k3, _) = write_pair(tmp.path(), "stray", 90);
    std::fs::copy(&k3, &key).unwrap();
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let s = connect(addr, client(&[&second], &[]), "localhost").await.unwrap();
    assert_eq!(serial_of(&s), second_serial, "the old certificate is still served");
    let status = server.tls().unwrap().status_json();
    let err = status["last_error"].as_str().unwrap_or_default();
    assert!(err.contains("does not belong"), "{status}");

    // Restore the matching key: served again, error cleared.
    std::fs::copy(&k2, &key).unwrap();
    std::fs::copy(&c2, &cert).unwrap();
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(
        serial_of(&connect(addr, client(&[&second], &[]), "localhost").await.unwrap()),
        second_serial
    );
    server.shutdown().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn sighup_reloads_when_polling_is_off() {
    let tmp = tempfile::tempdir().unwrap();
    let (cert, key, first) = write_pair(tmp.path(), "hup", 90);
    let mut cfg = config(tmp.path(), &cert, &key);
    cfg.tls.reload_interval_secs = 0;
    let server = https_server(&cfg).await;
    let addr = server.local_addr();

    let (c2, k2, second) = write_pair(tmp.path(), "hup2", 90);
    std::fs::copy(&c2, &cert).unwrap();
    std::fs::copy(&k2, &key).unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let first_serial = x509::parse_certificate(&first).unwrap().serial;
    let s = connect(addr, client(&[&first, &second], &[]), "localhost")
        .await
        .unwrap();
    assert_eq!(serial_of(&s), first_serial, "no polling: unchanged until SIGHUP");

    let ok = std::process::Command::new("kill")
        .args(["-HUP", &std::process::id().to_string()])
        .status()
        .unwrap();
    assert!(ok.success());
    let second_serial = x509::parse_certificate(&second).unwrap().serial;
    wait_for_serial(addr, &[&first, &second], &second_serial).await;
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_key_that_does_not_match_its_certificate_refuses_to_start() {
    let tmp = tempfile::tempdir().unwrap();
    let (cert, _, _) = write_pair(tmp.path(), "a", 90);
    let (_, other_key, _) = write_pair(tmp.path(), "b", 90);
    let cfg = config(tmp.path(), &cert, &other_key);
    let err = start(&cfg).await.unwrap_err();
    assert!(matches!(err, ServerError::Tls(_)), "{err}");
    assert_eq!(err.code(), "tls_key_mismatch");
    let msg = err.to_string();
    assert!(
        msg.contains("does not belong") && msg.contains("b.key") && msg.contains("a.crt"),
        "{msg}"
    );
    // Nothing was opened: no database file, and the lock is free again.
    assert!(!forgebase_core::DataLayout::new(&cfg.data_dir).database_path().exists());

    let missing = config(tmp.path(), &tmp.path().join("nope.crt"), &other_key);
    let err = start(&missing).await.unwrap_err();
    assert_eq!(err.code(), "tls_file_unreadable", "{err}");
    assert!(err.to_string().contains("nope.crt"));

    // The right pair starts in the same data directory.
    let (c, k, _) = write_pair(tmp.path(), "good", 90);
    let server = start(&config(tmp.path(), &c, &k)).await.unwrap();
    server.shutdown().await.unwrap();
}

/// Creates an administrator and an API key on the server's database.
async fn api_key(server: &RunningServer, dir: &Path) -> String {
    let mut acfg = AuthConfig::for_data_dir(dir);
    acfg.argon2 = Argon2Config::insecure_fast_for_tests();
    let auth = AuthService::open_with_data(
        Arc::new(server.database().clone()),
        Arc::new(DefaultAuthorizer),
        dir,
        acfg,
        None,
    )
    .await
    .unwrap();
    let ci = ClientInfo::default();
    let admin = auth
        .create_admin("tls@example.com", "a-long-admin-password-1", "test", &ci)
        .await
        .unwrap()
        .id;
    let caller = Authenticated {
        subject: Subject::Admin { id: admin },
        credential: CredentialKind::AccessToken,
        credential_id: "t".into(),
        scopes: None,
        mfa: false,
        impersonator: None,
    };
    let sa = auth
        .create_service_account(&caller, &ci, "tls", "", "admin")
        .await
        .unwrap();
    auth.create_api_key(&caller, &ci, &sa.id, "resp", &["*".to_owned()], None)
        .await
        .unwrap()
        .key
}

fn resp_cmd(args: &[&str]) -> Vec<u8> {
    let mut out = format!("*{}\r\n", args.len()).into_bytes();
    for a in args {
        out.extend_from_slice(format!("${}\r\n{a}\r\n", a.len()).as_bytes());
    }
    out
}

async fn resp_reply<S: AsyncRead + Unpin>(s: &mut S) -> String {
    let mut buf = vec![0u8; 512];
    let n = tokio::time::timeout(Duration::from_secs(5), s.read(&mut buf))
        .await
        .unwrap()
        .unwrap();
    String::from_utf8_lossy(&buf[..n]).to_string()
}

#[tokio::test]
async fn resp_over_tls_uses_the_same_certificate() {
    let tmp = tempfile::tempdir().unwrap();
    let (cert, key, der) = write_pair(tmp.path(), "resp", 90);
    let mut cfg = config(tmp.path(), &cert, &key);
    cfg.resp.enabled = true;
    cfg.resp.port = 0;
    cfg.resp.tls = true;
    let server = https_server(&cfg).await;
    let resp = server.resp_addr().unwrap();
    let key_text = api_key(&server, &cfg.data_dir).await;

    let mut s = connect(resp, client(&[&der], &[]), "localhost").await.unwrap();
    s.write_all(&resp_cmd(&["AUTH", &key_text])).await.unwrap();
    assert_eq!(resp_reply(&mut s).await, "+OK\r\n");
    s.write_all(&resp_cmd(&["SET", "greeting", "hello over tls"]))
        .await
        .unwrap();
    assert_eq!(resp_reply(&mut s).await, "+OK\r\n");
    s.write_all(&resp_cmd(&["GET", "greeting"])).await.unwrap();
    assert_eq!(resp_reply(&mut s).await, "$14\r\nhello over tls\r\n");

    // Plain RESP on the TLS port gets no answer.
    let mut plain = TcpStream::connect(resp).await.unwrap();
    plain.write_all(b"PING\r\n").await.unwrap();
    let mut out = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(3), plain.read_to_end(&mut out)).await;
    assert!(!String::from_utf8_lossy(&out).contains("PONG"), "{out:?}");

    let status = server.tls().unwrap().status_json();
    assert_eq!(status["resp"], true, "{status}");
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn resp_tls_without_tls_mode_is_a_config_error() {
    let mut cfg = Config::defaults(Path::new("/tmp/x"));
    cfg.resp.tls = true;
    let all = cfg
        .problems()
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(all.contains("resp.tls"), "{all}");
}

#[tokio::test]
async fn the_plain_listener_redirects_to_https_and_answers_http01() {
    let tmp = tempfile::tempdir().unwrap();
    let (cert, key, _) = write_pair(tmp.path(), "r", 90);
    let mut cfg = config(tmp.path(), &cert, &key);
    cfg.tls.redirect_http = true;
    cfg.tls.http_port = 0;
    let server = https_server(&cfg).await;
    let https_port = server.local_addr().port();
    let plain = server.http_redirect_addr().expect("plain listener runs");

    let mut s = TcpStream::connect(plain).await.unwrap();
    let (status, head, _) = request(&mut s, "/api/v1/collections?limit=5", "localhost:8080").await;
    assert_eq!(status, 308, "{head}");
    assert!(
        head.contains(&format!(
            "location: https://localhost:{https_port}/api/v1/collections?limit=5"
        )) || head.contains(&format!(
            "Location: https://localhost:{https_port}/api/v1/collections?limit=5"
        )),
        "{head}"
    );

    // A malformed host is not echoed into a redirect.
    let mut s = TcpStream::connect(plain).await.unwrap();
    let (status, _, _) = request(&mut s, "/", "evil.example\\@x").await;
    assert_eq!(status, 400);

    // http-01: the answer the store holds, 404 otherwise.
    server
        .tls()
        .unwrap()
        .store()
        .put_http_challenge("tok-123", "tok-123.thumbprint");
    let mut s = TcpStream::connect(plain).await.unwrap();
    let (status, _, body) = request(&mut s, "/.well-known/acme-challenge/tok-123", "localhost").await;
    assert_eq!((status, body.as_str()), (200, "tok-123.thumbprint"));
    let (status, _, _) = request(&mut s, "/.well-known/acme-challenge/other", "localhost").await;
    assert_eq!(status, 404);

    let status = server.tls().unwrap().status_json();
    assert_eq!(status["http_listener"], plain.to_string());
    server.shutdown().await.unwrap();
    // Both listeners are closed after shutdown.
    assert!(TcpStream::connect(plain).await.is_err());
}

/// The resolver answers `acme-tls/1` handshakes with the challenge
/// certificate for the asked name (and closes them), everything else with
/// the served certificate.
#[tokio::test]
async fn tls_alpn_01_handshakes_get_the_challenge_certificate() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(tls::CertStore::new());
    let (c, k, _) = write_pair(tmp.path(), "main2", 90);
    store.install(tls::certs::from_files(&c, &k).unwrap());
    let digest = [42u8; 32];
    store.put_alpn_challenge("app.example.com", &digest, now()).unwrap();
    let cfg = tls::certs::server_config(
        store.clone(),
        forgebase_core::config::tls_config::TlsVersion::V1_2,
        &[b"h2", b"http/1.1", tls::certs::ACME_TLS_ALPN],
    )
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = axum::Router::new().route("/", axum::routing::get(|| async { "hi" }));
    let (stop, rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(tls::serve::serve_tls(
        listener,
        tokio_rustls::TlsAcceptor::from(cfg),
        app,
        rx,
    ));

    // A validator: ALPN acme-tls/1 only, trusting anything (it checks the
    // extension itself, RFC 8737 §3).
    #[derive(Debug)]
    struct AnyCert;
    impl rustls::client::danger::ServerCertVerifier for AnyCert {
        fn verify_server_cert(
            &self,
            _: &CertificateDer<'_>,
            _: &[CertificateDer<'_>],
            _: &ServerName<'_>,
            _: &[u8],
            _: rustls::pki_types::UnixTime,
        ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        }
        fn verify_tls12_signature(
            &self,
            _: &[u8],
            _: &CertificateDer<'_>,
            _: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
        }
        fn verify_tls13_signature(
            &self,
            _: &[u8],
            _: &CertificateDer<'_>,
            _: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
        }
        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            rustls::crypto::ring::default_provider()
                .signature_verification_algorithms
                .supported_schemes()
        }
    }
    let mut vcfg = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AnyCert))
        .with_no_client_auth();
    vcfg.alpn_protocols = vec![tls::certs::ACME_TLS_ALPN.to_vec()];
    let vcfg = Arc::new(vcfg);

    let mut v = connect(addr, vcfg.clone(), "app.example.com").await.unwrap();
    assert_eq!(v.get_ref().1.alpn_protocol(), Some(tls::certs::ACME_TLS_ALPN));
    let presented = v.get_ref().1.peer_certificates().unwrap()[0].as_ref().to_vec();
    assert_eq!(x509::parse_certificate(&presented).unwrap().names, ["app.example.com"]);
    let mut needle = vec![0x04, 0x20];
    needle.extend_from_slice(&digest);
    assert!(
        presented.windows(needle.len()).any(|w| w == needle),
        "the key authorization digest"
    );
    // The server closes the validation connection after the handshake.
    let mut buf = [0u8; 16];
    let r = tokio::time::timeout(Duration::from_secs(5), v.read(&mut buf))
        .await
        .unwrap();
    assert!(matches!(r, Ok(0) | Err(_)), "{r:?}");

    // No challenge for this name: the handshake fails.
    assert!(connect(addr, vcfg, "other.example.com").await.is_err());

    // A normal client gets the served certificate and HTTP.
    let trusted = tls::certs::from_files(&c, &k).unwrap();
    let leaf = std::fs::read(&c).unwrap();
    let leaf = CertificateDer::pem_slice_iter(&leaf).next().unwrap().unwrap();
    let mut s = connect(addr, client(&[leaf.as_ref()], &[b"http/1.1"]), "localhost")
        .await
        .unwrap();
    assert_eq!(serial_of(&s), trusted.info.serial);
    assert_eq!(request(&mut s, "/", "localhost").await.0, 200);
    drop(s);

    let _ = stop.send(());
    task.await.unwrap().unwrap();
}

use rustls::pki_types::pem::PemObject as _;

#[test]
fn doctor_reports_expiry() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = Config::defaults(tmp.path());
    assert_eq!(tls::doctor_check(&cfg, now()), None, "off: no line");

    let (c, k, _) = write_pair(tmp.path(), "long", 90);
    cfg.tls.mode = TlsMode::Files;
    cfg.tls.cert_file = c.display().to_string();
    cfg.tls.key_file = k.display().to_string();
    let (level, detail) = tls::doctor_check(&cfg, now()).unwrap();
    assert_eq!(level, DoctorLevel::Ok, "{detail}");

    let (level, detail) = tls::doctor_check(&cfg, now() + 80 * 86_400 + 3600).unwrap();
    assert_eq!(level, DoctorLevel::Warn, "{detail}");
    assert!(detail.contains("expires in 9 day"), "{detail}");

    let (level, detail) = tls::doctor_check(&cfg, now() + 91 * 86_400).unwrap();
    assert_eq!(level, DoctorLevel::Fail, "{detail}");
    assert!(detail.contains("expired"), "{detail}");

    let (_, stray, _) = write_pair(tmp.path(), "stray", 90);
    cfg.tls.key_file = stray.display().to_string();
    let (level, detail) = tls::doctor_check(&cfg, now()).unwrap();
    assert_eq!(level, DoctorLevel::Fail, "{detail}");

    cfg.tls.mode = TlsMode::Acme;
    cfg.tls.acme.domains = vec!["app.example.com".into()];
    let (level, detail) = tls::doctor_check(&cfg, now()).unwrap();
    assert_eq!(level, DoctorLevel::Warn, "{detail}");
    assert!(detail.contains("no certificate issued yet"), "{detail}");
}
