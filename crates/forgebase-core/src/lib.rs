//! Shared foundations for every ForgeBase crate.
//!
//! This crate has no internal dependencies and no I/O-heavy dependencies. It
//! defines the pieces every other crate agrees on:
//!
//! * [`config`]: the configuration model and its precedence rules
//!   (CLI > `FORGEBASE_*` environment > `forgebase.toml` > defaults).
//! * [`error`]: the error model ([`ErrorKind`] + stable snake_case codes).
//! * [`features`]: the feature registry, its route patterns and the
//!   comment-preserving config writer (FB-91, ADR-0026).
//! * [`layout`]: the on-disk data-directory layout.
//! * [`build`]: build and version information.

pub mod build;
pub mod config;
pub mod error;
pub mod features;
pub mod layout;

pub use config::{
    AuthSettings, BillingConfig, Config, ConfigOrigin, ConfigOverrides, ConfigSources, LoadedConfig, LogFormat,
    ModuleSwitch, OpsAccess, QueueConfig, Source, StorageConfig, StorageDriverKind, Synchronous,
};
pub use error::{ErrorCode, ErrorKind};
pub use layout::DataLayout;

/// Current UTC time formatted as RFC 3339 (e.g. `2026-11-07T13:44:39.955Z`).
pub fn now_rfc3339() -> String {
    format_rfc3339(time::OffsetDateTime::now_utc())
}

/// Formats a timestamp as RFC 3339 in UTC.
pub fn format_rfc3339(ts: time::OffsetDateTime) -> String {
    ts.to_offset(time::UtcOffset::UTC)
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| ts.unix_timestamp().to_string())
}
