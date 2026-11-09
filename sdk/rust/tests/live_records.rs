//! Live collection and record tests.

mod common;

use common::Server;
use forgebase_sdk::{ForgeBase, ListOptions, TransferFormat, WriteOptions};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

async fn posts(s: &Server) -> ForgeBase {
    let admin = s.admin().await;
    let def = json!({
        "name": "posts", "type": "base",
        "fields": [
            {"name": "title", "type": "text", "required": true},
            {"name": "views", "type": "number"},
            {"name": "status", "type": "text"}
        ]
    });
    admin.collections().create(&def).await.expect("create collection");
    admin
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct Post {
    id: String,
    title: String,
    views: f64,
    status: String,
}

#[tokio::test(flavor = "multi_thread")]
async fn collection_definitions() {
    let Some(s) = Server::start() else { return };
    let admin = posts(&s).await;
    let all = admin.collections().list().await.expect("list");
    assert!(all.iter().any(|c| c.name == "posts"));
    let one = admin.collections().get("posts").await.expect("get");
    assert!(one.fields.iter().any(|f| f.name == "title"), "{one:?}");
    admin.collections().delete("posts").await.expect("delete");
    assert!(admin.collections().get("posts").await.expect_err("gone").is_not_found());
}

#[tokio::test(flavor = "multi_thread")]
async fn crud_with_optimistic_concurrency() {
    let Some(s) = Server::start() else { return };
    let admin = posts(&s).await;
    let posts = admin.collection("posts");

    let p = posts
        .create(&json!({"title": "hello", "views": 1, "status": "draft"}))
        .await
        .expect("create");
    let id = p["id"].as_str().expect("id").to_owned();
    assert_eq!(posts.get(&id).await.expect("get")["title"], "hello");

    let u = posts
        .update(&id, &json!({"views": 2}), WriteOptions::default())
        .await
        .expect("patch");
    assert_eq!(u["views"].as_f64(), Some(2.0));
    let stale = posts
        .update(&id, &json!({"views": 3}), WriteOptions { version: Some(999) })
        .await
        .expect_err("stale version");
    assert!(stale.is_conflict(), "{stale}");
    let version = u["_version"].as_u64().expect("_version");
    let ok = posts
        .update(&id, &json!({"views": 3}), WriteOptions { version: Some(version) })
        .await
        .expect("current version");
    assert_eq!(ok["views"].as_f64(), Some(3.0));

    let r = posts
        .replace(
            &id,
            &json!({"title": "replaced", "views": 0, "status": "draft"}),
            WriteOptions::default(),
        )
        .await
        .expect("put");
    assert_eq!(r["title"], "replaced");
    posts.delete(&id).await.expect("delete");
    assert!(posts.get(&id).await.expect_err("deleted").is_not_found());
}

#[tokio::test(flavor = "multi_thread")]
async fn validation_errors_carry_fields_and_request_id() {
    let Some(s) = Server::start() else { return };
    let admin = posts(&s).await;
    let err = admin
        .collection("posts")
        .create(&json!({"views": 1}))
        .await
        .expect_err("title is required");
    assert!(err.is_validation(), "{err}");
    let fields = err.api().expect("api error").field_errors();
    assert!(fields.iter().any(|(f, _, _)| f == "title"), "{fields:?}");
    assert!(err.request_id().is_some());
    assert_eq!(err.code(), "validation_failed");
}

#[tokio::test(flavor = "multi_thread")]
async fn filters_sorting_typed_params_and_projection() {
    let Some(s) = Server::start() else { return };
    let admin = posts(&s).await;
    let posts = admin.collection("posts").typed::<Post>();
    for i in 1..=7 {
        let status = if i % 2 == 1 { "live" } else { "draft" };
        admin
            .collection("posts")
            .create(&json!({"title": format!("item {i}"), "views": i * 10, "status": status}))
            .await
            .expect("create");
    }
    let live = posts
        .list(
            &ListOptions::new()
                .filter(r#"status = "live""#)
                .sort("-views")
                .per_page(50),
        )
        .await
        .expect("filter");
    assert_eq!(
        live.items.iter().map(|p| p.views as i64).collect::<Vec<_>>(),
        [70, 50, 30, 10]
    );
    assert_eq!(live.total_items, Some(4));

    let typed = posts
        .list(
            &ListOptions::new()
                .filter("views > {:min}")
                .params(json!({"min": 40}))
                .sort("views"),
        )
        .await
        .expect("typed params");
    assert_eq!(
        typed.items.iter().map(|p| p.views as i64).collect::<Vec<_>>(),
        [50, 60, 70]
    );

    assert_eq!(posts.count(Some(r#"status = "draft""#)).await.expect("count"), 3);
    assert_eq!(posts.count(None).await.expect("count all"), 7);
    assert_eq!(
        posts
            .first(r#"title = "item 3""#)
            .await
            .expect("first")
            .map(|p| p.views as i64),
        Some(30)
    );
    assert!(posts.first(r#"title = "nothing""#).await.expect("first").is_none());

    let projected = admin
        .collection("posts")
        .list(&ListOptions::new().field("title").per_page(1))
        .await
        .expect("projection");
    assert!(projected.items[0].get("title").is_some());
    assert!(projected.items[0].get("views").is_none(), "{}", projected.items[0]);

    let agg = admin
        .collection("posts")
        .aggregate(json!({"aggregates": [{"function": "count", "alias": "n"}], "group_by": ["status"], "params": {}}))
        .await
        .expect("aggregate");
    assert_eq!(agg.len(), 2, "{agg:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn pagination_by_page_and_by_cursor() {
    let Some(s) = Server::start() else { return };
    let admin = posts(&s).await;
    let posts = admin.collection("posts");
    for i in 1..=7 {
        posts
            .create(&json!({"title": format!("item {i}"), "views": i * 10, "status": "x"}))
            .await
            .expect("create");
    }
    // Page numbers (total known).
    let page = posts
        .list(&ListOptions::new().sort("views").per_page(3).page(2))
        .await
        .expect("page 2");
    assert_eq!((page.page, page.total_pages, page.items.len()), (Some(2), Some(3), 3));

    let views = |v: &[Value]| {
        v.iter()
            .map(|r| r["views"].as_f64().expect("views") as i64)
            .collect::<Vec<_>>()
    };
    // The iterator follows pages (or cursors) to the end, lazily.
    let all = posts
        .all(ListOptions::new().sort("views").per_page(3))
        .await
        .expect("all");
    assert_eq!(views(&all), [10, 20, 30, 40, 50, 60, 70]);
    let skip_total = posts
        .all(ListOptions::new().sort("views").per_page(2).skip_total(true))
        .await
        .expect("all, no total");
    assert_eq!(views(&skip_total), [10, 20, 30, 40, 50, 60, 70]);

    let mut it = posts.iter(ListOptions::new().sort("-views").per_page(4));
    let mut seen = Vec::new();
    while let Some(r) = it.next().await.expect("next") {
        seen.push(r["views"].as_f64().expect("views") as i64);
    }
    assert_eq!(seen, [70, 60, 50, 40, 30, 20, 10]);
}

#[tokio::test(flavor = "multi_thread")]
async fn export_and_import_round_trip() {
    let Some(s) = Server::start() else { return };
    let admin = posts(&s).await;
    let posts = admin.collection("posts");
    posts
        .create(&json!({"title": "a", "views": 1, "status": "x"}))
        .await
        .expect("create");
    posts
        .create(&json!({"title": "b", "views": 2, "status": "x"}))
        .await
        .expect("create");
    let nd = posts.export(TransferFormat::Ndjson, None).await.expect("export");
    assert_eq!(nd.lines().filter(|l| !l.trim().is_empty()).count(), 2, "{nd}");

    let def = json!({"name": "copy", "type": "base", "fields": [{"name": "title", "type": "text"}, {"name": "views", "type": "number"}, {"name": "status", "type": "text"}]});
    admin.collections().create(&def).await.expect("create copy");
    let res = admin
        .collection("copy")
        .import(TransferFormat::Ndjson, nd)
        .await
        .expect("import");
    assert_eq!(res.imported, 2, "{res:?}");
    assert_eq!(admin.collection("copy").count(None).await.expect("count"), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn anonymous_callers_are_denied() {
    let Some(s) = Server::start() else { return };
    let _admin = posts(&s).await;
    let anon = ForgeBase::new(&s.base).expect("client");
    let err = anon
        .collection("posts")
        .list(&ListOptions::new())
        .await
        .expect_err("anonymous");
    assert!(err.is_unauthenticated(), "{err}");
}
