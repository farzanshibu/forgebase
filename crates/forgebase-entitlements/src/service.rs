//! [`EntitlementService`]: entitlement checks, quota metering, usage queries
//! and maintenance. Catalog and subject administration live in
//! [`crate::admin`].

use std::fmt;
use std::sync::{Arc, OnceLock, RwLock};

use forgebase_data::{AccessContext, Principal, SubjectRef};
use forgebase_db::{Backend, TxMode, Value, WriteTx};
use forgebase_jobs::{JobService, NewJob};
use forgebase_policy::{Check, PolicyService};
use serde_json::{Value as Json, json};

use crate::cache::{Key, UsageCache};
use crate::catalog::{Catalog, Resolved};
use crate::clock::Clock;
use crate::error::{EntitlementError, Result};
use crate::model::{
    Consumption, Decision, Entitlements, FeatureKind, GaugeRequest, HasAnswer, Lease, MaintenanceReport, QuotaScope,
    Reservation, ReservationStatus, UsageAlert, UsageEvent, UsageEventPage, UsageRequest, UsageSummary,
};
use crate::period::Period;
use crate::rate::RateCounters;
use crate::usage::{
    self, Alloc, Db, NewEvent, Quota, RES_COLUMNS, State, int, new_id, opt_int, opt_text, res_row, text,
};

/// Job kind delivering one usage alert.
pub const ALERT_JOB: &str = "entitlements.alert";
/// Job kind of the periodic maintenance (period resets, expiries).
pub const MAINTENANCE_JOB: &str = "entitlements.maintenance";
/// Name of the maintenance schedule.
pub const MAINTENANCE_SCHEDULE: &str = "entitlements-maintenance";

/// Longest reservation hold.
pub const MAX_TTL_SECS: i64 = 86_400;
/// Default reservation hold.
pub const DEFAULT_TTL_SECS: i64 = 300;

/// Receives usage alerts after they are raised (once per threshold and
/// window). The realtime engine (FB-40) can publish them; the default logs.
pub trait AlertSink: Send + Sync + fmt::Debug {
    /// Called by the alert job; at least once per alert.
    fn publish(&self, alert: &UsageAlert);
}

/// The default sink: a structured log line.
#[derive(Debug, Clone, Copy, Default)]
pub struct LogSink;

impl AlertSink for LogSink {
    fn publish(&self, alert: &UsageAlert) {
        tracing::info!(
            subject = %alert.subject,
            feature = %alert.feature,
            threshold = alert.threshold,
            used = alert.used,
            limit = alert.limit,
            "usage alert"
        );
    }
}

/// State shared with the pipeline hooks and the rate-limit gate.
#[derive(Debug)]
pub(crate) struct Shared {
    pub catalog: RwLock<Arc<Catalog>>,
    pub usage: UsageCache,
    pub rates: RateCounters,
    pub clock: Arc<dyn Clock>,
}

impl Shared {
    pub fn catalog(&self) -> Arc<Catalog> {
        self.catalog.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub fn now(&self) -> i64 {
        self.clock.now_ms()
    }
}

/// The subject key of a [`SubjectRef`]: `org:<id>` or the principal key.
pub fn subject_key(s: &SubjectRef) -> String {
    match s {
        SubjectRef::Org(id) => format!("org:{id}"),
        SubjectRef::Principal(k) => k.clone(),
    }
}

/// Validates a subject key; returns the organization id of org subjects.
pub fn parse_subject(s: &str) -> Result<Option<&str>> {
    let ok_chars = |t: &str| {
        !t.is_empty()
            && t.len() <= 190
            && t.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'/' | b':' | b'@'))
    };
    if let Some(org) = s.strip_prefix("org:")
        && ok_chars(org)
        && !org.contains(':')
    {
        return Ok(Some(org));
    }
    if (s.starts_with("user:") || s.starts_with("service:")) && ok_chars(s) {
        return Ok(None);
    }
    Err(EntitlementError::invalid(
        "invalid_subject",
        "subject must be org:<id>, user:<collection>/<id> or service:<id>",
    ))
}

pub(crate) fn validate_idem(key: Option<&str>) -> Result<String> {
    match key.map(str::trim) {
        Some(k) if !k.is_empty() && k.len() <= 256 => Ok(k.to_owned()),
        Some(_) => Err(EntitlementError::invalid(
            "invalid_idempotency_key",
            "idempotency_key must be 1-256 characters",
        )),
        None => Err(EntitlementError::invalid(
            "idempotency_key_required",
            "idempotency_key is required",
        )),
    }
}

/// What a committed transaction changed, published after the commit.
#[derive(Debug, Default)]
pub(crate) struct Effects {
    pub keys: Vec<(Key, State, Quota)>,
    pub packs: Vec<(String, String, i64)>,
    pub alerts: Vec<String>,
}

/// The entitlement and quota service. Cheap to clone.
#[derive(Clone)]
pub struct EntitlementService {
    pub(crate) inner: Arc<Inner>,
}

impl fmt::Debug for EntitlementService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EntitlementService").finish_non_exhaustive()
    }
}

pub(crate) struct Inner {
    pub db: Arc<dyn Backend>,
    pub policy: PolicyService,
    pub shared: Arc<Shared>,
    pub jobs: OnceLock<JobService>,
    pub sink: RwLock<Arc<dyn AlertSink>>,
    pub reload_lock: tokio::sync::Mutex<()>,
}

impl EntitlementService {
    /// Loads the catalog and current usage from a migrated database.
    /// Install the hooks with `policy.set_hooks(service.hooks())`.
    pub async fn open(db: Arc<dyn Backend>, policy: PolicyService, clock: Arc<dyn Clock>) -> Result<Self> {
        let catalog = Catalog::load(db.as_ref()).await?;
        let svc = Self {
            inner: Arc::new(Inner {
                db,
                policy,
                shared: Arc::new(Shared {
                    catalog: RwLock::new(Arc::new(catalog)),
                    usage: UsageCache::default(),
                    rates: RateCounters::default(),
                    clock,
                }),
                jobs: OnceLock::new(),
                sink: RwLock::new(Arc::new(LogSink)),
                reload_lock: tokio::sync::Mutex::new(()),
            }),
        };
        svc.warm().await?;
        Ok(svc)
    }

    /// Attaches the job queue (alert delivery, maintenance schedule).
    pub fn attach_jobs(&self, jobs: JobService) {
        let _ = self.inner.jobs.set(jobs);
    }

    /// Replaces the alert sink (FB-40 realtime publishes alerts).
    pub fn set_alert_sink(&self, sink: Arc<dyn AlertSink>) {
        *self.inner.sink.write().unwrap_or_else(|p| p.into_inner()) = sink;
    }

    /// The policy service.
    pub fn policy(&self) -> &PolicyService {
        &self.inner.policy
    }

    pub(crate) fn db(&self) -> &dyn Backend {
        self.inner.db.as_ref()
    }

    pub(crate) fn shared(&self) -> &Arc<Shared> {
        &self.inner.shared
    }

    pub(crate) fn now(&self) -> i64 {
        self.inner.shared.now()
    }

    pub(crate) fn catalog(&self) -> Arc<Catalog> {
        self.inner.shared.catalog()
    }

    /// Reloads the catalog (after any catalog or subject change).
    pub async fn reload(&self) -> Result<()> {
        let _guard = self.inner.reload_lock.lock().await;
        let c = Catalog::load(self.db()).await?;
        *self.inner.shared.catalog.write().unwrap_or_else(|p| p.into_inner()) = Arc::new(c);
        Ok(())
    }

    /// Loads current counters, leases and pack balances into the cache.
    async fn warm(&self) -> Result<()> {
        let now = self.now();
        let seq = self.inner.shared.usage.next_seq();
        let cache = &self.inner.shared.usage;
        let rows = self
            .db()
            .fetch_all(&forgebase_db::Statement::new(
                "SELECT feature_key, subject, scope_key, window_start, used, reserved, carried_in, window_end
                 FROM _forgebase_usage_counters WHERE window_end > ?1",
                vec![now.into()],
            ))
            .await?;
        for r in &rows.rows {
            cache.put(
                Key::new(&text(r, 0), &text(r, 1), &text(r, 2)),
                crate::cache::Entry {
                    window_start: int(r, 3),
                    consumed: int(r, 4) + int(r, 5),
                    carried: int(r, 6),
                    valid_until: int(r, 7),
                    ..Default::default()
                },
                seq,
            );
        }
        let rows = self
            .db()
            .fetch_all(&forgebase_db::Statement::new(
                "SELECT feature_key, subject, scope_key, expires_at FROM _forgebase_quota_leases
                 WHERE released_at IS NULL AND expires_at > ?1 ORDER BY feature_key, subject, scope_key",
                vec![now.into()],
            ))
            .await?;
        let mut leases: std::collections::BTreeMap<Key, Vec<i64>> = std::collections::BTreeMap::new();
        for r in &rows.rows {
            leases
                .entry(Key::new(&text(r, 0), &text(r, 1), &text(r, 2)))
                .or_default()
                .push(int(r, 3));
        }
        for (k, l) in leases {
            cache.put(
                k,
                crate::cache::Entry {
                    consumed: l.len() as i64,
                    leases: l,
                    valid_until: i64::MAX,
                    ..Default::default()
                },
                seq,
            );
        }
        let rows = self
            .db()
            .fetch_all(&forgebase_db::Statement::new(
                "SELECT feature_key, subject, COALESCE(SUM(remaining), 0) FROM _forgebase_pack_grants
                 WHERE remaining > 0 AND (expires_at IS NULL OR expires_at > ?1) GROUP BY feature_key, subject",
                vec![now.into()],
            ))
            .await?;
        for r in &rows.rows {
            cache.put_packs(&text(r, 0), &text(r, 1), int(r, 2), seq);
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Authorization
    // -----------------------------------------------------------------------

    pub(crate) fn require_admin(&self, ctx: &AccessContext) -> Result<()> {
        if ctx.is_privileged() {
            return Ok(());
        }
        if ctx.principal == Principal::Anonymous {
            return Err(EntitlementError::Unauthenticated);
        }
        Err(EntitlementError::Forbidden("platform administrators only".into()))
    }

    pub(crate) fn require_caller(&self, ctx: &AccessContext) -> Result<()> {
        if ctx.principal == Principal::Anonymous {
            return Err(EntitlementError::Unauthenticated);
        }
        Ok(())
    }

    /// The subject a request acts on: the given one, else the caller's.
    pub(crate) fn subject_for(&self, ctx: &AccessContext, given: Option<&str>) -> Result<String> {
        self.require_caller(ctx)?;
        let s = match given {
            Some(s) => s.trim().to_owned(),
            None => subject_key(&ctx.subject()),
        };
        if parse_subject(&s).is_err() && given.is_none() {
            return Err(EntitlementError::invalid(
                "subject_required",
                "name the subject (org:<id> or a principal key)",
            ));
        }
        parse_subject(&s)?;
        Ok(s)
    }

    /// Runs the permission pipeline for `permission` on `subject`: an org
    /// subject needs an active membership and the permission in that org; a
    /// personal subject the permission at realm level, unless `own` allows
    /// the caller's own personal subject.
    fn allow(&self, ctx: &AccessContext, subject: &str, permission: &str, own: bool) -> Result<()> {
        if ctx.is_privileged() {
            return Ok(());
        }
        self.require_caller(ctx)?;
        let org = parse_subject(subject)?;
        if own && org.is_none() && subject == ctx.principal_key() {
            return Ok(());
        }
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

    /// May see the subject's entitlements (members and the user themselves).
    pub(crate) fn can_view(&self, ctx: &AccessContext, subject: &str) -> Result<()> {
        self.allow(ctx, subject, "org:read", true)
    }

    /// May read the subject's usage and events (`billing:read`).
    pub(crate) fn can_read_usage(&self, ctx: &AccessContext, subject: &str) -> Result<()> {
        self.allow(ctx, subject, "billing:read", true)
    }

    /// May meter the subject (`usage:write`: trusted backends and service
    /// accounts; never implied for the subject itself, which could release
    /// its own reservations).
    pub(crate) fn can_meter(&self, ctx: &AccessContext, subject: &str) -> Result<()> {
        self.allow(ctx, subject, "usage:write", false)
    }

    // -----------------------------------------------------------------------
    // Entitlements
    // -----------------------------------------------------------------------

    fn member_of(ctx: &AccessContext) -> Option<String> {
        match ctx.principal {
            Principal::User { .. } | Principal::Service { .. } => Some(ctx.principal_key()),
            _ => None,
        }
    }

    pub(crate) fn resolved(&self, subject: &str, member: Option<&str>) -> Arc<Resolved> {
        self.catalog().resolve(subject, member, self.now())
    }

    /// Effective entitlements of a subject (default: the caller's) for the
    /// caller as member.
    pub fn entitlements(&self, ctx: &AccessContext, subject: Option<&str>) -> Result<Entitlements> {
        let subject = self.subject_for(ctx, subject)?;
        self.can_view(ctx, &subject)?;
        let member = Self::member_of(ctx).filter(|_| !ctx.is_privileged());
        Ok(self.entitlements_of(&subject, member.as_deref()))
    }

    /// Effective entitlements of `subject` for `member` (no authorization:
    /// trusted callers).
    pub fn entitlements_of(&self, subject: &str, member: Option<&str>) -> Entitlements {
        let r = self.resolved(subject, member);
        Entitlements {
            subject: subject.to_owned(),
            member: member.map(str::to_owned),
            plan: r.plan.clone(),
            status: r.status,
            entitled: r.entitled,
            trial_ends_at: r.trial_ends_at,
            features: r.grants.clone(),
        }
    }

    /// `has({feature})` for the caller's subject (or a named one).
    pub fn has_feature(&self, ctx: &AccessContext, feature: &str, subject: Option<&str>) -> Result<HasAnswer> {
        let subject = self.subject_for(ctx, subject)?;
        self.can_view(ctx, &subject)?;
        let member = Self::member_of(ctx).filter(|_| !ctx.is_privileged());
        let r = self.resolved(&subject, member.as_deref());
        Ok(if r.enabled(feature) {
            HasAnswer {
                has: true,
                reason: None,
            }
        } else {
            HasAnswer {
                has: false,
                reason: Some(format!("feature {feature} is not included in the plan")),
            }
        })
    }

    /// `has({plan})`: whether the caller's subject is on `plan`.
    pub fn has_plan(&self, ctx: &AccessContext, plan: &str, subject: Option<&str>) -> Result<HasAnswer> {
        let subject = self.subject_for(ctx, subject)?;
        self.can_view(ctx, &subject)?;
        let r = self.resolved(&subject, None);
        let has = r.plan.as_deref() == Some(plan);
        Ok(HasAnswer {
            has,
            reason: (!has).then(|| format!("the plan is not {plan}")),
        })
    }

    /// `has({permission})` in the caller's active organization (or personal
    /// scope): the role permission (FB-21) and, when a feature gates the
    /// permission, that feature in the subject's plan.
    pub fn has_permission(&self, ctx: &AccessContext, permission: &str) -> Result<HasAnswer> {
        self.require_caller(ctx)?;
        if forgebase_policy::perm::validate(permission).is_err() {
            return Err(EntitlementError::invalid("invalid_permission", "not a permission key"));
        }
        if !self.inner.policy.has(ctx, permission, ctx.org_id.as_deref()) {
            return Ok(HasAnswer {
                has: false,
                reason: Some(format!("missing permission {permission}")),
            });
        }
        if ctx.is_privileged() {
            return Ok(HasAnswer {
                has: true,
                reason: None,
            });
        }
        let subject = subject_key(&ctx.subject());
        let cat = self.catalog();
        let member = Self::member_of(ctx);
        let r = cat.resolve(&subject, member.as_deref(), self.now());
        for f in cat.gating(permission) {
            if !r.enabled(f) {
                return Ok(HasAnswer {
                    has: false,
                    reason: Some(format!("feature {f} is not included in the plan")),
                });
            }
        }
        Ok(HasAnswer {
            has: true,
            reason: None,
        })
    }

    // -----------------------------------------------------------------------
    // Quota resolution
    // -----------------------------------------------------------------------

    /// The quota of `feature` for `subject`. `strict`: the feature must be
    /// enabled (metering); otherwise a disabled feature resolves with a zero
    /// limit (settling old reservations, summaries).
    pub(crate) fn quota(
        &self,
        cat: &Catalog,
        ctx: &AccessContext,
        subject: &str,
        feature: &str,
        scope_key: Option<&str>,
        strict: bool,
    ) -> Result<Quota> {
        let f = cat
            .features
            .get(feature)
            .ok_or_else(|| EntitlementError::NotFound(format!("feature {feature}")))?;
        let policy = cat.policy(feature);
        let caller = Self::member_of(ctx).filter(|_| !ctx.is_privileged());
        let scope_key = match policy.scope {
            QuotaScope::Subject => String::new(),
            QuotaScope::User => match scope_key.map(str::to_owned).or_else(|| caller.clone()) {
                Some(k) => k,
                None => {
                    return Err(EntitlementError::invalid(
                        "scope_key_required",
                        format!("{feature} is counted per user: name the user in scope_key"),
                    ));
                }
            },
            QuotaScope::Team | QuotaScope::Resource => match scope_key {
                Some(k) if !k.trim().is_empty() => k.trim().to_owned(),
                _ => {
                    return Err(EntitlementError::invalid(
                        "scope_key_required",
                        format!(
                            "{feature} is counted per {}: name it in scope_key",
                            policy.scope.as_str()
                        ),
                    ));
                }
            },
        };
        if scope_key.len() > 200 {
            return Err(EntitlementError::invalid("invalid_scope_key", "scope_key is too long"));
        }
        let member = if policy.scope == QuotaScope::User {
            Some(scope_key.clone())
        } else {
            caller
        };
        let r = cat.resolve(subject, member.as_deref(), self.now());
        let grant = r.grant(feature).filter(|g| g.enabled);
        if strict && grant.is_none() {
            return Err(EntitlementError::FeatureDisabled {
                feature: feature.to_owned(),
                subject: subject.to_owned(),
            });
        }
        let (limit, mut period, window_secs) = match grant {
            Some(g) => (g.limit, g.period, g.window_secs),
            None => (Some(0), Period::None, None),
        };
        match f.kind {
            FeatureKind::Metered if matches!(period, Period::None) => period = Period::Lifetime,
            FeatureKind::Metered => {}
            _ => period = Period::None,
        }
        Ok(Quota {
            key: Key::new(feature, subject, &scope_key),
            kind: f.kind,
            limit,
            period,
            window_secs,
            anchor: r.cycle_anchor,
            policy,
        })
    }

    fn need_kind(q: &Quota, kinds: &[FeatureKind], what: &str) -> Result<()> {
        if kinds.contains(&q.kind) {
            Ok(())
        } else {
            Err(EntitlementError::invalid(
                "wrong_feature_kind",
                format!("{} is a {} feature: {what}", q.key.feature, q.kind.as_str()),
            ))
        }
    }

    // -----------------------------------------------------------------------
    // Transactions
    // -----------------------------------------------------------------------

    pub(crate) async fn begin(&self) -> Result<(Box<dyn WriteTx>, u64)> {
        let tx = self.db().begin(TxMode::Normal).await?;
        // Taken while holding the single writer: follows commit order.
        let seq = self.inner.shared.usage.next_seq();
        Ok((tx, seq))
    }

    /// Commits and publishes what the transaction changed.
    pub(crate) async fn finish(&self, tx: Box<dyn WriteTx>, seq: u64, fx: Effects) -> Result<()> {
        tx.commit().await?;
        let cache = &self.inner.shared.usage;
        for (k, st, q) in fx.keys {
            cache.put(k, st.entry(&q), seq);
        }
        for (f, s, n) in fx.packs {
            cache.put_packs(&f, &s, n, seq);
        }
        for id in fx.alerts {
            self.enqueue_alert(&id).await;
        }
        Ok(())
    }

    async fn enqueue_alert(&self, id: &str) {
        let Some(jobs) = self.inner.jobs.get() else {
            tracing::debug!(alert = id, "no job queue attached; alert stays pending");
            return;
        };
        let job = NewJob {
            kind: ALERT_JOB.to_owned(),
            payload: json!({ "alert_id": id }),
            queue: None,
            priority: Some(10),
            run_at: None,
            delay_ms: None,
            max_attempts: Some(10),
            backoff_base_ms: None,
            backoff_max_ms: None,
            visibility_timeout_ms: None,
            idempotency_key: Some(format!("alert:{id}")),
            ..NewJob::default()
        };
        if let Err(e) = jobs.enqueue(&AccessContext::system(), job).await {
            tracing::warn!(error = %e, alert = id, "could not enqueue the usage alert; maintenance retries");
        }
    }

    // -----------------------------------------------------------------------
    // Metering
    // -----------------------------------------------------------------------

    fn usage_subject(&self, ctx: &AccessContext, given: Option<&str>) -> Result<String> {
        let s = self.subject_for(ctx, given)?;
        self.can_meter(ctx, &s)?;
        Ok(s)
    }

    /// Dry run: would `units` of the feature be granted now? No side effects.
    pub async fn check(&self, ctx: &AccessContext, req: &UsageRequest) -> Result<Decision> {
        let subject = self.subject_for(ctx, req.subject.as_deref())?;
        self.can_view(ctx, &subject)?;
        if req.units < 1 {
            return Err(EntitlementError::invalid("invalid_units", "units must be at least 1"));
        }
        let cat = self.catalog();
        let now = self.now();
        let q = match self.quota(&cat, ctx, &subject, &req.feature, req.scope_key.as_deref(), true) {
            Ok(q) => q,
            Err(EntitlementError::FeatureDisabled { feature, .. }) => {
                let q = self.quota(&cat, ctx, &subject, &req.feature, req.scope_key.as_deref(), false)?;
                let st = State::default();
                return Ok(usage::decision(
                    &q,
                    &st,
                    req.units,
                    Alloc::default(),
                    Some(format!("feature {feature} is not included in the plan")),
                ));
            }
            Err(e) => return Err(e),
        };
        let mut db = Db::Ro(self.db());
        let st = usage::load_state(&mut db, &q, now).await?;
        Ok(match usage::allocate(&q, &st, req.units) {
            Ok(a) => usage::decision(&q, &st, req.units, a, None),
            Err(why) => usage::decision(&q, &st, req.units, Alloc::default(), Some(why)),
        })
    }

    /// Records `units` of usage at once (reserve and commit in one
    /// transaction). Idempotent by `idempotency_key`.
    pub async fn consume(&self, ctx: &AccessContext, req: UsageRequest) -> Result<Consumption> {
        let subject = self.usage_subject(ctx, req.subject.as_deref())?;
        let idem = validate_idem(req.idempotency_key.as_deref())?;
        if req.units < 1 {
            return Err(EntitlementError::invalid("invalid_units", "units must be at least 1"));
        }
        let cat = self.catalog();
        let q = self.quota(&cat, ctx, &subject, &req.feature, req.scope_key.as_deref(), true)?;
        Self::need_kind(&q, &[FeatureKind::Metered], "use the gauge or lease API")?;
        let now = self.now();
        let (mut tx, seq) = self.begin().await?;
        let res = {
            let mut db = Db::Tx(tx.as_mut());
            consume_tx(&mut db, &q, &req, &idem, ctx, now).await
        };
        match res {
            Ok(Txn::Done(c, fx)) => {
                self.finish(tx, seq, fx).await?;
                Ok(c)
            }
            Ok(Txn::Replay(c)) => {
                tx.rollback().await?;
                Ok(c)
            }
            Err(e) => {
                let _ = tx.rollback().await;
                Err(e)
            }
        }
    }

    /// Holds `units` until committed or released (or the hold expires).
    pub async fn reserve(&self, ctx: &AccessContext, req: UsageRequest) -> Result<Reservation> {
        let subject = self.usage_subject(ctx, req.subject.as_deref())?;
        let idem = validate_idem(req.idempotency_key.as_deref())?;
        if req.units < 1 {
            return Err(EntitlementError::invalid("invalid_units", "units must be at least 1"));
        }
        let ttl = req.ttl_secs.unwrap_or(DEFAULT_TTL_SECS);
        if !(1..=MAX_TTL_SECS).contains(&ttl) {
            return Err(EntitlementError::invalid(
                "invalid_ttl",
                format!("ttl_secs must be 1-{MAX_TTL_SECS}"),
            ));
        }
        let cat = self.catalog();
        let q = self.quota(&cat, ctx, &subject, &req.feature, req.scope_key.as_deref(), true)?;
        Self::need_kind(&q, &[FeatureKind::Metered], "use the gauge or lease API")?;
        let now = self.now();
        let (mut tx, seq) = self.begin().await?;
        let res = {
            let mut db = Db::Tx(tx.as_mut());
            reserve_tx(&mut db, &q, &req, &idem, ctx, now, ttl).await
        };
        match res {
            Ok(Txn::Done(r, fx)) => {
                self.finish(tx, seq, fx).await?;
                Ok(r)
            }
            Ok(Txn::Replay(r)) => {
                tx.rollback().await?;
                Ok(r)
            }
            Err(e) => {
                let _ = tx.rollback().await;
                Err(e)
            }
        }
    }

    async fn reservation_row(&self, id: &str) -> Result<usage::ResRow> {
        let sql = format!("SELECT {RES_COLUMNS} FROM _forgebase_usage_reservations WHERE id = ?1");
        let rows = self
            .db()
            .fetch_all(&forgebase_db::Statement::new(sql, vec![id.into()]))
            .await?;
        rows.rows
            .first()
            .map(|r| res_row(r))
            .ok_or_else(|| EntitlementError::NotFound("reservation".into()))
    }

    /// Settles a reservation: `Some(units)` commits that much (at most the
    /// reserved units; the rest is released), `None` commits all of it.
    pub async fn commit(&self, ctx: &AccessContext, id: &str, units: Option<i64>) -> Result<Reservation> {
        self.settle(ctx, id, Some(units)).await
    }

    /// Gives a held reservation back.
    pub async fn release(&self, ctx: &AccessContext, id: &str) -> Result<Reservation> {
        self.settle(ctx, id, None).await
    }

    async fn settle(&self, ctx: &AccessContext, id: &str, commit: Option<Option<i64>>) -> Result<Reservation> {
        self.require_caller(ctx)?;
        let row = self.reservation_row(id).await?;
        // A reservation of a subject the caller cannot meter does not exist for them.
        if let Err(e) = self.can_meter(ctx, &row.subject) {
            return Err(match e {
                EntitlementError::Unauthenticated => e,
                _ => EntitlementError::NotFound("reservation".into()),
            });
        }
        let cat = self.catalog();
        let q = self.quota(
            &cat,
            &AccessContext::system(),
            &row.subject,
            &row.feature,
            Some(&row.scope_key),
            false,
        )?;
        let now = self.now();
        let (mut tx, seq) = self.begin().await?;
        let res = {
            let mut db = Db::Tx(tx.as_mut());
            settle_tx(&mut db, &q, id, commit, ctx, now).await
        };
        match res {
            Ok(Txn::Done(r, fx)) => {
                self.finish(tx, seq, fx).await?;
                Ok(r)
            }
            Ok(Txn::Replay(r)) => {
                tx.rollback().await?;
                Ok(r)
            }
            Err(e) => {
                let _ = tx.rollback().await;
                Err(e)
            }
        }
    }

    /// Acquires a concurrency lease (`ttl_secs`, default the policy's).
    pub async fn acquire_lease(&self, ctx: &AccessContext, req: UsageRequest) -> Result<Lease> {
        let subject = self.usage_subject(ctx, req.subject.as_deref())?;
        let idem = validate_idem(req.idempotency_key.as_deref())?;
        let cat = self.catalog();
        let q = self.quota(&cat, ctx, &subject, &req.feature, req.scope_key.as_deref(), true)?;
        Self::need_kind(&q, &[FeatureKind::Concurrency], "leases are for concurrency features")?;
        let ttl = req.ttl_secs.unwrap_or(q.policy.lease_ttl_secs);
        if !(1..=MAX_TTL_SECS).contains(&ttl) {
            return Err(EntitlementError::invalid(
                "invalid_ttl",
                format!("ttl_secs must be 1-{MAX_TTL_SECS}"),
            ));
        }
        let now = self.now();
        let (mut tx, seq) = self.begin().await?;
        let res = {
            let mut db = Db::Tx(tx.as_mut());
            lease_tx(&mut db, &q, &idem, ctx, now, ttl).await
        };
        match res {
            Ok(Txn::Done(l, fx)) => {
                self.finish(tx, seq, fx).await?;
                Ok(l)
            }
            Ok(Txn::Replay(l)) => {
                tx.rollback().await?;
                Ok(l)
            }
            Err(e) => {
                let _ = tx.rollback().await;
                Err(e)
            }
        }
    }

    async fn lease_row(&self, id: &str) -> Result<(String, String, String, i64, Option<i64>)> {
        let rows = self
            .db()
            .fetch_all(&forgebase_db::Statement::new(
                "SELECT feature_key, subject, scope_key, expires_at, released_at FROM _forgebase_quota_leases WHERE id = ?1",
                vec![id.into()],
            ))
            .await?;
        let r = rows
            .rows
            .first()
            .ok_or_else(|| EntitlementError::NotFound("lease".into()))?;
        Ok((text(r, 0), text(r, 1), text(r, 2), int(r, 3), opt_int(r, 4)))
    }

    /// Extends an active lease by `ttl_secs` from now.
    pub async fn renew_lease(&self, ctx: &AccessContext, id: &str, ttl_secs: Option<i64>) -> Result<Lease> {
        self.lease_change(ctx, id, Some(ttl_secs)).await
    }

    /// Releases a lease.
    pub async fn release_lease(&self, ctx: &AccessContext, id: &str) -> Result<Lease> {
        self.lease_change(ctx, id, None).await
    }

    async fn lease_change(&self, ctx: &AccessContext, id: &str, renew: Option<Option<i64>>) -> Result<Lease> {
        self.require_caller(ctx)?;
        let (feature, subject, scope_key, _, _) = self.lease_row(id).await?;
        if let Err(e) = self.can_meter(ctx, &subject) {
            return Err(match e {
                EntitlementError::Unauthenticated => e,
                _ => EntitlementError::NotFound("lease".into()),
            });
        }
        let cat = self.catalog();
        let q = self.quota(
            &cat,
            &AccessContext::system(),
            &subject,
            &feature,
            Some(&scope_key),
            false,
        )?;
        let now = self.now();
        let (mut tx, seq) = self.begin().await?;
        let res: Result<(Lease, Effects)> = async {
            let mut db = Db::Tx(tx.as_mut());
            let rows = db
                .fetch(
                    "SELECT expires_at, released_at FROM _forgebase_quota_leases WHERE id = ?1",
                    vec![id.into()],
                )
                .await?;
            let r = rows
                .rows
                .first()
                .ok_or_else(|| EntitlementError::NotFound("lease".into()))?;
            let (expires, released) = (int(r, 0), opt_int(r, 1));
            let expires_at = match renew {
                Some(ttl) => {
                    if released.is_some() || expires <= now {
                        return Err(EntitlementError::conflict(
                            "lease_expired",
                            "the lease is no longer active",
                        ));
                    }
                    let ttl = ttl.unwrap_or(q.policy.lease_ttl_secs);
                    if !(1..=MAX_TTL_SECS).contains(&ttl) {
                        return Err(EntitlementError::invalid(
                            "invalid_ttl",
                            format!("ttl_secs must be 1-{MAX_TTL_SECS}"),
                        ));
                    }
                    let until = now + ttl * 1000;
                    db.exec(
                        "UPDATE _forgebase_quota_leases SET expires_at = ?1 WHERE id = ?2",
                        vec![until.into(), id.into()],
                    )
                    .await?;
                    until
                }
                None => {
                    if released.is_none() {
                        db.exec(
                            "UPDATE _forgebase_quota_leases SET released_at = ?1 WHERE id = ?2",
                            vec![now.into(), id.into()],
                        )
                        .await?;
                    }
                    expires
                }
            };
            let st = usage::load_state(&mut db, &q, now).await?;
            let lease = Lease {
                id: id.to_owned(),
                feature: feature.clone(),
                subject: subject.clone(),
                scope_key: scope_key.clone(),
                expires_at,
                released: renew.is_none(),
                replayed: false,
            };
            Ok((
                lease,
                Effects {
                    keys: vec![(q.key.clone(), st, q.clone())],
                    ..Effects::default()
                },
            ))
        }
        .await;
        match res {
            Ok((l, fx)) => {
                self.finish(tx, seq, fx).await?;
                Ok(l)
            }
            Err(e) => {
                let _ = tx.rollback().await;
                Err(e)
            }
        }
    }

    /// Changes a gauge (storage, seats) by `delta` or to `value`; increases
    /// are checked against the limit. Idempotent by `idempotency_key`.
    pub async fn adjust_gauge(&self, ctx: &AccessContext, req: GaugeRequest) -> Result<Consumption> {
        let subject = self.usage_subject(ctx, req.subject.as_deref())?;
        let idem = validate_idem(req.idempotency_key.as_deref())?;
        if req.delta.is_some() == req.value.is_some() {
            return Err(EntitlementError::invalid(
                "invalid_gauge_change",
                "give exactly one of delta and value",
            ));
        }
        if req.value.is_some_and(|v| v < 0) {
            return Err(EntitlementError::invalid(
                "invalid_gauge_change",
                "value must not be negative",
            ));
        }
        let cat = self.catalog();
        let q = self.quota(&cat, ctx, &subject, &req.feature, req.scope_key.as_deref(), true)?;
        Self::need_kind(&q, &[FeatureKind::Gauge], "use consume or reserve")?;
        let now = self.now();
        let (mut tx, seq) = self.begin().await?;
        let res = {
            let mut db = Db::Tx(tx.as_mut());
            gauge_tx(&mut db, &q, &req, &idem, ctx, now).await
        };
        match res {
            Ok(Txn::Done(c, fx)) => {
                self.finish(tx, seq, fx).await?;
                Ok(c)
            }
            Ok(Txn::Replay(c)) => {
                tx.rollback().await?;
                Ok(c)
            }
            Err(e) => {
                let _ = tx.rollback().await;
                Err(e)
            }
        }
    }

    // -----------------------------------------------------------------------
    // Queries
    // -----------------------------------------------------------------------

    /// Current usage of every metered, gauge and concurrency feature of a
    /// subject. Per-user quotas show the caller's counter (or `scope_key`'s);
    /// team and resource quotas only with `scope_key`.
    pub async fn usage(
        &self,
        ctx: &AccessContext,
        subject: Option<&str>,
        scope_key: Option<&str>,
    ) -> Result<Vec<UsageSummary>> {
        let subject = self.subject_for(ctx, subject)?;
        self.can_read_usage(ctx, &subject)?;
        let cat = self.catalog();
        let now = self.now();
        let member = Self::member_of(ctx).filter(|_| !ctx.is_privileged());
        let r = cat.resolve(&subject, member.as_deref(), now);
        let mut out = Vec::new();
        for (feature, g) in &r.grants {
            if !matches!(
                g.kind,
                FeatureKind::Metered | FeatureKind::Gauge | FeatureKind::Concurrency
            ) {
                continue;
            }
            let policy = cat.policy(feature);
            let key = match policy.scope {
                QuotaScope::Subject => None,
                QuotaScope::User => scope_key.map(str::to_owned).or_else(|| member.clone()),
                QuotaScope::Team | QuotaScope::Resource => scope_key.map(str::to_owned),
            };
            if policy.scope != QuotaScope::Subject && key.is_none() {
                continue;
            }
            let q = self.quota(&cat, ctx, &subject, feature, key.as_deref(), false)?;
            let mut db = Db::Ro(self.db());
            let st = usage::load_state(&mut db, &q, now).await?;
            let cap = st.cap(&q);
            out.push(UsageSummary {
                feature: feature.clone(),
                kind: g.kind,
                scope_key: q.key.scope_key.clone(),
                enabled: g.enabled,
                limit: cap,
                carried_in: st.carried,
                used: st.used,
                reserved: st.reserved,
                remaining: cap.map(|c| (c - st.used - st.reserved).max(0) + st.packs),
                pack_remaining: st.packs,
                percent: cap.filter(|c| *c > 0).map(|c| (st.used as f64) * 100.0 / (c as f64)),
                enforcement: q.policy.enforcement,
                period: q.period,
                window_start: st.window.start,
                window_end: q.period.is_calendar().then_some(st.window.end),
            });
        }
        Ok(out)
    }

    /// Usage events in `seq` order (FB-70 reads these for billing).
    pub async fn events(
        &self,
        ctx: &AccessContext,
        subject: Option<&str>,
        feature: Option<&str>,
        after: Option<i64>,
        limit: Option<u32>,
    ) -> Result<UsageEventPage> {
        let subject = match subject {
            None if ctx.is_privileged() => None,
            s => Some(self.subject_for(ctx, s)?),
        };
        if let Some(s) = &subject {
            self.can_read_usage(ctx, s)?;
        }
        let limit = i64::from(limit.unwrap_or(100).clamp(1, 1000));
        let rows = self
            .db()
            .fetch_all(&forgebase_db::Statement::new(
                "SELECT seq, id, at, subject, feature_key, scope_key, units, pack_units, overage_units, window_start,
                        source, reservation_id, idempotency_key, actor, request_id, metadata
                 FROM _forgebase_usage_events
                 WHERE seq > ?1 AND (CAST(?2 AS TEXT) IS NULL OR subject = ?2) AND (CAST(?3 AS TEXT) IS NULL OR feature_key = ?3)
                 ORDER BY seq LIMIT ?4",
                vec![
                    after.unwrap_or(0).into(),
                    subject.into(),
                    feature.map(str::to_owned).into(),
                    (limit + 1).into(),
                ],
            ))
            .await?;
        let mut items: Vec<UsageEvent> = rows
            .rows
            .iter()
            .map(|r| UsageEvent {
                seq: int(r, 0),
                id: text(r, 1),
                at: int(r, 2),
                subject: text(r, 3),
                feature: text(r, 4),
                scope_key: text(r, 5),
                units: int(r, 6),
                pack_units: int(r, 7),
                overage_units: int(r, 8),
                window_start: int(r, 9),
                source: text(r, 10),
                reservation_id: opt_text(r, 11),
                idempotency_key: text(r, 12),
                actor: text(r, 13),
                request_id: opt_text(r, 14),
                metadata: serde_json::from_str(&text(r, 15)).unwrap_or(Json::Null),
            })
            .collect();
        let next = if items.len() as i64 > limit {
            items.truncate(limit as usize);
            items.last().map(|e| e.seq)
        } else {
            None
        };
        Ok(UsageEventPage { items, next })
    }

    /// Alerts of a subject, newest first.
    pub async fn alerts(&self, ctx: &AccessContext, subject: Option<&str>) -> Result<Vec<UsageAlert>> {
        let subject = self.subject_for(ctx, subject)?;
        self.can_read_usage(ctx, &subject)?;
        let rows = self
            .db()
            .fetch_all(&forgebase_db::Statement::new(
                "SELECT id, at, subject, feature_key, scope_key, window_start, threshold, used, limit_value, delivered_at
                 FROM _forgebase_usage_alerts WHERE subject = ?1 ORDER BY at DESC, threshold DESC LIMIT 500",
                vec![subject.into()],
            ))
            .await?;
        Ok(rows.rows.iter().map(|r| alert_row(r)).collect())
    }

    // -----------------------------------------------------------------------
    // Jobs
    // -----------------------------------------------------------------------

    /// Delivers one alert: publishes it to the sink, records it in the audit
    /// log and marks it delivered. A delivered alert is not published again.
    pub async fn deliver_alert(&self, id: &str) -> Result<bool> {
        let rows = self
            .db()
            .fetch_all(&forgebase_db::Statement::new(
                "SELECT id, at, subject, feature_key, scope_key, window_start, threshold, used, limit_value, delivered_at
                 FROM _forgebase_usage_alerts WHERE id = ?1",
                vec![id.into()],
            ))
            .await?;
        let Some(mut alert) = rows.rows.first().map(|r| alert_row(r)) else {
            return Err(EntitlementError::NotFound("alert".into()));
        };
        if alert.delivered_at.is_some() {
            return Ok(false);
        }
        let sink = self.inner.sink.read().unwrap_or_else(|p| p.into_inner()).clone();
        sink.publish(&alert);
        let now = self.now();
        let mut tx = self.db().begin(TxMode::Normal).await?;
        let n = tx
            .execute(&forgebase_db::Statement::new(
                "UPDATE _forgebase_usage_alerts SET delivered_at = ?1 WHERE id = ?2 AND delivered_at IS NULL",
                vec![now.into(), id.into()],
            ))
            .await?
            .rows_affected;
        if n == 1 {
            alert.delivered_at = Some(now);
            crate::admin::audit_tx(
                tx.as_mut(),
                now,
                "system",
                "usage_alert",
                Some(&alert.subject),
                &serde_json::to_value(&alert)?,
            )
            .await?;
        }
        tx.commit().await?;
        Ok(n == 1)
    }

    /// Period resets and expiries (the `entitlements.maintenance` job; tests
    /// call it with an injected clock):
    ///
    /// * releases expired reservations and closes expired leases,
    /// * ends trials past their end without conversion (status `expired`),
    /// * opens the current window of every subscribed subject's rollover
    ///   quota so unused allowance of an idle window still rolls over,
    /// * queues alerts whose delivery job was lost.
    pub async fn run_maintenance(&self) -> Result<MaintenanceReport> {
        let now = self.now();
        let mut report = MaintenanceReport::default();
        // Expired reservations, in batches.
        loop {
            let sql = format!(
                "SELECT {RES_COLUMNS} FROM _forgebase_usage_reservations
                 WHERE status = 'held' AND expires_at <= ?1 ORDER BY expires_at LIMIT 200"
            );
            let rows = self
                .db()
                .fetch_all(&forgebase_db::Statement::new(sql, vec![now.into()]))
                .await?;
            if rows.is_empty() {
                break;
            }
            let cat = self.catalog();
            let (mut tx, seq) = self.begin().await?;
            let mut fx = Effects::default();
            let res: Result<()> = async {
                let mut db = Db::Tx(tx.as_mut());
                for r in rows.rows.iter().map(|r| res_row(r)) {
                    let Ok(q) = self.quota(
                        &cat,
                        &AccessContext::system(),
                        &r.subject,
                        &r.feature,
                        Some(&r.scope_key),
                        false,
                    ) else {
                        // The feature was deleted: no counter to correct.
                        db.exec(
                            "UPDATE _forgebase_usage_reservations SET status = 'expired', settled_at = ?1 WHERE id = ?2",
                            vec![now.into(), r.id.clone().into()],
                        )
                        .await?;
                        usage::return_packs(&mut db, &r.packs, r.pack_units).await?;
                        report.reservations_expired += 1;
                        continue;
                    };
                    usage::settle_back(&mut db, &q, &r, "expired", now).await?;
                    let st = usage::load_state(&mut db, &q, now).await?;
                    fx.packs.push((
                        r.feature.clone(),
                        r.subject.clone(),
                        usage::pack_balance(&mut db, &r.feature, &r.subject, now).await?,
                    ));
                    fx.keys.push((q.key.clone(), st, q));
                    report.reservations_expired += 1;
                }
                Ok(())
            }
            .await;
            match res {
                Ok(()) => self.finish(tx, seq, fx).await?,
                Err(e) => {
                    let _ = tx.rollback().await;
                    return Err(e);
                }
            }
        }
        // Expired leases.
        let mut tx = self.db().begin(TxMode::Normal).await?;
        report.leases_expired = tx
            .execute(&forgebase_db::Statement::new(
                "UPDATE _forgebase_quota_leases SET released_at = expires_at WHERE released_at IS NULL AND expires_at <= ?1",
                vec![now.into()],
            ))
            .await?
            .rows_affected;
        // Trials that ended.
        let ended = tx
            .fetch_all(&forgebase_db::Statement::new(
                "SELECT subject FROM _forgebase_subscriptions WHERE status = 'trialing' AND trial_ends_at IS NOT NULL AND trial_ends_at <= ?1",
                vec![now.into()],
            ))
            .await?;
        for r in &ended.rows {
            let subject = text(r, 0);
            tx.execute(&forgebase_db::Statement::new(
                "UPDATE _forgebase_subscriptions SET status = 'expired', updated_at = ?1 WHERE subject = ?2",
                vec![now.into(), subject.clone().into()],
            ))
            .await?;
            crate::admin::audit_tx(tx.as_mut(), now, "system", "trial_expired", Some(&subject), &json!({})).await?;
            report.trials_expired += 1;
        }
        tx.commit().await?;
        if report.trials_expired > 0 {
            self.reload().await?;
        }
        // Open current windows of rollover quotas for subscribed subjects.
        let cat = self.catalog();
        let rollover: Vec<&str> = cat
            .policies
            .values()
            .filter(|p| p.rollover_pct > 0 && p.scope == QuotaScope::Subject)
            .map(|p| p.feature.as_str())
            .collect();
        if !rollover.is_empty() {
            let subjects: Vec<String> = cat.subscriptions.keys().cloned().collect();
            for chunk in subjects.chunks(200) {
                let (mut tx, seq) = self.begin().await?;
                let mut fx = Effects::default();
                let mut opened = 0u64;
                let res: Result<()> = async {
                    let mut db = Db::Tx(tx.as_mut());
                    for subject in chunk {
                        for feature in &rollover {
                            let Ok(q) = self.quota(&cat, &AccessContext::system(), subject, feature, None, true) else {
                                continue;
                            };
                            if !q.period.is_calendar() || q.limit.is_none() {
                                continue;
                            }
                            let mut st = usage::load_state(&mut db, &q, now).await?;
                            if !st.exists {
                                usage::ensure_row(&mut db, &q, &mut st, now).await?;
                                opened += 1;
                                fx.keys.push((q.key.clone(), st, q));
                            }
                        }
                    }
                    if opened > 0 {
                        crate::admin::audit_db(
                            &mut db,
                            now,
                            "system",
                            "period_reset",
                            None,
                            &json!({ "windows_opened": opened }),
                        )
                        .await?;
                    }
                    Ok(())
                }
                .await;
                match res {
                    Ok(()) => self.finish(tx, seq, fx).await?,
                    Err(e) => {
                        let _ = tx.rollback().await;
                        return Err(e);
                    }
                }
                report.windows_opened += opened;
            }
        }
        // Alerts whose delivery job was lost (raised over a minute ago).
        let pending = self
            .db()
            .fetch_all(&forgebase_db::Statement::new(
                "SELECT id FROM _forgebase_usage_alerts WHERE delivered_at IS NULL AND at <= ?1 ORDER BY at LIMIT 500",
                vec![(now - 60_000).into()],
            ))
            .await?;
        for r in &pending.rows {
            self.enqueue_alert(&text(r, 0)).await;
            report.alerts_requeued += 1;
        }
        Ok(report)
    }
}

pub(crate) fn alert_row(r: &[Value]) -> UsageAlert {
    UsageAlert {
        id: text(r, 0),
        at: int(r, 1),
        subject: text(r, 2),
        feature: text(r, 3),
        scope_key: text(r, 4),
        window_start: int(r, 5),
        threshold: int(r, 6),
        used: int(r, 7),
        limit: int(r, 8),
        delivered_at: opt_int(r, 9),
    }
}

/// Outcome of a metering transaction.
pub(crate) enum Txn<T> {
    /// Changed something: commit and publish.
    Done(T, Effects),
    /// Replay of an earlier request: nothing changed.
    Replay(T),
}

fn actor(ctx: &AccessContext) -> String {
    ctx.principal_key()
}

async fn consume_tx(
    db: &mut Db<'_>,
    q: &Quota,
    req: &UsageRequest,
    idem: &str,
    ctx: &AccessContext,
    now: i64,
) -> Result<Txn<Consumption>> {
    let prior = db
        .fetch(
            "SELECT id, feature_key, scope_key, units, source, result FROM _forgebase_usage_events
             WHERE subject = ?1 AND idempotency_key = ?2",
            vec![q.key.subject.clone().into(), idem.into()],
        )
        .await?;
    if let Some(r) = prior.rows.first() {
        if text(r, 1) != q.key.feature
            || text(r, 2) != q.key.scope_key
            || int(r, 3) != req.units
            || text(r, 4) != "consume"
        {
            return Err(EntitlementError::conflict(
                "idempotency_conflict",
                "this idempotency key was used for a different request",
            ));
        }
        let decision: Decision = serde_json::from_str(&text(r, 5))?;
        return Ok(Txn::Replay(Consumption {
            event_id: text(r, 0),
            replayed: true,
            decision,
        }));
    }
    usage::reap_key(db, q, now).await?;
    let mut st = usage::load_state(db, q, now).await?;
    let alloc = match usage::allocate(q, &st, req.units) {
        Ok(a) => a,
        Err(why) => {
            return Err(EntitlementError::QuotaExceeded(Box::new(usage::decision(
                q,
                &st,
                req.units,
                Alloc::default(),
                Some(why),
            ))));
        }
    };
    let before = st.clone();
    usage::ensure_row(db, q, &mut st, now).await?;
    usage::take_packs(db, q, alloc.pack_units, now).await?;
    if !usage::bump(
        db,
        q,
        st.window.start,
        alloc.window_units,
        0,
        usage::ceiling(q, &st),
        now,
    )
    .await?
    {
        return Err(EntitlementError::QuotaExceeded(Box::new(usage::decision(
            q,
            &st,
            req.units,
            Alloc::default(),
            Some(format!("quota exhausted for {}", q.key.feature)),
        ))));
    }
    st.used += alloc.window_units;
    st.packs -= alloc.pack_units;
    let d = usage::decision(q, &st, req.units, alloc, None);
    let (event_id, _) = usage::insert_event(
        db,
        NewEvent {
            q,
            at: now,
            units: req.units,
            pack_units: alloc.pack_units,
            overage: alloc.overage,
            window_start: st.window.start,
            source: "consume",
            reservation_id: None,
            idempotency_key: idem.to_owned(),
            actor: actor(ctx),
            request_id: ctx.request_id.clone(),
            metadata: req.metadata.clone().unwrap_or_else(|| json!({})),
            result: serde_json::to_value(&d)?,
        },
    )
    .await?;
    let alerts = usage::raise_alerts(db, q, &before, st.used, now).await?;
    let st = usage::load_state(db, q, now).await?;
    Ok(Txn::Done(
        Consumption {
            event_id,
            replayed: false,
            decision: d,
        },
        Effects {
            packs: vec![(q.key.feature.clone(), q.key.subject.clone(), st.packs)],
            keys: vec![(q.key.clone(), st, q.clone())],
            alerts,
        },
    ))
}

async fn reserve_tx(
    db: &mut Db<'_>,
    q: &Quota,
    req: &UsageRequest,
    idem: &str,
    ctx: &AccessContext,
    now: i64,
    ttl: i64,
) -> Result<Txn<Reservation>> {
    let sql =
        format!("SELECT {RES_COLUMNS} FROM _forgebase_usage_reservations WHERE subject = ?1 AND idempotency_key = ?2");
    let prior = db.fetch(&sql, vec![q.key.subject.clone().into(), idem.into()]).await?;
    if let Some(r) = prior.rows.first().map(|r| res_row(r)) {
        if r.feature != q.key.feature || r.scope_key != q.key.scope_key || r.units != req.units {
            return Err(EntitlementError::conflict(
                "idempotency_conflict",
                "this idempotency key was used for a different request",
            ));
        }
        return Ok(Txn::Replay(r.public(true)));
    }
    usage::reap_key(db, q, now).await?;
    let mut st = usage::load_state(db, q, now).await?;
    let alloc = match usage::allocate(q, &st, req.units) {
        Ok(a) => a,
        Err(why) => {
            return Err(EntitlementError::QuotaExceeded(Box::new(usage::decision(
                q,
                &st,
                req.units,
                Alloc::default(),
                Some(why),
            ))));
        }
    };
    usage::ensure_row(db, q, &mut st, now).await?;
    let packs = usage::take_packs(db, q, alloc.pack_units, now).await?;
    if !usage::bump(
        db,
        q,
        st.window.start,
        0,
        alloc.window_units,
        usage::ceiling(q, &st),
        now,
    )
    .await?
    {
        return Err(EntitlementError::QuotaExceeded(Box::new(usage::decision(
            q,
            &st,
            req.units,
            Alloc::default(),
            Some(format!("quota exhausted for {}", q.key.feature)),
        ))));
    }
    let id = new_id("res");
    let expires_at = now + ttl * 1000;
    db.exec(
        "INSERT INTO _forgebase_usage_reservations
           (id, feature_key, subject, scope_key, window_start, units, pack_units, overage_units, packs, status,
            expires_at, idempotency_key, actor, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'held', ?10, ?11, ?12, ?13)",
        vec![
            id.clone().into(),
            q.key.feature.clone().into(),
            q.key.subject.clone().into(),
            q.key.scope_key.clone().into(),
            st.window.start.into(),
            req.units.into(),
            alloc.pack_units.into(),
            alloc.overage.into(),
            serde_json::to_string(&packs)?.into(),
            expires_at.into(),
            idem.into(),
            actor(ctx).into(),
            now.into(),
        ],
    )
    .await?;
    let st = usage::load_state(db, q, now).await?;
    Ok(Txn::Done(
        Reservation {
            id,
            feature: q.key.feature.clone(),
            subject: q.key.subject.clone(),
            scope_key: q.key.scope_key.clone(),
            units: req.units,
            pack_units: alloc.pack_units,
            overage_units: alloc.overage,
            status: ReservationStatus::Held,
            committed_units: None,
            expires_at,
            created_at: now,
            replayed: false,
        },
        Effects {
            packs: vec![(q.key.feature.clone(), q.key.subject.clone(), st.packs)],
            keys: vec![(q.key.clone(), st, q.clone())],
            alerts: Vec::new(),
        },
    ))
}

async fn settle_tx(
    db: &mut Db<'_>,
    q: &Quota,
    id: &str,
    commit: Option<Option<i64>>,
    ctx: &AccessContext,
    now: i64,
) -> Result<Txn<Reservation>> {
    let sql = format!("SELECT {RES_COLUMNS} FROM _forgebase_usage_reservations WHERE id = ?1");
    let rows = db.fetch(&sql, vec![id.into()]).await?;
    let mut r = rows
        .rows
        .first()
        .map(|r| res_row(r))
        .ok_or_else(|| EntitlementError::NotFound("reservation".into()))?;
    match (r.status, commit.is_some()) {
        (ReservationStatus::Committed, true) | (ReservationStatus::Released, false) => {
            return Ok(Txn::Replay(r.public(true)));
        }
        (ReservationStatus::Held, _) if r.expires_at <= now => {
            return Err(EntitlementError::conflict(
                "reservation_expired",
                "the reservation expired and was given back",
            ));
        }
        (ReservationStatus::Held, _) => {}
        _ => {
            return Err(EntitlementError::conflict(
                "reservation_settled",
                "the reservation is already settled",
            ));
        }
    }
    let Some(units) = commit else {
        usage::settle_back(db, q, &r, "released", now).await?;
        r.status = ReservationStatus::Released;
        let st = usage::load_state(db, q, now).await?;
        return Ok(Txn::Done(
            r.public(false),
            Effects {
                packs: vec![(q.key.feature.clone(), q.key.subject.clone(), st.packs)],
                keys: vec![(q.key.clone(), st, q.clone())],
                alerts: Vec::new(),
            },
        ));
    };
    let actual = units.unwrap_or(r.units);
    if !(0..=r.units).contains(&actual) {
        return Err(EntitlementError::invalid(
            "invalid_units",
            format!("commit between 0 and the {} reserved units", r.units),
        ));
    }
    // Give back what was not used: usage-pack units first.
    let back = r.units - actual;
    let pack_back = back.min(r.pack_units);
    let window_back = back - pack_back;
    usage::return_packs(db, &r.packs, pack_back).await?;
    let window_units = r.units - r.pack_units;
    let committed_window = window_units - window_back;
    let overage = (r.overage - window_back).max(0);
    // Alerts are judged in the window the reservation was taken in.
    let before = usage::window_row(db, q, r.window_start).await?;
    usage::bump(db, q, r.window_start, committed_window, -window_units, None, now).await?;
    db.exec(
        "UPDATE _forgebase_usage_reservations SET status = 'committed', committed_units = ?1, settled_at = ?2 WHERE id = ?3",
        vec![actual.into(), now.into(), id.into()],
    )
    .await?;
    let mut alerts = Vec::new();
    if actual > 0 {
        usage::insert_event(
            db,
            NewEvent {
                q,
                at: now,
                units: actual,
                pack_units: r.pack_units - pack_back,
                overage,
                window_start: r.window_start,
                source: "reservation",
                reservation_id: Some(r.id.clone()),
                idempotency_key: format!("reservation:{}", r.id),
                actor: actor(ctx),
                request_id: ctx.request_id.clone(),
                metadata: json!({}),
                result: json!({}),
            },
        )
        .await?;
        if let Some(before) = &before {
            alerts = usage::raise_alerts(db, q, before, before.used + committed_window, now).await?;
        }
    }
    r.status = ReservationStatus::Committed;
    r.committed_units = Some(actual);
    let st = usage::load_state(db, q, now).await?;
    Ok(Txn::Done(
        r.public(false),
        Effects {
            packs: vec![(q.key.feature.clone(), q.key.subject.clone(), st.packs)],
            keys: vec![(q.key.clone(), st, q.clone())],
            alerts,
        },
    ))
}

async fn lease_tx(
    db: &mut Db<'_>,
    q: &Quota,
    idem: &str,
    ctx: &AccessContext,
    now: i64,
    ttl: i64,
) -> Result<Txn<Lease>> {
    let prior = db
        .fetch(
            "SELECT id, feature_key, scope_key, expires_at, released_at FROM _forgebase_quota_leases
             WHERE subject = ?1 AND idempotency_key = ?2",
            vec![q.key.subject.clone().into(), idem.into()],
        )
        .await?;
    if let Some(r) = prior.rows.first() {
        if text(r, 1) != q.key.feature || text(r, 2) != q.key.scope_key {
            return Err(EntitlementError::conflict(
                "idempotency_conflict",
                "this idempotency key was used for a different request",
            ));
        }
        return Ok(Txn::Replay(Lease {
            id: text(r, 0),
            feature: q.key.feature.clone(),
            subject: q.key.subject.clone(),
            scope_key: q.key.scope_key.clone(),
            expires_at: int(r, 3),
            released: opt_int(r, 4).is_some(),
            replayed: true,
        }));
    }
    let st = usage::load_state(db, q, now).await?;
    if let Err(why) = usage::allocate(q, &st, 1) {
        return Err(EntitlementError::QuotaExceeded(Box::new(usage::decision(
            q,
            &st,
            1,
            Alloc::default(),
            Some(why),
        ))));
    }
    let id = new_id("lse");
    let expires_at = now + ttl * 1000;
    db.exec(
        "INSERT INTO _forgebase_quota_leases (id, feature_key, subject, scope_key, expires_at, idempotency_key, actor, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        vec![
            id.clone().into(),
            q.key.feature.clone().into(),
            q.key.subject.clone().into(),
            q.key.scope_key.clone().into(),
            expires_at.into(),
            idem.into(),
            actor(ctx).into(),
            now.into(),
        ],
    )
    .await?;
    let st = usage::load_state(db, q, now).await?;
    Ok(Txn::Done(
        Lease {
            id,
            feature: q.key.feature.clone(),
            subject: q.key.subject.clone(),
            scope_key: q.key.scope_key.clone(),
            expires_at,
            released: false,
            replayed: false,
        },
        Effects {
            keys: vec![(q.key.clone(), st, q.clone())],
            ..Effects::default()
        },
    ))
}

async fn gauge_tx(
    db: &mut Db<'_>,
    q: &Quota,
    req: &GaugeRequest,
    idem: &str,
    ctx: &AccessContext,
    now: i64,
) -> Result<Txn<Consumption>> {
    let prior = db
        .fetch(
            "SELECT id, feature_key, scope_key, source, result FROM _forgebase_usage_events
             WHERE subject = ?1 AND idempotency_key = ?2",
            vec![q.key.subject.clone().into(), idem.into()],
        )
        .await?;
    if let Some(r) = prior.rows.first() {
        if text(r, 1) != q.key.feature || text(r, 2) != q.key.scope_key || text(r, 3) != "gauge" {
            return Err(EntitlementError::conflict(
                "idempotency_conflict",
                "this idempotency key was used for a different request",
            ));
        }
        let decision: Decision = serde_json::from_str(&text(r, 4))?;
        return Ok(Txn::Replay(Consumption {
            event_id: text(r, 0),
            replayed: true,
            decision,
        }));
    }
    let mut st = usage::load_state(db, q, now).await?;
    let delta = match (req.delta, req.value) {
        (Some(d), _) => d,
        (None, Some(v)) => v - st.used,
        (None, None) => 0,
    };
    if st.used + delta < 0 {
        return Err(EntitlementError::invalid(
            "invalid_gauge_change",
            "the level cannot go below zero",
        ));
    }
    let alloc = match usage::allocate(q, &st, delta) {
        Ok(a) => a,
        Err(why) => {
            return Err(EntitlementError::QuotaExceeded(Box::new(usage::decision(
                q,
                &st,
                delta,
                Alloc::default(),
                Some(why),
            ))));
        }
    };
    let before = st.clone();
    usage::ensure_row(db, q, &mut st, now).await?;
    if !usage::bump(db, q, st.window.start, delta, 0, usage::ceiling(q, &st), now).await? {
        return Err(EntitlementError::QuotaExceeded(Box::new(usage::decision(
            q,
            &st,
            delta,
            Alloc::default(),
            Some(format!("quota exhausted for {}", q.key.feature)),
        ))));
    }
    st.used += delta;
    let d = usage::decision(q, &st, delta, alloc, None);
    let (event_id, _) = usage::insert_event(
        db,
        NewEvent {
            q,
            at: now,
            units: delta,
            pack_units: 0,
            overage: alloc.overage,
            window_start: 0,
            source: "gauge",
            reservation_id: None,
            idempotency_key: idem.to_owned(),
            actor: actor(ctx),
            request_id: ctx.request_id.clone(),
            metadata: req.metadata.clone().unwrap_or_else(|| json!({})),
            result: serde_json::to_value(&d)?,
        },
    )
    .await?;
    let alerts = usage::raise_alerts(db, q, &before, st.used, now).await?;
    let st = usage::load_state(db, q, now).await?;
    Ok(Txn::Done(
        Consumption {
            event_id,
            replayed: false,
            decision: d,
        },
        Effects {
            keys: vec![(q.key.clone(), st, q.clone())],
            alerts,
            ..Effects::default()
        },
    ))
}
