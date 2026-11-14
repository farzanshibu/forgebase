//! Mock tests for queue webhook tooling, the billing webhook inspector, alerts and SLOs, and
//! operations insights (including the streamed audit export). Hermetic: no ForgeBase binary needed.

mod common;

use common::mock::{Mock, err, no_content, ok, serve, status, text};
use common::sample::{schema, with};
use forgebase_sdk::alerts::{AlertHistoryQuery, NotificationsQuery};
use forgebase_sdk::billing_admin::WebhookDeliveriesQuery;
use forgebase_sdk::generated::{
    AlertNotifierInput, AlertRuleInput, AlertRulePatch, AlertSilenceInput, PushOutcome, SloInput, SubscriptionPatch,
    TopicPatch,
};
use forgebase_sdk::insights::{
    AuditQuery, ExportFormat, InvocationsQuery, QueriesQuery, RegressionsQuery, SlowQueriesQuery, TimeRange, UsageQuery,
};
use forgebase_sdk::queue_webhooks::{AttemptsQuery, Redrive};
use forgebase_sdk::{ErrorKind, NewSubscription, NewTopic, QueuesApi};
use serde_json::json;

fn sub_json() -> serde_json::Value {
    json!({"name": "orders-push", "topic": "orders", "ack_deadline_ms": 10000, "backoff_base_ms": 1000, "backoff_max_ms": 600000,
        "ordering": false, "labels": {}, "created_by": "u", "created_at": "t", "updated_at": "t",
        "push": {"kind": "webhook", "endpoint": "https://hooks.test/in"}, "push_secret": "whsec_abc",
        "pause_after_failures": 5, "paused_at": "t", "paused_reason": "auto: 5 consecutive failures",
        "push_secret_previous_expires_at": "2026-01-02T00:00:00Z", "expires_at": "2026-02-01T00:00:00Z"})
}

fn queues(m: &Mock) -> QueuesApi {
    m.client().queues()
}

// ---- queue webhook tooling ----

#[tokio::test]
async fn queue_push_subscription_new_and_changed_fields() {
    let m = Mock::start(vec![
        status(201, sub_json()),
        ok(sub_json()),
        status(201, json!({"name": "orders", "retention_ms": 1, "max_message_bytes": 1, "dedup_window_ms": 1, "labels": {}, "published_total": 0,
            "published_bytes": 0, "created_by": "u", "created_at": "t", "updated_at": "t", "description": "Order events", "example": {"order": 1}, "retention_bytes": 1024})),
        ok(json!({"name": "orders", "description": "x"})),
        ok(schema("TopicMetrics")),
    ])
    .await;
    let q = queues(&m);
    let mut new = NewSubscription::new("orders-push", "orders");
    new.push = Some(forgebase_sdk::queues::PushTarget::webhook("https://hooks.test/in"));
    new.pause_after_failures = Some(5);
    new.ttl_ms = Some(86_400_000);
    new.labels.insert("team".into(), "billing".into());
    let s = q.create_subscription(&new).await.expect("create");
    assert_eq!(
        s.push_secret.as_deref(),
        Some("whsec_abc"),
        "the secret comes back on create"
    );
    assert_eq!(s.push.as_ref().map(|p| p.kind.as_str()), Some("webhook"));
    assert_eq!(s.paused_reason.as_deref(), Some("auto: 5 consecutive failures"));
    assert_eq!(s.pause_after_failures, Some(5));
    assert!(s.expires_at.is_some());
    assert_eq!(
        m.last().json(),
        json!({"name": "orders-push", "topic": "orders", "push": {"kind": "webhook", "endpoint": "https://hooks.test/in"},
            "pause_after_failures": 5, "ttl_ms": 86400000, "labels": {"team": "billing"}})
    );
    let patched = q
        .patch_subscription(
            "orders-push",
            &SubscriptionPatch {
                pause_after_failures: Some(0),
                clear_push: Some(false),
                ..SubscriptionPatch::default()
            },
        )
        .await
        .expect("patch");
    assert_eq!(patched.name, "orders-push");
    assert_eq!(
        (m.last().method.as_str(), m.last().json()),
        ("PATCH", json!({"pause_after_failures": 0, "clear_push": false}))
    );

    let mut t = NewTopic::new("orders");
    t.description = Some("Order events".into());
    t.example = Some(json!({"order": 1}));
    t.retention_bytes = Some(1024);
    let topic = q.create_topic(&t).await.expect("topic");
    assert_eq!(topic.description.as_deref(), Some("Order events"));
    assert_eq!(topic.example, Some(json!({"order": 1})));
    assert_eq!(
        m.last().json(),
        json!({"name": "orders", "description": "Order events", "example": {"order": 1}, "retention_bytes": 1024})
    );
    q.patch_topic(
        "orders",
        &TopicPatch {
            description: Some(String::new()),
            clear_example: Some(true),
            ..TopicPatch::default()
        },
    )
    .await
    .expect("patch topic");
    assert_eq!(m.last().json(), json!({"description": "", "clear_example": true}));
    q.topic_metrics("orders").await.expect("topic metrics");
    assert_eq!(m.last().path, "/api/v1/queues/topics/orders/metrics");
}

#[tokio::test]
async fn queue_webhook_attempts_and_test_send() {
    let attempt = with(
        "PushAttempt",
        json!({"id": 41, "subscription": "orders-push", "message_id": "test-1", "attempt": 1, "test": true,
        "outcome": "failure", "error_class": "http_status", "status": 500, "latency_ms": 12, "response_body": "boom", "response_truncated": false}),
    );
    let m = Mock::start(vec![
        ok(with(
            "PushAttemptPage",
            json!({"items": [attempt.clone()], "next_before": 41}),
        )),
        ok(attempt.clone()),
        ok(attempt.clone()),
        ok(attempt),
        err(409, "not_push_subscription"),
        err(429, "rate_limited"),
    ])
    .await;
    let w = queues(&m).webhooks("orders-push");
    let page = w
        .attempts(&AttemptsQuery {
            outcome: Some(PushOutcome::Failure),
            message_id: Some("m1".into()),
            since: Some("2026-01-01T00:00:00Z".into()),
            test: Some(false),
            before: Some(99),
            limit: Some(20),
            ..AttemptsQuery::default()
        })
        .await
        .expect("attempts");
    assert_eq!(page.next_before, Some(41));
    let a = &page.items[0];
    assert_eq!((a.id, a.status, a.test), (41, Some(500), true));
    assert_eq!(a.response_body.as_deref(), Some("boom"));
    let c = m.last();
    assert_eq!(c.route(), "/api/v1/queues/subscriptions/orders-push/attempts");
    assert_eq!(
        (
            c.q("outcome").as_deref(),
            c.q("message_id").as_deref(),
            c.q("test").as_deref(),
            c.q("before").as_deref(),
            c.q("limit").as_deref()
        ),
        (Some("failure"), Some("m1"), Some("false"), Some("99"), Some("20"))
    );
    assert_eq!(c.q("since").as_deref(), Some("2026-01-01T00:00:00Z"));
    assert_eq!(w.attempt(41).await.expect("attempt").id, 41);
    assert_eq!(m.last().path, "/api/v1/queues/subscriptions/orders-push/attempts/41");

    let t = w.test(Some(&json!({"hello": "world"}))).await.expect("test send");
    assert!(t.test);
    assert_eq!(
        (m.last().method.as_str(), m.last().route()),
        ("POST", "/api/v1/queues/subscriptions/orders-push/test")
    );
    assert_eq!(m.last().json(), json!({"data": {"hello": "world"}}));
    w.test(None).await.expect("default test");
    assert_eq!(m.last().json(), json!({}));

    let pull = w.test(None).await.expect_err("pull subscription");
    assert_eq!((pull.status(), pull.code()), (Some(409), "not_push_subscription"));
    let limited = w.test(None).await.expect_err("rate limited");
    assert_eq!(limited.kind(), Some(ErrorKind::RateLimit));
}

#[tokio::test]
async fn queue_webhook_redrive_retry_secret_pause_resume() {
    let m = Mock::start(vec![
        ok(json!({"redriven": [1, 2], "already_redriven": [3], "not_found": [99], "remaining": 0})),
        ok(json!({"redriven": [4], "already_redriven": [], "not_found": [], "remaining": 7})),
        ok(json!({"count": 2})),
        ok(json!({"count": 1})),
        ok(json!({"subscription": "orders-push", "push_secret": "whsec_new", "previous_expires_at": "2026-01-02T00:00:00Z"})),
        ok(sub_json()),
        ok(sub_json()),
        ok(sub_json()),
    ])
    .await;
    let w = queues(&m).webhooks("orders-push");
    let r = w.redrive(&Redrive::Ids(vec![1, 2, 3, 99])).await.expect("redrive ids");
    assert_eq!(
        (r.redriven, r.already_redriven, r.not_found, r.remaining),
        (vec![1, 2], vec![3], vec![99], 0)
    );
    assert_eq!(m.last().json(), json!({"ids": [1, 2, 3, 99]}));
    let r = w
        .redrive(&Redrive::All {
            since: Some("2026-01-01T00:00:00Z".into()),
            filter: Some(json!({"kind": "rush"})),
            limit: Some(100),
        })
        .await
        .expect("redrive all");
    assert_eq!(r.remaining, 7);
    assert_eq!(
        m.last().json(),
        json!({"all": true, "since": "2026-01-01T00:00:00Z", "filter": {"kind": "rush"}, "limit": 100})
    );
    assert_eq!(m.last().route(), "/api/v1/queues/subscriptions/orders-push/redrive");

    assert_eq!(w.retry(None).await.expect("retry").count, 2);
    assert_eq!(m.last().json(), json!({}));
    assert_eq!(w.retry(Some(&["m1"])).await.expect("retry some").count, 1);
    assert_eq!(m.last().json(), json!({"message_ids": ["m1"]}));

    let rot = w.rotate_secret(Some(3_600_000)).await.expect("rotate");
    assert_eq!(rot.push_secret, "whsec_new");
    assert!(rot.previous_expires_at.is_some());
    assert_eq!(
        (m.last().route(), m.last().json()),
        (
            "/api/v1/queues/subscriptions/orders-push/secret/rotate",
            json!({"grace_ms": 3600000})
        )
    );
    let s = w.expire_previous_secret().await.expect("expire previous");
    assert_eq!(
        s.push_secret_previous_expires_at.as_deref(),
        Some("2026-01-02T00:00:00Z")
    );
    assert_eq!(
        m.last().route(),
        "/api/v1/queues/subscriptions/orders-push/secret/expire-previous"
    );
    let paused = w.pause(Some("maintenance")).await.expect("pause");
    assert!(paused.paused_at.is_some());
    assert_eq!(m.last().json(), json!({"reason": "maintenance"}));
    w.resume().await.expect("resume");
    assert_eq!(m.last().route(), "/api/v1/queues/subscriptions/orders-push/resume");
}

#[tokio::test]
async fn queue_catalog_lists_topics_with_examples() {
    let (m, fb) = serve(ok(json!({"topics": [with("CatalogTopic", json!({"topic": "orders", "example": {"order": 1}, "example_source": "topic"}))]}))).await;
    let cat = fb.queues().catalog().await.expect("catalog");
    assert_eq!(cat.topics[0].topic, "orders");
    assert_eq!(cat.topics[0].example, json!({"order": 1}));
    assert_eq!(m.only().path, "/api/v1/queues/catalog");
}

// ---- billing webhook inspector ----

#[tokio::test]
async fn billing_webhook_deliveries_list_get_replay_reverify() {
    let delivery = with(
        "WebhookDelivery",
        json!({"id": "wd1", "provider": "stripe", "outcome": "signature_invalid"}),
    );
    let m = Mock::start(vec![
        ok(with(
            "WebhookDeliveryPage",
            json!({"items": [delivery.clone()], "total": 41, "limit": 50, "offset": 0}),
        )),
        ok(with("WebhookDeliveryDetail", json!({"id": "wd1"}))),
        ok(json!({"result": "applied", "detail": "subscription renewed", "delivery": delivery.clone()})),
        ok(json!({"result": "unverified", "delivery": delivery})),
        err(409, "delivery_not_verified"),
    ])
    .await;
    let w = m.client().billing().webhook_deliveries();
    let page = w
        .list(&WebhookDeliveriesQuery {
            provider: Some("stripe".into()),
            outcome: Some("signature_invalid".into()),
            event_type: Some("invoice.paid".into()),
            from: Some("2026-01-01T00:00:00Z".into()),
            limit: Some(50),
            offset: Some(10),
            ..WebhookDeliveriesQuery::default()
        })
        .await
        .expect("list");
    assert_eq!((page.total, page.items[0].provider.as_str()), (41, "stripe"));
    let q = m.last();
    assert_eq!(q.route(), "/api/v1/billing/webhook-deliveries");
    assert_eq!(
        (
            q.q("provider").as_deref(),
            q.q("outcome").as_deref(),
            q.q("event_type").as_deref(),
            q.q("offset").as_deref()
        ),
        (
            Some("stripe"),
            Some("signature_invalid"),
            Some("invoice.paid"),
            Some("10")
        )
    );
    assert_eq!(w.get("wd1").await.expect("get").id, "wd1");
    let r = w.replay("wd1").await.expect("replay");
    assert_eq!(
        (r.result.as_str(), r.detail.as_deref()),
        ("applied", Some("subscription renewed"))
    );
    assert_eq!(
        (m.last().method.as_str(), m.last().path.as_str()),
        ("POST", "/api/v1/billing/webhook-deliveries/wd1/replay")
    );
    let r = w.reverify("wd1").await.expect("reverify");
    assert_eq!(r.result, "unverified");
    assert_eq!(m.last().path, "/api/v1/billing/webhook-deliveries/wd1/reverify");
    let e = w.replay("wd1").await.expect_err("refused delivery");
    assert_eq!(
        (e.kind(), e.code()),
        (Some(ErrorKind::Conflict), "delivery_not_verified")
    );
}

// ---- alerts and SLOs ----

#[tokio::test]
async fn alerts_rules_crud() {
    let rule = with("AlertRule", json!({"id": "r1", "name": "high-latency"}));
    let m = Mock::start(vec![
        ok(json!([rule.clone()])),
        status(201, rule.clone()),
        ok(rule.clone()),
        ok(rule.clone()),
        no_content(),
        err(409, "rule_read_only"),
    ])
    .await;
    let rules = m.client().alerts().rules();
    assert_eq!(rules.list().await.expect("list")[0].name, "high-latency");
    let input: AlertRuleInput = serde_json::from_value(with(
        "AlertRuleInput",
        json!({"name": "high-latency", "expr": "http_p95 > 1", "op": ">", "threshold": 1.0}),
    ))
    .expect("input");
    let created = rules.create(&input).await.expect("create");
    assert_eq!(created.id, "r1");
    let b = m.last().json();
    assert_eq!(
        (m.last().route(), b["name"].clone(), b["expr"].clone()),
        ("/api/v1/alerts/rules", json!("high-latency"), json!("http_p95 > 1"))
    );
    rules.get("high-latency").await.expect("by name");
    assert_eq!(m.last().path, "/api/v1/alerts/rules/high-latency");
    rules
        .update(
            "r1",
            &AlertRulePatch {
                enabled: Some(false),
                ..AlertRulePatch::default()
            },
        )
        .await
        .expect("patch");
    assert_eq!(
        (m.last().method.as_str(), m.last().json()),
        ("PATCH", json!({"enabled": false}))
    );
    rules.delete("r1").await.expect("delete");
    let e = rules
        .update("builtin", &AlertRulePatch::default())
        .await
        .expect_err("builtin");
    assert_eq!((e.status(), e.code()), (Some(409), "rule_read_only"));
}

#[tokio::test]
async fn alerts_list_history_silences_notifiers_and_test_send() {
    let sil = with("AlertSilence", json!({"id": "s1"}));
    let notifier = with("AlertNotifier", json!({"id": "n1", "name": "ops-slack"}));
    let m = Mock::start(vec![
        ok(json!([schema("Alert")])),
        ok(with("AlertPage", json!({"next_before": "abc"}))),
        ok(schema("AlertDetail")),
        ok(schema("AlertQueryResult")),
        ok(schema("AlertDeliveryPage")),
        ok(json!([sil.clone()])),
        status(201, sil.clone()),
        ok(sil.clone()),
        ok(sil.clone()),
        ok(sil.clone()),
        ok(json!([notifier.clone()])),
        status(201, notifier.clone()),
        ok(notifier.clone()),
        ok(notifier.clone()),
        no_content(),
        ok(schema("AlertDelivery")),
        err(429, "rate_limited"),
    ])
    .await;
    let a = m.client().alerts();
    assert_eq!(
        a.active(Some("firing"), Some("critical")).await.expect("active").len(),
        1
    );
    assert_eq!(
        (m.last().q("state").as_deref(), m.last().q("severity").as_deref()),
        (Some("firing"), Some("critical"))
    );
    let h = a
        .history(&AlertHistoryQuery {
            state: Some("resolved".into()),
            rule_id: Some("r1".into()),
            before: Some("b1".into()),
            limit: Some(25),
            ..AlertHistoryQuery::default()
        })
        .await
        .expect("history");
    assert_eq!(h.next_before.as_deref(), Some("abc"));
    assert_eq!(m.last().route(), "/api/v1/alerts/history");
    assert_eq!(
        (m.last().q("rule_id").as_deref(), m.last().q("limit").as_deref()),
        (Some("r1"), Some("25"))
    );
    a.get("al1").await.expect("alert");
    assert_eq!(m.last().path, "/api/v1/alerts/al1");
    a.query("sum(rate(http_requests[5m]))").await.expect("query");
    assert_eq!(m.last().json(), json!({"expr": "sum(rate(http_requests[5m]))"}));
    a.notifications(&NotificationsQuery {
        notifier: Some("ops-slack".into()),
        status: Some("failed".into()),
        limit: Some(10),
    })
    .await
    .expect("notifications");
    assert_eq!(m.last().q("status").as_deref(), Some("failed"));

    let s = a.silences();
    assert_eq!(s.list(true).await.expect("silences").len(), 1);
    assert_eq!(m.last().q("all").as_deref(), Some("true"));
    let input: AlertSilenceInput = serde_json::from_value(with("AlertSilenceInput", json!({"matchers": [{"label": "severity", "op": "=", "value": "warning"}], "duration": "2h", "comment": "deploy"}))).expect("silence input");
    s.create(&input).await.expect("create");
    assert_eq!(m.last().json()["comment"], "deploy");
    s.get("s1").await.expect("get");
    s.update("s1", &input).await.expect("update");
    assert_eq!(m.last().method, "PUT");
    s.expire("s1").await.expect("expire");
    assert_eq!(
        (m.last().method.as_str(), m.last().path.as_str()),
        ("DELETE", "/api/v1/alerts/silences/s1")
    );

    let n = a.notifiers();
    assert_eq!(n.list().await.expect("notifiers").len(), 1);
    let ninput: AlertNotifierInput = serde_json::from_value(with(
        "AlertNotifierInput",
        json!({"name": "ops-slack", "kind": "slack"}),
    ))
    .expect("notifier input");
    n.create(&ninput).await.expect("create");
    assert_eq!(m.last().json()["name"], "ops-slack");
    n.get("ops-slack").await.expect("get");
    n.update("n1", &ninput).await.expect("update");
    n.delete("n1").await.expect("delete");
    n.test("n1").await.expect("test send");
    assert_eq!(
        (m.last().method.as_str(), m.last().path.as_str()),
        ("POST", "/api/v1/alerts/notifiers/n1/test")
    );
    let e = n.test("n1").await.expect_err("rate limited");
    assert_eq!(e.kind(), Some(ErrorKind::RateLimit));
}

#[tokio::test]
async fn slos_crud_and_status() {
    let slo = with("Slo", json!({"id": "slo1", "name": "api-availability"}));
    let m = Mock::start(vec![
        ok(json!([slo.clone()])),
        status(201, slo.clone()),
        ok(slo.clone()),
        ok(slo.clone()),
        no_content(),
        ok(schema("SloStatus")),
        ok(json!([schema("SloStatus")])),
    ])
    .await;
    let s = m.client().slos();
    assert_eq!(s.list().await.expect("list")[0].name, "api-availability");
    let input: SloInput = serde_json::from_value(with(
        "SloInput",
        json!({"name": "api-availability", "kind": "availability", "objective": 99.9}),
    ))
    .expect("input");
    s.create(&input).await.expect("create");
    assert_eq!(m.last().route(), "/api/v1/slos");
    s.get("api-availability").await.expect("get");
    s.update("slo1", &input).await.expect("update");
    assert_eq!(m.last().method, "PUT");
    s.delete("slo1").await.expect("delete");
    s.status("slo1").await.expect("status");
    assert_eq!(m.last().path, "/api/v1/slos/slo1/status");
    assert_eq!(s.statuses().await.expect("statuses").len(), 1);
    assert_eq!(m.last().path, "/api/v1/slos/status");
}

#[tokio::test]
async fn alerts_disabled_is_a_404_feature_disabled() {
    let (_m, fb) = serve(err(404, "feature_disabled")).await;
    let e = fb.alerts().active(None, None).await.expect_err("404");
    assert!(e.is_not_found());
    assert_eq!(e.code(), "feature_disabled");
}

// ---- insights ----

#[tokio::test]
async fn insights_statements_regressions_and_slow_queries() {
    let m = Mock::start(vec![
        ok(schema("StatementStatus")),
        ok(schema("FlushReport")),
        ok(schema("StatementList")),
        ok(schema("StatementDetail")),
        ok(schema("RegressionReport")),
        ok(schema("SlowStatementPage")),
    ])
    .await;
    let i = m.client().insights();
    i.status().await.expect("status");
    assert_eq!(m.last().path, "/api/v1/insights/status");
    i.flush().await.expect("flush");
    assert_eq!(
        (m.last().method.as_str(), m.last().path.as_str()),
        ("POST", "/api/v1/insights/flush")
    );
    i.queries(&QueriesQuery {
        range: TimeRange::new("2026-01-01", "2026-01-02"),
        sort: Some("p95".into()),
        area: Some("data".into()),
        backend: Some("postgres".into()),
        limit: Some(10),
        ..QueriesQuery::default()
    })
    .await
    .expect("queries");
    let q = m.last();
    assert_eq!(
        (
            q.q("from").as_deref(),
            q.q("to").as_deref(),
            q.q("sort").as_deref(),
            q.q("area").as_deref(),
            q.q("backend").as_deref()
        ),
        (
            Some("2026-01-01"),
            Some("2026-01-02"),
            Some("p95"),
            Some("data"),
            Some("postgres")
        )
    );
    i.query("a1b2 c3", &TimeRange::default()).await.expect("detail");
    assert_eq!(m.last().path, "/api/v1/insights/queries/a1b2%20c3");
    i.regressions(&RegressionsQuery {
        window: Some(1800),
        baseline: Some(86_400),
        min_calls: Some(5),
        min_ratio: Some(1.5),
        ..RegressionsQuery::default()
    })
    .await
    .expect("regressions");
    let q = m.last();
    assert_eq!(q.route(), "/api/v1/insights/queries/regressions");
    assert_eq!(
        (
            q.q("window").as_deref(),
            q.q("baseline").as_deref(),
            q.q("min_ratio").as_deref()
        ),
        (Some("1800"), Some("86400"), Some("1.5"))
    );
    i.slow_queries(&SlowQueriesQuery {
        min_ms: Some(250.0),
        cursor: Some(7),
        fingerprint: Some("abc".into()),
        ..SlowQueriesQuery::default()
    })
    .await
    .expect("slow");
    let q = m.last();
    assert_eq!(
        (
            q.q("min_ms").as_deref(),
            q.q("cursor").as_deref(),
            q.q("fingerprint").as_deref()
        ),
        (Some("250"), Some("7"), Some("abc"))
    );
}

#[tokio::test]
async fn insights_usage_functions_and_invocations() {
    let m = Mock::start(vec![
        ok(schema("UsageReport")),
        ok(schema("UsageReport")),
        ok(schema("UsageSeries")),
        ok(schema("MetricList")),
        ok(schema("RollupResult")),
        ok(schema("FunctionStatList")),
        ok(schema("FunctionDetail")),
        ok(schema("InvocationPage")),
        ok(schema("InvocationDetail")),
    ])
    .await;
    let i = m.client().insights();
    i.usage(&UsageQuery {
        period: Some("month".into()),
        date: Some("2026-01".into()),
        group_by: Some("project".into()),
        sort: Some("cost".into()),
        currency: Some("USD".into()),
        limit: Some(5),
        ..UsageQuery::default()
    })
    .await
    .expect("usage");
    let q = m.last();
    assert_eq!(q.route(), "/api/v1/insights/usage");
    assert_eq!(
        (
            q.q("period").as_deref(),
            q.q("date").as_deref(),
            q.q("group_by").as_deref(),
            q.q("sort").as_deref(),
            q.q("currency").as_deref()
        ),
        (
            Some("month"),
            Some("2026-01"),
            Some("project"),
            Some("cost"),
            Some("USD")
        )
    );
    i.usage_top("storage.bytes", Some(3), &UsageQuery::default())
        .await
        .expect("top");
    let q = m.last();
    assert_eq!(
        (q.route(), q.q("metric").as_deref(), q.q("n").as_deref()),
        ("/api/v1/insights/usage/top", Some("storage.bytes"), Some("3"))
    );
    i.usage_series("env:e1", &TimeRange::new("2026-01-01", "2026-02-01"))
        .await
        .expect("series");
    assert_eq!(m.last().q("scope").as_deref(), Some("env:e1"));
    i.usage_metrics().await.expect("metrics");
    i.usage_rollup(Some("2026-01-01")).await.expect("rollup");
    assert_eq!(
        (m.last().method.as_str(), m.last().q("day").as_deref()),
        ("POST", Some("2026-01-01"))
    );
    i.functions(&TimeRange::default(), Some("resize"), Some(3))
        .await
        .expect("functions");
    assert_eq!(
        (m.last().q("function").as_deref(), m.last().q("version").as_deref()),
        (Some("resize"), Some("3"))
    );
    i.function("resize", &TimeRange::default()).await.expect("function");
    assert_eq!(m.last().path, "/api/v1/insights/functions/resize");
    i.invocations(&InvocationsQuery {
        function: Some("resize".into()),
        errors: Some(true),
        min_duration_ms: Some(100),
        cursor: Some(9),
        ..InvocationsQuery::default()
    })
    .await
    .expect("invocations");
    let q = m.last();
    assert_eq!(q.route(), "/api/v1/insights/functions/invocations");
    assert_eq!(
        (
            q.q("errors").as_deref(),
            q.q("min_duration_ms").as_deref(),
            q.q("cursor").as_deref()
        ),
        (Some("true"), Some("100"), Some("9"))
    );
    i.invocation("42").await.expect("invocation");
    assert_eq!(m.last().path, "/api/v1/insights/functions/invocations/42");
}

#[tokio::test]
async fn insights_audit_search_related_and_summary() {
    let m = Mock::start(vec![
        ok(schema("AuditPageView")),
        ok(schema("AuditEntryView")),
        ok(schema("AuditRelatedView")),
        ok(schema("AuditSummaryView")),
    ])
    .await;
    let i = m.client().insights();
    i.audit(&AuditQuery {
        actor: Some("admin@acme.test".into()),
        action: Some("api_key.".into()),
        outcome: Some("failure".into()),
        q: Some("curl".into()),
        order: Some("asc".into()),
        cursor: Some(50),
        limit: Some(25),
        range: TimeRange::new("2026-01-01", "2026-01-31"),
        ..AuditQuery::default()
    })
    .await
    .expect("audit");
    let q = m.last();
    assert_eq!(q.route(), "/api/v1/insights/audit");
    assert_eq!(
        (
            q.q("actor").as_deref(),
            q.q("action").as_deref(),
            q.q("outcome").as_deref(),
            q.q("q").as_deref(),
            q.q("order").as_deref(),
            q.q("cursor").as_deref(),
            q.q("limit").as_deref()
        ),
        (
            Some("admin@acme.test"),
            Some("api_key."),
            Some("failure"),
            Some("curl"),
            Some("asc"),
            Some("50"),
            Some("25")
        )
    );
    assert_eq!(
        (q.q("from").as_deref(), q.q("to").as_deref()),
        (Some("2026-01-01"), Some("2026-01-31"))
    );
    i.audit_entry("17").await.expect("entry");
    i.audit_related("17", Some(60), Some(10)).await.expect("related");
    let q = m.last();
    assert_eq!(
        (q.route(), q.q("window_secs").as_deref(), q.q("limit").as_deref()),
        ("/api/v1/insights/audit/17/related", Some("60"), Some("10"))
    );
    i.audit_summary(&TimeRange::new("2026-01-01", "2026-02-01"))
        .await
        .expect("summary");
    assert_eq!(m.last().route(), "/api/v1/insights/audit/summary");
}

#[tokio::test]
async fn insights_audit_export_streams_csv_and_jsonl() {
    let csv = "id,at,actor\r\n1,2026-01-01T00:00:00Z,a\r\n2,2026-01-01T00:00:01Z,b\r\n";
    let jsonl = "{\"id\":1}\n{\"id\":2}\n{\"id\":3}";
    let m = Mock::start(vec![
        text("text/csv", csv),
        text("application/x-ndjson", jsonl),
        text("text/csv", csv),
        err(403, "forbidden"),
    ])
    .await;
    let i = m.client().insights();

    let mut export = i
        .export_audit(
            &AuditQuery {
                actor: Some("a".into()),
                ..AuditQuery::default()
            },
            ExportFormat::Csv,
            Some(1000),
        )
        .await
        .expect("csv export");
    let q = m.last();
    assert_eq!(q.route(), "/api/v1/insights/audit/export");
    assert_eq!(
        (
            q.q("format").as_deref(),
            q.q("max_rows").as_deref(),
            q.q("actor").as_deref()
        ),
        (Some("csv"), Some("1000"), Some("a"))
    );
    assert_eq!(export.content_type(), Some("text/csv"));
    let mut lines = Vec::new();
    while let Some(l) = export.line().await.expect("line") {
        lines.push(l);
    }
    assert_eq!(
        lines,
        ["id,at,actor", "1,2026-01-01T00:00:00Z,a", "2,2026-01-01T00:00:01Z,b"],
        "CRLF is stripped"
    );

    let mut export = i
        .export_audit(&AuditQuery::default(), ExportFormat::Jsonl, None)
        .await
        .expect("jsonl export");
    assert_eq!(m.last().q("format").as_deref(), Some("jsonl"));
    assert!(m.last().q("max_rows").is_none());
    let mut ids = Vec::new();
    while let Some(l) = export.line().await.expect("line") {
        let v: serde_json::Value = serde_json::from_str(&l).expect("one JSON object per line");
        ids.push(v["id"].as_i64().expect("id"));
    }
    assert_eq!(ids, [1, 2, 3], "a last line without a line break is still returned");

    let whole = i
        .export_audit(&AuditQuery::default(), ExportFormat::Csv, None)
        .await
        .expect("export")
        .text()
        .await
        .expect("text");
    assert_eq!(whole, csv);
    let e = i
        .export_audit(&AuditQuery::default(), ExportFormat::Csv, None)
        .await
        .err()
        .expect("forbidden");
    assert!(e.is_forbidden());
}
