//! `/api/v1/queues`: topics, publishing, subscriptions, pull, streaming pull
//! (WebSocket and SSE), ack/nack/deadlines, seek, snapshots, metrics and dead
//! letters. Every operation id starts with `queues_`.
//!
//! Handlers only translate HTTP; [`QueueService`] authorises each call
//! through the data core's authorizer (anonymous callers get `401`, callers
//! without permission `403`).

use std::collections::VecDeque;
use std::time::Duration;

use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use forgebase_api::http::{ApiJson, Query};
use forgebase_api::principal::Access;
use forgebase_api::{ApiError, ApiExtension};
use forgebase_data::{AccessContext, Action};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tower_http::limit::RequestBodyLimitLayer;
use utoipa::{OpenApi, ToSchema};

use crate::error::QueueError;
use crate::model::{
    AckRequest, AckResult, DeadLetter, DeadLetterPage, DeliveryAttempt, Message, MessagePage, ModifyDeadlineRequest,
    NewMessage, NewSnapshot, NewSubscription, NewTopic, PublishRequest, PublishResponse, Published, PullRequest,
    PullResponse, PushConfig, PushKind, ReceivedMessage, RecordSource, SeekRequest, SeekResult, Snapshot,
    StartPosition, Subscription, SubscriptionMetrics, SubscriptionPatch, Topic, TopicMetrics, TopicPatch,
};
use crate::service::QueueService;

type ApiResult = Result<Response, ApiError>;
type Body<T> = Result<ApiJson<T>, ApiError>;

/// Largest JSON body of the queue API.
const MAX_BODY: usize = 4 * 1024 * 1024;
/// Default and largest flow-control window of a stream.
const STREAM_MAX_MESSAGES: i64 = 1000;
const STREAM_DEFAULT_MESSAGES: i64 = 100;
const STREAM_DEFAULT_BYTES: i64 = 10 * 1024 * 1024;
/// SSE comment interval.
const SSE_PING: Duration = Duration::from_secs(15);

/// The HTTP form of a queue error.
pub fn api(e: QueueError) -> ApiError {
    match e {
        QueueError::Entitlement(inner) => forgebase_entitlements::http::api(inner),
        QueueError::TooLarge(msg) => ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, "message_too_large", msg),
        other => ApiError::from_error(&other),
    }
}

/// Authorises first, then surfaces the body: an anonymous or forbidden caller
/// gets `401`/`403` whatever they sent, never a `400` about their input.
fn body<T>(s: &QueueService, ctx: &AccessContext, action: Action, body: Body<T>) -> Result<T, ApiError> {
    s.authorize(ctx, action).map_err(api)?;
    Ok(body?.0)
}

/// The routes, relative to `/api/v1`, with their state applied.
pub fn router<S: Clone + Send + Sync + 'static>(service: QueueService) -> Router<S> {
    Router::<QueueService>::new()
        .route("/queues/topics", get(list_topics).post(create_topic))
        .route(
            "/queues/topics/{topic}",
            get(get_topic).patch(update_topic).delete(delete_topic),
        )
        .route("/queues/topics/{topic}/publish", post(publish))
        .route("/queues/topics/{topic}/messages", get(list_messages))
        .route("/queues/topics/{topic}/metrics", get(topic_metrics))
        .route(
            "/queues/subscriptions",
            get(list_subscriptions).post(create_subscription),
        )
        .route(
            "/queues/subscriptions/{sub}",
            get(get_subscription)
                .patch(update_subscription)
                .delete(delete_subscription),
        )
        .route("/queues/subscriptions/{sub}/pull", post(pull))
        .route("/queues/subscriptions/{sub}/ack", post(ack))
        .route("/queues/subscriptions/{sub}/nack", post(nack))
        .route("/queues/subscriptions/{sub}/modify-deadline", post(modify_deadline))
        .route("/queues/subscriptions/{sub}/seek", post(seek))
        .route("/queues/subscriptions/{sub}/metrics", get(subscription_metrics))
        .route("/queues/subscriptions/{sub}/dead-letters", get(dead_letters))
        .route("/queues/subscriptions/{sub}/stream", get(stream))
        .route("/queues/subscriptions/{sub}/events", get(events))
        .route("/queues/snapshots", get(list_snapshots).post(create_snapshot))
        .route(
            "/queues/snapshots/{snapshot}",
            get(get_snapshot).delete(delete_snapshot),
        )
        .layer(RequestBodyLimitLayer::new(MAX_BODY))
        .with_state(service)
}

/// The routes and OpenAPI fragment, ready for `RouterOptions::extensions`.
pub fn extension(service: QueueService) -> ApiExtension {
    ApiExtension {
        routes: router(service),
        openapi: openapi(),
    }
}

/// OpenAPI paths and schemas of the queue API.
pub fn openapi() -> utoipa::openapi::OpenApi {
    QueuesDoc::openapi()
}

#[derive(OpenApi)]
#[openapi(
    paths(
        list_topics, create_topic, get_topic, update_topic, delete_topic, publish, list_messages, topic_metrics,
        list_subscriptions, create_subscription, get_subscription, update_subscription, delete_subscription, pull,
        ack, nack, modify_deadline, seek, subscription_metrics, dead_letters, stream, events, list_snapshots,
        create_snapshot, get_snapshot, delete_snapshot,
    ),
    components(schemas(
        Topic, NewTopic, TopicPatch, TopicList, RecordSource, NewMessage, PublishRequest, PublishResponse, Published,
        Message, MessagePage, DeliveryAttempt, Subscription, NewSubscription, SubscriptionPatch, SubscriptionList,
        PushConfig, PushKind, StartPosition, PullRequest, PullResponse, ReceivedMessage, AckRequest, AckResult,
        ModifyDeadlineRequest, SeekRequest, SeekResult, NewSnapshot, Snapshot, SnapshotList, TopicMetrics,
        SubscriptionMetrics, DeadLetter, DeadLetterPage, StreamClientMessage, StreamServerMessage,
    )),
    tags((name = "queues", description = "Durable topics and queues: publish/subscribe, pull, streaming and push delivery")),
)]
struct QueuesDoc;

/// Topics.
#[derive(Serialize, ToSchema)]
pub struct TopicList {
    /// By name.
    pub items: Vec<Topic>,
}

/// Subscriptions.
#[derive(Serialize, ToSchema)]
pub struct SubscriptionList {
    /// By name.
    pub items: Vec<Subscription>,
}

/// Snapshots.
#[derive(Serialize, ToSchema)]
pub struct SnapshotList {
    /// By name.
    pub items: Vec<Snapshot>,
}

/// A client frame of the WebSocket stream (JSON text).
#[derive(Debug, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum StreamClientMessage {
    /// Acknowledge deliveries.
    Ack {
        /// Ack ids.
        ack_ids: Vec<String>,
    },
    /// Negative acknowledgement.
    Nack {
        /// Ack ids.
        ack_ids: Vec<String>,
    },
    /// Change deadlines (`0` nacks).
    ModifyDeadline {
        /// Ack ids.
        ack_ids: Vec<String>,
        /// New deadline from now.
        ack_deadline_ms: i64,
    },
    /// Change the flow-control window.
    Flow {
        /// Most unacknowledged messages held by this stream.
        max_messages: Option<i64>,
        /// Most unacknowledged bytes held by this stream.
        max_bytes: Option<i64>,
    },
}

/// A server frame of the WebSocket stream (JSON text).
#[derive(Debug, Serialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamServerMessage {
    /// New deliveries.
    Messages {
        /// Deliveries.
        messages: Vec<ReceivedMessage>,
    },
    /// Result of an ack, nack or deadline change.
    Result {
        /// `ack`, `nack` or `modify_deadline`.
        op: String,
        /// Outcome.
        result: AckResult,
    },
    /// A request failed; the stream stays open unless it is fatal.
    Error {
        /// Stable code.
        code: String,
        /// Explanation.
        message: String,
    },
}

// ------------------------------------------------------------------ topics

/// Lists topics.
#[utoipa::path(
    get, path = "/api/v1/queues/topics", tag = "queues", operation_id = "queues_list_topics",
    responses((status = 200, description = "Topics", body = TopicList))
)]
async fn list_topics(State(s): State<QueueService>, Access(ctx): Access) -> ApiResult {
    let items = s.topics(&ctx).await.map_err(api)?;
    Ok(Json(TopicList { items }).into_response())
}

/// Creates a topic.
#[utoipa::path(
    post, path = "/api/v1/queues/topics", tag = "queues", operation_id = "queues_create_topic",
    request_body = NewTopic,
    responses((status = 201, description = "Created", body = Topic), (status = 409, description = "The name is taken (`topic_exists`)"))
)]
async fn create_topic(State(s): State<QueueService>, Access(ctx): Access, b: Body<NewTopic>) -> ApiResult {
    let new = body(&s, &ctx, Action::QueueAdmin, b)?;
    Ok((StatusCode::CREATED, Json(s.create_topic(&ctx, new).await.map_err(api)?)).into_response())
}

/// One topic.
#[utoipa::path(
    get, path = "/api/v1/queues/topics/{topic}", tag = "queues", operation_id = "queues_get_topic",
    params(("topic" = String, Path, description = "Topic name")),
    responses((status = 200, description = "The topic", body = Topic))
)]
async fn get_topic(State(s): State<QueueService>, Access(ctx): Access, Path(topic): Path<String>) -> ApiResult {
    Ok(Json(s.topic(&ctx, &topic).await.map_err(api)?).into_response())
}

/// Changes a topic's retention, limits, schema or labels.
#[utoipa::path(
    patch, path = "/api/v1/queues/topics/{topic}", tag = "queues", operation_id = "queues_update_topic",
    params(("topic" = String, Path, description = "Topic name")),
    request_body = TopicPatch,
    responses((status = 200, description = "The topic", body = Topic))
)]
async fn update_topic(
    State(s): State<QueueService>,
    Access(ctx): Access,
    Path(topic): Path<String>,
    b: Body<TopicPatch>,
) -> ApiResult {
    let patch = body(&s, &ctx, Action::QueueAdmin, b)?;
    Ok(Json(s.update_topic(&ctx, &topic, patch).await.map_err(api)?).into_response())
}

/// Deletes a topic and its messages (`force=true` also deletes its subscriptions).
#[utoipa::path(
    delete, path = "/api/v1/queues/topics/{topic}", tag = "queues", operation_id = "queues_delete_topic",
    params(
        ("topic" = String, Path, description = "Topic name"),
        ("force" = Option<bool>, Query, description = "Delete the topic's subscriptions too"),
    ),
    responses((status = 204, description = "Deleted"), (status = 409, description = "The topic has subscriptions (`topic_has_subscriptions`)"))
)]
async fn delete_topic(
    State(s): State<QueueService>,
    Access(ctx): Access,
    Path(topic): Path<String>,
    q: Query,
) -> ApiResult {
    let force = q.bool("force")?;
    s.delete_topic(&ctx, &topic, force).await.map_err(api)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Publishes 1 to 1000 messages in one transaction.
#[utoipa::path(
    post, path = "/api/v1/queues/topics/{topic}/publish", tag = "queues", operation_id = "queues_publish",
    params(("topic" = String, Path, description = "Topic name")),
    request_body = PublishRequest,
    responses(
        (status = 200, description = "One result per message; repeated dedup ids are marked `duplicate`", body = PublishResponse),
        (status = 413, description = "A message is larger than the topic allows (`message_too_large`)"),
        (status = 429, description = "Quota exhausted (`quota_exceeded`)"),
    )
)]
async fn publish(
    State(s): State<QueueService>,
    Access(ctx): Access,
    Path(topic): Path<String>,
    b: Body<PublishRequest>,
) -> ApiResult {
    let req = body(&s, &ctx, Action::QueuePublish, b)?;
    let results = s.publish(&ctx, &topic, req.messages).await.map_err(api)?;
    Ok(Json(PublishResponse { results }).into_response())
}

/// Retained messages of a topic, oldest first.
#[utoipa::path(
    get, path = "/api/v1/queues/topics/{topic}/messages", tag = "queues", operation_id = "queues_list_messages",
    params(
        ("topic" = String, Path, description = "Topic name"),
        ("after" = Option<i64>, Query, description = "Only messages with a higher sequence number"),
        ("limit" = Option<u32>, Query, description = "Page size, default 50, at most 500"),
    ),
    responses((status = 200, description = "A page of messages", body = MessagePage))
)]
async fn list_messages(
    State(s): State<QueueService>,
    Access(ctx): Access,
    Path(topic): Path<String>,
    q: Query,
) -> ApiResult {
    let after = int_param(&q, "after")?.unwrap_or(0);
    let limit = q.u32("limit")?.map_or(50, i64::from);
    Ok(Json(s.messages(&ctx, &topic, after, limit).await.map_err(api)?).into_response())
}

/// Metrics of a topic.
#[utoipa::path(
    get, path = "/api/v1/queues/topics/{topic}/metrics", tag = "queues", operation_id = "queues_topic_metrics",
    params(("topic" = String, Path, description = "Topic name")),
    responses((status = 200, description = "Metrics", body = TopicMetrics))
)]
async fn topic_metrics(State(s): State<QueueService>, Access(ctx): Access, Path(topic): Path<String>) -> ApiResult {
    Ok(Json(s.topic_metrics(&ctx, &topic).await.map_err(api)?).into_response())
}

fn int_param(q: &Query, name: &str) -> Result<Option<i64>, ApiError> {
    q.get(name)
        .filter(|v| !v.is_empty())
        .map(|v| {
            v.parse::<i64>()
                .map_err(|_| ApiError::bad_request("invalid_parameter", format!("{name} must be an integer")))
        })
        .transpose()
}

// ----------------------------------------------------------- subscriptions

/// Lists subscriptions.
#[utoipa::path(
    get, path = "/api/v1/queues/subscriptions", tag = "queues", operation_id = "queues_list_subscriptions",
    params(("topic" = Option<String>, Query, description = "Only subscriptions of this topic")),
    responses((status = 200, description = "Subscriptions", body = SubscriptionList))
)]
async fn list_subscriptions(State(s): State<QueueService>, Access(ctx): Access, q: Query) -> ApiResult {
    let topic = q.get("topic").filter(|t| !t.is_empty());
    let items = s.subscriptions(&ctx, topic).await.map_err(api)?;
    Ok(Json(SubscriptionList { items }).into_response())
}

/// Creates a subscription. A webhook subscription's `push_secret` is returned here only.
#[utoipa::path(
    post, path = "/api/v1/queues/subscriptions", tag = "queues", operation_id = "queues_create_subscription",
    request_body = NewSubscription,
    responses((status = 201, description = "Created", body = Subscription), (status = 409, description = "The name is taken (`subscription_exists`)"))
)]
async fn create_subscription(
    State(s): State<QueueService>,
    Access(ctx): Access,
    b: Body<NewSubscription>,
) -> ApiResult {
    let new = body(&s, &ctx, Action::QueueAdmin, b)?;
    Ok((
        StatusCode::CREATED,
        Json(s.create_subscription(&ctx, new).await.map_err(api)?),
    )
        .into_response())
}

/// One subscription.
#[utoipa::path(
    get, path = "/api/v1/queues/subscriptions/{sub}", tag = "queues", operation_id = "queues_get_subscription",
    params(("sub" = String, Path, description = "Subscription name")),
    responses((status = 200, description = "The subscription", body = Subscription))
)]
async fn get_subscription(State(s): State<QueueService>, Access(ctx): Access, Path(sub): Path<String>) -> ApiResult {
    Ok(Json(s.subscription(&ctx, &sub).await.map_err(api)?).into_response())
}

/// Changes a subscription.
#[utoipa::path(
    patch, path = "/api/v1/queues/subscriptions/{sub}", tag = "queues", operation_id = "queues_update_subscription",
    params(("sub" = String, Path, description = "Subscription name")),
    request_body = SubscriptionPatch,
    responses((status = 200, description = "The subscription", body = Subscription))
)]
async fn update_subscription(
    State(s): State<QueueService>,
    Access(ctx): Access,
    Path(sub): Path<String>,
    b: Body<SubscriptionPatch>,
) -> ApiResult {
    let patch = body(&s, &ctx, Action::QueueAdmin, b)?;
    Ok(Json(s.update_subscription(&ctx, &sub, patch).await.map_err(api)?).into_response())
}

/// Deletes a subscription.
#[utoipa::path(
    delete, path = "/api/v1/queues/subscriptions/{sub}", tag = "queues", operation_id = "queues_delete_subscription",
    params(("sub" = String, Path, description = "Subscription name")),
    responses((status = 204, description = "Deleted"))
)]
async fn delete_subscription(State(s): State<QueueService>, Access(ctx): Access, Path(sub): Path<String>) -> ApiResult {
    s.delete_subscription(&ctx, &sub).await.map_err(api)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Pulls messages, optionally waiting (long poll) up to `wait_ms`.
#[utoipa::path(
    post, path = "/api/v1/queues/subscriptions/{sub}/pull", tag = "queues", operation_id = "queues_pull",
    params(("sub" = String, Path, description = "Subscription name")),
    request_body = PullRequest,
    responses((status = 200, description = "Deliveries (possibly none)", body = PullResponse))
)]
async fn pull(
    State(s): State<QueueService>,
    Access(ctx): Access,
    Path(sub): Path<String>,
    b: Body<PullRequest>,
) -> ApiResult {
    let req = body(&s, &ctx, Action::QueueConsume, b)?;
    let messages = s.pull(&ctx, &sub, req).await.map_err(api)?;
    Ok(Json(PullResponse { messages }).into_response())
}

/// Acknowledges deliveries.
#[utoipa::path(
    post, path = "/api/v1/queues/subscriptions/{sub}/ack", tag = "queues", operation_id = "queues_ack",
    params(("sub" = String, Path, description = "Subscription name")),
    request_body = AckRequest,
    responses((status = 200, description = "Outcome", body = AckResult))
)]
async fn ack(
    State(s): State<QueueService>,
    Access(ctx): Access,
    Path(sub): Path<String>,
    b: Body<AckRequest>,
) -> ApiResult {
    let req = body(&s, &ctx, Action::QueueConsume, b)?;
    Ok(Json(s.ack(&ctx, &sub, &req.ack_ids).await.map_err(api)?).into_response())
}

/// Negatively acknowledges deliveries: redelivered after the retry backoff.
#[utoipa::path(
    post, path = "/api/v1/queues/subscriptions/{sub}/nack", tag = "queues", operation_id = "queues_nack",
    params(("sub" = String, Path, description = "Subscription name")),
    request_body = AckRequest,
    responses((status = 200, description = "Outcome", body = AckResult))
)]
async fn nack(
    State(s): State<QueueService>,
    Access(ctx): Access,
    Path(sub): Path<String>,
    b: Body<AckRequest>,
) -> ApiResult {
    let req = body(&s, &ctx, Action::QueueConsume, b)?;
    Ok(Json(s.nack(&ctx, &sub, &req.ack_ids).await.map_err(api)?).into_response())
}

/// Extends (or, with `0`, ends) the ack deadline of deliveries.
#[utoipa::path(
    post, path = "/api/v1/queues/subscriptions/{sub}/modify-deadline", tag = "queues", operation_id = "queues_modify_deadline",
    params(("sub" = String, Path, description = "Subscription name")),
    request_body = ModifyDeadlineRequest,
    responses((status = 200, description = "Outcome", body = AckResult))
)]
async fn modify_deadline(
    State(s): State<QueueService>,
    Access(ctx): Access,
    Path(sub): Path<String>,
    b: Body<ModifyDeadlineRequest>,
) -> ApiResult {
    let req = body(&s, &ctx, Action::QueueConsume, b)?;
    let r = s
        .modify_deadline(&ctx, &sub, &req.ack_ids, req.ack_deadline_ms)
        .await
        .map_err(api)?;
    Ok(Json(r).into_response())
}

/// Seeks a subscription to a time or a snapshot (replay).
#[utoipa::path(
    post, path = "/api/v1/queues/subscriptions/{sub}/seek", tag = "queues", operation_id = "queues_seek",
    params(("sub" = String, Path, description = "Subscription name")),
    request_body = SeekRequest,
    responses((status = 200, description = "Outcome", body = SeekResult))
)]
async fn seek(
    State(s): State<QueueService>,
    Access(ctx): Access,
    Path(sub): Path<String>,
    b: Body<SeekRequest>,
) -> ApiResult {
    let req = body(&s, &ctx, Action::QueueAdmin, b)?;
    Ok(Json(s.seek(&ctx, &sub, req).await.map_err(api)?).into_response())
}

/// Metrics of a subscription: backlog, oldest unacked age, rates, dead letters.
#[utoipa::path(
    get, path = "/api/v1/queues/subscriptions/{sub}/metrics", tag = "queues", operation_id = "queues_subscription_metrics",
    params(("sub" = String, Path, description = "Subscription name")),
    responses((status = 200, description = "Metrics", body = SubscriptionMetrics))
)]
async fn subscription_metrics(
    State(s): State<QueueService>,
    Access(ctx): Access,
    Path(sub): Path<String>,
) -> ApiResult {
    Ok(Json(s.subscription_metrics(&ctx, &sub).await.map_err(api)?).into_response())
}

/// Dead-letter records of a subscription with their delivery history, newest first.
#[utoipa::path(
    get, path = "/api/v1/queues/subscriptions/{sub}/dead-letters", tag = "queues", operation_id = "queues_dead_letters",
    params(
        ("sub" = String, Path, description = "Subscription name"),
        ("before" = Option<i64>, Query, description = "Only records with a lower id"),
        ("limit" = Option<u32>, Query, description = "Page size, default 50, at most 500"),
    ),
    responses((status = 200, description = "Dead letters", body = DeadLetterPage))
)]
async fn dead_letters(
    State(s): State<QueueService>,
    Access(ctx): Access,
    Path(sub): Path<String>,
    q: Query,
) -> ApiResult {
    let before = int_param(&q, "before")?;
    let limit = q.u32("limit")?.map_or(50, i64::from);
    Ok(Json(s.dead_letters(&ctx, &sub, before, limit).await.map_err(api)?).into_response())
}

// --------------------------------------------------------------- streaming

struct Window {
    max_messages: i64,
    max_bytes: i64,
}

fn window(q: &Query) -> Result<Window, ApiError> {
    let max_messages = int_param(q, "max_messages")?.unwrap_or(STREAM_DEFAULT_MESSAGES);
    let max_bytes = int_param(q, "max_bytes")?.unwrap_or(STREAM_DEFAULT_BYTES);
    if !(1..=STREAM_MAX_MESSAGES).contains(&max_messages) || max_bytes < 1 {
        return Err(ApiError::bad_request(
            "invalid_parameter",
            format!("max_messages must be 1-{STREAM_MAX_MESSAGES} and max_bytes positive"),
        ));
    }
    Ok(Window {
        max_messages,
        max_bytes,
    })
}

/// Checks access and that `sub` is a pull subscription.
async fn stream_target(s: &QueueService, ctx: &AccessContext, sub: &str) -> Result<(), ApiError> {
    s.authorize(ctx, Action::QueueConsume).map_err(api)?;
    let found = s.subscription(&AccessContext::system(), sub).await.map_err(api)?;
    if found.push.is_some() {
        return Err(api(QueueError::conflict(
            "push_subscription",
            format!("subscription {sub:?} delivers by push and cannot be streamed"),
        )));
    }
    Ok(())
}

/// Streaming pull over WebSocket. Query `max_messages` and `max_bytes` set the
/// flow-control window (unacknowledged messages and bytes this stream may
/// hold; defaults 100 and 10 MiB). Frames are [`StreamServerMessage`] and
/// [`StreamClientMessage`] JSON. Leases still held when the socket closes are
/// released at once.
#[utoipa::path(
    get, path = "/api/v1/queues/subscriptions/{sub}/stream", tag = "queues", operation_id = "queues_stream",
    params(
        ("sub" = String, Path, description = "Subscription name"),
        ("max_messages" = Option<i64>, Query, description = "Flow control: most unacknowledged messages (default 100)"),
        ("max_bytes" = Option<i64>, Query, description = "Flow control: most unacknowledged bytes (default 10 MiB)"),
    ),
    responses((status = 101, description = "WebSocket upgrade"), (status = 409, description = "A push subscription (`push_subscription`)"))
)]
async fn stream(
    State(s): State<QueueService>,
    Access(ctx): Access,
    Path(sub): Path<String>,
    q: Query,
    ws: WebSocketUpgrade,
) -> ApiResult {
    let w = window(&q)?;
    stream_target(&s, &ctx, &sub).await?;
    Ok(ws
        .on_upgrade(move |socket| ws_session(s, ctx, sub, w, socket))
        .into_response())
}

fn frame(m: &StreamServerMessage) -> WsMessage {
    WsMessage::Text(serde_json::to_string(m).unwrap_or_default().into())
}

async fn ws_session(s: QueueService, ctx: AccessContext, sub: String, mut w: Window, socket: WebSocket) {
    let consumer = format!("ws-{}", uuid::Uuid::new_v4().simple());
    let (mut tx, mut rx) = socket.split();
    let mut wake = s.wake_rx();
    let mut closed = s.closed_rx();
    let poll = s.settings().poll_interval;
    loop {
        if *closed.borrow_and_update() {
            let _ = tx.send(WsMessage::Close(None)).await;
            break;
        }
        // Access can change while the stream is open.
        if let Err(e) = s.authorize(&ctx, Action::QueueConsume) {
            let a = api(e);
            let _ = tx
                .send(frame(&StreamServerMessage::Error {
                    code: a.code().to_owned(),
                    message: a.message().to_owned(),
                }))
                .await;
            break;
        }
        wake.borrow_and_update();
        match s.stream_batch(&sub, &consumer, w.max_messages, w.max_bytes).await {
            Ok(messages) if !messages.is_empty() => {
                if tx
                    .send(frame(&StreamServerMessage::Messages { messages }))
                    .await
                    .is_err()
                {
                    break;
                }
                continue;
            }
            Ok(_) => {}
            Err(e) => {
                let a = api(e);
                let _ = tx
                    .send(frame(&StreamServerMessage::Error {
                        code: a.code().to_owned(),
                        message: a.message().to_owned(),
                    }))
                    .await;
                break;
            }
        }
        tokio::select! {
            incoming = rx.next() => {
                let Some(Ok(msg)) = incoming else { break };
                let text = match msg {
                    WsMessage::Text(t) => t.to_string(),
                    WsMessage::Close(_) => break,
                    _ => continue,
                };
                let reply = handle_client(&s, &ctx, &sub, &mut w, &text).await;
                if let Some(reply) = reply
                    && tx.send(frame(&reply)).await.is_err()
                {
                    break;
                }
            }
            _ = wake.changed() => {}
            _ = closed.changed() => {}
            () = tokio::time::sleep(poll) => {}
        }
    }
    if let Err(e) = s.release_consumer(&sub, &consumer).await {
        tracing::warn!(error = %e, subscription = %sub, "stream leases not released");
    }
}

async fn handle_client(
    s: &QueueService,
    ctx: &AccessContext,
    sub: &str,
    w: &mut Window,
    text: &str,
) -> Option<StreamServerMessage> {
    let msg: StreamClientMessage = match serde_json::from_str(text) {
        Ok(m) => m,
        Err(e) => {
            return Some(StreamServerMessage::Error {
                code: "invalid_message".into(),
                message: e.to_string(),
            });
        }
    };
    let (op, r) = match msg {
        StreamClientMessage::Ack { ack_ids } => ("ack", s.ack(ctx, sub, &ack_ids).await),
        StreamClientMessage::Nack { ack_ids } => ("nack", s.nack(ctx, sub, &ack_ids).await),
        StreamClientMessage::ModifyDeadline {
            ack_ids,
            ack_deadline_ms,
        } => (
            "modify_deadline",
            s.modify_deadline(ctx, sub, &ack_ids, ack_deadline_ms).await,
        ),
        StreamClientMessage::Flow {
            max_messages,
            max_bytes,
        } => {
            let m = max_messages.unwrap_or(w.max_messages);
            let b = max_bytes.unwrap_or(w.max_bytes);
            if !(1..=STREAM_MAX_MESSAGES).contains(&m) || b < 1 {
                return Some(StreamServerMessage::Error {
                    code: "invalid_parameter".into(),
                    message: format!("max_messages must be 1-{STREAM_MAX_MESSAGES} and max_bytes positive"),
                });
            }
            w.max_messages = m;
            w.max_bytes = b;
            return None;
        }
    };
    Some(match r {
        Ok(result) => StreamServerMessage::Result { op: op.into(), result },
        Err(e) => {
            let a = api(e);
            StreamServerMessage::Error {
                code: a.code().to_owned(),
                message: a.message().to_owned(),
            }
        }
    })
}

/// Streaming pull over Server-Sent Events: one `message` event per delivery
/// (a [`ReceivedMessage`]); acknowledge with the REST endpoints. Flow control
/// as for the WebSocket stream; leases of a closed stream expire with their
/// deadline.
#[utoipa::path(
    get, path = "/api/v1/queues/subscriptions/{sub}/events", tag = "queues", operation_id = "queues_events",
    params(
        ("sub" = String, Path, description = "Subscription name"),
        ("max_messages" = Option<i64>, Query, description = "Flow control: most unacknowledged messages (default 100)"),
        ("max_bytes" = Option<i64>, Query, description = "Flow control: most unacknowledged bytes (default 10 MiB)"),
    ),
    responses((status = 200, description = "An event stream (`text/event-stream`)"), (status = 409, description = "A push subscription (`push_subscription`)"))
)]
async fn events(State(s): State<QueueService>, Access(ctx): Access, Path(sub): Path<String>, q: Query) -> ApiResult {
    let w = window(&q)?;
    stream_target(&s, &ctx, &sub).await?;
    let state = SseState {
        consumer: format!("sse-{}", uuid::Uuid::new_v4().simple()),
        wake: s.wake_rx(),
        closed: s.closed_rx(),
        s,
        ctx,
        sub,
        w,
        pending: VecDeque::new(),
        done: false,
    };
    let stream = futures_util::stream::unfold(state, |mut st| async move {
        loop {
            if st.done || *st.closed.borrow_and_update() {
                return None;
            }
            if let Some(m) = st.pending.pop_front() {
                let ev = SseEvent::default()
                    .event("message")
                    .id(m.message.id.clone())
                    .data(serde_json::to_string(&m).unwrap_or_default());
                return Some((Ok::<_, std::convert::Infallible>(ev), st));
            }
            if let Err(e) = st.s.authorize(&st.ctx, Action::QueueConsume) {
                st.done = true;
                return Some((Ok(error_event(api(e))), st));
            }
            st.wake.borrow_and_update();
            match st
                .s
                .stream_batch(&st.sub, &st.consumer, st.w.max_messages, st.w.max_bytes)
                .await
            {
                Ok(batch) if !batch.is_empty() => st.pending.extend(batch),
                Ok(_) => {
                    let poll = st.s.settings().poll_interval;
                    tokio::select! {
                        _ = st.wake.changed() => {}
                        _ = st.closed.changed() => {}
                        () = tokio::time::sleep(poll) => {}
                    }
                }
                Err(e) => {
                    st.done = true;
                    return Some((Ok(error_event(api(e))), st));
                }
            }
        }
    });
    Ok(Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(SSE_PING).text("ping"))
        .into_response())
}

struct SseState {
    s: QueueService,
    ctx: AccessContext,
    sub: String,
    consumer: String,
    w: Window,
    wake: tokio::sync::watch::Receiver<u64>,
    closed: tokio::sync::watch::Receiver<bool>,
    pending: VecDeque<ReceivedMessage>,
    done: bool,
}

fn error_event(a: ApiError) -> SseEvent {
    SseEvent::default()
        .event("error")
        .data(json!({"code": a.code(), "message": a.message()}).to_string())
}

// --------------------------------------------------------------- snapshots

/// Lists snapshots.
#[utoipa::path(
    get, path = "/api/v1/queues/snapshots", tag = "queues", operation_id = "queues_list_snapshots",
    responses((status = 200, description = "Snapshots", body = SnapshotList))
)]
async fn list_snapshots(State(s): State<QueueService>, Access(ctx): Access) -> ApiResult {
    let items = s.snapshots(&ctx).await.map_err(api)?;
    Ok(Json(SnapshotList { items }).into_response())
}

/// Saves a subscription's acknowledgement state.
#[utoipa::path(
    post, path = "/api/v1/queues/snapshots", tag = "queues", operation_id = "queues_create_snapshot",
    request_body = NewSnapshot,
    responses((status = 201, description = "Created", body = Snapshot), (status = 409, description = "The name is taken (`snapshot_exists`)"))
)]
async fn create_snapshot(State(s): State<QueueService>, Access(ctx): Access, b: Body<NewSnapshot>) -> ApiResult {
    let new = body(&s, &ctx, Action::QueueAdmin, b)?;
    Ok((
        StatusCode::CREATED,
        Json(s.create_snapshot(&ctx, new).await.map_err(api)?),
    )
        .into_response())
}

/// One snapshot.
#[utoipa::path(
    get, path = "/api/v1/queues/snapshots/{snapshot}", tag = "queues", operation_id = "queues_get_snapshot",
    params(("snapshot" = String, Path, description = "Snapshot name")),
    responses((status = 200, description = "The snapshot", body = Snapshot))
)]
async fn get_snapshot(State(s): State<QueueService>, Access(ctx): Access, Path(name): Path<String>) -> ApiResult {
    Ok(Json(s.snapshot(&ctx, &name).await.map_err(api)?).into_response())
}

/// Deletes a snapshot.
#[utoipa::path(
    delete, path = "/api/v1/queues/snapshots/{snapshot}", tag = "queues", operation_id = "queues_delete_snapshot",
    params(("snapshot" = String, Path, description = "Snapshot name")),
    responses((status = 204, description = "Deleted"))
)]
async fn delete_snapshot(State(s): State<QueueService>, Access(ctx): Access, Path(name): Path<String>) -> ApiResult {
    s.delete_snapshot(&ctx, &name).await.map_err(api)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
