//! Wire models generated from the server's OpenAPI document by `scripts/generate_models.py`.
//!
//! Do not edit by hand: change the script or the OpenAPI source and regenerate.

#![allow(
    missing_docs,
    clippy::large_enum_variant,
    clippy::enum_variant_names,
    clippy::struct_field_names
)]

use serde::{Deserialize, Serialize};

/// Versions to accept.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AcceptInput {
    /// Agreements and the version accepted.
    #[serde(default)]
    pub items: Vec<AcceptItem>,
}

/// One agreement version accepted.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AcceptItem {
    /// Kind.
    pub kind: String,
    /// Version (must be the current one).
    pub version: i64,
}

/// Acceptance record.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Acceptance {
    /// When (epoch ms).
    pub accepted_at: i64,
    /// Id.
    pub id: String,
    /// From which address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
    /// Kind.
    pub kind: String,
    /// Who (principal key).
    pub principal: String,
    /// Realm.
    pub realm: String,
    /// With which client.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    /// Version.
    pub version: i64,
}

/// `202 Accepted` body.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Accepted {
    /// The operation.
    pub operation: Operation,
}

/// Body of an ack or nack.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AckRequest {
    /// Ack ids from deliveries.
    #[serde(default)]
    pub ack_ids: Vec<String>,
}

/// Outcome of an ack, nack or deadline change.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AckResult {
    /// Deliveries changed.
    pub count: i64,
    /// Ack ids that are unknown or no longer current (redelivered since, acked, or dead-lettered).
    #[serde(default)]
    pub unknown: Vec<String>,
}

/// The ACL of a bucket (replaced as a whole).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Acl {
    /// Grants.
    #[serde(default)]
    pub entries: Vec<AclEntry>,
}

/// One ACL grant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AclEntry {
    /// `user:<collection>/<id>`, `service:<id>`, `role:<name>`,
    /// `authenticated` or `public`.
    pub grantee: String,
    /// Access level.
    pub permission: Permission,
    /// Key prefix the grant covers (`""`: the whole bucket).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
}

/// Body of `POST /api/system/database/nodes`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AddNode {
    /// `local` (a replica on this server, cloned now), or `external` (a
    /// replica you run, at `url`). Replicas on other hosts join with a
    /// join token instead.
    pub kind: String,
    /// Node name: 1 to 32 of `a-z 0-9 - _`, not `primary`.
    pub name: String,
    /// Set id (default `control`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set: Option<String>,
    /// `external` only: `postgres://` URL (stored sealed, never shown again).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Share of replica reads (default 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<i64>,
}

/// Body of `POST /api/system/database/shards`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AddShard {
    /// Display name (unique ignoring case).
    pub name: String,
    /// Its primary.
    pub node: ShardNode,
    /// Relative capacity (default 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<i64>,
}

/// Answer of `POST /api/system/database/nodes`: an operation (`local`) or the node (`external`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Added {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<NodeInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<Operation>,
}

/// An add-on.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Addon {
    /// Can be assigned.
    pub active: bool,
    /// Description.
    pub description: String,
    /// Grants.
    #[serde(default)]
    pub features: Vec<AddonFeature>,
    /// Key.
    pub key: String,
    /// Display name.
    pub name: String,
}

/// What one unit of an add-on grants.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AddonFeature {
    /// Feature key.
    pub feature: String,
    /// Units added per add-on quantity (`null`: unlimited when the add-on is
    /// what enables the feature, no change otherwise).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period: Option<Period>,
    /// Rolling window length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_secs: Option<i64>,
}

/// Body of `PUT /addons/{key}`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AddonInput {
    /// Can be assigned (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
    /// Description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Grants (replaces the set).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub features: Option<Vec<AddonFeature>>,
    /// Display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Add-on quantity of a subject.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AddonQuantity {
    /// Add-on key.
    pub addon: String,
    /// Quantity (0 removes it).
    pub quantity: i64,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AggregateBody {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aggregates: Option<Vec<AggregateSpecBody>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_by: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    #[serde(default)]
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AggregateSpecBody {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// `count`, `sum`, `avg`, `min`, `max`.
    pub function: String,
}

/// One version of a legal agreement.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Agreement {
    /// Text (plain text or Markdown; escape when rendering).
    pub body: String,
    /// Id.
    pub id: String,
    /// Kind (`terms`, `privacy`, ...).
    pub kind: String,
    /// Published (epoch ms).
    pub published_at: i64,
    /// Who published it.
    pub published_by: String,
    /// Realm.
    pub realm: String,
    /// Title.
    pub title: String,
    /// Canonical URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Version (1, 2, ...).
    pub version: i64,
}

/// Publishes the next version of an agreement.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AgreementInput {
    /// Text (max 200 KiB).
    pub body: String,
    /// Kind: lower-case letters, digits, `_`, `-` (max 64).
    pub kind: String,
    /// Title (max 200).
    pub title: String,
    /// Canonical URL (`http(s)://`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// A version of an agreement without its text.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AgreementRef {
    /// Kind.
    pub kind: String,
    /// Title.
    pub title: String,
    /// Version.
    pub version: i64,
}

/// One alert: a rule and one label set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Alert {
    /// When the condition first held.
    pub active_at: String,
    /// Annotations with labels and value filled in.
    #[serde(default)]
    pub annotations: std::collections::HashMap<String, String>,
    /// Rule id + labels, hashed: the same alert across evaluations.
    pub fingerprint: String,
    /// When it started firing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fired_at: Option<String>,
    /// Id.
    pub id: String,
    /// Labels (`alertname`, `severity`, the series' and the rule's).
    #[serde(default)]
    pub labels: std::collections::HashMap<String, String>,
    /// Last evaluation.
    pub last_evaluated_at: String,
    /// `alertname`.
    pub name: String,
    /// When it resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<String>,
    /// Rule id.
    pub rule_id: String,
    /// Severity.
    pub severity: AlertSeverity,
    /// Ids of the active silences that match it (notifications muted).
    #[serde(default)]
    pub silenced_by: Vec<String>,
    /// State.
    pub state: AlertState,
    /// Last value of the expression.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<f64>,
}

/// Comparison of a rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AlertComparison {
    #[serde(rename = ">")]
    Empty,
    #[serde(rename = ">=")]
    Empty_,
    #[serde(rename = "<")]
    Empty__,
    #[serde(rename = "<=")]
    Empty___,
    #[serde(rename = "==")]
    Empty____,
    #[serde(rename = "!=")]
    Empty_____,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// One notification and its delivery.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AlertDelivery {
    /// Attempts so far.
    pub attempts: i64,
    /// Created.
    pub created_at: String,
    /// `firing`, `resolved` or `test`.
    pub event: String,
    /// The alert group.
    pub group_key: String,
    /// Id.
    pub id: String,
    /// Error of the last attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// HTTP status of the last attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_status: Option<i64>,
    /// Next attempt (pending).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_attempt_at: Option<String>,
    /// Notifier id.
    pub notifier_id: String,
    /// What was (or will be) sent.
    #[serde(default)]
    pub payload: serde_json::Value,
    /// When it was delivered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_at: Option<String>,
    /// `pending` (waiting for a first try or a retry), `sent` or `failed` (gave up).
    pub status: String,
}

/// A page of deliveries.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AlertDeliveryPage {
    /// Newest first.
    #[serde(default)]
    pub items: Vec<AlertDelivery>,
}

/// An alert with its state changes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlertDetail {
    /// When the condition first held.
    pub active_at: String,
    /// Annotations with labels and value filled in.
    #[serde(default)]
    pub annotations: std::collections::HashMap<String, String>,
    /// Rule id + labels, hashed: the same alert across evaluations.
    pub fingerprint: String,
    /// When it started firing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fired_at: Option<String>,
    /// Id.
    pub id: String,
    /// Labels (`alertname`, `severity`, the series' and the rule's).
    #[serde(default)]
    pub labels: std::collections::HashMap<String, String>,
    /// Last evaluation.
    pub last_evaluated_at: String,
    /// `alertname`.
    pub name: String,
    /// When it resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<String>,
    /// Rule id.
    pub rule_id: String,
    /// Severity.
    pub severity: AlertSeverity,
    /// Ids of the active silences that match it (notifications muted).
    #[serde(default)]
    pub silenced_by: Vec<String>,
    /// State.
    pub state: AlertState,
    /// Last value of the expression.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<f64>,
    /// State changes, oldest first.
    #[serde(default)]
    pub events: Vec<AlertEvent>,
}

/// A state change of an alert.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlertEvent {
    /// When.
    pub at: String,
    /// New state.
    pub state: AlertState,
    /// Value then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<f64>,
}

/// A label matcher of a silence or notifier.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AlertMatcher {
    /// Label name.
    pub label: String,
    /// `=` (default), `!=`, `=~` or `!~` (anchored regular expression).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op: Option<String>,
    /// Value or pattern.
    pub value: String,
}

/// A notification destination. Secrets (the webhook signing secret, the
/// Slack URL, the PagerDuty routing key) are sealed and never listed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlertNotifier {
    /// Created.
    pub created_at: String,
    /// On or off.
    pub enabled: bool,
    /// Labels that group alerts into one notification (default `alertname`).
    #[serde(default)]
    pub group_by: Vec<String>,
    /// Id.
    pub id: String,
    /// Kind.
    pub kind: AlertNotifierKind,
    /// Last delivery error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Last failed delivery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_failure_at: Option<String>,
    /// Last successful delivery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_success_at: Option<String>,
    /// Only alerts matching all of these.
    #[serde(default)]
    pub matchers: Vec<AlertMatcher>,
    /// Only alerts at least this severe.
    pub min_severity: AlertSeverity,
    /// Unique name.
    pub name: String,
    /// Re-send a group that is still firing after this long (`4h`).
    pub repeat_interval: String,
    /// Also notify when a group resolves.
    pub send_resolved: bool,
    /// Webhook signing secret (`whsec_…`): only in the answer that created it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signing_secret: Option<String>,
    /// Email recipients.
    #[serde(default)]
    pub to: Vec<String>,
    /// Queue topic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
    /// Changed.
    pub updated_at: String,
    /// Webhook URL, PagerDuty events URL (Slack: host only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// A new notifier, or a change (every field replaces; secrets are kept
/// when not given).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlertNotifierInput {
    /// On (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Grouping labels (default `["alertname"]`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_by: Option<Vec<String>>,
    /// Kind.
    pub kind: AlertNotifierKind,
    /// Only alerts matching all of these.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matchers: Option<Vec<AlertMatcher>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_severity: Option<AlertSeverity>,
    /// Unique name (letters, digits, `_`, `-`, `.`; 1-64).
    pub name: String,
    /// Repeat interval (default `4h`; 1m to 7d).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat_interval: Option<String>,
    /// PagerDuty integration (routing) key (a secret).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing_key: Option<String>,
    /// Notify on resolve (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub send_resolved: Option<bool>,
    /// Email recipients (1-20).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<Vec<String>>,
    /// Queue topic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
    /// Webhook URL; Slack incoming-webhook URL (a secret); PagerDuty events
    /// URL (default `https://events.pagerduty.com/v2/enqueue`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// Where notifications go.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AlertNotifierKind {
    #[serde(rename = "webhook")]
    Webhook,
    #[serde(rename = "slack")]
    Slack,
    #[serde(rename = "email")]
    Email,
    #[serde(rename = "pagerduty")]
    Pagerduty,
    #[serde(rename = "queue")]
    Queue,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// A page of alerts.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AlertPage {
    /// Alerts, newest first.
    #[serde(default)]
    pub items: Vec<Alert>,
    /// Pass as `before` for the next page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_before: Option<String>,
}

/// An expression evaluated now.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AlertQueryInput {
    /// Expression.
    pub expr: String,
}

/// The result of a query.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AlertQueryResult {
    /// Evaluation time.
    pub at: String,
    /// Samples.
    #[serde(default)]
    pub samples: Vec<AlertQuerySample>,
}

/// One sample of a query.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AlertQuerySample {
    /// Labels.
    #[serde(default)]
    pub labels: std::collections::HashMap<String, String>,
    /// Value (`null` for NaN).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<f64>,
}

/// An alert rule.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlertRule {
    /// Annotations (`summary`, `description`, `runbook_url`).
    #[serde(default)]
    pub annotations: std::collections::HashMap<String, String>,
    /// Creation time (API rules).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// Switched off by `[alerts] disabled_rules` (the API cannot switch it on).
    pub disabled_by_config: bool,
    /// Whether it is evaluated.
    pub enabled: bool,
    /// Expression (PromQL subset).
    pub expr: String,
    /// How long the condition must hold before firing (`5m`).
    pub r#for: String,
    /// The same in milliseconds.
    pub for_ms: i64,
    /// Id: the built-in id (`queue_backlog_high`), `config:<name>`, `rule-…` or `slo:<slo>:<tier>`.
    pub id: String,
    /// Extra labels of its alerts.
    #[serde(default)]
    pub labels: std::collections::HashMap<String, String>,
    /// The `alertname` label.
    pub name: String,
    /// Comparison.
    pub op: AlertComparison,
    /// Severity.
    pub severity: AlertSeverity,
    /// Origin.
    pub source: AlertRuleSource,
    /// Threshold.
    pub threshold: f64,
    /// Last change (API rules).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

/// A new custom rule.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AlertRuleInput {
    /// Annotations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<std::collections::HashMap<String, String>>,
    /// Evaluated (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Expression.
    pub expr: String,
    /// Hold time before firing (default `0s`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#for: Option<String>,
    /// Extra labels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labels: Option<std::collections::HashMap<String, String>>,
    /// The `alertname` label (letters, digits, `_`, `-`, `.`; 1-128).
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op: Option<AlertComparison>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<AlertSeverity>,
    /// Threshold.
    pub threshold: f64,
}

/// A change to a rule. Built-in, file and SLO rules take `enabled` only.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AlertRulePatch {
    /// New annotations (replace).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<std::collections::HashMap<String, String>>,
    /// Switch the rule on or off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// New expression.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expr: Option<String>,
    /// New hold time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#for: Option<String>,
    /// New labels (replace).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labels: Option<std::collections::HashMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op: Option<AlertComparison>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<AlertSeverity>,
    /// New threshold.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold: Option<f64>,
}

/// Where a rule comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AlertRuleSource {
    #[serde(rename = "builtin")]
    Builtin,
    #[serde(rename = "config")]
    Config,
    #[serde(rename = "api")]
    Api,
    #[serde(rename = "slo")]
    Slo,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// How urgent an alert is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AlertSeverity {
    #[serde(rename = "info")]
    Info,
    #[serde(rename = "warning")]
    Warning,
    #[serde(rename = "critical")]
    Critical,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// A silence: notifications of matching alerts are muted until `ends_at`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AlertSilence {
    /// Why.
    pub comment: String,
    /// Created.
    pub created_at: String,
    /// Who.
    pub created_by: String,
    /// End.
    pub ends_at: String,
    /// Id.
    pub id: String,
    /// All must match.
    #[serde(default)]
    pub matchers: Vec<AlertMatcher>,
    /// Start.
    pub starts_at: String,
    /// `active`, `pending` (starts later) or `expired`.
    pub status: String,
}

/// A new silence (or a change: every field replaces).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AlertSilenceInput {
    /// Why (shown in the dashboard).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// Length from `starts_at` (`2h`), when `ends_at` is not given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<String>,
    /// End; or give `duration`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ends_at: Option<String>,
    /// At least one; all must match.
    #[serde(default)]
    pub matchers: Vec<AlertMatcher>,
    /// Start (default now).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starts_at: Option<String>,
}

/// State of an alert.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AlertState {
    #[serde(rename = "pending")]
    Pending,
    #[serde(rename = "firing")]
    Firing,
    #[serde(rename = "resolved")]
    Resolved,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// Body of a refund (of a consumption or spend) or a reversal (of a top-up
/// or grant).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AmendInput {
    /// Credits (default: all that is left to refund or reverse).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount: Option<i64>,
    /// The entry refunded or reversed.
    pub entry_id: String,
    /// External reference (FB-70: refund or credit note).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_ref: Option<String>,
    /// Idempotency key (unique per subject).
    pub idempotency_key: String,
    /// Caller data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    /// Why.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AnalyzeBody {
    /// Collection to analyze; every collection when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,
}

/// One attempt of a job.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Attempt {
    /// Attempt number (1 for the first; a graceful release does not count).
    pub attempt: i64,
    /// Run time in milliseconds (so far, while running).
    pub duration_ms: i64,
    /// Error of a failed attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// End time (RFC 3339); `null` while running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    /// The attempt started with a checkpoint of an earlier one.
    pub from_checkpoint: bool,
    /// `succeeded`, `failed`, `retry`, `timed_out`, `lease_expired`,
    /// `released` or `cancelled`; `null` while running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// Claim time (RFC 3339).
    pub started_at: String,
    /// Worker that ran it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_id: Option<String>,
}

/// Attempts of a job.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AttemptList {
    /// Oldest first.
    #[serde(default)]
    pub items: Vec<Attempt>,
}

/// An audit entry.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuditEntryView {
    /// Who acted: `user:<collection>/<id>`, `admin:<id>`, `service_account:<id>`, `anonymous`.
    pub actor: String,
    /// When (RFC 3339).
    pub at: String,
    /// Collection of the resource concerned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,
    /// Non-secret details.
    #[serde(default)]
    pub detail: serde_json::Value,
    /// The action, such as `login` or `api_key.create`.
    pub event: String,
    /// Row id (increasing; the paging cursor).
    pub id: i64,
    /// Administrator acting as the actor (impersonation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impersonator: Option<String>,
    /// Client address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
    /// `success` or `failure`.
    pub outcome: String,
    /// Request id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// Id of the resource concerned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_id: Option<String>,
    /// Kind of the resource concerned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_type: Option<String>,
    /// User agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
}

/// A page of audit entries.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuditPageView {
    /// The entries.
    #[serde(default)]
    pub items: Vec<AuditEntryView>,
    /// Pass as `cursor` for the next page; absent after the last one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<i64>,
}

/// An entry and the entries around it.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuditRelatedView {
    /// The entry.
    pub event: AuditEntryView,
    /// Other entries of the same actor within the window, oldest first.
    #[serde(default)]
    pub same_actor: Vec<AuditEntryView>,
    /// Other entries of the same request, oldest first.
    #[serde(default)]
    pub same_request: Vec<AuditEntryView>,
}

/// Consistency of one account: the balance, the ledger and the lots agree.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuditReport {
    /// Balance on the account.
    pub balance: i64,
    /// All of the above agree.
    pub consistent: bool,
    /// Every entry's amount equals the sum of its allocations.
    pub entries_match_allocations: bool,
    /// `balance_after` of the newest entry.
    pub last_balance_after: i64,
    /// Sum of the ledger.
    pub ledger_sum: i64,
    /// Every lot's remaining equals the sum of its allocations.
    pub lots_match_allocations: bool,
    /// Sum of what is left in the lots.
    pub lots_remaining: i64,
    /// Subject.
    pub subject: String,
}

/// Counts of an audit time range.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuditSummaryView {
    /// The most active actors.
    #[serde(default)]
    pub actors: Vec<Count>,
    /// The most frequent events.
    #[serde(default)]
    pub events: Vec<Count>,
    /// The most frequent client addresses.
    #[serde(default)]
    pub ips: Vec<Count>,
    /// Per outcome.
    #[serde(default)]
    pub outcomes: Vec<Count>,
    /// Entries in the range.
    pub total: i64,
}

/// `{status:"accepted", message}`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthAccepted {
    pub message: String,
    pub status: String,
}

/// `POST /api/auth/admins` body.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthAdminCreateRequest {
    pub email: String,
    pub password: String,
}

/// `POST /api/auth/password/change` body.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthChangePasswordRequest {
    pub current_password: String,
    pub new_password: String,
}

/// Body carrying one TOTP or recovery code.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthCodeRequest {
    pub code: String,
}

/// Body of the request-an-email endpoints.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthEmailRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,
    pub email: String,
}

/// `POST /api/auth/impersonate` body.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthImpersonateRequest {
    pub principal: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_minutes: Option<i64>,
}

/// `{items: [...]}`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthItems {
    #[serde(default)]
    pub items: Vec<serde_json::Value>,
}

/// A created token or API key; `key` is the secret, shown once, plus the key's metadata.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthKeyCreated {
    pub id: String,
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scopes: Option<Vec<String>>,
    /// Tenant the key is bound to (FB-138).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
}

/// Body of the token and API key creation endpoints.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthKeyRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in_days: Option<i64>,
    pub name: String,
    /// `<area>:read|write[:<name>]`.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Bind the key to this tenant (environment id): requests then act in it
    /// only (another `X-ForgeBase-Tenant` is refused) and RESP sessions run in
    /// it. The owner must be able to use the tenant (FB-138).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
}

/// `{active_kid}`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthKeyRotated {
    pub active_kid: String,
}

/// `POST /api/auth/login` and `/api/auth/admins/login` body.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthLoginRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,
    /// Use an HttpOnly session cookie instead of returning tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cookie: Option<bool>,
    /// Name for the device list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
    pub email: String,
    pub password: String,
}

/// A session, or an MFA challenge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AuthLoginResponse {
    /// `AuthSession`
    AuthSession(AuthSession),
    /// `AuthMfaChallenge`
    AuthMfaChallenge(AuthMfaChallenge),
}

/// A second factor is required.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthMfaChallenge {
    pub expires_in: i64,
    pub mfa_required: bool,
    pub mfa_token: String,
}

/// `POST /api/auth/mfa/verify` body.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthMfaVerifyRequest {
    /// TOTP or recovery code.
    pub code: String,
    /// From the `mfa_required` login answer.
    pub mfa_token: String,
}

/// `POST /api/auth/otp/verify` body.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthOtpVerifyRequest {
    pub code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cookie: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
    pub email: String,
}

/// New recovery codes.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthRecoveryCodes {
    #[serde(default)]
    pub recovery_codes: Vec<String>,
}

/// `POST /api/auth/refresh` body.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthRefreshRequest {
    pub refresh_token: String,
}

/// `POST /api/auth/password-reset/confirm` body.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthResetConfirmRequest {
    pub password: String,
    pub token: String,
}

/// `{revoked: n}`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthRevoked {
    pub revoked: i64,
}

/// `POST /api/auth/service-accounts` body.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthServiceAccountRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub name: String,
    /// Defaults to `service`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

/// Tokens of a new or refreshed session. In cookie mode the tokens are absent and
/// `Set-Cookie` headers carry the session and CSRF cookies.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthSession {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub csrf_token: Option<String>,
    pub expires_in: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// RFC 3339.
    pub session_expires_at: String,
    pub session_id: String,
    /// The signed-in subject.
    #[serde(default)]
    pub subject: serde_json::Value,
    /// Always `Bearer`.
    pub token_type: String,
    /// The user record (user sign-ins only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<serde_json::Value>,
}

/// `POST /api/auth/signup` body.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthSignupRequest {
    /// Auth collection (default: the configured default collection).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,
    /// Extra user fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    pub email: String,
    pub password: String,
}

/// Body of the confirm-with-token endpoints.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthTokenRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cookie: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
    pub token: String,
}

/// TOTP disabled.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthTotpDisabled {
    pub enabled: bool,
}

/// TOTP enabled.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthTotpEnabled {
    pub enabled: bool,
    #[serde(default)]
    pub recovery_codes: Vec<String>,
}

/// A pending TOTP enrolment.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthTotpSetup {
    /// `otpauth://` URI for QR codes.
    pub otpauth_url: String,
    /// Base32 secret.
    pub secret: String,
}

/// `{user}`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthUserEnvelope {
    #[serde(default)]
    pub user: serde_json::Value,
}

/// A subject's balance.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Balance {
    /// Credits available.
    pub balance: i64,
    /// Next expiries (soonest first, at most 10).
    #[serde(default)]
    pub expiring: Vec<Expiring>,
    /// Subject.
    pub subject: String,
    /// Last change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

/// Balance over time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BalanceHistory {
    /// Start (inclusive).
    pub from: String,
    /// Bucket width.
    pub interval: Interval,
    /// Balance at `from`.
    pub opening: i64,
    /// Every bucket, oldest first.
    #[serde(default)]
    pub points: Vec<BalancePoint>,
    /// Subject.
    pub subject: String,
    /// End (exclusive).
    pub to: String,
}

/// One bucket of a balance history.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct BalancePoint {
    /// Balance at the end of the bucket.
    pub closing: i64,
    /// Credits added in the bucket.
    pub credited: i64,
    /// Credits removed in the bucket.
    pub debited: i64,
    /// Bucket start.
    pub start: String,
}

/// Body of `POST /batch`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct BatchBody {
    /// Commands run in order with no other command in between.
    #[serde(default)]
    pub commands: Vec<Vec<Bin>>,
}

/// The result of a collection attempt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BillingCollection {
    /// The invoice after the attempt.
    pub invoice: Invoice,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payment: Option<Payment>,
    /// The hosted page where the customer must pay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payment_url: Option<String>,
}

/// Billing interval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BillingInterval {
    #[serde(rename = "month")]
    Month,
    #[serde(rename = "year")]
    Year,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// What a maintenance run did.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct BillingMaintenanceReport {
    /// Subscriptions ended (cancel at period end, dunning, incomplete).
    pub ended: i64,
    /// Invoices fulfilled (late fulfillment).
    pub fulfilled: i64,
    /// Subscriptions renewed (invoice raised).
    pub renewed: i64,
    /// Dunning retries attempted.
    pub retries: i64,
    /// Entitlement syncs applied.
    pub synced: i64,
    /// Trials converted or ended.
    pub trials_ended: i64,
    /// Usage events read.
    pub usage_events_read: i64,
    /// Usage records stored for metered prices.
    pub usage_recorded: i64,
    /// Inbound webhook deliveries deleted after the retention (ADR-0032).
    pub webhook_deliveries_pruned: i64,
}

/// Operator settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BillingSettings {
    /// Credit note number prefix.
    pub credit_note_prefix: String,
    /// Currency of new customers.
    pub default_currency: String,
    /// After the last retry.
    pub final_action: FinalAction,
    /// Days an unpaid first invoice keeps an `incomplete` subscription.
    pub incomplete_days: i64,
    /// Invoice number prefix.
    pub invoice_prefix: String,
    /// Days after the first failed attempt at which payment is retried
    /// (`[1, 3, 5]`: retries 1, 3 and 5 days after the failure).
    #[serde(default)]
    pub retry_days: Vec<i64>,
    /// Webhooks older than this are ignored (replay window).
    pub webhook_max_age_secs: i64,
    /// Seconds a signed webhook timestamp may differ from now (Stripe).
    pub webhook_tolerance_secs: i64,
}

/// Payment statistics over a window.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct BillingStats {
    /// Subscriptions active at the window start.
    pub active_at_start: i64,
    /// Subscriptions per status, now.
    #[serde(default)]
    pub by_status: std::collections::HashMap<String, i64>,
    /// churned / active_at_start in basis points (`null`: none at start).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub churn_rate_bp: Option<i64>,
    /// Subscriptions that ended in the window.
    pub churned: i64,
    /// Payments that failed in the window.
    pub failed_payments: i64,
    /// Window start.
    pub from: String,
    /// Subscriptions started in the window.
    pub new_subscriptions: i64,
    /// MRR and ARR per currency, now.
    #[serde(default)]
    pub recurring: Vec<RecurringRevenue>,
    /// Revenue per currency and bucket.
    #[serde(default)]
    pub revenue: Vec<RevenuePoint>,
    /// Window end.
    pub to: String,
}

/// A store product (App Store, Google Play, Creem) sold as a billing price.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct BillingStoreProduct {
    /// `subscription` (recurring price) or `one_time`.
    pub kind: String,
    /// Price key.
    pub price: String,
    /// Product id at the store.
    pub product_id: String,
    /// `app_store`, `google_play` or `creem`.
    pub provider: String,
    /// Changed.
    pub updated_at: String,
}

/// A store purchase bound to a subject.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct BillingStorePurchase {
    /// Renews at the period end.
    pub auto_renew: bool,
    /// Created.
    pub created_at: String,
    /// `test` (sandbox, licence tester, Creem test mode).
    pub environment: String,
    /// End of the current period.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Id.
    pub id: String,
    /// `subscription` or `one_time`.
    pub kind: String,
    /// Newest store transaction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_transaction_id: Option<String>,
    /// The billing order (one-time purchases).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_id: Option<String>,
    /// Price key.
    pub price: String,
    /// Store product id.
    pub product_id: String,
    /// Store.
    pub provider: String,
    /// Stable purchase id at the store (original transaction id, purchase
    /// token, Creem subscription or order).
    pub purchase_id: String,
    /// Store state: `active`, `trialing`, `grace`, `on_hold`, `paused`,
    /// `expired`, `refunded`, `revoked`, `pending`, `replaced`.
    pub status: String,
    /// Subject.
    pub subject: String,
    /// The billing subscription it drives.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription_id: Option<String>,
    /// Changed.
    pub updated_at: String,
}

/// A subscription.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BillingSubscription {
    /// Ends at the period end.
    pub cancel_at_period_end: bool,
    /// Why it was cancelled (`requested`, `payment_failed`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel_reason: Option<String>,
    /// When cancellation was asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canceled_at: Option<String>,
    /// Coupon code in force.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coupon: Option<String>,
    /// Discounted invoices left (repeating coupons).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coupon_cycles_left: Option<i64>,
    /// Created.
    pub created_at: String,
    /// Currency.
    pub currency: String,
    /// Current period end.
    pub current_period_end: String,
    /// Current period start.
    pub current_period_start: String,
    /// Customer id.
    pub customer_id: String,
    /// Cycle anchor (renewals keep its day of month and time).
    pub cycle_anchor: String,
    /// When it ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    /// Id.
    pub id: String,
    /// Interval.
    pub interval: BillingInterval,
    /// Intervals per period.
    pub interval_count: i64,
    /// Prices.
    #[serde(default)]
    pub items: Vec<SubscriptionItem>,
    /// Newest invoice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_invoice_id: Option<String>,
    /// Caller data.
    #[serde(default)]
    pub metadata: serde_json::Value,
    /// Price scheduled for the next period (downgrade at period end).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_price: Option<String>,
    /// Payment provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Lifecycle state.
    pub status: BillingSubscriptionStatus,
    /// Subject billed.
    pub subject: String,
    /// End of the trial.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trial_ends_at: Option<String>,
    /// Changes so far.
    pub version: i64,
}

/// Subscription lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BillingSubscriptionStatus {
    #[serde(rename = "incomplete")]
    Incomplete,
    #[serde(rename = "trialing")]
    Trialing,
    #[serde(rename = "active")]
    Active,
    #[serde(rename = "past_due")]
    PastDue,
    #[serde(rename = "paused")]
    Paused,
    #[serde(rename = "canceled")]
    Canceled,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// A binary-safe value: a string, or `{"base64": "..."}` for bytes that are
/// not UTF-8.
pub type Bin = serde_json::Value;

/// White-label branding of a realm.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Branding {
    /// Accent colour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent_color: Option<String>,
    /// Page background.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_color: Option<String>,
    /// Product name shown to users.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// A favicon is uploaded.
    pub has_favicon: bool,
    /// A logo is uploaded.
    pub has_logo: bool,
    /// Primary colour (`#rgb` or `#rrggbb`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_color: Option<String>,
    /// Realm.
    pub realm: String,
    /// Sign-in page footer (plain text).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signin_footer: Option<String>,
    /// Sign-in page sub-heading (plain text).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signin_subtitle: Option<String>,
    /// Sign-in page heading (plain text).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signin_title: Option<String>,
    /// Support contact shown on hosted pages and in email.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub support_email: Option<String>,
    /// Text colour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_color: Option<String>,
    /// Last change (epoch ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
}

/// Replaces the branding of a realm; a field left out is cleared.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct BrandingInput {
    /// Accent colour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent_color: Option<String>,
    /// Page background.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_color: Option<String>,
    /// Product name (max 80).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Primary colour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_color: Option<String>,
    /// Sign-in footer (max 300).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signin_footer: Option<String>,
    /// Sign-in sub-heading (max 300).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signin_subtitle: Option<String>,
    /// Sign-in heading (max 120).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signin_title: Option<String>,
    /// Support email.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub support_email: Option<String>,
    /// Text colour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_color: Option<String>,
}

/// A bucket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bucket {
    /// Allowed content types (`image/*` patterns allowed); `null` allows all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_content_types: Option<Vec<String>>,
    /// RFC 3339 creation time.
    pub created_at: String,
    /// Largest object, in bytes (`null`: the server limit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_object_bytes: Option<i64>,
    /// Name.
    pub name: String,
    /// Principal key of the creator.
    pub owner: String,
    /// Owning tenant (FB-140); `null` for an instance-wide bucket.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    /// RFC 3339 time of the last change.
    pub updated_at: String,
    /// Whether overwrites and deletes keep earlier versions.
    pub versioning: bool,
    /// Default read access.
    pub visibility: Visibility,
}

/// A bucket to create.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct BucketInput {
    /// Allowed content types.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_content_types: Option<Vec<String>>,
    /// Largest object in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_object_bytes: Option<i64>,
    /// Name.
    pub name: String,
    /// Keep versions (default off).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub versioning: Option<bool>,
    /// Default read access (default private).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<Visibility>,
}

/// A partial bucket update.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct BucketPatch {
    /// New content-type allow list (empty clears it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_content_types: Option<Vec<String>>,
    /// New size limit (`0` clears it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_object_bytes: Option<i64>,
    /// Turn versioning on or off (earlier versions are kept).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub versioning: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<Visibility>,
}

/// Space used by one bucket.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct BucketUsage {
    /// Bucket name.
    pub bucket: String,
    /// Logical bytes of current objects.
    pub logical_bytes: i64,
    /// Current (non-deleted) objects.
    pub objects: i64,
    /// Logical bytes of every version (before deduplication).
    pub version_bytes: i64,
    /// Object versions, including older ones and delete markers.
    pub versions: i64,
}

/// Body of `POST /billing/subscriptions/{id}/cancel`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CancelInput {
    /// End at the period end (default) or now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_period_end: Option<bool>,
    /// Why.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Every feature of this server.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Capabilities {
    /// `sqlite` or `postgres`.
    pub backend: String,
    /// One entry per feature, in a fixed order.
    #[serde(default)]
    pub features: Vec<Capability>,
}

/// One feature on this server.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Capability {
    /// Whether `engine` is a PostgreSQL extension (an accelerator).
    pub accelerated: bool,
    /// What it runs on: an extension and its version (`pgvector 0.8.7`,
    /// `Apache AGE 1.8.0`, `pg_trgm 1.6`) or a description of the
    /// portable or built-in engine.
    pub engine: String,
    /// The accelerator's PostgreSQL extension, for accelerated features.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension: Option<String>,
    /// Stable name: `vector`, `graph`, `opencypher`, `fulltext`, `fuzzy`,
    /// `queue`, `cron`, `kv`, `realtime`, `read_replicas`, `sharding`.
    pub feature: String,
    /// Human name.
    pub label: String,
    /// Limits of the engine in use (names and values).
    #[serde(default)]
    pub limits: serde_json::Value,
    /// `[database.postgres.extensions]` mode (`auto`, `on`, `off`) of an
    /// accelerated feature on PostgreSQL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Why the feature is not on its accelerator although it could be, or
    /// why it is unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// `active`, `portable`, `off`, `native` or `unavailable` (see the
    /// module documentation).
    pub state: String,
}

/// One kind of event of a record-source topic.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CatalogEventType {
    /// JSON Schema of the message `data`.
    #[serde(default)]
    pub data_schema: serde_json::Value,
    /// What it means.
    pub description: String,
    /// Example message `data`.
    #[serde(default)]
    pub example: serde_json::Value,
    /// A subscription filter that selects only this event type.
    #[serde(default)]
    pub filter: serde_json::Value,
    /// The record operation (`create`, `update`, `delete`): attribute `forgebase.op`.
    pub op: String,
    /// `created`, `updated` or `deleted`.
    pub r#type: String,
}

/// A subscribable topic in the event catalog.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogTopic {
    /// JSON Schema of message `data` (the topic schema; for a record source, the change event).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_schema: Option<serde_json::Value>,
    /// What the topic's events are.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// For a record source: its event types (empty otherwise).
    #[serde(default)]
    pub event_types: Vec<CatalogEventType>,
    /// Example message `data`.
    #[serde(default)]
    pub example: serde_json::Value,
    /// Where `example` comes from.
    pub example_source: ExampleSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<RecordSource>,
    /// Owning tenant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    /// Topic name.
    pub topic: String,
}

/// Body of `POST /billing/subscriptions/{id}/change`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ChangePlanInput {
    /// Switch at the end of the period instead (no proration).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_period_end: Option<bool>,
    /// New plan price key (replaces the subscription's recurring plan price).
    pub price: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proration: Option<Proration>,
    /// Units (default: unchanged).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantity: Option<i64>,
}

/// What a subscription, order or plan change returned: the object, the
/// invoice it produced, and where to pay when no payment method is saved.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Checkout {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invoice: Option<Invoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<Order>,
    /// The provider's hosted payment page, when the customer must pay there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payment_url: Option<String>,
    /// The idempotency key was seen before: this is the first result.
    pub replayed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription: Option<BillingSubscription>,
}

/// The checkpoint of a job.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Checkpoint {
    /// The saved state (`null` when none was saved).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<serde_json::Value>,
    /// Job id.
    pub job_id: String,
    /// When it was saved (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_at: Option<String>,
}

/// Body of a checkpoint.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CheckpointBody {
    /// State to resume from (JSON, at most 256 KiB).
    #[serde(default)]
    pub checkpoint: serde_json::Value,
    /// The lease from the claim.
    pub lease_id: String,
}

/// Child jobs.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ChildList {
    /// In request order.
    #[serde(default)]
    pub items: Vec<Job>,
}

/// Body of a child-job request.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ChildrenBody {
    /// The children (1 to 100).
    #[serde(default)]
    pub jobs: Vec<NewJob>,
    /// The lease of the running parent.
    pub lease_id: String,
}

/// A claim request.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ClaimRequest {
    /// Only these handlers (empty: all).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kinds: Option<Vec<String>>,
    /// At most this many jobs (default 1, at most 50).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<i64>,
    /// Only these queues (empty: all).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queues: Option<Vec<String>>,
    /// The claiming worker (any id of `[A-Za-z0-9._:-]`, at most 128 characters).
    pub worker_id: String,
}

/// Jobs claimed by a worker.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ClaimResponse {
    /// Up to the requested number; empty when nothing is due.
    #[serde(default)]
    pub jobs: Vec<ClaimedJob>,
}

/// A claimed job with the lease that fences it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClaimedJob {
    /// The last checkpoint of an earlier attempt: resume from it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<serde_json::Value>,
    /// The job (already `running`).
    pub job: Job,
    /// Secret token: `heartbeat`, `complete` and `fail` must present it.
    pub lease_id: String,
    /// The parents (dependencies) with their results.
    #[serde(default)]
    pub parents: Vec<ParentResult>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CollectionList {
    #[serde(default)]
    pub items: Vec<CollectionSchema>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CollectionSchema {
    pub created: String,
    #[serde(default)]
    pub fields: Vec<FieldSchema>,
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub options: serde_json::Value,
    /// `strict` or `flexible` (JSON documents with undeclared keys).
    pub schema_mode: String,
    pub system: bool,
    /// `base`, `auth` or `view`.
    pub r#type: String,
    pub updated: String,
    pub version: i64,
    /// Keep every version of every record.
    pub versioned: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view_query: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CollectionSchemaResponse {
    pub collection: CollectionSchema,
    /// JSON Schema (draft 2020-12) of a record of this collection.
    #[serde(default)]
    pub json_schema: serde_json::Value,
}

/// Statistics of one collection.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CollectionStats {
    /// Committed changes since creation.
    pub change_seq: i64,
    /// Collection.
    pub collection: String,
    /// One entry per vector field.
    #[serde(default)]
    pub indexes: Vec<IndexStatus>,
    /// Items.
    pub items: i64,
    /// Items with keyword-search text.
    pub items_with_text: i64,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ColumnInfoSchema {
    pub declared_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    pub name: String,
    pub not_null: bool,
    pub primary_key: bool,
}

/// Body of a reservation commit.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CommitBody {
    /// Units used (default: all reserved units; the rest is released).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub units: Option<i64>,
}

/// The job that undoes a step (saga compensation).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Compensation {
    /// Handler name.
    pub kind: String,
    /// Handler input (default `null`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    /// Queue (default: the step's queue).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue: Option<String>,
}

/// One compile error with its position in the source (1-based line and column, in characters).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CompileDiagnostic {
    /// Column (1-based, in characters).
    pub column: i64,
    /// Line (1-based).
    pub line: i64,
    /// What is wrong.
    pub message: String,
}

/// Body of a completion.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CompleteBody {
    /// The lease from the claim.
    pub lease_id: String,
    /// Stored as the job's result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
}

/// Completes a multipart upload.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CompleteInput {
    /// SHA-256 of the whole object (overrides the declared one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checksum_sha256: Option<String>,
    /// Parts to assemble, in order, each optionally with its checksum.
    /// Default: every received part in part-number order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parts: Option<Vec<CompletePart>>,
}

/// A part named in a completion.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CompletePart {
    /// Expected checksum of the part.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checksum_sha256: Option<String>,
    /// Part number.
    pub part_number: i64,
}

/// Result of a consumption.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Consumption {
    /// The decision (of the first request when replayed).
    pub decision: Decision,
    /// Usage event id.
    pub event_id: String,
    /// `true` when this is a replay of an earlier request with the same key.
    pub replayed: bool,
}

/// Credits consumed for one feature in one bucket.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ConsumptionRow {
    /// Credits debited (consumption and spend).
    pub debited: i64,
    /// Debit entries.
    pub entries: i64,
    /// Feature (`""`: spends without a feature).
    pub feature: String,
    /// `debited - refunded`.
    pub net: i64,
    /// Credits refunded.
    pub refunded: i64,
    /// Bucket start.
    pub start: String,
    /// Units paid for.
    pub units: i64,
}

/// Credit consumption by feature and period.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConsumptionStats {
    /// Start (inclusive).
    pub from: String,
    /// Bucket width.
    pub interval: Interval,
    /// Buckets with consumption, oldest first, then by feature.
    #[serde(default)]
    pub rows: Vec<ConsumptionRow>,
    /// Subject (`null`: the whole instance).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// End (exclusive).
    pub to: String,
    /// Sum of `net`.
    pub total_net: i64,
}

/// Copy or move target.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CopyBody {
    /// Destination bucket (default: the same).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dest_bucket: Option<String>,
    /// Destination key.
    pub dest_key: String,
    /// Source key.
    pub key: String,
    /// Source version (copy only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_id: Option<String>,
}

/// A cost estimate. It prices the period's quantities with the catalog as
/// they are now: not an invoice.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CostEstimate {
    /// The same as a decimal string.
    pub amount: String,
    /// Sum of the priced lines in minor units.
    pub amount_minor: i64,
    /// Credits the credit rates would charge, in total.
    pub credits: i64,
    /// ISO 4217 code.
    pub currency: String,
    /// Always true: this is an estimate.
    pub estimate: bool,
    /// What was priced.
    #[serde(default)]
    pub lines: Vec<CostLine>,
}

/// One priced line of a cost estimate.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CostLine {
    /// Estimated amount in minor units of the currency.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount_minor: Option<i64>,
    /// Credits the credit rate of the feature would charge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credits: Option<i64>,
    /// The metric.
    pub metric: String,
    /// The price used (its key), when one is set for the feature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price: Option<String>,
    /// Units priced.
    pub quantity: i64,
}

/// A count of something.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Count {
    /// Entries.
    pub count: i64,
    /// The value counted (an event, actor, address or outcome).
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CountResponse {
    pub count: i64,
}

/// A coupon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Coupon {
    /// Redeemable.
    pub active: bool,
    /// Amount off, minor units of `currency`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount_off: Option<i64>,
    /// Code customers enter.
    pub code: String,
    /// Currency of `amount_off`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    /// How long it applies.
    pub duration: CouponDuration,
    /// Invoices (repeating).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_cycles: Option<i64>,
    /// Last day it can be redeemed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Id.
    pub id: String,
    /// Percent or amount.
    pub kind: CouponKind,
    /// Redemption cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_redemptions: Option<i64>,
    /// Name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Percent off in basis points.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percent_bp: Option<i64>,
    /// Product keys it applies to (empty: all).
    #[serde(default)]
    pub products: Vec<String>,
    /// Times redeemed.
    pub redeemed: i64,
}

/// How long a coupon applies to a subscription.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CouponDuration {
    #[serde(rename = "once")]
    Once,
    #[serde(rename = "repeating")]
    Repeating,
    #[serde(rename = "forever")]
    Forever,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// Body of `PUT /billing/coupons/{code}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CouponInput {
    /// Redeemable (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
    /// Amount off (kind `amount`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount_off: Option<i64>,
    /// Currency of `amount_off`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<CouponDuration>,
    /// Invoices (repeating).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_cycles: Option<i64>,
    /// Last moment it can be redeemed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Percent or amount.
    pub kind: CouponKind,
    /// Redemption cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_redemptions: Option<i64>,
    /// Name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Percent off in basis points (kind `percent`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percent_bp: Option<i64>,
    /// Product keys it applies to (default: all).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub products: Option<Vec<String>>,
}

/// Coupon type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CouponKind {
    #[serde(rename = "percent")]
    Percent,
    #[serde(rename = "amount")]
    Amount,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// Body of `POST /system/extensions`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CreateExtension {
    /// An allowed extension name.
    pub name: String,
}

/// Body of `POST /system/backups`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CreateSnapshot {
    /// Include derived index files (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_indexes: Option<bool>,
    /// Free-form label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention: Option<Retention>,
}

/// Body of a top-up or grant.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CreditInput {
    /// Credits (at least 1).
    pub amount: i64,
    /// When the credits lapse (default: never).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Order, payment or invoice reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_ref: Option<String>,
    /// Idempotency key (unique per subject; FB-70: the payment or event id).
    pub idempotency_key: String,
    /// Caller data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    /// Why.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Subject (`org:<id>` or a principal key).
    pub subject: String,
}

/// A credit note.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CreditNote {
    /// Amount credited.
    pub amount: i64,
    /// Of which credited to the customer balance.
    pub balance_amount: i64,
    /// Created.
    pub created_at: String,
    /// FB-73 credits taken back (credit top-ups refunded).
    pub credits_reversed: i64,
    /// Currency.
    pub currency: String,
    /// Id.
    pub id: String,
    /// Invoice credited.
    pub invoice_id: String,
    /// Number.
    pub number: String,
    /// Why.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refund: Option<Refund>,
    /// Of which refunded to the payment method.
    pub refund_amount: i64,
    /// Subject.
    pub subject: String,
}

/// Body of `POST /billing/invoices/{id}/credit-notes`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CreditNoteInput {
    /// Amount (default: all that is left to credit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount: Option<i64>,
    /// A replay with the same key returns the first credit note.
    pub idempotency_key: String,
    /// Why.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Credit the customer balance instead of refunding the payment method.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_balance: Option<bool>,
}

/// Totals by entry kind over a period.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CreditSummary {
    /// Balance now (the subject's, or of every account).
    pub balance: i64,
    /// Credits charged for usage.
    pub consumption: i64,
    /// Credits lapsed.
    pub expiry: i64,
    /// Start (inclusive).
    pub from: String,
    /// Credits granted.
    pub grant: i64,
    /// Net change in the period.
    pub net: i64,
    /// Credits refunded.
    pub refund: i64,
    /// Credits reversed.
    pub reversal: i64,
    /// Usage cost not covered by balances in the period.
    pub shortfall: i64,
    /// Credits spent directly.
    pub spend: i64,
    /// Subject (`null`: the whole instance).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// End (exclusive).
    pub to: String,
    /// Credits topped up.
    pub top_up: i64,
}

/// What a maintenance run did.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CreditsMaintenanceReport {
    /// Expiries.
    pub expired: ExpiryReport,
    /// Usage consumption.
    pub usage: UsageRun,
}

/// The amounts of a price in one more currency.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CurrencyOption {
    /// Tiers (tiered schemes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tiers: Option<Vec<Tier>>,
    /// Price per unit, minor units.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit_amount: Option<i64>,
}

/// The consumer cursor.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Cursor {
    /// Last usage event `seq` read.
    pub seq: i64,
}

/// An enterprise contract limit.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CustomLimit {
    /// Included (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Feature key.
    pub feature: String,
    /// Limit (`null`: unlimited).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period: Option<Period>,
    /// Why (contract reference).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Subject.
    pub subject: String,
    /// Rolling window length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_secs: Option<i64>,
}

/// A customer: one billing subject (`org:<id>` or a principal key).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Customer {
    /// Credit owed to the customer, minor units of `currency`.
    pub balance: i64,
    /// Created.
    pub created_at: String,
    /// Currency of invoices.
    pub currency: String,
    /// Billing email.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// A saved payment method is on file (a provider reference, never card data).
    pub has_payment_method: bool,
    /// Id.
    pub id: String,
    /// Caller data.
    #[serde(default)]
    pub metadata: serde_json::Value,
    /// Billing name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Its label (`visa ••4242`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payment_method_label: Option<String>,
    /// Payment provider (`stripe`, `razorpay`, `manual`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// The customer at the provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_customer_id: Option<String>,
    /// Tax region.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    /// Subject billed.
    pub subject: String,
    /// No tax is charged.
    pub tax_exempt: bool,
}

/// Body of `PUT /billing/customers`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CustomerInput {
    /// Currency of invoices (default: the settings' default currency; fixed
    /// once a subscription or order exists).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    /// Billing email.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// Caller data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    /// Billing name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Payment provider (default: the only configured one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Tax region (`IN`, `US-CA`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    /// Subject (default: the caller's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// Operators: tax exempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tax_exempt: Option<bool>,
}

/// `POST /graph/cypher/drop`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CypherDropped {
    /// Whether there was a derived graph to drop.
    pub dropped: bool,
}

/// `POST /graph/cypher`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CypherQuery {
    /// Most rows to return, 1 to 1000 (default 100).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    /// Query parameters (`$name` in the query), a JSON object.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
    /// One read-only openCypher query with a final `RETURN` (no `RETURN *`).
    pub query: String,
    /// Statement timeout in milliseconds, 1 to 30 000 (default 10 000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<i64>,
}

/// The rows of an openCypher query.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CypherResult {
    /// One name per `RETURN` item (its alias, else its text).
    #[serde(default)]
    pub columns: Vec<String>,
    /// The engine that answered: `age` (Apache AGE on the derived graph) or
    /// `portable` (SQL over the canonical tables, ADR-0034).
    pub engine: String,
    /// The rows: one JSON value per column. Vertices come back as
    /// `{id, label, properties}`, edges as `{id, type, source, target,
    /// directed, properties}` and paths as `{nodes, edges}`, with ForgeBase
    /// ids.
    #[serde(default)]
    pub rows: Vec<Vec<serde_json::Value>>,
    /// More rows matched than `limit`.
    pub truncated: bool,
}

/// The state of a tenant's derived openCypher graph (in `GET /graph/stats`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CypherStatus {
    /// Rows copied so far.
    pub done: i64,
    /// Edges to copy.
    pub edges: i64,
    /// Why materialisation failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The AGE graph name.
    pub graph: String,
    /// The `graph.materialise` job, when one runs it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    /// Nodes to copy (when materialisation started).
    pub nodes: i64,
    /// `none` (never used), `materialising`, `ready`, `stale` (rebuilt on
    /// next use) or `failed`.
    pub state: String,
}

/// A named login, as listed.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DbLogin {
    /// SQL editor rights.
    pub admin: bool,
    /// Last claims refresh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claims_at: Option<String>,
    /// `CONNECTION LIMIT`.
    pub connection_limit: i64,
    /// When.
    pub created_at: String,
    /// Who created it.
    pub created_by: String,
    /// The claims are present (`false`: the principal or tenant is gone, or
    /// an admin login, whose sessions bypass row-level security).
    pub has_claims: bool,
    /// `idle_in_transaction_session_timeout` (ms).
    pub idle_timeout_ms: i64,
    /// Name.
    pub name: String,
    /// Principal key it acts as.
    pub principal: String,
    /// Reads only.
    pub readonly: bool,
    /// PostgreSQL role (`fbu_<name>`).
    pub role: String,
    /// Last password rotation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotated_at: Option<String>,
    /// `statement_timeout` (ms).
    pub statement_timeout_ms: i64,
    /// Tenant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    /// Expiry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_until: Option<String>,
}

/// `{items}` of `GET /system/db-logins`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DbLoginList {
    /// Logins, by name.
    #[serde(default)]
    pub items: Vec<DbLogin>,
}

/// A message that used up its delivery attempts.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DeadLetter {
    /// When (RFC 3339).
    pub at: String,
    /// Deliveries made.
    pub attempts: i64,
    /// Attributes.
    #[serde(default)]
    pub attributes: std::collections::HashMap<String, String>,
    /// Data.
    #[serde(default)]
    pub data: serde_json::Value,
    /// Dead-letter topic it was forwarded to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dead_letter_topic: Option<String>,
    /// Id of the copy on the dead-letter topic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forwarded_message_id: Option<String>,
    /// Every delivery attempt.
    #[serde(default)]
    pub history: Vec<DeliveryAttempt>,
    /// Record id.
    pub id: i64,
    /// Message id.
    pub message_id: String,
    /// Ordering key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ordering_key: Option<String>,
    /// Published (RFC 3339).
    pub published_at: String,
    /// Why it was dead-lettered.
    pub reason: String,
    /// When it was redriven to the subscription (RFC 3339); a record is redriven once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redriven_at: Option<String>,
    /// Who redrove it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redriven_by: Option<String>,
    /// Subscription it failed on.
    pub subscription: String,
    /// Its topic.
    pub topic: String,
}

/// Dead letters of a subscription.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DeadLetterPage {
    /// Newest first.
    #[serde(default)]
    pub items: Vec<DeadLetter>,
}

/// A quota decision with the counter state after it.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Decision {
    /// Allowed.
    pub allowed: bool,
    /// Feature.
    pub feature: String,
    /// Allowance of the window: limit plus rolled-over units (`null`: unlimited).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    /// Units of this request past the limit of a soft quota.
    pub overage_units: i64,
    /// Usage-pack units left.
    pub pack_remaining: i64,
    /// Units of this request taken from usage packs.
    pub pack_units: i64,
    /// Why not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Left in the window plus usage packs (`null`: unlimited).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining: Option<i64>,
    /// Held by open reservations.
    pub reserved: i64,
    /// Counter scope key (`""`: the subject's counter).
    pub scope_key: String,
    /// Subject.
    pub subject: String,
    /// Units asked for.
    pub units: i64,
    /// Committed usage in the window (gauges: the level; concurrency: active leases).
    pub used: i64,
    /// Window end (`null`: none).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_end: Option<String>,
    /// Window start.
    pub window_start: String,
}

/// Body of a batch delete.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DeleteRequest {
    /// 1 to 1000 ids; unknown ids are ignored.
    #[serde(default)]
    pub ids: Vec<String>,
}

/// What a delete did.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DeleteResult {
    /// `true` when a delete marker was added (versioned bucket).
    pub delete_marker: bool,
    /// The version removed, or the delete marker created.
    pub version_id: String,
}

/// One row of the delivery log (never the body).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Delivery {
    /// Attempts made.
    pub attempts: i64,
    /// Queued (epoch ms).
    pub created_at: i64,
    /// Id.
    pub id: String,
    /// Queue job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    /// Last failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Provider that handled it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// The provider's id of the message (its own dashboard and logs use it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_message_id: Option<String>,
    /// Realm.
    pub realm: String,
    /// Handed over (epoch ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_at: Option<i64>,
    /// State.
    pub status: DeliveryStatus,
    /// Subject as sent.
    pub subject: String,
    /// Template key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    /// Recipient.
    pub to: String,
    /// Last change (epoch ms).
    pub updated_at: i64,
}

/// One delivery attempt of a message to a subscription.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DeliveryAttempt {
    /// When it was handed out (RFC 3339).
    pub at: String,
    /// Attempt number, from 1.
    pub attempt: i64,
    /// Consumer it went to (`rest`, a stream id, `push`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consumer: Option<String>,
    /// Push or nack error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// `nacked`, `expired`, `failed` (push), `released`; absent while outstanding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
}

/// State of a delivery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeliveryStatus {
    #[serde(rename = "queued")]
    Queued,
    #[serde(rename = "retrying")]
    Retrying,
    #[serde(rename = "sent")]
    Sent,
    #[serde(rename = "failed")]
    Failed,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// Body of `POST /api/v1/functions`: deploys a new version (creating the
/// function on its first deploy) and, unless `activate` is `false`, makes it
/// the active version. Send either `wasm` or `source` (JavaScript or
/// TypeScript, with `language`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DeployRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<InvokeAccess>,
    /// Make the new version active (default `true`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activate: Option<bool>,
    /// Granted capabilities.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Vec<String>>,
    /// Description (kept from the previous deploy when omitted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Plain environment values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<std::collections::HashMap<String, String>>,
    /// Allowed fetch hosts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetch_hosts: Option<Vec<String>>,
    /// Record hooks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hooks: Option<Vec<HookSpec>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<Language>,
    /// Limits (defaults for missing fields).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<LimitsInput>,
    /// Function name.
    pub name: String,
    /// JavaScript or TypeScript source of a single-file ES module that calls
    /// `serve()` from `"forgebase"` (at most 1 MiB).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// The WebAssembly module (binary format), base64.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wasm: Option<String>,
}

/// A deploy result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Deployed {
    /// The function after the deploy.
    pub function: FunctionInfo,
    /// The new version.
    pub version: VersionInfo,
}

/// Which way edges are followed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Direction {
    #[serde(rename = "out")]
    Out,
    #[serde(rename = "in")]
    In,
    #[serde(rename = "both")]
    Both,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// A custom domain of a realm.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Domain {
    /// Created (epoch ms).
    pub created_at: i64,
    /// Who added it.
    pub created_by: String,
    /// Host name (lower case).
    pub host: String,
    /// Id.
    pub id: String,
    /// Last verification attempt (epoch ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_checked: Option<i64>,
    /// Why the last attempt failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Realm it routes to.
    pub realm: String,
    /// State.
    pub status: DomainStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<Verification>,
    /// Verified (epoch ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_at: Option<i64>,
}

/// Adds a domain.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DomainInput {
    /// Host name, e.g. `login.example.com` (no scheme, port or path).
    pub host: String,
    /// Realm (platform administrators; default: the caller's realm).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub realm: Option<String>,
}

/// State of a custom domain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DomainStatus {
    #[serde(rename = "pending")]
    Pending,
    #[serde(rename = "verified")]
    Verified,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// An unpaid invoice that dunning is working on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DunningEntry {
    /// Amount still due.
    pub amount_due: i64,
    /// Payment attempts so far.
    pub attempt_count: i64,
    /// Invoice created.
    pub created_at: String,
    /// Currency.
    pub currency: String,
    /// Customer id.
    pub customer_id: String,
    /// What happens after the last retry.
    pub final_action: FinalAction,
    /// Invoice id.
    pub invoice_id: String,
    /// Code of the last failed payment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_failure_code: Option<String>,
    /// Message of the last failed payment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_failure_message: Option<String>,
    /// Next automatic attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_attempt_at: Option<String>,
    /// Invoice number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number: Option<String>,
    /// Retries left on the dunning schedule.
    pub retries_left: i64,
    /// Subject.
    pub subject: String,
    /// Subscription id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription_status: Option<BillingSubscriptionStatus>,
}

/// A typed edge between two nodes.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Edge {
    /// RFC 3339 creation time.
    pub created_at: String,
    /// `false` for an undirected relationship (walked both ways).
    pub directed: bool,
    /// Edge id (unique per tenant).
    pub id: String,
    /// JSON object of properties.
    #[serde(default)]
    pub properties: serde_json::Value,
    /// Source node id.
    pub source: String,
    /// Target node id.
    pub target: String,
    /// Relationship type, e.g. `FOLLOWS`.
    pub r#type: String,
    /// RFC 3339 time of the last change.
    pub updated_at: String,
}

/// Body of `PATCH /edges/{id}`: only properties can change.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct EdgeChange {
    /// JSON merge patch (RFC 7396; `null` removes a key).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub properties: Option<serde_json::Value>,
}

/// A page of edges.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct EdgePage {
    #[serde(default)]
    pub items: Vec<Edge>,
    /// Pass as `after` for the next page; absent on the last page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_after: Option<String>,
}

/// An email template.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct EmailTemplate {
    /// The realm overrides the built-in template.
    pub customized: bool,
    /// HTML body template.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub html: Option<String>,
    /// Key (`verify_email`, `password_reset`, `magic_link`, `email_otp`, `invitation`, or your own).
    pub key: String,
    /// Realm.
    pub realm: String,
    /// Subject template.
    pub subject: String,
    /// Plain-text body template.
    pub text: String,
    /// Last change (epoch ms; built-ins: none).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
    /// Variables the template uses.
    #[serde(default)]
    pub variables: Vec<String>,
}

/// Hard or soft limit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Enforcement {
    #[serde(rename = "hard")]
    Hard,
    #[serde(rename = "soft")]
    Soft,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// The database engine that wrote a backup (ADR-0028 §5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EngineInfo {
    /// `sqlite` or `postgres`.
    pub kind: String,
    /// PostgreSQL: major version. A physical backup restores on this major only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub major: Option<i64>,
    /// How the database was copied.
    pub method: Method,
    /// PostgreSQL: `managed` (the bundled engine) or `external`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// PostgreSQL: server version, such as `18.3`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// Effective entitlements of a subject (and member).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Entitlements {
    /// Whether the subscription entitles (else the default plan applies).
    pub entitled: bool,
    /// Grants by feature key.
    #[serde(default)]
    pub features: std::collections::HashMap<String, Grant>,
    /// Member the grants were resolved for (member overrides).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    /// Plan in force (`null`: none, not even a default plan).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<SubscriptionStatus>,
    /// Subject.
    pub subject: String,
    /// Trial end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trial_ends_at: Option<String>,
}

/// What a maintenance run did.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct EntitlementsMaintenanceReport {
    /// Undelivered alerts queued again.
    pub alerts_requeued: i64,
    /// Leases marked expired.
    pub leases_expired: i64,
    /// Reservations released because they expired.
    pub reservations_expired: i64,
    /// Trials that ended without conversion.
    pub trials_expired: i64,
    /// Counter windows opened (with rollover).
    pub windows_opened: i64,
}

/// What a ledger entry records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryKind {
    #[serde(rename = "top_up")]
    TopUp,
    #[serde(rename = "grant")]
    Grant,
    #[serde(rename = "consumption")]
    Consumption,
    #[serde(rename = "spend")]
    Spend,
    #[serde(rename = "refund")]
    Refund,
    #[serde(rename = "reversal")]
    Reversal,
    #[serde(rename = "expiry")]
    Expiry,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// Error body (the standard envelope).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ErrorBody {
    /// `{code, message, request_id}`.
    pub error: RealtimeErrorDetail,
}

/// The `error` object of the envelope.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ErrorDetail {
    /// Stable machine-readable code.
    pub code: String,
    /// Structured context, e.g. per-field validation errors
    /// (`{"fields": {"name": {"code": "required", "message": "..."}}}`).
    /// Additive and optional (ADR-0001 §6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
    /// Human-readable message.
    pub message: String,
    /// Id of the request (also in the `x-request-id` response header).
    pub request_id: String,
}

/// The whole error body.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ErrorEnvelope {
    /// The error.
    pub error: ErrorDetail,
}

/// The subscribable event types: every topic the caller may read.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct EventCatalog {
    /// By topic name.
    #[serde(default)]
    pub topics: Vec<CatalogTopic>,
}

/// History of a job.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct EventList {
    /// Oldest first.
    #[serde(default)]
    pub items: Vec<JobEvent>,
}

/// Where an example in the catalog comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExampleSource {
    #[serde(rename = "topic")]
    Topic,
    #[serde(rename = "schema")]
    Schema,
    #[serde(rename = "none")]
    None,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// Body of `POST /exec`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ExecBody {
    /// Command name followed by arguments, e.g. `["INCRBY", "hits", "5"]`.
    #[serde(default)]
    pub command: Vec<Bin>,
}

/// Execution logs.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ExecutionList {
    /// Newest first.
    #[serde(default)]
    pub items: Vec<ExecutionLog>,
}

/// One recorded execution.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ExecutionLog {
    /// When it started (RFC 3339).
    pub created_at: String,
    /// Wall-clock duration.
    pub duration_ms: i64,
    /// Error message, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Fuel consumed.
    pub fuel_used: i64,
    /// Function name.
    pub function: String,
    /// Id (increasing).
    pub id: i64,
    /// Lines the function logged (capped).
    #[serde(default)]
    pub logs: Vec<LogLine>,
    /// Request id of the triggering request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// `ok`, `rejected`, `failed`, `trap`, `fuel_exhausted`, `memory_limit`, `timeout` or `bad_output`.
    pub status: String,
    /// Trace id of the run (32 hex digits; ADR-0035), when it ran in a trace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    /// `http`, `hook`, `job` or `test`.
    pub trigger: String,
    /// Version that ran.
    pub version: i64,
}

/// Credits lapsing at one time.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Expiring {
    /// Credits.
    pub amount: i64,
    /// When.
    pub expires_at: String,
}

/// What an expiry run did.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ExpiryReport {
    /// Credits lapsed.
    pub credits: i64,
    /// Lots emptied.
    pub lots: i64,
}

/// One extension of the allow list or of a feature.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ExtensionInfo {
    /// The engine ships it (`pg_available_extensions`).
    pub available: bool,
    /// The engine's description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// Version the engine offers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_version: Option<String>,
    /// Installed version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_version: Option<String>,
    /// Who manages it: `api` (this endpoint), `schema` (ForgeBase's own
    /// migrations), or `feature:<name>` (`[database.postgres.extensions]`).
    pub managed_by: String,
    /// `CREATE EXTENSION` name.
    pub name: String,
    /// Schema it lives in, when installed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
}

/// `{items}` of `GET /system/extensions`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ExtensionList {
    /// The allow list and the feature extensions, by name.
    #[serde(default)]
    pub items: Vec<ExtensionInfo>,
}

/// Body of a failure report.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct FailBody {
    /// What went wrong.
    pub error: String,
    /// The lease from the claim.
    pub lease_id: String,
    /// `false` sends the job straight to the dead-letter queue (default `true`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retryable: Option<bool>,
}

/// What a failed workflow step does to its workflow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FailurePolicy {
    #[serde(rename = "fail")]
    Fail,
    #[serde(rename = "compensate")]
    Compensate,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// A feature of the catalog.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Feature {
    /// Created.
    pub created_at: String,
    /// Description.
    pub description: String,
    /// Key (`ai_generation`, `storage`, `api_access`).
    pub key: String,
    /// What it measures.
    pub kind: FeatureKind,
    /// Display name.
    pub name: String,
    /// Permission patterns this feature gates (pipeline step 4). A key
    /// `org:<feature>:<permission>` is gated by feature `<feature>` without
    /// being listed.
    #[serde(default)]
    pub permissions: Vec<String>,
    /// Unit shown with limits (`requests`, `bytes`).
    pub unit: String,
    /// Last change.
    pub updated_at: String,
}

/// Body of `PUT /features/{key}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeatureInput {
    /// Description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// What it measures.
    pub kind: FeatureKind,
    /// Display name (default: the key).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Gated permission patterns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<Vec<String>>,
    /// Unit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
}

/// What a feature measures.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FeatureKind {
    #[serde(rename = "boolean")]
    Boolean,
    #[serde(rename = "metered")]
    Metered,
    #[serde(rename = "gauge")]
    Gauge,
    #[serde(rename = "concurrency")]
    Concurrency,
    #[serde(rename = "rate")]
    Rate,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// Vector fields of a collection.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct FieldList {
    /// By name.
    #[serde(default)]
    pub items: Vec<VectorField>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct FieldSchema {
    /// `relation`: delete this record when the referenced record is deleted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cascade_delete: Option<bool>,
    /// `relation`: the target collection's id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
    /// `vector`: number of dimensions (1 to 16 000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<i64>,
    pub hidden: bool,
    /// Stable id; renames keep it.
    pub id: String,
    /// `number`: integers only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integer: Option<bool>,
    /// `array`: item type, `text`, `number`, `bool` or `json` (default `json`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<String>,
    /// `number`: maximum value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// `array`: maximum number of items.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_items: Option<i64>,
    /// `text`: maximum length in characters (default 1 000 000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_length: Option<i64>,
    /// `select`, `relation`, `file`: values that may be chosen at once (default 1; above 1 the field holds an array).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_select: Option<i64>,
    /// `json`: maximum serialised size in bytes (default 1 MiB).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_size: Option<i64>,
    /// `number`: minimum value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    /// `array`: minimum number of items.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_items: Option<i64>,
    /// `text`: minimum length in characters; `password`: minimum length (default 8).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_length: Option<i64>,
    pub name: String,
    pub required: bool,
    pub system: bool,
    /// `text`, `number`, `bool`, `date`, `email`, `url`, `select`, `json`,
    /// `array`, `relation`, `file`, `password`, `geo`, `vector`.
    pub r#type: String,
    pub unique: bool,
    /// `select`: the allowed values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub values: Option<Vec<String>>,
}

/// Definition of a vector field, as sent when creating it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldSpec {
    /// HNSW links per node (default 16, 2 to 128).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connectivity: Option<i64>,
    /// Number of components, 1 to 4096.
    pub dimension: i64,
    /// HNSW candidate list size while inserting (default 128).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expansion_add: Option<i64>,
    /// HNSW candidate list size while searching (default 128). Higher means
    /// better recall and slower searches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expansion_search: Option<i64>,
    /// Distance function.
    pub metric: Metric,
    /// `[A-Za-z][A-Za-z0-9_]{0,62}`.
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantization: Option<Quantization>,
}

/// One file of a backup.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileEntry {
    /// Role of the file.
    pub kind: FileKind,
    /// `/`-separated path relative to the backup directory.
    pub path: String,
    /// Lower-case hex SHA-256.
    pub sha256: String,
    /// Size in bytes.
    pub size: i64,
}

/// What a file is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileKind {
    #[serde(rename = "database")]
    Database,
    #[serde(rename = "storage")]
    Storage,
    #[serde(rename = "index")]
    Index,
    #[serde(rename = "key")]
    Key,
    #[serde(rename = "postgres")]
    Postgres,
    #[serde(rename = "dump")]
    Dump,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// When a metadata filter is applied relative to the vector search.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilterMode {
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "pre")]
    Pre,
    #[serde(rename = "post")]
    Post,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// What dunning does after the last failed retry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FinalAction {
    #[serde(rename = "cancel")]
    Cancel,
    #[serde(rename = "leave_past_due")]
    LeavePastDue,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// What one flush wrote.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct FlushReport {
    /// Function (name, version) rows written.
    pub functions: i64,
    /// Slow statements dropped by the sampling budget.
    pub slow_dropped: i64,
    /// Slow statements written.
    pub slow_statements: i64,
    /// Statement fingerprints written.
    pub statements: i64,
}

/// A function over a range: its versions and its time series.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct FunctionDetail {
    /// Function name.
    pub function: String,
    /// Buckets of all versions, oldest first.
    #[serde(default)]
    pub series: Vec<FunctionPoint>,
    /// Statistics per version, newest version first.
    #[serde(default)]
    pub versions: Vec<FunctionStat>,
}

/// A function.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunctionInfo {
    /// HTTP access mode.
    pub access: InvokeAccess,
    /// The version that runs.
    pub active_version: i64,
    /// Created (RFC 3339).
    pub created_at: String,
    /// Who created it.
    pub created_by: String,
    /// Description.
    pub description: String,
    /// Id.
    pub id: String,
    /// The newest version.
    pub latest_version: i64,
    /// Name.
    pub name: String,
    /// Identity the function acts as (`service:function.<name>`): grant it
    /// permissions with the policy API.
    pub principal: String,
    /// Last deploy or rollback (RFC 3339).
    pub updated_at: String,
}

/// Functions.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct FunctionList {
    /// By name.
    #[serde(default)]
    pub items: Vec<FunctionInfo>,
}

/// One time bucket of a function.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct FunctionPoint {
    /// Start of the bucket (RFC 3339).
    pub at: String,
    /// Cold starts.
    pub cold_starts: i64,
    /// Failures.
    pub errors: i64,
    /// Executions.
    pub invocations: i64,
    /// Mean duration in milliseconds.
    pub mean_ms: f64,
    /// 95th percentile in milliseconds.
    pub p95_ms: f64,
}

/// How a version runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FunctionRuntime {
    #[serde(rename = "wasm")]
    Wasm,
    #[serde(rename = "js")]
    Js,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// Statistics of one function version over a range.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct FunctionStat {
    /// Executions that had to load their module or script.
    pub cold_starts: i64,
    /// `errors / invocations`.
    pub error_rate: f64,
    /// Executions that did not end `ok`.
    pub errors: i64,
    /// The fuel limit of the version.
    pub fuel_limit: i64,
    /// Most fuel one execution used.
    pub fuel_max: i64,
    /// Mean fuel (WebAssembly instructions, roughly) per execution.
    pub fuel_mean: i64,
    /// `fuel_max / fuel_limit`: how close the worst execution came.
    pub fuel_utilisation: f64,
    /// Function name.
    pub function: String,
    /// Host calls by operation, most time first.
    #[serde(default)]
    pub host: Vec<HostBreakdown>,
    /// Host calls per execution, on average.
    pub host_calls_per_invocation: f64,
    /// Executions.
    pub invocations: i64,
    /// Longest execution in milliseconds.
    pub max_ms: f64,
    /// Mean duration in milliseconds.
    pub mean_ms: f64,
    /// The memory limit in bytes.
    pub memory_limit_bytes: i64,
    /// Largest memory one execution had, in bytes.
    pub memory_max_bytes: i64,
    /// Median duration in milliseconds (within about 11 %).
    pub p50_ms: f64,
    /// 95th percentile in milliseconds.
    pub p95_ms: f64,
    /// 99th percentile in milliseconds.
    pub p99_ms: f64,
    /// Executions that hit their time limit.
    pub timeouts: i64,
    /// Version.
    pub version: i64,
}

/// Functions with their versions, most executions first.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct FunctionStatList {
    /// Start of the range (RFC 3339).
    pub from: String,
    /// One row per function version.
    #[serde(default)]
    pub items: Vec<FunctionStat>,
    /// End of the range, exclusive.
    pub to: String,
}

/// Body of a gauge change: `delta` (may be negative) or the new `value`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct GaugeRequest {
    /// Change of the level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta: Option<i64>,
    /// Feature key (a gauge).
    pub feature: String,
    /// Required: a replay returns the first result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    /// Stored with the usage event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    /// Counter scope key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_key: Option<String>,
    /// Subject (default: the caller's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// New level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<i64>,
}

/// A subject's effective grant of one feature.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Grant {
    /// Included.
    pub enabled: bool,
    /// Kind of the feature.
    pub kind: FeatureKind,
    /// Limit per window (`null`: unlimited).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    /// Window.
    pub period: Period,
    /// Strongest source applied.
    pub source: GrantSource,
    /// Unit.
    pub unit: String,
    /// Rolling window length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_secs: Option<i64>,
}

/// Where a feature's grant comes from (strongest last applied).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GrantSource {
    #[serde(rename = "plan")]
    Plan,
    #[serde(rename = "trial")]
    Trial,
    #[serde(rename = "addon")]
    Addon,
    #[serde(rename = "custom")]
    Custom,
    #[serde(rename = "override")]
    Override,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// Counts for the admin explorer.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct GraphStats {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cypher: Option<CypherStatus>,
    /// Edge count per relationship type.
    #[serde(default)]
    pub edge_types: Vec<TypeCount>,
    /// Number of edges.
    pub edges: i64,
    /// Number of nodes.
    pub nodes: i64,
}

/// Answer of `has()`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct HasAnswer {
    /// The answer.
    pub has: bool,
    /// Why not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Body of `POST .../hashes/{key}/delete`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct HashDeleteBody {
    /// Field names to remove.
    #[serde(default)]
    pub fields: Vec<String>,
}

/// Body of `POST .../hashes/{key}/incr`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct HashIncrBody {
    /// Amount to add (default 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<i64>,
    /// Field to change.
    pub field: String,
}

/// Body of `POST .../hashes/{key}/set`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct HashSetBody {
    /// Field names and values.
    #[serde(default)]
    pub fields: std::collections::HashMap<String, Bin>,
}

/// Answer of a heartbeat.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Heartbeat {
    /// The job was cancelled: stop and call `fail`.
    pub cancel_requested: bool,
    /// When the attempt times out (RFC 3339), if the job has a timeout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_at: Option<String>,
    /// New lease expiry (RFC 3339).
    pub visibility_until: String,
}

/// A record event a hook function runs on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HookEvent {
    #[serde(rename = "before_create")]
    BeforeCreate,
    #[serde(rename = "after_create")]
    AfterCreate,
    #[serde(rename = "before_update")]
    BeforeUpdate,
    #[serde(rename = "after_update")]
    AfterUpdate,
    #[serde(rename = "before_delete")]
    BeforeDelete,
    #[serde(rename = "after_delete")]
    AfterDelete,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// Record events of one collection that run a function.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct HookSpec {
    /// Collection name.
    pub collection: String,
    /// Events.
    #[serde(default)]
    pub events: Vec<HookEvent>,
}

/// Host calls of one operation.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct HostBreakdown {
    /// Calls.
    pub count: i64,
    /// Calls that returned an error to the function.
    pub errors: i64,
    /// Longest call in milliseconds.
    pub max_ms: f64,
    /// Mean time in milliseconds.
    pub mean_ms: f64,
    /// The operation (`db.get`, `kv`, `fetch`, `storage.put`, ...).
    pub op: String,
    /// Share of all the function's host time, 0 to 1.
    pub share: f64,
    /// Total time in milliseconds.
    pub total_ms: f64,
}

/// A provider-hosted page.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct HostedPage {
    /// Provider session id.
    pub session_id: String,
    /// Open this URL.
    pub url: String,
}

/// One hybrid search result.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct HybridHit {
    /// Item id.
    pub id: String,
    /// Metadata, unless `include_metadata` is false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    /// Fused RRF score (higher is better).
    pub score: f64,
    /// Text, when `include_text` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// 1-based rank in the keyword ranking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_rank: Option<i64>,
    /// BM25 relevance in the keyword ranking (higher is better).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_score: Option<f64>,
    /// Vector, when `include_vectors` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vector: Option<Vec<f64>>,
    /// Distance in the vector ranking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vector_distance: Option<f64>,
    /// 1-based rank in the vector ranking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vector_rank: Option<i64>,
}

/// A hybrid search: keyword (FTS5 and BM25 on SQLite, `tsvector` on PostgreSQL) and vector rankings fused with
/// Reciprocal Rank Fusion: `score = Σ weight / (rrf_k + rank)`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct HybridRequest {
    /// Candidates taken from each ranking before fusion (default
    /// `max(5 k, 50)`, at most 1000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidates: Option<i64>,
    /// Vector field; may be omitted when the collection has exactly one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// Metadata filter, applied to both rankings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    /// When the filter is applied on the vector side.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter_mode: Option<FilterMode>,
    /// Return each hit's metadata (default `true`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_metadata: Option<bool>,
    /// Return each hit's text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_text: Option<bool>,
    /// Return each hit's vector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_vectors: Option<bool>,
    /// Number of results, 1 to 1000 (default 10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub k: Option<i64>,
    /// Values for `{:name}` parameters of the filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
    /// RRF constant (default 60).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rrf_k: Option<f64>,
    /// Keyword query; every word is matched literally (optional:
    /// vector-only without it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// How the words combine (default `or`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_operator: Option<TextOperator>,
    /// Query vector (optional: keyword-only without it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vector: Option<Vec<f64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weights: Option<HybridWeights>,
}

/// Hybrid search results.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct HybridResponse {
    /// Best first.
    #[serde(default)]
    pub hits: Vec<HybridHit>,
    /// Candidates from the keyword ranking.
    pub text_candidates: i64,
    /// Server-side time in milliseconds.
    pub took_ms: f64,
    /// Candidates from the vector ranking.
    pub vector_candidates: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vector_strategy: Option<Strategy>,
}

/// Weights of the two rankings in hybrid search.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct HybridWeights {
    /// Weight of the keyword ranking (default 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<f64>,
    /// Weight of the vector ranking (default 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vector: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ImportResult {
    /// `json`, `ndjson` or `csv`.
    pub format: String,
    /// Records stored.
    pub imported: i64,
}

/// Body of `POST .../strings/{key}/incr`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct IncrBody {
    /// Amount to add (default 1; negative to decrement).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct IndexDefinitionSchema {
    pub collection: String,
    #[serde(default)]
    pub keys: Vec<IndexKeySchema>,
    /// `btree` (default), `fulltext` or `trigram` (pg_trgm where it is in
    /// use, else the portable engine; ADR-0034).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partial: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unique: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct IndexInfoSchema {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,
    #[serde(default)]
    pub columns: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition: Option<IndexDefinitionSchema>,
    pub kind: String,
    pub name: String,
    /// `auto`, `manual`, `sql` or `engine`.
    pub origin: String,
    pub partial: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sql: Option<String>,
    pub stats: IndexStatsSchema,
    pub table: String,
    pub unique: bool,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct IndexKeySchema {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desc: Option<bool>,
    /// Field path in filter syntax: `price`, `metadata.brand`, `title:lower`.
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct IndexList {
    #[serde(default)]
    pub items: Vec<IndexInfoSchema>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct IndexRecommendationSchema {
    pub definition: IndexDefinitionSchema,
    pub max_duration_ms: f64,
    pub occurrences: i64,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct IndexRecommendations {
    #[serde(default)]
    pub items: Vec<IndexRecommendationSchema>,
}

/// Life cycle of a derived index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum IndexState {
    #[serde(rename = "unloaded")]
    Unloaded,
    #[serde(rename = "ready")]
    Ready,
    #[serde(rename = "building")]
    Building,
    #[serde(rename = "failed")]
    Failed,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct IndexStatsSchema {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows_estimate: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stat: Option<String>,
}

/// State and metrics of one field's ANN index.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexStatus {
    /// Searches answered by the ANN index since start.
    pub ann_searches: i64,
    /// Canonical vectors stored for the field.
    pub canonical_vectors: i64,
    /// Reserved slots of the serving index.
    pub capacity: i64,
    /// HNSW links per node.
    pub connectivity: i64,
    /// The index has changes not yet written to its file.
    pub dirty: bool,
    /// ANN engine.
    pub engine: String,
    /// ANN engine version.
    pub engine_version: String,
    /// Searches answered by brute force since start.
    pub exact_searches: i64,
    /// HNSW candidate list size while searching.
    pub expansion_search: i64,
    /// Vector field.
    pub field: String,
    /// Vectors in the serving index.
    pub indexed_vectors: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_rebuild: Option<RebuildInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_recall: Option<RecallReport>,
    /// When the file was last written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_saved: Option<String>,
    /// Mean search latency since start, in milliseconds.
    pub mean_search_ms: f64,
    /// Memory used by the serving index.
    pub memory_bytes: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<VectorProgress>,
    /// Element type of the index.
    pub quantization: Quantization,
    /// Why the index is not ready, or why the running rebuild started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Searches since start.
    pub searches: i64,
    /// Size of the index file when saved.
    pub serialized_bytes: i64,
    /// `ann` when searches use an index, `exact` when they scan.
    pub serving: String,
    /// Life cycle state.
    pub state: IndexState,
}

/// Bucket width of statistics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Interval {
    #[serde(rename = "hour")]
    Hour,
    #[serde(rename = "day")]
    Day,
    #[serde(rename = "month")]
    Month,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// One recorded execution with its profile (ADR-0036 §3).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct InvocationDetail {
    /// When it started (RFC 3339).
    pub created_at: String,
    /// Wall-clock duration.
    pub duration_ms: i64,
    /// Error message, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Fuel consumed.
    pub fuel_used: i64,
    /// Function name.
    pub function: String,
    /// Id (increasing).
    pub id: i64,
    /// Lines the function logged (capped).
    #[serde(default)]
    pub logs: Vec<LogLine>,
    /// Request id of the triggering request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// `ok`, `rejected`, `failed`, `trap`, `fuel_exhausted`, `memory_limit`, `timeout` or `bad_output`.
    pub status: String,
    /// Trace id of the run (32 hex digits; ADR-0035), when it ran in a trace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    /// `http`, `hook`, `job` or `test`.
    pub trigger: String,
    /// Version that ran.
    pub version: i64,
    /// What the host-call boundary measured: cold start, fuel and memory
    /// against their limits, per-operation counts and times, and a timeline
    /// of the first host calls. `null` for executions recorded before
    /// profiling existed or while it was off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<serde_json::Value>,
}

/// A page of recorded executions with their profiles, newest first.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct InvocationPage {
    /// The executions.
    #[serde(default)]
    pub items: Vec<InvocationDetail>,
    /// Pass as `cursor` for the next page; absent after the last one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<i64>,
}

/// An invoice.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Invoice {
    /// Left to collect.
    pub amount_due: i64,
    /// Collected.
    pub amount_paid: i64,
    /// Given back by credit notes.
    pub amount_refunded: i64,
    /// Collection attempts.
    pub attempt_count: i64,
    /// Customer credit applied.
    pub balance_applied: i64,
    /// `subscription_create`, `subscription_cycle`, `subscription_update`, `order`.
    pub billing_reason: String,
    /// Coupon code applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coupon: Option<String>,
    /// Created.
    pub created_at: String,
    /// Currency.
    pub currency: String,
    /// Customer id.
    pub customer_id: String,
    /// Discount.
    pub discount: i64,
    /// Finalized.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finalized_at: Option<String>,
    /// What was paid for was delivered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fulfilled_at: Option<String>,
    /// Hosted payment page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hosted_url: Option<String>,
    /// Id.
    pub id: String,
    /// Lines.
    #[serde(default)]
    pub lines: Vec<InvoiceLine>,
    /// Next dunning retry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_attempt_at: Option<String>,
    /// Number (set when finalized).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number: Option<String>,
    /// Order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_id: Option<String>,
    /// Paid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paid_at: Option<String>,
    /// Period end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period_end: Option<String>,
    /// Period start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period_start: Option<String>,
    /// State.
    pub status: InvoiceStatus,
    /// Subject.
    pub subject: String,
    /// Subscription.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription_id: Option<String>,
    /// Sum of the lines before discount (exclusive of added tax).
    pub subtotal: i64,
    /// Tax (added and included).
    pub tax: i64,
    /// Tax region.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tax_region: Option<String>,
    /// subtotal − discount + exclusive tax.
    pub total: i64,
}

/// An invoice line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InvoiceLine {
    /// Signed amount before discount (credits are negative).
    pub amount: i64,
    /// Text on the invoice.
    pub description: String,
    /// Discount on the line.
    pub discount_amount: i64,
    /// Metered feature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feature: Option<String>,
    /// Id.
    pub id: String,
    /// Type.
    pub kind: LineKind,
    /// Service period end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period_end: Option<String>,
    /// Service period start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period_start: Option<String>,
    /// Price key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price: Option<String>,
    /// Product key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product: Option<String>,
    /// Units.
    pub quantity: i64,
    /// Tax of the line.
    pub tax_amount: i64,
    /// Tax is part of `amount`.
    pub tax_inclusive: bool,
    /// Tax rate in basis points.
    pub tax_rate_bp: i64,
    /// Price per unit (per-unit prices).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit_amount: Option<i64>,
}

/// Invoice lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InvoiceStatus {
    #[serde(rename = "draft")]
    Draft,
    #[serde(rename = "open")]
    Open,
    #[serde(rename = "paid")]
    Paid,
    #[serde(rename = "void")]
    Void,
    #[serde(rename = "uncollectible")]
    Uncollectible,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// Who may invoke a function over HTTP.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InvokeAccess {
    #[serde(rename = "public")]
    Public,
    #[serde(rename = "authenticated")]
    Authenticated,
    #[serde(rename = "permission")]
    Permission,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// A stored item.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Item {
    /// Creation time.
    pub created: String,
    /// Id.
    pub id: String,
    /// Metadata object.
    #[serde(default)]
    pub metadata: serde_json::Value,
    /// Keyword-search text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Last replace.
    pub updated: String,
    /// Canonical vectors per field.
    #[serde(default)]
    pub vectors: std::collections::HashMap<String, Vec<f64>>,
    /// Incremented by every replace.
    pub version: i64,
}

/// A price and a quantity.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemInput {
    /// Price key.
    pub price: String,
    /// Units (default 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantity: Option<i64>,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsAcceptance {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsAcceptanceItemsItem>,
}

/// Acceptance record.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsAcceptanceItemsItem {
    /// When (epoch ms).
    pub accepted_at: i64,
    /// Id.
    pub id: String,
    /// From which address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
    /// Kind.
    pub kind: String,
    /// Who (principal key).
    pub principal: String,
    /// Realm.
    pub realm: String,
    /// With which client.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    /// Version.
    pub version: i64,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsAddon {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsAddonItemsItem>,
}

/// An add-on.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsAddonItemsItem {
    /// Can be assigned.
    pub active: bool,
    /// Description.
    pub description: String,
    /// Grants.
    #[serde(default)]
    pub features: Vec<AddonFeature>,
    /// Key.
    pub key: String,
    /// Display name.
    pub name: String,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsAgreement {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsAgreementItemsItem>,
}

/// One version of a legal agreement.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsAgreementItemsItem {
    /// Text (plain text or Markdown; escape when rendering).
    pub body: String,
    /// Id.
    pub id: String,
    /// Kind (`terms`, `privacy`, ...).
    pub kind: String,
    /// Published (epoch ms).
    pub published_at: i64,
    /// Who published it.
    pub published_by: String,
    /// Realm.
    pub realm: String,
    /// Title.
    pub title: String,
    /// Canonical URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Version (1, 2, ...).
    pub version: i64,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsBillingStoreProduct {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsBillingStoreProductItemsItem>,
}

/// A store product (App Store, Google Play, Creem) sold as a billing price.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsBillingStoreProductItemsItem {
    /// `subscription` (recurring price) or `one_time`.
    pub kind: String,
    /// Price key.
    pub price: String,
    /// Product id at the store.
    pub product_id: String,
    /// `app_store`, `google_play` or `creem`.
    pub provider: String,
    /// Changed.
    pub updated_at: String,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsBillingStorePurchase {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsBillingStorePurchaseItemsItem>,
}

/// A store purchase bound to a subject.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsBillingStorePurchaseItemsItem {
    /// Renews at the period end.
    pub auto_renew: bool,
    /// Created.
    pub created_at: String,
    /// `test` (sandbox, licence tester, Creem test mode).
    pub environment: String,
    /// End of the current period.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Id.
    pub id: String,
    /// `subscription` or `one_time`.
    pub kind: String,
    /// Newest store transaction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_transaction_id: Option<String>,
    /// The billing order (one-time purchases).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_id: Option<String>,
    /// Price key.
    pub price: String,
    /// Store product id.
    pub product_id: String,
    /// Store.
    pub provider: String,
    /// Stable purchase id at the store (original transaction id, purchase
    /// token, Creem subscription or order).
    pub purchase_id: String,
    /// Store state: `active`, `trialing`, `grace`, `on_hold`, `paused`,
    /// `expired`, `refunded`, `revoked`, `pending`, `replaced`.
    pub status: String,
    /// Subject.
    pub subject: String,
    /// The billing subscription it drives.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription_id: Option<String>,
    /// Changed.
    pub updated_at: String,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsBillingSubscription {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsBillingSubscriptionItemsItem>,
}

/// A subscription.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemsBillingSubscriptionItemsItem {
    /// Ends at the period end.
    pub cancel_at_period_end: bool,
    /// Why it was cancelled (`requested`, `payment_failed`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel_reason: Option<String>,
    /// When cancellation was asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canceled_at: Option<String>,
    /// Coupon code in force.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coupon: Option<String>,
    /// Discounted invoices left (repeating coupons).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coupon_cycles_left: Option<i64>,
    /// Created.
    pub created_at: String,
    /// Currency.
    pub currency: String,
    /// Current period end.
    pub current_period_end: String,
    /// Current period start.
    pub current_period_start: String,
    /// Customer id.
    pub customer_id: String,
    /// Cycle anchor (renewals keep its day of month and time).
    pub cycle_anchor: String,
    /// When it ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    /// Id.
    pub id: String,
    /// Interval.
    pub interval: BillingInterval,
    /// Intervals per period.
    pub interval_count: i64,
    /// Prices.
    #[serde(default)]
    pub items: Vec<SubscriptionItem>,
    /// Newest invoice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_invoice_id: Option<String>,
    /// Caller data.
    #[serde(default)]
    pub metadata: serde_json::Value,
    /// Price scheduled for the next period (downgrade at period end).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_price: Option<String>,
    /// Payment provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Lifecycle state.
    pub status: BillingSubscriptionStatus,
    /// Subject billed.
    pub subject: String,
    /// End of the trial.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trial_ends_at: Option<String>,
    /// Changes so far.
    pub version: i64,
}

/// Buckets.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsBuckets {
    /// Entries.
    #[serde(default)]
    pub items: Vec<Bucket>,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsCoupon {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsCouponItemsItem>,
}

/// A coupon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemsCouponItemsItem {
    /// Redeemable.
    pub active: bool,
    /// Amount off, minor units of `currency`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount_off: Option<i64>,
    /// Code customers enter.
    pub code: String,
    /// Currency of `amount_off`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    /// How long it applies.
    pub duration: CouponDuration,
    /// Invoices (repeating).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_cycles: Option<i64>,
    /// Last day it can be redeemed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Id.
    pub id: String,
    /// Percent or amount.
    pub kind: CouponKind,
    /// Redemption cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_redemptions: Option<i64>,
    /// Name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Percent off in basis points.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percent_bp: Option<i64>,
    /// Product keys it applies to (empty: all).
    #[serde(default)]
    pub products: Vec<String>,
    /// Times redeemed.
    pub redeemed: i64,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsCreditNote {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsCreditNoteItemsItem>,
}

/// A credit note.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsCreditNoteItemsItem {
    /// Amount credited.
    pub amount: i64,
    /// Of which credited to the customer balance.
    pub balance_amount: i64,
    /// Created.
    pub created_at: String,
    /// FB-73 credits taken back (credit top-ups refunded).
    pub credits_reversed: i64,
    /// Currency.
    pub currency: String,
    /// Id.
    pub id: String,
    /// Invoice credited.
    pub invoice_id: String,
    /// Number.
    pub number: String,
    /// Why.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refund: Option<Refund>,
    /// Of which refunded to the payment method.
    pub refund_amount: i64,
    /// Subject.
    pub subject: String,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsCustomLimit {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsCustomLimitItemsItem>,
}

/// An enterprise contract limit.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsCustomLimitItemsItem {
    /// Included (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Feature key.
    pub feature: String,
    /// Limit (`null`: unlimited).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period: Option<Period>,
    /// Why (contract reference).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Subject.
    pub subject: String,
    /// Rolling window length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_secs: Option<i64>,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsCustomer {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsCustomerItemsItem>,
}

/// A customer: one billing subject (`org:<id>` or a principal key).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsCustomerItemsItem {
    /// Credit owed to the customer, minor units of `currency`.
    pub balance: i64,
    /// Created.
    pub created_at: String,
    /// Currency of invoices.
    pub currency: String,
    /// Billing email.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// A saved payment method is on file (a provider reference, never card data).
    pub has_payment_method: bool,
    /// Id.
    pub id: String,
    /// Caller data.
    #[serde(default)]
    pub metadata: serde_json::Value,
    /// Billing name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Its label (`visa ••4242`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payment_method_label: Option<String>,
    /// Payment provider (`stripe`, `razorpay`, `manual`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// The customer at the provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_customer_id: Option<String>,
    /// Tax region.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    /// Subject billed.
    pub subject: String,
    /// No tax is charged.
    pub tax_exempt: bool,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsDelivery {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsDeliveryItemsItem>,
}

/// One row of the delivery log (never the body).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemsDeliveryItemsItem {
    /// Attempts made.
    pub attempts: i64,
    /// Queued (epoch ms).
    pub created_at: i64,
    /// Id.
    pub id: String,
    /// Queue job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    /// Last failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Provider that handled it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// The provider's id of the message (its own dashboard and logs use it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_message_id: Option<String>,
    /// Realm.
    pub realm: String,
    /// Handed over (epoch ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_at: Option<i64>,
    /// State.
    pub status: DeliveryStatus,
    /// Subject as sent.
    pub subject: String,
    /// Template key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    /// Recipient.
    pub to: String,
    /// Last change (epoch ms).
    pub updated_at: i64,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsDomain {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsDomainItemsItem>,
}

/// A custom domain of a realm.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemsDomainItemsItem {
    /// Created (epoch ms).
    pub created_at: i64,
    /// Who added it.
    pub created_by: String,
    /// Host name (lower case).
    pub host: String,
    /// Id.
    pub id: String,
    /// Last verification attempt (epoch ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_checked: Option<i64>,
    /// Why the last attempt failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Realm it routes to.
    pub realm: String,
    /// State.
    pub status: DomainStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<Verification>,
    /// Verified (epoch ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_at: Option<i64>,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsDunningEntry {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsDunningEntryItemsItem>,
}

/// An unpaid invoice that dunning is working on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemsDunningEntryItemsItem {
    /// Amount still due.
    pub amount_due: i64,
    /// Payment attempts so far.
    pub attempt_count: i64,
    /// Invoice created.
    pub created_at: String,
    /// Currency.
    pub currency: String,
    /// Customer id.
    pub customer_id: String,
    /// What happens after the last retry.
    pub final_action: FinalAction,
    /// Invoice id.
    pub invoice_id: String,
    /// Code of the last failed payment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_failure_code: Option<String>,
    /// Message of the last failed payment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_failure_message: Option<String>,
    /// Next automatic attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_attempt_at: Option<String>,
    /// Invoice number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number: Option<String>,
    /// Retries left on the dunning schedule.
    pub retries_left: i64,
    /// Subject.
    pub subject: String,
    /// Subscription id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription_status: Option<BillingSubscriptionStatus>,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsEmailTemplate {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsEmailTemplateItemsItem>,
}

/// An email template.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsEmailTemplateItemsItem {
    /// The realm overrides the built-in template.
    pub customized: bool,
    /// HTML body template.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub html: Option<String>,
    /// Key (`verify_email`, `password_reset`, `magic_link`, `email_otp`, `invitation`, or your own).
    pub key: String,
    /// Realm.
    pub realm: String,
    /// Subject template.
    pub subject: String,
    /// Plain-text body template.
    pub text: String,
    /// Last change (epoch ms; built-ins: none).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
    /// Variables the template uses.
    #[serde(default)]
    pub variables: Vec<String>,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsFeature {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsFeatureItemsItem>,
}

/// A feature of the catalog.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemsFeatureItemsItem {
    /// Created.
    pub created_at: String,
    /// Description.
    pub description: String,
    /// Key (`ai_generation`, `storage`, `api_access`).
    pub key: String,
    /// What it measures.
    pub kind: FeatureKind,
    /// Display name.
    pub name: String,
    /// Permission patterns this feature gates (pipeline step 4). A key
    /// `org:<feature>:<permission>` is gated by feature `<feature>` without
    /// being listed.
    #[serde(default)]
    pub permissions: Vec<String>,
    /// Unit shown with limits (`requests`, `bytes`).
    pub unit: String,
    /// Last change.
    pub updated_at: String,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsInvoice {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsInvoiceItemsItem>,
}

/// An invoice.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemsInvoiceItemsItem {
    /// Left to collect.
    pub amount_due: i64,
    /// Collected.
    pub amount_paid: i64,
    /// Given back by credit notes.
    pub amount_refunded: i64,
    /// Collection attempts.
    pub attempt_count: i64,
    /// Customer credit applied.
    pub balance_applied: i64,
    /// `subscription_create`, `subscription_cycle`, `subscription_update`, `order`.
    pub billing_reason: String,
    /// Coupon code applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coupon: Option<String>,
    /// Created.
    pub created_at: String,
    /// Currency.
    pub currency: String,
    /// Customer id.
    pub customer_id: String,
    /// Discount.
    pub discount: i64,
    /// Finalized.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finalized_at: Option<String>,
    /// What was paid for was delivered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fulfilled_at: Option<String>,
    /// Hosted payment page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hosted_url: Option<String>,
    /// Id.
    pub id: String,
    /// Lines.
    #[serde(default)]
    pub lines: Vec<InvoiceLine>,
    /// Next dunning retry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_attempt_at: Option<String>,
    /// Number (set when finalized).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number: Option<String>,
    /// Order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_id: Option<String>,
    /// Paid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paid_at: Option<String>,
    /// Period end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period_end: Option<String>,
    /// Period start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period_start: Option<String>,
    /// State.
    pub status: InvoiceStatus,
    /// Subject.
    pub subject: String,
    /// Subscription.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription_id: Option<String>,
    /// Sum of the lines before discount (exclusive of added tax).
    pub subtotal: i64,
    /// Tax (added and included).
    pub tax: i64,
    /// Tax region.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tax_region: Option<String>,
    /// subtotal − discount + exclusive tax.
    pub total: i64,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsLot {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsLotItemsItem>,
}

/// What is left of one top-up or grant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemsLotItemsItem {
    /// Credits it started with.
    pub amount: i64,
    /// Created.
    pub created_at: String,
    /// The top-up or grant entry.
    pub entry_id: String,
    /// When it lapses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Id.
    pub id: String,
    /// `top_up` or `grant`.
    pub kind: EntryKind,
    /// Credits left.
    pub remaining: i64,
    /// Subject.
    pub subject: String,
}

/// Object versions.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsObjects {
    /// Entries, newest first.
    #[serde(default)]
    pub items: Vec<ObjectInfo>,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsOrder {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsOrderItemsItem>,
}

/// A one-time order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemsOrderItemsItem {
    /// Created.
    pub created_at: String,
    /// Currency.
    pub currency: String,
    /// Customer id.
    pub customer_id: String,
    /// Id.
    pub id: String,
    /// Invoice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invoice_id: Option<String>,
    /// State.
    pub status: OrderStatus,
    /// Subject.
    pub subject: String,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsOverride {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsOverrideItemsItem>,
}

/// An exception for a subject, a member or a user everywhere.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsOverrideItemsItem {
    /// When.
    pub created_at: String,
    /// Who created it.
    pub created_by: String,
    /// `allow` or `deny` (deny always wins).
    pub effect: String,
    /// Ignored after this time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Feature key.
    pub feature: String,
    /// Id.
    pub id: String,
    /// Replacement limit (allow only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    /// Member principal key (`null`: the whole subject).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period: Option<Period>,
    /// Why.
    pub reason: String,
    /// Subject (`null`: every subject of `member`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// Rolling window length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_secs: Option<i64>,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsPackGrant {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsPackGrantItemsItem>,
}

/// A usage pack a subject holds.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsPackGrantItemsItem {
    /// Granted.
    pub created_at: String,
    /// Expiry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Feature.
    pub feature: String,
    /// Id.
    pub id: String,
    /// Pack key (`null` for ad-hoc grants).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pack: Option<String>,
    /// Units left.
    pub remaining: i64,
    /// Subject.
    pub subject: String,
    /// Units granted.
    pub units: i64,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsPayment {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsPaymentItemsItem>,
}

/// A payment attempt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemsPaymentItemsItem {
    /// Amount.
    pub amount: i64,
    /// Refunded so far.
    pub amount_refunded: i64,
    /// Attempt number of the invoice.
    pub attempt: i64,
    /// Created.
    pub created_at: String,
    /// Currency.
    pub currency: String,
    /// Decline code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_code: Option<String>,
    /// Decline message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_message: Option<String>,
    /// Id.
    pub id: String,
    /// Invoice.
    pub invoice_id: String,
    /// Provider.
    pub provider: String,
    /// Payment id at the provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_ref: Option<String>,
    /// State.
    pub status: PaymentStatus,
    /// Subject.
    pub subject: String,
    /// Changed.
    pub updated_at: String,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsPlan {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsPlanItemsItem>,
}

/// A plan.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsPlanItemsItem {
    /// Can be assigned.
    pub active: bool,
    /// Created.
    pub created_at: String,
    /// Description.
    pub description: String,
    /// Features.
    #[serde(default)]
    pub features: Vec<PlanFeature>,
    /// The plan of subjects without an entitled subscription.
    pub is_default: bool,
    /// Key (`free`, `pro`).
    pub key: String,
    /// Free-form data (prices live in FB-70).
    #[serde(default)]
    pub metadata: serde_json::Value,
    /// Display name.
    pub name: String,
    /// Trial length offered with this plan.
    pub trial_days: i64,
    /// Last change.
    pub updated_at: String,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsPrice {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsPriceItemsItem>,
}

/// A price of a product.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemsPriceItemsItem {
    /// Sellable.
    pub active: bool,
    /// Created.
    pub created_at: String,
    /// Primary currency.
    pub currency: String,
    /// The same price in other currencies.
    #[serde(default)]
    pub currency_options: std::collections::HashMap<String, CurrencyOption>,
    /// Metered: the FB-24 feature whose usage events are billed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feature: Option<String>,
    /// Id.
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval: Option<BillingInterval>,
    /// Intervals per period.
    pub interval_count: i64,
    /// Stable key.
    pub key: String,
    /// Caller data.
    #[serde(default)]
    pub metadata: serde_json::Value,
    /// Metered: all units or only overage.
    pub meter_basis: MeterBasis,
    /// Product key.
    pub product: String,
    /// Product id.
    pub product_id: String,
    /// Quantity to amount.
    pub scheme: Scheme,
    /// Whether the amounts include tax.
    pub tax_behavior: TaxBehavior,
    /// Tiers in the primary currency.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tiers: Option<Vec<Tier>>,
    /// Free trial days when subscribing.
    pub trial_days: i64,
    /// How it is charged.
    pub r#type: PriceType,
    /// Price per unit in the primary currency (per-unit scheme).
    pub unit_amount: i64,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsProduct {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsProductItemsItem>,
}

/// A product.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemsProductItemsItem {
    /// Sellable.
    pub active: bool,
    /// FB-24 add-on (kind `addon`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addon_key: Option<String>,
    /// Created.
    pub created_at: String,
    /// Credits per unit (kind `credits`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credits: Option<i64>,
    /// Description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Id.
    pub id: String,
    /// Stable key.
    pub key: String,
    /// What a paid line delivers.
    pub kind: ProductKind,
    /// Caller data.
    #[serde(default)]
    pub metadata: serde_json::Value,
    /// Display name.
    pub name: String,
    /// FB-24 usage pack (kind `usage_pack`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pack_key: Option<String>,
    /// FB-24 plan (kind `plan`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_key: Option<String>,
    /// Changed.
    pub updated_at: String,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsProviderAccount {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsProviderAccountItemsItem>,
}

/// A configured payment provider (secrets are never returned).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsProviderAccountItemsItem {
    /// API base URL override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_base: Option<String>,
    /// The non-secret provider-specific settings (never `private_key`,
    /// `service_account` or `platform_keys`).
    #[serde(default)]
    pub config: serde_json::Value,
    /// In use.
    pub enabled: bool,
    /// The provider environment: `sandbox`, `xcode` or `local_testing` for the
    /// App Store, `test` for every other provider.
    pub environment: String,
    /// Last characters of the API secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// Public key id (Razorpay).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_id: Option<String>,
    /// `stripe`, `razorpay` or `manual`.
    pub provider: String,
    /// Names of the secrets that are stored (`api_key`, `webhook_secret`,
    /// `config.private_key`, ...). The values are never returned.
    #[serde(default)]
    pub secrets_set: Vec<String>,
    /// Always `true`: only test-mode credentials are accepted (live keys are
    /// refused when saved).
    pub test_mode: bool,
    /// Changed.
    pub updated_at: String,
    /// Webhook URL to register at the provider (relative to the public URL).
    pub webhook_path: String,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsQuotaPolicy {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsQuotaPolicyItemsItem>,
}

/// Enforcement of a feature's limit.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsQuotaPolicyItemsItem {
    /// Units one admitted operation needs (pipeline step 6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<i64>,
    /// Hard or soft.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enforcement: Option<Enforcement>,
    /// Feature key (set from the path).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feature: Option<String>,
    /// Default lease length for concurrency features.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_ttl_secs: Option<i64>,
    /// Soft limits: how far past the limit usage may go, in percent (`null`:
    /// no ceiling).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overage_pct: Option<i64>,
    /// Permission patterns pipeline step 6 admits against this quota.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<Vec<String>>,
    /// Most units that may roll over.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollover_cap: Option<i64>,
    /// Calendar periods: share of the limit that may roll over unused, 0-100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollover_pct: Option<i64>,
    /// Counter scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<QuotaScope>,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsRate {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsRateItemsItem>,
}

/// The price of a metered feature in credits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemsRateItemsItem {
    /// What is charged.
    pub basis: RateBasis,
    /// Created.
    pub created_at: String,
    /// Credits per `per_units` units.
    pub credits_per_unit: i64,
    /// Feature key.
    pub feature: String,
    /// Units the price is for (cost per event: `ceil(units × credits_per_unit / per_units)`).
    pub per_units: i64,
    /// Updated.
    pub updated_at: String,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsRefund {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsRefundItemsItem>,
}

/// A refund.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemsRefundItemsItem {
    /// Amount.
    pub amount: i64,
    /// Created.
    pub created_at: String,
    /// Credit note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credit_note_id: Option<String>,
    /// Currency.
    pub currency: String,
    /// Id.
    pub id: String,
    /// Invoice.
    pub invoice_id: String,
    /// Payment refunded.
    pub payment_id: String,
    /// Provider.
    pub provider: String,
    /// Refund id at the provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_ref: Option<String>,
    /// Why.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// State.
    pub status: RefundStatus,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsResponse {
    #[serde(default)]
    pub items: Vec<serde_json::Value>,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsTaxRate {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsTaxRateItemsItem>,
}

/// A tax rate of a region.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsTaxRateItemsItem {
    /// In use.
    pub active: bool,
    /// Id.
    pub id: String,
    /// Name on invoices (`GST`, `VAT`).
    pub name: String,
    /// Rate in basis points (1800 = 18 %).
    pub rate_bp: i64,
    /// Region (`IN`, `DE`, `US-CA`).
    pub region: String,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsUsageAlert {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsUsageAlertItemsItem>,
}

/// A usage alert.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsUsageAlertItemsItem {
    /// Raised.
    pub at: String,
    /// Delivered by the alert job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_at: Option<String>,
    /// Feature.
    pub feature: String,
    /// Id.
    pub id: String,
    /// Allowance when raised.
    pub limit: i64,
    /// Counter scope key.
    pub scope_key: String,
    /// Subject.
    pub subject: String,
    /// 70, 90 or 100.
    pub threshold: i64,
    /// Usage when raised.
    pub used: i64,
    /// Window.
    pub window_start: String,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsUsagePack {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsUsagePackItemsItem>,
}

/// A usage pack of the catalog.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsUsagePackItemsItem {
    /// Can be granted.
    pub active: bool,
    /// Metered feature it adds units to.
    pub feature: String,
    /// Key.
    pub key: String,
    /// Display name.
    pub name: String,
    /// Units per pack.
    pub units: i64,
    /// Validity after purchase (`null`: no expiry).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_days: Option<i64>,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsUsageSummary {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsUsageSummaryItemsItem>,
}

/// Current usage of one feature of a subject.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemsUsageSummaryItemsItem {
    /// Rolled over from the previous window.
    pub carried_in: i64,
    /// Included.
    pub enabled: bool,
    /// Hard or soft.
    pub enforcement: Enforcement,
    /// Feature.
    pub feature: String,
    /// Kind.
    pub kind: FeatureKind,
    /// Allowance (limit plus rolled-over units; `null`: unlimited).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    /// Usage-pack units left.
    pub pack_remaining: i64,
    /// `used` as a percentage of `limit`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percent: Option<f64>,
    /// Window.
    pub period: Period,
    /// Left including usage packs (`null`: unlimited).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining: Option<i64>,
    /// Held by reservations.
    pub reserved: i64,
    /// Counter scope key.
    pub scope_key: String,
    /// Committed usage (gauge level, active leases).
    pub used: i64,
    /// Window end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_end: Option<String>,
    /// Window start.
    pub window_start: String,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsWebhookEvent {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsWebhookEventItemsItem>,
}

/// A webhook event received.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsWebhookEventItemsItem {
    /// Deliveries seen.
    pub attempts: i64,
    /// Provider time of the event.
    pub event_at: String,
    /// Event id at the provider.
    pub event_id: String,
    /// Event type.
    pub event_type: String,
    /// Id.
    pub id: String,
    /// Provider.
    pub provider: String,
    /// Received.
    pub received_at: String,
    /// What happened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    /// `processed`, `ignored` or `failed`.
    pub status: String,
}

/// A job. The lease token is never part of it (see \[`ClaimedJob`\]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Job {
    /// Claims so far (a lost lease counts).
    pub attempts: i64,
    /// First retry delay in milliseconds; doubles per attempt up to `backoff_max_ms`.
    pub backoff_base_ms: i64,
    /// Cap of the retry delay in milliseconds.
    pub backoff_max_ms: i64,
    /// Cancellation was requested while running; workers should stop and `fail`.
    pub cancel_requested: bool,
    /// When the checkpoint was saved (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_at: Option<String>,
    /// The step this job compensates (saga compensation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compensates: Option<String>,
    /// Creation time (RFC 3339).
    pub created_at: String,
    /// Who enqueued it.
    pub created_by: String,
    /// While running with a timeout: when the attempt times out (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_at: Option<String>,
    /// When it reached a terminal state (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    /// A checkpoint is saved (`GET /api/v1/jobs/{id}/checkpoint`).
    pub has_checkpoint: bool,
    /// Last heartbeat of the current attempt (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heartbeat_at: Option<String>,
    /// Job id.
    pub id: String,
    /// Deduplication key, unique per queue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    /// Handler name.
    pub kind: String,
    /// Message of the last failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Attempts before the job is dead-lettered.
    pub max_attempts: i64,
    /// The job that spawned this one (child jobs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// Handler input.
    #[serde(default)]
    pub payload: serde_json::Value,
    /// Higher runs first.
    pub priority: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<Progress>,
    /// Queue name.
    pub queue: String,
    /// Handler output of a succeeded job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    /// Size of the stored result in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_size: Option<i64>,
    /// The result is kept in object storage (`GET /api/v1/jobs/{id}/result`);
    /// `result` is `null` then.
    pub result_stored: bool,
    /// Earliest start (RFC 3339); moves forward while waiting to retry.
    pub run_at: String,
    /// Schedule that enqueued the job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule_id: Option<String>,
    /// Start of the latest attempt (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    /// State.
    pub status: JobStatus,
    /// Step name inside the workflow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    /// Tenant (project environment) of the enqueuer (FB-142); `null`: none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    /// Longest run of one attempt in ms (`null`: no limit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<i64>,
    /// W3C `traceparent` of the span that enqueued the job (ADR-0035): the
    /// run continues that trace, and an external worker may too. Absent
    /// while trace propagation is off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub traceparent: Option<String>,
    /// Last change (RFC 3339).
    pub updated_at: String,
    /// Time in milliseconds a worker may hold the job without a heartbeat.
    pub visibility_timeout_ms: i64,
    /// While running: when the lease expires without a heartbeat (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility_until: Option<String>,
    /// Parents (dependencies) that have not succeeded yet; the job is not
    /// claimed before this is 0.
    pub waiting_for: i64,
    /// Worker that holds (or held last) the job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_id: Option<String>,
    /// Workflow the job belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_id: Option<String>,
}

/// One entry of a job's history.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct JobEvent {
    /// When (RFC 3339).
    pub at: String,
    /// Attempt number at the time.
    pub attempt: i64,
    /// `enqueued`, `claimed`, `succeeded`, `retry_scheduled`, `dead`,
    /// `cancel_requested`, `cancelled`, `lease_expired`, `released`, `requeued`.
    pub event: String,
    /// Sequence number.
    pub id: i64,
    /// Detail, e.g. the error message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Worker involved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_id: Option<String>,
}

/// A page of jobs, newest first.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct JobPage {
    /// The jobs.
    #[serde(default)]
    pub items: Vec<Job>,
    /// All jobs matching the filter.
    pub total: i64,
}

/// The result of a succeeded job.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct JobResult {
    /// Job id.
    pub job_id: String,
    /// The result (`null` when the handler returned none).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    /// Size in bytes when stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,
    /// It was kept in object storage.
    pub stored: bool,
}

/// Queue statistics.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct JobStats {
    /// Counts per queue.
    #[serde(default)]
    pub queues: Vec<QueueStats>,
    /// Counts over all queues by state.
    #[serde(default)]
    pub totals: std::collections::HashMap<String, i64>,
}

/// Lifecycle state of a job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobStatus {
    #[serde(rename = "queued")]
    Queued,
    #[serde(rename = "running")]
    Running,
    #[serde(rename = "succeeded")]
    Succeeded,
    #[serde(rename = "dead")]
    Dead,
    #[serde(rename = "cancelled")]
    Cancelled,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// A live update of a job, published on the realtime channel `jobs:<job id>`
/// (and on `jobs:<workflow id>` for workflow jobs).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobUpdate {
    /// Claims so far.
    pub attempts: i64,
    /// Cancellation was requested.
    pub cancel_requested: bool,
    /// `progress` (a progress report or checkpoint) or `status` (a state change).
    pub event: String,
    /// End time (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    /// A checkpoint is saved.
    pub has_checkpoint: bool,
    /// Job id.
    pub job_id: String,
    /// Handler name.
    pub kind: String,
    /// Last failure (first 500 characters).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<Progress>,
    /// Queue.
    pub queue: String,
    /// State.
    pub status: JobStatus,
    /// Step name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    /// Last change (RFC 3339).
    pub updated_at: String,
    /// Parents not finished yet.
    pub waiting_for: i64,
    /// Workflow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_id: Option<String>,
}

/// A join token, shown once.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct JoinToken {
    /// The command to run on the other host.
    pub command: String,
    /// RFC 3339 (15 minutes).
    pub expires_at: String,
    /// Token id (in the audit log).
    pub id: String,
    /// Role.
    pub role: String,
    /// Set id.
    pub set: String,
    /// The token (`fbj_…`); only its hash is stored.
    pub token: String,
}

/// Body of `POST /api/system/database/join-tokens`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct JoinTokenBody {
    /// `replica` (default). Shard primaries on other hosts come with FB-174.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Set id (default `control`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set: Option<String>,
}

/// Master key handling recorded in a backup.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct KeyInfo {
    /// First 16 hex characters of the key file's SHA-256 (`None` when the
    /// data directory had no key). Restore refuses a different key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    /// Whether `auth/master.key` is part of the backup.
    pub included: bool,
}

/// Answer of `GET .../keys/{key}`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct KeyValue {
    /// The key.
    pub key: String,
    /// Length: bytes, fields, items, members or entries.
    pub size: i64,
    /// Whether `value` was cut to `limit` items.
    pub truncated: bool,
    /// Milliseconds until expiry; absent when the key does not expire.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_ms: Option<i64>,
    /// `string`, `hash`, `list`, `set`, `zset` or `stream`.
    pub r#type: String,
    /// The value: string; `{field: value}`; `[items]`; `[{member, score}]`; `[{id, fields}]`.
    #[serde(default)]
    pub value: serde_json::Value,
}

/// What a key-value namespace rule allows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum KvAccess {
    #[serde(rename = "read")]
    Read,
    #[serde(rename = "write")]
    Write,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// Lets a JavaScript function reach an application key-value namespace
/// (beyond its private `fn.<name>`). Deny by default; administrators manage
/// the rules; the caller's own key-value permissions still apply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KvRule {
    /// What is allowed.
    pub access: KvAccess,
    /// Namespace name, or a prefix ending in `*` (`app.*`, `*` for every namespace).
    pub namespace: String,
}

/// Key-value namespace rules of a function.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct KvRuleList {
    /// Rules (deny by default).
    #[serde(default)]
    pub items: Vec<KvRule>,
}

/// Statistics: engine-wide for a privileged caller without a tenant, else
/// those of the caller's tenant (FB-138).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct KvStats {
    /// Live pub/sub channels.
    pub channels: i64,
    /// Keys removed to stay within the memory budget.
    pub evicted_keys: i64,
    /// Eviction policy: `noeviction`, `allkeys-lru` or `volatile-lru`.
    pub eviction_policy: String,
    /// Keys removed because their TTL passed.
    pub expired_keys: i64,
    /// Reads that found a key.
    pub hits: i64,
    /// Keys (expired keys not yet swept included).
    pub keys: i64,
    /// Memory budget in bytes (`0` = unlimited): the engine's, or the
    /// per-tenant budget `kv.tenant_max_bytes` for `scope = tenant`.
    pub max_memory_bytes: i64,
    /// Reads that found nothing.
    pub misses: i64,
    /// Whether changes are written to the database.
    pub persistent: bool,
    /// `engine` (every tenant) or `tenant` (the caller's tenant only).
    pub scope: String,
    /// The tenant the figures are for (`scope = tenant`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    /// Keys changed in memory and not yet written to the database.
    pub unsaved_keys: i64,
    /// Estimated memory in use, bytes.
    pub used_memory_bytes: i64,
}

/// One lag sample.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct LagSample {
    /// RFC 3339 minute.
    pub at: String,
    /// Highest byte lag in that minute.
    pub lag_bytes: i64,
    /// Highest time lag in that minute.
    pub lag_ms: i64,
}

/// Language a version was written in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Language {
    #[serde(rename = "wasm")]
    Wasm,
    #[serde(rename = "javascript")]
    Javascript,
    #[serde(rename = "typescript")]
    Typescript,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// A concurrency lease.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Lease {
    /// Active until (renew to extend).
    pub expires_at: String,
    /// Feature.
    pub feature: String,
    /// Id.
    pub id: String,
    /// Released.
    pub released: bool,
    /// `true` when returned for a replayed idempotency key.
    pub replayed: bool,
    /// Counter scope key.
    pub scope_key: String,
    /// Subject.
    pub subject: String,
}

/// Body of a heartbeat.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct LeaseBody {
    /// The lease from the claim.
    pub lease_id: String,
}

/// One immutable ledger row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LedgerEntry {
    /// Who recorded it.
    pub actor: String,
    /// Credits added (positive) or removed (negative).
    pub amount: i64,
    /// When.
    pub at: String,
    /// Balance right after the entry.
    pub balance_after: i64,
    /// Top-up and grant: when the credits lapse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Order, payment or invoice reference (FB-70).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_ref: Option<String>,
    /// Consumption and spend (and their refunds): the feature paid for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feature: Option<String>,
    /// Id.
    pub id: String,
    /// Idempotency key (unique per subject).
    pub idempotency_key: String,
    /// What it records.
    pub kind: EntryKind,
    /// Top-up and grant: the lot created; reversal and expiry: the lot taken from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lot_id: Option<String>,
    /// Caller data.
    #[serde(default)]
    pub metadata: serde_json::Value,
    /// Why.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Refund: the debit given back; reversal: the top-up or grant taken back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_entry: Option<String>,
    /// Request id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// Cursor (increasing in commit order).
    pub seq: i64,
    /// Subject (`org:<id>` or a principal key).
    pub subject: String,
    /// Units paid for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub units: Option<i64>,
    /// Consumption: the FB-24 usage event charged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_event_id: Option<String>,
    /// Consumption: the usage event's `seq`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_seq: Option<i64>,
}

/// A page of ledger entries.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct LedgerPage {
    /// Oldest first.
    #[serde(default)]
    pub items: Vec<LedgerEntry>,
    /// Pass as `after` for the next page (`null`: no more).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<i64>,
}

/// The level usage is grouped at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Level {
    #[serde(rename = "environment")]
    Environment,
    #[serde(rename = "project")]
    Project,
    #[serde(rename = "org")]
    Org,
    #[serde(rename = "realm")]
    Realm,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// Resource limits of one execution.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Limits {
    /// Fuel (roughly: WebAssembly instructions, plus a charge per host call).
    pub fuel: i64,
    /// Executions of this function at the same time; more get `429 function_busy`.
    pub max_concurrency: i64,
    /// Largest linear memory in bytes.
    pub memory_bytes: i64,
    /// Wall-clock limit in milliseconds (including host calls such as fetch).
    pub timeout_ms: i64,
}

/// Limits in a deploy request: every field optional (defaults apply).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct LimitsInput {
    /// Fuel per execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fuel: Option<i64>,
    /// Concurrent executions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrency: Option<i64>,
    /// Memory limit in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_bytes: Option<i64>,
    /// Wall-clock limit in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<i64>,
}

/// Invoice line type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LineKind {
    #[serde(rename = "subscription")]
    Subscription,
    #[serde(rename = "proration")]
    Proration,
    #[serde(rename = "usage")]
    Usage,
    #[serde(rename = "one_time")]
    OneTime,
    #[serde(rename = "adjustment")]
    Adjustment,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// Internal objects a delivery touched.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Links {
    /// Billing customer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub customer_id: Option<String>,
    /// Invoice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invoice_id: Option<String>,
    /// One-time order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_id: Option<String>,
    /// Payment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payment_id: Option<String>,
    /// Refund.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refund_id: Option<String>,
    /// Store purchase (App Store, Google Play, Creem).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_purchase_id: Option<String>,
    /// Subscription.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription_id: Option<String>,
    /// Row of the event dedup table (`webhook-events`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_event_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ListQueryBody {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expand: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fields: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    /// Fuzzy text: trigram similarity, best match first (pg_trgm where it
    /// is in use, else the portable engine; ADR-0034).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fuzzy: Option<String>,
    /// Fields `fuzzy` compares; each needs a trigram index (default: all
    /// fields of the collection's trigram indexes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fuzzy_fields: Option<Vec<String>>,
    /// Minimum similarity from 0 to 1 (default 0.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fuzzy_threshold: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<i64>,
    /// Values of `{:name}` parameters, typed.
    #[serde(default)]
    pub params: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_page: Option<i64>,
    /// Full-text query (needs a full-text index).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_total: Option<bool>,
    /// e.g. `-created,name`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort: Option<String>,
}

/// Body of `POST .../lists/{key}/remove` (`LREM`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListRemoveBody {
    /// `0` (default) removes every match; `n > 0` the first `n` from the head; `n < 0` the first `|n|` from the tail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<i64>,
    /// Value to remove.
    pub value: Bin,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ListResponse {
    #[serde(default)]
    pub items: Vec<serde_json::Value>,
    /// Pass as `cursor` to get the next page; absent on the last page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<i64>,
    pub per_page: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_items: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_pages: Option<i64>,
}

/// Body of `POST .../lists/{key}/set` (`LSET`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListSetBody {
    /// Index (negative counts from the tail).
    pub index: i64,
    /// New value.
    pub value: Bin,
}

/// A line the function logged.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct LogLine {
    /// `debug`, `info`, `warn` or `error`.
    pub level: String,
    /// Text.
    pub message: String,
}

/// A login with its password, returned once by create and rotate.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct LoginSecret {
    /// `psql` connection string over the Unix socket, without the password.
    pub hint: String,
    /// The login.
    pub login: DbLogin,
    /// The generated password: shown once, never stored by ForgeBase.
    pub password: String,
}

/// A login to create (`forgebase db login create`, `POST
/// /api/v1/system/db-logins`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct LoginSpec {
    /// The SQL editor's rights (`forgebase_sql_admin`, bypasses row-level
    /// security); platform administrators only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admin: Option<bool>,
    /// `CONNECTION LIMIT` (1 to 100, default 10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection_limit: Option<i64>,
    /// `[a-z][a-z0-9_]{0,39}`; the role is `fbu_<name>`.
    pub name: String,
    /// Principal key the login acts as: `user:<collection>/<id>` or
    /// `service:<id>`. Optional for `admin` logins (default: the creator).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
    /// Reads only (member of `forgebase_login`, not `forgebase_login_write`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readonly: Option<bool>,
    /// `statement_timeout` in milliseconds (1 to 3 600 000, default 30 000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub statement_timeout_ms: Option<i64>,
    /// The one tenant the login's statements are scoped to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    /// Expiry (`VALID UNTIL`), RFC 3339 such as `2027-01-31T00:00:00Z`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_until: Option<String>,
}

/// What is left of one top-up or grant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Lot {
    /// Credits it started with.
    pub amount: i64,
    /// Created.
    pub created_at: String,
    /// The top-up or grant entry.
    pub entry_id: String,
    /// When it lapses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Id.
    pub id: String,
    /// `top_up` or `grant`.
    pub kind: EntryKind,
    /// Credits left.
    pub remaining: i64,
    /// Subject.
    pub subject: String,
}

/// The manifest.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Manifest {
    /// RFC 3339 creation time (UTC).
    pub created: String,
    /// Only the database is in this backup (pre-migration snapshots): a
    /// restore leaves storage, indexes and the master key alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database_only: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<EngineInfo>,
    /// Every file with size and hash.
    #[serde(default)]
    pub files: Vec<FileEntry>,
    /// ForgeBase version that wrote it.
    pub forgebase_version: String,
    /// Manifest format version.
    pub format: i64,
    /// Backup id (the directory name for snapshots).
    pub id: String,
    /// Whether derived index files are included.
    pub includes_indexes: bool,
    /// `backup` (explicit destination) or `snapshot` (managed, with retention).
    pub kind: String,
    /// Free-form label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Master key handling.
    pub master_key: KeyInfo,
    /// Why this backup was taken, for managed ones (`pre-migration`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Highest applied system migration in the copied database.
    pub schema_version: i64,
}

/// Body of `POST /billing/invoices/{id}/mark-paid` (manual billing).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct MarkPaidInput {
    /// Bank transfer or cheque reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
}

/// A member of a channel's presence.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Member {
    /// Who: `user:<collection>/<id>`, `admin:<id>` or `anonymous`.
    pub actor: String,
    /// Connection id (unique per instance).
    pub connection_id: String,
    /// Instance the connection is on.
    pub instance: String,
    /// When the member joined (UTC).
    pub joined_at: String,
    /// Public metadata set by the member.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<ValueModel>,
}

/// Body of set add/remove and sorted-set remove.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct MembersBody {
    /// Members.
    #[serde(default)]
    pub members: Vec<Bin>,
}

/// A stored message.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Message {
    /// Attributes.
    #[serde(default)]
    pub attributes: std::collections::HashMap<String, String>,
    /// Size counted against limits.
    pub bytes: i64,
    /// Data.
    #[serde(default)]
    pub data: serde_json::Value,
    /// Dedup id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedup_id: Option<String>,
    /// For a message on a dead-letter topic: its deliveries on the source subscription.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_history: Option<Vec<DeliveryAttempt>>,
    /// Message id.
    pub id: String,
    /// Ordering key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ordering_key: Option<String>,
    /// Published (RFC 3339).
    pub published_at: String,
    /// Publisher (actor).
    pub publisher: String,
    /// Sequence number (strictly increasing across topics).
    pub seq: i64,
    /// Topic.
    pub topic: String,
}

/// Retained messages of a topic.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct MessagePage {
    /// Oldest first.
    #[serde(default)]
    pub items: Vec<Message>,
    /// Pass as `after` for the next page; absent at the end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_after: Option<i64>,
}

/// Which usage units a metered price bills.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MeterBasis {
    #[serde(rename = "units")]
    Units,
    #[serde(rename = "overage")]
    Overage,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// Where a meter stands against its limit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MeterState {
    #[serde(rename = "ok")]
    Ok,
    #[serde(rename = "warn")]
    Warn,
    #[serde(rename = "critical")]
    Critical,
    #[serde(rename = "over")]
    Over,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// How the database was copied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Method {
    #[serde(rename = "vacuum")]
    Vacuum,
    #[serde(rename = "basebackup")]
    Basebackup,
    #[serde(rename = "dump")]
    Dump,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// Distance function of a vector field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Metric {
    #[serde(rename = "cosine")]
    Cosine,
    #[serde(rename = "l2")]
    L2,
    #[serde(rename = "dot")]
    Dot,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// What a built-in metric is.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct MetricInfo {
    /// What it counts.
    pub description: String,
    /// Key, as used in `metric` and `sort`.
    pub key: String,
    /// `counter` (work done: summed over the period) or `gauge` (a resource
    /// held: last value of the period and its peak).
    pub kind: String,
    /// Where it comes from.
    pub source: String,
    /// Unit.
    pub unit: String,
}

/// A list of metrics.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct MetricList {
    /// The built-in metrics.
    #[serde(default)]
    pub items: Vec<MetricInfo>,
}

/// One metric of a tenant over the period.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct MetricValue {
    /// `counter` or `gauge`.
    pub kind: String,
    /// Gauges: the largest daily value of the period.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak: Option<i64>,
    /// Counters: the sum over the period. Gauges: the value of the last day
    /// with data in the period.
    pub value: i64,
}

/// Body of `POST /mget`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct MgetBody {
    /// Keys to read.
    #[serde(default)]
    pub keys: Vec<String>,
}

/// Body of a deadline change.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ModifyDeadlineRequest {
    /// New deadline from now, 0 to 10 min; `0` nacks.
    pub ack_deadline_ms: i64,
    /// Ack ids from deliveries.
    #[serde(default)]
    pub ack_ids: Vec<String>,
}

/// An amount in minor units of an ISO 4217 currency.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Money {
    /// Minor units (cents for USD, paise for INR, yen for JPY).
    pub amount: i64,
    /// ISO 4217 code, upper case.
    pub currency: String,
}

/// Body of `POST /api/system/database/placements/{tenant}/move`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct MoveBody {
    /// The tenant id, typed by the operator.
    pub confirm: String,
    /// Target set id.
    pub to: String,
}

/// Body of `POST /mset`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct MsetBody {
    /// Entries to write together.
    #[serde(default)]
    pub entries: Vec<MsetEntry>,
}

/// One entry of `POST /mset`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MsetEntry {
    /// Key.
    pub key: String,
    /// Value.
    pub value: Bin,
}

/// One namespace of the caller's tenant.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NamespaceInfo {
    /// Keys in it.
    pub keys: i64,
    /// Namespace name.
    pub name: String,
}

/// A neighbour of a node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Neighbor {
    /// `out` when the queried node is the edge's source, `in` when it is the
    /// target (also for undirected edges).
    pub direction: Direction,
    /// The connecting edge.
    pub edge: Edge,
    /// The node at the other end.
    pub node: Node,
}

/// A page of neighbours.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NeighborPage {
    #[serde(default)]
    pub items: Vec<Neighbor>,
    /// Pass as `offset` for the next page; absent on the last page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_after: Option<String>,
}

/// Body of `POST /edges`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NewEdge {
    /// `false` for an undirected relationship (default `true`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directed: Option<bool>,
    /// Id; generated when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Property object (default `{}`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub properties: Option<serde_json::Value>,
    /// Source node id; the node must exist.
    pub source: String,
    /// Target node id; the node must exist.
    pub target: String,
    /// Relationship type, 1 to 64 of `A-Z a-z 0-9 _ . : -`.
    pub r#type: String,
}

/// A job to enqueue.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NewJob {
    /// First retry delay in ms (default 1000); doubles per attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backoff_base_ms: Option<i64>,
    /// Cap of the retry delay in ms (default 1 hour).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backoff_max_ms: Option<i64>,
    /// Earliest start as a delay from now in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delay_ms: Option<i64>,
    /// Ids of jobs that must succeed first; the job waits for all of them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depends_on: Option<Vec<String>>,
    /// Enqueueing the same key twice in a queue returns the first job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    /// Handler name.
    pub kind: String,
    /// Attempts before dead-lettering (default 5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_attempts: Option<i64>,
    /// Handler input (default `null`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    /// Higher runs first (default 0, range -1000..=1000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i64>,
    /// Queue (default `default`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue: Option<String>,
    /// Earliest start, RFC 3339. Mutually exclusive with `delay_ms`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_at: Option<String>,
    /// Longest run of one attempt in ms (1 s to 30 days; default: the queue's,
    /// else none). A timed-out attempt is retried like a failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<i64>,
    /// Time in ms a worker may hold the job without a heartbeat (default 60000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility_timeout_ms: Option<i64>,
}

/// One message to publish.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NewMessage {
    /// String attributes (subscription filters match on them).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attributes: Option<std::collections::HashMap<String, String>>,
    /// Any JSON value.
    #[serde(default)]
    pub data: serde_json::Value,
    /// A repeat inside the topic's dedup window is accepted once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedup_id: Option<String>,
    /// Messages with the same key reach an ordered subscription in publish order, one at a time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ordering_key: Option<String>,
}

/// Body of `POST /nodes`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NewNode {
    /// Id; generated when absent. 1 to 128 of `A-Z a-z 0-9 _ . : @ -`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Kind, up to 64 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Property object (default `{}`), up to 64 KiB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub properties: Option<serde_json::Value>,
}

/// A schedule to create.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NewSchedule {
    /// First retry delay in ms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backoff_base_ms: Option<i64>,
    /// Cap of the retry delay in ms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backoff_max_ms: Option<i64>,
    /// Cron expression (UTC; 5 fields, or 6 with leading seconds).
    pub cron: String,
    /// Start enabled (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Handler name.
    pub kind: String,
    /// Attempts per job (default 5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_attempts: Option<i64>,
    /// Unique name.
    pub name: String,
    /// Handler input (default `null`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    /// Priority (default 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i64>,
    /// Queue (default `default`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue: Option<String>,
    /// Visibility timeout in ms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility_timeout_ms: Option<i64>,
}

/// Body of a snapshot creation.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NewSnapshot {
    /// 1-128 letters, digits, `.`, `_`, `:` or `-`.
    pub name: String,
    /// Subscription whose acknowledgement state is saved.
    pub subscription: String,
}

/// One step of a workflow to create.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NewStep {
    /// First retry delay in ms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backoff_base_ms: Option<i64>,
    /// Cap of the retry delay in ms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backoff_max_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compensate: Option<Compensation>,
    /// Names of steps that must succeed first (fan-in: all of them).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depends_on: Option<Vec<String>>,
    /// Handler name.
    pub kind: String,
    /// Attempts before the step is dead-lettered (default 5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_attempts: Option<i64>,
    /// Unique name inside the workflow.
    pub name: String,
    /// Handler input (default `null`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    /// Higher runs first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i64>,
    /// Queue (default `default`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue: Option<String>,
    /// Longest run of one attempt, in ms (1 s to 30 days).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<i64>,
    /// Lease length without a heartbeat, in ms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility_timeout_ms: Option<i64>,
}

/// Body of a subscription creation.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NewSubscription {
    /// Visibility timeout of a delivery, 1 s to 10 min (default 10 s).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ack_deadline_ms: Option<i64>,
    /// Redelivery backoff after a nack or a missed deadline: base (default 1 s).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backoff_base_ms: Option<i64>,
    /// Backoff cap (default 10 min).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backoff_max_ms: Option<i64>,
    /// Topic that receives dead-lettered messages with their delivery history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dead_letter_topic: Option<String>,
    /// Attribute filter (see the ADR); immutable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<serde_json::Value>,
    /// Free-form labels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labels: Option<std::collections::HashMap<String, String>>,
    /// After this many deliveries a message is dead-lettered (1-100; default: unlimited).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_delivery_attempts: Option<i64>,
    /// 1-128 letters, digits, `.`, `_`, `:` or `-`.
    pub name: String,
    /// Deliver messages with the same ordering key in publish order, one at a time; immutable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ordering: Option<bool>,
    /// Push: pause delivery after this many consecutive failed attempts
    /// (1-1000; default: never). Paused subscriptions keep their messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_after_failures: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push: Option<PushConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<StartPosition>,
    /// Topic to subscribe to.
    pub topic: String,
    /// Delete the subscription this long after creation (60 s to 31 days;
    /// default: never). A later change with `ttl_ms` extends it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_ms: Option<i64>,
}

/// Body of a topic creation.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NewTopic {
    /// A repeated `dedup_id` inside this window is accepted once (default 10 minutes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedup_window_ms: Option<i64>,
    /// What the topic's events are (event catalog; at most 1024 characters).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// An example of message `data` (event catalog); must satisfy the schema when one is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub example: Option<serde_json::Value>,
    /// Free-form labels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labels: Option<std::collections::HashMap<String, String>>,
    /// Largest message (data, attributes and ordering key), default and upper bound from the server config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_message_bytes: Option<i64>,
    /// 1-128 letters, digits, `.`, `_`, `:` or `-`, starting with a letter or digit.
    pub name: String,
    /// Most bytes kept; the oldest messages are dropped beyond it (default: no limit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention_bytes: Option<i64>,
    /// How long messages are kept, acknowledged or not (default 7 days).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention_ms: Option<i64>,
    /// JSON Schema every message's `data` must satisfy (subset, see the ADR).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<RecordSource>,
}

/// A worker to register.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NewWorker {
    /// Jobs it runs at once (default 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concurrency: Option<i64>,
    /// Id to use; generated when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Handlers it runs (empty: all).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kinds: Option<Vec<String>>,
    /// Display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Queues it works (empty: all).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queues: Option<Vec<String>>,
}

/// A workflow to create: a graph of steps. A chain is steps that each depend
/// on the one before; fan-out is several steps depending on one; fan-in is
/// one step depending on several (it waits for all).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NewWorkflow {
    /// Display name.
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_failure: Option<FailurePolicy>,
    /// The steps (1 to 500).
    #[serde(default)]
    pub steps: Vec<NewStep>,
}

/// A graph node.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Node {
    /// RFC 3339 creation time.
    pub created_at: String,
    /// Node id (unique per tenant).
    pub id: String,
    /// Optional kind, e.g. `User` or `Product`.
    pub label: String,
    /// JSON object of properties.
    #[serde(default)]
    pub properties: serde_json::Value,
    /// RFC 3339 time of the last change.
    pub updated_at: String,
}

/// Body of `PUT` (replace) and `PATCH` (merge) on a node.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NodeChange {
    /// New label (unchanged when absent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// `PUT`: replaces the properties. `PATCH`: JSON merge patch (RFC 7396;
    /// `null` removes a key).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub properties: Option<serde_json::Value>,
}

/// Result of deleting a node.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NodeDeleted {
    /// Edges that touched the node and were deleted with it.
    pub deleted_edges: i64,
    /// Deleted node id.
    pub id: String,
}

/// `GET /api/system/database/nodes/{id}`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NodeDetail {
    /// Last hour, one-minute buckets, oldest first (kept in memory: empty
    /// after a restart).
    #[serde(default)]
    pub lag_history: Vec<LagSample>,
    /// The node.
    pub node: NodeInfo,
}

/// Node filter: label and property equality (all must match).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NodeFilter {
    /// Only nodes with this label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Property equality, e.g. `{"country": "IN"}`. Values are strings, numbers
    /// or booleans; keys are `A-Z a-z 0-9 _ -`. At most 8.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub properties: Option<serde_json::Value>,
}

/// One server of a set.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NodeInfo {
    /// The `forgebase node` agent of a remote node: `state`, `address`,
    /// `replay_lsn`, `disk_free`, `last_seen_at`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<serde_json::Value>,
    /// RFC 3339.
    pub created_at: String,
    /// PostgreSQL version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_version: Option<String>,
    /// Id.
    pub id: String,
    /// Serves reads now (replicas).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_rotation: Option<bool>,
    /// `local` (this server), `remote` (`forgebase node`) or `external`.
    pub kind: String,
    /// Bytes of changes behind (replicas).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lag_bytes: Option<i64>,
    /// Time behind the primary at the last sample (replicas).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lag_ms: Option<i64>,
    /// The latest failure while out of rotation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Name (`postgres/nodes/<name>` for local nodes).
    pub name: String,
    /// `api`, or `config` (declared in `forgebase.toml`: read-only).
    pub origin: String,
    /// Last replayed position (`X/Y`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay_lsn: Option<String>,
    /// `primary` or `replica`.
    pub role: String,
    /// Set id.
    pub set: String,
    /// `provisioning`, `ready`, `catching_up`, `streaming`, `lagging`,
    /// `unreachable`, `needs_rebuild` or `promoting`.
    pub state: String,
    /// Cluster system identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_identifier: Option<String>,
    /// Timeline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeline: Option<i64>,
    /// RFC 3339.
    pub updated_at: String,
    /// Share of replica reads.
    pub weight: i64,
}

/// A page of nodes.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NodePage {
    #[serde(default)]
    pub items: Vec<Node>,
    /// Pass as `after` for the next page; absent on the last page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_after: Option<String>,
}

/// An object version.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ObjectInfo {
    /// Bucket.
    pub bucket: String,
    /// SHA-256 of the content (hex); also the ETag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checksum_sha256: Option<String>,
    /// Content type.
    pub content_type: String,
    /// RFC 3339 upload time.
    pub created_at: String,
    /// Principal key of the uploader.
    pub created_by: String,
    /// Whether this version is a delete marker.
    pub delete_marker: bool,
    /// Whether this is the current version.
    pub is_latest: bool,
    /// Key.
    pub key: String,
    /// User metadata.
    #[serde(default)]
    pub metadata: serde_json::Value,
    /// Size in bytes.
    pub size: i64,
    /// Version id.
    pub version_id: String,
}

/// A page of a listing.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ObjectPage {
    /// Pass as `cursor` for the next page; `null` at the end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// Objects (current versions only), in key order.
    #[serde(default)]
    pub objects: Vec<ObjectInfo>,
    /// "Directories" when a delimiter was given.
    #[serde(default)]
    pub prefixes: Vec<String>,
}

/// A topology operation (a job of a reserved `database.*` kind).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Operation {
    /// Cancel was asked for.
    pub cancel_requested: bool,
    /// It can still be cancelled.
    pub cancellable: bool,
    /// RFC 3339.
    pub created_at: String,
    /// Who started it.
    pub created_by: String,
    /// RFC 3339.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    /// Job id; progress streams on channel `jobs:<id>`.
    pub id: String,
    /// `database.node.add`, `database.promote`, …
    pub kind: String,
    /// Why it failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Node it changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<OperationProgress>,
    /// Result of a finished operation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    /// Set it changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set: Option<String>,
    /// RFC 3339.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    /// `pending`, `running`, `succeeded`, `failed`, `cancelled`, `dead`.
    pub status: String,
}

/// A page of operations.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct OperationList {
    /// Newest first.
    #[serde(default)]
    pub items: Vec<Operation>,
}

/// Progress of an operation.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct OperationProgress {
    /// Detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// 0 to 100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percent: Option<f64>,
    /// Stage (`cloning`, `starting`, `fencing`, `promoting`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
}

/// A one-time order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Order {
    /// Created.
    pub created_at: String,
    /// Currency.
    pub currency: String,
    /// Customer id.
    pub customer_id: String,
    /// Id.
    pub id: String,
    /// Invoice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invoice_id: Option<String>,
    /// State.
    pub status: OrderStatus,
    /// Subject.
    pub subject: String,
}

/// Body of `POST /billing/orders`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct OrderInput {
    /// Coupon code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coupon: Option<String>,
    /// A replay with the same key returns the first order.
    pub idempotency_key: String,
    /// One-time prices.
    #[serde(default)]
    pub items: Vec<ItemInput>,
    /// Where the hosted payment page returns to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub return_url: Option<String>,
    /// Subject (default: the caller's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
}

/// A page of orders.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct OrderPage {
    /// Orders, newest first.
    #[serde(default)]
    pub items: Vec<Order>,
    /// Page size.
    pub limit: i64,
    /// Rows skipped.
    pub offset: i64,
    /// All orders matching the filters.
    pub total: i64,
}

/// Order lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderStatus {
    #[serde(rename = "open")]
    Open,
    #[serde(rename = "paid")]
    Paid,
    #[serde(rename = "canceled")]
    Canceled,
    #[serde(rename = "refunded")]
    Refunded,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// An exception for a subject, a member or a user everywhere.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Override {
    /// When.
    pub created_at: String,
    /// Who created it.
    pub created_by: String,
    /// `allow` or `deny` (deny always wins).
    pub effect: String,
    /// Ignored after this time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Feature key.
    pub feature: String,
    /// Id.
    pub id: String,
    /// Replacement limit (allow only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    /// Member principal key (`null`: the whole subject).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period: Option<Period>,
    /// Why.
    pub reason: String,
    /// Subject (`null`: every subject of `member`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// Rolling window length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_secs: Option<i64>,
}

/// Body of `POST /entitlements/overrides`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct OverrideInput {
    /// `allow` or `deny`.
    pub effect: String,
    /// Expiry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Feature key.
    pub feature: String,
    /// Replacement limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    /// Member principal key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period: Option<Period>,
    /// Why.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Subject.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// Rolling window length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_secs: Option<i64>,
}

/// A usage pack a subject holds.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PackGrant {
    /// Granted.
    pub created_at: String,
    /// Expiry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Feature.
    pub feature: String,
    /// Id.
    pub id: String,
    /// Pack key (`null` for ad-hoc grants).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pack: Option<String>,
    /// Units left.
    pub remaining: i64,
    /// Subject.
    pub subject: String,
    /// Units granted.
    pub units: i64,
}

/// Body of `POST /entitlements/usage-packs`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PackGrantInput {
    /// A replay with the same key returns the first grant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    /// Pack key.
    pub pack: String,
    /// Number of packs (default 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantity: Option<i64>,
    /// Subject.
    pub subject: String,
}

/// A parent (dependency) of a claimed job, with its result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParentResult {
    /// Parent job id.
    pub id: String,
    /// Its result when stored inline; `null` when it lives in object storage
    /// (`result_stored`) — fetch it with `GET /api/v1/jobs/{id}/result`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    /// The result is in object storage.
    pub result_stored: bool,
    /// Its state (`succeeded` for dependencies; a compensated step may differ).
    pub status: JobStatus,
    /// Step name inside its workflow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
}

/// One received part.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PartInfo {
    /// SHA-256 of the part (hex).
    pub checksum_sha256: String,
    /// Part number (1-10000).
    pub part_number: i64,
    /// Size in bytes.
    pub size: i64,
}

/// Body of `POST /shortest-path`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PathQuery {
    /// Direction to follow (default `out`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<Direction>,
    /// Edge types to follow; empty or absent = all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge_types: Option<Vec<String>>,
    /// Id of the first node.
    pub from: String,
    /// Longest path in hops, 1 to 10 (default 6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<i64>,
    /// Id of the last node.
    pub to: String,
}

/// Result of a shortest-path search (unweighted: fewest hops).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PathResult {
    /// Edges between consecutive nodes.
    #[serde(default)]
    pub edges: Vec<Edge>,
    /// Whether a path within `max_depth` exists.
    pub found: bool,
    /// Number of edges on the path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length: Option<i64>,
    /// Nodes from `from` to `to`.
    #[serde(default)]
    pub nodes: Vec<Node>,
}

/// Body of a pause (optional).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PauseRequest {
    /// Why (shown as `paused_reason`; at most 500 characters).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Body of `POST /billing/invoices/{id}/pay`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PayInput {
    /// Where the hosted payment page returns to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub return_url: Option<String>,
}

/// A payment attempt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Payment {
    /// Amount.
    pub amount: i64,
    /// Refunded so far.
    pub amount_refunded: i64,
    /// Attempt number of the invoice.
    pub attempt: i64,
    /// Created.
    pub created_at: String,
    /// Currency.
    pub currency: String,
    /// Decline code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_code: Option<String>,
    /// Decline message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_message: Option<String>,
    /// Id.
    pub id: String,
    /// Invoice.
    pub invoice_id: String,
    /// Provider.
    pub provider: String,
    /// Payment id at the provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_ref: Option<String>,
    /// State.
    pub status: PaymentStatus,
    /// Subject.
    pub subject: String,
    /// Changed.
    pub updated_at: String,
}

/// Body of `POST /billing/portal/payment-method`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PaymentMethodInput {
    /// Where the provider page returns to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub return_url: Option<String>,
    /// Subject (default: the caller's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
}

/// One collection attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PaymentStatus {
    #[serde(rename = "pending")]
    Pending,
    #[serde(rename = "succeeded")]
    Succeeded,
    #[serde(rename = "failed")]
    Failed,
    #[serde(rename = "canceled")]
    Canceled,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// What a caller still has to accept.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Pending {
    /// Current versions not yet accepted.
    #[serde(default)]
    pub pending: Vec<AgreementRef>,
}

/// How usage is counted over time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Period {
    #[serde(rename = "none")]
    None,
    #[serde(rename = "hour")]
    Hour,
    #[serde(rename = "day")]
    Day,
    #[serde(rename = "month")]
    Month,
    #[serde(rename = "lifetime")]
    Lifetime,
    #[serde(rename = "rolling")]
    Rolling,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// A permission level on a bucket or key prefix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Permission {
    #[serde(rename = "read")]
    Read,
    #[serde(rename = "write")]
    Write,
    #[serde(rename = "admin")]
    Admin,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// One tenant's placement.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PlacementInfo {
    /// Estimated bytes.
    pub est_bytes: i64,
    /// Estimated rows.
    pub est_rows: i64,
    /// RFC 3339 of the last move.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub moved_at: Option<String>,
    /// Its organization, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization: Option<String>,
    /// Its project, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// Set id.
    pub set: String,
    /// `active`, `moving`, `frozen` or `cleanup`.
    pub state: String,
    /// Tenant (environment id).
    pub tenant: String,
}

/// A page of placements.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PlacementPage {
    /// Tenants by id.
    #[serde(default)]
    pub items: Vec<PlacementInfo>,
    /// Cursor of the next page (`None`: last page).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// A plan.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Plan {
    /// Can be assigned.
    pub active: bool,
    /// Created.
    pub created_at: String,
    /// Description.
    pub description: String,
    /// Features.
    #[serde(default)]
    pub features: Vec<PlanFeature>,
    /// The plan of subjects without an entitled subscription.
    pub is_default: bool,
    /// Key (`free`, `pro`).
    pub key: String,
    /// Free-form data (prices live in FB-70).
    #[serde(default)]
    pub metadata: serde_json::Value,
    /// Display name.
    pub name: String,
    /// Trial length offered with this plan.
    pub trial_days: i64,
    /// Last change.
    pub updated_at: String,
}

/// A feature in a plan, an add-on, a custom limit or an override.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PlanFeature {
    /// Included (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Feature key.
    pub feature: String,
    /// Limit per period; `null` = unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period: Option<Period>,
    /// Allowance while the subscription is trialing (replaces `limit`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trial_limit: Option<i64>,
    /// Unit shown with the limit (default: the feature's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// Length of a `rolling` window in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_secs: Option<i64>,
}

/// Body of `PUT /plans/{key}`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PlanInput {
    /// Can be assigned (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
    /// Description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The plan's features (replaces the set).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub features: Option<Vec<PlanFeature>>,
    /// Make this the default plan (unsets the previous default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_default: Option<bool>,
    /// Free-form data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    /// Display name (default: the key).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Trial length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trial_days: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PlanStepSchema {
    pub detail: String,
    pub id: i64,
    pub parent: i64,
}

/// One move of a rebalance plan.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PlannedMoveInfo {
    /// Estimated bytes.
    pub bytes: i64,
    /// Estimated freeze in milliseconds.
    pub est_freeze_ms: i64,
    /// From set id.
    pub from: String,
    /// Its project (a project's environments move together).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// Estimated rows.
    pub rows: i64,
    /// Tenant.
    pub tenant: String,
    /// To set id.
    pub to: String,
}

/// Body of `POST .../lists/{key}/pop`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PopBody {
    /// How many to remove (default 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<i64>,
    /// Head or tail.
    pub side: Side,
}

/// The customer portal: what a customer sees of its billing.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Portal {
    /// Newest credit notes.
    #[serde(default)]
    pub credit_notes: Vec<CreditNote>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub customer: Option<Customer>,
    /// Newest invoices.
    #[serde(default)]
    pub invoices: Vec<Invoice>,
    /// Newest payments.
    #[serde(default)]
    pub payments: Vec<Payment>,
    /// Subscriptions that have not ended.
    #[serde(default)]
    pub subscriptions: Vec<BillingSubscription>,
}

/// Result of a ledger write.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Posted {
    /// The entry.
    pub entry: LedgerEntry,
    /// The idempotency key was seen before: this is the first result.
    pub replayed: bool,
}

/// Members of a channel.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PresenceList {
    /// Channel name.
    pub channel: String,
    /// Members (every instance).
    #[serde(default)]
    pub members: Vec<Member>,
}

/// A rendered message.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Preview {
    /// HTML body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub html: Option<String>,
    /// Variables the template uses and the request did not provide.
    #[serde(default)]
    pub missing: Vec<String>,
    /// Subject.
    pub subject: String,
    /// Plain-text body.
    pub text: String,
}

/// Renders a template without sending.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PreviewInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft: Option<TemplateInput>,
    /// Render the stored (or built-in) template of this key …
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    /// Variables.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variables: Option<serde_json::Value>,
}

/// One line of a plan-change preview (the same lines the change would invoice).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreviewLine {
    /// Minor units; negative for a credit.
    pub amount: i64,
    /// What the line is for.
    pub description: String,
    /// `proration`.
    pub kind: LineKind,
    /// Price key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price: Option<String>,
    /// Units.
    pub quantity: i64,
}

/// A price of a product.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Price {
    /// Sellable.
    pub active: bool,
    /// Created.
    pub created_at: String,
    /// Primary currency.
    pub currency: String,
    /// The same price in other currencies.
    #[serde(default)]
    pub currency_options: std::collections::HashMap<String, CurrencyOption>,
    /// Metered: the FB-24 feature whose usage events are billed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feature: Option<String>,
    /// Id.
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval: Option<BillingInterval>,
    /// Intervals per period.
    pub interval_count: i64,
    /// Stable key.
    pub key: String,
    /// Caller data.
    #[serde(default)]
    pub metadata: serde_json::Value,
    /// Metered: all units or only overage.
    pub meter_basis: MeterBasis,
    /// Product key.
    pub product: String,
    /// Product id.
    pub product_id: String,
    /// Quantity to amount.
    pub scheme: Scheme,
    /// Whether the amounts include tax.
    pub tax_behavior: TaxBehavior,
    /// Tiers in the primary currency.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tiers: Option<Vec<Tier>>,
    /// Free trial days when subscribing.
    pub trial_days: i64,
    /// How it is charged.
    pub r#type: PriceType,
    /// Price per unit in the primary currency (per-unit scheme).
    pub unit_amount: i64,
}

/// Body of `PUT /billing/prices/{key}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PriceInput {
    /// Sellable (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
    /// Primary currency.
    pub currency: String,
    /// The same price in other currencies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency_options: Option<std::collections::HashMap<String, CurrencyOption>>,
    /// Metered: FB-24 feature key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval: Option<BillingInterval>,
    /// Intervals per period (default 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval_count: Option<i64>,
    /// Caller data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meter_basis: Option<MeterBasis>,
    /// Product key.
    pub product: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheme: Option<Scheme>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tax_behavior: Option<TaxBehavior>,
    /// Tiers (graduated and volume).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tiers: Option<Vec<Tier>>,
    /// Free trial days.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trial_days: Option<i64>,
    /// How it is charged.
    pub r#type: PriceType,
    /// Price per unit, minor units.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit_amount: Option<i64>,
}

/// How a price is charged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PriceType {
    #[serde(rename = "recurring")]
    Recurring,
    #[serde(rename = "one_time")]
    OneTime,
    #[serde(rename = "metered")]
    Metered,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// A product.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Product {
    /// Sellable.
    pub active: bool,
    /// FB-24 add-on (kind `addon`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addon_key: Option<String>,
    /// Created.
    pub created_at: String,
    /// Credits per unit (kind `credits`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credits: Option<i64>,
    /// Description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Id.
    pub id: String,
    /// Stable key.
    pub key: String,
    /// What a paid line delivers.
    pub kind: ProductKind,
    /// Caller data.
    #[serde(default)]
    pub metadata: serde_json::Value,
    /// Display name.
    pub name: String,
    /// FB-24 usage pack (kind `usage_pack`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pack_key: Option<String>,
    /// FB-24 plan (kind `plan`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_key: Option<String>,
    /// Changed.
    pub updated_at: String,
}

/// Body of `PUT /billing/products/{key}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProductInput {
    /// Sellable (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
    /// FB-24 add-on key (kind `addon`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addon_key: Option<String>,
    /// Credits per unit (kind `credits`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credits: Option<i64>,
    /// Description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// What a paid line delivers.
    pub kind: ProductKind,
    /// Caller data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    /// Display name.
    pub name: String,
    /// FB-24 pack key (kind `usage_pack`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pack_key: Option<String>,
    /// FB-24 plan key (kind `plan`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_key: Option<String>,
}

/// What a paid line of a product delivers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProductKind {
    #[serde(rename = "plan")]
    Plan,
    #[serde(rename = "addon")]
    Addon,
    #[serde(rename = "usage_pack")]
    UsagePack,
    #[serde(rename = "credits")]
    Credits,
    #[serde(rename = "service")]
    Service,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// The latest progress report of a job.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Progress {
    /// Human-readable detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// 0 to 100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percent: Option<f64>,
    /// Short name of the current phase, e.g. `download`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
    /// When it was reported (RFC 3339).
    pub updated_at: String,
}

/// Body of a progress report.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ProgressBody {
    /// The lease from the claim.
    pub lease_id: String,
    /// Detail (at most 2000 characters).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// 0 to 100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percent: Option<f64>,
    /// Phase name (at most 128 characters).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
}

/// A progress report. Fields left out keep their previous value.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ProgressReport {
    /// Detail (at most 2000 characters).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// 0 to 100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percent: Option<f64>,
    /// Phase name (at most 128 characters).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
}

/// Body of `POST /api/system/database/nodes/{id}/promote`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PromoteBody {
    /// "The old primary is down": promote even though recent commits may be lost.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accept_data_loss: Option<bool>,
    /// The node name, typed by the operator.
    pub confirm: String,
}

/// `GET /api/system/database/nodes/{id}/promote-check`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PromoteCheck {
    /// Bytes the replica is behind (when both are known).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behind_bytes: Option<i64>,
    /// The primary's position (when it answers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_lsn: Option<String>,
    /// The old primary answers: a promote loses nothing. `false`: the
    /// dialog shows "the old primary is down" and needs `accept_data_loss`.
    pub primary_reachable: bool,
    /// The replica's replay position.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replayed: Option<String>,
}

/// How to bill a plan change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Proration {
    #[serde(rename = "always_invoice")]
    AlwaysInvoice,
    #[serde(rename = "create_prorations")]
    CreateProrations,
    #[serde(rename = "none")]
    None,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// What `POST /billing/subscriptions/{id}/change` would do, without doing it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProrationPreview {
    /// The change is scheduled for the period end.
    pub at_period_end: bool,
    /// Charge for the remaining time (minor units).
    pub charge: i64,
    /// Credit for unused time (positive number, minor units).
    pub credit: i64,
    /// Currency of every amount.
    pub currency: String,
    /// When the change takes effect.
    pub effective_at: String,
    /// Current plan price key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_price: Option<String>,
    /// An invoice is raised immediately.
    pub invoiced_now: bool,
    /// Proration lines (credit for unused time, charge for the rest).
    #[serde(default)]
    pub lines: Vec<PreviewLine>,
    /// `charge - credit`: positive is collected now (`always_invoice`) or at
    /// renewal (`create_prorations`); negative goes to the customer balance.
    pub net: i64,
    /// Amount of the plan price for a whole period at the new price.
    pub new_period_amount: i64,
    /// Current period end.
    pub period_end: String,
    /// Current period start.
    pub period_start: String,
    /// Proration mode that applies (`none` for trials and period-end changes).
    pub proration: Proration,
    /// Units after the change.
    pub quantity: i64,
    /// Subscription id.
    pub subscription_id: String,
    /// New plan price key.
    pub to_price: String,
}

/// A configured payment provider (secrets are never returned).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ProviderAccount {
    /// API base URL override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_base: Option<String>,
    /// The non-secret provider-specific settings (never `private_key`,
    /// `service_account` or `platform_keys`).
    #[serde(default)]
    pub config: serde_json::Value,
    /// In use.
    pub enabled: bool,
    /// The provider environment: `sandbox`, `xcode` or `local_testing` for the
    /// App Store, `test` for every other provider.
    pub environment: String,
    /// Last characters of the API secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// Public key id (Razorpay).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_id: Option<String>,
    /// `stripe`, `razorpay` or `manual`.
    pub provider: String,
    /// Names of the secrets that are stored (`api_key`, `webhook_secret`,
    /// `config.private_key`, ...). The values are never returned.
    #[serde(default)]
    pub secrets_set: Vec<String>,
    /// Always `true`: only test-mode credentials are accepted (live keys are
    /// refused when saved).
    pub test_mode: bool,
    /// Changed.
    pub updated_at: String,
    /// Webhook URL to register at the provider (relative to the public URL).
    pub webhook_path: String,
}

/// Body of `PUT /billing/providers/{provider}`. Test-mode credentials only.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ProviderInput {
    /// API base URL (default: the provider's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_base: Option<String>,
    /// Stripe secret key (`sk_test_…`) or Razorpay key secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Fields to remove from the stored account: `api_key`, `webhook_secret`,
    /// `key_id`, `api_base` or `config.<name>`. A secret or config field that
    /// is omitted, `null` or empty keeps its stored value; list it here to
    /// clear it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clear: Option<Vec<String>>,
    /// Provider-specific settings, sealed with the secrets (FB-72): App Store
    /// `issuer_id`, `key_id`, `private_key`, `bundle_id`, `environment`;
    /// Google Play `package_name`, `service_account`, `push_audience`,
    /// `push_service_account`; WeChat Pay `appid`, `merchant_serial`,
    /// `private_key`, `platform_keys`, `notify_url`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
    /// In use (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Razorpay key id (`rzp_test_…`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_id: Option<String>,
    /// Webhook signing secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_secret: Option<String>,
}

/// Snapshots removed by a prune.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PruneResult {
    /// Ids of the deleted snapshots.
    #[serde(default)]
    pub deleted: Vec<String>,
}

/// The public face of a realm: what dashboards and hosted pages need.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PublicBranding {
    /// Accent colour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent_color: Option<String>,
    /// Background colour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_color: Option<String>,
    /// Product name (the realm id when none is set).
    pub display_name: String,
    /// Verified custom domains of the realm.
    #[serde(default)]
    pub domains: Vec<String>,
    /// URL of the favicon (when uploaded).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub favicon_url: Option<String>,
    /// URL of the logo (when uploaded).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logo_url: Option<String>,
    /// Primary colour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_color: Option<String>,
    /// Realm.
    pub realm: String,
    /// Sign-in footer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signin_footer: Option<String>,
    /// Sign-in sub-heading.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signin_subtitle: Option<String>,
    /// Sign-in heading.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signin_title: Option<String>,
    /// Support email.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub support_email: Option<String>,
    /// Text colour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_color: Option<String>,
}

/// Body of `POST /publish/{channel}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PublishBody {
    /// The message.
    pub message: Bin,
}

/// Body of a publish.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PublishRequest {
    /// 1 to 1000 messages, published in one transaction.
    #[serde(default)]
    pub messages: Vec<NewMessage>,
}

/// Response of a publish.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PublishResponse {
    /// One per message, in order.
    #[serde(default)]
    pub results: Vec<Published>,
}

/// The outcome of one published message.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Published {
    /// `true` when the dedup id was seen inside the window and nothing was stored.
    pub duplicate: bool,
    /// Message id (of the first message for a duplicate).
    pub message_id: String,
    /// Sequence number in the topic (`0` for a duplicate whose first message expired).
    pub seq: i64,
}

/// Body of a pull.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PullRequest {
    /// Consumer label recorded in the delivery history (default `rest`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consumer: Option<String>,
    /// Most messages to return, 1-1000 (default 10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_messages: Option<i64>,
    /// Long poll: wait up to this long for a message (default 0, capped by the server).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_ms: Option<i64>,
}

/// Response of a pull.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PullResponse {
    /// Possibly empty.
    #[serde(default)]
    pub messages: Vec<ReceivedMessage>,
}

/// One push attempt of a subscription: a delivery or a test send (ADR-0031 §1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PushAttempt {
    /// When it started (RFC 3339).
    pub at: String,
    /// Delivery attempt of the message, from 1.
    pub attempt: i64,
    /// Webhook URL or function name.
    pub endpoint: String,
    /// The failure in words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_class: Option<PushErrorClass>,
    /// Record id (increasing).
    pub id: i64,
    /// `webhook` or `function`.
    pub kind: PushKind,
    /// How long it took in milliseconds.
    pub latency_ms: i64,
    /// Message delivered (`test-…` for a test send).
    pub message_id: String,
    /// `success` or `failure`.
    pub outcome: PushOutcome,
    /// Headers sent (the signature masked).
    #[serde(default)]
    pub request_headers: std::collections::HashMap<String, String>,
    /// The answer's body, cut to `[queue] push_response_body_bytes` (lossy UTF-8).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_body: Option<String>,
    /// Full size of the answer's body in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_bytes: Option<i64>,
    /// Allowlisted headers of the answer (`content-type`, `retry-after`, request ids, ...).
    #[serde(default)]
    pub response_headers: std::collections::HashMap<String, String>,
    /// The body was cut.
    pub response_truncated: bool,
    /// HTTP status of the answer (webhooks that answered).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<i64>,
    /// Subscription.
    pub subscription: String,
    /// A test send: not a message, no effect on delivery state or totals.
    pub test: bool,
}

/// Push attempts of a subscription.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PushAttemptPage {
    /// Newest first.
    #[serde(default)]
    pub items: Vec<PushAttempt>,
    /// Pass as `before` for the next page; absent at the end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_before: Option<i64>,
}

/// Body of `POST .../lists/{key}/push`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PushBody {
    /// Head or tail.
    pub side: Side,
    /// Values, pushed one after the other.
    #[serde(default)]
    pub values: Vec<Bin>,
}

/// Push delivery of a subscription.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PushConfig {
    /// Webhook URL (`https://`) or function name.
    pub endpoint: String,
    /// `webhook` or `function`.
    pub kind: PushKind,
}

/// Why a push attempt failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PushErrorClass {
    #[serde(rename = "timeout")]
    Timeout,
    #[serde(rename = "dns")]
    Dns,
    #[serde(rename = "connect")]
    Connect,
    #[serde(rename = "tls")]
    Tls,
    #[serde(rename = "ssrf_refused")]
    SsrfRefused,
    #[serde(rename = "http_status")]
    HttpStatus,
    #[serde(rename = "redirect")]
    Redirect,
    #[serde(rename = "protocol")]
    Protocol,
    #[serde(rename = "invalid_request")]
    InvalidRequest,
    #[serde(rename = "config")]
    Config,
    #[serde(rename = "function")]
    Function,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// Endpoint health of a push subscription (ADR-0031 §5): the attempts of the
/// last hour (at most the newest 1000; test sends excluded) and the
/// circuit-breaker state.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PushHealth {
    /// Attempts in the window.
    pub attempts: i64,
    /// Failed attempts since the last success.
    pub consecutive_failures: i64,
    /// Failed attempts in the window.
    pub failures: i64,
    /// Error of the last failed attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Last failed attempt (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_failure_at: Option<String>,
    /// Last successful attempt (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_success_at: Option<String>,
    /// Median latency in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_p50_ms: Option<i64>,
    /// 95th percentile latency in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_p95_ms: Option<i64>,
    /// 99th percentile latency in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_p99_ms: Option<i64>,
    /// Auto-pause threshold.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_after_failures: Option<i64>,
    /// Push delivery is paused.
    pub paused: bool,
    /// When it was paused (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused_at: Option<String>,
    /// Why it was paused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused_reason: Option<String>,
    /// `successes / attempts` (absent without attempts).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub success_rate: Option<f64>,
    /// Successful attempts in the window.
    pub successes: i64,
    /// Length of the rolling window in milliseconds.
    pub window_ms: i64,
}

/// Kind of push endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PushKind {
    #[serde(rename = "webhook")]
    Webhook,
    #[serde(rename = "function")]
    Function,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// How a push attempt ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PushOutcome {
    #[serde(rename = "success")]
    Success,
    #[serde(rename = "failure")]
    Failure,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// Element type of the ANN index. Canonical vectors are always stored as
/// float32; a smaller type makes the derived index smaller and faster at some
/// cost in precision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Quantization {
    #[serde(rename = "f32")]
    F32,
    #[serde(rename = "f16")]
    F16,
    #[serde(rename = "i8")]
    I8,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct QueryPlanSchema {
    #[serde(default)]
    pub full_scans: Vec<String>,
    #[serde(default)]
    pub indexes_used: Vec<String>,
    pub sql: String,
    #[serde(default)]
    pub steps: Vec<PlanStepSchema>,
}

/// A named queue: its settings and current counts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueueInfo {
    /// Cancelled (until purged).
    pub cancelled: i64,
    /// Most jobs running at once over every worker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concurrency: Option<i64>,
    /// Settings were saved for it (otherwise it only has jobs).
    pub configured: bool,
    /// Created (RFC 3339) when configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// Who saved the settings first (FB-146); `null`: not configured, or
    /// configured before FB-146.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
    /// In the dead-letter queue.
    pub dead: i64,
    /// Priority of jobs enqueued without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_priority: Option<i64>,
    /// Per-attempt timeout of jobs enqueued without one, in ms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_timeout_ms: Option<i64>,
    /// Queue name.
    pub name: String,
    /// Hands out no jobs while paused.
    pub paused: bool,
    /// Waiting jobs that may run (due or delayed).
    pub queued: i64,
    /// Most jobs started per `rate_period`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit: Option<i64>,
    /// `second` or `minute`.
    pub rate_period: RatePeriod,
    /// Leased by a worker.
    pub running: i64,
    /// Finished successfully (until purged).
    pub succeeded: i64,
    /// Tenant whose jobs these settings govern (FB-146); `null`: the jobs
    /// without a tenant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    /// Last settings change (RFC 3339) when configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// Waiting for parents (dependencies) to succeed.
    pub waiting: i64,
}

/// Named queues.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct QueueList {
    /// By name.
    #[serde(default)]
    pub items: Vec<QueueInfo>,
}

/// Settings of a named queue (`PUT /api/v1/jobs/queues/{name}`). Limits left
/// out are removed; `paused` left out keeps its current value.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct QueueSettings {
    /// Most jobs of this queue running at once, over every worker (none: no limit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concurrency: Option<i64>,
    /// Priority of jobs enqueued without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_priority: Option<i64>,
    /// Per-attempt timeout of jobs enqueued without one, in ms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_timeout_ms: Option<i64>,
    /// Paused queues hand out no jobs (running jobs continue).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused: Option<bool>,
    /// Most jobs started per `rate_period` (none: no limit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_period: Option<RatePeriod>,
}

/// Counts for one queue.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct QueueStats {
    /// Cancelled (until purged).
    pub cancelled: i64,
    /// In the dead-letter queue.
    pub dead: i64,
    /// Queue name.
    pub queue: String,
    /// Waiting (including delayed and waiting to retry).
    pub queued: i64,
    /// Leased by a worker.
    pub running: i64,
    /// Finished successfully (until purged).
    pub succeeded: i64,
}

/// A subscription.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct QueueSubscription {
    /// Ack deadline in milliseconds.
    pub ack_deadline_ms: i64,
    /// Backoff base in milliseconds.
    pub backoff_base_ms: i64,
    /// Backoff cap in milliseconds.
    pub backoff_max_ms: i64,
    /// Created (RFC 3339).
    pub created_at: String,
    /// Creator.
    pub created_by: String,
    /// Dead-letter topic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dead_letter_topic: Option<String>,
    /// When the subscription is deleted (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Attribute filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<serde_json::Value>,
    /// Labels.
    #[serde(default)]
    pub labels: std::collections::HashMap<String, String>,
    /// Attempt limit (`None`: unlimited).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_delivery_attempts: Option<i64>,
    /// Name.
    pub name: String,
    /// Ordered delivery per ordering key.
    pub ordering: bool,
    /// Auto-pause after this many consecutive failed push attempts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_after_failures: Option<i64>,
    /// When push delivery was paused (RFC 3339); messages accumulate until it is resumed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused_at: Option<String>,
    /// Why it was paused (`auto: …` for the circuit breaker).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push: Option<PushConfig>,
    /// Webhook signing secret: returned only when it is created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push_secret: Option<String>,
    /// End of the grace window of the secret a rotation replaced (RFC 3339):
    /// until then deliveries carry a signature with each secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push_secret_previous_expires_at: Option<String>,
    /// Owning tenant (FB-141); `None` for an instance-wide subscription.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    /// Topic.
    pub topic: String,
    /// Last changed (RFC 3339).
    pub updated_at: String,
}

/// Enforcement of a feature's limit.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct QuotaPolicy {
    /// Units one admitted operation needs (pipeline step 6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<i64>,
    /// Hard or soft.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enforcement: Option<Enforcement>,
    /// Feature key (set from the path).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feature: Option<String>,
    /// Default lease length for concurrency features.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_ttl_secs: Option<i64>,
    /// Soft limits: how far past the limit usage may go, in percent (`null`:
    /// no ceiling).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overage_pct: Option<i64>,
    /// Permission patterns pipeline step 6 admits against this quota.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<Vec<String>>,
    /// Most units that may roll over.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollover_cap: Option<i64>,
    /// Calendar periods: share of the limit that may roll over unused, 0-100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollover_pct: Option<i64>,
    /// Counter scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<QuotaScope>,
}

/// Which counter a quota uses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum QuotaScope {
    #[serde(rename = "subject")]
    Subject,
    #[serde(rename = "user")]
    User,
    #[serde(rename = "team")]
    Team,
    #[serde(rename = "resource")]
    Resource,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// The price of a metered feature in credits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rate {
    /// What is charged.
    pub basis: RateBasis,
    /// Created.
    pub created_at: String,
    /// Credits per `per_units` units.
    pub credits_per_unit: i64,
    /// Feature key.
    pub feature: String,
    /// Units the price is for (cost per event: `ceil(units × credits_per_unit / per_units)`).
    pub per_units: i64,
    /// Updated.
    pub updated_at: String,
}

/// What usage of a feature costs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RateBasis {
    #[serde(rename = "units")]
    Units,
    #[serde(rename = "overage")]
    Overage,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// Body of `PUT /credits/rates/{feature}`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RateInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basis: Option<RateBasis>,
    /// Credits per `per_units` units (at least 1).
    pub credits_per_unit: i64,
    /// Units the price is for (default 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_units: Option<i64>,
}

/// Window of a queue rate limit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RatePeriod {
    #[serde(rename = "second")]
    Second,
    #[serde(rename = "minute")]
    Minute,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// Error details.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RealtimeErrorDetail {
    /// Stable snake_case code.
    pub code: String,
    /// Human-readable message.
    pub message: String,
    /// Request id.
    pub request_id: String,
}

/// A message to publish.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RealtimePublishBody {
    /// Any JSON value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<ValueModel>,
}

/// Counters of one instance (also exported as Prometheus metrics).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RealtimeStats {
    /// Open connections.
    pub connections: i64,
    /// Events sent to subscribers (after authorisation).
    pub events_delivered: i64,
    /// Events published to the bus.
    pub events_dispatched: i64,
    /// Events withheld because the subscriber may not see them.
    pub events_withheld: i64,
    /// Newest event cursor published to the bus.
    pub head: i64,
    /// Connections refused by a limit.
    pub rejected: i64,
    /// Connections closed because their credential became invalid.
    pub revoked: i64,
    /// Connections closed as slow consumers.
    pub slow_consumers: i64,
    /// Active subscriptions.
    pub subscriptions: i64,
}

/// Body of `POST /api/system/database/rebalance`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RebalanceBody {
    /// The plan id, typed by the operator.
    pub confirm: String,
    /// From the plan.
    pub plan_id: String,
}

/// Body of `POST /api/system/database/rebalance/plan`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RebalancePlanBody {
    /// Most moves in the plan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_moves: Option<i64>,
    /// Sets to balance (default all).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sets: Option<Vec<String>>,
}

/// `POST /api/system/database/rebalance/plan`: shown before it runs.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RebalancePlanView {
    /// Loads after the plan.
    #[serde(default)]
    pub after: Vec<SetLoadInfo>,
    /// Loads now.
    #[serde(default)]
    pub before: Vec<SetLoadInfo>,
    /// RFC 3339.
    pub created_at: String,
    /// Sum of the estimated freezes.
    pub est_freeze_ms: i64,
    /// RFC 3339: after this the plan must be made again.
    pub expires_at: String,
    /// Plan id (type it to run the plan).
    pub id: String,
    /// Moves, in the order they run (one at a time).
    #[serde(default)]
    pub moves: Vec<PlannedMoveInfo>,
    /// Spread after.
    pub spread_after: f64,
    /// Spread of the scores now ((max − min) ÷ max).
    pub spread_before: f64,
}

/// The last completed rebuild.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RebuildInfo {
    /// Detail of the reason.
    pub detail: String,
    /// Wall-clock duration.
    pub duration_ms: i64,
    /// Completion time.
    pub finished: String,
    /// Why it ran (`missing`, `corrupt`, `stale`, `version_mismatch`,
    /// `params_changed`, `manual`, `apply_failed`).
    pub reason: String,
    /// Vectors indexed.
    pub vectors: i64,
}

/// Body of a rebuild request.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RebuildRequest {
    /// Only this field (default: every field of the collection).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
}

/// Indexes being rebuilt.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RebuildResponse {
    /// One entry per field.
    #[serde(default)]
    pub indexes: Vec<IndexStatus>,
}

/// Result of a recall check: ANN results compared with exact results.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RecallReport {
    /// Total time of the ANN searches.
    pub ann_ms: f64,
    /// Total time of the exact searches.
    pub exact_ms: f64,
    /// Vector field.
    pub field: String,
    /// Results compared per query.
    pub k: i64,
    /// When it ran.
    pub measured: String,
    /// Mean of |ANN ∩ exact| / k, from 0 to 1.
    pub recall: f64,
    /// Queries run (stored vectors used as queries).
    pub samples: i64,
}

/// Body of a recall check.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RecallRequest {
    /// Vector field; may be omitted when the collection has exactly one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// Results per query (default 10, at most 100).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub k: Option<i64>,
    /// Number of queries (default 100, at most 1000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub samples: Option<i64>,
}

/// A delivered message.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ReceivedMessage {
    /// Deadline (RFC 3339) after which it is redelivered.
    pub ack_deadline: String,
    /// Token for ack, nack and deadline changes of this delivery.
    pub ack_id: String,
    /// 1 for the first delivery.
    pub delivery_attempt: i64,
    /// The message.
    pub message: Message,
}

/// What the reconciler did.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ReconcileReport {
    /// Blobs the metadata names but the driver lacks (reported, not repaired).
    #[serde(default)]
    pub missing_blobs: Vec<String>,
    /// Stored blobs without metadata removed.
    pub orphan_blobs_deleted: i64,
    /// Staged files removed (abandoned parts and single uploads).
    pub staging_deleted: i64,
    /// Unreferenced blobs (no object version uses them) removed.
    pub unreferenced_blobs_deleted: i64,
    /// Expired upload sessions aborted.
    pub uploads_aborted: i64,
}

/// Record changes of a collection relayed from the realtime outbox (FB-40)
/// into a topic: a message exists exactly when its change committed.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RecordSource {
    /// Collection name or id.
    pub collection: String,
    /// `create`, `update`, `delete`; empty means all three.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ops: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RecordVersionSchema {
    pub actor: String,
    pub changed_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    /// `create`, `update` or `delete`.
    pub op: String,
    pub version: i64,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RecordVersions {
    #[serde(default)]
    pub items: Vec<RecordVersionSchema>,
}

/// Monthly and annual recurring revenue of one currency.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RecurringRevenue {
    /// MRR × 12.
    pub arr: i64,
    /// Currency.
    pub currency: String,
    /// Monthly recurring revenue (yearly prices / 12, after coupons).
    pub mrr: i64,
    /// Subscriptions counted (active and past due).
    pub subscriptions: i64,
}

/// Body of a redrive: `ids`, or `all` (optionally filtered).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RedriveRequest {
    /// Every dead letter of the subscription not yet redriven (oldest first, at most `limit`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub all: Option<bool>,
    /// With `all`: only records whose message attributes match this filter (subscription filter syntax).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<serde_json::Value>,
    /// Dead-letter record ids (1-1000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ids: Option<Vec<i64>>,
    /// With `all`: most records to redrive in this call (1-1000, default 1000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    /// With `all`: only records dead-lettered at or after this RFC 3339 time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
}

/// Outcome of a redrive.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RedriveResult {
    /// Records that were redriven before (each record is redriven once).
    #[serde(default)]
    pub already_redriven: Vec<i64>,
    /// Ids that are not dead letters of this subscription.
    #[serde(default)]
    pub not_found: Vec<i64>,
    /// Records delivered to the subscription again (attempt counter reset).
    #[serde(default)]
    pub redriven: Vec<i64>,
    /// With `all`: matching records left for another call.
    pub remaining: i64,
}

/// A refund.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Refund {
    /// Amount.
    pub amount: i64,
    /// Created.
    pub created_at: String,
    /// Credit note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credit_note_id: Option<String>,
    /// Currency.
    pub currency: String,
    /// Id.
    pub id: String,
    /// Invoice.
    pub invoice_id: String,
    /// Payment refunded.
    pub payment_id: String,
    /// Provider.
    pub provider: String,
    /// Refund id at the provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_ref: Option<String>,
    /// Why.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// State.
    pub status: RefundStatus,
}

/// Refund state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RefundStatus {
    #[serde(rename = "pending")]
    Pending,
    #[serde(rename = "succeeded")]
    Succeeded,
    #[serde(rename = "failed")]
    Failed,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// A statement that got slower, or started failing.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Regression {
    /// Subsystem.
    pub area: String,
    /// `sqlite` or `postgres`.
    pub backend: String,
    /// The baseline window.
    pub baseline: WindowStat,
    /// `recent p95 - baseline p95` in milliseconds.
    pub delta_ms: f64,
    /// Sort key: how much extra time the recent runs cost, in milliseconds.
    pub extra_ms: f64,
    /// Fingerprint.
    pub fingerprint: String,
    /// `latency` (p95 grew) or `errors` (the error rate grew).
    pub kind: String,
    /// `recent p95 / baseline p95` (latency) or the error rate difference (errors).
    pub ratio: f64,
    /// The recent window.
    pub recent: WindowStat,
    /// The statement with every value replaced by `?`.
    pub statement: String,
}

/// What a regression check compared.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RegressionReport {
    /// Start of the baseline window.
    pub baseline_from: String,
    /// End of the baseline window (the start of the recent one).
    pub baseline_to: String,
    /// The regressions, most costly first.
    #[serde(default)]
    pub items: Vec<Regression>,
    /// Fewest runs in each window.
    pub min_calls: i64,
    /// Smallest p95 growth reported, in milliseconds.
    pub min_delta_ms: f64,
    /// Smallest `recent p95 / baseline p95` reported.
    pub min_ratio: f64,
    /// Start of the recent window (RFC 3339).
    pub recent_from: String,
    /// End of the recent window.
    pub recent_to: String,
}

/// Answer of the removals.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Removed {
    /// The removed node or set id.
    pub removed: String,
}

/// Body of a lease renewal.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RenewBody {
    /// New hold from now (default: the policy's lease length).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_secs: Option<i64>,
}

/// A usage reservation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reservation {
    /// Units recorded at commit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub committed_units: Option<i64>,
    /// Created.
    pub created_at: String,
    /// Held until.
    pub expires_at: String,
    /// Feature.
    pub feature: String,
    /// Id.
    pub id: String,
    /// Of which past a soft limit.
    pub overage_units: i64,
    /// Of which from usage packs.
    pub pack_units: i64,
    /// `true` when returned for a replayed idempotency key.
    pub replayed: bool,
    /// Counter scope key.
    pub scope_key: String,
    /// State.
    pub status: ReservationStatus,
    /// Subject.
    pub subject: String,
    /// Units held.
    pub units: i64,
}

/// State of a reservation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReservationStatus {
    #[serde(rename = "held")]
    Held,
    #[serde(rename = "committed")]
    Committed,
    #[serde(rename = "released")]
    Released,
    #[serde(rename = "expired")]
    Expired,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// Version to restore.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RestoreBody {
    /// Key.
    pub key: String,
    /// Version id.
    pub version_id: String,
}

/// Which snapshots to keep.
///
/// `keep_last` is the number of newest snapshots that are always kept;
/// `max_age_days` deletes older ones beyond that. With only `keep_last`, the
/// rest is deleted; with only `max_age_days`, nothing younger is deleted;
/// with neither, nothing is deleted.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Retention {
    /// Newest snapshots always kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_last: Option<i64>,
    /// Snapshots older than this many days are deleted (beyond `keep_last`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_age_days: Option<i64>,
}

/// Data of a removed shard kept on disk.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RetiredInfo {
    /// Bytes on disk.
    pub bytes: i64,
    /// Id (`<node name>-<unix time>`).
    pub id: String,
    /// The shard node's name (type it to delete).
    pub name: String,
    /// The set it held.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_id: Option<String>,
}

/// `GET /api/system/database/retired`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RetiredList {
    /// Oldest first.
    #[serde(default)]
    pub items: Vec<RetiredInfo>,
}

/// Body of a "retry now" (optional).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RetryRequest {
    /// Only these messages (default: every message waiting for a retry backoff).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_ids: Option<Vec<String>>,
}

/// Outcome of a "retry now".
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RetryResult {
    /// Deliveries that were waiting for a backoff and are ready now.
    pub count: i64,
}

/// Revenue of one currency in one bucket.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RevenuePoint {
    /// Currency.
    pub currency: String,
    /// Payments collected.
    pub gross: i64,
    /// gross − refunded.
    pub net: i64,
    /// Successful payments.
    pub payments: i64,
    /// Refunds.
    pub refunded: i64,
    /// Bucket start.
    pub start: String,
}

/// Result of a reversal: takes back what is left of the top-up or grant, up
/// to the amount asked; the rest was already spent (the balance never goes
/// negative).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Reversal {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<LedgerEntry>,
    /// The idempotency key was seen before.
    pub replayed: bool,
    /// Credits taken back.
    pub reversed: i64,
    /// Credits asked for but already spent or lapsed.
    pub unrecovered: i64,
}

/// Body of a rollback.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RollbackBody {
    /// Version to activate (default: the newest version before the active one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<i64>,
}

/// Result of a rollup.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RollupResult {
    /// The day rolled up.
    pub day: String,
    /// Values written.
    pub values: i64,
}

/// Body of `POST /system/db-logins/{name}/rotate`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RotateBody {
    /// New expiry (RFC 3339); absent keeps the current one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_until: Option<String>,
}

/// Body of a secret rotation (optional).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RotateSecretRequest {
    /// How long the old secret keeps signing too, 0 to 7 days (default 24 hours; `0` drops it at once).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grace_ms: Option<i64>,
}

/// A rotated webhook secret.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RotatedSecret {
    /// Until when deliveries are also signed with the old secret (RFC 3339; absent with no grace).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_expires_at: Option<String>,
    /// The new signing secret: shown only here.
    pub push_secret: String,
    /// Subscription.
    pub subscription: String,
}

/// `run_as` of a SQL editor body (FB-153): whose claims the statements run
/// with.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RunAsBody {
    /// Principal key: `user:<collection>/<id>` or `service:<id>`.
    pub principal: String,
    /// The tenant to act in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
}

/// Answer of `GET /{ns}/keys`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ScanResponse {
    /// Pass back as `cursor` for the next page; `"0"` when done.
    pub cursor: String,
    /// Keys of this page.
    #[serde(default)]
    pub keys: Vec<String>,
}

/// A recurring job definition.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Schedule {
    /// First retry delay in ms.
    pub backoff_base_ms: i64,
    /// Cap of the retry delay in ms.
    pub backoff_max_ms: i64,
    /// Creation time (RFC 3339).
    pub created_at: String,
    /// Who created it.
    pub created_by: String,
    /// Cron expression (UTC; 5 fields, or 6 with leading seconds).
    pub cron: String,
    /// Fires only while enabled.
    pub enabled: bool,
    /// What fires it: `forgebase` (the server's tick) or `pg_cron` (FB-165;
    /// PostgreSQL with the extension, expressions it reads like ForgeBase).
    pub fired_by: String,
    /// Schedule id.
    pub id: String,
    /// Handler name.
    pub kind: String,
    /// Job of the last firing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_job_id: Option<String>,
    /// Last firing (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<String>,
    /// Attempts per job.
    pub max_attempts: i64,
    /// Unique name.
    pub name: String,
    /// Next firing (RFC 3339). For a schedule pg_cron fires it is computed
    /// from the expression when read.
    pub next_run_at: String,
    /// Handler input.
    #[serde(default)]
    pub payload: serde_json::Value,
    /// Priority of the jobs.
    pub priority: i64,
    /// Queue of the jobs.
    pub queue: String,
    /// Tenant of the creator (FB-142); its jobs run in it. `null`: none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    /// Last change (RFC 3339).
    pub updated_at: String,
    /// Visibility timeout of the jobs in ms.
    pub visibility_timeout_ms: i64,
}

/// Schedules.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ScheduleList {
    /// By name.
    #[serde(default)]
    pub items: Vec<Schedule>,
}

/// Changes to a schedule.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SchedulePatch {
    /// New cron expression.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cron: Option<String>,
    /// Enable or disable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// New payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    /// New priority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SchemaChangeSchema {
    pub action: String,
    pub actor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<serde_json::Value>,
    pub applied_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection_id: Option<String>,
    pub id: i64,
    #[serde(default)]
    pub statements: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SchemaHistory {
    #[serde(default)]
    pub items: Vec<SchemaChangeSchema>,
}

/// How quantity becomes an amount.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Scheme {
    #[serde(rename = "per_unit")]
    PerUnit,
    #[serde(rename = "graduated")]
    Graduated,
    #[serde(rename = "volume")]
    Volume,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// One scored member.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoredMember {
    /// Member.
    pub member: Bin,
    /// Score (a finite number, or the strings are not accepted: use `exec` for infinities).
    pub score: f64,
}

/// One search result.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SearchHit {
    /// Distance in the field's metric (lower is closer).
    pub distance: f64,
    /// Item id.
    pub id: String,
    /// Metadata, unless `include_metadata` is false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    /// Similarity (higher is closer): cosine similarity, dot product, or
    /// `1 / (1 + euclidean distance)`.
    pub score: f64,
    /// Text, when `include_text` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Vector, when `include_vectors` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vector: Option<Vec<f64>>,
}

/// A similarity search.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SearchRequest {
    /// Brute force instead of the ANN index (ground truth, small sets).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exact: Option<bool>,
    /// Vector field; may be omitted when the collection has exactly one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// Metadata filter in the filter language, e.g.
    /// `category = 'news' && year >= 2020`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    /// When the filter is applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter_mode: Option<FilterMode>,
    /// Return each hit's metadata (default `true`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_metadata: Option<bool>,
    /// Return each hit's text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_text: Option<bool>,
    /// Return each hit's vector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_vectors: Option<bool>,
    /// Number of results, 1 to 1000 (default 10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub k: Option<i64>,
    /// Values for `{:name}` parameters of the filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
    /// Query vector.
    #[serde(default)]
    pub vector: Vec<f64>,
}

/// Search results.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchResponse {
    /// Items matching the filter, when the filter was evaluated first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidates: Option<i64>,
    /// Closest first.
    #[serde(default)]
    pub hits: Vec<SearchHit>,
    /// How the search was answered.
    pub strategy: Strategy,
    /// Server-side time in milliseconds.
    pub took_ms: f64,
}

/// Body of a secret.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SecretBody {
    /// The value (sealed at rest, never returned).
    pub value: String,
}

/// A secret's metadata (values are never returned).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SecretInfo {
    /// Created (RFC 3339).
    pub created_at: String,
    /// Name.
    pub name: String,
    /// Last change (RFC 3339).
    pub updated_at: String,
}

/// Secrets (names only).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SecretList {
    /// By name.
    #[serde(default)]
    pub items: Vec<SecretInfo>,
}

/// Body of a seek: exactly one of `time` and `snapshot`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SeekRequest {
    /// Restore the acknowledgement state saved in this snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<String>,
    /// Messages published before this RFC 3339 time count as acknowledged, the rest are delivered again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<String>,
}

/// Outcome of a seek.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SeekResult {
    /// The subscription's cursor afterwards.
    pub cursor_seq: i64,
    /// Messages restored from the snapshot (0 for a time seek).
    pub restored: i64,
}

/// Sends a templated message through the queue.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SendInput {
    /// Template key.
    pub template: String,
    /// Recipient.
    pub to: String,
    /// Variables.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variables: Option<serde_json::Value>,
}

/// Body of `PUT .../strings/{key}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetBody {
    /// Keep the key's current TTL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keepttl: Option<bool>,
    /// Only set when the key does not exist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nx: Option<bool>,
    /// Expire after this many milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_ms: Option<i64>,
    /// The value.
    pub value: Bin,
    /// Only set when the key exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xx: Option<bool>,
}

/// One set (control or a shard).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SetInfo {
    /// Sum of the tenants' estimated sizes in bytes.
    pub est_bytes: i64,
    /// Takes new tenants.
    pub hosts_tenants: bool,
    /// Id (`control` for control).
    pub id: String,
    /// `control` or `shard`.
    pub kind: String,
    /// Display name.
    pub name: String,
    /// Set number (0 for control).
    pub number: i64,
    /// `provisioning`, `active`, `draining`, `migrating` or `retired`.
    pub state: String,
    /// Tenants placed on it (control also serves tenants with no placement row).
    pub tenants: i64,
    /// Relative capacity.
    pub weight: i64,
}

/// A set's load in a plan (bytes ÷ weight).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SetLoadInfo {
    /// Estimated bytes.
    pub bytes: i64,
    /// Set name.
    pub name: String,
    /// Bytes ÷ weight.
    pub score: f64,
    /// Set id.
    pub set_id: String,
    /// Tenants.
    pub tenants: i64,
    /// Weight.
    pub weight: i64,
}

/// `POST /api/setup/admin` body.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SetupAdminRequest {
    /// Cookie session (default `true`); `false` returns bearer tokens instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cookie: Option<bool>,
    /// Name for the device list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
    pub email: String,
    /// At least 10 characters.
    pub password: String,
    /// The setup token from the link `forgebase serve` printed.
    pub token: String,
}

/// `GET /api/setup/status` response.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SetupStatus {
    /// `true` while no administrator exists.
    pub setup_required: bool,
}

/// Where a new shard's primary runs.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ShardNode {
    /// `external` (a server you run, at `url`). `local` and `remote` shard
    /// primaries come with FB-174.
    pub kind: String,
    /// Node name.
    pub name: String,
    /// `external` only: `postgres://` URL of an empty database (stored sealed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// Which end of a list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    #[serde(rename = "left")]
    Left,
    #[serde(rename = "right")]
    Right,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// A request for a signed URL.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SignRequest {
    /// Content type the upload must declare (uploads).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    /// File name to offer on download (`Content-Disposition: attachment`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download_as: Option<String>,
    /// Lifetime in seconds (default 900, at most the server limit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in: Option<i64>,
    /// Object key.
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<SignedMethod>,
    /// A specific version (downloads).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_id: Option<String>,
}

/// Which HTTP method a signed URL allows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SignedMethod {
    #[serde(rename = "GET")]
    GET,
    #[serde(rename = "PUT")]
    PUT,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// A signed URL.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignedUrl {
    /// RFC 3339 expiry.
    pub expires_at: String,
    /// `GET` or `PUT`.
    pub method: SignedMethod,
    /// Path and query, relative to the server root; usable without credentials
    /// until `expires_at`.
    pub url: String,
}

/// A service level objective over a group of routes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Slo {
    /// Multi-window burn-rate alerts on.
    pub alerting: bool,
    /// Created.
    pub created_at: String,
    /// What it promises.
    pub description: String,
    /// Id.
    pub id: String,
    /// Kind.
    pub kind: SloKind,
    /// Extra labels of its alerts.
    #[serde(default)]
    pub labels: std::collections::HashMap<String, String>,
    /// Latency SLOs: the threshold.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_threshold_ms: Option<i64>,
    /// Only these methods (empty: all).
    #[serde(default)]
    pub methods: Vec<String>,
    /// Unique name (the `slo` label).
    pub name: String,
    /// Target share of good requests (0.9 to 0.99999).
    pub objective: f64,
    /// Route templates starting with this (`/api/v1/collections`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_prefix: Option<String>,
    /// Or route templates matching this anchored regular expression.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_regex: Option<String>,
    /// Changed.
    pub updated_at: String,
    /// Rolling window (`30d`).
    pub window: String,
    /// The same in milliseconds.
    pub window_ms: i64,
}

/// One burn-rate alert tier.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SloBurnTier {
    /// Burn rate both windows must exceed.
    pub burn_rate_threshold: f64,
    /// Whether it fires now.
    pub firing: bool,
    /// Long window.
    pub long_window: String,
    /// Rule id.
    pub rule_id: String,
    /// Severity.
    pub severity: AlertSeverity,
    /// Short window.
    pub short_window: String,
    /// `page_fast`, `page_slow` or `ticket`.
    pub tier: String,
}

/// Error budget of an SLO over its window.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SloErrorBudget {
    /// Bad events the objective allows over the window: `(1 - objective) * total`.
    pub allowed: f64,
    /// Bad events so far.
    pub consumed: i64,
    /// Share left: `1 - consumed / allowed` (below 0 when overspent; 1 with no traffic).
    pub remaining_ratio: f64,
}

/// A new SLO or a change (every field replaces).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SloInput {
    /// Burn-rate alerts (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alerting: Option<bool>,
    /// Description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Kind.
    pub kind: SloKind,
    /// Extra alert labels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labels: Option<std::collections::HashMap<String, String>>,
    /// Latency threshold in milliseconds (latency SLOs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_threshold_ms: Option<i64>,
    /// Methods (`GET`, `POST`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub methods: Option<Vec<String>>,
    /// Unique name (letters, digits, `_`, `-`, `.`; 1-64).
    pub name: String,
    /// Objective, such as 0.999.
    pub objective: f64,
    /// Route prefix (one of prefix or regex; neither: every route).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_prefix: Option<String>,
    /// Route regular expression.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_regex: Option<String>,
    /// Window (default `30d`; 1h to 90d).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<String>,
}

/// SLO kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SloKind {
    #[serde(rename = "availability")]
    Availability,
    #[serde(rename = "latency")]
    Latency,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// Where an SLO stands.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SloStatus {
    /// Burn-rate alert tiers.
    #[serde(default)]
    pub alerts: Vec<SloBurnTier>,
    /// Bad requests.
    pub bad: i64,
    /// Burn rate per window (`5m`, `30m`, `1h`, `6h`, `3d`): error ratio / (1 - objective).
    #[serde(default)]
    pub burn_rates: std::collections::HashMap<String, Option<f64>>,
    /// The error budget.
    pub error_budget: SloErrorBudget,
    /// Good requests.
    pub good: i64,
    /// `good / total` (`null` without traffic).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sli: Option<f64>,
    /// The SLO.
    pub slo: Slo,
    /// Requests in the window.
    pub total: i64,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SlowQueryLabelSchema {
    pub collection: String,
    #[serde(default)]
    pub equality_keys: Vec<String>,
    #[serde(default)]
    pub range_keys: Vec<String>,
    #[serde(default)]
    pub sort_keys: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SlowQueryLog {
    #[serde(default)]
    pub items: Vec<SlowQuerySchema>,
    pub threshold_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SlowQuerySchema {
    pub at: String,
    pub duration_ms: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<SlowQueryLabelSchema>,
    pub param_count: i64,
    pub rows: i64,
    /// SQL text; parameter values are never recorded.
    pub sql: String,
}

/// One slow statement.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SlowStatementEntry {
    /// Subsystem.
    pub area: String,
    /// When it finished (RFC 3339).
    pub at: String,
    /// `sqlite` or `postgres`.
    pub backend: String,
    /// Wall time in milliseconds.
    pub duration_ms: f64,
    /// It failed.
    pub error: bool,
    /// Fingerprint.
    pub fingerprint: String,
    /// Row id (the paging cursor).
    pub id: i64,
    /// Rows returned or changed.
    pub rows: i64,
    /// The statement with every value replaced by `?`.
    pub statement: String,
    /// It ran inside a write transaction.
    pub write: bool,
}

/// A page of slow statements, newest first.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SlowStatementPage {
    /// The statements.
    #[serde(default)]
    pub items: Vec<SlowStatementEntry>,
    /// Pass as `cursor` for the next page; absent after the last one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<i64>,
}

/// A saved acknowledgement state of a subscription.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Snapshot {
    /// Created (RFC 3339).
    pub created_at: String,
    /// Creator.
    pub created_by: String,
    /// Cursor at the time.
    pub cursor_seq: i64,
    /// Expires (RFC 3339): the topic's retention after creation.
    pub expires_at: String,
    /// Name.
    pub name: String,
    /// Subscription.
    pub subscription: String,
    /// Owning tenant (FB-141); `None` for an instance-wide snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    /// Topic.
    pub topic: String,
    /// Messages that were unacknowledged at the time.
    pub unacked: i64,
}

/// Snapshots, newest first.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SnapshotList {
    /// The snapshots.
    #[serde(default)]
    pub items: Vec<SnapshotView>,
}

/// A snapshot.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SnapshotView {
    /// Snapshot id (its UTC creation time).
    pub id: String,
    /// Manifest with every file's size and SHA-256.
    pub manifest: Manifest,
}

/// Body of a direct debit.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SpendInput {
    /// Credits (at least 1). All or nothing.
    pub amount: i64,
    /// External reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_ref: Option<String>,
    /// Feature paid for (statistics).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feature: Option<String>,
    /// Idempotency key (unique per subject).
    pub idempotency_key: String,
    /// Caller data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    /// Why.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Subject (default: the caller's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// Units paid for (statistics).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub units: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SqlBatchBody {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_as: Option<RunAsBody>,
    /// Return secret columns unmasked for the whole batch and allow
    /// statements that name them; audited once per batch. Default `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_secrets: Option<bool>,
    /// 1 to 1000 statements, run in one all-or-nothing transaction.
    #[serde(default)]
    pub statements: Vec<SqlStatementBody>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SqlBatchResult {
    #[serde(default)]
    pub items: Vec<SqlResultSchema>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SqlResultSchema {
    #[serde(default)]
    pub columns: Vec<String>,
    pub duration_ms: f64,
    /// `read`, `write`, `schema` or `maintenance`.
    pub kind: String,
    /// Rowid of the last insert (SQLite; `null` on PostgreSQL: use `RETURNING`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_insert_id: Option<i64>,
    #[serde(default)]
    pub rows: Vec<Vec<serde_json::Value>>,
    pub rows_affected: i64,
    /// `true` when secret values in `rows` read `"********"` (send
    /// `show_secrets` to see them); absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secrets_masked: Option<bool>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SqlStatementBody {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Vec<serde_json::Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_as: Option<RunAsBody>,
    /// `POST /sql` only (a batch sets it on the batch body): return secret
    /// columns (`password`, `token_key`, password fields) unmasked and allow
    /// statements that name them. Each use is audited (`sql.show_secrets`
    /// in `GET /api/auth/audit`). Default `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_secrets: Option<bool>,
    /// One SQL statement; positional parameters `?` / `?1`.
    pub sql: String,
}

/// Where a new subscription starts reading.
pub type StartPosition = serde_json::Value;

/// One statement shape with its history.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct StatementDetail {
    /// The subsystem the statement belongs to.
    pub area: String,
    /// `sqlite` or `postgres`.
    pub backend: String,
    /// Times it ran.
    pub calls: i64,
    /// `errors / calls`.
    pub error_rate: f64,
    /// Times it failed.
    pub errors: i64,
    /// 16 hex digits identifying the statement shape.
    pub fingerprint: String,
    /// Longest run in milliseconds.
    pub max_ms: f64,
    /// Mean time in milliseconds.
    pub mean_ms: f64,
    /// Shortest run in milliseconds.
    pub min_ms: f64,
    /// Median in milliseconds (from the latency sketch: within about 11 %).
    pub p50_ms: f64,
    /// 95th percentile in milliseconds.
    pub p95_ms: f64,
    /// 99th percentile in milliseconds.
    pub p99_ms: f64,
    /// Rows returned or changed, total.
    pub rows: i64,
    /// Runs at or above the slow threshold.
    pub slow: i64,
    /// The statement with every value replaced by `?`.
    pub statement: String,
    /// Total wall time in milliseconds.
    pub total_ms: f64,
    /// Runs inside a write transaction.
    pub writes: i64,
    /// Buckets, oldest first (the not yet flushed part of the last bucket is
    /// the final point).
    #[serde(default)]
    pub series: Vec<StatementPoint>,
}

/// A page of \[`StatementStat`\].
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct StatementList {
    /// Start of the range (RFC 3339).
    pub from: String,
    /// The statements.
    #[serde(default)]
    pub items: Vec<StatementStat>,
    /// Page size used.
    pub limit: i64,
    /// Rows skipped.
    pub offset: i64,
    /// End of the range, exclusive (RFC 3339).
    pub to: String,
    /// How many statement shapes matched.
    pub total: i64,
}

/// One time bucket of a statement.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct StatementPoint {
    /// Start of the bucket (RFC 3339).
    pub at: String,
    /// Runs.
    pub calls: i64,
    /// Failures.
    pub errors: i64,
    /// Longest in milliseconds.
    pub max_ms: f64,
    /// Mean in milliseconds.
    pub mean_ms: f64,
    /// 95th percentile in milliseconds.
    pub p95_ms: f64,
    /// Length of the bucket in seconds.
    pub seconds: i64,
}

/// Statistics of one statement fingerprint over a time range.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct StatementStat {
    /// The subsystem the statement belongs to.
    pub area: String,
    /// `sqlite` or `postgres`.
    pub backend: String,
    /// Times it ran.
    pub calls: i64,
    /// `errors / calls`.
    pub error_rate: f64,
    /// Times it failed.
    pub errors: i64,
    /// 16 hex digits identifying the statement shape.
    pub fingerprint: String,
    /// Longest run in milliseconds.
    pub max_ms: f64,
    /// Mean time in milliseconds.
    pub mean_ms: f64,
    /// Shortest run in milliseconds.
    pub min_ms: f64,
    /// Median in milliseconds (from the latency sketch: within about 11 %).
    pub p50_ms: f64,
    /// 95th percentile in milliseconds.
    pub p95_ms: f64,
    /// 99th percentile in milliseconds.
    pub p99_ms: f64,
    /// Rows returned or changed, total.
    pub rows: i64,
    /// Runs at or above the slow threshold.
    pub slow: i64,
    /// The statement with every value replaced by `?`.
    pub statement: String,
    /// Total wall time in milliseconds.
    pub total_ms: f64,
    /// Runs inside a write transaction.
    pub writes: i64,
}

/// State of the statement statistics.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct StatementStatus {
    /// Statistics are collected.
    pub enabled: bool,
    /// Fingerprints held in memory now.
    pub fingerprints_in_memory: i64,
    /// Seconds between flushes.
    pub flush_interval_secs: i64,
    /// Days kept.
    pub retention_days: i64,
    /// Slow statement threshold in milliseconds.
    pub slow_query_ms: i64,
    /// Fingerprints stored.
    pub stored_rows: i64,
    /// Slow statements stored.
    pub stored_slow_statements: i64,
    /// Fingerprints per flush.
    pub top_n: i64,
    /// Start of the interval not yet flushed (RFC 3339).
    pub unflushed_since: String,
}

/// Instance totals.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct StorageStats {
    /// Buckets.
    pub buckets: i64,
    /// Logical bytes of current objects.
    pub logical_bytes: i64,
    /// Current (non-deleted) objects.
    pub objects: i64,
    /// Open upload sessions.
    pub open_uploads: i64,
    /// What the figures cover (FB-140): `tenant`, `instance` (buckets
    /// without a tenant) or `engine` (everything; privileged callers).
    pub scope: String,
    /// Physical bytes stored (after deduplication).
    pub stored_bytes: i64,
    /// The tenant of `scope: tenant`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    /// Object versions, including deleted and older ones.
    pub versions: i64,
}

/// Body of `POST /billing/store/{provider}/checkout` (Creem).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct StoreCheckoutInput {
    /// Sent to the store as the request id.
    pub idempotency_key: String,
    /// Price key mapped to a store product.
    pub price: String,
    /// Subject (default: the caller's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// Where the store returns to (`?checkout_id=` is appended by the store).
    pub success_url: String,
}

/// Body of `PUT /billing/store-products/{provider}/{product_id}`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct StoreProductInput {
    /// Price key: recurring for a store subscription, one-time otherwise.
    pub price: String,
}

/// Body of `POST /billing/store/{provider}/verify`: what the app received
/// from the store. Each store reads its own fields.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct StoreVerifyInput {
    /// Creem: the checkout id of the success redirect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkout_id: Option<String>,
    /// Google Play: `subscription` (default) or `one_time`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Google Play: the product id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_id: Option<String>,
    /// Google Play: the purchase token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purchase_token: Option<String>,
    /// App Store: the StoreKit 2 signed transaction (JWS).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signed_transaction: Option<String>,
    /// Subject the purchase is for (default: the caller's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// App Store: the transaction id (read back with the App Store Server API).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transaction_id: Option<String>,
}

/// How a search was answered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Strategy {
    #[serde(rename = "ann")]
    Ann,
    #[serde(rename = "filtered_ann")]
    FilteredAnn,
    #[serde(rename = "post_filter")]
    PostFilter,
    #[serde(rename = "exact")]
    Exact,
    #[serde(rename = "exact_fallback")]
    ExactFallback,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// Body of `POST .../streams/{key}/add`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct StreamAddBody {
    /// Field names and values (sent in name order).
    #[serde(default)]
    pub fields: std::collections::HashMap<String, Bin>,
    /// Entry id (`ms-seq`); generated when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Keep at most this many entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maxlen: Option<i64>,
}

/// A client frame of the WebSocket stream (JSON text).
pub type StreamClientMessage = serde_json::Value;

/// Body of `POST .../streams/{key}/delete` (`XDEL`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct StreamDeleteBody {
    /// Entry ids (`<ms>-<seq>`).
    #[serde(default)]
    pub ids: Vec<String>,
}

/// A server frame of the WebSocket stream (JSON text).
pub type StreamServerMessage = serde_json::Value;

/// Body of `PUT /entitlements/addons`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SubjectAddon {
    /// Add-on key.
    pub addon: String,
    /// Quantity (0 removes it).
    pub quantity: i64,
    /// Subject.
    pub subject: String,
}

/// Body of `POST /billing/subscriptions`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SubscribeInput {
    /// Coupon code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coupon: Option<String>,
    /// A replay with the same key returns the first subscription.
    pub idempotency_key: String,
    /// Prices (one recurring plan price, add-ons, metered prices).
    #[serde(default)]
    pub items: Vec<ItemInput>,
    /// Caller data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    /// Where the hosted payment page returns to (no saved payment method).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub return_url: Option<String>,
    /// Subject (default: the caller's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// Trial days (default: the plan price's `trial_days`; 0 = none).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trial_days: Option<i64>,
}

/// What a subscription listens to.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SubscribeSpec {
    /// Custom channel name (instead of `collection`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// Collection name: changes of its records (with `record`: of one record).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,
    /// Replay every event after this cursor before live events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<i64>,
    /// Filter-language expression the changed record must match
    /// (collection subscriptions only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    /// Client-chosen subscription id (1-64 characters of `[A-Za-z0-9_.:-]`),
    /// echoed in every message of this subscription.
    pub id: String,
    /// Values for `{:name}` parameters in `filter`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<std::collections::HashMap<String, ValueModel>>,
    /// Record id: only changes of this record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record: Option<String>,
}

/// A subject's subscription.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Subscription {
    /// Add-ons.
    #[serde(default)]
    pub addons: Vec<AddonQuantity>,
    /// Scheduled cancellation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel_at: Option<String>,
    /// Current billing period end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_period_end: Option<String>,
    /// Current billing period.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_period_start: Option<String>,
    /// Monthly quota windows start on this day and time of month.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cycle_anchor: Option<String>,
    /// Provider reference (FB-70).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_ref: Option<String>,
    /// Plan key.
    pub plan: String,
    /// Lifecycle state.
    pub status: SubscriptionStatus,
    /// `org:<id>` or a principal key.
    pub subject: String,
    /// End of the trial.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trial_ends_at: Option<String>,
    /// Last change.
    pub updated_at: String,
}

/// One price on a subscription.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SubscriptionItem {
    /// Id.
    pub id: String,
    /// Price key.
    pub price: String,
    /// Product key.
    pub product: String,
    /// Units (seats).
    pub quantity: i64,
}

/// Subscriptions.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SubscriptionList {
    /// By name.
    #[serde(default)]
    pub items: Vec<QueueSubscription>,
}

/// Metrics of a subscription.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SubscriptionMetrics {
    /// Average ack rate over the last 5 minutes, per second.
    pub ack_rate_per_sec: f64,
    /// Acknowledgements in the last minute.
    pub acked_last_minute: i64,
    /// Acknowledgements.
    pub acked_total: i64,
    /// Bytes of the backlog.
    pub backlog_bytes: i64,
    /// Messages not yet acknowledged (ready, delayed, in flight, and not yet read from the topic).
    pub backlog_messages: i64,
    /// Messages dead-lettered.
    pub dead_lettered_total: i64,
    /// Dead-letter records kept.
    pub dead_letters: i64,
    /// Waiting for a retry backoff.
    pub delayed: i64,
    /// Deliveries ever made.
    pub delivered_total: i64,
    /// Missed ack deadlines.
    pub expired_total: i64,
    /// Handed out and not yet acknowledged.
    pub in_flight: i64,
    /// Negative acknowledgements.
    pub nacked_total: i64,
    /// Age of the oldest unacknowledged message in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oldest_unacked_age_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push: Option<PushHealth>,
    /// Deliverable now.
    pub ready: i64,
    /// Subscription.
    pub subscription: String,
    /// Topic.
    pub topic: String,
}

/// Body of a subscription change; absent fields keep their value.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SubscriptionPatch {
    /// New ack deadline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ack_deadline_ms: Option<i64>,
    /// New backoff base.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backoff_base_ms: Option<i64>,
    /// New backoff cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backoff_max_ms: Option<i64>,
    /// Removes the dead-letter topic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clear_dead_letter_topic: Option<bool>,
    /// Turns push off (the subscription becomes a pull subscription).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clear_push: Option<bool>,
    /// New dead-letter topic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dead_letter_topic: Option<String>,
    /// Replaces the labels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labels: Option<std::collections::HashMap<String, String>>,
    /// New attempt limit; `0` removes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_delivery_attempts: Option<i64>,
    /// New auto-pause threshold; `0` turns auto-pause off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_after_failures: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push: Option<PushConfig>,
    /// Delete the subscription this long from now; `0` removes the expiry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_ms: Option<i64>,
}

/// Subscription lifecycle (set by billing, FB-70).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubscriptionStatus {
    #[serde(rename = "active")]
    Active,
    #[serde(rename = "trialing")]
    Trialing,
    #[serde(rename = "past_due")]
    PastDue,
    #[serde(rename = "paused")]
    Paused,
    #[serde(rename = "canceled")]
    Canceled,
    #[serde(rename = "expired")]
    Expired,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// The billing sync seam's input: the full state of a subject's subscription
/// (FB-70 calls \[`crate::BillingSync::sync_subscription`\]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubscriptionSync {
    /// Add-ons (`null`: unchanged; a list replaces the set).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addons: Option<Vec<AddonQuantity>>,
    /// Scheduled cancellation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel_at: Option<String>,
    /// Current billing period end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_period_end: Option<String>,
    /// Current billing period start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_period_start: Option<String>,
    /// Cycle anchor of monthly windows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cycle_anchor: Option<String>,
    /// Provider event time: an event older than the last applied one is
    /// ignored (out-of-order webhooks).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_at: Option<String>,
    /// Provider event id: a replay of an applied event changes nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    /// Provider reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_ref: Option<String>,
    /// Plan key.
    pub plan: String,
    /// Lifecycle state.
    pub status: SubscriptionStatus,
    /// `org:<id>` or a principal key.
    pub subject: String,
    /// End of the trial.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trial_ends_at: Option<String>,
}

/// Result of a billing sync.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyncOutcome {
    /// Plan after the sync.
    pub plan: String,
    /// What happened: `applied`, `replayed` (same event id) or `stale`
    /// (older than the last applied event).
    pub result: String,
    /// Status after the sync.
    pub status: SubscriptionStatus,
    /// Subject.
    pub subject: String,
}

/// `PATCH /api/system/config` body.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SystemConfigPatch {
    /// Dotted keys from the registry and their new values.
    #[serde(default)]
    pub set: serde_json::Value,
}

/// `PATCH /api/system/config` answer.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SystemConfigPatched {
    /// Keys that took effect without a restart (none in this version).
    #[serde(default)]
    pub applied: Vec<String>,
    /// The registry after the change.
    #[serde(default)]
    pub features: Vec<SystemFeature>,
    /// The change needs a restart.
    pub restart_required: bool,
    /// The file that was written.
    pub written: String,
}

/// One module.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SystemFeature {
    /// No switch: the module is part of every server.
    pub always_on: bool,
    /// `[<section>] enabled` switches it.
    pub can_disable: bool,
    /// Documentation, relative to the repository root, with a heading
    /// anchor (`docs/adr/0018-object-storage.md#adr-0018-object-storage`).
    pub docs: String,
    /// Whether it is on (after a restart, when `restart_pending`).
    pub enabled: bool,
    /// Stable id (`storage`).
    pub id: String,
    /// Display name.
    pub name: String,
    /// The switch first (when `can_disable`), then the module's settings.
    #[serde(default)]
    pub settings: Vec<SystemSetting>,
    /// Dashboard route of its studio.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub studio_path: Option<String>,
    /// One line on what it does.
    pub summary: String,
}

/// `GET /api/system/features`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SystemFeatures {
    /// Set when the config file cannot be read; values are then the running ones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_error: Option<String>,
    /// Absolute path of the config file (`PATCH` creates it when missing).
    pub config_file: String,
    /// Every module, in display order.
    #[serde(default)]
    pub features: Vec<SystemFeature>,
    /// The file differs from what the server runs: restart to apply it.
    pub restart_pending: bool,
}

/// One setting.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SystemSetting {
    /// Built-in default; `null` for a secret.
    #[serde(default)]
    pub default: serde_json::Value,
    /// Whether `PATCH /api/system/config` may change it (never for `env`,
    /// `flag` or secrets).
    pub editable: bool,
    /// Environment variable that sets it (`FORGEBASE_PORT`), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
    /// Command-line flag of `forgebase serve` that sets it (`--port`), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flag: Option<String>,
    /// What the setting changes and a sensible value, in plain words;
    /// shown as-is.
    pub help: String,
    /// Dotted key (`storage.max_object_bytes`), as in `forgebase.toml`.
    pub key: String,
    /// Short label, sentence case; shown as-is.
    pub label: String,
    /// Allowed values of an `enum`; `null` otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
    /// A change takes effect after a restart.
    pub restart_required: bool,
    /// Never listed with a value nor written by the API.
    pub secret: bool,
    /// `default`, `file`, `env` or `flag`.
    pub source: String,
    /// `bool`, `int`, `string`, `enum`, `duration`, `list` or `url`.
    pub r#type: String,
    /// Unit of an integer: `bytes`, `seconds`, `ms`, `days`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// Current value; `null` for a secret.
    #[serde(default)]
    pub value: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TableInfoSchema {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,
    #[serde(default)]
    pub columns: Vec<ColumnInfoSchema>,
    #[serde(default)]
    pub indexes: Vec<String>,
    /// `table` or `view`.
    pub kind: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sql: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TableList {
    #[serde(default)]
    pub items: Vec<TableInfoSchema>,
}

/// Whether prices include tax.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaxBehavior {
    #[serde(rename = "exclusive")]
    Exclusive,
    #[serde(rename = "inclusive")]
    Inclusive,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// A tax rate of a region.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TaxRate {
    /// In use.
    pub active: bool,
    /// Id.
    pub id: String,
    /// Name on invoices (`GST`, `VAT`).
    pub name: String,
    /// Rate in basis points (1800 = 18 %).
    pub rate_bp: i64,
    /// Region (`IN`, `DE`, `US-CA`).
    pub region: String,
}

/// Body of `PUT /billing/tax-rates/{region}`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TaxRateInput {
    /// In use (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
    /// Name on invoices.
    pub name: String,
    /// Basis points (0..=10000).
    pub rate_bp: i64,
}

/// Saves a template.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TemplateInput {
    /// HTML body template (max 128 KiB).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub html: Option<String>,
    /// Subject template (max 300).
    pub subject: String,
    /// Plain-text body template (max 64 KiB).
    pub text: String,
}

/// Body of `POST /email/provider/test`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TestMailInput {
    /// Recipient of the test message.
    pub to: String,
}

/// Result of a test send.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TestMailResult {
    /// The `From` address used.
    pub from: String,
    /// The provider's message id, when it returned one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    /// Transport that handled it (`resend`, `ses`, ...).
    pub provider: String,
}

/// Body of a test send (optional).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TestSendRequest {
    /// `data` of the synthetic message (default: a short `forgebase.test` object).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// How the words of a keyword query combine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextOperator {
    #[serde(rename = "or")]
    Or,
    #[serde(rename = "and")]
    And,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ThresholdBody {
    /// Statements slower than this are logged; `0` logs every statement.
    pub threshold_ms: i64,
}

/// One pricing tier: units up to `up_to` (inclusive; `None`: no upper
/// bound) cost `unit_amount` each plus `flat_amount` once.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Tier {
    /// Fixed fee when the tier is reached, minor units.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flat_amount: Option<i64>,
    /// Price per unit, minor units.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit_amount: Option<i64>,
    /// Last unit of the tier (`null`: the rest).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub up_to: Option<i64>,
}

/// A topic.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Topic {
    /// Created (RFC 3339).
    pub created_at: String,
    /// Creator.
    pub created_by: String,
    /// Dedup window in milliseconds.
    pub dedup_window_ms: i64,
    /// What the topic's events are.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Example of message `data`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub example: Option<serde_json::Value>,
    /// Labels.
    #[serde(default)]
    pub labels: std::collections::HashMap<String, String>,
    /// Largest message in bytes.
    pub max_message_bytes: i64,
    /// Name.
    pub name: String,
    /// Bytes ever published.
    pub published_bytes: i64,
    /// Messages ever published.
    pub published_total: i64,
    /// Size limit in bytes (`None`: no limit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention_bytes: Option<i64>,
    /// Retention in milliseconds.
    pub retention_ms: i64,
    /// JSON Schema of message data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<RecordSource>,
    /// Owning tenant (FB-141); `None` for an instance-wide topic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    /// Last changed (RFC 3339).
    pub updated_at: String,
}

/// Topics.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TopicList {
    /// By name.
    #[serde(default)]
    pub items: Vec<Topic>,
}

/// Metrics of a topic.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TopicMetrics {
    /// Retained bytes.
    pub bytes: i64,
    /// Retained messages.
    pub messages: i64,
    /// Age of the oldest retained message in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oldest_message_age_ms: Option<i64>,
    /// Average publish rate over the last 5 minutes, per second.
    pub publish_rate_per_sec: f64,
    /// Bytes ever published.
    pub published_bytes: i64,
    /// Messages published in the last minute.
    pub published_last_minute: i64,
    /// Messages ever published.
    pub published_total: i64,
    /// Subscriptions of the topic.
    pub subscriptions: i64,
    /// Topic.
    pub topic: String,
}

/// Body of a topic change; absent fields keep their value.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TopicPatch {
    /// Removes the example.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clear_example: Option<bool>,
    /// Removes the schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clear_schema: Option<bool>,
    /// New dedup window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedup_window_ms: Option<i64>,
    /// New description; an empty string removes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// New example of message `data`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub example: Option<serde_json::Value>,
    /// Replaces the labels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labels: Option<std::collections::HashMap<String, String>>,
    /// New message size limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_message_bytes: Option<i64>,
    /// New size limit; `0` removes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention_bytes: Option<i64>,
    /// New retention.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention_ms: Option<i64>,
    /// New schema (applies to later publishes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<serde_json::Value>,
}

/// `GET /api/system/database/topology`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TopologyView {
    /// `postgres` or `sqlite`.
    pub backend: String,
    /// PostgreSQL version of the control primary, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_version: Option<String>,
    /// Topology epoch (grows on every change).
    pub epoch: i64,
    /// Every node that is not retired.
    #[serde(default)]
    pub nodes: Vec<NodeInfo>,
    /// Operations queued or running.
    #[serde(default)]
    pub operations: Vec<Operation>,
    /// Set after a promote of control's primary: the server must be
    /// restarted to move its connections to the new primary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart_required: Option<String>,
    /// Control first, then shards by number.
    #[serde(default)]
    pub sets: Vec<SetInfo>,
}

/// Body of `POST /traverse`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TraverseQuery {
    /// Direction to follow (default `out`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<Direction>,
    /// Edge types to follow; empty or absent = all (at most 20).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge_types: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<NodeFilter>,
    /// Return the edges between the returned nodes (and the start node).
    /// Default `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_edges: Option<bool>,
    /// Return the start node too (depth 0). Default `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_start: Option<bool>,
    /// Most nodes returned, 1 to 1000 (default 100).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    /// Hops, 1 to 10 (default 3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<i64>,
    /// Id of the start node.
    pub start: String,
}

/// Result of a traversal.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TraverseResult {
    /// Edges between the returned nodes (and the start node).
    #[serde(default)]
    pub edges: Vec<Edge>,
    /// Reached nodes, nearest first, then by id.
    #[serde(default)]
    pub nodes: Vec<TraversedNode>,
    /// Start node id.
    pub start: String,
    /// More nodes or edges matched than were returned.
    pub truncated: bool,
}

/// A node reached by a traversal.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TraversedNode {
    /// Fewest hops from the start node.
    pub depth: i64,
    /// The node.
    pub node: Node,
}

/// Body of `POST /entitlements/trials`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TrialInput {
    /// Trial length (default: the plan's `trial_days`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub days: Option<i64>,
    /// Plan key.
    pub plan: String,
    /// Subject.
    pub subject: String,
}

/// Body of `PUT .../keys/{key}/ttl`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TtlBody {
    /// Milliseconds from now until the key expires (positive).
    pub ttl_ms: i64,
}

/// Edges of one type.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TypeCount {
    /// Number of edges.
    pub count: i64,
    /// Relationship type.
    pub r#type: String,
}

/// Body of `PATCH /api/system/database/nodes/{id}`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct UpdateNode {
    /// Share of replica reads, 0 to 1000 (0 takes it out of rotation).
    pub weight: i64,
}

/// Body of `PATCH /api/system/database/shards/{id}`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct UpdateShard {
    /// Takes new tenants.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hosts_tenants: Option<bool>,
    /// `active` or `draining` (takes no new tenants).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// Relative capacity, 1 to 1000.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<i64>,
}

/// A multipart or resumable upload session.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Upload {
    /// Bucket.
    pub bucket: String,
    /// Content type of the finished object.
    pub content_type: String,
    /// Declared SHA-256 of the whole object.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_sha256: Option<String>,
    /// Declared total size (resumable uploads complete when it is reached).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_size: Option<i64>,
    /// RFC 3339 expiry.
    pub expires_at: String,
    /// Session id.
    pub id: String,
    /// Target key.
    pub key: String,
    /// `multipart` (numbered parts, any order) or `resumable` (sequential
    /// offsets).
    pub kind: String,
    /// Received parts.
    #[serde(default)]
    pub parts: Vec<PartInfo>,
    /// Bytes received so far.
    pub received_bytes: i64,
}

/// A new upload session.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct UploadInput {
    /// SHA-256 of the whole object, verified on completion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checksum_sha256: Option<String>,
    /// Content type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    /// Target key.
    pub key: String,
    /// `multipart` (default) or `resumable`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// User metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<std::collections::HashMap<String, String>>,
    /// Total size, when known (required to auto-complete a resumable upload).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,
}

/// One item to insert or replace.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct UpsertItem {
    /// 1 to 64 of `[A-Za-z0-9_-]`; generated when absent. An existing item
    /// with this id is replaced completely.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// JSON object, filterable with the filter language (`category = 'news'`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    /// Text for keyword search (the full-text side of hybrid search).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Vector per field name. Fields left out have no vector for this item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vectors: Option<std::collections::HashMap<String, Vec<f64>>>,
}

/// Body of an upsert.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct UpsertRequest {
    /// 1 to 1000 items, written in one transaction.
    #[serde(default)]
    pub items: Vec<UpsertItem>,
}

/// Outcome of an upsert.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct UpsertResult {
    /// Items that did not exist before.
    pub created: i64,
    /// Ids in request order.
    #[serde(default)]
    pub ids: Vec<String>,
    /// Items replaced.
    pub updated: i64,
}

/// A usage alert.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct UsageAlert {
    /// Raised.
    pub at: String,
    /// Delivered by the alert job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_at: Option<String>,
    /// Feature.
    pub feature: String,
    /// Id.
    pub id: String,
    /// Allowance when raised.
    pub limit: i64,
    /// Counter scope key.
    pub scope_key: String,
    /// Subject.
    pub subject: String,
    /// 70, 90 or 100.
    pub threshold: i64,
    /// Usage when raised.
    pub used: i64,
    /// Window.
    pub window_start: String,
}

/// One FB-24 usage event charged in credits.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct UsageCharge {
    /// When it was charged.
    pub at: String,
    /// Credits debited.
    pub charged: i64,
    /// Credits the usage cost.
    pub cost: i64,
    /// The consumption entry (`null` when nothing could be charged).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_id: Option<String>,
    /// When the usage happened.
    pub event_at: String,
    /// Usage event id.
    pub event_id: String,
    /// Usage event `seq`.
    pub event_seq: i64,
    /// Feature.
    pub feature: String,
    /// Units the rate is for.
    pub per_units: i64,
    /// Credits per `per_units` units applied.
    pub rate: i64,
    /// Credits the balance could not cover (FB-70 may bill them).
    pub shortfall: i64,
    /// Subject.
    pub subject: String,
    /// Units priced.
    pub units: i64,
}

/// A page of usage charges.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct UsageChargePage {
    /// Oldest first.
    #[serde(default)]
    pub items: Vec<UsageCharge>,
    /// Pass as `after` for the next page (`null`: no more).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<i64>,
}

/// An immutable usage record (FB-70 bills from these).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct UsageEvent {
    /// Who recorded it.
    pub actor: String,
    /// When.
    pub at: String,
    /// Feature.
    pub feature: String,
    /// Id.
    pub id: String,
    /// Idempotency key.
    pub idempotency_key: String,
    /// Caller data.
    #[serde(default)]
    pub metadata: serde_json::Value,
    /// Of which past a soft limit (billable overage).
    pub overage_units: i64,
    /// Of which from usage packs.
    pub pack_units: i64,
    /// Request id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// Reservation committed, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reservation_id: Option<String>,
    /// Counter scope key.
    pub scope_key: String,
    /// Cursor (increasing).
    pub seq: i64,
    /// `consume`, `reservation` or `gauge`.
    pub source: String,
    /// Subject billed.
    pub subject: String,
    /// Units (in-window plus pack units; gauges: the change).
    pub units: i64,
    /// Window the usage counts in.
    pub window_start: String,
}

/// A page of usage events.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct UsageEventPage {
    /// Oldest first.
    #[serde(default)]
    pub items: Vec<UsageEvent>,
    /// Pass as `after` for the next page (`null`: no more).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<i64>,
}

/// The counter of one feature of one subject in its current window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageMeter {
    /// Rolled over from the previous window.
    pub carried_in: i64,
    /// Feature key.
    pub feature: String,
    /// Limit of the window (`null`: unlimited).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    /// `used` as a percentage of `limit`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percent: Option<f64>,
    /// Held by reservations.
    pub reserved: i64,
    /// Counter scope (`""` for the subject itself).
    pub scope_key: String,
    /// 70 / 90 / 100 % state.
    pub state: MeterState,
    /// Subject (`org:<id>` or a principal key).
    pub subject: String,
    /// Last change.
    pub updated_at: String,
    /// Committed usage.
    pub used: i64,
    /// Window end.
    pub window_end: String,
    /// Window start.
    pub window_start: String,
}

/// A page of usage meters.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct UsageMeterPage {
    /// Meters, most used first.
    #[serde(default)]
    pub items: Vec<UsageMeter>,
    /// Page size.
    pub limit: i64,
    /// Rows skipped.
    pub offset: i64,
    /// All meters matching the filters.
    pub total: i64,
}

/// A usage pack of the catalog.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct UsagePack {
    /// Can be granted.
    pub active: bool,
    /// Metered feature it adds units to.
    pub feature: String,
    /// Key.
    pub key: String,
    /// Display name.
    pub name: String,
    /// Units per pack.
    pub units: i64,
    /// Validity after purchase (`null`: no expiry).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_days: Option<i64>,
}

/// Body of `PUT /usage-packs/{key}`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct UsagePackInput {
    /// Can be granted (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
    /// Metered feature.
    pub feature: String,
    /// Display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Units per pack.
    pub units: i64,
    /// Validity in days.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_days: Option<i64>,
}

/// A page of usage rows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageReport {
    /// Whether the billing catalog was available for prices.
    pub cost_available: bool,
    /// Currency of the cost estimates, when there are any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    /// Always true.
    pub estimate: bool,
    /// Start of the period (RFC 3339).
    pub from: String,
    /// Grouping level asked for.
    pub group_by: Level,
    /// The rows.
    #[serde(default)]
    pub items: Vec<UsageRow>,
    /// Page size.
    pub limit: i64,
    /// Rows skipped.
    pub offset: i64,
    /// Sort key used.
    pub sort: String,
    /// End of the period, exclusive.
    pub to: String,
    /// Rows matching before paging.
    pub total: i64,
}

/// A quota check, consumption or reservation.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct UsageRequest {
    /// Feature key.
    pub feature: String,
    /// Required for consume and reserve: a replay returns the first result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    /// Stored with the usage event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    /// Counter of a `user`, `team` or `resource` quota (default for `user`:
    /// the caller).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_key: Option<String>,
    /// Subject billed (default: the caller's subject; another subject needs
    /// privileges or membership).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// Reservations: hold time in seconds (default 300, at most 86400).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_secs: Option<i64>,
    /// Units (default 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub units: Option<i64>,
}

/// Usage of one tenant (or tenant group).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct UsageRow {
    /// False for a metering row listed among environments: the events belong
    /// to an organization or principal, not to one environment.
    pub attributed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<CostEstimate>,
    /// Id at that level (`shared` for data that belongs to no tenant).
    pub id: String,
    /// The level of this row (`environment`, `project`, `org`, `user`, `realm`, `shared`).
    pub level: String,
    /// The metrics.
    #[serde(default)]
    pub metrics: std::collections::HashMap<String, MetricValue>,
    /// Display name.
    pub name: String,
    /// Organization id, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org: Option<String>,
    /// Project id, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// Realm id, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub realm: Option<String>,
}

/// What one run of the usage consumer did.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct UsageRun {
    /// Credits debited.
    pub credits_charged: i64,
    /// Cursor after the run (last usage event `seq` read).
    pub cursor: i64,
    /// Events charged (priced and not charged before).
    pub events_charged: i64,
    /// Usage events read.
    pub events_read: i64,
    /// Credits not covered by balances.
    pub shortfall: i64,
}

/// Daily values of one scope.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct UsageSeries {
    /// Per metric, `[day, value]` pairs, oldest first.
    #[serde(default)]
    pub metrics: std::collections::HashMap<String, Vec<Vec<serde_json::Value>>>,
    /// The scope (`env:<id>`).
    pub scope: String,
}

/// Current usage of one feature of a subject.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageSummary {
    /// Rolled over from the previous window.
    pub carried_in: i64,
    /// Included.
    pub enabled: bool,
    /// Hard or soft.
    pub enforcement: Enforcement,
    /// Feature.
    pub feature: String,
    /// Kind.
    pub kind: FeatureKind,
    /// Allowance (limit plus rolled-over units; `null`: unlimited).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    /// Usage-pack units left.
    pub pack_remaining: i64,
    /// `used` as a percentage of `limit`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percent: Option<f64>,
    /// Window.
    pub period: Period,
    /// Left including usage packs (`null`: unlimited).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining: Option<i64>,
    /// Held by reservations.
    pub reserved: i64,
    /// Counter scope key.
    pub scope_key: String,
    /// Committed usage (gauge level, active leases).
    pub used: i64,
    /// Window end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_end: Option<String>,
    /// Window start.
    pub window_start: String,
}

pub type ValueModel = serde_json::Value;

/// A vector collection.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct VectorCollection {
    /// Creation time.
    pub created: String,
    /// Free text.
    pub description: String,
    /// See \[`NewCollection::exact_threshold`\].
    pub exact_threshold: i64,
    /// Vector fields.
    #[serde(default)]
    pub fields: Vec<VectorField>,
    /// Number of items (that the caller's rule shows).
    pub items: i64,
    /// Name.
    pub name: String,
    /// See \[`NewCollection::source_collection`\].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_collection: Option<String>,
    /// Last change of the definition or the data.
    pub updated: String,
}

/// Collections of the caller's tenant.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct VectorCollectionList {
    /// By name.
    #[serde(default)]
    pub items: Vec<VectorCollection>,
}

/// Outcome of a delete.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct VectorDeleteResult {
    /// Items removed.
    pub deleted: i64,
}

/// A vector field of a collection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorField {
    /// HNSW links per node.
    pub connectivity: i64,
    /// Creation time.
    pub created: String,
    /// Number of components.
    pub dimension: i64,
    /// HNSW candidate list size while inserting.
    pub expansion_add: i64,
    /// HNSW candidate list size while searching.
    pub expansion_search: i64,
    /// Distance function.
    pub metric: Metric,
    /// Name.
    pub name: String,
    /// Element type of the ANN index.
    pub quantization: Quantization,
}

/// A new collection.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct VectorNewCollection {
    /// Free text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Searches over at most this many candidate vectors are exact (brute
    /// force) instead of approximate. Default from the server configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exact_threshold: Option<i64>,
    /// Vector fields (at most 16; more can be added later).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fields: Option<Vec<FieldSpec>>,
    /// `[A-Za-z][A-Za-z0-9_]{0,62}`, unique in the tenant.
    pub name: String,
    /// The data collection the items are copied from, item id = record id
    /// (FB-139). Searches and reads then re-check every item against that
    /// collection's View rule as the caller, leave out the metadata keys the
    /// caller may not read there, and refuse filters on them. Set at
    /// creation only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_collection: Option<String>,
}

/// Progress of a rebuild.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct VectorProgress {
    /// Vectors added so far.
    pub done: i64,
    /// Vectors to add.
    pub total: i64,
}

/// Statistics of the caller's tenant.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct VectorStats {
    /// Indexes currently being rebuilt.
    pub building: i64,
    /// Collections.
    pub collections: i64,
    /// Vectors in loaded indexes.
    pub indexed_vectors: i64,
    /// Vector fields (one ANN index each).
    pub indexes: i64,
    /// Memory used by loaded indexes.
    pub memory_bytes: i64,
}

/// The DNS record that proves ownership.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Verification {
    /// The token stops being accepted at this time (epoch ms).
    pub expires_at: i64,
    /// Record name (`_forgebase-verify.<host>`).
    pub record_name: String,
    /// Always `TXT`.
    pub record_type: String,
    /// Record value (`forgebase-verify=<token>`).
    pub record_value: String,
}

/// Outcome of \[`verify`\].
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct VerifyReport {
    /// Bytes hashed.
    pub bytes_checked: i64,
    /// Files whose size and hash were checked.
    pub files_checked: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest: Option<Manifest>,
    /// No problems found.
    pub ok: bool,
    /// Reasons the backup cannot be restored.
    #[serde(default)]
    pub problems: Vec<String>,
    /// Things worth knowing that do not block a restore.
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// Result of a verification attempt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerifyResult {
    /// The domain after the attempt.
    pub domain: Domain,
    /// Why not (when not verified).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The record was found.
    pub verified: bool,
}

/// Everything about one version except the module itself. Stored as JSON in
/// `_forgebase_function_versions.config`; a rollback restores all of it.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct VersionConfig {
    /// Granted capabilities (see \[`CAPABILITIES`\]).
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Plain (non-secret) environment values, readable with the `env` capability.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<std::collections::HashMap<String, String>>,
    /// With `fetch`: hosts the function may call (`api.example.com`, or
    /// `*.example.com` for subdomains). Empty = any public host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetch_hosts: Option<Vec<String>>,
    /// Record hooks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hooks: Option<Vec<HookSpec>>,
    /// The language it was written in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<Language>,
    /// Limits.
    pub limits: Limits,
    /// How the version runs (`wasm` for versions deployed before FB-93).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<FunctionRuntime>,
}

/// One version of a function. `runtime` and `language` are part of the
/// flattened configuration.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct VersionInfo {
    /// Granted capabilities (see \[`CAPABILITIES`\]).
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Plain (non-secret) environment values, readable with the `env` capability.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<std::collections::HashMap<String, String>>,
    /// With `fetch`: hosts the function may call (`api.example.com`, or
    /// `*.example.com` for subdomains). Empty = any public host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetch_hosts: Option<Vec<String>>,
    /// Record hooks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hooks: Option<Vec<HookSpec>>,
    /// The language it was written in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<Language>,
    /// Limits.
    pub limits: Limits,
    /// How the version runs (`wasm` for versions deployed before FB-93).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<FunctionRuntime>,
    /// Whether this is the active version.
    pub active: bool,
    /// When it was deployed (RFC 3339).
    pub created_at: String,
    /// Who deployed it.
    pub created_by: String,
    /// SHA-256 of the module (WebAssembly) or of the source (JavaScript, TypeScript), hex.
    pub sha256: String,
    /// Size of the module or of the source in bytes.
    pub size: i64,
    /// Version number (1, 2, ...).
    pub version: i64,
}

/// Versions.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct VersionList {
    /// Newest first.
    #[serde(default)]
    pub items: Vec<VersionInfo>,
}

/// The source of a JavaScript or TypeScript version.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VersionSource {
    /// `javascript` or `typescript`.
    pub language: Language,
    /// The source as deployed.
    pub source: String,
}

/// Who may read a bucket without an explicit grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Visibility {
    #[serde(rename = "private")]
    Private,
    #[serde(rename = "authenticated")]
    Authenticated,
    #[serde(rename = "public")]
    Public,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// One inbound webhook request (without headers and body).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WebhookDelivery {
    /// Provider account it was for, when known (Razorpay key id, WeChat Pay
    /// merchant id, App Store bundle id, Google Play package name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// SHA-256 of the whole body, hex (the event's `payload_hash`).
    pub body_sha256: String,
    /// Body size as received, in bytes.
    pub body_size: i64,
    /// The body is stored (unverified deliveries over the budget are not).
    pub body_stored: bool,
    /// Only the first `body_max_bytes` are stored.
    pub body_truncated: bool,
    /// Client address (socket peer, or the trusted `X-Forwarded-For`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_addr: Option<String>,
    /// Processing detail (`invoice_paid`, `stale`, `too_old`, ...) or the error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Event id at the provider (verified deliveries).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    /// Event type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_type: Option<String>,
    /// Id (`whd_…`).
    pub id: String,
    /// Verification and processing time in microseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_us: Option<i64>,
    /// Internal objects it touched.
    pub links: Links,
    /// `processing` when verified, else `verification`.
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub processing: Option<WebhookProcessing>,
    /// Provider named in the path.
    pub provider: String,
    /// Why verification refused it (stable code and a fixed message).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Received.
    pub received_at: String,
    /// Operator replays.
    pub replays: i64,
    /// When a re-verification accepted it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reverified_at: Option<String>,
    /// Operator re-verifications.
    pub reverify_attempts: i64,
    /// Last change.
    pub updated_at: String,
    /// What verification made of it.
    pub verification: WebhookVerification,
}

/// A delivery with its stored headers and body. Bodies can contain personal
/// data (names, emails, addresses): platform administrators only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WebhookDeliveryDetail {
    /// Provider account it was for, when known (Razorpay key id, WeChat Pay
    /// merchant id, App Store bundle id, Google Play package name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// SHA-256 of the whole body, hex (the event's `payload_hash`).
    pub body_sha256: String,
    /// Body size as received, in bytes.
    pub body_size: i64,
    /// The body is stored (unverified deliveries over the budget are not).
    pub body_stored: bool,
    /// Only the first `body_max_bytes` are stored.
    pub body_truncated: bool,
    /// Client address (socket peer, or the trusted `X-Forwarded-For`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_addr: Option<String>,
    /// Processing detail (`invoice_paid`, `stale`, `too_old`, ...) or the error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Event id at the provider (verified deliveries).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    /// Event type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_type: Option<String>,
    /// Id (`whd_…`).
    pub id: String,
    /// Verification and processing time in microseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_us: Option<i64>,
    /// Internal objects it touched.
    pub links: Links,
    /// `processing` when verified, else `verification`.
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub processing: Option<WebhookProcessing>,
    /// Provider named in the path.
    pub provider: String,
    /// Why verification refused it (stable code and a fixed message).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Received.
    pub received_at: String,
    /// Operator replays.
    pub replays: i64,
    /// When a re-verification accepted it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reverified_at: Option<String>,
    /// Operator re-verifications.
    pub reverify_attempts: i64,
    /// Last change.
    pub updated_at: String,
    /// What verification made of it.
    pub verification: WebhookVerification,
    /// The stored body: text when it is UTF-8, else base64.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// `utf8` or `base64`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_encoding: Option<String>,
    /// Allowlisted headers (lower-case names).
    #[serde(default)]
    pub headers: serde_json::Value,
}

/// A page of deliveries.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct WebhookDeliveryPage {
    /// Deliveries, newest first.
    #[serde(default)]
    pub items: Vec<WebhookDelivery>,
    /// Page size.
    pub limit: i64,
    /// Rows skipped.
    pub offset: i64,
    /// All deliveries matching the filters.
    pub total: i64,
}

/// What a replay or re-verification did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WebhookDeliveryResult {
    /// The delivery afterwards.
    pub delivery: WebhookDelivery,
    /// Processing detail, or why it does not verify.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// `applied`, `ignored`, `failed`, `already_applied` (the event was
    /// processed before: nothing changed), `already_ignored` (it was
    /// ignored before: nothing changed) or `unverified` (the stored delivery
    /// does not verify with the current secrets: nothing was processed).
    pub result: String,
}

/// A webhook event received.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct WebhookEvent {
    /// Deliveries seen.
    pub attempts: i64,
    /// Provider time of the event.
    pub event_at: String,
    /// Event id at the provider.
    pub event_id: String,
    /// Event type.
    pub event_type: String,
    /// Id.
    pub id: String,
    /// Provider.
    pub provider: String,
    /// Received.
    pub received_at: String,
    /// What happened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    /// `processed`, `ignored` or `failed`.
    pub status: String,
}

/// What a webhook delivery did.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct WebhookOutcome {
    /// Details (`stale`, `unknown_payment`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Event id.
    pub event_id: String,
    /// `processed`, `ignored`, `duplicate` (already handled) or `failed`.
    pub result: String,
}

/// What processing made of a verified delivery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WebhookProcessing {
    #[serde(rename = "applied")]
    Applied,
    #[serde(rename = "ignored")]
    Ignored,
    #[serde(rename = "duplicate")]
    Duplicate,
    #[serde(rename = "failed")]
    Failed,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// What verification made of a delivery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WebhookVerification {
    #[serde(rename = "verified")]
    Verified,
    #[serde(rename = "signature_invalid")]
    SignatureInvalid,
    #[serde(rename = "timestamp_out_of_tolerance")]
    TimestampOutOfTolerance,
    #[serde(rename = "unknown_provider")]
    UnknownProvider,
    #[serde(rename = "malformed")]
    Malformed,
    #[serde(rename = "rejected")]
    Rejected,
    #[serde(rename = "error")]
    Error,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// Latency numbers of one window of a regression.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct WindowStat {
    /// Runs.
    pub calls: i64,
    /// `errors / calls`.
    pub error_rate: f64,
    /// Failures.
    pub errors: i64,
    /// Mean in milliseconds.
    pub mean_ms: f64,
    /// Median in milliseconds.
    pub p50_ms: f64,
    /// 95th percentile in milliseconds.
    pub p95_ms: f64,
    /// 99th percentile in milliseconds.
    pub p99_ms: f64,
}

/// A worker that registered or claimed jobs.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Worker {
    /// Jobs it runs at once.
    pub concurrency: i64,
    /// Who registered it (FB-146); `null`: registered before FB-146.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
    /// Worker id.
    pub id: String,
    /// Handlers it runs (empty: all).
    #[serde(default)]
    pub kinds: Vec<String>,
    /// Last claim, heartbeat or registration (RFC 3339).
    pub last_seen_at: String,
    /// Display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Seen in the last minute.
    pub online: bool,
    /// Queues it works (empty: all).
    #[serde(default)]
    pub queues: Vec<String>,
    /// First seen (RFC 3339).
    pub registered_at: String,
    /// Tenant of the caller that registered it (FB-146); `null`: none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
}

/// Workers.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct WorkerList {
    /// Most recently seen first.
    #[serde(default)]
    pub items: Vec<Worker>,
}

/// A workflow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Workflow {
    /// Creation time (RFC 3339).
    pub created_at: String,
    /// Who created it.
    pub created_by: String,
    /// Why it failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The job whose failure failed the workflow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_job_id: Option<String>,
    /// End time (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    /// Workflow id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// What a failed step does.
    pub on_failure: FailurePolicy,
    /// State.
    pub status: WorkflowStatus,
    /// Tenant of the creator (FB-142). `null`: none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    /// Last change (RFC 3339).
    pub updated_at: String,
}

/// A workflow with its jobs (the graph) and their timings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowDetail {
    /// Creation time (RFC 3339).
    pub created_at: String,
    /// Who created it.
    pub created_by: String,
    /// Why it failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The job whose failure failed the workflow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_job_id: Option<String>,
    /// End time (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    /// Workflow id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// What a failed step does.
    pub on_failure: FailurePolicy,
    /// State.
    pub status: WorkflowStatus,
    /// Tenant of the creator (FB-142). `null`: none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    /// Last change (RFC 3339).
    pub updated_at: String,
    /// Its jobs in creation order: steps, child jobs and compensations.
    #[serde(default)]
    pub steps: Vec<WorkflowStep>,
}

/// A page of workflows, newest first.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct WorkflowPage {
    /// The workflows.
    #[serde(default)]
    pub items: Vec<Workflow>,
    /// All workflows matching the filter.
    pub total: i64,
}

/// State of a workflow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkflowStatus {
    #[serde(rename = "running")]
    Running,
    #[serde(rename = "succeeded")]
    Succeeded,
    #[serde(rename = "failed")]
    Failed,
    #[serde(rename = "compensating")]
    Compensating,
    #[serde(rename = "compensated")]
    Compensated,
    #[serde(rename = "cancelled")]
    Cancelled,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// A job of a workflow and the jobs it waits for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowStep {
    /// Ids of the jobs it waits for.
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// The job.
    pub job: Job,
    /// Step name (child jobs spawned by a step have none unless given).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `step` or `compensation`.
    pub role: String,
}

/// A live update of a workflow, published on `jobs:<workflow id>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowUpdate {
    /// Always `workflow`.
    pub event: String,
    /// The workflow.
    pub workflow: Workflow,
}

/// Body of `POST .../zsets/{key}/add`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ZAddBody {
    /// Members to add or update.
    #[serde(default)]
    pub members: Vec<ScoredMember>,
}
