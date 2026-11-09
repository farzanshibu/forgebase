//! ForgeBase entitlements and quotas (FB-24, ADR-0010).
//!
//! **Entitlement is not quota.** A feature check answers "is it in the
//! subject's plan"; a quota check answers "is there usage left". Both run
//! server side in the FB-21 pipeline: step 4 ([`hooks::EntitlementGate`])
//! and step 6 ([`hooks::QuotaGate`]), and both are available as APIs.
//!
//! | Piece | Where |
//! |---|---|
//! | Plans, features, plan features, add-ons, usage packs, quota policies | [`admin`] (catalog, migration 0009) |
//! | Subscriptions, trials, add-ons held, overrides, enterprise limits | [`admin`]; [`BillingSync`] is FB-70's seam |
//! | Resolution (plan → trial → add-ons → enterprise limits → overrides, deny wins) | [`catalog`] |
//! | `has({feature})`, `has({permission})`, `has({plan})` | [`EntitlementService::has_feature`] and friends |
//! | Atomic consume, reserve → commit / release, leases, gauges, idempotent replay | [`EntitlementService::consume`] and friends |
//! | Periods: hour, day, month (calendar or billing cycle), lifetime, rolling | [`period`] |
//! | Alerts at 70 / 90 / 100 %, rollover, expiries | [`usage`], [`jobs`], [`AlertSink`] |
//! | Plan-based API rate limits in the HTTP layer | [`rate`] |
//! | REST under `/api/v1` with OpenAPI | [`http`] |
//!
//! Subjects are the billing subjects of ADR-0008 §2: `org:<id>` when the
//! operation acts in an organization, else the caller's principal key.

pub mod admin;
mod cache;
pub mod catalog;
pub mod clock;
mod error;
pub mod hooks;
pub mod http;
pub mod jobs;
pub mod model;
pub mod period;
pub mod rate;
mod service;
pub mod usage;

pub use admin::BillingSync;
pub use clock::{Clock, ManualClock, SystemClock};
pub use error::{EntitlementError, Result};
pub use http::{extension, openapi, router};
pub use jobs::register_jobs;
pub use model::*;
pub use service::{
    ALERT_JOB, AlertSink, DEFAULT_TTL_SECS, EntitlementService, LogSink, MAINTENANCE_JOB, MAINTENANCE_SCHEDULE,
    MAX_TTL_SECS, parse_subject, subject_key,
};
