//! `forgebase admin`: bootstrap and recover administrator (superuser)
//! accounts directly in the data directory. Works while the server runs
//! (SQLite serialises the writes); the database must be migrated.

use std::io::{BufRead, IsTerminal, Write};
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{Context, bail};
use clap::Subcommand;
use forgebase_auth::{AuthConfig, AuthService, ClientInfo};
use forgebase_core::{Config, DataLayout};
use forgebase_db::{Database, DbOptions};
use serde_json::json;

use crate::GlobalArgs;

/// `forgebase admin <command>`.
#[derive(Debug, Subcommand)]
pub enum AdminCommand {
    /// Create an administrator account.
    Create {
        /// Email address (the login name).
        #[arg(long)]
        email: String,
        /// Read the password from the first line of standard input
        /// (recommended: keeps it out of the shell history and process list).
        #[arg(long, conflicts_with = "password")]
        password_stdin: bool,
        /// The password (visible to other local users in the process list).
        #[arg(long)]
        password: Option<String>,
    },
    /// Set a new password for an administrator and end their sessions.
    ResetPassword {
        /// Email address.
        #[arg(long)]
        email: String,
        /// Read the password from standard input.
        #[arg(long, conflicts_with = "password")]
        password_stdin: bool,
        /// The password.
        #[arg(long)]
        password: Option<String>,
    },
    /// List administrators.
    List {
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
}

fn read_password(stdin: bool, flag: Option<String>) -> anyhow::Result<String> {
    let pw = match (stdin, flag) {
        (_, Some(p)) => p,
        (true, None) => {
            let mut line = String::new();
            std::io::stdin()
                .lock()
                .read_line(&mut line)
                .context("cannot read the password from standard input")?;
            line.trim_end_matches(['\r', '\n']).to_owned()
        }
        (false, None) => bail!("give the password with --password-stdin (or --password)"),
    };
    if pw.is_empty() {
        bail!("the password is empty");
    }
    Ok(pw)
}

async fn open(g: &GlobalArgs) -> anyhow::Result<(Database, AuthService)> {
    open_config(&g.load()?.config).await
}

/// Opens the database and the auth service of `config`'s data directory.
async fn open_config(config: &Config) -> anyhow::Result<(Database, AuthService)> {
    let layout = DataLayout::new(&config.data_dir);
    if !layout.is_initialized() {
        bail!(
            "no database at {} (run `forgebase init`)",
            layout.database_path().display()
        );
    }
    let db = Database::open(DbOptions::from_config(&config.database, layout.database_path()).create_if_missing(false))
        .await?;
    let status = db.migration_status().await?;
    if !status.is_up_to_date() {
        db.close().await;
        bail!(
            "the database schema is at v{} of v{}: run `forgebase migrate` first",
            status.current_version,
            status.latest_version
        );
    }
    let mut cfg = AuthConfig::for_data_dir(layout.root());
    // The CLI never creates collections; the server does at startup.
    cfg.default_collection = None;
    let backend: Arc<dyn forgebase_db::Backend> = Arc::new(db.clone());
    match AuthService::open_with_data(
        backend,
        Arc::new(forgebase_auth::data::DefaultAuthorizer),
        layout.root(),
        cfg,
        None,
    )
    .await
    {
        Ok(auth) => Ok((db, auth)),
        Err(e) => {
            db.close().await;
            Err(e.into())
        }
    }
}

fn cli_client() -> ClientInfo {
    ClientInfo {
        ip: None,
        user_agent: Some("forgebase-cli".into()),
        request_id: None,
        realm: None,
        turnstile_token: None,
        impersonator: None,
    }
}

/// Runs an admin subcommand.
pub async fn run(g: &GlobalArgs, cmd: AdminCommand) -> anyhow::Result<ExitCode> {
    crate::commands::quiet_logging(g)?;
    match cmd {
        AdminCommand::Create {
            email,
            password_stdin,
            password,
        } => {
            let pw = read_password(password_stdin, password)?;
            let (db, auth) = open(g).await?;
            let res = auth.create_admin(&email, &pw, "cli", &cli_client()).await;
            db.close().await;
            let admin = res?;
            println!("created administrator {} ({})", admin.email, admin.id);
        }
        AdminCommand::ResetPassword {
            email,
            password_stdin,
            password,
        } => {
            let pw = read_password(password_stdin, password)?;
            let (db, auth) = open(g).await?;
            let res = auth.reset_admin_password(&email, &pw, "cli", &cli_client()).await;
            db.close().await;
            res?;
            println!(
                "password updated for {}; their sessions were ended",
                email.trim().to_lowercase()
            );
        }
        AdminCommand::List { json } => {
            let (db, auth) = open(g).await?;
            let res = auth.list_admins().await;
            db.close().await;
            let admins = res?;
            if json {
                println!("{}", json!({ "admins": admins }));
            } else if admins.is_empty() {
                println!(
                    "no administrators (create one with `forgebase admin create --email <email> --password-stdin`)"
                );
            } else {
                for a in admins {
                    println!(
                        "{}  {}  created {}  last login {}",
                        a.id,
                        a.email,
                        a.created_at,
                        a.last_login_at.as_deref().unwrap_or("never")
                    );
                }
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// `forgebase init`: the first administrator (FB-90)
// ---------------------------------------------------------------------------

/// Administrator flags of `forgebase init`.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct InitAdminArgs {
    /// Create the administrator with this email, without prompting
    /// (with --admin-password-stdin).
    #[arg(
        long,
        value_name = "EMAIL",
        requires = "admin_password_stdin",
        conflicts_with = "no_admin"
    )]
    pub admin_email: Option<String>,
    /// Read the administrator password from the first line of standard input.
    #[arg(long, requires = "admin_email")]
    pub admin_password_stdin: bool,
    /// Do not create an administrator now; the first `forgebase serve`
    /// prints a one-time setup link instead.
    #[arg(long)]
    pub no_admin: bool,
}

/// What `forgebase init` did about the administrator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InitAdmin {
    /// Created this administrator (email).
    Created(String),
    /// An administrator already existed.
    Exists,
    /// None created; the reason is shown to the user.
    Skipped(String),
}

impl InitAdmin {
    /// The `admin:` line of the init summary.
    pub fn describe(&self) -> String {
        match self {
            Self::Created(email) => format!("{email} (created)"),
            Self::Exists => "already set up (kept)".into(),
            Self::Skipped(why) => {
                format!("not created ({why}); the first `forgebase serve` prints a one-time setup link")
            }
        }
    }
}

/// Reads and checks the `--admin-password-stdin` password before
/// `forgebase init` touches the disk.
pub fn preset_admin(args: &InitAdminArgs) -> anyhow::Result<Option<(String, String)>> {
    let Some(email) = &args.admin_email else {
        return Ok(None);
    };
    let pw = read_password(args.admin_password_stdin, None)?;
    forgebase_auth::setup::check_admin_credentials(email, &pw)?;
    Ok(Some((email.clone(), pw)))
}

/// The administrator step of `forgebase init`, after the database is
/// migrated: `--no-admin`, the `--admin-email` preset, or a prompt on a
/// terminal. Never creates a second administrator.
pub async fn init_admin(
    config: &Config,
    args: &InitAdminArgs,
    preset: Option<(String, String)>,
) -> anyhow::Result<InitAdmin> {
    if args.no_admin {
        return Ok(InitAdmin::Skipped("--no-admin".into()));
    }
    // A plain read: opening the auth service creates the master key, which
    // `init` leaves to the first start unless it creates an administrator.
    if admin_exists(config).await? {
        return Ok(InitAdmin::Exists);
    }
    let creds = match preset {
        Some(c) => Some(c),
        None if !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) => {
            return Ok(InitAdmin::Skipped("no terminal to prompt on".into()));
        }
        None => {
            let stdin = std::io::stdin();
            let mut input = stdin.lock();
            let mut out = std::io::stdout();
            match prompt_admin(&mut input, &mut out, &mut set_echo) {
                Ok(c) => c,
                Err(e) => return Ok(InitAdmin::Skipped(format!("{e:#}"))),
            }
        }
    };
    let Some((email, pw)) = creds else {
        return Ok(InitAdmin::Skipped("declined".into()));
    };
    let (db, auth) = open_config(config).await?;
    let res = auth.create_admin(&email, &pw, "cli", &cli_client()).await;
    db.close().await;
    Ok(InitAdmin::Created(res?.email))
}

/// Whether the data directory of `config` has an administrator, over a
/// read-only connection.
async fn admin_exists(config: &Config) -> anyhow::Result<bool> {
    let layout = DataLayout::new(&config.data_dir);
    let db = Database::open(DbOptions::from_config(&config.database, layout.database_path()).read_only()).await?;
    let backend: Arc<dyn forgebase_db::Backend> = Arc::new(db.clone());
    let res = forgebase_auth::setup::admin_exists(&backend).await;
    db.close().await;
    Ok(res?)
}

/// Attempts at a valid email and matching passwords before giving up.
const PROMPT_ATTEMPTS: usize = 3;

/// Asks whether to create the administrator, then for the email and the
/// password twice (hidden: `echo(false)` while reading). `None` = declined.
pub fn prompt_admin(
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    echo: &mut dyn FnMut(bool) -> std::io::Result<()>,
) -> anyhow::Result<Option<(String, String)>> {
    write!(out, "Create the administrator account now? [Y/n] ")?;
    out.flush()?;
    let answer = read_line(input)?.ok_or_else(|| anyhow::anyhow!("no answer"))?;
    if !matches!(answer.trim().to_lowercase().as_str(), "" | "y" | "yes") {
        return Ok(None);
    }
    for _ in 0..PROMPT_ATTEMPTS {
        write!(out, "Email: ")?;
        out.flush()?;
        let email = read_line(input)?.ok_or_else(|| anyhow::anyhow!("no email given"))?;
        let pw = hidden(input, out, echo, "Password: ")?;
        let confirm = hidden(input, out, echo, "Confirm password: ")?;
        if pw != confirm {
            writeln!(out, "The passwords do not match; try again.")?;
            continue;
        }
        match forgebase_auth::setup::check_admin_credentials(&email, &pw) {
            Ok(()) => return Ok(Some((email.trim().to_owned(), pw))),
            Err(e) => writeln!(out, "{e}; try again.")?,
        }
    }
    bail!("no valid administrator details after {PROMPT_ATTEMPTS} attempts")
}

fn read_line(input: &mut dyn BufRead) -> std::io::Result<Option<String>> {
    let mut line = String::new();
    if input.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    Ok(Some(line.trim_end_matches(['\r', '\n']).to_owned()))
}

/// Reads one line with terminal echo off; echo is restored even when the
/// read fails.
fn hidden(
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    echo: &mut dyn FnMut(bool) -> std::io::Result<()>,
    prompt: &str,
) -> anyhow::Result<String> {
    write!(out, "{prompt}")?;
    out.flush()?;
    echo(false).context("cannot hide the password input on this terminal")?;
    let line = read_line(input);
    echo(true)?;
    // The newline the user typed was not echoed.
    writeln!(out)?;
    line?.ok_or_else(|| anyhow::anyhow!("no password given"))
}

/// Turns terminal echo on or off with `stty` (this crate forbids `unsafe`,
/// so no direct termios call).
#[cfg(unix)]
fn set_echo(on: bool) -> std::io::Result<()> {
    let status = std::process::Command::new("stty")
        .arg(if on { "echo" } else { "-echo" })
        .stdin(std::process::Stdio::inherit())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other("stty failed"))
    }
}

/// No hidden input without `stty`: refuse rather than echo the password.
#[cfg(not(unix))]
fn set_echo(_on: bool) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "hidden input needs a Unix terminal; use --admin-email with --admin-password-stdin, or the setup link",
    ))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    type Creds = anyhow::Result<Option<(String, String)>>;

    fn run(script: &str) -> (Creds, String, Vec<bool>) {
        let mut input = std::io::Cursor::new(script.as_bytes().to_vec());
        let mut out = Vec::new();
        let mut echoes = Vec::new();
        let res = prompt_admin(&mut input, &mut out, &mut |on| {
            echoes.push(on);
            Ok(())
        });
        (res, String::from_utf8(out).unwrap(), echoes)
    }

    #[test]
    fn prompt_creates_with_hidden_matching_passwords() {
        let (res, out, echoes) = run("\nadmin@example.com\ncorrect horse battery\ncorrect horse battery\n");
        let (email, pw) = res.unwrap().unwrap();
        assert_eq!(
            (email.as_str(), pw.as_str()),
            ("admin@example.com", "correct horse battery")
        );
        assert!(out.starts_with("Create the administrator account now? [Y/n] "), "{out}");
        assert!(out.contains("Email: ") && out.contains("Password: ") && out.contains("Confirm password: "));
        assert!(!out.contains("correct horse"), "password echoed: {out}");
        assert_eq!(echoes, [false, true, false, true], "echo off around each password");
    }

    #[test]
    fn prompt_declines_and_retries() {
        assert!(run("n\n").0.unwrap().is_none());
        let (res, out, _) = run(
            "y\na@example.com\nfirst password\nsecond password\na@example.com\nshort\nshort\na@example.com\nlong enough pw\nlong enough pw\n",
        );
        assert_eq!(res.unwrap().unwrap().1, "long enough pw");
        assert!(out.contains("do not match"), "{out}");
        assert!(out.contains("try again"), "{out}");
        assert!(
            run("y\nnope\nlong enough pw\nlong enough pw\n").0.is_err(),
            "input ends"
        );
    }

    #[test]
    fn prompt_refuses_when_echo_cannot_be_hidden() {
        let mut input = std::io::Cursor::new(b"y\na@example.com\nsecret password\n".to_vec());
        let mut out = Vec::new();
        let res = prompt_admin(&mut input, &mut out, &mut |_| Err(std::io::Error::other("no tty")));
        assert!(res.is_err());
        assert!(!String::from_utf8(out).unwrap().contains("secret"));
    }
}
