//! FB-153: named direct logins, their claims, the SQL editor's role and
//! "run as" (rls-coverage §3.5, G11). PostgreSQL only (they skip on SQLite,
//! where `create_db_login` refuses with `db_logins_unavailable`).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{Env, admin, pk, user};
use forgebase_data::{AccessContext, DataError, SqlOptions, SqlStatement};
use forgebase_db::{Backend, BackendKind, Statement, Value};
use forgebase_policy::PolicyError;
use forgebase_policy::logins::{LoginSecret, LoginSpec};
use forgebase_policy::native::NativeMode;
use forgebase_policy::store::PolicyInput;
use serde_json::json;
use sqlx::{Connection, PgConnection, Row};

fn rules(list: &str, write: &str) -> PolicyInput {
    PolicyInput {
        list_rule: Some(list.into()),
        view_rule: Some(list.into()),
        create_rule: Some(write.into()),
        update_rule: Some(write.into()),
        delete_rule: Some(write.into()),
        ..PolicyInput::default()
    }
}

/// People alice and bob, collection `notes` (owner, body) whose rules let
/// each owner see and write their own notes; n1, n3 are alice's, n2 bob's.
async fn world() -> Option<Env> {
    let env = Env::new().await;
    if env.db.kind() != BackendKind::Postgres {
        eprintln!("skipped: direct logins need PostgreSQL");
        let r = env
            .policy
            .create_db_login(
                &admin(),
                LoginSpec {
                    name: "x".into(),
                    principal: Some(pk("alice")),
                    ..LoginSpec::default()
                },
            )
            .await;
        assert!(matches!(r, Err(PolicyError::Invalid { code: "db_logins_unavailable", .. })), "{r:?}");
        return None;
    }
    env.policy.enable_native_rls(NativeMode::Logins).await.unwrap();
    env.person("alice", "alice@example.com").await;
    env.person("bob", "bob@example.com").await;
    env.collection(json!({"id": "notes_c", "name": "notes", "fields": [
        {"name": "owner", "type": "text"}, {"name": "body", "type": "text"}
    ]}))
    .await;
    env.policy
        .put_policy(
            &admin(),
            "notes",
            &rules("owner = @request.auth.id", "owner = @request.auth.id"),
        )
        .await
        .unwrap();
    for (id, owner) in [("n1", "alice"), ("n2", "bob"), ("n3", "alice")] {
        env.data
            .create_record(
                &AccessContext::system(),
                "notes",
                json!({"id": id, "owner": owner, "body": "x"}),
            )
            .await
            .unwrap();
    }
    Some(env)
}

async fn login(env: &Env, name: &str, who: &str, readonly: bool) -> LoginSecret {
    env.policy
        .create_db_login(
            &admin(),
            LoginSpec {
                name: name.into(),
                principal: Some(pk(who)),
                readonly,
                ..LoginSpec::default()
            },
        )
        .await
        .unwrap()
}

/// A driver connection as the login (the test cluster's URL with the
/// login's role and password).
async fn connect(env: &Env, secret: &LoginSecret) -> PgConnection {
    let mut url = url::Url::parse(&env.db_url().unwrap()).unwrap();
    url.set_username(&secret.login.role).unwrap();
    url.set_password(Some(&secret.password)).unwrap();
    PgConnection::connect(url.as_str()).await.unwrap()
}

async fn ids(conn: &mut PgConnection, sql: &'static str) -> Result<Vec<String>, String> {
    sqlx::query(sql)
        .fetch_all(&mut *conn)
        .await
        .map(|rows| rows.iter().map(|r| r.get::<String, _>(0)).collect())
        .map_err(|e| e.to_string())
}

#[tokio::test]
async fn a_login_sees_only_its_rows_and_its_claims_follow_every_reload() {
    let Some(env) = world().await else { return };
    let alice = login(&env, "alice", "alice", false).await;
    assert_eq!(alice.login.role, "fbu_alice");
    assert!(alice.login.has_claims);
    assert!(alice.password.len() >= 32, "a generated password");
    let mut conn = connect(&env, &alice).await;

    // Names resolve to the api views; the rules apply.
    let q = "SELECT id FROM notes ORDER BY id";
    assert_eq!(ids(&mut conn, q).await.unwrap(), ["n1", "n3"]);
    // Spoofing: its own SET of the claims is ignored; the tables and the
    // system schema are out of reach; it cannot switch role.
    let forged = json!({"auth": {"id": "bob"}}).to_string().replace('\'', "''");
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("SET forgebase.claims = '{forged}'")))
        .execute(&mut conn)
        .await
        .unwrap();
    assert_eq!(ids(&mut conn, q).await.unwrap(), ["n1", "n3"]);
    for bad in [
        "SELECT id FROM public.notes",
        "SELECT name FROM forgebase._forgebase_db_logins",
        "SET ROLE forgebase_rls",
        "SET ROLE forgebase_sql_admin",
    ] {
        assert!(sqlx::raw_sql(bad).execute(&mut conn).await.is_err(), "{bad} must fail");
    }

    // Writes go through the view's trigger, under the rules.
    sqlx::raw_sql("INSERT INTO notes (id, owner, body) VALUES ('n4', 'alice', 'mine')")
        .execute(&mut conn)
        .await
        .unwrap();
    assert!(
        sqlx::raw_sql("INSERT INTO notes (id, owner, body) VALUES ('n5', 'bob', 'forged')")
            .execute(&mut conn)
            .await
            .is_err(),
        "the create rule refuses another owner"
    );
    let n = sqlx::raw_sql("UPDATE notes SET body = 'taken' WHERE id = 'n2'")
        .execute(&mut conn)
        .await
        .unwrap();
    assert_eq!(n.rows_affected(), 0, "bob's note is not reachable");

    // A rule change reloads the snapshot and rewrites the claims; the same
    // session sees the new rows at its next statement.
    env.policy
        .put_policy(&admin(), "notes", &rules("", "owner = @request.auth.id"))
        .await
        .unwrap();
    assert_eq!(ids(&mut conn, q).await.unwrap(), ["n1", "n2", "n3", "n4"]);
    // A suspension (a snapshot change, no rule change) closes it at once.
    env.policy
        .suspend(&admin(), &pk("alice"), "test", None)
        .await
        .unwrap();
    assert_eq!(ids(&mut conn, q).await.unwrap(), Vec::<String>::new());
    env.policy.unsuspend(&admin(), &pk("alice")).await.unwrap();
    assert_eq!(ids(&mut conn, q).await.unwrap().len(), 4);

    // Read-only logins cannot write.
    let ro = login(&env, "alice_ro", "alice", true).await;
    let mut ro_conn = connect(&env, &ro).await;
    assert_eq!(ids(&mut ro_conn, q).await.unwrap().len(), 4);
    let w = sqlx::raw_sql("INSERT INTO notes (id, owner, body) VALUES ('n6', 'alice', 'x')")
        .execute(&mut ro_conn)
        .await;
    assert!(w.unwrap_err().to_string().contains("permission denied"));

    // A login whose principal is gone has no claims: it sees nothing.
    let bob = login(&env, "bob", "bob", true).await;
    let mut bob_conn = connect(&env, &bob).await;
    env.data
        .delete_record(&AccessContext::system(), "people", "bob", None)
        .await
        .unwrap();
    env.policy.refresh_db_logins().await.unwrap();
    assert_eq!(ids(&mut bob_conn, q).await.unwrap(), Vec::<String>::new());
    let listed = env.policy.list_db_logins(&admin()).await.unwrap();
    let names: Vec<(&str, bool)> = listed.iter().map(|l| (l.name.as_str(), l.has_claims)).collect();
    assert_eq!(names, [("alice", true), ("alice_ro", true), ("bob", false)]);

    // Dropping a login ends its sessions.
    env.policy.delete_db_login(&admin(), "alice").await.unwrap();
    assert!(ids(&mut conn, q).await.is_err(), "the session was terminated");
    assert!(
        env.policy.get_db_login(&admin(), "alice").await.is_err(),
        "the row is gone"
    );
    let role = env
        .db
        .fetch_all(&Statement::new(
            "SELECT count(*) FROM pg_catalog.pg_roles WHERE rolname = 'fbu_alice'",
            vec![],
        ))
        .await
        .unwrap();
    assert_eq!(role.scalar().and_then(Value::as_i64), Some(0), "the role is dropped");
}

#[tokio::test]
async fn logins_are_for_administrators_and_validated() {
    let Some(env) = world().await else { return };
    let spec = |name: &str, principal: &str| LoginSpec {
        name: name.into(),
        principal: Some(principal.into()),
        ..LoginSpec::default()
    };
    let r = env.policy.create_db_login(&user("alice"), spec("mine", &pk("alice"))).await;
    assert!(matches!(r, Err(PolicyError::Denied(_))), "{r:?}");
    for (name, principal, code) in [
        ("Bad-Name", pk("alice"), "invalid_name"),
        ("ok", "admin:root".to_owned(), "invalid_principal"),
        ("ok", pk("nobody"), "invalid_principal"),
    ] {
        let r = env.policy.create_db_login(&admin(), spec(name, &principal)).await;
        assert!(matches!(&r, Err(PolicyError::Invalid { code: c, .. }) if *c == code), "{name} {principal}: {r:?}");
    }
    let r = env
        .policy
        .create_db_login(
            &admin(),
            LoginSpec {
                valid_until: Some("2027-01-01'; DROP ROLE x; --".into()),
                ..spec("ok", &pk("alice"))
            },
        )
        .await;
    assert!(matches!(r, Err(PolicyError::Invalid { code: "invalid_valid_until", .. })), "{r:?}");
    login(&env, "dup", "alice", true).await;
    let r = env.policy.create_db_login(&admin(), spec("dup", &pk("alice"))).await;
    assert!(matches!(r, Err(PolicyError::Conflict(_))), "{r:?}");
    // Rotation gives a new password and keeps the login.
    let old = env.policy.get_db_login(&admin(), "dup").await.unwrap();
    let rotated = env
        .policy
        .rotate_db_login(&admin(), "dup", Some("2030-01-01T00:00:00Z".into()))
        .await
        .unwrap();
    assert!(rotated.login.rotated_at.is_some());
    assert_eq!(rotated.login.valid_until.as_deref(), Some("2030-01-01T00:00:00Z"));
    assert_eq!(rotated.login.created_at, old.created_at);
    let until = env
        .db
        .fetch_all(&Statement::new(
            "SELECT CAST(rolvaliduntil IS NOT NULL AS integer), rolconnlimit FROM pg_catalog.pg_roles WHERE rolname = 'fbu_dup'",
            vec![],
        ))
        .await
        .unwrap();
    assert_eq!(until.rows[0][0].as_i64(), Some(1));
    assert_eq!(until.rows[0][1].as_i64(), Some(10), "default connection limit");
    // Every change is audited.
    let audited = env
        .db
        .fetch_all(&Statement::new(
            "SELECT event FROM _forgebase_auth_audit WHERE event LIKE 'db.login.%' ORDER BY id",
            vec![],
        ))
        .await
        .unwrap();
    let events: Vec<&str> = audited.rows.iter().filter_map(|r| r[0].as_str()).collect();
    assert_eq!(events, ["db.login.create", "db.login.rotate"]);
}

fn stmt(sql: &str) -> SqlStatement {
    SqlStatement {
        sql: sql.into(),
        params: vec![],
    }
}

#[tokio::test]
async fn run_as_in_the_sql_editor_sees_what_the_login_would() {
    let Some(env) = world().await else { return };
    let a = admin();
    let none = SqlOptions::default();
    let as_alice = env.policy.run_as_claims(&a, &pk("alice"), None).await.unwrap();
    let as_bob = env.policy.run_as_claims(&a, &pk("bob"), None).await.unwrap();
    let rows = |r: forgebase_data::SqlResult| -> Vec<String> {
        r.rows.iter().map(|row| row[0].as_str().unwrap().to_owned()).collect()
    };
    let q = stmt("SELECT id FROM notes ORDER BY id");
    let r = env.data.execute_sql_as(&a, q.clone(), none, Some(&as_alice)).await.unwrap();
    assert_eq!(rows(r), ["n1", "n3"]);
    let r = env.data.execute_sql_as(&a, q.clone(), none, Some(&as_bob)).await.unwrap();
    assert_eq!(rows(r), ["n2"]);
    // Without run as, the editor (its own role) sees every row.
    let r = env.data.execute_sql_as(&a, q.clone(), none, None).await.unwrap();
    assert_eq!(rows(r), ["n1", "n2", "n3"]);
    // Writes too, through the views' triggers.
    let w = env
        .data
        .execute_sql_as(&a, stmt("UPDATE notes SET body = 'y' WHERE id = 'n2'"), none, Some(&as_alice))
        .await
        .unwrap();
    assert_eq!(w.rows_affected, 0, "alice cannot reach bob's note");
    let w = env
        .data
        .execute_sql_batch_as(
            &a,
            vec![
                stmt("UPDATE notes SET body = 'y' WHERE id = 'n2'"),
                stmt("SELECT body FROM notes WHERE id = 'n2'"),
            ],
            none,
            Some(&as_bob),
        )
        .await
        .unwrap();
    assert_eq!(w[0].rows_affected, 1);
    assert_eq!(w[1].rows[0][0], json!("y"));
    // DDL is refused with run as.
    let r = env
        .data
        .execute_sql_as(&a, stmt("CREATE TABLE t (a int)"), none, Some(&as_alice))
        .await;
    assert!(matches!(r, Err(DataError::Invalid { .. })), "{r:?}");
    // Only administrators pick someone to run as; every call is audited.
    assert!(env.policy.run_as_claims(&user("alice"), &pk("bob"), None).await.is_err());
    let n = env
        .db
        .fetch_all(&Statement::new(
            "SELECT count(*) FROM _forgebase_auth_audit WHERE event = 'sql.run_as'",
            vec![],
        ))
        .await
        .unwrap();
    assert_eq!(n.scalar().and_then(Value::as_i64), Some(4));
}

#[tokio::test]
async fn the_editor_role_has_no_secret_columns_of_collections() {
    let Some(env) = world().await else { return };
    assert!(env.data.sql_editor_role().is_some(), "enabled with native RLS");
    env.collection(json!({"name": "accounts", "fields": [
        {"name": "email", "type": "text"}, {"name": "pin", "type": "password"}
    ]}))
    .await;
    let a = admin();
    let none = SqlOptions::default();
    let ok = env.data.execute_sql_as(&a, stmt("SELECT id, email FROM accounts"), none, None).await;
    assert!(ok.is_ok(), "{ok:?}");
    // `pin` is caught by the name guard; `*` and a view over it by the role.
    for sql in [
        "SELECT * FROM accounts",
        "SELECT count(*) FROM (SELECT * FROM accounts) x",
    ] {
        let r = env.data.execute_sql_as(&a, stmt(sql), none, None).await;
        assert!(matches!(r, Err(DataError::Forbidden(_))), "{sql}: {r:?}");
    }
    env.data
        .execute_sql_as(&a, stmt("CREATE VIEW all_accounts AS SELECT * FROM accounts"), none, None)
        .await
        .unwrap();
    let r = env
        .data
        .execute_sql_as(&a, stmt("SELECT count(*) FROM all_accounts"), none, None)
        .await;
    assert!(matches!(r, Err(DataError::Forbidden(_))), "a view owned by the editor: {r:?}");
    // Rows of collections are writable (no secret column named).
    env.data
        .execute_sql_as(&a, stmt("INSERT INTO accounts (id, created, updated, _version, _data, email) VALUES ('ac1', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z', 1, '{}', 'a@b.c')"), none, None)
        .await
        .unwrap();
}
