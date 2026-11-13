//! Alerting and SLOs (ADR-0035).
//!
//! * **Rules** ([`rules`]): an expression ([`expr`], a PromQL subset) over
//!   this process's Prometheus metrics and built-in signals (`forgebase:*`:
//!   queue backlog, oldest unacknowledged age, dead letters, paused push
//!   subscriptions, disk space, backup age, SLO burn rates), a threshold, a
//!   `for` duration, a severity, labels and annotations. A built-in pack
//!   (each switchable), rules from `[[alerts.rules]]` and rules created
//!   through the API.
//! * **Alerts**: each rule and label set is one alert, `pending` while the
//!   condition holds for less than `for`, then `firing`, then `resolved`,
//!   with every change recorded ([`engine`]).
//! * **Notifications** ([`notify`]): notifiers (signed webhook, Slack,
//!   email, PagerDuty Events v2, a queue topic) group firing alerts by
//!   labels, are told about new, changed, repeated and resolved groups
//!   through a delivery outbox with retries; silences mute matching alerts.
//! * **SLOs** ([`slo`]): availability or latency objectives over route
//!   groups, counted per minute from every HTTP request, with error budgets
//!   and multi-window multi-burn-rate alerts.
//! * **HTTP** ([`http`]): `/api/v1/alerts` and `/api/v1/slos`, platform
//!   administrators only.

pub mod engine;
pub mod error;
pub mod expr;
pub mod http;
pub mod model;
pub mod notify;
pub mod rules;
pub mod samples;
pub mod service;
pub mod slo;
mod store;

pub use engine::RoundReport;
pub use error::AlertError;
pub use http::{extension, openapi, router};
pub use model::*;
pub use service::{AlertDeps, AlertService, AlertSettings, MetricsSource, PAGERDUTY_EVENTS_URL, ProcessMetrics};
