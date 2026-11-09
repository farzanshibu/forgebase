//! Conformance: every supported command, driven by a real Redis client
//! (redis-rs) against the in-process listener over the real KV engine.
//! These tests bind `127.0.0.1:0`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::{BTreeMap, BTreeSet, HashMap};

use common::*;
use forgebase_kv::KvConfig;
use redis::Value as V;
use redis::aio::MultiplexedConnection;

fn b(s: &str) -> V {
    V::BulkString(s.as_bytes().to_vec())
}
fn i(n: i64) -> V {
    V::Int(n)
}
fn ok() -> V {
    V::Okay
}
fn st(s: &str) -> V {
    V::SimpleString(s.into())
}
fn arr(v: Vec<V>) -> V {
    V::Array(v)
}

async fn run(con: &mut MultiplexedConnection, line: &str) -> V {
    let mut parts = line.split_whitespace();
    let mut c = redis::cmd(parts.next().unwrap());
    for a in parts {
        c.arg(a);
    }
    c.query_async::<V>(con).await.unwrap_or_else(|e| panic!("{line}: {e}"))
}

async fn err(con: &mut MultiplexedConnection, line: &str) -> String {
    let mut parts = line.split_whitespace();
    let mut c = redis::cmd(parts.next().unwrap());
    for a in parts {
        c.arg(a);
    }
    let e = c.query_async::<V>(con).await.expect_err(line);
    format!("{} {}", e.code().unwrap_or(""), e.detail().unwrap_or(""))
        .trim()
        .to_owned()
}

async fn expect(con: &mut MultiplexedConnection, steps: &[(&str, V)]) {
    for (line, want) in steps {
        assert_eq!(&run(con, line).await, want, "{line}");
    }
}

fn sorted_strings(v: V) -> Vec<String> {
    let mut out: Vec<String> = match v {
        V::Array(items) | V::Set(items) => items
            .into_iter()
            .map(|i| match i {
                V::BulkString(b) => String::from_utf8(b).unwrap(),
                other => panic!("not a bulk string: {other:?}"),
            })
            .collect(),
        other => panic!("not an array: {other:?}"),
    };
    out.sort();
    out
}

#[tokio::test]
async fn connection_commands() {
    let s = server().await;
    let mut c = s.conn("adm").await;
    expect(
        &mut c,
        &[
            ("PING", st("PONG")),
            ("PING hello", b("hello")),
            ("ECHO hi", b("hi")),
            ("SELECT 3", ok()),
            ("SELECT cache", ok()),
            ("SELECT 0", ok()),
            ("CLIENT SETNAME app-1", ok()),
            ("CLIENT GETNAME", b("app-1")),
            ("CLIENT SETINFO LIB-NAME redis-rs", ok()),
            ("COMMAND DOCS", arr(vec![])),
            ("CONFIG GET maxmemory", arr(vec![])),
        ],
    )
    .await;
    assert!(matches!(run(&mut c, "CLIENT ID").await, V::Int(n) if n > 0));
    let V::Array(t) = run(&mut c, "TIME").await else {
        panic!()
    };
    assert_eq!(t.len(), 2);
    let V::BulkString(info) = run(&mut c, "INFO").await else {
        panic!()
    };
    let info = String::from_utf8(info).unwrap();
    assert!(
        info.contains("forgebase_version:") && info.contains("connected_clients:"),
        "{info}"
    );
    let V::Int(n) = run(&mut c, "COMMAND COUNT").await else {
        panic!()
    };
    assert!(n > 90, "{n}");
    assert!(err(&mut c, "SELECT 999").await.contains("out of range"));
    assert!(err(&mut c, "SELECT bad name").await.contains("wrong number"));
    assert!(err(&mut c, "SELECT _x").await.starts_with("ERR"));
    assert!(err(&mut c, "CONFIG SET maxmemory 1").await.starts_with("ERR"));
    assert!(err(&mut c, "CLIENT KILL x").await.starts_with("ERR"));
}

#[tokio::test]
async fn strings_and_counters() {
    let s = server().await;
    let mut c = s.conn("adm").await;
    expect(
        &mut c,
        &[
            ("SET k v", ok()),
            ("GET k", b("v")),
            ("GET nope", V::Nil),
            ("SETNX n x", i(1)),
            ("SETNX n y", i(0)),
            ("GETDEL n", b("x")),
            ("GET n", V::Nil),
            ("SETEX e 100 v", ok()),
            ("PSETEX pe 100000 v", ok()),
            ("APPEND a hi", i(2)),
            ("APPEND a !!", i(4)),
            ("STRLEN a", i(4)),
            ("STRLEN nope", i(0)),
            ("MSET m1 1 m2 2", ok()),
            ("MGET m1 m2 nope", arr(vec![b("1"), b("2"), V::Nil])),
            ("INCR c", i(1)),
            ("INCR c", i(2)),
            ("DECR c", i(1)),
            ("INCRBY c 10", i(11)),
            ("DECRBY c 5", i(6)),
            ("INCRBYFLOAT f 1.5", b("1.5")),
            ("INCRBYFLOAT f 1", b("2.5")),
            ("SET o 1 NX", ok()),
            ("SET o 2 NX", V::Nil),
            ("SET o 3 XX", ok()),
            ("SET nx 1 XX", V::Nil),
            ("SET o 4 GET", b("3")),
            ("SET o 5 EX 100", ok()),
            ("SET o 6 KEEPTTL", ok()),
            ("SET o 7 PX 100000", ok()),
            ("SET o 8 EXAT 4102444800", ok()),
            ("SET o 9 PXAT 4102444800000", ok()),
            ("GET o", b("9")),
        ],
    )
    .await;
    let V::Int(ttl) = run(&mut c, "TTL e").await else {
        panic!()
    };
    assert!((90..=100).contains(&ttl), "{ttl}");
    let V::Int(pttl) = run(&mut c, "PTTL pe").await else {
        panic!()
    };
    assert!((90_000..=100_000).contains(&pttl), "{pttl}");
    assert!(err(&mut c, "INCR k").await.starts_with("ERR value is not an integer"));
    assert!(err(&mut c, "INCRBYFLOAT k 1").await.contains("not a valid float"));
    assert!(
        err(&mut c, "GET")
            .await
            .contains("wrong number of arguments for 'get' command")
    );
    assert!(err(&mut c, "SET k").await.contains("wrong number of arguments"));
    // binary-safe values with CRLF and NUL
    let bin: Vec<u8> = b"a\r\nb\0c\xff".to_vec();
    let _: () = redis::cmd("SET")
        .arg("bin")
        .arg(&bin)
        .query_async(&mut c)
        .await
        .unwrap();
    let got: Vec<u8> = redis::cmd("GET").arg("bin").query_async(&mut c).await.unwrap();
    assert_eq!(got, bin);
}

#[tokio::test]
async fn keys_and_expiry() {
    let s = server().await;
    let mut c = s.conn("adm").await;
    expect(
        &mut c,
        &[
            ("SET k v", ok()),
            ("MSET user:1 a user:2 b other c", ok()),
            ("EXISTS k user:1 nope", i(2)),
            ("TYPE k", st("string")),
            ("TYPE nope", st("none")),
            ("EXPIRE k 100", i(1)),
            ("EXPIRE nope 100", i(0)),
            ("PERSIST k", i(1)),
            ("TTL k", i(-1)),
            ("TTL nope", i(-2)),
            ("PEXPIRE k 100000", i(1)),
            ("EXPIREAT k 4102444800", i(1)),
            ("PEXPIREAT k 4102444800000", i(1)),
            ("DBSIZE", i(4)),
            ("DEL user:1", i(1)),
            ("UNLINK user:2 other", i(2)),
            ("DBSIZE", i(1)),
        ],
    )
    .await;
    let V::Int(p) = run(&mut c, "PTTL k").await else {
        panic!()
    };
    assert!(p > 0);
    run(&mut c, "MSET user:1 a user:2 b other c").await;
    assert_eq!(sorted_strings(run(&mut c, "KEYS user:*").await), ["user:1", "user:2"]);
    assert_eq!(sorted_strings(run(&mut c, "KEYS *").await).len(), 4);
    // SCAN walks everything exactly once
    let mut seen = BTreeSet::new();
    let mut cursor = "0".to_owned();
    loop {
        let (next, keys): (String, Vec<String>) = redis::cmd("SCAN")
            .arg(&cursor)
            .arg("COUNT")
            .arg(2)
            .query_async(&mut c)
            .await
            .unwrap();
        seen.extend(keys);
        if next == "0" {
            break;
        }
        cursor = next;
    }
    assert_eq!(seen.len(), 4);
    let (_, m): (String, Vec<String>) = redis::cmd("SCAN")
        .arg(0)
        .arg("MATCH")
        .arg("user:*")
        .arg("COUNT")
        .arg(100)
        .query_async(&mut c)
        .await
        .unwrap();
    assert_eq!(m.len(), 2);
    assert_eq!(run(&mut c, "FLUSHDB").await, ok());
    assert_eq!(run(&mut c, "DBSIZE").await, i(0));
    run(&mut c, "SET a 1").await;
    assert_eq!(run(&mut c, "FLUSHDB ASYNC").await, ok());
    assert_eq!(run(&mut c, "FLUSHDB SYNC").await, ok());
}

#[tokio::test]
async fn expired_keys_disappear_at_their_deadline() {
    let s = server().await;
    let mut c = s.conn("adm").await;
    run(&mut c, "SET short 1 PX 60").await;
    assert_eq!(run(&mut c, "GET short").await, b("1"));
    tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    assert_eq!(run(&mut c, "GET short").await, V::Nil);
    assert_eq!(run(&mut c, "TTL short").await, i(-2));
}

#[tokio::test]
async fn hashes() {
    let s = server().await;
    let mut c = s.conn("adm").await;
    expect(
        &mut c,
        &[
            ("HSET h a 1 b 2", i(2)),
            ("HSET h a 9", i(0)),
            ("HSETNX h c 3", i(1)),
            ("HSETNX h c 4", i(0)),
            ("HGET h a", b("9")),
            ("HGET h zz", V::Nil),
            ("HMGET h a zz b", arr(vec![b("9"), V::Nil, b("2")])),
            ("HLEN h", i(3)),
            ("HEXISTS h a", i(1)),
            ("HEXISTS h zz", i(0)),
            ("HINCRBY h a 1", i(10)),
            ("HDEL h c zz", i(1)),
            ("TYPE h", st("hash")),
        ],
    )
    .await;
    let all: HashMap<String, String> = redis::cmd("HGETALL").arg("h").query_async(&mut c).await.unwrap();
    assert_eq!(
        all,
        HashMap::from([("a".into(), "10".into()), ("b".into(), "2".into())])
    );
    assert_eq!(sorted_strings(run(&mut c, "HKEYS h").await), ["a", "b"]);
    assert_eq!(sorted_strings(run(&mut c, "HVALS h").await), ["10", "2"]);
    run(&mut c, "SET str x").await;
    assert!(err(&mut c, "HGET str a").await.starts_with("WRONGTYPE"));
}

#[tokio::test]
async fn lists() {
    let s = server().await;
    let mut c = s.conn("adm").await;
    expect(
        &mut c,
        &[
            ("RPUSH l a b c", i(3)),
            ("LPUSH l z", i(4)),
            ("LRANGE l 0 -1", arr(vec![b("z"), b("a"), b("b"), b("c")])),
            ("LLEN l", i(4)),
            ("LINDEX l 1", b("a")),
            ("LINDEX l 99", V::Nil),
            ("LSET l 1 A", ok()),
            ("LREM l 0 b", i(1)),
            ("LRANGE l 0 -1", arr(vec![b("z"), b("A"), b("c")])),
            ("LTRIM l 0 1", ok()),
            ("LPOP l", b("z")),
            ("RPOP l", b("A")),
            ("LPOP l", V::Nil),
            ("RPUSH l 1 2 3 4", i(4)),
            ("LPOP l 2", arr(vec![b("1"), b("2")])),
            ("RPOP l 5", arr(vec![b("4"), b("3")])),
            ("TYPE l", st("none")),
        ],
    )
    .await;
    assert!(err(&mut c, "LSET missing 0 x").await.starts_with("ERR"));
}

#[tokio::test]
async fn sets() {
    let s = server().await;
    let mut c = s.conn("adm").await;
    expect(
        &mut c,
        &[
            ("SADD s a b c", i(3)),
            ("SADD s a", i(0)),
            ("SISMEMBER s a", i(1)),
            ("SISMEMBER s z", i(0)),
            ("SCARD s", i(3)),
            ("SADD t b c d", i(3)),
            ("SREM s zz", i(0)),
            ("TYPE s", st("set")),
        ],
    )
    .await;
    assert_eq!(sorted_strings(run(&mut c, "SMEMBERS s").await), ["a", "b", "c"]);
    assert_eq!(sorted_strings(run(&mut c, "SUNION s t").await), ["a", "b", "c", "d"]);
    assert_eq!(sorted_strings(run(&mut c, "SINTER s t").await), ["b", "c"]);
    assert_eq!(sorted_strings(run(&mut c, "SDIFF s t").await), ["a"]);
    let V::BulkString(popped) = run(&mut c, "SPOP s").await else {
        panic!()
    };
    assert!(["a", "b", "c"].contains(&String::from_utf8(popped).unwrap().as_str()));
    assert_eq!(run(&mut c, "SCARD s").await, i(2));
    // SREM removes only the members still present (2 of a, b, c after SPOP).
    assert_eq!(run(&mut c, "SREM s a b c").await, i(2));
}

#[tokio::test]
async fn sorted_sets() {
    let s = server().await;
    let mut c = s.conn("adm").await;
    expect(
        &mut c,
        &[
            ("ZADD z 1 a 2 b 3 c", i(3)),
            ("ZADD z NX 9 a", i(0)),
            ("ZADD z XX CH 5 a", i(1)),
            ("ZSCORE z b", b("2")),
            ("ZSCORE z zz", V::Nil),
            ("ZRANK z b", i(0)),
            ("ZREVRANK z b", i(2)),
            ("ZRANGE z 0 -1", arr(vec![b("b"), b("c"), b("a")])),
            ("ZRANGE z 0 1 WITHSCORES", arr(vec![b("b"), b("2"), b("c"), b("3")])),
            ("ZREVRANGE z 0 0", arr(vec![b("a")])),
            ("ZRANGEBYSCORE z 2 3", arr(vec![b("b"), b("c")])),
            ("ZRANGEBYSCORE z (2 +inf", arr(vec![b("c"), b("a")])),
            ("ZINCRBY z 5 b", b("7")),
            ("ZCARD z", i(3)),
            ("ZCOUNT z 3 5", i(2)),
            ("ZPOPMIN z", arr(vec![b("c"), b("3")])),
            ("ZPOPMAX z", arr(vec![b("b"), b("7")])),
            ("ZREM z a zz", i(1)),
            ("TYPE z", st("none")),
        ],
    )
    .await;
}

#[tokio::test]
async fn streams() {
    let s = server().await;
    let mut c = s.conn("adm").await;
    expect(
        &mut c,
        &[
            ("XADD st 1-1 f v", b("1-1")),
            ("XADD st 1-2 g w", b("1-2")),
            ("XLEN st", i(2)),
            ("TYPE st", st("stream")),
            (
                "XRANGE st - +",
                arr(vec![
                    arr(vec![b("1-1"), arr(vec![b("f"), b("v")])]),
                    arr(vec![b("1-2"), arr(vec![b("g"), b("w")])]),
                ]),
            ),
            (
                "XREVRANGE st + - COUNT 1",
                arr(vec![arr(vec![b("1-2"), arr(vec![b("g"), b("w")])])]),
            ),
            (
                "XREAD STREAMS st 1-1",
                arr(vec![arr(vec![
                    b("st"),
                    arr(vec![arr(vec![b("1-2"), arr(vec![b("g"), b("w")])])]),
                ])]),
            ),
            ("XDEL st 1-1", i(1)),
            ("XLEN st", i(1)),
        ],
    )
    .await;
    let V::BulkString(id) = run(&mut c, "XADD st * h x").await else {
        panic!()
    };
    assert!(String::from_utf8(id).unwrap().contains('-'));
    assert_eq!(run(&mut c, "XTRIM st MAXLEN 1").await, i(1));
    assert_eq!(run(&mut c, "XLEN st").await, i(1));
}

#[tokio::test]
async fn publish_without_subscribers_counts_zero() {
    let s = server().await;
    let mut c = s.conn("adm").await;
    assert_eq!(run(&mut c, "PUBLISH ch hello").await, i(0));
}

#[tokio::test]
async fn errors_carry_the_codes_clients_branch_on() {
    let s = server().await;
    let mut c = s.conn("adm").await;
    run(&mut c, "SET k v").await;
    assert!(
        err(&mut c, "LPUSH k x")
            .await
            .starts_with("WRONGTYPE Operation against a key")
    );
    assert!(
        err(&mut c, "NOSUCHCOMMAND a b")
            .await
            .starts_with("ERR unknown command 'NOSUCHCOMMAND', with args beginning with: 'a' 'b'")
    );
    assert!(err(&mut c, "SET k v EX").await.starts_with("ERR"));
    assert!(err(&mut c, "SET k v EX notanumber").await.starts_with("ERR"));
    assert!(err(&mut c, "EXPIRE k abc").await.starts_with("ERR"));
    assert!(err(&mut c, "ZADD z abc a").await.starts_with("ERR"));
    // the connection survives every one of them
    assert_eq!(run(&mut c, "GET k").await, b("v"));
}

#[tokio::test]
async fn memory_limit_with_noeviction_answers_oom() {
    let s = server_with(
        |_| {},
        KvConfig {
            max_memory_bytes: 2_000,
            eviction: forgebase_kv::EvictionPolicy::NoEviction,
            ..KvConfig::default()
        },
    )
    .await;
    let mut c = s.conn("adm").await;
    let mut oom = false;
    for n in 0..100 {
        let r = redis::cmd("SET")
            .arg(format!("k{n}"))
            .arg("x".repeat(200))
            .query_async::<V>(&mut c)
            .await;
        if let Err(e) = r {
            assert_eq!(e.code(), Some("OOM"), "{e}");
            oom = true;
            break;
        }
    }
    assert!(oom, "the limit was never hit");
    assert!(
        matches!(run(&mut c, "GET k0").await, V::BulkString(_)),
        "reads still work"
    );
}

#[tokio::test]
async fn resp3_clients_get_resp3_types() {
    let s = server().await;
    let mut c = redis::Client::open(s.url3("adm", 0))
        .unwrap()
        .get_multiplexed_async_connection()
        .await
        .unwrap();
    run(&mut c, "HSET h a 1 b 2").await;
    run(&mut c, "SADD s x y").await;
    run(&mut c, "SET k v").await;
    let all: BTreeMap<String, String> = redis::cmd("HGETALL").arg("h").query_async(&mut c).await.unwrap();
    assert_eq!(
        all,
        BTreeMap::from([("a".into(), "1".into()), ("b".into(), "2".into())])
    );
    let m: BTreeSet<String> = redis::cmd("SMEMBERS").arg("s").query_async(&mut c).await.unwrap();
    assert_eq!(m, BTreeSet::from(["x".to_owned(), "y".to_owned()]));
    assert_eq!(run(&mut c, "GET nope").await, V::Nil);
    assert_eq!(run(&mut c, "GET k").await, b("v"));
    assert_eq!(run(&mut c, "INCR n").await, i(1));
}

#[tokio::test]
async fn pubsub_with_a_real_client_resp2() {
    use futures_util::StreamExt;
    let s = server().await;
    let client = redis::Client::open(s.url("adm", 0)).unwrap();
    let mut ps = client.get_async_pubsub().await.unwrap();
    ps.subscribe("alerts").await.unwrap();
    ps.psubscribe("news.*").await.unwrap();
    let mut publisher = s.conn("adm").await;
    assert_eq!(run(&mut publisher, "PUBLISH alerts fire").await, i(1));
    run(&mut publisher, "PUBLISH news.sport goal").await;
    run(&mut publisher, "PUBLISH other ignored").await;
    let mut stream = ps.on_message();
    let m1 = tokio::time::timeout(T, stream.next()).await.unwrap().unwrap();
    assert_eq!(m1.get_channel_name(), "alerts");
    assert_eq!(m1.get_payload::<String>().unwrap(), "fire");
    let m2 = tokio::time::timeout(T, stream.next()).await.unwrap().unwrap();
    assert_eq!(m2.get_channel_name(), "news.sport");
    assert_eq!(m2.get_payload::<String>().unwrap(), "goal");
}
