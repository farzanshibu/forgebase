//! `forgebase serve` exits cleanly on SIGTERM/SIGINT at any point after the
//! process starts, including right after `/health` answers (FIX-13): exit
//! code 0 and no bundled PostgreSQL left running.
//!
//! The PostgreSQL variant runs with `FORGEBASE_TEST_BACKEND=postgres` and an
//! engine in `FORGEBASE_PG_ENGINE_DIR`, and skips otherwise.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn postgres() -> bool {
    std::env::var("FORGEBASE_TEST_BACKEND").as_deref() == Ok("postgres")
}

fn serve(dir: &std::path::Path) -> Child {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_forgebase"));
    cmd.current_dir(dir);
    for (k, _) in std::env::vars_os() {
        let k = k.to_string_lossy().into_owned();
        if k.starts_with("FORGEBASE_") && k != "FORGEBASE_PG_ENGINE_DIR" {
            cmd.env_remove(k);
        }
    }
    cmd.env("NO_COLOR", "1").env("HOME", dir);
    if postgres() {
        cmd.env("FORGEBASE_DATABASE_BACKEND", "postgres");
    } else {
        cmd.env("FORGEBASE_DATABASE_BACKEND", "sqlite").env_remove("FORGEBASE_PG_ENGINE_DIR");
    }
    cmd.args(["serve", "--port", "0"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

/// The address from the "listening on http://ADDR" line.
fn listening(child: &mut Child) -> String {
    let mut lines = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    loop {
        line.clear();
        assert!(lines.read_line(&mut line).unwrap() > 0, "server exited before listening");
        if let Some(rest) = line.split("listening on http://").nth(1) {
            let addr = rest.split_whitespace().next().unwrap().to_owned();
            // Keep draining so the child never blocks on a full pipe.
            std::thread::spawn(move || {
                let mut sink = Vec::new();
                let _ = lines.read_to_end(&mut sink);
            });
            return addr;
        }
    }
}

fn healthy(addr: &str) -> bool {
    let Ok(mut s) = TcpStream::connect(addr) else { return false };
    s.set_read_timeout(Some(Duration::from_secs(2))).ok();
    let _ = s.write_all(format!("GET /health HTTP/1.0\r\nHost: {addr}\r\n\r\n").as_bytes());
    let mut buf = String::new();
    let _ = s.read_to_string(&mut buf);
    buf.starts_with("HTTP/1.0 200") || buf.starts_with("HTTP/1.1 200")
}

fn leftover(dir: &std::path::Path) -> bool {
    Command::new("pgrep")
        .arg("-f")
        .arg(dir.to_string_lossy().as_ref())
        .output()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false)
}

fn check(signal: &str, rounds: usize) {
    if std::env::var("FORGEBASE_TEST_BACKEND").as_deref() == Ok("postgres")
        && std::env::var_os("FORGEBASE_PG_ENGINE_DIR").is_none_or(|v| v.is_empty())
    {
        return;
    }
    for round in 0..rounds {
        let base = std::env::var_os("FORGEBASE_TEST_PG_TMP").unwrap_or_else(|| std::env::temp_dir().into_os_string());
        let tmp = tempfile::Builder::new().prefix("fx13-").tempdir_in(base).unwrap();
        let mut child = serve(tmp.path());
        let addr = listening(&mut child);
        let deadline = Instant::now() + Duration::from_secs(120);
        while !healthy(&addr) {
            assert!(Instant::now() < deadline, "no /health");
            std::thread::sleep(Duration::from_millis(20));
        }
        // Immediately, before anything else has run.
        let st = Command::new("kill")
            .args([format!("-{signal}"), child.id().to_string()])
            .status()
            .unwrap();
        assert!(st.success());
        let status = child.wait().unwrap();
        assert!(status.success(), "round {round}: SIG{signal} exit {status:?}");
        std::thread::sleep(Duration::from_millis(300));
        assert!(!leftover(tmp.path()), "round {round}: a process still uses {}", tmp.path().display());
    }
}

#[test]
fn sigterm_right_after_health_exits_zero_and_stops_the_database() {
    check("TERM", 4);
}

#[test]
fn sigint_right_after_health_exits_zero_and_stops_the_database() {
    check("INT", 2);
}
