//! Differential test of the two openCypher engines (ADR-0034): on
//! PostgreSQL with Apache AGE, every query of the portable subset returns
//! the same columns and rows on AGE (the derived graph) and on the
//! portable engine (SQL over the canonical tables), for an administrator
//! and for a caller under label and edge-type rules.
//!
//! Needs `FORGEBASE_TEST_BACKEND=postgres` and an engine with AGE
//! (`FORGEBASE_TEST_PG_ENGINE`, e.g. FB-160's r1 archive); skips otherwise.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]

mod common;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use forgebase_data::schema::Collection;
use forgebase_data::{
    AccessContext, Action, Authorizer, DataService, Decision, Predicate, Principal, ResourceKind, ResourceRef,
};
use forgebase_db::testing::{TestDb, selected_backend, test_backend};
use forgebase_db::{Backend, BackendKind, Database, ExtensionSetup, Feature, Statement, TxMode};
use forgebase_graph::{CypherQuery, GraphError, GraphService};
use serde_json::{Value as Json, json};

/// As `cypher_portable.rs`: `ruled` runs openCypher under rules (`Secret`
/// and `HIDDEN` hidden, `Doc` needs `team = 'a'`, `RATED` needs 3 stars).
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
            (Some("ruled"), Action::GraphRead | Action::GraphCypher) => Decision::Allow,
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

fn age_plugin() -> bool {
    let Some(engine) = ["FORGEBASE_TEST_PG_ENGINE", "FORGEBASE_PG_ENGINE_DIR"]
        .iter()
        .find_map(|k| std::env::var_os(k).filter(|v| !v.is_empty()))
        .map(PathBuf::from)
    else {
        return false;
    };
    ["so", "dylib"]
        .iter()
        .any(|ext| engine.join("lib/plugins").join(format!("age.{ext}")).exists())
}

/// A database with AGE in use, or `None` (skip).
async fn env() -> Option<(TestDb, Database, GraphService)> {
    if selected_backend() != BackendKind::Postgres || !age_plugin() {
        eprintln!("skipped: needs FORGEBASE_TEST_BACKEND=postgres and an engine with Apache AGE");
        return None;
    }
    let test = test_backend().await;
    static PRELOAD: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    PRELOAD
        .get_or_init(|| async {
            let mut tx = test.database().begin(TxMode::Normal).await.unwrap();
            tx.execute(&Statement::raw(
                "ALTER ROLE CURRENT_USER SET session_preload_libraries = '$libdir/plugins/age'",
            ))
            .await
            .unwrap();
            tx.commit().await.unwrap();
        })
        .await;
    let db = Database::open(test.options()).await.unwrap();
    db.setup_extensions(&ExtensionSetup::default()).await.unwrap();
    assert!(db.feature_active(Feature::Graph), "AGE is installed but not usable");
    let data = DataService::new(Arc::new(db.clone()), Arc::new(Policy)).await.unwrap();
    Some((test, db, GraphService::new(data)))
}

fn q(query: &str, params: Option<Json>) -> CypherQuery {
    CypherQuery {
        query: query.into(),
        params,
        limit: None,
        timeout_ms: None,
    }
}

/// Materialises the tenant's AGE graph (the first query answers 409).
async fn materialise(g: &GraphService, ctx: &AccessContext) {
    match g.cypher(ctx, q("RETURN 1 AS one", None)).await {
        Err(GraphError::Materialising { .. }) => {}
        other => panic!("expected graph_materialising, got {other:?}"),
    }
    for _ in 0..600 {
        let st = g.stats(ctx).await.unwrap().cypher.expect("AGE is on");
        match st.state.as_str() {
            "ready" => return,
            "failed" => panic!("materialisation failed: {:?}", st.error),
            _ => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    }
    panic!("the graph did not become ready");
}

/// Numbers as floats (AGE's `30.0::float` and PostgreSQL's `30` are the
/// same value), recursively.
fn norm(v: &Json) -> Json {
    match v {
        Json::Number(n) => json!(n.as_f64()),
        Json::Array(a) => Json::Array(a.iter().map(norm).collect()),
        Json::Object(m) => Json::Object(m.iter().map(|(k, v)| (k.clone(), norm(v))).collect()),
        other => other.clone(),
    }
}

const QUERIES: &[&str] = &[
    "MATCH (n:Person) RETURN n.name AS name, n.age AS age ORDER BY name",
    "MATCH (a {__id: 'a'})-[r:WORKS_AT]->(c) RETURN a, r, c.name AS c",
    "MATCH ({__id: 'a'})-[:KNOWS]-(x) RETURN x.__id AS id ORDER BY id",
    "MATCH ({__id: 'c'})-->(x) RETURN x.__id AS id ORDER BY id",
    "MATCH ({__id: 'c'})<--(x) RETURN x.__id AS id ORDER BY id",
    "MATCH ({__id: 'b'})-[:FRIEND]->(x) RETURN x.__id AS id",
    "MATCH ({__id: 'd'})-[:FRIEND]->(x) RETURN x.__id AS id",
    "MATCH ({__id: 'a'})-[r]->(x) RETURN x.__id AS id, type(r) AS t ORDER BY id",
    "MATCH (p:Person)-[:WORKS_AT]->(c:Company), (q:Person)-[:WORKS_AT]->(c) WHERE p.name < q.name RETURN p.name AS p, q.name AS q",
    "MATCH (a {__id: 'a'}) MATCH (a)-[:LIKES]->(x) RETURN x.name AS n",
    "MATCH ({__id: 'a'})-[:KNOWS]->(x)-[:KNOWS]->(y) RETURN y.__id AS id",
    "MATCH (x {__id: 'b'})-[r]-(y)-[s]-(x) RETURN count(*) AS c",
    "MATCH (n:Person) WHERE n.age > 26 RETURN n.name AS n ORDER BY n",
    "MATCH (n:Person) WHERE n.age >= 30 AND n.city = 'Paris' RETURN n.name AS n ORDER BY n",
    "MATCH (n:Person) WHERE NOT n.city = 'Paris' RETURN n.name AS n ORDER BY n",
    "MATCH (n:Person) WHERE n.age IS NULL RETURN n.name AS n",
    "MATCH (n:Person) WHERE n.age IS NOT NULL RETURN n.name AS n ORDER BY n",
    "MATCH (n:Person) WHERE n.name IN ['Ann', 'Dee', 'Nobody'] RETURN n.name AS n ORDER BY n",
    "MATCH (n:Person) WHERE n.name STARTS WITH 'B' OR n.name ENDS WITH 'e' OR n.city CONTAINS 'om' RETURN n.name AS n ORDER BY n",
    "MATCH (n:Person) WHERE n.score >= 2 RETURN n.name AS n",
    "MATCH (n:Person) WHERE n.active = true RETURN n.name AS n",
    "MATCH (n:Person) RETURN count(*) AS c, count(n.age) AS a, min(n.age) AS mi, max(n.age) AS ma, sum(n.age) AS s, avg(n.age) AS av",
    "MATCH (p:Person) RETURN p.city AS city, count(*) AS n ORDER BY n DESC, city",
    "MATCH (n:Person) RETURN DISTINCT n.city AS city ORDER BY city",
    "MATCH (n:Person) RETURN n.name AS name ORDER BY name SKIP 1 LIMIT 2",
    "MATCH (n:Person) RETURN n.name AS name ORDER BY n.age DESC",
    "MATCH (p:Person)-[:KNOWS]->(q) RETURN p.name AS p, count(q) AS c ORDER BY p",
    "MATCH ({__id: 'a'})-[:KNOWS*1..2]->(x) RETURN x.__id AS id ORDER BY id",
    "MATCH ({__id: 'a'})-[:KNOWS*0..1]->(x) RETURN x.__id AS id ORDER BY id",
    "MATCH ({__id: 'a'})-[:KNOWS*1..10]->(x) RETURN x.__id AS id ORDER BY id",
    "MATCH (x)-[:KNOWS*1..3]->({__id: 'd'}) RETURN x.__id AS id ORDER BY id",
    "MATCH ({__id: 'a'})-[*1..2]->(x {__id: 'c'}) RETURN count(*) AS c",
    "MATCH ({__id: 'a'})-[r:KNOWS*2]->(x) RETURN r, x.__id AS id",
    "MATCH (x)-[r:KNOWS*2]->({__id: 'c'}) RETURN r, x.__id AS id",
    "MATCH ({__id: 'a'})-[:KNOWS*2]-(x) RETURN DISTINCT x.__id AS id ORDER BY id",
    "MATCH ({__id: 'b'})-[:KNOWS*1..3 {w: 2}]->(x) RETURN x.__id AS id",
    "MATCH ({__id: 'a'})-[:KNOWS*1]->(m), (m)-[:KNOWS*1..2]-(y) RETURN count(*) AS c",
    "MATCH (a {__id: 'a'})-[e:KNOWS]->(b), (a)-[:KNOWS*1..4]->(b) RETURN count(*) AS c",
    "MATCH (n {__id: 'a'}) RETURN labels(n) AS l, size(n.tags) AS s",
    "MATCH ()-[r:RATED]->() RETURN r.__id AS id ORDER BY id",
    "MATCH ()-[r]->() RETURN r.__id AS id ORDER BY id",
    "MATCH (n) RETURN n.__id AS id ORDER BY id",
    "MATCH (n) RETURN count(n) AS c",
    "MATCH ({__id: 'c'})-[*1..2]->(x) RETURN DISTINCT x.__id AS id ORDER BY id",
    "MATCH (c:Company), (d:Doc) RETURN count(*) AS c",
    "RETURN 1 AS i, 'x' AS s, true AS t, null AS n, [1, 'a'] AS l",
];

#[tokio::test]
async fn portable_and_age_return_the_same_rows() {
    let Some((_test, _db, g)) = env().await else { return };
    common::fixture::fixture(&g).await;
    let admin = AccessContext::admin("root").with_tenant("t1");
    let ruled = AccessContext::user("users", "ruled").with_tenant("t1");
    materialise(&g, &admin).await;
    let mut compared = 0;
    let mut differ = Vec::new();
    for ctx in [&admin, &ruled] {
        for query in QUERIES {
            let who = user_of(ctx).unwrap_or("admin");
            let (age, portable) = match (
                g.cypher(ctx, q(query, None)).await,
                g.cypher_portable(ctx, q(query, None)).await,
            ) {
                (Ok(a), Ok(p)) => (a, p),
                (a, p) => {
                    differ.push(format!("{who}: {query}\n  age: {a:?}\n  portable: {p:?}"));
                    continue;
                }
            };
            assert_eq!((age.engine.as_str(), portable.engine.as_str()), ("age", "portable"));
            let mut a: Vec<Json> = age.rows.iter().map(|r| norm(&json!(r))).collect();
            let mut p: Vec<Json> = portable.rows.iter().map(|r| norm(&json!(r))).collect();
            if !query.contains("ORDER BY") {
                a.sort_by_key(Json::to_string);
                p.sort_by_key(Json::to_string);
            }
            if age.columns != portable.columns || a != p {
                differ.push(format!(
                    "{who}: {query}\n  age:      {:?} {a:?}\n  portable: {:?} {p:?}",
                    age.columns, portable.columns
                ));
            }
            compared += 1;
        }
    }
    assert!(differ.is_empty(), "the engines differ:\n{}", differ.join("\n"));
    // Parameters.
    let params = json!({"names": ["Ann", "Cid"], "min": 26, "name": "Bob"});
    for query in [
        "MATCH (n:Person) WHERE n.name IN $names AND n.age >= $min RETURN n.name AS n ORDER BY n",
        "MATCH (n:Person {name: $name}) RETURN n.__id AS id",
    ] {
        let age = g.cypher(&admin, q(query, Some(params.clone()))).await.unwrap();
        let portable = g.cypher_portable(&admin, q(query, Some(params.clone()))).await.unwrap();
        assert_eq!(norm(&json!(age.rows)), norm(&json!(portable.rows)), "{query}");
        compared += 1;
    }
    eprintln!("{compared} queries agree");
    // Writes after materialisation reach both engines at once.
    g.create_node(
        &admin,
        forgebase_graph::NewNode {
            id: Some("e".into()),
            label: Some("Person".into()),
            properties: Some(json!({"name": "Eve", "age": 41})),
        },
    )
    .await
    .unwrap();
    let query = "MATCH (n:Person) WHERE n.age > 40 RETURN n.name AS n";
    let age = g.cypher(&admin, q(query, None)).await.unwrap();
    let portable = g.cypher_portable(&admin, q(query, None)).await.unwrap();
    assert_eq!(age.rows, portable.rows);
    assert_eq!(portable.rows, [[json!("Eve")]]);
    assert!(compared >= QUERIES.len() * 2);
}
