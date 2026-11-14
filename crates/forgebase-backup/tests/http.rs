//! `/api/v1/system/backups` over HTTP: the real router, a real database,
//! identity injected the way the auth layer does.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::middleware::{self, Next};
use forgebase_api::{AppState, RouterOptions};
use forgebase_backup::BackupService;
use forgebase_data::{AccessContext, DefaultAuthorizer};
use serde_json::{Value as Json, json};
use tower::ServiceExt;

struct Api {
    app: Router,
    layout: forgebase_core::DataLayout,
    jobs: forgebase_jobs::JobService,
    _dir: tempfile::TempDir,
}

impl Api {
    async fn new(ctx: AccessContext) -> Self {
        Self::build(ctx, false).await
    }

    /// With the drill API wired to a job service (`can_drill`: the process may run drills).
    async fn build(ctx: AccessContext, can_drill: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let inst = common::open(&dir.path().join("data")).await;
        inst.seed(3).await;
        let state = AppState::with_authorizer(inst.db.clone(), inst.layout.clone(), Arc::new(DefaultAuthorizer))
            .await
            .unwrap();
        let jobs = forgebase_jobs::JobService::new(state.data().clone());
        let svc = BackupService::new(inst.db.clone(), inst.layout.clone()).with_drills(jobs.clone(), can_drill);
        let app = forgebase_api::router(
            state,
            RouterOptions {
                extensions: vec![forgebase_backup::extension(svc)],
                ..RouterOptions::default()
            },
        )
        .layer(middleware::from_fn(move |mut req: Request<Body>, next: Next| {
            let ctx = ctx.clone();
            async move {
                req.extensions_mut().insert(ctx);
                next.run(req).await
            }
        }));
        Self {
            app,
            layout: inst.layout,
            jobs,
            _dir: dir,
        }
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
        let bytes = axum::body::to_bytes(res.into_body(), 64 << 20).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(Json::Null))
    }
}

#[tokio::test]
async fn admin_manages_snapshots_over_http() {
    let api = Api::new(AccessContext::admin("root")).await;
    let (s, list) = api.call(Method::GET, "/api/v1/system/backups", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(list["items"], json!([]));

    let (s, m) = api
        .call(
            Method::POST,
            "/api/v1/system/backups",
            Some(json!({"label": "nightly"})),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{m}");
    assert_eq!(m["kind"], "snapshot");
    assert_eq!(m["label"], "nightly");
    assert_eq!(
        m["master_key"]["included"], false,
        "the key is never copied through the API"
    );
    let id = m["id"].as_str().unwrap().to_owned();
    assert!(
        api.layout
            .backups_dir()
            .join("snapshots")
            .join(&id)
            .join("MANIFEST.json")
            .is_file()
    );

    let (_, list) = api.call(Method::GET, "/api/v1/system/backups", None).await;
    assert_eq!(list["items"][0]["id"], json!(id));
    let (s, one) = api
        .call(Method::GET, &format!("/api/v1/system/backups/{id}"), None)
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(one["manifest"]["schema_version"], m["schema_version"]);

    let (s, v) = api
        .call(Method::POST, &format!("/api/v1/system/backups/{id}/verify"), None)
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["ok"], true, "{v}");
    // `latest` names the newest snapshot, as in the CLI.
    let (s, newest) = api.call(Method::GET, "/api/v1/system/backups/latest", None).await;
    assert_eq!(s, StatusCode::OK, "{newest}");
    assert_eq!(newest["id"], json!(id));
    let (s, v) = api
        .call(Method::POST, "/api/v1/system/backups/latest/verify", None)
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["ok"], true, "{v}");

    // Retention through the create call and through prune.
    for _ in 0..2 {
        api.call(
            Method::POST,
            "/api/v1/system/backups",
            Some(json!({"retention": {"keep_last": 3}})),
        )
        .await;
    }
    let (s, pruned) = api
        .call(
            Method::POST,
            "/api/v1/system/backups/prune",
            Some(json!({"keep_last": 1})),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(pruned["deleted"].as_array().unwrap().len(), 2);
    let (_, list) = api.call(Method::GET, "/api/v1/system/backups", None).await;
    let left = list["items"][0]["id"].as_str().unwrap().to_owned();
    assert_eq!(list["items"].as_array().unwrap().len(), 1);

    let (s, _) = api.call(Method::DELETE, "/api/v1/system/backups/latest", None).await;
    assert_eq!(s, StatusCode::NO_CONTENT, "`latest` deletes the newest ({left})");
    let (s, e) = api
        .call(Method::GET, &format!("/api/v1/system/backups/{left}"), None)
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(e["error"]["code"], "backup_not_found");
    let (s, e) = api.call(Method::GET, "/api/v1/system/backups/..%2Fetc", None).await;
    assert!(s == StatusCode::BAD_REQUEST || s == StatusCode::NOT_FOUND, "{s} {e}");
}

#[tokio::test]
async fn only_administrators_may_use_the_backup_api() {
    let anon = Api::new(AccessContext::anonymous()).await;
    let (s, e) = anon.call(Method::GET, "/api/v1/system/backups", None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(e["error"]["code"], "unauthenticated");
    let (s, _) = anon.call(Method::POST, "/api/v1/system/backups", Some(json!({}))).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    let user = Api::new(AccessContext::user("users", "u1")).await;
    for (m, uri) in [
        (Method::GET, "/api/v1/system/backups"),
        (Method::POST, "/api/v1/system/backups/prune"),
        (Method::DELETE, "/api/v1/system/backups/x"),
    ] {
        let body = (m == Method::POST).then(|| json!({}));
        let (s, e) = user.call(m, uri, body).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{uri}");
        assert_eq!(e["error"]["code"], "forbidden");
    }
}

#[tokio::test]
async fn openapi_documents_every_backup_route() {
    let api = Api::new(AccessContext::admin("root")).await;
    let (s, doc) = api.call(Method::GET, "/api/openapi.json", None).await;
    assert_eq!(s, StatusCode::OK);
    for (method, path) in [
        ("get", "/api/v1/system/backups"),
        ("post", "/api/v1/system/backups"),
        ("get", "/api/v1/system/backups/{id}"),
        ("delete", "/api/v1/system/backups/{id}"),
        ("post", "/api/v1/system/backups/{id}/verify"),
        ("post", "/api/v1/system/backups/prune"),
        ("post", "/api/v1/system/backups/{id}/drill"),
        ("get", "/api/v1/system/backups/{id}/drills"),
    ] {
        assert!(
            doc["paths"][path][method].is_object(),
            "{method} {path} is not documented"
        );
    }
}

fn drill_report(id: &str, backup_id: &str, outcome: &str) -> Json {
    json!({
        "id": id, "backup_id": backup_id, "backup_created": "2026-11-14T00:00:00Z", "engine": "sqlite",
        "schema_version": 66, "started_at": "2026-11-14T01:00:00Z", "finished_at": "2026-11-14T01:00:31Z",
        "outcome": outcome, "rto_seconds": 31.5, "rpo_seconds": 3600,
        "phases": {"restore_ms": 10000, "start_ms": 6000, "checks_ms": 15500},
        "counts": {"collections": 2, "records": 8, "records_sampled": 8, "storage_objects": 0, "blobs_sampled": 0},
        "checks": [{"name": "restore", "status": "ok", "detail": "restored"}],
        "problems": [], "trigger": "api"
    })
}

/// The drill endpoints: administrators only, the history of a snapshot with
/// its RTO and RPO, and the refusals of the start call.
#[tokio::test]
async fn the_drill_api_lists_history_and_refuses_what_it_cannot_do() {
    // Not an administrator.
    let anon = Api::new(AccessContext::anonymous()).await;
    let (s, _) = anon
        .call(Method::POST, "/api/v1/system/backups/latest/drill", None)
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let user = Api::new(AccessContext::user("users", "u1")).await;
    for (m, uri) in [
        (Method::POST, "/api/v1/system/backups/x/drill"),
        (Method::GET, "/api/v1/system/backups/x/drills"),
    ] {
        let (s, e) = user.call(m, uri, None).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{uri}");
        assert_eq!(e["error"]["code"], "forbidden");
    }

    // A process that is not the forgebase command cannot run one.
    let off = Api::new(AccessContext::admin("root")).await;
    let (s, e) = off
        .call(Method::POST, "/api/v1/system/backups/latest/drill", None)
        .await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{e}");
    assert_eq!(e["error"]["code"], "drill_unavailable");

    // One that can: 404, 400, 202 and 409.
    let api = Api::build(AccessContext::admin("root"), true).await;
    let (s, e) = api.call(Method::POST, "/api/v1/system/backups/nope/drill", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{e}");
    let (s, m) = api
        .call(Method::POST, "/api/v1/system/backups", Some(json!({"label": "x"})))
        .await;
    assert_eq!(s, StatusCode::CREATED, "{m}");
    let id = m["id"].as_str().unwrap().to_owned();
    let (s, e) = api
        .call(
            Method::POST,
            &format!("/api/v1/system/backups/{id}/drill?sample=0"),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{e}");
    let (s, e) = api
        .call(
            Method::POST,
            &format!("/api/v1/system/backups/{id}/drill?sample=many"),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{e}");

    let (s, started) = api
        .call(Method::POST, "/api/v1/system/backups/latest/drill?sample=3", None)
        .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{started}");
    assert_eq!(started["backup_id"], json!(id), "`latest` is resolved to the snapshot");
    // It is a job of the reserved kind, with the request's settings.
    let job = api
        .jobs
        .get(&AccessContext::system(), started["job_id"].as_str().unwrap())
        .await
        .unwrap();
    assert_eq!(job.kind, forgebase_backup::drill::DRILL_JOB);
    assert_eq!(job.payload["backup_id"], json!(id));
    assert_eq!(job.payload["trigger"], "api");
    assert_eq!(job.payload["sample"], 3);
    // One at a time.
    let (s, e) = api
        .call(Method::POST, &format!("/api/v1/system/backups/{id}/drill"), None)
        .await;
    assert_eq!(s, StatusCode::CONFLICT, "{e}");
    assert_eq!(e["error"]["code"], "drill_running");

    // History: empty, then what the store holds, newest first, for that snapshot only.
    let (s, h) = api
        .call(Method::GET, &format!("/api/v1/system/backups/{id}/drills"), None)
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(h["items"], json!([]));
    let store = forgebase_backup::drill::DrillStore::new(&api.layout);
    for (rid, backup, outcome, finished) in [
        ("20261114T010000-0001", id.as_str(), "passed", "2026-11-14T01:00:31Z"),
        ("20261115T010000-0001", id.as_str(), "failed", "2026-11-15T01:00:31Z"),
        ("20261116T010000-0001", "another", "passed", "2026-11-16T01:00:31Z"),
    ] {
        let mut r = drill_report(rid, backup, outcome);
        r["started_at"] = json!(finished);
        r["finished_at"] = json!(finished);
        store.record(&serde_json::from_value(r).unwrap()).unwrap();
    }
    let (s, h) = api
        .call(Method::GET, &format!("/api/v1/system/backups/{id}/drills"), None)
        .await;
    assert_eq!(s, StatusCode::OK);
    let items = h["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "{h}");
    assert_eq!(items[0]["outcome"], "failed");
    assert_eq!(items[1]["outcome"], "passed");
    assert_eq!(items[1]["rto_seconds"], 31.5);
    assert_eq!(items[1]["rpo_seconds"], 3600);
    let (s, latest) = api
        .call(Method::GET, "/api/v1/system/backups/latest/drills", None)
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(latest["backup_id"], json!(id));
    // The history outlives the snapshot.
    let (s, h) = api
        .call(Method::GET, "/api/v1/system/backups/another/drills", None)
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(h["items"].as_array().unwrap().len(), 1);
}
