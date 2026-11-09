//! Records engine: create, get by id, filtered list (ADR-0003).
#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used)]

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use forgebase_bench::Rig;
use forgebase_data::schema::Collection;
use forgebase_data::{GetOptions, IndexDefinition, ListQuery};
use serde_json::json;

fn bench(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let rig = rt.block_on(Rig::open());
    let data = rt.block_on(rig.data());
    let sys = Rig::system();
    let ids: Vec<String> = rt.block_on(async {
        let coll: Collection = serde_json::from_value(json!({
            "name": "notes",
            "fields": [
                {"name": "title", "type": "text"},
                {"name": "n", "type": "number"}
            ]
        }))
        .unwrap();
        data.create_collection(&sys, coll).await.unwrap();
        let idx: IndexDefinition = serde_json::from_value(json!({
            "name": "notes_n", "collection": "notes", "keys": [{"path": "n"}]
        }))
        .unwrap();
        data.create_index(&sys, idx).await.unwrap();
        let mut ids = Vec::new();
        for chunk in 0..5 {
            let rows = (0..1_000)
                .map(|j| {
                    let i = chunk * 1_000 + j;
                    json!({"title": format!("note {i}"), "n": i})
                })
                .collect();
            let made = data.create_records(&sys, "notes", rows).await.unwrap();
            ids.extend(made.into_iter().map(|r| r["id"].as_str().unwrap().to_owned()));
        }
        ids
    });

    // Diagnostic: the plan of the benchmarked list query (printed once).
    rt.block_on(async {
        let q = ListQuery {
            filter: Some("n >= 1000 && n < 1100".into()),
            sort: Some("n".into()),
            per_page: Some(20),
            ..ListQuery::default()
        };
        let plan = data.explain_list(&sys, "notes", &q).await.unwrap();
        eprintln!(
            "PLAN indexes_used={:?} full_scans={:?}\nPLAN sql={}",
            plan.indexes_used, plan.full_scans, plan.sql
        );
    });

    let mut g = c.benchmark_group("records");
    g.sample_size(30);
    g.throughput(Throughput::Elements(1));
    let mut n = 10_000_i64;
    g.bench_function("create", |b| {
        b.to_async(&rt).iter(|| {
            n += 1;
            let data = data.clone();
            let sys = sys.clone();
            async move {
                data.create_record(&sys, "notes", json!({"title": "bench", "n": n}))
                    .await
                    .unwrap();
            }
        });
    });
    let mut i = 0_usize;
    g.bench_function("get_by_id", |b| {
        b.to_async(&rt).iter(|| {
            i = (i + 7919) % ids.len();
            let (data, sys, id) = (data.clone(), sys.clone(), ids[i].clone());
            async move {
                data.get_record(&sys, "notes", &id, &GetOptions::default())
                    .await
                    .unwrap();
            }
        });
    });
    g.bench_function("list_indexed_filter_page_20", |b| {
        b.to_async(&rt).iter(|| {
            let (data, sys) = (data.clone(), sys.clone());
            async move {
                let q = ListQuery {
                    filter: Some("n >= 1000 && n < 1100".into()),
                    sort: Some("n".into()),
                    per_page: Some(20),
                    ..ListQuery::default()
                };
                let r = data.list_records(&sys, "notes", &q).await.unwrap();
                assert!(!r.items.is_empty());
            }
        });
    });
    g.bench_function("list_indexed_filter_page_20_skip_total", |b| {
        b.to_async(&rt).iter(|| {
            let (data, sys) = (data.clone(), sys.clone());
            async move {
                let q = ListQuery {
                    filter: Some("n >= 1000 && n < 1100".into()),
                    sort: Some("n".into()),
                    per_page: Some(20),
                    skip_total: true,
                    ..ListQuery::default()
                };
                let r = data.list_records(&sys, "notes", &q).await.unwrap();
                assert!(!r.items.is_empty());
            }
        });
    });
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
