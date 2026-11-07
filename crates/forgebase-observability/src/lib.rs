//! Logging and metrics.
//!
//! * [`init_logging`] installs a global `tracing` subscriber writing to
//!   stderr, human-readable or JSON lines, filtered by a `tracing` directive.
//! * [`metrics_handle`] installs the process-wide Prometheus recorder once
//!   and returns the handle used to render `/metrics`. Crates record through
//!   the `metrics` macros with the names in [`names`].

use std::io::IsTerminal;
use std::sync::OnceLock;

use forgebase_core::LogFormat;
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Metric names. Keep them stable: dashboards and alerts depend on them.
pub mod names {
    /// Counter: HTTP requests, labels `method`, `route`, `status`.
    pub const HTTP_REQUESTS_TOTAL: &str = "forgebase_http_requests_total";
    /// Histogram: HTTP request latency in seconds, labels `method`, `route`.
    pub const HTTP_REQUEST_DURATION_SECONDS: &str = "forgebase_http_request_duration_seconds";
    /// Gauge: requests currently being served.
    pub const HTTP_REQUESTS_IN_FLIGHT: &str = "forgebase_http_requests_in_flight";
    /// Gauge: always 1, labels `version`, `os`, `arch`.
    pub const BUILD_INFO: &str = "forgebase_build_info";
    /// Gauge: seconds since the server started.
    pub const UPTIME_SECONDS: &str = "forgebase_uptime_seconds";
    /// Gauge: size of the primary database file in bytes.
    pub const DB_SIZE_BYTES: &str = "forgebase_db_size_bytes";
    /// Gauge: size of the write-ahead log in bytes.
    pub const DB_WAL_SIZE_BYTES: &str = "forgebase_db_wal_size_bytes";
    /// Gauge: applied schema version.
    pub const DB_SCHEMA_VERSION: &str = "forgebase_db_schema_version";
}

/// Latency buckets in seconds (1 ms … 10 s).
const LATENCY_BUCKETS: &[f64] = &[
    0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

/// Errors from observability setup.
#[derive(Debug, thiserror::Error)]
pub enum ObservabilityError {
    /// The log filter directive does not parse.
    #[error("invalid log level {level:?}: {message}")]
    InvalidLogLevel {
        /// The directive.
        level: String,
        /// Parser message.
        message: String,
    },
}

impl forgebase_core::ErrorCode for ObservabilityError {
    fn code(&self) -> &'static str {
        "config_value_invalid"
    }
    fn kind(&self) -> forgebase_core::ErrorKind {
        forgebase_core::ErrorKind::InvalidInput
    }
}

/// Installs the global log subscriber. A second call is a no-op (the first
/// subscriber stays), so tests and embedders can call it freely.
pub fn init_logging(level: &str, format: LogFormat) -> Result<(), ObservabilityError> {
    let filter = EnvFilter::try_new(level).map_err(|e| ObservabilityError::InvalidLogLevel {
        level: level.to_owned(),
        message: e.to_string(),
    })?;
    let registry = tracing_subscriber::registry().with(filter);
    let result = match format {
        LogFormat::Json => registry
            .with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .with_current_span(true)
                    .with_span_list(false)
                    .with_writer(std::io::stderr),
            )
            .try_init(),
        LogFormat::Pretty => registry
            .with(
                tracing_subscriber::fmt::layer()
                    .compact()
                    .with_ansi(std::io::stderr().is_terminal())
                    .with_writer(std::io::stderr),
            )
            .try_init(),
    };
    if result.is_err() {
        tracing::debug!("log subscriber already installed; keeping the existing one");
    }
    Ok(())
}

/// The process-wide Prometheus handle, installing the recorder on first use.
///
/// If another global `metrics` recorder was installed first (an embedder's),
/// the returned handle renders an empty registry and a warning is logged.
pub fn metrics_handle() -> &'static PrometheusHandle {
    static HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();
    HANDLE.get_or_init(|| {
        let builder = PrometheusBuilder::new()
            .set_buckets_for_metric(
                Matcher::Full(names::HTTP_REQUEST_DURATION_SECONDS.to_owned()),
                LATENCY_BUCKETS,
            )
            .unwrap_or_else(|_| PrometheusBuilder::new());
        let recorder = builder.build_recorder();
        let handle = recorder.handle();
        if metrics::set_global_recorder(recorder).is_err() {
            tracing::warn!("a metrics recorder was already installed; /metrics will be empty");
        }
        describe();
        metrics::gauge!(
            names::BUILD_INFO,
            "version" => forgebase_core::build::VERSION,
            "os" => forgebase_core::build::OS,
            "arch" => forgebase_core::build::ARCH
        )
        .set(1.0);
        handle
    })
}

fn describe() {
    use metrics::{Unit, describe_counter, describe_gauge, describe_histogram};
    describe_counter!(names::HTTP_REQUESTS_TOTAL, "HTTP requests served.");
    describe_histogram!(
        names::HTTP_REQUEST_DURATION_SECONDS,
        Unit::Seconds,
        "HTTP request latency."
    );
    describe_gauge!(names::HTTP_REQUESTS_IN_FLIGHT, "HTTP requests currently being served.");
    describe_gauge!(names::BUILD_INFO, "Build information (always 1).");
    describe_gauge!(
        names::UPTIME_SECONDS,
        Unit::Seconds,
        "Seconds since the server started."
    );
    describe_gauge!(names::DB_SIZE_BYTES, Unit::Bytes, "Primary database file size.");
    describe_gauge!(names::DB_WAL_SIZE_BYTES, Unit::Bytes, "Write-ahead log size.");
    describe_gauge!(names::DB_SCHEMA_VERSION, "Applied system schema version.");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_is_shared_and_renders_build_info() {
        let a = metrics_handle() as *const _;
        let b = metrics_handle() as *const _;
        assert_eq!(a, b);
        metrics::counter!(names::HTTP_REQUESTS_TOTAL, "method" => "GET", "route" => "/x", "status" => "200")
            .increment(1);
        let text = metrics_handle().render();
        assert!(text.contains("forgebase_build_info"), "{text}");
        assert!(text.contains("forgebase_http_requests_total"), "{text}");
    }

    #[test]
    fn bad_log_level_is_rejected() {
        assert!(init_logging("not a [valid filter", LogFormat::Pretty).is_err());
        assert!(init_logging("info", LogFormat::Json).is_ok());
        assert!(
            init_logging("debug", LogFormat::Pretty).is_ok(),
            "second init is a no-op"
        );
    }
}
