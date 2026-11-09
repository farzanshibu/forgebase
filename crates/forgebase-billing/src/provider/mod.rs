//! Payment providers: the [`PaymentProvider`] trait, the normalised webhook
//! event, and the Stripe, Razorpay, WeChat Pay and manual drivers; the
//! [`StoreProvider`] trait of merchant-of-record stores (FB-72) and the App
//! Store, Google Play and Creem drivers.
//!
//! Drivers speak HTTP through FB-25's outbound client
//! ([`forgebase_auth::social::http_client::HttpClient`], rustls in
//! production, a mock in tests). Only **test-mode** credentials are accepted
//! (`sk_test_…`, `rzp_test_…`): live keys are refused when the account is
//! saved. Card data never passes through ForgeBase: customers pay on the
//! provider's hosted page and only provider references (customer, payment
//! method and payment ids) are stored.

pub mod app_store;
pub mod creem;
pub(crate) mod der;
pub mod google_play;
pub mod jws;
pub mod manual;
pub mod razorpay;
pub mod store;
pub mod stripe;
pub mod wechat_pay;

use std::fmt;

use forgebase_db::backend::BoxFuture;
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

use crate::error::{BillingError, Result};
use crate::model::{HostedPage, PaymentStatus, RefundStatus};
use crate::money::hex;

pub use app_store::AppStoreProvider;
pub use creem::CreemProvider;
pub use google_play::GooglePlayProvider;
pub use manual::ManualProvider;
pub use razorpay::RazorpayProvider;
pub use store::{PurchaseClaim, PurchaseKind, StoreNotification, StoreProvider, StorePurchase, StoreState};
pub use stripe::StripeProvider;
pub use wechat_pay::WechatPayProvider;

/// A customer as the provider needs it.
#[derive(Debug, Clone, Default)]
pub struct CustomerRef {
    /// ForgeBase customer id.
    pub customer_id: String,
    /// Billing subject.
    pub subject: String,
    /// Email.
    pub email: Option<String>,
    /// Name.
    pub name: Option<String>,
    /// The customer at the provider, once created.
    pub provider_customer_id: Option<String>,
}

/// An off-session charge of a saved payment method.
#[derive(Debug, Clone, Default)]
pub struct ChargeRequest {
    /// Invoice paid.
    pub invoice_id: String,
    /// Invoice number (receipt).
    pub invoice_number: String,
    /// Provider customer id.
    pub provider_customer_id: String,
    /// Saved payment method (Stripe `pm_…`, Razorpay `token_…`).
    pub payment_method: String,
    /// Customer email (Razorpay recurring payments need it).
    pub email: Option<String>,
    /// Minor units.
    pub amount: i64,
    /// ISO 4217, upper case.
    pub currency: String,
    /// Sent as the provider idempotency key.
    pub idempotency_key: String,
    /// Text on the statement.
    pub description: String,
}

/// What a charge did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChargeResult {
    /// `succeeded`, `pending` (webhook decides) or `failed`.
    pub status: PaymentStatus,
    /// Payment id at the provider.
    pub provider_ref: Option<String>,
    /// Decline code.
    pub failure_code: Option<String>,
    /// Decline message.
    pub failure_message: Option<String>,
}

impl ChargeResult {
    pub(crate) fn failed(code: &str, message: impl Into<String>) -> Self {
        Self {
            status: PaymentStatus::Failed,
            provider_ref: None,
            failure_code: Some(code.to_owned()),
            failure_message: Some(message.into()),
        }
    }
}

/// A refund of a provider payment.
#[derive(Debug, Clone, Default)]
pub struct RefundRequest {
    /// Payment id at the provider.
    pub payment_ref: String,
    /// Minor units.
    pub amount: i64,
    /// Currency.
    pub currency: String,
    /// Sent as the provider idempotency key.
    pub idempotency_key: String,
    /// Why.
    pub reason: Option<String>,
}

/// What a refund did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefundResult {
    /// Refund id at the provider.
    pub provider_ref: Option<String>,
    /// State.
    pub status: RefundStatus,
}

/// A hosted payment page for one invoice.
#[derive(Debug, Clone, Default)]
pub struct HostedPaymentRequest {
    /// Invoice paid.
    pub invoice_id: String,
    /// Text shown.
    pub description: String,
    /// Minor units.
    pub amount: i64,
    /// Currency.
    pub currency: String,
    /// Customer.
    pub customer: CustomerRef,
    /// Where the page returns to.
    pub return_url: String,
    /// Sent as the provider idempotency key.
    pub idempotency_key: String,
}

/// A hosted page that saves a payment method for later charges.
#[derive(Debug, Clone, Default)]
pub struct HostedSetupRequest {
    /// Customer.
    pub customer: CustomerRef,
    /// Currency of later charges.
    pub currency: String,
    /// Where the page returns to.
    pub return_url: String,
    /// Sent as the provider idempotency key.
    pub idempotency_key: String,
}

/// A saved payment method: a provider reference and a label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedMethod {
    /// Provider reference (`pm_…`, `token_…`).
    pub token: String,
    /// `visa ••4242`.
    pub label: String,
}

/// What a verified webhook event means for billing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventAction {
    /// A payment was collected.
    PaymentSucceeded {
        /// ForgeBase invoice id from the payment's metadata.
        invoice_id: Option<String>,
        /// Payment id at the provider.
        payment_ref: String,
        /// Minor units.
        amount: i64,
        /// Currency, upper case.
        currency: String,
        /// The provider customer.
        provider_customer: Option<String>,
        /// Payment method saved for later charges.
        method: Option<SavedMethod>,
    },
    /// A payment failed.
    PaymentFailed {
        /// ForgeBase invoice id.
        invoice_id: Option<String>,
        /// Payment id at the provider.
        payment_ref: String,
        /// Decline code.
        code: Option<String>,
        /// Decline message.
        message: Option<String>,
    },
    /// A refund changed state.
    RefundUpdated {
        /// Refund id at the provider.
        refund_ref: String,
        /// Payment refunded.
        payment_ref: Option<String>,
        /// State.
        status: RefundStatus,
    },
    /// A payment method was saved (hosted setup page).
    PaymentMethodSaved {
        /// ForgeBase customer id from the metadata.
        customer_id: Option<String>,
        /// The provider customer.
        provider_customer: Option<String>,
        /// The method.
        method: SavedMethod,
    },
    /// Not relevant to billing.
    Other,
}

/// A webhook event whose signature was verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderEvent {
    /// Event id at the provider (idempotency key).
    pub id: String,
    /// Event type.
    pub event_type: String,
    /// Provider time of the event, epoch milliseconds.
    pub created_at: i64,
    /// What it means.
    pub action: EventAction,
}

/// A payment provider driver. Every call is idempotent by the key it is
/// given; drivers never see card numbers.
pub trait PaymentProvider: Send + Sync + fmt::Debug {
    /// `stripe`, `razorpay`, `manual`, ...
    fn name(&self) -> &'static str;
    /// Creates the customer at the provider; returns its id.
    fn create_customer<'a>(&'a self, customer: &'a CustomerRef) -> BoxFuture<'a, Result<String>>;
    /// Charges a saved payment method off-session.
    fn charge<'a>(&'a self, req: &'a ChargeRequest) -> BoxFuture<'a, Result<ChargeResult>>;
    /// Refunds (part of) a payment.
    fn refund<'a>(&'a self, req: &'a RefundRequest) -> BoxFuture<'a, Result<RefundResult>>;
    /// A provider-hosted page where the customer pays one invoice.
    fn hosted_payment<'a>(&'a self, req: &'a HostedPaymentRequest) -> BoxFuture<'a, Result<HostedPage>>;
    /// A provider-hosted page where the customer saves a payment method.
    fn hosted_setup<'a>(&'a self, req: &'a HostedSetupRequest) -> BoxFuture<'a, Result<HostedPage>>;
    /// Verifies a webhook delivery (signature, timestamp) and parses it.
    /// `headers` names are lower case.
    fn verify_webhook(
        &self,
        headers: &[(String, String)],
        body: &[u8],
        now_ms: i64,
        tolerance_secs: i64,
    ) -> Result<ProviderEvent>;
}

/// HMAC-SHA256 of `msg` under `key`, lower-case hex.
pub fn hmac_hex(key: &[u8], msg: &[u8]) -> String {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("HMAC accepts keys of any length");
    mac.update(msg);
    hex(&mac.finalize().into_bytes())
}

/// Constant-time comparison of two hex signatures.
pub fn same_signature(expected: &str, given: &str) -> bool {
    let (a, b) = (expected.as_bytes(), given.trim().to_ascii_lowercase());
    a.len() == b.len() && bool::from(a.ct_eq(b.as_bytes()))
}

/// First value of a lower-case header.
pub fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

fn json_body(body: &[u8]) -> Result<serde_json::Value> {
    serde_json::from_slice(body).map_err(|_| BillingError::webhook("invalid_payload", "the body is not JSON"))
}

fn str_at<'a>(v: &'a serde_json::Value, path: &[&str]) -> Option<&'a str> {
    let mut cur = v;
    for p in path {
        cur = cur.get(p)?;
    }
    cur.as_str().filter(|s| !s.is_empty())
}

fn int_at(v: &serde_json::Value, path: &[&str]) -> Option<i64> {
    let mut cur = v;
    for p in path {
        cur = cur.get(p)?;
    }
    cur.as_i64()
}

/// Checks that a provider response is a success and parses its JSON body.
fn parse_ok(res: &forgebase_auth::social::http_client::HttpResponse) -> Result<serde_json::Value> {
    let v: serde_json::Value = serde_json::from_slice(&res.body).unwrap_or(serde_json::Value::Null);
    if (200..300).contains(&res.status) {
        Ok(v)
    } else {
        let msg = str_at(&v, &["error", "message"])
            .or_else(|| str_at(&v, &["error", "description"]))
            .unwrap_or("request refused");
        Err(BillingError::Provider(format!("HTTP {}: {msg}", res.status)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_matches_rfc_4231_case_2() {
        assert_eq!(
            hmac_hex(b"Jefe", b"what do ya want for nothing?"),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        assert!(same_signature("ab01", "AB01"));
        assert!(!same_signature("ab01", "ab02"));
        assert!(!same_signature("ab01", "ab0"));
    }
}
