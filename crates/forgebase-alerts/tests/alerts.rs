//! The alerting engine against real persistence (ADR-0035): rule states and
//! history, `for`, silences, every notifier against a local receiver (signed
//! webhook verified, Slack, PagerDuty trigger and resolve, email over a
//! scripted SMTP server, a queue topic), retries, the built-in rules for a
//! paused push subscription and dead-letter growth, SLO budgets and burn-rate
//! alerts under injected 5xx, and administrator-only HTTP access.
//! `FORGEBASE_TEST_BACKEND=postgres` runs it on PostgreSQL.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeMap;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::middleware::{self, Next};
use common::{H, admin, harness, harness_with, system};
use forgebase_alerts::*;
use forgebase_data::AccessContext;
use forgebase_db::Backend as _;
use forgebase_queue::{NewMessage, NewSubscription, NewTopic, PauseRequest, PullRequest, PushConfig, PushKind};
use serde_json::{Value as Json, json};
use tower::ServiceExt;

fn rule(name: &str, expr: &str, threshold: f64, for_: &str) -> RuleInput {
    serde_json::from_value(json!({
        "name": name, "expr": expr, "threshold": threshold, "for": for_,
        "annotations": {"summary": "{{ $labels.x }} is {{ $value }}"},
    }))
    .unwrap()
}

fn notifier(v: Json) -> NotifierInput {
    serde_json::from_value(v).unwrap()
}

async fn notifications(h: &H, notifier: &str) -> Vec<Delivery> {
    h.svc
        .deliveries(&admin(), Some(notifier), None, None)
        .await
        .unwrap()
        .items
}

#[tokio::test]
async fn a_rule_goes_pending_then_firing_after_for_then_resolved_with_history() {
    let h = harness().await;
    h.metrics
        .set("test_queue_depth{x=\"a\"} 10\ntest_queue_depth{x=\"b\"} 1\n");
    let r = h
        .svc
        .create_rule(&admin(), rule("DepthHigh", "test_queue_depth", 5.0, "1m"))
        .await
        .unwrap();
    assert_eq!(r.source, RuleSource::Api);
    assert_eq!(r.for_ms, 60_000);

    let rep = h.round().await;
    assert_eq!((rep.pending, rep.firing), (1, 0), "{rep:?}");
    let open = h.svc.active(&admin(), None, None).await.unwrap();
    assert_eq!(open.len(), 1);
    let a = &open[0];
    assert_eq!(a.state, AlertState::Pending);
    assert_eq!(a.labels["x"], "a");
    assert_eq!(a.labels["alertname"], "DepthHigh");
    assert_eq!(a.labels["severity"], "warning");
    assert_eq!(a.annotations["summary"], "a is 10");
    assert_eq!(a.value, Some(10.0));

    // `for` is honoured: 30 s later it is still pending.
    h.advance(Duration::from_secs(30));
    assert_eq!(h.round().await.pending, 1);
    assert_eq!(h.svc.active(&admin(), Some("firing"), None).await.unwrap().len(), 0);

    h.advance(Duration::from_secs(31));
    let rep = h.round().await;
    assert_eq!((rep.pending, rep.firing), (0, 1), "{rep:?}");
    let firing = h.svc.active(&admin(), Some("firing"), None).await.unwrap();
    assert_eq!(firing[0].id, a.id, "the same alert, now firing");
    assert!(firing[0].fired_at.is_some());

    // Still firing: the same alert again, value updated.
    h.metrics.set("test_queue_depth{x=\"a\"} 12\n");
    h.advance(Duration::from_secs(5));
    assert_eq!(h.round().await.firing, 1);
    assert_eq!(h.svc.active(&admin(), None, None).await.unwrap()[0].value, Some(12.0));

    // Below the threshold: resolved.
    h.metrics.set("test_queue_depth{x=\"a\"} 2\n");
    h.advance(Duration::from_secs(5));
    assert_eq!(h.round().await.resolved, 1);
    assert!(h.svc.active(&admin(), None, None).await.unwrap().is_empty());
    let page = h
        .svc
        .history(&admin(), None, None, None, None, None, None)
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    let d = h.svc.alert(&admin(), &a.id).await.unwrap();
    assert_eq!(d.alert.state, AlertState::Resolved);
    let states: Vec<AlertState> = d.events.iter().map(|e| e.state).collect();
    assert_eq!(states, [AlertState::Pending, AlertState::Firing, AlertState::Resolved]);

    // A pending alert that clears resolves without firing; it opens again
    // as a new alert.
    h.metrics.set("test_queue_depth{x=\"a\"} 9\n");
    h.round().await;
    h.metrics.set("test_queue_depth{x=\"a\"} 1\n");
    h.round().await;
    let page = h
        .svc
        .history(&admin(), Some("resolved"), Some(&r.id), None, None, None, Some(1))
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert!(page.items[0].fired_at.is_none(), "never fired");
    assert!(page.next_before.is_some(), "two resolved alerts: a second page");
    let next = h
        .svc
        .history(
            &admin(),
            Some("resolved"),
            None,
            None,
            None,
            page.next_before.as_deref(),
            Some(1),
        )
        .await
        .unwrap();
    assert_eq!(next.items[0].id, a.id);

    // Disabled rules are not evaluated; deleting resolves open alerts.
    h.metrics.set("test_queue_depth{x=\"a\"} 9\n");
    h.round().await;
    h.svc.delete_rule(&admin(), &r.id).await.unwrap();
    assert_eq!(h.round().await.resolved, 1);
}

#[tokio::test]
async fn webhook_is_signed_silences_mute_and_groups_notify_on_change_repeat_and_resolve() {
    let h = harness().await;
    h.metrics.set("test_errors{x=\"a\"} 3\ntest_errors{x=\"b\"} 4\n");
    h.svc
        .create_rule(&admin(), rule("ErrorsSeen", "test_errors", 0.0, "0s"))
        .await
        .unwrap();
    let n = h
        .svc
        .create_notifier(
            &admin(),
            notifier(
                json!({"name": "ops", "kind": "webhook", "url": "http://hooks.test/alerts",
                            "group_by": ["alertname", "x"], "repeat_interval": "1h"}),
            ),
        )
        .await
        .unwrap();
    let secret = n.signing_secret.clone().expect("shown once");
    assert!(secret.starts_with("whsec_"));
    assert!(
        h.svc.notifier(&admin(), "ops").await.unwrap().signing_secret.is_none(),
        "never listed"
    );
    // x=b is silenced for an hour.
    let sil = h
        .svc
        .create_silence(
            &admin(),
            serde_json::from_value(json!({"matchers": [{"label": "x", "value": "b"}], "duration": "1h",
                                          "comment": "maintenance"}))
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(sil.status, "active");

    let rep = h.round().await;
    assert_eq!(rep.firing, 2);
    assert_eq!(rep.notifications, 1, "only the unsilenced group");
    let calls = h.receiver.on("/alerts");
    assert_eq!(calls.len(), 1);
    let c = &calls[0];
    let now = h.clock_secs();
    assert!(
        forgebase_queue::push::verify(&secret, c.header("x-forgebase-signature"), &c.body, now, 300),
        "the ADR-0031 signature verifies"
    );
    assert!(!forgebase_queue::push::verify(
        "whsec_wrong",
        c.header("x-forgebase-signature"),
        &c.body,
        now,
        300
    ));
    assert_eq!(c.header("x-forgebase-event"), "alert.firing");
    let body = c.json();
    assert_eq!(body["status"], "firing");
    assert_eq!(body["group_labels"]["x"], "a");
    assert_eq!(body["alerts"][0]["labels"]["x"], "a");
    assert_eq!(body["external_url"], "https://forgebase.example.com/admin/");
    let silenced = h.svc.active(&admin(), None, None).await.unwrap();
    let b = silenced.iter().find(|a| a.labels["x"] == "b").unwrap();
    assert_eq!(b.silenced_by, vec![sil.id.clone()]);

    // Nothing changed: no new notification (dedup) until the repeat interval.
    h.advance(Duration::from_secs(60));
    assert_eq!(h.round().await.notifications, 0);
    assert_eq!(h.receiver.on("/alerts").len(), 1);

    // The silence ends: x=b is notified now.
    h.svc.expire_silence(&admin(), &sil.id).await.unwrap();
    assert_eq!(h.round().await.notifications, 1);
    let calls = h.receiver.on("/alerts");
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[1].json()["group_labels"]["x"], "b");

    // The repeat interval passes: both groups again.
    h.advance(Duration::from_secs(3600));
    assert_eq!(h.round().await.notifications, 2);

    // x=a resolves: a resolved notification for its group.
    h.metrics.set("test_errors{x=\"b\"} 4\n");
    h.advance(Duration::from_secs(5));
    h.round().await;
    let last = h.receiver.on("/alerts").pop().unwrap();
    assert_eq!(last.header("x-forgebase-event"), "alert.resolved");
    let body = last.json();
    assert_eq!(body["status"], "resolved");
    assert_eq!(body["alerts"][0]["status"], "resolved");
    assert!(body["alerts"][0]["ends_at"].is_string());
    let sent = notifications(&h, "ops").await;
    assert!(sent.iter().all(|d| d.status == "sent"), "{sent:?}");
    assert_eq!(sent.len(), 5);
}

#[tokio::test]
async fn failed_deliveries_are_retried_with_backoff_and_visible() {
    let h = harness().await;
    h.metrics.set("test_flag 1\n");
    h.svc
        .create_rule(&admin(), rule("Flag", "test_flag", 0.0, "0s"))
        .await
        .unwrap();
    h.svc
        .create_notifier(
            &admin(),
            notifier(json!({"name": "hook", "kind": "webhook", "url": "http://hooks.test/flaky"})),
        )
        .await
        .unwrap();
    h.receiver.set_status(503);
    h.round().await;
    let d = notifications(&h, "hook").await;
    assert_eq!(d.len(), 1);
    assert_eq!(
        (d[0].status.as_str(), d[0].attempts, d[0].last_status),
        ("pending", 1, Some(503))
    );
    assert!(d[0].last_error.as_deref().unwrap().contains("503"));
    let n = h.svc.notifier(&admin(), "hook").await.unwrap();
    assert!(n.last_failure_at.is_some() && n.last_error.is_some());

    // Not due yet: nothing is sent.
    h.svc.deliver_due().await.unwrap();
    assert_eq!(h.receiver.on("/flaky").len(), 1);
    // After the 1 s backoff (tests), the second attempt; then 2 s.
    h.advance(Duration::from_millis(1_100));
    h.svc.deliver_due().await.unwrap();
    assert_eq!(notifications(&h, "hook").await[0].attempts, 2);
    h.receiver.set_status(200);
    h.advance(Duration::from_millis(2_100));
    h.svc.deliver_due().await.unwrap();
    let d = notifications(&h, "hook").await;
    assert_eq!((d[0].status.as_str(), d[0].attempts), ("sent", 3));
    assert_eq!(h.receiver.on("/flaky").len(), 3);
    let failed = h.svc.deliveries(&admin(), None, Some("failed"), None).await.unwrap();
    assert!(failed.items.is_empty());
}

#[tokio::test]
async fn gives_up_after_the_last_attempt() {
    let h = harness_with(|s| s.max_attempts = 2).await;
    h.metrics.set("test_flag 1\n");
    h.svc
        .create_rule(&admin(), rule("Flag", "test_flag", 0.0, "0s"))
        .await
        .unwrap();
    h.svc
        .create_notifier(
            &admin(),
            notifier(json!({"name": "hook", "kind": "webhook", "url": "http://hooks.test/down"})),
        )
        .await
        .unwrap();
    h.receiver.set_status(500);
    h.round().await;
    h.advance(Duration::from_secs(2));
    h.svc.deliver_due().await.unwrap();
    let d = notifications(&h, "hook").await;
    assert_eq!((d[0].status.as_str(), d[0].attempts), ("failed", 2));
}

#[tokio::test]
async fn slack_pagerduty_email_and_queue_notifiers_deliver() {
    let h = harness().await;
    h.queue
        .create_topic(
            &system(),
            NewTopic {
                name: "alerts".into(),
                ..NewTopic::default()
            },
        )
        .await
        .unwrap();
    h.queue
        .create_subscription(
            &system(),
            NewSubscription {
                name: "fanout".into(),
                topic: "alerts".into(),
                ..NewSubscription::default()
            },
        )
        .await
        .unwrap();
    for n in [
        json!({"name": "slack", "kind": "slack", "url": "http://hooks.test/slack/T000/B000/XXX"}),
        json!({"name": "pd", "kind": "pagerduty", "url": "http://hooks.test/pd", "routing_key": "R0UT1NG"}),
        json!({"name": "mail", "kind": "email", "to": ["oncall@example.com"]}),
        json!({"name": "topic", "kind": "queue", "topic": "alerts"}),
    ] {
        h.svc.create_notifier(&admin(), notifier(n)).await.unwrap();
    }
    let slack = h.svc.notifier(&admin(), "slack").await.unwrap();
    assert_eq!(slack.url.as_deref(), Some("hooks.test"), "the Slack URL is a secret");

    h.metrics.set("test_paused{subscription=\"orders\"} 1\n");
    h.svc
        .create_rule(
            &admin(),
            serde_json::from_value(json!({"name": "Paused", "expr": "test_paused", "threshold": 0,
                "severity": "critical",
                "annotations": {"summary": "{{ $labels.subscription }} is paused"}}))
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(h.round().await.notifications, 4);

    let s = h.receiver.on("/slack/T000/B000/XXX");
    assert_eq!(s.len(), 1);
    let text = s[0].json()["text"].as_str().unwrap().to_owned();
    assert!(text.contains("[FIRING:1] Paused"), "{text}");
    assert!(text.contains("orders is paused"), "{text}");

    let pd = h.receiver.on("/pd");
    assert_eq!(pd.len(), 1);
    let trig = pd[0].json();
    assert_eq!(trig["event_action"], "trigger");
    assert_eq!(trig["routing_key"], "R0UT1NG");
    assert_eq!(trig["payload"]["severity"], "critical");
    assert_eq!(trig["payload"]["source"], "forgebase");
    let dedup = trig["dedup_key"].as_str().unwrap().to_owned();

    // Email: queued through customization, delivered over SMTP.
    let rows =
        h.db.fetch_all(&forgebase_db::Statement::raw(
            "SELECT id FROM _forgebase_email_deliveries WHERE recipient = 'oncall@example.com'",
        ))
        .await
        .unwrap();
    assert_eq!(rows.rows.len(), 1, "one email queued");
    let id = rows.rows[0][0].as_str().unwrap().to_owned();
    h.customize.deliver(&id, 1, true).await.unwrap();
    let mails = h.smtp.lock().unwrap().clone();
    assert_eq!(mails.len(), 1);
    assert!(mails[0].contains("To: oncall@example.com"), "{}", mails[0]);
    assert!(mails[0].contains("Subject:"), "{}", mails[0]);

    // Queue topic: the notification is a message.
    let got = h
        .queue
        .pull(
            &system(),
            "fanout",
            PullRequest {
                max_messages: Some(10),
                ..PullRequest::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].message.data["status"], "firing");
    assert_eq!(got[0].message.attributes["alertname"], "Paused");
    assert_eq!(got[0].message.attributes["severity"], "critical");

    // Resolve: PagerDuty gets resolve with the same dedup key.
    h.metrics.set("");
    h.advance(Duration::from_secs(5));
    h.round().await;
    let pd = h.receiver.on("/pd");
    assert_eq!(pd.len(), 2);
    assert_eq!(pd[1].json()["event_action"], "resolve");
    assert_eq!(pd[1].json()["dedup_key"], dedup.as_str());

    // Test sends: recorded, PagerDuty opens and closes at once.
    let t = h.svc.test_notifier(&admin(), "slack").await.unwrap();
    assert_eq!((t.event.as_str(), t.status.as_str()), ("test", "sent"));
    let before = h.receiver.on("/pd").len();
    assert_eq!(h.svc.test_notifier(&admin(), "pd").await.unwrap().status, "sent");
    let pd = h.receiver.on("/pd");
    assert_eq!(pd.len(), before + 2);
    assert_eq!(pd[before].json()["event_action"], "trigger");
    assert_eq!(pd[before + 1].json()["event_action"], "resolve");
    let all = notifications(&h, "slack").await;
    assert!(all.iter().any(|d| d.event == "test"));
}

#[tokio::test]
async fn notifier_validation_refuses_private_and_plain_http_targets() {
    let h = harness_with(|s| s.allow_http = false).await;
    for (input, needle) in [
        (
            json!({"name": "a", "kind": "webhook", "url": "http://hooks.test/x"}),
            "https://",
        ),
        (
            json!({"name": "b", "kind": "webhook", "url": "https://10.0.0.5/x"}),
            "public",
        ),
        (
            json!({"name": "c", "kind": "webhook", "url": "https://user:pw@hooks.test/x"}),
            "user or password",
        ),
        (json!({"name": "d", "kind": "pagerduty"}), "routing_key"),
        (
            json!({"name": "e", "kind": "email", "to": ["not an address"]}),
            "email address",
        ),
        (json!({"name": "f", "kind": "queue"}), "topic"),
        (json!({"name": "bad name!", "kind": "queue", "topic": "t"}), "letters"),
    ] {
        let e = h
            .svc
            .create_notifier(&admin(), notifier(input.clone()))
            .await
            .unwrap_err();
        assert!(e.to_string().contains(needle), "{input}: {e}");
    }
    // A host that resolves to a private address is refused at send time.
    let h = harness().await;
    h.svc
        .create_notifier(
            &admin(),
            notifier(json!({"name": "int", "kind": "webhook", "url": "http://internal.test/x"})),
        )
        .await
        .unwrap();
    let t = h.svc.test_notifier(&admin(), "int").await.unwrap();
    assert_eq!(t.status, "failed");
    assert!(t.last_error.unwrap().contains("blocked"));
}

#[tokio::test]
async fn built_in_rules_fire_for_a_paused_push_subscription_and_dead_letter_growth() {
    let h = harness_with(|s| {
        s.disabled_rules
            .retain(|r| r != "queue_push_paused" && r != "queue_dead_letters_growing")
    })
    .await;
    let sys = system();
    h.queue
        .create_topic(
            &sys,
            NewTopic {
                name: "events".into(),
                ..NewTopic::default()
            },
        )
        .await
        .unwrap();
    h.queue
        .create_subscription(
            &sys,
            NewSubscription {
                name: "hook".into(),
                topic: "events".into(),
                push: Some(PushConfig {
                    kind: PushKind::Webhook,
                    endpoint: "http://hooks.test/push".into(),
                }),
                ..NewSubscription::default()
            },
        )
        .await
        .unwrap();
    h.queue
        .create_subscription(
            &sys,
            NewSubscription {
                name: "work".into(),
                topic: "events".into(),
                max_delivery_attempts: Some(1),
                ..NewSubscription::default()
            },
        )
        .await
        .unwrap();
    // Nothing yet: both rules are quiet (and record the baseline).
    let rep = h.round().await;
    assert_eq!(rep.firing, 0, "{rep:?}");
    assert!(rep.rules >= 2);

    h.queue
        .pause(
            &sys,
            "hook",
            PauseRequest {
                reason: Some("by hand".into()),
            },
        )
        .await
        .unwrap();
    h.queue
        .publish(&sys, "events", vec![NewMessage::new(json!({"n": 1}))])
        .await
        .unwrap();
    let got = h
        .queue
        .pull(
            &sys,
            "work",
            PullRequest {
                max_messages: Some(1),
                ..PullRequest::default()
            },
        )
        .await
        .unwrap();
    h.queue.nack(&sys, "work", &[got[0].ack_id.clone()]).await.unwrap();
    h.advance(Duration::from_secs(30));
    h.round().await;
    let firing = h.svc.active(&admin(), Some("firing"), None).await.unwrap();
    let names: Vec<(&str, &str)> = firing
        .iter()
        .map(|a| (a.name.as_str(), a.labels.get("subscription").map_or("", String::as_str)))
        .collect();
    assert!(names.contains(&("QueuePushPaused", "hook")), "{names:?}");
    assert!(names.contains(&("QueueDeadLettersGrowing", "work")), "{names:?}");
    let paused = firing.iter().find(|a| a.name == "QueuePushPaused").unwrap();
    assert_eq!(paused.severity, Severity::Critical);
    assert_eq!(paused.annotations["summary"], "Push delivery of hook is paused");
    assert!(paused.annotations["runbook_url"].ends_with("observability.md#queue_push_paused"));

    // Resumed: the pause alert resolves.
    h.queue.resume(&sys, "hook").await.unwrap();
    h.advance(Duration::from_secs(30));
    h.round().await;
    let firing = h.svc.active(&admin(), Some("firing"), None).await.unwrap();
    assert!(!firing.iter().any(|a| a.name == "QueuePushPaused"), "{firing:?}");
}

#[tokio::test]
async fn built_in_rules_are_listed_and_can_be_switched_off_but_not_edited() {
    let h = harness_with(|s| s.disabled_rules = vec!["backup_stale".into()]).await;
    let rules = h.svc.rules(&admin()).await.unwrap();
    let ids: Vec<&str> = rules
        .iter()
        .filter(|r| r.source == RuleSource::Builtin)
        .map(|r| r.id.as_str())
        .collect();
    for id in [
        "http_5xx_ratio_high",
        "http_latency_p95_high",
        "queue_backlog_high",
        "queue_oldest_unacked_high",
        "queue_dead_letters_growing",
        "queue_push_paused",
        "billing_webhook_signature_invalid",
        "job_failure_ratio_high",
        "jobs_dead_lettered",
        "disk_free_low",
        "disk_free_critical",
        "backup_stale",
        "tls_cert_expiring",
        "tls_cert_expiry_critical",
    ] {
        assert!(ids.contains(&id), "{id} listed");
    }
    let stale = rules.iter().find(|r| r.id == "backup_stale").unwrap();
    assert!(stale.disabled_by_config && !stale.enabled);
    let e = h
        .svc
        .patch_rule(
            &admin(),
            "backup_stale",
            RulePatch {
                enabled: Some(true),
                ..RulePatch::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(forgebase_core::ErrorCode::code(&e), "rule_disabled_by_config");
    let r = h
        .svc
        .patch_rule(
            &admin(),
            "queue_backlog_high",
            RulePatch {
                enabled: Some(false),
                ..RulePatch::default()
            },
        )
        .await
        .unwrap();
    assert!(!r.enabled);
    let e = h
        .svc
        .patch_rule(
            &admin(),
            "queue_backlog_high",
            RulePatch {
                threshold: Some(1.0),
                ..RulePatch::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(forgebase_core::ErrorCode::code(&e), "rule_read_only");
    assert_eq!(
        forgebase_core::ErrorCode::code(&h.svc.delete_rule(&admin(), "disk_free_low").await.unwrap_err()),
        "rule_read_only"
    );
    let e = h
        .svc
        .create_rule(&admin(), rule("Bad", "rate(x)", 1.0, "0s"))
        .await
        .unwrap_err();
    assert_eq!(forgebase_core::ErrorCode::code(&e), "invalid_alert_rule");
    // The TLS rules use the configured gauge.
    let tls = rules.iter().find(|r| r.id == "tls_cert_expiring").unwrap();
    assert!(tls.expr.contains("forgebase_tls_certificate_expiry_timestamp_seconds"));
}

#[tokio::test]
async fn certificate_rules_plug_in_by_metric_name() {
    let h = harness_with(|s| {
        s.disabled_rules.retain(|r| !r.starts_with("tls_"));
        s.tls_cert_expiry_metric = "acme_cert_not_after".into();
    })
    .await;
    let soon = h.clock_secs() + 2 * 86_400;
    h.metrics
        .set(&format!("acme_cert_not_after{{domain=\"api.example.com\"}} {soon}\n"));
    h.round().await;
    let firing = h.svc.active(&admin(), Some("firing"), None).await.unwrap();
    let names: Vec<&str> = firing.iter().map(|a| a.name.as_str()).collect();
    assert!(
        names.contains(&"TlsCertificateExpiring") && names.contains(&"TlsCertificateExpiringSoon"),
        "{names:?}"
    );
    assert_eq!(firing[0].labels["domain"], "api.example.com");
}

#[tokio::test]
async fn query_evaluates_an_expression_now() {
    let h = harness().await;
    h.metrics.set("q_total{a=\"1\"} 4\nq_total{a=\"2\"} 6\n");
    let r = h
        .svc
        .query(
            &admin(),
            QueryInput {
                expr: "sum(q_total) / 2".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(r.samples.len(), 1);
    assert_eq!(r.samples[0].value, Some(5.0));
    let e = h
        .svc
        .query(&admin(), QueryInput { expr: "sum(".into() })
        .await
        .unwrap_err();
    assert_eq!(forgebase_core::ErrorCode::code(&e), "invalid_expression");
}

// ---------------------------------------------------------------- HTTP

/// `x-test-as: admin | user | anon` sets the caller (as the auth layer would).
fn app(h: &H) -> Router {
    let layout = forgebase_core::DataLayout::new(h.dir.path().join("data"));
    let state = forgebase_api::AppState::from_data(h.db.clone(), layout, h.data.clone());
    let flaky = Router::new().route(
        "/api/v1/test/flaky/{code}",
        axum::routing::get(|axum::extract::Path(code): axum::extract::Path<u16>| async move {
            StatusCode::from_u16(code).unwrap_or(StatusCode::OK)
        }),
    );
    let app = forgebase_api::router_with(
        state,
        forgebase_api::RouterOptions {
            extensions: vec![forgebase_alerts::extension(h.svc.clone())],
            http_observers: vec![h.svc.http_observer()],
            ..forgebase_api::RouterOptions::default()
        },
        flaky,
        None,
    );
    app.layer(middleware::from_fn(|mut req: Request<Body>, next: Next| async move {
        match req.headers().get("x-test-as").and_then(|v| v.to_str().ok()) {
            Some("admin") => {
                req.extensions_mut().insert(admin());
            }
            Some("user") => {
                req.extensions_mut().insert(AccessContext::user("users", "u1"));
            }
            _ => {}
        }
        next.run(req).await
    }))
}

async fn call(app: &Router, method: Method, uri: &str, who: &str, body: Option<&str>) -> (StatusCode, Json) {
    let mut req = Request::builder().method(method).uri(uri).header("x-test-as", who);
    if body.is_some() {
        req = req.header("content-type", "application/json");
    }
    let res = app
        .clone()
        .oneshot(
            req.body(body.map_or_else(Body::empty, |b| Body::from(b.to_owned())))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 22).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Json::Null))
}

#[tokio::test]
async fn the_api_is_for_administrators_only() {
    let h = harness().await;
    let app = app(&h);
    for (method, uri, body) in [
        (Method::GET, "/api/v1/alerts", None),
        (Method::GET, "/api/v1/alerts/history", None),
        (Method::GET, "/api/v1/alerts/rules", None),
        (Method::POST, "/api/v1/alerts/rules", Some("{not json")),
        (Method::GET, "/api/v1/alerts/silences", None),
        (Method::POST, "/api/v1/alerts/silences", Some("{")),
        (Method::GET, "/api/v1/alerts/notifiers", None),
        (Method::POST, "/api/v1/alerts/notifiers/x/test", None),
        (Method::GET, "/api/v1/alerts/notifications", None),
        (Method::POST, "/api/v1/alerts/query", Some("{")),
        (Method::GET, "/api/v1/slos", None),
        (Method::GET, "/api/v1/slos/status", None),
        (Method::DELETE, "/api/v1/slos/x", None),
    ] {
        let (s, j) = call(&app, method.clone(), uri, "anon", body).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{method} {uri}: {j}");
        // Refused before the (invalid) body is read: 403, not 400.
        let (s, j) = call(&app, method.clone(), uri, "user", body).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{method} {uri}: {j}");
        assert_eq!(j["error"]["code"], "forbidden");
    }
    let (s, j) = call(&app, Method::GET, "/api/v1/alerts/rules", "admin", None).await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert!(j.as_array().unwrap().len() >= 14);
    let (s, j) = call(
        &app,
        Method::POST,
        "/api/v1/alerts/rules",
        "admin",
        Some(r#"{"name": "X", "expr": "up", "threshold": 1}"#),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{j}");
    let (s, j) = call(
        &app,
        Method::POST,
        "/api/v1/alerts/rules",
        "admin",
        Some(r#"{"name": "X", "expr": "up", "threshold": 1}"#),
    )
    .await;
    assert_eq!(
        (s, j["error"]["code"].as_str()),
        (StatusCode::CONFLICT, Some("alert_rule_exists"))
    );
    let (s, j) = call(&app, Method::GET, "/api/v1/alerts/rules/nope", "admin", None).await;
    assert_eq!(
        (s, j["error"]["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("alert_rule_not_found"))
    );
    // Test sends are rate limited (429).
    let (s, _) = call(
        &app,
        Method::POST,
        "/api/v1/alerts/notifiers",
        "admin",
        Some(r#"{"name": "t", "kind": "webhook", "url": "http://hooks.test/rl"}"#),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let mut last = StatusCode::OK;
    for _ in 0..11 {
        last = call(&app, Method::POST, "/api/v1/alerts/notifiers/t/test", "admin", None)
            .await
            .0;
    }
    assert_eq!(last, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn slo_budget_and_burn_rate_alerts_under_injected_5xx() {
    let h = harness().await;
    let app = app(&h);
    let (s, slo) = call(
        &app,
        Method::POST,
        "/api/v1/slos",
        "admin",
        Some(
            r#"{"name": "flaky-api", "kind": "availability", "objective": 0.99, "window": "30d",
                 "route_prefix": "/api/v1/test/"}"#,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{slo}");
    let (s, lat) = call(
        &app,
        Method::POST,
        "/api/v1/slos",
        "admin",
        Some(
            r#"{"name": "flaky-latency", "kind": "latency", "objective": 0.9, "window": "1d",
                 "route_regex": "/api/v1/test/.*", "methods": ["GET"], "latency_threshold_ms": 60000}"#,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{lat}");
    // Its burn-rate rules exist: 3 tiers for 30d, 2 for 1d.
    let rules = h.svc.rules(&admin()).await.unwrap();
    assert_eq!(rules.iter().filter(|r| r.id.starts_with("slo:flaky-api:")).count(), 3);
    assert_eq!(
        rules.iter().filter(|r| r.id.starts_with("slo:flaky-latency:")).count(),
        2
    );

    // 80 good and 20 bad requests; 5 unmatched ones do not count.
    for i in 0..100 {
        let code = if i % 5 == 0 { "500" } else { "200" };
        let (s, _) = call(&app, Method::GET, &format!("/api/v1/test/flaky/{code}"), "anon", None).await;
        assert_eq!(s.as_u16().to_string(), code);
    }
    for _ in 0..5 {
        call(&app, Method::GET, "/nowhere", "anon", None).await;
    }
    let rep = h.round().await;
    assert!(rep.firing >= 3, "{rep:?}");
    let (s, st) = call(&app, Method::GET, "/api/v1/slos/flaky-api/status", "admin", None).await;
    assert_eq!(s, StatusCode::OK, "{st}");
    assert_eq!(
        (st["total"].as_i64(), st["good"].as_i64(), st["bad"].as_i64()),
        (Some(100), Some(80), Some(20))
    );
    assert!((st["sli"].as_f64().unwrap() - 0.8).abs() < 1e-9);
    // allowed = 1% of 100 = 1; consumed 20: remaining 1 - 20 = -19.
    assert!(
        (st["error_budget"]["allowed"].as_f64().unwrap() - 1.0).abs() < 1e-9,
        "{st}"
    );
    assert_eq!(st["error_budget"]["consumed"], 20);
    assert!(
        (st["error_budget"]["remaining_ratio"].as_f64().unwrap() + 19.0).abs() < 1e-9,
        "{st}"
    );
    // Burn rate: 20% errors / 1% allowed = 20 on every window.
    for w in ["5m", "30m", "1h", "6h", "3d"] {
        assert!((st["burn_rates"][w].as_f64().unwrap() - 20.0).abs() < 1e-9, "{w}: {st}");
    }
    let tiers: BTreeMap<String, (f64, bool)> = st["alerts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            (
                t["tier"].as_str().unwrap().to_owned(),
                (
                    t["burn_rate_threshold"].as_f64().unwrap(),
                    t["firing"].as_bool().unwrap(),
                ),
            )
        })
        .collect();
    assert!(
        (tiers["page_fast"].0 - 14.4).abs() < 1e-9 && tiers["page_fast"].1,
        "{tiers:?}"
    );
    assert!((tiers["page_slow"].0 - 6.0).abs() < 1e-9 && tiers["page_slow"].1);
    assert!((tiers["ticket"].0 - 1.0).abs() < 1e-9 && tiers["ticket"].1);
    let firing = h.svc.active(&admin(), Some("firing"), None).await.unwrap();
    let fast = firing.iter().find(|a| a.rule_id == "slo:flaky-api:page_fast").unwrap();
    assert_eq!(fast.name, "SloBurnRateHigh");
    assert_eq!(
        (fast.labels["slo"].as_str(), fast.labels["tier"].as_str()),
        ("flaky-api", "page_fast")
    );
    assert_eq!(fast.severity, Severity::Critical);
    assert!((fast.value.unwrap() - 20.0).abs() < 1e-9);
    // Latency: the 500s are left out; every 200 was within 60 s.
    let (_, st) = call(&app, Method::GET, "/api/v1/slos/flaky-latency/status", "admin", None).await;
    assert_eq!(
        (st["total"].as_i64(), st["good"].as_i64()),
        (Some(80), Some(80)),
        "{st}"
    );
    assert!(
        !firing
            .iter()
            .any(|a| a.labels.get("slo").is_some_and(|s| s == "flaky-latency"))
    );

    // The budget recovers as good traffic dilutes the short windows: after
    // 10 minutes of only good requests the 5m window is clean, so the fast
    // tier (which needs both windows) resolves while the 1h window still burns.
    h.advance(Duration::from_secs(600));
    // 20 bad of 130 in the hour: 15.4% / 1% = 15.4, still above 14.4.
    for _ in 0..30 {
        call(&app, Method::GET, "/api/v1/test/flaky/200", "anon", None).await;
    }
    h.round().await;
    let (_, st) = call(&app, Method::GET, "/api/v1/slos/flaky-api/status", "admin", None).await;
    assert_eq!(st["burn_rates"]["5m"].as_f64(), Some(0.0), "{st}");
    assert!(st["burn_rates"]["1h"].as_f64().unwrap() > 14.4);
    let firing = h.svc.active(&admin(), Some("firing"), None).await.unwrap();
    assert!(
        !firing.iter().any(|a| a.rule_id == "slo:flaky-api:page_fast"),
        "the 5m window is clean"
    );
    assert!(firing.iter().any(|a| a.rule_id == "slo:flaky-api:ticket"));

    // Validation.
    let (s, j) = call(
        &app,
        Method::POST,
        "/api/v1/slos",
        "admin",
        Some(r#"{"name": "bad", "kind": "latency", "objective": 0.99}"#),
    )
    .await;
    assert_eq!(
        (s, j["error"]["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_slo"))
    );
    let (s, _) = call(&app, Method::DELETE, "/api/v1/slos/flaky-latency", "admin", None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let rules = h.svc.rules(&admin()).await.unwrap();
    assert!(!rules.iter().any(|r| r.id.starts_with("slo:flaky-latency:")));
}

impl H {
    fn clock_secs(&self) -> i64 {
        forgebase_jobs::Clock::now_ms(&self.clock).div_euclid(1000)
    }
}

#[tokio::test]
async fn invalid_file_rules_stop_the_start() {
    let decl = forgebase_core::config::telemetry_config::AlertRuleDecl {
        name: "Broken".into(),
        expr: "rate(up)".into(),
        threshold: 1.0,
        ..Default::default()
    };
    let h = harness().await;
    let e = AlertService::open(AlertDeps {
        db: std::sync::Arc::new(h.db.clone()),
        keyring: forgebase_auth::crypto::Keyring::load_or_create(&h.dir.path().join("k2")).unwrap(),
        clock: std::sync::Arc::new(h.clock.clone()),
        fetcher: None,
        customize: None,
        queue: None,
        layout: None,
        metrics: std::sync::Arc::new(ProcessMetrics),
        settings: AlertSettings {
            config_rules: vec![decl],
            ..AlertSettings::default()
        },
        public_url: None,
    })
    .await
    .unwrap_err();
    assert!(e.to_string().contains("alerts.rules Broken"), "{e}");
}
