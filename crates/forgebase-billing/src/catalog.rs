//! The catalog: products, prices (multi-currency price sets, tiers), tax
//! rates and coupons. Writes are for platform administrators (an
//! organization's billing manager must not set its own prices); reads for any
//! authenticated caller.

use std::collections::BTreeMap;

use forgebase_data::AccessContext;
use forgebase_db::Value;
use serde_json::json;

use crate::error::{BillingError, Result};
use crate::model::{
    Coupon, CouponDuration, CouponInput, CouponKind, CurrencyOption, Interval, MeterBasis, Price, PriceInput,
    PriceType, Product, ProductInput, ProductKind, TaxBehavior, TaxRate, TaxRateInput,
};
use crate::money::{MAX_AMOUNT, Scheme, Tier, check_tiers, currency};
use crate::service::{
    BillingService, Tx, check_key, check_metadata, check_text, int, json_col, new_id, opt_int, opt_text, text,
};

const PRODUCT_COLUMNS: &str = "id, key, name, description, kind, plan_key, addon_key, pack_key, credits, active, metadata, created_at, updated_at";

fn product_row(r: &[Value]) -> Product {
    Product {
        id: text(r, 0),
        key: text(r, 1),
        name: text(r, 2),
        description: opt_text(r, 3),
        kind: ProductKind::parse(&text(r, 4)).unwrap_or(ProductKind::Service),
        plan_key: opt_text(r, 5),
        addon_key: opt_text(r, 6),
        pack_key: opt_text(r, 7),
        credits: opt_int(r, 8),
        active: int(r, 9) != 0,
        metadata: json_col(r, 10),
        created_at: int(r, 11),
        updated_at: int(r, 12),
    }
}

pub(crate) const PRICE_COLUMNS: &str =
    "p.id, p.key, p.product_id, pr.key, p.type, p.interval, p.interval_count, p.scheme,
    p.currency, p.unit_amount, p.tiers, p.currency_options, p.feature_key, p.meter_basis, p.trial_days, p.tax_behavior,
    p.active, p.metadata, p.created_at";

pub(crate) fn price_row(r: &[Value]) -> Price {
    Price {
        id: text(r, 0),
        key: text(r, 1),
        product_id: text(r, 2),
        product: text(r, 3),
        price_type: PriceType::parse(&text(r, 4)).unwrap_or(PriceType::OneTime),
        interval: opt_text(r, 5).and_then(|s| Interval::parse(&s)),
        interval_count: int(r, 6).max(1),
        scheme: Scheme::parse(&text(r, 7)).unwrap_or(Scheme::PerUnit),
        currency: text(r, 8),
        unit_amount: int(r, 9),
        tiers: opt_text(r, 10).and_then(|s| serde_json::from_str(&s).ok()),
        currency_options: serde_json::from_str(&text(r, 11)).unwrap_or_default(),
        feature: opt_text(r, 12),
        meter_basis: MeterBasis::parse(&text(r, 13)).unwrap_or(MeterBasis::Units),
        trial_days: int(r, 14),
        tax_behavior: TaxBehavior::parse(&text(r, 15)).unwrap_or(TaxBehavior::Exclusive),
        active: int(r, 16) != 0,
        metadata: json_col(r, 17),
        created_at: int(r, 18),
    }
}

const COUPON_COLUMNS: &str = "id, code, name, kind, percent_bp, amount_off, currency, duration, duration_cycles,
    max_redemptions, redeemed, expires_at, products, active";

pub(crate) fn coupon_row(r: &[Value]) -> Coupon {
    Coupon {
        id: text(r, 0),
        code: text(r, 1),
        name: opt_text(r, 2),
        kind: CouponKind::parse(&text(r, 3)).unwrap_or(CouponKind::Percent),
        percent_bp: opt_int(r, 4),
        amount_off: opt_int(r, 5),
        currency: opt_text(r, 6),
        duration: CouponDuration::parse(&text(r, 7)).unwrap_or(CouponDuration::Once),
        duration_cycles: opt_int(r, 8),
        max_redemptions: opt_int(r, 9),
        redeemed: int(r, 10),
        expires_at: opt_int(r, 11),
        products: serde_json::from_str(&text(r, 12)).unwrap_or_default(),
        active: int(r, 13) != 0,
    }
}

fn check_option_amounts(scheme: Scheme, unit_amount: i64, tiers: &Option<Vec<Tier>>) -> Result<()> {
    if !(0..=MAX_AMOUNT).contains(&unit_amount) {
        return Err(BillingError::invalid(
            "invalid_amount",
            "unit_amount must be between 0 and 10^13",
        ));
    }
    match (scheme, tiers) {
        (Scheme::PerUnit, None) => Ok(()),
        (Scheme::PerUnit, Some(_)) => Err(BillingError::invalid(
            "invalid_tiers",
            "per_unit prices take unit_amount, not tiers",
        )),
        (_, Some(t)) => check_tiers(t),
        (_, None) => Err(BillingError::invalid(
            "invalid_tiers",
            "graduated and volume prices need tiers",
        )),
    }
}

impl BillingService {
    // -----------------------------------------------------------------------
    // Products
    // -----------------------------------------------------------------------

    /// Products, by key.
    pub async fn products(&self, ctx: &AccessContext) -> Result<Vec<Product>> {
        self.require_caller(ctx)?;
        let rows = self
            .fetch(
                &format!("SELECT {PRODUCT_COLUMNS} FROM _forgebase_billing_products ORDER BY key"),
                Vec::new(),
            )
            .await?;
        Ok(rows.rows.iter().map(|r| product_row(r)).collect())
    }

    pub(crate) async fn product_by(&self, column: &str, v: &str) -> Result<Product> {
        let rows = self
            .fetch(
                &format!("SELECT {PRODUCT_COLUMNS} FROM _forgebase_billing_products WHERE {column} = ?1"),
                vec![v.into()],
            )
            .await?;
        rows.rows
            .first()
            .map(|r| product_row(r))
            .ok_or_else(|| BillingError::NotFound("product".into()))
    }

    /// One product.
    pub async fn product(&self, ctx: &AccessContext, key: &str) -> Result<Product> {
        self.require_caller(ctx)?;
        self.product_by("key", key).await
    }

    /// Creates or replaces a product (platform administrators). The FB-24
    /// plan, add-on or pack it delivers must exist.
    pub async fn put_product(&self, ctx: &AccessContext, key: &str, input: ProductInput) -> Result<Product> {
        self.require_admin(ctx)?;
        let key = check_key(key, "product key")?;
        let name = check_text(Some(input.name), "name", 200)?
            .ok_or_else(|| BillingError::invalid("invalid_field", "name is required"))?;
        let description = check_text(input.description, "description", 2000)?;
        let sys = AccessContext::system();
        let (mut plan, mut addon, mut pack, mut credits) = (None, None, None, None);
        let need = |v: Option<String>, what: &str| {
            v.map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| BillingError::invalid("invalid_product", format!("a {what} product needs {what}_key")))
        };
        match input.kind {
            ProductKind::Plan => {
                let k = need(input.plan_key, "plan")?;
                self.inner
                    .ent
                    .plan(&sys, &k)
                    .map_err(|_| BillingError::invalid("unknown_plan", format!("no FB-24 plan {k:?}")))?;
                plan = Some(k);
            }
            ProductKind::Addon => {
                let k = need(input.addon_key, "addon")?;
                if !self.inner.ent.addons(&sys)?.iter().any(|a| a.key == k) {
                    return Err(BillingError::invalid("unknown_addon", format!("no FB-24 add-on {k:?}")));
                }
                addon = Some(k);
            }
            ProductKind::UsagePack => {
                let k = need(input.pack_key, "pack")?;
                if !self.inner.ent.usage_packs(&sys)?.iter().any(|p| p.key == k) {
                    return Err(BillingError::invalid(
                        "unknown_pack",
                        format!("no FB-24 usage pack {k:?}"),
                    ));
                }
                pack = Some(k);
            }
            ProductKind::Credits => match input.credits {
                Some(c) if (1..=1_000_000_000).contains(&c) => credits = Some(c),
                _ => {
                    return Err(BillingError::invalid(
                        "invalid_product",
                        "a credits product needs credits (credits per unit, 1 to 10^9)",
                    ));
                }
            },
            ProductKind::Service => {}
        }
        let metadata = check_metadata(input.metadata)?;
        let now = self.now();
        let mut tx = self.begin().await?;
        let res = async {
            let mut t = Tx(tx.as_mut());
            t.exec(
                "INSERT INTO _forgebase_billing_products (id, key, name, description, kind, plan_key, addon_key, pack_key,
                     credits, active, metadata, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12)
                 ON CONFLICT (key) DO UPDATE SET name = excluded.name, description = excluded.description,
                     kind = excluded.kind, plan_key = excluded.plan_key, addon_key = excluded.addon_key,
                     pack_key = excluded.pack_key, credits = excluded.credits, active = excluded.active,
                     metadata = excluded.metadata, updated_at = excluded.updated_at",
                vec![
                    new_id("prod").into(),
                    key.clone().into(),
                    name.into(),
                    description.into(),
                    input.kind.as_str().into(),
                    plan.into(),
                    addon.into(),
                    pack.into(),
                    credits.into(),
                    input.active.into(),
                    metadata.to_string().into(),
                    now.into(),
                ],
            )
            .await?;
            t.audit(&ctx.principal_key(), "product.put", "product", &key, json!({"kind": input.kind}), now)
                .await?;
            Ok(((), true))
        }
        .await;
        self.end(tx, res).await?;
        self.product_by("key", &key).await
    }

    // -----------------------------------------------------------------------
    // Prices
    // -----------------------------------------------------------------------

    /// Prices, optionally of one product.
    pub async fn prices(&self, ctx: &AccessContext, product: Option<&str>) -> Result<Vec<Price>> {
        self.require_caller(ctx)?;
        let rows = self
            .fetch(
                &format!(
                    "SELECT {PRICE_COLUMNS} FROM _forgebase_billing_prices p
                     JOIN _forgebase_billing_products pr ON pr.id = p.product_id
                     WHERE (?1 IS NULL OR pr.key = ?1) ORDER BY p.key"
                ),
                vec![product.map(str::to_owned).into()],
            )
            .await?;
        Ok(rows.rows.iter().map(|r| price_row(r)).collect())
    }

    pub(crate) async fn price_by(&self, column: &str, v: &str) -> Result<Price> {
        let rows = self
            .fetch(
                &format!(
                    "SELECT {PRICE_COLUMNS} FROM _forgebase_billing_prices p
                     JOIN _forgebase_billing_products pr ON pr.id = p.product_id WHERE p.{column} = ?1"
                ),
                vec![v.into()],
            )
            .await?;
        rows.rows
            .first()
            .map(|r| price_row(r))
            .ok_or_else(|| BillingError::NotFound("price".into()))
    }

    /// One price.
    pub async fn price(&self, ctx: &AccessContext, key: &str) -> Result<Price> {
        self.require_caller(ctx)?;
        self.price_by("key", key).await
    }

    /// Creates or replaces a price (platform administrators). The amounts of a
    /// price that is on a subscription cannot change: create a new price and
    /// move subscriptions to it (a plan change) instead.
    pub async fn put_price(&self, ctx: &AccessContext, key: &str, input: PriceInput) -> Result<Price> {
        self.require_admin(ctx)?;
        let key = check_key(key, "price key")?;
        let product = self.product_by("key", input.product.trim()).await?;
        let scheme = input.scheme.unwrap_or(Scheme::PerUnit);
        let cur = currency(&input.currency)?;
        check_option_amounts(scheme, input.unit_amount, &input.tiers)?;
        let mut options = BTreeMap::new();
        for (c, o) in input.currency_options {
            let c = currency(&c)?;
            if c == cur {
                return Err(BillingError::invalid(
                    "invalid_currency",
                    "currency_options repeats the primary currency",
                ));
            }
            check_option_amounts(scheme, o.unit_amount, &o.tiers)?;
            options.insert(
                c,
                CurrencyOption {
                    unit_amount: o.unit_amount,
                    tiers: o.tiers,
                },
            );
        }
        let (interval, feature) = match input.price_type {
            PriceType::OneTime => {
                if input.interval.is_some() || input.feature.is_some() {
                    return Err(BillingError::invalid(
                        "invalid_price",
                        "one-time prices have no interval and no feature",
                    ));
                }
                if product.kind == ProductKind::Plan {
                    return Err(BillingError::invalid(
                        "invalid_price",
                        "plan products are sold by recurring prices",
                    ));
                }
                (None, None)
            }
            PriceType::Recurring => {
                if input.feature.is_some() {
                    return Err(BillingError::invalid(
                        "invalid_price",
                        "recurring prices have no feature",
                    ));
                }
                if matches!(product.kind, ProductKind::Credits | ProductKind::UsagePack) {
                    return Err(BillingError::invalid(
                        "invalid_price",
                        "credits and usage packs are sold by one-time prices",
                    ));
                }
                (Some(input.interval.unwrap_or(Interval::Month)), None)
            }
            PriceType::Metered => {
                let f = input
                    .feature
                    .map(|f| f.trim().to_owned())
                    .filter(|f| !f.is_empty())
                    .ok_or_else(|| {
                        BillingError::invalid("invalid_price", "metered prices need the FB-24 feature they bill")
                    })?;
                if product.kind != ProductKind::Service && product.kind != ProductKind::Plan {
                    return Err(BillingError::invalid(
                        "invalid_price",
                        "metered prices belong to plan or service products",
                    ));
                }
                (Some(input.interval.unwrap_or(Interval::Month)), Some(f))
            }
        };
        if !(1..=36).contains(&input.interval_count) {
            return Err(BillingError::invalid(
                "invalid_price",
                "interval_count must be between 1 and 36",
            ));
        }
        if !(0..=730).contains(&input.trial_days) {
            return Err(BillingError::invalid(
                "invalid_price",
                "trial_days must be between 0 and 730",
            ));
        }
        let metadata = check_metadata(input.metadata)?;
        let now = self.now();
        let mut tx = self.begin().await?;
        let res = async {
            let mut t = Tx(tx.as_mut());
            let existing = t
                .fetch(
                    "SELECT id, unit_amount, tiers, currency_options, currency, type, scheme FROM _forgebase_billing_prices WHERE key = ?1",
                    vec![key.clone().into()],
                )
                .await?;
            let tiers_json = input.tiers.as_ref().map(serde_json::to_string).transpose()?;
            let options_json = serde_json::to_string(&options)?;
            if let Some(r) = existing.rows.first() {
                let id = text(r, 0);
                let in_use = t
                    .fetch(
                        "SELECT COUNT(*) FROM _forgebase_billing_subscription_items WHERE price_id = ?1",
                        vec![id.clone().into()],
                    )
                    .await?
                    .scalar()
                    .and_then(Value::as_i64)
                    .unwrap_or_default()
                    > 0;
                let changed = int(r, 1) != input.unit_amount
                    || opt_text(r, 2) != tiers_json
                    || text(r, 3) != options_json
                    || text(r, 4) != cur
                    || text(r, 5) != input.price_type.as_str()
                    || text(r, 6) != scheme.as_str();
                if in_use && changed {
                    return Err(BillingError::conflict(
                        "price_in_use",
                        "the price is on subscriptions; create a new price instead of changing its amounts",
                    ));
                }
                t.exec(
                    "UPDATE _forgebase_billing_prices SET product_id = ?2, type = ?3, interval = ?4, interval_count = ?5,
                         scheme = ?6, currency = ?7, unit_amount = ?8, tiers = ?9, currency_options = ?10,
                         feature_key = ?11, meter_basis = ?12, trial_days = ?13, tax_behavior = ?14, active = ?15,
                         metadata = ?16, updated_at = ?17
                     WHERE id = ?1",
                    vec![
                        id.into(),
                        product.id.clone().into(),
                        input.price_type.as_str().into(),
                        interval.map(Interval::as_str).into(),
                        input.interval_count.into(),
                        scheme.as_str().into(),
                        cur.clone().into(),
                        input.unit_amount.into(),
                        tiers_json.into(),
                        options_json.into(),
                        feature.clone().into(),
                        input.meter_basis.unwrap_or(MeterBasis::Units).as_str().into(),
                        input.trial_days.into(),
                        input.tax_behavior.unwrap_or(TaxBehavior::Exclusive).as_str().into(),
                        input.active.into(),
                        metadata.to_string().into(),
                        now.into(),
                    ],
                )
                .await?;
            } else {
                t.exec(
                    "INSERT INTO _forgebase_billing_prices (id, key, product_id, type, interval, interval_count, scheme,
                         currency, unit_amount, tiers, currency_options, feature_key, meter_basis, trial_days,
                         tax_behavior, active, metadata, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?18)",
                    vec![
                        new_id("price").into(),
                        key.clone().into(),
                        product.id.clone().into(),
                        input.price_type.as_str().into(),
                        interval.map(Interval::as_str).into(),
                        input.interval_count.into(),
                        scheme.as_str().into(),
                        cur.clone().into(),
                        input.unit_amount.into(),
                        tiers_json.into(),
                        options_json.into(),
                        feature.clone().into(),
                        input.meter_basis.unwrap_or(MeterBasis::Units).as_str().into(),
                        input.trial_days.into(),
                        input.tax_behavior.unwrap_or(TaxBehavior::Exclusive).as_str().into(),
                        input.active.into(),
                        metadata.to_string().into(),
                        now.into(),
                    ],
                )
                .await?;
            }
            t.audit(&ctx.principal_key(), "price.put", "price", &key, json!({"product": product.key}), now)
                .await?;
            Ok(((), true))
        }
        .await;
        self.end(tx, res).await?;
        self.price_by("key", &key).await
    }

    // -----------------------------------------------------------------------
    // Tax rates
    // -----------------------------------------------------------------------

    /// Tax rates.
    pub async fn tax_rates(&self, ctx: &AccessContext) -> Result<Vec<TaxRate>> {
        self.require_caller(ctx)?;
        let rows = self
            .fetch(
                "SELECT id, region, name, rate_bp, active FROM _forgebase_billing_tax_rates ORDER BY region",
                Vec::new(),
            )
            .await?;
        Ok(rows
            .rows
            .iter()
            .map(|r| TaxRate {
                id: text(r, 0),
                region: text(r, 1),
                name: text(r, 2),
                rate_bp: int(r, 3),
                active: int(r, 4) != 0,
            })
            .collect())
    }

    /// Sets the tax rate of a region (platform administrators).
    pub async fn put_tax_rate(&self, ctx: &AccessContext, region: &str, input: TaxRateInput) -> Result<TaxRate> {
        self.require_admin(ctx)?;
        let region = region.trim().to_ascii_uppercase();
        if region.is_empty() || region.len() > 16 || !region.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            return Err(BillingError::invalid(
                "invalid_region",
                "region: 1 to 16 letters, digits or -",
            ));
        }
        if !(0..=10_000).contains(&input.rate_bp) {
            return Err(BillingError::invalid(
                "invalid_rate",
                "rate_bp must be between 0 and 10000",
            ));
        }
        let name = check_text(Some(input.name), "name", 64)?
            .ok_or_else(|| BillingError::invalid("invalid_field", "name is required"))?;
        let now = self.now();
        let mut tx = self.begin().await?;
        let res = async {
            let mut t = Tx(tx.as_mut());
            t.exec(
                "INSERT INTO _forgebase_billing_tax_rates (id, region, name, rate_bp, active, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
                 ON CONFLICT (region) DO UPDATE SET name = excluded.name, rate_bp = excluded.rate_bp,
                     active = excluded.active, updated_at = excluded.updated_at",
                vec![
                    new_id("txr").into(),
                    region.clone().into(),
                    name.into(),
                    input.rate_bp.into(),
                    input.active.into(),
                    now.into(),
                ],
            )
            .await?;
            t.audit(
                &ctx.principal_key(),
                "tax_rate.put",
                "tax_rate",
                &region,
                json!({"rate_bp": input.rate_bp}),
                now,
            )
            .await?;
            Ok(((), true))
        }
        .await;
        self.end(tx, res).await?;
        self.tax_rates(ctx)
            .await?
            .into_iter()
            .find(|r| r.region == region)
            .ok_or_else(|| BillingError::Internal("tax rate not stored".into()))
    }

    /// The tax rate for a region: the exact region (`US-CA`), else its
    /// country (`US`); `0` when none is active.
    pub(crate) async fn tax_rate_for(&self, region: Option<&str>) -> Result<(i64, Option<String>)> {
        let Some(region) = region else {
            return Ok((0, None));
        };
        let country = region.split('-').next().unwrap_or(region);
        let rows = self
            .fetch(
                "SELECT region, rate_bp FROM _forgebase_billing_tax_rates
                 WHERE active = 1 AND region IN (?1, ?2) ORDER BY length(region) DESC LIMIT 1",
                vec![region.into(), country.into()],
            )
            .await?;
        Ok(rows.rows.first().map_or((0, None), |r| (int(r, 1), Some(text(r, 0)))))
    }

    // -----------------------------------------------------------------------
    // Coupons
    // -----------------------------------------------------------------------

    /// Coupons (platform administrators).
    pub async fn coupons(&self, ctx: &AccessContext) -> Result<Vec<Coupon>> {
        self.require_admin(ctx)?;
        let rows = self
            .fetch(
                &format!("SELECT {COUPON_COLUMNS} FROM _forgebase_billing_coupons ORDER BY code"),
                Vec::new(),
            )
            .await?;
        Ok(rows.rows.iter().map(|r| coupon_row(r)).collect())
    }

    pub(crate) async fn coupon_by(&self, column: &str, v: &str) -> Result<Coupon> {
        let rows = self
            .fetch(
                &format!("SELECT {COUPON_COLUMNS} FROM _forgebase_billing_coupons WHERE {column} = ?1"),
                vec![v.into()],
            )
            .await?;
        rows.rows
            .first()
            .map(|r| coupon_row(r))
            .ok_or_else(|| BillingError::NotFound("coupon".into()))
    }

    /// Creates or replaces a coupon (platform administrators).
    pub async fn put_coupon(&self, ctx: &AccessContext, code: &str, input: CouponInput) -> Result<Coupon> {
        self.require_admin(ctx)?;
        let code = check_key(code, "coupon code")?.to_ascii_uppercase();
        let duration = input.duration.unwrap_or(CouponDuration::Once);
        let (bp, off, cur) = match input.kind {
            CouponKind::Percent => match input.percent_bp {
                Some(bp) if (1..=10_000).contains(&bp) && input.amount_off.is_none() => (Some(bp), None, None),
                _ => {
                    return Err(BillingError::invalid(
                        "invalid_coupon",
                        "percent coupons take percent_bp (1..=10000) only",
                    ));
                }
            },
            CouponKind::Amount => match (input.amount_off, input.currency.as_deref()) {
                (Some(a), Some(c)) if (1..=MAX_AMOUNT).contains(&a) && input.percent_bp.is_none() => {
                    (None, Some(a), Some(currency(c)?))
                }
                _ => {
                    return Err(BillingError::invalid(
                        "invalid_coupon",
                        "amount coupons take amount_off and currency only",
                    ));
                }
            },
        };
        let cycles = match (duration, input.duration_cycles) {
            (CouponDuration::Repeating, Some(n)) if (1..=120).contains(&n) => Some(n),
            (CouponDuration::Repeating, _) => {
                return Err(BillingError::invalid(
                    "invalid_coupon",
                    "repeating coupons need duration_cycles (1..=120)",
                ));
            }
            (_, None) => None,
            (_, Some(_)) => {
                return Err(BillingError::invalid(
                    "invalid_coupon",
                    "duration_cycles is for repeating coupons",
                ));
            }
        };
        if input.max_redemptions.is_some_and(|m| m < 1) {
            return Err(BillingError::invalid(
                "invalid_coupon",
                "max_redemptions must be at least 1",
            ));
        }
        for p in &input.products {
            self.product_by("key", p).await?;
        }
        let name = check_text(input.name, "name", 200)?;
        let now = self.now();
        let mut tx = self.begin().await?;
        let res = async {
            let mut t = Tx(tx.as_mut());
            t.exec(
                "INSERT INTO _forgebase_billing_coupons (id, code, name, kind, percent_bp, amount_off, currency, duration,
                     duration_cycles, max_redemptions, expires_at, products, active, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?14)
                 ON CONFLICT (code) DO UPDATE SET name = excluded.name, kind = excluded.kind,
                     percent_bp = excluded.percent_bp, amount_off = excluded.amount_off, currency = excluded.currency,
                     duration = excluded.duration, duration_cycles = excluded.duration_cycles,
                     max_redemptions = excluded.max_redemptions, expires_at = excluded.expires_at,
                     products = excluded.products, active = excluded.active, updated_at = excluded.updated_at",
                vec![
                    new_id("cpn").into(),
                    code.clone().into(),
                    name.into(),
                    input.kind.as_str().into(),
                    bp.into(),
                    off.into(),
                    cur.into(),
                    duration.as_str().into(),
                    cycles.into(),
                    input.max_redemptions.into(),
                    input.expires_at.into(),
                    serde_json::to_string(&input.products)?.into(),
                    input.active.into(),
                    now.into(),
                ],
            )
            .await?;
            t.audit(&ctx.principal_key(), "coupon.put", "coupon", &code, json!({"kind": input.kind}), now)
                .await?;
            Ok(((), true))
        }
        .await;
        self.end(tx, res).await?;
        self.coupon_by("code", &code).await
    }

    /// Redeems a coupon inside a transaction: active, not expired, under its
    /// cap, in the invoice currency (amount coupons). Returns the coupon and
    /// the invoices it applies to (`None`: forever).
    pub(crate) async fn redeem_coupon(
        &self,
        t: &mut Tx<'_>,
        code: &str,
        currency: &str,
        now: i64,
    ) -> Result<(Coupon, Option<i64>)> {
        let code = code.trim().to_ascii_uppercase();
        let rows = t
            .fetch(
                &format!("SELECT {COUPON_COLUMNS} FROM _forgebase_billing_coupons WHERE code = ?1"),
                vec![code.into()],
            )
            .await?;
        let c = rows
            .rows
            .first()
            .map(|r| coupon_row(r))
            .ok_or_else(|| BillingError::invalid("invalid_coupon", "no such coupon"))?;
        if !c.active || c.expires_at.is_some_and(|e| e <= now) {
            return Err(BillingError::invalid("coupon_expired", "the coupon is no longer valid"));
        }
        if c.kind == CouponKind::Amount && c.currency.as_deref() != Some(currency) {
            return Err(BillingError::invalid(
                "coupon_currency",
                format!(
                    "the coupon is in {}, the invoice in {currency}",
                    c.currency.clone().unwrap_or_default()
                ),
            ));
        }
        let n = t
            .exec(
                "UPDATE _forgebase_billing_coupons SET redeemed = redeemed + 1
                 WHERE id = ?1 AND (max_redemptions IS NULL OR redeemed < max_redemptions)",
                vec![c.id.clone().into()],
            )
            .await?;
        if n == 0 {
            return Err(BillingError::invalid(
                "coupon_exhausted",
                "the coupon was redeemed too often",
            ));
        }
        let cycles = match c.duration {
            CouponDuration::Once => Some(1),
            CouponDuration::Repeating => c.duration_cycles,
            CouponDuration::Forever => None,
        };
        Ok((c, cycles))
    }
}
