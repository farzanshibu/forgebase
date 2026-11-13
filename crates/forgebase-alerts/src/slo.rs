//! SLOs (ADR-0035 §5): the HTTP observer that counts good and bad requests
//! per SLO and minute, the flush into `_forgebase_slo_minutes`, and the
//! error-budget and burn-rate arithmetic.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, RwLock};

use forgebase_db::{Backend, Statement, Value};
use forgebase_jobs::Clock;
use regex::Regex;

use crate::error::Result;
use crate::model::{ErrorBudget, Slo, SloKind};
use crate::rules::BURN_WINDOWS;

const MINUTE: i64 = 60_000;

/// An SLO ready to match routes.
#[derive(Debug)]
struct Compiled {
    id: String,
    kind: SloKind,
    prefix: Option<String>,
    regex: Option<Regex>,
    methods: Vec<String>,
    threshold_s: f64,
}

impl Compiled {
    fn matches_route(&self, route: &str) -> bool {
        if route == "<unmatched>" {
            return false;
        }
        if let Some(p) = &self.prefix
            && !route.starts_with(p.as_str())
        {
            return false;
        }
        if let Some(r) = &self.regex
            && !r.is_match(route)
        {
            return false;
        }
        true
    }
}

/// Counts requests per SLO and minute, in memory until [`SloCounter::flush`].
#[derive(Debug)]
pub struct SloCounter {
    clock: Arc<dyn Clock>,
    slos: RwLock<Vec<Compiled>>,
    /// Route template → indexes of the matching SLOs (route templates are few).
    cache: RwLock<HashMap<String, Arc<[usize]>>>,
    /// `(slo id, minute) → (total, good)`.
    counts: Mutex<HashMap<(String, i64), (i64, i64)>>,
}

impl SloCounter {
    /// An empty counter.
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            clock,
            slos: RwLock::new(Vec::new()),
            cache: RwLock::new(HashMap::new()),
            counts: Mutex::new(HashMap::new()),
        }
    }

    /// Replaces the SLOs counted.
    pub fn set_slos(&self, slos: &[Slo]) {
        let compiled = slos
            .iter()
            .map(|s| Compiled {
                id: s.id.clone(),
                kind: s.kind,
                prefix: s.route_prefix.clone(),
                regex: s
                    .route_regex
                    .as_deref()
                    .and_then(|r| Regex::new(&format!("^(?:{r})$")).ok()),
                methods: s.methods.clone(),
                threshold_s: s.latency_threshold_ms.unwrap_or(0) as f64 / 1000.0,
            })
            .collect();
        if let Ok(mut g) = self.slos.write() {
            *g = compiled;
        }
        if let Ok(mut c) = self.cache.write() {
            c.clear();
        }
    }

    fn matching(&self, route: &str) -> Arc<[usize]> {
        if let Some(hit) = self.cache.read().ok().and_then(|c| c.get(route).cloned()) {
            return hit;
        }
        let Ok(slos) = self.slos.read() else {
            return Arc::from(Vec::new());
        };
        let idx: Arc<[usize]> = slos
            .iter()
            .enumerate()
            .filter(|(_, s)| s.matches_route(route))
            .map(|(i, _)| i)
            .collect::<Vec<_>>()
            .into();
        drop(slos);
        if let Ok(mut c) = self.cache.write()
            && c.len() < 10_000
        {
            c.insert(route.to_owned(), idx.clone());
        }
        idx
    }

    /// Takes the counts gathered so far.
    fn take(&self) -> HashMap<(String, i64), (i64, i64)> {
        self.counts
            .lock()
            .map(|mut c| std::mem::take(&mut *c))
            .unwrap_or_default()
    }

    /// Counts not yet flushed for `slo_id` with minute `>= from`.
    pub fn pending(&self, slo_id: &str, from: i64) -> (i64, i64) {
        self.counts
            .lock()
            .map(|c| {
                c.iter()
                    .filter(|((id, m), _)| id == slo_id && *m >= from)
                    .fold((0, 0), |(t, g), (_, (a, b))| (t + a, g + b))
            })
            .unwrap_or((0, 0))
    }

    /// Adds the counts to `_forgebase_slo_minutes` (one transaction).
    pub async fn flush(&self, db: &dyn Backend) -> Result<usize> {
        let counts = self.take();
        if counts.is_empty() {
            return Ok(0);
        }
        let mut tx = match db.begin(forgebase_db::TxMode::Normal).await {
            Ok(tx) => tx,
            Err(e) => {
                self.restore(counts);
                return Err(e.into());
            }
        };
        let n = counts.len();
        for ((slo, minute), (total, good)) in &counts {
            let r = tx
                .execute(&Statement::new(
                    "INSERT INTO _forgebase_slo_minutes (slo_id, minute, total, good)
                     SELECT ?1, CAST(?2 AS BIGINT), CAST(?3 AS BIGINT), CAST(?4 AS BIGINT)
                     WHERE EXISTS (SELECT 1 FROM _forgebase_slos WHERE id = ?1)
                     ON CONFLICT (slo_id, minute) DO UPDATE SET
                         total = _forgebase_slo_minutes.total + excluded.total,
                         good = _forgebase_slo_minutes.good + excluded.good",
                    vec![slo.as_str().into(), (*minute).into(), (*total).into(), (*good).into()],
                ))
                .await;
            if let Err(e) = r {
                drop(tx);
                self.restore(counts);
                return Err(e.into());
            }
        }
        if let Err(e) = tx.commit().await {
            self.restore(counts);
            return Err(e.into());
        }
        Ok(n)
    }

    fn restore(&self, counts: HashMap<(String, i64), (i64, i64)>) {
        if let Ok(mut c) = self.counts.lock() {
            for (k, (t, g)) in counts {
                let e = c.entry(k).or_insert((0, 0));
                e.0 += t;
                e.1 += g;
            }
        }
    }
}

impl forgebase_observability::HttpObserver for SloCounter {
    fn observe(&self, method: &str, route: &str, status: u16, seconds: f64) {
        let idx = self.matching(route);
        if idx.is_empty() {
            return;
        }
        let Ok(slos) = self.slos.read() else { return };
        let minute = self.clock.now_ms().div_euclid(MINUTE) * MINUTE;
        let Ok(mut counts) = self.counts.lock() else { return };
        for i in idx.iter() {
            let Some(s) = slos.get(*i) else { continue };
            if !s.methods.is_empty() && !s.methods.iter().any(|m| m.eq_ignore_ascii_case(method)) {
                continue;
            }
            let good = match s.kind {
                SloKind::Availability => status < 500,
                // Latency counts the requests that did not fail.
                SloKind::Latency => {
                    if status >= 500 {
                        continue;
                    }
                    seconds <= s.threshold_s
                }
            };
            let e = counts.entry((s.id.clone(), minute)).or_insert((0, 0));
            e.0 += 1;
            if good {
                e.1 += 1;
            }
        }
    }
}

/// Totals of an SLO: over its window and over each burn window.
#[derive(Debug, Clone, PartialEq)]
pub struct Totals {
    /// `(total, good)` over the SLO window.
    pub window: (i64, i64),
    /// `(total, good)` per burn window name.
    pub burn: BTreeMap<String, (i64, i64)>,
}

fn int(v: &Value) -> i64 {
    match v {
        Value::Integer(i) => *i,
        Value::Real(f) => *f as i64,
        Value::Text(t) => t.parse().unwrap_or(0),
        _ => 0,
    }
}

/// Reads the totals of `slo` at `now` (flushed minutes plus `pending`).
pub async fn totals(db: &dyn Backend, counter: &SloCounter, slo: &Slo, now: i64) -> Result<Totals> {
    let start = |w: i64| (now - w).div_euclid(MINUTE) * MINUTE + MINUTE;
    let window_start = start(slo.window_ms);
    let mut sql =
        String::from("SELECT COALESCE(CAST(SUM(total) AS BIGINT), 0), COALESCE(CAST(SUM(good) AS BIGINT), 0)");
    let mut params: Vec<Value> = vec![slo.id.as_str().into(), window_start.into()];
    for (_, w) in BURN_WINDOWS {
        let n = params.len() + 1;
        sql.push_str(&format!(
            ", COALESCE(CAST(SUM(CASE WHEN minute >= ?{n} THEN total ELSE 0 END) AS BIGINT), 0), \
             COALESCE(CAST(SUM(CASE WHEN minute >= ?{n} THEN good ELSE 0 END) AS BIGINT), 0)"
        ));
        params.push(start(w).into());
    }
    let longest = BURN_WINDOWS
        .iter()
        .map(|(_, w)| *w)
        .max()
        .unwrap_or(0)
        .max(slo.window_ms);
    params.push(start(longest).into());
    let last = params.len();
    sql.push_str(&format!(
        " FROM _forgebase_slo_minutes WHERE slo_id = ?1 AND minute >= ?{last}"
    ));
    let rows = db.fetch_all(&Statement::new(sql, params)).await?;
    let row = rows.rows.first().cloned().unwrap_or_default();
    let get = |i: usize| row.get(i).map_or(0, int);
    let (pt, pg) = counter.pending(&slo.id, window_start);
    let window = (get(0) + pt, get(1) + pg);
    let mut burn = BTreeMap::new();
    for (i, (name, w)) in BURN_WINDOWS.iter().enumerate() {
        let (pt, pg) = counter.pending(&slo.id, start(*w));
        burn.insert((*name).to_owned(), (get(2 + 2 * i) + pt, get(3 + 2 * i) + pg));
    }
    Ok(Totals { window, burn })
}

/// Burn rate: the error ratio over the allowed error ratio (`None` without traffic).
pub fn burn_rate(objective: f64, (total, good): (i64, i64)) -> Option<f64> {
    if total <= 0 {
        return None;
    }
    let bad = (total - good).max(0) as f64;
    let allowed = 1.0 - objective;
    if allowed <= 0.0 {
        return None;
    }
    Some(bad / total as f64 / allowed)
}

/// The error budget over the window.
pub fn budget(objective: f64, (total, good): (i64, i64)) -> ErrorBudget {
    let bad = (total - good).max(0);
    let allowed = (1.0 - objective) * total as f64;
    let remaining_ratio = if total <= 0 {
        1.0
    } else if allowed <= 0.0 {
        if bad == 0 { 1.0 } else { f64::NEG_INFINITY }
    } else {
        1.0 - bad as f64 / allowed
    };
    ErrorBudget {
        allowed,
        consumed: bad,
        remaining_ratio,
    }
}

/// Deletes minutes older than each SLO's window.
pub async fn prune(db: &dyn Backend, slos: &[Slo], now: i64) -> Result<()> {
    let longest = BURN_WINDOWS.iter().map(|(_, w)| *w).max().unwrap_or(0);
    let stmts = slos
        .iter()
        .map(|s| {
            Statement::new(
                "DELETE FROM _forgebase_slo_minutes WHERE slo_id = ?1 AND minute < ?2",
                vec![s.id.as_str().into(), (now - s.window_ms.max(longest) - MINUTE).into()],
            )
        })
        .collect();
    crate::store::write(db, stmts).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_and_burn_rate() {
        // 99.9%: 1000 requests allow 1 bad one.
        let b = budget(0.999, (1000, 999));
        assert!((b.allowed - 1.0).abs() < 1e-9);
        assert_eq!(b.consumed, 1);
        assert!(b.remaining_ratio.abs() < 1e-9);
        let b = budget(0.99, (1000, 995));
        assert!((b.remaining_ratio - 0.5).abs() < 1e-9);
        assert_eq!(budget(0.99, (0, 0)).remaining_ratio, 1.0);
        assert!((burn_rate(0.99, (100, 90)).unwrap_or_default() - 10.0).abs() < 1e-9);
        assert_eq!(burn_rate(0.99, (0, 0)), None);
    }
}
