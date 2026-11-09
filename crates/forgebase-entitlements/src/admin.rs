//! Catalog administration (plans, features, add-ons, usage packs, quota
//! policies) and subject administration (subscriptions, trials, add-ons,
//! usage packs, overrides, enterprise limits), plus the billing sync seam.
//!
//! Writes are platform-administrator only: a billing manager of an
//! organization must not be able to raise their own limits. Every write is
//! audited in the same transaction and swaps in a fresh catalog.

use forgebase_data::AccessContext;
use forgebase_db::backend::BoxFuture;
use forgebase_db::{Statement, TxMode, Value, WriteTx};
use serde_json::{Value as Json, json};

use crate::error::{EntitlementError, Result};
use crate::model::{
    Addon, AddonInput, AddonQuantity, CustomLimit, Feature, FeatureInput, FeatureKind, Override, OverrideInput,
    PackGrant, PackGrantInput, Plan, PlanFeature, PlanInput, QuotaPolicy, Subscription, SubscriptionStatus,
    SubscriptionSync, SyncOutcome, TrialInput, UsagePack, UsagePackInput,
};
use crate::period::{DAY_MS, Period};
use crate::service::{EntitlementService, parse_subject, validate_idem};
use crate::usage::{Db, int, new_id, opt_int, opt_text, text};

/// Writes one audit row inside a transaction.
pub(crate) async fn audit_tx(
    tx: &mut (dyn WriteTx + 'static),
    now: i64,
    actor: &str,
    action: &str,
    subject: Option<&str>,
    detail: &Json,
) -> Result<()> {
    tx.execute(&Statement::new(
        "INSERT INTO _forgebase_entitlement_audit (at, actor, action, subject, detail) VALUES (?1, ?2, ?3, ?4, ?5)",
        vec![
            now.into(),
            actor.into(),
            action.into(),
            subject.map(str::to_owned).into(),
            detail.to_string().into(),
        ],
    ))
    .await?;
    Ok(())
}

/// Same through a [`Db`].
pub(crate) async fn audit_db(
    db: &mut Db<'_>,
    now: i64,
    actor: &str,
    action: &str,
    subject: Option<&str>,
    detail: &Json,
) -> Result<()> {
    db.exec(
        "INSERT INTO _forgebase_entitlement_audit (at, actor, action, subject, detail) VALUES (?1, ?2, ?3, ?4, ?5)",
        vec![
            now.into(),
            actor.into(),
            action.into(),
            subject.map(str::to_owned).into(),
            detail.to_string().into(),
        ],
    )
    .await?;
    Ok(())
}

fn validate_key(what: &str, key: &str) -> Result<()> {
    let ok = !key.is_empty()
        && key.len() <= 64
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'));
    if ok {
        Ok(())
    } else {
        Err(EntitlementError::invalid(
            "invalid_key",
            format!("{what} key must be 1-64 of [A-Za-z0-9_.-]"),
        ))
    }
}

fn name_of(name: Option<&str>, key: &str) -> Result<String> {
    let n = name.map(str::trim).filter(|n| !n.is_empty()).unwrap_or(key);
    if n.len() > 128 {
        return Err(EntitlementError::invalid(
            "invalid_name",
            "name is at most 128 characters",
        ));
    }
    Ok(n.to_owned())
}

/// Checks a grant's limit and window against the feature kind; returns the
/// window to store.
pub(crate) fn validate_grant(
    kind: FeatureKind,
    feature: &str,
    period: Option<Period>,
    window_secs: Option<i64>,
    limits: &[Option<i64>],
) -> Result<Period> {
    if limits.iter().flatten().any(|l| *l < 0) {
        return Err(EntitlementError::invalid(
            "invalid_limit",
            "limits must not be negative",
        ));
    }
    let bad = |m: String| Err(EntitlementError::invalid("invalid_period", m));
    let p = period.unwrap_or_else(|| crate::catalog::default_period(kind));
    match kind {
        FeatureKind::Metered if p == Period::None => {
            return bad(format!(
                "{feature} is metered: give hour, day, month, lifetime or rolling"
            ));
        }
        FeatureKind::Rate if !matches!(p, Period::Hour | Period::Day) => {
            return bad(format!("{feature} is a rate limit: give hour or day"));
        }
        FeatureKind::Boolean | FeatureKind::Gauge | FeatureKind::Concurrency if p != Period::None => {
            return bad(format!("{feature} is a {} feature: it has no period", kind.as_str()));
        }
        _ => {}
    }
    match (p, window_secs) {
        (Period::Rolling, Some(w)) if (1..=366 * 86_400).contains(&w) => Ok(p),
        (Period::Rolling, _) => bad("a rolling window needs window_secs (1 s to 366 days)".into()),
        (_, Some(_)) => bad("window_secs applies to rolling windows only".into()),
        _ => Ok(p),
    }
}

fn validate_permissions(perms: &[String]) -> Result<()> {
    if perms.len() > 64 {
        return Err(EntitlementError::invalid(
            "invalid_permission",
            "at most 64 permission patterns",
        ));
    }
    for p in perms {
        forgebase_policy::perm::validate(p).map_err(|m| EntitlementError::invalid("invalid_permission", m))?;
    }
    Ok(())
}

/// The seam FB-70 (billing) calls: sets a subject's plan, subscription
/// status and add-ons from payment events, and grants purchased usage packs.
/// Idempotent: replays by event id change nothing, out-of-order events
/// older than the last applied one are ignored. Nothing about payments.
pub trait BillingSync: Send + Sync {
    /// Applies the full subscription state of a subject.
    fn sync_subscription<'a>(&'a self, sync: SubscriptionSync) -> BoxFuture<'a, Result<SyncOutcome>>;
    /// Grants purchased usage packs (idempotent by key).
    fn grant_usage_pack<'a>(&'a self, grant: PackGrantInput) -> BoxFuture<'a, Result<PackGrant>>;
}

impl BillingSync for EntitlementService {
    fn sync_subscription<'a>(&'a self, sync: SubscriptionSync) -> BoxFuture<'a, Result<SyncOutcome>> {
        Box::pin(async move { self.apply_subscription(&AccessContext::system(), sync).await })
    }

    fn grant_usage_pack<'a>(&'a self, grant: PackGrantInput) -> BoxFuture<'a, Result<PackGrant>> {
        Box::pin(async move { self.grant_pack(&AccessContext::system(), grant).await })
    }
}

impl EntitlementService {
    async fn wbegin(&self) -> Result<(Box<dyn WriteTx>, i64)> {
        Ok((self.db().begin(TxMode::Normal).await?, self.now()))
    }

    /// Commits and reloads on success, rolls back on failure.
    async fn wend<T>(&self, tx: Box<dyn WriteTx>, res: Result<T>) -> Result<T> {
        match res {
            Ok(v) => {
                tx.commit().await?;
                self.reload().await?;
                Ok(v)
            }
            Err(e) => {
                let _ = tx.rollback().await;
                Err(e)
            }
        }
    }

    // -----------------------------------------------------------------------
    // Features
    // -----------------------------------------------------------------------

    /// The catalog's features.
    pub fn features(&self, ctx: &AccessContext) -> Result<Vec<Feature>> {
        self.require_caller(ctx)?;
        Ok(self.catalog().features.values().map(feature_out).collect())
    }

    /// One feature.
    pub fn feature(&self, ctx: &AccessContext, key: &str) -> Result<Feature> {
        self.require_caller(ctx)?;
        self.catalog()
            .features
            .get(key)
            .map(feature_out)
            .ok_or_else(|| EntitlementError::NotFound(format!("feature {key}")))
    }

    /// Creates or replaces a feature.
    pub async fn put_feature(&self, ctx: &AccessContext, key: &str, input: FeatureInput) -> Result<Feature> {
        self.require_admin(ctx)?;
        validate_key("feature", key)?;
        validate_permissions(&input.permissions)?;
        let name = name_of(input.name.as_deref(), key)?;
        let unit = input.unit.unwrap_or_default();
        if unit.len() > 32 {
            return Err(EntitlementError::invalid(
                "invalid_unit",
                "unit is at most 32 characters",
            ));
        }
        if let Some(old) = self.catalog().features.get(key)
            && old.kind != input.kind
        {
            let used = self
                .db()
                .fetch_all(&Statement::new(
                    "SELECT 1 FROM _forgebase_plan_features WHERE feature_key = ?1
                     UNION ALL SELECT 1 FROM _forgebase_addon_features WHERE feature_key = ?1 LIMIT 1",
                    vec![key.into()],
                ))
                .await?;
            if !used.is_empty() {
                return Err(EntitlementError::conflict(
                    "feature_in_use",
                    "remove the feature from plans and add-ons before changing its kind",
                ));
            }
        }
        let actor = ctx.principal_key();
        let (mut tx, now) = self.wbegin().await?;
        let res = async {
            let tx = tx.as_mut();
            tx.execute(&Statement::new(
                "INSERT INTO _forgebase_features (key, name, description, kind, unit, permissions, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
                 ON CONFLICT (key) DO UPDATE SET name = excluded.name, description = excluded.description,
                   kind = excluded.kind, unit = excluded.unit, permissions = excluded.permissions,
                   updated_at = excluded.updated_at",
                vec![
                    key.into(),
                    name.clone().into(),
                    input.description.clone().unwrap_or_default().into(),
                    input.kind.as_str().into(),
                    unit.clone().into(),
                    serde_json::to_string(&input.permissions)?.into(),
                    now.into(),
                ],
            ))
            .await?;
            audit_tx(tx, now, &actor, "feature_put", None, &json!({"key": key, "kind": input.kind})).await
        }
        .await;
        self.wend(tx, res).await?;
        self.feature(ctx, key)
    }

    /// Deletes a feature (and its grants in plans, add-ons and policies).
    pub async fn delete_feature(&self, ctx: &AccessContext, key: &str) -> Result<()> {
        self.require_admin(ctx)?;
        let actor = ctx.principal_key();
        let (mut tx, now) = self.wbegin().await?;
        let res = async {
            let tx = tx.as_mut();
            let n = tx
                .execute(&Statement::new(
                    "DELETE FROM _forgebase_features WHERE key = ?1",
                    vec![key.into()],
                ))
                .await?
                .rows_affected;
            if n == 0 {
                return Err(EntitlementError::NotFound(format!("feature {key}")));
            }
            audit_tx(tx, now, &actor, "feature_delete", None, &json!({"key": key})).await
        }
        .await;
        self.wend(tx, res).await
    }

    // -----------------------------------------------------------------------
    // Plans
    // -----------------------------------------------------------------------

    /// The plans.
    pub fn plans(&self, ctx: &AccessContext) -> Result<Vec<Plan>> {
        self.require_caller(ctx)?;
        Ok(self.catalog().plans.values().map(plan_out).collect())
    }

    /// One plan.
    pub fn plan(&self, ctx: &AccessContext, key: &str) -> Result<Plan> {
        self.require_caller(ctx)?;
        self.catalog()
            .plans
            .get(key)
            .map(plan_out)
            .ok_or_else(|| EntitlementError::NotFound(format!("plan {key}")))
    }

    /// Creates or replaces a plan and its features.
    pub async fn put_plan(&self, ctx: &AccessContext, key: &str, input: PlanInput) -> Result<Plan> {
        self.require_admin(ctx)?;
        validate_key("plan", key)?;
        let name = name_of(input.name.as_deref(), key)?;
        if !(0..=3650).contains(&input.trial_days) {
            return Err(EntitlementError::invalid("invalid_trial", "trial_days must be 0-3650"));
        }
        let cat = self.catalog();
        let mut features = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for pf in &input.features {
            let f = cat
                .features
                .get(&pf.feature)
                .ok_or_else(|| EntitlementError::NotFound(format!("feature {}", pf.feature)))?;
            if !seen.insert(pf.feature.clone()) {
                return Err(EntitlementError::invalid(
                    "duplicate_feature",
                    format!("{} is listed twice", pf.feature),
                ));
            }
            let period = validate_grant(f.kind, &f.key, pf.period, pf.window_secs, &[pf.limit, pf.trial_limit])?;
            features.push((pf.clone(), period));
        }
        let metadata = input.metadata.clone().unwrap_or_else(|| json!({}));
        if !metadata.is_object() {
            return Err(EntitlementError::invalid(
                "invalid_metadata",
                "metadata must be an object",
            ));
        }
        let existing = cat.plans.get(key).map(|p| p.id.clone());
        let actor = ctx.principal_key();
        let (mut tx, now) = self.wbegin().await?;
        let res = async {
            let tx = tx.as_mut();
            if input.is_default {
                tx.execute(&Statement::new(
                    "UPDATE _forgebase_plans SET is_default = 0, updated_at = ?1 WHERE is_default = 1 AND key <> ?2",
                    vec![now.into(), key.into()],
                ))
                .await?;
            }
            let id = existing.clone().unwrap_or_else(|| new_id("pln"));
            tx.execute(&Statement::new(
                "INSERT INTO _forgebase_plans (id, key, name, description, is_default, active, trial_days, metadata, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)
                 ON CONFLICT (key) DO UPDATE SET name = excluded.name, description = excluded.description,
                   is_default = excluded.is_default, active = excluded.active, trial_days = excluded.trial_days,
                   metadata = excluded.metadata, updated_at = excluded.updated_at",
                vec![
                    id.clone().into(),
                    key.into(),
                    name.clone().into(),
                    input.description.clone().unwrap_or_default().into(),
                    input.is_default.into(),
                    input.active.into(),
                    input.trial_days.into(),
                    metadata.to_string().into(),
                    now.into(),
                ],
            ))
            .await?;
            tx.execute(&Statement::new(
                "DELETE FROM _forgebase_plan_features WHERE plan_id = ?1",
                vec![id.clone().into()],
            ))
            .await?;
            for (pf, period) in &features {
                tx.execute(&Statement::new(
                    "INSERT INTO _forgebase_plan_features (plan_id, feature_key, enabled, limit_value, unit, period, window_secs, trial_limit)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    vec![
                        id.clone().into(),
                        pf.feature.clone().into(),
                        pf.enabled.into(),
                        pf.limit.into(),
                        pf.unit.clone().into(),
                        period.as_str().into(),
                        pf.window_secs.into(),
                        pf.trial_limit.into(),
                    ],
                ))
                .await?;
            }
            audit_tx(
                tx,
                now,
                &actor,
                "plan_put",
                None,
                &json!({"key": key, "features": input.features.len(), "is_default": input.is_default}),
            )
            .await
        }
        .await;
        self.wend(tx, res).await?;
        self.plan(ctx, key)
    }

    /// Deletes a plan no subscription uses.
    pub async fn delete_plan(&self, ctx: &AccessContext, key: &str) -> Result<()> {
        self.require_admin(ctx)?;
        let Some(id) = self.catalog().plans.get(key).map(|p| p.id.clone()) else {
            return Err(EntitlementError::NotFound(format!("plan {key}")));
        };
        let actor = ctx.principal_key();
        let (mut tx, now) = self.wbegin().await?;
        let res = async {
            let tx = tx.as_mut();
            let used = tx
                .fetch_all(&Statement::new(
                    "SELECT 1 FROM _forgebase_subscriptions WHERE plan_id = ?1 LIMIT 1",
                    vec![id.clone().into()],
                ))
                .await?;
            if !used.is_empty() {
                return Err(EntitlementError::conflict(
                    "plan_in_use",
                    "subscriptions use this plan; move them first or deactivate it",
                ));
            }
            tx.execute(&Statement::new(
                "DELETE FROM _forgebase_plans WHERE id = ?1",
                vec![id.clone().into()],
            ))
            .await?;
            audit_tx(tx, now, &actor, "plan_delete", None, &json!({"key": key})).await
        }
        .await;
        self.wend(tx, res).await
    }

    // -----------------------------------------------------------------------
    // Add-ons and usage packs
    // -----------------------------------------------------------------------

    /// The add-ons.
    pub fn addons(&self, ctx: &AccessContext) -> Result<Vec<Addon>> {
        self.require_caller(ctx)?;
        Ok(self
            .catalog()
            .addons
            .values()
            .map(|a| Addon {
                key: a.key.clone(),
                name: a.name.clone(),
                description: a.description.clone(),
                active: a.active,
                features: a.features.clone(),
            })
            .collect())
    }

    /// Creates or replaces an add-on.
    pub async fn put_addon(&self, ctx: &AccessContext, key: &str, input: AddonInput) -> Result<Addon> {
        self.require_admin(ctx)?;
        validate_key("add-on", key)?;
        let name = name_of(input.name.as_deref(), key)?;
        let cat = self.catalog();
        let mut grants = Vec::new();
        for af in &input.features {
            let f = cat
                .features
                .get(&af.feature)
                .ok_or_else(|| EntitlementError::NotFound(format!("feature {}", af.feature)))?;
            let period = validate_grant(f.kind, &f.key, af.period, af.window_secs, &[af.limit])?;
            grants.push((af.clone(), period));
        }
        let existing = cat.addons.get(key).map(|a| a.id.clone());
        let actor = ctx.principal_key();
        let (mut tx, now) = self.wbegin().await?;
        let res = async {
            let tx = tx.as_mut();
            let id = existing.clone().unwrap_or_else(|| new_id("add"));
            tx.execute(&Statement::new(
                "INSERT INTO _forgebase_addons (id, key, name, description, active, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
                 ON CONFLICT (key) DO UPDATE SET name = excluded.name, description = excluded.description,
                   active = excluded.active, updated_at = excluded.updated_at",
                vec![
                    id.clone().into(),
                    key.into(),
                    name.clone().into(),
                    input.description.clone().unwrap_or_default().into(),
                    input.active.into(),
                    now.into(),
                ],
            ))
            .await?;
            tx.execute(&Statement::new(
                "DELETE FROM _forgebase_addon_features WHERE addon_id = ?1",
                vec![id.clone().into()],
            ))
            .await?;
            for (af, period) in &grants {
                tx.execute(&Statement::new(
                    "INSERT INTO _forgebase_addon_features (addon_id, feature_key, limit_value, period, window_secs)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    vec![
                        id.clone().into(),
                        af.feature.clone().into(),
                        af.limit.into(),
                        period.as_str().into(),
                        af.window_secs.into(),
                    ],
                ))
                .await?;
            }
            audit_tx(tx, now, &actor, "addon_put", None, &json!({"key": key})).await
        }
        .await;
        self.wend(tx, res).await?;
        self.addons(ctx)?
            .into_iter()
            .find(|a| a.key == key)
            .ok_or_else(|| EntitlementError::Internal("add-on vanished".into()))
    }

    /// Deletes an add-on (subjects holding it lose it).
    pub async fn delete_addon(&self, ctx: &AccessContext, key: &str) -> Result<()> {
        self.require_admin(ctx)?;
        let actor = ctx.principal_key();
        let (mut tx, now) = self.wbegin().await?;
        let res = async {
            let tx = tx.as_mut();
            let n = tx
                .execute(&Statement::new(
                    "DELETE FROM _forgebase_addons WHERE key = ?1",
                    vec![key.into()],
                ))
                .await?
                .rows_affected;
            if n == 0 {
                return Err(EntitlementError::NotFound(format!("add-on {key}")));
            }
            audit_tx(tx, now, &actor, "addon_delete", None, &json!({"key": key})).await
        }
        .await;
        self.wend(tx, res).await
    }

    /// The usage packs of the catalog.
    pub fn usage_packs(&self, ctx: &AccessContext) -> Result<Vec<UsagePack>> {
        self.require_caller(ctx)?;
        Ok(self
            .catalog()
            .packs
            .values()
            .map(|p| UsagePack {
                key: p.key.clone(),
                name: p.name.clone(),
                feature: p.feature.clone(),
                units: p.units,
                valid_days: p.valid_days,
                active: p.active,
            })
            .collect())
    }

    /// Creates or replaces a usage pack.
    pub async fn put_usage_pack(&self, ctx: &AccessContext, key: &str, input: UsagePackInput) -> Result<UsagePack> {
        self.require_admin(ctx)?;
        validate_key("usage pack", key)?;
        let name = name_of(input.name.as_deref(), key)?;
        let cat = self.catalog();
        let f = cat
            .features
            .get(&input.feature)
            .ok_or_else(|| EntitlementError::NotFound(format!("feature {}", input.feature)))?;
        if f.kind != FeatureKind::Metered {
            return Err(EntitlementError::invalid(
                "wrong_feature_kind",
                "usage packs add units to metered features",
            ));
        }
        if input.units < 1 || input.valid_days.is_some_and(|d| !(1..=3650).contains(&d)) {
            return Err(EntitlementError::invalid(
                "invalid_pack",
                "units must be at least 1 and valid_days 1-3650",
            ));
        }
        let existing = cat.packs.get(key).map(|p| p.id.clone());
        let actor = ctx.principal_key();
        let (mut tx, now) = self.wbegin().await?;
        let res = async {
            let tx = tx.as_mut();
            let id = existing.clone().unwrap_or_else(|| new_id("upk"));
            tx.execute(&Statement::new(
                "INSERT INTO _forgebase_usage_packs (id, key, name, feature_key, units, valid_days, active, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT (key) DO UPDATE SET name = excluded.name, feature_key = excluded.feature_key,
                   units = excluded.units, valid_days = excluded.valid_days, active = excluded.active",
                vec![
                    id.into(),
                    key.into(),
                    name.clone().into(),
                    input.feature.clone().into(),
                    input.units.into(),
                    input.valid_days.into(),
                    input.active.into(),
                    now.into(),
                ],
            ))
            .await?;
            audit_tx(tx, now, &actor, "usage_pack_put", None, &json!({"key": key})).await
        }
        .await;
        self.wend(tx, res).await?;
        self.usage_packs(ctx)?
            .into_iter()
            .find(|p| p.key == key)
            .ok_or_else(|| EntitlementError::Internal("usage pack vanished".into()))
    }

    /// Deletes a usage pack from the catalog (granted units stay).
    pub async fn delete_usage_pack(&self, ctx: &AccessContext, key: &str) -> Result<()> {
        self.require_admin(ctx)?;
        let actor = ctx.principal_key();
        let (mut tx, now) = self.wbegin().await?;
        let res = async {
            let tx = tx.as_mut();
            let n = tx
                .execute(&Statement::new(
                    "DELETE FROM _forgebase_usage_packs WHERE key = ?1",
                    vec![key.into()],
                ))
                .await?
                .rows_affected;
            if n == 0 {
                return Err(EntitlementError::NotFound(format!("usage pack {key}")));
            }
            audit_tx(tx, now, &actor, "usage_pack_delete", None, &json!({"key": key})).await
        }
        .await;
        self.wend(tx, res).await
    }

    // -----------------------------------------------------------------------
    // Quota policies
    // -----------------------------------------------------------------------

    /// The quota policies.
    pub fn quota_policies(&self, ctx: &AccessContext) -> Result<Vec<QuotaPolicy>> {
        self.require_caller(ctx)?;
        let mut v: Vec<QuotaPolicy> = self.catalog().policies.values().cloned().collect();
        v.sort_by(|a, b| a.feature.cmp(&b.feature));
        Ok(v)
    }

    /// Creates or replaces the quota policy of a feature.
    pub async fn put_quota_policy(
        &self,
        ctx: &AccessContext,
        feature: &str,
        mut p: QuotaPolicy,
    ) -> Result<QuotaPolicy> {
        self.require_admin(ctx)?;
        let kind = self
            .catalog()
            .features
            .get(feature)
            .map(|f| f.kind)
            .ok_or_else(|| EntitlementError::NotFound(format!("feature {feature}")))?;
        if !matches!(
            kind,
            FeatureKind::Metered | FeatureKind::Gauge | FeatureKind::Concurrency
        ) {
            return Err(EntitlementError::invalid(
                "wrong_feature_kind",
                "quota policies apply to metered, gauge and concurrency features",
            ));
        }
        p.feature = feature.to_owned();
        validate_permissions(&p.permissions)?;
        let bad = |m: &str| Err(EntitlementError::invalid("invalid_policy", m.to_owned()));
        if !(0..=100).contains(&p.rollover_pct) {
            return bad("rollover_pct must be 0-100");
        }
        if p.rollover_cap.is_some_and(|c| c < 0) || p.overage_pct.is_some_and(|c| c < 0) {
            return bad("rollover_cap and overage_pct must not be negative");
        }
        if p.cost < 1 || !(1..=crate::service::MAX_TTL_SECS).contains(&p.lease_ttl_secs) {
            return bad("cost must be at least 1 and lease_ttl_secs 1-86400");
        }
        let actor = ctx.principal_key();
        let (mut tx, now) = self.wbegin().await?;
        let res = async {
            let tx = tx.as_mut();
            tx.execute(&Statement::new(
                "INSERT INTO _forgebase_quota_policies
                   (feature_key, scope, enforcement, overage_pct, rollover_pct, rollover_cap, cost, lease_ttl_secs, permissions, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT (feature_key) DO UPDATE SET scope = excluded.scope, enforcement = excluded.enforcement,
                   overage_pct = excluded.overage_pct, rollover_pct = excluded.rollover_pct,
                   rollover_cap = excluded.rollover_cap, cost = excluded.cost,
                   lease_ttl_secs = excluded.lease_ttl_secs, permissions = excluded.permissions,
                   updated_at = excluded.updated_at",
                vec![
                    feature.into(),
                    p.scope.as_str().into(),
                    if p.enforcement == crate::model::Enforcement::Soft { "soft" } else { "hard" }.into(),
                    p.overage_pct.into(),
                    p.rollover_pct.into(),
                    p.rollover_cap.into(),
                    p.cost.into(),
                    p.lease_ttl_secs.into(),
                    serde_json::to_string(&p.permissions)?.into(),
                    now.into(),
                ],
            ))
            .await?;
            audit_tx(tx, now, &actor, "quota_policy_put", None, &serde_json::to_value(&p)?).await
        }
        .await;
        self.wend(tx, res).await?;
        Ok(self.catalog().policy(feature))
    }

    /// Deletes a quota policy (defaults apply again).
    pub async fn delete_quota_policy(&self, ctx: &AccessContext, feature: &str) -> Result<()> {
        self.require_admin(ctx)?;
        let actor = ctx.principal_key();
        let (mut tx, now) = self.wbegin().await?;
        let res = async {
            let tx = tx.as_mut();
            let n = tx
                .execute(&Statement::new(
                    "DELETE FROM _forgebase_quota_policies WHERE feature_key = ?1",
                    vec![feature.into()],
                ))
                .await?
                .rows_affected;
            if n == 0 {
                return Err(EntitlementError::NotFound(format!("quota policy of {feature}")));
            }
            audit_tx(
                tx,
                now,
                &actor,
                "quota_policy_delete",
                None,
                &json!({"feature": feature}),
            )
            .await
        }
        .await;
        self.wend(tx, res).await
    }

    // -----------------------------------------------------------------------
    // Subscriptions
    // -----------------------------------------------------------------------

    /// A subject's subscription.
    pub fn subscription(&self, ctx: &AccessContext, subject: &str) -> Result<Option<Subscription>> {
        parse_subject(subject)?;
        self.can_read_usage(ctx, subject)?;
        Ok(self.subscription_of(subject))
    }

    fn subscription_of(&self, subject: &str) -> Option<Subscription> {
        let cat = self.catalog();
        let s = cat.subscriptions.get(subject)?;
        let mut addons: Vec<AddonQuantity> = cat
            .sub_addons
            .get(subject)
            .into_iter()
            .flatten()
            .filter_map(|(id, q)| {
                cat.addon_ids.get(id).map(|k| AddonQuantity {
                    addon: k.clone(),
                    quantity: *q,
                })
            })
            .collect();
        addons.sort_by(|a, b| a.addon.cmp(&b.addon));
        Some(Subscription {
            subject: s.subject.clone(),
            plan: cat.plan_key(&s.plan_id).unwrap_or_default().to_owned(),
            status: s.status,
            trial_ends_at: s.trial_ends_at,
            current_period_start: s.current_period_start,
            current_period_end: s.current_period_end,
            cycle_anchor: s.cycle_anchor,
            cancel_at: s.cancel_at,
            external_ref: s.external_ref.clone(),
            addons,
            updated_at: s.updated_at,
        })
    }

    /// Sets a subject's subscription (administrators, and FB-70 through
    /// [`BillingSync`]). Idempotent by `event_id`; ordered by `event_at`.
    pub async fn apply_subscription(&self, ctx: &AccessContext, sync: SubscriptionSync) -> Result<SyncOutcome> {
        self.require_admin(ctx)?;
        parse_subject(&sync.subject)?;
        if let Some(e) = &sync.event_id
            && (e.is_empty() || e.len() > 256)
        {
            return Err(EntitlementError::invalid(
                "invalid_event_id",
                "event_id must be 1-256 characters",
            ));
        }
        let cat = self.catalog();
        let plan = cat
            .plans
            .get(&sync.plan)
            .ok_or_else(|| EntitlementError::NotFound(format!("plan {}", sync.plan)))?;
        let plan_id = plan.id.clone();
        let mut addon_ids = Vec::new();
        for a in sync.addons.iter().flatten() {
            let id = cat
                .addons
                .get(&a.addon)
                .map(|x| x.id.clone())
                .ok_or_else(|| EntitlementError::NotFound(format!("add-on {}", a.addon)))?;
            if a.quantity < 0 {
                return Err(EntitlementError::invalid(
                    "invalid_quantity",
                    "quantity must not be negative",
                ));
            }
            addon_ids.push((id, a.quantity));
        }
        if sync.status == SubscriptionStatus::Trialing && sync.trial_ends_at.is_none() {
            return Err(EntitlementError::invalid(
                "trial_end_required",
                "a trialing subscription needs trial_ends_at",
            ));
        }
        drop(cat);
        let actor = ctx.principal_key();
        let (mut tx, now) = self.wbegin().await?;
        let res = async {
            let tx = tx.as_mut();
                if let Some(e) = &sync.event_id {
                    let rows = tx
                        .fetch_all(&Statement::new(
                            "SELECT outcome FROM _forgebase_billing_sync_events WHERE event_id = ?1",
                            vec![e.clone().into()],
                        ))
                        .await?;
                    if let Some(r) = rows.rows.first() {
                        let mut o: SyncOutcome = serde_json::from_str(&text(r, 0))?;
                        o.result = "replayed".into();
                        return Ok(o);
                    }
                }
            // Read inside the transaction: concurrent webhooks are ordered by the writer.
            let previous = tx
                .fetch_all(&Statement::new(
                    "SELECT source_event_at FROM _forgebase_subscriptions WHERE subject = ?1",
                    vec![sync.subject.clone().into()],
                ))
                .await?
                .rows
                .first()
                .and_then(|r| opt_int(r, 0));
            let stale = matches!((sync.event_at, previous), (Some(at), Some(prev)) if at < prev);
                let outcome = if stale {
                    let cur = tx
                        .fetch_all(&Statement::new(
                            "SELECT p.key, s.status FROM _forgebase_subscriptions s JOIN _forgebase_plans p ON p.id = s.plan_id
                             WHERE s.subject = ?1",
                            vec![sync.subject.clone().into()],
                        ))
                        .await?;
                    let r = cur
                        .rows
                        .first()
                        .ok_or_else(|| EntitlementError::Internal("subscription vanished".into()))?;
                    SyncOutcome {
                        result: "stale".into(),
                        subject: sync.subject.clone(),
                        plan: text(r, 0),
                        status: SubscriptionStatus::parse(&text(r, 1)).unwrap_or(SubscriptionStatus::Expired),
                    }
                } else {
                    tx.execute(&Statement::new(
                        "INSERT INTO _forgebase_subscriptions
                           (subject, plan_id, status, trial_ends_at, current_period_start, current_period_end, cycle_anchor,
                            cancel_at, external_ref, source_event_at, created_at, updated_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)
                         ON CONFLICT (subject) DO UPDATE SET plan_id = excluded.plan_id, status = excluded.status,
                           trial_ends_at = excluded.trial_ends_at, current_period_start = excluded.current_period_start,
                           current_period_end = excluded.current_period_end, cycle_anchor = excluded.cycle_anchor,
                           cancel_at = excluded.cancel_at, external_ref = excluded.external_ref,
                           source_event_at = COALESCE(excluded.source_event_at, _forgebase_subscriptions.source_event_at),
                           updated_at = excluded.updated_at",
                        vec![
                            sync.subject.clone().into(),
                            plan_id.clone().into(),
                            sync.status.as_str().into(),
                            sync.trial_ends_at.into(),
                            sync.current_period_start.into(),
                            sync.current_period_end.into(),
                            sync.cycle_anchor.into(),
                            sync.cancel_at.into(),
                            sync.external_ref.clone().into(),
                            sync.event_at.into(),
                            now.into(),
                        ],
                    ))
                    .await?;
                    if sync.addons.is_some() {
                        tx.execute(&Statement::new(
                            "DELETE FROM _forgebase_subscription_addons WHERE subject = ?1",
                            vec![sync.subject.clone().into()],
                        ))
                        .await?;
                        for (id, q) in addon_ids.iter().filter(|(_, q)| *q > 0) {
                            tx.execute(&Statement::new(
                                "INSERT INTO _forgebase_subscription_addons (subject, addon_id, quantity, updated_at)
                                 VALUES (?1, ?2, ?3, ?4)",
                                vec![sync.subject.clone().into(), id.clone().into(), (*q).into(), now.into()],
                            ))
                            .await?;
                        }
                    }
                    SyncOutcome {
                        result: "applied".into(),
                        subject: sync.subject.clone(),
                        plan: sync.plan.clone(),
                        status: sync.status,
                    }
                };
                if let Some(e) = &sync.event_id {
                    tx.execute(&Statement::new(
                        "INSERT INTO _forgebase_billing_sync_events (event_id, subject, outcome, applied_at) VALUES (?1, ?2, ?3, ?4)",
                        vec![
                            e.clone().into(),
                            sync.subject.clone().into(),
                            serde_json::to_string(&outcome)?.into(),
                            now.into(),
                        ],
                    ))
                    .await?;
                }
                audit_tx(
                    tx,
                    now,
                    &actor,
                    "subscription_sync",
                    Some(&sync.subject),
                    &json!({"plan": sync.plan, "status": sync.status, "result": outcome.result, "event_id": sync.event_id}),
                )
                .await?;
                Ok(outcome)
            }
        .await;
        let outcome = self.wend(tx, res).await?;
        Ok(outcome)
    }

    /// Starts a trial of `plan` for a subject.
    pub async fn start_trial(&self, ctx: &AccessContext, input: TrialInput) -> Result<Subscription> {
        self.require_admin(ctx)?;
        let cat = self.catalog();
        let plan = cat
            .plans
            .get(&input.plan)
            .ok_or_else(|| EntitlementError::NotFound(format!("plan {}", input.plan)))?;
        let days = input.days.unwrap_or(plan.trial_days);
        if !(1..=3650).contains(&days) {
            return Err(EntitlementError::invalid("invalid_trial", "a trial lasts 1-3650 days"));
        }
        drop(cat);
        let now = self.now();
        let subject = input.subject.clone();
        self.apply_subscription(
            ctx,
            SubscriptionSync {
                subject: input.subject,
                plan: input.plan,
                status: SubscriptionStatus::Trialing,
                trial_ends_at: Some(now + days * DAY_MS),
                current_period_start: Some(now),
                current_period_end: Some(now + days * DAY_MS),
                cycle_anchor: None,
                cancel_at: None,
                external_ref: None,
                addons: None,
                event_id: None,
                event_at: None,
            },
        )
        .await?;
        self.subscription_of(&subject)
            .ok_or_else(|| EntitlementError::Internal("subscription vanished".into()))
    }

    /// Sets the quantity of one add-on of a subject (0 removes it).
    pub async fn set_addon(&self, ctx: &AccessContext, subject: &str, a: AddonQuantity) -> Result<Subscription> {
        self.require_admin(ctx)?;
        parse_subject(subject)?;
        let cat = self.catalog();
        if !cat.subscriptions.contains_key(subject) {
            return Err(EntitlementError::NotFound("subscription".into()));
        }
        let id = cat
            .addons
            .get(&a.addon)
            .map(|x| x.id.clone())
            .ok_or_else(|| EntitlementError::NotFound(format!("add-on {}", a.addon)))?;
        if a.quantity < 0 {
            return Err(EntitlementError::invalid(
                "invalid_quantity",
                "quantity must not be negative",
            ));
        }
        drop(cat);
        let actor = ctx.principal_key();
        let (mut tx, now) = self.wbegin().await?;
        let res = async {
            let tx = tx.as_mut();
            if a.quantity == 0 {
                tx.execute(&Statement::new(
                    "DELETE FROM _forgebase_subscription_addons WHERE subject = ?1 AND addon_id = ?2",
                    vec![subject.into(), id.clone().into()],
                ))
                .await?;
            } else {
                tx.execute(&Statement::new(
                    "INSERT INTO _forgebase_subscription_addons (subject, addon_id, quantity, updated_at) VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT (subject, addon_id) DO UPDATE SET quantity = excluded.quantity, updated_at = excluded.updated_at",
                    vec![subject.into(), id.clone().into(), a.quantity.into(), now.into()],
                ))
                .await?;
            }
            audit_tx(tx, now, &actor, "addon_set", Some(subject), &json!({"addon": a.addon, "quantity": a.quantity})).await
        }
        .await;
        self.wend(tx, res).await?;
        self.subscription_of(subject)
            .ok_or_else(|| EntitlementError::Internal("subscription vanished".into()))
    }

    // -----------------------------------------------------------------------
    // Usage packs held
    // -----------------------------------------------------------------------

    /// Grants usage packs to a subject. Idempotent by `idempotency_key`.
    pub async fn grant_pack(&self, ctx: &AccessContext, input: PackGrantInput) -> Result<PackGrant> {
        self.require_admin(ctx)?;
        parse_subject(&input.subject)?;
        let idem = input
            .idempotency_key
            .as_deref()
            .map(|k| validate_idem(Some(k)))
            .transpose()?;
        if !(1..=10_000).contains(&input.quantity) {
            return Err(EntitlementError::invalid(
                "invalid_quantity",
                "quantity must be 1-10000",
            ));
        }
        let cat = self.catalog();
        let pack = cat
            .packs
            .get(&input.pack)
            .filter(|p| p.active)
            .ok_or_else(|| EntitlementError::NotFound(format!("usage pack {}", input.pack)))?
            .clone();
        drop(cat);
        let actor = ctx.principal_key();
        let now = self.now();
        let mut tx = self.db().begin(TxMode::Normal).await?;
        let seq = self.shared().usage.next_seq();
        let res: Result<(PackGrant, bool, i64)> = async {
            if let Some(k) = &idem {
                let rows = tx
                    .fetch_all(&Statement::new(
                        "SELECT id FROM _forgebase_pack_grants WHERE subject = ?1 AND idempotency_key = ?2",
                        vec![input.subject.clone().into(), k.clone().into()],
                    ))
                    .await?;
                if let Some(r) = rows.rows.first() {
                    let g = grant_row(tx.as_mut(), &text(r, 0), &self.catalog()).await?;
                    return Ok((g, false, 0));
                }
            }
            let id = new_id("pck");
            let units = pack.units.saturating_mul(input.quantity);
            let expires_at = pack.valid_days.map(|d| now + d * DAY_MS);
            tx.execute(&Statement::new(
                "INSERT INTO _forgebase_pack_grants
                   (id, subject, pack_id, feature_key, units, remaining, expires_at, idempotency_key, created_by, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6, ?7, ?8, ?9)",
                vec![
                    id.clone().into(),
                    input.subject.clone().into(),
                    pack.id.clone().into(),
                    pack.feature.clone().into(),
                    units.into(),
                    expires_at.into(),
                    idem.clone().into(),
                    actor.clone().into(),
                    now.into(),
                ],
            ))
            .await?;
            audit_tx(
                tx.as_mut(),
                now,
                &actor,
                "usage_pack_grant",
                Some(&input.subject),
                &json!({"pack": input.pack, "quantity": input.quantity, "units": units}),
            )
            .await?;
            let mut db = Db::Tx(tx.as_mut());
            let balance = crate::usage::pack_balance(&mut db, &pack.feature, &input.subject, now).await?;
            let g = grant_row(tx.as_mut(), &id, &self.catalog()).await?;
            Ok((g, true, balance))
        }
        .await;
        match res {
            Ok((g, true, balance)) => {
                tx.commit().await?;
                self.shared().usage.put_packs(&g.feature, &g.subject, balance, seq);
                Ok(g)
            }
            Ok((g, false, _)) => {
                tx.rollback().await?;
                Ok(g)
            }
            Err(e) => {
                let _ = tx.rollback().await;
                Err(e)
            }
        }
    }

    /// Usage packs a subject holds.
    pub async fn pack_grants(&self, ctx: &AccessContext, subject: &str) -> Result<Vec<PackGrant>> {
        parse_subject(subject)?;
        self.can_read_usage(ctx, subject)?;
        let cat = self.catalog();
        let rows = self
            .db()
            .fetch_all(&Statement::new(
                "SELECT id, subject, pack_id, feature_key, units, remaining, expires_at, created_at
                 FROM _forgebase_pack_grants WHERE subject = ?1 ORDER BY created_at, id",
                vec![subject.into()],
            ))
            .await?;
        Ok(rows.rows.iter().map(|r| grant_out(r, &cat)).collect())
    }

    // -----------------------------------------------------------------------
    // Overrides and enterprise limits
    // -----------------------------------------------------------------------

    /// Overrides, optionally of one subject.
    pub fn overrides(&self, ctx: &AccessContext, subject: Option<&str>) -> Result<Vec<Override>> {
        self.require_admin(ctx)?;
        Ok(self
            .catalog()
            .overrides
            .iter()
            .filter(|o| subject.is_none() || o.subject.as_deref() == subject)
            .cloned()
            .collect())
    }

    /// Adds an override.
    pub async fn add_override(&self, ctx: &AccessContext, input: OverrideInput) -> Result<Override> {
        self.require_admin(ctx)?;
        if input.subject.is_none() && input.member.is_none() {
            return Err(EntitlementError::invalid(
                "invalid_override",
                "an override names a subject, a member or both",
            ));
        }
        if let Some(s) = &input.subject {
            parse_subject(s)?;
        }
        if let Some(m) = &input.member
            && (parse_subject(m)?.is_some())
        {
            return Err(EntitlementError::invalid(
                "invalid_member",
                "member must be a principal key",
            ));
        }
        let effect = match input.effect.as_str() {
            "allow" | "deny" => input.effect.clone(),
            _ => {
                return Err(EntitlementError::invalid(
                    "invalid_effect",
                    "effect must be allow or deny",
                ));
            }
        };
        let cat = self.catalog();
        let kind = cat
            .features
            .get(&input.feature)
            .map(|f| f.kind)
            .ok_or_else(|| EntitlementError::NotFound(format!("feature {}", input.feature)))?;
        let period = if input.period.is_some() || input.window_secs.is_some() {
            Some(validate_grant(
                kind,
                &input.feature,
                input.period,
                input.window_secs,
                &[input.limit],
            )?)
        } else {
            validate_grant(kind, &input.feature, None, None, &[input.limit])?;
            None
        };
        drop(cat);
        let id = new_id("ovr");
        let actor = ctx.principal_key();
        let (mut tx, now) = self.wbegin().await?;
        let res = async {
            let tx = tx.as_mut();
            tx.execute(&Statement::new(
                "INSERT INTO _forgebase_entitlement_overrides
                   (id, subject, member, feature_key, effect, limit_value, period, window_secs, expires_at, reason, created_by, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                vec![
                    id.clone().into(),
                    input.subject.clone().into(),
                    input.member.clone().into(),
                    input.feature.clone().into(),
                    effect.clone().into(),
                    input.limit.into(),
                    period.map(Period::as_str).into(),
                    input.window_secs.into(),
                    input.expires_at.into(),
                    input.reason.clone().into(),
                    actor.clone().into(),
                    now.into(),
                ],
            ))
            .await?;
            audit_tx(
                tx,
                now,
                &actor,
                "override_add",
                input.subject.as_deref(),
                &json!({"id": id, "member": input.member, "feature": input.feature, "effect": effect, "limit": input.limit}),
            )
            .await
        }
        .await;
        self.wend(tx, res).await?;
        self.catalog()
            .overrides
            .iter()
            .find(|o| o.id == id)
            .cloned()
            .ok_or_else(|| EntitlementError::Internal("override vanished".into()))
    }

    /// Deletes an override.
    pub async fn delete_override(&self, ctx: &AccessContext, id: &str) -> Result<()> {
        self.require_admin(ctx)?;
        let actor = ctx.principal_key();
        let (mut tx, now) = self.wbegin().await?;
        let res = async {
            let tx = tx.as_mut();
            let n = tx
                .execute(&Statement::new(
                    "DELETE FROM _forgebase_entitlement_overrides WHERE id = ?1",
                    vec![id.into()],
                ))
                .await?
                .rows_affected;
            if n == 0 {
                return Err(EntitlementError::NotFound("override".into()));
            }
            audit_tx(tx, now, &actor, "override_delete", None, &json!({"id": id})).await
        }
        .await;
        self.wend(tx, res).await
    }

    /// Enterprise limits, optionally of one subject.
    pub fn custom_limits(&self, ctx: &AccessContext, subject: Option<&str>) -> Result<Vec<CustomLimit>> {
        self.require_admin(ctx)?;
        let mut v: Vec<CustomLimit> = self
            .catalog()
            .custom
            .values()
            .filter(|c| subject.is_none_or(|s| c.subject == s))
            .cloned()
            .collect();
        v.sort_by(|a, b| (&a.subject, &a.feature).cmp(&(&b.subject, &b.feature)));
        Ok(v)
    }

    /// Sets an enterprise limit (replaces the plan's grant of the feature).
    pub async fn put_custom_limit(&self, ctx: &AccessContext, c: CustomLimit) -> Result<CustomLimit> {
        self.require_admin(ctx)?;
        parse_subject(&c.subject)?;
        let kind = self
            .catalog()
            .features
            .get(&c.feature)
            .map(|f| f.kind)
            .ok_or_else(|| EntitlementError::NotFound(format!("feature {}", c.feature)))?;
        let period = if c.period.is_some() || c.window_secs.is_some() {
            Some(validate_grant(kind, &c.feature, c.period, c.window_secs, &[c.limit])?)
        } else {
            validate_grant(kind, &c.feature, None, None, &[c.limit])?;
            None
        };
        let actor = ctx.principal_key();
        let (mut tx, now) = self.wbegin().await?;
        let res = async {
            let tx = tx.as_mut();
            tx.execute(&Statement::new(
                "INSERT INTO _forgebase_custom_limits (subject, feature_key, enabled, limit_value, period, window_secs, reason, created_by, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT (subject, feature_key) DO UPDATE SET enabled = excluded.enabled, limit_value = excluded.limit_value,
                   period = excluded.period, window_secs = excluded.window_secs, reason = excluded.reason,
                   created_by = excluded.created_by, updated_at = excluded.updated_at",
                vec![
                    c.subject.clone().into(),
                    c.feature.clone().into(),
                    c.enabled.into(),
                    c.limit.into(),
                    period.map(Period::as_str).into(),
                    c.window_secs.into(),
                    c.reason.clone().into(),
                    actor.clone().into(),
                    now.into(),
                ],
            ))
            .await?;
            audit_tx(tx, now, &actor, "custom_limit_put", Some(&c.subject), &serde_json::to_value(&c)?).await
        }
        .await;
        self.wend(tx, res).await?;
        self.catalog()
            .custom
            .get(&(c.subject.clone(), c.feature.clone()))
            .cloned()
            .ok_or_else(|| EntitlementError::Internal("limit vanished".into()))
    }

    /// Removes an enterprise limit.
    pub async fn delete_custom_limit(&self, ctx: &AccessContext, subject: &str, feature: &str) -> Result<()> {
        self.require_admin(ctx)?;
        let actor = ctx.principal_key();
        let (mut tx, now) = self.wbegin().await?;
        let res = async {
            let tx = tx.as_mut();
            let n = tx
                .execute(&Statement::new(
                    "DELETE FROM _forgebase_custom_limits WHERE subject = ?1 AND feature_key = ?2",
                    vec![subject.into(), feature.into()],
                ))
                .await?
                .rows_affected;
            if n == 0 {
                return Err(EntitlementError::NotFound("custom limit".into()));
            }
            audit_tx(
                tx,
                now,
                &actor,
                "custom_limit_delete",
                Some(subject),
                &json!({"feature": feature}),
            )
            .await
        }
        .await;
        self.wend(tx, res).await
    }
}

fn feature_out(f: &crate::catalog::FeatureRow) -> Feature {
    Feature {
        key: f.key.clone(),
        name: f.name.clone(),
        description: f.description.clone(),
        kind: f.kind,
        unit: f.unit.clone(),
        permissions: f.permissions.clone(),
        created_at: f.created_at,
        updated_at: f.updated_at,
    }
}

fn plan_out(p: &crate::catalog::PlanRow) -> Plan {
    Plan {
        key: p.key.clone(),
        name: p.name.clone(),
        description: p.description.clone(),
        is_default: p.is_default,
        active: p.active,
        trial_days: p.trial_days,
        metadata: p.metadata.clone(),
        features: p.features.iter().map(|f| PlanFeature { ..f.clone() }).collect(),
        created_at: p.created_at,
        updated_at: p.updated_at,
    }
}

fn grant_out(r: &[Value], cat: &crate::catalog::Catalog) -> PackGrant {
    PackGrant {
        id: text(r, 0),
        subject: text(r, 1),
        pack: opt_text(r, 2).and_then(|id| cat.pack_ids.get(&id).cloned()),
        feature: text(r, 3),
        units: int(r, 4),
        remaining: int(r, 5),
        expires_at: opt_int(r, 6),
        created_at: int(r, 7),
    }
}

async fn grant_row(tx: &mut (dyn WriteTx + 'static), id: &str, cat: &crate::catalog::Catalog) -> Result<PackGrant> {
    let rows = tx
        .fetch_all(&Statement::new(
            "SELECT id, subject, pack_id, feature_key, units, remaining, expires_at, created_at
             FROM _forgebase_pack_grants WHERE id = ?1",
            vec![id.into()],
        ))
        .await?;
    rows.rows
        .first()
        .map(|r| grant_out(r, cat))
        .ok_or_else(|| EntitlementError::Internal("grant vanished".into()))
}
