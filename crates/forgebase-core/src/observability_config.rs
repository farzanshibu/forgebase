//! `[observability]`: the operations insight keys (ADR-0036): the slow
//! statement threshold, statement statistics and function profiling.
//!
//! Every key is in the FB-91 registry ([`crate::features`]) and has an
//! environment variable `FORGEBASE_OBSERVABILITY_<KEY>`.

use serde::{Deserialize, Serialize};

use super::{parse_bool, parse_num};

/// `[observability]`, as the server runs with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservabilityConfig {
    /// Statements at least this long (milliseconds) are listed in the slow
    /// statement log with their redacted text. `0` records every statement.
    pub slow_query_ms: u64,
    /// Aggregate every statement by fingerprint (calls, latency quantiles,
    /// rows, errors) and flush the aggregates to the database.
    pub query_stats: bool,
    /// Seconds between two flushes of the statement and function aggregates.
    pub flush_interval_secs: u64,
    /// Fingerprints kept per flush interval (the busiest by total time).
    pub query_stats_top_n: u32,
    /// Days the aggregates, slow statements and function invocation details
    /// are kept.
    pub retention_days: u32,
    /// Profile function executions: host-call breakdown, fuel, memory,
    /// cold starts and a per-invocation timeline.
    pub function_profiling: bool,
}

impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self {
            slow_query_ms: 200,
            query_stats: true,
            flush_interval_secs: 60,
            query_stats_top_n: 200,
            retention_days: 14,
            function_profiling: true,
        }
    }
}

/// `[observability]` from the file or the environment.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservabilityOverrides {
    /// See [`ObservabilityConfig::slow_query_ms`].
    pub slow_query_ms: Option<u64>,
    /// See [`ObservabilityConfig::query_stats`].
    pub query_stats: Option<bool>,
    /// See [`ObservabilityConfig::flush_interval_secs`].
    pub flush_interval_secs: Option<u64>,
    /// See [`ObservabilityConfig::query_stats_top_n`].
    pub query_stats_top_n: Option<u32>,
    /// See [`ObservabilityConfig::retention_days`].
    pub retention_days: Option<u32>,
    /// See [`ObservabilityConfig::function_profiling`].
    pub function_profiling: Option<bool>,
}

impl ObservabilityOverrides {
    /// Reads one environment variable: `key` is the name after
    /// `FORGEBASE_`. `Ok(false)` when it is not an observability variable.
    ///
    /// # Errors
    /// A value that does not parse.
    pub fn env(&mut self, key: &str, value: &str) -> Result<bool, String> {
        let Some(k) = key.strip_prefix("OBSERVABILITY_") else {
            return Ok(false);
        };
        match k {
            "SLOW_QUERY_MS" => self.slow_query_ms = Some(parse_num(value)?),
            "QUERY_STATS" => self.query_stats = Some(parse_bool(value)?),
            "FLUSH_INTERVAL_SECS" => self.flush_interval_secs = Some(parse_num(value)?),
            "QUERY_STATS_TOP_N" => self.query_stats_top_n = Some(parse_num(value)?),
            "RETENTION_DAYS" => self.retention_days = Some(parse_num(value)?),
            "FUNCTION_PROFILING" => self.function_profiling = Some(parse_bool(value)?),
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// Dotted registry keys this layer sets.
    pub fn keys(&self, put: &mut dyn FnMut(bool, &'static str)) {
        put(self.slow_query_ms.is_some(), "observability.slow_query_ms");
        put(self.query_stats.is_some(), "observability.query_stats");
        put(self.flush_interval_secs.is_some(), "observability.flush_interval_secs");
        put(self.query_stats_top_n.is_some(), "observability.query_stats_top_n");
        put(self.retention_days.is_some(), "observability.retention_days");
        put(self.function_profiling.is_some(), "observability.function_profiling");
    }
}

impl ObservabilityConfig {
    /// Applies one layer.
    pub fn apply(&mut self, o: &ObservabilityOverrides) {
        if let Some(v) = o.slow_query_ms {
            self.slow_query_ms = v;
        }
        if let Some(v) = o.query_stats {
            self.query_stats = v;
        }
        if let Some(v) = o.flush_interval_secs {
            self.flush_interval_secs = v.max(1);
        }
        if let Some(v) = o.query_stats_top_n {
            self.query_stats_top_n = v.clamp(1, 5000);
        }
        if let Some(v) = o.retention_days {
            self.retention_days = v.max(1);
        }
        if let Some(v) = o.function_profiling {
            self.function_profiling = v;
        }
    }
}
