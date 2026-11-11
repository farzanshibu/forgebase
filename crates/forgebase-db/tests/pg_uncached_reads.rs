//! Regression (FB-161, found by FB-170): on PostgreSQL the first uncached
//! read (`Statement::raw`) whose result has a column of a type sqlx does not
//! know yet (`citext`, an enum, any extension type) failed on a fresh
//! connection with "unnamed prepared statement does not exist": sqlx looks the
//! type up with a simple query, which destroys the unnamed statement it is
//! about to bind. The retry worked because the type was cached by then.
//!
//! Runs only with `FORGEBASE_TEST_BACKEND=postgres` (skips otherwise).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use forgebase_db::testing::{postgres_test_db, selected_backend};
use forgebase_db::{Backend, BackendKind, Statement, TxMode, Value};

async fn pg() -> Option<forgebase_db::testing::TestDb> {
    if selected_backend() != BackendKind::Postgres {
        return None;
    }
    postgres_test_db().await
}

#[tokio::test]
async fn first_uncached_read_of_a_citext_column_succeeds() {
    let Some(db) = pg().await else { return };
    let mut tx = db.begin(TxMode::Normal).await.unwrap();
    tx.execute(&Statement::new(
        "INSERT INTO _forgebase_collections (id, name, type, definition, created_at, updated_at) \
         VALUES ('c1', 'Posts', 'base', '{}', '2026-11-11T00:00:00Z', '2026-11-11T00:00:00Z')",
        vec![],
    ))
    .await
    .unwrap();
    tx.commit().await.unwrap();

    // Every reader connection is fresh: each first read must work.
    for _ in 0..6 {
        let rows = db
            .fetch_all(&Statement::raw("SELECT name FROM _forgebase_collections"))
            .await
            .expect("uncached citext read on a fresh connection");
        assert_eq!(rows.rows, vec![vec![Value::Text("Posts".into())]]);
    }
}

#[tokio::test]
async fn first_uncached_read_of_a_user_type_succeeds_in_write_transactions_and_reads() {
    let Some(db) = pg().await else { return };
    let mut tx = db.begin(TxMode::SchemaChange).await.unwrap();
    tx.execute(&Statement::raw(
        "CREATE TYPE public.fb161_mood AS ENUM ('calm', 'busy'); \
         CREATE TABLE public.fb161_t (m public.fb161_mood NOT NULL); \
         INSERT INTO public.fb161_t VALUES ('busy')",
    ))
    .await
    .unwrap();
    tx.commit().await.unwrap();

    // On the writer, inside a transaction: the failure would abort it.
    let mut tx = db.begin(TxMode::Normal).await.unwrap();
    let rows = tx
        .fetch_all(&Statement::raw("SELECT m, m::text AS t FROM public.fb161_t"))
        .await
        .expect("uncached enum read in a write transaction");
    assert_eq!(
        rows.rows,
        vec![vec![Value::Text("busy".into()), Value::Text("busy".into())]]
    );
    tx.execute(&Statement::raw("INSERT INTO public.fb161_t VALUES ('calm')"))
        .await
        .unwrap();
    tx.commit().await.expect("the transaction is still usable");

    // On a reader (fresh connections, each its own READ ONLY transaction).
    let read = Statement::raw("SELECT m FROM public.fb161_t ORDER BY m");
    let rows = db.fetch_all(&read).await.expect("uncached enum read");
    assert_eq!(
        rows.rows,
        vec![vec![Value::Text("calm".into())], vec![Value::Text("busy".into())]]
    );
    let n = db
        .fetch_all(&Statement::raw("SELECT count(*) FROM public.fb161_t"))
        .await
        .unwrap();
    assert_eq!(n.scalar(), Some(&Value::Integer(2)));
}
