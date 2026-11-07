//! SPEC §27 scenarios 2–6 over HTTP, against real SQLite persistence.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::http::StatusCode;
use common::Api;
use serde_json::{Value as Json, json};

fn ids(items: &Json) -> Vec<String> {
    items["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_owned())
        .collect()
}

/// #2 Create a SQL table (and use it) through the SQL editor endpoints.
#[tokio::test]
async fn scenario_2_sql_table() {
    let api = Api::admin().await;
    api.post(
        "/api/v1/sql",
        json!({"sql": "CREATE TABLE invoices (id INTEGER PRIMARY KEY, customer TEXT NOT NULL, total REAL) STRICT"}),
    )
    .await
    .expect(StatusCode::OK);
    let ins = api
        .post(
            "/api/v1/sql/batch",
            json!({"statements": [
                {"sql": "INSERT INTO invoices (customer, total) VALUES (?1, ?2)", "params": ["acme", 12.5]},
                {"sql": "INSERT INTO invoices (customer, total) VALUES (?1, ?2)", "params": ["globex", 7]},
            ]}),
        )
        .await
        .expect(StatusCode::OK)
        .json();
    assert_eq!(ins["items"][0]["rows_affected"], 1);
    assert_eq!(ins["items"].as_array().unwrap().len(), 2);

    let rows = api
        .post(
            "/api/v1/sql",
            json!({"sql": "SELECT customer, total FROM invoices WHERE total > ?1 ORDER BY customer", "params": [10]}),
        )
        .await
        .expect(StatusCode::OK)
        .json();
    assert_eq!(rows["kind"], "read");
    assert_eq!(rows["columns"], json!(["customer", "total"]));
    assert_eq!(rows["rows"], json!([["acme", 12.5]]));

    let tables = api.get("/api/v1/sql/tables").await.expect(StatusCode::OK).json();
    assert!(
        tables["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "invoices")
    );
    let t = api
        .get("/api/v1/sql/tables/invoices")
        .await
        .expect(StatusCode::OK)
        .json();
    assert_eq!(t["columns"].as_array().unwrap().len(), 3);

    // A failing batch is all-or-nothing.
    api.post(
        "/api/v1/sql/batch",
        json!({"statements": [
            {"sql": "INSERT INTO invoices (customer, total) VALUES ('initech', 1)"},
            {"sql": "INSERT INTO invoices (customer, total) VALUES (NULL, 1)"},
        ]}),
    )
    .await
    .expect(StatusCode::CONFLICT);
    let n = api
        .post("/api/v1/sql", json!({"sql": "SELECT count(*) FROM invoices"}))
        .await
        .json();
    assert_eq!(
        n["rows"][0][0], 2,
        "the first statement of the failed batch was rolled back"
    );

    // Editor restrictions come back as 400 with a stable code.
    let r = api
        .post("/api/v1/sql", json!({"sql": "ATTACH DATABASE '/tmp/x' AS x"}))
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    let r = api.post("/api/v1/sql", json!({"sql": "SELECT 1; SELECT 2"})).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.code(), "multiple_statements");
    // System tables are read-only here.
    api.post("/api/v1/sql", json!({"sql": "DELETE FROM _forgebase_collections"}))
        .await
        .expect(StatusCode::FORBIDDEN);
}

/// #3 Create a document collection; merge patch; versions.
#[tokio::test]
async fn scenario_3_document_collection() {
    let api = Api::admin().await;
    let created = api
        .make_collection(json!({"name": "products", "schema_mode": "flexible", "versioned": true,
            "fields": [{"name": "title", "type": "text", "required": true}]}))
        .await;
    assert_eq!(created["schema_mode"], "flexible");
    assert!(!created["id"].as_str().unwrap().is_empty());

    let schema = api
        .get("/api/v1/collections/products/schema")
        .await
        .expect(StatusCode::OK)
        .json();
    assert_eq!(schema["json_schema"]["properties"]["title"]["type"], "string");
    assert_eq!(schema["json_schema"]["required"], json!(["title"]));
    assert_eq!(schema["json_schema"]["additionalProperties"], true);

    let doc = api
        .post(
            "/api/v1/documents/products",
            json!({"title": "Laptop", "specs": {"ram": 16, "cpu": "m3"}, "tags": ["a", "b"]}),
        )
        .await
        .expect(StatusCode::CREATED);
    let location = doc.headers["location"].to_str().unwrap().to_owned();
    assert!(location.starts_with("/api/v1/collections/products/records/"));
    assert_eq!(doc.headers["etag"], "\"1\"");
    let doc = doc.json();
    let id = doc["id"].as_str().unwrap().to_owned();
    assert_eq!(doc["specs"]["ram"], 16, "undeclared keys are kept as a document");

    // RFC 7386 merge patch, as application/merge-patch+json.
    let patched = api
        .send(
            axum::http::Method::PATCH,
            &format!("/api/v1/documents/products/{id}"),
            &[("content-type", "application/merge-patch+json"), ("if-match", "\"1\"")],
            axum::body::Body::from(json!({"specs": {"ram": 32, "cpu": null}, "tags": ["c"]}).to_string()),
        )
        .await
        .expect(StatusCode::OK);
    assert_eq!(patched.headers["etag"], "\"2\"");
    let p = patched.json();
    assert_eq!(p["specs"], json!({"ram": 32}), "null removes a key, objects merge");
    assert_eq!(p["tags"], json!(["c"]), "arrays are replaced");
    assert_eq!(p["title"], "Laptop");

    // Stale If-Match → 409.
    let r = api
        .send(
            axum::http::Method::PATCH,
            &format!("/api/v1/documents/products/{id}"),
            &[("content-type", "application/json"), ("if-match", "\"1\"")],
            axum::body::Body::from(json!({"title": "Stale"}).to_string()),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.code(), "version_conflict");

    // PUT replaces the whole document.
    let put = api
        .call(
            axum::http::Method::PUT,
            &format!("/api/v1/documents/products/{id}"),
            Some(json!({"title": "Replaced"})),
        )
        .await
        .expect(StatusCode::OK)
        .json();
    assert!(put.get("specs").is_none());

    let versions = api
        .get(&format!("/api/v1/documents/products/{id}/versions"))
        .await
        .expect(StatusCode::OK)
        .json();
    let ops: Vec<&str> = versions["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["op"].as_str().unwrap())
        .collect();
    assert_eq!(ops, ["create", "update", "update"]);
    assert_eq!(versions["items"][0]["data"]["specs"]["ram"], 16);

    api.delete(&format!("/api/v1/documents/products/{id}"))
        .await
        .expect(StatusCode::NO_CONTENT);
    api.get(&format!("/api/v1/documents/products/{id}"))
        .await
        .expect(StatusCode::NOT_FOUND);
    let versions = api
        .get(&format!("/api/v1/collections/products/records/{id}/versions"))
        .await
        .json();
    assert_eq!(versions["items"].as_array().unwrap().last().unwrap()["op"], "delete");
}

async fn seed_catalog(api: &Api) -> Json {
    let authors = api
        .make_collection(json!({"name": "authors", "fields": [{"name": "name", "type": "text", "required": true}]}))
        .await;
    api.make_collection(json!({"name": "books", "fields": [
        {"name": "title", "type": "text", "required": true},
        {"name": "category", "type": "text"},
        {"name": "price", "type": "number"},
        {"name": "author", "type": "relation", "collection_id": authors["id"]},
    ]}))
    .await;
    authors
}

/// #4 Insert and query records: filter, sort, projection, expand, pagination,
/// count, aggregate, search.
#[tokio::test]
async fn scenario_4_insert_and_query() {
    let api = Api::admin().await;
    seed_catalog(&api).await;
    let ann = api.make_record("authors", json!({"name": "Ann"})).await;
    let bob = api.make_record("authors", json!({"name": "Bob"})).await;

    // Bulk insert in one transaction.
    let bulk = api
        .post(
            "/api/v1/collections/books/records",
            json!([
                {"title": "Rust", "category": "tech", "price": 30, "author": ann["id"]},
                {"title": "SQLite", "category": "tech", "price": 20, "author": bob["id"]},
                {"title": "Cooking", "category": "food", "price": 15.5, "author": bob["id"]},
                {"title": "Baking", "category": "food", "price": 9, "author": ann["id"]},
            ]),
        )
        .await
        .expect(StatusCode::CREATED)
        .json();
    assert_eq!(bulk["items"].as_array().unwrap().len(), 4);

    // A bulk insert with one bad record stores nothing and names the record.
    let bad = api
        .post(
            "/api/v1/collections/books/records",
            json!([{"title": "ok"}, {"category": "missing title"}]),
        )
        .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    assert_eq!(bad.json()["error"]["details"]["fields"]["1.title"]["code"], "required");
    let all = api.get("/api/v1/collections/books/records?per_page=100").await.json();
    assert_eq!(all["total_items"], 4);

    // filter + sort + projection
    let r = api
        .get("/api/v1/collections/books/records?filter=category%3D'tech'&sort=-price&fields=title,price")
        .await
        .expect(StatusCode::OK)
        .json();
    assert_eq!(r["items"].as_array().unwrap().len(), 2);
    assert_eq!(r["items"][0]["title"], "Rust");
    assert_eq!(r["items"][1]["title"], "SQLite");
    assert!(r["items"][0].get("category").is_none(), "projection drops other fields");
    assert!(r["items"][0].get("price").is_some());

    // typed filter parameters
    let r = api
        .get("/api/v1/collections/books/records?filter=price%20%3C%20%7B%3Amax%7D&p.max=16&sort=price")
        .await
        .expect(StatusCode::OK)
        .json();
    assert_eq!(ids(&r).len(), 2);
    assert_eq!(r["items"][0]["title"], "Baking");
    // ... and in the JSON query form
    let r = api
        .post(
            "/api/v1/collections/books/records/query",
            json!({"filter": "price >= {:min} && category = {:c}", "params": {"min": 20, "c": "tech"}, "sort": "title"}),
        )
        .await
        .expect(StatusCode::OK)
        .json();
    assert_eq!(r["items"].as_array().unwrap().len(), 2);

    // relation filter + expand
    let r = api
        .get("/api/v1/collections/books/records?filter=author.name%3D'Bob'&expand=author&sort=title")
        .await
        .expect(StatusCode::OK)
        .json();
    assert_eq!(r["items"][0]["title"], "Cooking");
    assert_eq!(r["items"][0]["expand"]["author"]["name"], "Bob");

    // offset pagination with totals
    let p1 = api
        .get("/api/v1/collections/books/records?sort=title&per_page=3&page=1")
        .await
        .json();
    let p2 = api
        .get("/api/v1/collections/books/records?sort=title&per_page=3&page=2")
        .await
        .json();
    assert_eq!(
        (p1["total_items"].clone(), p1["total_pages"].clone()),
        (json!(4), json!(2))
    );
    assert_eq!(ids(&p1).len(), 3);
    assert_eq!(ids(&p2).len(), 1);

    // count and aggregate
    let c = api
        .get("/api/v1/collections/books/records/count?filter=category%3D'food'")
        .await
        .expect(StatusCode::OK)
        .json();
    assert_eq!(c["count"], 2);
    let agg = api
        .post(
            "/api/v1/collections/books/aggregate",
            json!({"group_by": ["category"], "aggregates": [
                {"function": "count"}, {"function": "sum", "field": "price", "alias": "total"}]}),
        )
        .await
        .expect(StatusCode::OK)
        .json();
    let mut rows: Vec<(String, f64)> = agg["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["category"].as_str().unwrap().to_owned(), r["total"].as_f64().unwrap()))
        .collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(rows, vec![("food".to_owned(), 24.5), ("tech".to_owned(), 50.0)]);

    // full-text search through a full-text index
    api.post(
        "/api/v1/indexes",
        json!({"name": "books_fts", "collection": "books", "kind": "fulltext", "keys": [{"path": "title"}]}),
    )
    .await
    .expect(StatusCode::CREATED);
    let r = api
        .get("/api/v1/collections/books/records?search=sqlite")
        .await
        .expect(StatusCode::OK)
        .json();
    assert_eq!(r["items"].as_array().unwrap().len(), 1);
    assert_eq!(r["items"][0]["title"], "SQLite");

    // update + delete + persistence across a fresh read
    let id = ids(&all)[0].clone();
    let upd = api
        .patch(&format!("/api/v1/collections/books/records/{id}"), json!({"price": 99}))
        .await
        .expect(StatusCode::OK)
        .json();
    assert_eq!(upd["price"], 99.0);
    let back = api.get(&format!("/api/v1/collections/books/records/{id}")).await.json();
    assert_eq!(back["price"], 99.0);
    api.delete(&format!("/api/v1/collections/books/records/{id}"))
        .await
        .expect(StatusCode::NO_CONTENT);
    api.get(&format!("/api/v1/collections/books/records/{id}"))
        .await
        .expect(StatusCode::NOT_FOUND);

    // Rows really are in SQLite (read through the SQL endpoint).
    let n = api
        .post("/api/v1/sql", json!({"sql": "SELECT count(*) FROM books"}))
        .await
        .json();
    assert_eq!(n["rows"][0][0], 3);
}

/// Validation errors name every bad field; conflicts and missing things map to 409 / 404.
#[tokio::test]
async fn errors_are_enveloped_with_field_details() {
    let api = Api::admin().await;
    api.make_collection(json!({"name": "people", "fields": [
        {"name": "email", "type": "email", "required": true, "unique": true},
        {"name": "age", "type": "number", "integer": true, "min": 0, "max": 150},
    ]}))
    .await;
    let r = api
        .send(
            axum::http::Method::POST,
            "/api/v1/collections/people/records",
            &[("content-type", "application/json"), ("x-request-id", "req-42")],
            axum::body::Body::from(json!({"email": "nope", "age": 200}).to_string()),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let j = r.json();
    assert_eq!(j["error"]["code"], "validation_failed");
    assert_eq!(j["error"]["request_id"], "req-42");
    assert_eq!(j["error"]["details"]["fields"]["email"]["code"], "invalid_email");
    assert!(j["error"]["details"]["fields"]["age"]["message"].is_string());

    api.make_record("people", json!({"email": "a@example.com"})).await;
    let dup = api
        .post("/api/v1/collections/people/records", json!({"email": "a@example.com"}))
        .await;
    // A unique violation is reported as a field error (400) by the data core.
    assert_eq!(dup.status, StatusCode::BAD_REQUEST, "{}", dup.text());
    assert_eq!(dup.json()["error"]["details"]["fields"]["email"]["code"], "unique");

    assert_eq!(
        api.get("/api/v1/collections/nope/records").await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        api.get("/api/v1/collections/nope/records").await.code(),
        "collection_not_found"
    );
    assert_eq!(
        api.get("/api/v1/collections/people/records/zzz").await.code(),
        "record_not_found"
    );
    assert_eq!(api.get("/api/v1/indexes/nope").await.code(), "index_not_found");

    // malformed requests
    let r = api
        .send(
            axum::http::Method::POST,
            "/api/v1/collections/people/records",
            &[("content-type", "application/json")],
            axum::body::Body::from("{not json"),
        )
        .await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::BAD_REQUEST, "invalid_json".to_owned())
    );
    let r = api
        .send(
            axum::http::Method::POST,
            "/api/v1/collections/people/records",
            &[("content-type", "text/plain")],
            axum::body::Body::from("{}"),
        )
        .await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported_media_type".to_owned())
    );
    let r = api
        .post("/api/v1/collections/people/records", json!("just a string"))
        .await;
    assert_eq!(r.code(), "invalid_body");
    let r = api.get("/api/v1/collections/people/records?page=abc").await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::BAD_REQUEST, "invalid_parameter".to_owned())
    );
    let r = api.get("/api/v1/collections/people/records/x?x=1").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = api
        .send(
            axum::http::Method::PATCH,
            "/api/v1/collections/people/records/x",
            &[("content-type", "application/json"), ("if-match", "abc")],
            axum::body::Body::from("{}"),
        )
        .await;
    assert_eq!(r.code(), "invalid_if_match");

    // schema changes: rename and drop a field through PUT, history records it
    let def = api.get("/api/v1/collections/people").await.json();
    let mut changed = def.clone();
    changed["fields"].as_array_mut().unwrap().retain(|f| f["name"] != "age");
    api.call(axum::http::Method::PUT, "/api/v1/collections/people", Some(changed))
        .await
        .expect(StatusCode::OK);
    let hist = api
        .get("/api/v1/collections/people/history")
        .await
        .expect(StatusCode::OK)
        .json();
    assert!(hist["items"].as_array().unwrap().len() >= 2);
    api.delete("/api/v1/collections/people")
        .await
        .expect(StatusCode::NO_CONTENT);
    api.get("/api/v1/collections/people")
        .await
        .expect(StatusCode::NOT_FOUND);
}

/// #5 Create a composite index; manage it. #6 Validate index usage with plans.
#[tokio::test]
async fn scenario_5_and_6_composite_index_and_plans() {
    let api = Api::admin().await;
    api.make_collection(json!({"name": "items", "fields": [
        {"name": "category", "type": "text"}, {"name": "price", "type": "number"}, {"name": "note", "type": "text"}]}))
        .await;
    let rows: Vec<Json> = (0..300)
        .map(|i| json!({"category": format!("c{}", i % 10), "price": i, "note": format!("n{i}")}))
        .collect();
    api.post("/api/v1/collections/items/records", json!(rows[..150]))
        .await
        .expect(StatusCode::CREATED);
    api.post("/api/v1/collections/items/records", json!(rows[150..]))
        .await
        .expect(StatusCode::CREATED);

    let plan_query = json!({"filter": "category = 'c3' && price > 100", "sort": "-price"});
    let before = api
        .post("/api/v1/collections/items/records/explain", plan_query.clone())
        .await
        .expect(StatusCode::OK)
        .json();
    assert!(before["indexes_used"].as_array().unwrap().is_empty(), "{before}");
    assert!(
        !before["full_scans"].as_array().unwrap().is_empty(),
        "no index yet: full scan"
    );

    let created = api
        .post(
            "/api/v1/indexes",
            json!({"name": "idx_items_cat_price", "collection": "items",
                   "keys": [{"path": "category"}, {"path": "price", "desc": true}]}),
        )
        .await
        .expect(StatusCode::CREATED)
        .json();
    assert_eq!(created["name"], "idx_items_cat_price");
    assert_eq!(created["origin"], "manual");
    assert_eq!(created["columns"].as_array().unwrap().len(), 2);

    api.post("/api/v1/indexes/analyze", json!({"collection": "items"}))
        .await
        .expect(StatusCode::NO_CONTENT);

    let after = api
        .post("/api/v1/collections/items/records/explain", plan_query.clone())
        .await
        .expect(StatusCode::OK)
        .json();
    assert_eq!(after["indexes_used"], json!(["idx_items_cat_price"]), "{after}");
    assert!(after["full_scans"].as_array().unwrap().is_empty(), "{after}");
    // The same statement the plan describes returns the right rows.
    let r = api
        .post("/api/v1/collections/items/records/query", plan_query)
        .await
        .expect(StatusCode::OK)
        .json();
    assert_eq!(r["total_items"], 20, "c3 has prices 103,113,...,293");
    assert_eq!(r["items"][0]["price"], 293.0);

    // raw SQL plan
    let sql_plan = api
        .post(
            "/api/v1/sql/explain",
            json!({"sql": "SELECT * FROM items WHERE category = ?1 AND price > ?2", "params": ["c3", 100]}),
        )
        .await
        .expect(StatusCode::OK)
        .json();
    assert_eq!(sql_plan["indexes_used"], json!(["idx_items_cat_price"]));

    // listing, inspection, stats
    let list = api
        .get("/api/v1/indexes?collection=items")
        .await
        .expect(StatusCode::OK)
        .json();
    assert!(
        list["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["name"] == "idx_items_cat_price")
    );
    let one = api
        .get("/api/v1/indexes/idx_items_cat_price")
        .await
        .expect(StatusCode::OK)
        .json();
    assert_eq!(one["definition"]["keys"][1]["desc"], true);
    let stats = api
        .get("/api/v1/indexes/idx_items_cat_price/stats")
        .await
        .expect(StatusCode::OK)
        .json();
    assert!(
        stats["rows_estimate"].as_i64().unwrap() >= 300 || stats["stat"].is_string(),
        "{stats}"
    );
    assert!(stats["size_bytes"].is_number() || stats["size_bytes"].is_null());

    // rebuild keeps the index working
    let rebuilt = api
        .post("/api/v1/indexes/idx_items_cat_price/rebuild", json!({}))
        .await
        .expect(StatusCode::OK)
        .json();
    assert_eq!(rebuilt["name"], "idx_items_cat_price");

    // slow-query log and recommendations
    api.call(
        axum::http::Method::PUT,
        "/api/v1/slow-queries/threshold",
        Some(json!({"threshold_ms": 0})),
    )
    .await
    .expect(StatusCode::OK);
    api.get("/api/v1/collections/items/records?filter=note%3D'n7'")
        .await
        .expect(StatusCode::OK);
    let slow = api.get("/api/v1/slow-queries").await.expect(StatusCode::OK).json();
    assert_eq!(slow["threshold_ms"], 0);
    let entries = slow["items"].as_array().unwrap();
    assert!(!entries.is_empty());
    assert!(entries.iter().any(|e| e["label"]["collection"] == "items"));
    assert!(!slow.to_string().contains("n7"), "parameter values are never logged");
    let rec = api
        .get("/api/v1/indexes/recommendations")
        .await
        .expect(StatusCode::OK)
        .json();
    assert!(
        rec["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["definition"]["keys"][0]["path"] == "note"),
        "{rec}"
    );
    api.delete("/api/v1/slow-queries").await.expect(StatusCode::NO_CONTENT);
    assert!(
        api.get("/api/v1/slow-queries").await.json()["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    // drop → plan goes back to a scan; duplicates for a unique index are a 409
    api.delete("/api/v1/indexes/idx_items_cat_price")
        .await
        .expect(StatusCode::NO_CONTENT);
    api.get("/api/v1/indexes/idx_items_cat_price")
        .await
        .expect(StatusCode::NOT_FOUND);
    let r = api
        .post(
            "/api/v1/indexes",
            json!({"name": "uq_cat", "collection": "items", "unique": true, "keys": [{"path": "category"}]}),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
}
