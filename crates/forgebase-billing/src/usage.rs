//! Metered billing from FB-24 usage events.
//!
//! The consumer reads usage events after its cursor (`seq` order, batches of
//! 200), and for every event of a subject with an `active` or `past_due`
//! subscription holding a metered price of the event's feature stores one
//! usage record (`units`, or only `overage_units` for basis `overage`),
//! **idempotent by event id**. The cursor moves in the same transaction.
//! Renewal invoices bill the unbilled records of the period that ended
//! through the price's scheme (per unit, graduated or volume tiers).
//! Gauge events are levels, not consumption, and are skipped.

use std::collections::HashMap;

use forgebase_data::AccessContext;
use forgebase_db::Value;

use crate::billing::{LineDraft, SubRow};
use crate::error::Result;
use crate::model::{LineKind, MeterBasis, Price, PriceType};
use crate::service::{BillingService, Tx, USAGE_CURSOR, int, text};

const BATCH: u32 = 200;
const MAX_BATCHES: usize = 50;

/// A metered price on a live subscription.
#[derive(Debug, Clone)]
struct Meter {
    subscription_id: String,
    price_id: String,
    basis: MeterBasis,
}

impl BillingService {
    async fn read_cursor(&self) -> Result<i64> {
        let rows = self
            .fetch(
                "SELECT seq FROM _forgebase_billing_cursors WHERE name = ?1",
                vec![USAGE_CURSOR.into()],
            )
            .await?;
        Ok(rows.rows.first().map(|r| int(r, 0)).unwrap_or_default())
    }

    /// (subject, feature) → meters of live subscriptions.
    async fn meters(&self) -> Result<HashMap<(String, String), Vec<Meter>>> {
        let rows = self
            .fetch(
                "SELECT s.subject, p.feature_key, s.id, p.id, p.meter_basis
                 FROM _forgebase_billing_subscriptions s
                 JOIN _forgebase_billing_subscription_items i ON i.subscription_id = s.id
                 JOIN _forgebase_billing_prices p ON p.id = i.price_id
                 WHERE p.type = 'metered' AND s.status IN ('active', 'past_due')",
                Vec::new(),
            )
            .await?;
        let mut map: HashMap<(String, String), Vec<Meter>> = HashMap::new();
        for r in &rows.rows {
            map.entry((text(r, 0), text(r, 1))).or_default().push(Meter {
                subscription_id: text(r, 2),
                price_id: text(r, 3),
                basis: MeterBasis::parse(&text(r, 4)).unwrap_or(MeterBasis::Units),
            });
        }
        Ok(map)
    }

    /// Stores usage records for new FB-24 usage events. Safe to run
    /// concurrently and to repeat. Returns (events read, records stored).
    pub async fn ingest_usage(&self) -> Result<(i64, i64)> {
        let mut cursor = self.read_cursor().await?;
        let meters = self.meters().await?;
        let (mut read, mut stored) = (0, 0);
        for _ in 0..MAX_BATCHES {
            let page = self
                .inner
                .ent
                .events(&AccessContext::system(), None, None, Some(cursor), Some(BATCH))
                .await?;
            let Some(last) = page.items.last().map(|e| e.seq) else {
                break;
            };
            let now = self.now();
            let mut tx = self.begin().await?;
            let res = async {
                let mut t = Tx(tx.as_mut());
                let mut n = 0;
                for ev in &page.items {
                    if ev.source == "gauge" {
                        continue;
                    }
                    let Some(ms) = meters.get(&(ev.subject.clone(), ev.feature.clone())) else {
                        continue;
                    };
                    for m in ms {
                        let q = match m.basis {
                            MeterBasis::Units => ev.units,
                            MeterBasis::Overage => ev.overage_units,
                        };
                        if q <= 0 {
                            continue;
                        }
                        n += t
                            .exec(
                                "INSERT INTO _forgebase_billing_usage_records (event_id, seq, subscription_id, price_id,
                                     subject, feature_key, quantity, at)
                                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) ON CONFLICT (event_id) DO NOTHING",
                                vec![
                                    format!("{}:{}", ev.id, m.price_id).into(),
                                    ev.seq.into(),
                                    m.subscription_id.clone().into(),
                                    m.price_id.clone().into(),
                                    ev.subject.clone().into(),
                                    ev.feature.clone().into(),
                                    q.into(),
                                    ev.at.into(),
                                ],
                            )
                            .await?;
                    }
                }
                t.exec(
                    "INSERT INTO _forgebase_billing_cursors (name, seq, updated_at) VALUES (?1, ?2, ?3)
                     ON CONFLICT (name) DO UPDATE SET seq = CASE WHEN excluded.seq > _forgebase_billing_cursors.seq THEN excluded.seq ELSE _forgebase_billing_cursors.seq END, updated_at = excluded.updated_at",
                    vec![USAGE_CURSOR.into(), last.into(), now.into()],
                )
                .await?;
                Ok((n, true))
            }
            .await;
            let n = self.end(tx, res).await?;
            read += i64::try_from(page.items.len()).unwrap_or_default();
            stored += i64::try_from(n).unwrap_or_default();
            cursor = last;
            if page.next.is_none() {
                break;
            }
        }
        Ok((read, stored))
    }

    /// Usage lines of a subscription for usage before `until`, and marks the
    /// records billed by `invoice_id` (inside the invoice's transaction).
    pub(crate) async fn usage_lines(
        &self,
        t: &mut Tx<'_>,
        sub: &SubRow,
        prices: &[(Price, i64)],
        period: (i64, i64),
    ) -> Result<Vec<(LineDraft, String)>> {
        let mut out = Vec::new();
        for (p, _) in prices.iter().filter(|(p, _)| p.price_type == PriceType::Metered) {
            let q = t
                .fetch(
                    "SELECT COALESCE(SUM(quantity), 0) FROM _forgebase_billing_usage_records
                     WHERE subscription_id = ?1 AND price_id = ?2 AND invoice_id IS NULL AND at < ?3",
                    vec![sub.sub.id.clone().into(), p.id.clone().into(), period.1.into()],
                )
                .await?
                .scalar()
                .and_then(Value::as_i64)
                .unwrap_or_default();
            if q <= 0 {
                continue;
            }
            let line = LineDraft::for_price(LineKind::Usage, p, &sub.sub.currency, q, Some(period))?;
            out.push((line, p.id.clone()));
        }
        Ok(out)
    }

    /// Marks the usage records billed (after the invoice id is known).
    pub(crate) async fn mark_usage_billed(
        &self,
        t: &mut Tx<'_>,
        sub_id: &str,
        price_ids: &[String],
        until: i64,
        invoice_id: &str,
    ) -> Result<()> {
        for p in price_ids {
            t.exec(
                "UPDATE _forgebase_billing_usage_records SET invoice_id = ?4
                 WHERE subscription_id = ?1 AND price_id = ?2 AND invoice_id IS NULL AND at < ?3",
                vec![sub_id.into(), p.clone().into(), until.into(), invoice_id.into()],
            )
            .await?;
        }
        Ok(())
    }
}
