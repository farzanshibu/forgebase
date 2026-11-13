//! [`CustomizeService`]: shared state, authorization, row helpers and the
//! in-memory caches that the Host router and the agreement gate read without
//! touching the database. The features are in [`crate::domains`],
//! [`crate::branding`], [`crate::email`] and [`crate::agreements`].

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, OnceLock, RwLock};

use forgebase_auth::Clock;
use forgebase_data::{AccessContext, Principal};
use forgebase_db::{Backend, Rows, Statement, TxMode, Value, WriteTx};
use forgebase_jobs::JobService;
use forgebase_policy::{Check, PolicyService};

use crate::dns::{DnsResolver, StaticResolver};
use crate::error::{CustomizeError, Result};
use crate::mail::{LogProvider, MailProvider};
use crate::mailconfig::{MailRegistry, MailRuntime};
use crate::model::AgreementRef;

/// Job kind that delivers one queued message.
pub const EMAIL_JOB: &str = "customize.email.send";
/// Permission to change a realm's customization (domains, branding, email
/// templates, agreements, sending mail).
pub const WRITE_PERMISSION: &str = "customization:write";
/// Permission to read the administrative views (domains, delivery log,
/// acceptance records, all agreement versions).
pub const READ_PERMISSION: &str = "customization:read";

/// Settings of the service.
#[derive(Debug, Clone)]
pub struct CustomizeConfig {
    /// How long a domain verification token stays valid.
    pub verify_ttl_ms: i64,
    /// Delivery attempts before a message is given up.
    pub email_max_attempts: i64,
    /// First retry delay (doubles per attempt).
    pub email_backoff_base_ms: i64,
    /// Address mail is sent from (`From` header; the SMTP envelope sender is
    /// configured on the provider).
    pub from_address: String,
    /// Take the host from `X-Forwarded-Host` (set only behind a reverse proxy
    /// that overwrites it).
    pub trust_forwarded_host: bool,
    /// Rendered bodies of failed deliveries are cleared after this long.
    pub failed_body_retention_ms: i64,
}

impl Default for CustomizeConfig {
    fn default() -> Self {
        Self {
            verify_ttl_ms: 7 * 24 * 3_600_000,
            email_max_attempts: 5,
            email_backoff_base_ms: 2_000,
            from_address: "no-reply@forgebase.invalid".into(),
            trust_forwarded_host: false,
            failed_body_retention_ms: 7 * 24 * 3_600_000,
        }
    }
}

/// What the Host router and the agreement gate read.
#[derive(Debug, Default)]
pub(crate) struct Cache {
    /// Verified custom domain -> realm.
    pub hosts: HashMap<String, String>,
    /// Realm -> current version of each published agreement kind.
    pub current: HashMap<String, Vec<AgreementRef>>,
    /// (realm, principal) -> highest accepted version per kind.
    pub accepted: HashMap<(String, String), HashMap<String, i64>>,
}

/// The customization service. Cheap to clone.
#[derive(Clone)]
pub struct CustomizeService {
    pub(crate) inner: Arc<Inner>,
}

impl fmt::Debug for CustomizeService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CustomizeService").finish_non_exhaustive()
    }
}

pub(crate) struct Inner {
    pub db: Arc<dyn Backend>,
    pub policy: PolicyService,
    pub clock: Arc<dyn Clock>,
    pub cfg: CustomizeConfig,
    pub jobs: OnceLock<JobService>,
    pub dns: RwLock<Arc<dyn DnsResolver>>,
    pub provider: RwLock<Arc<dyn MailProvider>>,
    pub mail: RwLock<MailRegistry>,
    pub mail_runtime: OnceLock<MailRuntime>,
    pub cache: RwLock<Cache>,
}

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

pub(crate) fn int(r: &[Value], i: usize) -> i64 {
    r.get(i).and_then(Value::as_i64).unwrap_or_default()
}

pub(crate) fn opt_int(r: &[Value], i: usize) -> Option<i64> {
    r.get(i).and_then(Value::as_i64)
}

pub(crate) fn text(r: &[Value], i: usize) -> String {
    r.get(i).and_then(Value::as_str).unwrap_or_default().to_owned()
}

pub(crate) fn opt_text(r: &[Value], i: usize) -> Option<String> {
    r.get(i).and_then(Value::as_str).map(str::to_owned)
}

pub(crate) fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4().simple())
}

/// A write transaction.
pub(crate) struct Tx(pub Box<dyn WriteTx>);

impl Tx {
    pub async fn fetch(&mut self, sql: &str, params: Vec<Value>) -> Result<Rows> {
        Ok(self.0.fetch_all(&Statement::new(sql, params)).await?)
    }

    pub async fn exec(&mut self, sql: &str, params: Vec<Value>) -> Result<u64> {
        Ok(self.0.execute(&Statement::new(sql, params)).await?.rows_affected)
    }

    pub async fn commit(self) -> Result<()> {
        Ok(self.0.commit().await?)
    }

    pub async fn rollback(self) {
        let _ = self.0.rollback().await;
    }
}

impl CustomizeService {
    /// Opens the service on a migrated database (migration 0019) and loads
    /// the caches. The default providers are a log mailer and an empty DNS
    /// resolver: install real ones with [`Self::set_dns`] and
    /// [`Self::set_mail_provider`].
    pub async fn open(
        db: Arc<dyn Backend>,
        policy: PolicyService,
        clock: Arc<dyn Clock>,
        cfg: CustomizeConfig,
    ) -> Result<Self> {
        let svc = Self {
            inner: Arc::new(Inner {
                db,
                policy,
                clock,
                cfg,
                jobs: OnceLock::new(),
                dns: RwLock::new(Arc::new(StaticResolver::new())),
                provider: RwLock::new(Arc::new(LogProvider::new())),
                mail: RwLock::new(MailRegistry::default()),
                mail_runtime: OnceLock::new(),
                cache: RwLock::new(Cache::default()),
            }),
        };
        svc.reload_hosts().await?;
        svc.reload_agreements().await?;
        Ok(svc)
    }

    /// Attaches the job queue that delivers mail.
    pub fn attach_jobs(&self, jobs: JobService) {
        let _ = self.inner.jobs.set(jobs);
    }

    /// Replaces the DNS resolver.
    pub fn set_dns(&self, dns: Arc<dyn DnsResolver>) {
        if let Ok(mut d) = self.inner.dns.write() {
            *d = dns;
        }
    }

    /// Replaces the mail provider.
    pub fn set_mail_provider(&self, provider: Arc<dyn MailProvider>) {
        if let Ok(mut p) = self.inner.provider.write() {
            *p = provider;
        }
    }

    pub(crate) fn dns(&self) -> Arc<dyn DnsResolver> {
        self.inner
            .dns
            .read()
            .map_or_else(|e| e.into_inner().clone(), |g| g.clone())
    }

    pub(crate) fn provider(&self) -> Arc<dyn MailProvider> {
        self.inner
            .provider
            .read()
            .map_or_else(|e| e.into_inner().clone(), |g| g.clone())
    }

    /// The service settings.
    pub fn config(&self) -> &CustomizeConfig {
        &self.inner.cfg
    }

    pub(crate) fn db(&self) -> &dyn Backend {
        self.inner.db.as_ref()
    }

    /// Now, in epoch milliseconds.
    pub fn now(&self) -> i64 {
        self.inner.clock.now().saturating_mul(1000)
    }

    pub(crate) async fn begin(&self) -> Result<Tx> {
        Ok(Tx(self.db().begin(TxMode::Normal).await?))
    }

    pub(crate) async fn query(&self, sql: &str, params: Vec<Value>) -> Result<Rows> {
        Ok(self.db().fetch_all(&Statement::new(sql, params)).await?)
    }

    // -----------------------------------------------------------------------
    // Authorization
    // -----------------------------------------------------------------------

    pub(crate) fn require_caller(&self, ctx: &AccessContext) -> Result<()> {
        if ctx.principal == Principal::Anonymous {
            return Err(CustomizeError::Unauthenticated);
        }
        Ok(())
    }

    fn check(&self, ctx: &AccessContext, permission: &str) -> Result<()> {
        self.inner.policy.check(
            ctx,
            &Check {
                permission: permission.to_owned(),
                ..Check::default()
            },
        )?;
        Ok(())
    }

    /// The realm an administrative call acts on: privileged callers name any
    /// realm, everyone else acts on the realm of the request.
    pub(crate) fn realm_for(&self, ctx: &AccessContext, requested: Option<&str>) -> Result<String> {
        match requested.map(str::trim).filter(|r| !r.is_empty()) {
            Some(r) if r != ctx.realm_id && !ctx.is_privileged() => Err(CustomizeError::Forbidden(
                "another realm can only be managed by platform administrators".into(),
            )),
            Some(r) => Ok(r.to_owned()),
            None => Ok(ctx.realm_id.clone()),
        }
    }

    /// May change customization of `realm`.
    pub(crate) fn can_write(&self, ctx: &AccessContext, realm: &str) -> Result<()> {
        self.require_caller(ctx)?;
        if ctx.is_privileged() {
            return Ok(());
        }
        if realm != ctx.realm_id {
            return Err(CustomizeError::Forbidden("another realm".into()));
        }
        self.check(ctx, WRITE_PERMISSION)
    }

    /// May read the administrative views of `realm`.
    pub(crate) fn can_read(&self, ctx: &AccessContext, realm: &str) -> Result<()> {
        self.require_caller(ctx)?;
        if ctx.is_privileged() {
            return Ok(());
        }
        if realm != ctx.realm_id {
            return Err(CustomizeError::Forbidden("another realm".into()));
        }
        self.check(ctx, READ_PERMISSION)
            .or_else(|_| self.check(ctx, WRITE_PERMISSION))
    }

    /// The realm exists.
    pub(crate) async fn require_realm(&self, realm: &str) -> Result<()> {
        let r = self
            .query("SELECT 1 FROM _forgebase_realms WHERE id = ?1", vec![realm.into()])
            .await?;
        if r.rows.is_empty() {
            return Err(CustomizeError::NotFound(format!("realm {realm:?}")));
        }
        Ok(())
    }

    /// Whether `realm` exists (never errors: a read failure is "no").
    pub async fn realm_exists(&self, realm: &str) -> bool {
        self.require_realm(realm).await.is_ok()
    }
}

/// Checks a short identifier (realm ids, agreement kinds, template keys).
pub(crate) fn check_key(what: &str, v: &str, allow_upper: bool) -> Result<()> {
    let ok = !v.is_empty()
        && v.len() <= 64
        && v.chars().all(|c| {
            c.is_ascii_lowercase()
                || c.is_ascii_digit()
                || c == '_'
                || c == '-'
                || (allow_upper && c.is_ascii_uppercase())
        });
    if ok {
        Ok(())
    } else {
        Err(CustomizeError::invalid(
            "invalid_key",
            format!("{what} must be 1-64 characters of a-z, 0-9, `_` and `-`"),
        ))
    }
}
