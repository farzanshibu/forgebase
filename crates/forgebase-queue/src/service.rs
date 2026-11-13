//! [`QueueService`]: topics, publishing and subscriptions.
//!
//! All state lives in the system database (migration 0021). The database has
//! one writer, so a publish, a claim or an ack is one atomic transaction, and
//! anything committed survives a crash or restart. Message sequence numbers
//! are assigned by the single writer in commit order, so a subscription's
//! cursor never skips a message that commits later with a lower number.
//!
//! Public methods take an [`AccessContext`] and ask the data core's
//! authorizer first ([`Action::QueueRead`], [`Action::QueuePublish`],
//! [`Action::QueueConsume`], [`Action::QueueAdmin`]).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

use forgebase_auth::crypto::Keyring;
use forgebase_data::{AccessContext, Action, DataService, ResourceKind, ResourceOp};
use forgebase_db::{Backend, DbRouter, Rows, SetHandle, Statement, TxMode, Value, WriteTx};
use forgebase_entitlements::{EntitlementError, EntitlementService, UsageRequest, parse_subject, subject_key};
use forgebase_functions::{Fetcher, FunctionService};
use forgebase_jobs::{Clock, SystemClock};
use serde_json::Value as Json;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::access::{OwnerContext, bare, stored};
use crate::error::QueueError;
use crate::filter::Filter;
use crate::model::{
    DeliveryAttempt, Message, MessagePage, NewMessage, NewSubscription, NewTopic, Published, PushConfig, PushKind,
    QueueSettings, RecordSource, StartPosition, Subscription, SubscriptionPatch, Topic, TopicPatch, check_labels,
    check_message, check_name, in_range, iso, parse_time,
};
use crate::row::{Row, scalar};
use crate::schema;

pub(crate) type Result<T> = std::result::Result<T, QueueError>;

/// Publisher recorded for messages the server writes itself.
pub(crate) const SYSTEM_PUBLISHER: &str = "system:queue";

/// The queue service. Cheap to clone.
///
/// With a router ([`QueueService::attach_router`], FB-172) every call runs
/// on the database set of the topic or subscription it is about; a clone
/// made by [`QueueService::on_set`] is a *view* bound to one set.
#[derive(Clone)]
pub struct QueueService {
    pub(crate) inner: Arc<Inner>,
    /// The set this view is bound to (`None`: the data core's database,
    /// i.e. control).
    pub(crate) set: Option<SetHandle>,
    /// Where this set's deliveries are stored (FB-164), resolved on first use.
    pub(crate) engine: Arc<crate::pgmq::EngineSlot>,
}

impl std::fmt::Debug for QueueService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QueueService")
            .field("settings", &self.inner.settings)
            .field("entitlements", &self.inner.ent.get().is_some())
            .field("functions", &self.inner.functions.get().is_some())
            .field("fetcher", &self.inner.fetcher.get().is_some())
            .finish_non_exhaustive()
    }
}

pub(crate) struct Inner {
    pub(crate) data: DataService,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) settings: QueueSettings,
    pub(crate) keyring: Keyring,
    pub(crate) ent: OnceLock<EntitlementService>,
    pub(crate) functions: OnceLock<FunctionService>,
    pub(crate) fetcher: OnceLock<Fetcher>,
    /// Bumped after every commit that may make messages deliverable.
    pub(crate) wake: watch::Sender<u64>,
    /// `true` once streams must end (server shutdown).
    pub(crate) closed: watch::Sender<bool>,
    pub(crate) background: Mutex<Option<(watch::Sender<bool>, JoinHandle<()>)>>,
    /// The delivery engine of every set (by set id), resolved on first use
    /// (FB-164: each database pins its own).
    pub(crate) engines: Mutex<std::collections::HashMap<String, Arc<crate::pgmq::EngineSlot>>>,
    /// The database sets (FB-172); none: one database.
    pub(crate) router: OnceLock<DbRouter>,
    /// Recent test sends per subscription (epoch ms), for their rate limit (ADR-0031 §2).
    pub(crate) test_sends: Mutex<std::collections::HashMap<String, std::collections::VecDeque<i64>>>,
}

/// Reads through the read pool or inside a write transaction.
pub(crate) enum Db<'a> {
    Ro(&'a dyn Backend),
    Tx(&'a mut dyn WriteTx),
}

impl Db<'_> {
    pub(crate) async fn fetch(&mut self, s: &Statement) -> Result<Rows> {
        Ok(match self {
            Db::Ro(b) => b.fetch_all(s).await?,
            Db::Tx(t) => t.fetch_all(s).await?,
        })
    }
}

/// Commits on `Ok`, rolls back on `Err`.
/// Message attribute carrying the W3C trace context of the publish
/// (ADR-0035 §2).
pub const TRACEPARENT_ATTRIBUTE: &str = "traceparent";
/// Message attribute carrying the publish's `tracestate`.
pub const TRACESTATE_ATTRIBUTE: &str = "tracestate";

/// Gives every message the publish's trace context, unless it has one.
pub(crate) fn stamp_trace(messages: &mut [NewMessage], traceparent: &str, tracestate: Option<&str>) {
    for m in messages {
        if m.attributes.contains_key(TRACEPARENT_ATTRIBUTE) {
            continue;
        }
        m.attributes
            .insert(TRACEPARENT_ATTRIBUTE.to_owned(), traceparent.to_owned());
        if let Some(ts) = tracestate {
            m.attributes
                .entry(TRACESTATE_ATTRIBUTE.to_owned())
                .or_insert_with(|| ts.to_owned());
        }
    }
}

pub(crate) async fn finish<T>(tx: Box<dyn WriteTx>, result: Result<T>) -> Result<T> {
    match result {
        Ok(v) => {
            tx.commit().await?;
            Ok(v)
        }
        Err(e) => {
            if let Err(rb) = tx.rollback().await {
                tracing::warn!(error = %rb, "rollback failed; writer connection discarded");
            }
            Err(e)
        }
    }
}

/// The epoch-millisecond minute bucket of `now`.
pub(crate) fn minute(now: i64) -> i64 {
    now.div_euclid(60_000) * 60_000
}

/// Adds `n` to a per-minute rate counter.
pub(crate) async fn bump_rate(
    tx: &mut dyn WriteTx,
    scope: &str,
    name: &str,
    metric: &str,
    now: i64,
    n: i64,
) -> Result<()> {
    if n == 0 {
        return Ok(());
    }
    tx.execute(&Statement::new(
        "INSERT INTO _forgebase_queue_rates (scope, name, metric, minute, count) VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT (scope, name, metric, minute) DO UPDATE SET count = _forgebase_queue_rates.count + excluded.count",
        vec![scope.into(), name.into(), metric.into(), minute(now).into(), n.into()],
    ))
    .await?;
    Ok(())
}

// --------------------------------------------------------------------- rows

pub(crate) const TOPIC_COLS: &str = "name, retention_ms, retention_bytes, max_message_bytes, dedup_window_ms, \
    message_schema, source_collection, source_ops, source_after_seq, labels, published_total, published_bytes, \
    created_by, created_at, updated_at, tenant, description, example";

/// A topic as stored.
#[derive(Debug, Clone)]
pub(crate) struct TopicRow {
    /// Stored name (`<tenant>/<name>` for a tenant topic).
    pub(crate) name: String,
    /// Owning tenant; `None` for an instance-wide topic.
    pub(crate) tenant: Option<String>,
    pub(crate) retention_ms: i64,
    pub(crate) retention_bytes: Option<i64>,
    pub(crate) max_message_bytes: i64,
    pub(crate) dedup_window_ms: i64,
    pub(crate) schema: Option<Json>,
    pub(crate) source: Option<RecordSource>,
    pub(crate) source_after_seq: i64,
    labels: BTreeMap<String, String>,
    pub(crate) description: Option<String>,
    pub(crate) example: Option<Json>,
    published_total: i64,
    published_bytes: i64,
    created_by: String,
    created_at: i64,
    updated_at: i64,
}

impl TopicRow {
    pub(crate) fn from_row(r: &Row<'_>) -> Result<Self> {
        let source = match r.opt_text("source_collection") {
            None => None,
            Some(collection) => Some(RecordSource {
                collection,
                ops: serde_json::from_str(&r.opt_text("source_ops").unwrap_or_else(|| "[]".into()))
                    .map_err(|e| QueueError::Internal(format!("source_ops: {e}")))?,
            }),
        };
        Ok(Self {
            name: r.text("name"),
            tenant: r.opt_text("tenant"),
            retention_ms: r.int("retention_ms"),
            retention_bytes: r.opt_int("retention_bytes"),
            max_message_bytes: r.int("max_message_bytes"),
            dedup_window_ms: r.int("dedup_window_ms"),
            schema: r.opt_json("message_schema")?,
            source,
            source_after_seq: r.int("source_after_seq"),
            labels: r.map("labels")?,
            description: r.opt_text("description"),
            example: r.opt_json("example")?,
            published_total: r.int("published_total"),
            published_bytes: r.int("published_bytes"),
            created_by: r.text("created_by"),
            created_at: r.int("created_at"),
            updated_at: r.int("updated_at"),
        })
    }

    pub(crate) fn public(&self) -> Topic {
        Topic {
            name: bare(&self.name).to_owned(),
            tenant: self.tenant.clone(),
            retention_ms: self.retention_ms,
            retention_bytes: self.retention_bytes,
            max_message_bytes: self.max_message_bytes,
            dedup_window_ms: self.dedup_window_ms,
            schema: self.schema.clone(),
            source: self.source.clone(),
            labels: self.labels.clone(),
            description: self.description.clone(),
            example: self.example.clone(),
            published_total: self.published_total,
            published_bytes: self.published_bytes,
            created_by: self.created_by.clone(),
            created_at: iso(self.created_at),
            updated_at: iso(self.updated_at),
        }
    }
}

pub(crate) async fn topic_row(db: &mut Db<'_>, name: &str) -> Result<Option<TopicRow>> {
    let rows = db
        .fetch(&Statement::new(
            format!("SELECT {TOPIC_COLS} FROM _forgebase_queue_topics WHERE name = ?1"),
            vec![name.into()],
        ))
        .await?;
    Row::all(&rows).next().map(|r| TopicRow::from_row(&r)).transpose()
}

pub(crate) const SUB_COLS: &str = "name, topic, filter, ack_deadline_ms, max_delivery_attempts, backoff_base_ms, \
    backoff_max_ms, dead_letter_topic, ordering, push_kind, push_endpoint, push_secret, cursor_seq, \
    delivered_total, acked_total, nacked_total, expired_total, dead_lettered_total, labels, created_by, \
    created_at, updated_at, tenant, owner_context, push_secret_previous, push_secret_previous_until, \
    pause_after_failures, paused_at, paused_reason, consecutive_failures, last_success_at, last_failure_at, \
    last_error, expires_at";

/// A subscription as stored.
#[derive(Debug, Clone)]
pub(crate) struct SubRow {
    /// Stored name (`<tenant>/<name>` for a tenant subscription).
    pub(crate) name: String,
    /// Stored name of the topic.
    pub(crate) topic: String,
    /// Owning tenant; `None` for an instance-wide subscription.
    pub(crate) tenant: Option<String>,
    /// The creator's recorded access context (JSON; `None` before FB-141).
    pub(crate) owner_context: Option<String>,
    pub(crate) filter_json: Option<Json>,
    pub(crate) filter: Option<Filter>,
    pub(crate) ack_deadline_ms: i64,
    pub(crate) max_delivery_attempts: Option<i64>,
    pub(crate) backoff_base_ms: i64,
    pub(crate) backoff_max_ms: i64,
    pub(crate) dead_letter_topic: Option<String>,
    pub(crate) ordering: bool,
    pub(crate) push: Option<PushConfig>,
    pub(crate) push_secret: Option<Vec<u8>>,
    /// The secret a rotation replaced, and the end of its grace window (ADR-0031 §4).
    pub(crate) push_secret_previous: Option<Vec<u8>>,
    pub(crate) push_secret_previous_until: Option<i64>,
    /// Circuit breaker and endpoint health (ADR-0031 §5).
    pub(crate) pause_after_failures: Option<i64>,
    pub(crate) paused_at: Option<i64>,
    pub(crate) paused_reason: Option<String>,
    pub(crate) consecutive_failures: i64,
    pub(crate) last_success_at: Option<i64>,
    pub(crate) last_failure_at: Option<i64>,
    pub(crate) last_error: Option<String>,
    /// Deleted by maintenance after this time.
    pub(crate) expires_at: Option<i64>,
    pub(crate) cursor_seq: i64,
    pub(crate) delivered_total: i64,
    pub(crate) acked_total: i64,
    pub(crate) nacked_total: i64,
    pub(crate) expired_total: i64,
    pub(crate) dead_lettered_total: i64,
    labels: BTreeMap<String, String>,
    /// Creator's principal key: the subscription's owner.
    pub(crate) created_by: String,
    created_at: i64,
    updated_at: i64,
}

impl SubRow {
    pub(crate) fn from_row(r: &Row<'_>) -> Result<Self> {
        let filter_json = r.opt_json("filter")?;
        let filter = filter_json
            .as_ref()
            .map(|f| Filter::parse(f).map_err(|e| QueueError::Internal(format!("stored filter: {e}"))))
            .transpose()?;
        let push = match (r.opt_text("push_kind"), r.opt_text("push_endpoint")) {
            (Some(kind), Some(endpoint)) => Some(PushConfig {
                kind: PushKind::parse(&kind).ok_or_else(|| QueueError::Internal(format!("push kind {kind:?}")))?,
                endpoint,
            }),
            _ => None,
        };
        Ok(Self {
            name: r.text("name"),
            topic: r.text("topic"),
            tenant: r.opt_text("tenant"),
            owner_context: r.opt_text("owner_context"),
            filter_json,
            filter,
            ack_deadline_ms: r.int("ack_deadline_ms"),
            max_delivery_attempts: r.opt_int("max_delivery_attempts"),
            backoff_base_ms: r.int("backoff_base_ms"),
            backoff_max_ms: r.int("backoff_max_ms"),
            dead_letter_topic: r.opt_text("dead_letter_topic"),
            ordering: r.int("ordering") == 1,
            push,
            push_secret: r.blob("push_secret"),
            push_secret_previous: r.blob("push_secret_previous"),
            push_secret_previous_until: r.opt_int("push_secret_previous_until"),
            pause_after_failures: r.opt_int("pause_after_failures"),
            paused_at: r.opt_int("paused_at"),
            paused_reason: r.opt_text("paused_reason"),
            consecutive_failures: r.int("consecutive_failures"),
            last_success_at: r.opt_int("last_success_at"),
            last_failure_at: r.opt_int("last_failure_at"),
            last_error: r.opt_text("last_error"),
            expires_at: r.opt_int("expires_at"),
            cursor_seq: r.int("cursor_seq"),
            delivered_total: r.int("delivered_total"),
            acked_total: r.int("acked_total"),
            nacked_total: r.int("nacked_total"),
            expired_total: r.int("expired_total"),
            dead_lettered_total: r.int("dead_lettered_total"),
            labels: r.map("labels")?,
            created_by: r.text("created_by"),
            created_at: r.int("created_at"),
            updated_at: r.int("updated_at"),
        })
    }

    pub(crate) fn public(&self) -> Subscription {
        Subscription {
            name: bare(&self.name).to_owned(),
            tenant: self.tenant.clone(),
            topic: bare(&self.topic).to_owned(),
            filter: self.filter_json.clone(),
            ack_deadline_ms: self.ack_deadline_ms,
            max_delivery_attempts: self.max_delivery_attempts,
            backoff_base_ms: self.backoff_base_ms,
            backoff_max_ms: self.backoff_max_ms,
            dead_letter_topic: self.dead_letter_topic.as_deref().map(|d| bare(d).to_owned()),
            ordering: self.ordering,
            push: self.push.clone(),
            push_secret: None,
            push_secret_previous_expires_at: self
                .push_secret_previous_until
                .filter(|_| self.push_secret_previous.is_some())
                .map(iso),
            pause_after_failures: self.pause_after_failures,
            paused_at: self.paused_at.map(iso),
            paused_reason: self.paused_reason.clone(),
            expires_at: self.expires_at.map(iso),
            labels: self.labels.clone(),
            created_by: self.created_by.clone(),
            created_at: iso(self.created_at),
            updated_at: iso(self.updated_at),
        }
    }

    /// The owner's recorded access context.
    pub(crate) fn owner(&self) -> Result<OwnerContext> {
        OwnerContext::parse(self.owner_context.as_deref(), &self.created_by)
    }
}

/// Whether `name` can name a queue resource (a stored tenant name never can).
fn addressable(name: &str) -> bool {
    check_name("name", name).is_ok()
}

/// The topic `name` resolves to for a caller in `tenant`: the tenant's own
/// topic of that name, else the instance-wide one.
pub(crate) async fn find_topic(db: &mut Db<'_>, tenant: Option<&str>, name: &str) -> Result<Option<TopicRow>> {
    if !addressable(name) {
        return Ok(None);
    }
    if let Some(t) = tenant
        && let Some(row) = topic_row(db, &stored(Some(t), name)).await?
    {
        return Ok(Some(row));
    }
    topic_row(db, name).await
}

/// [`find_topic`], or `TopicNotFound`.
pub(crate) async fn resolve_topic(db: &mut Db<'_>, tenant: Option<&str>, name: &str) -> Result<TopicRow> {
    find_topic(db, tenant, name)
        .await?
        .ok_or_else(|| QueueError::TopicNotFound(name.to_owned()))
}

/// The subscription `name` resolves to for a caller in `tenant`.
pub(crate) async fn find_sub(db: &mut Db<'_>, tenant: Option<&str>, name: &str) -> Result<Option<SubRow>> {
    if !addressable(name) {
        return Ok(None);
    }
    if let Some(t) = tenant
        && let Some(row) = sub_row_opt(db, &stored(Some(t), name)).await?
    {
        return Ok(Some(row));
    }
    sub_row_opt(db, name).await
}

async fn sub_row_opt(db: &mut Db<'_>, stored_name: &str) -> Result<Option<SubRow>> {
    let rows = db
        .fetch(&Statement::new(
            format!("SELECT {SUB_COLS} FROM _forgebase_queue_subscriptions WHERE name = ?1"),
            vec![stored_name.into()],
        ))
        .await?;
    Row::all(&rows).next().map(|r| SubRow::from_row(&r)).transpose()
}

/// Rows of a listing in the caller's scope: the tenant's own plus the
/// instance-wide ones it does not shadow.
pub(crate) fn scoped<T>(rows: Vec<T>, name: impl Fn(&T) -> &str, tenant: impl Fn(&T) -> Option<&str>) -> Vec<T> {
    let mine: std::collections::BTreeSet<String> = rows
        .iter()
        .filter(|r| tenant(r).is_some())
        .map(|r| bare(name(r)).to_owned())
        .collect();
    rows.into_iter()
        .filter(|r| tenant(r).is_some() || !mine.contains(name(r)))
        .collect()
}

pub(crate) async fn sub_row(db: &mut Db<'_>, name: &str) -> Result<SubRow> {
    let rows = db
        .fetch(&Statement::new(
            format!("SELECT {SUB_COLS} FROM _forgebase_queue_subscriptions WHERE name = ?1"),
            vec![name.into()],
        ))
        .await?;
    Row::all(&rows)
        .next()
        .map(|r| SubRow::from_row(&r))
        .transpose()?
        .ok_or_else(|| QueueError::SubscriptionNotFound(bare(name).to_owned()))
}

pub(crate) const MESSAGE_COLS: &str = "m.seq, m.topic, m.id, m.data, m.attributes, m.ordering_key, m.dedup_id, m.publisher, m.bytes, m.published_at, m.history, m.source_row";

pub(crate) fn message_from(r: &Row<'_>) -> Result<Message> {
    let history = match r.opt_text("history") {
        None => None,
        Some(t) => Some(parse_history(&t)?),
    };
    Ok(Message {
        id: r.text("id"),
        topic: bare(&r.text("topic")).to_owned(),
        seq: r.int("seq"),
        data: r.json("data")?,
        attributes: r.map("attributes")?,
        ordering_key: r.opt_text("ordering_key"),
        dedup_id: r.opt_text("dedup_id"),
        publisher: r.text("publisher"),
        bytes: r.int("bytes"),
        published_at: iso(r.int("published_at")),
        delivery_history: history,
    })
}

/// Decodes a stored delivery history (`[{attempt, at, consumer, outcome, error}]`, `at` in ms).
pub(crate) fn parse_history(text: &str) -> Result<Vec<DeliveryAttempt>> {
    let raw: Vec<Json> =
        serde_json::from_str(text).map_err(|e| QueueError::Internal(format!("delivery history: {e}")))?;
    Ok(raw
        .into_iter()
        .map(|e| DeliveryAttempt {
            attempt: e.get("attempt").and_then(Json::as_i64).unwrap_or(0),
            at: iso(e.get("at").and_then(Json::as_i64).unwrap_or(0)),
            consumer: e.get("consumer").and_then(Json::as_str).map(str::to_owned),
            outcome: e.get("outcome").and_then(Json::as_str).map(str::to_owned),
            error: e.get("error").and_then(Json::as_str).map(str::to_owned),
        })
        .collect())
}

// ---------------------------------------------------------------- publishing

/// A validated message ready to insert.
#[derive(Debug, Clone)]
pub(crate) struct Prepared {
    pub(crate) data: String,
    pub(crate) attributes: String,
    pub(crate) ordering_key: Option<String>,
    pub(crate) dedup_id: Option<String>,
    pub(crate) bytes: i64,
    pub(crate) history: Option<String>,
    /// The changed row of a record-change message ([`crate::access::source_row`]).
    pub(crate) source_row: Option<String>,
}

/// The condition "message `m` is for subscription `?n`": every published
/// message, and the copies of redriven dead letters stored for that one
/// subscription (ADR-0031 §3).
pub(crate) fn for_sub(alias: &str, n: u8) -> String {
    let col = if alias.is_empty() {
        "only_subscription".to_owned()
    } else {
        format!("{alias}.only_subscription")
    };
    format!("({col} IS NULL OR {col} = CAST(?{n} AS TEXT))")
}

pub(crate) fn prepare(topic: &TopicRow, m: &NewMessage, system: bool) -> Result<Prepared> {
    let data = serde_json::to_string(&m.data).map_err(|e| QueueError::invalid("invalid_message", e.to_string()))?;
    let bytes = check_message(m, &data, system)?;
    if bytes > topic.max_message_bytes {
        return Err(QueueError::TooLarge(format!(
            "message is {bytes} bytes; topic {:?} accepts at most {}",
            topic.name, topic.max_message_bytes
        )));
    }
    if let Some(s) = &topic.schema
        && !system
    {
        schema::validate(s, &m.data).map_err(|e| QueueError::invalid("schema_violation", e))?;
    }
    Ok(Prepared {
        data,
        attributes: serde_json::to_string(&m.attributes).map_err(|e| QueueError::Internal(e.to_string()))?,
        ordering_key: m.ordering_key.clone(),
        dedup_id: m.dedup_id.clone(),
        bytes,
        history: None,
        source_row: None,
    })
}

/// Inserts one message (honouring the topic's dedup window) and counts it.
pub(crate) async fn insert_message_tx(
    tx: &mut dyn WriteTx,
    topic: &TopicRow,
    p: &Prepared,
    publisher: &str,
    subject: Option<&str>,
    metered: i64,
    now: i64,
) -> Result<Published> {
    let id = format!("msg-{}", uuid::Uuid::new_v4().simple());
    if let Some(d) = &p.dedup_id {
        let rows = tx
            .fetch_all(&Statement::new(
                "SELECT x.message_id, coalesce(m.seq, 0) AS seq FROM _forgebase_queue_dedup x \
                 LEFT JOIN _forgebase_queue_messages m ON m.id = x.message_id \
                 WHERE x.topic = ?1 AND x.dedup_id = ?2 AND x.expires_at > ?3",
                vec![topic.name.as_str().into(), d.as_str().into(), now.into()],
            ))
            .await?;
        if let Some(r) = Row::all(&rows).next() {
            return Ok(Published {
                message_id: r.text("message_id"),
                seq: r.int("seq"),
                duplicate: true,
            });
        }
        tx.execute(&Statement::new(
            "INSERT INTO _forgebase_queue_dedup (topic, dedup_id, message_id, expires_at) VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT (topic, dedup_id) DO UPDATE SET message_id = excluded.message_id, \
             expires_at = excluded.expires_at",
            vec![
                topic.name.as_str().into(),
                d.as_str().into(),
                id.as_str().into(),
                (now + topic.dedup_window_ms).into(),
            ],
        ))
        .await?;
    }
    // `RETURNING seq`, not `last_insert_id` (PostgreSQL has none). Under the
    // single writer (ADR-0028 §7.3) the sequence order of messages is their
    // commit order, which the subscription cursors rely on (ADR-0022).
    let inserted = tx
        .fetch_all(&Statement::new(
            "INSERT INTO _forgebase_queue_messages (topic, id, data, attributes, ordering_key, dedup_id, bytes, \
             publisher, subject, metered, history, published_at, source_row) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13) RETURNING seq",
            vec![
                topic.name.as_str().into(),
                id.as_str().into(),
                p.data.as_str().into(),
                p.attributes.as_str().into(),
                p.ordering_key.clone().into(),
                p.dedup_id.clone().into(),
                p.bytes.into(),
                publisher.into(),
                subject.map(str::to_owned).into(),
                metered.into(),
                p.history.clone().into(),
                now.into(),
                p.source_row.clone().into(),
            ],
        ))
        .await?;
    tx.execute(&Statement::new(
        "UPDATE _forgebase_queue_topics SET published_total = published_total + 1, \
         published_bytes = published_bytes + ?2 WHERE name = ?1",
        vec![topic.name.as_str().into(), p.bytes.into()],
    ))
    .await?;
    bump_rate(tx, "topic", &topic.name, "published", now, 1).await?;
    Ok(Published {
        message_id: id,
        seq: inserted
            .scalar()
            .and_then(Value::as_i64)
            .ok_or_else(|| QueueError::Internal("message insert returned no seq".into()))?,
        duplicate: false,
    })
}

/// A quota reservation held while a publish runs.
struct Hold {
    feature: String,
    id: String,
}

impl QueueService {
    /// A queue service over the data core's database and authorizer.
    /// `keyring` seals webhook signing secrets.
    pub fn new(data: DataService, keyring: Keyring, settings: QueueSettings) -> Self {
        Self::with_clock(data, keyring, settings, Arc::new(SystemClock))
    }

    /// A queue service that reads time from `clock` (tests drive it by hand).
    pub fn with_clock(data: DataService, keyring: Keyring, settings: QueueSettings, clock: Arc<dyn Clock>) -> Self {
        let (wake, _) = watch::channel(0);
        let (closed, _) = watch::channel(false);
        let engine: Arc<crate::pgmq::EngineSlot> = Arc::new(tokio::sync::RwLock::new(None));
        let engines = [(forgebase_db::topology::CONTROL_SET_ID.to_owned(), engine.clone())]
            .into_iter()
            .collect();
        Self {
            set: None,
            engine,
            inner: Arc::new(Inner {
                data,
                clock,
                settings,
                keyring,
                ent: OnceLock::new(),
                functions: OnceLock::new(),
                fetcher: OnceLock::new(),
                wake,
                closed,
                background: Mutex::new(None),
                engines: Mutex::new(engines),
                router: OnceLock::new(),
                test_sends: Mutex::new(std::collections::HashMap::new()),
            }),
        }
    }

    /// Meters publishes through the quota engine (FB-24): `queue.messages`
    /// and `queue.bytes` of the publisher's billing subject.
    pub fn attach_entitlements(&self, ent: EntitlementService) {
        let _ = self.inner.ent.set(ent);
    }

    /// Enables push delivery to server functions (FB-60).
    pub fn attach_functions(&self, functions: FunctionService) {
        let _ = self.inner.functions.set(functions);
    }

    /// The SSRF-safe client for webhook push delivery.
    pub fn set_fetcher(&self, fetcher: Fetcher) {
        let _ = self.inner.fetcher.set(fetcher);
    }

    /// The settings.
    pub fn settings(&self) -> &QueueSettings {
        &self.inner.settings
    }

    /// Current time in Unix epoch milliseconds, from this service's clock.
    pub fn now(&self) -> i64 {
        self.inner.clock.now_ms()
    }

    pub(crate) fn backend(&self) -> &dyn Backend {
        match &self.set {
            Some(set) => set,
            None => self.inner.data.backend().as_ref(),
        }
    }

    pub(crate) fn allow(&self, ctx: &AccessContext, action: Action) -> Result<()> {
        Ok(self.inner.data.authorize_action(ctx, action)?)
    }

    /// Asks the authorizer the plain question (`engine:queue_<action>`,
    /// every topic and subscription) without doing anything.
    pub fn authorize(&self, ctx: &AccessContext, action: Action) -> Result<()> {
        self.allow(ctx, action)
    }

    /// Asks the authorizer about `action` (`op`) on topic `topic` without
    /// doing anything (the HTTP layer calls it before reading a body).
    pub fn authorize_topic(&self, ctx: &AccessContext, action: Action, topic: &str, op: ResourceOp) -> Result<()> {
        self.check(ctx, action, ResourceKind::Topic, topic, op, None)
    }

    /// Asks the authorizer about `action` (`op`) on subscription `sub`, which
    /// its owner holds every permission on; `SubscriptionNotFound` once the
    /// check passes and `sub` does not exist in the caller's scope.
    pub async fn authorize_subscription(
        &self,
        ctx: &AccessContext,
        action: Action,
        sub: &str,
        op: ResourceOp,
    ) -> Result<()> {
        self.at_sub(ctx.tenant.as_deref(), sub)
            .await?
            .authorize_subscription_here(ctx, action, sub, op)
            .await
    }

    /// [`Self::authorize_subscription`] on this view's set.
    pub(crate) async fn authorize_subscription_here(
        &self,
        ctx: &AccessContext,
        action: Action,
        sub: &str,
        op: ResourceOp,
    ) -> Result<()> {
        self.sub_for(&mut Db::Ro(self.backend()), ctx, action, op, sub)
            .await
            .map(|_| ())
    }

    /// Checks that `ctx` may stream subscription `sub` and that it is a pull
    /// subscription; returns its stored name.
    pub(crate) async fn stream_subscription(&self, ctx: &AccessContext, sub: &str) -> Result<String> {
        self.at_sub(ctx.tenant.as_deref(), sub)
            .await?
            .stream_subscription_here(ctx, sub)
            .await
    }

    /// [`Self::stream_subscription`] on this view's set.
    pub(crate) async fn stream_subscription_here(&self, ctx: &AccessContext, sub: &str) -> Result<String> {
        let s = self
            .sub_for(
                &mut Db::Ro(self.backend()),
                ctx,
                Action::QueueConsume,
                ResourceOp::List,
                sub,
            )
            .await?;
        if s.push.is_some() {
            return Err(QueueError::conflict(
                "push_subscription",
                format!("subscription {sub:?} delivers by push and cannot be streamed"),
            ));
        }
        Ok(s.name)
    }

    pub(crate) async fn begin(&self) -> Result<Box<dyn WriteTx>> {
        Ok(self.backend().begin(TxMode::Normal).await?)
    }

    /// Wakes long polls, streams and the push dispatcher.
    pub(crate) fn notify(&self) {
        self.inner.wake.send_modify(|v| *v = v.wrapping_add(1));
    }

    /// Ends every open WebSocket and SSE stream (their leases are released or
    /// expire); new streams end at once. The server calls it before graceful
    /// shutdown so open streams do not hold it up.
    pub fn close_streams(&self) {
        self.inner.closed.send_replace(true);
    }

    pub(crate) fn closed_rx(&self) -> watch::Receiver<bool> {
        self.inner.closed.subscribe()
    }

    /// A receiver that changes whenever new work may be available.
    pub(crate) fn wake_rx(&self) -> watch::Receiver<u64> {
        self.inner.wake.subscribe()
    }

    // ------------------------------------------------------------- topics

    /// Creates a topic.
    pub async fn create_topic(&self, ctx: &AccessContext, new: NewTopic) -> Result<Topic> {
        self.at_tenant(ctx.tenant.as_deref())
            .await?
            .create_topic_here(ctx, new)
            .await
    }

    /// [`Self::create_topic`] on this view's set.
    pub(crate) async fn create_topic_here(&self, ctx: &AccessContext, new: NewTopic) -> Result<Topic> {
        self.check(
            ctx,
            Action::QueueAdmin,
            ResourceKind::Topic,
            &new.name,
            ResourceOp::Create,
            None,
        )?;
        check_name("topic name", &new.name)?;
        check_labels(&new.labels)?;
        let s = &self.inner.settings;
        let retention = in_range(
            "retention_ms",
            new.retention_ms.unwrap_or(s.default_retention_ms),
            60_000,
            s.max_retention_ms,
        )?;
        let retention_bytes = match new.retention_bytes {
            None | Some(0) => None,
            Some(b) => Some(in_range("retention_bytes", b, 1024, i64::MAX)?),
        };
        let max_bytes = in_range(
            "max_message_bytes",
            new.max_message_bytes.unwrap_or(s.max_message_bytes),
            64,
            s.max_message_bytes,
        )?;
        let dedup = in_range(
            "dedup_window_ms",
            new.dedup_window_ms.unwrap_or(s.default_dedup_window_ms),
            1_000,
            s.max_dedup_window_ms,
        )?;
        let schema_text = match &new.schema {
            None => None,
            Some(sch) => Some(check_topic_schema(sch)?),
        };
        let description = check_description(new.description.as_deref())?;
        let example_text = match &new.example {
            None => None,
            Some(ex) => Some(check_example(ex, new.schema.as_ref())?),
        };
        let (source_collection, source_ops) = match &new.source {
            None => (None, None),
            Some(src) => {
                if !ctx.is_privileged() {
                    return Err(
                        forgebase_data::DataError::Forbidden("record sources need an administrator".into()).into(),
                    );
                }
                if new.schema.is_some() {
                    return Err(QueueError::invalid(
                        "invalid_topic",
                        "a topic fed by a record source cannot have a schema",
                    ));
                }
                (
                    Some(check_source(src)?),
                    Some(serde_json::to_string(&src.ops).unwrap_or_default()),
                )
            }
        };
        let tenant = ctx.tenant.clone();
        let key = stored(tenant.as_deref(), &new.name);
        let now = self.now();
        let mut tx = self.begin().await?;
        let r: Result<_> = async {
            // A tenant may not take a name an instance-wide topic holds.
            if topic_row(&mut Db::Tx(&mut *tx), &key).await?.is_some()
                || (tenant.is_some() && topic_row(&mut Db::Tx(&mut *tx), &new.name).await?.is_some())
            {
                return Err(QueueError::Exists {
                    kind: "topic",
                    name: new.name.clone(),
                });
            }
            // A record source relays changes committed from now on.
            let after = if source_collection.is_some() {
                scalar(
                    &tx.fetch_all(&Statement::raw(
                        "SELECT coalesce(max(seq), 0) FROM _forgebase_realtime_outbox",
                    ))
                    .await?,
                )
            } else {
                0
            };
            if source_collection.is_some() {
                // The relay starts here unless it already runs.
                tx.execute(&Statement::new(
                    "INSERT INTO _forgebase_queue_state (key, value) VALUES ('outbox_cursor', ?1) \
                     ON CONFLICT (key) DO NOTHING",
                    vec![after.into()],
                ))
                .await?;
            }
            tx.execute(&Statement::new(
                "INSERT INTO _forgebase_queue_topics (name, retention_ms, retention_bytes, max_message_bytes, \
                 dedup_window_ms, message_schema, source_collection, source_ops, source_after_seq, labels, \
                 created_by, created_at, updated_at, tenant, description, example) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12, ?13, ?14, ?15)",
                vec![
                    key.as_str().into(),
                    retention.into(),
                    retention_bytes.into(),
                    max_bytes.into(),
                    dedup.into(),
                    schema_text.into(),
                    source_collection.into(),
                    source_ops.into(),
                    after.into(),
                    serde_json::to_string(&new.labels).unwrap_or_default().into(),
                    ctx.actor().into(),
                    now.into(),
                    tenant.clone().into(),
                    description.clone().into(),
                    example_text.clone().into(),
                ],
            ))
            .await?;
            topic_row(&mut Db::Tx(&mut *tx), &key)
                .await?
                .ok_or_else(|| QueueError::Internal("topic vanished".into()))
        }
        .await;
        Ok(finish(tx, r).await?.public())
    }

    /// One topic.
    pub async fn topic(&self, ctx: &AccessContext, name: &str) -> Result<Topic> {
        self.at_topic(ctx.tenant.as_deref(), name)
            .await?
            .topic_here(ctx, name)
            .await
    }

    /// [`Self::topic`] on this view's set.
    pub(crate) async fn topic_here(&self, ctx: &AccessContext, name: &str) -> Result<Topic> {
        self.check(
            ctx,
            Action::QueueRead,
            ResourceKind::Topic,
            name,
            ResourceOp::View,
            None,
        )?;
        Ok(self.topic_for(ctx, name).await?.public())
    }

    /// The topic `name` resolves to for `ctx` (read pool).
    pub(crate) async fn topic_for(&self, ctx: &AccessContext, name: &str) -> Result<TopicRow> {
        resolve_topic(&mut Db::Ro(self.backend()), ctx.tenant.as_deref(), name).await
    }

    /// The topics of the caller's scope (its tenant's and the instance-wide
    /// ones), by name: every one with `engine:queue_read`, else those the
    /// caller may list (resource keys and rules).
    pub async fn topics(&self, ctx: &AccessContext) -> Result<Vec<Topic>> {
        self.at_tenant(ctx.tenant.as_deref()).await?.topics_here(ctx).await
    }

    /// [`Self::topics`] on this view's set.
    pub(crate) async fn topics_here(&self, ctx: &AccessContext) -> Result<Vec<Topic>> {
        let plain = self.allow(ctx, Action::QueueRead);
        let rows = match &ctx.tenant {
            Some(t) => {
                self.fetch_listing(&Statement::new(
                    format!(
                        "SELECT {TOPIC_COLS} FROM _forgebase_queue_topics WHERE tenant = ?1 OR tenant IS NULL \
                             ORDER BY name"
                    ),
                    vec![t.as_str().into()],
                ))
                .await?
            }
            None => {
                self.fetch_listing(&Statement::raw(format!(
                    "SELECT {TOPIC_COLS} FROM _forgebase_queue_topics WHERE tenant IS NULL ORDER BY name"
                )))
                .await?
            }
        };
        let rows: Vec<TopicRow> = Row::all(&rows).map(|r| TopicRow::from_row(&r)).collect::<Result<_>>()?;
        let mut out: Vec<Topic> = scoped(rows, |t| t.name.as_str(), |t| t.tenant.as_deref())
            .into_iter()
            .filter(|t| {
                plain.is_ok()
                    || self
                        .check(
                            ctx,
                            Action::QueueRead,
                            ResourceKind::Topic,
                            bare(&t.name),
                            ResourceOp::List,
                            None,
                        )
                        .is_ok()
            })
            .map(|t| t.public())
            .collect();
        if out.is_empty() {
            plain?;
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// Changes a topic's limits, schema or labels.
    pub async fn update_topic(&self, ctx: &AccessContext, name: &str, patch: TopicPatch) -> Result<Topic> {
        self.at_topic(ctx.tenant.as_deref(), name)
            .await?
            .update_topic_here(ctx, name, patch)
            .await
    }

    /// [`Self::update_topic`] on this view's set.
    pub(crate) async fn update_topic_here(&self, ctx: &AccessContext, name: &str, patch: TopicPatch) -> Result<Topic> {
        self.check(
            ctx,
            Action::QueueAdmin,
            ResourceKind::Topic,
            name,
            ResourceOp::Update,
            None,
        )?;
        let s = &self.inner.settings;
        let mut sets: Vec<String> = Vec::new();
        let mut params: Vec<Value> = vec![name.into()];
        let mut set = |col: &str, v: Value| {
            params.push(v);
            sets.push(format!("{col} = ?{}", params.len()));
        };
        if let Some(v) = patch.retention_ms {
            set(
                "retention_ms",
                in_range("retention_ms", v, 60_000, s.max_retention_ms)?.into(),
            );
        }
        if let Some(v) = patch.retention_bytes {
            let v = if v == 0 {
                None
            } else {
                Some(in_range("retention_bytes", v, 1024, i64::MAX)?)
            };
            set("retention_bytes", v.into());
        }
        if let Some(v) = patch.max_message_bytes {
            set(
                "max_message_bytes",
                in_range("max_message_bytes", v, 64, s.max_message_bytes)?.into(),
            );
        }
        if let Some(v) = patch.dedup_window_ms {
            set(
                "dedup_window_ms",
                in_range("dedup_window_ms", v, 1_000, s.max_dedup_window_ms)?.into(),
            );
        }
        if patch.clear_schema {
            set("message_schema", Value::Null);
        } else if let Some(sch) = &patch.schema {
            set("message_schema", check_topic_schema(sch)?.into());
        }
        if let Some(labels) = &patch.labels {
            check_labels(labels)?;
            set("labels", serde_json::to_string(labels).unwrap_or_default().into());
        }
        if let Some(d) = &patch.description {
            set("description", check_description(Some(d))?.into());
        }
        if patch.clear_example {
            set("example", Value::Null);
        } else if let Some(ex) = &patch.example {
            if ex.to_string().len() > crate::model::MAX_EXAMPLE_BYTES {
                return Err(QueueError::invalid(
                    "invalid_example",
                    format!("the example is larger than {} bytes", crate::model::MAX_EXAMPLE_BYTES),
                ));
            }
            set("example", ex.to_string().into());
        }
        let now = self.now();
        set("updated_at", now.into());
        let mut tx = self.begin().await?;
        let r: Result<_> = async {
            let t = resolve_topic(&mut Db::Tx(&mut *tx), ctx.tenant.as_deref(), name).await?;
            // The example must satisfy the schema the topic has afterwards.
            let schema_after = if patch.clear_schema {
                None
            } else {
                patch.schema.as_ref().or(t.schema.as_ref())
            };
            let example_after = if patch.clear_example {
                None
            } else {
                patch.example.as_ref().or(t.example.as_ref())
            };
            if let Some(ex) = example_after {
                check_example(ex, schema_after)?;
            }
            let mut params = params;
            params[0] = t.name.as_str().into();
            if t.source.is_some() && patch.schema.is_some() {
                return Err(QueueError::invalid(
                    "invalid_topic",
                    "a topic fed by a record source cannot have a schema",
                ));
            }
            tx.execute(&Statement::new(
                format!("UPDATE _forgebase_queue_topics SET {} WHERE name = ?1", sets.join(", ")),
                params,
            ))
            .await?;
            topic_row(&mut Db::Tx(&mut *tx), &t.name)
                .await?
                .ok_or_else(|| QueueError::Internal("topic vanished".into()))
        }
        .await;
        Ok(finish(tx, r).await?.public())
    }

    /// Deletes a topic and its messages. A topic with subscriptions is only
    /// deleted with `force`, which deletes them too.
    pub async fn delete_topic(&self, ctx: &AccessContext, name: &str, force: bool) -> Result<()> {
        self.at_topic(ctx.tenant.as_deref(), name)
            .await?
            .delete_topic_here(ctx, name, force)
            .await
    }

    /// [`Self::delete_topic`] on this view's set.
    pub(crate) async fn delete_topic_here(&self, ctx: &AccessContext, name: &str, force: bool) -> Result<()> {
        self.check(
            ctx,
            Action::QueueAdmin,
            ResourceKind::Topic,
            name,
            ResourceOp::Delete,
            None,
        )?;
        let eng = self.store().await?;
        let mut tx = self.begin().await?;
        let r: Result<_> = async {
            let shown = name;
            let name = resolve_topic(&mut Db::Tx(&mut *tx), ctx.tenant.as_deref(), name)
                .await?
                .name;
            let name = name.as_str();
            let subs = scalar(
                &tx.fetch_all(&Statement::new(
                    "SELECT COUNT(*) FROM _forgebase_queue_subscriptions WHERE topic = ?1",
                    vec![name.into()],
                ))
                .await?,
            );
            if subs > 0 && !force {
                return Err(QueueError::conflict(
                    "topic_has_subscriptions",
                    format!("topic {shown:?} has {subs} subscription(s); delete them first or pass force"),
                ));
            }
            if subs > 0 && *eng == crate::model::DeliveryEngine::Pgmq {
                let rows = tx
                    .fetch_all(&Statement::new(
                        "SELECT name FROM _forgebase_queue_subscriptions WHERE topic = ?1",
                        vec![name.into()],
                    ))
                    .await?;
                for r in Row::all(&rows) {
                    crate::pgmq::drop_queue(&mut *tx, &r.text("name")).await?;
                }
            }
            tx.execute(&Statement::new(
                "DELETE FROM _forgebase_queue_topics WHERE name = ?1",
                vec![name.into()],
            ))
            .await?;
            tx.execute(&Statement::new(
                "DELETE FROM _forgebase_queue_rates WHERE scope = 'topic' AND name = ?1",
                vec![name.into()],
            ))
            .await?;
            Ok(())
        }
        .await;
        finish(tx, r).await
    }

    // ----------------------------------------------------------- publish

    fn check_batch(&self, messages: &[NewMessage]) -> Result<()> {
        let max = self.inner.settings.max_batch;
        if messages.is_empty() || messages.len() > max {
            return Err(QueueError::invalid(
                "invalid_batch",
                format!("publish 1 to {max} messages at a time"),
            ));
        }
        Ok(())
    }

    /// The billing subject of a publisher, when publishes are metered.
    fn billable(&self, ctx: &AccessContext) -> Option<String> {
        self.inner.ent.get()?;
        if ctx.is_privileged() {
            return None;
        }
        let s = subject_key(&ctx.subject());
        parse_subject(&s).is_ok().then_some(s)
    }

    fn features(&self) -> Vec<(&str, bool)> {
        let s = &self.inner.settings;
        let mut out = Vec::new();
        if let Some(f) = s.messages_feature.as_deref() {
            out.push((f, true));
        }
        if let Some(f) = s.bytes_feature.as_deref() {
            out.push((f, false));
        }
        out
    }

    /// Reserves quota for `count` messages and `bytes` bytes.
    async fn hold(&self, subject: &str, count: i64, bytes: i64) -> Result<Vec<Hold>> {
        let Some(ent) = self.inner.ent.get() else {
            return Ok(Vec::new());
        };
        let mut holds: Vec<Hold> = Vec::new();
        for (feature, is_count) in self.features() {
            let units = if is_count { count } else { bytes };
            let key = format!("queue:pub:{}", uuid::Uuid::new_v4().simple());
            let req = UsageRequest::new(feature, units.max(1), key).for_subject(subject);
            match ent.reserve(&AccessContext::system(), req).await {
                Ok(r) => holds.push(Hold {
                    feature: feature.to_owned(),
                    id: r.id,
                }),
                // Not in the catalog: this feature is not metered here.
                Err(EntitlementError::NotFound(_)) => {}
                Err(e) => {
                    self.release_holds(&holds).await;
                    return Err(e.into());
                }
            }
        }
        Ok(holds)
    }

    async fn release_holds(&self, holds: &[Hold]) {
        let Some(ent) = self.inner.ent.get() else { return };
        for h in holds {
            if let Err(e) = ent.release(&AccessContext::system(), &h.id).await {
                tracing::warn!(error = %e, reservation = %h.id, "queue quota reservation not released");
            }
        }
    }

    async fn settle_holds(&self, holds: &[Hold], count: i64, bytes: i64) {
        let Some(ent) = self.inner.ent.get() else { return };
        let msg_feature = self.inner.settings.messages_feature.as_deref();
        for h in holds {
            let units = if Some(h.feature.as_str()) == msg_feature {
                count
            } else {
                bytes
            };
            let r = if units > 0 {
                ent.commit(&AccessContext::system(), &h.id, Some(units)).await
            } else {
                ent.release(&AccessContext::system(), &h.id).await
            };
            if let Err(e) = r {
                tracing::warn!(error = %e, reservation = %h.id, "queue usage not recorded");
            }
        }
    }

    /// Dry-run quota check (read pool only, safe inside a caller's transaction).
    async fn precheck(&self, subject: &str, count: i64, bytes: i64) -> Result<()> {
        let Some(ent) = self.inner.ent.get() else {
            return Ok(());
        };
        for (feature, is_count) in self.features() {
            let units = if is_count { count } else { bytes };
            let req = UsageRequest {
                feature: feature.to_owned(),
                units: units.max(1),
                subject: Some(subject.to_owned()),
                ..UsageRequest::default()
            };
            match ent.check(&AccessContext::system(), &req).await {
                Ok(d) if d.allowed => {}
                Ok(d) => return Err(EntitlementError::QuotaExceeded(Box::new(d)).into()),
                Err(EntitlementError::NotFound(_)) => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }

    /// Messages and bytes of a batch that are not duplicates as far as a read
    /// shows now (duplicates use no quota).
    async fn expected_fresh(&self, t: &TopicRow, prepared: &[Prepared], now: i64) -> Result<(i64, i64)> {
        let ids: Vec<&str> = prepared.iter().filter_map(|p| p.dedup_id.as_deref()).collect();
        let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        if !ids.is_empty() {
            let marks: Vec<String> = (0..ids.len()).map(|i| format!("?{}", i + 3)).collect();
            let mut params: Vec<Value> = vec![t.name.as_str().into(), now.into()];
            params.extend(ids.iter().map(|d| Value::from(*d)));
            let rows = self
                .backend()
                .fetch_all(&Statement::new(
                    format!(
                        "SELECT dedup_id FROM _forgebase_queue_dedup WHERE topic = ?1 AND expires_at > ?2 \
                         AND dedup_id IN ({})",
                        marks.join(", ")
                    ),
                    params,
                ))
                .await?;
            seen.extend(Row::all(&rows).map(|r| r.text("dedup_id")));
        }
        let (mut count, mut bytes) = (0, 0);
        for p in prepared {
            if let Some(d) = &p.dedup_id
                && !seen.insert(d.clone())
            {
                continue;
            }
            count += 1;
            bytes += p.bytes;
        }
        Ok((count, bytes))
    }

    fn prepare_all(topic: &TopicRow, messages: &[NewMessage]) -> Result<Vec<Prepared>> {
        if topic.source.is_some() {
            return Err(QueueError::conflict(
                "topic_has_source",
                format!("topic {:?} is fed by record changes of a collection", topic.name),
            ));
        }
        messages.iter().map(|m| prepare(topic, m, false)).collect()
    }

    /// Publishes 1 to `max_batch` messages in one transaction. A message whose
    /// dedup id was seen inside the topic's window is not stored again
    /// (`duplicate: true`). Usage is metered through the quota engine when one
    /// is attached; a refusal publishes nothing.
    pub async fn publish(&self, ctx: &AccessContext, topic: &str, messages: Vec<NewMessage>) -> Result<Vec<Published>> {
        self.at_topic(ctx.tenant.as_deref(), topic)
            .await?
            .publish_here(ctx, topic, messages)
            .await
    }

    /// [`Self::publish`] on this view's set: a producer span `publish
    /// <topic>` whose `traceparent` (and `tracestate`) each message carries
    /// as attributes while trace propagation is on, unless the publisher set
    /// them (ADR-0035 §2). Consumers continue the trace from there.
    pub(crate) async fn publish_here(
        &self,
        ctx: &AccessContext,
        topic: &str,
        mut messages: Vec<NewMessage>,
    ) -> Result<Vec<Published>> {
        use tracing::Instrument as _;
        let span = tracing::info_span!(
            "queue.publish",
            topic = %topic,
            otel.name = %format_args!("publish {topic}"),
            otel.kind = "producer",
            messaging.system = "forgebase",
            messaging.operation.type = "send",
            messaging.destination.name = %topic,
            messaging.batch.message_count = messages.len(),
            otel.status_code = tracing::field::Empty,
        );
        if forgebase_observability::trace::propagation_enabled()
            && let Some(c) = forgebase_observability::trace::span_context(&span)
        {
            stamp_trace(&mut messages, &c.traceparent(), c.trace_state.as_deref());
        }
        let r = self
            .publish_traced(ctx, topic, messages)
            .instrument(span.clone())
            .await;
        if r.is_err() {
            span.record("otel.status_code", "error");
        }
        r
    }

    async fn publish_traced(
        &self,
        ctx: &AccessContext,
        topic: &str,
        messages: Vec<NewMessage>,
    ) -> Result<Vec<Published>> {
        self.check(
            ctx,
            Action::QueuePublish,
            ResourceKind::Topic,
            topic,
            ResourceOp::Create,
            None,
        )?;
        self.check_batch(&messages)?;
        let t = self.topic_for(ctx, topic).await?;
        let prepared = Self::prepare_all(&t, &messages)?;
        let subject = self.billable(ctx);
        let now = self.now();
        let holds = match &subject {
            Some(s) => {
                let (count, bytes) = self.expected_fresh(&t, &prepared, now).await?;
                if count > 0 {
                    self.hold(s, count, bytes).await?
                } else {
                    Vec::new()
                }
            }
            None => Vec::new(),
        };
        let actor = ctx.actor();
        let mut tx = self.begin().await?;
        let r: Result<_> = async {
            // The topic may have been deleted or changed since it was read.
            let t = topic_row(&mut Db::Tx(&mut *tx), &t.name)
                .await?
                .ok_or_else(|| QueueError::TopicNotFound(topic.to_owned()))?;
            let mut out = Vec::with_capacity(prepared.len());
            for p in &prepared {
                out.push(insert_message_tx(&mut *tx, &t, p, &actor, subject.as_deref(), 1, now).await?);
            }
            Ok(out)
        }
        .await;
        match finish(tx, r).await {
            Ok(out) => {
                let fresh: Vec<_> = out.iter().zip(&prepared).filter(|(o, _)| !o.duplicate).collect();
                let bytes: i64 = fresh.iter().map(|(_, p)| p.bytes).sum();
                self.settle_holds(&holds, i64::try_from(fresh.len()).unwrap_or(0), bytes)
                    .await;
                metrics::counter!("forgebase_queue_published_total", "topic" => t.name.clone())
                    .increment(fresh.len() as u64);
                self.notify();
                Ok(out)
            }
            Err(e) => {
                self.release_holds(&holds).await;
                Err(e)
            }
        }
    }

    /// Publishes inside the caller's write transaction: the messages exist
    /// exactly when `tx` commits. Call [`QueueService::committed`] after the
    /// commit so consumers wake at once (otherwise they see the messages
    /// within one poll interval). Quota is checked here and usage is recorded
    /// after the commit by the background task.
    pub async fn publish_in_tx(
        &self,
        tx: &mut dyn WriteTx,
        ctx: &AccessContext,
        topic: &str,
        mut messages: Vec<NewMessage>,
    ) -> Result<Vec<Published>> {
        if let Some((tp, ts)) = forgebase_observability::trace::current_traceparent() {
            stamp_trace(&mut messages, &tp, ts.as_deref());
        }
        self.check(
            ctx,
            Action::QueuePublish,
            ResourceKind::Topic,
            topic,
            ResourceOp::Create,
            None,
        )?;
        self.check_batch(&messages)?;
        let t = resolve_topic(&mut Db::Tx(&mut *tx), ctx.tenant.as_deref(), topic).await?;
        let prepared = Self::prepare_all(&t, &messages)?;
        let subject = self.billable(ctx);
        let metered = if let Some(s) = &subject {
            let total: i64 = prepared.iter().map(|p| p.bytes).sum();
            self.precheck(s, i64::try_from(prepared.len()).unwrap_or(i64::MAX), total)
                .await?;
            0
        } else {
            1
        };
        let now = self.now();
        let actor = ctx.actor();
        let mut out = Vec::with_capacity(prepared.len());
        for p in &prepared {
            out.push(insert_message_tx(tx, &t, p, &actor, subject.as_deref(), metered, now).await?);
        }
        Ok(out)
    }

    /// Tells the service that a transaction with [`QueueService::publish_in_tx`]
    /// committed: wakes consumers and the metering pass.
    pub fn committed(&self) {
        self.notify();
    }

    /// Retained messages of a topic, oldest first, after sequence `after`.
    /// Record-change messages carry the record as the caller sees it now;
    /// those whose record the caller may not see are left out, so a page may
    /// hold fewer than `limit` items (follow `next_after`).
    pub async fn messages(&self, ctx: &AccessContext, topic: &str, after: i64, limit: i64) -> Result<MessagePage> {
        self.at_topic(ctx.tenant.as_deref(), topic)
            .await?
            .messages_here(ctx, topic, after, limit)
            .await
    }

    /// [`Self::messages`] on this view's set.
    pub(crate) async fn messages_here(
        &self,
        ctx: &AccessContext,
        topic: &str,
        after: i64,
        limit: i64,
    ) -> Result<MessagePage> {
        self.check(
            ctx,
            Action::QueueRead,
            ResourceKind::Topic,
            topic,
            ResourceOp::List,
            None,
        )?;
        let t = self.topic_for(ctx, topic).await?;
        let limit = limit.clamp(1, 500);
        let rows = self
            .read_replica(
                "queue.messages",
                &Statement::new(
                    format!(
                        "SELECT {MESSAGE_COLS} FROM _forgebase_queue_messages m WHERE m.topic = ?1 AND m.seq > ?2 \
                         AND m.only_subscription IS NULL ORDER BY m.seq LIMIT ?3"
                    ),
                    vec![t.name.as_str().into(), after.into(), (limit + 1).into()],
                ),
            )
            .await?;
        let page = usize::try_from(limit).unwrap_or(500);
        let all: Vec<Row<'_>> = Row::all(&rows).collect();
        let next_after = (all.len() > page).then(|| all[page - 1].int("seq"));
        let mut items = Vec::with_capacity(page.min(all.len()));
        for r in all.iter().take(page) {
            let mut m = message_from(r)?;
            if let Some(src) = r.opt_text("source_row")
                && !self.view_record(ctx, &mut m.data, &src).await?
            {
                continue;
            }
            items.push(m);
        }
        Ok(MessagePage { items, next_after })
    }

    // ------------------------------------------------------ subscriptions

    fn check_push(&self, p: &PushConfig) -> Result<()> {
        match p.kind {
            PushKind::Webhook => {
                let bad = |m: &str| QueueError::invalid("invalid_push_endpoint", m.to_owned());
                if p.endpoint.len() > 2048 {
                    return Err(bad("the webhook URL is longer than 2048 bytes"));
                }
                let url = url::Url::parse(&p.endpoint).map_err(|_| bad("the webhook URL does not parse"))?;
                match url.scheme() {
                    "https" => {}
                    "http" if self.inner.settings.allow_http_push => {}
                    _ => return Err(bad("webhook URLs must be https://")),
                }
                if !url.username().is_empty() || url.password().is_some() {
                    return Err(bad("webhook URLs may not carry credentials"));
                }
                let ip = match url.host() {
                    None => return Err(bad("the webhook URL has no host")),
                    Some(url::Host::Ipv4(a)) => Some(std::net::IpAddr::V4(a)),
                    Some(url::Host::Ipv6(a)) => Some(std::net::IpAddr::V6(a)),
                    Some(url::Host::Domain(_)) => None,
                };
                if let Some(ip) = ip
                    && let Some(why) = forgebase_functions::fetch::blocked_reason(ip)
                {
                    return Err(QueueError::invalid(
                        "push_endpoint_blocked",
                        format!("{ip} is in {why}; push endpoints must be public addresses"),
                    ));
                }
                Ok(())
            }
            PushKind::Function => check_name("function name", &p.endpoint),
        }
    }

    /// A fresh webhook signing secret, plain and sealed for storage.
    pub(crate) fn new_secret(&self, sub: &str) -> Result<(String, Vec<u8>)> {
        let plain = forgebase_auth::crypto::random_token(32).map_err(|e| QueueError::Internal(e.to_string()))?;
        let sealed = self
            .inner
            .keyring
            .seal(plain.as_bytes(), format!("queue:push:{sub}").as_bytes())
            .map_err(|e| QueueError::Internal(e.to_string()))?;
        Ok((format!("whsec_{plain}"), sealed))
    }

    /// The webhook signing secret of a subscription.
    pub(crate) fn open_secret(&self, sub: &str, sealed: &[u8]) -> Result<String> {
        let plain = self
            .inner
            .keyring
            .open(sealed, format!("queue:push:{sub}").as_bytes())
            .map_err(|e| QueueError::Internal(format!("push secret of {sub:?}: {e}")))?;
        let text = String::from_utf8(plain).map_err(|e| QueueError::Internal(e.to_string()))?;
        Ok(format!("whsec_{text}"))
    }

    /// Creates a subscription. A webhook push subscription's signing secret is
    /// in the response (`push_secret`) and never shown again.
    pub async fn create_subscription(&self, ctx: &AccessContext, new: NewSubscription) -> Result<Subscription> {
        self.at_topic(ctx.tenant.as_deref(), &new.topic)
            .await?
            .create_subscription_here(ctx, new)
            .await
    }

    /// [`Self::create_subscription`] on this view's set.
    pub(crate) async fn create_subscription_here(
        &self,
        ctx: &AccessContext,
        new: NewSubscription,
    ) -> Result<Subscription> {
        self.check(
            ctx,
            Action::QueueAdmin,
            ResourceKind::Subscription,
            &new.name,
            ResourceOp::Create,
            None,
        )?;
        // A subscription receives its topic's messages and publishes to its
        // dead-letter topic: the creator must be allowed both.
        self.check(
            ctx,
            Action::QueueRead,
            ResourceKind::Topic,
            &new.topic,
            ResourceOp::List,
            None,
        )?;
        if let Some(d) = &new.dead_letter_topic {
            self.check(
                ctx,
                Action::QueuePublish,
                ResourceKind::Topic,
                d,
                ResourceOp::Create,
                None,
            )?;
        }
        check_name("subscription name", &new.name)?;
        check_name("topic name", &new.topic)?;
        check_labels(&new.labels)?;
        let filter_text = match &new.filter {
            None => None,
            Some(f) => {
                Filter::parse(f).map_err(|e| QueueError::invalid("invalid_filter", e))?;
                Some(f.to_string())
            }
        };
        let deadline = in_range(
            "ack_deadline_ms",
            new.ack_deadline_ms.unwrap_or(10_000),
            crate::model::MIN_ACK_DEADLINE_MS,
            crate::model::MAX_ACK_DEADLINE_MS,
        )?;
        let attempts = match new.max_delivery_attempts {
            None | Some(0) => None,
            Some(n) => Some(in_range(
                "max_delivery_attempts",
                n,
                1,
                crate::model::MAX_DELIVERY_ATTEMPTS,
            )?),
        };
        let base = in_range("backoff_base_ms", new.backoff_base_ms.unwrap_or(1_000), 0, 600_000)?;
        let cap = in_range("backoff_max_ms", new.backoff_max_ms.unwrap_or(600_000), base, 3_600_000)?;
        if let Some(d) = &new.dead_letter_topic {
            check_name("dead_letter_topic", d)?;
            if *d == new.topic {
                return Err(QueueError::invalid(
                    "invalid_dead_letter_topic",
                    "the dead-letter topic must differ from the subscription's topic",
                ));
            }
        }
        if let Some(p) = &new.push {
            self.check_push(p)?;
        }
        let start_ms = match &new.start {
            Some(StartPosition::Time(t)) => Some(parse_time("start.time", t)?),
            _ => None,
        };
        let pause_after = new
            .pause_after_failures
            .map(|n| in_range("pause_after_failures", n, 1, crate::model::MAX_PAUSE_AFTER_FAILURES))
            .transpose()?;
        let ttl = new
            .ttl_ms
            .map(|t| {
                in_range(
                    "ttl_ms",
                    t,
                    crate::model::MIN_SUBSCRIPTION_TTL_MS,
                    crate::model::MAX_SUBSCRIPTION_TTL_MS,
                )
            })
            .transpose()?;
        let tenant = ctx.tenant.clone();
        let key = stored(tenant.as_deref(), &new.name);
        let secret = match &new.push {
            Some(p) if p.kind == PushKind::Webhook => Some(self.new_secret(&key)?),
            _ => None,
        };
        let owner = OwnerContext::of(ctx).to_text();
        let now = self.now();
        let eng = self.store().await?;
        let mut tx = self.begin().await?;
        let r: Result<_> = async {
            let topic = resolve_topic(&mut Db::Tx(&mut *tx), tenant.as_deref(), &new.topic).await?;
            let dead_letter = match &new.dead_letter_topic {
                Some(d) => Some(resolve_topic(&mut Db::Tx(&mut *tx), tenant.as_deref(), d).await?.name),
                None => None,
            };
            // A tenant may not take a name an instance-wide subscription holds.
            let exists = tx
                .fetch_all(&Statement::new(
                    "SELECT 1 FROM _forgebase_queue_subscriptions WHERE name = ?1 OR name = ?2",
                    vec![
                        key.as_str().into(),
                        if tenant.is_some() {
                            new.name.as_str()
                        } else {
                            key.as_str()
                        }
                        .into(),
                    ],
                ))
                .await?;
            if !exists.is_empty() {
                return Err(QueueError::Exists {
                    kind: "subscription",
                    name: new.name.clone(),
                });
            }
            let cursor = match (&new.start, start_ms) {
                (Some(StartPosition::Earliest), _) => 0,
                (Some(StartPosition::Time(_)), Some(ts)) => cursor_before(&mut *tx, &topic.name, ts).await?,
                _ => scalar(
                    &tx.fetch_all(&Statement::raw(
                        "SELECT coalesce(max(seq), 0) FROM _forgebase_queue_messages",
                    ))
                    .await?,
                ),
            };
            tx.execute(&Statement::new(
                "INSERT INTO _forgebase_queue_subscriptions (name, topic, filter, ack_deadline_ms, \
                 max_delivery_attempts, backoff_base_ms, backoff_max_ms, dead_letter_topic, ordering, push_kind, \
                 push_endpoint, push_secret, cursor_seq, labels, created_by, created_at, updated_at, tenant, \
                 owner_context, pause_after_failures, expires_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?16, ?17, ?18, ?19, ?20)",
                vec![
                    key.as_str().into(),
                    topic.name.as_str().into(),
                    filter_text.clone().into(),
                    deadline.into(),
                    attempts.into(),
                    base.into(),
                    cap.into(),
                    dead_letter.into(),
                    new.ordering.into(),
                    new.push.as_ref().map(|p| p.kind.as_str().to_owned()).into(),
                    new.push.as_ref().map(|p| p.endpoint.clone()).into(),
                    secret.as_ref().map_or(Value::Null, |(_, s)| Value::Blob(s.clone())),
                    cursor.into(),
                    serde_json::to_string(&new.labels).unwrap_or_default().into(),
                    ctx.actor().into(),
                    now.into(),
                    tenant.clone().into(),
                    owner.as_str().into(),
                    pause_after.into(),
                    ttl.map(|t| now + t).into(),
                ],
            ))
            .await?;
            if *eng == crate::model::DeliveryEngine::Pgmq {
                crate::pgmq::create_queue(&mut *tx, &key, new.ordering).await?;
            }
            sub_row(&mut Db::Tx(&mut *tx), &key).await
        }
        .await;
        let mut sub = finish(tx, r).await?.public();
        drop(eng);
        sub.push_secret = secret.map(|(plain, _)| plain);
        if sub.push.is_some() {
            self.notify();
        }
        Ok(sub)
    }

    /// The subscription `name` resolves to for `ctx`, once `ctx` may do
    /// `action` (`op`) on it: its owner holds every permission on it. A caller
    /// without access gets `401`/`403` whether it exists or not.
    pub(crate) async fn sub_for(
        &self,
        db: &mut Db<'_>,
        ctx: &AccessContext,
        action: Action,
        op: ResourceOp,
        name: &str,
    ) -> Result<SubRow> {
        let found = find_sub(db, ctx.tenant.as_deref(), name).await?;
        self.check(
            ctx,
            action,
            ResourceKind::Subscription,
            name,
            op,
            found.as_ref().map(|s| s.created_by.as_str()),
        )?;
        found.ok_or_else(|| QueueError::SubscriptionNotFound(name.to_owned()))
    }

    /// One subscription.
    pub async fn subscription(&self, ctx: &AccessContext, name: &str) -> Result<Subscription> {
        self.at_sub(ctx.tenant.as_deref(), name)
            .await?
            .subscription_here(ctx, name)
            .await
    }

    /// [`Self::subscription`] on this view's set.
    pub(crate) async fn subscription_here(&self, ctx: &AccessContext, name: &str) -> Result<Subscription> {
        let s = self
            .sub_for(
                &mut Db::Ro(self.backend()),
                ctx,
                Action::QueueRead,
                ResourceOp::View,
                name,
            )
            .await?;
        Ok(s.public())
    }

    /// Subscriptions of the caller's scope by name, optionally of one topic:
    /// every one with `engine:queue_read`, else those the caller owns or may
    /// list (resource keys and rules).
    pub async fn subscriptions(&self, ctx: &AccessContext, topic: Option<&str>) -> Result<Vec<Subscription>> {
        let view = match topic {
            Some(t) => self.at_topic(ctx.tenant.as_deref(), t).await?,
            None => self.at_tenant(ctx.tenant.as_deref()).await?,
        };
        view.subscriptions_here(ctx, topic).await
    }

    /// [`Self::subscriptions`] on this view's set.
    pub(crate) async fn subscriptions_here(
        &self,
        ctx: &AccessContext,
        topic: Option<&str>,
    ) -> Result<Vec<Subscription>> {
        let plain = self.allow(ctx, Action::QueueRead);
        let topic = match topic {
            Some(t) => match find_topic(&mut Db::Ro(self.backend()), ctx.tenant.as_deref(), t).await? {
                Some(row) => Some(row.name),
                None => {
                    plain?;
                    return Ok(Vec::new());
                }
            },
            None => None,
        };
        let rows = self
            .fetch_listing(&Statement::new(
                format!(
                    "SELECT {SUB_COLS} FROM _forgebase_queue_subscriptions \
                     WHERE (tenant IS NULL OR tenant = ?1) AND (CAST(?2 AS TEXT) IS NULL OR topic = ?2) ORDER BY name"
                ),
                vec![ctx.tenant.clone().into(), topic.into()],
            ))
            .await?;
        let rows: Vec<SubRow> = Row::all(&rows).map(|r| SubRow::from_row(&r)).collect::<Result<_>>()?;
        let mut out: Vec<Subscription> = scoped(rows, |s| s.name.as_str(), |s| s.tenant.as_deref())
            .into_iter()
            .filter(|s| {
                plain.is_ok()
                    || self
                        .check(
                            ctx,
                            Action::QueueRead,
                            ResourceKind::Subscription,
                            bare(&s.name),
                            ResourceOp::List,
                            Some(&s.created_by),
                        )
                        .is_ok()
            })
            .map(|s| s.public())
            .collect();
        if out.is_empty() {
            plain?;
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// Changes a subscription. A new webhook endpoint gets a new signing
    /// secret, returned in this response only.
    pub async fn update_subscription(
        &self,
        ctx: &AccessContext,
        name: &str,
        patch: SubscriptionPatch,
    ) -> Result<Subscription> {
        self.at_sub(ctx.tenant.as_deref(), name)
            .await?
            .update_subscription_here(ctx, name, patch)
            .await
    }

    /// [`Self::update_subscription`] on this view's set.
    pub(crate) async fn update_subscription_here(
        &self,
        ctx: &AccessContext,
        name: &str,
        patch: SubscriptionPatch,
    ) -> Result<Subscription> {
        let found = self
            .sub_for(
                &mut Db::Ro(self.backend()),
                ctx,
                Action::QueueAdmin,
                ResourceOp::Update,
                name,
            )
            .await?;
        if let Some(d) = &patch.dead_letter_topic {
            self.check(
                ctx,
                Action::QueuePublish,
                ResourceKind::Topic,
                d,
                ResourceOp::Create,
                None,
            )?;
        }
        let dead_letter = match &patch.dead_letter_topic {
            Some(d) if !patch.clear_dead_letter_topic => {
                check_name("dead_letter_topic", d)?;
                let t = resolve_topic(&mut Db::Ro(self.backend()), found.tenant.as_deref(), d).await?;
                if t.name == found.topic {
                    return Err(QueueError::invalid(
                        "invalid_dead_letter_topic",
                        "the dead-letter topic must differ from the subscription's topic",
                    ));
                }
                Some((d.clone(), t.name))
            }
            _ => None,
        };
        let name = found.name.as_str();
        let mut sets: Vec<String> = Vec::new();
        let mut params: Vec<Value> = vec![name.into()];
        let mut set = |col: &str, v: Value| {
            params.push(v);
            sets.push(format!("{col} = ?{}", params.len()));
        };
        if let Some(v) = patch.ack_deadline_ms {
            let v = in_range(
                "ack_deadline_ms",
                v,
                crate::model::MIN_ACK_DEADLINE_MS,
                crate::model::MAX_ACK_DEADLINE_MS,
            )?;
            set("ack_deadline_ms", v.into());
        }
        if let Some(v) = patch.max_delivery_attempts {
            let v = if v == 0 {
                None
            } else {
                Some(in_range(
                    "max_delivery_attempts",
                    v,
                    1,
                    crate::model::MAX_DELIVERY_ATTEMPTS,
                )?)
            };
            set("max_delivery_attempts", v.into());
        }
        if let Some(v) = patch.backoff_base_ms {
            set("backoff_base_ms", in_range("backoff_base_ms", v, 0, 600_000)?.into());
        }
        if let Some(v) = patch.backoff_max_ms {
            set("backoff_max_ms", in_range("backoff_max_ms", v, 0, 3_600_000)?.into());
        }
        if patch.clear_dead_letter_topic {
            set("dead_letter_topic", Value::Null);
        } else if let Some((_, d)) = &dead_letter {
            set("dead_letter_topic", d.as_str().into());
        }
        let mut secret = None;
        if patch.clear_push {
            set("push_kind", Value::Null);
            set("push_endpoint", Value::Null);
            set("push_secret", Value::Null);
            set("push_secret_previous", Value::Null);
            set("push_secret_previous_until", Value::Null);
            set("paused_at", Value::Null);
            set("paused_reason", Value::Null);
        } else if let Some(p) = &patch.push {
            self.check_push(p)?;
            set("push_kind", p.kind.as_str().into());
            set("push_endpoint", p.endpoint.as_str().into());
            // A new endpoint is a new receiver: no old secret signs for it.
            set("push_secret_previous", Value::Null);
            set("push_secret_previous_until", Value::Null);
            if p.kind == PushKind::Webhook {
                let (plain, sealed) = self.new_secret(name)?;
                set("push_secret", Value::Blob(sealed));
                secret = Some(plain);
            } else {
                set("push_secret", Value::Null);
            }
        }
        if let Some(n) = patch.pause_after_failures {
            let v = if n == 0 {
                None
            } else {
                Some(in_range(
                    "pause_after_failures",
                    n,
                    1,
                    crate::model::MAX_PAUSE_AFTER_FAILURES,
                )?)
            };
            set("pause_after_failures", v.into());
        }
        if let Some(t) = patch.ttl_ms {
            let v = if t == 0 {
                None
            } else {
                Some(
                    self.now()
                        + in_range(
                            "ttl_ms",
                            t,
                            crate::model::MIN_SUBSCRIPTION_TTL_MS,
                            crate::model::MAX_SUBSCRIPTION_TTL_MS,
                        )?,
                )
            };
            set("expires_at", v.into());
        }
        if let Some(labels) = &patch.labels {
            check_labels(labels)?;
            set("labels", serde_json::to_string(labels).unwrap_or_default().into());
        }
        let now = self.now();
        set("updated_at", now.into());
        let mut tx = self.begin().await?;
        let r: Result<_> = async {
            sub_row(&mut Db::Tx(&mut *tx), name).await?;
            if let Some((shown, d)) = &dead_letter
                && topic_row(&mut Db::Tx(&mut *tx), d).await?.is_none()
            {
                return Err(QueueError::TopicNotFound(shown.clone()));
            }
            tx.execute(&Statement::new(
                format!(
                    "UPDATE _forgebase_queue_subscriptions SET {} WHERE name = ?1",
                    sets.join(", ")
                ),
                params,
            ))
            .await?;
            let s = sub_row(&mut Db::Tx(&mut *tx), name).await?;
            if s.backoff_max_ms < s.backoff_base_ms {
                return Err(QueueError::invalid(
                    "invalid_parameter",
                    "backoff_max_ms must be at least backoff_base_ms",
                ));
            }
            Ok(s)
        }
        .await;
        let mut sub = finish(tx, r).await?.public();
        sub.push_secret = secret;
        self.notify();
        Ok(sub)
    }

    /// Deletes a subscription with its deliveries, snapshots and dead letters.
    pub async fn delete_subscription(&self, ctx: &AccessContext, name: &str) -> Result<()> {
        self.at_sub(ctx.tenant.as_deref(), name)
            .await?
            .delete_subscription_here(ctx, name)
            .await
    }

    /// [`Self::delete_subscription`] on this view's set.
    pub(crate) async fn delete_subscription_here(&self, ctx: &AccessContext, name: &str) -> Result<()> {
        let found = self
            .sub_for(
                &mut Db::Ro(self.backend()),
                ctx,
                Action::QueueAdmin,
                ResourceOp::Delete,
                name,
            )
            .await?;
        let shown = name;
        let name = found.name.as_str();
        let eng = self.store().await?;
        let mut tx = self.begin().await?;
        let r: Result<_> = async {
            if *eng == crate::model::DeliveryEngine::Pgmq {
                crate::pgmq::drop_queue(&mut *tx, name).await?;
            }
            let n = tx
                .execute(&Statement::new(
                    "DELETE FROM _forgebase_queue_subscriptions WHERE name = ?1",
                    vec![name.into()],
                ))
                .await?
                .rows_affected;
            if n == 0 {
                return Err(QueueError::SubscriptionNotFound(shown.to_owned()));
            }
            tx.execute(&Statement::new(
                "DELETE FROM _forgebase_queue_rates WHERE scope = 'subscription' AND name = ?1",
                vec![name.into()],
            ))
            .await?;
            Ok(())
        }
        .await;
        finish(tx, r).await
    }
}

/// The highest sequence of `topic` published before `ts` (0 when none): a
/// cursor that delivers everything published at or after `ts`.
pub(crate) async fn cursor_before(tx: &mut dyn WriteTx, topic: &str, ts: i64) -> Result<i64> {
    Ok(scalar(
        &tx.fetch_all(&Statement::new(
            "SELECT coalesce(max(seq), 0) FROM _forgebase_queue_messages WHERE topic = ?1 AND published_at < ?2",
            vec![topic.into(), ts.into()],
        ))
        .await?,
    ))
}

fn check_topic_schema(s: &Json) -> Result<String> {
    let text = s.to_string();
    if text.len() > crate::model::MAX_SCHEMA_BYTES {
        return Err(QueueError::invalid(
            "invalid_schema",
            format!("the schema is larger than {} bytes", crate::model::MAX_SCHEMA_BYTES),
        ));
    }
    schema::check_schema(s).map_err(|e| QueueError::invalid("invalid_schema", e))?;
    Ok(text)
}

fn check_description(d: Option<&str>) -> Result<Option<String>> {
    let Some(d) = d.map(str::trim).filter(|d| !d.is_empty()) else {
        return Ok(None);
    };
    if d.chars().count() > crate::model::MAX_DESCRIPTION {
        return Err(QueueError::invalid(
            "invalid_description",
            format!(
                "the description is longer than {} characters",
                crate::model::MAX_DESCRIPTION
            ),
        ));
    }
    Ok(Some(d.to_owned()))
}

/// Checks a topic example (size, and the schema when one is set); returns its text.
fn check_example(ex: &Json, schema: Option<&Json>) -> Result<String> {
    let text = ex.to_string();
    if text.len() > crate::model::MAX_EXAMPLE_BYTES {
        return Err(QueueError::invalid(
            "invalid_example",
            format!("the example is larger than {} bytes", crate::model::MAX_EXAMPLE_BYTES),
        ));
    }
    if let Some(s) = schema {
        schema::validate(s, ex).map_err(|e| {
            QueueError::invalid(
                "invalid_example",
                format!("the example does not satisfy the schema: {e}"),
            )
        })?;
    }
    Ok(text)
}

fn check_source(src: &RecordSource) -> Result<String> {
    let c = src.collection.trim();
    if c.is_empty() || c.len() > 128 {
        return Err(QueueError::invalid(
            "invalid_source",
            "source.collection must be 1-128 bytes",
        ));
    }
    for op in &src.ops {
        if !matches!(op.as_str(), "create" | "update" | "delete") {
            return Err(QueueError::invalid(
                "invalid_source",
                format!("source.ops: unknown operation {op:?} (create, update, delete)"),
            ));
        }
    }
    Ok(c.to_owned())
}
