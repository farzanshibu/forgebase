//! Native PostgreSQL row-level security (FB-152, rls-coverage §3.7).
//!
//! * Differential: for every principal, collection and action, the rows the
//!   data core gives (ForgeBase rules only) equal the rows `forgebase_rls`
//!   gets with that caller's claims (native policies only).
//! * Spoofing: a direct login that sets claims, switches roles, writes
//!   `fb_*` policies, calls the helpers or reads system tables gets nothing.
//! * Enforce: the API path really runs under the policies.
//!
//! PostgreSQL only (`FORGEBASE_TEST_BACKEND=postgres`); on SQLite every test
//! returns at once.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::{BTreeSet, HashMap};
use std::time::Instant;

use common::{Env, admin, pk, user};
use forgebase_data::{AccessContext, Action, GetOptions, ListQuery, UpdateOptions};
use forgebase_db::{Backend, BackendKind, RunAs, Statement, TxMode, Value};
use forgebase_policy::native::{NativeMode, RLS_ROLE, claims};
use forgebase_policy::snapshot::Effect;
use forgebase_policy::store::{PolicyInput, RoleInput};
use forgebase_schema::Collection;
use serde_json::{Value as Json, json};

fn rules(list: Option<&str>, view: Option<&str>, create: Option<&str>, update: Option<&str>, delete: Option<&str>) -> PolicyInput {
    PolicyInput {
        list_rule: list.map(str::to_owned),
        view_rule: view.map(str::to_owned),
        create_rule: create.map(str::to_owned),
        update_rule: update.map(str::to_owned),
        delete_rule: delete.map(str::to_owned),
        ..PolicyInput::default()
    }
}

/// A PostgreSQL environment with native RLS on, or `None` on SQLite.
async fn pg_env(mode: NativeMode) -> Option<Env> {
    let env = Env::new().await;
    if env.db.kind() != BackendKind::Postgres {
        return None;
    }
    env.policy.enable_native_rls(mode).await.unwrap();
    Some(env)
}

struct World {
    env: Env,
    tenant_a: String,
    tenant_b: String,
}

/// Collections, rules, roles, ACL entries and records used by the tests.
async fn world(mode: NativeMode) -> Option<World> {
    let env = pg_env(mode).await?;
    for (id, email, level) in [
        ("alice", "alice@example.com", 1),
        ("bob", "bob@test.org", 9),
        ("carol", "carol@example.com", 7),
        ("dave", "dave@example.com", 2),
    ] {
        env.data
            .create_record(
                &AccessContext::system(),
                "people",
                json!({"id": id, "email": email, "level": level}),
            )
            .await
            .unwrap();
    }
    let org_a = env.org("alice", "a").await;
    let org_b = env.org("bob", "b").await;
    let (_, tenant_a) = env.project("alice", &org_a, "app").await;
    let (_, tenant_b) = env.project("bob", &org_b, "app").await;
    env.member("alice", &org_a, "carol", &["member"]).await;

    env.collection(json!({"id": "posts_c", "name": "posts", "fields": [
        {"name": "title", "type": "text"},
        {"name": "owner", "type": "relation", "collection_id": env_people(&env).await},
        {"name": "status", "type": "text"},
        {"name": "score", "type": "number"},
        {"name": "secret", "type": "text"}
    ]}))
    .await;
    let mut posts_rules = rules(
        Some("status = 'public' || owner = @request.auth.id || @request.auth.level > 5"),
        Some("owner = @request.auth.id || @request.auth.roles ?= 'editor' || owner.level >= 7"),
        Some("@request.auth.id != '' && owner = @request.auth.id"),
        Some("owner = @request.auth.id && score < 100"),
        Some("@request.auth.roles ?= 'editor' && status != 'locked'"),
    );
    posts_rules.fields.insert(
        "secret".into(),
        serde_json::from_value(json!({"read": "@request.auth.roles ?= 'editor'"})).unwrap(),
    );
    env.policy.put_policy(&admin(), "posts", &posts_rules).await.unwrap();

    env.collection(json!({"id": "notes_c", "name": "notes", "options": {"tenant_scoped": true}, "fields": [
        {"name": "title", "type": "text"},
        {"name": "author", "type": "text"}
    ]}))
    .await;
    env.policy
        .put_policy(
            &admin(),
            "notes",
            &rules(
                Some(""),
                Some("author = @request.auth.id"),
                Some("author = @request.auth.id"),
                Some("author = @request.auth.id || @request.tenant = ''"),
                None,
            ),
        )
        .await
        .unwrap();

    env.collection(json!({"id": "cfg_c", "name": "cfg", "fields": [{"name": "k", "type": "text"}]}))
        .await;
    // `cfg` has no rules: access only through permissions and ACL entries.
    env.policy
        .put_role(
            &admin(),
            None,
            &RoleInput {
                key: "editor".into(),
                allow: vec!["data:cfg:read".into()],
                ..RoleInput::default()
            },
        )
        .await
        .unwrap();
    env.policy
        .put_realm_binding(&admin(), &pk("carol"), &["editor".into()], &[])
        .await
        .unwrap();

    for (id, owner, status, score) in [
        ("p1", "alice", "public", 10),
        ("p2", "alice", "draft", 150),
        ("p3", "bob", "draft", 5),
        ("p4", "carol", "locked", 1),
        ("p5", "dave", "public", 99),
    ] {
        env.data
            .create_record(
                &AccessContext::system(),
                "posts",
                json!({"id": id, "title": id, "owner": owner, "status": status, "score": score, "secret": "s"}),
            )
            .await
            .unwrap();
    }
    for (id, t) in [("n1", &tenant_a), ("n2", &tenant_a), ("n3", &tenant_b)] {
        env.data
            .create_record(
                &AccessContext::system().with_tenant(t.as_str()),
                "notes",
                json!({"id": id, "title": id, "author": if id == "n3" { "bob" } else { "alice" }}),
            )
            .await
            .unwrap();
    }
    for id in ["c1", "c2", "c3"] {
        env.data
            .create_record(&AccessContext::system(), "cfg", json!({"id": id, "k": id}))
            .await
            .unwrap();
    }
    // Record ACL: dave may list and view c1; carol is denied c2.
    env.policy
        .put_acl(&admin(), "record", "cfg/c1", &pk("dave"), "data:cfg:read", Effect::Allow)
        .await
        .unwrap();
    env.policy
        .put_acl(&admin(), "record", "cfg/c2", &pk("carol"), "data:cfg:*", Effect::Deny)
        .await
        .unwrap();
    Some(World {
        env,
        tenant_a,
        tenant_b,
    })
}

async fn env_people(env: &Env) -> String {
    env.data
        .get_collection(&AccessContext::system(), "people")
        .await
        .unwrap()
        .id
}

fn callers(w: &World) -> Vec<(&'static str, AccessContext)> {
    let mut attrs = serde_json::Map::new();
    attrs.insert("level".into(), json!(7));
    vec![
        ("anonymous", AccessContext::anonymous()),
        ("alice", user("alice")),
        ("alice@a", user("alice").with_tenant(w.tenant_a.as_str())),
        ("bob", user("bob")),
        ("bob@a", user("bob").with_tenant(w.tenant_a.as_str())),
        ("bob@b", user("bob").with_tenant(w.tenant_b.as_str())),
        ("carol", user("carol").with_attributes(attrs)),
        ("carol@a", user("carol").with_tenant(w.tenant_a.as_str())),
        ("dave", user("dave")),
    ]
}

async fn collection(env: &Env, name: &str) -> Collection {
    env.data.get_collection(&AccessContext::system(), name).await.unwrap()
}

fn run_as(env: &Env, ctx: &AccessContext, action: Action, coll: &Collection) -> RunAs {
    RunAs::new(RLS_ROLE, claims(&env.policy.snapshot(), ctx, action, coll).to_string())
}

fn ids_of(rows: &forgebase_db::Rows) -> BTreeSet<String> {
    rows.rows.iter().map(|r| r[0].as_str().unwrap().to_owned()).collect()
}

/// Every id of a collection (as the system).
async fn all_ids(env: &Env, name: &str) -> BTreeSet<String> {
    let rows = env
        .db
        .fetch_all(&Statement::raw(format!("SELECT id FROM public.\"{name}\"")))
        .await
        .unwrap();
    ids_of(&rows)
}

async fn api_list(env: &Env, ctx: &AccessContext, name: &str) -> BTreeSet<String> {
    let q = ListQuery {
        per_page: Some(500),
        ..ListQuery::default()
    };
    match env.data.list_records(ctx, name, &q).await {
        Ok(r) => r.items.iter().map(|i| i["id"].as_str().unwrap().to_owned()).collect(),
        Err(_) => BTreeSet::new(),
    }
}

async fn native_list(env: &Env, ctx: &AccessContext, coll: &Collection) -> BTreeSet<String> {
    let rows = env
        .db
        .fetch_all_as(
            &Statement::raw(format!("SELECT id FROM public.\"{}\"", coll.name)),
            &run_as(env, ctx, Action::List, coll),
        )
        .await
        .unwrap();
    ids_of(&rows)
}

async fn native_view(env: &Env, ctx: &AccessContext, coll: &Collection, id: &str) -> bool {
    let rows = env
        .db
        .fetch_all_as(
            &Statement::new(
                format!("SELECT id FROM public.\"{}\" WHERE id = ?1", coll.name),
                vec![id.into()],
            ),
            &run_as(env, ctx, Action::View, coll),
        )
        .await
        .unwrap();
    !rows.is_empty()
}

/// Whether a write as `forgebase_rls` with the caller's claims succeeds
/// (rolled back): an update or delete must reach the row, an insert must
/// pass `WITH CHECK`.
async fn native_write(env: &Env, ctx: &AccessContext, action: Action, coll: &Collection, stmt: Statement) -> bool {
    let mut tx = env.db.begin_as(TxMode::Normal, &run_as(env, ctx, action, coll)).await.unwrap();
    let ok = match tx.execute(&stmt).await {
        Ok(r) => r.rows_affected == 1,
        Err(e) => {
            assert!(e.to_string().contains("row-level security"), "{e}");
            false
        }
    };
    tx.rollback().await.unwrap();
    ok
}

/// Same rows for list and view, same outcome for create, update and delete,
/// through the data core and through the native policies.
async fn differential(w: &World) {
    let env = &w.env;
    let mut checked = 0;
    for name in ["posts", "notes", "cfg"] {
        let coll = collection(env, name).await;
        let every = all_ids(env, name).await;
        for (who, ctx) in callers(w) {
            let api = api_list(env, &ctx, name).await;
            let native = native_list(env, &ctx, &coll).await;
            assert_eq!(api, native, "list {name} as {who}");
            for id in &every {
                let api = env.data.get_record(&ctx, name, id, &GetOptions::default()).await.is_ok();
                assert_eq!(api, native_view(env, &ctx, &coll, id).await, "view {name}/{id} as {who}");

                let set = match name {
                    "cfg" => "\"k\" = \"k\"",
                    _ => "\"title\" = \"title\"",
                };
                let native = native_write(
                    env,
                    &ctx,
                    Action::Update,
                    &coll,
                    Statement::new(format!("UPDATE public.\"{name}\" SET {set} WHERE id = ?1"), vec![id.as_str().into()]),
                )
                .await;
                let current = env
                    .data
                    .get_record(&AccessContext::system(), name, id, &GetOptions::default())
                    .await
                    .unwrap();
                let field = if name == "cfg" { "k" } else { "title" };
                let api = env
                    .data
                    .update_record(&ctx, name, id, json!({ field: current[field].clone() }), UpdateOptions::default())
                    .await
                    .is_ok();
                assert_eq!(api, native, "update {name}/{id} as {who}");

                let native = native_write(
                    env,
                    &ctx,
                    Action::Delete,
                    &coll,
                    Statement::new(format!("DELETE FROM public.\"{name}\" WHERE id = ?1"), vec![id.as_str().into()]),
                )
                .await;
                let before = env
                    .db
                    .fetch_all(&Statement::new(
                        format!("SELECT * FROM public.\"{name}\" WHERE id = ?1"),
                        vec![id.as_str().into()],
                    ))
                    .await
                    .unwrap();
                let api = env.data.delete_record(&ctx, name, id, None).await.is_ok();
                assert_eq!(api, native, "delete {name}/{id} as {who}");
                if api {
                    // Put the row back exactly as it was.
                    let cols: Vec<String> = before.columns.iter().map(|c| format!("\"{c}\"")).collect();
                    let marks: Vec<String> = (1..=cols.len()).map(|i| format!("?{i}")).collect();
                    let mut tx = env.db.begin(TxMode::Normal).await.unwrap();
                    tx.execute(&Statement::new(
                        format!("INSERT INTO public.\"{name}\" ({}) VALUES ({})", cols.join(", "), marks.join(", ")),
                        before.rows[0].clone(),
                    ))
                    .await
                    .unwrap();
                    tx.commit().await.unwrap();
                }
                checked += 4;
            }
            // Create: the data core vs an INSERT checked by the policies.
            let owner = ctx.principal.id().unwrap_or("nobody").to_owned();
            let (doc, cols, vals): (Json, &str, Vec<Value>) = match name {
                "posts" => (
                    json!({"id": "new1", "title": "t", "owner": owner, "status": "draft", "score": 1}),
                    "id, created, updated, title, owner, status, score",
                    vec!["new1".into(), "x".into(), "x".into(), "t".into(), owner.clone().into(), "draft".into(), Value::Integer(1)],
                ),
                "notes" => (
                    json!({"id": "new1", "title": "t", "author": owner}),
                    "id, created, updated, title, author, _tenant",
                    vec![
                        "new1".into(),
                        "x".into(),
                        "x".into(),
                        "t".into(),
                        owner.clone().into(),
                        ctx.tenant.clone().unwrap_or_default().into(),
                    ],
                ),
                _ => (
                    json!({"id": "new1", "k": "x"}),
                    "id, created, updated, k",
                    vec!["new1".into(), "x".into(), "x".into(), "x".into()],
                ),
            };
            let marks: Vec<String> = (1..=vals.len()).map(|i| format!("?{i}")).collect();
            let native = native_write(
                env,
                &ctx,
                Action::Create,
                &coll,
                Statement::new(format!("INSERT INTO public.\"{name}\" ({cols}) VALUES ({})", marks.join(", ")), vals),
            )
            .await;
            let api = env.data.create_record(&ctx, name, doc).await.is_ok();
            assert_eq!(api, native, "create {name} as {who}");
            if api {
                env.data
                    .delete_record(&AccessContext::system(), name, "new1", None)
                    .await
                    .unwrap();
            }
            checked += 2;
        }
    }
    assert!(checked > 300, "{checked} comparisons");
}

#[tokio::test]
async fn native_policies_give_the_rows_forgebase_rules_give() {
    let Some(w) = world(NativeMode::Logins).await else { return };
    differential(&w).await;
}

#[tokio::test]
async fn enforce_mode_gives_the_same_rows_and_runs_under_the_policies() {
    let Some(w) = world(NativeMode::Enforce).await else { return };
    differential(&w).await;
    let env = &w.env;
    // A restrictive policy written by hand narrows the API path in enforce
    // mode: proof that list and get run as forgebase_rls.
    let mut tx = env.db.begin(TxMode::Normal).await.unwrap();
    tx.execute(&Statement::raw(
        "CREATE POLICY user_narrow ON public.\"posts\" AS RESTRICTIVE FOR ALL USING (\"title\" <> 'p1')",
    ))
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let alice = user("alice");
    assert!(!api_list(env, &alice, "posts").await.contains("p1"));
    assert!(env.data.get_record(&alice, "posts", "p1", &GetOptions::default()).await.is_err());
    assert!(
        env.data
            .update_record(&alice, "posts", "p1", json!({"title": "p1"}), UpdateOptions::default())
            .await
            .is_err(),
        "the data core allows it, the policy refuses it"
    );
    // Privileged callers never run under the policies.
    assert!(api_list(env, &admin(), "posts").await.contains("p1"));
}

#[tokio::test]
async fn rule_changes_and_schema_changes_regenerate_policies() {
    let Some(w) = world(NativeMode::Logins).await else { return };
    let env = &w.env;
    let posts = collection(env, "posts").await;
    let dave = user("dave");
    assert_eq!(native_list(env, &dave, &posts).await, ["p1", "p5"].map(String::from).into());
    env.policy
        .put_policy(&admin(), "posts", &rules(Some("score > 50"), None, None, None, None))
        .await
        .unwrap();
    assert_eq!(native_list(env, &dave, &posts).await, ["p2", "p5"].map(String::from).into());
    // A schema change (new field, then a rename) keeps the policies in step.
    let mut def = posts.clone();
    def.fields.push(serde_json::from_value(json!({"name": "extra", "type": "text"})).unwrap());
    def.name = "articles".into();
    env.data.update_collection(&AccessContext::system(), "posts", def).await.unwrap();
    let articles = collection(env, "articles").await;
    assert_eq!(native_list(env, &dave, &articles).await, ["p2", "p5"].map(String::from).into());
    let views = env
        .db
        .fetch_all(&Statement::raw("SELECT viewname FROM pg_views WHERE schemaname = 'api' ORDER BY 1"))
        .await
        .unwrap();
    let names: Vec<&str> = views.rows.iter().map(|r| r[0].as_str().unwrap()).collect();
    assert!(names.contains(&"articles") && !names.contains(&"posts"), "{names:?}");
    // Locked again: nothing.
    env.policy.delete_policy(&admin(), "articles").await.unwrap();
    assert!(native_list(env, &dave, &articles).await.is_empty());
    // Dropping the collection is not blocked by its api view.
    env.data.delete_collection(&AccessContext::system(), "articles").await.unwrap();
}

/// Runs `sql` in a transaction whose session user is the direct login
/// `fbu_spoof` (the test cluster's superuser switches the session
/// authorization, as a real login would have). `Err` is the error text.
async fn as_login(env: &Env, steps: &[&str], query: &str) -> Result<i64, String> {
    let mut tx = env.db.begin(TxMode::Normal).await.unwrap();
    tx.execute(&Statement::raw("SET LOCAL SESSION AUTHORIZATION fbu_spoof"))
        .await
        .unwrap();
    for s in steps {
        if let Err(e) = tx.execute(&Statement::raw(*s)).await {
            let _ = tx.rollback().await;
            return Err(e.to_string());
        }
    }
    let out = match tx.fetch_all(&Statement::raw(query)).await {
        Ok(rows) => Ok(rows.scalar().and_then(Value::as_i64).unwrap_or(0)),
        Err(e) => Err(e.to_string()),
    };
    let _ = tx.rollback().await;
    out
}

#[tokio::test]
async fn direct_logins_cannot_spoof_their_way_in() {
    let Some(w) = world(NativeMode::Logins).await else { return };
    let env = &w.env;
    let mut tx = env.db.begin(TxMode::Normal).await.unwrap();
    tx.execute(&Statement::raw(
        "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'fbu_spoof') THEN \
         CREATE ROLE fbu_spoof LOGIN NOSUPERUSER NOBYPASSRLS IN ROLE forgebase_login; END IF; END $$",
    ))
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let posts = collection(env, "posts").await;
    let forged = claims(&env.policy.snapshot(), &user("carol"), Action::List, &posts).to_string();
    let set = format!("SET LOCAL forgebase.claims = '{}'", forged.replace('\'', "''"));
    let set_config = format!(
        "SELECT set_config('forgebase.claims', '{}', true)",
        forged.replace('\'', "''")
    );
    let count = "SELECT count(*) FROM api.\"posts\"";
    // The view works, but without claims of its own the login sees nothing,
    // whatever it sets.
    assert_eq!(as_login(env, &[], count).await, Ok(0));
    assert_eq!(as_login(env, &[&set], count).await, Ok(0));
    assert_eq!(as_login(env, &[&set_config], count).await, Ok(0));
    // The tables, the helpers and the system schema are out of reach.
    for (what, q) in [
        ("table", "SELECT count(*) FROM public.\"posts\""),
        ("claims()", "SELECT count(*) FROM (SELECT forgebase.claims()) x"),
        ("req()", "SELECT count(*) FROM (SELECT forgebase.req('auth.id')) x"),
        ("system table", "SELECT count(*) FROM forgebase._forgebase_collections"),
        ("system table on the path", "SELECT count(*) FROM _forgebase_collection_policies"),
    ] {
        assert!(as_login(env, &[], q).await.is_err(), "{what} must be refused");
    }
    let rel = env
        .db
        .fetch_all(&Statement::raw(
            "SELECT p.proname FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
             WHERE n.nspname = 'forgebase' AND p.proname LIKE 'fb\\_rel\\_%' LIMIT 1",
        ))
        .await
        .unwrap();
    let rel = rel.scalar().and_then(Value::as_str).expect("a relation helper exists").to_owned();
    assert!(
        as_login(env, &[], &format!("SELECT count(*) FROM (SELECT forgebase.\"{rel}\"('alice')) x")).await.is_err(),
        "SECURITY DEFINER helpers cannot be called with forged arguments"
    );
    for step in [
        "SET LOCAL ROLE forgebase_rls",
        "SET LOCAL ROLE postgres",
        "CREATE POLICY fb_open ON public.\"posts\" USING (true)",
        "DROP POLICY fb_gate_select ON public.\"posts\"",
        "ALTER TABLE public.\"posts\" DISABLE ROW LEVEL SECURITY",
        "INSERT INTO api.\"posts\" (title, owner, status) VALUES ('x', 'alice', 'public')",
    ] {
        assert!(as_login(env, &[step], count).await.is_err(), "{step} must fail");
    }
    // The server's own sessions may set claims; the same claims then work.
    let rows = env
        .db
        .fetch_all_as(
            &Statement::raw("SELECT count(*) FROM api.\"posts\""),
            &RunAs::new(RLS_ROLE, forged),
        )
        .await
        .unwrap();
    assert!(rows.scalar().and_then(Value::as_i64).unwrap() > 0);
    // Secret columns are not in the view; masked ones read as NULL.
    let cols = env
        .db
        .fetch_all(&Statement::raw(
            "SELECT column_name FROM information_schema.columns WHERE table_schema = 'api' AND table_name = 'people'",
        ))
        .await
        .unwrap();
    assert!(!cols.rows.is_empty());
    let alice = run_as(env, &user("alice"), Action::List, &posts);
    let masked = env
        .db
        .fetch_all_as(&Statement::raw("SELECT count(secret) FROM api.\"posts\""), &alice)
        .await
        .unwrap();
    assert_eq!(masked.scalar().and_then(Value::as_i64), Some(0), "alice is no editor");
    let carol = run_as(env, &user("carol"), Action::List, &posts);
    let shown = env
        .db
        .fetch_all_as(&Statement::raw("SELECT count(secret) FROM api.\"posts\""), &carol)
        .await
        .unwrap();
    assert!(shown.scalar().and_then(Value::as_i64).unwrap() > 0, "carol is an editor");
}

#[tokio::test]
async fn off_mode_forces_rls_without_policies() {
    let Some(env) = pg_env(NativeMode::Off).await else { return };
    env.collection(json!({"name": "things", "fields": [{"name": "t", "type": "text"}]}))
        .await;
    env.policy
        .put_policy(&admin(), "things", &rules(Some(""), Some(""), None, None, None))
        .await
        .unwrap();
    env.data
        .create_record(&AccessContext::system(), "things", json!({"t": "x"}))
        .await
        .unwrap();
    let coll = collection(&env, "things").await;
    let anyone = user("alice");
    assert_eq!(api_list(&env, &anyone, "things").await.len(), 1, "the API path is unchanged");
    let flags = env
        .db
        .fetch_all(&Statement::raw(
            "SELECT relrowsecurity, relforcerowsecurity FROM pg_class WHERE oid = 'public.things'::regclass",
        ))
        .await
        .unwrap();
    assert_eq!(flags.rows[0], vec![Value::Integer(1), Value::Integer(1)]);
    let policies = env
        .db
        .fetch_all(&Statement::raw("SELECT count(*) FROM pg_policies WHERE tablename = 'things'"))
        .await
        .unwrap();
    assert_eq!(policies.scalar().and_then(Value::as_i64), Some(0));
    let _ = coll;
}

/// Overhead of `enforce` against `logins` (the API path as the server):
/// `cargo test -p forgebase-policy --test native_rls -- --ignored --nocapture`.
#[tokio::test]
#[ignore = "measurement, run by hand"]
async fn overhead() {
    let mut out = HashMap::new();
    for mode in [NativeMode::Logins, NativeMode::Enforce] {
        let Some(env) = pg_env(mode).await else { return };
        env.collection(json!({"name": "items", "fields": [
            {"name": "owner", "type": "text"}, {"name": "n", "type": "number"}
        ]}))
        .await;
        env.policy
            .put_policy(
                &admin(),
                "items",
                &rules(
                    Some("owner = @request.auth.id"),
                    Some("owner = @request.auth.id"),
                    Some("owner = @request.auth.id"),
                    Some("owner = @request.auth.id"),
                    None,
                ),
            )
            .await
            .unwrap();
        let mut tx = env.db.begin(TxMode::Normal).await.unwrap();
        tx.execute(&Statement::raw(
            "INSERT INTO public.items (id, created, updated, owner, n) \
             SELECT 'i' || g, 'x', 'x', CASE WHEN g % 10 = 0 THEN 'alice' ELSE 'u' || g END, g FROM generate_series(1, 10000) g",
        ))
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let alice = user("alice");
        let q = ListQuery {
            per_page: Some(50),
            ..ListQuery::default()
        };
        for _ in 0..20 {
            env.data.list_records(&alice, "items", &q).await.unwrap();
        }
        let n = 300;
        let t = Instant::now();
        for _ in 0..n {
            env.data.list_records(&alice, "items", &q).await.unwrap();
        }
        let list = t.elapsed().as_secs_f64() * 1000.0 / f64::from(n);
        let t = Instant::now();
        for i in 0..n {
            let id = format!("i{}", (i % 1000 + 1) * 10);
            env.data
                .get_record(&alice, "items", &id, &GetOptions::default())
                .await
                .unwrap();
        }
        let get = t.elapsed().as_secs_f64() * 1000.0 / f64::from(n);
        let t = Instant::now();
        for i in 0..n {
            let id = format!("i{}", (i % 1000 + 1) * 10);
            env.data
                .update_record(&alice, "items", &id, json!({"n": i}), UpdateOptions::default())
                .await
                .unwrap();
        }
        let update = t.elapsed().as_secs_f64() * 1000.0 / f64::from(n);
        out.insert(mode.as_str(), (list, get, update));
    }
    for (mode, (list, get, update)) in &out {
        println!("native_rls={mode}: list {list:.3} ms, get {get:.3} ms, update {update:.3} ms per call");
    }
}
