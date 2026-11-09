//! Protocol-level behaviour over a raw socket: the authentication gate,
//! authorisation, namespaces, tenant isolation, pipelining, RESP3 pushes,
//! limits, timeouts, graceful shutdown. These tests bind `127.0.0.1:0`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use common::*;
use forgebase_data::AccessContext;
use forgebase_kv::KvConfig;
use forgebase_resp::compat;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

// ---- the authentication gate ---------------------------------------------------

#[tokio::test]
async fn unauthenticated_connections_may_only_ping_hello_auth_quit() {
    let s = server().await;
    let mut r = s.raw().await;
    assert_eq!(r.cmd(&["PING"]).await, Val::Simple("PONG".into()));
    assert_eq!(r.cmd(&["PING", "x"]).await, Val::bulk("x"));
    for cmd in [
        &["GET", "k"][..],
        &["SET", "k", "v"],
        &["DEL", "k"],
        &["KEYS", "*"],
        &["FLUSHDB"],
        &["DBSIZE"],
        &["SELECT", "1"],
        &["SUBSCRIBE", "c"],
        &["PSUBSCRIBE", "c*"],
        &["PUBLISH", "c", "m"],
        &["INFO"],
        &["CLIENT", "ID"],
        &["COMMAND", "COUNT"],
        &["CONFIG", "GET", "*"],
        &["ECHO", "x"],
        &["TIME"],
        &["RESET"],
        &["MULTI"],
        &["EVAL", "return 1", "0"],
        &["NOSUCHCOMMAND"],
    ] {
        let v = r.cmd(cmd).await;
        assert!(v.is_err_starting("NOAUTH"), "{cmd:?} answered {v:?}");
    }
    // HELLO without credentials does not authenticate either
    assert!(r.cmd(&["HELLO", "3"]).await.is_err_starting("NOAUTH"));
    assert!(r.cmd(&["GET", "k"]).await.is_err_starting("NOAUTH"));
    assert_eq!(r.cmd(&["AUTH", "adm"]).await, Val::ok());
    assert_eq!(r.cmd(&["SET", "k", "v"]).await, Val::ok());
    assert_eq!(r.cmd(&["QUIT"]).await, Val::ok());
    assert!(r.closed_within(T).await);
}

#[tokio::test]
async fn auth_accepts_the_token_with_or_without_a_user_and_rejects_the_rest() {
    let s = server().await;
    let mut r = s.raw().await;
    for bad in [
        &["AUTH", "nope"][..],
        &["AUTH", "default", "nope"],
        &["AUTH", "someone-else", "adm"], // right token, wrong user
        &["AUTH", "a1", "nope"],
    ] {
        let v = r.cmd(bad).await;
        assert!(v.is_err_starting("WRONGPASS"), "{bad:?} -> {v:?}");
    }
    assert!(r.cmd(&["GET", "k"]).await.is_err_starting("NOAUTH"), "still locked out");
    assert!(s.raw().await.cmd(&["AUTH", "", ""]).await.is_err_starting("WRONGPASS"));
    assert!(r.cmd(&["AUTH"]).await.is_err_starting("ERR wrong number"));
    assert!(
        r.cmd(&["AUTH", "a", "b", "c"])
            .await
            .is_err_starting("ERR wrong number")
    );
    assert_eq!(r.cmd(&["AUTH", "default", "adm"]).await, Val::ok());
    let mut r2 = s.raw().await;
    assert_eq!(r2.cmd(&["AUTH", "adm"]).await, Val::ok());
    let mut r3 = s.raw().await;
    assert_eq!(
        r3.cmd(&["AUTH", "a1", "adm"]).await,
        Val::ok(),
        "user = the key owner's id"
    );
    // HELLO ... AUTH
    let mut r4 = s.raw().await;
    let hello = r4.cmd(&["HELLO", "2", "AUTH", "default", "adm", "SETNAME", "x"]).await;
    assert!(matches!(hello, Val::Arr(ref a) if a.len() == 14), "{hello:?}");
    assert_eq!(r4.cmd(&["SET", "k", "v"]).await, Val::ok());
    let mut r5 = s.raw().await;
    assert!(
        r5.cmd(&["HELLO", "3", "AUTH", "default", "nope"])
            .await
            .is_err_starting("WRONGPASS")
    );
    assert!(r5.cmd(&["GET", "k"]).await.is_err_starting("NOAUTH"));
    // a password-looking or JWT-looking secret is never a credential here
    let mut r6 = s.raw().await;
    assert!(
        r6.cmd(&["AUTH", "eyJhbGciOiJIUzI1NiJ9.e30.x"])
            .await
            .is_err_starting("WRONGPASS")
    );
}

#[tokio::test]
async fn repeated_failed_auth_closes_the_connection_and_blocks_the_address() {
    let s = server_with(
        |c| {
            c.max_auth_failures = 3;
            c.max_auth_failures_per_ip = 5;
        },
        KvConfig::default(),
    )
    .await;
    let mut r = s.raw().await;
    for _ in 0..2 {
        assert!(r.cmd(&["AUTH", "x"]).await.is_err_starting("WRONGPASS"));
    }
    assert!(r.cmd(&["AUTH", "x"]).await.is_err_starting("WRONGPASS"));
    assert!(r.closed_within(T).await, "the third failure closes the connection");
    // two more failures on a new connection exhaust the per-address budget (5)
    let mut r = s.raw().await;
    for _ in 0..2 {
        assert!(r.cmd(&["AUTH", "x"]).await.is_err_starting("WRONGPASS"));
    }
    // now even the right token is refused for this address
    let mut r = s.raw().await;
    let v = r.cmd(&["AUTH", "adm"]).await;
    assert!(v.is_err_starting("ERR too many failed authentication"), "{v:?}");
    assert!(r.closed_within(T).await);
}

#[tokio::test]
async fn unauthenticated_connections_have_small_limits_and_a_deadline() {
    let s = server_with(|c| c.auth_timeout = ms(300), KvConfig::default()).await;
    // a 5000-byte argument before AUTH is refused and the connection closed
    let mut r = s.raw().await;
    let big = "x".repeat(5000);
    r.send(&["AUTH", &big]).await;
    let v = r.read().await;
    assert!(v.is_err_starting("ERR Protocol error: invalid bulk length"), "{v:?}");
    assert!(r.closed_within(T).await);
    // 17 arguments before AUTH
    let mut r = s.raw().await;
    let many: Vec<&str> = std::iter::repeat_n("a", 17).collect();
    r.send(&many).await;
    assert!(
        r.read()
            .await
            .is_err_starting("ERR Protocol error: invalid multibulk length")
    );
    // silence: closed after the auth timeout
    let mut r = s.raw().await;
    assert!(r.closed_within(T).await);
}

// ---- authorisation ----------------------------------------------------------------

#[tokio::test]
async fn the_same_authorizer_actions_as_the_rest_api_apply() {
    let s = server().await;
    // a plain user: the fail-closed default authorizer allows only administrators
    let mut u = s.raw_as("usr").await;
    for cmd in [
        &["GET", "k"][..],
        &["SET", "k", "v"],
        &["DBSIZE"],
        &["FLUSHDB"],
        &["PUBLISH", "c", "m"],
    ] {
        let v = u.cmd(cmd).await;
        assert!(v.is_err_starting("NOPERM"), "{cmd:?} -> {v:?}");
    }
    assert!(u.cmd(&["SUBSCRIBE", "c"]).await.is_err_starting("NOPERM"));
    assert!(u.cmd(&["PSUBSCRIBE", "c*"]).await.is_err_starting("NOPERM"));
    assert_eq!(
        u.cmd(&["PING"]).await,
        Val::Simple("PONG".into()),
        "connection commands need no data access"
    );
    // the denial is the engine's: nothing was written
    assert_eq!(
        s.kv.exec(&AccessContext::system(), "default", &[b"DBSIZE".to_vec()])
            .unwrap(),
        forgebase_kv::Reply::Int(0)
    );
}

#[tokio::test]
async fn key_scopes_narrow_what_a_credential_may_do() {
    let s = server().await;
    let mut a = s.raw_as("adm").await;
    assert_eq!(a.cmd(&["SET", "k", "v"]).await, Val::ok());
    let mut ro = s.raw_as("ro").await;
    assert_eq!(ro.cmd(&["GET", "k"]).await, Val::bulk("v"));
    assert_eq!(ro.cmd(&["KEYS", "*"]).await, Val::Arr(vec![Val::bulk("k")]));
    let v = ro.cmd(&["SET", "k", "x"]).await;
    assert!(
        v.is_err_starting("NOPERM") && v.err_text().contains("kv:write:default"),
        "{v:?}"
    );
    assert!(ro.cmd(&["DEL", "k"]).await.is_err_starting("NOPERM"));
    assert!(ro.cmd(&["FLUSHDB"]).await.is_err_starting("NOPERM"));
    assert!(ro.cmd(&["PUBLISH", "c", "m"]).await.is_err_starting("NOPERM"));
    assert!(
        matches!(ro.cmd(&["SUBSCRIBE", "c"]).await, Val::Arr(_)),
        "kv:read allows subscribing"
    );
    assert_eq!(a.cmd(&["GET", "k"]).await, Val::bulk("v"), "the value is untouched");
    // write access in one namespace only
    let mut co = s.raw_as("cacheonly").await;
    assert!(co.cmd(&["SET", "k", "x"]).await.is_err_starting("NOPERM"));
    assert_eq!(co.cmd(&["SELECT", "cache"]).await, Val::ok());
    assert_eq!(co.cmd(&["SET", "k", "cached"]).await, Val::ok());
    assert_eq!(co.cmd(&["GET", "k"]).await, Val::bulk("cached"));
    assert_eq!(co.cmd(&["SELECT", "other"]).await, Val::ok());
    assert!(co.cmd(&["SET", "k", "x"]).await.is_err_starting("NOPERM"));
}

// ---- namespaces and tenants ------------------------------------------------------------

#[tokio::test]
async fn select_maps_to_kv_namespaces() {
    let s = server().await;
    let mut r = s.raw_as("adm").await;
    assert_eq!(r.cmd(&["SET", "k", "zero"]).await, Val::ok());
    assert_eq!(r.cmd(&["SELECT", "1"]).await, Val::ok());
    assert_eq!(r.cmd(&["GET", "k"]).await, Val::Null, "db1 is empty");
    assert_eq!(r.cmd(&["SET", "k", "one"]).await, Val::ok());
    assert_eq!(r.cmd(&["SELECT", "reports"]).await, Val::ok());
    assert_eq!(r.cmd(&["SET", "k", "named"]).await, Val::ok());
    assert_eq!(r.cmd(&["DBSIZE"]).await, Val::Int(1));
    assert_eq!(r.cmd(&["SELECT", "0"]).await, Val::ok());
    assert_eq!(r.cmd(&["GET", "k"]).await, Val::bulk("zero"));
    // the same data through the engine API, in its namespaces
    let sys = AccessContext::system();
    for (ns, want) in [("default", "zero"), ("db1", "one"), ("reports", "named")] {
        let got = s.kv.exec(&sys, ns, &[b"GET".to_vec(), b"k".to_vec()]).unwrap();
        assert_eq!(got, forgebase_kv::Reply::bulk(want), "{ns}");
    }
    // FLUSHDB clears one namespace only
    assert_eq!(r.cmd(&["SELECT", "1"]).await, Val::ok());
    assert_eq!(r.cmd(&["FLUSHDB"]).await, Val::ok());
    assert_eq!(r.cmd(&["SELECT", "0"]).await, Val::ok());
    assert_eq!(r.cmd(&["GET", "k"]).await, Val::bulk("zero"));
    // RESET goes back to namespace 0 and logs out
    assert_eq!(r.cmd(&["SELECT", "reports"]).await, Val::ok());
    assert_eq!(r.cmd(&["RESET"]).await, Val::Simple("RESET".into()));
    assert!(r.cmd(&["GET", "k"]).await.is_err_starting("NOAUTH"));
    assert_eq!(r.cmd(&["AUTH", "adm"]).await, Val::ok());
    assert_eq!(r.cmd(&["GET", "k"]).await, Val::bulk("zero"));
}

#[tokio::test]
async fn tenants_never_see_each_others_keys_channels_or_namespaces() {
    let s = server().await;
    let mut a = s.raw_as("ta").await;
    let mut b = s.raw_as("tb").await;
    let mut d = s.raw_as("adm").await; // default tenant
    for (r, v) in [(&mut a, "from-alpha"), (&mut b, "from-beta"), (&mut d, "from-default")] {
        assert_eq!(r.cmd(&["SET", "shared-name", v]).await, Val::ok());
        assert_eq!(r.cmd(&["HSET", "h", "f", v]).await, Val::Int(1));
    }
    assert_eq!(a.cmd(&["GET", "shared-name"]).await, Val::bulk("from-alpha"));
    assert_eq!(b.cmd(&["GET", "shared-name"]).await, Val::bulk("from-beta"));
    assert_eq!(d.cmd(&["GET", "shared-name"]).await, Val::bulk("from-default"));
    assert_eq!(a.cmd(&["DBSIZE"]).await, Val::Int(2));
    assert_eq!(a.cmd(&["KEYS", "*"]).await.as_array().len(), 2);
    // a key only alpha has is invisible to the others, whatever the namespace
    assert_eq!(a.cmd(&["SET", "secret", "alpha-only"]).await, Val::ok());
    for r in [&mut b, &mut d] {
        assert_eq!(r.cmd(&["GET", "secret"]).await, Val::Null);
        assert_eq!(r.cmd(&["EXISTS", "secret"]).await, Val::Int(0));
        assert_eq!(r.cmd(&["SELECT", "db1"]).await, Val::ok());
        assert_eq!(r.cmd(&["GET", "secret"]).await, Val::Null);
        assert_eq!(r.cmd(&["SELECT", "0"]).await, Val::ok());
    }
    // FLUSHDB in one tenant leaves the others alone
    assert_eq!(b.cmd(&["FLUSHDB"]).await, Val::ok());
    assert_eq!(b.cmd(&["DBSIZE"]).await, Val::Int(0));
    assert_eq!(a.cmd(&["GET", "shared-name"]).await, Val::bulk("from-alpha"));
    assert_eq!(d.cmd(&["GET", "shared-name"]).await, Val::bulk("from-default"));
    // engine view: three scopes, one key each of the shared name
    let sys = AccessContext::system();
    let mut alpha = AccessContext::admin("x");
    alpha.tenant = Some("alpha".into());
    assert_eq!(
        s.kv.exec(&alpha, "default", &[b"GET".to_vec(), b"secret".to_vec()])
            .unwrap(),
        forgebase_kv::Reply::bulk("alpha-only")
    );
    assert_eq!(
        s.kv.exec(&sys, "default", &[b"GET".to_vec(), b"secret".to_vec()])
            .unwrap(),
        forgebase_kv::Reply::Nil
    );
}

#[tokio::test]
async fn pubsub_channels_are_scoped_by_tenant_and_namespace() {
    let s = server().await;
    let mut sub_a = s.raw_as("ta").await;
    let mut sub_a_db1 = s.raw_as("ta").await;
    let mut sub_b = s.raw_as("tb").await;
    assert_eq!(
        sub_a.cmd(&["SUBSCRIBE", "news"]).await,
        Val::Arr(vec![Val::bulk("subscribe"), Val::bulk("news"), Val::Int(1)])
    );
    assert_eq!(sub_a_db1.cmd(&["SELECT", "1"]).await, Val::ok());
    sub_a_db1.cmd(&["SUBSCRIBE", "news"]).await;
    sub_b.cmd(&["SUBSCRIBE", "news"]).await;
    sub_b.cmd(&["PSUBSCRIBE", "*"]).await;
    // alpha publishes in its default namespace: one subscriber, nobody else
    let mut p = s.raw_as("ta").await;
    assert_eq!(p.cmd(&["PUBLISH", "news", "hello-alpha"]).await, Val::Int(1));
    assert_eq!(
        sub_a.read().await,
        Val::Arr(vec![Val::bulk("message"), Val::bulk("news"), Val::bulk("hello-alpha")])
    );
    // beta publishes: only beta's subscriptions (channel and pattern) hear it
    let mut pb = s.raw_as("tb").await;
    assert_eq!(pb.cmd(&["PUBLISH", "news", "hello-beta"]).await, Val::Int(1));
    let mut got = vec![sub_b.read().await, sub_b.read().await];
    got.sort_by_key(|v| v.as_array().len());
    assert_eq!(
        got,
        vec![
            Val::Arr(vec![Val::bulk("message"), Val::bulk("news"), Val::bulk("hello-beta")]),
            Val::Arr(vec![
                Val::bulk("pmessage"),
                Val::bulk("*"),
                Val::bulk("news"),
                Val::bulk("hello-beta")
            ]),
        ],
        "the channel subscription and the pattern subscription each deliver once (in either order)"
    );
    // nothing crossed over
    assert!(sub_a.closed_within(ms(150)).await.not(), "alpha got no extra message");
    assert!(sub_a_db1.closed_within(ms(150)).await.not());
    assert_eq!(p.cmd(&["PING"]).await, Val::Simple("PONG".into()));
}

trait Not {
    fn not(self) -> bool;
}
impl Not for bool {
    fn not(self) -> bool {
        !self
    }
}

// ---- pub/sub semantics ---------------------------------------------------------------------

#[tokio::test]
async fn resp2_subscribe_unsubscribe_psubscribe_flow() {
    let s = server().await;
    let mut sub = s.raw_as("adm").await;
    let sub_reply = |kind: &str, ch: Option<&str>, n: i64| {
        Val::Arr(vec![Val::bulk(kind), ch.map_or(Val::Null, Val::bulk), Val::Int(n)])
    };
    assert_eq!(
        sub.cmd(&["SUBSCRIBE", "a", "b"]).await,
        sub_reply("subscribe", Some("a"), 1)
    );
    assert_eq!(sub.read().await, sub_reply("subscribe", Some("b"), 2));
    assert_eq!(
        sub.cmd(&["PSUBSCRIBE", "x.*"]).await,
        sub_reply("psubscribe", Some("x.*"), 3)
    );
    assert_eq!(
        sub.cmd(&["SUBSCRIBE", "a"]).await,
        sub_reply("subscribe", Some("a"), 3),
        "idempotent"
    );
    // while subscribed (RESP2) only the pub/sub commands are allowed
    let v = sub.cmd(&["GET", "k"]).await;
    assert!(v.is_err_starting("ERR Can't execute 'get'"), "{v:?}");
    assert_eq!(
        sub.cmd(&["PING"]).await,
        Val::Arr(vec![Val::bulk("pong"), Val::bulk("")])
    );
    assert_eq!(
        sub.cmd(&["PING", "x"]).await,
        Val::Arr(vec![Val::bulk("pong"), Val::bulk("x")])
    );
    let mut p = s.raw_as("adm").await;
    assert_eq!(p.cmd(&["PUBLISH", "a", "1"]).await, Val::Int(1));
    assert_eq!(
        p.cmd(&["PUBLISH", "x.y", "2"]).await,
        Val::Int(0),
        "pattern subscribers are not counted"
    );
    assert_eq!(p.cmd(&["PUBLISH", "zzz", "3"]).await, Val::Int(0));
    assert_eq!(
        sub.read().await,
        Val::Arr(vec![Val::bulk("message"), Val::bulk("a"), Val::bulk("1")])
    );
    assert_eq!(
        sub.read().await,
        Val::Arr(vec![
            Val::bulk("pmessage"),
            Val::bulk("x.*"),
            Val::bulk("x.y"),
            Val::bulk("2")
        ])
    );
    // unsubscribe one, then the rest
    assert_eq!(
        sub.cmd(&["UNSUBSCRIBE", "a"]).await,
        sub_reply("unsubscribe", Some("a"), 2)
    );
    assert_eq!(p.cmd(&["PUBLISH", "a", "gone"]).await, Val::Int(0));
    assert_eq!(
        sub.cmd(&["PUNSUBSCRIBE"]).await,
        sub_reply("punsubscribe", Some("x.*"), 1)
    );
    assert_eq!(sub.cmd(&["UNSUBSCRIBE"]).await, sub_reply("unsubscribe", Some("b"), 0));
    // back to normal mode
    assert_eq!(sub.cmd(&["SET", "k", "v"]).await, Val::ok());
    assert_eq!(sub.cmd(&["UNSUBSCRIBE"]).await, sub_reply("unsubscribe", None, 0));
    // a message is delivered once per matching pattern
    assert_eq!(
        sub.cmd(&["PSUBSCRIBE", "n*", "ne*"]).await,
        sub_reply("psubscribe", Some("n*"), 1)
    );
    sub.read().await;
    p.cmd(&["PUBLISH", "news", "m"]).await;
    let mut pats = vec![sub.read().await, sub.read().await]
        .into_iter()
        .map(|v| match &v.as_array()[1] {
            Val::Bulk(b) => b.clone(),
            other => panic!("{other:?}"),
        })
        .collect::<Vec<_>>();
    pats.sort();
    assert_eq!(pats, vec![b"n*".to_vec(), b"ne*".to_vec()]);
}

#[tokio::test]
async fn resp3_pushes_and_commands_while_subscribed() {
    let s = server().await;
    let mut sub = s.raw().await;
    let hello = sub.cmd(&["HELLO", "3", "AUTH", "default", "adm"]).await;
    let Val::Map(fields) = hello else {
        panic!("HELLO 3 answers a map")
    };
    assert!(fields.contains(&(Val::bulk("proto"), Val::Int(3))));
    assert!(fields.contains(&(Val::bulk("server"), Val::bulk("forgebase"))));
    assert_eq!(
        sub.cmd(&["SUBSCRIBE", "c"]).await,
        Val::Push(vec![Val::bulk("subscribe"), Val::bulk("c"), Val::Int(1)])
    );
    // RESP3 allows other commands on a subscribed connection
    assert_eq!(sub.cmd(&["SET", "k", "v"]).await, Val::ok());
    assert_eq!(sub.cmd(&["GET", "nope"]).await, Val::Null);
    assert_eq!(sub.cmd(&["PING"]).await, Val::Simple("PONG".into()));
    let mut p = s.raw_as("adm").await;
    assert_eq!(p.cmd(&["PUBLISH", "c", "pushed"]).await, Val::Int(1));
    assert_eq!(
        sub.read().await,
        Val::Push(vec![Val::bulk("message"), Val::bulk("c"), Val::bulk("pushed")])
    );
    // typed replies
    sub.cmd(&["HSET", "h", "f", "1"]).await;
    assert_eq!(
        sub.cmd(&["HGETALL", "h"]).await,
        Val::Map(vec![(Val::bulk("f"), Val::bulk("1"))])
    );
    sub.cmd(&["SADD", "s", "m"]).await;
    assert_eq!(sub.cmd(&["SMEMBERS", "s"]).await, Val::Set(vec![Val::bulk("m")]));
    // HELLO 2 switches back (unsubscribe first: RESP2 restricts subscribed connections)
    assert_eq!(
        sub.cmd(&["UNSUBSCRIBE"]).await,
        Val::Push(vec![Val::bulk("unsubscribe"), Val::bulk("c"), Val::Int(0)])
    );
    let back = sub.cmd(&["HELLO", "2"]).await;
    assert!(matches!(back, Val::Arr(_)));
    assert_eq!(sub.cmd(&["GET", "nope"]).await, Val::Null);
    assert!(sub.cmd(&["HELLO", "4"]).await.is_err_starting("NOPROTO"));
    assert!(sub.cmd(&["HELLO", "x"]).await.is_err_starting("ERR Protocol version"));
}

#[tokio::test]
async fn subscription_limits_and_bad_arguments() {
    let s = server_with(|c| c.max_subscriptions = 2, KvConfig::default()).await;
    let mut r = s.raw_as("adm").await;
    r.cmd(&["SUBSCRIBE", "a"]).await;
    r.cmd(&["SUBSCRIBE", "b"]).await;
    assert!(
        r.cmd(&["SUBSCRIBE", "c"])
            .await
            .is_err_starting("ERR too many subscriptions")
    );
    assert!(r.cmd(&["SUBSCRIBE"]).await.is_err_starting("ERR wrong number"));
}

#[tokio::test]
async fn a_subscriber_that_stops_reading_is_dropped() {
    let s = server_with(|c| c.write_timeout = ms(300), KvConfig::default()).await;
    let mut sub = s.raw_as("adm").await;
    sub.cmd(&["SUBSCRIBE", "flood"]).await;
    let payload = "x".repeat(64 * 1024);
    let mut p = s.raw_as("adm").await;
    for _ in 0..400 {
        p.cmd(&["PUBLISH", "flood", &payload]).await;
    }
    // the subscriber never read; its connection must be gone
    wait_for(10, || async { (s.handle.connections() == 1).then_some(()) }).await;
    // and the publisher is unaffected
    assert_eq!(p.cmd(&["PING"]).await, Val::Simple("PONG".into()));
}

// ---- wire behaviour ----------------------------------------------------------------------------

#[tokio::test]
async fn pipelines_are_answered_in_order_including_errors() {
    let s = server().await;
    let mut r = s.raw_as("adm").await;
    let mut wire = Vec::new();
    for n in 0..2000 {
        wire.extend(encode(&["SET", &format!("k{n}"), &n.to_string()]));
        wire.extend(encode(&["GET", &format!("k{n}")]));
        if n % 100 == 0 {
            wire.extend(encode(&["LPUSH", &format!("k{n}"), "x"])); // WRONGTYPE in the middle
            wire.extend(encode(&["NOPE"]));
        }
    }
    r.send_raw(&wire).await;
    for n in 0..2000 {
        assert_eq!(r.read().await, Val::ok(), "SET {n}");
        assert_eq!(r.read().await, Val::bulk(&n.to_string()), "GET {n}");
        if n % 100 == 0 {
            assert!(r.read().await.is_err_starting("WRONGTYPE"));
            assert!(r.read().await.is_err_starting("ERR unknown command"));
        }
    }
    assert_eq!(r.cmd(&["DBSIZE"]).await, Val::Int(2000));
}

#[tokio::test]
async fn pipelined_ten_thousand_pings_in_one_write() {
    let s = server().await;
    let mut r = s.raw_as("adm").await;
    let wire = encode(&["PING"]).repeat(10_000);
    r.send_raw(&wire).await;
    for _ in 0..10_000 {
        assert_eq!(r.read().await, Val::Simple("PONG".into()));
    }
}

#[tokio::test]
async fn commands_split_across_many_small_writes_work() {
    let s = server().await;
    let mut r = s.raw_as("adm").await;
    let wire = encode(&["SET", "key", "a value with\r\nCRLF"]);
    for chunk in wire.chunks(3) {
        r.send_raw(chunk).await;
        tokio::time::sleep(ms(2)).await;
    }
    assert_eq!(r.read().await, Val::ok());
    assert_eq!(r.cmd(&["GET", "key"]).await, Val::bulk("a value with\r\nCRLF"));
}

#[tokio::test]
async fn inline_commands_work_like_telnet() {
    let s = server().await;
    let mut r = s.raw().await;
    r.send_raw(b"PING\r\n").await;
    assert_eq!(r.read().await, Val::Simple("PONG".into()));
    r.send_raw(b"AUTH adm\r\nSET \"a key\" 'a value'\r\nGET \"a key\"\r\n")
        .await;
    assert_eq!(r.read().await, Val::ok());
    assert_eq!(r.read().await, Val::ok());
    assert_eq!(r.read().await, Val::bulk("a value"));
    r.send_raw(b"SET \"unbalanced\r\n").await;
    let v = r.read().await;
    assert!(v.is_err_starting("ERR Protocol error: unbalanced quotes"), "{v:?}");
    assert!(r.closed_within(T).await);
}

#[tokio::test]
async fn protocol_errors_answer_once_and_close() {
    let s = server().await;
    for junk in [
        &b"*abc\r\n"[..],
        b"*1\r\n:5\r\n",
        b"*1\r\n$-7\r\n",
        b"*1\r\n$3\r\nabcXY",
    ] {
        let mut r = s.raw().await;
        r.send_raw(junk).await;
        let v = r.read().await;
        assert!(
            v.is_err_starting("ERR Protocol error"),
            "{:?} -> {v:?}",
            String::from_utf8_lossy(junk)
        );
        assert!(r.closed_within(T).await);
    }
    // the listener itself is fine
    let mut r = s.raw().await;
    assert_eq!(r.cmd(&["PING"]).await, Val::Simple("PONG".into()));
}

// ---- limits and timeouts ----------------------------------------------------------------------------

#[tokio::test]
async fn max_bulk_size_is_enforced() {
    let s = server_with(|c| c.max_bulk_bytes = 1024, KvConfig::default()).await;
    let mut r = s.raw_as("adm").await;
    assert_eq!(r.cmd(&["SET", "k", &"x".repeat(1024)]).await, Val::ok());
    r.send(&["SET", "k", &"x".repeat(1025)]).await;
    let v = r.read().await;
    assert!(v.is_err_starting("ERR Protocol error: invalid bulk length"), "{v:?}");
    assert!(r.closed_within(T).await);
    let mut r = s.raw_as("adm").await;
    assert_eq!(
        r.cmd(&["GET", "k"]).await.as_bulk_len(),
        1024,
        "the earlier value is intact"
    );
}

impl Val {
    fn as_bulk_len(&self) -> usize {
        match self {
            Val::Bulk(b) => b.len(),
            other => panic!("not bulk: {other:?}"),
        }
    }
}

#[tokio::test]
async fn the_engine_value_limit_is_an_error_not_a_disconnect() {
    let s = server_with(
        |_| {},
        KvConfig {
            max_value_bytes: 100,
            ..KvConfig::default()
        },
    )
    .await;
    let mut r = s.raw_as("adm").await;
    let v = r.cmd(&["SET", "k", &"x".repeat(101)]).await;
    assert!(v.is_err_starting("ERR"), "{v:?}");
    assert_eq!(
        r.cmd(&["PING"]).await,
        Val::Simple("PONG".into()),
        "the connection survives"
    );
}

#[tokio::test]
async fn connection_limit_refuses_extra_clients() {
    let s = server_with(|c| c.max_connections = 2, KvConfig::default()).await;
    let mut a = s.raw_as("adm").await;
    let mut b = s.raw_as("adm").await;
    let mut third = s.raw().await;
    let v = third.read().await;
    assert!(v.is_err_starting("ERR max number of clients reached"), "{v:?}");
    assert!(third.closed_within(T).await);
    // a slot frees up when a client leaves
    assert_eq!(a.cmd(&["QUIT"]).await, Val::ok());
    drop(a);
    wait_for(5, || async { (s.handle.connections() == 1).then_some(()) }).await;
    let mut again = s.raw_as("adm").await;
    assert_eq!(again.cmd(&["PING"]).await, Val::Simple("PONG".into()));
    assert_eq!(b.cmd(&["PING"]).await, Val::Simple("PONG".into()));
}

#[tokio::test]
async fn idle_connections_are_closed_but_subscribers_are_exempt() {
    let s = server_with(|c| c.idle_timeout = ms(250), KvConfig::default()).await;
    let mut idle = s.raw_as("adm").await;
    let mut sub = s.raw_as("adm").await;
    sub.cmd(&["SUBSCRIBE", "c"]).await;
    let mut busy = s.raw_as("adm").await;
    for _ in 0..6 {
        tokio::time::sleep(ms(100)).await;
        assert_eq!(
            busy.cmd(&["PING"]).await,
            Val::Simple("PONG".into()),
            "activity resets the timer"
        );
    }
    assert!(idle.closed_within(T).await, "idle connection closed");
    let mut p = s.raw_as("adm").await;
    assert_eq!(p.cmd(&["PUBLISH", "c", "still-here"]).await, Val::Int(1));
    assert_eq!(
        sub.read().await,
        Val::Arr(vec![Val::bulk("message"), Val::bulk("c"), Val::bulk("still-here")])
    );
}

// ---- refused and unknown commands ----------------------------------------------------------------------

#[tokio::test]
async fn every_unsupported_command_returns_its_documented_error() {
    let s = server().await;
    let mut r = s.raw_as("adm").await;
    let mut n = 0;
    for (msg, names) in compat::UNSUPPORTED {
        for name in *names {
            let v = r.cmd(&[name, "a", "b"]).await;
            assert_eq!(v, Val::Err((*msg).to_owned()), "{name}");
            n += 1;
        }
    }
    for name in compat::NOT_IMPLEMENTED {
        let v = r.cmd(&[name, "a", "b"]).await;
        assert_eq!(
            v,
            Val::Err(format!("ERR command '{name}' is not supported by ForgeBase KV")),
            "{name}"
        );
        n += 1;
    }
    assert!(n > 100, "{n}");
    // anything else is simply unknown
    let v = r.cmd(&["FROBNICATE", "x"]).await;
    assert_eq!(
        v,
        Val::Err("ERR unknown command 'FROBNICATE', with args beginning with: 'x'".into())
    );
    // lower case works as everywhere in Redis
    assert_eq!(r.cmd(&["multi"]).await, Val::Err(compat::UNSUPPORTED[0].0.into()));
    assert_eq!(
        r.cmd(&["PING"]).await,
        Val::Simple("PONG".into()),
        "the connection is fine"
    );
}

#[tokio::test]
async fn every_supported_command_is_known_to_the_server() {
    let s = server().await;
    let mut r = s.raw_as("adm").await;
    // no supported command may answer "unknown command" (arity errors are fine)
    for name in compat::DATA_COMMANDS.iter().chain(compat::CONNECTION_COMMANDS) {
        if matches!(
            *name,
            "QUIT" | "RESET" | "SUBSCRIBE" | "PSUBSCRIBE" | "UNSUBSCRIBE" | "PUNSUBSCRIBE" | "HELLO" | "AUTH"
        ) {
            continue;
        }
        let v = r.cmd(&[name]).await;
        assert!(
            !v.is_err_starting("ERR unknown command") && !matches!(&v, Val::Err(e) if e.contains("not supported")),
            "{name} -> {v:?}"
        );
    }
}

// ---- shutdown ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn shutdown_closes_connections_stops_accepting_and_finishes_quickly() {
    let s = server().await;
    let addr = s.addr;
    let mut r = s.raw_as("adm").await;
    assert_eq!(r.cmd(&["SET", "k", "v"]).await, Val::ok());
    let mut sub = s.raw_as("adm").await;
    sub.cmd(&["SUBSCRIBE", "c"]).await;
    let started = std::time::Instant::now();
    s.handle.shutdown().await;
    assert!(started.elapsed() < Duration::from_secs(3), "{:?}", started.elapsed());
    assert!(r.closed_within(T).await);
    assert!(sub.closed_within(T).await);
    assert!(
        tokio::net::TcpStream::connect(addr).await.is_err(),
        "no new connections"
    );
}

#[tokio::test]
async fn a_half_sent_command_cannot_stall_shutdown() {
    let s = server_with(|c| c.shutdown_grace = ms(300), KvConfig::default()).await;
    let mut r = s.raw_as("adm").await;
    r.send_raw(b"*3\r\n$3\r\nSET\r\n$1\r\nk\r\n$100\r\nonly-a-bit").await;
    tokio::time::sleep(ms(50)).await;
    let started = std::time::Instant::now();
    s.handle.shutdown().await;
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(r.closed_within(T).await);
}
