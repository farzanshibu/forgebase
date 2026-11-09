//! Stripe driver (test mode). REST calls are form-encoded with
//! `Authorization: Bearer sk_test_…` and an `Idempotency-Key`; webhooks are
//! verified with the `Stripe-Signature` scheme (`t=<unix>,v1=<hex>`, HMAC-SHA256
//! of `"<t>.<body>"` under the endpoint secret) and a timestamp tolerance.

use std::sync::Arc;

use forgebase_auth::social::http_client::{HttpClient, HttpRequest};
use forgebase_db::backend::BoxFuture;
use serde_json::Value as Json;

use super::{
    ChargeRequest, ChargeResult, CustomerRef, EventAction, HostedPaymentRequest, HostedSetupRequest, PaymentProvider,
    ProviderEvent, RefundRequest, RefundResult, SavedMethod, header, hmac_hex, int_at, json_body, parse_ok,
    same_signature, str_at,
};
use crate::error::{BillingError, Result};
use crate::model::{HostedPage, PaymentStatus, RefundStatus};

/// Default API base.
pub const API_BASE: &str = "https://api.stripe.com";

/// Stripe in test mode.
#[derive(Clone)]
pub struct StripeProvider {
    secret_key: String,
    webhook_secret: String,
    api_base: String,
    http: Arc<dyn HttpClient>,
}

impl std::fmt::Debug for StripeProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StripeProvider")
            .field("api_base", &self.api_base)
            .finish_non_exhaustive()
    }
}

impl StripeProvider {
    /// A driver for test-mode keys only (`sk_test_…` or `rk_test_…`, webhook
    /// secret `whsec_…`).
    pub fn new(
        secret_key: &str,
        webhook_secret: &str,
        api_base: Option<&str>,
        http: Arc<dyn HttpClient>,
    ) -> Result<Self> {
        check_keys(secret_key, webhook_secret)?;
        Ok(Self {
            secret_key: secret_key.to_owned(),
            webhook_secret: webhook_secret.to_owned(),
            api_base: api_base.unwrap_or(API_BASE).trim_end_matches('/').to_owned(),
            http,
        })
    }

    async fn post(&self, path: &str, fields: &[(&str, &str)], idem: &str) -> Result<Json> {
        let req = HttpRequest::post_form(format!("{}{path}", self.api_base), fields)
            .header("authorization", format!("Bearer {}", self.secret_key))
            .header("idempotency-key", idem)
            .header("stripe-version", "2024-06-20");
        let res = self.http.send(req).await?;
        parse_ok(&res)
    }
}

/// Refuses live and malformed Stripe keys.
pub fn check_keys(secret_key: &str, webhook_secret: &str) -> Result<()> {
    if secret_key.starts_with("sk_live_") || secret_key.starts_with("rk_live_") {
        return Err(BillingError::invalid(
            "live_mode_refused",
            "only Stripe test-mode keys (sk_test_…) are accepted",
        ));
    }
    if !(secret_key.starts_with("sk_test_") || secret_key.starts_with("rk_test_")) || secret_key.len() < 12 {
        return Err(BillingError::invalid("invalid_key", "the Stripe secret key must start with sk_test_"));
    }
    if !webhook_secret.starts_with("whsec_") || webhook_secret.len() < 12 {
        return Err(BillingError::invalid(
            "invalid_key",
            "the Stripe webhook secret must start with whsec_",
        ));
    }
    Ok(())
}

fn lower(c: &str) -> String {
    c.to_ascii_lowercase()
}

fn amount_str(a: i64) -> String {
    a.to_string()
}

impl PaymentProvider for StripeProvider {
    fn name(&self) -> &'static str {
        "stripe"
    }

    fn create_customer<'a>(&'a self, c: &'a CustomerRef) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let mut f = vec![
                ("metadata[forgebase_customer]", c.customer_id.as_str()),
                ("metadata[forgebase_subject]", c.subject.as_str()),
            ];
            if let Some(e) = &c.email {
                f.push(("email", e));
            }
            if let Some(n) = &c.name {
                f.push(("name", n));
            }
            let v = self
                .post("/v1/customers", &f, &format!("customer:{}", c.customer_id))
                .await?;
            str_at(&v, &["id"])
                .map(str::to_owned)
                .ok_or_else(|| BillingError::Provider("Stripe returned no customer id".into()))
        })
    }

    fn charge<'a>(&'a self, r: &'a ChargeRequest) -> BoxFuture<'a, Result<ChargeResult>> {
        Box::pin(async move {
            let (amount, currency) = (amount_str(r.amount), lower(&r.currency));
            let f = [
                ("amount", amount.as_str()),
                ("currency", currency.as_str()),
                ("customer", r.provider_customer_id.as_str()),
                ("payment_method", r.payment_method.as_str()),
                ("off_session", "true"),
                ("confirm", "true"),
                ("description", r.description.as_str()),
                ("metadata[forgebase_invoice]", r.invoice_id.as_str()),
                ("metadata[invoice_number]", r.invoice_number.as_str()),
            ];
            let req = HttpRequest::post_form(format!("{}/v1/payment_intents", self.api_base), &f)
                .header("authorization", format!("Bearer {}", self.secret_key))
                .header("idempotency-key", r.idempotency_key.as_str())
                .header("stripe-version", "2024-06-20");
            let res = self.http.send(req).await?;
            let v: Json = serde_json::from_slice(&res.body).unwrap_or(Json::Null);
            if res.status == 402 || (res.status >= 400 && v.get("error").is_some() && res.status < 500) {
                // Card declined: the PaymentIntent is in the error object.
                let code = str_at(&v, &["error", "decline_code"])
                    .or_else(|| str_at(&v, &["error", "code"]))
                    .unwrap_or("card_declined");
                let mut out = ChargeResult::failed(code, str_at(&v, &["error", "message"]).unwrap_or("declined"));
                out.provider_ref = str_at(&v, &["error", "payment_intent", "id"]).map(str::to_owned);
                return Ok(out);
            }
            let v = parse_ok(&res)?;
            let id = str_at(&v, &["id"]).map(str::to_owned);
            Ok(match str_at(&v, &["status"]).unwrap_or_default() {
                "succeeded" => ChargeResult {
                    status: PaymentStatus::Succeeded,
                    provider_ref: id,
                    failure_code: None,
                    failure_message: None,
                },
                "processing" => ChargeResult {
                    status: PaymentStatus::Pending,
                    provider_ref: id,
                    failure_code: None,
                    failure_message: None,
                },
                other => {
                    let mut out = ChargeResult::failed(
                        str_at(&v, &["last_payment_error", "code"]).unwrap_or(match other {
                            "requires_action" => "authentication_required",
                            _ => "payment_failed",
                        }),
                        str_at(&v, &["last_payment_error", "message"]).unwrap_or("the payment was not completed"),
                    );
                    out.provider_ref = id;
                    out
                }
            })
        })
    }

    fn refund<'a>(&'a self, r: &'a RefundRequest) -> BoxFuture<'a, Result<RefundResult>> {
        Box::pin(async move {
            let amount = amount_str(r.amount);
            let f = [
                ("payment_intent", r.payment_ref.as_str()),
                ("amount", amount.as_str()),
                ("metadata[forgebase_refund]", r.idempotency_key.as_str()),
            ];
            let v = self.post("/v1/refunds", &f, &r.idempotency_key).await?;
            Ok(RefundResult {
                provider_ref: str_at(&v, &["id"]).map(str::to_owned),
                status: refund_status(str_at(&v, &["status"]).unwrap_or("pending")),
            })
        })
    }

    fn hosted_payment<'a>(&'a self, r: &'a HostedPaymentRequest) -> BoxFuture<'a, Result<HostedPage>> {
        Box::pin(async move {
            let (amount, currency) = (amount_str(r.amount), lower(&r.currency));
            let mut f = vec![
                ("mode", "payment"),
                ("success_url", r.return_url.as_str()),
                ("cancel_url", r.return_url.as_str()),
                ("line_items[0][quantity]", "1"),
                ("line_items[0][price_data][currency]", currency.as_str()),
                ("line_items[0][price_data][unit_amount]", amount.as_str()),
                ("line_items[0][price_data][product_data][name]", r.description.as_str()),
                ("payment_intent_data[setup_future_usage]", "off_session"),
                ("payment_intent_data[metadata][forgebase_invoice]", r.invoice_id.as_str()),
                ("metadata[forgebase_invoice]", r.invoice_id.as_str()),
            ];
            if let Some(c) = &r.customer.provider_customer_id {
                f.push(("customer", c));
            }
            let v = self.post("/v1/checkout/sessions", &f, &r.idempotency_key).await?;
            hosted(&v)
        })
    }

    fn hosted_setup<'a>(&'a self, r: &'a HostedSetupRequest) -> BoxFuture<'a, Result<HostedPage>> {
        Box::pin(async move {
            let currency = lower(&r.currency);
            let mut f = vec![
                ("mode", "setup"),
                ("currency", currency.as_str()),
                ("success_url", r.return_url.as_str()),
                ("cancel_url", r.return_url.as_str()),
                ("setup_intent_data[metadata][forgebase_customer]", r.customer.customer_id.as_str()),
            ];
            if let Some(c) = &r.customer.provider_customer_id {
                f.push(("customer", c));
            }
            let v = self.post("/v1/checkout/sessions", &f, &r.idempotency_key).await?;
            hosted(&v)
        })
    }

    fn verify_webhook(
        &self,
        headers: &[(String, String)],
        body: &[u8],
        now_ms: i64,
        tolerance_secs: i64,
    ) -> Result<ProviderEvent> {
        let sig = header(headers, "stripe-signature")
            .ok_or_else(|| BillingError::webhook("invalid_signature", "missing Stripe-Signature header"))?;
        let mut t: Option<i64> = None;
        let mut v1: Vec<&str> = Vec::new();
        for part in sig.split(',') {
            match part.trim().split_once('=') {
                Some(("t", v)) => t = v.parse().ok(),
                Some(("v1", v)) => v1.push(v),
                _ => {}
            }
        }
        let t = t.ok_or_else(|| BillingError::webhook("invalid_signature", "the signature has no timestamp"))?;
        let mut signed = format!("{t}.").into_bytes();
        signed.extend_from_slice(body);
        let expected = hmac_hex(self.webhook_secret.as_bytes(), &signed);
        if !v1.iter().any(|s| same_signature(&expected, s)) {
            return Err(BillingError::webhook("invalid_signature", "the webhook signature does not match"));
        }
        if (now_ms / 1000 - t).abs() > tolerance_secs {
            return Err(BillingError::webhook(
                "stale_signature",
                "the signed timestamp is outside the tolerance (replayed delivery?)",
            ));
        }
        let ev = json_body(body)?;
        parse_event(&ev)
    }
}

fn hosted(v: &Json) -> Result<HostedPage> {
    match (str_at(v, &["url"]), str_at(v, &["id"])) {
        (Some(url), Some(id)) => Ok(HostedPage {
            url: url.to_owned(),
            session_id: id.to_owned(),
        }),
        _ => Err(BillingError::Provider("Stripe returned no checkout URL".into())),
    }
}

fn refund_status(s: &str) -> RefundStatus {
    match s {
        "succeeded" => RefundStatus::Succeeded,
        "failed" | "canceled" => RefundStatus::Failed,
        _ => RefundStatus::Pending,
    }
}

fn method(o: &Json) -> Option<SavedMethod> {
    let token = str_at(o, &["payment_method"])?;
    let label = match (
        str_at(o, &["payment_method_details", "card", "brand"]),
        str_at(o, &["payment_method_details", "card", "last4"]),
    ) {
        (Some(b), Some(l)) => format!("{b} ••{l}"),
        _ => "card".to_owned(),
    };
    Some(SavedMethod {
        token: token.to_owned(),
        label,
    })
}

/// Parses a Stripe event body into a [`ProviderEvent`].
pub fn parse_event(ev: &Json) -> Result<ProviderEvent> {
    let id = str_at(ev, &["id"]).ok_or_else(|| BillingError::webhook("invalid_payload", "the event has no id"))?;
    let event_type = str_at(ev, &["type"]).unwrap_or_default().to_owned();
    let created = int_at(ev, &["created"])
        .ok_or_else(|| BillingError::webhook("invalid_payload", "the event has no created time"))?;
    let o = ev.get("data").and_then(|d| d.get("object")).cloned().unwrap_or(Json::Null);
    let invoice_id = str_at(&o, &["metadata", "forgebase_invoice"]).map(str::to_owned);
    let action = match event_type.as_str() {
        "payment_intent.succeeded" => match str_at(&o, &["id"]) {
            Some(pi) => EventAction::PaymentSucceeded {
                invoice_id,
                payment_ref: pi.to_owned(),
                amount: int_at(&o, &["amount_received"])
                    .or_else(|| int_at(&o, &["amount"]))
                    .unwrap_or_default(),
                currency: str_at(&o, &["currency"]).unwrap_or_default().to_ascii_uppercase(),
                provider_customer: str_at(&o, &["customer"]).map(str::to_owned),
                method: method(&o),
            },
            None => EventAction::Other,
        },
        "payment_intent.payment_failed" => match str_at(&o, &["id"]) {
            Some(pi) => EventAction::PaymentFailed {
                invoice_id,
                payment_ref: pi.to_owned(),
                code: str_at(&o, &["last_payment_error", "decline_code"])
                    .or_else(|| str_at(&o, &["last_payment_error", "code"]))
                    .map(str::to_owned),
                message: str_at(&o, &["last_payment_error", "message"]).map(str::to_owned),
            },
            None => EventAction::Other,
        },
        "refund.created" | "refund.updated" | "refund.failed" => match str_at(&o, &["id"]) {
            Some(re) => EventAction::RefundUpdated {
                refund_ref: re.to_owned(),
                payment_ref: str_at(&o, &["payment_intent"]).map(str::to_owned),
                status: refund_status(str_at(&o, &["status"]).unwrap_or("pending")),
            },
            None => EventAction::Other,
        },
        "setup_intent.succeeded" => match str_at(&o, &["payment_method"]) {
            Some(pm) => EventAction::PaymentMethodSaved {
                customer_id: str_at(&o, &["metadata", "forgebase_customer"]).map(str::to_owned),
                provider_customer: str_at(&o, &["customer"]).map(str::to_owned),
                method: SavedMethod {
                    token: pm.to_owned(),
                    label: "card".to_owned(),
                },
            },
            None => EventAction::Other,
        },
        _ => EventAction::Other,
    };
    Ok(ProviderEvent {
        id: id.to_owned(),
        event_type,
        created_at: created.saturating_mul(1000),
        action,
    })
}

/// Signs a Stripe webhook body as Stripe does (tests and local tooling).
pub fn sign(webhook_secret: &str, t_secs: i64, body: &[u8]) -> String {
    let mut signed = format!("{t_secs}.").into_bytes();
    signed.extend_from_slice(body);
    format!("t={t_secs},v1={}", hmac_hex(webhook_secret.as_bytes(), &signed))
}
