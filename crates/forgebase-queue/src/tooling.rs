//! Webhook delivery tooling (ADR-0031): the push attempt log, dead-letter
//! redrive, "retry now", zero-downtime secret rotation, pause and resume
//! (the circuit breaker), endpoint health and audit events.
//!
//! Every mutation asks [`Action::QueueAdmin`] on the subscription, every read
//! [`Action::QueueRead`]; the subscription's owner holds both.

use forgebase_data::{AccessContext, Action, ResourceOp};
use forgebase_db::{ColumnType, Statement, TxMode, Value, WriteTx};
use forgebase_observability::names;
use serde_json::{Value as Json, json};

use crate::access::bare;
use crate::error::QueueError;
use crate::filter::Filter;
use crate::model::{
    AttemptQuery, DEFAULT_SECRET_GRACE_MS, DeliveryEngine, HEALTH_MAX_ATTEMPTS, HEALTH_WINDOW_MS, MAX_ACK_IDS,
    MAX_REDRIVE, MAX_SECRET_GRACE_MS, PauseRequest, PushAttempt, PushAttemptPage, PushHealth, PushKind, RedriveRequest,
    RedriveResult, RetryRequest, RetryResult, RotateSecretRequest, RotatedSecret, Subscription, in_range, iso,
    parse_time,
};
use crate::push::{ATTEMPT_COLS, attempt_from};
use crate::row::Row;
use crate::service::{Db, QueueService, Result, SYSTEM_PUBLISHER, SubRow, finish, sub_row};

/// Longest pause reason (characters).
const MAX_REASON: usize = 500;

/// `?first, ?first+1, …` for `n` parameters.
pub(crate) fn marks(first: usize, n: usize) -> String {
    (first..first + n)
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Nearest-rank percentile `p` (0-100) of `sorted` (ascending).
fn percentile(sorted: &[i64], p: f64) -> Option<i64> {
    if sorted.is_empty() {
        return None;
    }
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted.get(rank.clamp(1, sorted.len()) - 1).copied()
}

/// A dead-letter record as redrive reads it.
struct DeadRow {
    id: i64,
    message_id: String,
    data: String,
    attributes: String,
    ordering_key: Option<String>,
    redriven: bool,
    at: i64,
}

impl QueueService {
    // ------------------------------------------------------------- audit

    /// Records `event` in the audit log of the control database
    /// (`_forgebase_auth_audit`, where administrator actions are kept). A
    /// failure is logged, never returned: the action itself committed.
    pub(crate) async fn audit(&self, ctx: &AccessContext, event: &str, detail: Json) {
        let backend = self.inner.data.backend().clone();
        let now = self.now();
        let r: Result<()> = async {
            let d = backend.dialect();
            let mut tx = backend.begin(TxMode::Normal).await?;
            let r: Result<()> = async {
                tx.execute(&Statement::new(
                    format!(
                        "INSERT INTO _forgebase_auth_audit (at, event, outcome, actor, request_id, detail, \
                         impersonator, realm) VALUES (?1, ?2, 'success', ?3, ?4, CAST(?5 AS {}), ?6, ?7)",
                        d.column_type(ColumnType::Json)
                    ),
                    vec![
                        now.div_euclid(1000).into(),
                        event.into(),
                        ctx.actor().into(),
                        ctx.request_id.clone().into(),
                        detail.to_string().into(),
                        ctx.impersonator.clone().into(),
                        ctx.realm_id.clone().into(),
                    ],
                ))
                .await?;
                Ok(())
            }
            .await;
            finish(tx, r).await
        }
        .await;
        if let Err(e) = r {
            tracing::warn!(error = %e, event, "queue audit event not recorded");
        }
    }

    // ---------------------------------------------------------- attempts

    /// The push attempts of a subscription, newest first: deliveries and test
    /// sends, filtered by outcome, message, time and kind.
    pub async fn push_attempts(&self, ctx: &AccessContext, sub: &str, q: AttemptQuery) -> Result<PushAttemptPage> {
        self.at_sub(ctx.tenant.as_deref(), sub)
            .await?
            .push_attempts_here(ctx, sub, q)
            .await
    }

    /// [`Self::push_attempts`] on this view's set.
    pub(crate) async fn push_attempts_here(
        &self,
        ctx: &AccessContext,
        sub: &str,
        q: AttemptQuery,
    ) -> Result<PushAttemptPage> {
        let s = self
            .sub_for(
                &mut Db::Ro(self.backend()),
                ctx,
                Action::QueueRead,
                ResourceOp::List,
                sub,
            )
            .await?;
        let limit = q.limit.clamp(1, 500);
        let rows = self
            .read_replica(
                "queue.push_attempts",
                &Statement::new(
                    format!(
                        "SELECT {ATTEMPT_COLS} FROM _forgebase_queue_push_attempts WHERE subscription = ?1 \
                         AND id < ?2 AND (CAST(?3 AS TEXT) IS NULL OR outcome = ?3) \
                         AND (CAST(?4 AS TEXT) IS NULL OR message_id = ?4) \
                         AND (CAST(?5 AS BIGINT) IS NULL OR at >= ?5) AND (CAST(?6 AS BIGINT) IS NULL OR at < ?6) \
                         AND (CAST(?7 AS BIGINT) IS NULL OR test = ?7) ORDER BY id DESC LIMIT ?8"
                    ),
                    vec![
                        s.name.as_str().into(),
                        q.before.unwrap_or(i64::MAX).into(),
                        q.outcome.map(|o| o.as_str().to_owned()).into(),
                        q.message_id.clone().into(),
                        q.since_ms.into(),
                        q.until_ms.into(),
                        q.test.map(i64::from).into(),
                        (limit + 1).into(),
                    ],
                ),
            )
            .await?;
        let page = usize::try_from(limit).unwrap_or(500);
        let mut items = Row::all(&rows).map(|r| attempt_from(&r)).collect::<Result<Vec<_>>>()?;
        let next_before = (items.len() > page).then(|| items[page - 1].id);
        items.truncate(page);
        Ok(PushAttemptPage { items, next_before })
    }

    /// One push attempt of a subscription.
    pub async fn push_attempt(&self, ctx: &AccessContext, sub: &str, id: i64) -> Result<PushAttempt> {
        let view = self.at_sub(ctx.tenant.as_deref(), sub).await?;
        let s = view
            .sub_for(
                &mut Db::Ro(view.backend()),
                ctx,
                Action::QueueRead,
                ResourceOp::View,
                sub,
            )
            .await?;
        view.push_attempt_here(&s.name, id).await
    }

    /// Endpoint health of push subscription `s` at `now` (ADR-0031 §5).
    pub(crate) async fn push_health(&self, s: &SubRow, now: i64) -> Result<PushHealth> {
        let rows = self
            .read_replica(
                "queue.push_health",
                &Statement::new(
                    "SELECT outcome, latency_ms FROM _forgebase_queue_push_attempts \
                     WHERE subscription = ?1 AND test = 0 AND at > ?2 ORDER BY id DESC LIMIT ?3",
                    vec![
                        s.name.as_str().into(),
                        (now - HEALTH_WINDOW_MS).into(),
                        HEALTH_MAX_ATTEMPTS.into(),
                    ],
                ),
            )
            .await?;
        let mut latencies: Vec<i64> = Vec::with_capacity(rows.rows.len());
        let mut successes = 0_i64;
        for r in Row::all(&rows) {
            if r.text("outcome") == "success" {
                successes += 1;
            }
            latencies.push(r.int("latency_ms"));
        }
        latencies.sort_unstable();
        let attempts = i64::try_from(latencies.len()).unwrap_or(i64::MAX);
        #[allow(clippy::cast_precision_loss)]
        let success_rate = (attempts > 0).then(|| successes as f64 / attempts as f64);
        Ok(PushHealth {
            window_ms: HEALTH_WINDOW_MS,
            attempts,
            successes,
            failures: attempts - successes,
            success_rate,
            latency_p50_ms: percentile(&latencies, 50.0),
            latency_p95_ms: percentile(&latencies, 95.0),
            latency_p99_ms: percentile(&latencies, 99.0),
            consecutive_failures: s.consecutive_failures,
            last_success_at: s.last_success_at.map(iso),
            last_failure_at: s.last_failure_at.map(iso),
            last_error: s.last_error.clone(),
            paused: s.paused_at.is_some(),
            paused_at: s.paused_at.map(iso),
            paused_reason: s.paused_reason.clone(),
            pause_after_failures: s.pause_after_failures,
        })
    }

    // ------------------------------------------------------------ redrive

    /// Delivers dead letters of a subscription to it again, with a fresh
    /// attempt counter: the records named by `ids`, or with `all` every one
    /// not yet redriven (optionally since a time and matching an attribute
    /// filter), oldest first, at most `limit` per call. Each record is
    /// marked in the transaction that re-delivers it, so it is never
    /// redriven twice, even by concurrent calls. Audited.
    pub async fn redrive(&self, ctx: &AccessContext, sub: &str, req: RedriveRequest) -> Result<RedriveResult> {
        self.at_sub(ctx.tenant.as_deref(), sub)
            .await?
            .redrive_here(ctx, sub, req)
            .await
    }

    /// [`Self::redrive`] on this view's set.
    pub(crate) async fn redrive_here(
        &self,
        ctx: &AccessContext,
        sub: &str,
        req: RedriveRequest,
    ) -> Result<RedriveResult> {
        let found = self
            .sub_for(
                &mut Db::Ro(self.backend()),
                ctx,
                Action::QueueAdmin,
                ResourceOp::Update,
                sub,
            )
            .await?;
        let max = usize::try_from(MAX_REDRIVE).unwrap_or(1000);
        if req.all == !req.ids.is_empty() {
            return Err(QueueError::invalid(
                "invalid_redrive",
                "pass either ids (1-1000 dead-letter ids) or all: true",
            ));
        }
        if req.ids.len() > max {
            return Err(QueueError::invalid(
                "invalid_redrive",
                format!("at most {MAX_REDRIVE} ids per call"),
            ));
        }
        if !req.all && (req.since.is_some() || req.filter.is_some() || req.limit.is_some()) {
            return Err(QueueError::invalid(
                "invalid_redrive",
                "since, filter and limit apply to all: true only",
            ));
        }
        let limit = in_range("limit", req.limit.unwrap_or(MAX_REDRIVE), 1, MAX_REDRIVE)?;
        let since = req.since.as_deref().map(|t| parse_time("since", t)).transpose()?;
        let filter = req
            .filter
            .as_ref()
            .map(|f| Filter::parse(f).map_err(|e| QueueError::invalid("invalid_filter", e)))
            .transpose()?;
        let eng = self.store().await?;
        let now = self.now();
        let actor = ctx.actor();
        let mut tx = self.begin().await?;
        let r: Result<_> = async {
            let s = sub_row(&mut Db::Tx(&mut *tx), &found.name).await?;
            let cols = "id, message_id, data, attributes, ordering_key, redriven_at, at";
            let take = usize::try_from(limit).unwrap_or(max);
            let mut remaining = 0_i64;
            // `all`: pick the ids first (only they and the attributes are read
            // for a filter), then load at most `limit` records.
            let ids: Vec<i64> = if !req.all {
                req.ids.clone()
            } else if let Some(f) = &filter {
                let rows = tx
                    .fetch_all(&Statement::new(
                        "SELECT id, attributes FROM _forgebase_queue_dead_letters WHERE subscription = ?1 \
                         AND redriven_at IS NULL AND at >= ?2 ORDER BY id",
                        vec![s.name.as_str().into(), since.unwrap_or(i64::MIN).into()],
                    ))
                    .await?;
                let matching: Vec<i64> = Row::all(&rows)
                    .filter(|r| {
                        let attrs: std::collections::BTreeMap<String, String> =
                            serde_json::from_str(&r.text("attributes")).unwrap_or_default();
                        f.matches(&attrs)
                    })
                    .map(|r| r.int("id"))
                    .collect();
                remaining = i64::try_from(matching.len().saturating_sub(take)).unwrap_or(i64::MAX);
                matching.into_iter().take(take).collect()
            } else {
                let rows = tx
                    .fetch_all(&Statement::new(
                        "SELECT id FROM _forgebase_queue_dead_letters WHERE subscription = ?1 \
                         AND redriven_at IS NULL AND at >= ?2 ORDER BY id LIMIT ?3",
                        vec![
                            s.name.as_str().into(),
                            since.unwrap_or(i64::MIN).into(),
                            (limit + 1).into(),
                        ],
                    ))
                    .await?;
                let mut picked: Vec<i64> = Row::all(&rows).map(|r| r.int("id")).collect();
                if picked.len() > take {
                    picked.truncate(take);
                    let total = crate::row::scalar(
                        &tx.fetch_all(&Statement::new(
                            "SELECT COUNT(*) FROM _forgebase_queue_dead_letters WHERE subscription = ?1 \
                             AND redriven_at IS NULL AND at >= ?2",
                            vec![s.name.as_str().into(), since.unwrap_or(i64::MIN).into()],
                        ))
                        .await?,
                    );
                    remaining = (total - limit).max(0);
                }
                picked
            };
            let rows = if ids.is_empty() {
                forgebase_db::Rows::default()
            } else {
                let mut params: Vec<Value> = vec![s.name.as_str().into()];
                params.extend(ids.iter().map(|i| Value::from(*i)));
                tx.fetch_all(&Statement::new(
                    format!(
                        "SELECT {cols} FROM _forgebase_queue_dead_letters WHERE subscription = ?1 \
                         AND id IN ({}) ORDER BY id",
                        marks(2, ids.len())
                    ),
                    params,
                ))
                .await?
            };
            let dead: Vec<DeadRow> = Row::all(&rows)
                .map(|r| DeadRow {
                    id: r.int("id"),
                    message_id: r.text("message_id"),
                    data: r.text("data"),
                    attributes: r.text("attributes"),
                    ordering_key: r.opt_text("ordering_key"),
                    redriven: r.opt_int("redriven_at").is_some(),
                    at: r.int("at"),
                })
                .collect();
            let mut out = RedriveResult {
                remaining,
                ..RedriveResult::default()
            };
            if !req.all {
                let seen: std::collections::BTreeSet<i64> = dead.iter().map(|d| d.id).collect();
                out.not_found = req.ids.iter().copied().filter(|i| !seen.contains(i)).collect();
                out.not_found.sort_unstable();
                out.not_found.dedup();
            }
            for d in &dead {
                if d.redriven {
                    out.already_redriven.push(d.id);
                    continue;
                }
                // The mark decides: a concurrent redrive of the same record
                // finds it set and leaves the record alone.
                let marked = tx
                    .execute(&Statement::new(
                        "UPDATE _forgebase_queue_dead_letters SET redriven_at = ?3, redriven_by = ?4 \
                         WHERE subscription = ?1 AND id = ?2 AND redriven_at IS NULL",
                        vec![s.name.as_str().into(), d.id.into(), now.into(), actor.as_str().into()],
                    ))
                    .await?
                    .rows_affected;
                if marked == 0 {
                    out.already_redriven.push(d.id);
                    continue;
                }
                self.redeliver_tx(&mut *tx, *eng, &s, d, now).await?;
                out.redriven.push(d.id);
            }
            Ok(out)
        }
        .await;
        let out = finish(tx, r).await?;
        drop(eng);
        if !out.redriven.is_empty() {
            metrics::counter!(names::QUEUE_REDRIVEN_TOTAL, "subscription" => found.name.clone())
                .increment(out.redriven.len() as u64);
            self.notify();
            self.audit(
                ctx,
                "queue.dead_letters.redrive",
                json!({"subscription": bare(&found.name), "ids": out.redriven, "all": req.all}),
            )
            .await;
        }
        Ok(out)
    }

    /// Owes dead letter `d` to `s` again (attempts from zero). The message
    /// still retained: a delivery for it, unless the subscription's cursor
    /// has not reached it yet (it will be read from the topic anyway). The
    /// message dropped by retention: stored again on the topic for `s` only
    /// (same id, data, attributes and ordering key), read like any new message.
    async fn redeliver_tx(
        &self,
        tx: &mut dyn WriteTx,
        eng: DeliveryEngine,
        s: &SubRow,
        d: &DeadRow,
        now: i64,
    ) -> Result<()> {
        let rows = tx
            .fetch_all(&Statement::new(
                "SELECT seq, ordering_key, only_subscription FROM _forgebase_queue_messages WHERE id = ?1 AND topic = ?2",
                vec![d.message_id.as_str().into(), s.topic.as_str().into()],
            ))
            .await?;
        if let Some(r) = Row::all(&rows).next() {
            let seq = r.int("seq");
            let for_me = r.opt_text("only_subscription").is_none_or(|o| o == s.name);
            if s.cursor_seq < seq && for_me {
                return Ok(());
            }
            let key = r.opt_text("ordering_key");
            match eng {
                DeliveryEngine::Pgmq => {
                    self.pgmq_redeliver(tx, &s.name, seq, key, now).await?;
                }
                DeliveryEngine::Portable => {
                    tx.execute(&Statement::new(
                        "INSERT INTO _forgebase_queue_deliveries (subscription, seq, ordering_key, state, available_at) \
                         VALUES (?1, ?2, ?3, 'ready', ?4) ON CONFLICT (subscription, seq) DO NOTHING",
                        vec![s.name.as_str().into(), seq.into(), key.into(), now.into()],
                    ))
                    .await?;
                }
            }
            return Ok(());
        }
        let attrs: std::collections::BTreeMap<String, String> = serde_json::from_str(&d.attributes).unwrap_or_default();
        let bytes = d.data.len()
            + attrs.iter().map(|(k, v)| k.len() + v.len()).sum::<usize>()
            + d.ordering_key.as_ref().map_or(0, String::len);
        tx.execute(&Statement::new(
            "INSERT INTO _forgebase_queue_messages (topic, id, data, attributes, ordering_key, bytes, publisher, \
             metered, published_at, only_subscription) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, ?8, ?9)",
            vec![
                s.topic.as_str().into(),
                d.message_id.as_str().into(),
                d.data.as_str().into(),
                d.attributes.as_str().into(),
                d.ordering_key.clone().into(),
                i64::try_from(bytes).unwrap_or(i64::MAX).into(),
                SYSTEM_PUBLISHER.into(),
                now.into(),
                s.name.as_str().into(),
            ],
        ))
        .await?;
        tracing::debug!(
            subscription = %s.name,
            message = %d.message_id,
            dead_lettered_at = d.at,
            "redriven dead letter stored again for its subscription (message retention had dropped it)"
        );
        Ok(())
    }

    // ---------------------------------------------------------- retry now

    /// Makes deliveries that wait for a retry backoff ready at once (all of
    /// them, or those of `message_ids`). Leased deliveries are not touched.
    pub async fn retry_now(&self, ctx: &AccessContext, sub: &str, req: RetryRequest) -> Result<RetryResult> {
        self.at_sub(ctx.tenant.as_deref(), sub)
            .await?
            .retry_now_here(ctx, sub, req)
            .await
    }

    /// [`Self::retry_now`] on this view's set.
    pub(crate) async fn retry_now_here(
        &self,
        ctx: &AccessContext,
        sub: &str,
        req: RetryRequest,
    ) -> Result<RetryResult> {
        let s = self
            .sub_for(
                &mut Db::Ro(self.backend()),
                ctx,
                Action::QueueAdmin,
                ResourceOp::Update,
                sub,
            )
            .await?;
        if req.message_ids.len() > MAX_ACK_IDS {
            return Err(QueueError::invalid(
                "invalid_parameter",
                format!("at most {MAX_ACK_IDS} message ids"),
            ));
        }
        let eng = self.store().await?;
        let now = self.now();
        let mut tx = self.begin().await?;
        let r = self.retry_now_tx(&mut *tx, *eng, &s.name, &req.message_ids, now).await;
        let count = finish(tx, r).await?;
        drop(eng);
        if count > 0 {
            self.notify();
        }
        Ok(RetryResult { count })
    }

    async fn retry_now_tx(
        &self,
        tx: &mut dyn WriteTx,
        eng: DeliveryEngine,
        sub: &str,
        message_ids: &[String],
        now: i64,
    ) -> Result<i64> {
        let n = match eng {
            DeliveryEngine::Pgmq => self.pgmq_retry_now(tx, sub, now, message_ids).await?,
            DeliveryEngine::Portable => {
                let mut params: Vec<Value> = vec![sub.into(), now.into()];
                params.extend(message_ids.iter().map(|m| Value::from(m.as_str())));
                let only = if message_ids.is_empty() {
                    String::new()
                } else {
                    format!(
                        " AND seq IN (SELECT seq FROM _forgebase_queue_messages WHERE id IN ({}))",
                        marks(3, message_ids.len())
                    )
                };
                tx.execute(&Statement::new(
                    format!(
                        "UPDATE _forgebase_queue_deliveries SET available_at = ?2 WHERE subscription = ?1 \
                         AND state = 'ready' AND available_at > ?2{only}"
                    ),
                    params,
                ))
                .await?
                .rows_affected
            }
        };
        Ok(i64::try_from(n).unwrap_or(i64::MAX))
    }

    // ----------------------------------------------------- secret rotation

    /// Gives a webhook subscription a new signing secret, returned here only.
    /// For `grace_ms` (default 24 hours, at most 7 days) deliveries carry a
    /// signature with the new and the old secret (`t=…,v1=<new>,v1=<old>`),
    /// so receivers can switch at any time in between; then the old one is
    /// dropped. A rotation inside a grace window replaces the older secret.
    pub async fn rotate_secret(
        &self,
        ctx: &AccessContext,
        sub: &str,
        req: RotateSecretRequest,
    ) -> Result<RotatedSecret> {
        self.at_sub(ctx.tenant.as_deref(), sub)
            .await?
            .rotate_secret_here(ctx, sub, req)
            .await
    }

    /// [`Self::rotate_secret`] on this view's set.
    pub(crate) async fn rotate_secret_here(
        &self,
        ctx: &AccessContext,
        sub: &str,
        req: RotateSecretRequest,
    ) -> Result<RotatedSecret> {
        let found = self.webhook_sub(ctx, sub).await?;
        let grace = in_range(
            "grace_ms",
            req.grace_ms.unwrap_or(DEFAULT_SECRET_GRACE_MS),
            0,
            MAX_SECRET_GRACE_MS,
        )?;
        let (plain, sealed) = self.new_secret(&found.name)?;
        let now = self.now();
        let mut tx = self.begin().await?;
        let r: Result<_> = async {
            let s = sub_row(&mut Db::Tx(&mut *tx), &found.name).await?;
            let (previous, until) = match (&s.push_secret, grace) {
                (Some(current), g) if g > 0 => (Value::Blob(current.clone()), Value::from(now + g)),
                _ => (Value::Null, Value::Null),
            };
            tx.execute(&Statement::new(
                "UPDATE _forgebase_queue_subscriptions SET push_secret = ?2, push_secret_previous = ?3, \
                 push_secret_previous_until = ?4, updated_at = ?5 WHERE name = ?1",
                vec![s.name.as_str().into(), Value::Blob(sealed), previous, until, now.into()],
            ))
            .await?;
            Ok(())
        }
        .await;
        finish(tx, r).await?;
        self.audit(
            ctx,
            "queue.secret.rotate",
            json!({"subscription": bare(&found.name), "grace_ms": grace}),
        )
        .await;
        Ok(RotatedSecret {
            subscription: bare(&found.name).to_owned(),
            push_secret: plain,
            previous_expires_at: (grace > 0).then(|| iso(now + grace)),
        })
    }

    /// Ends the grace window of a rotation now: deliveries are signed with
    /// the new secret only.
    pub async fn expire_previous_secret(&self, ctx: &AccessContext, sub: &str) -> Result<Subscription> {
        self.at_sub(ctx.tenant.as_deref(), sub)
            .await?
            .expire_previous_secret_here(ctx, sub)
            .await
    }

    /// [`Self::expire_previous_secret`] on this view's set.
    pub(crate) async fn expire_previous_secret_here(&self, ctx: &AccessContext, sub: &str) -> Result<Subscription> {
        let found = self.webhook_sub(ctx, sub).await?;
        let now = self.now();
        let s = self
            .update_sub_tx(
                &found.name,
                "UPDATE _forgebase_queue_subscriptions SET push_secret_previous = NULL, \
                 push_secret_previous_until = NULL, updated_at = ?2 WHERE name = ?1",
                vec![found.name.as_str().into(), now.into()],
            )
            .await?;
        self.audit(
            ctx,
            "queue.secret.expire_previous",
            json!({"subscription": bare(&found.name)}),
        )
        .await;
        Ok(s.public())
    }

    /// The webhook push subscription `sub`, once `ctx` may administer it.
    async fn webhook_sub(&self, ctx: &AccessContext, sub: &str) -> Result<SubRow> {
        let s = self.push_sub(ctx, sub).await?;
        if s.push.as_ref().is_some_and(|p| p.kind == PushKind::Webhook) {
            Ok(s)
        } else {
            Err(QueueError::conflict(
                "not_webhook_subscription",
                format!("subscription {sub:?} does not push to a webhook; it has no signing secret"),
            ))
        }
    }

    /// The push subscription `sub`, once `ctx` may administer it.
    async fn push_sub(&self, ctx: &AccessContext, sub: &str) -> Result<SubRow> {
        let s = self
            .sub_for(
                &mut Db::Ro(self.backend()),
                ctx,
                Action::QueueAdmin,
                ResourceOp::Update,
                sub,
            )
            .await?;
        if s.push.is_none() {
            return Err(QueueError::conflict(
                "not_push_subscription",
                format!("subscription {sub:?} is a pull subscription"),
            ));
        }
        Ok(s)
    }

    /// Runs one update of subscription `name` and returns the row after it.
    async fn update_sub_tx(&self, name: &str, sql: &str, params: Vec<Value>) -> Result<SubRow> {
        let mut tx = self.begin().await?;
        let r: Result<_> = async {
            tx.execute(&Statement::new(sql, params)).await?;
            sub_row(&mut Db::Tx(&mut *tx), name).await
        }
        .await;
        finish(tx, r).await
    }

    // ------------------------------------------------------ pause, resume

    /// Pauses push delivery of a subscription: its messages accumulate (none
    /// is dead-lettered while it is paused) until [`Self::resume`].
    pub async fn pause(&self, ctx: &AccessContext, sub: &str, req: PauseRequest) -> Result<Subscription> {
        self.at_sub(ctx.tenant.as_deref(), sub)
            .await?
            .pause_here(ctx, sub, req)
            .await
    }

    /// [`Self::pause`] on this view's set.
    pub(crate) async fn pause_here(&self, ctx: &AccessContext, sub: &str, req: PauseRequest) -> Result<Subscription> {
        let found = self.push_sub(ctx, sub).await?;
        let reason = match req.reason.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
            Some(r) if r.chars().count() > MAX_REASON => {
                return Err(QueueError::invalid(
                    "invalid_parameter",
                    format!("reason is longer than {MAX_REASON} characters"),
                ));
            }
            Some(r) => r.to_owned(),
            None => format!("paused by {}", ctx.actor()),
        };
        let now = self.now();
        let s = self
            .update_sub_tx(
                &found.name,
                "UPDATE _forgebase_queue_subscriptions SET paused_at = ?2, paused_reason = ?3, updated_at = ?2 \
                 WHERE name = ?1 AND paused_at IS NULL",
                vec![found.name.as_str().into(), now.into(), reason.as_str().into()],
            )
            .await?;
        metrics::gauge!(names::QUEUE_PUSH_PAUSED, "subscription" => found.name.clone()).set(1.0);
        self.audit(
            ctx,
            "queue.subscription.pause",
            json!({"subscription": bare(&found.name), "reason": reason}),
        )
        .await;
        Ok(s.public())
    }

    /// Resumes push delivery: the circuit breaker closes (consecutive
    /// failures reset), every delivery waiting for a backoff is ready at
    /// once, and the backlog is delivered in sequence order.
    pub async fn resume(&self, ctx: &AccessContext, sub: &str) -> Result<Subscription> {
        self.at_sub(ctx.tenant.as_deref(), sub)
            .await?
            .resume_here(ctx, sub)
            .await
    }

    /// [`Self::resume`] on this view's set.
    pub(crate) async fn resume_here(&self, ctx: &AccessContext, sub: &str) -> Result<Subscription> {
        let found = self.push_sub(ctx, sub).await?;
        let eng = self.store().await?;
        let now = self.now();
        let mut tx = self.begin().await?;
        let r: Result<_> = async {
            tx.execute(&Statement::new(
                "UPDATE _forgebase_queue_subscriptions SET paused_at = NULL, paused_reason = NULL, \
                 consecutive_failures = 0, updated_at = ?2 WHERE name = ?1",
                vec![found.name.as_str().into(), now.into()],
            ))
            .await?;
            self.retry_now_tx(&mut *tx, *eng, &found.name, &[], now).await?;
            sub_row(&mut Db::Tx(&mut *tx), &found.name).await
        }
        .await;
        let s = finish(tx, r).await?;
        drop(eng);
        metrics::gauge!(names::QUEUE_PUSH_PAUSED, "subscription" => found.name.clone()).set(0.0);
        metrics::gauge!(names::QUEUE_PUSH_CONSECUTIVE_FAILURES, "subscription" => found.name.clone()).set(0.0);
        self.notify();
        self.audit(
            ctx,
            "queue.subscription.resume",
            json!({"subscription": bare(&found.name), "was_paused": found.paused_at.is_some()}),
        )
        .await;
        Ok(s.public())
    }

    // -------------------------------------------------------- maintenance

    /// Prunes the attempt log (age, and the newest `push_attempts_per_subscription`
    /// per subscription) and drops secrets whose grace window ended.
    pub(crate) async fn prune_push_state_tx(&self, tx: &mut dyn WriteTx, subs: &[SubRow], now: i64) -> Result<()> {
        let st = &self.inner.settings;
        tx.execute(&Statement::new(
            "DELETE FROM _forgebase_queue_push_attempts WHERE at < ?1",
            vec![(now - st.push_attempts_retention_ms).into()],
        ))
        .await?;
        for s in subs.iter().filter(|s| s.push.is_some()) {
            tx.execute(&Statement::new(
                "DELETE FROM _forgebase_queue_push_attempts WHERE subscription = ?1 AND id <= \
                 (SELECT id FROM _forgebase_queue_push_attempts WHERE subscription = ?1 ORDER BY id DESC \
                  LIMIT 1 OFFSET ?2)",
                vec![s.name.as_str().into(), st.push_attempts_per_subscription.max(1).into()],
            ))
            .await?;
        }
        tx.execute(&Statement::new(
            "UPDATE _forgebase_queue_subscriptions SET push_secret_previous = NULL, push_secret_previous_until = NULL \
             WHERE push_secret_previous_until IS NOT NULL AND push_secret_previous_until <= ?1",
            vec![now.into()],
        ))
        .await?;
        Ok(())
    }

    /// Deletes subscription `s` because its `ttl_ms` ran out. `true` when it
    /// was deleted (not changed or deleted meanwhile).
    pub(crate) async fn expire_subscription(&self, s: &SubRow, now: i64) -> Result<bool> {
        let eng = self.store().await?;
        let mut tx = self.begin().await?;
        let r: Result<_> = async {
            let n = tx
                .execute(&Statement::new(
                    "DELETE FROM _forgebase_queue_subscriptions WHERE name = ?1 AND expires_at IS NOT NULL \
                     AND expires_at <= ?2",
                    vec![s.name.as_str().into(), now.into()],
                ))
                .await?
                .rows_affected;
            if n > 0 {
                if *eng == DeliveryEngine::Pgmq {
                    crate::pgmq::drop_queue(&mut *tx, &s.name).await?;
                }
                tx.execute(&Statement::new(
                    "DELETE FROM _forgebase_queue_rates WHERE scope = 'subscription' AND name = ?1",
                    vec![s.name.as_str().into()],
                ))
                .await?;
            }
            Ok(n > 0)
        }
        .await;
        let deleted = finish(tx, r).await?;
        if deleted {
            tracing::info!(event = "queue.subscription.expired", subscription = %s.name, "subscription expired and was deleted");
        }
        Ok(deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank_percentiles() {
        let v: Vec<i64> = (1..=100).collect();
        assert_eq!(percentile(&v, 50.0), Some(50));
        assert_eq!(percentile(&v, 95.0), Some(95));
        assert_eq!(percentile(&v, 99.0), Some(99));
        assert_eq!(percentile(&[7], 99.0), Some(7));
        assert_eq!(percentile(&[], 50.0), None);
        assert_eq!(percentile(&[1, 2, 3, 4], 50.0), Some(2));
    }

    #[test]
    fn placeholder_lists() {
        assert_eq!(marks(3, 2), "?3, ?4");
        assert_eq!(marks(2, 0), "");
    }
}
