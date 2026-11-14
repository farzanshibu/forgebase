//! `topology-setup` and `topology`: a sustained mixed load against a primary
//! with read replicas (and shards), ADR-0029.
//!
//! * `topology-setup` adds `--replicas N` local replicas and one shard per
//!   `--shard-urls` URL through `/api/system/database/*`, and waits for each
//!   operation and for the replicas to enter the read rotation. The server
//!   takes replicas up when it opens its router, so restart it before
//!   `topology` (`run-topology.sh` does).
//! * `topology` runs writers and readers for `--duration` seconds:
//!   * writers `POST` records to `topo_events` (tenant-scoped over
//!     `--tenants` tenants when given, so the writes spread over the shards);
//!   * readers list records (eventual consistency), list with the latest
//!     commit token (`X-ForgeBase-Read-After`, read-your-writes) and run a
//!     SQL probe with `read_from_replica` that names the node that answered.
//!
//! Once a second a sampler reads `GET /api/system/database/topology`: the
//! replicas' lag (ms, bytes), state and rotation. The report gives the lag
//! p50/p95/max per replica, the share of probes each node served and the
//! delta of `forgebase_db_replica_fallbacks_total` (replica reads that fell
//! back to the primary).
//!
//! Reading the numbers: the API holds a read back until a replica has replayed the
//! caller's own writes, so a client never *sees* replica lag; the lag figures are
//! the replicas' apply lag, and the share of reads on replicas is what the lag
//! costs the read scaling.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use forgebase_bench::{Lcg, percentile};
use serde_json::{Value, json};

use crate::http::{Conn, Payload};
use crate::stats::{Stats, emit};
use crate::{Args, run_workers};

const BASE: &str = "/api/system/database";
const COLLECTION: &str = "topo_events";
const OPS: [&str; 4] = [
    "write",
    "read_list_eventual",
    "read_list_after_write",
    "read_probe_replica",
];

async fn topology(c: &mut Conn, token: &str) -> Value {
    let (s, j) = c.json("GET", &format!("{BASE}/topology"), Some(token), None).await;
    assert_eq!(s, 200, "topology: {j}");
    j
}

async fn wait_op(c: &mut Conn, token: &str, id: &str, what: &str) {
    let start = Instant::now();
    loop {
        let (s, j) = c
            .json("GET", &format!("{BASE}/operations/{id}"), Some(token), None)
            .await;
        assert_eq!(s, 200, "operation {id}: {j}");
        match j["status"].as_str() {
            Some("succeeded") => return,
            Some(st @ ("dead" | "cancelled")) => panic!("{what}: operation {st}: {j}"),
            _ => {}
        }
        assert!(
            start.elapsed() < Duration::from_secs(900),
            "{what}: still {} after 15 min",
            j["status"]
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn wait_rotation(c: &mut Conn, token: &str, name: &str) {
    let start = Instant::now();
    loop {
        let t = topology(c, token).await;
        let node = t["nodes"]
            .as_array()
            .and_then(|n| n.iter().find(|n| n["name"] == name))
            .cloned();
        if node.as_ref().is_some_and(|n| n["in_rotation"] == true) {
            return;
        }
        assert!(
            start.elapsed() < Duration::from_secs(300),
            "{name} never entered the read rotation: {node:?}"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

pub(crate) async fn setup(a: &Args, token: &str) {
    let mut c = Conn::new(&a.addr);
    for i in 1..=a.replicas {
        let name = format!("r{i}");
        let t0 = Instant::now();
        let (s, j) = c
            .json(
                "POST",
                &format!("{BASE}/nodes"),
                Some(token),
                Some(&json!({"name": name, "kind": "local"})),
            )
            .await;
        assert_eq!(s, 202, "add replica {name}: {j}");
        wait_op(
            &mut c,
            token,
            j["operation"]["id"].as_str().expect("operation id"),
            &name,
        )
        .await;
        wait_rotation(&mut c, token, &name).await;
        println!(
            "topology-setup: replica {name} in rotation after {:.1}s",
            t0.elapsed().as_secs_f64()
        );
    }
    for (i, url) in a.shard_urls.iter().enumerate() {
        let name = format!("s{}", i + 1);
        let t0 = Instant::now();
        let (s, j) = c
            .json(
                "POST",
                &format!("{BASE}/shards"),
                Some(token),
                Some(
                    &json!({"name": name, "node": {"name": format!("{name}-primary"), "kind": "external", "url": url}}),
                ),
            )
            .await;
        assert_eq!(s, 202, "add shard {name}: {j}");
        wait_op(
            &mut c,
            token,
            j["operation"]["id"].as_str().expect("operation id"),
            &name,
        )
        .await;
        if i == 0 {
            // Tenants live on the shards; control keeps its own data.
            let (s, j) = c
                .json(
                    "PATCH",
                    &format!("{BASE}/shards/control"),
                    Some(token),
                    Some(&json!({"hosts_tenants": false})),
                )
                .await;
            assert_eq!(s, 200, "control stops taking tenants: {j}");
        }
        println!(
            "topology-setup: shard {name} active after {:.1}s",
            t0.elapsed().as_secs_f64()
        );
    }
    let t = topology(&mut c, token).await;
    println!(
        "topology-setup: {} node(s), {} set(s)",
        t["nodes"].as_array().map_or(0, Vec::len),
        t["sets"].as_array().map_or(0, Vec::len)
    );
}

/// Creates `n` tenants (an organisation and a project each; the project's
/// production environment is the tenant) as a throw-away user.
async fn tenants(a: &Args, n: usize) -> Vec<String> {
    let mut c = Conn::new(&a.addr);
    let email = format!("topo-{}@example.com", std::process::id());
    let pw = "topology load password 1";
    let (s, j) = c
        .json(
            "POST",
            "/api/auth/signup",
            None,
            Some(&json!({"email": email, "password": pw})),
        )
        .await;
    assert!(s == 201 || s == 409, "signup: {s} {j}");
    let (s, j) = c
        .json(
            "POST",
            "/api/auth/login",
            None,
            Some(&json!({"email": email, "password": pw})),
        )
        .await;
    assert_eq!(s, 200, "login: {j}");
    let user = j["access_token"].as_str().unwrap().to_owned();
    let mut out = Vec::new();
    for i in 0..n {
        let slug = format!("load{}x{i}", std::process::id());
        let (s, j) = c
            .json(
                "POST",
                "/api/v1/orgs",
                Some(&user),
                Some(&json!({"slug": slug, "name": slug})),
            )
            .await;
        assert_eq!(s, 201, "org: {j}");
        let org = j["id"].as_str().unwrap().to_owned();
        let r = c
            .send(
                "POST",
                &format!("/api/v1/orgs/{org}/projects"),
                Some(&user),
                &[("X-ForgeBase-Org", &org)],
                Payload::Json(&json!({"slug": "app", "name": "app"})),
                true,
            )
            .await;
        assert_eq!(r.status, 201, "project: {}", String::from_utf8_lossy(&r.body));
        let env = r.json()["environments"]
            .as_array()
            .and_then(|e| e.iter().find(|e| e["name"] == "production"))
            .and_then(|e| e["id"].as_str().map(str::to_owned))
            .expect("production environment");
        out.push(env);
    }
    out
}

/// Sum of a counter over its label sets in the Prometheus text.
fn metric_sum(text: &str, name: &str) -> f64 {
    text.lines()
        .filter(|l| l.starts_with(name) && !l.starts_with('#'))
        .filter_map(|l| l.rsplit_once(' ')?.1.parse::<f64>().ok())
        .sum()
}

async fn fallbacks(c: &mut Conn, token: &str) -> Option<f64> {
    let r = c.send("GET", "/metrics", Some(token), &[], Payload::None, true).await;
    (r.status == 200).then(|| {
        metric_sum(
            &String::from_utf8_lossy(&r.body),
            "forgebase_db_replica_fallbacks_total",
        )
    })
}

/// Names the nodes of the probe: `primary`, and `replica-1`, `replica-2`, ...
/// in the order of their postmaster start times (not the topology names).
fn relabel(raw: &BTreeMap<String, u64>) -> BTreeMap<String, u64> {
    let mut out = BTreeMap::new();
    let mut standbys = 0;
    for (k, v) in raw {
        let label = if k.starts_with("standby|") {
            standbys += 1;
            format!("replica-{standbys}")
        } else {
            "primary".to_owned()
        };
        *out.entry(label).or_default() += v;
    }
    out
}

/// One second-by-second view of a replica.
#[derive(Default)]
struct Lag {
    ms: Vec<u64>,
    bytes: Vec<u64>,
    out_of_rotation: u64,
    samples: u64,
    states: BTreeMap<String, u64>,
}

fn lag_json(l: &mut Lag) -> Value {
    json!({
        "samples": l.samples,
        "lag_ms": {"p50": percentile(&mut l.ms, 0.5), "p95": percentile(&mut l.ms, 0.95), "max": percentile(&mut l.ms, 1.0)},
        "lag_bytes": {"p50": percentile(&mut l.bytes, 0.5), "p95": percentile(&mut l.bytes, 0.95), "max": percentile(&mut l.bytes, 1.0)},
        "out_of_rotation_samples": l.out_of_rotation,
        "states": l.states,
    })
}

pub(crate) async fn run(a: &Args, token: &str) {
    let mut c = Conn::new(&a.addr);
    let before = topology(&mut c, token).await;
    let replicas: Vec<String> = before["nodes"]
        .as_array()
        .map(|n| {
            n.iter()
                .filter(|n| n["role"] == "replica")
                .filter_map(|n| n["name"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let shard_sets = before["sets"]
        .as_array()
        .map_or(0, |s| s.iter().filter(|s| s["id"] != "control").count());
    println!(
        "topology: {} replica(s), {shard_sets} shard(s) before the run",
        replicas.len()
    );

    let tenant_scoped = a.tenants > 0;
    let (s, j) = c
        .json(
            "POST",
            "/api/v1/collections",
            Some(token),
            Some(&json!({"name": COLLECTION, "options": {"tenant_scoped": tenant_scoped},
                "fields": [{"name": "k", "type": "text"}, {"name": "n", "type": "number"}]})),
        )
        .await;
    assert!(s == 201 || s == 409, "create collection: {s} {j}");
    let tenant_ids = Arc::new(if tenant_scoped {
        tenants(a, a.tenants).await
    } else {
        Vec::new()
    });
    let fallbacks_before = fallbacks(&mut c, token).await;

    // Lag sampler: the topology view once a second.
    let stop_lag = Arc::new(AtomicBool::new(false));
    let lag_task = {
        let (addr, token, stop) = (a.addr.clone(), token.to_owned(), stop_lag.clone());
        tokio::spawn(async move {
            let mut c = Conn::new(&addr);
            let mut lags: BTreeMap<String, Lag> = BTreeMap::new();
            while !stop.load(Ordering::Relaxed) {
                let t = topology(&mut c, &token).await;
                for n in t["nodes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|n| n["role"] == "replica")
                {
                    let l = lags.entry(n["name"].as_str().unwrap_or("?").to_owned()).or_default();
                    l.samples += 1;
                    l.ms.push(n["lag_ms"].as_u64().unwrap_or(0));
                    l.bytes.push(n["lag_bytes"].as_u64().unwrap_or(0));
                    if n["in_rotation"] != true {
                        l.out_of_rotation += 1;
                    }
                    *l.states
                        .entry(n["state"].as_str().unwrap_or("?").to_owned())
                        .or_default() += 1;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            lags
        })
    };

    let writers = (a.concurrency * a.write_pct / 100).max(1);
    let latest = Arc::new(Mutex::new(String::new()));
    let served: Arc<Mutex<BTreeMap<String, u64>>> = Arc::default();
    let (addr, token_c) = (a.addr.clone(), token.to_owned());
    let (latest_c, served_c, tenants_c) = (latest.clone(), served.clone(), tenant_ids.clone());
    let (stats, secs) = run_workers(a.concurrency.max(2), a.duration, OPS.len(), move |w, stop: Arc<AtomicBool>| {
        let (addr, token, latest, served, tenants) = (addr.clone(), token_c.clone(), latest_c.clone(), served_c.clone(), tenants_c.clone());
        async move {
            let mut c = Conn::new(&addr);
            let mut st: Vec<Stats> = OPS.iter().map(|_| Stats::default()).collect();
            let mut rng = Lcg::new(w as u64 + 3);
            let mut n = 0_u64;
            while !stop.load(Ordering::Relaxed) {
                n += 1;
                let tenant = (!tenants.is_empty()).then(|| tenants[rng.below(tenants.len() as u64) as usize].clone());
                let mut headers: Vec<(&str, String)> = Vec::new();
                if let Some(t) = &tenant {
                    headers.push(("X-ForgeBase-Tenant", t.clone()));
                }
                let op = if w < writers { 0 } else { [1, 2, 3][(n % 3) as usize] };
                let (method, path, payload): (&str, String, Option<Value>) = match op {
                    0 => (
                        "POST",
                        format!("/api/v1/collections/{COLLECTION}/records"),
                        Some(json!({"k": format!("w{w}-{n}"), "n": n})),
                    ),
                    1 | 2 => ("GET", format!("/api/v1/collections/{COLLECTION}/records?per_page=20&skip_total=true"), None),
                    _ => (
                        "POST",
                        "/api/v1/sql".to_owned(),
                        Some(json!({"sql": "SELECT CAST(pg_postmaster_start_time() AS TEXT) AS node, CAST(pg_is_in_recovery() AS INT) AS standby",
                                    "read_from_replica": true})),
                    ),
                };
                if op == 1 {
                    headers.push(("X-ForgeBase-Consistency", "eventual".into()));
                }
                if op == 2 {
                    let t = latest.lock().unwrap().clone();
                    if !t.is_empty() {
                        headers.push(("X-ForgeBase-Read-After", t));
                    }
                }
                let hs: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (*k, v.as_str())).collect();
                let t = Instant::now();
                let r = c
                    .send(method, &path, Some(&token), &hs, payload.as_ref().map_or(Payload::None, Payload::Json), true)
                    .await;
                st[op].us.push(t.elapsed().as_micros() as u64);
                if r.status != 200 && r.status != 201 {
                    st[op].errors += 1;
                    continue;
                }
                match op {
                    0 => {
                        if let Some(tok) = r.header("x-forgebase-commit") {
                            *latest.lock().unwrap() = tok.to_owned();
                        }
                    }
                    3 => {
                        let row = &r.json()["rows"][0];
                        // The postmaster's start time tells the nodes apart.
                        let key = format!("{}|{}", if row[1] == 1 { "standby" } else { "primary" }, row[0].as_str().unwrap_or("?"));
                        *served.lock().unwrap().entry(key).or_default() += 1;
                    }
                    _ => {}
                }
            }
            st
        }
    })
    .await;
    stop_lag.store(true, Ordering::Relaxed);
    let mut lags = lag_task.await.unwrap();
    for (s, op) in stats.into_iter().zip(OPS) {
        s.report_with(
            "topology",
            op,
            secs,
            &[
                ("replicas", json!(replicas.len())),
                ("shards", json!(shard_sets)),
                ("tenants", json!(a.tenants)),
            ],
        );
    }

    let served = relabel(&served.lock().unwrap());
    let probes: u64 = served.values().sum();
    let on_standby: u64 = served
        .iter()
        .filter(|(k, _)| k.starts_with("replica"))
        .map(|(_, v)| v)
        .sum();
    let share = |n: u64| {
        if probes == 0 {
            0.0
        } else {
            (n as f64 / probes as f64 * 1000.0).round() / 10.0
        }
    };
    let by_node: BTreeMap<&String, Value> = served
        .iter()
        .map(|(k, v)| (k, json!({"count": v, "share_pct": share(*v)})))
        .collect();
    let fallbacks_after = fallbacks(&mut c, token).await;
    let after = topology(&mut c, token).await;
    let lag: BTreeMap<String, Value> = lags.iter_mut().map(|(k, v)| (k.clone(), lag_json(v))).collect();
    for (name, l) in &lag {
        emit(
            "topology",
            &format!("replica_lag_{name}"),
            &[
                ("samples", l["samples"].clone()),
                ("lag_ms_p50", l["lag_ms"]["p50"].clone()),
                ("lag_ms_p95", l["lag_ms"]["p95"].clone()),
                ("lag_ms_max", l["lag_ms"]["max"].clone()),
                ("lag_bytes_max", l["lag_bytes"]["max"].clone()),
                ("out_of_rotation", l["out_of_rotation_samples"].clone()),
            ],
        );
    }
    emit(
        "topology",
        "read_distribution",
        &[
            ("probes", json!(probes)),
            ("replica_share_pct", json!(share(on_standby))),
            ("nodes", json!(by_node)),
        ],
    );
    let fallback_delta = fallbacks_before.zip(fallbacks_after).map(|(b, a)| a - b);
    crate::stats::REPORT.lock().unwrap().extra(
        "topology",
        json!({
            "replicas": replicas,
            "shards": shard_sets,
            "tenants": a.tenants,
            "writers": writers,
            "readers": a.concurrency.max(2).saturating_sub(writers),
            "read_distribution": {"probes": probes, "replica_share_pct": share(on_standby), "by_node": by_node},
            "lag": lag,
            "replica_fallbacks_delta": fallback_delta,
            "sets_before": before["sets"],
            "sets_after": after["sets"],
        }),
    );
    if replicas.is_empty() {
        println!("topology: no replicas were configured: every read ran on the primary");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_nodes_get_stable_names() {
        let raw: BTreeMap<String, u64> = [
            ("primary|2026-01-01 00:00:00.1+00", 5),
            ("standby|2026-01-01 00:00:01.0+00", 7),
            ("standby|2026-01-01 00:00:02.0+00", 9),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect();
        let l = relabel(&raw);
        assert_eq!((l["primary"], l["replica-1"], l["replica-2"]), (5, 7, 9));
    }

    #[test]
    fn sums_a_labelled_counter() {
        let text = "# HELP forgebase_db_replica_fallbacks_total x\n# TYPE forgebase_db_replica_fallbacks_total counter\n\
                    forgebase_db_replica_fallbacks_total{reason=\"lag\"} 3\nforgebase_db_replica_fallbacks_total{reason=\"token\"} 4\nother 9\n";
        assert_eq!(metric_sum(text, "forgebase_db_replica_fallbacks_total"), 7.0);
        assert_eq!(metric_sum("", "x"), 0.0);
    }
}
