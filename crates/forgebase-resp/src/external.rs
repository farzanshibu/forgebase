//! Optional backend that keeps the data in an external Redis or Valkey
//! server (cargo feature `external-redis`, off by default; nothing needs a
//! Redis to build or test).
//!
//! ForgeBase stays in front: it authenticates the caller, runs the same
//! `Authorizer` actions as the built-in engine and confines the caller to its
//! tenant and namespace by prefixing every key and channel with
//! `fb:<len>:<tenant>:<len>:<namespace>:` (length-prefixed, so two different
//! `(tenant, namespace)` pairs can never share a prefix). Only commands the
//! built-in engine supports are forwarded, so `EVAL`, `CONFIG`, `DEBUG` and
//! friends never reach the server.
//!
//! Differences from the built-in engine: `DBSIZE` and `FLUSHDB` are refused
//! (they would have to scan the whole prefix), pub/sub is not available
//! (`SUBSCRIBE` answers an error; `PUBLISH` works), the engine's memory
//! limit, eviction and SQLite snapshot do not apply (the server's own
//! settings do), and KV stats of the engine do not include this data.

use std::fmt;
use std::sync::Arc;

use forgebase_data::{AccessContext, Action, Authorizer, Decision, Principal};
use forgebase_kv::{Class, KvError, KvResult, Reply, Subscription, spec};
use redis::aio::ConnectionManager;

use crate::backend::{BoxFuture, KvBackend};

/// A Redis/Valkey server used as the KV store.
#[derive(Clone)]
pub struct ExternalRedis {
    conn: ConnectionManager,
    authorizer: Arc<dyn Authorizer>,
}

impl fmt::Debug for ExternalRedis {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExternalRedis").finish_non_exhaustive()
    }
}

impl ExternalRedis {
    /// Connects to `url` (`redis://[user:password@]host:port/db`, or
    /// `rediss://` with a TLS-enabled build of the `redis` crate).
    ///
    /// # Errors
    /// The URL is invalid or the server cannot be reached.
    pub async fn connect(url: &str, authorizer: Arc<dyn Authorizer>) -> Result<Self, redis::RedisError> {
        let client = redis::Client::open(url)?;
        let conn = client.get_connection_manager().await?;
        Ok(Self { conn, authorizer })
    }

    fn authorize(&self, ctx: &AccessContext, action: Action) -> KvResult<()> {
        match self.authorizer.authorize(ctx, action, None) {
            Decision::Allow => Ok(()),
            _ => Err(if ctx.principal == Principal::Anonymous {
                KvError::Unauthenticated
            } else {
                KvError::Forbidden
            }),
        }
    }
}

/// The key prefix of a caller: `fb:<len>:<tenant>:<len>:<ns>:`.
pub fn key_prefix(ctx: &AccessContext, ns: &str) -> KvResult<Vec<u8>> {
    forgebase_kv::validate_namespace(ns)?;
    let tenant = ctx
        .tenant
        .as_ref()
        .map_or_else(|| "default".to_owned(), |t| format!("t:{t}"));
    Ok(format!("fb:{}:{tenant}:{}:{ns}:", tenant.len(), ns.len()).into_bytes())
}

/// A command rewritten to carry the caller's prefix.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Rewritten {
    /// The command to send.
    pub cmd: Vec<Vec<u8>>,
    /// Strip this prefix from the keys of the reply (`KEYS`, `SCAN`).
    pub strip: Option<Vec<u8>>,
    /// The reply is a `SCAN` reply: `[cursor, [keys]]`.
    pub scan: bool,
}

const KEY1: &[&str] = &[
    "TYPE",
    "EXPIRE",
    "PEXPIRE",
    "EXPIREAT",
    "PEXPIREAT",
    "PERSIST",
    "TTL",
    "PTTL",
    "GET",
    "SET",
    "SETNX",
    "SETEX",
    "PSETEX",
    "GETDEL",
    "APPEND",
    "STRLEN",
    "INCR",
    "DECR",
    "INCRBY",
    "DECRBY",
    "INCRBYFLOAT",
    "HSET",
    "HSETNX",
    "HGET",
    "HMGET",
    "HGETALL",
    "HKEYS",
    "HVALS",
    "HLEN",
    "HEXISTS",
    "HDEL",
    "HINCRBY",
    "LPUSH",
    "RPUSH",
    "LPOP",
    "RPOP",
    "LLEN",
    "LRANGE",
    "LINDEX",
    "LSET",
    "LREM",
    "LTRIM",
    "SADD",
    "SREM",
    "SISMEMBER",
    "SMEMBERS",
    "SCARD",
    "SPOP",
    "ZADD",
    "ZREM",
    "ZSCORE",
    "ZRANK",
    "ZREVRANK",
    "ZRANGE",
    "ZREVRANGE",
    "ZRANGEBYSCORE",
    "ZINCRBY",
    "ZCARD",
    "ZCOUNT",
    "ZPOPMIN",
    "ZPOPMAX",
    "XADD",
    "XLEN",
    "XRANGE",
    "XREVRANGE",
    "XDEL",
    "XTRIM",
    "PUBLISH",
];
const ALL_KEYS: &[&str] = &["DEL", "UNLINK", "EXISTS", "MGET", "SUNION", "SINTER", "SDIFF"];

fn glob_escape(p: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(p.len() + 8);
    for &b in p {
        if matches!(b, b'*' | b'?' | b'[' | b']' | b'\\' | b'^' | b'-') {
            out.push(b'\\');
        }
        out.push(b);
    }
    out
}

fn with_prefix(prefix: &[u8], key: &[u8]) -> Vec<u8> {
    let mut k = prefix.to_vec();
    k.extend_from_slice(key);
    k
}

/// Rewrites `cmd` (already known to the engine's `spec`) for the external server.
pub(crate) fn rewrite(prefix: &[u8], cmd: &[Vec<u8>]) -> KvResult<Rewritten> {
    let name = String::from_utf8_lossy(&cmd[0]).to_ascii_uppercase();
    let mut out: Vec<Vec<u8>> = cmd.to_vec();
    out[0] = name.clone().into_bytes();
    let mut strip = None;
    let mut scan = false;
    let not_here = |n: &str| KvError::Syntax(format!("{n} is not available with the external Redis backend"));
    match name.as_str() {
        "PING" => {}
        n if KEY1.contains(&n) => {
            if let Some(k) = out.get_mut(1) {
                *k = with_prefix(prefix, k);
            }
        }
        n if ALL_KEYS.contains(&n) => {
            for k in &mut out[1..] {
                *k = with_prefix(prefix, k);
            }
        }
        "MSET" => {
            for k in out[1..].iter_mut().step_by(2) {
                *k = with_prefix(prefix, k);
            }
        }
        "XREAD" => {
            let at = out
                .iter()
                .position(|a| a.eq_ignore_ascii_case(b"STREAMS"))
                .ok_or_else(|| KvError::Syntax("syntax error".into()))?;
            let rest = out.len() - at - 1;
            if rest == 0 || !rest.is_multiple_of(2) {
                return Err(KvError::Syntax(
                    "Unbalanced 'xread' list of streams: for each stream key an ID or '$' must be specified.".into(),
                ));
            }
            for k in &mut out[at + 1..at + 1 + rest / 2] {
                *k = with_prefix(prefix, k);
            }
        }
        "KEYS" => {
            let mut p = glob_escape(prefix);
            p.extend_from_slice(&out[1]);
            out[1] = p;
            strip = Some(prefix.to_vec());
        }
        "SCAN" => {
            let mut matched = false;
            let mut i = 2;
            while i < out.len() {
                if out[i].eq_ignore_ascii_case(b"MATCH") && i + 1 < out.len() {
                    let mut p = glob_escape(prefix);
                    p.extend_from_slice(&out[i + 1]);
                    out[i + 1] = p;
                    matched = true;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            if !matched {
                let mut p = glob_escape(prefix);
                p.push(b'*');
                out.push(b"MATCH".to_vec());
                out.push(p);
            }
            strip = Some(prefix.to_vec());
            scan = true;
        }
        "DBSIZE" | "FLUSHDB" => return Err(not_here(&name)),
        _ => return Err(KvError::UnknownCommand(name.to_ascii_lowercase())),
    }
    Ok(Rewritten { cmd: out, strip, scan })
}

fn strip_key(b: &[u8], prefix: &[u8]) -> Vec<u8> {
    b.strip_prefix(prefix).unwrap_or(b).to_vec()
}

fn convert(v: redis::Value, strip: Option<&[u8]>, scan: bool) -> KvResult<Reply> {
    use redis::Value as V;
    Ok(match v {
        V::Nil => Reply::Nil,
        V::Int(n) => Reply::Int(n),
        V::BulkString(b) => Reply::Bulk(b),
        V::SimpleString(s) => Reply::Status(s),
        V::Okay => Reply::ok(),
        V::Boolean(b) => Reply::Int(i64::from(b)),
        V::Double(d) => Reply::double(d),
        V::VerbatimString { text, .. } => Reply::Bulk(text.into_bytes()),
        V::BigNumber(n) => Reply::Bulk(n.to_string().into_bytes()),
        V::Array(items) | V::Set(items) => {
            if scan && items.len() == 2 {
                let mut it = items.into_iter();
                let cursor = convert(it.next().unwrap_or(V::Nil), None, false)?;
                let keys = match it.next() {
                    Some(V::Array(keys)) => keys
                        .into_iter()
                        .map(|k| match k {
                            V::BulkString(b) => Reply::Bulk(strip.map_or(b.clone(), |p| strip_key(&b, p))),
                            other => convert(other, None, false).unwrap_or(Reply::Nil),
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                return Ok(Reply::Array(vec![cursor, Reply::Array(keys)]));
            }
            let strip_here = if scan { None } else { strip };
            Reply::Array(
                items
                    .into_iter()
                    .map(|i| match (i, strip_here) {
                        (V::BulkString(b), Some(p)) => Ok(Reply::Bulk(strip_key(&b, p))),
                        (i, _) => convert(i, None, false),
                    })
                    .collect::<KvResult<Vec<_>>>()?,
            )
        }
        V::Map(pairs) => {
            let mut flat = Vec::with_capacity(pairs.len() * 2);
            for (k, v) in pairs {
                flat.push(convert(k, None, false)?);
                flat.push(convert(v, None, false)?);
            }
            Reply::Array(flat)
        }
        V::ServerError(e) => return Err(server_error(e.code(), e.details())),
        other => {
            return Err(KvError::Storage(format!(
                "unexpected reply from external Redis: {other:?}"
            )));
        }
    })
}

fn server_error(code: &str, detail: Option<&str>) -> KvError {
    match code {
        "WRONGTYPE" => KvError::WrongType,
        "OOM" => KvError::OutOfMemory,
        _ => KvError::Syntax(match detail {
            Some(d) => format!("{code} {d}"),
            None => code.to_owned(),
        }),
    }
}

impl KvBackend for ExternalRedis {
    fn exec<'a>(&'a self, ctx: &'a AccessContext, ns: &'a str, cmd: &'a [Vec<u8>]) -> BoxFuture<'a, KvResult<Reply>> {
        Box::pin(async move {
            let name = cmd
                .first()
                .map(|n| String::from_utf8_lossy(n).to_ascii_uppercase())
                .ok_or_else(|| KvError::Syntax("empty command".into()))?;
            // Authorise before revealing whether the command exists.
            let sp = spec(&name);
            self.authorize(
                ctx,
                match sp.map(|s| s.class) {
                    Some(Class::Write) => Action::KvWrite,
                    Some(Class::Admin) => Action::KvAdmin,
                    _ => Action::KvRead,
                },
            )?;
            let sp = sp.ok_or_else(|| KvError::UnknownCommand(name.to_ascii_lowercase()))?;
            if cmd.len() - 1 < sp.min_args || cmd.len() - 1 > sp.max_args {
                return Err(KvError::WrongArity(name.to_ascii_lowercase()));
            }
            let prefix = key_prefix(ctx, ns)?;
            let rw = rewrite(&prefix, cmd)?;
            let mut c = redis::cmd(&name);
            for a in &rw.cmd[1..] {
                c.arg(a.as_slice());
            }
            let mut conn = self.conn.clone();
            let v: redis::Value = c.query_async(&mut conn).await.map_err(|e| match e.code() {
                Some(code) => server_error(code, e.detail()),
                None => KvError::Storage(format!("external Redis: {e}")),
            })?;
            convert(v, rw.strip.as_deref(), rw.scan)
        })
    }

    fn subscribe(&self, _ctx: &AccessContext, _ns: &str, _channel: &str) -> KvResult<Subscription> {
        Err(KvError::Syntax(
            "pub/sub is not available with the external Redis backend".into(),
        ))
    }

    fn subscribe_all(&self, _ctx: &AccessContext, _ns: &str) -> KvResult<Subscription> {
        Err(KvError::Syntax(
            "pub/sub is not available with the external Redis backend".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn c(a: &[&str]) -> Vec<Vec<u8>> {
        a.iter().map(|s| s.as_bytes().to_vec()).collect()
    }

    fn ctx(tenant: &str) -> AccessContext {
        let mut x = AccessContext::admin("a");
        x.tenant = Some(tenant.into());
        x
    }

    #[test]
    fn prefixes_are_unambiguous_across_tenants_and_namespaces() {
        // ("t:a", "x:y") and ("t:a:x", "y") would collide without length prefixes.
        let p1 = key_prefix(&ctx("a"), "x:y").unwrap();
        let p2 = key_prefix(&ctx("a:x"), "y").unwrap();
        assert_ne!(p1, p2);
        assert!(!p1.starts_with(&p2) && !p2.starts_with(&p1));
        assert_eq!(
            key_prefix(&AccessContext::admin("a"), "cache").unwrap(),
            b"fb:7:default:5:cache:"
        );
        assert!(key_prefix(&ctx("a"), "bad name").is_err());
    }

    #[test]
    fn every_engine_command_has_a_rewrite_rule() {
        let p = b"P:";
        for name in crate::compat::DATA_COMMANDS {
            let r = rewrite(p, &c(&[name, "k", "v", "w", "x"]));
            match *name {
                "DBSIZE" | "FLUSHDB" => assert!(r.is_err(), "{name}"),
                "XREAD" => {} // needs STREAMS
                _ => assert!(r.is_ok(), "{name} has no rewrite rule: {r:?}"),
            }
        }
    }

    #[test]
    fn keys_are_prefixed_wherever_they_sit() {
        let p = b"P:";
        assert_eq!(rewrite(p, &c(&["GET", "k"])).unwrap().cmd, c(&["GET", "P:k"]));
        assert_eq!(rewrite(p, &c(&["get", "k"])).unwrap().cmd, c(&["GET", "P:k"]));
        assert_eq!(
            rewrite(p, &c(&["DEL", "a", "b"])).unwrap().cmd,
            c(&["DEL", "P:a", "P:b"])
        );
        assert_eq!(
            rewrite(p, &c(&["MSET", "a", "1", "b", "2"])).unwrap().cmd,
            c(&["MSET", "P:a", "1", "P:b", "2"]),
            "only keys, not values"
        );
        assert_eq!(
            rewrite(p, &c(&["XREAD", "COUNT", "2", "STREAMS", "s1", "s2", "0", "$"]))
                .unwrap()
                .cmd,
            c(&["XREAD", "COUNT", "2", "STREAMS", "P:s1", "P:s2", "0", "$"])
        );
        assert!(rewrite(p, &c(&["XREAD", "STREAMS", "s1"])).is_err());
        assert_eq!(
            rewrite(p, &c(&["PUBLISH", "ch", "m"])).unwrap().cmd,
            c(&["PUBLISH", "P:ch", "m"])
        );
        assert_eq!(rewrite(p, &c(&["PING"])).unwrap().cmd, c(&["PING"]));
    }

    #[test]
    fn patterns_cannot_escape_the_prefix() {
        let p = b"fb:7:default:1:a:";
        let r = rewrite(p, &c(&["KEYS", "*"])).unwrap();
        assert_eq!(r.cmd[1], b"fb:7:default:1:a:*");
        // glob characters inside the prefix itself are escaped
        let r = rewrite(b"a*b:", &c(&["KEYS", "k?"])).unwrap();
        assert_eq!(r.cmd[1], b"a\\*b:k?");
        let r = rewrite(p, &c(&["SCAN", "0", "COUNT", "5"])).unwrap();
        assert_eq!(r.cmd, c(&["SCAN", "0", "COUNT", "5", "MATCH", "fb:7:default:1:a:*"]));
        assert!(r.scan);
        let r = rewrite(p, &c(&["SCAN", "0", "MATCH", "u:*"])).unwrap();
        assert_eq!(r.cmd[3], b"fb:7:default:1:a:u:*");
    }

    #[test]
    fn replies_are_converted_and_prefixes_stripped() {
        use redis::Value as V;
        let keys = V::Array(vec![V::BulkString(b"P:a".to_vec()), V::BulkString(b"P:b".to_vec())]);
        assert_eq!(
            convert(keys, Some(b"P:"), false).unwrap(),
            Reply::Array(vec![Reply::bulk("a"), Reply::bulk("b")])
        );
        let scan = V::Array(vec![
            V::BulkString(b"17".to_vec()),
            V::Array(vec![V::BulkString(b"P:a".to_vec())]),
        ]);
        assert_eq!(
            convert(scan, Some(b"P:"), true).unwrap(),
            Reply::Array(vec![Reply::bulk("17"), Reply::Array(vec![Reply::bulk("a")])])
        );
        assert_eq!(convert(V::Okay, None, false).unwrap(), Reply::ok());
        assert_eq!(convert(V::Nil, None, false).unwrap(), Reply::Nil);
        assert_eq!(convert(V::Int(4), None, false).unwrap(), Reply::Int(4));
        assert_eq!(
            convert(V::Map(vec![(V::BulkString(b"f".to_vec()), V::Int(1))]), None, false).unwrap(),
            Reply::Array(vec![Reply::bulk("f"), Reply::Int(1)])
        );
        assert!(matches!(server_error("WRONGTYPE", Some("x")), KvError::WrongType));
    }

    /// Needs a real server: `FORGEBASE_TEST_REDIS_URL=redis://127.0.0.1:6379/ cargo test -p forgebase-resp --features external-redis -- --ignored`.
    #[tokio::test]
    #[ignore = "needs FORGEBASE_TEST_REDIS_URL pointing at a Redis or Valkey server"]
    async fn round_trip_against_a_real_server_and_tenant_isolation() {
        let url = std::env::var("FORGEBASE_TEST_REDIS_URL").expect("FORGEBASE_TEST_REDIS_URL");
        let b = ExternalRedis::connect(&url, Arc::new(forgebase_data::DefaultAuthorizer))
            .await
            .unwrap();
        let (a, other) = (ctx("alpha"), ctx("beta"));
        let set = c(&["SET", "k", "v"]);
        assert_eq!(b.exec(&a, "main", &set).await.unwrap(), Reply::ok());
        assert_eq!(b.exec(&a, "main", &c(&["GET", "k"])).await.unwrap(), Reply::bulk("v"));
        assert_eq!(b.exec(&other, "main", &c(&["GET", "k"])).await.unwrap(), Reply::Nil);
        assert_eq!(
            b.exec(&a, "main", &c(&["KEYS", "*"])).await.unwrap(),
            Reply::Array(vec![Reply::bulk("k")])
        );
        assert_eq!(b.exec(&a, "main", &c(&["DEL", "k"])).await.unwrap(), Reply::Int(1));
        assert!(
            b.exec(&AccessContext::anonymous(), "main", &c(&["GET", "k"]))
                .await
                .is_err()
        );
    }
}
