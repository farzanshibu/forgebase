//! Helpers shared by the integration tests.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::time::{Duration, Instant};

/// A free TCP port on loopback (bound then released).
pub fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// Blocking HTTP/1.1 request; returns (status, headers, body).
pub fn http(method: &str, addr: &str, path: &str) -> std::io::Result<(u16, String, String)> {
    let mut s = TcpStream::connect(addr)?;
    s.set_read_timeout(Some(Duration::from_secs(10)))?;
    s.set_write_timeout(Some(Duration::from_secs(10)))?;
    write!(
        s,
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"
    )?;
    let mut buf = Vec::new();
    s.read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    Ok((status, head.to_owned(), body.to_owned()))
}

/// GET returning status and parsed JSON body (Null if not JSON).
pub fn get_json(addr: &str, path: &str) -> (u16, serde_json::Value) {
    let (status, _, body) = http("GET", addr, path).unwrap();
    (status, serde_json::from_str(&body).unwrap_or(serde_json::Value::Null))
}

/// Start-up budget for a freshly started server to report healthy.
/// 120 s by default, since a first start on a fresh data dir under load can take minutes.
/// Override with env `FB_TEST_STARTUP_SECS` (whole seconds).
pub fn startup_timeout() -> Duration {
    std::env::var("FB_TEST_STARTUP_SECS")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map_or(Duration::from_secs(120), Duration::from_secs)
}

/// Polls `/health` until it answers 200 or the timeout passes.
pub fn wait_healthy(addr: &str, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if let Ok((200, _, _)) = http("GET", addr, "/health") {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// Size of a file, 0 if missing.
pub fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// True when the suite runs against PostgreSQL (`FORGEBASE_TEST_BACKEND=postgres`,
/// FB-149). The black-box suites drive the real binary, so the backend is
/// chosen the way a user chooses it: `FORGEBASE_DATABASE_BACKEND`.
pub fn postgres() -> bool {
    matches!(
        std::env::var("FORGEBASE_TEST_BACKEND").ok().as_deref().map(str::trim),
        Some("postgres" | "postgresql" | "pg")
    )
}

/// Applies the backend selection to a `forgebase` command whose `FORGEBASE_*`
/// variables were just cleared. SQLite (the default) needs nothing: a
/// `bundled-postgres` build would default to PostgreSQL, so SQLite is stated
/// explicitly. On PostgreSQL the engine comes from `FORGEBASE_PG_ENGINE_DIR`
/// (an extracted engine or a local install) or the embedded archive;
/// `FORGEBASE_TEST_PG_ACCELERATORS=off` turns every extension accelerator off.
pub fn backend_env(cmd: &mut std::process::Command) {
    if postgres() {
        cmd.env("FORGEBASE_DATABASE_BACKEND", "postgres");
        if let Some(dir) = std::env::var_os("FORGEBASE_PG_ENGINE_DIR").filter(|v| !v.is_empty()) {
            cmd.env("FORGEBASE_PG_ENGINE_DIR", dir);
        }
        // Nightly "portable" run: every accelerator off (FB-158 addendum).
        if std::env::var("FORGEBASE_TEST_PG_ACCELERATORS").as_deref() == Ok("off") {
            for f in ["VECTOR", "GRAPH", "QUEUE", "CRON", "TRIGRAM"] {
                cmd.env(format!("FORGEBASE_DATABASE_POSTGRES_EXTENSIONS_{f}"), "off");
            }
        }
    } else {
        cmd.env("FORGEBASE_DATABASE_BACKEND", "sqlite");
    }
}

/// The arguments of a `forgebase` call. On PostgreSQL, `init` of a new project
/// gets `--database postgres` (the way a user picks the engine: a build
/// without the embedded engine would otherwise create a SQLite project); a
/// project that already has a `forgebase.toml` is left alone.
pub fn backend_args(cwd: &Path, args: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
    if postgres() && args.first() == Some(&"init") && !args.contains(&"--database") && !args.contains(&"--database-url")
    {
        let dir = args.get(1).filter(|a| !a.starts_with('-')).copied().unwrap_or(".");
        if !cwd.join(dir).join("forgebase.toml").exists() {
            out.push("--database".into());
            out.push("postgres".into());
        }
    }
    out
}
