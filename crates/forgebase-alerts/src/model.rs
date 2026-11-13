//! API types (ADR-0035 §6). Times are RFC 3339 in the API, Unix
//! milliseconds in the database.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use utoipa::ToSchema;

use crate::expr::CmpOp;

/// Epoch milliseconds to RFC 3339 (UTC, millisecond precision).
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

/// RFC 3339 to epoch milliseconds.
pub fn parse_iso(s: &str) -> Option<i64> {
    let t = time::OffsetDateTime::parse(s.trim(), &time::format_description::well_known::Rfc3339).ok()?;
    i64::try_from(t.unix_timestamp_nanos() / 1_000_000).ok()
}

/// How urgent an alert is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
#[schema(as = AlertSeverity)]
pub enum Severity {
    /// Worth knowing.
    Info,
    /// Needs attention soon.
    Warning,
    /// Needs attention now.
    Critical,
}

impl Severity {
    /// Lowercase name.
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Warning => "warning",
            Severity::Critical => "critical",
        }
    }

    /// From the lowercase name.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "info" => Some(Severity::Info),
            "warning" => Some(Severity::Warning),
            "critical" => Some(Severity::Critical),
            _ => None,
        }
    }
}

/// Where a rule comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
#[schema(as = AlertRuleSource)]
pub enum RuleSource {
    /// The built-in pack.
    Builtin,
    /// `[[alerts.rules]]` in forgebase.toml.
    Config,
    /// Created through the API.
    Api,
    /// The burn-rate rules of an SLO.
    Slo,
}

/// An alert rule.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[schema(as = AlertRule)]
pub struct Rule {
    /// Id: the built-in id (`queue_backlog_high`), `config:<name>`, `rule-…` or `slo:<slo>:<tier>`.
    pub id: String,
    /// The `alertname` label.
    pub name: String,
    /// Origin.
    pub source: RuleSource,
    /// Expression (PromQL subset).
    pub expr: String,
    /// Comparison.
    pub op: CmpOp,
    /// Threshold.
    pub threshold: f64,
    /// How long the condition must hold before firing (`5m`).
    #[serde(rename = "for")]
    pub for_: String,
    /// The same in milliseconds.
    pub for_ms: i64,
    /// Severity.
    pub severity: Severity,
    /// Extra labels of its alerts.
    pub labels: BTreeMap<String, String>,
    /// Annotations (`summary`, `description`, `runbook_url`).
    pub annotations: BTreeMap<String, String>,
    /// Whether it is evaluated.
    pub enabled: bool,
    /// Switched off by `[alerts] disabled_rules` (the API cannot switch it on).
    pub disabled_by_config: bool,
    /// Creation time (API rules).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// Last change (API rules).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

/// A new custom rule.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = AlertRuleInput)]
pub struct RuleInput {
    /// The `alertname` label (letters, digits, `_`, `-`, `.`; 1-128).
    pub name: String,
    /// Expression.
    pub expr: String,
    /// Comparison (default `>`).
    #[serde(default)]
    pub op: Option<CmpOp>,
    /// Threshold.
    pub threshold: f64,
    /// Hold time before firing (default `0s`).
    #[serde(default, rename = "for")]
    pub for_: Option<String>,
    /// Severity (default `warning`).
    #[serde(default)]
    pub severity: Option<Severity>,
    /// Extra labels.
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    /// Annotations.
    #[serde(default)]
    pub annotations: BTreeMap<String, String>,
    /// Evaluated (default true).
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// A change to a rule. Built-in, file and SLO rules take `enabled` only.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = AlertRulePatch)]
pub struct RulePatch {
    /// Switch the rule on or off.
    #[serde(default)]
    pub enabled: Option<bool>,
    /// New expression.
    #[serde(default)]
    pub expr: Option<String>,
    /// New comparison.
    #[serde(default)]
    pub op: Option<CmpOp>,
    /// New threshold.
    #[serde(default)]
    pub threshold: Option<f64>,
    /// New hold time.
    #[serde(default, rename = "for")]
    pub for_: Option<String>,
    /// New severity.
    #[serde(default)]
    pub severity: Option<Severity>,
    /// New labels (replace).
    #[serde(default)]
    pub labels: Option<BTreeMap<String, String>>,
    /// New annotations (replace).
    #[serde(default)]
    pub annotations: Option<BTreeMap<String, String>>,
}

/// An expression evaluated now.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = AlertQueryInput)]
pub struct QueryInput {
    /// Expression.
    pub expr: String,
}

/// One sample of a query.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[schema(as = AlertQuerySample)]
pub struct QuerySample {
    /// Labels.
    pub labels: BTreeMap<String, String>,
    /// Value (`null` for NaN).
    pub value: Option<f64>,
}

/// The result of a query.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[schema(as = AlertQueryResult)]
pub struct QueryResult {
    /// Evaluation time.
    pub at: String,
    /// Samples.
    pub samples: Vec<QuerySample>,
}

/// State of an alert.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum AlertState {
    /// The condition holds; waiting out the rule's `for`.
    Pending,
    /// Firing: notifications go out.
    Firing,
    /// The condition no longer holds.
    Resolved,
}

impl AlertState {
    /// Lowercase name.
    pub fn as_str(self) -> &'static str {
        match self {
            AlertState::Pending => "pending",
            AlertState::Firing => "firing",
            AlertState::Resolved => "resolved",
        }
    }

    /// From the lowercase name.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(AlertState::Pending),
            "firing" => Some(AlertState::Firing),
            "resolved" => Some(AlertState::Resolved),
            _ => None,
        }
    }
}

/// One alert: a rule and one label set.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Alert {
    /// Id.
    pub id: String,
    /// Rule id + labels, hashed: the same alert across evaluations.
    pub fingerprint: String,
    /// Rule id.
    pub rule_id: String,
    /// `alertname`.
    pub name: String,
    /// Severity.
    pub severity: Severity,
    /// State.
    pub state: AlertState,
    /// Labels (`alertname`, `severity`, the series' and the rule's).
    pub labels: BTreeMap<String, String>,
    /// Annotations with labels and value filled in.
    pub annotations: BTreeMap<String, String>,
    /// Last value of the expression.
    pub value: Option<f64>,
    /// When the condition first held.
    pub active_at: String,
    /// When it started firing.
    pub fired_at: Option<String>,
    /// When it resolved.
    pub resolved_at: Option<String>,
    /// Last evaluation.
    pub last_evaluated_at: String,
    /// Ids of the active silences that match it (notifications muted).
    pub silenced_by: Vec<String>,
}

/// A state change of an alert.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct AlertEvent {
    /// When.
    pub at: String,
    /// New state.
    pub state: AlertState,
    /// Value then.
    pub value: Option<f64>,
}

/// An alert with its state changes.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct AlertDetail {
    /// The alert.
    #[serde(flatten)]
    pub alert: Alert,
    /// State changes, oldest first.
    pub events: Vec<AlertEvent>,
}

/// A page of alerts.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct AlertPage {
    /// Alerts, newest first.
    pub items: Vec<Alert>,
    /// Pass as `before` for the next page.
    pub next_before: Option<String>,
}

/// A label matcher of a silence or notifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = AlertMatcher)]
pub struct LabelMatcher {
    /// Label name.
    pub label: String,
    /// `=` (default), `!=`, `=~` or `!~` (anchored regular expression).
    #[serde(default = "eq_op")]
    pub op: String,
    /// Value or pattern.
    pub value: String,
}

fn eq_op() -> String {
    "=".into()
}

/// A silence: notifications of matching alerts are muted until `ends_at`.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[schema(as = AlertSilence)]
pub struct Silence {
    /// Id.
    pub id: String,
    /// All must match.
    pub matchers: Vec<LabelMatcher>,
    /// Start.
    pub starts_at: String,
    /// End.
    pub ends_at: String,
    /// Why.
    pub comment: String,
    /// Who.
    pub created_by: String,
    /// Created.
    pub created_at: String,
    /// `active`, `pending` (starts later) or `expired`.
    pub status: String,
}

/// A new silence (or a change: every field replaces).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = AlertSilenceInput)]
pub struct SilenceInput {
    /// At least one; all must match.
    pub matchers: Vec<LabelMatcher>,
    /// Start (default now).
    #[serde(default)]
    pub starts_at: Option<String>,
    /// End; or give `duration`.
    #[serde(default)]
    pub ends_at: Option<String>,
    /// Length from `starts_at` (`2h`), when `ends_at` is not given.
    #[serde(default)]
    pub duration: Option<String>,
    /// Why (shown in the dashboard).
    #[serde(default)]
    pub comment: String,
}

/// Where notifications go.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
#[schema(as = AlertNotifierKind)]
pub enum NotifierKind {
    /// Signed JSON `POST` (the ADR-0031 header scheme).
    Webhook,
    /// Slack incoming webhook.
    Slack,
    /// Email through the server's mail settings.
    Email,
    /// PagerDuty Events API v2.
    Pagerduty,
    /// A ForgeBase queue topic.
    Queue,
}

impl NotifierKind {
    /// Lowercase name.
    pub fn as_str(self) -> &'static str {
        match self {
            NotifierKind::Webhook => "webhook",
            NotifierKind::Slack => "slack",
            NotifierKind::Email => "email",
            NotifierKind::Pagerduty => "pagerduty",
            NotifierKind::Queue => "queue",
        }
    }

    /// From the lowercase name.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "webhook" => Some(NotifierKind::Webhook),
            "slack" => Some(NotifierKind::Slack),
            "email" => Some(NotifierKind::Email),
            "pagerduty" => Some(NotifierKind::Pagerduty),
            "queue" => Some(NotifierKind::Queue),
            _ => None,
        }
    }
}

/// A notification destination. Secrets (the webhook signing secret, the
/// Slack URL, the PagerDuty routing key) are sealed and never listed.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[schema(as = AlertNotifier)]
pub struct Notifier {
    /// Id.
    pub id: String,
    /// Unique name.
    pub name: String,
    /// Kind.
    pub kind: NotifierKind,
    /// Webhook URL, PagerDuty events URL (Slack: host only).
    pub url: Option<String>,
    /// Email recipients.
    pub to: Vec<String>,
    /// Queue topic.
    pub topic: Option<String>,
    /// Only alerts matching all of these.
    pub matchers: Vec<LabelMatcher>,
    /// Only alerts at least this severe.
    pub min_severity: Severity,
    /// Labels that group alerts into one notification (default `alertname`).
    pub group_by: Vec<String>,
    /// Re-send a group that is still firing after this long (`4h`).
    pub repeat_interval: String,
    /// Also notify when a group resolves.
    pub send_resolved: bool,
    /// On or off.
    pub enabled: bool,
    /// Last successful delivery.
    pub last_success_at: Option<String>,
    /// Last failed delivery.
    pub last_failure_at: Option<String>,
    /// Last delivery error.
    pub last_error: Option<String>,
    /// Created.
    pub created_at: String,
    /// Changed.
    pub updated_at: String,
    /// Webhook signing secret (`whsec_…`): only in the answer that created it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signing_secret: Option<String>,
}

/// A new notifier, or a change (every field replaces; secrets are kept
/// when not given).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = AlertNotifierInput)]
pub struct NotifierInput {
    /// Unique name (letters, digits, `_`, `-`, `.`; 1-64).
    pub name: String,
    /// Kind.
    pub kind: NotifierKind,
    /// Webhook URL; Slack incoming-webhook URL (a secret); PagerDuty events
    /// URL (default `https://events.pagerduty.com/v2/enqueue`).
    #[serde(default)]
    pub url: Option<String>,
    /// PagerDuty integration (routing) key (a secret).
    #[serde(default)]
    pub routing_key: Option<String>,
    /// Email recipients (1-20).
    #[serde(default)]
    pub to: Vec<String>,
    /// Queue topic.
    #[serde(default)]
    pub topic: Option<String>,
    /// Only alerts matching all of these.
    #[serde(default)]
    pub matchers: Vec<LabelMatcher>,
    /// Only alerts at least this severe (default `info`: all).
    #[serde(default)]
    pub min_severity: Option<Severity>,
    /// Grouping labels (default `["alertname"]`).
    #[serde(default)]
    pub group_by: Option<Vec<String>>,
    /// Repeat interval (default `4h`; 1m to 7d).
    #[serde(default)]
    pub repeat_interval: Option<String>,
    /// Notify on resolve (default true).
    #[serde(default)]
    pub send_resolved: Option<bool>,
    /// On (default true).
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// One notification and its delivery.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[schema(as = AlertDelivery)]
pub struct Delivery {
    /// Id.
    pub id: String,
    /// Notifier id.
    pub notifier_id: String,
    /// The alert group.
    pub group_key: String,
    /// `firing`, `resolved` or `test`.
    pub event: String,
    /// `pending` (waiting for a first try or a retry), `sent` or `failed` (gave up).
    pub status: String,
    /// Attempts so far.
    pub attempts: i64,
    /// Next attempt (pending).
    pub next_attempt_at: Option<String>,
    /// HTTP status of the last attempt.
    pub last_status: Option<i64>,
    /// Error of the last attempt.
    pub last_error: Option<String>,
    /// When it was delivered.
    pub sent_at: Option<String>,
    /// Created.
    pub created_at: String,
    /// What was (or will be) sent.
    #[schema(value_type = Object)]
    pub payload: Json,
}

/// A page of deliveries.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[schema(as = AlertDeliveryPage)]
pub struct DeliveryPage {
    /// Newest first.
    pub items: Vec<Delivery>,
}

/// SLO kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum SloKind {
    /// Share of requests not answered with a 5xx.
    Availability,
    /// Share of (non-5xx) requests answered within `latency_threshold_ms`.
    Latency,
}

impl SloKind {
    /// Lowercase name.
    pub fn as_str(self) -> &'static str {
        match self {
            SloKind::Availability => "availability",
            SloKind::Latency => "latency",
        }
    }
}

/// A service level objective over a group of routes.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Slo {
    /// Id.
    pub id: String,
    /// Unique name (the `slo` label).
    pub name: String,
    /// What it promises.
    pub description: String,
    /// Kind.
    pub kind: SloKind,
    /// Target share of good requests (0.9 to 0.99999).
    pub objective: f64,
    /// Rolling window (`30d`).
    pub window: String,
    /// The same in milliseconds.
    pub window_ms: i64,
    /// Route templates starting with this (`/api/v1/collections`).
    pub route_prefix: Option<String>,
    /// Or route templates matching this anchored regular expression.
    pub route_regex: Option<String>,
    /// Only these methods (empty: all).
    pub methods: Vec<String>,
    /// Latency SLOs: the threshold.
    pub latency_threshold_ms: Option<i64>,
    /// Multi-window burn-rate alerts on.
    pub alerting: bool,
    /// Extra labels of its alerts.
    pub labels: BTreeMap<String, String>,
    /// Created.
    pub created_at: String,
    /// Changed.
    pub updated_at: String,
}

/// A new SLO or a change (every field replaces).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SloInput {
    /// Unique name (letters, digits, `_`, `-`, `.`; 1-64).
    pub name: String,
    /// Description.
    #[serde(default)]
    pub description: String,
    /// Kind.
    pub kind: SloKind,
    /// Objective, such as 0.999.
    pub objective: f64,
    /// Window (default `30d`; 1h to 90d).
    #[serde(default)]
    pub window: Option<String>,
    /// Route prefix (one of prefix or regex; neither: every route).
    #[serde(default)]
    pub route_prefix: Option<String>,
    /// Route regular expression.
    #[serde(default)]
    pub route_regex: Option<String>,
    /// Methods (`GET`, `POST`, …).
    #[serde(default)]
    pub methods: Vec<String>,
    /// Latency threshold in milliseconds (latency SLOs).
    #[serde(default)]
    pub latency_threshold_ms: Option<i64>,
    /// Burn-rate alerts (default true).
    #[serde(default)]
    pub alerting: Option<bool>,
    /// Extra alert labels.
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

/// Error budget of an SLO over its window.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[schema(as = SloErrorBudget)]
pub struct ErrorBudget {
    /// Bad events the objective allows over the window: `(1 - objective) * total`.
    pub allowed: f64,
    /// Bad events so far.
    pub consumed: i64,
    /// Share left: `1 - consumed / allowed` (below 0 when overspent; 1 with no traffic).
    pub remaining_ratio: f64,
}

/// One burn-rate alert tier.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[schema(as = SloBurnTier)]
pub struct BurnTier {
    /// Rule id.
    pub rule_id: String,
    /// `page_fast`, `page_slow` or `ticket`.
    pub tier: String,
    /// Long window.
    pub long_window: String,
    /// Short window.
    pub short_window: String,
    /// Burn rate both windows must exceed.
    pub burn_rate_threshold: f64,
    /// Severity.
    pub severity: Severity,
    /// Whether it fires now.
    pub firing: bool,
}

/// Where an SLO stands.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct SloStatus {
    /// The SLO.
    pub slo: Slo,
    /// Requests in the window.
    pub total: i64,
    /// Good requests.
    pub good: i64,
    /// Bad requests.
    pub bad: i64,
    /// `good / total` (`null` without traffic).
    pub sli: Option<f64>,
    /// The error budget.
    pub error_budget: ErrorBudget,
    /// Burn rate per window (`5m`, `30m`, `1h`, `6h`, `3d`): error ratio / (1 - objective).
    pub burn_rates: BTreeMap<String, Option<f64>>,
    /// Burn-rate alert tiers.
    pub alerts: Vec<BurnTier>,
}
