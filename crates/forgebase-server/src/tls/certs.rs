//! Certificates in memory: PEM loading with the key-match check, the
//! certificate store every TLS listener resolves from (so a reload or a
//! renewal reaches HTTPS and RESP at once, without touching open
//! connections), and the ACME challenge answers (`tls-alpn-01` certificates
//! by server name, `http-01` key authorizations by token).

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, RwLock};

use forgebase_core::config::tls_config::TlsVersion;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use rustls::{ServerConfig, SupportedProtocolVersion};

use super::TlsError;
use super::x509::{self, CertInfo};

/// ALPN protocol of the `tls-alpn-01` challenge (RFC 8737 §6.2).
pub const ACME_TLS_ALPN: &[u8] = b"acme-tls/1";

/// A certificate chain with its signing key, ready to serve.
#[derive(Debug, Clone)]
pub struct Certificate {
    /// What rustls serves.
    pub key: Arc<CertifiedKey>,
    /// The leaf, as read by [`x509::parse_certificate`].
    pub info: CertInfo,
}

fn provider() -> rustls::crypto::CryptoProvider {
    rustls::crypto::ring::default_provider()
}

/// Builds a [`Certificate`] from PEM text; `cert_what` and `key_what` name
/// the sources in error messages (file paths, "the stored ACME certificate").
///
/// # Errors
/// No certificate or key in the PEM, a key rustls cannot use, a key that
/// does not belong to the first certificate, a leaf that cannot be read.
pub fn from_pem(chain_pem: &[u8], key_pem: &[u8], cert_what: &str, key_what: &str) -> Result<Certificate, TlsError> {
    let chain = CertificateDer::pem_slice_iter(chain_pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| TlsError::BadCertificate {
            what: cert_what.to_owned(),
            message: format!("not a PEM certificate chain: {e}"),
        })?;
    if chain.is_empty() {
        return Err(TlsError::BadCertificate {
            what: cert_what.to_owned(),
            message: "holds no -----BEGIN CERTIFICATE----- block".into(),
        });
    }
    let key = PrivateKeyDer::from_pem_slice(key_pem).map_err(|e| TlsError::BadKey {
        what: key_what.to_owned(),
        message: format!("holds no PEM private key ({e})"),
    })?;
    from_der(chain, key, cert_what, key_what)
}

/// Builds a [`Certificate`] from DER (leaf first).
///
/// # Errors
/// See [`from_pem`].
pub fn from_der(
    chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
    cert_what: &str,
    key_what: &str,
) -> Result<Certificate, TlsError> {
    let info = x509::parse_certificate(chain.first().map_or(&[][..], |c| c.as_ref())).map_err(|e| {
        TlsError::BadCertificate {
            what: cert_what.to_owned(),
            message: format!("the first certificate cannot be read: {e}"),
        }
    })?;
    let signing = provider()
        .key_provider
        .load_private_key(key)
        .map_err(|e| TlsError::BadKey {
            what: key_what.to_owned(),
            message: format!("is not a usable RSA, ECDSA or Ed25519 key: {e}"),
        })?;
    let ck = CertifiedKey::new(chain, signing);
    match ck.keys_match() {
        Ok(()) | Err(rustls::Error::InconsistentKeys(rustls::InconsistentKeys::Unknown)) => {}
        Err(rustls::Error::InconsistentKeys(rustls::InconsistentKeys::KeyMismatch)) => {
            return Err(TlsError::KeyMismatch {
                cert: cert_what.to_owned(),
                key: key_what.to_owned(),
            });
        }
        Err(e) => {
            return Err(TlsError::BadCertificate {
                what: cert_what.to_owned(),
                message: e.to_string(),
            });
        }
    }
    Ok(Certificate {
        key: Arc::new(ck),
        info,
    })
}

/// Reads and checks a certificate chain file and its key file.
///
/// # Errors
/// A file cannot be read, or see [`from_pem`].
pub fn from_files(cert: &Path, key: &Path) -> Result<Certificate, TlsError> {
    let read = |p: &Path| {
        std::fs::read(p).map_err(|source| TlsError::Read {
            path: p.to_path_buf(),
            source,
        })
    };
    let (c, k) = (read(cert)?, read(key)?);
    from_pem(&c, &k, &cert.display().to_string(), &key.display().to_string())
}

/// The certificate every TLS listener serves, plus pending ACME challenges.
#[derive(Default)]
pub struct CertStore {
    current: RwLock<Option<Certificate>>,
    alpn: RwLock<HashMap<String, Arc<CertifiedKey>>>,
    http: RwLock<HashMap<String, String>>,
}

impl std::fmt::Debug for CertStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CertStore")
            .field("current", &self.info())
            .field("alpn_challenges", &read(&self.alpn).len())
            .field("http_challenges", &read(&self.http).len())
            .finish()
    }
}

fn read<T>(l: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    l.read().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn write<T>(l: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    l.write().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl CertStore {
    /// An empty store (handshakes fail until a certificate is installed).
    pub fn new() -> Self {
        Self::default()
    }

    /// Serves `cert` from the next handshake on; open connections keep the
    /// certificate they were set up with.
    pub fn install(&self, cert: Certificate) {
        *write(&self.current) = Some(cert);
    }

    /// The leaf being served, if any.
    pub fn info(&self) -> Option<CertInfo> {
        read(&self.current).as_ref().map(|c| c.info.clone())
    }

    /// The certificate being served, if any.
    pub fn current(&self) -> Option<Certificate> {
        read(&self.current).clone()
    }

    /// Publishes the `tls-alpn-01` answer for `domain`: a self-signed
    /// certificate holding `digest` (SHA-256 of the key authorization).
    ///
    /// # Errors
    /// The challenge certificate cannot be built.
    pub fn put_alpn_challenge(&self, domain: &str, digest: &[u8], now: i64) -> Result<(), TlsError> {
        let (cert, key) =
            x509::alpn_challenge_certificate(domain, digest, now).map_err(|e| TlsError::Setup(e.to_string()))?;
        let signing = provider()
            .key_provider
            .load_private_key(PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key)))
            .map_err(|e| TlsError::Setup(e.to_string()))?;
        let ck = CertifiedKey::new(vec![CertificateDer::from(cert)], signing);
        write(&self.alpn).insert(domain.to_ascii_lowercase(), Arc::new(ck));
        Ok(())
    }

    /// Publishes the `http-01` answer: `key_authorization` under `token`.
    pub fn put_http_challenge(&self, token: &str, key_authorization: &str) {
        write(&self.http).insert(token.to_owned(), key_authorization.to_owned());
    }

    /// The `http-01` answer for `token`.
    pub fn http_challenge(&self, token: &str) -> Option<String> {
        read(&self.http).get(token).cloned()
    }

    /// Removes every pending challenge answer.
    pub fn clear_challenges(&self) {
        write(&self.alpn).clear();
        write(&self.http).clear();
    }
}

impl ResolvesServerCert for CertStore {
    fn resolve(&self, hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        let acme = hello.alpn().is_some_and(|mut p| p.any(|p| p == ACME_TLS_ALPN));
        if acme {
            // RFC 8737 §3: only the challenge certificate, only for its name.
            let name = hello.server_name()?.to_ascii_lowercase();
            return read(&self.alpn).get(&name).cloned();
        }
        read(&self.current).as_ref().map(|c| Arc::clone(&c.key))
    }
}

/// A rustls server configuration resolving from `store`, accepting TLS
/// `min` and later, offering `alpn` (server preference order).
///
/// # Errors
/// The protocol versions are not supported by the crypto provider.
pub fn server_config(store: Arc<CertStore>, min: TlsVersion, alpn: &[&[u8]]) -> Result<Arc<ServerConfig>, TlsError> {
    let versions: &[&'static SupportedProtocolVersion] = match min {
        TlsVersion::V1_2 => &[&rustls::version::TLS13, &rustls::version::TLS12],
        TlsVersion::V1_3 => &[&rustls::version::TLS13],
    };
    let mut cfg = ServerConfig::builder_with_provider(Arc::new(provider()))
        .with_protocol_versions(versions)
        .map_err(|e| TlsError::Setup(e.to_string()))?
        .with_no_client_auth()
        .with_cert_resolver(store);
    cfg.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
    Ok(Arc::new(cfg))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn pair(names: &[&str]) -> (String, String) {
        let now = 1_794_614_400;
        let (c, k) = x509::self_signed(names, now - 60, now + 86_400, &[]).unwrap();
        (x509::pem("CERTIFICATE", &c), x509::pem("PRIVATE KEY", &k))
    }

    #[test]
    fn a_matching_pair_loads_and_a_mismatched_key_is_refused() {
        let (c1, k1) = pair(&["a.example.com"]);
        let (_, k2) = pair(&["b.example.com"]);
        let cert = from_pem(c1.as_bytes(), k1.as_bytes(), "cert.pem", "key.pem").unwrap();
        assert_eq!(cert.info.names, ["a.example.com"]);
        let e = from_pem(c1.as_bytes(), k2.as_bytes(), "cert.pem", "other.pem").unwrap_err();
        assert!(matches!(e, TlsError::KeyMismatch { .. }), "{e}");
        assert!(e.to_string().contains("does not belong"), "{e}");
    }

    #[test]
    fn empty_or_swapped_files_say_what_is_wrong() {
        let (c, k) = pair(&["a.example.com"]);
        let e = from_pem(b"", k.as_bytes(), "cert.pem", "key.pem").unwrap_err();
        assert!(e.to_string().contains("no -----BEGIN CERTIFICATE-----"), "{e}");
        let e = from_pem(c.as_bytes(), c.as_bytes(), "cert.pem", "key.pem").unwrap_err();
        assert!(e.to_string().contains("no PEM private key"), "{e}");
        let e = from_pem(k.as_bytes(), k.as_bytes(), "cert.pem", "key.pem").unwrap_err();
        assert!(e.to_string().contains("CERTIFICATE"), "{e}");
    }

    #[test]
    fn challenges_are_kept_by_name_and_token() {
        let s = CertStore::new();
        s.put_alpn_challenge("A.Example.com", &[1; 32], 1_794_614_400).unwrap();
        assert_eq!(read(&s.alpn).len(), 1);
        assert!(read(&s.alpn).contains_key("a.example.com"));
        s.put_http_challenge("tok", "tok.thumb");
        assert_eq!(s.http_challenge("tok").as_deref(), Some("tok.thumb"));
        assert_eq!(s.http_challenge("other"), None);
        s.clear_challenges();
        assert_eq!(s.http_challenge("tok"), None);
        assert!(read(&s.alpn).is_empty());
    }
}
