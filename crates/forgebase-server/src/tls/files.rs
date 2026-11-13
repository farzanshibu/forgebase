//! `[tls] mode = "files"`: the certificate chain and key from PEM files,
//! loaded again when their contents change (checked every
//! `reload_interval_secs`) or when the process receives `SIGHUP` (Unix).
//!
//! The contents are compared, not the modification times, so an atomic
//! replace, a Kubernetes secret's symlink swap and a copy within the same
//! second are all noticed. A pair that does not load (a key written before
//! its certificate, a mismatch, a typo) is logged and counted; the server
//! keeps serving the previous certificate and tries again when either file
//! changes. Open connections are never touched: the next handshake uses the
//! new certificate.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use forgebase_observability::names;
use ring::digest::{SHA256, digest};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use super::certs::{self, CertStore};
use super::{Clock, TlsError, TlsStatus};

/// Result of one check of the files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReloadOutcome {
    /// Same contents as the certificate being served.
    Unchanged,
    /// The new pair is being served.
    Reloaded,
    /// The new contents do not load; the old certificate stays.
    Failed(String),
    /// The same contents failed before; not retried until they change.
    StillFailing,
}

/// The two files and what was last loaded from them.
#[derive(Debug)]
pub struct FileSource {
    cert: PathBuf,
    key: PathBuf,
    /// SHA-256 over both files of the pair being served.
    served: Mutex<Option<Vec<u8>>>,
    /// SHA-256 of contents that failed to load.
    failed: Mutex<Option<Vec<u8>>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn fingerprint(cert: &[u8], key: &[u8]) -> Vec<u8> {
    let mut both = Vec::with_capacity(cert.len() + key.len() + 16);
    both.extend_from_slice(&(cert.len() as u64).to_be_bytes());
    both.extend_from_slice(cert);
    both.extend_from_slice(key);
    digest(&SHA256, &both).as_ref().to_vec()
}

impl FileSource {
    /// The pair at `cert` and `key`.
    pub fn new(cert: impl Into<PathBuf>, key: impl Into<PathBuf>) -> Self {
        Self {
            cert: cert.into(),
            key: key.into(),
            served: Mutex::new(None),
            failed: Mutex::new(None),
        }
    }

    /// The certificate file.
    pub fn cert_path(&self) -> &std::path::Path {
        &self.cert
    }

    /// The key file.
    pub fn key_path(&self) -> &std::path::Path {
        &self.key
    }

    /// Loads the pair for the first time (at start: an error refuses to start).
    ///
    /// # Errors
    /// A file cannot be read, a mismatched key, a bad certificate.
    pub fn load_initial(&self, store: &CertStore) -> Result<(), TlsError> {
        let read = |p: &std::path::Path| {
            std::fs::read(p).map_err(|source| TlsError::Read {
                path: p.to_path_buf(),
                source,
            })
        };
        let (c, k) = (read(&self.cert)?, read(&self.key)?);
        let cert = certs::from_pem(
            &c,
            &k,
            &self.cert.display().to_string(),
            &self.key.display().to_string(),
        )?;
        store.install(cert);
        *lock(&self.served) = Some(fingerprint(&c, &k));
        Ok(())
    }

    /// Reads both files and serves them if they changed and load.
    pub async fn check(&self, store: &CertStore) -> ReloadOutcome {
        let (c, k) = match (tokio::fs::read(&self.cert).await, tokio::fs::read(&self.key).await) {
            (Ok(c), Ok(k)) => (c, k),
            (Err(e), _) => return self.fail(None, format!("cannot read {}: {e}", self.cert.display())),
            (_, Err(e)) => return self.fail(None, format!("cannot read {}: {e}", self.key.display())),
        };
        let fp = fingerprint(&c, &k);
        if lock(&self.served).as_deref() == Some(fp.as_slice()) {
            *lock(&self.failed) = None;
            return ReloadOutcome::Unchanged;
        }
        if lock(&self.failed).as_deref() == Some(fp.as_slice()) {
            return ReloadOutcome::StillFailing;
        }
        match certs::from_pem(
            &c,
            &k,
            &self.cert.display().to_string(),
            &self.key.display().to_string(),
        ) {
            Ok(cert) => {
                store.install(cert);
                *lock(&self.served) = Some(fp);
                *lock(&self.failed) = None;
                ReloadOutcome::Reloaded
            }
            Err(e) => self.fail(Some(fp), e.to_string()),
        }
    }

    fn fail(&self, fp: Option<Vec<u8>>, message: String) -> ReloadOutcome {
        let mut failed = lock(&self.failed);
        // An unreadable file has no fingerprint: report it once until it reads again.
        let key = fp.unwrap_or_default();
        if failed.as_deref() == Some(key.as_slice()) {
            return ReloadOutcome::StillFailing;
        }
        *failed = Some(key);
        ReloadOutcome::Failed(message)
    }
}

/// Runs one check and records its outcome (log, metrics, status).
pub async fn check_and_record(source: &FileSource, store: &CertStore, status: &TlsStatus, clock: &dyn Clock) {
    match source.check(store).await {
        ReloadOutcome::Reloaded => {
            metrics::counter!(names::TLS_RELOADS_TOTAL).increment(1);
            status.reloaded(clock.now());
            let info = store.info();
            tracing::info!(
                cert = %source.cert.display(),
                not_after = info.as_ref().map(|i| super::x509::rfc3339(i.not_after)),
                "TLS certificate reloaded"
            );
            super::set_expiry_gauge(store, clock.now());
        }
        ReloadOutcome::Failed(message) => {
            metrics::counter!(names::TLS_RELOAD_FAILURES_TOTAL).increment(1);
            tracing::warn!(error = %message, "TLS certificate files changed but do not load; still serving the previous certificate");
            status.failed(clock.now(), message);
        }
        ReloadOutcome::Unchanged | ReloadOutcome::StillFailing => {}
    }
}

/// Checks the files every `interval` (never when zero) and on `SIGHUP`
/// until `stop` turns true.
pub fn spawn_watcher(
    source: Arc<FileSource>,
    store: Arc<CertStore>,
    status: Arc<TlsStatus>,
    clock: Arc<dyn Clock>,
    interval: Duration,
    mut stop: watch::Receiver<bool>,
) -> JoinHandle<()> {
    // Registered before the task runs: a SIGHUP right after start is a
    // reload, never the default action (terminate).
    #[cfg(unix)]
    let mut hup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()).ok();
    tokio::spawn(async move {
        let mut tick = (!interval.is_zero()).then(|| {
            let mut t = tokio::time::interval_at(tokio::time::Instant::now() + interval, interval);
            t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            t
        });
        loop {
            let ticked = async {
                match tick.as_mut() {
                    Some(t) => {
                        t.tick().await;
                    }
                    None => std::future::pending::<()>().await,
                }
            };
            #[cfg(unix)]
            let hupped = async {
                match hup.as_mut() {
                    Some(h) => {
                        h.recv().await;
                    }
                    None => std::future::pending::<()>().await,
                }
            };
            #[cfg(not(unix))]
            let hupped = std::future::pending::<()>();
            tokio::select! {
                biased;
                _ = stop.changed() => break,
                () = hupped => {
                    tracing::info!("SIGHUP: checking the TLS certificate files");
                    check_and_record(&source, &store, &status, clock.as_ref()).await;
                }
                () = ticked => check_and_record(&source, &store, &status, clock.as_ref()).await,
            }
        }
    })
}
