//! `forgebase queues publish|pull|tail|listen|…` and `forgebase jobs watch <ID>`: talk to a running
//! server's durable topics (`/api/v1/queues`, ADR-0022) and long-running tasks (`/api/v1/jobs`,
//! ADR-0023).
//!
//! Connection and credentials work as for `forgebase functions` (`--server` / `FB_URL`, `--token` /
//! `FB_TOKEN`, plain `http://` only). Streaming is done by polling: `tail` reads the topic's retained
//! messages after the last sequence number it printed (it never takes deliveries from a subscription),
//! and `jobs watch` polls the job. No server route is added for either.
//!
//! Webhook tooling (ADR-0031): `listen` forwards a topic to a local URL like `stripe listen` (a
//! temporary pull subscription with a time to live, consumed over the WebSocket streaming pull, each
//! delivery signed with a session secret and acked on `2xx`); `test`, `redrive`, `retry`,
//! `rotate-secret`, `pause`, `resume`, `attempts` and `catalog` are thin calls of the matching routes.

use std::collections::VecDeque;
use std::future::Future;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, bail};
use clap::{Args, Subcommand};
use serde_json::{Value, json};

use crate::GlobalArgs;
use crate::functions::{Client, ServerArgs, enc};

const FORBIDDEN: &str = " (the credential lacks the queue or jobs permission this command needs)";
/// Largest message body read from a file or standard input.
const MAX_DATA: u64 = 1 << 20;
const PAGE: u32 = 500;

/// Arguments of `forgebase queues`.
#[derive(Debug, Args)]
pub struct QueuesCli {
    #[command(flatten)]
    pub server: ServerArgs,
    #[command(subcommand)]
    pub command: QueuesCommand,
}

#[derive(Debug, Subcommand)]
pub enum QueuesCommand {
    /// Publish one message to a topic.
    Publish(PublishArgs),
    /// Pull messages from a subscription (they are leased to this command; use --ack to finish them).
    Pull(PullArgs),
    /// Print a topic's newest messages, and with --follow every new one (does not consume anything).
    Tail(TailArgs),
    /// Forward a topic's messages to a local URL, signed like webhooks (Ctrl-C to stop).
    Listen(ListenArgs),
    /// Send a signed test event to a push subscription's endpoint and show the attempt.
    Test(TestArgs),
    /// Deliver dead letters of a subscription again (by id, or --all).
    Redrive(RedriveArgs),
    /// Retry now the deliveries of a subscription that wait for a retry backoff.
    Retry(RetryArgs),
    /// Rotate a webhook's signing secret (both secrets sign during the grace window).
    RotateSecret(RotateSecretArgs),
    /// Pause push delivery of a subscription (messages accumulate).
    Pause(PauseArgs),
    /// Resume push delivery of a paused subscription (the backlog is delivered in order).
    Resume(SubArgs),
    /// Show a push subscription's delivery attempts, newest first.
    Attempts(AttemptsArgs),
    /// List the subscribable event types: topics, schemas, examples.
    Catalog(CatalogArgs),
}

#[derive(Debug, Args)]
pub struct ListenArgs {
    /// Topic to listen to.
    pub topic: String,
    /// Local URL each message is POSTed to, e.g. http://localhost:3000/webhooks (http:// only).
    #[arg(long, value_name = "URL")]
    pub forward_to: String,
    /// Attribute filter of the temporary subscription, as JSON (e.g. '{"forgebase.op":"create"}').
    #[arg(long, value_name = "JSON")]
    pub filter: Option<String>,
    /// Lifetime of the temporary subscription while this command runs; it is renewed every third of it,
    /// so a crashed session leaves nothing behind for longer (60-86400 seconds).
    #[arg(long, default_value_t = 600, value_name = "SECONDS", value_parser = clap::value_parser!(u64).range(60..=86_400))]
    pub ttl: u64,
    /// Most deliveries in flight at once.
    #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u32).range(1..=1000))]
    pub max_in_flight: u32,
    /// One JSON object per delivery (NDJSON).
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct SubArgs {
    /// Subscription name.
    pub subscription: String,
    /// Print the server's answer as JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct TestArgs {
    /// Push subscription name.
    pub subscription: String,
    /// `data` of the test message as JSON (default: a short forgebase.test object).
    #[arg(long, value_name = "JSON")]
    pub data: Option<String>,
    /// Print the attempt as JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct RedriveArgs {
    /// Subscription name.
    pub subscription: String,
    /// Dead-letter record id. Repeatable.
    #[arg(long = "id", value_name = "ID", conflicts_with = "all")]
    pub ids: Vec<i64>,
    /// Every dead letter not redriven yet.
    #[arg(long)]
    pub all: bool,
    /// With --all: only those dead-lettered at or after this RFC 3339 time.
    #[arg(long, requires = "all", value_name = "TIME")]
    pub since: Option<String>,
    /// With --all: only those whose attributes match this filter (JSON).
    #[arg(long, requires = "all", value_name = "JSON")]
    pub filter: Option<String>,
    /// With --all: most records this call redrives (1-1000).
    #[arg(long, requires = "all", value_parser = clap::value_parser!(u32).range(1..=1000))]
    pub limit: Option<u32>,
    /// Print the server's answer as JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct RetryArgs {
    /// Subscription name.
    pub subscription: String,
    /// Only this message. Repeatable.
    #[arg(long = "message-id", value_name = "ID")]
    pub message_ids: Vec<String>,
    /// Print the server's answer as JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct RotateSecretArgs {
    /// Webhook subscription name.
    pub subscription: String,
    /// Seconds the old secret keeps signing too (0-604800).
    #[arg(long, default_value_t = 86_400, value_name = "SECONDS", value_parser = clap::value_parser!(u64).range(0..=604_800))]
    pub grace: u64,
    /// Do not rotate: end the grace window of the last rotation now.
    #[arg(long)]
    pub expire_previous: bool,
    /// Print the server's answer as JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct PauseArgs {
    /// Push subscription name.
    pub subscription: String,
    /// Why (shown as paused_reason).
    #[arg(long)]
    pub reason: Option<String>,
    /// Print the server's answer as JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct AttemptsArgs {
    /// Push subscription name.
    pub subscription: String,
    /// Only `success` or `failure`.
    #[arg(long, value_parser = ["success", "failure"])]
    pub outcome: Option<String>,
    /// Only attempts of this message.
    #[arg(long, value_name = "ID")]
    pub message_id: Option<String>,
    /// Only attempts at or after this RFC 3339 time.
    #[arg(long, value_name = "TIME")]
    pub since: Option<String>,
    /// Only test sends.
    #[arg(long)]
    pub tests: bool,
    /// How many (1-500).
    #[arg(short = 'n', long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..=500))]
    pub limit: u32,
    /// One JSON object per attempt (NDJSON).
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct CatalogArgs {
    /// Print the catalog as JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct PublishArgs {
    /// Topic name.
    pub topic: String,
    /// The message data as JSON text, e.g. '{"order":42}'. Use --file or --stdin for larger bodies.
    #[arg(conflicts_with_all = ["file", "stdin"])]
    pub data: Option<String>,
    /// Read the JSON data from a file.
    #[arg(long, value_name = "FILE", conflicts_with = "stdin")]
    pub file: Option<PathBuf>,
    /// Read the JSON data from standard input.
    #[arg(long)]
    pub stdin: bool,
    /// Send the data as a plain JSON string instead of parsing it.
    #[arg(long)]
    pub text: bool,
    /// Attribute `KEY=VALUE`. Repeatable.
    #[arg(long = "attr", value_name = "KEY=VALUE")]
    pub attrs: Vec<String>,
    /// Ordering key: messages with one key reach a subscription in publish order.
    #[arg(long, value_name = "KEY")]
    pub ordering_key: Option<String>,
    /// Dedup id: a repeat inside the topic's dedup window is accepted once.
    #[arg(long, value_name = "ID")]
    pub dedup_id: Option<String>,
    /// Print the server's answer as JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct PullArgs {
    /// Subscription name.
    pub subscription: String,
    /// Most messages to take (1-1000).
    #[arg(short = 'n', long, default_value_t = 10, value_parser = clap::value_parser!(u32).range(1..=1000))]
    pub max: u32,
    /// Long-poll up to this many milliseconds when nothing is ready.
    #[arg(long, default_value_t = 0, value_name = "MS")]
    pub wait_ms: u32,
    /// Acknowledge the messages after printing them.
    #[arg(long, conflicts_with = "nack")]
    pub ack: bool,
    /// Negatively acknowledge them (redelivered after the subscription's backoff).
    #[arg(long)]
    pub nack: bool,
    /// One JSON object per message (NDJSON), including the ack id.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct TailArgs {
    /// Topic name.
    pub topic: String,
    /// How many of the newest retained messages to show first.
    #[arg(short = 'n', long, default_value_t = 10)]
    pub lines: usize,
    /// Keep running and print new messages as they arrive (Ctrl-C to stop).
    #[arg(short, long)]
    pub follow: bool,
    /// Seconds between polls with --follow.
    #[arg(long, default_value_t = 1.0, value_name = "SECONDS")]
    pub interval: f64,
    /// One JSON object per message (NDJSON).
    #[arg(long)]
    pub json: bool,
}

/// Arguments of `forgebase jobs`.
#[derive(Debug, Args)]
pub struct JobsCli {
    #[command(flatten)]
    pub server: ServerArgs,
    #[command(subcommand)]
    pub command: JobsCommand,
}

#[derive(Debug, Subcommand)]
pub enum JobsCommand {
    /// Follow a job until it finishes: status, progress, stage and message as they change.
    /// Exit code 0 when it succeeded, 1 when it died or was cancelled.
    Watch(WatchArgs),
}

#[derive(Debug, Args)]
pub struct WatchArgs {
    /// Job id.
    pub id: String,
    /// Seconds between polls.
    #[arg(long, default_value_t = 1.0, value_name = "SECONDS")]
    pub interval: f64,
    /// Give up after this many seconds (exit code 2); 0 waits forever.
    #[arg(long, default_value_t = 0, value_name = "SECONDS")]
    pub timeout: u64,
    /// One JSON object per change (NDJSON).
    #[arg(long)]
    pub json: bool,
}

fn client(g: &GlobalArgs, s: &ServerArgs) -> anyhow::Result<Client> {
    let mut c = Client::new(g, s)?;
    c.forbidden = FORBIDDEN;
    Ok(c)
}

fn pause(seconds: f64) -> anyhow::Result<Duration> {
    if !(seconds.is_finite() && seconds >= 0.05) {
        bail!("--interval must be at least 0.05 seconds");
    }
    Ok(Duration::from_secs_f64(seconds))
}

async fn ctrl_c() {
    let _ = tokio::signal::ctrl_c().await;
}

/// Runs a `queues` subcommand.
pub async fn run_queues(g: &GlobalArgs, server: ServerArgs, cmd: QueuesCommand) -> anyhow::Result<ExitCode> {
    let c = client(g, &server)?;
    let out = &mut std::io::stdout();
    match cmd {
        QueuesCommand::Publish(a) => {
            let data = read_data(&a)?;
            publish(&c, &a, data, out).await?;
        }
        QueuesCommand::Pull(a) => pull(&c, &a, out).await?,
        QueuesCommand::Tail(a) => tail(&c, a, out, ctrl_c()).await?,
        QueuesCommand::Listen(a) => listen(&c, &a, out, ctrl_c()).await?,
        QueuesCommand::Test(a) => test_send(&c, &a, out).await?,
        QueuesCommand::Redrive(a) => redrive(&c, &a, out).await?,
        QueuesCommand::Retry(a) => retry(&c, &a, out).await?,
        QueuesCommand::RotateSecret(a) => rotate_secret(&c, &a, out).await?,
        QueuesCommand::Pause(a) => pause_sub(&c, &a, out).await?,
        QueuesCommand::Resume(a) => resume_sub(&c, &a, out).await?,
        QueuesCommand::Attempts(a) => attempts(&c, &a, out).await?,
        QueuesCommand::Catalog(a) => catalog(&c, &a, out).await?,
    }
    Ok(ExitCode::SUCCESS)
}

/// Runs a `jobs` subcommand.
pub async fn run_jobs(g: &GlobalArgs, server: ServerArgs, cmd: JobsCommand) -> anyhow::Result<ExitCode> {
    let c = client(g, &server)?;
    match cmd {
        JobsCommand::Watch(a) => watch(&c, a, &mut std::io::stdout(), ctrl_c()).await,
    }
}

// ------------------------------------------------------------------ publish

fn read_data(a: &PublishArgs) -> anyhow::Result<Value> {
    let text = if let Some(t) = &a.data {
        t.clone()
    } else if let Some(p) = &a.file {
        let mut s = String::new();
        std::fs::File::open(p)
            .with_context(|| format!("cannot read {}", p.display()))?
            .take(MAX_DATA + 1)
            .read_to_string(&mut s)
            .with_context(|| format!("cannot read {}", p.display()))?;
        s
    } else if a.stdin {
        let mut s = String::new();
        std::io::stdin()
            .lock()
            .take(MAX_DATA + 1)
            .read_to_string(&mut s)
            .context("cannot read standard input")?;
        s
    } else {
        bail!("give the message data as an argument, with --file, or with --stdin");
    };
    if text.len() as u64 > MAX_DATA {
        bail!("the message data is over {MAX_DATA} bytes");
    }
    if a.text {
        return Ok(Value::String(text.trim_end_matches(['\r', '\n']).to_owned()));
    }
    serde_json::from_str(&text)
        .with_context(|| "the data is not valid JSON (quote a plain string as '\"text\"', or pass --text)".to_owned())
}

fn parse_attrs(items: &[String]) -> anyhow::Result<serde_json::Map<String, Value>> {
    let mut out = serde_json::Map::new();
    for item in items {
        match item.split_once('=') {
            Some((k, v)) if !k.trim().is_empty() => {
                out.insert(k.trim().to_owned(), Value::String(v.to_owned()));
            }
            _ => bail!("--attr expects KEY=VALUE, got {item:?}"),
        }
    }
    Ok(out)
}

fn publish_body(a: &PublishArgs, data: Value) -> anyhow::Result<Value> {
    let mut msg = json!({ "data": data, "attributes": parse_attrs(&a.attrs)? });
    if let Some(k) = &a.ordering_key {
        msg["ordering_key"] = json!(k);
    }
    if let Some(d) = &a.dedup_id {
        msg["dedup_id"] = json!(d);
    }
    Ok(json!({ "messages": [msg] }))
}

async fn publish(c: &Client, a: &PublishArgs, data: Value, out: &mut impl Write) -> anyhow::Result<()> {
    let body = publish_body(a, data)?;
    let done = c
        .call(
            "POST",
            &format!("/api/v1/queues/topics/{}/publish", enc(&a.topic)),
            Some(&body),
        )
        .await?;
    if a.json {
        writeln!(out, "{done}")?;
        return Ok(());
    }
    let r = &done["results"][0];
    writeln!(
        out,
        "published {} to {} (seq {}){}",
        r["message_id"].as_str().unwrap_or("?"),
        a.topic,
        r["seq"],
        if r["duplicate"].as_bool() == Some(true) {
            ", duplicate: accepted once before"
        } else {
            ""
        }
    )?;
    Ok(())
}

// --------------------------------------------------------------------- pull

fn print_received(out: &mut impl Write, m: &Value, json: bool) -> anyhow::Result<()> {
    if json {
        writeln!(out, "{m}")?;
        return Ok(());
    }
    let msg = &m["message"];
    write!(
        out,
        "{}  seq {}  attempt {}  {}",
        msg["published_at"].as_str().unwrap_or("?"),
        msg["seq"],
        m["delivery_attempt"],
        msg["id"].as_str().unwrap_or("?"),
    )?;
    if let Some(k) = msg["ordering_key"].as_str() {
        write!(out, "  key {k}")?;
    }
    writeln!(out)?;
    writeln!(out, "    {}", msg["data"])?;
    if let Some(attrs) = msg["attributes"].as_object().filter(|a| !a.is_empty()) {
        let list: Vec<String> = attrs
            .iter()
            .map(|(k, v)| format!("{k}={}", v.as_str().unwrap_or("")))
            .collect();
        writeln!(out, "    attributes: {}", list.join(", "))?;
    }
    Ok(())
}

async fn pull(c: &Client, a: &PullArgs, out: &mut impl Write) -> anyhow::Result<()> {
    let sub = enc(&a.subscription);
    let body = json!({ "max_messages": a.max, "wait_ms": a.wait_ms, "consumer": "cli" });
    let got = c
        .call("POST", &format!("/api/v1/queues/subscriptions/{sub}/pull"), Some(&body))
        .await?;
    let messages = got["messages"].as_array().cloned().unwrap_or_default();
    for m in &messages {
        print_received(out, m, a.json)?;
    }
    if messages.is_empty() && !a.json {
        writeln!(out, "no messages ready on {}", a.subscription)?;
    }
    let ids: Vec<&str> = messages.iter().filter_map(|m| m["ack_id"].as_str()).collect();
    if (a.ack || a.nack) && !ids.is_empty() {
        let verb = if a.ack { "ack" } else { "nack" };
        let r = c
            .call(
                "POST",
                &format!("/api/v1/queues/subscriptions/{sub}/{verb}"),
                Some(&json!({ "ack_ids": ids })),
            )
            .await?;
        if !a.json {
            writeln!(
                out,
                "{}ed {} message(s){}",
                verb,
                r["count"],
                match r["unknown"].as_array().map_or(0, Vec::len) {
                    0 => String::new(),
                    n => format!(", {n} ack id(s) no longer valid"),
                }
            )?;
        }
    } else if !a.json && !ids.is_empty() {
        writeln!(
            out,
            "{} message(s) leased to this command; they return after the ack deadline unless you use --ack",
            ids.len()
        )?;
    }
    out.flush()?;
    Ok(())
}

// --------------------------------------------------------------------- tail

fn print_message(out: &mut impl Write, m: &Value, json: bool) -> anyhow::Result<()> {
    if json {
        writeln!(out, "{m}")?;
        return Ok(());
    }
    write!(
        out,
        "{}  seq {}  {}",
        m["published_at"].as_str().unwrap_or("?"),
        m["seq"],
        m["id"].as_str().unwrap_or("?"),
    )?;
    if let Some(k) = m["ordering_key"].as_str() {
        write!(out, "  key {k}")?;
    }
    writeln!(out)?;
    writeln!(out, "    {}", m["data"])?;
    Ok(())
}

/// One page of retained messages with a sequence number above `after`, oldest first, and the cursor of the next page.
async fn fetch_page(c: &Client, topic: &str, after: i64, limit: u32) -> anyhow::Result<(Vec<Value>, Option<i64>)> {
    let v = c
        .call(
            "GET",
            &format!(
                "/api/v1/queues/topics/{}/messages?after={after}&limit={limit}",
                enc(topic)
            ),
            None,
        )
        .await?;
    Ok((
        v["items"].as_array().cloned().unwrap_or_default(),
        v["next_after"].as_i64(),
    ))
}

async fn tail(c: &Client, a: TailArgs, out: &mut impl Write, stop: impl Future<Output = ()>) -> anyhow::Result<()> {
    let pause = if a.follow { Some(pause(a.interval)?) } else { None };
    // The newest `lines` of what is retained: page through, keeping a ring.
    let mut last = 0_i64;
    let mut ring: VecDeque<Value> = VecDeque::new();
    loop {
        let (items, next) = fetch_page(c, &a.topic, last, PAGE).await?;
        for m in items {
            last = last.max(m["seq"].as_i64().unwrap_or(0));
            if a.lines > 0 {
                if ring.len() == a.lines {
                    ring.pop_front();
                }
                ring.push_back(m);
            }
        }
        match next {
            Some(n) if n > 0 => last = last.max(n),
            _ => break,
        }
    }
    for m in &ring {
        print_message(out, m, a.json)?;
    }
    out.flush()?;
    let Some(pause) = pause else { return Ok(()) };
    tokio::pin!(stop);
    loop {
        tokio::select! {
            () = &mut stop => return Ok(()),
            () = tokio::time::sleep(pause) => {}
        }
        loop {
            let (items, next) = fetch_page(c, &a.topic, last, PAGE).await?;
            for m in items {
                last = last.max(m["seq"].as_i64().unwrap_or(0));
                print_message(out, &m, a.json)?;
            }
            match next {
                Some(n) if n > 0 => last = last.max(n),
                _ => break,
            }
        }
        out.flush()?;
    }
}

// ------------------------------------------------------------ webhook tooling

fn json_arg(what: &str, text: &str) -> anyhow::Result<Value> {
    serde_json::from_str(text).with_context(|| format!("{what} is not valid JSON"))
}

fn sub_path(sub: &str, rest: &str) -> String {
    format!("/api/v1/queues/subscriptions/{}{rest}", enc(sub))
}

fn print_attempt(out: &mut impl Write, a: &Value, json: bool) -> anyhow::Result<()> {
    if json {
        writeln!(out, "{a}")?;
        return Ok(());
    }
    let status = a["status"].as_i64().map_or_else(|| "-".to_owned(), |s| s.to_string());
    write!(
        out,
        "#{}  {}  {}  attempt {}  {}  HTTP {}  {} ms",
        a["id"],
        a["at"].as_str().unwrap_or("?"),
        a["message_id"].as_str().unwrap_or("?"),
        a["attempt"],
        a["outcome"].as_str().unwrap_or("?"),
        status,
        a["latency_ms"],
    )?;
    if a["test"].as_bool() == Some(true) {
        write!(out, "  (test)")?;
    }
    writeln!(out)?;
    if let Some(e) = a["error"].as_str() {
        writeln!(out, "    {}: {e}", a["error_class"].as_str().unwrap_or("error"))?;
    }
    if let Some(b) = a["response_body"].as_str().filter(|b| !b.is_empty()) {
        let cut = if a["response_truncated"].as_bool() == Some(true) {
            " …"
        } else {
            ""
        };
        writeln!(out, "    response: {b}{cut}")?;
    }
    Ok(())
}

async fn test_send(c: &Client, a: &TestArgs, out: &mut impl Write) -> anyhow::Result<()> {
    let body = match &a.data {
        Some(d) => json!({ "data": json_arg("--data", d)? }),
        None => json!({}),
    };
    let r = c.call("POST", &sub_path(&a.subscription, "/test"), Some(&body)).await?;
    print_attempt(out, &r, a.json)?;
    Ok(())
}

async fn redrive(c: &Client, a: &RedriveArgs, out: &mut impl Write) -> anyhow::Result<()> {
    if !a.all && a.ids.is_empty() {
        bail!("name dead letters with --id (repeatable), or pass --all");
    }
    let mut body = if a.all {
        json!({ "all": true })
    } else {
        json!({ "ids": a.ids })
    };
    if let Some(s) = &a.since {
        body["since"] = json!(s);
    }
    if let Some(f) = &a.filter {
        body["filter"] = json_arg("--filter", f)?;
    }
    if let Some(l) = a.limit {
        body["limit"] = json!(l);
    }
    let r = c
        .call("POST", &sub_path(&a.subscription, "/redrive"), Some(&body))
        .await?;
    if a.json {
        writeln!(out, "{r}")?;
        return Ok(());
    }
    let n = |k: &str| r[k].as_array().map_or(0, Vec::len);
    writeln!(out, "redrove {} dead letter(s) of {}", n("redriven"), a.subscription)?;
    if n("already_redriven") > 0 {
        writeln!(
            out,
            "{} were redriven before (each is redriven once)",
            n("already_redriven")
        )?;
    }
    if n("not_found") > 0 {
        writeln!(out, "not dead letters of {}: {}", a.subscription, r["not_found"])?;
    }
    if let Some(m) = r["remaining"].as_i64().filter(|m| *m > 0) {
        writeln!(out, "{m} more match; run it again")?;
    }
    Ok(())
}

async fn retry(c: &Client, a: &RetryArgs, out: &mut impl Write) -> anyhow::Result<()> {
    let body = json!({ "message_ids": a.message_ids });
    let r = c
        .call("POST", &sub_path(&a.subscription, "/retry"), Some(&body))
        .await?;
    if a.json {
        writeln!(out, "{r}")?;
    } else {
        writeln!(out, "{} delivery(ies) of {} retry now", r["count"], a.subscription)?;
    }
    Ok(())
}

async fn rotate_secret(c: &Client, a: &RotateSecretArgs, out: &mut impl Write) -> anyhow::Result<()> {
    if a.expire_previous {
        let r = c
            .call(
                "POST",
                &sub_path(&a.subscription, "/secret/expire-previous"),
                Some(&json!({})),
            )
            .await?;
        if a.json {
            writeln!(out, "{r}")?;
        } else {
            writeln!(
                out,
                "the previous secret of {} no longer signs deliveries",
                a.subscription
            )?;
        }
        return Ok(());
    }
    let body = json!({ "grace_ms": a.grace.saturating_mul(1000) });
    let r = c
        .call("POST", &sub_path(&a.subscription, "/secret/rotate"), Some(&body))
        .await?;
    if a.json {
        writeln!(out, "{r}")?;
        return Ok(());
    }
    writeln!(out, "new signing secret of {} (shown once):", a.subscription)?;
    writeln!(out, "{}", r["push_secret"].as_str().unwrap_or("?"))?;
    match r["previous_expires_at"].as_str() {
        Some(t) => writeln!(
            out,
            "until {t} deliveries carry both signatures (t=…,v1=<new>,v1=<old>); switch your receiver before then"
        )?,
        None => writeln!(out, "the old secret no longer signs deliveries")?,
    }
    Ok(())
}

async fn pause_sub(c: &Client, a: &PauseArgs, out: &mut impl Write) -> anyhow::Result<()> {
    let body = match &a.reason {
        Some(r) => json!({ "reason": r }),
        None => json!({}),
    };
    let r = c
        .call("POST", &sub_path(&a.subscription, "/pause"), Some(&body))
        .await?;
    if a.json {
        writeln!(out, "{r}")?;
    } else {
        writeln!(
            out,
            "paused {} ({}); messages accumulate until `forgebase queues resume {}`",
            a.subscription,
            r["paused_reason"].as_str().unwrap_or("paused"),
            a.subscription
        )?;
    }
    Ok(())
}

async fn resume_sub(c: &Client, a: &SubArgs, out: &mut impl Write) -> anyhow::Result<()> {
    let r = c
        .call("POST", &sub_path(&a.subscription, "/resume"), Some(&json!({})))
        .await?;
    if a.json {
        writeln!(out, "{r}")?;
    } else {
        writeln!(out, "resumed {}; its backlog is delivered in order", a.subscription)?;
    }
    Ok(())
}

async fn attempts(c: &Client, a: &AttemptsArgs, out: &mut impl Write) -> anyhow::Result<()> {
    let mut q = format!("?limit={}", a.limit);
    if let Some(o) = &a.outcome {
        q.push_str(&format!("&outcome={}", enc(o)));
    }
    if let Some(m) = &a.message_id {
        q.push_str(&format!("&message_id={}", enc(m)));
    }
    if let Some(s) = &a.since {
        q.push_str(&format!("&since={}", enc(s)));
    }
    if a.tests {
        q.push_str("&test=true");
    }
    let r = c
        .call("GET", &sub_path(&a.subscription, &format!("/attempts{q}")), None)
        .await?;
    let items = r["items"].as_array().cloned().unwrap_or_default();
    for item in &items {
        print_attempt(out, item, a.json)?;
    }
    if items.is_empty() && !a.json {
        writeln!(out, "no attempts recorded for {}", a.subscription)?;
    }
    Ok(())
}

async fn catalog(c: &Client, a: &CatalogArgs, out: &mut impl Write) -> anyhow::Result<()> {
    let r = c.call("GET", "/api/v1/queues/catalog", None).await?;
    if a.json {
        writeln!(out, "{r}")?;
        return Ok(());
    }
    for t in r["topics"].as_array().cloned().unwrap_or_default() {
        write!(out, "{}", t["topic"].as_str().unwrap_or("?"))?;
        if let Some(d) = t["description"].as_str() {
            write!(out, "  {d}")?;
        }
        writeln!(out)?;
        for e in t["event_types"].as_array().cloned().unwrap_or_default() {
            writeln!(out, "    {}  filter {}", e["type"].as_str().unwrap_or("?"), e["filter"])?;
        }
        if !t["example"].is_null() {
            writeln!(out, "    example: {}", t["example"])?;
        }
    }
    Ok(())
}

// ------------------------------------------------------------------- listen

/// Where `listen` forwards to: a plain `http://` URL.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Target {
    /// `host:port` to dial.
    addr: String,
    /// `Host` header.
    host: String,
    /// Path and query.
    path: String,
}

fn parse_target(url: &str) -> anyhow::Result<Target> {
    let rest = match url.strip_prefix("http://") {
        Some(r) => r,
        None if url.starts_with("https://") => {
            bail!("--forward-to must be http:// (the CLI has no TLS stack; local receivers rarely need it)")
        }
        None => bail!("--forward-to must look like http://localhost:3000/webhooks, got {url:?}"),
    };
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    if host.is_empty() || host.contains('@') {
        bail!("--forward-to must name a host (no credentials), got {url:?}");
    }
    let addr = if host.starts_with('[') {
        if host.contains("]:") {
            host.to_owned()
        } else {
            format!("{host}:80")
        }
    } else if host.contains(':') {
        host.to_owned()
    } else {
        format!("{host}:80")
    };
    Ok(Target {
        addr,
        host: host.to_owned(),
        path: path.to_owned(),
    })
}

/// POSTs `body` to `t` (HTTP/1.1, `Connection: close`) and returns the status.
async fn forward(t: &Target, headers: &[(String, String)], body: &[u8]) -> std::io::Result<u16> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    tokio::time::timeout(Duration::from_secs(30), async {
        let mut stream = tokio::net::TcpStream::connect(&t.addr).await?;
        let mut head = format!(
            "POST {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: forgebase-cli (queues listen)\r\nConnection: close\r\nContent-Length: {}\r\n",
            t.path,
            t.host,
            body.len()
        );
        for (k, v) in headers {
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        head.push_str("\r\n");
        stream.write_all(head.as_bytes()).await?;
        stream.write_all(body).await?;
        let mut buf = Vec::new();
        let mut chunk = [0_u8; 4096];
        // The status line is all that matters; stop once it is in.
        loop {
            let n = stream.read(&mut chunk).await?;
            buf.extend_from_slice(&chunk[..n]);
            if n == 0 || buf.windows(2).any(|w| w == b"\r\n") || buf.len() > 64 * 1024 {
                break;
            }
        }
        let line = String::from_utf8_lossy(&buf);
        line.lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse::<u16>().ok())
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "not an HTTP response"))
    })
    .await
    .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "the local endpoint did not answer in 30 s"))?
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// One delivery forwarded: signed like a webhook push, then acked on `2xx`, nacked otherwise.
async fn forward_one(
    t: &Target,
    secret: &str,
    sub: &str,
    m: &Value,
    out: &mut impl Write,
    json: bool,
) -> anyhow::Result<bool> {
    let envelope = json!({
        "subscription": sub,
        "delivery_attempt": m["delivery_attempt"],
        "message": m["message"],
    });
    let body = serde_json::to_vec(&envelope)?;
    let id = m["message"]["id"].as_str().unwrap_or("?").to_owned();
    let attempt = m["delivery_attempt"].as_i64().unwrap_or(1);
    let headers = vec![
        ("Content-Type".to_owned(), "application/json".to_owned()),
        (
            forgebase_queue::push::SIGNATURE_HEADER.to_owned(),
            forgebase_queue::push::sign(secret, unix_now(), &body),
        ),
        ("X-Forgebase-Subscription".to_owned(), sub.to_owned()),
        ("X-Forgebase-Message-Id".to_owned(), id.clone()),
        ("X-Forgebase-Delivery-Attempt".to_owned(), attempt.to_string()),
    ];
    let started = std::time::Instant::now();
    let res = forward(t, &headers, &body).await;
    let ms = started.elapsed().as_millis();
    let ok = matches!(res, Ok(s) if (200..300).contains(&s));
    if json {
        let (status, error) = match &res {
            Ok(s) => (json!(s), Value::Null),
            Err(e) => (Value::Null, json!(e.to_string())),
        };
        writeln!(
            out,
            "{}",
            json!({"message_id": id, "delivery_attempt": attempt, "status": status, "error": error,
                   "latency_ms": ms, "acked": ok})
        )?;
    } else {
        let at = m["message"]["published_at"].as_str().unwrap_or("?");
        match &res {
            Ok(s) => writeln!(
                out,
                "{at}  {id}  attempt {attempt}  -> {s} ({ms} ms)  {}",
                if ok { "acked" } else { "nacked" }
            )?,
            Err(e) => writeln!(out, "{at}  {id}  attempt {attempt}  -> error: {e}  nacked")?,
        }
    }
    out.flush()?;
    Ok(ok)
}

/// `queues listen`: a temporary pull subscription (with a time to live, renewed while running),
/// consumed over the WebSocket streaming pull; each delivery is signed with a session secret,
/// POSTed to the local URL, acked on `2xx` and nacked otherwise. The subscription is deleted on
/// exit (Ctrl-C), and expires on its own if this process dies.
async fn listen(
    c: &Client,
    a: &ListenArgs,
    out: &mut impl Write,
    stop: impl Future<Output = ()>,
) -> anyhow::Result<()> {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message as WsMessage;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    let target = parse_target(&a.forward_to)?;
    let filter = a.filter.as_deref().map(|f| json_arg("--filter", f)).transpose()?;
    let secret = format!(
        "whsec_{}",
        forgebase_auth::crypto::random_token(32).map_err(|e| anyhow::anyhow!("no random secret: {e}"))?
    );
    let tag = forgebase_auth::crypto::random_token(6)
        .map_err(|e| anyhow::anyhow!("no random name: {e}"))?
        .to_ascii_lowercase()
        .replace(['-', '_'], "x");
    let sub = format!("listen-{tag}");
    let ttl_ms = a.ttl.saturating_mul(1000);
    let mut body = json!({
        "name": sub,
        "topic": a.topic,
        "ack_deadline_ms": 60_000,
        "backoff_base_ms": 1_000,
        "backoff_max_ms": 30_000,
        "ttl_ms": ttl_ms,
        "labels": {"forgebase.listen": "true"},
    });
    if let Some(f) = filter {
        body["filter"] = f;
    }
    c.call("POST", "/api/v1/queues/subscriptions", Some(&body)).await?;
    if !a.json {
        writeln!(
            out,
            "Ready! Forwarding topic {} to {} through temporary subscription {sub} (Ctrl-C to stop).",
            a.topic, a.forward_to
        )?;
        writeln!(out, "Session signing secret: {secret}")?;
        writeln!(
            out,
            "Deliveries carry {}: t=<unix seconds>,v1=<hex HMAC-SHA256(secret, \"<t>.<body>\")>",
            forgebase_queue::push::SIGNATURE_HEADER
        )?;
    } else {
        writeln!(
            out,
            "{}",
            json!({"subscription": sub, "secret": secret, "topic": a.topic})
        )?;
    }
    out.flush()?;
    let result = async {
        let url = format!(
            "ws://{}{}?max_messages={}",
            c.addr,
            sub_path(&sub, "/stream"),
            a.max_in_flight
        );
        let mut req = url.into_client_request().context("bad stream URL")?;
        if let Some(t) = &c.token {
            req.headers_mut()
                .insert("authorization", format!("Bearer {t}").parse().context("bad token")?);
        }
        let (mut ws, _) = tokio_tungstenite::connect_async(req)
            .await
            .with_context(|| format!("cannot open the stream of {sub}"))?;
        let renew_every = Duration::from_millis(ttl_ms / 3);
        let mut renew = tokio::time::interval_at(tokio::time::Instant::now() + renew_every, renew_every);
        tokio::pin!(stop);
        loop {
            tokio::select! {
                () = &mut stop => {
                    let _ = ws.send(WsMessage::Close(None)).await;
                    return Ok(());
                }
                _ = renew.tick() => {
                    c.call("PATCH", &sub_path(&sub, ""), Some(&json!({"ttl_ms": ttl_ms}))).await?;
                }
                frame = ws.next() => {
                    let text = match frame {
                        None => bail!("the server closed the stream"),
                        Some(Err(e)) => bail!("stream failed: {e}"),
                        Some(Ok(WsMessage::Text(t))) => t.to_string(),
                        Some(Ok(WsMessage::Close(_))) => bail!("the server closed the stream"),
                        Some(Ok(_)) => continue,
                    };
                    let v: Value = serde_json::from_str(&text).context("bad stream frame")?;
                    match v["type"].as_str() {
                        Some("messages") => {
                            for m in v["messages"].as_array().cloned().unwrap_or_default() {
                                let ok = forward_one(&target, &secret, &sub, &m, out, a.json).await?;
                                let ack_id = m["ack_id"].as_str().unwrap_or_default();
                                let reply = json!({"type": if ok { "ack" } else { "nack" }, "ack_ids": [ack_id]});
                                ws.send(WsMessage::Text(reply.to_string().into())).await?;
                            }
                        }
                        Some("error") => bail!(
                            "{}: {}",
                            v["code"].as_str().unwrap_or("error"),
                            v["message"].as_str().unwrap_or("the stream failed")
                        ),
                        _ => {}
                    }
                }
            }
        }
    }
    .await;
    let cleanup = c.call("DELETE", &sub_path(&sub, ""), None).await;
    match (&cleanup, a.json) {
        (Ok(_), false) => writeln!(out, "Deleted temporary subscription {sub}.")?,
        (Err(e), false) => writeln!(out, "Could not delete {sub} ({e}); it expires on its own.")?,
        _ => {}
    }
    out.flush()?;
    result
}

// -------------------------------------------------------------------- watch

/// What a watcher shows about a job, to tell when something changed.
fn digest(j: &Value) -> String {
    let p = &j["progress"];
    format!(
        "{}|{}|{}|{}|{}|{}|{}",
        j["status"], j["attempts"], p["percent"], p["stage"], p["message"], j["cancel_requested"], j["waiting_for"]
    )
}

fn print_job(out: &mut impl Write, j: &Value, json: bool) -> anyhow::Result<()> {
    if json {
        writeln!(out, "{j}")?;
        return Ok(());
    }
    let status = j["status"].as_str().unwrap_or("?");
    let p = &j["progress"];
    let mut line = format!("{:<9} attempt {}/{}", status, j["attempts"], j["max_attempts"]);
    if let Some(w) = j["waiting_for"].as_i64().filter(|w| *w > 0) {
        line.push_str(&format!("  waiting for {w} parent job(s)"));
    }
    if let Some(pc) = p["percent"].as_f64() {
        line.push_str(&format!("  {pc:.0}%"));
    }
    if let Some(s) = p["stage"].as_str() {
        line.push_str(&format!("  [{s}]"));
    }
    if let Some(m) = p["message"].as_str() {
        line.push_str(&format!("  {m}"));
    }
    if j["cancel_requested"].as_bool() == Some(true) {
        line.push_str("  (cancel requested)");
    }
    writeln!(out, "{line}")?;
    if let Some(e) = j["last_error"].as_str().filter(|_| status == "dead") {
        writeln!(out, "    error: {e}")?;
    }
    Ok(())
}

/// Polls the job until it ends. Exit code: 0 succeeded, 1 dead or cancelled, 2 timed out.
async fn watch(
    c: &Client,
    a: WatchArgs,
    out: &mut impl Write,
    stop: impl Future<Output = ()>,
) -> anyhow::Result<ExitCode> {
    let pause = pause(a.interval)?;
    let path = format!("/api/v1/jobs/{}", enc(&a.id));
    let started = std::time::Instant::now();
    let mut seen = String::new();
    tokio::pin!(stop);
    loop {
        let j = c.call("GET", &path, None).await?;
        let d = digest(&j);
        if d != seen {
            seen = d;
            print_job(out, &j, a.json)?;
            out.flush()?;
        }
        match j["status"].as_str() {
            Some("succeeded") => return Ok(ExitCode::SUCCESS),
            Some("dead" | "cancelled") => return Ok(ExitCode::FAILURE),
            _ => {}
        }
        if a.timeout > 0 && started.elapsed() >= Duration::from_secs(a.timeout) {
            if !a.json {
                writeln!(out, "still running after {}s; gave up watching", a.timeout)?;
            }
            return Ok(ExitCode::from(2));
        }
        tokio::select! {
            () = &mut stop => return Ok(ExitCode::from(130)),
            () = tokio::time::sleep(pause) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testserver::fake_server;

    fn cl(addr: &str) -> Client {
        Client {
            addr: addr.to_owned(),
            token: Some("fbk_test".into()),
            forbidden: FORBIDDEN,
        }
    }

    fn text(out: Vec<u8>) -> String {
        String::from_utf8(out).unwrap()
    }

    fn publish_args(topic: &str, data: Option<&str>) -> PublishArgs {
        PublishArgs {
            topic: topic.into(),
            data: data.map(str::to_owned),
            file: None,
            stdin: false,
            text: false,
            attrs: vec![],
            ordering_key: None,
            dedup_id: None,
            json: false,
        }
    }

    #[test]
    fn data_is_json_or_text_and_attrs_are_pairs() {
        let a = publish_args("t", Some(r#"{"n":1}"#));
        assert_eq!(read_data(&a).unwrap(), json!({"n": 1}));
        let mut a = publish_args("t", Some("hello\n"));
        assert!(read_data(&a).unwrap_err().to_string().contains("not valid JSON"));
        a.text = true;
        assert_eq!(read_data(&a).unwrap(), json!("hello"));
        assert!(read_data(&publish_args("t", None)).is_err());
        assert!(parse_attrs(&["a=b".into(), "c=d=e".into()]).unwrap()["c"] == "d=e");
        assert!(parse_attrs(&["nokey".into()]).is_err());
        assert!(parse_attrs(&["=x".into()]).is_err());
    }

    #[tokio::test]
    async fn publish_sends_one_message_and_reports_the_id() {
        let reply = r#"{"results":[{"message_id":"m1","seq":7,"duplicate":false}]}"#;
        let (addr, seen) = fake_server(vec![(200, reply.into())]).await;
        let mut a = publish_args("orders.new", Some(r#"{"id":1}"#));
        a.attrs = vec!["kind=rush".into()];
        a.ordering_key = Some("o1".into());
        a.dedup_id = Some("d1".into());
        let mut out = Vec::new();
        publish(&cl(&addr), &a, read_data(&a).unwrap(), &mut out).await.unwrap();
        assert_eq!(text(out), "published m1 to orders.new (seq 7)\n");
        let s = seen.lock().unwrap();
        assert_eq!(
            s[0].request_line,
            "POST /api/v1/queues/topics/orders.new/publish HTTP/1.1"
        );
        assert!(s[0].headers.contains("Bearer fbk_test"));
        let body: Value = serde_json::from_str(&s[0].body).unwrap();
        assert_eq!(
            body,
            json!({"messages":[{"data":{"id":1},"attributes":{"kind":"rush"},"ordering_key":"o1","dedup_id":"d1"}]})
        );
    }

    #[tokio::test]
    async fn publish_says_when_a_message_was_a_duplicate_and_surfaces_errors() {
        let dup = r#"{"results":[{"message_id":"m1","seq":7,"duplicate":true}]}"#;
        let (addr, _) = fake_server(vec![(200, dup.into())]).await;
        let a = publish_args("t", Some("1"));
        let mut out = Vec::new();
        publish(&cl(&addr), &a, json!(1), &mut out).await.unwrap();
        assert!(text(out).contains("duplicate"));

        let err = r#"{"error":{"code":"topic_not_found","message":"no such topic"}}"#;
        let (addr, _) = fake_server(vec![(404, err.into())]).await;
        let e = publish(&cl(&addr), &a, json!(1), &mut Vec::new()).await.unwrap_err();
        assert!(
            e.to_string().contains("topic_not_found: no such topic (HTTP 404)"),
            "{e}"
        );

        let denied = r#"{"error":{"code":"forbidden","message":"no"}}"#;
        let (addr, _) = fake_server(vec![(403, denied.into())]).await;
        let e = publish(&cl(&addr), &a, json!(1), &mut Vec::new()).await.unwrap_err();
        assert!(e.to_string().contains("queue or jobs permission"), "{e}");
    }

    fn received() -> String {
        json!({"messages":[{
            "ack_id":"ack-1","delivery_attempt":2,"ack_deadline":"2026-11-10T00:00:30Z",
            "message":{"id":"m9","topic":"t","seq":9,"data":{"x":1},"attributes":{"a":"b"},"ordering_key":"k",
                       "publisher":"p","bytes":5,"published_at":"2026-11-10T00:00:00Z"}
        }]})
        .to_string()
    }

    #[tokio::test]
    async fn pull_prints_and_acks() {
        let (addr, seen) = fake_server(vec![(200, received()), (200, r#"{"count":1,"unknown":[]}"#.into())]).await;
        let a = PullArgs {
            subscription: "workers".into(),
            max: 5,
            wait_ms: 0,
            ack: true,
            nack: false,
            json: false,
        };
        let mut out = Vec::new();
        pull(&cl(&addr), &a, &mut out).await.unwrap();
        let o = text(out);
        assert!(o.contains("seq 9  attempt 2  m9  key k"), "{o}");
        assert!(o.contains(r#"{"x":1}"#) && o.contains("attributes: a=b"), "{o}");
        assert!(o.contains("acked 1 message(s)"), "{o}");
        let s = seen.lock().unwrap();
        assert_eq!(
            s[0].request_line,
            "POST /api/v1/queues/subscriptions/workers/pull HTTP/1.1"
        );
        assert_eq!(
            s[1].request_line,
            "POST /api/v1/queues/subscriptions/workers/ack HTTP/1.1"
        );
        assert_eq!(
            serde_json::from_str::<Value>(&s[1].body).unwrap(),
            json!({"ack_ids":["ack-1"]})
        );
    }

    #[tokio::test]
    async fn pull_without_ack_warns_and_empty_says_so() {
        let (addr, seen) = fake_server(vec![(200, received())]).await;
        let mut a = PullArgs {
            subscription: "w".into(),
            max: 1,
            wait_ms: 0,
            ack: false,
            nack: false,
            json: false,
        };
        let mut out = Vec::new();
        pull(&cl(&addr), &a, &mut out).await.unwrap();
        assert!(text(out).contains("return after the ack deadline"));
        assert_eq!(seen.lock().unwrap().len(), 1);

        let (addr, _) = fake_server(vec![(200, r#"{"messages":[]}"#.into())]).await;
        let mut out = Vec::new();
        pull(&cl(&addr), &a, &mut out).await.unwrap();
        assert_eq!(text(out), "no messages ready on w\n");
        a.json = true;
        let mut out = Vec::new();
        pull(&cl(&addr), &a, &mut out).await.unwrap();
        assert_eq!(text(out), "");
    }

    #[tokio::test]
    async fn pull_nack_uses_the_nack_route() {
        let (addr, seen) = fake_server(vec![(200, received()), (200, r#"{"count":1,"unknown":["z"]}"#.into())]).await;
        let a = PullArgs {
            subscription: "w".into(),
            max: 1,
            wait_ms: 0,
            ack: false,
            nack: true,
            json: false,
        };
        let mut out = Vec::new();
        pull(&cl(&addr), &a, &mut out).await.unwrap();
        assert!(text(out).contains("nacked 1 message(s), 1 ack id(s) no longer valid"));
        assert!(seen.lock().unwrap()[1].request_line.contains("/nack"));
    }

    fn page(seqs: &[i64], next: Option<i64>) -> String {
        let items: Vec<Value> = seqs
            .iter()
            .map(|s| json!({"id":format!("m{s}"),"seq":s,"data":{"n":s},"published_at":"t"}))
            .collect();
        json!({"items":items,"next_after":next}).to_string()
    }

    fn tail_args(n: usize, follow: bool) -> TailArgs {
        TailArgs {
            topic: "ev".into(),
            lines: n,
            follow,
            interval: 0.05,
            json: false,
        }
    }

    #[tokio::test]
    async fn tail_shows_the_newest_lines_across_pages() {
        let (addr, seen) = fake_server(vec![(200, page(&[1, 2, 3], Some(3))), (200, page(&[4, 5], None))]).await;
        let mut out = Vec::new();
        tail(&cl(&addr), tail_args(2, false), &mut out, std::future::pending())
            .await
            .unwrap();
        let o = text(out);
        assert!(
            !o.contains("seq 3") && o.contains("seq 4") && o.contains("seq 5"),
            "{o}"
        );
        let s = seen.lock().unwrap();
        assert!(s[0].request_line.contains("/messages?after=0&limit=500"));
        assert!(s[1].request_line.contains("after=3"));
    }

    #[tokio::test]
    async fn tail_follow_prints_only_new_messages_until_stopped() {
        let (addr, seen) = fake_server(vec![
            (200, page(&[1], None)),
            (200, page(&[], None)),
            (200, page(&[2], None)),
            (200, page(&[], None)),
        ])
        .await;
        let mut out = Vec::new();
        let stop = async { tokio::time::sleep(Duration::from_millis(400)).await };
        tail(&cl(&addr), tail_args(5, true), &mut out, stop).await.unwrap();
        let o = text(out);
        assert_eq!(o.matches("seq 1").count(), 1, "{o}");
        assert_eq!(o.matches("seq 2").count(), 1, "{o}");
        let s = seen.lock().unwrap();
        assert!(s[1].request_line.contains("after=1"), "{}", s[1].request_line);
        assert!(s.iter().any(|r| r.request_line.contains("after=2")));
    }

    #[test]
    fn rejects_bad_intervals() {
        assert!(pause(0.0).is_err());
        assert!(pause(f64::NAN).is_err());
        assert_eq!(pause(0.5).unwrap(), Duration::from_millis(500));
    }

    fn job(status: &str, pct: Option<f64>, stage: &str) -> String {
        json!({"id":"j1","status":status,"attempts":1,"max_attempts":3,"waiting_for":0,"cancel_requested":false,
               "progress": pct.map(|p| json!({"percent":p,"stage":stage,"message":"working","updated_at":"t"})),
               "last_error": if status == "dead" { json!("boom") } else { Value::Null }})
        .to_string()
    }

    fn watch_args(timeout: u64) -> WatchArgs {
        WatchArgs {
            id: "j1".into(),
            interval: 0.05,
            timeout,
            json: false,
        }
    }

    #[tokio::test]
    async fn watch_prints_changes_once_and_exits_zero_on_success() {
        let (addr, seen) = fake_server(vec![
            (200, job("queued", None, "")),
            (200, job("running", Some(10.0), "decode")),
            (200, job("running", Some(10.0), "decode")),
            (200, job("running", Some(60.0), "encode")),
            (200, job("succeeded", Some(100.0), "encode")),
        ])
        .await;
        let mut out = Vec::new();
        let code = watch(&cl(&addr), watch_args(0), &mut out, std::future::pending())
            .await
            .unwrap();
        assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::SUCCESS));
        let o = text(out);
        assert_eq!(o.lines().count(), 4, "{o}");
        assert!(o.contains("running   attempt 1/3  10%  [decode]  working"), "{o}");
        assert!(o.lines().last().unwrap().starts_with("succeeded"), "{o}");
        assert_eq!(seen.lock().unwrap()[0].request_line, "GET /api/v1/jobs/j1 HTTP/1.1");
    }

    #[tokio::test]
    async fn watch_fails_on_a_dead_job_and_times_out_on_a_slow_one() {
        let (addr, _) = fake_server(vec![(200, job("dead", Some(30.0), "x"))]).await;
        let mut out = Vec::new();
        let code = watch(&cl(&addr), watch_args(0), &mut out, std::future::pending())
            .await
            .unwrap();
        assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::FAILURE));
        assert!(text(out).contains("error: boom"));

        let (addr, _) = fake_server(vec![(200, job("running", Some(1.0), "x"))]).await;
        let mut out = Vec::new();
        let mut a = watch_args(1);
        a.interval = 0.2;
        let code = watch(&cl(&addr), a, &mut out, std::future::pending()).await.unwrap();
        assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::from(2)));
        assert!(text(out).contains("gave up watching"));
    }

    #[tokio::test]
    async fn watch_reports_a_missing_job() {
        let err = r#"{"error":{"code":"job_not_found","message":"no such job"}}"#;
        let (addr, _) = fake_server(vec![(404, err.into())]).await;
        let e = watch(&cl(&addr), watch_args(0), &mut Vec::new(), std::future::pending())
            .await
            .unwrap_err();
        assert!(e.to_string().contains("job_not_found"));
    }

    // ------------------------------------------------------- webhook tooling

    #[test]
    fn forward_targets_are_plain_http() {
        assert_eq!(
            parse_target("http://localhost:3000/webhooks?x=1").unwrap(),
            Target {
                addr: "localhost:3000".into(),
                host: "localhost:3000".into(),
                path: "/webhooks?x=1".into(),
            }
        );
        assert_eq!(parse_target("http://127.0.0.1").unwrap().addr, "127.0.0.1:80");
        assert_eq!(parse_target("http://[::1]:8080/h").unwrap().addr, "[::1]:8080");
        assert!(
            parse_target("https://example.com/h")
                .unwrap_err()
                .to_string()
                .contains("http://")
        );
        assert!(parse_target("localhost:3000").is_err());
        assert!(parse_target("http://user@host/").is_err());
    }

    #[tokio::test]
    async fn thin_commands_call_their_routes() {
        let rotated =
            r#"{"subscription":"hook","push_secret":"whsec_new","previous_expires_at":"2026-11-15T00:00:00Z"}"#;
        let redriven = r#"{"redriven":[1,2],"already_redriven":[3],"not_found":[],"remaining":4}"#;
        let attempt = r#"{"items":[{"id":7,"at":"t","message_id":"m1","attempt":2,"outcome":"failure","status":500,
            "latency_ms":12,"test":false,"error_class":"http_status","error":"the endpoint answered HTTP 500",
            "response_body":"oops","response_truncated":true}]}"#;
        let (addr, seen) = fake_server(vec![
            (200, rotated.into()),
            (200, redriven.into()),
            (200, attempt.into()),
            (200, r#"{"count":3}"#.into()),
            (200, r#"{"paused_reason":"deploy"}"#.into()),
            (200, "{}".into()),
        ])
        .await;
        let c = cl(&addr);
        let mut out = Vec::new();
        rotate_secret(
            &c,
            &RotateSecretArgs {
                subscription: "hook".into(),
                grace: 60,
                expire_previous: false,
                json: false,
            },
            &mut out,
        )
        .await
        .unwrap();
        redrive(
            &c,
            &RedriveArgs {
                subscription: "hook".into(),
                ids: vec![],
                all: true,
                since: Some("2026-11-01T00:00:00Z".into()),
                filter: Some(r#"{"kind":"a"}"#.into()),
                limit: Some(2),
                json: false,
            },
            &mut out,
        )
        .await
        .unwrap();
        attempts(
            &c,
            &AttemptsArgs {
                subscription: "hook".into(),
                outcome: Some("failure".into()),
                message_id: None,
                since: None,
                tests: false,
                limit: 5,
                json: false,
            },
            &mut out,
        )
        .await
        .unwrap();
        retry(
            &c,
            &RetryArgs {
                subscription: "hook".into(),
                message_ids: vec!["m1".into()],
                json: false,
            },
            &mut out,
        )
        .await
        .unwrap();
        pause_sub(
            &c,
            &PauseArgs {
                subscription: "hook".into(),
                reason: Some("deploy".into()),
                json: false,
            },
            &mut out,
        )
        .await
        .unwrap();
        resume_sub(
            &c,
            &SubArgs {
                subscription: "hook".into(),
                json: false,
            },
            &mut out,
        )
        .await
        .unwrap();
        let o = text(out);
        assert!(o.contains("whsec_new") && o.contains("both signatures"), "{o}");
        assert!(
            o.contains("redrove 2 dead letter(s)") && o.contains("4 more match"),
            "{o}"
        );
        assert!(o.contains("#7  t  m1  attempt 2  failure  HTTP 500  12 ms"), "{o}");
        assert!(
            o.contains("http_status: the endpoint answered HTTP 500") && o.contains("response: oops …"),
            "{o}"
        );
        assert!(o.contains("3 delivery(ies) of hook retry now"), "{o}");
        assert!(o.contains("paused hook (deploy)") && o.contains("resumed hook"), "{o}");
        let s = seen.lock().unwrap();
        let lines: Vec<&str> = s.iter().map(|r| r.request_line.as_str()).collect();
        assert_eq!(
            lines,
            vec![
                "POST /api/v1/queues/subscriptions/hook/secret/rotate HTTP/1.1",
                "POST /api/v1/queues/subscriptions/hook/redrive HTTP/1.1",
                "GET /api/v1/queues/subscriptions/hook/attempts?limit=5&outcome=failure HTTP/1.1",
                "POST /api/v1/queues/subscriptions/hook/retry HTTP/1.1",
                "POST /api/v1/queues/subscriptions/hook/pause HTTP/1.1",
                "POST /api/v1/queues/subscriptions/hook/resume HTTP/1.1",
            ]
        );
        assert_eq!(
            serde_json::from_str::<Value>(&s[0].body).unwrap(),
            json!({"grace_ms": 60000})
        );
        assert_eq!(
            serde_json::from_str::<Value>(&s[1].body).unwrap(),
            json!({"all": true, "since": "2026-11-01T00:00:00Z", "filter": {"kind": "a"}, "limit": 2})
        );
        assert_eq!(
            serde_json::from_str::<Value>(&s[3].body).unwrap(),
            json!({"message_ids": ["m1"]})
        );
    }

    #[tokio::test]
    async fn redrive_needs_ids_or_all_and_test_send_takes_json_data() {
        let (addr, seen) = fake_server(vec![(
            200,
            r#"{"id":1,"at":"t","message_id":"test-1","attempt":1,"outcome":"success","status":204,"latency_ms":3,"test":true}"#
                .into(),
        )])
        .await;
        let c = cl(&addr);
        let e = redrive(
            &c,
            &RedriveArgs {
                subscription: "hook".into(),
                ids: vec![],
                all: false,
                since: None,
                filter: None,
                limit: None,
                json: false,
            },
            &mut Vec::new(),
        )
        .await
        .unwrap_err();
        assert!(e.to_string().contains("--id"));
        let mut out = Vec::new();
        test_send(
            &c,
            &TestArgs {
                subscription: "hook".into(),
                data: Some(r#"{"hello":1}"#.into()),
                json: false,
            },
            &mut out,
        )
        .await
        .unwrap();
        assert!(text(out).contains("test-1  attempt 1  success  HTTP 204  3 ms  (test)"));
        {
            let s = seen.lock().unwrap();
            assert_eq!(
                s[0].request_line,
                "POST /api/v1/queues/subscriptions/hook/test HTTP/1.1"
            );
            assert_eq!(
                serde_json::from_str::<Value>(&s[0].body).unwrap(),
                json!({"data": {"hello": 1}})
            );
        }
        assert!(
            test_send(
                &c,
                &TestArgs {
                    subscription: "hook".into(),
                    data: Some("{nope".into()),
                    json: false
                },
                &mut Vec::new()
            )
            .await
            .is_err()
        );
    }

    /// `listen` against a real queue service (SQLite, the HTTP API with the
    /// WebSocket stream) and a local receiver that fails once.
    mod listen_e2e {
        use std::sync::Arc;

        use axum::extract::Request;
        use axum::http::HeaderMap;
        use axum::middleware::Next;
        use forgebase_data::{AccessContext, DataService};
        use forgebase_db::{Backend, Database, DbOptions, Statement, TxMode};
        use forgebase_queue::{NewMessage, NewTopic, QueueService, QueueSettings};

        use super::*;

        struct Server {
            svc: QueueService,
            addr: String,
            _dir: tempfile::TempDir,
        }

        fn root() -> AccessContext {
            AccessContext::admin("root")
        }

        async fn server() -> Server {
            let dir = tempfile::tempdir().unwrap();
            let layout = forgebase_core::DataLayout::new(dir.path().join("data"));
            layout.ensure().unwrap();
            let cfg = forgebase_core::Config::defaults(dir.path());
            let db = Database::open(DbOptions::from_config(&cfg.database, layout.database_path()))
                .await
                .unwrap();
            db.migrate(Some(&layout.backups_dir())).await.unwrap();
            let mut tx = db.begin(TxMode::Normal).await.unwrap();
            tx.execute(&Statement::raw(
                "INSERT INTO _forgebase_admins (id, email, password_hash, created_at, updated_at) \
                 VALUES ('root', 'root@example.com', '$argon2id$test', 0, 0)",
            ))
            .await
            .unwrap();
            tx.commit().await.unwrap();
            let backend: Arc<dyn Backend> = Arc::new(db.clone());
            let policy = forgebase_policy::PolicyService::open(backend.clone()).await.unwrap();
            let data = DataService::new(backend.clone(), policy.authorizer()).await.unwrap();
            policy.attach_data(data.clone());
            let svc = QueueService::new(
                data.clone(),
                forgebase_auth::crypto::Keyring::from_master(b"cli-listen-test-master-key-0123456"),
                QueueSettings {
                    poll_interval: Duration::from_millis(50),
                    ..QueueSettings::default()
                },
            );
            let state = forgebase_api::AppState::from_data(db.clone(), layout, data);
            let app = forgebase_api::router(
                state,
                forgebase_api::RouterOptions {
                    extensions: vec![forgebase_queue::extension(svc.clone())],
                    ..forgebase_api::RouterOptions::default()
                },
            )
            .layer(axum::middleware::from_fn(|mut req: Request, next: Next| async move {
                req.extensions_mut().insert(root());
                next.run(req).await
            }));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap().to_string();
            tokio::spawn(async move {
                axum::serve(
                    listener,
                    app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
                )
                .await
                .unwrap();
            });
            Server { svc, addr, _dir: dir }
        }

        type Calls = Arc<std::sync::Mutex<Vec<(HeaderMap, Vec<u8>)>>>;

        /// Answers 500 to the first request, 204 to the rest.
        async fn receiver() -> (String, Calls) {
            let calls: Calls = Arc::default();
            let log = calls.clone();
            let app = axum::Router::new().route(
                "/hook",
                axum::routing::post(move |headers: HeaderMap, body: axum::body::Bytes| {
                    let log = log.clone();
                    async move {
                        let mut l = log.lock().unwrap();
                        l.push((headers, body.to_vec()));
                        if l.len() == 1 {
                            axum::http::StatusCode::INTERNAL_SERVER_ERROR
                        } else {
                            axum::http::StatusCode::NO_CONTENT
                        }
                    }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap().to_string();
            tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            (addr, calls)
        }

        async fn eventually<T, F, Fut>(mut check: F) -> T
        where
            F: FnMut() -> Fut,
            Fut: Future<Output = Option<T>>,
        {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
            loop {
                if let Some(v) = check().await {
                    return v;
                }
                assert!(tokio::time::Instant::now() < deadline, "timed out");
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        }

        #[tokio::test]
        async fn listen_forwards_signed_deliveries_acks_2xx_and_cleans_up() {
            let srv = server().await;
            srv.svc
                .create_topic(
                    &root(),
                    NewTopic {
                        name: "orders".into(),
                        ..NewTopic::default()
                    },
                )
                .await
                .unwrap();
            let (hook, calls) = receiver().await;
            let c = Client {
                addr: srv.addr.clone(),
                token: None,
                forbidden: FORBIDDEN,
            };
            let args = ListenArgs {
                topic: "orders".into(),
                forward_to: format!("http://{hook}/hook"),
                filter: Some(r#"{"kind":"rush"}"#.into()),
                ttl: 600,
                max_in_flight: 10,
                json: false,
            };
            let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
            let svc = srv.svc.clone();
            let driver = async {
                let sub = eventually(|| async {
                    svc.subscriptions(&root(), Some("orders"))
                        .await
                        .unwrap()
                        .into_iter()
                        .next()
                })
                .await;
                assert!(sub.name.starts_with("listen-"), "{}", sub.name);
                assert!(sub.expires_at.is_some(), "a crash cannot leak it");
                assert_eq!(sub.filter, Some(json!({"kind": "rush"})));
                svc.publish(
                    &root(),
                    "orders",
                    vec![
                        NewMessage::new(json!({"n": 1})).attr("kind", "rush"),
                        NewMessage::new(json!({"n": 2})).attr("kind", "slow"),
                    ],
                )
                .await
                .unwrap();
                // 500 (nacked), redelivered after the 1 s backoff, 204 (acked).
                let m = eventually(|| async {
                    let m = svc.subscription_metrics(&root(), &sub.name).await.unwrap();
                    (m.acked_total == 1).then_some(m)
                })
                .await;
                assert_eq!((m.nacked_total, m.backlog_messages), (1, 0));
                stop_tx.send(()).unwrap();
                sub.name
            };
            let mut out = Vec::new();
            let (res, name) = tokio::join!(
                listen(&c, &args, &mut out, async {
                    let _ = stop_rx.await;
                }),
                driver
            );
            res.unwrap();
            let o = text(out);
            let secret = o
                .lines()
                .find_map(|l| l.strip_prefix("Session signing secret: "))
                .expect("the session secret is printed")
                .to_owned();
            assert!(secret.starts_with("whsec_"));
            let calls = calls.lock().unwrap().clone();
            assert_eq!(calls.len(), 2, "the filtered-out message is not forwarded");
            for (i, (headers, body)) in calls.iter().enumerate() {
                let sig = headers[forgebase_queue::push::SIGNATURE_HEADER].to_str().unwrap();
                assert!(
                    forgebase_queue::push::verify(&secret, sig, body, unix_now(), 300),
                    "signature of delivery {i}"
                );
                let b: Value = serde_json::from_slice(body).unwrap();
                assert_eq!(b["subscription"], name);
                assert_eq!(b["delivery_attempt"], i + 1);
                assert_eq!(b["message"]["data"], json!({"n": 1}));
                assert_eq!(headers["x-forgebase-message-id"], b["message"]["id"].as_str().unwrap());
            }
            assert!(o.contains("attempt 1  -> 500") && o.contains("nacked"), "{o}");
            assert!(o.contains("attempt 2  -> 204") && o.contains("acked"), "{o}");
            assert!(o.contains(&format!("Deleted temporary subscription {name}.")), "{o}");
            assert!(
                srv.svc.subscriptions(&root(), Some("orders")).await.unwrap().is_empty(),
                "deleted on exit"
            );
        }
    }
}
