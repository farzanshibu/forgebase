//! Usage primitives that run inside one database transaction: counter state,
//! rollover, allocation (window allowance, then usage packs, then overage),
//! reservations, events and alerts.
//!
//! Every metering call reads the state and writes the change on the single
//! writer in one transaction, and the counter update carries the limit in
//! its `WHERE` clause, so a quota is never over-granted, also on a backend
//! with concurrent writers.

use forgebase_db::{Backend, Rows, Statement, Value, WriteTx};
use serde_json::Value as Json;

use crate::cache::{Entry, Key};
use crate::error::{EntitlementError, Result};
use crate::model::{Decision, Enforcement, FeatureKind, QuotaPolicy, Reservation, ReservationStatus};
use crate::period::{self, Period, Window};

/// Alert thresholds in percent of a window's allowance.
pub const THRESHOLDS: [i64; 3] = [70, 90, 100];

/// A connection: inside a write transaction, or read-only (dry runs).
pub(crate) enum Db<'a> {
    Tx(&'a mut (dyn WriteTx + 'static)),
    Ro(&'a dyn Backend),
}

impl Db<'_> {
    pub async fn fetch(&mut self, sql: &str, params: Vec<Value>) -> Result<Rows> {
        let st = Statement::new(sql, params);
        Ok(match self {
            Db::Tx(tx) => tx.fetch_all(&st).await?,
            Db::Ro(db) => db.fetch_all(&st).await?,
        })
    }

    pub async fn exec(&mut self, sql: &str, params: Vec<Value>) -> Result<u64> {
        let st = Statement::new(sql, params);
        match self {
            Db::Tx(tx) => Ok(tx.execute(&st).await?.rows_affected),
            Db::Ro(_) => Err(EntitlementError::Internal("write on a read-only connection".into())),
        }
    }
}

pub(crate) fn int(r: &[Value], i: usize) -> i64 {
    r.get(i).and_then(Value::as_i64).unwrap_or_default()
}

pub(crate) fn opt_int(r: &[Value], i: usize) -> Option<i64> {
    r.get(i).and_then(Value::as_i64)
}

pub(crate) fn text(r: &[Value], i: usize) -> String {
    r.get(i).and_then(Value::as_str).unwrap_or_default().to_owned()
}

pub(crate) fn opt_text(r: &[Value], i: usize) -> Option<String> {
    r.get(i).and_then(Value::as_str).map(str::to_owned)
}

/// A new id with a prefix.
pub(crate) fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4().simple())
}

/// Everything a metering call needs to know about one counter.
#[derive(Debug, Clone)]
pub(crate) struct Quota {
    pub key: Key,
    pub kind: FeatureKind,
    /// Limit per window (`None`: unlimited).
    pub limit: Option<i64>,
    pub period: Period,
    pub window_secs: Option<i64>,
    pub anchor: Option<i64>,
    pub policy: QuotaPolicy,
}

impl Quota {
    pub fn window(&self, now: i64) -> Window {
        period::window(self.period, self.window_secs, self.anchor, now)
    }

    fn counted(&self) -> bool {
        self.kind != FeatureKind::Concurrency && self.period != Period::Rolling
    }

    fn key_params(&self) -> Vec<Value> {
        vec![
            self.key.feature.clone().into(),
            self.key.subject.clone().into(),
            self.key.scope_key.clone().into(),
        ]
    }
}

/// Counter state of a key at one time.
#[derive(Debug, Clone, Default)]
pub(crate) struct State {
    pub window: Window,
    pub carried: i64,
    pub used: i64,
    pub reserved: i64,
    pub packs: i64,
    pub alert_level: i64,
    pub exists: bool,
    /// Rolling windows: time of the oldest event in the window.
    pub oldest: Option<i64>,
    /// Concurrency: expiry of each active lease.
    pub leases: Vec<i64>,
}

impl State {
    /// Limit plus rolled-over units.
    pub fn cap(&self, q: &Quota) -> Option<i64> {
        q.limit.map(|l| l.saturating_add(self.carried))
    }

    /// The cache entry for this state.
    pub fn entry(&self, q: &Quota) -> Entry {
        let valid_until = match (q.period, self.oldest) {
            (Period::Rolling, Some(t)) => t.saturating_add(q.window_secs.unwrap_or(3600).max(1) * 1000),
            (Period::Rolling, None) => i64::MAX,
            _ => self.window.end,
        };
        Entry {
            window_start: self.window.start,
            consumed: self.used + self.reserved,
            carried: self.carried,
            leases: self.leases.clone(),
            valid_until,
            ..Entry::default()
        }
    }
}

/// Reads the state of `q`'s key at `now`.
pub(crate) async fn load_state(db: &mut Db<'_>, q: &Quota, now: i64) -> Result<State> {
    let w = q.window(now);
    let mut st = State {
        window: w,
        ..State::default()
    };
    if q.kind == FeatureKind::Concurrency {
        let mut p = q.key_params();
        p.push(now.into());
        let r = db
            .fetch(
                "SELECT expires_at FROM _forgebase_quota_leases
                 WHERE feature_key = ?1 AND subject = ?2 AND scope_key = ?3 AND released_at IS NULL AND expires_at > ?4",
                p,
            )
            .await?;
        st.leases = r.rows.iter().map(|r| int(r, 0)).collect();
        st.used = st.leases.len() as i64;
        st.exists = true;
        return Ok(st);
    }
    if q.period == Period::Rolling {
        let mut p = q.key_params();
        p.extend([w.start.into(), now.into()]);
        let r = db
            .fetch(
                "SELECT COALESCE(SUM(units - pack_units), 0), MIN(at) FROM _forgebase_usage_events
                 WHERE feature_key = ?1 AND subject = ?2 AND scope_key = ?3 AND at > ?4 AND at <= ?5",
                p,
            )
            .await?;
        if let Some(row) = r.rows.first() {
            st.used = int(row, 0);
            st.oldest = opt_int(row, 1);
        }
        let mut p = q.key_params();
        p.push(now.into());
        let r = db
            .fetch(
                "SELECT COALESCE(SUM(units - pack_units), 0) FROM _forgebase_usage_reservations
                 WHERE feature_key = ?1 AND subject = ?2 AND scope_key = ?3 AND status = 'held' AND expires_at > ?4",
                p,
            )
            .await?;
        st.reserved = r.rows.first().map_or(0, |r| int(r, 0));
        st.exists = true;
    } else {
        let mut p = q.key_params();
        p.push(w.start.into());
        let r = db
            .fetch(
                "SELECT used, reserved, carried_in, alert_level FROM _forgebase_usage_counters
                 WHERE feature_key = ?1 AND subject = ?2 AND scope_key = ?3 AND window_start = ?4",
                p,
            )
            .await?;
        if let Some(row) = r.rows.first() {
            st.used = int(row, 0);
            st.reserved = int(row, 1);
            st.carried = int(row, 2);
            st.alert_level = int(row, 3);
            st.exists = true;
        } else {
            st.carried = carry(db, q, w).await?;
        }
    }
    if q.kind == FeatureKind::Metered {
        st.packs = pack_balance(db, &q.key.feature, &q.key.subject, now).await?;
    }
    Ok(st)
}

/// The counter row of one window of `q`'s key, as a state (no packs).
pub(crate) async fn window_row(db: &mut Db<'_>, q: &Quota, window_start: i64) -> Result<Option<State>> {
    if !q.counted() {
        return Ok(None);
    }
    let mut p = q.key_params();
    p.push(window_start.into());
    let r = db
        .fetch(
            "SELECT used, reserved, carried_in, alert_level, window_end FROM _forgebase_usage_counters
             WHERE feature_key = ?1 AND subject = ?2 AND scope_key = ?3 AND window_start = ?4",
            p,
        )
        .await?;
    Ok(r.rows.first().map(|row| State {
        window: Window {
            start: window_start,
            end: int(row, 4),
        },
        used: int(row, 0),
        reserved: int(row, 1),
        carried: int(row, 2),
        alert_level: int(row, 3),
        exists: true,
        ..State::default()
    }))
}

/// Usage-pack units a subject has left for a feature.
pub(crate) async fn pack_balance(db: &mut Db<'_>, feature: &str, subject: &str, now: i64) -> Result<i64> {
    let r = db
        .fetch(
            "SELECT COALESCE(SUM(remaining), 0) FROM _forgebase_pack_grants
             WHERE feature_key = ?1 AND subject = ?2 AND remaining > 0 AND (expires_at IS NULL OR expires_at > ?3)",
            vec![feature.into(), subject.into(), now.into()],
        )
        .await?;
    Ok(r.rows.first().map_or(0, |r| int(r, 0)))
}

/// Units rolled over into window `w` from the previous window.
async fn carry(db: &mut Db<'_>, q: &Quota, w: Window) -> Result<i64> {
    let (Some(limit), true) = (q.limit, q.policy.rollover_pct > 0 && q.period.is_calendar()) else {
        return Ok(0);
    };
    let Some(prev) = period::previous(q.period, q.anchor, w) else {
        return Ok(0);
    };
    let mut p = q.key_params();
    p.push(prev.start.into());
    let r = db
        .fetch(
            "SELECT used, reserved, carried_in, limit_value FROM _forgebase_usage_counters
             WHERE feature_key = ?1 AND subject = ?2 AND scope_key = ?3 AND window_start = ?4",
            p,
        )
        .await?;
    let Some(row) = r.rows.first() else { return Ok(0) };
    let Some(prev_limit) = opt_int(row, 3) else { return Ok(0) };
    let unused = (prev_limit + int(row, 2) - int(row, 0) - int(row, 1)).max(0);
    let mut c = unused.min(limit.saturating_mul(q.policy.rollover_pct) / 100);
    if let Some(cap) = q.policy.rollover_cap {
        c = c.min(cap);
    }
    Ok(c.max(0))
}

/// How a request's units are covered.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Alloc {
    /// Units counted in the window (including overage).
    pub window_units: i64,
    /// Units taken from usage packs.
    pub pack_units: i64,
    /// Units past the limit of a soft quota.
    pub overage: i64,
}

/// The ceiling `used + reserved` may reach, for the guarded update.
pub(crate) fn ceiling(q: &Quota, st: &State) -> Option<i64> {
    let cap = st.cap(q)?;
    match (q.policy.enforcement, q.policy.overage_pct) {
        (Enforcement::Hard, _) => Some(cap),
        (Enforcement::Soft, Some(pct)) => Some(cap.saturating_add(cap.saturating_mul(pct) / 100)),
        (Enforcement::Soft, None) => None,
    }
}

/// Covers `units` from the window allowance, then usage packs, then (soft
/// quotas) overage. `Err` carries the reason of a denial.
pub(crate) fn allocate(q: &Quota, st: &State, units: i64) -> std::result::Result<Alloc, String> {
    let Some(cap) = st.cap(q) else {
        return Ok(Alloc {
            window_units: units,
            ..Alloc::default()
        });
    };
    let current = st.used + st.reserved;
    if units <= 0 {
        // Gauge decrease.
        return Ok(Alloc {
            window_units: units,
            ..Alloc::default()
        });
    }
    if q.kind == FeatureKind::Concurrency {
        return if current + units > cap {
            Err(format!("all {cap} concurrent {} leases are in use", q.key.feature))
        } else {
            Ok(Alloc {
                window_units: units,
                ..Alloc::default()
            })
        };
    }
    let w = units.min((cap - current).max(0));
    let mut rest = units - w;
    let p = if q.kind == FeatureKind::Metered {
        rest.min(st.packs)
    } else {
        0
    };
    rest -= p;
    if rest == 0 {
        return Ok(Alloc {
            window_units: w,
            pack_units: p,
            overage: 0,
        });
    }
    match q.policy.enforcement {
        Enforcement::Hard => Err(format!("quota exhausted for {}", q.key.feature)),
        Enforcement::Soft => {
            if let Some(c) = ceiling(q, st)
                && current + w + rest > c
            {
                return Err(format!("overage limit reached for {}", q.key.feature));
            }
            Ok(Alloc {
                window_units: w + rest,
                pack_units: p,
                overage: rest,
            })
        }
    }
}

/// A decision over a state.
pub(crate) fn decision(q: &Quota, st: &State, units: i64, alloc: Alloc, reason: Option<String>) -> Decision {
    let cap = st.cap(q);
    Decision {
        allowed: reason.is_none(),
        feature: q.key.feature.clone(),
        subject: q.key.subject.clone(),
        scope_key: q.key.scope_key.clone(),
        units,
        limit: cap,
        used: st.used,
        reserved: st.reserved,
        remaining: cap.map(|c| (c - st.used - st.reserved).max(0) + st.packs),
        pack_remaining: st.packs,
        overage_units: alloc.overage,
        pack_units: alloc.pack_units,
        window_start: st.window.start,
        window_end: q.period.is_calendar().then_some(st.window.end),
        reason,
    }
}

/// Creates the counter row of the state's window when missing.
pub(crate) async fn ensure_row(db: &mut Db<'_>, q: &Quota, st: &mut State, now: i64) -> Result<()> {
    if st.exists || !q.counted() {
        return Ok(());
    }
    let mut p = q.key_params();
    p.extend([
        st.window.start.into(),
        st.window.end.into(),
        st.carried.into(),
        q.limit.into(),
        now.into(),
    ]);
    db.exec(
        "INSERT INTO _forgebase_usage_counters
           (feature_key, subject, scope_key, window_start, window_end, used, reserved, carried_in, limit_value, alert_level, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 0, 0, ?6, ?7, 0, ?8)
         ON CONFLICT DO NOTHING",
        p,
    )
    .await?;
    st.exists = true;
    Ok(())
}

/// Changes a counter row; with a ceiling the update applies only while
/// `used + reserved` stays within it.
pub(crate) async fn bump(
    db: &mut Db<'_>,
    q: &Quota,
    window_start: i64,
    d_used: i64,
    d_reserved: i64,
    ceiling: Option<i64>,
    now: i64,
) -> Result<bool> {
    if !q.counted() {
        return Ok(true);
    }
    let mut p = q.key_params();
    p.extend([
        window_start.into(),
        d_used.into(),
        d_reserved.into(),
        now.into(),
        ceiling.into(),
    ]);
    p.push(q.limit.into());
    let n = db
        .exec(
            "UPDATE _forgebase_usage_counters
             SET used = used + ?5, reserved = reserved + ?6, updated_at = ?7, limit_value = ?9
             WHERE feature_key = ?1 AND subject = ?2 AND scope_key = ?3 AND window_start = ?4
               AND (?8 IS NULL OR ?5 + ?6 <= 0 OR used + reserved + ?5 + ?6 <= ?8)",
            p,
        )
        .await?;
    Ok(n == 1)
}

/// Takes `units` from the subject's usage packs, oldest expiry first.
/// Returns `[(grant id, units)]`.
pub(crate) async fn take_packs(db: &mut Db<'_>, q: &Quota, units: i64, now: i64) -> Result<Vec<(String, i64)>> {
    if units <= 0 {
        return Ok(Vec::new());
    }
    let r = db
        .fetch(
            "SELECT id, remaining FROM _forgebase_pack_grants
             WHERE feature_key = ?1 AND subject = ?2 AND remaining > 0 AND (expires_at IS NULL OR expires_at > ?3)
             ORDER BY expires_at IS NULL, expires_at, created_at, id",
            vec![q.key.feature.clone().into(), q.key.subject.clone().into(), now.into()],
        )
        .await?;
    let mut need = units;
    let mut out = Vec::new();
    for row in &r.rows {
        if need == 0 {
            break;
        }
        let take = need.min(int(row, 1));
        let id = text(row, 0);
        let n = db
            .exec(
                "UPDATE _forgebase_pack_grants SET remaining = remaining - ?1 WHERE id = ?2 AND remaining >= ?1",
                vec![take.into(), id.clone().into()],
            )
            .await?;
        if n != 1 {
            return Err(EntitlementError::Internal("usage pack balance changed".into()));
        }
        out.push((id, take));
        need -= take;
    }
    if need > 0 {
        return Err(EntitlementError::Internal("usage pack balance changed".into()));
    }
    Ok(out)
}

/// Gives `units` back to the grants they came from (last taken first).
/// Returns what stays taken.
pub(crate) async fn return_packs(db: &mut Db<'_>, taken: &[(String, i64)], units: i64) -> Result<Vec<(String, i64)>> {
    let mut back = units;
    let mut kept: Vec<(String, i64)> = taken.to_vec();
    for (id, n) in kept.iter_mut().rev() {
        if back == 0 {
            break;
        }
        let give = back.min(*n);
        db.exec(
            "UPDATE _forgebase_pack_grants SET remaining = MIN(units, remaining + ?1) WHERE id = ?2",
            vec![give.into(), id.clone().into()],
        )
        .await?;
        *n -= give;
        back -= give;
    }
    kept.retain(|(_, n)| *n > 0);
    Ok(kept)
}

/// A usage event to insert.
#[derive(Debug, Clone)]
pub(crate) struct NewEvent<'a> {
    pub q: &'a Quota,
    pub at: i64,
    pub units: i64,
    pub pack_units: i64,
    pub overage: i64,
    pub window_start: i64,
    pub source: &'static str,
    pub reservation_id: Option<String>,
    pub idempotency_key: String,
    pub actor: String,
    pub request_id: Option<String>,
    pub metadata: Json,
    pub result: Json,
}

/// Inserts an immutable usage event; returns (id, seq).
pub(crate) async fn insert_event(db: &mut Db<'_>, e: NewEvent<'_>) -> Result<(String, i64)> {
    let id = new_id("evt");
    let r = db
        .fetch(
            "INSERT INTO _forgebase_usage_events
               (id, at, subject, feature_key, scope_key, units, pack_units, overage_units, window_start, source,
                reservation_id, idempotency_key, actor, request_id, metadata, result)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
             RETURNING seq",
            vec![
                id.clone().into(),
                e.at.into(),
                e.q.key.subject.clone().into(),
                e.q.key.feature.clone().into(),
                e.q.key.scope_key.clone().into(),
                e.units.into(),
                e.pack_units.into(),
                e.overage.into(),
                e.window_start.into(),
                e.source.into(),
                e.reservation_id.into(),
                e.idempotency_key.into(),
                e.actor.into(),
                e.request_id.into(),
                e.metadata.to_string().into(),
                e.result.to_string().into(),
            ],
        )
        .await?;
    Ok((id, r.rows.first().map_or(0, |r| int(r, 0))))
}

/// Raises the alerts `used` crosses in a window (once per threshold) and
/// returns their ids. Gauges re-arm thresholds when the level drops.
pub(crate) async fn raise_alerts(
    db: &mut Db<'_>,
    q: &Quota,
    st: &State,
    used: i64,
    now: i64,
) -> Result<Vec<String>> {
    if !q.counted() {
        return Ok(Vec::new());
    }
    let Some(cap) = st.cap(q).filter(|c| *c > 0) else {
        return Ok(Vec::new());
    };
    let pct = used.saturating_mul(100) / cap;
    let crossed: Vec<i64> = THRESHOLDS
        .into_iter()
        .filter(|t| *t <= pct && *t > st.alert_level)
        .collect();
    let mut p = q.key_params();
    p.push(st.window.start.into());
    let Some(&top) = crossed.last() else {
        if q.kind == FeatureKind::Gauge {
            let level = THRESHOLDS.into_iter().rev().find(|t| *t <= pct).unwrap_or(0);
            if level < st.alert_level {
                p.push(level.into());
                db.exec(
                    "UPDATE _forgebase_usage_counters SET alert_level = ?5
                     WHERE feature_key = ?1 AND subject = ?2 AND scope_key = ?3 AND window_start = ?4",
                    p,
                )
                .await?;
            }
        }
        return Ok(Vec::new());
    };
    p.push(top.into());
    let n = db
        .exec(
            "UPDATE _forgebase_usage_counters SET alert_level = ?5
             WHERE feature_key = ?1 AND subject = ?2 AND scope_key = ?3 AND window_start = ?4 AND alert_level < ?5",
            p,
        )
        .await?;
    if n == 0 {
        return Ok(Vec::new());
    }
    let mut ids = Vec::new();
    for t in crossed {
        let id = new_id("alr");
        db.exec(
            "INSERT INTO _forgebase_usage_alerts
               (id, at, subject, feature_key, scope_key, window_start, threshold, used, limit_value)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            vec![
                id.clone().into(),
                now.into(),
                q.key.subject.clone().into(),
                q.key.feature.clone().into(),
                q.key.scope_key.clone().into(),
                st.window.start.into(),
                t.into(),
                used.into(),
                cap.into(),
            ],
        )
        .await?;
        ids.push(id);
    }
    Ok(ids)
}

/// A stored reservation.
#[derive(Debug, Clone)]
pub(crate) struct ResRow {
    pub id: String,
    pub feature: String,
    pub subject: String,
    pub scope_key: String,
    pub window_start: i64,
    pub units: i64,
    pub pack_units: i64,
    pub overage: i64,
    pub packs: Vec<(String, i64)>,
    pub status: ReservationStatus,
    pub committed_units: Option<i64>,
    pub expires_at: i64,
    pub created_at: i64,
}

pub(crate) const RES_COLUMNS: &str = "id, feature_key, subject, scope_key, window_start, units, pack_units, overage_units,
     packs, status, committed_units, expires_at, idempotency_key, created_at";

pub(crate) fn res_row(r: &[Value]) -> ResRow {
    ResRow {
        id: text(r, 0),
        feature: text(r, 1),
        subject: text(r, 2),
        scope_key: text(r, 3),
        window_start: int(r, 4),
        units: int(r, 5),
        pack_units: int(r, 6),
        overage: int(r, 7),
        packs: serde_json::from_str(&text(r, 8)).unwrap_or_default(),
        status: ReservationStatus::parse(&text(r, 9)).unwrap_or(ReservationStatus::Expired),
        committed_units: opt_int(r, 10),
        expires_at: int(r, 11),
        created_at: int(r, 13),
    }
}

impl ResRow {
    pub fn public(&self, replayed: bool) -> Reservation {
        Reservation {
            id: self.id.clone(),
            feature: self.feature.clone(),
            subject: self.subject.clone(),
            scope_key: self.scope_key.clone(),
            units: self.units,
            pack_units: self.pack_units,
            overage_units: self.overage,
            status: self.status,
            committed_units: self.committed_units,
            expires_at: self.expires_at,
            created_at: self.created_at,
            replayed,
        }
    }
}

/// Gives a held reservation back (released or expired): its window units
/// leave the counter, its pack units return to the packs.
pub(crate) async fn settle_back(db: &mut Db<'_>, q: &Quota, r: &ResRow, status: &str, now: i64) -> Result<()> {
    let n = db
        .exec(
            "UPDATE _forgebase_usage_reservations SET status = ?1, settled_at = ?2 WHERE id = ?3 AND status = 'held'",
            vec![status.into(), now.into(), r.id.clone().into()],
        )
        .await?;
    if n != 1 {
        return Ok(());
    }
    bump(db, q, r.window_start, 0, -(r.units - r.pack_units), None, now).await?;
    return_packs(db, &r.packs, r.pack_units).await?;
    Ok(())
}

/// Expires the held reservations of `q`'s key that ran out.
pub(crate) async fn reap_key(db: &mut Db<'_>, q: &Quota, now: i64) -> Result<()> {
    let mut p = q.key_params();
    p.push(now.into());
    let sql = format!(
        "SELECT {RES_COLUMNS} FROM _forgebase_usage_reservations
         WHERE feature_key = ?1 AND subject = ?2 AND scope_key = ?3 AND status = 'held' AND expires_at <= ?4"
    );
    let rows = db.fetch(&sql, p).await?;
    for row in &rows.rows {
        let r = res_row(row);
        settle_back(db, q, &r, "expired", now).await?;
    }
    Ok(())
}
