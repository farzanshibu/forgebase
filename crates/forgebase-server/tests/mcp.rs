//! MCP server (ADR-0030) against a real server with every module: off by
//! default, credentials and scopes decide what an agent may do, every curated
//! tool's operation exists, tool calls act with the caller's rights,
//! destructive calls need confirmation and are audited, browsers from other
//! origins are refused, and the protocol's edges answer as specified.
//! Binds a local port (`127.0.0.1:0`).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod security_common;

use std::path::Path;
use std::sync::Arc;

use forgebase_auth::{Argon2Config, AuthConfig, AuthService, ClientInfo};
use forgebase_core::Config;
use forgebase_data::DefaultAuthorizer;
use forgebase_server::{RunningServer, start};
use security_common::{ADMIN_PASSWORD, Resp, USER_PASSWORD, bearer, send};
use serde_json::{Value as Json, json};

/// Every curated tool: each must be listed by a full server, which proves its
/// method and path match a documented operation.
const CURATED: &[&str] = &[
    "whoami",
    "server_info",
    "get_features",
    "update_config",
    "list_collections",
    "get_collection",
    "create_collection",
    "update_collection",
    "delete_collection",
    "list_records",
    "get_record",
    "create_record",
    "update_record",
    "delete_record",
    "run_sql",
    "list_tables",
    "list_buckets",
    "list_objects",
    "read_object",
    "write_object",
    "delete_object",
    "list_functions",
    "get_function",
    "deploy_function",
    "invoke_function",
    "function_logs",
    "delete_function",
    "list_jobs",
    "enqueue_job",
    "get_job",
    "cancel_job",
    "retry_job",
    "list_service_accounts",
    "create_service_account",
    "create_api_key",
    "list_api_keys",
    "revoke_api_key",
    "audit_log",
];

fn config(dir: &Path, mcp: bool) -> Config {
    let mut cfg = Config::defaults(dir);
    cfg.server.host = "127.0.0.1".into();
    cfg.server.port = 0;
    cfg.server.shutdown_timeout_secs = 5;
    cfg.mcp.enabled = mcp;
    cfg
}

async fn side_auth(server: &RunningServer, data_dir: &Path) -> AuthService {
    let mut acfg = AuthConfig::for_data_dir(data_dir);
    acfg.argon2 = Argon2Config::insecure_fast_for_tests();
    AuthService::open_with_data(
        Arc::new(server.database().clone()),
        Arc::new(DefaultAuthorizer),
        data_dir,
        acfg,
        None,
    )
    .await
    .unwrap()
}

async fn admin(server: &RunningServer, data_dir: &Path) -> String {
    side_auth(server, data_dir)
        .await
        .create_admin(
            "agent-admin@example.com",
            ADMIN_PASSWORD,
            "test",
            &ClientInfo::default(),
        )
        .await
        .unwrap();
    let body = serde_json::to_vec(&json!({"email": "agent-admin@example.com", "password": ADMIN_PASSWORD})).unwrap();
    let r = send(server.local_addr(), "POST", "/api/auth/admins/login", &[], &body).await;
    assert_eq!(r.status, 200, "admin login: {}", r.text());
    r.json()["access_token"].as_str().unwrap().to_owned()
}

async fn user(server: &RunningServer, data_dir: &Path) -> String {
    side_auth(server, data_dir)
        .await
        .signup(
            &ClientInfo::default(),
            None,
            "agent-user@example.com",
            USER_PASSWORD,
            None,
        )
        .await
        .unwrap();
    let body = serde_json::to_vec(&json!({"email": "agent-user@example.com", "password": USER_PASSWORD})).unwrap();
    let r = send(server.local_addr(), "POST", "/api/auth/login", &[], &body).await;
    assert_eq!(r.status, 200, "login: {}", r.text());
    r.json()["access_token"].as_str().unwrap().to_owned()
}

async fn rest(server: &RunningServer, token: &str, method: &str, path: &str, body: Option<Json>) -> Resp {
    let auth = bearer(token);
    let bytes = body.map(|b| serde_json::to_vec(&b).unwrap()).unwrap_or_default();
    send(
        server.local_addr(),
        method,
        path,
        &[("Authorization", auth.as_str())],
        &bytes,
    )
    .await
}

/// Posts one MCP message.
async fn post(server: &RunningServer, token: Option<&str>, headers: &[(&str, &str)], msg: &Json) -> Resp {
    let auth = token.map(bearer);
    let mut h: Vec<(&str, &str)> = headers.to_vec();
    if let Some(a) = &auth {
        h.push(("Authorization", a.as_str()));
    }
    h.push(("Accept", "application/json, text/event-stream"));
    send(
        server.local_addr(),
        "POST",
        "/api/mcp",
        &h,
        &serde_json::to_vec(msg).unwrap(),
    )
    .await
}

/// The `result` of a JSON-RPC request.
async fn request(server: &RunningServer, token: &str, method: &str, params: Json) -> Json {
    let r = post(
        server,
        Some(token),
        &[],
        &json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}),
    )
    .await;
    assert_eq!(r.status, 200, "{method}: {}", r.text());
    assert_eq!(r.header("cache-control"), Some("no-store"));
    let j = r.json();
    assert!(j["error"].is_null(), "{method}: {j}");
    j["result"].clone()
}

/// Calls a tool: `(is_error, the text parsed as JSON or as a string)`.
async fn tool(server: &RunningServer, token: &str, name: &str, args: Json) -> (bool, Json) {
    let result = request(server, token, "tools/call", json!({"name": name, "arguments": args})).await;
    let text = result["content"][0]["text"].as_str().unwrap_or_default().to_owned();
    let parsed = serde_json::from_str(&text).unwrap_or(Json::String(text));
    (result["isError"].as_bool().unwrap_or(false), parsed)
}

/// A tool call that must succeed with an HTTP status below 300: its body.
async fn ok(server: &RunningServer, token: &str, name: &str, args: Json) -> Json {
    let (err, out) = tool(server, token, name, args).await;
    assert!(!err, "{name}: {out}");
    let status = out["status"].as_u64().unwrap_or(200);
    assert!(status < 300, "{name}: {out}");
    out["body"].clone()
}

#[tokio::test]
async fn the_mcp_server_is_off_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path(), false);
    let server = start(&cfg).await.unwrap();
    let token = admin(&server, &cfg.data_dir).await;
    let r = post(
        &server,
        Some(&token),
        &[],
        &json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}),
    )
    .await;
    assert_eq!((r.status, r.code().as_str()), (404, "feature_disabled"), "{}", r.text());
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_agent_operates_the_server_with_its_credential() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path(), true);
    let server = start(&cfg).await.unwrap();
    let ping = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"});

    // No credential: 401 with a Bearer challenge.
    let r = post(&server, None, &[], &ping).await;
    assert_eq!(r.status, 401, "{}", r.text());
    assert!(r.header("www-authenticate").unwrap().starts_with("Bearer"));

    let token = admin(&server, &cfg.data_dir).await;

    // Handshake.
    let init = request(
        &server,
        &token,
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}}),
    )
    .await;
    assert_eq!(init["protocolVersion"], "2025-06-18");
    assert_eq!(init["serverInfo"]["name"], "forgebase");
    assert!(init["capabilities"]["tools"].is_object());
    assert!(init["instructions"].as_str().unwrap().contains("whoami"));
    let old = request(&server, &token, "initialize", json!({"protocolVersion": "2099-01-01"})).await;
    assert_eq!(old["protocolVersion"], "2025-06-18", "unknown versions get the newest");
    let r = post(
        &server,
        Some(&token),
        &[],
        &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    )
    .await;
    assert_eq!(r.status, 202);
    assert_eq!(request(&server, &token, "ping", Json::Null).await, json!({}));

    // Every curated tool is listed (its operation exists), plus the generic three.
    let tools = request(&server, &token, "tools/list", json!({})).await;
    let names: Vec<&str> = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for t in CURATED
        .iter()
        .chain(&["list_operations", "describe_operation", "call_operation"])
    {
        assert!(names.contains(t), "{t} is listed: {names:?}");
    }
    for t in tools["tools"].as_array().unwrap() {
        assert_eq!(t["inputSchema"]["type"], "object", "{}", t["name"]);
    }

    // Who am I, and settings.
    assert!(ok(&server, &token, "whoami", json!({})).await.is_object());
    let features = ok(&server, &token, "get_features", json!({})).await;
    assert!(
        features["features"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["id"] == "mcp" && f["enabled"] == true)
    );

    // Collections and records.
    ok(
        &server,
        &token,
        "create_collection",
        json!({"definition": {"name": "notes", "fields": [{"name": "title", "type": "text", "required": true}]}}),
    )
    .await;
    let rec = ok(
        &server,
        &token,
        "create_record",
        json!({"collection": "notes", "record": {"title": "first"}}),
    )
    .await;
    let id = rec["id"].as_str().unwrap().to_owned();
    ok(
        &server,
        &token,
        "update_record",
        json!({"collection": "notes", "id": id, "changes": {"title": "changed"}}),
    )
    .await;
    let page = ok(
        &server,
        &token,
        "list_records",
        json!({"collection": "notes", "filter": "title = 'changed'"}),
    )
    .await;
    assert_eq!(page["items"][0]["id"], id.as_str(), "{page}");
    let (err, out) = tool(
        &server,
        &token,
        "create_record",
        json!({"collection": "notes", "record": {}}),
    )
    .await;
    assert!(
        err && out["status"] == 400,
        "a missing required field is the API's 400: {out}"
    );

    // Files: a bucket through the generic tool, then write and read a file.
    let (err, out) = tool(
        &server,
        &token,
        "call_operation",
        json!({"operation": "POST /api/v1/storage/buckets", "body": {"name": "files"}}),
    )
    .await;
    assert!(!err, "{out}");
    ok(
        &server,
        &token,
        "write_object",
        json!({"bucket": "files", "key": "docs/hello.txt", "content": "hello agent", "content_type": "text/plain"}),
    )
    .await;
    let (err, out) = tool(
        &server,
        &token,
        "read_object",
        json!({"bucket": "files", "key": "docs/hello.txt"}),
    )
    .await;
    assert!(!err, "{out}");
    assert_eq!(out["body_text"], "hello agent", "{out}");

    // The generic tools reach every other operation.
    let tags = ok_answer(&server, &token, "list_operations", json!({})).await;
    assert!(tags["total"].as_u64().unwrap() > 200, "{tags}");
    let found = ok_answer(&server, &token, "list_operations", json!({"search": "bucket"})).await;
    assert!(found["matches"].as_u64().unwrap() > 0, "{found}");
    let described = ok_answer(
        &server,
        &token,
        "describe_operation",
        json!({"operation": "collections_create"}),
    )
    .await;
    assert!(
        described["request_body"]["content"]["application/json"].is_object(),
        "{described}"
    );
    let (err, out) = tool(
        &server,
        &token,
        "describe_operation",
        json!({"operation": "POST /api/mcp"}),
    )
    .await;
    assert!(
        err && out.as_str().unwrap().contains("list_operations"),
        "the endpoint cannot call itself: {out}"
    );

    // Destructive calls need confirmation, then run and are audited.
    let (err, out) = tool(
        &server,
        &token,
        "delete_record",
        json!({"collection": "notes", "id": id}),
    )
    .await;
    assert!(err && out.as_str().unwrap().contains("confirm: true"), "{out}");
    ok(
        &server,
        &token,
        "delete_record",
        json!({"collection": "notes", "id": id, "confirm": true}),
    )
    .await;
    let audit = rest(&server, &token, "GET", "/api/auth/audit?limit=200", None).await;
    assert_eq!(audit.status, 200, "{}", audit.text());
    let calls: Vec<Json> = audit.json()["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["event"] == "mcp.tool_call")
        .cloned()
        .collect();
    let tools_audited: Vec<String> = calls
        .iter()
        .map(|e| detail(e)["tool"].as_str().unwrap_or("").to_owned())
        .collect();
    for t in [
        "create_collection",
        "create_record",
        "update_record",
        "write_object",
        "call_operation",
        "delete_record",
    ] {
        assert!(tools_audited.iter().any(|a| a == t), "{t} audited: {tools_audited:?}");
    }
    assert!(
        !tools_audited.iter().any(|a| a == "list_records"),
        "reads are not audited"
    );
    assert!(
        calls.iter().all(|e| e["actor"].as_str().unwrap().starts_with("admin:")),
        "{calls:?}"
    );

    // Protocol edges.
    let r = post(
        &server,
        Some(&token),
        &[],
        &json!({"jsonrpc": "2.0", "id": 9, "method": "tools/call", "params": {"name": "nope"}}),
    )
    .await;
    assert_eq!(r.json()["error"]["code"], -32602, "{}", r.text());
    let r = post(
        &server,
        Some(&token),
        &[],
        &json!({"jsonrpc": "2.0", "id": 9, "method": "resources/list"}),
    )
    .await;
    assert_eq!(r.json()["error"]["code"], -32601, "{}", r.text());
    let r = post(&server, Some(&token), &[], &json!([ping.clone()])).await;
    assert_eq!(
        (r.status, r.json()["error"]["code"].clone()),
        (400, json!(-32600)),
        "{}",
        r.text()
    );
    let auth = bearer(&token);
    let r = send(
        server.local_addr(),
        "POST",
        "/api/mcp",
        &[("Authorization", auth.as_str())],
        b"{not json",
    )
    .await;
    assert_eq!(
        (r.status, r.json()["error"]["code"].clone()),
        (400, json!(-32700)),
        "{}",
        r.text()
    );
    let r = send(
        server.local_addr(),
        "GET",
        "/api/mcp",
        &[("Authorization", auth.as_str())],
        b"",
    )
    .await;
    assert_eq!((r.status, r.header("allow")), (405, Some("POST")), "{}", r.text());
    let r = post(&server, Some(&token), &[("MCP-Protocol-Version", "1999-01-01")], &ping).await;
    assert_eq!(r.status, 400, "{}", r.text());

    // Browsers: the server's own origin passes, another is refused.
    let r = post(&server, Some(&token), &[("Origin", "http://forgebase")], &ping).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let r = post(&server, Some(&token), &[("Origin", "https://evil.example")], &ping).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (403, "origin_not_allowed"),
        "{}",
        r.text()
    );

    server.shutdown().await.unwrap();
}

/// The parsed answer of a tool that answers from the catalog.
async fn ok_answer(server: &RunningServer, token: &str, name: &str, args: Json) -> Json {
    let (err, out) = tool(server, token, name, args).await;
    assert!(!err, "{name}: {out}");
    out
}

/// An audit entry's `detail`, which may be stored as text.
fn detail(e: &Json) -> Json {
    match &e["detail"] {
        Json::String(s) => serde_json::from_str(s).unwrap_or(Json::Null),
        other => other.clone(),
    }
}

#[tokio::test]
async fn a_scoped_key_needs_the_mcp_scope_and_keeps_its_other_limits() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path(), true);
    let server = start(&cfg).await.unwrap();
    let admin = admin(&server, &cfg.data_dir).await;
    let sa = rest(
        &server,
        &admin,
        "POST",
        "/api/auth/service-accounts",
        Some(json!({"name": "agent", "role": "admin"})),
    )
    .await;
    assert!(sa.status < 300, "{}", sa.text());
    let sa_id = sa.json()["id"].as_str().unwrap().to_owned();
    let key = |scopes: Json| {
        let server = &server;
        let admin = admin.clone();
        let sa_id = sa_id.clone();
        async move {
            let r = rest(
                server,
                &admin,
                "POST",
                &format!("/api/auth/service-accounts/{sa_id}/keys"),
                Some(json!({"name": "k", "scopes": scopes})),
            )
            .await;
            assert!(r.status < 300, "{}", r.text());
            r.json()["key"].as_str().unwrap().to_owned()
        }
    };

    // Without `mcp`, the endpoint itself is out of scope.
    let read_only = key(json!(["collections:read"])).await;
    let r = post(
        &server,
        Some(&read_only),
        &[],
        &json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}),
    )
    .await;
    assert_eq!(r.status, 403, "{}", r.text());
    assert!(r.text().contains("insufficient_scope"), "{}", r.text());

    // With `mcp:*` and a read scope: reads work, writes are the API's 403.
    let agent = key(json!(["mcp:*", "collections:read"])).await;
    let listed = ok(&server, &agent, "list_collections", json!({})).await;
    assert!(listed.to_string().contains("users"), "{listed}");
    let (err, out) = tool(
        &server,
        &agent,
        "create_collection",
        json!({"definition": {"name": "nope", "fields": []}}),
    )
    .await;
    assert!(err && out["status"] == 403, "{out}");
    assert!(out.to_string().contains("insufficient_scope"), "{out}");

    // A tool cannot swap in another credential.
    let (err, out) = tool(
        &server,
        &agent,
        "call_operation",
        json!({"operation": "collections_list", "headers": {"Authorization": format!("Bearer {admin}")}}),
    )
    .await;
    assert!(err && out.as_str().unwrap().contains("cannot be set"), "{out}");

    // An end user reaches the endpoint but the data API still decides.
    let user = user(&server, &cfg.data_dir).await;
    assert!(request(&server, &user, "tools/list", json!({})).await["tools"].is_array());
    let (err, out) = tool(&server, &user, "list_tables", json!({})).await;
    assert!(err && out["status"] == 403, "{out}");

    server.shutdown().await.unwrap();
}
