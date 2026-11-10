//! Regression (MDF-52): a pooled connection that cached `SELECT t.*` must see
//! new columns after DDL committed through another connection.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use forgebase_core::Synchronous;
use forgebase_db::{Backend, Database, DbOptions, Statement, TxMode};

#[tokio::test]
async fn cached_star_select_sees_columns_added_by_another_connection() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(DbOptions {
        path: tmp.path().join("t.db"),
        synchronous: Synchronous::Full,
        busy_timeout: Duration::from_millis(5000),
        // One reader = connection A is the only one that can serve the reads.
        read_pool_size: 1,
        create_if_missing: true,
        read_only: false,
    })
    .await
    .unwrap();

    let mut tx = db.begin(TxMode::SchemaChange).await.unwrap();
    tx.execute(&Statement::raw("CREATE TABLE t (a INTEGER, b INTEGER)"))
        .await
        .unwrap();
    tx.execute(&Statement::raw("INSERT INTO t VALUES (1, 2)"))
        .await
        .unwrap();
    tx.commit().await.unwrap();

    // Prepare + cache on reader connection A.
    let star = Statement::new("SELECT t.* FROM t", vec![]);
    assert_eq!(db.fetch_all(&star).await.unwrap().columns, ["a", "b"]);

    // ALTER on the writer connection B.
    let mut tx = db.begin(TxMode::SchemaChange).await.unwrap();
    tx.execute(&Statement::raw("ALTER TABLE t ADD COLUMN c INTEGER DEFAULT 3"))
        .await
        .unwrap();
    tx.commit().await.unwrap();

    // Read on A again: used to panic "index out of bounds" or return stale columns.
    let rows = db.fetch_all(&star).await.unwrap();
    assert_eq!(rows.columns, ["a", "b", "c"]);
    assert_eq!(rows.rows[0].len(), 3);
}

/// FB-85 (SPEC §27 #5, #6 on the binary): `CREATE INDEX` committed in a
/// `Normal` transaction (the index manager's path) must reach a reader that
/// already loaded the schema, both for query plans and for `PRAGMA` reads,
/// which SQLite answers from the connection's in-memory schema.
#[tokio::test]
async fn readers_see_an_index_created_in_a_normal_transaction() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(DbOptions {
        path: tmp.path().join("t.db"),
        synchronous: Synchronous::Full,
        busy_timeout: Duration::from_millis(5000),
        read_pool_size: 1,
        create_if_missing: true,
        read_only: false,
    })
    .await
    .unwrap();
    let mut tx = db.begin(TxMode::SchemaChange).await.unwrap();
    tx.execute(&Statement::raw("CREATE TABLE items (category TEXT, price REAL)"))
        .await
        .unwrap();
    tx.commit().await.unwrap();

    // The only reader loads the schema (no index yet).
    let query = Statement::new(
        "SELECT * FROM items WHERE category = ?1 AND price > ?2",
        vec!["c3".into(), 1i64.into()],
    );
    let plan = db.explain(&query).await.unwrap();
    assert!(plan.iter().any(|s| s.detail.starts_with("SCAN")), "{plan:?}");

    let mut tx = db.begin(TxMode::Normal).await.unwrap();
    tx.execute(&Statement::raw(
        "  -- index manager\n CREATE INDEX \"idx_items_cat_price\" ON \"items\" (\"category\", \"price\" DESC)",
    ))
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let plan = db.explain(&query).await.unwrap();
    assert!(
        plan.iter().any(|s| s.detail.contains("idx_items_cat_price")),
        "the reader plans with the new index: {plan:?}"
    );
    let cols = db
        .fetch_all(&Statement::raw("PRAGMA index_info(\"idx_items_cat_price\")"))
        .await
        .unwrap();
    assert_eq!(cols.rows.len(), 2, "PRAGMA sees the new index");

    // Plain writes do not force a refresh, and DROP INDEX does.
    let mut tx = db.begin(TxMode::Normal).await.unwrap();
    tx.execute(&Statement::raw("INSERT INTO items VALUES ('c3', 2)"))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let mut tx = db.begin(TxMode::Normal).await.unwrap();
    tx.execute(&Statement::raw("drop index idx_items_cat_price"))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let plan = db.explain(&query).await.unwrap();
    assert!(
        !plan.iter().any(|s| s.detail.contains("idx_items_cat_price")),
        "{plan:?}"
    );
}
