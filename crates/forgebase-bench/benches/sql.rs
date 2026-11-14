//! The SQL editor endpoint's data layer (`DataService::execute_sql*`, the
//! code behind `POST /api/v1/sql`) on typical OLTP shapes: point lookup,
//! indexed range scan, aggregate, a join of two tables, single and batch
//! inserts. Real persistence, on SQLite or PostgreSQL
//! (`FORGEBASE_TEST_BACKEND`).
//!
//! `customers` (10 000 rows at full scale) and `orders` (50 000 rows, five
//! per customer on average) are created and loaded through the same database
//! the editor uses; the benched statements go through the editor, so the
//! statement classification, secret masking and row cap are in the numbers.
#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used)]

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use forgebase_bench::{Lcg, Rig, scaled};
use forgebase_data::{AccessContext, SqlStatement};
use forgebase_db::{Backend, Statement, TxMode};
use serde_json::{Value, json};

fn sql(text: &str, params: Vec<Value>) -> SqlStatement {
    SqlStatement {
        sql: text.into(),
        params,
    }
}

async fn load(rig: &Rig, customers: usize, orders: usize) {
    let db = &rig.db;
    let mut tx = db.begin(TxMode::Normal).await.unwrap();
    for ddl in [
        "CREATE TABLE customers (id BIGINT PRIMARY KEY, name TEXT NOT NULL, region TEXT NOT NULL)",
        "CREATE INDEX customers_region ON customers (region)",
        "CREATE TABLE orders (id BIGINT PRIMARY KEY, customer_id BIGINT NOT NULL REFERENCES customers(id), \
         amount BIGINT NOT NULL, status TEXT NOT NULL, placed_at BIGINT NOT NULL)",
        "CREATE INDEX orders_customer ON orders (customer_id)",
        "CREATE INDEX orders_placed ON orders (placed_at)",
    ] {
        tx.execute(&Statement::raw(ddl)).await.unwrap();
    }
    for i in 0..customers {
        tx.execute(&Statement::new(
            "INSERT INTO customers (id, name, region) VALUES (?, ?, ?)",
            vec![
                (i as i64).into(),
                format!("customer {i}").into(),
                format!("r{}", i % 20).into(),
            ],
        ))
        .await
        .unwrap();
    }
    let mut rng = Lcg::new(7);
    for i in 0..orders {
        tx.execute(&Statement::new(
            "INSERT INTO orders (id, customer_id, amount, status, placed_at) VALUES (?, ?, ?, ?, ?)",
            vec![
                (i as i64).into(),
                (rng.below(customers as u64) as i64).into(),
                (rng.below(100_000) as i64).into(),
                ["new", "paid", "shipped", "refunded"][i % 4].into(),
                (1_700_000_000 + i as i64 * 60).into(),
            ],
        ))
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
    // Statistics, so PostgreSQL plans for the loaded sizes (SQLite: a no-op cost).
    let mut tx = db.begin(TxMode::Normal).await.unwrap();
    tx.execute(&Statement::raw("ANALYZE")).await.unwrap();
    tx.commit().await.unwrap();
}

fn bench(c: &mut Criterion) {
    let (customers, orders) = (scaled(10_000, 500), scaled(50_000, 2_500));
    let rt = tokio::runtime::Runtime::new().unwrap();
    let rig = rt.block_on(Rig::open());
    rt.block_on(load(&rig, customers, orders));
    let data = rt.block_on(rig.data());
    let ctx = AccessContext::admin("root");

    let mut g = c.benchmark_group("sql");
    g.sample_size(30);
    g.throughput(Throughput::Elements(1));

    let mut i = 0_usize;
    g.bench_function("point_lookup_pk", |b| {
        b.to_async(&rt).iter(|| {
            i = (i + 7919) % customers;
            let (data, ctx) = (data.clone(), ctx.clone());
            async move {
                let r = data
                    .execute_sql(
                        &ctx,
                        sql("SELECT id, name, region FROM customers WHERE id = ?1", vec![json!(i)]),
                    )
                    .await
                    .unwrap();
                assert_eq!(r.rows.len(), 1);
            }
        });
    });
    let mut i = 0_usize;
    g.bench_function("point_lookup_secondary_index", |b| {
        b.to_async(&rt).iter(|| {
            i = (i + 7919) % customers;
            let (data, ctx) = (data.clone(), ctx.clone());
            async move {
                data.execute_sql(
                    &ctx,
                    sql(
                        "SELECT id, amount, status FROM orders WHERE customer_id = ?1 ORDER BY placed_at DESC LIMIT 20",
                        vec![json!(i)],
                    ),
                )
                .await
                .unwrap();
            }
        });
    });
    let mut i = 0_i64;
    g.bench_function("range_scan_indexed_100", |b| {
        b.to_async(&rt).iter(|| {
            i = (i + 7919) % (orders as i64 - 200);
            let from = 1_700_000_000 + i * 60;
            let (data, ctx) = (data.clone(), ctx.clone());
            async move {
                let r = data
                    .execute_sql(
                        &ctx,
                        sql(
                            "SELECT id, customer_id, amount FROM orders WHERE placed_at >= ?1 AND placed_at < ?2 \
                             ORDER BY placed_at LIMIT 100",
                            vec![json!(from), json!(from + 100 * 60)],
                        ),
                    )
                    .await
                    .unwrap();
                assert_eq!(r.rows.len(), 100);
            }
        });
    });
    g.bench_function("aggregate_group_by_full_scan", |b| {
        b.to_async(&rt).iter(|| {
            let (data, ctx) = (data.clone(), ctx.clone());
            async move {
                let r = data
                    .execute_sql(
                        &ctx,
                        sql(
                            "SELECT status, count(*) AS n, CAST(sum(amount) AS BIGINT) AS total FROM orders GROUP BY status",
                            vec![],
                        ),
                    )
                    .await
                    .unwrap();
                assert_eq!(r.rows.len(), 4);
            }
        });
    });
    let mut i = 0_usize;
    g.bench_function("aggregate_one_customer", |b| {
        b.to_async(&rt).iter(|| {
            i = (i + 7919) % customers;
            let (data, ctx) = (data.clone(), ctx.clone());
            async move {
                data.execute_sql(
                    &ctx,
                    sql(
                        "SELECT count(*) AS n, CAST(sum(amount) AS BIGINT) AS total FROM orders WHERE customer_id = ?1",
                        vec![json!(i)],
                    ),
                )
                .await
                .unwrap();
            }
        });
    });
    let mut i = 0_usize;
    g.bench_function("join_two_tables_by_region", |b| {
        b.to_async(&rt).iter(|| {
            i = (i + 1) % 20;
            let (data, ctx) = (data.clone(), ctx.clone());
            async move {
                data.execute_sql(
                    &ctx,
                    sql(
                        "SELECT c.name, count(*) AS n, CAST(sum(o.amount) AS BIGINT) AS total \
                         FROM customers c JOIN orders o ON o.customer_id = c.id \
                         WHERE c.region = ?1 GROUP BY c.name ORDER BY total DESC LIMIT 10",
                        vec![json!(format!("r{i}"))],
                    ),
                )
                .await
                .unwrap();
            }
        });
    });

    let mut next = orders as i64 + 1_000_000;
    g.bench_function("insert_1_row", |b| {
        b.to_async(&rt).iter(|| {
            next += 1;
            let id = next;
            let (data, ctx) = (data.clone(), ctx.clone());
            async move {
                data.execute_sql(
                    &ctx,
                    sql(
                        "INSERT INTO orders (id, customer_id, amount, status, placed_at) VALUES (?1, ?2, ?3, 'new', ?4)",
                        vec![json!(id), json!(id % customers as i64), json!(100), json!(id)],
                    ),
                )
                .await
                .unwrap();
            }
        });
    });
    g.throughput(Throughput::Elements(100));
    let mut base = orders as i64 + 10_000_000;
    g.bench_function("insert_batch_100_rows", |b| {
        b.to_async(&rt).iter(|| {
            base += 100;
            let start = base;
            let (data, ctx) = (data.clone(), ctx.clone());
            async move {
                let stmts = (0..100_i64)
                    .map(|k| {
                        let id = start + k;
                        sql(
                            "INSERT INTO orders (id, customer_id, amount, status, placed_at) VALUES (?1, ?2, ?3, 'new', ?4)",
                            vec![json!(id), json!(id % customers as i64), json!(100), json!(id)],
                        )
                    })
                    .collect();
                let r = data.execute_sql_batch(&ctx, stmts).await.unwrap();
                assert_eq!(r.len(), 100);
            }
        });
    });
    g.finish();
    drop(data);
    rt.block_on(rig.close());
}

criterion_group!(benches, bench);
criterion_main!(benches);
