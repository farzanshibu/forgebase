//! The writer and the reader pool (single writer, ADR-0001), on SQLite or the
//! bundled PostgreSQL (`FORGEBASE_TEST_BACKEND=postgres`, FB-148).
#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used)]

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use forgebase_bench::Rig;
use forgebase_db::{Backend, Statement, TxMode, Value};

fn bench(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let rig = rt.block_on(Rig::open());
    let db = rig.db.clone();
    rt.block_on(async {
        let mut tx = db.begin(TxMode::Normal).await.unwrap();
        tx.execute(&Statement::raw(
            "CREATE TABLE bench_t (id INTEGER PRIMARY KEY, k TEXT NOT NULL, v TEXT NOT NULL)",
        ))
        .await
        .unwrap();
        for i in 0..10_000_i64 {
            tx.execute(&Statement::new(
                "INSERT INTO bench_t (id, k, v) VALUES (?, ?, ?)",
                vec![i.into(), format!("k{i}").into(), "x".repeat(100).into()],
            ))
            .await
            .unwrap();
        }
        tx.commit().await.unwrap();
    });

    let mut g = c.benchmark_group("db");
    g.sample_size(30);

    // One committed (durable) row per transaction: the fsync-bound cost.
    let mut next = 1_000_000_i64;
    g.throughput(Throughput::Elements(1));
    g.bench_function("writer_commit_1_row", |b| {
        b.to_async(&rt).iter(|| {
            next += 1;
            let id = next;
            let db = db.clone();
            async move {
                let mut tx = db.begin(TxMode::Normal).await.unwrap();
                tx.execute(&Statement::new(
                    "INSERT INTO bench_t (id, k, v) VALUES (?, ?, ?)",
                    vec![id.into(), format!("w{id}").into(), "y".into()],
                ))
                .await
                .unwrap();
                tx.commit().await.unwrap();
            }
        });
    });

    // 100 rows in one transaction: amortised commit.
    let mut base = 10_000_000_i64;
    g.throughput(Throughput::Elements(100));
    g.bench_function("writer_commit_100_rows", |b| {
        b.to_async(&rt).iter(|| {
            base += 100;
            let start = base;
            let db = db.clone();
            async move {
                let mut tx = db.begin(TxMode::Normal).await.unwrap();
                for i in 0..100_i64 {
                    tx.execute(&Statement::new(
                        "INSERT INTO bench_t (id, k, v) VALUES (?, ?, ?)",
                        vec![(start + i).into(), format!("b{}", start + i).into(), "z".into()],
                    ))
                    .await
                    .unwrap();
                }
                tx.commit().await.unwrap();
            }
        });
    });

    // Reader pool: primary-key point lookup and an indexed-less scan count.
    g.throughput(Throughput::Elements(1));
    let mut i = 0_i64;
    g.bench_function("reader_point_select", |b| {
        b.to_async(&rt).iter(|| {
            i = (i + 7919) % 10_000;
            let db = db.clone();
            async move {
                let rows = db
                    .fetch_all(&Statement::new(
                        "SELECT v FROM bench_t WHERE id = ?",
                        vec![Value::from(i)],
                    ))
                    .await
                    .unwrap();
                assert_eq!(rows.len(), 1);
            }
        });
    });
    g.bench_function("reader_count_10k", |b| {
        b.to_async(&rt).iter(|| {
            let db = db.clone();
            async move {
                db.fetch_all(&Statement::raw("SELECT count(*) FROM bench_t WHERE id < 10000"))
                    .await
                    .unwrap();
            }
        });
    });
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
