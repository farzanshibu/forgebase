//! REST under `/api/v1/billing` (mounted as an `ApiExtension`, tags
//! `billing`, `billing-catalog`, `billing-portal`, `billing-webhooks`).
//!
//! Handlers only translate HTTP; the service authorises every call. Webhook
//! deliveries are anonymous: the provider signature is their authentication.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use forgebase_api::http::{ApiJson, Query};
use forgebase_api::principal::Access;
use forgebase_api::{ApiError, ApiExtension};
use forgebase_entitlements::model::parse_iso;
use forgebase_policy::PolicyError;
use serde::Serialize;
use serde_json::json;
use tower_http::limit::RequestBodyLimitLayer;
use utoipa::{OpenApi, ToSchema};

use crate::error::BillingError;
use crate::model::*;
use crate::service::{BillingService, DAY};
use crate::stats::Bucket;

type ApiResult = Result<Response, ApiError>;
type Body<T> = Result<ApiJson<T>, ApiError>;

const MAX_BODY: usize = 256 * 1024;

/// The HTTP form of a service error.
pub fn api(e: BillingError) -> ApiError {
    match &e {
        BillingError::Policy(PolicyError::Denied(d)) => ApiError::from_error(&e).with_details(json!({"step": d.step})),
        BillingError::Provider(_) => ApiError::new(StatusCode::BAD_GATEWAY, "provider_error", e.to_string()),
        _ => ApiError::from_error(&e),
    }
}

fn ok<T: Serialize>(v: T) -> ApiResult {
    Ok(Json(v).into_response())
}

fn created<T: Serialize>(v: T, replayed: bool) -> ApiResult {
    let status = if replayed { StatusCode::OK } else { StatusCode::CREATED };
    Ok((status, Json(v)).into_response())
}

/// A list.
#[derive(Serialize, ToSchema)]
pub struct Items<T> {
    /// Entries.
    pub items: Vec<T>,
}

fn items<T: Serialize>(v: Vec<T>) -> ApiResult {
    ok(Items { items: v })
}

/// Authenticates before surfacing body errors.
fn body<T>(s: &BillingService, ctx: &forgebase_data::AccessContext, b: Body<T>) -> Result<T, ApiError> {
    s.require_caller(ctx).map_err(api)?;
    Ok(b?.0)
}

fn time_param(q: &Query, name: &str) -> Result<Option<i64>, ApiError> {
    match q.get(name) {
        None | Some("") => Ok(None),
        Some(v) => parse_iso(v)
            .map(Some)
            .ok_or_else(|| ApiError::bad_request("invalid_parameter", format!("{name} must be an RFC 3339 time"))),
    }
}

/// The routes, relative to `/api/v1`, with their state applied.
pub fn router<S: Clone + Send + Sync + 'static>(svc: BillingService) -> Router<S> {
    Router::<BillingService>::new()
        .route("/billing/settings", get(get_settings).put(put_settings))
        .route("/billing/providers", get(providers))
        .route(
            "/billing/providers/{provider}",
            put(put_provider).delete(delete_provider),
        )
        .route("/billing/products", get(products))
        .route("/billing/products/{key}", get(product).put(put_product))
        .route("/billing/prices", get(prices))
        .route("/billing/prices/{key}", get(price).put(put_price))
        .route("/billing/tax-rates", get(tax_rates))
        .route("/billing/tax-rates/{region}", put(put_tax_rate))
        .route("/billing/coupons", get(coupons))
        .route("/billing/coupons/{code}", put(put_coupon))
        .route("/billing/customer", get(customer).put(put_customer))
        .route("/billing/customers", get(customers))
        .route("/billing/subscriptions", get(subscriptions).post(subscribe))
        .route("/billing/subscriptions/{id}", get(subscription))
        .route("/billing/subscriptions/{id}/change", post(change_plan))
        .route("/billing/subscriptions/{id}/cancel", post(cancel))
        .route("/billing/subscriptions/{id}/pause", post(pause))
        .route("/billing/subscriptions/{id}/resume", post(resume))
        .route("/billing/orders", get(orders).post(create_order))
        .route("/billing/orders/{id}", get(order))
        .route("/billing/invoices", get(invoices))
        .route("/billing/invoices/{id}", get(invoice))
        .route("/billing/invoices/{id}/pay", post(pay_invoice))
        .route("/billing/invoices/{id}/mark-paid", post(mark_paid))
        .route("/billing/invoices/{id}/void", post(void_invoice))
        .route("/billing/invoices/{id}/credit-notes", post(credit_invoice))
        .route("/billing/credit-notes", get(credit_notes))
        .route("/billing/payments", get(payments))
        .route("/billing/refunds", get(refunds))
        .route("/billing/portal", get(portal))
        .route("/billing/portal/payment-method", post(payment_method))
        .route("/billing/webhooks/{provider}", post(webhook))
        .route("/billing/webhook-events", get(webhook_events))
        .route("/billing/stats", get(stats))
        .route("/billing/maintenance", post(maintenance))
        .layer(RequestBodyLimitLayer::new(MAX_BODY))
        .with_state(svc)
}

/// The routes and OpenAPI fragment, ready for `RouterOptions::extensions`.
pub fn extension(svc: BillingService) -> ApiExtension {
    ApiExtension {
        routes: router(svc),
        openapi: openapi(),
    }
}

/// OpenAPI paths and schemas.
pub fn openapi() -> utoipa::openapi::OpenApi {
    BillingDoc::openapi()
}

#[derive(OpenApi)]
#[openapi(
    paths(
        get_settings, put_settings, providers, put_provider, delete_provider, products, product, put_product, prices,
        price, put_price, tax_rates, put_tax_rate, coupons, put_coupon, customer, put_customer, customers,
        subscriptions, subscribe, subscription, change_plan, cancel, pause, resume, orders, create_order, order,
        invoices, invoice, pay_invoice, mark_paid, void_invoice, credit_invoice, credit_notes, payments, refunds,
        portal, payment_method, webhook, webhook_events, stats, maintenance,
    ),
    components(schemas(
        Money, Tier, Scheme, ProductKind, PriceType, Interval, MeterBasis, TaxBehavior, SubscriptionStatus,
        InvoiceStatus, PaymentStatus, RefundStatus, CouponKind, CouponDuration, LineKind, OrderStatus, Proration,
        FinalAction, Product, ProductInput, CurrencyOption, Price, PriceInput, TaxRate, TaxRateInput, Coupon,
        CouponInput, Customer, CustomerInput, SubscriptionItem, Subscription, ItemInput, SubscribeInput,
        ChangePlanInput, CancelInput, Checkout, Order, OrderInput, InvoiceLine, Invoice, Payment, Refund, CreditNote,
        CreditNoteInput, Collection, PaymentMethodInput, HostedPage, ProviderAccount, ProviderInput, WebhookEvent,
        WebhookOutcome, BillingSettings, MaintenanceReport, RecurringRevenue, RevenuePoint, BillingStats, Portal,
        MarkPaidInput, PayInput, Items<Product>, Items<Price>, Items<Invoice>,
    )),
    tags(
        (name = "billing", description = "Subscriptions, one-time orders, invoices, payments, credit notes and refunds, dunning, payment statistics"),
        (name = "billing-catalog", description = "Products, multi-currency prices (recurring, one-time, metered; per unit, graduated, volume), tax rates, coupons, provider accounts, settings"),
        (name = "billing-portal", description = "What a customer sees and does: invoices, plan changes, cancellation, the payment method on the provider's hosted page"),
        (name = "billing-webhooks", description = "Signed, idempotent provider webhooks (Stripe, Razorpay) with replay protection"),
    ),
)]
struct BillingDoc;

const SUBJECT: &str = "org:<id> or a principal key (default: the caller's subject)";

// ---------------------------------------------------------------------------
// Settings, providers
// ---------------------------------------------------------------------------

/// Operator settings (dunning schedule, number prefixes, webhook windows).
#[utoipa::path(get, path = "/api/v1/billing/settings", operation_id = "billing_get_settings", tag = "billing-catalog",
    responses((status = 200, description = "Settings", body = BillingSettings)))]
async fn get_settings(State(s): State<BillingService>, Access(ctx): Access) -> ApiResult {
    s.require_admin(&ctx).map_err(api)?;
    ok(s.settings().await.map_err(api)?)
}

/// Replaces the settings (platform administrators).
#[utoipa::path(put, path = "/api/v1/billing/settings", operation_id = "billing_put_settings", tag = "billing-catalog", request_body = BillingSettings,
    responses((status = 200, description = "Settings", body = BillingSettings)))]
async fn put_settings(State(s): State<BillingService>, Access(ctx): Access, b: Body<BillingSettings>) -> ApiResult {
    let input = body(&s, &ctx, b)?;
    ok(s.put_settings(&ctx, input).await.map_err(api)?)
}

/// Configured payment providers (no secrets).
#[utoipa::path(get, path = "/api/v1/billing/providers", operation_id = "billing_providers", tag = "billing-catalog",
    responses((status = 200, description = "Accounts", body = Items<ProviderAccount>)))]
async fn providers(State(s): State<BillingService>, Access(ctx): Access) -> ApiResult {
    items(s.providers(&ctx).await.map_err(api)?)
}

/// Saves a provider account: `stripe`, `razorpay` or `manual`. Test-mode
/// keys only; secrets are sealed and never returned.
#[utoipa::path(put, path = "/api/v1/billing/providers/{provider}", operation_id = "billing_put_provider", tag = "billing-catalog", request_body = ProviderInput,
    params(("provider" = String, Path, description = "stripe, razorpay or manual")),
    responses((status = 200, description = "Saved", body = ProviderAccount), (status = 400, description = "Live or malformed keys")))]
async fn put_provider(
    State(s): State<BillingService>,
    Access(ctx): Access,
    Path(provider): Path<String>,
    b: Body<ProviderInput>,
) -> ApiResult {
    let input = body(&s, &ctx, b)?;
    ok(s.put_provider(&ctx, &provider, input).await.map_err(api)?)
}

/// Removes a provider account.
#[utoipa::path(delete, path = "/api/v1/billing/providers/{provider}", operation_id = "billing_delete_provider", tag = "billing-catalog",
    params(("provider" = String, Path, description = "Provider")),
    responses((status = 204, description = "Removed")))]
async fn delete_provider(
    State(s): State<BillingService>,
    Access(ctx): Access,
    Path(provider): Path<String>,
) -> ApiResult {
    s.delete_provider(&ctx, &provider).await.map_err(api)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

// ---------------------------------------------------------------------------
// Catalog
// ---------------------------------------------------------------------------

/// Products.
#[utoipa::path(get, path = "/api/v1/billing/products", operation_id = "billing_products", tag = "billing-catalog",
    responses((status = 200, description = "Products", body = Items<Product>)))]
async fn products(State(s): State<BillingService>, Access(ctx): Access) -> ApiResult {
    items(s.products(&ctx).await.map_err(api)?)
}

/// One product.
#[utoipa::path(get, path = "/api/v1/billing/products/{key}", operation_id = "billing_product", tag = "billing-catalog",
    params(("key" = String, Path, description = "Product key")),
    responses((status = 200, description = "Product", body = Product)))]
async fn product(State(s): State<BillingService>, Access(ctx): Access, Path(key): Path<String>) -> ApiResult {
    ok(s.product(&ctx, &key).await.map_err(api)?)
}

/// Creates or replaces a product (platform administrators). Its kind maps a
/// payment to entitlements: a FB-24 plan, add-on or usage pack, or FB-73
/// credits.
#[utoipa::path(put, path = "/api/v1/billing/products/{key}", operation_id = "billing_put_product", tag = "billing-catalog", request_body = ProductInput,
    params(("key" = String, Path, description = "Product key")),
    responses((status = 200, description = "Product", body = Product)))]
async fn put_product(
    State(s): State<BillingService>,
    Access(ctx): Access,
    Path(key): Path<String>,
    b: Body<ProductInput>,
) -> ApiResult {
    let input = body(&s, &ctx, b)?;
    ok(s.put_product(&ctx, &key, input).await.map_err(api)?)
}

/// Prices, optionally of one product.
#[utoipa::path(get, path = "/api/v1/billing/prices", operation_id = "billing_prices", tag = "billing-catalog",
    params(("product" = Option<String>, Query, description = "Product key")),
    responses((status = 200, description = "Prices", body = Items<Price>)))]
async fn prices(State(s): State<BillingService>, Access(ctx): Access, q: Query) -> ApiResult {
    items(s.prices(&ctx, q.get("product")).await.map_err(api)?)
}

/// One price.
#[utoipa::path(get, path = "/api/v1/billing/prices/{key}", operation_id = "billing_price", tag = "billing-catalog",
    params(("key" = String, Path, description = "Price key")),
    responses((status = 200, description = "Price", body = Price)))]
async fn price(State(s): State<BillingService>, Access(ctx): Access, Path(key): Path<String>) -> ApiResult {
    ok(s.price(&ctx, &key).await.map_err(api)?)
}

/// Creates or replaces a price (platform administrators): recurring monthly
/// or yearly, one-time or metered; per unit, graduated or volume tiers; the
/// same price in several currencies.
#[utoipa::path(put, path = "/api/v1/billing/prices/{key}", operation_id = "billing_put_price", tag = "billing-catalog", request_body = PriceInput,
    params(("key" = String, Path, description = "Price key")),
    responses((status = 200, description = "Price", body = Price), (status = 409, description = "Amounts of a price in use")))]
async fn put_price(
    State(s): State<BillingService>,
    Access(ctx): Access,
    Path(key): Path<String>,
    b: Body<PriceInput>,
) -> ApiResult {
    let input = body(&s, &ctx, b)?;
    ok(s.put_price(&ctx, &key, input).await.map_err(api)?)
}

/// Tax rates per region.
#[utoipa::path(get, path = "/api/v1/billing/tax-rates", operation_id = "billing_tax_rates", tag = "billing-catalog",
    responses((status = 200, description = "Rates", body = Items<TaxRate>)))]
async fn tax_rates(State(s): State<BillingService>, Access(ctx): Access) -> ApiResult {
    items(s.tax_rates(&ctx).await.map_err(api)?)
}

/// Sets the tax rate of a region (platform administrators).
#[utoipa::path(put, path = "/api/v1/billing/tax-rates/{region}", operation_id = "billing_put_tax_rate", tag = "billing-catalog", request_body = TaxRateInput,
    params(("region" = String, Path, description = "IN, DE, US-CA, ...")),
    responses((status = 200, description = "Rate", body = TaxRate)))]
async fn put_tax_rate(
    State(s): State<BillingService>,
    Access(ctx): Access,
    Path(region): Path<String>,
    b: Body<TaxRateInput>,
) -> ApiResult {
    let input = body(&s, &ctx, b)?;
    ok(s.put_tax_rate(&ctx, &region, input).await.map_err(api)?)
}

/// Coupons (platform administrators).
#[utoipa::path(get, path = "/api/v1/billing/coupons", operation_id = "billing_coupons", tag = "billing-catalog",
    responses((status = 200, description = "Coupons", body = Items<Coupon>)))]
async fn coupons(State(s): State<BillingService>, Access(ctx): Access) -> ApiResult {
    items(s.coupons(&ctx).await.map_err(api)?)
}

/// Creates or replaces a coupon (platform administrators).
#[utoipa::path(put, path = "/api/v1/billing/coupons/{code}", operation_id = "billing_put_coupon", tag = "billing-catalog", request_body = CouponInput,
    params(("code" = String, Path, description = "Coupon code")),
    responses((status = 200, description = "Coupon", body = Coupon)))]
async fn put_coupon(
    State(s): State<BillingService>,
    Access(ctx): Access,
    Path(code): Path<String>,
    b: Body<CouponInput>,
) -> ApiResult {
    let input = body(&s, &ctx, b)?;
    ok(s.put_coupon(&ctx, &code, input).await.map_err(api)?)
}

// ---------------------------------------------------------------------------
// Customers and subscriptions
// ---------------------------------------------------------------------------

/// The customer of a subject.
#[utoipa::path(get, path = "/api/v1/billing/customer", operation_id = "billing_customer", tag = "billing",
    params(("subject" = Option<String>, Query, description = SUBJECT)),
    responses((status = 200, description = "Customer", body = Customer)))]
async fn customer(State(s): State<BillingService>, Access(ctx): Access, q: Query) -> ApiResult {
    ok(s.customer(&ctx, q.get("subject")).await.map_err(api)?)
}

/// Creates or updates the customer of a subject (billing email, currency,
/// tax region, provider).
#[utoipa::path(put, path = "/api/v1/billing/customer", operation_id = "billing_put_customer", tag = "billing", request_body = CustomerInput,
    responses((status = 200, description = "Customer", body = Customer)))]
async fn put_customer(State(s): State<BillingService>, Access(ctx): Access, b: Body<CustomerInput>) -> ApiResult {
    let input = body(&s, &ctx, b)?;
    ok(s.put_customer(&ctx, input).await.map_err(api)?)
}

/// Every customer (realm-level `billing:read`).
#[utoipa::path(get, path = "/api/v1/billing/customers", operation_id = "billing_customers", tag = "billing",
    params(("limit" = Option<u32>, Query, description = "At most 1000")),
    responses((status = 200, description = "Customers", body = Items<Customer>)))]
async fn customers(State(s): State<BillingService>, Access(ctx): Access, q: Query) -> ApiResult {
    items(s.customers(&ctx, q.u32("limit")?).await.map_err(api)?)
}

/// Subscriptions of a subject.
#[utoipa::path(get, path = "/api/v1/billing/subscriptions", operation_id = "billing_subscriptions", tag = "billing",
    params(("subject" = Option<String>, Query, description = SUBJECT)),
    responses((status = 200, description = "Subscriptions", body = Items<Subscription>)))]
async fn subscriptions(State(s): State<BillingService>, Access(ctx): Access, q: Query) -> ApiResult {
    items(s.subscriptions(&ctx, q.get("subject")).await.map_err(api)?)
}

/// Subscribes (trial, or the first invoice collected). Idempotent by key.
#[utoipa::path(post, path = "/api/v1/billing/subscriptions", operation_id = "billing_subscribe", tag = "billing", request_body = SubscribeInput,
    responses((status = 201, description = "Created", body = Checkout), (status = 200, description = "Replayed", body = Checkout),
        (status = 409, description = "Already subscribed")))]
async fn subscribe(State(s): State<BillingService>, Access(ctx): Access, b: Body<SubscribeInput>) -> ApiResult {
    let input = body(&s, &ctx, b)?;
    let c = s.subscribe(&ctx, input).await.map_err(api)?;
    let replayed = c.replayed;
    created(c, replayed)
}

/// One subscription.
#[utoipa::path(get, path = "/api/v1/billing/subscriptions/{id}", operation_id = "billing_subscription", tag = "billing",
    params(("id" = String, Path, description = "Subscription id")),
    responses((status = 200, description = "Subscription", body = Subscription)))]
async fn subscription(State(s): State<BillingService>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult {
    ok(s.subscription(&ctx, &id).await.map_err(api)?)
}

/// Changes the plan (upgrade or downgrade) with proration (customer portal).
#[utoipa::path(post, path = "/api/v1/billing/subscriptions/{id}/change", operation_id = "billing_change_plan", tag = "billing-portal", request_body = ChangePlanInput,
    params(("id" = String, Path, description = "Subscription id")),
    responses((status = 200, description = "Changed", body = Checkout)))]
async fn change_plan(
    State(s): State<BillingService>,
    Access(ctx): Access,
    Path(id): Path<String>,
    b: Body<ChangePlanInput>,
) -> ApiResult {
    let input = body(&s, &ctx, b)?;
    ok(s.change_plan(&ctx, &id, input).await.map_err(api)?)
}

/// Cancels at the period end (default) or now (customer portal).
#[utoipa::path(post, path = "/api/v1/billing/subscriptions/{id}/cancel", operation_id = "billing_cancel", tag = "billing-portal", request_body = CancelInput,
    params(("id" = String, Path, description = "Subscription id")),
    responses((status = 200, description = "Cancelled", body = Checkout)))]
async fn cancel(
    State(s): State<BillingService>,
    Access(ctx): Access,
    Path(id): Path<String>,
    b: Body<CancelInput>,
) -> ApiResult {
    let input = body(&s, &ctx, b)?;
    ok(s.cancel(&ctx, &id, input).await.map_err(api)?)
}

/// Pauses an active subscription.
#[utoipa::path(post, path = "/api/v1/billing/subscriptions/{id}/pause", operation_id = "billing_pause", tag = "billing",
    params(("id" = String, Path, description = "Subscription id")),
    responses((status = 200, description = "Paused", body = Subscription)))]
async fn pause(State(s): State<BillingService>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult {
    ok(s.pause(&ctx, &id).await.map_err(api)?)
}

/// Resumes a paused subscription or withdraws a scheduled cancellation.
#[utoipa::path(post, path = "/api/v1/billing/subscriptions/{id}/resume", operation_id = "billing_resume", tag = "billing",
    params(("id" = String, Path, description = "Subscription id")),
    responses((status = 200, description = "Resumed", body = Checkout)))]
async fn resume(State(s): State<BillingService>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult {
    ok(s.resume(&ctx, &id).await.map_err(api)?)
}

/// Orders of a subject.
#[utoipa::path(get, path = "/api/v1/billing/orders", operation_id = "billing_orders", tag = "billing",
    params(("subject" = Option<String>, Query, description = SUBJECT)),
    responses((status = 200, description = "Orders", body = Items<Order>)))]
async fn orders(State(s): State<BillingService>, Access(ctx): Access, q: Query) -> ApiResult {
    items(s.orders(&ctx, q.get("subject")).await.map_err(api)?)
}

/// A one-time purchase (credit top-ups, usage packs). Idempotent by key.
#[utoipa::path(post, path = "/api/v1/billing/orders", operation_id = "billing_create_order", tag = "billing", request_body = OrderInput,
    responses((status = 201, description = "Created", body = Checkout), (status = 200, description = "Replayed", body = Checkout)))]
async fn create_order(State(s): State<BillingService>, Access(ctx): Access, b: Body<OrderInput>) -> ApiResult {
    let input = body(&s, &ctx, b)?;
    let c = s.create_order(&ctx, input).await.map_err(api)?;
    let replayed = c.replayed;
    created(c, replayed)
}

/// One order.
#[utoipa::path(get, path = "/api/v1/billing/orders/{id}", operation_id = "billing_order", tag = "billing",
    params(("id" = String, Path, description = "Order id")),
    responses((status = 200, description = "Order", body = Order)))]
async fn order(State(s): State<BillingService>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult {
    ok(s.order(&ctx, &id).await.map_err(api)?)
}

// ---------------------------------------------------------------------------
// Invoices, payments
// ---------------------------------------------------------------------------

/// Invoices of a subject (customer portal), newest first.
#[utoipa::path(get, path = "/api/v1/billing/invoices", operation_id = "billing_invoices", tag = "billing-portal",
    params(
        ("subject" = Option<String>, Query, description = SUBJECT),
        ("status" = Option<InvoiceStatus>, Query, description = "Only this status"),
        ("limit" = Option<u32>, Query, description = "At most 500"),
    ),
    responses((status = 200, description = "Invoices", body = Items<Invoice>)))]
async fn invoices(State(s): State<BillingService>, Access(ctx): Access, q: Query) -> ApiResult {
    let status = match q.get("status") {
        None | Some("") => None,
        Some(v) => {
            Some(InvoiceStatus::parse(v).ok_or_else(|| ApiError::bad_request("invalid_parameter", "unknown status"))?)
        }
    };
    items(
        s.invoices(&ctx, q.get("subject"), status, q.u32("limit")?)
            .await
            .map_err(api)?,
    )
}

/// One invoice with its lines.
#[utoipa::path(get, path = "/api/v1/billing/invoices/{id}", operation_id = "billing_invoice", tag = "billing-portal",
    params(("id" = String, Path, description = "Invoice id")),
    responses((status = 200, description = "Invoice", body = Invoice)))]
async fn invoice(State(s): State<BillingService>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult {
    ok(s.invoice(&ctx, &id).await.map_err(api)?)
}

/// Pays an open invoice now (saved method, else the hosted page URL).
#[utoipa::path(post, path = "/api/v1/billing/invoices/{id}/pay", operation_id = "billing_pay_invoice", tag = "billing-portal", request_body = PayInput,
    params(("id" = String, Path, description = "Invoice id")),
    responses((status = 200, description = "Attempted", body = Collection)))]
async fn pay_invoice(
    State(s): State<BillingService>,
    Access(ctx): Access,
    Path(id): Path<String>,
    b: Body<PayInput>,
) -> ApiResult {
    let input = body(&s, &ctx, b)?;
    ok(s.pay_invoice(&ctx, &id, input.return_url.as_deref())
        .await
        .map_err(api)?)
}

/// Records an out-of-band payment (operators; manual billing).
#[utoipa::path(post, path = "/api/v1/billing/invoices/{id}/mark-paid", operation_id = "billing_mark_paid", tag = "billing", request_body = MarkPaidInput,
    params(("id" = String, Path, description = "Invoice id")),
    responses((status = 200, description = "Paid", body = Invoice)))]
async fn mark_paid(
    State(s): State<BillingService>,
    Access(ctx): Access,
    Path(id): Path<String>,
    b: Body<MarkPaidInput>,
) -> ApiResult {
    let input = body(&s, &ctx, b)?;
    ok(s.mark_paid(&ctx, &id, input).await.map_err(api)?)
}

/// Voids an unpaid invoice (operators).
#[utoipa::path(post, path = "/api/v1/billing/invoices/{id}/void", operation_id = "billing_void_invoice", tag = "billing",
    params(("id" = String, Path, description = "Invoice id")),
    responses((status = 200, description = "Voided", body = Invoice)))]
async fn void_invoice(State(s): State<BillingService>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult {
    ok(s.void_invoice(&ctx, &id).await.map_err(api)?)
}

/// Issues a credit note: a refund or a credit to the customer balance
/// (operators). Idempotent by key.
#[utoipa::path(post, path = "/api/v1/billing/invoices/{id}/credit-notes", operation_id = "billing_credit_invoice", tag = "billing", request_body = CreditNoteInput,
    params(("id" = String, Path, description = "Invoice id")),
    responses((status = 201, description = "Issued", body = CreditNote)))]
async fn credit_invoice(
    State(s): State<BillingService>,
    Access(ctx): Access,
    Path(id): Path<String>,
    b: Body<CreditNoteInput>,
) -> ApiResult {
    let input = body(&s, &ctx, b)?;
    created(s.credit_invoice(&ctx, &id, input).await.map_err(api)?, false)
}

/// Credit notes of a subject.
#[utoipa::path(get, path = "/api/v1/billing/credit-notes", operation_id = "billing_credit_notes", tag = "billing",
    params(("subject" = Option<String>, Query, description = SUBJECT)),
    responses((status = 200, description = "Credit notes", body = Items<CreditNote>)))]
async fn credit_notes(State(s): State<BillingService>, Access(ctx): Access, q: Query) -> ApiResult {
    items(
        s.credit_notes(&ctx, q.get("subject"), q.u32("limit")?)
            .await
            .map_err(api)?,
    )
}

/// Payments of a subject (payment history).
#[utoipa::path(get, path = "/api/v1/billing/payments", operation_id = "billing_payments", tag = "billing",
    params(("subject" = Option<String>, Query, description = SUBJECT)),
    responses((status = 200, description = "Payments", body = Items<Payment>)))]
async fn payments(State(s): State<BillingService>, Access(ctx): Access, q: Query) -> ApiResult {
    items(s.payments(&ctx, q.get("subject"), q.u32("limit")?).await.map_err(api)?)
}

/// Refunds of a subject.
#[utoipa::path(get, path = "/api/v1/billing/refunds", operation_id = "billing_refunds", tag = "billing",
    params(("subject" = Option<String>, Query, description = SUBJECT)),
    responses((status = 200, description = "Refunds", body = Items<Refund>)))]
async fn refunds(State(s): State<BillingService>, Access(ctx): Access, q: Query) -> ApiResult {
    items(s.refunds(&ctx, q.get("subject"), q.u32("limit")?).await.map_err(api)?)
}

/// The customer portal: customer, live subscriptions, recent invoices,
/// payments and credit notes.
#[utoipa::path(get, path = "/api/v1/billing/portal", operation_id = "billing_portal", tag = "billing-portal",
    params(("subject" = Option<String>, Query, description = SUBJECT)),
    responses((status = 200, description = "Portal", body = Portal)))]
async fn portal(State(s): State<BillingService>, Access(ctx): Access, q: Query) -> ApiResult {
    ok(s.portal(&ctx, q.get("subject")).await.map_err(api)?)
}

/// A provider-hosted page that saves or replaces the payment method.
#[utoipa::path(post, path = "/api/v1/billing/portal/payment-method", operation_id = "billing_payment_method", tag = "billing-portal", request_body = PaymentMethodInput,
    responses((status = 200, description = "Hosted page", body = HostedPage)))]
async fn payment_method(
    State(s): State<BillingService>,
    Access(ctx): Access,
    b: Body<PaymentMethodInput>,
) -> ApiResult {
    let input = body(&s, &ctx, b)?;
    ok(s.payment_method_page(&ctx, input).await.map_err(api)?)
}

// ---------------------------------------------------------------------------
// Webhooks, statistics, maintenance
// ---------------------------------------------------------------------------

/// A provider webhook delivery. Verified by signature (forged: `400
/// invalid_signature`; replayed old delivery: `400 stale_signature`),
/// idempotent by event id (`duplicate`), out-of-order safe.
#[utoipa::path(post, path = "/api/v1/billing/webhooks/{provider}", operation_id = "billing_webhook", tag = "billing-webhooks",
    params(("provider" = String, Path, description = "stripe or razorpay")),
    responses((status = 200, description = "Handled", body = WebhookOutcome), (status = 400, description = "Signature refused"),
        (status = 404, description = "Provider not configured")))]
async fn webhook(
    State(s): State<BillingService>,
    Path(provider): Path<String>,
    headers: HeaderMap,
    raw: Bytes,
) -> ApiResult {
    let hs: Vec<(String, String)> = headers
        .iter()
        .filter_map(|(k, v)| v.to_str().ok().map(|v| (k.as_str().to_ascii_lowercase(), v.to_owned())))
        .collect();
    ok(s.handle_webhook(&provider, &hs, &raw).await.map_err(api)?)
}

/// Webhook events received (platform administrators).
#[utoipa::path(get, path = "/api/v1/billing/webhook-events", operation_id = "billing_webhook_events", tag = "billing-webhooks",
    params(("limit" = Option<u32>, Query, description = "At most 1000")),
    responses((status = 200, description = "Events", body = Items<WebhookEvent>)))]
async fn webhook_events(State(s): State<BillingService>, Access(ctx): Access, q: Query) -> ApiResult {
    items(s.webhook_events(&ctx, q.u32("limit")?).await.map_err(api)?)
}

/// Payment statistics: MRR, ARR, churn, revenue by currency (default: the
/// last 30 days by day).
#[utoipa::path(get, path = "/api/v1/billing/stats", operation_id = "billing_stats", tag = "billing",
    params(
        ("from" = Option<String>, Query, description = "Window start (RFC 3339)"),
        ("to" = Option<String>, Query, description = "Window end (RFC 3339, default now)"),
        ("interval" = Option<String>, Query, description = "day (default) or month"),
    ),
    responses((status = 200, description = "Statistics", body = BillingStats)))]
async fn stats(State(s): State<BillingService>, Access(ctx): Access, q: Query) -> ApiResult {
    let to = time_param(&q, "to")?.unwrap_or_else(|| s.now());
    let from = time_param(&q, "from")?.unwrap_or(to - 30 * DAY);
    let bucket = match q.get("interval") {
        None | Some("") => Bucket::Day,
        Some(v) => Bucket::parse(v)
            .ok_or_else(|| ApiError::bad_request("invalid_parameter", "interval must be day or month"))?,
    };
    ok(s.stats(&ctx, from, to, bucket).await.map_err(api)?)
}

/// Runs maintenance now (platform administrators): usage, renewals,
/// dunning, fulfillment, entitlement syncs.
#[utoipa::path(post, path = "/api/v1/billing/maintenance", operation_id = "billing_maintenance", tag = "billing",
    responses((status = 200, description = "Report", body = MaintenanceReport)))]
async fn maintenance(State(s): State<BillingService>, Access(ctx): Access) -> ApiResult {
    ok(s.maintenance(&ctx).await.map_err(api)?)
}
