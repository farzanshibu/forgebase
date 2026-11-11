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
use forgebase_data::db::{RunAs, Statement, Value};

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

/// The SQL editor (FB-153 runs it as `forgebase_sql_admin`) with "Read from
/// replica": the read goes to the standby, as the editor's role.
#[tokio::test]
async fn sql_editor_reads_the_replica_as_its_role() {
    let Some(rc) = RoutedCluster::start().await else {
        eprintln!("skipped: no PostgreSQL engine configured");
        return;
    };
    let svc = forgebase_data::DataService::new(rc.backend(), std::sync::Arc::new(forgebase_data::DefaultAuthorizer))
        .await
        .unwrap();
    svc.set_sql_editor_role(Some(forgebase_data::SQL_ADMIN_ROLE.to_owned()));
    rc.sync().await;
    let ctx = forgebase_data::AccessContext::admin("root");
    let stmt = || forgebase_data::SqlStatement {
        sql: "SELECT current_user::text AS who, pg_is_in_recovery()::int AS standby".into(),
        params: vec![],
    };
    let before = rc.replica_reads();
    let ticked = forgebase_data::SqlOptions {
        read_from_replica: true,
        ..Default::default()
    };
    let r = svc.execute_sql_with(&ctx, stmt(), ticked).await.unwrap();
    let row = serde_json::to_value(&r).unwrap()["rows"][0].clone();
    assert_eq!(row[0], "forgebase_sql_admin", "{r:?}");
    assert_eq!(row[1], 1, "read on the standby: {r:?}");
    assert_eq!(rc.replica_reads(), before + 1);
    // Unticked: the primary, still as the role.
    let r = svc
        .execute_sql_with(&ctx, stmt(), forgebase_data::SqlOptions::default())
        .await
        .unwrap();
    let row = serde_json::to_value(&r).unwrap()["rows"][0].clone();
    assert_eq!(
        (row[0].clone(), row[1].clone()),
        (serde_json::json!("forgebase_sql_admin"), serde_json::json!(0))
    );
    assert_eq!(rc.replica_reads(), before + 1);
}
