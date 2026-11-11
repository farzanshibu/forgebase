//! `/api/v1/vector` over HTTP: the real router and middleware, a real SQLite
//! database, identity injected the way the auth layer does. Covers the REST
//! flow (SPEC §27 #12, #13, #23 through the API), OpenAPI coverage, error
//! envelopes and authorisation (anonymous denied).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::middleware::{self, Next};
use common::{Rng, test_db};
use forgebase_api::{AppState, RouterOptions};
use forgebase_core::DataLayout;
use forgebase_data::{AccessContext, DefaultAuthorizer};
use forgebase_vector::{VectorConfig, VectorService};
use serde_json::{Value as Json, json};
use tower::ServiceExt;

struct Api {
    app: Router,
    _dir: tempfile::TempDir,
    _test: forgebase_db::testing::TestDb,
}

impl Api {
    async fn new(ctx: Option<AccessContext>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let test = test_db(dir.path()).await;
        let db = test.database().clone();
        let layout = DataLayout::new(dir.path().join("data"));
        let state = AppState::with_authorizer(db, layout.clone(), Arc::new(DefaultAuthorizer))
            .await
            .unwrap();
        let svc = VectorService::open(
            state.data().clone(),
            layout.indexes_dir(),
            VectorConfig {
                exact_threshold: 20,
                ..VectorConfig::default()
            },
        )
        .await
        .unwrap();
        let mut app = forgebase_api::router(
            state,
            RouterOptions {
                extensions: vec![forgebase_vector::extension(svc)],
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
            _dir: dir,
            _test: test,
        }
    }

    async fn admin() -> Self {
        Self::new(Some(AccessContext::admin("root"))).await
    }

    async fn call(&self, method: Method, uri: &str, body: Option<String>) -> (StatusCode, Json) {
        let mut req = Request::builder().method(method).uri(uri);
        let body = match body {
            Some(b) => {
                req = req.header("content-type", "application/json");
                Body::from(b)
            }
            None => Body::empty(),
        };
        let res = self.app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), 64 << 20).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(Json::Null))
    }

    async fn get(&self, uri: &str) -> (StatusCode, Json) {
        self.call(Method::GET, uri, None).await
    }

    async fn post(&self, uri: &str, body: Json) -> (StatusCode, Json) {
        self.call(Method::POST, uri, Some(body.to_string())).await
    }

    async fn delete(&self, uri: &str) -> (StatusCode, Json) {
        self.call(Method::DELETE, uri, None).await
    }
}

fn code(j: &Json) -> &str {
    j["error"]["code"].as_str().unwrap_or_default()
}

/// Every operation under `/api/v1/vector`, as (method, path template).
const OPERATIONS: &[(&str, &str)] = &[
    ("get", "/api/v1/vector/stats"),
    ("get", "/api/v1/vector/collections"),
    ("post", "/api/v1/vector/collections"),
    ("get", "/api/v1/vector/collections/{name}"),
    ("delete", "/api/v1/vector/collections/{name}"),
    ("get", "/api/v1/vector/collections/{name}/fields"),
    ("post", "/api/v1/vector/collections/{name}/fields"),
    ("delete", "/api/v1/vector/collections/{name}/fields/{field}"),
    ("post", "/api/v1/vector/collections/{name}/upsert"),
    ("post", "/api/v1/vector/collections/{name}/delete"),
    ("get", "/api/v1/vector/collections/{name}/items/{id}"),
    ("delete", "/api/v1/vector/collections/{name}/items/{id}"),
    ("post", "/api/v1/vector/collections/{name}/search"),
    ("post", "/api/v1/vector/collections/{name}/hybrid"),
    ("post", "/api/v1/vector/collections/{name}/rebuild"),
    ("get", "/api/v1/vector/collections/{name}/stats"),
    ("post", "/api/v1/vector/collections/{name}/recall"),
];

fn concrete(path: &str) -> String {
    path.replace("{name}", "docs")
        .replace("{field}", "emb")
        .replace("{id}", "a")
}

#[tokio::test]
async fn openapi_documents_every_vector_route() {
    let api = Api::admin().await;
    let (s, doc) = api.get("/api/openapi.json").await;
    assert_eq!(s, StatusCode::OK);
    let mut documented = Vec::new();
    for (path, item) in doc["paths"].as_object().unwrap() {
        if path.starts_with("/api/v1/vector") {
            for method in item.as_object().unwrap().keys() {
                documented.push((method.clone(), path.clone()));
            }
        }
    }
    for (m, p) in OPERATIONS {
        assert!(
            documented.contains(&((*m).to_owned(), (*p).to_owned())),
            "missing in OpenAPI: {m} {p}"
        );
    }
    assert_eq!(documented.len(), OPERATIONS.len(), "{documented:?}");
    assert!(doc["components"]["schemas"]["SearchRequest"].is_object());
    assert!(doc["components"]["schemas"]["HybridRequest"].is_object());
}

#[tokio::test]
async fn anonymous_callers_get_401_on_every_route_before_the_body_is_read() {
    let api = Api::new(None).await;
    for (m, p) in OPERATIONS {
        let method: Method = m.to_uppercase().parse().unwrap();
        let body = (method == Method::POST).then(|| "not json".to_owned());
        let (s, j) = api.call(method, &concrete(p), body).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{m} {p}: {j}");
        assert_eq!(code(&j), "unauthenticated");
        assert!(j["error"]["request_id"].is_string());
    }
    let user = Api::new(Some(AccessContext::user("users", "u1"))).await;
    let (s, j) = user
        .post("/api/v1/vector/collections/docs/search", json!({"vector": [1.0]}))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{j}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rest_flow_create_upsert_retrieve_search_hybrid_rebuild_stats() {
    let api = Api::admin().await;
    let (s, j) = api
        .post(
            "/api/v1/vector/collections",
            json!({"name": "docs", "description": "test", "fields": [
                {"name": "emb", "dimension": 3, "metric": "cosine"}
            ]}),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{j}");
    assert_eq!(j["fields"][0]["connectivity"], 16);
    assert_eq!(j["fields"][0]["quantization"], "f32");
    let (s, j) = api
        .post("/api/v1/vector/collections", json!({"name": "docs", "fields": []}))
        .await;
    assert_eq!((s, code(&j)), (StatusCode::CONFLICT, "vector_collection_exists"));
    let (s, j) = api
        .post(
            "/api/v1/vector/collections",
            json!({"name": "bad", "fields": [{"name": "e", "dimension": 3, "metric": "manhattan"}]}),
        )
        .await;
    assert_eq!((s, code(&j)), (StatusCode::BAD_REQUEST, "invalid_body"), "{j}");

    // SPEC #12 through the API.
    let (s, j) = api
        .post(
            "/api/v1/vector/collections/docs/upsert",
            json!({"items": [
                {"id": "a", "vectors": {"emb": [1.0, 0.0, 0.0]}, "metadata": {"lang": "en"}, "text": "rust vector database"},
                {"id": "b", "vectors": {"emb": [0.0, 1.0, 0.0]}, "metadata": {"lang": "de"}, "text": "eine Datenbank"},
                {"vectors": {"emb": [0.7, 0.7, 0.0]}, "metadata": {"lang": "en"}, "text": "hybrid search database"}
            ]}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(j["created"], 3);
    let generated = j["ids"][2].as_str().unwrap().to_owned();
    let (s, j) = api.get("/api/v1/vector/collections/docs/items/a").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(j["vectors"]["emb"], json!([1.0, 0.0, 0.0]));
    assert_eq!(j["metadata"]["lang"], "en");

    // Dimension mismatch.
    let (s, j) = api
        .post(
            "/api/v1/vector/collections/docs/upsert",
            json!({"items": [{"id": "c", "vectors": {"emb": [1.0, 2.0]}}]}),
        )
        .await;
    assert_eq!(
        (s, code(&j)),
        (StatusCode::BAD_REQUEST, "vector_dimension_mismatch"),
        "{j}"
    );
    let (s, j) = api
        .post("/api/v1/vector/collections/docs/search", json!({"vector": [1.0, 0.0]}))
        .await;
    assert_eq!((s, code(&j)), (StatusCode::BAD_REQUEST, "vector_dimension_mismatch"));

    // SPEC #13 through the API, with a filter.
    let (s, j) = api
        .post(
            "/api/v1/vector/collections/docs/search",
            json!({"vector": [1.0, 0.1, 0.0], "k": 2, "include_vectors": true}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(j["hits"][0]["id"], "a");
    assert_eq!(j["hits"][1]["id"], generated.as_str());
    assert_eq!(j["hits"][0]["vector"], json!([1.0, 0.0, 0.0]));
    assert_eq!(j["strategy"], "exact");
    let (_, j) = api
        .post(
            "/api/v1/vector/collections/docs/search",
            json!({"vector": [0.0, 1.0, 0.0], "k": 3, "filter": "lang = 'en'"}),
        )
        .await;
    let ids: Vec<&str> = j["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["id"].as_str().unwrap())
        .collect();
    assert!(!ids.contains(&"b") && ids.len() == 2, "{j}");
    let (s, j) = api
        .post(
            "/api/v1/vector/collections/docs/search",
            json!({"vector": [0.0, 1.0, 0.0], "filter": "lang = "}),
        )
        .await;
    assert_eq!((s, code(&j)), (StatusCode::BAD_REQUEST, "filter_syntax_error"), "{j}");

    // Hybrid.
    let (s, j) = api
        .post(
            "/api/v1/vector/collections/docs/hybrid",
            json!({"vector": [1.0, 0.0, 0.0], "text": "database", "k": 3, "rrf_k": 10,
                   "weights": {"vector": 1.0, "text": 2.0}, "include_text": true}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(j["text_candidates"], 2, "Datenbank is another word");
    assert!(j["hits"][0]["score"].as_f64().unwrap() > 0.0);
    assert!(j["hits"].as_array().unwrap().iter().all(|h| h["text"].is_string()));

    // Rebuild in the background, then stats and a recall check.
    let (s, j) = api
        .post("/api/v1/vector/collections/docs/rebuild", json!({"field": "emb"}))
        .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{j}");
    assert_eq!(j["indexes"][0]["field"], "emb");
    let mut ready = false;
    for _ in 0..500 {
        let (_, st) = api.get("/api/v1/vector/collections/docs/stats").await;
        if st["indexes"][0]["state"] == "ready" && st["indexes"][0]["last_rebuild"]["reason"] == "manual" {
            assert_eq!(st["items"], 3);
            assert_eq!(st["indexes"][0]["indexed_vectors"], 3);
            // USearch, or pgvector's HNSW in PostgreSQL (FB-162).
            assert!(
                matches!(st["indexes"][0]["engine"].as_str(), Some("usearch" | "pgvector")),
                "{st}"
            );
            ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(ready, "rebuild did not finish");
    let (s, j) = api
        .post("/api/v1/vector/collections/docs/recall", json!({"samples": 3, "k": 2}))
        .await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(j["recall"], 1.0);
    let (_, j) = api.get("/api/v1/vector/stats").await;
    assert_eq!(
        (j["collections"].as_u64(), j["indexed_vectors"].as_u64()),
        (Some(1), Some(3))
    );

    // Fields, deletes, 404s.
    let (s, _) = api
        .post(
            "/api/v1/vector/collections/docs/fields",
            json!({"name": "img", "dimension": 2, "metric": "l2", "quantization": "f16"}),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    let (_, j) = api.get("/api/v1/vector/collections/docs/fields").await;
    assert_eq!(j["items"].as_array().unwrap().len(), 2);
    let (s, _) = api.delete("/api/v1/vector/collections/docs/fields/img").await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, j) = api
        .post("/api/v1/vector/collections/docs/delete", json!({"ids": ["a", "zzz"]}))
        .await;
    assert_eq!((s, j["deleted"].as_u64()), (StatusCode::OK, Some(1)));
    let (s, j) = api.get("/api/v1/vector/collections/docs/items/a").await;
    assert_eq!((s, code(&j)), (StatusCode::NOT_FOUND, "vector_item_not_found"));
    let (s, _) = api.delete("/api/v1/vector/collections/docs/items/b").await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, j) = api.delete("/api/v1/vector/collections/docs/items/b").await;
    assert_eq!((s, code(&j)), (StatusCode::NOT_FOUND, "vector_item_not_found"));
    let (s, j) = api.get("/api/v1/vector/collections/nope").await;
    assert_eq!((s, code(&j)), (StatusCode::NOT_FOUND, "vector_collection_not_found"));
    let (s, _) = api.delete("/api/v1/vector/collections/docs").await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_, j) = api.get("/api/v1/vector/collections").await;
    assert_eq!(j["items"], json!([]));
}

#[tokio::test]
async fn large_batches_fit_the_body_limit() {
    let api = Api::admin().await;
    let (s, _) = api
        .post(
            "/api/v1/vector/collections",
            json!({"name": "big", "fields": [{"name": "emb", "dimension": 384, "metric": "dot"}]}),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    let mut rng = Rng::new(1);
    let items: Vec<Json> = (0..1000)
        .map(|i| json!({"id": format!("i{i}"), "vectors": {"emb": rng.vector(384)}}))
        .collect();
    let body = json!({ "items": items }).to_string();
    assert!(body.len() > 4 * 1024 * 1024, "larger than the core API's limit");
    let (s, j) = api
        .call(Method::POST, "/api/v1/vector/collections/big/upsert", Some(body))
        .await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(j["created"], 1000);
    let too_many: Vec<Json> = (0..1001).map(|i| json!({"id": format!("x{i}")})).collect();
    let (s, j) = api
        .post("/api/v1/vector/collections/big/upsert", json!({ "items": too_many }))
        .await;
    assert_eq!((s, code(&j)), (StatusCode::BAD_REQUEST, "invalid_batch"));
}
