//! REST API under `/api/v1/kv` and its OpenAPI fragment.
//!
//! Binary values: JSON strings are UTF-8 text. A value that is not valid UTF-8
//! is written and read as `{"base64": "..."}`. Everything else is the
//! command layer: `POST /{ns}/exec` runs any supported command given as an
//! array (`["SET","k","v"]`), `POST /{ns}/batch` runs several without any
//! other command in between.

use std::convert::Infallible;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use forgebase_api::http::{ApiJson, Query};
use forgebase_api::{Access, ApiError};
use forgebase_core::ErrorCode;
use futures_util::stream::{self, Stream};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Val, json};
use tokio::sync::broadcast::error::RecvError;
use tower_http::limit::RequestBodyLimitLayer;
use utoipa::{OpenApi, ToSchema};

use crate::error::KvError;
use crate::reply::Reply;
use crate::service::{KvService, KvStats, NamespaceInfo};

type ApiResult<T> = Result<T, ApiError>;

/// A binary-safe value: a string, or `{"base64": "..."}` for bytes that are
/// not UTF-8.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
#[serde(untagged)]
pub enum Bin {
    /// UTF-8 text.
    Text(String),
    /// Base64 (standard alphabet) bytes.
    Base64 {
        /// The encoded bytes.
        base64: String,
    },
}

impl Bin {
    fn bytes(self) -> ApiResult<Vec<u8>> {
        match self {
            Bin::Text(s) => Ok(s.into_bytes()),
            Bin::Base64 { base64 } => STANDARD
                .decode(base64)
                .map_err(|_| ApiError::bad_request("kv_invalid_base64", "value is not valid base64")),
        }
    }

    fn of(b: Vec<u8>) -> Val {
        match String::from_utf8(b) {
            Ok(s) => Val::String(s),
            Err(e) => json!({ "base64": STANDARD.encode(e.into_bytes()) }),
        }
    }
}

fn bytes_all(v: Vec<Bin>) -> ApiResult<Vec<Vec<u8>>> {
    v.into_iter().map(Bin::bytes).collect()
}

fn reply_json(r: Reply) -> Val {
    match r {
        Reply::Nil => Val::Null,
        Reply::Status(s) => Val::String(s),
        Reply::Int(i) => json!(i),
        Reply::Bulk(b) => Bin::of(b),
        Reply::Array(a) => Val::Array(a.into_iter().map(reply_json).collect()),
    }
}

/// A command under construction.
struct Cmd(Vec<Vec<u8>>);

impl Cmd {
    fn new(name: &str) -> Self {
        Cmd(vec![name.as_bytes().to_vec()])
    }

    fn arg(mut self, a: impl AsRef<[u8]>) -> Self {
        self.0.push(a.as_ref().to_vec());
        self
    }

    fn args(mut self, a: impl IntoIterator<Item = Vec<u8>>) -> Self {
        self.0.extend(a);
        self
    }
}

fn int_of(r: &Reply) -> i64 {
    match r {
        Reply::Int(i) => *i,
        _ => 0,
    }
}

fn run(svc: &KvService, ctx: &forgebase_data::AccessContext, ns: &str, cmd: Cmd) -> ApiResult<Reply> {
    Ok(svc.exec(ctx, ns, &cmd.0)?)
}

fn key_not_found() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "kv_key_not_found", "no such key")
}

// ---- request / response bodies ---------------------------------------------------

/// Body of `PUT .../strings/{key}`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct SetBody {
    /// The value.
    value: Bin,
    /// Expire after this many milliseconds.
    ttl_ms: Option<u64>,
    /// Only set when the key does not exist.
    #[serde(default)]
    nx: bool,
    /// Only set when the key exists.
    #[serde(default)]
    xx: bool,
    /// Keep the key's current TTL.
    #[serde(default)]
    keepttl: bool,
}

/// Body of `POST .../strings/{key}/incr`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct IncrBody {
    /// Amount to add (default 1; negative to decrement).
    by: Option<i64>,
}

/// Body of `POST /mget`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct MgetBody {
    /// Keys to read.
    keys: Vec<String>,
}

/// One entry of `POST /mset`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct MsetEntry {
    /// Key.
    key: String,
    /// Value.
    value: Bin,
}

/// Body of `POST /mset`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct MsetBody {
    /// Entries to write together.
    entries: Vec<MsetEntry>,
}

/// Body of `PUT .../keys/{key}/ttl`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct TtlBody {
    /// Milliseconds from now until the key expires (positive).
    ttl_ms: u64,
}

/// Body of `POST .../hashes/{key}/set`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct HashSetBody {
    /// Field names and values.
    fields: std::collections::BTreeMap<String, Bin>,
}

/// Body of `POST .../hashes/{key}/delete`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct HashDeleteBody {
    /// Field names to remove.
    fields: Vec<String>,
}

/// Body of `POST .../hashes/{key}/incr`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct HashIncrBody {
    /// Field to change.
    field: String,
    /// Amount to add (default 1).
    by: Option<i64>,
}

/// Which end of a list.
#[derive(Debug, Clone, Copy, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    /// The head.
    Left,
    /// The tail.
    Right,
}

/// Body of `POST .../lists/{key}/push`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct PushBody {
    /// Head or tail.
    side: Side,
    /// Values, pushed one after the other.
    values: Vec<Bin>,
}

/// Body of `POST .../lists/{key}/pop`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct PopBody {
    /// Head or tail.
    side: Side,
    /// How many to remove (default 1).
    count: Option<u64>,
}

/// Body of `POST .../lists/{key}/remove` (`LREM`).
#[derive(Debug, Deserialize, ToSchema)]
pub struct ListRemoveBody {
    /// Value to remove.
    value: Bin,
    /// `0` (default) removes every match; `n > 0` the first `n` from the head; `n < 0` the first `|n|` from the tail.
    #[serde(default)]
    count: Option<i64>,
}

/// Body of `POST .../lists/{key}/set` (`LSET`).
#[derive(Debug, Deserialize, ToSchema)]
pub struct ListSetBody {
    /// Index (negative counts from the tail).
    index: i64,
    /// New value.
    value: Bin,
}

/// Body of `POST .../streams/{key}/delete` (`XDEL`).
#[derive(Debug, Deserialize, ToSchema)]
pub struct StreamDeleteBody {
    /// Entry ids (`<ms>-<seq>`).
    ids: Vec<String>,
}

/// Body of set add/remove and sorted-set remove.
#[derive(Debug, Deserialize, ToSchema)]
pub struct MembersBody {
    /// Members.
    members: Vec<Bin>,
}

/// One scored member.
#[derive(Debug, Deserialize, ToSchema)]
pub struct ScoredMember {
    /// Member.
    member: Bin,
    /// Score (a finite number, or the strings are not accepted: use `exec` for infinities).
    score: f64,
}

/// Body of `POST .../zsets/{key}/add`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct ZAddBody {
    /// Members to add or update.
    members: Vec<ScoredMember>,
}

/// Body of `POST .../streams/{key}/add`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct StreamAddBody {
    /// Entry id (`ms-seq`); generated when absent.
    id: Option<String>,
    /// Field names and values (sent in name order).
    fields: std::collections::BTreeMap<String, Bin>,
    /// Keep at most this many entries.
    maxlen: Option<u64>,
}

/// Body of `POST /exec`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct ExecBody {
    /// Command name followed by arguments, e.g. `["INCRBY", "hits", "5"]`.
    command: Vec<Bin>,
}

/// Body of `POST /batch`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct BatchBody {
    /// Commands run in order with no other command in between.
    commands: Vec<Vec<Bin>>,
}

/// Body of `POST /publish/{channel}`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct PublishBody {
    /// The message.
    message: Bin,
}

/// Answer of `GET .../keys/{key}`.
#[derive(Debug, Serialize, ToSchema)]
pub struct KeyValue {
    /// The key.
    key: String,
    /// `string`, `hash`, `list`, `set`, `zset` or `stream`.
    #[serde(rename = "type")]
    kind: String,
    /// Milliseconds until expiry; absent when the key does not expire.
    ttl_ms: Option<i64>,
    /// Length: bytes, fields, items, members or entries.
    size: i64,
    /// Whether `value` was cut to `limit` items.
    truncated: bool,
    /// The value: string; `{field: value}`; `[items]`; `[{member, score}]`; `[{id, fields}]`.
    #[schema(value_type = Object)]
    value: Val,
}

/// Answer of `GET /{ns}/keys`.
#[derive(Debug, Serialize, ToSchema)]
pub struct ScanResponse {
    /// Pass back as `cursor` for the next page; `"0"` when done.
    cursor: String,
    /// Keys of this page.
    keys: Vec<String>,
}

// ---- admin ------------------------------------------------------------------------

/// Engine statistics: memory, hit rate, evictions, unsaved keys.
#[utoipa::path(
    get, path = "/api/v1/kv/_info", tag = "kv",
    responses((status = 200, description = "Statistics (administrators)", body = KvStats))
)]
async fn info(State(svc): State<KvService>, Access(ctx): Access) -> ApiResult<Json<KvStats>> {
    Ok(Json(svc.stats(&ctx)?))
}

/// Namespaces of the caller's tenant that hold keys.
#[utoipa::path(
    get, path = "/api/v1/kv/_namespaces", tag = "kv",
    responses((status = 200, description = "`{namespaces: [{name, keys}]}`", body = Object))
)]
async fn namespaces(State(svc): State<KvService>, Access(ctx): Access) -> ApiResult<Json<Val>> {
    let list: Vec<NamespaceInfo> = svc.namespaces(&ctx)?;
    Ok(Json(json!({ "namespaces": list })))
}

/// Deletes every key of a namespace.
#[utoipa::path(
    delete, path = "/api/v1/kv/{ns}", tag = "kv",
    params(("ns" = String, Path, description = "Namespace")),
    responses((status = 200, description = "`{deleted: n}`", body = Object))
)]
async fn flush_ns(State(svc): State<KvService>, Access(ctx): Access, Path(ns): Path<String>) -> ApiResult<Json<Val>> {
    let n = int_of(&run(&svc, &ctx, &ns, Cmd::new("FLUSHDB"))?);
    Ok(Json(json!({ "deleted": n })))
}

// ---- keys -------------------------------------------------------------------------

/// Lists keys with a cursor (`SCAN`): `cursor`, `match` (glob), `count` (1-1000), `type`.
#[utoipa::path(
    get, path = "/api/v1/kv/{ns}/keys", tag = "kv",
    description = "Query: `cursor` (start with 0), `match` glob pattern, `count`, `type`. Pages are by key order and tolerate concurrent changes.",
    params(("ns" = String, Path, description = "Namespace")),
    responses((status = 200, description = "A page of keys", body = ScanResponse))
)]
async fn scan(
    State(svc): State<KvService>,
    Access(ctx): Access,
    Path(ns): Path<String>,
    q: Query,
) -> ApiResult<Json<ScanResponse>> {
    let mut cmd = Cmd::new("SCAN").arg(q.get("cursor").unwrap_or("0"));
    if let Some(m) = q.get("match") {
        cmd = cmd.arg("MATCH").arg(m);
    }
    if let Some(c) = q.get("count") {
        cmd = cmd.arg("COUNT").arg(c);
    }
    if let Some(t) = q.get("type") {
        cmd = cmd.arg("TYPE").arg(t);
    }
    let Reply::Array(mut parts) = run(&svc, &ctx, &ns, cmd)? else {
        return Err(ApiError::internal());
    };
    let keys = match parts.pop() {
        Some(Reply::Array(k)) => k,
        _ => Vec::new(),
    };
    let cursor = match parts.pop() {
        Some(Reply::Bulk(c)) => String::from_utf8_lossy(&c).into_owned(),
        _ => "0".to_owned(),
    };
    Ok(Json(ScanResponse {
        cursor,
        keys: keys
            .into_iter()
            .filter_map(|k| match k {
                Reply::Bulk(b) => String::from_utf8(b).ok(),
                _ => None,
            })
            .collect(),
    }))
}

fn pairs_to_object(items: Vec<Reply>, limit: usize) -> (Val, bool) {
    let mut m = Map::new();
    let truncated = items.len() / 2 > limit;
    let mut it = items.into_iter();
    while let (Some(Reply::Bulk(k)), Some(v)) = (it.next(), it.next()) {
        if m.len() >= limit {
            break;
        }
        m.insert(String::from_utf8_lossy(&k).into_owned(), reply_json(v));
    }
    (Val::Object(m), truncated)
}

/// Type, TTL, size and the whole value of a key (up to `limit` items, default 1000).
#[utoipa::path(
    get, path = "/api/v1/kv/{ns}/keys/{key}", tag = "kv",
    description = "Query: `limit` caps the items returned for collections (default 1000, at most 10000).",
    params(("ns" = String, Path, description = "Namespace"), ("key" = String, Path, description = "Key")),
    responses((status = 200, description = "The key", body = KeyValue), (status = 404, description = "No such key"))
)]
async fn get_key(
    State(svc): State<KvService>,
    Access(ctx): Access,
    Path((ns, key)): Path<(String, String)>,
    q: Query,
) -> ApiResult<Json<KeyValue>> {
    let limit = q.u32("limit")?.unwrap_or(1000).clamp(1, 10_000) as usize;
    let kind = match run(&svc, &ctx, &ns, Cmd::new("TYPE").arg(&key))? {
        Reply::Status(t) if t != "none" => t,
        _ => return Err(key_not_found()),
    };
    let last = (limit - 1).to_string();
    let (read, size) = match kind.as_str() {
        "string" => (Cmd::new("GET").arg(&key), Cmd::new("STRLEN").arg(&key)),
        "hash" => (Cmd::new("HGETALL").arg(&key), Cmd::new("HLEN").arg(&key)),
        "list" => (
            Cmd::new("LRANGE").arg(&key).arg("0").arg(&last),
            Cmd::new("LLEN").arg(&key),
        ),
        "set" => (Cmd::new("SMEMBERS").arg(&key), Cmd::new("SCARD").arg(&key)),
        "zset" => (
            Cmd::new("ZRANGE").arg(&key).arg("0").arg(&last).arg("WITHSCORES"),
            Cmd::new("ZCARD").arg(&key),
        ),
        _ => (
            Cmd::new("XRANGE")
                .arg(&key)
                .arg("-")
                .arg("+")
                .arg("COUNT")
                .arg(limit.to_string()),
            Cmd::new("XLEN").arg(&key),
        ),
    };
    let cmds = vec![Cmd::new("PTTL").arg(&key).0, read.0, size.0];
    let mut res = svc.exec_batch(&ctx, &ns, &cmds)?.into_iter();
    let (Some(ttl), Some(read), Some(size)) = (res.next(), res.next(), res.next()) else {
        return Err(ApiError::internal());
    };
    let (ttl, read, size) = (ttl?, read, int_of(&size?));
    if matches!(ttl, Reply::Int(-2)) {
        return Err(key_not_found());
    }
    let read = match read {
        Ok(r) => r,
        Err(KvError::WrongType) => {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "kv_wrong_type",
                "the key changed type while it was read; try again",
            ));
        }
        Err(e) => return Err(e.into()),
    };
    let (value, truncated) = match (kind.as_str(), read) {
        ("hash", Reply::Array(items)) => pairs_to_object(items, limit),
        ("set", Reply::Array(items)) => {
            let t = items.len() > limit;
            (Val::Array(items.into_iter().take(limit).map(reply_json).collect()), t)
        }
        ("zset", Reply::Array(items)) => {
            let mut out = Vec::new();
            let mut it = items.into_iter();
            while let (Some(m), Some(Reply::Bulk(sc))) = (it.next(), it.next()) {
                let score = String::from_utf8_lossy(&sc).parse::<f64>().ok();
                out.push(json!({ "member": reply_json(m), "score": score }));
            }
            let t = size > i64::try_from(out.len()).unwrap_or(i64::MAX);
            (Val::Array(out), t)
        }
        ("stream", Reply::Array(entries)) => {
            let out: Vec<Val> = entries.into_iter().map(entry_json).collect();
            let t = size > i64::try_from(out.len()).unwrap_or(i64::MAX);
            (Val::Array(out), t)
        }
        ("list", r) => {
            let v = reply_json(r);
            let t = size > i64::try_from(v.as_array().map_or(0, Vec::len)).unwrap_or(i64::MAX);
            (v, t)
        }
        (_, r) => (reply_json(r), false),
    };
    Ok(Json(KeyValue {
        key,
        kind,
        ttl_ms: match ttl {
            Reply::Int(t) if t >= 0 => Some(t),
            _ => None,
        },
        size,
        truncated,
        value,
    }))
}

fn entry_json(e: Reply) -> Val {
    let Reply::Array(mut p) = e else { return Val::Null };
    let fields = match p.pop() {
        Some(Reply::Array(f)) => pairs_to_object(f, usize::MAX).0,
        _ => json!({}),
    };
    let id = match p.pop() {
        Some(r) => reply_json(r),
        None => Val::Null,
    };
    json!({ "id": id, "fields": fields })
}

/// Deletes a key of any type.
#[utoipa::path(
    delete, path = "/api/v1/kv/{ns}/keys/{key}", tag = "kv",
    params(("ns" = String, Path, description = "Namespace"), ("key" = String, Path, description = "Key")),
    responses((status = 200, description = "`{deleted: bool}`", body = Object))
)]
async fn delete_key(
    State(svc): State<KvService>,
    Access(ctx): Access,
    Path((ns, key)): Path<(String, String)>,
) -> ApiResult<Json<Val>> {
    let n = int_of(&run(&svc, &ctx, &ns, Cmd::new("DEL").arg(&key))?);
    Ok(Json(json!({ "deleted": n > 0 })))
}

/// Sets the time to live of a key.
#[utoipa::path(
    put, path = "/api/v1/kv/{ns}/keys/{key}/ttl", tag = "kv",
    params(("ns" = String, Path, description = "Namespace"), ("key" = String, Path, description = "Key")),
    request_body = TtlBody,
    responses((status = 200, description = "`{applied: true}`", body = Object), (status = 404, description = "No such key"))
)]
async fn set_ttl(
    State(svc): State<KvService>,
    Access(ctx): Access,
    Path((ns, key)): Path<(String, String)>,
    ApiJson(b): ApiJson<TtlBody>,
) -> ApiResult<Json<Val>> {
    if b.ttl_ms == 0 {
        return Err(ApiError::bad_request("kv_syntax", "ttl_ms must be positive"));
    }
    let n = int_of(&run(
        &svc,
        &ctx,
        &ns,
        Cmd::new("PEXPIRE").arg(&key).arg(b.ttl_ms.to_string()),
    )?);
    if n == 0 {
        return Err(key_not_found());
    }
    Ok(Json(json!({ "applied": true })))
}

/// Removes the time to live: the key no longer expires.
#[utoipa::path(
    delete, path = "/api/v1/kv/{ns}/keys/{key}/ttl", tag = "kv",
    params(("ns" = String, Path, description = "Namespace"), ("key" = String, Path, description = "Key")),
    responses((status = 200, description = "`{persisted: bool}` (false when the key had no TTL)", body = Object), (status = 404, description = "No such key"))
)]
async fn clear_ttl(
    State(svc): State<KvService>,
    Access(ctx): Access,
    Path((ns, key)): Path<(String, String)>,
) -> ApiResult<Json<Val>> {
    let ttl = int_of(&run(&svc, &ctx, &ns, Cmd::new("PTTL").arg(&key))?);
    if ttl == -2 {
        return Err(key_not_found());
    }
    let n = int_of(&run(&svc, &ctx, &ns, Cmd::new("PERSIST").arg(&key))?);
    Ok(Json(json!({ "persisted": n > 0 })))
}

// ---- strings ------------------------------------------------------------------------

/// Sets a string (`SET`): optional TTL, only-if-new, only-if-exists, keep-TTL.
#[utoipa::path(
    put, path = "/api/v1/kv/{ns}/strings/{key}", tag = "kv",
    params(("ns" = String, Path, description = "Namespace"), ("key" = String, Path, description = "Key")),
    request_body = SetBody,
    responses((status = 200, description = "`{stored: bool}` (false when `nx`/`xx` prevented it)", body = Object))
)]
async fn set_string(
    State(svc): State<KvService>,
    Access(ctx): Access,
    Path((ns, key)): Path<(String, String)>,
    ApiJson(b): ApiJson<SetBody>,
) -> ApiResult<Json<Val>> {
    let mut cmd = Cmd::new("SET").arg(&key).arg(b.value.bytes()?);
    if let Some(t) = b.ttl_ms {
        cmd = cmd.arg("PX").arg(t.to_string());
    }
    for (on, flag) in [(b.nx, "NX"), (b.xx, "XX"), (b.keepttl, "KEEPTTL")] {
        if on {
            cmd = cmd.arg(flag);
        }
    }
    let r = run(&svc, &ctx, &ns, cmd)?;
    Ok(Json(json!({ "stored": !matches!(r, Reply::Nil) })))
}

/// Adds to the integer in a string (created as 0 when missing).
#[utoipa::path(
    post, path = "/api/v1/kv/{ns}/strings/{key}/incr", tag = "kv",
    params(("ns" = String, Path, description = "Namespace"), ("key" = String, Path, description = "Key")),
    request_body = IncrBody,
    responses((status = 200, description = "`{value: n}`", body = Object))
)]
async fn incr(
    State(svc): State<KvService>,
    Access(ctx): Access,
    Path((ns, key)): Path<(String, String)>,
    ApiJson(b): ApiJson<IncrBody>,
) -> ApiResult<Json<Val>> {
    let cmd = Cmd::new("INCRBY").arg(&key).arg(b.by.unwrap_or(1).to_string());
    Ok(Json(json!({ "value": int_of(&run(&svc, &ctx, &ns, cmd)?) })))
}

/// Reads several strings at once; missing keys (or keys of another type) are `null`.
#[utoipa::path(
    post, path = "/api/v1/kv/{ns}/mget", tag = "kv",
    params(("ns" = String, Path, description = "Namespace")),
    request_body = MgetBody,
    responses((status = 200, description = "`{values: [...]}` in key order", body = Object))
)]
async fn mget(
    State(svc): State<KvService>,
    Access(ctx): Access,
    Path(ns): Path<String>,
    ApiJson(b): ApiJson<MgetBody>,
) -> ApiResult<Json<Val>> {
    if b.keys.is_empty() {
        return Err(ApiError::bad_request("kv_syntax", "keys must not be empty"));
    }
    let r = run(
        &svc,
        &ctx,
        &ns,
        Cmd::new("MGET").args(b.keys.into_iter().map(String::into_bytes)),
    )?;
    Ok(Json(json!({ "values": reply_json(r) })))
}

/// Writes several strings in one step.
#[utoipa::path(
    post, path = "/api/v1/kv/{ns}/mset", tag = "kv",
    params(("ns" = String, Path, description = "Namespace")),
    request_body = MsetBody,
    responses((status = 200, description = "`{stored: n}`", body = Object))
)]
async fn mset(
    State(svc): State<KvService>,
    Access(ctx): Access,
    Path(ns): Path<String>,
    ApiJson(b): ApiJson<MsetBody>,
) -> ApiResult<Json<Val>> {
    if b.entries.is_empty() {
        return Err(ApiError::bad_request("kv_syntax", "entries must not be empty"));
    }
    let n = b.entries.len();
    let mut cmd = Cmd::new("MSET");
    for e in b.entries {
        cmd = cmd.arg(e.key).arg(e.value.bytes()?);
    }
    run(&svc, &ctx, &ns, cmd)?;
    Ok(Json(json!({ "stored": n })))
}

// ---- hashes, lists, sets, sorted sets, streams -------------------------------------

/// Sets hash fields.
#[utoipa::path(
    post, path = "/api/v1/kv/{ns}/hashes/{key}/set", tag = "kv",
    params(("ns" = String, Path, description = "Namespace"), ("key" = String, Path, description = "Key")),
    request_body = HashSetBody,
    responses((status = 200, description = "`{added: n}` (new fields)", body = Object))
)]
async fn hash_set(
    State(svc): State<KvService>,
    Access(ctx): Access,
    Path((ns, key)): Path<(String, String)>,
    ApiJson(b): ApiJson<HashSetBody>,
) -> ApiResult<Json<Val>> {
    if b.fields.is_empty() {
        return Err(ApiError::bad_request("kv_syntax", "fields must not be empty"));
    }
    let mut cmd = Cmd::new("HSET").arg(&key);
    for (f, v) in b.fields {
        cmd = cmd.arg(f).arg(v.bytes()?);
    }
    Ok(Json(json!({ "added": int_of(&run(&svc, &ctx, &ns, cmd)?) })))
}

/// Removes hash fields.
#[utoipa::path(
    post, path = "/api/v1/kv/{ns}/hashes/{key}/delete", tag = "kv",
    params(("ns" = String, Path, description = "Namespace"), ("key" = String, Path, description = "Key")),
    request_body = HashDeleteBody,
    responses((status = 200, description = "`{removed: n}`", body = Object))
)]
async fn hash_delete(
    State(svc): State<KvService>,
    Access(ctx): Access,
    Path((ns, key)): Path<(String, String)>,
    ApiJson(b): ApiJson<HashDeleteBody>,
) -> ApiResult<Json<Val>> {
    if b.fields.is_empty() {
        return Err(ApiError::bad_request("kv_syntax", "fields must not be empty"));
    }
    let cmd = Cmd::new("HDEL")
        .arg(&key)
        .args(b.fields.into_iter().map(String::into_bytes));
    Ok(Json(json!({ "removed": int_of(&run(&svc, &ctx, &ns, cmd)?) })))
}

/// Adds to the integer in a hash field.
#[utoipa::path(
    post, path = "/api/v1/kv/{ns}/hashes/{key}/incr", tag = "kv",
    params(("ns" = String, Path, description = "Namespace"), ("key" = String, Path, description = "Key")),
    request_body = HashIncrBody,
    responses((status = 200, description = "`{value: n}`", body = Object))
)]
async fn hash_incr(
    State(svc): State<KvService>,
    Access(ctx): Access,
    Path((ns, key)): Path<(String, String)>,
    ApiJson(b): ApiJson<HashIncrBody>,
) -> ApiResult<Json<Val>> {
    let cmd = Cmd::new("HINCRBY")
        .arg(&key)
        .arg(b.field)
        .arg(b.by.unwrap_or(1).to_string());
    Ok(Json(json!({ "value": int_of(&run(&svc, &ctx, &ns, cmd)?) })))
}

/// Pushes values onto a list.
#[utoipa::path(
    post, path = "/api/v1/kv/{ns}/lists/{key}/push", tag = "kv",
    params(("ns" = String, Path, description = "Namespace"), ("key" = String, Path, description = "Key")),
    request_body = PushBody,
    responses((status = 200, description = "`{length: n}`", body = Object))
)]
async fn list_push(
    State(svc): State<KvService>,
    Access(ctx): Access,
    Path((ns, key)): Path<(String, String)>,
    ApiJson(b): ApiJson<PushBody>,
) -> ApiResult<Json<Val>> {
    if b.values.is_empty() {
        return Err(ApiError::bad_request("kv_syntax", "values must not be empty"));
    }
    let name = if matches!(b.side, Side::Left) { "LPUSH" } else { "RPUSH" };
    let cmd = Cmd::new(name).arg(&key).args(bytes_all(b.values)?);
    Ok(Json(json!({ "length": int_of(&run(&svc, &ctx, &ns, cmd)?) })))
}

/// Pops values from a list.
#[utoipa::path(
    post, path = "/api/v1/kv/{ns}/lists/{key}/pop", tag = "kv",
    params(("ns" = String, Path, description = "Namespace"), ("key" = String, Path, description = "Key")),
    request_body = PopBody,
    responses((status = 200, description = "`{values: [...]}` (empty when the list is empty)", body = Object))
)]
async fn list_pop(
    State(svc): State<KvService>,
    Access(ctx): Access,
    Path((ns, key)): Path<(String, String)>,
    ApiJson(b): ApiJson<PopBody>,
) -> ApiResult<Json<Val>> {
    let name = if matches!(b.side, Side::Left) { "LPOP" } else { "RPOP" };
    let cmd = Cmd::new(name).arg(&key).arg(b.count.unwrap_or(1).to_string());
    let v = match reply_json(run(&svc, &ctx, &ns, cmd)?) {
        Val::Null => json!([]),
        v => v,
    };
    Ok(Json(json!({ "values": v })))
}

/// Removes occurrences of a value from a list (`LREM`).
#[utoipa::path(
    post, path = "/api/v1/kv/{ns}/lists/{key}/remove", operation_id = "kv_list_remove", tag = "kv",
    params(("ns" = String, Path, description = "Namespace"), ("key" = String, Path, description = "Key")),
    request_body = ListRemoveBody,
    responses((status = 200, description = "`{removed: n}`", body = Object))
)]
async fn list_remove(
    State(svc): State<KvService>,
    Access(ctx): Access,
    Path((ns, key)): Path<(String, String)>,
    ApiJson(b): ApiJson<ListRemoveBody>,
) -> ApiResult<Json<Val>> {
    let cmd = Cmd::new("LREM")
        .arg(&key)
        .arg(b.count.unwrap_or(0).to_string())
        .arg(b.value.bytes()?);
    Ok(Json(json!({ "removed": int_of(&run(&svc, &ctx, &ns, cmd)?) })))
}

/// Replaces the list element at an index (`LSET`); `400` when out of range.
#[utoipa::path(
    post, path = "/api/v1/kv/{ns}/lists/{key}/set", operation_id = "kv_list_set", tag = "kv",
    params(("ns" = String, Path, description = "Namespace"), ("key" = String, Path, description = "Key")),
    request_body = ListSetBody,
    responses((status = 200, description = "`{ok: true}`", body = Object))
)]
async fn list_set(
    State(svc): State<KvService>,
    Access(ctx): Access,
    Path((ns, key)): Path<(String, String)>,
    ApiJson(b): ApiJson<ListSetBody>,
) -> ApiResult<Json<Val>> {
    let cmd = Cmd::new("LSET")
        .arg(&key)
        .arg(b.index.to_string())
        .arg(b.value.bytes()?);
    run(&svc, &ctx, &ns, cmd)?;
    Ok(Json(json!({ "ok": true })))
}

/// Adds members to a set.
#[utoipa::path(
    post, path = "/api/v1/kv/{ns}/sets/{key}/add", tag = "kv",
    params(("ns" = String, Path, description = "Namespace"), ("key" = String, Path, description = "Key")),
    request_body = MembersBody,
    responses((status = 200, description = "`{added: n}` (new members)", body = Object))
)]
async fn set_add(
    State(svc): State<KvService>,
    Access(ctx): Access,
    Path((ns, key)): Path<(String, String)>,
    ApiJson(b): ApiJson<MembersBody>,
) -> ApiResult<Json<Val>> {
    if b.members.is_empty() {
        return Err(ApiError::bad_request("kv_syntax", "members must not be empty"));
    }
    let cmd = Cmd::new("SADD").arg(&key).args(bytes_all(b.members)?);
    Ok(Json(json!({ "added": int_of(&run(&svc, &ctx, &ns, cmd)?) })))
}

/// Removes members from a set.
#[utoipa::path(
    post, path = "/api/v1/kv/{ns}/sets/{key}/remove", tag = "kv",
    params(("ns" = String, Path, description = "Namespace"), ("key" = String, Path, description = "Key")),
    request_body = MembersBody,
    responses((status = 200, description = "`{removed: n}`", body = Object))
)]
async fn set_remove(
    State(svc): State<KvService>,
    Access(ctx): Access,
    Path((ns, key)): Path<(String, String)>,
    ApiJson(b): ApiJson<MembersBody>,
) -> ApiResult<Json<Val>> {
    if b.members.is_empty() {
        return Err(ApiError::bad_request("kv_syntax", "members must not be empty"));
    }
    let cmd = Cmd::new("SREM").arg(&key).args(bytes_all(b.members)?);
    Ok(Json(json!({ "removed": int_of(&run(&svc, &ctx, &ns, cmd)?) })))
}

/// Adds or re-scores members of a sorted set.
#[utoipa::path(
    post, path = "/api/v1/kv/{ns}/zsets/{key}/add", tag = "kv",
    params(("ns" = String, Path, description = "Namespace"), ("key" = String, Path, description = "Key")),
    request_body = ZAddBody,
    responses((status = 200, description = "`{added: n}` (new members)", body = Object))
)]
async fn zset_add(
    State(svc): State<KvService>,
    Access(ctx): Access,
    Path((ns, key)): Path<(String, String)>,
    ApiJson(b): ApiJson<ZAddBody>,
) -> ApiResult<Json<Val>> {
    if b.members.is_empty() {
        return Err(ApiError::bad_request("kv_syntax", "members must not be empty"));
    }
    let mut cmd = Cmd::new("ZADD").arg(&key);
    for m in b.members {
        if !m.score.is_finite() {
            return Err(ApiError::bad_request("kv_syntax", "score must be a finite number"));
        }
        cmd = cmd.arg(m.score.to_string()).arg(m.member.bytes()?);
    }
    Ok(Json(json!({ "added": int_of(&run(&svc, &ctx, &ns, cmd)?) })))
}

/// Removes members from a sorted set.
#[utoipa::path(
    post, path = "/api/v1/kv/{ns}/zsets/{key}/remove", tag = "kv",
    params(("ns" = String, Path, description = "Namespace"), ("key" = String, Path, description = "Key")),
    request_body = MembersBody,
    responses((status = 200, description = "`{removed: n}`", body = Object))
)]
async fn zset_remove(
    State(svc): State<KvService>,
    Access(ctx): Access,
    Path((ns, key)): Path<(String, String)>,
    ApiJson(b): ApiJson<MembersBody>,
) -> ApiResult<Json<Val>> {
    if b.members.is_empty() {
        return Err(ApiError::bad_request("kv_syntax", "members must not be empty"));
    }
    let cmd = Cmd::new("ZREM").arg(&key).args(bytes_all(b.members)?);
    Ok(Json(json!({ "removed": int_of(&run(&svc, &ctx, &ns, cmd)?) })))
}

/// Appends an entry to a stream.
#[utoipa::path(
    post, path = "/api/v1/kv/{ns}/streams/{key}/add", tag = "kv",
    params(("ns" = String, Path, description = "Namespace"), ("key" = String, Path, description = "Key")),
    request_body = StreamAddBody,
    responses((status = 201, description = "`{id}` of the new entry", body = Object))
)]
async fn stream_add(
    State(svc): State<KvService>,
    Access(ctx): Access,
    Path((ns, key)): Path<(String, String)>,
    ApiJson(b): ApiJson<StreamAddBody>,
) -> ApiResult<Response> {
    if b.fields.is_empty() {
        return Err(ApiError::bad_request("kv_syntax", "fields must not be empty"));
    }
    let mut cmd = Cmd::new("XADD").arg(&key);
    if let Some(m) = b.maxlen {
        cmd = cmd.arg("MAXLEN").arg(m.to_string());
    }
    cmd = cmd.arg(b.id.as_deref().unwrap_or("*"));
    for (f, v) in b.fields {
        cmd = cmd.arg(f).arg(v.bytes()?);
    }
    let id = reply_json(run(&svc, &ctx, &ns, cmd)?);
    Ok((StatusCode::CREATED, Json(json!({ "id": id }))).into_response())
}

/// Deletes entries from a stream by id (`XDEL`).
#[utoipa::path(
    post, path = "/api/v1/kv/{ns}/streams/{key}/delete", operation_id = "kv_stream_delete", tag = "kv",
    params(("ns" = String, Path, description = "Namespace"), ("key" = String, Path, description = "Key")),
    request_body = StreamDeleteBody,
    responses((status = 200, description = "`{removed: n}`", body = Object))
)]
async fn stream_delete(
    State(svc): State<KvService>,
    Access(ctx): Access,
    Path((ns, key)): Path<(String, String)>,
    ApiJson(b): ApiJson<StreamDeleteBody>,
) -> ApiResult<Json<Val>> {
    if b.ids.is_empty() {
        return Err(ApiError::bad_request("kv_syntax", "ids must not be empty"));
    }
    let cmd = Cmd::new("XDEL")
        .arg(&key)
        .args(b.ids.into_iter().map(String::into_bytes));
    Ok(Json(json!({ "removed": int_of(&run(&svc, &ctx, &ns, cmd)?) })))
}

/// Entries of a stream between two ids (`-` and `+` are the ends); `rev=true` newest first.
#[utoipa::path(
    get, path = "/api/v1/kv/{ns}/streams/{key}/range", tag = "kv",
    description = "Query: `start` (default `-`), `end` (default `+`), `count` (default 100), `rev`.",
    params(("ns" = String, Path, description = "Namespace"), ("key" = String, Path, description = "Key")),
    responses((status = 200, description = "`{entries: [{id, fields}]}`", body = Object))
)]
async fn stream_range(
    State(svc): State<KvService>,
    Access(ctx): Access,
    Path((ns, key)): Path<(String, String)>,
    q: Query,
) -> ApiResult<Json<Val>> {
    let (start, end) = (q.get("start").unwrap_or("-"), q.get("end").unwrap_or("+"));
    let count = q.u32("count")?.unwrap_or(100).to_string();
    let cmd = if q.bool("rev")? {
        Cmd::new("XREVRANGE").arg(&key).arg(end).arg(start)
    } else {
        Cmd::new("XRANGE").arg(&key).arg(start).arg(end)
    };
    let r = run(&svc, &ctx, &ns, cmd.arg("COUNT").arg(count))?;
    Ok(Json(json!({ "entries": entries_of(r) })))
}

fn entries_of(r: Reply) -> Val {
    match r {
        Reply::Array(es) => Val::Array(es.into_iter().map(entry_json).collect()),
        _ => json!([]),
    }
}

/// Entries after an id (`after`, default `0-0`; `$` = only newer than now), without blocking.
#[utoipa::path(
    get, path = "/api/v1/kv/{ns}/streams/{key}/read", tag = "kv",
    description = "Query: `after` (default `0-0`), `count` (default 100). Does not wait for new entries; poll with the last id, or use pub/sub.",
    params(("ns" = String, Path, description = "Namespace"), ("key" = String, Path, description = "Key")),
    responses((status = 200, description = "`{entries: [{id, fields}]}`", body = Object))
)]
async fn stream_read(
    State(svc): State<KvService>,
    Access(ctx): Access,
    Path((ns, key)): Path<(String, String)>,
    q: Query,
) -> ApiResult<Json<Val>> {
    let count = q.u32("count")?.unwrap_or(100).to_string();
    let cmd = Cmd::new("XREAD")
        .arg("COUNT")
        .arg(count)
        .arg("STREAMS")
        .arg(&key)
        .arg(q.get("after").unwrap_or("0-0"));
    let entries = match run(&svc, &ctx, &ns, cmd)? {
        Reply::Array(streams) => streams
            .into_iter()
            .next()
            .and_then(|s| match s {
                Reply::Array(mut p) => p.pop(),
                _ => None,
            })
            .map_or_else(|| json!([]), entries_of),
        _ => json!([]),
    };
    Ok(Json(json!({ "entries": entries })))
}

// ---- generic commands, pub/sub -----------------------------------------------------

fn command_of(c: Vec<Bin>) -> ApiResult<Vec<Vec<u8>>> {
    if c.is_empty() {
        return Err(ApiError::bad_request("kv_syntax", "empty command"));
    }
    bytes_all(c)
}

/// Runs one command given as an array, e.g. `["ZRANGE","board","0","-1","WITHSCORES"]`.
#[utoipa::path(
    post, path = "/api/v1/kv/{ns}/exec", tag = "kv",
    params(("ns" = String, Path, description = "Namespace")),
    request_body = ExecBody,
    responses((status = 200, description = "`{result}`: null, string, number, value or array", body = Object))
)]
async fn exec(
    State(svc): State<KvService>,
    Access(ctx): Access,
    Path(ns): Path<String>,
    ApiJson(b): ApiJson<ExecBody>,
) -> ApiResult<Json<Val>> {
    let r = svc.exec(&ctx, &ns, &command_of(b.command)?)?;
    Ok(Json(json!({ "result": reply_json(r) })))
}

/// Runs several commands in order with no other command in between. A failing
/// command reports its error and the rest still run; earlier ones are not undone.
#[utoipa::path(
    post, path = "/api/v1/kv/{ns}/batch", operation_id = "kv_batch", tag = "kv",
    params(("ns" = String, Path, description = "Namespace")),
    request_body = BatchBody,
    responses((status = 200, description = "`{results: [{result} | {error: {code, message}}]}`", body = Object))
)]
async fn batch(
    State(svc): State<KvService>,
    Access(ctx): Access,
    Path(ns): Path<String>,
    ApiJson(b): ApiJson<BatchBody>,
) -> ApiResult<Json<Val>> {
    if b.commands.is_empty() || b.commands.len() > 1000 {
        return Err(ApiError::bad_request("kv_syntax", "send 1 to 1000 commands"));
    }
    let cmds = b.commands.into_iter().map(command_of).collect::<ApiResult<Vec<_>>>()?;
    let results: Vec<Val> = svc
        .exec_batch(&ctx, &ns, &cmds)?
        .into_iter()
        .map(|r| match r {
            Ok(r) => json!({ "result": reply_json(r) }),
            Err(e) => json!({ "error": { "code": e.code(), "message": e.to_string() } }),
        })
        .collect();
    Ok(Json(json!({ "results": results })))
}

/// Publishes a message to the channel's current subscribers.
#[utoipa::path(
    post, path = "/api/v1/kv/{ns}/publish/{channel}", operation_id = "kv_publish", tag = "kv",
    params(("ns" = String, Path, description = "Namespace"), ("channel" = String, Path, description = "Channel")),
    request_body = PublishBody,
    responses((status = 200, description = "`{receivers: n}`", body = Object))
)]
async fn publish(
    State(svc): State<KvService>,
    Access(ctx): Access,
    Path((ns, channel)): Path<(String, String)>,
    ApiJson(b): ApiJson<PublishBody>,
) -> ApiResult<Json<Val>> {
    let cmd = Cmd::new("PUBLISH").arg(&channel).arg(b.message.bytes()?);
    Ok(Json(json!({ "receivers": int_of(&run(&svc, &ctx, &ns, cmd)?) })))
}

/// Live pub/sub channels of a namespace (those with at least one subscriber).
#[utoipa::path(
    get, path = "/api/v1/kv/{ns}/channels", operation_id = "kv_channels", tag = "kv",
    params(("ns" = String, Path, description = "Namespace")),
    responses((status = 200, description = "`{channels: [{name, subscribers}]}`", body = Object))
)]
async fn channels(State(svc): State<KvService>, Access(ctx): Access, Path(ns): Path<String>) -> ApiResult<Json<Val>> {
    let list = svc.channels(&ctx, &ns)?;
    Ok(Json(json!({ "channels": list })))
}

/// Subscribes to a channel as server-sent events: `message` events carry
/// `{channel, message}`; a `lagged` event says how many messages a slow
/// reader missed. Messages are not stored: only subscribers connected at
/// publish time receive them.
#[utoipa::path(
    get, path = "/api/v1/kv/{ns}/subscribe/{channel}", tag = "kv",
    params(("ns" = String, Path, description = "Namespace"), ("channel" = String, Path, description = "Channel")),
    responses((status = 200, description = "`text/event-stream`", content_type = "text/event-stream", body = String))
)]
async fn subscribe(
    State(svc): State<KvService>,
    Access(ctx): Access,
    Path((ns, channel)): Path<(String, String)>,
) -> ApiResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    let rx = svc.subscribe(&ctx, &ns, &channel)?;
    let events = stream::unfold(rx, |mut rx| async move {
        match rx.recv().await {
            Ok(m) => {
                let ev = Event::default()
                    .event("message")
                    .json_data(json!({ "channel": m.channel, "message": Bin::of(m.payload) }))
                    .unwrap_or_else(|_| Event::default().event("error"));
                Some((Ok(ev), rx))
            }
            Err(RecvError::Lagged(n)) => Some((Ok(Event::default().event("lagged").data(n.to_string())), rx)),
            Err(RecvError::Closed) => None,
        }
    });
    Ok(Sse::new(events).keep_alive(KeepAlive::default()))
}

// ---- wiring -------------------------------------------------------------------------

/// The `/api/v1/kv` routes. Mount with `forgebase_api::router_with`.
pub fn router<S: Clone + Send + Sync + 'static>(svc: KvService, max_body_bytes: usize) -> Router<S> {
    Router::new()
        .route("/api/v1/kv/_info", get(info))
        .route("/api/v1/kv/_namespaces", get(namespaces))
        .route("/api/v1/kv/{ns}", delete(flush_ns))
        .route("/api/v1/kv/{ns}/keys", get(scan))
        .route("/api/v1/kv/{ns}/keys/{key}", get(get_key).delete(delete_key))
        .route("/api/v1/kv/{ns}/keys/{key}/ttl", put(set_ttl).delete(clear_ttl))
        .route("/api/v1/kv/{ns}/strings/{key}", put(set_string))
        .route("/api/v1/kv/{ns}/strings/{key}/incr", post(incr))
        .route("/api/v1/kv/{ns}/mget", post(mget))
        .route("/api/v1/kv/{ns}/mset", post(mset))
        .route("/api/v1/kv/{ns}/hashes/{key}/set", post(hash_set))
        .route("/api/v1/kv/{ns}/hashes/{key}/delete", post(hash_delete))
        .route("/api/v1/kv/{ns}/hashes/{key}/incr", post(hash_incr))
        .route("/api/v1/kv/{ns}/lists/{key}/push", post(list_push))
        .route("/api/v1/kv/{ns}/lists/{key}/pop", post(list_pop))
        .route("/api/v1/kv/{ns}/lists/{key}/remove", post(list_remove))
        .route("/api/v1/kv/{ns}/lists/{key}/set", post(list_set))
        .route("/api/v1/kv/{ns}/streams/{key}/delete", post(stream_delete))
        .route("/api/v1/kv/{ns}/channels", get(channels))
        .route("/api/v1/kv/{ns}/sets/{key}/add", post(set_add))
        .route("/api/v1/kv/{ns}/sets/{key}/remove", post(set_remove))
        .route("/api/v1/kv/{ns}/zsets/{key}/add", post(zset_add))
        .route("/api/v1/kv/{ns}/zsets/{key}/remove", post(zset_remove))
        .route("/api/v1/kv/{ns}/streams/{key}/add", post(stream_add))
        .route("/api/v1/kv/{ns}/streams/{key}/range", get(stream_range))
        .route("/api/v1/kv/{ns}/streams/{key}/read", get(stream_read))
        .route("/api/v1/kv/{ns}/exec", post(exec))
        .route("/api/v1/kv/{ns}/batch", post(batch))
        .route("/api/v1/kv/{ns}/publish/{channel}", post(publish))
        .route("/api/v1/kv/{ns}/subscribe/{channel}", get(subscribe))
        .layer(RequestBodyLimitLayer::new(max_body_bytes))
        .with_state(svc)
}

#[derive(OpenApi)]
#[openapi(
    paths(
        info, namespaces, flush_ns, scan, get_key, delete_key, set_ttl, clear_ttl, set_string, incr, mget, mset,
        hash_set, hash_delete, hash_incr, list_push, list_pop, set_add, set_remove, zset_add, zset_remove,
        stream_add, stream_range, stream_read, exec, batch, publish, subscribe,
        list_remove, list_set, stream_delete, channels,
    ),
    components(schemas(
        Bin, SetBody, IncrBody, MgetBody, MsetEntry, MsetBody, TtlBody, HashSetBody, HashDeleteBody, HashIncrBody,
        Side, PushBody, PopBody, MembersBody, ScoredMember, ZAddBody, StreamAddBody, ExecBody, BatchBody,
        PublishBody, KeyValue, ScanResponse, KvStats, NamespaceInfo, ListRemoveBody, ListSetBody, StreamDeleteBody,
    )),
    tags((name = "kv", description = "Key-value engine: strings, hashes, lists, sets, sorted sets, streams, TTL, pub/sub")),
)]
struct KvDoc;

/// The OpenAPI fragment for the KV routes (paths, schemas, tag). Merge it into
/// the server's document with `forgebase_api::router_with`.
pub fn openapi() -> utoipa::openapi::OpenApi {
    KvDoc::openapi()
}
