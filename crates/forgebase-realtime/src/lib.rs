//! ForgeBase realtime (FB-40, SPEC §9, ADR-0011).
//!
//! * **Transactional outbox** ([`outbox`]): every record change made through
//!   the data service writes an outbox row in the same transaction
//!   ([`forgebase_data::ChangeHook`]), so an event exists exactly when its
//!   change committed. The outbox `seq` is the event cursor.
//! * **Dispatcher**: tails the outbox (woken on commit, polling as a fallback)
//!   and publishes each committed event once to an in-process bus.
//! * **Connections** (WebSocket and SSE, [`router`]): subscriptions to a
//!   collection (optionally filtered), one record, or a custom channel;
//!   replay after reconnect from a cursor, without gaps or duplicates;
//!   presence; heartbeats; per-connection and per-principal limits; bounded
//!   queues with slow consumers closed (`4008`).
//! * **Authorisation**: a subscription is authorised when it is made, and
//!   every event is re-checked against the subscriber's *current* access
//!   ([`forgebase_data::DataService::view_snapshot`]) before it is sent, so a
//!   rule change takes effect on the next event; a revoked or expired
//!   credential closes its sockets (`4001`).
//! * **Fan-out** ([`fanout`]): optional, for channel messages and presence
//!   across instances (Redis/Valkey pub/sub).

pub mod fanout;
pub mod outbox;
pub mod presence;
pub mod protocol;

mod conn;
mod error;
mod http;

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use forgebase_auth::AuthService;
use forgebase_data::{AccessContext, Action, DataService};
use serde::Serialize;
use serde_json::Value as Json;
use tokio::sync::{Notify, broadcast, watch};
use tokio::task::JoinHandle;
use utoipa::ToSchema;

pub use error::RealtimeError;
pub use fanout::{FanOut, RedisFanOut};
pub use http::{openapi, router};
pub use outbox::Event;
pub use presence::{Ephemeral, EphemeralKind, Member};

/// Limits and timings. The defaults suit one embedded instance.
#[derive(Debug, Clone)]
pub struct RealtimeConfig {
    /// Open connections per instance.
    pub max_connections: usize,
    /// Open connections per principal (anonymous callers: per client IP).
    pub max_connections_per_principal: usize,
    /// Subscriptions per connection.
    pub max_subscriptions_per_connection: usize,
    /// Largest client message (WebSocket frame or channel publish body).
    pub max_message_bytes: usize,
    /// Messages queued for one connection before it counts as slow.
    pub outbound_queue: usize,
    /// How long a full queue may block before the connection is closed as a
    /// slow consumer (`4008`).
    pub send_timeout: Duration,
    /// Server ping interval (WebSocket ping frames, SSE comments).
    pub heartbeat_interval: Duration,
    /// A WebSocket that sends nothing (not even a pong) for this long is
    /// closed (`4002`).
    pub idle_timeout: Duration,
    /// How often a connection re-validates its credential (revoked or expired
    /// sessions and keys close with `4001`).
    pub revalidate_interval: Duration,
    /// Dispatcher polling interval (commits in this process wake it at once).
    pub poll_interval: Duration,
    /// Events buffered on the in-process bus per lagging connection before it
    /// catches up from the outbox.
    pub bus_capacity: usize,
    /// Most outbox events read for one replay; a client further behind gets a
    /// `reset`.
    pub max_replay: i64,
    /// Outbox events older than this are pruned.
    pub retention: Duration,
    /// At most this many outbox events are kept.
    pub max_retained_events: i64,
    /// How often old outbox events are pruned.
    pub prune_interval: Duration,
    /// Channel messages a connection may publish per second (burst = rate).
    pub publish_per_second: u32,
    /// Largest presence `meta` object (serialized JSON bytes).
    pub max_presence_meta_bytes: usize,
}

impl Default for RealtimeConfig {
    fn default() -> Self {
        Self {
            max_connections: 10_000,
            max_connections_per_principal: 32,
            max_subscriptions_per_connection: 100,
            max_message_bytes: 64 * 1024,
            outbound_queue: 1024,
            send_timeout: Duration::from_secs(10),
            heartbeat_interval: Duration::from_secs(25),
            idle_timeout: Duration::from_secs(70),
            revalidate_interval: Duration::from_secs(10),
            poll_interval: Duration::from_secs(1),
            bus_capacity: 4096,
            max_replay: 10_000,
            retention: Duration::from_secs(24 * 3600),
            max_retained_events: 1_000_000,
            prune_interval: Duration::from_secs(60),
            publish_per_second: 50,
            max_presence_meta_bytes: 1024,
        }
    }
}

/// Counters of one instance (also exported as Prometheus metrics).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, ToSchema)]
pub struct RealtimeStats {
    /// Open connections.
    pub connections: u64,
    /// Active subscriptions.
    pub subscriptions: u64,
    /// Newest event cursor published to the bus.
    pub head: i64,
    /// Events published to the bus.
    pub events_dispatched: u64,
    /// Events sent to subscribers (after authorisation).
    pub events_delivered: u64,
    /// Events withheld because the subscriber may not see them.
    pub events_withheld: u64,
    /// Connections closed as slow consumers.
    pub slow_consumers: u64,
    /// Connections closed because their credential became invalid.
    pub revoked: u64,
    /// Connections refused by a limit.
    pub rejected: u64,
}

#[derive(Debug, Default)]
struct Counters {
    connections: AtomicUsize,
    subscriptions: AtomicUsize,
    events_dispatched: AtomicU64,
    events_delivered: AtomicU64,
    events_withheld: AtomicU64,
    slow_consumers: AtomicU64,
    revoked: AtomicU64,
    rejected: AtomicU64,
}

/// The realtime service. Cheap to clone.
#[derive(Debug, Clone)]
pub struct RealtimeService {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    data: DataService,
    auth: Option<AuthService>,
    cfg: RealtimeConfig,
    instance: String,
    bus: broadcast::Sender<Arc<Event>>,
    ephemeral: broadcast::Sender<Arc<Ephemeral>>,
    fanout: Option<Arc<dyn FanOut>>,
    rooms: presence::Rooms,
    head: AtomicI64,
    counters: Counters,
    per_principal: Mutex<HashMap<String, usize>>,
    shutdown: watch::Sender<bool>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        for t in self.tasks.get_mut().unwrap_or_else(|p| p.into_inner()).drain(..) {
            t.abort();
        }
    }
}

/// Holds one connection slot; released on drop.
#[derive(Debug)]
pub(crate) struct ConnSlot {
    svc: RealtimeService,
    key: String,
}

impl Drop for ConnSlot {
    fn drop(&mut self) {
        let inner = &self.svc.inner;
        inner.counters.connections.fetch_sub(1, Ordering::SeqCst);
        metrics::gauge!("forgebase_realtime_connections").decrement(1.0);
        let mut m = inner.per_principal.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(n) = m.get_mut(&self.key) {
            *n -= 1;
            if *n == 0 {
                m.remove(&self.key);
            }
        }
    }
}

impl RealtimeService {
    /// Starts realtime on `data`: installs the outbox hook (so `data` must be
    /// the process's one data service, on a database migrated to 0011),
    /// starts the dispatcher, the pruner and, with `fanout`, the cross-instance
    /// receiver. `auth` authenticates query-string tokens and re-validates
    /// credentials; without it connections keep the identity they opened with.
    pub async fn start(
        data: DataService,
        auth: Option<AuthService>,
        cfg: RealtimeConfig,
        fanout: Option<Arc<dyn FanOut>>,
    ) -> Result<Self, RealtimeError> {
        let notify = Arc::new(Notify::new());
        let (_, head) = outbox::bounds(data.backend().as_ref()).await?;
        if !data.set_change_hook(Arc::new(outbox::OutboxHook { notify: notify.clone() })) {
            return Err(RealtimeError::Internal(
                "a change hook is already installed on this data service".into(),
            ));
        }
        let (bus, _) = broadcast::channel(cfg.bus_capacity.max(16));
        let (ephemeral, _) = broadcast::channel(cfg.bus_capacity.max(16));
        let (shutdown, _) = watch::channel(false);
        let svc = Self {
            inner: Arc::new(Inner {
                data,
                auth,
                cfg,
                instance: uuid::Uuid::new_v4().simple().to_string(),
                bus,
                ephemeral,
                fanout,
                rooms: presence::Rooms::default(),
                head: AtomicI64::new(head),
                counters: Counters::default(),
                per_principal: Mutex::new(HashMap::new()),
                shutdown,
                tasks: Mutex::new(Vec::new()),
            }),
        };
        let weak = Arc::downgrade(&svc.inner);
        let mut tasks = vec![tokio::spawn(dispatcher(weak.clone(), notify))];
        tasks.push(tokio::spawn(pruner(weak.clone())));
        if let Some(f) = &svc.inner.fanout {
            let rx = f.subscribe().await?;
            tasks.push(tokio::spawn(fanout_receiver(weak, rx)));
        }
        *svc.inner.tasks.lock().unwrap_or_else(|p| p.into_inner()) = tasks;
        Ok(svc)
    }

    /// Closes every connection (`1001`, SSE streams end) and stops the
    /// background tasks. Call before the HTTP server's graceful shutdown.
    pub fn shutdown(&self) {
        let _ = self.inner.shutdown.send(true);
        for t in self.inner.tasks.lock().unwrap_or_else(|p| p.into_inner()).drain(..) {
            t.abort();
        }
    }

    /// The data service.
    pub fn data(&self) -> &DataService {
        &self.inner.data
    }

    /// Configuration.
    pub fn config(&self) -> &RealtimeConfig {
        &self.inner.cfg
    }

    /// This instance's id (presence members carry it).
    pub fn instance_id(&self) -> &str {
        &self.inner.instance
    }

    /// Current counters.
    pub fn stats(&self) -> RealtimeStats {
        let c = &self.inner.counters;
        RealtimeStats {
            connections: c.connections.load(Ordering::SeqCst) as u64,
            subscriptions: c.subscriptions.load(Ordering::SeqCst) as u64,
            head: self.inner.head.load(Ordering::SeqCst),
            events_dispatched: c.events_dispatched.load(Ordering::SeqCst),
            events_delivered: c.events_delivered.load(Ordering::SeqCst),
            events_withheld: c.events_withheld.load(Ordering::SeqCst),
            slow_consumers: c.slow_consumers.load(Ordering::SeqCst),
            revoked: c.revoked.load(Ordering::SeqCst),
            rejected: c.rejected.load(Ordering::SeqCst),
        }
    }

    /// Reserves a connection slot for `key` (principal or client IP).
    pub(crate) fn admit(&self, key: String) -> Result<ConnSlot, RealtimeError> {
        let inner = &self.inner;
        let mut m = inner.per_principal.lock().unwrap_or_else(|p| p.into_inner());
        if inner.counters.connections.load(Ordering::SeqCst) >= inner.cfg.max_connections {
            inner.counters.rejected.fetch_add(1, Ordering::SeqCst);
            return Err(RealtimeError::TooManyConnections(
                "this instance has reached its connection limit".into(),
            ));
        }
        let n = m.entry(key.clone()).or_insert(0);
        if *n >= inner.cfg.max_connections_per_principal {
            inner.counters.rejected.fetch_add(1, Ordering::SeqCst);
            return Err(RealtimeError::TooManyConnections(format!(
                "at most {} realtime connections per user",
                inner.cfg.max_connections_per_principal
            )));
        }
        *n += 1;
        inner.counters.connections.fetch_add(1, Ordering::SeqCst);
        metrics::gauge!("forgebase_realtime_connections").increment(1.0);
        Ok(ConnSlot { svc: self.clone(), key })
    }

    /// Publishes a channel message as `ctx` (authorised with
    /// [`Action::ChannelPublish`]). Delivered to this instance's subscribers
    /// and, through the fan-out, to every other instance's.
    pub async fn publish(&self, ctx: &AccessContext, channel: &str, data: Json) -> Result<(), RealtimeError> {
        if !protocol::valid_channel(channel) {
            return Err(RealtimeError::invalid(
                "invalid_channel",
                "channel names are 1-128 characters of [A-Za-z0-9_.:-]",
            ));
        }
        self.inner
            .data
            .authorize_channel(ctx, Action::ChannelPublish, channel)
            .map_err(|e| RealtimeError::from_data(e, ctx))?;
        let size = data.to_string().len();
        if size > self.inner.cfg.max_message_bytes {
            return Err(RealtimeError::invalid("message_too_large", "the message is too large"));
        }
        self.emit_ephemeral(Ephemeral {
            origin: self.inner.instance.clone(),
            channel: channel.to_owned(),
            kind: EphemeralKind::Message {
                from: ctx.actor(),
                data,
                timestamp: forgebase_data::schema::now_timestamp(),
            },
        });
        Ok(())
    }

    /// Members of a channel, for callers allowed to subscribe to it.
    pub fn presence(&self, ctx: &AccessContext, channel: &str) -> Result<Vec<Member>, RealtimeError> {
        if !protocol::valid_channel(channel) {
            return Err(RealtimeError::invalid(
                "invalid_channel",
                "channel names are 1-128 characters of [A-Za-z0-9_.:-]",
            ));
        }
        self.inner
            .data
            .authorize_channel(ctx, Action::ChannelSubscribe, channel)
            .map_err(|e| RealtimeError::from_data(e, ctx))?;
        Ok(self.inner.rooms.members(channel))
    }

    /// Local delivery plus fan-out.
    pub(crate) fn emit_ephemeral(&self, e: Ephemeral) {
        self.inner.rooms.apply(&e);
        if let Some(f) = self.inner.fanout.clone() {
            match serde_json::to_string(&e) {
                Ok(payload) => {
                    tokio::spawn(async move {
                        if let Err(err) = f.publish(payload).await {
                            tracing::warn!(error = %err, "realtime fan-out publish failed");
                        }
                    });
                }
                Err(err) => tracing::error!(error = %err, "realtime message does not serialize"),
            }
        }
        let _ = self.inner.ephemeral.send(Arc::new(e));
    }
}

/// Tails the outbox and publishes each committed event once.
async fn dispatcher(weak: std::sync::Weak<Inner>, notify: Arc<Notify>) {
    loop {
        let Some(inner) = weak.upgrade() else { return };
        let poll = inner.cfg.poll_interval;
        let mut shutdown = inner.shutdown.subscribe();
        if *shutdown.borrow() {
            return;
        }
        let mut last = inner.head.load(Ordering::SeqCst);
        loop {
            match outbox::read_after(inner.data.backend().as_ref(), last, outbox::PAGE).await {
                Ok(events) => {
                    let n = events.len();
                    for e in events {
                        last = e.seq;
                        inner.head.store(last, Ordering::SeqCst);
                        inner.counters.events_dispatched.fetch_add(1, Ordering::SeqCst);
                        metrics::counter!("forgebase_realtime_events_dispatched_total").increment(1);
                        let _ = inner.bus.send(Arc::new(e));
                    }
                    if (n as i64) < outbox::PAGE {
                        break;
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "realtime dispatcher could not read the outbox");
                    break;
                }
            }
        }
        drop(inner);
        tokio::select! {
            () = notify.notified() => {}
            () = tokio::time::sleep(poll) => {}
            _ = shutdown.changed() => return,
        }
    }
}

/// Deletes outbox events beyond the retention window.
async fn pruner(weak: std::sync::Weak<Inner>) {
    loop {
        let interval = match weak.upgrade() {
            Some(inner) => inner.cfg.prune_interval,
            None => return,
        };
        tokio::time::sleep(interval).await;
        let Some(inner) = weak.upgrade() else { return };
        let cutoff = time_ago(inner.cfg.retention);
        match outbox::prune(inner.data.backend().as_ref(), &cutoff, inner.cfg.max_retained_events).await {
            Ok(0) => {}
            Ok(n) => tracing::debug!(deleted = n, "pruned realtime outbox"),
            Err(e) => tracing::warn!(error = %e, "realtime outbox pruning failed"),
        }
    }
}

/// UTC timestamp (outbox format) `d` before now.
fn time_ago(d: Duration) -> String {
    let then = time::OffsetDateTime::now_utc()
        .checked_sub(time::Duration::try_from(d).unwrap_or(time::Duration::MAX))
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    forgebase_data::schema::format_timestamp(then)
}

/// Receives other instances' channel messages and presence changes.
async fn fanout_receiver(weak: std::sync::Weak<Inner>, mut rx: tokio::sync::mpsc::Receiver<String>) {
    while let Some(payload) = rx.recv().await {
        let Some(inner) = weak.upgrade() else { return };
        match serde_json::from_str::<Ephemeral>(&payload) {
            Ok(e) if e.origin == inner.instance => {}
            Ok(e) => {
                inner.rooms.apply(&e);
                let _ = inner.ephemeral.send(Arc::new(e));
            }
            Err(err) => tracing::warn!(error = %err, "ignoring malformed fan-out message"),
        }
    }
}
