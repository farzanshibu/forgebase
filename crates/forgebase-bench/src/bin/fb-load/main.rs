//! `fb-load`: a small HTTP load harness for a running ForgeBase server (FB-81).
//!
//! Dev tool, not part of the server. Raw HTTP/1.1 keep-alive over tokio TCP
//! (no client library), WebSocket through the workspace's tokio-tungstenite.
//!
//! ```text
//! fb-load --addr 127.0.0.1:8090 --email root@example.com --password "root password 1" \
//!         --scenario records|auth|realtime|queue|all [--concurrency 32] [--duration 10] [--subscribers 100]
//! ```
//!
//! Every scenario prints one line per operation:
//! `RESULT scenario=<s> op=<o> n=<count> errors=<e> rps=<r> p50_us=<..> p95_us=<..> p99_us=<..>`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use forgebase_bench::percentile;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

/// One keep-alive HTTP/1.1 connection.
struct Conn {
    addr: String,
    stream: Option<TcpStream>,
    buf: Vec<u8>,
}

impl Conn {
    fn new(addr: &str) -> Self {
        Self {
            addr: addr.to_owned(),
            stream: None,
            buf: Vec::with_capacity(8192),
        }
    }

    /// Sends a request and returns (status, body). Reconnects once when the
    /// server closed the idle connection.
    async fn call(&mut self, method: &str, path: &str, token: Option<&str>, body: Option<&Value>) -> (u16, Vec<u8>) {
        for attempt in 0..2 {
            if self.stream.is_none() {
                let s = TcpStream::connect(&self.addr).await.expect("connect");
                s.set_nodelay(true).ok();
                self.stream = Some(s);
            }
            match self.try_call(method, path, token, body).await {
                Ok(r) => return r,
                Err(_) => {
                    self.stream = None;
                    if attempt == 1 {
                        return (0, Vec::new());
                    }
                }
            }
        }
        (0, Vec::new())
    }

    async fn try_call(
        &mut self,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: Option<&Value>,
    ) -> std::io::Result<(u16, Vec<u8>)> {
        let payload = body.map(Value::to_string).unwrap_or_default();
        let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {}\r\n", self.addr);
        if let Some(t) = token {
            req.push_str(&format!("Authorization: Bearer {t}\r\n"));
        }
        if body.is_some() {
            req.push_str(&format!(
                "Content-Type: application/json\r\nContent-Length: {}\r\n",
                payload.len()
            ));
        }
        req.push_str("\r\n");
        req.push_str(&payload);
        let s = self.stream.as_mut().expect("stream");
        s.write_all(req.as_bytes()).await?;
        self.buf.clear();
        let mut chunk = [0_u8; 8192];
        let (head_end, content_length) = loop {
            let n = s.read(&mut chunk).await?;
            if n == 0 {
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            self.buf.extend_from_slice(&chunk[..n]);
            if let Some(pos) = self.buf.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&self.buf[..pos]).to_lowercase();
                let len = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                break (pos + 4, len);
            }
        };
        while self.buf.len() < head_end + content_length {
            let n = s.read(&mut chunk).await?;
            if n == 0 {
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            self.buf.extend_from_slice(&chunk[..n]);
        }
        let head = String::from_utf8_lossy(&self.buf[..head_end]).to_string();
        let status = head
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        if head.to_lowercase().contains("connection: close") {
            self.stream = None;
        }
        Ok((status, self.buf[head_end..head_end + content_length].to_vec()))
    }

    async fn json(&mut self, method: &str, path: &str, token: Option<&str>, body: Option<&Value>) -> (u16, Value) {
        let (s, b) = self.call(method, path, token, body).await;
        (s, serde_json::from_slice(&b).unwrap_or(Value::Null))
    }
}

/// Latency samples of one operation across all workers.
#[derive(Default)]
struct Stats {
    us: Vec<u64>,
    errors: u64,
}

impl Stats {
    fn merge(&mut self, o: Stats) {
        self.us.extend(o.us);
        self.errors += o.errors;
    }
    fn report(mut self, scenario: &str, op: &str, secs: f64) {
        let n = self.us.len() as u64;
        let (p50, p95, p99) = (
            percentile(&mut self.us, 0.50),
            percentile(&mut self.us, 0.95),
            percentile(&mut self.us, 0.99),
        );
        println!(
            "RESULT scenario={scenario} op={op} n={n} errors={} rps={:.0} p50_us={p50} p95_us={p95} p99_us={p99}",
            self.errors,
            n as f64 / secs
        );
    }
}

struct Args {
    addr: String,
    email: String,
    password: String,
    scenario: String,
    concurrency: usize,
    duration: u64,
    subscribers: usize,
}

fn args() -> Args {
    let mut a = Args {
        addr: "127.0.0.1:8090".into(),
        email: "root@example.com".into(),
        password: "root password 1".into(),
        scenario: "all".into(),
        concurrency: 32,
        duration: 10,
        subscribers: 100,
    };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let v = it.next().unwrap_or_default();
        match k.as_str() {
            "--addr" => a.addr = v,
            "--email" => a.email = v,
            "--password" => a.password = v,
            "--scenario" => a.scenario = v,
            "--concurrency" => a.concurrency = v.parse().expect("--concurrency"),
            "--duration" => a.duration = v.parse().expect("--duration"),
            "--subscribers" => a.subscribers = v.parse().expect("--subscribers"),
            other => panic!("unknown argument {other}"),
        }
    }
    a
}

fn now_ns() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() as u64
}

/// Runs `body` on `workers` tasks until the deadline; each returns its per-op stats.
async fn run_workers<F, Fut>(workers: usize, secs: u64, ops: usize, body: F) -> (Vec<Stats>, f64)
where
    F: Fn(usize, Arc<AtomicBool>) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Vec<Stats>> + Send + 'static,
{
    let stop = Arc::new(AtomicBool::new(false));
    let body = Arc::new(body);
    let start = Instant::now();
    let handles: Vec<_> = (0..workers)
        .map(|i| {
            let (b, s) = (body.clone(), stop.clone());
            tokio::spawn(async move { b(i, s).await })
        })
        .collect();
    tokio::time::sleep(Duration::from_secs(secs)).await;
    stop.store(true, Ordering::Relaxed);
    let mut total: Vec<Stats> = (0..ops).map(|_| Stats::default()).collect();
    for h in handles {
        for (t, s) in total.iter_mut().zip(h.await.unwrap()) {
            t.merge(s);
        }
    }
    (total, start.elapsed().as_secs_f64())
}

async fn timed(
    conn: &mut Conn,
    st: &mut Stats,
    method: &str,
    path: &str,
    tok: Option<&str>,
    body: Option<&Value>,
    ok: &[u16],
) -> Value {
    let t = Instant::now();
    let (s, v) = conn.json(method, path, tok, body).await;
    st.us.push(t.elapsed().as_micros() as u64);
    if !ok.contains(&s) {
        st.errors += 1;
    }
    v
}

async fn admin_token(a: &Args) -> String {
    let mut c = Conn::new(&a.addr);
    let (s, j) = c
        .json(
            "POST",
            "/api/auth/admins/login",
            None,
            Some(&json!({"email": a.email, "password": a.password})),
        )
        .await;
    assert_eq!(s, 200, "admin login failed: {j}");
    j["access_token"].as_str().unwrap().to_owned()
}

async fn records(a: &Args, token: &str) {
    let mut c = Conn::new(&a.addr);
    let (s, j) = c
        .json(
            "POST",
            "/api/v1/collections",
            Some(token),
            Some(&json!({"name": "load_notes", "fields": [{"name": "text", "type": "text"}, {"name": "n", "type": "number"}]})),
        )
        .await;
    assert!(s == 201 || s == 409, "create collection: {s} {j}");
    let (addr, token_c) = (a.addr.clone(), token.to_owned());
    let (stats, secs) = run_workers(a.concurrency, a.duration, 4, move |w, stop| {
        let (addr, token) = (addr.clone(), token_c.clone());
        async move {
            let mut c = Conn::new(&addr);
            let mut st = [Stats::default(), Stats::default(), Stats::default(), Stats::default()];
            let mut i = 0_u64;
            while !stop.load(Ordering::Relaxed) {
                i += 1;
                let t = Some(token.as_str());
                let rec = timed(
                    &mut c,
                    &mut st[0],
                    "POST",
                    "/api/v1/collections/load_notes/records",
                    t,
                    Some(&json!({"text": format!("w{w} i{i}"), "n": i})),
                    &[201],
                )
                .await;
                let Some(id) = rec["id"].as_str().map(str::to_owned) else {
                    continue;
                };
                let p = format!("/api/v1/collections/load_notes/records/{id}");
                timed(&mut c, &mut st[1], "GET", &p, t, None, &[200]).await;
                timed(&mut c, &mut st[2], "PATCH", &p, t, Some(&json!({"n": i + 1})), &[200]).await;
                timed(&mut c, &mut st[3], "DELETE", &p, t, None, &[200, 204]).await;
            }
            st.into()
        }
    })
    .await;
    for (s, op) in stats.into_iter().zip(["create", "read", "update", "delete"]) {
        s.report("records", op, secs);
    }
    let mut c = Conn::new(&a.addr);
    timed_list(&mut c, &a.addr, token).await;
}

/// Single-connection list latency over a populated collection.
async fn timed_list(c: &mut Conn, _addr: &str, token: &str) {
    for i in 0..2_000 {
        c.json(
            "POST",
            "/api/v1/collections/load_notes/records",
            Some(token),
            Some(&json!({"text": "seed", "n": i})),
        )
        .await;
    }
    let mut st = Stats::default();
    let t0 = Instant::now();
    for _ in 0..500 {
        timed(
            c,
            &mut st,
            "GET",
            "/api/v1/collections/load_notes/records?filter=n%20%3E%3D%20100&per_page=20&skip_total=true",
            Some(token),
            None,
            &[200],
        )
        .await;
    }
    st.report("records", "list_page_20", t0.elapsed().as_secs_f64());
}

async fn auth(a: &Args) {
    let mut c = Conn::new(&a.addr);
    let (s, j) = c
        .json(
            "POST",
            "/api/auth/signup",
            None,
            Some(&json!({"email": "load@example.com", "password": "load password 1"})),
        )
        .await;
    assert!(s == 201 || s == 409 || s == 429, "signup: {s} {j}");
    let addr = a.addr.clone();
    let (stats, secs) = run_workers(a.concurrency.min(8), a.duration, 3, move |_, stop| {
        let addr = addr.clone();
        async move {
            let mut c = Conn::new(&addr);
            let mut ok = Stats::default();
            let mut limited = Stats::default();
            let mut other = Stats::default();
            while !stop.load(Ordering::Relaxed) {
                let t = Instant::now();
                let (s, _) = c
                    .json(
                        "POST",
                        "/api/auth/login",
                        None,
                        Some(&json!({"email": "load@example.com", "password": "load password 1"})),
                    )
                    .await;
                let us = t.elapsed().as_micros() as u64;
                match s {
                    200 => ok.us.push(us),
                    429 => limited.us.push(us),
                    _ => {
                        other.us.push(us);
                        other.errors += 1;
                    }
                }
            }
            vec![ok, limited, other]
        }
    })
    .await;
    for (s, op) in stats
        .into_iter()
        .zip(["login_200", "login_429_rate_limited", "login_other"])
    {
        s.report("auth", op, secs);
    }
}

async fn queue(a: &Args, token: &str) {
    let mut c = Conn::new(&a.addr);
    let (s, j) = c
        .json(
            "POST",
            "/api/v1/queues/topics",
            Some(token),
            Some(&json!({"name": "load"})),
        )
        .await;
    assert!(s == 201 || s == 200 || s == 409, "topic: {s} {j}");
    let (s, j) = c
        .json(
            "POST",
            "/api/v1/queues/subscriptions",
            Some(token),
            Some(&json!({"name": "load-sub", "topic": "load"})),
        )
        .await;
    assert!(s == 201 || s == 200 || s == 409, "subscription: {s} {j}");
    let (addr, token_c) = (a.addr.clone(), token.to_owned());
    let (stats, secs) = run_workers(a.concurrency, a.duration, 2, move |_, stop| {
        let (addr, token) = (addr.clone(), token_c.clone());
        async move {
            let mut c = Conn::new(&addr);
            let (mut one, mut batch) = (Stats::default(), Stats::default());
            let batch_body = json!({"messages": (0..10).map(|i| json!({"data": {"i": i}})).collect::<Vec<_>>()});
            let mut n = 0_u64;
            while !stop.load(Ordering::Relaxed) {
                n += 1;
                if n.is_multiple_of(4) {
                    timed(
                        &mut c,
                        &mut batch,
                        "POST",
                        "/api/v1/queues/topics/load/publish",
                        Some(&token),
                        Some(&batch_body),
                        &[200, 201],
                    )
                    .await;
                } else {
                    timed(
                        &mut c,
                        &mut one,
                        "POST",
                        "/api/v1/queues/topics/load/publish",
                        Some(&token),
                        Some(&json!({"messages": [{"data": {"n": n}}]})),
                        &[200, 201],
                    )
                    .await;
                }
            }
            vec![one, batch]
        }
    })
    .await;
    for (s, op) in stats.into_iter().zip(["publish_1", "publish_batch_10"]) {
        s.report("queue", op, secs);
    }
    // Drain: pull + ack round trips on one connection.
    let mut st = Stats::default();
    let t0 = Instant::now();
    let mut pulled = 0_u64;
    for _ in 0..200 {
        let t = Instant::now();
        let (s, j) = c
            .json(
                "POST",
                "/api/v1/queues/subscriptions/load-sub/pull",
                Some(token),
                Some(&json!({"max_messages": 100})),
            )
            .await;
        let acks: Vec<Value> = j["messages"]
            .as_array()
            .or_else(|| j.as_array())
            .map(|m| m.iter().filter_map(|x| x.get("ack_id").cloned()).collect())
            .unwrap_or_default();
        if s != 200 {
            st.errors += 1;
        }
        if acks.is_empty() {
            break;
        }
        pulled += acks.len() as u64;
        let (s2, _) = c
            .json(
                "POST",
                "/api/v1/queues/subscriptions/load-sub/ack",
                Some(token),
                Some(&json!({"ack_ids": acks})),
            )
            .await;
        if s2 != 200 {
            st.errors += 1;
        }
        st.us.push(t.elapsed().as_micros() as u64);
    }
    println!(
        "RESULT scenario=queue op=drain_msgs_per_s n={pulled} errors={} rps={:.0}",
        st.errors,
        pulled as f64 / t0.elapsed().as_secs_f64()
    );
    st.report("queue", "pull_ack_100_roundtrip", t0.elapsed().as_secs_f64());
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;

async fn ws_connect(addr: &str, token: &str) -> Ws {
    let mut req = format!("ws://{addr}/api/v1/realtime/ws").into_client_request().unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    tokio_tungstenite::connect_async(req).await.expect("ws connect").0
}

/// `subscribers` sockets on one collection; one publisher writes records; each
/// event's latency is receive time minus the send time embedded in the record.
async fn realtime(a: &Args, token: &str) {
    let mut c = Conn::new(&a.addr);
    let (s, j) = c
        .json(
            "POST",
            "/api/v1/collections",
            Some(token),
            Some(&json!({"name": "load_rt", "fields": [{"name": "sent_ns", "type": "text"}]})),
        )
        .await;
    assert!(s == 201 || s == 409, "create collection: {s} {j}");
    let received = Arc::new(AtomicU64::new(0));
    let samples: Arc<tokio::sync::Mutex<Vec<u64>>> = Arc::default();
    let mut ready = Vec::new();
    for _ in 0..a.subscribers {
        let mut ws = ws_connect(&a.addr, token).await;
        ws.next().await; // welcome
        ws.send(Message::Text(
            json!({"type": "subscribe", "id": "s", "collection": "load_rt"})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
        let (rc, sm) = (received.clone(), samples.clone());
        let (tx, rx) = tokio::sync::oneshot::channel();
        let mut tx = Some(tx);
        tokio::spawn(async move {
            let mut local = Vec::new();
            while let Some(Ok(m)) = ws.next().await {
                if let Message::Text(t) = m {
                    let v: Value = serde_json::from_str(t.as_str()).unwrap_or(Value::Null);
                    match v["type"].as_str() {
                        Some("subscribed") => {
                            if let Some(tx) = tx.take() {
                                tx.send(()).ok();
                            }
                        }
                        Some("record.created") => {
                            if let Some(sent) = v["data"]["sent_ns"].as_str().and_then(|x| x.parse::<u64>().ok()) {
                                local.push(now_ns().saturating_sub(sent) / 1000);
                                rc.fetch_add(1, Ordering::Relaxed);
                                if local.len() >= 256 {
                                    sm.lock().await.append(&mut local);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            sm.lock().await.append(&mut local);
        });
        ready.push(rx);
    }
    for r in ready {
        r.await.ok();
    }
    let (addr, token_s) = (a.addr.clone(), token.to_owned());
    let (stats, secs) = run_workers(4, a.duration, 1, move |_, stop| {
        let (addr, token) = (addr.clone(), token_s.clone());
        async move {
            let mut c = Conn::new(&addr);
            let mut st = Stats::default();
            while !stop.load(Ordering::Relaxed) {
                timed(
                    &mut c,
                    &mut st,
                    "POST",
                    "/api/v1/collections/load_rt/records",
                    Some(&token),
                    Some(&json!({"sent_ns": now_ns().to_string()})),
                    &[201],
                )
                .await;
            }
            vec![st]
        }
    })
    .await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let published = stats[0].us.len() as u64;
    let got = received.load(Ordering::Relaxed);
    let mut lat = samples.lock().await.clone();
    println!(
        "RESULT scenario=realtime op=fanout subscribers={} published={published} delivered={got} expected={} publish_rps={:.0} delivered_per_s={:.0} p50_us={} p95_us={} p99_us={}",
        a.subscribers,
        published * a.subscribers as u64,
        published as f64 / secs,
        got as f64 / secs,
        percentile(&mut lat, 0.50),
        percentile(&mut lat, 0.95),
        percentile(&mut lat, 0.99),
    );
}

#[tokio::main]
async fn main() {
    let a = args();
    let token = admin_token(&a).await;
    let all = a.scenario == "all";
    if all || a.scenario == "records" {
        records(&a, &token).await;
    }
    if all || a.scenario == "auth" {
        auth(&a).await;
    }
    if all || a.scenario == "queue" {
        queue(&a, &token).await;
    }
    if all || a.scenario == "realtime" {
        realtime(&a, &token).await;
    }
}
