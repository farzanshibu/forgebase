//! Host operations of the JavaScript engine (`call(op, arg)`, ADR-0027 §4).
//!
//! Every operation names its capability and runs **as the caller**
//! ([`HostEnv::caller`]): the data core, the policy engine, the vector,
//! storage, queue, job and realtime engines authorise it exactly as if the
//! caller had made the request, so collection rules, field rules, record
//! ACLs, tenancy and policies apply. With `"as": "admin"` (the code used
//! `ctx.admin.*`) an operation runs as the function's privileged service
//! principal instead; that needs the `service_role` capability.
//!
//! Key-value calls without a namespace use the function's private namespace
//! `fn.<name>` (granted by the `kv` capability alone). Application namespaces
//! need a namespace rule ([`crate::model::KvRule`], deny by default) and the
//! caller's own key-value permissions.

use std::collections::{BTreeMap, HashMap};

use base64::Engine as _;
use forgebase_core::ErrorCode;
use forgebase_data::{AccessContext, GetOptions, ListQuery, Principal, UpdateOptions};
use hmac::{Hmac, KeyInit, Mac};
use serde::Deserialize;
use serde_json::{Map, Value as Json, json};
use sha2::Digest;

use crate::host::{HostEnv, HostError, kv_namespace};
use crate::model::{KvAccess, SERVICE_ROLE};

type HostResult = Result<Vec<u8>, HostError>;

/// Largest object a function reads or writes in one call (use signed URLs beyond).
pub const MAX_OBJECT_BYTES: usize = 4 * 1024 * 1024;

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

fn err<E: ErrorCode + std::fmt::Display>(e: E) -> HostError {
    let message = if e.kind().is_client_visible() {
        e.to_string()
    } else {
        tracing::warn!(error = %e, "function host call failed");
        "internal error".to_owned()
    };
    HostError::new(e.code(), message)
}

fn bad(m: impl Into<String>) -> HostError {
    HostError::new("bad_argument", m)
}

/// `capability_denied`, saying which API needs what and how to fix it.
fn denied(cap: &str) -> HostError {
    let what = match cap {
        "db.read" => "ctx.db reads (get, list)",
        "db.write" => "ctx.db writes (create, update, delete)",
        "kv" => "ctx.kv",
        "vector.read" => "ctx.vector reads (search, get)",
        "vector.write" => "ctx.vector writes (upsert, delete)",
        "storage.read" => "ctx.storage reads (get, list, signedUrl)",
        "storage.write" => "ctx.storage writes (put, delete, upload URLs)",
        "queues.publish" => "ctx.queues.publish",
        "jobs.enqueue" => "ctx.jobs.enqueue",
        "realtime.publish" => "ctx.realtime.broadcast",
        "fetch" => "fetch()",
        other => other,
    };
    HostError::new(
        "capability_denied",
        format!("{what} needs the '{cap}' capability: add it under Capabilities and deploy again"),
    )
}

fn unavailable(service: &str) -> HostError {
    HostError::new(
        format!("{service}_unavailable"),
        format!("the {service} engine is not running on this server: enable it in the server configuration"),
    )
}

fn out(v: &impl serde::Serialize) -> HostResult {
    serde_json::to_vec(v).map_err(|e| HostError::new("internal_error", e.to_string()))
}

fn take<T: for<'de> Deserialize<'de>>(v: Json) -> Result<T, HostError> {
    serde_json::from_value(v).map_err(|e| bad(format!("invalid argument: {e}")))
}

fn str_field(o: &Map<String, Json>, k: &str) -> Result<String, HostError> {
    o.get(k)
        .and_then(Json::as_str)
        .map(str::to_owned)
        .ok_or_else(|| bad(format!("{k} must be a string")))
}

/// Keeps only `keys` of `o` that are present and not null.
fn pick(o: &Map<String, Json>, keys: &[&str]) -> Json {
    Json::Object(
        keys.iter()
            .filter_map(|k| o.get(*k).filter(|v| !v.is_null()).map(|v| ((*k).to_owned(), v.clone())))
            .collect(),
    )
}

/// The caller's identity as `ctx.auth` sees it.
pub(crate) fn auth_info(env_perms: Vec<String>, ctx: &AccessContext) -> Json {
    if ctx.principal == Principal::Anonymous {
        return json!({"type": "anonymous"});
    }
    let collection = match &ctx.principal {
        Principal::User { collection, .. } => Json::String(collection.clone()),
        _ => Json::Null,
    };
    json!({
        "type": ctx.principal.kind(),
        "id": ctx.principal.id(),
        "collection": collection,
        "roles": ctx.roles,
        "permissions": env_perms,
        "realm": ctx.realm_id,
        "org": ctx.org_id,
        "tenant": ctx.tenant,
    })
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
struct WriteArgs {
    collection: String,
    #[serde(default)]
    id: Option<String>,
    data: Json,
    #[serde(default)]
    expected_version: Option<i64>,
}

/// Runs one operation.
pub(crate) async fn run(env: &HostEnv, op: &str, args: &[u8]) -> HostResult {
    let mut v: Json = if args.is_empty() {
        Json::Object(Map::new())
    } else {
        serde_json::from_slice(args).map_err(|e| bad(format!("invalid JSON argument: {e}")))?
    };
    let Some(obj) = v.as_object_mut() else {
        return Err(bad("the argument must be a JSON object"));
    };
    let as_admin = match obj.remove("as") {
        Some(Json::String(s)) if s == "admin" => true,
        Some(Json::String(s)) if s == "caller" => false,
        None => false,
        Some(_) => return Err(bad("as must be \"caller\" or \"admin\"")),
    };
    if as_admin && !env.config.has(SERVICE_ROLE) {
        return Err(HostError::new(
            "capability_denied",
            "ctx.admin needs the 'service_role' capability, which only an administrator can grant: \
             ask an administrator to deploy this function with it",
        ));
    }
    let ctx = if as_admin { &env.admin } else { &env.caller };
    let need = |cap: &str| if env.config.has(cap) { Ok(()) } else { Err(denied(cap)) };
    match op {
        "db.get" => {
            need("db.read")?;
            let a: GetArgs = take(v)?;
            let rec = env
                .data
                .get_record(ctx, &a.collection, &a.id, &GetOptions::default())
                .await?;
            out(&rec)
        }
        "db.list" => {
            need("db.read")?;
            let a: ListArgs = take(v)?;
            let q = ListQuery {
                filter: a.filter,
                params: a.params,
                sort: a.sort,
                page: a.page,
                per_page: a.per_page,
                cursor: a.cursor,
                ..ListQuery::default()
            };
            out(&env.data.list_records(ctx, &a.collection, &q).await?)
        }
        "db.create" => {
            need("db.write")?;
            let a: WriteArgs = take(v)?;
            out(&env.data.create_record(ctx, &a.collection, a.data).await?)
        }
        "db.update" => {
            need("db.write")?;
            let a: WriteArgs = take(v)?;
            let id = a.id.ok_or_else(|| bad("update needs an id"))?;
            let opts = UpdateOptions {
                expected_version: a.expected_version,
                replace: false,
            };
            out(&env.data.update_record(ctx, &a.collection, &id, a.data, opts).await?)
        }
        "db.delete" => {
            need("db.write")?;
            let a: GetArgs = take(v)?;
            env.data.delete_record(ctx, &a.collection, &a.id, None).await?;
            Ok(b"{}".to_vec())
        }
        "kv" => {
            need("kv")?;
            kv(env, ctx, as_admin, v)
        }
        "vector.search" | "vector.get" => {
            need("vector.read")?;
            vector(env, ctx, op, v).await
        }
        "vector.upsert" | "vector.delete" => {
            need("vector.write")?;
            vector(env, ctx, op, v).await
        }
        "storage.get" | "storage.list" => {
            need("storage.read")?;
            storage(env, ctx, op, v).await
        }
        "storage.put" | "storage.delete" => {
            need("storage.write")?;
            storage(env, ctx, op, v).await
        }
        "storage.sign" => {
            let put = v
                .get("method")
                .and_then(Json::as_str)
                .is_some_and(|m| m.eq_ignore_ascii_case("PUT"));
            need(if put { "storage.write" } else { "storage.read" })?;
            storage(env, ctx, op, v).await
        }
        "queues.publish" => {
            need("queues.publish")?;
            let q = env.services.queue.as_ref().ok_or_else(|| unavailable("queue"))?;
            let o = v.as_object().ok_or_else(|| bad("expected an object"))?;
            let msg = crate::services::QueueMessage {
                topic: str_field(o, "topic")?,
                data: o.get("data").cloned().unwrap_or(Json::Null),
                ordering_key: o.get("ordering_key").and_then(Json::as_str).map(str::to_owned),
                attributes: match o.get("attributes") {
                    None | Some(Json::Null) => BTreeMap::new(),
                    Some(a) => take(a.clone())?,
                },
                dedup_id: o.get("dedup_id").and_then(Json::as_str).map(str::to_owned),
            };
            let published = q
                .publish(ctx, msg)
                .await
                .map_err(|e| HostError::new(e.code, e.message))?;
            out(&published)
        }
        "jobs.enqueue" => {
            need("jobs.enqueue")?;
            let jobs = env.services.jobs.as_ref().ok_or_else(|| unavailable("jobs"))?;
            let o = v.as_object().ok_or_else(|| bad("expected an object"))?;
            let new: forgebase_jobs::NewJob = take(pick(
                o,
                &[
                    "kind",
                    "payload",
                    "queue",
                    "priority",
                    "run_at",
                    "delay_ms",
                    "max_attempts",
                    "idempotency_key",
                    "timeout_ms",
                ],
            ))?;
            let (job, created) = jobs.enqueue(ctx, new).await.map_err(err)?;
            out(&json!({"job": job, "created": created}))
        }
        "realtime.broadcast" => {
            need("realtime.publish")?;
            let rt = env.services.realtime.as_ref().ok_or_else(|| unavailable("realtime"))?;
            let o = v.as_object().ok_or_else(|| bad("expected an object"))?;
            let channel = str_field(o, "channel")?;
            let event = str_field(o, "event")?;
            let payload = o.get("payload").cloned().unwrap_or(Json::Null);
            rt.publish(ctx, &channel, json!({"event": event, "payload": payload}))
                .await
                .map_err(err)?;
            Ok(b"{}".to_vec())
        }
        "auth.verify" => verify(env, v).await,
        "env.get" => {
            let name = v.get("name").and_then(Json::as_str).unwrap_or_default();
            let (env_ok, sec_ok) = (env.config.has("env"), env.config.has("secrets"));
            if !env_ok && !sec_ok {
                return Err(HostError::new(
                    "capability_denied",
                    "ctx.env.get needs the 'env' capability (plain values) or 'secrets' (sealed secrets): \
                     add one under Capabilities and deploy again",
                ));
            }
            let value = env_ok
                .then(|| env.config.env.get(name))
                .flatten()
                .or_else(|| sec_ok.then(|| env.secrets.get(name)).flatten());
            out(&json!({ "value": value }))
        }
        "crypto.digest" | "crypto.hmac" => crypto(op, &v),
        "fetch" => {
            need("fetch")?;
            let bytes = serde_json::to_vec(&v).map_err(|e| bad(e.to_string()))?;
            crate::host::run(env, crate::host::Op::Fetch, bytes).await
        }
        other => Err(bad(format!("unknown host operation {other:?}"))),
    }
}

fn reply_json(r: forgebase_kv::Reply) -> Json {
    use forgebase_kv::Reply;
    match r {
        Reply::Nil => Json::Null,
        Reply::Status(s) => Json::String(s),
        Reply::Int(i) => json!(i),
        Reply::Bulk(b) => Json::String(String::from_utf8_lossy(&b).into_owned()),
        Reply::Array(a) => Json::Array(a.into_iter().map(reply_json).collect()),
    }
}

/// Commands that only read (a `read` namespace rule allows only these).
const KV_READ: &[&str] = &[
    "GET",
    "MGET",
    "EXISTS",
    "TTL",
    "PTTL",
    "STRLEN",
    "TYPE",
    "SCAN",
    "KEYS",
    "HGET",
    "HMGET",
    "HGETALL",
    "HKEYS",
    "HVALS",
    "HLEN",
    "HEXISTS",
    "LRANGE",
    "LLEN",
    "LINDEX",
    "SMEMBERS",
    "SISMEMBER",
    "SCARD",
    "SUNION",
    "SINTER",
    "SDIFF",
    "ZRANGE",
    "ZREVRANGE",
    "ZRANGEBYSCORE",
    "ZSCORE",
    "ZCARD",
    "ZCOUNT",
    "ZRANK",
    "ZREVRANK",
    "XRANGE",
    "XREVRANGE",
    "XLEN",
    "PING",
];

/// Whether a namespace rule pattern (`name` or `prefix*`) matches `ns`.
pub(crate) fn pattern_matches(pattern: &str, ns: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => ns.starts_with(prefix),
        None => pattern == ns,
    }
}

fn kv(env: &HostEnv, ctx: &AccessContext, as_admin: bool, v: Json) -> HostResult {
    let Some(kv) = &env.kv else {
        return Err(unavailable("kv"));
    };
    let namespace = v.get("namespace").and_then(Json::as_str).map(str::to_owned);
    let cmd: Vec<String> = take(v.get("cmd").cloned().unwrap_or(Json::Null))?;
    let Some(name) = cmd.first().map(|c| c.to_ascii_uppercase()) else {
        return Err(bad("empty key-value command"));
    };
    let cmd: Vec<Vec<u8>> = cmd.into_iter().map(String::into_bytes).collect();
    let private = kv_namespace(&env.function);
    let (ns, run_as) = match namespace {
        None => {
            // The function's own namespace: its data, not the caller's; the
            // capability is the grant. The caller's tenant still scopes it.
            let mut sys = AccessContext::system();
            if let Some(t) = &env.caller.tenant {
                sys = sys.with_tenant(t.clone());
            }
            (private, sys)
        }
        Some(ns) if ns == private => {
            let mut sys = AccessContext::system();
            if let Some(t) = &env.caller.tenant {
                sys = sys.with_tenant(t.clone());
            }
            (ns, sys)
        }
        Some(ns) => {
            if !as_admin {
                let read_only = KV_READ.contains(&name.as_str());
                let allowed = env
                    .kv_rules
                    .iter()
                    .any(|r| pattern_matches(&r.namespace, &ns) && (read_only || matches!(r.access, KvAccess::Write)));
                if !allowed {
                    return Err(HostError::new(
                        "kv_namespace_denied",
                        format!(
                            "no key-value rule lets this function {} namespace {ns:?} (an administrator adds one with PUT /api/v1/functions/{}/kv-rules)",
                            if read_only { "read" } else { "write" },
                            env.function
                        ),
                    ));
                }
            }
            (ns, ctx.clone())
        }
    };
    let reply = kv.exec(&run_as, &ns, &cmd).map_err(err)?;
    out(&json!({ "value": reply_json(reply) }))
}

async fn vector(env: &HostEnv, ctx: &AccessContext, op: &str, v: Json) -> HostResult {
    let svc = env.services.vector.as_ref().ok_or_else(|| unavailable("vector"))?;
    let o = v.as_object().ok_or_else(|| bad("expected an object"))?;
    let coll = str_field(o, "collection")?;
    match op {
        "vector.search" => {
            if o.get("text").is_some_and(|t| !t.is_null()) {
                let req: forgebase_vector::model::HybridRequest =
                    take(pick(o, &["field", "vector", "text", "k", "filter", "params"]))?;
                out(&svc.hybrid(ctx, &coll, req).await.map_err(err)?)
            } else {
                let req: forgebase_vector::model::SearchRequest = take(pick(
                    o,
                    &[
                        "field",
                        "vector",
                        "k",
                        "filter",
                        "params",
                        "include_vectors",
                        "include_text",
                        "exact",
                    ],
                ))?;
                out(&svc.search(ctx, &coll, req).await.map_err(err)?)
            }
        }
        "vector.get" => out(&svc.get_item(ctx, &coll, &str_field(o, "id")?).await.map_err(err)?),
        "vector.upsert" => {
            let mut items = o.get("items").and_then(Json::as_array).cloned().unwrap_or_default();
            // `vector: [...]` is shorthand for the collection's only vector field.
            if items.iter().any(|i| i.get("vector").is_some()) {
                let fields = svc.list_fields(ctx, &coll).await.map_err(err)?;
                let [field] = fields.as_slice() else {
                    return Err(bad(
                        "`vector` works only for collections with one vector field; use `vectors: {field: [...]}`",
                    ));
                };
                for i in &mut items {
                    if let Some(m) = i.as_object_mut()
                        && let Some(vec) = m.remove("vector")
                    {
                        m.insert("vectors".into(), json!({ field.name.clone(): vec }));
                    }
                }
            }
            let items: Vec<forgebase_vector::model::UpsertItem> = take(Json::Array(items))?;
            out(&svc.upsert(ctx, &coll, items).await.map_err(err)?)
        }
        _ => {
            let ids: Vec<String> = take(o.get("ids").cloned().unwrap_or(Json::Null))?;
            let n = svc.delete_items(ctx, &coll, ids).await.map_err(err)?;
            out(&json!({ "deleted": n }))
        }
    }
}

async fn storage(env: &HostEnv, ctx: &AccessContext, op: &str, v: Json) -> HostResult {
    let svc = env.services.storage.as_ref().ok_or_else(|| unavailable("storage"))?;
    let o = v.as_object().ok_or_else(|| bad("expected an object"))?;
    let bucket = str_field(o, "bucket")?;
    match op {
        "storage.get" => {
            let key = str_field(o, "key")?;
            let read = svc.get_object(ctx, &bucket, &key, None, None).await.map_err(err)?;
            if usize::try_from(read.size).unwrap_or(usize::MAX) > MAX_OBJECT_BYTES {
                return Err(HostError::new(
                    "object_too_large",
                    format!("objects above {MAX_OBJECT_BYTES} bytes cannot be read by a function; use signedUrl"),
                ));
            }
            let bytes = forgebase_storage::driver::collect(read.stream)
                .await
                .map_err(|e| HostError::new("storage_error", e.to_string()))?;
            out(&json!({"info": read.info, "body_base64": b64().encode(bytes)}))
        }
        "storage.put" => {
            let key = str_field(o, "key")?;
            let body = b64()
                .decode(o.get("body_base64").and_then(Json::as_str).unwrap_or_default())
                .map_err(|_| bad("body_base64 is not base64"))?;
            if body.len() > MAX_OBJECT_BYTES {
                return Err(HostError::new(
                    "object_too_large",
                    format!("a function writes at most {MAX_OBJECT_BYTES} bytes per object; use signedUrl"),
                ));
            }
            let metadata: BTreeMap<String, String> = match o.get("metadata") {
                None | Some(Json::Null) => BTreeMap::new(),
                Some(m) => take(m.clone())?,
            };
            let req = forgebase_storage::PutRequest {
                bucket,
                key,
                content_type: o.get("content_type").and_then(Json::as_str).map(str::to_owned),
                metadata,
                expected_sha256: None,
                content_length: Some(body.len() as u64),
                body: forgebase_storage::driver::once(body),
            };
            out(&svc.put_object(ctx, req).await.map_err(err)?)
        }
        "storage.list" => {
            let prefix = o.get("prefix").and_then(Json::as_str).unwrap_or_default().to_owned();
            let limit = o
                .get("limit")
                .and_then(Json::as_u64)
                .map_or(100, |n| u32::try_from(n).unwrap_or(1000));
            let page = svc
                .list_objects(
                    ctx,
                    &bucket,
                    &prefix,
                    o.get("delimiter").and_then(Json::as_str),
                    limit,
                    o.get("cursor").and_then(Json::as_str),
                )
                .await
                .map_err(err)?;
            out(&page)
        }
        "storage.delete" => out(&svc
            .delete_object(ctx, &bucket, &str_field(o, "key")?, None)
            .await
            .map_err(err)?),
        _ => {
            let mut req = pick(o, &["key", "expires_in", "download_as", "content_type"]);
            if let Some(m) = o.get("method").and_then(Json::as_str) {
                req["method"] = Json::String(m.to_ascii_uppercase());
            }
            let req: forgebase_storage::SignRequest = take(req)?;
            out(&svc.sign_url(ctx, &bucket, req).await.map_err(err)?)
        }
    }
}

async fn verify(env: &HostEnv, v: Json) -> HostResult {
    let auth = env.services.auth.as_ref().ok_or_else(|| unavailable("auth"))?;
    let token = v.get("token").and_then(Json::as_str).unwrap_or_default().trim();
    let token = token.strip_prefix("Bearer ").unwrap_or(token).trim();
    if token.is_empty() {
        return Ok(b"null".to_vec());
    }
    let res = if token.starts_with("fbk_") || token.starts_with("fbp_") {
        auth.authenticate_key(token).await
    } else {
        auth.authenticate_access_token(token).await
    };
    match res {
        Ok(a) => {
            let ctx = a.access_context(None);
            let perms = env.services.permissions(&ctx);
            out(&auth_info(perms, &ctx))
        }
        Err(_) => Ok(b"null".to_vec()),
    }
}

fn crypto(op: &str, v: &Json) -> HostResult {
    let alg = v.get("algorithm").and_then(Json::as_str).unwrap_or_default();
    let data = b64()
        .decode(v.get("data_base64").and_then(Json::as_str).unwrap_or_default())
        .map_err(|_| bad("data_base64 is not base64"))?;
    if data.len() > 8 * 1024 * 1024 {
        return Err(bad("at most 8 MiB per digest"));
    }
    let digest = if op == "crypto.digest" {
        match alg {
            "sha-1" => sha1::Sha1::digest(&data).to_vec(),
            "sha-256" => sha2::Sha256::digest(&data).to_vec(),
            "sha-384" => sha2::Sha384::digest(&data).to_vec(),
            "sha-512" => sha2::Sha512::digest(&data).to_vec(),
            other => return Err(bad(format!("unsupported digest {other:?}"))),
        }
    } else {
        let key = b64()
            .decode(v.get("key_base64").and_then(Json::as_str).unwrap_or_default())
            .map_err(|_| bad("key_base64 is not base64"))?;
        fn mac<M: Mac + KeyInit>(key: &[u8], data: &[u8]) -> Result<Vec<u8>, HostError> {
            let mut m = <M as KeyInit>::new_from_slice(key).map_err(|_| bad("invalid HMAC key"))?;
            m.update(data);
            Ok(m.finalize().into_bytes().to_vec())
        }
        match alg {
            "sha-1" => mac::<Hmac<sha1::Sha1>>(&key, &data)?,
            "sha-256" => mac::<Hmac<sha2::Sha256>>(&key, &data)?,
            "sha-384" => mac::<Hmac<sha2::Sha384>>(&key, &data)?,
            "sha-512" => mac::<Hmac<sha2::Sha512>>(&key, &data)?,
            other => return Err(bad(format!("unsupported HMAC hash {other:?}"))),
        }
    };
    out(&json!({ "base64": b64().encode(digest) }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns() {
        assert!(pattern_matches("cache", "cache"));
        assert!(!pattern_matches("cache", "cache2"));
        assert!(pattern_matches("app.*", "app.users"));
        assert!(pattern_matches("*", "anything"));
        assert!(!pattern_matches("app.*", "other"));
    }

    #[test]
    fn digests_and_hmac() {
        let d = crypto(
            "crypto.digest",
            &json!({"algorithm": "sha-256", "data_base64": b64().encode(b"abc")}),
        )
        .unwrap();
        let v: Json = serde_json::from_slice(&d).unwrap();
        assert_eq!(
            b64().decode(v["base64"].as_str().unwrap()).unwrap(),
            hex_bytes("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
        // RFC 4231 test case 2.
        let h = crypto(
            "crypto.hmac",
            &json!({"algorithm": "sha-256", "key_base64": b64().encode(b"Jefe"), "data_base64": b64().encode(b"what do ya want for nothing?")}),
        )
        .unwrap();
        let v: Json = serde_json::from_slice(&h).unwrap();
        assert_eq!(
            b64().decode(v["base64"].as_str().unwrap()).unwrap(),
            hex_bytes("5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843")
        );
    }

    fn hex_bytes(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }
}
