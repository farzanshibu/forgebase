//! Native TLS (ADR-0033): HTTPS on the HTTP listener, TLS on the RESP
//! listener, certificates from files or from an ACME certificate authority.
//!
//! Off by default (`[tls] mode = "off"`): the server speaks plain HTTP and a
//! reverse proxy terminates TLS, which stays fully supported.
//!
//! * [`TlsSetup::prepare`] runs before the database opens, so a missing
//!   file or a key that does not match its certificate refuses to start with
//!   a clear message and nothing else is touched.
//! * One [`CertStore`] is the certificate resolver of every TLS listener:
//!   a reload (files) or a renewal (ACME) swaps the certificate for the next
//!   handshake on HTTPS and RESP alike; open connections are not dropped.
//! * [`files`]: content check every `reload_interval_secs` and on `SIGHUP`.
//! * [`acme`]: renewal at about two thirds of the lifetime with jitter,
//!   exponential backoff, the old certificate kept until a new one is issued,
//!   account and certificate sealed with the data directory's keyring.
//! * [`serve`]: the HTTPS accept loop (ALPN `h2` and `http/1.1`, plus
//!   `acme-tls/1` for `tls-alpn-01`), the plain HTTP redirect and `http-01`
//!   listener, and HSTS.
//!
//! Status: [`TlsSetup::status_json`] (shown in `GET /api/system/info` under
//! `tls`); metrics `forgebase_tls_certificate_expiry_seconds`,
//! `forgebase_tls_renewals_total`, `forgebase_tls_renewal_failures_total`,
//! `forgebase_tls_reloads_total`, `forgebase_tls_reload_failures_total`,
//! `forgebase_tls_handshake_failures_total`; [`doctor_check`] for
//! `forgebase doctor`.

pub mod acme;
mod acme_client;
pub mod certs;
pub mod files;
pub mod serve;
pub mod x509;

use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use forgebase_core::config::tls_config::{AcmeChallenge, TlsConfig, TlsMode};
pub use forgebase_core::config::tls_config::{LETS_ENCRYPT_PRODUCTION, LETS_ENCRYPT_STAGING};
use forgebase_core::{Config, DataLayout, ErrorCode, ErrorKind};
use forgebase_observability::names;
use rustls::ServerConfig;
use serde_json::{Value, json};
use tokio::sync::watch;
use tokio::task::JoinHandle;

pub use acme::{CertificateAuthority, IssuedCertificate, RenewalPolicy, Renewer};
pub use acme_client::AcmeCa;
pub use certs::{CertStore, Certificate};

/// TLS errors. Each names the file or setting at fault.
#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    /// A certificate or key file cannot be read.
    #[error("cannot read {path}: {cause}")]
    Read {
        /// The file.
        path: PathBuf,
        /// Why (in the message, not a separate source, so it prints once).
        cause: std::io::Error,
    },
    /// Not a usable certificate chain.
    #[error("{what}: {message}")]
    BadCertificate {
        /// The source (a file path).
        what: String,
        /// What is wrong.
        message: String,
    },
    /// Not a usable private key.
    #[error("{what}: {message}")]
    BadKey {
        /// The source (a file path).
        what: String,
        /// What is wrong.
        message: String,
    },
    /// The private key does not belong to the certificate.
    #[error(
        "the private key in {key} does not belong to the certificate in {cert}: check that tls.key_file is the key the certificate was issued for"
    )]
    KeyMismatch {
        /// Certificate source.
        cert: String,
        /// Key source.
        key: String,
    },
    /// The TLS stack cannot be set up (protocol versions, CA file, key generation).
    #[error("TLS setup: {0}")]
    Setup(String),
    /// The ACME state in the data directory cannot be read or written.
    #[error("ACME storage {path}: {message}")]
    Storage {
        /// The file.
        path: PathBuf,
        /// What failed.
        message: String,
    },
    /// The ACME certificate authority refused or failed a request.
    #[error("ACME: {0}")]
    Acme(String),
}

impl ErrorCode for TlsError {
    fn code(&self) -> &'static str {
        match self {
            TlsError::Read { .. } => "tls_file_unreadable",
            TlsError::BadCertificate { .. } => "tls_certificate_invalid",
            TlsError::BadKey { .. } => "tls_key_invalid",
            TlsError::KeyMismatch { .. } => "tls_key_mismatch",
            TlsError::Setup(_) => "tls_setup_failed",
            TlsError::Storage { .. } => "tls_storage_failed",
            TlsError::Acme(_) => "tls_acme_failed",
        }
    }

    fn kind(&self) -> ErrorKind {
        match self {
            TlsError::Read { .. }
            | TlsError::BadCertificate { .. }
            | TlsError::BadKey { .. }
            | TlsError::KeyMismatch { .. } => ErrorKind::InvalidInput,
            TlsError::Acme(_) => ErrorKind::Unavailable,
            TlsError::Setup(_) | TlsError::Storage { .. } => ErrorKind::Internal,
        }
    }
}

/// Wall-clock time in Unix seconds, replaceable in tests.
pub trait Clock: Send + Sync + std::fmt::Debug {
    /// Now.
    fn now(&self) -> i64;
}

/// The system clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
    }
}

/// A clock that moves only when told to.
#[derive(Debug, Default)]
pub struct ManualClock(AtomicI64);

impl ManualClock {
    /// A clock at `now`.
    pub fn new(now: i64) -> Self {
        Self(AtomicI64::new(now))
    }

    /// Sets the time.
    pub fn set(&self, now: i64) {
        self.0.store(now, Ordering::SeqCst);
    }

    /// Moves the time forward by `secs`.
    pub fn advance(&self, secs: i64) {
        self.0.fetch_add(secs, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// Sets `forgebase_tls_certificate_expiry_seconds` from the served certificate.
pub fn set_expiry_gauge(store: &CertStore, now: i64) {
    if let Some(info) = store.info() {
        metrics::gauge!(names::TLS_CERTIFICATE_EXPIRY_SECONDS).set((info.not_after - now) as f64);
    }
}

#[derive(Debug, Default, Clone)]
struct StatusInner {
    last_reload: Option<i64>,
    last_renewal: Option<i64>,
    next_renewal: Option<i64>,
    failures: u32,
    last_error: Option<(i64, String)>,
    http_listener: Option<String>,
    resp: bool,
}

/// What the background tasks report for the status endpoint.
#[derive(Debug, Default)]
pub struct TlsStatus(Mutex<StatusInner>);

impl TlsStatus {
    fn with<R>(&self, f: impl FnOnce(&mut StatusInner) -> R) -> R {
        f(&mut self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner))
    }

    /// Files mode: a reload succeeded at `now`.
    pub fn reloaded(&self, now: i64) {
        self.with(|s| {
            s.last_reload = Some(now);
            s.last_error = None;
        });
    }

    /// A reload or renewal failed at `now`.
    pub fn failed(&self, now: i64, message: String) {
        self.with(|s| s.last_error = Some((now, message)));
    }

    /// ACME mode: the scheduler's state.
    pub fn renewal(&self, next: i64, failures: u32, last_renewal: Option<i64>, last_error: Option<(i64, String)>) {
        self.with(|s| {
            s.next_renewal = Some(next);
            s.failures = failures;
            s.last_renewal = last_renewal;
            s.last_error = last_error;
        });
    }

    /// The plain HTTP listener's address, once bound.
    pub fn set_http_listener(&self, addr: Option<String>) {
        self.with(|s| s.http_listener = addr);
    }

    /// Whether RESP serves TLS.
    pub fn set_resp(&self, on: bool) {
        self.with(|s| s.resp = on);
    }
}

enum Source {
    Files(Arc<files::FileSource>),
    Acme(Arc<Renewer>),
}

/// TLS prepared for one server start.
pub struct TlsSetup {
    config: TlsConfig,
    store: Arc<CertStore>,
    status: Arc<TlsStatus>,
    clock: Arc<dyn Clock>,
    source: Source,
}

impl std::fmt::Debug for TlsSetup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TlsSetup")
            .field("mode", &self.config.mode)
            .field("store", &self.store)
            .finish_non_exhaustive()
    }
}

/// ALPN protocols of HTTP/2 and HTTP/1.1, in server preference order.
pub const HTTP_ALPN: &[&[u8]] = &[b"h2", b"http/1.1"];

/// Background tasks of a running TLS setup.
#[derive(Debug)]
pub struct TlsTasks {
    stop: watch::Sender<bool>,
    tasks: Vec<JoinHandle<()>>,
}

impl TlsTasks {
    /// Stops the watcher or the renewer (an issuance in progress is abandoned).
    pub async fn stop(self) {
        let _ = self.stop.send(true);
        for t in self.tasks {
            t.abort();
            let _ = t.await;
        }
    }
}

impl TlsSetup {
    /// Prepares TLS for `config`: `None` when `[tls] mode = "off"`. Files mode
    /// loads and checks the pair now; ACME mode loads the keyring and serves
    /// the stored certificate, if any (the first request runs in the
    /// background once the server is up).
    ///
    /// # Errors
    /// A file cannot be read, the key does not match the certificate, the
    /// keyring or the ACME settings cannot be used.
    pub fn prepare(config: &Config, layout: &DataLayout) -> Result<Option<Self>, TlsError> {
        Self::prepare_with(config, layout, Arc::new(SystemClock), RenewalPolicy::default())
    }

    /// [`TlsSetup::prepare`] with a clock and renewal policy.
    ///
    /// # Errors
    /// See [`TlsSetup::prepare`].
    pub fn prepare_with(
        config: &Config,
        layout: &DataLayout,
        clock: Arc<dyn Clock>,
        policy: RenewalPolicy,
    ) -> Result<Option<Self>, TlsError> {
        let tls = &config.tls;
        let store = Arc::new(CertStore::new());
        let status = Arc::new(TlsStatus::default());
        let source = match tls.mode {
            TlsMode::Off => return Ok(None),
            TlsMode::Files => {
                let src = Arc::new(files::FileSource::new(&tls.cert_file, &tls.key_file));
                src.load_initial(&store)?;
                status.reloaded(clock.now());
                Source::Files(src)
            }
            TlsMode::Acme => {
                let path = forgebase_auth::AuthConfig::for_data_dir(layout.root()).master_key_path;
                let keyring = forgebase_auth::crypto::Keyring::load_or_create(&path)
                    .map_err(|e| TlsError::Setup(format!("keyring: {e}")))?;
                let storage = acme::AcmeStorage::new(layout.root(), tls.acme.directory(), keyring);
                let ca = Arc::new(AcmeCa::new(&tls.acme, storage.clone(), clock.clone())?);
                Self::acme_source(config, ca, storage, &store, &status, clock.clone(), policy)
            }
        };
        let setup = Self {
            config: tls.clone(),
            store,
            status,
            clock,
            source,
        };
        if let Some(info) = setup.store.info() {
            let now = setup.clock.now();
            if info.not_after <= now {
                tracing::warn!(not_after = %x509::rfc3339(info.not_after), "the TLS certificate has expired; clients will refuse it");
            } else if info.not_after - now < 14 * 86_400 {
                tracing::warn!(not_after = %x509::rfc3339(info.not_after), "the TLS certificate expires in less than 14 days");
            }
            set_expiry_gauge(&setup.store, now);
        }
        Ok(Some(setup))
    }

    fn acme_source(
        config: &Config,
        ca: Arc<dyn CertificateAuthority>,
        storage: acme::AcmeStorage,
        store: &Arc<CertStore>,
        status: &Arc<TlsStatus>,
        clock: Arc<dyn Clock>,
        policy: RenewalPolicy,
    ) -> Source {
        let renewer = Arc::new(Renewer::new(
            ca,
            store.clone(),
            storage,
            clock,
            config.tls.acme.domains.clone(),
            policy,
            status.clone(),
        ));
        renewer.load_stored();
        Source::Acme(renewer)
    }

    /// ACME mode with a given certificate authority (tests: a fake CA).
    ///
    /// # Errors
    /// The keyring cannot be loaded.
    pub fn prepare_acme_with(
        config: &Config,
        layout: &DataLayout,
        ca: Arc<dyn CertificateAuthority>,
        clock: Arc<dyn Clock>,
        policy: RenewalPolicy,
    ) -> Result<Self, TlsError> {
        let path = forgebase_auth::AuthConfig::for_data_dir(layout.root()).master_key_path;
        let keyring = forgebase_auth::crypto::Keyring::load_or_create(&path)
            .map_err(|e| TlsError::Setup(format!("keyring: {e}")))?;
        let storage = acme::AcmeStorage::new(layout.root(), config.tls.acme.directory(), keyring);
        let store = Arc::new(CertStore::new());
        let status = Arc::new(TlsStatus::default());
        let source = Self::acme_source(config, ca, storage, &store, &status, clock.clone(), policy);
        Ok(Self {
            config: config.tls.clone(),
            store,
            status,
            clock,
            source,
        })
    }

    /// The `[tls]` settings.
    pub fn config(&self) -> &TlsConfig {
        &self.config
    }

    /// The certificate store all listeners resolve from.
    pub fn store(&self) -> &Arc<CertStore> {
        &self.store
    }

    /// The shared status.
    pub fn status(&self) -> &Arc<TlsStatus> {
        &self.status
    }

    /// The ACME scheduler, in acme mode.
    pub fn renewer(&self) -> Option<&Arc<Renewer>> {
        match &self.source {
            Source::Acme(r) => Some(r),
            Source::Files(_) => None,
        }
    }

    fn alpn_challenge(&self) -> bool {
        self.config.mode == TlsMode::Acme && self.config.acme.uses(AcmeChallenge::TlsAlpn01)
    }

    /// The rustls configuration of the HTTPS listener.
    ///
    /// # Errors
    /// The protocol versions are not supported.
    pub fn http_server_config(&self) -> Result<Arc<ServerConfig>, TlsError> {
        let mut alpn: Vec<&[u8]> = HTTP_ALPN.to_vec();
        if self.alpn_challenge() {
            alpn.push(certs::ACME_TLS_ALPN);
        }
        certs::server_config(self.store.clone(), self.config.min_version, &alpn)
    }

    /// The rustls configuration of the RESP listener (no ALPN).
    ///
    /// # Errors
    /// The protocol versions are not supported.
    pub fn resp_server_config(&self) -> Result<Arc<ServerConfig>, TlsError> {
        certs::server_config(self.store.clone(), self.config.min_version, &[])
    }

    /// Starts the file watcher or the renewal scheduler.
    pub fn start_background(&self) -> TlsTasks {
        let (stop, rx) = watch::channel(false);
        let task = match &self.source {
            Source::Files(src) => files::spawn_watcher(
                src.clone(),
                self.store.clone(),
                self.status.clone(),
                self.clock.clone(),
                Duration::from_secs(self.config.reload_interval_secs),
                rx,
            ),
            Source::Acme(r) => acme::spawn_renewer(r.clone(), rx),
        };
        TlsTasks {
            stop,
            tasks: vec![task],
        }
    }

    /// Refreshes the expiry gauge (before a `/metrics` scrape).
    pub fn refresh_gauge(&self) {
        set_expiry_gauge(&self.store, self.clock.now());
    }

    /// The `tls` entry of `GET /api/system/info`.
    pub fn status_json(&self) -> Value {
        let now = self.clock.now();
        let s = self.status.with(|s| s.clone());
        let t = |v: Option<i64>| v.map(x509::rfc3339);
        let cert = self.store.info().map(|i| {
            json!({
                "subject": i.subject,
                "names": i.names,
                "issuer": i.issuer,
                "serial": i.serial,
                "not_before": x509::rfc3339(i.not_before),
                "not_after": x509::rfc3339(i.not_after),
                "expires_in_secs": i.not_after - now,
            })
        });
        let mut alpn: Vec<&str> = vec!["h2", "http/1.1"];
        if self.alpn_challenge() {
            alpn.push("acme-tls/1");
        }
        let mut out = json!({
            "mode": self.config.mode.as_str(),
            "min_version": self.config.min_version.as_str(),
            "alpn": alpn,
            "hsts_max_age_secs": self.config.hsts_max_age_secs,
            "http_listener": s.http_listener,
            "resp": s.resp,
            "certificate": cert,
            "last_error": s.last_error.as_ref().map(|e| e.1.clone()),
            "last_error_at": t(s.last_error.as_ref().map(|e| e.0)),
        });
        match &self.source {
            Source::Files(src) => {
                out["cert_file"] = json!(src.cert_path().display().to_string());
                out["key_file"] = json!(src.key_path().display().to_string());
                out["reload_interval_secs"] = json!(self.config.reload_interval_secs);
                out["last_reload"] = json!(t(s.last_reload));
            }
            Source::Acme(r) => {
                out["domains"] = json!(r.domains());
                out["directory_url"] = json!(self.config.acme.directory());
                out["challenges"] = json!(
                    self.config
                        .acme
                        .challenges
                        .iter()
                        .map(|c| c.as_str())
                        .collect::<Vec<_>>()
                );
                out["last_renewal"] = json!(t(s.last_renewal));
                out["next_renewal"] = json!(t(s.next_renewal));
                out["renewal_failures"] = json!(s.failures);
            }
        }
        out
    }
}

/// Severity of a `forgebase doctor` finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoctorLevel {
    /// Fine.
    Ok,
    /// Needs attention soon.
    Warn,
    /// Broken now.
    Fail,
}

/// Days of validity left below which `forgebase doctor` warns.
pub const DOCTOR_WARN_DAYS: i64 = 14;

/// The `tls` check of `forgebase doctor` at Unix time `now`: `None` when TLS
/// is off. Read-only: files mode reads the certificate files, ACME mode the
/// `status.json` the server writes (no keyring needed).
pub fn doctor_check(config: &Config, now: i64) -> Option<(DoctorLevel, String)> {
    let tls = &config.tls;
    let (not_after, what) = match tls.mode {
        TlsMode::Off => return None,
        TlsMode::Files => match certs::from_files(
            std::path::Path::new(&tls.cert_file),
            std::path::Path::new(&tls.key_file),
        ) {
            Ok(c) => (
                c.info.not_after,
                format!("{} (issuer {})", tls.cert_file, c.info.issuer),
            ),
            Err(e) => return Some((DoctorLevel::Fail, format!("files mode: {e}"))),
        },
        TlsMode::Acme => {
            let dir = tls.acme.directory();
            let Some(s) = acme::read_status(&config.data_dir, dir) else {
                return Some((
                    DoctorLevel::Warn,
                    format!(
                        "acme mode: no certificate issued yet for {} (the server requests one when it starts)",
                        tls.acme.domains.join(", ")
                    ),
                ));
            };
            let Some(not_after) = s.not_after else {
                return Some((
                    DoctorLevel::Warn,
                    format!(
                        "acme mode: no certificate issued yet{}",
                        s.last_error.map(|e| format!("; last error: {e}")).unwrap_or_default()
                    ),
                ));
            };
            let mut what = format!(
                "acme certificate for {} (issuer {})",
                s.domains.join(", "),
                s.issuer.unwrap_or_default()
            );
            if let Some(e) = s.last_error {
                what.push_str(&format!("; last renewal error: {e}"));
            }
            (not_after, what)
        }
    };
    let left = not_after - now;
    let date = x509::rfc3339(not_after);
    Some(if left <= 0 {
        (DoctorLevel::Fail, format!("{what} expired on {date}"))
    } else if left < DOCTOR_WARN_DAYS * 86_400 {
        (
            DoctorLevel::Warn,
            format!("{what} expires in {} day(s), on {date}", left / 86_400),
        )
    } else {
        (
            DoctorLevel::Ok,
            format!("{what} valid until {date} ({} days)", left / 86_400),
        )
    })
}
