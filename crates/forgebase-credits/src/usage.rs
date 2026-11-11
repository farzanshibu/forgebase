//! Credits paying for usage: the consumer of FB-24's usage events.
//!
//! The consumer reads `_forgebase_usage_events` through
//! [`EntitlementService::events`] after its cursor (`seq`), in batches. For
//! each event of a priced feature (see [`crate::Rate`]) it debits
//! `ceil(units × credits_per_unit / per_units)` credits, or what the balance
//! holds when that is less (the balance never goes negative; the rest is the
//! charge's `shortfall`), and records the charge keyed by the event id. A
//! batch's charges and the cursor move in one transaction; an event already
//! charged is skipped, so resetting the cursor replays nothing twice.
//! Gauge events (levels, not consumption) are not charged.
//!
//! [`EntitlementService::events`]: forgebase_entitlements::EntitlementService::events

use std::collections::HashMap;

use forgebase_data::AccessContext;
use forgebase_db::{Statement, TxMode};
use forgebase_entitlements::UsageEvent;
use serde_json::json;

use crate::error::{CreditError, Result};
use crate::model::{Cursor, EntryKind, RateBasis, UsageCharge, UsageChargePage, UsageRun};
use crate::service::{
    CreditService, Fields, Meta, Tx, USAGE_CURSOR, debit_tx, int, opt_text, stored_balance, sweep_tx, text,
};

/// Events per transaction.
const BATCH: u32 = 200;
/// Batches per run (the next run continues).
const MAX_BATCHES: usize = 50;

#[derive(Debug, Clone, Copy)]
struct Price {
    credits_per_unit: i64,
    per_units: i64,
    basis: RateBasis,
}

/// `ceil(units × credits_per_unit / per_units)`, saturating.
fn cost(units: i64, p: Price) -> i64 {
    let n = i128::from(units) * i128::from(p.credits_per_unit);
    let d = i128::from(p.per_units);
    i64::try_from((n + d - 1) / d).unwrap_or(i64::MAX)
}

impl CreditService {
    async fn read_cursor(&self) -> Result<i64> {
        let rows = self
            .db()
            .fetch_all(&Statement::new(
                "SELECT seq FROM _forgebase_credit_cursors WHERE name = ?1",
                vec![USAGE_CURSOR.into()],
            ))
            .await?;
        Ok(rows.rows.first().map_or(0, |r| int(r, 0)))
    }

    async fn prices(&self) -> Result<HashMap<String, Price>> {
        let rows = self
            .db()
            .fetch_all(&Statement::raw(
                "SELECT feature_key, credits_per_unit, per_units, basis FROM _forgebase_credit_rates",
            ))
            .await?;
        Ok(rows
            .rows
            .iter()
            .map(|r| {
                (
                    text(r, 0),
                    Price {
                        credits_per_unit: int(r, 1),
                        per_units: int(r, 2).max(1),
                        basis: if text(r, 3) == "overage" {
                            RateBasis::Overage
                        } else {
                            RateBasis::Units
                        },
                    },
                )
            })
            .collect())
    }

    /// Charges new usage events of priced features (the maintenance job runs
    /// it every minute). Safe to run concurrently and to repeat.
    pub async fn consume_usage(&self) -> Result<UsageRun> {
        let _guard = self.inner.consumer.lock().await;
        let mut run = UsageRun {
            cursor: self.read_cursor().await?,
            ..UsageRun::default()
        };
        for _ in 0..MAX_BATCHES {
            let page = self
                .inner
                .usage
                .events(&AccessContext::system(), None, None, Some(run.cursor), Some(BATCH))
                .await?;
            let Some(last) = page.items.last().map(|e| e.seq) else {
                break;
            };
            let prices = self.prices().await?;
            let now = self.now();
            let mut tx = self.db().begin(TxMode::Normal).await?;
            let res = async {
                let mut t = Tx(tx.as_mut());
                let mut batch = UsageRun::default();
                for ev in &page.items {
                    batch.events_read += 1;
                    if let Some((charged, shortfall)) = charge(&mut t, ev, &prices, now).await? {
                        batch.events_charged += 1;
                        batch.credits_charged += charged;
                        batch.shortfall += shortfall;
                    }
                }
                t.exec(
                    "INSERT INTO _forgebase_credit_cursors (name, seq, updated_at) VALUES (?1, ?2, ?3)
                     ON CONFLICT (name) DO UPDATE SET seq = CASE WHEN excluded.seq > _forgebase_credit_cursors.seq THEN excluded.seq ELSE _forgebase_credit_cursors.seq END, updated_at = excluded.updated_at",
                    vec![USAGE_CURSOR.into(), last.into(), now.into()],
                )
                .await?;
                Ok((batch, true))
            }
            .await;
            let batch = self.end(tx, res).await?;
            run.events_read += batch.events_read;
            run.events_charged += batch.events_charged;
            run.credits_charged += batch.credits_charged;
            run.shortfall += batch.shortfall;
            run.cursor = last;
            if page.next.is_none() {
                break;
            }
        }
        Ok(run)
    }

    /// The consumer cursor (platform administrators).
    pub async fn usage_cursor(&self, ctx: &AccessContext) -> Result<Cursor> {
        self.require_admin(ctx)?;
        Ok(Cursor {
            seq: self.read_cursor().await?,
        })
    }

    /// Moves the consumer cursor (platform administrators), e.g. back to
    /// re-scan events after a price was added. Events already charged are
    /// never charged again.
    pub async fn set_usage_cursor(&self, ctx: &AccessContext, cursor: Cursor) -> Result<Cursor> {
        self.require_admin(ctx)?;
        if cursor.seq < 0 {
            return Err(CreditError::invalid("invalid_cursor", "seq must not be negative"));
        }
        let _guard = self.inner.consumer.lock().await;
        let now = self.now();
        let mut tx = self.db().begin(TxMode::Normal).await?;
        let res = async {
            let mut t = Tx(tx.as_mut());
            t.exec(
                "INSERT INTO _forgebase_credit_cursors (name, seq, updated_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT (name) DO UPDATE SET seq = excluded.seq, updated_at = excluded.updated_at",
                vec![USAGE_CURSOR.into(), cursor.seq.into(), now.into()],
            )
            .await?;
            Ok((cursor, true))
        }
        .await;
        self.end(tx, res).await
    }

    /// Usage events charged in credits, in event order: of a subject
    /// (default: the caller's), or of every subject for privileged callers
    /// with `all`. `shortfall_only` lists the charges the balance did not
    /// cover (what FB-70 may bill).
    pub async fn usage_charges(
        &self,
        ctx: &AccessContext,
        subject: Option<&str>,
        all: bool,
        after: Option<i64>,
        limit: Option<u32>,
        shortfall_only: bool,
    ) -> Result<UsageChargePage> {
        let subject = match subject {
            None if all && ctx.is_privileged() => None,
            s => Some(self.subject_for(ctx, s)?),
        };
        if let Some(s) = &subject {
            self.can_read(ctx, s)?;
        }
        let limit = i64::from(limit.unwrap_or(100).clamp(1, 1000));
        let rows = self
            .db()
            .fetch_all(&Statement::new(
                "SELECT event_id, event_seq, event_at, at, subject, feature_key, units, rate, per_units, cost, charged,
                        shortfall, entry_id
                 FROM _forgebase_credit_usage
                 WHERE event_seq > ?1 AND (CAST(?2 AS TEXT) IS NULL OR subject = ?2) AND (CAST(?3 AS BIGINT) = 0 OR shortfall > 0)
                 ORDER BY event_seq LIMIT ?4",
                vec![
                    after.unwrap_or(0).into(),
                    subject.into(),
                    shortfall_only.into(),
                    (limit + 1).into(),
                ],
            ))
            .await?;
        let mut items: Vec<UsageCharge> = rows
            .rows
            .iter()
            .map(|r| UsageCharge {
                event_id: text(r, 0),
                event_seq: int(r, 1),
                event_at: int(r, 2),
                at: int(r, 3),
                subject: text(r, 4),
                feature: text(r, 5),
                units: int(r, 6),
                rate: int(r, 7),
                per_units: int(r, 8),
                cost: int(r, 9),
                charged: int(r, 10),
                shortfall: int(r, 11),
                entry_id: opt_text(r, 12),
            })
            .collect();
        let next = if items.len() as i64 > limit {
            items.truncate(limit as usize);
            items.last().map(|c| c.event_seq)
        } else {
            None
        };
        Ok(UsageChargePage { items, next })
    }
}

/// Charges one event; `None` when it is not priced or already charged.
async fn charge(
    t: &mut Tx<'_>,
    ev: &UsageEvent,
    prices: &HashMap<String, Price>,
    now: i64,
) -> Result<Option<(i64, i64)>> {
    if ev.source == "gauge" {
        return Ok(None);
    }
    let Some(price) = prices.get(&ev.feature).copied() else {
        return Ok(None);
    };
    let units = match price.basis {
        RateBasis::Units => ev.units,
        RateBasis::Overage => ev.overage_units,
    };
    if units <= 0 {
        return Ok(None);
    }
    let seen = t
        .fetch(
            "SELECT 1 FROM _forgebase_credit_usage WHERE event_id = ?1",
            vec![ev.id.clone().into()],
        )
        .await?;
    if !seen.is_empty() {
        return Ok(None);
    }
    let cost = cost(units, price);
    sweep_tx(t, &ev.subject, now).await?;
    let charged = cost.min(stored_balance(t, &ev.subject).await?);
    let entry_id = if charged > 0 {
        let meta = Meta {
            metadata: json!({"source": ev.source, "scope_key": ev.scope_key}),
            ..Meta::system(
                format!("usage:{}", ev.id),
                "usage",
                json!({"op": "consumption", "event_id": ev.id}),
            )
        };
        let e = debit_tx(
            t,
            &ev.subject,
            EntryKind::Consumption,
            charged,
            Fields {
                feature: Some(ev.feature.clone()),
                units: Some(units),
                usage_event_id: Some(ev.id.clone()),
                usage_seq: Some(ev.seq),
                ..Fields::default()
            },
            &meta,
            now,
        )
        .await?;
        Some(e.id)
    } else {
        None
    };
    t.exec(
        "INSERT INTO _forgebase_credit_usage (event_id, event_seq, event_at, at, subject, feature_key, units, rate,
             per_units, cost, charged, shortfall, entry_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        vec![
            ev.id.clone().into(),
            ev.seq.into(),
            ev.at.into(),
            now.into(),
            ev.subject.clone().into(),
            ev.feature.clone().into(),
            units.into(),
            price.credits_per_unit.into(),
            price.per_units.into(),
            cost.into(),
            charged.into(),
            (cost - charged).into(),
            entry_id.into(),
        ],
    )
    .await?;
    Ok(Some((charged, cost - charged)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_rounds_up_per_event() {
        let p = |c, n| Price {
            credits_per_unit: c,
            per_units: n,
            basis: RateBasis::Units,
        };
        assert_eq!(cost(3, p(2, 1)), 6);
        assert_eq!(cost(1500, p(1, 1000)), 2);
        assert_eq!(cost(1000, p(1, 1000)), 1);
        assert_eq!(cost(1, p(1, 1000)), 1);
        assert_eq!(cost(i64::MAX, p(i64::MAX, 1)), i64::MAX);
    }
}
