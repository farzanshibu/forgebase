//! Creem (FB-72), test mode only (`creem_test_…` keys, `test-api.creem.io`).
//!
//! Creem is a merchant of record: it owns the checkout, the subscription and
//! its renewals, so it is driven like the app stores. ForgeBase opens a
//! hosted checkout for the store product mapped to a price (`POST
//! /v1/checkouts`, the subject in the metadata), verifies the success
//! redirect server side (`GET /v1/checkouts?checkout_id=`), and mirrors the
//! signed webhooks (`creem-signature`: HMAC-SHA256 hex of the raw body under
//! the webhook secret) into subscriptions, orders, invoices and refunds.
//! Events are deduplicated by their `id`.

use std::sync::Arc;

use forgebase_auth::social::http_client::{HttpClient, HttpRequest};
use forgebase_db::backend::BoxFuture;
use forgebase_entitlements::Clock;
use forgebase_entitlements::model::parse_iso;
use serde_json::{Value as Json, json};

use super::store::{
    PurchaseClaim, PurchaseKind, StoreCheckoutRequest, StoreNotification, StoreProvider, StorePurchase, StoreRefund,
    StoreState,
};
use super::{header, hmac_hex, json_body, parse_ok, same_signature, str_at};
use crate::error::{BillingError, Result};
use crate::model::HostedPage;

/// Test-mode API base.
pub const TEST_API: &str = "https://test-api.creem.io";
/// Production API base (refused).
pub const PRODUCTION_API: &str = "https://api.creem.io";

/// The Creem driver.
#[derive(Clone)]
pub struct CreemProvider {
    api_key: String,
    webhook_secret: String,
    api_base: String,
    http: Arc<dyn HttpClient>,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for CreemProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreemProvider")
            .field("api_base", &self.api_base)
            .finish_non_exhaustive()
    }
}

/// Signs a body like Creem does (tests and tools).
pub fn sign(secret: &str, body: &[u8]) -> String {
    hmac_hex(secret.as_bytes(), body)
}

/// Refuses live and malformed Creem keys.
pub fn check_keys(api_key: &str, webhook_secret: &str) -> Result<()> {
    if !api_key.starts_with("creem_test_") {
        return Err(if api_key.starts_with("creem_") {
            BillingError::invalid(
                "live_mode_refused",
                "only Creem test-mode keys (creem_test_…) are accepted",
            )
        } else {
            BillingError::invalid("invalid_key", "the Creem API key must start with creem_test_")
        });
    }
    if api_key.len() < 16 || webhook_secret.len() < 8 {
        return Err(BillingError::invalid(
            "invalid_key",
            "the Creem API key and webhook secret are required",
        ));
    }
    Ok(())
}

impl CreemProvider {
    /// A test-mode driver.
    pub fn new(
        api_key: &str,
        webhook_secret: &str,
        api_base: Option<&str>,
        http: Arc<dyn HttpClient>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self> {
        check_keys(api_key, webhook_secret)?;
        let api_base = api_base.unwrap_or(TEST_API).trim_end_matches('/').to_owned();
        if api_base == PRODUCTION_API {
            return Err(BillingError::invalid(
                "live_mode_refused",
                "the production Creem API is refused",
            ));
        }
        Ok(Self {
            api_key: api_key.to_owned(),
            webhook_secret: webhook_secret.to_owned(),
            api_base,
            http,
            clock,
        })
    }

    async fn get(&self, path: &str) -> Result<Json> {
        let req = HttpRequest::get(format!("{}{path}", self.api_base)).header("x-api-key", self.api_key.clone());
        let res = self.http.send(req).await?;
        if res.status == 404 {
            return Err(BillingError::invalid(
                "unknown_purchase",
                "Creem does not know this checkout",
            ));
        }
        parse_ok(&res)
    }

    fn id_of(v: Option<&Json>) -> Option<String> {
        match v? {
            Json::String(s) => Some(s.clone()),
            o => str_at(o, &["id"]).map(str::to_owned),
        }
    }

    fn live(v: &Json) -> bool {
        matches!(str_at(v, &["mode"]), Some("prod" | "live" | "production"))
    }

    fn hint(v: &Json) -> Option<String> {
        str_at(v, &["metadata", "forgebase_subject"]).map(str::to_owned)
    }

    /// A subscription object (`sub_…`).
    fn read_subscription(&self, s: &Json, transaction: Option<String>, hint: Option<String>) -> Result<StorePurchase> {
        let now = self.clock.now_ms();
        let id = str_at(s, &["id"])
            .ok_or_else(|| BillingError::webhook("invalid_payload", "no subscription id"))?
            .to_owned();
        let status = str_at(s, &["status"]).unwrap_or_default();
        let state = match status {
            "active" | "scheduled_cancel" => StoreState::Active,
            "trialing" => StoreState::Trialing,
            "past_due" => StoreState::Grace,
            "unpaid" => StoreState::OnHold,
            "paused" => StoreState::Paused,
            "incomplete" | "pending" => StoreState::Pending,
            _ => StoreState::Expired,
        };
        let product = s.get("product");
        let (amount, currency) = match product {
            Some(p @ Json::Object(_)) => (
                p.get("price").and_then(Json::as_i64),
                str_at(p, &["currency"]).map(str::to_ascii_uppercase),
            ),
            _ => (None, None),
        };
        let last = s.get("last_transaction");
        let canceled = s.get("canceled_at").is_some_and(|c| !c.is_null());
        Ok(StorePurchase {
            purchase_id: id,
            transaction_id: transaction.or_else(|| str_at(s, &["last_transaction_id"]).map(str::to_owned)),
            product_id: Self::id_of(product).unwrap_or_default(),
            kind: PurchaseKind::Subscription,
            state,
            auto_renew: state.live() && status != "scheduled_cancel" && !canceled,
            period_start: str_at(s, &["current_period_start_date"]).and_then(parse_iso),
            expires_at: str_at(s, &["current_period_end_date"]).and_then(parse_iso),
            purchased_at: str_at(s, &["created_at"]).and_then(parse_iso).unwrap_or(now),
            amount: last.and_then(|t| t.get("amount")).and_then(Json::as_i64).or(amount),
            currency: last
                .and_then(|t| str_at(t, &["currency"]))
                .map(str::to_ascii_uppercase)
                .or(currency),
            quantity: 1,
            test: !Self::live(s),
            subject_hint: Self::hint(s).or(hint),
            replaces: None,
            refund: None,
            refund_only: false,
            observed_at: now,
            needs_ack: false,
        })
    }

    /// A checkout object (`ch_…`): its subscription, else its order.
    fn read_checkout(&self, c: &Json) -> Result<StorePurchase> {
        let now = self.clock.now_ms();
        let order = c.get("order").cloned().unwrap_or(Json::Null);
        let txn = str_at(&order, &["transaction"])
            .or_else(|| str_at(&order, &["id"]))
            .map(str::to_owned);
        let hint = Self::hint(c);
        let completed = str_at(c, &["status"]) == Some("completed");
        if let Some(s @ Json::Object(_)) = c.get("subscription") {
            let mut p = self.read_subscription(s, txn, hint)?;
            if p.product_id.is_empty() {
                p.product_id = Self::id_of(c.get("product")).unwrap_or_default();
            }
            if p.amount.is_none() {
                p.amount = order.get("amount").and_then(Json::as_i64);
                p.currency = str_at(&order, &["currency"]).map(str::to_ascii_uppercase);
            }
            if !completed {
                p.state = StoreState::Pending;
            }
            p.test = p.test && !Self::live(c);
            return Ok(p);
        }
        let id = str_at(&order, &["id"])
            .ok_or_else(|| BillingError::invalid("unknown_purchase", "the checkout has no order yet"))?
            .to_owned();
        Ok(StorePurchase {
            purchase_id: id,
            transaction_id: txn,
            product_id: Self::id_of(c.get("product"))
                .or_else(|| Self::id_of(order.get("product")))
                .unwrap_or_default(),
            kind: PurchaseKind::OneTime,
            state: if completed && str_at(&order, &["status"]).is_none_or(|s| s == "paid") {
                StoreState::Active
            } else {
                StoreState::Pending
            },
            auto_renew: false,
            period_start: None,
            expires_at: None,
            purchased_at: str_at(&order, &["created_at"]).and_then(parse_iso).unwrap_or(now),
            amount: order.get("amount").and_then(Json::as_i64),
            currency: str_at(&order, &["currency"]).map(str::to_ascii_uppercase),
            quantity: c.get("units").and_then(Json::as_i64).unwrap_or(1).max(1),
            test: !Self::live(c) && !Self::live(&order),
            subject_hint: hint,
            replaces: None,
            refund: None,
            refund_only: false,
            observed_at: now,
            needs_ack: false,
        })
    }

    /// A refund object (`ref_…`): only the refund; state changes come with
    /// the subscription events.
    fn read_refund(&self, r: &Json) -> Result<StorePurchase> {
        let now = self.clock.now_ms();
        let refund_id = str_at(r, &["id"])
            .ok_or_else(|| BillingError::webhook("invalid_payload", "no refund id"))?
            .to_owned();
        let order = r.get("order").cloned().unwrap_or(Json::Null);
        let txn = Self::id_of(r.get("transaction"))
            .or_else(|| str_at(&order, &["transaction"]).map(str::to_owned))
            .or_else(|| str_at(&order, &["id"]).map(str::to_owned))
            .ok_or_else(|| BillingError::webhook("invalid_payload", "the refund names no transaction"))?;
        let (purchase_id, kind) = match Self::id_of(r.get("subscription")) {
            Some(s) => (s, PurchaseKind::Subscription),
            None => (
                str_at(&order, &["id"]).unwrap_or(&txn).to_owned(),
                PurchaseKind::OneTime,
            ),
        };
        Ok(StorePurchase {
            purchase_id,
            transaction_id: Some(txn.clone()),
            product_id: String::new(),
            kind,
            state: StoreState::Refunded,
            auto_renew: false,
            period_start: None,
            expires_at: None,
            purchased_at: now,
            amount: None,
            currency: str_at(r, &["refund_currency"]).map(str::to_ascii_uppercase),
            quantity: 1,
            test: !Self::live(r),
            subject_hint: None,
            replaces: None,
            refund: (str_at(r, &["status"]).is_none_or(|s| s == "succeeded")).then(|| StoreRefund {
                transaction_id: txn,
                refund_ref: refund_id,
                amount: r.get("refund_amount").and_then(Json::as_i64),
            }),
            refund_only: true,
            observed_at: now,
            needs_ack: false,
        })
    }
}

impl StoreProvider for CreemProvider {
    fn name(&self) -> &'static str {
        "creem"
    }

    fn verify_purchase<'a>(&'a self, claim: &'a PurchaseClaim) -> BoxFuture<'a, Result<StorePurchase>> {
        Box::pin(async move {
            let id = claim
                .checkout_id
                .as_deref()
                .filter(|c| !c.is_empty() && c.len() <= 200)
                .ok_or_else(|| BillingError::invalid("checkout_required", "send checkout_id"))?;
            let enc: String = url::form_urlencoded::byte_serialize(id.as_bytes()).collect();
            let c = self.get(&format!("/v1/checkouts?checkout_id={enc}")).await?;
            self.read_checkout(&c)
        })
    }

    fn refresh<'a>(
        &'a self,
        purchase_id: &'a str,
        _product_id: &'a str,
        kind: PurchaseKind,
    ) -> BoxFuture<'a, Result<StorePurchase>> {
        Box::pin(async move {
            if kind != PurchaseKind::Subscription {
                return Err(BillingError::invalid("not_supported", "Creem orders are not read back"));
            }
            let enc: String = url::form_urlencoded::byte_serialize(purchase_id.as_bytes()).collect();
            let s = self.get(&format!("/v1/subscriptions?subscription_id={enc}")).await?;
            self.read_subscription(&s, None, None)
        })
    }

    fn verify_notification<'a>(
        &'a self,
        headers: &'a [(String, String)],
        body: &'a [u8],
        _now_ms: i64,
    ) -> BoxFuture<'a, Result<StoreNotification>> {
        Box::pin(async move {
            let given = header(headers, "creem-signature")
                .ok_or_else(|| BillingError::webhook("invalid_signature", "no creem-signature header"))?;
            if !same_signature(&sign(&self.webhook_secret, body), given) {
                return Err(BillingError::webhook(
                    "invalid_signature",
                    "the Creem signature does not match",
                ));
            }
            let v = json_body(body)?;
            let id = str_at(&v, &["id"])
                .ok_or_else(|| BillingError::webhook("invalid_payload", "no event id"))?
                .to_owned();
            let event_type = str_at(&v, &["eventType"]).unwrap_or_default().to_owned();
            let created_at = v
                .get("created_at")
                .and_then(Json::as_i64)
                .unwrap_or_else(|| self.clock.now_ms());
            let obj = v.get("object").cloned().unwrap_or(Json::Null);
            let mut purchase = match event_type.as_str() {
                "checkout.completed" => Some(self.read_checkout(&obj)?),
                t if t.starts_with("subscription.") => {
                    let txn = (t == "subscription.paid")
                        .then(|| str_at(&obj, &["last_transaction_id"]).map(str::to_owned))
                        .flatten();
                    let mut p = self.read_subscription(&obj, txn, None)?;
                    if t != "subscription.paid" {
                        // Only a payment event raises an invoice.
                        p.transaction_id = None;
                    }
                    Some(p)
                }
                "refund.created" => Some(self.read_refund(&obj)?),
                _ => None,
            };
            if let Some(p) = &mut purchase {
                p.observed_at = created_at;
            }
            Ok(StoreNotification {
                id,
                event_type,
                created_at,
                purchase,
            })
        })
    }

    fn checkout<'a>(&'a self, req: &'a StoreCheckoutRequest) -> BoxFuture<'a, Result<HostedPage>> {
        Box::pin(async move {
            let mut body = json!({
                "product_id": req.product_id,
                "request_id": req.request_id,
                "success_url": req.success_url,
                "metadata": {"forgebase_subject": req.subject},
            });
            if let Some(e) = &req.email {
                body["customer"] = json!({"email": e});
            }
            let mut http = HttpRequest::get(format!("{}/v1/checkouts", self.api_base))
                .header("x-api-key", self.api_key.clone())
                .header("content-type", "application/json");
            http.method = "POST";
            http.body = body.to_string().into_bytes();
            let v = parse_ok(&self.http.send(http).await?)?;
            Ok(HostedPage {
                url: str_at(&v, &["checkout_url"])
                    .ok_or_else(|| BillingError::Provider("Creem returned no checkout_url".into()))?
                    .to_owned(),
                session_id: str_at(&v, &["id"]).unwrap_or_default().to_owned(),
            })
        })
    }
}
