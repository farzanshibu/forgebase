//! Statistics: balance history, consumption by feature and period, totals
//! by kind. Hours and days are fixed UTC buckets grouped in SQL; months are
//! calendar months folded from day buckets.

use std::collections::BTreeMap;

use forgebase_data::AccessContext;
use forgebase_db::{Statement, Value};
use time::OffsetDateTime;

use crate::error::{CreditError, Result};
use crate::model::{BalanceHistory, BalancePoint, ConsumptionRow, ConsumptionStats, CreditSummary, Interval};
use crate::service::{CreditService, int, text};

const HOUR_MS: i64 = 3_600_000;
const DAY_MS: i64 = 86_400_000;
/// Most buckets one query returns.
pub const MAX_BUCKETS: i64 = 1_000;

/// A statistics window: `[from, to)`, bucketed by `interval`.
#[derive(Debug, Clone, Copy)]
pub struct Window {
    /// Start (inclusive, epoch ms).
    pub from: i64,
    /// End (exclusive, epoch ms).
    pub to: i64,
    /// Bucket width.
    pub interval: Interval,
}

impl Window {
    /// The last 30 days by day, up to and including now.
    pub fn last_30_days(now: i64) -> Self {
        Self {
            from: now + 1 - 30 * DAY_MS,
            to: now + 1,
            interval: Interval::Day,
        }
    }
}

fn month_start(ms: i64) -> i64 {
    let t = OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000).unwrap_or(OffsetDateTime::UNIX_EPOCH);
    let first = t.date().replace_day(1).unwrap_or(t.date());
    i64::try_from(first.midnight().assume_utc().unix_timestamp_nanos() / 1_000_000).unwrap_or(0)
}

fn next_month(ms: i64) -> i64 {
    month_start(month_start(ms) + 32 * DAY_MS)
}

/// The start of the bucket holding `ms`.
pub fn bucket_start(ms: i64, interval: Interval) -> i64 {
    match interval {
        Interval::Hour => ms.div_euclid(HOUR_MS) * HOUR_MS,
        Interval::Day => ms.div_euclid(DAY_MS) * DAY_MS,
        Interval::Month => month_start(ms),
    }
}

fn next_bucket(start: i64, interval: Interval) -> i64 {
    match interval {
        Interval::Hour => start + HOUR_MS,
        Interval::Day => start + DAY_MS,
        Interval::Month => next_month(start),
    }
}

/// SQL bucket width (months group by day, then fold).
fn sql_width(interval: Interval) -> i64 {
    match interval {
        Interval::Hour => HOUR_MS,
        Interval::Day | Interval::Month => DAY_MS,
    }
}

fn check_window(w: Window) -> Result<Vec<i64>> {
    if w.to <= w.from {
        return Err(CreditError::invalid("invalid_window", "to must be after from"));
    }
    let mut starts = Vec::new();
    let mut b = bucket_start(w.from, w.interval);
    while b < w.to {
        starts.push(b);
        if starts.len() as i64 > MAX_BUCKETS {
            return Err(CreditError::invalid(
                "invalid_window",
                format!("at most {MAX_BUCKETS} buckets; use a wider interval or a shorter window"),
            ));
        }
        b = next_bucket(b, w.interval);
    }
    Ok(starts)
}

impl CreditService {
    fn stats_subject(&self, ctx: &AccessContext, subject: Option<&str>, all: bool) -> Result<Option<String>> {
        match subject {
            None if all => {
                self.require_admin(ctx)?;
                Ok(None)
            }
            s => {
                let s = self.subject_for(ctx, s)?;
                self.can_read(ctx, &s)?;
                Ok(Some(s))
            }
        }
    }

    /// The balance of a subject over a window: the opening balance and, per
    /// bucket, credits added, credits removed and the closing balance.
    pub async fn balance_history(
        &self,
        ctx: &AccessContext,
        subject: Option<&str>,
        w: Window,
    ) -> Result<BalanceHistory> {
        let subject = self.subject_for(ctx, subject)?;
        self.can_read(ctx, &subject)?;
        let starts = check_window(w)?;
        let opening = self
            .db()
            .fetch_all(&Statement::new(
                "SELECT balance_after FROM _forgebase_credit_ledger WHERE subject = ?1 AND at < ?2
                 ORDER BY seq DESC LIMIT 1",
                vec![subject.clone().into(), w.from.into()],
            ))
            .await?
            .rows
            .first()
            .map_or(0, |r| int(r, 0));
        let width = sql_width(w.interval);
        let rows = self
            .db()
            .fetch_all(&Statement::new(
                "SELECT g.b, g.credited, g.debited, l.balance_after FROM (
                     SELECT (at / ?1) * ?1 AS b,
                            SUM(CASE WHEN amount > 0 THEN amount ELSE 0 END) AS credited,
                            SUM(CASE WHEN amount < 0 THEN -amount ELSE 0 END) AS debited,
                            MAX(seq) AS last
                     FROM _forgebase_credit_ledger WHERE subject = ?2 AND at >= ?3 AND at < ?4
                     GROUP BY b) g
                 JOIN _forgebase_credit_ledger l ON l.seq = g.last
                 ORDER BY g.b",
                vec![width.into(), subject.clone().into(), w.from.into(), w.to.into()],
            ))
            .await?;
        // Fold SQL buckets into the requested ones (months from days).
        let mut by: BTreeMap<i64, (i64, i64, i64)> = BTreeMap::new();
        for r in &rows.rows {
            let b = bucket_start(int(r, 0), w.interval);
            let e = by.entry(b).or_insert((0, 0, 0));
            e.0 += int(r, 1);
            e.1 += int(r, 2);
            e.2 = int(r, 3);
        }
        let mut balance = opening;
        let points = starts
            .into_iter()
            .map(|start| {
                let (credited, debited) = match by.get(&start) {
                    Some(&(c, d, closing)) => {
                        balance = closing;
                        (c, d)
                    }
                    None => (0, 0),
                };
                BalancePoint {
                    start,
                    closing: balance,
                    credited,
                    debited,
                }
            })
            .collect();
        Ok(BalanceHistory {
            subject,
            interval: w.interval,
            from: w.from,
            to: w.to,
            opening,
            points,
        })
    }

    /// Credits consumed (usage charges and spends, net of refunds) by feature
    /// and bucket: of a subject, or of the whole instance (`all`, platform
    /// administrators).
    pub async fn consumption(
        &self,
        ctx: &AccessContext,
        subject: Option<&str>,
        all: bool,
        w: Window,
    ) -> Result<ConsumptionStats> {
        let subject = self.stats_subject(ctx, subject, all)?;
        check_window(w)?;
        let rows = self
            .db()
            .fetch_all(&Statement::new(
                "SELECT (at / ?1) * ?1 AS b, COALESCE(feature_key, ''), kind, SUM(amount), SUM(COALESCE(units, 0)), COUNT(*)
                 FROM _forgebase_credit_ledger
                 WHERE kind IN ('consumption', 'spend', 'refund') AND (CAST(?2 AS TEXT) IS NULL OR subject = ?2)
                   AND at >= ?3 AND at < ?4
                 GROUP BY b, 2, kind ORDER BY b",
                vec![
                    sql_width(w.interval).into(),
                    subject.clone().into(),
                    w.from.into(),
                    w.to.into(),
                ],
            ))
            .await?;
        let mut by: BTreeMap<(i64, String), ConsumptionRow> = BTreeMap::new();
        for r in &rows.rows {
            let start = bucket_start(int(r, 0), w.interval);
            let feature = text(r, 1);
            let row = by.entry((start, feature.clone())).or_insert(ConsumptionRow {
                start,
                feature,
                debited: 0,
                refunded: 0,
                net: 0,
                units: 0,
                entries: 0,
            });
            if text(r, 2) == "refund" {
                row.refunded += int(r, 3);
            } else {
                row.debited -= int(r, 3);
                row.units += int(r, 4);
                row.entries += int(r, 5);
            }
            row.net = row.debited - row.refunded;
        }
        let rows: Vec<ConsumptionRow> = by.into_values().collect();
        Ok(ConsumptionStats {
            total_net: rows.iter().map(|r| r.net).sum(),
            subject,
            interval: w.interval,
            from: w.from,
            to: w.to,
            rows,
        })
    }

    /// Credits by entry kind over `[from, to)`, the balance now and the
    /// usage shortfall: of a subject, or of the whole instance (`all`,
    /// platform administrators).
    pub async fn summary(
        &self,
        ctx: &AccessContext,
        subject: Option<&str>,
        all: bool,
        from: i64,
        to: i64,
    ) -> Result<CreditSummary> {
        let subject = self.stats_subject(ctx, subject, all)?;
        if to <= from {
            return Err(CreditError::invalid("invalid_window", "to must be after from"));
        }
        let s: Value = subject.clone().into();
        let rows = self
            .db()
            .fetch_all(&Statement::new(
                "SELECT kind, SUM(amount) FROM _forgebase_credit_ledger
                 WHERE (CAST(?1 AS TEXT) IS NULL OR subject = ?1) AND at >= ?2 AND at < ?3 GROUP BY kind",
                vec![s.clone(), from.into(), to.into()],
            ))
            .await?;
        let mut sum = CreditSummary {
            subject,
            from,
            to,
            top_up: 0,
            grant: 0,
            consumption: 0,
            spend: 0,
            refund: 0,
            reversal: 0,
            expiry: 0,
            net: 0,
            balance: 0,
            shortfall: 0,
        };
        for r in &rows.rows {
            let v = int(r, 1);
            sum.net += v;
            match text(r, 0).as_str() {
                "top_up" => sum.top_up = v,
                "grant" => sum.grant = v,
                "consumption" => sum.consumption = -v,
                "spend" => sum.spend = -v,
                "refund" => sum.refund = v,
                "reversal" => sum.reversal = -v,
                "expiry" => sum.expiry = -v,
                _ => {}
            }
        }
        let one = |sql: &str, params: Vec<Value>| Statement::new(sql, params);
        sum.balance = self
            .db()
            .fetch_all(&one(
                "SELECT COALESCE(SUM(balance), 0) FROM _forgebase_credit_accounts WHERE (CAST(?1 AS TEXT) IS NULL OR subject = ?1)",
                vec![s.clone()],
            ))
            .await?
            .rows
            .first()
            .map_or(0, |r| int(r, 0));
        sum.shortfall = self
            .db()
            .fetch_all(&one(
                "SELECT COALESCE(SUM(shortfall), 0) FROM _forgebase_credit_usage
                 WHERE (CAST(?1 AS TEXT) IS NULL OR subject = ?1) AND event_at >= ?2 AND event_at < ?3",
                vec![s, from.into(), to.into()],
            ))
            .await?
            .rows
            .first()
            .map_or(0, |r| int(r, 0));
        Ok(sum)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 2026-11-09 12:00 UTC.
    const T0: i64 = 1_794_225_600_000;

    #[test]
    fn buckets() {
        assert_eq!(bucket_start(T0 + 59 * 60_000, Interval::Hour), T0);
        assert_eq!(bucket_start(T0, Interval::Day), T0 - 12 * HOUR_MS);
        // 2026-11-01 00:00 UTC.
        assert_eq!(bucket_start(T0, Interval::Month), 1_793_491_200_000);
        // 2026-12-01 00:00 UTC.
        assert_eq!(next_bucket(1_793_491_200_000, Interval::Month), 1_796_083_200_000);
        let w = Window {
            from: T0,
            to: T0 + 3 * DAY_MS,
            interval: Interval::Day,
        };
        assert_eq!(check_window(w).unwrap().len(), 4);
        assert!(
            check_window(Window {
                from: T0,
                to: T0 + 2000 * HOUR_MS,
                interval: Interval::Hour
            })
            .is_err()
        );
    }
}
