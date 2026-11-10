//! Crash recovery of the `forgebase` binary (FB-81, SPEC §20 and §21): `kill -9`
//! while clients write, restart, then (1) time to ready, (2) every write the
//! server acknowledged is still there, (3) `doctor` finds the database sound.
//!
//! Times are printed as `RECOVERY ...` lines (run with `--nocapture`) and the
//! test asserts only a loose ceiling; the numbers live in docs/benchmarks.md.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::HashSet;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{free_port, http, startup_timeout, wait_healthy};
use serde_json::{Value, json};

const WRITERS: usize = 8;
const CYCLES: usize = 3;
const READY_CEILING: Duration = Duration::from_secs(10);

fn forgebase(cwd: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_forgebase"));
    cmd.current_dir(cwd);
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("FORGEBASE_") {
            cmd.env_remove(k);
        }
    }
    cmd.env("NO_COLOR", "1");
    cmd
}

fn run(cwd: &Path, args: &[&str], input: Option<&str>) -> Output {
    let mut child = forgebase(cwd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(i) = input {
        child.stdin.take().unwrap().write_all(i.as_bytes()).unwrap();
    }
    child.wait_with_output().unwrap()
}

fn call(
    addr: &str,
    method: &str,
    path: &str,
    bearer: Option<&str>,
    body: Option<&Value>,
) -> std::io::Result<(u16, Value)> {
    let mut s = TcpStream::connect(addr)?;
    s.set_read_timeout(Some(Duration::from_secs(10)))?;
    let payload = body.map(Value::to_string).unwrap_or_default();
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
    if let Some(t) = bearer {
        head.push_str(&format!("Authorization: Bearer {t}\r\n"));
    }
    if body.is_some() {
        head.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            payload.len()
        ));
    }
    head.push_str("\r\n");
    s.write_all(head.as_bytes())?;
    s.write_all(payload.as_bytes())?;
    let mut buf = Vec::new();
    s.read_to_end(&mut buf)?;
    let t = String::from_utf8_lossy(&buf).into_owned();
    let (h, b) = t.split_once("\r\n\r\n").unwrap_or((&t, ""));
    let status = h
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    Ok((status, serde_json::from_str(b).unwrap_or(Value::Null)))
}

struct Serve {
    child: Child,
    addr: String,
}

impl Serve {
    /// Spawns the server and returns it with the time until `/health/ready` answers 200.
    fn start(cwd: &Path, port: u16) -> (Self, Duration) {
        let t = Instant::now();
        let child = forgebase(cwd)
            .args(["serve", "--port", &port.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let s = Self {
            child,
            addr: format!("127.0.0.1:{port}"),
        };
        assert!(
            wait_healthy(&s.addr, startup_timeout()),
            "server did not become healthy"
        );
        while !matches!(http("GET", &s.addr, "/health/ready"), Ok((200, _, _))) {
            assert!(t.elapsed() < startup_timeout(), "server never became ready");
            std::thread::sleep(Duration::from_millis(5));
        }
        let ready = t.elapsed();
        (s, ready)
    }

    /// SIGKILL: no shutdown hook runs, the WAL is left as it is.
    fn kill9(mut self) {
        Command::new("kill")
            .args(["-KILL", &self.child.id().to_string()])
            .status()
            .unwrap();
        self.child.wait().unwrap();
    }
}

impl Drop for Serve {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn project(root: &Path) -> std::path::PathBuf {
    assert!(run(root, &["init", "app"], None).status.success());
    let p = root.join("app");
    let out = run(
        &p,
        &["admin", "create", "--email", "root@example.com", "--password-stdin"],
        Some("root password 1\n"),
    );
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    p
}

fn admin_token(addr: &str) -> String {
    let (s, j) = call(
        addr,
        "POST",
        "/api/auth/admins/login",
        None,
        Some(&json!({"email": "root@example.com", "password": "root password 1"})),
    )
    .unwrap();
    assert_eq!(s, 200, "{j}");
    j["access_token"].as_str().unwrap().to_owned()
}

/// Runs `WRITERS` threads that call `write(thread, i)` in a loop and record the
/// keys the server acknowledged. Stops them `after` the start by SIGKILLing the
/// server, then joins them (their connections fail once it is gone).
fn write_then_kill(
    server: Serve,
    after: Duration,
    write: impl Fn(usize, u64) -> Option<String> + Send + Sync + 'static,
) -> HashSet<String> {
    let acked = Arc::new(Mutex::new(HashSet::new()));
    let dead = Arc::new(AtomicBool::new(false));
    let write = Arc::new(write);
    let handles: Vec<_> = (0..WRITERS)
        .map(|t| {
            let (acked, dead, write) = (acked.clone(), dead.clone(), write.clone());
            std::thread::spawn(move || {
                let mut i = 0_u64;
                while !dead.load(Ordering::Relaxed) {
                    i += 1;
                    if let Some(key) = write(t, i) {
                        acked.lock().unwrap().insert(key);
                    }
                }
            })
        })
        .collect();
    std::thread::sleep(after);
    server.kill9();
    dead.store(true, Ordering::Relaxed);
    for h in handles {
        h.join().unwrap();
    }
    Arc::try_unwrap(acked).unwrap().into_inner().unwrap()
}

fn doctor_ok(cwd: &Path) {
    let out = run(cwd, &["doctor", "--json"], None);
    let report: Value = serde_json::from_slice(&out.stdout).expect("doctor --json");
    assert!(out.status.success(), "doctor failed: {report}");
}

#[test]
fn kill9_during_record_writes_loses_no_acknowledged_write() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let project = project(&root);
    let port = free_port();
    let mut all: HashSet<String> = HashSet::new();
    let (mut server, mut ready) = Serve::start(&project, port);
    println!("RECOVERY cold_start_to_ready_ms={}", ready.as_millis());
    let token = admin_token(&server.addr);
    let (s, j) = call(
        &server.addr,
        "POST",
        "/api/v1/collections",
        Some(&token),
        Some(&json!({"name": "rec", "fields": [{"name": "k", "type": "text"}]})),
    )
    .unwrap();
    assert_eq!(s, 201, "{j}");

    for cycle in 0..CYCLES {
        let addr = server.addr.clone();
        let tok = token.clone();
        let started = Instant::now();
        let acked = write_then_kill(server, Duration::from_secs(3), move |t, i| {
            let key = format!("c{cycle}-w{t}-{i}");
            match call(
                &addr,
                "POST",
                "/api/v1/collections/rec/records",
                Some(&tok),
                Some(&json!({"k": key})),
            ) {
                Ok((201, _)) => Some(key),
                _ => None,
            }
        });
        let wal = std::fs::read_dir(project.join("forgebase-data"))
            .map(|d| {
                d.flatten()
                    .filter(|e| e.file_name().to_string_lossy().ends_with("-wal"))
                    .map(|e| e.metadata().map(|m| m.len()).unwrap_or(0))
                    .sum::<u64>()
            })
            .unwrap_or(0);
        all.extend(acked.iter().cloned());

        let (s2, took) = Serve::start(&project, port);
        server = s2;
        ready = took;
        assert!(ready < READY_CEILING, "slow recovery: {ready:?}");
        // Read everything back with a fresh token (the session is in the database too).
        let token2 = admin_token(&server.addr);
        let mut seen: HashSet<String> = HashSet::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut path = "/api/v1/collections/rec/records?per_page=500&skip_total=true".to_owned();
            if let Some(c) = &cursor {
                path.push_str(&format!("&cursor={c}"));
            }
            let (s, j) = call(&server.addr, "GET", &path, Some(&token2), None).unwrap();
            assert_eq!(s, 200, "{j}");
            for r in j["items"].as_array().unwrap() {
                seen.insert(r["k"].as_str().unwrap().to_owned());
            }
            cursor = j["next_cursor"].as_str().map(str::to_owned);
            if cursor.is_none() {
                break;
            }
        }
        let lost: Vec<&String> = all.iter().filter(|k| !seen.contains(*k)).collect();
        println!(
            "RECOVERY cycle={cycle} writers={WRITERS} write_secs={:.1} acked_this_cycle={} total_acked={} rows_after_restart={} lost_acked={} wal_bytes_at_kill_approx={wal} kill9_to_ready_ms={}",
            started.elapsed().as_secs_f64(),
            acked.len(),
            all.len(),
            seen.len(),
            lost.len(),
            ready.as_millis()
        );
        assert!(
            lost.is_empty(),
            "acknowledged writes lost after kill -9: {} e.g. {:?}",
            lost.len(),
            &lost[..lost.len().min(5)]
        );
        assert!(
            !acked.is_empty(),
            "no write was acknowledged; the test measured nothing"
        );
    }
    drop(server);
    doctor_ok(&project);
}

#[test]
fn kill9_during_queue_publish_loses_no_acknowledged_message() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let project = project(&root);
    let port = free_port();
    let (server, _) = Serve::start(&project, port);
    let token = admin_token(&server.addr);
    let (s, j) = call(
        &server.addr,
        "POST",
        "/api/v1/queues/topics",
        Some(&token),
        Some(&json!({"name": "rec"})),
    )
    .unwrap();
    assert!(s == 200 || s == 201, "{s} {j}");
    let (s, j) = call(
        &server.addr,
        "POST",
        "/api/v1/queues/subscriptions",
        Some(&token),
        Some(&json!({"name": "rec-sub", "topic": "rec"})),
    )
    .unwrap();
    assert!(s == 200 || s == 201, "{s} {j}");

    let addr = server.addr.clone();
    let tok = token.clone();
    let acked = write_then_kill(server, Duration::from_secs(3), move |t, i| {
        let key = format!("w{t}-{i}");
        match call(
            &addr,
            "POST",
            "/api/v1/queues/topics/rec/publish",
            Some(&tok),
            Some(&json!({"messages": [{"data": {"k": key}}]})),
        ) {
            Ok((200 | 201, _)) => Some(key),
            _ => None,
        }
    });

    let (server, ready) = Serve::start(&project, port);
    assert!(ready < READY_CEILING, "slow recovery: {ready:?}");
    let token = admin_token(&server.addr);
    let mut seen: HashSet<String> = HashSet::new();
    loop {
        let (s, j) = call(
            &server.addr,
            "POST",
            "/api/v1/queues/subscriptions/rec-sub/pull",
            Some(&token),
            Some(&json!({"max_messages": 1000})),
        )
        .unwrap();
        assert_eq!(s, 200, "{j}");
        let msgs: Vec<Value> = j["messages"]
            .as_array()
            .or_else(|| j.as_array())
            .cloned()
            .unwrap_or_default();
        if msgs.is_empty() {
            break;
        }
        let mut acks = Vec::new();
        for m in &msgs {
            seen.insert(m["message"]["data"]["k"].as_str().unwrap().to_owned());
            acks.push(m["ack_id"].clone());
        }
        call(
            &server.addr,
            "POST",
            "/api/v1/queues/subscriptions/rec-sub/ack",
            Some(&token),
            Some(&json!({"ack_ids": acks})),
        )
        .unwrap();
    }
    let lost = acked.iter().filter(|k| !seen.contains(*k)).count();
    println!(
        "RECOVERY queue acked={} delivered_after_restart={} lost_acked={lost} kill9_to_ready_ms={}",
        acked.len(),
        seen.len(),
        ready.as_millis()
    );
    assert_eq!(lost, 0, "acknowledged queue messages lost after kill -9");
    assert!(!acked.is_empty());
    drop(server);
    doctor_ok(&project);
}
