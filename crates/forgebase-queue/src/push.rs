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
//! under the subscription's secret. They go through the SSRF-safe client of
//! FB-60 (every resolved address must be public; no DNS rebinding).

use forgebase_functions::fetch::FetchRequest;
use forgebase_functions::model::Trigger;
use hmac::{Hmac, KeyInit, Mac};
use serde_json::json;
use sha2::Sha256;

use crate::access::bare;
use crate::model::{PushKind, ReceivedMessage};
use crate::service::{QueueService, Result, SubRow};

/// Header carrying the webhook signature.
pub const SIGNATURE_HEADER: &str = "x-forgebase-signature";

/// `t=<ts>,v1=<hex>` for `body` signed with `secret` at `ts` (Unix seconds).
pub fn sign(secret: &str, ts: i64, body: &[u8]) -> String {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(secret.as_bytes())
        .unwrap_or_else(|_| unreachable!("HMAC takes any key length"));
    mac.update(ts.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    let digest = mac.finalize().into_bytes();
    format!("t={ts},v1={}", forgebase_auth::crypto::hex(&digest))
}

/// Checks a signature header against `body` (receivers can use it; tests do),
/// accepting timestamps within `tolerance_secs` of `now_secs`.
pub fn verify(secret: &str, header: &str, body: &[u8], now_secs: i64, tolerance_secs: i64) -> bool {
    let mut ts = None;
    let mut sig = None;
    for part in header.split(',') {
        match part.trim().split_once('=') {
            Some(("t", v)) => ts = v.parse::<i64>().ok(),
            Some(("v1", v)) => sig = Some(v.to_owned()),
            _ => {}
        }
    }
    let (Some(ts), Some(sig)) = (ts, sig) else { return false };
    if (now_secs - ts).abs() > tolerance_secs {
        return false;
    }
    let expected = sign(secret, ts, body);
    forgebase_auth::crypto::ct_eq(expected.as_bytes(), format!("t={ts},v1={sig}").as_bytes())
}

impl QueueService {
    /// One push round over every push subscription. Returns the number of
    /// messages delivered successfully.
    pub async fn deliver_push(&self) -> Result<usize> {
        let rows = self
            .backend()
            .fetch_all(&forgebase_db::Statement::raw(format!(
                "SELECT {} FROM _forgebase_queue_subscriptions WHERE push_kind IS NOT NULL ORDER BY name",
                crate::service::SUB_COLS
            )))
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
        let batch = self
            .claim(&sub.name, self.inner.settings.push_batch, None, "push")
            .await?;
        let mut ok = 0;
        for m in batch {
            let outcome = match push.kind {
                PushKind::Webhook => self.post_webhook(sub, &push.endpoint, &m).await,
                PushKind::Function => self.call_function(sub, &push.endpoint, &m).await,
            };
            match outcome {
                Ok(()) => {
                    self.ack_internal(&sub.name, &m.ack_id).await?;
                    ok += 1;
                }
                Err(why) => {
                    tracing::debug!(subscription = %sub.name, message = %m.message.id, error = %why, "push failed");
                    metrics::counter!("forgebase_queue_push_failures_total", "subscription" => sub.name.clone())
                        .increment(1);
                    self.nack_internal(&sub.name, std::slice::from_ref(&m.ack_id), "failed", Some(&why))
                        .await?;
                }
            }
        }
        Ok(ok)
    }

    async fn ack_internal(&self, sub: &str, ack_id: &str) -> Result<()> {
        self.ack_stored(sub, &[ack_id.to_owned()]).await.map(|_| ())
    }

    fn push_body(sub: &SubRow, m: &ReceivedMessage) -> serde_json::Value {
        json!({
            "subscription": bare(&sub.name),
            "delivery_attempt": m.delivery_attempt,
            "message": m.message,
        })
    }

    async fn post_webhook(&self, sub: &SubRow, url: &str, m: &ReceivedMessage) -> std::result::Result<(), String> {
        let fetcher = self
            .inner
            .fetcher
            .get()
            .ok_or("no push client is configured on this server")?;
        let secret = match &sub.push_secret {
            Some(sealed) => self.open_secret(&sub.name, sealed).map_err(|e| e.to_string())?,
            None => return Err("the subscription has no signing secret".into()),
        };
        let body = serde_json::to_vec(&Self::push_body(sub, m)).map_err(|e| e.to_string())?;
        let ts = self.now().div_euclid(1000);
        let req = FetchRequest {
            method: "POST".into(),
            url: url.to_owned(),
            headers: vec![
                ("content-type".into(), "application/json".into()),
                (SIGNATURE_HEADER.into(), sign(&secret, ts, &body)),
                ("x-forgebase-subscription".into(), bare(&sub.name).to_owned()),
                ("x-forgebase-message-id".into(), m.message.id.clone()),
                ("x-forgebase-delivery-attempt".into(), m.delivery_attempt.to_string()),
            ],
            body,
        };
        let res = tokio::time::timeout(self.inner.settings.push_timeout, fetcher.fetch(req, &[]))
            .await
            .map_err(|_| "the push request timed out".to_owned())?
            .map_err(|e| e.to_string())?;
        if (200..300).contains(&res.status) {
            Ok(())
        } else {
            Err(format!("the endpoint answered HTTP {}", res.status))
        }
    }

    async fn call_function(&self, sub: &SubRow, name: &str, m: &ReceivedMessage) -> std::result::Result<(), String> {
        let functions = self
            .inner
            .functions
            .get()
            .ok_or("server functions are not available on this server")?;
        let input = Self::push_body(sub, m);
        let run = functions.invoke(name, Trigger::Job, &input, Some(format!("queue:{}", m.message.id)));
        tokio::time::timeout(self.inner.settings.push_timeout, run)
            .await
            .map_err(|_| "the function timed out".to_owned())?
            .map(|_| ())
            .map_err(|e| e.to_string())
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
    }
}
