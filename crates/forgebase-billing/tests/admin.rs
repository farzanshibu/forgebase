//! FIX-4 additions: plan-change preview, all orders, instance-wide usage
//! meters, the dunning queue, and archiving of catalog items.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use common::*;
use forgebase_api::{AppState, RouterOptions};
use forgebase_billing::{ChangePlanInput, ItemInput, OrderInput, Proration, SubscribeInput};
use forgebase_core::DataLayout;
use forgebase_data::AccessContext;
use serde_json::{Value as Json, json};
use tower::ServiceExt;

struct Api {
    env: Env,
    app: Router,
}

impl Api {
    async fn new() -> Self {
        let env = Env::new().await;
        env.catalog().await;
        env.stripe().await;
        let layout = DataLayout::new(env.tmp.path().join("data"));
        let state = AppState::from_data(env.db.clone(), layout, env.data.clone());
        let app = forgebase_api::router(
            state,
            RouterOptions {
                extensions: vec![
                    forgebase_entitlements::extension(env.ent.clone()),
                    forgebase_credits::extension(env.credits.clone()),
                    forgebase_billing::extension(env.billing.clone()),
                ],
                ..RouterOptions::default()
            },
        );
        Self { env, app }
    }

    async fn call(&self, ctx: AccessContext, method: Method, uri: &str, body: Option<Json>) -> (StatusCode, Json) {
        let mut req = Request::builder().method(method).uri(uri);
        let bytes = match body {
            Some(b) => {
                req = req.header("content-type", "application/json");
                b.to_string().into_bytes()
            }
            None => Vec::new(),
        };
        let mut req = req.body(Body::from(bytes)).unwrap();
        req.extensions_mut().insert(ctx);
        let res = self.app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(Json::Null))
    }

    async fn get(&self, ctx: AccessContext, uri: &str) -> (StatusCode, Json) {
        self.call(ctx, Method::GET, uri, None).await
    }

    async fn post(&self, ctx: AccessContext, uri: &str, body: Json) -> (StatusCode, Json) {
        self.call(ctx, Method::POST, uri, Some(body)).await
    }
}

async fn api_with_card(who: &str) -> Api {
    let api = Api::new().await;
    api.env.customer(who, "USD").await;
    api.env.save_card(who).await;
    api
}

#[tokio::test]
async fn preview_matches_the_real_change_and_writes_nothing() {
    let env = Env::new().await;
    env.catalog().await;
    env.stripe().await;
    env.customer("ann", "USD").await;
    env.save_card("ann").await;
    let sub = env.subscribe("ann", &["basic-m"], "k").await.subscription.unwrap();
    env.clock.advance(15 * DAY);
    let input = || ChangePlanInput {
        price: "pro-m".into(),
        quantity: None,
        proration: Some(Proration::AlwaysInvoice),
        at_period_end: false,
    };
    let invoices_before = env.count("SELECT COUNT(*) FROM _forgebase_billing_invoices").await;
    let p = env
        .billing
        .preview_plan_change(&user("ann"), &sub.id, input())
        .await
        .unwrap();
    assert_eq!((p.credit, p.charge, p.net), (500, 1500, 1000));
    assert!(p.invoiced_now);
    assert_eq!(p.from_price.as_deref(), Some("basic-m"));
    assert_eq!(
        env.count("SELECT COUNT(*) FROM _forgebase_billing_invoices").await,
        invoices_before,
        "a preview writes nothing"
    );
    assert_eq!(env.plan_of(&pk("ann")).0.as_deref(), Some("basic"));
    assert!(
        env.http.calls("/v1/payment_intents").len() <= 1,
        "nothing charged by the preview"
    );
    // The real change invoices exactly the previewed lines.
    let inv = env
        .billing
        .change_plan(&user("ann"), &sub.id, input())
        .await
        .unwrap()
        .invoice
        .unwrap();
    assert_eq!(
        inv.lines.iter().map(|l| l.amount).collect::<Vec<_>>(),
        p.lines.iter().map(|l| l.amount).collect::<Vec<_>>()
    );
    assert_eq!(inv.total, p.net);
}

#[tokio::test]
async fn preview_over_http_period_end_and_errors() {
    let api = api_with_card("ann").await;
    let sub = api.env.subscribe("ann", &["basic-m"], "k").await.subscription.unwrap();
    let uri = format!("/api/v1/billing/subscriptions/{}/change/preview", sub.id);
    let (s, j) = api
        .post(user("ann"), &uri, json!({"price": "pro-m", "at_period_end": true}))
        .await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(
        (j["at_period_end"].clone(), j["net"].clone(), j["lines"].clone()),
        (json!(true), json!(0), json!([]))
    );
    assert_eq!(j["proration"], "none");
    let (s, j) = api.post(user("ann"), &uri, json!({"price": "basic-m"})).await;
    assert_eq!(
        (s, j["error"]["code"].as_str()),
        (StatusCode::CONFLICT, Some("no_change"))
    );
    let (s, _) = api.post(user("bob"), &uri, json!({"price": "pro-m"})).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "someone else's subscription");
}

#[tokio::test]
async fn all_orders_pages_and_filters_for_administrators() {
    let api = api_with_card("ann").await;
    api.env.customer("bob", "USD").await;
    api.env.save_card("bob").await;
    for (who, key) in [("ann", "o1"), ("ann", "o2"), ("bob", "o3")] {
        api.env
            .billing
            .create_order(
                &user(who),
                OrderInput {
                    items: vec![ItemInput::new("pack-1k")],
                    idempotency_key: key.into(),
                    ..OrderInput::default()
                },
            )
            .await
            .unwrap();
    }
    let (s, _) = api.get(user("ann"), "/api/v1/billing/admin/orders").await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, j) = api.get(admin(), "/api/v1/billing/admin/orders?limit=2").await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(
        (
            j["items"].as_array().unwrap().len(),
            j["total"].as_i64(),
            j["limit"].as_i64()
        ),
        (2, Some(3), Some(2))
    );
    let (_, j2) = api.get(admin(), "/api/v1/billing/admin/orders?limit=2&offset=2").await;
    assert_eq!(j2["items"].as_array().unwrap().len(), 1);
    let ids: Vec<&str> = j["items"]
        .as_array()
        .unwrap()
        .iter()
        .chain(j2["items"].as_array().unwrap())
        .map(|o| o["id"].as_str().unwrap())
        .collect();
    let unique: std::collections::BTreeSet<_> = ids.iter().collect();
    assert_eq!(unique.len(), 3, "pages do not overlap");
    let (_, j) = api
        .get(admin(), &format!("/api/v1/billing/admin/orders?subject={}", pk("bob")))
        .await;
    assert_eq!(j["total"], 1);
    let (s, _) = api.get(admin(), "/api/v1/billing/admin/orders?status=nope").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn usage_meters_list_every_subject_with_states() {
    let api = api_with_card("ann").await;
    api.env.customer("bob", "USD").await;
    api.env.save_card("bob").await;
    api.env.subscribe("ann", &["basic-m"], "a").await; // projects limit 20
    api.env.subscribe("bob", &["pro-m"], "b").await; // projects limit 100
    api.env.use_units(&pk("ann"), "projects", 15, "u1").await; // 75 %
    api.env.use_units(&pk("bob"), "projects", 10, "u2").await; // 10 %
    let (s, _) = api.get(user("ann"), "/api/v1/billing/admin/usage-meters").await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, j) = api.get(admin(), "/api/v1/billing/admin/usage-meters").await;
    assert_eq!(s, StatusCode::OK, "{j}");
    let items = j["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "{j}");
    assert_eq!(items[0]["subject"], pk("ann"), "most used first");
    assert_eq!(
        (items[0]["used"].clone(), items[0]["state"].clone()),
        (json!(15), json!("warn"))
    );
    assert_eq!(items[1]["state"], "ok");
    let (_, j) = api
        .get(admin(), "/api/v1/billing/admin/usage-meters?min_percent=70")
        .await;
    assert_eq!(j["total"], 1);
    let (_, j) = api
        .get(
            admin(),
            &format!(
                "/api/v1/billing/admin/usage-meters?subject={}&feature=projects",
                pk("bob")
            ),
        )
        .await;
    assert_eq!(j["items"].as_array().unwrap().len(), 1);
    let (s, _) = api
        .get(admin(), "/api/v1/billing/admin/usage-meters?min_percent=x")
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn dunning_queue_lists_failed_invoices_with_the_last_failure() {
    let api = api_with_card("ann").await;
    let sub = api.env.subscribe("ann", &["pro-m"], "k").await.subscription.unwrap();
    let (_, j) = api.get(admin(), "/api/v1/billing/dunning").await;
    assert_eq!(j["items"], json!([]));
    api.env.http.set_mode(ChargeMode::Decline);
    api.env.advance(30 * DAY).await;
    let (s, _) = api.get(user("ann"), "/api/v1/billing/dunning").await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, j) = api.get(admin(), "/api/v1/billing/dunning").await;
    assert_eq!(s, StatusCode::OK, "{j}");
    let q = j["items"].as_array().unwrap();
    assert_eq!(q.len(), 1);
    let e = &q[0];
    assert_eq!(e["subscription_id"], sub.id.as_str());
    assert_eq!(e["subscription_status"], "past_due");
    assert_eq!(
        (e["attempt_count"].clone(), e["retries_left"].clone()),
        (json!(1), json!(3))
    );
    assert_eq!(e["last_failure_code"], "insufficient_funds");
    assert_eq!(e["final_action"], "cancel");
    assert!(e["next_attempt_at"].is_string());
    // Recovery empties the queue.
    api.env.http.set_mode(ChargeMode::Succeed);
    api.env.advance(DAY).await;
    let (_, j) = api.get(admin(), "/api/v1/billing/dunning").await;
    assert_eq!(j["items"], json!([]));
}

#[tokio::test]
async fn archiving_is_admin_only_idempotent_and_keeps_subscriptions_running() {
    let api = api_with_card("ann").await;
    let sub = api.env.subscribe("ann", &["basic-m"], "k").await.subscription.unwrap();
    for path in ["products/basic", "prices/basic-m"] {
        let uri = format!("/api/v1/billing/{path}/archive");
        let (s, _) = api.post(user("ann"), &uri, json!({})).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        for _ in 0..2 {
            let (s, j) = api.post(admin(), &uri, json!({})).await;
            assert_eq!((s, j["active"].clone()), (StatusCode::OK, json!(false)), "{j}");
        }
    }
    let (s, j) = api
        .post(admin(), "/api/v1/billing/prices/nope/archive", json!({}))
        .await;
    assert_eq!(
        (s, j["error"]["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("not_found"))
    );
    // New subscriptions refuse the archived price; the existing one renews.
    let r = api
        .env
        .billing
        .subscribe(
            &user("ann"),
            SubscribeInput {
                items: vec![ItemInput::new("basic-m")],
                idempotency_key: "k2".into(),
                ..SubscribeInput::default()
            },
        )
        .await;
    assert!(r.is_err());
    let rep = api.env.advance(30 * DAY).await;
    assert_eq!(rep.renewed, 1);
    let s = api.env.billing.subscription(&user("ann"), &sub.id).await.unwrap();
    assert_eq!(format!("{:?}", s.status), "Active");

    // Tax rates and coupons.
    let (s, _) = api
        .call(
            admin(),
            Method::PUT,
            "/api/v1/billing/tax-rates/de",
            Some(json!({"name": "VAT", "rate_bp": 1900, "active": true})),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, j) = api
        .post(admin(), "/api/v1/billing/tax-rates/de/archive", json!({}))
        .await;
    assert_eq!(
        (s, j["active"].clone(), j["region"].clone()),
        (StatusCode::OK, json!(false), json!("DE"))
    );
    let (s, _) = api
        .call(
            admin(),
            Method::PUT,
            "/api/v1/billing/coupons/save10",
            Some(json!({"kind": "percent", "percent_bp": 1000, "active": true})),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, j) = api
        .post(admin(), "/api/v1/billing/coupons/save10/archive", json!({}))
        .await;
    assert_eq!(
        (s, j["active"].clone(), j["code"].clone()),
        (StatusCode::OK, json!(false), json!("SAVE10"))
    );
    let (s, _) = api
        .post(admin(), "/api/v1/billing/coupons/none/archive", json!({}))
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[test]
fn new_operations_are_documented_with_unique_ids() {
    let doc = serde_json::to_value(forgebase_billing::openapi()).unwrap();
    let mut seen = std::collections::BTreeSet::new();
    for (path, item) in doc["paths"].as_object().unwrap() {
        for (_, op) in item.as_object().unwrap() {
            assert!(
                seen.insert(op["operationId"].as_str().unwrap().to_owned()),
                "duplicate id at {path}"
            );
        }
    }
    for id in [
        "billing_preview_plan_change",
        "billing_all_orders",
        "billing_usage_meters",
        "billing_dunning_queue",
        "billing_archive_product",
        "billing_archive_price",
        "billing_archive_coupon",
        "billing_archive_tax_rate",
    ] {
        assert!(seen.contains(id), "{id} missing");
    }
}
