//! `forgebase dev`: the local emulator (ADR-0038).
//!
//! One command starts a server on a throw-away data directory, creates an
//! administrator and an API key, applies the project directory (ADR-0037)
//! the way `forgebase project apply` does but without a prompt, optionally
//! loads the seed fixtures, and serves until Ctrl-C. `--watch` applies again
//! when project files change. `forgebase dev seed` and `forgebase dev reset`
//! work on a running emulator.

mod emulator;
mod http;
mod seed;
mod state;
mod watch;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, bail};
use clap::{Args, Subcommand};
use forgebase_server::ShutdownSignals;
use serde_json::{Value, json};

use crate::GlobalArgs;
use crate::project::remote::{Connection, Remote};
use crate::project::{self, apply, layout, plan};
use emulator::{Emulator, Settings};
use http::Api;

/// Where an emulator is not found, said the same way everywhere.
const NOT_RUNNING: &str =
    "no running dev emulator found for this project: start one with `forgebase dev`, or pass --server and --token";

/// `forgebase dev`.
#[derive(Debug, Args)]
pub struct DevCli {
    #[command(flatten)]
    args: DevArgs,
    #[command(subcommand)]
    command: Option<DevCommand>,
}

/// Flags of `forgebase dev` and its subcommands.
#[derive(Debug, Clone, Args)]
pub struct DevArgs {
    /// The project directory (ADR-0037 layout).
    #[arg(long, global = true, value_name = "DIR", default_value = "forgebase")]
    dir: PathBuf,
    /// Database engine of a new data directory: sqlite or postgres [default: this build's default; a persisted directory keeps its own].
    #[arg(long, global = true, value_name = "ENGINE", value_parser = ["sqlite", "postgres"])]
    database: Option<String>,
    /// Keep the data in DIR instead of a directory deleted on exit (the generated credentials are kept there too).
    #[arg(long, global = true, value_name = "DIR")]
    persist: Option<PathBuf>,
    /// Load seed/<collection>.jsonl fixtures after applying the project (idempotent by record id).
    #[arg(long, global = true)]
    seed: bool,
    /// Apply the project again when its files change (destructive changes are allowed in dev).
    #[arg(long, global = true)]
    watch: bool,
    /// Print one JSON line when the emulator is ready (progress goes to standard error).
    #[arg(long, global = true)]
    json: bool,
}

#[derive(Debug, Subcommand)]
enum DevCommand {
    /// Load the project's seed fixtures into a running emulator (idempotent by record id).
    Seed(TargetArgs),
    /// Delete every record of the project's collections in a running emulator and load the seed fixtures again.
    /// With `--persist DIR` and no emulator running, wipes the persisted data directory instead.
    Reset(TargetArgs),
}

/// Which emulator a subcommand talks to.
#[derive(Debug, Clone, Args)]
struct TargetArgs {
    /// Server base URL [default: the emulator running for --dir]. Never read from FORGEBASE_URL.
    #[arg(long, value_name = "URL")]
    server: Option<String>,
    /// API key of the server [default: the emulator's dev key]. Never read from FORGEBASE_TOKEN.
    #[arg(long, value_name = "KEY")]
    token: Option<String>,
}

/// Runs `forgebase dev [seed|reset]`.
pub async fn run(g: &GlobalArgs, cli: DevCli) -> anyhow::Result<ExitCode> {
    match cli.command {
        // The emulator sets up logging and trace context itself once it has its configuration.
        None => serve(g, &cli.args).await,
        Some(DevCommand::Seed(t)) => {
            crate::commands::quiet_logging(g)?;
            seed_running(&cli.args, &t).await
        }
        Some(DevCommand::Reset(t)) => {
            crate::commands::quiet_logging(g)?;
            reset(&cli.args, &t).await
        }
    }
}

/// Progress lines: standard output, or standard error when standard output carries JSON.
struct Say {
    json: bool,
}

impl Say {
    fn line(&self, text: impl std::fmt::Display) {
        if self.json {
            eprintln!("{text}");
        } else {
            println!("{text}");
        }
    }

    fn sink(&self) -> Box<dyn Write> {
        if self.json {
            Box::new(std::io::stderr())
        } else {
            Box::new(std::io::stdout())
        }
    }
}

/// What applying the project did.
#[derive(Debug, Default)]
struct Synced {
    applied: usize,
    failed: Vec<String>,
}

/// Plans and applies the project directory to the emulator: no prompt, deletions
/// and destructive changes allowed (the data of an emulator is disposable).
async fn sync(g: &GlobalArgs, em: &Emulator, dir: &Path, say: &Say) -> anyhow::Result<Synced> {
    let project = layout::load(dir)?;
    project::warn(&project);
    let mut out = say.sink();
    project::run_builds(&project, &mut out)?;
    // The build may have written the entry files the project reads.
    let project = layout::load(dir)?;
    let remote = Remote::connect(
        &Connection {
            server: Some(em.url.clone()),
            token: Some(em.admin_token().await?),
            ..Connection::default()
        },
        &layout::Target::default(),
        g,
    )
    .await?;
    let plan = project::make_plan(&project, &remote, true).await?;
    for (function, key, var) in unset_secrets(&project) {
        eprintln!(
            "warning: {var} is not set: secret {key} of function {function} gets a placeholder value in the emulator (export {var} or put it in .env to use a real one)"
        );
    }
    if !plan.has_changes() {
        say.line("project: no changes");
        return Ok(Synced::default());
    }
    plan::render(&plan, &mut out)?;
    if plan.changes.iter().any(|c| !c.destructive.is_empty()) {
        say.line(
            "dev: destructive changes are allowed here: the data they drop is gone (the emulator's data is disposable)",
        );
    }
    let outcome = apply::execute(
        &remote,
        &project,
        &plan,
        &apply::Options {
            allow_destructive: true,
            // A secret whose variable is not set gets a stand-in: a missing key must not stop the emulator.
            placeholder_secret: Some("forgebase-dev-placeholder".into()),
            ..apply::Options::default()
        },
        &mut out,
    )
    .await?;
    let failed: Vec<String> = outcome.failed.iter().map(|(id, e)| format!("{id}: {e}")).collect();
    say.line(format!(
        "project: {} changed, {} already in place, {} failed, {} skipped",
        outcome.applied,
        outcome.in_place,
        failed.len(),
        outcome.skipped
    ));
    Ok(Synced {
        applied: outcome.applied,
        failed,
    })
}

/// `(function, secret, variable)` of every function secret whose environment variable is not set.
fn unset_secrets(project: &layout::Project) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for r in &project.resources {
        let crate::project::model::Extra::Function(f) = &r.extra else {
            continue;
        };
        for (key, var) in &f.secrets {
            if std::env::var_os(var).is_none() {
                out.push((r.name.clone(), key.clone(), var.clone()));
            }
        }
    }
    out
}

async fn seed_all(api: &Api, dir: &Path, say: &Say) -> anyhow::Result<seed::Report> {
    let report = seed::load_dir(api, dir).await?;
    if report.collections.is_empty() {
        say.line(format!("seed: no seed/*.jsonl files in {}", dir.display()));
    } else {
        report.render(&mut say.sink())?;
    }
    Ok(report)
}

fn ready_json(
    em: &Emulator,
    args: &DevArgs,
    has_project: bool,
    synced: &Synced,
    seeded: Option<&seed::Report>,
) -> Value {
    json!({
        "event": "ready",
        "url": em.url,
        "port": em.addr.rsplit(':').next().and_then(|p| p.parse::<u16>().ok()),
        "admin": {"email": em.creds.admin_email, "password": em.creds.admin_password},
        "api_key": em.creds.api_key,
        "database": em.database,
        "data_dir": em.root.join("data").display().to_string(),
        "persist": !em.ephemeral,
        "project": has_project.then(|| args.dir.display().to_string()),
        "applied": synced.applied,
        "seed": seeded.map(|r| json!({"collections": r.collections, "errors": r.errors})),
        "pid": std::process::id(),
    })
}

async fn serve(g: &GlobalArgs, args: &DevArgs) -> anyhow::Result<ExitCode> {
    let say = Say { json: args.json };
    let has_project = args.dir.join(layout::PROJECT_FILE).is_file();
    if args.watch && !has_project {
        bail!(
            "--watch needs a project: {} has no {} (create one with `forgebase new`, or pass --dir)",
            args.dir.display(),
            layout::PROJECT_FILE
        );
    }
    if args.seed && !has_project {
        bail!(
            "--seed needs a project: {} has no {}",
            args.dir.display(),
            layout::PROJECT_FILE
        );
    }
    // Validate before starting anything: a typo in a file is reported with file and line at once.
    if has_project {
        let p = layout::load(&args.dir)?;
        project::warn(&p);
    } else {
        eprintln!(
            "warning: no project at {} ({} is missing): starting an empty emulator",
            args.dir.display(),
            layout::PROJECT_FILE
        );
    }
    let mut signals = ShutdownSignals::install();
    let em = Emulator::start(
        g,
        &Settings {
            database: args.database.clone(),
            persist: args.persist.clone(),
        },
    )
    .await?;
    let registry = state::register(
        &args.dir,
        &state::Running {
            pid: std::process::id(),
            url: em.url.clone(),
            project_dir: args.dir.display().to_string(),
            creds: em.creds.clone(),
        },
    )
    .map_err(|e| eprintln!("warning: dev seed/reset will not find this emulator: {e:#}"))
    .ok();

    let result = drive(g, args, &em, &say, has_project, &mut signals).await;
    if let Some(p) = &registry {
        state::unregister(p);
    }
    say.line(format!(
        "stopping{}",
        if em.ephemeral {
            " (the ephemeral data directory is deleted)"
        } else {
            ""
        }
    ));
    let stopped = em.stop().await;
    match (result, stopped) {
        (Err(e), _) => Err(e),
        (Ok(code), Ok(())) => Ok(code),
        (Ok(_), Err(e)) => Err(e),
    }
}

async fn drive(
    g: &GlobalArgs,
    args: &DevArgs,
    em: &Emulator,
    say: &Say,
    has_project: bool,
    signals: &mut ShutdownSignals,
) -> anyhow::Result<ExitCode> {
    let mut synced = Synced::default();
    if has_project {
        synced = match sync(g, em, &args.dir, say).await {
            Ok(s) => s,
            // A broken project stops a plain `dev`; with --watch the next save may fix it.
            Err(e) if args.watch => {
                eprintln!("error: {e:#}");
                Synced::default()
            }
            Err(e) => return Err(e),
        };
        if !synced.failed.is_empty() && !args.watch {
            for f in &synced.failed {
                eprintln!("error: {f}");
            }
            bail!("the project did not apply completely");
        }
    }
    let mut seeded = None;
    if args.seed {
        let report = seed_all(&em.api(), &args.dir, say).await?;
        if !report.ok() && !args.watch {
            bail!("{} seed record(s) failed", report.errors.len());
        }
        seeded = Some(report);
    }
    if args.json {
        println!("{}", ready_json(em, args, has_project, &synced, seeded.as_ref()));
        std::io::stdout().flush()?;
    } else {
        println!();
        println!("ForgeBase dev emulator ready");
        println!("  url:        {}", em.url);
        println!("  dashboard:  {}/admin/", em.url);
        println!("  admin:      {} / {}", em.creds.admin_email, em.creds.admin_password);
        println!("  api key:    {}", em.creds.api_key.as_deref().unwrap_or(""));
        println!(
            "  database:   {} ({})",
            em.database,
            if em.ephemeral {
                "ephemeral: deleted on exit".to_owned()
            } else {
                format!("kept in {}", em.root.display())
            }
        );
        if has_project {
            println!(
                "  project:    {}{}",
                args.dir.display(),
                if args.watch { " (watching for changes)" } else { "" }
            );
        }
        println!("Press Ctrl-C to stop.");
        std::io::stdout().flush()?;
    }
    let mut watcher = args.watch.then(|| watch::Watcher::new(&args.dir));
    loop {
        let change = async {
            match watcher.as_mut() {
                Some(w) => w.next_change().await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            () = signals.recv() => break,
            changed = change => {
                say.line(format!("change detected: {}", changed.join(", ")));
                let outcome = sync(g, em, &args.dir, say).await;
                let (applied, failed) = match &outcome {
                    Ok(s) => (s.applied, s.failed.clone()),
                    Err(e) => {
                        eprintln!("error: {e:#}");
                        (0, vec![format!("{e:#}")])
                    }
                };
                let mut seed_errors = Vec::new();
                if args.seed && outcome.is_ok() {
                    match seed_all(&em.api(), &args.dir, say).await {
                        Ok(r) => seed_errors = r.errors,
                        Err(e) => seed_errors.push(format!("{e:#}")),
                    }
                }
                if let Some(w) = watcher.as_mut() {
                    // Files the apply wrote itself (a function build) are not a change.
                    w.rebase();
                }
                if args.json {
                    println!("{}", json!({"event": "reapplied", "changed": changed, "applied": applied, "failed": failed, "seed_errors": seed_errors}));
                    std::io::stdout().flush()?;
                }
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// The emulator a `seed` / `reset` invocation talks to: the flags, else the registry.
fn resolve(args: &DevArgs, t: &TargetArgs) -> anyhow::Result<Option<(String, Api)>> {
    fn api_for(base: &str, token: Option<String>) -> anyhow::Result<Api> {
        let u = url::Url::parse(base)?;
        if u.scheme() != "http" {
            bail!("the dev commands speak plain http:// to a local emulator");
        }
        let addr = crate::probe::dial_addr(
            u.host_str().unwrap_or("127.0.0.1").trim_matches(['[', ']']),
            u.port_or_known_default().unwrap_or(80),
        );
        Ok(Api::new(addr, token))
    }
    if let Some(server) = &t.server {
        let base = project::remote::normalize_base(server)?;
        let Some(token) = t.token.clone() else {
            bail!("--server needs --token (an API key of the server)");
        };
        return Ok(Some((base.clone(), api_for(&base, Some(token))?)));
    }
    let Some(running) = state::lookup(&args.dir) else {
        return Ok(None);
    };
    let token = t.token.clone().or(running.creds.api_key);
    Ok(Some((running.url.clone(), api_for(&running.url, token)?)))
}

async fn seed_running(args: &DevArgs, t: &TargetArgs) -> anyhow::Result<ExitCode> {
    let say = Say { json: args.json };
    let (url, api) = resolve(args, t)?.ok_or_else(|| anyhow::anyhow!(NOT_RUNNING))?;
    if !crate::probe::is_forgebase(&api.addr).await {
        bail!("nothing answers at {url}: is the dev emulator still running?");
    }
    let report = seed::load_dir(&api, &args.dir).await?;
    finish_seed(&say, &report)
}

fn finish_seed(say: &Say, report: &seed::Report) -> anyhow::Result<ExitCode> {
    if say.json {
        println!(
            "{}",
            json!({"event": "seed", "total": report.total(), "collections": report.collections, "errors": report.errors})
        );
    } else if report.collections.is_empty() {
        println!("no seed/*.jsonl files");
    } else {
        report.render(&mut std::io::stdout())?;
        let t = report.total();
        println!(
            "seeded: {} created, {} updated, {} unchanged, {} failed",
            t.created, t.updated, t.unchanged, t.failed
        );
    }
    Ok(if report.ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

async fn reset(args: &DevArgs, t: &TargetArgs) -> anyhow::Result<ExitCode> {
    let say = Say { json: args.json };
    match resolve(args, t)? {
        Some((url, api)) => {
            if !crate::probe::is_forgebase(&api.addr).await {
                bail!("nothing answers at {url}: is the dev emulator still running?");
            }
            if !is_loopback(&api.addr) {
                bail!("dev reset deletes records: it only runs against a server on this machine, not {url}");
            }
            let project = layout::load(&args.dir)?;
            let deleted = seed::wipe(&api, &project).await?;
            say.line(format!("reset: deleted {deleted} record(s)"));
            let report = seed::load(&api, &project).await?;
            finish_seed(&say, &report)
        }
        None => match &args.persist {
            Some(dir) => {
                wipe_persisted(dir)?;
                say.line(format!(
                    "reset: wiped {}; the next `forgebase dev --persist {} --seed` starts from the project and its fixtures",
                    dir.display(),
                    dir.display()
                ));
                Ok(ExitCode::SUCCESS)
            }
            None => bail!("{NOT_RUNNING}; `dev reset --persist DIR` wipes a stopped emulator's directory"),
        },
    }
}

fn is_loopback(addr: &str) -> bool {
    let host = addr.rsplit_once(':').map_or(addr, |(h, _)| h).trim_matches(['[', ']']);
    host == "localhost" || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// Empties a persisted dev directory, after checking it is one.
fn wipe_persisted(dir: &Path) -> anyhow::Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    if !dir.join(state::CREDENTIALS_FILE).is_file() && !dir.join("data").is_dir() {
        bail!(
            "{} does not look like a dev data directory (no {} and no data/): not deleting it",
            dir.display(),
            state::CREDENTIALS_FILE
        );
    }
    for e in std::fs::read_dir(dir).with_context(|| format!("cannot read {}", dir.display()))? {
        let p = e?.path();
        if p.is_dir() {
            std::fs::remove_dir_all(&p)?;
        } else {
            std::fs::remove_file(&p)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_addresses_are_recognised() {
        assert!(is_loopback("127.0.0.1:8090"));
        assert!(is_loopback("[::1]:8090"));
        assert!(is_loopback("localhost:1"));
        assert!(!is_loopback("10.0.0.5:8090"));
        assert!(!is_loopback("example.com:80"));
    }

    #[test]
    fn only_dev_directories_are_wiped() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("precious.txt"), "x").unwrap();
        assert!(wipe_persisted(dir.path()).is_err(), "an unrelated directory is refused");
        assert!(dir.path().join("precious.txt").exists());
        let dev = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dev.path().join("data")).unwrap();
        std::fs::write(dev.path().join("data/db"), "x").unwrap();
        std::fs::write(dev.path().join(state::CREDENTIALS_FILE), "{}").unwrap();
        wipe_persisted(dev.path()).unwrap();
        assert_eq!(std::fs::read_dir(dev.path()).unwrap().count(), 0);
    }
}
