//! `/api/v1/jobs` over HTTP: the real router and middleware, a real SQLite
//! database, identity injected like the auth layer will. Covers SPEC §27 #19
//! (schedule and execute a background job), the external worker protocol,
//! OpenAPI coverage and authorisation (anonymous denied).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::middleware::{self, Next};
use common::Store;
use forgebase_api::{AppState, RouterOptions};
use forgebase_core::DataLayout;
use forgebase_data::{AccessContext, DefaultAuthorizer};
use forgebase_jobs::{JobRegistry, JobService, PoolConfig, start_pool};
use serde_json::{Value as Json, json};
use tower::ServiceExt;

struct Api {
    app: Router,
    svc: JobService,
    clock: forgebase_jobs::ManualClock,
    _dir: Store,
}

impl Api {
    async fn new(ctx: Option<AccessContext>) -> Self {
        let dir = Store::new().await;
        let db = dir.open().await;
        let layout = DataLayout::new(dir.path().join("data"));
        let state = AppState::with_authorizer(db, layout, Arc::new(DefaultAuthorizer))
            .await
            .unwrap();
        let clock = forgebase_jobs::ManualClock::from_system();
        let svc = JobService::with_clock(state.data().clone(), Arc::new(clock.clone()));
        let mut app = forgebase_api::router(
            state,
            RouterOptions {
                extensions: vec![forgebase_jobs::extension(svc.clone())],
                ..RouterOptions::default()
            },
        );
        if let Some(ctx) = ctx {
            app = app.layer(middleware::from_fn(move |mut req: Request<Body>, next: Next| {
                let ctx = ctx.clone();
                async move {
                    req.extensions_mut().insert(ctx);
                    next.run(req).await
                }
            }));
        }
        Self {
            app,
            svc,
            clock,
            _dir: dir,
        }
    }

    async fn admin() -> Self {
        Self::new(Some(AccessContext::admin("root"))).await
    }

    async fn call(&self, method: Method, uri: &str, body: Option<Json>) -> (StatusCode, Json) {
        let mut req = Request::builder().method(method).uri(uri);
        let body = match body {
            Some(b) => {
                req = req.header("content-type", "application/json");
                Body::from(b.to_string())
            }
            None => Body::empty(),
        };
        let res = self.app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(Json::Null))
    }

    async fn get(&self, uri: &str) -> (StatusCode, Json) {
        self.call(Method::GET, uri, None).await
    }

    async fn post(&self, uri: &str, body: Json) -> (StatusCode, Json) {
        self.call(Method::POST, uri, Some(body)).await
    }
}

fn code(j: &Json) -> &str {
    j["error"]["code"].as_str().unwrap_or_default()
}

/// Every operation under `/api/v1/jobs`, as (method, path template).
const JOBS_OPERATIONS: &[(&str, &str)] = &[
    ("post", "/api/v1/jobs"),
    ("get", "/api/v1/jobs"),
    ("get", "/api/v1/jobs/stats"),
    ("post", "/api/v1/jobs/claim"),
    ("get", "/api/v1/jobs/schedules"),
    ("post", "/api/v1/jobs/schedules"),
    ("get", "/api/v1/jobs/schedules/{id}"),
    ("patch", "/api/v1/jobs/schedules/{id}"),
    ("delete", "/api/v1/jobs/schedules/{id}"),
    ("get", "/api/v1/jobs/workers"),
    ("post", "/api/v1/jobs/workers"),
    ("get", "/api/v1/jobs/{id}"),
    ("delete", "/api/v1/jobs/{id}"),
    ("get", "/api/v1/jobs/{id}/events"),
    ("post", "/api/v1/jobs/{id}/cancel"),
    ("post", "/api/v1/jobs/{id}/retry"),
    ("post", "/api/v1/jobs/{id}/heartbeat"),
    ("post", "/api/v1/jobs/{id}/complete"),
    ("post", "/api/v1/jobs/{id}/fail"),
    // FB-64
    ("get", "/api/v1/jobs/queues"),
    ("get", "/api/v1/jobs/queues/{name}"),
    ("put", "/api/v1/jobs/queues/{name}"),
    ("delete", "/api/v1/jobs/queues/{name}"),
    ("post", "/api/v1/jobs/queues/{name}/pause"),
    ("post", "/api/v1/jobs/queues/{name}/resume"),
    ("get", "/api/v1/jobs/workflows"),
    ("post", "/api/v1/jobs/workflows"),
    ("get", "/api/v1/jobs/workflows/{id}"),
    ("post", "/api/v1/jobs/workflows/{id}/cancel"),
    ("post", "/api/v1/jobs/{id}/progress"),
    ("get", "/api/v1/jobs/{id}/checkpoint"),
    ("post", "/api/v1/jobs/{id}/checkpoint"),
    ("get", "/api/v1/jobs/{id}/result"),
    ("get", "/api/v1/jobs/{id}/attempts"),
    ("post", "/api/v1/jobs/{id}/children"),
];

/// Operations added by FB-64: their operationIds carry the `jobs_` prefix.
const FB64_PATHS: &[&str] = &[
    "/api/v1/jobs/queues",
    "/api/v1/jobs/workflows",
    "/progress",
    "/checkpoint",
    "/result",
    "/attempts",
    "/children",
];

#[tokio::test]
async fn openapi_documents_every_jobs_route_and_every_route_exists() {
    let api = Api::admin().await;
    let (s, doc) = api.get("/api/openapi.json").await;
    assert_eq!(s, StatusCode::OK);

    let mut documented = Vec::new();
    for (path, item) in doc["paths"].as_object().unwrap() {
        if path.starts_with("/api/v1/jobs") {
            for method in item.as_object().unwrap().keys() {
                documented.push((method.clone(), path.clone()));
            }
        }
    }
    for (m, p) in JOBS_OPERATIONS {
        assert!(
            documented.contains(&((*m).to_owned(), (*p).to_owned())),
            "missing in OpenAPI: {m} {p}"
        );
    }
    assert_eq!(
        documented.len(),
        JOBS_OPERATIONS.len(),
        "documented but not expected: {documented:?}"
    );

    // The core API is still documented next to it, and error responses were added.
    assert!(doc["paths"]["/api/v1/collections"].is_object());
    let enqueue = &doc["paths"]["/api/v1/jobs"]["post"];
    assert_eq!(
        enqueue["responses"]["401"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/ErrorEnvelope"
    );
    assert!(doc["paths"]["/api/v1/jobs/{id}"]["get"]["responses"]["404"].is_object());
    for name in ["Job", "NewJob", "ClaimedJob", "Schedule", "JobStats"] {
        assert!(doc["components"]["schemas"][name].is_object(), "schema {name}");
    }
    // Every $ref resolves, every jobs operation has a tag and responses.
    let text = doc.to_string();
    for part in text.split("\"$ref\":\"#/components/schemas/").skip(1) {
        let name = part.split('"').next().unwrap();
        assert!(doc["components"]["schemas"][name].is_object(), "dangling $ref {name}");
    }
    for (m, p) in &documented {
        let op = &doc["paths"][p][m];
        assert_eq!(op["tags"][0], "jobs", "{m} {p}");
        if FB64_PATHS.iter().any(|f| p.starts_with(f) || p.ends_with(f)) {
            let id = op["operationId"].as_str().unwrap_or_default();
            assert!(id.starts_with("jobs_"), "{m} {p} has operationId {id:?}");
        }
        assert!(op["responses"].as_object().is_some_and(|r| !r.is_empty()), "{m} {p}");
    }

    // Documented means routed: no request gets the router's 404/405.
    for (m, p) in JOBS_OPERATIONS {
        let uri = p.replace("{id}", "x1").replace("{name}", "x1");
        let method = Method::from_bytes(m.to_ascii_uppercase().as_bytes()).unwrap();
        let (status, body) = api.call(method, &uri, Some(json!({}))).await;
        assert_ne!(status, StatusCode::METHOD_NOT_ALLOWED, "{m} {p}");
        assert!(
            !(status == StatusCode::NOT_FOUND && body["error"]["message"] == "no route for this path"),
            "{m} {p} is documented but not routed"
        );
    }
}

#[tokio::test]
async fn anonymous_is_denied_everywhere_and_ordinary_users_are_forbidden() {
    // No identity layer at all: what the server does until FB-20 lands.
    let anon = Api::new(None).await;
    // A signed-in user the default (fail-closed) authorizer knows nothing about.
    let user = Api::new(Some(AccessContext::user("users", "u1"))).await;
    for (m, p) in JOBS_OPERATIONS {
        let uri = p.replace("{id}", "x1").replace("{name}", "x1");
        let method = Method::from_bytes(m.to_ascii_uppercase().as_bytes()).unwrap();
        let body =
            json!({"kind": "k", "worker_id": "w", "lease_id": "l", "error": "e", "name": "n", "cron": "* * * * *"});
        let (s, j) = anon.call(method.clone(), &uri, Some(body.clone())).await;
        assert_eq!(
            (s, code(&j)),
            (StatusCode::UNAUTHORIZED, "unauthenticated"),
            "anonymous {m} {p}"
        );
        assert!(j["error"]["request_id"].as_str().is_some_and(|r| !r.is_empty()));
        let (s, j) = user.call(method, &uri, Some(body)).await;
        assert_eq!((s, code(&j)), (StatusCode::FORBIDDEN, "forbidden"), "user {m} {p}");
    }
    // Denied requests changed nothing.
    let stats = anon.svc.stats(&AccessContext::system()).await.unwrap();
    assert!(stats.totals.values().all(|n| *n == 0));
}

/// SPEC §27 #19: schedule and execute a background job.
#[tokio::test]
async fn scenario_19_schedule_and_execute_a_background_job() {
    let api = Api::admin().await;
    let registry = JobRegistry::new()
        .register("send_email", |ctx: forgebase_jobs::JobContext| async move {
            Ok(json!({"sent_to": ctx.payload()["recipient"]}))
        })
        .register("nightly_report", |_ctx: forgebase_jobs::JobContext| async {
            Ok(json!("report built"))
        });
    let pool = start_pool(
        api.svc.clone(),
        registry,
        PoolConfig {
            poll_interval: Duration::from_millis(20),
            maintenance_interval: Duration::from_millis(20),
            ..PoolConfig::default()
        },
    );

    // A delayed job, enqueued over HTTP.
    let (s, job) = api
        .post("/api/v1/jobs", json!({"kind": "send_email", "payload": {"recipient": "user@example.com"}, "delay_ms": 600, "idempotency_key": "welcome-1"}))
        .await;
    assert_eq!(s, StatusCode::CREATED, "{job}");
    assert_eq!(job["status"], "queued");
    let id = job["id"].as_str().unwrap().to_owned();
    assert!(job.get("lease_id").is_none(), "the lease never leaks through reads");

    // Same key again: the first job, 200.
    let (s, again) = api
        .post(
            "/api/v1/jobs",
            json!({"kind": "send_email", "payload": {}, "idempotency_key": "welcome-1"}),
        )
        .await;
    assert_eq!((s, again["id"].as_str()), (StatusCode::OK, Some(id.as_str())));

    // And a schedule that fires every second.
    let (s, sched) = api
        .post(
            "/api/v1/jobs/schedules",
            json!({"name": "report", "cron": "* * * * * *", "kind": "nightly_report", "payload": {"scope": "all"}}),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{sched}");

    // It does not run early ... (time only moves when the test moves it)
    api.clock.advance(Duration::from_millis(200));
    let (_, now) = api.get(&format!("/api/v1/jobs/{id}")).await;
    assert_eq!(now["status"], "queued");
    // ... and it runs once it is due.
    api.clock.advance(Duration::from_millis(500));
    let done = common::wait_for(Duration::from_secs(30), || async {
        let (_, j) = api.get(&format!("/api/v1/jobs/{id}")).await;
        (j["status"] == "succeeded").then_some(j)
    })
    .await;
    assert_eq!(done["result"], json!({"sent_to": "user@example.com"}));
    assert_eq!(done["attempts"], 1);

    let (_, events) = api.get(&format!("/api/v1/jobs/{id}/events")).await;
    let names: Vec<_> = events["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["event"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["enqueued", "claimed", "succeeded"]);

    // The schedule produced jobs that ran.
    common::wait_for(Duration::from_secs(30), || async {
        api.clock.advance(Duration::from_millis(500));
        let (_, l) = api.get("/api/v1/jobs?kind=nightly_report&status=succeeded").await;
        (l["total"].as_i64().unwrap() >= 2).then_some(())
    })
    .await;
    let (_, sched_now) = api
        .get(&format!("/api/v1/jobs/schedules/{}", sched["id"].as_str().unwrap()))
        .await;
    assert!(sched_now["last_run_at"].is_string());
    let (_, stats) = api.get("/api/v1/jobs/stats").await;
    assert!(stats["totals"]["succeeded"].as_i64().unwrap() >= 3);
    pool.shutdown().await;
}

#[tokio::test]
async fn external_worker_protocol() {
    let api = Api::admin().await;
    let (s, w) = api
        .post(
            "/api/v1/jobs/workers",
            json!({"name": "py-worker", "queues": ["mail"], "concurrency": 2}),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{w}");
    let worker = w["id"].as_str().unwrap().to_owned();

    let (_, job) = api
        .post("/api/v1/jobs", json!({"kind": "send_email", "queue": "mail", "payload": {"n": 1}, "max_attempts": 2, "backoff_base_ms": 0, "backoff_max_ms": 0}))
        .await;
    let id = job["id"].as_str().unwrap().to_owned();

    // Nothing for another queue.
    let (s, none) = api
        .post("/api/v1/jobs/claim", json!({"worker_id": worker, "queues": ["other"]}))
        .await;
    assert_eq!((s, none["jobs"].as_array().unwrap().len()), (StatusCode::OK, 0));

    // Claim, heartbeat, complete.
    let (s, claimed) = api
        .post(
            "/api/v1/jobs/claim",
            json!({"worker_id": worker, "queues": ["mail"], "max": 5}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{claimed}");
    let first = &claimed["jobs"][0];
    assert_eq!(first["job"]["id"], id.as_str());
    assert_eq!(first["job"]["status"], "running");
    let lease = first["lease_id"].as_str().unwrap().to_owned();

    let (s, hb) = api
        .post(&format!("/api/v1/jobs/{id}/heartbeat"), json!({"lease_id": lease}))
        .await;
    assert_eq!(
        (s, hb["cancel_requested"].as_bool()),
        (StatusCode::OK, Some(false)),
        "{hb}"
    );
    let (s, e) = api
        .post(&format!("/api/v1/jobs/{id}/heartbeat"), json!({"lease_id": "wrong"}))
        .await;
    assert_eq!((s, code(&e)), (StatusCode::CONFLICT, "lease_lost"));

    // First failure retries (no backoff configured), second exhausts the 2 attempts.
    let (s, failed) = api
        .post(
            &format!("/api/v1/jobs/{id}/fail"),
            json!({"lease_id": lease, "error": "smtp down"}),
        )
        .await;
    assert_eq!(
        (s, failed["status"].as_str()),
        (StatusCode::OK, Some("queued")),
        "{failed}"
    );
    let (_, again) = api.post("/api/v1/jobs/claim", json!({"worker_id": worker})).await;
    let lease2 = again["jobs"][0]["lease_id"].as_str().unwrap().to_owned();
    assert_ne!(lease, lease2);
    // The old lease is dead.
    let (s, e) = api
        .post(&format!("/api/v1/jobs/{id}/complete"), json!({"lease_id": lease}))
        .await;
    assert_eq!((s, code(&e)), (StatusCode::CONFLICT, "lease_lost"));
    let (_, dead) = api
        .post(
            &format!("/api/v1/jobs/{id}/fail"),
            json!({"lease_id": lease2, "error": "still down"}),
        )
        .await;
    assert_eq!(dead["status"], "dead");
    let (_, dlq) = api.get("/api/v1/jobs?status=dead").await;
    assert_eq!(
        (dlq["total"].as_i64(), dlq["items"][0]["last_error"].as_str()),
        (Some(1), Some("still down"))
    );

    // Back from the dead-letter queue, then complete exactly once.
    let (s, revived) = api.post(&format!("/api/v1/jobs/{id}/retry"), json!({})).await;
    assert_eq!(
        (s, revived["status"].as_str(), revived["attempts"].as_i64()),
        (StatusCode::OK, Some("queued"), Some(0))
    );
    let (_, c3) = api.post("/api/v1/jobs/claim", json!({"worker_id": worker})).await;
    let lease3 = c3["jobs"][0]["lease_id"].as_str().unwrap().to_owned();
    let (s, done) = api
        .post(
            &format!("/api/v1/jobs/{id}/complete"),
            json!({"lease_id": lease3, "result": {"ok": true}}),
        )
        .await;
    assert_eq!(
        (s, done["status"].as_str(), done["result"].clone()),
        (StatusCode::OK, Some("succeeded"), json!({"ok": true}))
    );
    let (s, e) = api
        .post(&format!("/api/v1/jobs/{id}/complete"), json!({"lease_id": lease3}))
        .await;
    assert_eq!(
        (s, code(&e)),
        (StatusCode::CONFLICT, "lease_lost"),
        "a second completion is rejected"
    );

    // The worker shows up as seen.
    let (_, workers) = api.get("/api/v1/jobs/workers").await;
    assert!(
        workers["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["id"] == worker.as_str() && w["online"] == true)
    );
}

#[tokio::test]
async fn cancel_retry_delete_and_validation_over_http() {
    let api = Api::admin().await;
    let (_, j) = api.post("/api/v1/jobs", json!({"kind": "k"})).await;
    let id = j["id"].as_str().unwrap();

    let (s, e) = api.call(Method::DELETE, &format!("/api/v1/jobs/{id}"), None).await;
    assert_eq!((s, code(&e)), (StatusCode::CONFLICT, "job_active"));
    let (s, c) = api.post(&format!("/api/v1/jobs/{id}/cancel"), json!({})).await;
    assert_eq!((s, c["status"].as_str()), (StatusCode::OK, Some("cancelled")));
    let (s, e) = api.post(&format!("/api/v1/jobs/{id}/cancel"), json!({})).await;
    assert_eq!((s, code(&e)), (StatusCode::CONFLICT, "job_finished"));
    let (s, _) = api.call(Method::DELETE, &format!("/api/v1/jobs/{id}"), None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, e) = api.get(&format!("/api/v1/jobs/{id}")).await;
    assert_eq!((s, code(&e)), (StatusCode::NOT_FOUND, "job_not_found"));

    for (body, want) in [
        (json!({"kind": ""}), "invalid_job"),
        (json!({"kind": "has space"}), "invalid_job"),
        (json!({"kind": "k", "priority": 99999}), "invalid_job"),
        (json!({"kind": "k", "max_attempts": 0}), "invalid_job"),
        (json!({"kind": "k", "run_at": "tomorrow"}), "invalid_job"),
        (
            json!({"kind": "k", "delay_ms": 5, "run_at": "2030-01-01T00:00:00Z"}),
            "invalid_job",
        ),
        (
            json!({"kind": "k", "payload": "x".repeat(300_000)}),
            "payload_too_large",
        ),
        (json!({"kind": "k", "typo": 1}), "invalid_body"),
        (json!({"payload": 1}), "invalid_body"),
    ] {
        let (s, e) = api.post("/api/v1/jobs", body.clone()).await;
        assert_eq!((s, code(&e)), (StatusCode::BAD_REQUEST, want), "{body}");
    }
    let (s, e) = api.get("/api/v1/jobs?status=bogus").await;
    assert_eq!((s, code(&e)), (StatusCode::BAD_REQUEST, "invalid_parameter"));
    let (s, e) = api
        .post("/api/v1/jobs/claim", json!({"worker_id": "w", "max": 1000}))
        .await;
    assert_eq!((s, code(&e)), (StatusCode::BAD_REQUEST, "invalid_claim"));
    let (s, e) = api
        .post("/api/v1/jobs/claim", json!({"worker_id": "bad worker!"}))
        .await;
    assert_eq!((s, code(&e)), (StatusCode::BAD_REQUEST, "invalid_job"));
}

#[tokio::test]
async fn schedules_over_http() {
    let api = Api::admin().await;
    let (s, created) = api
        .post(
            "/api/v1/jobs/schedules",
            json!({"name": "nightly", "cron": "0 3 * * *", "kind": "report", "priority": 5}),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{created}");
    assert_eq!(
        (created["enabled"].as_bool(), created["priority"].as_i64()),
        (Some(true), Some(5))
    );
    let id = created["id"].as_str().unwrap().to_owned();

    let (s, dup) = api
        .post(
            "/api/v1/jobs/schedules",
            json!({"name": "nightly", "cron": "0 3 * * *", "kind": "report"}),
        )
        .await;
    assert_eq!((s, code(&dup)), (StatusCode::CONFLICT, "schedule_exists"));
    let (s, bad) = api
        .post(
            "/api/v1/jobs/schedules",
            json!({"name": "x", "cron": "every day", "kind": "report"}),
        )
        .await;
    assert_eq!((s, code(&bad)), (StatusCode::BAD_REQUEST, "invalid_cron"));

    let (_, list) = api.get("/api/v1/jobs/schedules").await;
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    let (s, patched) = api
        .call(
            Method::PATCH,
            &format!("/api/v1/jobs/schedules/{id}"),
            Some(json!({"enabled": false, "cron": "*/5 * * * *"})),
        )
        .await;
    assert_eq!(
        (s, patched["enabled"].as_bool(), patched["cron"].as_str()),
        (StatusCode::OK, Some(false), Some("*/5 * * * *"))
    );
    let (s, _) = api
        .call(Method::DELETE, &format!("/api/v1/jobs/schedules/{id}"), None)
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, e) = api.get(&format!("/api/v1/jobs/schedules/{id}")).await;
    assert_eq!((s, code(&e)), (StatusCode::NOT_FOUND, "schedule_not_found"));
}

/// FB-64 over HTTP: an external worker reports progress, saves a
/// checkpoint, spawns a child and completes; queue settings, workflows,
/// attempts and results are served.
#[tokio::test]
async fn long_running_tasks_over_http() {
    let api = Api::admin().await;
    let (s, q) = api
        .call(
            Method::PUT,
            "/api/v1/jobs/queues/media",
            Some(json!({"concurrency": 1, "rate_limit": 10, "rate_period": "minute", "default_timeout_ms": 3600000})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{q}");
    assert_eq!(
        (q["concurrency"].as_i64(), q["rate_period"].as_str()),
        (Some(1), Some("minute"))
    );
    let (s, _) = api
        .call(
            Method::PUT,
            "/api/v1/jobs/queues/media",
            Some(json!({"concurrency": 0})),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    let (s, job) = api
        .post(
            "/api/v1/jobs",
            json!({"kind": "transcode", "queue": "media", "payload": {"file": "a.mov"}}),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(job["timeout_ms"], 3_600_000);
    assert_eq!(job["waiting_for"], 0);
    let id = job["id"].as_str().unwrap().to_owned();
    let (_, claimed) = api
        .post("/api/v1/jobs/claim", json!({"worker_id": "ext-1", "max": 5}))
        .await;
    let lease = claimed["jobs"][0]["lease_id"].as_str().unwrap().to_owned();
    assert!(claimed["jobs"][0]["checkpoint"].is_null());
    assert!(claimed["jobs"][0]["job"]["deadline_at"].is_string());

    let (s, hb) = api
        .post(
            &format!("/api/v1/jobs/{id}/progress"),
            json!({"lease_id": lease, "percent": 25.0, "stage": "audio", "message": "1 of 4"}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{hb}");
    assert_eq!(hb["cancel_requested"], false);
    let (s, _) = api
        .post(
            &format!("/api/v1/jobs/{id}/progress"),
            json!({"lease_id": "nope", "percent": 30}),
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _) = api
        .post(
            &format!("/api/v1/jobs/{id}/checkpoint"),
            json!({"lease_id": lease, "checkpoint": {"segment": 2}}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (_, cp) = api.get(&format!("/api/v1/jobs/{id}/checkpoint")).await;
    assert_eq!(cp["checkpoint"], json!({"segment": 2}));
    let (_, j) = api.get(&format!("/api/v1/jobs/{id}")).await;
    assert_eq!(j["progress"]["stage"], "audio");
    assert_eq!(j["has_checkpoint"], true);

    let (s, kids) = api
        .post(
            &format!("/api/v1/jobs/{id}/children"),
            json!({"lease_id": lease, "jobs": [{"kind": "thumb", "payload": 1}]}),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{kids}");
    assert_eq!(kids["items"][0]["parent_id"], id.as_str());
    let (_, listed) = api.get(&format!("/api/v1/jobs?parent_id={id}")).await;
    assert_eq!(listed["total"], 1);

    let (s, _) = api.get(&format!("/api/v1/jobs/{id}/result")).await;
    assert_eq!(s, StatusCode::CONFLICT, "no result before success");
    let (s, _) = api
        .post(
            &format!("/api/v1/jobs/{id}/complete"),
            json!({"lease_id": lease, "result": {"out": "a.mp4"}}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, r) = api.get(&format!("/api/v1/jobs/{id}/result")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        (r["result"]["out"].as_str(), r["stored"].as_bool()),
        (Some("a.mp4"), Some(false))
    );
    let (_, attempts) = api.get(&format!("/api/v1/jobs/{id}/attempts")).await;
    assert_eq!(attempts["items"][0]["outcome"], "succeeded");
    assert_eq!(attempts["items"][0]["worker_id"], "ext-1");

    let (s, p) = api.post("/api/v1/jobs/queues/media/pause", json!({})).await;
    assert_eq!((s, p["paused"].as_bool()), (StatusCode::OK, Some(true)));
    let (_, p) = api.post("/api/v1/jobs/queues/media/resume", json!({})).await;
    assert_eq!(p["paused"], false);
    let (_, all) = api.get("/api/v1/jobs/queues").await;
    assert!(all["items"].as_array().unwrap().iter().any(|q| q["name"] == "media"));
    let (s, _) = api.call(Method::DELETE, "/api/v1/jobs/queues/media", None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    let (s, wf) = api
        .post(
            "/api/v1/jobs/workflows",
            json!({"name": "pipeline", "on_failure": "compensate", "steps": [
                {"name": "a", "kind": "k"},
                {"name": "b", "kind": "k", "depends_on": ["a"], "compensate": {"kind": "undo"}}
            ]}),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{wf}");
    assert_eq!(wf["status"], "running");
    assert_eq!(wf["steps"][1]["depends_on"][0], wf["steps"][0]["job"]["id"]);
    let wid = wf["id"].as_str().unwrap().to_owned();
    let (_, got) = api.get(&format!("/api/v1/jobs/workflows/{wid}")).await;
    assert_eq!(got["on_failure"], "compensate");
    let (_, page) = api.get("/api/v1/jobs/workflows?status=running").await;
    assert_eq!(page["total"], 1);
    let (s, c) = api
        .post(&format!("/api/v1/jobs/workflows/{wid}/cancel"), json!({}))
        .await;
    assert_eq!((s, c["status"].as_str()), (StatusCode::OK, Some("cancelled")));
    let (s, e) = api
        .post(
            "/api/v1/jobs/workflows",
            json!({"name": "bad", "steps": [{"name": "a", "kind": "k", "depends_on": ["a"]}]}),
        )
        .await;
    assert_eq!((s, code(&e)), (StatusCode::BAD_REQUEST, "invalid_workflow"));
    let (s, _) = api.get("/api/v1/jobs/workflows/wf-missing").await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}
