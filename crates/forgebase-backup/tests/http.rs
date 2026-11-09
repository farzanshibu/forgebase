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
    _dir: tempfile::TempDir,
}

impl Api {
    async fn new(ctx: AccessContext) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let inst = common::open(&dir.path().join("data")).await;
        inst.seed(3).await;
        let state = AppState::with_authorizer(inst.db.clone(), inst.layout.clone(), Arc::new(DefaultAuthorizer))
            .await
            .unwrap();
        let svc = BackupService::new(inst.db.clone(), inst.layout.clone());
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

    let (s, _) = api
        .call(Method::DELETE, &format!("/api/v1/system/backups/{left}"), None)
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
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
    ] {
        assert!(
            doc["paths"][path][method].is_object(),
            "{method} {path} is not documented"
        );
    }
}
