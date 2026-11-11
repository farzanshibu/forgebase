//! FB-173: native policies on every shard (ADR-0029 §3.10): the catalog
//! replay of a new shard brings its policies, schema changes regenerate
//! them inside the two-phase schema transaction, rule changes commit on
//! every set at once, startup sync covers each set, and direct-login roles
//! are created per shard and copied with the same verifier. Real, separate
//! PostgreSQL clusters; skipped without an engine.

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use forgebase_data::{AccessContext, DataService};
use forgebase_db::shard_testing::Sharded;
use forgebase_db::shards::{self, ShardSetup};
use forgebase_db::{Backend, Database, Statement, Value};
use forgebase_policy::PolicyService;
use forgebase_policy::native::{self, LoginRole, NativeMode};
use forgebase_policy::store::PolicyInput;
use forgebase_schema::Collection;
use serde_json::json;

async fn rows(db: &Database, sql: &str) -> Vec<String> {
    db.fetch_all(&Statement::new(sql, vec![]))
        .await
        .unwrap()
        .rows
        .iter()
        .filter_map(|r| r.first().and_then(Value::as_str).map(str::to_owned))
        .collect()
}

/// Every generated policy of `table`: `name: using / check`.
async fn policies(db: &Database, table: &str) -> Vec<String> {
    rows(
        db,
        &format!(
            "SELECT policyname::text || ': ' || coalesce(qual, '') || ' / ' || coalesce(with_check, '') \
             FROM pg_catalog.pg_policies WHERE schemaname = 'public' AND tablename = '{table}' ORDER BY policyname"
        ),
    )
    .await
}

fn admin() -> AccessContext {
    AccessContext::admin("root")
}

fn list_rule(rule: &str) -> PolicyInput {
    PolicyInput {
        list_rule: Some(rule.into()),
        view_rule: Some(rule.into()),
        ..PolicyInput::default()
    }
}

#[tokio::test]
async fn native_policies_follow_schema_and_rule_changes_on_every_shard() {
    let Some(mut s) = Sharded::start().await else {
        eprintln!("skipped: no PostgreSQL engine configured");
        return;
    };
    let backend: Arc<dyn Backend> = Arc::new(s.control.clone());
    let policy = PolicyService::open(backend.clone()).await.unwrap();
    let data = DataService::new(backend, policy.authorizer()).await.unwrap();
    policy.attach_data(data.clone());
    policy.enable_native_rls(NativeMode::Logins).await.unwrap();
    let sys = AccessContext::system();
    let people: Collection = serde_json::from_value(json!({"id": "people_c", "name": "people", "fields": [
        {"name": "email", "type": "text"}
    ]}))
    .unwrap();
    data.create_collection(&sys, people).await.unwrap();
    let posts: Collection = serde_json::from_value(json!({"name": "posts", "fields": [
        {"name": "title", "type": "text"},
        {"name": "owner", "type": "relation", "collection_id": "people_c"},
        {"name": "status", "type": "text"}
    ]}))
    .unwrap();
    data.create_collection(&sys, posts).await.unwrap();
    policy
        .put_policy(&admin(), "posts", &list_rule("owner = @request.auth.id || owner.email = 'x'"))
        .await
        .unwrap();
    let want = policies(&s.control, "posts").await;
    assert!(want.iter().any(|p| p.starts_with("fb_rule_select")), "{want:?}");

    // The new shard replays tables and policies.
    let (set, shard) = s.provision("s1").await;
    shards::activate_shard(
        &s.router,
        &set,
        &shard,
        ShardSetup {
            backups_dir: None,
            replay: Some(&data),
            control_conninfo: None,
        },
    )
    .await
    .unwrap();
    assert!(data.attach_shards(s.router.clone()));
    assert_eq!(policies(&shard, "posts").await, want);
    assert_eq!(
        rows(&shard, "SELECT viewname::text FROM pg_catalog.pg_views WHERE schemaname = 'api' ORDER BY 1").await,
        ["people", "posts"]
    );

    // A rule change commits on both sets as one two-phase transaction.
    policy
        .put_policy(&admin(), "posts", &list_rule("status = 'public'"))
        .await
        .unwrap();
    let changed = policies(&s.control, "posts").await;
    assert_ne!(changed, want);
    assert_eq!(policies(&shard, "posts").await, changed);
    assert_eq!(
        rows(&s.control, "SELECT purpose FROM _forgebase_xact_journal WHERE state = 'committed'").await,
        ["native policies"]
    );
    policy.delete_policy(&admin(), "posts").await.unwrap();
    assert_eq!(policies(&shard, "posts").await, policies(&s.control, "posts").await);

    // A schema change regenerates inside the schema transaction on both.
    let mut p = data.get_collection(&sys, "posts").await.unwrap();
    p.fields.push(
        serde_json::from_value::<Collection>(json!({"name": "x", "fields": [{"name": "score", "type": "number"}]}))
            .unwrap()
            .fields
            .remove(0),
    );
    data.update_collection(&sys, "posts", p).await.unwrap();
    assert_eq!(policies(&shard, "posts").await, policies(&s.control, "posts").await);
    let api_cols = "SELECT column_name::text FROM information_schema.columns WHERE table_schema = 'api' \
                    AND table_name = 'posts' ORDER BY 1";
    assert_eq!(rows(&shard, api_cols).await, rows(&s.control, api_cols).await);
    assert!(rows(&shard, api_cols).await.contains(&"score".to_owned()));

    // Startup sync brings a set that drifted back (each set has its own fingerprint).
    shard
        .run_autocommit(&["DROP POLICY fb_rule_select ON public.posts".into(), "DELETE FROM forgebase._forgebase_meta WHERE key = 'native_rls.fingerprint'".into()])
        .await
        .unwrap();
    policy.sync_native_rls().await.unwrap();
    assert_eq!(policies(&shard, "posts").await, policies(&s.control, "posts").await);
}

#[tokio::test]
async fn direct_login_roles_live_on_their_shard_and_move_with_their_verifier() {
    let Some(mut s) = Sharded::start().await else {
        eprintln!("skipped: no PostgreSQL engine configured");
        return;
    };
    let (_, a) = s.provision("s1").await;
    let (_, b) = s.provision("s2").await;
    a.migrate(None).await.unwrap();
    b.migrate(None).await.unwrap();
    // A verifier made by the server (the password never reaches ForgeBase).
    a.run_autocommit(&[
        "SET password_encryption = 'scram-sha-256'".into(),
        "CREATE ROLE fb173_tmp PASSWORD 'correct horse'".into(),
    ])
    .await
    .unwrap();
    let verifier = native::login_role_verifier(&a, "fb173_tmp").await.unwrap().expect("verifier");
    assert!(verifier.starts_with("SCRAM-SHA-256$"));
    native::create_login_role(
        &a,
        &LoginRole {
            name: "app_ann",
            verifier: &verifier,
        },
    )
    .await
    .unwrap();
    let role = "SELECT rolname::text || ':' || rolcanlogin::text || ':' || rolsuper::text FROM pg_catalog.pg_roles WHERE rolname = 'app_ann'";
    assert_eq!(rows(&a, role).await, ["app_ann:true:false"]);
    let member = "SELECT r.rolname::text FROM pg_catalog.pg_auth_members m JOIN pg_catalog.pg_roles r ON r.oid = m.roleid \
                  JOIN pg_catalog.pg_roles u ON u.oid = m.member WHERE u.rolname = 'app_ann'";
    assert_eq!(rows(&a, member).await, [native::LOGIN_GROUP]);
    // Only on its tenant's shard.
    assert!(rows(&b, role).await.is_empty());
    // A move recreates it on the target with the same verifier.
    assert!(native::copy_login_role(&a, &b, "app_ann", None).await.unwrap());
    assert_eq!(native::login_role_verifier(&b, "app_ann").await.unwrap(), Some(verifier.clone()));
    assert_eq!(rows(&b, member).await, [native::LOGIN_GROUP]);
    // Creating again updates the verifier; dropping removes it.
    native::create_login_role(
        &b,
        &LoginRole {
            name: "app_ann",
            verifier: &verifier,
        },
    )
    .await
    .unwrap();
    assert!(native::drop_login_role(&a, "app_ann").await.unwrap());
    assert!(!native::drop_login_role(&a, "app_ann").await.unwrap());
    assert!(rows(&a, role).await.is_empty());
    // Bad names and plain passwords are refused before any SQL runs.
    let err = native::create_login_role(
        &b,
        &LoginRole {
            name: "x\"; DROP ROLE postgres; --",
            verifier: &verifier,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(forgebase_core::ErrorCode::code(&err), "invalid_login_name");
    let err = native::create_login_role(
        &b,
        &LoginRole {
            name: "app_bob",
            verifier: "hunter2",
        },
    )
    .await
    .unwrap_err();
    assert_eq!(forgebase_core::ErrorCode::code(&err), "invalid_login_verifier");
}
