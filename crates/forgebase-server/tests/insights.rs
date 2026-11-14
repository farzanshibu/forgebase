//! Operations insights (ADR-0036) over HTTP against the real server:
//! statement fingerprints, the redacted slow statement log and regression
//! detection; per-tenant usage rollup and cost estimates; function profiles;
//! audit search, export and related events; and administrator-only access.
//! SQLite always; PostgreSQL with `FORGEBASE_TEST_BACKEND=postgres`.
//! Binds `127.0.0.1:0`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod security_common;

use std::time::Duration;

use forgebase_billing::{MeterBasis, PriceInput, PriceType, ProductInput, ProductKind};
use forgebase_core::sketch::Sketch;
use forgebase_credits::{RateBasis, RateInput};
use forgebase_data::AccessContext;
use forgebase_db::stmtstats::{Drained, StatementAggregate, fingerprint_id, normalize};
use forgebase_db::{Backend, BackendKind, Statement, Value};
use forgebase_entitlements::{FeatureInput, FeatureKind};
use security_common::{Env, Resp};
use serde_json::{Value as Json, json};

/// The statement statistics are process-wide, as in a real server: one test
/// at a time owns them (a flush drains them).
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn backend_name(env: &Env) -> &'static str {
    if env.server.database().kind() == BackendKind::Postgres {
        "postgres"
    } else {
        "sqlite"
    }
}

async fn get(env: &Env, token: &str, path: &str) -> Resp {
    env.as_(token, "GET", path, &[], None).await
}

async fn ok_json(env: &Env, token: &str, path: &str) -> Json {
    let r = get(env, token, path).await;
    assert_eq!(r.status, 200, "{path}: {}", r.text());
    r.json()
}

async fn sql(env: &Env, stmt: &str, params: Vec<Value>) {
    let db = env.server.database();
    let mut tx = db.begin(forgebase_db::TxMode::Normal).await.unwrap();
    tx.execute(&Statement::new(stmt, params)).await.unwrap();
    tx.commit().await.unwrap();
}

// ---------------------------------------------------------------------------
// Access
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_insights_route_is_for_administrators_only() {
    let _serial = SERIAL.lock().await;
    let env = Env::new().await;
    let admin = env.admin("insights-admin@example.com").await;
    let (user, _) = env.user("insights-user@example.com").await;
    let paths = [
        "/api/v1/insights/status",
        "/api/v1/insights/queries",
        "/api/v1/insights/queries/regressions",
        "/api/v1/insights/slow-queries",
        "/api/v1/insights/usage",
        "/api/v1/insights/usage/top?metric=cost",
        "/api/v1/insights/usage/metrics",
        "/api/v1/insights/usage/series?scope=env:x",
        "/api/v1/insights/functions",
        "/api/v1/insights/functions/invocations",
        "/api/v1/insights/audit",
        "/api/v1/insights/audit/summary",
        "/api/v1/insights/audit/export",
    ];
    for p in paths {
        let anon = env.req("GET", p, &[], None).await;
        assert_eq!(anon.status, 401, "anonymous {p}: {}", anon.text());
        let u = get(&env, &user, p).await;
        assert_eq!(u.status, 403, "user {p}: {}", u.text());
        assert_eq!(u.code(), "forbidden", "{p}");
        let a = get(&env, &admin, p).await;
        assert_eq!(a.status, 200, "admin {p}: {}", a.text());
    }
    for p in ["/api/v1/insights/flush", "/api/v1/insights/usage/rollup"] {
        assert_eq!(env.req("POST", p, &[], None).await.status, 401, "{p}");
        assert_eq!(env.as_(&user, "POST", p, &[], None).await.status, 403, "{p}");
        assert_eq!(env.as_(&admin, "POST", p, &[], None).await.status, 200, "{p}");
    }
    // The documents are in the OpenAPI document with their operation ids.
    let doc = env.req("GET", "/api/openapi.json", &[], None).await.json();
    let op = &doc["paths"]["/api/v1/insights/queries/regressions"]["get"]["operationId"];
    assert_eq!(op, "insights_queries_regressions");
    env.shutdown().await;
}

// ---------------------------------------------------------------------------
// Statements
// ---------------------------------------------------------------------------

/// A statement that takes tens of milliseconds on both backends.
fn heavy(n: u32, literal: &str) -> String {
    format!(
        "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < {n}) \
         SELECT count(*), '{literal}' AS tag FROM c"
    )
}

#[tokio::test]
async fn statements_group_by_shape_and_slow_ones_keep_no_values() {
    let _serial = SERIAL.lock().await;
    let env = Env::with(|c| c.observability.insights.slow_query_ms = 15).await;
    let admin = env.admin("stmt-admin@example.com").await;
    let db = env.server.database();
    for (n, lit) in [
        (300_000, "hunter2-first"),
        (310_000, "hunter2-second"),
        (320_000, "hunter2-third"),
    ] {
        db.fetch_all(&Statement::raw(heavy(n, lit))).await.unwrap();
    }
    let f = env.as_(&admin, "POST", "/api/v1/insights/flush", &[], None).await;
    assert_eq!(f.status, 200, "{}", f.text());
    let list = ok_json(&env, &admin, "/api/v1/insights/queries?q=recursive&sort=total_time").await;
    let items = list["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "one shape for three literals: {list}");
    let s = &items[0];
    assert_eq!(s["calls"], 3);
    assert_eq!(s["errors"], 0);
    assert_eq!(s["backend"], backend_name(&env));
    let text = s["statement"].as_str().unwrap();
    assert!(text.starts_with("with recursive c(x) as (select ?"), "{text}");
    assert!(!text.contains("hunter2") && !text.contains("300000"), "{text}");
    assert!(s["p95_ms"].as_f64().unwrap() >= s["p50_ms"].as_f64().unwrap());
    assert!(s["total_ms"].as_f64().unwrap() > 0.0);

    // The detail of that shape has a time series.
    let fp = s["fingerprint"].as_str().unwrap();
    let detail = ok_json(&env, &admin, &format!("/api/v1/insights/queries/{fp}")).await;
    assert_eq!(detail["calls"], 3);
    assert!(!detail["series"].as_array().unwrap().is_empty());
    assert_eq!(
        get(&env, &admin, "/api/v1/insights/queries/0123456789abcdef")
            .await
            .status,
        404
    );

    // The slow statement log lists them, redacted, and never the values.
    let slow = get(&env, &admin, "/api/v1/insights/slow-queries?q=recursive").await;
    assert_eq!(slow.status, 200, "{}", slow.text());
    assert!(
        !slow.text().contains("hunter2"),
        "no literal value in the slow log: {}",
        slow.text()
    );
    let entries = slow.json()["items"].as_array().unwrap().clone();
    assert!(!entries.is_empty(), "{}", slow.text());
    assert!(entries.iter().all(|e| e["duration_ms"].as_f64().unwrap() >= 15.0));
    assert!(entries[0]["statement"].as_str().unwrap().contains("with recursive"));
    // The Prometheus counter by area is exported.
    let metrics = env.as_(&admin, "GET", "/metrics", &[], None).await;
    assert!(
        metrics.text().contains("forgebase_db_slow_statements_total{"),
        "{}",
        &metrics.text()[..200.min(metrics.text().len())]
    );
    env.shutdown().await;
}

fn aggregate(env: &Env, sql: &str, calls: u64, us: u64) -> StatementAggregate {
    let mut sketch = Sketch::new();
    for _ in 0..calls {
        sketch.record(us);
    }
    let backend = backend_name(env);
    StatementAggregate {
        fingerprint: fingerprint_id(backend, sql),
        text: normalize(sql),
        area: "other",
        backend,
        calls,
        errors: 0,
        writes: 0,
        slow: 0,
        total_us: calls * us,
        max_us: us,
        min_us: us,
        rows: calls,
        sketch,
    }
}

#[tokio::test]
async fn a_slowdown_is_reported_as_a_regression_against_the_baseline() {
    let _serial = SERIAL.lock().await;
    let env = Env::new().await;
    let admin = env.admin("regress-admin@example.com").await;
    let db = env.server.database();
    let probe = |n: u32| heavy(n, "probe");
    // Baseline: three hourly buckets from 3 hours ago, 60 runs each at 0.2 ms.
    let now = now_ms();
    for i in 0..3 {
        let d = Drained {
            aggregates: vec![aggregate(&env, &probe(1), 60, 200)],
            ..Drained::default()
        };
        env.server
            .insights()
            .ingest_statements(d, now - (3 - i) * 3_600_000 - 1_800_000, 60_000)
            .await
            .unwrap();
    }
    // Now: the same statement shape, 25 real runs, each tens of milliseconds.
    for i in 0..25 {
        db.fetch_all(&Statement::raw(probe(200_000 + i))).await.unwrap();
    }
    let r = ok_json(
        &env,
        &admin,
        "/api/v1/insights/queries/regressions?window=1800&baseline=43200&min_calls=20",
    )
    .await;
    let items = r["items"].as_array().unwrap();
    let fp = fingerprint_id(backend_name(&env), &probe(1));
    let hit = items
        .iter()
        .find(|i| i["fingerprint"] == fp.as_str() && i["kind"] == "latency")
        .unwrap_or_else(|| panic!("the slowdown is reported: {r}"));
    assert!(hit["ratio"].as_f64().unwrap() >= 2.0, "{hit}");
    assert!(hit["recent"]["calls"].as_u64().unwrap() >= 25);
    assert_eq!(hit["baseline"]["calls"], 180);
    assert!(hit["recent"]["p95_ms"].as_f64().unwrap() > hit["baseline"]["p95_ms"].as_f64().unwrap());
    // Too few runs, or a higher bar: not reported.
    let none = ok_json(
        &env,
        &admin,
        "/api/v1/insights/queries/regressions?window=1800&baseline=43200&min_calls=1000",
    )
    .await;
    assert!(none["items"].as_array().unwrap().is_empty());
    let none = ok_json(
        &env,
        &admin,
        "/api/v1/insights/queries/regressions?window=1800&baseline=43200&min_ratio=100000",
    )
    .await;
    assert!(
        none["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["kind"] != "latency" || i["fingerprint"] != fp.as_str())
    );
    // A window that ends before the slowdown sees the baseline only.
    let until = forgebase_insights::iso(now - 3_600_000);
    let before = ok_json(
        &env,
        &admin,
        &format!("/api/v1/insights/queries/regressions?window=1800&baseline=43200&until={until}"),
    )
    .await;
    assert!(
        before["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["fingerprint"] != fp.as_str())
    );
    env.shutdown().await;
}

// ---------------------------------------------------------------------------
// Usage
// ---------------------------------------------------------------------------

async fn seed_tenancy(env: &Env) {
    for (org, name) in [("org-a", "Acme"), ("org-b", "Beta")] {
        sql(
            env,
            "INSERT INTO _forgebase_orgs (id, realm_id, slug, name, created_by, created_at) VALUES (?1, 'default', ?1, ?2, 'test', 1)",
            vec![org.into(), name.into()],
        )
        .await;
    }
    for (proj, org) in [("proj-a", "org-a"), ("proj-b", "org-b")] {
        sql(
            env,
            "INSERT INTO _forgebase_projects (id, realm_id, org_id, slug, name, created_at) VALUES (?1, 'default', ?2, ?1, ?1, 1)",
            vec![proj.into(), org.into()],
        )
        .await;
    }
    for (e, proj) in [("env-a1", "proj-a"), ("env-a2", "proj-a"), ("env-b1", "proj-b")] {
        sql(
            env,
            "INSERT INTO _forgebase_environments (id, project_id, name, created_at) VALUES (?1, ?2, ?1, 1)",
            vec![e.into(), proj.into()],
        )
        .await;
    }
}

async fn seed_storage(env: &Env, tenant: &str, objects: &[(&str, i64)]) {
    let bucket = format!("bk-{tenant}");
    sql(
        env,
        "INSERT INTO _forgebase_storage_buckets (name, owner, created_at, updated_at, tenant) VALUES (?1, 'test', 1, 1, ?2)",
        vec![bucket.clone().into(), tenant.into()],
    )
    .await;
    for (i, (key, size)) in objects.iter().enumerate() {
        let sha = format!("{:064x}", (tenant.len() * 1000 + i) as u64 + size.unsigned_abs());
        sql(
            env,
            "INSERT INTO _forgebase_storage_blobs (sha256, size, refcount, created_at) VALUES (?1, ?2, 1, 1)",
            vec![sha.clone().into(), (*size).into()],
        )
        .await;
        sql(
            env,
            "INSERT INTO _forgebase_storage_objects (id, bucket, key, blob_sha, size, quota_subject, created_by, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'org:x', 'test', 1)",
            vec![
                format!("o-{tenant}-{i}").into(),
                bucket.clone().into(),
                (*key).into(),
                sha.into(),
                (*size).into(),
            ],
        )
        .await;
    }
}

async fn usage_event(env: &Env, subject: &str, feature: &str, units: i64, n: u32, at: i64) {
    sql(
        env,
        "INSERT INTO _forgebase_usage_events
             (id, at, subject, feature_key, scope_key, units, pack_units, overage_units, window_start, source, idempotency_key, actor)
         VALUES (?1, ?2, ?3, ?4, '', ?5, 0, 0, ?2, 'consume', ?1, 'test')",
        vec![format!("ev-{subject}-{feature}-{n}").into(), at.into(), subject.into(), feature.into(), units.into()],
    )
    .await;
}

fn row<'a>(report: &'a Json, id: &str) -> &'a Json {
    report["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == id)
        .unwrap_or_else(|| panic!("no row {id} in {report}"))
}

#[tokio::test]
async fn tenant_usage_matches_the_seeded_numbers_and_prices_come_from_the_catalog() {
    let _serial = SERIAL.lock().await;
    let env = Env::new().await;
    let admin = env.admin("usage-admin@example.com").await;
    let ctx = AccessContext::admin("root");
    seed_tenancy(&env).await;
    // Resources: storage (SQL), vectors (SQL), key-value (the engine), a queue topic with messages.
    seed_storage(&env, "env-a1", &[("a", 1000), ("b", 2500)]).await;
    seed_storage(&env, "env-a2", &[("c", 400)]).await;
    seed_storage(&env, "env-b1", &[("d", 90_000)]).await;
    sql(
        &env,
        "INSERT INTO _forgebase_vector_collections (tenant, name, created_at, updated_at) VALUES ('env-a1', 'docs', 'now', 'now')",
        vec![],
    )
    .await;
    let cid = env
        .server
        .database()
        .fetch_all(&Statement::raw(
            "SELECT id FROM _forgebase_vector_collections WHERE name = 'docs'",
        ))
        .await
        .unwrap()
        .rows[0][0]
        .as_i64()
        .unwrap();
    for i in 0..7 {
        sql(
            &env,
            "INSERT INTO _forgebase_vector_items (collection_id, id, created, updated) VALUES (?1, ?2, 'now', 'now')",
            vec![cid.into(), format!("v{i}").into()],
        )
        .await;
    }
    let kv = env.server.kv();
    let tenant_ctx = AccessContext::system().with_tenant("env-b1");
    for k in ["k1", "k2", "k3"] {
        kv.exec(
            &tenant_ctx,
            "app",
            &[b"SET".to_vec(), k.as_bytes().to_vec(), b"value".to_vec()],
        )
        .unwrap();
    }
    kv.flush().await.unwrap();
    sql(
        &env,
        "INSERT INTO _forgebase_queue_topics (name, retention_ms, max_message_bytes, dedup_window_ms, created_by, created_at, updated_at, tenant)
         VALUES ('orders', 86400000, 65536, 0, 'test', 1, 1, 'env-a1')",
        vec![],
    )
    .await;
    let now = now_ms();
    for i in 0..4 {
        sql(
            &env,
            "INSERT INTO _forgebase_queue_messages (topic, id, data, bytes, publisher, published_at) VALUES ('orders', ?1, 'x', 100, 'test', ?2)",
            vec![format!("m{i}").into(), now.into()],
        )
        .await;
    }
    // Metered usage: org-a 700 + 300 calls this month, org-b 50; a personal subject too.
    usage_event(&env, "org:org-a", "api.requests", 700, 1, now - 1000).await;
    usage_event(&env, "org:org-a", "api.requests", 300, 2, now - 2000).await;
    usage_event(&env, "org:org-b", "api.requests", 50, 3, now - 3000).await;
    usage_event(&env, "user:users/ann", "api.requests", 11, 4, now - 4000).await;
    // Last month's event: outside the period.
    usage_event(&env, "org:org-a", "api.requests", 9_999, 5, now - 40 * 86_400_000).await;
    // Function work attributed to a tenant by the profiler.
    {
        use forgebase_functions::profile::{Finished, Profile};
        let f = env.server.functions().unwrap();
        let p = Profile::new(true);
        for _ in 0..3 {
            f.profiler().record(&Finished {
                function: "f",
                version: 1,
                status: "ok",
                duration_us: 4_000,
                fuel_used: 100,
                fuel_limit: 1000,
                memory_limit: 1 << 20,
                cold: false,
                prepare_us: 0,
                tenant: Some("env-a2"),
                profile: &p,
            });
        }
    }
    let f = env.as_(&admin, "POST", "/api/v1/insights/flush", &[], None).await;
    assert_eq!(f.status, 200, "{}", f.text());
    assert_eq!(f.json()["functions"], 1);

    // The catalog: a metered price of 3 cents (USD) per api.requests unit, and a credit rate.
    env.server
        .entitlements()
        .put_feature(
            &ctx,
            "api.requests",
            FeatureInput {
                name: None,
                description: None,
                kind: FeatureKind::Metered,
                unit: None,
                permissions: Vec::new(),
            },
        )
        .await
        .unwrap();
    let billing = env.server.billing().unwrap();
    billing
        .put_product(
            &ctx,
            "api",
            ProductInput {
                name: "API".into(),
                description: None,
                kind: ProductKind::Service,
                plan_key: None,
                addon_key: None,
                pack_key: None,
                credits: None,
                active: true,
                metadata: None,
            },
        )
        .await
        .unwrap();
    billing
        .put_price(
            &ctx,
            "api-metered",
            PriceInput {
                product: "api".into(),
                price_type: PriceType::Metered,
                interval: Some(forgebase_billing::Interval::Month),
                interval_count: 1,
                scheme: None,
                currency: "USD".into(),
                unit_amount: 3,
                tiers: None,
                currency_options: Default::default(),
                feature: Some("api.requests".into()),
                meter_basis: Some(MeterBasis::Units),
                trial_days: 0,
                tax_behavior: None,
                active: true,
                metadata: None,
            },
        )
        .await
        .unwrap();
    env.server
        .credits()
        .put_rate(
            &ctx,
            "api.requests",
            RateInput {
                credits_per_unit: 2,
                per_units: Some(10),
                basis: Some(RateBasis::Units),
            },
        )
        .await
        .unwrap();

    let r = env
        .as_(&admin, "POST", "/api/v1/insights/usage/rollup", &[], None)
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(r.json()["values"].as_u64().unwrap() >= 10, "{}", r.text());

    // Per environment: resources.
    let by_env = ok_json(
        &env,
        &admin,
        "/api/v1/insights/usage?period=month&group_by=environment&limit=100",
    )
    .await;
    assert_eq!(by_env["estimate"], true);
    let a1 = row(&by_env, "env-a1");
    assert_eq!(a1["metrics"]["storage.bytes"]["value"], 3500);
    assert_eq!(a1["metrics"]["storage.objects"]["value"], 2);
    assert_eq!(a1["metrics"]["vector.items"]["value"], 7);
    assert_eq!(a1["metrics"]["queue.messages"]["value"], 4);
    assert_eq!(a1["metrics"]["queue.bytes"]["value"], 400);
    assert_eq!(a1["org"], "org-a");
    assert_eq!(a1["project"], "proj-a");
    assert_eq!(a1["metrics"]["storage.bytes"]["kind"], "gauge");

    let a2 = row(&by_env, "env-a2");
    assert_eq!(a2["metrics"]["storage.bytes"]["value"], 400);
    assert_eq!(a2["metrics"]["functions.invocations"]["value"], 3);
    assert_eq!(a2["metrics"]["functions.compute_ms"]["value"], 12);
    let b1 = row(&by_env, "env-b1");
    assert_eq!(b1["metrics"]["storage.bytes"]["value"], 90_000);
    assert_eq!(b1["metrics"]["kv.keys"]["value"], 3);
    // Metering rows are not attributed to an environment.
    let metered = row(&by_env, "org-a");
    assert_eq!(metered["attributed"], false);
    assert_eq!(
        metered["metrics"]["usage.api.requests"]["value"], 1000,
        "last month's 9999 is outside"
    );

    // Per organization: environments and metering add up; the cost uses the catalog price.
    let by_org = ok_json(
        &env,
        &admin,
        "/api/v1/insights/usage?period=month&group_by=org&sort=usage.api.requests",
    )
    .await;
    let a = row(&by_org, "org-a");
    assert_eq!(a["name"], "Acme");
    assert_eq!(a["metrics"]["storage.bytes"]["value"], 3900);
    assert_eq!(a["metrics"]["usage.api.requests"]["value"], 1000);
    let cost = &a["cost"];
    assert_eq!(cost["estimate"], true);
    assert_eq!(cost["currency"], "USD");
    assert_eq!(cost["amount_minor"], 3000, "1000 units at 3 cents: {cost}");
    assert_eq!(cost["amount"], "30.00");
    assert_eq!(cost["credits"], 200, "1000 units at 2 credits per 10: {cost}");
    let line = cost["lines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["metric"] == "usage.api.requests")
        .unwrap();
    assert_eq!(line["price"], "api-metered");
    // Sorted largest first, the personal subject last.
    let ids: Vec<&str> = by_org["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.first(), Some(&"org-a"), "{ids:?}");
    assert_eq!(row(&by_org, "org-b")["cost"]["amount_minor"], 150);
    assert_eq!(row(&by_org, "user:users/ann")["cost"]["amount_minor"], 33);

    // Top N by a metric, by cost, and by a bad metric.
    let top = ok_json(
        &env,
        &admin,
        "/api/v1/insights/usage/top?metric=storage.bytes&n=2&group_by=environment",
    )
    .await;
    let top_ids: Vec<&str> = top["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert_eq!(top_ids, ["env-b1", "env-a1"]);
    let top_cost = ok_json(&env, &admin, "/api/v1/insights/usage/top?metric=cost&n=1&group_by=org").await;
    assert_eq!(top_cost["items"][0]["id"], "org-a");
    let bad = get(&env, &admin, "/api/v1/insights/usage/top?metric=nonsense").await;
    assert_eq!(bad.status, 400, "{}", bad.text());
    // Realm level, day period, custom range, and the daily series.
    let realm = ok_json(&env, &admin, "/api/v1/insights/usage?period=month&group_by=realm").await;
    assert_eq!(row(&realm, "default")["metrics"]["storage.bytes"]["value"], 93_900);
    let today = forgebase_insights::iso(now).split('T').next().unwrap().to_owned();
    let day = ok_json(
        &env,
        &admin,
        &format!("/api/v1/insights/usage?period=day&date={today}&group_by=project"),
    )
    .await;
    assert_eq!(row(&day, "proj-a")["metrics"]["storage.bytes"]["value"], 3900);
    let tomorrow = forgebase_insights::iso(now + 86_400_000)
        .split('T')
        .next()
        .unwrap()
        .to_owned();
    let custom = ok_json(
        &env,
        &admin,
        &format!("/api/v1/insights/usage?from={today}&to={tomorrow}&group_by=org"),
    )
    .await;
    let too_long = get(&env, &admin, "/api/v1/insights/usage?from=2020-01-01&to=2100-01-01").await;
    assert_eq!(too_long.status, 400);
    assert_eq!(too_long.code(), "invalid_range");
    assert_eq!(row(&custom, "org-a")["metrics"]["usage.api.requests"]["value"], 1000);
    let series = ok_json(
        &env,
        &admin,
        &format!("/api/v1/insights/usage/series?scope=env:env-a1&from={today}"),
    )
    .await;
    assert_eq!(series["metrics"]["storage.bytes"][0][1], 3500);
    // A currency with no price: no cost, but the quantities stay.
    let eur = ok_json(
        &env,
        &admin,
        "/api/v1/insights/usage?period=month&group_by=org&currency=EUR",
    )
    .await;
    assert!(
        row(&eur, "org-a")["cost"]["lines"]
            .as_array()
            .unwrap()
            .iter()
            .all(|l| l["amount_minor"].is_null())
    );
    // Paging.
    let page = ok_json(
        &env,
        &admin,
        "/api/v1/insights/usage?period=month&group_by=environment&limit=1&offset=1",
    )
    .await;
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    assert!(page["total"].as_u64().unwrap() >= 4);
    let metrics = ok_json(&env, &admin, "/api/v1/insights/usage/metrics").await;
    assert!(
        metrics["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["key"] == "storage.bytes" && m["kind"] == "gauge")
    );
    env.shutdown().await;
}

// ---------------------------------------------------------------------------
// Functions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn function_profiles_show_host_calls_and_cold_starts() {
    let _serial = SERIAL.lock().await;
    let env = Env::new().await;
    let admin = env.admin("fn-admin@example.com").await;
    let src = "import { serve } from \"forgebase\";\nserve(async (req, ctx) => {\n  await ctx.kv.set(\"k\", \"v\");\n  await ctx.kv.get(\"k\");\n  return Response.json({ ok: true });\n});\n";
    let r = env
        .as_(
            &admin,
            "POST",
            "/api/v1/functions",
            &[],
            Some(json!({"name": "prof", "source": src, "access": "public", "capabilities": ["kv", "log"]})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.text());
    // A restart would find the script uncached: forget it.
    env.server.functions().unwrap().evict_caches();
    for _ in 0..3 {
        let r = env
            .as_(&admin, "POST", "/api/v1/functions/prof/invoke", &[], Some(json!({})))
            .await;
        assert_eq!(r.status, 200, "{}", r.text());
    }
    let list = ok_json(&env, &admin, "/api/v1/insights/functions?function=prof").await;
    let f = &list["items"][0];
    assert_eq!(
        (f["function"].as_str(), f["invocations"].as_u64(), f["errors"].as_u64()),
        (Some("prof"), Some(3), Some(0))
    );
    assert_eq!(f["cold_starts"], 1);
    assert_eq!(f["host"][0]["op"], "kv");
    assert_eq!(f["host"][0]["count"], 6);
    assert!(f["fuel_utilisation"].as_f64().unwrap() > 0.0 && f["fuel_utilisation"].as_f64().unwrap() < 1.0);
    assert!(f["p95_ms"].as_f64().unwrap() > 0.0);
    assert!(f["memory_max_bytes"].as_u64().unwrap() > 0);
    // Per invocation, newest first, with the timeline.
    let inv = ok_json(
        &env,
        &admin,
        "/api/v1/insights/functions/invocations?function=prof&limit=2",
    )
    .await;
    assert_eq!(inv["items"].as_array().unwrap().len(), 2);
    assert!(inv["next_cursor"].is_i64(), "{inv}");
    let first = &inv["items"][0];
    assert_eq!(first["trigger"], "http");
    assert_eq!(first["profile"]["timeline"][0]["op"], "kv");
    let one = ok_json(
        &env,
        &admin,
        &format!("/api/v1/insights/functions/invocations/{}", first["id"]),
    )
    .await;
    assert_eq!(one["id"], first["id"]);
    let all = ok_json(
        &env,
        &admin,
        "/api/v1/insights/functions/invocations?function=prof&limit=10",
    )
    .await;
    let profiles: Vec<bool> = all["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["profile"]["cold_start"].as_bool().unwrap())
        .collect();
    assert_eq!(profiles.iter().filter(|c| **c).count(), 1, "{profiles:?}");
    let detail = ok_json(&env, &admin, "/api/v1/insights/functions/prof").await;
    assert_eq!(detail["versions"][0]["invocations"], 3);
    assert!(!detail["series"].as_array().unwrap().is_empty());
    assert_eq!(get(&env, &admin, "/api/v1/insights/functions/nope").await.status, 404);
    env.shutdown().await;
}

// ---------------------------------------------------------------------------
// Audit
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
async fn audit_row(env: &Env, at: i64, event: &str, outcome: &str, actor: &str, ip: &str, rid: &str, detail: &str) {
    // `detail` is `jsonb` on PostgreSQL.
    let detail_sql = if backend_name(env) == "postgres" {
        "CAST(?9 AS jsonb)"
    } else {
        "?9"
    };
    sql(
        env,
        &format!(
            "INSERT INTO _forgebase_auth_audit (at, event, outcome, actor, subject_type, collection, subject_id, ip, user_agent, request_id, detail, realm)
             VALUES (?1, ?2, ?3, ?4, 'user', 'users', ?5, ?6, ?7, ?8, {detail_sql}, 'default')"
        ),
        vec![
            at.into(),
            event.into(),
            outcome.into(),
            actor.into(),
            "res-1".into(),
            ip.into(),
            "=cmd|' /C calc'!A0".into(),
            rid.into(),
            detail.into(),
        ],
    )
    .await;
}

#[tokio::test]
async fn audit_search_pages_exports_and_relates_events() {
    let _serial = SERIAL.lock().await;
    let env = Env::new().await;
    let admin = env.admin("audit-admin@example.com").await;
    // Real events from real sign-ins: a failed and a successful user login.
    let (_user, _) = env.user("audit-subject@example.com").await;
    let bad = env
        .req(
            "POST",
            "/api/auth/login",
            &[("X-Request-Id", "req-real-1")],
            Some(json!({"email": "audit-subject@example.com", "password": "wrong-password-xx"})),
        )
        .await;
    assert_eq!(bad.status, 401, "{}", bad.text());
    let real = ok_json(&env, &admin, "/api/v1/insights/audit?outcome=failure&action=login").await;
    assert!(!real["items"].as_array().unwrap().is_empty(), "{real}");
    assert!(
        real["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["outcome"] == "failure" && e["event"].as_str().unwrap().starts_with("login"))
    );

    // Seeded rows for exact assertions (Unix seconds, far from the real ones).
    let t0 = 1_700_000_000;
    let rows = [
        (
            t0,
            "api_key.create",
            "success",
            "service_account:sa1",
            "10.0.0.1",
            "rq-A",
            r#"{"name":"deploy-key"}"#,
        ),
        (
            t0 + 10,
            "api_key.use",
            "success",
            "service_account:sa1",
            "10.0.0.1",
            "rq-B",
            r#"{"scope":"read"}"#,
        ),
        (
            t0 + 20,
            "login",
            "failure",
            "user:users/u1",
            "10.0.0.2",
            "rq-C",
            r#"{"reason":"bad_password"}"#,
        ),
        (
            t0 + 25,
            "mfa.verify",
            "failure",
            "user:users/u1",
            "10.0.0.2",
            "rq-C",
            r#"{"reason":"bad_code"}"#,
        ),
        (
            t0 + 30,
            "login",
            "success",
            "user:users/u1",
            "10.0.0.2",
            "rq-D",
            r#"{}"#,
        ),
        (
            t0 + 4000,
            "login",
            "success",
            "admin:a1",
            "10.0.0.9",
            "rq-E",
            r#"{"note":"late, \"quoted\""}"#,
        ),
    ];
    for (at, event, outcome, actor, ip, rid, detail) in rows {
        audit_row(&env, at, event, outcome, actor, ip, rid, detail).await;
    }
    let range = format!(
        "from={}&to={}",
        forgebase_insights::iso(t0 * 1000),
        forgebase_insights::iso((t0 + 5000) * 1000)
    );
    let count = |j: &Json| j["items"].as_array().unwrap().len();
    let q = |extra: &str| format!("/api/v1/insights/audit?{range}&{extra}");
    assert_eq!(count(&ok_json(&env, &admin, &q("limit=100")).await), 6);
    assert_eq!(count(&ok_json(&env, &admin, &q("actor=service_account:sa1")).await), 2);
    assert_eq!(count(&ok_json(&env, &admin, &q("actor_type=service_account")).await), 2);
    assert_eq!(count(&ok_json(&env, &admin, &q("actor_type=user")).await), 3);
    assert_eq!(count(&ok_json(&env, &admin, &q("actor_type=admin")).await), 1);
    assert_eq!(count(&ok_json(&env, &admin, &q("action=api_key.")).await), 2);
    assert_eq!(
        count(&ok_json(&env, &admin, &q("action=login&outcome=failure")).await),
        1
    );
    assert_eq!(count(&ok_json(&env, &admin, &q("ip=10.0.0.2")).await), 3);
    assert_eq!(count(&ok_json(&env, &admin, &q("request_id=rq-C")).await), 2);
    assert_eq!(
        count(
            &ok_json(
                &env,
                &admin,
                &q("resource_type=user&collection=users&resource_id=res-1")
            )
            .await
        ),
        6
    );
    assert_eq!(
        count(&ok_json(&env, &admin, &q("q=DEPLOY-key")).await),
        1,
        "free text is case-insensitive"
    );
    assert_eq!(count(&ok_json(&env, &admin, &q("q=bad_")).await), 2);
    assert_eq!(
        count(&ok_json(&env, &admin, &q("q=100%25")).await),
        0,
        "% is not a wildcard"
    );
    let narrow = format!(
        "/api/v1/insights/audit?from={}&to={}",
        forgebase_insights::iso(t0 * 1000),
        forgebase_insights::iso((t0 + 26) * 1000)
    );
    assert_eq!(count(&ok_json(&env, &admin, &narrow).await), 4, "[from, to)");
    // A value is a parameter, never SQL.
    assert_eq!(count(&ok_json(&env, &admin, &q("actor=x'%20OR%20'1'='1")).await), 0);

    // Cursor paging: newest first, no gaps, no repeats; and oldest first.
    let mut seen = Vec::new();
    let mut cursor = String::new();
    loop {
        let p = ok_json(&env, &admin, &q(&format!("limit=4{cursor}"))).await;
        seen.extend(p["items"].as_array().unwrap().iter().map(|e| e["id"].as_i64().unwrap()));
        match p["next_cursor"].as_i64() {
            Some(c) => cursor = format!("&cursor={c}"),
            None => break,
        }
    }
    assert_eq!(seen.len(), 6);
    assert!(seen.windows(2).all(|w| w[0] > w[1]), "newest first: {seen:?}");
    let asc = ok_json(&env, &admin, &q("order=asc&limit=2")).await;
    assert!(asc["items"][0]["id"].as_i64() < asc["items"][1]["id"].as_i64());
    assert_eq!(asc["items"][0]["event"], "api_key.create");
    assert_eq!(get(&env, &admin, &q("order=sideways")).await.status, 400);

    // Related events: the same request, and the same actor around it.
    let c1 = ok_json(&env, &admin, &q("request_id=rq-C&order=asc")).await;
    let id = c1["items"][0]["id"].as_i64().unwrap();
    let rel = ok_json(
        &env,
        &admin,
        &format!("/api/v1/insights/audit/{id}/related?window_secs=60"),
    )
    .await;
    assert_eq!(rel["event"]["id"], id);
    assert_eq!(rel["same_request"].as_array().unwrap().len(), 1);
    assert_eq!(rel["same_request"][0]["event"], "mfa.verify");
    assert_eq!(
        rel["same_actor"].as_array().unwrap().len(),
        1,
        "login success 10 s later, same actor: {rel}"
    );
    assert_eq!(rel["same_actor"][0]["request_id"], "rq-D");
    let wide = ok_json(
        &env,
        &admin,
        &format!("/api/v1/insights/audit/{id}/related?window_secs=3600"),
    )
    .await;
    assert_eq!(
        wide["same_actor"].as_array().unwrap().len(),
        1,
        "the other actors' rows never match"
    );
    assert_eq!(
        get(&env, &admin, &format!("/api/v1/insights/audit/{id}")).await.json()["actor"],
        "user:users/u1"
    );
    assert_eq!(get(&env, &admin, "/api/v1/insights/audit/999999999").await.status, 404);
    assert_eq!(
        get(&env, &admin, "/api/v1/insights/audit/999999999/related")
            .await
            .status,
        404
    );

    // Summary.
    let sum = ok_json(&env, &admin, &format!("/api/v1/insights/audit/summary?{range}")).await;
    assert_eq!(sum["total"], 6);
    assert_eq!(sum["events"][0]["key"], "login");
    assert_eq!(sum["events"][0]["count"], 3);

    // CSV export, streamed in pages: header plus one record per entry, and a
    // user agent that looks like a formula is neutralised.
    let csv = env
        .as_(
            &admin,
            "GET",
            &format!("/api/v1/insights/audit/export?{range}&format=csv"),
            &[],
            None,
        )
        .await;
    assert_eq!(csv.status, 200);
    assert!(csv.header("content-type").unwrap().starts_with("text/csv"));
    assert!(csv.header("content-disposition").unwrap().contains("attachment"));
    let text = csv.text();
    assert!(text.starts_with("id,at,event,outcome,actor,resource_type,collection,resource_id,ip,user_agent,request_id,impersonator,detail\r\n"));
    assert_eq!(text.matches("\r\n").count(), 1 + 6, "{text}");
    assert!(text.contains("\"'=cmd|"), "formula neutralised: {text}");
    assert!(text.contains("api_key.create,success,service_account:sa1"), "{text}");
    // JSON lines: one object per line, filters apply, oldest first.
    let jl = env
        .as_(
            &admin,
            "GET",
            &format!("/api/v1/insights/audit/export?{range}&format=jsonl&actor=user:users/u1"),
            &[],
            None,
        )
        .await;
    assert_eq!(jl.status, 200);
    let lines: Vec<Json> = jl.text().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 3);
    assert!(lines.windows(2).all(|w| w[0]["id"].as_i64() < w[1]["id"].as_i64()));
    assert_eq!(lines[0]["detail"]["reason"], "bad_password");
    let capped = env
        .as_(
            &admin,
            "GET",
            &format!("/api/v1/insights/audit/export?{range}&format=jsonl&max_rows=2"),
            &[],
            None,
        )
        .await;
    assert_eq!(capped.text().lines().count(), 2);
    assert_eq!(
        get(&env, &admin, "/api/v1/insights/audit/export?format=xml")
            .await
            .status,
        400
    );
    tokio::time::sleep(Duration::from_millis(1)).await;
    env.shutdown().await;
}
