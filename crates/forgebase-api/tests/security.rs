//! Fail-closed access, injection, limits, CORS and pagination stability.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, StatusCode};
use common::Api;
use forgebase_api::RouterOptions;
use forgebase_data::schema::Collection;
use forgebase_data::{AccessContext, Action, Authorizer, Decision, Predicate, Principal};
use serde_json::{Value as Json, json};

/// Every kind of route, with a body where one is needed.
fn probes() -> Vec<(Method, &'static str, Option<Json>)> {
    vec![
        (Method::GET, "/api/v1/collections", None),
        (Method::POST, "/api/v1/collections", Some(json!({"name": "evil"}))),
        (Method::GET, "/api/v1/collections/secrets", None),
        (Method::DELETE, "/api/v1/collections/secrets", None),
        (Method::GET, "/api/v1/collections/secrets/schema", None),
        (Method::GET, "/api/v1/collections/secrets/history", None),
        (Method::GET, "/api/v1/collections/secrets/records", None),
        (
            Method::POST,
            "/api/v1/collections/secrets/records",
            Some(json!({"v": "x"})),
        ),
        (
            Method::POST,
            "/api/v1/collections/secrets/records/query",
            Some(json!({})),
        ),
        (Method::GET, "/api/v1/collections/secrets/records/count", None),
        (
            Method::POST,
            "/api/v1/collections/secrets/records/explain",
            Some(json!({})),
        ),
        (Method::POST, "/api/v1/collections/secrets/aggregate", Some(json!({}))),
        (Method::GET, "/api/v1/collections/secrets/export", None),
        (
            Method::POST,
            "/api/v1/collections/secrets/import",
            Some(json!([{"v": "x"}])),
        ),
        (Method::POST, "/api/v1/documents/secrets", Some(json!({"v": "x"}))),
        (Method::GET, "/api/v1/indexes", None),
        (
            Method::POST,
            "/api/v1/indexes",
            Some(json!({"collection": "secrets", "keys": [{"path": "v"}]})),
        ),
        (Method::GET, "/api/v1/indexes/recommendations", None),
        (Method::POST, "/api/v1/indexes/analyze", Some(json!({}))),
        (Method::GET, "/api/v1/slow-queries", None),
        (Method::DELETE, "/api/v1/slow-queries", None),
        (
            Method::PUT,
            "/api/v1/slow-queries/threshold",
            Some(json!({"threshold_ms": 1})),
        ),
        (
            Method::POST,
            "/api/v1/sql",
            Some(json!({"sql": "SELECT * FROM secrets"})),
        ),
        (Method::POST, "/api/v1/sql", Some(json!({"sql": "DROP TABLE secrets"}))),
        (
            Method::POST,
            "/api/v1/sql/batch",
            Some(json!({"statements": [{"sql": "SELECT 1"}]})),
        ),
        (Method::POST, "/api/v1/sql/explain", Some(json!({"sql": "SELECT 1"}))),
        (Method::GET, "/api/v1/sql/tables", None),
    ]
}

async fn seed(api: &Api) -> String {
    api.make_collection(json!({"name": "secrets", "fields": [{"name": "v", "type": "text"}]}))
        .await;
    api.make_record("secrets", json!({"v": "TOP-SECRET-VALUE"})).await["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn anonymous_requests_are_denied_everywhere_and_never_see_data() {
    // Seed through an admin handle, then serve the *same* database the way the
    // server does until FB-20: no identity layer, default authorizer.
    let admin_api = Api::admin().await;
    seed(&admin_api).await;
    let layout_tmp = tempfile::tempdir().unwrap();
    let layout = forgebase_core::DataLayout::new(layout_tmp.path().join("data"));
    layout.ensure().unwrap();
    let state = forgebase_api::AppState::new(admin_api.db.clone(), layout)
        .await
        .unwrap();
    let app = forgebase_api::router(state, RouterOptions::default());
    use tower::ServiceExt;

    let mut checked = 0;
    for (method, path, body) in probes() {
        let mut req = axum::http::Request::builder().method(method.clone()).uri(path);
        let body = match body {
            Some(b) => {
                req = req.header("content-type", "application/json");
                Body::from(b.to_string())
            }
            None => Body::empty(),
        };
        let res = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {path}: {text}");
        assert!(!text.contains("TOP-SECRET-VALUE"), "{method} {path} leaked data");
        let j: Json = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(j["error"]["code"], "unauthenticated", "{method} {path}");
        checked += 1;
    }
    assert!(checked >= 25);

    // Nothing was changed by the denied writes.
    let n = admin_api
        .post("/api/v1/sql", json!({"sql": "SELECT count(*) FROM secrets"}))
        .await
        .json();
    assert_eq!(n["rows"][0][0], 1);
    assert_eq!(
        admin_api.get("/api/v1/collections").await.json()["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn non_admins_cannot_probe_which_collections_exist() {
    let admin_api = Api::admin().await;
    seed(&admin_api).await;
    let tmp = tempfile::tempdir().unwrap();
    let layout = forgebase_core::DataLayout::new(tmp.path().join("d"));
    layout.ensure().unwrap();
    use tower::ServiceExt;
    for ctx in [None, Some(AccessContext::user("users", "u1"))] {
        let state = forgebase_api::AppState::new(admin_api.db.clone(), layout.clone())
            .await
            .unwrap();
        let mut app = forgebase_api::router(state, RouterOptions::default());
        if let Some(ctx) = ctx.clone() {
            app = app.layer(axum::middleware::from_fn(
                move |mut req: axum::http::Request<Body>, next: axum::middleware::Next| {
                    let ctx = ctx.clone();
                    async move {
                        req.extensions_mut().insert(ctx);
                        next.run(req).await
                    }
                },
            ));
        }
        let mut statuses = Vec::new();
        for name in ["secrets", "does_not_exist"] {
            let res = app
                .clone()
                .oneshot(
                    axum::http::Request::builder()
                        .uri(format!("/api/v1/collections/{name}/records"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            statuses.push(res.status());
        }
        assert_eq!(
            statuses[0], statuses[1],
            "{ctx:?}: existing and missing collections look alike"
        );
        assert!(matches!(statuses[0], StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN));
    }
    // Administrators still get the honest answer.
    assert_eq!(
        admin_api.get("/api/v1/collections/does_not_exist/records").await.code(),
        "collection_not_found"
    );
}

#[tokio::test]
async fn forged_identity_headers_do_not_elevate() {
    let api = Api::anonymous().await;
    for headers in [
        vec![("authorization", "Bearer admin")],
        vec![("x-forgebase-admin", "true")],
        vec![("x-user-id", "root"), ("x-role", "admin")],
        vec![("cookie", "session=admin")],
    ] {
        let r = api
            .send(Method::GET, "/api/v1/collections", &headers, Body::empty())
            .await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{headers:?}");
    }
}

#[tokio::test]
async fn authenticated_non_admins_are_forbidden_by_the_default_authorizer() {
    let api = Api::with(
        Some(AccessContext::user("users", "u1")),
        Arc::new(forgebase_data::DefaultAuthorizer),
        RouterOptions::default(),
    )
    .await;
    let r = api.get("/api/v1/collections").await;
    assert_eq!((r.status, r.code()), (StatusCode::FORBIDDEN, "forbidden".to_owned()));
    let r = api.post("/api/v1/sql", json!({"sql": "SELECT 1"})).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
}

/// A policy: users see only the rows they own, and may not write.
#[derive(Debug)]
struct OwnerOnly;

impl Authorizer for OwnerOnly {
    fn authorize(&self, ctx: &AccessContext, action: Action, _c: Option<&Collection>) -> Decision {
        match (&ctx.principal, action) {
            (Principal::System | Principal::Admin { .. }, _) => Decision::Allow,
            (Principal::User { id, .. }, Action::List | Action::View) => Decision::Filter(
                Predicate::parse("owner = {:uid}", HashMap::from([("uid".to_owned(), json!(id))])).unwrap(),
            ),
            _ => Decision::Deny,
        }
    }
}

#[tokio::test]
async fn handlers_use_the_context_from_the_request_extension() {
    // Admin seeds; the same database is then served to a user.
    let admin_api = Api::admin().await;
    admin_api
        .make_collection(json!({"name": "notes", "fields": [
            {"name": "owner", "type": "text"}, {"name": "body", "type": "text"}]}))
        .await;
    for (o, b) in [("u1", "mine"), ("u2", "theirs"), ("u1", "mine too")] {
        admin_api.make_record("notes", json!({"owner": o, "body": b})).await;
    }
    let tmp = tempfile::tempdir().unwrap();
    let layout = forgebase_core::DataLayout::new(tmp.path().join("d"));
    layout.ensure().unwrap();
    let state = forgebase_api::AppState::with_authorizer(admin_api.db.clone(), layout, Arc::new(OwnerOnly))
        .await
        .unwrap();
    let app = forgebase_api::router(state, RouterOptions::default()).layer(axum::middleware::from_fn(
        |mut req: axum::http::Request<Body>, next: axum::middleware::Next| async move {
            req.extensions_mut().insert(AccessContext::user("users", "u1"));
            next.run(req).await
        },
    ));
    use tower::ServiceExt;
    let get = |uri: &'static str| {
        let app = app.clone();
        async move {
            let res = app
                .oneshot(axum::http::Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            let status = res.status();
            let b = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
            (status, serde_json::from_slice::<Json>(&b).unwrap())
        }
    };
    let (s, j) = get("/api/v1/collections/notes/records?sort=body").await;
    assert_eq!(s, StatusCode::OK);
    let bodies: Vec<&str> = j["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["body"].as_str().unwrap())
        .collect();
    assert_eq!(
        bodies,
        ["mine", "mine too"],
        "the row predicate is applied inside the query"
    );
    assert_eq!(j["total_items"], 2);
    // Even a filter asking for the others cannot widen it.
    let (_, j) = get("/api/v1/collections/notes/records?filter=owner%3D'u2'").await;
    assert_eq!(j["total_items"], 0);
    // Writes and schema are denied.
    let res = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/v1/collections/notes/records")
                .header("content-type", "application/json")
                .body(Body::from(json!({"owner": "u1", "body": "x"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    let (s, _) = get("/api/v1/collections").await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn filter_injection_is_rejected_and_values_are_always_bound() {
    let api = Api::admin().await;
    seed(&api).await;
    let attacks = [
        "v = 'x' OR 1=1",
        "v = 'x'; DROP TABLE secrets",
        "v = 'x' -- comment",
        "v = 'x' /* c */",
        "v IN (SELECT id FROM _forgebase_collections)",
        "(SELECT 1) = 1",
        "load_extension('x') = 1",
        "v = 'x') OR (1=1",
        "v = \"x\"; DELETE FROM secrets",
        "_data = 1",
        "id; select 1",
        "1; --",
        "v = x",
    ];
    for filter in attacks {
        let uri = format!("/api/v1/collections/secrets/records?filter={}", url_encode(filter));
        let r = api.get(&uri).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{filter}: {}", r.text());
        let code = r.code();
        assert!(
            code.contains("filter") || code.contains("unknown") || code.contains("query"),
            "{filter}: {code}"
        );
        assert!(!r.text().contains("TOP-SECRET"), "{filter}");
    }
    // Same through the JSON form, sort and fields.
    let r = api
        .post(
            "/api/v1/collections/secrets/records/query",
            json!({"filter": "v = 'x' OR 1=1"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = api
        .get("/api/v1/collections/secrets/records?sort=v%3Bdrop%20table%20secrets")
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());

    // A value that looks like injection is just data: bound, matches nothing.
    let r = api
        .get(&format!(
            "/api/v1/collections/secrets/records?filter={}&p.t={}",
            url_encode("v = {:t}"),
            url_encode("' OR '1'='1")
        ))
        .await
        .expect(StatusCode::OK)
        .json();
    assert_eq!(r["total_items"], 0);
    // ... and a legitimate value still matches.
    let r = api
        .get(&format!(
            "/api/v1/collections/secrets/records?filter={}&p.t=TOP-SECRET-VALUE",
            url_encode("v = {:t}")
        ))
        .await
        .json();
    assert_eq!(r["total_items"], 1);

    // The table is intact.
    let n = api
        .post("/api/v1/sql", json!({"sql": "SELECT count(*) FROM secrets"}))
        .await
        .json();
    assert_eq!(n["rows"][0][0], 1);
}

fn url_encode(s: &str) -> String {
    url_form(s)
}

fn url_form(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[tokio::test]
async fn offset_and_cursor_pagination_are_stable() {
    let api = Api::admin().await;
    api.make_collection(json!({"name": "events", "fields": [
        {"name": "score", "type": "number", "integer": true}, {"name": "label", "type": "text"}]}))
        .await;
    let rows: Vec<Json> = (0..53)
        .map(|i| json!({"score": i % 5, "label": format!("e{i}")}))
        .collect();
    let created = api
        .post("/api/v1/collections/events/records", json!(rows))
        .await
        .expect(StatusCode::CREATED)
        .json();
    let originals: HashSet<String> = created["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(originals.len(), 53);

    // Offset pages with heavy ties on the sort key: every record exactly once.
    let mut seen = Vec::new();
    for page in 1..=6 {
        let r = api
            .get(&format!(
                "/api/v1/collections/events/records?sort=score&per_page=10&page={page}"
            ))
            .await
            .expect(StatusCode::OK)
            .json();
        assert_eq!(r["total_items"], 53);
        assert_eq!(r["total_pages"], 6);
        seen.extend(
            r["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|x| x["id"].as_str().unwrap().to_owned()),
        );
    }
    assert_eq!(seen.len(), 53);
    assert_eq!(
        seen.iter().collect::<HashSet<_>>().len(),
        53,
        "no duplicates across pages"
    );
    // Same order on a second run.
    let again = api
        .get("/api/v1/collections/events/records?sort=score&per_page=10&page=3")
        .await
        .json();
    assert_eq!(again["items"][0]["id"], json!(seen[20]));

    // Cursor pagination while the data changes underneath it.
    let first = api
        .get("/api/v1/collections/events/records?sort=score,label&per_page=10")
        .await
        .expect(StatusCode::OK)
        .json();
    let mut got: Vec<String> = first["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["id"].as_str().unwrap().to_owned())
        .collect();
    let mut cursor = first["next_cursor"].as_str().map(str::to_owned);
    assert!(cursor.is_some());
    // Writes between pages: new rows that sort first, and a delete of a row already seen.
    for i in 0..4 {
        api.make_record("events", json!({"score": 0, "label": format!("a-new-{i}")}))
            .await;
    }
    api.delete(&format!("/api/v1/collections/events/records/{}", got[0]))
        .await
        .expect(StatusCode::NO_CONTENT);
    while let Some(c) = cursor {
        let r = api
            .get(&format!(
                "/api/v1/collections/events/records?sort=score,label&per_page=10&cursor={}",
                url_encode(&c)
            ))
            .await
            .expect(StatusCode::OK)
            .json();
        got.extend(
            r["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|x| x["id"].as_str().unwrap().to_owned()),
        );
        cursor = r["next_cursor"].as_str().map(str::to_owned);
    }
    assert_eq!(
        got.iter().collect::<HashSet<_>>().len(),
        got.len(),
        "no duplicate across cursor pages"
    );
    for id in &originals {
        assert!(got.contains(id), "original record {id} was skipped");
    }

    // A cursor is bound to its sort order.
    let r = api
        .get(&format!(
            "/api/v1/collections/events/records?sort=label&cursor={}",
            url_encode(first["next_cursor"].as_str().unwrap())
        ))
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.code(), "invalid_cursor");
    assert_eq!(
        api.get("/api/v1/collections/events/records?cursor=%25%25%25")
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn body_limits_and_cors() {
    let opts = RouterOptions {
        max_body_bytes: 1024,
        max_import_bytes: 2048,
        cors_allowed_origins: vec!["https://app.example.com".into()],
        ..RouterOptions::default()
    };
    let api = Api::with(Some(common::admin()), Arc::new(forgebase_data::DefaultAuthorizer), opts).await;
    api.make_collection(json!({"name": "blobs", "fields": [{"name": "v", "type": "text"}]}))
        .await;
    let big = json!({"v": "x".repeat(4096)});
    let r = api.post("/api/v1/collections/blobs/records", big.clone()).await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large".to_owned())
    );
    // without a content-length (chunked) the limit still applies
    let r = api
        .post_chunked(
            "/api/v1/collections/blobs/records",
            "application/json",
            big.to_string().as_bytes(),
            512,
        )
        .await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE, "{}", r.text());
    // the import limit is separate and also enforced
    let many = format!(
        "[{}]",
        (0..400).map(|_| r#"{"v":"abcdefgh"}"#).collect::<Vec<_>>().join(",")
    );
    let r = api
        .post_chunked(
            "/api/v1/collections/blobs/import",
            "application/json",
            many.as_bytes(),
            256,
        )
        .await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large".to_owned())
    );

    // CORS: preflight from the allowed origin
    let pre = api
        .send(
            Method::OPTIONS,
            "/api/v1/collections",
            &[
                ("origin", "https://app.example.com"),
                ("access-control-request-method", "POST"),
                ("access-control-request-headers", "content-type,authorization"),
            ],
            Body::empty(),
        )
        .await;
    assert!(pre.status.is_success(), "{}", pre.status);
    assert_eq!(pre.headers["access-control-allow-origin"], "https://app.example.com");
    assert!(
        pre.headers["access-control-allow-methods"]
            .to_str()
            .unwrap()
            .contains("PATCH")
    );
    // a simple request carries the header; another origin does not
    let ok = api
        .send(
            Method::GET,
            "/api/v1/collections",
            &[("origin", "https://app.example.com")],
            Body::empty(),
        )
        .await;
    assert_eq!(ok.headers["access-control-allow-origin"], "https://app.example.com");
    let other = api
        .send(
            Method::GET,
            "/api/v1/collections",
            &[("origin", "https://evil.example")],
            Body::empty(),
        )
        .await;
    assert!(other.headers.get("access-control-allow-origin").is_none());

    // default options: no CORS headers at all
    let plain = Api::admin().await;
    let r = plain
        .send(
            Method::GET,
            "/api/v1/collections",
            &[("origin", "https://app.example.com")],
            Body::empty(),
        )
        .await;
    assert!(r.headers.get("access-control-allow-origin").is_none());
}
