//! Live realtime tests: WebSocket, SSE, resume, reconnect, fallback, channels and presence.

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::Server;
use forgebase_sdk::{
    ForgeBase, RealtimeConfig, RealtimeEvent, RealtimeTransport, SignInOptions, SubscribeOptions, Subscription,
};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const WAIT: Duration = Duration::from_secs(10);

async fn collections(s: &Server, names: &[&str]) -> ForgeBase {
    let admin = s.admin().await;
    for n in names {
        admin
            .collections()
            .create(&json!({"name": n, "type": "base", "fields": [{"name": "n", "type": "number"}]}))
            .await
            .expect("create collection");
    }
    admin
}

async fn client(base: &str, cfg: RealtimeConfig) -> ForgeBase {
    let fb = ForgeBase::builder(base).realtime(cfg).build().expect("client");
    fb.auth()
        .sign_in_admin(common::ADMIN_EMAIL, common::ADMIN_PASSWORD, &SignInOptions::default())
        .await
        .expect("sign in");
    fb
}

fn fast() -> RealtimeConfig {
    RealtimeConfig {
        reconnect_base: Duration::from_millis(20),
        reconnect_max: Duration::from_millis(200),
        ..RealtimeConfig::default()
    }
}

/// Reads record events until `n` arrived.
async fn records(sub: &mut Subscription, n: usize) -> Vec<forgebase_sdk::RecordEvent> {
    let mut out = Vec::new();
    while out.len() < n {
        match sub.next_timeout(WAIT).await {
            Some(RealtimeEvent::Record(r)) => out.push(r),
            Some(_) => {}
            None => panic!("expected {n} record events, got {}: {out:?}", out.len()),
        }
    }
    out
}

fn n_of(e: &forgebase_sdk::RecordEvent) -> i64 {
    e.data["n"].as_f64().expect("n") as i64
}

#[tokio::test(flavor = "multi_thread")]
async fn streams_record_events_over_websocket() {
    let Some(s) = Server::start() else { return };
    let admin = collections(&s, &["rt_ws"]).await;
    let fb = client(&s.base, fast()).await;
    let mut sub = fb
        .realtime()
        .subscribe(SubscribeOptions::collection("rt_ws"))
        .await
        .expect("subscribe");

    let col = admin.collection("rt_ws");
    let rec = col.create(&json!({"n": 1})).await.expect("create");
    let id = rec["id"].as_str().expect("id").to_owned();
    col.update(&id, &json!({"n": 2}), forgebase_sdk::WriteOptions::default())
        .await
        .expect("update");
    col.delete(&id).await.expect("delete");

    let got = records(&mut sub, 3).await;
    assert_eq!(
        got.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(),
        ["record.created", "record.updated", "record.deleted"]
    );
    assert!(got.iter().all(|e| e.record_id == id && e.collection == "rt_ws"));
    assert_eq!(sub.cursor(), Some(got[2].cursor));
    assert!(got[0].cursor < got[1].cursor && got[1].cursor < got[2].cursor);
    fb.realtime().close();
}

#[tokio::test(flavor = "multi_thread")]
async fn resumes_from_a_stored_cursor_without_loss_or_duplicates() {
    let Some(s) = Server::start() else { return };
    let admin = collections(&s, &["rt_resume"]).await;
    let col = admin.collection("rt_resume");

    let fb = client(&s.base, fast()).await;
    let mut sub = fb
        .realtime()
        .subscribe(SubscribeOptions::collection("rt_resume"))
        .await
        .expect("subscribe");
    col.create(&json!({"n": 1})).await.expect("create");
    assert_eq!(records(&mut sub, 1).await.len(), 1);
    let cursor = sub.cursor().expect("cursor");
    fb.realtime().close();
    drop(sub);

    // Writes while nobody listens.
    col.create(&json!({"n": 2})).await.expect("create");
    col.create(&json!({"n": 3})).await.expect("create");

    // A new client resumes from the stored cursor: it replays exactly the missed events, then goes live.
    let fb2 = client(&s.base, fast()).await;
    let mut sub2 = fb2
        .realtime()
        .subscribe(SubscribeOptions::collection("rt_resume").from_cursor(cursor))
        .await
        .expect("resume");
    let replay = records(&mut sub2, 2).await;
    assert_eq!(replay.iter().map(n_of).collect::<Vec<_>>(), [2, 3]);
    col.create(&json!({"n": 4})).await.expect("create");
    let live = records(&mut sub2, 1).await;
    assert_eq!(n_of(&live[0]), 4);
    let cursors: std::collections::HashSet<u64> = replay.iter().chain(&live).map(|e| e.cursor).collect();
    assert_eq!(cursors.len(), 3, "no duplicates");
    fb2.realtime().close();
}

#[tokio::test(flavor = "multi_thread")]
async fn streams_over_sse() {
    let Some(s) = Server::start() else { return };
    let admin = collections(&s, &["rt_sse"]).await;
    let cfg = RealtimeConfig {
        transport: RealtimeTransport::Sse,
        ..fast()
    };
    let fb = client(&s.base, cfg).await;
    let mut sub = fb
        .realtime()
        .subscribe(SubscribeOptions::collection("rt_sse"))
        .await
        .expect("subscribe");
    let col = admin.collection("rt_sse");
    col.create(&json!({"n": 5})).await.expect("create");
    let got = records(&mut sub, 1).await;
    assert_eq!(n_of(&got[0]), 5);
    assert_eq!(sub.cursor(), Some(got[0].cursor));

    // A second subscription on the same connection (SSE restarts with both).
    let mut other = fb
        .realtime()
        .subscribe(SubscribeOptions::collection("rt_sse").filter("n > 100"))
        .await
        .expect("second subscription");
    col.create(&json!({"n": 6})).await.expect("create");
    col.create(&json!({"n": 101})).await.expect("create");
    assert_eq!(
        n_of(&records(&mut sub, 1).await[0]),
        6,
        "first sub resumes without replaying 5"
    );
    assert_eq!(n_of(&records(&mut other, 1).await[0]), 101);

    // SSE is receive-only: presence needs the WebSocket transport.
    let err = fb
        .realtime()
        .join_presence("room", None)
        .await
        .expect_err("no presence over SSE");
    assert_eq!(err.code(), "not_connected");
    fb.realtime().close();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_subscription_is_an_error() {
    let Some(s) = Server::start() else { return };
    let _admin = collections(&s, &["rt_denied"]).await;
    let anon = ForgeBase::builder(&s.base)
        .realtime(RealtimeConfig {
            max_attempts: Some(2),
            ..fast()
        })
        .build()
        .expect("client");
    let err = anon
        .realtime()
        .subscribe(SubscribeOptions::collection("rt_denied"))
        .await
        .err()
        .expect("anonymous cannot read");
    assert!(
        err.is_unauthenticated() || err.is_forbidden() || matches!(err, forgebase_sdk::Error::Realtime { .. }),
        "{err}"
    );
    anon.realtime().close();
}

#[tokio::test(flavor = "multi_thread")]
async fn channels_publish_and_presence() {
    let Some(s) = Server::start() else { return };
    let _admin = s.admin().await;
    let a = client(&s.base, fast()).await;
    let b = client(&s.base, fast()).await;
    let mut sub_a = a
        .realtime()
        .subscribe(SubscribeOptions::channel("room"))
        .await
        .expect("subscribe a");
    let mut sub_b = b
        .realtime()
        .subscribe(SubscribeOptions::channel("room"))
        .await
        .expect("subscribe b");

    b.realtime()
        .publish("room", json!({"hello": "world"}))
        .await
        .expect("publish");
    for sub in [&mut sub_a, &mut sub_b] {
        loop {
            match sub.next_timeout(WAIT).await {
                Some(RealtimeEvent::Message(m)) => {
                    assert_eq!(m.channel, "room");
                    assert_eq!(m.data, json!({"hello": "world"}));
                    break;
                }
                Some(_) => {}
                None => panic!("no channel message"),
            }
        }
    }

    let members = a
        .realtime()
        .join_presence("room", Some(json!({"name": "a"})))
        .await
        .expect("join");
    assert_eq!(members.len(), 1);
    assert_eq!(a.realtime().presence("room").await.expect("rest presence").len(), 1);
    loop {
        match sub_b.next_timeout(WAIT).await {
            Some(RealtimeEvent::PresenceJoin(p)) => {
                assert_eq!(p.channel, "room");
                break;
            }
            Some(_) => {}
            None => panic!("no presence.join event"),
        }
    }
    a.realtime().leave_presence("room").await.expect("leave");
    a.realtime().close();
    b.realtime().close();
}

/// A TCP relay in front of the server that tests can sever, or make refuse WebSocket upgrades.
struct Relay {
    addr: String,
    conns: Arc<Mutex<Vec<tokio::task::AbortHandle>>>,
    block_ws: Arc<AtomicBool>,
}

impl Relay {
    async fn start(upstream: &str) -> Self {
        let upstream = upstream.trim_start_matches("http://").to_owned();
        let l = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = format!("http://{}", l.local_addr().expect("addr"));
        let conns: Arc<Mutex<Vec<tokio::task::AbortHandle>>> = Arc::default();
        let block_ws = Arc::new(AtomicBool::new(false));
        let (c2, b2) = (conns.clone(), block_ws.clone());
        tokio::spawn(async move {
            loop {
                let Ok((mut down, _)) = l.accept().await else { return };
                let (up_addr, block, conns) = (upstream.clone(), b2.clone(), c2.clone());
                let h = tokio::spawn(async move {
                    let mut head = Vec::new();
                    let mut buf = [0u8; 4096];
                    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                        match down.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => head.extend_from_slice(&buf[..n]),
                        }
                    }
                    if block.load(Ordering::SeqCst)
                        && String::from_utf8_lossy(&head)
                            .to_ascii_lowercase()
                            .contains("upgrade: websocket")
                    {
                        return; // refuse: drop the connection
                    }
                    let Ok(mut up) = TcpStream::connect(&up_addr).await else {
                        return;
                    };
                    if up.write_all(&head).await.is_err() {
                        return;
                    }
                    let _ = tokio::io::copy_bidirectional(&mut down, &mut up).await;
                });
                conns.lock().expect("lock").push(h.abort_handle());
            }
        });
        Self { addr, conns, block_ws }
    }

    fn sever(&self) {
        for h in self.conns.lock().expect("lock").drain(..) {
            h.abort();
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn reconnects_and_resumes_after_the_connection_drops() {
    let Some(s) = Server::start() else { return };
    let admin = collections(&s, &["rt_drop"]).await;
    let relay = Relay::start(&s.base).await;
    let fb = client(&relay.addr, fast()).await;
    let mut sub = fb
        .realtime()
        .subscribe(SubscribeOptions::collection("rt_drop"))
        .await
        .expect("subscribe");
    let col = admin.collection("rt_drop");

    col.create(&json!({"n": 1})).await.expect("create");
    assert_eq!(n_of(&records(&mut sub, 1).await[0]), 1);

    relay.sever();
    // Written while the client is (re)connecting: must still arrive, once.
    col.create(&json!({"n": 2})).await.expect("create");
    col.create(&json!({"n": 3})).await.expect("create");
    let got = records(&mut sub, 2).await;
    assert_eq!(got.iter().map(n_of).collect::<Vec<_>>(), [2, 3]);

    col.create(&json!({"n": 4})).await.expect("create");
    assert_eq!(n_of(&records(&mut sub, 1).await[0]), 4);
    fb.realtime().close();
}

#[tokio::test(flavor = "multi_thread")]
async fn falls_back_to_sse_when_websocket_cannot_connect() {
    let Some(s) = Server::start() else { return };
    let admin = collections(&s, &["rt_fallback"]).await;
    let relay = Relay::start(&s.base).await;
    relay.block_ws.store(true, Ordering::SeqCst);
    let fb = client(
        &relay.addr,
        RealtimeConfig {
            fallback_after: 2,
            ..fast()
        },
    )
    .await;
    let mut sub = fb
        .realtime()
        .subscribe(SubscribeOptions::collection("rt_fallback"))
        .await
        .expect("falls back to SSE and subscribes");
    admin
        .collection("rt_fallback")
        .create(&json!({"n": 7}))
        .await
        .expect("create");
    assert_eq!(n_of(&records(&mut sub, 1).await[0]), 7);
    fb.realtime().close();
}

#[tokio::test(flavor = "multi_thread")]
async fn gives_up_and_reports_when_the_server_is_gone() {
    let Some(s) = Server::start() else { return };
    let _admin = s.admin().await;
    let relay = Relay::start(&s.base).await;
    let fb = client(
        &relay.addr,
        RealtimeConfig {
            max_attempts: Some(3),
            transport: RealtimeTransport::WebSocket,
            ..fast()
        },
    )
    .await;
    drop(s); // server killed: every reconnect is refused
    let _ = relay.addr.clone();
    let err = fb
        .realtime()
        .subscribe(SubscribeOptions::collection("anything"))
        .await
        .err()
        .expect("no server");
    assert!(
        matches!(
            err,
            forgebase_sdk::Error::Network(_) | forgebase_sdk::Error::Realtime { .. }
        ),
        "{err}"
    );
}
