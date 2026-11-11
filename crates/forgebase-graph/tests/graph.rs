//! Graph API tests: SPEC §27 #14 (create nodes and edges) and #15 (traversal),
//! caps, authorisation, tenant isolation and a 10k-edge performance check.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::http::{Method, StatusCode};
use common::*;
use forgebase_data::AccessContext;
use forgebase_db::{Backend, Statement};
use serde_json::{Value as Json, json};

fn depths(r: &Json) -> Vec<(String, u64)> {
    r["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| {
            (
                n["node"]["id"].as_str().unwrap().to_owned(),
                n["depth"].as_u64().unwrap(),
            )
        })
        .collect()
}

fn d(items: &[(&str, u64)]) -> Vec<(String, u64)> {
    items.iter().map(|(i, n)| ((*i).to_owned(), *n)).collect()
}

// ---- #14 ---------------------------------------------------------------------

#[tokio::test]
async fn scenario_14_create_graph_nodes_and_edges() {
    let env = env().await;
    let c = env.admin();

    // User A -FOLLOWS-> User B -PURCHASED-> Product C (the SPEC §7 example).
    let (s, a) = c
        .post(
            "/nodes",
            json!({"id": "user_a", "label": "User", "properties": {"name": "Ann", "age": 31}}),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{a}");
    assert_eq!(a["id"], "user_a");
    assert_eq!(a["properties"]["name"], "Ann");
    c.node("user_b", "User").await;
    let (s, generated) = c
        .post("/nodes", json!({"label": "Product", "properties": {"sku": "C-1"}}))
        .await;
    assert_eq!(s, StatusCode::CREATED);
    let product = generated["id"].as_str().unwrap().to_owned();
    assert_eq!(product.len(), 32, "generated ids are 32 hex characters");

    let (s, e1) = c
        .post("/edges", json!({"id": "e1", "type": "FOLLOWS", "source": "user_a", "target": "user_b", "properties": {"since": 2024}}))
        .await;
    assert_eq!(s, StatusCode::CREATED, "{e1}");
    assert_eq!(e1["type"], "FOLLOWS");
    assert_eq!(e1["directed"], true);
    assert_eq!(e1["properties"]["since"], 2024);
    let (s, _) = c
        .post(
            "/edges",
            json!({"id": "e2", "type": "PURCHASED", "source": "user_b", "target": product}),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);

    // read back, list, filter
    let (s, n) = c.get("/nodes/user_a").await;
    assert_eq!((s, n["label"].as_str()), (StatusCode::OK, Some("User")));
    let (_, users) = c.get("/nodes?label=User").await;
    assert_eq!(ids(&users["items"]), ["user_a", "user_b"]);
    let (_, page) = c.get("/nodes?limit=2").await;
    assert_eq!(page["items"].as_array().unwrap().len(), 2);
    assert!(page["next_after"].is_string());
    let (_, rest) = c
        .get(&format!(
            "/nodes?limit=2&after={}",
            page["next_after"].as_str().unwrap()
        ))
        .await;
    assert_eq!(rest["items"].as_array().unwrap().len(), 1);
    assert!(rest["next_after"].is_null());
    let (_, purchased) = c.get("/edges?type=PURCHASED").await;
    assert_eq!(ids(&purchased["items"]), ["e2"]);
    let (_, from_a) = c.get("/edges?source=user_a").await;
    assert_eq!(ids(&from_a["items"]), ["e1"]);
    let (_, stats) = c.get("/stats").await;
    assert_eq!(stats["nodes"], 3);
    assert_eq!(stats["edges"], 2);
    assert_eq!(
        stats["edge_types"],
        json!([{"type": "FOLLOWS", "count": 1}, {"type": "PURCHASED", "count": 1}])
    );

    // it is really stored: a second client (separate request path) and the table agree
    let rows = env
        .db
        .fetch_all(&Statement::new(
            "SELECT count(*) FROM _forgebase_graph_edges WHERE tenant = '' AND type = ?1",
            vec!["FOLLOWS".into()],
        ))
        .await
        .unwrap();
    assert_eq!(rows.scalar().and_then(forgebase_db::Value::as_i64), Some(1));

    // update: merge patch and replace
    let (s, n) = c
        .call(
            Method::PATCH,
            "/nodes/user_a",
            Some(json!({"properties": {"age": null, "city": "Pune"}})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{n}");
    assert_eq!(n["properties"], json!({"name": "Ann", "city": "Pune"}));
    let (_, n) = c
        .call(
            Method::PUT,
            "/nodes/user_a",
            Some(json!({"label": "Member", "properties": {"x": 1}})),
        )
        .await;
    assert_eq!(
        (n["label"].as_str(), n["properties"].clone()),
        (Some("Member"), json!({"x": 1}))
    );
    let (_, e) = c
        .call(Method::PATCH, "/edges/e1", Some(json!({"properties": {"weight": 2}})))
        .await;
    assert_eq!(e["properties"], json!({"since": 2024, "weight": 2}));

    // errors
    let (s, e) = c.post("/nodes", json!({"id": "user_a"})).await;
    assert_eq!(
        (s, e["error"]["code"].as_str()),
        (StatusCode::CONFLICT, Some("node_exists"))
    );
    let (s, e) = c
        .post(
            "/edges",
            json!({"id": "e1", "type": "X", "source": "user_a", "target": "user_b"}),
        )
        .await;
    assert_eq!(
        (s, e["error"]["code"].as_str()),
        (StatusCode::CONFLICT, Some("edge_exists"))
    );
    let (s, e) = c
        .post("/edges", json!({"type": "X", "source": "user_a", "target": "ghost"}))
        .await;
    assert_eq!(
        (s, e["error"]["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("edge_endpoint_not_found"))
    );
    let (s, e) = c.get("/nodes/ghost").await;
    assert_eq!(
        (s, e["error"]["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("node_not_found"))
    );
    let (s, e) = c.get("/edges/ghost").await;
    assert_eq!(
        (s, e["error"]["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("edge_not_found"))
    );
    for bad in [
        json!({"id": "has space"}),
        json!({"id": "a'b"}),
        json!({"id": ""}),
        json!({"properties": [1]}),
        json!({"label": "x".repeat(65)}),
        json!({"surprise": 1}),
    ] {
        let (s, e) = c.post("/nodes", bad.clone()).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{bad} -> {e}");
    }
    let (s, e) = c
        .post(
            "/edges",
            json!({"type": "bad type", "source": "user_a", "target": "user_b"}),
        )
        .await;
    assert_eq!(
        (s, e["error"]["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_edge_type"))
    );
    let big = "x".repeat(70_000);
    let (s, e) = c.post("/nodes", json!({"properties": {"big": big}})).await;
    assert_eq!(
        (s, e["error"]["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("properties_too_large"))
    );
    let (s, _) = c.call(Method::POST, "/nodes", None).await;
    assert_eq!(s, StatusCode::UNSUPPORTED_MEDIA_TYPE);

    // delete an edge, then a node: its edges go with it
    let (s, _) = c.delete("/edges/e2").await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert_eq!(c.get("/edges/e2").await.0, StatusCode::NOT_FOUND);
    c.edge("e3", "FOLLOWS", "user_b", "user_a").await;
    let (s, r) = c.delete("/nodes/user_a").await;
    assert_eq!((s, r["deleted_edges"].as_u64()), (StatusCode::OK, Some(2)));
    assert_eq!(c.get("/edges/e1").await.0, StatusCode::NOT_FOUND);
    assert_eq!(c.get("/edges/e3").await.0, StatusCode::NOT_FOUND);
    assert_eq!(c.get("/nodes/user_b").await.0, StatusCode::OK);
    assert_eq!(c.delete("/nodes/user_a").await.0, StatusCode::NOT_FOUND);

    // restart durability: a fresh service over the same database sees the data
    let again = forgebase_graph::GraphService::new(env.data.clone());
    let got = again.get_node(&AccessContext::admin("root"), "user_b").await.unwrap();
    assert_eq!(got.label, "User");
}

// ---- #15 ---------------------------------------------------------------------

/// a -FOLLOWS-> b -> c -> d -> e -FOLLOWS-> a (a cycle), a -LIKES-> x,
/// b -KNOWS- y (undirected), z isolated.
async fn seed_cycle(c: &Client) {
    for (id, label) in [
        ("a", "User"),
        ("b", "User"),
        ("c", "User"),
        ("d", "User"),
        ("e", "User"),
        ("x", "Product"),
        ("y", "User"),
        ("z", "User"),
    ] {
        c.node(id, label).await;
    }
    for (id, s, t) in [
        ("f1", "a", "b"),
        ("f2", "b", "c"),
        ("f3", "c", "d"),
        ("f4", "d", "e"),
        ("f5", "e", "a"),
    ] {
        c.edge(id, "FOLLOWS", s, t).await;
    }
    c.edge("l1", "LIKES", "a", "x").await;
    c.edge_dir("k1", "KNOWS", "b", "y", false).await;
}

#[tokio::test]
async fn scenario_15_traversal_neighbors_and_shortest_path() {
    let env = env().await;
    let c = env.admin();
    seed_cycle(&c).await;
    let trav = |body: Json| {
        let c = &c;
        async move { c.post("/traverse", body).await }
    };

    // depth limit, edge type, the SPEC example shape (`edge`, `depth`)
    let (s, r) = trav(json!({"start": "a", "edge": "FOLLOWS", "depth": 2})).await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!(depths(&r), d(&[("b", 1), ("c", 2)]));
    assert_eq!(r["truncated"], false);
    // edges between the returned nodes and the start: a-b and b-c
    assert_eq!(ids(&r["edges"]), ["f1", "f2"]);

    // a cycle terminates; the start is not repeated; depths are fewest hops
    let (_, r) = trav(json!({"start": "a", "edge_types": ["FOLLOWS"], "max_depth": 10})).await;
    assert_eq!(depths(&r), d(&[("b", 1), ("c", 2), ("d", 3), ("e", 4)]));
    let (_, r) = trav(json!({"start": "a", "edge_types": ["FOLLOWS"], "max_depth": 10, "include_start": true})).await;
    assert_eq!(depths(&r)[0], ("a".to_owned(), 0));
    assert_eq!(depths(&r).len(), 5);

    // all edge types; undirected KNOWS is walked both ways
    let (_, r) = trav(json!({"start": "a", "max_depth": 2})).await;
    assert_eq!(depths(&r), d(&[("b", 1), ("x", 1), ("c", 2), ("y", 2)]));
    let (_, r) = trav(json!({"start": "y", "max_depth": 1})).await;
    assert_eq!(depths(&r), d(&[("b", 1)]), "out from the target of an undirected edge");
    let (_, r) = trav(json!({"start": "b", "edge_types": ["KNOWS"], "max_depth": 1})).await;
    assert_eq!(depths(&r), d(&[("y", 1)]));

    // direction in / both
    let (_, r) = trav(json!({"start": "a", "direction": "in", "edge_types": ["FOLLOWS"], "max_depth": 3})).await;
    assert_eq!(depths(&r), d(&[("e", 1), ("d", 2), ("c", 3)]));
    let (_, r) = trav(json!({"start": "a", "direction": "both", "edge_types": ["FOLLOWS"], "max_depth": 1})).await;
    assert_eq!(depths(&r), d(&[("b", 1), ("e", 1)]));
    let (_, r) = trav(json!({"start": "x", "direction": "in", "max_depth": 2})).await;
    assert_eq!(depths(&r), d(&[("a", 1), ("e", 2)]));

    // filters on the returned nodes
    let (_, r) = trav(json!({"start": "a", "max_depth": 1, "filter": {"label": "Product"}})).await;
    assert_eq!(depths(&r), d(&[("x", 1)]));
    c.call(
        Method::PATCH,
        "/nodes/c",
        Some(json!({"properties": {"country": "IN", "vip": true, "score": 7}})),
    )
    .await;
    let (_, r) = trav(
        json!({"start": "a", "max_depth": 3, "filter": {"properties": {"country": "IN", "vip": true, "score": 7}}}),
    )
    .await;
    assert_eq!(depths(&r), d(&[("c", 2)]));
    let (s, r) = trav(json!({"start": "a", "filter": {"properties": {"bad\"key": "v"}}})).await;
    assert_eq!(
        (s, r["error"]["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_filter"))
    );
    // a value that looks like SQL is only a value
    let (s, r) = trav(json!({"start": "a", "filter": {"properties": {"country": "x' OR 1=1 --"}}})).await;
    assert_eq!((s, r["nodes"].as_array().unwrap().len()), (StatusCode::OK, 0));
    // an injection attempt in start or edge type is a plain lookup / a validation error
    let (s, _) = trav(json!({"start": "a' OR '1'='1"})).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, r) = trav(json!({"start": "a", "edge_types": ["FOLLOWS') --"]})).await;
    assert_eq!(
        (s, r["error"]["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_edge_type"))
    );
    assert_eq!(c.get("/stats").await.1["nodes"], 8, "nothing was dropped or changed");

    // result limit and truncation
    let (_, r) = trav(json!({"start": "a", "max_depth": 10, "limit": 2})).await;
    assert_eq!(r["nodes"].as_array().unwrap().len(), 2);
    assert_eq!(r["truncated"], true);
    let (_, r) = trav(json!({"start": "a", "max_depth": 10, "include_edges": false})).await;
    assert!(r["edges"].as_array().unwrap().is_empty());

    // caps
    for (body, code) in [
        (json!({"start": "a", "max_depth": 0}), "invalid_depth"),
        (json!({"start": "a", "max_depth": 11}), "invalid_depth"),
        (json!({"start": "a", "limit": 0}), "invalid_limit"),
        (json!({"start": "a", "limit": 1001}), "invalid_limit"),
        (
            json!({"start": "a", "edge_types": vec!["T"; 21]}),
            "too_many_edge_types",
        ),
    ] {
        let (s, r) = trav(body.clone()).await;
        assert_eq!(
            (s, r["error"]["code"].as_str()),
            (StatusCode::BAD_REQUEST, Some(code)),
            "{body}"
        );
    }
    assert_eq!(trav(json!({"start": "nope"})).await.0, StatusCode::NOT_FOUND);

    // neighbours
    let (s, r) = c.get("/nodes/a/neighbors").await;
    assert_eq!(s, StatusCode::OK, "{r}");
    let pairs = |r: &Json| -> Vec<(String, String, String)> {
        r["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| {
                (
                    i["direction"].as_str().unwrap().to_owned(),
                    i["edge"]["id"].as_str().unwrap().to_owned(),
                    i["node"]["id"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    };
    let t = |a: &str, b: &str, c: &str| (a.to_owned(), b.to_owned(), c.to_owned());
    assert_eq!(pairs(&r), [t("out", "f1", "b"), t("out", "l1", "x")]);
    let (_, r) = c.get("/nodes/a/neighbors?direction=in").await;
    assert_eq!(pairs(&r), [t("in", "f5", "e")]);
    let (_, r) = c.get("/nodes/a/neighbors?direction=both&type=FOLLOWS").await;
    assert_eq!(pairs(&r), [t("out", "f1", "b"), t("in", "f5", "e")]);
    let (_, r) = c.get("/nodes/y/neighbors").await;
    assert_eq!(
        pairs(&r),
        [t("in", "k1", "b")],
        "undirected: reachable, seen from the target side"
    );
    let (_, r) = c
        .get("/nodes/b/neighbors?direction=both&type=KNOWS,FOLLOWS&limit=2")
        .await;
    assert_eq!(r["items"].as_array().unwrap().len(), 2);
    assert_eq!(r["next_after"], "2");
    let (_, r) = c
        .get("/nodes/b/neighbors?direction=both&type=KNOWS,FOLLOWS&limit=2&offset=2")
        .await;
    assert_eq!(r["items"].as_array().unwrap().len(), 1);
    assert!(r["next_after"].is_null());
    // self loops once, parallel edges both
    c.edge("loop", "SELF", "z", "z").await;
    c.edge("p1", "P", "z", "y").await;
    c.edge("p2", "P", "z", "y").await;
    let (_, r) = c.get("/nodes/z/neighbors?direction=both").await;
    assert_eq!(pairs(&r).len(), 3, "loop once plus two parallel edges: {r}");
    assert_eq!(
        c.get("/nodes/a/neighbors?direction=sideways").await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(c.get("/nodes/ghost/neighbors").await.0, StatusCode::NOT_FOUND);
    assert_eq!(c.get("/nodes/a/neighbors?limit=1001").await.0, StatusCode::BAD_REQUEST);

    // shortest path
    let path = |r: &Json| ids(&r["nodes"]);
    let (s, r) = c.post("/shortest-path", json!({"from": "a", "to": "d"})).await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!((r["found"].clone(), r["length"].as_u64()), (json!(true), Some(3)));
    assert_eq!(path(&r), ["a", "b", "c", "d"]);
    assert_eq!(ids(&r["edges"]), ["f1", "f2", "f3"]);
    c.edge("shortcut", "LIKES", "a", "d").await;
    let (_, r) = c.post("/shortest-path", json!({"from": "a", "to": "d"})).await;
    assert_eq!(
        (path(&r), r["length"].as_u64()),
        (vec!["a".to_owned(), "d".to_owned()], Some(1))
    );
    let (_, r) = c
        .post(
            "/shortest-path",
            json!({"from": "a", "to": "d", "edge_types": ["FOLLOWS"]}),
        )
        .await;
    assert_eq!(r["length"].as_u64(), Some(3));
    let (_, r) = c
        .post(
            "/shortest-path",
            json!({"from": "a", "to": "d", "edge_types": ["FOLLOWS"], "max_depth": 2}),
        )
        .await;
    assert_eq!((r["found"].clone(), r["nodes"].clone()), (json!(false), json!([])));
    let (_, r) = c
        .post(
            "/shortest-path",
            json!({"from": "d", "to": "a", "direction": "in", "edge_types": ["FOLLOWS"]}),
        )
        .await;
    assert_eq!(path(&r), ["d", "c", "b", "a"], "walking edges backwards");
    let (_, r) = c.post("/shortest-path", json!({"from": "a", "to": "z"})).await;
    assert_eq!(r["found"], false, "z is not connected to a");
    let (_, r) = c.post("/shortest-path", json!({"from": "a", "to": "y"})).await;
    assert_eq!(path(&r), ["a", "b", "y"], "through the undirected edge");
    let (_, r) = c
        .post(
            "/shortest-path",
            json!({"from": "y", "to": "a", "direction": "both", "edge_types": ["KNOWS", "FOLLOWS"]}),
        )
        .await;
    assert_eq!(
        path(&r),
        ["y", "b", "a"]
            .map(String::from)
            .to_vec()
            .iter()
            .rev()
            .rev()
            .cloned()
            .collect::<Vec<_>>()
    );
    let (_, r) = c.post("/shortest-path", json!({"from": "a", "to": "a"})).await;
    assert_eq!(
        (r["found"].clone(), r["length"].as_u64(), path(&r)),
        (json!(true), Some(0), vec!["a".to_owned()])
    );
    for body in [
        json!({"from": "a", "to": "d", "max_depth": 11}),
        json!({"from": "a", "to": "d", "max_depth": 0}),
    ] {
        assert_eq!(c.post("/shortest-path", body).await.0, StatusCode::BAD_REQUEST);
    }
    assert_eq!(
        c.post("/shortest-path", json!({"from": "a", "to": "ghost"})).await.0,
        StatusCode::NOT_FOUND
    );
}

// ---- authorisation -------------------------------------------------------------

#[tokio::test]
async fn anonymous_is_denied_everywhere_and_nothing_is_written() {
    let env = env().await;
    env.admin().node("a", "User").await;
    let anon = env.client(None);
    let probes: Vec<(Method, &str, Option<Json>)> = vec![
        (Method::GET, "/nodes", None),
        (Method::POST, "/nodes", Some(json!({"id": "n"}))),
        (Method::GET, "/nodes/a", None),
        (Method::PUT, "/nodes/a", Some(json!({"label": "X"}))),
        (Method::PATCH, "/nodes/a", Some(json!({"properties": {"x": 1}}))),
        (Method::DELETE, "/nodes/a", None),
        (Method::GET, "/nodes/a/neighbors", None),
        (Method::GET, "/edges", None),
        (
            Method::POST,
            "/edges",
            Some(json!({"type": "T", "source": "a", "target": "a"})),
        ),
        (Method::GET, "/edges/e", None),
        (Method::PATCH, "/edges/e", Some(json!({}))),
        (Method::DELETE, "/edges/e", None),
        (Method::POST, "/traverse", Some(json!({"start": "a"}))),
        (Method::POST, "/shortest-path", Some(json!({"from": "a", "to": "a"}))),
        (Method::GET, "/stats", None),
        // unknown ids must not leak existence either
        (Method::GET, "/nodes/does-not-exist", None),
        (Method::POST, "/traverse", Some(json!({"start": "does-not-exist"}))),
    ];
    for (m, p, b) in probes {
        let (s, r) = anon.call(m.clone(), p, b).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{m} {p}: {r}");
        assert_eq!(r["error"]["code"], "unauthenticated");
    }
    // the admin still sees exactly what it created
    let (_, n) = env.admin().get("/nodes/a").await;
    assert_eq!(
        (n["label"].as_str(), n["properties"].clone()),
        (Some("User"), json!({}))
    );
    assert_eq!(env.admin().get("/nodes").await.1["items"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn policy_decisions_are_honoured_and_fail_closed() {
    let env = env_with(Arc::new(TestPolicy)).await;
    let admin = env.client(Some(admin_in("t1")));
    admin.node("a", "User").await;
    admin.node("b", "User").await;
    let user = |id: &str| env.client(Some(AccessContext::user("users", id).with_tenant("t1")));

    let reader = user("reader");
    assert_eq!(reader.get("/nodes/a").await.0, StatusCode::OK);
    assert_eq!(reader.post("/traverse", json!({"start": "a"})).await.0, StatusCode::OK);
    let (s, r) = reader.post("/nodes", json!({"id": "n"})).await;
    assert_eq!(
        (s, r["error"]["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("forbidden"))
    );
    assert_eq!(reader.delete("/nodes/a").await.0, StatusCode::FORBIDDEN);

    let writer = user("writer");
    assert_eq!(writer.post("/nodes", json!({"id": "n"})).await.0, StatusCode::CREATED);
    writer.edge("w1", "T", "a", "b").await;

    assert_eq!(user("stranger").get("/nodes/a").await.0, StatusCode::FORBIDDEN);
    // FB-139: a Filter decision is a label rule, compiled into the statement
    // (this one is `true`); it never grants writes.
    let (s, r) = user("rowfilter").get("/nodes/a").await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!(user("rowfilter").delete("/nodes/a").await.0, StatusCode::FORBIDDEN);
    // FB-139: callers that are not privileged need a tenant, but only once
    // they are allowed at all.
    let tenantless = |id: &str| env.client(Some(AccessContext::user("users", id)));
    let (s, r) = tenantless("reader").get("/nodes/a").await;
    assert_eq!(
        (s, r["error"]["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("tenant_required")),
        "{r}"
    );
    assert_eq!(tenantless("stranger").get("/nodes/a").await.0, StatusCode::FORBIDDEN);
    // the default authorizer lets non-admin users do nothing
    let plain = env_with(Arc::new(forgebase_data::DefaultAuthorizer)).await;
    assert_eq!(
        plain
            .client(Some(AccessContext::user("users", "u1")))
            .get("/nodes")
            .await
            .0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn errors_carry_the_request_id_and_use_the_envelope() {
    let env = env().await;
    let (s, r) = env
        .admin()
        .call_with(Method::GET, "/nodes/ghost", None, &[("x-request-id", "req-42")])
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(r["error"]["request_id"], "req-42");
    let (s, r) = env
        .admin()
        .call(Method::POST, "/nodes", Some(json!("not an object")))
        .await;
    assert_eq!(
        (s, r["error"]["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_body"))
    );
    let (s, _) = env
        .admin()
        .call(
            Method::POST,
            "/nodes",
            Some(json!({"properties": {"b": "x".repeat(1_100_000)}})),
        )
        .await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
}

// ---- tenant isolation ------------------------------------------------------------

#[tokio::test]
async fn tenants_cannot_see_or_touch_each_other() {
    let env = env().await;
    let a = env.client(Some(admin_in("tenant-a")));
    let b = env.client(Some(admin_in("tenant-b")));
    let none = env.client(Some(AccessContext::admin("root")));

    // the same ids live independently in each tenant
    for t in [&a, &b] {
        t.node("u1", "User").await;
        t.node("u2", "User").await;
    }
    a.node("only-a", "User").await;
    a.edge("e1", "FOLLOWS", "u1", "u2").await;
    a.edge("e2", "FOLLOWS", "u2", "only-a").await;

    // B sees none of A's data
    assert_eq!(b.get("/nodes/only-a").await.0, StatusCode::NOT_FOUND);
    assert_eq!(b.get("/edges/e1").await.0, StatusCode::NOT_FOUND);
    assert!(b.get("/edges").await.1["items"].as_array().unwrap().is_empty());
    assert_eq!(ids(&b.get("/nodes").await.1["items"]), ["u1", "u2"]);
    assert_eq!(b.get("/stats").await.1["edges"], 0);
    let (_, r) = b.post("/traverse", json!({"start": "u1", "max_depth": 5})).await;
    assert!(r["nodes"].as_array().unwrap().is_empty());
    let (_, r) = b.get("/nodes/u1/neighbors?direction=both").await;
    assert!(r["items"].as_array().unwrap().is_empty());
    let (_, r) = b.post("/shortest-path", json!({"from": "u1", "to": "u2"})).await;
    assert_eq!(r["found"], false);
    assert_eq!(
        b.post("/traverse", json!({"start": "only-a"})).await.0,
        StatusCode::NOT_FOUND
    );

    // A sees its own graph
    let (_, r) = a.post("/traverse", json!({"start": "u1", "max_depth": 5})).await;
    assert_eq!(depths(&r), d(&[("u2", 1), ("only-a", 2)]));
    let (_, r) = a.post("/shortest-path", json!({"from": "u1", "to": "only-a"})).await;
    assert_eq!(r["length"], 2);

    // B cannot change or delete A's rows, nor link to them
    assert_eq!(b.delete("/edges/e1").await.0, StatusCode::NOT_FOUND);
    assert_eq!(
        b.call(Method::PATCH, "/edges/e1", Some(json!({"properties": {"x": 1}})))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        b.call(Method::PATCH, "/nodes/only-a", Some(json!({"label": "Hacked"})))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(b.delete("/nodes/only-a").await.0, StatusCode::NOT_FOUND);
    let (s, r) = b
        .post("/edges", json!({"type": "FOLLOWS", "source": "u1", "target": "only-a"}))
        .await;
    assert_eq!(
        (s, r["error"]["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("edge_endpoint_not_found"))
    );
    // deleting u1 in B leaves A's u1 and its edge alone
    b.delete("/nodes/u1").await;
    assert_eq!(a.get("/nodes/u1").await.0, StatusCode::OK);
    assert_eq!(a.get("/edges/e1").await.0, StatusCode::OK);
    assert_eq!(a.get("/nodes/only-a").await.1["label"], "User");
    // an edge id can repeat across tenants
    b.edge("e1", "OTHER", "u2", "u2").await;
    assert_eq!(a.get("/edges/e1").await.1["type"], "FOLLOWS");

    // the default tenant is a tenant too: an administrator without one sees none of the above
    assert!(none.get("/nodes").await.1["items"].as_array().unwrap().is_empty());
    none.node("u1", "Default").await;
    assert_eq!(a.get("/nodes/u1").await.1["label"], "User");
}

// ---- performance sanity -----------------------------------------------------------

#[tokio::test]
async fn ten_thousand_edges_stay_fast_and_use_the_directional_indexes() {
    let env = env().await;
    const NODES: usize = 2_000;
    const EDGES: usize = 10_000;

    seed_random(&env, NODES, EDGES).await;

    // Both directions are served by an index.
    for (col, index) in [
        ("source", "_forgebase_graph_edges_out"),
        ("target", "_forgebase_graph_edges_in"),
    ] {
        let plan = env
            .db
            .explain(&Statement::new(
                format!("SELECT target, source FROM _forgebase_graph_edges WHERE tenant = '' AND {col} = 'n1' AND type = 'LIKES'"),
                vec![],
            ))
            .await
            .unwrap();
        let text: Vec<_> = plan.iter().map(|p| p.detail.clone()).collect();
        assert!(text.iter().any(|d| d.contains(index)), "{col}: {text:?}");
    }

    let c = env.admin();
    // Generous: this guards against a quadratic blow-up, not against a slow or busy machine.
    let limit = Duration::from_secs(60);
    let t = Instant::now();
    let (s, r) = c
        .post("/traverse", json!({"start": "n0", "max_depth": 4, "limit": 1000}))
        .await;
    let took = t.elapsed();
    assert_eq!(s, StatusCode::OK, "{r}");
    assert!(
        r["nodes"].as_array().unwrap().len() > 100,
        "{}",
        r["nodes"].as_array().unwrap().len()
    );
    assert!(took < limit, "traverse took {took:?}");

    let t = Instant::now();
    let (_, r) = c.post("/traverse", json!({"start": "n0", "max_depth": 10, "limit": 1000, "edge_types": ["FOLLOWS"], "direction": "both", "include_edges": false})).await;
    assert!(!r["nodes"].as_array().unwrap().is_empty());
    assert!(t.elapsed() < limit, "deep traverse took {:?}", t.elapsed());

    let t = Instant::now();
    let (_, r) = c
        .post(
            "/shortest-path",
            json!({"from": "n0", "to": "n1500", "direction": "both", "max_depth": 10}),
        )
        .await;
    assert_eq!(r["found"], true, "{r}");
    assert!(r["length"].as_u64().unwrap() <= 10);
    assert!(t.elapsed() < limit, "shortest path took {:?}", t.elapsed());

    let t = Instant::now();
    let (_, r) = c.get("/nodes/n7/neighbors?direction=both&limit=1000").await;
    assert!(!r["items"].as_array().unwrap().is_empty());
    assert!(
        t.elapsed() < Duration::from_secs(20),
        "neighbors took {:?}",
        t.elapsed()
    );
    assert_eq!(c.get("/stats").await.1["edges"], EDGES);
}
