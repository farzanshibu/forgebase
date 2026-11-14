//! Prepaid credits (`/api/v1/credits`): `client.credits()`.
//!
//! A balance per user or organization (`subject`: `org:<id>` or a principal key; default the
//! caller's) backed by an append-only ledger. A debit the balance does not cover fails with
//! `402 insufficient_credits` (an [`Error::Api`](crate::Error::Api) with `details.balance` and
//! `details.requested`). Amounts are whole credits. Times are RFC 3339 strings.

use std::sync::Arc;

use serde::Serialize;
use serde_json::{Value, json};

use crate::error::Result;
use crate::generated::{
    AuditReport, Balance, BalanceHistory, ConsumptionStats, CreditSummary, CreditsMaintenanceReport, EntryKind,
    Interval, LedgerEntry, LedgerPage, Lot, Posted, Rate, RateBasis, Reversal, UsageChargePage,
};
use crate::http::{Req, Transport, seg, wire_str};

/// Which subject a call is about.
#[derive(Debug, Clone, Default)]
pub struct SubjectOptions {
    /// `org:<id>` or a principal key (default: the caller's subject).
    pub subject: Option<String>,
}

impl SubjectOptions {
    /// Calls about `subject`.
    #[must_use]
    pub fn new(subject: impl Into<String>) -> Self {
        Self {
            subject: Some(subject.into()),
        }
    }
}

/// Options of [`CreditsApi::lots`].
#[derive(Debug, Clone, Default)]
pub struct LotsOptions {
    /// Subject (default: the caller's).
    pub subject: Option<String>,
    /// Include empty and lapsed lots.
    pub all: bool,
}

/// Options of [`CreditsApi::ledger`] and [`CreditsApi::all_entries`].
#[derive(Debug, Clone, Default)]
pub struct LedgerOptions {
    /// Subject (default: the caller's).
    pub subject: Option<String>,
    /// Platform administrators: every subject.
    pub all: bool,
    /// Only this kind of entry.
    pub kind: Option<EntryKind>,
    /// Entries after this `seq` (the `next` of the previous page).
    pub after: Option<i64>,
    /// At or after (RFC 3339).
    pub from: Option<String>,
    /// Before (RFC 3339).
    pub to: Option<String>,
    /// Page size (default 100, at most 1000).
    pub limit: Option<u32>,
}

/// Options of [`CreditsApi::usage`].
#[derive(Debug, Clone, Default)]
pub struct CreditUsageOptions {
    /// Subject (default: the caller's).
    pub subject: Option<String>,
    /// Platform administrators: every subject.
    pub all: bool,
    /// Only charges whose balance did not cover the cost.
    pub shortfall: bool,
    /// Charges after this usage event seq (the `next` of the previous page).
    pub after: Option<i64>,
    /// Page size (default 100, at most 1000).
    pub limit: Option<u32>,
}

/// Statistics window; default: the last 30 days, bucketed by day.
#[derive(Debug, Clone, Default)]
pub struct CreditWindow {
    /// Subject (default: the caller's).
    pub subject: Option<String>,
    /// Platform administrators: the whole instance.
    pub all: bool,
    /// Start (RFC 3339).
    pub from: Option<String>,
    /// End (RFC 3339).
    pub to: Option<String>,
    /// Bucket width.
    pub interval: Option<Interval>,
}

impl CreditWindow {
    fn apply(&self, r: Req, with_interval: bool) -> Req {
        let r = r
            .q_opt("subject", self.subject.as_deref())
            .q_opt("from", self.from.as_deref())
            .q_opt("to", self.to.as_deref());
        let r = if self.all { r.q("all", "true") } else { r };
        if with_interval {
            r.q_opt("interval", self.interval.as_ref().and_then(wire_str))
        } else {
            r
        }
    }
}

/// Adds credits ([`CreditsApi::top_up`]: purchased; [`CreditsApi::grant`]: free, optionally lapsing). Operators only.
#[derive(Debug, Clone, Serialize)]
pub struct CreditInput {
    /// Subject to credit.
    pub subject: String,
    /// Credits, positive.
    pub amount: i64,
    /// A replay returns the first result with `replayed: true`.
    pub idempotency_key: String,
    /// Grants: when the credits lapse (RFC 3339).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Id in the payment system or whatever caused the credit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_ref: Option<String>,
    /// Reason.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Free-form metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

impl CreditInput {
    /// `amount` credits for `subject` under `idempotency_key`.
    #[must_use]
    pub fn new(subject: impl Into<String>, amount: i64, idempotency_key: impl Into<String>) -> Self {
        Self {
            subject: subject.into(),
            amount,
            idempotency_key: idempotency_key.into(),
            expires_at: None,
            external_ref: None,
            reason: None,
            metadata: None,
        }
    }

    /// Credits lapse at `when` (RFC 3339).
    #[must_use]
    pub fn expires_at(mut self, when: impl Into<String>) -> Self {
        self.expires_at = Some(when.into());
        self
    }
}

/// Debits credits at once, all or nothing.
#[derive(Debug, Clone, Serialize)]
pub struct SpendInput {
    /// Credits to debit.
    pub amount: i64,
    /// A replay returns the first result with `replayed: true`.
    pub idempotency_key: String,
    /// Subject to debit (default: the caller's).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// Feature the spend is for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feature: Option<String>,
    /// Units of `feature` consumed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub units: Option<i64>,
    /// External reference.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_ref: Option<String>,
    /// Reason.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Free-form metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

impl SpendInput {
    /// Spends `amount` credits under `idempotency_key`.
    #[must_use]
    pub fn new(amount: i64, idempotency_key: impl Into<String>) -> Self {
        Self {
            amount,
            idempotency_key: idempotency_key.into(),
            subject: None,
            feature: None,
            units: None,
            external_ref: None,
            reason: None,
            metadata: None,
        }
    }
}

/// Refund or reversal of an earlier entry.
#[derive(Debug, Clone, Serialize)]
pub struct AmendInput {
    /// The consumption / spend (refund) or top-up / grant (reversal) to amend.
    pub entry_id: String,
    /// A replay returns the first result.
    pub idempotency_key: String,
    /// Credits; default: all that is left to give back or take back.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amount: Option<i64>,
    /// External reference.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_ref: Option<String>,
    /// Reason.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Free-form metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

impl AmendInput {
    /// Amends `entry_id` entirely.
    #[must_use]
    pub fn new(entry_id: impl Into<String>, idempotency_key: impl Into<String>) -> Self {
        Self {
            entry_id: entry_id.into(),
            idempotency_key: idempotency_key.into(),
            amount: None,
            external_ref: None,
            reason: None,
            metadata: None,
        }
    }
}

/// The price of a feature in credits ([`CreditRatesApi::set`]).
#[derive(Debug, Clone, Serialize)]
pub struct RateInput {
    /// Credits charged per `per_units` units.
    pub credits_per_unit: i64,
    /// Units the price covers (default 1).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub per_units: Option<i64>,
    /// `units` (default): every unit used is charged; `overage`: only units past the feature's soft limit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub basis: Option<RateBasis>,
}

impl RateInput {
    /// `credits_per_unit` credits per unit.
    #[must_use]
    pub fn new(credits_per_unit: i64) -> Self {
        Self {
            credits_per_unit,
            per_units: None,
            basis: None,
        }
    }
}

/// Credits helpers: `client.credits()`.
#[derive(Clone)]
pub struct CreditsApi {
    pub(crate) t: Arc<Transport>,
}

impl CreditsApi {
    /// Prices of metered features in credits.
    #[must_use]
    pub fn rates(&self) -> CreditRatesApi {
        CreditRatesApi { t: self.t.clone() }
    }

    /// Position of the usage-event consumer.
    #[must_use]
    pub fn usage_cursor(&self) -> CreditUsageCursorApi {
        CreditUsageCursorApi { t: self.t.clone() }
    }

    /// Statistics.
    #[must_use]
    pub fn stats(&self) -> CreditStatsApi {
        CreditStatsApi { t: self.t.clone() }
    }

    /// Balance and next expiries of a subject (the caller by default).
    pub async fn balance(&self, subject: Option<&str>) -> Result<Balance> {
        self.t
            .json(Req::get("/api/v1/credits/balance").q_opt("subject", subject))
            .await
    }

    /// What is left of each top-up and grant, oldest first.
    pub async fn lots(&self, o: &LotsOptions) -> Result<Vec<Lot>> {
        let r = Req::get("/api/v1/credits/lots").q_opt("subject", o.subject.as_deref());
        let r = if o.all { r.q("all", "true") } else { r };
        self.t.items(r).await
    }

    /// One page of ledger entries in `seq` order; pass `next` back as `after` for the following page.
    pub async fn ledger(&self, o: &LedgerOptions) -> Result<LedgerPage> {
        let r = Req::get("/api/v1/credits/ledger")
            .q_opt("subject", o.subject.as_deref())
            .q_opt("kind", o.kind.as_ref().and_then(wire_str))
            .q_opt("after", o.after)
            .q_opt("from", o.from.as_deref())
            .q_opt("to", o.to.as_deref())
            .q_opt("limit", o.limit);
        let r = if o.all { r.q("all", "true") } else { r };
        self.t.json(r).await
    }

    /// Every ledger entry matching `o`, page after page, collected.
    pub async fn all_entries(&self, o: &LedgerOptions) -> Result<Vec<LedgerEntry>> {
        let mut o = o.clone();
        let mut out = Vec::new();
        loop {
            let page = self.ledger(&o).await?;
            let empty = page.items.is_empty();
            out.extend(page.items);
            match page.next {
                Some(n) if !empty => o.after = Some(n),
                _ => return Ok(out),
            }
        }
    }

    /// One ledger entry.
    pub async fn entry(&self, id: &str) -> Result<LedgerEntry> {
        self.t
            .json(Req::get(format!("/api/v1/credits/ledger/{}", seg(id))))
            .await
    }

    /// Checks that the balance, ledger, lots and their allocations agree.
    pub async fn audit(&self, subject: Option<&str>) -> Result<AuditReport> {
        self.t
            .json(Req::get("/api/v1/credits/audit").q_opt("subject", subject))
            .await
    }

    /// Adds purchased credits (operators). Idempotent by key.
    pub async fn top_up(&self, i: &CreditInput) -> Result<Posted> {
        self.t
            .json(Req::post("/api/v1/credits/top-ups").json(i)?.idempotent())
            .await
    }

    /// Adds free credits, optionally lapsing at `expires_at` (operators). Idempotent by key.
    pub async fn grant(&self, i: &CreditInput) -> Result<Posted> {
        self.t
            .json(Req::post("/api/v1/credits/grants").json(i)?.idempotent())
            .await
    }

    /// Debits credits, all or nothing. A balance that does not cover it is an API error with
    /// status 402 (`insufficient_credits`).
    pub async fn spend(&self, i: &SpendInput) -> Result<Posted> {
        self.t
            .json(Req::post("/api/v1/credits/spend").json(i)?.idempotent())
            .await
    }

    /// Gives back all or part of a consumption or spend.
    pub async fn refund(&self, i: &AmendInput) -> Result<Posted> {
        self.t
            .json(Req::post("/api/v1/credits/refunds").json(i)?.idempotent())
            .await
    }

    /// Takes back what is left of a top-up or grant (operators). Nothing left is not an error:
    /// `reversed` is 0.
    pub async fn reverse(&self, i: &AmendInput) -> Result<Reversal> {
        self.t
            .json(Req::post("/api/v1/credits/reversals").json(i)?.idempotent())
            .await
    }

    /// One page of usage events charged in credits, with the shortfall the balance did not cover.
    pub async fn usage(&self, o: &CreditUsageOptions) -> Result<UsageChargePage> {
        let r = Req::get("/api/v1/credits/usage")
            .q_opt("subject", o.subject.as_deref())
            .q_opt("after", o.after)
            .q_opt("limit", o.limit);
        let r = if o.all { r.q("all", "true") } else { r };
        let r = if o.shortfall { r.q("shortfall", "true") } else { r };
        self.t.json(r).await
    }

    /// Runs expiry and usage consumption now (platform administrators); the server also does it every minute.
    pub async fn maintenance(&self) -> Result<CreditsMaintenanceReport> {
        self.t
            .json(Req::post("/api/v1/credits/maintenance").json_raw(json!({})))
            .await
    }
}

/// Prices of metered features in credits: `client.credits().rates()`.
#[derive(Clone)]
pub struct CreditRatesApi {
    t: Arc<Transport>,
}

impl CreditRatesApi {
    /// Every price.
    pub async fn list(&self) -> Result<Vec<Rate>> {
        self.t.items(Req::get("/api/v1/credits/rates")).await
    }

    /// Sets the price of a feature (platform administrators).
    pub async fn set(&self, feature: &str, rate: &RateInput) -> Result<Rate> {
        self.t
            .json(Req::put(format!("/api/v1/credits/rates/{}", seg(feature))).json(rate)?)
            .await
    }

    /// Removes the price of a feature (platform administrators).
    pub async fn delete(&self, feature: &str) -> Result<()> {
        self.t
            .unit(Req::delete(format!("/api/v1/credits/rates/{}", seg(feature))))
            .await
    }
}

/// Position of the consumer that charges usage events in credits: `client.credits().usage_cursor()`.
#[derive(Clone)]
pub struct CreditUsageCursorApi {
    t: Arc<Transport>,
}

impl CreditUsageCursorApi {
    /// The usage event seq charged so far (platform administrators).
    pub async fn get(&self) -> Result<i64> {
        let c: crate::generated::Cursor = self.t.json(Req::get("/api/v1/credits/usage/cursor")).await?;
        Ok(c.seq)
    }

    /// Moves the consumer (platform administrators). Events already charged are never charged again.
    pub async fn set(&self, seq: i64) -> Result<i64> {
        let c: crate::generated::Cursor = self
            .t
            .json(Req::put("/api/v1/credits/usage/cursor").json_raw(json!({"seq": seq})))
            .await?;
        Ok(c.seq)
    }
}

/// Statistics: `client.credits().stats()`. Windows default to the last 30 days, bucketed by day.
#[derive(Clone)]
pub struct CreditStatsApi {
    t: Arc<Transport>,
}

impl CreditStatsApi {
    /// Opening balance, then per bucket the credits added, removed and the closing balance.
    pub async fn balance(&self, w: &CreditWindow) -> Result<BalanceHistory> {
        self.t
            .json(w.apply(Req::get("/api/v1/credits/stats/balance"), true))
            .await
    }

    /// Credits consumed by feature and period, net of refunds.
    pub async fn consumption(&self, w: &CreditWindow) -> Result<ConsumptionStats> {
        self.t
            .json(w.apply(Req::get("/api/v1/credits/stats/consumption"), true))
            .await
    }

    /// Totals by entry kind, the balance and the usage shortfall.
    pub async fn summary(&self, w: &CreditWindow) -> Result<CreditSummary> {
        self.t
            .json(w.apply(Req::get("/api/v1/credits/stats/summary"), false))
            .await
    }
}
