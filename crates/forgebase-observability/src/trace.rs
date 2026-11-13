//! Trace context (ADR-0035 §2): W3C `traceparent` / `tracestate`, span and
//! trace ids, the parent-based ratio sampler, and the [`ContextLayer`] that
//! gives every `tracing` span a trace context (and, when export is on,
//! records it for OTLP).
//!
//! Spans opt into OpenTelemetry semantics with fields named after the
//! `tracing-opentelemetry` conventions:
//!
//! | Field | Meaning |
//! |---|---|
//! | `otel.name` | Span name (default: the `tracing` span name) |
//! | `otel.kind` | `server`, `client`, `producer`, `consumer` or `internal` (default) |
//! | `otel.parent` | A remote parent as a `traceparent` value (a stored message attribute, a job's context, an accepted request header); empty means none |
//! | `otel.tracestate` | Its `tracestate` |
//! | `otel.status_code` | `ok` or `error` |
//! | `otel.status_message` | Error description |
//!
//! Only fields whose name contains a dot (`http.route`, `messaging.destination.name`,
//! `forgebase.request_id`) become span attributes; plain fields (`path`,
//! `request_id`, `error`) stay in the local logs. So nothing leaves the box
//! by accident when a log field is added.

use std::cell::Cell;
use std::fmt;
use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

use tracing::Subscriber;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::registry::LookupSpan;

use crate::otlp::{self, AttrValue, LogData, SpanData, SpanKind, StatusCode};

/// The `traceparent` header (W3C Trace Context).
pub const TRACEPARENT: &str = "traceparent";
/// The `tracestate` header.
pub const TRACESTATE: &str = "tracestate";

/// The trace context of one span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpanContext {
    /// 16-byte trace id (never all zero).
    pub trace_id: [u8; 16],
    /// 8-byte span id (never all zero).
    pub span_id: [u8; 8],
    /// Whether the trace is recorded (the `sampled` flag).
    pub sampled: bool,
    /// Vendor `tracestate`, passed through unchanged.
    pub trace_state: Option<String>,
}

impl SpanContext {
    /// `00-<trace id>-<span id>-<flags>`.
    pub fn traceparent(&self) -> String {
        format!(
            "00-{}-{}-{:02x}",
            hex(&self.trace_id),
            hex(&self.span_id),
            u8::from(self.sampled)
        )
    }

    /// The trace id as 32 lowercase hex digits.
    pub fn trace_id_hex(&self) -> String {
        hex(&self.trace_id)
    }

    /// The span id as 16 lowercase hex digits.
    pub fn span_id_hex(&self) -> String {
        hex(&self.span_id)
    }

    /// Parses a `traceparent` (and its `tracestate`). `None` when it is not
    /// a valid W3C value: wrong length or version `ff`, upper-case or
    /// non-hex digits, an all-zero trace or span id. Versions above `00`
    /// are read as `00` (the spec's forward compatibility). A `tracestate`
    /// longer than 512 bytes or with control characters is dropped.
    pub fn parse(traceparent: &str, tracestate: Option<&str>) -> Option<Self> {
        let t = traceparent.trim();
        let b = t.as_bytes();
        if b.len() < 55 {
            return None;
        }
        let version = parse_hex::<1>(&t[0..2])?;
        if version[0] == 0xff || (version[0] == 0 && b.len() != 55) || (b.len() > 55 && b[55] != b'-') {
            return None;
        }
        if b[2] != b'-' || b[35] != b'-' || b[52] != b'-' {
            return None;
        }
        let trace_id = parse_hex::<16>(&t[3..35])?;
        let span_id = parse_hex::<8>(&t[36..52])?;
        let flags = parse_hex::<1>(&t[53..55])?;
        if trace_id == [0; 16] || span_id == [0; 8] {
            return None;
        }
        let trace_state = tracestate
            .map(str::trim)
            .filter(|s| !s.is_empty() && s.len() <= 512 && s.bytes().all(|c| (0x20..0x7f).contains(&c)))
            .map(str::to_owned);
        Some(Self {
            trace_id,
            span_id,
            sampled: flags[0] & 1 == 1,
            trace_state,
        })
    }
}

/// Lowercase hex of `bytes`.
pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from(DIGITS[usize::from(b >> 4)]));
        s.push(char::from(DIGITS[usize::from(b & 0xf)]));
    }
    s
}

fn parse_hex<const N: usize>(s: &str) -> Option<[u8; N]> {
    let b = s.as_bytes();
    if b.len() != N * 2 {
        return None;
    }
    let digit = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    };
    let mut out = [0u8; N];
    for (i, o) in out.iter_mut().enumerate() {
        *o = (digit(b[2 * i])? << 4) | digit(b[2 * i + 1])?;
    }
    Some(out)
}

// ------------------------------------------------------------------ ids

thread_local! {
    static RNG: Cell<u64> = Cell::new(seed());
}

fn seed() -> u64 {
    let s = getrandom::u64().unwrap_or_else(|_| {
        // No OS randomness: the clock and the thread still make ids unique
        // enough for correlation (they are not secrets).
        let t = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9e37_79b9_7f4a_7c15);
        t ^ (std::process::id() as u64).rotate_left(32)
    });
    s | 1
}

/// splitmix64: fast, well distributed, not cryptographic (trace ids are
/// identifiers, not secrets).
fn next_u64() -> u64 {
    RNG.with(|c| {
        let mut z = c.get().wrapping_add(0x9e37_79b9_7f4a_7c15);
        c.set(z);
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    })
}

/// A random, non-zero trace id.
pub fn new_trace_id() -> [u8; 16] {
    loop {
        let mut id = [0u8; 16];
        id[..8].copy_from_slice(&next_u64().to_be_bytes());
        id[8..].copy_from_slice(&next_u64().to_be_bytes());
        if id != [0; 16] {
            return id;
        }
    }
}

/// A random, non-zero span id.
pub fn new_span_id() -> [u8; 8] {
    loop {
        let id = next_u64().to_be_bytes();
        if id != [0; 8] {
            return id;
        }
    }
}

// ------------------------------------------------------------ settings

/// How trace context is accepted and propagated (set once at start by
/// [`crate::init_telemetry`]).
#[derive(Debug, Clone, PartialEq)]
pub struct TraceOptions {
    /// Share of new traces sampled (0 to 1).
    pub sampling_ratio: f64,
    /// Incoming `traceparent` policy.
    pub trust: forgebase_core::config::telemetry_config::TraceTrust,
    /// `[auth] trust_forwarded_for`: a proxy you run is in front.
    pub trust_proxy: bool,
    /// Whether trace context is propagated: `traceparent` response headers,
    /// outbound request headers, queue message attributes and job context.
    /// On when OTLP export is on.
    pub propagate: bool,
}

impl Default for TraceOptions {
    fn default() -> Self {
        Self {
            sampling_ratio: 1.0,
            trust: forgebase_core::config::telemetry_config::TraceTrust::Auto,
            trust_proxy: false,
            propagate: false,
        }
    }
}

static OPTIONS: RwLock<Option<TraceOptions>> = RwLock::new(None);

/// Sets the process-wide options (the server does this once; tests may call
/// it again).
pub fn configure(opts: TraceOptions) {
    if let Ok(mut g) = OPTIONS.write() {
        *g = Some(opts);
    }
}

/// The process-wide options.
pub fn options() -> TraceOptions {
    OPTIONS
        .read()
        .ok()
        .and_then(|g| g.clone())
        .unwrap_or_default()
}

/// Whether trace context is propagated (headers, message attributes, jobs).
pub fn propagation_enabled() -> bool {
    OPTIONS.read().ok().is_some_and(|g| g.as_ref().is_some_and(|o| o.propagate))
}

/// The ratio sampler: whether a new trace with `trace_id` is recorded. It
/// looks only at the id, so every service with the same ratio decides the
/// same way.
pub fn ratio_sampled(trace_id: &[u8; 16], ratio: f64) -> bool {
    if ratio >= 1.0 {
        return true;
    }
    if ratio <= 0.0 || ratio.is_nan() {
        return false;
    }
    let mut low = [0u8; 8];
    low.copy_from_slice(&trace_id[8..]);
    let v = u64::from_be_bytes(low) >> 1;
    (v as f64) < ratio * (1u64 << 63) as f64
}

/// The parent an incoming request's span gets, as a `traceparent` value
/// (`None`: start a new trace). `trusted` says whether the caller may decide
/// the sampling (ADR-0035 §2): an untrusted caller keeps its trace id, but
/// the ratio sampler decides whether the trace is recorded.
pub fn accept_incoming(traceparent: Option<&str>, tracestate: Option<&str>, trusted: bool) -> Option<SpanContext> {
    use forgebase_core::config::telemetry_config::TraceTrust;
    let opts = options();
    if opts.trust == TraceTrust::Never {
        return None;
    }
    let mut ctx = SpanContext::parse(traceparent?, tracestate)?;
    let trusted = trusted || opts.trust == TraceTrust::Always;
    if !trusted {
        ctx.sampled = ratio_sampled(&ctx.trace_id, opts.sampling_ratio);
    }
    Some(ctx)
}

/// Whether a request from `peer` may decide its sampling under
/// `trust = "auto"`: behind a proxy you declared, or from this machine.
pub fn caller_trusted(peer_is_loopback: bool) -> bool {
    let o = options();
    o.trust_proxy || peer_is_loopback
}

// ------------------------------------------------------- current context

/// What the layer stores on each span.
#[derive(Debug)]
pub(crate) struct SpanState {
    pub(crate) ctx: SpanContext,
    pub(crate) parent_span_id: Option<[u8; 8]>,
    rec: Option<Box<Recording>>,
}

#[derive(Debug)]
struct Recording {
    name: String,
    kind: SpanKind,
    start_ns: u64,
    attributes: Vec<(String, AttrValue)>,
    status: StatusCode,
    status_message: String,
}

/// The trace context of the current span (`None` without the context layer
/// or outside any span).
pub fn current() -> Option<SpanContext> {
    span_context(&tracing::Span::current())
}

/// The trace context of `span`.
pub fn span_context(span: &tracing::Span) -> Option<SpanContext> {
    span.with_subscriber(|(id, dispatch)| {
        let registry = dispatch.downcast_ref::<tracing_subscriber::Registry>()?;
        let data = registry.span(id)?;
        let ext = data.extensions();
        ext.get::<SpanState>().map(|s| s.ctx.clone())
    })
    .flatten()
}

/// Adds `traceparent` (and `tracestate`) of the current span to outbound
/// request headers, unless propagation is off or the caller set them.
pub fn inject_headers(headers: &mut Vec<(String, String)>) {
    if !propagation_enabled() {
        return;
    }
    let Some(ctx) = current() else { return };
    inject_context(&ctx, headers);
}

/// Adds `ctx` as `traceparent` / `tracestate` unless already present.
pub fn inject_context(ctx: &SpanContext, headers: &mut Vec<(String, String)>) {
    if headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(TRACEPARENT)) {
        return;
    }
    headers.push((TRACEPARENT.to_owned(), ctx.traceparent()));
    if let Some(ts) = &ctx.trace_state {
        headers.push((TRACESTATE.to_owned(), ts.clone()));
    }
}

/// The current span's `traceparent` when propagation is on (for a queue
/// message attribute or a job's stored context).
pub fn current_traceparent() -> Option<(String, Option<String>)> {
    if !propagation_enabled() {
        return None;
    }
    current().map(|c| (c.traceparent(), c.trace_state))
}

// ---------------------------------------------------------------- layer

/// The `tracing` layer that assigns trace contexts and, when an exporter is
/// running, records sampled spans (and log events) for OTLP.
#[derive(Debug, Clone, Copy)]
pub struct ContextLayer {
    record_spans: bool,
    record_logs: bool,
}

impl ContextLayer {
    /// Ids only: for trace ids in JSON log lines.
    pub fn ids_only() -> Self {
        Self {
            record_spans: false,
            record_logs: false,
        }
    }

    /// Records spans and/or log events for the running exporter.
    pub fn exporting(spans: bool, logs: bool) -> Self {
        Self {
            record_spans: spans,
            record_logs: logs,
        }
    }
}

#[derive(Default)]
struct Fields {
    name: Option<String>,
    kind: Option<SpanKind>,
    parent: Option<String>,
    tracestate: Option<String>,
    status: Option<StatusCode>,
    status_message: Option<String>,
    message: Option<String>,
    attributes: Vec<(String, AttrValue)>,
    /// Events: every field (log records keep them all).
    all: bool,
}

impl Fields {
    fn put(&mut self, field: &Field, value: AttrValue) {
        let name = field.name();
        match name {
            "otel.name" => self.name = Some(value.to_text()),
            "otel.kind" => self.kind = Some(SpanKind::parse(&value.to_text())),
            "otel.parent" => self.parent = Some(value.to_text()),
            "otel.tracestate" => self.tracestate = Some(value.to_text()),
            "otel.status_code" => {
                self.status = Some(match value.to_text().to_ascii_lowercase().as_str() {
                    "ok" => StatusCode::Ok,
                    "error" => StatusCode::Error,
                    _ => StatusCode::Unset,
                });
            }
            "otel.status_message" => self.status_message = Some(value.to_text()),
            "message" if self.all => self.message = Some(value.to_text()),
            n if n.starts_with("log.") && self.all => {}
            n if self.all || n.contains('.') => self.attributes.push((n.to_owned(), value)),
            _ => {}
        }
    }
}

impl Visit for Fields {
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.put(field, AttrValue::F64(value));
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.put(field, AttrValue::Int(value));
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.put(field, AttrValue::Int(i64::try_from(value).unwrap_or(i64::MAX)));
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.put(field, AttrValue::Bool(value));
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        self.put(field, AttrValue::Str(value.to_owned()));
    }
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.put(field, AttrValue::Str(format!("{value:?}")));
    }
}

pub(crate) fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

impl<S> Layer<S> for ContextLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut f = Fields::default();
        attrs.record(&mut f);
        let remote = f
            .parent
            .as_deref()
            .filter(|p| !p.is_empty())
            .and_then(|p| SpanContext::parse(p, f.tracestate.as_deref()));
        let local = || -> Option<SpanContext> {
            let parent = if attrs.is_contextual() {
                ctx.lookup_current()
            } else {
                attrs.parent().and_then(|p| ctx.span(p))
            }?;
            let ext = parent.extensions();
            ext.get::<SpanState>().map(|s| s.ctx.clone())
        };
        let (trace_id, parent_span_id, sampled, trace_state) = match remote.or_else(local) {
            Some(p) => (p.trace_id, Some(p.span_id), p.sampled, p.trace_state),
            None => {
                let t = new_trace_id();
                (t, None, ratio_sampled(&t, options().sampling_ratio), None)
            }
        };
        let rec = (self.record_spans && sampled).then(|| {
            Box::new(Recording {
                name: f.name.take().unwrap_or_else(|| attrs.metadata().name().to_owned()),
                kind: f.kind.unwrap_or(SpanKind::Internal),
                start_ns: now_ns(),
                attributes: std::mem::take(&mut f.attributes),
                status: f.status.unwrap_or(StatusCode::Unset),
                status_message: f.status_message.take().unwrap_or_default(),
            })
        });
        span.extensions_mut().insert(SpanState {
            ctx: SpanContext {
                trace_id,
                span_id: new_span_id(),
                sampled,
                trace_state,
            },
            parent_span_id,
            rec,
        });
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        if !self.record_spans {
            return;
        }
        let Some(span) = ctx.span(id) else { return };
        let mut ext = span.extensions_mut();
        let Some(rec) = ext.get_mut::<SpanState>().and_then(|s| s.rec.as_mut()) else {
            return;
        };
        let mut f = Fields::default();
        values.record(&mut f);
        if let Some(n) = f.name {
            rec.name = n;
        }
        if let Some(k) = f.kind {
            rec.kind = k;
        }
        if let Some(s) = f.status {
            rec.status = s;
        }
        if let Some(m) = f.status_message {
            rec.status_message = m;
        }
        for (k, v) in f.attributes {
            match rec.attributes.iter_mut().find(|(n, _)| *n == k) {
                Some(slot) => slot.1 = v,
                None => rec.attributes.push((k, v)),
            }
        }
    }

    fn on_event(&self, event: &tracing::Event<'_>, ctx: Context<'_, S>) {
        if !self.record_logs {
            return;
        }
        let Some(exporter) = otlp::exporter() else { return };
        let meta = event.metadata();
        if meta.target().starts_with("forgebase_observability") {
            return;
        }
        let mut f = Fields {
            all: true,
            ..Fields::default()
        };
        event.record(&mut f);
        let trace = ctx.event_span(event).and_then(|s| {
            let ext = s.extensions();
            ext.get::<SpanState>().map(|st| st.ctx.clone())
        });
        let now = now_ns();
        exporter.send_log(LogData {
            time_ns: now,
            severity: *meta.level(),
            body: f.message.unwrap_or_default(),
            target: meta.target().to_owned(),
            attributes: f.attributes,
            trace: trace.map(|t| (t.trace_id, t.span_id, t.sampled)),
        });
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        if !self.record_spans {
            return;
        }
        let Some(span) = ctx.span(&id) else { return };
        let mut ext = span.extensions_mut();
        let Some(state) = ext.get_mut::<SpanState>() else { return };
        let Some(rec) = state.rec.take() else { return };
        let Some(exporter) = otlp::exporter() else { return };
        let rec = *rec;
        exporter.send_span(SpanData {
            trace_id: state.ctx.trace_id,
            span_id: state.ctx.span_id,
            parent_span_id: state.parent_span_id,
            trace_state: state.ctx.trace_state.clone(),
            name: rec.name,
            kind: rec.kind,
            start_ns: rec.start_ns,
            end_ns: now_ns(),
            attributes: rec.attributes,
            status: rec.status,
            status_message: rec.status_message,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traceparent_round_trip_and_validation() {
        let tp = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
        let c = SpanContext::parse(tp, Some("congo=t61rcWkgMzE")).expect("valid");
        assert_eq!(c.trace_id_hex(), "4bf92f3577b34da6a3ce929d0e0e4736");
        assert_eq!(c.span_id_hex(), "00f067aa0ba902b7");
        assert!(c.sampled);
        assert_eq!(c.trace_state.as_deref(), Some("congo=t61rcWkgMzE"));
        assert_eq!(c.traceparent(), tp);
        for bad in [
            "",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7",
            "00-00000000000000000000000000000000-00f067aa0ba902b7-01",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-0000000000000000-01",
            "00-4BF92F3577B34DA6A3CE929D0E0E4736-00f067aa0ba902b7-01",
            "ff-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01-extra",
            "00_4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
        ] {
            assert!(SpanContext::parse(bad, None).is_none(), "{bad:?}");
        }
        // A future version with extra fields is read as 00.
        let v = "01-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-00-what";
        assert!(!SpanContext::parse(v, None).expect("forward compatible").sampled);
        // A bad tracestate is dropped, the parent kept.
        let c = SpanContext::parse(tp, Some("a=\u{7}")).expect("valid");
        assert_eq!(c.trace_state, None);
    }

    #[test]
    fn ids_are_unique_and_non_zero() {
        let a = new_trace_id();
        let b = new_trace_id();
        assert_ne!(a, b);
        assert_ne!(a, [0; 16]);
        assert_ne!(new_span_id(), [0; 8]);
    }

    #[test]
    fn ratio_sampler_is_deterministic_and_proportional() {
        let mut hits = 0;
        for _ in 0..10_000 {
            let t = new_trace_id();
            assert_eq!(ratio_sampled(&t, 0.25), ratio_sampled(&t, 0.25));
            if ratio_sampled(&t, 0.25) {
                hits += 1;
            }
        }
        assert!((2000..3000).contains(&hits), "{hits}");
        let t = new_trace_id();
        assert!(ratio_sampled(&t, 1.0));
        assert!(!ratio_sampled(&t, 0.0));
    }
}
