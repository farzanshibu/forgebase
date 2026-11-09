//! `/api/v1/functions`: deploy, inspect, roll back and delete functions,
//! manage their secrets, read execution logs, and invoke HTTP functions.
//!
//! Management needs `manage_functions` (`engine:manage_functions`), reading
//! `read_functions`; invoking depends on the function's access mode
//! ([`InvokeAccess`]).
//!
//! **HTTP functions.** `ANY /api/v1/functions/{name}/invoke` runs the active
//! version with the input
//! `{"method", "path", "query": {..}, "headers": {..}, "body", "auth": {"type", "id"}, "request_id"}`
//! (credential headers `authorization`, `cookie`, `x-api-key` and
//! `x-csrf-token` are never passed in; a JSON body arrives parsed, other
//! UTF-8 as a string, binary as `body_base64`). The function's output must be
//! a JSON object `{"status": 200, "headers": {..}, "body": ..}` (all
//! optional; a string body is sent as text, anything else as JSON) or empty
//! (`204`). `Set-Cookie` and hop-by-hop headers from the function are
//! dropped.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post, put};
use axum::{Json, Router};
use base64::Engine as _;
use forgebase_api::http::{ApiJson, Query};
use forgebase_api::principal::Access;
use forgebase_api::{ApiError, ApiExtension};
use forgebase_core::ErrorCode;
use forgebase_data::AccessContext;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json_, json};
use tower_http::limit::RequestBodyLimitLayer;
use utoipa::{OpenApi, ToSchema};

use crate::FunctionService;
use crate::error::{ExecError, FunctionError};
use crate::hooks::auth_json;
use crate::model::{
    DeployRequest, Deployed, ExecutionLog, FunctionInfo, HookEvent, HookSpec, InvokeAccess, Limits, LimitsInput,
    LogLine, SecretInfo, Trigger, VersionConfig, VersionInfo,
};

type ApiResult = Result<Response, ApiError>;

/// Largest management body (deploys carry base64 modules).
const MAX_BODY: usize = 24 * 1024 * 1024;
/// Largest HTTP-function request body.
pub const MAX_INVOKE_BODY: usize = 1024 * 1024;

/// Maps an engine error to the API envelope (execution failures keep their
/// own status and code; internal errors hide their message).
pub fn api_error(e: &FunctionError) -> ApiError {
    if let FunctionError::Data(d) = e {
        return ApiError::from_error(d);
    }
    let message = if e.kind().is_client_visible() {
        e.to_string()
    } else {
        tracing::error!(error = %e, "functions request failed");
        "internal error".to_owned()
    };
    ApiError::new(e.http_status(), e.code(), message)
}

fn api(e: FunctionError) -> ApiError {
    api_error(&e)
}

/// The routes, relative to `/api/v1`, with their state applied.
pub fn router<S: Clone + Send + Sync + 'static>(svc: FunctionService) -> Router<S> {
    let manage = Router::<FunctionService>::new()
        .route("/functions", get(list).post(deploy))
        .route("/functions/{name}", get(get_one).delete(delete_one))
        .route("/functions/{name}/versions", get(versions))
        .route("/functions/{name}/rollback", post(rollback))
        .route("/functions/{name}/logs", get(logs))
        .route("/functions/{name}/secrets", get(secrets))
        .route("/functions/{name}/secrets/{key}", put(put_secret).delete(delete_secret))
        .layer(RequestBodyLimitLayer::new(MAX_BODY))
        .layer(axum::extract::DefaultBodyLimit::max(MAX_BODY));
    let invoke = Router::<FunctionService>::new()
        .route("/functions/{name}/invoke", any(invoke))
        .layer(RequestBodyLimitLayer::new(MAX_INVOKE_BODY));
    manage.merge(invoke).with_state(svc)
}

/// The routes and OpenAPI fragment, ready for `RouterOptions::extensions`.
pub fn extension(svc: FunctionService) -> ApiExtension {
    ApiExtension {
        routes: router(svc),
        openapi: openapi(),
    }
}

/// OpenAPI paths and schemas of the functions API.
pub fn openapi() -> utoipa::openapi::OpenApi {
    FunctionsDoc::openapi()
}

#[derive(OpenApi)]
#[openapi(
    paths(list, deploy, get_one, delete_one, versions, rollback, logs, secrets, put_secret, delete_secret, invoke),
    components(schemas(
        DeployRequest, Deployed, FunctionInfo, VersionInfo, VersionConfig, Limits, LimitsInput, HookSpec, HookEvent,
        InvokeAccess, ExecutionLog, LogLine, SecretInfo, FunctionList, VersionList, ExecutionList, SecretList,
        RollbackBody, SecretBody,
    )),
    tags((name = "functions", description = "Sandboxed WebAssembly server functions: deploy, versions and rollback, secrets, logs, HTTP invocation")),
)]
struct FunctionsDoc;

/// Functions.
#[derive(Serialize, ToSchema)]
pub struct FunctionList {
    /// By name.
    pub items: Vec<FunctionInfo>,
}

/// Versions.
#[derive(Serialize, ToSchema)]
pub struct VersionList {
    /// Newest first.
    pub items: Vec<VersionInfo>,
}

/// Execution logs.
#[derive(Serialize, ToSchema)]
pub struct ExecutionList {
    /// Newest first.
    pub items: Vec<ExecutionLog>,
}

/// Secrets (names only).
#[derive(Serialize, ToSchema)]
pub struct SecretList {
    /// By name.
    pub items: Vec<SecretInfo>,
}

/// Body of a rollback.
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RollbackBody {
    /// Version to activate (default: the newest version before the active one).
    #[serde(default)]
    pub version: Option<i64>,
}

/// Body of a secret.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SecretBody {
    /// The value (sealed at rest, never returned).
    pub value: String,
}

/// Authorises first, then surfaces body errors.
fn body<T>(
    s: &FunctionService,
    ctx: &AccessContext,
    action: forgebase_data::Action,
    b: Result<ApiJson<T>, ApiError>,
) -> Result<T, ApiError> {
    s.data()
        .authorize_action(ctx, action)
        .map_err(|e| ApiError::from_error(&e))?;
    Ok(b?.0)
}

/// Lists functions.
#[utoipa::path(get, path = "/api/v1/functions", tag = "functions",
    responses((status = 200, description = "Functions", body = FunctionList)))]
async fn list(State(s): State<FunctionService>, Access(ctx): Access) -> ApiResult {
    Ok(Json(FunctionList {
        items: s.list(&ctx).map_err(api)?,
    })
    .into_response())
}

/// Deploys a new version (creates the function on its first deploy).
#[utoipa::path(post, path = "/api/v1/functions", tag = "functions",
    request_body = DeployRequest,
    responses(
        (status = 201, description = "Deployed", body = Deployed),
        (status = 400, description = "Invalid module (`invalid_module`), capability not granted for an import (`capability_not_granted`), invalid limits"),
    ))]
async fn deploy(
    State(s): State<FunctionService>,
    Access(ctx): Access,
    req: Result<ApiJson<DeployRequest>, ApiError>,
) -> ApiResult {
    let req = body(&s, &ctx, forgebase_data::Action::ManageFunctions, req)?;
    let out = s.deploy(&ctx, req).await.map_err(api)?;
    Ok((StatusCode::CREATED, Json(out)).into_response())
}

/// One function.
#[utoipa::path(get, path = "/api/v1/functions/{name}", tag = "functions",
    params(("name" = String, Path, description = "Function name")),
    responses((status = 200, description = "The function", body = FunctionInfo)))]
async fn get_one(State(s): State<FunctionService>, Access(ctx): Access, Path(name): Path<String>) -> ApiResult {
    Ok(Json(s.get(&ctx, &name).map_err(api)?).into_response())
}

/// Deletes a function, its versions, secrets and logs.
#[utoipa::path(delete, path = "/api/v1/functions/{name}", tag = "functions",
    params(("name" = String, Path, description = "Function name")),
    responses((status = 204, description = "Deleted")))]
async fn delete_one(State(s): State<FunctionService>, Access(ctx): Access, Path(name): Path<String>) -> ApiResult {
    s.delete(&ctx, &name).await.map_err(api)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// A function's versions, newest first.
#[utoipa::path(get, path = "/api/v1/functions/{name}/versions", operation_id = "functions_versions", tag = "functions",
    params(("name" = String, Path, description = "Function name")),
    responses((status = 200, description = "Versions", body = VersionList)))]
async fn versions(State(s): State<FunctionService>, Access(ctx): Access, Path(name): Path<String>) -> ApiResult {
    Ok(Json(VersionList {
        items: s.versions(&ctx, &name).await.map_err(api)?,
    })
    .into_response())
}

/// Activates an older (or any) version.
#[utoipa::path(post, path = "/api/v1/functions/{name}/rollback", tag = "functions",
    params(("name" = String, Path, description = "Function name")),
    request_body = RollbackBody,
    responses(
        (status = 200, description = "The function with its new active version", body = FunctionInfo),
        (status = 400, description = "No earlier version (`no_previous_version`)"),
        (status = 404, description = "No such version"),
    ))]
async fn rollback(
    State(s): State<FunctionService>,
    Access(ctx): Access,
    Path(name): Path<String>,
    b: Bytes,
) -> ApiResult {
    s.data()
        .authorize_action(&ctx, forgebase_data::Action::ManageFunctions)
        .map_err(|e| ApiError::from_error(&e))?;
    let rb: RollbackBody = if b.is_empty() {
        RollbackBody::default()
    } else {
        serde_json::from_slice(&b).map_err(|e| ApiError::bad_request("invalid_body", e.to_string()))?
    };
    Ok(Json(s.rollback(&ctx, &name, rb.version).await.map_err(api)?).into_response())
}

/// Recent executions.
#[utoipa::path(get, path = "/api/v1/functions/{name}/logs", tag = "functions",
    params(
        ("name" = String, Path, description = "Function name"),
        ("limit" = Option<u32>, Query, description = "At most this many (default 50, at most 500)"),
    ),
    responses((status = 200, description = "Executions, newest first", body = ExecutionList)))]
async fn logs(State(s): State<FunctionService>, Access(ctx): Access, Path(name): Path<String>, q: Query) -> ApiResult {
    let limit = q.u32("limit")?.unwrap_or(50);
    Ok(Json(ExecutionList {
        items: s.logs(&ctx, &name, limit).await.map_err(api)?,
    })
    .into_response())
}

/// Secret names.
#[utoipa::path(get, path = "/api/v1/functions/{name}/secrets", tag = "functions",
    params(("name" = String, Path, description = "Function name")),
    responses((status = 200, description = "Secrets (names only)", body = SecretList)))]
async fn secrets(State(s): State<FunctionService>, Access(ctx): Access, Path(name): Path<String>) -> ApiResult {
    Ok(Json(SecretList {
        items: s.secrets(&ctx, &name).await.map_err(api)?,
    })
    .into_response())
}

/// Sets a secret.
#[utoipa::path(put, path = "/api/v1/functions/{name}/secrets/{key}", tag = "functions",
    params(("name" = String, Path, description = "Function name"), ("key" = String, Path, description = "Secret name")),
    request_body = SecretBody,
    responses((status = 200, description = "Stored (sealed)", body = SecretInfo)))]
async fn put_secret(
    State(s): State<FunctionService>,
    Access(ctx): Access,
    Path((name, key)): Path<(String, String)>,
    b: Result<ApiJson<SecretBody>, ApiError>,
) -> ApiResult {
    let b = body(&s, &ctx, forgebase_data::Action::ManageFunctions, b)?;
    Ok(Json(s.put_secret(&ctx, &name, &key, &b.value).await.map_err(api)?).into_response())
}

/// Deletes a secret.
#[utoipa::path(delete, path = "/api/v1/functions/{name}/secrets/{key}", tag = "functions",
    params(("name" = String, Path, description = "Function name"), ("key" = String, Path, description = "Secret name")),
    responses((status = 204, description = "Deleted")))]
async fn delete_secret(
    State(s): State<FunctionService>,
    Access(ctx): Access,
    Path((name, key)): Path<(String, String)>,
) -> ApiResult {
    s.delete_secret(&ctx, &name, &key).await.map_err(api)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Invokes an HTTP function (any method). See the module documentation for
/// the input and output contract.
#[utoipa::path(post, path = "/api/v1/functions/{name}/invoke", tag = "functions",
    params(("name" = String, Path, description = "Function name")),
    request_body(content = Object, description = "Any body (at most 1 MiB); passed to the function"),
    responses(
        (status = 200, description = "Whatever the function returns"),
        (status = 401, description = "The function is not public and the caller is anonymous"),
        (status = 429, description = "At the function's concurrency limit (`function_busy`)"),
        (status = 502, description = "The function trapped, ran out of fuel or memory, or returned an invalid response"),
        (status = 504, description = "The function exceeded its time limit (`function_timeout`)"),
    ))]
async fn invoke(
    State(s): State<FunctionService>,
    Access(ctx): Access,
    Path(name): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult {
    let req = HttpInvocation {
        method: method.to_string(),
        path: uri.path().to_owned(),
        query: uri.query().unwrap_or_default().to_owned(),
        headers,
        body: body.to_vec(),
    };
    s.invoke_http(&ctx, &name, req).await.map_err(api)
}

/// An HTTP request to a function.
#[derive(Debug, Clone)]
pub struct HttpInvocation {
    /// Method.
    pub method: String,
    /// Path.
    pub path: String,
    /// Raw query string.
    pub query: String,
    /// Headers.
    pub headers: HeaderMap,
    /// Body.
    pub body: Vec<u8>,
}

/// Headers never given to functions.
const HIDDEN_REQUEST_HEADERS: &[&str] = &[
    "authorization",
    "cookie",
    forgebase_auth::http::API_KEY_HEADER,
    forgebase_auth::http::CSRF_HEADER,
    "proxy-authorization",
];

/// Response headers a function may not set.
const FORBIDDEN_RESPONSE_HEADERS: &[&str] = &[
    "set-cookie",
    "connection",
    "content-length",
    "transfer-encoding",
    "keep-alive",
    "upgrade",
    "te",
    "trailer",
    "proxy-authenticate",
    "x-request-id",
];

fn input_of(ctx: &AccessContext, req: &HttpInvocation) -> Json_ {
    let mut headers = Map::new();
    for (k, v) in &req.headers {
        let k = k.as_str();
        if HIDDEN_REQUEST_HEADERS.contains(&k) {
            continue;
        }
        if let Ok(v) = v.to_str() {
            headers
                .entry(k.to_owned())
                .or_insert_with(|| Json_::String(v.to_owned()));
        }
    }
    let query: Map<String, Json_> = url::form_urlencoded::parse(req.query.as_bytes())
        .map(|(k, v)| (k.into_owned(), Json_::String(v.into_owned())))
        .collect();
    let mut input = json!({
        "trigger": "http",
        "method": req.method,
        "path": req.path,
        "query": query,
        "headers": headers,
        "auth": auth_json(&ctx.principal),
        "request_id": ctx.request_id,
        "body": Json_::Null,
    });
    if !req.body.is_empty() {
        let is_json = req
            .headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| {
                let ct = ct.split(';').next().unwrap_or_default().trim().to_ascii_lowercase();
                ct == "application/json" || ct.ends_with("+json")
            });
        match (
            is_json,
            serde_json::from_slice::<Json_>(&req.body),
            std::str::from_utf8(&req.body),
        ) {
            (true, Ok(j), _) => input["body"] = j,
            (_, _, Ok(text)) => input["body"] = Json_::String(text.to_owned()),
            _ => input["body_base64"] = Json_::String(base64::engine::general_purpose::STANDARD.encode(&req.body)),
        }
    }
    input
}

/// Turns a function's output into an HTTP response.
fn response_of(output: Option<&[u8]>) -> Result<Response, ExecError> {
    let Some(bytes) = output.filter(|b| !b.is_empty()) else {
        return Ok(StatusCode::NO_CONTENT.into_response());
    };
    let v: Json_ = serde_json::from_slice(bytes).map_err(|_| ExecError::BadOutput("the output is not JSON".into()))?;
    let Json_::Object(mut obj) = v else {
        return Err(ExecError::BadOutput(
            "the output must be a JSON object {status, headers, body}".into(),
        ));
    };
    let status = match obj.get("status") {
        None => StatusCode::OK,
        Some(s) => s
            .as_u64()
            .and_then(|n| u16::try_from(n).ok())
            .filter(|n| (200..600).contains(n))
            .and_then(|n| StatusCode::from_u16(n).ok())
            .ok_or_else(|| ExecError::BadOutput("status must be a number from 200 to 599".into()))?,
    };
    let mut out_headers = HeaderMap::new();
    if let Some(h) = obj.get("headers") {
        let Some(h) = h.as_object() else {
            return Err(ExecError::BadOutput("headers must be an object of strings".into()));
        };
        for (k, v) in h {
            let lk = k.to_ascii_lowercase();
            if FORBIDDEN_RESPONSE_HEADERS.contains(&lk.as_str()) {
                continue;
            }
            let (Ok(name), Some(Ok(value))) = (
                HeaderName::from_bytes(lk.as_bytes()),
                v.as_str().map(HeaderValue::from_str),
            ) else {
                return Err(ExecError::BadOutput(format!("invalid response header {k:?}")));
            };
            out_headers.insert(name, value);
        }
    }
    let (body, default_type) = match obj.remove("body") {
        None | Some(Json_::Null) => (Vec::new(), None),
        Some(Json_::String(s)) => (s.into_bytes(), Some("text/plain; charset=utf-8")),
        Some(other) => (other.to_string().into_bytes(), Some("application/json")),
    };
    if let Some(t) = default_type
        && !out_headers.contains_key(header::CONTENT_TYPE)
    {
        out_headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(t));
    }
    let mut res = (status, body).into_response();
    res.headers_mut().extend(out_headers);
    Ok(res)
}

impl FunctionService {
    /// Runs an HTTP function for `ctx` (access mode checked) and builds the response.
    pub async fn invoke_http(
        &self,
        ctx: &AccessContext,
        name: &str,
        req: HttpInvocation,
    ) -> Result<Response, FunctionError> {
        let entry = self.http_entry(ctx, name)?;
        let input = input_of(ctx, &req).to_string().into_bytes();
        let mut built: Option<Result<Response, ExecError>> = None;
        self.execute(&entry, Trigger::Http, input, ctx.request_id.clone(), |out| {
            let r = response_of(out);
            let judged = r.as_ref().err().map(|e| (e.status(), e.to_string()));
            built = Some(r);
            judged
        })
        .await?;
        match built {
            Some(Ok(res)) => Ok(res),
            Some(Err(e)) => Err(e.into()),
            None => Err(FunctionError::Internal("no response was built".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outputs() {
        let r = response_of(Some(
            br#"{"status":201,"headers":{"X-A":"1","Set-Cookie":"s=1"},"body":{"ok":true}}"#,
        ))
        .unwrap();
        assert_eq!(r.status(), StatusCode::CREATED);
        assert_eq!(r.headers().get("x-a").unwrap(), "1");
        assert!(r.headers().get("set-cookie").is_none());
        assert_eq!(r.headers().get("content-type").unwrap(), "application/json");
        assert_eq!(response_of(None).unwrap().status(), StatusCode::NO_CONTENT);
        assert!(response_of(Some(b"nope")).is_err());
        assert!(response_of(Some(b"[1]")).is_err());
        assert!(response_of(Some(br#"{"status":99}"#)).is_err());
        assert!(response_of(Some(br#"{"headers":{"x":"a\nb"}}"#)).is_err());
    }
}
