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
    tx.execute(&Statement::raw("CREATE TABLE t (a INTEGER, b INTEGER)")).await.unwrap();
    tx.execute(&Statement::raw("INSERT INTO t VALUES (1, 2)")).await.unwrap();
    tx.commit().await.unwrap();

    // Prepare + cache on reader connection A.
    let star = Statement::new("SELECT t.* FROM t", vec![]);
    assert_eq!(db.fetch_all(&star).await.unwrap().columns, ["a", "b"]);

    // ALTER on the writer connection B.
    let mut tx = db.begin(TxMode::SchemaChange).await.unwrap();
    tx.execute(&Statement::raw("ALTER TABLE t ADD COLUMN c INTEGER DEFAULT 3")).await.unwrap();
    tx.commit().await.unwrap();

    // Read on A again: used to panic "index out of bounds" or return stale columns.
    let rows = db.fetch_all(&star).await.unwrap();
    assert_eq!(rows.columns, ["a", "b", "c"]);
    assert_eq!(rows.rows[0].len(), 3);
}
