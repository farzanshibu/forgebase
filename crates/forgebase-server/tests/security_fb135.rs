//! FB-135 (rls-coverage G11 data-core part, G12, G13): SQL editor secrets,
//! `describe_table` on system tables, and record-history visibility, over
//! HTTP against the real server. Each test failed before FB-135: secret
//! columns came back in clear and `show_secrets` was ignored (not audited),
//! a `schema:read` holder could describe every `_forgebase_*` table, and the
//! history of a record was visible without a visibility check (unknown ids
//! answered `[]`) or not at all once the record was deleted.
//! Binds `127.0.0.1:0`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod security_common;

use security_common::{Env, Resp};
use serde_json::{Value as Json, json};

/// A realm role bound to `principal` (as an administrator).
async fn realm_role(env: &Env, admin: &str, principal: &str, key: &str, allow: &[&str]) {
    let r = env
        .as_(
            admin,
            "PUT",
            "/api/v1/roles",
            &[],
            Some(json!({"key": key, "allow": allow, "deny": []})),
        )
        .await;
    assert_eq!(r.status, 200, "role: {}", r.text());
    let r = env
        .as_(
            admin,
            "PUT",
            "/api/v1/realm-bindings",
            &[],
            Some(json!({"principal": principal, "roles": [key]})),
        )
        .await;
    assert_eq!(r.status, 200, "binding: {}", r.text());
}

async fn collection(env: &Env, admin: &str, body: Json, rules: Option<Json>) {
    let name = body["name"].as_str().unwrap().to_owned();
    let r = env.as_(admin, "POST", "/api/v1/collections", &[], Some(body)).await;
    assert_eq!(r.status, 201, "{}", r.text());
    if let Some(rules) = rules {
        let r = env
            .as_(admin, "PUT", &format!("/api/v1/policies/{name}"), &[], Some(rules))
            .await;
        assert_eq!(r.status, 200, "{}", r.text());
    }
}

async fn sql(env: &Env, token: &str, body: Json) -> Resp {
    env.as_(token, "POST", "/api/v1/sql", &[], Some(body)).await
}

/// `sql.show_secrets` entries of the auth audit log.
async fn shown(env: &Env, admin: &str) -> Vec<Json> {
    let r = env
        .as_(admin, "GET", "/api/auth/audit?event=sql.show_secrets", &[], None)
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    r.json()["items"].as_array().cloned().unwrap()
}

fn column(r: &Json, name: &str) -> Vec<Json> {
    let i = r["columns"]
        .as_array()
        .unwrap()
        .iter()
        .position(|c| c == name)
        .unwrap_or_else(|| panic!("no column {name}: {r}"));
    r["rows"].as_array().unwrap().iter().map(|row| row[i].clone()).collect()
}

// ---------------------------------------------------------------------------
// G12: describe_table
// ---------------------------------------------------------------------------

#[tokio::test]
async fn describe_table_hides_system_tables_from_non_privileged_callers() {
    let env = Env::new().await;
    let admin = env.admin("fb135-admin@example.com").await;
    let (reader, reader_pk) = env.user("schema-reader@example.com").await;
    realm_role(&env, &admin, &reader_pk, "schema_reader", &["schema:read"]).await;
    collection(
        &env,
        &admin,
        json!({"name": "notes", "fields": [{"name": "title", "type": "text"}]}),
        None,
    )
    .await;

    // The grant works: tables and a collection's table are described.
    let r = env.as_(&reader, "GET", "/api/v1/sql/tables", &[], None).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(!r.text().contains("_forgebase_"), "list_tables: {}", r.text());
    let r = env.as_(&reader, "GET", "/api/v1/sql/tables/notes", &[], None).await;
    assert_eq!(r.status, 200, "{}", r.text());

    // System tables answer exactly like a missing table, in any spelling.
    let missing = env
        .as_(&reader, "GET", "/api/v1/sql/tables/no_such_table", &[], None)
        .await;
    assert_eq!(missing.status, 404, "{}", missing.text());
    for name in [
        "_forgebase_auth_audit",
        "_FORGEBASE_AUTH_AUDIT",
        "_forgebase_collections",
        "_forgebase_record_versions",
        "sqlite_sequence",
    ] {
        let r = env
            .as_(&reader, "GET", &format!("/api/v1/sql/tables/{name}"), &[], None)
            .await;
        assert_eq!(r.status, 404, "{name}: {}", r.text());
        assert_eq!(r.code(), missing.code(), "{name}: {}", r.text());
        assert!(!r.text().contains("CREATE TABLE"), "{name} leaks its DDL: {}", r.text());
    }

    // Administrators still describe them.
    let r = env
        .as_(&admin, "GET", "/api/v1/sql/tables/_forgebase_auth_audit", &[], None)
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["name"], "_forgebase_auth_audit");
    env.shutdown().await;
}

// ---------------------------------------------------------------------------
// G11: SQL editor secrets
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sql_editor_masks_secret_columns_unless_show_secrets() {
    let env = Env::new().await;
    let admin = env.admin("fb135-sql@example.com").await;
    let _ = env.user("hash-owner@example.com").await;

    // `SELECT *` returns the columns, masked.
    let r = sql(&env, &admin, json!({"sql": "SELECT * FROM users"})).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let j = r.json();
    assert_eq!(j["secrets_masked"], true, "{j}");
    for col in ["password", "token_key"] {
        let values = column(&j, col);
        assert!(!values.is_empty());
        assert!(values.iter().all(|v| v == "********"), "{col}: {values:?}");
    }
    assert!(!r.text().contains("$argon2"), "a password hash leaked: {}", r.text());
    assert_eq!(
        column(&j, "email")[0],
        "hash-owner@example.com",
        "other columns are kept"
    );

    // Nested JSON documents are masked by key.
    collection(
        &env,
        &admin,
        json!({"name": "configs", "fields": [{"name": "meta", "type": "json"}]}),
        None,
    )
    .await;
    let r = env
        .as_(
            &admin,
            "POST",
            "/api/v1/collections/configs/records",
            &[],
            Some(json!({"meta": {"smtp": {"user": "mailer", "password": "hunter2-smtp"}}})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.text());
    let r = sql(&env, &admin, json!({"sql": "SELECT meta FROM configs"})).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(!r.text().contains("hunter2-smtp"), "{}", r.text());
    assert!(r.text().contains("mailer"), "{}", r.text());

    // Naming a secret column needs show_secrets: aliases, expressions,
    // filters (an oracle), JSON paths and copies into readable columns.
    for stmt in [
        "SELECT email, password FROM users",
        "SELECT password AS p FROM users",
        "SELECT hex(\"token_key\") FROM users",
        "SELECT id FROM users WHERE password LIKE '$argon2%'",
        "SELECT json_extract(data, '$.token_key') FROM _forgebase_record_versions",
        "UPDATE users SET name = token_key",
    ] {
        let r = sql(&env, &admin, json!({"sql": stmt})).await;
        assert_eq!(r.status, 403, "{stmt}: {}", r.text());
        assert!(r.text().contains("show_secrets"), "{stmt}: {}", r.text());
    }
    let r = env
        .as_(
            &admin,
            "POST",
            "/api/v1/sql/batch",
            &[],
            Some(json!({"statements": [{"sql": "SELECT 1"}, {"sql": "SELECT token_key FROM users"}]})),
        )
        .await;
    assert_eq!(r.status, 403, "batch: {}", r.text());
    // A plan holds no values.
    let r = env
        .as_(
            &admin,
            "POST",
            "/api/v1/sql/explain",
            &[],
            Some(json!({"sql": "SELECT id FROM users WHERE token_key = 'x'"})),
        )
        .await;
    assert_eq!(r.status, 200, "explain: {}", r.text());

    // Nothing has been shown yet, so nothing is audited.
    assert!(shown(&env, &admin).await.is_empty());

    // show_secrets returns the values and is audited.
    let r = sql(
        &env,
        &admin,
        json!({"sql": "SELECT email, password, token_key FROM users", "show_secrets": true}),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let j = r.json();
    assert!(j.get("secrets_masked").is_none(), "{j}");
    assert!(
        column(&j, "password")[0].as_str().unwrap().starts_with("$argon2"),
        "{j}"
    );
    assert_ne!(column(&j, "token_key")[0], "********");
    let r = env
        .as_(
            &admin,
            "POST",
            "/api/v1/sql/batch",
            &[],
            Some(json!({"statements": [{"sql": "SELECT token_key FROM users"}], "show_secrets": true})),
        )
        .await;
    assert_eq!(r.status, 200, "batch: {}", r.text());
    assert_ne!(r.json()["items"][0]["rows"][0][0], "********");

    let entries = shown(&env, &admin).await;
    assert_eq!(entries.len(), 2, "{entries:?}");
    for e in &entries {
        assert_eq!(e["event"], "sql.show_secrets");
        assert_eq!(e["outcome"], "success");
        assert!(e["actor"].as_str().unwrap().starts_with("admin:"), "{e}");
    }
    assert!(
        entries
            .iter()
            .any(|e| e.to_string().contains("SELECT email, password, token_key FROM users")),
        "the statement is on record: {entries:?}"
    );

    // Non-administrators still cannot use the editor at all.
    let (user, _) = env.user("no-sql@example.com").await;
    let r = sql(&env, &user, json!({"sql": "SELECT 1", "show_secrets": true})).await;
    assert_eq!(r.status, 403, "{}", r.text());
    assert_eq!(shown(&env, &admin).await.len(), 2);
    env.shutdown().await;
}

// ---------------------------------------------------------------------------
// G13: record versions
// ---------------------------------------------------------------------------

async fn create(env: &Env, token: &str, coll: &str, headers: &[(&str, &str)], body: Json) -> String {
    let r = env
        .as_(
            token,
            "POST",
            &format!("/api/v1/collections/{coll}/records"),
            headers,
            Some(body),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.text());
    r.json()["id"].as_str().unwrap().to_owned()
}

async fn versions(env: &Env, token: &str, coll: &str, id: &str, headers: &[(&str, &str)]) -> Resp {
    env.as_(
        token,
        "GET",
        &format!("/api/v1/collections/{coll}/records/{id}/versions"),
        headers,
        None,
    )
    .await
}

fn ops(r: &Resp) -> Vec<String> {
    r.json()["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["op"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn record_history_is_always_visibility_checked_including_tombstones() {
    let env = Env::new().await;
    let admin = env.admin("fb135-versions@example.com").await;
    let (alice, alice_pk) = env.user("alice-v@example.com").await;
    let (bob, _) = env.user("bob-v@example.com").await;
    let alice_id = alice_pk.rsplit('/').next().unwrap().to_owned();

    // Owner-only rules on a versioned collection.
    let own = "owner = @request.auth.id";
    collection(
        &env,
        &admin,
        json!({"name": "diaries", "versioned": true,
               "fields": [{"name": "owner", "type": "text"}, {"name": "body", "type": "text"}]}),
        Some(
            json!({"list_rule": own, "view_rule": own, "create_rule": "@request.auth.id != ''",
                    "update_rule": own, "delete_rule": own}),
        ),
    )
    .await;
    let id = create(
        &env,
        &alice,
        "diaries",
        &[],
        json!({"owner": alice_id, "body": "dear diary"}),
    )
    .await;
    let r = versions(&env, &bob, "diaries", &id, &[]).await;
    assert_eq!(r.status, 404, "a live record bob cannot see: {}", r.text());
    let r = env
        .as_(
            &alice,
            "DELETE",
            &format!("/api/v1/collections/diaries/records/{id}"),
            &[],
            None,
        )
        .await;
    assert!(r.status == 200 || r.status == 204, "{}", r.text());

    // The tombstone is judged by the last version: the owner keeps the
    // history of her deleted record, nobody else sees it.
    let r = versions(&env, &alice, "diaries", &id, &[]).await;
    assert_eq!(r.status, 200, "the owner's deleted record: {}", r.text());
    assert_eq!(ops(&r), ["create", "delete"]);
    let r = versions(&env, &bob, "diaries", &id, &[]).await;
    assert_eq!(r.status, 404, "bob reads alice's deleted history: {}", r.text());
    assert!(!r.text().contains("dear diary"));
    let r = versions(&env, &admin, "diaries", &id, &[]).await;
    assert_eq!(r.status, 200, "{}", r.text());

    // Unrestricted collections are checked too: an unknown id is not found
    // (it answered an empty history before).
    let open = "@request.auth.id != ''";
    collection(
        &env,
        &admin,
        json!({"name": "wiki", "versioned": true, "fields": [{"name": "body", "type": "text"}]}),
        Some(json!({"list_rule": open, "view_rule": open, "create_rule": open,
                    "update_rule": open, "delete_rule": open})),
    )
    .await;
    let r = versions(&env, &bob, "wiki", "nosuchrecord123", &[]).await;
    assert_eq!(r.status, 404, "unknown id: {}", r.text());
    let page = create(&env, &alice, "wiki", &[], json!({"body": "public page"})).await;
    let r = versions(&env, &bob, "wiki", &page, &[]).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(ops(&r), ["create"]);
    env.shutdown().await;
}

async fn org(env: &Env, token: &str, slug: &str) -> (String, String) {
    let r = env
        .as_(
            token,
            "POST",
            "/api/v1/orgs",
            &[],
            Some(json!({"slug": slug, "name": slug})),
        )
        .await;
    assert_eq!(r.status, 201, "org: {}", r.text());
    let id = r.json()["id"].as_str().unwrap().to_owned();
    let p = env
        .as_(
            token,
            "POST",
            &format!("/api/v1/orgs/{id}/projects"),
            &[("X-ForgeBase-Org", &id)],
            Some(json!({"slug": "app", "name": "app"})),
        )
        .await;
    assert_eq!(p.status, 201, "project: {}", p.text());
    let tenant = p.json()["environments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "production")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    (id, tenant)
}

#[tokio::test]
async fn deleted_record_history_stays_in_its_tenant() {
    let env = Env::new().await;
    let admin = env.admin("fb135-tenants@example.com").await;
    let (alice, _) = env.user("alice-t@example.com").await;
    let (bob, _) = env.user("bob-t@example.com").await;
    let (org_a, ta) = org(&env, &alice, "alpha").await;
    let (org_b, tb) = org(&env, &bob, "bravo").await;
    let open = "@request.auth.id != ''";
    collection(
        &env,
        &admin,
        json!({"name": "tickets", "versioned": true, "options": {"tenant_scoped": true},
               "fields": [{"name": "title", "type": "text"}]}),
        Some(json!({"list_rule": open, "view_rule": open, "create_rule": open,
                    "update_rule": open, "delete_rule": open})),
    )
    .await;
    let a_hdr = [("X-ForgeBase-Org", org_a.as_str()), ("X-ForgeBase-Tenant", ta.as_str())];
    let b_hdr = [("X-ForgeBase-Org", org_b.as_str()), ("X-ForgeBase-Tenant", tb.as_str())];
    let id = create(&env, &alice, "tickets", &a_hdr, json!({"title": "alpha only"})).await;
    let r = env
        .as_(
            &alice,
            "DELETE",
            &format!("/api/v1/collections/tickets/records/{id}"),
            &a_hdr,
            None,
        )
        .await;
    assert!(r.status == 200 || r.status == 204, "{}", r.text());

    let r = versions(&env, &alice, "tickets", &id, &a_hdr).await;
    assert_eq!(r.status, 200, "alice's own tenant: {}", r.text());
    assert_eq!(ops(&r), ["create", "delete"]);
    let r = versions(&env, &bob, "tickets", &id, &b_hdr).await;
    assert_eq!(r.status, 404, "another tenant: {}", r.text());
    assert!(!r.text().contains("alpha only"));
    env.shutdown().await;
}
