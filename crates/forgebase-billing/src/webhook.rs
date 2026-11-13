//! Provider webhooks: verified, idempotent, replay-protected and durable.
//!
//! 1. The provider driver verifies the signature (constant-time HMAC) and,
//!    for Stripe, the signed timestamp against the tolerance: a forged body
//!    is `400 invalid_signature`, a replayed old delivery `400
//!    stale_signature`.
//! 2. The event id is unique per provider (`_forgebase_billing_webhook_events`):
//!    a redelivery of a processed or ignored event changes nothing and is
//!    answered `duplicate`; a failed one is processed again.
//! 3. Events older than `webhook_max_age_secs` are ignored (`too_old`).
//! 4. Out-of-order delivery: every payment and refund keeps the provider
//!    time of the newest event applied; an older event is ignored (`stale`),
//!    and a succeeded payment never goes back to failed.
//! 5. The state change and the event row commit in one transaction; what
//!    follows (fulfillment, entitlement sync) is idempotent and retried by
//!    the maintenance job.

use std::time::Instant;

use forgebase_data::AccessContext;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::error::Result;
use crate::inspector::{Duplicate, Handled, Incoming, Links, Mode, Verified};
use crate::model::{PaymentStatus, RefundStatus, WebhookEvent, WebhookOutcome};
use crate::money::hex;
use crate::provider::{ChargeResult, EventAction, ProviderEvent, SavedMethod};
use crate::service::{BillingService, Tx, int, new_id, opt_int, opt_text, text};

/// What applying an event did.
struct Applied {
    status: &'static str,
    detail: Option<String>,
    paid_invoice: Option<String>,
    subscription: Option<String>,
    /// Internal objects the event touched (recorded by the inspector).
    links: Links,
}

impl Applied {
    fn ignored(detail: &str) -> Self {
        Self {
            status: "ignored",
            detail: Some(detail.to_owned()),
            paid_invoice: None,
            subscription: None,
            links: Links::default(),
        }
    }

    fn processed(detail: Option<&str>) -> Self {
        Self {
            status: "processed",
            detail: detail.map(str::to_owned),
            paid_invoice: None,
            subscription: None,
            links: Links::default(),
        }
    }

    fn with(mut self, links: Links) -> Self {
        self.links = links;
        self
    }
}

/// A prior row of `_forgebase_billing_webhook_events`.
struct Prior {
    id: String,
    status: String,
    hash: String,
}

/// The dedup row of an event.
async fn prior_event(t: &mut Tx<'_>, provider: &str, event_id: &str) -> Result<Option<Prior>> {
    let rows = t
        .fetch(
            "SELECT id, status, payload_hash FROM _forgebase_billing_webhook_events WHERE provider = ?1 AND event_id = ?2",
            vec![provider.into(), event_id.into()],
        )
        .await?;
    Ok(rows.rows.first().map(|r| Prior {
        id: text(r, 0),
        status: text(r, 1),
        hash: text(r, 2),
    }))
}

/// Inside the write transaction: what the event-processing step found.
enum Step {
    /// Another delivery of the event was handled first.
    Duplicate(Prior),
    /// Applied (or ignored) and recorded under this event row id.
    Applied(Box<Applied>, String),
}

impl BillingService {
    /// Handles one webhook delivery of `provider` (anonymous: the signature
    /// is the authentication). Every delivery is recorded by the webhook
    /// inspector (ADR-0032), which never changes the answer.
    pub async fn handle_webhook(
        &self,
        provider: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> Result<WebhookOutcome> {
        self.receive_webhook(provider, headers, body, None).await
    }

    /// [`Self::handle_webhook`] with the client address the HTTP layer saw
    /// (recorded with the delivery).
    pub async fn receive_webhook(
        &self,
        provider: &str,
        headers: &[(String, String)],
        body: &[u8],
        client_addr: Option<String>,
    ) -> Result<WebhookOutcome> {
        let received_at = self.now();
        let started = Instant::now();
        let handled = self
            .process_delivery(provider, headers, body, received_at, Mode::Live)
            .await;
        let incoming = Incoming {
            provider,
            headers,
            body,
            client_addr,
            received_at,
        };
        self.log_delivery(&incoming, &handled, started.elapsed()).await;
        handled.outcome
    }

    /// Verifies and processes one delivery. `verify_at` is the time the
    /// signature timestamp and the replay window are checked against: now for
    /// a live delivery, the time it was received for a stored one.
    pub(crate) async fn process_delivery(
        &self,
        provider: &str,
        headers: &[(String, String)],
        body: &[u8],
        verify_at: i64,
        mode: Mode,
    ) -> Handled {
        // App Store, Google Play and Creem notifications (FB-72).
        if let Some(store) = self.store(provider) {
            return self
                .handle_store_notification(store, headers, body, verify_at, mode)
                .await;
        }
        let driver = match self.provider(provider) {
            Ok(d) => d,
            Err(e) => return Handled::unverified(e, None),
        };
        let account = driver.account();
        let settings = match self.settings().await {
            Ok(s) => s,
            Err(e) => return Handled::unverified(e, account),
        };
        let ev = match driver.verify_webhook(headers, body, verify_at, settings.webhook_tolerance_secs) {
            Ok(ev) => ev,
            Err(e) => return Handled::unverified(e, account),
        };
        let delivery = Verified::new(account, &ev.id, &ev.event_type);
        let hash = hex(&Sha256::digest(body));
        match self.replayed(provider, &ev.id, &hash, mode == Mode::Live).await {
            Ok(Some(dup)) => return delivery.duplicate(dup),
            Ok(None) => {}
            Err(e) => return delivery.failed(e, None),
        }
        let now = self.now();
        let too_old = ev.created_at < verify_at - settings.webhook_max_age_secs * 1000;
        let mut tx = match self.begin().await {
            Ok(tx) => tx,
            Err(e) => return delivery.failed(e, None),
        };
        let res = async {
            let mut t = Tx(tx.as_mut());
            // Dedup again inside the write transaction: writes are serialised,
            // so two deliveries of one event that both passed the check above
            // (a provider retry racing an operator re-verification) apply once.
            if let Some(p) = prior_event(&mut t, provider, &ev.id).await?
                && p.status != "failed"
            {
                return Ok((Step::Duplicate(p), false));
            }
            let applied = if too_old {
                Applied::ignored("too_old")
            } else {
                self.apply_event(&mut t, provider, &ev, now).await?
            };
            let id = record(
                &mut t,
                provider,
                &ev,
                &hash,
                applied.status,
                applied.detail.as_deref(),
                now,
            )
            .await?;
            Ok((Step::Applied(Box::new(applied), id), true))
        }
        .await;
        let (applied, event_row) = match self.end(tx, res).await {
            Ok(Step::Applied(a, id)) => (*a, id),
            Ok(Step::Duplicate(p)) => {
                return delivery.duplicate(Duplicate {
                    outcome: WebhookOutcome {
                        result: "duplicate".into(),
                        detail: (p.hash != hash).then(|| "payload_differs".to_owned()),
                        event_id: ev.id.clone(),
                    },
                    prior: p.status,
                    event_row: p.id,
                });
            }
            Err(e) => {
                // Durable failure record; the provider's redelivery retries it.
                let msg = e.to_string();
                let failed = async {
                    let mut tx = self.begin().await?;
                    let res = async {
                        let mut t = Tx(tx.as_mut());
                        let id = record(&mut t, provider, &ev, &hash, "failed", Some(&msg), now).await?;
                        Ok((id, true))
                    }
                    .await;
                    self.end(tx, res).await
                }
                .await;
                return match failed {
                    Ok(id) => delivery.failed(e, Some(id)),
                    Err(e2) => delivery.failed(e2, None),
                };
            }
        };
        if let Some(inv) = &applied.paid_invoice {
            self.after_paid(inv).await;
        }
        if let Some(s) = &applied.subscription {
            self.sync_logged(s).await;
        }
        let mut links = applied.links;
        links.webhook_event = Some(event_row);
        delivery.done(applied.status, applied.detail, links)
    }

    /// A delivery of an event already processed or ignored: counted (when
    /// `count`) and answered `duplicate` without effect (a failed one is
    /// processed again).
    pub(crate) async fn replayed(
        &self,
        provider: &str,
        event_id: &str,
        hash: &str,
        count: bool,
    ) -> Result<Option<Duplicate>> {
        let prior = self
            .fetch(
                "SELECT id, status, payload_hash FROM _forgebase_billing_webhook_events WHERE provider = ?1 AND event_id = ?2",
                vec![provider.into(), event_id.into()],
            )
            .await?;
        let Some(r) = prior.rows.first().filter(|r| text(r, 1) != "failed") else {
            return Ok(None);
        };
        if count {
            let mut tx = self.begin().await?;
            let res = async {
                let mut t = Tx(tx.as_mut());
                t.exec(
                    "UPDATE _forgebase_billing_webhook_events SET attempts = attempts + 1 WHERE provider = ?1 AND event_id = ?2",
                    vec![provider.into(), event_id.into()],
                )
                .await?;
                Ok(((), true))
            }
            .await;
            self.end(tx, res).await?;
        }
        Ok(Some(Duplicate {
            outcome: WebhookOutcome {
                result: "duplicate".into(),
                detail: (text(r, 2) != hash).then(|| "payload_differs".to_owned()),
                event_id: event_id.to_owned(),
            },
            prior: text(r, 1),
            event_row: text(r, 0),
        }))
    }

    async fn apply_event(&self, t: &mut Tx<'_>, provider: &str, ev: &ProviderEvent, now: i64) -> Result<Applied> {
        match &ev.action {
            EventAction::PaymentSucceeded {
                invoice_id,
                payment_ref,
                amount,
                currency,
                provider_customer,
                method,
            } => {
                self.on_payment_succeeded(
                    t,
                    provider,
                    ev,
                    invoice_id.as_deref(),
                    payment_ref,
                    *amount,
                    currency,
                    provider_customer.as_deref(),
                    method.as_ref(),
                    now,
                )
                .await
            }
            EventAction::PaymentFailed {
                invoice_id,
                payment_ref,
                code,
                message,
            } => {
                self.on_payment_failed(
                    t,
                    provider,
                    ev,
                    invoice_id.as_deref(),
                    payment_ref,
                    code.clone(),
                    message.clone(),
                    now,
                )
                .await
            }
            EventAction::RefundUpdated {
                refund_ref,
                payment_ref,
                status,
            } => {
                self.on_refund(t, provider, ev, refund_ref, payment_ref.as_deref(), *status, now)
                    .await
            }
            EventAction::PaymentMethodSaved {
                customer_id,
                provider_customer,
                method,
            } => {
                let rows = t
                    .fetch(
                        "SELECT id FROM _forgebase_billing_customers
                         WHERE (CAST(?1 AS TEXT) IS NOT NULL AND id = ?1) OR (CAST(?2 AS TEXT) IS NOT NULL AND provider = ?3 AND provider_customer_id = ?2)
                         LIMIT 1",
                        vec![customer_id.clone().into(), provider_customer.clone().into(), provider.into()],
                    )
                    .await?;
                let Some(r) = rows.rows.first() else {
                    return Ok(Applied::ignored("unknown_customer"));
                };
                save_method(t, &text(r, 0), provider, provider_customer.as_deref(), method, now).await?;
                Ok(Applied::processed(Some("payment_method_saved")).with(Links {
                    customer: Some(text(r, 0)),
                    ..Links::default()
                }))
            }
            EventAction::Other => Ok(Applied::ignored("unhandled_type")),
        }
    }

    /// The payment of an event: by provider reference, else the invoice's
    /// pending attempt.
    async fn find_payment(
        &self,
        t: &mut Tx<'_>,
        provider: &str,
        invoice_id: Option<&str>,
        payment_ref: &str,
    ) -> Result<Option<(String, String, String, Option<i64>)>> {
        let rows = t
            .fetch(
                "SELECT id, invoice_id, status, provider_event_at FROM _forgebase_billing_payments
                 WHERE provider = ?1 AND provider_ref = ?2",
                vec![provider.into(), payment_ref.into()],
            )
            .await?;
        if let Some(r) = rows.rows.first() {
            return Ok(Some((text(r, 0), text(r, 1), text(r, 2), opt_int(r, 3))));
        }
        let Some(inv) = invoice_id else { return Ok(None) };
        let rows = t
            .fetch(
                "SELECT id, invoice_id, status, provider_event_at FROM _forgebase_billing_payments
                 WHERE provider = ?1 AND invoice_id = ?2 AND status = 'pending' AND provider_ref IS NULL
                 ORDER BY attempt DESC LIMIT 1",
                vec![provider.into(), inv.into()],
            )
            .await?;
        Ok(rows
            .rows
            .first()
            .map(|r| (text(r, 0), text(r, 1), text(r, 2), opt_int(r, 3))))
    }

    #[allow(clippy::too_many_arguments)]
    async fn on_payment_succeeded(
        &self,
        t: &mut Tx<'_>,
        provider: &str,
        ev: &ProviderEvent,
        invoice_id: Option<&str>,
        payment_ref: &str,
        amount: i64,
        currency: &str,
        provider_customer: Option<&str>,
        method: Option<&SavedMethod>,
        now: i64,
    ) -> Result<Applied> {
        let found = self.find_payment(t, provider, invoice_id, payment_ref).await?;
        let (payment_id, inv_id) = match found {
            Some((id, inv, status, last)) => {
                let links = Links {
                    payment: Some(id.clone()),
                    invoice: Some(inv.clone()),
                    ..Links::default()
                };
                if last.is_some_and(|l| ev.created_at < l) {
                    return Ok(Applied::ignored("stale").with(links));
                }
                if status == "succeeded" {
                    return Ok(Applied::ignored("already_succeeded").with(links));
                }
                (id, inv)
            }
            None => {
                let Some(inv_id) = invoice_id else {
                    return Ok(Applied::ignored("unknown_payment"));
                };
                let rows = t
                    .fetch(
                        "SELECT customer_id, subject, attempt_count FROM _forgebase_billing_invoices WHERE id = ?1",
                        vec![inv_id.into()],
                    )
                    .await?;
                let Some(r) = rows.rows.first() else {
                    return Ok(Applied::ignored("unknown_invoice"));
                };
                // Paid on the hosted page: a new payment row.
                let id = new_id("pay");
                t.exec(
                    "INSERT INTO _forgebase_billing_payments (id, invoice_id, customer_id, subject, provider, provider_ref,
                         status, amount, currency, attempt, idempotency_key, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7, ?8, ?9, ?10, ?11, ?11)",
                    vec![
                        id.clone().into(),
                        inv_id.into(),
                        text(r, 0).into(),
                        text(r, 1).into(),
                        provider.into(),
                        payment_ref.into(),
                        amount.max(0).into(),
                        currency.into(),
                        (int(r, 2) + 1).into(),
                        format!("webhook:{provider}:{payment_ref}").into(),
                        now.into(),
                    ],
                )
                .await?;
                (id, inv_id.to_owned())
            }
        };
        let inv = t
            .fetch(
                "SELECT currency, customer_id, subscription_id, status FROM _forgebase_billing_invoices WHERE id = ?1",
                vec![inv_id.clone().into()],
            )
            .await?;
        let Some(ir) = inv.rows.first() else {
            return Ok(Applied::ignored("unknown_invoice"));
        };
        if !currency.is_empty() && text(ir, 0) != currency {
            return Ok(Applied::ignored("currency_mismatch"));
        }
        let (customer_id, subscription) = (text(ir, 1), opt_text(ir, 2));
        let links = Links {
            customer: Some(customer_id.clone()),
            subscription: subscription.clone(),
            invoice: Some(inv_id.clone()),
            payment: Some(payment_id.clone()),
            ..Links::default()
        };
        let was_open = matches!(text(ir, 3).as_str(), "open" | "uncollectible");
        let paid = self
            .apply_charge_tx(
                t,
                &payment_id,
                &ChargeResult {
                    status: PaymentStatus::Succeeded,
                    provider_ref: Some(payment_ref.to_owned()),
                    failure_code: None,
                    failure_message: None,
                },
                Some(ev.created_at),
                now,
            )
            .await?;
        if let Some(m) = method {
            save_method(t, &customer_id, provider, provider_customer, m, now).await?;
        }
        if !paid && !was_open {
            t.audit(
                "system",
                "payment.duplicate",
                "invoice",
                &inv_id,
                json!({"payment": payment_id, "provider_ref": payment_ref}),
                now,
            )
            .await?;
            return Ok(Applied {
                status: "processed",
                detail: Some("invoice_already_paid".into()),
                paid_invoice: None,
                subscription: None,
                links,
            });
        }
        Ok(Applied {
            status: "processed",
            detail: Some("invoice_paid".into()),
            paid_invoice: paid.then_some(inv_id),
            subscription,
            links,
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn on_payment_failed(
        &self,
        t: &mut Tx<'_>,
        provider: &str,
        ev: &ProviderEvent,
        invoice_id: Option<&str>,
        payment_ref: &str,
        code: Option<String>,
        message: Option<String>,
        now: i64,
    ) -> Result<Applied> {
        let Some((payment_id, inv_id, status, last)) = self.find_payment(t, provider, invoice_id, payment_ref).await?
        else {
            return Ok(Applied::ignored("unknown_payment"));
        };
        let links = Links {
            payment: Some(payment_id.clone()),
            invoice: Some(inv_id.clone()),
            ..Links::default()
        };
        if last.is_some_and(|l| ev.created_at < l) {
            return Ok(Applied::ignored("stale").with(links));
        }
        if status == "succeeded" {
            return Ok(Applied::ignored("already_succeeded").with(links));
        }
        if status == "failed" {
            return Ok(Applied::ignored("already_failed").with(links));
        }
        let sub = t
            .fetch(
                "SELECT subscription_id FROM _forgebase_billing_invoices WHERE id = ?1",
                vec![inv_id.into()],
            )
            .await?
            .rows
            .first()
            .and_then(|r| opt_text(r, 0));
        self.apply_charge_tx(
            t,
            &payment_id,
            &ChargeResult {
                status: PaymentStatus::Failed,
                provider_ref: Some(payment_ref.to_owned()),
                failure_code: code.or_else(|| Some("payment_failed".into())),
                failure_message: message,
            },
            Some(ev.created_at),
            now,
        )
        .await?;
        Ok(Applied {
            status: "processed",
            detail: Some("payment_failed".into()),
            paid_invoice: None,
            subscription: sub.clone(),
            links: Links {
                subscription: sub,
                ..links
            },
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn on_refund(
        &self,
        t: &mut Tx<'_>,
        provider: &str,
        ev: &ProviderEvent,
        refund_ref: &str,
        payment_ref: Option<&str>,
        status: RefundStatus,
        now: i64,
    ) -> Result<Applied> {
        let rows = t
            .fetch(
                "SELECT id FROM _forgebase_billing_refunds WHERE provider = ?1 AND provider_ref = ?2",
                vec![provider.into(), refund_ref.into()],
            )
            .await?;
        let id = match rows.rows.first() {
            Some(r) => Some(text(r, 0)),
            None => match payment_ref {
                // The webhook can arrive before the refund call returned.
                Some(p) => t
                    .fetch(
                        "SELECT r.id FROM _forgebase_billing_refunds r JOIN _forgebase_billing_payments p ON p.id = r.payment_id
                         WHERE r.provider = ?1 AND p.provider_ref = ?2 AND r.provider_ref IS NULL AND r.status = 'pending'
                         ORDER BY r.created_at LIMIT 1",
                        vec![provider.into(), p.into()],
                    )
                    .await?
                    .rows
                    .first()
                    .map(|r| text(r, 0)),
                None => None,
            },
        };
        let Some(id) = id else {
            return Ok(Applied::ignored("unknown_refund"));
        };
        let links = t
            .fetch(
                "SELECT payment_id, invoice_id FROM _forgebase_billing_refunds WHERE id = ?1",
                vec![id.clone().into()],
            )
            .await?
            .rows
            .first()
            .map(|r| Links {
                refund: Some(id.clone()),
                payment: opt_text(r, 0),
                invoice: opt_text(r, 1),
                ..Links::default()
            })
            .unwrap_or_default();
        if status == RefundStatus::Pending {
            t.exec(
                "UPDATE _forgebase_billing_refunds SET provider_ref = COALESCE(provider_ref, ?2) WHERE id = ?1",
                vec![id.into(), refund_ref.into()],
            )
            .await?;
            return Ok(Applied::processed(Some("refund_pending")).with(links));
        }
        if self
            .set_refund_tx(t, &id, status, Some(refund_ref.to_owned()), Some(ev.created_at), now)
            .await?
        {
            Ok(Applied::processed(Some(match status {
                RefundStatus::Succeeded => "refund_succeeded",
                _ => "refund_failed",
            }))
            .with(links))
        } else {
            Ok(Applied::ignored("stale").with(links))
        }
    }

    /// Webhook events received (platform administrators), newest first.
    pub async fn webhook_events(&self, ctx: &AccessContext, limit: Option<u32>) -> Result<Vec<WebhookEvent>> {
        self.require_admin(ctx)?;
        let limit = i64::from(limit.unwrap_or(100).clamp(1, 1000));
        let rows = self
            .fetch(
                "SELECT id, provider, event_id, event_type, event_at, received_at, status, result, attempts
                 FROM _forgebase_billing_webhook_events ORDER BY received_at DESC, id DESC LIMIT ?1",
                vec![limit.into()],
            )
            .await?;
        Ok(rows
            .rows
            .iter()
            .map(|r| WebhookEvent {
                id: text(r, 0),
                provider: text(r, 1),
                event_id: text(r, 2),
                event_type: text(r, 3),
                event_at: int(r, 4),
                received_at: int(r, 5),
                status: text(r, 6),
                result: opt_text(r, 7),
                attempts: int(r, 8),
            })
            .collect())
    }
}

async fn save_method(
    t: &mut Tx<'_>,
    customer_id: &str,
    provider: &str,
    provider_customer: Option<&str>,
    m: &SavedMethod,
    now: i64,
) -> Result<()> {
    t.exec(
        "UPDATE _forgebase_billing_customers SET payment_method = ?2, payment_method_label = ?3, provider = ?4,
             provider_customer_id = COALESCE(provider_customer_id, ?5), updated_at = ?6
         WHERE id = ?1",
        vec![
            customer_id.into(),
            m.token.clone().into(),
            m.label.clone().into(),
            provider.into(),
            provider_customer.map(str::to_owned).into(),
            now.into(),
        ],
    )
    .await?;
    Ok(())
}

pub(crate) async fn record(
    t: &mut Tx<'_>,
    provider: &str,
    ev: &ProviderEvent,
    hash: &str,
    status: &str,
    detail: Option<&str>,
    now: i64,
) -> Result<String> {
    let rows = t
        .fetch(
            "INSERT INTO _forgebase_billing_webhook_events (id, provider, event_id, event_type, event_at, received_at,
                 payload_hash, status, result, processed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?6)
             ON CONFLICT (provider, event_id) DO UPDATE SET status = excluded.status, result = excluded.result,
                 attempts = _forgebase_billing_webhook_events.attempts + 1, processed_at = excluded.processed_at
             RETURNING id",
            vec![
                new_id("whe").into(),
                provider.into(),
                ev.id.clone().into(),
                ev.event_type.clone().into(),
                ev.created_at.into(),
                now.into(),
                hash.into(),
                status.into(),
                detail.map(str::to_owned).into(),
            ],
        )
        .await?;
    Ok(rows.rows.first().map(|r| text(r, 0)).unwrap_or_default())
}
