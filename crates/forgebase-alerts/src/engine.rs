//! The evaluation round (ADR-0035 §3, §4): snapshot (metrics and
//! signals), rule evaluation, the pending → firing → resolved state
//! machine, notification grouping into the delivery outbox, delivery with
//! retries, pruning, and the background task.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::time::Duration;

use forgebase_data::AccessContext;
use forgebase_db::{Statement, Value};
use forgebase_observability::names;
use serde_json::{Value as Json, json};

use crate::error::{AlertError, Result};
use crate::expr;
use crate::model::*;
use crate::rules::{self, RuleDef};
use crate::samples::Snapshot;
use crate::service::{
    ALERT_COLS, AlertService, DELIVERY_COLS, NotifierRow, alert_from, compile_matchers, delivery_from, silenced_by,
};
use crate::store::{Row, new_id, read, to_json, write};

/// What one round did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoundReport {
    /// Rules evaluated.
    pub rules: usize,
    /// Rules skipped because a signal they read could not be collected.
    pub skipped: usize,
    /// Alerts now pending.
    pub pending: usize,
    /// Alerts now firing.
    pub firing: usize,
    /// Alerts that resolved this round.
    pub resolved: usize,
    /// Notifications queued this round.
    pub notifications: usize,
}

struct OpenRow {
    id: String,
    rule_id: String,
    state: AlertState,
    active_at: i64,
}

const QUEUE_SIGNALS: [&str; 5] = [
    "forgebase:queue_backlog",
    "forgebase:queue_oldest_unacked_seconds",
    "forgebase:queue_dead_lettered_total",
    "forgebase:queue_dead_letters",
    "forgebase:queue_push_paused",
];

impl AlertService {
    /// Runs the next round now instead of after the interval.
    pub fn wake(&self) {
        self.inner.wake.notify_one();
    }

    /// The current values: this process's Prometheus metrics and the
    /// built-in signals the rules read. Returns the names whose signal
    /// could not be collected (rules reading them are skipped, so their
    /// alerts neither fire nor resolve on missing data).
    pub(crate) async fn snapshot(&self, names: &BTreeSet<String>, slo: bool) -> (Snapshot, BTreeSet<String>) {
        let mut snap = Snapshot::new(self.now());
        let mut failed = BTreeSet::new();
        if names.iter().any(|n| !n.starts_with("forgebase:")) {
            snap.add_exposition(&self.inner.deps.metrics.render());
        }
        let want = |prefix: &str| names.iter().any(|n| n.starts_with(prefix));
        if want("forgebase:queue_")
            && let Err(e) = self.queue_signals(&mut snap).await
        {
            tracing::debug!(error = %e, "queue signals not collected");
            failed.extend(QUEUE_SIGNALS.iter().map(|s| (*s).to_owned()));
        }
        if want("forgebase:data_dir_")
            && let Err(e) = self.disk_signals(&mut snap).await
        {
            tracing::debug!(error = %e, "disk signals not collected");
            failed.extend(
                [
                    "forgebase:data_dir_free_bytes",
                    "forgebase:data_dir_total_bytes",
                    "forgebase:data_dir_free_ratio",
                ]
                .map(str::to_owned),
            );
        }
        if want("forgebase:backup_")
            && let Err(e) = self.backup_signals(&mut snap).await
        {
            tracing::debug!(error = %e, "backup signals not collected");
            failed.extend(
                [
                    "forgebase:backup_last_success_timestamp_seconds",
                    "forgebase:backup_last_success_age_seconds",
                ]
                .map(str::to_owned),
            );
        }
        if slo
            && want("forgebase:slo_")
            && let Err(e) = self.slo_signals(&mut snap, false).await
        {
            tracing::debug!(error = %e, "SLO signals not collected");
            failed.extend(
                [
                    "forgebase:slo_burn_rate",
                    "forgebase:slo_error_budget_remaining",
                    "forgebase:slo_sli",
                ]
                .map(str::to_owned),
            );
        }
        (snap, failed)
    }

    async fn queue_signals(&self, snap: &mut Snapshot) -> Result<()> {
        let Some(q) = &self.inner.deps.queue else {
            return Ok(());
        };
        let sys = AccessContext::system();
        let subs = q
            .subscriptions(&sys, None)
            .await
            .map_err(|e| AlertError::Internal(e.to_string()))?;
        for s in subs.iter().take(1000) {
            let m = match q.subscription_metrics(&sys, &s.name).await {
                Ok(m) => m,
                // Deleted between the list and the read.
                Err(_) => continue,
            };
            let l = [("subscription", s.name.as_str()), ("topic", s.topic.as_str())];
            snap.push("forgebase:queue_backlog", &l, m.backlog_messages as f64);
            if let Some(age) = m.oldest_unacked_age_ms {
                snap.push("forgebase:queue_oldest_unacked_seconds", &l, age as f64 / 1000.0);
            }
            snap.push("forgebase:queue_dead_lettered_total", &l, m.dead_lettered_total as f64);
            snap.push("forgebase:queue_dead_letters", &l, m.dead_letters as f64);
            if s.push.is_some() {
                snap.push(
                    "forgebase:queue_push_paused",
                    &l,
                    f64::from(u8::from(s.paused_at.is_some())),
                );
            }
        }
        Ok(())
    }

    async fn disk_signals(&self, snap: &mut Snapshot) -> Result<()> {
        let Some(layout) = &self.inner.deps.layout else {
            return Ok(());
        };
        let root = layout.root().to_path_buf();
        let (free, total) = tokio::task::spawn_blocking(move || -> std::io::Result<(u64, u64)> {
            Ok((fs4::available_space(&root)?, fs4::total_space(&root)?))
        })
        .await
        .map_err(|e| AlertError::Internal(e.to_string()))?
        .map_err(|e| AlertError::Internal(format!("disk space of the data directory: {e}")))?;
        snap.push("forgebase:data_dir_free_bytes", &[], free as f64);
        snap.push("forgebase:data_dir_total_bytes", &[], total as f64);
        if total > 0 {
            snap.push("forgebase:data_dir_free_ratio", &[], free as f64 / total as f64);
        }
        Ok(())
    }

    async fn backup_signals(&self, snap: &mut Snapshot) -> Result<()> {
        let Some(layout) = &self.inner.deps.layout else {
            return Ok(());
        };
        let store = forgebase_backup::SnapshotStore::new(layout);
        let list = tokio::task::spawn_blocking(move || store.list())
            .await
            .map_err(|e| AlertError::Internal(e.to_string()))?
            .map_err(|e| AlertError::Internal(e.to_string()))?;
        let last = list.iter().filter_map(|s| parse_iso(&s.manifest.created)).max();
        if let Some(at) = last {
            snap.push(
                "forgebase:backup_last_success_timestamp_seconds",
                &[],
                at as f64 / 1000.0,
            );
            snap.push(
                "forgebase:backup_last_success_age_seconds",
                &[],
                (snap.at_ms - at).max(0) as f64 / 1000.0,
            );
        }
        Ok(())
    }

    /// SLO samples, and (`gauges`) the Prometheus gauges.
    async fn slo_signals(&self, snap: &mut Snapshot, gauges: bool) -> Result<()> {
        for slo in self.slos_unchecked().await? {
            let st = self.status_of(slo).await?;
            let name = st.slo.name.clone();
            let l = [("slo", name.as_str())];
            for (w, v) in &st.burn_rates {
                if let Some(v) = v {
                    snap.push(
                        "forgebase:slo_burn_rate",
                        &[("slo", name.as_str()), ("window", w.as_str())],
                        *v,
                    );
                }
                if gauges {
                    metrics::gauge!(names::SLO_BURN_RATE, "slo" => name.clone(), "window" => w.clone())
                        .set(v.unwrap_or(0.0));
                }
            }
            snap.push(
                "forgebase:slo_error_budget_remaining",
                &l,
                st.error_budget.remaining_ratio,
            );
            if let Some(s) = st.sli {
                snap.push("forgebase:slo_sli", &l, s);
            }
            if gauges {
                metrics::gauge!(names::SLO_ERROR_BUDGET_REMAINING, "slo" => name.clone())
                    .set(st.error_budget.remaining_ratio);
                metrics::gauge!(names::SLO_SLI, "slo" => name.clone()).set(st.sli.unwrap_or(1.0));
                metrics::gauge!(names::SLO_OBJECTIVE, "slo" => name.clone()).set(st.slo.objective);
            }
        }
        Ok(())
    }

    /// One evaluation round: counts of SLOs flushed, every enabled rule
    /// evaluated, alert states updated in one transaction, notifications
    /// queued. Rounds never overlap.
    pub async fn evaluate(&self) -> Result<RoundReport> {
        let _round = self.inner.round.lock().await;
        if let Err(e) = self.inner.slo.flush(self.db()).await {
            tracing::warn!(error = %e, "SLO counts not saved; kept for the next round");
        }
        let all = self.all_rules().await?;
        let active: Vec<&RuleDef> = all.iter().filter(|r| r.enabled && !r.disabled_by_config).collect();
        let mut names = BTreeSet::new();
        let mut ranged = BTreeSet::new();
        let mut longest = 0;
        for r in &active {
            r.expr.names(&mut names);
            r.expr.ranges(&mut ranged, &mut longest);
        }
        let interval = i64::try_from(self.settings().interval.as_millis()).unwrap_or(i64::MAX);
        let (mut snap, mut failed) = self.snapshot(&names, false).await;
        // SLO samples and gauges every round, whether or not a rule reads them.
        if let Err(e) = self.slo_signals(&mut snap, true).await {
            tracing::debug!(error = %e, "SLO signals not collected");
            failed.extend(
                [
                    "forgebase:slo_burn_rate",
                    "forgebase:slo_error_budget_remaining",
                    "forgebase:slo_sli",
                ]
                .map(str::to_owned),
            );
        }
        let now = snap.at_ms;
        let results = {
            let mut history = self
                .inner
                .history
                .lock()
                .map_err(|_| AlertError::Internal("history lock".into()))?;
            history.configure(ranged.into_iter().collect(), longest + 2 * interval);
            history.record(&snap);
            let cx = expr::Context {
                now: &snap,
                history: &history,
            };
            let mut results = Vec::new();
            for r in &active {
                let mut used = BTreeSet::new();
                r.expr.names(&mut used);
                if used.iter().any(|n| failed.contains(n)) {
                    results.push((*r, None));
                    continue;
                }
                let hits: Vec<_> = expr::eval(&r.expr, &cx)
                    .into_samples()
                    .into_iter()
                    .filter(|s| r.op.holds(s.value, r.threshold))
                    .collect();
                results.push((*r, Some(hits)));
            }
            results
        };
        let mut report = RoundReport {
            rules: results.iter().filter(|(_, h)| h.is_some()).count(),
            skipped: results.iter().filter(|(_, h)| h.is_none()).count(),
            ..RoundReport::default()
        };
        let open = read(
            self.db(),
            "SELECT id, fingerprint, rule_id, state, active_at FROM _forgebase_alerts WHERE state IN ('pending', 'firing')",
            vec![],
        )
        .await?;
        let mut open: HashMap<String, OpenRow> = Row::all(&open)
            .map(|r| {
                (
                    r.text("fingerprint"),
                    OpenRow {
                        id: r.text("id"),
                        rule_id: r.text("rule_id"),
                        state: AlertState::parse(&r.text("state")).unwrap_or(AlertState::Pending),
                        active_at: r.int("active_at"),
                    },
                )
            })
            .collect();
        let skipped: HashSet<&str> = results
            .iter()
            .filter(|(_, h)| h.is_none())
            .map(|(r, _)| r.id.as_str())
            .collect();
        let mut stmts = Vec::new();
        let mut seen = HashSet::new();
        for (rule, hits) in &results {
            let Some(hits) = hits else { continue };
            for s in hits {
                let labels = rules::alert_labels(rule, &s.labels);
                let fp = rules::fingerprint(&rule.id, &labels);
                if !seen.insert(fp.clone()) {
                    continue;
                }
                let annotations: BTreeMap<String, String> = rule
                    .annotations
                    .iter()
                    .map(|(k, v)| (k.clone(), rules::render(v, &labels, s.value)))
                    .collect();
                let ann = to_json(&annotations)?;
                let value: Value = if s.value.is_finite() {
                    s.value.into()
                } else {
                    Value::Null
                };
                match open.remove(&fp) {
                    Some(row) if row.state == AlertState::Pending && now - row.active_at >= rule.for_ms => {
                        stmts.push(Statement::new(
                            "UPDATE _forgebase_alerts SET state = 'firing', fired_at = ?2, value = ?3,
                                 annotations = ?4, last_eval_at = ?2, updated_at = ?2 WHERE id = ?1",
                            vec![row.id.as_str().into(), now.into(), value.clone(), ann.into()],
                        ));
                        stmts.push(event(&row.id, now, AlertState::Firing, value));
                        report.firing += 1;
                    }
                    Some(row) => {
                        stmts.push(Statement::new(
                            "UPDATE _forgebase_alerts SET value = ?3, annotations = ?4, last_eval_at = ?2,
                                 updated_at = ?2 WHERE id = ?1",
                            vec![row.id.as_str().into(), now.into(), value, ann.into()],
                        ));
                        if row.state == AlertState::Firing {
                            report.firing += 1;
                        } else {
                            report.pending += 1;
                        }
                    }
                    None => {
                        let id = new_id("alr");
                        let state = if rule.for_ms == 0 {
                            AlertState::Firing
                        } else {
                            AlertState::Pending
                        };
                        stmts.push(Statement::new(
                            format!(
                                "INSERT INTO _forgebase_alerts ({ALERT_COLS}, updated_at) \
                                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, NULL, ?10, ?10)"
                            ),
                            vec![
                                id.as_str().into(),
                                fp.as_str().into(),
                                rule.id.as_str().into(),
                                rule.name.as_str().into(),
                                rule.severity.as_str().into(),
                                state.as_str().into(),
                                to_json(&labels)?.into(),
                                ann.into(),
                                value.clone(),
                                now.into(),
                                if state == AlertState::Firing {
                                    now.into()
                                } else {
                                    Value::Null
                                },
                            ],
                        ));
                        stmts.push(event(&id, now, AlertState::Pending, value.clone()));
                        if state == AlertState::Firing {
                            stmts.push(event(&id, now, AlertState::Firing, value));
                            report.firing += 1;
                        } else {
                            report.pending += 1;
                        }
                    }
                }
            }
        }
        for row in open.values() {
            if skipped.contains(row.rule_id.as_str()) {
                if row.state == AlertState::Firing {
                    report.firing += 1;
                } else {
                    report.pending += 1;
                }
                continue;
            }
            stmts.push(Statement::new(
                "UPDATE _forgebase_alerts SET state = 'resolved', resolved_at = ?2, last_eval_at = ?2,
                     updated_at = ?2 WHERE id = ?1",
                vec![row.id.as_str().into(), now.into()],
            ));
            stmts.push(event(&row.id, now, AlertState::Resolved, Value::Null));
            report.resolved += 1;
        }
        write(self.db(), stmts).await?;
        report.notifications = self.plan_notifications(now).await?;
        self.alert_gauges().await;
        Ok(report)
    }

    async fn alert_gauges(&self) {
        let Ok(rows) = read(
            self.db(),
            "SELECT state, severity, COUNT(*) AS n FROM _forgebase_alerts WHERE state IN ('pending', 'firing')
             GROUP BY state, severity",
            vec![],
        )
        .await
        else {
            return;
        };
        let counts: HashMap<(String, String), i64> = Row::all(&rows)
            .map(|r| ((r.text("state"), r.text("severity")), r.int("n")))
            .collect();
        for state in ["pending", "firing"] {
            for sev in [Severity::Info, Severity::Warning, Severity::Critical] {
                let n = counts
                    .get(&(state.to_owned(), sev.as_str().to_owned()))
                    .copied()
                    .unwrap_or(0);
                metrics::gauge!(names::ALERTS, "state" => state, "severity" => sev.as_str()).set(n as f64);
            }
        }
    }

    // ------------------------------------------------------ notifications

    /// Compares each notifier's alert groups with what it was last told and
    /// queues a notification for a new or changed group, a group due for a
    /// repeat, and a group that resolved. Silenced alerts are left out.
    async fn plan_notifications(&self, now: i64) -> Result<usize> {
        let notifiers: Vec<NotifierRow> = self.notifier_rows().await?.into_iter().filter(|n| n.enabled).collect();
        if notifiers.is_empty() {
            return Ok(0);
        }
        let rows = read(
            self.db(),
            &format!("SELECT {ALERT_COLS} FROM _forgebase_alerts WHERE state = 'firing' ORDER BY active_at"),
            vec![],
        )
        .await?;
        let silences = self.active_silences().await?;
        let firing: Vec<Alert> = Row::all(&rows)
            .map(|r| alert_from(&r))
            .filter(|a| silenced_by(&silences, &a.labels).is_empty())
            .collect();
        let mut queued = 0;
        for n in &notifiers {
            let Ok(matchers) = compile_matchers(&n.matchers) else {
                continue;
            };
            let mut groups: BTreeMap<String, (BTreeMap<String, String>, Vec<&Alert>)> = BTreeMap::new();
            for a in firing
                .iter()
                .filter(|a| a.severity >= n.min_severity && matchers.iter().all(|m| m.matches(&a.labels)))
            {
                let gl: BTreeMap<String, String> = n
                    .group_by
                    .iter()
                    .map(|k| (k.clone(), a.labels.get(k).cloned().unwrap_or_default()))
                    .collect();
                let key = group_key(&gl);
                groups.entry(key).or_insert_with(|| (gl, Vec::new())).1.push(a);
            }
            let stored = read(
                self.db(),
                "SELECT group_key, firing, last_sent_at FROM _forgebase_alert_groups WHERE notifier_id = ?1",
                vec![n.id.as_str().into()],
            )
            .await?;
            let stored: HashMap<String, (Vec<String>, i64)> = Row::all(&stored)
                .map(|r| {
                    (
                        r.text("group_key"),
                        (r.json::<Vec<String>>("firing"), r.int("last_sent_at")),
                    )
                })
                .collect();
            let mut stmts = Vec::new();
            for (key, (gl, alerts)) in &groups {
                let mut fps: Vec<String> = alerts.iter().map(|a| a.fingerprint.clone()).collect();
                fps.sort();
                let (send, gone) = match stored.get(key) {
                    None => (true, Vec::new()),
                    Some((old, _)) if *old != fps => (true, old.iter().filter(|f| !fps.contains(f)).cloned().collect()),
                    Some((_, last)) => (now - last >= n.repeat_ms, Vec::new()),
                };
                if !send {
                    continue;
                }
                let resolved = if n.send_resolved {
                    self.resolved_alerts(&gone).await?
                } else {
                    Vec::new()
                };
                let payload = self.payload(n, key, gl, "firing", alerts, &resolved, now);
                stmts.push(outbox(&n.id, key, "firing", &payload, now)?);
                stmts.push(Statement::new(
                    "INSERT INTO _forgebase_alert_groups (notifier_id, group_key, firing, last_sent_at)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT (notifier_id, group_key) DO UPDATE SET firing = excluded.firing,
                         last_sent_at = excluded.last_sent_at",
                    vec![
                        n.id.as_str().into(),
                        key.as_str().into(),
                        to_json(&fps)?.into(),
                        now.into(),
                    ],
                ));
                queued += 1;
            }
            for (key, (old, _)) in &stored {
                if groups.contains_key(key) {
                    continue;
                }
                // Alerts that left because they are silenced are not resolved.
                let resolved = self.resolved_alerts(old).await?;
                if !resolved.is_empty() && (n.send_resolved || n.kind == NotifierKind::Pagerduty) {
                    let gl = resolved
                        .first()
                        .map(|a| {
                            n.group_by
                                .iter()
                                .map(|k| (k.clone(), a.labels.get(k).cloned().unwrap_or_default()))
                                .collect()
                        })
                        .unwrap_or_default();
                    let payload = self.payload(n, key, &gl, "resolved", &[], &resolved, now);
                    stmts.push(outbox(&n.id, key, "resolved", &payload, now)?);
                    queued += 1;
                }
                stmts.push(Statement::new(
                    "DELETE FROM _forgebase_alert_groups WHERE notifier_id = ?1 AND group_key = ?2",
                    vec![n.id.as_str().into(), key.as_str().into()],
                ));
            }
            write(self.db(), stmts).await?;
        }
        Ok(queued)
    }

    /// The latest alert of each fingerprint that is resolved now.
    async fn resolved_alerts(&self, fps: &[String]) -> Result<Vec<Alert>> {
        let mut out = Vec::new();
        for fp in fps {
            let rows = read(
                self.db(),
                &format!(
                    "SELECT {ALERT_COLS} FROM _forgebase_alerts WHERE fingerprint = ?1 ORDER BY active_at DESC LIMIT 1"
                ),
                vec![fp.as_str().into()],
            )
            .await?;
            if let Some(a) = Row::all(&rows).next().map(|r| alert_from(&r))
                && a.state == AlertState::Resolved
            {
                out.push(a);
            }
        }
        Ok(out)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn payload(
        &self,
        n: &NotifierRow,
        key: &str,
        group_labels: &BTreeMap<String, String>,
        status: &str,
        firing: &[&Alert],
        resolved: &[Alert],
        now: i64,
    ) -> Json {
        let one = |a: &Alert, st: &str| {
            json!({
                "status": st,
                "id": a.id,
                "fingerprint": a.fingerprint,
                "rule_id": a.rule_id,
                "name": a.name,
                "severity": a.severity,
                "labels": a.labels,
                "annotations": a.annotations,
                "value": a.value,
                "starts_at": a.fired_at.clone().unwrap_or_else(|| a.active_at.clone()),
                "ends_at": a.resolved_at,
            })
        };
        let alerts: Vec<Json> = firing
            .iter()
            .map(|a| one(a, "firing"))
            .chain(resolved.iter().map(|a| one(a, "resolved")))
            .collect();
        json!({
            "version": "1",
            "status": status,
            "notifier": n.name,
            "group_key": key,
            "group_labels": group_labels,
            "alerts": alerts,
            "created_at": iso(now),
            "external_url": self.inner.deps.public_url.as_ref().map(|u| format!("{u}/admin/")),
        })
    }

    // ----------------------------------------------------------- delivery

    /// Sends the notifications that are due (first tries and retries).
    /// A failed attempt is retried after 30 s, doubling up to an hour, until
    /// the eighth attempt fails; then the delivery is `failed`.
    pub async fn deliver_due(&self) -> Result<usize> {
        let now = self.now();
        let rows = read(
            self.db(),
            &format!(
                "SELECT {DELIVERY_COLS} FROM _forgebase_alert_notifications WHERE status = 'pending'
                 AND next_attempt_at <= ?1 ORDER BY next_attempt_at, id LIMIT 50"
            ),
            vec![now.into()],
        )
        .await?;
        let due: Vec<Delivery> = Row::all(&rows).map(|r| delivery_from(&r)).collect();
        let mut sent = 0;
        let mut cache: HashMap<String, Option<NotifierRow>> = HashMap::new();
        for d in due {
            let n = match cache.get(&d.notifier_id) {
                Some(n) => n.clone(),
                None => {
                    let n = self.notifier_row(&d.notifier_id).await.ok();
                    cache.insert(d.notifier_id.clone(), n.clone());
                    n
                }
            };
            let attempt = match &n {
                Some(n) if n.enabled => self.send(n, &d.id, &d.event, &d.payload).await,
                Some(_) => crate::notify::Attempt::failed(None, "the notifier is switched off"),
                None => crate::notify::Attempt::failed(None, "the notifier no longer exists"),
            };
            if attempt.ok {
                sent += 1;
            }
            self.record_attempt(&d, n.as_ref(), &attempt).await?;
        }
        Ok(sent)
    }

    async fn record_attempt(&self, d: &Delivery, n: Option<&NotifierRow>, a: &crate::notify::Attempt) -> Result<()> {
        let now = self.now();
        let attempts = d.attempts + 1;
        let kind = n.map_or("unknown", |n| n.kind.as_str());
        metrics::counter!(
            names::ALERT_NOTIFICATIONS_TOTAL,
            "notifier_type" => kind,
            "outcome" => if a.ok { "sent" } else { "failed" }
        )
        .increment(1);
        let s = self.settings();
        let mut stmts = Vec::new();
        if a.ok {
            stmts.push(Statement::new(
                "UPDATE _forgebase_alert_notifications SET status = 'sent', attempts = ?2, last_status = ?3,
                     last_error = NULL, sent_at = ?4, updated_at = ?4 WHERE id = ?1",
                vec![d.id.as_str().into(), attempts.into(), a.status.into(), now.into()],
            ));
        } else {
            let give_up = attempts >= s.max_attempts || n.is_none();
            let delay = s
                .retry_base_ms
                .saturating_mul(1_i64 << u32::try_from((attempts - 1).clamp(0, 20)).unwrap_or(20))
                .min(3_600_000);
            stmts.push(Statement::new(
                "UPDATE _forgebase_alert_notifications SET status = ?2, attempts = ?3, last_status = ?4,
                     last_error = ?5, next_attempt_at = ?6, updated_at = ?7 WHERE id = ?1",
                vec![
                    d.id.as_str().into(),
                    if give_up { "failed" } else { "pending" }.into(),
                    attempts.into(),
                    a.status.into(),
                    a.error.clone().into(),
                    (now + delay).into(),
                    now.into(),
                ],
            ));
            tracing::warn!(
                notifier = n.map_or("?", |n| n.name.as_str()),
                delivery = %d.id,
                attempt = attempts,
                error = a.error.as_deref().unwrap_or(""),
                "alert notification failed"
            );
        }
        if let Some(n) = n {
            stmts.push(if a.ok {
                Statement::new(
                    "UPDATE _forgebase_alert_notifiers SET last_success_at = ?2 WHERE id = ?1",
                    vec![n.id.as_str().into(), now.into()],
                )
            } else {
                Statement::new(
                    "UPDATE _forgebase_alert_notifiers SET last_failure_at = ?2, last_error = ?3 WHERE id = ?1",
                    vec![n.id.as_str().into(), now.into(), a.error.clone().into()],
                )
            });
        }
        write(self.db(), stmts).await
    }

    /// Sends a test notification to a notifier now and records it as a
    /// delivery (`event = "test"`, not retried). At most
    /// `test_sends_per_minute` per notifier.
    pub async fn test_notifier(&self, ctx: &AccessContext, id: &str) -> Result<Delivery> {
        self.require_admin(ctx)?;
        let n = self.notifier_row(id).await?;
        let now = self.now();
        {
            let mut t = self
                .inner
                .tests
                .lock()
                .map_err(|_| AlertError::Internal("rate limiter lock".into()))?;
            let v = t.entry(n.id.clone()).or_default();
            v.retain(|at| now - at < 60_000);
            if v.len() >= self.settings().test_sends_per_minute {
                return Err(AlertError::RateLimited(format!(
                    "at most {} test notifications per notifier and minute",
                    self.settings().test_sends_per_minute
                )));
            }
            v.push(now);
        }
        let test = Alert {
            id: "test".into(),
            fingerprint: "test".into(),
            rule_id: "test".into(),
            name: "ForgeBaseTest".into(),
            severity: Severity::Info,
            state: AlertState::Firing,
            labels: [
                ("alertname".to_owned(), "ForgeBaseTest".to_owned()),
                ("severity".to_owned(), "info".to_owned()),
            ]
            .into_iter()
            .collect(),
            annotations: [(
                "summary".to_owned(),
                "Test notification from ForgeBase: this notifier works".to_owned(),
            )]
            .into_iter()
            .collect(),
            value: Some(1.0),
            active_at: iso(now),
            fired_at: Some(iso(now)),
            resolved_at: None,
            last_evaluated_at: iso(now),
            silenced_by: Vec::new(),
        };
        let gl: BTreeMap<String, String> = [("alertname".to_owned(), "ForgeBaseTest".to_owned())]
            .into_iter()
            .collect();
        let key = format!("test:{}", new_id("t"));
        let payload = self.payload(&n, &key, &gl, "test", &[&test], &[], now);
        let delivery_id = new_id("ntn");
        let a = self.send(&n, &delivery_id, "test", &payload).await;
        metrics::counter!(
            names::ALERT_NOTIFICATIONS_TOTAL,
            "notifier_type" => n.kind.as_str(),
            "outcome" => if a.ok { "sent" } else { "failed" }
        )
        .increment(1);
        write(
            self.db(),
            vec![Statement::new(
                "INSERT INTO _forgebase_alert_notifications (id, notifier_id, group_key, event, status, payload,
                     attempts, next_attempt_at, last_status, last_error, sent_at, created_at, updated_at)
                 VALUES (?1, ?2, ?3, 'test', ?4, ?5, 1, ?6, ?7, ?8, ?9, ?6, ?6)",
                vec![
                    delivery_id.as_str().into(),
                    n.id.as_str().into(),
                    key.as_str().into(),
                    if a.ok { "sent" } else { "failed" }.into(),
                    to_json(&payload)?.into(),
                    now.into(),
                    a.status.into(),
                    a.error.clone().into(),
                    if a.ok { now.into() } else { Value::Null },
                ],
            )],
        )
        .await?;
        let rows = read(
            self.db(),
            &format!("SELECT {DELIVERY_COLS} FROM _forgebase_alert_notifications WHERE id = ?1"),
            vec![delivery_id.as_str().into()],
        )
        .await?;
        Row::all(&rows)
            .next()
            .map(|r| delivery_from(&r))
            .ok_or_else(|| AlertError::Internal("test delivery not recorded".into()))
    }

    // ------------------------------------------------------- maintenance

    /// Deletes resolved alerts, finished deliveries and expired silences
    /// older than `history_retention_days`, and SLO minutes older than
    /// their window.
    pub async fn prune(&self) -> Result<()> {
        let now = self.now();
        let cutoff = now - self.settings().history_retention_ms;
        write(
            self.db(),
            vec![
                Statement::new(
                    "DELETE FROM _forgebase_alert_events WHERE alert_id IN (SELECT id FROM _forgebase_alerts
                         WHERE state = 'resolved' AND resolved_at < ?1)",
                    vec![cutoff.into()],
                ),
                Statement::new(
                    "DELETE FROM _forgebase_alerts WHERE state = 'resolved' AND resolved_at < ?1",
                    vec![cutoff.into()],
                ),
                Statement::new(
                    "DELETE FROM _forgebase_alert_notifications WHERE status <> 'pending' AND created_at < ?1",
                    vec![cutoff.into()],
                ),
                Statement::new(
                    "DELETE FROM _forgebase_alert_silences WHERE ends_at < ?1",
                    vec![cutoff.into()],
                ),
            ],
        )
        .await?;
        crate::slo::prune(self.db(), &self.slos_unchecked().await?, now).await
    }

    /// Starts the evaluation and delivery task: a round every
    /// `evaluation_interval_secs` (sooner after a change through the API),
    /// pruning every hour.
    pub fn start_background(&self) {
        let svc = self.clone();
        let handle = tokio::spawn(async move {
            let mut last_prune = 0_i64;
            loop {
                if let Err(e) = svc.evaluate().await {
                    tracing::warn!(error = %e, "alert evaluation round failed");
                }
                if let Err(e) = svc.deliver_due().await {
                    tracing::warn!(error = %e, "alert delivery round failed");
                }
                let now = svc.now();
                if now - last_prune >= 3_600_000 {
                    last_prune = now;
                    if let Err(e) = svc.prune().await {
                        tracing::warn!(error = %e, "alert history pruning failed");
                    }
                }
                tokio::select! {
                    _ = tokio::time::sleep(svc.settings().interval) => {}
                    _ = svc.inner.wake.notified() => {
                        // Coalesce a burst of API changes.
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                    _ = svc.inner.stop.notified() => break,
                }
            }
        });
        if let Ok(mut t) = self.inner.task.lock() {
            *t = Some(handle);
        }
    }

    /// Stops the task (waiting up to `grace` for a running round) and saves
    /// the SLO counts gathered since the last round.
    pub async fn shutdown(&self, grace: Duration) {
        let handle = self.inner.task.lock().ok().and_then(|mut t| t.take());
        if let Some(h) = handle {
            self.inner.stop.notify_one();
            let abort = h.abort_handle();
            if tokio::time::timeout(grace, h).await.is_err() {
                abort.abort();
            }
        }
        if let Err(e) = self.inner.slo.flush(self.db()).await {
            tracing::warn!(error = %e, "SLO counts not saved at shutdown");
        }
    }
}

fn event(alert_id: &str, at: i64, state: AlertState, value: Value) -> Statement {
    Statement::new(
        "INSERT INTO _forgebase_alert_events (alert_id, at, state, value) VALUES (?1, ?2, ?3, ?4)",
        vec![alert_id.into(), at.into(), state.as_str().into(), value],
    )
}

fn outbox(notifier: &str, key: &str, event: &str, payload: &Json, now: i64) -> Result<Statement> {
    Ok(Statement::new(
        "INSERT INTO _forgebase_alert_notifications (id, notifier_id, group_key, event, status, payload, attempts,
             next_attempt_at, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, 'pending', ?5, 0, ?6, ?6, ?6)",
        vec![
            new_id("ntn").into(),
            notifier.into(),
            key.into(),
            event.into(),
            to_json(payload)?.into(),
            now.into(),
        ],
    ))
}

/// `{alertname="X",subscription="y"}`: the group's labels as text.
pub(crate) fn group_key(labels: &BTreeMap<String, String>) -> String {
    let inner: Vec<String> = labels.iter().map(|(k, v)| format!("{k}={v:?}")).collect();
    format!("{{{}}}", inner.join(","))
}
