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
pub mod sketch;

pub use config::{
    AuthSettings, BackupSettings, BackupSource, BackupTls, BillingConfig, Config, ConfigOrigin, ConfigOverrides,
    ConfigSources, LoadedConfig, LogFormat, ModuleSwitch, OpsAccess, QueueConfig, SignedUrlsMode, Source,
    StorageConfig, StorageDriverKind, Synchronous,
};
pub use error::{ErrorCode, ErrorKind};
pub use layout::DataLayout;

/// Whether a JSON value holds U+0000 in a string or an object key.
/// PostgreSQL stores neither in `text` nor in `jsonb` (where it is the
/// escape `\u0000`); SQLite would. ForgeBase refuses such values on both
/// engines, so they answer alike and a project stays convertible.
pub fn json_contains_nul(v: &serde_json::Value) -> bool {
    match v {
        serde_json::Value::String(s) => s.contains('\0'),
        serde_json::Value::Array(a) => a.iter().any(json_contains_nul),
        serde_json::Value::Object(m) => m.iter().any(|(k, v)| k.contains('\0') || json_contains_nul(v)),
        _ => false,
    }
}

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

#[cfg(test)]
mod nul_tests {
    use super::json_contains_nul;
    use serde_json::json;

    #[test]
    fn json_nul_is_found_in_strings_and_keys() {
        assert!(!json_contains_nul(&json!({"a": ["x", 1, {"b": "y"}], "n": null})));
        assert!(json_contains_nul(&json!({"a": ["x", {"b": "y\u{0}"}]})));
        assert!(json_contains_nul(&json!({"k\u{0}": 1})));
        assert!(
            !json_contains_nul(&json!("\\u0000")),
            "the six characters are text, not NUL"
        );
    }
}
