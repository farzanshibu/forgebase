//! Delivery: claims, acks, nacks, deadlines, dead letters, seek and snapshots.
//!
//! A subscription owes a message when it has a delivery row for it
//! (`ready` or `leased`). Rows are created lazily from the topic, in sequence
//! order, from the subscription's cursor on ("materialization"), applying the
//! subscription's attribute filter; a filtered-out message is simply skipped.
//! Claiming leases ready rows to one consumer until `lease_until`; an ack
//! deletes the row. Because every claim is one transaction on the single
//! writer, two consumers never lease the same row at once.

use std::collections::BTreeMap;
use std::time::Duration;

use forgebase_data::{AccessContext, Action};
use forgebase_db::{Statement, Value, WriteTx};
use forgebase_jobs::backoff_ms;

use crate::error::QueueError;
use crate::model::{
    AckResult, MAX_ACK_DEADLINE_MS, MAX_ACK_IDS, MAX_PULL, NewSnapshot, PullRequest, ReceivedMessage, SeekRequest,
    SeekResult, Snapshot, check_name, iso, parse_time,
};
use crate::row::{Row, scalar};
use crate::service::{
    Db, MESSAGE_COLS, Prepared, QueueService, Result, SYSTEM_PUBLISHER, SubRow, bump_rate, cursor_before, finish,
    insert_message_tx, message_from, sub_row, topic_row,
};

/// Messages turned into delivery rows per claim at most.
const MATERIALIZE_MAX: i64 = 2000;

/// Appends a history entry for a new attempt.
const PUSH_HISTORY: &str = "json_insert(CASE WHEN json_array_length(history) >= 25 THEN json_remove(history, '$[0]') \
    ELSE history END, '$[#]', json_object('attempt', attempts + 1, 'at', ?3, 'consumer', ?4))";

impl QueueService {
    /// Reads up to `MATERIALIZE_MAX` new topic messages into delivery rows of
    /// `sub` and advances its cursor. Returns the rows created.
    pub(crate) async fn materialize_tx(&self, tx: &mut dyn WriteTx, sub: &SubRow, now: i64) -> Result<i64> {
        let mut cursor = sub.cursor_seq;
        let mut created = 0;
        match &sub.filter {
            None => {
                let head = scalar(
                    &tx.fetch_all(&Statement::new(
                        "SELECT coalesce(max(seq), 0) FROM (SELECT seq FROM _forgebase_queue_messages \
                         WHERE topic = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3)",
                        vec![sub.topic.as_str().into(), cursor.into(), MATERIALIZE_MAX.into()],
                    ))
                    .await?,
                );
                if head > cursor {
                    created = i64::try_from(
                        tx.execute(&Statement::new(
                            "INSERT INTO _forgebase_queue_deliveries (subscription, seq, ordering_key, state, available_at) \
                             SELECT ?1, seq, ordering_key, 'ready', ?4 FROM _forgebase_queue_messages \
                             WHERE topic = ?2 AND seq > ?3 AND seq <= ?5",
                            vec![
                                sub.name.as_str().into(),
                                sub.topic.as_str().into(),
                                cursor.into(),
                                now.into(),
                                head.into(),
                            ],
                        ))
                        .await?
                        .rows_affected,
                    )
                    .unwrap_or(0);
                    cursor = head;
                }
            }
            Some(filter) => {
                let rows = tx
                    .fetch_all(&Statement::new(
                        "SELECT seq, attributes, ordering_key FROM _forgebase_queue_messages \
                         WHERE topic = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3",
                        vec![sub.topic.as_str().into(), cursor.into(), MATERIALIZE_MAX.into()],
                    ))
                    .await?;
                for r in Row::all(&rows) {
                    let seq = r.int("seq");
                    let attrs: BTreeMap<String, String> = r.map("attributes")?;
                    if filter.matches(&attrs) {
                        tx.execute(&Statement::new(
                            "INSERT INTO _forgebase_queue_deliveries (subscription, seq, ordering_key, state, \
                             available_at) VALUES (?1, ?2, ?3, 'ready', ?4)",
                            vec![
                                sub.name.as_str().into(),
                                seq.into(),
                                r.opt_text("ordering_key").into(),
                                now.into(),
                            ],
                        ))
                        .await?;
                        created += 1;
                    }
                    cursor = seq;
                }
            }
        }
        if cursor != sub.cursor_seq {
            tx.execute(&Statement::new(
                "UPDATE _forgebase_queue_subscriptions SET cursor_seq = ?2 WHERE name = ?1",
                vec![sub.name.as_str().into(), cursor.into()],
            ))
            .await?;
        }
        Ok(created)
    }

    /// Ends the expired leases of `sub`: the message becomes ready again after
    /// the retry backoff, or is dead-lettered when attempts are used up.
    pub(crate) async fn reap_tx(&self, tx: &mut dyn WriteTx, sub: &SubRow, now: i64) -> Result<usize> {
        let rows = tx
            .fetch_all(&Statement::new(
                "SELECT seq, attempts FROM _forgebase_queue_deliveries \
                 WHERE subscription = ?1 AND state = 'leased' AND lease_until <= ?2",
                vec![sub.name.as_str().into(), now.into()],
            ))
            .await?;
        for r in Row::all(&rows) {
            let (seq, attempts) = (r.int("seq"), r.int("attempts"));
            tx.execute(&Statement::new(
                "UPDATE _forgebase_queue_deliveries SET history = json_set(history, '$[#-1].outcome', 'expired') \
                 WHERE subscription = ?1 AND seq = ?2 AND json_array_length(history) > 0",
                vec![sub.name.as_str().into(), seq.into()],
            ))
            .await?;
            if sub.max_delivery_attempts.is_some_and(|m| attempts >= m) {
                self.dead_letter_tx(
                    tx,
                    sub,
                    seq,
                    "max delivery attempts reached (ack deadline expired)",
                    now,
                )
                .await?;
            } else {
                // The lease id stays valid until the next lease, so a late ack still counts.
                tx.execute(&Statement::new(
                    "UPDATE _forgebase_queue_deliveries SET state = 'ready', available_at = ?3, lease_until = NULL, \
                     consumer = NULL WHERE subscription = ?1 AND seq = ?2",
                    vec![
                        sub.name.as_str().into(),
                        seq.into(),
                        (now + self.backoff(sub, attempts)).into(),
                    ],
                ))
                .await?;
            }
        }
        if !rows.is_empty() {
            tx.execute(&Statement::new(
                "UPDATE _forgebase_queue_subscriptions SET expired_total = expired_total + ?2 WHERE name = ?1",
                vec![sub.name.as_str().into(), i64::try_from(rows.len()).unwrap_or(0).into()],
            ))
            .await?;
        }
        Ok(rows.len())
    }

    pub(crate) fn backoff(&self, sub: &SubRow, attempts: i64) -> i64 {
        if sub.backoff_base_ms == 0 {
            return 0;
        }
        backoff_ms(sub.backoff_base_ms, sub.backoff_max_ms, attempts, 0.0)
    }

    /// Moves a message off `sub`: a dead-letter record with its delivery
    /// history, and a copy on the dead-letter topic when one is set.
    pub(crate) async fn dead_letter_tx(
        &self,
        tx: &mut dyn WriteTx,
        sub: &SubRow,
        seq: i64,
        reason: &str,
        now: i64,
    ) -> Result<()> {
        let rows = tx
            .fetch_all(&Statement::new(
                format!(
                    "SELECT {MESSAGE_COLS}, d.attempts AS d_attempts, d.history AS d_history \
                     FROM _forgebase_queue_deliveries d JOIN _forgebase_queue_messages m ON m.seq = d.seq \
                     WHERE d.subscription = ?1 AND d.seq = ?2"
                ),
                vec![sub.name.as_str().into(), seq.into()],
            ))
            .await?;
        let Some(r) = Row::all(&rows).next() else {
            return Ok(());
        };
        let (attempts, history) = (r.int("d_attempts"), r.text("d_history"));
        let msg = message_from(&r)?;
        let mut forwarded: Option<String> = None;
        if let Some(dlt) = &sub.dead_letter_topic
            && let Some(t) = topic_row(&mut Db::Tx(&mut *tx), dlt).await?
        {
            let mut attrs = msg.attributes.clone();
            attrs.insert("forgebase.dead_letter.subscription".into(), sub.name.clone());
            attrs.insert("forgebase.dead_letter.topic".into(), msg.topic.clone());
            attrs.insert("forgebase.dead_letter.message_id".into(), msg.id.clone());
            attrs.insert("forgebase.dead_letter.attempts".into(), attempts.to_string());
            let p = Prepared {
                data: r.text("data"),
                attributes: serde_json::to_string(&attrs).map_err(|e| QueueError::Internal(e.to_string()))?,
                ordering_key: msg.ordering_key.clone(),
                dedup_id: None,
                bytes: msg.bytes,
                history: Some(history.clone()),
            };
            let published = insert_message_tx(tx, &t, &p, SYSTEM_PUBLISHER, None, 1, now).await?;
            forwarded = Some(published.message_id);
        }
        tx.execute(&Statement::new(
            "INSERT INTO _forgebase_queue_dead_letters (subscription, topic, message_id, data, attributes, \
             ordering_key, published_at, attempts, history, reason, dead_letter_topic, forwarded_message_id, at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            vec![
                sub.name.as_str().into(),
                msg.topic.as_str().into(),
                msg.id.as_str().into(),
                r.text("data").into(),
                r.text("attributes").into(),
                msg.ordering_key.clone().into(),
                r.int("published_at").into(),
                attempts.into(),
                history.into(),
                reason.into(),
                sub.dead_letter_topic.clone().into(),
                forwarded.clone().into(),
                now.into(),
            ],
        ))
        .await?;
        tx.execute(&Statement::new(
            "DELETE FROM _forgebase_queue_deliveries WHERE subscription = ?1 AND seq = ?2",
            vec![sub.name.as_str().into(), seq.into()],
        ))
        .await?;
        tx.execute(&Statement::new(
            "UPDATE _forgebase_queue_subscriptions SET dead_lettered_total = dead_lettered_total + 1 WHERE name = ?1",
            vec![sub.name.as_str().into()],
        ))
        .await?;
        metrics::counter!("forgebase_queue_dead_lettered_total", "subscription" => sub.name.clone()).increment(1);
        Ok(())
    }

    /// Cheap read: is anything claimable for `sub` right now?
    async fn has_work(&self, sub: &str, now: i64) -> Result<bool> {
        let rows = self
            .backend()
            .fetch_all(&Statement::new(
                "SELECT EXISTS (SELECT 1 FROM _forgebase_queue_deliveries WHERE subscription = ?1 \
                    AND ((state = 'ready' AND available_at <= ?2) OR (state = 'leased' AND lease_until <= ?2))) \
                 OR EXISTS (SELECT 1 FROM _forgebase_queue_subscriptions s JOIN _forgebase_queue_messages m \
                    ON m.topic = s.topic AND m.seq > s.cursor_seq WHERE s.name = ?1)",
                vec![sub.into(), now.into()],
            ))
            .await?;
        Ok(scalar(&rows) == 1)
    }

    /// Leases up to `max` messages (and, with `max_bytes`, at most that many
    /// bytes, but always one if nothing else fits) of `sub` to `consumer`.
    pub(crate) async fn claim(
        &self,
        sub: &str,
        max: i64,
        max_bytes: Option<i64>,
        consumer: &str,
    ) -> Result<Vec<ReceivedMessage>> {
        let now = self.now();
        if max < 1 || !self.has_work(sub, now).await? {
            return Ok(Vec::new());
        }
        let mut tx = self.begin().await?;
        let r = self.claim_tx(&mut *tx, sub, max, max_bytes, consumer, now).await;
        let out = finish(tx, r).await?;
        if !out.is_empty() {
            metrics::counter!("forgebase_queue_delivered_total", "subscription" => sub.to_owned())
                .increment(out.len() as u64);
        }
        Ok(out)
    }

    async fn claim_tx(
        &self,
        tx: &mut dyn WriteTx,
        name: &str,
        max: i64,
        max_bytes: Option<i64>,
        consumer: &str,
        now: i64,
    ) -> Result<Vec<ReceivedMessage>> {
        let sub = sub_row(&mut Db::Tx(&mut *tx), name).await?;
        self.reap_tx(tx, &sub, now).await?;
        self.materialize_tx(tx, &sub, now).await?;
        let order = if sub.ordering {
            "AND (d.ordering_key IS NULL OR NOT EXISTS (SELECT 1 FROM _forgebase_queue_deliveries e \
             WHERE e.subscription = d.subscription AND e.ordering_key = d.ordering_key AND e.seq < d.seq))"
        } else {
            ""
        };
        let rows = tx
            .fetch_all(&Statement::new(
                format!(
                    "SELECT {MESSAGE_COLS}, d.attempts AS d_attempts FROM _forgebase_queue_deliveries d \
                     JOIN _forgebase_queue_messages m ON m.seq = d.seq \
                     WHERE d.subscription = ?1 AND d.state = 'ready' AND d.available_at <= ?2 {order} \
                     ORDER BY d.seq LIMIT ?3"
                ),
                vec![name.into(), now.into(), max.min(MAX_PULL).into()],
            ))
            .await?;
        let mut out = Vec::new();
        let mut bytes = 0_i64;
        let until = now + sub.ack_deadline_ms;
        for r in Row::all(&rows) {
            let size = r.int("bytes");
            if let Some(limit) = max_bytes
                && !out.is_empty()
                && bytes + size > limit
            {
                break;
            }
            bytes += size;
            let seq = r.int("seq");
            let lease = format!("ack-{}", uuid::Uuid::new_v4().simple());
            tx.execute(&Statement::new(
                format!(
                    "UPDATE _forgebase_queue_deliveries SET state = 'leased', history = {PUSH_HISTORY}, \
                     attempts = attempts + 1, lease_id = ?5, lease_until = ?6, consumer = ?4 \
                     WHERE subscription = ?1 AND seq = ?2"
                ),
                vec![
                    name.into(),
                    seq.into(),
                    now.into(),
                    consumer.into(),
                    lease.as_str().into(),
                    until.into(),
                ],
            ))
            .await?;
            out.push(ReceivedMessage {
                ack_id: lease,
                delivery_attempt: r.int("d_attempts") + 1,
                ack_deadline: iso(until),
                message: message_from(&r)?,
            });
        }
        if !out.is_empty() {
            tx.execute(&Statement::new(
                "UPDATE _forgebase_queue_subscriptions SET delivered_total = delivered_total + ?2 WHERE name = ?1",
                vec![name.into(), i64::try_from(out.len()).unwrap_or(0).into()],
            ))
            .await?;
        }
        Ok(out)
    }

    /// Pulls up to `max_messages` messages. With `wait_ms`, waits (long poll)
    /// until at least one message is available or the time is up.
    pub async fn pull(&self, ctx: &AccessContext, sub: &str, req: PullRequest) -> Result<Vec<ReceivedMessage>> {
        self.allow(ctx, Action::QueueConsume)?;
        let max = crate::model::in_range("max_messages", req.max_messages.unwrap_or(10), 1, MAX_PULL)?;
        let wait = crate::model::in_range(
            "wait_ms",
            req.wait_ms.unwrap_or(0),
            0,
            self.inner.settings.max_pull_wait_ms,
        )?;
        let consumer = req.consumer.unwrap_or_else(|| "rest".into());
        check_name("consumer", &consumer)?;
        let s = sub_row(&mut Db::Ro(self.backend()), sub).await?;
        if s.push.is_some() {
            return Err(QueueError::conflict(
                "push_subscription",
                format!("subscription {sub:?} delivers by push and cannot be pulled"),
            ));
        }
        let deadline = tokio::time::Instant::now() + Duration::from_millis(u64::try_from(wait).unwrap_or(0));
        let mut rx = self.wake_rx();
        loop {
            rx.borrow_and_update();
            let got = self.claim(sub, max, None, &consumer).await?;
            let now = tokio::time::Instant::now();
            if !got.is_empty() || now >= deadline {
                return Ok(got);
            }
            let nap = self.inner.settings.poll_interval.min(deadline - now);
            tokio::select! {
                _ = rx.changed() => {}
                () = tokio::time::sleep(nap) => {}
            }
        }
    }

    fn check_ids(ids: &[String]) -> Result<()> {
        if ids.is_empty() || ids.len() > MAX_ACK_IDS {
            return Err(QueueError::invalid(
                "invalid_ack_ids",
                format!("pass 1 to {MAX_ACK_IDS} ack ids"),
            ));
        }
        Ok(())
    }

    /// Acknowledges deliveries: the messages are done for this subscription.
    pub async fn ack(&self, ctx: &AccessContext, sub: &str, ack_ids: &[String]) -> Result<AckResult> {
        self.allow(ctx, Action::QueueConsume)?;
        Self::check_ids(ack_ids)?;
        let now = self.now();
        let mut tx = self.begin().await?;
        let r: Result<_> = async {
            sub_row(&mut Db::Tx(&mut *tx), sub).await?;
            let mut res = AckResult::default();
            for id in ack_ids {
                let n = tx
                    .execute(&Statement::new(
                        "DELETE FROM _forgebase_queue_deliveries WHERE subscription = ?1 AND lease_id = ?2",
                        vec![sub.into(), id.as_str().into()],
                    ))
                    .await?
                    .rows_affected;
                if n == 0 {
                    res.unknown.push(id.clone());
                } else {
                    res.count += 1;
                }
            }
            if res.count > 0 {
                tx.execute(&Statement::new(
                    "UPDATE _forgebase_queue_subscriptions SET acked_total = acked_total + ?2 WHERE name = ?1",
                    vec![sub.into(), res.count.into()],
                ))
                .await?;
                bump_rate(&mut *tx, "subscription", sub, "acked", now, res.count).await?;
            }
            Ok(res)
        }
        .await;
        let res = finish(tx, r).await?;
        if res.count > 0 {
            metrics::counter!("forgebase_queue_acked_total", "subscription" => sub.to_owned())
                .increment(u64::try_from(res.count).unwrap_or(0));
            // An ack can unblock the next message of an ordering key.
            self.notify();
        }
        Ok(res)
    }

    /// Negative acknowledgement: redeliver after the retry backoff (or
    /// dead-letter when attempts are used up).
    pub async fn nack(&self, ctx: &AccessContext, sub: &str, ack_ids: &[String]) -> Result<AckResult> {
        self.allow(ctx, Action::QueueConsume)?;
        Self::check_ids(ack_ids)?;
        self.nack_internal(sub, ack_ids, "nacked", None).await
    }

    /// Nack with an outcome and error recorded in the delivery history (push).
    pub(crate) async fn nack_internal(
        &self,
        sub: &str,
        ack_ids: &[String],
        outcome: &str,
        error: Option<&str>,
    ) -> Result<AckResult> {
        let now = self.now();
        let mut tx = self.begin().await?;
        let r: Result<_> = async {
            let s = sub_row(&mut Db::Tx(&mut *tx), sub).await?;
            let mut res = AckResult::default();
            for id in ack_ids {
                let rows = tx
                    .fetch_all(&Statement::new(
                        "SELECT seq, attempts FROM _forgebase_queue_deliveries \
                         WHERE subscription = ?1 AND lease_id = ?2 AND state = 'leased'",
                        vec![sub.into(), id.as_str().into()],
                    ))
                    .await?;
                let Some(r) = Row::all(&rows).next() else {
                    res.unknown.push(id.clone());
                    continue;
                };
                let (seq, attempts) = (r.int("seq"), r.int("attempts"));
                let err: Value = error.map(|e| e.chars().take(500).collect::<String>()).into();
                tx.execute(&Statement::new(
                    "UPDATE _forgebase_queue_deliveries SET history = CASE WHEN ?4 IS NULL \
                       THEN json_set(history, '$[#-1].outcome', ?3) \
                       ELSE json_set(history, '$[#-1].outcome', ?3, '$[#-1].error', ?4) END \
                     WHERE subscription = ?1 AND seq = ?2 AND json_array_length(history) > 0",
                    vec![sub.into(), seq.into(), outcome.into(), err],
                ))
                .await?;
                if s.max_delivery_attempts.is_some_and(|m| attempts >= m) {
                    self.dead_letter_tx(
                        &mut *tx,
                        &s,
                        seq,
                        &format!("max delivery attempts reached ({outcome})"),
                        now,
                    )
                    .await?;
                } else {
                    tx.execute(&Statement::new(
                        "UPDATE _forgebase_queue_deliveries SET state = 'ready', available_at = ?3, lease_id = NULL, \
                         lease_until = NULL, consumer = NULL WHERE subscription = ?1 AND seq = ?2",
                        vec![sub.into(), seq.into(), (now + self.backoff(&s, attempts)).into()],
                    ))
                    .await?;
                }
                res.count += 1;
            }
            if res.count > 0 {
                tx.execute(&Statement::new(
                    "UPDATE _forgebase_queue_subscriptions SET nacked_total = nacked_total + ?2 WHERE name = ?1",
                    vec![sub.into(), res.count.into()],
                ))
                .await?;
            }
            Ok(res)
        }
        .await;
        let res = finish(tx, r).await?;
        if res.count > 0 {
            self.notify();
        }
        Ok(res)
    }

    /// Sets the deadline of deliveries to `ms` from now; `0` nacks them.
    pub async fn modify_deadline(
        &self,
        ctx: &AccessContext,
        sub: &str,
        ack_ids: &[String],
        ms: i64,
    ) -> Result<AckResult> {
        self.allow(ctx, Action::QueueConsume)?;
        Self::check_ids(ack_ids)?;
        let ms = crate::model::in_range("ack_deadline_ms", ms, 0, MAX_ACK_DEADLINE_MS)?;
        if ms == 0 {
            return self.nack_internal(sub, ack_ids, "nacked", None).await;
        }
        let now = self.now();
        let mut tx = self.begin().await?;
        let r: Result<_> = async {
            sub_row(&mut Db::Tx(&mut *tx), sub).await?;
            let mut res = AckResult::default();
            for id in ack_ids {
                let n = tx
                    .execute(&Statement::new(
                        "UPDATE _forgebase_queue_deliveries SET lease_until = ?3 \
                         WHERE subscription = ?1 AND lease_id = ?2 AND state = 'leased'",
                        vec![sub.into(), id.as_str().into(), (now + ms).into()],
                    ))
                    .await?
                    .rows_affected;
                if n == 0 {
                    res.unknown.push(id.clone());
                } else {
                    res.count += 1;
                }
            }
            Ok(res)
        }
        .await;
        finish(tx, r).await
    }

    /// Gives back every lease `consumer` holds on `sub` without counting the
    /// attempt (a stream that closed). Returns the number released.
    pub(crate) async fn release_consumer(&self, sub: &str, consumer: &str) -> Result<u64> {
        let now = self.now();
        let mut tx = self.begin().await?;
        let r: Result<_> = async {
            Ok(tx
                .execute(&Statement::new(
                    "UPDATE _forgebase_queue_deliveries SET state = 'ready', available_at = ?3, \
                     attempts = max(attempts - 1, 0), lease_id = NULL, lease_until = NULL, consumer = NULL, \
                     history = json_set(history, '$[#-1].outcome', 'released') \
                     WHERE subscription = ?1 AND consumer = ?2 AND state = 'leased' AND json_array_length(history) > 0",
                    vec![sub.into(), consumer.into(), now.into()],
                ))
                .await?
                .rows_affected)
        }
        .await;
        let n = finish(tx, r).await?;
        if n > 0 {
            self.notify();
        }
        Ok(n)
    }

    /// Messages and bytes `consumer` holds on `sub` with unexpired leases.
    pub(crate) async fn in_flight(&self, sub: &str, consumer: &str) -> Result<(i64, i64)> {
        let rows = self
            .backend()
            .fetch_all(&Statement::new(
                "SELECT COUNT(*) AS n, coalesce(sum(m.bytes), 0) AS b FROM _forgebase_queue_deliveries d \
                 JOIN _forgebase_queue_messages m ON m.seq = d.seq \
                 WHERE d.subscription = ?1 AND d.consumer = ?2 AND d.state = 'leased' AND d.lease_until > ?3",
                vec![sub.into(), consumer.into(), self.now().into()],
            ))
            .await?;
        let r = Row::new(&rows, 0);
        Ok((r.int("n"), r.int("b")))
    }

    /// Streaming pull with flow control: leases as many messages as fit in
    /// `max_messages` / `max_bytes` minus what `consumer` already holds.
    pub(crate) async fn stream_batch(
        &self,
        sub: &str,
        consumer: &str,
        max_messages: i64,
        max_bytes: i64,
    ) -> Result<Vec<ReceivedMessage>> {
        let (n, b) = self.in_flight(sub, consumer).await?;
        let room = (max_messages - n).min(100);
        if room < 1 || (n > 0 && b >= max_bytes) {
            return Ok(Vec::new());
        }
        // Nothing in flight: one message always goes, however large.
        let byte_room = if n == 0 { (max_bytes - b).max(1) } else { max_bytes - b };
        self.claim(sub, room, Some(byte_room), consumer).await
    }

    // ------------------------------------------------------------- replay

    /// Seeks a subscription. To a time: messages published before it count as
    /// acknowledged, every retained message from it on is delivered (again).
    /// To a snapshot: the acknowledgement state saved in it is restored.
    /// Outstanding leases are dropped either way.
    pub async fn seek(&self, ctx: &AccessContext, sub: &str, req: SeekRequest) -> Result<SeekResult> {
        self.allow(ctx, Action::QueueAdmin)?;
        let ts = match (&req.time, &req.snapshot) {
            (Some(t), None) => Some(parse_time("time", t)?),
            (None, Some(_)) => None,
            _ => {
                return Err(QueueError::invalid(
                    "invalid_seek",
                    "pass exactly one of time and snapshot",
                ));
            }
        };
        let now = self.now();
        let mut tx = self.begin().await?;
        let r: Result<_> = async {
            let s = sub_row(&mut Db::Tx(&mut *tx), sub).await?;
            tx.execute(&Statement::new(
                "DELETE FROM _forgebase_queue_deliveries WHERE subscription = ?1",
                vec![sub.into()],
            ))
            .await?;
            let (cursor, restored) = if let Some(ts) = ts {
                (cursor_before(&mut *tx, &s.topic, ts).await?, 0)
            } else {
                let name = req.snapshot.as_deref().unwrap_or_default();
                let snap = snapshot_row(&mut Db::Tx(&mut *tx), name).await?;
                if snap.topic != s.topic {
                    return Err(QueueError::invalid(
                        "invalid_seek",
                        format!("snapshot {name:?} is of topic {:?}", snap.topic),
                    ));
                }
                let n = tx
                    .execute(&Statement::new(
                        "INSERT INTO _forgebase_queue_deliveries (subscription, seq, ordering_key, state, available_at) \
                         SELECT ?1, i.seq, m.ordering_key, 'ready', ?3 FROM _forgebase_queue_snapshot_items i \
                         JOIN _forgebase_queue_messages m ON m.seq = i.seq WHERE i.snapshot = ?2",
                        vec![sub.into(), name.into(), now.into()],
                    ))
                    .await?
                    .rows_affected;
                (snap.cursor_seq, i64::try_from(n).unwrap_or(0))
            };
            tx.execute(&Statement::new(
                "UPDATE _forgebase_queue_subscriptions SET cursor_seq = ?2, updated_at = ?3 WHERE name = ?1",
                vec![sub.into(), cursor.into(), now.into()],
            ))
            .await?;
            Ok(SeekResult {
                cursor_seq: cursor,
                restored,
            })
        }
        .await;
        let res = finish(tx, r).await?;
        self.notify();
        Ok(res)
    }

    /// Saves the acknowledgement state of a subscription (its cursor and the
    /// messages it still owes) for a later [`QueueService::seek`].
    pub async fn create_snapshot(&self, ctx: &AccessContext, new: NewSnapshot) -> Result<Snapshot> {
        self.allow(ctx, Action::QueueAdmin)?;
        check_name("snapshot name", &new.name)?;
        let now = self.now();
        let mut tx = self.begin().await?;
        let r: Result<_> = async {
            let s = sub_row(&mut Db::Tx(&mut *tx), &new.subscription).await?;
            if snapshot_row(&mut Db::Tx(&mut *tx), &new.name).await.is_ok() {
                return Err(QueueError::Exists {
                    kind: "snapshot",
                    name: new.name.clone(),
                });
            }
            let t = topic_row(&mut Db::Tx(&mut *tx), &s.topic)
                .await?
                .ok_or_else(|| QueueError::TopicNotFound(s.topic.clone()))?;
            // Bring the delivery rows up to date so the cursor means "everything before it".
            loop {
                let cur = sub_row(&mut Db::Tx(&mut *tx), &s.name).await?;
                let before = cur.cursor_seq;
                self.materialize_tx(&mut *tx, &cur, now).await?;
                let after = sub_row(&mut Db::Tx(&mut *tx), &s.name).await?.cursor_seq;
                if after == before {
                    break;
                }
            }
            let cursor = sub_row(&mut Db::Tx(&mut *tx), &s.name).await?.cursor_seq;
            tx.execute(&Statement::new(
                "INSERT INTO _forgebase_queue_snapshots (name, subscription, topic, cursor_seq, created_by, \
                 created_at, expires_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                vec![
                    new.name.as_str().into(),
                    s.name.as_str().into(),
                    s.topic.as_str().into(),
                    cursor.into(),
                    ctx.actor().into(),
                    now.into(),
                    (now + t.retention_ms).into(),
                ],
            ))
            .await?;
            tx.execute(&Statement::new(
                "INSERT INTO _forgebase_queue_snapshot_items (snapshot, seq) \
                 SELECT ?1, seq FROM _forgebase_queue_deliveries WHERE subscription = ?2",
                vec![new.name.as_str().into(), s.name.as_str().into()],
            ))
            .await?;
            snapshot_row(&mut Db::Tx(&mut *tx), &new.name).await
        }
        .await;
        finish(tx, r).await
    }

    /// One snapshot.
    pub async fn snapshot(&self, ctx: &AccessContext, name: &str) -> Result<Snapshot> {
        self.allow(ctx, Action::QueueRead)?;
        snapshot_row(&mut Db::Ro(self.backend()), name).await
    }

    /// Every snapshot, by name.
    pub async fn snapshots(&self, ctx: &AccessContext) -> Result<Vec<Snapshot>> {
        self.allow(ctx, Action::QueueRead)?;
        let rows = self
            .backend()
            .fetch_all(&Statement::raw(format!("{SNAPSHOT_SELECT} ORDER BY s.name")))
            .await?;
        Ok(Row::all(&rows).map(|r| snapshot_from(&r)).collect())
    }

    /// Deletes a snapshot.
    pub async fn delete_snapshot(&self, ctx: &AccessContext, name: &str) -> Result<()> {
        self.allow(ctx, Action::QueueAdmin)?;
        let mut tx = self.begin().await?;
        let r: Result<_> = async {
            let n = tx
                .execute(&Statement::new(
                    "DELETE FROM _forgebase_queue_snapshots WHERE name = ?1",
                    vec![name.into()],
                ))
                .await?
                .rows_affected;
            if n == 0 {
                return Err(QueueError::SnapshotNotFound(name.to_owned()));
            }
            Ok(())
        }
        .await;
        finish(tx, r).await
    }
}

const SNAPSHOT_SELECT: &str = "SELECT s.name, s.subscription, s.topic, s.cursor_seq, s.created_by, s.created_at, \
    s.expires_at, (SELECT COUNT(*) FROM _forgebase_queue_snapshot_items i WHERE i.snapshot = s.name) AS unacked \
    FROM _forgebase_queue_snapshots s";

fn snapshot_from(r: &Row<'_>) -> Snapshot {
    Snapshot {
        name: r.text("name"),
        subscription: r.text("subscription"),
        topic: r.text("topic"),
        cursor_seq: r.int("cursor_seq"),
        unacked: r.int("unacked"),
        created_by: r.text("created_by"),
        created_at: iso(r.int("created_at")),
        expires_at: iso(r.int("expires_at")),
    }
}

async fn snapshot_row(db: &mut Db<'_>, name: &str) -> Result<Snapshot> {
    let rows = db
        .fetch(&Statement::new(
            format!("{SNAPSHOT_SELECT} WHERE s.name = ?1"),
            vec![name.into()],
        ))
        .await?;
    Row::all(&rows)
        .next()
        .map(|r| snapshot_from(&r))
        .ok_or_else(|| QueueError::SnapshotNotFound(name.to_owned()))
}
