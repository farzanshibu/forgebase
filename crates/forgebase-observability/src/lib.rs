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
    /// Gauge: resident memory of the server process in bytes (standard Prometheus name).
    pub const PROCESS_RESIDENT_MEMORY_BYTES: &str = "process_resident_memory_bytes";
    /// Counter: user plus system CPU time of the process in seconds (standard Prometheus name).
    pub const PROCESS_CPU_SECONDS_TOTAL: &str = "process_cpu_seconds_total";
    /// Histogram: database statement latency in seconds, label `mode` (`read` or `write`).
    pub const DB_QUERY_DURATION_SECONDS: &str = "forgebase_db_query_duration_seconds";
    /// Gauge: database pool connections currently checked out, label `pool` (`writer` or `reader`).
    pub const DB_POOL_IN_USE: &str = "forgebase_db_pool_connections_in_use";
    /// Gauge: database pool connections open and idle, label `pool` (`writer` or `reader`).
    pub const DB_POOL_IDLE: &str = "forgebase_db_pool_connections_idle";
    /// Gauge: jobs per state, label `status` (`queued`, `running`, `succeeded`, `dead`, `cancelled`).
    pub const JOBS: &str = "forgebase_jobs";
    /// Gauge: object storage bytes, label `kind` (`logical` for current objects, `stored` after deduplication).
    pub const STORAGE_BYTES: &str = "forgebase_storage_bytes";
    /// Gauge: object storage object count, label `kind` (`current` or `versions`).
    pub const STORAGE_OBJECTS: &str = "forgebase_storage_objects";
    /// Gauge: vector indexes by `state` (`ready` or `building`).
    pub const VECTOR_INDEXES: &str = "forgebase_vector_indexes";
    /// Gauge: vectors held in loaded vector indexes.
    pub const VECTOR_INDEXED_VECTORS: &str = "forgebase_vector_indexed_vectors";
    /// Gauge: memory used by loaded vector indexes in bytes.
    pub const VECTOR_INDEX_MEMORY_BYTES: &str = "forgebase_vector_index_memory_bytes";
    /// Gauge: keys in the key-value store.
    pub const KV_KEYS: &str = "forgebase_kv_keys";
    /// Gauge: estimated memory used by the key-value store in bytes.
    pub const KV_MEMORY_BYTES: &str = "forgebase_kv_memory_bytes";
    /// Gauge: queue deliveries by `state` (`ready`, `delayed`, `in_flight`).
    pub const QUEUE_DELIVERIES: &str = "forgebase_queue_deliveries";
    /// Gauge: messages retained across all topics.
    pub const QUEUE_MESSAGES: &str = "forgebase_queue_messages";
    /// Gauge: seconds until the served TLS certificate expires (negative
    /// once expired). Only with `[tls] mode` other than `off` (ADR-0033).
    pub const TLS_CERTIFICATE_EXPIRY_SECONDS: &str = "forgebase_tls_certificate_expiry_seconds";
    /// Counter: certificates issued over ACME.
    pub const TLS_RENEWALS_TOTAL: &str = "forgebase_tls_renewals_total";
    /// Counter: failed ACME certificate requests (each is retried with backoff).
    pub const TLS_RENEWAL_FAILURES_TOTAL: &str = "forgebase_tls_renewal_failures_total";
    /// Counter: certificate files loaded again after they changed.
    pub const TLS_RELOADS_TOTAL: &str = "forgebase_tls_reloads_total";
    /// Counter: changed certificate files that did not load (the old certificate stays).
    pub const TLS_RELOAD_FAILURES_TOTAL: &str = "forgebase_tls_reload_failures_total";
    /// Counter: TLS handshakes that failed or timed out.
    pub const TLS_HANDSHAKE_FAILURES_TOTAL: &str = "forgebase_tls_handshake_failures_total";
}

/// Resident memory and CPU time of this process, where the platform reports them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProcessStats {
    /// Resident set size in bytes.
    pub resident_bytes: Option<u64>,
    /// User plus system CPU seconds since the process started.
    pub cpu_seconds: Option<f64>,
}

/// Reads the process statistics (best effort; `None` fields are not exported).
pub fn process_stats() -> ProcessStats {
    ProcessStats {
        resident_bytes: memory_stats::memory_stats().map(|m| m.physical_mem as u64),
        cpu_seconds: cpu_time::ProcessTime::try_now()
            .ok()
            .map(|t| t.as_duration().as_secs_f64()),
    }
}

/// Prometheus text for the `process_*` metrics, to append to a scrape.
///
/// Rendered by hand because the CPU total is fractional and the `metrics`
/// counter is integral. Metrics the platform cannot report are left out.
pub fn render_process_metrics() -> String {
    use std::fmt::Write;
    let s = process_stats();
    let mut out = String::new();
    if let Some(b) = s.resident_bytes {
        let n = names::PROCESS_RESIDENT_MEMORY_BYTES;
        let _ = write!(
            out,
            "# HELP {n} Resident memory size in bytes.\n# TYPE {n} gauge\n{n} {b}\n"
        );
    }
    if let Some(c) = s.cpu_seconds {
        let n = names::PROCESS_CPU_SECONDS_TOTAL;
        let _ = write!(
            out,
            "# HELP {n} Total user and system CPU time spent in seconds.\n# TYPE {n} counter\n{n} {c}\n"
        );
    }
    out
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
            .and_then(|b| {
                b.set_buckets_for_metric(
                    Matcher::Full(names::DB_QUERY_DURATION_SECONDS.to_owned()),
                    LATENCY_BUCKETS,
                )
            })
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
    describe_histogram!(
        names::DB_QUERY_DURATION_SECONDS,
        Unit::Seconds,
        "Database statement latency."
    );
    describe_gauge!(names::DB_POOL_IN_USE, "Database pool connections checked out.");
    describe_gauge!(names::DB_POOL_IDLE, "Database pool connections open and idle.");
    describe_gauge!(names::JOBS, "Jobs by status.");
    describe_gauge!(names::STORAGE_BYTES, Unit::Bytes, "Object storage bytes.");
    describe_gauge!(names::STORAGE_OBJECTS, "Object storage objects.");
    describe_gauge!(names::VECTOR_INDEXES, "Vector indexes by state.");
    describe_gauge!(names::VECTOR_INDEXED_VECTORS, "Vectors in loaded indexes.");
    describe_gauge!(
        names::VECTOR_INDEX_MEMORY_BYTES,
        Unit::Bytes,
        "Memory used by loaded vector indexes."
    );
    describe_gauge!(names::KV_KEYS, "Keys in the key-value store.");
    describe_gauge!(names::KV_MEMORY_BYTES, Unit::Bytes, "Key-value store memory.");
    describe_gauge!(names::QUEUE_DELIVERIES, "Queue deliveries by state.");
    describe_gauge!(names::QUEUE_MESSAGES, "Messages retained in topics.");
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
    fn process_metrics_render_in_exposition_format() {
        let text = render_process_metrics();
        assert!(text.contains("# TYPE process_resident_memory_bytes gauge"), "{text}");
        assert!(text.contains("# TYPE process_cpu_seconds_total counter"), "{text}");
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
