//! Webhook delivery tooling of push subscriptions ([ADR-0031](https://github.com/farzanshibu/forgebase/blob/main/docs/adr/0031-webhook-tooling.md)):
//! the attempt log, test sends, dead-letter redrive, retry now, signing-secret rotation, pause and
//! resume. `client.queues().webhooks("orders-push")`.
//!
//! Reads need `engine:queue_read`, everything else `engine:queue_admin`, on the subscription (its
//! creator holds both). Pull subscriptions answer `409 not_push_subscription` to the calls that
//! need a push target.

use std::sync::Arc;

use serde_json::{Value, json};

use crate::error::Result;
use crate::generated::{
    PushAttempt, PushAttemptPage, PushOutcome, RedriveRequest, RedriveResult, RetryResult, RotatedSecret,
};
use crate::http::{Req, Transport, seg, wire_str};
use crate::queues::QueueSubscription;

/// Filters of [`QueueWebhooksApi::attempts`].
#[derive(Debug, Clone, Default)]
pub struct AttemptsQuery {
    /// Only `success` or only `failure` attempts.
    pub outcome: Option<PushOutcome>,
    /// Only the attempts of one message.
    pub message_id: Option<String>,
    /// Only attempts at or after this time (RFC 3339).
    pub since: Option<String>,
    /// Only attempts before this time (RFC 3339).
    pub until: Option<String>,
    /// `Some(true)`: test sends only; `Some(false)`: real deliveries only.
    pub test: Option<bool>,
    /// Page cursor: the `next_before` of the previous page.
    pub before: Option<i64>,
    /// Page size.
    pub limit: Option<u32>,
}

/// Which dead letters to redrive.
#[derive(Debug, Clone)]
pub enum Redrive {
    /// These dead-letter record ids (1-1000).
    Ids(Vec<i64>),
    /// Every dead letter not redriven yet, oldest first.
    All {
        /// Only records dead-lettered at or after this RFC 3339 time.
        since: Option<String>,
        /// Only records whose message attributes match this filter (subscription filter syntax).
        filter: Option<Value>,
        /// Most records to redrive in this call (1-1000, default 1000).
        limit: Option<i64>,
    },
}

impl Redrive {
    /// Every dead letter not redriven yet.
    #[must_use]
    pub fn all() -> Self {
        Self::All {
            since: None,
            filter: None,
            limit: None,
        }
    }
}

/// Webhook tooling of one push subscription.
#[derive(Clone)]
pub struct QueueWebhooksApi {
    t: Arc<Transport>,
    base: String,
}

impl QueueWebhooksApi {
    pub(crate) fn new(t: Arc<Transport>, subscription: &str) -> Self {
        Self {
            t,
            base: format!("/api/v1/queues/subscriptions/{}", seg(subscription)),
        }
    }

    /// Push attempts, newest first: deliveries and test sends with status, latency, error class,
    /// the request headers sent (signature masked), allowlisted response headers and the response
    /// body (cut to the server's limit; see `response_truncated`).
    pub async fn attempts(&self, q: &AttemptsQuery) -> Result<PushAttemptPage> {
        self.t
            .json(
                Req::get(format!("{}/attempts", self.base))
                    .q_opt("outcome", q.outcome.as_ref().and_then(wire_str))
                    .q_opt("message_id", q.message_id.as_deref())
                    .q_opt("since", q.since.as_deref())
                    .q_opt("until", q.until.as_deref())
                    .q_opt("test", q.test)
                    .q_opt("before", q.before)
                    .q_opt("limit", q.limit),
            )
            .await
    }

    /// One attempt (`404 attempt_not_found`).
    pub async fn attempt(&self, id: i64) -> Result<PushAttempt> {
        self.t.json(Req::get(format!("{}/attempts/{id}", self.base))).await
    }

    /// Sends a synthetic signed event (`"type": "forgebase.test"` in the usual envelope, header
    /// `X-Forgebase-Test: 1`) and returns the attempt. Not a message: cursors, totals, health and
    /// dead letters are untouched. Rate limited per subscription (`429 rate_limited`). `data`
    /// replaces the default payload.
    pub async fn test(&self, data: Option<&Value>) -> Result<PushAttempt> {
        let body = match data {
            Some(d) => json!({"data": d}),
            None => json!({}),
        };
        self.t
            .json(Req::post(format!("{}/test", self.base)).json_raw(body))
            .await
    }

    /// Delivers dead letters to the subscription again with a fresh attempt counter. Each record
    /// is redriven once; the result lists `redriven`, `already_redriven`, `not_found` and `remaining`.
    pub async fn redrive(&self, what: &Redrive) -> Result<RedriveResult> {
        let body = match what {
            Redrive::Ids(ids) => RedriveRequest {
                ids: Some(ids.clone()),
                ..RedriveRequest::default()
            },
            Redrive::All { since, filter, limit } => RedriveRequest {
                all: Some(true),
                since: since.clone(),
                filter: filter.clone(),
                limit: *limit,
                ..RedriveRequest::default()
            },
        };
        self.t
            .json(Req::post(format!("{}/redrive", self.base)).json(&body)?)
            .await
    }

    /// Makes deliveries that wait for a retry backoff ready now (all, or only `message_ids`).
    pub async fn retry(&self, message_ids: Option<&[&str]>) -> Result<RetryResult> {
        let body = match message_ids {
            Some(ids) => json!({"message_ids": ids}),
            None => json!({}),
        };
        self.t
            .json(Req::post(format!("{}/retry", self.base)).json_raw(body))
            .await
    }

    /// Rotates the signing secret and returns the new `push_secret` once. During the grace window
    /// (`grace_ms`: default 24 h, at most 7 days) `X-Forgebase-Signature` carries both signatures,
    /// new first; receivers accept the request when any `v1` matches
    /// ([`webhooks::verify`](crate::webhooks::verify) does). `409 not_webhook_subscription` for functions.
    pub async fn rotate_secret(&self, grace_ms: Option<i64>) -> Result<RotatedSecret> {
        self.t
            .json(Req::post(format!("{}/secret/rotate", self.base)).json_value(json!({"grace_ms": grace_ms})))
            .await
    }

    /// Ends the grace window of a rotation now.
    pub async fn expire_previous_secret(&self) -> Result<QueueSubscription> {
        self.t
            .json(Req::post(format!("{}/secret/expire-previous", self.base)).json_raw(json!({})))
            .await
    }

    /// Pauses delivery. The subscription keeps accumulating messages; none is dead-lettered while paused.
    pub async fn pause(&self, reason: Option<&str>) -> Result<QueueSubscription> {
        self.t
            .json(Req::post(format!("{}/pause", self.base)).json_value(json!({"reason": reason})))
            .await
    }

    /// Resumes delivery: every delayed delivery becomes ready and the backlog is delivered in order.
    pub async fn resume(&self) -> Result<QueueSubscription> {
        self.t
            .json(Req::post(format!("{}/resume", self.base)).json_raw(json!({})))
            .await
    }
}
