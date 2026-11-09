//! The vector service against a real SQLite database and real USearch index
//! files: SPEC §27 #12 (store and retrieve a vector), #13 (similarity search),
//! #23 (rebuild derived indexes), filters, tenant isolation, hybrid search,
//! validation, recall.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::HashSet;
use std::time::{Duration, Instant};

use common::*;
use forgebase_data::AccessContext;
use forgebase_vector::model::*;
use forgebase_vector::{VectorConfig, VectorError};
use serde_json::json;

fn small(exact_threshold: u32) -> VectorConfig {
    VectorConfig {
        exact_threshold,
        ..VectorConfig::default()
    }
}

// ------------------------------------------------------------ SPEC #12, #13

#[tokio::test]
async fn spec12_store_and_retrieve_a_vector_survives_restart() {
    let h = harness().await;
    let ctx = admin();
    create(&h.svc, &ctx, "docs", vec![field("emb", 4, Metric::Cosine)], None).await;
    let r = h
        .svc
        .upsert(
            &ctx,
            "docs",
            vec![UpsertItem {
                id: Some("a".into()),
                vectors: [("emb".to_owned(), vec![0.5, -0.25, 1.0, 0.125])].into(),
                metadata: json!({"title": "first", "tags": ["x"]}).as_object().cloned(),
                text: Some("hello vector world".into()),
            }],
        )
        .await
        .unwrap();
    assert_eq!((r.ids.clone(), r.created, r.updated), (vec!["a".to_owned()], 1, 0));

    let item = h.svc.get_item(&ctx, "docs", "a").await.unwrap();
    assert_eq!(item.vectors["emb"], vec![0.5, -0.25, 1.0, 0.125]);
    assert_eq!(item.metadata["title"], "first");
    assert_eq!(item.text.as_deref(), Some("hello vector world"));
    assert_eq!(item.version, 1);

    // Replace: a new version, the old vector is gone.
    let r = h
        .svc
        .upsert(&ctx, "docs", vec![item_of("a", &[1.0, 0.0, 0.0, 0.0])])
        .await
        .unwrap();
    assert_eq!((r.created, r.updated), (0, 1));
    let h = h.restart(VectorConfig::default()).await;
    let item = h.svc.get_item(&ctx, "docs", "a").await.unwrap();
    assert_eq!(item.vectors["emb"], vec![1.0, 0.0, 0.0, 0.0]);
    assert_eq!(item.version, 2);
    assert!(item.text.is_none());
    let hit = h
        .svc
        .search(&ctx, "docs", query("emb", &[1.0, 0.0, 0.0, 0.0], 1))
        .await
        .unwrap();
    assert_eq!(ids(&hit), vec!["a"]);

    assert!(matches!(
        h.svc.get_item(&ctx, "docs", "missing").await,
        Err(VectorError::ItemNotFound(_))
    ));
    h.svc.delete_item(&ctx, "docs", "a").await.unwrap();
    assert!(matches!(
        h.svc.get_item(&ctx, "docs", "a").await,
        Err(VectorError::ItemNotFound(_))
    ));
}

fn item_of(id: &str, v: &[f32]) -> UpsertItem {
    item(id, "emb", v, json!({}))
}

#[tokio::test]
async fn spec13_similarity_search_ranks_by_each_metric() {
    let h = harness().await;
    let ctx = admin();
    for (name, metric) in [("cos", Metric::Cosine), ("euc", Metric::L2), ("ip", Metric::Dot)] {
        create(&h.svc, &ctx, name, vec![field("emb", 2, metric)], None).await;
        h.svc
            .upsert(
                &ctx,
                name,
                vec![
                    item_of("east", &[1.0, 0.0]),
                    item_of("north", &[0.0, 1.0]),
                    item_of("far_east", &[10.0, 0.5]),
                    item_of("west", &[-1.0, 0.0]),
                ],
            )
            .await
            .unwrap();
    }
    let q = [2.0, 0.1];
    // Cosine: direction only.
    let r = h.svc.search(&ctx, "cos", query("emb", &q, 4)).await.unwrap();
    assert_eq!(ids(&r)[..2], ["far_east", "east"]);
    assert_eq!(ids(&r)[3], "west");
    assert!(
        (r.hits[3].score + 1.0).abs() < 0.01,
        "opposite direction: similarity -1"
    );
    // Euclidean: position.
    let r = h.svc.search(&ctx, "euc", query("emb", &q, 4)).await.unwrap();
    assert_eq!(ids(&r), ["east", "north", "west", "far_east"]);
    let d = (1.0f32 + 0.01).sqrt();
    assert!((r.hits[0].distance - d).abs() < 1e-4, "euclidean, not squared");
    // Dot product: magnitude counts.
    let r = h.svc.search(&ctx, "ip", query("emb", &q, 4)).await.unwrap();
    assert_eq!(ids(&r)[0], "far_east");
    assert!((r.hits[0].score - 20.05).abs() < 1e-3);
    assert_eq!(r.strategy, Strategy::Exact, "small sets are searched exactly");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ann_search_agrees_with_exact_search() {
    let h = harness_with(small(50)).await;
    let ctx = admin();
    create(&h.svc, &ctx, "docs", vec![field("emb", 16, Metric::L2)], None).await;
    let vectors = fill(&h.svc, &ctx, "docs", "emb", 2000, 16, 7).await;
    let mut agree = 0;
    for (i, q) in vectors.iter().step_by(100).enumerate() {
        let ann = h.svc.search(&ctx, "docs", query("emb", q, 10)).await.unwrap();
        assert_eq!(ann.strategy, Strategy::Ann);
        assert_eq!(
            ann.hits[0].id,
            format!("v{}", i * 100),
            "a stored vector is its own nearest"
        );
        let mut req = query("emb", q, 10);
        req.exact = true;
        let exact = h.svc.search(&ctx, "docs", req).await.unwrap();
        assert_eq!(exact.strategy, Strategy::Exact);
        if ids(&ann) == ids(&exact) {
            agree += 1;
        }
    }
    assert!(agree >= 18, "ANN matched exact on {agree}/20 queries");
}

// ---------------------------------------------------------------- validation

#[tokio::test]
async fn invalid_vectors_and_requests_are_rejected() {
    let h = harness().await;
    let ctx = admin();
    create(&h.svc, &ctx, "docs", vec![field("emb", 3, Metric::Cosine)], None).await;
    let err = h
        .svc
        .upsert(&ctx, "docs", vec![item_of("a", &[1.0, 2.0])])
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            VectorError::DimensionMismatch {
                expected: 3,
                got: 2,
                ..
            }
        ),
        "{err:?}"
    );
    let err = h
        .svc
        .search(&ctx, "docs", query("emb", &[1.0, 2.0, 3.0, 4.0], 1))
        .await
        .unwrap_err();
    assert!(matches!(err, VectorError::DimensionMismatch { got: 4, .. }), "{err:?}");
    for (bad, code) in [
        (vec![0.0, 0.0, 0.0], "invalid_vector"),
        (vec![1e39, 0.0, 0.0], "invalid_vector"),
    ] {
        let mut it = item_of("a", &[1.0, 1.0, 1.0]);
        it.vectors.insert("emb".into(), bad);
        let err = h.svc.upsert(&ctx, "docs", vec![it]).await.unwrap_err();
        assert!(
            matches!(err, VectorError::Invalid { code: c, .. } if c == code),
            "{err:?}"
        );
    }
    let mut it = item_of("a", &[1.0, 1.0, 1.0]);
    it.vectors.insert("nope".into(), vec![1.0]);
    assert!(matches!(
        h.svc.upsert(&ctx, "docs", vec![it]).await,
        Err(VectorError::Invalid {
            code: "unknown_vector_field",
            ..
        })
    ));
    let bad_id = item_of("../x", &[1.0, 1.0, 1.0]);
    assert!(h.svc.upsert(&ctx, "docs", vec![bad_id]).await.is_err());
    let dup = vec![item_of("a", &[1.0, 1.0, 1.0]), item_of("a", &[1.0, 1.0, 1.0])];
    assert!(h.svc.upsert(&ctx, "docs", dup).await.is_err());
    assert!(h.svc.upsert(&ctx, "docs", vec![]).await.is_err());
    let mut q = query("emb", &[1.0, 1.0, 1.0], 0);
    assert!(h.svc.search(&ctx, "docs", q.clone()).await.is_err(), "k = 0");
    q.k = 5;
    q.filter = Some("a = ".into());
    assert!(matches!(
        h.svc.search(&ctx, "docs", q).await,
        Err(VectorError::Filter(_))
    ));
    // Nothing was written by any rejected request.
    assert_eq!(h.svc.get_collection(&ctx, "docs").await.unwrap().items, 0);

    // Collection and field definitions.
    let bad_dim = NewCollection {
        name: "x".into(),
        description: String::new(),
        fields: vec![field("emb", 0, Metric::L2)],
        exact_threshold: None,
    };
    assert!(h.svc.create_collection(&ctx, bad_dim).await.is_err());
    let dup = NewCollection {
        name: "docs".into(),
        description: String::new(),
        fields: vec![],
        exact_threshold: None,
    };
    assert!(matches!(
        h.svc.create_collection(&ctx, dup).await,
        Err(VectorError::CollectionExists(_))
    ));
    assert!(matches!(
        h.svc.add_field(&ctx, "docs", field("EMB", 3, Metric::L2)).await,
        Err(VectorError::FieldExists(_))
    ));
}

#[tokio::test]
async fn anonymous_and_unprivileged_callers_are_denied() {
    let h = harness().await;
    create(&h.svc, &admin(), "docs", vec![field("emb", 2, Metric::L2)], None).await;
    let anon = AccessContext::anonymous();
    let user = AccessContext::user("users", "u1");
    for ctx in [&anon, &user] {
        let want_401 = ctx == &anon;
        let check = |r: Result<(), VectorError>| match r {
            Err(VectorError::Unauthenticated) => assert!(want_401),
            Err(VectorError::Forbidden(_)) => assert!(!want_401),
            other => panic!("expected a denial, got {other:?}"),
        };
        check(h.svc.list_collections(ctx).await.map(|_| ()));
        check(
            h.svc
                .search(ctx, "docs", query("emb", &[1.0, 0.0], 1))
                .await
                .map(|_| ()),
        );
        check(
            h.svc
                .upsert(ctx, "docs", vec![item_of("a", &[1.0, 0.0])])
                .await
                .map(|_| ()),
        );
        check(h.svc.get_item(ctx, "docs", "a").await.map(|_| ()));
        check(h.svc.rebuild(ctx, "docs", None).await.map(|_| ()));
        check(h.svc.stats(ctx, "docs").await.map(|_| ()));
        check(h.svc.delete_collection(ctx, "docs").await);
    }
}

// ------------------------------------------------------------------- filters

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn metadata_filters_pre_and_post_return_only_matches() {
    let h = harness_with(small(100)).await;
    let ctx = admin();
    create(&h.svc, &ctx, "docs", vec![field("emb", 8, Metric::Cosine)], None).await;
    let vectors = fill(&h.svc, &ctx, "docs", "emb", 3000, 8, 11).await;
    let q = &vectors[5];
    let truth = |filter: &str| {
        let mut r = query("emb", q, 20);
        r.filter = Some(filter.into());
        r.exact = true;
        r
    };
    for (filter, check) in [
        (
            "category = 'b'",
            (|m: &serde_json::Value| m["category"] == "b") as fn(&serde_json::Value) -> bool,
        ),
        ("n < 50", |m| m["n"].as_i64().unwrap() < 50),
        ("even = true && n >= 1000", |m| {
            m["even"] == true && m["n"].as_i64().unwrap() >= 1000
        }),
    ] {
        let exact = h.svc.search(&ctx, "docs", truth(filter)).await.unwrap();
        assert_eq!(exact.hits.len(), 20);
        for mode in [FilterMode::Auto, FilterMode::Pre, FilterMode::Post] {
            let mut req = query("emb", q, 20);
            req.filter = Some(filter.into());
            req.filter_mode = mode;
            let r = h.svc.search(&ctx, "docs", req).await.unwrap();
            assert!(
                r.hits
                    .iter()
                    .all(|hit| check(&serde_json::Value::Object(hit.metadata.clone().unwrap()))),
                "{filter} {mode:?} returned a non-matching item"
            );
            assert_eq!(r.hits.len(), 20, "{filter} {mode:?}");
            let overlap = ids(&r).iter().filter(|id| ids(&exact).contains(id)).count();
            assert!(overlap >= 18, "{filter} {mode:?}: {overlap}/20 of the exact results");
            match mode {
                FilterMode::Post => assert_eq!(r.strategy, Strategy::PostFilter),
                _ if filter == "n < 50" => {
                    assert_eq!(r.strategy, Strategy::Exact, "few matches: exact");
                    assert_eq!(ids(&r), ids(&exact));
                    assert_eq!(r.candidates, Some(50));
                }
                _ => assert_eq!(r.strategy, Strategy::FilteredAnn),
            }
        }
    }
    // A filter with parameters, and one that matches nothing.
    let mut req = query("emb", q, 5);
    req.filter = Some("category = {:c} && n = {:n}".into());
    req.params = [("c".to_owned(), json!("a")), ("n".to_owned(), json!(3))].into();
    assert_eq!(ids(&h.svc.search(&ctx, "docs", req).await.unwrap()), vec!["v3"]);
    let mut req = query("emb", q, 5);
    req.filter = Some("category = 'zzz'".into());
    for mode in [FilterMode::Pre, FilterMode::Post] {
        req.filter_mode = mode;
        assert!(h.svc.search(&ctx, "docs", req.clone()).await.unwrap().hits.is_empty());
    }
}

// ----------------------------------------------------------- tenant isolation

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn filters_never_leak_other_tenants_vectors() {
    let h = harness_with(small(10)).await;
    let (a, b) = (tenant("tenant_a"), tenant("tenant_b"));
    for ctx in [&a, &b] {
        create(&h.svc, ctx, "kb", vec![field("emb", 4, Metric::L2)], None).await;
    }
    // Same ids in both tenants; tenant B's vectors sit exactly on the query.
    let mut rng = Rng::new(3);
    let a_items: Vec<UpsertItem> = (0..300)
        .map(|i| {
            let mut it = item(&format!("d{i}"), "emb", &rng.vector(4), json!({"owner": "a", "n": i}));
            it.text = Some(format!("alpha shared words {i}"));
            it
        })
        .collect();
    h.svc.upsert(&a, "kb", a_items).await.unwrap();
    let target = [0.5f32, 0.5, 0.5, 0.5];
    let b_items: Vec<UpsertItem> = (0..300)
        .map(|i| {
            let mut it = item(
                &format!("d{i}"),
                "emb",
                &target,
                json!({"owner": "b", "secret": true, "n": i}),
            );
            it.text = Some("bravo secret shared words".into());
            it
        })
        .collect();
    h.svc.upsert(&b, "kb", b_items).await.unwrap();

    let filters = [
        None,
        Some("owner = 'b'"),
        Some("secret = true"),
        Some("id != '' || id = ''"),
        Some("owner != 'a'"),
        Some("n >= 0"),
    ];
    for filter in filters {
        for mode in [FilterMode::Auto, FilterMode::Pre, FilterMode::Post] {
            for exact in [false, true] {
                let mut req = query("emb", &target, 50);
                req.filter = filter.map(str::to_owned);
                req.filter_mode = mode;
                req.exact = exact;
                let r = h.svc.search(&a, "kb", req).await.unwrap();
                assert!(
                    r.hits.iter().all(|hit| hit.metadata.as_ref().unwrap()["owner"] == "a"),
                    "{filter:?} {mode:?} exact={exact} leaked tenant b"
                );
            }
        }
        let r = h
            .svc
            .hybrid(
                &a,
                "kb",
                HybridRequest {
                    field: None,
                    vector: Some(target.iter().map(|x| f64::from(*x)).collect()),
                    text: Some("bravo secret shared".into()),
                    text_operator: TextOperator::Or,
                    k: 50,
                    candidates: None,
                    rrf_k: None,
                    weights: None,
                    filter: filter.map(str::to_owned),
                    params: Default::default(),
                    filter_mode: FilterMode::Auto,
                    include_vectors: true,
                    include_metadata: true,
                    include_text: true,
                },
            )
            .await
            .unwrap();
        assert!(r.hits.iter().all(|hit| hit.metadata.as_ref().unwrap()["owner"] == "a"));
        assert!(r.hits.iter().all(|hit| !hit.text.as_ref().unwrap().contains("bravo")));
    }
    // Reads by id and writes stay in the caller's tenant.
    let item_a = h.svc.get_item(&a, "kb", "d1").await.unwrap();
    assert_eq!(item_a.metadata["owner"], "a");
    assert_eq!(
        h.svc
            .delete_items(&a, "kb", vec!["d1".into(), "d2".into()])
            .await
            .unwrap(),
        2
    );
    assert_eq!(h.svc.get_item(&b, "kb", "d1").await.unwrap().metadata["owner"], "b");
    assert_eq!(h.svc.get_collection(&b, "kb").await.unwrap().items, 300);
    // A third tenant (and the default one) sees no collection at all.
    for ctx in [tenant("tenant_c"), admin()] {
        assert!(h.svc.list_collections(&ctx).await.unwrap().is_empty());
        assert!(matches!(
            h.svc.search(&ctx, "kb", query("emb", &target, 1)).await,
            Err(VectorError::CollectionNotFound(_))
        ));
    }
    // Rebuilding tenant a's index never pulls in tenant b's vectors.
    h.svc.rebuild(&a, "kb", None).await.unwrap();
    wait_ready(&h.svc, &a, "kb").await;
    let s = h.svc.stats(&a, "kb").await.unwrap();
    assert_eq!(
        (s.indexes[0].indexed_vectors, s.indexes[0].canonical_vectors),
        (298, 298)
    );
    let r = h.svc.search(&a, "kb", query("emb", &target, 298)).await.unwrap();
    assert!(r.hits.iter().all(|hit| hit.metadata.as_ref().unwrap()["owner"] == "a"));
}

// -------------------------------------------------------------------- hybrid

#[tokio::test]
async fn hybrid_search_fuses_keyword_and_vector_rankings() {
    let h = harness().await;
    let ctx = admin();
    create(&h.svc, &ctx, "kb", vec![field("emb", 3, Metric::Cosine)], None).await;
    let docs = [
        (
            "rust",
            [1.0, 0.0, 0.0],
            "Rust ownership and borrowing explained",
            "lang",
        ),
        ("sqlite", [0.0, 1.0, 0.0], "SQLite full-text search with FTS5", "db"),
        (
            "vectors",
            [0.9, 0.1, 0.0],
            "Approximate nearest neighbour vector search",
            "db",
        ),
        ("cooking", [0.0, 0.0, 1.0], "A recipe for bread", "food"),
        ("both", [0.95, 0.05, 0.0], "Vector search inside SQLite", "db"),
    ];
    let items = docs
        .iter()
        .map(|(id, v, text, cat)| {
            let mut it = item(id, "emb", v, json!({"category": cat}));
            it.text = Some((*text).into());
            it
        })
        .collect();
    h.svc.upsert(&ctx, "kb", items).await.unwrap();
    let req = |vector: Option<[f64; 3]>, text: Option<&str>| HybridRequest {
        field: None,
        vector: vector.map(|v| v.to_vec()),
        text: text.map(str::to_owned),
        text_operator: TextOperator::Or,
        k: 3,
        candidates: None,
        rrf_k: None,
        weights: None,
        filter: None,
        params: Default::default(),
        filter_mode: FilterMode::Auto,
        include_vectors: false,
        include_metadata: true,
        include_text: true,
    };
    let hid = |r: &HybridResponse| r.hits.iter().map(|x| x.id.clone()).collect::<Vec<_>>();

    // Keyword only: BM25 ranking, words matched literally.
    let r = h.svc.hybrid(&ctx, "kb", req(None, Some("sqlite"))).await.unwrap();
    assert_eq!(r.text_candidates, 2);
    assert_eq!(r.vector_strategy, None);
    assert_eq!(
        hid(&r).into_iter().collect::<HashSet<_>>(),
        ["sqlite", "both"].map(String::from).into()
    );
    let r = h.svc.hybrid(&ctx, "kb", req(None, Some("OR NEAR NOT"))).await.unwrap();
    assert!(r.hits.is_empty(), "FTS5 operators are literal words, found nowhere");
    let r = h.svc.hybrid(&ctx, "kb", req(None, Some("NOT -bread*"))).await.unwrap();
    assert_eq!(hid(&r), vec!["cooking"], "NOT and - do not negate");

    // Both: the document that ranks well on both sides wins.
    let r = h
        .svc
        .hybrid(&ctx, "kb", req(Some([1.0, 0.0, 0.0]), Some("sqlite search")))
        .await
        .unwrap();
    assert_eq!(r.hits[0].id, "both");
    let top = &r.hits[0];
    assert!(top.vector_rank.is_some() && top.text_rank.is_some());
    let expected =
        1.0 / (60.0 + f64::from(top.vector_rank.unwrap())) + 1.0 / (60.0 + f64::from(top.text_rank.unwrap()));
    assert!((top.score - expected).abs() < 1e-12);

    // Weights: text only.
    let mut text_only = req(Some([1.0, 0.0, 0.0]), Some("bread"));
    text_only.weights = Some(HybridWeights { vector: 0.0, text: 1.0 });
    let r = h.svc.hybrid(&ctx, "kb", text_only).await.unwrap();
    assert_eq!(r.hits[0].id, "cooking");

    // AND operator and a metadata filter applied to both sides.
    let mut and = req(Some([1.0, 0.0, 0.0]), Some("vector search"));
    and.text_operator = TextOperator::And;
    and.filter = Some("category = 'db'".into());
    let r = h.svc.hybrid(&ctx, "kb", and).await.unwrap();
    assert!(r.hits.iter().all(|x| x.metadata.as_ref().unwrap()["category"] == "db"));
    assert_eq!(r.text_candidates, 2, "vectors + both contain both words");

    // Configurable RRF constant.
    let mut k1 = req(Some([1.0, 0.0, 0.0]), Some("sqlite"));
    k1.rrf_k = Some(1.0);
    let r = h.svc.hybrid(&ctx, "kb", k1).await.unwrap();
    assert!(r.hits[0].score > 0.5);

    for bad in [req(None, None), req(None, Some("  "))] {
        assert!(h.svc.hybrid(&ctx, "kb", bad).await.is_err());
    }
    let mut bad = req(Some([1.0, 0.0, 0.0]), None);
    bad.weights = Some(HybridWeights { vector: 0.0, text: 0.0 });
    assert!(h.svc.hybrid(&ctx, "kb", bad).await.is_err());
}

// ------------------------------------------------------------- SPEC #23

async fn queries(h: &Harness, ctx: &AccessContext, qs: &[Vec<f32>]) -> Vec<Vec<(String, f32)>> {
    let mut out = Vec::new();
    for q in qs {
        let r = h.svc.search(ctx, "docs", query("emb", q, 10)).await.unwrap();
        assert_eq!(r.strategy, Strategy::Ann);
        out.push(r.hits.iter().map(|x| (x.id.clone(), x.distance)).collect());
    }
    out
}

fn same(a: &[Vec<(String, f32)>], b: &[Vec<(String, f32)>]) {
    assert_eq!(a.len(), b.len());
    for (x, y) in a.iter().zip(b) {
        let xi: Vec<&String> = x.iter().map(|p| &p.0).collect();
        let yi: Vec<&String> = y.iter().map(|p| &p.0).collect();
        assert_eq!(xi, yi);
        for (p, q) in x.iter().zip(y) {
            assert!((p.1 - q.1).abs() < 1e-5);
        }
    }
}

fn last_rebuild(s: &CollectionStats) -> Option<String> {
    s.indexes[0].last_rebuild.as_ref().map(|r| r.reason.clone())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spec23_rebuild_after_deleting_index_files_returns_identical_results() {
    let h = harness_with(small(10)).await;
    let ctx = admin();
    create(&h.svc, &ctx, "docs", vec![field("emb", 24, Metric::Cosine)], None).await;
    let vectors = fill(&h.svc, &ctx, "docs", "emb", 3000, 24, 5).await;
    let qs: Vec<Vec<f32>> = vectors.iter().step_by(150).cloned().collect();
    let before = queries(&h, &ctx, &qs).await;

    // Clean restart: the saved file is loaded, not rebuilt.
    let h = h.restart(small(10)).await;
    wait_ready(&h.svc, &ctx, "docs").await;
    let s = h.svc.stats(&ctx, "docs").await.unwrap();
    assert_eq!(last_rebuild(&s), None, "loaded from disk");
    same(&before, &queries(&h, &ctx, &qs).await);

    // Delete every derived file: rebuilt from SQLite, same answers.
    h.svc.shutdown().await;
    std::fs::remove_dir_all(index_dir(h.dir.path())).unwrap();
    let h = h.reopen_without_shutdown(small(10)).await;
    h.svc.start_background();
    wait_ready(&h.svc, &ctx, "docs").await;
    let s = h.svc.stats(&ctx, "docs").await.unwrap();
    assert_eq!(last_rebuild(&s).as_deref(), Some("missing"));
    assert_eq!(s.indexes[0].indexed_vectors, 3000);
    assert!(!s.indexes[0].dirty, "rebuilt index was saved");
    same(&before, &queries(&h, &ctx, &qs).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn corrupt_stale_and_foreign_version_files_are_rebuilt() {
    let h = harness_with(small(10)).await;
    let ctx = admin();
    create(&h.svc, &ctx, "docs", vec![field("emb", 8, Metric::L2)], None).await;
    let vectors = fill(&h.svc, &ctx, "docs", "emb", 1500, 8, 9).await;
    let qs: Vec<Vec<f32>> = vectors.iter().step_by(300).cloned().collect();
    let before = queries(&h, &ctx, &qs).await;
    h.svc.shutdown().await;
    let dir = index_dir(h.dir.path());
    let coll_dir = std::fs::read_dir(&dir).unwrap().next().unwrap().unwrap().path();
    let file = coll_dir.join("emb.usearch");
    let manifest = coll_dir.join("emb.manifest.json");

    // 1. Corrupt bytes in the middle of the index file.
    let mut bytes = std::fs::read(&file).unwrap();
    let mid = bytes.len() / 2;
    for b in &mut bytes[mid..mid + 64] {
        *b ^= 0x5a;
    }
    std::fs::write(&file, &bytes).unwrap();
    let h = h.reopen_without_shutdown(small(10)).await;
    wait_ready(&h.svc, &ctx, "docs").await;
    let s = h.svc.stats(&ctx, "docs").await.unwrap();
    assert_eq!(last_rebuild(&s).as_deref(), Some("corrupt"));
    same(&before, &queries(&h, &ctx, &qs).await);

    // 2. A file written by another engine version.
    h.svc.shutdown().await;
    let mut m: serde_json::Value = serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
    m["engine_version"] = json!("1.0.0");
    std::fs::write(&manifest, serde_json::to_vec(&m).unwrap()).unwrap();
    let h = h.reopen_without_shutdown(small(10)).await;
    wait_ready(&h.svc, &ctx, "docs").await;
    let s = h.svc.stats(&ctx, "docs").await.unwrap();
    assert_eq!(last_rebuild(&s).as_deref(), Some("version_mismatch"));
    same(&before, &queries(&h, &ctx, &qs).await);

    // 3. A crash after writes that were never saved: the file is stale.
    h.svc.shutdown().await;
    let h = h.reopen_without_shutdown(small(10)).await;
    h.svc
        .upsert(&ctx, "docs", vec![item("late", "emb", &[9.0; 8], json!({}))])
        .await
        .unwrap();
    h.svc.delete_items(&ctx, "docs", vec!["v0".into()]).await.unwrap();
    let h = h.reopen_without_shutdown(small(10)).await; // no save
    wait_ready(&h.svc, &ctx, "docs").await;
    let s = h.svc.stats(&ctx, "docs").await.unwrap();
    assert_eq!(last_rebuild(&s).as_deref(), Some("stale"));
    assert_eq!(s.indexes[0].indexed_vectors, 1500);
    let r = h.svc.search(&ctx, "docs", query("emb", &[9.0; 8], 1)).await.unwrap();
    assert_eq!(ids(&r), vec!["late"]);
    let r = h.svc.search(&ctx, "docs", query("emb", &vectors[0], 1)).await.unwrap();
    assert_ne!(ids(&r), vec!["v0"], "deleted item is gone from the rebuilt index");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn searches_fall_back_to_exact_while_the_index_is_rebuilt() {
    let h = harness_with(small(10)).await;
    let ctx = admin();
    create(&h.svc, &ctx, "docs", vec![field("emb", 16, Metric::L2)], None).await;
    let vectors = fill(&h.svc, &ctx, "docs", "emb", 4000, 16, 13).await;
    h.svc.shutdown().await;
    std::fs::remove_dir_all(index_dir(h.dir.path())).unwrap();

    let h = h.reopen_without_shutdown(small(10)).await;
    let r = h.svc.search(&ctx, "docs", query("emb", &vectors[42], 5)).await.unwrap();
    assert_eq!(
        r.strategy,
        Strategy::ExactFallback,
        "no index yet: exact, still correct"
    );
    assert_eq!(r.hits[0].id, "v42");
    wait_ready(&h.svc, &ctx, "docs").await;

    // Too large to scan while rebuilding: a clear 503-class error.
    h.svc.shutdown().await;
    std::fs::remove_dir_all(index_dir(h.dir.path())).unwrap();
    let tight = VectorConfig {
        max_exact_scan: 100,
        ..small(10)
    };
    let h = h.reopen_without_shutdown(tight).await;
    match h.svc.search(&ctx, "docs", query("emb", &vectors[42], 5)).await {
        Err(VectorError::IndexNotReady(..)) => {}
        Ok(r) => assert_eq!(r.strategy, Strategy::Ann, "rebuild already finished"),
        Err(e) => panic!("{e:?}"),
    }
    wait_ready(&h.svc, &ctx, "docs").await;
    assert_eq!(
        h.svc
            .search(&ctx, "docs", query("emb", &vectors[42], 1))
            .await
            .unwrap()
            .hits[0]
            .id,
        "v42"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn manual_rebuild_reports_progress_and_keeps_writes_made_meanwhile() {
    let h = harness_with(VectorConfig {
        batch: 200,
        ..small(10)
    })
    .await;
    let ctx = admin();
    create(&h.svc, &ctx, "docs", vec![field("emb", 16, Metric::L2)], None).await;
    let vectors = fill(&h.svc, &ctx, "docs", "emb", 6000, 16, 21).await;
    let started = h.svc.rebuild(&ctx, "docs", None).await.unwrap();
    assert_eq!(started[0].state, IndexState::Building);
    assert_eq!(started[0].serving, "ann", "the old index serves meanwhile");
    assert!(started[0].progress.is_some(), "a running rebuild reports progress");
    // Writes while the rebuild runs.
    for i in 0..20 {
        h.svc
            .upsert(
                &ctx,
                "docs",
                vec![item(&format!("new{i}"), "emb", &[100.0 + i as f32; 16], json!({}))],
            )
            .await
            .unwrap();
    }
    h.svc
        .delete_items(&ctx, "docs", (0..50).map(|i| format!("v{i}")).collect())
        .await
        .unwrap();
    h.svc
        .upsert(&ctx, "docs", vec![item("v60", "emb", &[-50.0; 16], json!({}))])
        .await
        .unwrap();
    for _ in 0..2000 {
        let s = h.svc.stats(&ctx, "docs").await.unwrap();
        if let Some(p) = s.indexes[0].progress {
            assert!(p.done <= p.total, "{p:?}");
        }
        if s.indexes[0].state == IndexState::Ready {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    wait_ready(&h.svc, &ctx, "docs").await;
    let s = h.svc.stats(&ctx, "docs").await.unwrap();
    assert_eq!(last_rebuild(&s).as_deref(), Some("manual"));
    assert_eq!(s.indexes[0].indexed_vectors, s.indexes[0].canonical_vectors);
    assert_eq!(s.indexes[0].canonical_vectors, 6000 + 20 - 50);
    let r = h.svc.search(&ctx, "docs", query("emb", &[119.0; 16], 1)).await.unwrap();
    assert_eq!(ids(&r), vec!["new19"]);
    let r = h.svc.search(&ctx, "docs", query("emb", &[-50.0; 16], 1)).await.unwrap();
    assert_eq!(ids(&r), vec!["v60"], "replaced vector is the new one");
    let r = h.svc.search(&ctx, "docs", query("emb", &vectors[3], 3)).await.unwrap();
    assert!(!ids(&r).contains(&"v3".to_owned()), "deleted during rebuild");
}

#[tokio::test]
async fn fields_and_collections_can_be_added_and_dropped() {
    let h = harness().await;
    let ctx = admin();
    create(&h.svc, &ctx, "docs", vec![field("a", 2, Metric::L2)], None).await;
    h.svc
        .upsert(&ctx, "docs", vec![item("x", "a", &[1.0, 1.0], json!({}))])
        .await
        .unwrap();
    h.svc.add_field(&ctx, "docs", field("b", 3, Metric::Dot)).await.unwrap();
    let mut it = item("y", "a", &[2.0, 2.0], json!({}));
    it.vectors.insert("b".into(), vec![1.0, 2.0, 3.0]);
    h.svc.upsert(&ctx, "docs", vec![it]).await.unwrap();
    assert!(matches!(
        h.svc
            .search(
                &ctx,
                "docs",
                SearchRequest {
                    field: None,
                    ..query("a", &[1.0, 1.0], 1)
                }
            )
            .await,
        Err(VectorError::Invalid {
            code: "field_required",
            ..
        })
    ));
    let r = h
        .svc
        .search(&ctx, "docs", query("b", &[1.0, 2.0, 3.0], 5))
        .await
        .unwrap();
    assert_eq!(ids(&r), vec!["y"], "x has no vector in b");
    let h = h.restart(VectorConfig::default()).await;
    assert_eq!(h.svc.list_fields(&ctx, "docs").await.unwrap().len(), 2);
    h.svc.delete_field(&ctx, "docs", "b").await.unwrap();
    let y = h.svc.get_item(&ctx, "docs", "y").await.unwrap();
    assert!(!y.vectors.contains_key("b"), "vectors of a dropped field are gone");
    assert!(matches!(
        h.svc.search(&ctx, "docs", query("b", &[1.0, 2.0, 3.0], 5)).await,
        Err(VectorError::FieldNotFound(_))
    ));
    h.svc.delete_collection(&ctx, "docs").await.unwrap();
    assert!(h.svc.list_collections(&ctx).await.unwrap().is_empty());
    assert_eq!(
        std::fs::read_dir(index_dir(h.dir.path())).unwrap().count(),
        0,
        "files removed"
    );
    create(&h.svc, &ctx, "docs", vec![field("a", 2, Metric::L2)], None).await;
    assert_eq!(
        h.svc.get_collection(&ctx, "docs").await.unwrap().items,
        0,
        "no leftovers"
    );
}

// -------------------------------------------------------------------- recall

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recall_at_10_on_10k_generated_vectors_is_at_least_095() {
    let started = Instant::now();
    let h = harness().await;
    let ctx = admin();
    create(&h.svc, &ctx, "bench", vec![field("emb", 32, Metric::Cosine)], None).await;
    fill(&h.svc, &ctx, "bench", "emb", 10_000, 32, 42).await;
    let indexed = started.elapsed();
    let report = h
        .svc
        .recall(
            &ctx,
            "bench",
            RecallRequest {
                field: None,
                samples: Some(200),
                k: Some(10),
            },
        )
        .await
        .unwrap();
    let total = started.elapsed();
    println!(
        "recall@10 = {:.4} over {} queries; insert+index {indexed:?}, ann {:.1} ms, exact {:.1} ms, total {total:?}",
        report.recall, report.samples, report.ann_ms, report.exact_ms
    );
    assert!(report.recall >= 0.95, "recall@10 = {}", report.recall);
    assert_eq!(report.samples, 200);
    assert!(total < Duration::from_secs(120), "took {total:?}");
    assert!(report.ann_ms < report.exact_ms, "ANN is faster than brute force");
    let s = h.svc.stats(&ctx, "bench").await.unwrap();
    assert!(s.indexes[0].last_recall.is_some());
    assert_eq!(s.indexes[0].indexed_vectors, 10_000);
    assert!(s.indexes[0].memory_bytes > 10_000 * 32 * 4);
}
