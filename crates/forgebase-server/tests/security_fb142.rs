//! FB-142 (rls-coverage G3, G4, G5): job tenancy, reserved job kinds and the
//! contexts functions run with from jobs and hooks, against the real server.
//! Each test failed before FB-142: any `read_jobs` holder saw every tenant's
//! jobs and could watch their channels, any `enqueue_job` holder could queue
//! system kinds and invoke any function through `function.invoke` (which ran
//! as the function, not the enqueuer), after-hooks had no tenant, a
//! privileged writer lent its rights to before-hooks, and `ctx.auth.verify`
//! dropped a key's tenant binding. Binds `127.0.0.1:0`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod security_common;

use std::time::Duration;

use axum::http::HeaderMap;
use forgebase_data::AccessContext;
use forgebase_functions::http::HttpInvocation;
use forgebase_functions::{DeployRequest, HookEvent, HookSpec, InvokeAccess};
use forgebase_jobs::ClaimRequest;
use security_common::Env;
use serde_json::{Value as Json, json};

/// A user's organization with one project; returns (org id, production tenant).
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

/// A realm role `key` with `allow`, bound to `principal` (as an administrator).
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

/// Adds `principal` to `org` with the `member` role (as the owner).
async fn member(env: &Env, owner: &str, org: &str, principal: &str) {
    let r = env
        .as_(
            owner,
            "PUT",
            &format!("/api/v1/orgs/{org}/members"),
            &[("X-ForgeBase-Org", org)],
            Some(json!({"principal": principal, "roles": ["member"]})),
        )
        .await;
    assert!(r.status < 300, "member: {}", r.text());
}

fn tenant(t: &str) -> [(&str, &str); 1] {
    [("X-ForgeBase-Tenant", t)]
}

/// `POST /api/v1/jobs` as `token` in `tenant`.
async fn enqueue(env: &Env, token: &str, t: Option<&str>, body: Json) -> security_common::Resp {
    let h: Vec<(&str, &str)> = t.map(|t| ("X-ForgeBase-Tenant", t)).into_iter().collect();
    env.as_(token, "POST", "/api/v1/jobs", &h, Some(body)).await
}

/// Ids of the jobs `token` lists in `tenant`.
async fn listed(env: &Env, token: &str, t: Option<&str>) -> Vec<String> {
    let h: Vec<(&str, &str)> = t.map(|t| ("X-ForgeBase-Tenant", t)).into_iter().collect();
    let r = env.as_(token, "GET", "/api/v1/jobs?limit=200", &h, None).await;
    assert_eq!(r.status, 200, "list: {}", r.text());
    r.json()["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|j| j["id"].as_str().unwrap().to_owned())
        .collect()
}

/// The job once it reached a terminal state (as `token`).
async fn finished(env: &Env, token: &str, t: Option<&str>, id: &str) -> Json {
    let h: Vec<(&str, &str)> = t.map(|t| ("X-ForgeBase-Tenant", t)).into_iter().collect();
    for _ in 0..200 {
        let r = env.as_(token, "GET", &format!("/api/v1/jobs/{id}"), &h, None).await;
        assert_eq!(r.status, 200, "job: {}", r.text());
        let j = r.json();
        if matches!(j["status"].as_str(), Some("succeeded" | "dead" | "cancelled")) {
            return j;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("job {id} did not finish");
}

/// The principal id of `user:users/<id>`.
fn id_of(pk: &str) -> &str {
    pk.rsplit('/').next().unwrap()
}

async fn deploy(env: &Env, name: &str, source: &str, access: InvokeAccess, hooks: Vec<HookSpec>) {
    let caps = if hooks.is_empty() {
        vec![]
    } else {
        vec!["db.read".to_owned()]
    };
    env.server
        .functions()
        .expect("functions enabled")
        .deploy(
            &AccessContext::admin("root"),
            DeployRequest {
                name: name.into(),
                source: Some(source.into()),
                capabilities: caps,
                access: Some(access),
                hooks,
                ..DeployRequest::default()
            },
        )
        .await
        .unwrap();
}

const WHOAMI: &str = r#"import { serve } from "forgebase";
serve(async (req: Request, ctx) => {
  console.log("auth " + JSON.stringify({ type: ctx.auth?.type ?? null, id: ctx.auth?.id ?? null, tenant: ctx.auth?.tenant ?? null }));
  return { auth: ctx.auth, trigger: ctx.trigger };
});
"#;

#[tokio::test]
async fn g3_jobs_reads_and_channels_follow_tenant_and_creator() {
    let env = Env::new().await;
    let admin = env.admin("root@fb142.test").await;
    let (alice, alice_pk) = env.user("alice@fb142.test").await;
    let (bob, bob_pk) = env.user("bob@fb142.test").await;
    let (carol, carol_pk) = env.user("carol@fb142.test").await;
    let (org_a, ta) = org(&env, &alice, "acme").await;
    let (_org_b, tb) = org(&env, &bob, "bobco").await;
    member(&env, &alice, &org_a, &carol_pk).await;
    realm_role(
        &env,
        &admin,
        &carol_pk,
        "fb142_jobs",
        &[
            "engine:read_jobs",
            "engine:enqueue_job",
            "engine:manage_jobs",
            "engine:channel_subscribe",
        ],
    )
    .await;

    let r = enqueue(
        &env,
        &alice,
        Some(&ta),
        json!({"kind": "report", "payload": {"secret": "alpha"}}),
    )
    .await;
    assert_eq!(r.status, 201, "alice enqueues: {}", r.text());
    assert_eq!(r.json()["tenant"], json!(ta), "the job records its tenant");
    let a_job = r.json()["id"].as_str().unwrap().to_owned();
    let r = enqueue(
        &env,
        &bob,
        Some(&tb),
        json!({"kind": "report", "payload": {"secret": "beta"}}),
    )
    .await;
    assert_eq!(r.status, 201, "bob enqueues: {}", r.text());
    let b_job = r.json()["id"].as_str().unwrap().to_owned();

    // Another tenant: not listed, not readable, not manageable.
    let seen = listed(&env, &bob, Some(&tb)).await;
    assert!(
        seen.contains(&b_job) && !seen.contains(&a_job),
        "bob lists only his tenant: {seen:?}"
    );
    let r = env.as_(&bob, "GET", "/api/v1/jobs", &[], None).await;
    assert!(
        r.status == 403 || !r.text().contains(&a_job),
        "no tenant: no tenant jobs: {}",
        r.text()
    );
    let path = format!("/api/v1/jobs/{a_job}");
    for (method, p) in [
        ("GET", path.clone()),
        ("GET", format!("{path}/events")),
        ("GET", format!("{path}/attempts")),
        ("POST", format!("{path}/cancel")),
        ("DELETE", path.clone()),
    ] {
        let r = env.as_(&bob, method, &p, &tenant(&tb), None).await;
        assert_eq!(r.status, 404, "{method} {p} across tenants: {}", r.text());
    }
    let r = env.as_(&bob, "GET", "/api/v1/jobs/stats", &tenant(&tb), None).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(
        r.json()["totals"]["queued"],
        1,
        "stats count bob's own job only: {}",
        r.text()
    );
    let r = enqueue(
        &env,
        &bob,
        Some(&tb),
        json!({"kind": "report", "idempotency_key": "k1", "payload": {}}),
    )
    .await;
    assert_eq!(r.status, 201, "{}", r.text());
    let r = enqueue(
        &env,
        &alice,
        Some(&ta),
        json!({"kind": "report", "idempotency_key": "k1", "payload": {}}),
    )
    .await;
    assert_eq!(
        r.status,
        201,
        "another tenant's idempotency key is not matched: {}",
        r.text()
    );
    assert_eq!(r.json()["tenant"], json!(ta));

    // Same tenant, another creator: only with `engine:read_jobs:all`.
    let r = enqueue(&env, &carol, Some(&ta), json!({"kind": "report", "payload": {}})).await;
    assert_eq!(r.status, 201, "carol enqueues: {}", r.text());
    let c_job = r.json()["id"].as_str().unwrap().to_owned();
    let seen = listed(&env, &carol, Some(&ta)).await;
    assert!(
        seen.contains(&c_job) && !seen.contains(&a_job),
        "carol sees her own jobs: {seen:?}"
    );
    let r = env.as_(&carol, "GET", &path, &tenant(&ta), None).await;
    assert_eq!(r.status, 404, "another member's job: {}", r.text());
    let r = env
        .as_(&carol, "POST", &format!("{path}/cancel"), &tenant(&ta), None)
        .await;
    assert_eq!(r.status, 404, "cancel another member's job: {}", r.text());
    // The owner (`*`) sees every job of the tenant.
    let seen = listed(&env, &alice, Some(&ta)).await;
    assert!(
        seen.contains(&a_job) && seen.contains(&c_job) && !seen.contains(&b_job),
        "{seen:?}"
    );

    // Job channels follow the same visibility.
    let realtime = env.server.realtime();
    let bob_ctx = AccessContext::user("users", id_of(&bob_pk)).with_tenant(tb.clone());
    let carol_ctx = AccessContext::user("users", id_of(&carol_pk))
        .with_org(org_a.clone())
        .with_tenant(ta.clone());
    let alice_ctx = AccessContext::user("users", id_of(&alice_pk))
        .with_org(org_a.clone())
        .with_tenant(ta.clone());
    let ch = format!("jobs:{a_job}");
    assert!(realtime.presence(&bob_ctx, &ch).is_err(), "bob watches alice's job");
    assert!(realtime.presence(&carol_ctx, &ch).is_err(), "carol watches alice's job");
    assert!(realtime.presence(&alice_ctx, &ch).is_ok(), "alice watches her job");
    assert!(realtime.presence(&carol_ctx, &format!("jobs:{c_job}")).is_ok());

    realm_role(
        &env,
        &admin,
        &carol_pk,
        "fb142_jobs",
        &[
            "engine:read_jobs",
            "engine:read_jobs:all",
            "engine:enqueue_job",
            "engine:channel_subscribe",
        ],
    )
    .await;
    let seen = listed(&env, &carol, Some(&ta)).await;
    assert!(
        seen.contains(&a_job) && !seen.contains(&b_job),
        "read_jobs:all: the tenant only: {seen:?}"
    );
    let r = env.as_(&carol, "GET", &path, &tenant(&ta), None).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(
        realtime.presence(&carol_ctx, &ch).is_ok(),
        "read_jobs:all watches the tenant's jobs"
    );
    env.shutdown().await;
}

#[tokio::test]
async fn g4_reserved_kinds_invoke_access_and_worker_claims() {
    let env = Env::new().await;
    let admin = env.admin("root@fb142.test").await;
    let (alice, _) = env.user("alice@fb142.test").await;
    let (bob, _) = env.user("bob@fb142.test").await;
    let (carol, carol_pk) = env.user("carol@fb142.test").await;
    let (org_a, ta) = org(&env, &alice, "acme").await;
    let (_org_b, tb) = org(&env, &bob, "bobco").await;
    member(&env, &alice, &org_a, &carol_pk).await;
    realm_role(
        &env,
        &admin,
        &carol_pk,
        "fb142_worker",
        &[
            "engine:read_jobs",
            "engine:enqueue_job",
            "engine:manage_jobs",
            "engine:work_jobs",
        ],
    )
    .await;
    deploy(&env, "secret", WHOAMI, InvokeAccess::Permission, vec![]).await;
    deploy(&env, "open", WHOAMI, InvokeAccess::Public, vec![]).await;

    // System kinds are reserved, even for an owner with `*`.
    for kind in [
        "customize.email.send",
        "storage.reconcile",
        "billing.maintenance",
        "forgebase.x",
    ] {
        let r = enqueue(
            &env,
            &alice,
            Some(&ta),
            json!({"kind": kind, "payload": {"to": "x@y.z"}}),
        )
        .await;
        assert_eq!(r.status, 403, "{kind}: {}", r.text());
    }
    let r = env
        .as_(
            &alice,
            "POST",
            "/api/v1/jobs/schedules",
            &tenant(&ta),
            Some(json!({"name": "spam", "cron": "* * * * *", "kind": "customize.email.send", "payload": {}})),
        )
        .await;
    assert_eq!(r.status, 403, "a schedule of a reserved kind: {}", r.text());
    let r = env
        .as_(
            &alice,
            "POST",
            "/api/v1/jobs/workflows",
            &tenant(&ta),
            Some(json!({"name": "w", "steps": [{"name": "a", "kind": "billing.maintenance"}]})),
        )
        .await;
    assert_eq!(r.status, 403, "a workflow step of a reserved kind: {}", r.text());
    let r = enqueue(&env, &admin, None, json!({"kind": "forgebase.x", "payload": {}})).await;
    assert_eq!(r.status, 201, "administrators may: {}", r.text());
    let reserved = r.json()["id"].as_str().unwrap().to_owned();

    // function.invoke: the caller's invoke access, checked at enqueue.
    for f in ["secret", "missing"] {
        let r = enqueue(
            &env,
            &carol,
            Some(&ta),
            json!({"kind": "function.invoke", "payload": {"function": f}}),
        )
        .await;
        assert_eq!(r.status, 403, "function {f}: {}", r.text());
    }
    let r = env
        .as_(
            &carol,
            "POST",
            "/api/v1/jobs/schedules",
            &tenant(&ta),
            Some(json!({"name": "s", "cron": "* * * * *", "kind": "function.invoke",
                        "payload": {"function": "secret"}})),
        )
        .await;
    assert_eq!(
        r.status,
        403,
        "a schedule invoking a function carol may not: {}",
        r.text()
    );
    let r = enqueue(
        &env,
        &carol,
        Some(&ta),
        json!({"kind": "function.invoke", "payload": {"function": "open"}}),
    )
    .await;
    assert_eq!(r.status, 201, "a public function: {}", r.text());

    // A non-privileged worker claims public kinds of its own tenant only.
    let r = enqueue(&env, &bob, Some(&tb), json!({"kind": "crunch", "payload": {}})).await;
    assert_eq!(r.status, 201, "{}", r.text());
    let foreign = r.json()["id"].as_str().unwrap().to_owned();
    let r = enqueue(&env, &carol, Some(&ta), json!({"kind": "crunch", "payload": {}})).await;
    let own = r.json()["id"].as_str().unwrap().to_owned();
    let worker = AccessContext::user("users", id_of(&carol_pk))
        .with_org(org_a.clone())
        .with_tenant(ta.clone());
    let claimed = env
        .server
        .jobs()
        .claim(
            &worker,
            ClaimRequest {
                worker_id: "w1".into(),
                kinds: vec!["crunch".into(), "forgebase.x".into()],
                max: Some(10),
                ..ClaimRequest::default()
            },
        )
        .await
        .unwrap();
    let ids: Vec<&str> = claimed.iter().map(|c| c.job.id.as_str()).collect();
    assert_eq!(ids, vec![own.as_str()], "only carol's tenant's public job");
    assert!(!ids.contains(&foreign.as_str()) && !ids.contains(&reserved.as_str()));
    env.shutdown().await;
}

#[tokio::test]
async fn g4_function_invoke_runs_as_the_recorded_enqueuer() {
    let env = Env::new().await;
    let admin = env.admin("root@fb142.test").await;
    let (alice, alice_pk) = env.user("alice@fb142.test").await;
    let (_org_a, ta) = org(&env, &alice, "acme").await;
    deploy(&env, "whoami", WHOAMI, InvokeAccess::Authenticated, vec![]).await;

    let r = enqueue(
        &env,
        &alice,
        Some(&ta),
        json!({"kind": "function.invoke", "payload": {"function": "whoami"}}),
    )
    .await;
    assert_eq!(r.status, 201, "{}", r.text());
    let id = r.json()["id"].as_str().unwrap().to_owned();
    let job = finished(&env, &alice, Some(&ta), &id).await;
    assert_eq!(job["status"], "succeeded", "{job}");
    let auth = &job["result"]["auth"];
    assert_eq!(auth["type"], "user", "runs as alice: {job}");
    assert_eq!(auth["id"], json!(id_of(&alice_pk)), "{job}");
    assert_eq!(auth["tenant"], json!(ta), "in alice's tenant: {job}");

    // An administrator's job does not lend its rights to function code.
    let r = enqueue(
        &env,
        &admin,
        Some(&ta),
        json!({"kind": "function.invoke", "payload": {"function": "whoami"}}),
    )
    .await;
    assert_eq!(r.status, 201, "{}", r.text());
    let id = r.json()["id"].as_str().unwrap().to_owned();
    let job = finished(&env, &admin, None, &id).await;
    assert_eq!(job["result"]["auth"]["type"], "service", "{job}");
    assert_eq!(job["result"]["auth"]["id"], "function.whoami", "{job}");
    assert_eq!(job["result"]["auth"]["tenant"], json!(ta), "{job}");
    env.shutdown().await;
}

#[tokio::test]
async fn g5_hooks_run_with_an_explicit_context() {
    let env = Env::new().await;
    let admin = env.admin("root@fb142.test").await;
    let (alice, _) = env.user("alice@fb142.test").await;
    let (org_a, ta) = org(&env, &alice, "acme").await;
    let r = env
        .as_(
            &admin,
            "POST",
            "/api/v1/collections",
            &[],
            Some(json!({"name": "notes", "options": {"tenant_scoped": true},
                        "fields": [{"name": "title", "type": "text"}]})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.text());
    let open = "@request.auth.id != ''";
    let r = env
        .as_(
            &admin,
            "PUT",
            "/api/v1/policies/notes",
            &[],
            Some(json!({"list_rule": open, "view_rule": open, "create_rule": open,
                        "update_rule": open, "delete_rule": open})),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let hook = |e: HookEvent| {
        vec![HookSpec {
            collection: "notes".into(),
            events: vec![e],
        }]
    };
    deploy(
        &env,
        "gate",
        WHOAMI,
        InvokeAccess::Permission,
        hook(HookEvent::BeforeCreate),
    )
    .await;
    deploy(
        &env,
        "audit",
        WHOAMI,
        InvokeAccess::Permission,
        hook(HookEvent::AfterCreate),
    )
    .await;

    let h = [("X-ForgeBase-Org", org_a.as_str()), ("X-ForgeBase-Tenant", ta.as_str())];
    let r = env
        .as_(
            &alice,
            "POST",
            "/api/v1/collections/notes/records",
            &h,
            Some(json!({"title": "a"})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.text());
    let r = env
        .as_(
            &admin,
            "POST",
            "/api/v1/collections/notes/records",
            &tenant(&ta),
            Some(json!({"title": "b"})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.text());
    let fns = env.server.functions().unwrap();
    let lines = |logs: Vec<forgebase_functions::ExecutionLog>| -> Vec<Json> {
        logs.iter()
            .flat_map(|l| l.logs.iter())
            .filter_map(|l| l.message.strip_prefix("auth "))
            .map(|m| serde_json::from_str(m).unwrap())
            .collect()
    };
    let root = AccessContext::admin("root");
    // The server's after-hook task delivers the changes (at least once: a
    // manual `run_after_hooks` would race it on the shared cursor).
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let after = loop {
        let after = lines(fns.logs(&root, "audit", 50).await.unwrap());
        if after.len() >= 2 || std::time::Instant::now() > deadline {
            break after;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert!(after.len() >= 2, "{after:?}");
    for a in &after {
        assert_eq!(a["type"], "service", "{a}");
        assert_eq!(a["tenant"], json!(ta), "after-hooks act in the change's tenant: {a}");
    }
    let before = lines(fns.logs(&root, "gate", 50).await.unwrap());
    assert_eq!(before.len(), 2, "{before:?}");
    assert!(before.iter().any(|a| a["type"] == "user"), "alice's write: {before:?}");
    assert!(
        before.iter().all(|a| a["type"] != "admin"),
        "an administrator's write runs the hook as the function: {before:?}"
    );
    assert!(
        before
            .iter()
            .any(|a| a["type"] == "service" && a["id"] == "function.gate" && a["tenant"] == json!(ta))
    );
    env.shutdown().await;
}

#[tokio::test]
async fn functions_auth_verify_reports_the_key_tenant_binding() {
    let env = Env::new().await;
    let (alice, _) = env.user("alice@fb142.test").await;
    let (_org_a, ta) = org(&env, &alice, "acme").await;
    let r = env
        .as_(
            &alice,
            "POST",
            "/api/auth/tokens",
            &[],
            Some(json!({"name": "bound", "scopes": ["*"], "tenant": ta})),
        )
        .await;
    assert_eq!(r.status, 201, "bound PAT: {}", r.text());
    let key = r.json()["key"].as_str().unwrap().to_owned();
    deploy(
        &env,
        "verify",
        r#"import { serve } from "forgebase";
serve(async (req: Request, ctx) => ({ v: await ctx.verifyToken(new URL(req.url).searchParams.get("t") ?? "") }));
"#,
        InvokeAccess::Public,
        vec![],
    )
    .await;
    let res = env
        .server
        .functions()
        .unwrap()
        .invoke_http(
            &AccessContext::anonymous(),
            "verify",
            HttpInvocation {
                method: "GET".into(),
                path: "/api/v1/functions/verify/invoke".into(),
                sub_path: String::new(),
                query: format!("t={key}"),
                headers: HeaderMap::new(),
                body: Vec::new(),
            },
        )
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: Json = serde_json::from_slice(&axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap()).unwrap();
    assert_eq!(body["v"]["type"], "user", "{body}");
    assert_eq!(body["v"]["tenant"], json!(ta), "the binding is reported: {body}");
    env.shutdown().await;
}
