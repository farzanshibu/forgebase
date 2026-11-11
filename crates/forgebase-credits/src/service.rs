//! [`CreditService`]: balances, the ledger, top-ups, grants, spends,
//! refunds, reversals, expiry and authorization. Usage consumption is in
//! [`crate::usage`], statistics in [`crate::stats`].
//!
//! Every write is one transaction on the single writer: idempotency lookup,
//! lazy expiry of the subject's lapsed lots, the balance change (a debit's
//! `UPDATE` carries `WHERE balance >= n`), the lot changes with their
//! allocations, and the ledger row.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, OnceLock};

use forgebase_data::{AccessContext, Principal};
use forgebase_db::backend::BoxFuture;
use forgebase_db::{Backend, Rows, Statement, TxMode, Value, WriteTx};
use forgebase_entitlements::{Clock, EntitlementService, parse_subject, subject_key};
use forgebase_jobs::JobService;
use forgebase_policy::{Check, Denial, PolicyError, PolicyService, Step};
use serde_json::{Value as Json, json};

use crate::error::{CreditError, Result};
use crate::model::{
    AmendInput, AuditReport, Balance, CreditInput, EntryKind, Expiring, ExpiryReport, LedgerEntry, LedgerPage, Lot,
    MaintenanceReport, Posted, PurchaseReversal, Rate, RateBasis, RateInput, Reversal, SpendInput,
};

/// Job kind of the periodic maintenance (expiry, usage consumption).
pub const MAINTENANCE_JOB: &str = "credits.maintenance";
/// Name of the maintenance schedule.
pub const MAINTENANCE_SCHEDULE: &str = "credits-maintenance";
/// Name of the usage-event cursor.
pub const USAGE_CURSOR: &str = "usage";
/// Largest amount of one entry.
pub const MAX_AMOUNT: i64 = 1_000_000_000_000;

/// Idempotency-key prefixes the service uses itself.
const RESERVED_PREFIXES: [&str; 2] = ["usage:", "expiry:"];
/// Lots expired per transaction.
const SWEEP_BATCH: i64 = 500;

/// What FB-70 calls when a payment buys credits, a plan includes credits,
/// or a purchase is refunded. Every call is idempotent by its key and runs
/// as the system principal.
pub trait CreditSink: Send + Sync {
    /// Credits a purchase as a top-up (idempotency key: the payment or
    /// webhook event id; `external_ref`: the order or invoice).
    fn credit_purchase<'a>(&'a self, purchase: CreditInput) -> BoxFuture<'a, Result<Posted>>;
    /// Grants free credits (plan allowances, promotions), optionally expiring.
    fn grant_credits<'a>(&'a self, grant: CreditInput) -> BoxFuture<'a, Result<Posted>>;
    /// Takes back what is left of a purchase (refund, chargeback).
    fn reverse_purchase<'a>(&'a self, reversal: PurchaseReversal) -> BoxFuture<'a, Result<Reversal>>;
}

impl CreditSink for CreditService {
    fn credit_purchase<'a>(&'a self, purchase: CreditInput) -> BoxFuture<'a, Result<Posted>> {
        Box::pin(async move { self.top_up(&AccessContext::system(), purchase).await })
    }

    fn grant_credits<'a>(&'a self, grant: CreditInput) -> BoxFuture<'a, Result<Posted>> {
        Box::pin(async move { self.grant(&AccessContext::system(), grant).await })
    }

    fn reverse_purchase<'a>(&'a self, r: PurchaseReversal) -> BoxFuture<'a, Result<Reversal>> {
        Box::pin(async move {
            let subject = check_subject(&r.subject)?;
            let rows = self
                .db()
                .fetch_all(&Statement::new(
                    "SELECT id FROM _forgebase_credit_ledger
                     WHERE subject = ?1 AND idempotency_key = ?2 AND kind = 'top_up'",
                    vec![subject.into(), r.purchase_key.trim().into()],
                ))
                .await?;
            let Some(id) = rows.rows.first().map(|row| text(row, 0)) else {
                return Err(CreditError::NotFound("purchase".into()));
            };
            self.reverse(
                &AccessContext::system(),
                AmendInput {
                    entry_id: id,
                    amount: r.amount,
                    idempotency_key: r.idempotency_key,
                    reason: r.reason,
                    external_ref: r.external_ref,
                    metadata: None,
                },
            )
            .await
        })
    }
}

/// The credit service. Cheap to clone.
#[derive(Clone)]
pub struct CreditService {
    pub(crate) inner: Arc<Inner>,
}

impl fmt::Debug for CreditService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CreditService").finish_non_exhaustive()
    }
}

pub(crate) struct Inner {
    pub db: Arc<dyn Backend>,
    pub policy: PolicyService,
    pub usage: EntitlementService,
    pub clock: Arc<dyn Clock>,
    pub jobs: OnceLock<JobService>,
    pub consumer: tokio::sync::Mutex<()>,
}

// ---------------------------------------------------------------------------
// Rows and transactions
// ---------------------------------------------------------------------------

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

pub(crate) fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4().simple())
}

fn scalar(rows: &Rows) -> i64 {
    rows.scalar().and_then(Value::as_i64).unwrap_or_default()
}

/// A write transaction.
pub(crate) struct Tx<'a>(pub &'a mut (dyn WriteTx + 'static));

impl Tx<'_> {
    pub async fn fetch(&mut self, sql: &str, params: Vec<Value>) -> Result<Rows> {
        Ok(self.0.fetch_all(&Statement::new(sql, params)).await?)
    }

    pub async fn exec(&mut self, sql: &str, params: Vec<Value>) -> Result<u64> {
        Ok(self.0.execute(&Statement::new(sql, params)).await?.rows_affected)
    }

    async fn insert(&mut self, sql: &str, params: Vec<Value>) -> Result<i64> {
        Ok(self.0.execute(&Statement::new(sql, params)).await?.last_insert_id)
    }
}

const ENTRY_COLUMNS: &str = "seq, id, at, subject, kind, amount, balance_after, lot_id, ref_entry, feature_key, units,
     usage_event_id, usage_seq, expires_at, idempotency_key, actor, reason, external_ref, request_id, metadata, request";

fn entry_row(r: &[Value]) -> Result<(LedgerEntry, Json)> {
    let kind = EntryKind::parse(&text(r, 4)).ok_or_else(|| CreditError::Internal("unknown entry kind".into()))?;
    Ok((
        LedgerEntry {
            seq: int(r, 0),
            id: text(r, 1),
            at: int(r, 2),
            subject: text(r, 3),
            kind,
            amount: int(r, 5),
            balance_after: int(r, 6),
            lot_id: opt_text(r, 7),
            ref_entry: opt_text(r, 8),
            feature: opt_text(r, 9),
            units: opt_int(r, 10),
            usage_event_id: opt_text(r, 11),
            usage_seq: opt_int(r, 12),
            expires_at: opt_int(r, 13),
            idempotency_key: text(r, 14),
            actor: text(r, 15),
            reason: opt_text(r, 16),
            external_ref: opt_text(r, 17),
            request_id: opt_text(r, 18),
            metadata: serde_json::from_str(&text(r, 19)).unwrap_or(Json::Null),
        },
        serde_json::from_str(&text(r, 20)).unwrap_or(Json::Null),
    ))
}

fn entries(rows: &Rows) -> Result<Vec<LedgerEntry>> {
    rows.rows.iter().map(|r| entry_row(r).map(|(e, _)| e)).collect()
}

const LOT_COLUMNS: &str = "id, subject, entry_id, kind, amount, remaining, expires_at, created_at";

fn lot_row(r: &[Value]) -> Lot {
    Lot {
        id: text(r, 0),
        subject: text(r, 1),
        entry_id: text(r, 2),
        kind: EntryKind::parse(&text(r, 3)).unwrap_or(EntryKind::TopUp),
        amount: int(r, 4),
        remaining: int(r, 5),
        expires_at: opt_int(r, 6),
        created_at: int(r, 7),
    }
}

/// Descriptive fields of a new entry, and the request fingerprint that tells
/// a replay from a conflicting reuse of the idempotency key.
#[derive(Debug, Clone)]
pub(crate) struct Meta {
    pub idem: String,
    pub actor: String,
    pub reason: Option<String>,
    pub external_ref: Option<String>,
    pub request_id: Option<String>,
    pub metadata: Json,
    pub request: Json,
}

impl Meta {
    fn new(
        ctx: &AccessContext,
        idem: String,
        reason: Option<String>,
        external_ref: Option<String>,
        metadata: Option<Json>,
        request: Json,
    ) -> Self {
        Self {
            idem,
            actor: ctx.principal_key(),
            reason,
            external_ref,
            request_id: ctx.request_id.clone(),
            metadata: metadata.unwrap_or_else(|| json!({})),
            request,
        }
    }

    pub fn system(idem: String, reason: &str, request: Json) -> Self {
        Self {
            idem,
            actor: "system".into(),
            reason: Some(reason.into()),
            external_ref: None,
            request_id: None,
            metadata: json!({}),
            request,
        }
    }
}

/// The fields of a ledger row that differ by kind.
#[derive(Debug, Default)]
pub(crate) struct Fields {
    pub lot_id: Option<String>,
    pub ref_entry: Option<String>,
    pub feature: Option<String>,
    pub units: Option<i64>,
    pub usage_event_id: Option<String>,
    pub usage_seq: Option<i64>,
    pub expires_at: Option<i64>,
}

#[allow(clippy::too_many_arguments)]
async fn insert_entry(
    t: &mut Tx<'_>,
    id: String,
    subject: &str,
    kind: EntryKind,
    amount: i64,
    balance_after: i64,
    f: Fields,
    meta: &Meta,
    now: i64,
) -> Result<LedgerEntry> {
    let seq = t
        .insert(
            "INSERT INTO _forgebase_credit_ledger (id, at, subject, kind, amount, balance_after, lot_id, ref_entry,
                 feature_key, units, usage_event_id, usage_seq, expires_at, idempotency_key, actor, reason,
                 external_ref, request_id, metadata, request)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
            vec![
                id.clone().into(),
                now.into(),
                subject.into(),
                kind.as_str().into(),
                amount.into(),
                balance_after.into(),
                f.lot_id.clone().into(),
                f.ref_entry.clone().into(),
                f.feature.clone().into(),
                f.units.into(),
                f.usage_event_id.clone().into(),
                f.usage_seq.into(),
                f.expires_at.into(),
                meta.idem.clone().into(),
                meta.actor.clone().into(),
                meta.reason.clone().into(),
                meta.external_ref.clone().into(),
                meta.request_id.clone().into(),
                meta.metadata.to_string().into(),
                meta.request.to_string().into(),
            ],
        )
        .await?;
    Ok(LedgerEntry {
        seq,
        id,
        at: now,
        subject: subject.to_owned(),
        kind,
        amount,
        balance_after,
        lot_id: f.lot_id,
        ref_entry: f.ref_entry,
        feature: f.feature,
        units: f.units,
        usage_event_id: f.usage_event_id,
        usage_seq: f.usage_seq,
        expires_at: f.expires_at,
        idempotency_key: meta.idem.clone(),
        actor: meta.actor.clone(),
        reason: meta.reason.clone(),
        external_ref: meta.external_ref.clone(),
        request_id: meta.request_id.clone(),
        metadata: meta.metadata.clone(),
    })
}

async fn allocate(t: &mut Tx<'_>, entry: &str, lot: &str, amount: i64) -> Result<()> {
    t.exec(
        "INSERT INTO _forgebase_credit_allocations (entry_id, lot_id, amount) VALUES (?1, ?2, ?3)",
        vec![entry.into(), lot.into(), amount.into()],
    )
    .await?;
    Ok(())
}

/// The stored balance (0 without an account).
pub(crate) async fn stored_balance(t: &mut Tx<'_>, subject: &str) -> Result<i64> {
    Ok(scalar(
        &t.fetch(
            "SELECT balance FROM _forgebase_credit_accounts WHERE subject = ?1",
            vec![subject.into()],
        )
        .await?,
    ))
}

async fn add_balance(t: &mut Tx<'_>, subject: &str, amount: i64, now: i64) -> Result<i64> {
    t.exec(
        "INSERT INTO _forgebase_credit_accounts (subject, balance, created_at, updated_at) VALUES (?1, 0, ?2, ?2)
         ON CONFLICT (subject) DO NOTHING",
        vec![subject.into(), now.into()],
    )
    .await?;
    let n = t
        .exec(
            "UPDATE _forgebase_credit_accounts SET balance = balance + ?1, updated_at = ?2
             WHERE subject = ?3 AND balance <= ?4 - ?1",
            vec![amount.into(), now.into(), subject.into(), i64::MAX.into()],
        )
        .await?;
    if n != 1 {
        return Err(CreditError::conflict("balance_overflow", "the balance would overflow"));
    }
    stored_balance(t, subject).await
}

/// Takes `amount` off the balance; `None` when the balance does not cover
/// it. The check is in the `UPDATE`, so concurrent writers cannot overdraw.
async fn sub_balance(t: &mut Tx<'_>, subject: &str, amount: i64, now: i64) -> Result<Option<i64>> {
    let n = t
        .exec(
            "UPDATE _forgebase_credit_accounts SET balance = balance - ?1, updated_at = ?2
             WHERE subject = ?3 AND balance >= ?1",
            vec![amount.into(), now.into(), subject.into()],
        )
        .await?;
    if n != 1 {
        return Ok(None);
    }
    Ok(Some(stored_balance(t, subject).await?))
}

/// Adds credits: a new lot holding them, its allocation and the entry.
pub(crate) async fn credit_tx(
    t: &mut Tx<'_>,
    subject: &str,
    kind: EntryKind,
    amount: i64,
    expires_at: Option<i64>,
    meta: &Meta,
    now: i64,
) -> Result<LedgerEntry> {
    let after = add_balance(t, subject, amount, now).await?;
    let id = new_id("cre");
    let lot = new_id("lot");
    t.exec(
        "INSERT INTO _forgebase_credit_lots (id, subject, entry_id, kind, amount, remaining, expires_at, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6, ?7)",
        vec![
            lot.clone().into(),
            subject.into(),
            id.clone().into(),
            kind.as_str().into(),
            amount.into(),
            expires_at.into(),
            now.into(),
        ],
    )
    .await?;
    allocate(t, &id, &lot, amount).await?;
    insert_entry(
        t,
        id,
        subject,
        kind,
        amount,
        after,
        Fields {
            lot_id: Some(lot),
            expires_at,
            ..Fields::default()
        },
        meta,
        now,
    )
    .await
}

/// Debits `amount` (all or nothing) from the lots that lapse first. Run
/// [`sweep_tx`] for the subject first so the balance equals what the open
/// lots hold.
pub(crate) async fn debit_tx(
    t: &mut Tx<'_>,
    subject: &str,
    kind: EntryKind,
    amount: i64,
    f: Fields,
    meta: &Meta,
    now: i64,
) -> Result<LedgerEntry> {
    let Some(after) = sub_balance(t, subject, amount, now).await? else {
        return Err(CreditError::InsufficientCredits {
            subject: subject.to_owned(),
            balance: stored_balance(t, subject).await?,
            requested: amount,
        });
    };
    let id = new_id("cre");
    let lots = t
        .fetch(
            "SELECT id, remaining FROM _forgebase_credit_lots
             WHERE subject = ?1 AND remaining > 0 AND (expires_at IS NULL OR expires_at > ?2)
             ORDER BY expires_at IS NULL, expires_at, CASE kind WHEN 'grant' THEN 0 ELSE 1 END, created_at, id",
            vec![subject.into(), now.into()],
        )
        .await?;
    let mut rest = amount;
    for r in &lots.rows {
        if rest == 0 {
            break;
        }
        let lot = text(r, 0);
        let take = rest.min(int(r, 1));
        let n = t
            .exec(
                "UPDATE _forgebase_credit_lots SET remaining = remaining - ?1 WHERE id = ?2 AND remaining >= ?1",
                vec![take.into(), lot.clone().into()],
            )
            .await?;
        if n != 1 {
            return Err(CreditError::Internal(format!("lot {lot} changed during the debit")));
        }
        allocate(t, &id, &lot, -take).await?;
        rest -= take;
    }
    if rest != 0 {
        return Err(CreditError::Internal(format!(
            "the open lots of {subject} hold {} credits less than the balance",
            rest
        )));
    }
    insert_entry(t, id, subject, kind, -amount, after, f, meta, now).await
}

/// Empties lapsed lots (of one subject, or any), writing an expiry entry for
/// each. At most `limit` lots.
pub(crate) async fn sweep_batch(t: &mut Tx<'_>, subject: Option<&str>, now: i64, limit: i64) -> Result<ExpiryReport> {
    let rows = t
        .fetch(
            "SELECT id, subject, entry_id, remaining FROM _forgebase_credit_lots
             WHERE remaining > 0 AND expires_at IS NOT NULL AND expires_at <= ?1 AND (?2 IS NULL OR subject = ?2)
             ORDER BY expires_at, id LIMIT ?3",
            vec![now.into(), subject.map(str::to_owned).into(), limit.into()],
        )
        .await?;
    let mut report = ExpiryReport::default();
    for r in &rows.rows {
        let (lot, who, source, remaining) = (text(r, 0), text(r, 1), text(r, 2), int(r, 3));
        let Some(after) = sub_balance(t, &who, remaining, now).await? else {
            return Err(CreditError::Internal(format!(
                "the balance of {who} is below lot {lot}"
            )));
        };
        let n = t
            .exec(
                "UPDATE _forgebase_credit_lots SET remaining = 0 WHERE id = ?1 AND remaining = ?2",
                vec![lot.clone().into(), remaining.into()],
            )
            .await?;
        if n != 1 {
            return Err(CreditError::Internal(format!("lot {lot} changed during expiry")));
        }
        let id = new_id("cre");
        allocate(t, &id, &lot, -remaining).await?;
        let meta = Meta::system(format!("expiry:{id}"), "expired", json!({"op": "expiry", "lot": lot}));
        insert_entry(
            t,
            id,
            &who,
            EntryKind::Expiry,
            -remaining,
            after,
            Fields {
                lot_id: Some(lot),
                ref_entry: Some(source),
                ..Fields::default()
            },
            &meta,
            now,
        )
        .await?;
        report.lots += 1;
        report.credits += remaining;
    }
    Ok(report)
}

/// Empties every lapsed lot of `subject`.
pub(crate) async fn sweep_tx(t: &mut Tx<'_>, subject: &str, now: i64) -> Result<ExpiryReport> {
    let mut total = ExpiryReport::default();
    loop {
        let r = sweep_batch(t, Some(subject), now, SWEEP_BATCH).await?;
        total.lots += r.lots;
        total.credits += r.credits;
        if r.lots < SWEEP_BATCH {
            return Ok(total);
        }
    }
}

/// Gives `amount` (default: what is left) of a debit back to the lots it
/// came from.
async fn refund_tx(
    t: &mut Tx<'_>,
    orig: &LedgerEntry,
    amount: Option<i64>,
    meta: &Meta,
    now: i64,
) -> Result<LedgerEntry> {
    let refunded = scalar(
        &t.fetch(
            "SELECT COALESCE(SUM(amount), 0) FROM _forgebase_credit_ledger WHERE ref_entry = ?1 AND kind = 'refund'",
            vec![orig.id.clone().into()],
        )
        .await?,
    );
    let left = -orig.amount - refunded;
    if left <= 0 {
        return Err(CreditError::conflict(
            "already_refunded",
            format!("{} is refunded in full", orig.id),
        ));
    }
    let amount = amount.unwrap_or(left);
    if amount > left {
        return Err(CreditError::conflict(
            "refund_exceeds_debit",
            format!("only {left} credits of {} are left to refund", orig.id),
        ));
    }
    let after = add_balance(t, &orig.subject, amount, now).await?;
    let drawn = t
        .fetch(
            "SELECT lot_id, -SUM(amount) FROM _forgebase_credit_allocations WHERE entry_id = ?1
             GROUP BY lot_id ORDER BY MIN(id) DESC",
            vec![orig.id.clone().into()],
        )
        .await?;
    let restored: HashMap<String, i64> = t
        .fetch(
            "SELECT a.lot_id, SUM(a.amount) FROM _forgebase_credit_allocations a
             JOIN _forgebase_credit_ledger l ON l.id = a.entry_id
             WHERE l.ref_entry = ?1 AND l.kind = 'refund' GROUP BY a.lot_id",
            vec![orig.id.clone().into()],
        )
        .await?
        .rows
        .iter()
        .map(|r| (text(r, 0), int(r, 1)))
        .collect();
    let id = new_id("cre");
    let mut rest = amount;
    for r in &drawn.rows {
        if rest == 0 {
            break;
        }
        let lot = text(r, 0);
        let room = int(r, 1) - restored.get(&lot).copied().unwrap_or_default();
        let give = rest.min(room);
        if give <= 0 {
            continue;
        }
        t.exec(
            "UPDATE _forgebase_credit_lots SET remaining = remaining + ?1 WHERE id = ?2",
            vec![give.into(), lot.clone().into()],
        )
        .await?;
        allocate(t, &id, &lot, give).await?;
        rest -= give;
    }
    if rest != 0 {
        return Err(CreditError::Internal(format!(
            "the allocations of {} do not cover the refund",
            orig.id
        )));
    }
    insert_entry(
        t,
        id,
        &orig.subject,
        EntryKind::Refund,
        amount,
        after,
        Fields {
            ref_entry: Some(orig.id.clone()),
            feature: orig.feature.clone(),
            ..Fields::default()
        },
        meta,
        now,
    )
    .await
}

/// Takes back up to `amount` (default: what is not reversed yet) of a
/// top-up or grant from what is left of its lot.
async fn reverse_tx(
    t: &mut Tx<'_>,
    orig: &LedgerEntry,
    amount: Option<i64>,
    meta: &Meta,
    now: i64,
) -> Result<(Option<LedgerEntry>, i64, i64)> {
    let lot = orig
        .lot_id
        .clone()
        .ok_or_else(|| CreditError::Internal(format!("{} has no lot", orig.id)))?;
    let reversed = -scalar(
        &t.fetch(
            "SELECT COALESCE(SUM(amount), 0) FROM _forgebase_credit_ledger WHERE ref_entry = ?1 AND kind = 'reversal'",
            vec![orig.id.clone().into()],
        )
        .await?,
    );
    let left = orig.amount - reversed;
    if left <= 0 {
        return Err(CreditError::conflict(
            "already_reversed",
            format!("{} is reversed in full", orig.id),
        ));
    }
    let want = amount.unwrap_or(left);
    if want > left {
        return Err(CreditError::conflict(
            "reversal_exceeds_credit",
            format!("only {left} credits of {} are left to reverse", orig.id),
        ));
    }
    let remaining = scalar(
        &t.fetch(
            "SELECT remaining FROM _forgebase_credit_lots WHERE id = ?1",
            vec![lot.clone().into()],
        )
        .await?,
    );
    let take = want.min(remaining);
    if take == 0 {
        return Ok((None, 0, want));
    }
    let Some(after) = sub_balance(t, &orig.subject, take, now).await? else {
        return Err(CreditError::Internal(format!(
            "the balance of {} is below lot {lot}",
            orig.subject
        )));
    };
    let n = t
        .exec(
            "UPDATE _forgebase_credit_lots SET remaining = remaining - ?1 WHERE id = ?2 AND remaining >= ?1",
            vec![take.into(), lot.clone().into()],
        )
        .await?;
    if n != 1 {
        return Err(CreditError::Internal(format!("lot {lot} changed during the reversal")));
    }
    let id = new_id("cre");
    allocate(t, &id, &lot, -take).await?;
    let e = insert_entry(
        t,
        id,
        &orig.subject,
        EntryKind::Reversal,
        -take,
        after,
        Fields {
            lot_id: Some(lot),
            ref_entry: Some(orig.id.clone()),
            ..Fields::default()
        },
        meta,
        now,
    )
    .await?;
    Ok((Some(e), take, want - take))
}

/// The first result of an idempotency key, or a conflict when the key was
/// used for another request.
pub(crate) async fn replay(t: &mut Tx<'_>, subject: &str, meta: &Meta) -> Result<Option<LedgerEntry>> {
    let rows = t
        .fetch(
            &format!(
                "SELECT {ENTRY_COLUMNS} FROM _forgebase_credit_ledger WHERE subject = ?1 AND idempotency_key = ?2"
            ),
            vec![subject.into(), meta.idem.clone().into()],
        )
        .await?;
    let Some(r) = rows.rows.first() else {
        return Ok(None);
    };
    let (entry, request) = entry_row(r)?;
    if request != meta.request {
        return Err(CreditError::conflict(
            "idempotency_conflict",
            "this idempotency key was used for another request",
        ));
    }
    Ok(Some(entry))
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

pub(crate) fn check_subject(s: &str) -> Result<String> {
    let s = s.trim();
    parse_subject(s).map_err(|_| {
        CreditError::invalid(
            "invalid_subject",
            "subject must be org:<id>, user:<collection>/<id> or service:<id>",
        )
    })?;
    Ok(s.to_owned())
}

fn org_of(subject: &str) -> Option<&str> {
    parse_subject(subject).ok().flatten()
}

fn check_idem(key: &str) -> Result<String> {
    let k = key.trim();
    if k.is_empty() || k.len() > 256 {
        return Err(CreditError::invalid(
            "invalid_idempotency_key",
            "idempotency_key must be 1-256 characters",
        ));
    }
    if RESERVED_PREFIXES.iter().any(|p| k.starts_with(p)) {
        return Err(CreditError::invalid(
            "invalid_idempotency_key",
            "idempotency keys starting with usage: or expiry: are reserved",
        ));
    }
    Ok(k.to_owned())
}

fn check_amount(amount: i64) -> Result<()> {
    if (1..=MAX_AMOUNT).contains(&amount) {
        Ok(())
    } else {
        Err(CreditError::invalid(
            "invalid_amount",
            format!("amount must be between 1 and {MAX_AMOUNT}"),
        ))
    }
}

pub(crate) fn check_feature(f: &str) -> Result<String> {
    let f = f.trim();
    if f.is_empty()
        || f.len() > 128
        || !f
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':'))
    {
        return Err(CreditError::invalid(
            "invalid_feature",
            "feature keys are 1-128 letters, digits, '_', '-', '.' or ':'",
        ));
    }
    Ok(f.to_owned())
}

fn check_text(v: Option<String>, what: &str, max: usize) -> Result<Option<String>> {
    match v.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()) {
        Some(s) if s.len() > max => Err(CreditError::invalid(
            "invalid_parameter",
            format!("{what} is longer than {max} bytes"),
        )),
        v => Ok(v),
    }
}

fn check_metadata(m: Option<Json>) -> Result<Option<Json>> {
    match m {
        None | Some(Json::Null) => Ok(None),
        Some(Json::Object(o)) if Json::Object(o.clone()).to_string().len() <= 16 * 1024 => Ok(Some(Json::Object(o))),
        Some(Json::Object(_)) => Err(CreditError::invalid(
            "invalid_metadata",
            "metadata is larger than 16 KiB",
        )),
        Some(_) => Err(CreditError::invalid("invalid_metadata", "metadata must be an object")),
    }
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

impl CreditService {
    /// Opens the service on a migrated database. `usage` is the FB-24
    /// service whose usage events pay with credits.
    pub async fn open(
        db: Arc<dyn Backend>,
        policy: PolicyService,
        usage: EntitlementService,
        clock: Arc<dyn Clock>,
    ) -> Result<Self> {
        // Fails early on a database without migration 15.
        db.fetch_all(&Statement::raw("SELECT COUNT(*) FROM _forgebase_credit_cursors"))
            .await?;
        Ok(Self {
            inner: Arc::new(Inner {
                db,
                policy,
                usage,
                clock,
                jobs: OnceLock::new(),
                consumer: tokio::sync::Mutex::new(()),
            }),
        })
    }

    /// Attaches the job queue (maintenance schedule).
    pub fn attach_jobs(&self, jobs: JobService) {
        let _ = self.inner.jobs.set(jobs);
    }

    /// The policy service.
    pub fn policy(&self) -> &PolicyService {
        &self.inner.policy
    }

    pub(crate) fn db(&self) -> &dyn Backend {
        self.inner.db.as_ref()
    }

    pub(crate) fn now(&self) -> i64 {
        self.inner.clock.now_ms()
    }

    /// Commits when `res` wrote something, rolls back otherwise.
    pub(crate) async fn end<T>(&self, tx: Box<dyn WriteTx>, res: Result<(T, bool)>) -> Result<T> {
        match res {
            Ok((v, true)) => {
                tx.commit().await?;
                Ok(v)
            }
            Ok((v, false)) => {
                tx.rollback().await?;
                Ok(v)
            }
            Err(e) => {
                let _ = tx.rollback().await;
                Err(e)
            }
        }
    }

    // -----------------------------------------------------------------------
    // Authorization
    // -----------------------------------------------------------------------

    pub(crate) fn require_caller(&self, ctx: &AccessContext) -> Result<()> {
        if ctx.principal == Principal::Anonymous {
            return Err(CreditError::Unauthenticated);
        }
        Ok(())
    }

    pub(crate) fn require_admin(&self, ctx: &AccessContext) -> Result<()> {
        if ctx.is_privileged() {
            return Ok(());
        }
        self.require_caller(ctx)?;
        Err(CreditError::Forbidden("platform administrators only".into()))
    }

    /// The subject a request acts on: the given one, else the caller's (the
    /// active organization, else the caller).
    pub(crate) fn subject_for(&self, ctx: &AccessContext, given: Option<&str>) -> Result<String> {
        self.require_caller(ctx)?;
        match given {
            Some(s) => check_subject(s),
            None => check_subject(&subject_key(&ctx.subject())).map_err(|_| {
                CreditError::invalid("subject_required", "name the subject (org:<id> or a principal key)")
            }),
        }
    }

    fn check(&self, ctx: &AccessContext, permission: &str, org: Option<&str>) -> Result<()> {
        self.inner.policy.check(
            ctx,
            &Check {
                permission: permission.to_owned(),
                org: org.map(str::to_owned),
                ..Check::default()
            },
        )?;
        Ok(())
    }

    /// `permission` at realm level, for a subject of the caller's realm only
    /// (G18): an organization of that realm or an identity whose home realm
    /// it is. A realm-level grant never reaches another realm's subjects.
    fn check_realm_level(&self, ctx: &AccessContext, subject: &str, permission: &str) -> Result<()> {
        let in_realm = self
            .inner
            .policy
            .snapshot()
            .realm_of_subject(subject)
            .is_some_and(|r| r == ctx.realm_id);
        if !in_realm {
            return Err(PolicyError::Denied(Denial::at(
                Step::Tenant,
                format!("{permission}: the subject is not in realm {}", ctx.realm_id),
            ))
            .into());
        }
        self.check(ctx, permission, None)
    }

    /// `permission` in the subject's organization, else at realm level when
    /// the subject is in the caller's realm.
    fn check_for(&self, ctx: &AccessContext, subject: &str, permission: &str) -> Result<()> {
        match org_of(subject) {
            Some(org) => self
                .check(ctx, permission, Some(org))
                .or_else(|e| self.check_realm_level(ctx, subject, permission).map_err(|_| e)),
            None => self.check_realm_level(ctx, subject, permission),
        }
    }

    /// May read the subject's balance, ledger and statistics: the subject
    /// itself (personal subjects), `billing:read` in the organization or at
    /// realm level, or privileged callers.
    pub(crate) fn can_read(&self, ctx: &AccessContext, subject: &str) -> Result<()> {
        if ctx.is_privileged() {
            return Ok(());
        }
        self.require_caller(ctx)?;
        if org_of(subject).is_none() && subject == ctx.principal_key() {
            return Ok(());
        }
        self.check_for(ctx, subject, "billing:read")
    }

    /// May add credits to `subject` (top-ups, grants, refunds of usage
    /// charges) or take them back (reversals): `billing:write` at realm level
    /// (operators of the subject's realm, G18) or privileged callers. An
    /// organization's own owner or billing manager must not mint credits for
    /// it, so the org-level permission is not enough.
    pub(crate) fn can_mint(&self, ctx: &AccessContext, subject: &str) -> Result<()> {
        if ctx.is_privileged() {
            return Ok(());
        }
        self.require_caller(ctx)?;
        self.check_realm_level(ctx, subject, "billing:write")
    }

    /// May debit the subject (spend, and refund its own spends):
    /// `billing:write` or `usage:write` (trusted backends) in the subject's
    /// organization or at realm level. Never implied for the subject itself.
    pub(crate) fn can_spend(&self, ctx: &AccessContext, subject: &str) -> Result<()> {
        if ctx.is_privileged() {
            return Ok(());
        }
        self.require_caller(ctx)?;
        self.check_for(ctx, subject, "billing:write")
            .or_else(|e| self.check_for(ctx, subject, "usage:write").map_err(|_| e))
    }

    // -----------------------------------------------------------------------
    // Reads
    // -----------------------------------------------------------------------

    /// The balance of a subject (default: the caller's) and its next expiries.
    pub async fn balance(&self, ctx: &AccessContext, subject: Option<&str>) -> Result<Balance> {
        let subject = self.subject_for(ctx, subject)?;
        self.can_read(ctx, &subject)?;
        let now = self.now();
        let acct = self
            .db()
            .fetch_all(&Statement::new(
                "SELECT balance, updated_at FROM _forgebase_credit_accounts WHERE subject = ?1",
                vec![subject.clone().into()],
            ))
            .await?;
        let (stored, updated_at) = acct.rows.first().map_or((0, None), |r| (int(r, 0), opt_int(r, 1)));
        // Lapsed lots not swept yet are not spendable.
        let lapsed = scalar(
            &self
                .db()
                .fetch_all(&Statement::new(
                    "SELECT COALESCE(SUM(remaining), 0) FROM _forgebase_credit_lots
                     WHERE subject = ?1 AND remaining > 0 AND expires_at IS NOT NULL AND expires_at <= ?2",
                    vec![subject.clone().into(), now.into()],
                ))
                .await?,
        );
        let expiring = self
            .db()
            .fetch_all(&Statement::new(
                "SELECT expires_at, SUM(remaining) FROM _forgebase_credit_lots
                 WHERE subject = ?1 AND remaining > 0 AND expires_at > ?2
                 GROUP BY expires_at ORDER BY expires_at LIMIT 10",
                vec![subject.clone().into(), now.into()],
            ))
            .await?
            .rows
            .iter()
            .map(|r| Expiring {
                expires_at: int(r, 0),
                amount: int(r, 1),
            })
            .collect();
        Ok(Balance {
            subject,
            balance: (stored - lapsed).max(0),
            expiring,
            updated_at,
        })
    }

    /// The lots of a subject: open ones (credits left, not lapsed), or all.
    pub async fn lots(&self, ctx: &AccessContext, subject: Option<&str>, all: bool) -> Result<Vec<Lot>> {
        let subject = self.subject_for(ctx, subject)?;
        self.can_read(ctx, &subject)?;
        let rows = self
            .db()
            .fetch_all(&Statement::new(
                format!(
                    "SELECT {LOT_COLUMNS} FROM _forgebase_credit_lots
                     WHERE subject = ?1 AND (?2 OR (remaining > 0 AND (expires_at IS NULL OR expires_at > ?3)))
                     ORDER BY created_at, id LIMIT 1000"
                ),
                vec![subject.into(), all.into(), self.now().into()],
            ))
            .await?;
        Ok(rows.rows.iter().map(|r| lot_row(r)).collect())
    }

    /// Ledger entries in `seq` order: of a subject (default: the caller's;
    /// privileged callers may omit it to read every subject).
    pub async fn ledger(&self, ctx: &AccessContext, q: &LedgerQuery) -> Result<LedgerPage> {
        let subject = match q.subject.as_deref() {
            None if ctx.is_privileged() && q.all => None,
            s => Some(self.subject_for(ctx, s)?),
        };
        if let Some(s) = &subject {
            self.can_read(ctx, s)?;
        }
        let limit = i64::from(q.limit.unwrap_or(100).clamp(1, 1000));
        let rows = self
            .db()
            .fetch_all(&Statement::new(
                format!(
                    "SELECT {ENTRY_COLUMNS} FROM _forgebase_credit_ledger
                     WHERE seq > ?1 AND (?2 IS NULL OR subject = ?2) AND (?3 IS NULL OR kind = ?3)
                       AND (?4 IS NULL OR at >= ?4) AND (?5 IS NULL OR at < ?5)
                     ORDER BY seq LIMIT ?6"
                ),
                vec![
                    q.after.unwrap_or(0).into(),
                    subject.into(),
                    q.kind.map(EntryKind::as_str).into(),
                    q.from.into(),
                    q.to.into(),
                    (limit + 1).into(),
                ],
            ))
            .await?;
        let mut items = entries(&rows)?;
        let next = if items.len() as i64 > limit {
            items.truncate(limit as usize);
            items.last().map(|e| e.seq)
        } else {
            None
        };
        Ok(LedgerPage { items, next })
    }

    async fn load_entry(&self, id: &str) -> Result<Option<LedgerEntry>> {
        let rows = self
            .db()
            .fetch_all(&Statement::new(
                format!("SELECT {ENTRY_COLUMNS} FROM _forgebase_credit_ledger WHERE id = ?1"),
                vec![id.trim().into()],
            ))
            .await?;
        rows.rows.first().map(|r| entry_row(r).map(|(e, _)| e)).transpose()
    }

    /// One ledger entry (`404` when the caller may not read its subject).
    pub async fn entry(&self, ctx: &AccessContext, id: &str) -> Result<LedgerEntry> {
        self.require_caller(ctx)?;
        match self.load_entry(id).await? {
            Some(e) if self.can_read(ctx, &e.subject).is_ok() => Ok(e),
            _ => Err(CreditError::NotFound("ledger entry".into())),
        }
    }

    /// Checks that the account, the ledger, the lots and the allocations of
    /// a subject agree.
    pub async fn audit(&self, ctx: &AccessContext, subject: &str) -> Result<AuditReport> {
        let subject = check_subject(subject)?;
        self.can_read(ctx, &subject)?;
        let one = |sql: &str| Statement::new(sql, vec![subject.clone().into()]);
        let q = |rows: Rows| scalar(&rows);
        let db = self.db();
        let balance = q(db
            .fetch_all(&one(
                "SELECT balance FROM _forgebase_credit_accounts WHERE subject = ?1",
            ))
            .await?);
        let ledger_sum = q(db
            .fetch_all(&one(
                "SELECT COALESCE(SUM(amount), 0) FROM _forgebase_credit_ledger WHERE subject = ?1",
            ))
            .await?);
        let lots_remaining = q(db
            .fetch_all(&one(
                "SELECT COALESCE(SUM(remaining), 0) FROM _forgebase_credit_lots WHERE subject = ?1",
            ))
            .await?);
        let last_balance_after = q(db
            .fetch_all(&one(
                "SELECT balance_after FROM _forgebase_credit_ledger WHERE subject = ?1 ORDER BY seq DESC LIMIT 1",
            ))
            .await?);
        let bad_lots = q(db
            .fetch_all(&one("SELECT COUNT(*) FROM _forgebase_credit_lots l
                 WHERE l.subject = ?1 AND l.remaining <> (SELECT COALESCE(SUM(a.amount), 0)
                     FROM _forgebase_credit_allocations a WHERE a.lot_id = l.id)"))
            .await?);
        let bad_entries = q(db
            .fetch_all(&one("SELECT COUNT(*) FROM _forgebase_credit_ledger e
                 WHERE e.subject = ?1 AND e.amount <> (SELECT COALESCE(SUM(a.amount), 0)
                     FROM _forgebase_credit_allocations a WHERE a.entry_id = e.id)"))
            .await?);
        let lots_match_allocations = bad_lots == 0;
        let entries_match_allocations = bad_entries == 0;
        Ok(AuditReport {
            consistent: balance == ledger_sum
                && balance == lots_remaining
                && balance == last_balance_after
                && balance >= 0
                && lots_match_allocations
                && entries_match_allocations,
            subject,
            balance,
            ledger_sum,
            lots_remaining,
            last_balance_after,
            lots_match_allocations,
            entries_match_allocations,
        })
    }

    // -----------------------------------------------------------------------
    // Writes
    // -----------------------------------------------------------------------

    /// Adds purchased credits (operators, FB-70 through [`CreditSink`]).
    pub async fn top_up(&self, ctx: &AccessContext, input: CreditInput) -> Result<Posted> {
        self.credit(ctx, EntryKind::TopUp, input).await
    }

    /// Adds free credits, optionally lapsing at `expires_at`.
    pub async fn grant(&self, ctx: &AccessContext, input: CreditInput) -> Result<Posted> {
        self.credit(ctx, EntryKind::Grant, input).await
    }

    async fn credit(&self, ctx: &AccessContext, kind: EntryKind, input: CreditInput) -> Result<Posted> {
        if !ctx.is_privileged() {
            self.require_caller(ctx)?;
        }
        let subject = check_subject(&input.subject)?;
        self.can_mint(ctx, &subject)?;
        let idem = check_idem(&input.idempotency_key)?;
        check_amount(input.amount)?;
        let now = self.now();
        if let Some(at) = input.expires_at
            && at <= now
        {
            return Err(CreditError::invalid(
                "invalid_expiry",
                "expires_at must be in the future",
            ));
        }
        let external_ref = check_text(input.external_ref, "external_ref", 256)?;
        let request = json!({
            "op": kind.as_str(),
            "amount": input.amount,
            "expires_at": input.expires_at,
            "external_ref": external_ref,
        });
        let meta = Meta::new(
            ctx,
            idem,
            check_text(input.reason, "reason", 500)?,
            external_ref,
            check_metadata(input.metadata)?,
            request,
        );
        let mut tx = self.db().begin(TxMode::Normal).await?;
        let res = async {
            let mut t = Tx(tx.as_mut());
            if let Some(entry) = replay(&mut t, &subject, &meta).await? {
                return Ok((Posted { entry, replayed: true }, false));
            }
            sweep_tx(&mut t, &subject, now).await?;
            let entry = credit_tx(&mut t, &subject, kind, input.amount, input.expires_at, &meta, now).await?;
            Ok((Posted { entry, replayed: false }, true))
        }
        .await;
        self.end(tx, res).await
    }

    /// Debits `amount` credits at once (all or nothing; `402
    /// insufficient_credits` otherwise).
    pub async fn spend(&self, ctx: &AccessContext, input: SpendInput) -> Result<Posted> {
        let subject = self.subject_for(ctx, input.subject.as_deref())?;
        self.can_spend(ctx, &subject)?;
        let idem = check_idem(&input.idempotency_key)?;
        check_amount(input.amount)?;
        let feature = input.feature.as_deref().map(check_feature).transpose()?;
        if let Some(u) = input.units
            && u < 0
        {
            return Err(CreditError::invalid("invalid_units", "units must not be negative"));
        }
        let external_ref = check_text(input.external_ref, "external_ref", 256)?;
        let request = json!({
            "op": "spend",
            "amount": input.amount,
            "feature": feature,
            "units": input.units,
            "external_ref": external_ref,
        });
        let meta = Meta::new(
            ctx,
            idem,
            check_text(input.reason, "reason", 500)?,
            external_ref,
            check_metadata(input.metadata)?,
            request,
        );
        let now = self.now();
        let mut tx = self.db().begin(TxMode::Normal).await?;
        let res = async {
            let mut t = Tx(tx.as_mut());
            if let Some(entry) = replay(&mut t, &subject, &meta).await? {
                return Ok((Posted { entry, replayed: true }, false));
            }
            sweep_tx(&mut t, &subject, now).await?;
            let entry = debit_tx(
                &mut t,
                &subject,
                EntryKind::Spend,
                input.amount,
                Fields {
                    feature,
                    units: input.units,
                    ..Fields::default()
                },
                &meta,
                now,
            )
            .await?;
            Ok((Posted { entry, replayed: false }, true))
        }
        .await;
        self.end(tx, res).await
    }

    /// Gives back all or part of a consumption or spend, to the lots it came
    /// from (credits of lots that lapsed meanwhile lapse again at once).
    /// Spends: whoever may spend for the subject; usage charges: operators.
    pub async fn refund(&self, ctx: &AccessContext, input: AmendInput) -> Result<Posted> {
        self.require_caller(ctx)?;
        let orig = self.amend_target(ctx, &input.entry_id).await?;
        match orig.kind {
            EntryKind::Spend => self.can_spend(ctx, &orig.subject)?,
            EntryKind::Consumption => self.can_mint(ctx, &orig.subject)?,
            _ => {
                return Err(CreditError::invalid(
                    "not_refundable",
                    "only consumption and spend entries can be refunded",
                ));
            }
        }
        let idem = check_idem(&input.idempotency_key)?;
        if let Some(a) = input.amount {
            check_amount(a)?;
        }
        let external_ref = check_text(input.external_ref, "external_ref", 256)?;
        let request =
            json!({"op": "refund", "entry_id": orig.id, "amount": input.amount, "external_ref": external_ref});
        let meta = Meta::new(
            ctx,
            idem,
            check_text(input.reason, "reason", 500)?,
            external_ref,
            check_metadata(input.metadata)?,
            request,
        );
        let now = self.now();
        let mut tx = self.db().begin(TxMode::Normal).await?;
        let res = async {
            let mut t = Tx(tx.as_mut());
            if let Some(entry) = replay(&mut t, &orig.subject, &meta).await? {
                return Ok((Posted { entry, replayed: true }, false));
            }
            sweep_tx(&mut t, &orig.subject, now).await?;
            let entry = refund_tx(&mut t, &orig, input.amount, &meta, now).await?;
            sweep_tx(&mut t, &orig.subject, now).await?;
            Ok((Posted { entry, replayed: false }, true))
        }
        .await;
        self.end(tx, res).await
    }

    /// Takes back all or part of a top-up or grant (a refunded payment): at
    /// most what is left of it; the rest is reported as `unrecovered`.
    pub async fn reverse(&self, ctx: &AccessContext, input: AmendInput) -> Result<Reversal> {
        self.require_caller(ctx)?;
        let orig = self.amend_target(ctx, &input.entry_id).await?;
        self.can_mint(ctx, &orig.subject)?;
        if !matches!(orig.kind, EntryKind::TopUp | EntryKind::Grant) {
            return Err(CreditError::invalid(
                "not_reversible",
                "only top-up and grant entries can be reversed",
            ));
        }
        let idem = check_idem(&input.idempotency_key)?;
        if let Some(a) = input.amount {
            check_amount(a)?;
        }
        let external_ref = check_text(input.external_ref, "external_ref", 256)?;
        let request =
            json!({"op": "reversal", "entry_id": orig.id, "amount": input.amount, "external_ref": external_ref});
        let meta = Meta::new(
            ctx,
            idem,
            check_text(input.reason, "reason", 500)?,
            external_ref,
            check_metadata(input.metadata)?,
            request,
        );
        let now = self.now();
        let mut tx = self.db().begin(TxMode::Normal).await?;
        let res = async {
            let mut t = Tx(tx.as_mut());
            if let Some(entry) = replay(&mut t, &orig.subject, &meta).await? {
                // What was asked then: the amount given, else what was not
                // reversed before this entry.
                let before = -scalar(
                    &t.fetch(
                        "SELECT COALESCE(SUM(amount), 0) FROM _forgebase_credit_ledger
                         WHERE ref_entry = ?1 AND kind = 'reversal' AND seq < ?2",
                        vec![orig.id.clone().into(), entry.seq.into()],
                    )
                    .await?,
                );
                let reversed = -entry.amount;
                let asked = input.amount.unwrap_or(orig.amount - before);
                return Ok((
                    Reversal {
                        entry: Some(entry),
                        reversed,
                        unrecovered: (asked - reversed).max(0),
                        replayed: true,
                    },
                    false,
                ));
            }
            sweep_tx(&mut t, &orig.subject, now).await?;
            let (entry, reversed, unrecovered) = reverse_tx(&mut t, &orig, input.amount, &meta, now).await?;
            let wrote = entry.is_some();
            Ok((
                Reversal {
                    entry,
                    reversed,
                    unrecovered,
                    replayed: false,
                },
                wrote,
            ))
        }
        .await;
        self.end(tx, res).await
    }

    /// The entry a refund or reversal names; `404` when the caller may not
    /// read its subject.
    async fn amend_target(&self, ctx: &AccessContext, id: &str) -> Result<LedgerEntry> {
        match self.load_entry(id).await? {
            Some(e) if self.can_read(ctx, &e.subject).is_ok() => Ok(e),
            Some(_) | None => Err(CreditError::NotFound("ledger entry".into())),
        }
    }

    // -----------------------------------------------------------------------
    // Rates
    // -----------------------------------------------------------------------

    /// Prices of metered features in credits (any authenticated caller).
    pub async fn rates(&self, ctx: &AccessContext) -> Result<Vec<Rate>> {
        self.require_caller(ctx)?;
        let rows = self
            .db()
            .fetch_all(&Statement::raw(
                "SELECT feature_key, credits_per_unit, per_units, basis, created_at, updated_at
                 FROM _forgebase_credit_rates ORDER BY feature_key",
            ))
            .await?;
        Ok(rows.rows.iter().map(|r| rate_row(r)).collect())
    }

    /// Sets the price of a feature (platform administrators). Applies to
    /// usage events charged from now on.
    pub async fn put_rate(&self, ctx: &AccessContext, feature: &str, input: RateInput) -> Result<Rate> {
        self.require_admin(ctx)?;
        let feature = check_feature(feature)?;
        let per_units = input.per_units.unwrap_or(1);
        if !(1..=MAX_AMOUNT).contains(&input.credits_per_unit) || !(1..=MAX_AMOUNT).contains(&per_units) {
            return Err(CreditError::invalid(
                "invalid_rate",
                format!("credits_per_unit and per_units must be between 1 and {MAX_AMOUNT}"),
            ));
        }
        let basis = input.basis.unwrap_or(RateBasis::Units);
        let now = self.now();
        let mut tx = self.db().begin(TxMode::Normal).await?;
        let res = async {
            let mut t = Tx(tx.as_mut());
            t.exec(
                "INSERT INTO _forgebase_credit_rates (feature_key, credits_per_unit, per_units, basis, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5)
                 ON CONFLICT (feature_key) DO UPDATE SET credits_per_unit = excluded.credits_per_unit,
                     per_units = excluded.per_units, basis = excluded.basis, updated_at = excluded.updated_at",
                vec![
                    feature.clone().into(),
                    input.credits_per_unit.into(),
                    per_units.into(),
                    basis.as_str().into(),
                    now.into(),
                ],
            )
            .await?;
            let rows = t
                .fetch(
                    "SELECT feature_key, credits_per_unit, per_units, basis, created_at, updated_at
                     FROM _forgebase_credit_rates WHERE feature_key = ?1",
                    vec![feature.clone().into()],
                )
                .await?;
            let r = rows
                .rows
                .first()
                .map(|r| rate_row(r))
                .ok_or_else(|| CreditError::Internal("rate not stored".into()))?;
            Ok((r, true))
        }
        .await;
        self.end(tx, res).await
    }

    /// Removes the price of a feature: its usage is no longer charged.
    pub async fn delete_rate(&self, ctx: &AccessContext, feature: &str) -> Result<()> {
        self.require_admin(ctx)?;
        let feature = check_feature(feature)?;
        let mut tx = self.db().begin(TxMode::Normal).await?;
        let res = async {
            let mut t = Tx(tx.as_mut());
            let n = t
                .exec(
                    "DELETE FROM _forgebase_credit_rates WHERE feature_key = ?1",
                    vec![feature.into()],
                )
                .await?;
            if n == 0 {
                return Err(CreditError::NotFound("rate".into()));
            }
            Ok(((), true))
        }
        .await;
        self.end(tx, res).await
    }

    // -----------------------------------------------------------------------
    // Maintenance
    // -----------------------------------------------------------------------

    /// Empties every lapsed lot, writing an expiry entry for each.
    pub async fn expire_due(&self) -> Result<ExpiryReport> {
        let mut total = ExpiryReport::default();
        for _ in 0..20 {
            let now = self.now();
            let mut tx = self.db().begin(TxMode::Normal).await?;
            let res = async {
                let mut t = Tx(tx.as_mut());
                let r = sweep_batch(&mut t, None, now, SWEEP_BATCH).await?;
                let wrote = r.lots > 0;
                Ok((r, wrote))
            }
            .await;
            let r = self.end(tx, res).await?;
            total.lots += r.lots;
            total.credits += r.credits;
            if r.lots < SWEEP_BATCH {
                break;
            }
        }
        Ok(total)
    }

    /// Expires lapsed credits, then charges new usage events.
    pub async fn run_maintenance(&self) -> Result<MaintenanceReport> {
        let expired = self.expire_due().await?;
        let usage = self.consume_usage().await?;
        Ok(MaintenanceReport { expired, usage })
    }
}

fn rate_row(r: &[Value]) -> Rate {
    Rate {
        feature: text(r, 0),
        credits_per_unit: int(r, 1),
        per_units: int(r, 2),
        basis: if text(r, 3) == "overage" {
            RateBasis::Overage
        } else {
            RateBasis::Units
        },
        created_at: int(r, 4),
        updated_at: int(r, 5),
    }
}

/// Filters of [`CreditService::ledger`].
#[derive(Debug, Clone, Default)]
pub struct LedgerQuery {
    /// Subject (default: the caller's).
    pub subject: Option<String>,
    /// Privileged callers: every subject when `subject` is not given.
    pub all: bool,
    /// Only this kind.
    pub kind: Option<EntryKind>,
    /// Entries after this `seq`.
    pub after: Option<i64>,
    /// At or after (epoch ms).
    pub from: Option<i64>,
    /// Before (epoch ms).
    pub to: Option<i64>,
    /// Page size (default 100, at most 1000).
    pub limit: Option<u32>,
}
