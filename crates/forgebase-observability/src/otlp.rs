//! OTLP/HTTP export of spans and log records (ADR-0035 §1).
//!
//! A bounded queue feeds one exporter thread with its own single-threaded
//! Tokio runtime, so export never runs on (or blocks) the server's workers.
//! The thread sends batches of up to [`OtlpOptions::batch_size`] items every
//! [`OtlpOptions::flush_interval`] (or as soon as a batch is full) to
//! `<endpoint>/v1/traces` and `<endpoint>/v1/logs`, encoded as protobuf
//! (`application/x-protobuf`, the OTLP default) or JSON
//! (`application/json`), both by hand from the OTLP 1.x schema: no gRPC, no
//! generated code. A full queue drops new items and counts them
//! (`forgebase_otlp_dropped_total`); a failed request is retried twice on
//! connection errors and `429`/`502`/`503`/`504`, then dropped and counted.
//!
//! Nothing here logs through `tracing` except under the target
//! `forgebase_observability::otlp`, which the log export skips (no feedback
//! loop).

use std::sync::OnceLock;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::time::{Duration, Instant};

use forgebase_core::config::telemetry_config::{OtlpConfig, OtlpProtocol};

use crate::names;

/// A span or log attribute value.
#[derive(Debug, Clone, PartialEq)]
pub enum AttrValue {
    /// Text.
    Str(String),
    /// Boolean.
    Bool(bool),
    /// 64-bit integer.
    Int(i64),
    /// Float.
    F64(f64),
}

impl AttrValue {
    /// The value as text.
    pub fn to_text(&self) -> String {
        match self {
            AttrValue::Str(s) => s.clone(),
            AttrValue::Bool(b) => b.to_string(),
            AttrValue::Int(i) => i.to_string(),
            AttrValue::F64(f) => f.to_string(),
        }
    }
}

/// OTLP span kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanKind {
    /// Internal work.
    Internal,
    /// An incoming request.
    Server,
    /// An outgoing request.
    Client,
    /// A message sent.
    Producer,
    /// A message received.
    Consumer,
}

impl SpanKind {
    /// From an `otel.kind` field value (unknown: internal).
    pub fn parse(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "server" => SpanKind::Server,
            "client" => SpanKind::Client,
            "producer" => SpanKind::Producer,
            "consumer" => SpanKind::Consumer,
            _ => SpanKind::Internal,
        }
    }

    pub(crate) fn code(self) -> u64 {
        match self {
            SpanKind::Internal => 1,
            SpanKind::Server => 2,
            SpanKind::Client => 3,
            SpanKind::Producer => 4,
            SpanKind::Consumer => 5,
        }
    }
}

/// OTLP status code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusCode {
    /// Not set.
    Unset,
    /// Ok.
    Ok,
    /// Error.
    Error,
}

impl StatusCode {
    pub(crate) fn code(self) -> u64 {
        match self {
            StatusCode::Unset => 0,
            StatusCode::Ok => 1,
            StatusCode::Error => 2,
        }
    }
}

/// A finished span.
#[derive(Debug, Clone, PartialEq)]
pub struct SpanData {
    /// Trace id.
    pub trace_id: [u8; 16],
    /// Span id.
    pub span_id: [u8; 8],
    /// Parent span id (local or remote).
    pub parent_span_id: Option<[u8; 8]>,
    /// W3C `tracestate`.
    pub trace_state: Option<String>,
    /// Name.
    pub name: String,
    /// Kind.
    pub kind: SpanKind,
    /// Start, Unix nanoseconds.
    pub start_ns: u64,
    /// End, Unix nanoseconds.
    pub end_ns: u64,
    /// Attributes.
    pub attributes: Vec<(String, AttrValue)>,
    /// Status.
    pub status: StatusCode,
    /// Status message.
    pub status_message: String,
}

/// One log record.
#[derive(Debug, Clone, PartialEq)]
pub struct LogData {
    /// Time, Unix nanoseconds.
    pub time_ns: u64,
    /// Level.
    pub severity: tracing::Level,
    /// The message.
    pub body: String,
    /// `tracing` target (`code.namespace`).
    pub target: String,
    /// The other fields.
    pub attributes: Vec<(String, AttrValue)>,
    /// Trace id, span id and sampled flag of the span it was logged in.
    pub trace: Option<([u8; 16], [u8; 8], bool)>,
}

/// Exporter settings, resolved from `[observability.otlp]`.
#[derive(Debug, Clone)]
pub struct OtlpOptions {
    /// Full URL for spans.
    pub traces_url: String,
    /// Full URL for logs.
    pub logs_url: String,
    /// Body encoding.
    pub protocol: OtlpProtocol,
    /// Request headers (secrets resolved).
    pub headers: Vec<(String, String)>,
    /// Resource attributes (`service.name` first).
    pub resource: Vec<(String, AttrValue)>,
    /// Timeout of one request.
    pub timeout: Duration,
    /// Extra CA certificates (PEM) for an `https://` collector.
    pub ca_pem: Option<Vec<u8>>,
    /// Export spans.
    pub export_traces: bool,
    /// Export logs.
    pub export_logs: bool,
    /// Items per request.
    pub batch_size: usize,
    /// Items waiting before new ones are dropped.
    pub queue_size: usize,
    /// Longest wait before a partial batch is sent.
    pub flush_interval: Duration,
}

/// Why the exporter could not be set up.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OtlpError {
    /// A setting cannot be used.
    #[error("{0}")]
    Config(String),
}

impl OtlpOptions {
    /// Options from the config, reading `env:NAME` header values and
    /// `FORGEBASE_OTLP_HEADERS` from `env` (pass `|k| std::env::var(k).ok()`).
    pub fn from_config(
        c: &OtlpConfig,
        instance_id: Option<&str>,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, OtlpError> {
        let base = c.endpoint.trim_end_matches('/');
        let url = |signal: &str| {
            if base.ends_with("/v1/traces") || base.ends_with("/v1/logs") {
                let root = base.rsplit_once("/v1/").map_or(base, |(r, _)| r);
                format!("{root}/v1/{signal}")
            } else {
                format!("{base}/v1/{signal}")
            }
        };
        let mut headers = Vec::new();
        for (k, v) in &c.headers {
            let value = match v.strip_prefix("env:") {
                Some(var) => env(var.trim()).ok_or_else(|| {
                    OtlpError::Config(format!(
                        "observability.otlp.headers.{k} reads the environment variable {var}, which is not set"
                    ))
                })?,
                None => v.clone(),
            };
            headers.push((k.to_ascii_lowercase(), value));
        }
        if let Some(extra) = env("FORGEBASE_OTLP_HEADERS") {
            for pair in extra.split(',').map(str::trim).filter(|p| !p.is_empty()) {
                let (k, v) = pair.split_once('=').ok_or_else(|| {
                    OtlpError::Config("FORGEBASE_OTLP_HEADERS must be name=value pairs separated by commas".into())
                })?;
                let v = percent_decode(v.trim());
                if v.contains(['\r', '\n', '\0']) {
                    return Err(OtlpError::Config(
                        "FORGEBASE_OTLP_HEADERS holds a control character".into(),
                    ));
                }
                headers.push((k.trim().to_ascii_lowercase(), v));
            }
        }
        let mut resource = vec![
            ("service.name".to_owned(), AttrValue::Str(c.service_name.clone())),
            (
                "service.version".to_owned(),
                AttrValue::Str(forgebase_core::build::VERSION.to_owned()),
            ),
            (
                "service.instance.id".to_owned(),
                AttrValue::Str(match (c.service_instance_id.trim(), instance_id) {
                    ("", Some(id)) => id.to_owned(),
                    ("", None) => {
                        let id = crate::trace::new_trace_id();
                        crate::trace::hex(&id)
                    }
                    (id, _) => id.to_owned(),
                }),
            ),
            ("telemetry.sdk.name".to_owned(), AttrValue::Str("forgebase".into())),
            ("telemetry.sdk.language".to_owned(), AttrValue::Str("rust".into())),
        ];
        for (k, v) in &c.resource_attributes {
            resource.retain(|(n, _)| n != k);
            resource.push((k.clone(), AttrValue::Str(v.clone())));
        }
        let ca_pem = if c.ca_file.trim().is_empty() {
            None
        } else {
            Some(
                std::fs::read(c.ca_file.trim())
                    .map_err(|e| OtlpError::Config(format!("observability.otlp.ca_file {}: {e}", c.ca_file.trim())))?,
            )
        };
        Ok(Self {
            traces_url: url("traces"),
            logs_url: url("logs"),
            protocol: c.protocol,
            headers,
            resource,
            timeout: Duration::from_secs(c.timeout_secs.max(1)),
            ca_pem,
            export_traces: c.export_traces,
            export_logs: c.export_logs,
            batch_size: 512,
            queue_size: 8192,
            flush_interval: Duration::from_secs(2),
        })
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 3 <= b.len()
            && let Some(v) = std::str::from_utf8(&b[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

enum Item {
    Span(SpanData),
    Log(LogData),
    Flush(mpsc::Sender<()>),
}

/// Handle of the running exporter.
#[derive(Debug)]
pub struct Exporter {
    tx: SyncSender<Item>,
}

impl std::fmt::Debug for Item {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Item::Span(_) => "span",
            Item::Log(_) => "log",
            Item::Flush(_) => "flush",
        })
    }
}

static EXPORTER: OnceLock<Exporter> = OnceLock::new();

/// The running exporter, if [`start`] was called.
pub fn exporter() -> Option<&'static Exporter> {
    EXPORTER.get()
}

impl Exporter {
    fn send(&self, item: Item, signal: &'static str) {
        match self.tx.try_send(item) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                metrics::counter!(names::OTLP_DROPPED_TOTAL, "signal" => signal, "reason" => "queue_full").increment(1);
            }
            Err(TrySendError::Disconnected(_)) => {}
        }
    }

    /// Queues a finished span.
    pub fn send_span(&self, s: SpanData) {
        self.send(Item::Span(s), "traces");
    }

    /// Queues a log record.
    pub fn send_log(&self, l: LogData) {
        self.send(Item::Log(l), "logs");
    }
}

/// Starts the exporter thread (once per process; a second call returns the
/// first exporter).
pub fn start(opts: OtlpOptions) -> Result<&'static Exporter, OtlpError> {
    if let Some(e) = EXPORTER.get() {
        return Ok(e);
    }
    let tls = crate::http_client::tls_config(opts.ca_pem.as_deref()).map_err(OtlpError::Config)?;
    let (tx, rx) = mpsc::sync_channel(opts.queue_size.max(16));
    std::thread::Builder::new()
        .name("forgebase-otlp".into())
        .spawn(move || run(opts, tls, rx))
        .map_err(|e| OtlpError::Config(format!("cannot start the OTLP exporter thread: {e}")))?;
    Ok(EXPORTER.get_or_init(|| Exporter { tx }))
}

/// Sends everything queued so far and waits up to `timeout` for it to be
/// exported (or dropped). `false`: no exporter, or it did not finish in time.
pub fn flush(timeout: Duration) -> bool {
    let Some(e) = EXPORTER.get() else { return false };
    let (tx, rx) = mpsc::channel();
    if e.tx.send(Item::Flush(tx)).is_err() {
        return false;
    }
    rx.recv_timeout(timeout).is_ok()
}

fn run(opts: OtlpOptions, tls: std::sync::Arc<rustls::ClientConfig>, rx: Receiver<Item>) {
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("forgebase: OTLP exporter disabled: cannot start its runtime: {e}");
            return;
        }
    };
    let mut spans: Vec<SpanData> = Vec::new();
    let mut logs: Vec<LogData> = Vec::new();
    let mut deadline = Instant::now() + opts.flush_interval;
    let mut last_warn: Option<Instant> = None;
    loop {
        let wait = deadline.saturating_duration_since(Instant::now());
        let mut flushers = Vec::new();
        match rx.recv_timeout(wait) {
            Ok(Item::Span(s)) => {
                if opts.export_traces {
                    spans.push(s);
                }
            }
            Ok(Item::Log(l)) => {
                if opts.export_logs {
                    logs.push(l);
                }
            }
            Ok(Item::Flush(done)) => flushers.push(done),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        let due = Instant::now() >= deadline;
        if due || !flushers.is_empty() || spans.len() >= opts.batch_size || logs.len() >= opts.batch_size {
            // Drain what is already queued before a flush answers.
            if !flushers.is_empty() {
                while let Ok(item) = rx.try_recv() {
                    match item {
                        Item::Span(s) if opts.export_traces => spans.push(s),
                        Item::Log(l) if opts.export_logs => logs.push(l),
                        Item::Flush(d) => flushers.push(d),
                        _ => {}
                    }
                }
            }
            while !spans.is_empty() {
                let n = spans.len().min(opts.batch_size);
                let batch: Vec<SpanData> = spans.drain(..n).collect();
                let body = crate::otlp_encode::traces(&opts, &batch);
                export(
                    &rt,
                    &opts,
                    &tls,
                    &opts.traces_url,
                    body,
                    "traces",
                    batch.len(),
                    &mut last_warn,
                );
            }
            while !logs.is_empty() {
                let n = logs.len().min(opts.batch_size);
                let batch: Vec<LogData> = logs.drain(..n).collect();
                let body = crate::otlp_encode::logs(&opts, &batch);
                export(
                    &rt,
                    &opts,
                    &tls,
                    &opts.logs_url,
                    body,
                    "logs",
                    batch.len(),
                    &mut last_warn,
                );
            }
            for f in flushers {
                let _ = f.send(());
            }
            deadline = Instant::now() + opts.flush_interval;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn export(
    rt: &tokio::runtime::Runtime,
    opts: &OtlpOptions,
    tls: &std::sync::Arc<rustls::ClientConfig>,
    url: &str,
    body: Vec<u8>,
    signal: &'static str,
    items: usize,
    last_warn: &mut Option<Instant>,
) {
    let content_type = match opts.protocol {
        OtlpProtocol::HttpProtobuf => "application/x-protobuf",
        OtlpProtocol::HttpJson => "application/json",
    };
    let mut error = String::new();
    for attempt in 0..3u32 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(250 * u64::from(attempt * attempt)));
        }
        let r = rt.block_on(crate::http_client::post(
            url,
            &opts.headers,
            content_type,
            &body,
            opts.timeout,
            tls,
        ));
        match r {
            Ok(status) if (200..300).contains(&status) => {
                metrics::counter!(names::OTLP_EXPORTED_TOTAL, "signal" => signal).increment(items as u64);
                return;
            }
            Ok(status) if matches!(status, 429 | 502 | 503 | 504) => error = format!("HTTP {status}"),
            Ok(status) => {
                error = format!("HTTP {status}");
                break;
            }
            Err(e) => error = e,
        }
    }
    metrics::counter!(names::OTLP_DROPPED_TOTAL, "signal" => signal, "reason" => "export_failed")
        .increment(items as u64);
    // At most one warning a minute.
    if last_warn.is_none_or(|t| t.elapsed() >= Duration::from_secs(60)) {
        *last_warn = Some(Instant::now());
        tracing::warn!(target: "forgebase_observability::otlp", %url, %error, items, "OTLP export failed; items dropped");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_headers_and_resource() {
        let mut c = forgebase_core::config::telemetry_config::ObservabilityConfig::default().otlp;
        c.endpoint = "https://collector.example.com/".into();
        c.headers.insert("X-Team".into(), "env:TEAM_KEY".into());
        c.headers.insert("x-plain".into(), "v".into());
        c.resource_attributes
            .insert("deployment.environment.name".into(), "prod".into());
        let env = |k: &str| match k {
            "TEAM_KEY" => Some("secret".to_owned()),
            "FORGEBASE_OTLP_HEADERS" => Some("authorization=Bearer%20abc, x-two = 2".to_owned()),
            _ => None,
        };
        let o = OtlpOptions::from_config(&c, Some("inst-1"), env).expect("options");
        assert_eq!(o.traces_url, "https://collector.example.com/v1/traces");
        assert_eq!(o.logs_url, "https://collector.example.com/v1/logs");
        assert!(o.headers.contains(&("x-team".into(), "secret".into())));
        assert!(o.headers.contains(&("x-plain".into(), "v".into())));
        assert!(o.headers.contains(&("authorization".into(), "Bearer abc".into())));
        assert!(o.headers.contains(&("x-two".into(), "2".into())));
        assert_eq!(
            o.resource[0],
            ("service.name".into(), AttrValue::Str("forgebase".into()))
        );
        assert!(
            o.resource
                .contains(&("service.instance.id".into(), AttrValue::Str("inst-1".into())))
        );
        assert!(
            o.resource
                .contains(&("deployment.environment.name".into(), AttrValue::Str("prod".into())))
        );
        c.endpoint = "http://c:4318/v1/traces".into();
        let o = OtlpOptions::from_config(&c, None, env).expect("options");
        assert_eq!(o.logs_url, "http://c:4318/v1/logs");
        let missing = OtlpOptions::from_config(&c, None, |_| None).unwrap_err();
        assert!(missing.to_string().contains("TEAM_KEY"), "{missing}");
    }
}
