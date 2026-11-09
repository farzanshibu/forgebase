//! Merchant-of-record stores (FB-72): App Store, Google Play and Creem own
//! the customer's payment and its renewals; ForgeBase verifies what the
//! store says (server side) and mirrors it as billing subscriptions, orders,
//! invoices, refunds and FB-24 entitlements.
//!
//! A [`StoreProvider`] turns a purchase claim from an app (a transaction id,
//! a signed transaction, a purchase token, a checkout id) or a server
//! notification into a normalised [`StorePurchase`]. The billing side
//! ([`crate::BillingService`], `store.rs`) does the mapping.

use std::fmt;

use forgebase_db::backend::BoxFuture;

use crate::error::{BillingError, Result};
use crate::model::HostedPage;

/// What was bought.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PurchaseKind {
    /// An auto-renewing subscription.
    Subscription,
    /// A one-time product (consumable or not).
    OneTime,
}

impl PurchaseKind {
    /// Stored name.
    pub fn as_str(self) -> &'static str {
        match self {
            PurchaseKind::Subscription => "subscription",
            PurchaseKind::OneTime => "one_time",
        }
    }

    /// Parses a stored name.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "subscription" => Some(PurchaseKind::Subscription),
            "one_time" => Some(PurchaseKind::OneTime),
            _ => None,
        }
    }
}

/// The store's state of a purchase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreState {
    /// Paid and in force.
    Active,
    /// In a free trial.
    Trialing,
    /// A renewal failed; the store retries and access continues.
    Grace,
    /// A renewal failed; access is suspended while the store retries.
    OnHold,
    /// Paused by the customer.
    Paused,
    /// Ended (not renewed).
    Expired,
    /// Refunded by the store.
    Refunded,
    /// Revoked (family sharing removed, chargeback, voided).
    Revoked,
    /// Not paid yet (deferred payment).
    Pending,
}

impl StoreState {
    /// Stored name.
    pub fn as_str(self) -> &'static str {
        match self {
            StoreState::Active => "active",
            StoreState::Trialing => "trialing",
            StoreState::Grace => "grace",
            StoreState::OnHold => "on_hold",
            StoreState::Paused => "paused",
            StoreState::Expired => "expired",
            StoreState::Refunded => "refunded",
            StoreState::Revoked => "revoked",
            StoreState::Pending => "pending",
        }
    }

    /// Access continues.
    pub fn live(self) -> bool {
        matches!(self, StoreState::Active | StoreState::Trialing | StoreState::Grace)
    }

    /// Money was taken back or access withdrawn by the store.
    pub fn reversed(self) -> bool {
        matches!(self, StoreState::Refunded | StoreState::Revoked)
    }
}

/// A refund the store made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreRefund {
    /// The transaction refunded.
    pub transaction_id: String,
    /// Refund reference at the store (idempotency key of the credit note).
    pub refund_ref: String,
    /// Minor units refunded (`None`: the whole transaction).
    pub amount: Option<i64>,
}

/// A purchase as the store reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorePurchase {
    /// Stable id of the purchase: App Store original transaction id, Google
    /// purchase token, Creem subscription or order id.
    pub purchase_id: String,
    /// This payment (renewals have their own): App Store transaction id,
    /// Google order id, Creem transaction or order id.
    pub transaction_id: Option<String>,
    /// Store product id.
    pub product_id: String,
    /// Subscription or one-time.
    pub kind: PurchaseKind,
    /// State.
    pub state: StoreState,
    /// Renews at the period end.
    pub auto_renew: bool,
    /// Current period start, epoch ms.
    pub period_start: Option<i64>,
    /// Current period end (subscriptions), epoch ms.
    pub expires_at: Option<i64>,
    /// Purchase time of this transaction, epoch ms.
    pub purchased_at: i64,
    /// Price paid, minor units, when the store says.
    pub amount: Option<i64>,
    /// Currency of `amount`, ISO 4217.
    pub currency: Option<String>,
    /// Units bought.
    pub quantity: i64,
    /// Bought in the store's test environment (sandbox, licence tester).
    pub test: bool,
    /// Subject the purchase was made for, when the store carries a value
    /// ForgeBase signed itself (Creem checkout metadata). App-supplied
    /// account tokens are not trusted for this.
    pub subject_hint: Option<String>,
    /// The purchase this one replaces (Google upgrades, Creem plan changes).
    pub replaces: Option<String>,
    /// A refund the store made on this purchase.
    pub refund: Option<StoreRefund>,
    /// The report only carries the refund: the purchase state is left as it
    /// is (Creem sends state changes as separate events).
    pub refund_only: bool,
    /// Store time of this state, epoch ms (out-of-order protection).
    pub observed_at: i64,
    /// Needs an acknowledgement (Google Play: refunded after 3 days otherwise).
    pub needs_ack: bool,
}

/// What an app sends to have a purchase verified. Each store reads its own
/// fields.
#[derive(Debug, Clone, Default)]
pub struct PurchaseClaim {
    /// App Store transaction id (looked up with the App Store Server API).
    pub transaction_id: Option<String>,
    /// App Store signed transaction (StoreKit 2 `jwsRepresentation`).
    pub signed_transaction: Option<String>,
    /// Google Play purchase token.
    pub purchase_token: Option<String>,
    /// Google Play product id (subscription or in-app product).
    pub product_id: Option<String>,
    /// Google Play: subscription or one-time.
    pub kind: Option<PurchaseKind>,
    /// Creem checkout id (from the success redirect).
    pub checkout_id: Option<String>,
}

/// A verified server notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreNotification {
    /// Id at the store (deduplication): App Store notification UUID, Pub/Sub
    /// message id, Creem event id.
    pub id: String,
    /// Type (`DID_RENEW`, `SUBSCRIPTION_RENEWED`, `subscription.paid`, ...).
    pub event_type: String,
    /// Store time, epoch ms.
    pub created_at: i64,
    /// The purchase state it carries; `None` for test pings and types
    /// billing does not act on.
    pub purchase: Option<StorePurchase>,
}

/// A hosted checkout at a store (Creem).
#[derive(Debug, Clone, Default)]
pub struct StoreCheckoutRequest {
    /// Store product id.
    pub product_id: String,
    /// Billing subject (signed back in the webhooks).
    pub subject: String,
    /// Customer email.
    pub email: Option<String>,
    /// Where the store returns to.
    pub success_url: String,
    /// Idempotency key.
    pub request_id: String,
}

/// A merchant-of-record store driver. Test environments only.
pub trait StoreProvider: Send + Sync + fmt::Debug {
    /// `app_store`, `google_play`, `creem`.
    fn name(&self) -> &'static str;
    /// Verifies a purchase an app reports, with the store.
    fn verify_purchase<'a>(&'a self, claim: &'a PurchaseClaim) -> BoxFuture<'a, Result<StorePurchase>>;
    /// Reads a known purchase again (renewal safety net).
    fn refresh<'a>(
        &'a self,
        purchase_id: &'a str,
        product_id: &'a str,
        kind: PurchaseKind,
    ) -> BoxFuture<'a, Result<StorePurchase>>;
    /// Verifies a server notification (signature, origin) and reads the
    /// purchase it is about. Header names are lower case.
    fn verify_notification<'a>(
        &'a self,
        headers: &'a [(String, String)],
        body: &'a [u8],
        now_ms: i64,
    ) -> BoxFuture<'a, Result<StoreNotification>>;
    /// A hosted checkout (Creem); stores sell inside the app.
    fn checkout<'a>(&'a self, _req: &'a StoreCheckoutRequest) -> BoxFuture<'a, Result<HostedPage>> {
        Box::pin(async move {
            Err(BillingError::invalid(
                "not_supported",
                "this store sells inside the app; verify the purchase instead",
            ))
        })
    }
    /// Acknowledges a verified purchase (Google Play).
    fn acknowledge<'a>(&'a self, _p: &'a StorePurchase) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move { Ok(()) })
    }
}

/// Refuses a purchase made outside the store's test environment.
pub(crate) fn require_test(p: &StorePurchase, store: &str) -> Result<()> {
    if p.test {
        Ok(())
    } else {
        Err(BillingError::invalid(
            "live_mode_refused",
            format!("only {store} test purchases are accepted"),
        ))
    }
}
