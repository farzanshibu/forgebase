//! HTTP surface under `/api/v1/realtime` and its OpenAPI fragment.
//!
//! | Route | Purpose |
//! |---|---|
//! | `GET /ws` | WebSocket (protocol in ADR-0011 §5) |
//! | `GET /sse` | Server-Sent Events: `subscriptions` (JSON array), `cursor` or `Last-Event-ID` |
//! | `POST /channels/{channel}/messages` | publish to a channel |
//! | `GET /channels/{channel}/presence` | members of a channel |
//! | `GET /stats` | counters (administrators) |
//!
//! Identity comes from the auth layer (headers or session cookie) like every
//! `/api/v1` route; browsers that cannot set headers on a WebSocket or an
//! `EventSource` may pass `access_token` in the query string instead.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::connect_info::ConnectInfo;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{DefaultBodyLimit, FromRequestParts, Path, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event as SseEvent, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use forgebase_auth::Authenticated;
use forgebase_auth::scopes::Access;
use forgebase_core::{ErrorCode, ErrorKind};
use forgebase_data::{AccessContext, Principal};
use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt, stream};
use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;
use utoipa::{OpenApi, ToSchema};

use crate::conn::{self, CloseReason, Conn, Credential, Identity, Inbound, Outbound};
use crate::presence::Member;
use crate::protocol::SubscribeSpec;
use crate::{RealtimeError, RealtimeService, RealtimeStats};

/// How long a closing connection may take to flush its close frame.
const CLOSE_GRACE: std::time::Duration = std::time::Duration::from_secs(5);
/// Longest accepted query string.
const MAX_QUERY_LEN: usize = 16 * 1024;

// ---- errors ----------------------------------------------------------------

/// An error response. The request id is filled in by [`with_request_id`].
#[derive(Debug, Clone)]
struct HttpError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl HttpError {
    fn body(&self, request_id: &str) -> Response {
        let body = json!({"error": {"code": self.code, "message": self.message, "request_id": request_id}});
        (self.status, Json(body)).into_response()
    }
}

impl From<RealtimeError> for HttpError {
    fn from(e: RealtimeError) -> Self {
        let status = match (&e, e.kind()) {
            (RealtimeError::TooManyConnections(_) | RealtimeError::RateLimited(_), _) => StatusCode::TOO_MANY_REQUESTS,
            (_, ErrorKind::InvalidInput) => StatusCode::BAD_REQUEST,
            (_, ErrorKind::Unauthenticated) => StatusCode::UNAUTHORIZED,
            (_, ErrorKind::Forbidden) => StatusCode::FORBIDDEN,
            (_, ErrorKind::NotFound) => StatusCode::NOT_FOUND,
            (_, ErrorKind::Conflict) => StatusCode::CONFLICT,
            (_, ErrorKind::Unavailable) => StatusCode::SERVICE_UNAVAILABLE,
            (_, ErrorKind::Internal) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let message = if e.kind().is_client_visible() {
            e.to_string()
        } else {
            tracing::error!(code = e.code(), error = %e, "realtime request failed");
            "internal server error".to_owned()
        };
        Self {
            status,
            code: e.code(),
            message,
        }
    }
}

impl IntoResponse for HttpError {
    fn into_response(self) -> Response {
        let mut res = self.status.into_response();
        res.extensions_mut().insert(self);
        res
    }
}

/// Puts the request id into the error body of this router's responses.
async fn with_request_id(req: Request, next: Next) -> Response {
    let id = req
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let mut res = next.run(req).await;
    match res.extensions_mut().remove::<HttpError>() {
        Some(err) => err.body(&id),
        None => res,
    }
}

/// Error body (the standard envelope).
#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorBody {
    /// `{code, message, request_id}`.
    pub error: ErrorDetail,
}

/// Error details.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = RealtimeErrorDetail)]
pub struct ErrorDetail {
    /// Stable snake_case code.
    pub code: String,
    /// Human-readable message.
    pub message: String,
    /// Request id.
    pub request_id: String,
}

// ---- identity --------------------------------------------------------------

fn query_pairs(parts: &Parts) -> Result<Vec<(String, String)>, HttpError> {
    let q = parts.uri.query().unwrap_or("");
    if q.len() > MAX_QUERY_LEN {
        return Err(RealtimeError::invalid("query_too_long", "the query string is too long").into());
    }
    Ok(url::form_urlencoded::parse(q.as_bytes()).into_owned().collect())
}

fn param<'a>(pairs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    pairs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
}

/// The credential of a request, as the auth layer reads it.
fn header_credential(headers: &HeaderMap) -> Option<Credential> {
    if let Some(v) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        return v
            .strip_prefix("Bearer ")
            .or_else(|| v.strip_prefix("bearer "))
            .map(|t| Credential::Bearer(t.trim().to_owned()));
    }
    if let Some(v) = headers
        .get(forgebase_auth::http::API_KEY_HEADER)
        .and_then(|v| v.to_str().ok())
    {
        return Some(Credential::ApiKey(v.trim().to_owned()));
    }
    forgebase_auth::http::cookie_value(headers, forgebase_auth::http::SESSION_COOKIE)
        .filter(|c| !c.is_empty())
        .map(Credential::Cookie)
}

/// Resolves who connects: the auth layer's result, or an `access_token`
/// query parameter.
async fn identity(svc: &RealtimeService, parts: &Parts, pairs: &[(String, String)]) -> Result<Identity, HttpError> {
    let mut ctx = parts
        .extensions
        .get::<AccessContext>()
        .cloned()
        .unwrap_or_else(AccessContext::anonymous);
    if ctx.request_id.is_none() {
        ctx.request_id = parts
            .headers
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
    }
    if let Some(a) = parts.extensions.get::<Authenticated>() {
        let credential = header_credential(&parts.headers);
        if matches!(credential, Some(Credential::Cookie(_))) && !same_origin(&parts.headers) {
            return Err(RealtimeError::Forbidden(
                "cross_origin: cookie-authenticated realtime connections must come from the same origin".into(),
            )
            .into());
        }
        return Ok(Identity {
            ctx,
            credential,
            scopes: a.scopes.clone(),
        });
    }
    if let Some(token) = param(pairs, "access_token").filter(|t| !t.is_empty()) {
        let Some(auth) = svc.inner.auth.as_ref() else {
            return Err(RealtimeError::Unavailable("authentication is not configured".into()).into());
        };
        let cred = Credential::Bearer(token.to_owned());
        let a = conn::authenticate(auth, &cred)
            .await
            .map_err(|e| RealtimeError::Unauthenticated(e.to_string()))?;
        if !conn::scopes_grant(a.scopes.as_deref(), "realtime", Access::Read, None) {
            return Err(
                RealtimeError::Forbidden("insufficient_scope: realtime needs scope realtime:read".into()).into(),
            );
        }
        let ctx = a.access_context(ctx.request_id.as_deref());
        return Ok(Identity {
            ctx,
            credential: Some(cred),
            scopes: a.scopes,
        });
    }
    Ok(Identity {
        ctx,
        credential: None,
        scopes: None,
    })
}

/// `Origin` (when sent) must name the `Host` the request was sent to. Stops
/// cross-site WebSocket hijacking with a session cookie.
fn same_origin(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("");
    let origin_host = origin
        .split_once("://")
        .map_or(origin, |(_, h)| h)
        .trim_end_matches('/');
    !host.is_empty() && origin_host.eq_ignore_ascii_case(host)
}

fn limit_key(parts: &Parts, ctx: &AccessContext) -> String {
    if ctx.principal == Principal::Anonymous {
        let ip = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0.ip().to_string())
            .unwrap_or_default();
        format!("anon:{ip}")
    } else {
        ctx.actor()
    }
}

// ---- WebSocket -------------------------------------------------------------

/// WebSocket endpoint.
#[utoipa::path(
    get, path = "/api/v1/realtime/ws", tag = "realtime",
    params(("access_token" = Option<String>, Query, description = "Credential for clients that cannot send headers")),
    responses(
        (status = 101, description = "Switching protocols; JSON messages as documented in ADR-0011 §5"),
        (status = 400, description = "Not a WebSocket upgrade", body = ErrorBody),
        (status = 401, description = "Invalid credential", body = ErrorBody),
        (status = 429, description = "Connection limit reached", body = ErrorBody),
    )
)]
async fn ws(State(svc): State<RealtimeService>, req: Request) -> Result<Response, HttpError> {
    let (mut parts, _body) = req.into_parts();
    let upgrade = WebSocketUpgrade::from_request_parts(&mut parts, &())
        .await
        .map_err(|r| RealtimeError::invalid("websocket_upgrade_required", r.body_text()))?;
    let pairs = query_pairs(&parts)?;
    let ident = identity(&svc, &parts, &pairs).await?;
    let slot = svc.admit(limit_key(&parts, &ident.ctx))?;
    let max = svc.inner.cfg.max_message_bytes;
    Ok(upgrade
        .max_message_size(max)
        .max_frame_size(max)
        .on_upgrade(move |socket| async move {
            ws_session(svc, socket, ident).await;
            drop(slot);
        }))
}

fn inbound(m: Result<Message, axum::Error>) -> Inbound {
    match m {
        Ok(Message::Text(t)) => Inbound::Text(t.as_str().to_owned()),
        Ok(Message::Binary(_)) => Inbound::Binary,
        Ok(Message::Ping(_) | Message::Pong(_)) => Inbound::Activity,
        Ok(Message::Close(_)) => Inbound::Closed,
        Err(e) => {
            let text = e.to_string();
            if text.contains("too long") || text.contains("too big") || text.contains("limit") {
                Inbound::TooBig
            } else {
                Inbound::Closed
            }
        }
    }
}

async fn ws_session(svc: RealtimeService, socket: WebSocket, ident: Identity) {
    let cfg = svc.inner.cfg.clone();
    let (sink, stream) = socket.split();
    let (tx, rx) = mpsc::channel(cfg.outbound_queue.max(1));
    let (ctrl_tx, ctrl_rx) = oneshot::channel();
    let slow = Arc::new(AtomicBool::new(false));
    let writer = tokio::spawn(ws_writer(svc.clone(), sink, rx, ctrl_rx, slow.clone()));
    let (mut conn, receivers) = Conn::new(svc, ident, tx, slow, false);
    let mut stream = stream.map(inbound);
    let reason = match conn.welcome().await {
        Ok(()) => conn.run(&mut stream, receivers).await,
        Err(r) => r,
    };
    tracing::debug!(conn = %conn.id, code = reason.code(), reason = %reason.text(), "realtime websocket closed");
    let client_closed = reason == CloseReason::ClientGone;
    let _ = ctrl_tx.send(reason);
    drop(conn);
    let _ = timeout(cfg.send_timeout + CLOSE_GRACE, writer).await;
    if !client_closed {
        // Closing handshake: keep the socket until the client answers the
        // close frame (or the grace period ends), so it can read the code.
        let _ = timeout(CLOSE_GRACE, async {
            while let Some(m) = stream.next().await {
                if matches!(m, Inbound::Closed) {
                    break;
                }
            }
        })
        .await;
    }
}

/// Writes queued messages; on a close request or a send that blocks past the
/// timeout it sends a close frame (waiting up to the grace period for the
/// client to read it) and stops.
async fn ws_writer(
    svc: RealtimeService,
    mut sink: SplitSink<WebSocket, Message>,
    mut rx: mpsc::Receiver<Outbound>,
    mut ctrl: oneshot::Receiver<CloseReason>,
    slow: Arc<AtomicBool>,
) {
    let send_timeout = svc.inner.cfg.send_timeout;
    let reason = loop {
        tokio::select! {
            biased;
            r = &mut ctrl => break r.unwrap_or(CloseReason::GoingAway),
            m = rx.recv() => {
                let Some(m) = m else {
                    // The connection ended; its reason follows on `ctrl`.
                    break (&mut ctrl).await.unwrap_or(CloseReason::ClientGone);
                };
                let frame = match m {
                    Outbound::Msg { json, .. } => Message::Text(json.into()),
                    Outbound::Ping => Message::Ping(Bytes::new()),
                };
                match timeout(send_timeout, sink.send(frame)).await {
                    Ok(Ok(())) => {}
                    Ok(Err(_)) => return,
                    Err(_) => {
                        conn::mark_slow(&svc, &slow);
                        break CloseReason::SlowConsumer;
                    }
                }
            }
        }
    };
    if reason == CloseReason::ClientGone {
        let _ = timeout(send_timeout, sink.close()).await;
        return;
    }
    let frame = CloseFrame {
        code: reason.code(),
        reason: reason.text().into(),
    };
    let _ = timeout(send_timeout.max(CLOSE_GRACE), sink.send(Message::Close(Some(frame)))).await;
}

// ---- SSE -------------------------------------------------------------------

/// Server-Sent Events endpoint.
#[utoipa::path(
    get, path = "/api/v1/realtime/sse", tag = "realtime",
    params(
        ("subscriptions" = String, Query, description = "JSON array of subscriptions (SubscribeSpec)"),
        ("cursor" = Option<i64>, Query, description = "Replay record events after this cursor (Last-Event-ID wins)"),
        ("access_token" = Option<String>, Query, description = "Credential for clients that cannot send headers"),
        ("Last-Event-ID" = Option<String>, Header, description = "Cursor of the last event received (set by EventSource on reconnect)"),
    ),
    responses(
        (status = 200, description = "text/event-stream: `event:` = message type, `id:` = cursor of record events, `data:` = JSON message", content_type = "text/event-stream", body = String),
        (status = 400, description = "Invalid subscriptions", body = ErrorBody),
        (status = 401, description = "Authentication required", body = ErrorBody),
        (status = 403, description = "A subscription is not allowed", body = ErrorBody),
        (status = 429, description = "Connection limit reached", body = ErrorBody),
    )
)]
async fn sse(State(svc): State<RealtimeService>, req: Request) -> Result<Response, HttpError> {
    let (parts, _body) = req.into_parts();
    let pairs = query_pairs(&parts)?;
    let specs: Vec<SubscribeSpec> = match param(&pairs, "subscriptions") {
        Some(text) => serde_json::from_str(text)
            .map_err(|e| RealtimeError::invalid("invalid_subscriptions", format!("subscriptions: {e}")))?,
        None => Vec::new(),
    };
    let header_cursor = parts.headers.get("last-event-id").and_then(|v| v.to_str().ok());
    let cursor = match header_cursor.or_else(|| param(&pairs, "cursor")) {
        Some(c) => Some(
            c.trim()
                .parse::<i64>()
                .ok()
                .filter(|c| *c >= 0)
                .ok_or_else(|| RealtimeError::invalid("invalid_cursor", "a cursor is a non-negative integer"))?,
        ),
        None => None,
    };
    let ident = identity(&svc, &parts, &pairs).await?;
    let cfg = svc.inner.cfg.clone();
    if specs.len() > cfg.max_subscriptions_per_connection {
        return Err(RealtimeError::invalid(
            "too_many_subscriptions",
            format!(
                "at most {} subscriptions per connection",
                cfg.max_subscriptions_per_connection
            ),
        )
        .into());
    }
    let slot = svc.admit(limit_key(&parts, &ident.ctx))?;
    let (tx, rx) = mpsc::channel(cfg.outbound_queue.max(1));
    let slow = Arc::new(AtomicBool::new(false));
    let (mut conn, receivers) = Conn::new(svc.clone(), ident, tx, slow, true);
    // Authorise every subscription before the stream starts, so a denial is
    // an HTTP status.
    let mut prepared = Vec::with_capacity(specs.len());
    let mut seen = std::collections::HashSet::new();
    for mut spec in specs {
        if !seen.insert(spec.id.clone()) {
            return Err(RealtimeError::invalid("duplicate_subscription", format!("duplicate id {:?}", spec.id)).into());
        }
        if spec.cursor.is_none() {
            spec.cursor = cursor;
        }
        prepared.push(conn.prepare(&spec).await?);
    }
    let (final_tx, final_rx) = oneshot::channel::<CloseReason>();
    tokio::spawn(async move {
        let _slot = slot;
        let reason = match conn.welcome().await {
            Err(r) => r,
            Ok(()) => match conn.activate(prepared, None).await {
                Err(r) => r,
                Ok(()) => conn.run(&mut stream::pending(), receivers).await,
            },
        };
        tracing::debug!(conn = %conn.id, code = reason.code(), reason = %reason.text(), "realtime sse stream closed");
        let _ = final_tx.send(reason);
        drop(conn);
    });
    let events = stream::unfold((rx, Some(final_rx)), |(mut rx, final_rx)| async move {
        match rx.recv().await {
            Some(Outbound::Msg { event, id, json }) => {
                let mut e = SseEvent::default().event(event).data(json);
                if let Some(id) = id {
                    e = e.id(id.to_string());
                }
                Some((Ok::<_, Infallible>(e), (rx, final_rx)))
            }
            Some(Outbound::Ping) => Some((Ok(SseEvent::default().comment("ping")), (rx, final_rx))),
            None => {
                let reason = final_rx?.await.unwrap_or(CloseReason::GoingAway);
                if reason == CloseReason::ClientGone {
                    return None;
                }
                let data = json!({"type": "close", "code": reason.code(), "reason": reason.text()});
                Some((
                    Ok(SseEvent::default().event("close").data(data.to_string())),
                    (rx, None),
                ))
            }
        }
    });
    let mut res = Sse::new(events).into_response();
    res.headers_mut()
        .insert(header::CACHE_CONTROL, header::HeaderValue::from_static("no-store"));
    res.headers_mut()
        .insert("x-accel-buffering", header::HeaderValue::from_static("no"));
    Ok(res)
}

// ---- REST ------------------------------------------------------------------

/// The request's identity for REST routes.
struct Caller(AccessContext);

impl<S: Send + Sync> FromRequestParts<S> for Caller {
    type Rejection = HttpError;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, HttpError> {
        Ok(Caller(
            parts
                .extensions
                .get::<AccessContext>()
                .cloned()
                .unwrap_or_else(AccessContext::anonymous),
        ))
    }
}

/// A message to publish.
#[derive(Debug, Deserialize, ToSchema)]
#[schema(as = RealtimePublishBody)]
pub struct PublishBody {
    /// Any JSON value.
    #[serde(default)]
    pub data: JsonValue,
}

/// Members of a channel.
#[derive(Debug, Serialize, ToSchema)]
pub struct PresenceList {
    /// Channel name.
    pub channel: String,
    /// Members (every instance).
    pub members: Vec<Member>,
}

/// Publishes a message to a channel.
#[utoipa::path(
    post, path = "/api/v1/realtime/channels/{channel}/messages", operation_id = "realtime_publish", tag = "realtime",
    params(("channel" = String, Path, description = "Channel name")),
    request_body = PublishBody,
    responses(
        (status = 202, description = "Queued for delivery to current subscribers"),
        (status = 400, description = "Invalid channel or message", body = ErrorBody),
        (status = 401, description = "Authentication required", body = ErrorBody),
        (status = 403, description = "Not allowed to publish", body = ErrorBody),
    )
)]
async fn publish(
    State(svc): State<RealtimeService>,
    Caller(ctx): Caller,
    Path(channel): Path<String>,
    body: Bytes,
) -> Result<Response, HttpError> {
    let body: PublishBody = serde_json::from_slice(&body)
        .map_err(|e| RealtimeError::invalid("invalid_body", format!("invalid JSON body: {e}")))?;
    svc.publish(&ctx, &channel, body.data).await?;
    Ok((StatusCode::ACCEPTED, Json(json!({"status": "accepted"}))).into_response())
}

/// Lists a channel's presence.
#[utoipa::path(
    get, path = "/api/v1/realtime/channels/{channel}/presence", tag = "realtime",
    params(("channel" = String, Path, description = "Channel name")),
    responses(
        (status = 200, description = "Members", body = PresenceList),
        (status = 401, description = "Authentication required", body = ErrorBody),
        (status = 403, description = "Not allowed", body = ErrorBody),
    )
)]
async fn presence(
    State(svc): State<RealtimeService>,
    Caller(ctx): Caller,
    Path(channel): Path<String>,
) -> Result<Json<PresenceList>, HttpError> {
    let members = svc.presence(&ctx, &channel)?;
    Ok(Json(PresenceList { channel, members }))
}

/// Realtime counters (administrators).
#[utoipa::path(
    get, path = "/api/v1/realtime/stats", operation_id = "realtime_stats", tag = "realtime",
    responses(
        (status = 200, description = "Counters", body = RealtimeStats),
        (status = 401, description = "Authentication required", body = ErrorBody),
        (status = 403, description = "Administrators only", body = ErrorBody),
    )
)]
async fn stats(State(svc): State<RealtimeService>, Caller(ctx): Caller) -> Result<Json<RealtimeStats>, HttpError> {
    if !ctx.is_privileged() {
        return Err(if ctx.principal == Principal::Anonymous {
            RealtimeError::Unauthenticated("authentication required".into())
        } else {
            RealtimeError::Forbidden("administrators only".into())
        }
        .into());
    }
    Ok(Json(svc.stats()))
}

/// The router, to be mounted at `/api/v1/realtime` inside the auth layer.
pub fn router(svc: RealtimeService) -> Router {
    let body_limit = svc.inner.cfg.max_message_bytes + 1024;
    Router::new()
        .route("/ws", get(ws))
        .route("/sse", get(sse))
        .route("/channels/{channel}/messages", post(publish))
        .route("/channels/{channel}/presence", get(presence))
        .route("/stats", get(stats))
        .layer(DefaultBodyLimit::max(body_limit))
        .layer(middleware::from_fn(with_request_id))
        .with_state(svc)
}

/// OpenAPI paths and schemas of the realtime API, merged into the server's
/// document by `forgebase-api`.
#[derive(OpenApi)]
#[openapi(
    paths(ws, sse, publish, presence, stats),
    components(schemas(SubscribeSpec, Member, PresenceList, PublishBody, RealtimeStats, ErrorBody, ErrorDetail)),
    tags((name = "realtime", description = "Realtime: WebSocket and SSE subscriptions to record changes and channels, presence"))
)]
struct RealtimeApiDoc;

/// The realtime OpenAPI fragment.
pub fn openapi() -> utoipa::openapi::OpenApi {
    RealtimeApiDoc::openapi()
}
