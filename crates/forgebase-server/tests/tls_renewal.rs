//! The ACME renewal scheduler with a fake clock and a fake certificate
//! authority (ADR-0033 §3): first issuance, renewal at two thirds of the
//! lifetime (with and without jitter), exponential backoff on failure while
//! the old certificate keeps being served, sealed storage that survives a
//! restart, and a new request when the configured domains change.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use forgebase_core::config::tls_config::TlsMode;
use forgebase_core::{Config, DataLayout};
use forgebase_server::tls::acme::{self, BoxFuture};
use forgebase_server::tls::{
    CertStore, CertificateAuthority, Clock, IssuedCertificate, ManualClock, RenewalPolicy, TlsError, TlsSetup, x509,
};

const DAY: i64 = 86_400;
const T0: i64 = 1_794_614_400; // 2026-11-14T00:00:00Z

#[derive(Debug)]
struct FakeCa {
    clock: Arc<ManualClock>,
    fail: AtomicBool,
    issued: AtomicUsize,
    attempts: AtomicUsize,
}

impl FakeCa {
    fn new(clock: Arc<ManualClock>) -> Arc<Self> {
        Arc::new(Self {
            clock,
            fail: AtomicBool::new(false),
            issued: AtomicUsize::new(0),
            attempts: AtomicUsize::new(0),
        })
    }
}

impl CertificateAuthority for FakeCa {
    fn issue<'a>(
        &'a self,
        domains: &'a [String],
        _challenges: &'a CertStore,
    ) -> BoxFuture<'a, Result<IssuedCertificate, TlsError>> {
        Box::pin(async move {
            self.attempts.fetch_add(1, Ordering::SeqCst);
            if self.fail.load(Ordering::SeqCst) {
                return Err(TlsError::Acme(
                    "urn:ietf:params:acme:error:rateLimited: too many certificates".into(),
                ));
            }
            let now = self.clock.now();
            let names: Vec<&str> = domains.iter().map(String::as_str).collect();
            let (cert, key) = x509::self_signed(&names, now, now + 90 * DAY, &[]).unwrap();
            self.issued.fetch_add(1, Ordering::SeqCst);
            Ok(IssuedCertificate {
                chain_pem: x509::pem("CERTIFICATE", &cert),
                key_pem: x509::pem("PRIVATE KEY", &key),
            })
        })
    }
}

fn config(dir: &std::path::Path, domains: &[&str]) -> Config {
    let mut cfg = Config::defaults(dir);
    cfg.tls.mode = TlsMode::Acme;
    cfg.tls.acme.domains = domains.iter().map(|d| (*d).to_owned()).collect();
    cfg.tls.acme.directory_url = "https://ca.test/directory".into();
    cfg
}

fn policy(jitter: bool) -> RenewalPolicy {
    RenewalPolicy {
        jitter,
        ..RenewalPolicy::default()
    }
}

fn setup(cfg: &Config, ca: &Arc<FakeCa>, clock: &Arc<ManualClock>, jitter: bool) -> TlsSetup {
    let layout = DataLayout::new(&cfg.data_dir);
    TlsSetup::prepare_acme_with(cfg, &layout, ca.clone(), clock.clone(), policy(jitter)).unwrap()
}

fn serial(s: &TlsSetup) -> Option<String> {
    s.store().info().map(|i| i.serial)
}

#[tokio::test]
async fn renews_at_two_thirds_backs_off_on_failure_and_keeps_the_old_certificate() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config(tmp.path(), &["app.example.com", "www.example.com"]);
    let clock = Arc::new(ManualClock::new(T0));
    let ca = FakeCa::new(clock.clone());
    let s = setup(&cfg, &ca, &clock, false);
    let r = s.renewer().unwrap().clone();

    // Nothing stored: the first tick requests a certificate at once.
    assert!(serial(&s).is_none());
    assert_eq!(r.next_attempt(), T0);
    let wait = r.tick().await;
    assert_eq!(ca.issued.load(Ordering::SeqCst), 1);
    let first = serial(&s).expect("served after the first tick");
    assert_eq!(s.store().info().unwrap().names, ["app.example.com", "www.example.com"]);
    // Without jitter: exactly two thirds of 90 days.
    assert_eq!(r.next_attempt(), T0 + 60 * DAY);
    assert_eq!(wait, Duration::from_secs(3600), "sleeps at most an hour at a time");
    let status = s.status_json();
    assert_eq!(status["renewal_failures"], 0);
    assert_eq!(status["next_renewal"], x509::rfc3339(T0 + 60 * DAY));
    assert_eq!(
        status["domains"],
        serde_json::json!(["app.example.com", "www.example.com"])
    );

    // Day 59: not due, nothing requested.
    clock.set(T0 + 59 * DAY);
    r.tick().await;
    assert_eq!(ca.attempts.load(Ordering::SeqCst), 1);

    // Day 60: due, but the CA fails. The old certificate stays; retry after
    // 2 minutes, then 4, then 8.
    ca.fail.store(true, Ordering::SeqCst);
    clock.set(T0 + 60 * DAY);
    let wait = r.tick().await;
    assert_eq!(ca.attempts.load(Ordering::SeqCst), 2);
    assert_eq!(serial(&s).as_deref(), Some(first.as_str()), "old certificate kept");
    assert_eq!(r.failures(), 1);
    assert_eq!(r.next_attempt(), T0 + 60 * DAY + 120);
    assert_eq!(wait, Duration::from_secs(120));
    let status = s.status_json();
    assert_eq!(status["renewal_failures"], 1);
    assert!(
        status["last_error"].as_str().unwrap().contains("rateLimited"),
        "{status}"
    );

    clock.advance(119);
    r.tick().await;
    assert_eq!(ca.attempts.load(Ordering::SeqCst), 2, "not before the backoff");
    clock.advance(1);
    let wait = r.tick().await;
    assert_eq!(ca.attempts.load(Ordering::SeqCst), 3);
    assert_eq!(r.failures(), 2);
    assert_eq!(wait, Duration::from_secs(240));
    clock.advance(240);
    let wait = r.tick().await;
    assert_eq!(r.failures(), 3);
    assert_eq!(wait, Duration::from_secs(480));
    assert_eq!(serial(&s).as_deref(), Some(first.as_str()));

    // The CA recovers: a new certificate, failures reset, next renewal at
    // two thirds of the new one.
    ca.fail.store(false, Ordering::SeqCst);
    clock.advance(480);
    let renewed_at = clock.now();
    r.tick().await;
    assert_eq!(ca.issued.load(Ordering::SeqCst), 2);
    let second = serial(&s).unwrap();
    assert_ne!(second, first);
    assert_eq!(r.failures(), 0);
    assert_eq!(r.next_attempt(), renewed_at + 60 * DAY);
    let status = s.status_json();
    assert!(status["last_error"].is_null(), "{status}");
    assert_eq!(status["last_renewal"], x509::rfc3339(renewed_at));

    // The account and certificate are sealed on disk; status.json has no secrets.
    let dir = r.storage().dir().to_path_buf();
    let sealed = std::fs::read_to_string(dir.join("certificate.sealed")).unwrap();
    assert!(!sealed.contains("BEGIN"), "the certificate store is sealed");
    let doc = acme::read_status(&cfg.data_dir, "https://ca.test/directory").unwrap();
    assert_eq!(doc.domains, ["app.example.com", "www.example.com"]);
    assert_eq!(doc.not_after, Some(renewed_at + 90 * DAY));
    assert!(
        !std::fs::read_to_string(dir.join("status.json"))
            .unwrap()
            .contains("PRIVATE")
    );

    // A restart serves the stored certificate without asking the CA.
    let ca2 = FakeCa::new(clock.clone());
    let s2 = setup(&cfg, &ca2, &clock, false);
    assert_eq!(serial(&s2).as_deref(), Some(second.as_str()));
    let r2 = s2.renewer().unwrap();
    assert_eq!(r2.next_attempt(), renewed_at + 60 * DAY);
    r2.tick().await;
    assert_eq!(ca2.attempts.load(Ordering::SeqCst), 0);

    // Another master key cannot unseal it: the server requests a new one.
    let other = tempfile::tempdir().unwrap();
    let mut moved = config(other.path(), &["app.example.com", "www.example.com"]);
    moved.data_dir = other.path().join("data");
    std::fs::create_dir_all(moved.data_dir.join("tls")).unwrap();
    copy_dir(&cfg.data_dir.join("tls"), &moved.data_dir.join("tls"));
    let ca3 = FakeCa::new(clock.clone());
    let s3 = setup(&moved, &ca3, &clock, false);
    assert!(serial(&s3).is_none(), "sealed with another key: not served");
    assert_eq!(s3.renewer().unwrap().next_attempt(), clock.now());
}

fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let p = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &p);
        } else {
            std::fs::copy(e.path(), p).unwrap();
        }
    }
}

#[tokio::test]
async fn jitter_moves_renewal_into_the_last_fifteenth_before_two_thirds() {
    for _ in 0..5 {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = config(tmp.path(), &["app.example.com"]);
        let clock = Arc::new(ManualClock::new(T0));
        let ca = FakeCa::new(clock.clone());
        let s = setup(&cfg, &ca, &clock, true);
        let r = s.renewer().unwrap();
        r.tick().await;
        let next = r.next_attempt();
        assert!(
            (T0 + 54 * DAY..=T0 + 60 * DAY).contains(&next),
            "{} days",
            (next - T0) as f64 / DAY as f64
        );
    }
}

#[tokio::test]
async fn changed_domains_or_an_expired_certificate_are_requested_at_once() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new(T0));
    let ca = FakeCa::new(clock.clone());
    let cfg = config(tmp.path(), &["app.example.com"]);
    let s = setup(&cfg, &ca, &clock, false);
    s.renewer().unwrap().tick().await;
    let first = serial(&s).unwrap();

    // Another domain list: the old certificate is served meanwhile, and a new
    // one is due now.
    clock.advance(DAY);
    let cfg2 = config(tmp.path(), &["app.example.com", "api.example.com"]);
    let s2 = setup(&cfg2, &ca, &clock, false);
    assert_eq!(serial(&s2).as_deref(), Some(first.as_str()));
    let r2 = s2.renewer().unwrap();
    assert_eq!(r2.next_attempt(), clock.now());
    r2.tick().await;
    assert_eq!(s2.store().info().unwrap().names, ["app.example.com", "api.example.com"]);

    // Past its expiry, a stored certificate is not served.
    clock.advance(91 * DAY);
    let s3 = setup(&cfg2, &ca, &clock, false);
    assert!(serial(&s3).is_none());
    assert_eq!(s3.renewer().unwrap().next_attempt(), clock.now());
}

#[tokio::test]
async fn the_backoff_is_capped() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config(tmp.path(), &["app.example.com"]);
    let clock = Arc::new(ManualClock::new(T0));
    let ca = FakeCa::new(clock.clone());
    ca.fail.store(true, Ordering::SeqCst);
    let s = setup(&cfg, &ca, &clock, false);
    let r = s.renewer().unwrap();
    let mut last = Duration::ZERO;
    for _ in 0..12 {
        let before = clock.now();
        r.tick().await;
        last = Duration::from_secs((r.next_attempt() - before) as u64);
        clock.set(r.next_attempt());
    }
    assert_eq!(last, Duration::from_secs(6 * 3600));
    assert_eq!(r.failures(), 12);
    assert!(serial(&s).is_none());
}
