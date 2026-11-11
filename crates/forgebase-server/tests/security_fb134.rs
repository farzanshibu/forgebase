//! FB-134 (rls-coverage G7, G9, G10, G14, G16 framework): resource-scoped
//! engine permissions and engine resource rules, over HTTP against the real
//! server. Each test failed before FB-134: resource keys were refused in
//! roles or granted nothing, and the `/api/v1/policies/resources` routes did
//! not exist.
//! Binds `127.0.0.1:0`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod security_common;

use security_common::Env;
use serde_json::{Value as Json, json};

/// A realm role bound to `principal` (as an administrator).
async fn realm_role(env: &Env, admin: &str, principal: &str, key: &str, allow: &[&str], deny: &[&str]) {
    let r = env
        .as_(
            admin,
            "PUT",
            "/api/v1/roles",
            &[],
            Some(json!({"key": key, "allow": allow, "deny": deny})),
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

async fn allowed(env: &Env, token: &str, permission: &str) -> Json {
    let r = env
        .as_(
            token,
            "POST",
            "/api/v1/authz/check",
            &[],
            Some(json!({"permission": permission})),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    r.json()
}

async fn decision(env: &Env, token: Option<&str>, kind: &str, name: &str, action: &str, op: &str) -> Json {
    let path = format!("/api/v1/policies/resources/{kind}/{name}/check");
    let body = Some(json!({"action": action, "op": op}));
    let r = match token {
        Some(t) => env.as_(t, "POST", &path, &[], body).await,
        None => env.req("POST", &path, &[], body).await,
    };
    assert_eq!(r.status, 200, "{path}: {}", r.text());
    r.json()
}

#[tokio::test]
async fn resource_keys_narrow_engine_permissions_and_the_plain_key_keeps_granting_all() {
    let env = Env::new().await;
    let admin = env.admin("fb134-admin@example.com").await;
    let (wide, wide_pk) = env.user("wide@example.com").await;
    let (narrow, narrow_pk) = env.user("narrow@example.com").await;
    // Existing grants keep their meaning: engine:kv_read is every namespace,
    // minus an explicit deny.
    realm_role(
        &env,
        &admin,
        &wide_pk,
        "kv_all",
        &["engine:kv_read"],
        &["engine:kv_read:secret*"],
    )
    .await;
    // Resource patterns (with `.`, `:` and `*` in the name) are accepted.
    realm_role(
        &env,
        &admin,
        &narrow_pk,
        "kv_cache",
        &["engine:kv_read:cache.*", "engine:queue_consume:orders:billing"],
        &[],
    )
    .await;
    assert_eq!(
        allowed(&env, &wide, "engine:kv_read:cache.users").await["allowed"],
        true
    );
    assert_eq!(allowed(&env, &wide, "engine:kv_read").await["allowed"], true);
    let denied = allowed(&env, &wide, "engine:kv_read:secrets").await;
    assert_eq!(denied["allowed"], false, "{denied}");
    assert_eq!(denied["step"], "permission");

    assert_eq!(
        allowed(&env, &narrow, "engine:kv_read:cache.users").await["allowed"],
        true
    );
    assert_eq!(
        allowed(&env, &narrow, "engine:kv_read:sessions").await["allowed"],
        false
    );
    assert_eq!(
        allowed(&env, &narrow, "engine:kv_read").await["allowed"],
        false,
        "a resource grant is not a grant of every namespace"
    );
    assert_eq!(
        allowed(&env, &narrow, "engine:queue_consume:orders:billing").await["allowed"],
        true
    );
    // The engine decision says the same.
    for (token, ns, want) in [
        (&wide, "anything", "allow"),
        (&wide, "secrets", "deny"),
        (&narrow, "cache.users", "allow"),
        (&narrow, "sessions", "deny"),
    ] {
        let d = decision(&env, Some(token), "kv_namespace", ns, "kv_read", "view").await;
        assert_eq!(d["decision"], want, "{ns}: {d}");
    }
    // Not a resource action: no resource segment, no base-key widening.
    let r = env
        .as_(
            &admin,
            "PUT",
            "/api/v1/roles",
            &[],
            Some(json!({"key": "bad", "allow": ["engine:read_jobs:a.b"]})),
        )
        .await;
    assert_eq!(r.status, 400, "{}", r.text());
    env.shutdown().await;
}

#[tokio::test]
async fn resource_rules_are_for_administrators_and_validated_against_item_fields() {
    let env = Env::new().await;
    let admin = env.admin("fb134-rules@example.com").await;
    let (user, _) = env.user("rules-user@example.com").await;
    let path = "/api/v1/policies/resources/bucket/avatars";
    let body =
        json!({"rules": {"read": "created_by = @request.auth.id", "create": "@request.auth.id != ''", "delete": null}});
    // Callers who are not administrators cannot read or write rules.
    for (m, b) in [("PUT", Some(body.clone())), ("GET", None), ("DELETE", None)] {
        let r = env.as_(&user, m, path, &[], b.clone()).await;
        assert_eq!(r.status, 403, "{m}: {}", r.text());
        let r = env.req(m, path, &[], b).await;
        assert!(matches!(r.status, 401 | 403), "{m} anonymous: {}", r.status);
    }
    let r = env
        .as_(&user, "GET", "/api/v1/policies/resources/bucket", &[], None)
        .await;
    assert_eq!(r.status, 403);
    // Validation: kind, action, fields.
    for (p, b, code) in [
        (
            "/api/v1/policies/resources/table/x",
            json!({"rules": {"read": ""}}),
            "invalid_resource_kind",
        ),
        (path, json!({"rules": {"admin": ""}}), "invalid_action"),
        (path, json!({"rules": {"read": "no_such_field = 'x'"}}), "invalid_rule"),
        (path, json!({"rules": {}}), "invalid_rule"),
    ] {
        let r = env.as_(&admin, "PUT", p, &[], Some(b)).await;
        assert_eq!(r.status, 400, "{p}: {}", r.text());
        assert_eq!(r.code(), code, "{}", r.text());
    }
    let r = env.as_(&admin, "PUT", path, &[], Some(body)).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["rules"]["delete"], Json::Null);
    let r = env
        .as_(&admin, "GET", "/api/v1/policies/resources/bucket", &[], None)
        .await;
    assert_eq!(r.status, 200);
    let listed = r.json();
    assert!(
        listed["fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["name"] == "created_by"),
        "{listed}"
    );
    assert_eq!(listed["items"][0]["name"], "avatars");

    // Without a storage permission, only the rule grants: a filter for reads,
    // a decision for creates, nothing for deletes and admin actions.
    let check = |action: &'static str, op: &'static str| decision(&env, Some(&user), "bucket", "avatars", action, op);
    assert_eq!(check("storage_read", "list").await["decision"], "filter");
    assert_eq!(check("storage_write", "create").await["decision"], "allow");
    assert_eq!(check("storage_write", "delete").await["decision"], "deny");
    assert_eq!(check("storage_write", "update").await["decision"], "deny");
    assert_eq!(check("storage_admin", "update").await["decision"], "deny");
    let anon = decision(&env, None, "bucket", "avatars", "storage_write", "create").await;
    assert_eq!(anon["decision"], "deny", "{anon}");
    // Other buckets have no rule: deny by default.
    let other = decision(&env, Some(&user), "bucket", "logs", "storage_read", "list").await;
    assert_eq!(other["decision"], "deny");
    // Removing the rules locks the bucket again.
    let r = env.as_(&admin, "DELETE", path, &[], None).await;
    assert!(r.status < 300, "{}", r.text());
    assert_eq!(check("storage_write", "create").await["decision"], "deny");
    env.shutdown().await;
}

#[tokio::test]
async fn the_new_routes_are_in_the_openapi_document() {
    let env = Env::new().await;
    let doc = env.req("GET", "/api/openapi.json", &[], None).await.json();
    for p in [
        "/api/v1/policies/resources/{kind}",
        "/api/v1/policies/resources/{kind}/{name}",
        "/api/v1/policies/resources/{kind}/{name}/check",
    ] {
        assert!(doc["paths"][p].is_object(), "missing {p}");
    }
    env.shutdown().await;
}

/// The server wraps the policy authorizer (jobs' channel authorizer around
/// the shared data service's): the wrapper must pass resource decisions and
/// schema registrations through, or engines would silently get the
/// pre-FB-134 answer (`engine:<action>` only, no resource keys, no rules).
#[test]
fn the_wrapping_authorizer_forwards_resource_decisions() {
    use std::sync::{Arc, Mutex};

    use forgebase_data::schema::Collection;
    use forgebase_data::{
        AccessContext, Action, Authorizer, Decision, Predicate, ResourceKind, ResourceOp, ResourceRef, ResourceSchema,
    };

    #[derive(Debug, Default)]
    struct Inner(Mutex<Vec<ResourceKind>>);
    impl Authorizer for Inner {
        fn authorize(&self, _: &AccessContext, _: Action, _: Option<&Collection>) -> Decision {
            Decision::Deny
        }
        fn authorize_resource(&self, _: &AccessContext, _: Action, _: &ResourceRef) -> Decision {
            Decision::Filter(Predicate::parse("key = 'a'", Default::default()).unwrap())
        }
        fn register_resource_schema(&self, schema: ResourceSchema) {
            self.0.lock().unwrap().push(schema.kind);
        }
    }
    let inner = Arc::new(Inner::default());
    let wrapper = forgebase_jobs::JobChannelAuthorizer::new(inner.clone());
    let r = ResourceRef::new(ResourceKind::KvNamespace, "cache", ResourceOp::View);
    assert!(matches!(
        wrapper.authorize_resource(&AccessContext::user("users", "u1"), Action::KvRead, &r),
        Decision::Filter(_)
    ));
    wrapper.register_resource_schema(ResourceKind::Bucket.default_schema());
    assert_eq!(*inner.0.lock().unwrap(), [ResourceKind::Bucket]);
}
