//! Harness: the graph router over a real SQLite database, driven in-process.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::middleware::{self, Next};
use forgebase_core::DataLayout;
use forgebase_data::{AccessContext, Action, Authorizer, DataService, DefaultAuthorizer};
use forgebase_db::Database;
use forgebase_db::testing::{TestDb, test_backend};
use forgebase_graph::GraphService;
use serde_json::{Value as Json, json};
use tower::ServiceExt;

pub struct Env {
    pub db: Database,
    pub data: DataService,
    pub graph: GraphService,
    _test: TestDb,
}

pub async fn env_with(authorizer: Arc<dyn Authorizer>) -> Env {
    let test = test_backend().await;
    let db = test.database().clone();
    let data = DataService::new(Arc::new(db.clone()), authorizer).await.unwrap();
    let graph = GraphService::new(data.clone());
    Env {
        db,
        data,
        graph,
        _test: test,
    }
}

pub async fn env() -> Env {
    env_with(Arc::new(DefaultAuthorizer)).await
}

pub fn admin_in(tenant: &str) -> AccessContext {
    let mut c = AccessContext::admin("root");
    c.tenant = Some(tenant.to_owned());
    c
}

/// An HTTP client for one identity (`None` = no identity layer: anonymous).
pub struct Client(Router);

impl Env {
    pub fn client(&self, ctx: Option<AccessContext>) -> Client {
        let mut app = Router::new().nest_service("/api/v1/graph", forgebase_graph::router(self.graph.clone()));
        if let Some(ctx) = ctx {
            app = app.layer(middleware::from_fn(move |mut req: Request<Body>, next: Next| {
                let ctx = ctx.clone();
                async move {
                    req.extensions_mut().insert(ctx);
                    next.run(req).await
                }
            }));
        }
        Client(app)
    }

    pub fn admin(&self) -> Client {
        self.client(Some(AccessContext::admin("root")))
    }
}

impl Client {
    pub async fn call(&self, method: Method, path: &str, body: Option<Json>) -> (StatusCode, Json) {
        self.call_with(method, path, body, &[]).await
    }

    pub async fn call_with(
        &self,
        method: Method,
        path: &str,
        body: Option<Json>,
        headers: &[(&str, &str)],
    ) -> (StatusCode, Json) {
        let mut req = Request::builder().method(method).uri(format!("/api/v1/graph{path}"));
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let req = match body {
            Some(b) => req
                .header("content-type", "application/json")
                .body(Body::from(b.to_string())),
            None => req.body(Body::empty()),
        }
        .unwrap();
        let res = self.0.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), 64 << 20).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(Json::Null))
    }

    pub async fn get(&self, path: &str) -> (StatusCode, Json) {
        self.call(Method::GET, path, None).await
    }

    pub async fn post(&self, path: &str, body: Json) -> (StatusCode, Json) {
        self.call(Method::POST, path, Some(body)).await
    }

    pub async fn delete(&self, path: &str) -> (StatusCode, Json) {
        self.call(Method::DELETE, path, None).await
    }

    pub async fn node(&self, id: &str, label: &str) {
        let (s, b) = self.post("/nodes", json!({"id": id, "label": label})).await;
        assert_eq!(s, StatusCode::CREATED, "{b}");
    }

    pub async fn edge(&self, id: &str, ty: &str, source: &str, target: &str) {
        self.edge_dir(id, ty, source, target, true).await;
    }

    pub async fn edge_dir(&self, id: &str, ty: &str, source: &str, target: &str, directed: bool) {
        let (s, b) = self
            .post(
                "/edges",
                json!({"id": id, "type": ty, "source": source, "target": target, "directed": directed}),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED, "{b}");
    }
}

pub fn ids(v: &Json) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap().to_owned())
        .collect()
}

/// A test authorizer: user `reader` may read the graph, user `writer` may read
/// and write it, user `rowfilter` gets a filter that matches every row (a
/// label rule since FB-139), everybody else is denied. Administrators may do everything.
#[derive(Debug)]
pub struct TestPolicy;

impl Authorizer for TestPolicy {
    fn authorize(
        &self,
        ctx: &AccessContext,
        action: Action,
        _c: Option<&forgebase_data::schema::Collection>,
    ) -> forgebase_data::Decision {
        use forgebase_data::Decision;
        if ctx.is_privileged() {
            return Decision::Allow;
        }
        let forgebase_data::Principal::User { id, .. } = &ctx.principal else {
            return Decision::Deny;
        };
        match (id.as_str(), action) {
            ("reader", Action::GraphRead) | ("writer", Action::GraphRead | Action::GraphWrite) => Decision::Allow,
            ("rowfilter", Action::GraphRead) => Decision::Filter(forgebase_data::Predicate {
                filter: forgebase_data::query::Expr::Const(true),
                params: Default::default(),
            }),
            _ => Decision::Deny,
        }
    }
}

/// Seeds `nodes` nodes and `edges` edges in one transaction (the REST path
/// would commit once per row): a ring for connectivity, then pseudo-random
/// edges of three types; every seventh edge is undirected.
pub async fn seed_random(env: &Env, nodes: usize, edges: usize) {
    use forgebase_db::{Backend, Statement, TxMode};
    let (nodes_n, edges_n) = (nodes, edges);
    let mut tx = env.db.begin(TxMode::Normal).await.unwrap();
    let now = "2026-01-01T00:00:00.000Z";
    for i in 0..nodes_n {
        tx.execute(&Statement::new(
            "INSERT INTO _forgebase_graph_nodes (tenant, id, label, properties, created_at, updated_at) VALUES ('', ?1, 'N', '{}', ?2, ?2)",
            vec![format!("n{i}").into(), now.into()],
        ))
        .await
        .unwrap();
    }
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut next = move || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        usize::try_from(state >> 33).unwrap()
    };
    for i in 0..edges_n {
        // A ring guarantees connectivity; the rest is random.
        let (s, t) = if i < nodes_n {
            (i, (i + 1) % nodes_n)
        } else {
            (next() % nodes_n, next() % nodes_n)
        };
        let ty = ["FOLLOWS", "LIKES", "KNOWS"][i % 3];
        tx.execute(&Statement::new(
            "INSERT INTO _forgebase_graph_edges (tenant, id, type, source, target, directed, properties, created_at, updated_at) \
             VALUES ('', ?1, ?2, ?3, ?4, ?5, '{}', ?6, ?6)",
            vec![format!("e{i}").into(), ty.into(), format!("n{s}").into(), format!("n{t}").into(), i64::from(i % 7 != 0).into(), now.into()],
        ))
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
}
