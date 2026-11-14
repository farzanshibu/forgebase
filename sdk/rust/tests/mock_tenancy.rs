//! Mock tests for access control, organizations and identity administration: request shapes and
//! response decoding against a scripted server. Hermetic: no ForgeBase binary needed.

mod common;

use std::collections::BTreeMap;

use common::mock::{Mock, err, no_content, ok, serve, status};
use forgebase_sdk::access::{
    AccessCheck, CollectionPolicy, Effect, NewAcl, NewOverride, ResourceCheck, RuleOp, RuleVerdict, rules,
};
use forgebase_sdk::identity::{
    AuthAuditQuery, ClientInput, NewApiKey, NewRealm, NewServiceAccount, ProviderPreset, RealmProviderConfig,
};
use forgebase_sdk::orgs::{MemberInput, NewOrg, NewRole, NewRoleSet, NewTeam};
use forgebase_sdk::{ErrorKind, SignInOptions, SignInResult};
use serde_json::json;

// ---- access ----

#[tokio::test]
async fn access_me_check_can_and_audit() {
    let m = Mock::start(vec![
        ok(json!({"principal": "user:users/u1", "realm_id": "default", "org_id": "o1", "super_admin": false,
            "suspended": false, "roles": ["editor"], "allow": ["records:*"], "deny": ["records:delete"], "teams": ["t1"]})),
        ok(json!({"allowed": false, "step": "permission", "reason": "denied by records:delete"})),
        ok(json!({"allowed": true})),
        ok(json!([{"at": 1, "permission": "records:delete", "step": "permission", "principal": "user:users/u1"}])),
    ])
    .await;
    let fb = m.client();
    let me = fb.access().me(Some("o1")).await.expect("me");
    assert_eq!(me.roles, ["editor"]);
    assert_eq!(me.deny, ["records:delete"]);
    assert_eq!(m.last().q("org").as_deref(), Some("o1"));
    assert_eq!(m.last().route(), "/api/v1/authz/me");

    let d = fb
        .access()
        .check(
            &AccessCheck::new("records:delete")
                .org("o1")
                .resource("collection", "posts"),
        )
        .await
        .expect("check");
    assert!(!d.allowed);
    assert_eq!(d.step.as_deref(), Some("permission"));
    let body = m.last().json();
    assert_eq!(body["permission"], "records:delete");
    assert_eq!(body["resource_type"], "collection");
    assert_eq!(body["resource_id"], "posts");
    assert!(body.get("principal").is_none(), "unset fields stay off the wire");

    assert!(fb.access().can("records:create").await.expect("can"));
    let audit = fb.access().audit(Some(5)).await.expect("audit");
    assert_eq!(audit[0].permission.as_deref(), Some("records:delete"));
    assert_eq!(m.last().q("limit").as_deref(), Some("5"));
}

#[tokio::test]
async fn access_acl_overrides_policies_and_resource_rules() {
    let acl = json!({"id": "a1", "resource_type": "record", "resource_id": "posts/1", "grantee": "team:t1",
        "permission": "records:update", "effect": "allow"});
    let m = Mock::start(vec![
        ok(json!([acl.clone()])),
        status(201, acl),
        no_content(),
        ok(json!([{"id": "o1", "org_id": null, "principal": "user:users/u1", "permission": "files:*", "effect": "deny"}])),
        status(201, json!({"id": "o2", "org_id": "org1", "principal": "user:users/u1", "permission": "files:*", "effect": "deny"})),
        no_content(),
        ok(json!({"collection": "posts", "list_rule": "", "view_rule": null, "create_rule": "@request.auth.id != \"\"",
            "update_rule": null, "delete_rule": null, "fields": {"secret": {"read": null, "write": null}}})),
        ok(json!({"collection": "posts", "list_rule": "", "view_rule": null, "create_rule": null, "update_rule": null, "delete_rule": null, "fields": {}})),
        no_content(),
        ok(json!({"kind": "kv", "fields": [{"name": "key", "type": "string"}], "operations": ["read"], "actions": ["read"], "items": [{"name": "*", "rules": {"read": ""}}]})),
        ok(json!({"kind": "kv", "name": "cache", "rules": {"read": "", "write": null}})),
        ok(json!({"kind": "kv", "name": "cache", "rules": {"read": ""}})),
        no_content(),
        ok(json!({"decision": "filter", "step": "rule"})),
    ])
    .await;
    let fb = m.client();
    let a = fb.access();

    let entries = a.acl().list("record", "posts/1").await.expect("acl list");
    assert_eq!(entries[0].effect, Effect::Allow);
    let c = m.last();
    assert_eq!(
        (
            c.route(),
            c.q("resource_type").as_deref(),
            c.q("resource_id").as_deref()
        ),
        ("/api/v1/acl", Some("record"), Some("posts/1"))
    );
    let put = a
        .acl()
        .put(&NewAcl::new(
            "record",
            "posts/1",
            "team:t1",
            "records:update",
            Effect::Allow,
        ))
        .await
        .expect("acl put");
    assert_eq!(put.grantee, "team:t1");
    assert_eq!(m.last().json()["effect"], "allow");
    a.acl().delete("a 1").await.expect("acl delete");
    assert_eq!(
        (m.last().method.as_str(), m.last().path.as_str()),
        ("DELETE", "/api/v1/acl/a%201")
    );

    let os = a.overrides().list(None).await.expect("overrides");
    assert!(os[0].org_id.is_none());
    assert!(m.last().q("org").is_none());
    a.overrides()
        .add(&NewOverride::new("user:users/u1", "files:*", Effect::Deny).org("org1"))
        .await
        .expect("add override");
    assert_eq!(m.last().json()["org_id"], "org1");
    a.overrides().delete("o2").await.expect("delete override");

    let p = a.policies().get("posts").await.expect("policy");
    assert_eq!(p.list_rule.as_deref(), Some(""), "an empty rule is public");
    assert!(p.view_rule.is_none(), "null locks");
    let new = CollectionPolicy {
        list_rule: Some(String::new()),
        ..CollectionPolicy::default()
    };
    a.policies().put("posts", &new).await.expect("put policy");
    let b = m.last().json();
    assert_eq!(m.last().method, "PUT");
    assert_eq!(b["list_rule"], "");
    assert!(b["view_rule"].is_null(), "a locked rule is sent as null");
    assert!(
        b.get("collection").is_none(),
        "the collection name is not part of the body"
    );
    a.policies().delete("posts").await.expect("delete policy");

    let cat = a.resource_rules().list("kv").await.expect("catalog");
    assert_eq!(cat.items[0].name, "*");
    assert_eq!(cat.fields[0].ty, "string");
    let set = a.resource_rules().get("kv", "cache").await.expect("rules");
    assert_eq!(set.rules["read"].as_deref(), Some(""));
    assert_eq!(set.rules["write"], None);
    a.resource_rules()
        .put("kv", "cache", &rules([("read", Some("")), ("write", None)]))
        .await
        .expect("put rules");
    assert_eq!(m.last().json(), json!({"rules": {"read": "", "write": null}}));
    a.resource_rules().delete("kv", "cache").await.expect("delete rules");
    let d = a
        .resource_rules()
        .check("kv", "cache", &ResourceCheck::new("read", RuleOp::List))
        .await
        .expect("check");
    assert_eq!(d.decision, RuleVerdict::Filter);
    assert_eq!(m.last().route(), "/api/v1/policies/resources/kv/cache/check");
    assert_eq!(m.last().json(), json!({"action": "read", "op": "list"}));
}

#[tokio::test]
async fn access_errors_are_typed() {
    let (_m, fb) = serve(err(403, "forbidden")).await;
    let e = fb.access().audit(None).await.expect_err("403");
    assert_eq!(e.kind(), Some(ErrorKind::Permission));
    assert!(e.is_forbidden());
}

// ---- orgs ----

#[tokio::test]
async fn orgs_crud_and_projects() {
    let org = json!({"id": "o1", "realm_id": "default", "slug": "acme", "name": "Acme"});
    let proj =
        json!({"id": "p1", "realm_id": "default", "org_id": "o1", "owner_user": null, "slug": "web", "name": "Web"});
    let m = Mock::start(vec![
        ok(json!([org.clone()])),
        status(201, org.clone()),
        ok(org.clone()),
        ok(json!({"id": "o1", "realm_id": "default", "slug": "acme", "name": "Acme Inc"})),
        no_content(),
        ok(json!([proj.clone()])),
        ok(json!([])),
        status(201, proj.clone()),
        ok(
            json!({"id": "p1", "realm_id": "default", "org_id": "o1", "owner_user": null, "slug": "web", "name": "Web",
            "environments": [{"id": "e1", "project_id": "p1", "name": "production"}]}),
        ),
        no_content(),
        status(201, json!({"id": "e2", "project_id": "p1", "name": "staging"})),
    ])
    .await;
    let orgs = m.client().orgs();
    assert_eq!(orgs.list().await.expect("list")[0].slug, "acme");
    let created = orgs.create(&NewOrg::new("acme", "Acme")).await.expect("create");
    assert_eq!(created.id, "o1");
    assert_eq!(m.last().json(), json!({"slug": "acme", "name": "Acme"}));
    orgs.get("o 1").await.expect("get");
    assert_eq!(m.last().path, "/api/v1/orgs/o%201");
    let r = orgs.rename("o1", "Acme Inc").await.expect("rename");
    assert_eq!((r.name.as_str(), m.last().method.as_str()), ("Acme Inc", "PATCH"));
    orgs.delete("o1").await.expect("delete");

    let p = orgs.projects();
    assert_eq!(
        p.list(Some("o1")).await.expect("projects")[0].org_id.as_deref(),
        Some("o1")
    );
    assert_eq!(m.last().path, "/api/v1/orgs/o1/projects");
    assert!(p.list(None).await.expect("personal").is_empty());
    assert_eq!(m.last().path, "/api/v1/projects");
    p.create(&NewOrg::new("web", "Web"), Some("o1"))
        .await
        .expect("create project");
    assert_eq!(m.last().path, "/api/v1/orgs/o1/projects");
    let one = p.get("p1").await.expect("project");
    assert_eq!(one.environments[0].name, "production");
    p.delete("p1").await.expect("delete project");
    let env = p.create_environment("p1", "staging").await.expect("env");
    assert_eq!(env.name, "staging");
    assert_eq!(m.last().path, "/api/v1/projects/p1/environments");
}

#[tokio::test]
async fn orgs_members_invitations_teams_roles() {
    let m = Mock::start(vec![
        ok(json!([{"id": "m1", "scope_id": "o1", "principal": "user:users/u1", "roles": ["admin"], "role_sets": [], "status": "active"}])),
        ok(json!({"org_id": "o1", "principal": "user:users/u2", "roles": ["viewer"], "role_sets": [], "status": "active"})),
        no_content(),
        no_content(),
        ok(json!([{"id": "i1", "org_id": "o1", "email": "a@b.c", "roles": ["viewer"], "expires_at": 9, "status": "pending"}])),
        status(201, json!({"id": "i2", "org_id": "o1", "email": "a@b.c", "roles": ["viewer"], "expires_at": 9, "token": "tok"})),
        ok(json!({"org_id": "o1", "principal": "user:users/u2", "roles": ["viewer"]})),
        ok(json!([{"id": "t1", "org_id": "o1", "parent_id": null, "key": "eng", "name": "Eng", "roles": [], "deny": [], "members": ["user:users/u1"]}])),
        status(201, json!({"id": "t2", "org_id": "o1", "parent_id": "t1", "key": "web", "name": "Web", "roles": ["editor"], "deny": []})),
        no_content(),
        no_content(),
        no_content(),
        ok(json!([{"id": "r1", "realm_id": "default", "org_id": null, "key": "editor", "name": "Editor", "description": "",
            "allow": ["records:*"], "deny": [], "system": true, "assignable": true}])),
        ok(json!({"id": "r2", "realm_id": "default", "org_id": "o1", "key": "triager", "name": "triager", "description": "",
            "allow": ["issues:read"], "deny": ["issues:delete"], "system": false, "assignable": true})),
        no_content(),
        ok(json!([{"id": "s1", "realm_id": "default", "org_id": null, "key": "staff", "name": "Staff", "roles": ["editor"]}])),
        ok(json!({"id": "s2", "realm_id": "default", "org_id": "o1", "key": "staff", "name": "Staff", "roles": ["editor"]})),
        ok(json!([{"principal": "user:users/u1", "roles": ["admin"], "role_sets": []}])),
        ok(json!({"principal": "user:users/u1", "roles": ["admin"], "role_sets": []})),
        ok(json!([{"principal": "user:users/u9", "reason": "abuse", "suspended_by": "admin", "created_at": 1, "until": null}])),
        no_content(),
        no_content(),
        ok(json!([{"id": "default", "name": "Default"}])),
        status(201, json!({"id": "eu", "name": "EU"})),
    ])
    .await;
    let orgs = m.client().orgs();

    let ms = orgs.members();
    assert_eq!(ms.list("o1").await.expect("members")[0].status, "active");
    ms.set("o1", &MemberInput::new("user:users/u2", &["viewer"]))
        .await
        .expect("set");
    assert_eq!(m.last().method, "PUT");
    assert_eq!(
        m.last().json(),
        json!({"principal": "user:users/u2", "roles": ["viewer"], "role_sets": []})
    );
    ms.remove("o1", "user:users/u2").await.expect("remove");
    assert_eq!(m.last().path, "/api/v1/orgs/o1/members/remove");
    ms.set_active("o1", "user:users/u2", false).await.expect("status");
    assert_eq!(m.last().json(), json!({"principal": "user:users/u2", "active": false}));

    let inv = orgs.invitations();
    assert_eq!(
        inv.list("o1").await.expect("invitations")[0].status.as_deref(),
        Some("pending")
    );
    let made = inv.create("o1", "a@b.c", &["viewer"]).await.expect("invite");
    assert_eq!((made.token.as_str(), made.invitation.email.as_str()), ("tok", "a@b.c"));
    assert_eq!(inv.accept("tok").await.expect("accept").org_id, "o1");
    assert_eq!(m.last().json(), json!({"token": "tok"}));

    let teams = orgs.teams();
    assert_eq!(teams.list("o1").await.expect("teams")[0].members, ["user:users/u1"]);
    let t = teams
        .create("o1", &NewTeam::new("web", "Web").parent("t1"))
        .await
        .expect("team");
    assert_eq!(t.parent_id.as_deref(), Some("t1"));
    assert_eq!(m.last().json()["parent_id"], "t1");
    teams.delete("t2").await.expect("delete team");
    teams.add_member("t1", "user:users/u1").await.expect("add member");
    assert_eq!(m.last().path, "/api/v1/teams/t1/members");
    teams.remove_member("t1", "user:users/u1").await.expect("remove member");
    assert_eq!(m.last().path, "/api/v1/teams/t1/members/remove");

    let roles = orgs.roles();
    assert!(roles.list(None).await.expect("roles")[0].system);
    assert_eq!(m.last().path, "/api/v1/roles");
    let role = roles
        .put(
            &NewRole::new("triager").allow("issues:read").deny("issues:delete"),
            Some("o1"),
        )
        .await
        .expect("put role");
    assert_eq!(role.deny, ["issues:delete"]);
    assert_eq!(
        (m.last().method.as_str(), m.last().route()),
        ("PUT", "/api/v1/orgs/o1/roles")
    );
    roles.delete("triager", Some("o1")).await.expect("delete role");
    assert_eq!(m.last().path, "/api/v1/orgs/o1/roles/triager");

    let sets = orgs.role_sets();
    assert_eq!(sets.list(None).await.expect("sets")[0].roles, ["editor"]);
    sets.put(
        &NewRoleSet {
            key: "staff".into(),
            name: "Staff".into(),
            roles: vec!["editor".into()],
        },
        Some("o1"),
    )
    .await
    .expect("put set");
    assert_eq!(m.last().path, "/api/v1/orgs/o1/role-sets");

    assert_eq!(orgs.realm_bindings().list().await.expect("bindings").len(), 1);
    orgs.realm_bindings()
        .put(&MemberInput::new("user:users/u1", &["admin"]))
        .await
        .expect("bind");
    assert_eq!(m.last().path, "/api/v1/realm-bindings");

    let sus = orgs.suspensions();
    assert_eq!(
        sus.list().await.expect("suspensions")[0].reason.as_deref(),
        Some("abuse")
    );
    sus.suspend("user:users/u9", Some("abuse"), Some(100))
        .await
        .expect("suspend");
    assert_eq!(
        m.last().json(),
        json!({"principal": "user:users/u9", "reason": "abuse", "until": 100})
    );
    sus.lift("user:users/u9").await.expect("lift");
    assert_eq!(m.last().path, "/api/v1/suspensions/lift");

    assert_eq!(orgs.realms().list().await.expect("realms")[0].id, "default");
    orgs.realms().create("eu", "EU").await.expect("create realm");
    assert_eq!(m.last().json(), json!({"id": "eu", "name": "EU"}));
}

// ---- identity ----

#[tokio::test]
async fn identity_api_keys_tokens_service_accounts() {
    let key = json!({"id": "k1", "kind": "api_key", "name": "ci", "owner_type": "service", "owner_id": "sa1",
        "scopes": ["records:read"], "created_at": "t", "revoked": false, "tenant": "env1"});
    let sa = json!({"id": "sa1", "name": "ci", "description": "", "role": "service", "created_by": "a", "created_at": "t", "disabled": false, "realm": "default"});
    let m = Mock::start(vec![
        ok(json!({"items": [key.clone()]})),
        no_content(),
        ok(json!({"items": [key.clone()]})),
        no_content(),
        ok(json!({"items": [sa.clone()]})),
        status(201, sa.clone()),
        ok(sa.clone()),
        no_content(),
        ok(json!({"items": [key.clone()]})),
        status(
            201,
            json!({"id": "k2", "key": "fb_secret", "name": "ci", "scopes": ["records:read"], "created_at": "t"}),
        ),
        ok(json!({"items": [{"id": "ad1", "email": "a@b.c", "created_at": "t", "last_login_at": null}]})),
        status(201, json!({"id": "ad2", "email": "x@b.c", "created_at": "t"})),
    ])
    .await;
    let id = m.client().identity();
    assert!(id.api_keys().list().await.expect("keys")[0].tenant.as_deref() == Some("env1"));
    id.api_keys().revoke("k1").await.expect("revoke");
    assert_eq!(
        (m.last().method.as_str(), m.last().path.as_str()),
        ("DELETE", "/api/auth/api-keys/k1")
    );
    assert_eq!(id.tokens().list().await.expect("tokens").len(), 1);
    id.tokens().revoke("k1").await.expect("revoke token");
    assert_eq!(m.last().path, "/api/auth/tokens/k1");

    let sas = id.service_accounts();
    assert_eq!(sas.list().await.expect("list")[0].role, "service");
    sas.create(&NewServiceAccount::new("ci")).await.expect("create");
    assert_eq!(m.last().json(), json!({"name": "ci"}));
    assert_eq!(sas.get("sa1").await.expect("get").realm, "default");
    sas.delete("sa1").await.expect("delete");
    assert_eq!(sas.keys().list("sa1").await.expect("keys").len(), 1);
    assert_eq!(m.last().path, "/api/auth/service-accounts/sa1/keys");
    let created = sas
        .keys()
        .create(
            "sa1",
            &NewApiKey::new("ci", &["records:read"])
                .expires_in_days(30)
                .tenant("env1"),
        )
        .await
        .expect("key");
    assert_eq!(created.key, "fb_secret");
    assert_eq!(
        m.last().json(),
        json!({"name": "ci", "scopes": ["records:read"], "expires_in_days": 30, "tenant": "env1"})
    );

    assert_eq!(id.admins().list().await.expect("admins")[0].email, "a@b.c");
    id.admins().create("x@b.c", "pw").await.expect("admin");
    assert_eq!(m.last().json(), json!({"email": "x@b.c", "password": "pw"}));
}

fn client_json() -> serde_json::Value {
    json!({"client_id": "c1", "realm": "default", "name": "Web", "type": "confidential", "redirect_uris": ["https://a/cb"],
        "post_logout_redirect_uris": [], "grant_types": ["authorization_code"], "scopes": ["openid"], "skip_consent": false,
        "disabled": false, "created_at": "t", "updated_at": "t"})
}

#[tokio::test]
async fn identity_oauth_clients_and_oidc() {
    let m = Mock::start(vec![
        ok(json!({"items": [client_json()]})),
        status(201, json!({"client": client_json(), "client_secret": "s3cret"})),
        ok(client_json()),
        ok(client_json()),
        no_content(),
        ok(json!({"client": client_json(), "client_secret": "new"})),
        ok(json!({"issuer": "x"})),
        ok(json!({"keys": []})),
        ok(json!({"items": [{"id": "co1", "client_id": "c1", "client_name": "Web", "realm": "default", "scopes": ["openid"], "created_at": "t", "updated_at": "t"}]})),
        no_content(),
    ])
    .await;
    let id = m.client().identity();
    let c = id.clients();
    assert_eq!(c.list(Some("eu")).await.expect("list")[0].client_type, "confidential");
    assert_eq!(m.last().q("realm").as_deref(), Some("eu"));
    let made = c
        .create(&ClientInput::new("Web").redirect_uri("https://a/cb").public())
        .await
        .expect("create");
    assert_eq!(made.client_secret.as_deref(), Some("s3cret"));
    assert_eq!(
        m.last().json(),
        json!({"name": "Web", "type": "public", "redirect_uris": ["https://a/cb"]})
    );
    c.get("c1").await.expect("get");
    c.update(
        "c1",
        &ClientInput {
            disabled: Some(true),
            ..ClientInput::default()
        },
    )
    .await
    .expect("update");
    assert_eq!(
        (m.last().method.as_str(), m.last().json()),
        ("PATCH", json!({"disabled": true}))
    );
    c.delete("c1").await.expect("delete");
    let rot = c.rotate_secret("c1", Some(3600)).await.expect("rotate");
    assert_eq!(rot.client_secret.as_deref(), Some("new"));
    assert_eq!(m.last().json(), json!({"grace_seconds": 3600}));

    let oidc = id.oidc();
    assert!(oidc.issuer("default").ends_with("/api/auth/oidc/default"));
    assert!(
        oidc.discovery_url("default")
            .ends_with("/.well-known/openid-configuration")
    );
    oidc.discovery("default").await.expect("discovery");
    assert!(
        m.last().header("authorization").is_none() && m.last().header("x-api-key").is_none(),
        "public: no credentials"
    );
    oidc.jwks("default").await.expect("jwks");
    assert_eq!(id.consents().list().await.expect("consents")[0].client_name, "Web");
    id.consents().revoke("co1").await.expect("revoke");
}

#[tokio::test]
async fn identity_realms_providers_presets_and_sms() {
    let realm = json!({"realm": "eu", "collection": "eu_users", "created_at": "t", "updated_at": "t",
        "config": {"signup": true, "methods": {"password": true, "magic_link": false, "email_otp": false, "oauth": true, "passkey": false, "phone": false, "ldap": false}, "phone": {"signup": false}}});
    let m = Mock::start(vec![
        ok(json!({"realm": "eu", "collection": "eu_users", "signup": true, "methods": {"password": true, "magic_link": false, "email_otp": false, "oauth": true, "passkey": false, "phone": false, "ldap": false}, "phone": {"signup": false}, "oauth_providers": [{"id": "gh"}]})),
        ok(json!({"items": [realm.clone()]})),
        ok(realm.clone()),
        status(201, realm.clone()),
        ok(realm.clone()),
        ok(realm.clone()),
        ok(json!({"realm": "eu", "current": "k1", "items": [{"kid": "k1", "alg": "RS256", "status": "current", "created_at": "t", "verifying": true}]})),
        ok(json!({"items": [{"id": "gh", "realm": "eu", "kind": "github", "display_name": "GitHub", "client_id": "abc", "client_secret": "********"}]})),
        ok(json!({"id": "gh", "realm": "eu", "kind": "github"})),
        ok(json!({"id": "bb", "realm": "eu", "kind": "bitbucket", "client_id": "id"})),
        no_content(),
        ok(json!({"realm": "eu", "configured": true, "provider": "twilio", "account_sid": "AC1", "auth_token": "********"})),
        ok(json!({"realm": "eu", "configured": true, "provider": "dev"})),
        no_content(),
        ok(json!({"items": [], "limit": 50, "offset": 0})),
        no_content(),
    ])
    .await;
    let realms = m.client().identity().realms();
    let info = realms.info().await.expect("info");
    assert!(info.signup && info.methods.oauth);
    assert!(m.last().header("x-api-key").is_none(), "realm info is public");
    assert_eq!(realms.list().await.expect("list")[0].collection, "eu_users");
    assert!(realms.get("eu").await.expect("get").config.methods.password);
    realms.create(&NewRealm::new("eu")).await.expect("create");
    assert_eq!(m.last().json(), json!({"id": "eu"}));
    let mut cfg = realms.get("eu").await.expect("get").config;
    cfg.methods.magic_link = true;
    realms.update("eu", None, &cfg).await.expect("update");
    assert_eq!(m.last().method, "PUT");
    assert_eq!(m.last().json()["config"]["methods"]["magic_link"], true);
    let keys = realms.keys("eu").await.expect("keys");
    assert_eq!(keys.current.as_deref(), Some("k1"));

    let pr = realms.providers();
    let list = pr.list("eu").await.expect("providers");
    assert_eq!(list[0].kind, "github");
    assert_eq!(
        list[0].extra["client_secret"], "********",
        "extra fields stay reachable"
    );
    pr.get("eu", "gh").await.expect("get provider");
    let cfg = RealmProviderConfig::preset(ProviderPreset::Bitbucket, "id", "secret").set("scopes", json!(["account"]));
    let p = pr.put("eu", "bb", &cfg).await.expect("put provider");
    assert_eq!(p.kind, "bitbucket");
    assert_eq!(
        (m.last().method.as_str(), m.last().json()),
        (
            "PUT",
            json!({"kind": "bitbucket", "client_id": "id", "client_secret": "secret", "scopes": ["account"]})
        )
    );
    pr.delete("eu", "bb").await.expect("delete provider");
    let ms = RealmProviderConfig::preset(ProviderPreset::Microsoft, "id", "secret")
        .tenant("organizations")
        .allowed_tenants(&["tid-1"])
        .authorize_param("prompt", "select_account");
    assert_eq!(
        serde_json::to_value(&ms).expect("json"),
        json!({"kind": "microsoft", "client_id": "id", "client_secret": "secret", "tenant": "organizations",
            "allowed_tenants": ["tid-1"], "authorize_params": {"prompt": "select_account"}})
    );
    for (preset, name) in [
        (ProviderPreset::Google, "google"),
        (ProviderPreset::Github, "github"),
        (ProviderPreset::Apple, "apple"),
        (ProviderPreset::Facebook, "facebook"),
        (ProviderPreset::Wechat, "wechat"),
        (ProviderPreset::Discord, "discord"),
        (ProviderPreset::Bitbucket, "bitbucket"),
        (ProviderPreset::Oidc, "oidc"),
        (ProviderPreset::Microsoft, "microsoft"),
        (ProviderPreset::Gitlab, "gitlab"),
        (ProviderPreset::Slack, "slack"),
        (ProviderPreset::Linkedin, "linkedin"),
        (ProviderPreset::Twitch, "twitch"),
        (ProviderPreset::Spotify, "spotify"),
        (ProviderPreset::Keycloak, "keycloak"),
    ] {
        assert_eq!(preset.as_str(), name);
    }

    let sms = realms.sms();
    let g = sms.get("eu").await.expect("sms");
    assert_eq!((g.configured, g.provider.as_deref()), (Some(true), Some("twilio")));
    sms.put("eu", &json!({"provider": "dev"})).await.expect("put sms");
    assert_eq!(m.last().json(), json!({"provider": "dev"}));
    sms.delete("eu").await.expect("delete sms");
    assert!(
        realms
            .consents("eu", Some("c1"), Some(10), None)
            .await
            .expect("consents")
            .items
            .is_empty()
    );
    assert_eq!(m.last().q("client_id").as_deref(), Some("c1"));
    realms.revoke_consent("eu", "co1").await.expect("revoke");
}

#[tokio::test]
async fn identity_users_impersonation_audit_keys_and_sign_in_flows() {
    let st = json!({"status": "suspended", "reason": "abuse", "until": null, "changed_by": "a", "changed_at": "t"});
    let session = json!({"token_type": "Bearer", "access_token": "at", "expires_in": 3600, "refresh_token": "rt",
        "session_id": "s1", "session_expires_at": "2099-01-01T00:00:00Z", "subject": {"kind": "user"}, "user": {"id": "u1"}});
    let m = Mock::start(vec![
        ok(json!({"id": "u1"})),
        ok(st.clone()),
        ok(st.clone()),
        ok(json!({"status": "active"})),
        ok(json!({"status": "deactivated"})),
        ok(json!({"status": "active"})),
        ok(json!({"ok": true})),
        no_content(),
        ok(json!({"user": {}})),
        ok(json!({"items": [{"id": "s1", "auth_method": "password", "mfa": false, "cookie": false, "created_at": "t", "last_seen_at": "t", "expires_at": "t", "current": false}]})),
        ok(json!({"revoked": 3})),
        ok(json!({"items": [{"ip": "1.2.3.4"}]})),
        ok(json!({"token_type": "Bearer", "access_token": "imp", "expires_in": 900, "expires_at": "t", "session_id": "s9", "impersonator": "a", "subject": {}, "reason": "support"})),
        ok(json!({"items": [{"session_id": "s9", "admin_id": "a", "target": "user:users/u1", "reason": "support", "created_at": "t", "expires_at": "t", "active": true}]})),
        no_content(),
        ok(json!({"items": [{"id": 5, "at": "t", "event": "login.failed", "outcome": "failure", "actor": "x", "detail": {}}]})),
        ok(json!({"items": []})),
        ok(json!({"active_kid": "k2"})),
        ok(json!({"realm": "eu", "kid": "k3"})),
        ok(session.clone()),
        ok(json!({"phone": "+15550100", "verified_at": null})),
        ok(json!({"mfa_required": true, "mfa_token": "mt", "expires_in": 300})),
        ok(session),
    ])
    .await;
    let id = m.client().identity();
    let u = id.users();
    u.get("users", "u 1").await.expect("get");
    assert_eq!(m.last().path, "/api/auth/users/users/u%201");
    assert_eq!(u.status("users", "u1").await.expect("status").status, "suspended");
    u.suspend("users", "u1", Some("abuse"), Some(99))
        .await
        .expect("suspend");
    assert_eq!(m.last().json(), json!({"reason": "abuse", "until": 99}));
    assert_eq!(u.unsuspend("users", "u1").await.expect("unsuspend").status, "active");
    assert_eq!(
        u.deactivate("users", "u1", None).await.expect("deactivate").status,
        "deactivated"
    );
    u.reactivate("users", "u1").await.expect("reactivate");
    u.anonymise("users", "u1").await.expect("anonymise");
    u.delete("users", "u1").await.expect("delete");
    assert_eq!(m.last().path, "/api/auth/users/users/u1/delete");
    u.export("users", "u1").await.expect("export");
    assert_eq!(u.sessions("users", "u1").await.expect("sessions").len(), 1);
    assert_eq!(u.revoke_sessions("users", "u1").await.expect("revoke"), 3);
    assert_eq!(u.login_history("users", "u1", Some(5)).await.expect("history").len(), 1);

    let grant = id
        .impersonation()
        .start("user:users/u1", "support", Some(5))
        .await
        .expect("impersonate");
    assert_eq!(grant.access_token, "imp");
    assert_eq!(
        m.last().json(),
        json!({"principal": "user:users/u1", "reason": "support", "ttl_minutes": 5})
    );
    assert!(id.impersonation().list().await.expect("list")[0].active);
    id.impersonation().end("s9").await.expect("end");
    assert_eq!(m.last().path, "/api/auth/impersonations/s9/end");

    let audit = id
        .audit(&AuthAuditQuery {
            event: Some("login.failed".into()),
            before: Some(10),
            limit: Some(5),
            ..AuthAuditQuery::default()
        })
        .await
        .expect("audit");
    assert_eq!(audit[0].event, "login.failed");
    let c = m.last();
    assert_eq!(
        (
            c.q("event").as_deref(),
            c.q("before").as_deref(),
            c.q("limit").as_deref()
        ),
        (Some("login.failed"), Some("10"), Some("5"))
    );
    assert!(id.login_history().await.expect("login history").is_empty());
    assert_eq!(id.rotate_signing_key().await.expect("rotate"), "k2");
    assert_eq!(
        id.rotate_oidc_key(Some("eu")).await.expect("rotate oidc"),
        ("eu".into(), "k3".into())
    );

    // phone sign-in stores the session like auth().sign_in
    let r = id
        .phone()
        .sign_in("+15550100", "123456", &SignInOptions::default())
        .await
        .expect("phone");
    assert!(matches!(r, SignInResult::Session(_)));
    assert_eq!(m.last().route(), "/api/auth/phone/verify");
    assert!(m.last().header("x-api-key").is_none(), "sign-in sends no credentials");
    assert_eq!(
        id.phone().get().await.expect("phone").phone.as_deref(),
        Some("+15550100")
    );
    let r = id
        .ldap()
        .sign_in("jo", "pw", &SignInOptions::default())
        .await
        .expect("ldap");
    assert!(matches!(r, SignInResult::MfaRequired(c) if c.mfa_token == "mt"));
    let r = id
        .webauthn()
        .verify_mfa("mt", "ceremony", &json!({"id": "cred"}))
        .await
        .expect("webauthn mfa");
    assert!(matches!(r, SignInResult::Session(_)));
}

#[tokio::test]
async fn identity_account_self_service() {
    let m = Mock::start(vec![
        ok(json!({"status": "deactivated"})),
        ok(json!({"deleted": true})),
        ok(json!({"user": {}})),
    ])
    .await;
    let acc = m.client().identity().account();
    acc.deactivate(Some("pw")).await.expect("deactivate");
    assert_eq!(m.last().json(), json!({"current_password": "pw"}));
    acc.delete(None, true).await.expect("delete");
    assert_eq!(m.last().json(), json!({"anonymise": true}));
    acc.export().await.expect("export");
    assert_eq!(m.last().path, "/api/auth/account/export");
}

#[test]
fn rules_helper_builds_the_wire_map() {
    let r: BTreeMap<String, Option<String>> = rules([("read", Some("")), ("write", None)]);
    assert_eq!(
        serde_json::to_value(&r).expect("json"),
        json!({"read": "", "write": null})
    );
}
