//! `[tls] mode = "acme"`: the renewal scheduler and the sealed storage of
//! the ACME account and certificate (ADR-0033 §3).
//!
//! * **When.** A certificate is renewed at two thirds of its lifetime, minus
//!   a random jitter of up to a fifteenth of it (90-day certificates: between
//!   day 54 and day 60), so servers started together do not renew together. A
//!   missing certificate, one for other domains, or one past its renewal time
//!   is requested at once.
//! * **Failure.** The old certificate keeps being served. The next attempt
//!   waits `backoff_base`, doubling after each failure up to `backoff_max`
//!   (2 minutes up to 6 hours by default). Every failure is counted
//!   (`forgebase_tls_renewal_failures_total`), logged and shown in the status.
//! * **Storage.** `<data_dir>/tls/acme/<directory id>/` holds `account.sealed`
//!   (the ACME account credentials) and `certificate.sealed` (chain, private
//!   key and domains), both sealed with the data directory's keyring
//!   (XChaCha20-Poly1305, bound to the directory URL), plus `status.json`
//!   (no secrets: names, issuer, dates, last error) for `forgebase doctor`.
//!   The directory id is a hash of the directory URL, so staging and
//!   production accounts never mix.
//!
//! [`Renewer::tick`] does one step and says how long to wait; the loop in
//! [`spawn_renewer`] sleeps in between. Tests drive `tick` with a
//! [`ManualClock`](super::ManualClock) and a fake [`CertificateAuthority`].

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use forgebase_auth::crypto::Keyring;
use forgebase_observability::names;
use ring::rand::SecureRandom as _;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use super::certs::{self, CertStore};
use super::x509::{self, CertInfo};
use super::{Clock, TlsError, TlsStatus};

/// A boxed, sendable future.
pub type BoxFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// A certificate as a CA returns it.
#[derive(Clone)]
pub struct IssuedCertificate {
    /// PEM chain, leaf first.
    pub chain_pem: String,
    /// PEM (PKCS#8) private key of the leaf.
    pub key_pem: String,
}

impl std::fmt::Debug for IssuedCertificate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IssuedCertificate").finish_non_exhaustive()
    }
}

/// Something that issues certificates: the ACME client, or a fake in tests.
pub trait CertificateAuthority: Send + Sync + std::fmt::Debug {
    /// Obtains a certificate for `domains`, publishing challenge answers in
    /// `challenges` while it runs (and removing them afterwards).
    fn issue<'a>(
        &'a self,
        domains: &'a [String],
        challenges: &'a CertStore,
    ) -> BoxFuture<'a, Result<IssuedCertificate, TlsError>>;
}

/// Timing of renewals and retries.
#[derive(Debug, Clone, Copy)]
pub struct RenewalPolicy {
    /// Wait after the first failure; doubles per failure.
    pub backoff_base: Duration,
    /// Longest wait between failed attempts.
    pub backoff_max: Duration,
    /// Longest sleep between ticks (the expiry gauge and clock jumps are
    /// looked at again after it).
    pub max_sleep: Duration,
    /// Whether to subtract a random jitter from the renewal time.
    pub jitter: bool,
}

impl Default for RenewalPolicy {
    fn default() -> Self {
        Self {
            backoff_base: Duration::from_secs(120),
            backoff_max: Duration::from_secs(6 * 3600),
            max_sleep: Duration::from_secs(3600),
            jitter: true,
        }
    }
}

impl RenewalPolicy {
    /// Wait after `failures` consecutive failures (at least one).
    pub fn backoff(&self, failures: u32) -> Duration {
        let shift = failures.saturating_sub(1).min(30);
        self.backoff_base
            .checked_mul(1u32 << shift)
            .unwrap_or(self.backoff_max)
            .min(self.backoff_max)
    }
}

/// When a certificate is due: two thirds of its lifetime, minus
/// `jitter` (0 to 1) times a fifteenth of it.
pub fn renewal_time(info: &CertInfo, jitter: f64) -> i64 {
    let life = info.lifetime();
    let spread = life as f64 / 15.0 * jitter.clamp(0.0, 1.0);
    info.not_before + life * 2 / 3 - spread as i64
}

fn random_fraction() -> f64 {
    let mut b = [0u8; 4];
    if ring::rand::SystemRandom::new().fill(&mut b).is_err() {
        return 0.5;
    }
    f64::from(u32::from_be_bytes(b)) / f64::from(u32::MAX)
}

/// What `certificate.sealed` holds.
#[derive(Serialize, Deserialize)]
struct StoredCertificate {
    domains: Vec<String>,
    chain_pem: String,
    key_pem: String,
    issued_at: i64,
}

/// `status.json`: what `forgebase doctor` and operators may read (no secrets).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredStatus {
    /// The ACME directory.
    pub directory_url: String,
    /// Configured domains.
    pub domains: Vec<String>,
    /// Issuer of the certificate being served.
    pub issuer: Option<String>,
    /// Its validity start (Unix seconds).
    pub not_before: Option<i64>,
    /// Its validity end (Unix seconds).
    pub not_after: Option<i64>,
    /// Last successful issuance (Unix seconds).
    pub last_renewal: Option<i64>,
    /// Next attempt (Unix seconds).
    pub next_renewal: Option<i64>,
    /// Last failure message.
    pub last_error: Option<String>,
    /// When it happened (Unix seconds).
    pub last_error_at: Option<i64>,
}

/// The sealed files of one ACME directory.
#[derive(Clone)]
pub struct AcmeStorage {
    dir: PathBuf,
    directory_url: String,
    keyring: Keyring,
}

impl std::fmt::Debug for AcmeStorage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AcmeStorage")
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

/// `<data_dir>/tls/acme/<16 hex digits of SHA-256(directory URL)>`.
pub fn storage_dir(data_dir: &Path, directory_url: &str) -> PathBuf {
    let d = ring::digest::digest(&ring::digest::SHA256, directory_url.as_bytes());
    let id: String = d.as_ref()[..8].iter().map(|b| format!("{b:02x}")).collect();
    data_dir.join("tls").join("acme").join(id)
}

fn private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for d in dir.ancestors().take(3) {
            let _ = std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700));
        }
    }
    Ok(())
}

/// Writes `bytes` to `path` atomically (temporary file, fsync, rename), owner-only.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let dir = path.parent().unwrap_or(Path::new("."));
    private_dir(dir)?;
    let tmp = path.with_extension("tmp");
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path)?;
    #[cfg(unix)]
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

impl AcmeStorage {
    /// Storage for `directory_url` under `data_dir`.
    pub fn new(data_dir: &Path, directory_url: &str, keyring: Keyring) -> Self {
        Self {
            dir: storage_dir(data_dir, directory_url),
            directory_url: directory_url.to_owned(),
            keyring,
        }
    }

    /// The directory holding the files.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn aad(&self, what: &str) -> Vec<u8> {
        format!("forgebase/tls/acme/{what}/v1|{}", self.directory_url).into_bytes()
    }

    fn storage_err(&self, path: &Path, message: impl std::fmt::Display) -> TlsError {
        TlsError::Storage {
            path: path.to_path_buf(),
            message: message.to_string(),
        }
    }

    fn load_sealed(&self, name: &str, what: &str) -> Result<Option<Vec<u8>>, TlsError> {
        let path = self.dir.join(name);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(self.storage_err(&path, e)),
        };
        let sealed = base64::engine::general_purpose::STANDARD
            .decode(text.trim())
            .map_err(|e| self.storage_err(&path, format!("not base64: {e}")))?;
        let plain = self
            .keyring
            .open(&sealed, &self.aad(what))
            .map_err(|e| self.storage_err(&path, e))?;
        Ok(Some(plain))
    }

    fn save_sealed(&self, name: &str, what: &str, plain: &[u8]) -> Result<(), TlsError> {
        let path = self.dir.join(name);
        let sealed = self
            .keyring
            .seal(plain, &self.aad(what))
            .map_err(|e| self.storage_err(&path, e))?;
        let text = base64::engine::general_purpose::STANDARD.encode(sealed);
        write_private(&path, text.as_bytes()).map_err(|e| self.storage_err(&path, e))
    }

    /// The stored ACME account credentials (JSON), if any.
    ///
    /// # Errors
    /// The file cannot be read or unsealed (another master key).
    pub fn load_account(&self) -> Result<Option<Vec<u8>>, TlsError> {
        self.load_sealed("account.sealed", "account")
    }

    /// Stores the ACME account credentials (JSON).
    ///
    /// # Errors
    /// The file cannot be written.
    pub fn save_account(&self, json: &[u8]) -> Result<(), TlsError> {
        self.save_sealed("account.sealed", "account", json)
    }

    fn load_certificate(&self) -> Result<Option<StoredCertificate>, TlsError> {
        let Some(plain) = self.load_sealed("certificate.sealed", "certificate")? else {
            return Ok(None);
        };
        let path = self.dir.join("certificate.sealed");
        serde_json::from_slice(&plain)
            .map(Some)
            .map_err(|e| self.storage_err(&path, e))
    }

    fn save_certificate(&self, c: &StoredCertificate) -> Result<(), TlsError> {
        let json = serde_json::to_vec(c).map_err(|e| TlsError::Setup(e.to_string()))?;
        self.save_sealed("certificate.sealed", "certificate", &json)
    }

    /// Writes `status.json`.
    ///
    /// # Errors
    /// The file cannot be written.
    pub fn save_status(&self, s: &StoredStatus) -> Result<(), TlsError> {
        let path = self.dir.join("status.json");
        let json = serde_json::to_vec_pretty(s).map_err(|e| TlsError::Setup(e.to_string()))?;
        write_private(&path, &json).map_err(|e| self.storage_err(&path, e))
    }
}

/// Reads `status.json` for `directory_url` under `data_dir` (no keyring
/// needed; `forgebase doctor`).
pub fn read_status(data_dir: &Path, directory_url: &str) -> Option<StoredStatus> {
    let path = storage_dir(data_dir, directory_url).join("status.json");
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

#[derive(Debug, Default)]
struct RenewState {
    next_attempt: i64,
    failures: u32,
    last_renewal: Option<i64>,
    last_error: Option<(i64, String)>,
}

/// Keeps the certificate of `domains` fresh.
#[derive(Debug)]
pub struct Renewer {
    ca: Arc<dyn CertificateAuthority>,
    store: Arc<CertStore>,
    storage: AcmeStorage,
    clock: Arc<dyn Clock>,
    domains: Vec<String>,
    policy: RenewalPolicy,
    status: Arc<TlsStatus>,
    state: Mutex<RenewState>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Renewer {
    /// A renewer; call [`Renewer::load_stored`] before the first tick.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        ca: Arc<dyn CertificateAuthority>,
        store: Arc<CertStore>,
        storage: AcmeStorage,
        clock: Arc<dyn Clock>,
        domains: Vec<String>,
        policy: RenewalPolicy,
        status: Arc<TlsStatus>,
    ) -> Self {
        Self {
            ca,
            store,
            storage,
            clock,
            domains,
            policy,
            status,
            state: Mutex::new(RenewState::default()),
        }
    }

    /// The configured domains.
    pub fn domains(&self) -> &[String] {
        &self.domains
    }

    /// The storage directory.
    pub fn storage(&self) -> &AcmeStorage {
        &self.storage
    }

    /// Unix time of the next attempt.
    pub fn next_attempt(&self) -> i64 {
        lock(&self.state).next_attempt
    }

    /// Consecutive failures so far.
    pub fn failures(&self) -> u32 {
        lock(&self.state).failures
    }

    fn jitter(&self) -> f64 {
        if self.policy.jitter { random_fraction() } else { 0.0 }
    }

    /// Serves the stored certificate, if there is a usable one, and schedules
    /// the first attempt: at its renewal time, or now when there is none, it
    /// is for other domains, or it has expired. A stored certificate that
    /// cannot be unsealed or read is logged and replaced.
    ///
    /// Returns whether a stored certificate is being served.
    pub fn load_stored(&self) -> bool {
        let now = self.clock.now();
        let stored = match self.storage.load_certificate() {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "the stored ACME certificate cannot be read; requesting a new one");
                None
            }
        };
        let mut next = now;
        let mut serving = false;
        if let Some(s) = stored {
            match certs::from_pem(
                s.chain_pem.as_bytes(),
                s.key_pem.as_bytes(),
                "the stored ACME certificate",
                "its stored key",
            ) {
                Ok(cert) if cert.info.not_after > now => {
                    let info = cert.info.clone();
                    self.store.install(cert);
                    serving = true;
                    let mut st = lock(&self.state);
                    st.last_renewal = Some(s.issued_at);
                    if s.domains == self.domains {
                        next = renewal_time(&info, self.jitter()).max(now);
                    } else {
                        tracing::info!(stored = ?s.domains, configured = ?self.domains,
                            "the configured domains changed; requesting a new certificate (the old one is served meanwhile)");
                    }
                }
                Ok(_) => tracing::info!("the stored ACME certificate has expired; requesting a new one"),
                Err(e) => tracing::warn!(error = %e, "the stored ACME certificate does not load; requesting a new one"),
            }
        }
        lock(&self.state).next_attempt = next;
        let snapshot = self.publish();
        if let Err(e) = self.storage.save_status(&snapshot) {
            tracing::warn!(error = %e, "cannot write the ACME status file");
        }
        serving
    }

    /// Does what is due now and returns how long to wait before the next call.
    pub async fn tick(&self) -> Duration {
        let now = self.clock.now();
        let due = self.next_attempt();
        if now < due {
            super::set_expiry_gauge(&self.store, now);
            return self.wait(now, due);
        }
        tracing::info!(domains = ?self.domains, "requesting a TLS certificate over ACME");
        let result = match self.ca.issue(&self.domains, &self.store).await {
            Ok(issued) => certs::from_pem(
                issued.chain_pem.as_bytes(),
                issued.key_pem.as_bytes(),
                "the certificate the CA issued",
                "its key",
            )
            .map(|cert| (cert, issued)),
            Err(e) => Err(e),
        };
        let now = self.clock.now();
        let next = match result {
            Ok((cert, issued)) => {
                let info = cert.info.clone();
                self.store.install(cert);
                let stored = StoredCertificate {
                    domains: self.domains.clone(),
                    chain_pem: issued.chain_pem,
                    key_pem: issued.key_pem,
                    issued_at: now,
                };
                let storage = self.storage.clone();
                let saved = tokio::task::spawn_blocking(move || storage.save_certificate(&stored)).await;
                match saved {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        tracing::error!(error = %e, "the new certificate is served but could not be stored; it is requested again after a restart")
                    }
                    Err(e) => tracing::error!(error = %e, "storing the certificate failed"),
                }
                metrics::counter!(names::TLS_RENEWALS_TOTAL).increment(1);
                let next = renewal_time(&info, self.jitter()).max(now + 60);
                {
                    let mut st = lock(&self.state);
                    st.failures = 0;
                    st.last_error = None;
                    st.last_renewal = Some(now);
                    st.next_attempt = next;
                }
                tracing::info!(
                    issuer = %info.issuer,
                    not_after = %x509::rfc3339(info.not_after),
                    next_renewal = %x509::rfc3339(next),
                    "TLS certificate issued"
                );
                next
            }
            Err(e) => {
                metrics::counter!(names::TLS_RENEWAL_FAILURES_TOTAL).increment(1);
                let mut st = lock(&self.state);
                st.failures += 1;
                let wait = self.policy.backoff(st.failures);
                let next = now + i64::try_from(wait.as_secs()).unwrap_or(i64::MAX / 2);
                st.next_attempt = next;
                st.last_error = Some((now, e.to_string()));
                tracing::warn!(
                    error = %e,
                    failures = st.failures,
                    retry_at = %x509::rfc3339(next),
                    serving_old = self.store.info().is_some(),
                    "TLS certificate request failed; the current certificate (if any) is kept"
                );
                next
            }
        };
        let snapshot = self.publish();
        let storage = self.storage.clone();
        match tokio::task::spawn_blocking(move || storage.save_status(&snapshot)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!(error = %e, "cannot write the ACME status file"),
            Err(e) => tracing::warn!(error = %e, "cannot write the ACME status file"),
        }
        super::set_expiry_gauge(&self.store, now);
        self.wait(now, next)
    }

    fn wait(&self, now: i64, due: i64) -> Duration {
        let secs = u64::try_from(due.saturating_sub(now)).unwrap_or(0).max(1);
        Duration::from_secs(secs).min(self.policy.max_sleep)
    }

    /// Copies the state into the shared status; returns what `status.json` holds.
    fn publish(&self) -> StoredStatus {
        let (next, failures, last_renewal, last_error) = {
            let st = lock(&self.state);
            (st.next_attempt, st.failures, st.last_renewal, st.last_error.clone())
        };
        self.status.renewal(next, failures, last_renewal, last_error.clone());
        let info = self.store.info();
        StoredStatus {
            directory_url: self.storage.directory_url.clone(),
            domains: self.domains.clone(),
            issuer: info.as_ref().map(|i| i.issuer.clone()),
            not_before: info.as_ref().map(|i| i.not_before),
            not_after: info.as_ref().map(|i| i.not_after),
            last_renewal,
            next_renewal: Some(next),
            last_error_at: last_error.as_ref().map(|e| e.0),
            last_error: last_error.map(|e| e.1),
        }
    }
}

/// Runs `renewer` until `stop` turns true.
pub fn spawn_renewer(renewer: Arc<Renewer>, mut stop: watch::Receiver<bool>) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let wait = tokio::select! {
                biased;
                _ = stop.changed() => break,
                d = renewer.tick() => d,
            };
            tokio::select! {
                biased;
                _ = stop.changed() => break,
                () = tokio::time::sleep(wait) => {}
            }
        }
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn renewal_is_at_two_thirds_minus_up_to_a_fifteenth() {
        let info = CertInfo {
            serial: String::new(),
            issuer: String::new(),
            subject: String::new(),
            not_before: 0,
            not_after: 90 * 86_400,
            names: vec![],
        };
        assert_eq!(renewal_time(&info, 0.0), 60 * 86_400);
        assert_eq!(renewal_time(&info, 1.0), 54 * 86_400);
        let mid = renewal_time(&info, 0.5);
        assert!(mid > 54 * 86_400 && mid < 60 * 86_400);
    }

    #[test]
    fn backoff_doubles_up_to_the_cap() {
        let p = RenewalPolicy::default();
        assert_eq!(p.backoff(1), Duration::from_secs(120));
        assert_eq!(p.backoff(2), Duration::from_secs(240));
        assert_eq!(p.backoff(5), Duration::from_secs(1920));
        assert_eq!(p.backoff(20), Duration::from_secs(6 * 3600));
        assert_eq!(p.backoff(u32::MAX), Duration::from_secs(6 * 3600));
    }

    #[test]
    fn storage_dirs_differ_per_directory() {
        let a = storage_dir(Path::new("/d"), super::super::LETS_ENCRYPT_PRODUCTION);
        let b = storage_dir(Path::new("/d"), super::super::LETS_ENCRYPT_STAGING);
        assert_ne!(a, b);
        assert!(a.starts_with("/d/tls/acme"));
    }
}
