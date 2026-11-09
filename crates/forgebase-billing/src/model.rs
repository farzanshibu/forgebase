//! Wire and storage types of the billing service. Times are epoch
//! milliseconds inside and RFC 3339 on the wire; money is integer minor units
//! next to an ISO 4217 currency code.

use std::collections::BTreeMap;

use forgebase_entitlements::model::{ts, ts_opt};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use utoipa::ToSchema;

pub use crate::money::{Money, Scheme, Tier};

macro_rules! str_enum {
    ($(#[$m:meta])* $name:ident { $($(#[$vm:meta])* $v:ident = $s:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($(#[$vm])* $v),+ }

        impl $name {
            /// Stored name.
            pub fn as_str(self) -> &'static str {
                match self { $($name::$v => $s),+ }
            }

            /// Parses a stored name.
            pub fn parse(s: &str) -> Option<Self> {
                match s { $($s => Some($name::$v),)+ _ => None }
            }
        }
    };
}

str_enum!(
    /// What a paid line of a product delivers.
    ProductKind {
        /// Sets the FB-24 plan `plan_key` of the subject.
        Plan = "plan",
        /// Adds the FB-24 add-on `addon_key`.
        Addon = "addon",
        /// Grants the FB-24 usage pack `pack_key` per unit.
        UsagePack = "usage_pack",
        /// Tops up `credits` FB-73 credits per unit.
        Credits = "credits",
        /// Nothing automatic.
        Service = "service",
    }
);

str_enum!(
    /// How a price is charged.
    PriceType {
        /// In advance, each interval.
        Recurring = "recurring",
        /// Once (orders).
        OneTime = "one_time",
        /// In arrears, each interval, from usage events of `feature`.
        Metered = "metered",
    }
);

str_enum!(
    /// Billing interval.
    Interval {
        /// Calendar months from the cycle anchor.
        Month = "month",
        /// Years from the cycle anchor.
        Year = "year",
    }
);

str_enum!(
    /// Which usage units a metered price bills.
    MeterBasis {
        /// Every unit used.
        Units = "units",
        /// Only units past the plan's soft limit (FB-24 `overage_units`).
        Overage = "overage",
    }
);

str_enum!(
    /// Whether prices include tax.
    TaxBehavior {
        /// Tax is added to the price.
        Exclusive = "exclusive",
        /// The price includes tax.
        Inclusive = "inclusive",
    }
);

str_enum!(
    /// Subscription lifecycle.
    SubscriptionStatus {
        /// Created; the first invoice is not paid yet (not entitled).
        Incomplete = "incomplete",
        /// In a free trial (entitled).
        Trialing = "trialing",
        /// Paid and current.
        Active = "active",
        /// A renewal payment failed; dunning retries (still entitled).
        PastDue = "past_due",
        /// Paused: no invoices, not entitled.
        Paused = "paused",
        /// Ended.
        Canceled = "canceled",
    }
);

str_enum!(
    /// Invoice lifecycle.
    InvoiceStatus {
        /// Being built; lines can change.
        Draft = "draft",
        /// Finalized and numbered; awaiting payment.
        Open = "open",
        /// Paid.
        Paid = "paid",
        /// Cancelled before payment.
        Void = "void",
        /// Dunning gave up.
        Uncollectible = "uncollectible",
    }
);

str_enum!(
    /// One collection attempt.
    PaymentStatus {
        /// Waiting for the provider (async confirmation, hosted page).
        Pending = "pending",
        /// Collected.
        Succeeded = "succeeded",
        /// Declined or errored.
        Failed = "failed",
        /// Abandoned.
        Canceled = "canceled",
    }
);

str_enum!(
    /// Refund state.
    RefundStatus {
        /// Sent to the provider.
        Pending = "pending",
        /// Returned to the customer.
        Succeeded = "succeeded",
        /// The provider refused it.
        Failed = "failed",
    }
);

str_enum!(
    /// Coupon type.
    CouponKind {
        /// A percentage (basis points) off.
        Percent = "percent",
        /// A fixed amount off.
        Amount = "amount",
    }
);

str_enum!(
    /// How long a coupon applies to a subscription.
    CouponDuration {
        /// The first invoice.
        Once = "once",
        /// `duration_cycles` invoices.
        Repeating = "repeating",
        /// Every invoice.
        Forever = "forever",
    }
);

str_enum!(
    /// Invoice line type.
    LineKind {
        /// A recurring price for the period.
        Subscription = "subscription",
        /// Plan change credit or charge for the time left.
        Proration = "proration",
        /// Metered usage of the past period.
        Usage = "usage",
        /// A one-time price.
        OneTime = "one_time",
        /// Manual adjustment.
        Adjustment = "adjustment",
    }
);

str_enum!(
    /// Order lifecycle.
    OrderStatus {
        /// Awaiting payment.
        Open = "open",
        /// Paid and fulfilled (or being fulfilled).
        Paid = "paid",
        /// Abandoned.
        Canceled = "canceled",
        /// Fully refunded.
        Refunded = "refunded",
    }
);

str_enum!(
    /// How to bill a plan change.
    Proration {
        /// Credit the unused time and charge the new price for it now (default).
        AlwaysInvoice = "always_invoice",
        /// Add the proration lines to the next renewal invoice.
        CreateProrations = "create_prorations",
        /// No proration: the new price starts with the next period.
        None = "none",
    }
);

str_enum!(
    /// What dunning does after the last failed retry.
    FinalAction {
        /// Cancel the subscription and mark the invoice uncollectible.
        Cancel = "cancel",
        /// Keep it `past_due`, invoice uncollectible.
        LeavePastDue = "leave_past_due",
    }
);

// ---------------------------------------------------------------------------
// Catalog
// ---------------------------------------------------------------------------

/// A product.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Product {
    /// Id.
    pub id: String,
    /// Stable key.
    pub key: String,
    /// Display name.
    pub name: String,
    /// Description.
    pub description: Option<String>,
    /// What a paid line delivers.
    pub kind: ProductKind,
    /// FB-24 plan (kind `plan`).
    pub plan_key: Option<String>,
    /// FB-24 add-on (kind `addon`).
    pub addon_key: Option<String>,
    /// FB-24 usage pack (kind `usage_pack`).
    pub pack_key: Option<String>,
    /// Credits per unit (kind `credits`).
    pub credits: Option<i64>,
    /// Sellable.
    pub active: bool,
    /// Caller data.
    #[schema(value_type = Object)]
    pub metadata: Json,
    /// Created.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub created_at: i64,
    /// Changed.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub updated_at: i64,
}

/// Body of `PUT /billing/products/{key}`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ProductInput {
    /// Display name.
    pub name: String,
    /// Description.
    #[serde(default)]
    pub description: Option<String>,
    /// What a paid line delivers.
    pub kind: ProductKind,
    /// FB-24 plan key (kind `plan`).
    #[serde(default)]
    pub plan_key: Option<String>,
    /// FB-24 add-on key (kind `addon`).
    #[serde(default)]
    pub addon_key: Option<String>,
    /// FB-24 pack key (kind `usage_pack`).
    #[serde(default)]
    pub pack_key: Option<String>,
    /// Credits per unit (kind `credits`).
    #[serde(default)]
    pub credits: Option<i64>,
    /// Sellable (default true).
    #[serde(default = "yes")]
    pub active: bool,
    /// Caller data.
    #[serde(default)]
    #[schema(value_type = Option<Object>)]
    pub metadata: Option<Json>,
}

fn yes() -> bool {
    true
}

fn one() -> i64 {
    1
}

/// The amounts of a price in one more currency.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CurrencyOption {
    /// Price per unit, minor units.
    #[serde(default)]
    pub unit_amount: i64,
    /// Tiers (tiered schemes).
    #[serde(default)]
    pub tiers: Option<Vec<Tier>>,
}

/// A price of a product.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Price {
    /// Id.
    pub id: String,
    /// Stable key.
    pub key: String,
    /// Product id.
    pub product_id: String,
    /// Product key.
    pub product: String,
    /// How it is charged.
    #[serde(rename = "type")]
    pub price_type: PriceType,
    /// Interval (recurring and metered).
    pub interval: Option<Interval>,
    /// Intervals per period.
    pub interval_count: i64,
    /// Quantity to amount.
    pub scheme: Scheme,
    /// Primary currency.
    pub currency: String,
    /// Price per unit in the primary currency (per-unit scheme).
    pub unit_amount: i64,
    /// Tiers in the primary currency.
    pub tiers: Option<Vec<Tier>>,
    /// The same price in other currencies.
    pub currency_options: BTreeMap<String, CurrencyOption>,
    /// Metered: the FB-24 feature whose usage events are billed.
    pub feature: Option<String>,
    /// Metered: all units or only overage.
    pub meter_basis: MeterBasis,
    /// Free trial days when subscribing.
    pub trial_days: i64,
    /// Whether the amounts include tax.
    pub tax_behavior: TaxBehavior,
    /// Sellable.
    pub active: bool,
    /// Caller data.
    #[schema(value_type = Object)]
    pub metadata: Json,
    /// Created.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub created_at: i64,
}

impl Price {
    /// The amounts in `currency` (`None`: not sold in it).
    pub fn amounts(&self, currency: &str) -> Option<(i64, Vec<Tier>)> {
        if self.currency == currency {
            return Some((self.unit_amount, self.tiers.clone().unwrap_or_default()));
        }
        self.currency_options
            .get(currency)
            .map(|o| (o.unit_amount, o.tiers.clone().unwrap_or_default()))
    }

    /// Currencies the price is sold in.
    pub fn currencies(&self) -> Vec<String> {
        let mut v = vec![self.currency.clone()];
        v.extend(self.currency_options.keys().cloned());
        v
    }
}

/// Body of `PUT /billing/prices/{key}`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PriceInput {
    /// Product key.
    pub product: String,
    /// How it is charged.
    #[serde(rename = "type")]
    pub price_type: PriceType,
    /// Interval (recurring and metered).
    #[serde(default)]
    pub interval: Option<Interval>,
    /// Intervals per period (default 1).
    #[serde(default = "one")]
    pub interval_count: i64,
    /// Quantity to amount (default `per_unit`).
    #[serde(default)]
    pub scheme: Option<Scheme>,
    /// Primary currency.
    pub currency: String,
    /// Price per unit, minor units.
    #[serde(default)]
    pub unit_amount: i64,
    /// Tiers (graduated and volume).
    #[serde(default)]
    pub tiers: Option<Vec<Tier>>,
    /// The same price in other currencies.
    #[serde(default)]
    pub currency_options: BTreeMap<String, CurrencyOption>,
    /// Metered: FB-24 feature key.
    #[serde(default)]
    pub feature: Option<String>,
    /// Metered: `units` (default) or `overage`.
    #[serde(default)]
    pub meter_basis: Option<MeterBasis>,
    /// Free trial days.
    #[serde(default)]
    pub trial_days: i64,
    /// Default `exclusive`.
    #[serde(default)]
    pub tax_behavior: Option<TaxBehavior>,
    /// Sellable (default true).
    #[serde(default = "yes")]
    pub active: bool,
    /// Caller data.
    #[serde(default)]
    #[schema(value_type = Option<Object>)]
    pub metadata: Option<Json>,
}

/// A tax rate of a region.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct TaxRate {
    /// Id.
    pub id: String,
    /// Region (`IN`, `DE`, `US-CA`).
    pub region: String,
    /// Name on invoices (`GST`, `VAT`).
    pub name: String,
    /// Rate in basis points (1800 = 18 %).
    pub rate_bp: i64,
    /// In use.
    pub active: bool,
}

/// Body of `PUT /billing/tax-rates/{region}`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TaxRateInput {
    /// Name on invoices.
    pub name: String,
    /// Basis points (0..=10000).
    pub rate_bp: i64,
    /// In use (default true).
    #[serde(default = "yes")]
    pub active: bool,
}

/// A coupon.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Coupon {
    /// Id.
    pub id: String,
    /// Code customers enter.
    pub code: String,
    /// Name.
    pub name: Option<String>,
    /// Percent or amount.
    pub kind: CouponKind,
    /// Percent off in basis points.
    pub percent_bp: Option<i64>,
    /// Amount off, minor units of `currency`.
    pub amount_off: Option<i64>,
    /// Currency of `amount_off`.
    pub currency: Option<String>,
    /// How long it applies.
    pub duration: CouponDuration,
    /// Invoices (repeating).
    pub duration_cycles: Option<i64>,
    /// Redemption cap.
    pub max_redemptions: Option<i64>,
    /// Times redeemed.
    pub redeemed: i64,
    /// Last day it can be redeemed.
    #[serde(with = "ts_opt")]
    #[schema(value_type = Option<String>)]
    pub expires_at: Option<i64>,
    /// Product keys it applies to (empty: all).
    pub products: Vec<String>,
    /// Redeemable.
    pub active: bool,
}

/// Body of `PUT /billing/coupons/{code}`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CouponInput {
    /// Name.
    #[serde(default)]
    pub name: Option<String>,
    /// Percent or amount.
    pub kind: CouponKind,
    /// Percent off in basis points (kind `percent`).
    #[serde(default)]
    pub percent_bp: Option<i64>,
    /// Amount off (kind `amount`).
    #[serde(default)]
    pub amount_off: Option<i64>,
    /// Currency of `amount_off`.
    #[serde(default)]
    pub currency: Option<String>,
    /// Default `once`.
    #[serde(default)]
    pub duration: Option<CouponDuration>,
    /// Invoices (repeating).
    #[serde(default)]
    pub duration_cycles: Option<i64>,
    /// Redemption cap.
    #[serde(default)]
    pub max_redemptions: Option<i64>,
    /// Last moment it can be redeemed.
    #[serde(default, with = "ts_opt")]
    #[schema(value_type = Option<String>)]
    pub expires_at: Option<i64>,
    /// Product keys it applies to (default: all).
    #[serde(default)]
    pub products: Vec<String>,
    /// Redeemable (default true).
    #[serde(default = "yes")]
    pub active: bool,
}

// ---------------------------------------------------------------------------
// Customers and subscriptions
// ---------------------------------------------------------------------------

/// A customer: one billing subject (`org:<id>` or a principal key).
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Customer {
    /// Id.
    pub id: String,
    /// Subject billed.
    pub subject: String,
    /// Billing email.
    pub email: Option<String>,
    /// Billing name.
    pub name: Option<String>,
    /// Currency of invoices.
    pub currency: String,
    /// Tax region.
    pub region: Option<String>,
    /// No tax is charged.
    pub tax_exempt: bool,
    /// Payment provider (`stripe`, `razorpay`, `manual`).
    pub provider: Option<String>,
    /// The customer at the provider.
    pub provider_customer_id: Option<String>,
    /// A saved payment method is on file (a provider reference, never card data).
    pub has_payment_method: bool,
    /// Its label (`visa ••4242`).
    pub payment_method_label: Option<String>,
    /// Credit owed to the customer, minor units of `currency`.
    pub balance: i64,
    /// Caller data.
    #[schema(value_type = Object)]
    pub metadata: Json,
    /// Created.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub created_at: i64,
}

/// Body of `PUT /billing/customers`.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CustomerInput {
    /// Subject (default: the caller's).
    #[serde(default)]
    pub subject: Option<String>,
    /// Billing email.
    #[serde(default)]
    pub email: Option<String>,
    /// Billing name.
    #[serde(default)]
    pub name: Option<String>,
    /// Currency of invoices (default: the settings' default currency; fixed
    /// once a subscription or order exists).
    #[serde(default)]
    pub currency: Option<String>,
    /// Tax region (`IN`, `US-CA`).
    #[serde(default)]
    pub region: Option<String>,
    /// Payment provider (default: the only configured one).
    #[serde(default)]
    pub provider: Option<String>,
    /// Operators: tax exempt.
    #[serde(default)]
    pub tax_exempt: Option<bool>,
    /// Caller data.
    #[serde(default)]
    #[schema(value_type = Option<Object>)]
    pub metadata: Option<Json>,
}

/// One price on a subscription.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct SubscriptionItem {
    /// Id.
    pub id: String,
    /// Price key.
    pub price: String,
    /// Product key.
    pub product: String,
    /// Units (seats).
    pub quantity: i64,
}

/// A subscription.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Subscription {
    /// Id.
    pub id: String,
    /// Customer id.
    pub customer_id: String,
    /// Subject billed.
    pub subject: String,
    /// Lifecycle state.
    pub status: SubscriptionStatus,
    /// Currency.
    pub currency: String,
    /// Interval.
    pub interval: Interval,
    /// Intervals per period.
    pub interval_count: i64,
    /// Cycle anchor (renewals keep its day of month and time).
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub cycle_anchor: i64,
    /// Current period start.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub current_period_start: i64,
    /// Current period end.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub current_period_end: i64,
    /// End of the trial.
    #[serde(with = "ts_opt")]
    #[schema(value_type = Option<String>)]
    pub trial_ends_at: Option<i64>,
    /// Ends at the period end.
    pub cancel_at_period_end: bool,
    /// When cancellation was asked for.
    #[serde(with = "ts_opt")]
    #[schema(value_type = Option<String>)]
    pub canceled_at: Option<i64>,
    /// When it ended.
    #[serde(with = "ts_opt")]
    #[schema(value_type = Option<String>)]
    pub ended_at: Option<i64>,
    /// Why it was cancelled (`requested`, `payment_failed`, ...).
    pub cancel_reason: Option<String>,
    /// Coupon code in force.
    pub coupon: Option<String>,
    /// Discounted invoices left (repeating coupons).
    pub coupon_cycles_left: Option<i64>,
    /// Price scheduled for the next period (downgrade at period end).
    pub pending_price: Option<String>,
    /// Payment provider.
    pub provider: Option<String>,
    /// Newest invoice.
    pub latest_invoice_id: Option<String>,
    /// Prices.
    pub items: Vec<SubscriptionItem>,
    /// Changes so far.
    pub version: i64,
    /// Caller data.
    #[schema(value_type = Object)]
    pub metadata: Json,
    /// Created.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub created_at: i64,
}

/// A price and a quantity.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ItemInput {
    /// Price key.
    pub price: String,
    /// Units (default 1).
    #[serde(default = "one")]
    pub quantity: i64,
}

impl ItemInput {
    /// One unit of `price`.
    pub fn new(price: impl Into<String>) -> Self {
        Self {
            price: price.into(),
            quantity: 1,
        }
    }
}

/// Body of `POST /billing/subscriptions`.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SubscribeInput {
    /// Subject (default: the caller's).
    #[serde(default)]
    pub subject: Option<String>,
    /// Prices (one recurring plan price, add-ons, metered prices).
    pub items: Vec<ItemInput>,
    /// Coupon code.
    #[serde(default)]
    pub coupon: Option<String>,
    /// Trial days (default: the plan price's `trial_days`; 0 = none).
    #[serde(default)]
    pub trial_days: Option<i64>,
    /// A replay with the same key returns the first subscription.
    pub idempotency_key: String,
    /// Where the hosted payment page returns to (no saved payment method).
    #[serde(default)]
    pub return_url: Option<String>,
    /// Caller data.
    #[serde(default)]
    #[schema(value_type = Option<Object>)]
    pub metadata: Option<Json>,
}

/// Body of `POST /billing/subscriptions/{id}/change`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ChangePlanInput {
    /// New plan price key (replaces the subscription's recurring plan price).
    pub price: String,
    /// Units (default: unchanged).
    #[serde(default)]
    pub quantity: Option<i64>,
    /// Default `always_invoice`.
    #[serde(default)]
    pub proration: Option<Proration>,
    /// Switch at the end of the period instead (no proration).
    #[serde(default)]
    pub at_period_end: bool,
}

/// Body of `POST /billing/subscriptions/{id}/cancel`.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CancelInput {
    /// End at the period end (default) or now.
    #[serde(default = "yes")]
    pub at_period_end: bool,
    /// Why.
    #[serde(default)]
    pub reason: Option<String>,
}

/// What a subscription, order or plan change returned: the object, the
/// invoice it produced, and where to pay when no payment method is saved.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Checkout {
    /// The subscription (subscriptions and plan changes).
    pub subscription: Option<Subscription>,
    /// The order (one-time purchases).
    pub order: Option<Order>,
    /// The invoice raised.
    pub invoice: Option<Invoice>,
    /// The provider's hosted payment page, when the customer must pay there.
    pub payment_url: Option<String>,
    /// The idempotency key was seen before: this is the first result.
    pub replayed: bool,
}

/// A one-time order.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Order {
    /// Id.
    pub id: String,
    /// Customer id.
    pub customer_id: String,
    /// Subject.
    pub subject: String,
    /// State.
    pub status: OrderStatus,
    /// Currency.
    pub currency: String,
    /// Invoice.
    pub invoice_id: Option<String>,
    /// Created.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub created_at: i64,
}

/// Body of `POST /billing/orders`.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct OrderInput {
    /// Subject (default: the caller's).
    #[serde(default)]
    pub subject: Option<String>,
    /// One-time prices.
    pub items: Vec<ItemInput>,
    /// Coupon code.
    #[serde(default)]
    pub coupon: Option<String>,
    /// A replay with the same key returns the first order.
    pub idempotency_key: String,
    /// Where the hosted payment page returns to.
    #[serde(default)]
    pub return_url: Option<String>,
}

// ---------------------------------------------------------------------------
// Invoices, payments, refunds
// ---------------------------------------------------------------------------

/// An invoice line.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct InvoiceLine {
    /// Id.
    pub id: String,
    /// Type.
    pub kind: LineKind,
    /// Text on the invoice.
    pub description: String,
    /// Product key.
    pub product: Option<String>,
    /// Price key.
    pub price: Option<String>,
    /// Metered feature.
    pub feature: Option<String>,
    /// Units.
    pub quantity: i64,
    /// Price per unit (per-unit prices).
    pub unit_amount: Option<i64>,
    /// Signed amount before discount (credits are negative).
    pub amount: i64,
    /// Discount on the line.
    pub discount_amount: i64,
    /// Tax rate in basis points.
    pub tax_rate_bp: i64,
    /// Tax of the line.
    pub tax_amount: i64,
    /// Tax is part of `amount`.
    pub tax_inclusive: bool,
    /// Service period start.
    #[serde(with = "ts_opt")]
    #[schema(value_type = Option<String>)]
    pub period_start: Option<i64>,
    /// Service period end.
    #[serde(with = "ts_opt")]
    #[schema(value_type = Option<String>)]
    pub period_end: Option<i64>,
}

/// An invoice.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Invoice {
    /// Id.
    pub id: String,
    /// Number (set when finalized).
    pub number: Option<String>,
    /// Customer id.
    pub customer_id: String,
    /// Subject.
    pub subject: String,
    /// Subscription.
    pub subscription_id: Option<String>,
    /// Order.
    pub order_id: Option<String>,
    /// State.
    pub status: InvoiceStatus,
    /// `subscription_create`, `subscription_cycle`, `subscription_update`, `order`.
    pub billing_reason: String,
    /// Currency.
    pub currency: String,
    /// Sum of the lines before discount (exclusive of added tax).
    pub subtotal: i64,
    /// Discount.
    pub discount: i64,
    /// Tax (added and included).
    pub tax: i64,
    /// subtotal − discount + exclusive tax.
    pub total: i64,
    /// Customer credit applied.
    pub balance_applied: i64,
    /// Left to collect.
    pub amount_due: i64,
    /// Collected.
    pub amount_paid: i64,
    /// Given back by credit notes.
    pub amount_refunded: i64,
    /// Coupon code applied.
    pub coupon: Option<String>,
    /// Tax region.
    pub tax_region: Option<String>,
    /// Period start.
    #[serde(with = "ts_opt")]
    #[schema(value_type = Option<String>)]
    pub period_start: Option<i64>,
    /// Period end.
    #[serde(with = "ts_opt")]
    #[schema(value_type = Option<String>)]
    pub period_end: Option<i64>,
    /// Collection attempts.
    pub attempt_count: i64,
    /// Next dunning retry.
    #[serde(with = "ts_opt")]
    #[schema(value_type = Option<String>)]
    pub next_attempt_at: Option<i64>,
    /// Hosted payment page.
    pub hosted_url: Option<String>,
    /// Finalized.
    #[serde(with = "ts_opt")]
    #[schema(value_type = Option<String>)]
    pub finalized_at: Option<i64>,
    /// Paid.
    #[serde(with = "ts_opt")]
    #[schema(value_type = Option<String>)]
    pub paid_at: Option<i64>,
    /// What was paid for was delivered.
    #[serde(with = "ts_opt")]
    #[schema(value_type = Option<String>)]
    pub fulfilled_at: Option<i64>,
    /// Lines.
    pub lines: Vec<InvoiceLine>,
    /// Created.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub created_at: i64,
}

/// A payment attempt.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Payment {
    /// Id.
    pub id: String,
    /// Invoice.
    pub invoice_id: String,
    /// Subject.
    pub subject: String,
    /// Provider.
    pub provider: String,
    /// Payment id at the provider.
    pub provider_ref: Option<String>,
    /// State.
    pub status: PaymentStatus,
    /// Amount.
    pub amount: i64,
    /// Currency.
    pub currency: String,
    /// Refunded so far.
    pub amount_refunded: i64,
    /// Decline code.
    pub failure_code: Option<String>,
    /// Decline message.
    pub failure_message: Option<String>,
    /// Attempt number of the invoice.
    pub attempt: i64,
    /// Created.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub created_at: i64,
    /// Changed.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub updated_at: i64,
}

/// A refund.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Refund {
    /// Id.
    pub id: String,
    /// Payment refunded.
    pub payment_id: String,
    /// Invoice.
    pub invoice_id: String,
    /// Credit note.
    pub credit_note_id: Option<String>,
    /// Provider.
    pub provider: String,
    /// Refund id at the provider.
    pub provider_ref: Option<String>,
    /// State.
    pub status: RefundStatus,
    /// Amount.
    pub amount: i64,
    /// Currency.
    pub currency: String,
    /// Why.
    pub reason: Option<String>,
    /// Created.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub created_at: i64,
}

/// A credit note.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct CreditNote {
    /// Id.
    pub id: String,
    /// Number.
    pub number: String,
    /// Invoice credited.
    pub invoice_id: String,
    /// Subject.
    pub subject: String,
    /// Currency.
    pub currency: String,
    /// Amount credited.
    pub amount: i64,
    /// Of which refunded to the payment method.
    pub refund_amount: i64,
    /// Of which credited to the customer balance.
    pub balance_amount: i64,
    /// Why.
    pub reason: Option<String>,
    /// The refund settling it.
    pub refund: Option<Refund>,
    /// FB-73 credits taken back (credit top-ups refunded).
    pub credits_reversed: i64,
    /// Created.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub created_at: i64,
}

/// Body of `POST /billing/invoices/{id}/credit-notes`.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreditNoteInput {
    /// Amount (default: all that is left to credit).
    #[serde(default)]
    pub amount: Option<i64>,
    /// Credit the customer balance instead of refunding the payment method.
    #[serde(default)]
    pub to_balance: bool,
    /// Why.
    #[serde(default)]
    pub reason: Option<String>,
    /// A replay with the same key returns the first credit note.
    pub idempotency_key: String,
}

/// The result of a collection attempt.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Collection {
    /// The invoice after the attempt.
    pub invoice: Invoice,
    /// The payment made.
    pub payment: Option<Payment>,
    /// The hosted page where the customer must pay.
    pub payment_url: Option<String>,
}

/// Body of `POST /billing/portal/payment-method`.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PaymentMethodInput {
    /// Subject (default: the caller's).
    #[serde(default)]
    pub subject: Option<String>,
    /// Where the provider page returns to.
    #[serde(default)]
    pub return_url: Option<String>,
}

/// A provider-hosted page.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HostedPage {
    /// Open this URL.
    pub url: String,
    /// Provider session id.
    pub session_id: String,
}

// ---------------------------------------------------------------------------
// Providers, webhooks, settings
// ---------------------------------------------------------------------------

/// A configured payment provider (secrets are never returned).
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct ProviderAccount {
    /// `stripe`, `razorpay` or `manual`.
    pub provider: String,
    /// Public key id (Razorpay).
    pub key_id: Option<String>,
    /// Last characters of the API secret.
    pub hint: Option<String>,
    /// API base URL override.
    pub api_base: Option<String>,
    /// In use.
    pub enabled: bool,
    /// Webhook URL to register at the provider (relative to the public URL).
    pub webhook_path: String,
    /// Changed.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub updated_at: i64,
}

/// Body of `PUT /billing/providers/{provider}`. Test-mode credentials only.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ProviderInput {
    /// Stripe secret key (`sk_test_…`) or Razorpay key secret.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Razorpay key id (`rzp_test_…`).
    #[serde(default)]
    pub key_id: Option<String>,
    /// Webhook signing secret.
    #[serde(default)]
    pub webhook_secret: Option<String>,
    /// API base URL (default: the provider's).
    #[serde(default)]
    pub api_base: Option<String>,
    /// In use (default true).
    #[serde(default = "yes")]
    pub enabled: bool,
}

/// A webhook event received.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct WebhookEvent {
    /// Id.
    pub id: String,
    /// Provider.
    pub provider: String,
    /// Event id at the provider.
    pub event_id: String,
    /// Event type.
    pub event_type: String,
    /// Provider time of the event.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub event_at: i64,
    /// Received.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub received_at: i64,
    /// `processed`, `ignored` or `failed`.
    pub status: String,
    /// What happened.
    pub result: Option<String>,
    /// Deliveries seen.
    pub attempts: i64,
}

/// What a webhook delivery did.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct WebhookOutcome {
    /// `processed`, `ignored`, `duplicate` (already handled) or `failed`.
    pub result: String,
    /// Details (`stale`, `unknown_payment`, ...).
    pub detail: Option<String>,
    /// Event id.
    pub event_id: String,
}

/// Operator settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct BillingSettings {
    /// Currency of new customers.
    pub default_currency: String,
    /// Days after the first failed attempt at which payment is retried
    /// (`[1, 3, 5]`: retries 1, 3 and 5 days after the failure).
    pub retry_days: Vec<i64>,
    /// After the last retry.
    pub final_action: FinalAction,
    /// Invoice number prefix.
    pub invoice_prefix: String,
    /// Credit note number prefix.
    pub credit_note_prefix: String,
    /// Days an unpaid first invoice keeps an `incomplete` subscription.
    pub incomplete_days: i64,
    /// Webhooks older than this are ignored (replay window).
    pub webhook_max_age_secs: i64,
    /// Seconds a signed webhook timestamp may differ from now (Stripe).
    pub webhook_tolerance_secs: i64,
}

impl Default for BillingSettings {
    fn default() -> Self {
        Self {
            default_currency: "USD".into(),
            retry_days: vec![1, 3, 5],
            final_action: FinalAction::Cancel,
            invoice_prefix: "INV".into(),
            credit_note_prefix: "CN".into(),
            incomplete_days: 1,
            webhook_max_age_secs: 3 * 86_400,
            webhook_tolerance_secs: 300,
        }
    }
}

/// What a maintenance run did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, ToSchema)]
pub struct MaintenanceReport {
    /// Usage events read.
    pub usage_events_read: i64,
    /// Usage records stored for metered prices.
    pub usage_recorded: i64,
    /// Trials converted or ended.
    pub trials_ended: i64,
    /// Subscriptions renewed (invoice raised).
    pub renewed: i64,
    /// Subscriptions ended (cancel at period end, dunning, incomplete).
    pub ended: i64,
    /// Dunning retries attempted.
    pub retries: i64,
    /// Invoices fulfilled (late fulfillment).
    pub fulfilled: i64,
    /// Entitlement syncs applied.
    pub synced: i64,
}

/// A page of objects.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Page<T> {
    /// Items, newest first.
    pub items: Vec<T>,
}

// ---------------------------------------------------------------------------
// Statistics
// ---------------------------------------------------------------------------

/// Monthly and annual recurring revenue of one currency.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct RecurringRevenue {
    /// Currency.
    pub currency: String,
    /// Monthly recurring revenue (yearly prices / 12, after coupons).
    pub mrr: i64,
    /// MRR × 12.
    pub arr: i64,
    /// Subscriptions counted (active and past due).
    pub subscriptions: i64,
}

/// Revenue of one currency in one bucket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct RevenuePoint {
    /// Bucket start.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub start: i64,
    /// Currency.
    pub currency: String,
    /// Payments collected.
    pub gross: i64,
    /// Refunds.
    pub refunded: i64,
    /// gross − refunded.
    pub net: i64,
    /// Successful payments.
    pub payments: i64,
}

/// Payment statistics over a window.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct BillingStats {
    /// Window start.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub from: i64,
    /// Window end.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub to: i64,
    /// MRR and ARR per currency, now.
    pub recurring: Vec<RecurringRevenue>,
    /// Subscriptions per status, now.
    pub by_status: BTreeMap<String, i64>,
    /// Subscriptions active at the window start.
    pub active_at_start: i64,
    /// Subscriptions started in the window.
    pub new_subscriptions: i64,
    /// Subscriptions that ended in the window.
    pub churned: i64,
    /// churned / active_at_start in basis points (`null`: none at start).
    pub churn_rate_bp: Option<i64>,
    /// Revenue per currency and bucket.
    pub revenue: Vec<RevenuePoint>,
    /// Payments that failed in the window.
    pub failed_payments: i64,
}

/// The customer portal: what a customer sees of its billing.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Portal {
    /// The customer (`null`: never billed).
    pub customer: Option<Customer>,
    /// Subscriptions that have not ended.
    pub subscriptions: Vec<Subscription>,
    /// Newest invoices.
    pub invoices: Vec<Invoice>,
    /// Newest payments.
    pub payments: Vec<Payment>,
    /// Newest credit notes.
    pub credit_notes: Vec<CreditNote>,
}

/// Body of `POST /billing/invoices/{id}/mark-paid` (manual billing).
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MarkPaidInput {
    /// Bank transfer or cheque reference.
    #[serde(default)]
    pub reference: Option<String>,
}

/// Body of `POST /billing/invoices/{id}/pay`.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PayInput {
    /// Where the hosted payment page returns to.
    #[serde(default)]
    pub return_url: Option<String>,
}
