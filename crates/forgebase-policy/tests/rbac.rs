//! Roles, permissions, deny precedence, inheritance, super-admin, suspension,
//! escalation guard, cache invalidation and the pipeline order (FB-21
//! addendum tests).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::{Arc, Mutex};

use common::{Env, admin, pk, user};
use forgebase_data::{AccessContext, ListQuery};
use forgebase_policy::store::{PolicyInput, RoleInput};
use forgebase_policy::{
    Check, EntitlementHook, HookOutcome, HookRequest, Hooks, PolicyError, QuotaHook, ResourceRef, Step,
};
use forgebase_policy::snapshot::Effect;
use serde_json::json;

fn step(e: PolicyError) -> Step {
    match e {
        PolicyError::Denied(d) => d.step,
        other => panic!("expected a denial, got {other:?}"),
    }
}

fn check(perm: &str, org: &str) -> Check {
    Check {
        permission: perm.into(),
        org: Some(org.into()),
        ..Check::default()
    }
}

#[tokio::test]
async fn seeded_roles_are_checked_by_permission() {
    let env = Env::new().await;
    let org = env.org("alice", "acme").await;
    env.member("alice", &org, "bob", &["viewer"]).await;
    env.member("alice", &org, "bill", &["billing_manager"]).await;
    let p = &env.policy;
    assert!(p.has(&user("alice"), "projects:delete", Some(&org)), "owner has everything");
    assert!(p.has(&user("bob"), "projects:read", Some(&org)));
    assert!(!p.has(&user("bob"), "projects:create", Some(&org)));
    assert!(p.has(&user("bill"), "billing:manage", Some(&org)));
    assert!(!p.has(&user("bill"), "projects:read", Some(&org)));
    assert!(!p.has(&user("mallory"), "org:read", Some(&org)), "non-members hold nothing");
    // Ten seeded roles; super-admin is not assignable.
    let roles = p.list_roles(&admin(), None).unwrap();
    let keys: Vec<&str> = roles.as_array().unwrap().iter().map(|r| r["key"].as_str().unwrap()).collect();
    for k in [
        "super_admin", "owner", "admin", "manager", "developer", "billing_manager", "analyst", "member", "viewer", "guest",
    ] {
        assert!(keys.contains(&k), "{k} seeded");
    }
    let err = p
        .set_member(&user("alice"), &org, &pk("bob"), &["super_admin".into()], &[])
        .await
        .unwrap_err();
    assert!(matches!(err, PolicyError::Invalid { code: "role_not_assignable", .. }), "{err:?}");
}

#[tokio::test]
async fn deny_beats_grant_at_every_level() {
    let env = Env::new().await;
    let org = env.org("alice", "acme").await;
    let p = &env.policy;
    // Role level: a custom role that allows and denies.
    p.put_role(
        &user("alice"),
        Some(&org),
        &RoleInput {
            key: "builder".into(),
            allow: vec!["projects:*".into()],
            deny: vec!["projects:delete".into()],
            ..RoleInput::default()
        },
    )
    .await
    .unwrap();
    env.member("alice", &org, "bob", &["builder"]).await;
    assert!(p.has(&user("bob"), "projects:create", Some(&org)));
    assert!(!p.has(&user("bob"), "projects:delete", Some(&org)));

    // Team level: a parent team's deny beats the owner role of a member.
    env.member("alice", &org, "carol", &["owner"]).await;
    let parent = p
        .create_team(&user("alice"), &org, "eng", "Eng", None, &[], &["teams:delete".into()])
        .await
        .unwrap();
    let child = p
        .create_team(&user("alice"), &org, "web", "Web", parent["id"].as_str(), &[], &[])
        .await
        .unwrap();
    p.add_team_member(&user("alice"), child["id"].as_str().unwrap(), &pk("carol"))
        .await
        .unwrap();
    assert!(!p.has(&user("carol"), "teams:delete", Some(&org)), "inherited team deny wins over owner");
    assert!(p.has(&user("carol"), "teams:create", Some(&org)));

    // Override level: an explicit user deny beats the owner role.
    env.member("alice", &org, "dave", &["owner"]).await;
    p.add_override(&user("alice"), Some(&org), &pk("dave"), "billing:*", Effect::Deny)
        .await
        .unwrap();
    assert!(!p.has(&user("dave"), "billing:manage", Some(&org)));

    // Realm level: a realm role's deny reaches into every org.
    p.put_role(
        &admin(),
        None,
        &RoleInput {
            key: "no_ai".into(),
            deny: vec!["ai:*".into()],
            ..RoleInput::default()
        },
    )
    .await
    .unwrap();
    p.put_realm_binding(&admin(), &pk("dave"), &["no_ai".into()], &[]).await.unwrap();
    assert!(!p.has(&user("dave"), "ai:generate", Some(&org)));

    // Resource level: an ACL deny beats the owner role on that project.
    let (project, _) = env.project("alice", &org, "site").await;
    env.member("alice", &org, "erin", &["owner"]).await;
    p.put_acl(&user("alice"), "project", &project, &pk("erin"), "projects:read", Effect::Deny)
        .await
        .unwrap();
    let e = p.get_project(&user("erin"), &project).unwrap_err();
    assert_eq!(step(e), Step::Resource);
    assert!(p.get_project(&user("alice"), &project).is_ok());

    // Record level: an ACL deny on one record beats a public rule.
    env.collection(json!({"name": "posts", "fields": [{"name": "title", "type": "text"}]}))
        .await;
    p.put_policy(
        &admin(),
        "posts",
        &PolicyInput {
            list_rule: Some(String::new()),
            view_rule: Some(String::new()),
            ..PolicyInput::default()
        },
    )
    .await
    .unwrap();
    for id in ["p1", "p2"] {
        env.data
            .create_record(&AccessContext::system(), "posts", json!({"id": id, "title": id}))
            .await
            .unwrap();
    }
    p.put_acl(&admin().with_org(&org), "record", "posts/p1", &pk("frank"), "data:posts:read", Effect::Deny)
        .await
        .unwrap();
    let seen = env.data.list_records(&user("frank"), "posts", &ListQuery::default()).await.unwrap();
    let ids: Vec<&str> = seen.items.iter().map(|r| r["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["p2"]);
    assert!(env.data.get_record(&user("frank"), "posts", "p1", &Default::default()).await.is_err());
}

#[tokio::test]
async fn team_roles_are_inherited_and_changes_apply_at_once() {
    let env = Env::new().await;
    let org = env.org("alice", "acme").await;
    let p = &env.policy;
    env.member("alice", &org, "bob", &["guest"]).await;
    let parent = p
        .create_team(&user("alice"), &org, "platform", "Platform", None, &["developer".into()], &[])
        .await
        .unwrap();
    let child = p
        .create_team(&user("alice"), &org, "infra", "Infra", parent["id"].as_str(), &[], &[])
        .await
        .unwrap();
    let child = child["id"].as_str().unwrap().to_owned();
    assert!(!p.has(&user("bob"), "api_keys:create", Some(&org)));
    p.add_team_member(&user("alice"), &child, &pk("bob")).await.unwrap();
    assert!(p.has(&user("bob"), "api_keys:create", Some(&org)), "inherits the parent's developer role");
    let me = p.my_permissions(&user("bob"), Some(&org)).unwrap();
    assert!(me["roles"].as_array().unwrap().contains(&json!("developer")));
    p.remove_team_member(&user("alice"), &child, &pk("bob")).await.unwrap();
    assert!(!p.has(&user("bob"), "api_keys:create", Some(&org)), "cache invalidated on change");
    env.member("alice", &org, "bob", &["admin"]).await;
    assert!(p.has(&user("bob"), "users:invite", Some(&org)));
    p.remove_member(&user("alice"), &org, &pk("bob")).await.unwrap();
    assert!(!p.has(&user("bob"), "org:read", Some(&org)));
    p.set_member_status(&user("alice"), &org, &pk("alice"), true).await.unwrap();
}

#[tokio::test]
async fn super_admin_is_a_platform_capability_not_owner() {
    let env = Env::new().await;
    let a = env.org("alice", "a").await;
    let b = env.org("bob", "b").await;
    let p = &env.policy;
    // The owner of A has nothing in B.
    assert_eq!(step(p.get_org(&user("alice"), &b).unwrap_err()), Step::Tenant);
    assert!(!p.has(&user("alice"), "org:read", Some(&b)));
    // Platform operations are refused to owners ...
    assert!(p.create_realm(&user("alice"), "eu", "EU").await.is_err());
    assert!(p.suspend(&user("alice"), &pk("bob"), "x", None).await.is_err());
    assert!(p.get_policy(&user("alice"), "people").await.is_err());
    assert!(p.put_role(&user("alice"), None, &RoleInput { key: "x".into(), ..RoleInput::default() }).await.is_err());
    // ... and allowed to super-admins, everywhere.
    assert!(p.get_org(&admin(), &a).is_ok() && p.get_org(&admin(), &b).is_ok());
    p.create_realm(&admin(), "eu", "EU").await.unwrap();
    // Cross-realm is denied to everyone but super-admins.
    let eu_user = user("alice").with_realm("eu");
    assert_eq!(step(p.create_org(&eu_user, "x", "x").await.unwrap_err()), Step::Tenant);
    assert!(p.create_org(&admin().with_realm("eu"), "eu-org", "EU org").await.is_ok());
    let list = p.list_orgs(&admin().with_realm("eu")).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1, "realms do not see each other's orgs");
}

#[tokio::test]
async fn suspended_accounts_are_denied_everywhere() {
    let env = Env::new().await;
    let org = env.org("alice", "acme").await;
    let (_, tenant) = env.project("alice", &org, "app").await;
    env.collection(json!({"name": "open", "fields": [{"name": "t", "type": "text"}]})).await;
    env.policy
        .put_policy(&admin(), "open", &PolicyInput { list_rule: Some(String::new()), ..PolicyInput::default() })
        .await
        .unwrap();
    let p = &env.policy;
    p.suspend(&admin(), &pk("alice"), "abuse", None).await.unwrap();
    assert!(!p.has(&user("alice"), "org:read", Some(&org)));
    assert_eq!(step(p.check(&user("alice"), &check("org:read", &org)).unwrap_err()), Step::Authentication);
    assert!(p.create_org(&user("alice"), "other", "Other").await.is_err());
    let ctx = user("alice").with_tenant(&tenant);
    assert!(env.data.list_records(&ctx, "open", &ListQuery::default()).await.is_err(), "even public data");
    assert!(env.audit_steps(&pk("alice"), 3).await.iter().all(|s| s == "authentication"));
    p.unsuspend(&admin(), &pk("alice")).await.unwrap();
    assert!(p.has(&user("alice"), "org:read", Some(&org)));
    // A suspension with an end in the past is over.
    p.suspend(&admin(), &pk("alice"), "", Some(1)).await.unwrap();
    assert!(p.has(&user("alice"), "org:read", Some(&org)));
}

#[tokio::test]
async fn nobody_grants_what_they_do_not_hold() {
    let env = Env::new().await;
    let org = env.org("alice", "acme").await;
    env.member("alice", &org, "bob", &["admin"]).await;
    let p = &env.policy;
    // An admin cannot make anyone (themselves included) an owner.
    let e = p.set_member(&user("bob"), &org, &pk("bob"), &["owner".into()], &[]).await.unwrap_err();
    assert_eq!(step(e), Step::Permission);
    // ... nor define a role broader than their own.
    let e = p
        .put_role(&user("bob"), Some(&org), &RoleInput { key: "god".into(), allow: vec!["*".into()], ..RoleInput::default() })
        .await
        .unwrap_err();
    assert_eq!(step(e), Step::Permission);
    // ... nor override-allow a denied permission (admin denies org:delete).
    let e = p.add_override(&user("bob"), Some(&org), &pk("bob"), "org:delete", Effect::Allow).await.unwrap_err();
    assert_eq!(step(e), Step::Permission);
    assert!(p.delete_org(&user("bob"), &org).await.is_err());
    // Role sets bundle roles.
    p.put_role_set(&user("alice"), Some(&org), "ops", "Ops", &["developer".into(), "analyst".into()])
        .await
        .unwrap();
    p.set_member(&user("alice"), &org, &pk("carol"), &[], &["ops".into()]).await.unwrap();
    assert!(p.has(&user("carol"), "analytics:export", Some(&org)));
    assert!(p.has(&user("carol"), "api_keys:create", Some(&org)));
}

#[derive(Debug, Default)]
struct Recorder {
    calls: Mutex<Vec<&'static str>>,
    deny_entitlement: bool,
    deny_quota: bool,
}

#[derive(Debug, Clone)]
struct Hook(Arc<Recorder>);

impl EntitlementHook for Hook {
    fn check(&self, _req: &HookRequest<'_>) -> HookOutcome {
        self.0.calls.lock().unwrap().push("entitlement");
        if self.0.deny_entitlement {
            HookOutcome::Deny("not in plan".into())
        } else {
            HookOutcome::Allow
        }
    }
}

impl QuotaHook for Hook {
    fn check(&self, req: &HookRequest<'_>) -> HookOutcome {
        self.0.calls.lock().unwrap().push("quota");
        assert!(matches!(req.subject, forgebase_data::SubjectRef::Org(_)), "billed to the active org");
        if self.0.deny_quota {
            HookOutcome::Deny("quota exhausted".into())
        } else {
            HookOutcome::Allow
        }
    }
}

fn hooks(env: &Env, deny_entitlement: bool, deny_quota: bool) -> Arc<Recorder> {
    let r = Arc::new(Recorder {
        deny_entitlement,
        deny_quota,
        ..Recorder::default()
    });
    env.policy.set_hooks(Hooks {
        entitlement: Arc::new(Hook(r.clone())),
        quota: Arc::new(Hook(r.clone())),
    });
    r
}

#[tokio::test]
async fn the_pipeline_runs_in_order_and_stops_at_the_first_failure() {
    let env = Env::new().await;
    let org = env.org("alice", "acme").await;
    env.member("alice", &org, "bob", &["viewer"]).await;
    let (project, _) = env.project("alice", &org, "app").await;
    let ctx = user("bob").with_org(&org);

    // 3 fails: the hooks never run.
    let r = hooks(&env, false, false);
    assert_eq!(step(env.policy.check(&ctx, &check("projects:create", &org)).unwrap_err()), Step::Permission);
    assert!(r.calls.lock().unwrap().is_empty());

    // 4 fails: 5 and 6 never run.
    let r = hooks(&env, true, false);
    assert_eq!(step(env.policy.check(&ctx, &check("projects:read", &org)).unwrap_err()), Step::Entitlement);
    assert_eq!(*r.calls.lock().unwrap(), ["entitlement"]);

    // 5 fails after 4 and before 6.
    let r = hooks(&env, false, false);
    env.policy
        .put_acl(&user("alice"), "project", &project, &pk("bob"), "projects:read", Effect::Deny)
        .await
        .unwrap();
    let c = Check {
        resource: Some(ResourceRef::new("project", &project)),
        ..check("projects:read", &org)
    };
    assert_eq!(step(env.policy.check(&ctx, &c).unwrap_err()), Step::Resource);
    assert_eq!(*r.calls.lock().unwrap(), ["entitlement"]);

    // 6 fails last.
    let r = hooks(&env, false, true);
    assert_eq!(step(env.policy.check(&ctx, &check("projects:read", &org)).unwrap_err()), Step::Quota);
    assert_eq!(*r.calls.lock().unwrap(), ["entitlement", "quota"]);

    // Everything passes.
    let r = hooks(&env, false, false);
    env.policy.check(&ctx, &check("projects:read", &org)).unwrap();
    assert_eq!(*r.calls.lock().unwrap(), ["entitlement", "quota"]);

    // 1 and 2 fail before 3.
    assert_eq!(
        step(env.policy.check(&AccessContext::anonymous(), &check("projects:read", &org)).unwrap_err()),
        Step::Authentication
    );
    assert_eq!(step(env.policy.check(&user("mallory"), &check("projects:read", &org)).unwrap_err()), Step::Tenant);

    // Every denial is audited with its step, in order.
    let steps = env.audit_steps(&pk("bob"), 4).await;
    assert_eq!(steps, ["permission", "entitlement", "resource", "quota"]);
}
