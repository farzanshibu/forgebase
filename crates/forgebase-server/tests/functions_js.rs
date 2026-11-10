//! JavaScript functions on the live server (FB-93, ADR-0027): every `ctx`
//! service runs as the caller, so the policy engine, bucket ACLs and
//! key-value namespace rules allow or deny it exactly as for a direct call;
//! `ctx.admin` (service_role) bypasses them. Binds `127.0.0.1:0`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use axum::http::HeaderMap;
use forgebase_core::Config;
use forgebase_data::AccessContext;
use forgebase_functions::http::HttpInvocation;
use forgebase_functions::{DeployRequest, InvokeAccess, KvAccess, KvRule};
use forgebase_policy::snapshot::Effect;
use serde_json::{Value as Json, json};

fn config(dir: &Path) -> Config {
    let mut cfg = Config::defaults(dir);
    cfg.server.host = "127.0.0.1".into();
    cfg.server.port = 0;
    cfg.server.shutdown_timeout_secs = 5;
    cfg
}

const SERVICES: &str = r#"import { serve } from "forgebase";

// Tries every ctx service and reports "ok"-ish values or the error code.
serve(async (req: Request, ctx) => {
  const admin = new URL(req.url).searchParams.get("as") === "admin";
  const s = admin ? ctx.admin : ctx;
  const out: Record<string, unknown> = {};
  const t = async (name: string, f: () => Promise<unknown>) => {
    try {
      out[name] = await f();
    } catch (e: any) {
      out[name] = `ERR:${e.code}`;
    }
  };
  await t("db_list", async () => (await s.db.from("notes").list()).items.length);
  await t("vector_upsert", async () => (await s.vector.from("docs").upsert({ id: "d1", vector: [1, 0] })) ? "ok" : "?");
  await t("vector_search", async () => (await s.vector.from("docs").search({ vector: [1, 0], k: 1, exact: true })).hits.length);
  await t("storage_put", async () => (await s.storage.from("files").put("a.txt", "hello")) ? "ok" : "?");
  await t("storage_get", async () => (await s.storage.from("files").get("a.txt"))?.text());
  await t("storage_sign", async () => (await s.storage.from("files").signedUrl("a.txt", { expiresIn: 60 })).url.includes("a.txt"));
  await t("queue", async () => (await s.queues.publish("events", { a: 1 })).duplicate === false);
  await t("job", async () => (await s.jobs.enqueue("noop", { a: 1 })).created);
  await t("realtime", async () => { await s.realtime.broadcast("room.1", "hi", { a: 1 }); return "ok"; });
  await t("kv_app", async () => s.kv.namespace("app").set("k", "v"));
  return Response.json(out);
});
"#;

async fn invoke(server: &forgebase_server::RunningServer, ctx: &AccessContext, query: &str) -> Json {
    let res = server
        .functions()
        .invoke_http(
            ctx,
            "svc",
            HttpInvocation {
                method: "GET".into(),
                path: "/api/v1/functions/svc/invoke".into(),
                sub_path: String::new(),
                query: query.into(),
                headers: HeaderMap::new(),
                body: Vec::new(),
            },
        )
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
    serde_json::from_slice(&body).unwrap()
}

#[tokio::test]
async fn every_ctx_service_runs_as_the_caller_and_service_role_bypasses() {
    let dir = tempfile::tempdir().unwrap();
    let server = forgebase_server::start(&config(dir.path())).await.unwrap();
    let admin = AccessContext::admin("root");
    let fns = server.functions();
    let services = fns.attached_services();
    let policy = services.policy.clone().unwrap();
    let storage = services.storage.clone().unwrap();

    // Data the function reaches, created by an administrator.
    let data = fns.data();
    data.create_collection(
        &admin,
        serde_json::from_value(json!({"name": "notes", "fields": [{"name": "owner", "type": "text"}]})).unwrap(),
    )
    .await
    .unwrap();
    for owner in ["u1", "u2"] {
        data.create_record(&admin, "notes", json!({"owner": owner})).await.unwrap();
    }
    policy
        .put_policy(
            &admin,
            "notes",
            &forgebase_policy::store::PolicyInput {
                list_rule: Some("owner = @request.auth.id".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    server
        .vector()
        .create_collection(
            &admin,
            serde_json::from_value(json!({"name": "docs", "fields": [{"name": "v", "dimension": 2, "metric": "cosine"}]}))
                .unwrap(),
        )
        .await
        .unwrap();
    storage
        .create_bucket(&admin, serde_json::from_value(json!({"name": "files"})).unwrap())
        .await
        .unwrap();
    server
        .queue()
        .unwrap()
        .create_topic(&admin, serde_json::from_value(json!({"name": "events"})).unwrap())
        .await
        .unwrap();

    let caps = [
        "db.read",
        "kv",
        "vector.read",
        "vector.write",
        "storage.read",
        "storage.write",
        "queues.publish",
        "jobs.enqueue",
        "realtime.publish",
        "service_role",
    ];
    fns.deploy(
        &admin,
        DeployRequest {
            name: "svc".into(),
            source: Some(SERVICES.into()),
            capabilities: caps.iter().map(|c| (*c).to_owned()).collect(),
            access: Some(InvokeAccess::Authenticated),
            ..DeployRequest::default()
        },
    )
    .await
    .unwrap();
    fns.set_kv_rules(
        &admin,
        "svc",
        vec![KvRule {
            namespace: "app".into(),
            access: KvAccess::Write,
        }],
    )
    .await
    .unwrap();

    // An ordinary caller with no grants: every engine refuses.
    let u1 = AccessContext::user("users", "u1");
    let denied = invoke(&server, &u1, "").await;
    assert_eq!(denied["db_list"], 1, "the list rule shows u1's own note: {denied}");
    for k in [
        "vector_upsert",
        "vector_search",
        "storage_put",
        "storage_get",
        "storage_sign",
        "queue",
        "job",
        "realtime",
        "kv_app",
    ] {
        let v = denied[k].as_str().unwrap_or_default();
        assert!(v.starts_with("ERR:"), "{k} should be denied: {denied}");
    }

    // Grants through the policy engine and the bucket ACL allow each one.
    for p in [
        "engine:vector_read",
        "engine:vector_write",
        "engine:queue_publish",
        "engine:enqueue_job",
        "engine:channel_publish",
        "engine:kv_read",
        "engine:kv_write",
    ] {
        policy
            .add_override(&admin, None, "user:users/u1", p, Effect::Allow)
            .await
            .unwrap();
    }
    storage
        .set_acl(
            &admin,
            "files",
            serde_json::from_value(json!({"entries": [{"prefix": "", "grantee": "user:users/u1", "permission": "write"}]}))
                .unwrap(),
        )
        .await
        .unwrap();
    let allowed = invoke(&server, &u1, "").await;
    assert_eq!(
        allowed,
        json!({
            "db_list": 1, "vector_upsert": "ok", "vector_search": 1, "storage_put": "ok", "storage_get": "hello",
            "storage_sign": true, "queue": true, "job": true, "realtime": "ok", "kv_app": true
        }),
        "{allowed}"
    );

    // Another ordinary caller still has nothing...
    let u2 = AccessContext::user("users", "u2");
    let other = invoke(&server, &u2, "").await;
    assert!(other["storage_get"].as_str().unwrap().starts_with("ERR:"), "{other}");
    // ...but ctx.admin (service_role) bypasses rules, policies and ACLs.
    let bypass = invoke(&server, &u2, "as=admin").await;
    assert_eq!(bypass["db_list"], 2, "{bypass}");
    for k in ["vector_upsert", "storage_put", "realtime"] {
        assert_eq!(bypass[k], "ok", "{bypass}");
    }
    assert_eq!(bypass["storage_get"], "hello");
    assert_eq!(bypass["queue"], true);
    assert_eq!(bypass["job"], true);
    assert_eq!(bypass["kv_app"], true);

    server.shutdown().await.unwrap();
}
