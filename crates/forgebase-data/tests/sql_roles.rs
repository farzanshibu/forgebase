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
    SqlOptions {
        show_secrets: true,
        ..SqlOptions::default()
    }
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

/// Without the editor's role (SQLite, or PostgreSQL before the server sets
/// it), the system tables' secret columns follow the rule PostgreSQL's
/// grants use (`forgebase.is_secret_column`): a statement that names one is
/// refused, `SELECT *` masks them, and `show_secrets` shows them.
#[tokio::test]
async fn system_table_secrets_are_guarded_without_the_editors_role() {
    use forgebase_db::{Backend, Statement, TxMode};

    let db = test_db().await;
    let data = service(&db).await;
    let a = admin();
    let mut tx = db.begin(TxMode::Normal).await.unwrap();
    tx.execute(&Statement::raw(
        "INSERT INTO _forgebase_admins (id, email, password_hash, created_at, updated_at) \
         VALUES ('adm1', 'root@example.com', '$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA', 1, 1)",
    ))
    .await
    .unwrap();
    tx.commit().await.unwrap();

    for sql in [
        "SELECT password_hash FROM _forgebase_admins",
        "SELECT substr(password_hash, 1, 9) AS p FROM _forgebase_admins",
        "SELECT secret_enc FROM _forgebase_auth_keys",
        "SELECT count(*) FROM _forgebase_admins WHERE password_hash LIKE '$argon2%'",
    ] {
        let r = data.execute_sql(&a, stmt(sql)).await;
        assert!(matches!(r, Err(DataError::Forbidden(_))), "{sql}: {r:?}");
        let b = data.execute_sql_batch(&a, vec![stmt(sql)]).await;
        assert!(matches!(b, Err(DataError::Forbidden(_))), "batch {sql}: {b:?}");
    }
    let all = data
        .execute_sql(&a, stmt("SELECT * FROM _forgebase_admins"))
        .await
        .unwrap();
    let col = all.columns.iter().position(|c| c == "password_hash").unwrap();
    assert_eq!(all.rows[0][col], json!(forgebase_data::SECRET_MASK), "{all:?}");
    assert!(all.secrets_masked);
    let email = all.columns.iter().position(|c| c == "email").unwrap();
    assert_eq!(all.rows[0][email], json!("root@example.com"), "other columns are shown");

    let shown = data
        .execute_sql_with(&a, stmt("SELECT password_hash FROM _forgebase_admins"), secrets())
        .await
        .unwrap();
    assert!(
        shown.rows[0][0].as_str().unwrap().starts_with("$argon2id$"),
        "{shown:?}"
    );

    // A table of the project's own with a column named like a secret is not
    // a system table: the rule does not apply.
    data.execute_sql(&a, stmt("CREATE TABLE notes (id text PRIMARY KEY, api_token text)"))
        .await
        .unwrap();
    data.execute_sql(&a, stmt("INSERT INTO notes VALUES ('n1', 'visible')"))
        .await
        .unwrap();
    let n = data.execute_sql(&a, stmt("SELECT api_token FROM notes")).await.unwrap();
    assert_eq!(n.rows[0][0], json!("visible"));
}

/// PostgreSQL: the SQL editor keeps `?`, `?|` and `?&` as the `jsonb`
/// operators in text that uses `$N` placeholders or none, and still takes
/// SQLite-style `?` placeholders in a statement that has values and no `$N`.
#[tokio::test]
async fn jsonb_question_mark_operators_work_in_the_editor() {
    let db = test_db().await;
    if !is_postgres(&db) {
        eprintln!("skipped: PostgreSQL only");
        return;
    }
    let data = service(&db).await;
    let a = admin();
    let r = data
        .execute_sql(
            &a,
            stmt("SELECT '{\"a\": 1}'::jsonb ? 'a' AS has, '{\"a\": 1}'::jsonb ?| array['x', 'a'] AS any_of, '{\"a\": 1}'::jsonb ?& array['a', 'b'] AS all_of"),
        )
        .await
        .unwrap();
    // Booleans read as 1 / 0 on both engines.
    assert_eq!(r.rows[0], vec![json!(1), json!(1), json!(0)], "{r:?}");
    let with_param = SqlStatement {
        sql: "SELECT $1::jsonb ? 'a' AS has".into(),
        params: vec![json!("{\"a\": 2}")],
    };
    assert_eq!(
        data.execute_sql(&a, with_param.clone()).await.unwrap().rows[0][0],
        json!(1)
    );
    let b = data.execute_sql_batch(&a, vec![with_param]).await.unwrap();
    assert_eq!(b[0].rows[0][0], json!(1), "batches too");
    let question = SqlStatement {
        sql: "SELECT CAST(? AS integer) + 1 AS n".into(),
        params: vec![json!(41)],
    };
    assert_eq!(data.execute_sql(&a, question).await.unwrap().rows[0][0], json!(42));
    let plan = data.explain_sql(&a, stmt("SELECT '{\"a\": 1}'::jsonb ? 'a'")).await;
    assert!(plan.is_ok(), "{plan:?}");
}
