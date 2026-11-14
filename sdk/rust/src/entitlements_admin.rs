//! The administrative side of entitlements and quotas: the catalog (plans, features, add-ons,
//! usage packs, quota policies), per-subject overrides and enterprise limits, subscription sync,
//! trials, gauges, usage events and alerts. Reached through
//! [`ForgeBase::entitlements`](crate::ForgeBase::entitlements); writes need platform administrator rights.
//!
//! Request and response types are the generated wire models ([`generated`](crate::generated)).

use std::sync::Arc;

use serde_json::json;

use crate::entitlements::EntitlementsApi;
use crate::error::Result;
use crate::generated::{
    Addon, AddonInput, Consumption, CustomLimit, EntitlementsMaintenanceReport, Feature, FeatureInput, GaugeRequest,
    Override, OverrideInput, PackGrant, PackGrantInput, Plan, PlanInput, QuotaPolicy, SubjectAddon, Subscription,
    SubscriptionSync, SyncOutcome, TrialInput, UsageAlert, UsageEventPage, UsagePack, UsagePackInput,
};
use crate::http::{Req, Transport, seg};

/// Options of [`EntitlementsApi::usage_events`].
#[derive(Debug, Clone, Default)]
pub struct UsageEventsOptions {
    /// Subject (administrators may omit it: every subject; others get their own).
    pub subject: Option<String>,
    /// Only this feature.
    pub feature: Option<String>,
    /// Cursor: events with a larger `seq` (the previous page's `next`).
    pub after: Option<i64>,
    /// Page size, default 100, at most 1000.
    pub limit: Option<u32>,
}

impl EntitlementsApi {
    /// Plans of the catalog.
    #[must_use]
    pub fn plans(&self) -> EntitlementPlansApi {
        EntitlementPlansApi { t: self.t.clone() }
    }
    /// Features of the catalog.
    #[must_use]
    pub fn features(&self) -> EntitlementFeaturesApi {
        EntitlementFeaturesApi { t: self.t.clone() }
    }
    /// Add-ons of the catalog.
    #[must_use]
    pub fn addons(&self) -> EntitlementAddonsApi {
        EntitlementAddonsApi { t: self.t.clone() }
    }
    /// Usage packs: the catalog and the packs a subject holds.
    #[must_use]
    pub fn usage_packs(&self) -> UsagePacksApi {
        UsagePacksApi { t: self.t.clone() }
    }
    /// Quota policies per feature.
    #[must_use]
    pub fn quota_policies(&self) -> QuotaPoliciesApi {
        QuotaPoliciesApi { t: self.t.clone() }
    }
    /// Per-subject allow / deny overrides of a feature.
    #[must_use]
    pub fn overrides(&self) -> EntitlementOverridesApi {
        EntitlementOverridesApi { t: self.t.clone() }
    }
    /// Enterprise limits of a subject.
    #[must_use]
    pub fn limits(&self) -> EntitlementLimitsApi {
        EntitlementLimitsApi { t: self.t.clone() }
    }

    /// The subscription of `subject` (default: the caller's).
    pub async fn subscription(&self, subject: Option<&str>) -> Result<Subscription> {
        self.t
            .json(Req::get("/api/v1/entitlements/subscription").q_opt("subject", subject))
            .await
    }

    /// Sets a subject's plan, status and add-ons from your own billing system (platform administrators).
    pub async fn sync_subscription(&self, input: &SubscriptionSync) -> Result<SyncOutcome> {
        self.t
            .json(Req::put("/api/v1/entitlements/subscription").json(input)?)
            .await
    }

    /// Starts a trial of a plan (platform administrators).
    pub async fn start_trial(&self, input: &TrialInput) -> Result<Subscription> {
        self.t.json(Req::post("/api/v1/entitlements/trials").json(input)?).await
    }

    /// Sets the quantity of an add-on on a subject's subscription (platform administrators).
    pub async fn set_subject_addon(&self, subject: &str, addon: &str, quantity: i64) -> Result<Subscription> {
        let body = SubjectAddon {
            subject: subject.to_owned(),
            addon: addon.to_owned(),
            quantity,
        };
        self.t.json(Req::put("/api/v1/entitlements/addons").json(&body)?).await
    }

    /// Changes a gauge (storage, seats) by `delta` or to `value`. A replayed `idempotency_key`
    /// returns the first result; past the limit it fails with a quota error.
    pub async fn gauge(&self, input: &GaugeRequest) -> Result<Consumption> {
        self.t
            .json(Req::post("/api/v1/usage/gauge").json(input)?.idempotent())
            .await
    }

    /// One page of usage events in `seq` order.
    pub async fn usage_events(&self, o: &UsageEventsOptions) -> Result<UsageEventPage> {
        self.t
            .json(
                Req::get("/api/v1/usage/events")
                    .q_opt("subject", o.subject.as_deref())
                    .q_opt("feature", o.feature.as_deref())
                    .q_opt("after", o.after)
                    .q_opt("limit", o.limit),
            )
            .await
    }

    /// Usage alerts (70 / 90 / 100 %) of `subject` (default: the caller's), newest first.
    pub async fn usage_alerts(&self, subject: Option<&str>) -> Result<Vec<UsageAlert>> {
        self.t
            .items(Req::get("/api/v1/usage/alerts").q_opt("subject", subject))
            .await
    }

    /// Runs expiry, rollover and alert maintenance now (platform administrators).
    pub async fn maintenance(&self) -> Result<EntitlementsMaintenanceReport> {
        self.t
            .json(Req::post("/api/v1/usage/maintenance").json_raw(json!({})))
            .await
    }
}

/// Plans: `client.entitlements().plans()`. Reads need `billing:read`; writes need a platform administrator.
#[derive(Clone)]
pub struct EntitlementPlansApi {
    t: Arc<Transport>,
}

impl EntitlementPlansApi {
    /// Every plan.
    pub async fn list(&self) -> Result<Vec<Plan>> {
        self.t.items(Req::get("/api/v1/plans")).await
    }

    /// One plan.
    pub async fn get(&self, key: &str) -> Result<Plan> {
        self.t.json(Req::get(format!("/api/v1/plans/{}", seg(key)))).await
    }

    /// Creates or replaces a plan and its features.
    pub async fn put(&self, key: &str, input: &PlanInput) -> Result<Plan> {
        self.t
            .json(Req::put(format!("/api/v1/plans/{}", seg(key))).json(input)?)
            .await
    }

    /// Deletes a plan no subscription uses.
    pub async fn delete(&self, key: &str) -> Result<()> {
        self.t.unit(Req::delete(format!("/api/v1/plans/{}", seg(key)))).await
    }
}

/// Features of the catalog: `client.entitlements().features()`.
#[derive(Clone)]
pub struct EntitlementFeaturesApi {
    t: Arc<Transport>,
}

impl EntitlementFeaturesApi {
    /// Every feature.
    pub async fn list(&self) -> Result<Vec<Feature>> {
        self.t.items(Req::get("/api/v1/features")).await
    }

    /// One feature.
    pub async fn get(&self, key: &str) -> Result<Feature> {
        self.t.json(Req::get(format!("/api/v1/features/{}", seg(key)))).await
    }

    /// Creates or replaces a feature (`kind`: `boolean`, `metered`, `gauge`, `concurrency` or `rate`).
    pub async fn put(&self, key: &str, input: &FeatureInput) -> Result<Feature> {
        self.t
            .json(Req::put(format!("/api/v1/features/{}", seg(key))).json(input)?)
            .await
    }

    /// Deletes a feature.
    pub async fn delete(&self, key: &str) -> Result<()> {
        self.t.unit(Req::delete(format!("/api/v1/features/{}", seg(key)))).await
    }
}

/// Add-ons of the catalog: `client.entitlements().addons()`.
#[derive(Clone)]
pub struct EntitlementAddonsApi {
    t: Arc<Transport>,
}

impl EntitlementAddonsApi {
    /// Every add-on.
    pub async fn list(&self) -> Result<Vec<Addon>> {
        self.t.items(Req::get("/api/v1/addons")).await
    }

    /// Creates or replaces an add-on.
    pub async fn put(&self, key: &str, input: &AddonInput) -> Result<Addon> {
        self.t
            .json(Req::put(format!("/api/v1/addons/{}", seg(key))).json(input)?)
            .await
    }

    /// Deletes an add-on.
    pub async fn delete(&self, key: &str) -> Result<()> {
        self.t.unit(Req::delete(format!("/api/v1/addons/{}", seg(key)))).await
    }
}

/// Usage packs: the catalog and the packs a subject holds. `client.entitlements().usage_packs()`.
#[derive(Clone)]
pub struct UsagePacksApi {
    t: Arc<Transport>,
}

impl UsagePacksApi {
    /// Usage packs of the catalog.
    pub async fn list(&self) -> Result<Vec<UsagePack>> {
        self.t.items(Req::get("/api/v1/usage-packs")).await
    }

    /// Creates or replaces a usage pack of the catalog.
    pub async fn put(&self, key: &str, input: &UsagePackInput) -> Result<UsagePack> {
        self.t
            .json(Req::put(format!("/api/v1/usage-packs/{}", seg(key))).json(input)?)
            .await
    }

    /// Deletes a usage pack of the catalog.
    pub async fn delete(&self, key: &str) -> Result<()> {
        self.t
            .unit(Req::delete(format!("/api/v1/usage-packs/{}", seg(key))))
            .await
    }

    /// Packs `subject` holds (default: the caller's; `billing:read`).
    pub async fn grants(&self, subject: Option<&str>) -> Result<Vec<PackGrant>> {
        self.t
            .items(Req::get("/api/v1/entitlements/usage-packs").q_opt("subject", subject))
            .await
    }

    /// Grants a pack to a subject (after a purchase). A replayed `idempotency_key` returns the first grant.
    pub async fn grant(&self, input: &PackGrantInput) -> Result<PackGrant> {
        self.t
            .json(Req::post("/api/v1/entitlements/usage-packs").json(input)?.idempotent())
            .await
    }
}

/// Quota policies per feature: `client.entitlements().quota_policies()`.
#[derive(Clone)]
pub struct QuotaPoliciesApi {
    t: Arc<Transport>,
}

impl QuotaPoliciesApi {
    /// Every policy.
    pub async fn list(&self) -> Result<Vec<QuotaPolicy>> {
        self.t.items(Req::get("/api/v1/quota-policies")).await
    }

    /// Creates or replaces the policy of a feature (enforcement, scope, cost, overage, rollover, lease length).
    pub async fn put(&self, feature: &str, policy: &QuotaPolicy) -> Result<QuotaPolicy> {
        self.t
            .json(Req::put(format!("/api/v1/quota-policies/{}", seg(feature))).json(policy)?)
            .await
    }

    /// Deletes the policy of a feature.
    pub async fn delete(&self, feature: &str) -> Result<()> {
        self.t
            .unit(Req::delete(format!("/api/v1/quota-policies/{}", seg(feature))))
            .await
    }
}

/// Per-subject allow / deny overrides of a feature: `client.entitlements().overrides()`. Deny always wins.
#[derive(Clone)]
pub struct EntitlementOverridesApi {
    t: Arc<Transport>,
}

impl EntitlementOverridesApi {
    /// Overrides, of one subject when `subject` is given.
    pub async fn list(&self, subject: Option<&str>) -> Result<Vec<Override>> {
        self.t
            .items(Req::get("/api/v1/entitlements/overrides").q_opt("subject", subject))
            .await
    }

    /// Adds an override.
    pub async fn add(&self, input: &OverrideInput) -> Result<Override> {
        self.t
            .json(Req::post("/api/v1/entitlements/overrides").json(input)?)
            .await
    }

    /// Removes an override.
    pub async fn delete(&self, id: &str) -> Result<()> {
        self.t
            .unit(Req::delete(format!("/api/v1/entitlements/overrides/{}", seg(id))))
            .await
    }
}

/// Enterprise limits of a subject (they replace the plan's grant): `client.entitlements().limits()`.
#[derive(Clone)]
pub struct EntitlementLimitsApi {
    t: Arc<Transport>,
}

impl EntitlementLimitsApi {
    /// Limits, of one subject when `subject` is given.
    pub async fn list(&self, subject: Option<&str>) -> Result<Vec<CustomLimit>> {
        self.t
            .items(Req::get("/api/v1/entitlements/limits").q_opt("subject", subject))
            .await
    }

    /// Sets a limit.
    pub async fn put(&self, limit: &CustomLimit) -> Result<CustomLimit> {
        self.t.json(Req::put("/api/v1/entitlements/limits").json(limit)?).await
    }

    /// Removes a limit.
    pub async fn delete(&self, subject: &str, feature: &str) -> Result<()> {
        self.t
            .unit(
                Req::delete("/api/v1/entitlements/limits")
                    .q("subject", subject)
                    .q("feature", feature),
            )
            .await
    }
}
