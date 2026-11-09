//! Payment statistics: MRR and ARR per currency (no exchange rates: each
//! currency is reported on its own), subscriptions per status, new and
//! churned subscriptions with the churn rate, revenue (payments less
//! refunds) per currency and day or month, failed payments.

use std::collections::{BTreeMap, HashMap};

use forgebase_data::AccessContext;
use forgebase_entitlements::period::{civil_from_days, days_from_civil};

use crate::error::{BillingError, Result};
use crate::model::{BillingStats, CouponKind, Interval, PriceType, RecurringRevenue, RevenuePoint};
use crate::money::mul_div;
use crate::service::{BillingService, DAY, int, text};

/// Revenue buckets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bucket {
    /// UTC days.
    Day,
    /// UTC calendar months.
    Month,
}

impl Bucket {
    /// Parses `day` or `month`.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "day" => Some(Bucket::Day),
            "month" => Some(Bucket::Month),
            _ => None,
        }
    }

    fn start(self, ms: i64) -> i64 {
        let day = ms.div_euclid(DAY);
        match self {
            Bucket::Day => day * DAY,
            Bucket::Month => {
                let (y, m, _) = civil_from_days(day);
                days_from_civil(y, m, 1) * DAY
            }
        }
    }
}

/// Per subscription: currency, MRR before coupons, months per period, the
/// coupon (kind, percent, amount off).
type SubMrr = (String, i64, i64, Option<(String, i64, i64)>);

impl BillingService {
    /// Statistics over `[from, to)` (realm-level `billing:read` or
    /// privileged).
    pub async fn stats(&self, ctx: &AccessContext, from: i64, to: i64, bucket: Bucket) -> Result<BillingStats> {
        self.can_read_all(ctx)?;
        if to <= from || to - from > 3 * 366 * DAY {
            return Err(BillingError::invalid(
                "invalid_window",
                "the window must be 1 ms to 3 years",
            ));
        }
        // MRR: active and past-due subscriptions, recurring prices, normalised
        // to a month, after percent coupons and amount coupons.
        let rows = self
            .fetch(
                "SELECT s.id, s.currency, p.key, i.quantity, c.kind, c.percent_bp, c.amount_off
                 FROM _forgebase_billing_subscriptions s
                 JOIN _forgebase_billing_subscription_items i ON i.subscription_id = s.id
                 JOIN _forgebase_billing_prices p ON p.id = i.price_id
                 LEFT JOIN _forgebase_billing_coupons c ON c.id = s.coupon_id
                 WHERE s.status IN ('active', 'past_due') AND p.type = 'recurring'
                 ORDER BY s.id",
                Vec::new(),
            )
            .await?;
        let mut per_sub: HashMap<String, SubMrr> = HashMap::new();
        for r in &rows.rows {
            let price = self.price_by("key", &text(r, 2)).await?;
            if price.price_type != PriceType::Recurring {
                continue;
            }
            let currency = text(r, 1);
            let amount = crate::billing::item_amount(&price, &currency, int(r, 3)).unwrap_or_default();
            let months = match price.interval.unwrap_or(Interval::Month) {
                Interval::Month => price.interval_count,
                Interval::Year => 12 * price.interval_count,
            }
            .max(1);
            let coupon = (!text(r, 4).is_empty()).then(|| (text(r, 4), int(r, 5), int(r, 6)));
            let e = per_sub
                .entry(text(r, 0))
                .or_insert_with(|| (currency.clone(), 0, months, coupon));
            e.1 += mul_div(amount, 1, months);
        }
        let mut recurring: BTreeMap<String, (i64, i64)> = BTreeMap::new();
        for (currency, mrr, months, coupon) in per_sub.into_values() {
            let mrr = match coupon {
                Some((kind, bp, _)) if CouponKind::parse(&kind) == Some(CouponKind::Percent) => {
                    mrr - mul_div(mrr, bp, 10_000)
                }
                Some((_, _, off)) => (mrr - mul_div(off, 1, months)).max(0),
                None => mrr,
            };
            let e = recurring.entry(currency).or_default();
            e.0 += mrr;
            e.1 += 1;
        }
        let recurring = recurring
            .into_iter()
            .map(|(currency, (mrr, n))| RecurringRevenue {
                currency,
                mrr,
                arr: mrr * 12,
                subscriptions: n,
            })
            .collect();
        let by_status = self
            .fetch(
                "SELECT status, COUNT(*) FROM _forgebase_billing_subscriptions GROUP BY status",
                Vec::new(),
            )
            .await?
            .rows
            .iter()
            .map(|r| (text(r, 0), int(r, 1)))
            .collect();
        // Subscriptions that never started (unpaid first invoice) are not churn.
        let counts = self
            .fetch(
                "SELECT
                    SUM(CASE WHEN created_at < ?1 AND (ended_at IS NULL OR ended_at >= ?1) AND status <> 'incomplete' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN created_at >= ?1 AND created_at < ?2 THEN 1 ELSE 0 END),
                    SUM(CASE WHEN ended_at >= ?1 AND ended_at < ?2 THEN 1 ELSE 0 END)
                 FROM _forgebase_billing_subscriptions
                 WHERE COALESCE(cancel_reason, '') NOT IN ('incomplete_expired', 'invoice_voided')",
                vec![from.into(), to.into()],
            )
            .await?;
        let c = counts.rows.first().cloned().unwrap_or_default();
        let (active_at_start, new_subscriptions, churned) = (int(&c, 0), int(&c, 1), int(&c, 2));
        let churn_rate_bp = (active_at_start > 0).then(|| mul_div(churned, 10_000, active_at_start));
        // Revenue.
        let mut revenue: BTreeMap<(i64, String), RevenuePoint> = BTreeMap::new();
        let pays = self
            .fetch(
                "SELECT updated_at, currency, amount FROM _forgebase_billing_payments
                 WHERE status = 'succeeded' AND updated_at >= ?1 AND updated_at < ?2",
                vec![from.into(), to.into()],
            )
            .await?;
        for r in &pays.rows {
            let k = (bucket.start(int(r, 0)), text(r, 1));
            let e = revenue.entry(k.clone()).or_insert_with(|| RevenuePoint {
                start: k.0,
                currency: k.1.clone(),
                gross: 0,
                refunded: 0,
                net: 0,
                payments: 0,
            });
            e.gross += int(r, 2);
            e.payments += 1;
        }
        let refunds = self
            .fetch(
                "SELECT updated_at, currency, amount FROM _forgebase_billing_refunds
                 WHERE status = 'succeeded' AND updated_at >= ?1 AND updated_at < ?2",
                vec![from.into(), to.into()],
            )
            .await?;
        for r in &refunds.rows {
            let k = (bucket.start(int(r, 0)), text(r, 1));
            let e = revenue.entry(k.clone()).or_insert_with(|| RevenuePoint {
                start: k.0,
                currency: k.1.clone(),
                gross: 0,
                refunded: 0,
                net: 0,
                payments: 0,
            });
            e.refunded += int(r, 2);
        }
        let revenue = revenue
            .into_values()
            .map(|mut p| {
                p.net = p.gross - p.refunded;
                p
            })
            .collect();
        let failed_payments = self
            .fetch(
                "SELECT COUNT(*) FROM _forgebase_billing_payments WHERE status = 'failed' AND updated_at >= ?1 AND updated_at < ?2",
                vec![from.into(), to.into()],
            )
            .await?
            .scalar()
            .and_then(forgebase_db::Value::as_i64)
            .unwrap_or_default();
        Ok(BillingStats {
            from,
            to,
            recurring,
            by_status,
            active_at_start,
            new_subscriptions,
            churned,
            churn_rate_bp,
            revenue,
            failed_payments,
        })
    }
}
