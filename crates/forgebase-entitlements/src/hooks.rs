//! The FB-21 pipeline hooks (ADR-0008 §8): step 4 (entitlement) and step 6
//! (quota admission). Both answer from memory: the catalog snapshot and the
//! usage cache.
//!
//! * Step 4 denies when a feature that gates the permission is not enabled
//!   for the subject (and member). A feature gates the permissions in its
//!   `permissions` patterns, and `org:<feature>:<permission>` keys.
//! * Step 6 denies when a hard quota whose policy lists the permission
//!   cannot cover the policy's `cost` (a soft quota only past its overage
//!   ceiling; a concurrency quota when every lease is taken). It admits; the
//!   operation meters its usage through the reservation API.

use std::sync::Arc;

use forgebase_data::{Principal, SubjectRef};
use forgebase_policy::{EntitlementHook, HookOutcome, HookRequest, Hooks, QuotaHook};

use crate::cache::Key;
use crate::catalog::Catalog;
use crate::model::{Enforcement, FeatureKind, QuotaScope};
use crate::period::{self, Period};
use crate::service::{EntitlementService, Shared, subject_key};

/// Step 4.
#[derive(Debug, Clone)]
pub struct EntitlementGate {
    shared: Arc<Shared>,
}

/// Step 6.
#[derive(Debug, Clone)]
pub struct QuotaGate {
    shared: Arc<Shared>,
}

impl EntitlementService {
    /// The hooks to install with `PolicyService::set_hooks`.
    pub fn hooks(&self) -> Hooks {
        Hooks {
            entitlement: Arc::new(EntitlementGate {
                shared: self.shared().clone(),
            }),
            quota: Arc::new(QuotaGate {
                shared: self.shared().clone(),
            }),
        }
    }
}

fn member(req: &HookRequest<'_>) -> Option<String> {
    match req.ctx.principal {
        Principal::User { .. } | Principal::Service { .. } => Some(req.ctx.principal_key()),
        _ => None,
    }
}

fn subject(s: &SubjectRef) -> String {
    subject_key(s)
}

impl EntitlementHook for EntitlementGate {
    fn check(&self, req: &HookRequest<'_>) -> HookOutcome {
        let cat = self.shared.catalog();
        let gates = cat.gating(req.permission);
        if gates.is_empty() {
            return HookOutcome::Allow;
        }
        let r = cat.resolve(&subject(&req.subject), member(req).as_deref(), self.shared.now());
        for f in gates {
            if !r.enabled(f) {
                return HookOutcome::Deny(format!("feature {f} is not included in the plan"));
            }
        }
        HookOutcome::Allow
    }
}

impl QuotaHook for QuotaGate {
    fn check(&self, req: &HookRequest<'_>) -> HookOutcome {
        let cat = self.shared.catalog();
        let metered = cat.metering(req.permission);
        if metered.is_empty() {
            return HookOutcome::Allow;
        }
        let now = self.shared.now();
        let subject = subject(&req.subject);
        let member = member(req);
        for f in metered {
            if let Some(why) = admit(&self.shared, &cat, req, &subject, member.as_deref(), f, now) {
                return HookOutcome::Deny(why);
            }
        }
        HookOutcome::Allow
    }
}

/// `Some(reason)` when the quota of `feature` cannot admit the operation.
fn admit(
    shared: &Shared,
    cat: &Catalog,
    req: &HookRequest<'_>,
    subject: &str,
    member: Option<&str>,
    feature: &str,
    now: i64,
) -> Option<String> {
    let kind = cat.features.get(feature)?.kind;
    let policy = cat.policy(feature);
    let scope_key = match policy.scope {
        QuotaScope::Subject => String::new(),
        QuotaScope::User => member?.to_owned(),
        QuotaScope::Team => req.resource.filter(|r| r.kind == "team").map(|r| r.id.clone())?,
        QuotaScope::Resource => req.resource.map(ToString::to_string)?,
    };
    let member = if policy.scope == QuotaScope::User {
        Some(scope_key.as_str())
    } else {
        member
    };
    let r = cat.resolve(subject, member, now);
    let Some(g) = r.grant(feature).filter(|g| g.enabled) else {
        return Some(format!("{feature} is not included in the plan"));
    };
    let limit = g.limit?;
    let entry = shared.usage.get(&Key::new(feature, subject, &scope_key));
    if kind == FeatureKind::Concurrency {
        let active = entry.map_or(0, |e| e.leases.iter().filter(|t| **t > now).count() as i64);
        return (active >= limit).then(|| format!("all {limit} concurrent {feature} leases are in use"));
    }
    let mut p = g.period;
    if kind == FeatureKind::Metered && p == Period::None {
        p = Period::Lifetime;
    }
    if kind != FeatureKind::Metered {
        p = Period::None;
    }
    let w = period::window(p, g.window_secs, r.cycle_anchor, now);
    let (consumed, carried) = match entry {
        Some(e) if p == Period::Rolling && now < e.valid_until => (e.consumed, 0),
        Some(e) if p != Period::Rolling && e.window_start == w.start => (e.consumed, e.carried),
        _ => (0, 0),
    };
    let cap = limit.saturating_add(carried);
    let packs = if kind == FeatureKind::Metered {
        shared.usage.packs(feature, subject)
    } else {
        0
    };
    let cost = if kind == FeatureKind::Gauge { 0 } else { policy.cost };
    let ceiling = match (policy.enforcement, policy.overage_pct) {
        (Enforcement::Hard, _) => cap,
        (Enforcement::Soft, Some(pct)) => cap.saturating_add(cap.saturating_mul(pct) / 100),
        (Enforcement::Soft, None) => return None,
    };
    let over = if kind == FeatureKind::Gauge {
        consumed >= ceiling
    } else {
        consumed + cost > ceiling + packs
    };
    over.then(|| format!("quota exhausted for {feature}"))
}
