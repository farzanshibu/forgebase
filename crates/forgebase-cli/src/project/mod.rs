//! Project as code (ADR-0037): `forgebase project pull|plan|apply|drift|validate|schemas`,
//! `forgebase deploy` and `forgebase new`.
//!
//! A project directory (default `./forgebase`) declares collections,
//! functions, queues, buckets, engine rules and OAuth clients as files
//! ([`layout`]). `plan` compares them with a server and prints what `apply`
//! would change; `apply` makes the changes in dependency order with
//! optimistic concurrency; `drift` reports changes made outside the project;
//! `pull` writes a server's state as a project; `deploy` builds functions and
//! applies; `new` scaffolds an app.
//!
//! Exit codes: `plan` and `drift` exit 0 when nothing differs, 2 when
//! something does, 1 on an error; `apply` and `deploy` exit 0 when every
//! change succeeded and 1 otherwise.

pub mod apply;
pub mod layout;
pub mod model;
pub mod plan;
pub mod pull;
pub mod remote;
pub mod scaffold;
pub mod schema;

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, bail};
use clap::{Args, Subcommand};
use serde_json::json;

use crate::GlobalArgs;
use layout::Project;
use model::Kind;
use plan::{Inputs, Plan};
use remote::{Connection, Remote};

/// Exit code of `plan` / `drift` when something differs.
const EXIT_CHANGES: u8 = 2;

/// Where the project is and how to reach the server.
#[derive(Debug, Clone, Args)]
pub struct ConnArgs {
    /// Project directory.
    #[arg(long, global = true, value_name = "DIR", default_value = "forgebase")]
    pub dir: PathBuf,
    /// Server base URL, http:// or https:// [default: the project's url, else the local config] [env: FORGEBASE_URL]
    #[arg(long, global = true, value_name = "URL")]
    pub server: Option<String>,
    /// API key (fbk_...) or personal access token (fbp_...) [env: FORGEBASE_TOKEN]
    #[arg(long, global = true, value_name = "TOKEN")]
    pub token: Option<String>,
    /// Sign in as this administrator instead (password: --password-stdin or FORGEBASE_ADMIN_PASSWORD) [env: FORGEBASE_ADMIN_EMAIL]
    #[arg(long, global = true, value_name = "EMAIL")]
    pub admin_email: Option<String>,
    /// Read the administrator password from the first line of standard input.
    #[arg(long, global = true)]
    pub password_stdin: bool,
    /// Target: an [environments.NAME] table of the project file, and the ForgeBase environment of its project.
    #[arg(long = "env", global = true, value_name = "NAME")]
    pub env: Option<String>,
    /// Realm of OAuth clients (also sent as X-Forgebase-Realm) [default: the project's realm, else default].
    #[arg(long, global = true, value_name = "REALM")]
    pub realm: Option<String>,
    /// Extra CA certificates (PEM) trusted for https:// servers [env: FORGEBASE_CA_FILE]
    #[arg(long, global = true, value_name = "FILE")]
    pub ca_file: Option<PathBuf>,
}

/// Flags of `apply` and `deploy`.
#[derive(Debug, Clone, Args)]
pub struct ApplyArgs {
    /// Also delete server resources the project does not declare.
    #[arg(long)]
    pub prune: bool,
    /// Allow changes that lose data: dropping fields or collections, type changes, deleting buckets, replacements.
    #[arg(long)]
    pub allow_destructive: bool,
    /// Do not ask for confirmation (CI).
    #[arg(long)]
    pub auto_approve: bool,
    /// Send every function secret again from its environment variable (values cannot be compared).
    #[arg(long)]
    pub update_secrets: bool,
    /// Write secrets the server issues (webhook signing secrets, client secrets) to this file as NAME=value (mode 0600).
    #[arg(long, value_name = "FILE")]
    pub secrets_out: Option<PathBuf>,
    /// Print the plan and the outcome as JSON.
    #[arg(long)]
    pub json: bool,
}

/// `forgebase project`.
#[derive(Debug, Args)]
pub struct ProjectCli {
    #[command(flatten)]
    conn: ConnArgs,
    #[command(subcommand)]
    command: ProjectCommand,
}

#[derive(Debug, Subcommand)]
enum ProjectCommand {
    /// Write the server's current state into the project directory (secrets are never written).
    Pull {
        /// Overwrite an existing project's resource files (and delete files of resources the server no longer has).
        #[arg(long)]
        force: bool,
    },
    /// Show what `apply` would change. Exit code 0: no changes, 2: changes, 1: error.
    Plan {
        /// Print the plan as JSON.
        #[arg(long)]
        json: bool,
        /// Save the plan for `apply --plan FILE`.
        #[arg(long, value_name = "FILE")]
        out: Option<PathBuf>,
        /// Plan deletions of server resources the project does not declare.
        #[arg(long)]
        prune: bool,
    },
    /// Make the server match the project.
    Apply {
        #[command(flatten)]
        args: ApplyArgs,
        /// Apply a plan saved by `plan --out`; fails if the server or the project changed since.
        #[arg(long, value_name = "FILE")]
        plan: Option<PathBuf>,
    },
    /// Report changes made on the server outside the project. Exit code 0: none, 2: drift, 1: error.
    Drift {
        /// Print the report as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Check the project files without a server.
    Validate {
        /// Print the result as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Write the JSON Schemas of the project files to <dir>/.schemas/ (for editors).
    Schemas,
}

/// `forgebase deploy`.
#[derive(Debug, Args)]
pub struct DeployCli {
    #[command(flatten)]
    conn: ConnArgs,
    #[command(flatten)]
    args: ApplyArgs,
    /// Do not run the functions' build commands.
    #[arg(long)]
    skip_build: bool,
}

/// `forgebase new`.
#[derive(Debug, Args)]
pub struct NewCli {
    /// App directory to create.
    dir: PathBuf,
    /// Starting point.
    #[arg(long, value_enum, default_value = "basic")]
    template: scaffold::Template,
    /// Project name [default: the directory name].
    #[arg(long)]
    name: Option<String>,
    /// Directory of the ForgeBase project inside the app.
    #[arg(long, default_value = "forgebase", value_name = "DIR")]
    project_dir: String,
    /// Write into an existing, non-empty directory.
    #[arg(long)]
    force: bool,
}

fn connection(c: &ConnArgs) -> anyhow::Result<Connection> {
    let admin_password = if c.password_stdin {
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        let pw = line.trim_end_matches(['\r', '\n']).to_owned();
        if pw.is_empty() {
            bail!("--password-stdin: standard input is empty");
        }
        Some(pw)
    } else {
        None
    };
    Ok(Connection {
        server: c.server.clone(),
        token: c.token.clone(),
        admin_email: c.admin_email.clone(),
        admin_password,
        ca_file: c.ca_file.clone(),
        realm: c.realm.clone(),
    })
}

pub(crate) fn warn(project: &Project) {
    for w in &project.warnings {
        eprintln!("warning: {w}");
    }
}

/// Loads the project and connects to its server.
async fn open(g: &GlobalArgs, c: &ConnArgs) -> anyhow::Result<(Project, Remote)> {
    let project = layout::load(&c.dir)?;
    warn(&project);
    if let Some(e) = &c.env
        && !project.config.environments.contains_key(e)
        && project.config.defaults.project.is_none()
    {
        bail!("--env {e}: the project file has no [environments.{e}] table and [defaults] names no ForgeBase project");
    }
    let target = project.config.target(c.env.as_deref());
    let remote = Remote::connect(&connection(c)?, &target, g).await?;
    if !remote.has_credential() {
        eprintln!("warning: no credential: set FORGEBASE_TOKEN (or pass --token / --admin-email)");
    }
    Ok((project, remote))
}

fn managed_kinds(project: &Project) -> BTreeSet<Kind> {
    Kind::ALL.into_iter().filter(|k| project.config.manages(*k)).collect()
}

/// Reads the server and plans.
pub(crate) async fn make_plan(project: &Project, remote: &Remote, prune: bool) -> anyhow::Result<Plan> {
    let actual = remote.fetch_all(&managed_kinds(project)).await?;
    let server_collections = remote.collection_names().await?.into_values().collect();
    plan::compute(
        project,
        &actual,
        &Inputs {
            server: remote.base.clone(),
            prune,
            server_collections,
        },
    )
}

/// Runs a `project` subcommand.
pub async fn run_project(g: &GlobalArgs, cli: ProjectCli) -> anyhow::Result<ExitCode> {
    let c = &cli.conn;
    let mut stdout = std::io::stdout();
    match cli.command {
        ProjectCommand::Schemas => {
            layout::write_schemas(&c.dir)?;
            writeln!(stdout, "wrote {}", c.dir.join(layout::SCHEMAS_DIR).display())?;
            Ok(ExitCode::SUCCESS)
        }
        ProjectCommand::Validate { json } => {
            let p = layout::load(&c.dir)?;
            warn(&p);
            let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
            for r in &p.resources {
                *counts.entry(r.kind.as_str()).or_default() += 1;
            }
            if json {
                writeln!(
                    stdout,
                    "{}",
                    json!({
                        "valid": true,
                        "resources": counts,
                        "seeds": p.seeds.iter().map(|s| json!({"collection": s.collection, "file": s.path.display().to_string(), "records": s.records})).collect::<Vec<_>>(),
                        "warnings": p.warnings,
                    })
                )?;
            } else {
                let list: Vec<String> = counts.iter().map(|(k, n)| format!("{n} {k}")).collect();
                writeln!(
                    stdout,
                    "{} is valid: {} resource{} ({}), {} seed file{}",
                    c.dir.display(),
                    p.resources.len(),
                    if p.resources.len() == 1 { "" } else { "s" },
                    if list.is_empty() {
                        "none".to_owned()
                    } else {
                        list.join(", ")
                    },
                    p.seeds.len(),
                    if p.seeds.len() == 1 { "" } else { "s" }
                )?;
            }
            Ok(ExitCode::SUCCESS)
        }
        ProjectCommand::Pull { force } => {
            let target = existing_target(&c.dir, c.env.as_deref());
            let remote = Remote::connect(&connection(c)?, &target, g).await?;
            pull::pull(&remote, &c.dir, &pull::Options { force }, &mut stdout).await?;
            Ok(ExitCode::SUCCESS)
        }
        ProjectCommand::Plan { json, out, prune } => {
            let (project, remote) = open(g, c).await?;
            let plan = make_plan(&project, &remote, prune).await?;
            if json {
                writeln!(stdout, "{}", serde_json::to_string_pretty(&plan)?)?;
            } else {
                plan::render(&plan, &mut stdout)?;
            }
            if let Some(path) = out {
                std::fs::write(&path, serde_json::to_string_pretty(&plan)? + "\n")
                    .with_context(|| format!("cannot write {}", path.display()))?;
                if !json {
                    writeln!(
                        stdout,
                        "Saved the plan to {}; apply it with `forgebase project apply --plan {}`.",
                        path.display(),
                        path.display()
                    )?;
                }
            }
            Ok(if plan.has_changes() {
                ExitCode::from(EXIT_CHANGES)
            } else {
                ExitCode::SUCCESS
            })
        }
        ProjectCommand::Apply { args, plan } => {
            let (project, remote) = open(g, c).await?;
            apply_flow(&project, &remote, &args, plan.as_deref(), c.password_stdin).await
        }
        ProjectCommand::Drift { json } => {
            let (project, remote) = open(g, c).await?;
            let plan = make_plan(&project, &remote, false).await?;
            let drift = plan.has_changes() || !plan.unmanaged.is_empty();
            if json {
                writeln!(
                    stdout,
                    "{}",
                    serde_json::to_string_pretty(&json!({"drift": drift, "plan": plan}))?
                )?;
            } else {
                render_drift(&plan, &mut stdout)?;
            }
            Ok(if drift {
                ExitCode::from(EXIT_CHANGES)
            } else {
                ExitCode::SUCCESS
            })
        }
    }
}

/// The connection target of a project directory that may not exist yet (pull).
fn existing_target(dir: &Path, env: Option<&str>) -> layout::Target {
    match layout::load(dir) {
        Ok(p) => p.config.target(env),
        Err(_) => layout::Target::default(),
    }
}

fn render_drift(plan: &Plan, out: &mut impl Write) -> std::io::Result<()> {
    if !plan.has_changes() && plan.unmanaged.is_empty() {
        return writeln!(out, "No drift: {} matches the project.", plan.server);
    }
    writeln!(out, "Drift between the project and {}:", plan.server)?;
    for c in &plan.changes {
        let what = match c.action {
            plan::Action::Create => "missing on the server",
            plan::Action::Delete => "only on the server",
            plan::Action::Update | plan::Action::Replace => "differs on the server",
        };
        writeln!(out, "  {} {}: {what}", c.kind, c.name)?;
        for d in &c.diff {
            writeln!(
                out,
                "    {}: server {} / project {}",
                d.path,
                d.before.as_ref().map_or("(absent)".into(), model::canonical_text),
                d.after.as_ref().map_or("(absent)".into(), model::canonical_text)
            )?;
        }
    }
    for u in &plan.unmanaged {
        writeln!(out, "  {} {}: only on the server (not in the project)", u.kind, u.name)?;
    }
    writeln!(
        out,
        "Run `forgebase project plan` to see how apply would resolve it, or `forgebase project pull --force` to take the server's state."
    )
}

fn confirm(plan: &Plan, auto_approve: bool, stdin_used: bool) -> anyhow::Result<bool> {
    if auto_approve {
        return Ok(true);
    }
    if stdin_used || !std::io::stdin().is_terminal() {
        bail!("confirmation needed: standard input is not a terminal; pass --auto-approve to apply without asking");
    }
    eprint!(
        "Apply {} change{} to {}? Only \"yes\" is accepted: ",
        plan.changes.len(),
        if plan.changes.len() == 1 { "" } else { "s" },
        plan.server
    );
    std::io::stderr().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim() == "yes")
}

/// Checks a saved plan against a fresh one; returns the fresh plan restricted to the saved changes.
fn reconcile(saved: &Plan, fresh: Plan, project: &Project) -> anyhow::Result<Plan> {
    if saved.format != plan::PLAN_FORMAT {
        bail!(
            "the plan file has format {} (this CLI reads {})",
            saved.format,
            plan::PLAN_FORMAT
        );
    }
    if saved.server != fresh.server {
        bail!("the plan was made for {}, not {}", saved.server, fresh.server);
    }
    let mut problems = Vec::new();
    for s in &saved.changes {
        if let Some(d) = project.get(s.kind, &s.name)
            && s.desired_digest.as_deref() != Some(model::digest(&d.spec).as_str())
        {
            problems.push(format!(
                "{} {}: the project file changed since the plan",
                s.kind, s.name
            ));
        }
        if let Some(f) = fresh.changes.iter().find(|f| f.kind == s.kind && f.name == s.name)
            && f.fingerprint != s.fingerprint
        {
            problems.push(format!(
                "changed_since_plan: {} {} changed on the server after the plan was made",
                s.kind, s.name
            ));
        }
    }
    for f in &fresh.changes {
        if !saved.changes.iter().any(|s| s.kind == f.kind && s.name == f.name) {
            problems.push(format!(
                "{} {}: {} is needed now but not in the saved plan (the server or the project changed)",
                f.kind,
                f.name,
                match f.action {
                    plan::Action::Create => "a create",
                    plan::Action::Update => "an update",
                    plan::Action::Replace => "a replacement",
                    plan::Action::Delete => "a delete",
                }
            ));
        }
    }
    if !problems.is_empty() {
        bail!(
            "{}\nnothing was changed; run `forgebase project plan` again",
            problems.join("\n")
        );
    }
    Ok(fresh)
}

async fn apply_flow(
    project: &Project,
    remote: &Remote,
    args: &ApplyArgs,
    saved: Option<&Path>,
    stdin_used: bool,
) -> anyhow::Result<ExitCode> {
    let mut stdout = std::io::stdout();
    let opts = apply::Options {
        allow_destructive: args.allow_destructive,
        update_secrets: args.update_secrets,
        secrets_out: args.secrets_out.clone(),
    };
    apply::check_secrets_out(project, &opts)?;
    let (plan, approved) = match saved {
        Some(path) => {
            let text = std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
            let saved: Plan =
                serde_json::from_str(&text).with_context(|| format!("{} is not a plan file", path.display()))?;
            let fresh = make_plan(project, remote, saved.prune).await?;
            // A saved plan was reviewed when it was made.
            (reconcile(&saved, fresh, project)?, true)
        }
        None => (make_plan(project, remote, args.prune).await?, args.auto_approve),
    };
    let update_secrets_only = !plan.has_changes() && args.update_secrets;
    if !plan.has_changes() && !update_secrets_only {
        if args.json {
            writeln!(stdout, "{}", json!({"plan": plan, "applied": 0, "failed": []}))?;
        } else {
            plan::render(&plan, &mut stdout)?;
        }
        return Ok(ExitCode::SUCCESS);
    }
    if !args.json {
        plan::render(&plan, &mut stdout)?;
        writeln!(stdout)?;
    }
    apply::check_destructive(&plan, &opts)?;
    if !confirm(&plan, approved, stdin_used)? {
        bail!("not applied: the answer was not \"yes\"");
    }
    let outcome = if args.json {
        apply::execute(remote, project, &plan, &opts, &mut std::io::stderr()).await?
    } else {
        apply::execute(remote, project, &plan, &opts, &mut stdout).await?
    };
    if args.json {
        writeln!(
            stdout,
            "{}",
            json!({
                "plan": plan,
                "applied": outcome.applied,
                "in_place": outcome.in_place,
                "skipped": outcome.skipped,
                "failed": outcome.failed.iter().map(|(id, e)| json!({"change": id, "error": e})).collect::<Vec<_>>(),
                "secrets_written": args.secrets_out.as_ref().filter(|_| !outcome.secrets.is_empty()).map(|p| p.display().to_string()),
                "secrets_not_saved": outcome.unsaved_secrets,
            })
        )?;
    } else {
        writeln!(
            stdout,
            "\nApply {}: {} changed, {} already in place, {} failed, {} skipped.",
            if outcome.failed.is_empty() {
                "complete"
            } else {
                "incomplete"
            },
            outcome.applied,
            outcome.in_place,
            outcome.failed.len(),
            outcome.skipped
        )?;
        if !outcome.failed.is_empty() {
            writeln!(
                stdout,
                "Fix the errors above and run apply again: it continues where this run stopped."
            )?;
        }
        if let Some(p) = &args.secrets_out
            && !outcome.secrets.is_empty()
        {
            writeln!(
                stdout,
                "Wrote {} issued secret(s) to {}.",
                outcome.secrets.len(),
                p.display()
            )?;
        }
    }
    if !outcome.unsaved_secrets.is_empty() {
        eprintln!(
            "warning: the server issued secrets that were not saved ({}); they are shown only once: pass --secrets-out FILE to keep them, or rotate them on the server",
            outcome.unsaved_secrets.join(", ")
        );
    }
    Ok(if outcome.failed.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// Runs each function's build command (in its directory).
pub(crate) fn run_builds(project: &Project, out: &mut impl Write) -> anyhow::Result<usize> {
    let mut n = 0;
    for r in &project.resources {
        let model::Extra::Function(f) = &r.extra else { continue };
        let Some(cmd) = &f.build else { continue };
        writeln!(out, "building function {}: {cmd}", r.name)?;
        out.flush()?;
        let status = if cfg!(windows) {
            std::process::Command::new("cmd")
                .args(["/C", cmd])
                .current_dir(&f.dir)
                .status()
        } else {
            std::process::Command::new("sh")
                .args(["-c", cmd])
                .current_dir(&f.dir)
                .status()
        }
        .with_context(|| format!("cannot run the build of function {}", r.name))?;
        if !status.success() {
            bail!("the build of function {} failed ({status}): {cmd}", r.name);
        }
        if !f.entry.is_file() {
            bail!(
                "the build of function {} did not write its entry {}",
                r.name,
                f.entry.display()
            );
        }
        n += 1;
    }
    Ok(n)
}

/// `forgebase deploy`: build functions, then apply.
pub async fn run_deploy(g: &GlobalArgs, cli: DeployCli) -> anyhow::Result<ExitCode> {
    let c = &cli.conn;
    // A function whose entry its build writes loads with `code: None`; it is read again after the build.
    let project = layout::load(&c.dir)?;
    if !cli.skip_build {
        let mut out: Box<dyn Write> = if cli.args.json {
            Box::new(std::io::stderr())
        } else {
            Box::new(std::io::stdout())
        };
        if run_builds(&project, &mut out)? > 0 {
            writeln!(out)?;
        }
    }
    let (project, remote) = open(g, c).await?;
    apply_flow(&project, &remote, &cli.args, None, c.password_stdin).await
}

/// `forgebase new`.
pub fn run_new(cli: NewCli) -> anyhow::Result<ExitCode> {
    scaffold::create(
        &scaffold::Options {
            dir: cli.dir,
            template: cli.template,
            name: cli.name,
            project_dir: cli.project_dir,
            force: cli.force,
        },
        &mut std::io::stdout(),
    )?;
    Ok(ExitCode::SUCCESS)
}
