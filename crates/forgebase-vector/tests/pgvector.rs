//! The pgvector engine (ADR-0029 §1.4, FB-162): one partial HNSW index per
//! field with the metric and element-type mapping, the search settings
//! applied before the index scan, the plan using the index, concurrent
//! rebuilds, `vector.convert` both ways, reconcile at open, the layout check
//! with its fallback, and a recall + latency bench (`--ignored`).
//!
//! Tests that need pgvector return early elsewhere; the conversion tests
//! also check what SQLite and PostgreSQL without pgvector answer.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::{Duration, Instant};

use common::*;
use forgebase_core::config::ExtensionMode;
use forgebase_data::AccessContext;
use forgebase_db::{Backend, Statement, TxMode, Value};
use forgebase_vector::model::*;
use forgebase_vector::{CONVERT_JOB, VectorConfig, VectorError, register_jobs};
use serde_json::json;

fn cfg(exact_threshold: u32) -> VectorConfig {
    VectorConfig {
        exact_threshold,
        ..VectorConfig::default()
    }
}

async fn scalar(h: &Harness, sql: &str) -> i64 {
    h.db.fetch_all(&Statement::raw(sql))
        .await
        .unwrap()
        .scalar()
        .and_then(Value::as_i64)
        .unwrap_or(0)
}

async fn text_rows(h: &Harness, sql: &str) -> Vec<String> {
    h.db.fetch_all(&Statement::raw(sql))
        .await
        .unwrap()
        .rows
        .iter()
        .map(|r| r[0].as_str().unwrap_or_default().to_owned())
        .collect()
}

async fn exec(h: &Harness, sql: &str) {
    let mut tx = h.db.begin(TxMode::SchemaChange).await.unwrap();
    tx.execute(&Statement::raw(sql)).await.unwrap();
    tx.commit().await.unwrap();
}

async fn meta(h: &Harness) -> Option<String> {
    h.db.fetch_all(&Statement::raw(
        "SELECT value FROM _forgebase_meta WHERE key = 'ext.vector.engine'",
    ))
    .await
    .unwrap()
    .rows
    .first()
    .and_then(|r| r[0].as_str().map(str::to_owned))
}

fn spec(name: &str, dimension: u32, metric: Metric, q: Quantization) -> FieldSpec {
    FieldSpec {
        quantization: Some(q),
        ..field(name, dimension, metric)
    }
}

/// A unit-ish vector of `dim` from `rng` (never all zero).
fn vector(rng: &mut Rng, dim: usize) -> Vec<f32> {
    let mut v = rng.vector(dim);
    v[0] += 2.0;
    v
}

// --------------------------------------------------------------- indexes

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn each_field_gets_a_partial_hnsw_index_of_its_metric_and_element_type() {
    let h = harness_with(cfg(0)).await;
    if !on_pgvector(&h.svc).await {
        return;
    }
    let ctx = admin();
    let fields = vec![
        spec("cos", 16, Metric::Cosine, Quantization::F32),
        spec("half", 8, Metric::L2, Quantization::F16),
        spec("byte", 8, Metric::Dot, Quantization::I8),
        spec("wide", 3000, Metric::L2, Quantization::F32),
        spec("huge", 4096, Metric::Cosine, Quantization::F32),
    ];
    create(&h.svc, &ctx, "docs", fields, None).await;
    let defs = text_rows(
        &h,
        "SELECT indexdef FROM pg_indexes WHERE indexname LIKE 'fbv\\_%' ORDER BY indexdef",
    )
    .await;
    assert_eq!(defs.len(), 4, "every field up to 4000 dimensions: {defs:#?}");
    let has = |needle: &str| defs.iter().any(|d| d.contains(needle));
    for (cast, ops, field) in [
        ("vector(16)", "vector_cosine_ops", "cos"),
        ("halfvec(8)", "halfvec_l2_ops", "half"),
        ("halfvec(8)", "halfvec_ip_ops", "byte"),
        ("halfvec(3000)", "halfvec_l2_ops", "wide"),
    ] {
        assert!(
            defs.iter().any(|d| d.contains("USING hnsw")
                && d.contains(cast)
                && d.contains(ops)
                && d.contains(&format!("field = '{field}'::text"))),
            "{field}: {defs:#?}"
        );
    }
    assert!(has("m='16'") && has("ef_construction='128'"), "{defs:#?}");
    assert!(defs.iter().all(|d| d.contains("collection_id = ")), "partial per collection");

    let mut rng = Rng::new(5);
    let items: Vec<UpsertItem> = (0..300)
        .map(|i| UpsertItem {
            id: Some(format!("i{i}")),
            vectors: [
                ("cos", vector(&mut rng, 16)),
                ("half", vector(&mut rng, 8)),
                ("byte", vector(&mut rng, 8)),
                ("wide", vector(&mut rng, 3000)),
                ("huge", vector(&mut rng, 4096)),
            ]
            .into_iter()
            .map(|(f, v)| (f.to_owned(), v.iter().map(|x| f64::from(*x)).collect()))
            .collect(),
            metadata: json!({"n": i}).as_object().cloned(),
            text: None,
        })
        .collect();
    let stored = items[7].clone();
    h.svc.upsert(&ctx, "docs", items).await.unwrap();
    // The canonical vector is float4 whatever the index type.
    let got = h.svc.get_item(&ctx, "docs", "i7").await.unwrap();
    for (f, v) in &stored.vectors {
        let want: Vec<f32> = v.iter().map(|x| *x as f32).collect();
        assert_eq!(got.vectors[f], want, "{f}");
    }
    for f in ["cos", "half", "byte", "wide"] {
        let q: Vec<f32> = stored.vectors[f].iter().map(|x| *x as f32).collect();
        let r = h.svc.search(&ctx, "docs", query(f, &q, 5)).await.unwrap();
        assert_eq!(r.strategy, Strategy::Ann, "{f}");
        assert_eq!(r.hits[0].id, "i7", "{f}: a stored vector is its own nearest");
        // Distances are the float4 ones, also through a halfvec index.
        let mut exact = query(f, &q, 5);
        exact.exact = true;
        let e = h.svc.search(&ctx, "docs", exact).await.unwrap();
        assert!((r.hits[0].distance - e.hits[0].distance).abs() < 1e-6, "{f}");
    }
    let q: Vec<f32> = stored.vectors["huge"].iter().map(|x| *x as f32).collect();
    let r = h.svc.search(&ctx, "docs", query("huge", &q, 3)).await.unwrap();
    assert_eq!((r.strategy, r.hits[0].id.as_str()), (Strategy::Exact, "i7"), "over 4000: exact");
    let s = h.svc.stats(&ctx, "docs").await.unwrap();
    for st in &s.indexes {
        assert_eq!(st.engine, "pgvector");
        assert_eq!(st.state, IndexState::Ready, "{st:?}");
        if st.field == "huge" {
            assert_eq!((st.serving.as_str(), st.indexed_vectors), ("exact", 0));
            assert!(st.reason.as_deref().unwrap().contains("4000"), "{st:?}");
        } else {
            assert_eq!((st.serving.as_str(), st.indexed_vectors), ("ann", 300), "{st:?}");
            assert!(st.memory_bytes > 0);
        }
    }
    // No index files: the index is in the database.
    assert_eq!(std::fs::read_dir(index_dir(h.dir.path())).unwrap().count(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn searches_run_on_the_index_with_their_settings() {
    let h = harness_with(cfg(10)).await;
    if !on_pgvector(&h.svc).await {
        return;
    }
    let ctx = admin();
    create(&h.svc, &ctx, "docs", vec![field("emb", 8, Metric::L2)], None).await;
    let vectors = fill(&h.svc, &ctx, "docs", "emb", 2000, 8, 3).await;
    // The plan of a plain and of a filtered search scans the partial index.
    for filter in [None, Some("category = 'b'")] {
        let mut req = query("emb", &vectors[1], 10);
        req.filter = filter.map(str::to_owned);
        let plan = h.svc.explain_search(&ctx, "docs", req).await.unwrap();
        assert!(
            plan.iter().any(|p| p.starts_with("Index Scan using fbv_")),
            "{filter:?}: {plan:#?}"
        );
    }
    // The settings are made before the scan starts: without an iterative
    // scan pgvector returns at most ef_search rows.
    let cid = scalar(&h, "SELECT id FROM _forgebase_vector_collections WHERE name = 'docs'").await;
    let q = format!(
        "[{}]",
        vectors[1].iter().map(f32::to_string).collect::<Vec<_>>().join(",")
    );
    for (ef, want) in [(5, 5), (100, 50)] {
        let n = scalar(
            &h,
            &format!(
                "SELECT COUNT(*) FROM (SELECT set_config('hnsw.ef_search', '{ef}', true) AS ef, \
                 set_config('hnsw.iterative_scan', 'off', true) AS it) cfg CROSS JOIN LATERAL (\
                 SELECT v.key FROM _forgebase_vector_values v WHERE v.collection_id = {cid} AND v.field = 'emb' \
                 AND cfg.ef IS NOT NULL AND cfg.it IS NOT NULL \
                 ORDER BY CAST(v.vec AS forgebase.vector(8)) OPERATOR(forgebase.<->) CAST('{q}' AS forgebase.vector(8)) \
                 LIMIT 50) s"
            ),
        )
        .await;
        assert_eq!(n, want, "ef_search = {ef}");
    }
    // A selective filter: the iterative scan (or the exact fallback) still
    // returns k matches, never a non-match.
    let mut req = query("emb", &vectors[1], 20);
    req.filter = Some("n >= 1900".into());
    for mode in [FilterMode::Pre, FilterMode::Post] {
        req.filter_mode = mode;
        let r = h.svc.search(&ctx, "docs", req.clone()).await.unwrap();
        assert_eq!(r.hits.len(), 20, "{mode:?}");
        assert!(
            r.hits.iter().all(|x| x.metadata.as_ref().unwrap()["n"].as_i64().unwrap() >= 1900),
            "{mode:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rebuild_reindexes_concurrently() {
    let h = harness_with(cfg(10)).await;
    if !on_pgvector(&h.svc).await {
        return;
    }
    let ctx = admin();
    create(&h.svc, &ctx, "docs", vec![field("emb", 16, Metric::Cosine)], None).await;
    let vectors = fill(&h.svc, &ctx, "docs", "emb", 3000, 16, 8).await;
    let oid = "SELECT c.oid::bigint FROM pg_class c WHERE c.relname LIKE 'fbv\\_%' AND c.relkind = 'i'";
    let before = scalar(&h, oid).await;
    let started = h.svc.rebuild(&ctx, "docs", None).await.unwrap();
    assert_eq!((started[0].state, started[0].serving.as_str()), (IndexState::Building, "ann"));
    // Writes go on while it runs.
    h.svc
        .upsert(&ctx, "docs", vec![item("new", "emb", &[5.0; 16], json!({}))])
        .await
        .unwrap();
    wait_ready(&h.svc, &ctx, "docs").await;
    let s = h.svc.stats(&ctx, "docs").await.unwrap();
    let st = &s.indexes[0];
    assert_eq!(st.last_rebuild.as_ref().unwrap().reason, "manual");
    assert_eq!(st.indexed_vectors, 3001);
    assert_ne!(scalar(&h, oid).await, before, "REINDEX CONCURRENTLY swapped in a new index");
    assert_eq!(scalar(&h, "SELECT COUNT(*) FROM pg_class WHERE relname LIKE 'fbv\\_%'").await, 1);
    let r = h.svc.search(&ctx, "docs", query("emb", &vectors[9], 1)).await.unwrap();
    assert_eq!((r.strategy, r.hits[0].id.as_str()), (Strategy::Ann, "v9"));
    let r = h.svc.search(&ctx, "docs", query("emb", &[5.0; 16], 1)).await.unwrap();
    assert_eq!(r.hits[0].id, "new");
}

// ------------------------------------------------------------- conversion

async fn vectors_of(h: &Harness, ctx: &AccessContext, ids: &[&str]) -> Vec<Item> {
    let mut out = Vec::new();
    for id in ids {
        out.push(h.svc.get_item(ctx, "docs", id).await.unwrap());
    }
    out
}

async fn exact_ids(h: &Harness, ctx: &AccessContext, field: &str, q: &[f32]) -> Vec<String> {
    let mut req = query(field, q, 10);
    req.exact = true;
    ids(&h.svc.search(ctx, "docs", req).await.unwrap())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn convert_moves_the_vectors_both_ways() {
    let h = harness_with(cfg(10)).await;
    let ctx = admin();
    // Refused for tenants and non-administrators everywhere.
    assert!(matches!(
        h.svc.convert(&tenant("t1"), VectorEngine::Portable).await,
        Err(VectorError::Invalid {
            code: "invalid_request",
            ..
        })
    ));
    assert!(matches!(
        h.svc
            .convert(&AccessContext::user("users", "u1"), VectorEngine::Portable)
            .await,
        Err(VectorError::Forbidden(_))
    ));
    let start = h.svc.storage_engine().await;
    if start == VectorEngine::Portable && !pgvector_installed(&h.db).await {
        // An engine without pgvector (or SQLite): auto prepares nothing.
        setup_extensions(&h.db, ExtensionMode::Auto).await;
    }
    if start == VectorEngine::Portable && !pgvector_installed(&h.db).await {
        let err = h.svc.convert(&ctx, VectorEngine::Pgvector).await.unwrap_err();
        assert!(
            matches!(err, VectorError::Invalid { code: "pgvector_unavailable", .. }),
            "{err:?}"
        );
        let r = h.svc.convert(&ctx, VectorEngine::Portable).await.unwrap();
        assert_eq!((r.from, r.to, r.vectors), (VectorEngine::Portable, VectorEngine::Portable, 0));
        return;
    }
    create(
        &h.svc,
        &ctx,
        "docs",
        vec![
            field("emb", 8, Metric::Cosine),
            spec("half", 4, Metric::L2, Quantization::F16),
        ],
        None,
    )
    .await;
    let mut rng = Rng::new(17);
    let items: Vec<UpsertItem> = (0..400)
        .map(|i| UpsertItem {
            id: Some(format!("v{i}")),
            vectors: [("emb", vector(&mut rng, 8)), ("half", vector(&mut rng, 4))]
                .into_iter()
                .map(|(f, v)| (f.to_owned(), v.iter().map(|x| f64::from(*x)).collect()))
                .collect(),
            metadata: json!({"n": i}).as_object().cloned(),
            text: Some(format!("document {i}")),
        })
        .collect();
    h.svc.upsert(&ctx, "docs", items).await.unwrap();
    let sample = ["v0", "v7", "v399"];
    let before = vectors_of(&h, &ctx, &sample).await;
    let q: Vec<f32> = before[1].vectors["emb"].clone();
    let truth = exact_ids(&h, &ctx, "emb", &q).await;

    let other = match start {
        VectorEngine::Pgvector => VectorEngine::Portable,
        VectorEngine::Portable => VectorEngine::Pgvector,
    };
    let mut h = h;
    for (from, to) in [(start, other), (other, start)] {
        let r = h.svc.convert(&ctx, to).await.unwrap();
        assert_eq!((r.from, r.to, r.vectors, r.indexes), (from, to, 800, 2), "{r:?}");
        assert_eq!(h.svc.storage_engine().await, to);
        let (pinned, vec_rows, bytea_rows, hnsw) = match to {
            VectorEngine::Pgvector => ("extension", 800, 0, 2),
            VectorEngine::Portable => ("portable", 0, 800, 0),
        };
        assert_eq!(meta(&h).await.as_deref(), Some(pinned));
        assert_eq!(
            scalar(&h, "SELECT COUNT(*) FROM _forgebase_vector_values WHERE vec IS NOT NULL").await,
            vec_rows
        );
        assert_eq!(
            scalar(&h, "SELECT COUNT(*) FROM _forgebase_vector_values WHERE vector IS NOT NULL").await,
            bytea_rows
        );
        assert_eq!(scalar(&h, "SELECT COUNT(*) FROM pg_indexes WHERE indexname LIKE 'fbv\\_%'").await, hnsw);
        wait_ready(&h.svc, &ctx, "docs").await;
        // The same vectors and answers on either engine.
        let after = vectors_of(&h, &ctx, &sample).await;
        for (a, b) in before.iter().zip(&after) {
            assert_eq!(a.vectors, b.vectors);
            assert_eq!((a.version, &a.metadata, &a.text), (b.version, &b.metadata, &b.text));
        }
        assert_eq!(exact_ids(&h, &ctx, "emb", &q).await, truth);
        let r = h.svc.search(&ctx, "docs", query("emb", &q, 10)).await.unwrap();
        assert_eq!((r.strategy, r.hits[0].id.as_str()), (Strategy::Ann, "v7"));
        let s = h.svc.stats(&ctx, "docs").await.unwrap();
        let engine = if to == VectorEngine::Pgvector { "pgvector" } else { "usearch" };
        assert!(s.indexes.iter().all(|i| i.engine == engine && i.indexed_vectors == 400), "{s:?}");
        // Writes after the conversion land in the new storage.
        h.svc
            .upsert(&ctx, "docs", vec![item("late", "emb", &[0.0, 9.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0], json!({}))])
            .await
            .unwrap();
        h.svc.delete_items(&ctx, "docs", vec!["late".into()]).await.unwrap();
        // The next start follows the stored engine.
        h = h.reopen_without_shutdown(cfg(10)).await;
        assert_eq!(h.svc.storage_engine().await, to);
    }
    // Converting to the engine in use moves nothing.
    let r = h.svc.convert(&ctx, start).await.unwrap();
    assert_eq!((r.from, r.to, r.vectors, r.indexes), (start, start, 0, 0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vectors_in_the_other_column_are_moved_at_open() {
    let h = harness_with(cfg(10)).await;
    if !on_pgvector(&h.svc).await {
        return;
    }
    let ctx = admin();
    create(&h.svc, &ctx, "docs", vec![field("emb", 8, Metric::L2)], None).await;
    let vectors = fill(&h.svc, &ctx, "docs", "emb", 500, 8, 4).await;
    let before = h.svc.get_item(&ctx, "docs", "v3").await.unwrap();
    // Portable rows under a pgvector pin (databases set up between FB-161
    // and FB-162): convert away, then put the pin back behind its back.
    h.svc.convert(&ctx, VectorEngine::Portable).await.unwrap();
    exec(&h, "UPDATE _forgebase_meta SET value = 'extension' WHERE key = 'ext.vector.engine'").await;
    let h = h.reopen_without_shutdown(cfg(10)).await;
    assert_eq!(h.svc.storage_engine().await, VectorEngine::Pgvector);
    assert_eq!(
        scalar(&h, "SELECT COUNT(*) FROM _forgebase_vector_values WHERE vector IS NOT NULL").await,
        0
    );
    wait_ready(&h.svc, &ctx, "docs").await;
    let s = h.svc.stats(&ctx, "docs").await.unwrap();
    assert_eq!(s.indexes[0].last_rebuild.as_ref().unwrap().reason, "missing");
    assert_eq!(h.svc.get_item(&ctx, "docs", "v3").await.unwrap().vectors, before.vectors);
    let r = h.svc.search(&ctx, "docs", query("emb", &vectors[3], 1)).await.unwrap();
    assert_eq!((r.strategy, r.hits[0].id.as_str()), (Strategy::Ann, "v3"));

    // pgvector rows under a portable pin (a conversion that stopped before
    // its last batch): moved back, the leftover index dropped.
    exec(&h, "UPDATE _forgebase_meta SET value = 'portable' WHERE key = 'ext.vector.engine'").await;
    let h = h.reopen_without_shutdown(cfg(10)).await;
    assert_eq!(h.svc.storage_engine().await, VectorEngine::Portable);
    assert_eq!(
        scalar(&h, "SELECT COUNT(*) FROM _forgebase_vector_values WHERE vec IS NOT NULL").await,
        0
    );
    assert_eq!(h.svc.get_item(&ctx, "docs", "v3").await.unwrap().vectors, before.vectors);
    let deadline = Instant::now() + Duration::from_secs(30);
    while scalar(&h, "SELECT COUNT(*) FROM pg_indexes WHERE indexname LIKE 'fbv\\_%'").await > 0 {
        assert!(Instant::now() < deadline, "leftover HNSW index not dropped");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    wait_ready(&h.svc, &ctx, "docs").await;
    let r = h.svc.search(&ctx, "docs", query("emb", &vectors[3], 1)).await.unwrap();
    assert_eq!(r.hits[0].id, "v3");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_layout_mismatch_falls_back_to_portable_and_refuses_conversion() {
    let h = harness_with(cfg(10)).await;
    if !on_pgvector(&h.svc).await {
        return;
    }
    let ctx = admin();
    exec(&h, "ALTER TABLE _forgebase_vector_values RENAME COLUMN vec TO vec_moved").await;
    let h = h.reopen_without_shutdown(cfg(10)).await;
    assert_eq!(h.svc.storage_engine().await, VectorEngine::Portable, "falls back with a warning");
    let err = h.svc.convert(&ctx, VectorEngine::Pgvector).await.unwrap_err();
    assert!(
        matches!(&err, VectorError::Invalid { code: "pgvector_unavailable", message } if message.contains("no column vec")),
        "{err:?}"
    );
    // The portable engine works meanwhile.
    create(&h.svc, &ctx, "docs", vec![field("emb", 2, Metric::L2)], None).await;
    h.svc
        .upsert(&ctx, "docs", vec![item("a", "emb", &[1.0, 2.0], json!({}))])
        .await
        .unwrap();
    assert_eq!(h.svc.get_item(&ctx, "docs", "a").await.unwrap().vectors["emb"], vec![1.0, 2.0]);
}

#[tokio::test]
async fn the_convert_job_kind_is_reserved() {
    let h = harness().await;
    let reg = register_jobs(forgebase_jobs::JobRegistry::default(), h.svc.clone());
    assert_eq!(CONVERT_JOB, "vector.convert");
    assert!(
        matches!(reg.access(CONVERT_JOB), Some(forgebase_jobs::KindAccess::Reserved)),
        "only privileged callers may enqueue it"
    );
}

// ------------------------------------------------------------------ bench

/// Recall and latency: 10 000 x 128 cosine, 200 recall samples, 200
/// searches of each strategy. `cargo test -p forgebase-vector --test
/// pgvector -- --ignored --nocapture bench`, on each backend.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "bench: run explicitly"]
async fn bench_recall_and_latency() {
    const N: usize = 10_000;
    const DIM: usize = 128;
    let h = harness_with(cfg(0)).await;
    let ctx = admin();
    let engine = h.svc.storage_engine().await;
    create(&h.svc, &ctx, "bench", vec![field("emb", DIM as u32, Metric::Cosine)], None).await;
    let t = Instant::now();
    let vectors = fill(&h.svc, &ctx, "bench", "emb", N, DIM, 42).await;
    wait_ready(&h.svc, &ctx, "bench").await;
    let load = t.elapsed();
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
    let mut rng = Rng::new(99);
    let lat = |label: &'static str| (label, Vec::<f64>::with_capacity(200));
    let mut runs = vec![lat("ann"), lat("filtered"), lat("exact")];
    for (label, samples) in &mut runs {
        for _ in 0..200 {
            let mut req = query("emb", &rng.vector(DIM), 10);
            match *label {
                "filtered" => req.filter = Some("category = 'b'".into()),
                "exact" => req.exact = true,
                _ => {}
            }
            let t = Instant::now();
            let r = h.svc.search(&ctx, "bench", req).await.unwrap();
            samples.push(t.elapsed().as_secs_f64() * 1000.0);
            assert_eq!(r.hits.len(), 10);
        }
        samples.sort_by(f64::total_cmp);
    }
    let pct = |s: &[f64], p: f64| s[((s.len() - 1) as f64 * p).round() as usize];
    println!(
        "BENCH backend={:?} engine={} n={N} dim={DIM}: load+index {load:?}; recall@10={:.4} (ann {:.1} ms, exact {:.1} ms for 200 queries)",
        h.db.kind(),
        engine.as_str(),
        report.recall,
        report.ann_ms,
        report.exact_ms
    );
    for (label, s) in &runs {
        println!(
            "BENCH {label}: p50 {:.2} ms, p95 {:.2} ms, p99 {:.2} ms",
            pct(s, 0.5),
            pct(s, 0.95),
            pct(s, 0.99)
        );
    }
    drop(vectors);
    assert!(report.recall >= 0.9, "recall@10 = {}", report.recall);
}
