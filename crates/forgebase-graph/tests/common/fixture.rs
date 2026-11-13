//! The openCypher test graph shared by the portable suite and the
//! differential test against Apache AGE (ADR-0034).
#![allow(dead_code, clippy::unwrap_used)]

use forgebase_data::AccessContext;
use forgebase_graph::{GraphService, NewEdge, NewNode};
use serde_json::{Value as Json, json};

fn admin(tenant: &str) -> AccessContext {
    AccessContext::admin("root").with_tenant(tenant)
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
