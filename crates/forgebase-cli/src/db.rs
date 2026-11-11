//! `forgebase db`: the database engine of a project (FB-148).
//!
//! - `db status`: backend, engine, cluster, socket, size, connections and
//!   extensions. Never starts the engine: a stopped server shows what is on
//!   disk.
//! - `db psql [ARGS…]`: the bundled engine's `psql`, logged in through the
//!   Unix socket only (human decision 2026-11-11: TCP logins stay off by
//!   default). With a server running it joins that server's engine; without
//!   one it starts the engine for the session and stops it afterwards.

use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, bail};
use clap::{Args, Subcommand};
use forgebase_core::DataLayout;
use forgebase_core::config::PostgresMode;
use forgebase_db::{Backend, Statement};
use forgebase_pgengine::{Role, RoleSecrets};
use forgebase_server::DataDirLock;
use forgebase_server::database::{self, Access, BackendMarker};
use serde_json::{Value, json};

use crate::GlobalArgs;
use crate::commands::{explicit_backend, open_attached, quiet_logging, sqlite_notice};

/// `forgebase db <command>`.
#[derive(Debug, Args)]
pub struct DbCli {
    #[command(subcommand)]
    pub command: DbCommand,
}

/// Subcommands of `forgebase db`.
#[derive(Debug, Subcommand)]
pub enum DbCommand {
    /// Show the database engine: backend, cluster, socket, size and extensions.
    Status {
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Write a `pg_dump` of the database that any PostgreSQL can load (works
    /// while the server runs). `forgebase backup` is the restorable backup.
    Dump(crate::dataops::DumpCli),
    /// Open psql on the bundled PostgreSQL, through its Unix socket.
    ///
    /// Arguments after the options go to psql, for example
    /// `forgebase db psql -c 'select 1'`. Logs in as forgebase_server (the
    /// server's own login: reads and writes every ForgeBase table, bypasses
    /// row-level security, not a superuser).
    Psql {
        /// Log in as forgebase_reader (read-only).
        #[arg(long, conflicts_with = "superuser")]
        read_only: bool,
        /// Log in as forgebase_super (the cluster's superuser).
        #[arg(long)]
        superuser: bool,
        /// Arguments passed to psql.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, value_name = "PSQL_ARGS")]
        args: Vec<String>,
    },
}

/// Runs a `db` subcommand.
pub async fn run(g: &GlobalArgs, cli: DbCli) -> anyhow::Result<ExitCode> {
    quiet_logging(g)?;
    match cli.command {
        DbCommand::Status { json } => status(g, json).await,
        DbCommand::Dump(args) => crate::dataops::run_db_dump(g, args).await,
        DbCommand::Psql {
            read_only,
            superuser,
            args,
        } => {
            let access = if read_only {
                Access::Read
            } else if superuser {
                Access::Super
            } else {
                Access::Write
            };
            psql(g, access, &args).await
        }
    }
}

/// Bytes used by the files under `dir`.
fn dir_size(dir: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    rd.flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            Ok(t) if t.is_file() => e.metadata().map_or(0, |m| m.len()),
            _ => 0,
        })
        .sum()
}

#[allow(clippy::too_many_lines)]
async fn status(g: &GlobalArgs, as_json: bool) -> anyhow::Result<ExitCode> {
    let loaded = g.load()?;
    let config = &loaded.config;
    let layout = DataLayout::new(&config.data_dir);
    let marker = database::resolve(config, explicit_backend(&loaded), &layout)?;
    let initialized = database::is_initialized(&layout);
    let pg = &config.database.postgres;
    let mut out = json!({
        "backend": marker.backend().as_str(),
        "mode": marker.mode().map(PostgresMode::as_str),
        "initialized": initialized,
        "marker": database::read_marker(&layout)?.map(BackendMarker::as_str),
    });
    let server_running = DataDirLock::probe(&layout).is_some();
    out["server_running"] = json!(server_running);

    match marker {
        BackendMarker::Sqlite => {
            out["path"] = json!(layout.database_path());
            out["size_bytes"] = json!(std::fs::metadata(layout.database_path()).map_or(0, |m| m.len()));
        }
        BackendMarker::PostgresManaged => {
            let el = database::engine_layout(&layout);
            let pgdata = el.pgdata();
            out["cluster"] = json!(pgdata);
            out["cluster_exists"] = json!(pgdata.join("PG_VERSION").is_file());
            out["cluster_major"] = json!(
                std::fs::read_to_string(pgdata.join("PG_VERSION"))
                    .ok()
                    .map(|v| v.trim().to_owned())
            );
            out["cluster_bytes_on_disk"] = json!(dir_size(&pgdata));
            out["engine_dir"] = json!(database::engine_root(&config.database, &layout));
            out["bundled_engine"] = json!(forgebase_pgengine::HAS_EMBEDDED);
            out["listen"] = json!(if pg.listen.is_empty() {
                Value::Null
            } else {
                json!(pg.listen)
            });
            match database::postmaster(&layout) {
                Some(pm) => {
                    out["engine"] = json!({
                        "running": true,
                        "pid": pm.pid,
                        "status": pm.status,
                        "socket_dir": pm.socket_dir,
                        "port": pm.port,
                    });
                }
                None => out["engine"] = json!({ "running": false }),
            }
        }
        BackendMarker::PostgresExternal => {
            out["url"] = json!(pg.url.as_ref().map(|u| u.redacted()));
        }
    }
    out["extensions"] = json!(
        pg.extensions
            .list()
            .iter()
            .map(|(f, e, m)| json!({"feature": f, "extension": e, "mode": m.as_str()}))
            .collect::<Vec<_>>()
    );

    // Live numbers only from a running engine (or any external server).
    let live = initialized
        && match marker {
            BackendMarker::PostgresManaged => {
                server_running && database::postmaster(&layout).is_some_and(|p| p.status == "ready")
            }
            _ => true,
        };
    if live {
        match open_attached(&loaded, Access::Read).await {
            Ok(primary) => {
                let db = primary.db();
                if let Ok(info) = db.info().await {
                    out["size_bytes"] = json!(info.size_bytes);
                    out["server_version"] = json!(info.server_version);
                    out["schema_version"] = json!(info.schema_version);
                    if let Some(c) = info.connections {
                        out["connections"] = json!(c);
                    }
                }
                if marker != BackendMarker::Sqlite {
                    let names = pg
                        .extensions
                        .list()
                        .iter()
                        .map(|(_, e, _)| format!("'{e}'"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    let stmt = Statement::raw(format!(
                        "SELECT a.name::text, a.default_version::text, e.extversion::text \
                         FROM pg_available_extensions a LEFT JOIN pg_extension e ON e.extname = a.name \
                         WHERE a.name IN ({names}) ORDER BY 1"
                    ));
                    if let Ok(rows) = db.fetch_all(&stmt).await
                        && let Some(list) = out["extensions"].as_array_mut()
                    {
                        for row in rows.rows {
                            let text = |i: usize| row.get(i).and_then(|v| v.as_str()).map(str::to_owned);
                            if let Some(item) = list.iter_mut().find(|x| x["extension"].as_str() == text(0).as_deref())
                            {
                                item["available"] = json!(text(1));
                                item["installed"] = json!(text(2));
                            }
                        }
                    }
                }
                primary.close().await;
            }
            Err(e) => out["error"] = json!(format!("{e:#}")),
        }
    }
    if let Some(n) = sqlite_notice(initialized.then_some(marker)) {
        out["notice"] = json!(n);
    }

    if as_json {
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(ExitCode::SUCCESS);
    }
    println!("backend:     {}", marker.label());
    if !initialized {
        println!("database:    not initialized (run `forgebase init`)");
    }
    match marker {
        BackendMarker::Sqlite => println!(
            "file:        {} ({} bytes)",
            layout.database_path().display(),
            out["size_bytes"]
        ),
        BackendMarker::PostgresManaged => {
            println!(
                "cluster:     {} (PostgreSQL {}, {} bytes on disk)",
                out["cluster"].as_str().unwrap_or_default(),
                out["cluster_major"].as_str().unwrap_or("?"),
                out["cluster_bytes_on_disk"]
            );
            println!(
                "engine dir:  {}",
                out["engine_dir"]
                    .as_str()
                    .unwrap_or("not extracted yet (the first start extracts it)")
            );
            let e = &out["engine"];
            if e["running"] == true {
                println!(
                    "engine:      running, pid {} ({}), socket {} port {}",
                    e["pid"],
                    e["status"].as_str().unwrap_or("?"),
                    e["socket_dir"].as_str().unwrap_or("-"),
                    e["port"]
                );
            } else {
                println!("engine:      stopped (it runs while `forgebase serve` runs)");
            }
            println!(
                "tcp logins:  {}",
                out["listen"]
                    .as_str()
                    .map_or("off (Unix socket only)".to_owned(), |l| format!("on, {l}"))
            );
        }
        BackendMarker::PostgresExternal => println!(
            "server:      {}",
            out["url"].as_str().unwrap_or("not set (FORGEBASE_DATABASE_URL)")
        ),
    }
    if out.get("server_version").is_some() {
        println!(
            "database:    {} bytes, schema v{}, server {}{}",
            out["size_bytes"],
            out["schema_version"],
            out["server_version"].as_str().unwrap_or("?"),
            out.get("connections")
                .map(|c| format!(", {c} connection(s)"))
                .unwrap_or_default()
        );
    }
    if marker != BackendMarker::Sqlite {
        for x in out["extensions"].as_array().into_iter().flatten() {
            let have = match (
                x.get("installed").and_then(Value::as_str),
                x.get("available").and_then(Value::as_str),
            ) {
                (Some(v), _) => format!("installed {v}"),
                (None, Some(v)) => format!("available {v}"),
                _ if live => "not available".to_owned(),
                _ => "unknown while stopped".to_owned(),
            };
            println!(
                "extension:   {:<8} {:<8} {:<5} {have}",
                x["feature"].as_str().unwrap_or_default(),
                x["extension"].as_str().unwrap_or_default(),
                x["mode"].as_str().unwrap_or_default()
            );
        }
    }
    if let Some(e) = out["error"].as_str() {
        println!("error:       {e}");
    }
    if let Some(n) = out["notice"].as_str() {
        println!("notice:      {n}");
    }
    Ok(ExitCode::SUCCESS)
}

/// A `.pgpass` line value: `:` and `\` escaped.
fn pgpass_field(s: &str) -> String {
    s.replace('\\', "\\\\").replace(':', "\\:")
}

async fn psql(g: &GlobalArgs, access: Access, args: &[String]) -> anyhow::Result<ExitCode> {
    let loaded = g.load()?;
    let config = &loaded.config;
    let layout = DataLayout::new(&config.data_dir);
    match database::resolve(config, explicit_backend(&loaded), &layout)? {
        BackendMarker::PostgresManaged => {}
        BackendMarker::Sqlite => bail!(
            "this project uses SQLite: open {} with the sqlite3 shell instead",
            layout.database_path().display()
        ),
        BackendMarker::PostgresExternal => bail!(
            "db psql logs in to the bundled PostgreSQL only; for an external server run psql \"$FORGEBASE_DATABASE_URL\""
        ),
    }
    let primary = open_attached(&loaded, access).await?;
    let result = run_psql(config, &layout, &primary, access, args).await;
    primary.close().await;
    result
}

async fn run_psql(
    config: &forgebase_core::Config,
    layout: &DataLayout,
    primary: &database::PrimaryDatabase,
    access: Access,
    args: &[String],
) -> anyhow::Result<ExitCode> {
    let (socket_dir, port) = primary
        .socket()
        .context("the bundled PostgreSQL has no Unix socket (Windows): db psql needs one")?;
    let bin = match primary.engine() {
        Some(e) => e.engine().bin("psql"),
        None => database::engine_root(&config.database, layout)
            .context("cannot find the engine directory of this cluster")?
            .join("bin")
            .join(format!("psql{}", std::env::consts::EXE_SUFFIX)),
    };
    if !bin.is_file() {
        bail!("{} is missing: this engine has no psql", bin.display());
    }
    let role = match access {
        Access::Read => Role::Reader,
        Access::Write => Role::Server,
        Access::Super => Role::Super,
    };
    let secrets = RoleSecrets::load(&database::engine_layout(layout).secrets_file())?
        .context("postgres/roles.secret is missing")?;
    let password = secrets.password(role).unwrap_or_default();
    // The password goes through a private passfile, never argv or the environment.
    let tmp = layout.tmp_dir();
    std::fs::create_dir_all(&tmp).with_context(|| format!("cannot create {}", tmp.display()))?;
    let passfile = tmp.join(format!(".pgpass-{}", std::process::id()));
    {
        use std::io::Write;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts
            .open(&passfile)
            .with_context(|| format!("cannot write {}", passfile.display()))?;
        writeln!(f, "*:*:*:{}:{}", pgpass_field(role.name()), pgpass_field(password))?;
    }
    // Ctrl-C belongs to psql (it cancels the running query): keep this
    // process alive so it can stop the engine and remove the passfile.
    let ignore = tokio::spawn(async {
        loop {
            if tokio::signal::ctrl_c().await.is_err() {
                break;
            }
        }
    });
    let status = tokio::process::Command::new(&bin)
        .args(args)
        .env("PGHOST", &socket_dir)
        .env("PGPORT", port.to_string())
        .env("PGUSER", role.name())
        .env("PGDATABASE", forgebase_pgengine::DATABASE)
        .env("PGPASSFILE", &passfile)
        .env("PGAPPNAME", "forgebase-psql")
        .env_remove("PGPASSWORD")
        .env_remove("PGSERVICE")
        .env_remove("PGHOSTADDR")
        .status()
        .await;
    ignore.abort();
    let _ = std::fs::remove_file(&passfile);
    let status = status.with_context(|| format!("cannot run {}", bin.display()))?;
    Ok(match status.code() {
        Some(0) => ExitCode::SUCCESS,
        Some(c) => ExitCode::from(u8::try_from(c).unwrap_or(1)),
        None => ExitCode::FAILURE,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pgpass_fields_are_escaped() {
        assert_eq!(pgpass_field("a:b\\c"), "a\\:b\\\\c");
    }
}
