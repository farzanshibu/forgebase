//! Billing helpers: merchant-of-record store receipts (`client.billing().store()`).

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::{Error, Result};
use crate::http::{Req, Transport, seg};

/// A store that is merchant of record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum StoreProvider {
    /// Apple App Store.
    AppStore,
    /// Google Play.
    GooglePlay,
    /// Creem (sells on the web).
    Creem,
}

impl StoreProvider {
    /// The provider name on the wire.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AppStore => "app_store",
            Self::GooglePlay => "google_play",
            Self::Creem => "creem",
        }
    }
}

/// What the app received from the store; each store reads its own fields.
#[derive(Debug, Clone, Default)]
pub struct StoreReceipt {
    /// App Store: the StoreKit 2 signed transaction (JWS).
    pub signed_transaction: Option<String>,
    /// App Store: the transaction id (read back with the App Store Server API).
    pub transaction_id: Option<String>,
    /// Google Play: the purchase token.
    pub purchase_token: Option<String>,
    /// Google Play: the product id.
    pub product_id: Option<String>,
    /// Google Play: `subscription` (default) or `one_time`.
    pub kind: Option<String>,
    /// Creem: the checkout id of the success redirect.
    pub checkout_id: Option<String>,
    /// Subject the purchase is for (default: the caller's).
    pub subject: Option<String>,
}

/// A hosted checkout request (Creem).
#[derive(Debug, Clone, Default)]
pub struct StoreCheckout {
    /// Price key mapped to a store product.
    pub price: String,
    /// Where the store returns to (`?checkout_id=` is appended by the store).
    pub success_url: String,
    /// Sent to the store as the request id; a replay returns the same checkout.
    pub idempotency_key: String,
    /// Subject (default: the caller's).
    pub subject: Option<String>,
}

/// A store purchase bound to a subject.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct StorePurchase {
    /// Id.
    pub id: String,
    /// Store.
    pub provider: String,
    /// Stable purchase id at the store.
    pub purchase_id: String,
    /// Subject.
    pub subject: String,
    /// Store state: `active`, `trialing`, `grace`, `on_hold`, `paused`, `expired`, `refunded`, `revoked`, `pending`, `replaced`.
    pub status: String,
    /// `subscription` or `one_time`.
    pub kind: String,
    /// Price key.
    pub price: String,
    /// Store product id.
    pub product_id: String,
    /// `test` (sandbox, licence tester, Creem test mode).
    pub environment: String,
    /// Renews at the period end.
    pub auto_renew: bool,
    /// End of the current period.
    #[serde(default)]
    pub expires_at: Option<String>,
    /// Newest store transaction.
    #[serde(default)]
    pub last_transaction_id: Option<String>,
    /// The billing order (one-time purchases).
    #[serde(default)]
    pub order_id: Option<String>,
    /// The billing subscription it drives.
    #[serde(default)]
    pub subscription_id: Option<String>,
    /// Created.
    pub created_at: String,
    /// Changed.
    pub updated_at: String,
}

/// A hosted page to send the customer to.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct HostedCheckout {
    /// Provider session id.
    pub session_id: String,
    /// Open this URL.
    pub url: String,
}

/// Billing: `client.billing()`.
#[derive(Clone)]
pub struct BillingApi {
    pub(crate) t: Arc<Transport>,
}

impl BillingApi {
    /// Store receipts, checkouts and purchases.
    #[must_use]
    pub fn store(&self) -> BillingStoreApi {
        BillingStoreApi { t: self.t.clone() }
    }
}

/// Merchant-of-record store helpers: `client.billing().store()`.
#[derive(Clone)]
pub struct BillingStoreApi {
    pub(crate) t: Arc<Transport>,
}

impl BillingStoreApi {
    /// Verifies a purchase server side with the store and applies it. Idempotent; a purchase bound
    /// to another account fails with a conflict (`purchase_owned`).
    pub async fn verify(&self, provider: StoreProvider, receipt: &StoreReceipt) -> Result<StorePurchase> {
        let body = json!({
            "signed_transaction": receipt.signed_transaction, "transaction_id": receipt.transaction_id,
            "purchase_token": receipt.purchase_token, "product_id": receipt.product_id, "kind": receipt.kind,
            "checkout_id": receipt.checkout_id, "subject": receipt.subject,
        });
        self.t
            .json(Req::post(format!("/api/v1/billing/store/{}/verify", seg(provider.as_str()))).json_value(body))
            .await
    }

    /// A hosted checkout at a store that sells on the web (Creem): open `url`, then `verify` with
    /// the returned `checkout_id`.
    pub async fn checkout(&self, provider: StoreProvider, input: &StoreCheckout) -> Result<HostedCheckout> {
        let body = json!({
            "price": input.price, "success_url": input.success_url,
            "idempotency_key": input.idempotency_key, "subject": input.subject,
        });
        self.t
            .json(Req::post(format!("/api/v1/billing/store/{}/checkout", seg(provider.as_str()))).json_value(body))
            .await
    }

    /// Store purchases of `subject` (default: the caller).
    pub async fn purchases(&self, subject: Option<&str>) -> Result<Vec<StorePurchase>> {
        let v: Value = self
            .t
            .value(Req::get("/api/v1/billing/store-purchases").q_opt("subject", subject))
            .await?;
        serde_json::from_value(v.get("items").cloned().unwrap_or(Value::Array(Vec::new())))
            .map_err(|e| Error::InvalidResponse(format!("store purchases: {e}")))
    }
}
