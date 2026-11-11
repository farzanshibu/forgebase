//! `forgebase node` (ADR-0029 §2.3, FB-168): run this host's PostgreSQL as a
//! managed replica of a ForgeBase server on another host.
//!
//! The agent is engine-only: no HTTP API, no services. It joins once with a
//! one-time token (`forgebase node join <server url> --token … --data-dir
//! DIR`), clones the primary with `pg_basebackup -R` as
//! `forgebase_replicator`, runs the engine under the same supervision as the
//! server's, and keeps an outbound long-poll for commands (`rebuild`,
//! `stop`, `promote`, `rejoin`, `follow`) plus a heartbeat every 5 s (state,
//! replay position, free disk, engine version). Nothing listens on this
//! host but PostgreSQL itself.
//!
//! State lives in the data directory: `node.json` (node id, agent key,
//! replication login; 0600) and `postgres/roles.secret` holding only the
//! replicator's password (this host never learns the superuser's).
//! `forgebase node run --data-dir DIR` resumes after a restart.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use clap::{Args, Subcommand};
use forgebase_pgengine::replication::service::{
    self, AgentState, CommandBatch, CommandKind, CommandResult, Heartbeat, HeartbeatReply, JoinGrant, JoinRequest,
    NodeCommand, UpstreamGrant,
};
use forgebase_pgengine::replication::{self, EngineIdentity, Lsn};
use forgebase_pgengine::standby::{self, CloneOptions};
use forgebase_pgengine::{Engine, EngineConfig, EngineHandle, EngineSource, Layout, NodeKind, Role};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::GlobalArgs;
use crate::probe;

/// `forgebase node …`.
#[derive(Debug, Args)]
pub struct NodeCli {
    #[command(subcommand)]
    command: NodeCommandCli,
}

#[derive(Debug, Subcommand)]
enum NodeCommandCli {
    /// Join a ForgeBase server as a replica (token from the dashboard or
    /// `POST /api/system/database/join-tokens`) and keep running.
    Join(JoinArgs),
    /// Resume an agent that joined before (after a restart of this host).
    Run(EngineArgs),
    /// Print what this host's agent knows about itself.
    Status {
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Clone, Args)]
struct EngineArgs {
    /// PostgreSQL engine directory (default: the embedded engine, else
    /// `FORGEBASE_PG_ENGINE_DIR`). Must be the server's exact engine.
    #[arg(long, value_name = "DIR")]
    engine_dir: Option<PathBuf>,
    /// TCP address the replica listens on for the server's read pool
    /// (TLS required, FB-153).
    #[arg(long, value_name = "ADDR")]
    listen: Option<String>,
}

#[derive(Debug, Args)]
struct JoinArgs {
    /// The ForgeBase server, e.g. `http://10.0.0.5:8090`.
    server: String,
    /// The one-time join token (`fbj_…`, valid 15 minutes).
    #[arg(long)]
    token: String,
    /// Node name (a-z, 0-9, '-', '_'; 1-32). Default: this host's name.
    #[arg(long)]
    name: Option<String>,
    #[command(flatten)]
    engine: EngineArgs,
}

/// Runs `forgebase node`.
///
/// # Errors
/// Any step of the agent.
pub async fn run(g: &GlobalArgs, cli: NodeCli) -> anyhow::Result<ExitCode> {
    let data_dir = g
        .data_dir
        .clone()
        .context("forgebase node needs --data-dir DIR (where this host keeps the replica)")?;
    match cli.command {
        NodeCommandCli::Join(a) => join(&data_dir, a).await,
        NodeCommandCli::Run(e) => {
            let file = NodeFile::load(&data_dir)?;
            Agent::new(&data_dir, file, &e)?.run().await
        }
        NodeCommandCli::Status { json } => {
            let file = NodeFile::load(&data_dir)?;
            let view = serde_json::json!({
                "server": file.server,
                "node_id": file.node_id,
                "name": file.name,
                "set": file.set,
                "engine": file.engine.label(),
                "upstream": format!("{}:{}", file.upstream.host, file.upstream.port),
                "slot": file.upstream.slot,
            });
            if json {
                println!("{}", serde_json::to_string_pretty(&view)?);
            } else {
                println!(
                    "node {} ({}) of set {} on {}; streams from {}:{} (slot {}); engine {}",
                    file.name,
                    file.node_id,
                    file.set,
                    file.server,
                    file.upstream.host,
                    file.upstream.port,
                    file.upstream.slot,
                    file.engine.label()
                );
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

// ---------------------------------------------------------------------------
// node.json
// ---------------------------------------------------------------------------

const NODE_FILE: &str = "node.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct NodeFile {
    version: u32,
    server: String,
    node_id: String,
    name: String,
    set: String,
    agent_key: String,
    upstream: UpstreamGrant,
    engine: EngineIdentity,
    heartbeat_ms: u64,
    poll_wait_ms: u64,
    /// `primary` for a shard primary bootstrapped here (FB-174); absent
    /// (older files) or `replica` for a standby.
    #[serde(default)]
    role: Option<String>,
}

impl NodeFile {
    fn is_primary(&self) -> bool {
        self.role.as_deref() == Some("primary")
    }

    fn layout_kind(&self) -> NodeKind {
        if self.is_primary() {
            NodeKind::Primary
        } else {
            NodeKind::Standby
        }
    }

    fn path(data_dir: &Path) -> PathBuf {
        data_dir.join(NODE_FILE)
    }

    fn load(data_dir: &Path) -> anyhow::Result<Self> {
        let path = Self::path(data_dir);
        let text = std::fs::read_to_string(&path).with_context(|| {
            format!(
                "{} not found: join first with `forgebase node join <server url> --token … --data-dir {}`",
                path.display(),
                data_dir.display()
            )
        })?;
        serde_json::from_str(&text).with_context(|| format!("{} is not a node file", path.display()))
    }

    fn save(&self, data_dir: &Path) -> anyhow::Result<()> {
        let path = Self::path(data_dir);
        write_private(&path, serde_json::to_string_pretty(self)?.as_bytes())
    }
}

fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp).with_context(|| format!("create {}", tmp.display()))?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path).with_context(|| format!("write {}", path.display()))
}

fn private_dir(dir: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The server's agent endpoints over HTTP.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Server {
    /// `host:port` to dial.
    addr: String,
    /// Path prefix of the URL (usually empty).
    base: String,
}

impl Server {
    fn parse(url: &str) -> anyhow::Result<Self> {
        let rest = if let Some(r) = url.strip_prefix("http://") {
            r
        } else if url.starts_with("https://") {
            bail!(
                "this build's agent speaks plain HTTP only: HTTPS to the server comes with the engine TLS work \
                 (FB-153). Use http:// over a private network or a tunnel, never across the internet"
            );
        } else {
            bail!("the server URL must start with http:// (got {url:?})");
        };
        let (host, base) = rest.split_once('/').map_or((rest, ""), |(h, b)| (h, b));
        if host.is_empty() {
            bail!("the server URL has no host");
        }
        let addr = if host.rsplit_once(':').is_some_and(|(_, p)| p.parse::<u16>().is_ok()) {
            host.to_owned()
        } else {
            format!("{host}:80")
        };
        let base = base.trim_end_matches('/');
        Ok(Self {
            addr,
            base: if base.is_empty() {
                String::new()
            } else {
                format!("/{base}")
            },
        })
    }

    async fn call<T: DeserializeOwned>(
        &self,
        method: &str,
        path: &str,
        bearer: Option<&str>,
        body: Option<&impl Serialize>,
        timeout: Duration,
    ) -> Result<T, CallError> {
        let json = body
            .map(serde_json::to_vec)
            .transpose()
            .map_err(|e| CallError::Io(e.to_string()))?;
        let reply = probe::request(
            &self.addr,
            method,
            &format!("{}{path}", self.base),
            bearer,
            json.as_deref(),
            timeout,
        )
        .await
        .map_err(|e| CallError::Io(e.to_string()))?;
        if reply.status == 200 {
            serde_json::from_slice(&reply.body).map_err(|e| CallError::Io(format!("bad reply: {e}")))
        } else {
            Err(CallError::Status(
                reply.status,
                String::from_utf8_lossy(&reply.body).into_owned(),
            ))
        }
    }
}

#[derive(Debug)]
enum CallError {
    Io(String),
    Status(u16, String),
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CallError::Io(e) => write!(f, "{e}"),
            CallError::Status(s, body) => write!(f, "the server answered {s}: {body}"),
        }
    }
}

fn node_path(template: &str, id: &str) -> String {
    template.replace("{id}", id)
}

// ---------------------------------------------------------------------------
// join
// ---------------------------------------------------------------------------

fn default_name() -> String {
    let raw = std::process::Command::new("hostname")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_default();
    let mut name: String = raw
        .split('.')
        .next()
        .unwrap_or("")
        .chars()
        .map(|c| match c.to_ascii_lowercase() {
            c @ ('a'..='z' | '0'..='9' | '-' | '_') => c,
            _ => '-',
        })
        .take(32)
        .collect();
    name = name.trim_start_matches(['-', '_']).to_owned();
    if name.is_empty() || name == "primary" {
        "replica-1".to_owned()
    } else {
        name
    }
}

fn engine_config(args: &EngineArgs) -> anyhow::Result<EngineConfig> {
    let mut cfg = EngineConfig::default();
    if let Some(d) = &args.engine_dir {
        cfg.source = EngineSource::Dir(d.clone());
    }
    cfg.listen = args
        .listen
        .as_deref()
        .map(EngineConfig::parse_listen)
        .transpose()
        .context("--listen")?
        .flatten();
    cfg.remove_old_engines = false;
    Ok(cfg)
}

async fn join(data_dir: &Path, a: JoinArgs) -> anyhow::Result<ExitCode> {
    if NodeFile::path(data_dir).exists() {
        bail!(
            "{} already joined a server; resume it with `forgebase node run --data-dir {}`",
            data_dir.display(),
            data_dir.display()
        );
    }
    let server = Server::parse(&a.server)?;
    let name = a.name.clone().unwrap_or_else(default_name);
    private_dir(data_dir)?;
    let layout = Layout::node(data_dir, &name, NodeKind::Standby)?;
    let cfg = engine_config(&a.engine)?;
    let engine = standby::resolve_engine(&layout, &cfg).await?;
    let identity = EngineIdentity::of(&engine);
    eprintln!("joining {} as {name} (engine {})", a.server, identity.label());
    let req = JoinRequest {
        token: a.token.clone(),
        name: name.clone(),
        engine: identity,
        listen: a.engine.listen.clone(),
    };
    let grant: JoinGrant = server
        .call("POST", service::paths::JOIN, None, Some(&req), Duration::from_secs(60))
        .await
        .map_err(|e| anyhow::anyhow!("join refused: {e}"))?;
    let file = NodeFile {
        version: 1,
        server: a.server.clone(),
        node_id: grant.node_id.clone(),
        name: grant.name.clone(),
        set: grant.set_id.clone(),
        agent_key: grant.agent_key.clone(),
        upstream: grant.upstream.clone(),
        engine: grant.engine.clone(),
        heartbeat_ms: grant.heartbeat_ms,
        poll_wait_ms: grant.poll_wait_ms,
        role: Some(grant.role.as_str().to_owned()),
    };
    if file.is_primary() {
        // A shard primary (FB-174): the cluster's roles, so the server and
        // every other node log in with the same passwords.
        let Some(secrets) = grant.secrets.as_deref() else {
            bail!("the server sent no role passwords for this shard primary; upgrade the server");
        };
        let layout = Layout::node(data_dir, &grant.name, NodeKind::Primary)?;
        private_dir(layout.postgres_dir())?;
        write_private(&layout.secrets_file(), secrets.as_bytes())?;
    }
    file.save(data_dir)?;
    eprintln!("joined: node {} of set {}", file.node_id, file.set);
    Agent::new(data_dir, file, &a.engine)?.run().await
}

// ---------------------------------------------------------------------------
// The agent loop.
// ---------------------------------------------------------------------------

struct Agent {
    data_dir: PathBuf,
    file: NodeFile,
    server: Server,
    layout: Layout,
    cfg: EngineConfig,
    engine: Option<Arc<EngineHandle>>,
    state: AgentState,
    detail: Option<String>,
    results: Vec<CommandResult>,
}

enum Next {
    Continue,
    Exit,
}

impl Agent {
    fn new(data_dir: &Path, file: NodeFile, args: &EngineArgs) -> anyhow::Result<Self> {
        let server = Server::parse(&file.server)?;
        let layout = Layout::node(data_dir, &file.name, file.layout_kind())?;
        Ok(Self {
            data_dir: data_dir.to_path_buf(),
            server,
            layout,
            cfg: engine_config(args)?,
            engine: None,
            state: AgentState::Joining,
            detail: None,
            results: Vec::new(),
            file,
        })
    }

    /// `postgres/roles.secret` with the replicator's password only: the
    /// engine's readiness check on a standby logs in as the replicator.
    fn write_secrets(&self) -> anyhow::Result<()> {
        private_dir(self.layout.postgres_dir())?;
        let text = format!(
            "# ForgeBase node agent: the replicator's password only (FB-168). Never share or log this file.\n{}={}\n",
            Role::Replicator.name(),
            self.file.upstream.password
        );
        write_private(&self.layout.secrets_file(), text.as_bytes())
    }

    fn upstream(&self) -> standby::Upstream {
        self.file.upstream.upstream(&self.file.name)
    }

    async fn run(mut self) -> anyhow::Result<ExitCode> {
        if self.file.is_primary() {
            // initdb and bootstrap on the first start (roles.secret came with the join).
            self.start().await;
        } else {
            self.write_secrets()?;
            if !self.layout.pgdata().exists() {
                self.clone_primary().await;
            }
            if self.layout.pgdata().exists() {
                self.start().await;
            }
        }
        let (tx, mut rx) = mpsc::channel::<NodeCommand>(16);
        let poller = tokio::spawn(poll_loop(
            self.server.clone(),
            self.file.node_id.clone(),
            self.file.agent_key.clone(),
            self.file.poll_wait_ms,
            tx,
        ));
        let mut tick = tokio::time::interval(Duration::from_millis(self.file.heartbeat_ms.clamp(500, 60_000)));
        let code = loop {
            tokio::select! {
                _ = tick.tick() => {
                    if let Err(e) = self.heartbeat().await {
                        match e {
                            CallError::Status(401 | 404, _) => {
                                eprintln!("the server no longer knows this node ({e}); stopping");
                                break ExitCode::FAILURE;
                            }
                            e => eprintln!("heartbeat failed: {e}"),
                        }
                    }
                }
                cmd = rx.recv() => {
                    let Some(cmd) = cmd else {
                        eprintln!("the command long-poll ended; stopping");
                        break ExitCode::FAILURE;
                    };
                    match self.execute(cmd).await {
                        Next::Continue => {
                            let _ = self.heartbeat().await;
                        }
                        Next::Exit => break ExitCode::SUCCESS,
                    }
                }
                _ = tokio::signal::ctrl_c() => {
                    eprintln!("stopping");
                    break ExitCode::SUCCESS;
                }
            }
        };
        poller.abort();
        if let Some(h) = self.engine.take() {
            let _ = h.stop().await;
        }
        Ok(code)
    }

    async fn clone_primary(&mut self) {
        self.state = AgentState::Cloning;
        let _ = self.heartbeat().await;
        eprintln!(
            "cloning {}:{} (slot {})",
            self.file.upstream.host, self.file.upstream.port, self.file.upstream.slot
        );
        let r = async {
            let engine = standby::resolve_engine(&self.layout, &self.cfg).await?;
            standby::clone_from(&engine, &self.layout, &self.upstream(), CloneOptions::default()).await
        }
        .await;
        if let Err(e) = r {
            eprintln!("clone failed: {e}");
            self.state = AgentState::Failed;
            self.detail = Some(e.to_string());
        }
    }

    async fn start(&mut self) {
        self.state = AgentState::Starting;
        match Engine::prepare(&self.layout, &self.cfg).await {
            Ok(h) => {
                let what = if self.file.is_primary() {
                    "a shard primary"
                } else {
                    "a standby"
                };
                eprintln!("PostgreSQL {} is running as {what}", h.version().text);
                self.engine = Some(Arc::new(h));
                self.state = if self.file.is_primary() {
                    AgentState::Primary
                } else {
                    AgentState::CatchingUp
                };
                self.detail = None;
            }
            Err(e) => {
                eprintln!("the engine did not start: {e}");
                self.state = AgentState::Failed;
                self.detail = Some(e.to_string());
            }
        }
    }

    async fn stop_engine(&mut self) {
        if let Some(h) = self.engine.take() {
            let _ = h.stop().await;
        }
        self.state = AgentState::Stopped;
    }

    async fn observe(&mut self) -> Option<Lsn> {
        let h = self.engine.clone()?;
        if !matches!(h.health().state, forgebase_pgengine::EngineState::Ready) {
            self.state = AgentState::Starting;
            return None;
        }
        match standby::facts(&h).await {
            Ok(f) => {
                self.state = if !f.in_recovery {
                    AgentState::Primary
                } else if f.receiver.as_deref() == Some("streaming") {
                    AgentState::Streaming
                } else {
                    AgentState::CatchingUp
                };
                f.lsn
            }
            Err(e) => {
                self.detail = Some(e.to_string());
                None
            }
        }
    }

    async fn heartbeat(&mut self) -> Result<HeartbeatReply, CallError> {
        let replay_lsn = if matches!(
            self.state,
            AgentState::Cloning | AgentState::Rebuilding | AgentState::Failed | AgentState::Stopped
        ) {
            None
        } else {
            self.observe().await
        };
        let hb = Heartbeat {
            state: self.state,
            replay_lsn,
            disk_free: standby::disk_free(&self.data_dir),
            engine: self.file.engine.clone(),
            detail: self.detail.clone(),
            results: std::mem::take(&mut self.results),
        };
        let path = node_path(service::paths::HEARTBEAT, &self.file.node_id);
        let r = self
            .server
            .call(
                "POST",
                &path,
                Some(&self.file.agent_key),
                Some(&hb),
                Duration::from_secs(20),
            )
            .await;
        if r.is_err() {
            // Answers are sent again with the next heartbeat.
            self.results = hb.results;
        }
        r
    }

    fn answer(&mut self, id: &str, r: anyhow::Result<String>) {
        let (ok, message) = match r {
            Ok(m) => (true, m),
            Err(e) => (false, format!("{e:#}")),
        };
        eprintln!("command {id}: {}", if ok { "done" } else { "failed" });
        self.results.push(CommandResult {
            id: id.to_owned(),
            ok,
            message,
        });
    }

    async fn execute(&mut self, cmd: NodeCommand) -> Next {
        eprintln!("command {} {}", cmd.id, cmd.kind.as_str());
        if self.file.is_primary() && cmd.kind != CommandKind::Stop {
            self.answer(
                &cmd.id,
                Err(anyhow::anyhow!(
                    "{} is a shard primary; only stop applies",
                    self.file.name
                )),
            );
            return Next::Continue;
        }
        match cmd.kind {
            CommandKind::Stop => {
                let remove = cmd.args.get("remove").and_then(serde_json::Value::as_bool) == Some(true);
                self.stop_engine().await;
                if remove {
                    let r = self.remove_data();
                    self.answer(&cmd.id, r.map(|()| "stopped and removed".to_owned()));
                    let _ = self.heartbeat().await;
                    return Next::Exit;
                }
                self.answer(&cmd.id, Ok("stopped".into()));
            }
            CommandKind::Rebuild => {
                let r = self.rebuild().await;
                self.answer(&cmd.id, r.map(|()| "rebuilt".to_owned()));
            }
            CommandKind::Promote => {
                let r = self.promote(&cmd.args).await;
                self.answer(&cmd.id, r);
            }
            CommandKind::Follow => {
                let r = self.follow(&cmd.args).await;
                self.answer(&cmd.id, r.map(|()| "following".to_owned()));
            }
            CommandKind::Rejoin => {
                let r = self.rejoin(&cmd.args).await;
                self.answer(&cmd.id, r);
            }
        }
        Next::Continue
    }

    fn remove_data(&self) -> anyhow::Result<()> {
        let dir = self.layout.node_dir();
        if dir.exists() {
            std::fs::remove_dir_all(&dir).with_context(|| format!("remove {}", dir.display()))?;
        }
        let _ = std::fs::remove_file(self.layout.secrets_file());
        let _ = std::fs::remove_file(NodeFile::path(&self.data_dir));
        Ok(())
    }

    /// The slot on the primary again (an invalidated one cannot be reused):
    /// the replicator has `REPLICATION`, which is all slot functions need.
    async fn recreate_slot(&self) -> anyhow::Result<()> {
        let mut c = replication::connect(&self.upstream().url("postgres")).await?;
        replication::drop_slot(&mut c, &self.file.upstream.slot).await?;
        replication::ensure_slot(&mut c, &self.file.upstream.slot).await?;
        Ok(())
    }

    async fn rebuild(&mut self) -> anyhow::Result<()> {
        self.stop_engine().await;
        self.state = AgentState::Rebuilding;
        let _ = self.heartbeat().await;
        let pgdata = self.layout.pgdata();
        if pgdata.exists() {
            std::fs::remove_dir_all(&pgdata).with_context(|| format!("remove {}", pgdata.display()))?;
        }
        self.recreate_slot().await?;
        self.clone_primary().await;
        if self.state == AgentState::Failed {
            bail!("{}", self.detail.clone().unwrap_or_default());
        }
        self.start().await;
        if self.state == AgentState::Failed {
            bail!("{}", self.detail.clone().unwrap_or_default());
        }
        Ok(())
    }

    async fn promote(&mut self, args: &serde_json::Value) -> anyhow::Result<String> {
        let h = self.engine.clone().context("the engine is not running")?;
        self.state = AgentState::Promoting;
        if let Some(min) = args.get("min_lsn").and_then(serde_json::Value::as_str) {
            let lsn: Lsn = min.parse()?;
            standby::wait_replayed(&h, lsn, Duration::from_secs(60)).await?;
        }
        standby::promote(&h, Duration::from_secs(60)).await?;
        let slots: Vec<String> = args
            .get("slots")
            .and_then(serde_json::Value::as_array)
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
            .unwrap_or_default();
        if !slots.is_empty() {
            let mut c = replication::connect(&standby::standby_url(&h)).await?;
            for s in &slots {
                replication::ensure_slot(&mut c, s).await?;
            }
        }
        let f = standby::facts(&h).await?;
        self.state = AgentState::Primary;
        Ok(serde_json::json!({ "lsn": f.lsn, "timeline": f.timeline }).to_string())
    }

    /// `upstream` (a whole new login) or `upstream_change` (host, port and
    /// slot of a promoted clone: same roles, same login).
    fn new_upstream(&mut self, args: &serde_json::Value) -> anyhow::Result<()> {
        if let Some(change) = args.get("upstream_change") {
            let c: service::UpstreamGrantArgs = serde_json::from_value(change.clone())?;
            self.file.upstream.host = c.host;
            self.file.upstream.port = c.port;
            self.file.upstream.slot = c.slot;
        } else {
            let up: UpstreamGrant =
                serde_json::from_value(args.get("upstream").cloned().context("the command has no upstream")?)?;
            self.file.upstream = up;
        }
        self.file.save(&self.data_dir)?;
        self.write_secrets()
    }

    async fn follow(&mut self, args: &serde_json::Value) -> anyhow::Result<()> {
        self.new_upstream(args)?;
        let h = self.engine.clone().context("the engine is not running")?;
        standby::refollow(&h, &self.upstream()).await?;
        Ok(())
    }

    async fn rejoin(&mut self, args: &serde_json::Value) -> anyhow::Result<String> {
        self.new_upstream(args)?;
        self.stop_engine().await;
        self.state = AgentState::Rebuilding;
        let _ = self.heartbeat().await;
        let engine = standby::resolve_engine(&self.layout, &self.cfg).await?;
        let up = self.upstream();
        let how = standby::rejoin(&engine, &self.layout, &up, Some(&up), CloneOptions::default()).await?;
        self.start().await;
        if self.state == AgentState::Failed {
            bail!("{}", self.detail.clone().unwrap_or_default());
        }
        Ok(serde_json::to_string(&how)?)
    }
}

/// The outbound long-poll: commands go to the agent loop in order.
async fn poll_loop(server: Server, node_id: String, key: String, wait_ms: u64, tx: mpsc::Sender<NodeCommand>) {
    let wait = wait_ms.clamp(1_000, 30_000);
    let path = format!("{}?wait_ms={wait}", node_path(service::paths::COMMANDS, &node_id));
    let mut backoff = Duration::from_secs(1);
    let mut first = true;
    loop {
        let path = if first {
            format!("{path}&resend=1")
        } else {
            path.clone()
        };
        let r: Result<CommandBatch, CallError> = server
            .call(
                "GET",
                &path,
                Some(&key),
                None::<&()>,
                Duration::from_millis(wait + 15_000),
            )
            .await;
        match r {
            Ok(batch) => {
                first = false;
                backoff = Duration::from_secs(1);
                for c in batch.commands {
                    if tx.send(c).await.is_err() {
                        return;
                    }
                }
            }
            Err(CallError::Status(401 | 404, _)) => return,
            Err(e) => {
                eprintln!("command poll failed: {e}; retrying in {backoff:?}");
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(30));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_urls() {
        let s = Server::parse("http://10.0.0.5:8090").expect("parse");
        assert_eq!((s.addr.as_str(), s.base.as_str()), ("10.0.0.5:8090", ""));
        let s = Server::parse("http://fb.internal/base/").expect("parse");
        assert_eq!((s.addr.as_str(), s.base.as_str()), ("fb.internal:80", "/base"));
        assert!(Server::parse("https://x").is_err());
        assert!(Server::parse("ftp://x").is_err());
        assert!(Server::parse("http://").is_err());
        assert_eq!(
            node_path(service::paths::HEARTBEAT, "n1"),
            "/api/system/database/agent/nodes/n1/heartbeat"
        );
    }

    #[test]
    fn default_name_is_a_valid_node_name() {
        let n = default_name();
        assert!(Layout::node("/d", &n, NodeKind::Standby).is_ok(), "{n:?}");
    }
}
