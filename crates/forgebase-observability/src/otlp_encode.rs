//! OTLP request bodies, by hand (ADR-0035 §1): protobuf for
//! `http/protobuf`, the OTLP JSON mapping for `http/json`.
//!
//! The messages written (from `opentelemetry-proto` 1.x):
//!
//! ```text
//! ExportTraceServiceRequest { repeated ResourceSpans resource_spans = 1; }
//! ResourceSpans  { Resource resource = 1; repeated ScopeSpans scope_spans = 2; }
//! ScopeSpans     { InstrumentationScope scope = 1; repeated Span spans = 2; }
//! Span           { bytes trace_id = 1; bytes span_id = 2; string trace_state = 3;
//!                  bytes parent_span_id = 4; string name = 5; SpanKind kind = 6;
//!                  fixed64 start_time_unix_nano = 7; fixed64 end_time_unix_nano = 8;
//!                  repeated KeyValue attributes = 9; Status status = 15; fixed32 flags = 16; }
//! Status         { string message = 2; StatusCode code = 3; }
//! ExportLogsServiceRequest { repeated ResourceLogs resource_logs = 1; }
//! ResourceLogs   { Resource resource = 1; repeated ScopeLogs scope_logs = 2; }
//! ScopeLogs      { InstrumentationScope scope = 1; repeated LogRecord log_records = 2; }
//! LogRecord      { fixed64 time_unix_nano = 1; SeverityNumber severity_number = 2;
//!                  string severity_text = 3; AnyValue body = 5; repeated KeyValue attributes = 6;
//!                  fixed32 flags = 8; bytes trace_id = 9; bytes span_id = 10;
//!                  fixed64 observed_time_unix_nano = 11; }
//! Resource       { repeated KeyValue attributes = 1; }
//! InstrumentationScope { string name = 1; string version = 2; }
//! KeyValue       { string key = 1; AnyValue value = 2; }
//! AnyValue       { oneof { string string_value = 1; bool bool_value = 2;
//!                  int64 int_value = 3; double double_value = 4; } }
//! ```

use forgebase_core::config::telemetry_config::OtlpProtocol;
use serde_json::{Value as Json, json};

use crate::otlp::{AttrValue, LogData, OtlpOptions, SpanData};
use crate::trace::hex;

const SCOPE: &str = "forgebase";

/// The body of a traces export request.
pub(crate) fn traces(opts: &OtlpOptions, spans: &[SpanData]) -> Vec<u8> {
    match opts.protocol {
        OtlpProtocol::HttpProtobuf => {
            let mut req = Vec::new();
            message(&mut req, 1, |rs| {
                message(rs, 1, |r| resource(r, &opts.resource));
                message(rs, 2, |ss| {
                    message(ss, 1, scope);
                    for s in spans {
                        message(ss, 2, |b| span(b, s));
                    }
                });
            });
            req
        }
        OtlpProtocol::HttpJson => {
            let body = json!({"resourceSpans": [{
                "resource": {"attributes": json_attrs(&opts.resource)},
                "scopeSpans": [{
                    "scope": {"name": SCOPE, "version": forgebase_core::build::VERSION},
                    "spans": spans.iter().map(json_span).collect::<Vec<_>>(),
                }],
            }]});
            serde_json::to_vec(&body).unwrap_or_default()
        }
    }
}

/// The body of a logs export request.
pub(crate) fn logs(opts: &OtlpOptions, logs: &[LogData]) -> Vec<u8> {
    match opts.protocol {
        OtlpProtocol::HttpProtobuf => {
            let mut req = Vec::new();
            message(&mut req, 1, |rl| {
                message(rl, 1, |r| resource(r, &opts.resource));
                message(rl, 2, |sl| {
                    message(sl, 1, scope);
                    for l in logs {
                        message(sl, 2, |b| log(b, l));
                    }
                });
            });
            req
        }
        OtlpProtocol::HttpJson => {
            let body = json!({"resourceLogs": [{
                "resource": {"attributes": json_attrs(&opts.resource)},
                "scopeLogs": [{
                    "scope": {"name": SCOPE, "version": forgebase_core::build::VERSION},
                    "logRecords": logs.iter().map(json_log).collect::<Vec<_>>(),
                }],
            }]});
            serde_json::to_vec(&body).unwrap_or_default()
        }
    }
}

/// `(number, text)` of a `tracing` level (OTLP SeverityNumber).
fn severity(level: tracing::Level) -> (u64, &'static str) {
    match level {
        tracing::Level::TRACE => (1, "TRACE"),
        tracing::Level::DEBUG => (5, "DEBUG"),
        tracing::Level::INFO => (9, "INFO"),
        tracing::Level::WARN => (13, "WARN"),
        tracing::Level::ERROR => (17, "ERROR"),
    }
}

// -------------------------------------------------------------- protobuf

fn varint(buf: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        buf.push((v as u8) | 0x80);
        v >>= 7;
    }
    buf.push(v as u8);
}

fn key(buf: &mut Vec<u8>, field: u32, wire: u8) {
    varint(buf, (u64::from(field) << 3) | u64::from(wire));
}

fn bytes_field(buf: &mut Vec<u8>, field: u32, b: &[u8]) {
    key(buf, field, 2);
    varint(buf, b.len() as u64);
    buf.extend_from_slice(b);
}

fn string_field(buf: &mut Vec<u8>, field: u32, s: &str) {
    if !s.is_empty() {
        bytes_field(buf, field, s.as_bytes());
    }
}

fn varint_field(buf: &mut Vec<u8>, field: u32, v: u64) {
    if v != 0 {
        key(buf, field, 0);
        varint(buf, v);
    }
}

fn fixed64_field(buf: &mut Vec<u8>, field: u32, v: u64) {
    key(buf, field, 1);
    buf.extend_from_slice(&v.to_le_bytes());
}

fn fixed32_field(buf: &mut Vec<u8>, field: u32, v: u32) {
    key(buf, field, 5);
    buf.extend_from_slice(&v.to_le_bytes());
}

/// A length-delimited sub-message.
fn message(buf: &mut Vec<u8>, field: u32, f: impl FnOnce(&mut Vec<u8>)) {
    let mut inner = Vec::new();
    f(&mut inner);
    bytes_field(buf, field, &inner);
}

fn any_value(buf: &mut Vec<u8>, v: &AttrValue) {
    match v {
        AttrValue::Str(s) => bytes_field(buf, 1, s.as_bytes()),
        AttrValue::Bool(b) => {
            key(buf, 2, 0);
            varint(buf, u64::from(*b));
        }
        AttrValue::Int(i) => {
            key(buf, 3, 0);
            varint(buf, *i as u64);
        }
        AttrValue::F64(f) => {
            key(buf, 4, 1);
            buf.extend_from_slice(&f.to_le_bytes());
        }
    }
}

fn key_value(buf: &mut Vec<u8>, field: u32, k: &str, v: &AttrValue) {
    message(buf, field, |kv| {
        string_field(kv, 1, k);
        message(kv, 2, |av| any_value(av, v));
    });
}

fn resource(buf: &mut Vec<u8>, attrs: &[(String, AttrValue)]) {
    for (k, v) in attrs {
        key_value(buf, 1, k, v);
    }
}

fn scope(buf: &mut Vec<u8>) {
    string_field(buf, 1, SCOPE);
    string_field(buf, 2, forgebase_core::build::VERSION);
}

fn span(buf: &mut Vec<u8>, s: &SpanData) {
    bytes_field(buf, 1, &s.trace_id);
    bytes_field(buf, 2, &s.span_id);
    if let Some(ts) = &s.trace_state {
        string_field(buf, 3, ts);
    }
    if let Some(p) = &s.parent_span_id {
        bytes_field(buf, 4, p);
    }
    string_field(buf, 5, &s.name);
    varint_field(buf, 6, s.kind.code());
    fixed64_field(buf, 7, s.start_ns);
    fixed64_field(buf, 8, s.end_ns.max(s.start_ns));
    for (k, v) in &s.attributes {
        key_value(buf, 9, k, v);
    }
    message(buf, 15, |st| {
        string_field(st, 2, &s.status_message);
        varint_field(st, 3, s.status.code());
    });
    // W3C flags (sampled) plus "has remote parent known" bits are optional;
    // exported spans are always sampled.
    fixed32_field(buf, 16, 0x01);
}

fn log(buf: &mut Vec<u8>, l: &LogData) {
    fixed64_field(buf, 1, l.time_ns);
    let (num, text) = severity(l.severity);
    varint_field(buf, 2, num);
    string_field(buf, 3, text);
    message(buf, 5, |b| any_value(b, &AttrValue::Str(l.body.clone())));
    key_value(buf, 6, "code.namespace", &AttrValue::Str(l.target.clone()));
    for (k, v) in &l.attributes {
        key_value(buf, 6, k, v);
    }
    if let Some((t, s, sampled)) = &l.trace {
        fixed32_field(buf, 8, u32::from(*sampled));
        bytes_field(buf, 9, t);
        bytes_field(buf, 10, s);
    }
    fixed64_field(buf, 11, l.time_ns);
}

// ------------------------------------------------------------------ JSON

fn json_value(v: &AttrValue) -> Json {
    match v {
        AttrValue::Str(s) => json!({"stringValue": s}),
        AttrValue::Bool(b) => json!({"boolValue": b}),
        // int64 is a string in the OTLP JSON mapping.
        AttrValue::Int(i) => json!({"intValue": i.to_string()}),
        AttrValue::F64(f) if f.is_finite() => json!({"doubleValue": f}),
        AttrValue::F64(f) => json!({"stringValue": f.to_string()}),
    }
}

fn json_attrs(attrs: &[(String, AttrValue)]) -> Json {
    Json::Array(
        attrs
            .iter()
            .map(|(k, v)| json!({"key": k, "value": json_value(v)}))
            .collect(),
    )
}

fn json_span(s: &SpanData) -> Json {
    let mut o = json!({
        "traceId": hex(&s.trace_id),
        "spanId": hex(&s.span_id),
        "name": s.name,
        "kind": s.kind.code(),
        "startTimeUnixNano": s.start_ns.to_string(),
        "endTimeUnixNano": s.end_ns.max(s.start_ns).to_string(),
        "attributes": json_attrs(&s.attributes),
        "status": {"code": s.status.code(), "message": s.status_message},
        "flags": 1,
    });
    if let Some(p) = &s.parent_span_id {
        o["parentSpanId"] = json!(hex(p));
    }
    if let Some(ts) = &s.trace_state {
        o["traceState"] = json!(ts);
    }
    o
}

fn json_log(l: &LogData) -> Json {
    let (num, text) = severity(l.severity);
    let mut attrs = vec![("code.namespace".to_owned(), AttrValue::Str(l.target.clone()))];
    attrs.extend(l.attributes.iter().cloned());
    let mut o = json!({
        "timeUnixNano": l.time_ns.to_string(),
        "observedTimeUnixNano": l.time_ns.to_string(),
        "severityNumber": num,
        "severityText": text,
        "body": {"stringValue": l.body},
        "attributes": json_attrs(&attrs),
    });
    if let Some((t, s, sampled)) = &l.trace {
        o["traceId"] = json!(hex(t));
        o["spanId"] = json!(hex(s));
        o["flags"] = json!(u32::from(*sampled));
    }
    o
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::otlp::{SpanKind, StatusCode};

    /// A minimal protobuf reader for the tests: `(field, wire, bytes)`.
    pub(crate) fn fields(mut b: &[u8]) -> Vec<(u32, u8, Vec<u8>)> {
        fn varint(b: &mut &[u8]) -> u64 {
            let mut v = 0u64;
            let mut shift = 0;
            loop {
                let byte = b[0];
                *b = &b[1..];
                v |= u64::from(byte & 0x7f) << shift;
                if byte < 0x80 {
                    return v;
                }
                shift += 7;
            }
        }
        let mut out = Vec::new();
        while !b.is_empty() {
            let k = varint(&mut b);
            let (field, wire) = ((k >> 3) as u32, (k & 7) as u8);
            let data = match wire {
                0 => varint(&mut b).to_le_bytes().to_vec(),
                1 => {
                    let d = b[..8].to_vec();
                    b = &b[8..];
                    d
                }
                2 => {
                    let n = varint(&mut b) as usize;
                    let d = b[..n].to_vec();
                    b = &b[n..];
                    d
                }
                5 => {
                    let d = b[..4].to_vec();
                    b = &b[4..];
                    d
                }
                w => panic!("wire type {w}"),
            };
            out.push((field, wire, data));
        }
        out
    }

    fn opts(protocol: OtlpProtocol) -> OtlpOptions {
        let mut c = forgebase_core::config::telemetry_config::ObservabilityConfig::default().otlp;
        c.protocol = protocol;
        OtlpOptions::from_config(&c, Some("i"), |_| None).expect("options")
    }

    fn sample() -> SpanData {
        SpanData {
            trace_id: [1; 16],
            span_id: [2; 8],
            parent_span_id: Some([3; 8]),
            trace_state: None,
            name: "GET /api/v1/x".into(),
            kind: SpanKind::Server,
            start_ns: 10,
            end_ns: 20,
            attributes: vec![
                ("http.route".into(), AttrValue::Str("/api/v1/x".into())),
                ("http.response.status_code".into(), AttrValue::Int(200)),
            ],
            status: StatusCode::Error,
            status_message: "boom".into(),
        }
    }

    #[test]
    fn protobuf_span_has_the_otlp_layout() {
        let body = traces(&opts(OtlpProtocol::HttpProtobuf), &[sample()]);
        let req = fields(&body);
        assert_eq!(req.len(), 1);
        assert_eq!(req[0].0, 1, "resource_spans");
        let rs = fields(&req[0].2);
        let resource = fields(&rs.iter().find(|f| f.0 == 1).expect("resource").2);
        let first_kv = fields(&resource[0].2);
        assert_eq!(first_kv[0].2, b"service.name");
        let ss = fields(&rs.iter().find(|f| f.0 == 2).expect("scope spans").2);
        let span = fields(&ss.iter().find(|f| f.0 == 2).expect("span").2);
        let get = |n: u32| span.iter().find(|f| f.0 == n).map(|f| f.2.clone()).expect("field");
        assert_eq!(get(1), vec![1; 16]);
        assert_eq!(get(2), vec![2; 8]);
        assert_eq!(get(4), vec![3; 8]);
        assert_eq!(get(5), b"GET /api/v1/x");
        assert_eq!(u64::from_le_bytes(get(6).try_into().expect("8")), 2, "server");
        assert_eq!(u64::from_le_bytes(get(7).try_into().expect("8")), 10);
        assert_eq!(span.iter().filter(|f| f.0 == 9).count(), 2, "attributes");
        let status = fields(&get(15));
        assert_eq!(status[0].2, b"boom");
        assert_eq!(u64::from_le_bytes(status[1].2.clone().try_into().expect("8")), 2, "error");
    }

    #[test]
    fn json_span_uses_hex_ids_and_string_int64() {
        let body = traces(&opts(OtlpProtocol::HttpJson), &[sample()]);
        let v: Json = serde_json::from_slice(&body).expect("json");
        let s = &v["resourceSpans"][0]["scopeSpans"][0]["spans"][0];
        assert_eq!(s["traceId"], "01010101010101010101010101010101");
        assert_eq!(s["parentSpanId"], "0303030303030303");
        assert_eq!(s["kind"], 2);
        assert_eq!(s["startTimeUnixNano"], "10");
        assert_eq!(s["attributes"][1]["value"]["intValue"], "200");
        assert_eq!(
            v["resourceSpans"][0]["resource"]["attributes"][0]["value"]["stringValue"],
            "forgebase"
        );
    }

    #[test]
    fn logs_carry_trace_ids_and_severity() {
        let l = LogData {
            time_ns: 5,
            severity: tracing::Level::WARN,
            body: "hello".into(),
            target: "forgebase_queue::push".into(),
            attributes: vec![("subscription".into(), AttrValue::Str("s".into()))],
            trace: Some(([7; 16], [8; 8], true)),
        };
        let v: Json = serde_json::from_slice(&logs(&opts(OtlpProtocol::HttpJson), std::slice::from_ref(&l)))
            .expect("json");
        let r = &v["resourceLogs"][0]["scopeLogs"][0]["logRecords"][0];
        assert_eq!(r["severityNumber"], 13);
        assert_eq!(r["body"]["stringValue"], "hello");
        assert_eq!(r["traceId"], hex(&[7; 16]));
        let pb = logs(&opts(OtlpProtocol::HttpProtobuf), &[l]);
        let rl = fields(&fields(&pb)[0].2);
        let sl = fields(&rl.iter().find(|f| f.0 == 2).expect("scope logs").2);
        let rec = fields(&sl.iter().find(|f| f.0 == 2).expect("record").2);
        assert_eq!(rec.iter().find(|f| f.0 == 9).expect("trace id").2, vec![7; 16]);
    }
}
