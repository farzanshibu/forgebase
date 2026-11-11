//! Subscriptions and orders: subscribe (trial or first invoice), plan
//! changes with proration (upgrade and downgrade mid-cycle), cancel at period
//! end or now, pause and resume, one-time orders, renewals and the
//! maintenance run (usage, trials, renewals, dunning retries, late
//! fulfillment and entitlement syncs).

use std::collections::HashSet;

use forgebase_data::AccessContext;
use serde_json::json;

use crate::billing::{InvoiceSpec, LineDraft, SubRow, item_amount, load_invoice_tx};
use crate::error::{BillingError, Result};
use crate::model::{
    CancelInput, ChangePlanInput, Checkout, Coupon, ItemInput, LineKind, MaintenanceReport, Order, OrderInput,
    OrderStatus, Price, PriceType, ProductKind, Proration, SubscribeInput, Subscription, SubscriptionStatus,
};
use crate::money::prorate;
use crate::service::{
    BillingService, DAY, Tx, add_months, check_idem, check_metadata, check_url, int, new_id, next_boundary, opt_text,
    period_months, realm_clause, text,
};

const MAX_QUANTITY: i64 = 1_000_000;

/// The validated outcome of a plan change: nothing written yet.
pub(crate) struct ChangeCalc {
    pub s: SubRow,
    pub new: Price,
    pub old: Option<(String, Price, i64)>,
    pub quantity: i64,
    pub now: i64,
    pub mode: Proration,
    pub lines: Vec<LineDraft>,
}

impl BillingService {
    /// Prices of `items`, validated for `currency` and `allowed` types.
    async fn resolve_items(
        &self,
        items: &[ItemInput],
        currency: &str,
        allowed: &[PriceType],
    ) -> Result<Vec<(Price, i64)>> {
        if items.is_empty() || items.len() > 20 {
            return Err(BillingError::invalid("invalid_items", "name 1 to 20 prices"));
        }
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for it in items {
            if !(1..=MAX_QUANTITY).contains(&it.quantity) {
                return Err(BillingError::invalid(
                    "invalid_quantity",
                    "quantity must be between 1 and 10^6",
                ));
            }
            let p = self.price_by("key", it.price.trim()).await?;
            if !seen.insert(p.id.clone()) {
                return Err(BillingError::invalid("invalid_items", "a price is listed twice"));
            }
            if !p.active {
                return Err(BillingError::invalid(
                    "price_inactive",
                    format!("price {} is not sold", p.key),
                ));
            }
            if !allowed.contains(&p.price_type) {
                return Err(BillingError::invalid(
                    "invalid_price",
                    format!("price {} ({}) cannot be used here", p.key, p.price_type.as_str()),
                ));
            }
            if p.amounts(currency).is_none() {
                return Err(BillingError::invalid(
                    "currency_not_supported",
                    format!(
                        "price {} is not sold in {currency} (sold in {})",
                        p.key,
                        p.currencies().join(", ")
                    ),
                ));
            }
            if p.price_type == PriceType::Metered && it.quantity != 1 {
                return Err(BillingError::invalid(
                    "invalid_quantity",
                    "metered prices take no quantity",
                ));
            }
            out.push((p, it.quantity));
        }
        Ok(out)
    }

    async fn kind_of(&self, p: &Price) -> Result<ProductKind> {
        Ok(self.product_by("id", &p.product_id).await?.kind)
    }

    pub(crate) async fn sub_prices(&self, sub: &Subscription) -> Result<Vec<(Price, i64)>> {
        let mut out = Vec::new();
        for it in &sub.items {
            out.push((self.price_by("key", &it.price).await?, it.quantity));
        }
        Ok(out)
    }

    fn recurring_lines(prices: &[(Price, i64)], currency: &str, period: (i64, i64)) -> Result<Vec<LineDraft>> {
        prices
            .iter()
            .filter(|(p, _)| p.price_type == PriceType::Recurring)
            .map(|(p, q)| LineDraft::for_price(LineKind::Subscription, p, currency, *q, Some(period)))
            .collect()
    }

    async fn draft_of(&self, t: &mut Tx<'_>, sub_id: &str, customer_id: &str) -> Result<Option<String>> {
        let rows = t
            .fetch(
                "SELECT id FROM _forgebase_billing_invoices WHERE subscription_id = ?1 AND status = 'draft'",
                vec![sub_id.into()],
            )
            .await?;
        let id = rows.rows.first().map(|r| text(r, 0));
        if let Some(d) = &id {
            self.retax_draft(t, d, customer_id).await?;
        }
        Ok(id)
    }

    /// Decrements the coupon of a subscription after a discounted invoice.
    async fn use_coupon_cycle(&self, t: &mut Tx<'_>, sub_id: &str, discounted: bool) -> Result<()> {
        if !discounted {
            return Ok(());
        }
        t.exec(
            "UPDATE _forgebase_billing_subscriptions SET coupon_cycles_left = coupon_cycles_left - 1
             WHERE id = ?1 AND coupon_cycles_left IS NOT NULL",
            vec![sub_id.into()],
        )
        .await?;
        t.exec(
            "UPDATE _forgebase_billing_subscriptions SET coupon_id = NULL, coupon_cycles_left = NULL
             WHERE id = ?1 AND coupon_cycles_left IS NOT NULL AND coupon_cycles_left <= 0",
            vec![sub_id.into()],
        )
        .await?;
        Ok(())
    }

    async fn coupon_of(&self, s: &SubRow) -> Result<Option<Coupon>> {
        match &s.coupon_id {
            Some(id) => Ok(Some(self.coupon_by("id", id).await?)),
            None => Ok(None),
        }
    }

    async fn checkout(
        &self,
        sub_id: &str,
        invoice_id: Option<&str>,
        payment_url: Option<String>,
        replayed: bool,
    ) -> Result<Checkout> {
        let sub = self.load_sub(sub_id).await?.sub;
        let invoice = match invoice_id.or(sub.latest_invoice_id.as_deref()) {
            Some(i) => Some(self.load_invoice(i).await?),
            None => None,
        };
        Ok(Checkout {
            subscription: Some(sub),
            order: None,
            invoice,
            payment_url,
            replayed,
        })
    }

    /// After a transaction that produced an invoice: sync, then collect an
    /// open invoice (or deliver one that is already paid).
    async fn settle(
        &self,
        sub_id: Option<&str>,
        invoice: Option<&crate::model::Invoice>,
        return_url: Option<&str>,
        automatic: bool,
    ) -> Result<Option<String>> {
        if let Some(s) = sub_id {
            self.sync_logged(s).await;
        }
        let Some(inv) = invoice else { return Ok(None) };
        match inv.status {
            crate::model::InvoiceStatus::Open => {
                let c = self.collect(&inv.id, return_url, automatic).await?;
                Ok(c.payment_url)
            }
            crate::model::InvoiceStatus::Paid => {
                self.after_paid(&inv.id).await;
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    // -----------------------------------------------------------------------
    // Subscribe
    // -----------------------------------------------------------------------

    /// Subscribes a subject to recurring (and metered) prices. With a trial
    /// the subscription is `trialing` and nothing is charged until the trial
    /// ends; otherwise the first period is invoiced and collected (saved
    /// payment method, else the provider's hosted page in `payment_url`) and
    /// the subscription is `incomplete` until it is paid. Idempotent by key.
    pub async fn subscribe(&self, ctx: &AccessContext, input: SubscribeInput) -> Result<Checkout> {
        let subject = self.subject_for(ctx, input.subject.as_deref())?;
        self.can_manage(ctx, &subject)?;
        let idem = check_idem(&input.idempotency_key)?;
        let return_url = input.return_url.as_deref().map(check_url).transpose()?;
        let metadata = check_metadata(input.metadata)?;
        let cust = self.ensure_customer(&subject).await?;
        let prior = self
            .fetch(
                "SELECT id FROM _forgebase_billing_subscriptions WHERE customer_id = ?1 AND idempotency_key = ?2",
                vec![cust.id.clone().into(), idem.clone().into()],
            )
            .await?;
        if let Some(r) = prior.rows.first() {
            return self.checkout(&text(r, 0), None, None, true).await;
        }
        self.ensure_provider(&cust)?;
        let prices = self
            .resolve_items(
                &input.items,
                &cust.currency,
                &[PriceType::Recurring, PriceType::Metered],
            )
            .await?;
        let (interval, count) = {
            let (p, _) = &prices[0];
            (p.interval.unwrap_or(crate::model::Interval::Month), p.interval_count)
        };
        if prices
            .iter()
            .any(|(p, _)| p.interval != Some(interval) || p.interval_count != count)
        {
            return Err(BillingError::invalid(
                "mixed_intervals",
                "all prices of a subscription need the same interval",
            ));
        }
        let mut plan_price = None;
        for (p, _) in &prices {
            if self.kind_of(p).await? == ProductKind::Plan {
                if plan_price.is_some() {
                    return Err(BillingError::invalid("several_plans", "a subscription holds one plan"));
                }
                plan_price = Some(p.clone());
            }
        }
        if plan_price.is_some() {
            let live = self
                .fetch(
                    "SELECT COUNT(*) FROM _forgebase_billing_subscriptions s
                     JOIN _forgebase_billing_subscription_items i ON i.subscription_id = s.id
                     JOIN _forgebase_billing_prices p ON p.id = i.price_id
                     JOIN _forgebase_billing_products pr ON pr.id = p.product_id
                     WHERE s.subject = ?1 AND s.status <> 'canceled' AND pr.kind = 'plan'",
                    vec![subject.clone().into()],
                )
                .await?;
            if live.scalar().and_then(forgebase_db::Value::as_i64).unwrap_or_default() > 0 {
                return Err(BillingError::conflict(
                    "already_subscribed",
                    "the subject already has a plan; change it instead",
                ));
            }
        }
        let trial_days = match input.trial_days {
            Some(d) if !(0..=730).contains(&d) => {
                return Err(BillingError::invalid(
                    "invalid_trial",
                    "trial_days must be between 0 and 730",
                ));
            }
            Some(d) => d,
            None => plan_price.as_ref().map(|p| p.trial_days).unwrap_or_default(),
        };
        let months = period_months(interval, count);
        let now = self.now();
        let id = new_id("sub");
        let mut tx = self.begin().await?;
        let res = async {
            let mut t = Tx(tx.as_mut());
            let (coupon_id, cycles) = match &input.coupon {
                Some(code) => {
                    let (c, cycles) = self.redeem_coupon(&mut t, code, &cust.currency, now).await?;
                    (Some(c), cycles)
                }
                None => (None, None),
            };
            let (status, anchor, start, end, trial_end) = if trial_days > 0 {
                let te = now + trial_days * DAY;
                ("trialing", te, now, te, Some(te))
            } else {
                ("incomplete", now, now, add_months(now, months), None)
            };
            t.exec(
                "INSERT INTO _forgebase_billing_subscriptions (id, customer_id, subject, status, currency, interval,
                     interval_count, cycle_anchor, current_period_start, current_period_end, trial_ends_at, coupon_id,
                     coupon_cycles_left, provider, idempotency_key, metadata, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?17)",
                vec![
                    id.clone().into(),
                    cust.id.clone().into(),
                    subject.clone().into(),
                    status.into(),
                    cust.currency.clone().into(),
                    interval.as_str().into(),
                    count.into(),
                    anchor.into(),
                    start.into(),
                    end.into(),
                    trial_end.into(),
                    coupon_id.as_ref().map(|c| c.id.clone()).into(),
                    cycles.into(),
                    cust.provider.clone().into(),
                    idem.clone().into(),
                    metadata.to_string().into(),
                    now.into(),
                ],
            )
            .await?;
            for (p, q) in &prices {
                t.exec(
                    "INSERT INTO _forgebase_billing_subscription_items (id, subscription_id, price_id, quantity, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    vec![new_id("si").into(), id.clone().into(), p.id.clone().into(), (*q).into(), now.into()],
                )
                .await?;
            }
            let mut invoice = None;
            if trial_days == 0 {
                let lines = Self::recurring_lines(&prices, &cust.currency, (start, end))?;
                let inv = self
                    .build_invoice(
                        &mut t,
                        &InvoiceSpec {
                            customer_id: cust.id.clone(),
                            subject: subject.clone(),
                            currency: cust.currency.clone(),
                            reason: "subscription_create",
                            subscription_id: Some(id.clone()),
                            period: Some((start, end)),
                            ..InvoiceSpec::default()
                        },
                        lines,
                        coupon_id.as_ref(),
                        now,
                    )
                    .await?;
                self.use_coupon_cycle(&mut t, &id, inv.discount > 0).await?;
                if inv.status == crate::model::InvoiceStatus::Paid {
                    t.exec(
                        "UPDATE _forgebase_billing_subscriptions SET status = 'active', version = version + 1 WHERE id = ?1",
                        vec![id.clone().into()],
                    )
                    .await?;
                }
                invoice = Some(inv);
            }
            t.audit(&ctx.principal_key(), "subscription.create", "subscription", &id, json!({"subject": subject, "status": status}), now)
                .await?;
            Ok((invoice, true))
        }
        .await;
        let invoice = self.end(tx, res).await?;
        let url = self
            .settle(Some(&id), invoice.as_ref(), return_url.as_deref(), false)
            .await?;
        self.checkout(&id, invoice.as_ref().map(|i| i.id.as_str()), url, false)
            .await
    }

    /// Subscriptions of a subject (newest first; instance-wide readers may
    /// omit the subject).
    pub async fn subscriptions(&self, ctx: &AccessContext, subject: Option<&str>) -> Result<Vec<Subscription>> {
        let subs = if subject.is_none()
            && let Ok(realm) = self.can_read_all(ctx)
        {
            let mut params = Vec::new();
            let only = realm_clause(realm.as_deref(), "s.subject", &mut params);
            self.subs_where(&format!("1 = 1{only} ORDER BY s.created_at DESC LIMIT 500"), params)
                .await?
        } else {
            let s = self.subject_for(ctx, subject)?;
            self.can_read(ctx, &s)?;
            self.subs_where("s.subject = ?1 ORDER BY s.created_at DESC", vec![s.into()])
                .await?
        };
        Ok(subs.into_iter().map(|s| s.sub).collect())
    }

    /// One subscription.
    pub async fn subscription(&self, ctx: &AccessContext, id: &str) -> Result<Subscription> {
        let s = self.load_sub(id).await?;
        self.can_read(ctx, &s.sub.subject)
            .map_err(|_| BillingError::NotFound("subscription".into()))?;
        Ok(s.sub)
    }

    pub(crate) async fn managed_sub(&self, ctx: &AccessContext, id: &str) -> Result<SubRow> {
        let s = self.load_sub(id).await?;
        self.can_read(ctx, &s.sub.subject)
            .map_err(|_| BillingError::NotFound("subscription".into()))?;
        self.can_manage(ctx, &s.sub.subject)?;
        if self.store_managed(id).await? {
            return Err(BillingError::conflict(
                "store_managed",
                "the store owns this subscription; the customer changes it in the store",
            ));
        }
        Ok(s)
    }

    // -----------------------------------------------------------------------
    // Plan changes
    // -----------------------------------------------------------------------

    /// Validates a plan change and computes its proration lines (no writes);
    /// shared by [`Self::change_plan`] and [`Self::preview_plan_change`].
    pub(crate) async fn plan_change_calc(&self, s: SubRow, input: &ChangePlanInput) -> Result<ChangeCalc> {
        if !matches!(
            s.sub.status,
            SubscriptionStatus::Active | SubscriptionStatus::Trialing | SubscriptionStatus::PastDue
        ) {
            return Err(BillingError::conflict(
                "subscription_not_active",
                "only active, trialing or past-due subscriptions change plans",
            ));
        }
        let new = self.price_by("key", input.price.trim()).await?;
        if !new.active || new.price_type != PriceType::Recurring || self.kind_of(&new).await? != ProductKind::Plan {
            return Err(BillingError::invalid(
                "invalid_price",
                "plan changes need an active recurring plan price",
            ));
        }
        if new.interval != Some(s.sub.interval) || new.interval_count != s.sub.interval_count {
            return Err(BillingError::invalid(
                "mixed_intervals",
                "the new price needs the subscription's interval",
            ));
        }
        if new.amounts(&s.sub.currency).is_none() {
            return Err(BillingError::invalid(
                "currency_not_supported",
                format!("price {} is not sold in {}", new.key, s.sub.currency),
            ));
        }
        let prices = self.sub_prices(&s.sub).await?;
        let mut old = None;
        for (i, (p, q)) in prices.iter().enumerate() {
            if self.kind_of(p).await? == ProductKind::Plan {
                old = Some((s.sub.items[i].id.clone(), p.clone(), *q));
            }
        }
        let quantity = input.quantity.unwrap_or_else(|| old.as_ref().map_or(1, |o| o.2));
        if !(1..=MAX_QUANTITY).contains(&quantity) {
            return Err(BillingError::invalid(
                "invalid_quantity",
                "quantity must be between 1 and 10^6",
            ));
        }
        if old.as_ref().is_some_and(|(_, p, q)| p.id == new.id && *q == quantity) {
            return Err(BillingError::conflict(
                "no_change",
                "the subscription already has this price",
            ));
        }
        let now = self.now();
        if input.at_period_end {
            if input.quantity.is_some_and(|q| Some(q) != old.as_ref().map(|o| o.2)) {
                return Err(BillingError::invalid(
                    "invalid_quantity",
                    "a change at period end keeps the quantity",
                ));
            }
            return Ok(ChangeCalc {
                s,
                new,
                old,
                quantity,
                now,
                mode: Proration::None,
                lines: Vec::new(),
            });
        }
        let mode = if s.sub.status == SubscriptionStatus::Trialing {
            Proration::None
        } else {
            input.proration.unwrap_or(Proration::AlwaysInvoice)
        };
        let (start, end) = (s.sub.current_period_start, s.sub.current_period_end);
        let mut lines = Vec::new();
        if mode != Proration::None {
            if let Some((_, op, oq)) = &old {
                let full = item_amount(op, &s.sub.currency, *oq)?;
                let credit = prorate(full, start, end, now);
                if credit > 0 {
                    let mut l = LineDraft::for_price(LineKind::Proration, op, &s.sub.currency, *oq, Some((now, end)))?;
                    l.amount = -credit;
                    l.unit_amount = None;
                    l.discountable = false;
                    l.description = format!("Unused time on {} × {}", oq, op.product);
                    lines.push(l);
                }
            }
            let full = item_amount(&new, &s.sub.currency, quantity)?;
            let charge = prorate(full, start, end, now);
            if charge > 0 {
                let mut l =
                    LineDraft::for_price(LineKind::Proration, &new, &s.sub.currency, quantity, Some((now, end)))?;
                l.amount = charge;
                l.unit_amount = None;
                l.discountable = false;
                l.description = format!("Remaining time on {} × {}", quantity, new.product);
                lines.push(l);
            }
        }
        Ok(ChangeCalc {
            s,
            new,
            old,
            quantity,
            now,
            mode,
            lines,
        })
    }

    /// Moves a subscription to another plan price (upgrade or downgrade).
    /// Mid-cycle the unused time of the old price is credited and the rest of
    /// the period of the new one charged: `always_invoice` (default) invoices
    /// both lines now (a net charge is collected, a net credit goes to the
    /// customer balance); `create_prorations` adds them to the next renewal;
    /// `none` switches without proration. `at_period_end` schedules the
    /// switch for the renewal instead. Trials switch without proration.
    pub async fn change_plan(&self, ctx: &AccessContext, id: &str, input: ChangePlanInput) -> Result<Checkout> {
        let s = self.managed_sub(ctx, id).await?;
        let ChangeCalc {
            s,
            new,
            old,
            quantity,
            now,
            mode,
            lines,
        } = self.plan_change_calc(s, &input).await?;
        let actor = ctx.principal_key();
        let end = s.sub.current_period_end;
        if input.at_period_end {
            let mut tx = self.begin().await?;
            let res = async {
                let mut t = Tx(tx.as_mut());
                t.exec(
                    "UPDATE _forgebase_billing_subscriptions SET pending_price_id = ?2, version = version + 1,
                         updated_at = CASE WHEN CAST(?3 AS BIGINT) > updated_at + 1 THEN CAST(?3 AS BIGINT) ELSE updated_at + 1 END WHERE id = ?1",
                    vec![id.into(), new.id.clone().into(), now.into()],
                )
                .await?;
                t.audit(
                    &actor,
                    "subscription.schedule_change",
                    "subscription",
                    id,
                    json!({"price": new.key}),
                    now,
                )
                .await?;
                Ok(((), true))
            }
            .await;
            self.end(tx, res).await?;
            self.sync_logged(id).await;
            return self.checkout(id, None, None, false).await;
        }
        let mut tx = self.begin().await?;
        let res = async {
            let mut t = Tx(tx.as_mut());
            match &old {
                Some((item, _, _)) => {
                    t.exec(
                        "UPDATE _forgebase_billing_subscription_items SET price_id = ?2, quantity = ?3 WHERE id = ?1",
                        vec![item.clone().into(), new.id.clone().into(), quantity.into()],
                    )
                    .await?;
                }
                None => {
                    t.exec(
                        "INSERT INTO _forgebase_billing_subscription_items (id, subscription_id, price_id, quantity, created_at)
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                        vec![new_id("si").into(), id.into(), new.id.clone().into(), quantity.into(), now.into()],
                    )
                    .await?;
                }
            }
            t.exec(
                "UPDATE _forgebase_billing_subscriptions SET pending_price_id = NULL, version = version + 1,
                     updated_at = CASE WHEN CAST(?2 AS BIGINT) > updated_at + 1 THEN CAST(?2 AS BIGINT) ELSE updated_at + 1 END WHERE id = ?1",
                vec![id.into(), now.into()],
            )
            .await?;
            let mut invoice = None;
            match mode {
                Proration::AlwaysInvoice if !lines.is_empty() => {
                    let inv = self
                        .build_invoice(
                            &mut t,
                            &InvoiceSpec {
                                customer_id: s.sub.customer_id.clone(),
                                subject: s.sub.subject.clone(),
                                currency: s.sub.currency.clone(),
                                reason: "subscription_update",
                                subscription_id: Some(id.to_owned()),
                                period: Some((now, end)),
                                ..InvoiceSpec::default()
                            },
                            lines.clone(),
                            None,
                            now,
                        )
                        .await?;
                    invoice = Some(inv);
                }
                Proration::CreateProrations if !lines.is_empty() => {
                    self.add_pending_lines(&mut t, &s.sub, &lines, now).await?;
                }
                _ => {}
            }
            t.audit(
                &actor,
                "subscription.change",
                "subscription",
                id,
                json!({"from": old.as_ref().map(|o| o.1.key.clone()), "to": new.key, "proration": mode}),
                now,
            )
            .await?;
            Ok((invoice, true))
        }
        .await;
        let invoice = self.end(tx, res).await?;
        let url = self.settle(Some(id), invoice.as_ref(), None, false).await?;
        self.checkout(id, invoice.as_ref().map(|i| i.id.as_str()), url, false)
            .await
    }

    // -----------------------------------------------------------------------
    // Cancel, pause, resume
    // -----------------------------------------------------------------------

    /// Cancels at the period end (default: entitled until then) or now (the
    /// usage of the partial period and pending prorations are invoiced).
    pub async fn cancel(&self, ctx: &AccessContext, id: &str, input: CancelInput) -> Result<Checkout> {
        let s = self.managed_sub(ctx, id).await?;
        if s.sub.status == SubscriptionStatus::Canceled {
            return Err(BillingError::conflict(
                "subscription_canceled",
                "the subscription has ended",
            ));
        }
        let reason = crate::service::check_text(input.reason, "reason", 200)?.unwrap_or_else(|| "requested".into());
        let now = self.now();
        let actor = ctx.principal_key();
        let prices = self.sub_prices(&s.sub).await?;
        let mut tx = self.begin().await?;
        let res = async {
            let mut t = Tx(tx.as_mut());
            let mut invoice = None;
            if input.at_period_end && s.sub.status != SubscriptionStatus::Incomplete && s.sub.status != SubscriptionStatus::Paused {
                t.exec(
                    "UPDATE _forgebase_billing_subscriptions SET cancel_at_period_end = 1, canceled_at = ?2, cancel_reason = ?3,
                         version = version + 1, updated_at = CASE WHEN CAST(?2 AS BIGINT) > updated_at + 1 THEN CAST(?2 AS BIGINT) ELSE updated_at + 1 END WHERE id = ?1",
                    vec![id.into(), now.into(), reason.clone().into()],
                )
                .await?;
            } else {
                let usage = if matches!(s.sub.status, SubscriptionStatus::Active | SubscriptionStatus::PastDue) {
                    self.usage_lines(&mut t, &s, &prices, (s.sub.current_period_start, now)).await?
                } else {
                    Vec::new()
                };
                let draft = self.draft_of(&mut t, id, &s.sub.customer_id).await?;
                if !usage.is_empty() || draft.is_some() {
                    let inv = self
                        .build_invoice(
                            &mut t,
                            &InvoiceSpec {
                                customer_id: s.sub.customer_id.clone(),
                                subject: s.sub.subject.clone(),
                                currency: s.sub.currency.clone(),
                                reason: "subscription_final",
                                subscription_id: Some(id.to_owned()),
                                period: Some((s.sub.current_period_start, now)),
                                draft_id: draft,
                                ..InvoiceSpec::default()
                            },
                            usage.iter().map(|(l, _)| l.clone()).collect(),
                            None,
                            now,
                        )
                        .await?;
                    let ids: Vec<String> = usage.iter().map(|(_, p)| p.clone()).collect();
                    self.mark_usage_billed(&mut t, id, &ids, now, &inv.id).await?;
                    invoice = Some(inv);
                }
                if s.sub.status == SubscriptionStatus::Incomplete {
                    t.exec(
                        "UPDATE _forgebase_billing_invoices SET status = 'void', voided_at = ?2, updated_at = ?2
                         WHERE subscription_id = ?1 AND status = 'open'",
                        vec![id.into(), now.into()],
                    )
                    .await?;
                }
                t.exec(
                    "UPDATE _forgebase_billing_subscriptions SET status = 'canceled', canceled_at = ?2, ended_at = ?2,
                         cancel_reason = ?3, cancel_at_period_end = 0, version = version + 1,
                         updated_at = CASE WHEN CAST(?2 AS BIGINT) > updated_at + 1 THEN CAST(?2 AS BIGINT) ELSE updated_at + 1 END WHERE id = ?1",
                    vec![id.into(), now.into(), reason.clone().into()],
                )
                .await?;
            }
            t.audit(&actor, "subscription.cancel", "subscription", id, json!({"at_period_end": input.at_period_end, "reason": reason}), now)
                .await?;
            Ok((invoice, true))
        }
        .await;
        let invoice = self.end(tx, res).await?;
        let url = self.settle(Some(id), invoice.as_ref(), None, false).await?;
        self.checkout(id, invoice.as_ref().map(|i| i.id.as_str()), url, false)
            .await
    }

    /// Pauses an active subscription: no invoices, not entitled.
    pub async fn pause(&self, ctx: &AccessContext, id: &str) -> Result<Subscription> {
        let s = self.managed_sub(ctx, id).await?;
        if s.sub.status != SubscriptionStatus::Active {
            return Err(BillingError::conflict(
                "subscription_not_active",
                "only active subscriptions pause",
            ));
        }
        let now = self.now();
        let actor = ctx.principal_key();
        let mut tx = self.begin().await?;
        let res = async {
            let mut t = Tx(tx.as_mut());
            t.exec(
                "UPDATE _forgebase_billing_subscriptions SET status = 'paused', paused_at = ?2, version = version + 1,
                     updated_at = CASE WHEN CAST(?2 AS BIGINT) > updated_at + 1 THEN CAST(?2 AS BIGINT) ELSE updated_at + 1 END WHERE id = ?1 AND status = 'active'",
                vec![id.into(), now.into()],
            )
            .await?;
            t.audit(&actor, "subscription.pause", "subscription", id, json!({}), now)
                .await?;
            Ok(((), true))
        }
        .await;
        self.end(tx, res).await?;
        self.sync_logged(id).await;
        Ok(self.load_sub(id).await?.sub)
    }

    /// Resumes a paused subscription (a new period starts now and is
    /// invoiced) or withdraws a cancellation at period end.
    pub async fn resume(&self, ctx: &AccessContext, id: &str) -> Result<Checkout> {
        let s = self.managed_sub(ctx, id).await?;
        let now = self.now();
        let actor = ctx.principal_key();
        let prices = self.sub_prices(&s.sub).await?;
        let coupon = self.coupon_of(&s).await?;
        let mut tx = self.begin().await?;
        let res = async {
            let mut t = Tx(tx.as_mut());
            let mut invoice = None;
            if s.sub.status == SubscriptionStatus::Paused {
                let end = add_months(now, period_months(s.sub.interval, s.sub.interval_count));
                t.exec(
                    "UPDATE _forgebase_billing_subscriptions SET status = 'active', paused_at = NULL, cycle_anchor = ?2,
                         current_period_start = ?2, current_period_end = ?3, version = version + 1,
                         updated_at = CASE WHEN CAST(?2 AS BIGINT) > updated_at + 1 THEN CAST(?2 AS BIGINT) ELSE updated_at + 1 END WHERE id = ?1",
                    vec![id.into(), now.into(), end.into()],
                )
                .await?;
                let draft = self.draft_of(&mut t, id, &s.sub.customer_id).await?;
                let inv = self
                    .build_invoice(
                        &mut t,
                        &InvoiceSpec {
                            customer_id: s.sub.customer_id.clone(),
                            subject: s.sub.subject.clone(),
                            currency: s.sub.currency.clone(),
                            reason: "subscription_cycle",
                            subscription_id: Some(id.to_owned()),
                            period: Some((now, end)),
                            draft_id: draft,
                            ..InvoiceSpec::default()
                        },
                        Self::recurring_lines(&prices, &s.sub.currency, (now, end))?,
                        coupon.as_ref(),
                        now,
                    )
                    .await?;
                self.use_coupon_cycle(&mut t, id, inv.discount > 0).await?;
                invoice = Some(inv);
            } else if s.sub.cancel_at_period_end && s.sub.status != SubscriptionStatus::Canceled {
                t.exec(
                    "UPDATE _forgebase_billing_subscriptions SET cancel_at_period_end = 0, canceled_at = NULL,
                         cancel_reason = NULL, version = version + 1, updated_at = CASE WHEN CAST(?2 AS BIGINT) > updated_at + 1 THEN CAST(?2 AS BIGINT) ELSE updated_at + 1 END WHERE id = ?1",
                    vec![id.into(), now.into()],
                )
                .await?;
            } else {
                return Err(BillingError::conflict(
                    "nothing_to_resume",
                    "the subscription is neither paused nor scheduled to cancel",
                ));
            }
            t.audit(&actor, "subscription.resume", "subscription", id, json!({}), now).await?;
            Ok((invoice, true))
        }
        .await;
        let invoice = self.end(tx, res).await?;
        let url = self.settle(Some(id), invoice.as_ref(), None, true).await?;
        self.checkout(id, invoice.as_ref().map(|i| i.id.as_str()), url, false)
            .await
    }

    // -----------------------------------------------------------------------
    // Orders
    // -----------------------------------------------------------------------

    pub(crate) fn order_row(r: &[forgebase_db::Value]) -> Order {
        Order {
            id: text(r, 0),
            customer_id: text(r, 1),
            subject: text(r, 2),
            status: OrderStatus::parse(&text(r, 3)).unwrap_or(OrderStatus::Canceled),
            currency: text(r, 4),
            invoice_id: opt_text(r, 5),
            created_at: int(r, 6),
        }
    }

    async fn order_by_id(&self, id: &str) -> Result<Order> {
        let rows = self
            .fetch(
                "SELECT id, customer_id, subject, status, currency, invoice_id, created_at FROM _forgebase_billing_orders WHERE id = ?1",
                vec![id.into()],
            )
            .await?;
        rows.rows
            .first()
            .map(|r| Self::order_row(r))
            .ok_or_else(|| BillingError::NotFound("order".into()))
    }

    async fn order_checkout(&self, id: &str, payment_url: Option<String>, replayed: bool) -> Result<Checkout> {
        let order = self.order_by_id(id).await?;
        let invoice = match &order.invoice_id {
            Some(i) => Some(self.load_invoice(i).await?),
            None => None,
        };
        Ok(Checkout {
            subscription: None,
            order: Some(order),
            invoice,
            payment_url,
            replayed,
        })
    }

    /// A one-time purchase (credit top-ups, usage packs, services): invoiced
    /// and collected at once; paid lines are delivered by [`Self::fulfill`].
    /// Idempotent by key.
    pub async fn create_order(&self, ctx: &AccessContext, input: OrderInput) -> Result<Checkout> {
        let subject = self.subject_for(ctx, input.subject.as_deref())?;
        self.can_manage(ctx, &subject)?;
        let idem = check_idem(&input.idempotency_key)?;
        let return_url = input.return_url.as_deref().map(check_url).transpose()?;
        let cust = self.ensure_customer(&subject).await?;
        let prior = self
            .fetch(
                "SELECT id FROM _forgebase_billing_orders WHERE customer_id = ?1 AND idempotency_key = ?2",
                vec![cust.id.clone().into(), idem.clone().into()],
            )
            .await?;
        if let Some(r) = prior.rows.first() {
            return self.order_checkout(&text(r, 0), None, true).await;
        }
        self.ensure_provider(&cust)?;
        let prices = self
            .resolve_items(&input.items, &cust.currency, &[PriceType::OneTime])
            .await?;
        let lines = prices
            .iter()
            .map(|(p, q)| LineDraft::for_price(LineKind::OneTime, p, &cust.currency, *q, None))
            .collect::<Result<Vec<_>>>()?;
        let now = self.now();
        let id = new_id("ord");
        let mut tx = self.begin().await?;
        let res = async {
            let mut t = Tx(tx.as_mut());
            t.exec(
                "INSERT INTO _forgebase_billing_orders (id, customer_id, subject, status, currency, idempotency_key,
                     created_at, updated_at)
                 VALUES (?1, ?2, ?3, 'open', ?4, ?5, ?6, ?6)",
                vec![
                    id.clone().into(),
                    cust.id.clone().into(),
                    subject.clone().into(),
                    cust.currency.clone().into(),
                    idem.clone().into(),
                    now.into(),
                ],
            )
            .await?;
            let coupon = match &input.coupon {
                Some(code) => Some(self.redeem_coupon(&mut t, code, &cust.currency, now).await?.0),
                None => None,
            };
            let inv = self
                .build_invoice(
                    &mut t,
                    &InvoiceSpec {
                        customer_id: cust.id.clone(),
                        subject: subject.clone(),
                        currency: cust.currency.clone(),
                        reason: "order",
                        order_id: Some(id.clone()),
                        ..InvoiceSpec::default()
                    },
                    lines,
                    coupon.as_ref(),
                    now,
                )
                .await?;
            t.exec(
                "UPDATE _forgebase_billing_orders SET invoice_id = ?2, status = ?3 WHERE id = ?1",
                vec![
                    id.clone().into(),
                    inv.id.clone().into(),
                    if inv.status == crate::model::InvoiceStatus::Paid {
                        "paid"
                    } else {
                        "open"
                    }
                    .into(),
                ],
            )
            .await?;
            t.audit(
                &ctx.principal_key(),
                "order.create",
                "order",
                &id,
                json!({"subject": subject, "total": inv.total}),
                now,
            )
            .await?;
            Ok((inv, true))
        }
        .await;
        let inv = self.end(tx, res).await?;
        let url = self.settle(None, Some(&inv), return_url.as_deref(), false).await?;
        self.order_checkout(&id, url, false).await
    }

    /// Orders of a subject (newest first).
    pub async fn orders(&self, ctx: &AccessContext, subject: Option<&str>) -> Result<Vec<Order>> {
        let s = self.subject_for(ctx, subject)?;
        self.can_read(ctx, &s)?;
        let rows = self
            .fetch(
                "SELECT id, customer_id, subject, status, currency, invoice_id, created_at FROM _forgebase_billing_orders
                 WHERE subject = ?1 ORDER BY created_at DESC LIMIT 200",
                vec![s.into()],
            )
            .await?;
        Ok(rows.rows.iter().map(|r| Self::order_row(r)).collect())
    }

    /// One order.
    pub async fn order(&self, ctx: &AccessContext, id: &str) -> Result<Order> {
        let o = self.order_by_id(id).await?;
        self.can_read(ctx, &o.subject)
            .map_err(|_| BillingError::NotFound("order".into()))?;
        Ok(o)
    }

    // -----------------------------------------------------------------------
    // Renewals and maintenance
    // -----------------------------------------------------------------------

    /// Ends the period of a due subscription: converts or ends a trial,
    /// cancels at period end, or renews (the next period in advance, the
    /// usage of the past one in arrears, pending prorations, coupon, balance)
    /// and collects the invoice. Returns what happened.
    async fn renew(&self, s: SubRow, rep: &mut MaintenanceReport) -> Result<()> {
        let now = self.now();
        let months = period_months(s.sub.interval, s.sub.interval_count);
        let mut prices = self.sub_prices(&s.sub).await?;
        let coupon = self.coupon_of(&s).await?;
        let pending = match &s.pending_price_id {
            Some(p) => Some(self.price_by("id", p).await?),
            None => None,
        };
        let mut plan_item = None;
        for (i, (p, _)) in prices.iter().enumerate() {
            if self.kind_of(p).await? == ProductKind::Plan {
                plan_item = Some(i);
            }
        }
        let (start, end) = (s.sub.current_period_start, s.sub.current_period_end);
        let id = s.sub.id.clone();
        let mut tx = self.begin().await?;
        let res = async {
            let mut t = Tx(tx.as_mut());
            // Another run renewed it meanwhile.
            let cur = t
                .fetch(
                    "SELECT status, current_period_end FROM _forgebase_billing_subscriptions WHERE id = ?1",
                    vec![id.clone().into()],
                )
                .await?;
            let Some(r) = cur.rows.first() else { return Ok((None, false)) };
            if text(r, 0) != s.sub.status.as_str() || int(r, 1) != end {
                return Ok((None, false));
            }
            let mut invoice = None;
            let trial = s.sub.status == SubscriptionStatus::Trialing;
            if s.sub.cancel_at_period_end {
                let usage = if trial {
                    Vec::new()
                } else {
                    self.usage_lines(&mut t, &s, &prices, (start, end)).await?
                };
                let draft = self.draft_of(&mut t, &id, &s.sub.customer_id).await?;
                if !usage.is_empty() || draft.is_some() {
                    let inv = self
                        .build_invoice(
                            &mut t,
                            &InvoiceSpec {
                                customer_id: s.sub.customer_id.clone(),
                                subject: s.sub.subject.clone(),
                                currency: s.sub.currency.clone(),
                                reason: "subscription_final",
                                subscription_id: Some(id.clone()),
                                period: Some((start, end)),
                                draft_id: draft,
                                ..InvoiceSpec::default()
                            },
                            usage.iter().map(|(l, _)| l.clone()).collect(),
                            None,
                            now,
                        )
                        .await?;
                    let ids: Vec<String> = usage.iter().map(|(_, p)| p.clone()).collect();
                    self.mark_usage_billed(&mut t, &id, &ids, end, &inv.id).await?;
                    invoice = Some(inv);
                }
                t.exec(
                    "UPDATE _forgebase_billing_subscriptions SET status = 'canceled', ended_at = ?2,
                         cancel_reason = COALESCE(cancel_reason, 'requested'), version = version + 1,
                         updated_at = CASE WHEN CAST(?3 AS BIGINT) > updated_at + 1 THEN CAST(?3 AS BIGINT) ELSE updated_at + 1 END WHERE id = ?1",
                    vec![id.clone().into(), end.into(), now.into()],
                )
                .await?;
                t.audit("system", "subscription.ended", "subscription", &id, json!({"at": end}), now).await?;
                rep.ended += 1;
                return Ok((invoice, true));
            }
            if let (Some(p), Some(i)) = (&pending, plan_item) {
                t.exec(
                    "UPDATE _forgebase_billing_subscription_items SET price_id = ?2 WHERE id = ?1",
                    vec![s.sub.items[i].id.clone().into(), p.id.clone().into()],
                )
                .await?;
                prices[i].0 = p.clone();
            }
            let new_end = if trial {
                add_months(s.sub.cycle_anchor, months)
            } else {
                next_boundary(s.sub.cycle_anchor, months, end)
            };
            let mut lines = Self::recurring_lines(&prices, &s.sub.currency, (end, new_end))?;
            let usage = if trial {
                Vec::new()
            } else {
                self.usage_lines(&mut t, &s, &prices, (start, end)).await?
            };
            lines.extend(usage.iter().map(|(l, _)| l.clone()));
            let draft = self.draft_of(&mut t, &id, &s.sub.customer_id).await?;
            let inv = self
                .build_invoice(
                    &mut t,
                    &InvoiceSpec {
                        customer_id: s.sub.customer_id.clone(),
                        subject: s.sub.subject.clone(),
                        currency: s.sub.currency.clone(),
                        reason: "subscription_cycle",
                        subscription_id: Some(id.clone()),
                        period: Some((end, new_end)),
                        draft_id: draft,
                        ..InvoiceSpec::default()
                    },
                    lines,
                    coupon.as_ref(),
                    now,
                )
                .await?;
            let ids: Vec<String> = usage.iter().map(|(_, p)| p.clone()).collect();
            self.mark_usage_billed(&mut t, &id, &ids, end, &inv.id).await?;
            self.use_coupon_cycle(&mut t, &id, inv.discount > 0).await?;
            t.exec(
                "UPDATE _forgebase_billing_subscriptions SET status = CASE WHEN status = 'trialing' THEN 'active' ELSE status END,
                     current_period_start = ?2, current_period_end = ?3, pending_price_id = NULL,
                     version = version + 1, updated_at = CASE WHEN CAST(?4 AS BIGINT) > updated_at + 1 THEN CAST(?4 AS BIGINT) ELSE updated_at + 1 END WHERE id = ?1",
                vec![id.clone().into(), end.into(), new_end.into(), now.into()],
            )
            .await?;
            t.audit("system", "subscription.renewed", "subscription", &id, json!({"period_end": new_end, "invoice": inv.id}), now)
                .await?;
            if trial {
                rep.trials_ended += 1;
            } else {
                rep.renewed += 1;
            }
            Ok((Some(inv), true))
        }
        .await;
        let invoice = self.end(tx, res).await?;
        self.settle(Some(&id), invoice.as_ref(), None, true).await?;
        Ok(())
    }

    /// One maintenance run (the `billing.maintenance` job, every minute):
    /// usage ingestion, expiry of unpaid `incomplete` subscriptions,
    /// renewals and trial ends, dunning retries, late fulfillment and
    /// entitlement syncs. Safe to repeat.
    pub async fn run_maintenance(&self) -> Result<MaintenanceReport> {
        let _guard = self.inner.runner.lock().await;
        let mut rep = MaintenanceReport::default();
        let (read, stored) = self.ingest_usage().await?;
        rep.usage_events_read = read;
        rep.usage_recorded = stored;
        let settings = self.settings().await?;

        // Unpaid first invoices.
        let now = self.now();
        let stale = self
            .subs_where(
                "s.status = 'incomplete' AND s.created_at <= ?1 LIMIT 200",
                vec![(now - settings.incomplete_days * DAY).into()],
            )
            .await?;
        for s in stale {
            let id = s.sub.id.clone();
            let mut tx = self.begin().await?;
            let res = async {
                let mut t = Tx(tx.as_mut());
                t.exec(
                    "UPDATE _forgebase_billing_invoices SET status = 'void', voided_at = ?2, updated_at = ?2
                     WHERE subscription_id = ?1 AND status = 'open'",
                    vec![id.clone().into(), now.into()],
                )
                .await?;
                let n = t
                    .exec(
                        "UPDATE _forgebase_billing_subscriptions SET status = 'canceled', ended_at = ?2, canceled_at = ?2,
                             cancel_reason = 'incomplete_expired', version = version + 1, updated_at = CASE WHEN CAST(?2 AS BIGINT) > updated_at + 1 THEN CAST(?2 AS BIGINT) ELSE updated_at + 1 END
                         WHERE id = ?1 AND status = 'incomplete'",
                        vec![id.clone().into(), now.into()],
                    )
                    .await?;
                Ok((n, true))
            }
            .await;
            if self.end(tx, res).await? > 0 {
                rep.ended += 1;
                self.sync_logged(&id).await;
            }
        }

        // Renewals; a subscription more than one period behind renews once
        // per round.
        for _ in 0..12 {
            let now = self.now();
            let due = self
                .subs_where(
                    "s.status IN ('active', 'past_due', 'trialing') AND s.current_period_end <= ?1
                     AND NOT EXISTS (SELECT 1 FROM _forgebase_billing_store_purchases sp WHERE sp.subscription_id = s.id)
                     ORDER BY s.current_period_end LIMIT 100",
                    vec![now.into()],
                )
                .await?;
            if due.is_empty() {
                break;
            }
            for s in due {
                let id = s.sub.id.clone();
                if let Err(e) = self.renew(s, &mut rep).await {
                    tracing::warn!(subscription = %id, error = %e, "renewal failed");
                }
            }
        }

        // Dunning retries.
        let now = self.now();
        let retries = self
            .fetch(
                "SELECT id FROM _forgebase_billing_invoices WHERE status = 'open' AND next_attempt_at IS NOT NULL
                 AND next_attempt_at <= ?1 ORDER BY next_attempt_at LIMIT 100",
                vec![now.into()],
            )
            .await?;
        for r in &retries.rows {
            let id = text(r, 0);
            rep.retries += 1;
            if let Err(e) = self.collect(&id, None, true).await {
                tracing::warn!(invoice = %id, error = %e, "dunning retry failed");
                // Count it as a failed attempt so dunning progresses.
                let mut tx = self.begin().await?;
                let res = async {
                    let mut t = Tx(tx.as_mut());
                    t.exec(
                        "UPDATE _forgebase_billing_invoices SET attempt_count = attempt_count + 1 WHERE id = ?1",
                        vec![id.clone().into()],
                    )
                    .await?;
                    let inv = load_invoice_tx(&mut t, &id).await?;
                    self.on_failure(&mut t, &inv, now).await?;
                    Ok((inv.subscription_id, true))
                }
                .await;
                if let Some(s) = self.end(tx, res).await? {
                    self.sync_logged(&s).await;
                }
            }
        }

        // Store subscriptions past their period end: read back from the store
        // (a missed renewal or expiry notification).
        self.refresh_stores().await;

        // Late fulfillment.
        let unfulfilled = self
            .fetch(
                "SELECT id FROM _forgebase_billing_invoices WHERE status = 'paid' AND fulfilled_at IS NULL LIMIT 100",
                Vec::new(),
            )
            .await?;
        for r in &unfulfilled.rows {
            match self.fulfill(&text(r, 0)).await {
                Ok(true) => rep.fulfilled += 1,
                Ok(false) => {}
                Err(e) => tracing::warn!(invoice = %text(r, 0), error = %e, "fulfillment failed"),
            }
        }

        // Entitlement syncs not applied yet.
        let unsynced = self
            .fetch(
                "SELECT id FROM _forgebase_billing_subscriptions WHERE synced_version < version LIMIT 200",
                Vec::new(),
            )
            .await?;
        for r in &unsynced.rows {
            match self.sync(&text(r, 0)).await {
                Ok(true) => rep.synced += 1,
                Ok(false) => {}
                Err(e) => tracing::warn!(subscription = %text(r, 0), error = %e, "entitlement sync failed"),
            }
        }
        Ok(rep)
    }

    /// Runs maintenance on request (platform administrators).
    pub async fn maintenance(&self, ctx: &AccessContext) -> Result<MaintenanceReport> {
        self.require_admin(ctx)?;
        self.run_maintenance().await
    }

    /// A hosted page where the customer saves or replaces its payment method
    /// (the subject itself or `billing:write`).
    pub async fn payment_method_page(
        &self,
        ctx: &AccessContext,
        input: crate::model::PaymentMethodInput,
    ) -> Result<crate::model::HostedPage> {
        let subject = self.subject_for(ctx, input.subject.as_deref())?;
        self.can_manage(ctx, &subject)?;
        let return_url = input.return_url.as_deref().map(check_url).transpose()?;
        let cust = self.ensure_customer(&subject).await?;
        let (provider, cref) = self.provider_customer(&cust).await?;
        provider
            .hosted_setup(&crate::provider::HostedSetupRequest {
                customer: cref,
                currency: cust.currency.clone(),
                return_url: return_url.unwrap_or_else(|| "https://localhost/billing/return".into()),
                idempotency_key: format!("{}:setup:{}", cust.id, self.now() / 60_000),
            })
            .await
    }
}
