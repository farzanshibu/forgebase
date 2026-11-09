//! Queue helpers against a scripted HTTP server. Hermetic: no ForgeBase binary needed.

use std::sync::{Arc, Mutex};

use forgebase_sdk::{
    ErrorKind, ForgeBase, NewMessage, NewSubscription, NewTopic, PullOptions, RetryPolicy, SeekTo, StreamOptions,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// One reply: status, content type, body.
type Reply = (u16, &'static str, String);

fn js(status: u16, v: &Value) -> Reply {
    (status, "application/json", v.to_string())
}

#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    body: String,
}

struct Mock {
    base: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Mock {
    /// Replies are consumed in order; the last one repeats.
    async fn start(script: Vec<Reply>) -> Self {
        let l = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let base = format!("http://{}", l.local_addr().expect("addr"));
        let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
        let (s2, script) = (seen.clone(), Arc::new(script));
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = l.accept().await else { return };
                let (seen, script) = (s2.clone(), script.clone());
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    let head_end = loop {
                        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            break i + 4;
                        }
                        match sock.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                    };
                    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
                    let mut first = head.lines().next().unwrap_or_default().split(' ');
                    let (method, path) = (
                        first.next().unwrap_or_default().to_owned(),
                        first.next().unwrap_or_default().to_owned(),
                    );
                    let len = head
                        .lines()
                        .filter_map(|l| l.split_once(':'))
                        .find(|(k, _)| k.trim().eq_ignore_ascii_case("content-length"))
                        .and_then(|(_, v)| v.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    while buf.len() < head_end + len {
                        match sock.read(&mut chunk).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                    }
                    let body = String::from_utf8_lossy(&buf[head_end.min(buf.len())..]).into_owned();
                    let n = {
                        let mut g = seen.lock().expect("lock");
                        g.push(Seen { method, path, body });
                        g.len()
                    };
                    let (status, ct, body) = script[(n - 1).min(script.len() - 1)].clone();
                    let out = format!(
                        "HTTP/1.1 {status} X\r\ncontent-type: {ct}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = sock.write_all(out.as_bytes()).await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        Self { base, seen }
    }

    fn calls(&self) -> Vec<Seen> {
        self.seen.lock().expect("lock").clone()
    }

    fn client(&self) -> ForgeBase {
        ForgeBase::builder(&self.base)
            .retry(RetryPolicy::none())
            .build()
            .expect("client")
    }
}

fn received(n: i64) -> Value {
    json!({"ack_id": format!("a{n}"), "delivery_attempt": 1, "ack_deadline": "t",
        "message": {"id": format!("m{n}"), "topic": "t", "seq": n, "data": {"n": n}, "attributes": {"k": "v"}, "published_at": "t"}})
}

#[tokio::test]
async fn publishes_with_attributes_ordering_and_dedup() {
    let m = Mock::start(vec![js(
        200,
        &json!({"results": [{"message_id": "m1", "seq": 4, "duplicate": false}]}),
    )])
    .await;
    let r = m
        .client()
        .queues()
        .publish(
            "orders",
            &NewMessage::new(json!({"id": 1}))
                .attr("kind", "rush")
                .ordering_key("u1")
                .dedup_id("d1"),
        )
        .await
        .expect("published");
    assert_eq!((r.message_id.as_str(), r.seq, r.duplicate), ("m1", 4, false));
    let c = &m.calls()[0];
    assert_eq!(
        (c.method.as_str(), c.path.as_str()),
        ("POST", "/api/v1/queues/topics/orders/publish")
    );
    let body: Value = serde_json::from_str(&c.body).expect("json");
    assert_eq!(
        body,
        json!({"messages": [{"data": {"id": 1}, "attributes": {"kind": "rush"}, "ordering_key": "u1", "dedup_id": "d1"}]})
    );
}

#[tokio::test]
async fn topic_errors_carry_the_server_code() {
    let m = Mock::start(vec![js(
        409,
        &json!({"error": {"code": "topic_exists", "message": "exists"}}),
    )])
    .await;
    let e = m
        .client()
        .queues()
        .create_topic(&NewTopic::new("t"))
        .await
        .expect_err("conflict");
    match e {
        forgebase_sdk::Error::Api(a) => {
            assert_eq!(a.code, "topic_exists");
            assert_eq!(a.kind(), ErrorKind::Conflict);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test]
async fn lists_topics_creates_subscription_and_deletes_with_force() {
    let m = Mock::start(vec![
        js(200, &json!({"items": [{"name": "orders", "published_total": 3}]})),
        js(
            201,
            &json!({"name": "billing", "topic": "orders", "ack_deadline_ms": 30000}),
        ),
        (204, "application/json", String::new()),
    ])
    .await;
    let q = m.client().queues();
    assert_eq!(q.list_topics().await.expect("topics")[0].published_total, 3);
    let mut s = NewSubscription::new("billing", "orders");
    s.ack_deadline_ms = Some(30_000);
    s.max_delivery_attempts = Some(5);
    assert_eq!(q.create_subscription(&s).await.expect("sub").ack_deadline_ms, 30_000);
    q.delete_topic("orders", true).await.expect("deleted");
    let calls = m.calls();
    assert_eq!(
        serde_json::from_str::<Value>(&calls[1].body).expect("json"),
        json!({"name": "billing", "topic": "orders", "ack_deadline_ms": 30000, "max_delivery_attempts": 5})
    );
    assert_eq!(calls[2].path, "/api/v1/queues/topics/orders?force=true");
}

#[tokio::test]
async fn pulls_acks_nacks_and_seeks() {
    let m = Mock::start(vec![
        js(200, &json!({"messages": [received(1), received(2)]})),
        js(200, &json!({"count": 1, "unknown": []})),
        js(200, &json!({"count": 0, "unknown": ["a2"]})),
        js(200, &json!({"cursor_seq": 1, "restored": 3})),
    ])
    .await;
    let q = m.client().queues();
    let got = q
        .pull(
            "s",
            &PullOptions {
                max_messages: Some(2),
                wait_ms: Some(500),
                ..PullOptions::default()
            },
        )
        .await
        .expect("pulled");
    assert_eq!(got.len(), 2);
    assert_eq!(got[1].message.data["n"], 2);
    assert_eq!(q.ack("s", &[got[0].ack_id.clone()]).await.expect("ack").count, 1);
    assert_eq!(
        q.nack("s", &[got[1].ack_id.clone()]).await.expect("nack").unknown,
        vec!["a2"]
    );
    assert_eq!(
        q.seek("s", &SeekTo::Snapshot("snap".into()))
            .await
            .expect("seek")
            .restored,
        3
    );
    let calls = m.calls();
    assert_eq!(
        serde_json::from_str::<Value>(&calls[0].body).expect("json"),
        json!({"max_messages": 2, "wait_ms": 500})
    );
    assert_eq!(calls[1].path, "/api/v1/queues/subscriptions/s/ack");
    assert_eq!(
        serde_json::from_str::<Value>(&calls[2].body).expect("json"),
        json!({"ack_ids": ["a2"]})
    );
    assert_eq!(
        serde_json::from_str::<Value>(&calls[3].body).expect("json"),
        json!({"snapshot": "snap"})
    );
}

#[tokio::test]
async fn streaming_pull_yields_deliveries_and_settles_them() {
    let sse = format!(
        ": hello\n\nevent: message\ndata: {}\n\nevent: ping\ndata: {{}}\n\nevent: message\ndata: {}\n\n",
        received(1),
        received(2)
    );
    let m = Mock::start(vec![
        (200, "text/event-stream", sse),
        js(200, &json!({"count": 1, "unknown": []})),
        js(200, &json!({"count": 1, "unknown": []})),
    ])
    .await;
    let q = m.client().queues();
    let mut st = q
        .subscribe(
            "s",
            &StreamOptions {
                max_messages: Some(5),
                max_bytes: None,
            },
        )
        .await
        .expect("stream");
    let a = st.next().await.expect("first").expect("some");
    let b = st.next().await.expect("second").expect("some");
    assert_eq!(
        (a.message.data["n"].clone(), b.message.data["n"].clone()),
        (json!(1), json!(2))
    );
    st.ack(&a).await.expect("ack");
    st.nack(&b).await.expect("nack");
    assert!(st.next().await.expect("end").is_none());
    let calls = m.calls();
    assert_eq!(calls[0].path, "/api/v1/queues/subscriptions/s/events?max_messages=5");
    assert_eq!(calls[1].path, "/api/v1/queues/subscriptions/s/ack");
    assert_eq!(calls[2].path, "/api/v1/queues/subscriptions/s/nack");
}

#[tokio::test]
async fn stream_error_event_becomes_a_realtime_error() {
    let m = Mock::start(vec![(
        200,
        "text/event-stream",
        "event: error\ndata: {\"code\":\"push_subscription\",\"message\":\"push subscription\"}\n\n".into(),
    )])
    .await;
    let mut st = m
        .client()
        .queues()
        .subscribe("s", &StreamOptions::default())
        .await
        .expect("stream");
    match st.next().await.expect_err("error event") {
        forgebase_sdk::Error::Realtime { code, .. } => assert_eq!(code, "push_subscription"),
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test]
async fn stream_refusal_is_an_api_error() {
    let m = Mock::start(vec![js(403, &json!({"error": {"code": "forbidden", "message": "no"}}))]).await;
    let r = m.client().queues().subscribe("s", &StreamOptions::default()).await;
    assert!(matches!(r, Err(forgebase_sdk::Error::Api(a)) if a.status == 403));
}
