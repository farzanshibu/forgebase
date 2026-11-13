//! The ACME client (RFC 8555): `instant-acme` for the protocol (JWS, nonces,
//! orders, authorizations), over ForgeBase's own outbound HTTPS client
//! (`forgebase_auth::net::HttpsClient`: rustls with the ring provider, the
//! Mozilla roots plus `[tls.acme] ca_file`), so no second TLS stack or HTTP
//! client enters the binary.
//!
//! One issuance: load or create the account (sealed in the data directory),
//! create an order for the domains, answer each pending authorization with
//! the first configured challenge type the CA offers (`tls-alpn-01`: a
//! challenge certificate served on the HTTPS listener; `http-01`: the key
//! authorization served by the plain HTTP listener), wait for the order to be
//! ready, finalize it with a CSR for a fresh ECDSA P-256 key, and download the
//! chain. Challenge answers are removed afterwards, whatever happened.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use bytes::Bytes;
use forgebase_auth::net::{HttpsClient, TlsSettings};
use forgebase_auth::social::{HttpClient as _, HttpRequest};
use forgebase_core::config::tls_config::{AcmeChallenge, AcmeConfig};
use http_body_util::BodyExt as _;
use instant_acme::{
    Account, AccountCredentials, AuthorizationStatus, BodyWrapper, BytesResponse, ChallengeType, ExternalAccountKey,
    Identifier, NewAccount, NewOrder, OrderStatus, RetryPolicy,
};

use super::acme::{AcmeStorage, BoxFuture, CertificateAuthority, IssuedCertificate};
use super::certs::CertStore;
use super::{Clock, TlsError, x509};

/// `instant_acme::HttpClient` over [`HttpsClient`].
struct Transport(HttpsClient);

fn other(e: impl std::error::Error + Send + Sync + 'static) -> instant_acme::Error {
    instant_acme::Error::Other(Box::new(e))
}

impl instant_acme::HttpClient for Transport {
    fn request(
        &self,
        req: http::Request<BodyWrapper<Bytes>>,
    ) -> Pin<Box<dyn Future<Output = Result<BytesResponse, instant_acme::Error>> + Send>> {
        let client = self.0.clone();
        Box::pin(async move {
            let (parts, body) = req.into_parts();
            let body = body.collect().await.map(|c| c.to_bytes()).unwrap_or_default();
            let method = match parts.method {
                http::Method::GET => "GET",
                http::Method::POST => "POST",
                http::Method::HEAD => "HEAD",
                _ => return Err(instant_acme::Error::Str("unsupported HTTP method")),
            };
            let mut headers: Vec<(String, String)> = parts
                .headers
                .iter()
                .filter(|(k, _)| *k != http::header::USER_AGENT)
                .filter_map(|(k, v)| v.to_str().ok().map(|v| (k.as_str().to_owned(), v.to_owned())))
                .collect();
            if !parts.headers.contains_key(http::header::ACCEPT) {
                // The certificate is PEM, not JSON: accept whatever the CA sends.
                headers.push(("accept".to_owned(), "*/*".to_owned()));
            }
            let resp = client
                .send(HttpRequest {
                    method,
                    url: parts.uri.to_string(),
                    headers,
                    body: body.to_vec(),
                })
                .await
                .map_err(other)?;
            let mut b = http::Response::builder().status(resp.status);
            for (k, v) in &resp.headers {
                b = b.header(k.as_str(), v.as_str());
            }
            let (parts, ()) = b.body(()).map_err(instant_acme::Error::Http)?.into_parts();
            Ok(BytesResponse {
                parts,
                body: Box::new(Bytes::from(resp.body)),
            })
        })
    }
}

/// An RFC 8555 certificate authority reached over ACME.
pub struct AcmeCa {
    directory: String,
    contact: Vec<String>,
    eab: Option<(String, Vec<u8>)>,
    challenges: Vec<AcmeChallenge>,
    http: HttpsClient,
    storage: AcmeStorage,
    clock: Arc<dyn Clock>,
    account: tokio::sync::Mutex<Option<Account>>,
    retry: RetryPolicy,
}

impl std::fmt::Debug for AcmeCa {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AcmeCa")
            .field("directory", &self.directory)
            .field("challenges", &self.challenges)
            .finish_non_exhaustive()
    }
}

fn acme_err(context: &str, e: &instant_acme::Error) -> TlsError {
    TlsError::Acme(format!("{context}: {e}"))
}

fn challenge_type(c: AcmeChallenge) -> ChallengeType {
    match c {
        AcmeChallenge::TlsAlpn01 => ChallengeType::TlsAlpn01,
        AcmeChallenge::Http01 => ChallengeType::Http01,
    }
}

/// Removes the published challenge answers when an issuance ends.
struct ClearOnDrop<'a>(&'a CertStore);

impl Drop for ClearOnDrop<'_> {
    fn drop(&mut self) {
        self.0.clear_challenges();
    }
}

impl AcmeCa {
    /// A client for `cfg`'s directory, storing its account in `storage`.
    ///
    /// # Errors
    /// `ca_file` cannot be read; the EAB key is not base64url.
    pub fn new(cfg: &AcmeConfig, storage: AcmeStorage, clock: Arc<dyn Clock>) -> Result<Self, TlsError> {
        let mut tls = TlsSettings::new();
        if !cfg.ca_file.trim().is_empty() {
            tls = tls
                .with_pem_file(Path::new(&cfg.ca_file))
                .map_err(|e| TlsError::Setup(format!("[tls.acme] ca_file: {e}")))?;
        }
        let http = HttpsClient::new(&tls)
            .map_err(|e| TlsError::Setup(e.to_string()))?
            .with_timeout(Duration::from_secs(30))
            .with_max_body(1 << 20);
        let eab = match &cfg.eab_hmac_key {
            Some(key) => {
                let raw = key.expose().trim_end_matches('=');
                let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .decode(raw)
                    .map_err(|_| TlsError::Setup("the ACME EAB HMAC key is not base64url".into()))?;
                Some((cfg.eab_kid.trim().to_owned(), bytes))
            }
            None => None,
        };
        let email = cfg.email.trim();
        Ok(Self {
            directory: cfg.directory().to_owned(),
            contact: if email.is_empty() {
                Vec::new()
            } else {
                vec![format!("mailto:{email}")]
            },
            eab,
            challenges: cfg.challenges.clone(),
            http,
            storage,
            clock,
            account: tokio::sync::Mutex::new(None),
            retry: RetryPolicy::new()
                .initial_delay(Duration::from_millis(500))
                .backoff(1.5)
                .timeout(Duration::from_secs(120)),
        })
    }

    fn builder(&self) -> instant_acme::AccountBuilder {
        Account::builder_with_http(Box::new(Transport(self.http.clone())))
    }

    async fn account(&self) -> Result<Account, TlsError> {
        let mut slot = self.account.lock().await;
        if let Some(a) = slot.as_ref() {
            return Ok(a.clone());
        }
        let storage = self.storage.clone();
        let stored = tokio::task::spawn_blocking(move || storage.load_account())
            .await
            .map_err(|e| TlsError::Setup(e.to_string()))?;
        let stored = match stored {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "the stored ACME account cannot be read; creating a new one");
                None
            }
        };
        if let Some(json) = stored
            && let Ok(creds) = serde_json::from_slice::<AccountCredentials>(&json)
        {
            let account = self
                .builder()
                .from_credentials(creds)
                .await
                .map_err(|e| acme_err("cannot reach the ACME directory", &e))?;
            *slot = Some(account.clone());
            return Ok(account);
        }
        let contact: Vec<&str> = self.contact.iter().map(String::as_str).collect();
        let eab = self
            .eab
            .as_ref()
            .map(|(kid, key)| ExternalAccountKey::new(kid.clone(), key));
        let (account, creds) = self
            .builder()
            .create(
                &NewAccount {
                    contact: &contact,
                    terms_of_service_agreed: true,
                    only_return_existing: false,
                },
                self.directory.clone(),
                eab.as_ref(),
            )
            .await
            .map_err(|e| acme_err("cannot create the ACME account", &e))?;
        let json = serde_json::to_vec(&creds).map_err(|e| TlsError::Setup(e.to_string()))?;
        let storage = self.storage.clone();
        tokio::task::spawn_blocking(move || storage.save_account(&json))
            .await
            .map_err(|e| TlsError::Setup(e.to_string()))??;
        tracing::info!(directory = %self.directory, id = %account.id(), "created an ACME account");
        *slot = Some(account.clone());
        Ok(account)
    }

    async fn order(&self, domains: &[String], challenges: &CertStore) -> Result<IssuedCertificate, TlsError> {
        let account = self.account().await?;
        let identifiers: Vec<Identifier> = domains.iter().map(|d| Identifier::Dns(d.clone())).collect();
        let mut order = account
            .new_order(&NewOrder::new(&identifiers))
            .await
            .map_err(|e| acme_err("the CA refused the order", &e))?;
        let _clear = ClearOnDrop(challenges);
        let mut used = Vec::new();
        {
            let mut authz = order.authorizations();
            while let Some(a) = authz.next().await {
                let mut a = a.map_err(|e| acme_err("cannot read an authorization", &e))?;
                let name = a.identifier().to_string();
                match a.status {
                    AuthorizationStatus::Pending => {}
                    AuthorizationStatus::Valid => continue,
                    other => {
                        return Err(TlsError::Acme(format!(
                            "the authorization for {name} is {other:?}; it cannot be answered"
                        )));
                    }
                }
                let Some(kind) = self
                    .challenges
                    .iter()
                    .copied()
                    .find(|c| a.challenges.iter().any(|offered| offered.r#type == challenge_type(*c)))
                else {
                    return Err(TlsError::Acme(format!(
                        "the CA offers none of the configured challenges ({}) for {name}",
                        self.challenges
                            .iter()
                            .map(|c| c.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )));
                };
                let Some(mut ch) = a.challenge(challenge_type(kind)) else {
                    return Err(TlsError::Acme(format!("no {} challenge for {name}", kind.as_str())));
                };
                let key_auth = ch.key_authorization();
                match kind {
                    AcmeChallenge::TlsAlpn01 => {
                        challenges.put_alpn_challenge(&name, key_auth.digest().as_ref(), self.clock.now())?;
                    }
                    AcmeChallenge::Http01 => challenges.put_http_challenge(&ch.token, key_auth.as_str()),
                }
                ch.set_ready()
                    .await
                    .map_err(|e| acme_err(&format!("the {} challenge for {name} failed", kind.as_str()), &e))?;
                used.push(format!("{name}: {}", kind.as_str()));
            }
        }
        tracing::debug!(challenges = ?used, "ACME challenges answered");
        let status = order
            .poll_ready(&self.retry)
            .await
            .map_err(|e| acme_err("the CA did not validate the domains", &e))?;
        if status != OrderStatus::Ready {
            let mut why = Vec::new();
            let mut authz = order.authorizations();
            while let Some(Ok(mut a)) = authz.next().await {
                if a.refresh().await.is_ok() {
                    for c in &a.challenges {
                        if let Some(p) = &c.error {
                            why.push(format!("{}: {p}", a.identifier()));
                        }
                    }
                }
            }
            return Err(TlsError::Acme(format!(
                "the order is {status:?}: {}",
                if why.is_empty() {
                    "no details from the CA".to_owned()
                } else {
                    why.join("; ")
                }
            )));
        }
        let (key_der, pair) = x509::generate_key().map_err(|e| TlsError::Setup(e.to_string()))?;
        let csr = x509::csr(domains, &pair).map_err(|e| TlsError::Setup(e.to_string()))?;
        order
            .finalize_csr(&csr)
            .await
            .map_err(|e| acme_err("the CA refused to finalize the order", &e))?;
        let chain = order
            .poll_certificate(&self.retry)
            .await
            .map_err(|e| acme_err("cannot download the certificate", &e))?;
        Ok(IssuedCertificate {
            chain_pem: chain,
            key_pem: x509::pem("PRIVATE KEY", &key_der),
        })
    }
}

impl CertificateAuthority for AcmeCa {
    fn issue<'a>(
        &'a self,
        domains: &'a [String],
        challenges: &'a CertStore,
    ) -> BoxFuture<'a, Result<IssuedCertificate, TlsError>> {
        Box::pin(self.order(domains, challenges))
    }
}
