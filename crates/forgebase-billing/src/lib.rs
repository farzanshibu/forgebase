//! ForgeBase billing (FB-70, ADR-0021). On by default (`[billing] enabled`).
//!
//! A billing application on top of the entitlement engine (FB-24) and the
//! credit ledger (FB-73):
//!
//! | Piece | Where |
//! |---|---|
//! | Products, multi-currency prices (recurring monthly/yearly, one-time, metered; per unit, graduated, volume), tax rates, coupons | [`catalog`] |
//! | Customers (users or organizations), settings, provider accounts (test mode, sealed secrets) | [`BillingService`] |
//! | Subscriptions (trial, active, past due, paused, cancelled), plan changes with proration, one-time orders, renewals | [`subscription`] |
//! | Invoices (lines, discounts, tax, balance, gapless numbers), collection, dunning, credit notes, refunds, fulfillment, entitlement sync | [`billing`] |
//! | Metered billing from FB-24 usage events (cursor, idempotent by event id) | [`usage`] |
//! | Signed, idempotent, replay-protected webhooks | [`webhook`] |
//! | Inbound webhook inspector: every delivery recorded (budgeted when unverified), replay, re-verify (ADR-0032) | [`inspector`] |
//! | Stripe, Razorpay, WeChat Pay and manual drivers behind [`PaymentProvider`] | [`provider`] |
//! | App Store, Google Play and Creem (merchant-of-record stores, FB-72) behind [`provider::StoreProvider`]: server-side purchase verification, signed notifications, refunds and revocations mapped to subscriptions, orders and entitlements | [`store`] |
//! | MRR, ARR, churn, revenue by currency | [`stats`] |
//! | REST under `/api/v1/billing` with OpenAPI | [`http`] |
//!
//! Money is integer minor units next to an ISO 4217 code ([`money`]); there
//! are no floating point amounts. Payment-to-entitlement mapping: a paid
//! plan or add-on reaches FB-24 through `BillingSync`, a paid credits
//! product tops up FB-73 through `CreditSink`, a paid usage pack is granted
//! through `BillingSync::grant_usage_pack`.

pub mod admin;
pub mod billing;
pub mod catalog;
mod error;
pub mod http;
pub mod inspector;
pub mod jobs;
pub mod model;
pub mod money;
pub mod provider;
mod service;
pub mod stats;
pub mod store;
pub mod subscription;
pub mod usage;
pub mod webhook;

pub use error::{BillingError, Result};
pub use http::{extension, openapi, router};
pub use inspector::{
    WebhookDelivery, WebhookDeliveryDetail, WebhookDeliveryFilter, WebhookDeliveryPage, WebhookDeliveryResult,
    WebhookLogConfig, WebhookProcessing, WebhookVerification,
};
pub use jobs::register_jobs;
pub use model::*;
pub use provider::{PaymentProvider, ProviderEvent, StoreProvider};
pub use service::{
    BillingDeps, BillingService, DAY, MAINTENANCE_JOB, MAINTENANCE_SCHEDULE, USAGE_CURSOR, add_months, next_boundary,
    period_months,
};
pub use stats::Bucket;
