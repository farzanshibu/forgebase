//! Realtime subscriptions over WebSocket, with Server-Sent Events as the fallback.
//!
//! One connection (owned by a background task) carries every subscription of a client. It sends
//! heartbeats, reconnects with exponential backoff and resumes every subscription from its last
//! cursor, so events are neither lost nor duplicated across a drop. See [`Realtime::subscribe`].

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use crate::error::{ApiError, Error, Result, parse_retry_after};
use crate::http::{Req, Transport, describe, seg};

// ---- events ---------------------------------------------------------------------------------

/// A record was created, updated or deleted.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct RecordEvent {
    /// `record.created`, `record.updated` or `record.deleted`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Subscription id.
    pub id: String,
    /// Resume cursor: the SDK stores the highest one per subscription.
    pub cursor: u64,
    /// Collection name.
    pub collection: String,
    /// Id of the record.
    pub record_id: String,
    /// Tenant of the record, when tenant-scoped.
    #[serde(default)]
    pub tenant_id: Option<String>,
    /// Event time (RFC 3339).
    pub timestamp: String,
    /// The record (the deleted record's last state for deletes).
    #[serde(default)]
    pub data: Value,
}

/// A message published on a channel.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ChannelMessage {
    /// Subscription id.
    pub id: String,
    /// Channel name.
    pub channel: String,
    /// Publisher.
    pub from: String,
    /// Payload.
    #[serde(default)]
    pub data: Value,
    /// Publish time (RFC 3339).
    pub timestamp: String,
}

/// Someone present on a channel.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PresenceMember {
    /// Connection id.
    pub connection_id: String,
    /// Who it is.
    pub actor: String,
    /// Free-form metadata the member announced.
    #[serde(default)]
    pub meta: Value,
    /// Join time (RFC 3339).
    pub joined_at: String,
    /// Server instance (multi-node setups).
    #[serde(default)]
    pub instance: Option<String>,
}

/// A member joined or left a channel.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PresenceEvent {
    /// Subscription id.
    pub id: String,
    /// Channel name.
    pub channel: String,
    /// The member.
    pub member: PresenceMember,
}

/// The server could not replay from the cursor: reload your state, then continue from `cursor`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ResetEvent {
    /// Subscription id.
    pub id: String,
    /// `cursor_expired`, `cursor_invalid`, `too_far_behind`, ...
    pub reason: String,
    /// Cursor to continue from.
    pub cursor: u64,
}

/// Everything a [`Subscription`] delivers.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum RealtimeEvent {
    /// A record changed.
    Record(RecordEvent),
    /// A channel message.
    Message(ChannelMessage),
    /// A member joined a channel.
    PresenceJoin(PresenceEvent),
    /// A member left a channel.
    PresenceLeave(PresenceEvent),
    /// Replay was impossible; reload.
    Reset(ResetEvent),
    /// The server dropped this subscription (for example a rule change) or the connection failed for good.
    /// No more events follow.
    Error {
        /// Server or SDK error code.
        code: String,
        /// Message.
        message: String,
    },
}

fn decode_event(m: &Value) -> Option<RealtimeEvent> {
    let t = m.get("type")?.as_str()?;
    match t {
        "record.created" | "record.updated" | "record.deleted" => {
            serde_json::from_value(m.clone()).ok().map(RealtimeEvent::Record)
        }
        "message" => serde_json::from_value(m.clone()).ok().map(RealtimeEvent::Message),
        "presence.join" => serde_json::from_value(m.clone()).ok().map(RealtimeEvent::PresenceJoin),
        "presence.leave" => serde_json::from_value(m.clone()).ok().map(RealtimeEvent::PresenceLeave),
        "reset" => serde_json::from_value(m.clone()).ok().map(RealtimeEvent::Reset),
        _ => None,
    }
}

// ---- configuration --------------------------------------------------------------------------

/// Which transport to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RealtimeTransport {
    /// WebSocket, falling back to SSE when it cannot connect.
    #[default]
    Auto,
    /// WebSocket only.
    WebSocket,
    /// Server-Sent Events only (receive-only: no presence).
    Sse,
}

/// Realtime connection settings.
#[derive(Debug, Clone)]
pub struct RealtimeConfig {
    /// Transport choice (default [`RealtimeTransport::Auto`]).
    pub transport: RealtimeTransport,
    /// Consecutive failed WebSocket connects before `Auto` falls back to SSE (default 2).
    pub fallback_after: u32,
    /// First reconnect delay (default 500 ms); doubles with full jitter.
    pub reconnect_base: Duration,
    /// Longest reconnect delay (default 10 s).
    pub reconnect_max: Duration,
    /// Consecutive failed attempts before giving up (default 8). `None` retries forever.
    pub max_attempts: Option<u32>,
}

impl Default for RealtimeConfig {
    fn default() -> Self {
        Self {
            transport: RealtimeTransport::Auto,
            fallback_after: 2,
            reconnect_base: Duration::from_millis(500),
            reconnect_max: Duration::from_secs(10),
            max_attempts: Some(8),
        }
    }
}

/// What to subscribe to.
#[derive(Debug, Clone, Default)]
pub struct SubscribeOptions {
    /// Client-chosen id (1-64 of `[A-Za-z0-9_.:-]`); generated when `None`.
    pub id: Option<String>,
    /// Collection to watch.
    pub collection: Option<String>,
    /// Watch one record of the collection.
    pub record: Option<String>,
    /// Only events whose record matches this filter-language expression.
    pub filter: Option<String>,
    /// Typed values for `{:name}` placeholders in `filter`.
    pub params: Option<Value>,
    /// Custom pub/sub channel (instead of `collection`).
    pub channel: Option<String>,
    /// Replay every event after this cursor, then go live. Without it the stream starts at the head.
    pub cursor: Option<u64>,
}

impl SubscribeOptions {
    /// Watch a collection.
    #[must_use]
    pub fn collection(name: impl Into<String>) -> Self {
        Self {
            collection: Some(name.into()),
            ..Self::default()
        }
    }

    /// Watch a pub/sub channel.
    #[must_use]
    pub fn channel(name: impl Into<String>) -> Self {
        Self {
            channel: Some(name.into()),
            ..Self::default()
        }
    }

    /// Replay from a cursor (see [`Subscription::cursor`]).
    #[must_use]
    pub fn from_cursor(mut self, cursor: u64) -> Self {
        self.cursor = Some(cursor);
        self
    }

    /// Only matching records.
    #[must_use]
    pub fn filter(mut self, filter: impl Into<String>) -> Self {
        self.filter = Some(filter.into());
        self
    }

    fn spec(&self) -> Value {
        let mut m = serde_json::Map::new();
        let mut put = |k: &str, v: Option<Value>| {
            if let Some(v) = v {
                m.insert(k.into(), v);
            }
        };
        put("collection", self.collection.clone().map(Value::String));
        put("record", self.record.clone().map(Value::String));
        put("filter", self.filter.clone().map(Value::String));
        put("params", self.params.clone());
        put("channel", self.channel.clone().map(Value::String));
        Value::Object(m)
    }
}

// ---- public handle --------------------------------------------------------------------------

/// A live subscription. Read events with [`next`](Self::next); dropping it unsubscribes.
pub struct Subscription {
    id: String,
    rx: mpsc::UnboundedReceiver<RealtimeEvent>,
    cursor: Arc<Mutex<Option<u64>>>,
    cmd: mpsc::UnboundedSender<Cmd>,
}

impl Subscription {
    /// Subscription id.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Highest cursor delivered so far: store it to resume later with [`SubscribeOptions::from_cursor`].
    #[must_use]
    pub fn cursor(&self) -> Option<u64> {
        self.cursor.lock().ok().and_then(|g| *g)
    }

    /// The next event; `None` once the subscription or the client was closed.
    pub async fn next(&mut self) -> Option<RealtimeEvent> {
        self.rx.recv().await
    }

    /// The next event within `timeout`; `None` on timeout or close.
    pub async fn next_timeout(&mut self, timeout: Duration) -> Option<RealtimeEvent> {
        tokio::time::timeout(timeout, self.rx.recv()).await.ok().flatten()
    }

    /// Stops the subscription.
    pub fn unsubscribe(self) {
        drop(self);
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        let _ = self.cmd.send(Cmd::Unsubscribe { id: self.id.clone() });
    }
}

/// Realtime client: `client.realtime()`. Cheap to clone; clones share one connection.
#[derive(Clone)]
pub struct Realtime {
    inner: Arc<Inner>,
}

struct Inner {
    t: Arc<Transport>,
    cfg: RealtimeConfig,
    cmd: Mutex<Option<mpsc::UnboundedSender<Cmd>>>,
    seq: AtomicU64,
}

enum Cmd {
    Subscribe {
        id: String,
        spec: Value,
        cursor: Option<u64>,
        tx: mpsc::UnboundedSender<RealtimeEvent>,
        cell: Arc<Mutex<Option<u64>>>,
        reply: oneshot::Sender<Result<()>>,
    },
    Unsubscribe {
        id: String,
    },
    Publish {
        channel: String,
        data: Value,
        reply: oneshot::Sender<Result<()>>,
    },
    Presence {
        join: bool,
        channel: String,
        meta: Option<Value>,
        reply: oneshot::Sender<Result<Vec<PresenceMember>>>,
    },
    Close,
}

impl Realtime {
    pub(crate) fn new(t: Arc<Transport>, cfg: RealtimeConfig) -> Self {
        Self {
            inner: Arc::new(Inner {
                t,
                cfg,
                cmd: Mutex::new(None),
                seq: AtomicU64::new(0),
            }),
        }
    }

    fn sender(&self) -> mpsc::UnboundedSender<Cmd> {
        let mut g = self.inner.cmd.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(s) = g.as_ref()
            && !s.is_closed()
        {
            return s.clone();
        }
        let (tx, rx) = mpsc::unbounded_channel();
        let actor = Actor::new(self.inner.t.clone(), self.inner.cfg.clone(), rx);
        tokio::spawn(actor.run());
        *g = Some(tx.clone());
        tx
    }

    async fn call<T>(&self, make: impl FnOnce(oneshot::Sender<Result<T>>) -> Cmd) -> Result<T> {
        let (tx, rx) = oneshot::channel();
        self.sender().send(make(tx)).map_err(|_| closed())?;
        rx.await.map_err(|_| closed())?
    }

    /// Subscribes and resolves once the server acknowledged it (after replaying from `cursor`, if given).
    ///
    /// The first call opens the connection. A subscription the caller may not read is refused with the
    /// server's error.
    pub async fn subscribe(&self, opts: SubscribeOptions) -> Result<Subscription> {
        let id = opts.id.clone().unwrap_or_else(|| {
            format!(
                "sub-{}-{:08x}",
                self.inner.seq.fetch_add(1, Ordering::Relaxed) + 1,
                fastrand::u32(..)
            )
        });
        let (etx, erx) = mpsc::unbounded_channel();
        let cell = Arc::new(Mutex::new(opts.cursor));
        let cmd = self.sender();
        let (tx, rx) = oneshot::channel();
        cmd.send(Cmd::Subscribe {
            id: id.clone(),
            spec: opts.spec(),
            cursor: opts.cursor,
            tx: etx,
            cell: cell.clone(),
            reply: tx,
        })
        .map_err(|_| closed())?;
        rx.await.map_err(|_| closed())??;
        Ok(Subscription {
            id,
            rx: erx,
            cursor: cell,
            cmd,
        })
    }

    /// Publishes to a channel (over the socket when connected by WebSocket, else over REST).
    pub async fn publish(&self, channel: &str, data: Value) -> Result<()> {
        self.call(|reply| Cmd::Publish {
            channel: channel.to_owned(),
            data,
            reply,
        })
        .await
    }

    /// Joins a channel's presence (WebSocket only; subscribe to the channel first). Returns the members.
    pub async fn join_presence(&self, channel: &str, meta: Option<Value>) -> Result<Vec<PresenceMember>> {
        self.call(|reply| Cmd::Presence {
            join: true,
            channel: channel.to_owned(),
            meta,
            reply,
        })
        .await
    }

    /// Leaves a channel's presence.
    pub async fn leave_presence(&self, channel: &str) -> Result<()> {
        self.call(|reply| Cmd::Presence {
            join: false,
            channel: channel.to_owned(),
            meta: None,
            reply,
        })
        .await
        .map(drop)
    }

    /// Members currently present on a channel (REST: works on both transports).
    pub async fn presence(&self, channel: &str) -> Result<Vec<PresenceMember>> {
        let v = self
            .inner
            .t
            .value(Req::get(format!("/api/v1/realtime/channels/{}/presence", seg(channel))))
            .await?;
        serde_json::from_value(v.get("members").cloned().unwrap_or(Value::Array(Vec::new())))
            .map_err(|e| Error::InvalidResponse(format!("presence: {e}")))
    }

    /// Closes the connection and every subscription. A later `subscribe` opens a new connection.
    pub fn close(&self) {
        let mut g = self.inner.cmd.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(s) = g.take() {
            let _ = s.send(Cmd::Close);
        }
    }
}

fn closed() -> Error {
    Error::Realtime {
        code: "closed".into(),
        message: "realtime client closed".into(),
    }
}

fn rt_err(code: &str, message: impl Into<String>) -> Error {
    Error::Realtime {
        code: code.into(),
        message: message.into(),
    }
}

// ---- the connection task --------------------------------------------------------------------

struct Sub {
    spec: Value,
    cursor: Option<u64>,
    cell: Arc<Mutex<Option<u64>>>,
    tx: mpsc::UnboundedSender<RealtimeEvent>,
    active: bool,
    reply: Option<oneshot::Sender<Result<()>>>,
}

enum Pending {
    Subscribe(String),
    Ack(oneshot::Sender<Result<()>>),
    Presence(oneshot::Sender<Result<Vec<PresenceMember>>>),
    Ignore,
}

/// Why a connection attempt or a connection ended.
enum End {
    /// `Close` or all handles dropped: stop for good.
    Stop,
    /// Connected, then lost: reconnect (resuming from cursors).
    Lost,
    /// Subscriptions changed (SSE restarts to apply them).
    Restart,
    /// The socket asked for fresh credentials (close code 4001).
    Reauth,
}

struct Actor {
    t: Arc<Transport>,
    cfg: RealtimeConfig,
    rx: mpsc::UnboundedReceiver<Cmd>,
    subs: HashMap<String, Sub>,
    use_sse: bool,
    seq: u64,
}

trait AsyncStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncStream for T {}
type WsStream = tokio_tungstenite::WebSocketStream<Box<dyn AsyncStream>>;

impl Actor {
    fn new(t: Arc<Transport>, cfg: RealtimeConfig, rx: mpsc::UnboundedReceiver<Cmd>) -> Self {
        let use_sse = cfg.transport == RealtimeTransport::Sse;
        Self {
            t,
            cfg,
            rx,
            subs: HashMap::new(),
            use_sse,
            seq: 0,
        }
    }

    async fn run(mut self) {
        let mut failures = 0u32;
        let mut ws_failures = 0u32;
        let outcome: Error = loop {
            let res = if self.use_sse {
                self.run_sse().await
            } else {
                self.run_ws().await
            };
            match res {
                Ok(End::Stop) => break closed(),
                Ok(End::Restart | End::Reauth) => {
                    failures = 0;
                    ws_failures = 0;
                    continue;
                }
                Ok(End::Lost) => {
                    failures = 0;
                    ws_failures = 0;
                    if self.pause(Duration::from_millis(50)).await {
                        break closed();
                    }
                }
                Err(e) => {
                    // Credentials the server rejects: rotate them once; otherwise this is final.
                    if let Some(a) = e.api() {
                        if a.status == 401 && failures == 0 && self.refresh_credentials().await {
                            failures += 1;
                            continue;
                        }
                        if (400..500).contains(&a.status) && a.status != 429 {
                            break e;
                        }
                    }
                    failures += 1;
                    ws_failures += 1;
                    if !self.use_sse
                        && self.cfg.transport == RealtimeTransport::Auto
                        && ws_failures >= self.cfg.fallback_after.max(1)
                    {
                        self.use_sse = true;
                        failures = 0;
                        continue;
                    }
                    if self.cfg.max_attempts.is_some_and(|m| failures >= m) {
                        break e;
                    }
                    let d = self.backoff(failures, e.api().and_then(|a| a.retry_after));
                    if self.pause(d).await {
                        break closed();
                    }
                }
            }
        };
        let (code, message) = match &outcome {
            Error::Realtime { code, message } => (code.clone(), message.clone()),
            other => (other.code().to_owned(), other.to_string()),
        };
        for (_, mut s) in self.subs.drain() {
            if let Some(r) = s.reply.take() {
                let _ = r.send(Err(clone_err(&outcome)));
            }
            let _ = s.tx.send(RealtimeEvent::Error {
                code: code.clone(),
                message: message.clone(),
            });
        }
    }

    fn backoff(&self, attempt: u32, retry_after: Option<Duration>) -> Duration {
        if let Some(r) = retry_after {
            return r;
        }
        let exp = self
            .cfg
            .reconnect_base
            .saturating_mul(2u32.saturating_pow(attempt.saturating_sub(1)));
        let d = exp.min(self.cfg.reconnect_max);
        Duration::from_millis(50).max(d.mul_f64(fastrand::f64()))
    }

    /// Sleeps while still serving commands. Returns true when the actor must stop.
    async fn pause(&mut self, d: Duration) -> bool {
        let end = Instant::now() + d;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            tokio::select! {
                () = tokio::time::sleep(left) => return false,
                cmd = self.rx.recv() => match cmd {
                    Some(c) => if self.absorb(c).await { return true; },
                    None => return true,
                },
            }
        }
    }

    async fn refresh_credentials(&self) -> bool {
        let sent = self
            .t
            .credentials()
            .await
            .into_iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
            .map(|(_, v)| v);
        self.t.on_unauthorized(sent.as_deref()).await
    }

    /// Handles a command while no connection is serving it. Returns true to stop.
    async fn absorb(&mut self, cmd: Cmd) -> bool {
        match cmd {
            Cmd::Subscribe {
                id,
                spec,
                cursor,
                tx,
                cell,
                reply,
            } => {
                self.subs.insert(
                    id,
                    Sub {
                        spec,
                        cursor,
                        cell,
                        tx,
                        active: false,
                        reply: Some(reply),
                    },
                );
            }
            Cmd::Unsubscribe { id } => {
                self.subs.remove(&id);
            }
            Cmd::Publish { channel, data, reply } => {
                let _ = reply.send(self.publish_rest(&channel, data).await);
            }
            Cmd::Presence { reply, .. } => {
                let _ = reply.send(Err(rt_err("not_connected", "presence needs an open WebSocket")));
            }
            Cmd::Close => return true,
        }
        false
    }

    async fn publish_rest(&self, channel: &str, data: Value) -> Result<()> {
        self.t
            .unit(
                Req::post(format!("/api/v1/realtime/channels/{}/messages", seg(channel)))
                    .json_value(json!({"message": data})),
            )
            .await
    }

    fn deliver(&mut self, m: &Value) {
        let Some(id) = m.get("id").and_then(Value::as_str) else {
            return;
        };
        let Some(sub) = self.subs.get_mut(id) else { return };
        let Some(ev) = decode_event(m) else { return };
        if let Some(c) = m.get("cursor").and_then(Value::as_u64) {
            let is_reset = matches!(ev, RealtimeEvent::Reset(_));
            if sub.cursor.is_none_or(|cur| c > cur) || is_reset {
                sub.cursor = Some(c);
                if let Ok(mut g) = sub.cell.lock() {
                    *g = Some(c);
                }
            }
        }
        let _ = sub.tx.send(ev);
    }

    fn mark_subscribed(&mut self, id: &str, cursor: Option<u64>) {
        if let Some(s) = self.subs.get_mut(id) {
            s.active = true;
            if let Some(c) = cursor
                && s.cursor.is_none()
            {
                s.cursor = Some(c);
                if let Ok(mut g) = s.cell.lock() {
                    *g = Some(c);
                }
            }
            if let Some(r) = s.reply.take() {
                let _ = r.send(Ok(()));
            }
        }
    }

    /// The server refused (or dropped) one subscription.
    fn fail_sub(&mut self, id: &str, code: &str, message: &str) {
        if let Some(mut s) = self.subs.remove(id) {
            match s.reply.take() {
                Some(r) => {
                    let _ = r.send(Err(rt_err(code, message)));
                }
                None => {
                    let _ = s.tx.send(RealtimeEvent::Error {
                        code: code.into(),
                        message: message.into(),
                    });
                }
            }
        }
    }

    // ---- WebSocket ----

    async fn ws_connect(&self) -> Result<(WsStream, Duration)> {
        let base = url::Url::parse(&self.t.base).map_err(|e| Error::Config(format!("bad base URL: {e}")))?;
        let secure = base.scheme() == "https";
        let mut url = base.clone();
        url.set_scheme(if secure { "wss" } else { "ws" })
            .map_err(|()| Error::Config("cannot derive the WebSocket URL".into()))?;
        url.set_path(&format!("{}/api/v1/realtime/ws", base.path().trim_end_matches('/')));
        let host = base
            .host_str()
            .ok_or_else(|| Error::Config("base URL has no host".into()))?
            .to_owned();
        let port = base.port_or_known_default().unwrap_or(if secure { 443 } else { 80 });

        let mut request = url
            .as_str()
            .into_client_request()
            .map_err(|e| Error::Config(format!("bad WebSocket request: {e}")))?;
        for (k, v) in self
            .t
            .credentials()
            .await
            .into_iter()
            .chain(self.t.extra_headers.iter().cloned())
        {
            if let (Ok(name), Ok(val)) = (
                k.parse::<tokio_tungstenite::tungstenite::http::HeaderName>(),
                v.parse::<tokio_tungstenite::tungstenite::http::HeaderValue>(),
            ) {
                request.headers_mut().insert(name, val);
            }
        }

        let tcp = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect((host.as_str(), port)))
            .await
            .map_err(|_| Error::Network(format!("connecting to {host}:{port} timed out")))?
            .map_err(|e| Error::Network(format!("connecting to {host}:{port}: {e}")))?;
        let _ = tcp.set_nodelay(true);
        let stream: Box<dyn AsyncStream> = if secure {
            let mut roots = rustls::RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .map_err(|e| Error::Config(format!("TLS setup: {e}")))?
                .with_root_certificates(roots)
                .with_no_client_auth();
            let name = rustls::pki_types::ServerName::try_from(host.clone())
                .map_err(|e| Error::Config(format!("bad TLS server name {host:?}: {e}")))?;
            let tls = tokio_rustls::TlsConnector::from(Arc::new(cfg))
                .connect(name, tcp)
                .await
                .map_err(|e| Error::Network(format!("TLS handshake with {host}: {e}")))?;
            Box::new(tls)
        } else {
            Box::new(tcp)
        };

        let (mut ws, _resp) = tokio::time::timeout(
            Duration::from_secs(10),
            tokio_tungstenite::client_async(request, stream),
        )
        .await
        .map_err(|_| Error::Network("WebSocket handshake timed out".into()))?
        .map_err(ws_handshake_error)?;

        // The server greets first: `{"type":"welcome","heartbeat_ms":...}`.
        let hb = tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(msg) = ws.next().await {
                match msg.map_err(|e| Error::Network(format!("WebSocket: {e}")))? {
                    Message::Text(t) => {
                        let v: Value = serde_json::from_str(t.as_str()).unwrap_or(Value::Null);
                        match v.get("type").and_then(Value::as_str) {
                            Some("welcome") => {
                                return Ok(Duration::from_millis(
                                    v.get("heartbeat_ms")
                                        .and_then(Value::as_u64)
                                        .filter(|n| *n > 0)
                                        .unwrap_or(25_000),
                                ));
                            }
                            Some("error") => {
                                return Err(rt_err(
                                    v.get("code").and_then(Value::as_str).unwrap_or("error"),
                                    v.get("message").and_then(Value::as_str).unwrap_or("connection refused"),
                                ));
                            }
                            _ => {}
                        }
                    }
                    Message::Close(f) => return Err(Error::Network(format!("WebSocket closed before welcome: {f:?}"))),
                    _ => {}
                }
            }
            Err(Error::Network("WebSocket closed before welcome".into()))
        })
        .await
        .map_err(|_| Error::Network("no welcome from the server".into()))??;
        Ok((ws, hb))
    }

    async fn run_ws(&mut self) -> Result<End> {
        while let Ok(c) = self.rx.try_recv() {
            if self.absorb(c).await {
                return Ok(End::Stop);
            }
        }
        let (mut ws, heartbeat) = self.ws_connect().await?;
        let mut auth_rx = self.t.auth.watch();
        let mut pending: HashMap<String, Pending> = HashMap::new();
        let mut last_rx = Instant::now();
        let mut tick = tokio::time::interval(heartbeat);
        tick.tick().await;

        // Resubscribe everything from the stored cursors.
        let ids: Vec<String> = self.subs.keys().cloned().collect();
        for id in ids {
            self.ws_subscribe(&mut ws, &mut pending, &id).await?;
        }

        loop {
            tokio::select! {
                msg = ws.next() => {
                    let Some(msg) = msg else { return Ok(End::Lost) };
                    last_rx = Instant::now();
                    match msg {
                        Ok(Message::Text(t)) => {
                            if let Ok(v) = serde_json::from_str::<Value>(t.as_str()) {
                                self.ws_message(&v, &mut pending);
                            }
                        }
                        Ok(Message::Close(f)) => {
                            let code: u16 = f.as_ref().map_or(1005, |f| f.code.into());
                            fail_pending(&mut pending, &rt_err("connection_closed", "the WebSocket closed"));
                            if code == 4001 && self.refresh_credentials().await {
                                return Ok(End::Reauth);
                            }
                            return Ok(End::Lost);
                        }
                        Ok(_) => {}
                        Err(_) => {
                            fail_pending(&mut pending, &rt_err("connection_lost", "the WebSocket dropped"));
                            return Ok(End::Lost);
                        }
                    }
                }
                cmd = self.rx.recv() => {
                    let Some(cmd) = cmd else { let _ = ws.close(None).await; return Ok(End::Stop) };
                    if matches!(cmd, Cmd::Close) {
                        let _ = ws.close(None).await;
                        return Ok(End::Stop);
                    }
                    if self.ws_command(&mut ws, &mut pending, cmd).await.is_err() {
                        fail_pending(&mut pending, &rt_err("connection_lost", "the WebSocket dropped"));
                        return Ok(End::Lost);
                    }
                }
                _ = tick.tick() => {
                    if last_rx.elapsed() > heartbeat.mul_f64(2.5) {
                        let _ = ws.close(None).await;
                        fail_pending(&mut pending, &rt_err("heartbeat_timeout", "no data from the server"));
                        return Ok(End::Lost);
                    }
                    if ws.send(Message::text(json!({"type": "ping"}).to_string())).await.is_err() {
                        return Ok(End::Lost);
                    }
                }
                changed = auth_rx.changed() => {
                    if changed.is_ok() {
                        // The access token rotated: tell the server, which keeps the connection.
                        let bearer = self.t.credentials().await.into_iter().find(|(k, _)| k.eq_ignore_ascii_case("authorization"));
                        if let Some((_, v)) = bearer {
                            let token = v.strip_prefix("Bearer ").unwrap_or(&v).to_owned();
                            let _ = ws.send(Message::text(json!({"type": "auth", "token": token}).to_string())).await;
                        }
                    }
                }
            }
        }
    }

    fn next_ref(&mut self) -> String {
        self.seq += 1;
        self.seq.to_string()
    }

    async fn ws_send(&mut self, ws: &mut WsStream, msg: Value) -> Result<()> {
        ws.send(Message::text(msg.to_string()))
            .await
            .map_err(|e| Error::Network(format!("WebSocket send: {e}")))
    }

    async fn ws_subscribe(
        &mut self,
        ws: &mut WsStream,
        pending: &mut HashMap<String, Pending>,
        id: &str,
    ) -> Result<()> {
        let Some(s) = self.subs.get(id) else { return Ok(()) };
        let mut m = s.spec.clone();
        m["type"] = json!("subscribe");
        m["id"] = json!(id);
        if let Some(c) = s.cursor {
            m["cursor"] = json!(c);
        }
        let r = self.next_ref();
        m["ref"] = json!(r);
        pending.insert(r, Pending::Subscribe(id.to_owned()));
        self.ws_send(ws, m).await
    }

    async fn ws_command(&mut self, ws: &mut WsStream, pending: &mut HashMap<String, Pending>, cmd: Cmd) -> Result<()> {
        match cmd {
            Cmd::Subscribe {
                id,
                spec,
                cursor,
                tx,
                cell,
                reply,
            } => {
                self.subs.insert(
                    id.clone(),
                    Sub {
                        spec,
                        cursor,
                        cell,
                        tx,
                        active: false,
                        reply: Some(reply),
                    },
                );
                self.ws_subscribe(ws, pending, &id).await
            }
            Cmd::Unsubscribe { id } => {
                if self.subs.remove(&id).is_some() {
                    let r = self.next_ref();
                    pending.insert(r.clone(), Pending::Ignore);
                    self.ws_send(ws, json!({"type": "unsubscribe", "id": id, "ref": r}))
                        .await?;
                }
                Ok(())
            }
            Cmd::Publish { channel, data, reply } => {
                let r = self.next_ref();
                pending.insert(r.clone(), Pending::Ack(reply));
                self.ws_send(
                    ws,
                    json!({"type": "publish", "channel": channel, "data": data, "ref": r}),
                )
                .await
            }
            Cmd::Presence {
                join,
                channel,
                meta,
                reply,
            } => {
                let r = self.next_ref();
                pending.insert(r.clone(), Pending::Presence(reply));
                let m = if join {
                    json!({"type": "presence.join", "channel": channel, "meta": meta, "ref": r})
                } else {
                    json!({"type": "presence.leave", "channel": channel, "ref": r})
                };
                self.ws_send(ws, m).await
            }
            Cmd::Close => Ok(()),
        }
    }

    fn ws_message(&mut self, m: &Value, pending: &mut HashMap<String, Pending>) {
        let rf = match m.get("ref") {
            Some(Value::String(s)) => Some(s.clone()),
            Some(Value::Number(n)) => Some(n.to_string()),
            _ => None,
        };
        let ty = m.get("type").and_then(Value::as_str).unwrap_or_default();
        if let Some(p) = rf.and_then(|r| pending.remove(&r)) {
            let failed = ty == "error";
            let code = m.get("code").and_then(Value::as_str).unwrap_or("error");
            let message = m.get("message").and_then(Value::as_str).unwrap_or("realtime error");
            match p {
                Pending::Subscribe(id) => {
                    if failed {
                        self.fail_sub(&id, code, message);
                    } else {
                        self.mark_subscribed(&id, m.get("cursor").and_then(Value::as_u64));
                    }
                }
                Pending::Ack(r) => {
                    let _ = r.send(if failed { Err(rt_err(code, message)) } else { Ok(()) });
                }
                Pending::Presence(r) => {
                    let _ = r.send(if failed {
                        Err(rt_err(code, message))
                    } else {
                        Ok(
                            serde_json::from_value(m.get("members").cloned().unwrap_or(Value::Array(Vec::new())))
                                .unwrap_or_default(),
                        )
                    });
                }
                Pending::Ignore => {}
            }
            return;
        }
        match ty {
            "record.created" | "record.updated" | "record.deleted" | "message" | "presence.join" | "presence.leave"
            | "reset" => self.deliver(m),
            "error" => {
                if let Some(id) = m.get("id").and_then(Value::as_str) {
                    self.fail_sub(
                        id,
                        m.get("code").and_then(Value::as_str).unwrap_or("error"),
                        m.get("message").and_then(Value::as_str).unwrap_or(""),
                    );
                }
            }
            _ => {}
        }
    }

    // ---- SSE ----

    async fn run_sse(&mut self) -> Result<End> {
        // Nothing to stream yet: serve commands until a subscription arrives.
        while self.subs.is_empty() {
            match self.rx.recv().await {
                Some(c) => {
                    if self.absorb(c).await {
                        return Ok(End::Stop);
                    }
                }
                None => return Ok(End::Stop),
            }
        }
        while let Ok(c) = self.rx.try_recv() {
            if self.absorb(c).await {
                return Ok(End::Stop);
            }
        }
        let specs: Vec<Value> = self
            .subs
            .iter()
            .map(|(id, s)| {
                let mut m = s.spec.clone();
                m["id"] = json!(id);
                if let Some(c) = s.cursor {
                    m["cursor"] = json!(c);
                }
                m
            })
            .collect();
        let mut req = self
            .t
            .http
            .get(self.t.url("/api/v1/realtime/sse"))
            .query(&[("subscriptions", Value::Array(specs).to_string())])
            .header("Accept", "text/event-stream");
        for (k, v) in self
            .t
            .credentials()
            .await
            .into_iter()
            .chain(self.t.extra_headers.iter().cloned())
        {
            req = req.header(k, v);
        }
        // The overall request timeout of the shared client would cut a long-lived stream: lift it here.
        let res = req
            .timeout(Duration::from_secs(60 * 60 * 24))
            .send()
            .await
            .map_err(|e| Error::Network(describe(&e)))?;
        if !res.status().is_success() {
            let status = res.status().as_u16();
            let ra = parse_retry_after(res.headers().get("retry-after").and_then(|v| v.to_str().ok()));
            let rid = res
                .headers()
                .get("x-request-id")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            let body = res
                .bytes()
                .await
                .ok()
                .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
            return Err(Error::Api(Box::new(ApiError::from_response(status, body, rid, ra))));
        }

        let mut stream = res.bytes_stream();
        let mut buf = String::new();
        let idle = Duration::from_secs(75);
        let mut deadline = Instant::now() + idle;
        loop {
            tokio::select! {
                chunk = stream.next() => {
                    let Some(chunk) = chunk else { return Ok(End::Lost) };
                    let Ok(chunk) = chunk else { return Ok(End::Lost) };
                    deadline = Instant::now() + idle;
                    buf.push_str(&String::from_utf8_lossy(&chunk).replace("\r\n", "\n"));
                    while let Some(i) = buf.find("\n\n") {
                        let block: String = buf.drain(..i + 2).collect();
                        if let Some((event, data)) = parse_sse_block(&block) {
                            if event == "close" {
                                return Ok(End::Lost);
                            }
                            let mut m = data;
                            if m.get("type").is_none() {
                                m["type"] = json!(event);
                            }
                            self.sse_message(&m);
                        }
                    }
                }
                cmd = self.rx.recv() => {
                    let Some(cmd) = cmd else { return Ok(End::Stop) };
                    let reconnect = matches!(cmd, Cmd::Subscribe { .. } | Cmd::Unsubscribe { .. });
                    if self.absorb(cmd).await { return Ok(End::Stop); }
                    if reconnect { return Ok(End::Restart); }
                }
                () = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => return Ok(End::Lost),
            }
        }
    }

    fn sse_message(&mut self, m: &Value) {
        match m.get("type").and_then(Value::as_str).unwrap_or_default() {
            "subscribed" => {
                if let Some(id) = m.get("id").and_then(Value::as_str) {
                    let id = id.to_owned();
                    self.mark_subscribed(&id, m.get("cursor").and_then(Value::as_u64));
                }
            }
            "error" => {
                if let Some(id) = m.get("id").and_then(Value::as_str) {
                    let id = id.to_owned();
                    self.fail_sub(
                        &id,
                        m.get("code").and_then(Value::as_str).unwrap_or("error"),
                        m.get("message").and_then(Value::as_str).unwrap_or(""),
                    );
                }
            }
            _ => self.deliver(m),
        }
    }
}

fn clone_err(e: &Error) -> Error {
    match e {
        Error::Api(a) => Error::Api(a.clone()),
        Error::Network(s) => Error::Network(s.clone()),
        Error::InvalidResponse(s) => Error::InvalidResponse(s.clone()),
        Error::Realtime { code, message } => Error::Realtime {
            code: code.clone(),
            message: message.clone(),
        },
        Error::Config(s) => Error::Config(s.clone()),
    }
}

fn fail_pending(pending: &mut HashMap<String, Pending>, e: &Error) {
    for (_, p) in pending.drain() {
        match p {
            Pending::Ack(r) => {
                let _ = r.send(Err(clone_err(e)));
            }
            Pending::Presence(r) => {
                let _ = r.send(Err(clone_err(e)));
            }
            Pending::Subscribe(_) | Pending::Ignore => {}
        }
    }
}

fn ws_handshake_error(e: tokio_tungstenite::tungstenite::Error) -> Error {
    use tokio_tungstenite::tungstenite::Error as E;
    match e {
        E::Http(resp) => {
            let body = resp
                .body()
                .as_ref()
                .and_then(|b| serde_json::from_slice::<Value>(b).ok());
            let rid = resp
                .headers()
                .get("x-request-id")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            Error::Api(Box::new(ApiError::from_response(
                resp.status().as_u16(),
                body,
                rid,
                None,
            )))
        }
        other => Error::Network(format!("WebSocket handshake: {other}")),
    }
}

/// Parses one SSE block into `(event, json data)`; comments and blocks without data give `None`.
fn parse_sse_block(block: &str) -> Option<(String, Value)> {
    let mut event = "message".to_owned();
    let mut data = Vec::new();
    for line in block.lines() {
        if line.starts_with(':') {
            continue;
        }
        let (field, val) = line
            .split_once(':')
            .map_or((line, ""), |(f, v)| (f, v.strip_prefix(' ').unwrap_or(v)));
        match field {
            "event" => event = val.to_owned(),
            "data" => data.push(val),
            _ => {}
        }
    }
    if data.is_empty() {
        return None;
    }
    serde_json::from_str(&data.join("\n")).ok().map(|v| (event, v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sse_blocks() {
        let (e, d) = parse_sse_block("event: record.created\ndata: {\"id\":\"s\",\"cursor\":3}\n\n").expect("block");
        assert_eq!(e, "record.created");
        assert_eq!(d["cursor"], 3);
        assert!(parse_sse_block(": keep-alive\n\n").is_none());
        let (e, d) = parse_sse_block("data: {\"a\":\ndata: 1}\n\n").expect("multi-line data");
        assert_eq!(e, "message");
        assert_eq!(d["a"], 1);
        assert!(parse_sse_block("data: not json\n\n").is_none());
    }

    #[test]
    fn decodes_events() {
        let m = json!({"type":"record.updated","id":"s","cursor":9,"collection":"posts","record_id":"r","timestamp":"t","data":{"n":1}});
        match decode_event(&m) {
            Some(RealtimeEvent::Record(r)) => {
                assert_eq!(
                    (r.cursor, r.collection.as_str(), r.data["n"].as_i64()),
                    (9, "posts", Some(1))
                );
            }
            other => panic!("unexpected {other:?}"),
        }
        let reset = json!({"type":"reset","id":"s","reason":"cursor_expired","cursor":4});
        assert!(matches!(decode_event(&reset), Some(RealtimeEvent::Reset(r)) if r.cursor == 4));
        assert!(decode_event(&json!({"type":"welcome"})).is_none());
    }

    #[test]
    fn subscribe_spec_only_carries_set_fields() {
        let s = SubscribeOptions::collection("posts").filter("n > 1").spec();
        assert_eq!(s, json!({"collection": "posts", "filter": "n > 1"}));
        assert_eq!(SubscribeOptions::channel("room").spec(), json!({"channel": "room"}));
    }
}
