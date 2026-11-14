//! `fb-load`: a small HTTP load harness for a running ForgeBase server (FB-81).
//!
//! Dev tool, not part of the server. Raw HTTP/1.1 keep-alive over tokio TCP
//! (no client library), WebSocket through the workspace's tokio-tungstenite.
//!
//! ```text
//! fb-load --addr 127.0.0.1:8090 --email root@example.com --password "root password 1" \
//!         --scenario records,auth,realtime,queue,storage,graph,sql|all|topology-setup|topology \
//!         [--concurrency 32] [--duration 10] [--subscribers 100] \
//!         [--json out.json --backend postgres --label linux-postgres] \
//!         [--pid <server pid> --data-dir <server data dir>]
//! ```
//!
//! Every scenario prints one line per operation:
//! `RESULT scenario=<s> op=<o> n=<count> errors=<e> rps=<r> p50_us=<..> p95_us=<..> p99_us=<..>`.
//!
//! * `--json <file>`: also writes the whole run as one JSON document
//!   (`forgebase_bench::report`), the file CI stores and compares.
//! * `--pid` (and `--data-dir`): samples the server's process tree once a
//!   second during each scenario (CPU %, RSS, open fds, data directory disk
//!   usage; `forgebase_bench::resources`) and prints `RESOURCES` lines and
//!   a `resources` array in the JSON.
//! * Scenario sizes: `--graph-nodes/--graph-edges`, `--sql-customers/
//!   --sql-orders`, `--storage-large-mib` (0 skips the large objects).
//! * `topology-setup` adds `--replicas N` local replicas and the shards of
//!   `--shard-urls u1,u2` through `/api/system/database/*`; restart the
//!   server, then `topology` runs a mixed read/write load and reports replica
//!   lag, the share of reads each node served and fallbacks to the primary
//!   (`run-topology.sh` does all of it).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

mod graph;
mod http;
mod sql;
mod stats;
mod storage;
mod topology;

use forgebase_bench::percentile;
use futures_util::{SinkExt, StreamExt};
use http::Conn;
use serde_json::{Value, json};
use stats::{Stats, emit};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

pub(crate) struct Args {
    pub(crate) addr: String,
    pub(crate) email: String,
    pub(crate) password: String,
    pub(crate) scenario: String,
    pub(crate) concurrency: usize,
    pub(crate) duration: u64,
    pub(crate) subscribers: usize,
    /// `--json`: where to write the report.
    pub(crate) json: Option<String>,
    /// `--backend` and `--label`: free text for the report.
    pub(crate) backend: String,
    pub(crate) label: String,
    /// `--pid` and `--data-dir`: what to sample.
    pub(crate) pid: Option<u32>,
    pub(crate) data_dir: Option<String>,
    pub(crate) sample_interval_ms: u64,
    pub(crate) graph_nodes: usize,
    pub(crate) graph_edges: usize,
    pub(crate) sql_customers: usize,
    pub(crate) sql_orders: usize,
    pub(crate) storage_large_mib: usize,
    /// `topology-setup`: local replicas to add and shard URLs to add.
    pub(crate) replicas: usize,
    pub(crate) shard_urls: Vec<String>,
    /// `topology`: tenants to spread the load over (0: none).
    pub(crate) tenants: usize,
    /// `topology`: share of the workers that write, percent.
    pub(crate) write_pct: usize,
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
        json: None,
        backend: "unknown".into(),
        label: String::new(),
        pid: None,
        data_dir: None,
        sample_interval_ms: 1000,
        graph_nodes: 10_000,
        graph_edges: 50_000,
        sql_customers: 5_000,
        sql_orders: 25_000,
        storage_large_mib: 16,
        replicas: 0,
        shard_urls: Vec::new(),
        tenants: 0,
        write_pct: 30,
    };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let v = it.next().unwrap_or_default();
        let num = |name: &str| v.parse::<usize>().unwrap_or_else(|_| panic!("{name} needs a number"));
        match k.as_str() {
            "--addr" => a.addr = v,
            "--email" => a.email = v,
            "--password" => a.password = v,
            "--scenario" => a.scenario = v,
            "--concurrency" => a.concurrency = num("--concurrency"),
            "--duration" => a.duration = v.parse().expect("--duration"),
            "--subscribers" => a.subscribers = num("--subscribers"),
            "--json" => a.json = Some(v),
            "--backend" => a.backend = v,
            "--label" => a.label = v,
            "--pid" => a.pid = Some(v.parse().expect("--pid")),
            "--data-dir" => a.data_dir = Some(v),
            "--sample-interval-ms" => a.sample_interval_ms = v.parse().expect("--sample-interval-ms"),
            "--graph-nodes" => a.graph_nodes = num("--graph-nodes"),
            "--graph-edges" => a.graph_edges = num("--graph-edges"),
            "--sql-customers" => a.sql_customers = num("--sql-customers"),
            "--sql-orders" => a.sql_orders = num("--sql-orders"),
            "--storage-large-mib" => a.storage_large_mib = num("--storage-large-mib"),
            "--replicas" => a.replicas = num("--replicas"),
            "--shard-urls" => a.shard_urls = v.split(',').filter(|u| !u.is_empty()).map(str::to_owned).collect(),
            "--tenants" => a.tenants = num("--tenants"),
            "--write-pct" => a.write_pct = num("--write-pct"),
            other => panic!("unknown argument {other}"),
        }
    }
    a
}

pub(crate) fn now_ns() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() as u64
}

/// Runs `body` on `workers` tasks until the deadline; each returns its per-op stats.
pub(crate) async fn run_workers<F, Fut>(workers: usize, secs: u64, ops: usize, body: F) -> (Vec<Stats>, f64)
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

pub(crate) async fn timed(
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

pub(crate) async fn admin_token(a: &Args) -> String {
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
    emit(
        "queue",
        "drain_msgs_per_s",
        &[
            ("n", json!(pulled)),
            ("errors", json!(st.errors)),
            ("rps", json!((pulled as f64 / t0.elapsed().as_secs_f64()).round())),
        ],
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
    emit(
        "realtime",
        "fanout",
        &[
            ("subscribers", json!(a.subscribers)),
            ("published", json!(published)),
            ("delivered", json!(got)),
            ("expected", json!(published * a.subscribers as u64)),
            ("publish_rps", json!((published as f64 / secs).round())),
            ("delivered_per_s", json!((got as f64 / secs).round())),
            ("p50_us", json!(percentile(&mut lat, 0.50))),
            ("p95_us", json!(percentile(&mut lat, 0.95))),
            ("p99_us", json!(percentile(&mut lat, 0.99))),
        ],
    );
}

const DEFAULT_SCENARIOS: [&str; 7] = ["records", "auth", "queue", "realtime", "storage", "graph", "sql"];

async fn run_scenario(name: &str, a: &Args, token: &str) {
    match name {
        "records" => records(a, token).await,
        "auth" => auth(a).await,
        "queue" => queue(a, token).await,
        "realtime" => realtime(a, token).await,
        "storage" => storage::run(a, token).await,
        "graph" => graph::run(a, token).await,
        "sql" => sql::run(a, token).await,
        "topology-setup" => topology::setup(a, token).await,
        "topology" => topology::run(a, token).await,
        other => panic!("unknown scenario {other}"),
    }
}

#[tokio::main]
async fn main() {
    let a = args();
    let token = admin_token(&a).await;
    let names: Vec<String> = if a.scenario == "all" {
        DEFAULT_SCENARIOS.iter().map(|s| (*s).to_owned()).collect()
    } else {
        a.scenario
            .split(',')
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect()
    };
    for name in names {
        let sampler = a.pid.map(|pid| {
            forgebase_bench::resources::Sampler::start(
                pid,
                a.data_dir.as_ref().map(std::path::PathBuf::from),
                Duration::from_millis(a.sample_interval_ms),
            )
        });
        run_scenario(&name, &a, &token).await;
        if let Some(sampler) = sampler {
            let summary = sampler.finish().await;
            println!("{}", summary.line(&name));
            stats::REPORT.lock().unwrap().resource(summary.to_json(&name));
        }
    }
    if let Some(path) = &a.json {
        let config = json!({
            "addr": a.addr, "scenario": a.scenario, "concurrency": a.concurrency, "duration_secs": a.duration,
            "subscribers": a.subscribers, "graph_nodes": a.graph_nodes, "graph_edges": a.graph_edges,
            "sql_customers": a.sql_customers, "sql_orders": a.sql_orders, "storage_large_mib": a.storage_large_mib,
            "sampled_pid": a.pid, "sample_interval_ms": a.sample_interval_ms,
        });
        let doc = stats::REPORT.lock().unwrap().document(&a.label, &a.backend, config);
        std::fs::write(path, serde_json::to_string_pretty(&doc).unwrap()).expect("write --json");
        println!("REPORT written to {path}");
    }
}
