//! `forgebase functions deploy|logs`: manage WebAssembly functions of a running
//! server over its REST API (`/api/v1/functions`).
//!
//! The server is `--server URL` (env `FB_URL`), or the configured bind address
//! (`--host`/`--port`/config) when omitted. Credentials are an API key or
//! personal access token in `--token` (env `FB_TOKEN`), sent as
//! `Authorization: Bearer`. Only plain `http://` is spoken (the CLI carries no
//! TLS stack); put a TLS-terminating proxy or tunnel in front for remote servers.
//! (The variables avoid the `FORGEBASE_` prefix: the config loader rejects
//! unknown variables of that prefix.)

use std::collections::BTreeMap;
use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, bail};
use base64::Engine as _;
use clap::{Args, Subcommand, ValueEnum};
use serde_json::{Value, json};

use crate::GlobalArgs;
use crate::probe;

const TIMEOUT: Duration = Duration::from_secs(60);
/// Largest module accepted by the server (request body limit is generous; refuse the absurd early).
const MAX_WASM: u64 = 64 << 20;
const FOLLOW_BATCH: u32 = 200;

/// Where and how to reach the server.
#[derive(Debug, Clone, Default, Args)]
pub struct ServerArgs {
    /// Server base URL, e.g. http://127.0.0.1:8090 [default: the configured bind address] [env: FB_URL]
    #[arg(long, global = true, value_name = "URL")]
    pub server: Option<String>,
    /// API key (fbk_...) or personal access token (fbp_...) [env: FB_TOKEN]
    #[arg(long, global = true, value_name = "TOKEN")]
    pub token: Option<String>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Access {
    /// Anyone, including anonymous callers.
    Public,
    /// Any authenticated caller.
    Authenticated,
    /// Callers allowed `engine:invoke_function`.
    Permission,
}

impl Access {
    fn as_str(self) -> &'static str {
        match self {
            Access::Public => "public",
            Access::Authenticated => "authenticated",
            Access::Permission => "permission",
        }
    }
}

/// Arguments of `forgebase functions`.
#[derive(Debug, Args)]
pub struct FunctionsCli {
    #[command(flatten)]
    pub server: ServerArgs,
    #[command(subcommand)]
    pub command: FunctionsCommand,
}

#[derive(Debug, Subcommand)]
pub enum FunctionsCommand {
    /// Upload a WebAssembly module as a new version of a function (creates it on the first deploy).
    Deploy(DeployArgs),
    /// Print a function's recent executions and their log lines.
    Logs(LogsArgs),
}

#[derive(Debug, Args)]
pub struct DeployArgs {
    /// Function name: a lower-case letter, then lower-case letters, digits, `_` or `-` (at most 48).
    pub name: String,
    /// The compiled `.wasm` module (binary format).
    pub path: PathBuf,
    /// Description (kept from the previous deploy when omitted).
    #[arg(long)]
    pub description: Option<String>,
    /// Who may invoke over HTTP (kept from the previous deploy when omitted; `permission` on create).
    #[arg(long, value_enum)]
    pub access: Option<Access>,
    /// Grant a capability: db.read, db.write, kv, log, env, secrets, fetch. Repeatable.
    #[arg(long = "capability", value_name = "NAME")]
    pub capabilities: Vec<String>,
    /// With `fetch`: a host the function may call (`api.example.com`, `*.example.com`). Repeatable.
    #[arg(long = "fetch-host", value_name = "HOST")]
    pub fetch_hosts: Vec<String>,
    /// Plain environment value `KEY=VALUE`. Repeatable. Use the dashboard or API for secrets.
    #[arg(long = "env", value_name = "KEY=VALUE")]
    pub env: Vec<String>,
    /// Run on record events: `collection:event[,event...]`, e.g. `orders:after_create,after_update`. Repeatable.
    #[arg(long = "hook", value_name = "COLLECTION:EVENTS")]
    pub hooks: Vec<String>,
    /// Fuel per execution.
    #[arg(long)]
    pub fuel: Option<u64>,
    /// Largest linear memory in bytes.
    #[arg(long)]
    pub memory_bytes: Option<u64>,
    /// Wall-clock limit per execution in milliseconds.
    #[arg(long)]
    pub timeout_ms: Option<u64>,
    /// Concurrent executions of this function.
    #[arg(long)]
    pub max_concurrency: Option<u32>,
    /// Deploy the version without making it the active one.
    #[arg(long)]
    pub no_activate: bool,
    /// Print the server's answer as JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct LogsArgs {
    /// Function name.
    pub name: String,
    /// How many of the newest executions to show (1-500).
    #[arg(short = 'n', long, default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..=500))]
    pub limit: u32,
    /// Keep running and print new executions as they arrive (Ctrl-C to stop).
    #[arg(short, long)]
    pub follow: bool,
    /// Seconds between polls with --follow.
    #[arg(long, default_value_t = 2.0, value_name = "SECONDS")]
    pub interval: f64,
    /// One JSON object per execution (NDJSON).
    #[arg(long)]
    pub json: bool,
}

/// A connection target and credential.
struct Client {
    addr: String,
    token: Option<String>,
}

impl Client {
    fn new(g: &GlobalArgs, s: &ServerArgs) -> anyhow::Result<Self> {
        let url = s.server.clone().or_else(|| std::env::var("FB_URL").ok());
        let addr = match url.as_deref().map(str::trim).filter(|u| !u.is_empty()) {
            Some(url) => addr_of(url)?,
            None => {
                let c = g.load()?.config;
                probe::dial_addr(&c.server.host, c.server.port)
            }
        };
        let token = s
            .token
            .clone()
            .or_else(|| std::env::var("FB_TOKEN").ok())
            .filter(|t| !t.trim().is_empty());
        Ok(Self { addr, token })
    }

    /// Sends a request and returns the parsed JSON body (`Null` for an empty one); non-2xx becomes an error carrying the server's message.
    async fn call(&self, method: &str, path: &str, body: Option<&Value>) -> anyhow::Result<Value> {
        let raw = body.map(serde_json::to_vec).transpose()?;
        let reply = probe::request(&self.addr, method, path, self.token.as_deref(), raw.as_deref(), TIMEOUT)
            .await
            .with_context(|| {
                format!(
                    "cannot reach the server at {} (is `forgebase serve` running?)",
                    self.addr
                )
            })?;
        let parsed: Value = if reply.body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&reply.body).unwrap_or(Value::Null)
        };
        if (200..300).contains(&reply.status) {
            return Ok(parsed);
        }
        let err = &parsed["error"];
        let code = err["code"].as_str().unwrap_or("error");
        let message = err["message"].as_str().unwrap_or("the server refused the request");
        let hint = match reply.status {
            401 => " (pass --token or set FB_TOKEN to an API key or personal access token)",
            403 => " (the credential lacks manage_functions / read_functions)",
            _ => "",
        };
        bail!("{code}: {message} (HTTP {}){hint}", reply.status)
    }
}

/// `http://host:port[/]` to a dial address; anything else is refused with a reason.
fn addr_of(url: &str) -> anyhow::Result<String> {
    let rest = if let Some(r) = url.strip_prefix("http://") {
        r
    } else if url.starts_with("https://") {
        bail!("https:// is not supported by the CLI (no TLS stack): use http:// to a local or tunnelled port")
    } else {
        url
    };
    let host = rest.trim_end_matches('/');
    if host.is_empty() || host.contains('/') || host.contains('@') {
        bail!("--server must look like http://host:port (no path or credentials), got {url:?}");
    }
    Ok(if host.starts_with('[') {
        if host.contains("]:") {
            host.to_owned()
        } else {
            format!("{host}:80")
        }
    } else {
        match host.matches(':').count() {
            0 => format!("{host}:80"),
            1 => host.to_owned(),
            _ => format!("[{host}]:80"), // a bare IPv6 address
        }
    })
}

/// Runs a `functions` subcommand.
pub async fn run(g: &GlobalArgs, server: ServerArgs, cmd: FunctionsCommand) -> anyhow::Result<ExitCode> {
    let client = Client::new(g, &server)?;
    match cmd {
        FunctionsCommand::Deploy(a) => deploy(&client, a, &mut std::io::stdout()).await?,
        FunctionsCommand::Logs(a) => {
            let stop = async {
                let _ = tokio::signal::ctrl_c().await;
            };
            logs(&client, a, &mut std::io::stdout(), stop).await?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn enc(name: &str) -> String {
    name.bytes()
        .map(|b| match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn module_bytes(path: &Path) -> anyhow::Result<Vec<u8>> {
    let meta = std::fs::metadata(path).with_context(|| format!("cannot read {}", path.display()))?;
    if !meta.is_file() {
        bail!("{} is not a file", path.display());
    }
    if meta.len() > MAX_WASM {
        bail!(
            "{} is {} bytes; the CLI refuses modules over {MAX_WASM}",
            path.display(),
            meta.len()
        );
    }
    let bytes = std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    if !bytes.starts_with(b"\0asm") {
        bail!(
            "{} is not a WebAssembly binary (it must start with the \\0asm magic); compile it first, e.g. to .wasm",
            path.display()
        );
    }
    Ok(bytes)
}

fn parse_env(items: &[String]) -> anyhow::Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for item in items {
        match item.split_once('=') {
            Some((k, v)) if !k.trim().is_empty() => {
                out.insert(k.trim().to_owned(), v.to_owned());
            }
            _ => bail!("--env expects KEY=VALUE, got {item:?}"),
        }
    }
    Ok(out)
}

fn parse_hooks(items: &[String]) -> anyhow::Result<Vec<Value>> {
    items
        .iter()
        .map(|item| {
            let (collection, events) = item
                .split_once(':')
                .with_context(|| format!("--hook expects COLLECTION:EVENT[,EVENT...], got {item:?}"))?;
            let events: Vec<&str> = events.split(',').map(str::trim).filter(|e| !e.is_empty()).collect();
            if collection.trim().is_empty() || events.is_empty() {
                bail!("--hook expects COLLECTION:EVENT[,EVENT...], got {item:?}");
            }
            Ok(json!({ "collection": collection.trim(), "events": events }))
        })
        .collect()
}

/// The `POST /api/v1/functions` body for the arguments.
fn deploy_body(a: &DeployArgs, wasm: &[u8]) -> anyhow::Result<Value> {
    let mut limits = serde_json::Map::new();
    if let Some(v) = a.fuel {
        limits.insert("fuel".into(), v.into());
    }
    if let Some(v) = a.memory_bytes {
        limits.insert("memory_bytes".into(), v.into());
    }
    if let Some(v) = a.timeout_ms {
        limits.insert("timeout_ms".into(), v.into());
    }
    if let Some(v) = a.max_concurrency {
        limits.insert("max_concurrency".into(), v.into());
    }
    let mut body = json!({
        "name": a.name,
        "wasm": base64::engine::general_purpose::STANDARD.encode(wasm),
        "capabilities": a.capabilities,
        "fetch_hosts": a.fetch_hosts,
        "limits": limits,
        "env": parse_env(&a.env)?,
        "hooks": parse_hooks(&a.hooks)?,
        "activate": !a.no_activate,
    });
    if let Some(d) = &a.description {
        body["description"] = json!(d);
    }
    if let Some(x) = a.access {
        body["access"] = json!(x.as_str());
    }
    Ok(body)
}

async fn deploy(client: &Client, a: DeployArgs, out: &mut impl Write) -> anyhow::Result<()> {
    let wasm = module_bytes(&a.path)?;
    let body = deploy_body(&a, &wasm)?;
    let done = client.call("POST", "/api/v1/functions", Some(&body)).await?;
    if a.json {
        writeln!(out, "{done}")?;
        return Ok(());
    }
    let v = &done["version"];
    let active = done["function"]["active_version"].as_i64().unwrap_or(0);
    writeln!(
        out,
        "deployed {} v{} ({} bytes), {}",
        done["function"]["name"].as_str().unwrap_or(&a.name),
        v["version"],
        v["size"],
        if v["active"].as_bool() == Some(true) {
            "active".to_owned()
        } else {
            format!("not active (v{active} still serves)")
        }
    )?;
    Ok(())
}

fn print_execution(out: &mut impl Write, e: &Value, json: bool) -> anyhow::Result<()> {
    if json {
        writeln!(out, "{e}")?;
        return Ok(());
    }
    writeln!(
        out,
        "{}  {:<14} v{} {} {}ms {} fuel{}",
        e["created_at"].as_str().unwrap_or("?"),
        e["status"].as_str().unwrap_or("?"),
        e["version"],
        e["trigger"].as_str().unwrap_or("?"),
        e["duration_ms"],
        e["fuel_used"],
        e["request_id"]
            .as_str()
            .map(|r| format!("  request {r}"))
            .unwrap_or_default()
    )?;
    if let Some(err) = e["error"].as_str() {
        writeln!(out, "    error: {err}")?;
    }
    for l in e["logs"].as_array().into_iter().flatten() {
        writeln!(
            out,
            "    [{}] {}",
            l["level"].as_str().unwrap_or("?"),
            l["message"].as_str().unwrap_or("")
        )?;
    }
    Ok(())
}

async fn fetch_logs(client: &Client, name: &str, limit: u32) -> anyhow::Result<Vec<Value>> {
    let v = client
        .call(
            "GET",
            &format!("/api/v1/functions/{}/logs?limit={limit}", enc(name)),
            None,
        )
        .await?;
    // Newest first on the wire; print oldest first so the tail reads like a log.
    let mut items = v["items"].as_array().cloned().unwrap_or_default();
    items.reverse();
    Ok(items)
}

/// Prints the newest `limit` executions; with `--follow` keeps polling until `stop` resolves.
async fn logs(
    client: &Client,
    a: LogsArgs,
    out: &mut impl Write,
    stop: impl Future<Output = ()>,
) -> anyhow::Result<()> {
    let mut last = 0_i64;
    for e in fetch_logs(client, &a.name, a.limit).await? {
        last = last.max(e["id"].as_i64().unwrap_or(0));
        print_execution(out, &e, a.json)?;
    }
    out.flush()?;
    if !a.follow {
        return Ok(());
    }
    if !(a.interval.is_finite() && a.interval >= 0.05) {
        bail!("--interval must be at least 0.05 seconds");
    }
    let pause = Duration::from_secs_f64(a.interval);
    tokio::pin!(stop);
    loop {
        tokio::select! {
            () = &mut stop => return Ok(()),
            () = tokio::time::sleep(pause) => {}
        }
        for e in fetch_logs(client, &a.name, FOLLOW_BATCH).await? {
            let id = e["id"].as_i64().unwrap_or(0);
            if id > last {
                last = id;
                print_execution(out, &e, a.json)?;
            }
        }
        out.flush()?;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    /// A request as the fake server saw it.
    #[derive(Debug, Clone)]
    struct Seen {
        request_line: String,
        headers: String,
        body: String,
    }

    /// Serves `replies` in order (the last repeats) and records each request.
    async fn fake_server(replies: Vec<(u16, String)>) -> (String, Arc<Mutex<Vec<Seen>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            let mut n = 0;
            loop {
                let Ok((mut s, _)) = listener.accept().await else {
                    return;
                };
                let mut buf = Vec::new();
                let mut tmp = [0_u8; 4096];
                loop {
                    let k = s.read(&mut tmp).await.unwrap();
                    buf.extend_from_slice(&tmp[..k]);
                    let text = String::from_utf8_lossy(&buf).into_owned();
                    if let Some((head, body)) = text.split_once("\r\n\r\n") {
                        let want = head
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if body.len() >= want {
                            let mut lines = head.lines();
                            log.lock().unwrap().push(Seen {
                                request_line: lines.next().unwrap().to_owned(),
                                headers: lines.collect::<Vec<_>>().join("\n"),
                                body: body.to_owned(),
                            });
                            break;
                        }
                    }
                    if k == 0 {
                        break;
                    }
                }
                let (status, body) = replies.get(n).or(replies.last()).cloned().unwrap();
                n += 1;
                let head = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = s.write_all(head.as_bytes()).await;
                let _ = s.write_all(body.as_bytes()).await;
            }
        });
        (addr, seen)
    }

    fn client(addr: &str, token: Option<&str>) -> Client {
        Client {
            addr: addr.to_owned(),
            token: token.map(str::to_owned),
        }
    }

    fn deploy_args(name: &str, path: &Path) -> DeployArgs {
        DeployArgs {
            name: name.into(),
            path: path.into(),
            description: None,
            access: None,
            capabilities: vec![],
            fetch_hosts: vec![],
            env: vec![],
            hooks: vec![],
            fuel: None,
            memory_bytes: None,
            timeout_ms: None,
            max_concurrency: None,
            no_activate: false,
            json: false,
        }
    }

    fn wasm_file(dir: &Path) -> PathBuf {
        let p = dir.join("m.wasm");
        std::fs::write(&p, b"\0asm\x01\0\0\0").unwrap();
        p
    }

    #[test]
    fn server_urls_become_dial_addresses() {
        assert_eq!(addr_of("http://127.0.0.1:8090").unwrap(), "127.0.0.1:8090");
        assert_eq!(addr_of("http://localhost:1/").unwrap(), "localhost:1");
        assert_eq!(addr_of("example.com").unwrap(), "example.com:80");
        assert_eq!(addr_of("http://[::1]:9").unwrap(), "[::1]:9");
        assert_eq!(addr_of("http://[::1]").unwrap(), "[::1]:80");
        assert!(addr_of("https://example.com").unwrap_err().to_string().contains("TLS"));
        assert!(addr_of("http://host:1/api").is_err());
        assert!(addr_of("http://u:p@host:1").is_err());
        assert!(addr_of("http://").is_err());
    }

    #[test]
    fn deploy_body_carries_every_option() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = deploy_args("hello", &wasm_file(dir.path()));
        a.description = Some("d".into());
        a.access = Some(Access::Public);
        a.capabilities = vec!["log".into(), "env".into()];
        a.fetch_hosts = vec!["*.example.com".into()];
        a.env = vec!["A=1".into(), "B=x=y".into()];
        a.hooks = vec!["orders:after_create, after_update".into()];
        a.fuel = Some(5);
        a.timeout_ms = Some(900);
        a.no_activate = true;
        let b = deploy_body(&a, b"\0asm\x01\0\0\0").unwrap();
        assert_eq!(b["wasm"], "AGFzbQEAAAA=");
        assert_eq!(b["name"], "hello");
        assert_eq!(b["description"], "d");
        assert_eq!(b["access"], "public");
        assert_eq!(b["capabilities"], json!(["log", "env"]));
        assert_eq!(b["fetch_hosts"], json!(["*.example.com"]));
        assert_eq!(b["env"], json!({"A": "1", "B": "x=y"}));
        assert_eq!(
            b["hooks"],
            json!([{"collection": "orders", "events": ["after_create", "after_update"]}])
        );
        assert_eq!(b["limits"], json!({"fuel": 5, "timeout_ms": 900}));
        assert_eq!(b["activate"], false);
        // Omitted optional fields are not sent, so the server keeps the previous values.
        let plain = deploy_body(&deploy_args("hello", dir.path()), b"x").unwrap();
        assert!(plain.get("description").is_none() && plain.get("access").is_none());
        assert_eq!(plain["activate"], true);
    }

    #[test]
    fn bad_options_and_files_are_refused_before_any_request() {
        let dir = tempfile::tempdir().unwrap();
        assert!(parse_env(&["noequals".into()]).is_err());
        assert!(parse_env(&["=v".into()]).is_err());
        assert!(parse_hooks(&["orders".into()]).is_err());
        assert!(parse_hooks(&["orders:".into()]).is_err());
        assert!(parse_hooks(&[":after_create".into()]).is_err());
        assert!(module_bytes(&dir.path().join("missing.wasm")).is_err());
        assert!(module_bytes(dir.path()).unwrap_err().to_string().contains("not a file"));
        let text = dir.path().join("a.txt");
        std::fs::write(&text, "(module)").unwrap();
        assert!(
            module_bytes(&text)
                .unwrap_err()
                .to_string()
                .contains("not a WebAssembly binary")
        );
    }

    #[tokio::test]
    async fn deploy_posts_the_module_with_the_bearer_token_and_reports_the_version() {
        let dir = tempfile::tempdir().unwrap();
        let answer = json!({
            "function": {"name": "hello", "active_version": 3, "latest_version": 3},
            "version": {"version": 3, "size": 8, "active": true}
        });
        let (addr, seen) = fake_server(vec![(201, answer.to_string())]).await;
        let mut out = Vec::new();
        deploy(
            &client(&addr, Some("fbk_secret")),
            deploy_args("hello", &wasm_file(dir.path())),
            &mut out,
        )
        .await
        .unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "deployed hello v3 (8 bytes), active\n");
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0].request_line, "POST /api/v1/functions HTTP/1.1");
        assert!(seen[0].headers.contains("Authorization: Bearer fbk_secret"));
        assert!(seen[0].headers.contains("Content-Type: application/json"));
        let body: Value = serde_json::from_str(&seen[0].body).unwrap();
        assert_eq!(body["wasm"], "AGFzbQEAAAA=");
    }

    #[tokio::test]
    async fn deploy_without_activation_says_which_version_still_serves() {
        let dir = tempfile::tempdir().unwrap();
        let answer = json!({"function": {"name": "hello", "active_version": 2}, "version": {"version": 3, "size": 8, "active": false}});
        let (addr, _) = fake_server(vec![(201, answer.to_string())]).await;
        let mut a = deploy_args("hello", &wasm_file(dir.path()));
        a.json = false;
        let mut out = Vec::new();
        deploy(&client(&addr, None), a, &mut out).await.unwrap();
        assert!(String::from_utf8(out).unwrap().contains("not active (v2 still serves)"));
    }

    #[tokio::test]
    async fn server_errors_carry_code_message_and_a_hint() {
        let dir = tempfile::tempdir().unwrap();
        let body = json!({"error": {"code": "invalid_module", "message": "bad wasm", "request_id": "r"}});
        let (addr, _) = fake_server(vec![(400, body.to_string())]).await;
        let err = deploy(
            &client(&addr, None),
            deploy_args("hello", &wasm_file(dir.path())),
            &mut Vec::new(),
        )
        .await
        .unwrap_err()
        .to_string();
        assert_eq!(err, "invalid_module: bad wasm (HTTP 400)");

        let body = json!({"error": {"code": "unauthenticated", "message": "authentication required"}});
        let (addr, _) = fake_server(vec![(401, body.to_string())]).await;
        let err = logs_once(&addr).await.unwrap_err().to_string();
        assert!(
            err.contains("unauthenticated: authentication required (HTTP 401)") && err.contains("--token"),
            "{err}"
        );
    }

    async fn logs_once(addr: &str) -> anyhow::Result<String> {
        let mut out = Vec::new();
        logs(
            &client(addr, None),
            logs_args("hello", false),
            &mut out,
            std::future::pending(),
        )
        .await?;
        Ok(String::from_utf8(out)?)
    }

    fn logs_args(name: &str, follow: bool) -> LogsArgs {
        LogsArgs {
            name: name.into(),
            limit: 50,
            follow,
            interval: 0.05,
            json: false,
        }
    }

    fn exec(id: i64, status: &str, msg: Option<&str>, error: Option<&str>) -> Value {
        json!({
            "id": id, "function": "hello", "version": 2, "trigger": "http", "status": status, "duration_ms": 3, "fuel_used": 20,
            "error": error, "logs": msg.map(|m| vec![json!({"level": "info", "message": m})]).unwrap_or_default(),
            "request_id": format!("r{id}"), "created_at": "2026-11-09T10:00:00Z"
        })
    }

    #[tokio::test]
    async fn logs_print_oldest_first_with_lines_and_errors() {
        let items =
            json!({"items": [exec(2, "trap", None, Some("unreachable")), exec(1, "ok", Some("hi there"), None)]});
        let (addr, seen) = fake_server(vec![(200, items.to_string())]).await;
        let out = logs_once(&addr).await.unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert!(
            lines[0].contains("ok") && lines[0].contains("v2 http 3ms 20 fuel") && lines[0].contains("request r1"),
            "{out}"
        );
        assert_eq!(lines[1], "    [info] hi there");
        assert!(lines[2].contains("trap"));
        assert_eq!(lines[3], "    error: unreachable");
        assert_eq!(
            seen.lock().unwrap()[0].request_line,
            "GET /api/v1/functions/hello/logs?limit=50 HTTP/1.1"
        );
    }

    #[tokio::test]
    async fn follow_prints_only_new_executions_and_stops_on_signal() {
        let first = json!({"items": [exec(2, "ok", None, None), exec(1, "ok", None, None)]}).to_string();
        let second = json!({"items": [exec(3, "failed", None, Some("boom")), exec(2, "ok", None, None), exec(1, "ok", None, None)]}).to_string();
        let (addr, seen) = fake_server(vec![(200, first), (200, second)]).await;
        let mut a = logs_args("hello", true);
        a.json = true;
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let watch = seen.clone();
        tokio::spawn(async move {
            while watch.lock().unwrap().len() < 3 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            let _ = tx.send(());
        });
        let mut out = Vec::new();
        logs(&client(&addr, None), a, &mut out, async {
            let _ = rx.await;
        })
        .await
        .unwrap();
        let ids: Vec<i64> = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap()["id"].as_i64().unwrap())
            .collect();
        // Initial page (1, 2), then only the new 3; the repeated poll adds nothing.
        assert_eq!(ids, vec![1, 2, 3]);
        assert!(seen.lock().unwrap()[1].request_line.contains("limit=200"));
    }

    #[tokio::test]
    async fn an_unreachable_server_gets_a_helpful_error() {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap().to_string();
        drop(l);
        let err = format!("{:#}", logs_once(&addr).await.unwrap_err());
        assert!(
            err.contains("cannot reach the server") && err.contains("forgebase serve"),
            "{err}"
        );
    }

    #[test]
    fn names_are_percent_encoded_in_paths() {
        assert_eq!(enc("hello_world-1"), "hello_world-1");
        assert_eq!(enc("a/b c"), "a%2Fb%20c");
    }
}
