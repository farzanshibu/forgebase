//! `forgebase queues publish|pull|tail` and `forgebase jobs watch <ID>`: talk to a running server's
//! durable topics (`/api/v1/queues`, ADR-0022) and long-running tasks (`/api/v1/jobs`, ADR-0023).
//!
//! Connection and credentials work as for `forgebase functions` (`--server` / `FB_URL`, `--token` /
//! `FB_TOKEN`, plain `http://` only). Streaming is done by polling: `tail` reads the topic's retained
//! messages after the last sequence number it printed (it never takes deliveries from a subscription),
//! and `jobs watch` polls the job. No server route is added for either.

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
}
