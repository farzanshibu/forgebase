//! Administrator views and catalog archiving (MDF-59 / FIX-4): a plan-change
//! preview, all orders with paging, instance-wide usage meters, the dunning
//! queue, and archiving of products, prices, coupons and tax rates.
//!
//! Everything here is add-only: no existing route, status code or field
//! changes. Archiving sets the existing `active` flag to `false`; archived
//! items cannot be bought or applied to new subscriptions, while
//! subscriptions that already use them keep renewing.

use forgebase_data::AccessContext;
use forgebase_entitlements::model::{ts, ts_opt};
use serde::Serialize;
use serde_json::json;
use utoipa::ToSchema;

use crate::error::{BillingError, Result};
use crate::model::*;
use crate::service::{BillingService, Tx, int, opt_int, opt_text, realm_clause, text};

/// One line of a plan-change preview (the same lines the change would invoice).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PreviewLine {
    /// `proration`.
    pub kind: LineKind,
    /// What the line is for.
    pub description: String,
    /// Price key.
    pub price: Option<String>,
    /// Units.
    pub quantity: i64,
    /// Minor units; negative for a credit.
    pub amount: i64,
}

/// What `POST /billing/subscriptions/{id}/change` would do, without doing it.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProrationPreview {
    /// Subscription id.
    pub subscription_id: String,
    /// Currency of every amount.
    pub currency: String,
    /// Current plan price key.
    pub from_price: Option<String>,
    /// New plan price key.
    pub to_price: String,
    /// Units after the change.
    pub quantity: i64,
    /// Proration mode that applies (`none` for trials and period-end changes).
    pub proration: Proration,
    /// The change is scheduled for the period end.
    pub at_period_end: bool,
    /// When the change takes effect.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub effective_at: i64,
    /// Current period start.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub period_start: i64,
    /// Current period end.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub period_end: i64,
    /// Proration lines (credit for unused time, charge for the rest).
    pub lines: Vec<PreviewLine>,
    /// Credit for unused time (positive number, minor units).
    pub credit: i64,
    /// Charge for the remaining time (minor units).
    pub charge: i64,
    /// `charge - credit`: positive is collected now (`always_invoice`) or at
    /// renewal (`create_prorations`); negative goes to the customer balance.
    pub net: i64,
    /// An invoice is raised immediately.
    pub invoiced_now: bool,
    /// Amount of the plan price for a whole period at the new price.
    pub new_period_amount: i64,
}

/// A page of orders.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct OrderPage {
    /// Orders, newest first.
    pub items: Vec<Order>,
    /// All orders matching the filters.
    pub total: i64,
    /// Page size.
    pub limit: i64,
    /// Rows skipped.
    pub offset: i64,
}

/// Where a meter stands against its limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MeterState {
    /// Under 70 %, or unlimited.
    Ok,
    /// 70 % up to 90 %.
    Warn,
    /// 90 % up to 100 %.
    Critical,
    /// At or over the limit.
    Over,
}

/// The counter of one feature of one subject in its current window.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct UsageMeter {
    /// Subject (`org:<id>` or a principal key).
    pub subject: String,
    /// Feature key.
    pub feature: String,
    /// Counter scope (`""` for the subject itself).
    pub scope_key: String,
    /// Committed usage.
    pub used: i64,
    /// Held by reservations.
    pub reserved: i64,
    /// Rolled over from the previous window.
    pub carried_in: i64,
    /// Limit of the window (`null`: unlimited).
    pub limit: Option<i64>,
    /// `used` as a percentage of `limit`.
    pub percent: Option<f64>,
    /// 70 / 90 / 100 % state.
    pub state: MeterState,
    /// Window start.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub window_start: i64,
    /// Window end.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub window_end: i64,
    /// Last change.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub updated_at: i64,
}

/// A page of usage meters.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct UsageMeterPage {
    /// Meters, most used first.
    pub items: Vec<UsageMeter>,
    /// All meters matching the filters.
    pub total: i64,
    /// Page size.
    pub limit: i64,
    /// Rows skipped.
    pub offset: i64,
}

/// An unpaid invoice that dunning is working on.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DunningEntry {
    /// Invoice id.
    pub invoice_id: String,
    /// Invoice number.
    pub number: Option<String>,
    /// Subject.
    pub subject: String,
    /// Customer id.
    pub customer_id: String,
    /// Subscription id.
    pub subscription_id: Option<String>,
    /// Subscription status (`past_due` while dunning runs).
    pub subscription_status: Option<SubscriptionStatus>,
    /// Currency.
    pub currency: String,
    /// Amount still due.
    pub amount_due: i64,
    /// Payment attempts so far.
    pub attempt_count: i64,
    /// Retries left on the dunning schedule.
    pub retries_left: i64,
    /// Next automatic attempt.
    #[serde(default, with = "ts_opt")]
    #[schema(value_type = Option<String>)]
    pub next_attempt_at: Option<i64>,
    /// Code of the last failed payment.
    pub last_failure_code: Option<String>,
    /// Message of the last failed payment.
    pub last_failure_message: Option<String>,
    /// What happens after the last retry.
    pub final_action: FinalAction,
    /// Invoice created.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub created_at: i64,
}

fn paging(limit: Option<u32>, offset: Option<u32>) -> (i64, i64) {
    (
        i64::from(limit.unwrap_or(50).clamp(1, 500)),
        i64::from(offset.unwrap_or(0)),
    )
}

impl BillingService {
    /// The plan change as it would be made now, with no side effects.
    pub async fn preview_plan_change(
        &self,
        ctx: &AccessContext,
        id: &str,
        input: ChangePlanInput,
    ) -> Result<ProrationPreview> {
        let s = self.managed_sub(ctx, id).await?;
        let c = self.plan_change_calc(s, &input).await?;
        let (start, end) = (c.s.sub.current_period_start, c.s.sub.current_period_end);
        let mut lines = Vec::new();
        let (mut credit, mut charge) = (0, 0);
        for l in &c.lines {
            if l.amount < 0 {
                credit += -l.amount;
            } else {
                charge += l.amount;
            }
            lines.push(PreviewLine {
                kind: l.kind,
                description: l.description.clone(),
                price: l.price.clone(),
                quantity: l.quantity,
                amount: l.amount,
            });
        }
        let new_period_amount = crate::billing::item_amount(&c.new, &c.s.sub.currency, c.quantity)?;
        Ok(ProrationPreview {
            subscription_id: id.to_owned(),
            currency: c.s.sub.currency.clone(),
            from_price: c.old.as_ref().map(|o| o.1.key.clone()),
            to_price: c.new.key.clone(),
            quantity: c.quantity,
            proration: c.mode,
            at_period_end: input.at_period_end,
            effective_at: if input.at_period_end { end } else { c.now },
            period_start: start,
            period_end: end,
            lines,
            credit,
            charge,
            net: charge - credit,
            invoiced_now: c.mode == Proration::AlwaysInvoice && !c.lines.is_empty(),
            new_period_amount,
        })
    }

    /// Every order of the instance, newest first (billing readers).
    pub async fn all_orders(
        &self,
        ctx: &AccessContext,
        subject: Option<&str>,
        status: Option<&str>,
        limit: Option<u32>,
        offset: Option<u32>,
    ) -> Result<OrderPage> {
        let realm = self.can_read_all(ctx)?;
        if let Some(st) = status
            && OrderStatus::parse(st).is_none()
        {
            return Err(BillingError::invalid("invalid_parameter", "unknown order status"));
        }
        let (limit, offset) = paging(limit, offset);
        let mut params = vec![subject.map(str::to_owned).into(), status.map(str::to_owned).into()];
        let only = realm_clause(realm.as_deref(), "subject", &mut params);
        let n = params.len();
        let total = self
            .fetch(
                &format!(
                    "SELECT COUNT(*) FROM _forgebase_billing_orders
                     WHERE (?1 IS NULL OR subject = ?1) AND (?2 IS NULL OR status = ?2){only}"
                ),
                params.clone(),
            )
            .await?
            .rows
            .first()
            .map(|r| int(r, 0))
            .unwrap_or_default();
        let mut p = params;
        p.push(limit.into());
        p.push(offset.into());
        let rows = self
            .fetch(
                &format!(
                    "SELECT id, customer_id, subject, status, currency, invoice_id, created_at FROM _forgebase_billing_orders
                     WHERE (?1 IS NULL OR subject = ?1) AND (?2 IS NULL OR status = ?2){only}
                     ORDER BY created_at DESC, id DESC LIMIT ?{} OFFSET ?{}",
                    n + 1,
                    n + 2
                ),
                p,
            )
            .await?;
        Ok(OrderPage {
            items: rows.rows.iter().map(|r| Self::order_row(r)).collect(),
            total,
            limit,
            offset,
        })
    }

    /// Usage counters of every subject in their current windows (billing
    /// readers), most used first. `min_percent` keeps meters at or above it.
    pub async fn usage_meters(
        &self,
        ctx: &AccessContext,
        subject: Option<&str>,
        feature: Option<&str>,
        min_percent: Option<f64>,
        limit: Option<u32>,
        offset: Option<u32>,
    ) -> Result<UsageMeterPage> {
        let realm = self.can_read_all(ctx)?;
        let (limit, offset) = paging(limit, offset);
        let now = self.now();
        let min = min_percent.unwrap_or(0.0);
        let mut params = vec![
            now.into(),
            subject.map(str::to_owned).into(),
            feature.map(str::to_owned).into(),
            min.into(),
        ];
        let only = realm_clause(realm.as_deref(), "subject", &mut params);
        let n = params.len();
        let filter = format!(
            "window_end > ?1 AND window_start <= ?1 AND (?2 IS NULL OR subject = ?2) AND (?3 IS NULL OR feature_key = ?3)
             AND (?4 <= 0 OR (limit_value IS NOT NULL AND limit_value > 0 AND used * 100.0 >= ?4 * limit_value)){only}"
        );
        let total = self
            .fetch(
                &format!("SELECT COUNT(*) FROM _forgebase_usage_counters WHERE {filter}"),
                params.clone(),
            )
            .await?
            .rows
            .first()
            .map(|r| int(r, 0))
            .unwrap_or_default();
        let mut p = params;
        p.push(limit.into());
        p.push(offset.into());
        let rows = self
            .fetch(
                &format!(
                    "SELECT subject, feature_key, scope_key, used, reserved, carried_in, limit_value, window_start,
                            window_end, updated_at
                     FROM _forgebase_usage_counters WHERE {filter}
                     ORDER BY CASE WHEN limit_value > 0 THEN used * 1.0 / limit_value ELSE 0 END DESC,
                              subject, feature_key, scope_key LIMIT ?{} OFFSET ?{}",
                    n + 1,
                    n + 2
                ),
                p,
            )
            .await?;
        let items = rows
            .rows
            .iter()
            .map(|r| {
                let used = int(r, 3);
                let limit_v = opt_int(r, 6);
                let percent = limit_v.filter(|l| *l > 0).map(|l| used as f64 * 100.0 / l as f64);
                let state = match (limit_v, percent) {
                    (Some(l), _) if l <= 0 && used > 0 => MeterState::Over,
                    (_, Some(p)) if p >= 100.0 => MeterState::Over,
                    (_, Some(p)) if p >= 90.0 => MeterState::Critical,
                    (_, Some(p)) if p >= 70.0 => MeterState::Warn,
                    _ => MeterState::Ok,
                };
                UsageMeter {
                    subject: text(r, 0),
                    feature: text(r, 1),
                    scope_key: text(r, 2),
                    used,
                    reserved: int(r, 4),
                    carried_in: int(r, 5),
                    limit: limit_v,
                    percent,
                    state,
                    window_start: int(r, 7),
                    window_end: int(r, 8),
                    updated_at: int(r, 9),
                }
            })
            .collect();
        Ok(UsageMeterPage {
            items,
            total,
            limit,
            offset,
        })
    }

    /// Open invoices dunning is working on: at least one failed attempt or a
    /// retry scheduled. Most overdue first.
    pub async fn dunning_queue(&self, ctx: &AccessContext, limit: Option<u32>) -> Result<Vec<DunningEntry>> {
        let realm = self.can_read_all(ctx)?;
        let settings = self.settings().await?;
        let limit = i64::from(limit.unwrap_or(200).clamp(1, 500));
        let mut params = vec![limit.into()];
        let only = realm_clause(realm.as_deref(), "i.subject", &mut params);
        let rows = self
            .fetch(
                &format!("SELECT i.id, i.number, i.subject, i.customer_id, i.subscription_id, s.status, i.currency, i.amount_due,
                        i.attempt_count, i.next_attempt_at, i.created_at,
                        (SELECT p.failure_code FROM _forgebase_billing_payments p
                          WHERE p.invoice_id = i.id AND p.status = 'failed' ORDER BY p.created_at DESC, p.attempt DESC LIMIT 1),
                        (SELECT p.failure_message FROM _forgebase_billing_payments p
                          WHERE p.invoice_id = i.id AND p.status = 'failed' ORDER BY p.created_at DESC, p.attempt DESC LIMIT 1)
                 FROM _forgebase_billing_invoices i
                 LEFT JOIN _forgebase_billing_subscriptions s ON s.id = i.subscription_id
                 WHERE i.status = 'open' AND (i.attempt_count > 0 OR i.next_attempt_at IS NOT NULL){only}
                 ORDER BY CASE WHEN i.next_attempt_at IS NULL THEN 1 ELSE 0 END, i.next_attempt_at, i.created_at
                 LIMIT ?1"),
                params,
            )
            .await?;
        let total_retries = i64::try_from(settings.retry_days.len()).unwrap_or(i64::MAX);
        Ok(rows
            .rows
            .iter()
            .map(|r| {
                let attempts = int(r, 8);
                DunningEntry {
                    invoice_id: text(r, 0),
                    number: opt_text(r, 1),
                    subject: text(r, 2),
                    customer_id: text(r, 3),
                    subscription_id: opt_text(r, 4),
                    subscription_status: opt_text(r, 5).and_then(|s| SubscriptionStatus::parse(&s)),
                    currency: text(r, 6),
                    amount_due: int(r, 7),
                    attempt_count: attempts,
                    retries_left: (total_retries - (attempts - 1).max(0)).max(0),
                    next_attempt_at: opt_int(r, 9),
                    last_failure_code: opt_text(r, 11),
                    last_failure_message: opt_text(r, 12),
                    final_action: settings.final_action,
                    created_at: int(r, 10),
                }
            })
            .collect())
    }

    async fn archive(&self, ctx: &AccessContext, table: &str, column: &str, key: &str, what: &str) -> Result<()> {
        self.require_admin(ctx)?;
        let now = self.now();
        let mut tx = self.begin().await?;
        let res = async {
            let mut t = Tx(tx.as_mut());
            let n = t
                .exec(
                    &format!("UPDATE {table} SET active = 0, updated_at = MAX(?2, updated_at + 1) WHERE {column} = ?1 AND active = 1"),
                    vec![key.into(), now.into()],
                )
                .await?;
            if n == 0 {
                let r = t
                    .fetch(&format!("SELECT 1 FROM {table} WHERE {column} = ?1"), vec![key.into()])
                    .await?;
                if r.rows.is_empty() {
                    return Err(BillingError::NotFound(what.into()));
                }
                // Already archived: idempotent.
                return Ok(((), false));
            }
            t.audit(&ctx.principal_key(), &format!("{what}.archive"), what, key, json!({}), now)
                .await?;
            Ok(((), true))
        }
        .await;
        self.end(tx, res).await
    }

    /// Archives a product (`active = false`): it cannot be bought, its
    /// existing subscriptions continue. Idempotent (platform administrators).
    pub async fn archive_product(&self, ctx: &AccessContext, key: &str) -> Result<Product> {
        self.archive(ctx, "_forgebase_billing_products", "key", key, "product")
            .await?;
        self.product(ctx, key).await
    }

    /// Archives a price. Idempotent (platform administrators).
    pub async fn archive_price(&self, ctx: &AccessContext, key: &str) -> Result<Price> {
        self.archive(ctx, "_forgebase_billing_prices", "key", key, "price")
            .await?;
        self.price(ctx, key).await
    }

    /// Archives a coupon: it cannot be redeemed again; subscriptions already
    /// carrying it keep their discount. Idempotent (platform administrators).
    pub async fn archive_coupon(&self, ctx: &AccessContext, code: &str) -> Result<Coupon> {
        let code = code.trim().to_ascii_uppercase();
        self.archive(ctx, "_forgebase_billing_coupons", "code", &code, "coupon")
            .await?;
        self.coupon_by("code", &code).await
    }

    /// Archives the tax rate of a region (no tax is added for it). Idempotent
    /// (platform administrators).
    pub async fn archive_tax_rate(&self, ctx: &AccessContext, region: &str) -> Result<TaxRate> {
        let region = region.trim().to_ascii_uppercase();
        self.archive(ctx, "_forgebase_billing_tax_rates", "region", &region, "tax_rate")
            .await?;
        self.tax_rates(ctx)
            .await?
            .into_iter()
            .find(|r| r.region == region)
            .ok_or_else(|| BillingError::NotFound("tax_rate".into()))
    }
}
