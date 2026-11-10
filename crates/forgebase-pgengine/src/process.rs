//! The `postgres` child: spawn, logs into `tracing`, readiness, stop, stale
//! `postmaster.pid` handling and the parent-death watcher (ADR-0028 §4.3).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};

use crate::discover::EngineDir;
use crate::error::{EngineError, Result};
use crate::fsutil;

/// How many engine log lines are kept for error messages.
const LOG_TAIL: usize = 40;

/// The last engine log lines, shared with the stderr reader.
#[derive(Debug, Clone, Default)]
pub(crate) struct LogTail(Arc<Mutex<VecDeque<String>>>);

impl LogTail {
    fn push(&self, line: String) {
        if let Ok(mut q) = self.0.lock() {
            if q.len() == LOG_TAIL {
                q.pop_front();
            }
            q.push_back(line);
        }
    }

    /// The kept lines, oldest first.
    pub(crate) fn lines(&self) -> Vec<String> {
        self.0.lock().map(|q| q.iter().cloned().collect()).unwrap_or_default()
    }
}

/// Where the cluster listens.
#[derive(Debug, Clone)]
pub(crate) struct Endpoint {
    /// Unix socket directory (`None` on Windows).
    pub socket_dir: Option<PathBuf>,
    /// TCP address, when listening.
    pub tcp: Option<std::net::SocketAddr>,
    /// Port (socket name, or TCP).
    pub port: u16,
}

/// A running postmaster.
#[derive(Debug)]
pub(crate) struct Postmaster {
    pub child: Child,
    pub pid: u32,
    /// Held open; the watcher stops the engine when this process dies and
    /// the pipe closes.
    watcher: Option<(Child, ChildStdin)>,
    /// Set when the stderr reader saw end of file (all log lines are in).
    log_eof: Arc<std::sync::atomic::AtomicBool>,
}

/// Socket path limit of the platform (bytes, without the NUL).
fn socket_path_limit() -> usize {
    if cfg!(target_os = "linux") { 107 } else { 103 }
}

/// The Unix socket directory: `preferred` when `<dir>/.s.PGSQL.<port>` fits
/// the platform limit, otherwise `$TMPDIR/forgebase-<12 hex of key>/` (ADR-0028
/// §4.3), checked to be private and ours.
pub(crate) fn socket_dir(preferred: &Path, key: &Path, port: u16) -> Result<PathBuf> {
    let socket_name = format!(".s.PGSQL.{port}");
    let fits = |d: &Path| d.join(&socket_name).as_os_str().len() <= socket_path_limit();
    if fits(preferred) {
        fsutil::private_dir(preferred)?;
        return Ok(preferred.to_path_buf());
    }
    let hash = fsutil::sha256_hex(key.as_os_str().to_string_lossy().as_bytes());
    let name = format!("forgebase-{}", &hash[..12]);
    let mut candidate = std::env::temp_dir().join(&name);
    if !fits(&candidate) {
        candidate = PathBuf::from("/tmp").join(&name);
    }
    if !fits(&candidate) {
        return Err(EngineError::Config(format!(
            "no socket directory short enough for {socket_name} (tried {} and {})",
            preferred.display(),
            candidate.display()
        )));
    }
    fsutil::private_dir(&candidate)?;
    check_owned_private_dir(&candidate)?;
    tracing::info!(
        target: "forgebase::postgres",
        socket_dir = %candidate.display(),
        "the data directory path is too long for a Unix socket; using a private directory under the temp dir"
    );
    Ok(candidate)
}

#[cfg(unix)]
fn check_owned_private_dir(dir: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(dir).map_err(|e| EngineError::io("stat", dir, e))?;
    let me = rustix::process::geteuid().as_raw();
    if !meta.is_dir() || meta.uid() != me || meta.mode() & 0o077 != 0 {
        return Err(EngineError::Config(format!(
            "{} must be a directory owned by this user with mode 0700",
            dir.display()
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_owned_private_dir(_dir: &Path) -> Result<()> {
    Ok(())
}

/// Starts `postgres -D <pgdata>`. Network settings are also passed on the
/// command line, so no file can widen what the engine listens on.
pub(crate) fn spawn(engine: &EngineDir, pgdata: &Path, ep: &Endpoint, node: &str, log: &LogTail) -> Result<Postmaster> {
    let mut cmd = Command::new(engine.bin("postgres"));
    cmd.arg("-D").arg(pgdata);
    let listen = ep.tcp.map(|a| a.ip().to_string()).unwrap_or_else(|| {
        if ep.socket_dir.is_none() { "127.0.0.1".into() } else { String::new() }
    });
    cmd.arg("-c").arg(format!("listen_addresses={listen}"));
    cmd.arg("-c").arg(format!("port={}", ep.port));
    match &ep.socket_dir {
        Some(d) => cmd.arg("-c").arg(format!("unix_socket_directories={}", d.display())),
        None => cmd.arg("-c").arg("unix_socket_directories="),
    };
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    cmd.env_remove("PGDATA").env("LC_MESSAGES", "C");
    #[cfg(unix)]
    {
        // Its own process group: a terminal Ctrl-C reaches ForgeBase only,
        // which then stops the engine in order.
        cmd.process_group(0);
    }
    cmd.kill_on_drop(false);
    let mut child = cmd
        .spawn()
        .map_err(|e| EngineError::io("start", &engine.bin("postgres"), e))?;
    let pid = child.id().ok_or_else(|| EngineError::Start {
        reason: "postgres exited immediately".into(),
        log: Vec::new(),
    })?;
    let log_eof = Arc::new(std::sync::atomic::AtomicBool::new(false));
    if let Some(stderr) = child.stderr.take() {
        let tail = log.clone();
        let node = node.to_owned();
        let eof = log_eof.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            let mut last = tracing::Level::INFO;
            while let Ok(Some(line)) = lines.next_line().await {
                let (level, msg) = classify(&line, last);
                last = level;
                emit(level, &node, msg);
                tail.push(line);
            }
            eof.store(true, std::sync::atomic::Ordering::SeqCst);
        });
    } else {
        log_eof.store(true, std::sync::atomic::Ordering::SeqCst);
    }
    let watcher = spawn_watcher(pid);
    Ok(Postmaster {
        child,
        pid,
        watcher,
        log_eof,
    })
}

/// The parent-death watcher (Unix): a `/bin/sh` that blocks reading a pipe
/// whose only writer is this process. When ForgeBase exits for any reason,
/// including `SIGKILL`, the pipe closes and the watcher gives the engine a
/// fast shutdown (then an immediate one after 30 s). This replaces
/// `PR_SET_PDEATHSIG`, which needs `pre_exec` (unsafe) and is Linux-only.
#[cfg(unix)]
fn spawn_watcher(pid: u32) -> Option<(Child, ChildStdin)> {
    const SCRIPT: &str = r#"read _ignored
kill -INT "$1" 2>/dev/null || exit 0
n=0
while kill -0 "$1" 2>/dev/null; do
  n=$((n+1))
  if [ "$n" -gt 30 ]; then kill -QUIT "$1" 2>/dev/null; exit 0; fi
  sleep 1
done"#;
    let sh = Path::new("/bin/sh");
    if !sh.exists() {
        tracing::debug!(target: "forgebase::postgres", "no /bin/sh: the engine is not stopped if ForgeBase is killed; the next start cleans up");
        return None;
    }
    let mut cmd = Command::new(sh);
    cmd.arg("-c")
        .arg(SCRIPT)
        .arg("forgebase-pg-watch")
        .arg(pid.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .kill_on_drop(false);
    match cmd.spawn() {
        Ok(mut child) => {
            let stdin = child.stdin.take()?;
            Some((child, stdin))
        }
        Err(e) => {
            tracing::warn!(target: "forgebase::postgres", error = %e, "could not start the engine watcher");
            None
        }
    }
}

#[cfg(not(unix))]
fn spawn_watcher(_pid: u32) -> Option<(Child, ChildStdin)> {
    None
}

impl Postmaster {
    /// Ends the watcher without letting it signal the engine.
    pub(crate) async fn disarm_watcher(&mut self) {
        if let Some((mut child, stdin)) = self.watcher.take() {
            let _ = child.start_kill();
            let _ = child.wait().await;
            drop(stdin);
        }
    }

    /// Waits (up to a second) until the stderr reader has every line.
    pub(crate) async fn wait_log_eof(&self) {
        let until = Instant::now() + Duration::from_secs(1);
        while !self.log_eof.load(std::sync::atomic::Ordering::SeqCst) && Instant::now() < until {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Whether the postmaster has exited (without waiting).
    pub(crate) fn exited(&mut self) -> Option<std::process::ExitStatus> {
        self.child.try_wait().ok().flatten()
    }

    /// Stops the engine: fast shutdown, then immediate after `timeout`,
    /// then a kill (ADR-0028 §4.3). `immediate` skips the fast step.
    pub(crate) async fn stop(&mut self, pgdata: &Path, engine: &EngineDir, timeout: Duration, immediate: bool) {
        self.disarm_watcher().await;
        if self.exited().is_some() {
            return;
        }
        #[cfg(unix)]
        {
            use rustix::process::Signal;
            let steps: &[(Signal, Duration)] = if immediate {
                &[(Signal::QUIT, Duration::from_secs(5))]
            } else {
                &[(Signal::INT, timeout), (Signal::QUIT, Duration::from_secs(5))]
            };
            for (sig, wait) in steps {
                signal(self.pid, *sig);
                if tokio::time::timeout(*wait, self.child.wait()).await.is_ok() {
                    return;
                }
                tracing::warn!(target: "forgebase::postgres", pid = self.pid, signal = ?sig, "PostgreSQL did not stop in time");
            }
        }
        #[cfg(not(unix))]
        {
            let mode = if immediate { "immediate" } else { "fast" };
            let _ = Command::new(engine.bin("pg_ctl"))
                .args(["stop", "-m", mode, "-w", "-t"])
                .arg(timeout.as_secs().max(1).to_string())
                .arg("-D")
                .arg(pgdata)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await;
            if tokio::time::timeout(Duration::from_secs(5), self.child.wait()).await.is_ok() {
                return;
            }
        }
        let _ = (pgdata, engine);
        let _ = self.child.start_kill();
        let _ = self.child.wait().await;
    }
}

#[cfg(unix)]
fn signal(pid: u32, sig: rustix::process::Signal) {
    if let Some(p) = i32::try_from(pid).ok().and_then(rustix::process::Pid::from_raw) {
        let _ = rustix::process::kill_process(p, sig);
    }
}

/// Sends a fast-shutdown signal without waiting (used from `Drop`).
pub(crate) fn request_fast_shutdown(pid: u32) {
    #[cfg(unix)]
    signal(pid, rustix::process::Signal::INT);
    #[cfg(not(unix))]
    let _ = pid;
}

/// Sends `SIGKILL`-like termination (tests simulate a crash with it).
#[cfg(unix)]
pub(crate) fn kill_hard(pid: u32) {
    signal(pid, rustix::process::Signal::KILL);
}

/// Fields of `postmaster.pid` ForgeBase reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PidFile {
    pub pid: u32,
    pub port: Option<u16>,
    pub socket_dir: Option<PathBuf>,
    pub status: Option<String>,
}

impl PidFile {
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let lines: Vec<&str> = text.lines().collect();
        let pid = lines.first()?.trim().parse().ok()?;
        let field = |n: usize| lines.get(n).map(|s| s.trim()).filter(|s| !s.is_empty());
        Some(Self {
            pid,
            port: field(3).and_then(|p| p.parse().ok()),
            socket_dir: field(4).map(PathBuf::from),
            status: field(7).map(str::to_owned),
        })
    }

    pub(crate) fn read(pgdata: &Path) -> Option<Self> {
        std::fs::read_to_string(pgdata.join("postmaster.pid"))
            .ok()
            .and_then(|t| Self::parse(&t))
    }

    /// Whether a server accepts connections on this file's socket.
    fn socket_answers(&self) -> bool {
        #[cfg(unix)]
        {
            match (&self.socket_dir, self.port) {
                (Some(d), Some(p)) => std::os::unix::net::UnixStream::connect(d.join(format!(".s.PGSQL.{p}"))).is_ok(),
                _ => false,
            }
        }
        #[cfg(not(unix))]
        {
            false
        }
    }
}

/// Liveness of a pid: `Some(true)` alive and ours to signal, `Some(false)`
/// gone, `None` alive but another user's.
#[cfg(unix)]
fn alive(pid: u32) -> Option<bool> {
    let Some(p) = i32::try_from(pid).ok().and_then(rustix::process::Pid::from_raw) else {
        return Some(false);
    };
    match rustix::process::test_kill_process(p) {
        Ok(()) => Some(true),
        Err(rustix::io::Errno::SRCH) => Some(false),
        Err(_) => None,
    }
}

/// Handles a `postmaster.pid` left by a killed server (ADR-0028 §4.3). The
/// caller holds `.forgebase.lock`, so no other ForgeBase owns the cluster:
/// a live postmaster answering on the file's socket is a leftover and gets a
/// fast shutdown. A dead pid is left to PostgreSQL, which removes stale
/// files itself. A live pid that never answers is refused, never deleted
/// (deleting the file under a live postmaster can corrupt the cluster).
pub(crate) async fn clear_stale(pgdata: &Path, timeout: Duration) -> Result<()> {
    #[cfg(unix)]
    {
        let Some(pf) = PidFile::read(pgdata) else {
            return Ok(());
        };
        if pf.pid == std::process::id() {
            return Ok(());
        }
        let deadline = Instant::now() + timeout;
        loop {
            match alive(pf.pid) {
                Some(false) => return Ok(()),
                None => {
                    return Err(EngineError::Cluster {
                        dir: pgdata.to_path_buf(),
                        reason: format!(
                            "postmaster.pid names process {}, which belongs to another user",
                            pf.pid
                        ),
                    });
                }
                Some(true) if pf.socket_answers() => break,
                Some(true) if Instant::now() >= deadline => {
                    return Err(EngineError::Cluster {
                        dir: pgdata.to_path_buf(),
                        reason: format!(
                            "postmaster.pid names process {}, which is running but does not answer on its \
                             socket; stop it, or delete postmaster.pid if it is not PostgreSQL",
                            pf.pid
                        ),
                    });
                }
                Some(true) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
        tracing::warn!(
            target: "forgebase::postgres",
            pid = pf.pid,
            "stopping a PostgreSQL left running by an earlier ForgeBase"
        );
        use rustix::process::Signal;
        for (sig, wait) in [(Signal::INT, timeout), (Signal::QUIT, Duration::from_secs(5))] {
            signal(pf.pid, sig);
            let until = Instant::now() + wait;
            while Instant::now() < until {
                if alive(pf.pid) != Some(true) {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        Err(EngineError::Cluster {
            dir: pgdata.to_path_buf(),
            reason: format!("the leftover postmaster {} did not stop", pf.pid),
        })
    }
    #[cfg(not(unix))]
    {
        let _ = (pgdata, timeout);
        Ok(())
    }
}

/// Waits until the postmaster we started reports `ready` in
/// `postmaster.pid`, failing early when it exits.
pub(crate) async fn wait_pid_ready(pm: &mut Postmaster, pgdata: &Path, deadline: Instant, log: &LogTail) -> Result<()> {
    loop {
        if let Some(status) = pm.exited() {
            pm.wait_log_eof().await;
            return Err(EngineError::Start {
                reason: format!("postgres exited during start ({status})"),
                log: log.lines(),
            });
        }
        if let Some(pf) = PidFile::read(pgdata) {
            if pf.pid == pm.pid && pf.status.as_deref() == Some("ready") {
                return Ok(());
            }
        }
        if Instant::now() >= deadline {
            return Err(EngineError::Start {
                reason: "timed out waiting for the engine to accept connections".into(),
                log: log.lines(),
            });
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Splits `[pid] LEVEL:  message` into a tracing level and the text.
/// Continuation lines (`DETAIL`, `HINT`, `CONTEXT`, `STATEMENT`) keep the
/// level of the line before.
pub(crate) fn classify(line: &str, last: tracing::Level) -> (tracing::Level, &str) {
    use tracing::Level;
    let body = match line.strip_prefix('[').and_then(|r| r.split_once("] ")) {
        Some((pid, rest)) if pid.bytes().all(|b| b.is_ascii_digit()) => rest,
        _ => line,
    };
    let Some((tag, _)) = body.split_once(':') else {
        return (last, body);
    };
    let level = match tag {
        "PANIC" | "FATAL" | "ERROR" => Level::ERROR,
        "WARNING" => Level::WARN,
        "LOG" | "INFO" | "NOTICE" => Level::INFO,
        t if t.starts_with("DEBUG") => Level::DEBUG,
        "DETAIL" | "HINT" | "CONTEXT" | "STATEMENT" | "QUERY" | "LOCATION" => last,
        _ => last,
    };
    (level, body)
}

fn emit(level: tracing::Level, node: &str, msg: &str) {
    use tracing::Level;
    match level {
        Level::ERROR => tracing::error!(target: "forgebase::postgres", node, "{msg}"),
        Level::WARN => tracing::warn!(target: "forgebase::postgres", node, "{msg}"),
        Level::INFO => tracing::info!(target: "forgebase::postgres", node, "{msg}"),
        Level::DEBUG => tracing::debug!(target: "forgebase::postgres", node, "{msg}"),
        Level::TRACE => tracing::trace!(target: "forgebase::postgres", node, "{msg}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing::Level;

    #[test]
    fn log_lines_are_classified() {
        assert_eq!(
            classify("[123] LOG:  database system is ready", Level::INFO),
            (Level::INFO, "LOG:  database system is ready")
        );
        assert_eq!(classify("[9] FATAL:  boom", Level::INFO).0, Level::ERROR);
        assert_eq!(classify("[9] WARNING:  w", Level::INFO).0, Level::WARN);
        assert_eq!(classify("[9] DEBUG2:  d", Level::INFO).0, Level::DEBUG);
        assert_eq!(classify("[9] DETAIL:  more", Level::ERROR).0, Level::ERROR);
        assert_eq!(classify("plain text", Level::WARN), (Level::WARN, "plain text"));
    }

    #[test]
    fn pid_file_parsing() {
        let text = "4242\n/d/postgres/data\n1731300000\n5432\n/d/postgres/run\n\n  5432001   65536\nready   \n";
        let pf = PidFile::parse(text).expect("parse");
        assert_eq!(pf.pid, 4242);
        assert_eq!(pf.port, Some(5432));
        assert_eq!(pf.socket_dir.as_deref(), Some(Path::new("/d/postgres/run")));
        assert_eq!(pf.status.as_deref(), Some("ready"));
        let starting = PidFile::parse("7\n/d\n1\n5432\n/r\n").expect("parse");
        assert_eq!(starting.status, None);
        assert!(PidFile::parse("").is_none());
        assert!(PidFile::parse("abc\n").is_none());
    }

    #[test]
    fn log_tail_keeps_the_newest_lines() {
        let t = LogTail::default();
        for i in 0..100 {
            t.push(format!("line {i}"));
        }
        let lines = t.lines();
        assert_eq!(lines.len(), LOG_TAIL);
        assert_eq!(lines.last().map(String::as_str), Some("line 99"));
    }

    #[test]
    fn long_socket_paths_fall_back_to_a_private_temp_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let short = dir.path().join("run");
        assert_eq!(socket_dir(&short, dir.path(), 5432).expect("short"), short);
        let long = dir.path().join("x".repeat(120)).join("run");
        let got = socket_dir(&long, &long, 5432).expect("fallback");
        assert_ne!(got, long);
        assert!(got.file_name().is_some_and(|n| n.to_string_lossy().starts_with("forgebase-")));
        assert!(got.join(".s.PGSQL.5432").as_os_str().len() <= socket_path_limit());
        // Same key, same directory.
        assert_eq!(socket_dir(&long, &long, 5432).expect("again"), got);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&got).expect("meta").permissions().mode() & 0o777;
            assert_eq!(mode, 0o700);
        }
        let _ = std::fs::remove_dir(&got);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stale_pid_of_a_dead_process_is_left_to_postgres() {
        let dir = tempfile::tempdir().expect("tempdir");
        // A pid that cannot exist on any system.
        std::fs::write(dir.path().join("postmaster.pid"), "2147483646\n/d\n").expect("write");
        clear_stale(dir.path(), Duration::from_millis(200)).await.expect("dead pid");
        assert!(dir.path().join("postmaster.pid").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn live_pid_without_a_socket_is_refused_not_deleted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut sleeper = Command::new("/bin/sleep").arg("30").spawn().expect("sleep");
        let pid = sleeper.id().expect("pid");
        std::fs::write(
            dir.path().join("postmaster.pid"),
            format!("{pid}\n/d\n1\n5432\n{}\n", dir.path().display()),
        )
        .expect("write");
        let err = clear_stale(dir.path(), Duration::from_millis(300)).await.expect_err("refused");
        assert!(err.to_string().contains("does not answer"), "{err}");
        assert!(dir.path().join("postmaster.pid").exists());
        let _ = sleeper.start_kill();
        let _ = sleeper.wait().await;
    }
}
