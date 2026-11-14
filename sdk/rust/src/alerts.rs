//! Alerts and service level objectives (`/api/v1/alerts`, `/api/v1/slos`, ADR-0035):
//! rules, alerts and their history, silences, notifiers (with a test send), notification
//! deliveries, expression queries, SLOs and their status. `client.alerts()` and `client.slos()`.
//!
//! Platform administrators only. `[alerts] enabled = false` turns both prefixes into
//! `404 feature_disabled`. Request and response types are the generated wire models
//! ([`generated`](crate::generated)); rules, silences and notifiers can be addressed by id or name.

use std::sync::Arc;

use serde_json::json;

use crate::error::Result;
use crate::generated::{
    Alert, AlertDelivery, AlertDeliveryPage, AlertDetail, AlertNotifier, AlertNotifierInput, AlertPage,
    AlertQueryInput, AlertQueryResult, AlertRule, AlertRuleInput, AlertRulePatch, AlertSilence, AlertSilenceInput, Slo,
    SloInput, SloStatus,
};
use crate::http::{Req, Transport, seg};

/// Filters of [`AlertsApi::history`].
#[derive(Debug, Clone, Default)]
pub struct AlertHistoryQuery {
    /// `pending`, `firing` or `resolved`.
    pub state: Option<String>,
    /// Only this rule's alerts.
    pub rule_id: Option<String>,
    /// Active at or after (RFC 3339).
    pub from: Option<String>,
    /// Active before (RFC 3339).
    pub to: Option<String>,
    /// The `next_before` of the previous page.
    pub before: Option<String>,
    /// 1-500, default 100.
    pub limit: Option<u32>,
}

/// Filters of [`AlertsApi::notifications`].
#[derive(Debug, Clone, Default)]
pub struct NotificationsQuery {
    /// Notifier id or name.
    pub notifier: Option<String>,
    /// `pending`, `sent` or `failed`.
    pub status: Option<String>,
    /// Page size.
    pub limit: Option<u32>,
}

/// Alerting: `client.alerts()`.
#[derive(Clone)]
pub struct AlertsApi {
    pub(crate) t: Arc<Transport>,
}

const A: &str = "/api/v1/alerts";

impl AlertsApi {
    /// Alert rules.
    #[must_use]
    pub fn rules(&self) -> AlertRulesApi {
        AlertRulesApi { t: self.t.clone() }
    }
    /// Silences.
    #[must_use]
    pub fn silences(&self) -> AlertSilencesApi {
        AlertSilencesApi { t: self.t.clone() }
    }
    /// Notifiers.
    #[must_use]
    pub fn notifiers(&self) -> AlertNotifiersApi {
        AlertNotifiersApi { t: self.t.clone() }
    }

    /// Open alerts (`pending`, `firing`), most severe first, optionally filtered by `state` and `severity`.
    pub async fn active(&self, state: Option<&str>, severity: Option<&str>) -> Result<Vec<Alert>> {
        self.t
            .json(Req::get(A).q_opt("state", state).q_opt("severity", severity))
            .await
    }

    /// Every alert, newest first, resolved ones included.
    pub async fn history(&self, q: &AlertHistoryQuery) -> Result<AlertPage> {
        self.t
            .json(
                Req::get(format!("{A}/history"))
                    .q_opt("state", q.state.as_deref())
                    .q_opt("rule_id", q.rule_id.as_deref())
                    .q_opt("from", q.from.as_deref())
                    .q_opt("to", q.to.as_deref())
                    .q_opt("before", q.before.as_deref())
                    .q_opt("limit", q.limit),
            )
            .await
    }

    /// One alert with its state changes (`events`).
    pub async fn get(&self, id: &str) -> Result<AlertDetail> {
        self.t.json(Req::get(format!("{A}/{}", seg(id)))).await
    }

    /// Evaluates an expression now and returns its samples.
    pub async fn query(&self, expr: &str) -> Result<AlertQueryResult> {
        let body = AlertQueryInput { expr: expr.to_owned() };
        self.t
            .json(Req::post(format!("{A}/query")).json(&body)?.idempotent())
            .await
    }

    /// Notification deliveries, newest first.
    pub async fn notifications(&self, q: &NotificationsQuery) -> Result<AlertDeliveryPage> {
        self.t
            .json(
                Req::get(format!("{A}/notifications"))
                    .q_opt("notifier", q.notifier.as_deref())
                    .q_opt("status", q.status.as_deref())
                    .q_opt("limit", q.limit),
            )
            .await
    }
}

/// Alert rules: `client.alerts().rules()`.
#[derive(Clone)]
pub struct AlertRulesApi {
    t: Arc<Transport>,
}

impl AlertRulesApi {
    /// Every rule (built-in, `[[alerts.rules]]`, custom, SLO burn-rate) with `enabled`,
    /// `disabled_by_config` and `source`.
    pub async fn list(&self) -> Result<Vec<AlertRule>> {
        self.t.json(Req::get(format!("{A}/rules"))).await
    }

    /// Creates a custom rule (`name`, `expr`, `op`, `threshold`, `for`, `severity`, labels, annotations).
    pub async fn create(&self, input: &AlertRuleInput) -> Result<AlertRule> {
        self.t.json(Req::post(format!("{A}/rules")).json(input)?).await
    }

    /// One rule.
    pub async fn get(&self, id: &str) -> Result<AlertRule> {
        self.t.json(Req::get(format!("{A}/rules/{}", seg(id)))).await
    }

    /// Changes a rule (built-in, file and SLO rules: `enabled` only, else `409 rule_read_only`).
    pub async fn update(&self, id: &str, patch: &AlertRulePatch) -> Result<AlertRule> {
        self.t
            .json(Req::patch(format!("{A}/rules/{}", seg(id))).json(patch)?)
            .await
    }

    /// Deletes a custom rule.
    pub async fn delete(&self, id: &str) -> Result<()> {
        self.t.unit(Req::delete(format!("{A}/rules/{}", seg(id)))).await
    }
}

/// Silences: `client.alerts().silences()`.
#[derive(Clone)]
pub struct AlertSilencesApi {
    t: Arc<Transport>,
}

impl AlertSilencesApi {
    /// Active silences (`all`: with expired ones).
    pub async fn list(&self, all: bool) -> Result<Vec<AlertSilence>> {
        let r = Req::get(format!("{A}/silences"));
        self.t.json(if all { r.q("all", "true") } else { r }).await
    }

    /// Creates a silence: matching alerts send no notifications until it ends. `matchers`
    /// (`label`, `op` `=` `!=` `=~` `!~`, `value`), `starts_at`, `ends_at` or `duration`, `comment`.
    pub async fn create(&self, input: &AlertSilenceInput) -> Result<AlertSilence> {
        self.t.json(Req::post(format!("{A}/silences")).json(input)?).await
    }

    /// One silence.
    pub async fn get(&self, id: &str) -> Result<AlertSilence> {
        self.t.json(Req::get(format!("{A}/silences/{}", seg(id)))).await
    }

    /// Replaces a silence.
    pub async fn update(&self, id: &str, input: &AlertSilenceInput) -> Result<AlertSilence> {
        self.t
            .json(Req::put(format!("{A}/silences/{}", seg(id))).json(input)?)
            .await
    }

    /// Ends a silence now.
    pub async fn expire(&self, id: &str) -> Result<AlertSilence> {
        self.t.json(Req::delete(format!("{A}/silences/{}", seg(id)))).await
    }
}

/// Notifiers (`webhook`, `slack`, `email`, `pagerduty`, `queue`): `client.alerts().notifiers()`.
/// Secrets are never listed; a webhook's `signing_secret` is in the create answer only.
#[derive(Clone)]
pub struct AlertNotifiersApi {
    t: Arc<Transport>,
}

impl AlertNotifiersApi {
    /// Every notifier.
    pub async fn list(&self) -> Result<Vec<AlertNotifier>> {
        self.t.json(Req::get(format!("{A}/notifiers"))).await
    }

    /// Creates a notifier.
    pub async fn create(&self, input: &AlertNotifierInput) -> Result<AlertNotifier> {
        self.t.json(Req::post(format!("{A}/notifiers")).json(input)?).await
    }

    /// One notifier (by id or name).
    pub async fn get(&self, id: &str) -> Result<AlertNotifier> {
        self.t.json(Req::get(format!("{A}/notifiers/{}", seg(id)))).await
    }

    /// Replaces a notifier.
    pub async fn update(&self, id: &str, input: &AlertNotifierInput) -> Result<AlertNotifier> {
        self.t
            .json(Req::put(format!("{A}/notifiers/{}", seg(id))).json(input)?)
            .await
    }

    /// Deletes a notifier.
    pub async fn delete(&self, id: &str) -> Result<()> {
        self.t.unit(Req::delete(format!("{A}/notifiers/{}", seg(id)))).await
    }

    /// Sends a test notification now (10 per notifier and minute, then `429 rate_limited`) and
    /// returns its delivery.
    pub async fn test(&self, id: &str) -> Result<AlertDelivery> {
        self.t
            .json(Req::post(format!("{A}/notifiers/{}/test", seg(id))).json_raw(json!({})))
            .await
    }
}

/// Service level objectives: `client.slos()`.
#[derive(Clone)]
pub struct SlosApi {
    pub(crate) t: Arc<Transport>,
}

const S: &str = "/api/v1/slos";

impl SlosApi {
    /// Every SLO.
    pub async fn list(&self) -> Result<Vec<Slo>> {
        self.t.json(Req::get(S)).await
    }

    /// Creates an SLO (counting starts now): `kind` `availability` or `latency`, `objective`,
    /// `window`, `route_prefix` or `route_regex`, `methods`, `latency_threshold_ms`, `alerting`, `labels`.
    pub async fn create(&self, input: &SloInput) -> Result<Slo> {
        self.t.json(Req::post(S).json(input)?).await
    }

    /// One SLO (by id or name).
    pub async fn get(&self, id: &str) -> Result<Slo> {
        self.t.json(Req::get(format!("{S}/{}", seg(id)))).await
    }

    /// Replaces an SLO.
    pub async fn update(&self, id: &str, input: &SloInput) -> Result<Slo> {
        self.t.json(Req::put(format!("{S}/{}", seg(id))).json(input)?).await
    }

    /// Deletes an SLO.
    pub async fn delete(&self, id: &str) -> Result<()> {
        self.t.unit(Req::delete(format!("{S}/{}", seg(id)))).await
    }

    /// Totals, SLI, `error_budget` (`allowed`, `consumed`, `remaining_ratio`), `burn_rates` per
    /// window and the burn-rate tiers of one SLO.
    pub async fn status(&self, id: &str) -> Result<SloStatus> {
        self.t.json(Req::get(format!("{S}/{}/status", seg(id)))).await
    }

    /// The status of every SLO.
    pub async fn statuses(&self) -> Result<Vec<SloStatus>> {
        self.t.json(Req::get(format!("{S}/status"))).await
    }
}
