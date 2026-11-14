//! Vector search, the property graph with openCypher, and the SQL editor.
//!
//!     FORGEBASE_URL=http://localhost:8090 FORGEBASE_API_KEY=fb_... cargo run --example vector_graph_sql
//!
//! The key needs the `engine:vector`, `engine:graph_*`, `engine:graph_cypher` and SQL permissions
//! (an administrator's key has them).

use forgebase_sdk::ForgeBase;
use forgebase_sdk::generated::Metric;
use forgebase_sdk::graph::{CypherOptions, NewEdge, NewNode};
use forgebase_sdk::sql::SqlOptions;
use forgebase_sdk::vector::{FieldSpec, HybridOptions, NewVectorCollection, SearchOptions, UpsertItem};
use serde_json::json;

#[tokio::main]
async fn main() -> forgebase_sdk::Result<()> {
    let url = std::env::var("FORGEBASE_URL").unwrap_or_else(|_| "http://localhost:8090".into());
    let key = std::env::var("FORGEBASE_API_KEY").unwrap_or_default();
    let fb = ForgeBase::builder(url).api_key(key).build()?;

    // Which engine runs what on this server.
    for f in fb.system().capabilities().await?.features {
        println!("{:<12} {:<10} {}", f.feature, f.state, f.engine);
    }

    // Vector search: closest first, with a metadata filter, then keyword + vector fusion.
    let vector = fb.vector();
    vector
        .create(&NewVectorCollection::new("example_docs").field(FieldSpec::new("emb", 3, Metric::Cosine)))
        .await?;
    let docs = vector.collection("example_docs");
    docs.upsert(&[
        UpsertItem::new()
            .id("rust")
            .vector("emb", &[1.0, 0.0, 0.0])
            .metadata(json!({"year": 2015}))
            .text("systems programming language"),
        UpsertItem::new()
            .id("sql")
            .vector("emb", &[0.0, 0.0, 1.0])
            .metadata(json!({"year": 1974}))
            .text("query language for databases"),
    ])
    .await?;
    let hits = docs
        .search(
            &SearchOptions::new(&[1.0, 0.1, 0.0])
                .k(2)
                .filter("year > {:min}", Some(json!({"min": 2000}))),
        )
        .await?;
    for h in &hits.hits {
        println!("{} score {:.3}", h.id, h.score);
    }
    let fused = docs
        .hybrid(&HybridOptions::new("databases", &[0.0, 0.0, 1.0]).k(2))
        .await?;
    println!("hybrid best: {:?}", fused.hits.first().map(|h| &h.id));
    docs.drop_collection().await?;

    // Graph: nodes, an edge and an openCypher query. `engine` says whether AGE or the portable engine answered.
    let g = fb.graph();
    g.nodes()
        .create(&NewNode::new("Person", json!({"name": "Ada"})).id("example-ada"))
        .await?;
    g.nodes()
        .create(&NewNode::new("Person", json!({"name": "Bob"})).id("example-bob"))
        .await?;
    g.edges()
        .create(&NewEdge::new("KNOWS", "example-ada", "example-bob"))
        .await?;
    let r = g
        .cypher(
            "MATCH (a:Person)-[:KNOWS]->(b:Person) RETURN a.name AS who, b.name AS friend",
            &CypherOptions::default(),
        )
        .await?;
    println!("{:?} -> {:?} ({} engine)", r.columns, r.rows, r.engine);
    g.nodes().delete("example-ada").await?;
    g.nodes().delete("example-bob").await?;

    // SQL: parameters are positional.
    let rows = fb
        .sql()
        .query(
            "SELECT name FROM sqlite_master WHERE type = ? LIMIT 5",
            &[json!("table")],
            &SqlOptions::default(),
        )
        .await?;
    println!("{} table(s)", rows.len());
    Ok(())
}
