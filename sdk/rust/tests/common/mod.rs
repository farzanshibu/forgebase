//! Boots the real `forgebase` binary for live tests.
//!
//! Set `FB_LIVE_BIN=/path/to/target/debug/forgebase`. Without it live tests print a notice and pass
//! vacuously, so `cargo test` stays hermetic; CI sets the variable.
#![allow(dead_code, missing_docs)]

use std::io::Write;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub mod mock;
pub mod sample;

use forgebase_sdk::{ForgeBase, SignInOptions};

pub const ADMIN_EMAIL: &str = "sdk-admin@example.com";
pub const ADMIN_PASSWORD: &str = "correct horse battery staple 42";

pub struct Server {
    pub base: String,
    pub dir: tempfile::TempDir,
    child: Child,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn live_bin() -> Option<PathBuf> {
    std::env::var_os("FB_LIVE_BIN")
        .map(PathBuf::from)
        .filter(|p| p.exists())
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .expect("a free port")
}

impl Server {
    /// Starts a fresh server with an admin account, or `None` when `FB_LIVE_BIN` is not set.
    pub fn start() -> Option<Self> {
        let Some(bin) = live_bin() else {
            eprintln!("FB_LIVE_BIN is not set: skipping live test");
            return None;
        };
        let dir = tempfile::tempdir().expect("temp dir");
        let port = free_port();
        let data = dir.path().join("data");
        let envs = [
            ("FORGEBASE_DATA_DIR", data.to_str().expect("utf8").to_owned()),
            ("FORGEBASE_HOST", "127.0.0.1".into()),
            ("FORGEBASE_PORT", port.to_string()),
        ];
        let run = |args: &[&str], input: Option<&str>| {
            let mut c = Command::new(&bin);
            c.args(args)
                .current_dir(dir.path())
                .envs(envs.iter().map(|(k, v)| (*k, v.as_str())))
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .stdin(Stdio::piped());
            let mut child = c.spawn().expect("spawn forgebase");
            if let Some(i) = input {
                child
                    .stdin
                    .take()
                    .expect("stdin")
                    .write_all(i.as_bytes())
                    .expect("write stdin");
            }
            let out = child.wait_with_output().expect("wait");
            assert!(
                out.status.success(),
                "forgebase {args:?} failed: {}{}",
                String::from_utf8_lossy(&out.stderr),
                String::from_utf8_lossy(&out.stdout)
            );
        };
        run(&["init", dir.path().to_str().expect("utf8")], None);
        run(
            &["admin", "create", "--email", ADMIN_EMAIL, "--password-stdin"],
            Some(&format!("{ADMIN_PASSWORD}\n")),
        );
        let child = Command::new(&bin)
            .arg("serve")
            .current_dir(dir.path())
            .envs(envs.iter().map(|(k, v)| (*k, v.as_str())))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn serve");
        let server = Self {
            base: format!("http://127.0.0.1:{port}"),
            dir,
            child,
        };
        let end = Instant::now() + Duration::from_secs(20);
        while Instant::now() < end {
            if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                std::thread::sleep(Duration::from_millis(100));
                return Some(server);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("server did not start on port {port}");
    }

    /// A client signed in as the administrator.
    pub async fn admin(&self) -> ForgeBase {
        let fb = ForgeBase::new(&self.base).expect("client");
        fb.auth()
            .sign_in_admin(ADMIN_EMAIL, ADMIN_PASSWORD, &SignInOptions::default())
            .await
            .expect("admin sign-in")
            .into_session()
            .expect("session");
        fb
    }

    /// Mails the dev mailer wrote so far (the SDK cannot read mail: tests do, like a user's inbox).
    pub fn mails(&self) -> Vec<String> {
        let dir = self.dir.path().join("data").join("mail");
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
            .map(|d| d.filter_map(|e| e.ok().map(|e| e.path())).collect())
            .unwrap_or_default();
        files.sort();
        files.iter().filter_map(|p| std::fs::read_to_string(p).ok()).collect()
    }

    /// Waits for the n-th (0-based) mail.
    pub async fn mail(&self, n: usize) -> String {
        until(|| self.mails().get(n).cloned(), 8000).await
    }
}

/// Polls `f` until it returns `Some`, or panics after `ms`.
pub async fn until<T>(mut f: impl FnMut() -> Option<T>, ms: u64) -> T {
    let end = Instant::now() + Duration::from_millis(ms);
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < end, "timed out waiting for condition");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

pub fn path_exists(p: &Path) -> bool {
    p.exists()
}

/// RFC 6238 TOTP (SHA-1, 6 digits, 30 s) for a base32 secret.
pub fn totp(secret_b32: &str, unix_secs: u64) -> String {
    use hmac::{Hmac, KeyInit, Mac};
    let mut bits = 0u32;
    let mut nbits = 0;
    let mut key = Vec::new();
    for c in secret_b32.trim_end_matches('=').bytes() {
        let v = match c.to_ascii_uppercase() {
            b'A'..=b'Z' => u32::from(c.to_ascii_uppercase() - b'A'),
            b'2'..=b'7' => u32::from(c - b'2') + 26,
            _ => continue,
        };
        bits = (bits << 5) | v;
        nbits += 5;
        if nbits >= 8 {
            nbits -= 8;
            key.push(u8::try_from((bits >> nbits) & 0xff).expect("byte"));
        }
    }
    let mut mac = Hmac::<sha1::Sha1>::new_from_slice(&key).expect("hmac key");
    mac.update(&(unix_secs / 30).to_be_bytes());
    let h = mac.finalize().into_bytes();
    let off = usize::from(h[h.len() - 1] & 0x0f);
    let n = u32::from_be_bytes([h[off] & 0x7f, h[off + 1], h[off + 2], h[off + 3]]) % 1_000_000;
    format!("{n:06}")
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs()
}
