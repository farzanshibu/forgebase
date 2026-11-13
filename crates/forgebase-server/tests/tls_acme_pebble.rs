//! ACME end to end against Pebble, Let's Encrypt's test CA (ADR-0033):
//! a real RFC 8555 exchange (account, order, `tls-alpn-01` and `http-01`
//! validation by Pebble connecting back to the server, finalize, download),
//! HTTPS with the issued chain verified up to Pebble's root, and the sealed
//! certificate served again after a restart without asking the CA.
//!
//! Ignored by default; needs Pebble (docs/qa.md, "Native TLS and ACME"):
//!
//! ```text
//! go install github.com/letsencrypt/pebble/v2/cmd/pebble@v2.10.3
//! go install github.com/letsencrypt/pebble/v2/cmd/pebble-challtestsrv@v2.10.3
//! FORGEBASE_TEST_PEBBLE_BIN=$(go env GOPATH)/bin \
//! FORGEBASE_TEST_PEBBLE_CERTS=$(go env GOMODCACHE)/github.com/letsencrypt/pebble/v2@v2.10.3/test/certs \
//!   cargo test -p forgebase-server --test tls_acme_pebble -- --ignored
//! ```
//!
//! `pebble-challtestsrv` is the DNS server Pebble resolves the test domains
//! with (every name is 127.0.0.1). Binds loopback ports only.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use forgebase_core::Config;
use forgebase_core::config::DatabaseBackend;
use forgebase_core::config::tls_config::{AcmeChallenge, TlsMode};
use forgebase_server::start;
use forgebase_server::tls::x509;
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, ServerName};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

struct Kill(Child);

impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn pebble_env() -> Option<(PathBuf, PathBuf)> {
    let bin = std::env::var_os("FORGEBASE_TEST_PEBBLE_BIN").map(PathBuf::from)?;
    let certs = std::env::var_os("FORGEBASE_TEST_PEBBLE_CERTS").map(PathBuf::from)?;
    (bin.join("pebble").is_file() && certs.join("pebble.minica.pem").is_file()).then_some((bin, certs))
}

async fn wait_for_port(port: u16) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while TcpStream::connect(("127.0.0.1", port)).await.is_err() {
        assert!(Instant::now() < deadline, "port {port} did not open");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn client(roots: &[CertificateDer<'static>]) -> Arc<rustls::ClientConfig> {
    let mut store = rustls::RootCertStore::empty();
    for r in roots {
        store.add(r.clone()).unwrap();
    }
    let mut cfg = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(store)
        .with_no_client_auth();
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    Arc::new(cfg)
}

/// GET over TLS (HTTP/1.1, `Connection: close`): `(status, body, serial of the leaf)`.
async fn get(addr: SocketAddr, sni: &str, roots: &[CertificateDer<'static>], path: &str) -> (u16, String, String) {
    let tcp = TcpStream::connect(addr).await.unwrap();
    let mut s = tokio_rustls::TlsConnector::from(client(roots))
        .connect(ServerName::try_from(sni.to_owned()).unwrap(), tcp)
        .await
        .unwrap_or_else(|e| panic!("TLS to {addr} as {sni}: {e}"));
    let serial = {
        let leaf = &s.get_ref().1.peer_certificates().unwrap()[0];
        x509::parse_certificate(leaf.as_ref()).unwrap().serial
    };
    s.write_all(format!("GET {path} HTTP/1.1\r\nHost: {sni}\r\nConnection: close\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out).await;
    let text = String::from_utf8_lossy(&out).to_string();
    let status = text.split_whitespace().nth(1).unwrap_or("0").parse().unwrap_or(0);
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_owned())
        .unwrap_or_default();
    (status, body, serial)
}

struct Pebble {
    _dns: Kill,
    _ca: Kill,
    dir_port: u16,
    mgmt_port: u16,
    minica: CertificateDer<'static>,
}

/// Pebble's own test EAB key (test/config/pebble-config-external-account-bindings.json).
const EAB_KID: &str = "kid-1";
const EAB_KEY: &str = "zWNDZM6eQGHWpSRTPal5eIUYFTu7EajVIoguysqZ9wG44nMEtx3MUAsUDkMTQ12W";

fn start_pebble(bin: &Path, certs: &Path, work: &Path, https_port: u16, http_port: u16, eab: bool) -> Pebble {
    let (dir_port, mgmt_port, dns_port, dns_mgmt) = (free_port(), free_port(), free_port(), free_port());
    let mut config = serde_json::json!({
        "pebble": {
            "listenAddress": format!("127.0.0.1:{dir_port}"),
            "managementListenAddress": format!("127.0.0.1:{mgmt_port}"),
            "certificate": certs.join("localhost/cert.pem"),
            "privateKey": certs.join("localhost/key.pem"),
            "httpPort": http_port,
            "tlsPort": https_port,
            "ocspResponderURL": "",
            "externalAccountBindingRequired": false,
            "retryAfter": { "authz": 1, "order": 1 },
            "keyAlgorithm": "ecdsa",
            "profiles": { "default": { "description": "default", "validityPeriod": 7_776_000 } }
        }
    });
    if eab {
        config["pebble"]["externalAccountBindingRequired"] = true.into();
        config["pebble"]["externalAccountMACKeys"] = serde_json::json!({ EAB_KID: EAB_KEY });
    }
    let path = work.join(format!("pebble-config-{dir_port}.json"));
    std::fs::write(&path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
    let quiet = || Stdio::null();
    let dns = Command::new(bin.join("pebble-challtestsrv"))
        .args([
            "-dnsserver",
            &format!("127.0.0.1:{dns_port}"),
            "-defaultIPv4",
            "127.0.0.1",
            "-defaultIPv6",
            "",
            "-http01",
            "",
            "-https01",
            "",
            "-tlsalpn01",
            "",
            "-doh",
            "",
            "-management",
            &format!("127.0.0.1:{dns_mgmt}"),
        ])
        .stdout(quiet())
        .stderr(quiet())
        .spawn()
        .expect("pebble-challtestsrv runs");
    let ca = Command::new(bin.join("pebble"))
        .args([
            "-config",
            &path.display().to_string(),
            "-dnsserver",
            &format!("127.0.0.1:{dns_port}"),
        ])
        .env("PEBBLE_VA_NOSLEEP", "1")
        .env("PEBBLE_WFE_NONCEREJECT", "0")
        .env("PEBBLE_AUTHZREUSE", "0")
        .stdout(quiet())
        .stderr(quiet())
        .spawn()
        .expect("pebble runs");
    let minica = CertificateDer::from_pem_file(certs.join("pebble.minica.pem")).unwrap();
    Pebble {
        _dns: Kill(dns),
        _ca: Kill(ca),
        dir_port,
        mgmt_port,
        minica,
    }
}

fn config(work: &Path, pebble: &Pebble, certs: &Path, https: u16, http: u16, c: AcmeChallenge, domain: &str) -> Config {
    let mut cfg = Config::defaults(work);
    cfg.data_dir = work.join(format!("data-{}", c.as_str()));
    cfg.server.host = "127.0.0.1".into();
    cfg.server.port = https;
    cfg.server.shutdown_timeout_secs = 5;
    cfg.database.backend = DatabaseBackend::Sqlite;
    cfg.tls.mode = TlsMode::Acme;
    cfg.tls.http_port = http;
    cfg.tls.acme.domains = vec![domain.to_owned()];
    cfg.tls.acme.email = "ops@forgebase.test".into();
    cfg.tls.acme.directory_url = format!("https://127.0.0.1:{}/dir", pebble.dir_port);
    cfg.tls.acme.challenges = vec![c];
    cfg.tls.acme.ca_file = certs.join("pebble.minica.pem").display().to_string();
    cfg.validate().unwrap();
    cfg
}

async fn scenario(work: &Path, pebble: &Pebble, certs: &Path, https: u16, http: u16, c: AcmeChallenge, domain: &str) {
    let cfg = config(work, pebble, certs, https, http, c, domain);
    let server = start(&cfg).await.unwrap();
    let tls = server.tls().unwrap().clone();
    let deadline = Instant::now() + Duration::from_secs(90);
    while tls.store().info().is_none() {
        assert!(
            Instant::now() < deadline,
            "no certificate from Pebble ({}): {}",
            c.as_str(),
            tls.status_json()
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    // Pebble's issuing root, from its management interface.
    let mgmt: SocketAddr = format!("127.0.0.1:{}", pebble.mgmt_port).parse().unwrap();
    let (status, root_pem, _) = get(mgmt, "localhost", std::slice::from_ref(&pebble.minica), "/roots/0").await;
    assert_eq!(status, 200, "{root_pem}");
    let root = CertificateDer::from_pem_slice(root_pem.as_bytes())
        .unwrap()
        .into_owned();

    // A client that trusts only Pebble's root verifies the served chain.
    let addr: SocketAddr = format!("127.0.0.1:{https}").parse().unwrap();
    let (status, body, serial) = get(addr, domain, std::slice::from_ref(&root), "/health").await;
    assert_eq!(status, 200, "{body}");
    let info = tls.store().info().unwrap();
    assert_eq!(serial, info.serial);
    assert_eq!(info.names, [domain]);
    assert!(info.issuer.to_lowercase().contains("pebble"), "{}", info.issuer);

    let s = tls.status_json();
    assert_eq!(s["mode"], "acme");
    assert_eq!(s["renewal_failures"], 0, "{s}");
    assert!(s["last_error"].is_null(), "{s}");
    assert!(s["last_renewal"].is_string(), "{s}");
    let expires = s["certificate"]["expires_in_secs"].as_i64().unwrap();
    assert!(expires > 89 * 86_400, "{s}");
    server.shutdown().await.unwrap();

    // Restart: the sealed certificate is served at once.
    let again = start(&cfg).await.unwrap();
    assert_eq!(
        again.tls().unwrap().store().info().map(|i| i.serial).as_deref(),
        Some(serial.as_str()),
        "the stored certificate is served before any ACME traffic"
    );
    let (status, _, served) = get(addr, domain, &[root], "/health").await;
    assert_eq!((status, served.as_str()), (200, serial.as_str()));
    again.shutdown().await.unwrap();
}

#[tokio::test]
#[ignore = "needs Pebble: FORGEBASE_TEST_PEBBLE_BIN and FORGEBASE_TEST_PEBBLE_CERTS (docs/qa.md)"]
async fn acme_end_to_end_against_pebble() {
    let Some((bin, certs)) = pebble_env() else {
        eprintln!("skipped: set FORGEBASE_TEST_PEBBLE_BIN and FORGEBASE_TEST_PEBBLE_CERTS (docs/qa.md)");
        return;
    };
    let work = tempfile::tempdir().unwrap();
    let (https, http) = (free_port(), free_port());
    let pebble = start_pebble(&bin, &certs, work.path(), https, http, false);
    wait_for_port(pebble.dir_port).await;
    wait_for_port(pebble.mgmt_port).await;

    scenario(
        work.path(),
        &pebble,
        &certs,
        https,
        http,
        AcmeChallenge::TlsAlpn01,
        "alpn.forgebase.test",
    )
    .await;
    scenario(
        work.path(),
        &pebble,
        &certs,
        https,
        http,
        AcmeChallenge::Http01,
        "http.forgebase.test",
    )
    .await;
}

/// A CA that requires external account binding (ZeroSSL-style): without the
/// EAB key the request fails, is retried later and the server keeps running;
/// with it a certificate is issued.
#[tokio::test]
#[ignore = "needs Pebble: FORGEBASE_TEST_PEBBLE_BIN and FORGEBASE_TEST_PEBBLE_CERTS (docs/qa.md)"]
async fn acme_with_external_account_binding_against_pebble() {
    let Some((bin, certs)) = pebble_env() else {
        eprintln!("skipped: set FORGEBASE_TEST_PEBBLE_BIN and FORGEBASE_TEST_PEBBLE_CERTS (docs/qa.md)");
        return;
    };
    let work = tempfile::tempdir().unwrap();
    let (https, http) = (free_port(), free_port());
    let pebble = start_pebble(&bin, &certs, work.path(), https, http, true);
    wait_for_port(pebble.dir_port).await;
    wait_for_port(pebble.mgmt_port).await;

    // No EAB: refused, counted, no certificate, still serving.
    let cfg = config(
        work.path(),
        &pebble,
        &certs,
        https,
        http,
        AcmeChallenge::TlsAlpn01,
        "eab.forgebase.test",
    );
    let server = start(&cfg).await.unwrap();
    let tls = server.tls().unwrap().clone();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let s = tls.status_json();
        if s["renewal_failures"].as_u64().unwrap_or(0) >= 1 {
            let err = s["last_error"].as_str().unwrap_or_default().to_lowercase();
            assert!(err.contains("external"), "{s}");
            assert!(s["certificate"].is_null(), "{s}");
            break;
        }
        assert!(Instant::now() < deadline, "no failure reported: {s}");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    server.shutdown().await.unwrap();

    // With the key: issued.
    let mut cfg = cfg;
    cfg.data_dir = work.path().join("data-eab");
    cfg.tls.acme.eab_kid = EAB_KID.into();
    cfg.tls.acme.eab_hmac_key = Some(forgebase_core::config::tls_config::SecretValue::new(EAB_KEY));
    cfg.validate().unwrap();
    let server = start(&cfg).await.unwrap();
    let tls = server.tls().unwrap().clone();
    let deadline = Instant::now() + Duration::from_secs(60);
    while tls.store().info().is_none() {
        assert!(
            Instant::now() < deadline,
            "no certificate with EAB: {}",
            tls.status_json()
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert_eq!(tls.store().info().unwrap().names, ["eab.forgebase.test"]);
    server.shutdown().await.unwrap();
}
