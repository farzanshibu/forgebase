//! Managed standbys (ADR-0029 §2.2, §2.3, §2.8; FB-168): clone one with
//! `pg_basebackup -R`, point it at a primary, promote it, rewind an old
//! primary with `pg_rewind`, and supervise the replicas on this host.
//!
//! A standby is an ordinary node layout ([`Layout::node`] with
//! [`NodeKind::Standby`]) run by [`crate::Engine::prepare`]: the same
//! supervision, restart and log stream as the primary, its log lines tagged
//! with the node name.
//!
//! How a standby reaches its primary is ForgeBase's, not the user's
//! (`primary_conninfo` and `primary_slot_name` are on the deny list):
//! [`write_follow`] writes both into the standby's `postgresql.auto.conf`
//! (read after `postgresql.conf`, so nothing overrides it), without a
//! password: the replicator's password is in a private passfile next to the
//! data directory (`upstream.pgpass`, 0600), which `primary_conninfo` names.
//! Both settings reload without a restart, so a standby follows a new
//! primary after [`refollow`].

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::discover::EngineDir;
use crate::error::{EngineError, Result};
use crate::layout::{Layout, NodeKind};
use crate::replication::{self, Lsn};
use crate::secret::{Role, Secret};
use crate::{Engine, EngineConfig, EngineHandle, fsutil};

/// Node name of the original primary (`postgres/data`) once a replica was
/// promoted over it: it rejoins as a standby under this name.
pub const OLD_PRIMARY: &str = "primary";

/// Passfile with the replicator's password, next to the node's data directory.
pub const PASSFILE: &str = "upstream.pgpass";

/// TLS mode of a standby's connection to its primary (`sslmode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SslMode {
    /// No TLS: Unix sockets and loopback only.
    #[default]
    Disable,
    /// TLS required, certificate not verified.
    Require,
    /// TLS and the server certificate checked against the CA and host name.
    VerifyFull,
}

impl SslMode {
    /// The libpq value.
    pub fn as_str(self) -> &'static str {
        match self {
            SslMode::Disable => "disable",
            SslMode::Require => "require",
            SslMode::VerifyFull => "verify-full",
        }
    }
}

/// How a standby reaches its primary: the replicator's login and the slot.
#[derive(Debug, Clone)]
pub struct Upstream {
    /// Unix socket directory or host name / address.
    pub host: String,
    /// Port (names the socket on Unix).
    pub port: u16,
    /// TLS mode.
    pub sslmode: SslMode,
    /// Login role (`forgebase_replicator`, or `forgebase_super` for a local rewind).
    pub user: String,
    /// Its password.
    pub password: Secret,
    /// Physical slot on the primary (`fbr_<node>`).
    pub slot: String,
    /// `application_name`: the node name (shows in `pg_stat_replication`).
    pub application_name: String,
}

impl Upstream {
    /// The local `primary` (this host, its socket) for node `node`.
    ///
    /// # Errors
    /// The primary has no socket (Windows) and no TCP address.
    pub fn local(primary: &EngineHandle, node: &str) -> Result<Self> {
        let host = match (primary.socket_dir(), primary.tcp_addr()) {
            (Some(d), _) => d.display().to_string(),
            (None, Some(a)) => a.ip().to_string(),
            (None, None) => "127.0.0.1".to_owned(),
        };
        let password = primary
            .ctx
            .secrets
            .password(Role::Replicator)
            .ok_or_else(|| EngineError::Config("roles.secret has no forgebase_replicator password".into()))?;
        Ok(Self {
            host,
            port: primary.port(),
            sslmode: SslMode::Disable,
            user: Role::Replicator.name().to_owned(),
            password: Secret::new(password),
            slot: replication::slot_name(node),
            application_name: node.to_owned(),
        })
    }

    /// The same primary, logging in as `forgebase_super` (a local `pg_rewind`).
    pub fn as_super(mut self, primary: &EngineHandle) -> Self {
        self.user = Role::Super.name().to_owned();
        self.password = Secret::new(primary.ctx.secrets.password(Role::Super).unwrap_or_default());
        self
    }

    /// A libpq connection string naming `passfile` (never the password).
    pub fn conninfo(&self, passfile: &Path, dbname: Option<&str>) -> String {
        let mut parts = vec![
            ("host", self.host.clone()),
            ("port", self.port.to_string()),
            ("user", self.user.clone()),
            ("passfile", passfile.display().to_string()),
            ("sslmode", self.sslmode.as_str().to_owned()),
            ("application_name", self.application_name.clone()),
        ];
        if let Some(d) = dbname {
            parts.push(("dbname", d.to_owned()));
        }
        parts
            .into_iter()
            .map(|(k, v)| format!("{k}={}", conninfo_quote(&v)))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// A connection URL for `database` on the primary with the password
    /// (for slot functions over a normal connection; never log it).
    pub fn url(&self, database: &str) -> Secret {
        let host = if self.host.starts_with('/') {
            format!("localhost/{}?host={}&port={}&sslmode={}", crate::pct(database), crate::pct_path(&self.host), self.port, self.sslmode.as_str())
        } else {
            let h = if self.host.contains(':') { format!("[{}]", self.host) } else { self.host.clone() };
            format!("{h}:{}/{}?sslmode={}", self.port, crate::pct(database), self.sslmode.as_str())
        };
        Secret::new(format!(
            "postgres://{}:{}@{host}&application_name={}",
            crate::pct(&self.user),
            crate::pct(self.password.expose()),
            crate::pct(&self.application_name)
        ))
    }

    /// Writes the passfile (`*:*:*:user:password`, 0600) at `path`.
    ///
    /// # Errors
    /// File-system errors.
    pub fn write_passfile(&self, path: &Path) -> Result<()> {
        let mut f = fsutil::create_private_file(path)?;
        writeln!(
            f,
            "*:*:*:{}:{}",
            pgpass_field(&self.user),
            pgpass_field(self.password.expose())
        )
        .and_then(|()| f.sync_all())
        .map_err(|e| EngineError::io("write", path, e))
    }
}

fn conninfo_quote(v: &str) -> String {
    let mut s = String::from("'");
    for c in v.chars() {
        if c == '\'' || c == '\\' {
            s.push('\\');
        }
        s.push(c);
    }
    s.push('\'');
    s
}

fn pgpass_field(s: &str) -> String {
    s.replace('\\', "\\\\").replace(':', "\\:")
}

/// The passfile of a node layout.
pub fn passfile(layout: &Layout) -> PathBuf {
    layout.node_dir().join(PASSFILE)
}

fn tail(bytes: &[u8], lines: usize) -> String {
    let text = String::from_utf8_lossy(bytes);
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

/// Options of [`clone_from`].
#[derive(Debug, Clone, Copy, Default)]
pub struct CloneOptions {
    /// Tests: `--no-sync`.
    pub no_sync: bool,
}

/// Clones the primary `up` points at into `layout`'s data directory with
/// `pg_basebackup -D … -X stream -R -S <slot> --checkpoint=fast` (into
/// `data.partial`, renamed when complete) and writes the follow settings.
/// The slot must exist on the primary ([`replication::ensure_slot`]).
///
/// # Errors
/// A data directory already exists, or `pg_basebackup` fails (its last
/// lines are in the message).
pub async fn clone_from(engine: &EngineDir, layout: &Layout, up: &Upstream, opts: CloneOptions) -> Result<()> {
    let pgdata = layout.pgdata();
    if pgdata.exists() {
        return Err(EngineError::Cluster {
            dir: pgdata,
            reason: "a data directory already exists; remove it before cloning (rebuild does)".into(),
        });
    }
    fsutil::private_dir(layout.postgres_dir())?;
    fsutil::private_dir(&layout.node_dir())?;
    let pass = passfile(layout);
    up.write_passfile(&pass)?;
    let partial = layout.node_dir().join("data.partial");
    let _ = fsutil::remove_tree(&partial);
    let bin = engine.bin("pg_basebackup");
    let mut cmd = tokio::process::Command::new(&bin);
    cmd.arg("-D")
        .arg(&partial)
        .args(["-X", "stream", "-R", "--checkpoint=fast", "-S", &up.slot, "-d"])
        .arg(up.conninfo(&pass, None));
    if opts.no_sync {
        cmd.arg("--no-sync");
    }
    let out = cmd
        .env_remove("PGPASSWORD")
        .env_remove("PGDATA")
        .env("PGPASSFILE", &pass)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| EngineError::io("run", &bin, e))?;
    if !out.status.success() {
        let _ = fsutil::remove_tree(&partial);
        return Err(EngineError::Cluster {
            dir: pgdata,
            reason: format!("pg_basebackup failed:\n{}", tail(&out.stderr, 15)),
        });
    }
    fsutil::set_mode(&partial, 0o700)?;
    std::fs::rename(&partial, &pgdata).map_err(|e| EngineError::io("rename into place", &pgdata, e))?;
    write_follow(layout, up)?;
    tracing::info!(target: "forgebase::postgres", node = layout.name(), "cloned the primary");
    Ok(())
}

/// Keys of `postgresql.auto.conf` that ForgeBase writes for a standby.
const FOLLOW_KEYS: &[&str] = &["primary_conninfo", "primary_slot_name"];

/// Writes `primary_conninfo` (with the passfile, no password) and
/// `primary_slot_name` into the standby's `postgresql.auto.conf`, replacing
/// what `pg_basebackup -R` or `pg_rewind` wrote, and `standby.signal`.
///
/// # Errors
/// File-system errors.
pub fn write_follow(layout: &Layout, up: &Upstream) -> Result<()> {
    let pass = passfile(layout);
    up.write_passfile(&pass)?;
    let pgdata = layout.pgdata();
    let mut lines = auto_conf_without(&pgdata, FOLLOW_KEYS)?;
    lines.push(format!(
        "primary_conninfo = '{}'",
        up.conninfo(&pass, None).replace('\'', "''")
    ));
    lines.push(format!("primary_slot_name = '{}'", up.slot.replace('\'', "''")));
    write_auto_conf(&pgdata, &lines)?;
    let signal = pgdata.join("standby.signal");
    if !signal.exists() {
        fsutil::create_private_file(&signal)?;
    }
    Ok(())
}

/// Removes the follow settings (a promoted node is a primary).
///
/// # Errors
/// File-system errors.
pub fn clear_follow(layout: &Layout) -> Result<()> {
    let pgdata = layout.pgdata();
    let lines = auto_conf_without(&pgdata, FOLLOW_KEYS)?;
    write_auto_conf(&pgdata, &lines)?;
    let _ = std::fs::remove_file(passfile(layout));
    Ok(())
}

fn auto_conf_without(pgdata: &Path, keys: &[&str]) -> Result<Vec<String>> {
    let path = pgdata.join("postgresql.auto.conf");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(EngineError::io("read", &path, e)),
    };
    Ok(text
        .lines()
        .filter(|l| {
            let key = l.split('=').next().unwrap_or("").trim().to_ascii_lowercase();
            !keys.contains(&key.as_str())
        })
        .map(str::to_owned)
        .collect())
}

fn write_auto_conf(pgdata: &Path, lines: &[String]) -> Result<()> {
    let path = pgdata.join("postgresql.auto.conf");
    let tmp = pgdata.join("postgresql.auto.conf.forgebase-tmp");
    let mut text = lines.join("\n");
    text.push('\n');
    {
        let mut f = fsutil::create_private_file(&tmp)?;
        f.write_all(text.as_bytes())
            .and_then(|()| f.sync_all())
            .map_err(|e| EngineError::io("write", &tmp, e))?;
    }
    std::fs::rename(&tmp, &path).map_err(|e| EngineError::io("rename to", &path, e))
}

/// Points a running standby at another primary (after a promote): rewrites
/// the follow settings and reloads; the standby follows the new timeline
/// (`recovery_target_timeline = latest`).
///
/// # Errors
/// File-system or SQL errors.
pub async fn refollow(standby: &EngineHandle, up: &Upstream) -> Result<()> {
    write_follow(standby.layout(), up)?;
    reload(standby).await
}

/// Reloads the node's configuration (`pg_ctl reload`).
///
/// # Errors
/// `pg_ctl` fails.
pub async fn reload(handle: &EngineHandle) -> Result<()> {
    handle.pg_ctl(&["reload", "-s"]).await
}

/// The URL standby-side checks use: `forgebase_replicator` on database
/// `postgres`. A remote replica's host holds only that password, never the
/// superuser's, so everything this module does on a standby works with it.
pub fn standby_url(handle: &EngineHandle) -> Secret {
    handle.url_for(Role::Replicator, "postgres")
}

/// Facts of a running node ([`replication::facts`]) as the replicator.
///
/// # Errors
/// SQL errors.
pub async fn facts(handle: &EngineHandle) -> Result<replication::ClusterFacts> {
    let mut c = replication::connect(&standby_url(handle)).await?;
    replication::facts(&mut c).await
}

/// Waits until the standby replayed `lsn` (`pg_last_wal_replay_lsn() >= lsn`),
/// polling every 20 ms. Returns the replayed position.
///
/// # Errors
/// `timeout` passed first, or SQL errors.
pub async fn wait_replayed(standby: &EngineHandle, lsn: Lsn, timeout: Duration) -> Result<Lsn> {
    let mut c = replication::connect(&standby_url(standby)).await?;
    let deadline = Instant::now() + timeout;
    loop {
        let at: Option<String> = sqlx::query_scalar("SELECT pg_last_wal_replay_lsn()::text")
            .fetch_one(&mut c)
            .await
            .map_err(|e| EngineError::Sql(format!("pg_last_wal_replay_lsn: {e}")))?;
        let at = at.map(|s| s.parse::<Lsn>()).transpose()?;
        if let Some(at) = at.filter(|a| *a >= lsn) {
            return Ok(at);
        }
        if Instant::now() >= deadline {
            return Err(EngineError::State(format!(
                "standby {} replayed {} but not {lsn} within {timeout:?}",
                standby.layout().name(),
                at.map_or_else(|| "nothing".to_owned(), |a| a.to_string())
            )));
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Waits until the standby's WAL receiver is `streaming` (and received
/// WAL from its primary).
///
/// # Errors
/// `timeout` passed first, or SQL errors.
pub async fn wait_streaming(standby: &EngineHandle, timeout: Duration) -> Result<()> {
    let mut c = replication::connect(&standby_url(standby)).await?;
    let deadline = Instant::now() + timeout;
    loop {
        let status = replication::receiver_status(&mut c).await?;
        if status.as_deref() == Some("streaming") {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(EngineError::State(format!(
                "standby {} is not streaming after {timeout:?} (receiver: {})",
                standby.layout().name(),
                status.as_deref().unwrap_or("none")
            )));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Promotes the standby (`pg_ctl promote -w`), then removes its
/// follow settings. The node is a primary on a new timeline when this
/// returns.
///
/// # Errors
/// The node is not in recovery, or promotion did not finish in time.
pub async fn promote(standby: &EngineHandle, timeout: Duration) -> Result<()> {
    let mut c = replication::connect(&standby_url(standby)).await?;
    let in_recovery: bool = sqlx::query_scalar("SELECT pg_is_in_recovery()")
        .fetch_one(&mut c)
        .await
        .map_err(|e| EngineError::Sql(format!("pg_is_in_recovery: {e}")))?;
    if !in_recovery {
        return Err(EngineError::State(format!(
            "node {} is not a standby; nothing to promote",
            standby.layout().name()
        )));
    }
    // `pg_ctl promote -w` is `pg_promote(true, …)` without a superuser:
    // ForgeBase owns the postmaster, a remote host has no superuser login.
    let secs = timeout.as_secs().clamp(1, 3600).to_string();
    standby.pg_ctl(&["promote", "-w", "-s", "-t", &secs]).await?;
    let deadline = Instant::now() + timeout;
    loop {
        let still: bool = sqlx::query_scalar("SELECT pg_is_in_recovery()")
            .fetch_one(&mut c)
            .await
            .map_err(|e| EngineError::Sql(format!("pg_is_in_recovery: {e}")))?;
        if !still {
            break;
        }
        if Instant::now() >= deadline {
            return Err(EngineError::State(format!(
                "node {} did not finish promotion within {secs} s",
                standby.layout().name()
            )));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    clear_follow(standby.layout())?;
    tracing::info!(target: "forgebase::postgres", node = standby.layout().name(), "promoted to primary");
    Ok(())
}

/// How [`rejoin`] brought an old primary back as a standby.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejoinHow {
    /// `pg_rewind` replayed only the divergence.
    Rewound,
    /// A fresh clone (no `pg_rewind` in the engine, or it failed).
    Recloned,
}

/// Rewinds the stopped cluster of `layout` to `source` (a superuser or the
/// replicator with the rewind grants, database `postgres`) with
/// `pg_rewind`. The target must be stopped, and both clusters need
/// `full_page_writes = on` (ForgeBase's default; only the tests' fast mode
/// turns it off) and data checksums (`initdb --data-checksums`).
///
/// # Errors
/// The engine has no `pg_rewind`, or it fails (last lines in the message).
pub async fn rewind(engine: &EngineDir, layout: &Layout, source: &Upstream, opts: CloneOptions) -> Result<()> {
    let bin = engine.bin("pg_rewind");
    if !bin.is_file() {
        return Err(EngineError::Engine {
            dir: engine.root.clone(),
            reason: "this engine has no pg_rewind".into(),
        });
    }
    let pass = layout.node_dir().join("rewind.pgpass");
    source.write_passfile(&pass)?;
    let mut cmd = tokio::process::Command::new(&bin);
    cmd.arg("--target-pgdata")
        .arg(layout.pgdata())
        .arg(format!("--source-server={}", source.conninfo(&pass, Some("postgres"))));
    if opts.no_sync {
        cmd.arg("--no-sync");
    }
    let out = cmd
        .env_remove("PGPASSWORD")
        .env_remove("PGDATA")
        .env("PGPASSFILE", &pass)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await;
    let _ = std::fs::remove_file(&pass);
    let out = out.map_err(|e| EngineError::io("run", &bin, e))?;
    if !out.status.success() {
        return Err(EngineError::Cluster {
            dir: layout.pgdata(),
            reason: format!("pg_rewind failed:\n{}", tail(&out.stderr, 15)),
        });
    }
    tracing::info!(target: "forgebase::postgres", node = layout.name(), "rewound to the new primary");
    Ok(())
}

/// Brings the stopped old primary of `layout` back as a standby of `up`:
/// `pg_rewind` from `rewind_source` when given and possible, otherwise (or
/// when it fails) a fresh clone. Writes the follow settings either way.
/// The slot `up.slot` must exist on the new primary.
///
/// # Errors
/// Cloning fails too.
pub async fn rejoin(
    engine: &EngineDir,
    layout: &Layout,
    up: &Upstream,
    rewind_source: Option<&Upstream>,
    opts: CloneOptions,
) -> Result<RejoinHow> {
    if let Some(src) = rewind_source {
        match rewind(engine, layout, src, opts).await {
            Ok(()) => {
                // pg_rewind copies the source's configuration files.
                write_follow(layout, up)?;
                return Ok(RejoinHow::Rewound);
            }
            Err(e) => {
                tracing::warn!(target: "forgebase::postgres", node = layout.name(), error = %e, "pg_rewind failed; cloning instead");
            }
        }
    }
    let pgdata = layout.pgdata();
    if pgdata.exists() {
        let aside = layout.node_dir().join("data.rejoin-old");
        let _ = fsutil::remove_tree(&aside);
        std::fs::rename(&pgdata, &aside).map_err(|e| EngineError::io("move aside", &pgdata, e))?;
        let r = clone_from(engine, layout, up, opts).await;
        match r {
            Ok(()) => {
                let _ = fsutil::remove_tree(&aside);
            }
            Err(e) => {
                let _ = fsutil::remove_tree(&pgdata);
                let _ = std::fs::rename(&aside, &pgdata);
                return Err(e);
            }
        }
    } else {
        clone_from(engine, layout, up, opts).await?;
    }
    Ok(RejoinHow::Recloned)
}

/// Resolves the engine an [`EngineConfig`] names for `layout` (embedded,
/// archive or directory), extracting it when needed, without starting a
/// cluster: what a standby needs before its first start.
///
/// # Errors
/// As [`crate::Engine::prepare`]'s discovery step.
pub async fn resolve_engine(layout: &Layout, cfg: &EngineConfig) -> Result<EngineDir> {
    fsutil::private_dir(layout.postgres_dir())?;
    crate::discover::resolve(cfg, layout).await
}

/// Free bytes on the file system holding `path` (heartbeats).
pub fn disk_free(path: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        rustix::fs::statvfs(path)
            .ok()
            .map(|s| s.f_bavail.saturating_mul(s.f_frsize))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

// ---------------------------------------------------------------------------
// Replicas on this host.
// ---------------------------------------------------------------------------

/// What [`LocalReplicas::status`] reports per node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalReplicaStatus {
    /// Node name.
    pub name: String,
    /// Engine state (`ready`, `restarting`, `failed`, `stopped`).
    pub engine: String,
    /// Postmaster pid.
    pub pid: Option<u32>,
    /// Restarts since start.
    pub restarts: u32,
}

/// The managed replicas on this host (ADR-0029 §2.2): their data under
/// `<data_dir>/postgres/nodes/<name>`, each a supervised [`EngineHandle`].
/// The server keeps one and calls it from the `database.node.*` jobs.
#[derive(Debug)]
pub struct LocalReplicas {
    data_dir: PathBuf,
    cfg: EngineConfig,
    clone: CloneOptions,
    nodes: Mutex<BTreeMap<String, Arc<EngineHandle>>>,
}

impl LocalReplicas {
    /// Replicas of the cluster in `data_dir`, started with `cfg` (the
    /// primary's engine configuration; replicas never listen on TCP).
    pub fn new(data_dir: impl Into<PathBuf>, cfg: &EngineConfig) -> Self {
        let mut cfg = cfg.clone();
        cfg.listen = None;
        cfg.remove_old_engines = false;
        let clone = CloneOptions {
            no_sync: cfg.unsafe_fast,
        };
        Self {
            data_dir: data_dir.into(),
            cfg,
            clone,
            nodes: Mutex::new(BTreeMap::new()),
        }
    }

    /// The layout of replica `name`: `postgres/nodes/<name>`, or, for
    /// [`OLD_PRIMARY`], the old primary's `postgres/data` as a standby.
    ///
    /// # Errors
    /// An invalid node name.
    pub fn layout(&self, name: &str) -> Result<Layout> {
        if name == OLD_PRIMARY {
            return Ok(Layout::primary(&self.data_dir).with_kind(NodeKind::Standby));
        }
        Layout::node(&self.data_dir, name, NodeKind::Standby)
    }

    /// The data directory replicas live under.
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// The engine configuration replicas start with.
    pub fn config(&self) -> &EngineConfig {
        &self.cfg
    }

    /// Provisions replica `name` of `primary` (ADR-0029 §2.2): slot
    /// `fbr_<name>`, clone, start, supervised. A half-made clone is removed
    /// on failure; the slot is kept for a retry (`remove` drops it).
    ///
    /// # Errors
    /// The node exists, or a step fails.
    pub async fn add(&self, primary: &EngineHandle, name: &str) -> Result<Arc<EngineHandle>> {
        let layout = self.layout(name)?;
        if self.nodes.lock().await.contains_key(name) {
            return Err(EngineError::State(format!("replica {name} is already running")));
        }
        let up = Upstream::local(primary, name)?;
        {
            let mut c = replication::connect(&primary.url_for(Role::Super, "postgres")).await?;
            replication::ensure_slot(&mut c, &up.slot).await?;
        }
        clone_from(primary.engine(), &layout, &up, self.clone).await?;
        let handle = match Engine::prepare(&layout, &self.cfg).await {
            Ok(h) => Arc::new(h),
            Err(e) => {
                let _ = fsutil::remove_tree(&layout.pgdata());
                return Err(e);
            }
        };
        self.nodes.lock().await.insert(name.to_owned(), handle.clone());
        Ok(handle)
    }

    /// Starts every provisioned replica that is not running (server start).
    /// Returns each node tried with its outcome.
    pub async fn start_existing(&self) -> Vec<(String, Result<()>)> {
        let dir = self.data_dir.join("postgres").join("nodes");
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().join("data").join("standby.signal").is_file())
            .filter_map(|e| e.file_name().to_str().map(str::to_owned))
            .collect();
        names.sort();
        if self.data_dir.join("postgres/data/standby.signal").is_file() {
            names.push(OLD_PRIMARY.to_owned());
        }
        let mut out = Vec::new();
        for name in names {
            if self.nodes.lock().await.contains_key(&name) {
                continue;
            }
            let r = self.start(&name).await.map(|_| ());
            out.push((name, r));
        }
        out
    }

    /// Starts the provisioned replica `name`.
    ///
    /// # Errors
    /// Not provisioned, or the engine fails to start.
    pub async fn start(&self, name: &str) -> Result<Arc<EngineHandle>> {
        if let Some(h) = self.get(name).await {
            return Ok(h);
        }
        let layout = self.layout(name)?;
        let h = Arc::new(Engine::prepare(&layout, &self.cfg).await?);
        self.nodes.lock().await.insert(name.to_owned(), h.clone());
        Ok(h)
    }

    /// Takes over an already running handle (a promoted node keeps running
    /// as the new primary; the caller decides who owns it).
    pub async fn take(&self, name: &str) -> Option<Arc<EngineHandle>> {
        self.nodes.lock().await.remove(name)
    }

    /// Adopts a running handle as replica `name` (an old primary rejoined).
    pub async fn adopt(&self, name: &str, handle: Arc<EngineHandle>) {
        self.nodes.lock().await.insert(name.to_owned(), handle);
    }

    /// The running replica `name`.
    pub async fn get(&self, name: &str) -> Option<Arc<EngineHandle>> {
        self.nodes.lock().await.get(name).cloned()
    }

    /// Names of the running replicas.
    pub async fn names(&self) -> Vec<String> {
        self.nodes.lock().await.keys().cloned().collect()
    }

    /// Engine state of every running replica.
    pub async fn status(&self) -> Vec<LocalReplicaStatus> {
        self.nodes
            .lock()
            .await
            .iter()
            .map(|(n, h)| {
                let health = h.health();
                LocalReplicaStatus {
                    name: n.clone(),
                    engine: health.state.as_str().to_owned(),
                    pid: health.pid,
                    restarts: health.restarts,
                }
            })
            .collect()
    }

    /// Stops replica `name` (fast shutdown); its data stays.
    ///
    /// # Errors
    /// Never today.
    pub async fn stop(&self, name: &str) -> Result<()> {
        let h = self.nodes.lock().await.remove(name);
        if let Some(h) = h {
            h.stop().await?;
        }
        Ok(())
    }

    /// Stops replica `name`, drops its slot on `primary` and deletes its
    /// directory.
    ///
    /// # Errors
    /// SQL or file-system errors.
    pub async fn remove(&self, primary: Option<&EngineHandle>, name: &str) -> Result<()> {
        self.stop(name).await?;
        if let Some(p) = primary {
            let mut c = replication::connect(&p.url_for(Role::Super, "postgres")).await?;
            replication::drop_slot(&mut c, &replication::slot_name(name)).await?;
        }
        let layout = self.layout(name)?;
        if layout.is_primary_layout() {
            // The old primary lives in `postgres/data`; `postgres/` itself
            // holds the engine, roles.secret and every other node.
            let pgdata = layout.pgdata();
            if pgdata.exists() {
                fsutil::remove_tree(&pgdata).map_err(|e| EngineError::io("remove", &pgdata, e))?;
            }
            let _ = std::fs::remove_file(passfile(&layout));
            return Ok(());
        }
        let dir = layout.node_dir();
        if dir.exists() {
            fsutil::remove_tree(&dir).map_err(|e| EngineError::io("remove", &dir, e))?;
        }
        Ok(())
    }

    /// Re-clones replica `name` from `primary` (slot invalidated, restored
    /// primary, diverged): stop, delete the data, recreate the slot, clone,
    /// start.
    ///
    /// # Errors
    /// A step fails.
    pub async fn rebuild(&self, primary: &EngineHandle, name: &str) -> Result<Arc<EngineHandle>> {
        self.stop(name).await?;
        let layout = self.layout(name)?;
        let pgdata = layout.pgdata();
        if pgdata.exists() {
            fsutil::remove_tree(&pgdata).map_err(|e| EngineError::io("remove", &pgdata, e))?;
        }
        {
            let mut c = replication::connect(&primary.url_for(Role::Super, "postgres")).await?;
            replication::drop_slot(&mut c, &replication::slot_name(name)).await?;
        }
        self.add(primary, name).await
    }

    /// Stops every replica.
    pub async fn stop_all(&self) {
        let all: Vec<Arc<EngineHandle>> = std::mem::take(&mut *self.nodes.lock().await).into_values().collect();
        for h in all {
            let _ = h.stop().await;
        }
    }
}

// ---------------------------------------------------------------------------
// Promote on this host (ADR-0029 §2.8 steps 1-4, 6).
// ---------------------------------------------------------------------------

/// What [`promote_local`] did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromoteReport {
    /// The promoted node.
    pub node: String,
    /// The old primary stopped cleanly and the target replayed its last
    /// record before promotion: no commit was lost.
    pub clean: bool,
    /// The old primary's shutdown checkpoint (its last record), when clean.
    pub old_primary_end: Option<Lsn>,
    /// What the target had replayed when it was promoted.
    pub replayed: Option<Lsn>,
    /// The new primary's timeline.
    pub timeline: Option<i64>,
    /// Other replicas pointed at the new primary.
    pub refollowed: Vec<String>,
}

/// The old primary of a local promote.
#[derive(Debug)]
pub enum OldPrimary<'a> {
    /// Running here: ForgeBase stops it (the fence).
    Running(&'a EngineHandle),
    /// Already stopped (or gone); its data directory is `layout`'s.
    Stopped {
        /// Its layout, read with `pg_controldata` when it still exists.
        layout: &'a Layout,
        /// Its engine.
        engine: &'a EngineDir,
    },
}

/// Promotes local replica `target` (ADR-0029 §2.8):
///
/// 1. checks the target is a standby;
/// 2. fences the old primary: the caller has closed its writer pool; this
///    stops it with a fast shutdown;
/// 3. if it stopped cleanly (`pg_controldata`: `shut down`), waits until the
///    target replayed past its shutdown checkpoint, so no commit is lost;
///    otherwise refuses unless `accept_data_loss`;
/// 4. `pg_promote(true, 60)`;
/// 6. points every other replica in `replicas` at the new primary (new
///    slots, `primary_conninfo` reloaded).
///
/// Step 5 (pools, control tables, epoch, `topology.json`) is the caller's:
/// [`crate::replication::service::promote`].
///
/// # Errors
/// A step fails; nothing after it ran. A refused unclean stop leaves the
/// old primary stopped and the target a standby.
pub async fn promote_local(
    replicas: &LocalReplicas,
    target: &str,
    old: OldPrimary<'_>,
    accept_data_loss: bool,
    replay_timeout: Duration,
) -> Result<PromoteReport> {
    let standby = replicas
        .get(target)
        .await
        .ok_or_else(|| EngineError::State(format!("replica {target} is not running here")))?;
    let before = facts(&standby).await?;
    if !before.in_recovery {
        return Err(EngineError::State(format!("node {target} is not a standby")));
    }
    let (layout, engine) = match &old {
        OldPrimary::Running(h) => {
            h.stop().await?;
            (h.layout().clone(), h.engine().clone())
        }
        OldPrimary::Stopped { layout, engine } => ((*layout).clone(), (*engine).clone()),
    };
    let control = if layout.pgdata().join("global").join("pg_control").is_file() {
        Some(replication::control_data(&engine, &layout.pgdata()).await?)
    } else {
        None
    };
    let clean_end = control
        .as_ref()
        .filter(|c| c.primary_stopped_cleanly())
        .map(|c| c.checkpoint);
    let replayed = match clean_end {
        Some(end) => {
            // The shutdown checkpoint record starts at `end`; the standby
            // has replayed it once its replay position is past `end`.
            Some(wait_replayed(&standby, Lsn(end.0 + 1), replay_timeout).await?)
        }
        None if accept_data_loss => facts(&standby).await?.lsn,
        None => {
            return Err(EngineError::State(format!(
                "the old primary did not stop cleanly ({}), so commits after {} may be lost: \
                 confirm with accept_data_loss",
                control.as_ref().map_or("no pg_control", |c| c.state.as_str()),
                before.lsn.map_or_else(|| "the last replayed position".to_owned(), |l| l.to_string())
            )));
        }
    };
    promote(&standby, Duration::from_secs(60)).await?;
    let after = facts(&standby).await?;
    let mut refollowed = Vec::new();
    for name in replicas.names().await {
        if name == target {
            continue;
        }
        let Some(h) = replicas.get(&name).await else { continue };
        let up = Upstream::local(&standby, &name)?;
        {
            let mut c = replication::connect(&standby.url_for(Role::Super, "postgres")).await?;
            replication::ensure_slot(&mut c, &up.slot).await?;
        }
        refollow(&h, &up).await?;
        refollowed.push(name);
    }
    Ok(PromoteReport {
        node: target.to_owned(),
        clean: clean_end.is_some(),
        old_primary_end: clean_end,
        replayed,
        timeline: after.timeline,
        refollowed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn up() -> Upstream {
        Upstream {
            host: "/d/postgres/run".into(),
            port: 5432,
            sslmode: SslMode::Disable,
            user: "forgebase_replicator".into(),
            password: Secret::new("p:w\\x"),
            slot: "fbr_r1".into(),
            application_name: "r1".into(),
        }
    }

    #[test]
    fn conninfo_never_holds_the_password() {
        let c = up().conninfo(Path::new("/d/postgres/nodes/r1/upstream.pgpass"), None);
        assert_eq!(
            c,
            "host='/d/postgres/run' port='5432' user='forgebase_replicator' \
             passfile='/d/postgres/nodes/r1/upstream.pgpass' sslmode='disable' application_name='r1'"
        );
        assert!(!c.contains("p:w"));
        let c = up().conninfo(Path::new("/x y/it's"), Some("postgres"));
        assert!(c.contains("passfile='/x y/it\\'s'"), "{c}");
        assert!(c.ends_with("dbname='postgres'"));
    }

    #[test]
    fn follow_settings_replace_what_basebackup_wrote() {
        let dir = tempfile::tempdir().expect("tempdir");
        let layout = Layout::node(dir.path(), "r1", NodeKind::Standby).expect("layout");
        std::fs::create_dir_all(layout.pgdata()).expect("pgdata");
        std::fs::write(
            layout.pgdata().join("postgresql.auto.conf"),
            "# Do not edit this file manually!\nwork_mem = '8MB'\nprimary_conninfo = 'user=x password=secret'\nprimary_slot_name = 'old'\n",
        )
        .expect("write");
        write_follow(&layout, &up()).expect("follow");
        let text = std::fs::read_to_string(layout.pgdata().join("postgresql.auto.conf")).expect("read");
        assert!(text.contains("work_mem = '8MB'"));
        assert!(!text.contains("secret"), "{text}");
        assert!(!text.contains("'old'"));
        assert!(text.contains("primary_slot_name = 'fbr_r1'"));
        assert!(text.contains("passfile=''"), "quotes are doubled: {text}");
        assert!(layout.pgdata().join("standby.signal").is_file());
        let pass = std::fs::read_to_string(passfile(&layout)).expect("passfile");
        assert_eq!(pass, "*:*:*:forgebase_replicator:p\\:w\\\\x\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(passfile(&layout)).expect("meta").permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        clear_follow(&layout).expect("clear");
        let text = std::fs::read_to_string(layout.pgdata().join("postgresql.auto.conf")).expect("read");
        assert!(!text.contains("primary_"), "{text}");
        assert!(!passfile(&layout).exists());
    }
}
