//! The alert service: settings, administrator checks and the CRUD of
//! rules, silences, notifiers and SLOs. Evaluation and delivery live in
//! [`crate::engine`].

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use forgebase_auth::crypto::Keyring;
use forgebase_core::DataLayout;
use forgebase_core::config::telemetry_config::{AlertRuleDecl, AlertsConfig};
use forgebase_data::{AccessContext, Principal};
use forgebase_db::{Backend, Statement, Value};
use forgebase_jobs::Clock;
use serde_json::{Value as Json, json};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::error::{AlertError, Result};
use crate::expr::{self, CmpOp, MatchOp, Matcher};
use crate::model::*;
use crate::rules::{self, RuleDef};
use crate::samples::History;
use crate::slo::SloCounter;
use crate::store::{Row, new_id, read, to_json, write};

/// Where the engine reads Prometheus metrics from.
pub trait MetricsSource: Send + Sync + fmt::Debug {
    /// The Prometheus text exposition.
    fn render(&self) -> String;
}

/// This process's Prometheus registry (`forgebase_observability::metrics_handle`).
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessMetrics;

impl MetricsSource for ProcessMetrics {
    fn render(&self) -> String {
        forgebase_observability::metrics_handle().render()
    }
}

/// Engine settings, from `[alerts]`.
#[derive(Debug, Clone)]
pub struct AlertSettings {
    /// Between evaluation rounds.
    pub interval: Duration,
    /// Built-in rules (by id) switched off in the file.
    pub disabled_rules: Vec<String>,
    /// Notifier URLs may be `http://`.
    pub allow_http: bool,
    /// Resolved alerts and deliveries are kept this long.
    pub history_retention_ms: i64,
    /// Certificate expiry gauge name.
    pub tls_cert_expiry_metric: String,
    /// `[[alerts.rules]]`.
    pub config_rules: Vec<AlertRuleDecl>,
    /// Delivery attempts before a notification is given up.
    pub max_attempts: i64,
    /// First retry delay (doubles, capped at an hour).
    pub retry_base_ms: i64,
    /// Test sends per notifier and minute.
    pub test_sends_per_minute: usize,
}

impl Default for AlertSettings {
    fn default() -> Self {
        Self::from_config(&AlertsConfig::default())
    }
}

impl AlertSettings {
    /// From the resolved config.
    pub fn from_config(c: &AlertsConfig) -> Self {
        Self {
            interval: Duration::from_secs(c.evaluation_interval_secs.max(1)),
            disabled_rules: c.disabled_rules.clone(),
            allow_http: c.allow_http,
            history_retention_ms: i64::try_from(c.history_retention_days.saturating_mul(86_400_000))
                .unwrap_or(i64::MAX),
            tls_cert_expiry_metric: c.tls_cert_expiry_metric.clone(),
            config_rules: c.rules.clone(),
            max_attempts: 8,
            retry_base_ms: 30_000,
            test_sends_per_minute: 10,
        }
    }
}

/// What the service talks to.
#[derive(Clone)]
pub struct AlertDeps {
    /// The control database.
    pub db: Arc<dyn Backend>,
    /// Seals notifier secrets.
    pub keyring: Keyring,
    /// Time source.
    pub clock: Arc<dyn Clock>,
    /// SSRF-safe client for webhook, Slack and PagerDuty notifiers.
    pub fetcher: Option<forgebase_functions::Fetcher>,
    /// Email (templates and SMTP) for email notifiers.
    pub customize: Option<forgebase_customize::CustomizeService>,
    /// Queues: signals and queue-topic notifiers.
    pub queue: Option<forgebase_queue::QueueService>,
    /// The data directory: disk and backup signals.
    pub layout: Option<DataLayout>,
    /// Prometheus metrics.
    pub metrics: Arc<dyn MetricsSource>,
    /// Settings.
    pub settings: AlertSettings,
    /// `[server] public_url`, linked from notifications.
    pub public_url: Option<String>,
}

impl fmt::Debug for AlertDeps {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AlertDeps")
            .field("fetcher", &self.fetcher.is_some())
            .field("customize", &self.customize.is_some())
            .field("queue", &self.queue.is_some())
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

pub(crate) struct Inner {
    pub(crate) deps: AlertDeps,
    pub(crate) builtins: Vec<RuleDef>,
    pub(crate) config_rules: Vec<RuleDef>,
    pub(crate) history: Mutex<History>,
    pub(crate) slo: Arc<SloCounter>,
    pub(crate) wake: Notify,
    pub(crate) stop: Notify,
    pub(crate) task: Mutex<Option<JoinHandle<()>>>,
    pub(crate) tests: Mutex<HashMap<String, Vec<i64>>>,
    pub(crate) round: tokio::sync::Mutex<()>,
    pub(crate) template_ready: std::sync::atomic::AtomicBool,
}

/// The alerting engine and its API (ADR-0035). Cheap to clone.
#[derive(Clone)]
pub struct AlertService {
    pub(crate) inner: Arc<Inner>,
}

impl fmt::Debug for AlertService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AlertService").field("deps", &self.inner.deps).finish()
    }
}

pub(crate) const RULE_COLS: &str =
    "id, name, expr, op, threshold, for_ms, severity, labels, annotations, enabled, created_at, updated_at";
pub(crate) const ALERT_COLS: &str = "id, fingerprint, rule_id, name, severity, state, labels, annotations, value, \
     active_at, fired_at, resolved_at, last_eval_at";
pub(crate) const NOTIFIER_COLS: &str = "id, name, kind, config, secret, enabled, send_resolved, last_success_at, \
     last_failure_at, last_error, created_at, updated_at";
pub(crate) const SLO_COLS: &str = "id, name, description, kind, objective, window_ms, route_prefix, route_regex, \
     methods, latency_threshold_ms, alerting, labels, created_at, updated_at";
pub(crate) const DELIVERY_COLS: &str = "id, notifier_id, group_key, event, status, payload, attempts, \
     next_attempt_at, last_status, last_error, sent_at, created_at";

/// A notifier as stored.
#[derive(Debug, Clone)]
pub(crate) struct NotifierRow {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) kind: NotifierKind,
    pub(crate) url: Option<String>,
    pub(crate) to: Vec<String>,
    pub(crate) topic: Option<String>,
    pub(crate) matchers: Vec<LabelMatcher>,
    pub(crate) min_severity: Severity,
    pub(crate) group_by: Vec<String>,
    pub(crate) repeat_ms: i64,
    pub(crate) secret: Option<Vec<u8>>,
    pub(crate) enabled: bool,
    pub(crate) send_resolved: bool,
    pub(crate) last_success_at: Option<i64>,
    pub(crate) last_failure_at: Option<i64>,
    pub(crate) last_error: Option<String>,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
}

impl NotifierRow {
    pub(crate) fn from_row(r: &Row<'_>) -> Self {
        let cfg: Json = serde_json::from_str(&r.text("config")).unwrap_or(Json::Null);
        let strs = |k: &str| -> Vec<String> {
            cfg.get(k)
                .and_then(Json::as_array)
                .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
                .unwrap_or_default()
        };
        Self {
            id: r.text("id"),
            name: r.text("name"),
            kind: NotifierKind::parse(&r.text("kind")).unwrap_or(NotifierKind::Webhook),
            url: cfg.get("url").and_then(Json::as_str).map(str::to_owned),
            to: strs("to"),
            topic: cfg.get("topic").and_then(Json::as_str).map(str::to_owned),
            matchers: cfg
                .get("matchers")
                .cloned()
                .and_then(|m| serde_json::from_value(m).ok())
                .unwrap_or_default(),
            min_severity: cfg
                .get("min_severity")
                .and_then(Json::as_str)
                .and_then(Severity::parse)
                .unwrap_or(Severity::Info),
            group_by: {
                let g = strs("group_by");
                if g.is_empty() { vec!["alertname".to_owned()] } else { g }
            },
            repeat_ms: cfg
                .get("repeat_interval_ms")
                .and_then(Json::as_i64)
                .unwrap_or(4 * 3_600_000),
            secret: r.blob("secret"),
            enabled: r.int("enabled") == 1,
            send_resolved: r.int("send_resolved") == 1,
            last_success_at: r.opt_int("last_success_at"),
            last_failure_at: r.opt_int("last_failure_at"),
            last_error: r.opt_text("last_error"),
            created_at: r.int("created_at"),
            updated_at: r.int("updated_at"),
        }
    }

    pub(crate) fn view(&self, signing_secret: Option<String>) -> Notifier {
        // For Slack `url` holds the host only: the full URL is the secret.
        let url = self.url.clone();
        Notifier {
            id: self.id.clone(),
            name: self.name.clone(),
            kind: self.kind,
            url,
            to: self.to.clone(),
            topic: self.topic.clone(),
            matchers: self.matchers.clone(),
            min_severity: self.min_severity,
            group_by: self.group_by.clone(),
            repeat_interval: expr::format_duration(self.repeat_ms),
            send_resolved: self.send_resolved,
            enabled: self.enabled,
            last_success_at: self.last_success_at.map(iso),
            last_failure_at: self.last_failure_at.map(iso),
            last_error: self.last_error.clone(),
            created_at: iso(self.created_at),
            updated_at: iso(self.updated_at),
            signing_secret,
        }
    }
}

pub(crate) fn alert_from(r: &Row<'_>) -> Alert {
    Alert {
        id: r.text("id"),
        fingerprint: r.text("fingerprint"),
        rule_id: r.text("rule_id"),
        name: r.text("name"),
        severity: Severity::parse(&r.text("severity")).unwrap_or(Severity::Warning),
        state: AlertState::parse(&r.text("state")).unwrap_or(AlertState::Resolved),
        labels: r.map("labels"),
        annotations: r.map("annotations"),
        value: r.opt_float("value"),
        active_at: iso(r.int("active_at")),
        fired_at: r.opt_int("fired_at").map(iso),
        resolved_at: r.opt_int("resolved_at").map(iso),
        last_evaluated_at: iso(r.int("last_eval_at")),
        silenced_by: Vec::new(),
    }
}

pub(crate) fn slo_from(r: &Row<'_>) -> Slo {
    Slo {
        id: r.text("id"),
        name: r.text("name"),
        description: r.text("description"),
        kind: if r.text("kind") == "latency" {
            SloKind::Latency
        } else {
            SloKind::Availability
        },
        objective: r.float("objective"),
        window: expr::format_duration(r.int("window_ms")),
        window_ms: r.int("window_ms"),
        route_prefix: r.opt_text("route_prefix"),
        route_regex: r.opt_text("route_regex"),
        methods: r.json("methods"),
        latency_threshold_ms: r.opt_int("latency_threshold_ms"),
        alerting: r.int("alerting") == 1,
        labels: r.map("labels"),
        created_at: iso(r.int("created_at")),
        updated_at: iso(r.int("updated_at")),
    }
}

pub(crate) fn delivery_from(r: &Row<'_>) -> Delivery {
    let status = r.text("status");
    Delivery {
        id: r.text("id"),
        notifier_id: r.text("notifier_id"),
        group_key: r.text("group_key"),
        event: r.text("event"),
        next_attempt_at: (status == "pending").then(|| iso(r.int("next_attempt_at"))),
        status,
        attempts: r.int("attempts"),
        last_status: r.opt_int("last_status"),
        last_error: r.opt_text("last_error"),
        sent_at: r.opt_int("sent_at").map(iso),
        created_at: iso(r.int("created_at")),
        payload: serde_json::from_str(&r.text("payload")).unwrap_or(Json::Null),
    }
}

/// Compiles matchers of a silence or notifier.
pub(crate) fn compile_matchers(ms: &[LabelMatcher]) -> Result<Vec<Matcher>> {
    ms.iter()
        .map(|m| {
            let op = match m.op.as_str() {
                "=" => MatchOp::Eq,
                "!=" => MatchOp::Ne,
                "=~" => MatchOp::Re,
                "!~" => MatchOp::NotRe,
                other => {
                    return Err(AlertError::invalid(
                        "invalid_matcher",
                        format!("matcher op {other:?}: use =, !=, =~ or !~"),
                    ));
                }
            };
            if m.label.is_empty() || m.label.len() > 64 {
                return Err(AlertError::invalid("invalid_matcher", "a matcher needs a label name"));
            }
            Matcher::new(&m.label, op, &m.value).map_err(|e| AlertError::invalid("invalid_matcher", e.0))
        })
        .collect()
}

fn check_url(raw: &str, allow_http: bool, what: &str) -> Result<String> {
    let u = url::Url::parse(raw.trim())
        .map_err(|_| AlertError::invalid("invalid_notifier", format!("{what} is not an absolute URL")))?;
    match u.scheme() {
        "https" => {}
        "http" if allow_http => {}
        "http" => {
            return Err(AlertError::invalid(
                "invalid_notifier",
                format!("{what} must start with https:// (set [alerts] allow_http = true for http://)"),
            ));
        }
        _ => {
            return Err(AlertError::invalid(
                "invalid_notifier",
                format!("{what} must be an https:// URL"),
            ));
        }
    }
    if !u.username().is_empty() || u.password().is_some() {
        return Err(AlertError::invalid(
            "invalid_notifier",
            format!("{what} may not carry a user or password"),
        ));
    }
    let host = u
        .host_str()
        .ok_or_else(|| AlertError::invalid("invalid_notifier", format!("{what} has no host")))?;
    let literal = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = literal.parse::<std::net::IpAddr>()
        && let Some(why) = forgebase_auth::net::blocked_reason(ip)
    {
        return Err(AlertError::invalid(
            "invalid_notifier",
            format!("{what} points at {ip} ({why}); notifiers only reach public addresses"),
        ));
    }
    Ok(u.to_string())
}

/// Default PagerDuty Events API v2 endpoint.
pub const PAGERDUTY_EVENTS_URL: &str = "https://events.pagerduty.com/v2/enqueue";

impl AlertService {
    /// Opens the service: checks the file's rules (an invalid one stops
    /// the start) and loads the SLOs into the request counter.
    pub async fn open(deps: AlertDeps) -> Result<Self> {
        let config_rules = rules::from_config(&deps.settings.config_rules)?;
        let builtins = rules::builtins(&deps.settings.tls_cert_expiry_metric);
        let slo = Arc::new(SloCounter::new(deps.clock.clone()));
        let svc = Self {
            inner: Arc::new(Inner {
                deps,
                builtins,
                config_rules,
                history: Mutex::new(History::default()),
                slo,
                wake: Notify::new(),
                stop: Notify::new(),
                task: Mutex::new(None),
                tests: Mutex::new(HashMap::new()),
                round: tokio::sync::Mutex::new(()),
                template_ready: std::sync::atomic::AtomicBool::new(false),
            }),
        };
        svc.reload_slos().await?;
        Ok(svc)
    }

    /// The request counter to register as an HTTP observer.
    pub fn http_observer(&self) -> Arc<dyn forgebase_observability::HttpObserver> {
        self.inner.slo.clone()
    }

    pub(crate) fn db(&self) -> &dyn Backend {
        self.inner.deps.db.as_ref()
    }

    pub(crate) fn now(&self) -> i64 {
        self.inner.deps.clock.now_ms()
    }

    pub(crate) fn settings(&self) -> &AlertSettings {
        &self.inner.deps.settings
    }

    /// Platform administrators (and the system) only.
    pub fn require_admin(&self, ctx: &AccessContext) -> Result<()> {
        if ctx.is_privileged() {
            return Ok(());
        }
        if ctx.principal == Principal::Anonymous {
            return Err(AlertError::Unauthenticated);
        }
        Err(AlertError::Forbidden)
    }

    // ------------------------------------------------------------- rules

    async fn rule_settings(&self) -> Result<HashMap<String, bool>> {
        let rows = read(
            self.db(),
            "SELECT rule_id, enabled FROM _forgebase_alert_rule_settings",
            vec![],
        )
        .await?;
        Ok(Row::all(&rows)
            .map(|r| (r.text("rule_id"), r.int("enabled") == 1))
            .collect())
    }

    fn api_rule(r: &Row<'_>) -> Option<RuleDef> {
        let op = CmpOp::parse(&r.text("op"))?;
        let mut def = rules::build(
            r.text("id"),
            &r.text("name"),
            RuleSource::Api,
            &r.text("expr"),
            op,
            r.float("threshold"),
            r.int("for_ms"),
            Severity::parse(&r.text("severity")).unwrap_or(Severity::Warning),
            r.map("labels"),
            r.map("annotations"),
        )
        .ok()?;
        def.enabled = r.int("enabled") == 1;
        def.times = Some((r.int("created_at"), r.int("updated_at")));
        Some(def)
    }

    /// Every rule: built-ins, the file's, the API's and the SLOs' burn-rate
    /// rules, with their enabled state.
    pub(crate) async fn all_rules(&self) -> Result<Vec<RuleDef>> {
        let settings = self.rule_settings().await?;
        let disabled: BTreeSet<&str> = self.settings().disabled_rules.iter().map(String::as_str).collect();
        let mut out = Vec::new();
        for r in self.inner.builtins.iter().chain(&self.inner.config_rules) {
            let mut r = r.clone();
            r.disabled_by_config = disabled.contains(r.id.as_str());
            if let Some(e) = settings.get(&r.id) {
                r.enabled = *e;
            }
            out.push(r);
        }
        let rows = read(
            self.db(),
            &format!("SELECT {RULE_COLS} FROM _forgebase_alert_rules ORDER BY name"),
            vec![],
        )
        .await?;
        out.extend(Row::all(&rows).filter_map(|r| Self::api_rule(&r)));
        for slo in self.slos_unchecked().await? {
            for mut r in rules::slo_rules(&slo) {
                if let Some(e) = settings.get(&r.id) {
                    r.enabled = *e;
                }
                out.push(r);
            }
        }
        Ok(out)
    }

    /// Lists every rule.
    pub async fn rules(&self, ctx: &AccessContext) -> Result<Vec<Rule>> {
        self.require_admin(ctx)?;
        Ok(self.all_rules().await?.iter().map(RuleDef::view).collect())
    }

    /// One rule.
    pub async fn rule(&self, ctx: &AccessContext, id: &str) -> Result<Rule> {
        self.require_admin(ctx)?;
        self.find_rule(id).await.map(|r| r.view())
    }

    async fn find_rule(&self, id: &str) -> Result<RuleDef> {
        self.all_rules()
            .await?
            .into_iter()
            .find(|r| r.id == id)
            .ok_or_else(|| AlertError::not_found("alert_rule_not_found", format!("no alert rule {id:?}")))
    }

    /// Creates a custom rule.
    pub async fn create_rule(&self, ctx: &AccessContext, input: RuleInput) -> Result<Rule> {
        self.require_admin(ctx)?;
        let def = rules::build(
            new_id("rule"),
            &input.name,
            RuleSource::Api,
            &input.expr,
            input.op.unwrap_or(CmpOp::Gt),
            input.threshold,
            rules::parse_for(input.for_.as_deref())?,
            input.severity.unwrap_or(Severity::Warning),
            input.labels,
            input.annotations,
        )?;
        let taken = read(
            self.db(),
            "SELECT id FROM _forgebase_alert_rules WHERE name = ?1",
            vec![def.name.as_str().into()],
        )
        .await?;
        if !taken.rows.is_empty() || self.inner.config_rules.iter().any(|r| r.name == def.name) {
            return Err(AlertError::conflict(
                "alert_rule_exists",
                format!("a rule named {} exists", def.name),
            ));
        }
        let now = self.now();
        write(
            self.db(),
            vec![Statement::new(
                format!(
                    "INSERT INTO _forgebase_alert_rules ({RULE_COLS}, created_by) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11, ?12)"
                ),
                vec![
                    def.id.as_str().into(),
                    def.name.as_str().into(),
                    def.expr_src.as_str().into(),
                    def.op.as_str().into(),
                    def.threshold.into(),
                    def.for_ms.into(),
                    def.severity.as_str().into(),
                    to_json(&def.labels)?.into(),
                    to_json(&def.annotations)?.into(),
                    i64::from(input.enabled.unwrap_or(true)).into(),
                    now.into(),
                    ctx.principal_key().into(),
                ],
            )],
        )
        .await
        .map_err(|e| match e {
            AlertError::Internal(m) if m.to_ascii_lowercase().contains("unique") => {
                AlertError::conflict("alert_rule_exists", format!("a rule named {} exists", def.name))
            }
            e => e,
        })?;
        self.wake();
        self.rule(ctx, &def.id).await
    }

    /// Changes a rule: any field of an API rule; only `enabled` of a
    /// built-in, file or SLO rule.
    pub async fn patch_rule(&self, ctx: &AccessContext, id: &str, p: RulePatch) -> Result<Rule> {
        self.require_admin(ctx)?;
        let cur = self.find_rule(id).await?;
        let now = self.now();
        if cur.source != RuleSource::Api {
            let other = p.expr.is_some()
                || p.op.is_some()
                || p.threshold.is_some()
                || p.for_.is_some()
                || p.severity.is_some()
                || p.labels.is_some()
                || p.annotations.is_some();
            if other {
                return Err(AlertError::conflict(
                    "rule_read_only",
                    "built-in, file and SLO rules can only be switched on or off; create a custom rule instead",
                ));
            }
            let Some(enabled) = p.enabled else {
                return Ok(cur.view());
            };
            if enabled && cur.disabled_by_config {
                return Err(AlertError::conflict(
                    "rule_disabled_by_config",
                    format!("{id} is listed in [alerts] disabled_rules; remove it there"),
                ));
            }
            write(
                self.db(),
                vec![Statement::new(
                    "INSERT INTO _forgebase_alert_rule_settings (rule_id, enabled, updated_by, updated_at)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT (rule_id) DO UPDATE SET enabled = excluded.enabled,
                         updated_by = excluded.updated_by, updated_at = excluded.updated_at",
                    vec![
                        id.into(),
                        i64::from(enabled).into(),
                        ctx.principal_key().into(),
                        now.into(),
                    ],
                )],
            )
            .await?;
            self.wake();
            return self.rule(ctx, id).await;
        }
        let def = rules::build(
            cur.id.clone(),
            &cur.name,
            RuleSource::Api,
            p.expr.as_deref().unwrap_or(&cur.expr_src),
            p.op.unwrap_or(cur.op),
            p.threshold.unwrap_or(cur.threshold),
            match &p.for_ {
                Some(f) => rules::parse_for(Some(f))?,
                None => cur.for_ms,
            },
            p.severity.unwrap_or(cur.severity),
            p.labels.unwrap_or(cur.labels),
            p.annotations.unwrap_or(cur.annotations),
        )?;
        write(
            self.db(),
            vec![Statement::new(
                "UPDATE _forgebase_alert_rules SET expr = ?2, op = ?3, threshold = ?4, for_ms = ?5, severity = ?6,
                     labels = ?7, annotations = ?8, enabled = ?9, updated_at = ?10 WHERE id = ?1",
                vec![
                    id.into(),
                    def.expr_src.as_str().into(),
                    def.op.as_str().into(),
                    def.threshold.into(),
                    def.for_ms.into(),
                    def.severity.as_str().into(),
                    to_json(&def.labels)?.into(),
                    to_json(&def.annotations)?.into(),
                    i64::from(p.enabled.unwrap_or(cur.enabled)).into(),
                    now.into(),
                ],
            )],
        )
        .await?;
        self.wake();
        self.rule(ctx, id).await
    }

    /// Deletes a custom rule (its open alerts resolve at the next round).
    pub async fn delete_rule(&self, ctx: &AccessContext, id: &str) -> Result<()> {
        self.require_admin(ctx)?;
        let cur = self.find_rule(id).await?;
        if cur.source != RuleSource::Api {
            return Err(AlertError::conflict(
                "rule_read_only",
                "only custom rules can be deleted; switch this one off instead",
            ));
        }
        write(
            self.db(),
            vec![Statement::new(
                "DELETE FROM _forgebase_alert_rules WHERE id = ?1",
                vec![id.into()],
            )],
        )
        .await?;
        self.wake();
        Ok(())
    }

    // ------------------------------------------------------------ alerts

    /// Open alerts (pending and firing), most severe and oldest first.
    pub async fn active(&self, ctx: &AccessContext, state: Option<&str>, severity: Option<&str>) -> Result<Vec<Alert>> {
        self.require_admin(ctx)?;
        let rows = read(
            self.db(),
            &format!(
                "SELECT {ALERT_COLS} FROM _forgebase_alerts WHERE state IN ('pending', 'firing') ORDER BY active_at"
            ),
            vec![],
        )
        .await?;
        let silences = self.active_silences().await?;
        let mut out: Vec<Alert> = Row::all(&rows)
            .map(|r| alert_from(&r))
            .filter(|a| state.is_none_or(|s| a.state.as_str() == s))
            .filter(|a| severity.is_none_or(|s| a.severity.as_str() == s))
            .map(|mut a| {
                a.silenced_by = silenced_by(&silences, &a.labels);
                a
            })
            .collect();
        out.sort_by(|a, b| b.severity.cmp(&a.severity).then(a.active_at.cmp(&b.active_at)));
        Ok(out)
    }

    /// Every alert, newest first: `state`, `rule_id`, `from`/`to` (active
    /// time), `before` (paging), `limit` (1-500, default 100).
    #[allow(clippy::too_many_arguments)]
    pub async fn history(
        &self,
        ctx: &AccessContext,
        state: Option<&str>,
        rule_id: Option<&str>,
        from: Option<i64>,
        to: Option<i64>,
        before: Option<&str>,
        limit: Option<u32>,
    ) -> Result<AlertPage> {
        self.require_admin(ctx)?;
        let limit = i64::from(limit.unwrap_or(100).clamp(1, 500));
        let mut sql = format!("SELECT {ALERT_COLS} FROM _forgebase_alerts WHERE 1 = 1");
        let mut params: Vec<Value> = Vec::new();
        let mut add = |cond: &str, v: Value, params: &mut Vec<Value>| {
            params.push(v);
            sql.push_str(&cond.replace("?", &format!("?{}", params.len())));
        };
        if let Some(s) = state {
            if AlertState::parse(s).is_none() {
                return Err(AlertError::invalid(
                    "invalid_query",
                    "state is pending, firing or resolved",
                ));
            }
            add(" AND state = ?", s.into(), &mut params);
        }
        if let Some(r) = rule_id {
            add(" AND rule_id = ?", r.into(), &mut params);
        }
        if let Some(f) = from {
            add(" AND active_at >= ?", f.into(), &mut params);
        }
        if let Some(t) = to {
            add(" AND active_at < ?", t.into(), &mut params);
        }
        if let Some(b) = before {
            let (at, id) = b
                .split_once('_')
                .and_then(|(a, i)| a.parse::<i64>().ok().map(|a| (a, i.to_owned())))
                .ok_or_else(|| AlertError::invalid("invalid_query", "before is the next_before of a previous page"))?;
            params.push(at.into());
            let n = params.len();
            params.push(id.into());
            sql.push_str(&format!(
                " AND (active_at < ?{n} OR (active_at = ?{n} AND id < ?{}))",
                n + 1
            ));
        }
        params.push((limit + 1).into());
        sql.push_str(&format!(" ORDER BY active_at DESC, id DESC LIMIT ?{}", params.len()));
        let rows = read(self.db(), &sql, params).await?;
        let mut items: Vec<(i64, Alert)> = Row::all(&rows).map(|r| (r.int("active_at"), alert_from(&r))).collect();
        let next_before = if items.len() as i64 > limit {
            items.truncate(usize::try_from(limit).unwrap_or(100));
            items.last().map(|(at, a)| format!("{at}_{}", a.id))
        } else {
            None
        };
        Ok(AlertPage {
            items: items.into_iter().map(|(_, a)| a).collect(),
            next_before,
        })
    }

    /// One alert with its state changes.
    pub async fn alert(&self, ctx: &AccessContext, id: &str) -> Result<AlertDetail> {
        self.require_admin(ctx)?;
        let rows = read(
            self.db(),
            &format!("SELECT {ALERT_COLS} FROM _forgebase_alerts WHERE id = ?1"),
            vec![id.into()],
        )
        .await?;
        let mut alert = Row::all(&rows)
            .next()
            .map(|r| alert_from(&r))
            .ok_or_else(|| AlertError::not_found("alert_not_found", format!("no alert {id:?}")))?;
        if alert.state != AlertState::Resolved {
            alert.silenced_by = silenced_by(&self.active_silences().await?, &alert.labels);
        }
        let ev = read(
            self.db(),
            "SELECT at, state, value FROM _forgebase_alert_events WHERE alert_id = ?1 ORDER BY id",
            vec![id.into()],
        )
        .await?;
        let events = Row::all(&ev)
            .map(|r| AlertEvent {
                at: iso(r.int("at")),
                state: AlertState::parse(&r.text("state")).unwrap_or(AlertState::Resolved),
                value: r.opt_float("value"),
            })
            .collect();
        Ok(AlertDetail { alert, events })
    }

    /// Evaluates an expression now (no state change).
    pub async fn query(&self, ctx: &AccessContext, input: QueryInput) -> Result<QueryResult> {
        self.require_admin(ctx)?;
        let e = expr::parse(&input.expr).map_err(|e| AlertError::invalid("invalid_expression", e.0))?;
        let mut names = BTreeSet::new();
        e.names(&mut names);
        let snap = self.snapshot(&names, true).await.0;
        let history = self
            .inner
            .history
            .lock()
            .map_err(|_| AlertError::Internal("history lock".into()))?;
        let samples = expr::eval(
            &e,
            &expr::Context {
                now: &snap,
                history: &history,
            },
        )
        .into_samples();
        Ok(QueryResult {
            at: iso(snap.at_ms),
            samples: samples
                .into_iter()
                .map(|s| QuerySample {
                    labels: s.labels.into_iter().filter(|(k, _)| k != "__name__").collect(),
                    value: (!s.value.is_nan()).then_some(s.value),
                })
                .collect(),
        })
    }

    // ---------------------------------------------------------- silences

    pub(crate) async fn active_silences(&self) -> Result<Vec<(String, Vec<Matcher>)>> {
        let now = self.now();
        let rows = read(
            self.db(),
            "SELECT id, matchers FROM _forgebase_alert_silences WHERE starts_at <= ?1 AND ends_at > ?1",
            vec![now.into()],
        )
        .await?;
        Ok(Row::all(&rows)
            .filter_map(|r| {
                let ms: Vec<LabelMatcher> = r.json("matchers");
                compile_matchers(&ms).ok().map(|m| (r.text("id"), m))
            })
            .collect())
    }

    fn silence_from(&self, r: &Row<'_>) -> Silence {
        let now = self.now();
        let (start, end) = (r.int("starts_at"), r.int("ends_at"));
        Silence {
            id: r.text("id"),
            matchers: r.json("matchers"),
            starts_at: iso(start),
            ends_at: iso(end),
            comment: r.text("comment"),
            created_by: r.text("created_by"),
            created_at: iso(r.int("created_at")),
            status: if end <= now {
                "expired"
            } else if start > now {
                "pending"
            } else {
                "active"
            }
            .into(),
        }
    }

    fn silence_window(&self, input: &SilenceInput) -> Result<(i64, i64)> {
        if input.matchers.is_empty() || input.matchers.len() > 16 {
            return Err(AlertError::invalid(
                "invalid_silence",
                "a silence needs 1 to 16 matchers",
            ));
        }
        compile_matchers(&input.matchers).map_err(|e| AlertError::invalid("invalid_silence", e.to_string()))?;
        let now = self.now();
        let start = match &input.starts_at {
            Some(s) => {
                parse_iso(s).ok_or_else(|| AlertError::invalid("invalid_silence", "starts_at is not RFC 3339"))?
            }
            None => now,
        };
        let end = match (&input.ends_at, &input.duration) {
            (Some(e), _) => {
                parse_iso(e).ok_or_else(|| AlertError::invalid("invalid_silence", "ends_at is not RFC 3339"))?
            }
            (None, Some(d)) => {
                start
                    + expr::parse_duration(d)
                        .filter(|d| *d > 0)
                        .ok_or_else(|| AlertError::invalid("invalid_silence", "duration is a length such as 2h"))?
            }
            (None, None) => return Err(AlertError::invalid("invalid_silence", "give ends_at or duration")),
        };
        if end <= start || end <= now {
            return Err(AlertError::invalid(
                "invalid_silence",
                "ends_at must be after starts_at and in the future",
            ));
        }
        if end - start > 366 * 86_400_000 {
            return Err(AlertError::invalid("invalid_silence", "a silence lasts at most a year"));
        }
        if input.comment.len() > 2000 {
            return Err(AlertError::invalid("invalid_silence", "comment is at most 2000 bytes"));
        }
        Ok((start, end))
    }

    /// Silences, newest first (`all`: expired ones too).
    pub async fn silences(&self, ctx: &AccessContext, all: bool) -> Result<Vec<Silence>> {
        self.require_admin(ctx)?;
        let rows = if all {
            read(
                self.db(),
                "SELECT * FROM _forgebase_alert_silences ORDER BY created_at DESC LIMIT 1000",
                vec![],
            )
            .await?
        } else {
            read(
                self.db(),
                "SELECT * FROM _forgebase_alert_silences WHERE ends_at > ?1 ORDER BY created_at DESC LIMIT 1000",
                vec![self.now().into()],
            )
            .await?
        };
        Ok(Row::all(&rows).map(|r| self.silence_from(&r)).collect())
    }

    /// One silence.
    pub async fn silence(&self, ctx: &AccessContext, id: &str) -> Result<Silence> {
        self.require_admin(ctx)?;
        let rows = read(
            self.db(),
            "SELECT * FROM _forgebase_alert_silences WHERE id = ?1",
            vec![id.into()],
        )
        .await?;
        Row::all(&rows)
            .next()
            .map(|r| self.silence_from(&r))
            .ok_or_else(|| AlertError::not_found("silence_not_found", format!("no silence {id:?}")))
    }

    /// Creates a silence.
    pub async fn create_silence(&self, ctx: &AccessContext, input: SilenceInput) -> Result<Silence> {
        self.require_admin(ctx)?;
        let (start, end) = self.silence_window(&input)?;
        let id = new_id("sil");
        let now = self.now();
        write(
            self.db(),
            vec![Statement::new(
                "INSERT INTO _forgebase_alert_silences (id, matchers, starts_at, ends_at, comment, created_by,
                     created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
                vec![
                    id.as_str().into(),
                    to_json(&input.matchers)?.into(),
                    start.into(),
                    end.into(),
                    input.comment.as_str().into(),
                    ctx.principal_key().into(),
                    now.into(),
                ],
            )],
        )
        .await?;
        self.wake();
        self.silence(ctx, &id).await
    }

    /// Replaces a silence's matchers, window and comment.
    pub async fn update_silence(&self, ctx: &AccessContext, id: &str, input: SilenceInput) -> Result<Silence> {
        self.silence(ctx, id).await?;
        let (start, end) = self.silence_window(&input)?;
        write(
            self.db(),
            vec![Statement::new(
                "UPDATE _forgebase_alert_silences SET matchers = ?2, starts_at = ?3, ends_at = ?4, comment = ?5,
                     updated_at = ?6 WHERE id = ?1",
                vec![
                    id.into(),
                    to_json(&input.matchers)?.into(),
                    start.into(),
                    end.into(),
                    input.comment.as_str().into(),
                    self.now().into(),
                ],
            )],
        )
        .await?;
        self.wake();
        self.silence(ctx, id).await
    }

    /// Ends a silence now (it stays listed as expired).
    pub async fn expire_silence(&self, ctx: &AccessContext, id: &str) -> Result<Silence> {
        self.silence(ctx, id).await?;
        let now = self.now();
        write(
            self.db(),
            vec![Statement::new(
                "UPDATE _forgebase_alert_silences SET ends_at = ?2, starts_at = CASE WHEN starts_at > ?2 THEN ?2
                     ELSE starts_at END, updated_at = ?2 WHERE id = ?1 AND ends_at > ?2",
                vec![id.into(), now.into()],
            )],
        )
        .await?;
        self.wake();
        self.silence(ctx, id).await
    }

    // --------------------------------------------------------- notifiers

    pub(crate) async fn notifier_rows(&self) -> Result<Vec<NotifierRow>> {
        let rows = read(
            self.db(),
            &format!("SELECT {NOTIFIER_COLS} FROM _forgebase_alert_notifiers ORDER BY name"),
            vec![],
        )
        .await?;
        Ok(Row::all(&rows).map(|r| NotifierRow::from_row(&r)).collect())
    }

    pub(crate) async fn notifier_row(&self, id_or_name: &str) -> Result<NotifierRow> {
        let rows = read(
            self.db(),
            &format!("SELECT {NOTIFIER_COLS} FROM _forgebase_alert_notifiers WHERE id = ?1 OR name = ?1"),
            vec![id_or_name.into()],
        )
        .await?;
        Row::all(&rows)
            .next()
            .map(|r| NotifierRow::from_row(&r))
            .ok_or_else(|| AlertError::not_found("notifier_not_found", format!("no notifier {id_or_name:?}")))
    }

    pub(crate) fn aad(id: &str) -> Vec<u8> {
        format!("alerts:notifier:{id}").into_bytes()
    }

    pub(crate) fn open_secret(&self, n: &NotifierRow) -> Result<Option<String>> {
        let Some(sealed) = &n.secret else { return Ok(None) };
        let plain = self
            .inner
            .deps
            .keyring
            .open(sealed, &Self::aad(&n.id))
            .map_err(|e| AlertError::Internal(format!("notifier secret of {}: {e}", n.name)))?;
        String::from_utf8(plain)
            .map(Some)
            .map_err(|e| AlertError::Internal(e.to_string()))
    }

    fn seal(&self, id: &str, plain: &str) -> Result<Vec<u8>> {
        self.inner
            .deps
            .keyring
            .seal(plain.as_bytes(), &Self::aad(id))
            .map_err(|e| AlertError::Internal(e.to_string()))
    }

    /// Validates an input; returns `(config JSON, new secret, secret shown to the caller)`.
    fn notifier_config(
        &self,
        input: &NotifierInput,
        existing: Option<&NotifierRow>,
    ) -> Result<(Json, Option<String>, Option<String>)> {
        rules::check_name("name", &input.name, 64)
            .map_err(|e| AlertError::invalid("invalid_notifier", e.to_string()))?;
        compile_matchers(&input.matchers).map_err(|e| AlertError::invalid("invalid_notifier", e.to_string()))?;
        if input.matchers.len() > 16 {
            return Err(AlertError::invalid("invalid_notifier", "at most 16 matchers"));
        }
        let repeat_ms = match &input.repeat_interval {
            None => existing.map_or(4 * 3_600_000, |e| e.repeat_ms),
            Some(r) => expr::parse_duration(r)
                .filter(|d| (60_000..=7 * 86_400_000).contains(d))
                .ok_or_else(|| {
                    AlertError::invalid("invalid_notifier", "repeat_interval is from 1m to 7d, such as 4h")
                })?,
        };
        let group_by = input.group_by.clone().unwrap_or_else(|| vec!["alertname".into()]);
        if group_by.len() > 16 {
            return Err(AlertError::invalid(
                "invalid_notifier",
                "group_by names at most 16 labels",
            ));
        }
        let mut cfg = json!({
            "matchers": input.matchers,
            "min_severity": input.min_severity.unwrap_or(Severity::Info).as_str(),
            "group_by": group_by,
            "repeat_interval_ms": repeat_ms,
        });
        let allow_http = self.settings().allow_http;
        let same_kind = existing.filter(|e| e.kind == input.kind);
        let mut secret = None;
        let mut shown = None;
        match input.kind {
            NotifierKind::Webhook => {
                let url = input
                    .url
                    .as_deref()
                    .ok_or_else(|| AlertError::invalid("invalid_notifier", "a webhook notifier needs url"))?;
                cfg["url"] = json!(check_url(url, allow_http, "url")?);
                if same_kind.is_none() {
                    let s = format!(
                        "whsec_{}",
                        forgebase_auth::crypto::random_token(32).map_err(|e| AlertError::Internal(e.to_string()))?
                    );
                    shown = Some(s.clone());
                    secret = Some(s);
                }
            }
            NotifierKind::Slack => match input.url.as_deref() {
                Some(url) => {
                    let url = check_url(url, allow_http, "url")?;
                    cfg["url"] = json!(url::Url::parse(&url).ok().and_then(|u| u.host_str().map(str::to_owned)));
                    secret = Some(url);
                }
                None if same_kind.is_some() => cfg["url"] = json!(same_kind.and_then(|e| e.url.clone())),
                None => {
                    return Err(AlertError::invalid(
                        "invalid_notifier",
                        "a Slack notifier needs its webhook url",
                    ));
                }
            },
            NotifierKind::Pagerduty => {
                let url = input.url.as_deref().unwrap_or(PAGERDUTY_EVENTS_URL);
                cfg["url"] = json!(check_url(url, allow_http, "url")?);
                match input.routing_key.as_deref().map(str::trim) {
                    Some(k) if !k.is_empty() && k.len() <= 128 => secret = Some(k.to_owned()),
                    Some(_) => {
                        return Err(AlertError::invalid(
                            "invalid_notifier",
                            "routing_key is 1 to 128 characters",
                        ));
                    }
                    None if same_kind.is_some() => {}
                    None => {
                        return Err(AlertError::invalid(
                            "invalid_notifier",
                            "a PagerDuty notifier needs routing_key (the integration key)",
                        ));
                    }
                }
            }
            NotifierKind::Email => {
                if input.to.is_empty() || input.to.len() > 20 {
                    return Err(AlertError::invalid(
                        "invalid_notifier",
                        "an email notifier needs 1 to 20 recipients in to",
                    ));
                }
                for a in &input.to {
                    if !valid_email(a) {
                        return Err(AlertError::invalid(
                            "invalid_notifier",
                            format!("{a:?} is not an email address"),
                        ));
                    }
                }
                cfg["to"] = json!(input.to);
            }
            NotifierKind::Queue => {
                let topic = input
                    .topic
                    .as_deref()
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .ok_or_else(|| AlertError::invalid("invalid_notifier", "a queue notifier needs topic"))?;
                cfg["topic"] = json!(topic);
            }
        }
        Ok((cfg, secret, shown))
    }

    /// Notifiers.
    pub async fn notifiers(&self, ctx: &AccessContext) -> Result<Vec<Notifier>> {
        self.require_admin(ctx)?;
        Ok(self.notifier_rows().await?.iter().map(|n| n.view(None)).collect())
    }

    /// One notifier (by id or name).
    pub async fn notifier(&self, ctx: &AccessContext, id: &str) -> Result<Notifier> {
        self.require_admin(ctx)?;
        Ok(self.notifier_row(id).await?.view(None))
    }

    /// Creates a notifier; a webhook's signing secret is in the answer only.
    pub async fn create_notifier(&self, ctx: &AccessContext, input: NotifierInput) -> Result<Notifier> {
        self.require_admin(ctx)?;
        let (cfg, secret, shown) = self.notifier_config(&input, None)?;
        if self.notifier_row(&input.name).await.is_ok() {
            return Err(AlertError::conflict(
                "notifier_exists",
                format!("a notifier named {} exists", input.name),
            ));
        }
        let id = new_id("ntf");
        let sealed = secret.as_deref().map(|s| self.seal(&id, s)).transpose()?;
        let now = self.now();
        write(
            self.db(),
            vec![Statement::new(
                "INSERT INTO _forgebase_alert_notifiers (id, name, kind, config, secret, enabled, send_resolved,
                     created_by, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
                vec![
                    id.as_str().into(),
                    input.name.as_str().into(),
                    input.kind.as_str().into(),
                    to_json(&cfg)?.into(),
                    sealed.map_or(Value::Null, Value::Blob),
                    i64::from(input.enabled.unwrap_or(true)).into(),
                    i64::from(input.send_resolved.unwrap_or(true)).into(),
                    ctx.principal_key().into(),
                    now.into(),
                ],
            )],
        )
        .await?;
        Ok(self.notifier_row(&id).await?.view(shown))
    }

    /// Replaces a notifier's settings (a secret not given is kept; a new
    /// kind needs its secret again).
    pub async fn update_notifier(&self, ctx: &AccessContext, id: &str, input: NotifierInput) -> Result<Notifier> {
        self.require_admin(ctx)?;
        let cur = self.notifier_row(id).await?;
        let (cfg, secret, shown) = self.notifier_config(&input, Some(&cur))?;
        if input.name != cur.name && self.notifier_row(&input.name).await.is_ok() {
            return Err(AlertError::conflict(
                "notifier_exists",
                format!("a notifier named {} exists", input.name),
            ));
        }
        let sealed = match secret {
            Some(s) => Some(self.seal(&cur.id, &s)?),
            None if cur.kind == input.kind => cur.secret.clone(),
            None => None,
        };
        write(
            self.db(),
            vec![Statement::new(
                "UPDATE _forgebase_alert_notifiers SET name = ?2, kind = ?3, config = ?4, secret = ?5, enabled = ?6,
                     send_resolved = ?7, updated_at = ?8 WHERE id = ?1",
                vec![
                    cur.id.as_str().into(),
                    input.name.as_str().into(),
                    input.kind.as_str().into(),
                    to_json(&cfg)?.into(),
                    sealed.map_or(Value::Null, Value::Blob),
                    i64::from(input.enabled.unwrap_or(cur.enabled)).into(),
                    i64::from(input.send_resolved.unwrap_or(cur.send_resolved)).into(),
                    self.now().into(),
                ],
            )],
        )
        .await?;
        Ok(self.notifier_row(&cur.id).await?.view(shown))
    }

    /// Deletes a notifier with its pending deliveries.
    pub async fn delete_notifier(&self, ctx: &AccessContext, id: &str) -> Result<()> {
        self.require_admin(ctx)?;
        let cur = self.notifier_row(id).await?;
        write(
            self.db(),
            vec![
                Statement::new(
                    "DELETE FROM _forgebase_alert_notifications WHERE notifier_id = ?1",
                    vec![cur.id.as_str().into()],
                ),
                Statement::new(
                    "DELETE FROM _forgebase_alert_groups WHERE notifier_id = ?1",
                    vec![cur.id.as_str().into()],
                ),
                Statement::new(
                    "DELETE FROM _forgebase_alert_notifiers WHERE id = ?1",
                    vec![cur.id.as_str().into()],
                ),
            ],
        )
        .await
    }

    /// Deliveries, newest first (of one notifier, or all; `status` filter).
    pub async fn deliveries(
        &self,
        ctx: &AccessContext,
        notifier: Option<&str>,
        status: Option<&str>,
        limit: Option<u32>,
    ) -> Result<DeliveryPage> {
        self.require_admin(ctx)?;
        let limit = i64::from(limit.unwrap_or(100).clamp(1, 500));
        let notifier_id = match notifier {
            Some(n) => Some(self.notifier_row(n).await?.id),
            None => None,
        };
        let mut sql = format!("SELECT {DELIVERY_COLS} FROM _forgebase_alert_notifications WHERE 1 = 1");
        let mut params: Vec<Value> = Vec::new();
        if let Some(n) = notifier_id {
            params.push(n.into());
            sql.push_str(&format!(" AND notifier_id = ?{}", params.len()));
        }
        if let Some(s) = status {
            params.push(s.into());
            sql.push_str(&format!(" AND status = ?{}", params.len()));
        }
        params.push(limit.into());
        sql.push_str(&format!(" ORDER BY created_at DESC, id DESC LIMIT ?{}", params.len()));
        let rows = read(self.db(), &sql, params).await?;
        Ok(DeliveryPage {
            items: Row::all(&rows).map(|r| delivery_from(&r)).collect(),
        })
    }

    // -------------------------------------------------------------- SLOs

    pub(crate) async fn slos_unchecked(&self) -> Result<Vec<Slo>> {
        let rows = read(
            self.db(),
            &format!("SELECT {SLO_COLS} FROM _forgebase_slos ORDER BY name"),
            vec![],
        )
        .await?;
        Ok(Row::all(&rows).map(|r| slo_from(&r)).collect())
    }

    pub(crate) async fn reload_slos(&self) -> Result<()> {
        let slos = self.slos_unchecked().await?;
        self.inner.slo.set_slos(&slos);
        Ok(())
    }

    async fn slo_row(&self, id_or_name: &str) -> Result<Slo> {
        let rows = read(
            self.db(),
            &format!("SELECT {SLO_COLS} FROM _forgebase_slos WHERE id = ?1 OR name = ?1"),
            vec![id_or_name.into()],
        )
        .await?;
        Row::all(&rows)
            .next()
            .map(|r| slo_from(&r))
            .ok_or_else(|| AlertError::not_found("slo_not_found", format!("no SLO {id_or_name:?}")))
    }

    fn check_slo(input: &SloInput) -> Result<i64> {
        rules::check_name("name", &input.name, 64).map_err(|e| AlertError::invalid("invalid_slo", e.to_string()))?;
        if !(0.5..1.0).contains(&input.objective) || input.objective > 0.99999 {
            return Err(AlertError::invalid(
                "invalid_slo",
                "objective is a share from 0.5 to 0.99999, such as 0.999",
            ));
        }
        let window = match &input.window {
            None => 30 * 86_400_000,
            Some(w) => expr::parse_duration(w)
                .filter(|w| (3_600_000..=90 * 86_400_000).contains(w))
                .ok_or_else(|| AlertError::invalid("invalid_slo", "window is from 1h to 90d, such as 30d"))?,
        };
        if input.route_prefix.is_some() && input.route_regex.is_some() {
            return Err(AlertError::invalid(
                "invalid_slo",
                "give route_prefix or route_regex, not both",
            ));
        }
        if let Some(p) = &input.route_prefix
            && !p.starts_with('/')
        {
            return Err(AlertError::invalid(
                "invalid_slo",
                "route_prefix is the start of a route template, such as /api/v1/collections",
            ));
        }
        if let Some(r) = &input.route_regex {
            regex::Regex::new(&format!("^(?:{r})$"))
                .map_err(|e| AlertError::invalid("invalid_slo", format!("route_regex: {e}")))?;
        }
        for m in &input.methods {
            if m.is_empty() || m.len() > 16 || !m.bytes().all(|b| b.is_ascii_uppercase()) {
                return Err(AlertError::invalid(
                    "invalid_slo",
                    "methods are upper case, such as GET",
                ));
            }
        }
        match (input.kind, input.latency_threshold_ms) {
            (SloKind::Latency, Some(t)) if (1..=600_000).contains(&t) => {}
            (SloKind::Latency, _) => {
                return Err(AlertError::invalid(
                    "invalid_slo",
                    "a latency SLO needs latency_threshold_ms from 1 to 600000, such as 300",
                ));
            }
            (SloKind::Availability, Some(_)) => {
                return Err(AlertError::invalid(
                    "invalid_slo",
                    "latency_threshold_ms is for latency SLOs",
                ));
            }
            (SloKind::Availability, None) => {}
        }
        for reserved in ["slo", "tier", "alertname", "severity"] {
            if input.labels.contains_key(reserved) {
                return Err(AlertError::invalid(
                    "invalid_slo",
                    format!("the label {reserved} is set by the SLO"),
                ));
            }
        }
        Ok(window)
    }

    /// SLOs.
    pub async fn slos(&self, ctx: &AccessContext) -> Result<Vec<Slo>> {
        self.require_admin(ctx)?;
        self.slos_unchecked().await
    }

    /// One SLO (by id or name).
    pub async fn slo(&self, ctx: &AccessContext, id: &str) -> Result<Slo> {
        self.require_admin(ctx)?;
        self.slo_row(id).await
    }

    /// Creates an SLO. Counting starts now (no history before).
    pub async fn create_slo(&self, ctx: &AccessContext, input: SloInput) -> Result<Slo> {
        self.require_admin(ctx)?;
        let window = Self::check_slo(&input)?;
        if self.slo_row(&input.name).await.is_ok() {
            return Err(AlertError::conflict(
                "slo_exists",
                format!("an SLO named {} exists", input.name),
            ));
        }
        let id = new_id("slo");
        let now = self.now();
        write(
            self.db(),
            vec![Statement::new(
                format!(
                    "INSERT INTO _forgebase_slos ({SLO_COLS}, created_by) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13, ?14)"
                ),
                vec![
                    id.as_str().into(),
                    input.name.as_str().into(),
                    input.description.as_str().into(),
                    input.kind.as_str().into(),
                    input.objective.into(),
                    window.into(),
                    input.route_prefix.clone().into(),
                    input.route_regex.clone().into(),
                    to_json(&input.methods)?.into(),
                    input.latency_threshold_ms.into(),
                    i64::from(input.alerting.unwrap_or(true)).into(),
                    to_json(&input.labels)?.into(),
                    now.into(),
                    ctx.principal_key().into(),
                ],
            )],
        )
        .await?;
        self.reload_slos().await?;
        self.wake();
        self.slo_row(&id).await
    }

    /// Replaces an SLO's definition (counts already gathered stay).
    pub async fn update_slo(&self, ctx: &AccessContext, id: &str, input: SloInput) -> Result<Slo> {
        self.require_admin(ctx)?;
        let cur = self.slo_row(id).await?;
        let window = Self::check_slo(&input)?;
        if input.name != cur.name && self.slo_row(&input.name).await.is_ok() {
            return Err(AlertError::conflict(
                "slo_exists",
                format!("an SLO named {} exists", input.name),
            ));
        }
        write(
            self.db(),
            vec![Statement::new(
                "UPDATE _forgebase_slos SET name = ?2, description = ?3, kind = ?4, objective = ?5, window_ms = ?6,
                     route_prefix = ?7, route_regex = ?8, methods = ?9, latency_threshold_ms = ?10, alerting = ?11,
                     labels = ?12, updated_at = ?13 WHERE id = ?1",
                vec![
                    cur.id.as_str().into(),
                    input.name.as_str().into(),
                    input.description.as_str().into(),
                    input.kind.as_str().into(),
                    input.objective.into(),
                    window.into(),
                    input.route_prefix.clone().into(),
                    input.route_regex.clone().into(),
                    to_json(&input.methods)?.into(),
                    input.latency_threshold_ms.into(),
                    i64::from(input.alerting.unwrap_or(cur.alerting)).into(),
                    to_json(&input.labels)?.into(),
                    self.now().into(),
                ],
            )],
        )
        .await?;
        self.reload_slos().await?;
        self.wake();
        self.slo_row(&cur.id).await
    }

    /// Deletes an SLO and its counts.
    pub async fn delete_slo(&self, ctx: &AccessContext, id: &str) -> Result<()> {
        self.require_admin(ctx)?;
        let cur = self.slo_row(id).await?;
        write(
            self.db(),
            vec![
                Statement::new(
                    "DELETE FROM _forgebase_slo_minutes WHERE slo_id = ?1",
                    vec![cur.id.as_str().into()],
                ),
                Statement::new(
                    "DELETE FROM _forgebase_slos WHERE id = ?1",
                    vec![cur.id.as_str().into()],
                ),
            ],
        )
        .await?;
        self.reload_slos().await?;
        self.wake();
        Ok(())
    }

    /// Where an SLO stands now: totals, SLI, error budget, burn rates and
    /// the burn-rate alert tiers.
    pub async fn slo_status(&self, ctx: &AccessContext, id: &str) -> Result<SloStatus> {
        self.require_admin(ctx)?;
        let slo = self.slo_row(id).await?;
        self.status_of(slo).await
    }

    /// The status of every SLO.
    pub async fn slo_statuses(&self, ctx: &AccessContext) -> Result<Vec<SloStatus>> {
        self.require_admin(ctx)?;
        let mut out = Vec::new();
        for s in self.slos_unchecked().await? {
            out.push(self.status_of(s).await?);
        }
        Ok(out)
    }

    pub(crate) async fn status_of(&self, slo: Slo) -> Result<SloStatus> {
        let t = crate::slo::totals(self.db(), &self.inner.slo, &slo, self.now()).await?;
        let (total, good) = t.window;
        let burn_rates: BTreeMap<String, Option<f64>> = t
            .burn
            .iter()
            .map(|(k, v)| (k.clone(), crate::slo::burn_rate(slo.objective, *v)))
            .collect();
        let open = read(
            self.db(),
            "SELECT rule_id FROM _forgebase_alerts WHERE state = 'firing' AND rule_id LIKE ?1",
            vec![format!("slo:{}:%", slo.name).into()],
        )
        .await?;
        let firing: BTreeSet<String> = Row::all(&open).map(|r| r.text("rule_id")).collect();
        let alerts = rules::TIERS
            .iter()
            .filter(|(_, long, ..)| expr::parse_duration(long).unwrap_or(0) <= slo.window_ms)
            .map(|(tier, long, short, share, severity)| {
                let rule_id = format!("slo:{}:{tier}", slo.name);
                BurnTier {
                    firing: firing.contains(&rule_id),
                    rule_id,
                    tier: (*tier).to_owned(),
                    long_window: (*long).to_owned(),
                    short_window: (*short).to_owned(),
                    burn_rate_threshold: rules::tier_threshold(
                        slo.window_ms,
                        expr::parse_duration(long).unwrap_or(1),
                        *share,
                    ),
                    severity: *severity,
                }
            })
            .collect();
        Ok(SloStatus {
            total,
            good,
            bad: (total - good).max(0),
            sli: (total > 0).then(|| good as f64 / total as f64),
            error_budget: crate::slo::budget(slo.objective, (total, good)),
            burn_rates,
            alerts,
            slo,
        })
    }
}

/// A plausible email address (`local@domain.tld`, no spaces).
fn valid_email(a: &str) -> bool {
    let Some((local, domain)) = a.split_once('@') else {
        return false;
    };
    a.len() <= 254
        && !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !domain.contains('@')
        && !a
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '<' | '>' | ',' | ';'))
}

/// Ids of the silences matching `labels`.
pub(crate) fn silenced_by(silences: &[(String, Vec<Matcher>)], labels: &BTreeMap<String, String>) -> Vec<String> {
    silences
        .iter()
        .filter(|(_, ms)| ms.iter().all(|m| m.matches(labels)))
        .map(|(id, _)| id.clone())
        .collect()
}
