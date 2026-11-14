//! `sql`: typical OLTP statements through `POST /api/v1/sql` (the SQL
//! editor): point lookups, an indexed range scan, aggregates, a join, single
//! and batch inserts. `customers` and `orders` (`--sql-customers`,
//! `--sql-orders`) are loaded through `POST /api/v1/sql/batch` first.
//! The statements work on SQLite and on PostgreSQL.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use forgebase_bench::Lcg;
use serde_json::{Value, json};

use crate::http::Conn;
use crate::stats::Stats;
use crate::{Args, run_workers};

const OPS: [&str; 8] = [
    "point_lookup_pk",
    "lookup_secondary_index",
    "range_scan_100",
    "aggregate_one_customer",
    "join_by_region",
    "aggregate_full_scan",
    "insert_1",
    "insert_batch_100",
];

/// Which of [`OPS`] the `n`-th operation is: 20 statements, mostly reads.
fn pick(n: u64) -> usize {
    match n % 20 {
        0..=5 => 0,
        6..=9 => 1,
        10..=12 => 2,
        13 | 14 => 3,
        15 | 16 => 4,
        17 => 5,
        18 => 6,
        _ => 7,
    }
}

fn stmt(sql: &str, params: Vec<Value>) -> Value {
    json!({"sql": sql, "params": params})
}

const INSERT_ORDER: &str =
    "INSERT INTO orders (id, customer_id, amount, status, placed_at) VALUES (?1, ?2, ?3, ?4, ?5)";

async fn batch(c: &mut Conn, token: &str, statements: Vec<Value>) -> (u16, Value) {
    c.json(
        "POST",
        "/api/v1/sql/batch",
        Some(token),
        Some(&json!({"statements": statements})),
    )
    .await
}

pub(crate) async fn run(a: &Args, token: &str) {
    let (customers, orders) = (a.sql_customers.max(20), a.sql_orders.max(100));
    let mut c = Conn::new(&a.addr);
    let ddl = [
        "CREATE TABLE IF NOT EXISTS customers (id BIGINT PRIMARY KEY, name TEXT NOT NULL, region TEXT NOT NULL)",
        "CREATE INDEX IF NOT EXISTS customers_region ON customers (region)",
        "CREATE TABLE IF NOT EXISTS orders (id BIGINT PRIMARY KEY, customer_id BIGINT NOT NULL REFERENCES customers(id), \
         amount BIGINT NOT NULL, status TEXT NOT NULL, placed_at BIGINT NOT NULL)",
        "CREATE INDEX IF NOT EXISTS orders_customer ON orders (customer_id)",
        "CREATE INDEX IF NOT EXISTS orders_placed ON orders (placed_at)",
    ];
    let (s, j) = batch(&mut c, token, ddl.iter().map(|d| stmt(d, vec![])).collect()).await;
    assert_eq!(s, 200, "sql ddl: {j}");
    let (s, j) = c
        .json(
            "POST",
            "/api/v1/sql",
            Some(token),
            Some(&stmt("SELECT count(*) FROM customers", vec![])),
        )
        .await;
    assert_eq!(s, 200, "{j}");
    if j["rows"][0][0].as_i64().unwrap_or(0) == 0 {
        let t0 = Instant::now();
        for start in (0..customers).step_by(1000) {
            let st = (start..(start + 1000).min(customers))
                .map(|i| {
                    stmt(
                        "INSERT INTO customers (id, name, region) VALUES (?1, ?2, ?3)",
                        vec![json!(i), json!(format!("customer {i}")), json!(format!("r{}", i % 20))],
                    )
                })
                .collect();
            let (s, j) = batch(&mut c, token, st).await;
            assert_eq!(s, 200, "load customers: {j}");
        }
        let mut rng = Lcg::new(7);
        for start in (0..orders).step_by(1000) {
            let st = (start..(start + 1000).min(orders))
                .map(|i| {
                    stmt(
                        INSERT_ORDER,
                        vec![
                            json!(i),
                            json!(rng.below(customers as u64)),
                            json!(rng.below(100_000)),
                            json!(["new", "paid", "shipped", "refunded"][i % 4]),
                            json!(1_700_000_000 + i as i64 * 60),
                        ],
                    )
                })
                .collect();
            let (s, j) = batch(&mut c, token, st).await;
            assert_eq!(s, 200, "load orders: {j}");
        }
        let (s, _) = c
            .json("POST", "/api/v1/sql", Some(token), Some(&stmt("ANALYZE", vec![])))
            .await;
        println!(
            "sql: loaded {customers} customers and {orders} orders in {:.1}s (ANALYZE: {s})",
            t0.elapsed().as_secs_f64()
        );
    }
    let (addr, token_c) = (a.addr.clone(), token.to_owned());
    let (stats, secs) = run_workers(a.concurrency, a.duration, OPS.len(), move |w, stop: std::sync::Arc<AtomicBool>| {
        let (addr, token) = (addr.clone(), token_c.clone());
        async move {
            let mut c = Conn::new(&addr);
            let mut st: Vec<Stats> = OPS.iter().map(|_| Stats::default()).collect();
            let mut rng = Lcg::new(w as u64 + 5);
            let mut next_id = 1_000_000_000_i64 + w as i64 * 10_000_000;
            let mut n = 0_u64;
            while !stop.load(Ordering::Relaxed) {
                n += 1;
                let op = pick(n);
                let cust = rng.below(customers as u64);
                let (path, body) = match op {
                    0 => ("/api/v1/sql", stmt("SELECT id, name, region FROM customers WHERE id = ?1", vec![json!(cust)])),
                    1 => (
                        "/api/v1/sql",
                        stmt(
                            "SELECT id, amount, status FROM orders WHERE customer_id = ?1 ORDER BY placed_at DESC LIMIT 20",
                            vec![json!(cust)],
                        ),
                    ),
                    2 => {
                        let from = 1_700_000_000 + rng.below(orders as u64 - 150) as i64 * 60;
                        (
                            "/api/v1/sql",
                            stmt(
                                "SELECT id, customer_id, amount FROM orders WHERE placed_at >= ?1 AND placed_at < ?2 ORDER BY placed_at LIMIT 100",
                                vec![json!(from), json!(from + 6000)],
                            ),
                        )
                    }
                    3 => (
                        "/api/v1/sql",
                        stmt(
                            "SELECT count(*) AS n, CAST(sum(amount) AS BIGINT) AS total FROM orders WHERE customer_id = ?1",
                            vec![json!(cust)],
                        ),
                    ),
                    4 => (
                        "/api/v1/sql",
                        stmt(
                            "SELECT c.name, count(*) AS n, CAST(sum(o.amount) AS BIGINT) AS total FROM customers c \
                             JOIN orders o ON o.customer_id = c.id WHERE c.region = ?1 GROUP BY c.name ORDER BY total DESC LIMIT 10",
                            vec![json!(format!("r{}", cust % 20))],
                        ),
                    ),
                    5 => (
                        "/api/v1/sql",
                        stmt("SELECT status, count(*) AS n, CAST(sum(amount) AS BIGINT) AS total FROM orders GROUP BY status", vec![]),
                    ),
                    6 => {
                        next_id += 1;
                        (
                            "/api/v1/sql",
                            stmt(INSERT_ORDER, vec![json!(next_id), json!(cust), json!(100), json!("new"), json!(next_id)]),
                        )
                    }
                    _ => {
                        let rows: Vec<Value> = (0..100)
                            .map(|_| {
                                next_id += 1;
                                stmt(INSERT_ORDER, vec![json!(next_id), json!(cust), json!(100), json!("new"), json!(next_id)])
                            })
                            .collect();
                        ("/api/v1/sql/batch", json!({"statements": rows}))
                    }
                };
                let t = Instant::now();
                let (s, _) = c.json("POST", path, Some(&token), Some(&body)).await;
                st[op].us.push(t.elapsed().as_micros() as u64);
                if s != 200 {
                    st[op].errors += 1;
                }
            }
            st
        }
    })
    .await;
    for (s, op) in stats.into_iter().zip(OPS) {
        s.report_with(
            "sql",
            op,
            secs,
            &[("customers", json!(customers)), ("orders", json!(orders))],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mix_covers_every_statement() {
        let mut count = [0; 8];
        for n in 0..20 {
            count[pick(n)] += 1;
        }
        assert!(count.iter().all(|c| *c > 0), "{count:?}");
        assert_eq!(count.iter().sum::<i32>(), 20);
    }
}
