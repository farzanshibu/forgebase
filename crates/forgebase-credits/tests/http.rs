//! REST under `/api/v1/credits` and the OpenAPI fragment, through the real
//! router (no sockets).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use common::*;
use forgebase_api::{AppState, RouterOptions};
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
        let layout = DataLayout::new(env.tmp.path().join("data"));
        let state = AppState::from_data(env.db.clone(), layout, env.data.clone());
        let app = forgebase_api::router(
            state,
            RouterOptions {
                extensions: vec![
                    forgebase_entitlements::extension(env.ent.clone()),
                    forgebase_credits::extension(env.credits.clone()),
                ],
                ..RouterOptions::default()
            },
        );
        Self { env, app }
    }

    async fn call(
        &self,
        ctx: Option<AccessContext>,
        method: Method,
        uri: &str,
        body: Option<Json>,
    ) -> (StatusCode, Json) {
        let mut req = Request::builder().method(method).uri(uri);
        let body = match body {
            Some(b) => {
                req = req.header("content-type", "application/json");
                Body::from(b.to_string())
            }
            None => Body::empty(),
        };
        let mut req = req.body(body).unwrap();
        if let Some(ctx) = ctx {
            req.extensions_mut().insert(ctx);
        }
        let res = self.app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(Json::Null))
    }

    async fn get(&self, ctx: AccessContext, uri: &str) -> (StatusCode, Json) {
        self.call(Some(ctx), Method::GET, uri, None).await
    }

    async fn post(&self, ctx: AccessContext, uri: &str, body: Json) -> (StatusCode, Json) {
        self.call(Some(ctx), Method::POST, uri, Some(body)).await
    }
}

fn code(j: &Json) -> &str {
    j["error"]["code"].as_str().unwrap_or_default()
}

const OPERATIONS: &[(&str, &str)] = &[
    ("get", "/api/v1/credits/balance"),
    ("get", "/api/v1/credits/lots"),
    ("get", "/api/v1/credits/ledger"),
    ("get", "/api/v1/credits/ledger/{id}"),
    ("get", "/api/v1/credits/audit"),
    ("post", "/api/v1/credits/top-ups"),
    ("post", "/api/v1/credits/grants"),
    ("post", "/api/v1/credits/spend"),
    ("post", "/api/v1/credits/refunds"),
    ("post", "/api/v1/credits/reversals"),
    ("get", "/api/v1/credits/rates"),
    ("put", "/api/v1/credits/rates/{feature}"),
    ("delete", "/api/v1/credits/rates/{feature}"),
    ("get", "/api/v1/credits/usage"),
    ("get", "/api/v1/credits/usage/cursor"),
    ("put", "/api/v1/credits/usage/cursor"),
    ("get", "/api/v1/credits/stats/balance"),
    ("get", "/api/v1/credits/stats/consumption"),
    ("get", "/api/v1/credits/stats/summary"),
    ("post", "/api/v1/credits/maintenance"),
];

#[tokio::test]
async fn openapi_documents_every_route_and_every_route_answers() {
    let api = Api::new().await;
    let (s, doc) = api.call(None, Method::GET, "/api/openapi.json", None).await;
    assert_eq!(s, StatusCode::OK);
    for (m, p) in OPERATIONS {
        assert!(doc["paths"][*p][*m].is_object(), "missing in OpenAPI: {m} {p}");
        assert_eq!(doc["paths"][*p][*m]["tags"][0], "credits", "{p}");
        let uri = p.replace("{id}", "x").replace("{feature}", "x");
        let method = Method::from_bytes(m.to_uppercase().as_bytes()).unwrap();
        let body = (*m != "get" && *m != "delete").then(|| json!({}));
        let (s, j) = api.call(Some(AccessContext::anonymous()), method, &uri, body).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{m} {uri}: {j}");
    }
    let credit_paths = doc["paths"]
        .as_object()
        .unwrap()
        .keys()
        .filter(|k| k.starts_with("/api/v1/credits"))
        .count();
    assert_eq!(credit_paths, 18);
    for schema in [
        "LedgerEntry",
        "CreditInput",
        "Reversal",
        "BalanceHistory",
        "ConsumptionStats",
    ] {
        assert!(doc["components"]["schemas"][schema].is_object(), "{schema}");
    }
}

#[tokio::test]
async fn credits_over_rest() {
    let api = Api::new().await;
    let s = pk("alice");
    let alice = user("alice");
    // Top-up: 201, replay 200, conflict 409; users cannot mint.
    let body = json!({"subject": s, "amount": 50, "idempotency_key": "pay_1", "external_ref": "order_1"});
    let (st, j) = api.post(admin(), "/api/v1/credits/top-ups", body.clone()).await;
    assert_eq!(st, StatusCode::CREATED, "{j}");
    assert_eq!(j["entry"]["kind"], "top_up");
    assert_eq!(j["entry"]["balance_after"], 50);
    let top = j["entry"]["id"].as_str().unwrap().to_owned();
    let (st, j) = api.post(admin(), "/api/v1/credits/top-ups", body).await;
    assert_eq!((st, j["replayed"].as_bool()), (StatusCode::OK, Some(true)));
    let (st, j) = api
        .post(
            admin(),
            "/api/v1/credits/top-ups",
            json!({"subject": s, "amount": 51, "idempotency_key": "pay_1"}),
        )
        .await;
    assert_eq!((st, code(&j)), (StatusCode::CONFLICT, "idempotency_conflict"));
    let (st, _) = api
        .post(
            alice.clone(),
            "/api/v1/credits/top-ups",
            json!({"subject": s, "amount": 5, "idempotency_key": "self"}),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, j) = api
        .post(
            admin(),
            "/api/v1/credits/grants",
            json!({"subject": s, "amount": 10, "idempotency_key": "promo", "expires_at": "2026-11-10T12:00:00Z"}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{j}");
    assert_eq!(j["entry"]["expires_at"], "2026-11-10T12:00:00.000Z");
    let (st, j) = api
        .post(
            admin(),
            "/api/v1/credits/grants",
            json!({"subject": s, "amount": 1, "bogus": 1}),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{j}");

    // The subject reads its own balance, lots and ledger.
    let (st, j) = api.get(alice.clone(), "/api/v1/credits/balance").await;
    assert_eq!((st, j["balance"].as_i64()), (StatusCode::OK, Some(60)));
    assert_eq!(j["expiring"][0]["amount"], 10);
    let (_, j) = api.get(alice.clone(), "/api/v1/credits/lots").await;
    assert_eq!(j["items"].as_array().unwrap().len(), 2);
    let (st, _) = api
        .get(user("bob"), &format!("/api/v1/credits/balance?subject={s}"))
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    // Spending: 402 with the balance when it does not cover it.
    let (st, j) = api
        .post(
            admin(),
            "/api/v1/credits/spend",
            json!({"subject": s, "amount": 61, "idempotency_key": "big"}),
        )
        .await;
    assert_eq!((st, code(&j)), (StatusCode::PAYMENT_REQUIRED, "insufficient_credits"));
    assert_eq!(j["error"]["details"]["balance"], 60);
    let (st, _) = api
        .post(
            alice.clone(),
            "/api/v1/credits/spend",
            json!({"amount": 1, "idempotency_key": "x"}),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, j) = api
        .post(
            admin(),
            "/api/v1/credits/spend",
            json!({"subject": s, "amount": 15, "idempotency_key": "gen_1", "feature": "images", "units": 3}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{j}");
    let spend = j["entry"]["id"].as_str().unwrap().to_owned();
    let (st, j) = api
        .post(
            admin(),
            "/api/v1/credits/refunds",
            json!({"entry_id": spend, "amount": 5, "idempotency_key": "gen_1_failed"}),
        )
        .await;
    assert_eq!((st, j["entry"]["amount"].as_i64()), (StatusCode::CREATED, Some(5)));
    let (st, j) = api
        .post(
            admin(),
            "/api/v1/credits/reversals",
            json!({"entry_id": top, "idempotency_key": "chargeback"}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{j}");
    // 50 bought; 10 of the spend came from the expiring grant first.
    assert_eq!((j["reversed"].as_i64(), j["unrecovered"].as_i64()), (Some(50), Some(0)));

    let (_, j) = api.get(alice.clone(), "/api/v1/credits/ledger?limit=2").await;
    assert_eq!(j["items"].as_array().unwrap().len(), 2);
    let next = j["next"].as_i64().unwrap();
    let (_, j) = api
        .get(
            alice.clone(),
            &format!("/api/v1/credits/ledger?after={next}&kind=reversal"),
        )
        .await;
    assert_eq!(j["items"][0]["amount"], -50);
    let (st, j) = api.get(alice.clone(), &format!("/api/v1/credits/ledger/{spend}")).await;
    assert_eq!((st, j["feature"].as_str()), (StatusCode::OK, Some("images")));
    let (st, _) = api.get(user("bob"), &format!("/api/v1/credits/ledger/{spend}")).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, j) = api.get(alice.clone(), "/api/v1/credits/audit").await;
    assert_eq!((st, j["consistent"].as_bool()), (StatusCode::OK, Some(true)), "{j}");
    let (st, _) = api.get(alice.clone(), "/api/v1/credits/ledger?kind=nope").await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    // Statistics.
    let (st, j) = api
        .get(
            alice.clone(),
            "/api/v1/credits/stats/balance?from=2026-11-09T00:00:00Z&to=2026-11-10T00:00:00Z&interval=hour",
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{j}");
    assert_eq!(j["points"].as_array().unwrap().len(), 24);
    assert_eq!(j["points"][12]["closing"], 0);
    let (_, j) = api.get(alice.clone(), "/api/v1/credits/stats/consumption").await;
    assert_eq!(j["rows"][0]["feature"], "images");
    assert_eq!(j["total_net"], 10);
    let (_, j) = api.get(alice.clone(), "/api/v1/credits/stats/summary").await;
    assert_eq!((j["top_up"].as_i64(), j["reversal"].as_i64()), (Some(50), Some(50)));
    let (st, _) = api.get(alice.clone(), "/api/v1/credits/stats/summary?all=true").await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = api
        .get(alice.clone(), "/api/v1/credits/stats/balance?interval=week")
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    // Prices and maintenance: administrators.
    let (st, _) = api
        .call(
            Some(alice.clone()),
            Method::PUT,
            "/api/v1/credits/rates/images",
            Some(json!({"credits_per_unit": 5})),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, j) = api
        .call(
            Some(admin()),
            Method::PUT,
            "/api/v1/credits/rates/images",
            Some(json!({"credits_per_unit": 5, "basis": "overage"})),
        )
        .await;
    assert_eq!((st, j["basis"].as_str()), (StatusCode::OK, Some("overage")));
    let (_, j) = api.get(alice.clone(), "/api/v1/credits/rates").await;
    assert_eq!(j["items"][0]["feature"], "images");
    let (st, _) = api
        .call(Some(admin()), Method::DELETE, "/api/v1/credits/rates/images", None)
        .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (st, _) = api.post(alice.clone(), "/api/v1/credits/maintenance", json!({})).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, j) = api
        .call(Some(admin()), Method::POST, "/api/v1/credits/maintenance", None)
        .await;
    assert_eq!(st, StatusCode::OK, "{j}");
    let (st, j) = api
        .call(
            Some(admin()),
            Method::PUT,
            "/api/v1/credits/usage/cursor",
            Some(json!({"seq": 0})),
        )
        .await;
    assert_eq!((st, j["seq"].as_i64()), (StatusCode::OK, Some(0)));
    let (_, j) = api.get(alice, "/api/v1/credits/usage").await;
    assert_eq!(j["items"].as_array().unwrap().len(), 0);
    api.env.assert_consistent(&s).await;
}
