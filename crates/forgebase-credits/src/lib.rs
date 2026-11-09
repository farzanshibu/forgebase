//! ForgeBase credits (FB-73, ADR-0016).
//!
//! Credits are an **append-only ledger** per subject (a user, a service
//! account or an organization: the billing subjects of ADR-0008 §2). Every
//! top-up, grant, consumption, spend, refund, reversal and expiry is one
//! immutable ledger row; balances are derived from the ledger, kept on the
//! account row for the single-writer balance check, and checked by
//! [`CreditService::audit`]. Credits are separate from quotas (FB-24) but
//! can pay for usage: [`CreditService::consume_usage`] charges FB-24 usage
//! events of priced features, idempotent by event id, reading the event feed
//! by `seq`.
//!
//! | Piece | Where |
//! |---|---|
//! | Balances, lots, the ledger | [`CreditService::balance`], [`CreditService::lots`], [`CreditService::ledger`] |
//! | Top-ups, grants (optional expiry), spend, refunds, reversals | [`CreditService::top_up`] and friends |
//! | Prices of metered features in credits, usage consumption | [`CreditService::put_rate`], [`CreditService::consume_usage`] |
//! | Expiry and usage consumption job | [`jobs`] |
//! | Balance history, consumption by feature and period, totals | [`stats`] |
//! | FB-70's seam for purchases | [`CreditSink`] |
//! | REST under `/api/v1/credits` with OpenAPI | [`http`] |
//!
//! **The balance never goes negative**, also under concurrency: every debit
//! runs in one transaction on the single writer and its `UPDATE` carries the
//! check (`WHERE balance >= n`), so a backend with concurrent writers cannot
//! overdraw either.

mod error;
pub mod http;
pub mod jobs;
pub mod model;
mod service;
pub mod stats;
mod usage;

pub use error::{CreditError, Result};
pub use http::{extension, openapi, router};
pub use jobs::register_jobs;
pub use model::*;
pub use service::{
    CreditService, CreditSink, LedgerQuery, MAINTENANCE_JOB, MAINTENANCE_SCHEDULE, MAX_AMOUNT, USAGE_CURSOR,
};
pub use stats::Window;
