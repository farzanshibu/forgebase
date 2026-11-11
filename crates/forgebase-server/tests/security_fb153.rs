//! FB-153 (rls-coverage §3.5–3.6, G11): direct database logins, the SQL
//! editor's role and "run as", and the extensions API, end to end through
//! the HTTP API and the bundled engine's Unix socket (SCRAM logins). On
//! SQLite the endpoints answer 400 and the PostgreSQL-only tests skip.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod security_common;

use std::path::PathBuf;

use security_common::{Env, Resp};
use serde_json::{Value as Json, json};
use sqlx::postgres::PgConnectOptions;
use sqlx::{Connection, PgConnection, Row};

fn postgres() -> bool {
    matches!(
        std::env::var("FORGEBASE_TEST_BACKEND").ok().as_deref(),
        Some("postgres" | "postgresql" | "pg")
    )
}

async fn post(env: &Env, token: &str, path: &str, body: Json) -> Resp {
    env.as_(token, "POST", path, &[], Some(body)).await
}

async fn sql(env: &Env, token: &str, body: Json) -> Resp {
    post(env, token, "/api/v1/sql", body).await
}

/// Collection `notes` (owner, body) whose rules let each owner see and
/// write only their own notes; two notes for `a`, one for `b`.
async fn notes(env: &Env, admin: &str, a: &str, b: &str) {
    let r = post(
        env,
        admin,
        "/api/v1/collections",
        json!({"name": "notes", "fields": [{"name": "owner", "type": "text"}, {"name": "body", "type": "text"}]}),
    )
    .await;
    assert_eq!(r.status, 201, "{}", r.text());
    let rule = "owner = @request.auth.id";
    let r = env
        .as_(
            admin,
            "PUT",
            "/api/v1/policies/notes",
            &[],
            Some(json!({"list_rule": rule, "view_rule": rule, "create_rule": rule,
                        "update_rule": rule, "delete_rule": rule})),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    for (id, owner) in [("n1", a), ("n2", b), ("n3", a)] {
        let r = post(
            env,
            admin,
            "/api/v1/collections/notes/records",
            json!({"id": id, "owner": owner, "body": "x"}),
        )
        .await;
        assert_eq!(r.status, 201, "{}", r.text());
    }
}

fn id_of(principal: &str) -> String {
    principal.rsplit('/').next().unwrap().to_owned()
}

/// The engine's socket directory and port, from `/api/system/info`.
async fn socket(env: &Env, admin: &str) -> (PathBuf, u16) {
    let r = env.as_(admin, "GET", "/api/system/info", &[], None).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let j = r.json();
    assert_eq!(j["database"]["engine"]["tcp"], Json::Null, "no TCP by default: {j}");
    let dir = PathBuf::from(j["database"]["engine"]["socket_dir"].as_str().expect("socket dir"));
    let port = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .find_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.strip_prefix(".s.PGSQL.")
                .filter(|p| !p.ends_with(".lock"))
                .and_then(|p| p.parse().ok())
        })
        .expect("socket file");
    (dir, port)
}

async fn as_login(dir: &PathBuf, port: u16, role: &str, password: &str) -> Result<PgConnection, sqlx::Error> {
    let o = PgConnectOptions::new()
        .socket(dir)
        .port(port)
        .username(role)
        .password(password)
        .database("forgebase");
    PgConnection::connect_with(&o).await
}

async fn ids(conn: &mut PgConnection) -> Vec<String> {
    sqlx::query("SELECT id FROM notes ORDER BY id")
        .fetch_all(&mut *conn)
        .await
        .unwrap()
        .iter()
        .map(|r| r.get::<String, _>(0))
        .collect()
}

async fn audit(env: &Env, admin: &str, event: &str) -> usize {
    let r = env
        .as_(admin, "GET", &format!("/api/auth/audit?event={event}"), &[], None)
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    r.json()["items"].as_array().map_or(0, Vec::len)
}

#[tokio::test]
async fn db_logins_and_extensions_are_for_administrators_on_postgres() {
    let env = Env::new().await;
    let admin = env.admin("fb153-gate@example.com").await;
    let (user, pk) = env.user("fb153-gate-user@example.com").await;
    let r = post(&env, &user, "/api/v1/system/db-logins", json!({"name": "mine", "principal": pk})).await;
    assert!(matches!(r.status, 400 | 403), "{}", r.text());
    let r = env.as_(&user, "GET", "/api/v1/system/extensions", &[], None).await;
    assert!(matches!(r.status, 400 | 403), "{}", r.text());
    let r = sql(&env, &user, json!({"sql": "SELECT 1", "run_as": {"principal": pk}})).await;
    assert!(matches!(r.status, 400 | 403), "{}", r.text());
    if !postgres() {
        let r = post(&env, &admin, "/api/v1/system/db-logins", json!({"name": "x", "principal": pk})).await;
        assert_eq!((r.status, r.code()), (400, "db_logins_unavailable".into()), "{}", r.text());
        let r = env.as_(&admin, "GET", "/api/v1/system/extensions", &[], None).await;
        assert_eq!((r.status, r.code()), (400, "extensions_unavailable".into()), "{}", r.text());
        env.shutdown().await;
        return;
    }
    assert_eq!(r.status, 403, "only administrators run as someone");
    let r = post(&env, &user, "/api/v1/system/db-logins", json!({"name": "mine", "principal": pk})).await;
    assert_eq!(r.status, 403, "{}", r.text());
    env.shutdown().await;
}

#[tokio::test]
async fn a_direct_login_sees_only_its_rows_through_the_socket() {
    if !postgres() {
        eprintln!("skipped: PostgreSQL only");
        return;
    }
    let env = Env::new().await;
    let admin = env.admin("fb153-login@example.com").await;
    let (_, alice) = env.user("fb153-alice@example.com").await;
    let (_, bob) = env.user("fb153-bob@example.com").await;
    notes(&env, &admin, &id_of(&alice), &id_of(&bob)).await;

    let r = post(&env, &admin, "/api/v1/system/db-logins", json!({"name": "alice", "principal": alice})).await;
    assert_eq!(r.status, 201, "{}", r.text());
    let created = r.json();
    let password = created["password"].as_str().unwrap().to_owned();
    assert_eq!(created["login"]["role"], "fbu_alice");
    assert_eq!(created["login"]["readonly"], false);
    let r = env.as_(&admin, "GET", "/api/v1/system/db-logins", &[], None).await;
    assert!(!r.text().contains(&password), "the password is shown once");

    let (dir, port) = socket(&env, &admin).await;
    assert!(as_login(&dir, port, "fbu_alice", "wrong-password").await.is_err(), "SCRAM checks the password");
    let mut conn = as_login(&dir, port, "fbu_alice", &password).await.expect("login over the socket");
    let mut want = vec!["n1".to_owned(), "n3".to_owned()];
    assert_eq!(ids(&mut conn).await, want, "only alice's notes");
    assert!(
        sqlx::raw_sql("SELECT * FROM forgebase._forgebase_admins").execute(&mut conn).await.is_err(),
        "system tables are out of reach"
    );

    // Claims follow a rule change at once.
    let r = env
        .as_(
            &admin,
            "PUT",
            "/api/v1/policies/notes",
            &[],
            Some(json!({"list_rule": "", "view_rule": "", "create_rule": null, "update_rule": null, "delete_rule": null})),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    want.insert(1, "n2".into());
    assert_eq!(ids(&mut conn).await, want, "the new rule shows every note");
    assert!(
        sqlx::raw_sql("INSERT INTO notes (id, owner, body) VALUES ('n9', 'x', 'x')")
            .execute(&mut conn)
            .await
            .is_err(),
        "a locked create rule refuses the insert"
    );

    // Rotation: the old password stops working.
    let r = post(&env, &admin, "/api/v1/system/db-logins/alice/rotate", json!({})).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let fresh = r.json()["password"].as_str().unwrap().to_owned();
    assert!(as_login(&dir, port, "fbu_alice", &password).await.is_err());
    as_login(&dir, port, "fbu_alice", &fresh).await.expect("new password");

    // Deleting ends the session and the role.
    let r = env.as_(&admin, "DELETE", "/api/v1/system/db-logins/alice", &[], None).await;
    assert_eq!(r.status, 204, "{}", r.text());
    assert!(sqlx::query("SELECT 1").execute(&mut conn).await.is_err(), "the session ended");
    assert!(as_login(&dir, port, "fbu_alice", &fresh).await.is_err());
    assert_eq!(audit(&env, &admin, "db.login.create").await, 1);
    assert_eq!(audit(&env, &admin, "db.login.rotate").await, 1);
    assert_eq!(audit(&env, &admin, "db.login.delete").await, 1);
    env.shutdown().await;
}

#[tokio::test]
async fn the_sql_editor_has_no_secret_columns_and_runs_as_users() {
    if !postgres() {
        eprintln!("skipped: PostgreSQL only");
        return;
    }
    let env = Env::new().await;
    let admin = env.admin("fb153-sql@example.com").await;
    let (_, alice) = env.user("fb153-sql-alice@example.com").await;
    let (_, bob) = env.user("fb153-sql-bob@example.com").await;
    notes(&env, &admin, &id_of(&alice), &id_of(&bob)).await;

    let r = sql(&env, &admin, json!({"sql": "SELECT current_user"})).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["rows"][0][0], "forgebase_sql_admin");
    // The role refuses secret columns the name guard cannot see.
    for stmt in [
        "SELECT * FROM users",
        "SELECT password_hash FROM _forgebase_admins",
        "SELECT secret_enc FROM _forgebase_auth_keys",
        "SELECT count(*) FROM (SELECT * FROM _forgebase_auth_sessions) s",
    ] {
        let r = sql(&env, &admin, json!({"sql": stmt})).await;
        assert_eq!(r.status, 403, "{stmt}: {}", r.text());
    }
    let r = sql(&env, &admin, json!({"sql": "SELECT email FROM _forgebase_admins"})).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let r = sql(&env, &admin, json!({"sql": "SELECT email FROM users ORDER BY email"})).await;
    assert_eq!(r.status, 200, "{}", r.text());
    // Generated policies and api objects are ForgeBase's.
    for stmt in ["DROP POLICY fb_rule_select ON notes", "DROP VIEW api.notes"] {
        let r = sql(&env, &admin, json!({"sql": stmt})).await;
        assert_eq!(r.status, 403, "{stmt}: {}", r.text());
    }
    let r = sql(&env, &admin, json!({"sql": "CREATE EXTENSION hstore"})).await;
    assert!(matches!(r.status, 400 | 403), "the editor refuses extensions: {}", r.text());

    // Run as: what a direct login bound to that user would see.
    let rows = |r: &Resp| -> Vec<Json> { r.json()["rows"].as_array().unwrap().iter().map(|x| x[0].clone()).collect() };
    let q = "SELECT id FROM notes ORDER BY id";
    let r = sql(&env, &admin, json!({"sql": q, "run_as": {"principal": alice}})).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(rows(&r), [json!("n1"), json!("n3")]);
    let r = sql(&env, &admin, json!({"sql": q, "run_as": {"principal": bob}})).await;
    assert_eq!(rows(&r), [json!("n2")]);
    let r = sql(&env, &admin, json!({"sql": q})).await;
    assert_eq!(rows(&r).len(), 3, "the editor itself sees every row");
    let r = post(
        &env,
        &admin,
        "/api/v1/sql/batch",
        json!({"statements": [{"sql": "UPDATE notes SET body = 'y' WHERE id = 'n2'"}], "run_as": {"principal": alice}}),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["items"][0]["rows_affected"], 0, "alice cannot reach bob's note");
    let r = sql(&env, &admin, json!({"sql": "CREATE TABLE t (a int)", "run_as": {"principal": alice}})).await;
    assert_eq!((r.status, r.code()), (400, "run_as_ddl".into()), "{}", r.text());
    assert_eq!(audit(&env, &admin, "sql.run_as").await, 3);
    env.shutdown().await;
}

#[tokio::test]
async fn extensions_follow_the_allow_list() {
    if !postgres() {
        eprintln!("skipped: PostgreSQL only");
        return;
    }
    let env = Env::new().await;
    let admin = env.admin("fb153-ext@example.com").await;
    let r = env.as_(&admin, "GET", "/api/v1/system/extensions", &[], None).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let items = r.json()["items"].as_array().unwrap().clone();
    let find = |name: &str| items.iter().find(|e| e["name"] == name).cloned().unwrap();
    assert_eq!(find("pgcrypto")["managed_by"], "api");
    assert_eq!(find("vector")["managed_by"], "feature:vector");
    assert_eq!(find("citext")["managed_by"], "schema");
    assert!(find("citext")["installed_version"].is_string(), "the schema installs citext");

    let r = post(&env, &admin, "/api/v1/system/extensions", json!({"name": "pgcrypto"})).await;
    assert_eq!(r.status, 201, "{}", r.text());
    assert!(r.json()["installed_version"].is_string(), "{}", r.text());
    assert_eq!(r.json()["schema"], "public");
    let r = sql(&env, &admin, json!({"sql": "SELECT length(digest('x', 'sha256'))"})).await;
    assert_eq!(r.status, 200, "the editor uses it: {}", r.text());
    for (name, status, code) in [
        ("plpython3u", 400, "extension_not_allowed"),
        ("dblink", 400, "extension_not_allowed"),
        ("vector", 409, "extension_managed"),
        ("pg_cron", 409, "extension_managed"),
    ] {
        let r = post(&env, &admin, "/api/v1/system/extensions", json!({"name": name})).await;
        assert_eq!((r.status, r.code()), (status, code.into()), "{name}: {}", r.text());
    }
    let r = env.as_(&admin, "DELETE", "/api/v1/system/extensions/citext", &[], None).await;
    assert_eq!(r.status, 409, "{}", r.text());
    let r = env.as_(&admin, "DELETE", "/api/v1/system/extensions/pgcrypto", &[], None).await;
    assert_eq!(r.status, 204, "{}", r.text());
    assert_eq!(audit(&env, &admin, "db.extension.create").await, 1);
    assert_eq!(audit(&env, &admin, "db.extension.drop").await, 1);
    env.shutdown().await;
}
