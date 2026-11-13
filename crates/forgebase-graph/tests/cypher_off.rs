//! openCypher without Apache AGE (ADR-0029 §1.5, FB-163; ADR-0034): SQLite,
//! Windows and PostgreSQL servers without AGE answer `POST /graph/cypher`
//! on the portable engine (`engine: "portable"`); the derived-graph
//! operations (`drop`, materialisation) answer `400 cypher_unsupported`;
//! the REST graph API works as before and writes do not touch any derived
//! graph. Runs on whichever backend `FORGEBASE_TEST_BACKEND` selects (the
//! PostgreSQL test database has no extensions set up, so AGE is not in use
//! there either).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::http::StatusCode;
use forgebase_graph::{CypherQuery, GraphError};
use serde_json::json;

#[tokio::test]
async fn cypher_answers_on_the_portable_engine_without_age() {
    let env = common::env().await;
    let ctx = forgebase_data::AccessContext::admin("root");
    let q = CypherQuery {
        query: "MATCH (n) RETURN n".into(),
        params: None,
        limit: None,
        timeout_ms: None,
    };
    let r = env.graph.cypher(&ctx, q).await.unwrap();
    assert_eq!((r.engine.as_str(), r.rows.len()), ("portable", 0));
    assert_eq!(env.graph.cypher_engine(), "portable");
    assert!(matches!(
        env.graph.drop_cypher(&ctx).await,
        Err(GraphError::CypherUnsupported)
    ));
    assert!(matches!(
        env.graph.materialise("").await,
        Err(GraphError::CypherUnsupported)
    ));
    assert!(!env.graph.mark_cypher_stale("").await.unwrap());

    let admin = env.admin();
    admin.node("a", "Person").await;
    admin.node("b", "Person").await;
    admin.edge("ab", "KNOWS", "a", "b").await;
    let (s, b) = admin
        .post("/cypher", json!({"query": "MATCH (n) RETURN n.__id AS id ORDER BY id"}))
        .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(
        (b["engine"].as_str(), &b["rows"]),
        (Some("portable"), &json!([["a"], ["b"]]))
    );
    let (s, b) = admin.post("/cypher/drop", json!({})).await;
    assert_eq!(
        (s, b["error"]["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("cypher_unsupported"))
    );
    // The REST graph is unchanged, and stats carry no openCypher state.
    let (s, stats) = admin.get("/stats").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!((stats["nodes"].as_i64(), stats["edges"].as_i64()), (Some(2), Some(1)));
    assert!(stats.get("cypher").is_none(), "{stats}");
}
