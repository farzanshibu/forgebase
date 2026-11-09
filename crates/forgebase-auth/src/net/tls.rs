//! TLS for outbound connections: rustls with the ring crypto provider,
//! trusting the Mozilla CA set from `webpki-roots` plus optional extra PEM
//! anchors (a corporate CA for LDAPS, an egress proxy, tests).
//!
//! Certificates are always verified (chain, validity, name); there is no
//! option to turn verification off. TLS 1.2 and 1.3 with rustls' safe
//! defaults.

use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName};
use rustls::{ClientConfig, RootCertStore};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

use super::{host_port, http1};
use crate::AuthError;
use crate::social::http_client::{BoxFuture, HttpClient, HttpRequest, HttpResponse};

/// Trust settings for outbound TLS.
#[derive(Clone, Default)]
pub struct TlsSettings {
    extra_roots: Vec<CertificateDer<'static>>,
}

impl std::fmt::Debug for TlsSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TlsSettings")
            .field("extra_roots", &self.extra_roots.len())
            .finish()
    }
}

impl TlsSettings {
    /// The webpki-roots trust anchors only.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds every certificate of a PEM bundle as a trust anchor.
    pub fn with_pem(mut self, pem: &[u8]) -> Result<Self, AuthError> {
        let mut n = 0;
        for c in CertificateDer::pem_slice_iter(pem) {
            let c = c.map_err(|e| AuthError::invalid(format!("bad PEM certificate: {e}")))?;
            self.extra_roots.push(c.into_owned());
            n += 1;
        }
        if n == 0 {
            return Err(AuthError::invalid("the PEM bundle holds no certificate"));
        }
        Ok(self)
    }

    /// Adds the certificates of a PEM file.
    pub fn with_pem_file(self, path: &std::path::Path) -> Result<Self, AuthError> {
        let pem = std::fs::read(path)
            .map_err(|e| AuthError::Internal(format!("cannot read CA file {}: {e}", path.display())))?;
        self.with_pem(&pem)
    }

    /// Number of extra trust anchors.
    pub fn extra_roots(&self) -> usize {
        self.extra_roots.len()
    }

    /// A rustls client configuration offering `alpn` protocols.
    pub fn client_config(&self, alpn: &[&[u8]]) -> Result<Arc<ClientConfig>, AuthError> {
        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        for c in &self.extra_roots {
            roots
                .add(c.clone())
                .map_err(|e| AuthError::invalid(format!("unusable CA certificate: {e}")))?;
        }
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut cfg = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| AuthError::Internal(format!("TLS setup: {e}")))?
            .with_root_certificates(roots)
            .with_no_client_auth();
        cfg.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
        Ok(Arc::new(cfg))
    }

    /// Wraps a connected stream in TLS for `host` (certificate verified).
    pub async fn upgrade(
        config: &Arc<ClientConfig>,
        host: &str,
        tcp: TcpStream,
    ) -> Result<TlsStream<TcpStream>, AuthError> {
        let name = ServerName::try_from(host.to_owned())
            .map_err(|_| AuthError::Provider(format!("{host:?} is not a valid TLS server name")))?;
        TlsConnector::from(config.clone())
            .connect(name, tcp)
            .await
            .map_err(|e| AuthError::Provider(format!("TLS handshake with {host} failed: {e}")))
    }
}

/// The default outbound HTTP client: `https://` over rustls (certificates
/// verified against [`TlsSettings`]) and plain `http://` (for loopback
/// mocks or a TLS-terminating egress proxy the operator configured).
#[derive(Clone)]
pub struct HttpsClient {
    config: Arc<ClientConfig>,
    timeout: Duration,
    max_body: usize,
}

impl std::fmt::Debug for HttpsClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpsClient")
            .field("timeout", &self.timeout)
            .field("max_body", &self.max_body)
            .finish_non_exhaustive()
    }
}

impl HttpsClient {
    /// A client trusting `tls`; 10 s timeout, 1 MiB response cap.
    pub fn new(tls: &TlsSettings) -> Result<Self, AuthError> {
        Ok(Self {
            config: tls.client_config(&[b"http/1.1"])?,
            timeout: Duration::from_secs(10),
            max_body: 1 << 20,
        })
    }

    /// Overall timeout of one request (connect, handshake, exchange).
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Response body cap.
    pub fn with_max_body(mut self, max_body: usize) -> Self {
        self.max_body = max_body;
        self
    }

    async fn exchange(&self, req: HttpRequest) -> Result<HttpResponse, AuthError> {
        let url = url::Url::parse(&req.url).map_err(|_| AuthError::Provider("bad URL".into()))?;
        let tls = match url.scheme() {
            "https" => true,
            "http" => false,
            _ => return Err(AuthError::Provider("only http and https URLs are supported".into())),
        };
        let (host, port) = host_port(&url, if tls { 443 } else { 80 })?;
        let tcp = TcpStream::connect((host.as_str(), port))
            .await
            .map_err(|_| AuthError::Provider(format!("could not reach {host}")))?;
        let _ = tcp.set_nodelay(true);
        if tls {
            let mut s = TlsSettings::upgrade(&self.config, &host, tcp).await?;
            http1::exchange(&mut s, &req, &url, self.max_body).await
        } else {
            let mut s = tcp;
            http1::exchange(&mut s, &req, &url, self.max_body).await
        }
    }
}

impl HttpClient for HttpsClient {
    fn send(&self, req: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, AuthError>> {
        Box::pin(async move {
            tokio::time::timeout(self.timeout, self.exchange(req))
                .await
                .map_err(|_| AuthError::Provider("request to the remote server timed out".into()))?
        })
    }
}
