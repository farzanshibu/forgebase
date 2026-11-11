//! FB-153: the SQL editor's role on PostgreSQL (rls-coverage §3.5, G11).
//! Skips on SQLite, where the editor has no roles.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{admin, is_postgres, service, test_db};
use forgebase_data::{DataError, SQL_ADMIN_ROLE, SqlOptions, SqlStatement};
use serde_json::json;

fn stmt(sql: &str) -> SqlStatement {
    SqlStatement {
        sql: sql.into(),
        params: vec![],
    }
}

fn secrets() -> SqlOptions {
    SqlOptions { show_secrets: true }
}

#[tokio::test]
async fn the_editor_runs_as_its_role_without_secret_columns() {
    let db = test_db().await;
    if !is_postgres(&db) {
        eprintln!("skipped: SQLite has no roles");
        return;
    }
    let data = service(&db).await;
    let a = admin();
    // Before the server turns it on, the pools' own roles run the editor.
    let who = data.execute_sql(&a, stmt("SELECT current_user")).await.unwrap();
    assert_ne!(who.rows[0][0], json!(SQL_ADMIN_ROLE));

    data.set_sql_editor_role(Some(SQL_ADMIN_ROLE.into()));
    let who = data.execute_sql(&a, stmt("SELECT current_user")).await.unwrap();
    assert_eq!(who.rows[0][0], json!(SQL_ADMIN_ROLE), "reads run as the editor's role");
    let who = data
        .execute_sql_batch(&a, vec![stmt("SELECT current_user"), stmt("SELECT 1")])
        .await
        .unwrap();
    assert_eq!(who[0].rows[0][0], json!(SQL_ADMIN_ROLE), "batches too");

    // System tables: every column but the secret ones (migration 0042's
    // grants). The data core's name guard does not know these columns: the
    // role is what refuses them.
    let ok = data
        .execute_sql(&a, stmt("SELECT id, email FROM _forgebase_admins"))
        .await;
    assert!(ok.is_ok(), "non-secret columns are readable: {ok:?}");
    for sql in [
        "SELECT password_hash FROM _forgebase_admins",
        "SELECT * FROM _forgebase_admins",
        "SELECT secret_enc FROM _forgebase_auth_keys",
        "SELECT token_hash FROM _forgebase_auth_refresh_tokens",
        "SELECT secret_hash FROM _forgebase_auth_api_keys",
        "SELECT data FROM _forgebase_record_versions",
        "SELECT claims FROM _forgebase_db_logins",
        "SELECT count(*) FROM (SELECT * FROM _forgebase_auth_mfa) x",
    ] {
        let r = data.execute_sql(&a, stmt(sql)).await;
        assert!(
            matches!(r, Err(DataError::Forbidden(_))),
            "{sql} must be refused by the role: {r:?}"
        );
        let b = data.execute_sql_batch(&a, vec![stmt(sql)]).await;
        assert!(matches!(b, Err(DataError::Forbidden(_))), "batch {sql}: {b:?}");
    }
    // show_secrets is the audited way to see them: the pools' own roles.
    let shown = data
        .execute_sql_with(&a, stmt("SELECT password_hash FROM _forgebase_admins"), secrets())
        .await;
    assert!(shown.is_ok(), "{shown:?}");
    let who = data
        .execute_sql_with(&a, stmt("SELECT current_user"), secrets())
        .await
        .unwrap();
    assert_ne!(who.rows[0][0], json!(SQL_ADMIN_ROLE));
    // Generated policies and the api schema are ForgeBase's.
    data.execute_sql(&a, stmt("CREATE TABLE notes (id text PRIMARY KEY, body text)"))
        .await
        .unwrap();
    for sql in [
        "DROP POLICY IF EXISTS fb_rule_select ON notes",
        "CREATE POLICY \"fb_mine\" ON notes USING (true)",
        "DROP VIEW IF EXISTS api.notes",
    ] {
        let r = data.execute_sql(&a, stmt(sql)).await;
        assert!(matches!(r, Err(DataError::Forbidden(_))), "{sql}: {r:?}");
    }
    // Hand-written policies run as the server (the table's owner).
    data.execute_sql(
        &a,
        stmt("CREATE POLICY mine ON notes AS RESTRICTIVE USING (body IS NOT NULL)"),
    )
    .await
    .unwrap();
    // The editor's own tables are its own.
    data.execute_sql(&a, stmt("INSERT INTO notes VALUES ('n1', 'x')"))
        .await
        .unwrap();
    let n = data.execute_sql(&a, stmt("SELECT count(*) FROM notes")).await.unwrap();
    assert_eq!(n.rows[0][0], json!(1));
    // Extensions go through the API only.
    let r = data.execute_sql(&a, stmt("CREATE EXTENSION hstore")).await;
    assert!(r.is_err(), "the editor refuses CREATE EXTENSION");
}
