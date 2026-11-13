//! `[observability]` (trace context and OpenTelemetry export) and `[alerts]`
//! (the alerting engine and SLOs), ADR-0035.
//!
//! Kept apart from the rest of the configuration model: each section has its
//! resolved shape, its overrides (the file shape and the environment), the
//! defaults, `apply`, validation and the dotted keys a layer sets.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Whether a caller's W3C `traceparent` continues its trace (ADR-0035 §2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TraceTrust {
    /// Trusted callers (`[auth] trust_forwarded_for = true`, which says a
    /// proxy you run is in front, or a loopback peer) continue their trace
    /// with their sampling decision; other callers keep their trace id but
    /// the server's own sampler decides whether it is recorded.
    #[default]
    Auto,
    /// Every caller's trace and sampling decision is continued.
    Always,
    /// Incoming trace headers are ignored; every request starts a trace.
    Never,
}

impl TraceTrust {
    /// The config value.
    pub fn as_str(self) -> &'static str {
        match self {
            TraceTrust::Auto => "auto",
            TraceTrust::Always => "always",
            TraceTrust::Never => "never",
        }
    }
}

impl std::str::FromStr for TraceTrust {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(TraceTrust::Auto),
            "always" => Ok(TraceTrust::Always),
            "never" => Ok(TraceTrust::Never),
            other => Err(format!("{other:?}: expected auto, always or never")),
        }
    }
}

/// OTLP wire format.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum OtlpProtocol {
    /// OTLP/HTTP with protobuf bodies (the OTLP default).
    #[default]
    #[serde(rename = "http/protobuf")]
    HttpProtobuf,
    /// OTLP/HTTP with JSON bodies.
    #[serde(rename = "http/json")]
    HttpJson,
}

impl OtlpProtocol {
    /// The config value.
    pub fn as_str(self) -> &'static str {
        match self {
            OtlpProtocol::HttpProtobuf => "http/protobuf",
            OtlpProtocol::HttpJson => "http/json",
        }
    }
}

impl std::str::FromStr for OtlpProtocol {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "http/protobuf" => Ok(OtlpProtocol::HttpProtobuf),
            "http/json" => Ok(OtlpProtocol::HttpJson),
            "grpc" => Err("\"grpc\" is not supported: use http/protobuf (port 4318)".into()),
            other => Err(format!("{other:?}: expected http/protobuf or http/json")),
        }
    }
}

/// `[observability.otlp]`: export of traces and logs over OTLP/HTTP.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OtlpConfig {
    /// Export on or off (off by default).
    pub enabled: bool,
    /// Collector base URL; `/v1/traces` and `/v1/logs` are appended unless
    /// the URL already ends with one of them.
    pub endpoint: String,
    /// `http/protobuf` or `http/json`.
    pub protocol: OtlpProtocol,
    /// Extra request headers. A value `env:NAME` is read from the
    /// environment variable `NAME` at start (keep API keys out of the file);
    /// `FORGEBASE_OTLP_HEADERS` (`k=v,k2=v2`) adds more.
    pub headers: BTreeMap<String, String>,
    /// `service.name` resource attribute.
    pub service_name: String,
    /// `service.instance.id`; empty: a random id per process.
    pub service_instance_id: String,
    /// More resource attributes (`deployment.environment.name`, …).
    pub resource_attributes: BTreeMap<String, String>,
    /// Share of new traces recorded, 0 to 1 (parent-based: a sampled parent
    /// is always followed).
    pub sampling_ratio: f64,
    /// Export spans.
    pub export_traces: bool,
    /// Export log events (the lines the server logs, at `log.level`).
    pub export_logs: bool,
    /// Timeout of one export request.
    pub timeout_secs: u64,
    /// PEM file with extra CA certificates for an `https://` collector.
    pub ca_file: String,
}

/// `[observability]`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ObservabilityConfig {
    /// Whether incoming `traceparent` headers continue the caller's trace.
    pub trust_incoming_trace: TraceTrust,
    /// OTLP export.
    pub otlp: OtlpConfig,
}

impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self {
            trust_incoming_trace: TraceTrust::Auto,
            otlp: OtlpConfig {
                enabled: false,
                endpoint: "http://127.0.0.1:4318".into(),
                protocol: OtlpProtocol::HttpProtobuf,
                headers: BTreeMap::new(),
                service_name: "forgebase".into(),
                service_instance_id: String::new(),
                resource_attributes: BTreeMap::new(),
                sampling_ratio: 1.0,
                export_traces: true,
                export_logs: false,
                timeout_secs: 10,
                ca_file: String::new(),
            },
        }
    }
}

/// One `[[alerts.rules]]` entry: a custom rule declared in the file
/// (validated by the alerting engine at start).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlertRuleDecl {
    /// Rule name (the `alertname` label): letters, digits, `_`, `-`, `.`.
    pub name: String,
    /// Expression (PromQL subset, ADR-0035 §3).
    pub expr: String,
    /// Comparison against `threshold`: `>`, `>=`, `<`, `<=`, `==`, `!=`.
    #[serde(default)]
    pub op: Option<String>,
    /// Threshold.
    pub threshold: f64,
    /// How long the condition must hold before the alert fires (`5m`).
    #[serde(default, rename = "for")]
    pub for_: Option<String>,
    /// `info`, `warning` (default) or `critical`.
    #[serde(default)]
    pub severity: Option<String>,
    /// Extra labels.
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    /// `summary`, `description`, `runbook_url`, …; `{{ $labels.x }}` and
    /// `{{ $value }}` are expanded.
    #[serde(default)]
    pub annotations: BTreeMap<String, String>,
}

/// `[alerts]`: the alerting engine, notifiers and SLOs.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AlertsConfig {
    /// Whether `/api/v1/alerts` and `/api/v1/slos` are served and rules are evaluated.
    pub enabled: bool,
    /// Seconds between evaluation rounds.
    pub evaluation_interval_secs: u64,
    /// Built-in rules (by id) that never run.
    pub disabled_rules: Vec<String>,
    /// Let notifiers use `http://` URLs (default: `https://` only).
    pub allow_http: bool,
    /// Days resolved alerts and notification deliveries are kept.
    pub history_retention_days: u64,
    /// Gauge holding the Unix time a TLS certificate expires (the built-in
    /// certificate rules read it).
    pub tls_cert_expiry_metric: String,
    /// Custom rules from the file.
    pub rules: Vec<AlertRuleDecl>,
}

impl Default for AlertsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            evaluation_interval_secs: 30,
            disabled_rules: Vec::new(),
            allow_http: false,
            history_retention_days: 30,
            tls_cert_expiry_metric: "forgebase_tls_certificate_expiry_timestamp_seconds".into(),
            rules: Vec::new(),
        }
    }
}

/// `[observability.otlp]` from the file or the environment.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OtlpOverrides {
    /// Export on or off.
    pub enabled: Option<bool>,
    /// Collector URL.
    pub endpoint: Option<String>,
    /// Wire format.
    pub protocol: Option<OtlpProtocol>,
    /// Request headers (replace the file's).
    pub headers: Option<BTreeMap<String, String>>,
    /// `service.name`.
    pub service_name: Option<String>,
    /// `service.instance.id`.
    pub service_instance_id: Option<String>,
    /// Resource attributes.
    pub resource_attributes: Option<BTreeMap<String, String>>,
    /// Sampling ratio.
    pub sampling_ratio: Option<f64>,
    /// Export spans.
    pub export_traces: Option<bool>,
    /// Export logs.
    pub export_logs: Option<bool>,
    /// Request timeout.
    pub timeout_secs: Option<u64>,
    /// Extra CA file.
    pub ca_file: Option<String>,
}

/// `[observability]` from the file or the environment.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservabilityOverrides {
    /// Trace header trust.
    pub trust_incoming_trace: Option<TraceTrust>,
    /// `[observability.otlp]`.
    pub otlp: Option<OtlpOverrides>,
}

/// `[alerts]` from the file or the environment.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlertsOverrides {
    /// On or off.
    pub enabled: Option<bool>,
    /// Evaluation interval.
    pub evaluation_interval_secs: Option<u64>,
    /// Disabled built-in rules.
    pub disabled_rules: Option<Vec<String>>,
    /// Allow `http://` notifier URLs.
    pub allow_http: Option<bool>,
    /// History retention.
    pub history_retention_days: Option<u64>,
    /// Certificate expiry gauge.
    pub tls_cert_expiry_metric: Option<String>,
    /// `[[alerts.rules]]` (file only; replaces the list).
    pub rules: Option<Vec<AlertRuleDecl>>,
}

// `ConfigOverrides` is `Eq`; the only floats here are thresholds and ratios
// read from the file, never NaN after validation.
impl Eq for ObservabilityOverrides {}
impl Eq for AlertsOverrides {}

impl ObservabilityOverrides {
    fn otlp(&mut self) -> &mut OtlpOverrides {
        self.otlp.get_or_insert_with(OtlpOverrides::default)
    }

    /// Sets the field of `FORGEBASE_<key>`; `Ok(false)`: not one of ours.
    pub(crate) fn env(&mut self, key: &str, value: &str) -> Result<bool, String> {
        match key {
            "TRUST_INCOMING_TRACE" => self.trust_incoming_trace = Some(value.parse()?),
            "OTLP_ENABLED" => self.otlp().enabled = Some(super::parse_bool(value)?),
            "OTLP_ENDPOINT" => self.otlp().endpoint = Some(value.to_owned()),
            "OTLP_PROTOCOL" => self.otlp().protocol = Some(value.parse()?),
            "OTLP_SERVICE_NAME" => self.otlp().service_name = Some(value.to_owned()),
            "OTLP_SERVICE_INSTANCE_ID" => self.otlp().service_instance_id = Some(value.to_owned()),
            "OTLP_SAMPLING_RATIO" => self.otlp().sampling_ratio = Some(super::parse_num(value)?),
            "OTLP_EXPORT_TRACES" => self.otlp().export_traces = Some(super::parse_bool(value)?),
            "OTLP_EXPORT_LOGS" => self.otlp().export_logs = Some(super::parse_bool(value)?),
            "OTLP_TIMEOUT_SECS" => self.otlp().timeout_secs = Some(super::parse_num(value)?),
            "OTLP_CA_FILE" => self.otlp().ca_file = Some(value.to_owned()),
            _ => return Ok(false),
        }
        Ok(true)
    }

    pub(crate) fn keys(&self, put: &mut dyn FnMut(bool, &'static str)) {
        put(self.trust_incoming_trace.is_some(), "observability.trust_incoming_trace");
        if let Some(o) = &self.otlp {
            put(o.enabled.is_some(), "observability.otlp.enabled");
            put(o.endpoint.is_some(), "observability.otlp.endpoint");
            put(o.protocol.is_some(), "observability.otlp.protocol");
            put(o.headers.is_some(), "observability.otlp.headers");
            put(o.service_name.is_some(), "observability.otlp.service_name");
            put(o.service_instance_id.is_some(), "observability.otlp.service_instance_id");
            put(o.resource_attributes.is_some(), "observability.otlp.resource_attributes");
            put(o.sampling_ratio.is_some(), "observability.otlp.sampling_ratio");
            put(o.export_traces.is_some(), "observability.otlp.export_traces");
            put(o.export_logs.is_some(), "observability.otlp.export_logs");
            put(o.timeout_secs.is_some(), "observability.otlp.timeout_secs");
            put(o.ca_file.is_some(), "observability.otlp.ca_file");
        }
    }
}

impl AlertsOverrides {
    /// Sets the field of `FORGEBASE_<key>`; `Ok(false)`: not one of ours.
    pub(crate) fn env(&mut self, key: &str, value: &str) -> Result<bool, String> {
        match key {
            "ALERTS_ENABLED" => self.enabled = Some(super::parse_bool(value)?),
            "ALERTS_EVALUATION_INTERVAL_SECS" => self.evaluation_interval_secs = Some(super::parse_num(value)?),
            "ALERTS_ALLOW_HTTP" => self.allow_http = Some(super::parse_bool(value)?),
            "ALERTS_HISTORY_RETENTION_DAYS" => self.history_retention_days = Some(super::parse_num(value)?),
            "ALERTS_DISABLED_RULES" => {
                self.disabled_rules = Some(
                    value
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .collect(),
                );
            }
            "ALERTS_TLS_CERT_EXPIRY_METRIC" => self.tls_cert_expiry_metric = Some(value.to_owned()),
            _ => return Ok(false),
        }
        Ok(true)
    }

    pub(crate) fn keys(&self, put: &mut dyn FnMut(bool, &'static str)) {
        put(self.enabled.is_some(), "alerts.enabled");
        put(self.evaluation_interval_secs.is_some(), "alerts.evaluation_interval_secs");
        put(self.disabled_rules.is_some(), "alerts.disabled_rules");
        put(self.allow_http.is_some(), "alerts.allow_http");
        put(self.history_retention_days.is_some(), "alerts.history_retention_days");
        put(self.tls_cert_expiry_metric.is_some(), "alerts.tls_cert_expiry_metric");
        put(self.rules.is_some(), "alerts.rules");
    }
}

impl ObservabilityConfig {
    /// Applies one layer.
    pub(crate) fn apply(&mut self, o: &ObservabilityOverrides) {
        if let Some(v) = o.trust_incoming_trace {
            self.trust_incoming_trace = v;
        }
        let Some(x) = &o.otlp else { return };
        let c = &mut self.otlp;
        if let Some(v) = x.enabled {
            c.enabled = v;
        }
        if let Some(v) = &x.endpoint {
            c.endpoint = v.trim().to_owned();
        }
        if let Some(v) = x.protocol {
            c.protocol = v;
        }
        if let Some(v) = &x.headers {
            c.headers = v.clone();
        }
        if let Some(v) = &x.service_name {
            c.service_name = v.clone();
        }
        if let Some(v) = &x.service_instance_id {
            c.service_instance_id = v.clone();
        }
        if let Some(v) = &x.resource_attributes {
            c.resource_attributes = v.clone();
        }
        if let Some(v) = x.sampling_ratio {
            c.sampling_ratio = v;
        }
        if let Some(v) = x.export_traces {
            c.export_traces = v;
        }
        if let Some(v) = x.export_logs {
            c.export_logs = v;
        }
        if let Some(v) = x.timeout_secs {
            c.timeout_secs = v;
        }
        if let Some(v) = &x.ca_file {
            c.ca_file = v.clone();
        }
    }

    /// Validation problems as `(field, message)`.
    pub(crate) fn problems(&self) -> Vec<(&'static str, String)> {
        let mut out = Vec::new();
        let o = &self.otlp;
        let url_ok = |u: &str| {
            (u.starts_with("http://") || u.starts_with("https://"))
                && u.split_once("://").is_some_and(|(_, rest)| !rest.is_empty())
                && !u.contains(char::is_whitespace)
        };
        if !url_ok(&o.endpoint) {
            out.push((
                "observability.otlp.endpoint",
                "must be the collector's address starting with http:// or https://, such as http://127.0.0.1:4318".into(),
            ));
        }
        if !(0.0..=1.0).contains(&o.sampling_ratio) || o.sampling_ratio.is_nan() {
            out.push((
                "observability.otlp.sampling_ratio",
                "must be a number from 0 to 1, such as 1.0 (every trace) or 0.1 (one in ten)".into(),
            ));
        }
        if o.service_name.trim().is_empty() {
            out.push((
                "observability.otlp.service_name",
                "must not be empty, such as forgebase".into(),
            ));
        }
        if !(1..=120).contains(&o.timeout_secs) {
            out.push((
                "observability.otlp.timeout_secs",
                "must be a whole number from 1 to 120 seconds, such as 10".into(),
            ));
        }
        for (k, v) in o.headers.iter().chain(o.resource_attributes.iter()) {
            if k.trim().is_empty() || k.contains(['\r', '\n', ':']) || v.contains(['\r', '\n', '\0']) {
                out.push((
                    "observability.otlp.headers",
                    format!("must hold names and one-line values, such as {{\"x-api-key\" = \"env:MY_KEY\"}} ({k:?} is not)"),
                ));
                break;
            }
        }
        out
    }
}

impl AlertsConfig {
    /// Applies one layer.
    pub(crate) fn apply(&mut self, o: &AlertsOverrides) {
        if let Some(v) = o.enabled {
            self.enabled = v;
        }
        if let Some(v) = o.evaluation_interval_secs {
            self.evaluation_interval_secs = v;
        }
        if let Some(v) = &o.disabled_rules {
            self.disabled_rules = v.clone();
        }
        if let Some(v) = o.allow_http {
            self.allow_http = v;
        }
        if let Some(v) = o.history_retention_days {
            self.history_retention_days = v;
        }
        if let Some(v) = &o.tls_cert_expiry_metric {
            self.tls_cert_expiry_metric = v.trim().to_owned();
        }
        if let Some(v) = &o.rules {
            self.rules = v.clone();
        }
    }

    /// Validation problems as `(field, message)`.
    pub(crate) fn problems(&self) -> Vec<(&'static str, String)> {
        let mut out = Vec::new();
        if !(1..=3600).contains(&self.evaluation_interval_secs) {
            out.push((
                "alerts.evaluation_interval_secs",
                "must be a whole number from 1 to 3600 seconds, such as 30".into(),
            ));
        }
        if !(1..=3650).contains(&self.history_retention_days) {
            out.push((
                "alerts.history_retention_days",
                "must be a whole number from 1 to 3650 days, such as 30".into(),
            ));
        }
        let metric_ok = |m: &str| {
            !m.is_empty()
                && m.bytes().enumerate().all(|(i, b)| {
                    b.is_ascii_alphabetic() || b == b'_' || b == b':' || (i > 0 && b.is_ascii_digit())
                })
        };
        if !metric_ok(&self.tls_cert_expiry_metric) {
            out.push((
                "alerts.tls_cert_expiry_metric",
                "must be a Prometheus metric name, such as forgebase_tls_certificate_expiry_timestamp_seconds".into(),
            ));
        }
        out
    }
}
