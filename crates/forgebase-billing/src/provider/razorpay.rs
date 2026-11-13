//! Razorpay driver (test mode). REST calls are JSON with HTTP Basic auth
//! (`rzp_test_…` key id and key secret); webhooks carry
//! `X-Razorpay-Signature` = hex HMAC-SHA256 of the raw body under the webhook
//! secret and `X-Razorpay-Event-Id`. Razorpay signatures have no timestamp,
//! so replay protection is the stored event id plus the event's `created_at`
//! age limit (settings `webhook_max_age_secs`).

use std::sync::Arc;

use base64::Engine as _;
use forgebase_auth::social::http_client::{HttpClient, HttpRequest};
use forgebase_db::backend::BoxFuture;
use serde_json::{Value as Json, json};

use super::{
    ChargeRequest, ChargeResult, CustomerRef, EventAction, HostedPaymentRequest, HostedSetupRequest, PaymentProvider,
    ProviderEvent, RefundRequest, RefundResult, SavedMethod, header, hmac_hex, int_at, json_body, parse_ok,
    same_signature, str_at,
};
use crate::error::{BillingError, Result};
use crate::model::{HostedPage, PaymentStatus, RefundStatus};

/// Default API base.
pub const API_BASE: &str = "https://api.razorpay.com";

/// Razorpay in test mode.
#[derive(Clone)]
pub struct RazorpayProvider {
    key_id: String,
    key_secret: String,
    webhook_secret: String,
    api_base: String,
    http: Arc<dyn HttpClient>,
}

impl std::fmt::Debug for RazorpayProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RazorpayProvider")
            .field("key_id", &self.key_id)
            .field("api_base", &self.api_base)
            .finish_non_exhaustive()
    }
}

/// Refuses live and malformed Razorpay keys.
pub fn check_keys(key_id: &str, key_secret: &str, webhook_secret: &str) -> Result<()> {
    if key_id.starts_with("rzp_live_") {
        return Err(BillingError::invalid(
            "live_mode_refused",
            "only Razorpay test-mode keys (rzp_test_…) are accepted",
        ));
    }
    if !key_id.starts_with("rzp_test_") || key_id.len() < 12 {
        return Err(BillingError::invalid(
            "invalid_key",
            "the Razorpay key id must start with rzp_test_",
        ));
    }
    if key_secret.len() < 8 || webhook_secret.len() < 8 {
        return Err(BillingError::invalid(
            "invalid_key",
            "the Razorpay key secret and webhook secret need at least 8 characters",
        ));
    }
    Ok(())
}

impl RazorpayProvider {
    /// A driver for test-mode keys only.
    pub fn new(
        key_id: &str,
        key_secret: &str,
        webhook_secret: &str,
        api_base: Option<&str>,
        http: Arc<dyn HttpClient>,
    ) -> Result<Self> {
        check_keys(key_id, key_secret, webhook_secret)?;
        Ok(Self {
            key_id: key_id.to_owned(),
            key_secret: key_secret.to_owned(),
            webhook_secret: webhook_secret.to_owned(),
            api_base: api_base.unwrap_or(API_BASE).trim_end_matches('/').to_owned(),
            http,
        })
    }

    fn request(&self, path: &str, body: &Json, idem: &str) -> HttpRequest {
        let auth = base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", self.key_id, self.key_secret));
        HttpRequest {
            method: "POST",
            url: format!("{}{path}", self.api_base),
            headers: vec![
                ("content-type".into(), "application/json".into()),
                ("authorization".into(), format!("Basic {auth}")),
                ("x-razorpay-idempotency-key".into(), idem.to_owned()),
            ],
            body: body.to_string().into_bytes(),
        }
    }

    async fn post(&self, path: &str, body: &Json, idem: &str) -> Result<Json> {
        let res = self.http.send(self.request(path, body, idem)).await?;
        parse_ok(&res)
    }
}

/// Razorpay `receipt` and `reference_id` are limited to 40 characters.
fn receipt(s: &str) -> String {
    s.chars().take(40).collect()
}

fn refund_status(s: &str) -> RefundStatus {
    match s {
        "processed" => RefundStatus::Succeeded,
        "failed" => RefundStatus::Failed,
        _ => RefundStatus::Pending,
    }
}

impl PaymentProvider for RazorpayProvider {
    fn name(&self) -> &'static str {
        "razorpay"
    }

    fn account(&self) -> Option<String> {
        Some(self.key_id.clone()).filter(|a| !a.is_empty())
    }

    fn create_customer<'a>(&'a self, c: &'a CustomerRef) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let body = json!({
                "name": c.name.clone().unwrap_or_else(|| c.subject.clone()),
                "email": c.email,
                "fail_existing": "0",
                "notes": {"forgebase_customer": c.customer_id, "forgebase_subject": c.subject},
            });
            let v = self
                .post("/v1/customers", &body, &format!("customer:{}", c.customer_id))
                .await?;
            str_at(&v, &["id"])
                .map(str::to_owned)
                .ok_or_else(|| BillingError::Provider("Razorpay returned no customer id".into()))
        })
    }

    fn charge<'a>(&'a self, r: &'a ChargeRequest) -> BoxFuture<'a, Result<ChargeResult>> {
        Box::pin(async move {
            // A recurring payment needs an order first.
            let order = self
                .post(
                    "/v1/orders",
                    &json!({
                        "amount": r.amount,
                        "currency": r.currency,
                        "receipt": receipt(&r.invoice_number),
                        "payment_capture": 1,
                        "notes": {"forgebase_invoice": r.invoice_id},
                    }),
                    &format!("{}:order", r.idempotency_key),
                )
                .await?;
            let Some(order_id) = str_at(&order, &["id"]) else {
                return Err(BillingError::Provider("Razorpay returned no order id".into()));
            };
            let res = self
                .http
                .send(self.request(
                    "/v1/payments/create/recurring",
                    &json!({
                        "email": r.email,
                        "amount": r.amount,
                        "currency": r.currency,
                        "order_id": order_id,
                        "customer_id": r.provider_customer_id,
                        "token": r.payment_method,
                        "recurring": "1",
                        "description": r.description,
                        "notes": {"forgebase_invoice": r.invoice_id},
                    }),
                    &r.idempotency_key,
                ))
                .await?;
            let v: Json = serde_json::from_slice(&res.body).unwrap_or(Json::Null);
            if (400..500).contains(&res.status) {
                return Ok(ChargeResult::failed(
                    str_at(&v, &["error", "code"]).unwrap_or("payment_failed"),
                    str_at(&v, &["error", "description"]).unwrap_or("declined"),
                ));
            }
            let v = parse_ok(&res)?;
            // Recurring payments are confirmed by the payment.captured webhook.
            Ok(ChargeResult {
                status: PaymentStatus::Pending,
                provider_ref: str_at(&v, &["razorpay_payment_id"]).map(str::to_owned),
                failure_code: None,
                failure_message: None,
            })
        })
    }

    fn refund<'a>(&'a self, r: &'a RefundRequest) -> BoxFuture<'a, Result<RefundResult>> {
        Box::pin(async move {
            let v = self
                .post(
                    &format!("/v1/payments/{}/refund", r.payment_ref),
                    &json!({
                        "amount": r.amount,
                        "speed": "normal",
                        "receipt": receipt(&r.idempotency_key),
                        "notes": {"reason": r.reason},
                    }),
                    &r.idempotency_key,
                )
                .await?;
            Ok(RefundResult {
                provider_ref: str_at(&v, &["id"]).map(str::to_owned),
                status: refund_status(str_at(&v, &["status"]).unwrap_or("pending")),
            })
        })
    }

    fn hosted_payment<'a>(&'a self, r: &'a HostedPaymentRequest) -> BoxFuture<'a, Result<HostedPage>> {
        Box::pin(async move {
            let v = self
                .post(
                    "/v1/payment_links",
                    &json!({
                        "amount": r.amount,
                        "currency": r.currency,
                        "description": r.description,
                        "reference_id": receipt(&r.idempotency_key),
                        "customer": {"name": r.customer.name, "email": r.customer.email},
                        "callback_url": r.return_url,
                        "callback_method": "get",
                        "notes": {"forgebase_invoice": r.invoice_id},
                    }),
                    &r.idempotency_key,
                )
                .await?;
            match (str_at(&v, &["short_url"]), str_at(&v, &["id"])) {
                (Some(url), Some(id)) => Ok(HostedPage {
                    url: url.to_owned(),
                    session_id: id.to_owned(),
                }),
                _ => Err(BillingError::Provider("Razorpay returned no payment link".into())),
            }
        })
    }

    fn hosted_setup<'a>(&'a self, r: &'a HostedSetupRequest) -> BoxFuture<'a, Result<HostedPage>> {
        Box::pin(async move {
            let v = self
                .post(
                    "/v1/subscription_registration/auth_links",
                    &json!({
                        "customer": {"name": r.customer.name, "email": r.customer.email},
                        "type": "link",
                        "amount": 0,
                        "currency": r.currency,
                        "description": "Save a payment method",
                        "subscription_registration": {"method": "card", "max_amount": 50_000_000},
                        "receipt": receipt(&r.idempotency_key),
                        "notes": {"forgebase_customer": r.customer.customer_id},
                    }),
                    &r.idempotency_key,
                )
                .await?;
            match (str_at(&v, &["short_url"]), str_at(&v, &["id"])) {
                (Some(url), Some(id)) => Ok(HostedPage {
                    url: url.to_owned(),
                    session_id: id.to_owned(),
                }),
                _ => Err(BillingError::Provider("Razorpay returned no authorization link".into())),
            }
        })
    }

    fn verify_webhook(
        &self,
        headers: &[(String, String)],
        body: &[u8],
        _now_ms: i64,
        _tolerance_secs: i64,
    ) -> Result<ProviderEvent> {
        let sig = header(headers, "x-razorpay-signature")
            .ok_or_else(|| BillingError::webhook("invalid_signature", "missing X-Razorpay-Signature header"))?;
        let expected = hmac_hex(self.webhook_secret.as_bytes(), body);
        if !same_signature(&expected, sig) {
            return Err(BillingError::webhook(
                "invalid_signature",
                "the webhook signature does not match",
            ));
        }
        let id = header(headers, "x-razorpay-event-id")
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| BillingError::webhook("invalid_payload", "missing X-Razorpay-Event-Id header"))?;
        let ev = json_body(body)?;
        parse_event(id, &ev)
    }
}

/// Parses a Razorpay event body (with the event id from its header).
pub fn parse_event(id: &str, ev: &Json) -> Result<ProviderEvent> {
    let event_type = str_at(ev, &["event"]).unwrap_or_default().to_owned();
    let created = int_at(ev, &["created_at"])
        .ok_or_else(|| BillingError::webhook("invalid_payload", "the event has no created_at"))?;
    let payment = ev.pointer("/payload/payment/entity").cloned().unwrap_or(Json::Null);
    let invoice_id = str_at(&payment, &["notes", "forgebase_invoice"])
        .or_else(|| {
            ev.pointer("/payload/payment_link/entity/notes/forgebase_invoice")
                .and_then(Json::as_str)
        })
        .map(str::to_owned);
    let action = match event_type.as_str() {
        "payment.captured" | "payment_link.paid" | "order.paid" => match str_at(&payment, &["id"]) {
            Some(pay) => EventAction::PaymentSucceeded {
                invoice_id,
                payment_ref: pay.to_owned(),
                amount: int_at(&payment, &["amount"]).unwrap_or_default(),
                currency: str_at(&payment, &["currency"]).unwrap_or_default().to_ascii_uppercase(),
                provider_customer: str_at(&payment, &["customer_id"]).map(str::to_owned),
                method: str_at(&payment, &["token_id"]).map(|t| SavedMethod {
                    token: t.to_owned(),
                    label: str_at(&payment, &["method"]).unwrap_or("card").to_owned(),
                }),
            },
            None => EventAction::Other,
        },
        "payment.failed" => match str_at(&payment, &["id"]) {
            Some(pay) => EventAction::PaymentFailed {
                invoice_id,
                payment_ref: pay.to_owned(),
                code: str_at(&payment, &["error_code"]).map(str::to_owned),
                message: str_at(&payment, &["error_description"]).map(str::to_owned),
            },
            None => EventAction::Other,
        },
        "refund.processed" | "refund.failed" | "refund.created" => {
            let r = ev.pointer("/payload/refund/entity").cloned().unwrap_or(Json::Null);
            match str_at(&r, &["id"]) {
                Some(re) => EventAction::RefundUpdated {
                    refund_ref: re.to_owned(),
                    payment_ref: str_at(&r, &["payment_id"]).map(str::to_owned),
                    status: refund_status(str_at(&r, &["status"]).unwrap_or("pending")),
                },
                None => EventAction::Other,
            }
        }
        "token.confirmed" => {
            let t = ev.pointer("/payload/token/entity").cloned().unwrap_or(Json::Null);
            match str_at(&t, &["id"]) {
                Some(tok) => EventAction::PaymentMethodSaved {
                    customer_id: str_at(&t, &["notes", "forgebase_customer"]).map(str::to_owned),
                    provider_customer: str_at(&t, &["customer_id"]).map(str::to_owned),
                    method: SavedMethod {
                        token: tok.to_owned(),
                        label: match (str_at(&t, &["card", "network"]), str_at(&t, &["card", "last4"])) {
                            (Some(n), Some(l)) => format!("{} ••{l}", n.to_ascii_lowercase()),
                            _ => str_at(&t, &["method"]).unwrap_or("card").to_owned(),
                        },
                    },
                },
                None => EventAction::Other,
            }
        }
        _ => EventAction::Other,
    };
    Ok(ProviderEvent {
        id: id.trim().to_owned(),
        event_type,
        created_at: created.saturating_mul(1000),
        action,
    })
}

/// Signs a Razorpay webhook body as Razorpay does (tests and tooling).
pub fn sign(webhook_secret: &str, body: &[u8]) -> String {
    hmac_hex(webhook_secret.as_bytes(), body)
}
