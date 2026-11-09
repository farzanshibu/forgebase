//! Engine tests: every data type, TTL, LRU, scans, accounting. They drive the
//! store with an explicit clock, so nothing sleeps.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::config::{EvictionPolicy, KvConfig};
use crate::error::KvError;
use crate::reply::Reply;
use crate::store::{Scope, Store};

const T0: u64 = 1_000_000;

struct Db {
    store: Store,
    scope: Scope,
    now: u64,
}

fn scope(tenant: &str, ns: &str) -> Scope {
    Scope {
        tenant: tenant.into(),
        ns: ns.into(),
    }
}

fn db() -> Db {
    db_with(KvConfig::default())
}

fn db_with(cfg: KvConfig) -> Db {
    Db {
        store: Store::new(cfg),
        scope: scope("t:a", "main"),
        now: T0,
    }
}

impl Db {
    fn run(&mut self, line: &str) -> Result<Reply, KvError> {
        let args: Vec<Vec<u8>> = line.split_whitespace().map(|s| s.as_bytes().to_vec()).collect();
        self.store.exec(&self.scope, &args, self.now)
    }

    fn ok(&mut self, line: &str) -> Reply {
        self.run(line).unwrap_or_else(|e| panic!("{line}: {e}"))
    }

    fn err(&mut self, line: &str) -> KvError {
        self.run(line).expect_err(line)
    }

    fn advance(&mut self, ms: u64) {
        self.now += ms;
    }
}

fn b(s: &str) -> Reply {
    Reply::bulk(s)
}

fn arr(items: &[&str]) -> Reply {
    Reply::Array(items.iter().map(|s| b(s)).collect())
}

fn int(i: i64) -> Reply {
    Reply::Int(i)
}

#[test]
fn strings_basic_and_options() {
    let mut d = db();
    assert_eq!(d.ok("GET k"), Reply::Nil);
    assert_eq!(d.ok("SET k v"), Reply::ok());
    assert_eq!(d.ok("GET k"), b("v"));
    assert_eq!(d.ok("STRLEN k"), int(1));
    // NX / XX
    assert_eq!(d.ok("SET k other NX"), Reply::Nil);
    assert_eq!(d.ok("GET k"), b("v"));
    assert_eq!(d.ok("SET missing x XX"), Reply::Nil);
    assert_eq!(d.ok("GET missing"), Reply::Nil);
    assert_eq!(d.ok("SET k w XX"), Reply::ok());
    // GET returns the old value
    assert_eq!(d.ok("SET k z GET"), b("w"));
    assert_eq!(d.ok("SETNX k q"), int(0));
    assert_eq!(d.ok("SETNX fresh q"), int(1));
    assert!(matches!(d.err("SET k v NX XX"), KvError::Syntax(_)));
    assert!(matches!(d.err("SET k v EX 0"), KvError::Syntax(_)));
    // append / getdel
    assert_eq!(d.ok("APPEND log ab"), int(2));
    assert_eq!(d.ok("APPEND log cd"), int(4));
    assert_eq!(d.ok("GET log"), b("abcd"));
    assert_eq!(d.ok("GETDEL log"), b("abcd"));
    assert_eq!(d.ok("GET log"), Reply::Nil);
}

#[test]
fn strings_are_binary_safe() {
    let mut d = db();
    let args = vec![b"SET".to_vec(), b"bin".to_vec(), vec![0, 159, 255]];
    d.store.exec(&d.scope, &args, T0).unwrap();
    let got = d.store.exec(&d.scope, &[b"GET".to_vec(), b"bin".to_vec()], T0).unwrap();
    assert_eq!(got, Reply::Bulk(vec![0, 159, 255]));
}

#[test]
fn mset_mget_and_counters() {
    let mut d = db();
    assert_eq!(d.ok("MSET a 1 b 2 c x"), Reply::ok());
    assert_eq!(
        d.ok("MGET a b nope c"),
        Reply::Array(vec![b("1"), b("2"), Reply::Nil, b("x")])
    );
    assert_eq!(d.ok("INCR a"), int(2));
    assert_eq!(d.ok("INCRBY a 10"), int(12));
    assert_eq!(d.ok("DECR a"), int(11));
    assert_eq!(d.ok("DECRBY a 11"), int(0));
    assert_eq!(d.ok("INCR fresh"), int(1));
    assert!(matches!(d.err("INCR c"), KvError::NotInteger));
    d.ok("SET big 9223372036854775807");
    assert!(matches!(d.err("INCR big"), KvError::Overflow));
    assert_eq!(
        d.ok("GET big"),
        b("9223372036854775807"),
        "failed INCR leaves the value"
    );
    assert_eq!(d.ok("INCRBYFLOAT f 1.5"), b("1.5"));
    assert_eq!(d.ok("INCRBYFLOAT f 1.5"), b("3"));
    assert!(matches!(d.err("MSET odd"), KvError::WrongArity(_)));
    // a failed INCR on a missing key must not leave a key behind
    assert!(matches!(d.err("INCRBY nokey notanumber"), KvError::NotInteger));
    assert_eq!(d.ok("EXISTS nokey"), int(0));
}

#[test]
fn wrong_type_is_reported_and_changes_nothing() {
    let mut d = db();
    d.ok("SET s v");
    d.ok("HSET h f v");
    d.ok("RPUSH l a");
    assert!(matches!(d.err("HGET s f"), KvError::WrongType));
    assert!(matches!(d.err("GET h"), KvError::WrongType));
    assert!(matches!(d.err("LPUSH s x"), KvError::WrongType));
    assert!(matches!(d.err("SADD l x"), KvError::WrongType));
    assert!(matches!(d.err("ZADD h 1 m"), KvError::WrongType));
    assert!(matches!(d.err("XADD s * f v"), KvError::WrongType));
    assert_eq!(d.ok("GET s"), b("v"));
    assert_eq!(d.ok("TYPE h"), Reply::Status("hash".into()));
    assert_eq!(d.ok("TYPE nope"), Reply::Status("none".into()));
    // SET replaces any type
    d.ok("SET h now-a-string");
    assert_eq!(d.ok("TYPE h"), Reply::Status("string".into()));
}

#[test]
fn ttl_expire_persist_and_lazy_expiry() {
    let mut d = db();
    d.ok("SET k v");
    assert_eq!(d.ok("TTL k"), int(-1));
    assert_eq!(d.ok("TTL nope"), int(-2));
    assert_eq!(d.ok("EXPIRE k 10"), int(1));
    assert_eq!(d.ok("TTL k"), int(10));
    assert_eq!(d.ok("PTTL k"), int(10_000));
    d.advance(4_000);
    assert_eq!(d.ok("PTTL k"), int(6_000));
    assert_eq!(d.ok("PERSIST k"), int(1));
    assert_eq!(d.ok("PERSIST k"), int(0));
    assert_eq!(d.ok("TTL k"), int(-1));
    assert_eq!(d.ok("PEXPIRE k 500"), int(1));
    d.advance(499);
    assert_eq!(d.ok("GET k"), b("v"));
    d.advance(1);
    assert_eq!(d.ok("GET k"), Reply::Nil, "expired exactly at its deadline");
    assert_eq!(d.ok("EXISTS k"), int(0));
    assert_eq!(d.ok("EXPIRE nope 5"), int(0));
    // SET with EX / PX, and SET clears the TTL unless KEEPTTL
    d.ok("SET a 1 EX 2");
    d.ok("SET b 1 PX 1500");
    assert_eq!(d.ok("PTTL a"), int(2000));
    d.ok("SET a 2");
    assert_eq!(d.ok("TTL a"), int(-1));
    d.ok("SET b 2 KEEPTTL");
    assert_eq!(d.ok("PTTL b"), int(1500));
    // writes that modify keep the TTL
    d.ok("SET c 1 EX 100");
    d.ok("INCR c");
    d.ok("APPEND c 0");
    assert_eq!(d.ok("TTL c"), int(100));
    // a TTL in the past deletes
    assert_eq!(d.ok("EXPIRE c -1"), int(1));
    assert_eq!(d.ok("EXISTS c"), int(0));
    d.ok("SET e 1");
    assert_eq!(d.ok("PEXPIREAT e 1"), int(1));
    assert_eq!(d.ok("EXISTS e"), int(0));
    assert_eq!(d.ok("SETEX s 3 v"), Reply::ok());
    assert_eq!(d.ok("TTL s"), int(3));
    assert!(matches!(d.err("SETEX s 0 v"), KvError::Syntax(_)));
}

#[test]
fn sweeper_removes_expired_keys_and_accounting_returns_to_zero() {
    let mut d = db();
    for i in 0..50 {
        d.ok(&format!("SET k{i} value EX 5"));
    }
    d.ok("HSET h f v");
    d.ok("EXPIRE h 5");
    assert_eq!(d.store.key_count(), 51);
    assert_eq!(d.store.sweep_expired(T0 + 4_999, 1000), 0);
    assert_eq!(d.store.sweep_expired(T0 + 5_000, 1000), 51);
    assert_eq!(d.store.key_count(), 0);
    assert_eq!(d.store.used_memory(), 0, "no memory is left accounted for");
    assert_eq!(d.store.stats.expired, 51);
}

#[test]
fn hashes() {
    let mut d = db();
    assert_eq!(d.ok("HSET h a 1 b 2"), int(2));
    assert_eq!(d.ok("HSET h a 9 c 3"), int(1), "only new fields are counted");
    assert_eq!(d.ok("HGET h a"), b("9"));
    assert_eq!(d.ok("HGET h nope"), Reply::Nil);
    assert_eq!(d.ok("HMGET h a nope c"), Reply::Array(vec![b("9"), Reply::Nil, b("3")]));
    assert_eq!(d.ok("HLEN h"), int(3));
    assert_eq!(d.ok("HEXISTS h b"), int(1));
    assert_eq!(d.ok("HKEYS h"), arr(&["a", "b", "c"]));
    assert_eq!(d.ok("HVALS h"), arr(&["9", "2", "3"]));
    assert_eq!(d.ok("HGETALL h"), arr(&["a", "9", "b", "2", "c", "3"]));
    assert_eq!(d.ok("HSETNX h a 0"), int(0));
    assert_eq!(d.ok("HSETNX h d 4"), int(1));
    assert_eq!(d.ok("HINCRBY h n 5"), int(5));
    assert_eq!(d.ok("HINCRBY h n -2"), int(3));
    assert!(matches!(d.err("HINCRBY h a x"), KvError::NotInteger));
    assert_eq!(d.ok("HDEL h a b nope"), int(2));
    assert_eq!(d.ok("HLEN h"), int(3));
    d.ok("HDEL h c d n");
    assert_eq!(d.ok("EXISTS h"), int(0), "an empty hash disappears");
    assert_eq!(d.store.used_memory(), 0);
    assert!(matches!(d.err("HSET h a"), KvError::WrongArity(_)));
}

#[test]
fn lists() {
    let mut d = db();
    assert_eq!(d.ok("RPUSH l a b c"), int(3));
    assert_eq!(d.ok("LPUSH l z y"), int(5));
    assert_eq!(d.ok("LRANGE l 0 -1"), arr(&["y", "z", "a", "b", "c"]));
    assert_eq!(d.ok("LRANGE l 1 2"), arr(&["z", "a"]));
    assert_eq!(d.ok("LRANGE l -2 -1"), arr(&["b", "c"]));
    assert_eq!(d.ok("LRANGE l 10 20"), arr(&[]));
    assert_eq!(d.ok("LLEN l"), int(5));
    assert_eq!(d.ok("LINDEX l 0"), b("y"));
    assert_eq!(d.ok("LINDEX l -1"), b("c"));
    assert_eq!(d.ok("LINDEX l 9"), Reply::Nil);
    assert_eq!(d.ok("LPOP l"), b("y"));
    assert_eq!(d.ok("RPOP l"), b("c"));
    assert_eq!(d.ok("LPOP l 2"), arr(&["z", "a"]));
    assert_eq!(d.ok("LPOP l 5"), arr(&["b"]));
    assert_eq!(d.ok("LPOP l"), Reply::Nil);
    assert_eq!(d.ok("EXISTS l"), int(0));
    d.ok("RPUSH l a b a c a");
    assert_eq!(d.ok("LREM l 2 a"), int(2));
    assert_eq!(d.ok("LRANGE l 0 -1"), arr(&["b", "c", "a"]));
    d.ok("RPUSH l a a");
    assert_eq!(d.ok("LREM l -1 a"), int(1), "negative count removes from the tail");
    assert_eq!(d.ok("LREM l 0 a"), int(2));
    assert_eq!(d.ok("LRANGE l 0 -1"), arr(&["b", "c"]));
    assert_eq!(d.ok("LSET l 1 X"), Reply::ok());
    assert_eq!(d.ok("LINDEX l 1"), b("X"));
    assert!(matches!(d.err("LSET l 9 X"), KvError::OutOfRange(_)));
    assert!(matches!(d.err("LSET nokey 0 X"), KvError::OutOfRange(_)));
    d.ok("RPUSH l 1 2 3 4");
    d.ok("LTRIM l 1 3");
    assert_eq!(d.ok("LRANGE l 0 -1"), arr(&["X", "1", "2"]));
    d.ok("LTRIM l 5 9");
    assert_eq!(d.ok("EXISTS l"), int(0));
    assert_eq!(d.store.used_memory(), 0);
}

#[test]
fn sets() {
    let mut d = db();
    assert_eq!(d.ok("SADD s a b c a"), int(3));
    assert_eq!(d.ok("SCARD s"), int(3));
    assert_eq!(d.ok("SISMEMBER s a"), int(1));
    assert_eq!(d.ok("SISMEMBER s z"), int(0));
    assert_eq!(d.ok("SMEMBERS s"), arr(&["a", "b", "c"]));
    d.ok("SADD t b c d");
    assert_eq!(d.ok("SUNION s t"), arr(&["a", "b", "c", "d"]));
    assert_eq!(d.ok("SINTER s t"), arr(&["b", "c"]));
    assert_eq!(d.ok("SDIFF s t"), arr(&["a"]));
    assert_eq!(d.ok("SINTER s nope"), arr(&[]));
    assert_eq!(d.ok("SREM s a z"), int(1));
    let Reply::Bulk(p) = d.ok("SPOP s") else {
        panic!("expected a member")
    };
    assert!(p == b"b" || p == b"c");
    assert_eq!(d.ok("SCARD s"), int(1));
    let Reply::Array(rest) = d.ok("SPOP s 5") else {
        panic!("array")
    };
    assert_eq!(rest.len(), 1);
    assert_eq!(d.ok("EXISTS s"), int(0));
    assert_eq!(d.ok("SPOP s"), Reply::Nil);
}

#[test]
fn sorted_sets() {
    let mut d = db();
    assert_eq!(d.ok("ZADD z 3 c 1 a 2 b"), int(3));
    assert_eq!(d.ok("ZADD z 1.5 a"), int(0), "re-scoring is not an add");
    assert_eq!(
        d.ok("ZADD z CH 1.5 a 9 d"),
        int(1),
        "CH counts changes: a unchanged, d new"
    );
    assert_eq!(d.ok("ZCARD z"), int(4));
    assert_eq!(d.ok("ZSCORE z a"), b("1.5"));
    assert_eq!(d.ok("ZSCORE z nope"), Reply::Nil);
    assert_eq!(d.ok("ZRANGE z 0 -1"), arr(&["a", "b", "c", "d"]));
    assert_eq!(d.ok("ZRANGE z 0 1 WITHSCORES"), arr(&["a", "1.5", "b", "2"]));
    assert_eq!(d.ok("ZREVRANGE z 0 1"), arr(&["d", "c"]));
    assert_eq!(d.ok("ZRANGE z 0 0 REV"), arr(&["d"]));
    assert_eq!(d.ok("ZRANK z c"), int(2));
    assert_eq!(d.ok("ZREVRANK z c"), int(1));
    assert_eq!(d.ok("ZRANK z nope"), Reply::Nil);
    assert_eq!(d.ok("ZRANGEBYSCORE z 2 3"), arr(&["b", "c"]));
    assert_eq!(d.ok("ZRANGEBYSCORE z (2 +inf"), arr(&["c", "d"]));
    assert_eq!(d.ok("ZRANGEBYSCORE z -inf +inf LIMIT 1 2"), arr(&["b", "c"]));
    assert_eq!(d.ok("ZCOUNT z 2 9"), int(3));
    assert_eq!(d.ok("ZINCRBY z 10 a"), b("11.5"));
    assert_eq!(d.ok("ZINCRBY z 1 new"), b("1"));
    // ties order by member
    d.ok("ZADD t 1 b 1 a 1 c");
    assert_eq!(d.ok("ZRANGE t 0 -1"), arr(&["a", "b", "c"]));
    assert_eq!(d.ok("ZPOPMIN t"), arr(&["a", "1"]));
    assert_eq!(d.ok("ZPOPMAX t 5"), arr(&["c", "1", "b", "1"]));
    assert_eq!(d.ok("EXISTS t"), int(0));
    // NX / XX / infinities
    assert_eq!(d.ok("ZADD z NX 100 a"), int(0));
    assert_eq!(d.ok("ZSCORE z a"), b("11.5"));
    assert_eq!(d.ok("ZADD z XX 100 ghost"), int(0));
    assert_eq!(d.ok("ZSCORE z ghost"), Reply::Nil);
    assert_eq!(d.ok("ZADD inf +inf top -inf bottom"), int(2));
    assert_eq!(
        d.ok("ZRANGE inf 0 -1 WITHSCORES"),
        arr(&["bottom", "-inf", "top", "inf"])
    );
    assert_eq!(d.ok("ZREM z a b nope"), int(2));
    assert!(matches!(d.err("ZADD z nan m"), KvError::NotFloat));
    assert!(matches!(d.err("ZADD z 1"), KvError::WrongArity(_)));
    assert!(matches!(d.err("ZADD z 1 a 2"), KvError::Syntax(_)));
}

#[test]
fn streams() {
    let mut d = db();
    let Reply::Bulk(first) = d.ok("XADD s * a 1") else {
        panic!("id")
    };
    assert_eq!(first, format!("{T0}-0").into_bytes());
    let Reply::Bulk(second) = d.ok("XADD s * a 2") else {
        panic!("id")
    };
    assert_eq!(
        second,
        format!("{T0}-1").into_bytes(),
        "same millisecond bumps the sequence"
    );
    d.advance(5);
    let Reply::Bulk(third) = d.ok("XADD s * a 3 b x") else {
        panic!("id")
    };
    assert_eq!(third, format!("{}-0", T0 + 5).into_bytes());
    assert_eq!(d.ok("XLEN s"), int(3));
    // a clock that goes backwards never produces a smaller id
    d.now = T0 - 1000;
    let Reply::Bulk(fourth) = d.ok("XADD s * a 4") else {
        panic!("id")
    };
    assert_eq!(fourth, format!("{}-1", T0 + 5).into_bytes());
    d.now = T0 + 5;
    // explicit ids must grow
    assert_eq!(d.ok("XADD s 9999999999999-5 a 5"), b("9999999999999-5"));
    assert!(matches!(d.err("XADD s 5-0 a 6"), KvError::Syntax(_)));
    assert!(matches!(d.err("XADD s 0-0 a 6"), KvError::Syntax(_)));
    assert_eq!(d.ok("XLEN s"), int(5));
    // ranges
    let Reply::Array(all) = d.ok("XRANGE s - +") else {
        panic!()
    };
    assert_eq!(all.len(), 5);
    let Reply::Array(two) = d.ok("XRANGE s - + COUNT 2") else {
        panic!()
    };
    assert_eq!(two.len(), 2);
    assert_eq!(
        d.ok(&format!("XRANGE s {T0}-1 {T0}-1")),
        Reply::Array(vec![Reply::Array(vec![b(&format!("{T0}-1")), arr(&["a", "2"])])])
    );
    let Reply::Array(rev) = d.ok("XREVRANGE s + - COUNT 1") else {
        panic!()
    };
    assert_eq!(rev.len(), 1);
    // XREAD from an id
    let Reply::Array(streams) = d.ok(&format!("XREAD COUNT 10 STREAMS s {T0}-1")) else {
        panic!()
    };
    let Reply::Array(first) = &streams[0] else { panic!() };
    assert_eq!(first[0], b("s"));
    let Reply::Array(entries) = &first[1] else { panic!() };
    assert_eq!(entries.len(), 3);
    assert_eq!(d.ok("XREAD STREAMS s $"), Reply::Nil, "nothing newer than the last id");
    assert_eq!(d.ok("XREAD STREAMS nope 0"), Reply::Nil);
    // XDEL and trimming
    assert_eq!(d.ok(&format!("XDEL s {T0}-0 {T0}-0")), int(1));
    assert_eq!(d.ok("XTRIM s MAXLEN 2"), int(2));
    assert_eq!(d.ok("XLEN s"), int(2));
    // MAXLEN on add
    assert!(matches!(d.ok("XADD cap MAXLEN 3 * n 1"), Reply::Bulk(_)));
    for _ in 0..10 {
        d.advance(1);
        d.ok("XADD cap MAXLEN 3 * n 1");
    }
    assert_eq!(d.ok("XLEN cap"), int(3));
    // a stream outlives being emptied (the last id is kept)
    d.ok("XTRIM cap MAXLEN 0");
    assert_eq!(d.ok("XLEN cap"), int(0));
    assert_eq!(d.ok("TYPE cap"), Reply::Status("stream".into()));
}

#[test]
fn keys_scan_and_namespaces() {
    let mut d = db();
    for i in 0..25 {
        d.ok(&format!("SET user:{i:02} x"));
    }
    d.ok("SET other v");
    d.ok("HSET hash:1 f v");
    assert_eq!(d.ok("DBSIZE"), int(27));
    let Reply::Array(keys) = d.ok("KEYS user:*") else {
        panic!()
    };
    assert_eq!(keys.len(), 25);
    assert!(matches!(d.ok("KEYS user:0?"), Reply::Array(a) if a.len() == 10));
    assert_eq!(d.ok("KEYS nomatch*"), arr(&[]));
    // SCAN pages through every key exactly once
    let mut seen = Vec::new();
    let mut cursor = "0".to_owned();
    loop {
        let Reply::Array(r) = d.ok(&format!("SCAN {cursor} COUNT 10")) else {
            panic!()
        };
        let Reply::Bulk(next) = &r[0] else { panic!() };
        let Reply::Array(page) = &r[1] else { panic!() };
        seen.extend(page.iter().map(|k| match k {
            Reply::Bulk(b) => String::from_utf8(b.clone()).unwrap(),
            _ => panic!(),
        }));
        cursor = String::from_utf8(next.clone()).unwrap();
        if cursor == "0" {
            break;
        }
    }
    assert_eq!(seen.len(), 27);
    let mut sorted = seen.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), 27, "no key twice");
    // MATCH and TYPE filters
    let Reply::Array(r) = d.ok("SCAN 0 MATCH user:1* COUNT 1000") else {
        panic!()
    };
    let Reply::Array(page) = &r[1] else { panic!() };
    assert_eq!(page.len(), 10);
    let Reply::Array(r) = d.ok("SCAN 0 TYPE hash COUNT 1000") else {
        panic!()
    };
    assert_eq!(r[1], arr(&["hash:1"]));
    // SCAN survives deletes between pages
    let Reply::Array(r) = d.ok("SCAN 0 COUNT 5") else {
        panic!()
    };
    let Reply::Bulk(cur) = &r[0] else { panic!() };
    d.ok("DEL user:00 user:01 user:02 user:03 user:04");
    let cur = String::from_utf8(cur.clone()).unwrap();
    assert!(matches!(d.ok(&format!("SCAN {cur} COUNT 5")), Reply::Array(_)));
    assert!(matches!(d.err("SCAN notacursor"), KvError::Syntax(_)));
    // DEL / EXISTS counting
    assert_eq!(d.ok("EXISTS other other nope"), int(2));
    assert_eq!(d.ok("DEL other nope"), int(1));
    // FLUSHDB only touches this namespace
    let other = scope("t:a", "second");
    d.store
        .exec(&other, &[b"SET".to_vec(), b"k".to_vec(), b"v".to_vec()], T0)
        .unwrap();
    assert_eq!(d.ok("FLUSHDB"), int(21));
    assert_eq!(d.ok("DBSIZE"), int(0));
    assert_eq!(d.store.scope_count(&other), 1);
    assert_eq!(d.store.namespaces("t:a"), vec![("second".to_owned(), 1)]);
}

#[test]
fn keys_refuses_huge_answers() {
    let mut d = db_with(KvConfig {
        keys_limit: 5,
        ..KvConfig::default()
    });
    for i in 0..6 {
        d.ok(&format!("SET k{i} v"));
    }
    assert!(matches!(d.err("KEYS *"), KvError::TooManyKeys(5)));
    assert_eq!(d.ok("KEYS k1"), arr(&["k1"]));
}

#[test]
fn input_is_validated() {
    let mut d = db_with(KvConfig {
        max_key_bytes: 8,
        max_value_bytes: 16,
        ..KvConfig::default()
    });
    assert!(matches!(d.err("NOSUCH x"), KvError::UnknownCommand(_)));
    assert!(matches!(d.err("GET"), KvError::WrongArity(_)));
    assert!(matches!(d.err("GET a b"), KvError::WrongArity(_)));
    assert!(matches!(d.err("SET toolongkey1 v"), KvError::InvalidKey(_)));
    let empty: Vec<Vec<u8>> = vec![b"GET".to_vec(), Vec::new()];
    assert!(matches!(
        d.store.exec(&d.scope, &empty, T0),
        Err(KvError::InvalidKey(_))
    ));
    let bad_utf8: Vec<Vec<u8>> = vec![b"GET".to_vec(), vec![0xff, 0xfe]];
    assert!(matches!(
        d.store.exec(&d.scope, &bad_utf8, T0),
        Err(KvError::InvalidKey(_))
    ));
    assert!(matches!(d.err("SET k 12345678901234567"), KvError::TooLarge(16)));
    assert!(matches!(d.err("HSET h f 12345678901234567"), KvError::TooLarge(16)));
    d.ok("APPEND a 1234567890");
    assert!(matches!(d.err("APPEND a 1234567890"), KvError::TooLarge(16)));
    assert!(matches!(d.store.exec(&d.scope, &[], T0), Err(KvError::Syntax(_))));
    assert_eq!(d.ok("get k"), Reply::Nil, "command names are case-insensitive");
    assert_eq!(d.ok("PING"), Reply::Status("PONG".into()));
    assert_eq!(d.ok("PING hi"), b("hi"));
}

#[test]
fn scopes_never_see_each_other() {
    let mut s = Store::new(KvConfig::default());
    let run = |s: &mut Store, sc: &Scope, line: &str| {
        let args: Vec<Vec<u8>> = line.split_whitespace().map(|x| x.as_bytes().to_vec()).collect();
        s.exec(sc, &args, T0).unwrap()
    };
    let a = scope("t:a", "main");
    let b2 = scope("t:b", "main");
    let a2 = scope("t:a", "other");
    run(&mut s, &a, "SET k va");
    run(&mut s, &b2, "SET k vb");
    run(&mut s, &a2, "SET k vo");
    assert_eq!(run(&mut s, &a, "GET k"), b("va"));
    assert_eq!(run(&mut s, &b2, "GET k"), b("vb"));
    assert_eq!(run(&mut s, &a2, "GET k"), b("vo"));
    run(&mut s, &a, "DEL k");
    assert_eq!(run(&mut s, &a, "GET k"), Reply::Nil);
    assert_eq!(run(&mut s, &b2, "GET k"), b("vb"));
    assert_eq!(run(&mut s, &b2, "KEYS *"), arr(&["k"]));
    // a tenant whose id is a prefix of another does not leak either
    let ab = scope("t:ab", "main");
    run(&mut s, &ab, "SET z 1");
    assert_eq!(run(&mut s, &a, "KEYS *"), arr(&[]));
    assert_eq!(run(&mut s, &a, "FLUSHDB"), int(0));
    assert_eq!(run(&mut s, &ab, "GET z"), b("1"));
}

// ---- eviction -------------------------------------------------------------------------

fn small(policy: EvictionPolicy, max: usize) -> Db {
    db_with(KvConfig {
        max_memory_bytes: max,
        eviction: policy,
        ..KvConfig::default()
    })
}

fn one_entry_cost() -> usize {
    let mut d = db();
    d.ok("SET key00 vvvvvvvvvvvvvvvvvvvvvvvvvvvvvv");
    d.store.used_memory()
}

#[test]
fn lru_evicts_the_least_recently_used_key() {
    let cost = one_entry_cost();
    let mut d = small(EvictionPolicy::AllKeysLru, cost * 3 + cost / 2);
    for i in 0..3 {
        d.ok(&format!("SET key0{i} vvvvvvvvvvvvvvvvvvvvvvvvvvvvvv"));
    }
    assert_eq!(d.store.key_count(), 3);
    // touch key00 so key01 is now the oldest
    assert!(matches!(d.ok("GET key00"), Reply::Bulk(_)));
    d.ok("SET key03 vvvvvvvvvvvvvvvvvvvvvvvvvvvvvv");
    assert_eq!(d.ok("EXISTS key01"), int(0), "the least recently used key went");
    assert_eq!(d.ok("EXISTS key00 key02 key03"), int(3));
    assert_eq!(d.store.stats.evicted, 1);
    assert!(d.store.used_memory() <= cost * 3 + cost / 2);
    // writes also count as use
    d.ok("SET key02 vvvvvvvvvvvvvvvvvvvvvvvvvvvvvv");
    d.ok("SET key04 vvvvvvvvvvvvvvvvvvvvvvvvvvvvvv");
    assert_eq!(d.ok("EXISTS key00"), int(0));
    assert_eq!(d.ok("EXISTS key02 key03 key04"), int(3));
}

#[test]
fn eviction_covers_collections_and_never_drops_the_newest_key() {
    let cost = one_entry_cost();
    let mut d = small(EvictionPolicy::AllKeysLru, cost * 2);
    d.ok("SET a vvvvvvvvvvvvvvvvvvvvvvvvvvvvvv");
    // one value larger than the whole budget stays (it is the newest) and pushes the rest out
    let big = "x".repeat(cost * 3);
    d.ok(&format!("SET big {big}"));
    assert_eq!(d.ok("EXISTS a"), int(0));
    assert_eq!(d.ok("EXISTS big"), int(1));
    d.ok("RPUSH list one two three");
    assert_eq!(d.ok("EXISTS big"), int(0), "the next write evicts the oversized key");
    assert_eq!(d.ok("LLEN list"), int(3));
}

#[test]
fn noeviction_refuses_growth_when_full_but_allows_reads_and_deletes() {
    let cost = one_entry_cost();
    let mut d = small(EvictionPolicy::NoEviction, cost * 2 + cost / 2);
    d.ok("SET key00 vvvvvvvvvvvvvvvvvvvvvvvvvvvvvv");
    d.ok("SET key01 vvvvvvvvvvvvvvvvvvvvvvvvvvvvvv");
    d.ok("SET key02 vvvvvvvvvvvvvvvvvvvvvvvvvvvvvv"); // lands over the limit
    assert!(matches!(d.err("SET key03 v"), KvError::OutOfMemory));
    assert!(matches!(d.err("RPUSH l a"), KvError::OutOfMemory));
    assert!(matches!(d.err("INCR counter"), KvError::OutOfMemory));
    assert!(matches!(d.ok("GET key00"), Reply::Bulk(_)), "reads still work");
    assert_eq!(d.ok("DEL key00"), int(1));
    assert_eq!(d.ok("DEL key01"), int(1));
    d.ok("SET key03 v"); // room again
    assert_eq!(d.store.stats.evicted, 0);
}

#[test]
fn volatile_lru_only_evicts_keys_with_a_ttl() {
    let cost = one_entry_cost();
    let mut d = small(EvictionPolicy::VolatileLru, cost * 3 + cost / 2);
    d.ok("SET keep0 vvvvvvvvvvvvvvvvvvvvvvvvvvvvvv");
    d.ok("SET temp1 vvvvvvvvvvvvvvvvvvvvvvvvvvvvvv EX 100");
    d.ok("SET temp2 vvvvvvvvvvvvvvvvvvvvvvvvvvvvvv EX 100");
    d.ok("SET temp3 vvvvvvvvvvvvvvvvvvvvvvvvvvvvvv EX 100");
    assert_eq!(d.ok("EXISTS keep0"), int(1), "a key without TTL is never evicted");
    assert_eq!(d.ok("EXISTS temp1"), int(0));
    assert_eq!(d.store.stats.evicted, 1);
    // when nothing volatile is left, growth is refused
    d.ok("PERSIST temp2");
    d.ok("PERSIST temp3");
    d.ok("SET more vvvvvvvvvvvvvvvvvvvvvvvvvvvvvv");
    assert!(matches!(d.err("SET more2 v"), KvError::OutOfMemory));
}

#[test]
fn unlimited_memory_never_evicts() {
    let mut d = small(EvictionPolicy::AllKeysLru, 0);
    for i in 0..200 {
        d.ok(&format!("SET k{i} {}", "v".repeat(100)));
    }
    assert_eq!(d.store.key_count(), 200);
    assert_eq!(d.store.stats.evicted, 0);
}

#[test]
fn memory_accounting_tracks_growth_and_shrinkage() {
    let mut d = db();
    assert_eq!(d.store.used_memory(), 0);
    d.ok("SET s abc");
    let base = d.store.used_memory();
    d.ok("SET s abcdef");
    assert_eq!(d.store.used_memory(), base + 3);
    d.ok("APPEND s 123");
    assert_eq!(d.store.used_memory(), base + 6);
    d.ok("RPUSH l aaaa bbbb");
    d.ok("HSET h f v");
    d.ok("SADD st m");
    d.ok("ZADD z 1 m");
    d.ok("XADD x * f v");
    let full = d.store.used_memory();
    assert!(full > base);
    d.ok("LPOP l 2");
    d.ok("HDEL h f");
    d.ok("SREM st m");
    d.ok("ZREM z m");
    d.ok("DEL x s");
    assert_eq!(d.store.used_memory(), 0, "everything given back");
    assert_eq!(d.store.key_count(), 0);
}
