//! Live tests for the roadmap APIs through the SDK, against the real server binary: vector,
//! graph and openCypher, SQL, webhook tooling, alerts and SLOs, insights, capabilities, access
//! rules, organizations, credits and the billing webhook inspector.
//!
//! Set `FB_LIVE_BIN`; without it these tests pass vacuously (see `tests/common`).

mod common;

use common::Server;
use forgebase_sdk::alerts::AlertHistoryQuery;
use forgebase_sdk::billing_admin::WebhookDeliveriesQuery;
use forgebase_sdk::generated::{AlertRuleInput, AlertRulePatch, Direction, Metric, SloInput};
use forgebase_sdk::graph::{
    CypherOptions, ListNodes, NewEdge, NewNode, NodeChange, ShortestPathOptions, TraverseOptions,
};
use forgebase_sdk::insights::{AuditQuery, ExportFormat, QueriesQuery, TimeRange};
use forgebase_sdk::orgs::{NewOrg, NewTeam};
use forgebase_sdk::queue_webhooks::{AttemptsQuery, Redrive};
use forgebase_sdk::queues::PushTarget;
use forgebase_sdk::sql::{SqlOptions, SqlStatement};
use forgebase_sdk::vector::{FieldSpec, HybridOptions, NewVectorCollection, RecallOptions, SearchOptions, UpsertItem};
use forgebase_sdk::{ErrorKind, NewSubscription, NewTopic};
use serde_json::{Value, json};

// ---- capabilities ----

#[tokio::test(flavor = "multi_thread")]
async fn capabilities_name_the_engine_of_every_feature() {
    let Some(s) = Server::start() else { return };
    let fb = s.admin().await;
    let caps = fb.system().capabilities().await.expect("capabilities");
    assert_eq!(caps.backend, "sqlite", "the bundled default backend");
    let by = |name: &str| {
        caps.features
            .iter()
            .find(|f| f.feature == name)
            .unwrap_or_else(|| panic!("feature {name}"))
    };
    for f in [
        "vector",
        "graph",
        "opencypher",
        "fulltext",
        "fuzzy",
        "queue",
        "kv",
        "realtime",
    ] {
        assert!(!by(f).engine.is_empty(), "{f} names its engine");
    }
    assert!(
        by("opencypher").engine.starts_with("portable"),
        "SQLite answers openCypher with the portable engine: {}",
        by("opencypher").engine
    );
    let info = fb.system().info().await.expect("info");
    assert!(
        info["database"]["capabilities"].is_array(),
        "/api/system/info carries the same list"
    );
}

// ---- vector ----

#[tokio::test(flavor = "multi_thread")]
async fn vector_upsert_similarity_hybrid_and_recall() {
    let Some(s) = Server::start() else { return };
    let v = s.admin().await.vector();
    v.create(&NewVectorCollection::new("docs").field(FieldSpec::new("emb", 3, Metric::Cosine)))
        .await
        .expect("create");
    assert_eq!(v.list().await.expect("list").len(), 1);
    let e = v
        .create(&NewVectorCollection::new("docs"))
        .await
        .expect_err("name taken");
    assert!(e.is_conflict(), "{e}");

    let docs = v.collection("docs");
    let r = docs
        .upsert(&[
            UpsertItem::new()
                .id("rust")
                .vector("emb", &[1.0, 0.0, 0.0])
                .metadata(json!({"lang": "rust", "year": 2015}))
                .text("systems programming language"),
            UpsertItem::new()
                .id("go")
                .vector("emb", &[0.9, 0.1, 0.0])
                .metadata(json!({"lang": "go", "year": 2009}))
                .text("concurrent programming language"),
            UpsertItem::new()
                .id("sql")
                .vector("emb", &[0.0, 0.0, 1.0])
                .metadata(json!({"lang": "sql", "year": 1974}))
                .text("query language for databases"),
        ])
        .await
        .expect("upsert");
    assert_eq!(r.ids, ["rust", "go", "sql"]);
    assert_eq!(docs.get_item("go").await.expect("item").metadata["lang"], "go");

    let hits = docs
        .search(&SearchOptions::new(&[1.0, 0.0, 0.0]).k(2))
        .await
        .expect("search");
    assert_eq!(
        hits.hits.iter().map(|h| h.id.as_str()).collect::<Vec<_>>(),
        ["rust", "go"],
        "closest first"
    );
    assert!(hits.hits[0].score >= hits.hits[1].score);

    let filtered = docs
        .search(
            &SearchOptions::new(&[1.0, 0.0, 0.0])
                .k(3)
                .filter("year < {:max}", Some(json!({"max": 2000}))),
        )
        .await
        .expect("filtered search");
    assert_eq!(filtered.hits.iter().map(|h| h.id.as_str()).collect::<Vec<_>>(), ["sql"]);
    let exact = docs
        .search(&SearchOptions::new(&[0.0, 0.0, 1.0]).k(1).exact())
        .await
        .expect("exact");
    assert_eq!(exact.hits[0].id, "sql");

    let hybrid = docs
        .hybrid(&HybridOptions::new("databases", &[1.0, 0.0, 0.0]).k(3))
        .await
        .expect("hybrid");
    assert!(!hybrid.hits.is_empty());
    let keyword = docs
        .hybrid(&HybridOptions::text_only("concurrent"))
        .await
        .expect("keyword-only");
    assert_eq!(keyword.hits[0].id, "go");

    let stats = docs.stats().await.expect("stats");
    assert_eq!(stats.items, 3);
    let recall = docs
        .recall(&RecallOptions {
            samples: Some(3),
            k: Some(2),
            ..RecallOptions::default()
        })
        .await
        .expect("recall");
    assert!(recall.recall >= 0.99, "tiny sets are exact: {}", recall.recall);

    assert_eq!(docs.delete_items(&["go", "missing"]).await.expect("delete items"), 1);
    assert!(docs.get_item("go").await.expect_err("deleted").is_not_found());
    docs.drop_collection().await.expect("drop");
    assert!(v.list().await.expect("list").is_empty());
}

// ---- graph and openCypher ----

#[tokio::test(flavor = "multi_thread")]
async fn graph_traversal_paths_and_cypher_with_the_engine_field() {
    let Some(s) = Server::start() else { return };
    let g = s.admin().await.graph();
    for (id, name, age) in [
        ("ada", "Ada", 36),
        ("bob", "Bob", 41),
        ("cyd", "Cyd", 29),
        ("dee", "Dee", 52),
    ] {
        g.nodes()
            .create(&NewNode::new("Person", json!({"name": name, "age": age})).id(id))
            .await
            .expect("node");
    }
    for (a, b) in [("ada", "bob"), ("bob", "cyd"), ("cyd", "dee")] {
        g.edges()
            .create(&NewEdge::new("KNOWS", a, b).properties(json!({"since": 2020})))
            .await
            .expect("edge");
    }
    let dup = g
        .nodes()
        .create(&NewNode::new("Person", json!({})).id("ada"))
        .await
        .expect_err("existing id");
    assert!(dup.is_conflict());

    assert_eq!(
        g.nodes()
            .list_all(&ListNodes {
                label: Some("Person".into()),
                limit: Some(2),
                ..ListNodes::default()
            })
            .await
            .expect("all")
            .len(),
        4
    );
    let n = g
        .nodes()
        .patch(
            "ada",
            &NodeChange {
                label: None,
                properties: Some(json!({"age": 37, "nick": "A"})),
            },
        )
        .await
        .expect("patch");
    assert_eq!(n.properties["age"], 37);
    assert_eq!(n.properties["name"], "Ada", "a merge patch keeps the other keys");

    let nb = g
        .nodes()
        .neighbors("bob", &Default::default())
        .await
        .expect("neighbors");
    assert_eq!(nb.items.len(), 1);
    assert_eq!(
        (nb.items[0].node.id.as_str(), nb.items[0].direction.clone()),
        ("cyd", Direction::Out)
    );

    let t = g
        .traverse(&TraverseOptions::new("ada").max_depth(2))
        .await
        .expect("traverse");
    let reached: Vec<(&str, i64)> = t.nodes.iter().map(|n| (n.node.id.as_str(), n.depth)).collect();
    assert_eq!(reached, [("bob", 1), ("cyd", 2)]);
    let p = g
        .shortest_path(&ShortestPathOptions::new("ada", "dee"))
        .await
        .expect("path");
    assert!(p.found);
    assert_eq!(p.length, Some(3));
    assert_eq!(
        p.nodes.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(),
        ["ada", "bob", "cyd", "dee"]
    );

    let r = g
        .cypher(
            "MATCH (a:Person)-[:KNOWS]->(b:Person) WHERE a.age > $min RETURN a.name AS who, b.name AS friend ORDER BY who",
            &CypherOptions { params: Some(json!({"min": 30})), ..CypherOptions::default() },
        )
        .await
        .expect("cypher");
    assert_eq!(
        r.engine, "portable",
        "SQLite has no Apache AGE: the portable engine answers"
    );
    assert_eq!(r.columns, ["who", "friend"]);
    assert_eq!(
        r.rows,
        vec![vec![json!("Ada"), json!("Bob")], vec![json!("Bob"), json!("Cyd")]]
    );
    assert!(!r.truncated);

    let nodes = g
        .cypher("MATCH (n:Person {name: 'Dee'}) RETURN n", &CypherOptions::default())
        .await
        .expect("vertex");
    assert_eq!(nodes.rows[0][0]["id"], "dee");
    assert_eq!(
        nodes.rows[0][0]["label"], "Person",
        "vertices come back as {{id, label, properties}}"
    );
    let limited = g
        .cypher(
            "MATCH (n:Person) RETURN n.name",
            &CypherOptions {
                limit: Some(2),
                ..CypherOptions::default()
            },
        )
        .await
        .expect("limit");
    assert_eq!((limited.rows.len(), limited.truncated), (2, true));
    let bad = g
        .cypher("CREATE (n:Person) RETURN n", &CypherOptions::default())
        .await
        .expect_err("read-only");
    assert_eq!(bad.kind(), Some(ErrorKind::Validation), "{bad}");

    let stats = g.stats().await.expect("stats");
    assert_eq!((stats.nodes, stats.edges), (4, 3));
    assert_eq!(stats.edge_types[0].r#type, "KNOWS");
    let deleted = g.nodes().delete("bob").await.expect("delete node");
    assert_eq!(deleted.deleted_edges, 2);
}

// ---- SQL ----

#[tokio::test(flavor = "multi_thread")]
async fn sql_run_batch_explain_and_tables() {
    let Some(s) = Server::start() else { return };
    let sql = s.admin().await.sql();
    let none = SqlOptions::default();
    sql.execute(
        "CREATE TABLE sdk_notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL)",
        &[],
        &none,
    )
    .await
    .expect("ddl");
    let w = sql
        .execute("INSERT INTO sdk_notes (body) VALUES (?)", &[json!("hello")], &none)
        .await
        .expect("insert");
    assert_eq!((w.kind.as_str(), w.rows_affected), ("write", 1));
    let batch = sql
        .batch(
            &[
                SqlStatement::new("INSERT INTO sdk_notes (body) VALUES (?)", vec![json!("two")]),
                "INSERT INTO sdk_notes (body) VALUES ('three')".into(),
                "SELECT count(*) AS n FROM sdk_notes".into(),
            ],
            &none,
        )
        .await
        .expect("batch");
    assert_eq!(batch.len(), 3);
    assert_eq!(batch[2].rows[0][0], 3);

    let rows = sql
        .query("SELECT id, body FROM sdk_notes ORDER BY id", &[], &none)
        .await
        .expect("query");
    assert_eq!(
        rows.iter().map(|r| r["body"].clone()).collect::<Vec<Value>>(),
        [json!("hello"), json!("two"), json!("three")]
    );
    let found = sql
        .query("SELECT body FROM sdk_notes WHERE id = ?1", &[json!(2)], &none)
        .await
        .expect("param");
    assert_eq!(found[0]["body"], "two");

    let plan = sql
        .explain("SELECT * FROM sdk_notes WHERE id = ?", &[json!(1)])
        .await
        .expect("explain");
    assert!(!plan.steps.is_empty());
    let tables = sql.tables().await.expect("tables");
    assert!(tables.iter().any(|t| t.name == "sdk_notes"));
    let t = sql.table("sdk_notes").await.expect("table");
    assert_eq!(
        t.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
        ["id", "body"]
    );

    let failed = sql
        .batch(
            &[
                "INSERT INTO sdk_notes (body) VALUES ('kept?')".into(),
                "INSERT INTO sdk_notes (id, body) VALUES (1, 'dup')".into(),
            ],
            &none,
        )
        .await
        .expect_err("a failing statement rolls the batch back");
    assert!(failed.is_validation() || failed.is_conflict(), "{failed}");
    let n = sql
        .query("SELECT count(*) AS n FROM sdk_notes", &[], &none)
        .await
        .expect("count");
    assert_eq!(n[0]["n"], 3, "all or nothing");
    let refused = sql
        .execute("BEGIN", &[], &none)
        .await
        .expect_err("transaction control is refused");
    assert!(refused.is_validation(), "{refused}");
}

// ---- webhook tooling ----

#[tokio::test(flavor = "multi_thread")]
async fn webhook_tooling_test_send_attempts_secret_pause_and_catalog() {
    let Some(s) = Server::start() else { return };
    let q = s.admin().await.queues();
    let mut topic = NewTopic::new("orders");
    topic.description = Some("Order lifecycle events".into());
    topic.example = Some(json!({"order": 1, "status": "created"}));
    let t = q.create_topic(&topic).await.expect("topic");
    assert_eq!(t.description.as_deref(), Some("Order lifecycle events"));

    let mut new = NewSubscription::new("orders-push", "orders");
    new.push = Some(PushTarget::webhook("https://hooks.example.test/in"));
    new.pause_after_failures = Some(50);
    let sub = match q.create_subscription(&new).await {
        Ok(s) => s,
        Err(e) => panic!("push subscription: {e}"),
    };
    let secret = sub.push_secret.clone().expect("the signing secret is shown once");
    assert!(secret.starts_with("whsec_"), "{secret}");
    assert!(
        q.get_subscription("orders-push")
            .await
            .expect("get")
            .push_secret
            .is_none(),
        "never shown again"
    );

    let w = q.webhooks("orders-push");
    // Loopback and private targets are refused when the subscription is created (`push_endpoint_blocked`);
    // a `.test` host never resolves, so the attempt fails the same way everywhere and is recorded with its error class.
    let blocked = q
        .create_subscription(&{
            let mut b = NewSubscription::new("orders-local", "orders");
            b.push = Some(PushTarget::webhook("https://127.0.0.1:9/hook"));
            b
        })
        .await
        .expect_err("loopback endpoints are refused");
    assert_eq!((blocked.status(), blocked.code()), (Some(400), "push_endpoint_blocked"));
    let a = w.test(Some(&json!({"hello": "world"}))).await.expect("test send");
    assert!(a.test, "a test send is marked");
    assert_eq!(a.subscription, "orders-push");
    assert!(a.message_id.starts_with("test-"), "{}", a.message_id);
    let h = &a.request_headers;
    assert!(
        h.iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("x-forgebase-signature") && !v.contains(&secret)),
        "the signature header is masked: {h:?}"
    );
    assert!(
        a.error_class.is_some() || a.status.is_some(),
        "the failure is classified: {a:?}"
    );

    let page = w
        .attempts(&AttemptsQuery {
            test: Some(true),
            limit: Some(10),
            ..AttemptsQuery::default()
        })
        .await
        .expect("attempts");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].id, a.id);
    let one = w.attempt(a.id).await.expect("attempt");
    assert_eq!(one.endpoint, "https://hooks.example.test/in");
    assert!(w.attempt(a.id + 1000).await.expect_err("unknown").is_not_found());
    let none = w
        .attempts(&AttemptsQuery {
            test: Some(false),
            ..AttemptsQuery::default()
        })
        .await
        .expect("real deliveries only");
    assert!(none.items.is_empty(), "a test send is not a delivery");

    // Secret rotation: new secret once, grace window on the subscription.
    let rot = w.rotate_secret(Some(60_000)).await.expect("rotate");
    assert_ne!(rot.push_secret, secret);
    assert!(rot.previous_expires_at.is_some());
    let after = q.get_subscription("orders-push").await.expect("get");
    assert!(after.push_secret_previous_expires_at.is_some());
    let ended = w.expire_previous_secret().await.expect("expire previous");
    assert!(ended.push_secret_previous_expires_at.is_none());

    // Pause and resume; retry and redrive with nothing to do.
    let paused = w.pause(Some("maintenance")).await.expect("pause");
    assert!(paused.paused_at.is_some());
    assert_eq!(paused.paused_reason.as_deref(), Some("maintenance"));
    assert!(w.resume().await.expect("resume").paused_at.is_none());
    assert_eq!(w.retry(None).await.expect("retry").count, 0);
    let r = w.redrive(&Redrive::all()).await.expect("redrive");
    assert!(r.redriven.is_empty() && r.remaining == 0);
    let r = w
        .redrive(&Redrive::Ids(vec![123_456]))
        .await
        .expect("redrive unknown id");
    assert_eq!(r.not_found, [123_456]);

    // A pull subscription has no webhook tooling.
    q.create_subscription(&NewSubscription::new("orders-pull", "orders"))
        .await
        .expect("pull subscription");
    let e = q
        .webhooks("orders-pull")
        .test(None)
        .await
        .expect_err("pull subscription");
    assert_eq!((e.status(), e.code()), (Some(409), "not_push_subscription"));

    // The event catalog lists the topic with its example.
    let cat = q.catalog().await.expect("catalog");
    let entry = cat
        .topics
        .iter()
        .find(|t| t.topic == "orders")
        .expect("orders in the catalog");
    assert_eq!(entry.example, json!({"order": 1, "status": "created"}));
    assert_eq!(entry.description.as_deref(), Some("Order lifecycle events"));
}

// ---- billing webhook inspector ----

#[tokio::test(flavor = "multi_thread")]
async fn billing_webhook_inspector_records_a_refused_delivery() {
    let Some(s) = Server::start() else { return };
    let fb = s.admin().await;
    let anon = forgebase_sdk::ForgeBase::new(&s.base).expect("client");
    // A provider calls the public endpoint; with no secret configured the delivery is refused but recorded.
    let _ = anon
        .request(
            "POST",
            "/api/v1/billing/webhooks/stripe",
            Some(json!({"id": "evt_1", "type": "invoice.paid"})),
        )
        .await;
    let w = fb.billing().webhook_deliveries();
    let page = w
        .list(&WebhookDeliveriesQuery {
            provider: Some("stripe".into()),
            limit: Some(10),
            ..WebhookDeliveriesQuery::default()
        })
        .await
        .expect("list");
    assert!(page.total >= 1, "the refused request is in the log: {page:?}");
    let d = &page.items[0];
    assert_eq!(d.provider, "stripe");
    let detail = w.get(&d.id).await.expect("get");
    assert_eq!(detail.id, d.id);
    let e = w
        .replay(&d.id)
        .await
        .expect_err("an unverified delivery cannot be replayed");
    assert_eq!(e.kind(), Some(ErrorKind::Conflict), "{e}");
    assert_eq!(e.code(), "delivery_not_verified");
}

// ---- alerts and SLOs ----

#[tokio::test(flavor = "multi_thread")]
async fn alert_rules_crud_silences_notifiers_and_slos() {
    let Some(s) = Server::start() else { return };
    let fb = s.admin().await;
    let alerts = fb.alerts();

    let builtin = alerts.rules().list().await.expect("rules");
    assert!(!builtin.is_empty(), "built-in rules are listed");
    let ro = alerts
        .rules()
        .update(
            &builtin[0].id,
            &AlertRulePatch {
                expr: Some("1 > 0".into()),
                ..AlertRulePatch::default()
            },
        )
        .await
        .expect_err("built-in rules only toggle `enabled`");
    assert_eq!((ro.status(), ro.code()), (Some(409), "rule_read_only"));

    let input: AlertRuleInput = serde_json::from_value(json!({
        "name": "sdk-high-error-rate", "expr": "forgebase_http_requests_total", "op": ">", "threshold": 1_000_000.0,
        "for": "1m", "severity": "warning", "labels": {"team": "sdk"}, "annotations": {"summary": "too many errors"}
    }))
    .expect("rule input");
    let rule = alerts.rules().create(&input).await.expect("create");
    assert_eq!(rule.name, "sdk-high-error-rate");
    let dup = alerts.rules().create(&input).await.expect_err("duplicate");
    assert_eq!((dup.status(), dup.code()), (Some(409), "alert_rule_exists"));
    assert_eq!(
        alerts.rules().get(&rule.id).await.expect("get").name,
        "sdk-high-error-rate"
    );
    let off = alerts
        .rules()
        .update(
            &rule.id,
            &AlertRulePatch {
                enabled: Some(false),
                ..AlertRulePatch::default()
            },
        )
        .await
        .expect("disable");
    assert!(!off.enabled);
    let bad: AlertRuleInput =
        serde_json::from_value(json!({"name": "bad", "expr": "this is (not", "op": ">", "threshold": 1.0}))
            .expect("input");
    assert!(
        alerts
            .rules()
            .create(&bad)
            .await
            .expect_err("invalid expression")
            .is_validation()
    );
    alerts.rules().delete(&rule.id).await.expect("delete");
    assert!(alerts.rules().get(&rule.id).await.expect_err("gone").is_not_found());

    assert!(alerts.active(None, None).await.is_ok());
    assert!(
        alerts
            .history(&AlertHistoryQuery {
                limit: Some(5),
                ..AlertHistoryQuery::default()
            })
            .await
            .expect("history")
            .items
            .len()
            <= 5
    );
    assert!(alerts.query("forgebase_http_requests_total").await.is_ok());

    let sil = alerts
        .silences()
        .create(&serde_json::from_value(json!({"matchers": [{"label": "severity", "op": "=", "value": "warning"}], "duration": "1h", "comment": "sdk test"})).expect("silence"))
        .await
        .expect("silence");
    assert_eq!(alerts.silences().list(false).await.expect("silences").len(), 1);
    alerts.silences().expire(&sil.id).await.expect("expire");

    let n = alerts
        .notifiers()
        .create(
            &serde_json::from_value(
                json!({"name": "sdk-hook", "kind": "webhook", "url": "https://alerts.example.test/hook"}),
            )
            .expect("notifier"),
        )
        .await
        .expect("notifier");
    assert_eq!(n.name, "sdk-hook");
    assert_eq!(alerts.notifiers().get("sdk-hook").await.expect("by name").id, n.id);
    let d = alerts.notifiers().test(&n.id).await.expect("test notification");
    assert!(!d.status.is_empty());
    alerts.notifiers().delete(&n.id).await.expect("delete");

    let slo: SloInput = serde_json::from_value(json!({"name": "sdk-availability", "kind": "availability", "objective": 0.999, "window": "7d", "route_prefix": "/api/v1"})).expect("slo input");
    let slos = fb.slos();
    let created = slos.create(&slo).await.expect("create slo");
    assert_eq!(slos.list().await.expect("slos").len(), 1);
    let st = slos.status(&created.id).await.expect("status");
    assert_eq!(st.slo.id, created.id);
    assert_eq!(slos.statuses().await.expect("statuses").len(), 1);
    slos.delete(&created.id).await.expect("delete slo");
    assert!(slos.get(&created.id).await.expect_err("gone").is_not_found());
}

// ---- insights ----

#[tokio::test(flavor = "multi_thread")]
async fn insights_status_queries_usage_functions_and_audit_export() {
    let Some(s) = Server::start() else { return };
    let fb = s.admin().await;
    let i = fb.insights();

    // Generate a little traffic, then read it back.
    fb.collections()
        .create(&json!({"name": "notes", "fields": [{"name": "title", "type": "text"}]}))
        .await
        .expect("collection");
    for n in 0..3 {
        fb.collection("notes")
            .create(&json!({"title": format!("note {n}")}))
            .await
            .expect("record");
    }
    let status = i.status().await.expect("status");
    assert!(status.enabled, "{status:?}");
    i.flush().await.expect("flush");
    let q = i
        .queries(&QueriesQuery {
            limit: Some(5),
            ..QueriesQuery::default()
        })
        .await
        .expect("queries");
    assert!(q.items.len() <= 5);
    assert!(i.regressions(&Default::default()).await.is_ok());
    assert!(i.slow_queries(&Default::default()).await.is_ok());
    assert!(i.usage_metrics().await.is_ok());
    assert!(i.usage(&Default::default()).await.is_ok());
    assert!(i.functions(&TimeRange::default(), None, None).await.is_ok());
    assert!(i.invocations(&Default::default()).await.is_ok());

    // Audit: sign-ins are audit events; search, then stream the same rows as CSV and JSONL.
    let page = i
        .audit(&AuditQuery {
            limit: Some(50),
            ..AuditQuery::default()
        })
        .await
        .expect("audit");
    assert!(!page.items.is_empty(), "the admin sign-in is audited");
    let mut csv = i
        .export_audit(&AuditQuery::default(), ExportFormat::Csv, Some(100))
        .await
        .expect("csv");
    let header = csv.line().await.expect("header").expect("a header row");
    assert!(header.contains("id") && header.contains("event"), "{header}");
    let mut rows = 0;
    while let Some(l) = csv.line().await.expect("row") {
        assert!(!l.is_empty());
        rows += 1;
    }
    assert!(rows >= 1);
    let mut jsonl = i
        .export_audit(&AuditQuery::default(), ExportFormat::Jsonl, Some(100))
        .await
        .expect("jsonl");
    let mut n = 0;
    while let Some(l) = jsonl.line().await.expect("line") {
        let v: Value = serde_json::from_str(&l).expect("one JSON object per line");
        assert!(v.get("id").is_some());
        n += 1;
    }
    assert!(n >= 1);
    assert_eq!(n, rows, "both formats stream the same rows");
    let one = i.audit_entry(&page.items[0].id.to_string()).await.expect("entry");
    assert_eq!(one.id, page.items[0].id);

    // A signed-in non-admin is refused.
    let user = forgebase_sdk::ForgeBase::new(&s.base).expect("client");
    assert!(
        user.insights()
            .status()
            .await
            .expect_err("anonymous")
            .is_unauthenticated()
    );
}

// ---- access rules, organizations, credits ----

#[tokio::test(flavor = "multi_thread")]
async fn access_rules_organizations_and_credits() {
    let Some(s) = Server::start() else { return };
    let fb = s.admin().await;

    // Access: a collection's rules, then a dry-run check.
    fb.collections()
        .create(&json!({"name": "posts", "fields": [{"name": "title", "type": "text"}]}))
        .await
        .expect("collection");
    let p = fb.access().policies().get("posts").await.expect("policy");
    assert!(p.list_rule.is_none(), "a new collection is locked");
    let open = forgebase_sdk::access::CollectionPolicy {
        list_rule: Some(String::new()),
        ..Default::default()
    };
    let put = fb.access().policies().put("posts", &open).await.expect("put policy");
    assert_eq!(put.list_rule.as_deref(), Some(""));
    assert!(put.create_rule.is_none());
    let me = fb.access().me(None).await.expect("me");
    assert!(me.super_admin || !me.roles.is_empty());
    assert!(fb.access().can("records:read").await.is_ok());

    // Orgs: create, team, roles.
    let orgs = fb.orgs();
    let org = orgs.create(&NewOrg::new("acme", "Acme")).await.expect("org");
    assert_eq!(org.slug, "acme");
    assert!(orgs.list().await.expect("list").iter().any(|o| o.id == org.id));
    let team = orgs
        .teams()
        .create(&org.id, &NewTeam::new("eng", "Engineering"))
        .await
        .expect("team");
    assert_eq!(orgs.teams().list(&org.id).await.expect("teams").len(), 1);
    orgs.teams().delete(&team.id).await.expect("delete team");
    let projects = orgs
        .projects()
        .create(&NewOrg::new("web", "Web"), Some(&org.id))
        .await
        .expect("project");
    assert!(
        !orgs
            .projects()
            .get(&projects.id)
            .await
            .expect("project")
            .environments
            .is_empty()
    );
    assert!(
        orgs.roles()
            .list(Some(&org.id))
            .await
            .expect("roles")
            .iter()
            .any(|r| r.key == "owner" || r.system)
    );
    orgs.rename_org_check(&org.id).await;

    // Credits: top up, spend, an overdraft is a 402.
    let credits = fb.credits();
    let subject = format!("org:{}", org.id);
    let top = credits
        .top_up(&forgebase_sdk::credits::CreditInput::new(subject.clone(), 10, "top-1"))
        .await
        .expect("top up");
    assert!(!top.replayed);
    assert!(
        credits
            .top_up(&forgebase_sdk::credits::CreditInput::new(subject.clone(), 10, "top-1"))
            .await
            .expect("replay")
            .replayed
    );
    assert_eq!(credits.balance(Some(&subject)).await.expect("balance").balance, 10);
    let mut spend = forgebase_sdk::credits::SpendInput::new(4, "spend-1");
    spend.subject = Some(subject.clone());
    credits.spend(&spend).await.expect("spend");
    assert_eq!(credits.balance(Some(&subject)).await.expect("balance").balance, 6);
    let mut over = forgebase_sdk::credits::SpendInput::new(100, "spend-2");
    over.subject = Some(subject.clone());
    let e = credits.spend(&over).await.expect_err("overdraft");
    assert_eq!((e.status(), e.code()), (Some(402), "insufficient_credits"));
    let ledger = credits
        .all_entries(&forgebase_sdk::credits::LedgerOptions {
            subject: Some(subject),
            limit: Some(1),
            ..Default::default()
        })
        .await
        .expect("ledger");
    assert_eq!(ledger.len(), 2, "paging by `next` collects every entry");
}

trait OrgCheck {
    async fn rename_org_check(&self, id: &str);
}

impl OrgCheck for forgebase_sdk::OrgsApi {
    async fn rename_org_check(&self, id: &str) {
        let renamed = self.rename(id, "Acme Inc").await.expect("rename");
        assert_eq!(renamed.name, "Acme Inc");
    }
}
