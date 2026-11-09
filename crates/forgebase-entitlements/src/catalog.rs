//! The catalog and subject state in memory, and entitlement resolution.
//!
//! Every table of migration 0009 except usage loads into one immutable
//! [`Catalog`]; a write through the service commits and then swaps in a fresh
//! one (cache invalidation, as in ADR-0008 §7). Hooks on the request path
//! read only this snapshot and the usage cache: no I/O per check.
//!
//! Resolution order for a subject `S` and member `U` (ADR-0010 §3, the
//! requirements' priority list):
//!
//! 1. the plan of `S`'s subscription when it entitles (active, past_due, or
//!    trialing before the trial end), else the default plan; the trial
//!    allowance replaces limits while trialing;
//! 2. add-ons of `S` (enable features, add units per quantity);
//! 3. enterprise custom limits of `S` (replace the grant);
//! 4. allow overrides, least specific first: `U` everywhere, then `S`, then
//!    `U` inside `S` (enable, replace the limit);
//! 5. deny overrides of any of those: always win.
//!
//! Then quota availability is checked against the resulting limit.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use forgebase_db::{Backend, Rows, Statement, Value};
use serde_json::Value as Json;

use crate::error::{EntitlementError, Result};
use crate::model::{
    AddonFeature, CustomLimit, FeatureKind, Grant, GrantSource, Override, PlanFeature, QuotaPolicy, SubscriptionStatus,
};
use crate::period::Period;

/// A feature row.
#[derive(Debug, Clone)]
pub(crate) struct FeatureRow {
    pub key: String,
    pub name: String,
    pub description: String,
    pub kind: FeatureKind,
    pub unit: String,
    pub permissions: Vec<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// A plan row with its features.
#[derive(Debug, Clone)]
pub(crate) struct PlanRow {
    pub id: String,
    pub key: String,
    pub name: String,
    pub description: String,
    pub is_default: bool,
    pub active: bool,
    pub trial_days: i64,
    pub metadata: Json,
    pub created_at: i64,
    pub updated_at: i64,
    pub features: Vec<PlanFeature>,
}

/// An add-on row with its grants.
#[derive(Debug, Clone)]
pub(crate) struct AddonRow {
    pub id: String,
    pub key: String,
    pub name: String,
    pub description: String,
    pub active: bool,
    pub features: Vec<AddonFeature>,
}

/// A usage pack row.
#[derive(Debug, Clone)]
pub(crate) struct PackRow {
    pub id: String,
    pub key: String,
    pub name: String,
    pub feature: String,
    pub units: i64,
    pub valid_days: Option<i64>,
    pub active: bool,
}

/// A subscription row.
#[derive(Debug, Clone)]
pub(crate) struct SubRow {
    pub subject: String,
    pub plan_id: String,
    pub status: SubscriptionStatus,
    pub trial_ends_at: Option<i64>,
    pub current_period_start: Option<i64>,
    pub current_period_end: Option<i64>,
    pub cycle_anchor: Option<i64>,
    pub cancel_at: Option<i64>,
    pub external_ref: Option<String>,
    pub updated_at: i64,
}

impl SubRow {
    /// Whether the subscription grants its plan at `now`.
    pub fn entitles(&self, now: i64) -> bool {
        if self.cancel_at.is_some_and(|c| now >= c) {
            return false;
        }
        match self.status {
            SubscriptionStatus::Active | SubscriptionStatus::PastDue => true,
            SubscriptionStatus::Trialing => self.trial_ends_at.is_none_or(|t| now < t),
            SubscriptionStatus::Paused | SubscriptionStatus::Canceled | SubscriptionStatus::Expired => false,
        }
    }

    /// In a running trial at `now`.
    pub fn trialing(&self, now: i64) -> bool {
        self.status == SubscriptionStatus::Trialing && self.entitles(now)
    }
}

/// A subject's resolved entitlements.
#[derive(Debug, Clone)]
pub(crate) struct Resolved {
    pub plan: Option<String>,
    pub status: Option<SubscriptionStatus>,
    pub entitled: bool,
    pub trial_ends_at: Option<i64>,
    pub cycle_anchor: Option<i64>,
    pub grants: BTreeMap<String, Grant>,
    /// Recompute at or after this time (trial end, cancellation, override expiry).
    pub valid_until: i64,
}

impl Resolved {
    /// The grant of a feature, if any.
    pub fn grant(&self, feature: &str) -> Option<&Grant> {
        self.grants.get(feature)
    }

    /// Whether the feature is enabled.
    pub fn enabled(&self, feature: &str) -> bool {
        self.grants.get(feature).is_some_and(|g| g.enabled)
    }
}

const RESOLVED_CACHE_MAX: usize = 50_000;

/// Resolutions by (subject, member).
type ResolvedCache = Mutex<HashMap<(String, Option<String>), Arc<Resolved>>>;

/// The catalog and subject state.
#[derive(Debug, Default)]
pub(crate) struct Catalog {
    pub features: BTreeMap<String, FeatureRow>,
    pub plans: BTreeMap<String, PlanRow>,
    pub plan_ids: HashMap<String, String>,
    pub default_plan: Option<String>,
    pub addons: BTreeMap<String, AddonRow>,
    pub addon_ids: HashMap<String, String>,
    pub packs: BTreeMap<String, PackRow>,
    pub pack_ids: HashMap<String, String>,
    pub policies: HashMap<String, QuotaPolicy>,
    pub subscriptions: HashMap<String, SubRow>,
    pub sub_addons: HashMap<String, Vec<(String, i64)>>,
    pub overrides: Vec<Override>,
    pub custom: HashMap<(String, String), CustomLimit>,
    /// (permission pattern, feature) gating pairs of step 4.
    pub gates: Vec<(String, String)>,
    /// (permission pattern, feature) admission pairs of step 6.
    pub quota_gates: Vec<(String, String)>,
    /// Features of kind `rate`.
    pub rate_features: Vec<String>,
    resolved: ResolvedCache,
}

fn text(r: &[Value], i: usize) -> String {
    r.get(i).and_then(Value::as_str).unwrap_or_default().to_owned()
}

fn opt_text(r: &[Value], i: usize) -> Option<String> {
    r.get(i).and_then(Value::as_str).map(str::to_owned)
}

fn int(r: &[Value], i: usize) -> i64 {
    r.get(i).and_then(Value::as_i64).unwrap_or_default()
}

fn opt_int(r: &[Value], i: usize) -> Option<i64> {
    r.get(i).and_then(Value::as_i64)
}

fn json(r: &[Value], i: usize) -> Json {
    r.get(i)
        .and_then(Value::as_str)
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(Json::Null)
}

fn strings(r: &[Value], i: usize) -> Vec<String> {
    serde_json::from_value(json(r, i)).unwrap_or_default()
}

fn period(r: &[Value], i: usize) -> Option<Period> {
    r.get(i).and_then(Value::as_str).and_then(Period::parse)
}

async fn rows(db: &dyn Backend, sql: &str) -> Result<Rows> {
    Ok(db.fetch_all(&Statement::new(sql, Vec::new())).await?)
}

/// The default window of a feature kind.
pub(crate) fn default_period(kind: FeatureKind) -> Period {
    match kind {
        FeatureKind::Metered => Period::Month,
        FeatureKind::Rate => Period::Hour,
        FeatureKind::Boolean | FeatureKind::Gauge | FeatureKind::Concurrency => Period::None,
    }
}

impl Catalog {
    /// Loads everything but usage.
    pub async fn load(db: &dyn Backend) -> Result<Self> {
        let mut c = Catalog::default();
        for r in rows(
            db,
            "SELECT key, name, description, kind, unit, permissions, created_at, updated_at FROM _forgebase_features",
        )
        .await?
        .rows
        {
            let kind = FeatureKind::parse(&text(&r, 3))
                .ok_or_else(|| EntitlementError::Internal("bad feature kind".into()))?;
            let f = FeatureRow {
                key: text(&r, 0),
                name: text(&r, 1),
                description: text(&r, 2),
                kind,
                unit: text(&r, 4),
                permissions: strings(&r, 5),
                created_at: int(&r, 6),
                updated_at: int(&r, 7),
            };
            for p in &f.permissions {
                c.gates.push((p.clone(), f.key.clone()));
            }
            if kind == FeatureKind::Rate {
                c.rate_features.push(f.key.clone());
            }
            c.features.insert(f.key.clone(), f);
        }
        for r in rows(
            db,
            "SELECT id, key, name, description, is_default, active, trial_days, metadata, created_at, updated_at FROM _forgebase_plans",
        )
        .await?
        .rows
        {
            let p = PlanRow {
                id: text(&r, 0),
                key: text(&r, 1),
                name: text(&r, 2),
                description: text(&r, 3),
                is_default: int(&r, 4) == 1,
                active: int(&r, 5) == 1,
                trial_days: int(&r, 6),
                metadata: json(&r, 7),
                created_at: int(&r, 8),
                updated_at: int(&r, 9),
                features: Vec::new(),
            };
            if p.is_default {
                c.default_plan = Some(p.key.clone());
            }
            c.plan_ids.insert(p.id.clone(), p.key.clone());
            c.plans.insert(p.key.clone(), p);
        }
        for r in rows(
            db,
            "SELECT plan_id, feature_key, enabled, limit_value, unit, period, window_secs, trial_limit
             FROM _forgebase_plan_features ORDER BY feature_key",
        )
        .await?
        .rows
        {
            let Some(key) = c.plan_ids.get(&text(&r, 0)).cloned() else {
                continue;
            };
            if let Some(p) = c.plans.get_mut(&key) {
                p.features.push(PlanFeature {
                    feature: text(&r, 1),
                    enabled: int(&r, 2) == 1,
                    limit: opt_int(&r, 3),
                    unit: text(&r, 4),
                    period: period(&r, 5),
                    window_secs: opt_int(&r, 6),
                    trial_limit: opt_int(&r, 7),
                });
            }
        }
        for r in rows(db, "SELECT id, key, name, description, active FROM _forgebase_addons")
            .await?
            .rows
        {
            let a = AddonRow {
                id: text(&r, 0),
                key: text(&r, 1),
                name: text(&r, 2),
                description: text(&r, 3),
                active: int(&r, 4) == 1,
                features: Vec::new(),
            };
            c.addon_ids.insert(a.id.clone(), a.key.clone());
            c.addons.insert(a.key.clone(), a);
        }
        for r in rows(
            db,
            "SELECT addon_id, feature_key, limit_value, period, window_secs FROM _forgebase_addon_features ORDER BY feature_key",
        )
        .await?
        .rows
        {
            let Some(key) = c.addon_ids.get(&text(&r, 0)).cloned() else {
                continue;
            };
            if let Some(a) = c.addons.get_mut(&key) {
                a.features.push(AddonFeature {
                    feature: text(&r, 1),
                    limit: opt_int(&r, 2),
                    period: period(&r, 3),
                    window_secs: opt_int(&r, 4),
                });
            }
        }
        for r in rows(
            db,
            "SELECT id, key, name, feature_key, units, valid_days, active FROM _forgebase_usage_packs",
        )
        .await?
        .rows
        {
            let p = PackRow {
                id: text(&r, 0),
                key: text(&r, 1),
                name: text(&r, 2),
                feature: text(&r, 3),
                units: int(&r, 4),
                valid_days: opt_int(&r, 5),
                active: int(&r, 6) == 1,
            };
            c.pack_ids.insert(p.id.clone(), p.key.clone());
            c.packs.insert(p.key.clone(), p);
        }
        for r in rows(
            db,
            "SELECT feature_key, scope, enforcement, overage_pct, rollover_pct, rollover_cap, cost, lease_ttl_secs, permissions
             FROM _forgebase_quota_policies",
        )
        .await?
        .rows
        {
            let p = QuotaPolicy {
                feature: text(&r, 0),
                scope: crate::model::QuotaScope::parse(&text(&r, 1)).unwrap_or_default(),
                enforcement: if text(&r, 2) == "soft" {
                    crate::model::Enforcement::Soft
                } else {
                    crate::model::Enforcement::Hard
                },
                overage_pct: opt_int(&r, 3),
                rollover_pct: int(&r, 4),
                rollover_cap: opt_int(&r, 5),
                cost: int(&r, 6).max(1),
                lease_ttl_secs: int(&r, 7).max(1),
                permissions: strings(&r, 8),
            };
            for perm in &p.permissions {
                c.quota_gates.push((perm.clone(), p.feature.clone()));
            }
            c.policies.insert(p.feature.clone(), p);
        }
        for r in rows(
            db,
            "SELECT subject, plan_id, status, trial_ends_at, current_period_start, current_period_end, cycle_anchor,
                    cancel_at, external_ref, source_event_at, updated_at
             FROM _forgebase_subscriptions",
        )
        .await?
        .rows
        {
            let status = SubscriptionStatus::parse(&text(&r, 2))
                .ok_or_else(|| EntitlementError::Internal("bad subscription status".into()))?;
            let s = SubRow {
                subject: text(&r, 0),
                plan_id: text(&r, 1),
                status,
                trial_ends_at: opt_int(&r, 3),
                current_period_start: opt_int(&r, 4),
                current_period_end: opt_int(&r, 5),
                cycle_anchor: opt_int(&r, 6),
                cancel_at: opt_int(&r, 7),
                external_ref: opt_text(&r, 8),
                updated_at: int(&r, 10),
            };
            c.subscriptions.insert(s.subject.clone(), s);
        }
        for r in rows(
            db,
            "SELECT subject, addon_id, quantity FROM _forgebase_subscription_addons ORDER BY subject, addon_id",
        )
        .await?
        .rows
        {
            c.sub_addons
                .entry(text(&r, 0))
                .or_default()
                .push((text(&r, 1), int(&r, 2)));
        }
        for r in rows(
            db,
            "SELECT id, subject, member, feature_key, effect, limit_value, period, window_secs, expires_at, reason,
                    created_by, created_at
             FROM _forgebase_entitlement_overrides ORDER BY created_at, id",
        )
        .await?
        .rows
        {
            c.overrides.push(Override {
                id: text(&r, 0),
                subject: opt_text(&r, 1),
                member: opt_text(&r, 2),
                feature: text(&r, 3),
                effect: text(&r, 4),
                limit: opt_int(&r, 5),
                period: period(&r, 6),
                window_secs: opt_int(&r, 7),
                expires_at: opt_int(&r, 8),
                reason: text(&r, 9),
                created_by: text(&r, 10),
                created_at: int(&r, 11),
            });
        }
        for r in rows(
            db,
            "SELECT subject, feature_key, enabled, limit_value, period, window_secs, reason FROM _forgebase_custom_limits",
        )
        .await?
        .rows
        {
            let l = CustomLimit {
                subject: text(&r, 0),
                feature: text(&r, 1),
                enabled: int(&r, 2) == 1,
                limit: opt_int(&r, 3),
                period: period(&r, 4),
                window_secs: opt_int(&r, 5),
                reason: text(&r, 6),
            };
            c.custom.insert((l.subject.clone(), l.feature.clone()), l);
        }
        Ok(c)
    }

    /// The plan key of a plan id.
    pub fn plan_key(&self, id: &str) -> Option<&str> {
        self.plan_ids.get(id).map(String::as_str)
    }

    /// The quota policy of a feature (defaults when none is stored).
    pub fn policy(&self, feature: &str) -> QuotaPolicy {
        self.policies.get(feature).cloned().unwrap_or_else(|| QuotaPolicy {
            feature: feature.to_owned(),
            ..QuotaPolicy::default()
        })
    }

    /// Features that gate `permission` at step 4: listed patterns, and
    /// `org:<feature>:<permission>` keys of catalog features.
    pub fn gating(&self, permission: &str) -> Vec<&str> {
        let mut out: Vec<&str> = self
            .gates
            .iter()
            .filter(|(p, _)| forgebase_policy::perm::matches(p, permission))
            .map(|(_, f)| f.as_str())
            .collect();
        let mut segs = permission.split(':');
        if segs.next() == Some("org")
            && let Some(f) = segs.next()
            && segs.next().is_some()
            && let Some(row) = self.features.get(f)
        {
            out.push(row.key.as_str());
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Features whose quota pipeline step 6 checks for `permission`.
    pub fn metering(&self, permission: &str) -> Vec<&str> {
        let mut out: Vec<&str> = self
            .quota_gates
            .iter()
            .filter(|(p, _)| forgebase_policy::perm::matches(p, permission))
            .map(|(_, f)| f.as_str())
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Effective entitlements of `subject` for `member`, cached until the
    /// next time-dependent change.
    pub fn resolve(&self, subject: &str, member: Option<&str>, now: i64) -> Arc<Resolved> {
        let key = (subject.to_owned(), member.map(str::to_owned));
        {
            let cache = self.resolved.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(r) = cache.get(&key)
                && now < r.valid_until
            {
                return r.clone();
            }
        }
        let r = Arc::new(self.compute(subject, member, now));
        let mut cache = self.resolved.lock().unwrap_or_else(|p| p.into_inner());
        if cache.len() >= RESOLVED_CACHE_MAX {
            cache.clear();
        }
        cache.insert(key, r.clone());
        r
    }

    fn grant_of(&self, feature: &str, enabled: bool, limit: Option<i64>, unit: &str, period: Option<Period>) -> Grant {
        let (kind, default_unit) = self
            .features
            .get(feature)
            .map_or((FeatureKind::Boolean, ""), |f| (f.kind, f.unit.as_str()));
        Grant {
            enabled,
            kind,
            limit,
            unit: if unit.is_empty() {
                default_unit.to_owned()
            } else {
                unit.to_owned()
            },
            period: period.unwrap_or_else(|| default_period(kind)),
            window_secs: None,
            source: GrantSource::Plan,
        }
    }

    fn compute(&self, subject: &str, member: Option<&str>, now: i64) -> Resolved {
        let mut valid_until = i64::MAX;
        let mut soon = |t: Option<i64>| {
            if let Some(t) = t
                && t > now
            {
                valid_until = valid_until.min(t);
            }
        };
        let sub = self.subscriptions.get(subject);
        let entitled = sub.is_some_and(|s| s.entitles(now));
        let trialing = sub.is_some_and(|s| s.trialing(now));
        if let Some(s) = sub {
            if s.status == SubscriptionStatus::Trialing {
                soon(s.trial_ends_at);
            }
            soon(s.cancel_at);
        }
        let plan = if entitled {
            sub.and_then(|s| self.plan_key(&s.plan_id)).map(str::to_owned)
        } else {
            self.default_plan.clone()
        };
        let mut grants: BTreeMap<String, Grant> = BTreeMap::new();
        if let Some(p) = plan.as_ref().and_then(|k| self.plans.get(k)) {
            for pf in &p.features {
                if !self.features.contains_key(&pf.feature) {
                    continue;
                }
                let mut g = self.grant_of(&pf.feature, pf.enabled, pf.limit, &pf.unit, pf.period);
                g.window_secs = pf.window_secs;
                if trialing && let Some(t) = pf.trial_limit {
                    g.limit = Some(t);
                    g.source = GrantSource::Trial;
                }
                grants.insert(pf.feature.clone(), g);
            }
        }
        if entitled {
            for (addon_id, qty) in self.sub_addons.get(subject).into_iter().flatten() {
                let Some(a) = self.addon_ids.get(addon_id).and_then(|k| self.addons.get(k)) else {
                    continue;
                };
                for af in &a.features {
                    if !self.features.contains_key(&af.feature) {
                        continue;
                    }
                    let add = af.limit.map(|l| l.saturating_mul(*qty));
                    match grants.get_mut(&af.feature) {
                        Some(g) if g.enabled => {
                            if let (Some(l), Some(d)) = (g.limit, add) {
                                g.limit = Some(l.saturating_add(d));
                            }
                            g.source = GrantSource::Addon;
                        }
                        _ => {
                            let mut g = self.grant_of(&af.feature, true, add, "", af.period);
                            g.window_secs = af.window_secs;
                            g.source = GrantSource::Addon;
                            grants.insert(af.feature.clone(), g);
                        }
                    }
                }
            }
        }
        for ((s, f), c) in &self.custom {
            if s != subject || !self.features.contains_key(f) {
                continue;
            }
            let period = c.period.or_else(|| grants.get(f).map(|g| g.period));
            let mut g = self.grant_of(f, c.enabled, c.limit, "", period);
            g.window_secs = c.window_secs.or_else(|| grants.get(f).and_then(|g| g.window_secs));
            g.source = GrantSource::Custom;
            grants.insert(f.clone(), g);
        }
        // Overrides, least specific first; deny wins at the end.
        let live = |o: &&Override| o.expires_at.is_none_or(|t| now < t) && self.features.contains_key(&o.feature);
        let rank = |o: &Override| -> Option<u8> {
            match (o.subject.as_deref(), o.member.as_deref()) {
                (None, Some(m)) if Some(m) == member => Some(0),
                (Some(s), None) if s == subject => Some(1),
                (Some(s), Some(m)) if s == subject && Some(m) == member => Some(2),
                _ => None,
            }
        };
        let mut matching: Vec<(u8, &Override)> = self
            .overrides
            .iter()
            .filter(live)
            .filter_map(|o| rank(o).map(|r| (r, o)))
            .collect();
        matching.sort_by_key(|(r, _)| *r);
        let mut denied = Vec::new();
        for (_, o) in &matching {
            soon(o.expires_at);
            if o.effect == "deny" {
                denied.push(o.feature.clone());
                continue;
            }
            let period = o.period.or_else(|| grants.get(&o.feature).map(|g| g.period));
            let limit = match o.limit {
                Some(l) => Some(l),
                None => grants.get(&o.feature).filter(|g| g.enabled).and_then(|g| g.limit),
            };
            let window = o
                .window_secs
                .or_else(|| grants.get(&o.feature).and_then(|g| g.window_secs));
            let mut g = self.grant_of(&o.feature, true, limit, "", period);
            g.window_secs = window;
            g.source = GrantSource::Override;
            grants.insert(o.feature.clone(), g);
        }
        for f in denied {
            let g = grants
                .entry(f.clone())
                .or_insert_with(|| self.grant_of(&f, false, Some(0), "", None));
            g.enabled = false;
            g.source = GrantSource::Override;
        }
        Resolved {
            plan,
            status: sub.map(|s| s.status),
            entitled,
            trial_ends_at: sub.and_then(|s| s.trial_ends_at),
            cycle_anchor: sub.filter(|_| entitled).and_then(|s| s.cycle_anchor),
            grants,
            valid_until,
        }
    }
}
