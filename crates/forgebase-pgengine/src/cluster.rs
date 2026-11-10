//! `initdb` (ADR-0028 §4.2) and the bootstrap of roles, the `forgebase`
//! database and its schemas (ADR-0028 §7.6, rls-coverage §3.2).

use std::path::Path;
use std::process::Stdio;

use sqlx::postgres::PgConnectOptions;
use sqlx::{AssertSqlSafe, ConnectOptions, Connection, PgConnection};

use crate::discover::EngineDir;
use crate::error::{EngineError, Result};
use crate::fsutil;
use crate::secret::{Role, RoleSecrets};

/// Database ForgeBase uses inside the cluster (also `cron.database_name`).
pub const DATABASE: &str = "forgebase";

/// NOLOGIN roles the bootstrap creates (rls-coverage §3.2).
pub const OWNER_ROLE: &str = "forgebase_owner";
/// Role for requests in `enforce` mode and direct logins (no `BYPASSRLS`).
pub const RLS_ROLE: &str = "forgebase_rls";
/// Role of the SQL editor and administrator logins (FB-153 grants it).
pub const SQL_ADMIN_ROLE: &str = "forgebase_sql_admin";

/// `search_path` of the server and reader roles (ADR-0028 §7.6).
pub const SEARCH_PATH: &str = "forgebase, public";

/// Whether `pgdata` holds a cluster.
pub(crate) fn exists(pgdata: &Path) -> bool {
    pgdata.join("PG_VERSION").is_file()
}

/// Major version recorded in `PG_VERSION`.
pub(crate) fn data_major(pgdata: &Path) -> Result<u32> {
    let path = pgdata.join("PG_VERSION");
    let text = std::fs::read_to_string(&path).map_err(|e| EngineError::io("read", &path, e))?;
    text.trim().parse().map_err(|_| EngineError::Cluster {
        dir: pgdata.to_path_buf(),
        reason: format!("PG_VERSION holds {:?}", text.trim()),
    })
}

/// Runs `initdb` into `<pgdata>.partial` and renames it into place, so an
/// interrupted `initdb` never leaves a half-made cluster behind.
pub(crate) async fn initdb(engine: &EngineDir, pgdata: &Path, secrets: &RoleSecrets, no_sync: bool) -> Result<()> {
    if pgdata.exists() {
        let empty = std::fs::read_dir(pgdata)
            .map_err(|e| EngineError::io("list", pgdata, e))?
            .next()
            .is_none();
        if !empty {
            return Err(EngineError::Cluster {
                dir: pgdata.to_path_buf(),
                reason: "the directory is not empty but holds no PostgreSQL cluster (no PG_VERSION); \
                         move it away or delete it"
                    .into(),
            });
        }
        std::fs::remove_dir(pgdata).map_err(|e| EngineError::io("remove", pgdata, e))?;
    }
    let parent = pgdata.parent().ok_or_else(|| EngineError::Cluster {
        dir: pgdata.to_path_buf(),
        reason: "no parent directory".into(),
    })?;
    fsutil::private_dir(parent)?;
    let partial = parent.join("data.partial");
    let _ = fsutil::remove_tree(&partial);
    let pwfile = parent.join(".initdb-password");
    let super_pw = secrets
        .password(Role::Super)
        .ok_or_else(|| EngineError::Config("roles.secret has no forgebase_super password".into()))?;
    {
        use std::io::Write;
        let mut f = fsutil::create_private_file(&pwfile)?;
        f.write_all(super_pw.as_bytes())
            .map_err(|e| EngineError::io("write", &pwfile, e))?;
    }
    let mut cmd = tokio::process::Command::new(engine.bin("initdb"));
    cmd.arg("-D")
        .arg(&partial)
        .args(["-U", Role::Super.name()])
        .arg(format!("--pwfile={}", pwfile.display()))
        .args([
            "--auth-local=scram-sha-256",
            "--auth-host=scram-sha-256",
            "-E",
            "UTF8",
            "--locale-provider=builtin",
            "--builtin-locale=C.UTF-8",
            // libc categories (messages, numbers, money, time) stay
            // host-independent too; collation comes from the builtin provider.
            "--locale=C",
            "--data-checksums",
            "--no-instructions",
        ]);
    if no_sync {
        cmd.arg("--no-sync");
    }
    cmd.env_remove("PGDATA")
        .env("TZ", "UTC")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = cmd.output().await;
    let _ = std::fs::remove_file(&pwfile);
    let out = out.map_err(|e| EngineError::io("run", &engine.bin("initdb"), e))?;
    if !out.status.success() {
        let _ = fsutil::remove_tree(&partial);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let tail: Vec<&str> = stderr
            .lines()
            .rev()
            .take(15)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        return Err(EngineError::Initdb(tail.join("\n")));
    }
    std::fs::rename(&partial, pgdata).map_err(|e| EngineError::io("rename into place", pgdata, e))?;
    tracing::info!(target: "forgebase::postgres", pgdata = %pgdata.display(), "initialised a new PostgreSQL cluster");
    Ok(())
}

/// Removes a `data.partial` left by an interrupted `initdb`.
pub(crate) fn clean_partial(pgdata: &Path) {
    if let Some(parent) = pgdata.parent() {
        let partial = parent.join("data.partial");
        if partial.exists() {
            tracing::info!(target: "forgebase::postgres", path = %partial.display(), "removing an interrupted initdb");
            let _ = fsutil::remove_tree(&partial);
        }
    }
}

/// Connection options for `role` on `database` at the given endpoint.
pub(crate) fn connect_options(
    ep: &crate::process::Endpoint,
    role: &str,
    password: &str,
    database: &str,
) -> PgConnectOptions {
    let mut o = PgConnectOptions::new()
        .username(role)
        .password(password)
        .database(database)
        .application_name("forgebase-engine")
        .port(ep.port);
    o = match (&ep.socket_dir, ep.tcp) {
        (Some(dir), _) => o.socket(dir).host("localhost"),
        (None, Some(a)) => o.host(&a.ip().to_string()).port(a.port()),
        (None, None) => o.host("127.0.0.1"),
    };
    o.disable_statement_logging()
}

async fn connect(opts: &PgConnectOptions) -> Result<PgConnection> {
    PgConnection::connect_with(opts)
        .await
        .map_err(|e| EngineError::Sql(format!("connect: {e}")))
}

async fn exec(conn: &mut PgConnection, sql: String) -> Result<()> {
    sqlx::raw_sql(AssertSqlSafe(sql))
        .execute(&mut *conn)
        .await
        .map(|_| ())
        .map_err(|e| EngineError::Sql(redact(&e.to_string())))
}

/// Never repeat a password from SQL into an error message.
fn redact(msg: &str) -> String {
    match msg.find("PASSWORD '") {
        Some(i) => format!("{}PASSWORD '<redacted>' …", &msg[..i]),
        None => msg.to_owned(),
    }
}

/// Settings of the bootstrap session: statements carrying passwords are
/// never written to the engine log, even when one fails.
const QUIET: &str = "SET log_statement = 'none'; SET log_min_error_statement = 'panic'; \
                     SET log_min_duration_statement = -1; SET client_min_messages = 'warning';";

/// Creates or updates the bootstrap roles, the `forgebase` database and its
/// schemas. Idempotent; runs on every start of a writable cluster, so a
/// later ForgeBase can add roles or grants to existing clusters.
pub(crate) async fn bootstrap(ep: &crate::process::Endpoint, secrets: &RoleSecrets) -> Result<()> {
    let pw = |r: Role| {
        secrets
            .password(r)
            .ok_or_else(|| EngineError::Config(format!("roles.secret has no {} password", r.name())))
    };
    let super_pw = pw(Role::Super)?;
    let mut conn = connect(&connect_options(ep, Role::Super.name(), super_pw, "postgres")).await?;
    exec(&mut conn, QUIET.into()).await?;
    let (server, reader) = (Role::Server.name(), Role::Reader.name());
    exec(
        &mut conn,
        format!(
            "DO $fb$ BEGIN
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = '{OWNER_ROLE}') THEN CREATE ROLE {OWNER_ROLE} NOLOGIN; END IF;
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = '{RLS_ROLE}') THEN CREATE ROLE {RLS_ROLE} NOLOGIN; END IF;
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = '{SQL_ADMIN_ROLE}') THEN CREATE ROLE {SQL_ADMIN_ROLE} NOLOGIN; END IF;
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = '{server}') THEN CREATE ROLE {server} LOGIN; END IF;
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = '{reader}') THEN CREATE ROLE {reader} LOGIN; END IF;
             END $fb$;
             ALTER ROLE {OWNER_ROLE} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION BYPASSRLS;
             ALTER ROLE {RLS_ROLE} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
             ALTER ROLE {SQL_ADMIN_ROLE} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION BYPASSRLS;
             ALTER ROLE {server} LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION BYPASSRLS;
             ALTER ROLE {reader} LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION BYPASSRLS;
             ALTER ROLE {server} SET search_path = {SEARCH_PATH};
             ALTER ROLE {reader} SET search_path = {SEARCH_PATH};
             ALTER ROLE {reader} SET default_transaction_read_only = on;
             GRANT {OWNER_ROLE} TO {server};
             GRANT {RLS_ROLE} TO {server};"
        ),
    )
    .await?;
    exec(
        &mut conn,
        format!(
            "ALTER ROLE {server} PASSWORD '{}'; ALTER ROLE {reader} PASSWORD '{}';",
            pw(Role::Server)?,
            pw(Role::Reader)?
        ),
    )
    .await?;
    let created: Option<(i32,)> = sqlx::query_as("SELECT 1 FROM pg_database WHERE datname = $1")
        .bind(DATABASE)
        .fetch_optional(&mut conn)
        .await
        .map_err(|e| EngineError::Sql(e.to_string()))?;
    let fresh = created.is_none();
    if fresh {
        exec(&mut conn, format!("CREATE DATABASE {DATABASE} OWNER {OWNER_ROLE}")).await?;
    }
    exec(
        &mut conn,
        format!(
            "REVOKE ALL ON DATABASE {DATABASE} FROM PUBLIC;
             GRANT CONNECT ON DATABASE {DATABASE} TO {reader};"
        ),
    )
    .await?;
    let _ = conn.close().await;

    let mut conn = connect(&connect_options(ep, Role::Super.name(), super_pw, DATABASE)).await?;
    exec(&mut conn, QUIET.into()).await?;
    exec(
        &mut conn,
        format!(
            "CREATE SCHEMA IF NOT EXISTS forgebase AUTHORIZATION {OWNER_ROLE};
             ALTER SCHEMA forgebase OWNER TO {OWNER_ROLE};
             ALTER SCHEMA public OWNER TO {OWNER_ROLE};
             REVOKE ALL ON SCHEMA forgebase FROM PUBLIC;
             REVOKE CREATE ON SCHEMA public FROM PUBLIC;
             GRANT USAGE ON SCHEMA forgebase, public TO {reader};
             ALTER DEFAULT PRIVILEGES FOR ROLE {OWNER_ROLE} IN SCHEMA forgebase, public GRANT SELECT ON TABLES TO {reader};
             ALTER DEFAULT PRIVILEGES FOR ROLE {server} IN SCHEMA forgebase, public GRANT SELECT ON TABLES TO {reader};"
        ),
    )
    .await?;
    if fresh {
        exec(
            &mut conn,
            format!("GRANT SELECT ON ALL TABLES IN SCHEMA forgebase, public TO {reader};"),
        )
        .await?;
    }
    let _ = conn.close().await;
    Ok(())
}

/// Connects once as `forgebase_super`, proving the password works.
pub(crate) async fn probe(ep: &crate::process::Endpoint, secrets: &RoleSecrets) -> Result<()> {
    let pw = secrets
        .password(Role::Super)
        .ok_or_else(|| EngineError::Config("roles.secret has no forgebase_super password".into()))?;
    let conn = connect(&connect_options(ep, Role::Super.name(), pw, "postgres")).await?;
    let _ = conn.close().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords_never_reach_error_text() {
        let r = redact("error near ALTER ROLE x PASSWORD 'abc123' something");
        assert!(!r.contains("abc123"), "{r}");
        assert_eq!(redact("plain"), "plain");
    }
}
