//! FB-134: resource-scoped engine permissions (`engine:<action>:<name>`) and
//! engine resource rules, through `Authorizer::authorize_resource`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeMap;

use common::{Env, admin, pk, user};
use forgebase_data::{
    AccessContext, Action, Authorizer, Decision, DefaultAuthorizer, ResourceFieldType, ResourceKind, ResourceOp,
    ResourceRef, ResourceSchema,
};
use forgebase_db::{Backend, SqliteDialect, Statement};
use forgebase_policy::store::{ResourceRulesInput, RoleInput};

fn ns(name: &str, op: ResourceOp) -> ResourceRef {
    ResourceRef::new(ResourceKind::KvNamespace, name, op)
}

fn rules(pairs: &[(&str, Option<&str>)]) -> ResourceRulesInput {
    ResourceRulesInput {
        rules: pairs
            .iter()
            .map(|(a, r)| ((*a).to_owned(), r.map(str::to_owned)))
            .collect::<BTreeMap<_, _>>(),
    }
}

/// A realm role with `allow` / `deny` bound to `who`.
async fn grant(env: &Env, who: &str, allow: &[&str], deny: &[&str]) {
    let key = format!("r_{who}");
    env.policy
        .put_role(
            &admin(),
            None,
            &RoleInput {
                key: key.clone(),
                allow: allow.iter().map(|s| (*s).to_owned()).collect(),
                deny: deny.iter().map(|s| (*s).to_owned()).collect(),
                ..RoleInput::default()
            },
        )
        .await
        .unwrap();
    env.policy
        .put_realm_binding(&admin(), &pk(who), &[key], &[])
        .await
        .unwrap();
}

fn is_allow(d: &Decision) -> bool {
    matches!(d, Decision::Allow)
}

#[tokio::test]
async fn the_plain_engine_key_still_grants_every_resource() {
    let env = Env::new().await;
    let a = env.policy.authorizer();
    grant(&env, "kim", &["engine:kv_read"], &[]).await;
    let kim = user("kim");
    // Existing behaviour: the global check is unchanged.
    assert!(is_allow(&a.authorize(&kim, Action::KvRead, None)));
    for name in ["cache", "sessions:eu", "a.b"] {
        assert!(
            is_allow(&a.authorize_resource(&kim, Action::KvRead, &ns(name, ResourceOp::View))),
            "{name}"
        );
    }
    // Another action is not granted.
    assert_eq!(
        a.authorize_resource(&kim, Action::KvWrite, &ns("cache", ResourceOp::Update)),
        Decision::Deny
    );
}

#[tokio::test]
async fn a_resource_key_grants_that_resource_only() {
    let env = Env::new().await;
    let a = env.policy.authorizer();
    grant(
        &env,
        "rita",
        &["engine:kv_read:cache", "engine:queue_consume:orders.*"],
        &[],
    )
    .await;
    let rita = user("rita");
    assert!(is_allow(&a.authorize_resource(
        &rita,
        Action::KvRead,
        &ns("cache", ResourceOp::List)
    )));
    assert_eq!(
        a.authorize_resource(&rita, Action::KvRead, &ns("cache2", ResourceOp::List)),
        Decision::Deny
    );
    assert_eq!(
        a.authorize_resource(&rita, Action::KvRead, &ns("Cache", ResourceOp::List)),
        Decision::Deny,
        "names are case-sensitive"
    );
    // A resource grant is not a global grant.
    assert_eq!(a.authorize(&rita, Action::KvRead, None), Decision::Deny);
    let sub = |n: &str| ResourceRef::new(ResourceKind::Subscription, n, ResourceOp::View);
    assert!(is_allow(&a.authorize_resource(
        &rita,
        Action::QueueConsume,
        &sub("orders.billing")
    )));
    assert_eq!(
        a.authorize_resource(&rita, Action::QueueConsume, &sub("payments")),
        Decision::Deny
    );
}

#[tokio::test]
async fn a_deny_of_a_resource_or_of_the_base_key_wins() {
    let env = Env::new().await;
    let a = env.policy.authorizer();
    grant(&env, "dan", &["engine:kv_read"], &["engine:kv_read:secret*"]).await;
    grant(&env, "eve", &["engine:kv_read:*"], &["engine:kv_read"]).await;
    let dan = user("dan");
    assert!(is_allow(&a.authorize_resource(
        &dan,
        Action::KvRead,
        &ns("cache", ResourceOp::View)
    )));
    assert_eq!(
        a.authorize_resource(&dan, Action::KvRead, &ns("secrets", ResourceOp::View)),
        Decision::Deny
    );
    // A public rule does not beat an explicit deny.
    env.policy
        .put_resource_rules(&admin(), "kv_namespace", "secrets", &rules(&[("read", Some(""))]))
        .await
        .unwrap();
    assert_eq!(
        a.authorize_resource(&dan, Action::KvRead, &ns("secrets", ResourceOp::View)),
        Decision::Deny
    );
    assert_eq!(
        a.authorize_resource(&user("eve"), Action::KvRead, &ns("cache", ResourceOp::View)),
        Decision::Deny,
        "denying engine:kv_read denies every namespace"
    );
    let steps = env.audit_steps(&pk("dan"), 2).await;
    assert!(steps.iter().all(|s| s == "permission"), "{steps:?}");
}

#[tokio::test]
async fn without_a_permission_only_a_rule_grants_and_it_may_filter_items() {
    let env = Env::new().await;
    let a = env.policy.authorizer();
    let bob = user("bob");
    let bucket = |op| ResourceRef::new(ResourceKind::Bucket, "avatars", op);
    // Deny by default.
    assert_eq!(
        a.authorize_resource(&bob, Action::StorageRead, &bucket(ResourceOp::List)),
        Decision::Deny
    );
    env.policy
        .put_resource_rules(
            &admin(),
            "bucket",
            "avatars",
            &rules(&[
                ("read", Some("created_by = @request.auth.id")),
                ("create", Some("@request.auth.id != ''")),
                ("delete", None),
            ]),
        )
        .await
        .unwrap();
    // A predicate over the item fields, compiled with the kind's schema.
    let Decision::Filter(p) = a.authorize_resource(&bob, Action::StorageRead, &bucket(ResourceOp::List)) else {
        panic!("expected a filter");
    };
    let schema = ResourceKind::Bucket.default_schema();
    let mut b = forgebase_data::query::SqlBuilder::new(&SqliteDialect);
    let sql = schema.compile(&mut b, "o", &p).unwrap();
    assert!(sql.contains("o.\"created_by\""), "{sql}");
    assert!(
        b.params().iter().any(|v| v.as_str() == Some("bob")),
        "the caller's id is bound: {:?}",
        b.params()
    );
    // A rule over @request values only is decided in Rust.
    assert!(is_allow(&a.authorize_resource(
        &bob,
        Action::StorageWrite,
        &bucket(ResourceOp::Create)
    )));
    let anon = AccessContext::anonymous();
    assert_eq!(
        a.authorize_resource(&anon, Action::StorageWrite, &bucket(ResourceOp::Create)),
        Decision::Deny
    );
    // `null` locks; update has no rule at all.
    for op in [ResourceOp::Delete, ResourceOp::Update] {
        assert_eq!(
            a.authorize_resource(&bob, Action::StorageWrite, &bucket(op)),
            Decision::Deny,
            "{op}"
        );
    }
    // Admin actions are never granted by a rule.
    env.policy
        .put_resource_rules(
            &admin(),
            "bucket",
            "open",
            &rules(&[("read", Some("")), ("write", Some(""))]),
        )
        .await
        .unwrap();
    let open = |op| ResourceRef::new(ResourceKind::Bucket, "open", op);
    assert!(is_allow(&a.authorize_resource(
        &anon,
        Action::StorageRead,
        &open(ResourceOp::View)
    )));
    assert_eq!(
        a.authorize_resource(&bob, Action::StorageAdmin, &open(ResourceOp::Update)),
        Decision::Deny
    );
}

#[tokio::test]
async fn named_rules_override_the_wildcard_and_exact_operations_override_aliases() {
    let env = Env::new().await;
    let a = env.policy.authorizer();
    let carl = user("carl");
    let coll = |n: &str, op| ResourceRef::new(ResourceKind::VectorCollection, n, op);
    env.policy
        .put_resource_rules(&admin(), "vector_collection", "*", &rules(&[("read", Some(""))]))
        .await
        .unwrap();
    env.policy
        .put_resource_rules(
            &admin(),
            "vector_collection",
            "private",
            &rules(&[("view", Some("metadata.owner = @request.auth.id"))]),
        )
        .await
        .unwrap();
    assert!(is_allow(&a.authorize_resource(
        &carl,
        Action::VectorRead,
        &coll("docs", ResourceOp::List)
    )));
    assert!(matches!(
        a.authorize_resource(&carl, Action::VectorRead, &coll("private", ResourceOp::View)),
        Decision::Filter(_)
    ));
    assert_eq!(
        a.authorize_resource(&carl, Action::VectorRead, &coll("private", ResourceOp::List)),
        Decision::Deny,
        "the named resource's rules replace the * rules"
    );
    env.policy
        .put_resource_rules(
            &admin(),
            "vector_collection",
            "docs",
            &rules(&[("read", Some("")), ("list", Some("@request.auth.roles ?= 'reader'"))]),
        )
        .await
        .unwrap();
    assert_eq!(
        a.authorize_resource(&carl, Action::VectorRead, &coll("docs", ResourceOp::List)),
        Decision::Deny,
        "list beats read"
    );
    assert!(is_allow(&a.authorize_resource(
        &carl,
        Action::VectorRead,
        &coll("docs", ResourceOp::View)
    )));
    // Deleting the named rules falls back to the wildcard.
    env.policy
        .delete_resource_rules(&admin(), "vector_collection", "docs")
        .await
        .unwrap();
    assert!(is_allow(&a.authorize_resource(
        &carl,
        Action::VectorRead,
        &coll("docs", ResourceOp::List)
    )));
}

#[tokio::test]
async fn the_owner_of_a_resource_holds_every_permission_on_it_but_a_deny_wins() {
    let env = Env::new().await;
    let a = env.policy.authorizer();
    let sub = ResourceRef::new(ResourceKind::Subscription, "mine", ResourceOp::View).with_owner(pk("olga"));
    assert!(is_allow(&a.authorize_resource(
        &user("olga"),
        Action::QueueConsume,
        &sub
    )));
    assert_eq!(
        a.authorize_resource(&user("pete"), Action::QueueConsume, &sub),
        Decision::Deny
    );
    grant(&env, "olga", &[], &["engine:queue_consume"]).await;
    assert_eq!(
        a.authorize_resource(&user("olga"), Action::QueueConsume, &sub),
        Decision::Deny
    );
}

#[tokio::test]
async fn tenants_are_checked_before_any_rule() {
    let env = Env::new().await;
    let a = env.policy.authorizer();
    let org = env.org("alice", "acme").await;
    let (_, tenant) = env.project("alice", &org, "app").await;
    env.policy
        .put_resource_rules(&admin(), "kv_namespace", "*", &rules(&[("read", Some(""))]))
        .await
        .unwrap();
    let r = ns("cache", ResourceOp::View);
    assert!(is_allow(&a.authorize_resource(
        &user("alice").with_tenant(&tenant),
        Action::KvRead,
        &r
    )));
    assert_eq!(
        a.authorize_resource(&user("mallory").with_tenant(&tenant), Action::KvRead, &r),
        Decision::Deny,
        "not a member of the tenant's organization"
    );
    assert_eq!(
        a.authorize_resource(&user("alice").with_tenant("no-such-tenant"), Action::KvRead, &r),
        Decision::Deny
    );
    assert_eq!(
        a.authorize_resource(&user("alice").with_realm("elsewhere"), Action::KvRead, &r),
        Decision::Deny
    );
    let steps = env.audit_steps(&pk("mallory"), 1).await;
    assert_eq!(steps, ["tenant"]);
}

#[tokio::test]
async fn rules_are_validated_against_the_engine_schema_when_saved() {
    let env = Env::new().await;
    let p = &env.policy;
    let code = |r: Result<serde_json::Value, forgebase_policy::PolicyError>| {
        forgebase_core::ErrorCode::code(&r.unwrap_err()).to_owned()
    };
    assert_eq!(
        code(
            p.put_resource_rules(&admin(), "table", "x", &rules(&[("read", Some(""))]))
                .await
        ),
        "invalid_resource_kind"
    );
    assert_eq!(
        code(
            p.put_resource_rules(&admin(), "bucket", "x", &rules(&[("admin", Some(""))]))
                .await
        ),
        "invalid_action"
    );
    assert_eq!(
        code(
            p.put_resource_rules(&admin(), "bucket", "x", &rules(&[("read", Some("owner = 'a'"))]))
                .await
        ),
        "invalid_rule",
        "buckets have created_by, not owner"
    );
    assert_eq!(
        code(
            p.put_resource_rules(&admin(), "bucket", "x", &rules(&[("read", Some("size >"))]))
                .await
        ),
        "invalid_rule"
    );
    assert_eq!(
        code(
            p.put_resource_rules(&user("u"), "bucket", "x", &rules(&[("read", Some(""))]))
                .await
        ),
        "forbidden"
    );
    assert_eq!(code(p.get_resource_rules(&user("u"), "bucket", "x").await), "forbidden");
    // The engine's registration replaces the default schema.
    env.policy.authorizer().register_resource_schema(
        ResourceSchema::new(ResourceKind::Topic)
            .field("priority", ResourceFieldType::Number)
            .with_ops(&[ResourceOp::Create, ResourceOp::View]),
    );
    p.put_resource_rules(
        &admin(),
        "queue_topic",
        "jobs",
        &rules(&[("create", Some("priority < 5"))]),
    )
    .await
    .unwrap();
    assert_eq!(
        code(
            p.put_resource_rules(&admin(), "queue_topic", "jobs", &rules(&[("write", Some(""))]))
                .await
        ),
        "invalid_action",
        "write includes update and delete, which topics do not have"
    );
    let listed = p.list_resource_rules(&admin(), "queue_topic").await.unwrap();
    assert_eq!(listed["fields"][0]["name"], "priority");
    assert_eq!(listed["items"][0]["name"], "jobs");
    assert_eq!(listed["items"][0]["rules"]["create"], "priority < 5");
    let got = p.get_resource_rules(&admin(), "queue_topic", "jobs").await.unwrap();
    assert_eq!(got["rules"], serde_json::json!({"create": "priority < 5"}));
}

#[tokio::test]
async fn a_stored_rule_that_no_longer_parses_locks_its_action() {
    let env = Env::new().await;
    env.policy
        .put_resource_rules(&admin(), "graph_label", "*", &rules(&[("read", Some(""))]))
        .await
        .unwrap();
    let mut tx = env.db.begin(forgebase_db::TxMode::Normal).await.unwrap();
    tx.execute(&Statement::new(
        "INSERT INTO _forgebase_resource_rules (kind, name, action, rule, updated_by, updated_at) VALUES ('graph_label', '*', 'view', 'broken ((', 'test', 0)",
        vec![],
    ))
    .await
    .unwrap();
    tx.commit().await.unwrap();
    env.policy.reload().await.unwrap();
    let a = env.policy.authorizer();
    let label = |op| ResourceRef::new(ResourceKind::GraphLabel, "Person", op);
    assert!(is_allow(&a.authorize_resource(
        &user("x"),
        Action::GraphRead,
        &label(ResourceOp::List)
    )));
    assert_eq!(
        a.authorize_resource(&user("x"), Action::GraphRead, &label(ResourceOp::View)),
        Decision::Deny,
        "no fallback to the read alias"
    );
}

#[tokio::test]
async fn other_actions_and_other_authorizers_keep_their_answers() {
    let env = Env::new().await;
    let a = env.policy.authorizer();
    grant(&env, "jo", &["engine:read_jobs"], &[]).await;
    let jo = user("jo");
    // Not a resource action: the plain decision.
    let r = ns("x", ResourceOp::View);
    assert_eq!(
        a.authorize_resource(&jo, Action::ReadJobs, &r),
        a.authorize(&jo, Action::ReadJobs, None)
    );
    assert!(
        !env.policy.has(&jo, "engine:read_jobs:all", None),
        "read_jobs takes no resource segment"
    );
    // Authorizers without an override fall back to `authorize`.
    let d = DefaultAuthorizer;
    assert_eq!(d.authorize_resource(&jo, Action::KvRead, &r), Decision::Deny);
    assert_eq!(d.authorize_resource(&admin(), Action::KvRead, &r), Decision::Allow);
    // Privileged callers and the system pass.
    assert!(is_allow(&a.authorize_resource(&admin(), Action::KvAdmin, &r)));
    assert!(is_allow(&a.authorize_resource(
        &AccessContext::system(),
        Action::KvAdmin,
        &r
    )));
}

#[tokio::test]
async fn resource_patterns_are_accepted_in_roles_and_guarded_against_escalation() {
    let env = Env::new().await;
    let org = env.org("alice", "acme").await;
    // An org role with resource patterns.
    env.policy
        .put_role(
            &user("alice"),
            Some(&org),
            &RoleInput {
                key: "cache_reader".into(),
                allow: vec!["engine:kv_read:cache.*".into()],
                ..RoleInput::default()
            },
        )
        .await
        .unwrap();
    assert!(
        env.policy
            .put_role(
                &admin(),
                None,
                &RoleInput {
                    key: "bad".into(),
                    allow: vec!["engine:kv_read:has space".into()],
                    ..RoleInput::default()
                },
            )
            .await
            .is_err()
    );
    // A role manager holding only a resource key cannot grant the plain key
    // (which would mean every namespace), but can grant what they hold.
    env.policy
        .put_role(
            &user("alice"),
            Some(&org),
            &RoleInput {
                key: "mgr".into(),
                allow: vec!["roles:manage".into(), "engine:kv_read:cache".into()],
                ..RoleInput::default()
            },
        )
        .await
        .unwrap();
    env.member("alice", &org, "nina", &["mgr"]).await;
    let nina = user("nina").with_org(&org);
    let role = |allow: &str| RoleInput {
        key: "made_by_nina".into(),
        allow: vec![allow.to_owned()],
        ..RoleInput::default()
    };
    for wide in ["engine:kv_read", "engine:kv_read:*", "engine:kv_read:cache*"] {
        assert!(
            env.policy.put_role(&nina, Some(&org), &role(wide)).await.is_err(),
            "{wide} must be refused"
        );
    }
    env.policy
        .put_role(&nina, Some(&org), &role("engine:kv_read:cache"))
        .await
        .unwrap();
    // `/authz/check`-style dry runs see resource keys and the base rule.
    grant(&env, "kim", &["engine:kv_read"], &[]).await;
    let dry = env.policy.dry_run(
        &user("kim"),
        &forgebase_policy::Check {
            permission: "engine:kv_read:cache".into(),
            ..forgebase_policy::Check::default()
        },
    );
    assert!(dry.is_ok(), "{dry:?}");
}
