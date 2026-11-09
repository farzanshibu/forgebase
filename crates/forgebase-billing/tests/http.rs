//! REST under `/api/v1/billing` and the OpenAPI fragment, through the real
//! router (no sockets): who may do what, the customer portal, anonymous
//! signed webhooks.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use common::*;
use forgebase_api::{AppState, RouterOptions};
use forgebase_billing::provider::stripe;
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

    async fn raw(
        &self,
        ctx: Option<AccessContext>,
        method: Method,
        uri: &str,
        headers: &[(&str, String)],
        body: Vec<u8>,
    ) -> (StatusCode, Json) {
        let mut req = Request::builder().method(method).uri(uri);
        for (k, v) in headers {
            req = req.header(*k, v);
        }
        let mut req = req.body(Body::from(body)).unwrap();
        if let Some(ctx) = ctx {
            req.extensions_mut().insert(ctx);
        }
        let res = self.app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(Json::Null))
    }

    async fn call(
        &self,
        ctx: Option<AccessContext>,
        method: Method,
        uri: &str,
        body: Option<Json>,
    ) -> (StatusCode, Json) {
        match body {
            Some(b) => {
                self.raw(
                    ctx,
                    method,
                    uri,
                    &[("content-type", "application/json".into())],
                    b.to_string().into_bytes(),
                )
                .await
            }
            None => self.raw(ctx, method, uri, &[], Vec::new()).await,
        }
    }

    async fn get(&self, ctx: AccessContext, uri: &str) -> (StatusCode, Json) {
        self.call(Some(ctx), Method::GET, uri, None).await
    }

    async fn post(&self, ctx: AccessContext, uri: &str, body: Json) -> (StatusCode, Json) {
        self.call(Some(ctx), Method::POST, uri, Some(body)).await
    }

    async fn put(&self, ctx: AccessContext, uri: &str, body: Json) -> (StatusCode, Json) {
        self.call(Some(ctx), Method::PUT, uri, Some(body)).await
    }
}

fn code(j: &Json) -> &str {
    j["error"]["code"].as_str().unwrap_or_default()
}

#[tokio::test]
async fn catalog_is_read_by_callers_and_written_by_administrators() {
    let api = Api::new().await;
    let (s, _) = api.call(None, Method::GET, "/api/v1/billing/products", None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, j) = api.get(user("ann"), "/api/v1/billing/products").await;
    assert_eq!(s, StatusCode::OK);
    assert!(j["items"].as_array().unwrap().len() >= 6);
    let (s, j) = api.get(user("ann"), "/api/v1/billing/prices/pro-m").await;
    assert_eq!(
        (s, j["currency_options"]["EUR"]["unit_amount"].as_i64()),
        (StatusCode::OK, Some(2800))
    );
    let body = json!({"name": "Gold", "kind": "service"});
    let (s, _) = api
        .put(user("ann"), "/api/v1/billing/products/gold", body.clone())
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, j) = api.put(admin(), "/api/v1/billing/products/gold", body).await;
    assert_eq!((s, j["key"].as_str()), (StatusCode::OK, Some("gold")));
    let (s, j) = api
        .put(
            admin(),
            "/api/v1/billing/prices/gold-m",
            json!({"product": "gold", "type": "recurring", "currency": "usd", "unit_amount": -5}),
        )
        .await;
    assert_eq!((s, code(&j)), (StatusCode::BAD_REQUEST, "invalid_amount"));
    let (s, j) = api.get(user("ann"), "/api/v1/billing/settings").await;
    assert_eq!((s, code(&j)), (StatusCode::FORBIDDEN, "forbidden"));
    let (s, j) = api.get(admin(), "/api/v1/billing/settings").await;
    assert_eq!((s, j["retry_days"].clone()), (StatusCode::OK, json!([1, 3, 5])));
    let (s, _) = api.get(user("ann"), "/api/v1/billing/stats").await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, j) = api.get(admin(), "/api/v1/billing/stats?interval=month").await;
    assert_eq!(s, StatusCode::OK, "{j}");
}

#[tokio::test]
async fn customer_portal_flow() {
    let api = Api::new().await;
    let (s, _) = api
        .put(
            user("ann"),
            "/api/v1/billing/customer",
            json!({"email": "ann@example.test", "currency": "USD"}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    api.env.save_card("ann").await;
    let body = json!({"items": [{"price": "basic-m"}], "idempotency_key": "s1"});
    let (s, j) = api
        .post(user("ann"), "/api/v1/billing/subscriptions", body.clone())
        .await;
    assert_eq!(s, StatusCode::CREATED, "{j}");
    let sub = j["subscription"]["id"].as_str().unwrap().to_owned();
    assert_eq!(j["subscription"]["status"], "active");
    let (s, j) = api.post(user("ann"), "/api/v1/billing/subscriptions", body).await;
    assert_eq!((s, j["replayed"].as_bool()), (StatusCode::OK, Some(true)));

    let (s, j) = api.get(user("ann"), "/api/v1/billing/portal").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(j["subscriptions"].as_array().unwrap().len(), 1);
    assert_eq!(j["customer"]["has_payment_method"], true);
    assert!(
        j["customer"].get("payment_method").is_none(),
        "the provider token is not exposed"
    );
    let inv = j["invoices"][0]["id"].as_str().unwrap().to_owned();
    // Another user sees nothing of it.
    let (s, _) = api.get(user("bob"), &format!("/api/v1/billing/invoices/{inv}")).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, j) = api
        .get(user("bob"), &format!("/api/v1/billing/invoices?subject={}", pk("ann")))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{j}");

    // Upgrade, then cancel at period end.
    let (s, j) = api
        .post(
            user("ann"),
            &format!("/api/v1/billing/subscriptions/{sub}/change"),
            json!({"price": "pro-m"}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(j["invoice"]["billing_reason"], "subscription_update");
    let (s, j) = api
        .post(
            user("ann"),
            &format!("/api/v1/billing/subscriptions/{sub}/cancel"),
            json!({}),
        )
        .await;
    assert_eq!(
        (s, j["subscription"]["cancel_at_period_end"].as_bool()),
        (StatusCode::OK, Some(true))
    );
    let (s, j) = api
        .post(
            user("ann"),
            "/api/v1/billing/portal/payment-method",
            json!({"return_url": "https://app.example.test/x"}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert!(j["url"].as_str().unwrap().starts_with("https://checkout.stripe.test/"));
    let (s, j) = api.get(user("ann"), "/api/v1/billing/invoices").await;
    assert_eq!((s, j["items"].as_array().unwrap().len()), (StatusCode::OK, 2));
    // Refunds are for operators.
    let (s, _) = api
        .post(
            user("ann"),
            &format!("/api/v1/billing/invoices/{inv}/credit-notes"),
            json!({"idempotency_key": "x"}),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, j) = api
        .post(
            admin(),
            &format!("/api/v1/billing/invoices/{inv}/credit-notes"),
            json!({"amount": 100, "idempotency_key": "x"}),
        )
        .await;
    assert_eq!((s, j["number"].as_str()), (StatusCode::CREATED, Some("CN-000001")));
}

#[tokio::test]
async fn organization_billing_managers_buy_for_their_org_but_cannot_refund_it() {
    let api = Api::new().await;
    let org = api.env.policy.create_org(&user("olga"), "acme", "acme").await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    api.env
        .policy
        .set_member(&user("olga"), &org, &pk("mia"), &["member".into()], &[])
        .await
        .unwrap();
    let subject = format!("org:{org}");
    let body = json!({"subject": subject, "items": [{"price": "pro-m"}], "idempotency_key": "o"});
    let (s, _) = api
        .post(user("mia"), "/api/v1/billing/subscriptions", body.clone())
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "a plain member cannot spend the org's money");
    let (s, j) = api.post(user("olga"), "/api/v1/billing/subscriptions", body).await;
    assert_eq!(s, StatusCode::CREATED, "{j}");
    assert_eq!(j["subscription"]["status"], "incomplete");
    assert!(j["payment_url"].as_str().is_some());
    let inv = j["invoice"]["id"].as_str().unwrap().to_owned();
    let (s, _) = api
        .get(user("olga"), &format!("/api/v1/billing/invoices?subject={subject}"))
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = api
        .post(
            user("olga"),
            &format!("/api/v1/billing/invoices/{inv}/mark-paid"),
            json!({}),
        )
        .await;
    assert_eq!(
        s,
        StatusCode::FORBIDDEN,
        "an org owner cannot mark its own invoices paid"
    );
}

#[tokio::test]
async fn webhooks_are_anonymous_and_signed() {
    let api = Api::new().await;
    api.env.customer("ann", "USD").await;
    let c = api.env.subscribe("ann", &["pro-m"], "k").await;
    let inv = c.invoice.unwrap().id;
    let now = api.env.clock_secs();
    let body = stripe_event(
        "evt_http",
        "payment_intent.succeeded",
        now,
        json!({"id": "pi_http", "amount": 3000, "currency": "usd", "metadata": {"forgebase_invoice": inv}}),
    )
    .to_string();
    let (s, j) = api
        .raw(
            None,
            Method::POST,
            "/api/v1/billing/webhooks/stripe",
            &[("stripe-signature", "t=1,v1=00".into())],
            body.clone().into_bytes(),
        )
        .await;
    assert_eq!((s, code(&j)), (StatusCode::BAD_REQUEST, "invalid_signature"));
    let sig = stripe::sign(STRIPE_WHSEC, now, body.as_bytes());
    for expected in ["processed", "duplicate"] {
        let (s, j) = api
            .raw(
                None,
                Method::POST,
                "/api/v1/billing/webhooks/stripe",
                &[("stripe-signature", sig.clone())],
                body.clone().into_bytes(),
            )
            .await;
        assert_eq!((s, j["result"].as_str()), (StatusCode::OK, Some(expected)), "{j}");
    }
    let (s, _) = api
        .raw(
            None,
            Method::POST,
            "/api/v1/billing/webhooks/razorpay",
            &[],
            b"{}".to_vec(),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, j) = api.get(admin(), "/api/v1/billing/webhook-events").await;
    assert_eq!((s, j["items"][0]["attempts"].as_i64()), (StatusCode::OK, Some(2)));
}

#[tokio::test]
async fn openapi_documents_every_route() {
    let doc = serde_json::to_value(forgebase_billing::openapi()).unwrap();
    let paths = doc["paths"].as_object().unwrap();
    for p in [
        "/api/v1/billing/products/{key}",
        "/api/v1/billing/prices/{key}",
        "/api/v1/billing/subscriptions",
        "/api/v1/billing/subscriptions/{id}/change",
        "/api/v1/billing/subscriptions/{id}/cancel",
        "/api/v1/billing/orders",
        "/api/v1/billing/invoices/{id}/credit-notes",
        "/api/v1/billing/portal",
        "/api/v1/billing/portal/payment-method",
        "/api/v1/billing/webhooks/{provider}",
        "/api/v1/billing/stats",
    ] {
        assert!(paths.contains_key(p), "{p} not documented");
    }
    // Names do not collide with other extensions in the merged document.
    let schemas = doc["components"]["schemas"].as_object().unwrap();
    for n in [
        "BillingSubscription",
        "BillingSubscriptionStatus",
        "BillingInterval",
        "BillingCollection",
        "BillingMaintenanceReport",
    ] {
        assert!(schemas.contains_key(n), "{n} missing");
    }
    for n in [
        "Subscription",
        "SubscriptionStatus",
        "Interval",
        "Collection",
        "MaintenanceReport",
    ] {
        assert!(!schemas.contains_key(n), "{n} collides with another crate's schema");
    }
    // Every route of the router is documented.
    let src = include_str!("../src/http.rs");
    let routes = src.matches(".route(\"").count();
    assert_eq!(routes, paths.len(), "routes and documented paths differ");
}
