//! Public types of the credit API (Rust and REST).
//!
//! Times are epoch milliseconds in Rust and RFC 3339 strings on the wire;
//! amounts are whole credits.

use forgebase_entitlements::model::{ts, ts_opt};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use utoipa::ToSchema;

/// What a ledger entry records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    /// Purchased credits (FB-70 or an operator).
    TopUp,
    /// Free credits (promotion, plan allowance, goodwill).
    Grant,
    /// Credits charged for an FB-24 usage event.
    Consumption,
    /// Credits debited directly by a trusted backend.
    Spend,
    /// Credits given back for a consumption or spend.
    Refund,
    /// Credits of a top-up or grant taken back (payment refunded).
    Reversal,
    /// Credits of a top-up or grant that lapsed.
    Expiry,
}

impl EntryKind {
    /// Storage form.
    pub fn as_str(self) -> &'static str {
        match self {
            EntryKind::TopUp => "top_up",
            EntryKind::Grant => "grant",
            EntryKind::Consumption => "consumption",
            EntryKind::Spend => "spend",
            EntryKind::Refund => "refund",
            EntryKind::Reversal => "reversal",
            EntryKind::Expiry => "expiry",
        }
    }

    /// Parses the storage form.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "top_up" => EntryKind::TopUp,
            "grant" => EntryKind::Grant,
            "consumption" => EntryKind::Consumption,
            "spend" => EntryKind::Spend,
            "refund" => EntryKind::Refund,
            "reversal" => EntryKind::Reversal,
            "expiry" => EntryKind::Expiry,
            _ => return None,
        })
    }

    /// Every kind.
    pub const ALL: [EntryKind; 7] = [
        EntryKind::TopUp,
        EntryKind::Grant,
        EntryKind::Consumption,
        EntryKind::Spend,
        EntryKind::Refund,
        EntryKind::Reversal,
        EntryKind::Expiry,
    ];
}

/// One immutable ledger row.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct LedgerEntry {
    /// Cursor (increasing in commit order).
    pub seq: i64,
    /// Id.
    pub id: String,
    /// When.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub at: i64,
    /// Subject (`org:<id>` or a principal key).
    pub subject: String,
    /// What it records.
    pub kind: EntryKind,
    /// Credits added (positive) or removed (negative).
    pub amount: i64,
    /// Balance right after the entry.
    pub balance_after: i64,
    /// Top-up and grant: the lot created; reversal and expiry: the lot taken from.
    pub lot_id: Option<String>,
    /// Refund: the debit given back; reversal: the top-up or grant taken back.
    pub ref_entry: Option<String>,
    /// Consumption and spend (and their refunds): the feature paid for.
    pub feature: Option<String>,
    /// Units paid for.
    pub units: Option<i64>,
    /// Consumption: the FB-24 usage event charged.
    pub usage_event_id: Option<String>,
    /// Consumption: the usage event's `seq`.
    pub usage_seq: Option<i64>,
    /// Top-up and grant: when the credits lapse.
    #[serde(with = "ts_opt")]
    #[schema(value_type = Option<String>)]
    pub expires_at: Option<i64>,
    /// Idempotency key (unique per subject).
    pub idempotency_key: String,
    /// Who recorded it.
    pub actor: String,
    /// Why.
    pub reason: Option<String>,
    /// Order, payment or invoice reference (FB-70).
    pub external_ref: Option<String>,
    /// Request id.
    pub request_id: Option<String>,
    /// Caller data.
    #[schema(value_type = Object)]
    pub metadata: Json,
}

/// Result of a ledger write.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Posted {
    /// The entry.
    pub entry: LedgerEntry,
    /// The idempotency key was seen before: this is the first result.
    pub replayed: bool,
}

/// A page of ledger entries.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct LedgerPage {
    /// Oldest first.
    pub items: Vec<LedgerEntry>,
    /// Pass as `after` for the next page (`null`: no more).
    pub next: Option<i64>,
}

/// What is left of one top-up or grant.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Lot {
    /// Id.
    pub id: String,
    /// Subject.
    pub subject: String,
    /// The top-up or grant entry.
    pub entry_id: String,
    /// `top_up` or `grant`.
    pub kind: EntryKind,
    /// Credits it started with.
    pub amount: i64,
    /// Credits left.
    pub remaining: i64,
    /// When it lapses.
    #[serde(with = "ts_opt")]
    #[schema(value_type = Option<String>)]
    pub expires_at: Option<i64>,
    /// Created.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub created_at: i64,
}

/// Credits lapsing at one time.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Expiring {
    /// When.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub expires_at: i64,
    /// Credits.
    pub amount: i64,
}

/// A subject's balance.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Balance {
    /// Subject.
    pub subject: String,
    /// Credits available.
    pub balance: i64,
    /// Next expiries (soonest first, at most 10).
    pub expiring: Vec<Expiring>,
    /// Last change.
    #[serde(with = "ts_opt")]
    #[schema(value_type = Option<String>)]
    pub updated_at: Option<i64>,
}

/// Body of a top-up or grant.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreditInput {
    /// Subject (`org:<id>` or a principal key).
    pub subject: String,
    /// Credits (at least 1).
    pub amount: i64,
    /// When the credits lapse (default: never).
    #[serde(default, with = "ts_opt")]
    #[schema(value_type = Option<String>)]
    pub expires_at: Option<i64>,
    /// Idempotency key (unique per subject; FB-70: the payment or event id).
    pub idempotency_key: String,
    /// Why.
    #[serde(default)]
    pub reason: Option<String>,
    /// Order, payment or invoice reference.
    #[serde(default)]
    pub external_ref: Option<String>,
    /// Caller data.
    #[serde(default)]
    #[schema(value_type = Option<Object>)]
    pub metadata: Option<Json>,
}

impl CreditInput {
    /// `amount` credits for `subject`, never lapsing.
    pub fn new(subject: impl Into<String>, amount: i64, key: impl Into<String>) -> Self {
        Self {
            subject: subject.into(),
            amount,
            idempotency_key: key.into(),
            ..Self::default()
        }
    }

    /// Lapses at `ms`.
    pub fn expiring(mut self, ms: i64) -> Self {
        self.expires_at = Some(ms);
        self
    }
}

/// Body of a direct debit.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SpendInput {
    /// Subject (default: the caller's).
    #[serde(default)]
    pub subject: Option<String>,
    /// Credits (at least 1). All or nothing.
    pub amount: i64,
    /// Feature paid for (statistics).
    #[serde(default)]
    pub feature: Option<String>,
    /// Units paid for (statistics).
    #[serde(default)]
    pub units: Option<i64>,
    /// Idempotency key (unique per subject).
    pub idempotency_key: String,
    /// Why.
    #[serde(default)]
    pub reason: Option<String>,
    /// External reference.
    #[serde(default)]
    pub external_ref: Option<String>,
    /// Caller data.
    #[serde(default)]
    #[schema(value_type = Option<Object>)]
    pub metadata: Option<Json>,
}

impl SpendInput {
    /// Spends `amount` credits of `subject`.
    pub fn new(subject: impl Into<String>, amount: i64, key: impl Into<String>) -> Self {
        Self {
            subject: Some(subject.into()),
            amount,
            idempotency_key: key.into(),
            ..Self::default()
        }
    }

    /// For `feature`.
    pub fn for_feature(mut self, feature: impl Into<String>, units: i64) -> Self {
        self.feature = Some(feature.into());
        self.units = Some(units);
        self
    }
}

/// Body of a refund (of a consumption or spend) or a reversal (of a top-up
/// or grant).
#[derive(Debug, Clone, Default, PartialEq, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AmendInput {
    /// The entry refunded or reversed.
    pub entry_id: String,
    /// Credits (default: all that is left to refund or reverse).
    #[serde(default)]
    pub amount: Option<i64>,
    /// Idempotency key (unique per subject).
    pub idempotency_key: String,
    /// Why.
    #[serde(default)]
    pub reason: Option<String>,
    /// External reference (FB-70: refund or credit note).
    #[serde(default)]
    pub external_ref: Option<String>,
    /// Caller data.
    #[serde(default)]
    #[schema(value_type = Option<Object>)]
    pub metadata: Option<Json>,
}

impl AmendInput {
    /// All of `entry_id`.
    pub fn new(entry_id: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            entry_id: entry_id.into(),
            idempotency_key: key.into(),
            ..Self::default()
        }
    }

    /// Only `amount` credits.
    pub fn amount(mut self, amount: i64) -> Self {
        self.amount = Some(amount);
        self
    }
}

/// Result of a reversal: takes back what is left of the top-up or grant, up
/// to the amount asked; the rest was already spent (the balance never goes
/// negative).
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Reversal {
    /// The reversal entry (`null`: nothing was left to take back).
    pub entry: Option<LedgerEntry>,
    /// Credits taken back.
    pub reversed: i64,
    /// Credits asked for but already spent or lapsed.
    pub unrecovered: i64,
    /// The idempotency key was seen before.
    pub replayed: bool,
}

/// FB-70 reversing a purchase it credited with [`crate::CreditSink::credit_purchase`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PurchaseReversal {
    /// Subject credited.
    pub subject: String,
    /// The idempotency key the purchase was credited with.
    pub purchase_key: String,
    /// Credits (default: all that is left of the purchase).
    #[serde(default)]
    pub amount: Option<i64>,
    /// Idempotency key of the reversal.
    pub idempotency_key: String,
    /// Why.
    #[serde(default)]
    pub reason: Option<String>,
    /// Refund or credit note reference.
    #[serde(default)]
    pub external_ref: Option<String>,
}

/// What usage of a feature costs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RateBasis {
    /// Every unit used.
    Units,
    /// Only units past a soft limit (FB-24 `overage_units`).
    Overage,
}

impl RateBasis {
    /// Storage form.
    pub fn as_str(self) -> &'static str {
        match self {
            RateBasis::Units => "units",
            RateBasis::Overage => "overage",
        }
    }
}

/// The price of a metered feature in credits.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Rate {
    /// Feature key.
    pub feature: String,
    /// Credits per `per_units` units.
    pub credits_per_unit: i64,
    /// Units the price is for (cost per event: `ceil(units × credits_per_unit / per_units)`).
    pub per_units: i64,
    /// What is charged.
    pub basis: RateBasis,
    /// Created.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub created_at: i64,
    /// Updated.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub updated_at: i64,
}

/// Body of `PUT /credits/rates/{feature}`.
#[derive(Debug, Clone, PartialEq, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RateInput {
    /// Credits per `per_units` units (at least 1).
    pub credits_per_unit: i64,
    /// Units the price is for (default 1).
    #[serde(default)]
    pub per_units: Option<i64>,
    /// What is charged (default `units`).
    #[serde(default)]
    pub basis: Option<RateBasis>,
}

/// One FB-24 usage event charged in credits.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct UsageCharge {
    /// Usage event id.
    pub event_id: String,
    /// Usage event `seq`.
    pub event_seq: i64,
    /// When the usage happened.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub event_at: i64,
    /// When it was charged.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub at: i64,
    /// Subject.
    pub subject: String,
    /// Feature.
    pub feature: String,
    /// Units priced.
    pub units: i64,
    /// Credits per `per_units` units applied.
    pub rate: i64,
    /// Units the rate is for.
    pub per_units: i64,
    /// Credits the usage cost.
    pub cost: i64,
    /// Credits debited.
    pub charged: i64,
    /// Credits the balance could not cover (FB-70 may bill them).
    pub shortfall: i64,
    /// The consumption entry (`null` when nothing could be charged).
    pub entry_id: Option<String>,
}

/// A page of usage charges.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct UsageChargePage {
    /// Oldest first.
    pub items: Vec<UsageCharge>,
    /// Pass as `after` for the next page (`null`: no more).
    pub next: Option<i64>,
}

/// What one run of the usage consumer did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, ToSchema)]
pub struct UsageRun {
    /// Usage events read.
    pub events_read: i64,
    /// Events charged (priced and not charged before).
    pub events_charged: i64,
    /// Credits debited.
    pub credits_charged: i64,
    /// Credits not covered by balances.
    pub shortfall: i64,
    /// Cursor after the run (last usage event `seq` read).
    pub cursor: i64,
}

/// What an expiry run did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, ToSchema)]
pub struct ExpiryReport {
    /// Lots emptied.
    pub lots: i64,
    /// Credits lapsed.
    pub credits: i64,
}

/// What a maintenance run did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, ToSchema)]
#[schema(as = CreditsMaintenanceReport)]
pub struct MaintenanceReport {
    /// Expiries.
    pub expired: ExpiryReport,
    /// Usage consumption.
    pub usage: UsageRun,
}

/// The consumer cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Cursor {
    /// Last usage event `seq` read.
    pub seq: i64,
}

/// Consistency of one account: the balance, the ledger and the lots agree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct AuditReport {
    /// Subject.
    pub subject: String,
    /// Balance on the account.
    pub balance: i64,
    /// Sum of the ledger.
    pub ledger_sum: i64,
    /// Sum of what is left in the lots.
    pub lots_remaining: i64,
    /// `balance_after` of the newest entry.
    pub last_balance_after: i64,
    /// Every lot's remaining equals the sum of its allocations.
    pub lots_match_allocations: bool,
    /// Every entry's amount equals the sum of its allocations.
    pub entries_match_allocations: bool,
    /// All of the above agree.
    pub consistent: bool,
}

/// Bucket width of statistics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Interval {
    /// UTC hours.
    Hour,
    /// UTC days.
    Day,
    /// Calendar months (UTC).
    Month,
}

impl Interval {
    /// Parses `hour`, `day` or `month`.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "hour" => Interval::Hour,
            "day" => Interval::Day,
            "month" => Interval::Month,
            _ => return None,
        })
    }
}

/// One bucket of a balance history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct BalancePoint {
    /// Bucket start.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub start: i64,
    /// Balance at the end of the bucket.
    pub closing: i64,
    /// Credits added in the bucket.
    pub credited: i64,
    /// Credits removed in the bucket.
    pub debited: i64,
}

/// Balance over time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct BalanceHistory {
    /// Subject.
    pub subject: String,
    /// Bucket width.
    pub interval: Interval,
    /// Start (inclusive).
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub from: i64,
    /// End (exclusive).
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub to: i64,
    /// Balance at `from`.
    pub opening: i64,
    /// Every bucket, oldest first.
    pub points: Vec<BalancePoint>,
}

/// Credits consumed for one feature in one bucket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ConsumptionRow {
    /// Bucket start.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub start: i64,
    /// Feature (`""`: spends without a feature).
    pub feature: String,
    /// Credits debited (consumption and spend).
    pub debited: i64,
    /// Credits refunded.
    pub refunded: i64,
    /// `debited - refunded`.
    pub net: i64,
    /// Units paid for.
    pub units: i64,
    /// Debit entries.
    pub entries: i64,
}

/// Credit consumption by feature and period.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ConsumptionStats {
    /// Subject (`null`: the whole instance).
    pub subject: Option<String>,
    /// Bucket width.
    pub interval: Interval,
    /// Start (inclusive).
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub from: i64,
    /// End (exclusive).
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub to: i64,
    /// Buckets with consumption, oldest first, then by feature.
    pub rows: Vec<ConsumptionRow>,
    /// Sum of `net`.
    pub total_net: i64,
}

/// Totals by entry kind over a period.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct CreditSummary {
    /// Subject (`null`: the whole instance).
    pub subject: Option<String>,
    /// Start (inclusive).
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub from: i64,
    /// End (exclusive).
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub to: i64,
    /// Credits topped up.
    pub top_up: i64,
    /// Credits granted.
    pub grant: i64,
    /// Credits charged for usage.
    pub consumption: i64,
    /// Credits spent directly.
    pub spend: i64,
    /// Credits refunded.
    pub refund: i64,
    /// Credits reversed.
    pub reversal: i64,
    /// Credits lapsed.
    pub expiry: i64,
    /// Net change in the period.
    pub net: i64,
    /// Balance now (the subject's, or of every account).
    pub balance: i64,
    /// Usage cost not covered by balances in the period.
    pub shortfall: i64,
}
