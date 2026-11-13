//! Rules: the built-in pack, rules from the file, the burn-rate rules of
//! SLOs, validation and annotation templates (ADR-0035 §3, §5).

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use crate::error::{AlertError, Result};
use crate::expr::{self, CmpOp, Expr};
use crate::model::{Rule, RuleSource, Severity, Slo, SloKind};
use crate::samples::Labels;

/// A rule ready to evaluate.
#[derive(Debug, Clone)]
pub struct RuleDef {
    /// Id.
    pub id: String,
    /// `alertname`.
    pub name: String,
    /// Origin.
    pub source: RuleSource,
    /// Source text of the expression.
    pub expr_src: String,
    /// Parsed expression.
    pub expr: Expr,
    /// Comparison.
    pub op: CmpOp,
    /// Threshold.
    pub threshold: f64,
    /// Hold time.
    pub for_ms: i64,
    /// Severity.
    pub severity: Severity,
    /// Labels.
    pub labels: BTreeMap<String, String>,
    /// Annotations (templates).
    pub annotations: BTreeMap<String, String>,
    /// Evaluated.
    pub enabled: bool,
    /// Switched off by `[alerts] disabled_rules`.
    pub disabled_by_config: bool,
    /// API rules: created / updated (ms).
    pub times: Option<(i64, i64)>,
}

impl RuleDef {
    /// The API view.
    pub fn view(&self) -> Rule {
        Rule {
            id: self.id.clone(),
            name: self.name.clone(),
            source: self.source,
            expr: self.expr_src.clone(),
            op: self.op,
            threshold: self.threshold,
            for_: expr::format_duration(self.for_ms),
            for_ms: self.for_ms,
            severity: self.severity,
            labels: self.labels.clone(),
            annotations: self.annotations.clone(),
            enabled: self.enabled && !self.disabled_by_config,
            disabled_by_config: self.disabled_by_config,
            created_at: self.times.map(|t| crate::model::iso(t.0)),
            updated_at: self.times.map(|t| crate::model::iso(t.1)),
        }
    }
}

/// Checks a rule or SLO name.
pub fn check_name(what: &str, name: &str, max: usize) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= max
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'));
    if ok {
        Ok(())
    } else {
        Err(AlertError::invalid(
            "invalid_name",
            format!("{what} must be 1 to {max} letters, digits, _, - or . (such as HighErrorRate)"),
        ))
    }
}

fn check_labels(labels: &BTreeMap<String, String>, what: &str) -> Result<()> {
    if labels.len() > 32 {
        return Err(AlertError::invalid("invalid_alert_rule", format!("at most 32 {what}")));
    }
    for (k, v) in labels {
        let ok = !k.is_empty()
            && k.len() <= 64
            && k.bytes().enumerate().all(|(i, b)| b.is_ascii_alphabetic() || b == b'_' || (i > 0 && b.is_ascii_digit()));
        if !ok || v.len() > 2048 {
            return Err(AlertError::invalid(
                "invalid_alert_rule",
                format!("{what} names are letters, digits and _ (such as team), values at most 2048 bytes ({k:?})"),
            ));
        }
    }
    Ok(())
}

/// Builds and validates a rule.
#[allow(clippy::too_many_arguments)]
pub fn build(
    id: String,
    name: &str,
    source: RuleSource,
    expr_src: &str,
    op: CmpOp,
    threshold: f64,
    for_ms: i64,
    severity: Severity,
    labels: BTreeMap<String, String>,
    annotations: BTreeMap<String, String>,
) -> Result<RuleDef> {
    check_name("name", name, 128)?;
    let expr = expr::parse(expr_src).map_err(|e| AlertError::invalid("invalid_alert_rule", format!("expr: {e}")))?;
    if !threshold.is_finite() {
        return Err(AlertError::invalid("invalid_alert_rule", "threshold must be a finite number"));
    }
    if !(0..=7 * 86_400_000).contains(&for_ms) {
        return Err(AlertError::invalid("invalid_alert_rule", "for must be from 0s to 7d, such as 5m"));
    }
    check_labels(&labels, "labels")?;
    check_labels(&annotations, "annotations")?;
    for reserved in ["alertname", "severity"] {
        if labels.contains_key(reserved) {
            return Err(AlertError::invalid(
                "invalid_alert_rule",
                format!("the label {reserved} is set by the rule itself"),
            ));
        }
    }
    Ok(RuleDef {
        id,
        name: name.to_owned(),
        source,
        expr_src: expr_src.to_owned(),
        expr,
        op,
        threshold,
        for_ms,
        severity,
        labels,
        annotations,
        enabled: true,
        disabled_by_config: false,
        times: None,
    })
}

/// Parses a `for` value.
pub fn parse_for(s: Option<&str>) -> Result<i64> {
    match s.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(0),
        Some(s) => expr::parse_duration(s)
            .ok_or_else(|| AlertError::invalid("invalid_alert_rule", format!("for: {s:?} is not a duration such as 5m"))),
    }
}

/// The rules of `[[alerts.rules]]`.
pub fn from_config(decls: &[forgebase_core::config::telemetry_config::AlertRuleDecl]) -> Result<Vec<RuleDef>> {
    let mut out: Vec<RuleDef> = Vec::new();
    for d in decls {
        let op = match d.op.as_deref() {
            None => CmpOp::Gt,
            Some(o) => CmpOp::parse(o).ok_or_else(|| {
                AlertError::invalid(
                    "invalid_alert_rule",
                    format!("alerts.rules {}: op must be >, >=, <, <=, == or !=", d.name),
                )
            })?,
        };
        let severity = match d.severity.as_deref() {
            None => Severity::Warning,
            Some(s) => Severity::parse(s).ok_or_else(|| {
                AlertError::invalid(
                    "invalid_alert_rule",
                    format!("alerts.rules {}: severity must be info, warning or critical", d.name),
                )
            })?,
        };
        let rule = build(
            format!("config:{}", d.name),
            &d.name,
            RuleSource::Config,
            &d.expr,
            op,
            d.threshold,
            parse_for(d.for_.as_deref())?,
            severity,
            d.labels.clone(),
            d.annotations.clone(),
        )
        .map_err(|e| AlertError::invalid("invalid_alert_rule", format!("alerts.rules {}: {e}", d.name)))?;
        if out.iter().any(|r| r.id == rule.id) {
            return Err(AlertError::invalid(
                "invalid_alert_rule",
                format!("alerts.rules: the name {} is used twice", d.name),
            ));
        }
        out.push(rule);
    }
    Ok(out)
}

struct Builtin {
    id: &'static str,
    name: &'static str,
    expr: String,
    op: CmpOp,
    threshold: f64,
    for_: &'static str,
    severity: Severity,
    summary: &'static str,
    description: &'static str,
}

/// The built-in rule pack (ADR-0035 §3). `tls_metric` is
/// `[alerts] tls_cert_expiry_metric`.
pub fn builtins(tls_metric: &str) -> Vec<RuleDef> {
    let req = "forgebase_http_requests_total";
    let pack = vec![
        Builtin {
            id: "http_5xx_ratio_high",
            name: "HttpServerErrorsHigh",
            expr: format!("sum(rate({req}{{status=~\"5..\"}}[5m])) / sum(rate({req}[5m]))"),
            op: CmpOp::Gt,
            threshold: 0.05,
            for_: "5m",
            severity: Severity::Warning,
            summary: "{{ $value | percent }} of HTTP requests failed with a server error (5xx) over 5 minutes",
            description: "More than 5% of requests answered 5xx for 5 minutes. Check the server log for the request ids of the failing requests.",
        },
        Builtin {
            id: "http_latency_p95_high",
            name: "HttpLatencyHigh",
            expr: "histogram_quantile(0.95, sum by (le) (rate(forgebase_http_request_duration_seconds_bucket[5m])))"
                .into(),
            op: CmpOp::Gt,
            threshold: 1.0,
            for_: "10m",
            severity: Severity::Warning,
            summary: "95% of HTTP requests take up to {{ $value }} s (over 1 s for 10 minutes)",
            description: "The 95th percentile of request latency is above one second.",
        },
        Builtin {
            id: "queue_backlog_high",
            name: "QueueBacklogHigh",
            expr: "forgebase:queue_backlog".into(),
            op: CmpOp::Gt,
            threshold: 10_000.0,
            for_: "15m",
            severity: Severity::Warning,
            summary: "Subscription {{ $labels.subscription }} has {{ $value }} unacknowledged messages",
            description: "Its consumers do not keep up. Add consumers, or check that they acknowledge.",
        },
        Builtin {
            id: "queue_oldest_unacked_high",
            name: "QueueMessagesStuck",
            expr: "forgebase:queue_oldest_unacked_seconds".into(),
            op: CmpOp::Gt,
            threshold: 3600.0,
            for_: "10m",
            severity: Severity::Warning,
            summary: "The oldest unacknowledged message of {{ $labels.subscription }} is {{ $value | duration }} old",
            description: "A message has waited for more than an hour.",
        },
        Builtin {
            id: "queue_dead_letters_growing",
            name: "QueueDeadLettersGrowing",
            expr: "increase(forgebase:queue_dead_lettered_total[15m])".into(),
            op: CmpOp::Gt,
            threshold: 0.0,
            for_: "0s",
            severity: Severity::Warning,
            summary: "{{ $value }} messages of {{ $labels.subscription }} were dead-lettered in 15 minutes",
            description: "Messages ran out of delivery attempts. Inspect the dead letters and redrive them once fixed.",
        },
        Builtin {
            id: "queue_push_paused",
            name: "QueuePushPaused",
            expr: "forgebase:queue_push_paused".into(),
            op: CmpOp::Gt,
            threshold: 0.0,
            for_: "0s",
            severity: Severity::Critical,
            summary: "Push delivery of {{ $labels.subscription }} is paused",
            description: "Nothing is delivered until it is resumed; messages accumulate. The circuit breaker pauses after repeated failures: check the push attempt log.",
        },
        Builtin {
            id: "billing_webhook_signature_invalid",
            name: "BillingWebhookSignaturesInvalid",
            expr: "sum by (provider) (increase(forgebase_billing_webhook_deliveries_total{outcome=\"signature_invalid\"}[15m]))"
                .into(),
            op: CmpOp::Gt,
            threshold: 5.0,
            for_: "0s",
            severity: Severity::Warning,
            summary: "{{ $value }} {{ $labels.provider }} webhooks failed signature checks in 15 minutes",
            description: "Usually a wrong or rotated webhook secret; otherwise forged requests. See the billing webhook inspector.",
        },
        Builtin {
            id: "job_failure_ratio_high",
            name: "JobFailuresHigh",
            expr: "sum by (queue) (rate(forgebase_jobs_completed_total{outcome=~\"failed|dead\"}[15m])) / sum by (queue) (rate(forgebase_jobs_completed_total[15m]))"
                .into(),
            op: CmpOp::Gt,
            threshold: 0.25,
            for_: "15m",
            severity: Severity::Warning,
            summary: "{{ $value | percent }} of job attempts in queue {{ $labels.queue }} failed over 15 minutes",
            description: "More than a quarter of job attempts fail.",
        },
        Builtin {
            id: "jobs_dead_lettered",
            name: "JobsDeadLettered",
            expr: "sum by (queue) (increase(forgebase_jobs_completed_total{outcome=\"dead\"}[15m]))".into(),
            op: CmpOp::Gt,
            threshold: 0.0,
            for_: "0s",
            severity: Severity::Warning,
            summary: "{{ $value }} jobs of queue {{ $labels.queue }} went to the dead-letter queue in 15 minutes",
            description: "They ran out of attempts. Retry them from the jobs screen once fixed.",
        },
        Builtin {
            id: "disk_free_low",
            name: "DiskSpaceLow",
            expr: "forgebase:data_dir_free_ratio".into(),
            op: CmpOp::Lt,
            threshold: 0.10,
            for_: "5m",
            severity: Severity::Warning,
            summary: "Only {{ $value | percent }} of the data directory's disk is free",
            description: "Free space or grow the volume before writes fail.",
        },
        Builtin {
            id: "disk_free_critical",
            name: "DiskSpaceCritical",
            expr: "forgebase:data_dir_free_ratio".into(),
            op: CmpOp::Lt,
            threshold: 0.05,
            for_: "5m",
            severity: Severity::Critical,
            summary: "Only {{ $value | percent }} of the data directory's disk is free",
            description: "Writes will fail soon.",
        },
        Builtin {
            id: "backup_stale",
            name: "BackupStale",
            expr: "forgebase:backup_last_success_age_seconds".into(),
            op: CmpOp::Gt,
            threshold: 26.0 * 3600.0,
            for_: "0s",
            severity: Severity::Warning,
            summary: "The last backup is {{ $value | duration }} old",
            description: "No backup in more than 26 hours. Check the backup schedule.",
        },
        Builtin {
            id: "tls_cert_expiring",
            name: "TlsCertificateExpiring",
            expr: format!("{tls_metric} - time()"),
            op: CmpOp::Lt,
            threshold: 14.0 * 86_400.0,
            for_: "0s",
            severity: Severity::Warning,
            summary: "A TLS certificate expires in {{ $value | duration }}",
            description: "Renew it (with ACME it should renew by itself: check its log).",
        },
        Builtin {
            id: "tls_cert_expiry_critical",
            name: "TlsCertificateExpiringSoon",
            expr: format!("{tls_metric} - time()"),
            op: CmpOp::Lt,
            threshold: 3.0 * 86_400.0,
            for_: "0s",
            severity: Severity::Critical,
            summary: "A TLS certificate expires in {{ $value | duration }}",
            description: "Clients will refuse the connection once it expires.",
        },
    ];
    pack.into_iter()
        .filter_map(|b| {
            let annotations = [
                ("summary".to_owned(), b.summary.to_owned()),
                ("description".to_owned(), b.description.to_owned()),
                (
                    "runbook_url".to_owned(),
                    format!("https://github.com/farzanshibu/forgebase/blob/main/docs/observability.md#{}", b.id),
                ),
            ]
            .into_iter()
            .collect();
            build(
                b.id.to_owned(),
                b.name,
                RuleSource::Builtin,
                &b.expr,
                b.op,
                b.threshold,
                expr::parse_duration(b.for_).unwrap_or(0),
                b.severity,
                BTreeMap::new(),
                annotations,
            )
            .map_err(|e| tracing::warn!(rule = b.id, error = %e, "built-in rule not loaded"))
            .ok()
        })
        .collect()
}

/// Burn-rate windows reported for every SLO.
pub const BURN_WINDOWS: [(&str, i64); 5] = [
    ("5m", 300_000),
    ("30m", 1_800_000),
    ("1h", 3_600_000),
    ("6h", 21_600_000),
    ("3d", 259_200_000),
];

/// `(tier, long, short, share of the budget, severity)` (Google SRE workbook).
pub const TIERS: [(&str, &str, &str, f64, Severity); 3] = [
    ("page_fast", "1h", "5m", 0.02, Severity::Critical),
    ("page_slow", "6h", "30m", 0.05, Severity::Critical),
    ("ticket", "3d", "6h", 0.10, Severity::Warning),
];

/// The burn rate both windows of a tier must exceed: the rate that spends
/// `share` of the budget in the long window.
pub fn tier_threshold(slo_window_ms: i64, long_ms: i64, share: f64) -> f64 {
    share * slo_window_ms as f64 / long_ms as f64
}

/// The burn-rate rules of an SLO (tiers whose long window fits in the SLO window).
pub fn slo_rules(slo: &Slo) -> Vec<RuleDef> {
    if !slo.alerting {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (tier, long, short, share, severity) in TIERS {
        let long_ms = expr::parse_duration(long).unwrap_or(0);
        if long_ms > slo.window_ms {
            continue;
        }
        let threshold = tier_threshold(slo.window_ms, long_ms, share);
        let mut labels = slo.labels.clone();
        labels.insert("slo".into(), slo.name.clone());
        labels.insert("tier".into(), tier.into());
        let what = match slo.kind {
            SloKind::Availability => "availability",
            SloKind::Latency => "latency",
        };
        let annotations = [
            (
                "summary".to_owned(),
                format!(
                    "SLO {} ({what} {}) burns its error budget {{{{ $value }}}}x too fast ({long} and {short} windows)",
                    slo.name,
                    slo.objective
                ),
            ),
            (
                "description".to_owned(),
                format!(
                    "At this rate {:.0}% of the {} error budget is gone within {long}.",
                    share * 100.0,
                    expr::format_duration(slo.window_ms)
                ),
            ),
            (
                "runbook_url".to_owned(),
                "https://github.com/farzanshibu/forgebase/blob/main/docs/observability.md#3-slos".to_owned(),
            ),
        ]
        .into_iter()
        .collect();
        let src = format!(
            "min by (slo) (forgebase:slo_burn_rate{{slo=\"{}\", window=~\"{long}|{short}\"}})",
            slo.name
        );
        if let Ok(r) = build(
            format!("slo:{}:{tier}", slo.name),
            "SloBurnRateHigh",
            RuleSource::Slo,
            &src,
            CmpOp::Gt,
            threshold,
            0,
            severity,
            labels,
            annotations,
        ) {
            out.push(r);
        }
    }
    out
}

/// The labels of an alert: the sample's (without `__name__`), the rule's,
/// `alertname` and `severity`.
pub fn alert_labels(rule: &RuleDef, sample: &Labels) -> BTreeMap<String, String> {
    let mut l: BTreeMap<String, String> = sample
        .iter()
        .filter(|(k, _)| k.as_str() != "__name__")
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    for (k, v) in &rule.labels {
        l.insert(k.clone(), v.clone());
    }
    l.insert("alertname".into(), rule.name.clone());
    l.insert("severity".into(), rule.severity.as_str().into());
    l
}

/// The fingerprint of an alert: rule id and labels, hashed.
pub fn fingerprint(rule_id: &str, labels: &BTreeMap<String, String>) -> String {
    let mut h = Sha256::new();
    h.update(rule_id.as_bytes());
    for (k, v) in labels {
        h.update([0u8]);
        h.update(k.as_bytes());
        h.update([0xffu8]);
        h.update(v.as_bytes());
    }
    let d = h.finalize();
    forgebase_observability::trace::hex(&d[..12])
}

/// A value for messages: integers plain, others with up to 4 decimals.
pub fn format_value(v: f64) -> String {
    if v.is_nan() {
        return "NaN".into();
    }
    if v.fract() == 0.0 && v.abs() < 1e15 {
        return format!("{}", v as i64);
    }
    let s = format!("{v:.4}");
    s.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// Fills `{{ $labels.x }}`, `{{ $value }}`, `{{ $value | percent }}` and
/// `{{ $value | duration }}` (seconds as `2h5m`). Unknown placeholders are
/// left as written.
pub fn render(template: &str, labels: &BTreeMap<String, String>, value: f64) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let Some(end) = rest[start..].find("}}") else {
            out.push_str(&rest[start..]);
            return out;
        };
        let inner = rest[start + 2..start + end].trim();
        let (head, filter) = match inner.split_once('|') {
            Some((h, f)) => (h.trim(), Some(f.trim())),
            None => (inner, None),
        };
        let text = if head == "$value" {
            Some(match filter {
                Some("percent") => format!("{}%", format_value(value * 100.0)),
                Some("duration") if value.is_finite() => {
                    expr::format_duration(((value.max(0.0)) * 1000.0) as i64 / 1000 * 1000)
                }
                _ => format_value(value),
            })
        } else {
            head.strip_prefix("$labels.").map(|k| labels.get(k).cloned().unwrap_or_default())
        };
        match text {
            Some(t) => out.push_str(&t),
            None => out.push_str(&rest[start..start + end + 2]),
        }
        rest = &rest[start + end + 2..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_parse_and_have_runbooks() {
        let b = builtins("forgebase_tls_certificate_expiry_timestamp_seconds");
        assert_eq!(b.len(), 14, "every built-in parses");
        for r in &b {
            assert!(r.annotations["runbook_url"].contains("observability.md#"), "{}", r.id);
        }
    }

    #[test]
    fn templates_fill_labels_and_values() {
        let l: BTreeMap<String, String> = [("subscription".to_owned(), "orders".to_owned())].into_iter().collect();
        assert_eq!(
            render("{{ $labels.subscription }} has {{ $value }} ({{$value|percent}})", &l, 0.125),
            "orders has 0.125 (12.5%)"
        );
        assert_eq!(render("{{ $value | duration }} {{ nope }}", &l, 7_500.0), "2h5m {{ nope }}");
        assert_eq!(render("unclosed {{ x", &l, 1.0), "unclosed {{ x");
    }

    #[test]
    fn slo_tiers_scale_with_the_window() {
        let slo = Slo {
            id: "s".into(),
            name: "api".into(),
            description: String::new(),
            kind: SloKind::Availability,
            objective: 0.999,
            window: "30d".into(),
            window_ms: 30 * 86_400_000,
            route_prefix: None,
            route_regex: None,
            methods: Vec::new(),
            latency_threshold_ms: None,
            alerting: true,
            labels: BTreeMap::new(),
            created_at: String::new(),
            updated_at: String::new(),
        };
        let r = slo_rules(&slo);
        assert_eq!(r.len(), 3);
        assert!((r[0].threshold - 14.4).abs() < 1e-9);
        assert!((r[1].threshold - 6.0).abs() < 1e-9);
        assert!((r[2].threshold - 1.0).abs() < 1e-9);
        let short = Slo {
            window: "1d".into(),
            window_ms: 86_400_000,
            ..slo
        };
        assert_eq!(slo_rules(&short).len(), 2, "the 3d tier does not fit a 1d window");
    }

    #[test]
    fn fingerprints_depend_on_rule_and_labels() {
        let a: BTreeMap<String, String> = [("x".to_owned(), "1".to_owned())].into_iter().collect();
        let b: BTreeMap<String, String> = [("x".to_owned(), "2".to_owned())].into_iter().collect();
        assert_ne!(fingerprint("r", &a), fingerprint("r", &b));
        assert_ne!(fingerprint("r", &a), fingerprint("s", &a));
        assert_eq!(fingerprint("r", &a), fingerprint("r", &a.clone()));
    }
}
