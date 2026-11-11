//! FB-146 (FB-141/FB-142 follow-up): named-queue settings and workers belong
//! to a tenant, like jobs, against the real server. Both tests failed before
//! FB-146: `_forgebase_job_queues` and `_forgebase_job_workers` were one
//! instance-wide namespace, so any `read_jobs` holder listed every tenant's
//! queue settings, counts and workers, any `manage_jobs` holder changed or
//! deleted another tenant's settings (and pausing a queue paused it for every
//! tenant), and any `work_jobs` holder took another tenant's worker id over.
//! Binds `127.0.0.1:0`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod security_common;

use security_common::{Env, Resp};
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

/// `method path` as `token`, in tenant `t` when given.
async fn call(env: &Env, token: &str, t: Option<&str>, method: &str, path: &str, body: Option<Json>) -> Resp {
    let h: Vec<(&str, &str)> = t.map(|t| ("X-ForgeBase-Tenant", t)).into_iter().collect();
    env.as_(token, method, path, &h, body).await
}

/// The queues `token` lists in `t`, as (name, tenant, paused, queued).
async fn queues(env: &Env, token: &str, t: Option<&str>) -> Vec<(String, Json, bool, i64)> {
    let r = call(env, token, t, "GET", "/api/v1/jobs/queues", None).await;
    assert_eq!(r.status, 200, "queues: {}", r.text());
    r.json()["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|q| {
            (
                q["name"].as_str().unwrap().to_owned(),
                q["tenant"].clone(),
                q["paused"].as_bool().unwrap(),
                q["queued"].as_i64().unwrap(),
            )
        })
        .collect()
}

/// The worker ids `token` lists in `t`.
async fn workers(env: &Env, token: &str, t: Option<&str>) -> Vec<String> {
    let r = call(env, token, t, "GET", "/api/v1/jobs/workers", None).await;
    assert_eq!(r.status, 200, "workers: {}", r.text());
    r.json()["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["id"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn queue_settings_belong_to_a_tenant_and_govern_only_its_jobs() {
    let env = Env::new().await;
    let admin = env.admin("root@fb146.test").await;
    let (alice, _) = env.user("alice@fb146.test").await;
    let (bob, _) = env.user("bob@fb146.test").await;
    let (carol, carol_pk) = env.user("carol@fb146.test").await;
    let (_org_a, ta) = org(&env, &alice, "acme").await;
    let (_org_b, tb) = org(&env, &bob, "bobco").await;
    // Carol has no tenant; her realm role grants every jobs action.
    realm_role(
        &env,
        &admin,
        &carol_pk,
        "fb146_jobs",
        &[
            "engine:read_jobs",
            "engine:enqueue_job",
            "engine:manage_jobs",
            "engine:work_jobs",
        ],
    )
    .await;

    // Alice pauses `emails` in her tenant and queues a job there.
    let r = call(
        &env,
        &alice,
        Some(&ta),
        "PUT",
        "/api/v1/jobs/queues/emails",
        Some(json!({"paused": true, "default_priority": 7})),
    )
    .await;
    assert_eq!(r.status, 200, "alice configures: {}", r.text());
    assert_eq!(r.json()["tenant"], json!(ta), "the settings record their tenant");
    let r = call(
        &env,
        &alice,
        Some(&ta),
        "POST",
        "/api/v1/jobs",
        Some(json!({"kind": "send", "queue": "emails", "payload": {}})),
    )
    .await;
    assert_eq!(r.status, 201, "{}", r.text());
    assert_eq!(r.json()["priority"], 7, "her queue's default priority applies");

    // Bob's tenant does not see Alice's settings or her job counts.
    let seen = queues(&env, &bob, Some(&tb)).await;
    assert!(seen.is_empty(), "bob sees no queue of another tenant: {seen:?}");
    let r = call(&env, &bob, Some(&tb), "GET", "/api/v1/jobs/queues/emails", None).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["configured"], false, "bob's emails queue is his own: {}", r.text());
    assert_eq!(r.json()["queued"], 0, "{}", r.text());

    // Bob's queue of the same name is not paused by Alice: his job is claimed,
    // and his own default priority applies.
    let r = call(
        &env,
        &bob,
        Some(&tb),
        "PUT",
        "/api/v1/jobs/queues/emails",
        Some(json!({"default_priority": 3})),
    )
    .await;
    assert_eq!(r.status, 200, "bob configures his own: {}", r.text());
    assert_eq!(r.json()["paused"], false, "{}", r.text());
    let r = call(
        &env,
        &bob,
        Some(&tb),
        "POST",
        "/api/v1/jobs",
        Some(json!({"kind": "send", "queue": "emails", "payload": {}})),
    )
    .await;
    assert_eq!(r.status, 201, "{}", r.text());
    assert_eq!(r.json()["priority"], 3, "{}", r.text());
    let b_job = r.json()["id"].as_str().unwrap().to_owned();
    let r = call(
        &env,
        &bob,
        Some(&tb),
        "POST",
        "/api/v1/jobs/claim",
        Some(json!({"worker_id": "bob-w", "queues": ["emails"], "max": 5})),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(
        r.json()["jobs"].as_array().unwrap().len(),
        1,
        "another tenant's pause does not hold bob's queue: {}",
        r.text()
    );
    let r = call(&env, &bob, Some(&tb), "GET", &format!("/api/v1/jobs/{b_job}"), None).await;
    assert_eq!(r.json()["status"], "running", "{}", r.text());

    // Bob deletes and resumes only his own settings.
    let r = call(&env, &bob, Some(&tb), "POST", "/api/v1/jobs/queues/emails/resume", None).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let r = call(&env, &bob, Some(&tb), "DELETE", "/api/v1/jobs/queues/emails", None).await;
    assert!(r.status < 300, "{}", r.text());
    let r = call(&env, &alice, Some(&ta), "GET", "/api/v1/jobs/queues/emails", None).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let a = r.json();
    assert_eq!(
        (a["configured"].clone(), a["paused"].clone(), a["default_priority"].clone()),
        (json!(true), json!(true), json!(7)),
        "alice's settings are untouched: {a}"
    );
    assert_eq!(a["queued"], 1, "alice's job is still held by her pause: {a}");

    // A tenantless caller sees and changes only tenantless settings.
    let seen = queues(&env, &carol, None).await;
    assert!(seen.is_empty(), "carol (no tenant) sees no tenant's queues: {seen:?}");
    let r = call(
        &env,
        &carol,
        None,
        "PUT",
        "/api/v1/jobs/queues/emails",
        Some(json!({"paused": false, "concurrency": 1})),
    )
    .await;
    assert_eq!(r.status, 200, "carol configures the tenantless queue: {}", r.text());
    assert_eq!(r.json()["tenant"], Json::Null, "{}", r.text());
    let r = call(&env, &carol, None, "DELETE", "/api/v1/jobs/queues/emails", None).await;
    assert!(r.status < 300, "{}", r.text());
    let r = call(&env, &alice, Some(&ta), "GET", "/api/v1/jobs/queues/emails", None).await;
    assert_eq!(
        (r.json()["paused"].clone(), r.json()["concurrency"].clone()),
        (json!(true), Json::Null),
        "carol did not change alice's settings: {}",
        r.text()
    );
    let r = call(
        &env,
        &carol,
        None,
        "POST",
        "/api/v1/jobs/claim",
        Some(json!({"worker_id": "carol-w", "queues": ["emails"], "max": 5})),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["jobs"], json!([]), "no tenant: no tenant jobs: {}", r.text());

    // An administrator sees every tenant's settings, each with its tenant.
    let all = queues(&env, &admin, None).await;
    let emails: Vec<&Json> = all.iter().filter(|q| q.0 == "emails").map(|q| &q.1).collect();
    assert!(
        emails.contains(&&json!(ta)) && emails.contains(&&json!(tb)),
        "the administrator lists both tenants' emails queues: {all:?}"
    );
}

#[tokio::test]
async fn workers_belong_to_a_tenant_and_their_ids_cannot_be_taken_over() {
    let env = Env::new().await;
    let admin = env.admin("root@fb146w.test").await;
    let (alice, alice_pk) = env.user("alice@fb146w.test").await;
    let (bob, _) = env.user("bob@fb146w.test").await;
    let (carol, carol_pk) = env.user("carol@fb146w.test").await;
    let (_org_a, ta) = org(&env, &alice, "acme").await;
    let (_org_b, tb) = org(&env, &bob, "bobco").await;
    realm_role(
        &env,
        &admin,
        &carol_pk,
        "fb146_workers",
        &["engine:read_jobs", "engine:work_jobs"],
    )
    .await;

    let r = call(
        &env,
        &alice,
        Some(&ta),
        "POST",
        "/api/v1/jobs/workers",
        Some(json!({"id": "w-mailer", "name": "alice mailer", "queues": ["emails"]})),
    )
    .await;
    assert_eq!(r.status, 201, "alice registers: {}", r.text());
    assert_eq!(r.json()["tenant"], json!(ta), "{}", r.text());
    assert_eq!(r.json()["created_by"], json!(alice_pk), "{}", r.text());

    // Not listed for another tenant or for a tenantless caller.
    let seen = workers(&env, &bob, Some(&tb)).await;
    assert!(!seen.contains(&"w-mailer".to_owned()), "bob: {seen:?}");
    let seen = workers(&env, &carol, None).await;
    assert!(!seen.contains(&"w-mailer".to_owned()), "carol: {seen:?}");
    assert!(workers(&env, &alice, Some(&ta)).await.contains(&"w-mailer".to_owned()));

    // Nobody else registers under Alice's worker id; her row is unchanged.
    for (token, t) in [(&bob, Some(tb.as_str())), (&carol, None)] {
        let r = call(
            &env,
            token,
            t,
            "POST",
            "/api/v1/jobs/workers",
            Some(json!({"id": "w-mailer", "name": "hijack", "queues": ["other"]})),
        )
        .await;
        assert_eq!(r.status, 409, "taking another tenant's worker id over: {}", r.text());
        assert_eq!(r.json()["error"]["code"], "worker_id_taken", "{}", r.text());
    }
    // A claim under that id does not touch her worker either.
    let r = call(
        &env,
        &bob,
        Some(&tb),
        "POST",
        "/api/v1/jobs/claim",
        Some(json!({"worker_id": "w-mailer", "max": 1})),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let r = call(&env, &admin, None, "GET", "/api/v1/jobs/workers", None).await;
    let w = r.json()["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["id"] == "w-mailer")
        .cloned()
        .unwrap();
    assert_eq!(
        (w["name"].clone(), w["queues"].clone(), w["tenant"].clone()),
        (json!("alice mailer"), json!(["emails"]), json!(ta)),
        "alice's worker is untouched: {w}"
    );
    let first_seen = w["last_seen_at"].clone();
    let r = call(
        &env,
        &alice,
        Some(&ta),
        "POST",
        "/api/v1/jobs/workers",
        Some(json!({"id": "w-mailer", "name": "alice mailer 2", "queues": ["emails"]})),
    )
    .await;
    assert_eq!(r.status, 201, "alice updates her own worker: {}", r.text());
    assert_eq!(r.json()["name"], "alice mailer 2", "{}", r.text());
    assert!(r.json()["last_seen_at"].as_str() >= first_seen.as_str());
}
