//! `forgebase admin`: bootstrap and recover administrator (superuser)
//! accounts directly in the data directory. Works while the server runs
//! (SQLite serialises the writes); the database must be migrated.

use std::io::BufRead;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{Context, bail};
use clap::Subcommand;
use forgebase_auth::{AuthConfig, AuthService, ClientInfo};
use forgebase_core::DataLayout;
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
    let config = g.load()?.config;
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
