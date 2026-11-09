//! [`BillingService`]: opening, authorization, settings, provider accounts,
//! customers, row helpers and billing periods. The catalog is in
//! [`crate::catalog`], subscriptions, orders, invoices, payments and refunds
//! in [`crate::billing`], webhooks in [`crate::webhook`], metered usage in
//! [`crate::usage`], statistics in [`crate::stats`].
//!
//! Every write is one transaction on the single writer. Provider calls
//! (HTTP) never run inside a transaction: a payment row is stored as
//! `pending` first, the provider is called, and the result is applied in a
//! second transaction (a crash in between is repaired by the webhook or the
//! next dunning attempt, both idempotent).

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, OnceLock, RwLock};

use base64::Engine as _;
use forgebase_auth::crypto::Keyring;
use forgebase_auth::social::http_client::HttpClient;
use forgebase_credits::CreditSink;
use forgebase_data::{AccessContext, Principal};
use forgebase_db::{Backend, Rows, Statement, TxMode, Value, WriteTx};
use forgebase_entitlements::period::{civil_from_days, days_from_civil, days_in_month};
use forgebase_entitlements::{Clock, EntitlementService, parse_subject, subject_key};
use forgebase_jobs::JobService;
use forgebase_policy::{Check, PolicyService};
use serde_json::{Value as Json, json};

use crate::error::{BillingError, Result};
use crate::model::{BillingSettings, Customer, CustomerInput, Interval, ProviderAccount, ProviderInput};
use crate::money::currency;
use crate::provider::app_store::AppStoreConfig;
use crate::provider::google_play::GooglePlayConfig;
use crate::provider::wechat_pay::WechatPayConfig;
use crate::provider::{
    AppStoreProvider, CreemProvider, GooglePlayProvider, ManualProvider, PaymentProvider, RazorpayProvider,
    StoreProvider, StripeProvider, WechatPayProvider,
};

/// A driver built from a provider account.
pub(crate) enum Driver {
    /// Collects ForgeBase invoices (Stripe, Razorpay, WeChat Pay, manual).
    Payment(Arc<dyn PaymentProvider>),
    /// A merchant-of-record store (App Store, Google Play, Creem).
    Store(Arc<dyn StoreProvider>),
}

/// Job kind of the periodic maintenance (usage, renewals, dunning).
pub const MAINTENANCE_JOB: &str = "billing.maintenance";
/// Name of the maintenance schedule.
pub const MAINTENANCE_SCHEDULE: &str = "billing-maintenance";
/// Name of the usage-event cursor.
pub const USAGE_CURSOR: &str = "usage";
/// One day in milliseconds.
pub const DAY: i64 = 86_400_000;

/// The billing service. Cheap to clone.
#[derive(Clone)]
pub struct BillingService {
    pub(crate) inner: Arc<Inner>,
}

impl fmt::Debug for BillingService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BillingService").finish_non_exhaustive()
    }
}

pub(crate) struct Inner {
    pub db: Arc<dyn Backend>,
    pub policy: PolicyService,
    pub ent: EntitlementService,
    pub credits: Arc<dyn CreditSink>,
    pub clock: Arc<dyn Clock>,
    pub keyring: Keyring,
    pub http: Arc<dyn HttpClient>,
    /// Drivers built from the stored provider accounts.
    pub providers: RwLock<HashMap<String, Arc<dyn PaymentProvider>>>,
    /// Drivers registered in code (FB-72 providers, tests).
    pub registered: RwLock<HashMap<String, Arc<dyn PaymentProvider>>>,
    /// Store drivers built from the stored accounts (FB-72).
    pub stores: RwLock<HashMap<String, Arc<dyn StoreProvider>>>,
    /// Store drivers registered in code (tests).
    pub registered_stores: RwLock<HashMap<String, Arc<dyn StoreProvider>>>,
    pub jobs: OnceLock<JobService>,
    /// Serialises maintenance runs and usage ingestion.
    pub runner: tokio::sync::Mutex<()>,
}

// ---------------------------------------------------------------------------
// Rows and transactions
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

pub(crate) fn json_col(r: &[Value], i: usize) -> Json {
    serde_json::from_str(&text(r, i)).unwrap_or(Json::Null)
}

pub(crate) fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4().simple())
}

/// A write transaction.
pub(crate) struct Tx<'a>(pub &'a mut (dyn WriteTx + 'static));

impl Tx<'_> {
    pub async fn fetch(&mut self, sql: &str, params: Vec<Value>) -> Result<Rows> {
        Ok(self.0.fetch_all(&Statement::new(sql, params)).await?)
    }

    pub async fn exec(&mut self, sql: &str, params: Vec<Value>) -> Result<u64> {
        Ok(self.0.execute(&Statement::new(sql, params)).await?.rows_affected)
    }

    /// Appends an audit row.
    pub async fn audit(
        &mut self,
        actor: &str,
        action: &str,
        object: &str,
        id: &str,
        detail: Json,
        now: i64,
    ) -> Result<()> {
        self.exec(
            "INSERT INTO _forgebase_billing_audit (at, actor, action, object, object_id, detail)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            vec![
                now.into(),
                actor.into(),
                action.into(),
                object.into(),
                id.into(),
                detail.to_string().into(),
            ],
        )
        .await?;
        Ok(())
    }

    /// The next gapless number of `prefix` (`INV-000042`).
    pub async fn next_number(&mut self, prefix: &str) -> Result<String> {
        let rows = self
            .fetch(
                "INSERT INTO _forgebase_billing_sequences (prefix, next) VALUES (?1, 1)
                 ON CONFLICT (prefix) DO UPDATE SET next = next + 1
                 RETURNING next",
                vec![prefix.into()],
            )
            .await?;
        let n = rows.rows.first().map(|r| int(r, 0)).unwrap_or(1);
        Ok(format!("{prefix}-{n:06}"))
    }
}

pub(crate) fn check_subject(s: &str) -> Result<String> {
    let s = s.trim();
    parse_subject(s).map_err(|_| {
        BillingError::invalid(
            "invalid_subject",
            "subject must be org:<id>, user:<collection>/<id> or service:<id>",
        )
    })?;
    Ok(s.to_owned())
}

pub(crate) fn org_of(subject: &str) -> Option<&str> {
    parse_subject(subject).ok().flatten()
}

pub(crate) fn check_key(key: &str, what: &str) -> Result<String> {
    let k = key.trim();
    if k.is_empty()
        || k.len() > 128
        || !k
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':'))
    {
        return Err(BillingError::invalid(
            "invalid_key",
            format!("{what} must be 1 to 128 letters, digits or _ - . :"),
        ));
    }
    Ok(k.to_owned())
}

pub(crate) fn check_idem(key: &str) -> Result<String> {
    let k = key.trim();
    if k.is_empty() || k.len() > 200 {
        return Err(BillingError::invalid(
            "invalid_idempotency_key",
            "idempotency_key must be 1 to 200 characters",
        ));
    }
    Ok(k.to_owned())
}

pub(crate) fn check_text(v: Option<String>, field: &str, max: usize) -> Result<Option<String>> {
    match v.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()) {
        Some(s) if s.len() > max => Err(BillingError::invalid(
            "invalid_field",
            format!("{field} must be at most {max} characters"),
        )),
        v => Ok(v),
    }
}

pub(crate) fn check_metadata(v: Option<Json>) -> Result<Json> {
    match v {
        None | Some(Json::Null) => Ok(json!({})),
        Some(Json::Object(m)) if Json::Object(m.clone()).to_string().len() <= 8 * 1024 => Ok(Json::Object(m)),
        Some(_) => Err(BillingError::invalid(
            "invalid_metadata",
            "metadata must be a JSON object of at most 8 KiB",
        )),
    }
}

pub(crate) fn check_url(v: &str) -> Result<String> {
    let u =
        url::Url::parse(v.trim()).map_err(|_| BillingError::invalid("invalid_url", format!("{v:?} is not a URL")))?;
    let local = matches!(u.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if u.scheme() == "https" || (u.scheme() == "http" && local) {
        Ok(u.to_string())
    } else {
        Err(BillingError::invalid(
            "invalid_url",
            "URLs must be https (http only for localhost)",
        ))
    }
}

/// Adds calendar months, keeping the day of month (clamped to short months)
/// and the time of day.
pub fn add_months(ms: i64, months: i64) -> i64 {
    let (day, rem) = (ms.div_euclid(DAY), ms.rem_euclid(DAY));
    let (y, m, d) = civil_from_days(day);
    let total = y * 12 + i64::from(m) - 1 + months;
    let ny = total.div_euclid(12);
    let nm = u32::try_from(total.rem_euclid(12) + 1).unwrap_or(1);
    let nd = d.min(days_in_month(ny, nm));
    days_from_civil(ny, nm, nd) * DAY + rem
}

/// Months per billing period.
pub fn period_months(interval: Interval, count: i64) -> i64 {
    match interval {
        Interval::Month => count.max(1),
        Interval::Year => 12 * count.max(1),
    }
}

/// The first period boundary after `after`: the smallest `add_months(anchor,
/// k × months)` greater than `after` (k ≥ 1). Computing every boundary from
/// the anchor keeps the 31st on the 31st after a short month.
pub fn next_boundary(anchor: i64, months: i64, after: i64) -> i64 {
    let months = months.max(1);
    let mut k = ((after - anchor) / (31 * DAY * months)).max(1);
    while add_months(anchor, k * months) <= after {
        k += 1;
    }
    add_months(anchor, k * months)
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

/// What [`BillingService::open`] needs besides the database.
pub struct BillingDeps {
    /// The authorization pipeline.
    pub policy: PolicyService,
    /// FB-24: plans, the usage feed and the [`forgebase_entitlements::BillingSync`] seam.
    pub entitlements: EntitlementService,
    /// FB-73: credit top-ups and their reversal.
    pub credits: Arc<dyn CreditSink>,
    /// Time.
    pub clock: Arc<dyn Clock>,
    /// Seals provider secrets.
    pub keyring: Keyring,
    /// Outbound HTTP for provider APIs (FB-25 `HttpsClient` in production).
    pub http: Arc<dyn HttpClient>,
}

impl BillingService {
    /// Opens the service on a migrated database and loads the provider
    /// accounts.
    pub async fn open(db: Arc<dyn Backend>, deps: BillingDeps) -> Result<Self> {
        // Fails early on a database without migration 20.
        db.fetch_all(&Statement::raw("SELECT COUNT(*) FROM _forgebase_billing_settings"))
            .await?;
        let svc = Self {
            inner: Arc::new(Inner {
                db,
                policy: deps.policy,
                ent: deps.entitlements,
                credits: deps.credits,
                clock: deps.clock,
                keyring: deps.keyring,
                http: deps.http,
                providers: RwLock::new(HashMap::new()),
                registered: RwLock::new(HashMap::new()),
                stores: RwLock::new(HashMap::new()),
                registered_stores: RwLock::new(HashMap::new()),
                jobs: OnceLock::new(),
                runner: tokio::sync::Mutex::new(()),
            }),
        };
        svc.load_providers().await?;
        Ok(svc)
    }

    /// Attaches the job queue (maintenance schedule).
    pub fn attach_jobs(&self, jobs: JobService) {
        let _ = self.inner.jobs.set(jobs);
    }

    /// Registers a provider driver in code (more providers, tests). It takes
    /// precedence over a stored account of the same name.
    pub fn register_provider(&self, provider: Arc<dyn PaymentProvider>) {
        if let Ok(mut m) = self.inner.registered.write() {
            m.insert(provider.name().to_owned(), provider);
        }
    }

    /// Registers a store driver in code (tests). It takes precedence over a
    /// stored account of the same name.
    pub fn register_store(&self, store: Arc<dyn StoreProvider>) {
        if let Ok(mut m) = self.inner.registered_stores.write() {
            m.insert(store.name().to_owned(), store);
        }
    }

    /// The store driver of a provider, if it is a configured store.
    pub fn store(&self, name: &str) -> Option<Arc<dyn StoreProvider>> {
        if let Some(p) = self
            .inner
            .registered_stores
            .read()
            .ok()
            .and_then(|m| m.get(name).cloned())
        {
            return Some(p);
        }
        self.inner.stores.read().ok().and_then(|m| m.get(name).cloned())
    }

    pub(crate) fn db(&self) -> &dyn Backend {
        self.inner.db.as_ref()
    }

    /// Now, epoch milliseconds.
    pub fn now(&self) -> i64 {
        self.inner.clock.now_ms()
    }

    /// Commits when `res` wrote something, rolls back otherwise.
    pub(crate) async fn end<T>(&self, tx: Box<dyn WriteTx>, res: Result<(T, bool)>) -> Result<T> {
        match res {
            Ok((v, true)) => {
                tx.commit().await?;
                Ok(v)
            }
            Ok((v, false)) => {
                tx.rollback().await?;
                Ok(v)
            }
            Err(e) => {
                let _ = tx.rollback().await;
                Err(e)
            }
        }
    }

    pub(crate) async fn begin(&self) -> Result<Box<dyn WriteTx>> {
        Ok(self.db().begin(TxMode::Normal).await?)
    }

    pub(crate) async fn fetch(&self, sql: &str, params: Vec<Value>) -> Result<Rows> {
        Ok(self.db().fetch_all(&Statement::new(sql, params)).await?)
    }

    // -----------------------------------------------------------------------
    // Authorization
    // -----------------------------------------------------------------------

    pub(crate) fn require_caller(&self, ctx: &AccessContext) -> Result<()> {
        if ctx.principal == Principal::Anonymous {
            return Err(BillingError::Unauthenticated);
        }
        Ok(())
    }

    pub(crate) fn require_admin(&self, ctx: &AccessContext) -> Result<()> {
        if ctx.is_privileged() {
            return Ok(());
        }
        self.require_caller(ctx)?;
        Err(BillingError::Forbidden("platform administrators only".into()))
    }

    /// The subject a request acts on: the given one, else the caller's (the
    /// active organization, else the caller).
    pub(crate) fn subject_for(&self, ctx: &AccessContext, given: Option<&str>) -> Result<String> {
        self.require_caller(ctx)?;
        match given.filter(|s| !s.trim().is_empty()) {
            Some(s) => check_subject(s),
            None => check_subject(&subject_key(&ctx.subject())).map_err(|_| {
                BillingError::invalid("subject_required", "name the subject (org:<id> or a principal key)")
            }),
        }
    }

    fn check(&self, ctx: &AccessContext, permission: &str, org: Option<&str>) -> Result<()> {
        self.inner.policy.check(
            ctx,
            &Check {
                permission: permission.to_owned(),
                org: org.map(str::to_owned),
                ..Check::default()
            },
        )?;
        Ok(())
    }

    /// `permission` in the subject's organization, else at realm level.
    fn check_for(&self, ctx: &AccessContext, subject: &str, permission: &str) -> Result<()> {
        match org_of(subject) {
            Some(org) => self
                .check(ctx, permission, Some(org))
                .or_else(|e| self.check(ctx, permission, None).map_err(|_| e)),
            None => self.check(ctx, permission, None),
        }
    }

    /// May read the subject's billing: the subject itself (personal
    /// subjects), `billing:read` in the organization or at realm level, or
    /// privileged callers.
    pub(crate) fn can_read(&self, ctx: &AccessContext, subject: &str) -> Result<()> {
        if ctx.is_privileged() {
            return Ok(());
        }
        self.require_caller(ctx)?;
        if org_of(subject).is_none() && subject == ctx.principal_key() {
            return Ok(());
        }
        self.check_for(ctx, subject, "billing:read")
    }

    /// May subscribe, change plans, cancel, buy and manage payment methods for
    /// the subject: the subject itself (personal subjects), `billing:write` in
    /// the organization (owner, billing manager) or at realm level, or
    /// privileged callers. Customers only spend their own money.
    pub(crate) fn can_manage(&self, ctx: &AccessContext, subject: &str) -> Result<()> {
        if ctx.is_privileged() {
            return Ok(());
        }
        self.require_caller(ctx)?;
        if org_of(subject).is_none() && subject == ctx.principal_key() {
            return Ok(());
        }
        self.check_for(ctx, subject, "billing:write")
    }

    /// May give money back or forgive it (credit notes, refunds, manual
    /// payments, voiding): `billing:write` at realm level (operators) or
    /// privileged callers. An organization's own billing manager must not
    /// refund itself.
    pub(crate) fn can_operate(&self, ctx: &AccessContext) -> Result<()> {
        if ctx.is_privileged() {
            return Ok(());
        }
        self.require_caller(ctx)?;
        self.check(ctx, "billing:write", None)
    }

    /// Instance-wide reads (statistics, every customer): privileged callers or
    /// realm-level `billing:read`.
    pub(crate) fn can_read_all(&self, ctx: &AccessContext) -> Result<()> {
        if ctx.is_privileged() {
            return Ok(());
        }
        self.require_caller(ctx)?;
        self.check(ctx, "billing:read", None)
    }

    // -----------------------------------------------------------------------
    // Settings
    // -----------------------------------------------------------------------

    /// The operator settings.
    pub async fn settings(&self) -> Result<BillingSettings> {
        let rows = self
            .fetch(
                "SELECT value FROM _forgebase_billing_settings WHERE key = 'settings'",
                Vec::new(),
            )
            .await?;
        Ok(rows
            .rows
            .first()
            .and_then(|r| serde_json::from_str(&text(r, 0)).ok())
            .unwrap_or_default())
    }

    /// Replaces the settings (platform administrators).
    pub async fn put_settings(&self, ctx: &AccessContext, s: BillingSettings) -> Result<BillingSettings> {
        self.require_admin(ctx)?;
        let mut s = s;
        s.default_currency = currency(&s.default_currency)?;
        if s.retry_days.len() > 10
            || s.retry_days.windows(2).any(|w| w[1] <= w[0])
            || s.retry_days.iter().any(|d| !(1..=60).contains(d))
        {
            return Err(BillingError::invalid(
                "invalid_settings",
                "retry_days: at most 10 increasing days between 1 and 60",
            ));
        }
        for (p, name) in [
            (&s.invoice_prefix, "invoice_prefix"),
            (&s.credit_note_prefix, "credit_note_prefix"),
        ] {
            if p.is_empty() || p.len() > 12 || !p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
                return Err(BillingError::invalid(
                    "invalid_settings",
                    format!("{name}: 1 to 12 letters, digits or -"),
                ));
            }
        }
        if s.invoice_prefix == s.credit_note_prefix {
            return Err(BillingError::invalid(
                "invalid_settings",
                "invoices and credit notes need different prefixes",
            ));
        }
        if !(1..=30).contains(&s.incomplete_days)
            || !(60..=30 * 86_400).contains(&s.webhook_max_age_secs)
            || !(10..=3600).contains(&s.webhook_tolerance_secs)
        {
            return Err(BillingError::invalid(
                "invalid_settings",
                "incomplete_days 1..30, webhook_max_age_secs 60..2592000, webhook_tolerance_secs 10..3600",
            ));
        }
        let now = self.now();
        let mut tx = self.begin().await?;
        let res = async {
            let mut t = Tx(tx.as_mut());
            t.exec(
                "INSERT INTO _forgebase_billing_settings (key, value, updated_at) VALUES ('settings', ?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                vec![serde_json::to_string(&s)?.into(), now.into()],
            )
            .await?;
            t.audit(
                &ctx.principal_key(),
                "settings.put",
                "settings",
                "settings",
                json!(s),
                now,
            )
            .await?;
            Ok((s.clone(), true))
        }
        .await;
        self.end(tx, res).await
    }

    // -----------------------------------------------------------------------
    // Provider accounts
    // -----------------------------------------------------------------------

    fn aad(provider: &str) -> Vec<u8> {
        format!("billing:provider:{provider}").into_bytes()
    }

    fn build_provider(
        &self,
        provider: &str,
        key_id: Option<&str>,
        secret: &Json,
        api_base: Option<&str>,
    ) -> Result<Driver> {
        let s = |k: &str| secret.get(k).and_then(Json::as_str).unwrap_or_default().to_owned();
        let config = secret.get("config").cloned().unwrap_or(Json::Null);
        let (http, clock) = (self.inner.http.clone(), self.inner.clock.clone());
        Ok(Driver::Payment(match provider {
            "app_store" => {
                return Ok(Driver::Store(Arc::new(AppStoreProvider::new(
                    AppStoreConfig::from_json(&config),
                    api_base,
                    None,
                    http,
                    clock,
                )?)));
            }
            "google_play" => {
                return Ok(Driver::Store(Arc::new(GooglePlayProvider::new(
                    GooglePlayConfig::from_json(&config),
                    api_base,
                    http,
                    clock,
                )?)));
            }
            "creem" => {
                return Ok(Driver::Store(Arc::new(CreemProvider::new(
                    &s("api_key"),
                    &s("webhook_secret"),
                    api_base,
                    http,
                    clock,
                )?)));
            }
            "wechat_pay" => Arc::new(WechatPayProvider::new(
                WechatPayConfig::from_json(key_id.unwrap_or_default(), &s("api_key"), &config),
                api_base,
                http,
                clock,
            )?),
            "stripe" => Arc::new(StripeProvider::new(
                &s("api_key"),
                &s("webhook_secret"),
                api_base,
                self.inner.http.clone(),
            )?),
            "razorpay" => Arc::new(RazorpayProvider::new(
                key_id.unwrap_or_default(),
                &s("api_key"),
                &s("webhook_secret"),
                api_base,
                self.inner.http.clone(),
            )?),
            "manual" => Arc::new(ManualProvider),
            other => {
                return Err(BillingError::invalid(
                    "unknown_provider",
                    format!(
                        "unknown provider {other:?} (stripe, razorpay, wechat_pay, manual, app_store, google_play, creem)"
                    ),
                ));
            }
        }))
    }

    async fn load_providers(&self) -> Result<()> {
        let rows = self
            .fetch(
                "SELECT provider, key_id, secret, api_base FROM _forgebase_billing_providers WHERE enabled = 1",
                Vec::new(),
            )
            .await?;
        let (mut map, mut stores) = (HashMap::new(), HashMap::new());
        for r in &rows.rows {
            let name = text(r, 0);
            let secret = self.open_secret(&name, &text(r, 2))?;
            match self.build_provider(&name, opt_text(r, 1).as_deref(), &secret, opt_text(r, 3).as_deref()) {
                Ok(Driver::Payment(p)) => {
                    map.insert(name, p);
                }
                Ok(Driver::Store(p)) => {
                    stores.insert(name, p);
                }
                Err(e) => tracing::warn!(provider = %name, error = %e, "billing provider not loaded"),
            }
        }
        if let Ok(mut m) = self.inner.providers.write() {
            *m = map;
        }
        if let Ok(mut m) = self.inner.stores.write() {
            *m = stores;
        }
        Ok(())
    }

    fn open_secret(&self, provider: &str, sealed: &str) -> Result<Json> {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(sealed)
            .map_err(|_| BillingError::Internal("provider secret is not base64".into()))?;
        let plain = self
            .inner
            .keyring
            .open(&bytes, &Self::aad(provider))
            .map_err(|_| BillingError::Internal("provider secret cannot be opened".into()))?;
        Ok(serde_json::from_slice(&plain)?)
    }

    /// The driver of a provider.
    pub fn provider(&self, name: &str) -> Result<Arc<dyn PaymentProvider>> {
        if let Some(p) = self.inner.registered.read().ok().and_then(|m| m.get(name).cloned()) {
            return Ok(p);
        }
        self.inner
            .providers
            .read()
            .ok()
            .and_then(|m| m.get(name).cloned())
            .ok_or_else(|| BillingError::NotConfigured(format!("payment provider {name:?} is not configured")))
    }

    /// The provider used when a customer names none: the only configured one.
    pub(crate) fn default_provider(&self) -> Result<String> {
        let mut names: Vec<String> = Vec::new();
        if let Ok(m) = self.inner.registered.read() {
            names.extend(m.keys().cloned());
        }
        if let Ok(m) = self.inner.providers.read() {
            names.extend(m.keys().cloned());
        }
        names.sort();
        names.dedup();
        match names.as_slice() {
            [one] => Ok(one.clone()),
            [] => Err(BillingError::NotConfigured("no payment provider is configured".into())),
            _ => Err(BillingError::invalid(
                "provider_required",
                format!("several providers are configured ({}); name one", names.join(", ")),
            )),
        }
    }

    fn account_row(r: &[Value]) -> ProviderAccount {
        let provider = text(r, 0);
        ProviderAccount {
            webhook_path: format!("/api/v1/billing/webhooks/{provider}"),
            provider,
            key_id: opt_text(r, 1),
            hint: opt_text(r, 2),
            api_base: opt_text(r, 3),
            enabled: int(r, 4) != 0,
            updated_at: int(r, 5),
        }
    }

    /// Configured provider accounts (platform administrators; no secrets).
    pub async fn providers(&self, ctx: &AccessContext) -> Result<Vec<ProviderAccount>> {
        self.require_admin(ctx)?;
        let rows = self
            .fetch(
                "SELECT provider, key_id, hint, api_base, enabled, updated_at FROM _forgebase_billing_providers
                 ORDER BY provider",
                Vec::new(),
            )
            .await?;
        Ok(rows.rows.iter().map(|r| Self::account_row(r)).collect())
    }

    /// Saves a provider account (platform administrators). Test-mode keys
    /// only; the secrets are sealed with the instance keyring and never
    /// returned.
    pub async fn put_provider(
        &self,
        ctx: &AccessContext,
        provider: &str,
        input: ProviderInput,
    ) -> Result<ProviderAccount> {
        self.require_admin(ctx)?;
        let api_base = match input.api_base.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(u) => Some(check_url(u)?.trim_end_matches('/').to_owned()),
            None => None,
        };
        let mut secret = json!({
            "api_key": input.api_key.clone().unwrap_or_default().trim(),
            "webhook_secret": input.webhook_secret.clone().unwrap_or_default().trim(),
        });
        if let Some(c) = &input.config {
            if !c.is_object() || c.to_string().len() > 64 * 1024 {
                return Err(BillingError::invalid(
                    "invalid_config",
                    "config must be an object of at most 64 KiB",
                ));
            }
            secret["config"] = c.clone();
        }
        let key_id = input.key_id.as_deref().map(str::trim).map(str::to_owned);
        // Validates the keys (test mode) before anything is stored.
        let driver = self.build_provider(provider, key_id.as_deref(), &secret, api_base.as_deref())?;
        let hint = input
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|k| k.len() > 8)
            .map(|k| format!("…{}", &k[k.len() - 4..]));
        let sealed = self
            .inner
            .keyring
            .seal(secret.to_string().as_bytes(), &Self::aad(provider))
            .map_err(|e| BillingError::Internal(e.to_string()))?;
        let sealed = base64::engine::general_purpose::STANDARD.encode(sealed);
        let now = self.now();
        let mut tx = self.begin().await?;
        let res = async {
            let mut t = Tx(tx.as_mut());
            t.exec(
                "INSERT INTO _forgebase_billing_providers (provider, key_id, secret, api_base, hint, enabled, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
                 ON CONFLICT (provider) DO UPDATE SET key_id = excluded.key_id, secret = excluded.secret,
                     api_base = excluded.api_base, hint = excluded.hint, enabled = excluded.enabled,
                     updated_at = excluded.updated_at",
                vec![
                    provider.into(),
                    key_id.clone().into(),
                    sealed.into(),
                    api_base.clone().into(),
                    hint.clone().into(),
                    input.enabled.into(),
                    now.into(),
                ],
            )
            .await?;
            t.audit(&ctx.principal_key(), "provider.put", "provider", provider, json!({"enabled": input.enabled}), now)
                .await?;
            Ok(((), true))
        }
        .await;
        self.end(tx, res).await?;
        match driver {
            Driver::Payment(d) => {
                if let Ok(mut m) = self.inner.providers.write() {
                    if input.enabled {
                        m.insert(provider.to_owned(), d);
                    } else {
                        m.remove(provider);
                    }
                }
            }
            Driver::Store(d) => {
                if let Ok(mut m) = self.inner.stores.write() {
                    if input.enabled {
                        m.insert(provider.to_owned(), d);
                    } else {
                        m.remove(provider);
                    }
                }
            }
        }
        Ok(ProviderAccount {
            provider: provider.to_owned(),
            key_id,
            hint,
            api_base,
            enabled: input.enabled,
            webhook_path: format!("/api/v1/billing/webhooks/{provider}"),
            updated_at: now,
        })
    }

    /// Removes a provider account (platform administrators).
    pub async fn delete_provider(&self, ctx: &AccessContext, provider: &str) -> Result<()> {
        self.require_admin(ctx)?;
        let now = self.now();
        let mut tx = self.begin().await?;
        let res = async {
            let mut t = Tx(tx.as_mut());
            let n = t
                .exec(
                    "DELETE FROM _forgebase_billing_providers WHERE provider = ?1",
                    vec![provider.into()],
                )
                .await?;
            if n == 0 {
                return Err(BillingError::NotFound("provider".into()));
            }
            t.audit(
                &ctx.principal_key(),
                "provider.delete",
                "provider",
                provider,
                json!({}),
                now,
            )
            .await?;
            Ok(((), true))
        }
        .await;
        self.end(tx, res).await?;
        if let Ok(mut m) = self.inner.providers.write() {
            m.remove(provider);
        }
        if let Ok(mut m) = self.inner.stores.write() {
            m.remove(provider);
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Customers
    // -----------------------------------------------------------------------

    pub(crate) const CUSTOMER_COLUMNS: &'static str =
        "id, subject, email, name, currency, region, tax_exempt, provider,
        provider_customer_id, payment_method, payment_method_label, balance, metadata, created_at";

    pub(crate) fn customer_row(r: &[Value]) -> Customer {
        Customer {
            id: text(r, 0),
            subject: text(r, 1),
            email: opt_text(r, 2),
            name: opt_text(r, 3),
            currency: text(r, 4),
            region: opt_text(r, 5),
            tax_exempt: int(r, 6) != 0,
            provider: opt_text(r, 7),
            provider_customer_id: opt_text(r, 8),
            has_payment_method: opt_text(r, 9).is_some(),
            payment_method_label: opt_text(r, 10),
            balance: int(r, 11),
            metadata: json_col(r, 12),
            created_at: int(r, 13),
        }
    }

    pub(crate) async fn customer_by_subject(&self, subject: &str) -> Result<Option<Customer>> {
        let rows = self
            .fetch(
                &format!(
                    "SELECT {} FROM _forgebase_billing_customers WHERE subject = ?1",
                    Self::CUSTOMER_COLUMNS
                ),
                vec![subject.into()],
            )
            .await?;
        Ok(rows.rows.first().map(|r| Self::customer_row(r)))
    }

    pub(crate) async fn customer_by_id(&self, id: &str) -> Result<Customer> {
        let rows = self
            .fetch(
                &format!(
                    "SELECT {} FROM _forgebase_billing_customers WHERE id = ?1",
                    Self::CUSTOMER_COLUMNS
                ),
                vec![id.into()],
            )
            .await?;
        rows.rows
            .first()
            .map(|r| Self::customer_row(r))
            .ok_or_else(|| BillingError::NotFound("customer".into()))
    }

    /// The saved payment method of a customer (a provider reference).
    pub(crate) async fn payment_method(&self, customer_id: &str) -> Result<Option<String>> {
        let rows = self
            .fetch(
                "SELECT payment_method FROM _forgebase_billing_customers WHERE id = ?1",
                vec![customer_id.into()],
            )
            .await?;
        Ok(rows.rows.first().and_then(|r| opt_text(r, 0)))
    }

    /// The customer of a subject (the subject itself or `billing:read`).
    pub async fn customer(&self, ctx: &AccessContext, subject: Option<&str>) -> Result<Customer> {
        let subject = self.subject_for(ctx, subject)?;
        self.can_read(ctx, &subject)?;
        self.customer_by_subject(&subject)
            .await?
            .ok_or_else(|| BillingError::NotFound("customer".into()))
    }

    /// Every customer (realm-level `billing:read` or privileged).
    pub async fn customers(&self, ctx: &AccessContext, limit: Option<u32>) -> Result<Vec<Customer>> {
        self.can_read_all(ctx)?;
        let limit = i64::from(limit.unwrap_or(100).clamp(1, 1000));
        let rows = self
            .fetch(
                &format!(
                    "SELECT {} FROM _forgebase_billing_customers ORDER BY created_at DESC LIMIT ?1",
                    Self::CUSTOMER_COLUMNS
                ),
                vec![limit.into()],
            )
            .await?;
        Ok(rows.rows.iter().map(|r| Self::customer_row(r)).collect())
    }

    /// Creates or updates the customer of a subject (the subject itself or
    /// `billing:write`). The currency cannot change once something was billed.
    pub async fn put_customer(&self, ctx: &AccessContext, input: CustomerInput) -> Result<Customer> {
        let subject = self.subject_for(ctx, input.subject.as_deref())?;
        self.can_manage(ctx, &subject)?;
        if input.tax_exempt.is_some() {
            self.can_operate(ctx)?;
        }
        let email = check_text(input.email, "email", 254)?;
        let name = check_text(input.name, "name", 200)?;
        let region = check_text(input.region, "region", 16)?.map(|r| r.to_ascii_uppercase());
        let cur = match input.currency {
            Some(c) => Some(currency(&c)?),
            None => None,
        };
        let provider = match check_text(input.provider, "provider", 32)? {
            Some(p) => {
                self.provider(&p)?;
                Some(p)
            }
            None => None,
        };
        let metadata = match input.metadata {
            Some(m) => Some(check_metadata(Some(m))?),
            None => None,
        };
        let default_currency = self.settings().await?.default_currency;
        let now = self.now();
        let mut tx = self.begin().await?;
        let res = async {
            let mut t = Tx(tx.as_mut());
            let existing = t
                .fetch(
                    "SELECT id, currency FROM _forgebase_billing_customers WHERE subject = ?1",
                    vec![subject.clone().into()],
                )
                .await?;
            let id = match existing.rows.first() {
                Some(r) => {
                    let id = text(r, 0);
                    if let Some(c) = &cur
                        && *c != text(r, 1)
                    {
                        let billed = t
                            .fetch(
                                "SELECT COUNT(*) FROM _forgebase_billing_invoices WHERE customer_id = ?1",
                                vec![id.clone().into()],
                            )
                            .await?;
                        if billed.scalar().and_then(Value::as_i64).unwrap_or_default() > 0 {
                            return Err(BillingError::conflict(
                                "currency_locked",
                                "the customer's currency cannot change once it was invoiced",
                            ));
                        }
                    }
                    t.exec(
                        "UPDATE _forgebase_billing_customers SET email = COALESCE(?2, email), name = COALESCE(?3, name),
                             currency = COALESCE(?4, currency), region = COALESCE(?5, region),
                             provider = COALESCE(?6, provider), tax_exempt = COALESCE(?7, tax_exempt),
                             metadata = COALESCE(?8, metadata), updated_at = ?9
                         WHERE id = ?1",
                        vec![
                            id.clone().into(),
                            email.clone().into(),
                            name.clone().into(),
                            cur.clone().into(),
                            region.clone().into(),
                            provider.clone().into(),
                            input.tax_exempt.into(),
                            metadata.as_ref().map(Json::to_string).into(),
                            now.into(),
                        ],
                    )
                    .await?;
                    id
                }
                None => {
                    let id = new_id("cus");
                    t.exec(
                        "INSERT INTO _forgebase_billing_customers (id, subject, email, name, currency, region, tax_exempt,
                             provider, metadata, created_at, updated_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)",
                        vec![
                            id.clone().into(),
                            subject.clone().into(),
                            email.clone().into(),
                            name.clone().into(),
                            cur.clone().unwrap_or_else(|| default_currency.clone()).into(),
                            region.clone().into(),
                            input.tax_exempt.unwrap_or(false).into(),
                            provider.clone().into(),
                            metadata.clone().unwrap_or_else(|| json!({})).to_string().into(),
                            now.into(),
                        ],
                    )
                    .await?;
                    id
                }
            };
            t.audit(&ctx.principal_key(), "customer.put", "customer", &id, json!({"subject": subject}), now)
                .await?;
            Ok((id, true))
        }
        .await;
        let id = self.end(tx, res).await?;
        self.customer_by_id(&id).await
    }

    /// The customer of `subject`, created with defaults when missing.
    pub(crate) async fn ensure_customer(&self, subject: &str) -> Result<Customer> {
        if let Some(c) = self.customer_by_subject(subject).await? {
            return Ok(c);
        }
        let default_currency = self.settings().await?.default_currency;
        let now = self.now();
        let mut tx = self.begin().await?;
        let res = async {
            let mut t = Tx(tx.as_mut());
            t.exec(
                "INSERT INTO _forgebase_billing_customers (id, subject, currency, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?4) ON CONFLICT (subject) DO NOTHING",
                vec![
                    new_id("cus").into(),
                    subject.into(),
                    default_currency.into(),
                    now.into(),
                ],
            )
            .await?;
            Ok(((), true))
        }
        .await;
        self.end(tx, res).await?;
        self.customer_by_subject(subject)
            .await?
            .ok_or_else(|| BillingError::Internal("customer not created".into()))
    }

    /// Fails early when the customer has no usable payment provider.
    pub(crate) fn ensure_provider(&self, c: &Customer) -> Result<()> {
        match &c.provider {
            Some(p) => self.provider(p).map(|_| ()),
            None => self.default_provider().map(|_| ()),
        }
    }

    /// The provider of a customer, its provider-side customer id (created on
    /// first use) and the reference the provider needs.
    pub(crate) async fn provider_customer(
        &self,
        c: &Customer,
    ) -> Result<(Arc<dyn PaymentProvider>, crate::provider::CustomerRef)> {
        let name = match &c.provider {
            Some(p) => p.clone(),
            None => self.default_provider()?,
        };
        let p = self.provider(&name)?;
        let mut r = crate::provider::CustomerRef {
            customer_id: c.id.clone(),
            subject: c.subject.clone(),
            email: c.email.clone(),
            name: c.name.clone(),
            provider_customer_id: c
                .provider_customer_id
                .clone()
                .filter(|_| c.provider.as_deref() == Some(name.as_str())),
        };
        if r.provider_customer_id.is_none() {
            let pid = p.create_customer(&r).await?;
            let now = self.now();
            let mut tx = self.begin().await?;
            let res = async {
                let mut t = Tx(tx.as_mut());
                t.exec(
                    "UPDATE _forgebase_billing_customers SET provider = ?2, provider_customer_id = ?3, updated_at = ?4
                     WHERE id = ?1",
                    vec![c.id.clone().into(), name.clone().into(), pid.clone().into(), now.into()],
                )
                .await?;
                Ok(((), true))
            }
            .await;
            self.end(tx, res).await?;
            r.provider_customer_id = Some(pid);
        }
        Ok((p, r))
    }
}
