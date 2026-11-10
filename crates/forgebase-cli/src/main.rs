//! `forgebase`: the single ForgeBase executable.
//!
//! ```text
//! forgebase init [DIR]     create config, data directory and database
//! forgebase serve          run the server (--host --port --data-dir --config)
//! forgebase migrate        apply pending system migrations (--status to list)
//! forgebase status         show project, database and server state
//! forgebase doctor         run diagnostics; exit code 1 if any check fails
//! forgebase backup         online backup or snapshot (also: list, verify, prune, delete)
//! forgebase restore <SRC>  verified restore into a stopped data directory
//! forgebase export <...>   export collections, records or the configuration
//! forgebase import <...>   import collections, records or a configuration file
//! forgebase admin create   create an administrator (also reset-password, list)
//! forgebase version        print version information
//! forgebase functions deploy <NAME> <FILE.wasm>   deploy a function version (also: logs <NAME> [--follow])
//! forgebase queues publish|pull|tail   publish to a topic, pull from a subscription, follow a topic
//! forgebase jobs watch <ID>            follow a long-running job's progress until it finishes
//! ```
//!
//! Configuration precedence: flags > `FORGEBASE_*` environment > config file > defaults.

mod admin;
mod commands;
mod dataops;
mod doctor;
mod dotenv;
mod functions;
mod probe;
mod queues;
#[cfg(test)]
mod testserver;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use forgebase_core::config::{ConfigLoader, ConfigOverrides};
use forgebase_core::{LoadedConfig, LogFormat};

#[derive(Debug, Parser)]
#[command(
    name = "forgebase",
    version,
    about = "ForgeBase: a single-binary backend with an embedded database",
    propagate_version = true
)]
struct Cli {
    #[command(flatten)]
    global: GlobalArgs,
    #[command(subcommand)]
    command: Command,
}

/// Flags accepted by every command. They override environment variables and
/// the config file.
#[derive(Debug, Clone, Default, Args)]
struct GlobalArgs {
    /// Config file [default: ./forgebase.toml if it exists] [env: FORGEBASE_CONFIG]
    #[arg(long, global = true, value_name = "FILE")]
    config: Option<PathBuf>,
    /// Data directory [default: forgebase-data] [env: FORGEBASE_DATA_DIR]
    #[arg(long, global = true, value_name = "DIR")]
    data_dir: Option<PathBuf>,
    /// Bind host [default: 127.0.0.1] [env: FORGEBASE_HOST]
    #[arg(long, global = true)]
    host: Option<String>,
    /// Bind port; 0 picks a free port [default: 8090] [env: FORGEBASE_PORT]
    #[arg(long, global = true)]
    port: Option<u16>,
    /// Log filter, e.g. `info` or `forgebase=debug` [env: FORGEBASE_LOG_LEVEL]
    #[arg(long, global = true, value_name = "FILTER")]
    log_level: Option<String>,
    /// Log format: pretty or json [env: FORGEBASE_LOG_FORMAT]
    #[arg(long, global = true, value_name = "FORMAT")]
    log_format: Option<LogFormat>,
    /// Also serve the Redis protocol (RESP2/RESP3) for the built-in KV engine [env: FORGEBASE_RESP_ENABLED]
    #[arg(long, global = true)]
    resp: bool,
    /// RESP bind host [default: 127.0.0.1] [env: FORGEBASE_RESP_HOST]
    #[arg(long, global = true, value_name = "HOST")]
    resp_host: Option<String>,
    /// RESP bind port; 0 picks a free port [default: 6380] [env: FORGEBASE_RESP_PORT]
    #[arg(long, global = true, value_name = "PORT")]
    resp_port: Option<u16>,
    /// Mount billing (on by default; set the env var/config to false to turn off) [env: FORGEBASE_BILLING_ENABLED]
    #[arg(long, global = true)]
    billing: bool,
}

impl GlobalArgs {
    fn overrides(&self) -> ConfigOverrides {
        ConfigOverrides {
            config_file: self.config.clone(),
            data_dir: self.data_dir.clone(),
            host: self.host.clone(),
            port: self.port,
            log_level: self.log_level.clone(),
            log_format: self.log_format,
            resp_enabled: self.resp.then_some(true),
            resp_host: self.resp_host.clone(),
            resp_port: self.resp_port,
            billing_enabled: self.billing.then_some(true),
            ..ConfigOverrides::default()
        }
    }

    /// Loads configuration from flags, environment and file.
    fn load(&self) -> anyhow::Result<LoadedConfig> {
        self.load_with(self.overrides())
    }

    fn load_with(&self, cli: ConfigOverrides) -> anyhow::Result<LoadedConfig> {
        let cwd = std::env::current_dir()?;
        let vars = std::env::vars_os().filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)));
        let env = ConfigOverrides::from_env(vars)?;
        Ok(ConfigLoader::new(cwd).env(env).cli(cli).load()?)
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create a project: config file, data directory and migrated database.
    Init {
        /// Project directory; created if missing.
        #[arg(default_value = ".")]
        dir: PathBuf,
    },
    /// Start the server and serve until Ctrl-C / SIGTERM.
    Serve,
    /// Apply pending system migrations.
    Migrate {
        /// Only list applied and pending migrations.
        #[arg(long)]
        status: bool,
    },
    /// Show project, database and server state.
    Status {
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Check configuration, permissions, disk, database integrity, migrations,
    /// indexes, storage blobs, master key, snapshots and port.
    Doctor {
        /// Print JSON.
        #[arg(long)]
        json: bool,
        /// Also verify the newest snapshot's files and database.
        #[arg(long)]
        deep: bool,
    },
    /// Back up the data directory online (a snapshot, or a directory you name).
    Backup(Box<dataops::BackupCli>),
    /// Restore a verified backup or snapshot into a stopped data directory.
    Restore(Box<dataops::RestoreCli>),
    /// Export collections, records or the configuration.
    Export {
        #[command(subcommand)]
        command: dataops::ExportCommand,
    },
    /// Import collections, records or a configuration file.
    Import {
        #[command(subcommand)]
        command: dataops::ImportCommand,
    },
    /// Print version information.
    Version {
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Manage administrator (superuser) accounts.
    Admin {
        #[command(subcommand)]
        command: admin::AdminCommand,
    },
    /// Deploy WebAssembly functions and read their logs on a running server.
    Functions(Box<functions::FunctionsCli>),
    /// Publish to durable topics, pull from subscriptions and follow a topic on a running server.
    Queues(Box<queues::QueuesCli>),
    /// Follow long-running jobs on a running server.
    Jobs(Box<queues::JobsCli>),
}

fn main() -> ExitCode {
    // `.env` first: its values feed the environment layer of the config loader.
    if let Err(e) = dotenv::load_from_cwd() {
        eprintln!("error: {e:#}");
        return ExitCode::FAILURE;
    }
    let cli = Cli::parse();
    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: cannot start the async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(cli)) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    let g = &cli.global;
    match cli.command {
        Command::Init { dir } => commands::init(g, &dir).await,
        Command::Serve => commands::serve(g).await,
        Command::Migrate { status } => commands::migrate(g, status).await,
        Command::Status { json } => commands::status(g, json).await,
        Command::Doctor { json, deep } => doctor::run(g, json, deep).await,
        Command::Backup(b) => dataops::run_backup(g, *b).await,
        Command::Restore(r) => dataops::run_restore(g, *r).await,
        Command::Export { command } => dataops::run_export(g, command).await,
        Command::Import { command } => dataops::run_import(g, command).await,
        Command::Version { json } => commands::version(json).await,
        Command::Admin { command } => admin::run(g, command).await,
        Command::Functions(f) => functions::run(g, f.server, f.command).await,
        Command::Queues(q) => queues::run_queues(g, q.server, q.command).await,
        Command::Jobs(j) => queues::run_jobs(g, j.server, j.command).await,
    }
}
