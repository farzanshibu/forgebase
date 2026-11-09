//! Invoices and money movement: building and finalizing invoices (discounts,
//! tax, customer balance, gapless numbers), collecting them (saved payment
//! method or hosted page), applying payment results, dunning, fulfillment of
//! paid lines (FB-73 credits, FB-24 usage packs), the entitlement sync of
//! subscriptions (FB-24 `BillingSync`), credit notes and refunds, manual
//! payments, and the read side (invoices, payments, refunds, portal).

use forgebase_credits::{CreditInput, PurchaseReversal};
use forgebase_data::AccessContext;
use forgebase_db::Value;
use forgebase_entitlements::{AddonQuantity, BillingSync, PackGrantInput, SubscriptionSync};
use serde_json::json;

use crate::error::{BillingError, Result};
use crate::model::{
    Collection, Coupon, CouponKind, CreditNote, CreditNoteInput, FinalAction, Invoice, InvoiceLine, InvoiceStatus,
    LineKind, MarkPaidInput, Payment, PaymentStatus, Portal, Price, ProductKind, Refund, RefundStatus,
    Subscription, SubscriptionItem, SubscriptionStatus, TaxBehavior,
};
use crate::money::{allocate, mul_div, price_amount, tax};
use crate::provider::{ChargeRequest, ChargeResult, HostedPaymentRequest, RefundRequest};
use crate::service::{BillingService, DAY, Tx, check_idem, check_text, int, json_col, new_id, opt_int, opt_text, text};

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

pub(crate) const SUB_COLUMNS: &str = "s.id, s.customer_id, s.subject, s.status, s.currency, s.interval, s.interval_count,
    s.cycle_anchor, s.current_period_start, s.current_period_end, s.trial_ends_at, s.cancel_at_period_end, s.canceled_at,
    s.ended_at, s.cancel_reason, c.code, s.coupon_cycles_left, pp.key, s.provider, s.latest_invoice_id, s.version,
    s.metadata, s.created_at, s.coupon_id, s.updated_at, s.synced_version, s.pending_price_id";

pub(crate) const SUB_FROM: &str = "_forgebase_billing_subscriptions s
    LEFT JOIN _forgebase_billing_coupons c ON c.id = s.coupon_id
    LEFT JOIN _forgebase_billing_prices pp ON pp.id = s.pending_price_id";

/// A subscription row with the fields the service needs beyond the wire type.
#[derive(Debug, Clone)]
pub(crate) struct SubRow {
    pub sub: Subscription,
    pub coupon_id: Option<String>,
    pub updated_at: i64,
    pub synced_version: i64,
    pub pending_price_id: Option<String>,
}

fn sub_row(r: &[Value]) -> SubRow {
    SubRow {
        sub: Subscription {
            id: text(r, 0),
            customer_id: text(r, 1),
            subject: text(r, 2),
            status: SubscriptionStatus::parse(&text(r, 3)).unwrap_or(SubscriptionStatus::Canceled),
            currency: text(r, 4),
            interval: crate::model::Interval::parse(&text(r, 5)).unwrap_or(crate::model::Interval::Month),
            interval_count: int(r, 6).max(1),
            cycle_anchor: int(r, 7),
            current_period_start: int(r, 8),
            current_period_end: int(r, 9),
            trial_ends_at: opt_int(r, 10),
            cancel_at_period_end: int(r, 11) != 0,
            canceled_at: opt_int(r, 12),
            ended_at: opt_int(r, 13),
            cancel_reason: opt_text(r, 14),
            coupon: opt_text(r, 15),
            coupon_cycles_left: opt_int(r, 16),
            pending_price: opt_text(r, 17),
            provider: opt_text(r, 18),
            latest_invoice_id: opt_text(r, 19),
            items: Vec::new(),
            version: int(r, 20),
            metadata: json_col(r, 21),
            created_at: int(r, 22),
        },
        coupon_id: opt_text(r, 23),
        updated_at: int(r, 24),
        synced_version: int(r, 25),
        pending_price_id: opt_text(r, 26),
    }
}

const INVOICE_COLUMNS: &str = "i.id, i.number, i.customer_id, i.subject, i.subscription_id, i.order_id, i.status,
    i.billing_reason, i.currency, i.subtotal, i.discount, i.tax, i.total, i.balance_applied, i.amount_due, i.amount_paid,
    i.amount_refunded, c.code, i.tax_region, i.period_start, i.period_end, i.attempt_count, i.next_attempt_at,
    i.hosted_url, i.finalized_at, i.paid_at, i.fulfilled_at, i.created_at";

const INVOICE_FROM: &str =
    "_forgebase_billing_invoices i LEFT JOIN _forgebase_billing_coupons c ON c.id = i.coupon_id";

fn invoice_row(r: &[Value]) -> Invoice {
    Invoice {
        id: text(r, 0),
        number: opt_text(r, 1),
        customer_id: text(r, 2),
        subject: text(r, 3),
        subscription_id: opt_text(r, 4),
        order_id: opt_text(r, 5),
        status: InvoiceStatus::parse(&text(r, 6)).unwrap_or(InvoiceStatus::Void),
        billing_reason: text(r, 7),
        currency: text(r, 8),
        subtotal: int(r, 9),
        discount: int(r, 10),
        tax: int(r, 11),
        total: int(r, 12),
        balance_applied: int(r, 13),
        amount_due: int(r, 14),
        amount_paid: int(r, 15),
        amount_refunded: int(r, 16),
        coupon: opt_text(r, 17),
        tax_region: opt_text(r, 18),
        period_start: opt_int(r, 19),
        period_end: opt_int(r, 20),
        attempt_count: int(r, 21),
        next_attempt_at: opt_int(r, 22),
        hosted_url: opt_text(r, 23),
        finalized_at: opt_int(r, 24),
        paid_at: opt_int(r, 25),
        fulfilled_at: opt_int(r, 26),
        lines: Vec::new(),
        created_at: int(r, 27),
    }
}

const LINE_COLUMNS: &str = "id, kind, description, product_key, price_key, feature_key, quantity, unit_amount, amount,
    discount_amount, tax_rate_bp, tax_amount, tax_inclusive, period_start, period_end";

fn line_row(r: &[Value]) -> InvoiceLine {
    InvoiceLine {
        id: text(r, 0),
        kind: LineKind::parse(&text(r, 1)).unwrap_or(LineKind::Adjustment),
        description: text(r, 2),
        product: opt_text(r, 3),
        price: opt_text(r, 4),
        feature: opt_text(r, 5),
        quantity: int(r, 6),
        unit_amount: opt_int(r, 7),
        amount: int(r, 8),
        discount_amount: int(r, 9),
        tax_rate_bp: int(r, 10),
        tax_amount: int(r, 11),
        tax_inclusive: int(r, 12) != 0,
        period_start: opt_int(r, 13),
        period_end: opt_int(r, 14),
    }
}

const PAYMENT_COLUMNS: &str = "id, invoice_id, subject, provider, provider_ref, status, amount, currency, amount_refunded,
    failure_code, failure_message, attempt, created_at, updated_at";

pub(crate) fn payment_row(r: &[Value]) -> Payment {
    Payment {
        id: text(r, 0),
        invoice_id: text(r, 1),
        subject: text(r, 2),
        provider: text(r, 3),
        provider_ref: opt_text(r, 4),
        status: PaymentStatus::parse(&text(r, 5)).unwrap_or(PaymentStatus::Failed),
        amount: int(r, 6),
        currency: text(r, 7),
        amount_refunded: int(r, 8),
        failure_code: opt_text(r, 9),
        failure_message: opt_text(r, 10),
        attempt: int(r, 11),
        created_at: int(r, 12),
        updated_at: int(r, 13),
    }
}

const REFUND_COLUMNS: &str =
    "id, payment_id, invoice_id, credit_note_id, provider, provider_ref, status, amount, currency, reason, created_at";

fn refund_row(r: &[Value]) -> Refund {
    Refund {
        id: text(r, 0),
        payment_id: text(r, 1),
        invoice_id: text(r, 2),
        credit_note_id: opt_text(r, 3),
        provider: text(r, 4),
        provider_ref: opt_text(r, 5),
        status: RefundStatus::parse(&text(r, 6)).unwrap_or(RefundStatus::Pending),
        amount: int(r, 7),
        currency: text(r, 8),
        reason: opt_text(r, 9),
        created_at: int(r, 10),
    }
}

const NOTE_COLUMNS: &str = "id, number, invoice_id, subject, currency, amount, refund_amount, balance_amount, reason,
    credits_reversed, created_at";

fn note_row(r: &[Value]) -> CreditNote {
    CreditNote {
        id: text(r, 0),
        number: text(r, 1),
        invoice_id: text(r, 2),
        subject: text(r, 3),
        currency: text(r, 4),
        amount: int(r, 5),
        refund_amount: int(r, 6),
        balance_amount: int(r, 7),
        reason: opt_text(r, 8),
        credits_reversed: int(r, 9),
        refund: None,
        created_at: int(r, 10),
    }
}

// ---------------------------------------------------------------------------
// Invoice building
// ---------------------------------------------------------------------------

/// A line before it is stored.
#[derive(Debug, Clone)]
pub(crate) struct LineDraft {
    pub kind: LineKind,
    pub description: String,
    pub product: Option<String>,
    pub price: Option<String>,
    pub feature: Option<String>,
    pub quantity: i64,
    pub unit_amount: Option<i64>,
    pub amount: i64,
    pub tax_inclusive: bool,
    pub period: Option<(i64, i64)>,
    /// Coupons apply to it.
    pub discountable: bool,
    /// Discount set when the invoice is built.
    pub discount: i64,
}

/// The amount of `quantity` units of `price` in `currency`.
pub(crate) fn item_amount(price: &Price, currency: &str, quantity: i64) -> Result<i64> {
    let (unit, tiers) = price.amounts(currency).ok_or_else(|| {
        BillingError::invalid(
            "currency_not_supported",
            format!("price {} is not sold in {currency}", price.key),
        )
    })?;
    price_amount(price.scheme, unit, &tiers, quantity)
}

impl LineDraft {
    /// A line for `quantity` units of a price.
    pub fn for_price(kind: LineKind, price: &Price, currency: &str, quantity: i64, period: Option<(i64, i64)>) -> Result<Self> {
        let amount = item_amount(price, currency, quantity)?;
        let unit = (price.scheme == crate::money::Scheme::PerUnit)
            .then(|| price.amounts(currency).map(|(u, _)| u))
            .flatten();
        Ok(Self {
            kind,
            description: match kind {
                LineKind::Usage => format!("{} × {} ({})", quantity, price.product, price.feature.clone().unwrap_or_default()),
                _ if quantity == 1 => price.product.clone(),
                _ => format!("{} × {}", quantity, price.product),
            },
            product: Some(price.product.clone()),
            price: Some(price.key.clone()),
            feature: price.feature.clone(),
            quantity,
            unit_amount: unit,
            amount,
            tax_inclusive: price.tax_behavior == TaxBehavior::Inclusive,
            period,
            discountable: true,
            discount: 0,
        })
    }
}

/// What an invoice is for.
#[derive(Debug, Clone, Default)]
pub(crate) struct InvoiceSpec {
    pub customer_id: String,
    pub subject: String,
    pub currency: String,
    pub reason: &'static str,
    pub subscription_id: Option<String>,
    pub order_id: Option<String>,
    pub period: Option<(i64, i64)>,
    /// An existing draft (pending prorations) to finalize with the new lines.
    pub draft_id: Option<String>,
}

impl BillingService {
    /// Builds and finalizes an invoice inside `t`: discounts, tax per line,
    /// totals, the customer balance and a gapless number. An invoice with
    /// nothing to collect is paid at once; a negative one (downgrade credit)
    /// credits the customer balance.
    pub(crate) async fn build_invoice(
        &self,
        t: &mut Tx<'_>,
        spec: &InvoiceSpec,
        mut lines: Vec<LineDraft>,
        coupon: Option<&Coupon>,
        now: i64,
    ) -> Result<Invoice> {
        let cust = t
            .fetch(
                "SELECT region, tax_exempt, balance FROM _forgebase_billing_customers WHERE id = ?1",
                vec![spec.customer_id.clone().into()],
            )
            .await?;
        let (region, exempt, balance) = cust
            .rows
            .first()
            .map(|r| (opt_text(r, 0), int(r, 1) != 0, int(r, 2)))
            .ok_or_else(|| BillingError::NotFound("customer".into()))?;
        let (rate_bp, tax_region) = if exempt {
            (0, None)
        } else {
            self.tax_rate_for(region.as_deref()).await?
        };
        let settings = self.settings().await?;
        let id = match &spec.draft_id {
            Some(d) => d.clone(),
            None => {
                let id = new_id("in");
                t.exec(
                    "INSERT INTO _forgebase_billing_invoices (id, customer_id, subject, subscription_id, order_id, status,
                         billing_reason, currency, period_start, period_end, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, 'draft', ?6, ?7, ?8, ?9, ?10, ?10)",
                    vec![
                        id.clone().into(),
                        spec.customer_id.clone().into(),
                        spec.subject.clone().into(),
                        spec.subscription_id.clone().into(),
                        spec.order_id.clone().into(),
                        spec.reason.into(),
                        spec.currency.clone().into(),
                        spec.period.map(|p| p.0).into(),
                        spec.period.map(|p| p.1).into(),
                        now.into(),
                    ],
                )
                .await?;
                id
            }
        };
        // Pending lines of a draft take part in the totals.
        let mut position = t
            .fetch(
                "SELECT COALESCE(MAX(position), 0) FROM _forgebase_billing_invoice_lines WHERE invoice_id = ?1",
                vec![id.clone().into()],
            )
            .await?
            .scalar()
            .and_then(Value::as_i64)
            .unwrap_or_default();
        // Discounts on eligible lines.
        if let Some(c) = coupon {
            let eligible: Vec<usize> = lines
                .iter()
                .enumerate()
                .filter(|(_, l)| {
                    l.discountable
                        && l.amount > 0
                        && (c.products.is_empty() || l.product.as_ref().is_some_and(|p| c.products.contains(p)))
                })
                .map(|(i, _)| i)
                .collect();
            let weights: Vec<i64> = eligible.iter().map(|i| lines[*i].amount).collect();
            let base: i64 = weights.iter().sum();
            let total_off = match c.kind {
                CouponKind::Percent => None,
                CouponKind::Amount => Some(c.amount_off.unwrap_or_default().min(base)),
            };
            let parts = match total_off {
                Some(off) => allocate(off, &weights),
                None => weights
                    .iter()
                    .map(|w| mul_div(*w, c.percent_bp.unwrap_or_default(), 10_000))
                    .collect(),
            };
            for (k, i) in eligible.iter().enumerate() {
                let off = parts.get(k).copied().unwrap_or_default();
                discounts_set(&mut lines[*i], off);
            }
        }
        for l in &lines {
            position += 1;
            let off = discount_of(l);
            let tax_amount = tax(l.amount - off, rate_bp, l.tax_inclusive);
            t.exec(
                "INSERT INTO _forgebase_billing_invoice_lines (id, invoice_id, position, kind, description, product_key,
                     price_key, feature_key, quantity, unit_amount, amount, discount_amount, tax_rate_bp, tax_amount,
                     tax_inclusive, period_start, period_end)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
                vec![
                    new_id("il").into(),
                    id.clone().into(),
                    position.into(),
                    l.kind.as_str().into(),
                    l.description.clone().into(),
                    l.product.clone().into(),
                    l.price.clone().into(),
                    l.feature.clone().into(),
                    l.quantity.into(),
                    l.unit_amount.into(),
                    l.amount.into(),
                    off.into(),
                    rate_bp.into(),
                    tax_amount.into(),
                    l.tax_inclusive.into(),
                    l.period.map(|p| p.0).into(),
                    l.period.map(|p| p.1).into(),
                ],
            )
            .await?;
        }
        // Totals over every line of the invoice (draft lines included).
        let sums = t
            .fetch(
                "SELECT COALESCE(SUM(amount), 0), COALESCE(SUM(discount_amount), 0), COALESCE(SUM(tax_amount), 0),
                        COALESCE(SUM(CASE WHEN tax_inclusive = 0 THEN tax_amount ELSE 0 END), 0)
                 FROM _forgebase_billing_invoice_lines WHERE invoice_id = ?1",
                vec![id.clone().into()],
            )
            .await?;
        let r = sums.rows.first().cloned().unwrap_or_default();
        let (subtotal, discount, tax_total, exclusive) = (int(&r, 0), int(&r, 1), int(&r, 2), int(&r, 3));
        let total = subtotal - discount + exclusive;
        let (applied, due, credit) = if total > 0 {
            let applied = balance.min(total);
            (applied, total - applied, 0)
        } else {
            (0, 0, -total)
        };
        if applied > 0 || credit > 0 {
            t.exec(
                "UPDATE _forgebase_billing_customers SET balance = balance - ?2 + ?3, updated_at = ?4 WHERE id = ?1",
                vec![spec.customer_id.clone().into(), applied.into(), credit.into(), now.into()],
            )
            .await?;
        }
        let number = t.next_number(&settings.invoice_prefix).await?;
        let paid = due == 0;
        t.exec(
            "UPDATE _forgebase_billing_invoices SET number = ?2, status = ?3, subtotal = ?4, discount = ?5, tax = ?6,
                 total = ?7, balance_applied = ?8, amount_due = ?9, coupon_id = ?10, tax_region = ?11,
                 finalized_at = ?12, paid_at = ?13, billing_reason = ?14, period_start = COALESCE(?15, period_start),
                 period_end = COALESCE(?16, period_end), updated_at = ?12
             WHERE id = ?1",
            vec![
                id.clone().into(),
                number.into(),
                if paid { "paid" } else { "open" }.into(),
                subtotal.into(),
                discount.into(),
                tax_total.into(),
                total.into(),
                applied.into(),
                due.into(),
                coupon.filter(|_| discount > 0).map(|c| c.id.clone()).into(),
                tax_region.into(),
                now.into(),
                paid.then_some(now).into(),
                spec.reason.into(),
                spec.period.map(|p| p.0).into(),
                spec.period.map(|p| p.1).into(),
            ],
        )
        .await?;
        if let Some(s) = &spec.subscription_id {
            t.exec(
                "UPDATE _forgebase_billing_subscriptions SET latest_invoice_id = ?2 WHERE id = ?1",
                vec![s.clone().into(), id.clone().into()],
            )
            .await?;
        }
        t.audit(
            "system",
            "invoice.finalize",
            "invoice",
            &id,
            json!({"total": total, "currency": spec.currency, "reason": spec.reason}),
            now,
        )
        .await?;
        load_invoice_tx(t, &id).await
    }

    /// Adds lines to the subscription's draft invoice (pending prorations,
    /// billed with the next renewal).
    pub(crate) async fn add_pending_lines(&self, t: &mut Tx<'_>, sub: &Subscription, lines: &[LineDraft], now: i64) -> Result<()> {
        let draft = t
            .fetch(
                "SELECT id FROM _forgebase_billing_invoices WHERE subscription_id = ?1 AND status = 'draft'",
                vec![sub.id.clone().into()],
            )
            .await?;
        let id = match draft.rows.first() {
            Some(r) => text(r, 0),
            None => {
                let id = new_id("in");
                t.exec(
                    "INSERT INTO _forgebase_billing_invoices (id, customer_id, subject, subscription_id, status,
                         billing_reason, currency, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, 'draft', 'subscription_cycle', ?5, ?6, ?6)",
                    vec![
                        id.clone().into(),
                        sub.customer_id.clone().into(),
                        sub.subject.clone().into(),
                        sub.id.clone().into(),
                        sub.currency.clone().into(),
                        now.into(),
                    ],
                )
                .await?;
                id
            }
        };
        let base = t
            .fetch(
                "SELECT COALESCE(MAX(position), 0) FROM _forgebase_billing_invoice_lines WHERE invoice_id = ?1",
                vec![id.clone().into()],
            )
            .await?
            .scalar()
            .and_then(Value::as_i64)
            .unwrap_or_default();
        for (i, l) in lines.iter().enumerate() {
            t.exec(
                "INSERT INTO _forgebase_billing_invoice_lines (id, invoice_id, position, kind, description, product_key,
                     price_key, quantity, unit_amount, amount, tax_inclusive, period_start, period_end)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                vec![
                    new_id("il").into(),
                    id.clone().into(),
                    (base + 1 + i64::try_from(i).unwrap_or_default()).into(),
                    l.kind.as_str().into(),
                    l.description.clone().into(),
                    l.product.clone().into(),
                    l.price.clone().into(),
                    l.quantity.into(),
                    l.unit_amount.into(),
                    l.amount.into(),
                    l.tax_inclusive.into(),
                    l.period.map(|p| p.0).into(),
                    l.period.map(|p| p.1).into(),
                ],
            )
            .await?;
        }
        Ok(())
    }

    /// The tax of draft lines is set when the draft is finalized.
    pub(crate) async fn retax_draft(&self, t: &mut Tx<'_>, draft_id: &str, customer_id: &str) -> Result<()> {
        let cust = t
            .fetch(
                "SELECT region, tax_exempt FROM _forgebase_billing_customers WHERE id = ?1",
                vec![customer_id.into()],
            )
            .await?;
        let (region, exempt) = cust
            .rows
            .first()
            .map(|r| (opt_text(r, 0), int(r, 1) != 0))
            .unwrap_or_default();
        let (bp, _) = if exempt { (0, None) } else { self.tax_rate_for(region.as_deref()).await? };
        let lines = t
            .fetch(
                "SELECT id, amount, discount_amount, tax_inclusive FROM _forgebase_billing_invoice_lines WHERE invoice_id = ?1",
                vec![draft_id.into()],
            )
            .await?;
        for r in &lines.rows {
            let tx_amount = tax(int(r, 1) - int(r, 2), bp, int(r, 3) != 0);
            t.exec(
                "UPDATE _forgebase_billing_invoice_lines SET tax_rate_bp = ?2, tax_amount = ?3 WHERE id = ?1",
                vec![text(r, 0).into(), bp.into(), tx_amount.into()],
            )
            .await?;
        }
        Ok(())
    }
}

fn discounts_set(l: &mut LineDraft, off: i64) {
    l.discount = off.clamp(0, l.amount.max(0));
}

fn discount_of(l: &LineDraft) -> i64 {
    l.discount
}

pub(crate) async fn load_invoice_tx(t: &mut Tx<'_>, id: &str) -> Result<Invoice> {
    let rows = t
        .fetch(
            &format!("SELECT {INVOICE_COLUMNS} FROM {INVOICE_FROM} WHERE i.id = ?1"),
            vec![id.into()],
        )
        .await?;
    let mut inv = rows
        .rows
        .first()
        .map(|r| invoice_row(r))
        .ok_or_else(|| BillingError::NotFound("invoice".into()))?;
    let lines = t
        .fetch(
            &format!("SELECT {LINE_COLUMNS} FROM _forgebase_billing_invoice_lines WHERE invoice_id = ?1 ORDER BY position"),
            vec![id.into()],
        )
        .await?;
    inv.lines = lines.rows.iter().map(|r| line_row(r)).collect();
    Ok(inv)
}

// ---------------------------------------------------------------------------
// Collection, payment results, dunning
// ---------------------------------------------------------------------------

impl BillingService {
    pub(crate) async fn load_invoice(&self, id: &str) -> Result<Invoice> {
        let rows = self
            .fetch(
                &format!("SELECT {INVOICE_COLUMNS} FROM {INVOICE_FROM} WHERE i.id = ?1"),
                vec![id.into()],
            )
            .await?;
        let mut inv = rows
            .rows
            .first()
            .map(|r| invoice_row(r))
            .ok_or_else(|| BillingError::NotFound("invoice".into()))?;
        let lines = self
            .fetch(
                &format!("SELECT {LINE_COLUMNS} FROM _forgebase_billing_invoice_lines WHERE invoice_id = ?1 ORDER BY position"),
                vec![id.into()],
            )
            .await?;
        inv.lines = lines.rows.iter().map(|r| line_row(r)).collect();
        Ok(inv)
    }

    pub(crate) async fn load_sub(&self, id: &str) -> Result<SubRow> {
        let rows = self
            .fetch(&format!("SELECT {SUB_COLUMNS} FROM {SUB_FROM} WHERE s.id = ?1"), vec![id.into()])
            .await?;
        let mut s = rows
            .rows
            .first()
            .map(|r| sub_row(r))
            .ok_or_else(|| BillingError::NotFound("subscription".into()))?;
        s.sub.items = self.items(&s.sub.id).await?;
        Ok(s)
    }

    pub(crate) async fn subs_where(&self, cond: &str, params: Vec<Value>) -> Result<Vec<SubRow>> {
        let rows = self
            .fetch(&format!("SELECT {SUB_COLUMNS} FROM {SUB_FROM} WHERE {cond}"), params)
            .await?;
        let mut out = Vec::with_capacity(rows.rows.len());
        for r in &rows.rows {
            let mut s = sub_row(r);
            s.sub.items = self.items(&s.sub.id).await?;
            out.push(s);
        }
        Ok(out)
    }

    pub(crate) async fn items(&self, sub_id: &str) -> Result<Vec<SubscriptionItem>> {
        let rows = self
            .fetch(
                "SELECT i.id, p.key, pr.key, i.quantity FROM _forgebase_billing_subscription_items i
                 JOIN _forgebase_billing_prices p ON p.id = i.price_id
                 JOIN _forgebase_billing_products pr ON pr.id = p.product_id
                 WHERE i.subscription_id = ?1 ORDER BY i.created_at, i.id",
                vec![sub_id.into()],
            )
            .await?;
        Ok(rows
            .rows
            .iter()
            .map(|r| SubscriptionItem {
                id: text(r, 0),
                price: text(r, 1),
                product: text(r, 2),
                quantity: int(r, 3),
            })
            .collect())
    }

    /// Collects an open invoice: charges the saved payment method, else
    /// returns the provider's hosted payment page. A transport failure leaves
    /// the payment `pending`; the next attempt reuses its idempotency key, so
    /// the provider never charges twice for one attempt.
    /// `automatic`: a renewal or dunning attempt; without a saved payment
    /// method it counts as a failed attempt (the subscription goes `past_due`
    /// and dunning runs), while a customer-initiated payment only returns the
    /// hosted page.
    pub(crate) async fn collect(&self, invoice_id: &str, return_url: Option<&str>, automatic: bool) -> Result<Collection> {
        let inv = self.load_invoice(invoice_id).await?;
        if inv.status != InvoiceStatus::Open || inv.amount_due == 0 {
            return Ok(Collection {
                invoice: inv,
                payment: None,
                payment_url: None,
            });
        }
        let cust = self.customer_by_id(&inv.customer_id).await?;
        let (provider, cref) = self.provider_customer(&cust).await?;
        let pm = self.payment_method(&cust.id).await?;
        let now = self.now();
        if provider.name() == "manual" {
            // Paid out of band; an operator records the payment.
            return Ok(Collection {
                invoice: inv,
                payment: None,
                payment_url: None,
            });
        }
        let Some(pm) = pm else {
            let url = match &inv.hosted_url {
                Some(u) => u.clone(),
                None => {
                    let page = provider
                        .hosted_payment(&HostedPaymentRequest {
                            invoice_id: inv.id.clone(),
                            description: format!("Invoice {}", inv.number.clone().unwrap_or_default()),
                            amount: inv.amount_due,
                            currency: inv.currency.clone(),
                            customer: cref.clone(),
                            return_url: return_url.unwrap_or("https://localhost/billing/return").to_owned(),
                            idempotency_key: format!("{}:hosted", inv.id),
                        })
                        .await?;
                    let mut tx = self.begin().await?;
                    let res = async {
                        let mut t = Tx(tx.as_mut());
                        t.exec(
                            "UPDATE _forgebase_billing_invoices SET hosted_url = ?2, updated_at = ?3 WHERE id = ?1",
                            vec![inv.id.clone().into(), page.url.clone().into(), now.into()],
                        )
                        .await?;
                        Ok(((), true))
                    }
                    .await;
                    self.end(tx, res).await?;
                    page.url
                }
            };
            if automatic && inv.subscription_id.is_some() && inv.billing_reason != "subscription_create" {
                let mut tx = self.begin().await?;
                let res = async {
                    let mut t = Tx(tx.as_mut());
                    t.exec(
                        "UPDATE _forgebase_billing_invoices SET attempt_count = attempt_count + 1, updated_at = ?2 WHERE id = ?1",
                        vec![inv.id.clone().into(), now.into()],
                    )
                    .await?;
                    let inv = load_invoice_tx(&mut t, &inv.id).await?;
                    t.audit("system", "payment.failed", "invoice", &inv.id, json!({"code": "no_payment_method"}), now)
                        .await?;
                    self.on_failure(&mut t, &inv, now).await?;
                    Ok(((), true))
                }
                .await;
                self.end(tx, res).await?;
                if let Some(sid) = &inv.subscription_id {
                    self.sync_logged(sid).await;
                }
            }
            return Ok(Collection {
                invoice: self.load_invoice(invoice_id).await?,
                payment: None,
                payment_url: Some(url),
            });
        };
        // The attempt: reuse an unresolved pending payment, else a new one.
        let mut tx = self.begin().await?;
        let res = async {
            let mut t = Tx(tx.as_mut());
            let pending = t
                .fetch(
                    "SELECT id, idempotency_key, attempt FROM _forgebase_billing_payments
                     WHERE invoice_id = ?1 AND status = 'pending' AND provider = ?2 ORDER BY attempt DESC LIMIT 1",
                    vec![inv.id.clone().into(), provider.name().into()],
                )
                .await?;
            if let Some(r) = pending.rows.first() {
                return Ok(((text(r, 0), text(r, 1), int(r, 2)), false));
            }
            let attempt = inv.attempt_count + 1;
            let id = new_id("pay");
            let key = format!("{}:attempt:{attempt}", inv.id);
            t.exec(
                "INSERT INTO _forgebase_billing_payments (id, invoice_id, customer_id, subject, provider, status, amount,
                     currency, attempt, idempotency_key, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6, ?7, ?8, ?9, ?10, ?10)",
                vec![
                    id.clone().into(),
                    inv.id.clone().into(),
                    cust.id.clone().into(),
                    inv.subject.clone().into(),
                    provider.name().into(),
                    inv.amount_due.into(),
                    inv.currency.clone().into(),
                    attempt.into(),
                    key.clone().into(),
                    now.into(),
                ],
            )
            .await?;
            t.exec(
                "UPDATE _forgebase_billing_invoices SET attempt_count = ?2, updated_at = ?3 WHERE id = ?1",
                vec![inv.id.clone().into(), attempt.into(), now.into()],
            )
            .await?;
            Ok(((id, key, attempt), true))
        }
        .await;
        let (payment_id, key, _attempt) = self.end(tx, res).await?;
        let result = provider
            .charge(&ChargeRequest {
                invoice_id: inv.id.clone(),
                invoice_number: inv.number.clone().unwrap_or_default(),
                provider_customer_id: cref.provider_customer_id.clone().unwrap_or_default(),
                payment_method: pm,
                email: cust.email.clone(),
                amount: inv.amount_due,
                currency: inv.currency.clone(),
                idempotency_key: key,
                description: format!("Invoice {}", inv.number.clone().unwrap_or_default()),
            })
            .await;
        let result = match result {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(invoice = %inv.id, error = %e, "charge failed in transport; payment left pending");
                // Unknown outcome: keep it pending and retry on the dunning schedule.
                let mut tx = self.begin().await?;
                let res = async {
                    let mut t = Tx(tx.as_mut());
                    self.schedule_retry(&mut t, &inv, now).await?;
                    Ok(((), true))
                }
                .await;
                self.end(tx, res).await?;
                return Ok(Collection {
                    invoice: self.load_invoice(invoice_id).await?,
                    payment: self.payment_by_id(&payment_id).await.ok(),
                    payment_url: None,
                });
            }
        };
        let paid = self.apply_charge(&payment_id, &result, None).await?;
        if paid {
            self.after_paid(invoice_id).await;
        } else if let Some(sid) = &inv.subscription_id {
            // A failure may have moved the subscription (past_due, canceled).
            self.sync_logged(sid).await;
        }
        let payment = self.payment_by_id(&payment_id).await.ok();
        let invoice = self.load_invoice(invoice_id).await?;
        let payment_url = if invoice.status == InvoiceStatus::Open && result.status == PaymentStatus::Failed {
            // Let the customer pay by hand on the provider page.
            invoice.hosted_url.clone()
        } else {
            None
        };
        Ok(Collection {
            invoice,
            payment,
            payment_url,
        })
    }

    pub(crate) async fn payment_by_id(&self, id: &str) -> Result<Payment> {
        let rows = self
            .fetch(
                &format!("SELECT {PAYMENT_COLUMNS} FROM _forgebase_billing_payments WHERE id = ?1"),
                vec![id.into()],
            )
            .await?;
        rows.rows
            .first()
            .map(|r| payment_row(r))
            .ok_or_else(|| BillingError::NotFound("payment".into()))
    }

    /// Applies a charge result to its payment. Returns whether the invoice
    /// became paid.
    pub(crate) async fn apply_charge(&self, payment_id: &str, r: &ChargeResult, event_at: Option<i64>) -> Result<bool> {
        let now = self.now();
        let mut tx = self.begin().await?;
        let res = async {
            let mut t = Tx(tx.as_mut());
            let paid = self.apply_charge_tx(&mut t, payment_id, r, event_at, now).await?;
            Ok((paid, true))
        }
        .await;
        let paid = self.end(tx, res).await?;
        Ok(paid)
    }

    pub(crate) async fn apply_charge_tx(
        &self,
        t: &mut Tx<'_>,
        payment_id: &str,
        r: &ChargeResult,
        event_at: Option<i64>,
        now: i64,
    ) -> Result<bool> {
        let rows = t
            .fetch(
                "SELECT invoice_id, status, amount FROM _forgebase_billing_payments WHERE id = ?1",
                vec![payment_id.into()],
            )
            .await?;
        let Some(row) = rows.rows.first() else {
            return Err(BillingError::NotFound("payment".into()));
        };
        let (invoice_id, status, amount) = (text(row, 0), text(row, 1), int(row, 2));
        if status == "succeeded" {
            return Ok(false);
        }
        t.exec(
            "UPDATE _forgebase_billing_payments SET status = ?2, provider_ref = COALESCE(?3, provider_ref),
                 failure_code = ?4, failure_message = ?5, provider_event_at = COALESCE(?6, provider_event_at),
                 updated_at = ?7
             WHERE id = ?1",
            vec![
                payment_id.into(),
                r.status.as_str().into(),
                r.provider_ref.clone().into(),
                r.failure_code.clone().into(),
                r.failure_message.clone().into(),
                event_at.into(),
                now.into(),
            ],
        )
        .await?;
        let inv = load_invoice_tx(t, &invoice_id).await?;
        match r.status {
            PaymentStatus::Succeeded => self.mark_paid_tx(t, &inv, amount, now).await,
            PaymentStatus::Failed => {
                t.audit(
                    "system",
                    "payment.failed",
                    "invoice",
                    &invoice_id,
                    json!({"code": r.failure_code, "attempt": inv.attempt_count}),
                    now,
                )
                .await?;
                self.on_failure(t, &inv, now).await?;
                Ok(false)
            }
            PaymentStatus::Pending | PaymentStatus::Canceled => Ok(false),
        }
    }

    /// Marks an invoice paid and moves what depends on it: an `incomplete` or
    /// `past_due` subscription becomes `active`, an open order `paid`.
    /// Returns false when the invoice was not open (already paid, void).
    pub(crate) async fn mark_paid_tx(&self, t: &mut Tx<'_>, inv: &Invoice, amount: i64, now: i64) -> Result<bool> {
        let n = t
            .exec(
                "UPDATE _forgebase_billing_invoices SET status = 'paid', amount_paid = amount_paid + ?2, paid_at = ?3,
                     next_attempt_at = NULL, updated_at = ?3
                 WHERE id = ?1 AND status IN ('open', 'uncollectible')",
                vec![inv.id.clone().into(), amount.into(), now.into()],
            )
            .await?;
        if n == 0 {
            return Ok(false);
        }
        if let Some(s) = &inv.subscription_id {
            // Incomplete subscriptions start their first period now.
            t.exec(
                "UPDATE _forgebase_billing_subscriptions SET status = 'active', version = version + 1,
                     updated_at = MAX(?2, updated_at + 1)
                 WHERE id = ?1 AND status IN ('incomplete', 'past_due')",
                vec![s.clone().into(), now.into()],
            )
            .await?;
        }
        if let Some(o) = &inv.order_id {
            t.exec(
                "UPDATE _forgebase_billing_orders SET status = 'paid', updated_at = ?2 WHERE id = ?1 AND status = 'open'",
                vec![o.clone().into(), now.into()],
            )
            .await?;
        }
        t.audit("system", "invoice.paid", "invoice", &inv.id, json!({"amount": amount}), now)
            .await?;
        Ok(true)
    }

    /// After a failed attempt: schedule the next retry, or apply the final
    /// action of the dunning settings. A failed first invoice keeps its
    /// subscription `incomplete` (the customer pays on the hosted page).
    pub(crate) async fn on_failure(&self, t: &mut Tx<'_>, inv: &Invoice, now: i64) -> Result<()> {
        let first = inv.billing_reason == "subscription_create";
        if first || inv.subscription_id.is_none() {
            t.exec(
                "UPDATE _forgebase_billing_invoices SET next_attempt_at = NULL, updated_at = ?2 WHERE id = ?1",
                vec![inv.id.clone().into(), now.into()],
            )
            .await?;
            return Ok(());
        }
        let settings = self.settings().await?;
        let attempts = inv.attempt_count.max(1);
        let base = inv.finalized_at.unwrap_or(now);
        let sub = inv.subscription_id.clone().unwrap_or_default();
        if let Some(days) = settings.retry_days.get(usize::try_from(attempts - 1).unwrap_or(usize::MAX)) {
            let next = (base + days * DAY).max(now + 1);
            t.exec(
                "UPDATE _forgebase_billing_invoices SET next_attempt_at = ?2, updated_at = ?3 WHERE id = ?1",
                vec![inv.id.clone().into(), next.into(), now.into()],
            )
            .await?;
            t.exec(
                "UPDATE _forgebase_billing_subscriptions SET status = 'past_due', version = version + 1,
                     updated_at = MAX(?2, updated_at + 1)
                 WHERE id = ?1 AND status IN ('active', 'trialing')",
                vec![sub.into(), now.into()],
            )
            .await?;
            return Ok(());
        }
        // Retries exhausted.
        t.exec(
            "UPDATE _forgebase_billing_invoices SET status = 'uncollectible', next_attempt_at = NULL, updated_at = ?2
             WHERE id = ?1 AND status = 'open'",
            vec![inv.id.clone().into(), now.into()],
        )
        .await?;
        if settings.final_action == FinalAction::Cancel {
            t.exec(
                "UPDATE _forgebase_billing_subscriptions SET status = 'canceled', ended_at = ?2, canceled_at = COALESCE(canceled_at, ?2),
                     cancel_reason = 'payment_failed', version = version + 1, updated_at = MAX(?2, updated_at + 1)
                 WHERE id = ?1 AND status <> 'canceled'",
                vec![sub.clone().into(), now.into()],
            )
            .await?;
            t.audit("system", "subscription.dunning_canceled", "subscription", &sub, json!({"invoice": inv.id}), now)
                .await?;
        }
        Ok(())
    }

    /// Schedules a retry after an attempt with an unknown outcome.
    async fn schedule_retry(&self, t: &mut Tx<'_>, inv: &Invoice, now: i64) -> Result<()> {
        if inv.subscription_id.is_none() || inv.billing_reason == "subscription_create" {
            return Ok(());
        }
        let settings = self.settings().await?;
        let idx = usize::try_from(inv.attempt_count.max(1) - 1).unwrap_or_default();
        let days = settings.retry_days.get(idx).copied().unwrap_or(1);
        t.exec(
            "UPDATE _forgebase_billing_invoices SET next_attempt_at = ?2, updated_at = ?3 WHERE id = ?1",
            vec![inv.id.clone().into(), (now + days * DAY).into(), now.into()],
        )
        .await?;
        Ok(())
    }

    /// Delivers a paid invoice and syncs its subscription. Failures are
    /// logged; the maintenance job retries both (idempotent).
    pub(crate) async fn after_paid(&self, invoice_id: &str) {
        if let Err(e) = self.fulfill(invoice_id).await {
            tracing::warn!(invoice = %invoice_id, error = %e, "fulfillment failed; the maintenance job retries");
        }
        if let Ok(inv) = self.load_invoice(invoice_id).await
            && let Some(s) = inv.subscription_id
        {
            self.sync_logged(&s).await;
        }
    }

    pub(crate) async fn sync_logged(&self, sub_id: &str) {
        if let Err(e) = self.sync(sub_id).await {
            tracing::warn!(subscription = %sub_id, error = %e, "entitlement sync failed; the maintenance job retries");
        }
    }

    /// Delivers the paid lines of an invoice (payment-to-entitlement
    /// mapping): credits products top up FB-73 credits, usage-pack products
    /// grant FB-24 packs. Idempotent by `billing:<invoice>:<line>`. Plans and
    /// add-ons are delivered by the subscription sync.
    pub async fn fulfill(&self, invoice_id: &str) -> Result<bool> {
        let inv = self.load_invoice(invoice_id).await?;
        if inv.status != InvoiceStatus::Paid || inv.fulfilled_at.is_some() {
            return Ok(false);
        }
        for l in &inv.lines {
            let Some(pk) = &l.product else { continue };
            if l.amount <= 0 && l.kind != LineKind::OneTime {
                continue;
            }
            let product = match self.product_by("key", pk).await {
                Ok(p) => p,
                Err(BillingError::NotFound(_)) => continue,
                Err(e) => return Err(e),
            };
            let key = format!("billing:{}:{}", inv.id, l.id);
            match product.kind {
                ProductKind::Credits => {
                    let amount = product.credits.unwrap_or_default().saturating_mul(l.quantity.max(1));
                    let mut input = CreditInput::new(&inv.subject, amount, &key);
                    input.reason = Some(format!("purchase {}", inv.number.clone().unwrap_or_default()));
                    input.external_ref = inv.number.clone();
                    self.inner.credits.credit_purchase(input).await?;
                }
                ProductKind::UsagePack => {
                    if let Some(pack) = &product.pack_key {
                        self.inner
                            .ent
                            .grant_usage_pack(PackGrantInput {
                                subject: inv.subject.clone(),
                                pack: pack.clone(),
                                quantity: l.quantity.max(1),
                                idempotency_key: Some(key),
                            })
                            .await?;
                    }
                }
                _ => {}
            }
        }
        let now = self.now();
        let mut tx = self.begin().await?;
        let res = async {
            let mut t = Tx(tx.as_mut());
            let n = t
                .exec(
                    "UPDATE _forgebase_billing_invoices SET fulfilled_at = ?2 WHERE id = ?1 AND fulfilled_at IS NULL",
                    vec![invoice_id.into(), now.into()],
                )
                .await?;
            Ok((n > 0, true))
        }
        .await;
        self.end(tx, res).await
    }

    /// Pushes the subscription's state to FB-24 (`BillingSync`): the plan
    /// product's plan, the add-on items, status, trial and period, the
    /// scheduled cancellation. Idempotent by `billing:<id>:v<version>`;
    /// FB-24 ignores events older than the last one it applied.
    pub async fn sync(&self, sub_id: &str) -> Result<bool> {
        let s = self.load_sub(sub_id).await?;
        if s.synced_version >= s.sub.version {
            return Ok(false);
        }
        let mut plan = None;
        let mut addons = Vec::new();
        for it in &s.sub.items {
            let product = self.product_by("key", &it.product).await?;
            match product.kind {
                ProductKind::Plan => plan = product.plan_key.clone(),
                ProductKind::Addon => {
                    if let Some(a) = product.addon_key.clone() {
                        addons.push(AddonQuantity {
                            addon: a,
                            quantity: it.quantity,
                        });
                    }
                }
                _ => {}
            }
        }
        let status = match s.sub.status {
            SubscriptionStatus::Incomplete if s.synced_version == 0 => None,
            SubscriptionStatus::Incomplete => Some(forgebase_entitlements::SubscriptionStatus::Expired),
            SubscriptionStatus::Trialing => Some(forgebase_entitlements::SubscriptionStatus::Trialing),
            SubscriptionStatus::Active => Some(forgebase_entitlements::SubscriptionStatus::Active),
            SubscriptionStatus::PastDue => Some(forgebase_entitlements::SubscriptionStatus::PastDue),
            SubscriptionStatus::Paused => Some(forgebase_entitlements::SubscriptionStatus::Paused),
            SubscriptionStatus::Canceled => Some(forgebase_entitlements::SubscriptionStatus::Canceled),
        };
        let mut applied = false;
        if let (Some(status), Some(plan)) = (status, plan) {
            self.inner
                .ent
                .sync_subscription(SubscriptionSync {
                    subject: s.sub.subject.clone(),
                    plan,
                    status,
                    trial_ends_at: s.sub.trial_ends_at,
                    current_period_start: Some(s.sub.current_period_start),
                    current_period_end: Some(s.sub.current_period_end),
                    cycle_anchor: Some(s.sub.cycle_anchor),
                    cancel_at: s.sub.cancel_at_period_end.then_some(s.sub.current_period_end),
                    external_ref: Some(s.sub.id.clone()),
                    addons: Some(addons),
                    event_id: Some(format!("billing:{}:v{}", s.sub.id, s.sub.version)),
                    event_at: Some(s.updated_at),
                })
                .await?;
            applied = true;
        }
        let mut tx = self.begin().await?;
        let res = async {
            let mut t = Tx(tx.as_mut());
            t.exec(
                "UPDATE _forgebase_billing_subscriptions SET synced_version = MAX(synced_version, ?2) WHERE id = ?1",
                vec![sub_id.into(), s.sub.version.into()],
            )
            .await?;
            Ok(((), true))
        }
        .await;
        self.end(tx, res).await?;
        Ok(applied)
    }

    // -----------------------------------------------------------------------
    // Reads
    // -----------------------------------------------------------------------

    /// `None` with instance-wide read access lists everything.
    fn list_subject(&self, ctx: &AccessContext, subject: Option<&str>) -> Result<Option<String>> {
        if subject.is_none() && self.can_read_all(ctx).is_ok() {
            return Ok(None);
        }
        let s = self.subject_for(ctx, subject)?;
        self.can_read(ctx, &s)?;
        Ok(Some(s))
    }

    /// Invoices of a subject (newest first), optionally of one status.
    pub async fn invoices(
        &self,
        ctx: &AccessContext,
        subject: Option<&str>,
        status: Option<InvoiceStatus>,
        limit: Option<u32>,
    ) -> Result<Vec<Invoice>> {
        let subject = self.list_subject(ctx, subject)?;
        let limit = i64::from(limit.unwrap_or(50).clamp(1, 500));
        let rows = self
            .fetch(
                &format!(
                    "SELECT {INVOICE_COLUMNS} FROM {INVOICE_FROM}
                     WHERE (?1 IS NULL OR i.subject = ?1) AND (?2 IS NULL OR i.status = ?2) AND i.status <> 'draft'
                     ORDER BY i.created_at DESC, i.id DESC LIMIT ?3"
                ),
                vec![subject.into(), status.map(InvoiceStatus::as_str).into(), limit.into()],
            )
            .await?;
        let mut out = Vec::new();
        for r in &rows.rows {
            out.push(self.load_invoice(&text(r, 0)).await?);
        }
        Ok(out)
    }

    /// One invoice with its lines.
    pub async fn invoice(&self, ctx: &AccessContext, id: &str) -> Result<Invoice> {
        let inv = self.load_invoice(id).await?;
        self.can_read(ctx, &inv.subject)
            .map_err(|_| BillingError::NotFound("invoice".into()))?;
        Ok(inv)
    }

    /// Payments of a subject (newest first).
    pub async fn payments(&self, ctx: &AccessContext, subject: Option<&str>, limit: Option<u32>) -> Result<Vec<Payment>> {
        let subject = self.list_subject(ctx, subject)?;
        let limit = i64::from(limit.unwrap_or(50).clamp(1, 500));
        let rows = self
            .fetch(
                &format!(
                    "SELECT {PAYMENT_COLUMNS} FROM _forgebase_billing_payments WHERE (?1 IS NULL OR subject = ?1)
                     ORDER BY created_at DESC, id DESC LIMIT ?2"
                ),
                vec![subject.into(), limit.into()],
            )
            .await?;
        Ok(rows.rows.iter().map(|r| payment_row(r)).collect())
    }

    /// Refunds of a subject (newest first).
    pub async fn refunds(&self, ctx: &AccessContext, subject: Option<&str>, limit: Option<u32>) -> Result<Vec<Refund>> {
        let subject = self.list_subject(ctx, subject)?;
        let limit = i64::from(limit.unwrap_or(50).clamp(1, 500));
        let rows = self
            .fetch(
                "SELECT r.id, r.payment_id, r.invoice_id, r.credit_note_id, r.provider, r.provider_ref, r.status, r.amount,
                        r.currency, r.reason, r.created_at
                 FROM _forgebase_billing_refunds r JOIN _forgebase_billing_invoices i ON i.id = r.invoice_id
                 WHERE (?1 IS NULL OR i.subject = ?1) ORDER BY r.created_at DESC, r.id DESC LIMIT ?2",
                vec![subject.into(), limit.into()],
            )
            .await?;
        Ok(rows.rows.iter().map(|r| refund_row(r)).collect())
    }

    /// Credit notes of a subject (newest first).
    pub async fn credit_notes(&self, ctx: &AccessContext, subject: Option<&str>, limit: Option<u32>) -> Result<Vec<CreditNote>> {
        let subject = self.list_subject(ctx, subject)?;
        let limit = i64::from(limit.unwrap_or(50).clamp(1, 500));
        let rows = self
            .fetch(
                &format!(
                    "SELECT {NOTE_COLUMNS} FROM _forgebase_billing_credit_notes WHERE (?1 IS NULL OR subject = ?1)
                     ORDER BY created_at DESC, id DESC LIMIT ?2"
                ),
                vec![subject.into(), limit.into()],
            )
            .await?;
        let mut out = Vec::new();
        for r in &rows.rows {
            out.push(self.with_refund(note_row(r)).await?);
        }
        Ok(out)
    }

    async fn with_refund(&self, mut n: CreditNote) -> Result<CreditNote> {
        let rows = self
            .fetch(
                &format!("SELECT {REFUND_COLUMNS} FROM _forgebase_billing_refunds WHERE credit_note_id = ?1"),
                vec![n.id.clone().into()],
            )
            .await?;
        n.refund = rows.rows.first().map(|r| refund_row(r));
        Ok(n)
    }

    /// The customer portal of a subject: customer, live subscriptions,
    /// recent invoices, payments and credit notes.
    pub async fn portal(&self, ctx: &AccessContext, subject: Option<&str>) -> Result<Portal> {
        let subject = self.subject_for(ctx, subject)?;
        self.can_read(ctx, &subject)?;
        let customer = self.customer_by_subject(&subject).await?;
        let subscriptions = self
            .subs_where(
                "s.subject = ?1 AND s.status <> 'canceled' ORDER BY s.created_at DESC",
                vec![subject.clone().into()],
            )
            .await?
            .into_iter()
            .map(|s| s.sub)
            .collect();
        Ok(Portal {
            customer,
            subscriptions,
            invoices: self.invoices(ctx, Some(&subject), None, Some(20)).await?,
            payments: self.payments(ctx, Some(&subject), Some(20)).await?,
            credit_notes: self.credit_notes(ctx, Some(&subject), Some(20)).await?,
        })
    }

    // -----------------------------------------------------------------------
    // Customer and operator actions on invoices
    // -----------------------------------------------------------------------

    /// Tries to collect an open invoice now (the customer, `billing:write`, or
    /// operators): charges the saved method or returns the hosted page.
    pub async fn pay_invoice(&self, ctx: &AccessContext, id: &str, return_url: Option<&str>) -> Result<Collection> {
        let inv = self.invoice(ctx, id).await?;
        self.can_manage(ctx, &inv.subject)?;
        let url = return_url.map(crate::service::check_url).transpose()?;
        if inv.status != InvoiceStatus::Open {
            return Err(BillingError::conflict("invoice_not_open", "only open invoices can be paid"));
        }
        self.collect(id, url.as_deref(), false).await
    }

    /// Records an out-of-band payment (operators; manual billing).
    pub async fn mark_paid(&self, ctx: &AccessContext, id: &str, input: MarkPaidInput) -> Result<Invoice> {
        self.can_operate(ctx)?;
        let reference = check_text(input.reference, "reference", 200)?;
        let now = self.now();
        let mut tx = self.begin().await?;
        let res = async {
            let mut t = Tx(tx.as_mut());
            let inv = load_invoice_tx(&mut t, id).await?;
            if !matches!(inv.status, InvoiceStatus::Open | InvoiceStatus::Uncollectible) {
                return Err(BillingError::conflict("invoice_not_open", "the invoice is not open"));
            }
            let pid = new_id("pay");
            t.exec(
                "INSERT INTO _forgebase_billing_payments (id, invoice_id, customer_id, subject, provider, provider_ref,
                     status, amount, currency, attempt, idempotency_key, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, 'manual', ?5, 'succeeded', ?6, ?7, ?8, ?9, ?10, ?10)",
                vec![
                    pid.clone().into(),
                    inv.id.clone().into(),
                    inv.customer_id.clone().into(),
                    inv.subject.clone().into(),
                    reference.clone().unwrap_or_else(|| format!("manual:{pid}")).into(),
                    inv.amount_due.into(),
                    inv.currency.clone().into(),
                    (inv.attempt_count + 1).into(),
                    format!("{}:manual", inv.id).into(),
                    now.into(),
                ],
            )
            .await?;
            self.mark_paid_tx(&mut t, &inv, inv.amount_due, now).await?;
            t.audit(&ctx.principal_key(), "invoice.mark_paid", "invoice", id, json!({"reference": reference}), now)
                .await?;
            Ok(((), true))
        }
        .await;
        self.end(tx, res).await?;
        self.after_paid(id).await;
        self.load_invoice(id).await
    }

    /// Voids an open invoice (operators). The `incomplete` subscription of a
    /// voided first invoice is cancelled.
    pub async fn void_invoice(&self, ctx: &AccessContext, id: &str) -> Result<Invoice> {
        self.can_operate(ctx)?;
        let now = self.now();
        let mut sub = None;
        let mut tx = self.begin().await?;
        let res = async {
            let mut t = Tx(tx.as_mut());
            let inv = load_invoice_tx(&mut t, id).await?;
            if !matches!(inv.status, InvoiceStatus::Open | InvoiceStatus::Uncollectible | InvoiceStatus::Draft) {
                return Err(BillingError::conflict("invoice_not_open", "only unpaid invoices can be voided"));
            }
            t.exec(
                "UPDATE _forgebase_billing_invoices SET status = 'void', voided_at = ?2, next_attempt_at = NULL,
                     updated_at = ?2 WHERE id = ?1",
                vec![id.into(), now.into()],
            )
            .await?;
            if inv.balance_applied > 0 {
                t.exec(
                    "UPDATE _forgebase_billing_customers SET balance = balance + ?2 WHERE id = ?1",
                    vec![inv.customer_id.clone().into(), inv.balance_applied.into()],
                )
                .await?;
            }
            if let Some(s) = &inv.subscription_id {
                let n = t
                    .exec(
                        "UPDATE _forgebase_billing_subscriptions SET status = 'canceled', ended_at = ?2, canceled_at = ?2,
                             cancel_reason = 'invoice_voided', version = version + 1, updated_at = MAX(?2, updated_at + 1)
                         WHERE id = ?1 AND status = 'incomplete'",
                        vec![s.clone().into(), now.into()],
                    )
                    .await?;
                if n > 0 {
                    sub = Some(s.clone());
                }
            }
            if let Some(o) = &inv.order_id {
                t.exec(
                    "UPDATE _forgebase_billing_orders SET status = 'canceled', updated_at = ?2 WHERE id = ?1 AND status = 'open'",
                    vec![o.clone().into(), now.into()],
                )
                .await?;
            }
            t.audit(&ctx.principal_key(), "invoice.void", "invoice", id, json!({}), now)
                .await?;
            Ok(((), true))
        }
        .await;
        self.end(tx, res).await?;
        if let Some(s) = sub {
            self.sync_logged(&s).await;
        }
        self.load_invoice(id).await
    }

    /// Issues a credit note for a paid invoice (operators): a refund to the
    /// payment method (default) or a credit to the customer balance. Credits
    /// bought on the invoice are taken back in proportion (FB-73
    /// `reverse_purchase`). Idempotent by key.
    pub async fn credit_invoice(&self, ctx: &AccessContext, id: &str, input: CreditNoteInput) -> Result<CreditNote> {
        self.can_operate(ctx)?;
        let idem = check_idem(&input.idempotency_key)?;
        let reason = check_text(input.reason, "reason", 500)?;
        let settings = self.settings().await?;
        let now = self.now();
        // Replay.
        let prior = self
            .fetch(
                &format!("SELECT {NOTE_COLUMNS} FROM _forgebase_billing_credit_notes WHERE idempotency_key = ?1"),
                vec![idem.clone().into()],
            )
            .await?;
        if let Some(r) = prior.rows.first() {
            let n = note_row(r);
            if n.invoice_id != id {
                return Err(BillingError::conflict(
                    "idempotency_conflict",
                    "the idempotency key was used for another invoice",
                ));
            }
            return self.with_refund(n).await;
        }
        let mut tx = self.begin().await?;
        let res = async {
            let mut t = Tx(tx.as_mut());
            let inv = load_invoice_tx(&mut t, id).await?;
            if inv.status != InvoiceStatus::Paid {
                return Err(BillingError::conflict("invoice_not_paid", "only paid invoices can be credited"));
            }
            let creditable = inv.amount_paid + inv.balance_applied - inv.amount_refunded;
            let amount = input.amount.unwrap_or(creditable);
            if amount <= 0 || amount > creditable {
                return Err(BillingError::invalid(
                    "invalid_amount",
                    format!("the amount must be between 1 and {creditable}"),
                ));
            }
            // The refund comes from the newest succeeded payment with room.
            let pay = t
                .fetch(
                    "SELECT id, provider, provider_ref, amount - amount_refunded FROM _forgebase_billing_payments
                     WHERE invoice_id = ?1 AND status = 'succeeded' AND amount > amount_refunded
                     ORDER BY created_at DESC LIMIT 1",
                    vec![id.into()],
                )
                .await?;
            let refundable = pay.rows.first().map(|r| int(r, 3)).unwrap_or_default();
            let (refund_amount, balance_amount) = if input.to_balance {
                (0, amount)
            } else if amount <= refundable {
                (amount, 0)
            } else {
                // What the payment cannot carry (balance applied) goes back to the balance.
                (refundable, amount - refundable)
            };
            let note_id = new_id("cn");
            let number = t.next_number(&settings.credit_note_prefix).await?;
            t.exec(
                "INSERT INTO _forgebase_billing_credit_notes (id, number, invoice_id, customer_id, subject, currency, amount,
                     refund_amount, balance_amount, reason, idempotency_key, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                vec![
                    note_id.clone().into(),
                    number.into(),
                    id.into(),
                    inv.customer_id.clone().into(),
                    inv.subject.clone().into(),
                    inv.currency.clone().into(),
                    amount.into(),
                    refund_amount.into(),
                    balance_amount.into(),
                    reason.clone().into(),
                    idem.clone().into(),
                    now.into(),
                ],
            )
            .await?;
            t.exec(
                "UPDATE _forgebase_billing_invoices SET amount_refunded = amount_refunded + ?2, updated_at = ?3 WHERE id = ?1",
                vec![id.into(), amount.into(), now.into()],
            )
            .await?;
            if balance_amount > 0 {
                t.exec(
                    "UPDATE _forgebase_billing_customers SET balance = balance + ?2, updated_at = ?3 WHERE id = ?1",
                    vec![inv.customer_id.clone().into(), balance_amount.into(), now.into()],
                )
                .await?;
            }
            let mut refund = None;
            if refund_amount > 0
                && let Some(p) = pay.rows.first()
            {
                let rid = new_id("re");
                t.exec(
                    "INSERT INTO _forgebase_billing_refunds (id, payment_id, invoice_id, credit_note_id, provider, status,
                         amount, currency, reason, idempotency_key, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6, ?7, ?8, ?9, ?10, ?10)",
                    vec![
                        rid.clone().into(),
                        text(p, 0).into(),
                        id.into(),
                        note_id.clone().into(),
                        text(p, 1).into(),
                        refund_amount.into(),
                        inv.currency.clone().into(),
                        reason.clone().into(),
                        format!("refund:{note_id}").into(),
                        now.into(),
                    ],
                )
                .await?;
                t.exec(
                    "UPDATE _forgebase_billing_payments SET amount_refunded = amount_refunded + ?2, updated_at = ?3 WHERE id = ?1",
                    vec![text(p, 0).into(), refund_amount.into(), now.into()],
                )
                .await?;
                refund = Some((rid, text(p, 1), opt_text(p, 2)));
            }
            if inv.amount_refunded + amount >= inv.amount_paid + inv.balance_applied
                && let Some(o) = &inv.order_id
            {
                t.exec(
                    "UPDATE _forgebase_billing_orders SET status = 'refunded', updated_at = ?2 WHERE id = ?1",
                    vec![o.clone().into(), now.into()],
                )
                .await?;
            }
            t.audit(
                &ctx.principal_key(),
                "credit_note.issue",
                "invoice",
                id,
                json!({"credit_note": note_id, "amount": amount, "refund": refund_amount}),
                now,
            )
            .await?;
            Ok(((note_id, inv, amount, refund), true))
        }
        .await;
        let (note_id, inv, amount, refund) = self.end(tx, res).await?;
        // The provider refund, outside the transaction.
        if let Some((rid, provider, Some(payment_ref))) = refund {
            let result = match self.provider(&provider) {
                Ok(p) => {
                    p.refund(&RefundRequest {
                        payment_ref,
                        amount: self.refund_amount(&rid).await?,
                        currency: inv.currency.clone(),
                        idempotency_key: format!("refund:{note_id}"),
                        reason: reason.clone(),
                    })
                    .await
                }
                Err(e) => Err(e),
            };
            let (status, pref) = match result {
                Ok(r) => (r.status, r.provider_ref),
                Err(e) => {
                    tracing::warn!(credit_note = %note_id, error = %e, "refund failed at the provider");
                    (RefundStatus::Failed, None)
                }
            };
            self.set_refund(&rid, status, pref, None).await?;
        }
        // Credits bought on the invoice are taken back in proportion.
        let base = inv.amount_paid + inv.balance_applied;
        let mut reversed = 0;
        for l in &inv.lines {
            let Some(pk) = &l.product else { continue };
            let Ok(product) = self.product_by("key", pk).await else { continue };
            if product.kind != ProductKind::Credits {
                continue;
            }
            let bought = product.credits.unwrap_or_default().saturating_mul(l.quantity.max(1));
            let take = if amount >= base { bought } else { mul_div(bought, amount, base.max(1)) };
            if take <= 0 {
                continue;
            }
            match self
                .inner
                .credits
                .reverse_purchase(PurchaseReversal {
                    subject: inv.subject.clone(),
                    purchase_key: format!("billing:{}:{}", inv.id, l.id),
                    amount: Some(take),
                    idempotency_key: format!("billing:{note_id}:{}", l.id),
                    reason: Some("refund".into()),
                    external_ref: Some(note_id.clone()),
                })
                .await
            {
                Ok(r) => reversed += r.reversed,
                Err(e) => tracing::warn!(credit_note = %note_id, error = %e, "credit reversal failed"),
            }
        }
        let mut tx = self.begin().await?;
        let res = async {
            let mut t = Tx(tx.as_mut());
            t.exec(
                "UPDATE _forgebase_billing_credit_notes SET credits_reversed = ?2 WHERE id = ?1",
                vec![note_id.clone().into(), reversed.into()],
            )
            .await?;
            Ok(((), true))
        }
        .await;
        self.end(tx, res).await?;
        let rows = self
            .fetch(
                &format!("SELECT {NOTE_COLUMNS} FROM _forgebase_billing_credit_notes WHERE id = ?1"),
                vec![note_id.into()],
            )
            .await?;
        let n = rows
            .rows
            .first()
            .map(|r| note_row(r))
            .ok_or_else(|| BillingError::Internal("credit note not stored".into()))?;
        self.with_refund(n).await
    }

    async fn refund_amount(&self, id: &str) -> Result<i64> {
        let rows = self
            .fetch("SELECT amount FROM _forgebase_billing_refunds WHERE id = ?1", vec![id.into()])
            .await?;
        Ok(rows.rows.first().map(|r| int(r, 0)).unwrap_or_default())
    }

    /// Sets a refund's state; a failed refund gives its amount back to the
    /// payment (it can be refunded again).
    pub(crate) async fn set_refund(
        &self,
        id: &str,
        status: RefundStatus,
        provider_ref: Option<String>,
        event_at: Option<i64>,
    ) -> Result<bool> {
        let now = self.now();
        let mut tx = self.begin().await?;
        let res = async {
            let mut t = Tx(tx.as_mut());
            let changed = self.set_refund_tx(&mut t, id, status, provider_ref, event_at, now).await?;
            Ok((changed, true))
        }
        .await;
        self.end(tx, res).await
    }

    pub(crate) async fn set_refund_tx(
        &self,
        t: &mut Tx<'_>,
        id: &str,
        status: RefundStatus,
        provider_ref: Option<String>,
        event_at: Option<i64>,
        now: i64,
    ) -> Result<bool> {
        let rows = t
            .fetch(
                "SELECT status, payment_id, amount, provider_event_at FROM _forgebase_billing_refunds WHERE id = ?1",
                vec![id.into()],
            )
            .await?;
        let Some(r) = rows.rows.first() else {
            return Err(BillingError::NotFound("refund".into()));
        };
        let (old, payment, amount, last) = (text(r, 0), text(r, 1), int(r, 2), opt_int(r, 3));
        if old != "pending" || event_at.zip(last).is_some_and(|(e, l)| e < l) {
            return Ok(false);
        }
        t.exec(
            "UPDATE _forgebase_billing_refunds SET status = ?2, provider_ref = COALESCE(?3, provider_ref),
                 provider_event_at = COALESCE(?4, provider_event_at), updated_at = ?5 WHERE id = ?1",
            vec![id.into(), status.as_str().into(), provider_ref.into(), event_at.into(), now.into()],
        )
        .await?;
        if status == RefundStatus::Failed {
            // The provider refused: the payment can be refunded again, and the
            // credit note settles to the customer balance instead.
            t.exec(
                "UPDATE _forgebase_billing_payments SET amount_refunded = amount_refunded - ?2, updated_at = ?3 WHERE id = ?1",
                vec![payment.into(), amount.into(), now.into()],
            )
            .await?;
            t.exec(
                "UPDATE _forgebase_billing_credit_notes SET refund_amount = refund_amount - ?2, balance_amount = balance_amount + ?2
                 WHERE id = (SELECT credit_note_id FROM _forgebase_billing_refunds WHERE id = ?1)",
                vec![id.into(), amount.into()],
            )
            .await?;
            t.exec(
                "UPDATE _forgebase_billing_customers SET balance = balance + ?2, updated_at = ?3
                 WHERE id = (SELECT i.customer_id FROM _forgebase_billing_refunds r
                             JOIN _forgebase_billing_invoices i ON i.id = r.invoice_id WHERE r.id = ?1)",
                vec![id.into(), amount.into(), now.into()],
            )
            .await?;
            t.audit("system", "refund.failed", "refund", id, json!({"amount": amount, "to_balance": true}), now)
                .await?;
        }
        Ok(true)
    }
}
