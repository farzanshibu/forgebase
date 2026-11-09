//! One realtime connection (WebSocket or SSE): subscriptions, replay, live
//! delivery with per-event authorisation, presence, heartbeats, credential
//! re-validation and backpressure.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use forgebase_auth::scopes::{Access, RequiredScope};
use forgebase_auth::{AuthError, AuthService, Authenticated, Scope};
use forgebase_data::db::Value;
use forgebase_data::query::Expr;
use forgebase_data::{AccessContext, Action};
use futures_util::{Stream, StreamExt};
use serde_json::{Value as Json, json};
use tokio::sync::{broadcast, mpsc};
use tokio::time::{MissedTickBehavior, interval_at, timeout};

use crate::outbox::{self, Event};
use crate::presence::{Ephemeral, EphemeralKind, Member};
use crate::protocol::{ClientMsg, SubscribeSpec, close, valid_channel, valid_subscription_id};
use crate::{RealtimeError, RealtimeService};

/// A credential kept to re-validate the connection.
#[derive(Debug, Clone)]
pub(crate) enum Credential {
    /// Access token, API key or PAT sent as a bearer token.
    Bearer(String),
    /// `X-API-Key`.
    ApiKey(String),
    /// Session cookie.
    Cookie(String),
}

/// Who the connection acts for.
#[derive(Debug, Clone)]
pub(crate) struct Identity {
    pub(crate) ctx: AccessContext,
    pub(crate) credential: Option<Credential>,
    pub(crate) scopes: Option<Vec<Scope>>,
}

/// Verifies a credential with the auth service.
pub(crate) async fn authenticate(auth: &AuthService, cred: &Credential) -> Result<Authenticated, AuthError> {
    match cred {
        Credential::Bearer(t) if AuthService::is_api_key(t) => auth.authenticate_key(t).await,
        Credential::Bearer(t) => auth.authenticate_access_token(t).await,
        Credential::ApiKey(k) => auth.authenticate_key(k).await,
        Credential::Cookie(c) => auth.authenticate_cookie(c).await,
    }
}

/// Whether `scopes` (if restricted) grant `area:access[:resource]`.
pub(crate) fn scopes_grant(scopes: Option<&[Scope]>, area: &str, access: Access, resource: Option<&str>) -> bool {
    let Some(scopes) = scopes else { return true };
    let need = RequiredScope {
        area: area.to_owned(),
        access,
        resource: resource.map(str::to_owned),
    };
    scopes.iter().any(|s| s.grants(&need))
}

/// Something read from the client.
#[derive(Debug)]
pub(crate) enum Inbound {
    /// A text message.
    Text(String),
    /// A ping or pong frame (liveness only).
    Activity,
    /// A binary frame (not supported).
    Binary,
    /// A frame above the size limit.
    TooBig,
    /// The client closed or the transport failed.
    Closed,
}

/// Something to send to the client.
#[derive(Debug)]
pub(crate) enum Outbound {
    /// A JSON message; `event` is its `type`, `id` the cursor of record events.
    Msg {
        event: String,
        id: Option<i64>,
        json: String,
    },
    /// Heartbeat (WebSocket ping frame, SSE comment).
    Ping,
}

impl Outbound {
    fn of(v: Json, id: Option<i64>) -> Self {
        let event = v.get("type").and_then(Json::as_str).unwrap_or("message").to_owned();
        Outbound::Msg {
            event,
            id,
            json: v.to_string(),
        }
    }
}

/// Why a connection ended.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CloseReason {
    /// The client went away (closed, disconnected).
    ClientGone,
    /// The server is shutting down.
    GoingAway,
    /// The client did not read fast enough.
    SlowConsumer,
    /// The credential is no longer valid.
    Unauthenticated(String),
    /// No sign of life within the idle timeout.
    Idle,
    /// A frame above the size limit.
    TooBig,
    /// A binary frame.
    Unsupported,
    /// Repeated protocol errors.
    Protocol,
}

impl CloseReason {
    pub(crate) fn code(&self) -> u16 {
        match self {
            CloseReason::ClientGone => close::NORMAL,
            CloseReason::GoingAway => close::GOING_AWAY,
            CloseReason::SlowConsumer => close::SLOW_CONSUMER,
            CloseReason::Unauthenticated(_) => close::UNAUTHENTICATED,
            CloseReason::Idle => close::HEARTBEAT_TIMEOUT,
            CloseReason::TooBig => close::MESSAGE_TOO_BIG,
            CloseReason::Unsupported => close::UNSUPPORTED_DATA,
            CloseReason::Protocol => close::PROTOCOL_ERROR,
        }
    }

    pub(crate) fn text(&self) -> String {
        match self {
            CloseReason::ClientGone => "closed".into(),
            CloseReason::GoingAway => "server shutting down".into(),
            CloseReason::SlowConsumer => "slow_consumer".into(),
            CloseReason::Unauthenticated(m) => format!("unauthenticated: {m}"),
            CloseReason::Idle => "heartbeat_timeout".into(),
            CloseReason::TooBig => "message_too_big".into(),
            CloseReason::Unsupported => "binary frames are not supported".into(),
            CloseReason::Protocol => "protocol_error".into(),
        }
    }
}

type Step = Result<(), CloseReason>;

#[derive(Debug)]
enum Target {
    Records {
        collection_id: String,
        record: Option<String>,
        filter: Option<(String, Expr, HashMap<String, Json>)>,
        action: Action,
    },
    Channel(String),
}

#[derive(Debug)]
pub(crate) struct Sub {
    id: String,
    target: Target,
    last_seq: i64,
}

/// Records a slow consumer once per connection.
pub(crate) fn mark_slow(svc: &RealtimeService, flag: &AtomicBool) {
    if !flag.swap(true, Ordering::SeqCst) {
        svc.inner.counters.slow_consumers.fetch_add(1, Ordering::SeqCst);
        metrics::counter!("forgebase_realtime_slow_consumers_total").increment(1);
    }
}

/// Queues `msg`, waiting at most the send timeout for room.
async fn emit(svc: &RealtimeService, out: &mpsc::Sender<Outbound>, slow: &AtomicBool, msg: Outbound) -> Step {
    match timeout(svc.inner.cfg.send_timeout, out.send(msg)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(CloseReason::ClientGone),
        Err(_) => {
            mark_slow(svc, slow);
            Err(CloseReason::SlowConsumer)
        }
    }
}

fn error_json(reference: &Option<Json>, sub: Option<&str>, e: &RealtimeError) -> Json {
    use forgebase_core::ErrorCode;
    let message = if e.kind().is_client_visible() {
        e.to_string()
    } else {
        tracing::error!(error = %e, "realtime request failed");
        "internal server error".to_owned()
    };
    let mut v = json!({"type": "error", "code": e.code(), "message": message});
    if let Some(r) = reference {
        v["ref"] = r.clone();
    }
    if let Some(s) = sub {
        v["id"] = json!(s);
    }
    v
}

/// A connection's state.
pub(crate) struct Conn {
    svc: RealtimeService,
    pub(crate) id: String,
    ident: Identity,
    subs: Vec<Sub>,
    out: mpsc::Sender<Outbound>,
    slow: Arc<AtomicBool>,
    sse: bool,
    joined: Vec<String>,
    tokens: f64,
    refill_at: Instant,
    strikes: u32,
}

/// Bus receivers, created before any replay so no event can fall between
/// the replay and live delivery.
pub(crate) struct Receivers {
    bus: broadcast::Receiver<Arc<Event>>,
    eph: broadcast::Receiver<Arc<Ephemeral>>,
}

impl Conn {
    pub(crate) fn new(
        svc: RealtimeService,
        ident: Identity,
        out: mpsc::Sender<Outbound>,
        slow: Arc<AtomicBool>,
        sse: bool,
    ) -> (Self, Receivers) {
        let rx = Receivers {
            bus: svc.inner.bus.subscribe(),
            eph: svc.inner.ephemeral.subscribe(),
        };
        let rate = f64::from(svc.inner.cfg.publish_per_second.max(1));
        (
            Self {
                svc,
                id: uuid::Uuid::new_v4().simple().to_string(),
                ident,
                subs: Vec::new(),
                out,
                slow,
                sse,
                joined: Vec::new(),
                tokens: rate,
                refill_at: Instant::now(),
                strikes: 0,
            },
            rx,
        )
    }

    async fn send(&self, v: Json) -> Step {
        emit(&self.svc, &self.out, &self.slow, Outbound::of(v, None)).await
    }

    /// The first message.
    pub(crate) async fn welcome(&self) -> Step {
        let cfg = &self.svc.inner.cfg;
        self.send(json!({
            "type": "welcome",
            "connection_id": self.id,
            "actor": self.ident.ctx.actor(),
            "cursor": self.svc.inner.head.load(Ordering::SeqCst),
            "heartbeat_ms": cfg.heartbeat_interval.as_millis() as u64,
            "limits": {
                "max_subscriptions": cfg.max_subscriptions_per_connection,
                "max_message_bytes": cfg.max_message_bytes,
                "publish_per_second": cfg.publish_per_second,
            },
        }))
        .await
    }

    /// Validates and authorises a subscription (does not activate it).
    pub(crate) async fn prepare(&self, spec: &SubscribeSpec) -> Result<Sub, RealtimeError> {
        let ctx = &self.ident.ctx;
        let cfg = &self.svc.inner.cfg;
        if !valid_subscription_id(&spec.id) {
            return Err(RealtimeError::invalid(
                "invalid_subscription_id",
                "subscription ids are 1-64 characters of [A-Za-z0-9_.:-]",
            ));
        }
        if self.subs.iter().any(|s| s.id == spec.id) {
            return Err(RealtimeError::invalid(
                "duplicate_subscription",
                format!("subscription {:?} already exists", spec.id),
            ));
        }
        if self.subs.len() >= cfg.max_subscriptions_per_connection {
            return Err(RealtimeError::invalid(
                "too_many_subscriptions",
                format!("at most {} subscriptions per connection", cfg.max_subscriptions_per_connection),
            ));
        }
        let target = match (&spec.collection, &spec.channel) {
            (Some(collection), None) => {
                if spec.record.is_some() && spec.filter.is_some() {
                    return Err(RealtimeError::invalid(
                        "invalid_subscription",
                        "a record subscription takes no filter",
                    ));
                }
                if let Some(r) = &spec.record
                    && !forgebase_data::schema::is_valid_id(r)
                {
                    return Err(RealtimeError::invalid("invalid_record_id", "invalid record id"));
                }
                if !scopes_grant(self.ident.scopes.as_deref(), "collections", Access::Read, Some(collection)) {
                    return Err(RealtimeError::Forbidden(format!(
                        "insufficient_scope: this subscription needs scope collections:read:{collection}"
                    )));
                }
                let action = if spec.record.is_some() { Action::View } else { Action::List };
                let filter = match spec.filter.as_deref().map(str::trim).filter(|f| !f.is_empty()) {
                    Some(text) => forgebase_data::query::parse_filter(text)
                        .map_err(|e| RealtimeError::from_data(e.into(), ctx))?
                        .map(|expr| (text.to_owned(), expr, spec.params.clone())),
                    None => None,
                };
                let coll = self
                    .svc
                    .inner
                    .data
                    .subscription_target(ctx, collection, action, filter.as_ref().map(|(_, e, p)| (e, p)))
                    .map_err(|e| RealtimeError::from_data(e, ctx))?;
                Target::Records {
                    collection_id: coll.id,
                    record: spec.record.clone(),
                    filter,
                    action,
                }
            }
            (None, Some(channel)) => {
                if spec.record.is_some() || spec.filter.is_some() {
                    return Err(RealtimeError::invalid(
                        "invalid_subscription",
                        "channel subscriptions take no record or filter",
                    ));
                }
                if !valid_channel(channel) {
                    return Err(RealtimeError::invalid(
                        "invalid_channel",
                        "channel names are 1-128 characters of [A-Za-z0-9_.:-]",
                    ));
                }
                self.svc
                    .inner
                    .data
                    .authorize_channel(ctx, Action::ChannelSubscribe, channel)
                    .map_err(|e| RealtimeError::from_data(e, ctx))?;
                Target::Channel(channel.clone())
            }
            _ => {
                return Err(RealtimeError::invalid(
                    "invalid_subscription",
                    "subscribe to exactly one of collection or channel",
                ));
            }
        };
        Ok(Sub {
            id: spec.id.clone(),
            target,
            last_seq: spec.cursor.unwrap_or(-1),
        })
    }

    /// Activates prepared subscriptions: record subscriptions with a cursor
    /// replay (merged, in cursor order) from the outbox, the others start at
    /// the head. Sends `subscribed` for each (after its replay).
    pub(crate) async fn activate(&mut self, subs: Vec<Sub>, reference: Option<Json>) -> Step {
        let (_, head) = match outbox::bounds(self.svc.inner.data.backend().as_ref()).await {
            Ok(b) => b,
            Err(e) => {
                for s in &subs {
                    self.send(error_json(&reference, Some(&s.id), &e)).await?;
                }
                return Ok(());
            }
        };
        let first = self.subs.len();
        let mut replay = Vec::new();
        for mut s in subs {
            let is_records = matches!(s.target, Target::Records { .. });
            if s.last_seq < 0 || !is_records {
                s.last_seq = head;
            } else {
                replay.push(self.subs.len());
            }
            self.subs.push(s);
            self.svc.inner.counters.subscriptions.fetch_add(1, Ordering::SeqCst);
        }
        let (counts, resets) = self.replay(&replay).await?;
        for i in first..self.subs.len() {
            let s = &self.subs[i];
            let mut v = json!({"type": "subscribed", "id": s.id, "cursor": s.last_seq});
            if let Some(r) = &reference {
                v["ref"] = r.clone();
            }
            if let Some(n) = counts.get(&i) {
                v["replayed"] = json!(n);
            }
            if let Some(reason) = resets.get(&i) {
                v["reset"] = json!(reason);
            }
            self.send(v).await?;
        }
        Ok(())
    }

    /// Replays outbox events after each listed subscription's `last_seq`, in
    /// one pass in cursor order. A subscription whose cursor is unusable gets
    /// a `reset` (and starts at the head). Returns per-subscription delivered
    /// counts and resets.
    async fn replay(&mut self, idx: &[usize]) -> Result<(HashMap<usize, u64>, HashMap<usize, &'static str>), CloseReason> {
        let mut counts = HashMap::new();
        let mut resets = HashMap::new();
        if idx.is_empty() {
            return Ok((counts, resets));
        }
        let backend = self.svc.inner.data.backend().clone();
        let (min, max) = match outbox::bounds(backend.as_ref()).await {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(error = %e, "realtime replay could not read the outbox");
                (0, self.svc.inner.head.load(Ordering::SeqCst))
            }
        };
        let mut from = i64::MAX;
        let mut active = Vec::new();
        for &i in idx {
            let s = &mut self.subs[i];
            let reason = if s.last_seq > max {
                Some("cursor_invalid")
            } else if s.last_seq < max && min > 0 && s.last_seq < min - 1 {
                Some("cursor_expired")
            } else if max - s.last_seq > self.svc.inner.cfg.max_replay {
                Some("too_far_behind")
            } else {
                None
            };
            if let Some(r) = reason {
                s.last_seq = max;
                resets.insert(i, r);
                let v = json!({"type": "reset", "id": s.id, "reason": r, "cursor": max});
                emit(&self.svc, &self.out, &self.slow, Outbound::of(v, None)).await?;
            } else {
                from = from.min(s.last_seq);
                active.push(i);
                counts.insert(i, 0);
            }
        }
        let mut cur = from;
        'pages: while cur < max {
            let page = match outbox::read_after(backend.as_ref(), cur, outbox::PAGE).await {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(error = %e, "realtime replay could not read the outbox");
                    break;
                }
            };
            if page.is_empty() {
                break;
            }
            for ev in page {
                if ev.seq > max {
                    break 'pages;
                }
                cur = ev.seq;
                for (i, delivered) in self.deliver(&ev, Some(&active)).await? {
                    if delivered {
                        *counts.entry(i).or_insert(0) += 1;
                    }
                }
            }
        }
        for &i in &active {
            if self.subs[i].last_seq < max {
                self.subs[i].last_seq = max;
            }
        }
        Ok((counts, resets))
    }

    /// Catches up every record subscription from the outbox (after the bus
    /// dropped events for this connection).
    async fn catch_up(&mut self) -> Step {
        let idx: Vec<usize> = (0..self.subs.len())
            .filter(|&i| matches!(self.subs[i].target, Target::Records { .. }))
            .collect();
        self.replay(&idx).await.map(|_| ())
    }

    /// Sends `ev` to every matching subscription (or only those in `only`)
    /// whose cursor it is past, if the subscriber may see it *now*. Returns
    /// `(subscription index, delivered)` for each matching subscription.
    async fn deliver(&mut self, ev: &Event, only: Option<&[usize]>) -> Result<Vec<(usize, bool)>, CloseReason> {
        let Conn {
            svc,
            ident,
            subs,
            out,
            slow,
            ..
        } = self;
        let ctx = &ident.ctx;
        let tenant = match ev.snapshot.get("_tenant") {
            Some(Value::Text(t)) => Some(t.clone()),
            _ => ev.tenant.clone(),
        };
        let mut cache: Vec<(Action, Option<String>, Option<Json>)> = Vec::new();
        let mut result = Vec::new();
        for (i, s) in subs.iter_mut().enumerate() {
            if only.is_some_and(|o| !o.contains(&i)) {
                continue;
            }
            let Target::Records {
                collection_id,
                record,
                filter,
                action,
            } = &s.target
            else {
                continue;
            };
            if *collection_id != ev.collection_id
                || record.as_ref().is_some_and(|r| *r != ev.record_id)
                || ev.seq <= s.last_seq
            {
                continue;
            }
            s.last_seq = ev.seq;
            // Defence in depth: never cross tenants for non-privileged callers.
            let same_tenant = ctx.is_privileged() || tenant.is_none() || tenant == ctx.tenant;
            let key = filter.as_ref().map(|(t, _, _)| t.clone());
            let data = if !same_tenant {
                None
            } else if let Some((_, _, d)) = cache.iter().find(|(a, k, _)| a == action && *k == key) {
                d.clone()
            } else {
                let d = match svc
                    .inner
                    .data
                    .view_snapshot(
                        ctx,
                        *action,
                        collection_id,
                        &ev.snapshot,
                        filter.as_ref().map(|(_, e, p)| (e, p)),
                    )
                    .await
                {
                    Ok(Some(rec)) => Some(Json::Object(rec)),
                    Ok(None) => None,
                    Err(e) => {
                        tracing::warn!(error = %e, seq = ev.seq, "realtime event withheld: access check failed");
                        None
                    }
                };
                cache.push((*action, key, d.clone()));
                d
            };
            let Some(data) = data else {
                svc.inner.counters.events_withheld.fetch_add(1, Ordering::SeqCst);
                result.push((i, false));
                continue;
            };
            let v = json!({
                "type": ev.kind(),
                "id": s.id,
                "cursor": ev.seq,
                "collection": ev.collection,
                "record_id": ev.record_id,
                "tenant_id": tenant,
                "timestamp": ev.created_at,
                "data": data,
            });
            emit(svc, out, slow, Outbound::of(v, Some(ev.seq))).await?;
            svc.inner.counters.events_delivered.fetch_add(1, Ordering::SeqCst);
            metrics::counter!("forgebase_realtime_events_delivered_total").increment(1);
            result.push((i, true));
        }
        Ok(result)
    }

    async fn on_ephemeral(&mut self, e: &Ephemeral) -> Step {
        let Conn {
            svc,
            ident,
            subs,
            out,
            slow,
            ..
        } = self;
        for s in subs.iter() {
            let Target::Channel(ch) = &s.target else { continue };
            if *ch != e.channel {
                continue;
            }
            // Re-check: channel rules can change while subscribed.
            if svc
                .inner
                .data
                .authorize_channel(&ident.ctx, Action::ChannelSubscribe, ch)
                .is_err()
            {
                svc.inner.counters.events_withheld.fetch_add(1, Ordering::SeqCst);
                continue;
            }
            let v = match &e.kind {
                EphemeralKind::Message { from, data, timestamp } => json!({
                    "type": "message", "id": s.id, "channel": ch, "from": from, "data": data, "timestamp": timestamp,
                }),
                EphemeralKind::Join { member } => json!({
                    "type": "presence.join", "id": s.id, "channel": ch, "member": member,
                }),
                EphemeralKind::Leave { member } => json!({
                    "type": "presence.leave", "id": s.id, "channel": ch, "member": member,
                }),
            };
            emit(svc, out, slow, Outbound::of(v, None)).await?;
        }
        Ok(())
    }

    async fn reply(&self, reference: &Option<Json>, mut v: Json) -> Step {
        if let Some(r) = reference {
            v["ref"] = r.clone();
        }
        self.send(v).await
    }

    async fn fail(&self, reference: &Option<Json>, sub: Option<&str>, e: &RealtimeError) -> Step {
        self.send(error_json(reference, sub, e)).await
    }

    fn take_token(&mut self) -> bool {
        let rate = f64::from(self.svc.inner.cfg.publish_per_second.max(1));
        let now = Instant::now();
        self.tokens = (self.tokens + now.duration_since(self.refill_at).as_secs_f64() * rate).min(rate);
        self.refill_at = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    async fn handle_text(&mut self, text: &str) -> Step {
        let msg: ClientMsg = match serde_json::from_str(text) {
            Ok(m) => m,
            Err(e) => {
                self.strikes += 1;
                if self.strikes > 10 {
                    return Err(CloseReason::Protocol);
                }
                let reference = serde_json::from_str::<Json>(text).ok().and_then(|v| v.get("ref").cloned());
                return self
                    .fail(
                        &reference,
                        None,
                        &RealtimeError::invalid("invalid_message", format!("invalid message: {e}")),
                    )
                    .await;
            }
        };
        match msg {
            ClientMsg::Subscribe { spec, reference } => match self.prepare(&spec).await {
                Ok(sub) => self.activate(vec![sub], reference).await,
                Err(e) => self.fail(&reference, Some(&spec.id), &e).await,
            },
            ClientMsg::Unsubscribe { id, reference } => {
                let Some(pos) = self.subs.iter().position(|s| s.id == id) else {
                    let e = RealtimeError::invalid("not_subscribed", format!("no subscription {id:?}"));
                    return self.fail(&reference, Some(&id), &e).await;
                };
                let sub = self.subs.remove(pos);
                self.svc.inner.counters.subscriptions.fetch_sub(1, Ordering::SeqCst);
                if let Target::Channel(ch) = &sub.target
                    && !self.subs.iter().any(|s| matches!(&s.target, Target::Channel(c) if c == ch))
                {
                    self.leave(ch);
                }
                self.reply(&reference, json!({"type": "unsubscribed", "id": id})).await
            }
            ClientMsg::Publish {
                channel,
                data,
                reference,
            } => {
                if !scopes_grant(self.ident.scopes.as_deref(), "realtime", Access::Write, None) {
                    let e = RealtimeError::Forbidden("insufficient_scope: publishing needs scope realtime:write".into());
                    return self.fail(&reference, None, &e).await;
                }
                if !self.take_token() {
                    let e = RealtimeError::RateLimited("too many messages; slow down".into());
                    return self.fail(&reference, None, &e).await;
                }
                match self.svc.publish(&self.ident.ctx, &channel, data).await {
                    Ok(()) => self.reply(&reference, json!({"type": "ack"})).await,
                    Err(e) => self.fail(&reference, None, &e).await,
                }
            }
            ClientMsg::PresenceJoin {
                channel,
                meta,
                reference,
            } => {
                let r = self.join(&channel, meta);
                match r {
                    Ok(members) => {
                        self.reply(
                            &reference,
                            json!({"type": "presence.state", "channel": channel, "members": members}),
                        )
                        .await
                    }
                    Err(e) => self.fail(&reference, None, &e).await,
                }
            }
            ClientMsg::PresenceLeave { channel, reference } => {
                if self.leave(&channel) {
                    self.reply(&reference, json!({"type": "presence.left", "channel": channel}))
                        .await
                } else {
                    let e = RealtimeError::invalid("not_joined", "this connection has not joined the channel");
                    self.fail(&reference, None, &e).await
                }
            }
            ClientMsg::Ping { reference } => self.reply(&reference, json!({"type": "pong"})).await,
            ClientMsg::Auth { token, reference } => match self.reauth(token).await {
                Ok(()) => {
                    self.reply(&reference, json!({"type": "authenticated", "actor": self.ident.ctx.actor()}))
                        .await
                }
                Err(e) => self.fail(&reference, None, &e).await,
            },
        }
    }

    fn join(&mut self, channel: &str, meta: Json) -> Result<Vec<Member>, RealtimeError> {
        let ctx = &self.ident.ctx;
        if !self
            .subs
            .iter()
            .any(|s| matches!(&s.target, Target::Channel(c) if c == channel))
        {
            return Err(RealtimeError::invalid(
                "not_subscribed",
                "subscribe to the channel before joining its presence",
            ));
        }
        self.svc
            .inner
            .data
            .authorize_channel(ctx, Action::ChannelPublish, channel)
            .map_err(|e| RealtimeError::from_data(e, ctx))?;
        if !scopes_grant(self.ident.scopes.as_deref(), "realtime", Access::Write, None) {
            return Err(RealtimeError::Forbidden(
                "insufficient_scope: presence needs scope realtime:write".into(),
            ));
        }
        if meta.to_string().len() > self.svc.inner.cfg.max_presence_meta_bytes {
            return Err(RealtimeError::invalid("meta_too_large", "presence meta is too large"));
        }
        let member = Member {
            connection_id: self.id.clone(),
            actor: ctx.actor(),
            meta,
            joined_at: forgebase_data::schema::now_timestamp(),
            instance: self.svc.inner.instance.clone(),
        };
        if !self.joined.iter().any(|c| c == channel) {
            self.joined.push(channel.to_owned());
        }
        self.svc.emit_ephemeral(Ephemeral {
            origin: self.svc.inner.instance.clone(),
            channel: channel.to_owned(),
            kind: EphemeralKind::Join { member },
        });
        Ok(self.svc.inner.rooms.members(channel))
    }

    fn leave(&mut self, channel: &str) -> bool {
        let Some(pos) = self.joined.iter().position(|c| c == channel) else {
            return false;
        };
        self.joined.remove(pos);
        if let Some(member) = self.svc.inner.rooms.leave(channel, &self.svc.inner.instance, &self.id) {
            self.svc.emit_ephemeral(Ephemeral {
                origin: self.svc.inner.instance.clone(),
                channel: channel.to_owned(),
                kind: EphemeralKind::Leave { member },
            });
        }
        true
    }

    async fn reauth(&mut self, token: String) -> Result<(), RealtimeError> {
        let Some(auth) = self.svc.inner.auth.clone() else {
            return Err(RealtimeError::Unavailable("authentication is not configured".into()));
        };
        let cred = Credential::Bearer(token);
        let a = authenticate(&auth, &cred)
            .await
            .map_err(|e| RealtimeError::Unauthenticated(e.to_string()))?;
        if a.subject.access_context().principal != self.ident.ctx.principal {
            return Err(RealtimeError::Forbidden(
                "principal_mismatch: open a new connection to change identity".into(),
            ));
        }
        self.ident.credential = Some(cred);
        self.ident.scopes = a.scopes;
        Ok(())
    }

    /// Re-validates the credential: revoked or expired → close.
    async fn revalidate(&mut self) -> Step {
        let (Some(cred), Some(auth)) = (&self.ident.credential, &self.svc.inner.auth) else {
            return Ok(());
        };
        match authenticate(auth, cred).await {
            Ok(a) if a.subject.access_context().principal == self.ident.ctx.principal => Ok(()),
            Ok(_) => self.revoked("identity changed".into()),
            Err(e @ (AuthError::InvalidToken(_) | AuthError::Unauthenticated | AuthError::InvalidCredentials)) => {
                self.revoked(e.to_string())
            }
            Err(e) => {
                tracing::warn!(error = %e, "realtime credential check failed; will retry");
                Ok(())
            }
        }
    }

    fn revoked(&self, why: String) -> Step {
        self.svc.inner.counters.revoked.fetch_add(1, Ordering::SeqCst);
        metrics::counter!("forgebase_realtime_revoked_total").increment(1);
        Err(CloseReason::Unauthenticated(why))
    }

    /// The connection loop. Returns why it ended; cleans up presence and
    /// counters.
    pub(crate) async fn run<S>(&mut self, mut inbound: S, rx: Receivers) -> CloseReason
    where
        S: Stream<Item = Inbound> + Unpin,
    {
        let Receivers { mut bus, mut eph } = rx;
        let cfg = self.svc.inner.cfg.clone();
        let mut shutdown = self.svc.inner.shutdown.subscribe();
        let start = tokio::time::Instant::now();
        let mut heartbeat = interval_at(start + cfg.heartbeat_interval, cfg.heartbeat_interval);
        heartbeat.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut recheck = interval_at(start + cfg.revalidate_interval, cfg.revalidate_interval);
        recheck.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let out = self.out.clone();
        let mut last_seen = Instant::now();
        let reason = loop {
            if *shutdown.borrow() {
                break CloseReason::GoingAway;
            }
            let step: Step = tokio::select! {
                biased;
                _ = shutdown.changed() => Err(CloseReason::GoingAway),
                () = out.closed() => Err(CloseReason::ClientGone),
                m = inbound.next() => match m {
                    None | Some(Inbound::Closed) => Err(CloseReason::ClientGone),
                    Some(Inbound::Text(t)) => {
                        last_seen = Instant::now();
                        self.handle_text(&t).await
                    }
                    Some(Inbound::Activity) => {
                        last_seen = Instant::now();
                        Ok(())
                    }
                    Some(Inbound::Binary) => Err(CloseReason::Unsupported),
                    Some(Inbound::TooBig) => Err(CloseReason::TooBig),
                },
                r = bus.recv() => match r {
                    Ok(ev) => self.deliver(&ev, None).await.map(|_| ()),
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::debug!(skipped = n, conn = %self.id, "realtime connection lagged; catching up from the outbox");
                        self.catch_up().await
                    }
                    Err(broadcast::error::RecvError::Closed) => Err(CloseReason::GoingAway),
                },
                r = eph.recv() => match r {
                    Ok(e) => self.on_ephemeral(&e).await,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(skipped = n, conn = %self.id, "realtime connection dropped channel messages");
                        self.send(json!({"type": "error", "code": "messages_dropped", "message": format!("{n} channel messages were dropped")})).await
                    }
                    Err(broadcast::error::RecvError::Closed) => Err(CloseReason::GoingAway),
                },
                _ = heartbeat.tick() => {
                    if !self.sse && last_seen.elapsed() > cfg.idle_timeout {
                        Err(CloseReason::Idle)
                    } else {
                        emit(&self.svc, &self.out, &self.slow, Outbound::Ping).await
                    }
                }
                _ = recheck.tick() => self.revalidate().await,
            };
            if let Err(r) = step {
                break r;
            }
        };
        for ch in std::mem::take(&mut self.joined) {
            if let Some(member) = self.svc.inner.rooms.leave(&ch, &self.svc.inner.instance, &self.id) {
                self.svc.emit_ephemeral(Ephemeral {
                    origin: self.svc.inner.instance.clone(),
                    channel: ch,
                    kind: EphemeralKind::Leave { member },
                });
            }
        }
        self.svc
            .inner
            .counters
            .subscriptions
            .fetch_sub(self.subs.len(), Ordering::SeqCst);
        self.subs.clear();
        reason
    }
}
