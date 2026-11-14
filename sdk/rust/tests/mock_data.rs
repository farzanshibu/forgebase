//! Mock tests for credits, vector, graph, SQL, indexes and documents. Responses of generated
//! models are built from the OpenAPI document (`common::sample`), so a model the document and
//! the SDK disagree on fails to decode here. Hermetic: no ForgeBase binary needed.

mod common;

use common::mock::{Mock, err, no_content, ok, serve, status};
use common::sample::{schema, with};
use forgebase_sdk::credits::{
    AmendInput, CreditInput, CreditUsageOptions, CreditWindow, LedgerOptions, LotsOptions, RateInput, SpendInput,
};
use forgebase_sdk::documents::GetDocumentOptions;
use forgebase_sdk::generated::{Direction, EntryKind, FilterMode, Interval, Metric, RateBasis};
use forgebase_sdk::graph::{
    CypherOptions, ListEdges, ListNodes, NeighborsOptions, NewEdge, NewNode, NodeChange, ShortestPathOptions,
    TraverseOptions,
};
use forgebase_sdk::indexes::{IndexKey, IndexKind, NewIndex};
use forgebase_sdk::sql::{SqlOptions, SqlRunAs, SqlStatement, rows_as_objects};
use forgebase_sdk::vector::{FieldSpec, HybridOptions, NewVectorCollection, RecallOptions, SearchOptions, UpsertItem};
use forgebase_sdk::{ErrorKind, WriteOptions};
use serde_json::{Value, json};

// ---- credits ----

#[tokio::test]
async fn credits_reads() {
    let m = Mock::start(vec![
        ok(with("Balance", json!({"balance": 40, "subject": "org:1"}))),
        ok(json!({"items": [schema("Lot")]})),
        ok(with("LedgerPage", json!({"next": 7}))),
        ok(json!({"items": [], "next": null})),
        ok(schema("LedgerEntry")),
        ok(schema("AuditReport")),
        ok(schema("UsageChargePage")),
        ok(json!({"items": [schema("Rate")]})),
        ok(with("Cursor", json!({"seq": 9}))),
        ok(schema("BalanceHistory")),
        ok(schema("ConsumptionStats")),
        ok(schema("CreditSummary")),
    ])
    .await;
    let c = m.client().credits();
    let b = c.balance(Some("org:1")).await.expect("balance");
    assert_eq!(b.balance, 40);
    assert_eq!(m.last().q("subject").as_deref(), Some("org:1"));

    let lots = c
        .lots(&LotsOptions {
            subject: None,
            all: true,
        })
        .await
        .expect("lots");
    assert_eq!(lots.len(), 1);
    assert_eq!(m.last().q("all").as_deref(), Some("true"));

    let page = c
        .ledger(&LedgerOptions {
            kind: Some(EntryKind::TopUp),
            after: Some(3),
            limit: Some(10),
            from: Some("2026-01-01T00:00:00Z".into()),
            ..LedgerOptions::default()
        })
        .await
        .expect("ledger");
    assert_eq!(page.next, Some(7));
    let q = m.last();
    assert_eq!(q.route(), "/api/v1/credits/ledger");
    assert_eq!(
        (q.q("kind").as_deref(), q.q("after").as_deref(), q.q("limit").as_deref()),
        (Some("top_up"), Some("3"), Some("10"))
    );
    assert_eq!(q.q("from").as_deref(), Some("2026-01-01T00:00:00Z"));

    // all_entries stops when `next` is null (the first scripted reply with a next was consumed above;
    // this one ends the walk at once).
    assert!(c.all_entries(&LedgerOptions::default()).await.expect("all").is_empty());
    c.entry("e 1").await.expect("entry");
    assert_eq!(m.last().path, "/api/v1/credits/ledger/e%201");
    c.audit(None).await.expect("audit");
    c.usage(&CreditUsageOptions {
        shortfall: true,
        after: Some(1),
        ..CreditUsageOptions::default()
    })
    .await
    .expect("usage");
    assert_eq!(m.last().q("shortfall").as_deref(), Some("true"));
    assert_eq!(c.rates().list().await.expect("rates").len(), 1);
    assert_eq!(c.usage_cursor().get().await.expect("cursor"), 9);

    let w = CreditWindow {
        interval: Some(Interval::Month),
        to: Some("2026-02-01T00:00:00Z".into()),
        ..CreditWindow::default()
    };
    c.stats().balance(&w).await.expect("balance history");
    assert_eq!(m.last().q("interval").as_deref(), Some("month"));
    c.stats()
        .consumption(&CreditWindow {
            all: true,
            ..CreditWindow::default()
        })
        .await
        .expect("consumption");
    assert_eq!(m.last().q("all").as_deref(), Some("true"));
    c.stats().summary(&w).await.expect("summary");
    assert!(m.last().q("interval").is_none(), "the summary has no buckets");
}

#[tokio::test]
async fn credits_all_entries_follows_next() {
    let entry = schema("LedgerEntry");
    let m = Mock::start(vec![
        ok(json!({"items": [entry.clone()], "next": 5})),
        ok(json!({"items": [entry.clone(), entry], "next": null})),
    ])
    .await;
    let all = m
        .client()
        .credits()
        .all_entries(&LedgerOptions::default())
        .await
        .expect("all");
    assert_eq!(all.len(), 3);
    let calls = m.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[1].q("after").as_deref(), Some("5"));
}

#[tokio::test]
async fn credits_writes_are_idempotent_and_shaped() {
    let posted = json!({"entry": schema("LedgerEntry"), "replayed": false});
    let m = Mock::start(vec![
        ok(posted.clone()),
        ok(posted.clone()),
        ok(posted.clone()),
        ok(posted),
        ok(schema("Reversal")),
        ok(schema("Rate")),
        no_content(),
        ok(with("Cursor", json!({"seq": 12}))),
        ok(schema("CreditsMaintenanceReport")),
    ])
    .await;
    let c = m.client().credits();
    let p = c
        .top_up(&CreditInput::new("org:1", 100, "k1").expires_at("2027-01-01T00:00:00Z"))
        .await
        .expect("top up");
    assert!(!p.replayed);
    let b = m.last();
    assert_eq!(b.route(), "/api/v1/credits/top-ups");
    assert_eq!(
        b.json(),
        json!({"subject": "org:1", "amount": 100, "idempotency_key": "k1", "expires_at": "2027-01-01T00:00:00Z"})
    );
    c.grant(&CreditInput::new("org:1", 5, "k2")).await.expect("grant");
    assert_eq!(m.last().route(), "/api/v1/credits/grants");
    let mut spend = SpendInput::new(3, "k3");
    spend.feature = Some("ai".into());
    spend.units = Some(30);
    c.spend(&spend).await.expect("spend");
    assert_eq!(
        m.last().json(),
        json!({"amount": 3, "idempotency_key": "k3", "feature": "ai", "units": 30})
    );
    c.refund(&AmendInput::new("e1", "k4")).await.expect("refund");
    assert_eq!(m.last().json(), json!({"entry_id": "e1", "idempotency_key": "k4"}));
    c.reverse(&AmendInput::new("e1", "k5")).await.expect("reverse");
    assert_eq!(m.last().route(), "/api/v1/credits/reversals");
    let mut r = RateInput::new(2);
    r.basis = Some(RateBasis::Overage);
    c.rates().set("ai", &r).await.expect("rate");
    assert_eq!(
        (m.last().method.as_str(), m.last().json()),
        ("PUT", json!({"credits_per_unit": 2, "basis": "overage"}))
    );
    c.rates().delete("ai").await.expect("delete rate");
    assert_eq!(c.usage_cursor().set(12).await.expect("cursor"), 12);
    c.maintenance().await.expect("maintenance");
}

#[tokio::test]
async fn credits_insufficient_balance_is_a_402() {
    let (_m, fb) = serve(status(
        402,
        json!({"error": {"code": "insufficient_credits", "message": "no", "details": {"balance": 1, "requested": 5}}}),
    ))
    .await;
    let e = fb.credits().spend(&SpendInput::new(5, "k")).await.expect_err("402");
    assert_eq!(e.status(), Some(402));
    assert_eq!(e.code(), "insufficient_credits");
    assert_eq!(
        e.api().and_then(|a| a.details.as_ref()).map(|d| d["requested"].clone()),
        Some(json!(5))
    );
}

// ---- vector ----

#[tokio::test]
async fn vector_collections_fields_and_items() {
    let coll = with("VectorCollection", json!({"name": "docs"}));
    let m = Mock::start(vec![
        ok(json!({"items": [coll.clone()]})),
        status(201, coll.clone()),
        ok(schema("VectorStats")),
        ok(coll),
        no_content(),
        ok(json!({"items": [schema("VectorField")]})),
        status(201, schema("VectorField")),
        no_content(),
        ok(with("UpsertResult", json!({"ids": ["a", "b"]}))),
        ok(schema("Item")),
        no_content(),
        ok(json!({"deleted": 2})),
        ok(json!({"indexes": [schema("IndexStatus")]})),
        ok(schema("CollectionStats")),
    ])
    .await;
    let v = m.client().vector();
    assert_eq!(v.list().await.expect("list")[0].name, "docs");
    let mut f = FieldSpec::new("emb", 3, Metric::Cosine);
    f.connectivity = Some(32);
    v.create(&NewVectorCollection::new("docs").field(f))
        .await
        .expect("create");
    assert_eq!(
        m.last().json(),
        json!({"name": "docs", "fields": [{"name": "emb", "dimension": 3, "metric": "cosine", "connectivity": 32}]})
    );
    v.stats().await.expect("stats");
    let docs = v.collection("docs");
    assert_eq!(docs.name(), "docs");
    docs.get().await.expect("get");
    docs.drop_collection().await.expect("drop");
    assert_eq!(
        (m.last().method.as_str(), m.last().path.as_str()),
        ("DELETE", "/api/v1/vector/collections/docs")
    );
    assert_eq!(docs.fields().list().await.expect("fields").len(), 1);
    docs.fields()
        .add(&FieldSpec::new("img", 8, Metric::L2))
        .await
        .expect("add field");
    assert_eq!(m.last().path, "/api/v1/vector/collections/docs/fields");
    docs.fields().drop_field("img").await.expect("drop field");

    let r = docs
        .upsert(&[
            UpsertItem::new()
                .id("a")
                .vector("emb", &[1.0, 0.0, 0.0])
                .metadata(json!({"k": 1}))
                .text("hello"),
            UpsertItem::new().vector("emb", &[0.0, 1.0, 0.0]),
        ])
        .await
        .expect("upsert");
    assert_eq!(r.ids, ["a", "b"]);
    assert_eq!(
        m.last().json(),
        json!({"items": [
            {"id": "a", "vectors": {"emb": [1.0, 0.0, 0.0]}, "metadata": {"k": 1}, "text": "hello"},
            {"vectors": {"emb": [0.0, 1.0, 0.0]}}
        ]})
    );
    docs.get_item("a").await.expect("item");
    docs.delete_item("a").await.expect("delete item");
    assert_eq!(docs.delete_items(&["a", "b"]).await.expect("delete items"), 2);
    assert_eq!(m.last().json(), json!({"ids": ["a", "b"]}));
    assert_eq!(docs.rebuild(Some("emb")).await.expect("rebuild").len(), 1);
    assert_eq!(m.last().json(), json!({"field": "emb"}));
    docs.stats().await.expect("stats");
}

#[tokio::test]
async fn vector_search_hybrid_and_recall() {
    let m = Mock::start(vec![
        ok(with(
            "SearchResponse",
            json!({"hits": [{"id": "a", "distance": 0.1, "score": 0.9}], "strategy": "ann"}),
        )),
        ok(schema("HybridResponse")),
        ok(schema("HybridResponse")),
        ok(schema("RecallReport")),
    ])
    .await;
    let docs = m.client().vector().collection("docs");
    let mut o = SearchOptions::new(&[1.0, 0.0, 0.0])
        .k(3)
        .field("emb")
        .filter("year >= {:min}", Some(json!({"min": 2020})))
        .exact();
    o.filter_mode = Some(FilterMode::Pre);
    o.include_text = Some(true);
    let r = docs.search(&o).await.expect("search");
    assert_eq!(r.hits[0].id, "a");
    assert_eq!(
        m.last().json(),
        json!({"vector": [1.0, 0.0, 0.0], "field": "emb", "k": 3, "filter": "year >= {:min}", "params": {"min": 2020},
            "filter_mode": "pre", "exact": true, "include_text": true})
    );
    assert_eq!(m.last().route(), "/api/v1/vector/collections/docs/search");
    docs.hybrid(&HybridOptions::new("rust sdk", &[0.5, 0.5, 0.0]).k(5))
        .await
        .expect("hybrid");
    assert_eq!(
        m.last().json(),
        json!({"vector": [0.5, 0.5, 0.0], "text": "rust sdk", "k": 5})
    );
    docs.hybrid(&HybridOptions::text_only("only words"))
        .await
        .expect("keyword only");
    assert_eq!(m.last().json(), json!({"text": "only words"}));
    docs.recall(&RecallOptions {
        samples: Some(50),
        ..RecallOptions::default()
    })
    .await
    .expect("recall");
    assert_eq!(m.last().json(), json!({"samples": 50}));
}

// ---- graph ----

#[tokio::test]
async fn graph_nodes_and_edges() {
    let node = with(
        "Node",
        json!({"id": "n1", "label": "User", "properties": {"name": "Ada"}}),
    );
    let edge = with(
        "Edge",
        json!({"id": "e1", "type": "FOLLOWS", "source": "n1", "target": "n2", "directed": true, "properties": {}}),
    );
    let m = Mock::start(vec![
        status(201, node.clone()),
        ok(with("NodePage", json!({"items": [node.clone()], "next_after": "n1"}))),
        ok(json!({"items": [node.clone()]})),
        ok(node.clone()),
        ok(node.clone()),
        ok(node.clone()),
        ok(with("NodeDeleted", json!({"id": "n1", "deleted_edges": 2}))),
        ok(with(
            "NeighborPage",
            json!({"items": [{"direction": "out", "edge": edge.clone(), "node": node.clone()}], "next_after": "100"}),
        )),
        status(201, edge.clone()),
        ok(with("EdgePage", json!({"items": [edge.clone()]}))),
        ok(edge.clone()),
        ok(edge.clone()),
        no_content(),
    ])
    .await;
    let g = m.client().graph();
    let n = g
        .nodes()
        .create(&NewNode::new("User", json!({"name": "Ada"})).id("n1"))
        .await
        .expect("create node");
    assert_eq!(n.properties["name"], "Ada");
    assert_eq!(
        m.last().json(),
        json!({"id": "n1", "label": "User", "properties": {"name": "Ada"}})
    );
    let page = g
        .nodes()
        .list(&ListNodes {
            label: Some("User".into()),
            limit: Some(1),
            ..ListNodes::default()
        })
        .await
        .expect("list");
    assert_eq!(page.next_after.as_deref(), Some("n1"));
    assert_eq!(m.last().q("label").as_deref(), Some("User"));
    assert_eq!(g.nodes().list_all(&ListNodes::default()).await.expect("all").len(), 1);
    g.nodes().get("n 1").await.expect("get");
    assert_eq!(m.last().path, "/api/v1/graph/nodes/n%201");
    g.nodes()
        .replace(
            "n1",
            &NodeChange {
                label: None,
                properties: Some(json!({"a": 1})),
            },
        )
        .await
        .expect("replace");
    assert_eq!(m.last().method, "PUT");
    g.nodes()
        .patch(
            "n1",
            &NodeChange {
                label: Some("Admin".into()),
                properties: Some(json!({"a": null})),
            },
        )
        .await
        .expect("patch");
    assert_eq!(
        (m.last().method.as_str(), m.last().json()),
        ("PATCH", json!({"label": "Admin", "properties": {"a": null}}))
    );
    assert_eq!(g.nodes().delete("n1").await.expect("delete").deleted_edges, 2);
    let nb = g
        .nodes()
        .neighbors(
            "n1",
            &NeighborsOptions {
                direction: Some(Direction::Both),
                types: vec!["A".into(), "B".into()],
                limit: Some(10),
                offset: Some(5),
            },
        )
        .await
        .expect("neighbors");
    assert_eq!(nb.items[0].direction, Direction::Out);
    let q = m.last();
    assert_eq!(
        (
            q.q("direction").as_deref(),
            q.q("type").as_deref(),
            q.q("offset").as_deref()
        ),
        (Some("both"), Some("A,B"), Some("5"))
    );

    g.edges()
        .create(&NewEdge::new("FOLLOWS", "n1", "n2").properties(json!({"since": 2020})))
        .await
        .expect("edge");
    assert_eq!(
        m.last().json(),
        json!({"type": "FOLLOWS", "source": "n1", "target": "n2", "properties": {"since": 2020}})
    );
    g.edges()
        .list(&ListEdges {
            edge_type: Some("FOLLOWS".into()),
            source: Some("n1".into()),
            ..ListEdges::default()
        })
        .await
        .expect("edges");
    assert_eq!(
        (m.last().q("type").as_deref(), m.last().q("source").as_deref()),
        (Some("FOLLOWS"), Some("n1"))
    );
    g.edges().get("e1").await.expect("get edge");
    g.edges()
        .patch("e1", &json!({"since": null}))
        .await
        .expect("patch edge");
    assert_eq!(m.last().json(), json!({"properties": {"since": null}}));
    g.edges().delete("e1").await.expect("delete edge");
}

#[tokio::test]
async fn graph_traversal_paths_stats_and_cypher_engine() {
    let node = with("Node", json!({"id": "n1", "label": "User"}));
    let m = Mock::start(vec![
        ok(with("TraverseResult", json!({"start": "n1", "nodes": [{"depth": 1, "node": node.clone()}], "truncated": false}))),
        ok(with("PathResult", json!({"found": true, "length": 1, "nodes": [node.clone()]}))),
        ok(with("GraphStats", json!({"nodes": 3, "edges": 2, "edge_types": [{"type": "FOLLOWS", "count": 2}]}))),
        ok(json!({"columns": ["n.name"], "rows": [["Ada"], [3]], "truncated": false, "engine": "portable"})),
        ok(json!({"columns": ["n"], "rows": [[{"id": "n1", "label": "User", "properties": {}}]], "truncated": true, "engine": "age"})),
        ok(json!({"dropped": true})),
        status(409, json!({"error": {"code": "graph_materialising", "message": "building", "job_id": "j1"}})),
    ])
    .await;
    let g = m.client().graph();
    let t = g
        .traverse(&TraverseOptions::new("n1").max_depth(2).direction(Direction::In))
        .await
        .expect("traverse");
    assert_eq!(t.nodes[0].depth, 1);
    assert_eq!(
        m.last().json(),
        json!({"start": "n1", "direction": "in", "max_depth": 2})
    );
    let p = g
        .shortest_path(&ShortestPathOptions::new("n1", "n2"))
        .await
        .expect("path");
    assert!(p.found);
    assert_eq!(m.last().json(), json!({"from": "n1", "to": "n2"}));
    assert_eq!(g.stats().await.expect("stats").edge_types[0].r#type, "FOLLOWS");

    let r = g
        .cypher(
            "MATCH (n:User) WHERE n.age > $min RETURN n.name",
            &CypherOptions {
                params: Some(json!({"min": 18})),
                limit: Some(10),
                timeout_ms: Some(2000),
            },
        )
        .await
        .expect("cypher");
    assert_eq!(r.engine, "portable", "the engine that answered is reported");
    assert_eq!(r.rows, vec![vec![json!("Ada")], vec![json!(3)]], "scalar cells decode");
    assert_eq!(
        m.last().json(),
        json!({"query": "MATCH (n:User) WHERE n.age > $min RETURN n.name", "params": {"min": 18}, "limit": 10, "timeout_ms": 2000})
    );
    let r = g
        .cypher("MATCH (n) RETURN n", &CypherOptions::default())
        .await
        .expect("cypher age");
    assert_eq!(r.engine, "age");
    assert!(r.truncated);
    assert_eq!(r.rows[0][0]["label"], "User");
    assert!(g.drop_cypher().await.expect("drop").dropped);
    let e = g
        .cypher("MATCH (n) RETURN n", &CypherOptions::default())
        .await
        .expect_err("materialising");
    assert_eq!((e.kind(), e.code()), (Some(ErrorKind::Conflict), "graph_materialising"));
}

// ---- sql ----

#[tokio::test]
async fn sql_run_batch_explain_and_tables() {
    let result = json!({"kind": "read", "columns": ["id", "name"], "rows": [[1, "a"], [2, null]], "rows_affected": 0,
        "truncated": false, "duration_ms": 0.5});
    let m = Mock::start(vec![
        ok(result.clone()),
        ok(result.clone()),
        ok(json!({"items": [result.clone(), {"kind": "write", "columns": [], "rows": [], "rows_affected": 1, "truncated": false, "duration_ms": 0.1}]})),
        ok(schema("QueryPlanSchema")),
        ok(json!({"items": [schema("TableInfoSchema")]})),
        ok(schema("TableInfoSchema")),
    ])
    .await;
    let sql = m.client().sql();
    let r = sql
        .execute(
            "SELECT * FROM t WHERE id = ?",
            &[json!(1)],
            &SqlOptions {
                show_secrets: Some(true),
                ..SqlOptions::default()
            },
        )
        .await
        .expect("execute");
    assert_eq!((r.kind.as_str(), r.rows.len()), ("read", 2));
    assert_eq!(m.last().route(), "/api/v1/sql");
    assert_eq!(
        m.last().json(),
        json!({"sql": "SELECT * FROM t WHERE id = ?", "params": [1], "show_secrets": true})
    );

    let rows = sql
        .query("SELECT id, name FROM t", &[], &SqlOptions::default())
        .await
        .expect("query");
    assert_eq!(rows[0]["name"], "a");
    assert_eq!(rows[1]["name"], Value::Null);
    assert_eq!(rows_as_objects(&r).len(), 2);

    let run_as = SqlOptions {
        run_as: Some(SqlRunAs {
            principal: "user:users/u1".into(),
            tenant: None,
        }),
        read_from_replica: Some(true),
        ..SqlOptions::default()
    };
    let batch = sql
        .batch(
            &[
                "SELECT 1".into(),
                SqlStatement::new("INSERT INTO t VALUES (?)", vec![json!("x")]),
            ],
            &run_as,
        )
        .await
        .expect("batch");
    assert_eq!(batch.len(), 2);
    assert_eq!(batch[1].rows_affected, 1);
    let b = m.last().json();
    assert_eq!(m.last().route(), "/api/v1/sql/batch");
    assert_eq!(
        b["statements"],
        json!([{"sql": "SELECT 1"}, {"sql": "INSERT INTO t VALUES (?)", "params": ["x"]}])
    );
    assert_eq!(b["run_as"]["principal"], "user:users/u1");
    assert!(b.get("read_from_replica").is_none(), "batches stay on the primary");

    sql.explain("SELECT 1", &[]).await.expect("explain");
    assert_eq!(m.last().json(), json!({"sql": "SELECT 1", "params": []}));
    assert_eq!(sql.tables().await.expect("tables").len(), 1);
    sql.table("my table").await.expect("table");
    assert_eq!(m.last().path, "/api/v1/sql/tables/my%20table");
}

#[tokio::test]
async fn sql_forbidden_for_non_admins() {
    let (_m, fb) = serve(err(403, "forbidden")).await;
    let e = fb
        .sql()
        .execute("SELECT 1", &[], &SqlOptions::default())
        .await
        .expect_err("403");
    assert!(e.is_forbidden());
}

// ---- indexes ----

#[tokio::test]
async fn indexes_lifecycle_and_slow_queries() {
    let idx = with("IndexInfoSchema", json!({"name": "idx_posts_title"}));
    let m = Mock::start(vec![
        ok(json!({"items": [idx.clone()]})),
        status(201, idx.clone()),
        ok(idx.clone()),
        ok(idx.clone()),
        ok(schema("IndexStatsSchema")),
        no_content(),
        ok(json!({"items": [schema("IndexRecommendationSchema")]})),
        no_content(),
        ok(with("SlowQueryLog", json!({"threshold_ms": 100}))),
        no_content(),
        ok(json!({"threshold_ms": 250})),
    ])
    .await;
    let ix = m.client().indexes();
    assert_eq!(ix.list(Some("posts")).await.expect("list")[0].name, "idx_posts_title");
    assert_eq!(m.last().q("collection").as_deref(), Some("posts"));
    ix.create(
        &NewIndex::new("posts", vec![IndexKey::asc("title:lower"), IndexKey::desc("created")])
            .name("idx_posts_title")
            .unique()
            .kind(IndexKind::Btree),
    )
    .await
    .expect("create");
    assert_eq!(
        m.last().json(),
        json!({"collection": "posts", "keys": [{"path": "title:lower"}, {"path": "created", "desc": true}],
            "name": "idx_posts_title", "unique": true, "kind": "btree"})
    );
    ix.get("idx_posts_title").await.expect("get");
    ix.rebuild("idx_posts_title").await.expect("rebuild");
    assert_eq!(
        (m.last().method.as_str(), m.last().path.as_str()),
        ("POST", "/api/v1/indexes/idx_posts_title/rebuild")
    );
    ix.stats("idx_posts_title").await.expect("stats");
    ix.drop_index("idx_posts_title").await.expect("drop");
    assert_eq!(ix.recommendations().await.expect("recommendations").len(), 1);
    ix.analyze(Some("posts")).await.expect("analyze");
    assert_eq!(m.last().json(), json!({"collection": "posts"}));
    assert_eq!(ix.slow_queries().await.expect("slow").threshold_ms, 100);
    ix.clear_slow_queries().await.expect("clear");
    assert_eq!(ix.set_slow_query_threshold(250).await.expect("threshold"), 250);
    assert_eq!(
        (m.last().method.as_str(), m.last().json()),
        ("PUT", json!({"threshold_ms": 250}))
    );
}

// ---- documents ----

#[tokio::test]
async fn documents_crud_versions_and_if_match() {
    let m = Mock::start(vec![
        status(201, json!({"id": "d1", "title": "Hi", "_version": 1})),
        ok(json!({"items": [{"id": "d1"}, {"id": "d2"}]})),
        ok(json!({"id": "d1", "title": "Hi", "_version": 1})),
        ok(json!({"id": "d1", "done": true, "_version": 2})),
        ok(json!({"id": "d1", "_version": 3})),
        no_content(),
        ok(json!({"items": [schema("RecordVersionSchema")]})),
        err(412, "version_mismatch"),
    ])
    .await;
    let notes = m.client().documents().collection("notes");
    let d = notes.create(&json!({"title": "Hi"})).await.expect("create");
    assert_eq!(d["_version"], 1);
    assert_eq!(m.last().route(), "/api/v1/documents/notes");
    assert_eq!(
        notes
            .create_many(&[json!({"a": 1}), json!({"a": 2})])
            .await
            .expect("many")
            .len(),
        2
    );
    assert!(m.last().json().is_array());
    let got = notes
        .get(
            "d1",
            &GetDocumentOptions {
                expand: vec!["owner".into()],
                fields: vec!["title".into(), "done".into()],
                ..GetDocumentOptions::default()
            },
        )
        .await
        .expect("get");
    assert_eq!(got["title"], "Hi");
    let q = m.last();
    assert_eq!(
        (q.q("expand").as_deref(), q.q("fields").as_deref()),
        (Some("owner"), Some("title,done"))
    );
    notes
        .patch("d1", &json!({"done": true}), WriteOptions { version: Some(1) })
        .await
        .expect("patch");
    assert_eq!(
        (m.last().method.as_str(), m.last().header("if-match")),
        ("PATCH", Some("\"1\""))
    );
    notes
        .replace("d1", &json!({"x": 1}), WriteOptions::default())
        .await
        .expect("replace");
    assert!(m.last().header("if-match").is_none());
    notes
        .delete("d1", WriteOptions { version: Some(3) })
        .await
        .expect("delete");
    assert_eq!(m.last().header("if-match"), Some("\"3\""));
    assert_eq!(notes.versions("d1").await.expect("versions").len(), 1);
    assert_eq!(m.last().path, "/api/v1/documents/notes/d1/versions");
    let e = notes
        .patch("d1", &json!({"a": 1}), WriteOptions { version: Some(1) })
        .await
        .expect_err("stale");
    assert!(e.is_conflict());

    let typed = notes.typed::<serde_json::Map<String, Value>>();
    assert_eq!(typed.name(), "notes");
}
