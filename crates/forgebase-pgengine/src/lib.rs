//! ForgeBase's bundled PostgreSQL engine (ADR-0028 §3–§4, ADR-0029 §1.3).
//!
//! [`Engine::prepare`] takes a cluster [`Layout`] and an [`EngineConfig`] and
//! returns a running, ready [`EngineHandle`]:
//!
//! 1. refuse to run as root;
//! 2. find the engine: the archive embedded with the `bundled-postgres`
//!    feature, an archive file, or an installed directory
//!    (`FORGEBASE_PG_ENGINE_DIR` in development); archives are extracted
//!    into `postgres/engine/<version>-<build>-<sha12>/`, verified against
//!    their manifest, relocated (Linux `PT_INTERP`) and made read-only;
//! 3. `initdb` on first start (builtin C.UTF-8 locale, SCRAM, checksums) with
//!    passwords from `postgres/roles.secret` (0600);
//! 4. stop a postmaster left running by a killed server;
//! 5. write `pg_hba.conf` ([`hba`]: no TCP line unless `listen` is set,
//!    TLS-only TCP logins when it is) and `forgebase-postgres.conf` (Unix
//!    socket only by default, the
//!    ADR-0029 replication, prepared-transaction and pg_cron settings; pg_cron
//!    is skipped with a warning when the engine lacks it);
//! 6. spawn `postgres` with its stderr in `tracing` (target
//!    `forgebase::postgres`), wait for readiness, and bootstrap roles, the
//!    `forgebase` database, its schemas and `search_path`, and the roles'
//!    per-role Apache AGE preload ([`AGE_PRELOAD`], ADR-0029 §1.2);
//! 7. supervise: restart with backoff when the postmaster dies, stop with a
//!    fast shutdown.
//!
//! The caller holds `<data_dir>/.forgebase.lock` (the server does), so no
//! other ForgeBase process owns the cluster.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, oneshot, watch};

pub mod archive;
mod cluster;
mod conf;
mod config;
mod discover;
mod error;
mod extensions;
mod fsutil;
pub mod hba;
pub mod interp;
mod layout;
#[cfg(test)]
mod lock;
mod process;
pub mod replication;
mod secret;
pub mod shard;
pub mod standby;

pub use cluster::{DATABASE, LOGIN_ROLE, LOGIN_WRITE_ROLE, OWNER_ROLE, RLS_ROLE, SEARCH_PATH, SQL_ADMIN_ROLE};
pub use conf::{DENY as DENIED_SETTINGS, DENY_PREFIXES as DENIED_SETTING_PREFIXES};
pub use config::{ENGINE_DIR_ENV, EngineConfig, EngineSource, ExtensionMode, RestartPolicy, TlsFiles};
pub use discover::{EngineDir, EngineOrigin, HAS_EMBEDDED, MIN_MAJOR, PgVersion, embedded_archive};
pub use error::{EngineError, Result};
pub use extensions::AGE_PRELOAD;
pub use layout::{CONF_FILE, Layout, NodeKind};
pub use secret::{Role, RoleSecrets, Secret};

use process::{Endpoint, LogTail, Postmaster};

/// Lifecycle state of a supervised engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineState {
    /// Accepting connections.
    Ready,
    /// The postmaster exited; waiting to restart it (requests get 503).
    Restarting,
    /// Too many failures in the window; ForgeBase stopped trying
    /// (`/health/ready` reports `database: failed`).
    Failed,
    /// Stopped on request.
    Stopped,
}

impl EngineState {
    /// Lowercase name for health output.
    pub fn as_str(self) -> &'static str {
        match self {
            EngineState::Ready => "ready",
            EngineState::Restarting => "restarting",
            EngineState::Failed => "failed",
            EngineState::Stopped => "stopped",
        }
    }
}

/// What [`EngineHandle::health`] reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineHealth {
    /// Current state.
    pub state: EngineState,
    /// Postmaster pid while running.
    pub pid: Option<u32>,
    /// Restarts since [`Engine::prepare`].
    pub restarts: u32,
    /// The last failure, when there was one.
    pub last_error: Option<String>,
}

/// Entry point.
#[derive(Debug, Clone, Copy)]
pub struct Engine;

/// Everything a (re)start needs.
#[derive(Debug)]
struct Ctx {
    layout: Layout,
    engine: EngineDir,
    endpoint: Endpoint,
    secrets: RoleSecrets,
    ready_timeout: Duration,
    stop_timeout: Duration,
    restart: RestartPolicy,
    preload: Vec<&'static str>,
    /// The server and reader roles preload Apache AGE (`session_preload_libraries`).
    age: bool,
    warnings: Vec<String>,
    stopping: AtomicBool,
    pid: AtomicU32,
    log: LogTail,
}

enum Cmd {
    Stop {
        immediate: bool,
        reply: oneshot::Sender<()>,
    },
    #[cfg(unix)]
    KillForTest,
}

impl Engine {
    /// Prepares and starts the cluster of `layout` (see the crate docs).
    ///
    /// # Errors
    /// Every step reports its own [`EngineError`]; nothing keeps running
    /// when this fails.
    pub async fn prepare(layout: &Layout, cfg: &EngineConfig) -> Result<EngineHandle> {
        refuse_root()?;
        conf::check_user_settings(&cfg.settings)?;
        if let Some(a) = cfg.listen {
            config::check_listen(a)?;
        }
        let engine = discover::resolve(cfg, layout).await?;
        fsutil::private_dir(layout.postgres_dir())?;
        fsutil::private_dir(&layout.node_dir())?;

        let (preload, cron, mut warnings) = preloads(&engine, cfg)?;
        let age = extensions::age_preload(&engine, cfg, &mut warnings)?;
        let pgdata = layout.pgdata();
        cluster::clean_partial(&pgdata);
        let has_cluster = cluster::exists(&pgdata);
        let secrets = load_secrets(layout, has_cluster)?;
        if has_cluster {
            let major = cluster::data_major(&pgdata)?;
            if major != engine.version.major {
                return Err(EngineError::Cluster {
                    dir: pgdata,
                    reason: format!(
                        "it was created by PostgreSQL {major} and this engine is {}; ForgeBase never \
                         upgrades a major on its own (use forgebase db upgrade-engine, or point \
                         engine_dir at a PostgreSQL {major} engine)",
                        engine.version.text
                    ),
                });
            }
        } else {
            match layout.kind() {
                NodeKind::Primary => cluster::initdb(&engine, &pgdata, &secrets, cfg.unsafe_fast).await?,
                NodeKind::Standby => {
                    return Err(EngineError::Cluster {
                        dir: pgdata,
                        reason: "a standby's data directory must be provisioned (pg_basebackup -R) before it starts"
                            .into(),
                    });
                }
            }
        }

        let endpoint = endpoint(layout, cfg)?;
        process::clear_stale(&pgdata, cfg.stop_timeout).await?;
        // FB-153: TCP only when the operator set `listen`, and then always
        // with TLS; pg_hba.conf is rewritten every start.
        let tls = match cfg.listen {
            Some(addr) => Some(hba::ensure_certificate(
                cfg.tls.as_ref(),
                &layout.node_dir().join("tls"),
                addr,
            )?),
            None => None,
        };
        hba::write(
            &pgdata,
            &hba::HbaInput {
                socket: endpoint.socket_dir.is_some(),
                tcp: endpoint.tcp,
                tls: cfg.listen.is_some(),
            },
        )?;
        let settings = conf::settings(&conf::ConfInput {
            socket_dir: endpoint.socket_dir.as_deref(),
            port: endpoint.port,
            listen: endpoint.tcp,
            tls: tls.as_ref().map(|(c, k)| (c.as_path(), k.as_path())),
            preload: preload.clone(),
            cron,
            standby: layout.kind() == NodeKind::Standby,
            major: engine.version.major,
            slots: &cfg.replication,
            slow_query: cfg.slow_query,
            unsafe_fast: cfg.unsafe_fast,
            user: &cfg.settings,
        });
        let conf_path = layout.conf_file();
        std::fs::write(&conf_path, conf::render(&settings)).map_err(|e| EngineError::io("write", &conf_path, e))?;
        conf::ensure_include(&pgdata)?;

        let ctx = Arc::new(Ctx {
            layout: layout.clone(),
            engine,
            endpoint,
            secrets,
            ready_timeout: cfg.ready_timeout,
            stop_timeout: cfg.stop_timeout,
            restart: cfg.restart,
            preload,
            age,
            warnings,
            stopping: AtomicBool::new(false),
            pid: AtomicU32::new(0),
            log: LogTail::default(),
        });
        let mut pm = start_once(&ctx).await?;
        if layout.kind() == NodeKind::Primary
            && let Err(e) = async {
                cluster::bootstrap(&ctx.endpoint, &ctx.secrets).await?;
                extensions::apply_role_settings(&ctx.endpoint, &ctx.secrets, ctx.age).await
            }
            .await
        {
            pm.stop(&pgdata, &ctx.engine, ctx.stop_timeout, false).await;
            return Err(e);
        }
        tracing::info!(
            target: "forgebase::postgres",
            node = layout.name(),
            version = %ctx.engine.version.text,
            pid = pm.pid,
            "PostgreSQL is ready"
        );
        if cfg.remove_old_engines && layout.is_primary_layout() {
            remove_old_engines(layout, &ctx.engine);
        }

        let (health_tx, health_rx) = watch::channel(EngineHealth {
            state: EngineState::Ready,
            pid: Some(pm.pid),
            restarts: 0,
            last_error: None,
        });
        let (tx, rx) = mpsc::channel(4);
        let task = tokio::spawn(supervise(ctx.clone(), pm, rx, health_tx));
        Ok(EngineHandle {
            ctx,
            cmd: tx,
            health: health_rx,
            task: Mutex::new(Some(task)),
        })
    }
}

#[cfg(unix)]
fn refuse_root() -> Result<()> {
    if rustix::process::geteuid().is_root() {
        return Err(EngineError::Root);
    }
    Ok(())
}

#[cfg(not(unix))]
fn refuse_root() -> Result<()> {
    Ok(())
}

fn load_secrets(layout: &Layout, has_cluster: bool) -> Result<RoleSecrets> {
    let path = layout.secrets_file();
    match RoleSecrets::load(&path)? {
        Some(mut s) => {
            if s.fill_missing()? {
                s.save(&path)?;
            }
            Ok(s)
        }
        None if has_cluster => Err(EngineError::Cluster {
            dir: layout.pgdata(),
            reason: format!(
                "{} is missing, so ForgeBase cannot log in to this cluster; restore it from a backup",
                path.display()
            ),
        }),
        None => {
            let s = RoleSecrets::generate()?;
            s.save(&path)?;
            Ok(s)
        }
    }
}

/// `shared_preload_libraries`: pg_stat_statements and pg_cron when the
/// engine ships them (pg_cron per `cfg.cron`).
fn preloads(engine: &EngineDir, cfg: &EngineConfig) -> Result<(Vec<&'static str>, bool, Vec<String>)> {
    let mut preload = Vec::new();
    let mut warnings = Vec::new();
    if engine.has_module("pg_stat_statements") {
        preload.push("pg_stat_statements");
    } else {
        warnings.push("pg_stat_statements is not in this engine; query statistics are unavailable".to_owned());
    }
    let has_cron = engine.has_module("pg_cron");
    let cron = match (cfg.cron, has_cron) {
        (ExtensionMode::Off, _) => false,
        (_, true) => true,
        (ExtensionMode::On, false) => {
            return Err(EngineError::Config(format!(
                "extensions.cron = \"on\", but the engine in {} has no pg_cron module",
                engine.root.display()
            )));
        }
        (ExtensionMode::Auto, false) => {
            warnings.push(
                "pg_cron is not in this engine; it is not preloaded and schedules stay on the ForgeBase scheduler"
                    .to_owned(),
            );
            false
        }
    };
    if cron {
        preload.push("pg_cron");
    }
    for w in &warnings {
        tracing::warn!(target: "forgebase::postgres", engine = %engine.root.display(), "{w}");
    }
    Ok((preload, cron, warnings))
}

fn endpoint(layout: &Layout, cfg: &EngineConfig) -> Result<Endpoint> {
    #[cfg(unix)]
    {
        let port = cfg.listen.map_or(cfg.socket_port, |a| a.port());
        let dir = process::socket_dir(&layout.run_dir(), &layout.pgdata(), port)?;
        Ok(Endpoint {
            socket_dir: Some(dir),
            tcp: cfg.listen,
            port,
        })
    }
    #[cfg(not(unix))]
    {
        let _ = layout;
        let addr = match cfg.listen {
            Some(a) => a,
            None => {
                let l = std::net::TcpListener::bind("127.0.0.1:0")
                    .map_err(|e| EngineError::Config(format!("no free loopback port: {e}")))?;
                l.local_addr()
                    .map_err(|e| EngineError::Config(format!("no free loopback port: {e}")))?
            }
        };
        Ok(Endpoint {
            socket_dir: None,
            tcp: Some(addr),
            port: addr.port(),
        })
    }
}

/// The role readiness logs in as: a standby's host may hold only the
/// replicator's password (FB-168 remote replicas).
fn probe_role(kind: NodeKind) -> Role {
    match kind {
        NodeKind::Primary => Role::Super,
        NodeKind::Standby => Role::Replicator,
    }
}

/// Spawns the postmaster and waits until it accepts our login.
async fn start_once(ctx: &Ctx) -> Result<Postmaster> {
    let pgdata = ctx.layout.pgdata();
    let mut pm = process::spawn(&ctx.engine, &pgdata, &ctx.endpoint, ctx.layout.name(), &ctx.log)?;
    ctx.pid.store(pm.pid, Ordering::SeqCst);
    let deadline = Instant::now() + ctx.ready_timeout;
    if let Err(e) = process::wait_pid_ready(&mut pm, &pgdata, deadline, &ctx.log).await {
        pm.stop(&pgdata, &ctx.engine, Duration::from_secs(5), true).await;
        return Err(e);
    }
    loop {
        match cluster::probe(&ctx.endpoint, &ctx.secrets, probe_role(ctx.layout.kind())).await {
            Ok(()) => return Ok(pm),
            Err(e) if Instant::now() >= deadline || pm.exited().is_some() => {
                pm.stop(&pgdata, &ctx.engine, Duration::from_secs(5), true).await;
                return Err(EngineError::Start {
                    reason: format!("cannot log in as {}: {e}", probe_role(ctx.layout.kind()).name()),
                    log: ctx.log.lines(),
                });
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
}

async fn supervise(
    ctx: Arc<Ctx>,
    mut pm: Postmaster,
    mut rx: mpsc::Receiver<Cmd>,
    health: watch::Sender<EngineHealth>,
) {
    let pgdata = ctx.layout.pgdata();
    let mut failures: VecDeque<Instant> = VecDeque::new();
    let mut restarts = 0u32;
    let set = |state: EngineState, pid: Option<u32>, restarts: u32, last_error: Option<String>| {
        health.send_replace(EngineHealth {
            state,
            pid,
            restarts,
            last_error,
        });
    };
    loop {
        tokio::select! {
            status = pm.child.wait() => {
                pm.disarm_watcher().await;
                if ctx.stopping.load(Ordering::SeqCst) {
                    set(EngineState::Stopped, None, restarts, None);
                    return;
                }
                let mut last = format!(
                    "the postmaster exited unexpectedly ({})",
                    status.map_or_else(|e| e.to_string(), |s| s.to_string())
                );
                tracing::error!(target: "forgebase::postgres", node = ctx.layout.name(), "{last}; restarting");
                loop {
                    let now = Instant::now();
                    failures.push_back(now);
                    while failures.front().is_some_and(|t| now.duration_since(*t) > ctx.restart.window) {
                        failures.pop_front();
                    }
                    let attempt = u32::try_from(failures.len()).unwrap_or(u32::MAX);
                    if attempt >= ctx.restart.max_failures {
                        tracing::error!(
                            target: "forgebase::postgres",
                            node = ctx.layout.name(),
                            failures = attempt,
                            "PostgreSQL keeps failing; not restarting it again"
                        );
                        set(EngineState::Failed, None, restarts, Some(last));
                        // Stay reachable for stop requests.
                        while let Some(cmd) = rx.recv().await {
                            if let Cmd::Stop { reply, .. } = cmd {
                                let _ = reply.send(());
                            }
                        }
                        return;
                    }
                    set(EngineState::Restarting, None, restarts, Some(last.clone()));
                    let delay = ctx.restart.delay(attempt);
                    tokio::select! {
                        () = tokio::time::sleep(delay) => {}
                        cmd = rx.recv() => {
                            ctx.stopping.store(true, Ordering::SeqCst);
                            set(EngineState::Stopped, None, restarts, Some(last));
                            if let Some(Cmd::Stop { reply, .. }) = cmd {
                                let _ = reply.send(());
                            }
                            return;
                        }
                    }
                    if ctx.stopping.load(Ordering::SeqCst) {
                        set(EngineState::Stopped, None, restarts, Some(last));
                        return;
                    }
                    if let Err(e) = process::clear_stale(&pgdata, ctx.stop_timeout).await {
                        last = e.to_string();
                        continue;
                    }
                    match start_once(&ctx).await {
                        Ok(new) => {
                            pm = new;
                            restarts += 1;
                            tracing::info!(target: "forgebase::postgres", node = ctx.layout.name(), pid = pm.pid, "PostgreSQL restarted");
                            set(EngineState::Ready, Some(pm.pid), restarts, Some(last));
                            break;
                        }
                        Err(e) => {
                            last = e.to_string();
                            tracing::error!(target: "forgebase::postgres", node = ctx.layout.name(), error = %last, "PostgreSQL restart failed");
                        }
                    }
                }
            }
            cmd = rx.recv() => {
                match cmd {
                    Some(Cmd::Stop { immediate, reply }) => {
                        ctx.stopping.store(true, Ordering::SeqCst);
                        pm.stop(&pgdata, &ctx.engine, ctx.stop_timeout, immediate).await;
                        set(EngineState::Stopped, None, restarts, None);
                        let _ = reply.send(());
                        return;
                    }
                    #[cfg(unix)]
                    Some(Cmd::KillForTest) => {
                        process::kill_hard(pm.pid);
                    }
                    None => {
                        ctx.stopping.store(true, Ordering::SeqCst);
                        pm.stop(&pgdata, &ctx.engine, ctx.stop_timeout, false).await;
                        set(EngineState::Stopped, None, restarts, None);
                        return;
                    }
                }
            }
        }
    }
}

/// Deletes older extracted engines of the same major after a clean start
/// (minor upgrade by engine swap, ADR-0028 §4.5).
fn remove_old_engines(layout: &Layout, engine: &EngineDir) {
    if !matches!(engine.origin, EngineOrigin::Extracted { in_cache: false, .. }) {
        return;
    }
    let Ok(rd) = std::fs::read_dir(layout.engines_dir()) else {
        return;
    };
    let prefix = format!("{}.", engine.version.major);
    for e in rd.flatten() {
        let path = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if path == engine.root
            || !name.starts_with(&prefix)
            || name.ends_with(".partial")
            || !path.join(archive::MANIFEST_NAME).is_file()
        {
            continue;
        }
        match fsutil::remove_tree(&path) {
            Ok(()) => {
                tracing::info!(target: "forgebase::postgres", engine = %path.display(), "removed the previous engine")
            }
            Err(e) => {
                tracing::warn!(target: "forgebase::postgres", engine = %path.display(), error = %e, "could not remove the previous engine")
            }
        }
    }
}

/// A running, supervised cluster.
#[derive(Debug)]
pub struct EngineHandle {
    ctx: Arc<Ctx>,
    cmd: mpsc::Sender<Cmd>,
    health: watch::Receiver<EngineHealth>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl EngineHandle {
    /// Connection URL of `role` for the `forgebase` database. Unix:
    /// `postgres://<role>:<pw>@localhost/forgebase?host=<socket dir>&port=<port>`;
    /// TCP: `postgres://<role>:<pw>@127.0.0.1:<port>/forgebase`.
    pub fn url(&self, role: Role) -> Secret {
        self.url_for(role, DATABASE)
    }

    /// Connection URL of `role` for `database`.
    pub fn url_for(&self, role: Role, database: &str) -> Secret {
        let pw = self.ctx.secrets.password(role).unwrap_or_default();
        build_url(&self.ctx.endpoint, role.name(), pw, database)
    }

    /// The current health.
    pub fn health(&self) -> EngineHealth {
        self.health.borrow().clone()
    }

    /// A receiver that sees every health change.
    pub fn watch_health(&self) -> watch::Receiver<EngineHealth> {
        self.health.clone()
    }

    /// Fast shutdown (`SIGINT`; `SIGQUIT` after the stop timeout; on
    /// Windows `pg_ctl stop -m fast`). Idempotent.
    ///
    /// # Errors
    /// Never today; kept fallible for callers.
    pub async fn stop(&self) -> Result<()> {
        self.stop_mode(false).await
    }

    /// Immediate shutdown (`SIGQUIT`: no checkpoint, crash recovery on the
    /// next start).
    ///
    /// # Errors
    /// Never today; kept fallible for callers.
    pub async fn stop_immediate(&self) -> Result<()> {
        self.stop_mode(true).await
    }

    async fn stop_mode(&self, immediate: bool) -> Result<()> {
        self.ctx.stopping.store(true, Ordering::SeqCst);
        let (reply, rx) = oneshot::channel();
        if self.cmd.send(Cmd::Stop { immediate, reply }).await.is_ok() {
            let _ = rx.await;
        }
        let task = self.task.lock().ok().and_then(|mut t| t.take());
        if let Some(task) = task {
            let _ = task.await;
        }
        Ok(())
    }

    /// Engine version.
    pub fn version(&self) -> &PgVersion {
        &self.ctx.engine.version
    }

    /// The engine directory in use.
    pub fn engine(&self) -> &EngineDir {
        &self.ctx.engine
    }

    /// The cluster layout.
    pub fn layout(&self) -> &Layout {
        &self.ctx.layout
    }

    /// The Unix socket directory (`None` on Windows).
    pub fn socket_dir(&self) -> Option<&Path> {
        self.ctx.endpoint.socket_dir.as_deref()
    }

    /// The TCP address, when the engine listens on one.
    pub fn tcp_addr(&self) -> Option<SocketAddr> {
        self.ctx.endpoint.tcp
    }

    /// Port (socket name or TCP).
    pub fn port(&self) -> u16 {
        self.ctx.endpoint.port
    }

    /// `shared_preload_libraries` as written.
    pub fn preloaded(&self) -> &[&'static str] {
        &self.ctx.preload
    }

    /// Whether pg_cron is preloaded.
    pub fn has_cron(&self) -> bool {
        self.ctx.preload.contains(&"pg_cron")
    }

    /// Whether the server and reader roles preload Apache AGE
    /// ([`AGE_PRELOAD`] in their `session_preload_libraries`).
    pub fn has_age(&self) -> bool {
        self.ctx.age
    }

    /// Warnings from preparation (missing optional modules).
    pub fn warnings(&self) -> &[String] {
        &self.ctx.warnings
    }

    /// Postmaster pid, while running.
    pub fn pid(&self) -> Option<u32> {
        self.health().pid
    }

    /// The engine's last log lines.
    pub fn recent_log(&self) -> Vec<String> {
        self.ctx.log.lines()
    }

    /// Runs `pg_ctl <action> -D <pgdata>` (`reload`, or `promote -w`):
    /// signals the postmaster as the OS user that owns it, no SQL role
    /// needed (FB-168 standbys).
    ///
    /// # Errors
    /// `pg_ctl` fails; its output is in the message.
    pub async fn pg_ctl(&self, args: &[&str]) -> Result<()> {
        let bin = self.ctx.engine.bin("pg_ctl");
        let out = tokio::process::Command::new(&bin)
            .args(args)
            .arg("-D")
            .arg(self.ctx.layout.pgdata())
            .env_remove("PGDATA")
            .stdin(std::process::Stdio::null())
            .output()
            .await
            .map_err(|e| EngineError::io("run", &bin, e))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(EngineError::State(format!(
                "pg_ctl {} failed: {}{}",
                args.join(" "),
                String::from_utf8_lossy(&out.stdout).trim(),
                String::from_utf8_lossy(&out.stderr).trim()
            )))
        }
    }

    /// Tests: kill the postmaster with `SIGKILL` to exercise crash restart.
    #[cfg(unix)]
    #[doc(hidden)]
    pub async fn crash_for_test(&self) {
        let _ = self.cmd.send(Cmd::KillForTest).await;
    }
}

impl Drop for EngineHandle {
    fn drop(&mut self) {
        if self.ctx.stopping.swap(true, Ordering::SeqCst) {
            return;
        }
        // No async work in Drop: ask for a fast shutdown and let the
        // supervisor (or, if the runtime is gone, PostgreSQL itself) finish.
        let pid = self.ctx.pid.load(Ordering::SeqCst);
        if pid != 0 && self.health().pid.is_some() {
            process::request_fast_shutdown(pid);
        }
    }
}

fn build_url(ep: &Endpoint, role: &str, password: &str, database: &str) -> Secret {
    let userinfo = format!("{}:{}", pct(role), pct(password));
    let url = match (&ep.socket_dir, ep.tcp) {
        (Some(dir), _) => format!(
            "postgres://{userinfo}@localhost/{}?host={}&port={}",
            pct(database),
            pct_path(&dir.display().to_string()),
            ep.port
        ),
        (None, Some(addr)) => format!("postgres://{userinfo}@{addr}/{}", pct(database)),
        (None, None) => format!("postgres://{userinfo}@127.0.0.1:{}/{}", ep.port, pct(database)),
    };
    Secret::new(url)
}

fn pct_with(s: &str, keep: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) || keep.contains(&b) {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

fn pct(s: &str) -> String {
    pct_with(s, b"")
}

fn pct_path(s: &str) -> String {
    pct_with(s, b"/")
}

/// The data directory a layout belongs to (`<data_dir>` of `<data_dir>/postgres`).
pub fn data_dir_of(layout: &Layout) -> Option<PathBuf> {
    layout.postgres_dir().parent().map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_encoded() {
        let ep = Endpoint {
            socket_dir: Some(PathBuf::from("/data dir/postgres/run")),
            tcp: None,
            port: 5432,
        };
        let u = build_url(&ep, "forgebase_server", "ab12", "forgebase");
        assert_eq!(
            u.expose(),
            "postgres://forgebase_server:ab12@localhost/forgebase?host=/data%20dir/postgres/run&port=5432"
        );
        let ep = Endpoint {
            socket_dir: None,
            tcp: Some("127.0.0.1:5433".parse().expect("addr")),
            port: 5433,
        };
        assert_eq!(
            build_url(&ep, "forgebase_reader", "p&w", "forgebase").expose(),
            "postgres://forgebase_reader:p%26w@127.0.0.1:5433/forgebase"
        );
        let ep = Endpoint {
            socket_dir: None,
            tcp: Some("[::1]:5433".parse().expect("addr")),
            port: 5433,
        };
        assert_eq!(build_url(&ep, "r", "p", "d").expose(), "postgres://r:p@[::1]:5433/d");
    }

    #[test]
    fn engine_lock_entries_are_checked() {
        let lock = r#"
[engine]
postgres = "18.6"
build = "r1"

[archives."aarch64-apple-darwin"]
file = "forgebase-pg18.6-r1-aarch64-apple-darwin.tar.zst"
sha256 = "AAAA"

[archives."x86_64-unknown-linux-musl"]
sha256 = "bbbb"
"#;
        assert!(lock::check_archive(lock, "aarch64-apple-darwin", "aaaa").is_ok());
        let e = lock::check_archive(lock, "aarch64-apple-darwin", "cccc").expect_err("mismatch");
        assert!(e.contains("does not match"), "{e}");
        let e = lock::check_archive(lock, "x86_64-pc-windows-msvc", "bbbb").expect_err("other target");
        assert!(e.contains("x86_64-unknown-linux-musl"), "{e}");
        let e = lock::check_archive(lock, "x86_64-pc-windows-msvc", "dddd").expect_err("no entry");
        assert!(e.contains("no archive entry"), "{e}");
        assert!(lock::check_archive("not toml [", "t", "a").is_err());
        assert!(lock::check_archive("[engine]\n", "t", "a").is_err());
    }

    #[test]
    fn data_dir_of_layout() {
        assert_eq!(data_dir_of(&Layout::primary("/d")), Some(PathBuf::from("/d")));
    }
}
