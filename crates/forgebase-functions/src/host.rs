//! Capability operations behind the `forgebase` host functions.
//!
//! Every data operation runs as the function's own identity
//! (`service:function.<name>`, not an administrator) through the data core,
//! so the installed [`forgebase_data::Authorizer`] (the policy engine)
//! decides what the function may read and write, exactly as for any other
//! service account. Key-value data lives in the function's own namespace
//! `fn.<name>`.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use base64::Engine as _;
use forgebase_core::ErrorCode;
use forgebase_data::{AccessContext, DataError, DataService, GetOptions, ListQuery, UpdateOptions};
use forgebase_kv::{KvService, Reply};
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::fetch::{FetchRequest, Fetcher};
use crate::model::{LogLine, VersionConfig};
use crate::runtime::StoreData;

/// Lines kept per execution.
pub const MAX_LOG_LINES: usize = 200;
/// Bytes of log text kept per execution.
pub const MAX_LOG_BYTES: usize = 64 * 1024;

/// What one execution may use.
#[derive(Debug)]
pub(crate) struct HostEnv {
    pub(crate) data: DataService,
    pub(crate) kv: Option<KvService>,
    pub(crate) fetcher: Option<Fetcher>,
    /// The function's identity.
    pub(crate) identity: AccessContext,
    pub(crate) function: String,
    pub(crate) config: Arc<VersionConfig>,
    /// Decrypted secrets (empty without the `secrets` capability).
    pub(crate) secrets: HashMap<String, String>,
    /// Who JavaScript `ctx` services run as: the caller (HTTP), the writer
    /// (before-hooks), else the function's identity (after-hooks, jobs).
    pub(crate) caller: AccessContext,
    /// The function's privileged principal for `ctx.admin.*` (`service_role`).
    pub(crate) admin: AccessContext,
    /// Engines behind `ctx`.
    pub(crate) services: Arc<crate::services::Services>,
    /// Application key-value namespaces the function may reach.
    pub(crate) kv_rules: Arc<Vec<crate::model::KvRule>>,
}

/// A capability operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Op {
    DbGet,
    DbList,
    DbCreate,
    DbUpdate,
    DbDelete,
    Kv,
    EnvGet,
    SecretGet,
    Fetch,
}

impl Op {
    pub(crate) fn capability(self) -> &'static str {
        match self {
            Op::DbGet | Op::DbList => "db.read",
            Op::DbCreate | Op::DbUpdate | Op::DbDelete => "db.write",
            Op::Kv => "kv",
            Op::EnvGet => "env",
            Op::SecretGet => "secrets",
            Op::Fetch => "fetch",
        }
    }
}

/// An error handed back to the function as `{"code", "message"}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HostError {
    code: String,
    message: String,
}

impl HostError {
    pub(crate) fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    pub(crate) fn to_json(&self) -> Vec<u8> {
        json!({"code": self.code, "message": self.message})
            .to_string()
            .into_bytes()
    }
}

impl From<DataError> for HostError {
    fn from(e: DataError) -> Self {
        let message = if e.kind().is_client_visible() {
            e.to_string()
        } else {
            "internal error".to_owned()
        };
        HostError::new(e.code(), message)
    }
}

type HostResult = Result<Vec<u8>, HostError>;

fn parse<T: for<'de> Deserialize<'de>>(args: &[u8]) -> Result<T, HostError> {
    serde_json::from_slice(args).map_err(|e| HostError::new("bad_argument", format!("invalid JSON argument: {e}")))
}

fn to_bytes(v: &impl serde::Serialize) -> HostResult {
    serde_json::to_vec(v).map_err(|e| HostError::new("internal_error", e.to_string()))
}

/// Appends a log line within the per-execution caps.
pub(crate) fn push_log(d: &mut StoreData, level: &str, message: &str) {
    if d.logs.len() >= MAX_LOG_LINES || d.log_bytes >= MAX_LOG_BYTES {
        return;
    }
    let mut msg = message.to_owned();
    let room = MAX_LOG_BYTES - d.log_bytes;
    if msg.len() > room {
        let mut end = room;
        while !msg.is_char_boundary(end) {
            end -= 1;
        }
        msg.truncate(end);
    }
    d.log_bytes += msg.len();
    if d.logs.len() + 1 == MAX_LOG_LINES || d.log_bytes >= MAX_LOG_BYTES {
        msg.push_str(" [further log output dropped]");
    }
    d.logs.push(LogLine {
        level: level.to_owned(),
        message: msg,
    });
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GetArgs {
    collection: String,
    id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    collection: String,
    #[serde(default)]
    filter: Option<String>,
    #[serde(default)]
    params: HashMap<String, Json>,
    #[serde(default)]
    sort: Option<String>,
    #[serde(default)]
    page: Option<u32>,
    #[serde(default)]
    per_page: Option<u32>,
    #[serde(default)]
    cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateArgs {
    collection: String,
    data: Json,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateArgs {
    collection: String,
    id: String,
    data: Json,
    #[serde(default)]
    expected_version: Option<i64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KvArgs {
    op: String,
    key: String,
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    ttl_ms: Option<u64>,
    #[serde(default)]
    by: Option<i64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FetchArgs {
    url: String,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    body_base64: Option<String>,
}

fn reply_json(r: Reply) -> Json {
    match r {
        Reply::Nil => Json::Null,
        Reply::Status(s) => Json::String(s),
        Reply::Int(i) => json!(i),
        Reply::Bulk(b) => Json::String(String::from_utf8_lossy(&b).into_owned()),
        Reply::Array(a) => Json::Array(a.into_iter().map(reply_json).collect()),
    }
}

/// The key-value namespace of a function.
pub fn kv_namespace(function: &str) -> String {
    format!("fn.{function}")
}

/// Runs one operation for the function.
pub(crate) async fn run(env: &HostEnv, op: Op, args: Vec<u8>) -> HostResult {
    let ctx = &env.identity;
    match op {
        Op::DbGet => {
            let a: GetArgs = parse(&args)?;
            let rec = env
                .data
                .get_record(ctx, &a.collection, &a.id, &GetOptions::default())
                .await?;
            to_bytes(&rec)
        }
        Op::DbList => {
            let a: ListArgs = parse(&args)?;
            let q = ListQuery {
                filter: a.filter,
                params: a.params,
                sort: a.sort,
                page: a.page,
                per_page: a.per_page,
                cursor: a.cursor,
                ..ListQuery::default()
            };
            let res = env.data.list_records(ctx, &a.collection, &q).await?;
            to_bytes(&res)
        }
        Op::DbCreate => {
            let a: CreateArgs = parse(&args)?;
            let rec = env.data.create_record(ctx, &a.collection, a.data).await?;
            to_bytes(&rec)
        }
        Op::DbUpdate => {
            let a: UpdateArgs = parse(&args)?;
            let opts = UpdateOptions {
                expected_version: a.expected_version,
                replace: false,
            };
            let rec = env.data.update_record(ctx, &a.collection, &a.id, a.data, opts).await?;
            to_bytes(&rec)
        }
        Op::DbDelete => {
            let a: GetArgs = parse(&args)?;
            env.data.delete_record(ctx, &a.collection, &a.id, None).await?;
            Ok(b"{}".to_vec())
        }
        Op::Kv => kv(env, &args),
        Op::EnvGet => {
            let key = String::from_utf8_lossy(&args);
            env.config
                .env
                .get(key.as_ref())
                .map(|v| v.clone().into_bytes())
                .ok_or_else(|| HostError::new("not_found", format!("no environment value {key:?}")))
        }
        Op::SecretGet => {
            let key = String::from_utf8_lossy(&args);
            env.secrets
                .get(key.as_ref())
                .map(|v| v.clone().into_bytes())
                .ok_or_else(|| HostError::new("not_found", format!("no secret {key:?}")))
        }
        Op::Fetch => fetch(env, &args).await,
    }
}

fn kv(env: &HostEnv, args: &[u8]) -> HostResult {
    let Some(kv) = &env.kv else {
        return Err(HostError::new("kv_unavailable", "the key-value engine is not running"));
    };
    let a: KvArgs = parse(args)?;
    let key = a.key.into_bytes();
    let cmd: Vec<Vec<u8>> = match a.op.as_str() {
        "get" => vec![b"GET".to_vec(), key],
        "delete" => vec![b"DEL".to_vec(), key],
        "incr" => vec![b"INCRBY".to_vec(), key, a.by.unwrap_or(1).to_string().into_bytes()],
        "set" => {
            let value = a
                .value
                .ok_or_else(|| HostError::new("bad_argument", "set needs a value"))?;
            let mut c = vec![b"SET".to_vec(), key, value.into_bytes()];
            if let Some(ttl) = a.ttl_ms {
                c.push(b"PX".to_vec());
                c.push(ttl.to_string().into_bytes());
            }
            c
        }
        other => {
            return Err(HostError::new(
                "bad_argument",
                format!("unknown kv op {other:?} (get, set, delete, incr)"),
            ));
        }
    };
    // The function's own `fn.<name>` namespace is its private data, granted
    // by the `kv` capability alone: FB-138 refuses it to non-privileged
    // callers, so it is reached as the system context in the caller's tenant
    // (as `ctx.kv` does in JavaScript).
    let mut sys = AccessContext::system();
    if let Some(t) = &env.caller.tenant {
        sys = sys.with_tenant(t.clone());
    }
    let reply = kv
        .exec(&sys, &kv_namespace(&env.function), &cmd)
        .map_err(|e| HostError::new(e.code(), e.to_string()))?;
    to_bytes(&json!({ "value": reply_json(reply) }))
}

async fn fetch(env: &HostEnv, args: &[u8]) -> HostResult {
    let Some(fetcher) = &env.fetcher else {
        return Err(HostError::new(
            "fetch_unavailable",
            "outbound requests are not configured on this server",
        ));
    };
    let a: FetchArgs = parse(args)?;
    let body = match (a.body, a.body_base64) {
        (Some(_), Some(_)) => return Err(HostError::new("bad_argument", "send body or body_base64, not both")),
        (Some(b), None) => b.into_bytes(),
        (None, Some(b)) => base64::engine::general_purpose::STANDARD
            .decode(b)
            .map_err(|_| HostError::new("bad_argument", "body_base64 is not base64"))?,
        (None, None) => Vec::new(),
    };
    let req = FetchRequest {
        method: a.method.unwrap_or_else(|| "GET".into()),
        url: a.url,
        headers: a.headers.into_iter().collect(),
        body,
    };
    let res = fetcher
        .fetch(req, &env.config.fetch_hosts)
        .await
        .map_err(|e| HostError::new(e.code(), e.to_string()))?;
    let mut headers = serde_json::Map::new();
    for (k, v) in res.headers {
        headers.entry(k).or_insert(Json::String(v));
    }
    let mut out = json!({"status": res.status, "headers": headers, "url": res.url});
    match String::from_utf8(res.body) {
        Ok(s) => out["body"] = Json::String(s),
        Err(e) => out["body_base64"] = Json::String(base64::engine::general_purpose::STANDARD.encode(e.into_bytes())),
    }
    to_bytes(&out)
}
