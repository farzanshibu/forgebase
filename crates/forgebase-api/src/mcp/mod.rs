//! MCP server: `POST /api/mcp` speaks the Model Context Protocol (Streamable
//! HTTP transport, JSON responses) so an AI agent can operate and configure
//! ForgeBase (ADR-0030).
//!
//! * **Off by default.** `[mcp] enabled = true` (or `FORGEBASE_MCP_ENABLED`,
//!   `--mcp`) mounts it; otherwise the feature gate answers
//!   `404 feature_disabled`.
//! * **Authentication is the server's own.** The auth layer has already turned
//!   the request's `Authorization: Bearer` API key, personal access token or
//!   session into the caller's context; anonymous callers get `401`. A scoped
//!   credential needs the `mcp` area (`mcp:write` or `mcp:*`) to reach the
//!   endpoint at all, like any other area.
//! * **No second authorization path.** Every tool call is one HTTP request
//!   sent through this same router ([`McpState::attach`]) with the caller's
//!   credential headers, so scopes, policy rules, tenancy, feature switches,
//!   rate limits and the handlers' own checks apply exactly as over HTTP. An
//!   agent can never do more than its credential could with `curl`.
//! * **Destructive calls need `confirm: true`**, so an agent asks its user
//!   first; every call that changes something is written to the audit log
//!   (`mcp.tool_call`).
//! * **Browsers are kept out** by an `Origin` check (DNS rebinding): a request
//!   that carries `Origin` must come from this server's own origin or one in
//!   `server.cors_allowed_origins`.
//!
//! The server is stateless: no `Mcp-Session-Id`, no server-initiated stream
//! (`GET` and `DELETE` answer `405`).

pub mod catalog;
pub mod tools;

use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};

use axum::Router;
use axum::body::Body;
use axum::extract::{ConnectInfo, FromRequestParts, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use forgebase_data::{AccessContext, Principal};
use forgebase_policy::PolicyService;
use serde_json::{Map, Value as Json, json};
use tower::ServiceExt as _;

use crate::error::ApiError;
use crate::principal::Access;
use catalog::Catalog;
use tools::{Outgoing, Plan};

/// Protocol revisions this server speaks, newest first.
pub const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// Largest MCP request body.
const MAX_REQUEST_BYTES: usize = 4 << 20;
/// Largest response a tool reads from the API.
const MAX_RESPONSE_BYTES: usize = 8 << 20;
/// Largest binary body returned inline (base64).
const MAX_INLINE_BINARY: usize = 1 << 20;
/// Longest text a tool result carries.
const MAX_TEXT: usize = 200_000;

/// Request headers a tool call inherits from the MCP request: the
/// credentials, the client's address as proxies report it, and ForgeBase's
/// own `X-ForgeBase-*` headers (realm, tenant, consistency).
const INHERITED: &[&str] = &[
    "authorization",
    "x-api-key",
    "cookie",
    "x-csrf-token",
    "user-agent",
    "x-forwarded-for",
    "x-real-ip",
    "forwarded",
];

const INSTRUCTIONS: &str = "ForgeBase is a single-binary backend: database collections and records, SQL, \
file storage, edge functions, background jobs, queues, key-value, vector and graph stores, users, \
API keys, roles and rules, billing and server settings. Every tool call runs with this connection's \
credential and its permissions; a 401 or 403 means the credential may not do it. Start with whoami. \
Use the named tools for common work; for anything else use list_operations (by tag or search), then \
describe_operation, then call_operation. Deleting, revoking, rotating or rolling back needs \
confirm: true: ask the user first. Setting changes (update_config) mostly apply after a server restart.";

/// State of the MCP endpoint.
#[derive(Clone)]
pub struct McpState(Arc<Inner>);

struct Inner {
    catalog: Catalog,
    app: OnceLock<Router>,
    policy: Option<PolicyService>,
    origins: Vec<String>,
}

impl std::fmt::Debug for McpState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpState")
            .field("operations", &self.0.catalog.len())
            .field("attached", &self.0.app.get().is_some())
            .finish_non_exhaustive()
    }
}

impl McpState {
    /// The endpoint for the API documented by `doc` (what
    /// `GET /api/openapi.json` serves). `policy` records the audit trail;
    /// `origins` are the extra browser origins allowed (CORS).
    pub fn new(doc: &utoipa::openapi::OpenApi, policy: Option<PolicyService>, origins: Vec<String>) -> Self {
        let json = serde_json::to_value(doc).unwrap_or(Json::Null);
        Self(Arc::new(Inner {
            catalog: Catalog::new(&json),
            app: OnceLock::new(),
            policy,
            origins,
        }))
    }

    /// Hands over the finished router that tool calls are sent through. Until
    /// then a tool call answers "starting".
    pub fn attach(&self, app: Router) {
        let _ = self.0.app.set(app);
    }

    /// The routes: `POST /api/mcp`, and `405` for `GET` and `DELETE`.
    pub fn routes<S: Clone + Send + Sync + 'static>(self) -> Router<S> {
        Router::new()
            .route("/api/mcp", post(handle).get(no_stream).delete(no_stream))
            .with_state(self)
    }
}

/// Who is calling, as each tool request inherits it.
struct Caller {
    ctx: AccessContext,
    headers: HeaderMap,
    connect: Option<ConnectInfo<SocketAddr>>,
}

/// `GET` and `DELETE /api/mcp`: no server-initiated stream, no sessions.
async fn no_stream() -> Response {
    let mut res = ApiError::new(
        StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
        "This MCP server answers POST requests only: it opens no event stream and keeps no sessions.",
    )
    .into_response();
    res.headers_mut()
        .insert(header::ALLOW, HeaderValue::from_static("POST"));
    res
}

/// `POST /api/mcp`: one JSON-RPC message.
async fn handle(State(mcp): State<McpState>, req: Request) -> Response {
    let (mut parts, body) = req.into_parts();
    let ctx = match Access::from_request_parts(&mut parts, &()).await {
        Ok(Access(ctx)) => ctx,
        Err(e) => return e.into_response(),
    };
    if !mcp.origin_ok(&parts.headers) {
        return ApiError::new(
            StatusCode::FORBIDDEN,
            "origin_not_allowed",
            "This origin may not use the MCP endpoint. Add it to server.cors_allowed_origins to allow a browser app.",
        )
        .into_response();
    }
    if let Some(v) = parts.headers.get("mcp-protocol-version")
        && !v.to_str().is_ok_and(|v| PROTOCOL_VERSIONS.contains(&v))
    {
        return ApiError::bad_request(
            "unsupported_protocol_version",
            format!("Supported MCP protocol versions: {}.", PROTOCOL_VERSIONS.join(", ")),
        )
        .into_response();
    }
    if matches!(ctx.principal, Principal::Anonymous) {
        let mut res = ApiError::new(
            StatusCode::UNAUTHORIZED,
            "unauthenticated",
            "Send an API key or personal access token: Authorization: Bearer fbk_… or fbp_….",
        )
        .into_response();
        res.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_static("Bearer realm=\"forgebase\""),
        );
        return res;
    }
    let bytes = match axum::body::to_bytes(body, MAX_REQUEST_BYTES).await {
        Ok(b) => b,
        Err(_) => {
            return ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "payload_too_large",
                format!("An MCP message may be at most {MAX_REQUEST_BYTES} bytes."),
            )
            .into_response();
        }
    };
    let msg: Json = match serde_json::from_slice(&bytes) {
        Ok(m) => m,
        Err(_) => {
            return rpc(
                StatusCode::BAD_REQUEST,
                error(Json::Null, -32700, "Parse error: the body is not JSON"),
            );
        }
    };
    let Some(obj) = msg.as_object() else {
        let why = if msg.is_array() {
            "Batches are not supported: send one message per request"
        } else {
            "Invalid request: expected a JSON-RPC 2.0 object"
        };
        return rpc(StatusCode::BAD_REQUEST, error(Json::Null, -32600, why));
    };
    let id = obj.get("id").cloned();
    let Some(method) = obj.get("method").and_then(Json::as_str) else {
        // A response to a request of ours: there are none, so just accept it.
        return StatusCode::ACCEPTED.into_response();
    };
    let Some(id) = id.filter(|i| i.is_string() || i.is_number()) else {
        // A notification (initialized, cancelled…): nothing to answer.
        return StatusCode::ACCEPTED.into_response();
    };
    if obj.get("jsonrpc").and_then(Json::as_str) != Some("2.0") {
        return rpc(
            StatusCode::OK,
            error(id, -32600, "Invalid request: jsonrpc must be \"2.0\""),
        );
    }
    let params = obj.get("params").cloned().unwrap_or(Json::Null);
    let caller = Caller {
        headers: inherited(&parts.headers),
        connect: parts.extensions.get::<ConnectInfo<SocketAddr>>().cloned(),
        ctx,
    };
    let answer = match mcp.call(&caller, method, &params).await {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err((code, message)) => error(id, code, &message),
    };
    rpc(StatusCode::OK, answer)
}

fn rpc(status: StatusCode, body: Json) -> Response {
    let mut res = (status, axum::Json(body)).into_response();
    // Answers can carry secrets (a new API key, a config value).
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

fn error(id: Json, code: i64, message: &str) -> Json {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn inherited(headers: &HeaderMap) -> HeaderMap {
    let mut out = HeaderMap::new();
    for (name, value) in headers {
        let n = name.as_str();
        if INHERITED.contains(&n) || n.starts_with("x-forgebase-") {
            out.append(name.clone(), value.clone());
        }
    }
    out
}

/// A tool result with one text block.
fn text_result(text: String, is_error: bool) -> Json {
    json!({"content": [{"type": "text", "text": text}], "isError": is_error})
}

fn truncate(mut text: String) -> String {
    if text.len() <= MAX_TEXT {
        return text;
    }
    let total = text.len();
    let mut cut = MAX_TEXT;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    text.truncate(cut);
    text.push_str(&format!(
        "\n… cut at {MAX_TEXT} of {total} bytes. Narrow the request (filter, fields, per_page, limit) to see the rest."
    ));
    text
}

impl McpState {
    /// Whether a request may come from where its `Origin` header says.
    fn origin_ok(&self, headers: &HeaderMap) -> bool {
        let Some(origin) = headers.get(header::ORIGIN) else {
            // Not a browser: agents and CLIs send no Origin.
            return true;
        };
        let Ok(origin) = origin.to_str() else { return false };
        if self
            .0
            .origins
            .iter()
            .any(|o| o == "*" || o.eq_ignore_ascii_case(origin))
        {
            return true;
        }
        let Ok(url) = url::Url::parse(origin) else { return false };
        let Some(host) = url.host_str() else { return false };
        let origin_host = match url.port() {
            Some(p) => format!("{host}:{p}"),
            None => host.to_owned(),
        };
        headers
            .get(header::HOST)
            .and_then(|h| h.to_str().ok())
            .is_some_and(|h| h.eq_ignore_ascii_case(&origin_host))
    }

    /// Answers one JSON-RPC request: a result, or `(code, message)`.
    async fn call(&self, caller: &Caller, method: &str, params: &Json) -> Result<Json, (i64, String)> {
        match method {
            "initialize" => {
                let asked = params["protocolVersion"].as_str().unwrap_or("");
                let version = if PROTOCOL_VERSIONS.contains(&asked) {
                    asked
                } else {
                    PROTOCOL_VERSIONS[0]
                };
                Ok(json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {"name": "forgebase", "title": "ForgeBase", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": INSTRUCTIONS,
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tools::list(&self.0.catalog)})),
            "tools/call" => {
                let name = params["name"]
                    .as_str()
                    .ok_or((-32602, "Invalid params: `name` is required".to_owned()))?;
                let empty = Map::new();
                let args = match &params["arguments"] {
                    Json::Null => &empty,
                    Json::Object(m) => m,
                    _ => return Err((-32602, "Invalid params: `arguments` must be an object".to_owned())),
                };
                match tools::plan(&self.0.catalog, name, args) {
                    Ok(None) => Err((-32602, format!("Unknown tool: {name}"))),
                    Err(why) => Ok(text_result(why, true)),
                    Ok(Some(Plan::Answer(answer))) => Ok(text_result(
                        truncate(serde_json::to_string_pretty(&answer).unwrap_or_default()),
                        false,
                    )),
                    Ok(Some(Plan::Send(out))) => Ok(self.send(caller, name, out).await),
                }
            }
            _ => Err((-32601, format!("Method not found: {method}"))),
        }
    }

    /// Sends a tool's request through the router as the caller, and turns the
    /// answer into a tool result.
    async fn send(&self, caller: &Caller, tool: &str, out: Outgoing) -> Json {
        if !catalog::allowed_path(&out.path) {
            return text_result(format!("{} cannot be called over MCP.", out.path), true);
        }
        let Some(app) = self.0.app.get() else {
            return text_result("The MCP server is still starting; try again.".to_owned(), true);
        };
        let uri = if out.query.is_empty() {
            out.path.clone()
        } else {
            format!("{}?{}", out.path, out.query)
        };
        let mut headers = caller.headers.clone();
        for (k, v) in &out.headers {
            headers.insert(k.clone(), v.clone());
        }
        let body = match &out.body {
            Some((bytes, ct)) => {
                if let Ok(v) = HeaderValue::from_str(ct) {
                    headers.insert(header::CONTENT_TYPE, v);
                }
                Body::from(bytes.clone())
            }
            None => Body::empty(),
        };
        let mut req = match Request::builder().method(out.method.clone()).uri(&uri).body(body) {
            Ok(r) => r,
            Err(_) => return text_result(format!("{uri} is not a valid request path."), true),
        };
        *req.headers_mut() = headers;
        if let Some(ci) = &caller.connect {
            req.extensions_mut().insert(*ci);
        }
        let res = match app.clone().oneshot(req).await {
            Ok(r) => r,
            Err(never) => match never {},
        };
        let status = res.status();
        let res_headers = res.headers().clone();
        let bytes = axum::body::to_bytes(res.into_body(), MAX_RESPONSE_BYTES).await;
        let request_id = res_headers
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();
        tracing::info!(
            tool,
            actor = %caller.ctx.actor(),
            method = %out.method,
            path = %out.template,
            status = status.as_u16(),
            request_id = %request_id,
            "mcp tool call"
        );
        if out.method != axum::http::Method::GET
            && let Some(policy) = &self.0.policy
        {
            let detail = json!({
                "tool": tool,
                "method": out.method.as_str(),
                "operation": out.template,
                "path": out.path,
                "status": status.as_u16(),
                "request_id": request_id,
            });
            if let Err(e) = policy.audit_event(&caller.ctx, "mcp.tool_call", detail).await {
                tracing::warn!(error = %e, tool, "could not audit an MCP tool call");
            }
        }
        match bytes {
            Ok(b) => reply(status, &res_headers, &b),
            Err(_) => text_result(
                format!(
                    "HTTP {}: the answer is larger than {MAX_RESPONSE_BYTES} bytes. Narrow the request (filter, fields, limit).",
                    status.as_u16()
                ),
                true,
            ),
        }
    }
}

/// The tool result for an API answer: status, ids the agent may need next,
/// and the body as JSON, text or base64.
fn reply(status: StatusCode, headers: &HeaderMap, body: &[u8]) -> Json {
    let get = |h: &str| headers.get(h).and_then(|v| v.to_str().ok());
    let mut out = Map::new();
    out.insert("status".into(), json!(status.as_u16()));
    for (key, h) in [
        ("request_id", "x-request-id"),
        ("location", "location"),
        ("etag", "etag"),
        ("commit", "x-forgebase-commit"),
    ] {
        if let Some(v) = get(h) {
            out.insert(key.into(), json!(v));
        }
    }
    let ct = get("content-type").unwrap_or("").to_ascii_lowercase();
    if !body.is_empty() {
        let texty = ct.starts_with("text/")
            || ["xml", "javascript", "yaml", "csv", "ndjson", "x-www-form-urlencoded"]
                .iter()
                .any(|t| ct.contains(t));
        if ct.contains("json") && !ct.contains("ndjson") {
            match serde_json::from_slice::<Json>(body) {
                Ok(v) => {
                    out.insert("body".into(), v);
                }
                Err(_) => {
                    out.insert("body_text".into(), json!(String::from_utf8_lossy(body)));
                }
            }
        } else if texty || (ct.is_empty() && std::str::from_utf8(body).is_ok()) {
            out.insert("body_text".into(), json!(String::from_utf8_lossy(body)));
        } else if body.len() <= MAX_INLINE_BINARY {
            use base64::Engine as _;
            out.insert("content_type".into(), json!(ct));
            out.insert(
                "body_base64".into(),
                json!(base64::engine::general_purpose::STANDARD.encode(body)),
            );
        } else {
            out.insert("content_type".into(), json!(ct));
            out.insert("size".into(), json!(body.len()));
            out.insert(
                "note".into(),
                json!("Binary content larger than 1 MiB is not returned; create a signed URL to download it."),
            );
        }
    }
    let text = serde_json::to_string_pretty(&Json::Object(out)).unwrap_or_default();
    text_result(truncate(text), status.as_u16() >= 400)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn state(origins: &[&str]) -> McpState {
        McpState(Arc::new(Inner {
            catalog: Catalog::default(),
            app: OnceLock::new(),
            policy: None,
            origins: origins.iter().map(|s| (*s).to_owned()).collect(),
        }))
    }

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, HeaderValue::from_static(v));
        }
        h
    }

    #[test]
    fn browsers_must_come_from_this_server_or_an_allowed_origin() {
        let s = state(&["https://app.example.com"]);
        assert!(s.origin_ok(&headers(&[])), "no Origin: not a browser");
        assert!(s.origin_ok(&headers(&[
            ("origin", "http://127.0.0.1:8090"),
            ("host", "127.0.0.1:8090")
        ])));
        assert!(s.origin_ok(&headers(&[
            ("origin", "https://app.example.com"),
            ("host", "api.example.com")
        ])));
        assert!(!s.origin_ok(&headers(&[
            ("origin", "https://evil.example"),
            ("host", "127.0.0.1:8090")
        ])));
        assert!(!s.origin_ok(&headers(&[("origin", "null"), ("host", "127.0.0.1:8090")])));
        assert!(state(&["*"]).origin_ok(&headers(&[("origin", "https://any.example")])));
    }

    #[test]
    fn tool_requests_inherit_credentials_and_forgebase_headers_only() {
        let h = headers(&[
            ("authorization", "Bearer fbk_x"),
            ("x-forgebase-tenant", "env1"),
            ("x-request-id", "abc"),
            ("content-type", "application/json"),
            ("origin", "https://x"),
        ]);
        let mut names: Vec<String> = inherited(&h).keys().map(|n| n.as_str().to_owned()).collect();
        names.sort();
        assert_eq!(names, vec!["authorization", "x-forgebase-tenant"]);
    }

    #[test]
    fn replies_carry_status_ids_and_the_body_in_a_readable_form() {
        let h = headers(&[
            ("content-type", "application/json"),
            ("x-request-id", "r1"),
            ("etag", "\"v1\""),
        ]);
        let r = reply(StatusCode::CREATED, &h, br#"{"id":"a"}"#);
        assert_eq!(r["isError"], false);
        let t: Json = serde_json::from_str(r["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(
            t,
            json!({"status": 201, "request_id": "r1", "etag": "\"v1\"", "body": {"id": "a"}})
        );

        let r = reply(StatusCode::FORBIDDEN, &h, br#"{"error":{"code":"forbidden"}}"#);
        assert_eq!(r["isError"], true);

        let png = headers(&[("content-type", "image/png")]);
        let t: Json = serde_json::from_str(
            reply(StatusCode::OK, &png, &[0, 159, 146]).as_object().unwrap()["content"][0]["text"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(t["body_base64"], "AJ+S");
        let text = headers(&[("content-type", "text/plain")]);
        let t: Json = serde_json::from_str(
            reply(StatusCode::OK, &text, b"hi").as_object().unwrap()["content"][0]["text"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(t["body_text"], "hi");
    }

    #[test]
    fn long_text_is_cut_on_a_character_boundary() {
        let s = "é".repeat(MAX_TEXT);
        let t = truncate(s);
        assert!(t.contains("cut at"));
        assert!(t.len() < MAX_TEXT + 200);
    }
}
