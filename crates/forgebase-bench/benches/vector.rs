//! Vector search: HNSW (approximate) and exact (brute force) top-10 over 10 000 x 128.
#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use forgebase_bench::Rig;
use forgebase_vector::model::{FieldSpec, Metric, NewCollection, SearchRequest, UpsertItem};
use forgebase_vector::{VectorConfig, VectorService};

const DIM: usize = 128;
const N: usize = 10_000;

fn vec_of(seed: usize) -> Vec<f64> {
    // Deterministic pseudo-random components in [-1, 1).
    let mut x = (seed as u64).wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    (0..DIM)
        .map(|_| {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((x >> 33) as f64 / f64::from(1_u32 << 30)) - 1.0
        })
        .collect()
}

fn bench(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let rig = rt.block_on(Rig::open());
    let sys = Rig::system();
    let svc = rt.block_on(async {
        let data = rig.data().await;
        let svc = VectorService::open(data, rig.tmp.path().join("vec"), VectorConfig::default())
            .await
            .unwrap();
        for (name, threshold) in [("hnsw", 0_u32), ("exact", 1_000_000_u32)] {
            svc.create_collection(
                &sys,
                NewCollection {
                    name: name.into(),
                    description: String::new(),
                    fields: vec![FieldSpec {
                        name: "emb".into(),
                        dimension: DIM as u32,
                        metric: Metric::Cosine,
                        quantization: None,
                        connectivity: None,
                        expansion_add: None,
                        expansion_search: None,
                    }],
                    exact_threshold: Some(threshold),
                },
            )
            .await
            .unwrap();
            for chunk in 0..(N / 1000) {
                let items = (0..1000)
                    .map(|j| {
                        let i = chunk * 1000 + j;
                        UpsertItem {
                            id: Some(format!("v{i}")),
                            vectors: [("emb".to_owned(), vec_of(i))].into(),
                            metadata: None,
                            text: None,
                        }
                    })
                    .collect();
                svc.upsert(&sys, name, items).await.unwrap();
            }
            assert!(svc.wait_ready(&sys, name, Duration::from_secs(600)).await.unwrap());
        }
        svc
    });
    let mut g = c.benchmark_group("vector");
    g.sample_size(30);
    g.throughput(Throughput::Elements(1));
    for name in ["hnsw", "exact"] {
        let mut q = 0_usize;
        g.bench_function(format!("search_top10_{name}"), |b| {
            b.to_async(&rt).iter(|| {
                q += 1;
                let (svc, sys) = (svc.clone(), sys.clone());
                let req = SearchRequest {
                    vector: vec_of(N + q),
                    k: 10,
                    ..serde_json::from_value(serde_json::json!({"vector": [0.0], "k": 10})).unwrap()
                };
                async move {
                    let r = svc.search(&sys, name, req).await.unwrap();
                    assert_eq!(r.hits.len(), 10);
                }
            });
        });
    }
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
