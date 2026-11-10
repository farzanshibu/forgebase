//! JavaScript and TypeScript functions (FB-93, ADR-0027) over the real
//! `/api/v1` router: deploy from source, compile errors with positions,
//! capabilities at deploy and at call time, `ctx` services as the caller
//! under collection rules and policies, `service_role`, limits, Web APIs,
//! hooks, the source and types endpoints and test runs of a version.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::middleware::{self, Next};
use common::{admin, harness};
use forgebase_api::{AppState, RouterOptions};
use forgebase_core::DataLayout;
use forgebase_data::AccessContext;
use forgebase_data::db::Backend as _;
use forgebase_policy::snapshot::Effect;
use forgebase_policy::store::PolicyInput;
use serde_json::{Value as Json, json};
use tower::ServiceExt;

/// `x-test-as: admin | user:<id> | anon` sets the caller (as the auth layer would).
fn app(h: &common::H) -> Router {
    let layout = DataLayout::new(h.dir.path().join("data"));
    let state = AppState::from_data(h.db.clone(), layout, h.data.clone());
    let app = forgebase_api::router(
        state,
        RouterOptions {
            extensions: vec![forgebase_functions::extension(h.svc.clone())],
            ..RouterOptions::default()
        },
    );
    app.layer(middleware::from_fn(|mut req: Request<Body>, next: Next| async move {
        let who = req
            .headers()
            .get("x-test-as")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        match who.as_deref() {
            Some("admin") => {
                req.extensions_mut().insert(admin());
            }
            Some(w) if w.starts_with("user:") => {
                req.extensions_mut().insert(AccessContext::user("users", &w[5..]));
            }
            _ => {}
        }
        next.run(req).await
    }))
}

struct Res {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: Vec<u8>,
}

impl Res {
    fn json(&self) -> Json {
        serde_json::from_slice(&self.body).unwrap_or(Json::Null)
    }
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

async fn call(app: &Router, method: Method, uri: &str, who: &str, body: Option<Json>) -> Res {
    let mut req = Request::builder().method(method).uri(uri).header("x-test-as", who);
    let body = match body {
        Some(b) => {
            req = req.header("content-type", "application/json");
            Body::from(b.to_string())
        }
        None => Body::empty(),
    };
    let res = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let body = axum::body::to_bytes(res.into_body(), 1 << 24).await.unwrap().to_vec();
    Res { status, headers, body }
}

/// `POST /api/v1/functions` as administrator with `extra` merged into the body.
async fn deploy(app: &Router, name: &str, source: &str, extra: Json) -> Res {
    let mut body = json!({"name": name, "source": source, "access": "public"});
    if let Json::Object(m) = extra {
        for (k, v) in m {
            body[k] = v;
        }
    }
    call(app, Method::POST, "/api/v1/functions", "admin", Some(body)).await
}

async fn deploy_ok(app: &Router, name: &str, source: &str, extra: Json) -> Json {
    let r = deploy(app, name, source, extra).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()
}

async fn grant(h: &common::H, principal: &str, permission: &str) {
    h.policy
        .add_override(&admin(), None, principal, permission, Effect::Allow)
        .await
        .unwrap();
}

const TODOS: &str = r#"import { serve } from "forgebase";

interface Todo { id?: string; title: string; owner?: string }

serve(async (req: Request, ctx): Promise<Response> => {
  const todos = ctx.db.from("todos");
  if (req.method === "POST") {
    const body = (await req.json()) as Todo;
    try {
      const rec = await todos.create({ title: body.title, owner: ctx.auth?.id ?? "" });
      return Response.json(rec, { status: 201 });
    } catch (e: any) {
      return Response.json({ code: e.code, message: e.message }, { status: 403 });
    }
  }
  const url = new URL(req.url);
  const list = await todos.list({ sort: "title" });
  return Response.json({
    path: url.pathname,
    q: url.searchParams.get("q"),
    caller: ctx.auth?.id ?? null,
    titles: list.items.map((t: any) => t.title),
  });
});
"#;

async fn todos(h: &common::H) {
    h.collection(json!({"name": "todos", "fields": [
        {"name": "title", "type": "text"},
        {"name": "owner", "type": "text"}
    ]}))
    .await;
    // Row-level security: callers see and create only their own rows.
    h.policy
        .put_policy(
            &admin(),
            "todos",
            &PolicyInput {
                list_rule: Some("owner = @request.auth.id".into()),
                view_rule: Some("owner = @request.auth.id".into()),
                create_rule: Some("@request.auth.id != ''".into()),
                ..PolicyInput::default()
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn a_typescript_http_function_runs_end_to_end_as_the_caller() {
    let h = harness().await;
    todos(&h).await;
    let app = app(&h);
    let d = deploy_ok(
        &app,
        "todos",
        TODOS,
        json!({"capabilities": ["db.read", "db.write", "log"]}),
    )
    .await;
    assert_eq!(d["version"]["runtime"], "js");
    assert_eq!(d["version"]["language"], "typescript");

    for (who, title) in [("user:u1", "b"), ("user:u1", "a"), ("user:u2", "c")] {
        let r = call(
            &app,
            Method::POST,
            "/api/v1/functions/todos/invoke",
            who,
            Some(json!({"title": title})),
        )
        .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    }
    // Anonymous callers pass no create rule.
    let r = call(
        &app,
        Method::POST,
        "/api/v1/functions/todos/invoke",
        "anon",
        Some(json!({"title": "x"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());

    // Each caller sees only their rows (list rule), through the sub-path route.
    let r = call(
        &app,
        Method::GET,
        "/api/v1/functions/todos/invoke/items/all?q=1",
        "user:u1",
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        r.json(),
        json!({"path": "/items/all", "q": "1", "caller": "u1", "titles": ["a", "b"]})
    );
    let rid = r.headers["x-request-id"].to_str().unwrap().to_owned();
    let r2 = call(&app, Method::GET, "/api/v1/functions/todos/invoke", "user:u2", None).await;
    assert_eq!(r2.json()["titles"], json!(["c"]));
    assert_eq!(r2.json()["path"], "/");
    // Anonymous callers are refused by the data core; the trace shows the
    // function's own frames only.
    let r3 = call(&app, Method::GET, "/api/v1/functions/todos/invoke", "anon", None).await;
    assert_eq!(r3.status, StatusCode::BAD_GATEWAY);
    let msg = r3.json()["error"]["message"].as_str().unwrap().to_owned();
    assert!(
        msg.contains("authentication required") && msg.contains("index.ts:"),
        "{msg}"
    );
    assert!(!msg.contains("forgebase:prelude"), "{msg}");

    // The response's x-request-id is the execution's log entry.
    let logs = call(
        &app,
        Method::GET,
        "/api/v1/functions/todos/logs?limit=20",
        "admin",
        None,
    )
    .await;
    let items = logs.json()["items"].as_array().cloned().unwrap();
    assert!(
        items
            .iter()
            .any(|e| e["request_id"] == json!(rid) && e["status"] == "ok"),
        "{rid} not in {items:?}"
    );

    // Versions carry runtime and language; the source is served as deployed.
    let v = call(&app, Method::GET, "/api/v1/functions/todos/versions", "admin", None).await;
    assert_eq!(v.json()["items"][0]["runtime"], "js");
    assert_eq!(v.json()["items"][0]["language"], "typescript");
    let src = call(
        &app,
        Method::GET,
        "/api/v1/functions/todos/versions/1/source",
        "admin",
        None,
    )
    .await;
    assert_eq!(src.status, StatusCode::OK);
    assert_eq!(src.json(), json!({"language": "typescript", "source": TODOS}));
    assert_eq!(
        call(
            &app,
            Method::GET,
            "/api/v1/functions/todos/versions/9/source",
            "admin",
            None
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    // The types for editors, as text.
    let t = call(&app, Method::GET, "/api/v1/functions/types", "admin", None).await;
    assert_eq!(t.status, StatusCode::OK);
    assert!(
        t.headers["content-type"]
            .to_str()
            .unwrap()
            .starts_with("application/typescript")
    );
    assert!(t.text().contains("declare module \"forgebase\""));
    assert_eq!(t.text(), forgebase_functions::FORGEBASE_DTS);
}

#[tokio::test]
async fn a_webassembly_version_has_no_source_and_old_configs_read_as_wasm() {
    let h = harness().await;
    let app = app(&h);
    let w = common::Wat::new(&[]);
    h.deploy(common::req("plain", &w.wasm("(i32.const 0)"), &[])).await;
    let v = call(&app, Method::GET, "/api/v1/functions/plain/versions", "admin", None).await;
    assert_eq!(v.json()["items"][0]["runtime"], "wasm");
    assert_eq!(v.json()["items"][0]["language"], "wasm");
    let r = call(
        &app,
        Method::GET,
        "/api/v1/functions/plain/versions/1/source",
        "admin",
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(r.json()["error"]["code"], "function_source_not_found");
    // A version row written before 0026 (no runtime/language in its config).
    let c: forgebase_functions::VersionConfig =
        serde_json::from_value(json!({"capabilities": [], "limits": forgebase_functions::Limits::default()})).unwrap();
    assert_eq!(c.runtime, forgebase_functions::FunctionRuntime::Wasm);
    assert_eq!(c.language, forgebase_functions::Language::Wasm);
}

#[tokio::test]
async fn compile_errors_carry_line_and_column() {
    let h = harness().await;
    let app = app(&h);
    let r = deploy(
        &app,
        "bad",
        "import { serve } from \"forgebase\";\nserve((req) => {\n  return new Response(;\n});\n",
        json!({}),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    let e = &r.json()["error"];
    assert_eq!(e["code"], "function_compile_failed");
    assert_eq!(e["details"]["line"], 3, "{e}");
    assert!(e["details"]["column"].as_u64().unwrap() >= 1);
    assert!(e["details"]["message"].is_string());
    assert_eq!(e["details"]["errors"][0]["line"], 3);
    assert!(e["message"].as_str().unwrap().contains("line 3"));

    // Imports other than "forgebase" are compile errors at their position.
    let r = deploy(
        &app,
        "imp",
        "import x from \"./util.ts\";\nimport y from \"npm:left-pad\";\n",
        json!({}),
    )
    .await;
    let e = &r.json()["error"];
    assert_eq!(e["code"], "function_compile_failed");
    assert_eq!(e["details"]["errors"].as_array().unwrap().len(), 2);
    assert_eq!(e["details"]["errors"][1]["line"], 2);
    // TypeScript syntax in a JavaScript version.
    let r = deploy(&app, "jsts", "let x: number = 1;", json!({"language": "javascript"})).await;
    assert_eq!(r.json()["error"]["code"], "function_compile_failed");
    // Nothing was created.
    assert_eq!(
        call(&app, Method::GET, "/api/v1/functions/bad", "admin", None)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    // Both or neither code field.
    let r = call(
        &app,
        Method::POST,
        "/api/v1/functions",
        "admin",
        Some(json!({"name": "x", "source": "1", "wasm": "AA=="})),
    )
    .await;
    assert_eq!(r.json()["error"]["code"], "invalid_deploy");
    let r = call(
        &app,
        Method::POST,
        "/api/v1/functions",
        "admin",
        Some(json!({"name": "x"})),
    )
    .await;
    assert_eq!(r.json()["error"]["code"], "invalid_deploy");
    // "types" is a route, not a function name.
    let r = deploy(&app, "types", "1", json!({})).await;
    assert_eq!(r.json()["error"]["code"], "invalid_function_name");
}

#[tokio::test]
async fn capabilities_are_checked_at_deploy_and_at_call_time() {
    let h = harness().await;
    todos(&h).await;
    let app = app(&h);
    // A visible write without db.write is refused at deploy, at its position.
    let src = "import { serve } from \"forgebase\";\nserve(async (r, ctx) => {\n  await ctx.db.from(\"todos\").create({ title: \"x\" });\n});\n";
    let r = deploy(&app, "w", src, json!({"capabilities": ["db.read"]})).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    let e = &r.json()["error"];
    assert_eq!(e["code"], "capability_not_granted");
    assert_eq!(e["details"]["line"], 3);
    assert!(e["message"].as_str().unwrap().contains("db.write"), "{e}");
    let r = deploy(&app, "f", "serve(() => fetch(\"https://example.com\"));", json!({})).await;
    assert_eq!(r.json()["error"]["code"], "capability_not_granted");

    // Hidden use (computed names) deploys and is refused when called.
    let hidden = r#"import { serve } from "forgebase";
serve(async (req, ctx) => {
  const out: Record<string, string> = {};
  for (const svc of ["db", "kv", "admin"]) {
    try {
      if (svc === "db") await ctx[svc].from("todos").list();
      else if (svc === "kv") await ctx[svc].get("k");
      else await ctx[svc]["db"].from("todos").list();
      out[svc] = "ok";
    } catch (e: any) {
      out[svc] = e.code;
    }
  }
  try {
    out.env = String(ctx.env["get"]("A"));
  } catch (e: any) {
    out.env = e.code;
  }
  return Response.json(out);
});
"#;
    deploy_ok(&app, "hidden", hidden, json!({})).await;
    let msg = "serve(async (r, ctx) => { try { await ctx[\"kv\"].get(\"a\"); } catch (e) { return new Response(e.message); } });";
    deploy_ok(&app, "kvmsg", msg, json!({"language": "javascript"})).await;
    let r = call(&app, Method::GET, "/api/v1/functions/kvmsg/invoke", "anon", None).await;
    assert_eq!(
        r.text(),
        "ctx.kv needs the 'kv' capability: add it under Capabilities and deploy again"
    );
    let r = call(&app, Method::GET, "/api/v1/functions/hidden/invoke", "user:u1", None).await;
    assert_eq!(
        r.json(),
        json!({"db": "capability_denied", "kv": "capability_denied", "admin": "capability_denied", "env": "capability_denied"}),
        "{}",
        r.text()
    );
}

#[tokio::test]
async fn service_role_is_granted_only_by_administrators_and_bypasses_rules() {
    let h = harness().await;
    todos(&h).await;
    let app = app(&h);
    for (owner, title) in [("u1", "a"), ("u2", "b")] {
        h.data
            .create_record(&admin(), "todos", json!({"title": title, "owner": owner}))
            .await
            .unwrap();
    }
    let src = r#"import { serve } from "forgebase";
serve(async (req, ctx) => {
  const mine = await ctx.db.from("todos").list();
  const all = await ctx.admin.db.from("todos").list({ sort: "title" });
  return Response.json({ mine: mine.items.length, all: all.items.map((t) => t.title) });
});
"#;
    // ctx.admin needs service_role (deploy-time check).
    let r = deploy(&app, "adm", src, json!({"capabilities": ["db.read"]})).await;
    assert_eq!(r.json()["error"]["code"], "capability_not_granted");
    // A manager who is not an administrator cannot grant it: 403.
    grant(&h, "user:users/dev", "engine:manage_functions").await;
    let r = call(
        &app,
        Method::POST,
        "/api/v1/functions",
        "user:dev",
        Some(json!({"name": "adm", "source": src, "capabilities": ["db.read", "service_role"]})),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    assert_eq!(r.json()["error"]["code"], "service_role_requires_admin");
    // ...but may deploy other versions.
    let r = call(
        &app,
        Method::POST,
        "/api/v1/functions",
        "user:dev",
        Some(json!({"name": "devfn", "source": "serve(() => new Response('x'));", "language": "javascript"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());

    deploy_ok(&app, "adm", src, json!({"capabilities": ["db.read", "service_role"]})).await;
    let r = call(&app, Method::GET, "/api/v1/functions/adm/invoke", "user:u1", None).await;
    assert_eq!(r.json(), json!({"mine": 1, "all": ["a", "b"]}), "{}", r.text());
    // Every deploy that grants it is audited.
    let rows =
        h.db.fetch_all(&forgebase_data::db::Statement::new(
            "SELECT actor, detail FROM _forgebase_auth_audit WHERE event = 'functions.service_role_granted'",
            vec![],
        ))
        .await
        .unwrap();
    assert_eq!(rows.rows.len(), 1);
    assert_eq!(rows.rows[0][0].as_str(), Some("admin:root"));
    assert!(rows.rows[0][1].as_str().unwrap().contains("\"function\":\"adm\""));
    // service_role is for JavaScript only.
    let w = common::Wat::new(&[]);
    let mut req = common::req("wsr", &w.wasm("(i32.const 0)"), &["service_role"]);
    req.access = None;
    let err = h.svc.deploy(&admin(), req).await.unwrap_err();
    assert_eq!(forgebase_core::ErrorCode::code(&err), "invalid_capability");
}

#[tokio::test]
async fn limits_stop_javascript_loops_allocations_and_waits() {
    let h = harness().await;
    let app = app(&h);
    deploy_ok(
        &app,
        "spin",
        "serve(() => { while (true) {} });",
        json!({"language": "javascript"}),
    )
    .await;
    let r = call(&app, Method::GET, "/api/v1/functions/spin/invoke", "anon", None).await;
    assert_eq!(r.status, StatusCode::BAD_GATEWAY, "{}", r.text());
    assert_eq!(r.json()["error"]["code"], "function_fuel_exhausted");

    let hog = "serve(() => { const a = []; while (true) a.push(new Array(100000).fill(1)); });";
    deploy_ok(
        &app,
        "hog",
        hog,
        json!({"language": "javascript", "limits": {"memory_bytes": 8388608, "fuel": 20000000000u64, "timeout_ms": 20000}}),
    )
    .await;
    let r = call(&app, Method::GET, "/api/v1/functions/hog/invoke", "anon", None).await;
    assert_eq!(r.json()["error"]["code"], "function_memory_limit", "{}", r.text());

    deploy_ok(
        &app,
        "slow",
        "serve(() => { while (true) {} });",
        json!({"language": "javascript", "limits": {"fuel": 20000000000u64, "timeout_ms": 300}}),
    )
    .await;
    let t = std::time::Instant::now();
    let r = call(&app, Method::GET, "/api/v1/functions/slow/invoke", "anon", None).await;
    assert_eq!(r.status, StatusCode::GATEWAY_TIMEOUT, "{}", r.text());
    assert_eq!(r.json()["error"]["code"], "function_timeout");
    assert!(t.elapsed() < std::time::Duration::from_secs(5));

    deploy_ok(
        &app,
        "wait",
        "serve(async () => { await new Promise((r) => setTimeout(r, 60000)); return new Response('late'); });",
        json!({"language": "javascript", "limits": {"timeout_ms": 300}}),
    )
    .await;
    let r = call(&app, Method::GET, "/api/v1/functions/wait/invoke", "anon", None).await;
    assert_eq!(r.json()["error"]["code"], "function_timeout");

    // Deep recursion is an exception (or a trap), never a crash.
    deploy_ok(
        &app,
        "deep",
        "function f(n) { return f(n + 1) + 1; }\nserve(() => { try { f(0); } catch (e) { return new Response(e.name); } });",
        json!({"language": "javascript", "limits": {"fuel": 20000000000u64, "timeout_ms": 20000}}),
    )
    .await;
    let r = call(&app, Method::GET, "/api/v1/functions/deep/invoke", "anon", None).await;
    assert!(
        r.text() == "RangeError" || r.json()["error"]["code"] == "function_trap",
        "{} {}",
        r.status,
        r.text()
    );

    // Memory limits below the engine's own need are refused at deploy.
    let r = deploy(
        &app,
        "tiny",
        "1",
        json!({"language": "javascript", "limits": {"memory_bytes": 1048576}}),
    )
    .await;
    assert_eq!(r.json()["error"]["code"], "memory_limit_too_small");

    // An uncaught exception fails the execution with its message and position.
    deploy_ok(
        &app,
        "boom",
        "serve(() => {\n  throw new Error(\"kaput\");\n});",
        json!({"language": "javascript"}),
    )
    .await;
    let r = call(&app, Method::GET, "/api/v1/functions/boom/invoke", "anon", None).await;
    assert_eq!(r.status, StatusCode::BAD_GATEWAY);
    assert_eq!(r.json()["error"]["code"], "function_failed");
    let msg = r.json()["error"]["message"].as_str().unwrap().to_owned();
    assert!(msg.contains("kaput") && msg.contains("index.js:2"), "{msg}");

    // TypeScript traces point at the .ts source, and the trace is logged.
    let ts = "import { serve } from \"forgebase\";\n\ninterface Big {\n  n: number;\n}\n\nfunction fail(b: Big): never {\n  throw new TypeError(`bad ${b.n}`);\n}\n\nserve(() => fail({ n: 7 }));\n";
    deploy_ok(&app, "tsboom", ts, json!({})).await;
    let r = call(&app, Method::GET, "/api/v1/functions/tsboom/invoke", "anon", None).await;
    let msg = r.json()["error"]["message"].as_str().unwrap().to_owned();
    assert!(
        msg.contains("uncaught exception") && msg.contains("TypeError: bad 7"),
        "{msg}"
    );
    assert!(msg.contains("index.ts:8:"), "{msg}");
    assert!(!msg.contains("index.js"), "{msg}");
    let logs = h.svc.logs(&admin(), "tsboom", 1).await.unwrap();
    let line = logs[0].logs.last().unwrap();
    assert_eq!(line.level, "error");
    assert!(
        line.message.starts_with("Uncaught TypeError: bad 7") && line.message.contains("index.ts:8:"),
        "{line:?}"
    );
    // Limit errors say how to fix them.
    let r = call(&app, Method::GET, "/api/v1/functions/spin/invoke", "anon", None).await;
    assert!(
        r.json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("raise limits.fuel"),
        "{}",
        r.text()
    );
}

const WEB: &str = r#"import { serve, ForgeBaseError } from "forgebase";
serve(async (req, ctx) => {
  const out = {};
  const h = new Headers({ "X-A": "1" });
  h.append("x-a", "2");
  out.headers = h.get("x-a");
  const u = new URL("../b/c?x=1&y=two%20words#h", "https://Example.COM:443/a/z");
  out.url = [u.href, u.host, u.pathname, u.searchParams.get("y"), u.hash, u.origin];
  const p = new URLSearchParams({ a: "1 2", b: "é" });
  p.append("c", "&");
  out.params = p.toString();
  const bytes = new TextEncoder().encode("héllo");
  out.utf8 = [bytes.length, new TextDecoder().decode(bytes)];
  out.b64 = [btoa("hi"), atob("aGk=")];
  const d = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode("abc")));
  out.sha = Array.from(d.slice(0, 4));
  const key = await crypto.subtle.importKey("raw", new TextEncoder().encode("Jefe"), { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
  const mac = new Uint8Array(await crypto.subtle.sign("HMAC", key, new TextEncoder().encode("what do ya want for nothing?")));
  out.hmac = Array.from(mac.slice(0, 4));
  out.uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(crypto.randomUUID());
  out.rand = crypto.getRandomValues(new Uint8Array(8)).length;
  const c = structuredClone({ d: new Date(0), m: new Map([[1, 2]]), a: [1, { b: 2 }] });
  out.clone = [c.d instanceof Date, c.m.get(1), c.a[1].b];
  const order = [];
  setTimeout(() => order.push("t2"), 20);
  setTimeout(() => order.push("t1"), 5);
  queueMicrotask(() => order.push("micro"));
  await new Promise((r) => setTimeout(r, 30));
  out.order = order;
  const res = new Response("text body", { status: 201, headers: { "x-b": "3" } });
  out.response = [res.status, res.headers.get("content-type"), await res.text(), res.ok];
  const j = Response.json({ k: 1 });
  out.json = [j.headers.get("content-type"), await j.json()];
  out.req = [req.method, req.headers.get("x-custom"), await req.text()];
  out.random = Math.random();
  out.isError = new ForgeBaseError("c", "m") instanceof Error;
  console.debug("dbg");
  console.info("info", { n: 1 });
  console.warn("warn");
  console.error("err", new Error("e1").message);
  out.env = [ctx.env.get("PLAIN"), ctx.env.get("SECRET"), Deno.env.get("PLAIN"), ctx.env.get("NONE") ?? null];
  out.function = ctx.function;
  out.trigger = ctx.trigger;
  return Response.json(out, { headers: { "x-out": "yes", "set-cookie": "no=1" } });
});
"#;

#[tokio::test]
async fn web_apis_behave_like_other_runtimes() {
    let h = harness().await;
    let app = app(&h);
    deploy_ok(
        &app,
        "web",
        WEB,
        json!({"language": "javascript", "capabilities": ["env", "secrets"], "env": {"PLAIN": "p"}}),
    )
    .await;
    h.svc.put_secret(&admin(), "web", "SECRET", "s3cret").await.unwrap();
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/functions/web/invoke")
        .header("x-test-as", "anon")
        .header("x-custom", "c")
        .header("content-type", "text/plain")
        .body(Body::from("raw body"))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()["x-out"], "yes");
    assert!(res.headers().get("set-cookie").is_none());
    let v: Json = serde_json::from_slice(&axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap()).unwrap();
    assert_eq!(v["headers"], "1, 2");
    assert_eq!(
        v["url"],
        json!([
            "https://example.com/b/c?x=1&y=two%20words#h",
            "example.com",
            "/b/c",
            "two words",
            "#h",
            "https://example.com"
        ])
    );
    assert_eq!(v["params"], "a=1+2&b=%C3%A9&c=%26");
    assert_eq!(v["utf8"], json!([6, "héllo"]));
    assert_eq!(v["b64"], json!(["aGk=", "hi"]));
    assert_eq!(v["sha"], json!([0xba, 0x78, 0x16, 0xbf]));
    assert_eq!(v["hmac"], json!([0x5b, 0xdc, 0xc1, 0x46]));
    assert_eq!(v["uuid"], true);
    assert_eq!(v["rand"], 8);
    assert_eq!(v["clone"], json!([true, 2, 2]));
    assert_eq!(v["order"], json!(["micro", "t1", "t2"]));
    assert_eq!(
        v["response"],
        json!([201, "text/plain;charset=UTF-8", "text body", true])
    );
    assert_eq!(v["json"], json!(["application/json", {"k": 1}]));
    assert_eq!(v["req"], json!(["POST", "c", "raw body"]));
    assert_eq!(v["isError"], true);
    assert_eq!(v["env"], json!(["p", "s3cret", "p", null]));
    assert_eq!(v["function"], json!({"name": "web", "version": 1}));
    assert_eq!(v["trigger"], "http");
    // Math.random is seeded per execution (the engine snapshot is shared).
    let again = call(
        &app,
        Method::POST,
        "/api/v1/functions/web/invoke",
        "anon",
        Some(json!({})),
    )
    .await;
    assert_ne!(again.json()["random"], v["random"]);
    // console.* became execution log lines with their levels.
    let logs = h.svc.logs(&admin(), "web", 5).await.unwrap();
    let last = logs.iter().find(|l| l.status == "ok").unwrap();
    let levels: Vec<(&str, &str)> = last
        .logs
        .iter()
        .map(|l| (l.level.as_str(), l.message.as_str()))
        .collect();
    assert_eq!(
        levels,
        vec![
            ("debug", "dbg"),
            ("info", "info { n: 1 }"),
            ("warn", "warn"),
            ("error", "err e1")
        ]
    );
    assert!(last.fuel_used < 30_000_000, "fuel {}", last.fuel_used);
}

// FB-138: callers without a tenant reach application data in shared.* namespaces.
#[tokio::test]
async fn key_value_namespaces_are_private_by_default_and_ruled_for_applications() {
    let h = harness().await;
    let app = app(&h);
    let src = r#"import { serve } from "forgebase";
serve(async (req, ctx) => {
  const n = await ctx.kv.incr("hits");
  await ctx.kv.hset("h", { a: 1, b: "x" });
  const h = await ctx.kv.hgetall("h");
  await ctx.kv.set("t", "v", { ttlMs: 60000 });
  const keys = await ctx.kv.keys("");
  let app;
  try {
    await ctx.kv.namespace("shared.app").set("shared", "1");
    app = await ctx.kv.namespace("shared.app").get("shared");
  } catch (e) {
    app = e.code;
  }
  return Response.json({ n, h, keys, exists: await ctx.kv.exists("t"), app });
});
"#;
    deploy_ok(
        &app,
        "counter",
        src,
        json!({"language": "javascript", "capabilities": ["kv"]}),
    )
    .await;
    // The private namespace needs only the capability, even for anonymous callers.
    let r = call(&app, Method::GET, "/api/v1/functions/counter/invoke", "anon", None).await;
    assert_eq!(
        r.json(),
        json!({"n": 1, "h": {"a": "1", "b": "x"}, "keys": ["h", "hits", "t"], "exists": true, "app": "kv_namespace_denied"}),
        "{}",
        r.text()
    );
    // Rules are for administrators only.
    grant(&h, "user:users/dev", "engine:manage_functions").await;
    let rules = json!({"items": [{"namespace": "app", "access": "write"}]});
    let r = call(
        &app,
        Method::PUT,
        "/api/v1/functions/counter/kv-rules",
        "user:dev",
        Some(rules.clone()),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    let r = call(
        &app,
        Method::PUT,
        "/api/v1/functions/counter/kv-rules",
        "admin",
        Some(rules.clone()),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json(), rules);
    // With the rule, the caller's own key-value permission still decides.
    let r = call(&app, Method::GET, "/api/v1/functions/counter/invoke", "user:u1", None).await;
    assert_eq!(r.json()["app"], "forbidden", "{}", r.text());
    assert_eq!(r.json()["n"], 2);
    grant(&h, "user:users/u1", "engine:kv_read").await;
    grant(&h, "user:users/u1", "engine:kv_write").await;
    let r = call(&app, Method::GET, "/api/v1/functions/counter/invoke", "user:u1", None).await;
    assert_eq!(r.json()["app"], "1", "{}", r.text());
    // A read rule allows reads only.
    let ro = json!({"items": [{"namespace": "app*", "access": "read"}]});
    call(
        &app,
        Method::PUT,
        "/api/v1/functions/counter/kv-rules",
        "admin",
        Some(ro),
    )
    .await;
    let r = call(&app, Method::GET, "/api/v1/functions/counter/invoke", "user:u1", None).await;
    assert_eq!(r.json()["app"], "kv_namespace_denied");
    let r = call(
        &app,
        Method::PUT,
        "/api/v1/functions/counter/kv-rules",
        "admin",
        Some(json!({"items": [{"namespace": "a b", "access": "read"}]})),
    )
    .await;
    assert_eq!(r.json()["error"]["code"], "invalid_kv_rule");
}

#[tokio::test]
async fn javascript_hooks_and_jobs_see_their_event_and_the_writer() {
    let h = harness().await;
    h.collection(json!({"name": "posts", "fields": [{"name": "title", "type": "text"}]}))
        .await;
    let app = app(&h);
    let guard = r#"import { serve } from "forgebase";
serve(async (req, ctx) => {
  const ev = await req.json();
  if (ev.data?.title === "spam") return { allow: false, reason: `no spam from ${ctx.auth?.id}` };
  return { allow: true };
});
"#;
    deploy_ok(
        &app,
        "guard",
        guard,
        json!({"language": "javascript", "hooks": [{"collection": "posts", "events": ["before_create"]}]}),
    )
    .await;
    let writer = AccessContext::user("users", "w1");
    grant(&h, "user:users/w1", "data:posts:create").await;
    let err = h
        .data
        .create_record(&writer, "posts", json!({"title": "spam"}))
        .await
        .unwrap_err();
    assert_eq!(forgebase_core::ErrorCode::code(&err), "hook_rejected");
    assert!(err.to_string().contains("no spam from w1"), "{err}");
    h.data
        .create_record(&writer, "posts", json!({"title": "fine"}))
        .await
        .unwrap();

    let job = r#"import { serve } from "forgebase";
serve(async (req, ctx) => {
  const ev = await req.json();
  return { got: ev.input, trigger: ctx.trigger, caller: ctx.auth?.id, event: ctx.event.trigger };
});
"#;
    deploy_ok(&app, "job", job, json!({"language": "javascript"})).await;
    let out = h
        .svc
        .invoke(
            "job",
            forgebase_functions::Trigger::Job,
            &json!({"trigger": "job", "input": {"x": 1}}),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        out.json().unwrap().unwrap(),
        json!({"got": {"x": 1}, "trigger": "job", "caller": "function.job", "event": "job"})
    );
}

#[tokio::test]
async fn administrators_test_a_specific_version() {
    let h = harness().await;
    let app = app(&h);
    deploy_ok(
        &app,
        "ver",
        "serve(() => new Response('one'));",
        json!({"language": "javascript"}),
    )
    .await;
    deploy_ok(
        &app,
        "ver",
        "serve((req) => new Response('two ' + new URL(req.url).search));",
        json!({"language": "javascript", "activate": false}),
    )
    .await;
    let r = call(
        &app,
        Method::GET,
        "/api/v1/functions/ver/invoke?version=2&a=b",
        "admin",
        None,
    )
    .await;
    assert_eq!(r.text(), "two ?a=b");
    let r = call(
        &app,
        Method::GET,
        "/api/v1/functions/ver/invoke?version=2",
        "anon",
        None,
    )
    .await;
    assert_eq!(r.text(), "one", "others get the active version");
    let r = call(
        &app,
        Method::GET,
        "/api/v1/functions/ver/invoke?version=7",
        "admin",
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let logs = h.svc.logs(&admin(), "ver", 5).await.unwrap();
    assert!(logs.iter().any(|l| l.version == 2 && l.trigger == "test"));
}

mod net {
    use std::io;
    use std::net::{IpAddr, SocketAddr};

    use forgebase_db::BoxFuture;
    use forgebase_functions::fetch::{Connector, Resolver};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    pub const PUBLIC: &str = "93.184.215.14";

    /// `api.test` and `other.test` are public, `evil.test` is loopback.
    #[derive(Debug)]
    pub struct Names;

    impl Resolver for Names {
        fn resolve<'a>(&'a self, host: &'a str, _port: u16) -> BoxFuture<'a, io::Result<Vec<IpAddr>>> {
            let ip = match host {
                "api.test" | "other.test" => Some(PUBLIC),
                "evil.test" => Some("127.0.0.1"),
                _ => None,
            };
            Box::pin(async move {
                ip.map(|i| vec![i.parse().unwrap()])
                    .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such host"))
            })
        }
    }

    /// Routes the public address to `mock`.
    #[derive(Debug)]
    pub struct Route(pub SocketAddr);

    impl Connector for Route {
        fn connect(&self, addr: SocketAddr) -> BoxFuture<'_, io::Result<TcpStream>> {
            let target = if addr.ip().to_string() == PUBLIC { self.0 } else { addr };
            Box::pin(TcpStream::connect(target))
        }
    }

    /// Answers every request with `{"method", "body"}` as JSON.
    pub async fn echo() -> SocketAddr {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    loop {
                        let n = s.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        let text = String::from_utf8_lossy(&buf).to_string();
                        if let Some(i) = text.find("\r\n\r\n") {
                            let len = text[..i]
                                .lines()
                                .find_map(|l| {
                                    l.to_ascii_lowercase()
                                        .strip_prefix("content-length:")
                                        .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                                })
                                .unwrap_or(0);
                            if buf.len() >= i + 4 + len {
                                let method = text.split(' ').next().unwrap_or("").to_owned();
                                let body = String::from_utf8_lossy(&buf[i + 4..i + 4 + len]).to_string();
                                let out = serde_json::json!({"method": method, "body": body}).to_string();
                                let resp = format!(
                                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{out}",
                                    out.len()
                                );
                                let _ = s.write_all(resp.as_bytes()).await;
                                return;
                            }
                        }
                    }
                });
            }
        });
        addr
    }
}

#[tokio::test]
async fn fetch_goes_through_the_ssrf_guard_and_the_host_list() {
    use forgebase_functions::fetch::{FetchConfig, Fetcher};
    let mock = net::echo().await;
    let fetcher = Fetcher::with(
        &forgebase_auth::net::TlsSettings::new(),
        FetchConfig::default(),
        std::sync::Arc::new(net::Names),
        std::sync::Arc::new(net::Route(mock)),
    )
    .unwrap();
    let h = common::harness_with(forgebase_functions::FunctionsConfig {
        fetcher: Some(fetcher),
        ..forgebase_functions::FunctionsConfig::default()
    })
    .await;
    let app = app(&h);
    let src = r#"serve(async () => {
  const out = {};
  const r = await fetch("http://api.test/echo", { method: "POST", body: "hi", headers: { "content-type": "text/plain" } });
  out.ok = [r.status, r.ok, await r.json()];
  for (const u of ["http://evil.test/", "http://169.254.169.254/latest/meta-data/", "http://other.test/", "file:///etc/passwd"]) {
    try { await fetch(u); out[u] = "reached"; } catch (e) { out[u] = e.name + ":" + (e.code ?? "-"); }
  }
  return Response.json(out);
});"#;
    deploy_ok(
        &app,
        "fetcher",
        src,
        json!({"language": "javascript", "capabilities": ["fetch"], "fetch_hosts": ["api.test", "evil.test"]}),
    )
    .await;
    let r = call(&app, Method::GET, "/api/v1/functions/fetcher/invoke", "anon", None).await;
    let v = r.json();
    assert_eq!(
        v["ok"],
        json!([200, true, {"method": "POST", "body": "hi"}]),
        "{}",
        r.text()
    );
    for u in [
        "http://evil.test/",
        "http://169.254.169.254/latest/meta-data/",
        "http://other.test/",
    ] {
        let got = v[u].as_str().unwrap();
        assert!(got.starts_with("TypeError:") && !got.ends_with(":-"), "{u}: {got}");
    }
    assert!(
        v["file:///etc/passwd"].as_str().unwrap().starts_with("TypeError"),
        "{v}"
    );
}

#[tokio::test]
async fn javascript_after_hooks_run_for_committed_changes() {
    let h = harness().await;
    h.collection(json!({"name": "orders", "fields": [{"name": "n", "type": "number"}]}))
        .await;
    let app = app(&h);
    let src = r#"serve(async (req, ctx) => {
  const ev = await req.json();
  await ctx.kv.incr(`${ev.event}:${ev.collection}`);
  await ctx.kv.set("last", String(ev.record.n));
  return new Response(null, { status: 204 });
});"#;
    deploy_ok(
        &app,
        "counter",
        src,
        json!({"language": "javascript", "capabilities": ["kv"], "hooks": [{"collection": "orders", "events": ["after_create", "after_update"]}]}),
    )
    .await;
    let rec = h.data.create_record(&admin(), "orders", json!({"n": 1})).await.unwrap();
    let id = rec["id"].as_str().unwrap().to_owned();
    h.data
        .update_record(
            &admin(),
            "orders",
            &id,
            json!({"n": 2}),
            forgebase_data::UpdateOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(h.svc.run_after_hooks().await.unwrap(), 2);
    let get = |k: &str| {
        h.kv.exec(
            &AccessContext::system(),
            "fn.counter",
            &[b"GET".to_vec(), k.as_bytes().to_vec()],
        )
        .unwrap()
    };
    assert_eq!(get("after_create:orders"), forgebase_kv::Reply::Bulk(b"1".to_vec()));
    assert_eq!(get("after_update:orders"), forgebase_kv::Reply::Bulk(b"1".to_vec()));
    assert_eq!(get("last"), forgebase_kv::Reply::Bulk(b"2".to_vec()));
}
