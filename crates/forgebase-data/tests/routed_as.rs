//! FB-171 (god's scope addition for the SQL editor): a routed read as a
//! role. `Backend::fetch_routed_as` reads a healthy replica that has the
//! caller's commits, switching role and claims in the replica's read-only
//! transaction exactly as `fetch_all_as` does on the primary; strong
//! requests and backends without replicas read the primary as that role.
//! Needs a PostgreSQL engine, else it prints a skip (SQLite has no roles
//! and no replicas: `fetch_routed_as` is `fetch_all`).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use forgebase_data::db::routing::testing::RoutedCluster;
use forgebase_data::db::routing::{Consistency, ReadIntent, RequestScope, scope};
use forgebase_data::db::{Backend, RunAs, Statement, Value};

fn who(rows: &forgebase_data::db::Rows) -> (String, String) {
    let r = rows.rows.first().unwrap();
    let text = |i: usize| r.get(i).and_then(Value::as_str).unwrap_or_default().to_owned();
    (text(0), text(1))
}

#[tokio::test]
async fn routed_reads_switch_role_on_the_replica_and_the_primary() {
    let Some(rc) = RoutedCluster::start().await else {
        eprintln!("skipped: no PostgreSQL engine configured");
        return;
    };
    rc.cluster
        .primary_sql("CREATE ROLE fb171_editor NOLOGIN; GRANT USAGE ON SCHEMA public TO fb171_editor")
        .await
        .unwrap();
    rc.sync().await;
    let backend = rc.backend();
    let run_as = RunAs::new("fb171_editor", r#"{"sub":"a1"}"#);
    let stmt = Statement::new(
        "SELECT current_user::text, current_setting('forgebase.claims', true) AS claims, pg_is_in_recovery()::int",
        vec![],
    );
    let intent = ReadIntent::replica("sql.editor");

    let before = rc.replica_reads();
    let rows = backend.fetch_routed_as(&stmt, &intent, &run_as).await.unwrap();
    assert_eq!(who(&rows), ("fb171_editor".to_owned(), r#"{"sub":"a1"}"#.to_owned()));
    assert_eq!(rows.rows[0][2].as_i64(), Some(1), "read on the standby");
    assert_eq!(rc.replica_reads(), before + 1);
    assert_eq!(rc.fallbacks(), 0);

    // A strong request reads the primary, still as the role.
    let strong = RequestScope::new(Consistency::Strong, Default::default());
    let rows = scope::scoped(strong, backend.fetch_routed_as(&stmt, &intent, &run_as))
        .await
        .unwrap();
    assert_eq!(who(&rows).0, "fb171_editor");
    assert_eq!(rows.rows[0][2].as_i64(), Some(0), "read on the primary");
    assert_eq!(rc.replica_reads(), before + 1);

    // An intent that does not allow replicas reads the primary as the role.
    let rows = backend
        .fetch_routed_as(&stmt, &ReadIntent::primary("sql.editor"), &run_as)
        .await
        .unwrap();
    assert_eq!(rows.rows[0][2].as_i64(), Some(0));
    assert_eq!(who(&rows).0, "fb171_editor");
}
