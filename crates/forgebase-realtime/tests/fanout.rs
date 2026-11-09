//! Cross-instance fan-out through `RedisFanOut` against an in-process fake
//! Redis that speaks the RESP2 pub/sub subset (no Redis needed).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{Stack, admin, fast_config};
use forgebase_realtime::{FanOut, RedisFanOut};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

type Subscribers = Arc<Mutex<Vec<(String, mpsc::UnboundedSender<Vec<u8>>)>>>;

fn bulk(b: &[u8]) -> Vec<u8> {
    let mut v = format!("${}\r\n", b.len()).into_bytes();
    v.extend_from_slice(b);
    v.extend_from_slice(b"\r\n");
    v
}

/// A fake Redis: `SUBSCRIBE`, `PUBLISH`, `AUTH`. Returns its address and the
/// subscriber list.
async fn fake_redis() -> (String, Subscribers) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let subs: Subscribers = Arc::default();
    let s = subs.clone();
    tokio::spawn(async move {
        loop {
            let (sock, _) = listener.accept().await.unwrap();
            let subs = s.clone();
            tokio::spawn(async move {
                let (r, mut w) = sock.into_split();
                let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
                tokio::spawn(async move {
                    while let Some(b) = rx.recv().await {
                        if w.write_all(&b).await.is_err() {
                            return;
                        }
                    }
                });
                let mut r = BufReader::new(r);
                loop {
                    let mut line = String::new();
                    if r.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    let n: usize = line.trim().trim_start_matches('*').parse().unwrap();
                    let mut args = Vec::new();
                    for _ in 0..n {
                        let mut l = String::new();
                        r.read_line(&mut l).await.unwrap();
                        let len: usize = l.trim().trim_start_matches('$').parse().unwrap();
                        let mut buf = vec![0; len + 2];
                        r.read_exact(&mut buf).await.unwrap();
                        buf.truncate(len);
                        args.push(buf);
                    }
                    match args[0].as_slice() {
                        b"SUBSCRIBE" => {
                            let ch = String::from_utf8(args[1].clone()).unwrap();
                            let mut reply = b"*3\r\n".to_vec();
                            reply.extend(bulk(b"subscribe"));
                            reply.extend(bulk(ch.as_bytes()));
                            reply.extend(b":1\r\n");
                            tx.send(reply).unwrap();
                            subs.lock().unwrap().push((ch, tx.clone()));
                        }
                        b"PUBLISH" => {
                            let ch = String::from_utf8(args[1].clone()).unwrap();
                            let mut msg = b"*3\r\n".to_vec();
                            msg.extend(bulk(b"message"));
                            msg.extend(bulk(ch.as_bytes()));
                            msg.extend(bulk(&args[2]));
                            let mut n = 0;
                            for (c, s) in subs.lock().unwrap().iter() {
                                if *c == ch && s.send(msg.clone()).is_ok() {
                                    n += 1;
                                }
                            }
                            tx.send(format!(":{n}\r\n").into_bytes()).unwrap();
                        }
                        b"AUTH" => tx.send(b"+OK\r\n".to_vec()).unwrap(),
                        _ => tx.send(b"-ERR unknown command\r\n".to_vec()).unwrap(),
                    }
                }
            });
        }
    });
    (addr, subs)
}

#[tokio::test]
async fn channel_messages_and_presence_cross_instances() {
    let (addr, subs) = fake_redis().await;
    let url = format!("redis://:pw@{addr}");
    let one: Arc<dyn FanOut> = Arc::new(RedisFanOut::new(&url, "forgebase").unwrap());
    let two: Arc<dyn FanOut> = Arc::new(RedisFanOut::new(&url, "forgebase").unwrap());
    let a = Stack::start_with(fast_config(), Some(one)).await;
    let b = Stack::start_with(fast_config(), Some(two)).await;
    for _ in 0..200 {
        if subs.lock().unwrap().len() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(subs.lock().unwrap().len(), 2, "both instances subscribed");

    let (_u, token, _) = a.user("ann@example.com").await;
    let mut ws = a.ws(Some(&token)).await;
    ws.subscribe(json!({"id": "c", "channel": "public:room"})).await;

    // Published on instance B, delivered on instance A.
    b.rt.publish(&admin(), "public:room", json!({"hello": "from b"}))
        .await
        .unwrap();
    let m = ws.next_json().await;
    assert_eq!(m["type"], "message", "{m}");
    assert_eq!(m["data"]["hello"], "from b");
    assert_eq!(m["from"], "admin:root");

    // Presence joined on A is listed on B.
    ws.send(json!({"type": "presence.join", "channel": "public:room", "meta": {"n": 1}}))
        .await;
    let mut listed = Vec::new();
    for _ in 0..200 {
        listed = b.rt.presence(&admin(), "public:room").unwrap();
        if !listed.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].instance, a.rt.instance_id());
    // Disconnecting on A removes it on B.
    drop(ws);
    for _ in 0..200 {
        if b.rt.presence(&admin(), "public:room").unwrap().is_empty() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("remote presence not removed");
}
