//! Logging, metrics and tracing.
//!
//! * [`init_logging`] installs a global `tracing` subscriber writing to
//!   stderr, human-readable or JSON lines, filtered by a `tracing` directive.
//!   [`init_telemetry`] does the same with `[observability]` (ADR-0035):
//!   trace ids in JSON lines and, when `[observability.otlp]` is on, OTLP
//!   export of spans and logs ([`trace`], [`otlp`]).
//! * [`metrics_handle`] installs the process-wide Prometheus recorder once
//!   and returns the handle used to render `/metrics`. Crates record through
//!   the `metrics` macros with the names in [`names`].
//! * [`HttpObserver`]: a hook the HTTP layer calls once per request (the SLO
//!   engine counts good and bad requests through it).

mod http_client;
pub mod otlp;
mod otlp_encode;
pub mod trace;

use std::io::IsTerminal;
use std::sync::OnceLock;

use forgebase_core::LogFormat;
use forgebase_core::config::telemetry_config::ObservabilityConfig;
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};
use tracing::{Event, Level, Metadata, Subscriber};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::fmt::format::{FormatEvent, FormatFields, Writer};
use tracing_subscriber::fmt::{FmtContext, format};
use tracing_subscriber::layer::{Context, Filter, Layer, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;

/// Told about every HTTP request once it is answered (ADR-0035 §5). Keep it
/// cheap: it runs on the request path.
pub trait HttpObserver: Send + Sync + std::fmt::Debug {
    /// One request: method, matched route template (`<unmatched>` when no
    /// route matched), status code and latency in seconds.
    fn observe(&self, method: &str, route: &str, status: u16, seconds: f64);
}

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
    /// Counter: inbound billing webhook deliveries, labels `provider` (a known
    /// provider or `other`) and `outcome` (`applied`, `ignored`, `duplicate`,
    /// `failed`, or the verification failure).
    pub const BILLING_WEBHOOK_DELIVERIES_TOTAL: &str = "forgebase_billing_webhook_deliveries_total";
    /// Histogram: verification and processing time of a billing webhook in seconds, label `provider`.
    pub const BILLING_WEBHOOK_PROCESSING_SECONDS: &str = "forgebase_billing_webhook_processing_seconds";
    /// Counter: unverified billing webhook deliveries the inspector budget did not store,
    /// labels `provider` and `kind` (`body`: row without body; `delivery`: no row).
    pub const BILLING_WEBHOOK_LOG_DROPPED_TOTAL: &str = "forgebase_billing_webhook_log_dropped_total";
    /// Counter: push delivery attempts (test sends excluded), labels `subscription`, `outcome` (`success`, `failure`).
    pub const QUEUE_PUSH_ATTEMPTS_TOTAL: &str = "forgebase_queue_push_attempts_total";
    /// Histogram: push delivery latency in seconds, label `subscription`.
    pub const QUEUE_PUSH_LATENCY_SECONDS: &str = "forgebase_queue_push_latency_seconds";
    /// Gauge: failed push attempts since the last success, label `subscription`.
    pub const QUEUE_PUSH_CONSECUTIVE_FAILURES: &str = "forgebase_queue_push_consecutive_failures";
    /// Gauge: 1 while push delivery of a subscription is paused, label `subscription`.
    pub const QUEUE_PUSH_PAUSED: &str = "forgebase_queue_push_paused";
    /// Counter: push subscriptions paused by the circuit breaker, label `subscription`.
    pub const QUEUE_PUSH_AUTO_PAUSED_TOTAL: &str = "forgebase_queue_push_auto_paused_total";
    /// Gauge: Unix time of the last successful push attempt, label `subscription`.
    pub const QUEUE_PUSH_LAST_SUCCESS_SECONDS: &str = "forgebase_queue_push_last_success_timestamp_seconds";
    /// Counter: dead letters redriven to their subscription, label `subscription`.
    pub const QUEUE_REDRIVEN_TOTAL: &str = "forgebase_queue_redriven_total";
    /// Counter: spans or log records sent to the OTLP collector, label `signal` (`traces`, `logs`).
    pub const OTLP_EXPORTED_TOTAL: &str = "forgebase_otlp_exported_total";
    /// Counter: spans or log records not exported, labels `signal` and
    /// `reason` (`queue_full`, `export_failed`).
    pub const OTLP_DROPPED_TOTAL: &str = "forgebase_otlp_dropped_total";
    /// Counter: finished job attempts, labels `queue` and `outcome`
    /// (`succeeded`, `failed` = will be retried, `dead`).
    pub const JOBS_COMPLETED_TOTAL: &str = "forgebase_jobs_completed_total";
    /// Gauge: alerts by `state` (`pending`, `firing`) and `severity`.
    pub const ALERTS: &str = "forgebase_alerts";
    /// Counter: alert notification attempts, labels `notifier_type` and `outcome` (`sent`, `failed`).
    pub const ALERT_NOTIFICATIONS_TOTAL: &str = "forgebase_alert_notifications_total";
    /// Counter: rule evaluations that failed (unknown metric names are not failures), label `rule`.
    pub const ALERT_EVALUATION_FAILURES_TOTAL: &str = "forgebase_alert_evaluation_failures_total";
    /// Gauge: share of good events over the SLO window, label `slo`.
    pub const SLO_SLI: &str = "forgebase_slo_sli_ratio";
    /// Gauge: the objective (0-1), label `slo`.
    pub const SLO_OBJECTIVE: &str = "forgebase_slo_objective_ratio";
    /// Gauge: share of the error budget left (below 0 when overspent), label `slo`.
    pub const SLO_ERROR_BUDGET_REMAINING: &str = "forgebase_slo_error_budget_remaining_ratio";
    /// Gauge: error-budget burn rate over a window, labels `slo` and `window` (`5m`, `30m`, `1h`, `2h`, `6h`, `1d`, `3d`).
    pub const SLO_BURN_RATE: &str = "forgebase_slo_burn_rate";
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
    /// `[observability.otlp]` cannot be used.
    #[error("{0}")]
    Otlp(String),
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
/// subscriber stays), so tests and embedders can call it freely. Tracing
/// export stays off; see [`init_telemetry`].
pub fn init_logging(level: &str, format: LogFormat) -> Result<(), ObservabilityError> {
    init_telemetry(level, format, &ObservabilityConfig::default(), false)
}

fn parse_filter(level: &str) -> Result<EnvFilter, ObservabilityError> {
    EnvFilter::try_new(level).map_err(|e| ObservabilityError::InvalidLogLevel {
        level: level.to_owned(),
        message: e.to_string(),
    })
}

/// Installs the global subscriber with `[observability]` (ADR-0035):
///
/// * OTLP off (the default): the log layer alone, as before, plus the
///   trace-context layer in JSON mode so each line carries `trace_id` and
///   `span_id`. Nothing is recorded or sent; nothing is propagated.
/// * OTLP on: the exporter thread starts; spans at `INFO` and above get a
///   trace context and sampled ones are exported (whatever `log.level`
///   says), log events at `log.level` too when `export_logs` is on; trace
///   context is propagated (response `traceparent`, outbound requests, queue
///   messages, jobs).
///
/// `trust_proxy` is `[auth] trust_forwarded_for`. A second call keeps the
/// first subscriber.
pub fn init_telemetry(
    level: &str,
    format: LogFormat,
    obs: &ObservabilityConfig,
    trust_proxy: bool,
) -> Result<(), ObservabilityError> {
    let filter = parse_filter(level)?;
    let otlp_cfg = &obs.otlp;
    let installed_before = tracing::dispatcher::has_been_set();
    if !installed_before {
        trace::configure(trace::TraceOptions {
            sampling_ratio: otlp_cfg.sampling_ratio,
            trust: obs.trust_incoming_trace,
            trust_proxy,
            propagate: otlp_cfg.enabled,
        });
    }
    let result = if otlp_cfg.enabled && !installed_before {
        let opts = otlp::OtlpOptions::from_config(otlp_cfg, None, |k| std::env::var(k).ok())
            .map_err(|e| ObservabilityError::Otlp(e.to_string()))?;
        let (spans, logs) = (opts.export_traces, opts.export_logs);
        otlp::start(opts).map_err(|e| ObservabilityError::Otlp(e.to_string()))?;
        let log_filter = if logs { Some(parse_filter(level)?) } else { None };
        let ctx = trace::ContextLayer::exporting(spans, logs).with_filter(TelemetryFilter { logs: log_filter });
        let registry = tracing_subscriber::registry().with(ctx);
        match format {
            LogFormat::Json => registry.with(json_layer().with_filter(filter)).try_init(),
            LogFormat::Pretty => registry.with(pretty_layer().with_filter(filter)).try_init(),
        }
    } else {
        let registry = tracing_subscriber::registry().with(filter);
        match format {
            LogFormat::Json => registry.with(trace::ContextLayer::ids_only()).with(json_layer()).try_init(),
            LogFormat::Pretty => registry.with(pretty_layer()).try_init(),
        }
    };
    if result.is_err() {
        tracing::debug!("log subscriber already installed; keeping the existing one");
    }
    Ok(())
}

fn json_layer<S>() -> impl Layer<S>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    tracing_subscriber::fmt::layer()
        .fmt_fields(format::JsonFields::new())
        .event_format(WithTraceIds(
            format().json().with_current_span(true).with_span_list(false),
        ))
        .with_writer(std::io::stderr)
}

fn pretty_layer<S>() -> impl Layer<S>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    tracing_subscriber::fmt::layer()
        .compact()
        .with_ansi(std::io::stderr().is_terminal())
        .with_writer(std::io::stderr)
}

/// The JSON event format with `trace_id` and `span_id` of the event's span
/// as the first keys (ADR-0035 §2).
struct WithTraceIds<F>(F);

impl<S, N, F> FormatEvent<S, N> for WithTraceIds<F>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'w> FormatFields<'w> + 'static,
    F: FormatEvent<S, N>,
{
    fn format_event(&self, ctx: &FmtContext<'_, S, N>, mut writer: Writer<'_>, event: &Event<'_>) -> std::fmt::Result {
        let ids = ctx.parent_span().and_then(|span| {
            let ext = span.extensions();
            ext.get::<trace::SpanState>()
                .map(|st| (st.ctx.trace_id_hex(), st.ctx.span_id_hex()))
        });
        let Some((trace_id, span_id)) = ids else {
            return self.0.format_event(ctx, writer, event);
        };
        let mut buf = String::new();
        self.0.format_event(ctx, Writer::new(&mut buf), event)?;
        match buf.strip_prefix('{') {
            Some(rest) => write!(writer, "{{\"trace_id\":\"{trace_id}\",\"span_id\":\"{span_id}\",{rest}"),
            None => writer.write_str(&buf),
        }
    }
}

/// What the trace-context layer sees while exporting: spans at `INFO` and
/// above (whatever `log.level` says, so a quiet log still traces), and
/// events at `log.level` when logs are exported (never this crate's own,
/// which would loop through the exporter).
struct TelemetryFilter {
    logs: Option<EnvFilter>,
}

impl<S> Filter<S> for TelemetryFilter
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn enabled(&self, meta: &Metadata<'_>, cx: &Context<'_, S>) -> bool {
        if meta.is_span() {
            return *meta.level() <= Level::INFO;
        }
        match &self.logs {
            Some(f) => !meta.target().starts_with("forgebase_observability") && Filter::<S>::enabled(f, meta, cx),
            None => false,
        }
    }

    fn callsite_enabled(&self, meta: &'static Metadata<'static>) -> tracing::subscriber::Interest {
        use tracing::subscriber::Interest;
        if meta.is_span() {
            return if *meta.level() <= Level::INFO {
                Interest::always()
            } else {
                Interest::never()
            };
        }
        match &self.logs {
            Some(f) if !meta.target().starts_with("forgebase_observability") => {
                Filter::<S>::callsite_enabled(f, meta)
            }
            _ => Interest::never(),
        }
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        let logs = self.logs.as_ref().and_then(|f| Filter::<S>::max_level_hint(f));
        Some(logs.map_or(LevelFilter::INFO, |l| l.max(LevelFilter::INFO)))
    }

    fn on_new_span(&self, attrs: &tracing::span::Attributes<'_>, id: &tracing::span::Id, ctx: Context<'_, S>) {
        if let Some(f) = &self.logs {
            Filter::<S>::on_new_span(f, attrs, id, ctx);
        }
    }

    fn on_record(&self, id: &tracing::span::Id, values: &tracing::span::Record<'_>, ctx: Context<'_, S>) {
        if let Some(f) = &self.logs {
            Filter::<S>::on_record(f, id, values, ctx);
        }
    }

    fn on_enter(&self, id: &tracing::span::Id, ctx: Context<'_, S>) {
        if let Some(f) = &self.logs {
            Filter::<S>::on_enter(f, id, ctx);
        }
    }

    fn on_exit(&self, id: &tracing::span::Id, ctx: Context<'_, S>) {
        if let Some(f) = &self.logs {
            Filter::<S>::on_exit(f, id, ctx);
        }
    }

    fn on_close(&self, id: tracing::span::Id, ctx: Context<'_, S>) {
        if let Some(f) = &self.logs {
            Filter::<S>::on_close(f, id, ctx);
        }
    }
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
            .and_then(|b| {
                b.set_buckets_for_metric(
                    Matcher::Full(names::BILLING_WEBHOOK_PROCESSING_SECONDS.to_owned()),
                    LATENCY_BUCKETS,
                )
            })
            .and_then(|b| {
                b.set_buckets_for_metric(
                    Matcher::Full(names::QUEUE_PUSH_LATENCY_SECONDS.to_owned()),
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
    describe_counter!(
        names::BILLING_WEBHOOK_DELIVERIES_TOTAL,
        "Inbound billing webhook deliveries by outcome."
    );
    describe_histogram!(
        names::BILLING_WEBHOOK_PROCESSING_SECONDS,
        Unit::Seconds,
        "Billing webhook verification and processing time."
    );
    describe_counter!(
        names::BILLING_WEBHOOK_LOG_DROPPED_TOTAL,
        "Unverified billing webhook deliveries not stored (over the inspector budget)."
    );
    describe_counter!(names::QUEUE_PUSH_ATTEMPTS_TOTAL, "Push delivery attempts by outcome.");
    describe_histogram!(
        names::QUEUE_PUSH_LATENCY_SECONDS,
        Unit::Seconds,
        "Push delivery latency."
    );
    describe_gauge!(
        names::QUEUE_PUSH_CONSECUTIVE_FAILURES,
        "Failed push attempts since the last success."
    );
    describe_gauge!(names::QUEUE_PUSH_PAUSED, "1 while push delivery is paused.");
    describe_counter!(
        names::QUEUE_PUSH_AUTO_PAUSED_TOTAL,
        "Push subscriptions paused by the circuit breaker."
    );
    describe_gauge!(
        names::QUEUE_PUSH_LAST_SUCCESS_SECONDS,
        Unit::Seconds,
        "Unix time of the last successful push attempt."
    );
    describe_counter!(
        names::QUEUE_REDRIVEN_TOTAL,
        "Dead letters redriven to their subscription."
    );
    describe_counter!(names::OTLP_EXPORTED_TOTAL, "Spans and log records exported over OTLP.");
    describe_counter!(names::OTLP_DROPPED_TOTAL, "Spans and log records not exported.");
    describe_counter!(names::JOBS_COMPLETED_TOTAL, "Finished job attempts by outcome.");
    describe_gauge!(names::ALERTS, "Alerts by state and severity.");
    describe_counter!(names::ALERT_NOTIFICATIONS_TOTAL, "Alert notification attempts by outcome.");
    describe_counter!(names::ALERT_EVALUATION_FAILURES_TOTAL, "Alert rule evaluations that failed.");
    describe_gauge!(names::SLO_SLI, "Share of good events over the SLO window.");
    describe_gauge!(names::SLO_OBJECTIVE, "SLO objective.");
    describe_gauge!(names::SLO_ERROR_BUDGET_REMAINING, "Share of the SLO error budget left.");
    describe_gauge!(names::SLO_BURN_RATE, "SLO error-budget burn rate over a window.");
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
