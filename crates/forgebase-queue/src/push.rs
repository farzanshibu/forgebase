//! Push delivery: signed HTTPS webhooks and server functions.
//!
//! A push subscription is consumed by the background task as consumer
//! `push`: each round claims up to `push_batch` messages per subscription and
//! delivers them one after another (so ordering keys keep their order). A
//! `2xx` answer (or a function that finishes without error) acknowledges the
//! message; anything else nacks it with the error in the delivery history,
//! so it is retried after the subscription's backoff and dead-lettered after
//! `max_delivery_attempts`.
//!
//! Webhook requests are `POST` with a JSON body and
//! `X-Forgebase-Signature: t=<unix seconds>,v1=<hex HMAC-SHA256 of "<t>.<body>">`
//! under the subscription's secret. While a rotated secret is in its grace
//! window the header carries one `v1` per secret, the new one first
//! (`t=…,v1=<new>,v1=<old>`): receivers accept the request when **any**
//! `v1` matches ([`verify`]). They go through the SSRF-safe client of FB-60
//! (every resolved address must be public; no DNS rebinding).
//!
//! Every attempt is recorded in the attempt log (ADR-0031 §1) in the
//! transaction of its ack or nack, with the endpoint health of the
//! subscription; `pause_after_failures` consecutive failures pause the
//! subscription (the circuit breaker, §5).

use std::collections::BTreeMap;
use std::time::Instant;

use forgebase_data::{AccessContext, Action, ResourceOp};
use forgebase_db::{Statement, WriteTx};
use forgebase_functions::fetch::{FetchError, FetchRequest};
use forgebase_functions::model::Trigger;
use forgebase_observability::names;
use hmac::{Hmac, KeyInit, Mac};
use serde_json::{Value as Json, json};
use sha2::Sha256;

use crate::access::bare;
use crate::error::QueueError;
use crate::model::{
    Message, PushAttempt, PushConfig, PushErrorClass, PushKind, PushOutcome, ReceivedMessage, TestSendRequest, iso,
};
use crate::row::{Row, scalar};
use crate::service::{Db, QueueService, Result, SYSTEM_PUBLISHER, SubRow, finish, sub_row};

/// Header carrying the webhook signature.
pub const SIGNATURE_HEADER: &str = "x-forgebase-signature";

/// `type` of the envelope of a test send, and its marker header.
pub const TEST_EVENT_TYPE: &str = "forgebase.test";

/// Header set (to `1`) on test sends only.
pub const TEST_HEADER: &str = "x-forgebase-test";

/// Response headers kept in the attempt log (everything else is dropped:
/// cookies and the like are not ForgeBase's to store).
const RESPONSE_HEADERS: &[&str] = &[
    "content-type",
    "content-length",
    "date",
    "server",
    "retry-after",
    "location",
    "cache-control",
    "x-request-id",
    "request-id",
    "x-correlation-id",
    "x-amzn-requestid",
    "cf-ray",
    "www-authenticate",
];

/// Longest error text kept.
const MAX_ERROR: usize = 500;

fn hmac_hex(secret: &str, ts: i64, body: &[u8]) -> String {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(secret.as_bytes())
        .unwrap_or_else(|_| unreachable!("HMAC takes any key length"));
    mac.update(ts.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    forgebase_auth::crypto::hex(&mac.finalize().into_bytes())
}

/// `t=<ts>,v1=<hex>` for `body` signed with `secret` at `ts` (Unix seconds).
pub fn sign(secret: &str, ts: i64, body: &[u8]) -> String {
    sign_all(&[secret], ts, body)
}

/// `t=<ts>,v1=<hex>,v1=<hex>…`: one signature per secret, in order (the
/// current secret first during a rotation's grace window).
pub fn sign_all(secrets: &[&str], ts: i64, body: &[u8]) -> String {
    let mut out = format!("t={ts}");
    for s in secrets {
        out.push_str(",v1=");
        out.push_str(&hmac_hex(s, ts, body));
    }
    out
}

/// Checks a signature header against `body` (receivers can use it; tests do),
/// accepting timestamps within `tolerance_secs` of `now_secs`. The header
/// may carry several `v1` signatures (a secret rotation): it is valid when
/// any of them matches `secret`.
pub fn verify(secret: &str, header: &str, body: &[u8], now_secs: i64, tolerance_secs: i64) -> bool {
    let mut ts = None;
    let mut sigs = Vec::new();
    for part in header.split(',') {
        match part.trim().split_once('=') {
            Some(("t", v)) => ts = v.parse::<i64>().ok(),
            Some(("v1", v)) => sigs.push(v.to_owned()),
            _ => {}
        }
    }
    let Some(ts) = ts else { return false };
    if sigs.is_empty() || (now_secs - ts).abs() > tolerance_secs {
        return false;
    }
    let expected = hmac_hex(secret, ts, body);
    // Compare every candidate (no early exit).
    let mut ok = false;
    for s in &sigs {
        ok |= forgebase_auth::crypto::ct_eq(expected.as_bytes(), s.as_bytes());
    }
    ok
}

/// The header as recorded in the attempt log: the timestamp kept, every
/// signature masked.
fn masked(header: &str) -> String {
    header
        .split(',')
        .map(|p| if p.starts_with("v1=") { "v1=<redacted>" } else { p })
        .collect::<Vec<_>>()
        .join(",")
}

/// The error class of a failed fetch (the SSRF-safe client reports
/// resolution, connection, TLS and protocol failures in words).
fn classify(e: &FetchError) -> PushErrorClass {
    match e {
        FetchError::Blocked(_) => PushErrorClass::SsrfRefused,
        FetchError::Invalid(_) => PushErrorClass::InvalidRequest,
        FetchError::Failed(m) => {
            let m = m.to_ascii_lowercase();
            if m.contains("timed out") {
                PushErrorClass::Timeout
            } else if m.contains("could not resolve") || m.contains("has no addresses") {
                PushErrorClass::Dns
            } else if m.contains("could not connect") {
                PushErrorClass::Connect
            } else if m.contains("tls") || m.contains("certificate") {
                PushErrorClass::Tls
            } else if m.contains("redirect") {
                PushErrorClass::Redirect
            } else {
                PushErrorClass::Protocol
            }
        }
    }
}

fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// One push attempt, before it is stored.
#[derive(Debug, Clone)]
pub(crate) struct AttemptRecord {
    pub(crate) message_id: String,
    pub(crate) attempt: i64,
    pub(crate) test: bool,
    pub(crate) kind: PushKind,
    pub(crate) endpoint: String,
    pub(crate) at: i64,
    pub(crate) latency_ms: i64,
    pub(crate) outcome: PushOutcome,
    pub(crate) status: Option<i64>,
    pub(crate) error_class: Option<PushErrorClass>,
    pub(crate) error: Option<String>,
    pub(crate) request_headers: BTreeMap<String, String>,
    pub(crate) response_headers: BTreeMap<String, String>,
    pub(crate) response_body: Option<String>,
    pub(crate) response_bytes: Option<i64>,
    pub(crate) response_truncated: bool,
}

impl AttemptRecord {
    fn new(push: &PushConfig, message_id: &str, attempt: i64, test: bool, at: i64) -> Self {
        Self {
            message_id: message_id.to_owned(),
            attempt,
            test,
            kind: push.kind,
            endpoint: push.endpoint.clone(),
            at,
            latency_ms: 0,
            outcome: PushOutcome::Failure,
            status: None,
            error_class: None,
            error: None,
            request_headers: BTreeMap::new(),
            response_headers: BTreeMap::new(),
            response_body: None,
            response_bytes: None,
            response_truncated: false,
        }
    }

    fn fail(mut self, class: PushErrorClass, error: impl Into<String>, started: Instant) -> Self {
        self.outcome = PushOutcome::Failure;
        self.error_class = Some(class);
        self.error = Some(clip(&error.into(), MAX_ERROR));
        self.latency_ms = elapsed_ms(started);
        self
    }

    /// The failure as one line (delivery history, `last_error`).
    pub(crate) fn summary(&self) -> String {
        let class = self.error_class.map_or("error", PushErrorClass::as_str);
        format!("{class}: {}", self.error.as_deref().unwrap_or("failed"))
    }

    pub(crate) fn succeeded(&self) -> bool {
        self.outcome == PushOutcome::Success
    }
}

fn elapsed_ms(started: Instant) -> i64 {
    i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX)
}

pub(crate) const ATTEMPT_COLS: &str = "id, subscription, message_id, attempt, test, kind, endpoint, at, latency_ms, \
    outcome, status, error_class, error, request_headers, response_headers, response_body, response_bytes, \
    response_truncated";

pub(crate) fn attempt_from(r: &Row<'_>) -> Result<PushAttempt> {
    Ok(PushAttempt {
        id: r.int("id"),
        subscription: bare(&r.text("subscription")).to_owned(),
        message_id: r.text("message_id"),
        attempt: r.int("attempt"),
        test: r.int("test") == 1,
        kind: PushKind::parse(&r.text("kind")).unwrap_or(PushKind::Webhook),
        endpoint: r.text("endpoint"),
        at: iso(r.int("at")),
        latency_ms: r.int("latency_ms"),
        outcome: if r.text("outcome") == "success" {
            PushOutcome::Success
        } else {
            PushOutcome::Failure
        },
        status: r.opt_int("status"),
        error_class: r.opt_text("error_class").as_deref().and_then(PushErrorClass::parse),
        error: r.opt_text("error"),
        request_headers: r.map("request_headers")?,
        response_headers: r.map("response_headers")?,
        response_body: r.opt_text("response_body"),
        response_bytes: r.opt_int("response_bytes"),
        response_truncated: r.int("response_truncated") == 1,
    })
}

/// Stores an attempt; returns its id.
pub(crate) async fn insert_attempt_tx(tx: &mut dyn WriteTx, sub: &str, a: &AttemptRecord) -> Result<i64> {
    let json = |m: &BTreeMap<String, String>| serde_json::to_string(m).unwrap_or_else(|_| "{}".into());
    let rows = tx
        .fetch_all(&Statement::new(
            "INSERT INTO _forgebase_queue_push_attempts (subscription, message_id, attempt, test, kind, endpoint, at, \
             latency_ms, outcome, status, error_class, error, request_headers, response_headers, response_body, \
             response_bytes, response_truncated) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17) RETURNING id",
            vec![
                sub.into(),
                a.message_id.as_str().into(),
                a.attempt.into(),
                i64::from(a.test).into(),
                a.kind.as_str().into(),
                a.endpoint.as_str().into(),
                a.at.into(),
                a.latency_ms.into(),
                a.outcome.as_str().into(),
                a.status.into(),
                a.error_class.map(|c| c.as_str().to_owned()).into(),
                a.error.clone().into(),
                json(&a.request_headers).into(),
                json(&a.response_headers).into(),
                a.response_body.clone().into(),
                a.response_bytes.into(),
                i64::from(a.response_truncated).into(),
            ],
        ))
        .await?;
    Ok(scalar(&rows))
}

/// What settling one push attempt changed.
struct Settled {
    /// The attempt paused the subscription (circuit breaker).
    paused: bool,
}

impl QueueService {
    /// One push round over every push subscription. Returns the number of
    /// messages delivered successfully.
    ///
    /// Sharded (FB-172): on every set, skipping the subscriptions of tenants
    /// moving in or out.
    pub async fn deliver_push(&self) -> Result<usize> {
        let mut n = 0;
        for view in self.views() {
            n += view.deliver_push_here().await?;
        }
        Ok(n)
    }

    pub(crate) async fn deliver_push_here(&self) -> Result<usize> {
        let mut params = Vec::new();
        let guard = forgebase_jobs::tenant_guard("tenant", &self.excluded(), &mut params, 0);
        let rows = self
            .backend()
            .fetch_all(&forgebase_db::Statement::new(
                format!(
                    "SELECT {} FROM _forgebase_queue_subscriptions WHERE push_kind IS NOT NULL \
                     AND paused_at IS NULL{guard} ORDER BY name",
                    crate::service::SUB_COLS
                ),
                params,
            ))
            .await?;
        let subs: Vec<SubRow> = crate::row::Row::all(&rows)
            .map(|r| SubRow::from_row(&r))
            .collect::<Result<_>>()?;
        let mut delivered = 0;
        for chunk in subs.chunks(self.inner.settings.push_concurrency.max(1)) {
            let results = futures_util::future::join_all(chunk.iter().map(|s| self.push_one(s))).await;
            for r in results {
                match r {
                    Ok(n) => delivered += n,
                    Err(e) => tracing::warn!(error = %e, "push round failed"),
                }
            }
        }
        Ok(delivered)
    }

    async fn push_one(&self, sub: &SubRow) -> Result<usize> {
        let Some(push) = &sub.push else { return Ok(0) };
        if sub.paused_at.is_some() {
            return Ok(0);
        }
        let batch = self
            .claim(&sub.name, self.inner.settings.push_batch, None, "push")
            .await?;
        let mut ok = 0;
        for m in &batch {
            let envelope = Self::push_body(sub, m);
            let rec = self
                .send_push(sub, push, &envelope, &m.message.id, m.delivery_attempt)
                .await;
            if rec.succeeded() {
                ok += 1;
            } else {
                tracing::debug!(subscription = %sub.name, message = %m.message.id, error = %rec.summary(), "push failed");
                metrics::counter!("forgebase_queue_push_failures_total", "subscription" => sub.name.clone())
                    .increment(1);
            }
            if self.settle_push(sub, &m.ack_id, &rec).await?.paused {
                // The circuit breaker opened: the rest of the batch waits for
                // the resume, without counting an attempt.
                self.release_consumer_here(&sub.name, "push").await?;
                break;
            }
        }
        Ok(ok)
    }

    fn push_body(sub: &SubRow, m: &ReceivedMessage) -> Json {
        json!({
            "subscription": bare(&sub.name),
            "delivery_attempt": m.delivery_attempt,
            "message": m.message,
        })
    }

    /// The secrets that sign a delivery at `now`: the current one, and the
    /// one a rotation replaced while its grace window lasts.
    fn signing_secrets(&self, sub: &SubRow, now: i64) -> Result<Vec<String>> {
        let Some(sealed) = &sub.push_secret else {
            return Err(QueueError::Internal("the subscription has no signing secret".into()));
        };
        let mut out = vec![self.open_secret(&sub.name, sealed)?];
        if let (Some(old), Some(until)) = (&sub.push_secret_previous, sub.push_secret_previous_until)
            && until > now
        {
            out.push(self.open_secret(&sub.name, old)?);
        }
        Ok(out)
    }

    /// Sends `envelope` to the push endpoint and records what happened.
    async fn send_push(
        &self,
        sub: &SubRow,
        push: &PushConfig,
        envelope: &Json,
        message_id: &str,
        attempt: i64,
    ) -> AttemptRecord {
        self.send_push_as(sub, push, envelope, message_id, attempt, false).await
    }

    async fn send_push_as(
        &self,
        sub: &SubRow,
        push: &PushConfig,
        envelope: &Json,
        message_id: &str,
        attempt: i64,
        test: bool,
    ) -> AttemptRecord {
        let now = self.now();
        let started = Instant::now();
        let rec = AttemptRecord::new(push, message_id, attempt, test, now);
        match push.kind {
            PushKind::Webhook => self.post_webhook(sub, &push.endpoint, envelope, rec, started).await,
            PushKind::Function => self.call_function(&push.endpoint, envelope, rec, started).await,
        }
    }

    async fn post_webhook(
        &self,
        sub: &SubRow,
        url: &str,
        envelope: &Json,
        mut rec: AttemptRecord,
        started: Instant,
    ) -> AttemptRecord {
        let Some(fetcher) = self.inner.fetcher.get() else {
            return rec.fail(
                PushErrorClass::Config,
                "no push client is configured on this server",
                started,
            );
        };
        let secrets = match self.signing_secrets(sub, rec.at) {
            Ok(s) => s,
            Err(e) => return rec.fail(PushErrorClass::Config, e.to_string(), started),
        };
        let body = match serde_json::to_vec(envelope) {
            Ok(b) => b,
            Err(e) => return rec.fail(PushErrorClass::InvalidRequest, e.to_string(), started),
        };
        let ts = rec.at.div_euclid(1000);
        let refs: Vec<&str> = secrets.iter().map(String::as_str).collect();
        let signature = sign_all(&refs, ts, &body);
        let mut headers = vec![
            ("content-type".to_owned(), "application/json".to_owned()),
            (SIGNATURE_HEADER.to_owned(), signature),
            ("x-forgebase-subscription".to_owned(), bare(&sub.name).to_owned()),
            ("x-forgebase-message-id".to_owned(), rec.message_id.clone()),
            ("x-forgebase-delivery-attempt".to_owned(), rec.attempt.to_string()),
        ];
        if rec.test {
            headers.push((TEST_HEADER.to_owned(), "1".to_owned()));
        }
        rec.request_headers = headers
            .iter()
            .map(|(k, v)| {
                let v = if k == SIGNATURE_HEADER { masked(v) } else { v.clone() };
                (k.clone(), v)
            })
            .collect();
        let req = FetchRequest {
            method: "POST".into(),
            url: url.to_owned(),
            headers,
            body,
        };
        let res = match tokio::time::timeout(self.inner.settings.push_timeout, fetcher.fetch(req, &[])).await {
            Err(_) => return rec.fail(PushErrorClass::Timeout, "the push request timed out", started),
            Ok(Err(e)) => {
                let class = classify(&e);
                return rec.fail(class, e.to_string(), started);
            }
            Ok(Ok(r)) => r,
        };
        rec.latency_ms = elapsed_ms(started);
        rec.status = Some(i64::from(res.status));
        rec.response_headers = res
            .headers
            .iter()
            .filter(|(k, _)| RESPONSE_HEADERS.contains(&k.to_ascii_lowercase().as_str()))
            .map(|(k, v)| (k.to_ascii_lowercase(), clip(v, 1024)))
            .collect();
        let limit = self.inner.settings.push_response_body_bytes;
        rec.response_bytes = Some(i64::try_from(res.body.len()).unwrap_or(i64::MAX));
        rec.response_truncated = res.body.len() > limit;
        if !res.body.is_empty() && limit > 0 {
            let text = String::from_utf8_lossy(&res.body[..res.body.len().min(limit)]);
            rec.response_body = Some(text.into_owned());
        }
        if (200..300).contains(&res.status) {
            rec.outcome = PushOutcome::Success;
        } else {
            rec.outcome = PushOutcome::Failure;
            rec.error_class = Some(PushErrorClass::HttpStatus);
            rec.error = Some(format!("the endpoint answered HTTP {}", res.status));
        }
        rec
    }

    async fn call_function(&self, name: &str, input: &Json, rec: AttemptRecord, started: Instant) -> AttemptRecord {
        let Some(functions) = self.inner.functions.get() else {
            return rec.fail(
                PushErrorClass::Config,
                "server functions are not available on this server",
                started,
            );
        };
        let run = functions.invoke(name, Trigger::Job, input, Some(format!("queue:{}", rec.message_id)));
        match tokio::time::timeout(self.inner.settings.push_timeout, run).await {
            Err(_) => rec.fail(PushErrorClass::Timeout, "the function timed out", started),
            Ok(Err(e)) => rec.fail(PushErrorClass::Function, e.to_string(), started),
            Ok(Ok(_)) => {
                let mut rec = rec;
                rec.latency_ms = elapsed_ms(started);
                rec.outcome = PushOutcome::Success;
                rec
            }
        }
    }

    /// Acks or nacks the delivery of one push attempt, stores the attempt and
    /// updates the endpoint health, in one transaction; opens the circuit
    /// breaker after `pause_after_failures` consecutive failures.
    async fn settle_push(&self, sub: &SubRow, ack_id: &str, rec: &AttemptRecord) -> Result<Settled> {
        let eng = self.store().await?;
        let now = self.now();
        let ids = [ack_id.to_owned()];
        let mut tx = self.begin().await?;
        let r: Result<_> = async {
            let s = sub_row(&mut Db::Tx(&mut *tx), &sub.name).await?;
            let mut paused: Option<(i64, String)> = None;
            let failures;
            let res = if rec.succeeded() {
                failures = 0;
                tx.execute(&Statement::new(
                    "UPDATE _forgebase_queue_subscriptions SET consecutive_failures = 0, last_success_at = ?2 \
                     WHERE name = ?1",
                    vec![s.name.as_str().into(), rec.at.into()],
                ))
                .await?;
                self.ack_tx(&mut *tx, *eng, &s.name, &ids, now).await?
            } else {
                failures = s.consecutive_failures + 1;
                let err = clip(&rec.summary(), MAX_ERROR);
                tx.execute(&Statement::new(
                    "UPDATE _forgebase_queue_subscriptions SET consecutive_failures = consecutive_failures + 1, \
                     last_failure_at = ?2, last_error = ?3 WHERE name = ?1",
                    vec![s.name.as_str().into(), rec.at.into(), err.as_str().into()],
                ))
                .await?;
                if s.paused_at.is_none() && s.pause_after_failures.is_some_and(|n| failures >= n) {
                    let reason = clip(
                        &format!("auto: {failures} consecutive failed deliveries (last: {err})"),
                        MAX_ERROR,
                    );
                    tx.execute(&Statement::new(
                        "UPDATE _forgebase_queue_subscriptions SET paused_at = ?2, paused_reason = ?3 \
                         WHERE name = ?1 AND paused_at IS NULL",
                        vec![s.name.as_str().into(), now.into(), reason.as_str().into()],
                    ))
                    .await?;
                    paused = Some((failures, reason));
                }
                // Paused (now or before): the message stays, whatever its attempts.
                let dead_letter = s.paused_at.is_none() && paused.is_none();
                self.nack_tx(&mut *tx, *eng, &s, &ids, "failed", Some(&err), now, dead_letter)
                    .await?
            };
            insert_attempt_tx(&mut *tx, &s.name, rec).await?;
            Ok((res, failures, paused))
        }
        .await;
        let (res, failures, paused) = finish(tx, r).await?;
        drop(eng);
        let name = sub.name.clone();
        metrics::counter!(names::QUEUE_PUSH_ATTEMPTS_TOTAL, "subscription" => name.clone(), "outcome" => rec.outcome.as_str())
            .increment(1);
        #[allow(clippy::cast_precision_loss)]
        let secs = rec.latency_ms as f64 / 1000.0;
        metrics::histogram!(names::QUEUE_PUSH_LATENCY_SECONDS, "subscription" => name.clone()).record(secs);
        #[allow(clippy::cast_precision_loss)]
        metrics::gauge!(names::QUEUE_PUSH_CONSECUTIVE_FAILURES, "subscription" => name.clone()).set(failures as f64);
        if rec.succeeded() {
            #[allow(clippy::cast_precision_loss)]
            metrics::gauge!(names::QUEUE_PUSH_LAST_SUCCESS_SECONDS, "subscription" => name.clone())
                .set(rec.at.div_euclid(1000) as f64);
            self.acked(&sub.name, &res);
        } else if res.count > 0 {
            self.notify();
        }
        if let Some((failures, reason)) = &paused {
            metrics::counter!(names::QUEUE_PUSH_AUTO_PAUSED_TOTAL, "subscription" => name.clone()).increment(1);
            metrics::gauge!(names::QUEUE_PUSH_PAUSED, "subscription" => name.clone()).set(1.0);
            tracing::warn!(
                event = "queue.push.auto_paused",
                subscription = %name,
                consecutive_failures = failures,
                endpoint = %rec.endpoint,
                reason = %reason,
                "push subscription paused after consecutive failed deliveries"
            );
            self.audit(
                &AccessContext::system(),
                "queue.subscription.auto_pause",
                json!({"subscription": bare(&name), "consecutive_failures": failures, "reason": reason}),
            )
            .await;
        }
        Ok(Settled {
            paused: paused.is_some(),
        })
    }

    // ------------------------------------------------------------ test send

    /// Sends a synthetic signed event (`type: "forgebase.test"`, the same
    /// envelope and headers as a delivery, plus `x-forgebase-test: 1`) to a
    /// push subscription's endpoint and returns the attempt. It is not a
    /// message: cursors, deliveries, totals, health and dead letters are
    /// untouched; only the attempt log keeps it (`test: true`). Limited to
    /// `[queue] push_test_sends_per_minute` per subscription.
    pub async fn test_push(&self, ctx: &AccessContext, sub: &str, req: TestSendRequest) -> Result<PushAttempt> {
        self.at_sub(ctx.tenant.as_deref(), sub)
            .await?
            .test_push_here(ctx, sub, req)
            .await
    }

    /// [`Self::test_push`] on this view's set.
    pub(crate) async fn test_push_here(
        &self,
        ctx: &AccessContext,
        sub: &str,
        req: TestSendRequest,
    ) -> Result<PushAttempt> {
        let s = self
            .sub_for(
                &mut Db::Ro(self.backend()),
                ctx,
                Action::QueueAdmin,
                ResourceOp::Update,
                sub,
            )
            .await?;
        let Some(push) = s.push.clone() else {
            return Err(QueueError::conflict(
                "not_push_subscription",
                format!("subscription {sub:?} is a pull subscription; test sends need push"),
            ));
        };
        let data = req.data.unwrap_or_else(|| {
            json!({
                "type": TEST_EVENT_TYPE,
                "message": "A test event from ForgeBase: this endpoint receives the subscription's deliveries.",
            })
        });
        let text = serde_json::to_string(&data).map_err(|e| QueueError::invalid("invalid_message", e.to_string()))?;
        let bytes = i64::try_from(text.len()).unwrap_or(i64::MAX);
        if bytes > self.inner.settings.max_message_bytes {
            return Err(QueueError::TooLarge(format!(
                "the test data is {bytes} bytes; at most {}",
                self.inner.settings.max_message_bytes
            )));
        }
        self.take_test_slot(&s.name)?;
        let now = self.now();
        let id = format!("test-{}", uuid::Uuid::new_v4().simple());
        let message = Message {
            id: id.clone(),
            topic: bare(&s.topic).to_owned(),
            seq: 0,
            data,
            attributes: [("forgebase.test".to_owned(), "true".to_owned())].into_iter().collect(),
            ordering_key: None,
            dedup_id: None,
            publisher: SYSTEM_PUBLISHER.to_owned(),
            bytes,
            published_at: iso(now),
            delivery_history: None,
        };
        let envelope = json!({
            "type": TEST_EVENT_TYPE,
            "subscription": bare(&s.name),
            "delivery_attempt": 1,
            "message": message,
        });
        let rec = self.send_push_as(&s, &push, &envelope, &id, 1, true).await;
        let mut tx = self.begin().await?;
        let r = insert_attempt_tx(&mut *tx, &s.name, &rec).await;
        let row_id = finish(tx, r).await?;
        self.push_attempt_here(&s.name, row_id).await
    }

    /// Takes one test send of `sub` from its per-minute allowance.
    fn take_test_slot(&self, sub: &str) -> Result<()> {
        let now = self.now();
        let limit = usize::try_from(self.inner.settings.push_test_sends_per_minute.max(1)).unwrap_or(usize::MAX);
        let mut map = self
            .inner
            .test_sends
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Forget subscriptions idle for a minute (bounded memory).
        map.retain(|_, sent| sent.back().is_some_and(|t| now - t < 60_000));
        let sent = map.entry(sub.to_owned()).or_default();
        while sent.front().is_some_and(|t| now - t >= 60_000) {
            sent.pop_front();
        }
        if sent.len() >= limit {
            let oldest = sent.front().copied().unwrap_or(now);
            let wait = u64::try_from((oldest + 60_000 - now).max(1000) / 1000).unwrap_or(1);
            return Err(QueueError::RateLimited {
                message: format!(
                    "at most {limit} test sends per minute for subscription {:?}; try again in {wait} s",
                    bare(sub)
                ),
                retry_after_secs: wait,
            });
        }
        sent.push_back(now);
        Ok(())
    }

    /// One stored attempt of the subscription stored as `sub`.
    pub(crate) async fn push_attempt_here(&self, sub: &str, id: i64) -> Result<PushAttempt> {
        let rows = self
            .backend()
            .fetch_all(&Statement::new(
                format!(
                    "SELECT {ATTEMPT_COLS} FROM _forgebase_queue_push_attempts WHERE subscription = ?1 AND id = ?2"
                ),
                vec![sub.into(), id.into()],
            ))
            .await?;
        Row::all(&rows)
            .next()
            .map(|r| attempt_from(&r))
            .transpose()?
            .ok_or_else(|| QueueError::AttemptNotFound(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_verify_and_expire() {
        let h = sign("whsec_x", 1_700_000_000, b"{}");
        assert!(h.starts_with("t=1700000000,v1="));
        assert!(verify("whsec_x", &h, b"{}", 1_700_000_100, 300));
        assert!(!verify("whsec_x", &h, b"{ }", 1_700_000_100, 300));
        assert!(!verify("whsec_y", &h, b"{}", 1_700_000_100, 300));
        assert!(!verify("whsec_x", &h, b"{}", 1_700_001_000, 300));
        assert!(!verify("whsec_x", "t=1700000000", b"{}", 1_700_000_000, 300));
    }

    #[test]
    fn a_rotation_header_verifies_with_either_secret() {
        let h = sign_all(&["whsec_new", "whsec_old"], 1_700_000_000, b"{}");
        assert_eq!(h.matches(",v1=").count(), 2);
        assert!(h.starts_with(&sign("whsec_new", 1_700_000_000, b"{}")), "new first");
        assert!(verify("whsec_new", &h, b"{}", 1_700_000_000, 300));
        assert!(verify("whsec_old", &h, b"{}", 1_700_000_000, 300));
        assert!(!verify("whsec_other", &h, b"{}", 1_700_000_000, 300));
        assert_eq!(masked(&h), "t=1700000000,v1=<redacted>,v1=<redacted>");
    }

    #[test]
    fn fetch_failures_are_classified() {
        let f = |m: &str| classify(&FetchError::Failed(m.into()));
        assert_eq!(f("the request timed out"), PushErrorClass::Timeout);
        assert_eq!(f("could not resolve hooks.test"), PushErrorClass::Dns);
        assert_eq!(
            f("could not connect to hooks.test (ConnectionRefused)"),
            PushErrorClass::Connect
        );
        assert_eq!(
            f("TLS handshake with hooks.test failed: invalid peer certificate"),
            PushErrorClass::Tls
        );
        assert_eq!(f("more than 5 redirects"), PushErrorClass::Redirect);
        assert_eq!(f("malformed response from the remote server"), PushErrorClass::Protocol);
        assert_eq!(
            classify(&FetchError::Blocked("x resolves to 10.0.0.1".into())),
            PushErrorClass::SsrfRefused
        );
        assert_eq!(
            classify(&FetchError::Invalid("bad header".into())),
            PushErrorClass::InvalidRequest
        );
    }

    #[test]
    fn clipping_respects_char_boundaries() {
        assert_eq!(clip("héllo", 2), "h");
        assert_eq!(clip("abc", 10), "abc");
    }
}
