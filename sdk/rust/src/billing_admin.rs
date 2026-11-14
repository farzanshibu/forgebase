//! Billing beyond the stores (`/api/v1/billing/*`): customers, subscriptions, orders, invoices,
//! the catalog (products, prices, coupons, tax rates), providers, settings, the operator views
//! (stats, usage meters, dunning, maintenance) and the provider webhook inspector. Reached
//! through [`ForgeBase::billing`](crate::ForgeBase::billing).
//!
//! Request and response types are the generated wire models ([`generated`](crate::generated)).

use std::marker::PhantomData;
use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde_json::json;

use crate::billing::BillingApi;
use crate::error::Result;
use crate::generated::{
    BillingCollection, BillingMaintenanceReport, BillingSettings, BillingStats, BillingSubscription, CancelInput,
    ChangePlanInput, Checkout, Coupon, CouponInput, CreditNote, CreditNoteInput, Customer, CustomerInput, DunningEntry,
    HostedPage, Invoice, Order, OrderInput, OrderPage, Payment, PaymentMethodInput, Portal, Price, PriceInput, Product,
    ProductInput, ProrationPreview, ProviderAccount, ProviderInput, Refund, SubscribeInput, TaxRate, TaxRateInput,
    UsageMeterPage, WebhookDeliveryDetail, WebhookDeliveryPage, WebhookDeliveryResult,
};
use crate::http::{Req, Transport, seg};

/// List filters of the per-subject lists.
#[derive(Debug, Clone, Default)]
pub struct SubjectListOptions {
    /// `org:<id>` or a principal key (default: the caller's subject).
    pub subject: Option<String>,
    /// Rows to return.
    pub limit: Option<u32>,
}

/// Options of [`BillingInvoicesApi::list`].
#[derive(Debug, Clone, Default)]
pub struct ListInvoices {
    /// Subject and limit (at most 500).
    pub list: SubjectListOptions,
    /// Only this status (`draft`, `open`, `paid`, `void`, ...).
    pub status: Option<String>,
}

/// Options of [`BillingOrdersApi::list_all`].
#[derive(Debug, Clone, Default)]
pub struct ListAllOrders {
    /// Only this subject.
    pub subject: Option<String>,
    /// Only this order status.
    pub status: Option<String>,
    /// Page size, 1-500 (default 50).
    pub limit: Option<u32>,
    /// Rows to skip.
    pub offset: Option<u32>,
}

/// Options of [`BillingApi::usage_meters`].
#[derive(Debug, Clone, Default)]
pub struct UsageMetersOptions {
    /// Only this subject.
    pub subject: Option<String>,
    /// Only this feature.
    pub feature: Option<String>,
    /// Only meters at or above this percentage of their limit.
    pub min_percent: Option<u32>,
    /// Page size.
    pub limit: Option<u32>,
    /// Rows to skip.
    pub offset: Option<u32>,
}

/// Options of [`BillingApi::stats`].
#[derive(Debug, Clone, Default)]
pub struct StatsOptions {
    /// Window start, RFC 3339 (default: 30 days before `to`).
    pub from: Option<String>,
    /// Window end, RFC 3339 (default: now).
    pub to: Option<String>,
    /// Bucket size of the revenue series: `day` (default) or `month`.
    pub interval: Option<String>,
}

/// Filters of [`BillingWebhookDeliveriesApi::list`].
#[derive(Debug, Clone, Default)]
pub struct WebhookDeliveriesQuery {
    /// Provider (`stripe`, `razorpay`, ...).
    pub provider: Option<String>,
    /// Processing outcome (`applied`, `ignored`, `duplicate`, `failed`) or verification outcome of
    /// a refused delivery (`signature_invalid`, `timestamp_out_of_tolerance`, `unknown_provider`,
    /// `malformed`, `rejected`, `error`).
    pub outcome: Option<String>,
    /// Provider event type.
    pub event_type: Option<String>,
    /// Provider event id.
    pub event_id: Option<String>,
    /// Received at or after (RFC 3339).
    pub from: Option<String>,
    /// Received before (RFC 3339, exclusive).
    pub to: Option<String>,
    /// 1-500 (default 50).
    pub limit: Option<u32>,
    /// Rows to skip.
    pub offset: Option<u32>,
}

impl BillingApi {
    /// The billing customer of a subject.
    #[must_use]
    pub fn customers(&self) -> BillingCustomersApi {
        BillingCustomersApi { t: self.t.clone() }
    }
    /// What a customer sees and does.
    #[must_use]
    pub fn portal(&self) -> BillingPortalApi {
        BillingPortalApi { t: self.t.clone() }
    }
    /// Recurring subscriptions.
    #[must_use]
    pub fn subscriptions(&self) -> BillingSubscriptionsApi {
        BillingSubscriptionsApi { t: self.t.clone() }
    }
    /// One-time purchases.
    #[must_use]
    pub fn orders(&self) -> BillingOrdersApi {
        BillingOrdersApi { t: self.t.clone() }
    }
    /// Invoices.
    #[must_use]
    pub fn invoices(&self) -> BillingInvoicesApi {
        BillingInvoicesApi { t: self.t.clone() }
    }
    /// Credit notes of a subject.
    #[must_use]
    pub fn credit_notes(&self) -> BillingSubjectList<CreditNote> {
        BillingSubjectList::new(self.t.clone(), "/api/v1/billing/credit-notes")
    }
    /// Payments of a subject.
    #[must_use]
    pub fn payments(&self) -> BillingSubjectList<Payment> {
        BillingSubjectList::new(self.t.clone(), "/api/v1/billing/payments")
    }
    /// Refunds of a subject.
    #[must_use]
    pub fn refunds(&self) -> BillingSubjectList<Refund> {
        BillingSubjectList::new(self.t.clone(), "/api/v1/billing/refunds")
    }
    /// Products, prices, coupons and tax rates.
    #[must_use]
    pub fn catalog(&self) -> BillingCatalogApi {
        BillingCatalogApi { t: self.t.clone() }
    }
    /// Payment provider accounts.
    #[must_use]
    pub fn providers(&self) -> BillingProvidersApi {
        BillingProvidersApi { t: self.t.clone() }
    }
    /// Operator settings.
    #[must_use]
    pub fn settings(&self) -> BillingSettingsApi {
        BillingSettingsApi { t: self.t.clone() }
    }
    /// The webhook inspector: every request to the provider webhook endpoint, verified or not (platform administrators).
    #[must_use]
    pub fn webhook_deliveries(&self) -> BillingWebhookDeliveriesApi {
        BillingWebhookDeliveriesApi { t: self.t.clone() }
    }

    /// MRR, ARR, churn and revenue by currency (default: the last 30 days by day).
    pub async fn stats(&self, o: &StatsOptions) -> Result<BillingStats> {
        self.t
            .json(
                Req::get("/api/v1/billing/stats")
                    .q_opt("from", o.from.as_deref())
                    .q_opt("to", o.to.as_deref())
                    .q_opt("interval", o.interval.as_deref()),
            )
            .await
    }

    /// Usage counters of every subject in their current windows, most used first, with 70 / 90 /
    /// 100 % states.
    pub async fn usage_meters(&self, o: &UsageMetersOptions) -> Result<UsageMeterPage> {
        self.t
            .json(
                Req::get("/api/v1/billing/admin/usage-meters")
                    .q_opt("subject", o.subject.as_deref())
                    .q_opt("feature", o.feature.as_deref())
                    .q_opt("min_percent", o.min_percent)
                    .q_opt("limit", o.limit)
                    .q_opt("offset", o.offset),
            )
            .await
    }

    /// Open invoices with a failed attempt or a scheduled retry, next attempt first. `limit` is 1-500 (default 200).
    pub async fn dunning(&self, limit: Option<u32>) -> Result<Vec<DunningEntry>> {
        self.t
            .items(Req::get("/api/v1/billing/dunning").q_opt("limit", limit))
            .await
    }

    /// Runs maintenance now (usage, renewals, dunning, fulfillment, entitlement syncs). Platform administrators.
    pub async fn maintenance(&self) -> Result<BillingMaintenanceReport> {
        self.t
            .json(Req::post("/api/v1/billing/maintenance").json_raw(json!({})))
            .await
    }
}

/// The billing customer of a subject: `client.billing().customers()`.
#[derive(Clone)]
pub struct BillingCustomersApi {
    t: Arc<Transport>,
}

impl BillingCustomersApi {
    /// The customer of `subject` (default: the caller's).
    pub async fn get(&self, subject: Option<&str>) -> Result<Customer> {
        self.t
            .json(Req::get("/api/v1/billing/customer").q_opt("subject", subject))
            .await
    }

    /// Creates or updates the customer of a subject: billing email, currency, tax region, provider.
    pub async fn put(&self, input: &CustomerInput) -> Result<Customer> {
        self.t.json(Req::put("/api/v1/billing/customer").json(input)?).await
    }

    /// Every customer (realm-level `billing:read`). `limit` is at most 1000.
    pub async fn list(&self, limit: Option<u32>) -> Result<Vec<Customer>> {
        self.t
            .items(Req::get("/api/v1/billing/customers").q_opt("limit", limit))
            .await
    }
}

/// What a customer sees and does: `client.billing().portal()`.
#[derive(Clone)]
pub struct BillingPortalApi {
    t: Arc<Transport>,
}

impl BillingPortalApi {
    /// The customer, live subscriptions and recent invoices, payments and credit notes of `subject`.
    pub async fn get(&self, subject: Option<&str>) -> Result<Portal> {
        self.t
            .json(Req::get("/api/v1/billing/portal").q_opt("subject", subject))
            .await
    }

    /// A provider-hosted page that saves or replaces the payment method: send the customer to `url`.
    pub async fn payment_method(&self, input: &PaymentMethodInput) -> Result<HostedPage> {
        self.t
            .json(Req::post("/api/v1/billing/portal/payment-method").json(input)?)
            .await
    }
}

/// Recurring subscriptions: `client.billing().subscriptions()`.
#[derive(Clone)]
pub struct BillingSubscriptionsApi {
    t: Arc<Transport>,
}

impl BillingSubscriptionsApi {
    /// Subscriptions of `subject` (default: the caller's).
    pub async fn list(&self, subject: Option<&str>) -> Result<Vec<BillingSubscription>> {
        self.t
            .items(Req::get("/api/v1/billing/subscriptions").q_opt("subject", subject))
            .await
    }

    /// One subscription.
    pub async fn get(&self, id: &str) -> Result<BillingSubscription> {
        self.t
            .json(Req::get(format!("/api/v1/billing/subscriptions/{}", seg(id))))
            .await
    }

    /// Subscribes: a trial, or the first invoice collected (`payment_url` when the customer must
    /// pay). A replayed `idempotency_key` returns the first result with `replayed: true`; a
    /// second live subscription is a conflict.
    pub async fn create(&self, input: &SubscribeInput) -> Result<Checkout> {
        self.t
            .json(Req::post("/api/v1/billing/subscriptions").json(input)?.idempotent())
            .await
    }

    /// Upgrades or downgrades with proration.
    pub async fn change(&self, id: &str, input: &ChangePlanInput) -> Result<Checkout> {
        self.t
            .json(Req::post(format!("/api/v1/billing/subscriptions/{}/change", seg(id))).json(input)?)
            .await
    }

    /// What [`change`](Self::change) would do (proration lines, credit, charge, net) with no side effects.
    pub async fn preview_change(&self, id: &str, input: &ChangePlanInput) -> Result<ProrationPreview> {
        self.t
            .json(
                Req::post(format!("/api/v1/billing/subscriptions/{}/change/preview", seg(id)))
                    .json(input)?
                    .idempotent(),
            )
            .await
    }

    /// Cancels at the period end (default) or now (`at_period_end: Some(false)`).
    pub async fn cancel(&self, id: &str, input: &CancelInput) -> Result<Checkout> {
        self.t
            .json(Req::post(format!("/api/v1/billing/subscriptions/{}/cancel", seg(id))).json(input)?)
            .await
    }

    /// Pauses an active subscription.
    pub async fn pause(&self, id: &str) -> Result<BillingSubscription> {
        self.t
            .json(Req::post(format!("/api/v1/billing/subscriptions/{}/pause", seg(id))).json_raw(json!({})))
            .await
    }

    /// Resumes a paused subscription or withdraws a scheduled cancellation.
    pub async fn resume(&self, id: &str) -> Result<Checkout> {
        self.t
            .json(Req::post(format!("/api/v1/billing/subscriptions/{}/resume", seg(id))).json_raw(json!({})))
            .await
    }
}

/// One-time purchases (credit top-ups, usage packs): `client.billing().orders()`.
#[derive(Clone)]
pub struct BillingOrdersApi {
    t: Arc<Transport>,
}

impl BillingOrdersApi {
    /// Orders of `subject` (default: the caller's).
    pub async fn list(&self, subject: Option<&str>) -> Result<Vec<Order>> {
        self.t
            .items(Req::get("/api/v1/billing/orders").q_opt("subject", subject))
            .await
    }

    /// One order.
    pub async fn get(&self, id: &str) -> Result<Order> {
        self.t
            .json(Req::get(format!("/api/v1/billing/orders/{}", seg(id))))
            .await
    }

    /// Buys once. A replayed `idempotency_key` returns the first result with `replayed: true`.
    pub async fn create(&self, input: &OrderInput) -> Result<Checkout> {
        self.t
            .json(Req::post("/api/v1/billing/orders").json(input)?.idempotent())
            .await
    }

    /// Every order of the instance, newest first, with paging (billing readers).
    pub async fn list_all(&self, o: &ListAllOrders) -> Result<OrderPage> {
        self.t
            .json(
                Req::get("/api/v1/billing/admin/orders")
                    .q_opt("subject", o.subject.as_deref())
                    .q_opt("status", o.status.as_deref())
                    .q_opt("limit", o.limit)
                    .q_opt("offset", o.offset),
            )
            .await
    }
}

/// Invoices: `client.billing().invoices()`.
#[derive(Clone)]
pub struct BillingInvoicesApi {
    t: Arc<Transport>,
}

impl BillingInvoicesApi {
    /// Invoices of `subject`, newest first. `limit` is at most 500.
    pub async fn list(&self, o: &ListInvoices) -> Result<Vec<Invoice>> {
        self.t
            .items(
                Req::get("/api/v1/billing/invoices")
                    .q_opt("subject", o.list.subject.as_deref())
                    .q_opt("status", o.status.as_deref())
                    .q_opt("limit", o.list.limit),
            )
            .await
    }

    /// One invoice with its lines.
    pub async fn get(&self, id: &str) -> Result<Invoice> {
        self.t
            .json(Req::get(format!("/api/v1/billing/invoices/{}", seg(id))))
            .await
    }

    /// Pays an open invoice now: with the saved method, else the answer carries the hosted page `payment_url`.
    pub async fn pay(&self, id: &str, return_url: Option<&str>) -> Result<BillingCollection> {
        self.t
            .json(
                Req::post(format!("/api/v1/billing/invoices/{}/pay", seg(id)))
                    .json_value(json!({"return_url": return_url})),
            )
            .await
    }

    /// Voids an unpaid invoice (operators).
    pub async fn void(&self, id: &str) -> Result<Invoice> {
        self.t
            .json(Req::post(format!("/api/v1/billing/invoices/{}/void", seg(id))).json_raw(json!({})))
            .await
    }

    /// Records an out-of-band payment (operators; manual billing).
    pub async fn mark_paid(&self, id: &str, reference: Option<&str>) -> Result<Invoice> {
        self.t
            .json(
                Req::post(format!("/api/v1/billing/invoices/{}/mark-paid", seg(id)))
                    .json_value(json!({"reference": reference})),
            )
            .await
    }

    /// Issues a credit note: a refund, or a credit to the customer balance with `to_balance`
    /// (operators). Idempotent by `idempotency_key`.
    pub async fn credit_note(&self, id: &str, input: &CreditNoteInput) -> Result<CreditNote> {
        self.t
            .json(
                Req::post(format!("/api/v1/billing/invoices/{}/credit-notes", seg(id)))
                    .json(input)?
                    .idempotent(),
            )
            .await
    }
}

/// A read-only list of a subject's records (credit notes, payments, refunds).
pub struct BillingSubjectList<T> {
    t: Arc<Transport>,
    path: &'static str,
    _marker: PhantomData<fn() -> T>,
}

impl<T: DeserializeOwned> BillingSubjectList<T> {
    fn new(t: Arc<Transport>, path: &'static str) -> Self {
        Self {
            t,
            path,
            _marker: PhantomData,
        }
    }

    /// Records of `subject` (default: the caller's), newest first.
    pub async fn list(&self, o: &SubjectListOptions) -> Result<Vec<T>> {
        self.t
            .items(
                Req::get(self.path)
                    .q_opt("subject", o.subject.as_deref())
                    .q_opt("limit", o.limit),
            )
            .await
    }
}

/// Products, prices, coupons and tax rates: `client.billing().catalog()`. Writes are for platform administrators.
#[derive(Clone)]
pub struct BillingCatalogApi {
    t: Arc<Transport>,
}

impl BillingCatalogApi {
    /// Products.
    #[must_use]
    pub fn products(&self) -> BillingProductsApi {
        BillingProductsApi { t: self.t.clone() }
    }
    /// Prices.
    #[must_use]
    pub fn prices(&self) -> BillingPricesApi {
        BillingPricesApi { t: self.t.clone() }
    }
    /// Coupons.
    #[must_use]
    pub fn coupons(&self) -> BillingCouponsApi {
        BillingCouponsApi { t: self.t.clone() }
    }
    /// Tax rates.
    #[must_use]
    pub fn tax_rates(&self) -> BillingTaxRatesApi {
        BillingTaxRatesApi { t: self.t.clone() }
    }
}

/// Products: `client.billing().catalog().products()`.
#[derive(Clone)]
pub struct BillingProductsApi {
    t: Arc<Transport>,
}

impl BillingProductsApi {
    /// Every product.
    pub async fn list(&self) -> Result<Vec<Product>> {
        self.t.items(Req::get("/api/v1/billing/products")).await
    }

    /// One product.
    pub async fn get(&self, key: &str) -> Result<Product> {
        self.t
            .json(Req::get(format!("/api/v1/billing/products/{}", seg(key))))
            .await
    }

    /// Creates or replaces a product. Its `kind` maps a payment to a plan, add-on, usage pack or credits.
    pub async fn put(&self, key: &str, input: &ProductInput) -> Result<Product> {
        self.t
            .json(Req::put(format!("/api/v1/billing/products/{}", seg(key))).json(input)?)
            .await
    }

    /// Archives a product (idempotent): it can no longer be bought; existing subscriptions continue.
    pub async fn archive(&self, key: &str) -> Result<Product> {
        self.t
            .json(Req::post(format!("/api/v1/billing/products/{}/archive", seg(key))).json_raw(json!({})))
            .await
    }
}

/// Prices: `client.billing().catalog().prices()`.
#[derive(Clone)]
pub struct BillingPricesApi {
    t: Arc<Transport>,
}

impl BillingPricesApi {
    /// Prices, of one product when `product` is given.
    pub async fn list(&self, product: Option<&str>) -> Result<Vec<Price>> {
        self.t
            .items(Req::get("/api/v1/billing/prices").q_opt("product", product))
            .await
    }

    /// One price.
    pub async fn get(&self, key: &str) -> Result<Price> {
        self.t
            .json(Req::get(format!("/api/v1/billing/prices/{}", seg(key))))
            .await
    }

    /// Creates or replaces a price: recurring, one-time or metered; per unit, graduated or volume
    /// tiers; several currencies. Changing the amounts of a price in use is a conflict.
    pub async fn put(&self, key: &str, input: &PriceInput) -> Result<Price> {
        self.t
            .json(Req::put(format!("/api/v1/billing/prices/{}", seg(key))).json(input)?)
            .await
    }

    /// Archives a price (idempotent): new subscriptions and plan changes refuse it; existing ones keep renewing on it.
    pub async fn archive(&self, key: &str) -> Result<Price> {
        self.t
            .json(Req::post(format!("/api/v1/billing/prices/{}/archive", seg(key))).json_raw(json!({})))
            .await
    }
}

/// Coupons: `client.billing().catalog().coupons()` (platform administrators).
#[derive(Clone)]
pub struct BillingCouponsApi {
    t: Arc<Transport>,
}

impl BillingCouponsApi {
    /// Every coupon.
    pub async fn list(&self) -> Result<Vec<Coupon>> {
        self.t.items(Req::get("/api/v1/billing/coupons")).await
    }

    /// Creates or replaces a coupon.
    pub async fn put(&self, code: &str, input: &CouponInput) -> Result<Coupon> {
        self.t
            .json(Req::put(format!("/api/v1/billing/coupons/{}", seg(code))).json(input)?)
            .await
    }

    /// Archives a coupon (idempotent): it cannot be redeemed again.
    pub async fn archive(&self, code: &str) -> Result<Coupon> {
        self.t
            .json(Req::post(format!("/api/v1/billing/coupons/{}/archive", seg(code))).json_raw(json!({})))
            .await
    }
}

/// Tax rates per region: `client.billing().catalog().tax_rates()`.
#[derive(Clone)]
pub struct BillingTaxRatesApi {
    t: Arc<Transport>,
}

impl BillingTaxRatesApi {
    /// Every tax rate.
    pub async fn list(&self) -> Result<Vec<TaxRate>> {
        self.t.items(Req::get("/api/v1/billing/tax-rates")).await
    }

    /// Sets the rate of a region (`IN`, `DE`, `US-CA`, ...; `rate_bp` in basis points). Platform administrators.
    pub async fn put(&self, region: &str, input: &TaxRateInput) -> Result<TaxRate> {
        self.t
            .json(Req::put(format!("/api/v1/billing/tax-rates/{}", seg(region))).json(input)?)
            .await
    }

    /// Archives the rate of a region (idempotent): no tax is added for it afterwards.
    pub async fn archive(&self, region: &str) -> Result<TaxRate> {
        self.t
            .json(Req::post(format!("/api/v1/billing/tax-rates/{}/archive", seg(region))).json_raw(json!({})))
            .await
    }
}

/// Payment provider accounts: `client.billing().providers()`. Secrets are sealed and never returned.
#[derive(Clone)]
pub struct BillingProvidersApi {
    t: Arc<Transport>,
}

impl BillingProvidersApi {
    /// Configured providers (no secrets).
    pub async fn list(&self) -> Result<Vec<ProviderAccount>> {
        self.t.items(Req::get("/api/v1/billing/providers")).await
    }

    /// Saves a provider account (`stripe`, `razorpay`, `wechat_pay`, `manual`, or a store:
    /// `app_store`, `google_play`, `creem` with its `config`). Test-mode credentials only;
    /// `clear` names stored secrets to remove.
    pub async fn put(&self, provider: &str, input: &ProviderInput) -> Result<ProviderAccount> {
        self.t
            .json(Req::put(format!("/api/v1/billing/providers/{}", seg(provider))).json(input)?)
            .await
    }

    /// Removes a provider account.
    pub async fn delete(&self, provider: &str) -> Result<()> {
        self.t
            .unit(Req::delete(format!("/api/v1/billing/providers/{}", seg(provider))))
            .await
    }
}

/// Operator settings (dunning schedule, number prefixes, webhook windows): `client.billing().settings()`.
#[derive(Clone)]
pub struct BillingSettingsApi {
    t: Arc<Transport>,
}

impl BillingSettingsApi {
    /// The settings.
    pub async fn get(&self) -> Result<BillingSettings> {
        self.t.json(Req::get("/api/v1/billing/settings")).await
    }

    /// Replaces the settings (platform administrators): read with [`get`](Self::get), change, write back.
    pub async fn put(&self, settings: &BillingSettings) -> Result<BillingSettings> {
        self.t.json(Req::put("/api/v1/billing/settings").json(settings)?).await
    }
}

/// The webhook inspector (ADR-0032): `client.billing().webhook_deliveries()`. Platform administrators only.
#[derive(Clone)]
pub struct BillingWebhookDeliveriesApi {
    t: Arc<Transport>,
}

const W: &str = "/api/v1/billing/webhook-deliveries";

impl BillingWebhookDeliveriesApi {
    /// Deliveries, newest first, with their total. No bodies.
    pub async fn list(&self, q: &WebhookDeliveriesQuery) -> Result<WebhookDeliveryPage> {
        self.t
            .json(
                Req::get(W)
                    .q_opt("provider", q.provider.as_deref())
                    .q_opt("outcome", q.outcome.as_deref())
                    .q_opt("event_type", q.event_type.as_deref())
                    .q_opt("event_id", q.event_id.as_deref())
                    .q_opt("from", q.from.as_deref())
                    .q_opt("to", q.to.as_deref())
                    .q_opt("limit", q.limit)
                    .q_opt("offset", q.offset),
            )
            .await
    }

    /// One delivery with its `headers` and `body` (`body_encoding` `utf8`, or `base64` for binary
    /// bodies; `None` when the budget dropped it).
    pub async fn get(&self, id: &str) -> Result<WebhookDeliveryDetail> {
        self.t.json(Req::get(format!("{W}/{}", seg(id)))).await
    }

    /// Processes a verified delivery again through the normal path (the event dedup decides).
    /// `result` is `applied`, `ignored`, `failed`, `already_applied`, `already_ignored` or `unverified`.
    pub async fn replay(&self, id: &str) -> Result<WebhookDeliveryResult> {
        self.t
            .json(Req::post(format!("{W}/{}/replay", seg(id))).json_raw(json!({})))
            .await
    }

    /// Verifies a refused delivery again with the current secrets (after fixing a webhook secret)
    /// and processes it if it now verifies, exactly once.
    pub async fn reverify(&self, id: &str) -> Result<WebhookDeliveryResult> {
        self.t
            .json(Req::post(format!("{W}/{}/reverify", seg(id))).json_raw(json!({})))
            .await
    }
}
