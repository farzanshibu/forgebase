//! Public types of the entitlement and quota API (Rust and REST).
//!
//! Times are epoch milliseconds in Rust and RFC 3339 strings on the wire.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use utoipa::ToSchema;

use crate::period::Period;

/// RFC 3339 (UTC, milliseconds) for epoch milliseconds.
pub fn iso(ms: i64) -> String {
    let t = time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute(),
        t.second(),
        t.millisecond()
    )
}

/// Epoch milliseconds of an RFC 3339 string.
pub fn parse_iso(s: &str) -> Option<i64> {
    let t = time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok()?;
    i64::try_from(t.unix_timestamp_nanos() / 1_000_000).ok()
}

/// Serde for epoch milliseconds as RFC 3339.
pub mod ts {
    use serde::{Deserialize, Deserializer, Serializer};

    /// Serialises.
    pub fn serialize<S: Serializer>(ms: &i64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&super::iso(*ms))
    }

    /// Deserialises.
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
        let text = String::deserialize(d)?;
        super::parse_iso(&text).ok_or_else(|| serde::de::Error::custom("expected an RFC 3339 time"))
    }
}

/// Serde for optional epoch milliseconds as RFC 3339.
pub mod ts_opt {
    use serde::{Deserialize, Deserializer, Serializer};

    /// Serialises.
    pub fn serialize<S: Serializer>(ms: &Option<i64>, s: S) -> Result<S::Ok, S::Error> {
        match ms {
            Some(ms) => s.serialize_str(&super::iso(*ms)),
            None => s.serialize_none(),
        }
    }

    /// Deserialises.
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
        match Option::<String>::deserialize(d)? {
            None => Ok(None),
            Some(text) => super::parse_iso(&text)
                .map(Some)
                .ok_or_else(|| serde::de::Error::custom("expected an RFC 3339 time")),
        }
    }
}

// ---------------------------------------------------------------------------
// Catalog
// ---------------------------------------------------------------------------

/// What a feature measures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FeatureKind {
    /// On or off.
    Boolean,
    /// Counted per period (API calls, AI generations, exports).
    Metered,
    /// A level that goes up and down (storage bytes, seats).
    Gauge,
    /// Simultaneous leases (running jobs).
    Concurrency,
    /// API requests per hour or day, enforced in the HTTP layer.
    Rate,
}

impl FeatureKind {
    /// Stored name.
    pub fn as_str(self) -> &'static str {
        match self {
            FeatureKind::Boolean => "boolean",
            FeatureKind::Metered => "metered",
            FeatureKind::Gauge => "gauge",
            FeatureKind::Concurrency => "concurrency",
            FeatureKind::Rate => "rate",
        }
    }

    /// Parses a stored name.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "boolean" => FeatureKind::Boolean,
            "metered" => FeatureKind::Metered,
            "gauge" => FeatureKind::Gauge,
            "concurrency" => FeatureKind::Concurrency,
            "rate" => FeatureKind::Rate,
            _ => return None,
        })
    }
}

/// A feature of the catalog.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Feature {
    /// Key (`ai_generation`, `storage`, `api_access`).
    pub key: String,
    /// Display name.
    pub name: String,
    /// Description.
    pub description: String,
    /// What it measures.
    pub kind: FeatureKind,
    /// Unit shown with limits (`requests`, `bytes`).
    pub unit: String,
    /// Permission patterns this feature gates (pipeline step 4). A key
    /// `org:<feature>:<permission>` is gated by feature `<feature>` without
    /// being listed.
    pub permissions: Vec<String>,
    /// Created.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub created_at: i64,
    /// Last change.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub updated_at: i64,
}

/// Body of `PUT /features/{key}`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct FeatureInput {
    /// Display name (default: the key).
    #[serde(default)]
    pub name: Option<String>,
    /// Description.
    #[serde(default)]
    pub description: Option<String>,
    /// What it measures.
    pub kind: FeatureKind,
    /// Unit.
    #[serde(default)]
    pub unit: Option<String>,
    /// Gated permission patterns.
    #[serde(default)]
    pub permissions: Vec<String>,
}

/// A feature in a plan, an add-on, a custom limit or an override.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanFeature {
    /// Feature key.
    pub feature: String,
    /// Included (default true).
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Limit per period; `null` = unlimited.
    #[serde(default)]
    pub limit: Option<i64>,
    /// Unit shown with the limit (default: the feature's).
    #[serde(default)]
    pub unit: String,
    /// Window of the limit (metered: `hour`, `day`, `month`, `lifetime`,
    /// `rolling`; rate: `hour` or `day`). Default `month` for metered
    /// features, `none` otherwise.
    #[serde(default)]
    pub period: Option<Period>,
    /// Length of a `rolling` window in seconds.
    #[serde(default)]
    pub window_secs: Option<i64>,
    /// Allowance while the subscription is trialing (replaces `limit`).
    #[serde(default)]
    pub trial_limit: Option<i64>,
}

pub(crate) fn yes() -> bool {
    true
}

/// A plan.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Plan {
    /// Key (`free`, `pro`).
    pub key: String,
    /// Display name.
    pub name: String,
    /// Description.
    pub description: String,
    /// The plan of subjects without an entitled subscription.
    pub is_default: bool,
    /// Can be assigned.
    pub active: bool,
    /// Trial length offered with this plan.
    pub trial_days: i64,
    /// Free-form data (prices live in FB-70).
    #[schema(value_type = Object)]
    pub metadata: Json,
    /// Features.
    pub features: Vec<PlanFeature>,
    /// Created.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub created_at: i64,
    /// Last change.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub updated_at: i64,
}

/// Body of `PUT /plans/{key}`.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanInput {
    /// Display name (default: the key).
    #[serde(default)]
    pub name: Option<String>,
    /// Description.
    #[serde(default)]
    pub description: Option<String>,
    /// Make this the default plan (unsets the previous default).
    #[serde(default)]
    pub is_default: bool,
    /// Can be assigned (default true).
    #[serde(default = "yes")]
    pub active: bool,
    /// Trial length.
    #[serde(default)]
    pub trial_days: i64,
    /// Free-form data.
    #[serde(default)]
    #[schema(value_type = Object)]
    pub metadata: Option<Json>,
    /// The plan's features (replaces the set).
    #[serde(default)]
    pub features: Vec<PlanFeature>,
}

/// What one unit of an add-on grants.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AddonFeature {
    /// Feature key.
    pub feature: String,
    /// Units added per add-on quantity (`null`: unlimited when the add-on is
    /// what enables the feature, no change otherwise).
    #[serde(default)]
    pub limit: Option<i64>,
    /// Window when the add-on enables the feature.
    #[serde(default)]
    pub period: Option<Period>,
    /// Rolling window length.
    #[serde(default)]
    pub window_secs: Option<i64>,
}

/// An add-on.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Addon {
    /// Key.
    pub key: String,
    /// Display name.
    pub name: String,
    /// Description.
    pub description: String,
    /// Can be assigned.
    pub active: bool,
    /// Grants.
    pub features: Vec<AddonFeature>,
}

/// Body of `PUT /addons/{key}`.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AddonInput {
    /// Display name.
    #[serde(default)]
    pub name: Option<String>,
    /// Description.
    #[serde(default)]
    pub description: Option<String>,
    /// Can be assigned (default true).
    #[serde(default = "yes")]
    pub active: bool,
    /// Grants (replaces the set).
    #[serde(default)]
    pub features: Vec<AddonFeature>,
}

/// A usage pack of the catalog.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct UsagePack {
    /// Key.
    pub key: String,
    /// Display name.
    pub name: String,
    /// Metered feature it adds units to.
    pub feature: String,
    /// Units per pack.
    pub units: i64,
    /// Validity after purchase (`null`: no expiry).
    pub valid_days: Option<i64>,
    /// Can be granted.
    pub active: bool,
}

/// Body of `PUT /usage-packs/{key}`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UsagePackInput {
    /// Display name.
    #[serde(default)]
    pub name: Option<String>,
    /// Metered feature.
    pub feature: String,
    /// Units per pack.
    pub units: i64,
    /// Validity in days.
    #[serde(default)]
    pub valid_days: Option<i64>,
    /// Can be granted (default true).
    #[serde(default = "yes")]
    pub active: bool,
}

/// Which counter a quota uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum QuotaScope {
    /// One counter per billing subject (the organization or the user).
    #[default]
    Subject,
    /// One counter per member of the subject.
    User,
    /// One counter per team (scope key = team id).
    Team,
    /// One counter per resource (scope key = `kind:id`).
    Resource,
}

impl QuotaScope {
    /// Stored name.
    pub fn as_str(self) -> &'static str {
        match self {
            QuotaScope::Subject => "subject",
            QuotaScope::User => "user",
            QuotaScope::Team => "team",
            QuotaScope::Resource => "resource",
        }
    }

    /// Parses a stored name.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "subject" => QuotaScope::Subject,
            "user" => QuotaScope::User,
            "team" => QuotaScope::Team,
            "resource" => QuotaScope::Resource,
            _ => return None,
        })
    }
}

/// Hard or soft limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Enforcement {
    /// Deny at the limit.
    #[default]
    Hard,
    /// Allow past the limit, record overage (up to `overage_pct` when set).
    Soft,
}

/// Enforcement of a feature's limit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct QuotaPolicy {
    /// Feature key (set from the path).
    #[serde(default)]
    pub feature: String,
    /// Counter scope.
    #[serde(default)]
    pub scope: QuotaScope,
    /// Hard or soft.
    #[serde(default)]
    pub enforcement: Enforcement,
    /// Soft limits: how far past the limit usage may go, in percent (`null`:
    /// no ceiling).
    #[serde(default)]
    pub overage_pct: Option<i64>,
    /// Calendar periods: share of the limit that may roll over unused, 0-100.
    #[serde(default)]
    pub rollover_pct: i64,
    /// Most units that may roll over.
    #[serde(default)]
    pub rollover_cap: Option<i64>,
    /// Units one admitted operation needs (pipeline step 6).
    #[serde(default = "one")]
    pub cost: i64,
    /// Default lease length for concurrency features.
    #[serde(default = "five_minutes")]
    pub lease_ttl_secs: i64,
    /// Permission patterns pipeline step 6 admits against this quota.
    #[serde(default)]
    pub permissions: Vec<String>,
}

fn one() -> i64 {
    1
}

fn five_minutes() -> i64 {
    300
}

impl Default for QuotaPolicy {
    fn default() -> Self {
        Self {
            feature: String::new(),
            scope: QuotaScope::Subject,
            enforcement: Enforcement::Hard,
            overage_pct: None,
            rollover_pct: 0,
            rollover_cap: None,
            cost: 1,
            lease_ttl_secs: 300,
            permissions: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Subjects
// ---------------------------------------------------------------------------

/// Subscription lifecycle (set by billing, FB-70).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionStatus {
    /// Paid and current.
    Active,
    /// In a trial until `trial_ends_at`.
    Trialing,
    /// Payment failed; still entitled during dunning.
    PastDue,
    /// Paused: not entitled (default plan applies).
    Paused,
    /// Cancelled: not entitled.
    Canceled,
    /// Ended (trial over without conversion): not entitled.
    Expired,
}

impl SubscriptionStatus {
    /// Stored name.
    pub fn as_str(self) -> &'static str {
        match self {
            SubscriptionStatus::Active => "active",
            SubscriptionStatus::Trialing => "trialing",
            SubscriptionStatus::PastDue => "past_due",
            SubscriptionStatus::Paused => "paused",
            SubscriptionStatus::Canceled => "canceled",
            SubscriptionStatus::Expired => "expired",
        }
    }

    /// Parses a stored name.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "active" => SubscriptionStatus::Active,
            "trialing" => SubscriptionStatus::Trialing,
            "past_due" => SubscriptionStatus::PastDue,
            "paused" => SubscriptionStatus::Paused,
            "canceled" => SubscriptionStatus::Canceled,
            "expired" => SubscriptionStatus::Expired,
            _ => return None,
        })
    }
}

/// Add-on quantity of a subject.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AddonQuantity {
    /// Add-on key.
    pub addon: String,
    /// Quantity (0 removes it).
    pub quantity: i64,
}

/// A subject's subscription.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Subscription {
    /// `org:<id>` or a principal key.
    pub subject: String,
    /// Plan key.
    pub plan: String,
    /// Lifecycle state.
    pub status: SubscriptionStatus,
    /// End of the trial.
    #[serde(with = "ts_opt")]
    #[schema(value_type = Option<String>)]
    pub trial_ends_at: Option<i64>,
    /// Current billing period.
    #[serde(with = "ts_opt")]
    #[schema(value_type = Option<String>)]
    pub current_period_start: Option<i64>,
    /// Current billing period end.
    #[serde(with = "ts_opt")]
    #[schema(value_type = Option<String>)]
    pub current_period_end: Option<i64>,
    /// Monthly quota windows start on this day and time of month.
    #[serde(with = "ts_opt")]
    #[schema(value_type = Option<String>)]
    pub cycle_anchor: Option<i64>,
    /// Scheduled cancellation.
    #[serde(with = "ts_opt")]
    #[schema(value_type = Option<String>)]
    pub cancel_at: Option<i64>,
    /// Provider reference (FB-70).
    pub external_ref: Option<String>,
    /// Add-ons.
    pub addons: Vec<AddonQuantity>,
    /// Last change.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub updated_at: i64,
}

/// The billing sync seam's input: the full state of a subject's subscription
/// (FB-70 calls [`crate::BillingSync::sync_subscription`]).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SubscriptionSync {
    /// `org:<id>` or a principal key.
    pub subject: String,
    /// Plan key.
    pub plan: String,
    /// Lifecycle state.
    pub status: SubscriptionStatus,
    /// End of the trial.
    #[serde(default, with = "ts_opt")]
    #[schema(value_type = Option<String>)]
    pub trial_ends_at: Option<i64>,
    /// Current billing period start.
    #[serde(default, with = "ts_opt")]
    #[schema(value_type = Option<String>)]
    pub current_period_start: Option<i64>,
    /// Current billing period end.
    #[serde(default, with = "ts_opt")]
    #[schema(value_type = Option<String>)]
    pub current_period_end: Option<i64>,
    /// Cycle anchor of monthly windows.
    #[serde(default, with = "ts_opt")]
    #[schema(value_type = Option<String>)]
    pub cycle_anchor: Option<i64>,
    /// Scheduled cancellation.
    #[serde(default, with = "ts_opt")]
    #[schema(value_type = Option<String>)]
    pub cancel_at: Option<i64>,
    /// Provider reference.
    #[serde(default)]
    pub external_ref: Option<String>,
    /// Add-ons (`null`: unchanged; a list replaces the set).
    #[serde(default)]
    pub addons: Option<Vec<AddonQuantity>>,
    /// Provider event id: a replay of an applied event changes nothing.
    #[serde(default)]
    pub event_id: Option<String>,
    /// Provider event time: an event older than the last applied one is
    /// ignored (out-of-order webhooks).
    #[serde(default, with = "ts_opt")]
    #[schema(value_type = Option<String>)]
    pub event_at: Option<i64>,
}

/// Result of a billing sync.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SyncOutcome {
    /// What happened: `applied`, `replayed` (same event id) or `stale`
    /// (older than the last applied event).
    pub result: String,
    /// Subject.
    pub subject: String,
    /// Plan after the sync.
    pub plan: String,
    /// Status after the sync.
    pub status: SubscriptionStatus,
}

/// Body of `POST /entitlements/trials`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TrialInput {
    /// Subject.
    pub subject: String,
    /// Plan key.
    pub plan: String,
    /// Trial length (default: the plan's `trial_days`).
    #[serde(default)]
    pub days: Option<i64>,
}

/// An exception for a subject, a member or a user everywhere.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Override {
    /// Id.
    pub id: String,
    /// Subject (`null`: every subject of `member`).
    pub subject: Option<String>,
    /// Member principal key (`null`: the whole subject).
    pub member: Option<String>,
    /// Feature key.
    pub feature: String,
    /// `allow` or `deny` (deny always wins).
    pub effect: String,
    /// Replacement limit (allow only).
    pub limit: Option<i64>,
    /// Replacement window.
    pub period: Option<Period>,
    /// Rolling window length.
    pub window_secs: Option<i64>,
    /// Ignored after this time.
    #[serde(with = "ts_opt")]
    #[schema(value_type = Option<String>)]
    pub expires_at: Option<i64>,
    /// Why.
    pub reason: String,
    /// Who created it.
    pub created_by: String,
    /// When.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub created_at: i64,
}

/// Body of `POST /entitlements/overrides`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct OverrideInput {
    /// Subject.
    #[serde(default)]
    pub subject: Option<String>,
    /// Member principal key.
    #[serde(default)]
    pub member: Option<String>,
    /// Feature key.
    pub feature: String,
    /// `allow` or `deny`.
    pub effect: String,
    /// Replacement limit.
    #[serde(default)]
    pub limit: Option<i64>,
    /// Replacement window.
    #[serde(default)]
    pub period: Option<Period>,
    /// Rolling window length.
    #[serde(default)]
    pub window_secs: Option<i64>,
    /// Expiry.
    #[serde(default, with = "ts_opt")]
    #[schema(value_type = Option<String>)]
    pub expires_at: Option<i64>,
    /// Why.
    #[serde(default)]
    pub reason: String,
}

/// An enterprise contract limit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CustomLimit {
    /// Subject.
    pub subject: String,
    /// Feature key.
    pub feature: String,
    /// Included (default true).
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Limit (`null`: unlimited).
    #[serde(default)]
    pub limit: Option<i64>,
    /// Window.
    #[serde(default)]
    pub period: Option<Period>,
    /// Rolling window length.
    #[serde(default)]
    pub window_secs: Option<i64>,
    /// Why (contract reference).
    #[serde(default)]
    pub reason: String,
}

/// A usage pack a subject holds.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct PackGrant {
    /// Id.
    pub id: String,
    /// Subject.
    pub subject: String,
    /// Pack key (`null` for ad-hoc grants).
    pub pack: Option<String>,
    /// Feature.
    pub feature: String,
    /// Units granted.
    pub units: i64,
    /// Units left.
    pub remaining: i64,
    /// Expiry.
    #[serde(with = "ts_opt")]
    #[schema(value_type = Option<String>)]
    pub expires_at: Option<i64>,
    /// Granted.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub created_at: i64,
}

/// Body of `POST /entitlements/usage-packs`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PackGrantInput {
    /// Subject.
    pub subject: String,
    /// Pack key.
    pub pack: String,
    /// Number of packs (default 1).
    #[serde(default = "one")]
    pub quantity: i64,
    /// A replay with the same key returns the first grant.
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

// ---------------------------------------------------------------------------
// Effective entitlements
// ---------------------------------------------------------------------------

/// Where a feature's grant comes from (strongest last applied).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GrantSource {
    /// The subject's plan (or the default plan).
    Plan,
    /// The trial allowance.
    Trial,
    /// An add-on.
    Addon,
    /// An enterprise custom limit.
    Custom,
    /// An override.
    Override,
}

/// A subject's effective grant of one feature.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Grant {
    /// Included.
    pub enabled: bool,
    /// Kind of the feature.
    pub kind: FeatureKind,
    /// Limit per window (`null`: unlimited).
    pub limit: Option<i64>,
    /// Unit.
    pub unit: String,
    /// Window.
    pub period: Period,
    /// Rolling window length.
    pub window_secs: Option<i64>,
    /// Strongest source applied.
    pub source: GrantSource,
}

/// Effective entitlements of a subject (and member).
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Entitlements {
    /// Subject.
    pub subject: String,
    /// Member the grants were resolved for (member overrides).
    pub member: Option<String>,
    /// Plan in force (`null`: none, not even a default plan).
    pub plan: Option<String>,
    /// Subscription status (`null`: no subscription; the default plan applies).
    pub status: Option<SubscriptionStatus>,
    /// Whether the subscription entitles (else the default plan applies).
    pub entitled: bool,
    /// Trial end.
    #[serde(with = "ts_opt")]
    #[schema(value_type = Option<String>)]
    pub trial_ends_at: Option<i64>,
    /// Grants by feature key.
    pub features: BTreeMap<String, Grant>,
}

/// Answer of `has()`.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct HasAnswer {
    /// The answer.
    pub has: bool,
    /// Why not.
    pub reason: Option<String>,
}

// ---------------------------------------------------------------------------
// Usage
// ---------------------------------------------------------------------------

/// A quota check, consumption or reservation.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UsageRequest {
    /// Feature key.
    pub feature: String,
    /// Units (default 1).
    #[serde(default = "one")]
    pub units: i64,
    /// Subject billed (default: the caller's subject; another subject needs
    /// privileges or membership).
    #[serde(default)]
    pub subject: Option<String>,
    /// Counter of a `user`, `team` or `resource` quota (default for `user`:
    /// the caller).
    #[serde(default)]
    pub scope_key: Option<String>,
    /// Required for consume and reserve: a replay returns the first result.
    #[serde(default)]
    pub idempotency_key: Option<String>,
    /// Reservations: hold time in seconds (default 300, at most 86400).
    #[serde(default)]
    pub ttl_secs: Option<i64>,
    /// Stored with the usage event.
    #[serde(default)]
    #[schema(value_type = Option<Object>)]
    pub metadata: Option<Json>,
}

impl UsageRequest {
    /// A request for `units` of `feature` with an idempotency key.
    pub fn new(feature: impl Into<String>, units: i64, key: impl Into<String>) -> Self {
        Self {
            feature: feature.into(),
            units,
            idempotency_key: Some(key.into()),
            ..Self::default()
        }
    }

    /// Same request billed to `subject`.
    pub fn for_subject(mut self, subject: impl Into<String>) -> Self {
        self.subject = Some(subject.into());
        self
    }

    /// Same request on the counter `scope_key`.
    pub fn scoped(mut self, scope_key: impl Into<String>) -> Self {
        self.scope_key = Some(scope_key.into());
        self
    }

    /// Same request held for `secs`.
    pub fn ttl(mut self, secs: i64) -> Self {
        self.ttl_secs = Some(secs);
        self
    }
}

/// A quota decision with the counter state after it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Decision {
    /// Allowed.
    pub allowed: bool,
    /// Feature.
    pub feature: String,
    /// Subject.
    pub subject: String,
    /// Counter scope key (`""`: the subject's counter).
    pub scope_key: String,
    /// Units asked for.
    pub units: i64,
    /// Allowance of the window: limit plus rolled-over units (`null`: unlimited).
    pub limit: Option<i64>,
    /// Committed usage in the window (gauges: the level; concurrency: active leases).
    pub used: i64,
    /// Held by open reservations.
    pub reserved: i64,
    /// Left in the window plus usage packs (`null`: unlimited).
    pub remaining: Option<i64>,
    /// Usage-pack units left.
    pub pack_remaining: i64,
    /// Units of this request past the limit of a soft quota.
    pub overage_units: i64,
    /// Units of this request taken from usage packs.
    pub pack_units: i64,
    /// Window start.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub window_start: i64,
    /// Window end (`null`: none).
    #[serde(with = "ts_opt")]
    #[schema(value_type = Option<String>)]
    pub window_end: Option<i64>,
    /// Why not.
    pub reason: Option<String>,
}

/// Result of a consumption.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Consumption {
    /// Usage event id.
    pub event_id: String,
    /// `true` when this is a replay of an earlier request with the same key.
    pub replayed: bool,
    /// The decision (of the first request when replayed).
    pub decision: Decision,
}

/// State of a reservation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReservationStatus {
    /// Counts against the quota until settled.
    Held,
    /// Usage recorded.
    Committed,
    /// Given back.
    Released,
    /// Not settled in time; given back.
    Expired,
}

impl ReservationStatus {
    /// Parses a stored name.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "held" => ReservationStatus::Held,
            "committed" => ReservationStatus::Committed,
            "released" => ReservationStatus::Released,
            "expired" => ReservationStatus::Expired,
            _ => return None,
        })
    }
}

/// A usage reservation.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Reservation {
    /// Id.
    pub id: String,
    /// Feature.
    pub feature: String,
    /// Subject.
    pub subject: String,
    /// Counter scope key.
    pub scope_key: String,
    /// Units held.
    pub units: i64,
    /// Of which from usage packs.
    pub pack_units: i64,
    /// Of which past a soft limit.
    pub overage_units: i64,
    /// State.
    pub status: ReservationStatus,
    /// Units recorded at commit.
    pub committed_units: Option<i64>,
    /// Held until.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub expires_at: i64,
    /// Created.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub created_at: i64,
    /// `true` when returned for a replayed idempotency key.
    pub replayed: bool,
}

/// A concurrency lease.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Lease {
    /// Id.
    pub id: String,
    /// Feature.
    pub feature: String,
    /// Subject.
    pub subject: String,
    /// Counter scope key.
    pub scope_key: String,
    /// Active until (renew to extend).
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub expires_at: i64,
    /// Released.
    pub released: bool,
    /// `true` when returned for a replayed idempotency key.
    pub replayed: bool,
}

/// Body of a gauge change: `delta` (may be negative) or the new `value`.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct GaugeRequest {
    /// Feature key (a gauge).
    pub feature: String,
    /// Change of the level.
    #[serde(default)]
    pub delta: Option<i64>,
    /// New level.
    #[serde(default)]
    pub value: Option<i64>,
    /// Subject (default: the caller's).
    #[serde(default)]
    pub subject: Option<String>,
    /// Counter scope key.
    #[serde(default)]
    pub scope_key: Option<String>,
    /// Required: a replay returns the first result.
    #[serde(default)]
    pub idempotency_key: Option<String>,
    /// Stored with the usage event.
    #[serde(default)]
    #[schema(value_type = Option<Object>)]
    pub metadata: Option<Json>,
}

/// An immutable usage record (FB-70 bills from these).
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct UsageEvent {
    /// Cursor (increasing).
    pub seq: i64,
    /// Id.
    pub id: String,
    /// When.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub at: i64,
    /// Subject billed.
    pub subject: String,
    /// Feature.
    pub feature: String,
    /// Counter scope key.
    pub scope_key: String,
    /// Units (in-window plus pack units; gauges: the change).
    pub units: i64,
    /// Of which from usage packs.
    pub pack_units: i64,
    /// Of which past a soft limit (billable overage).
    pub overage_units: i64,
    /// Window the usage counts in.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub window_start: i64,
    /// `consume`, `reservation` or `gauge`.
    pub source: String,
    /// Reservation committed, if any.
    pub reservation_id: Option<String>,
    /// Idempotency key.
    pub idempotency_key: String,
    /// Who recorded it.
    pub actor: String,
    /// Request id.
    pub request_id: Option<String>,
    /// Caller data.
    #[schema(value_type = Object)]
    pub metadata: Json,
}

/// A page of usage events.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct UsageEventPage {
    /// Oldest first.
    pub items: Vec<UsageEvent>,
    /// Pass as `after` for the next page (`null`: no more).
    pub next: Option<i64>,
}

/// Current usage of one feature of a subject.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct UsageSummary {
    /// Feature.
    pub feature: String,
    /// Kind.
    pub kind: FeatureKind,
    /// Counter scope key.
    pub scope_key: String,
    /// Included.
    pub enabled: bool,
    /// Allowance (limit plus rolled-over units; `null`: unlimited).
    pub limit: Option<i64>,
    /// Rolled over from the previous window.
    pub carried_in: i64,
    /// Committed usage (gauge level, active leases).
    pub used: i64,
    /// Held by reservations.
    pub reserved: i64,
    /// Left including usage packs (`null`: unlimited).
    pub remaining: Option<i64>,
    /// Usage-pack units left.
    pub pack_remaining: i64,
    /// `used` as a percentage of `limit`.
    pub percent: Option<f64>,
    /// Hard or soft.
    pub enforcement: Enforcement,
    /// Window.
    pub period: Period,
    /// Window start.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub window_start: i64,
    /// Window end.
    #[serde(with = "ts_opt")]
    #[schema(value_type = Option<String>)]
    pub window_end: Option<i64>,
}

/// A usage alert.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct UsageAlert {
    /// Id.
    pub id: String,
    /// Raised.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub at: i64,
    /// Subject.
    pub subject: String,
    /// Feature.
    pub feature: String,
    /// Counter scope key.
    pub scope_key: String,
    /// Window.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub window_start: i64,
    /// 70, 90 or 100.
    pub threshold: i64,
    /// Usage when raised.
    pub used: i64,
    /// Allowance when raised.
    pub limit: i64,
    /// Delivered by the alert job.
    #[serde(with = "ts_opt")]
    #[schema(value_type = Option<String>)]
    pub delivered_at: Option<i64>,
}

/// What a maintenance run did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = EntitlementsMaintenanceReport)]
pub struct MaintenanceReport {
    /// Reservations released because they expired.
    pub reservations_expired: u64,
    /// Leases marked expired.
    pub leases_expired: u64,
    /// Trials that ended without conversion.
    pub trials_expired: u64,
    /// Counter windows opened (with rollover).
    pub windows_opened: u64,
    /// Undelivered alerts queued again.
    pub alerts_requeued: u64,
}
