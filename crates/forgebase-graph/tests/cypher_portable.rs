//! The portable openCypher engine (ADR-0034): `POST /graph/cypher` without
//! Apache AGE compiles a read-only subset to SQL over the canonical tables.
//! Runs on whichever backend `FORGEBASE_TEST_BACKEND` selects (the
//! PostgreSQL test database sets up no extensions, so AGE is not in use
//! there either); the differential test against AGE is in `cypher.rs`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]

mod common;

use std::collections::HashMap;
use std::sync::Arc;

use axum::http::StatusCode;
use forgebase_core::ErrorCode;
use forgebase_data::schema::Collection;
use forgebase_data::{AccessContext, Action, Authorizer, Decision, Predicate, Principal, ResourceKind, ResourceRef};
use forgebase_graph::{CypherQuery, CypherResult, GraphError, GraphService, NewEdge, NewNode};
use serde_json::{Value as Json, json};

/// Users: `cy` may read the graph and run openCypher; `reader` may only read
/// the graph; `ruled` may run openCypher under label and edge-type rules:
/// `Secret` nodes and `HIDDEN` edges are hidden, `Doc` nodes need
/// `properties.team = 'a'`, `RATED` edges need `properties.stars >= 3`.
#[derive(Debug)]
struct Policy;

fn user_of(ctx: &AccessContext) -> Option<&str> {
    match &ctx.principal {
        Principal::User { id, .. } => Some(id.as_str()),
        _ => None,
    }
}

impl Authorizer for Policy {
    fn authorize(&self, ctx: &AccessContext, action: Action, _c: Option<&Collection>) -> Decision {
        if ctx.is_privileged() {
            return Decision::Allow;
        }
        match (user_of(ctx), action) {
            (Some("cy" | "ruled"), Action::GraphRead | Action::GraphCypher) => Decision::Allow,
            (Some("reader"), Action::GraphRead) => Decision::Allow,
            _ => Decision::Deny,
        }
    }

    fn authorize_resource(&self, ctx: &AccessContext, action: Action, r: &ResourceRef) -> Decision {
        let base = self.authorize(ctx, action, None);
        if user_of(ctx) != Some("ruled") || !matches!(base, Decision::Allow) {
            return base;
        }
        let pred = |s: &str| Decision::Filter(Predicate::parse(s, HashMap::new()).unwrap());
        match (r.kind, r.name.as_str()) {
            (ResourceKind::GraphLabel, "Secret") | (ResourceKind::GraphEdgeType, "HIDDEN") => Decision::Deny,
            (ResourceKind::GraphLabel, "Doc") => pred("properties.team = 'a'"),
            (ResourceKind::GraphEdgeType, "RATED") => pred("properties.stars >= 3"),
            _ => Decision::Allow,
        }
    }
}

fn admin(tenant: &str) -> AccessContext {
    AccessContext::admin("root").with_tenant(tenant)
}

fn user(id: &str, tenant: &str) -> AccessContext {
    AccessContext::user("users", id).with_tenant(tenant)
}

fn q(query: &str) -> CypherQuery {
    CypherQuery {
        query: query.into(),
        params: None,
        limit: None,
        timeout_ms: None,
    }
}

fn qp(query: &str, params: Json) -> CypherQuery {
    CypherQuery {
        params: Some(params),
        ..q(query)
    }
}

async fn node(g: &GraphService, ctx: &AccessContext, id: &str, label: &str, props: Json) {
    g.create_node(
        ctx,
        NewNode {
            id: Some(id.into()),
            label: Some(label.into()),
            properties: Some(props),
        },
    )
    .await
    .unwrap();
}

async fn edge(g: &GraphService, ctx: &AccessContext, id: &str, ty: &str, s: &str, t: &str, props: Json) {
    edge_dir(g, ctx, id, ty, s, t, props, true).await;
}

#[allow(clippy::too_many_arguments)]
async fn edge_dir(
    g: &GraphService,
    ctx: &AccessContext,
    id: &str,
    ty: &str,
    s: &str,
    t: &str,
    props: Json,
    directed: bool,
) {
    g.create_edge(
        ctx,
        NewEdge {
            id: Some(id.into()),
            edge_type: ty.into(),
            source: s.into(),
            target: t.into(),
            directed: Some(directed),
            properties: Some(props),
        },
    )
    .await
    .unwrap();
}

/// Tenant `t1`: people who know each other in a cycle a → b → c → d → a,
/// a company, documents, a secret; tenant `t2`: one person.
pub async fn fixture(g: &GraphService) {
    let t1 = admin("t1");
    node(
        g,
        &t1,
        "a",
        "Person",
        json!({"name": "Ann", "age": 30, "city": "Paris", "tags": ["x", "y"], "active": true}),
    )
    .await;
    node(
        g,
        &t1,
        "b",
        "Person",
        json!({"name": "Bob", "age": 25, "city": "Berlin", "active": false}),
    )
    .await;
    node(
        g,
        &t1,
        "c",
        "Person",
        json!({"name": "Cid", "age": 35, "city": "Paris", "score": 1.5}),
    )
    .await;
    node(
        g,
        &t1,
        "d",
        "Person",
        json!({"name": "Dee", "city": "Rome", "score": 2.5}),
    )
    .await;
    node(g, &t1, "acme", "Company", json!({"name": "Acme"})).await;
    node(g, &t1, "doc1", "Doc", json!({"name": "Plan", "team": "a"})).await;
    node(g, &t1, "doc2", "Doc", json!({"name": "Budget", "team": "b"})).await;
    node(g, &t1, "s1", "Secret", json!({"name": "Vault"})).await;
    edge(g, &t1, "ab", "KNOWS", "a", "b", json!({"w": 1})).await;
    edge(g, &t1, "bc", "KNOWS", "b", "c", json!({"w": 2})).await;
    edge(g, &t1, "cd", "KNOWS", "c", "d", json!({"w": 3})).await;
    edge(g, &t1, "da", "KNOWS", "d", "a", json!({"w": 4})).await;
    edge(g, &t1, "a-acme", "WORKS_AT", "a", "acme", json!({"since": 2020})).await;
    edge(g, &t1, "b-acme", "WORKS_AT", "b", "acme", json!({"since": 2021})).await;
    edge(g, &t1, "ac", "LIKES", "a", "c", json!({})).await;
    edge_dir(g, &t1, "bd", "FRIEND", "b", "d", json!({}), false).await;
    edge(g, &t1, "a-doc1", "RATED", "a", "doc1", json!({"stars": 5})).await;
    edge(g, &t1, "a-doc2", "RATED", "a", "doc2", json!({"stars": 1})).await;
    edge(g, &t1, "c-doc1", "RATED", "c", "doc1", json!({"stars": 2})).await;
    edge(g, &t1, "a-s1", "OWNS", "a", "s1", json!({})).await;
    edge(g, &t1, "b-s1", "HIDDEN", "b", "s1", json!({})).await;
    edge(g, &t1, "c-a", "HIDDEN", "c", "a", json!({})).await;
    node(g, &admin("t2"), "z", "Person", json!({"name": "Zed", "age": 50})).await;
}

struct Env {
    graph: GraphService,
    _db: forgebase_db::testing::TestDb,
}

async fn env() -> Env {
    let test = forgebase_db::testing::test_backend().await;
    let db = test.database().clone();
    let data = forgebase_data::DataService::new(Arc::new(db), Arc::new(Policy))
        .await
        .unwrap();
    let graph = GraphService::new(data);
    fixture(&graph).await;
    Env { graph, _db: test }
}

async fn cy(g: &GraphService, ctx: &AccessContext, query: CypherQuery) -> CypherResult {
    let text = query.query.clone();
    let r = g.cypher(ctx, query).await.unwrap_or_else(|e| panic!("{text}: {e}"));
    assert_eq!(r.engine, "portable", "{text}");
    r
}

/// All rows, as JSON arrays.
async fn rows(g: &GraphService, ctx: &AccessContext, query: &str) -> Vec<Vec<Json>> {
    cy(g, ctx, q(query)).await.rows
}

/// The first column, as strings, sorted.
async fn col(g: &GraphService, ctx: &AccessContext, query: &str) -> Vec<String> {
    let mut v: Vec<String> = rows(g, ctx, query)
        .await
        .iter()
        .map(|r| match &r[0] {
            Json::String(s) => s.clone(),
            other => other.to_string(),
        })
        .collect();
    v.sort();
    v
}

fn code(e: &GraphError) -> &'static str {
    e.code()
}

async fn err(g: &GraphService, ctx: &AccessContext, query: &str) -> GraphError {
    match g.cypher(ctx, q(query)).await {
        Err(e) => e,
        Ok(r) => panic!("{query}: expected an error, got {r:?}"),
    }
}

// ---- tests ----------------------------------------------------------------------

#[tokio::test]
async fn match_return_and_shapes() {
    let e = env().await;
    let g = &e.graph;
    let ctx = admin("t1");
    assert_eq!(g.cypher_engine(), "portable");
    let r = cy(
        g,
        &ctx,
        q("MATCH (n:Person) RETURN n.name AS name, n.age ORDER BY name"),
    )
    .await;
    assert_eq!(r.columns, ["name", "n.age"]);
    assert_eq!(
        r.rows,
        [
            [json!("Ann"), json!(30)],
            [json!("Bob"), json!(25)],
            [json!("Cid"), json!(35)],
            [json!("Dee"), Json::Null]
        ]
    );
    assert!(!r.truncated);
    // Nodes and edges in ForgeBase's shapes; `__id` reads the ForgeBase id.
    let r = rows(g, &ctx, "MATCH (a {__id: 'a'})-[r:WORKS_AT]->(c) RETURN a, r, c.name").await;
    assert_eq!(
        r[0][0],
        json!({"id": "a", "label": "Person", "properties": {"name": "Ann", "age": 30, "city": "Paris", "tags": ["x", "y"], "active": true}})
    );
    assert_eq!(
        r[0][1],
        json!({"id": "a-acme", "type": "WORKS_AT", "source": "a", "target": "acme", "directed": true, "properties": {"since": 2020}})
    );
    assert_eq!(r[0][2], json!("Acme"));
    let r = rows(
        g,
        &ctx,
        "MATCH ()-[r:FRIEND]-() RETURN DISTINCT r.__id, r.__source, r.__target, type(r)",
    )
    .await;
    assert_eq!(r, [[json!("bd"), json!("b"), json!("d"), json!("FRIEND")]]);
    let r = rows(g, &ctx, "MATCH (n {__id: 'b'})-[r:FRIEND]-(m) RETURN r").await;
    assert_eq!(r[0][0]["directed"], json!(false));
    // A bare RETURN, literals of every type.
    let r = rows(
        g,
        &ctx,
        "RETURN 1 AS i, -2.5 AS f, 'x' AS s, true AS t, null AS n, [1, 'a', [false]] AS l",
    )
    .await;
    assert_eq!(
        r,
        [[
            json!(1),
            json!(-2.5),
            json!("x"),
            json!(true),
            Json::Null,
            json!([1, "a", [false]])
        ]]
    );
    // labels(), size(), toLower(), coalesce().
    let r = rows(
        g,
        &ctx,
        "MATCH (n {__id: 'a'}) RETURN labels(n), size(n.tags), size(n.name), toLower(n.name), toUpper(n.city), \
         coalesce(n.missing, n.name), coalesce(n.missing, 7)",
    )
    .await;
    assert_eq!(
        r,
        [[
            json!(["Person"]),
            json!(2),
            json!(3),
            json!("ann"),
            json!("PARIS"),
            json!("Ann"),
            json!(7)
        ]]
    );
}

#[tokio::test]
async fn directions_and_multiple_patterns() {
    let e = env().await;
    let g = &e.graph;
    let ctx = admin("t1");
    assert_eq!(
        col(g, &ctx, "MATCH ({__id: 'a'})-[:KNOWS]->(x) RETURN x.__id").await,
        ["b"]
    );
    assert_eq!(
        col(g, &ctx, "MATCH ({__id: 'a'})<-[:KNOWS]-(x) RETURN x.__id").await,
        ["d"]
    );
    assert_eq!(
        col(g, &ctx, "MATCH ({__id: 'a'})-[:KNOWS]-(x) RETURN x.__id").await,
        ["b", "d"]
    );
    // The short forms.
    assert_eq!(
        col(g, &ctx, "MATCH ({__id: 'c'})-->(x) RETURN x.__id").await,
        ["a", "d", "doc1"]
    );
    assert_eq!(
        col(g, &ctx, "MATCH ({__id: 'c'})<--(x) RETURN x.__id").await,
        ["a", "b"]
    );
    assert_eq!(
        col(g, &ctx, "MATCH ({__id: 'd'})--(x) RETURN x.__id").await,
        ["a", "b", "c"]
    );
    // An undirected edge follows its stored direction with `->` (as on AGE).
    assert_eq!(
        col(g, &ctx, "MATCH ({__id: 'b'})-[:FRIEND]->(x) RETURN x.__id").await,
        ["d"]
    );
    assert!(
        col(g, &ctx, "MATCH ({__id: 'd'})-[:FRIEND]->(x) RETURN x.__id")
            .await
            .is_empty()
    );
    // Type alternatives.
    assert_eq!(
        col(g, &ctx, "MATCH ({__id: 'a'})-[r:LIKES|WORKS_AT]->(x) RETURN x.__id").await,
        ["acme", "c"]
    );
    // Two patterns sharing a node, and a second MATCH.
    assert_eq!(
        rows(
            g,
            &ctx,
            "MATCH (p:Person)-[:WORKS_AT]->(c:Company), (q:Person)-[:WORKS_AT]->(c) \
             WHERE p.name < q.name RETURN p.name, q.name, c.name"
        )
        .await,
        [[json!("Ann"), json!("Bob"), json!("Acme")]]
    );
    assert_eq!(
        col(g, &ctx, "MATCH (a {__id: 'a'}) MATCH (a)-[:LIKES]->(x) RETURN x.name").await,
        ["Cid"]
    );
    // A chain; a relationship is matched once per clause (edge uniqueness).
    assert_eq!(
        col(g, &ctx, "MATCH ({__id: 'a'})-[:KNOWS]->(x)-[:KNOWS]->(y) RETURN y.__id").await,
        ["c"]
    );
    assert_eq!(
        rows(g, &ctx, "MATCH (x {__id: 'b'})-[r]-(y)-[s]-(x) RETURN count(*)").await,
        [[json!(0)]],
        "r and s must be different edges"
    );
    // A node may repeat (a -> b -> ... -> a needs 4 hops).
    assert_eq!(
        col(
            g,
            &ctx,
            "MATCH (a {__id: 'a'})-[:KNOWS]->()-[:KNOWS]->()-[:KNOWS]->()-[:KNOWS]->(a) RETURN a.__id"
        )
        .await,
        ["a"]
    );
    // Cartesian product of two disconnected patterns.
    assert_eq!(
        rows(g, &ctx, "MATCH (c:Company), (d:Doc) RETURN count(*)").await,
        [[json!(2)]]
    );
}

#[tokio::test]
async fn where_operators_and_parameters() {
    let e = env().await;
    let g = &e.graph;
    let ctx = admin("t1");
    let names = |w: &str| format!("MATCH (n:Person) WHERE {w} RETURN n.name");
    for (w, want) in [
        ("n.age > 26", vec!["Ann", "Cid"]),
        ("n.age >= 30 AND n.city = 'Paris'", vec!["Ann", "Cid"]),
        ("n.age < 30 OR n.city = 'Rome'", vec!["Bob", "Dee"]),
        ("NOT n.city = 'Paris'", vec!["Bob", "Dee"]),
        ("n.age <> 30", vec!["Bob", "Cid"]),
        ("n.age IS NULL", vec!["Dee"]),
        ("n.age IS NOT NULL", vec!["Ann", "Bob", "Cid"]),
        ("n.name IN ['Ann', 'Dee', 'Nobody']", vec!["Ann", "Dee"]),
        ("n.name STARTS WITH 'B'", vec!["Bob"]),
        ("n.name ENDS WITH 'e'", vec!["Dee"]),
        ("n.city CONTAINS 'ar'", vec!["Ann", "Cid"]),
        ("'x' IN n.tags", vec!["Ann"]),
        ("n.active = true", vec!["Ann"]),
        ("n.active", vec!["Ann"]),
        ("NOT n.active", vec!["Bob"]),
        ("n.score >= 2", vec!["Dee"]),
        ("n.score = 1.5", vec!["Cid"]),
        // Different types: `=` is false, `<>` true, ordering null.
        ("n.age = '30'", vec![]),
        ("n.name > 1", vec![]),
        ("n.__id = 'c'", vec!["Cid"]),
        ("n.city = n.city AND n.age = n.age", vec!["Ann", "Bob", "Cid"]),
    ] {
        let mut got = col(g, &ctx, &names(w)).await;
        got.sort();
        assert_eq!(got, want, "WHERE {w}");
    }
    // Parameters: scalars, lists, numbers.
    let r = cy(
        g,
        &ctx,
        qp(
            "MATCH (n:Person) WHERE n.name IN $names AND n.age >= $min RETURN n.name ORDER BY n.name",
            json!({"names": ["Ann", "Bob", "Cid"], "min": 26}),
        ),
    )
    .await;
    assert_eq!(r.rows, [[json!("Ann")], [json!("Cid")]]);
    let r = cy(
        g,
        &ctx,
        qp("MATCH (n {name: $name}) RETURN n.__id", json!({"name": "Bob"})),
    )
    .await;
    assert_eq!(r.rows, [[json!("b")]]);
    let r = cy(
        g,
        &ctx,
        qp("RETURN $x AS x, $m AS m", json!({"x": [1, 2], "m": {"k": "v"}})),
    )
    .await;
    assert_eq!(r.rows, [[json!([1, 2]), json!({"k": "v"})]]);
    let e = g
        .cypher(&ctx, q("MATCH (n) WHERE n.name = $nope RETURN n"))
        .await
        .unwrap_err();
    assert_eq!(code(&e), "cypher_invalid", "{e}");
    // Property maps on relationships.
    assert_eq!(
        col(g, &ctx, "MATCH (p)-[:WORKS_AT {since: 2021}]->() RETURN p.name").await,
        ["Bob"]
    );
}

#[tokio::test]
async fn aggregation_distinct_order_skip_limit() {
    let e = env().await;
    let g = &e.graph;
    let ctx = admin("t1");
    assert_eq!(rows(g, &ctx, "MATCH (n:Person) RETURN count(*)").await, [[json!(4)]]);
    assert_eq!(
        rows(
            g,
            &ctx,
            "MATCH (n:Person) RETURN count(n.age), min(n.age), max(n.age), sum(n.age), avg(n.age), count(DISTINCT n.city)"
        )
        .await,
        [[json!(3), json!(25), json!(35), json!(90), json!(30.0), json!(3)]]
    );
    assert_eq!(
        rows(
            g,
            &ctx,
            "MATCH (n:Person) WHERE n.name = 'Nobody' RETURN count(*), sum(n.age), avg(n.age), collect(n.name)"
        )
        .await,
        [[json!(0), json!(0), Json::Null, json!([])]]
    );
    // Implicit grouping.
    assert_eq!(
        rows(
            g,
            &ctx,
            "MATCH (p:Person) RETURN p.city AS city, count(*) AS n ORDER BY n DESC, city"
        )
        .await,
        [
            [json!("Paris"), json!(2)],
            [json!("Berlin"), json!(1)],
            [json!("Rome"), json!(1)]
        ]
    );
    assert_eq!(
        rows(
            g,
            &ctx,
            "MATCH (p:Person)-[:WORKS_AT]->(c) RETURN c.name, count(p), collect(p.name)"
        )
        .await
        .iter()
        .map(|r| {
            let mut names: Vec<String> = r[2]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_owned())
                .collect();
            names.sort();
            (r[0].clone(), r[1].clone(), names)
        })
        .collect::<Vec<_>>(),
        [(json!("Acme"), json!(2), vec!["Ann".to_owned(), "Bob".to_owned()])]
    );
    // Grouping by node.
    let r = rows(
        g,
        &ctx,
        "MATCH (p:Person)-[:KNOWS]->(q) RETURN p, count(q) ORDER BY p.name",
    )
    .await;
    assert_eq!(r.len(), 4);
    assert_eq!(r[0][0]["id"], json!("a"));
    // collect() skips nulls; DISTINCT.
    let mut ages: Vec<i64> = rows(g, &ctx, "MATCH (n:Person) RETURN collect(n.age)").await[0][0]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_i64().unwrap())
        .collect();
    ages.sort_unstable();
    assert_eq!(ages, [25, 30, 35]);
    assert_eq!(
        rows(g, &ctx, "MATCH (n:Person) RETURN DISTINCT n.city AS city ORDER BY city").await,
        [[json!("Berlin")], [json!("Paris")], [json!("Rome")]]
    );
    // ORDER BY: nulls last ascending, first descending; SKIP and LIMIT.
    assert_eq!(
        col(g, &ctx, "MATCH (n:Person) RETURN n.age ORDER BY n.age").await.len(),
        4
    );
    assert_eq!(
        rows(g, &ctx, "MATCH (n:Person) RETURN n.name ORDER BY n.age").await,
        [[json!("Bob")], [json!("Ann")], [json!("Cid")], [json!("Dee")]]
    );
    assert_eq!(
        rows(g, &ctx, "MATCH (n:Person) RETURN n.name ORDER BY n.age DESC").await,
        [[json!("Dee")], [json!("Cid")], [json!("Ann")], [json!("Bob")]]
    );
    assert_eq!(
        rows(
            g,
            &ctx,
            "MATCH (n:Person) RETURN n.name AS name ORDER BY name SKIP 1 LIMIT 2"
        )
        .await,
        [[json!("Bob")], [json!("Cid")]]
    );
    let r = cy(
        g,
        &ctx,
        qp(
            "MATCH (n:Person) RETURN n.name ORDER BY n.name LIMIT $l",
            json!({"l": 1}),
        ),
    )
    .await;
    assert_eq!((r.rows.len(), r.truncated), (1, false));
    // The request's limit truncates.
    let r = cy(
        g,
        &ctx,
        CypherQuery {
            limit: Some(2),
            ..q("MATCH (n:Person) RETURN n.name ORDER BY n.name")
        },
    )
    .await;
    assert_eq!((r.rows.len(), r.truncated), (2, true));
    // After an aggregation, ORDER BY may only use returned columns.
    let e = err(g, &ctx, "MATCH (n:Person) RETURN count(*) AS c ORDER BY n.name").await;
    assert_eq!(code(&e), "cypher_invalid", "{e}");
    let e = err(g, &ctx, "MATCH (n:Person) WHERE count(n) > 1 RETURN n").await;
    assert_eq!(code(&e), "cypher_invalid", "{e}");
}

#[tokio::test]
async fn variable_length_paths() {
    let e = env().await;
    let g = &e.graph;
    let ctx = admin("t1");
    assert_eq!(
        col(g, &ctx, "MATCH ({__id: 'a'})-[:KNOWS*1..2]->(x) RETURN x.__id").await,
        ["b", "c"]
    );
    assert_eq!(
        col(g, &ctx, "MATCH ({__id: 'a'})-[:KNOWS*0..1]->(x) RETURN x.__id").await,
        ["a", "b"]
    );
    assert_eq!(
        col(g, &ctx, "MATCH ({__id: 'a'})-[:KNOWS*3]->(x) RETURN x.__id").await,
        ["d"]
    );
    // A walk never reuses an edge: around the cycle once, back to a.
    assert_eq!(
        col(g, &ctx, "MATCH ({__id: 'a'})-[:KNOWS*1..10]->(x) RETURN x.__id").await,
        ["a", "b", "c", "d"]
    );
    // Walked from the constrained end (right to left).
    assert_eq!(
        col(g, &ctx, "MATCH (x)-[:KNOWS*1..3]->({__id: 'd'}) RETURN x.__id").await,
        ["a", "b", "c"]
    );
    // One row per path: a reaches c through b (KNOWS KNOWS) and directly (LIKES).
    assert_eq!(
        rows(
            g,
            &ctx,
            "MATCH ({__id: 'a'})-[:KNOWS|LIKES*1..2]->(x {__id: 'c'}) RETURN count(*)"
        )
        .await,
        [[json!(2)]]
    );
    // The relationship list, in pattern order, whichever end the walk started at.
    let r = rows(
        g,
        &ctx,
        "MATCH ({__id: 'a'})-[r:KNOWS*2]->(x) RETURN r, size(r), x.__id",
    )
    .await;
    let ids: Vec<&str> = r[0][0]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap())
        .collect();
    assert_eq!((ids, &r[0][1], &r[0][2]), (vec!["ab", "bc"], &json!(2), &json!("c")));
    let r = rows(g, &ctx, "MATCH (x)-[r:KNOWS*2]->({__id: 'c'}) RETURN r, x.__id").await;
    let ids: Vec<&str> = r[0][0]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap())
        .collect();
    assert_eq!((ids, &r[0][1]), (vec!["ab", "bc"], &json!("a")));
    // Either direction; property maps on each hop.
    assert_eq!(
        col(g, &ctx, "MATCH ({__id: 'a'})-[:KNOWS*2]-(x) RETURN DISTINCT x.__id").await,
        ["c"]
    );
    assert_eq!(
        col(g, &ctx, "MATCH ({__id: 'b'})-[:KNOWS*1..3 {w: 2}]->(x) RETURN x.__id").await,
        ["c"]
    );
    // A walk and a fixed edge of the same clause are different edges.
    assert_eq!(
        rows(
            g,
            &ctx,
            "MATCH (a {__id: 'a'})-[e:KNOWS]->(b), (a)-[:KNOWS*1..4]->(b) RETURN count(*)"
        )
        .await,
        [[json!(0)]]
    );
    // Two walks of one clause share no edge: from b (reached over ab), 1–2
    // hops either way without ab: bc (to c) and bc + cd (to d).
    assert_eq!(
        rows(
            g,
            &ctx,
            "MATCH ({__id: 'a'})-[:KNOWS*1]->(m), (m)-[:KNOWS*1..2]-(y) RETURN count(*)"
        )
        .await,
        [[json!(2)]]
    );
}

#[tokio::test]
async fn caps_unsupported_and_errors() {
    let e = env().await;
    let g = &e.graph;
    let ctx = admin("t1");
    for (query, want) in [
        ("MATCH (n) WITH n RETURN n", "cypher_unsupported"),
        ("OPTIONAL MATCH (n) RETURN n", "cypher_unsupported"),
        ("MATCH (a)-[*]->(b) RETURN b", "cypher_unsupported"),
        ("MATCH p = (a)-->(b) RETURN p", "cypher_unsupported"),
        ("MATCH (n) RETURN n.age + 1", "cypher_unsupported"),
        ("MATCH (n) RETURN id(n)", "cypher_unsupported"),
        ("MATCH (n) RETURN nosuchfn(n)", "cypher_unsupported"),
        ("MATCH (a)-[*1..11]->(b) RETURN b", "cypher_too_complex"),
        ("MATCH (n) SET n.x = 1 RETURN n", "cypher_read_only"),
        ("CREATE (n) RETURN n", "cypher_read_only"),
        ("MATCH (n) DETACH DELETE n", "cypher_read_only"),
        ("CALL db.labels()", "cypher_read_only"),
        ("MATCH (n RETURN n", "cypher_invalid"),
        ("MATCH (n) RETURN m", "cypher_invalid"),
        ("MATCH (n) RETURN n;", "cypher_invalid"),
        ("MATCH (n)-[r]->(m) RETURN n.x.y.z, type(n)", "cypher_invalid"),
    ] {
        let e = err(g, &ctx, query).await;
        assert_eq!(code(&e), want, "{query}: {e}");
        if want == "cypher_unsupported" || want == "cypher_invalid" {
            assert!(e.to_string().contains("line 1, column"), "{query}: {e}");
        }
    }
    // The construct and its position are named.
    let e = err(g, &ctx, "MATCH (n)\nWITH n RETURN n").await;
    assert!(
        e.to_string().contains("WITH") && e.to_string().contains("line 2, column 1"),
        "{e}"
    );
    // Request limits are the AGE engine's.
    for bad in [
        CypherQuery {
            limit: Some(1001),
            ..q("RETURN 1 AS x")
        },
        CypherQuery {
            timeout_ms: Some(30_001),
            ..q("RETURN 1 AS x")
        },
        CypherQuery {
            params: Some(json!([1])),
            ..q("RETURN 1 AS x")
        },
        q(""),
    ] {
        assert!(matches!(g.cypher(&ctx, bad).await, Err(GraphError::Invalid { .. })));
    }
}

#[tokio::test]
async fn walk_and_time_caps_are_enforced() {
    let e = env().await;
    // A dense graph in the shared tenant: millions of paths of 1..6 hops.
    let env2 = common::env().await;
    common::seed_random(&env2, 300, 3000).await;
    let g2 = &env2.graph;
    let root = AccessContext::admin("root");
    let e2 = g2
        .cypher(&root, q("MATCH (a)-[*1..6]->(b) RETURN count(*)"))
        .await
        .unwrap_err();
    assert_eq!(code(&e2), "cypher_too_complex", "{e2}");
    // A constrained start stays under the cap.
    let r = g2
        .cypher(&root, q("MATCH (a {__id: 'n1'})-[*1..2]->(b) RETURN count(*)"))
        .await
        .unwrap();
    assert!(r.rows[0][0].as_i64().unwrap() > 0);
    // A cartesian product runs into timeout_ms.
    let slow = CypherQuery {
        timeout_ms: Some(50),
        ..q("MATCH (a), (b), (c) RETURN count(*)")
    };
    let err = g2.cypher(&root, slow).await.unwrap_err();
    assert_eq!(code(&err), "cypher_timeout", "{err}");
    drop(e);
}

#[tokio::test]
async fn tenants_rules_and_permissions() {
    let e = env().await;
    let g = &e.graph;
    // Tenant isolation: each tenant sees its own graph only.
    assert_eq!(rows(g, &admin("t1"), "MATCH (n) RETURN count(n)").await, [[json!(8)]]);
    assert_eq!(col(g, &admin("t2"), "MATCH (n) RETURN n.name").await, ["Zed"]);
    assert!(
        col(g, &admin("t1"), "MATCH (n {name: 'Zed'}) RETURN n.__id")
            .await
            .is_empty()
    );
    assert!(
        col(g, &admin("t2"), "MATCH ({__id: 'a'})-[*1..3]->(x) RETURN x.__id")
            .await
            .is_empty()
    );
    // Permission: engine:graph_cypher, not implied by graph_read.
    assert!(matches!(
        g.cypher(&user("reader", "t1"), q("MATCH (n) RETURN n")).await,
        Err(GraphError::Forbidden(_))
    ));
    assert!(matches!(
        g.cypher(&AccessContext::anonymous(), q("MATCH (n) RETURN n")).await,
        Err(GraphError::Unauthenticated | GraphError::Forbidden(_) | GraphError::Invalid { .. })
    ));
    // Rules: Secret hidden, Doc filtered by team, HIDDEN and low-star RATED
    // edges hidden, as in the REST API.
    let ruled = user("ruled", "t1");
    assert_eq!(
        col(g, &ruled, "MATCH (n) RETURN n.__id").await,
        ["a", "acme", "b", "c", "d", "doc1"]
    );
    assert!(col(g, &ruled, "MATCH (n:Secret) RETURN n.__id").await.is_empty());
    assert!(col(g, &ruled, "MATCH ()-[r:HIDDEN]->() RETURN r.__id").await.is_empty());
    // OWNS is allowed, but its target is hidden: the edge is too.
    assert!(col(g, &ruled, "MATCH ()-[r:OWNS]->() RETURN r.__id").await.is_empty());
    assert_eq!(col(g, &ruled, "MATCH ()-[r:RATED]->() RETURN r.__id").await, ["a-doc1"]);
    // A walk does not cross a hidden edge or enter a hidden node.
    assert_eq!(
        col(g, &ruled, "MATCH ({__id: 'c'})-[*1..2]->(x) RETURN DISTINCT x.__id").await,
        ["a", "d"],
        "c -HIDDEN-> a is hidden; c -KNOWS-> d -KNOWS-> a is not; c -RATED(2)-> doc1 is hidden"
    );
    let cy_user = user("cy", "t1");
    assert_eq!(
        col(g, &cy_user, "MATCH ({__id: 'c'})-[*1..1]->(x) RETURN x.__id").await,
        ["a", "d", "doc1"]
    );
}

#[tokio::test]
async fn http_route_answers_on_the_portable_engine() {
    let env = common::env().await;
    let admin = env.admin();
    admin.node("a", "Person").await;
    admin.node("b", "Person").await;
    admin.edge("ab", "KNOWS", "a", "b").await;
    let (s, b) = admin
        .post(
            "/cypher",
            json!({"query": "MATCH (x)-[:KNOWS]->(y) RETURN x.__id AS x, y.__id AS y"}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["engine"], "portable");
    assert_eq!(b["columns"], json!(["x", "y"]));
    assert_eq!(b["rows"], json!([["a", "b"]]));
    assert_eq!(b["truncated"], json!(false));
    let (s, b) = admin
        .post("/cypher", json!({"query": "MATCH (n) WITH n RETURN n"}))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{b}");
    assert_eq!(b["error"]["code"], "cypher_unsupported");
    // The derived-graph operations exist for AGE only.
    let (s, b) = admin.post("/cypher/drop", json!({})).await;
    assert_eq!(
        (s, b["error"]["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("cypher_unsupported"))
    );
}
