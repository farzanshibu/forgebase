//! Request and response types of the queue API, and their validation.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use utoipa::ToSchema;

use crate::error::QueueError;

pub use forgebase_jobs::iso;

/// Attribute keys starting with this prefix are set by the server only.
pub const RESERVED_PREFIX: &str = "forgebase.";
/// Most attributes per message.
pub const MAX_ATTRIBUTES: usize = 100;
/// Longest attribute key (bytes).
pub const MAX_ATTRIBUTE_KEY: usize = 256;
/// Longest attribute value (bytes).
pub const MAX_ATTRIBUTE_VALUE: usize = 1024;
/// Longest ordering key (bytes).
pub const MAX_ORDERING_KEY: usize = 1024;
/// Longest dedup id (bytes).
pub const MAX_DEDUP_ID: usize = 256;
/// Largest topic schema (serialized bytes).
pub const MAX_SCHEMA_BYTES: usize = 64 * 1024;
/// Longest ack deadline and deadline extension (10 minutes).
pub const MAX_ACK_DEADLINE_MS: i64 = 600_000;
/// Shortest ack deadline.
pub const MIN_ACK_DEADLINE_MS: i64 = 1_000;
/// Most delivery attempts that can be configured.
pub const MAX_DELIVERY_ATTEMPTS: i64 = 100;
/// Most messages returned by one pull.
pub const MAX_PULL: i64 = 1000;
/// Most ack ids in one ack, nack or deadline change.
pub const MAX_ACK_IDS: usize = 1000;
/// Shortest subscription time to live (`ttl_ms`).
pub const MIN_SUBSCRIPTION_TTL_MS: i64 = 60_000;
/// Longest subscription time to live (31 days).
pub const MAX_SUBSCRIPTION_TTL_MS: i64 = 31 * 86_400_000;
/// Default grace window of a secret rotation (24 hours).
pub const DEFAULT_SECRET_GRACE_MS: i64 = 86_400_000;
/// Longest grace window of a secret rotation (7 days).
pub const MAX_SECRET_GRACE_MS: i64 = 7 * 86_400_000;
/// Most consecutive failures `pause_after_failures` may name.
pub const MAX_PAUSE_AFTER_FAILURES: i64 = 1000;
/// Rolling window of the endpoint health of a push subscription (1 hour).
pub const HEALTH_WINDOW_MS: i64 = 3_600_000;
/// Most recent attempts the endpoint health looks at.
pub const HEALTH_MAX_ATTEMPTS: i64 = 1000;
/// Most dead letters one redrive call handles.
pub const MAX_REDRIVE: i64 = 1000;
/// Longest topic description (characters).
pub const MAX_DESCRIPTION: usize = 1024;
/// Largest topic example (serialized bytes).
pub const MAX_EXAMPLE_BYTES: usize = 64 * 1024;

/// Limits and timings of the queue service. The defaults suit one embedded
/// instance; the server fills them from the `[queue]` config section.
#[derive(Debug, Clone)]
pub struct QueueSettings {
    /// Upper bound of a topic's `max_message_bytes` (and its default).
    pub max_message_bytes: i64,
    /// Retention of topics that do not set one.
    pub default_retention_ms: i64,
    /// Longest retention a topic may set.
    pub max_retention_ms: i64,
    /// Dedup window of topics that do not set one.
    pub default_dedup_window_ms: i64,
    /// Longest dedup window.
    pub max_dedup_window_ms: i64,
    /// Most messages in one publish call.
    pub max_batch: usize,
    /// Longest long-poll wait of a pull.
    pub max_pull_wait_ms: i64,
    /// How often the background task runs maintenance and push delivery
    /// (publishes in this process wake it at once).
    pub poll_interval: Duration,
    /// Timeout of one push request (webhook or function).
    pub push_timeout: Duration,
    /// Messages one push subscription delivers per round.
    pub push_batch: i64,
    /// Push subscriptions served concurrently.
    pub push_concurrency: usize,
    /// Accept `http://` push endpoints (tests, local development). Off by
    /// default: webhooks must be `https://`.
    pub allow_http_push: bool,
    /// Dead-letter records older than this are deleted.
    pub dead_letter_retention_ms: i64,
    /// Quota feature counting published messages; `None` turns it off.
    pub messages_feature: Option<String>,
    /// Quota feature counting published bytes; `None` turns it off.
    pub bytes_feature: Option<String>,
    /// Bytes of a push endpoint's response body kept per attempt (ADR-0031).
    pub push_response_body_bytes: usize,
    /// Push attempts kept per subscription in the attempt log.
    pub push_attempts_per_subscription: i64,
    /// How long push attempts are kept in the attempt log.
    pub push_attempts_retention_ms: i64,
    /// Test sends one push subscription may make per minute.
    pub push_test_sends_per_minute: u32,
}

impl Default for QueueSettings {
    fn default() -> Self {
        Self {
            max_message_bytes: 1024 * 1024,
            default_retention_ms: 7 * 86_400_000,
            max_retention_ms: 31 * 86_400_000,
            default_dedup_window_ms: 600_000,
            max_dedup_window_ms: 86_400_000,
            max_batch: 1000,
            max_pull_wait_ms: 20_000,
            poll_interval: Duration::from_secs(1),
            push_timeout: Duration::from_secs(10),
            push_batch: 20,
            push_concurrency: 8,
            allow_http_push: false,
            dead_letter_retention_ms: 14 * 86_400_000,
            messages_feature: Some("queue.messages".into()),
            bytes_feature: Some("queue.bytes".into()),
            push_response_body_bytes: 4096,
            push_attempts_per_subscription: 1000,
            push_attempts_retention_ms: 7 * 86_400_000,
            push_test_sends_per_minute: 10,
        }
    }
}

// ------------------------------------------------------------------- topics

/// Record changes of a collection relayed from the realtime outbox (FB-40)
/// into a topic: a message exists exactly when its change committed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RecordSource {
    /// Collection name or id.
    pub collection: String,
    /// `create`, `update`, `delete`; empty means all three.
    #[serde(default)]
    pub ops: Vec<String>,
}

/// Body of a topic creation.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewTopic {
    /// 1-128 letters, digits, `.`, `_`, `:` or `-`, starting with a letter or digit.
    pub name: String,
    /// How long messages are kept, acknowledged or not (default 7 days).
    #[serde(default)]
    pub retention_ms: Option<i64>,
    /// Most bytes kept; the oldest messages are dropped beyond it (default: no limit).
    #[serde(default)]
    pub retention_bytes: Option<i64>,
    /// Largest message (data, attributes and ordering key), default and upper bound from the server config.
    #[serde(default)]
    pub max_message_bytes: Option<i64>,
    /// A repeated `dedup_id` inside this window is accepted once (default 10 minutes).
    #[serde(default)]
    pub dedup_window_ms: Option<i64>,
    /// JSON Schema every message's `data` must satisfy (subset, see the ADR).
    #[serde(default)]
    #[schema(value_type = Option<Object>)]
    pub schema: Option<Json>,
    /// Feed the topic from a collection's committed record changes.
    #[serde(default)]
    pub source: Option<RecordSource>,
    /// Free-form labels.
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    /// What the topic's events are (event catalog; at most 1024 characters).
    #[serde(default)]
    pub description: Option<String>,
    /// An example of message `data` (event catalog); must satisfy the schema when one is set.
    #[serde(default)]
    #[schema(value_type = Option<Object>)]
    pub example: Option<Json>,
}

/// Body of a topic change; absent fields keep their value.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TopicPatch {
    /// New retention.
    #[serde(default)]
    pub retention_ms: Option<i64>,
    /// New size limit; `0` removes it.
    #[serde(default)]
    pub retention_bytes: Option<i64>,
    /// New message size limit.
    #[serde(default)]
    pub max_message_bytes: Option<i64>,
    /// New dedup window.
    #[serde(default)]
    pub dedup_window_ms: Option<i64>,
    /// New schema (applies to later publishes).
    #[serde(default)]
    #[schema(value_type = Option<Object>)]
    pub schema: Option<Json>,
    /// Removes the schema.
    #[serde(default)]
    pub clear_schema: bool,
    /// Replaces the labels.
    #[serde(default)]
    pub labels: Option<BTreeMap<String, String>>,
    /// New description; an empty string removes it.
    #[serde(default)]
    pub description: Option<String>,
    /// New example of message `data`.
    #[serde(default)]
    #[schema(value_type = Option<Object>)]
    pub example: Option<Json>,
    /// Removes the example.
    #[serde(default)]
    pub clear_example: bool,
}

/// A topic.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Topic {
    /// Name.
    pub name: String,
    /// Owning tenant (FB-141); `None` for an instance-wide topic.
    pub tenant: Option<String>,
    /// Retention in milliseconds.
    pub retention_ms: i64,
    /// Size limit in bytes (`None`: no limit).
    pub retention_bytes: Option<i64>,
    /// Largest message in bytes.
    pub max_message_bytes: i64,
    /// Dedup window in milliseconds.
    pub dedup_window_ms: i64,
    /// JSON Schema of message data.
    #[schema(value_type = Option<Object>)]
    pub schema: Option<Json>,
    /// Record-change source.
    pub source: Option<RecordSource>,
    /// Labels.
    pub labels: BTreeMap<String, String>,
    /// What the topic's events are.
    pub description: Option<String>,
    /// Example of message `data`.
    #[schema(value_type = Option<Object>)]
    pub example: Option<Json>,
    /// Messages ever published.
    pub published_total: i64,
    /// Bytes ever published.
    pub published_bytes: i64,
    /// Creator.
    pub created_by: String,
    /// Created (RFC 3339).
    pub created_at: String,
    /// Last changed (RFC 3339).
    pub updated_at: String,
}

// ----------------------------------------------------------------- messages

/// One message to publish.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewMessage {
    /// Any JSON value.
    #[schema(value_type = Object)]
    pub data: Json,
    /// String attributes (subscription filters match on them).
    #[serde(default)]
    pub attributes: BTreeMap<String, String>,
    /// Messages with the same key reach an ordered subscription in publish order, one at a time.
    #[serde(default)]
    pub ordering_key: Option<String>,
    /// A repeat inside the topic's dedup window is accepted once.
    #[serde(default)]
    pub dedup_id: Option<String>,
}

impl NewMessage {
    /// A message with `data` and nothing else.
    pub fn new(data: Json) -> Self {
        Self {
            data,
            ..Self::default()
        }
    }

    /// Adds an attribute.
    pub fn attr(mut self, key: &str, value: &str) -> Self {
        self.attributes.insert(key.to_owned(), value.to_owned());
        self
    }

    /// Sets the ordering key.
    pub fn ordering_key(mut self, key: &str) -> Self {
        self.ordering_key = Some(key.to_owned());
        self
    }

    /// Sets the dedup id.
    pub fn dedup_id(mut self, id: &str) -> Self {
        self.dedup_id = Some(id.to_owned());
        self
    }
}

/// Body of a publish.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PublishRequest {
    /// 1 to 1000 messages, published in one transaction.
    pub messages: Vec<NewMessage>,
}

/// The outcome of one published message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Published {
    /// Message id (of the first message for a duplicate).
    pub message_id: String,
    /// Sequence number in the topic (`0` for a duplicate whose first message expired).
    pub seq: i64,
    /// `true` when the dedup id was seen inside the window and nothing was stored.
    pub duplicate: bool,
}

/// Response of a publish.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PublishResponse {
    /// One per message, in order.
    pub results: Vec<Published>,
}

/// One delivery attempt of a message to a subscription.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DeliveryAttempt {
    /// Attempt number, from 1.
    pub attempt: i64,
    /// When it was handed out (RFC 3339).
    pub at: String,
    /// Consumer it went to (`rest`, a stream id, `push`).
    pub consumer: Option<String>,
    /// `nacked`, `expired`, `failed` (push), `released`; absent while outstanding.
    pub outcome: Option<String>,
    /// Push or nack error.
    pub error: Option<String>,
}

/// A stored message.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Message {
    /// Message id.
    pub id: String,
    /// Topic.
    pub topic: String,
    /// Sequence number (strictly increasing across topics).
    pub seq: i64,
    /// Data.
    #[schema(value_type = Object)]
    pub data: Json,
    /// Attributes.
    pub attributes: BTreeMap<String, String>,
    /// Ordering key.
    pub ordering_key: Option<String>,
    /// Dedup id.
    pub dedup_id: Option<String>,
    /// Publisher (actor).
    pub publisher: String,
    /// Size counted against limits.
    pub bytes: i64,
    /// Published (RFC 3339).
    pub published_at: String,
    /// For a message on a dead-letter topic: its deliveries on the source subscription.
    pub delivery_history: Option<Vec<DeliveryAttempt>>,
}

/// Retained messages of a topic.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MessagePage {
    /// Oldest first.
    pub items: Vec<Message>,
    /// Pass as `after` for the next page; absent at the end.
    pub next_after: Option<i64>,
}

// ------------------------------------------------------- delivery engines

/// Where the owed deliveries of subscriptions are stored (ADR-0029 §1.6):
/// ForgeBase's own table, or one pgmq queue per subscription (PostgreSQL
/// with pgmq). Callers see the same behaviour either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryEngine {
    /// `_forgebase_queue_deliveries` (SQLite, and PostgreSQL without pgmq).
    Portable,
    /// One pgmq queue per subscription.
    Pgmq,
}

impl DeliveryEngine {
    /// `portable` or `pgmq`.
    pub fn as_str(self) -> &'static str {
        match self {
            DeliveryEngine::Portable => "portable",
            DeliveryEngine::Pgmq => "pgmq",
        }
    }
}

/// What a `queue.convert` moved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ConvertReport {
    /// Engine before the conversion.
    pub from: DeliveryEngine,
    /// Engine now.
    pub to: DeliveryEngine,
    /// Subscriptions whose deliveries moved.
    pub subscriptions: i64,
    /// Owed deliveries moved.
    pub deliveries: i64,
}

// ------------------------------------------------------------ subscriptions

/// Where a new subscription starts reading.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum StartPosition {
    /// Only messages published after creation (default).
    #[default]
    Latest,
    /// Every retained message.
    Earliest,
    /// Retained messages published at or after this RFC 3339 time.
    Time(String),
}

/// Kind of push endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PushKind {
    /// HTTPS `POST` signed with the subscription's secret.
    Webhook,
    /// A server function (FB-60), invoked with the message as input.
    Function,
}

impl PushKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            PushKind::Webhook => "webhook",
            PushKind::Function => "function",
        }
    }

    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s {
            "webhook" => Some(PushKind::Webhook),
            "function" => Some(PushKind::Function),
            _ => None,
        }
    }
}

/// Push delivery of a subscription.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PushConfig {
    /// `webhook` or `function`.
    pub kind: PushKind,
    /// Webhook URL (`https://`) or function name.
    pub endpoint: String,
}

/// Body of a subscription creation.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewSubscription {
    /// 1-128 letters, digits, `.`, `_`, `:` or `-`.
    pub name: String,
    /// Topic to subscribe to.
    pub topic: String,
    /// Attribute filter (see the ADR); immutable.
    #[serde(default)]
    #[schema(value_type = Option<Object>)]
    pub filter: Option<Json>,
    /// Visibility timeout of a delivery, 1 s to 10 min (default 10 s).
    #[serde(default)]
    pub ack_deadline_ms: Option<i64>,
    /// After this many deliveries a message is dead-lettered (1-100; default: unlimited).
    #[serde(default)]
    pub max_delivery_attempts: Option<i64>,
    /// Redelivery backoff after a nack or a missed deadline: base (default 1 s).
    #[serde(default)]
    pub backoff_base_ms: Option<i64>,
    /// Backoff cap (default 10 min).
    #[serde(default)]
    pub backoff_max_ms: Option<i64>,
    /// Topic that receives dead-lettered messages with their delivery history.
    #[serde(default)]
    pub dead_letter_topic: Option<String>,
    /// Deliver messages with the same ordering key in publish order, one at a time; immutable.
    #[serde(default)]
    pub ordering: bool,
    /// Push delivery instead of pull.
    #[serde(default)]
    pub push: Option<PushConfig>,
    /// Where to start (default `latest`).
    #[serde(default)]
    pub start: Option<StartPosition>,
    /// Free-form labels.
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    /// Push: pause delivery after this many consecutive failed attempts
    /// (1-1000; default: never). Paused subscriptions keep their messages.
    #[serde(default)]
    pub pause_after_failures: Option<i64>,
    /// Delete the subscription this long after creation (60 s to 31 days;
    /// default: never). A later change with `ttl_ms` extends it.
    #[serde(default)]
    pub ttl_ms: Option<i64>,
}

/// Body of a subscription change; absent fields keep their value.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SubscriptionPatch {
    /// New ack deadline.
    #[serde(default)]
    pub ack_deadline_ms: Option<i64>,
    /// New attempt limit; `0` removes it.
    #[serde(default)]
    pub max_delivery_attempts: Option<i64>,
    /// New backoff base.
    #[serde(default)]
    pub backoff_base_ms: Option<i64>,
    /// New backoff cap.
    #[serde(default)]
    pub backoff_max_ms: Option<i64>,
    /// New dead-letter topic.
    #[serde(default)]
    pub dead_letter_topic: Option<String>,
    /// Removes the dead-letter topic.
    #[serde(default)]
    pub clear_dead_letter_topic: bool,
    /// New push endpoint (a webhook gets a new signing secret).
    #[serde(default)]
    pub push: Option<PushConfig>,
    /// Turns push off (the subscription becomes a pull subscription).
    #[serde(default)]
    pub clear_push: bool,
    /// Replaces the labels.
    #[serde(default)]
    pub labels: Option<BTreeMap<String, String>>,
    /// New auto-pause threshold; `0` turns auto-pause off.
    #[serde(default)]
    pub pause_after_failures: Option<i64>,
    /// Delete the subscription this long from now; `0` removes the expiry.
    #[serde(default)]
    pub ttl_ms: Option<i64>,
}

/// A subscription.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[schema(as = QueueSubscription)]
pub struct Subscription {
    /// Name.
    pub name: String,
    /// Owning tenant (FB-141); `None` for an instance-wide subscription.
    pub tenant: Option<String>,
    /// Topic.
    pub topic: String,
    /// Attribute filter.
    #[schema(value_type = Option<Object>)]
    pub filter: Option<Json>,
    /// Ack deadline in milliseconds.
    pub ack_deadline_ms: i64,
    /// Attempt limit (`None`: unlimited).
    pub max_delivery_attempts: Option<i64>,
    /// Backoff base in milliseconds.
    pub backoff_base_ms: i64,
    /// Backoff cap in milliseconds.
    pub backoff_max_ms: i64,
    /// Dead-letter topic.
    pub dead_letter_topic: Option<String>,
    /// Ordered delivery per ordering key.
    pub ordering: bool,
    /// Push endpoint.
    pub push: Option<PushConfig>,
    /// Webhook signing secret: returned only when it is created.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub push_secret: Option<String>,
    /// End of the grace window of the secret a rotation replaced (RFC 3339):
    /// until then deliveries carry a signature with each secret.
    pub push_secret_previous_expires_at: Option<String>,
    /// Auto-pause after this many consecutive failed push attempts.
    pub pause_after_failures: Option<i64>,
    /// When push delivery was paused (RFC 3339); messages accumulate until it is resumed.
    pub paused_at: Option<String>,
    /// Why it was paused (`auto: …` for the circuit breaker).
    pub paused_reason: Option<String>,
    /// When the subscription is deleted (RFC 3339).
    pub expires_at: Option<String>,
    /// Labels.
    pub labels: BTreeMap<String, String>,
    /// Creator.
    pub created_by: String,
    /// Created (RFC 3339).
    pub created_at: String,
    /// Last changed (RFC 3339).
    pub updated_at: String,
}

// ------------------------------------------------------------------ consume

/// Body of a pull.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PullRequest {
    /// Most messages to return, 1-1000 (default 10).
    #[serde(default)]
    pub max_messages: Option<i64>,
    /// Long poll: wait up to this long for a message (default 0, capped by the server).
    #[serde(default)]
    pub wait_ms: Option<i64>,
    /// Consumer label recorded in the delivery history (default `rest`).
    #[serde(default)]
    pub consumer: Option<String>,
}

/// A delivered message.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct ReceivedMessage {
    /// Token for ack, nack and deadline changes of this delivery.
    pub ack_id: String,
    /// 1 for the first delivery.
    pub delivery_attempt: i64,
    /// Deadline (RFC 3339) after which it is redelivered.
    pub ack_deadline: String,
    /// The message.
    pub message: Message,
}

/// Response of a pull.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PullResponse {
    /// Possibly empty.
    pub messages: Vec<ReceivedMessage>,
}

/// Body of an ack or nack.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AckRequest {
    /// Ack ids from deliveries.
    pub ack_ids: Vec<String>,
}

/// Body of a deadline change.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ModifyDeadlineRequest {
    /// Ack ids from deliveries.
    pub ack_ids: Vec<String>,
    /// New deadline from now, 0 to 10 min; `0` nacks.
    pub ack_deadline_ms: i64,
}

/// Outcome of an ack, nack or deadline change.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, ToSchema)]
pub struct AckResult {
    /// Deliveries changed.
    pub count: i64,
    /// Ack ids that are unknown or no longer current (redelivered since, acked, or dead-lettered).
    pub unknown: Vec<String>,
}

/// Body of a seek: exactly one of `time` and `snapshot`.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SeekRequest {
    /// Messages published before this RFC 3339 time count as acknowledged, the rest are delivered again.
    #[serde(default)]
    pub time: Option<String>,
    /// Restore the acknowledgement state saved in this snapshot.
    #[serde(default)]
    pub snapshot: Option<String>,
}

/// Outcome of a seek.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct SeekResult {
    /// The subscription's cursor afterwards.
    pub cursor_seq: i64,
    /// Messages restored from the snapshot (0 for a time seek).
    pub restored: i64,
}

/// Body of a snapshot creation.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewSnapshot {
    /// 1-128 letters, digits, `.`, `_`, `:` or `-`.
    pub name: String,
    /// Subscription whose acknowledgement state is saved.
    pub subscription: String,
}

/// A saved acknowledgement state of a subscription.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Snapshot {
    /// Name.
    pub name: String,
    /// Owning tenant (FB-141); `None` for an instance-wide snapshot.
    pub tenant: Option<String>,
    /// Subscription.
    pub subscription: String,
    /// Topic.
    pub topic: String,
    /// Cursor at the time.
    pub cursor_seq: i64,
    /// Messages that were unacknowledged at the time.
    pub unacked: i64,
    /// Creator.
    pub created_by: String,
    /// Created (RFC 3339).
    pub created_at: String,
    /// Expires (RFC 3339): the topic's retention after creation.
    pub expires_at: String,
}

// ------------------------------------------------------------------ metrics

/// Metrics of a topic.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct TopicMetrics {
    /// Topic.
    pub topic: String,
    /// Retained messages.
    pub messages: i64,
    /// Retained bytes.
    pub bytes: i64,
    /// Age of the oldest retained message in milliseconds.
    pub oldest_message_age_ms: Option<i64>,
    /// Messages ever published.
    pub published_total: i64,
    /// Bytes ever published.
    pub published_bytes: i64,
    /// Messages published in the last minute.
    pub published_last_minute: i64,
    /// Average publish rate over the last 5 minutes, per second.
    pub publish_rate_per_sec: f64,
    /// Subscriptions of the topic.
    pub subscriptions: i64,
}

/// Metrics of a subscription.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct SubscriptionMetrics {
    /// Subscription.
    pub subscription: String,
    /// Topic.
    pub topic: String,
    /// Messages not yet acknowledged (ready, delayed, in flight, and not yet read from the topic).
    pub backlog_messages: i64,
    /// Bytes of the backlog.
    pub backlog_bytes: i64,
    /// Age of the oldest unacknowledged message in milliseconds.
    pub oldest_unacked_age_ms: Option<i64>,
    /// Deliverable now.
    pub ready: i64,
    /// Waiting for a retry backoff.
    pub delayed: i64,
    /// Handed out and not yet acknowledged.
    pub in_flight: i64,
    /// Deliveries ever made.
    pub delivered_total: i64,
    /// Acknowledgements.
    pub acked_total: i64,
    /// Negative acknowledgements.
    pub nacked_total: i64,
    /// Missed ack deadlines.
    pub expired_total: i64,
    /// Messages dead-lettered.
    pub dead_lettered_total: i64,
    /// Dead-letter records kept.
    pub dead_letters: i64,
    /// Acknowledgements in the last minute.
    pub acked_last_minute: i64,
    /// Average ack rate over the last 5 minutes, per second.
    pub ack_rate_per_sec: f64,
    /// Endpoint health of a push subscription (absent for pull).
    pub push: Option<PushHealth>,
}

/// Endpoint health of a push subscription (ADR-0031 §5): the attempts of the
/// last hour (at most the newest 1000; test sends excluded) and the
/// circuit-breaker state.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct PushHealth {
    /// Length of the rolling window in milliseconds.
    pub window_ms: i64,
    /// Attempts in the window.
    pub attempts: i64,
    /// Successful attempts in the window.
    pub successes: i64,
    /// Failed attempts in the window.
    pub failures: i64,
    /// `successes / attempts` (absent without attempts).
    pub success_rate: Option<f64>,
    /// Median latency in milliseconds.
    pub latency_p50_ms: Option<i64>,
    /// 95th percentile latency in milliseconds.
    pub latency_p95_ms: Option<i64>,
    /// 99th percentile latency in milliseconds.
    pub latency_p99_ms: Option<i64>,
    /// Failed attempts since the last success.
    pub consecutive_failures: i64,
    /// Last successful attempt (RFC 3339).
    pub last_success_at: Option<String>,
    /// Last failed attempt (RFC 3339).
    pub last_failure_at: Option<String>,
    /// Error of the last failed attempt.
    pub last_error: Option<String>,
    /// Push delivery is paused.
    pub paused: bool,
    /// When it was paused (RFC 3339).
    pub paused_at: Option<String>,
    /// Why it was paused.
    pub paused_reason: Option<String>,
    /// Auto-pause threshold.
    pub pause_after_failures: Option<i64>,
}

/// A message that used up its delivery attempts.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct DeadLetter {
    /// Record id.
    pub id: i64,
    /// Subscription it failed on.
    pub subscription: String,
    /// Its topic.
    pub topic: String,
    /// Message id.
    pub message_id: String,
    /// Data.
    #[schema(value_type = Object)]
    pub data: Json,
    /// Attributes.
    pub attributes: BTreeMap<String, String>,
    /// Ordering key.
    pub ordering_key: Option<String>,
    /// Published (RFC 3339).
    pub published_at: String,
    /// Deliveries made.
    pub attempts: i64,
    /// Every delivery attempt.
    pub history: Vec<DeliveryAttempt>,
    /// Why it was dead-lettered.
    pub reason: String,
    /// Dead-letter topic it was forwarded to.
    pub dead_letter_topic: Option<String>,
    /// Id of the copy on the dead-letter topic.
    pub forwarded_message_id: Option<String>,
    /// When (RFC 3339).
    pub at: String,
    /// When it was redriven to the subscription (RFC 3339); a record is redriven once.
    pub redriven_at: Option<String>,
    /// Who redrove it.
    pub redriven_by: Option<String>,
}

/// Dead letters of a subscription.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DeadLetterPage {
    /// Newest first.
    pub items: Vec<DeadLetter>,
}

/// What one maintenance round did (tests and logs).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MaintenanceReport {
    /// Expired leases returned or dead-lettered.
    pub expired: usize,
    /// Messages dropped by retention.
    pub pruned: u64,
    /// Messages relayed from the realtime outbox.
    pub relayed: usize,
    /// Messages metered after a transactional publish.
    pub metered: usize,
    /// Subscriptions deleted because their `ttl_ms` ran out.
    pub expired_subscriptions: usize,
}

// --------------------------------------------------------- webhook tooling

/// How a push attempt ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PushOutcome {
    /// `2xx` (webhook) or a function that finished without error: acknowledged.
    Success,
    /// Anything else: negatively acknowledged (retried after the backoff).
    Failure,
}

impl PushOutcome {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            PushOutcome::Success => "success",
            PushOutcome::Failure => "failure",
        }
    }
}

/// Why a push attempt failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PushErrorClass {
    /// No answer within `[queue] push_timeout_secs`.
    Timeout,
    /// The host name did not resolve.
    Dns,
    /// No TCP connection could be made.
    Connect,
    /// The TLS handshake failed (certificate, protocol).
    Tls,
    /// Refused before connecting: the host resolves to a private or reserved address.
    SsrfRefused,
    /// The endpoint answered with a non-`2xx` status.
    HttpStatus,
    /// Too many redirects or an invalid `Location`.
    Redirect,
    /// The response was malformed, cut off or too large.
    Protocol,
    /// The request could not be built (size, headers).
    InvalidRequest,
    /// The server cannot push (no client, no signing secret).
    Config,
    /// The function failed.
    Function,
}

impl PushErrorClass {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            PushErrorClass::Timeout => "timeout",
            PushErrorClass::Dns => "dns",
            PushErrorClass::Connect => "connect",
            PushErrorClass::Tls => "tls",
            PushErrorClass::SsrfRefused => "ssrf_refused",
            PushErrorClass::HttpStatus => "http_status",
            PushErrorClass::Redirect => "redirect",
            PushErrorClass::Protocol => "protocol",
            PushErrorClass::InvalidRequest => "invalid_request",
            PushErrorClass::Config => "config",
            PushErrorClass::Function => "function",
        }
    }

    pub(crate) fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "timeout" => PushErrorClass::Timeout,
            "dns" => PushErrorClass::Dns,
            "connect" => PushErrorClass::Connect,
            "tls" => PushErrorClass::Tls,
            "ssrf_refused" => PushErrorClass::SsrfRefused,
            "http_status" => PushErrorClass::HttpStatus,
            "redirect" => PushErrorClass::Redirect,
            "protocol" => PushErrorClass::Protocol,
            "invalid_request" => PushErrorClass::InvalidRequest,
            "config" => PushErrorClass::Config,
            "function" => PushErrorClass::Function,
            _ => return None,
        })
    }
}

/// One push attempt of a subscription: a delivery or a test send (ADR-0031 §1).
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct PushAttempt {
    /// Record id (increasing).
    pub id: i64,
    /// Subscription.
    pub subscription: String,
    /// Message delivered (`test-…` for a test send).
    pub message_id: String,
    /// Delivery attempt of the message, from 1.
    pub attempt: i64,
    /// A test send: not a message, no effect on delivery state or totals.
    pub test: bool,
    /// `webhook` or `function`.
    pub kind: PushKind,
    /// Webhook URL or function name.
    pub endpoint: String,
    /// When it started (RFC 3339).
    pub at: String,
    /// How long it took in milliseconds.
    pub latency_ms: i64,
    /// `success` or `failure`.
    pub outcome: PushOutcome,
    /// HTTP status of the answer (webhooks that answered).
    pub status: Option<i64>,
    /// Why it failed.
    pub error_class: Option<PushErrorClass>,
    /// The failure in words.
    pub error: Option<String>,
    /// Headers sent (the signature masked).
    pub request_headers: BTreeMap<String, String>,
    /// Allowlisted headers of the answer (`content-type`, `retry-after`, request ids, ...).
    pub response_headers: BTreeMap<String, String>,
    /// The answer's body, cut to `[queue] push_response_body_bytes` (lossy UTF-8).
    pub response_body: Option<String>,
    /// Full size of the answer's body in bytes.
    pub response_bytes: Option<i64>,
    /// The body was cut.
    pub response_truncated: bool,
}

/// Push attempts of a subscription.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PushAttemptPage {
    /// Newest first.
    pub items: Vec<PushAttempt>,
    /// Pass as `before` for the next page; absent at the end.
    pub next_before: Option<i64>,
}

/// Filters of an attempt listing.
#[derive(Debug, Clone, Default)]
pub struct AttemptQuery {
    /// Only this outcome.
    pub outcome: Option<PushOutcome>,
    /// Only attempts of this message.
    pub message_id: Option<String>,
    /// Only attempts at or after this time (epoch ms).
    pub since_ms: Option<i64>,
    /// Only attempts before this time (epoch ms).
    pub until_ms: Option<i64>,
    /// Only test sends (`true`) or only deliveries (`false`).
    pub test: Option<bool>,
    /// Only records with a lower id (paging).
    pub before: Option<i64>,
    /// Page size, 1-500.
    pub limit: i64,
}

/// Body of a test send (optional).
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TestSendRequest {
    /// `data` of the synthetic message (default: a short `forgebase.test` object).
    #[serde(default)]
    #[schema(value_type = Option<Object>)]
    pub data: Option<Json>,
}

/// Body of a redrive: `ids`, or `all` (optionally filtered).
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RedriveRequest {
    /// Dead-letter record ids (1-1000).
    #[serde(default)]
    pub ids: Vec<i64>,
    /// Every dead letter of the subscription not yet redriven (oldest first, at most `limit`).
    #[serde(default)]
    pub all: bool,
    /// With `all`: only records dead-lettered at or after this RFC 3339 time.
    #[serde(default)]
    pub since: Option<String>,
    /// With `all`: only records whose message attributes match this filter (subscription filter syntax).
    #[serde(default)]
    #[schema(value_type = Option<Object>)]
    pub filter: Option<Json>,
    /// With `all`: most records to redrive in this call (1-1000, default 1000).
    #[serde(default)]
    pub limit: Option<i64>,
}

/// Outcome of a redrive.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, ToSchema)]
pub struct RedriveResult {
    /// Records delivered to the subscription again (attempt counter reset).
    pub redriven: Vec<i64>,
    /// Records that were redriven before (each record is redriven once).
    pub already_redriven: Vec<i64>,
    /// Ids that are not dead letters of this subscription.
    pub not_found: Vec<i64>,
    /// With `all`: matching records left for another call.
    pub remaining: i64,
}

/// Body of a "retry now" (optional).
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RetryRequest {
    /// Only these messages (default: every message waiting for a retry backoff).
    #[serde(default)]
    pub message_ids: Vec<String>,
}

/// Outcome of a "retry now".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, ToSchema)]
pub struct RetryResult {
    /// Deliveries that were waiting for a backoff and are ready now.
    pub count: i64,
}

/// Body of a secret rotation (optional).
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RotateSecretRequest {
    /// How long the old secret keeps signing too, 0 to 7 days (default 24 hours; `0` drops it at once).
    #[serde(default)]
    pub grace_ms: Option<i64>,
}

/// A rotated webhook secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct RotatedSecret {
    /// Subscription.
    pub subscription: String,
    /// The new signing secret: shown only here.
    pub push_secret: String,
    /// Until when deliveries are also signed with the old secret (RFC 3339; absent with no grace).
    pub previous_expires_at: Option<String>,
}

/// Body of a pause (optional).
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PauseRequest {
    /// Why (shown as `paused_reason`; at most 500 characters).
    #[serde(default)]
    pub reason: Option<String>,
}

/// Where an example in the catalog comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExampleSource {
    /// The topic's `example`.
    Topic,
    /// Derived from the topic schema or the collection's fields.
    Schema,
    /// Nothing describes the data.
    None,
}

/// One kind of event of a record-source topic.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct CatalogEventType {
    /// `created`, `updated` or `deleted`.
    #[serde(rename = "type")]
    pub event_type: String,
    /// The record operation (`create`, `update`, `delete`): attribute `forgebase.op`.
    pub op: String,
    /// What it means.
    pub description: String,
    /// A subscription filter that selects only this event type.
    #[schema(value_type = Object)]
    pub filter: Json,
    /// JSON Schema of the message `data`.
    #[schema(value_type = Object)]
    pub data_schema: Json,
    /// Example message `data`.
    #[schema(value_type = Object)]
    pub example: Json,
}

/// A subscribable topic in the event catalog.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct CatalogTopic {
    /// Topic name.
    pub topic: String,
    /// Owning tenant.
    pub tenant: Option<String>,
    /// What the topic's events are.
    pub description: Option<String>,
    /// JSON Schema of message `data` (the topic schema; for a record source, the change event).
    #[schema(value_type = Option<Object>)]
    pub data_schema: Option<Json>,
    /// Example message `data`.
    #[schema(value_type = Object)]
    pub example: Json,
    /// Where `example` comes from.
    pub example_source: ExampleSource,
    /// Record-change source.
    pub source: Option<RecordSource>,
    /// For a record source: its event types (empty otherwise).
    pub event_types: Vec<CatalogEventType>,
}

/// The subscribable event types: every topic the caller may read.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct EventCatalog {
    /// By topic name.
    pub topics: Vec<CatalogTopic>,
}

// --------------------------------------------------------------- validation

/// Checks a topic, subscription, snapshot or consumer name.
pub(crate) fn check_name(what: &str, value: &str) -> Result<(), QueueError> {
    let ok = !value.is_empty()
        && value.len() <= 128
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'));
    if ok {
        Ok(())
    } else {
        Err(QueueError::invalid(
            "invalid_name",
            format!("{what} must be 1-128 letters, digits, '.', '_', ':' or '-', starting with a letter or digit"),
        ))
    }
}

pub(crate) fn check_labels(labels: &BTreeMap<String, String>) -> Result<(), QueueError> {
    if labels.len() > 64
        || labels
            .iter()
            .any(|(k, v)| k.is_empty() || k.len() > 64 || v.len() > 256)
    {
        return Err(QueueError::invalid(
            "invalid_labels",
            "at most 64 labels, keys 1-64 bytes, values up to 256 bytes",
        ));
    }
    Ok(())
}

/// Checks a message from a publisher and returns its counted size.
pub(crate) fn check_message(m: &NewMessage, data_text: &str, system: bool) -> Result<i64, QueueError> {
    if m.attributes.len() > MAX_ATTRIBUTES {
        return Err(QueueError::invalid(
            "invalid_message",
            format!("at most {MAX_ATTRIBUTES} attributes"),
        ));
    }
    let mut bytes = data_text.len();
    for (k, v) in &m.attributes {
        if k.is_empty() || k.len() > MAX_ATTRIBUTE_KEY || v.len() > MAX_ATTRIBUTE_VALUE {
            return Err(QueueError::invalid(
                "invalid_message",
                format!("attribute keys must be 1-{MAX_ATTRIBUTE_KEY} bytes and values at most {MAX_ATTRIBUTE_VALUE}"),
            ));
        }
        if !system && k.starts_with(RESERVED_PREFIX) {
            return Err(QueueError::invalid(
                "invalid_message",
                format!("attribute keys starting with {RESERVED_PREFIX:?} are reserved"),
            ));
        }
        bytes += k.len() + v.len();
    }
    if let Some(k) = &m.ordering_key {
        if k.is_empty() || k.len() > MAX_ORDERING_KEY {
            return Err(QueueError::invalid(
                "invalid_message",
                format!("ordering_key must be 1-{MAX_ORDERING_KEY} bytes"),
            ));
        }
        bytes += k.len();
    }
    if let Some(d) = &m.dedup_id
        && (d.is_empty() || d.len() > MAX_DEDUP_ID)
    {
        return Err(QueueError::invalid(
            "invalid_message",
            format!("dedup_id must be 1-{MAX_DEDUP_ID} bytes"),
        ));
    }
    Ok(i64::try_from(bytes).unwrap_or(i64::MAX))
}

/// `value` when inside `lo..=hi`, else an `invalid_<field>` error.
pub(crate) fn in_range(field: &'static str, value: i64, lo: i64, hi: i64) -> Result<i64, QueueError> {
    if (lo..=hi).contains(&value) {
        Ok(value)
    } else {
        Err(QueueError::Invalid {
            code: "invalid_parameter",
            message: format!("{field} must be between {lo} and {hi}"),
        })
    }
}

/// Parses an RFC 3339 time into epoch milliseconds.
pub(crate) fn parse_time(field: &str, text: &str) -> Result<i64, QueueError> {
    use time::OffsetDateTime;
    use time::format_description::well_known::Rfc3339;
    let t = OffsetDateTime::parse(text, &Rfc3339)
        .map_err(|_| QueueError::invalid("invalid_parameter", format!("{field} must be an RFC 3339 time")))?;
    Ok(i64::try_from(t.unix_timestamp_nanos() / 1_000_000).unwrap_or(0))
}
