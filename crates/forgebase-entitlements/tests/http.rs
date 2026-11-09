//! REST under `/api/v1`, the OpenAPI fragment and the plan-based rate-limit
//! gate, through the real router (no sockets).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use common::*;
use forgebase_api::{AppState, RouterOptions};
use forgebase_core::DataLayout;
use forgebase_data::AccessContext;
use forgebase_entitlements::FeatureKind;
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
                extensions: vec![forgebase_entitlements::extension(env.ent.clone())],
                gates: vec![env.ent.rate_gate()],
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
    ) -> (StatusCode, HeaderMap, Json) {
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
        let headers = res.headers().clone();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
        (status, headers, serde_json::from_slice(&bytes).unwrap_or(Json::Null))
    }

    async fn get(&self, ctx: AccessContext, uri: &str) -> (StatusCode, Json) {
        let (s, _, j) = self.call(Some(ctx), Method::GET, uri, None).await;
        (s, j)
    }

    async fn send(&self, ctx: AccessContext, method: Method, uri: &str, body: Json) -> (StatusCode, Json) {
        let (s, _, j) = self.call(Some(ctx), method, uri, Some(body)).await;
        (s, j)
    }
}

fn code(j: &Json) -> &str {
    j["error"]["code"].as_str().unwrap_or_default()
}

const OPERATIONS: &[(&str, &str)] = &[
    ("get", "/api/v1/entitlements"),
    ("get", "/api/v1/entitlements/has"),
    ("get", "/api/v1/entitlements/subscription"),
    ("put", "/api/v1/entitlements/subscription"),
    ("post", "/api/v1/entitlements/trials"),
    ("put", "/api/v1/entitlements/addons"),
    ("get", "/api/v1/entitlements/usage-packs"),
    ("post", "/api/v1/entitlements/usage-packs"),
    ("get", "/api/v1/entitlements/overrides"),
    ("post", "/api/v1/entitlements/overrides"),
    ("delete", "/api/v1/entitlements/overrides/{id}"),
    ("get", "/api/v1/entitlements/limits"),
    ("put", "/api/v1/entitlements/limits"),
    ("delete", "/api/v1/entitlements/limits"),
    ("get", "/api/v1/features"),
    ("get", "/api/v1/features/{key}"),
    ("put", "/api/v1/features/{key}"),
    ("delete", "/api/v1/features/{key}"),
    ("get", "/api/v1/plans"),
    ("get", "/api/v1/plans/{key}"),
    ("put", "/api/v1/plans/{key}"),
    ("delete", "/api/v1/plans/{key}"),
    ("get", "/api/v1/addons"),
    ("put", "/api/v1/addons/{key}"),
    ("delete", "/api/v1/addons/{key}"),
    ("get", "/api/v1/usage-packs"),
    ("put", "/api/v1/usage-packs/{key}"),
    ("delete", "/api/v1/usage-packs/{key}"),
    ("get", "/api/v1/quota-policies"),
    ("put", "/api/v1/quota-policies/{feature}"),
    ("delete", "/api/v1/quota-policies/{feature}"),
    ("post", "/api/v1/quotas/check"),
    ("get", "/api/v1/usage"),
    ("post", "/api/v1/usage/consume"),
    ("post", "/api/v1/usage/reservations"),
    ("post", "/api/v1/usage/reservations/{id}/commit"),
    ("post", "/api/v1/usage/reservations/{id}/release"),
    ("post", "/api/v1/usage/leases"),
    ("post", "/api/v1/usage/leases/{id}/renew"),
    ("delete", "/api/v1/usage/leases/{id}"),
    ("post", "/api/v1/usage/gauge"),
    ("get", "/api/v1/usage/events"),
    ("get", "/api/v1/usage/alerts"),
    ("post", "/api/v1/usage/maintenance"),
];

#[tokio::test]
async fn openapi_documents_every_route_and_every_route_answers() {
    let api = Api::new().await;
    let (s, _, doc) = api.call(None, Method::GET, "/api/openapi.json", None).await;
    assert_eq!(s, StatusCode::OK);
    for (m, p) in OPERATIONS {
        assert!(
            doc["paths"][*p][*m].is_object(),
            "missing in OpenAPI: {m} {p}"
        );
        let tag = doc["paths"][*p][*m]["tags"][0].as_str().unwrap();
        assert!(["entitlements", "entitlements-catalog", "usage"].contains(&tag), "{p}: {tag}");
        // Every route exists: anonymous callers get 401, never a 404/405.
        let uri = p.replace("{id}", "x").replace("{key}", "x").replace("{feature}", "x");
        let method = Method::from_bytes(m.to_uppercase().as_bytes()).unwrap();
        let (s, _, j) = api.call(None, method, &uri, Some(json!({}))).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{m} {p}: {j}");
    }
    assert!(doc["components"]["schemas"]["Decision"].is_object());
    assert!(doc["components"]["schemas"]["SubscriptionSync"].is_object());
}

#[tokio::test]
async fn catalog_writes_are_for_platform_administrators() {
    let api = Api::new().await;
    let body = json!({"kind": "metered", "permissions": ["ai:*"], "unit": "generations"});
    let (s, j) = api.send(user("alice"), Method::PUT, "/api/v1/features/ai", body.clone()).await;
    assert_eq!((s, code(&j)), (StatusCode::FORBIDDEN, "forbidden"));
    let (s, j) = api.send(admin(), Method::PUT, "/api/v1/features/ai", body).await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(j["kind"], "metered");
    let plan = json!({"name": "Pro", "features": [{"feature": "ai", "limit": 2, "period": "month"}]});
    let (s, j) = api.send(admin(), Method::PUT, "/api/v1/plans/pro", plan).await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(j["features"][0]["limit"], 2);
    let bad = json!({"features": [{"feature": "ai", "limit": 2, "period": "none"}]});
    let (s, j) = api.send(admin(), Method::PUT, "/api/v1/plans/bad", bad).await;
    assert_eq!((s, code(&j)), (StatusCode::BAD_REQUEST, "invalid_period"));
    let (s, j) = api.get(user("alice"), "/api/v1/plans").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(j["items"][0]["key"], "pro");
    // A body error is not revealed before the caller is authorised.
    let (s, _, j) = api
        .call(Some(user("alice")), Method::PUT, "/api/v1/plans/x", None)
        .await;
    assert_eq!((s, code(&j)), (StatusCode::FORBIDDEN, "forbidden"));
    let sync = json!({"subject": "user:people/alice", "plan": "pro", "status": "active", "event_id": "e1"});
    let (s, j) = api.send(admin(), Method::PUT, "/api/v1/entitlements/subscription", sync.clone()).await;
    assert_eq!((s, j["result"].as_str()), (StatusCode::OK, Some("applied")));
    let (_, j) = api.send(admin(), Method::PUT, "/api/v1/entitlements/subscription", sync).await;
    assert_eq!(j["result"], "replayed");
    let (s, j) = api.get(user("alice"), "/api/v1/entitlements/subscription").await;
    assert_eq!((s, j["plan"].as_str()), (StatusCode::OK, Some("pro")));
    let (s, _) = api.send(admin(), Method::DELETE, "/api/v1/plans/pro", json!({})).await;
    assert_eq!(s, StatusCode::CONFLICT, "plan in use");
}

#[tokio::test]
async fn metering_over_rest_reports_quota_and_entitlement_denials() {
    let api = Api::new().await;
    let env = &api.env;
    env.feature("ai", FeatureKind::Metered, &[]).await;
    env.feature("export", FeatureKind::Metered, &[]).await;
    let mut off = pf("export", Some(5), Some("month"));
    off.enabled = false;
    env.plan("pro", vec![pf("ai", Some(2), Some("month")), off]).await;
    env.subscribe(&pk("alice"), "pro").await;
    let svc = AccessContext::service("billing-backend", true);
    let body = |units: i64, key: &str| json!({"feature": "ai", "units": units, "idempotency_key": key, "subject": "user:people/alice"});

    let (s, j) = api.send(svc.clone(), Method::POST, "/api/v1/usage/consume", body(1, "a")).await;
    assert_eq!(s, StatusCode::CREATED, "{j}");
    let (s, j2) = api.send(svc.clone(), Method::POST, "/api/v1/usage/consume", body(1, "a")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!((j2["replayed"].as_bool(), &j2["event_id"]), (Some(true), &j["event_id"]));
    let (s, j) = api.send(svc.clone(), Method::POST, "/api/v1/usage/consume", body(2, "b")).await;
    assert_eq!((s, code(&j)), (StatusCode::TOO_MANY_REQUESTS, "quota_exceeded"));
    assert_eq!(j["error"]["details"]["remaining"], 1);
    assert_eq!(j["error"]["details"]["allowed"], false);
    let (s, j) = api.send(svc.clone(), Method::POST, "/api/v1/usage/consume", json!({"feature": "ai"})).await;
    assert_eq!((s, code(&j)), (StatusCode::BAD_REQUEST, "idempotency_key_required"));
    let exp = json!({"feature": "export", "idempotency_key": "e", "subject": "user:people/alice"});
    let (s, j) = api.send(svc.clone(), Method::POST, "/api/v1/usage/consume", exp).await;
    assert_eq!((s, code(&j)), (StatusCode::FORBIDDEN, "feature_not_entitled"));
    // The subject itself may not meter (it could release its own holds).
    let (s, _) = api
        .send(user("alice"), Method::POST, "/api/v1/usage/consume", json!({"feature": "ai", "idempotency_key": "z"}))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // Reserve → partial commit; dry runs; usage; events.
    let (s, r) = api
        .send(svc.clone(), Method::POST, "/api/v1/usage/reservations", body(1, "r1"))
        .await;
    assert_eq!(s, StatusCode::CREATED, "{r}");
    let id = r["id"].as_str().unwrap();
    let (s, c) = api
        .send(svc.clone(), Method::POST, &format!("/api/v1/usage/reservations/{id}/commit"), json!({"units": 1}))
        .await;
    assert_eq!((s, c["status"].as_str()), (StatusCode::OK, Some("committed")));
    let (s, d) = api
        .send(user("alice"), Method::POST, "/api/v1/quotas/check", json!({"feature": "ai"}))
        .await;
    assert_eq!((s, d["allowed"].as_bool()), (StatusCode::OK, Some(false)));
    let (s, u) = api.get(user("alice"), "/api/v1/usage").await;
    assert_eq!(s, StatusCode::OK);
    let ai = u["items"].as_array().unwrap().iter().find(|x| x["feature"] == "ai").unwrap();
    assert_eq!((ai["used"].as_i64(), ai["limit"].as_i64(), ai["percent"].as_f64()), (Some(2), Some(2), Some(100.0)));
    let (s, e) = api.get(user("alice"), "/api/v1/usage/events").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(e["items"].as_array().unwrap().len(), 2);
    let (s, e) = api.get(user("bob"), "/api/v1/usage/events?subject=user:people/alice").await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{e}");
    let (s, h) = api.get(user("alice"), "/api/v1/entitlements/has?feature=ai").await;
    assert_eq!((s, h["has"].as_bool()), (StatusCode::OK, Some(true)));
    let (_, h) = api.get(user("alice"), "/api/v1/entitlements/has?feature=export").await;
    assert_eq!(h["has"], false);
    let (s, _) = api.get(user("alice"), "/api/v1/entitlements/has?feature=ai&plan=pro").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (_, h) = api.get(user("alice"), "/api/v1/entitlements/has?plan=pro").await;
    assert_eq!(h["has"], true);
}

#[tokio::test]
async fn plan_based_rate_limits_apply_per_subject_and_window() {
    let api = Api::new().await;
    let env = &api.env;
    env.feature("api_hourly", FeatureKind::Rate, &[]).await;
    env.feature("api_daily", FeatureKind::Rate, &[]).await;
    env.plan(
        "starter",
        vec![pf("api_hourly", Some(3), Some("hour")), pf("api_daily", Some(5), Some("day"))],
    )
    .await;
    env.subscribe(&pk("alice"), "starter").await;
    env.subscribe(&pk("bob"), "starter").await;
    for left in [2, 1, 0] {
        let (s, h, _) = api.call(Some(user("alice")), Method::GET, "/api/v1/entitlements", None).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(h["x-ratelimit-limit"], "3");
        assert_eq!(h["x-ratelimit-remaining"], left.to_string().as_str());
    }
    let (s, h, j) = api.call(Some(user("alice")), Method::GET, "/api/v1/entitlements", None).await;
    assert_eq!((s, code(&j)), (StatusCode::TOO_MANY_REQUESTS, "rate_limited"));
    assert_eq!(h["retry-after"], "3600");
    assert_eq!(j["error"]["details"]["feature"], "api_hourly");
    // Other subjects, administrators and routes outside /api/v1 are not affected.
    let (s, _, _) = api.call(Some(user("bob")), Method::GET, "/api/v1/entitlements", None).await;
    assert_eq!(s, StatusCode::OK);
    let (s, _, _) = api.call(Some(admin()), Method::GET, "/api/v1/plans", None).await;
    assert_eq!(s, StatusCode::OK);
    let (s, _, _) = api.call(Some(user("alice")), Method::GET, "/health", None).await;
    assert_eq!(s, StatusCode::OK);
    // Next hour: the daily window (5) still has 2 left, and is the tighter one.
    env.clock.set(T0 + HOUR);
    for left in [1, 0] {
        let (s, h, _) = api.call(Some(user("alice")), Method::GET, "/api/v1/entitlements", None).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(h["x-ratelimit-remaining"], left.to_string().as_str());
    }
    let (s, _, j) = api.call(Some(user("alice")), Method::GET, "/api/v1/entitlements", None).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(j["error"]["details"]["feature"], "api_daily");
    // A refused request does not use up the hourly window.
    env.clock.set(T0 + DAY);
    for _ in 0..3 {
        let (s, _, _) = api.call(Some(user("alice")), Method::GET, "/api/v1/entitlements", None).await;
        assert_eq!(s, StatusCode::OK);
    }
}
