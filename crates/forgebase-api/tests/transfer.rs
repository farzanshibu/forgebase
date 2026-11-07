//! Streamed import / export: JSON, NDJSON and CSV round trips.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::http::StatusCode;
use common::Api;
use serde_json::{Map, Value as Json, json};

fn def(name: &str, mode: &str) -> Json {
    json!({"name": name, "schema_mode": mode, "fields": [
        {"name": "title", "type": "text", "required": true},
        {"name": "n", "type": "number"},
        {"name": "count", "type": "number", "integer": true},
        {"name": "flag", "type": "bool"},
        {"name": "when", "type": "date"},
        {"name": "kind", "type": "select", "values": ["a", "b", "c"], "max_select": 3},
        {"name": "meta", "type": "json"},
        {"name": "note", "type": "text"},
    ]})
}

fn tricky_rows() -> Vec<Json> {
    let mut rows = vec![
        json!({"id": "row_a", "title": "plain", "n": 1.5, "count": 3, "flag": true, "when": "2024-05-06T07:08:09.000Z",
               "kind": ["a", "c"], "meta": {"k": [1, 2, {"deep": "x,y"}]}, "note": "hello"}),
        json!({"id": "row_b", "title": "comma, \"quotes\" and\nnewline", "n": -2, "count": 0, "flag": false,
               "kind": ["b"], "meta": [1, "two", null], "note": "ünïcödé ✓ 日本語"}),
        json!({"id": "row_c", "title": "} ] { [ \"brace\" \\ in a string", "note": "line1\r\nline2"}),
    ];
    for i in 0..620 {
        rows.push(json!({"id": format!("bulk{i:04}"), "title": format!("bulk {i}"), "n": i, "flag": i % 2 == 0}));
    }
    rows
}

/// Compares two sets of records on everything the user wrote (not on the
/// system timestamps and version).
fn comparable(items: &[Json]) -> Vec<Json> {
    let mut v: Vec<Json> = items
        .iter()
        .map(|r| {
            let mut m: Map<String, Json> = r.as_object().unwrap().clone();
            for k in ["created", "updated", "_version"] {
                m.remove(k);
            }
            // nulls and missing keys are the same thing for an unset field
            m.retain(|_, v| !v.is_null());
            Json::Object(m)
        })
        .collect();
    v.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    v
}

async fn all(api: &Api, collection: &str) -> Vec<Json> {
    let mut out = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let uri = match &cursor {
            Some(c) => format!("/api/v1/collections/{collection}/records?per_page=100&skip_total=true&cursor={c}"),
            None => format!("/api/v1/collections/{collection}/records?per_page=100&skip_total=true"),
        };
        let r = api.get(&uri).await.expect(StatusCode::OK).json();
        out.extend(r["items"].as_array().unwrap().iter().cloned());
        cursor = r["next_cursor"].as_str().map(str::to_owned);
        if cursor.is_none() {
            return out;
        }
    }
}

async fn seeded(mode: &str) -> (Api, Vec<Json>) {
    let api = Api::admin().await;
    api.make_collection(def("src", mode)).await;
    let rows = tricky_rows();
    for chunk in rows.chunks(300) {
        api.post("/api/v1/collections/src/records", json!(chunk))
            .await
            .expect(StatusCode::CREATED);
    }
    let stored = all(&api, "src").await;
    assert_eq!(stored.len(), rows.len());
    (api, stored)
}

async fn round_trip(format: &str, content_type: &str, chunk: usize) {
    let (api, original) = seeded("strict").await;
    let exported = api
        .get(&format!("/api/v1/collections/src/export?format={format}"))
        .await
        .expect(StatusCode::OK);
    assert!(
        exported.headers["content-disposition"]
            .to_str()
            .unwrap()
            .contains("src.")
    );
    let body = exported.bytes.clone();
    assert!(!body.is_empty());

    api.make_collection(def("dst", "strict")).await;
    let imported = api
        .post_chunked(
            &format!("/api/v1/collections/dst/import?format={format}"),
            content_type,
            &body,
            chunk,
        )
        .await
        .expect(StatusCode::OK)
        .json();
    assert_eq!(imported["imported"], original.len());
    assert_eq!(imported["format"], format);

    let copy = all(&api, "dst").await;
    assert_eq!(copy.len(), original.len());
    assert_eq!(
        comparable(&copy),
        comparable(&original),
        "{format} round trip changed data"
    );
    // ids were preserved
    assert!(copy.iter().any(|r| r["id"] == "row_b"));
    // and it is really in the database
    let n = api
        .post("/api/v1/sql", json!({"sql": "SELECT count(*) FROM dst"}))
        .await
        .json();
    assert_eq!(n["rows"][0][0], original.len());
}

#[tokio::test]
async fn json_export_then_import_round_trip() {
    round_trip("json", "application/json", 97).await;
}

#[tokio::test]
async fn ndjson_export_then_import_round_trip() {
    round_trip("ndjson", "application/x-ndjson", 61).await;
}

#[tokio::test]
async fn csv_export_then_import_round_trip() {
    round_trip("csv", "text/csv", 53).await;
}

#[tokio::test]
async fn export_shapes_and_filters() {
    let (api, original) = seeded("strict").await;
    // JSON: one array, terminated
    let j = api.get("/api/v1/collections/src/export").await.expect(StatusCode::OK);
    assert_eq!(j.headers["content-type"], "application/json");
    let arr: Vec<Json> = serde_json::from_slice(&j.bytes).unwrap();
    assert_eq!(arr.len(), original.len());
    // NDJSON: one object per line
    let nd = api
        .get("/api/v1/collections/src/export?format=ndjson")
        .await
        .expect(StatusCode::OK)
        .text();
    assert_eq!(nd.lines().count(), original.len());
    assert!(nd.ends_with('\n'));
    for line in nd.lines() {
        serde_json::from_str::<Json>(line).unwrap();
    }
    // CSV: header + rows; quoting; filter + sort + fields apply
    let csv = api
        .get("/api/v1/collections/src/export?format=csv&filter=id%20~%20'bulk00'&sort=id&fields=id,title")
        .await
        .expect(StatusCode::OK)
        .text();
    let lines: Vec<&str> = csv.lines().collect();
    assert_eq!(lines[0], "id,title");
    assert_eq!(lines[1], "bulk0000,bulk 0");
    assert_eq!(lines.len(), 1 + 100, "bulk 0000..0099");
    let full = api
        .get("/api/v1/collections/src/export?format=csv&filter=id%3D'row_b'")
        .await
        .text();
    assert!(
        full.starts_with("id,created,updated,title,n,count,flag,when,kind,meta,note\n"),
        "{full}"
    );
    assert!(full.contains("\"comma, \"\"quotes\"\" and\nnewline\""), "{full}");
    // empty result is still valid JSON
    let empty = api
        .get("/api/v1/collections/src/export?filter=title%3D'none'")
        .await
        .expect(StatusCode::OK);
    assert_eq!(serde_json::from_slice::<Vec<Json>>(&empty.bytes).unwrap().len(), 0);
    // errors before streaming are normal errors
    assert_eq!(
        api.get("/api/v1/collections/nope/export").await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        api.get("/api/v1/collections/src/export?format=xml").await.code(),
        "invalid_format"
    );
    assert_eq!(
        api.get("/api/v1/collections/src/export?filter=1%3B%20drop")
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn flexible_documents_round_trip_through_csv_extra_column() {
    let api = Api::admin().await;
    for name in ["docs_src", "docs_dst"] {
        api.make_collection(json!({"name": name, "schema_mode": "flexible",
            "fields": [{"name": "title", "type": "text"}]}))
            .await;
    }
    api.post(
        "/api/v1/collections/docs_src/records",
        json!([{"id": "d1", "title": "one", "specs": {"ram": 16}, "tags": ["x", "y"]},
               {"id": "d2", "title": "two", "weight": 3.5}]),
    )
    .await
    .expect(StatusCode::CREATED);
    let csv = api
        .get("/api/v1/collections/docs_src/export?format=csv")
        .await
        .expect(StatusCode::OK);
    assert!(csv.text().starts_with("id,created,updated,title,_extra\n"));
    api.post_chunked(
        "/api/v1/collections/docs_dst/import?format=csv",
        "text/csv",
        &csv.bytes,
        40,
    )
    .await
    .expect(StatusCode::OK);
    let (a, b) = (all(&api, "docs_src").await, all(&api, "docs_dst").await);
    assert_eq!(comparable(&a), comparable(&b));
    assert_eq!(b.iter().find(|r| r["id"] == "d1").unwrap()["specs"]["ram"], 16);
}

#[tokio::test]
async fn import_reports_the_failing_record_and_keeps_earlier_batches() {
    let api = Api::admin().await;
    api.make_collection(def("dst", "strict")).await;
    // 450 good rows, then a bad one (missing required title), then good rows.
    let mut lines: Vec<String> = (0..450)
        .map(|i| json!({"title": format!("t{i}")}).to_string())
        .collect();
    lines.push(json!({"n": 1}).to_string());
    lines.push(json!({"title": "after"}).to_string());
    let body = lines.join("\n");
    let r = api
        .post_chunked(
            "/api/v1/collections/dst/import?format=ndjson",
            "application/x-ndjson",
            body.as_bytes(),
            1000,
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    let j = r.json();
    assert_eq!(j["error"]["code"], "validation_failed");
    let imported = j["error"]["details"]["imported"].as_u64().unwrap();
    assert!(imported > 0 && imported <= 450, "{j}");
    let fields = j["error"]["details"]["fields"].as_object().unwrap();
    assert!(
        fields
            .keys()
            .any(|k| k.starts_with("record 451") && k.ends_with("title")),
        "{j}"
    );
    let stored = api.get("/api/v1/collections/dst/records/count").await.json()["count"]
        .as_u64()
        .unwrap();
    assert_eq!(stored, imported, "exactly the committed batches are stored");

    // malformed input
    for (fmt, ct, body) in [
        ("json", "application/json", "{\"not\": \"an array\"}"),
        ("json", "application/json", "[{\"title\": \"x\"}"),
        ("json", "application/json", "[{\"title\": \"x\"}] trailing"),
        ("json", "application/json", "[1, 2]"),
        ("ndjson", "application/x-ndjson", "{\"title\": \"x\"}\nnot json\n"),
        ("ndjson", "application/x-ndjson", "[1]\n"),
        ("csv", "text/csv", "title,title\nx,y\n"),
        ("csv", "text/csv", "title,n\nx\n"),
        ("csv", "text/csv", "title\n\"unterminated\n"),
        ("csv", "text/csv", ""),
        ("yaml", "text/plain", "a: 1"),
    ] {
        let r = api
            .post_chunked(
                &format!("/api/v1/collections/dst/import?format={fmt}"),
                ct,
                body.as_bytes(),
                7,
            )
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{fmt} {body:?}: {}", r.text());
    }
    // format from Content-Type, no query parameter
    let r = api
        .post_chunked(
            "/api/v1/collections/dst/import",
            "text/csv",
            b"title,n\nfrom csv,2\n",
            5,
        )
        .await
        .expect(StatusCode::OK)
        .json();
    assert_eq!((r["imported"].as_u64(), r["format"].as_str()), (Some(1), Some("csv")));
    // a cell that is not a number is a normal field error
    let r = api
        .post_chunked(
            "/api/v1/collections/dst/import?format=csv",
            "text/csv",
            b"title,n\nx,abc\n",
            5,
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.json()["error"]["code"], "validation_failed");
    // views and unknown collections
    api.post_chunked(
        "/api/v1/collections/nope/import?format=ndjson",
        "application/x-ndjson",
        b"{}\n",
        5,
    )
    .await
    .expect(StatusCode::NOT_FOUND);
}
