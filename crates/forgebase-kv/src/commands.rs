//! The command layer: Redis-style commands over byte arguments.
//!
//! A command is its name followed by arguments, all as byte strings
//! (`["SET", "k", "v", "EX", "10"]`). The answer is a [`Reply`]. Nothing here
//! knows about HTTP or RESP, so the REST layer and a RESP listener (FB-32) both
//! drive the same code. [`spec`] says what a command needs (read, write or
//! admin access, arity, whether it may grow memory) so the caller can
//! authorise it before running it.

use std::collections::{BTreeSet, VecDeque};

use crate::data::{Data, Fields, HashV, Kind, ListV, SetV, StreamId, StreamV, ZSetV};
use crate::error::{KvError, KvResult};
use crate::glob;
use crate::reply::{Reply, format_double};
use crate::store::{KeyId, Scope, Store};

/// What access a command needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Class {
    /// Reads only.
    Read,
    /// Changes data.
    Write,
    /// Administrative (flushing a namespace).
    Admin,
}

/// Static facts about a command.
#[derive(Debug, Clone, Copy)]
pub struct Spec {
    /// Access needed.
    pub class: Class,
    /// Fewest arguments after the command name.
    pub min_args: usize,
    /// Most arguments after the command name.
    pub max_args: usize,
    /// May add data, so it is refused when memory is full and nothing can be evicted.
    pub grows: bool,
}

const MANY: usize = usize::MAX;

/// Looks a command up by its upper-case name.
pub fn spec(name: &str) -> Option<Spec> {
    use Class::{Admin, Read, Write};
    let (class, min_args, max_args, grows) = match name {
        "PING" => (Read, 0, 1, false),
        // keys
        "DEL" | "UNLINK" => (Write, 1, MANY, false),
        "EXISTS" => (Read, 1, MANY, false),
        "TYPE" | "TTL" | "PTTL" => (Read, 1, 1, false),
        "EXPIRE" | "PEXPIRE" | "EXPIREAT" | "PEXPIREAT" => (Write, 2, 2, false),
        "PERSIST" => (Write, 1, 1, false),
        "KEYS" => (Read, 1, 1, false),
        "SCAN" => (Read, 1, 7, false),
        "DBSIZE" => (Read, 0, 0, false),
        "FLUSHDB" => (Admin, 0, 0, false),
        // strings
        "GET" | "STRLEN" => (Read, 1, 1, false),
        "SET" => (Write, 2, 7, true),
        "SETNX" | "APPEND" => (Write, 2, 2, true),
        "SETEX" | "PSETEX" => (Write, 3, 3, true),
        "GETDEL" => (Write, 1, 1, false),
        "MGET" => (Read, 1, MANY, false),
        "MSET" => (Write, 2, MANY, true),
        "INCR" | "DECR" => (Write, 1, 1, true),
        "INCRBY" | "DECRBY" | "INCRBYFLOAT" => (Write, 2, 2, true),
        // hashes
        "HSET" => (Write, 3, MANY, true),
        "HSETNX" | "HINCRBY" => (Write, 3, 3, true),
        "HGET" | "HEXISTS" => (Read, 2, 2, false),
        "HMGET" => (Read, 2, MANY, false),
        "HGETALL" | "HKEYS" | "HVALS" | "HLEN" => (Read, 1, 1, false),
        "HDEL" => (Write, 2, MANY, false),
        // lists
        "LPUSH" | "RPUSH" => (Write, 2, MANY, true),
        "LPOP" | "RPOP" => (Write, 1, 2, false),
        "LLEN" => (Read, 1, 1, false),
        "LRANGE" => (Read, 3, 3, false),
        "LINDEX" => (Read, 2, 2, false),
        "LSET" => (Write, 3, 3, true),
        "LREM" => (Write, 3, 3, false),
        "LTRIM" => (Write, 3, 3, false),
        // sets
        "SADD" => (Write, 2, MANY, true),
        "SREM" => (Write, 2, MANY, false),
        "SISMEMBER" => (Read, 2, 2, false),
        "SMEMBERS" | "SCARD" => (Read, 1, 1, false),
        "SPOP" => (Write, 1, 2, false),
        "SUNION" | "SINTER" | "SDIFF" => (Read, 1, MANY, false),
        // sorted sets
        "ZADD" => (Write, 3, MANY, true),
        "ZREM" => (Write, 2, MANY, false),
        "ZSCORE" | "ZRANK" | "ZREVRANK" => (Read, 2, 2, false),
        "ZRANGE" | "ZREVRANGE" => (Read, 3, 4, false),
        "ZRANGEBYSCORE" => (Read, 3, 7, false),
        "ZINCRBY" => (Write, 3, 3, true),
        "ZCARD" => (Read, 1, 1, false),
        "ZCOUNT" => (Read, 3, 3, false),
        "ZPOPMIN" | "ZPOPMAX" => (Write, 1, 2, false),
        // streams
        "XADD" => (Write, 4, MANY, true),
        "XLEN" => (Read, 1, 1, false),
        "XRANGE" | "XREVRANGE" => (Read, 3, 5, false),
        "XREAD" => (Read, 3, MANY, false),
        "XDEL" => (Write, 2, MANY, false),
        "XTRIM" => (Write, 3, 4, false),
        // pub/sub is handled by the service (it owns the channels)
        "PUBLISH" => (Write, 2, 2, false),
        _ => return None,
    };
    Some(Spec {
        class,
        min_args,
        max_args,
        grows,
    })
}

// ---- argument helpers --------------------------------------------------------

fn text(b: &[u8]) -> KvResult<&str> {
    std::str::from_utf8(b).map_err(|_| KvError::Syntax("argument must be valid UTF-8".into()))
}

fn int(b: &[u8]) -> KvResult<i64> {
    std::str::from_utf8(b)
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .ok_or(KvError::NotInteger)
}

fn uint(b: &[u8]) -> KvResult<u64> {
    std::str::from_utf8(b)
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .ok_or(KvError::NotInteger)
}

fn float(b: &[u8]) -> KvResult<f64> {
    let s = std::str::from_utf8(b).map_err(|_| KvError::NotFloat)?;
    let v = match s.to_ascii_lowercase().as_str() {
        "inf" | "+inf" | "infinity" | "+infinity" => f64::INFINITY,
        "-inf" | "-infinity" => f64::NEG_INFINITY,
        t => t.parse::<f64>().map_err(|_| KvError::NotFloat)?,
    };
    if v.is_nan() { Err(KvError::NotFloat) } else { Ok(v) }
}

/// Redis-style inclusive index range over `len` items; `None` when empty.
fn norm_range(start: i64, stop: i64, len: usize) -> Option<(usize, usize)> {
    let len = i64::try_from(len).ok()?;
    let mut s = if start < 0 { len + start } else { start };
    let mut e = if stop < 0 { len + stop } else { stop };
    if s < 0 {
        s = 0;
    }
    if e >= len {
        e = len - 1;
    }
    if len == 0 || s > e || s >= len {
        return None;
    }
    Some((usize::try_from(s).ok()?, usize::try_from(e).ok()?))
}

fn upper(b: &[u8]) -> String {
    String::from_utf8_lossy(b).to_ascii_uppercase()
}

fn bulks(items: impl IntoIterator<Item = Vec<u8>>) -> Reply {
    Reply::Array(items.into_iter().map(Reply::Bulk).collect())
}

fn opt_bulk(v: Option<Vec<u8>>) -> Reply {
    v.map_or(Reply::Nil, Reply::Bulk)
}

macro_rules! typed {
    ($name:ident, $variant:ident, $ty:ty) => {
        fn $name(d: &mut Data) -> KvResult<&mut $ty> {
            match d {
                Data::$variant(v) => Ok(v),
                _ => Err(KvError::WrongType),
            }
        }
    };
}
typed!(str_mut, Str, Vec<u8>);
typed!(hash_mut, Hash, HashV);
typed!(list_mut, List, ListV);
typed!(set_mut, Set, SetV);
typed!(zset_mut, ZSet, ZSetV);
typed!(stream_mut, Stream, StreamV);

macro_rules! reader {
    ($name:ident, $variant:ident, $ty:ty) => {
        fn $name(&mut self, id: &KeyId, now: u64) -> KvResult<Option<&$ty>> {
            match self.get_entry(id, now) {
                None => Ok(None),
                Some(e) => match &e.data {
                    Data::$variant(v) => Ok(Some(v)),
                    _ => Err(KvError::WrongType),
                },
            }
        }
    };
}

impl Store {
    reader!(str_ref, Str, Vec<u8>);
    reader!(hash_ref, Hash, HashV);
    reader!(list_ref, List, ListV);
    reader!(set_ref, Set, SetV);
    reader!(zset_ref, ZSet, ZSetV);
    reader!(stream_ref, Stream, StreamV);

    fn key(&self, scope: &Scope, raw: &[u8]) -> KvResult<KeyId> {
        let k = text(raw).map_err(|_| KvError::InvalidKey("key must be valid UTF-8".into()))?;
        if k.is_empty() {
            return Err(KvError::InvalidKey("key must not be empty".into()));
        }
        if k.len() > self.cfg.max_key_bytes {
            return Err(KvError::InvalidKey(format!(
                "key longer than {} bytes",
                self.cfg.max_key_bytes
            )));
        }
        Ok(KeyId::new(scope, k))
    }

    fn check_value(&self, v: &[u8]) -> KvResult<()> {
        if v.len() > self.cfg.max_value_bytes {
            Err(KvError::TooLarge(self.cfg.max_value_bytes))
        } else {
            Ok(())
        }
    }

    fn check_values<'a>(&self, vs: impl IntoIterator<Item = &'a Vec<u8>>) -> KvResult<()> {
        vs.into_iter().try_for_each(|v| self.check_value(v))
    }

    /// Runs one command in `scope` at time `now` (unix milliseconds).
    pub(crate) fn exec(&mut self, scope: &Scope, args: &[Vec<u8>], now: u64) -> KvResult<Reply> {
        let Some(first) = args.first() else {
            return Err(KvError::Syntax("empty command".into()));
        };
        let name = upper(first);
        let a = &args[1..];
        let sp = spec(&name).ok_or_else(|| KvError::UnknownCommand(name.to_ascii_lowercase()))?;
        if a.len() < sp.min_args || a.len() > sp.max_args {
            return Err(KvError::WrongArity(name.to_ascii_lowercase()));
        }
        if sp.grows {
            self.make_room()?;
        }
        let reply = self.dispatch(&name, scope, a, now)?;
        if sp.class != Class::Read {
            self.evict_to_limit();
        }
        Ok(reply)
    }

    fn dispatch(&mut self, name: &str, s: &Scope, a: &[Vec<u8>], now: u64) -> KvResult<Reply> {
        match name {
            "PING" => Ok(a
                .first()
                .map_or_else(|| Reply::Status("PONG".into()), |m| Reply::bulk(m.clone()))),
            "DEL" | "UNLINK" => self.del(s, a, now),
            "EXISTS" => {
                let mut n = 0;
                for k in a {
                    let id = self.key(s, k)?;
                    n += usize::from(self.live(&id, now));
                }
                Ok(Reply::count(n))
            }
            "TYPE" => {
                let id = self.key(s, &a[0])?;
                let t = self.get_entry(&id, now).map_or("none", |e| e.data.kind().name());
                Ok(Reply::Status(t.into()))
            }
            "EXPIRE" => self.expire(s, a, now, |v, _| v.checked_mul(1000).map(|ms| now_plus(now, ms))),
            "PEXPIRE" => self.expire(s, a, now, |v, _| Some(now_plus(now, v))),
            "EXPIREAT" => self.expire(s, a, now, |v, _| {
                v.checked_mul(1000).and_then(|m| u64::try_from(m).ok())
            }),
            "PEXPIREAT" => self.expire(s, a, now, |v, _| u64::try_from(v).ok()),
            "PERSIST" => {
                let id = self.key(s, &a[0])?;
                let had = self.live(&id, now) && self.peek(&id).is_some_and(|e| e.expires_at.is_some());
                if had {
                    self.set_expiry(&id, None, now);
                }
                Ok(Reply::Int(i64::from(had)))
            }
            "TTL" | "PTTL" => {
                let id = self.key(s, &a[0])?;
                if !self.live(&id, now) {
                    return Ok(Reply::Int(-2));
                }
                Ok(Reply::Int(match self.peek(&id).and_then(|e| e.expires_at) {
                    None => -1,
                    Some(t) => {
                        let ms = i64::try_from(t.saturating_sub(now)).unwrap_or(i64::MAX);
                        if name == "TTL" {
                            (ms.saturating_add(500)) / 1000
                        } else {
                            ms
                        }
                    }
                }))
            }
            "KEYS" => self.keys(s, text(&a[0])?, now),
            "SCAN" => self.scan(s, a, now),
            "DBSIZE" => Ok(Reply::count(self.scope_count(s))),
            "FLUSHDB" => Ok(Reply::count(self.flush_scope(s))),
            // strings
            "GET" => {
                let id = self.key(s, &a[0])?;
                Ok(opt_bulk(self.str_ref(&id, now)?.cloned()))
            }
            "STRLEN" => {
                let id = self.key(s, &a[0])?;
                Ok(Reply::count(self.str_ref(&id, now)?.map_or(0, Vec::len)))
            }
            "SET" => self.set(s, a, now),
            "SETNX" => {
                let id = self.key(s, &a[0])?;
                self.check_value(&a[1])?;
                if self.live(&id, now) {
                    return Ok(Reply::Int(0));
                }
                self.replace(&id, Data::Str(a[1].clone()), None, false, now);
                Ok(Reply::Int(1))
            }
            "SETEX" | "PSETEX" => {
                let id = self.key(s, &a[0])?;
                let n = int(&a[1])?;
                if n <= 0 {
                    return Err(KvError::Syntax("invalid expire time".into()));
                }
                let ms = if name == "SETEX" { n.checked_mul(1000) } else { Some(n) }
                    .ok_or_else(|| KvError::Syntax("invalid expire time".into()))?;
                self.check_value(&a[2])?;
                self.replace(&id, Data::Str(a[2].clone()), Some(now_plus(now, ms)), false, now);
                Ok(Reply::ok())
            }
            "GETDEL" => {
                let id = self.key(s, &a[0])?;
                let v = self.str_ref(&id, now)?.cloned();
                if v.is_some() {
                    self.delete(&id, now);
                }
                Ok(opt_bulk(v))
            }
            "MGET" => {
                let mut out = Vec::with_capacity(a.len());
                for k in a {
                    let id = self.key(s, k)?;
                    out.push(match self.get_entry(&id, now) {
                        Some(e) => match &e.data {
                            Data::Str(v) => Reply::Bulk(v.clone()),
                            _ => Reply::Nil,
                        },
                        None => Reply::Nil,
                    });
                }
                Ok(Reply::Array(out))
            }
            "MSET" => {
                if !a.len().is_multiple_of(2) {
                    return Err(KvError::WrongArity("mset".into()));
                }
                let mut pairs = Vec::new();
                for kv in a.chunks(2) {
                    self.check_value(&kv[1])?;
                    pairs.push((self.key(s, &kv[0])?, kv[1].clone()));
                }
                for (id, v) in pairs {
                    self.replace(&id, Data::Str(v), None, false, now);
                }
                Ok(Reply::ok())
            }
            "INCR" => self.incr(s, &a[0], 1, now),
            "DECR" => self.incr(s, &a[0], -1, now),
            "INCRBY" => self.incr(s, &a[0], int(&a[1])?, now),
            "DECRBY" => {
                let by = int(&a[1])?;
                self.incr(s, &a[0], by.checked_neg().ok_or(KvError::Overflow)?, now)
            }
            "INCRBYFLOAT" => {
                let id = self.key(s, &a[0])?;
                let by = float(&a[1])?;
                let r = self.modify(&id, now, Some(Data::Str(b"0".to_vec())), |d| {
                    let v = str_mut(d)?;
                    let cur = float(v)?;
                    let sum = cur + by;
                    if !sum.is_finite() {
                        return Err(KvError::Overflow);
                    }
                    *v = format_double(sum).into_bytes();
                    Ok(v.clone())
                })?;
                Ok(opt_bulk(r))
            }
            "APPEND" => {
                let id = self.key(s, &a[0])?;
                let add = &a[1];
                let max = self.cfg.max_value_bytes;
                let n = self.modify(&id, now, Some(Data::Str(Vec::new())), |d| {
                    let v = str_mut(d)?;
                    if v.len() + add.len() > max {
                        return Err(KvError::TooLarge(max));
                    }
                    v.extend_from_slice(add);
                    Ok(v.len())
                })?;
                Ok(Reply::count(n.unwrap_or(0)))
            }
            // hashes
            "HSET" => {
                if a.len() % 2 != 1 {
                    return Err(KvError::WrongArity("hset".into()));
                }
                let id = self.key(s, &a[0])?;
                self.check_values(&a[1..])?;
                let n = self.modify(&id, now, Some(Data::empty(Kind::Hash)), |d| {
                    let h = hash_mut(d)?;
                    Ok(a[1..]
                        .chunks(2)
                        .filter(|kv| h.set(kv[0].clone(), kv[1].clone()))
                        .count())
                })?;
                Ok(Reply::count(n.unwrap_or(0)))
            }
            "HSETNX" => {
                let id = self.key(s, &a[0])?;
                self.check_values(&a[1..])?;
                let n = self.modify(&id, now, Some(Data::empty(Kind::Hash)), |d| {
                    let h = hash_mut(d)?;
                    Ok(i64::from(
                        !h.map.contains_key(&a[1]) && h.set(a[1].clone(), a[2].clone()),
                    ))
                })?;
                Ok(Reply::Int(n.unwrap_or(0)))
            }
            "HGET" => {
                let id = self.key(s, &a[0])?;
                Ok(opt_bulk(
                    self.hash_ref(&id, now)?.and_then(|h| h.map.get(&a[1]).cloned()),
                ))
            }
            "HMGET" => {
                let id = self.key(s, &a[0])?;
                let h = self.hash_ref(&id, now)?;
                Ok(Reply::Array(
                    a[1..]
                        .iter()
                        .map(|f| opt_bulk(h.and_then(|h| h.map.get(f).cloned())))
                        .collect(),
                ))
            }
            "HGETALL" => {
                let id = self.key(s, &a[0])?;
                Ok(bulks(
                    self.hash_ref(&id, now)?
                        .map(|h| {
                            h.map
                                .iter()
                                .flat_map(|(k, v)| [k.clone(), v.clone()])
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default(),
                ))
            }
            "HKEYS" => {
                let id = self.key(s, &a[0])?;
                Ok(bulks(
                    self.hash_ref(&id, now)?
                        .map(|h| h.map.keys().cloned().collect::<Vec<_>>())
                        .unwrap_or_default(),
                ))
            }
            "HVALS" => {
                let id = self.key(s, &a[0])?;
                Ok(bulks(
                    self.hash_ref(&id, now)?
                        .map(|h| h.map.values().cloned().collect::<Vec<_>>())
                        .unwrap_or_default(),
                ))
            }
            "HLEN" => {
                let id = self.key(s, &a[0])?;
                Ok(Reply::count(self.hash_ref(&id, now)?.map_or(0, |h| h.map.len())))
            }
            "HEXISTS" => {
                let id = self.key(s, &a[0])?;
                Ok(Reply::Int(i64::from(
                    self.hash_ref(&id, now)?.is_some_and(|h| h.map.contains_key(&a[1])),
                )))
            }
            "HDEL" => {
                let id = self.key(s, &a[0])?;
                let n = self.modify(&id, now, None, |d| {
                    let h = hash_mut(d)?;
                    Ok(a[1..].iter().filter(|f| h.remove(f)).count())
                })?;
                Ok(Reply::count(n.unwrap_or(0)))
            }
            "HINCRBY" => {
                let id = self.key(s, &a[0])?;
                let by = int(&a[2])?;
                let r = self.modify(&id, now, Some(Data::empty(Kind::Hash)), |d| {
                    let h = hash_mut(d)?;
                    let cur = match h.map.get(&a[1]) {
                        Some(v) => int(v)?,
                        None => 0,
                    };
                    let next = cur.checked_add(by).ok_or(KvError::Overflow)?;
                    h.set(a[1].clone(), next.to_string().into_bytes());
                    Ok(next)
                })?;
                Ok(Reply::Int(r.unwrap_or(0)))
            }
            // lists
            "LPUSH" | "RPUSH" => {
                let id = self.key(s, &a[0])?;
                self.check_values(&a[1..])?;
                let front = name == "LPUSH";
                let n = self.modify(&id, now, Some(Data::empty(Kind::List)), |d| {
                    let l = list_mut(d)?;
                    for v in &a[1..] {
                        if front {
                            l.push_front(v.clone())
                        } else {
                            l.push_back(v.clone())
                        }
                    }
                    Ok(l.items.len())
                })?;
                Ok(Reply::count(n.unwrap_or(0)))
            }
            "LPOP" | "RPOP" => {
                let id = self.key(s, &a[0])?;
                let count = a.get(1).map(|c| uint(c)).transpose()?;
                let front = name == "LPOP";
                let r = self.modify(&id, now, None, |d| {
                    let l = list_mut(d)?;
                    let pop = |l: &mut ListV| if front { l.pop_front() } else { l.pop_back() };
                    Ok(match count {
                        None => opt_bulk(pop(l)),
                        Some(n) => {
                            let mut out = Vec::new();
                            for _ in 0..n {
                                match pop(l) {
                                    Some(v) => out.push(v),
                                    None => break,
                                }
                            }
                            bulks(out)
                        }
                    })
                })?;
                Ok(r.unwrap_or(Reply::Nil))
            }
            "LLEN" => {
                let id = self.key(s, &a[0])?;
                Ok(Reply::count(self.list_ref(&id, now)?.map_or(0, |l| l.items.len())))
            }
            "LRANGE" => {
                let id = self.key(s, &a[0])?;
                let (st, en) = (int(&a[1])?, int(&a[2])?);
                let out = match self.list_ref(&id, now)? {
                    Some(l) => norm_range(st, en, l.items.len())
                        .map(|(b, e)| l.items.range(b..=e).cloned().collect::<Vec<_>>())
                        .unwrap_or_default(),
                    None => Vec::new(),
                };
                Ok(bulks(out))
            }
            "LINDEX" => {
                let id = self.key(s, &a[0])?;
                let i = int(&a[1])?;
                Ok(opt_bulk(self.list_ref(&id, now)?.and_then(|l| {
                    let len = i64::try_from(l.items.len()).ok()?;
                    let idx = if i < 0 { len + i } else { i };
                    l.items.get(usize::try_from(idx).ok()?).cloned()
                })))
            }
            "LSET" => {
                let id = self.key(s, &a[0])?;
                let i = int(&a[1])?;
                self.check_value(&a[2])?;
                let r = self.modify(&id, now, None, |d| {
                    let l = list_mut(d)?;
                    let len = i64::try_from(l.items.len()).unwrap_or(i64::MAX);
                    let idx = if i < 0 { len + i } else { i };
                    let idx = usize::try_from(idx)
                        .ok()
                        .filter(|n| *n < l.items.len())
                        .ok_or_else(|| KvError::OutOfRange("index out of range".into()))?;
                    l.set(idx, a[2].clone());
                    Ok(())
                })?;
                r.map(|()| Reply::ok())
                    .ok_or_else(|| KvError::OutOfRange("no such key".into()))
            }
            "LREM" => {
                let id = self.key(s, &a[0])?;
                let count = int(&a[1])?;
                let r = self.modify(&id, now, None, |d| {
                    let l = list_mut(d)?;
                    let mut limit = if count == 0 {
                        usize::MAX
                    } else {
                        count.unsigned_abs() as usize
                    };
                    let mut removed = 0usize;
                    let mut items: Vec<Vec<u8>> = l.items.drain(..).collect();
                    if count >= 0 {
                        items.retain(|v| {
                            if limit > 0 && *v == a[2] {
                                limit -= 1;
                                removed += 1;
                                false
                            } else {
                                true
                            }
                        });
                    } else {
                        items.reverse();
                        items.retain(|v| {
                            if limit > 0 && *v == a[2] {
                                limit -= 1;
                                removed += 1;
                                false
                            } else {
                                true
                            }
                        });
                        items.reverse();
                    }
                    l.replace(items.into_iter().collect());
                    Ok(removed)
                })?;
                Ok(Reply::count(r.unwrap_or(0)))
            }
            "LTRIM" => {
                let id = self.key(s, &a[0])?;
                let (st, en) = (int(&a[1])?, int(&a[2])?);
                self.modify(&id, now, None, |d| {
                    let l = list_mut(d)?;
                    let kept: VecDeque<Vec<u8>> = match norm_range(st, en, l.items.len()) {
                        Some((b, e)) => l.items.range(b..=e).cloned().collect(),
                        None => VecDeque::new(),
                    };
                    l.replace(kept);
                    Ok(())
                })?;
                Ok(Reply::ok())
            }
            // sets
            "SADD" => {
                let id = self.key(s, &a[0])?;
                self.check_values(&a[1..])?;
                let n = self.modify(&id, now, Some(Data::empty(Kind::Set)), |d| {
                    let set = set_mut(d)?;
                    Ok(a[1..].iter().filter(|m| set.insert((*m).clone())).count())
                })?;
                Ok(Reply::count(n.unwrap_or(0)))
            }
            "SREM" => {
                let id = self.key(s, &a[0])?;
                let n = self.modify(&id, now, None, |d| {
                    let set = set_mut(d)?;
                    Ok(a[1..].iter().filter(|m| set.remove(m)).count())
                })?;
                Ok(Reply::count(n.unwrap_or(0)))
            }
            "SISMEMBER" => {
                let id = self.key(s, &a[0])?;
                Ok(Reply::Int(i64::from(
                    self.set_ref(&id, now)?.is_some_and(|x| x.set.contains(&a[1])),
                )))
            }
            "SMEMBERS" => {
                let id = self.key(s, &a[0])?;
                Ok(bulks(
                    self.set_ref(&id, now)?
                        .map(|x| x.set.iter().cloned().collect::<Vec<_>>())
                        .unwrap_or_default(),
                ))
            }
            "SCARD" => {
                let id = self.key(s, &a[0])?;
                Ok(Reply::count(self.set_ref(&id, now)?.map_or(0, |x| x.set.len())))
            }
            "SPOP" => {
                let id = self.key(s, &a[0])?;
                let count = a.get(1).map(|c| uint(c)).transpose()?;
                let mut seed = now ^ (self.stats.hits.wrapping_mul(0x9E37_79B9_7F4A_7C15));
                let r = self.modify(&id, now, None, |d| {
                    let set = set_mut(d)?;
                    let mut pop = |set: &mut SetV| {
                        if set.set.is_empty() {
                            return None;
                        }
                        seed ^= seed << 13;
                        seed ^= seed >> 7;
                        seed ^= seed << 17;
                        let idx = (seed % set.set.len() as u64) as usize;
                        let m = set.set.iter().nth(idx).cloned()?;
                        set.remove(&m);
                        Some(m)
                    };
                    Ok(match count {
                        None => opt_bulk(pop(set)),
                        Some(n) => {
                            let mut out = Vec::new();
                            for _ in 0..n {
                                match pop(set) {
                                    Some(m) => out.push(m),
                                    None => break,
                                }
                            }
                            bulks(out)
                        }
                    })
                })?;
                Ok(r.unwrap_or(Reply::Nil))
            }
            "SUNION" | "SINTER" | "SDIFF" => {
                let mut sets: Vec<BTreeSet<Vec<u8>>> = Vec::new();
                for k in a {
                    let id = self.key(s, k)?;
                    sets.push(self.set_ref(&id, now)?.map(|x| x.set.clone()).unwrap_or_default());
                }
                let mut it = sets.into_iter();
                let first = it.next().unwrap_or_default();
                let out: BTreeSet<Vec<u8>> = match name {
                    "SUNION" => it.fold(first, |mut acc, x| {
                        acc.extend(x);
                        acc
                    }),
                    "SINTER" => it.fold(first, |acc, x| acc.intersection(&x).cloned().collect()),
                    _ => it.fold(first, |acc, x| acc.difference(&x).cloned().collect()),
                };
                Ok(bulks(out))
            }
            // sorted sets
            "ZADD" => self.zadd(s, a, now),
            "ZREM" => {
                let id = self.key(s, &a[0])?;
                let n = self.modify(&id, now, None, |d| {
                    let z = zset_mut(d)?;
                    Ok(a[1..].iter().filter(|m| z.remove(m)).count())
                })?;
                Ok(Reply::count(n.unwrap_or(0)))
            }
            "ZSCORE" => {
                let id = self.key(s, &a[0])?;
                Ok(self
                    .zset_ref(&id, now)?
                    .and_then(|z| z.scores.get(&a[1]).copied())
                    .map_or(Reply::Nil, Reply::double))
            }
            "ZRANK" | "ZREVRANK" => {
                let id = self.key(s, &a[0])?;
                let rev = name == "ZREVRANK";
                Ok(match self.zset_ref(&id, now)? {
                    Some(z) if z.scores.contains_key(&a[1]) => {
                        let pos = if rev {
                            z.order.iter().rev().position(|(_, m)| *m == a[1])
                        } else {
                            z.order.iter().position(|(_, m)| *m == a[1])
                        };
                        pos.map_or(Reply::Nil, Reply::count)
                    }
                    _ => Reply::Nil,
                })
            }
            "ZRANGE" | "ZREVRANGE" => {
                let id = self.key(s, &a[0])?;
                let (st, en) = (int(&a[1])?, int(&a[2])?);
                let mut rev = name == "ZREVRANGE";
                let mut with = false;
                for o in &a[3..] {
                    match upper(o).as_str() {
                        "WITHSCORES" => with = true,
                        "REV" if name == "ZRANGE" => rev = true,
                        _ => return Err(KvError::Syntax("syntax error".into())),
                    }
                }
                let items: Vec<(Vec<u8>, f64)> = match self.zset_ref(&id, now)? {
                    Some(z) => match norm_range(st, en, z.order.len()) {
                        Some((b, e)) => {
                            let it = z.order.iter().map(|(sc, m)| (m.clone(), sc.0));
                            if rev {
                                it.rev().skip(b).take(e - b + 1).collect()
                            } else {
                                it.skip(b).take(e - b + 1).collect()
                            }
                        }
                        None => Vec::new(),
                    },
                    None => Vec::new(),
                };
                Ok(scored(items, with))
            }
            "ZRANGEBYSCORE" => {
                let id = self.key(s, &a[0])?;
                let (min, max) = (score_bound(&a[1])?, score_bound(&a[2])?);
                let mut with = false;
                let mut limit: Option<(usize, usize)> = None;
                let mut i = 3;
                while i < a.len() {
                    match upper(&a[i]).as_str() {
                        "WITHSCORES" => with = true,
                        "LIMIT" if i + 2 < a.len() => {
                            limit = Some((
                                usize::try_from(uint(&a[i + 1])?).unwrap_or(usize::MAX),
                                usize::try_from(uint(&a[i + 2])?).unwrap_or(usize::MAX),
                            ));
                            i += 2;
                        }
                        _ => return Err(KvError::Syntax("syntax error".into())),
                    }
                    i += 1;
                }
                let items: Vec<(Vec<u8>, f64)> = self
                    .zset_ref(&id, now)?
                    .map(|z| {
                        let (off, cnt) = limit.unwrap_or((0, usize::MAX));
                        z.order
                            .iter()
                            .filter(|(sc, _)| min.admits_low(sc.0) && max.admits_high(sc.0))
                            .skip(off)
                            .take(cnt)
                            .map(|(sc, m)| (m.clone(), sc.0))
                            .collect()
                    })
                    .unwrap_or_default();
                Ok(scored(items, with))
            }
            "ZINCRBY" => {
                let id = self.key(s, &a[0])?;
                let by = float(&a[1])?;
                self.check_value(&a[2])?;
                let r = self.modify(&id, now, Some(Data::empty(Kind::ZSet)), |d| {
                    let z = zset_mut(d)?;
                    let cur = z.scores.get(&a[2]).copied().unwrap_or(0.0);
                    let next = cur + by;
                    if next.is_nan() {
                        return Err(KvError::NotFloat);
                    }
                    z.set(a[2].clone(), next);
                    Ok(next)
                })?;
                Ok(Reply::double(r.unwrap_or(0.0)))
            }
            "ZCARD" => {
                let id = self.key(s, &a[0])?;
                Ok(Reply::count(self.zset_ref(&id, now)?.map_or(0, |z| z.scores.len())))
            }
            "ZCOUNT" => {
                let id = self.key(s, &a[0])?;
                let (min, max) = (score_bound(&a[1])?, score_bound(&a[2])?);
                Ok(Reply::count(self.zset_ref(&id, now)?.map_or(0, |z| {
                    z.order
                        .iter()
                        .filter(|(sc, _)| min.admits_low(sc.0) && max.admits_high(sc.0))
                        .count()
                })))
            }
            "ZPOPMIN" | "ZPOPMAX" => {
                let id = self.key(s, &a[0])?;
                let count = match a.get(1) {
                    Some(c) => usize::try_from(uint(c)?).unwrap_or(usize::MAX),
                    None => 1,
                };
                let max = name == "ZPOPMAX";
                let r = self.modify(&id, now, None, |d| {
                    let z = zset_mut(d)?;
                    let mut out = Vec::new();
                    for _ in 0..count {
                        let next = if max { z.order.last() } else { z.order.first() }.cloned();
                        match next {
                            Some((sc, m)) => {
                                z.remove(&m);
                                out.push((m, sc.0));
                            }
                            None => break,
                        }
                    }
                    Ok(out)
                })?;
                Ok(scored(r.unwrap_or_default(), true))
            }
            // streams
            "XADD" => self.xadd(s, a, now),
            "XLEN" => {
                let id = self.key(s, &a[0])?;
                Ok(Reply::count(self.stream_ref(&id, now)?.map_or(0, |x| x.entries.len())))
            }
            "XRANGE" | "XREVRANGE" => {
                let id = self.key(s, &a[0])?;
                let rev = name == "XREVRANGE";
                let (lo_raw, hi_raw) = if rev { (&a[2], &a[1]) } else { (&a[1], &a[2]) };
                let lo = parse_range_id(lo_raw, true)?;
                let hi = parse_range_id(hi_raw, false)?;
                let mut count = usize::MAX;
                if a.len() > 3 {
                    if a.len() != 5 || upper(&a[3]) != "COUNT" {
                        return Err(KvError::Syntax("syntax error".into()));
                    }
                    count = usize::try_from(uint(&a[4])?).unwrap_or(usize::MAX);
                }
                let entries: Vec<(StreamId, Fields)> = self
                    .stream_ref(&id, now)?
                    .map(|x| {
                        if lo > hi {
                            return Vec::new();
                        }
                        let r = x.entries.range(lo..=hi).map(|(i, f)| (*i, f.clone()));
                        if rev {
                            r.rev().take(count).collect()
                        } else {
                            r.take(count).collect()
                        }
                    })
                    .unwrap_or_default();
                Ok(entries_reply(entries))
            }
            "XREAD" => self.xread(s, a, now),
            "XDEL" => {
                let id = self.key(s, &a[0])?;
                let ids = a[1..].iter().map(|i| parse_id(i)).collect::<KvResult<Vec<_>>>()?;
                let n = self.modify(&id, now, None, |d| {
                    let x = stream_mut(d)?;
                    Ok(ids.iter().filter(|i| x.remove(i)).count())
                })?;
                Ok(Reply::count(n.unwrap_or(0)))
            }
            "XTRIM" => {
                let id = self.key(s, &a[0])?;
                if upper(&a[1]) != "MAXLEN" {
                    return Err(KvError::Syntax("only MAXLEN is supported".into()));
                }
                let n = usize::try_from(uint(a.last().unwrap_or(&a[1]))?).unwrap_or(usize::MAX);
                let r = self.modify(&id, now, None, |d| Ok(stream_mut(d)?.trim(n)))?;
                Ok(Reply::count(r.unwrap_or(0)))
            }
            // PUBLISH never reaches the store: the service answers it.
            _ => Err(KvError::UnknownCommand(name.to_ascii_lowercase())),
        }
    }

    fn del(&mut self, s: &Scope, keys: &[Vec<u8>], now: u64) -> KvResult<Reply> {
        let mut n = 0;
        for k in keys {
            let id = self.key(s, k)?;
            n += usize::from(self.delete(&id, now));
        }
        Ok(Reply::count(n))
    }

    fn expire(&mut self, s: &Scope, a: &[Vec<u8>], now: u64, at: impl Fn(i64, u64) -> Option<u64>) -> KvResult<Reply> {
        let id = self.key(s, &a[0])?;
        let v = int(&a[1])?;
        if v <= 0 && !matches!(at(v, now), Some(t) if t > now) {
            // A TTL in the past deletes the key, as in Redis.
            return Ok(Reply::Int(i64::from(self.delete(&id, now))));
        }
        let t = at(v, now).ok_or_else(|| KvError::Syntax("invalid expire time".into()))?;
        if t <= now {
            return Ok(Reply::Int(i64::from(self.delete(&id, now))));
        }
        Ok(Reply::Int(i64::from(self.set_expiry(&id, Some(t), now))))
    }

    fn keys(&mut self, s: &Scope, pattern: &str, now: u64) -> KvResult<Reply> {
        let limit = self.cfg.keys_limit;
        let mut out = Vec::new();
        for (id, _) in self.scan_scope(s, None, now) {
            if glob::matches(pattern, &id.key) {
                if out.len() >= limit {
                    return Err(KvError::TooManyKeys(limit));
                }
                out.push(id.key.clone().into_bytes());
            }
        }
        Ok(bulks(out))
    }

    fn scan(&mut self, s: &Scope, a: &[Vec<u8>], now: u64) -> KvResult<Reply> {
        let cursor = text(&a[0])?;
        let after = decode_cursor(cursor)?;
        let (mut pattern, mut count, mut kind) = (None, 100usize, None);
        let mut i = 1;
        while i < a.len() {
            let v = a.get(i + 1).ok_or_else(|| KvError::Syntax("syntax error".into()))?;
            match upper(&a[i]).as_str() {
                "MATCH" => pattern = Some(text(v)?.to_owned()),
                "COUNT" => {
                    count = usize::try_from(uint(v)?).unwrap_or(usize::MAX).clamp(1, 1000);
                }
                "TYPE" => {
                    kind = Some(
                        Kind::from_name(&text(v)?.to_ascii_lowercase())
                            .ok_or_else(|| KvError::Syntax("unknown type".into()))?,
                    );
                }
                _ => return Err(KvError::Syntax("syntax error".into())),
            }
            i += 2;
        }
        // `count` keys are examined per call, as in Redis; matches may be fewer.
        let mut last: Option<String> = None;
        let mut out = Vec::new();
        let mut more = false;
        for (examined, (id, e)) in self.scan_scope(s, after.as_deref(), now).enumerate() {
            if examined == count {
                more = true;
                break;
            }
            last = Some(id.key.clone());
            let kind_ok = kind.is_none_or(|k| e.data.kind() == k);
            if kind_ok && pattern.as_deref().is_none_or(|p| glob::matches(p, &id.key)) {
                out.push(Reply::Bulk(id.key.clone().into_bytes()));
            }
        }
        let next = match (more, last) {
            (true, Some(k)) => encode_cursor(&k),
            _ => "0".to_owned(),
        };
        Ok(Reply::Array(vec![Reply::bulk(next), Reply::Array(out)]))
    }

    fn set(&mut self, s: &Scope, a: &[Vec<u8>], now: u64) -> KvResult<Reply> {
        let id = self.key(s, &a[0])?;
        self.check_value(&a[1])?;
        let (mut nx, mut xx, mut get, mut keep) = (false, false, false, false);
        let mut expire: Option<u64> = None;
        let mut i = 2;
        while i < a.len() {
            let opt = upper(&a[i]);
            match opt.as_str() {
                "NX" => nx = true,
                "XX" => xx = true,
                "GET" => get = true,
                "KEEPTTL" => keep = true,
                "EX" | "PX" | "EXAT" | "PXAT" => {
                    let v = int(a.get(i + 1).ok_or_else(|| KvError::Syntax("syntax error".into()))?)?;
                    let bad = || KvError::Syntax("invalid expire time".into());
                    if v <= 0 {
                        return Err(bad());
                    }
                    let t = match opt.as_str() {
                        "EX" => v.checked_mul(1000).map(|ms| now_plus(now, ms)),
                        "PX" => Some(now_plus(now, v)),
                        "EXAT" => v.checked_mul(1000).and_then(|m| u64::try_from(m).ok()),
                        _ => u64::try_from(v).ok(),
                    };
                    if expire.is_some() {
                        return Err(KvError::Syntax("syntax error".into()));
                    }
                    expire = Some(t.ok_or_else(bad)?);
                    i += 1;
                }
                _ => return Err(KvError::Syntax("syntax error".into())),
            }
            i += 1;
        }
        if (nx && xx) || (keep && expire.is_some()) {
            return Err(KvError::Syntax("syntax error".into()));
        }
        let old = if get { self.str_ref(&id, now)?.cloned() } else { None };
        let exists = self.live(&id, now);
        if (nx && exists) || (xx && !exists) {
            return Ok(if get { opt_bulk(old) } else { Reply::Nil });
        }
        if expire.is_some_and(|t| t <= now) {
            // EXAT/PXAT in the past: the key ends up deleted.
            self.delete(&id, now);
            return Ok(if get { opt_bulk(old) } else { Reply::ok() });
        }
        self.replace(&id, Data::Str(a[1].clone()), expire, keep, now);
        Ok(if get { opt_bulk(old) } else { Reply::ok() })
    }

    fn incr(&mut self, s: &Scope, k: &[u8], by: i64, now: u64) -> KvResult<Reply> {
        let id = self.key(s, k)?;
        let r = self.modify(&id, now, Some(Data::Str(b"0".to_vec())), |d| {
            let v = str_mut(d)?;
            let next = int(v)?.checked_add(by).ok_or(KvError::Overflow)?;
            *v = next.to_string().into_bytes();
            Ok(next)
        })?;
        Ok(Reply::Int(r.unwrap_or(0)))
    }

    fn zadd(&mut self, s: &Scope, a: &[Vec<u8>], now: u64) -> KvResult<Reply> {
        let id = self.key(s, &a[0])?;
        let (mut nx, mut xx, mut ch) = (false, false, false);
        let mut i = 1;
        while i < a.len() {
            match upper(&a[i]).as_str() {
                "NX" => nx = true,
                "XX" => xx = true,
                "CH" => ch = true,
                _ => break,
            }
            i += 1;
        }
        let pairs = &a[i..];
        if pairs.is_empty() || !pairs.len().is_multiple_of(2) || (nx && xx) {
            return Err(KvError::Syntax("syntax error".into()));
        }
        let mut items = Vec::new();
        for p in pairs.chunks(2) {
            self.check_value(&p[1])?;
            items.push((float(&p[0])?, p[1].clone()));
        }
        let n = self.modify(&id, now, Some(Data::empty(Kind::ZSet)), |d| {
            let z = zset_mut(d)?;
            let mut changed = 0usize;
            for (score, member) in items {
                let prev = z.scores.get(&member).copied();
                match prev {
                    Some(_) if nx => continue,
                    None if xx => continue,
                    Some(p) if p == score => continue,
                    _ => {}
                }
                let new = z.set(member, score);
                if new || ch {
                    changed += 1;
                }
            }
            Ok(changed)
        })?;
        Ok(Reply::count(n.unwrap_or(0)))
    }

    fn xadd(&mut self, s: &Scope, a: &[Vec<u8>], now: u64) -> KvResult<Reply> {
        let id = self.key(s, &a[0])?;
        let mut i = 1;
        let mut maxlen: Option<usize> = None;
        if upper(&a[i]) == "MAXLEN" {
            i += 1;
            if a.get(i).is_some_and(|x| x == b"~" || x == b"=") {
                i += 1;
            }
            let v = a.get(i).ok_or_else(|| KvError::Syntax("syntax error".into()))?;
            maxlen = Some(usize::try_from(uint(v)?).unwrap_or(usize::MAX));
            i += 1;
        }
        let id_arg = a.get(i).ok_or_else(|| KvError::Syntax("syntax error".into()))?;
        let rest = &a[i + 1..];
        if rest.is_empty() || !rest.len().is_multiple_of(2) {
            return Err(KvError::WrongArity("xadd".into()));
        }
        self.check_values(rest)?;
        let fields: Fields = rest.chunks(2).map(|p| (p[0].clone(), p[1].clone())).collect();
        let auto = id_arg == b"*";
        let explicit = if auto { None } else { Some(parse_id(id_arg)?) };
        let r = self.modify(&id, now, Some(Data::empty(Kind::Stream)), |d| {
            let x = stream_mut(d)?;
            let new = match explicit {
                Some(e) => {
                    if e == StreamId::MIN {
                        return Err(KvError::Syntax("the stream id must be greater than 0-0".into()));
                    }
                    if e <= x.last && !(x.entries.is_empty() && x.last == StreamId::MIN) {
                        return Err(KvError::Syntax(
                            "the stream id is equal or smaller than the top item".into(),
                        ));
                    }
                    e
                }
                None => {
                    if now > x.last.ms {
                        StreamId { ms: now, seq: 0 }
                    } else {
                        x.last
                            .next()
                            .ok_or_else(|| KvError::Syntax("the stream is full".into()))?
                    }
                }
            };
            x.insert(new, fields);
            if let Some(m) = maxlen {
                x.trim(m);
            }
            Ok(new)
        })?;
        Ok(Reply::bulk(r.unwrap_or_default().render()))
    }

    fn xread(&mut self, s: &Scope, a: &[Vec<u8>], now: u64) -> KvResult<Reply> {
        let mut i = 0;
        let mut count = usize::MAX;
        loop {
            match a.get(i).map(|x| upper(x)).as_deref() {
                Some("COUNT") => {
                    count = usize::try_from(uint(
                        a.get(i + 1).ok_or_else(|| KvError::Syntax("syntax error".into()))?,
                    )?)
                    .unwrap_or(usize::MAX);
                    i += 2;
                }
                Some("STREAMS") => {
                    i += 1;
                    break;
                }
                _ => return Err(KvError::Syntax("syntax error".into())),
            }
        }
        let rest = &a[i..];
        if rest.is_empty() || !rest.len().is_multiple_of(2) {
            return Err(KvError::Syntax(
                "unbalanced list of streams: give one id per key".into(),
            ));
        }
        let (keys, ids) = rest.split_at(rest.len() / 2);
        let mut out = Vec::new();
        for (k, idarg) in keys.iter().zip(ids) {
            let id = self.key(s, k)?;
            let Some(x) = self.stream_ref(&id, now)? else { continue };
            let after = if idarg == b"$" { x.last } else { parse_id(idarg)? };
            let entries: Vec<(StreamId, Fields)> = match after.next() {
                Some(from) => x
                    .entries
                    .range(from..)
                    .take(count)
                    .map(|(i, f)| (*i, f.clone()))
                    .collect(),
                None => Vec::new(),
            };
            if !entries.is_empty() {
                out.push(Reply::Array(vec![Reply::bulk(id.key.clone()), entries_reply(entries)]));
            }
        }
        Ok(if out.is_empty() { Reply::Nil } else { Reply::Array(out) })
    }
}

fn now_plus(now: u64, ms: i64) -> u64 {
    now.saturating_add(u64::try_from(ms).unwrap_or(0))
}

fn scored(items: Vec<(Vec<u8>, f64)>, with: bool) -> Reply {
    let mut out = Vec::with_capacity(items.len() * if with { 2 } else { 1 });
    for (m, sc) in items {
        out.push(Reply::Bulk(m));
        if with {
            out.push(Reply::double(sc));
        }
    }
    Reply::Array(out)
}

fn entries_reply(entries: Vec<(StreamId, Fields)>) -> Reply {
    Reply::Array(
        entries
            .into_iter()
            .map(|(id, f)| {
                Reply::Array(vec![
                    Reply::bulk(id.render()),
                    Reply::Array(
                        f.into_iter()
                            .flat_map(|(k, v)| [Reply::Bulk(k), Reply::Bulk(v)])
                            .collect(),
                    ),
                ])
            })
            .collect(),
    )
}

/// `ms` or `ms-seq`.
fn parse_id(b: &[u8]) -> KvResult<StreamId> {
    let bad = || KvError::Syntax("invalid stream id".into());
    let t = text(b).map_err(|_| bad())?;
    let (ms, seq) = match t.split_once('-') {
        Some((m, s)) => (m, s),
        None => (t, "0"),
    };
    Ok(StreamId {
        ms: ms.parse().map_err(|_| bad())?,
        seq: seq.parse().map_err(|_| bad())?,
    })
}

/// Range bound: `-`, `+`, `ms` (low = `ms-0`, high = `ms-MAX`) or `ms-seq`.
fn parse_range_id(b: &[u8], low: bool) -> KvResult<StreamId> {
    match b {
        b"-" => Ok(StreamId::MIN),
        b"+" => Ok(StreamId::MAX),
        _ => {
            let t = text(b).map_err(|_| KvError::Syntax("invalid stream id".into()))?;
            if t.contains('-') {
                parse_id(b)
            } else {
                let ms = t.parse().map_err(|_| KvError::Syntax("invalid stream id".into()))?;
                Ok(StreamId {
                    ms,
                    seq: if low { 0 } else { u64::MAX },
                })
            }
        }
    }
}

/// A sorted-set score bound: number, `-inf`, `+inf`, or `(number` (exclusive).
#[derive(Debug, Clone, Copy)]
struct Bound {
    v: f64,
    exclusive: bool,
}

impl Bound {
    fn admits_low(self, score: f64) -> bool {
        if self.exclusive {
            score > self.v
        } else {
            score >= self.v
        }
    }

    fn admits_high(self, score: f64) -> bool {
        if self.exclusive {
            score < self.v
        } else {
            score <= self.v
        }
    }
}

fn score_bound(b: &[u8]) -> KvResult<Bound> {
    let (exclusive, rest) = match b.split_first() {
        Some((b'(', rest)) => (true, rest),
        _ => (false, b),
    };
    Ok(Bound {
        v: float(rest).map_err(|_| KvError::Syntax("min or max is not a float".into()))?,
        exclusive,
    })
}

fn encode_cursor(key: &str) -> String {
    let mut s = String::from("k");
    for b in key.as_bytes() {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn decode_cursor(c: &str) -> KvResult<Option<String>> {
    if c == "0" {
        return Ok(None);
    }
    let bad = || KvError::Syntax("invalid cursor".into());
    let hex = c.strip_prefix('k').ok_or_else(bad)?;
    if !hex.len().is_multiple_of(2) {
        return Err(bad());
    }
    let bytes: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|_| bad()))
        .collect::<KvResult<_>>()?;
    String::from_utf8(bytes).map(Some).map_err(|_| bad())
}
