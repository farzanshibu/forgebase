//! Durable topics and queues (`/api/v1/queues`): topics, publish, pull, ack, nack, replay and streaming pull.
//!
//! The wire types are written by hand (they follow the server's JSON, snake_case) until the OpenAPI
//! fixture carries the queue schemas.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::{Error, Result};
use crate::http::{Req, Transport, describe, seg};

const Q: &str = "/api/v1/queues";

/// A topic.
#[derive(Debug, Clone, Deserialize)]
pub struct Topic {
    /// Name.
    pub name: String,
    /// How long messages are kept, in ms.
    #[serde(default)]
    pub retention_ms: i64,
    /// Largest accepted message, in bytes.
    #[serde(default)]
    pub max_message_bytes: i64,
    /// Window in which a repeated dedup id is accepted once, in ms.
    #[serde(default)]
    pub dedup_window_ms: i64,
    /// Labels.
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    /// Messages published since creation.
    #[serde(default)]
    pub published_total: i64,
    /// Creation time (RFC 3339).
    #[serde(default)]
    pub created_at: String,
}

/// Settings of a new topic (fields left `None` use the server default).
#[derive(Debug, Clone, Default, Serialize)]
pub struct NewTopic {
    /// Name (required).
    pub name: String,
    /// Retention in ms.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retention_ms: Option<i64>,
    /// Largest message in bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_message_bytes: Option<i64>,
    /// Dedup window in ms.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dedup_window_ms: Option<i64>,
    /// JSON Schema publishes are validated against.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<Value>,
    /// Labels.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
}

impl NewTopic {
    /// A topic with server defaults.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Self::default()
        }
    }
}

/// A message stored in a topic.
#[derive(Debug, Clone, Deserialize)]
pub struct QueueMessage {
    /// Message id.
    pub id: String,
    /// Topic.
    #[serde(default)]
    pub topic: String,
    /// Sequence number inside the topic.
    #[serde(default)]
    pub seq: i64,
    /// Payload.
    #[serde(default)]
    pub data: Value,
    /// Attributes (subscription filters match on these).
    #[serde(default)]
    pub attributes: BTreeMap<String, String>,
    /// Ordering key.
    #[serde(default)]
    pub ordering_key: Option<String>,
    /// Dedup id.
    #[serde(default)]
    pub dedup_id: Option<String>,
    /// Publish time (RFC 3339).
    #[serde(default)]
    pub published_at: String,
}

/// One message to publish.
#[derive(Debug, Clone, Serialize)]
pub struct NewMessage {
    /// Payload.
    pub data: Value,
    /// Attributes.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub attributes: BTreeMap<String, String>,
    /// Messages sharing a key reach an ordered subscription in publish order.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ordering_key: Option<String>,
    /// A repeat inside the dedup window is accepted once.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dedup_id: Option<String>,
}

impl NewMessage {
    /// A message carrying `data`.
    #[must_use]
    pub fn new(data: Value) -> Self {
        Self {
            data,
            attributes: BTreeMap::new(),
            ordering_key: None,
            dedup_id: None,
        }
    }

    /// Adds an attribute.
    #[must_use]
    pub fn attr(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.attributes.insert(k.into(), v.into());
        self
    }

    /// Sets the ordering key.
    #[must_use]
    pub fn ordering_key(mut self, k: impl Into<String>) -> Self {
        self.ordering_key = Some(k.into());
        self
    }

    /// Sets the dedup id (also makes the publish safe to retry).
    #[must_use]
    pub fn dedup_id(mut self, k: impl Into<String>) -> Self {
        self.dedup_id = Some(k.into());
        self
    }
}

/// Result of one published message.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Published {
    /// Message id.
    pub message_id: String,
    /// Sequence number.
    pub seq: i64,
    /// The dedup id was seen inside the window: nothing was stored.
    #[serde(default)]
    pub duplicate: bool,
}

/// A subscription.
#[derive(Debug, Clone, Deserialize)]
pub struct QueueSubscription {
    /// Name.
    pub name: String,
    /// Topic.
    pub topic: String,
    /// Attribute filter.
    #[serde(default)]
    pub filter: Option<Value>,
    /// Ack deadline in ms.
    #[serde(default)]
    pub ack_deadline_ms: i64,
    /// Deliveries before a message is dead-lettered.
    #[serde(default)]
    pub max_delivery_attempts: Option<i64>,
    /// Dead-letter topic.
    #[serde(default)]
    pub dead_letter_topic: Option<String>,
    /// Ordered delivery.
    #[serde(default)]
    pub ordering: bool,
    /// Creation time (RFC 3339).
    #[serde(default)]
    pub created_at: String,
}

/// Settings of a new subscription (fields left `None` use the server default).
#[derive(Debug, Clone, Default, Serialize)]
pub struct NewSubscription {
    /// Name (required).
    pub name: String,
    /// Topic (required).
    pub topic: String,
    /// Attribute filter, e.g. `{"kind": "rush"}`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter: Option<Value>,
    /// Ack deadline in ms.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ack_deadline_ms: Option<i64>,
    /// Deliveries before dead-lettering.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_delivery_attempts: Option<i64>,
    /// Retry backoff start in ms.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backoff_base_ms: Option<i64>,
    /// Retry backoff cap in ms.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backoff_max_ms: Option<i64>,
    /// Dead-letter topic.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dead_letter_topic: Option<String>,
    /// Ordered delivery per ordering key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ordering: Option<bool>,
    /// Where a new subscription starts: `"latest"`, `"earliest"` or `{"time": "..."}`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start: Option<Value>,
}

impl NewSubscription {
    /// A subscription of `topic` with server defaults.
    #[must_use]
    pub fn new(name: impl Into<String>, topic: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            topic: topic.into(),
            ..Self::default()
        }
    }
}

/// A delivery: one message leased to a consumer until its ack deadline.
#[derive(Debug, Clone, Deserialize)]
pub struct ReceivedMessage {
    /// Handle for `ack`, `nack` and `modify_deadline`.
    pub ack_id: String,
    /// 1 on the first delivery.
    #[serde(default)]
    pub delivery_attempt: i64,
    /// When the lease ends (RFC 3339).
    #[serde(default)]
    pub ack_deadline: String,
    /// The message.
    pub message: QueueMessage,
}

/// Result of an ack, nack or deadline change.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AckResult {
    /// Messages settled.
    #[serde(default)]
    pub count: i64,
    /// Ack ids the server no longer knows (expired or already settled).
    #[serde(default)]
    pub unknown: Vec<String>,
}

/// Where [`QueuesApi::seek`] replays from.
#[derive(Debug, Clone)]
pub enum SeekTo {
    /// A time (RFC 3339), inside the topic's retention.
    Time(String),
    /// A snapshot name.
    Snapshot(String),
}

/// Result of a seek.
#[derive(Debug, Clone, Deserialize)]
pub struct SeekResult {
    /// The subscription's new cursor.
    #[serde(default)]
    pub cursor_seq: i64,
    /// Messages put back.
    #[serde(default)]
    pub restored: i64,
}

/// A snapshot of a subscription's position.
#[derive(Debug, Clone, Deserialize)]
pub struct QueueSnapshot {
    /// Name.
    pub name: String,
    /// Topic.
    #[serde(default)]
    pub topic: String,
    /// Subscription it was taken from.
    #[serde(default)]
    pub subscription: String,
    /// Creation time (RFC 3339).
    #[serde(default)]
    pub created_at: String,
}

/// A page of a topic's message log.
#[derive(Debug, Clone, Deserialize)]
pub struct MessagePage {
    /// Messages, oldest first.
    pub items: Vec<QueueMessage>,
    /// Pass as `after` for the next page; `None` at the end.
    #[serde(default)]
    pub next_after: Option<i64>,
}

/// Options of [`QueuesApi::pull`].
#[derive(Debug, Clone, Default)]
pub struct PullOptions {
    /// 1 to 1000 (default 10).
    pub max_messages: Option<u32>,
    /// Long-poll up to this many ms when nothing is ready.
    pub wait_ms: Option<u64>,
    /// Consumer label shown in the delivery history.
    pub consumer: Option<String>,
}

/// Flow control of [`QueuesApi::subscribe`].
#[derive(Debug, Clone, Default)]
pub struct StreamOptions {
    /// Most unacknowledged messages the stream holds (default 100).
    pub max_messages: Option<u32>,
    /// Most unacknowledged bytes the stream holds (default 10 MiB).
    pub max_bytes: Option<u64>,
}

/// One server-sent event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SseEvent {
    pub event: String,
    pub data: String,
}

/// Incremental `text/event-stream` parser: feed chunks, get complete events.
#[derive(Debug, Default)]
pub(crate) struct SseParser {
    buf: Vec<u8>,
}

impl SseParser {
    pub fn push(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some((end, skip)) = frame_end(&self.buf) {
            let frame: Vec<u8> = self.buf.drain(..end + skip).take(end).collect();
            let text = String::from_utf8_lossy(&frame).replace("\r\n", "\n");
            let mut event = String::from("message");
            let mut data: Vec<&str> = Vec::new();
            for line in text.split('\n') {
                if line.starts_with(':') {
                    continue;
                }
                let (k, v) = line.split_once(':').unwrap_or((line, ""));
                let v = v.strip_prefix(' ').unwrap_or(v);
                match k {
                    "event" => event = v.to_owned(),
                    "data" => data.push(v),
                    _ => {}
                }
            }
            if !data.is_empty() {
                out.push(SseEvent {
                    event,
                    data: data.join("\n"),
                });
            }
        }
        out
    }
}

/// Position and length of the first blank line (`\n\n` or `\r\n\r\n`) in `buf`.
fn frame_end(buf: &[u8]) -> Option<(usize, usize)> {
    let mut i = 0;
    while i < buf.len() {
        if buf[i..].starts_with(b"\n\n") {
            return Some((i, 2));
        }
        if buf[i..].starts_with(b"\r\n\r\n") {
            return Some((i, 4));
        }
        i += 1;
    }
    None
}

/// A streaming pull in progress (server-sent events): read deliveries with [`next`](Self::next) and
/// settle each with [`ack`](Self::ack) or [`nack`](Self::nack). The stream ends (`None`) when the
/// server closes it; open a new one to continue. Deliveries not settled return after their ack deadline.
pub struct QueueStream {
    t: Arc<Transport>,
    subscription: String,
    res: reqwest::Response,
    parser: SseParser,
    ready: VecDeque<SseEvent>,
}

impl QueueStream {
    /// The next delivery; `None` when the stream ended. A server `error` event becomes an
    /// [`Error::Realtime`] carrying its code.
    pub async fn next(&mut self) -> Result<Option<ReceivedMessage>> {
        loop {
            while let Some(ev) = self.ready.pop_front() {
                match ev.event.as_str() {
                    "message" => {
                        let m = serde_json::from_str::<ReceivedMessage>(&ev.data)
                            .map_err(|e| Error::InvalidResponse(format!("queue stream message: {e}")))?;
                        return Ok(Some(m));
                    }
                    "error" => {
                        let v: Value = serde_json::from_str(&ev.data).unwrap_or(Value::Null);
                        return Err(Error::Realtime {
                            code: v
                                .get("code")
                                .and_then(Value::as_str)
                                .unwrap_or("stream_error")
                                .to_owned(),
                            message: v
                                .get("message")
                                .and_then(Value::as_str)
                                .map_or_else(|| ev.data.clone(), str::to_owned),
                        });
                    }
                    _ => {}
                }
            }
            let Some(chunk) = self.res.chunk().await.map_err(|e| Error::Network(describe(&e)))? else {
                return Ok(None);
            };
            self.ready.extend(self.parser.push(&chunk));
        }
    }

    /// Acknowledges a delivery.
    pub async fn ack(&self, m: &ReceivedMessage) -> Result<AckResult> {
        ack_call(&self.t, &self.subscription, "ack", std::slice::from_ref(&m.ack_id)).await
    }

    /// Negative acknowledgement: the message is redelivered after the subscription's backoff.
    pub async fn nack(&self, m: &ReceivedMessage) -> Result<AckResult> {
        ack_call(&self.t, &self.subscription, "nack", std::slice::from_ref(&m.ack_id)).await
    }
}

async fn ack_call(t: &Transport, sub: &str, op: &str, ids: &[String]) -> Result<AckResult> {
    t.json(Req::post(format!("{Q}/subscriptions/{}/{op}", seg(sub))).json_value(json!({"ack_ids": ids})))
        .await
}

/// Topics, subscriptions, publish, pull and streaming pull: `client.queues()`.
#[derive(Clone)]
pub struct QueuesApi {
    pub(crate) t: Arc<Transport>,
}

impl QueuesApi {
    // ---- topics ----

    /// Every topic.
    pub async fn list_topics(&self) -> Result<Vec<Topic>> {
        items(self.t.value(Req::get(format!("{Q}/topics"))).await?)
    }

    /// One topic.
    pub async fn get_topic(&self, name: &str) -> Result<Topic> {
        self.t.json(Req::get(format!("{Q}/topics/{}", seg(name)))).await
    }

    /// Creates a topic.
    pub async fn create_topic(&self, topic: &NewTopic) -> Result<Topic> {
        self.t.json(Req::post(format!("{Q}/topics")).json(topic)?).await
    }

    /// Deletes a topic. `force` also deletes its subscriptions (otherwise the server answers `topic_has_subscriptions`).
    pub async fn delete_topic(&self, name: &str, force: bool) -> Result<()> {
        let mut r = Req::delete(format!("{Q}/topics/{}", seg(name)));
        if force {
            r = r.q("force", "true");
        }
        self.t.unit(r).await
    }

    /// A page of a topic's message log, oldest first.
    pub async fn messages(&self, topic: &str, after: Option<i64>, limit: Option<u32>) -> Result<MessagePage> {
        self.t
            .json(
                Req::get(format!("{Q}/topics/{}/messages", seg(topic)))
                    .q_opt("after", after)
                    .q_opt("limit", limit),
            )
            .await
    }

    // ---- publish ----

    /// Publishes one message.
    pub async fn publish(&self, topic: &str, message: &NewMessage) -> Result<Published> {
        let mut r = self.publish_batch(topic, std::slice::from_ref(message)).await?;
        if r.is_empty() {
            return Err(Error::InvalidResponse("publish returned no result".into()));
        }
        Ok(r.swap_remove(0))
    }

    /// Publishes up to 1000 messages in one request; results are in the order sent.
    pub async fn publish_batch(&self, topic: &str, messages: &[NewMessage]) -> Result<Vec<Published>> {
        let mut req = Req::post(format!("{Q}/topics/{}/publish", seg(topic))).json(&json!({"messages": messages}))?;
        // A dedup id makes a repeat harmless, so those publishes are safe to retry.
        if !messages.is_empty() && messages.iter().all(|m| m.dedup_id.is_some()) {
            req = req.idempotent();
        }
        let v = self.t.value(req).await?;
        results(&v)
    }

    // ---- subscriptions ----

    /// Every subscription (of `topic` when given).
    pub async fn list_subscriptions(&self, topic: Option<&str>) -> Result<Vec<QueueSubscription>> {
        items(
            self.t
                .value(Req::get(format!("{Q}/subscriptions")).q_opt("topic", topic))
                .await?,
        )
    }

    /// One subscription.
    pub async fn get_subscription(&self, name: &str) -> Result<QueueSubscription> {
        self.t.json(Req::get(format!("{Q}/subscriptions/{}", seg(name)))).await
    }

    /// Creates a subscription.
    pub async fn create_subscription(&self, sub: &NewSubscription) -> Result<QueueSubscription> {
        self.t.json(Req::post(format!("{Q}/subscriptions")).json(sub)?).await
    }

    /// Deletes a subscription.
    pub async fn delete_subscription(&self, name: &str) -> Result<()> {
        self.t
            .unit(Req::delete(format!("{Q}/subscriptions/{}", seg(name))))
            .await
    }

    /// Backlog, in-flight and rate counters of a subscription, as the server reports them.
    pub async fn subscription_metrics(&self, name: &str) -> Result<Value> {
        self.t
            .value(Req::get(format!("{Q}/subscriptions/{}/metrics", seg(name))))
            .await
    }

    /// Messages that used up their delivery attempts, newest first.
    pub async fn dead_letters(
        &self,
        subscription: &str,
        before: Option<i64>,
        limit: Option<u32>,
    ) -> Result<Vec<Value>> {
        items(
            self.t
                .value(
                    Req::get(format!("{Q}/subscriptions/{}/dead-letters", seg(subscription)))
                        .q_opt("before", before)
                        .q_opt("limit", limit),
                )
                .await?,
        )
    }

    // ---- pull and ack ----

    /// Takes up to `max_messages` ready messages, leasing them for the ack deadline. Not retried
    /// automatically: a blind retry could lease the same messages twice.
    pub async fn pull(&self, subscription: &str, o: &PullOptions) -> Result<Vec<ReceivedMessage>> {
        let body = json!({"max_messages": o.max_messages, "wait_ms": o.wait_ms, "consumer": o.consumer});
        let mut req = Req::post(format!("{Q}/subscriptions/{}/pull", seg(subscription))).json_value(body);
        req.retry = Some(crate::RetryPolicy::none());
        let v = self.t.value(req).await?;
        let list = v.get("messages").cloned().unwrap_or(Value::Array(vec![]));
        serde_json::from_value(list).map_err(|e| Error::InvalidResponse(format!("pull: {e}")))
    }

    /// Acknowledges deliveries by ack id.
    pub async fn ack(&self, subscription: &str, ack_ids: &[String]) -> Result<AckResult> {
        ack_call(&self.t, subscription, "ack", ack_ids).await
    }

    /// Negative acknowledgement: redelivered after the subscription's backoff.
    pub async fn nack(&self, subscription: &str, ack_ids: &[String]) -> Result<AckResult> {
        ack_call(&self.t, subscription, "nack", ack_ids).await
    }

    /// Changes the ack deadline of held messages (1 s to 10 min from now; `0` is a nack).
    pub async fn modify_deadline(
        &self,
        subscription: &str,
        ack_ids: &[String],
        ack_deadline_ms: u64,
    ) -> Result<AckResult> {
        self.t
            .json(
                Req::post(format!("{Q}/subscriptions/{}/modify-deadline", seg(subscription)))
                    .json_value(json!({"ack_ids": ack_ids, "ack_deadline_ms": ack_deadline_ms})),
            )
            .await
    }

    // ---- replay and snapshots ----

    /// Replays: puts messages back so they are delivered again, from a time or a snapshot.
    pub async fn seek(&self, subscription: &str, to: &SeekTo) -> Result<SeekResult> {
        let body = match to {
            SeekTo::Time(t) => json!({"time": t}),
            SeekTo::Snapshot(s) => json!({"snapshot": s}),
        };
        self.t
            .json(Req::post(format!("{Q}/subscriptions/{}/seek", seg(subscription))).json_value(body))
            .await
    }

    /// Every snapshot.
    pub async fn list_snapshots(&self) -> Result<Vec<QueueSnapshot>> {
        items(self.t.value(Req::get(format!("{Q}/snapshots"))).await?)
    }

    /// Saves a subscription's position.
    pub async fn create_snapshot(&self, name: &str, subscription: &str) -> Result<QueueSnapshot> {
        self.t
            .json(Req::post(format!("{Q}/snapshots")).json_value(json!({"name": name, "subscription": subscription})))
            .await
    }

    /// Deletes a snapshot.
    pub async fn delete_snapshot(&self, name: &str) -> Result<()> {
        self.t.unit(Req::delete(format!("{Q}/snapshots/{}", seg(name)))).await
    }

    // ---- streaming pull ----

    /// Opens a streaming pull (server-sent events, flow-controlled by `o`).
    pub async fn subscribe(&self, subscription: &str, o: &StreamOptions) -> Result<QueueStream> {
        let mut req = Req::get(format!("{Q}/subscriptions/{}/events", seg(subscription)))
            .q_opt("max_messages", o.max_messages)
            .q_opt("max_bytes", o.max_bytes);
        req.retry = Some(crate::RetryPolicy::none());
        let res = self.t.send(&req).await?;
        Ok(QueueStream {
            t: self.t.clone(),
            subscription: subscription.to_owned(),
            res,
            parser: SseParser::default(),
            ready: VecDeque::new(),
        })
    }
}

/// `{"items": [...]}` (or a bare array) decoded into a list.
fn items<T: serde::de::DeserializeOwned>(v: Value) -> Result<Vec<T>> {
    let list = match v {
        Value::Object(mut m) => m.remove("items").unwrap_or(Value::Array(vec![])),
        other => other,
    };
    serde_json::from_value(list).map_err(|e| Error::InvalidResponse(format!("list: {e}")))
}

fn results(v: &Value) -> Result<Vec<Published>> {
    let list = v.get("results").cloned().unwrap_or(Value::Array(vec![]));
    serde_json::from_value(list).map_err(|e| Error::InvalidResponse(format!("publish: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_parser_handles_split_chunks_comments_and_crlf() {
        let mut p = SseParser::default();
        assert!(p.push(b": hi\n\nevent: mess").is_empty());
        let ev = p.push(b"age\ndata: {\"a\":1}\n\nevent: error\r\ndata: x\r\n\r\n");
        assert_eq!(
            ev,
            vec![
                SseEvent {
                    event: "message".into(),
                    data: "{\"a\":1}".into()
                },
                SseEvent {
                    event: "error".into(),
                    data: "x".into()
                },
            ]
        );
    }

    #[test]
    fn sse_parser_joins_multiline_data() {
        let mut p = SseParser::default();
        let ev = p.push(b"data: a\ndata: b\n\n");
        assert_eq!(ev[0].data, "a\nb");
    }

    #[test]
    fn new_subscription_omits_unset_fields() {
        let v = serde_json::to_value(NewSubscription::new("s", "t")).unwrap_or_default();
        assert_eq!(v, json!({"name": "s", "topic": "t"}));
    }

    #[test]
    fn new_message_builder_sets_fields() {
        let m = NewMessage::new(json!(1)).attr("k", "v").ordering_key("o").dedup_id("d");
        let v = serde_json::to_value(&m).unwrap_or_default();
        assert_eq!(
            v,
            json!({"data": 1, "attributes": {"k": "v"}, "ordering_key": "o", "dedup_id": "d"})
        );
    }

    #[test]
    fn items_accepts_wrapped_and_bare_lists() {
        let a: Vec<Published> = items(json!({"items": [{"message_id": "m", "seq": 1}]})).unwrap_or_default();
        let b: Vec<Published> = items(json!([{"message_id": "m", "seq": 1}])).unwrap_or_default();
        assert_eq!(a, b);
        assert!(!a[0].duplicate);
    }
}
