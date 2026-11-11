//! Metrics, maintenance (expired leases, retention, pruning), metering of
//! transactional publishes, the realtime-outbox relay and the background task.

use std::sync::{Arc, Weak};
use std::time::Duration;

use forgebase_data::{AccessContext, Action, ChangeOp, ResourceKind, ResourceOp};
use forgebase_db::Statement;
use forgebase_entitlements::{EntitlementError, UsageRequest};
use serde_json::{Value as Json, json};
use tokio::sync::watch;

use crate::access::{bare, source_row};
use crate::error::QueueError;
use crate::model::{DeadLetter, DeadLetterPage, MaintenanceReport, NewMessage, SubscriptionMetrics, TopicMetrics, iso};
use crate::row::{Row, scalar};
use crate::service::{
    Db, Inner, QueueService, Result, SUB_COLS, SubRow, TOPIC_COLS, TopicRow, finish, insert_message_tx, minute,
    parse_history, prepare, sub_row,
};

/// Outbox events relayed per transaction.
const RELAY_PAGE: i64 = 200;
/// Key of the relay cursor in `_forgebase_queue_state`.
const RELAY_CURSOR: &str = "outbox_cursor";

/// Server-wide queue backlog ([`QueueService::backlog`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueBacklog {
    /// Deliveries ready now.
    pub ready: i64,
    /// Deliveries waiting for a retry or delay to pass.
    pub delayed: i64,
    /// Deliveries leased to a consumer.
    pub in_flight: i64,
    /// Messages retained across all topics.
    pub messages: i64,
}

impl QueueService {
    // ------------------------------------------------------------ metrics

    /// `(count in the last minute, rate/s over 5 min)`. Counters are per-minute
    /// buckets, so "last minute" counts every bucket overlapping the last 60 s
    /// (stable across a bucket boundary).
    async fn rate(&self, scope: &str, name: &str, metric: &str, now: i64) -> Result<(i64, f64)> {
        let rows = self
            .backend()
            .fetch_all(&Statement::new(
                "SELECT coalesce(sum(CASE WHEN minute > ?4 - 120000 THEN count ELSE 0 END), 0) AS last, \
                        coalesce(sum(count), 0) AS window FROM _forgebase_queue_rates \
                 WHERE scope = ?1 AND name = ?2 AND metric = ?3 AND minute > ?4 - 300000",
                vec![scope.into(), name.into(), metric.into(), minute(now).into()],
            ))
            .await?;
        let r = Row::new(&rows, 0);
        #[allow(clippy::cast_precision_loss)]
        let per_sec = r.int("window") as f64 / 300.0;
        Ok((r.int("last"), per_sec))
    }

    /// Server-wide backlog for `/metrics`: deliveries by state and messages
    /// retained. Two cheap aggregate queries, no per-topic or per-subscription
    /// labels. No authorisation: counts only, for the process itself.
    pub async fn backlog(&self) -> Result<QueueBacklog> {
        let now = self.now();
        let rows = self
            .backend()
            .fetch_all(&Statement::new(
                "SELECT \
                   coalesce(sum(CASE WHEN state = 'ready' AND available_at <= ?1 THEN 1 ELSE 0 END), 0) AS ready, \
                   coalesce(sum(CASE WHEN state = 'ready' AND available_at > ?1 THEN 1 ELSE 0 END), 0) AS delayed, \
                   coalesce(sum(CASE WHEN state = 'leased' THEN 1 ELSE 0 END), 0) AS in_flight, \
                   (SELECT COUNT(*) FROM _forgebase_queue_messages) AS messages \
                 FROM _forgebase_queue_deliveries",
                vec![now.into()],
            ))
            .await?;
        let r = Row::new(&rows, 0);
        Ok(QueueBacklog {
            ready: r.int("ready"),
            delayed: r.int("delayed"),
            in_flight: r.int("in_flight"),
            messages: r.int("messages"),
        })
    }

    /// Metrics of a topic.
    pub async fn topic_metrics(&self, ctx: &AccessContext, topic: &str) -> Result<TopicMetrics> {
        self.check(
            ctx,
            Action::QueueRead,
            ResourceKind::Topic,
            topic,
            ResourceOp::View,
            None,
        )?;
        let row = self.topic_for(ctx, topic).await?;
        let stored = row.name.clone();
        let t = row.public();
        let now = self.now();
        // Subscriptions of the caller's scope only: other tenants' are not counted.
        let rows = self
            .backend()
            .fetch_all(&Statement::new(
                "SELECT COUNT(*) AS n, coalesce(sum(bytes), 0) AS b, min(published_at) AS oldest, \
                 (SELECT COUNT(*) FROM _forgebase_queue_subscriptions WHERE topic = ?1 \
                    AND (tenant IS NULL OR tenant = ?2)) AS subs \
                 FROM _forgebase_queue_messages WHERE topic = ?1",
                vec![stored.as_str().into(), ctx.tenant.clone().into()],
            ))
            .await?;
        let r = Row::new(&rows, 0);
        let (last, per_sec) = self.rate("topic", &stored, "published", now).await?;
        Ok(TopicMetrics {
            topic: t.name,
            messages: r.int("n"),
            bytes: r.int("b"),
            oldest_message_age_ms: r.opt_int("oldest").map(|o| (now - o).max(0)),
            published_total: t.published_total,
            published_bytes: t.published_bytes,
            published_last_minute: last,
            publish_rate_per_sec: per_sec,
            subscriptions: r.int("subs"),
        })
    }

    /// Metrics of a subscription. Messages not yet read from the topic count
    /// into the backlog without the filter (the background task reads them
    /// every poll interval, applying it).
    pub async fn subscription_metrics(&self, ctx: &AccessContext, sub: &str) -> Result<SubscriptionMetrics> {
        let s = self
            .sub_for(
                &mut Db::Ro(self.backend()),
                ctx,
                Action::QueueRead,
                ResourceOp::View,
                sub,
            )
            .await?;
        let sub = s.name.as_str();
        let now = self.now();
        let rows = self
            .backend()
            .fetch_all(&Statement::new(
                "SELECT \
                   coalesce(sum(CASE WHEN d.state = 'ready' AND d.available_at <= ?2 THEN 1 ELSE 0 END), 0) AS ready, \
                   coalesce(sum(CASE WHEN d.state = 'ready' AND d.available_at > ?2 THEN 1 ELSE 0 END), 0) AS delayed, \
                   coalesce(sum(CASE WHEN d.state = 'leased' THEN 1 ELSE 0 END), 0) AS in_flight, \
                   COUNT(*) AS n, coalesce(sum(m.bytes), 0) AS b, min(m.published_at) AS oldest \
                 FROM _forgebase_queue_deliveries d JOIN _forgebase_queue_messages m ON m.seq = d.seq \
                 WHERE d.subscription = ?1",
                vec![sub.into(), now.into()],
            ))
            .await?;
        let d = Row::new(&rows, 0);
        let rows = self
            .backend()
            .fetch_all(&Statement::new(
                "SELECT COUNT(*) AS n, coalesce(sum(bytes), 0) AS b, min(published_at) AS oldest, \
                 (SELECT COUNT(*) FROM _forgebase_queue_dead_letters WHERE subscription = ?3) AS dl \
                 FROM _forgebase_queue_messages WHERE topic = ?1 AND seq > ?2",
                vec![s.topic.as_str().into(), s.cursor_seq.into(), sub.into()],
            ))
            .await?;
        let u = Row::new(&rows, 0);
        let oldest = match (d.opt_int("oldest"), u.opt_int("oldest")) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let (last, per_sec) = self.rate("subscription", sub, "acked", now).await?;
        Ok(SubscriptionMetrics {
            subscription: bare(&s.name).to_owned(),
            topic: bare(&s.topic).to_owned(),
            backlog_messages: d.int("n") + u.int("n"),
            backlog_bytes: d.int("b") + u.int("b"),
            oldest_unacked_age_ms: oldest.map(|o| (now - o).max(0)),
            ready: d.int("ready") + u.int("n"),
            delayed: d.int("delayed"),
            in_flight: d.int("in_flight"),
            delivered_total: s.delivered_total,
            acked_total: s.acked_total,
            nacked_total: s.nacked_total,
            expired_total: s.expired_total,
            dead_lettered_total: s.dead_lettered_total,
            dead_letters: u.int("dl"),
            acked_last_minute: last,
            ack_rate_per_sec: per_sec,
        })
    }

    /// Dead-letter records of a subscription, newest first (ids below `before`).
    pub async fn dead_letters(
        &self,
        ctx: &AccessContext,
        sub: &str,
        before: Option<i64>,
        limit: i64,
    ) -> Result<DeadLetterPage> {
        let s = self
            .sub_for(
                &mut Db::Ro(self.backend()),
                ctx,
                Action::QueueRead,
                ResourceOp::List,
                sub,
            )
            .await?;
        let sub = s.name.as_str();
        let rows = self
            .backend()
            .fetch_all(&Statement::new(
                "SELECT id, subscription, topic, message_id, data, attributes, ordering_key, published_at, attempts, \
                 history, reason, dead_letter_topic, forwarded_message_id, at FROM _forgebase_queue_dead_letters \
                 WHERE subscription = ?1 AND id < ?2 ORDER BY id DESC LIMIT ?3",
                vec![
                    sub.into(),
                    before.unwrap_or(i64::MAX).into(),
                    limit.clamp(1, 500).into(),
                ],
            ))
            .await?;
        let items = Row::all(&rows)
            .map(|r| {
                Ok(DeadLetter {
                    id: r.int("id"),
                    subscription: bare(&r.text("subscription")).to_owned(),
                    topic: bare(&r.text("topic")).to_owned(),
                    message_id: r.text("message_id"),
                    data: r.json("data")?,
                    attributes: r.map("attributes")?,
                    ordering_key: r.opt_text("ordering_key"),
                    published_at: iso(r.int("published_at")),
                    attempts: r.int("attempts"),
                    history: parse_history(&r.text("history"))?,
                    reason: r.text("reason"),
                    dead_letter_topic: r.opt_text("dead_letter_topic").map(|d| bare(&d).to_owned()),
                    forwarded_message_id: r.opt_text("forwarded_message_id"),
                    at: iso(r.int("at")),
                })
            })
            .collect::<Result<_>>()?;
        Ok(DeadLetterPage { items })
    }

    // -------------------------------------------------------- maintenance

    /// One maintenance round: ends expired leases and reads new messages into
    /// every subscription, applies topic retention, prunes dedup entries,
    /// rate buckets, old dead letters and expired snapshots, relays record
    /// changes from the realtime outbox and meters transactional publishes.
    /// The background task runs it every poll interval; tests call it directly.
    pub async fn run_maintenance(&self) -> Result<MaintenanceReport> {
        let mut report = MaintenanceReport {
            relayed: self.relay_outbox().await?,
            ..MaintenanceReport::default()
        };
        let now = self.now();
        let subs = self.all_subs().await?;
        for s in &subs {
            let mut tx = self.begin().await?;
            let r: Result<_> = async {
                let cur = sub_row(&mut Db::Tx(&mut *tx), &s.name).await?;
                let n = self.reap_tx(&mut *tx, &cur, now).await?;
                let cur = sub_row(&mut Db::Tx(&mut *tx), &s.name).await?;
                self.materialize_tx(&mut *tx, &cur, now).await?;
                Ok(n)
            }
            .await;
            match finish(tx, r).await {
                Ok(n) => report.expired += n,
                // Deleted meanwhile.
                Err(QueueError::SubscriptionNotFound(_)) => {}
                Err(e) => return Err(e),
            }
        }
        report.pruned = self.enforce_retention(now).await?;
        report.metered = self.meter_pending().await?;
        if report.expired > 0 || report.relayed > 0 {
            self.notify();
        }
        Ok(report)
    }

    async fn all_subs(&self) -> Result<Vec<SubRow>> {
        let rows = self
            .backend()
            .fetch_all(&Statement::raw(format!(
                "SELECT {SUB_COLS} FROM _forgebase_queue_subscriptions ORDER BY name"
            )))
            .await?;
        Row::all(&rows).map(|r| SubRow::from_row(&r)).collect()
    }

    async fn all_topics(&self) -> Result<Vec<TopicRow>> {
        let rows = self
            .backend()
            .fetch_all(&Statement::raw(format!(
                "SELECT {TOPIC_COLS} FROM _forgebase_queue_topics ORDER BY name"
            )))
            .await?;
        Row::all(&rows).map(|r| TopicRow::from_row(&r)).collect()
    }

    /// Drops messages beyond each topic's retention (time, then size) with
    /// their deliveries, and prunes expired bookkeeping. Returns the number of
    /// messages dropped.
    pub(crate) async fn enforce_retention(&self, now: i64) -> Result<u64> {
        let topics = self.all_topics().await?;
        let dl_cutoff = now - self.inner.settings.dead_letter_retention_ms;
        let mut tx = self.begin().await?;
        let r: Result<_> = async {
            let mut pruned = 0;
            for t in &topics {
                pruned += tx
                    .execute(&Statement::new(
                        "DELETE FROM _forgebase_queue_messages WHERE topic = ?1 AND published_at < ?2",
                        vec![t.name.as_str().into(), (now - t.retention_ms).into()],
                    ))
                    .await?
                    .rows_affected;
                if let Some(limit) = t.retention_bytes {
                    let cut = scalar(
                        &tx.fetch_all(&Statement::new(
                            "SELECT coalesce(max(seq), 0) FROM (SELECT seq, sum(bytes) OVER (ORDER BY seq DESC) AS run \
                             FROM _forgebase_queue_messages WHERE topic = ?1) WHERE run > ?2",
                            vec![t.name.as_str().into(), limit.into()],
                        ))
                        .await?,
                    );
                    if cut > 0 {
                        pruned += tx
                            .execute(&Statement::new(
                                "DELETE FROM _forgebase_queue_messages WHERE topic = ?1 AND seq <= ?2",
                                vec![t.name.as_str().into(), cut.into()],
                            ))
                            .await?
                            .rows_affected;
                    }
                }
            }
            for sql in [
                "DELETE FROM _forgebase_queue_dedup WHERE expires_at <= ?1",
                "DELETE FROM _forgebase_queue_snapshots WHERE expires_at <= ?1",
                "DELETE FROM _forgebase_queue_rates WHERE minute < ?1 - 3600000",
            ] {
                tx.execute(&Statement::new(sql, vec![now.into()])).await?;
            }
            tx.execute(&Statement::new(
                "DELETE FROM _forgebase_queue_dead_letters WHERE at < ?1",
                vec![dl_cutoff.into()],
            ))
            .await?;
            Ok(pruned)
        }
        .await;
        let pruned = finish(tx, r).await?;
        if pruned > 0 {
            metrics::counter!("forgebase_queue_retention_dropped_total").increment(pruned);
        }
        Ok(pruned)
    }

    /// Records the usage of messages published inside a caller's transaction
    /// (`metered = 0`), once each. Returns the number of messages handled.
    pub async fn meter_pending(&self) -> Result<usize> {
        let rows = self
            .backend()
            .fetch_all(&Statement::raw(
                "SELECT seq, id, subject, bytes FROM _forgebase_queue_messages WHERE metered = 0 ORDER BY seq LIMIT 200",
            ))
            .await?;
        let ent = self.inner.ent.get();
        let s = &self.inner.settings;
        for r in Row::all(&rows) {
            let (seq, id) = (r.int("seq"), r.text("id"));
            let mut state = 1;
            if let (Some(ent), Some(subject)) = (ent, r.opt_text("subject")) {
                for (feature, units, tag) in [
                    (s.messages_feature.as_deref(), 1, "m"),
                    (s.bytes_feature.as_deref(), r.int("bytes").max(1), "b"),
                ] {
                    let Some(feature) = feature else { continue };
                    let req = UsageRequest::new(feature, units, format!("queue:{id}:{tag}")).for_subject(&subject);
                    match ent.consume(&AccessContext::system(), req).await {
                        Ok(_) | Err(EntitlementError::NotFound(_)) => {}
                        Err(e) => {
                            // The transaction committed: the message stays; the refusal is recorded.
                            tracing::warn!(error = %e, message = %id, "usage of a transactional publish not recorded");
                            state = 2;
                        }
                    }
                }
            }
            let mut tx = self.begin().await?;
            let res: Result<_> = async {
                tx.execute(&Statement::new(
                    "UPDATE _forgebase_queue_messages SET metered = ?2 WHERE seq = ?1",
                    vec![seq.into(), state.into()],
                ))
                .await?;
                Ok(())
            }
            .await;
            finish(tx, res).await?;
        }
        Ok(rows.len())
    }

    // -------------------------------------------------------------- relay

    /// Publishes committed record changes from the realtime outbox (FB-40) to
    /// the topics whose `source` names their collection. The relay cursor
    /// advances in the same transaction as the inserts, so each change is
    /// published once. Returns the number of messages published.
    pub async fn relay_outbox(&self) -> Result<usize> {
        let backend = self.backend();
        let cursor = {
            let rows = backend
                .fetch_all(&Statement::new(
                    "SELECT value FROM _forgebase_queue_state WHERE key = ?1",
                    vec![RELAY_CURSOR.into()],
                ))
                .await?;
            match Row::all(&rows).next() {
                Some(r) => r.int("value"),
                None => {
                    // First run: from the oldest source topic's start, else the head.
                    let start = scalar(
                        &backend
                            .fetch_all(&Statement::raw(
                                "SELECT min(source_after_seq) FROM _forgebase_queue_topics \
                                 WHERE source_collection IS NOT NULL",
                            ))
                            .await?,
                    );
                    let start = if start > 0 {
                        start
                    } else {
                        forgebase_realtime::outbox::bounds(backend)
                            .await
                            .map_err(|e| QueueError::Internal(e.to_string()))?
                            .1
                    };
                    self.set_relay_cursor(start).await?;
                    start
                }
            }
        };
        let topics: Vec<TopicRow> = self
            .all_topics()
            .await?
            .into_iter()
            .filter(|t| t.source.is_some())
            .collect();
        let events = forgebase_realtime::outbox::read_after(backend, cursor, RELAY_PAGE)
            .await
            .map_err(|e| QueueError::Internal(e.to_string()))?;
        let Some(last) = events.last().map(|e| e.seq) else {
            return Ok(0);
        };
        if topics.is_empty() {
            self.set_relay_cursor(last).await?;
            return Ok(0);
        }
        let now = self.now();
        let mut tx = self.begin().await?;
        let r: Result<_> = async {
            let mut n = 0;
            for ev in &events {
                let op = match ev.op {
                    ChangeOp::Create => "create",
                    ChangeOp::Update => "update",
                    ChangeOp::Delete => "delete",
                };
                for t in &topics {
                    let Some(src) = &t.source else { continue };
                    // A tenant topic carries its own tenant's changes only.
                    let hit = (src.collection == ev.collection || src.collection == ev.collection_id)
                        && (src.ops.is_empty() || src.ops.iter().any(|o| o == op))
                        && ev.seq > t.source_after_seq
                        && (t.tenant.is_none() || t.tenant == ev.tenant);
                    if !hit {
                        continue;
                    }
                    let m = NewMessage {
                        data: json!({
                            "op": op,
                            "collection": ev.collection,
                            "record_id": ev.record_id,
                            // Filled per reader at delivery (G2): the owner's view.
                            "record": Json::Null,
                            "tenant": ev.tenant,
                            "actor": ev.actor,
                            "changed_at": ev.created_at,
                            "outbox_seq": ev.seq,
                        }),
                        attributes: [
                            ("forgebase.op".to_owned(), op.to_owned()),
                            ("forgebase.collection".to_owned(), ev.collection.clone()),
                            ("forgebase.record_id".to_owned(), ev.record_id.clone()),
                        ]
                        .into_iter()
                        .collect(),
                        ordering_key: Some(ev.record_id.clone()),
                        dedup_id: Some(format!("outbox:{}", ev.seq)),
                    };
                    let prepared = prepare(t, &m, true).and_then(|mut p| {
                        let row = source_row(ev);
                        p.bytes += i64::try_from(row.len()).unwrap_or(i64::MAX);
                        if p.bytes > t.max_message_bytes {
                            return Err(QueueError::TooLarge(format!(
                                "the changed row is {} bytes; topic {:?} accepts at most {}",
                                p.bytes,
                                bare(&t.name),
                                t.max_message_bytes
                            )));
                        }
                        p.source_row = Some(row);
                        Ok(p)
                    });
                    match prepared {
                        Ok(p) => {
                            insert_message_tx(&mut *tx, t, &p, &ev.actor, None, 1, now).await?;
                            n += 1;
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, topic = %t.name, outbox_seq = ev.seq, "record change not relayed");
                        }
                    }
                }
            }
            tx.execute(&Statement::new(
                "INSERT INTO _forgebase_queue_state (key, value) VALUES (?1, ?2) \
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                vec![RELAY_CURSOR.into(), last.into()],
            ))
            .await?;
            Ok(n)
        }
        .await;
        let n = finish(tx, r).await?;
        if n > 0 {
            self.notify();
        }
        Ok(n)
    }

    async fn set_relay_cursor(&self, value: i64) -> Result<()> {
        let mut tx = self.begin().await?;
        let r: Result<_> = async {
            tx.execute(&Statement::new(
                "INSERT INTO _forgebase_queue_state (key, value) VALUES (?1, ?2) \
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                vec![RELAY_CURSOR.into(), value.into()],
            ))
            .await?;
            Ok(())
        }
        .await;
        finish(tx, r).await
    }

    // --------------------------------------------------------- background

    /// Starts the background task: maintenance every poll interval and push
    /// delivery whenever messages may be due. Idempotent.
    pub fn start_background(&self) {
        let mut guard = self
            .inner
            .background
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if guard.is_some() {
            return;
        }
        let (stop_tx, stop_rx) = watch::channel(false);
        let weak = Arc::downgrade(&self.inner);
        let wake = self.wake_rx();
        let every = self.inner.settings.poll_interval;
        let task = tokio::spawn(background(weak, wake, stop_rx, every));
        *guard = Some((stop_tx, task));
    }

    /// Stops the background task, letting a running round finish (bounded by
    /// `grace`).
    pub async fn shutdown(&self, grace: Duration) {
        let taken = self
            .inner
            .background
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some((stop, mut task)) = taken {
            let _ = stop.send(true);
            if tokio::time::timeout(grace, &mut task).await.is_err() {
                task.abort();
            }
        }
    }
}

async fn background(
    weak: Weak<Inner>,
    mut wake: watch::Receiver<u64>,
    mut stop: watch::Receiver<bool>,
    every: Duration,
) {
    let mut last_maintenance: Option<tokio::time::Instant> = None;
    loop {
        let Some(inner) = weak.upgrade() else { return };
        let svc = QueueService { inner };
        if last_maintenance.is_none_or(|t| t.elapsed() >= every) {
            if let Err(e) = svc.run_maintenance().await {
                tracing::warn!(error = %e, "queue maintenance failed");
            }
            last_maintenance = Some(tokio::time::Instant::now());
        }
        if let Err(e) = svc.deliver_push().await {
            tracing::warn!(error = %e, "queue push delivery failed");
        }
        drop(svc);
        tokio::select! {
            _ = stop.changed() => return,
            r = wake.changed() => if r.is_err() { return },
            () = tokio::time::sleep(every) => {}
        }
    }
}
