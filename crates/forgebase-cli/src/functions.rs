//! `forgebase functions new|deploy|logs|invoke`: scaffold JavaScript/TypeScript
//! functions and manage functions (TypeScript, JavaScript or WebAssembly) of
//! a running server over its REST API (`/api/v1/functions`).
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
use std::time::{Duration, Instant};

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
    /// Write `functions/<name>/index.ts` (or `index.js` with --js) and `forgebase.d.ts` from a template.
    New(NewArgs),
    /// Deploy a `.ts`, `.js` or `.wasm` file as a new version of a function (creates it on the first deploy).
    Deploy(DeployArgs),
    /// Print a function's recent executions and their log lines.
    Logs(LogsArgs),
    /// Call an HTTP function and show the result, status, duration, log lines and trace id.
    Invoke(InvokeArgs),
}

#[derive(Debug, Args)]
pub struct NewArgs {
    /// Function name: a lower-case letter, then lower-case letters, digits, `_` or `-` (at most 48).
    pub name: String,
    /// JavaScript instead of TypeScript.
    #[arg(long)]
    pub js: bool,
    /// Directory that holds the functions.
    #[arg(long, default_value = "functions", value_name = "DIR")]
    pub dir: PathBuf,
    /// Overwrite existing files.
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct DeployArgs {
    /// Function name: a lower-case letter, then lower-case letters, digits, `_` or `-` (at most 48).
    pub name: String,
    /// The function: TypeScript (`.ts`, `.mts`), JavaScript (`.js`, `.mjs`) or a WebAssembly module (`.wasm`).
    pub path: PathBuf,
    /// Description (kept from the previous deploy when omitted).
    #[arg(long)]
    pub description: Option<String>,
    /// Who may invoke over HTTP (kept from the previous deploy when omitted; `permission` on create).
    #[arg(long, value_enum)]
    pub access: Option<Access>,
    /// Grant a capability: db.read, db.write, kv, log, env, secrets, fetch, vector.read, vector.write,
    /// storage.read, storage.write, queues.publish, jobs.enqueue, realtime.publish, service_role (administrators). Repeatable.
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

#[derive(Debug, Args)]
pub struct InvokeArgs {
    /// Function name.
    pub name: String,
    /// Request body: a JSON document, or `@file` to read it from a file (at most 1 MiB).
    #[arg(long, value_name = "JSON|@FILE")]
    pub data: Option<String>,
    /// HTTP method [default: POST with --data, else GET].
    #[arg(long, value_name = "METHOD")]
    pub method: Option<String>,
    /// Sub-path (and query) after `/invoke`, e.g. `/hello?name=you`.
    #[arg(long, value_name = "PATH")]
    pub path: Option<String>,
    /// Also show where the time went: the host-call timeline of the execution from the server's insights API
    /// (`/api/v1/insights`), when the server has it; otherwise the logs and the trace id only.
    #[arg(long)]
    pub trace: bool,
    /// Print one JSON object instead of text.
    #[arg(long)]
    pub json: bool,
}

/// A connection target and credential.
pub(crate) struct Client {
    pub(crate) addr: String,
    pub(crate) token: Option<String>,
    /// Hint appended to a `403` error.
    pub(crate) forbidden: &'static str,
}

impl Client {
    pub(crate) fn new(g: &GlobalArgs, s: &ServerArgs) -> anyhow::Result<Self> {
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
        Ok(Self {
            addr,
            token,
            forbidden: " (the credential lacks manage_functions / read_functions)",
        })
    }

    /// Sends a request and returns the raw answer, whatever its status (the caller decides what is an error).
    pub(crate) async fn raw(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<&[u8]>,
    ) -> anyhow::Result<probe::Reply> {
        probe::request_with(&self.addr, method, path, self.token.as_deref(), headers, body, TIMEOUT)
            .await
            .with_context(|| {
                format!(
                    "cannot reach the server at {} (is `forgebase serve` running?)",
                    self.addr
                )
            })
    }

    /// Sends a request and returns the parsed JSON body (`Null` for an empty one); non-2xx becomes an error carrying the server's message.
    pub(crate) async fn call(&self, method: &str, path: &str, body: Option<&Value>) -> anyhow::Result<Value> {
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
            403 => self.forbidden,
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
    if let FunctionsCommand::New(a) = &cmd {
        scaffold(a, &mut std::io::stdout())?;
        return Ok(ExitCode::SUCCESS);
    }
    let client = Client::new(g, &server)?;
    match cmd {
        FunctionsCommand::New(_) => {}
        FunctionsCommand::Deploy(a) => deploy(&client, a, &mut std::io::stdout()).await?,
        FunctionsCommand::Logs(a) => {
            let stop = async {
                let _ = tokio::signal::ctrl_c().await;
            };
            logs(&client, a, &mut std::io::stdout(), stop).await?;
        }
        FunctionsCommand::Invoke(a) => {
            return if invoke(&client, a, &mut std::io::stdout()).await? {
                Ok(ExitCode::SUCCESS)
            } else {
                Ok(ExitCode::FAILURE)
            };
        }
    }
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn enc(name: &str) -> String {
    name.bytes()
        .map(|b| match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The function API declarations (the same file the server serves at `/api/v1/functions/types`).
const FORGEBASE_DTS: &str = include_str!("../../../sdk/functions/forgebase.d.ts");

/// Largest source the server accepts.
const MAX_SOURCE: u64 = 1 << 20;

fn template(ts: bool) -> String {
    let (req, ret) = if ts {
        (": Request", ": Promise<Response>")
    } else {
        ("", "")
    };
    format!(
        r#"/// <reference path="./forgebase.d.ts" />
import {{ serve }} from "forgebase";

// An HTTP function: GET /api/v1/functions/<name>/invoke?name=you
// Deploy with: forgebase functions deploy <name> {file} --access public --capability log
serve(async (req{req}, ctx){ret} => {{
  const url = new URL(req.url);
  const name = url.searchParams.get("name") ?? "world";
  console.log("greeting", name);
  return Response.json({{ message: `Hello, ${{name}}!`, caller: ctx.auth?.id ?? null }});
}});
"#,
        file = if ts { "index.ts" } else { "index.js" }
    )
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 48
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
        && name != "types"
}

/// `forgebase functions new`.
fn scaffold(a: &NewArgs, out: &mut impl Write) -> anyhow::Result<()> {
    if !valid_name(&a.name) {
        bail!(
            "{:?} is not a function name: a lower-case letter, then lower-case letters, digits, _ or - (at most 48)",
            a.name
        );
    }
    let dir = a.dir.join(&a.name);
    let entry = dir.join(if a.js { "index.js" } else { "index.ts" });
    let types = dir.join("forgebase.d.ts");
    if !a.force {
        for p in [&entry, &types] {
            if p.exists() {
                bail!("{} exists (pass --force to overwrite)", p.display());
            }
        }
    }
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    std::fs::write(&entry, template(!a.js).replace("<name>", &a.name))
        .with_context(|| format!("cannot write {}", entry.display()))?;
    std::fs::write(&types, FORGEBASE_DTS).with_context(|| format!("cannot write {}", types.display()))?;
    writeln!(out, "wrote {} and {}", entry.display(), types.display())?;
    writeln!(
        out,
        "deploy: forgebase functions deploy {} {} --access public --capability log",
        a.name,
        entry.display()
    )?;
    Ok(())
}

/// What a deploy uploads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Code {
    /// A WebAssembly module.
    Wasm(Vec<u8>),
    /// JavaScript or TypeScript source (`language` as the server names it).
    Source { language: &'static str, text: String },
}

/// Reads the function file: `.ts`/`.mts` TypeScript, `.js`/`.mjs` JavaScript, otherwise a WebAssembly module.
fn code_of(path: &Path) -> anyhow::Result<Code> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let language = match ext.as_str() {
        "ts" | "mts" => "typescript",
        "js" | "mjs" => "javascript",
        _ => return module_bytes(path).map(Code::Wasm),
    };
    let meta = std::fs::metadata(path).with_context(|| format!("cannot read {}", path.display()))?;
    if !meta.is_file() {
        bail!("{} is not a file", path.display());
    }
    if meta.len() > MAX_SOURCE {
        bail!(
            "{} is {} bytes; sources are limited to {MAX_SOURCE}",
            path.display(),
            meta.len()
        );
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("cannot read {} as UTF-8", path.display()))?;
    Ok(Code::Source { language, text })
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
fn deploy_body(a: &DeployArgs, code: &Code) -> anyhow::Result<Value> {
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
        "capabilities": a.capabilities,
        "fetch_hosts": a.fetch_hosts,
        "limits": limits,
        "env": parse_env(&a.env)?,
        "hooks": parse_hooks(&a.hooks)?,
        "activate": !a.no_activate,
    });
    match code {
        Code::Wasm(wasm) => body["wasm"] = json!(base64::engine::general_purpose::STANDARD.encode(wasm)),
        Code::Source { language, text } => {
            body["source"] = json!(text);
            body["language"] = json!(language);
        }
    }
    if let Some(d) = &a.description {
        body["description"] = json!(d);
    }
    if let Some(x) = a.access {
        body["access"] = json!(x.as_str());
    }
    Ok(body)
}

async fn deploy(client: &Client, a: DeployArgs, out: &mut impl Write) -> anyhow::Result<()> {
    let code = code_of(&a.path)?;
    let body = deploy_body(&a, &code)?;
    let done = client.call("POST", "/api/v1/functions", Some(&body)).await?;
    if a.json {
        writeln!(out, "{done}")?;
        return Ok(());
    }
    let v = &done["version"];
    let active = done["function"]["active_version"].as_i64().unwrap_or(0);
    writeln!(
        out,
        "deployed {} v{} ({}, {} bytes), {}",
        done["function"]["name"].as_str().unwrap_or(&a.name),
        v["version"],
        v["language"].as_str().unwrap_or("wasm"),
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
            + &e["trace_id"]
                .as_str()
                .map(|t| format!("  trace {t}"))
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
    let mut warned: Option<String> = None;
    tokio::pin!(stop);
    loop {
        tokio::select! {
            () = &mut stop => return Ok(()),
            () = tokio::time::sleep(pause) => {}
        }
        // A server that restarts (or a dev emulator that reloads) must not end the stream: say so once, keep polling.
        match fetch_logs(client, &a.name, FOLLOW_BATCH).await {
            Ok(items) => {
                if warned.take().is_some() {
                    eprintln!("logs: the server answers again");
                }
                for e in items {
                    let id = e["id"].as_i64().unwrap_or(0);
                    if id > last {
                        last = id;
                        print_execution(out, &e, a.json)?;
                    }
                }
            }
            Err(e) => {
                let msg = format!("{e:#}");
                if warned.as_deref() != Some(msg.as_str()) {
                    eprintln!("logs: {msg}; still trying");
                    warned = Some(msg);
                }
            }
        }
        out.flush()?;
    }
}

/// The body of `--data`: a JSON document, or the contents of `@file`.
fn invoke_body(data: &str) -> anyhow::Result<(Vec<u8>, bool)> {
    const MAX_BODY: usize = 1 << 20;
    let bytes = match data.strip_prefix('@') {
        Some(file) => std::fs::read(file).with_context(|| format!("cannot read {file}"))?,
        None => data.as_bytes().to_vec(),
    };
    if bytes.len() > MAX_BODY {
        bail!("--data is {} bytes; a function takes at most 1 MiB", bytes.len());
    }
    let is_json = serde_json::from_slice::<Value>(&bytes).is_ok();
    Ok((bytes, is_json))
}

/// The trace id (32 hex digits) of a W3C `traceparent` value.
fn trace_of(traceparent: &str) -> Option<String> {
    let mut parts = traceparent.split('-');
    let (_version, trace) = (parts.next()?, parts.next()?);
    (trace.len() == 32 && trace.bytes().all(|b| b.is_ascii_hexdigit())).then(|| trace.to_owned())
}

fn header<'a>(r: &'a probe::Reply, name: &str) -> Option<&'a str> {
    r.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
}

/// The execution log row of one request: the log is written when the run ends, so wait a moment for it.
async fn find_execution(client: &Client, name: &str, request_id: &str) -> Option<Value> {
    for attempt in 0..20 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let items = fetch_logs(client, name, 50).await.ok()?;
        if let Some(e) = items
            .into_iter()
            .rev()
            .find(|e| e["request_id"].as_str() == Some(request_id))
        {
            return Some(e);
        }
    }
    None
}

/// What the insights API said about an execution.
enum Profile {
    /// The execution's profile.
    Found(Value),
    /// The server answered but has no profile for it (profiling is off, or the row is gone).
    Empty,
    /// The server has no insights API, or the credential may not read it.
    Unavailable(String),
}

async fn fetch_profile(client: &Client, request_id: &str, execution_id: i64) -> Profile {
    let path = format!(
        "/api/v1/insights/functions/invocations?request_id={}&limit=20",
        enc(request_id)
    );
    match client.raw("GET", &path, &[], None).await {
        Err(e) => Profile::Unavailable(format!("{e:#}")),
        Ok(r) if r.status == 404 || r.status == 405 => Profile::Unavailable(
            "this server has no /api/v1/insights API (it comes with ops insights); showing the logs and trace id only"
                .into(),
        ),
        Ok(r) if r.status == 401 || r.status == 403 => Profile::Unavailable(format!(
            "the credential may not read /api/v1/insights (HTTP {}: it needs the engine:read_insights permission or an admin key)",
            r.status
        )),
        Ok(r) if !(200..300).contains(&r.status) => {
            Profile::Unavailable(format!("/api/v1/insights answered HTTP {}", r.status))
        }
        Ok(r) => {
            let v: Value = serde_json::from_slice(&r.body).unwrap_or(Value::Null);
            let item = v["items"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|i| i["id"].as_i64() == Some(execution_id));
            match item.map(|i| &i["profile"]) {
                Some(p) if p.is_object() => Profile::Found(p.clone()),
                _ => Profile::Empty,
            }
        }
    }
}

fn ms(us: Value) -> String {
    format!("{:.2}", us.as_f64().unwrap_or(0.0) / 1000.0)
}

fn render_profile(out: &mut impl Write, p: &Value) -> anyhow::Result<()> {
    let mut facts = Vec::new();
    if p["cold_start"].as_bool() == Some(true) {
        facts.push(format!("cold start (prepare {} ms)", ms(p["prepare_us"].clone())));
    }
    if let Some(f) = p["fuel_used"].as_u64() {
        facts.push(format!("fuel {f}/{}", p["fuel_limit"].as_u64().unwrap_or(0)));
    }
    if let Some(m) = p["memory_peak_bytes"].as_u64() {
        facts.push(format!(
            "peak memory {} KiB of {} KiB",
            m / 1024,
            p["memory_limit_bytes"].as_u64().unwrap_or(0) / 1024
        ));
    }
    if !facts.is_empty() {
        writeln!(out, "  {}", facts.join(", "))?;
    }
    let timeline = p["timeline"].as_array().cloned().unwrap_or_default();
    let total = p["host_calls"].as_u64().unwrap_or(timeline.len() as u64);
    if total == 0 {
        writeln!(out, "  host calls: none")?;
        return Ok(());
    }
    writeln!(out, "  host calls ({total}):")?;
    for c in &timeline {
        writeln!(
            out,
            "    +{:>8} ms  {:<14} {:>8} ms  {}",
            ms(c["at_us"].clone()),
            c["op"].as_str().unwrap_or("?"),
            ms(c["dur_us"].clone()),
            if c["ok"].as_bool() == Some(false) {
                "FAILED"
            } else {
                "ok"
            }
        )?;
    }
    let dropped = p["timeline_dropped"].as_u64().unwrap_or(0);
    if dropped > 0 {
        writeln!(out, "    ... {dropped} more call(s) not on the timeline")?;
    }
    if let Some(host) = p["host"].as_object().filter(|h| !h.is_empty()) {
        writeln!(out, "  by operation:")?;
        for (op, st) in host {
            writeln!(
                out,
                "    {:<14} {} call(s), {} ms total, {} ms longest{}",
                op,
                st["count"].as_u64().unwrap_or(0),
                ms(st["total_us"].clone()),
                ms(st["max_us"].clone()),
                match st["errors"].as_u64().unwrap_or(0) {
                    0 => String::new(),
                    n => format!(", {n} failed"),
                }
            )?;
        }
    }
    Ok(())
}

/// Calls the function; returns whether the HTTP status was a success.
async fn invoke(client: &Client, a: InvokeArgs, out: &mut impl Write) -> anyhow::Result<bool> {
    let (body, is_json) = match &a.data {
        Some(d) => {
            let (b, j) = invoke_body(d)?;
            (Some(b), j)
        }
        None => (None, true),
    };
    let method = a
        .method
        .clone()
        .unwrap_or_else(|| if body.is_some() { "POST" } else { "GET" }.to_owned())
        .to_ascii_uppercase();
    if !method.bytes().all(|b| b.is_ascii_uppercase()) {
        bail!("--method {method:?} is not an HTTP method");
    }
    let sub = a.path.clone().unwrap_or_default();
    let sub = if sub.is_empty() || sub.starts_with('/') || sub.starts_with('?') {
        sub
    } else {
        format!("/{sub}")
    };
    let path = format!("/api/v1/functions/{}/invoke{sub}", enc(&a.name));
    let request_id = format!(
        "fbcli-{}",
        forgebase_auth::crypto::random_token(9)
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .replace(['-', '_'], "x")
    );
    let content_type = if is_json {
        "application/json"
    } else {
        "text/plain; charset=utf-8"
    };
    let mut headers = vec![("X-Request-Id", request_id.as_str())];
    if body.is_some() {
        headers.push(("Content-Type", content_type));
    }
    let started = Instant::now();
    let reply = client.raw(&method, &path, &headers, body.as_deref()).await?;
    let duration_ms = started.elapsed().as_secs_f64() * 1000.0;
    let ok = (200..300).contains(&reply.status);

    // The execution (and its logs) when the function ran at all.
    let execution = if matches!(reply.status, 401 | 403 | 404 | 405) {
        None
    } else {
        find_execution(client, &a.name, &request_id).await
    };
    let trace_id = execution
        .as_ref()
        .and_then(|e| e["trace_id"].as_str().map(str::to_owned))
        .or_else(|| header(&reply, "traceparent").and_then(trace_of));
    let profile = match (&execution, a.trace) {
        (Some(e), true) => Some(fetch_profile(client, &request_id, e["id"].as_i64().unwrap_or(0)).await),
        (None, true) => Some(Profile::Empty),
        _ => None,
    };
    let content_type_of_reply = header(&reply, "content-type").unwrap_or("").to_owned();
    let parsed: Option<Value> = serde_json::from_slice(&reply.body).ok();

    if a.json {
        let body_value = match &parsed {
            Some(v) => v.clone(),
            None if reply.body.is_empty() => Value::Null,
            None => json!(String::from_utf8_lossy(&reply.body)),
        };
        let (timeline, note) = match &profile {
            Some(Profile::Found(p)) => (Some(p.clone()), None),
            Some(Profile::Unavailable(why)) => (None, Some(why.clone())),
            _ => (None, None),
        };
        writeln!(
            out,
            "{}",
            json!({
                "status": reply.status,
                "ok": ok,
                "duration_ms": (duration_ms * 100.0).round() / 100.0,
                "request_id": request_id,
                "trace_id": trace_id,
                "content_type": content_type_of_reply,
                "body": body_value,
                "execution": execution,
                "profile": timeline,
                "profile_note": note,
            })
        )?;
        return Ok(ok);
    }

    writeln!(out, "{method} {path}")?;
    writeln!(out, "status:    {}{}", reply.status, if ok { "" } else { "  (failed)" })?;
    writeln!(out, "duration:  {duration_ms:.1} ms (round trip)")?;
    writeln!(out, "request:   {request_id}")?;
    match &trace_id {
        Some(t) => writeln!(out, "trace id:  {t}")?,
        None => writeln!(out, "trace id:  (none: tracing is off for this request)")?,
    }
    match &parsed {
        Some(v) if !reply.body.is_empty() => {
            writeln!(out, "response:  {content_type_of_reply}")?;
            for l in serde_json::to_string_pretty(v)?.lines() {
                writeln!(out, "  {l}")?;
            }
        }
        _ if reply.body.is_empty() => writeln!(out, "response:  (empty)")?,
        _ => match std::str::from_utf8(&reply.body) {
            Ok(t) => {
                writeln!(out, "response:  {content_type_of_reply}")?;
                for l in t.lines().take(200) {
                    writeln!(out, "  {l}")?;
                }
            }
            Err(_) => writeln!(out, "response:  {} bytes of {content_type_of_reply}", reply.body.len())?,
        },
    }
    match &execution {
        Some(e) => {
            writeln!(
                out,
                "execution: #{} v{} {} {} in {} ms, {} fuel",
                e["id"],
                e["version"],
                e["trigger"].as_str().unwrap_or("?"),
                e["status"].as_str().unwrap_or("?"),
                e["duration_ms"],
                e["fuel_used"]
            )?;
            if let Some(err) = e["error"].as_str() {
                writeln!(out, "  error: {err}")?;
            }
            let lines = e["logs"].as_array().cloned().unwrap_or_default();
            if lines.is_empty() {
                writeln!(out, "logs:      (none)")?;
            } else {
                writeln!(out, "logs:")?;
                for l in &lines {
                    writeln!(
                        out,
                        "  [{}] {}",
                        l["level"].as_str().unwrap_or("?"),
                        l["message"].as_str().unwrap_or("")
                    )?;
                }
            }
        }
        None if matches!(reply.status, 401 | 403 | 404 | 405) => {
            writeln!(out, "execution: none (the function did not run)")?;
        }
        None => writeln!(out, "execution: not found in the function's log (yet)")?,
    }
    match profile {
        Some(Profile::Found(p)) => {
            writeln!(out, "trace:")?;
            render_profile(out, &p)?;
        }
        Some(Profile::Empty) => writeln!(
            out,
            "trace:     no profile recorded for this execution ([observability] function_profiling may be off)"
        )?,
        Some(Profile::Unavailable(why)) => writeln!(out, "trace:     host-call timeline unavailable: {why}")?,
        None => {}
    }
    Ok(ok)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testserver::fake_server;
    use tokio::net::TcpListener;

    fn client(addr: &str, token: Option<&str>) -> Client {
        Client {
            addr: addr.to_owned(),
            token: token.map(str::to_owned),
            forbidden: " (the credential lacks manage_functions / read_functions)",
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
        let b = deploy_body(&a, &Code::Wasm(b"\0asm\x01\0\0\0".to_vec())).unwrap();
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
        let plain = deploy_body(&deploy_args("hello", dir.path()), &Code::Wasm(b"x".to_vec())).unwrap();
        assert!(plain.get("description").is_none() && plain.get("access").is_none());
        assert_eq!(plain["activate"], true);
    }

    #[test]
    fn typescript_and_javascript_files_deploy_as_source() {
        let dir = tempfile::tempdir().unwrap();
        let ts = dir.path().join("index.ts");
        std::fs::write(&ts, "serve(() => new Response(\"hi\"));").unwrap();
        let code = code_of(&ts).unwrap();
        assert_eq!(
            code,
            Code::Source {
                language: "typescript",
                text: "serve(() => new Response(\"hi\"));".into()
            }
        );
        let b = deploy_body(&deploy_args("hello", &ts), &code).unwrap();
        assert_eq!(b["language"], "typescript");
        assert!(b.get("wasm").is_none());
        let js = dir.path().join("index.mjs");
        std::fs::write(&js, "x").unwrap();
        assert!(matches!(
            code_of(&js).unwrap(),
            Code::Source {
                language: "javascript",
                ..
            }
        ));
        assert!(matches!(code_of(&wasm_file(dir.path())).unwrap(), Code::Wasm(_)));
    }

    #[test]
    fn new_writes_a_template_and_the_types() {
        let dir = tempfile::tempdir().unwrap();
        let mut out = Vec::new();
        let a = NewArgs {
            name: "hello".into(),
            js: false,
            dir: dir.path().join("functions"),
            force: false,
        };
        scaffold(&a, &mut out).unwrap();
        let index = std::fs::read_to_string(dir.path().join("functions/hello/index.ts")).unwrap();
        assert!(index.contains("import { serve } from \"forgebase\""));
        assert!(index.contains("req: Request"));
        let dts = std::fs::read_to_string(dir.path().join("functions/hello/forgebase.d.ts")).unwrap();
        assert!(dts.contains("declare module \"forgebase\""));
        assert!(scaffold(&a, &mut out).unwrap_err().to_string().contains("--force"));
        let js = NewArgs {
            name: "plain".into(),
            js: true,
            dir: dir.path().join("functions"),
            force: false,
        };
        scaffold(&js, &mut out).unwrap();
        let index = std::fs::read_to_string(dir.path().join("functions/plain/index.js")).unwrap();
        assert!(!index.contains(": Request"));
        let bad = NewArgs {
            name: "Bad".into(),
            js: true,
            dir: dir.path().into(),
            force: false,
        };
        assert!(scaffold(&bad, &mut out).is_err());
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
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "deployed hello v3 (wasm, 8 bytes), active\n"
        );
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
