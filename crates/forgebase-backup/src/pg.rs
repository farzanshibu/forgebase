//! The PostgreSQL side of backups (FB-150, ADR-0028 §5): how to reach the
//! cluster, and the engine's own tools that copy, check and reload it.
//!
//! * managed (bundled engine): `pg_basebackup -Fp -X stream` over the Unix
//!   socket as `forgebase_super`, checked with `pg_verifybackup`;
//! * external server: `pg_dump -Fc` of the ForgeBase schema, loaded with
//!   `pg_restore`.
//!
//! This crate does not depend on the engine crate: the server and the CLI
//! describe the cluster in a [`PgSource`]. Passwords never reach argv or the
//! environment; the tools read a private `PGPASSFILE` that is removed
//! afterwards.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use crate::error::{BackupError, Result, io};
use crate::fsutil::{create_private, mkdir_private};

/// The schema ForgeBase owns on an external server.
pub const FORGEBASE_SCHEMA: &str = "forgebase";

/// A password that never prints.
#[derive(Clone, PartialEq, Eq)]
pub struct Password(String);

impl Password {
    /// Wraps a password.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Password {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Password(<redacted>)")
    }
}

/// How the backup tools reach the cluster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PgEndpoint {
    /// The bundled engine, through its Unix socket as its superuser.
    Managed {
        /// Directory of the socket.
        socket_dir: PathBuf,
        /// Port (the socket file's suffix).
        port: u16,
        /// Login (`forgebase_super`).
        user: String,
        /// Its password.
        password: Password,
        /// `<data dir>/postgres`: holds `data/`, `roles.secret`, `nodes/`.
        cluster_dir: PathBuf,
    },
    /// An external server, by connection URL.
    External {
        /// `postgres://user:password@host:port/database`.
        url: Password,
    },
}

/// What a backup of a PostgreSQL database needs besides the [`Database`](forgebase_db::Database)
/// handle: the engine's tools and the way in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PgSource {
    /// Engine directory holding `bin/` (`None`: look the tools up on `PATH`).
    pub engine_root: Option<PathBuf>,
    /// Server version, such as `18.3`.
    pub version: String,
    /// The cluster.
    pub endpoint: PgEndpoint,
}

impl PgSource {
    /// The bundled engine of a data directory.
    pub fn managed(
        engine_root: Option<PathBuf>,
        version: impl Into<String>,
        socket_dir: impl Into<PathBuf>,
        port: u16,
        user: impl Into<String>,
        password: Password,
        cluster_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            engine_root,
            version: version.into(),
            endpoint: PgEndpoint::Managed {
                socket_dir: socket_dir.into(),
                port,
                user: user.into(),
                password,
                cluster_dir: cluster_dir.into(),
            },
        }
    }

    /// An external server.
    pub fn external(engine_root: Option<PathBuf>, version: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            engine_root,
            version: version.into(),
            endpoint: PgEndpoint::External {
                url: Password::new(url),
            },
        }
    }

    /// Only the engine's programs, for commands that read a backup and never
    /// connect (`verify`, the version check of `restore`).
    pub fn tools_only(engine_root: Option<PathBuf>, version: impl Into<String>) -> Self {
        Self::external(engine_root, version, "postgres://localhost/postgres")
    }

    /// Whether this is the bundled engine.
    pub fn is_managed(&self) -> bool {
        matches!(self.endpoint, PgEndpoint::Managed { .. })
    }

    /// `managed` or `external`.
    pub fn mode(&self) -> &'static str {
        if self.is_managed() { "managed" } else { "external" }
    }

    /// The major version (`18` for `18.3`).
    pub fn major(&self) -> Option<u32> {
        major_of(&self.version)
    }

    fn tool(&self, name: &str) -> PathBuf {
        match &self.engine_root {
            Some(root) => root
                .join("bin")
                .join(format!("{name}{}", std::env::consts::EXE_SUFFIX)),
            None => PathBuf::from(name),
        }
    }

    /// Connection flags (no database), the database name, user and password.
    fn connection(&self) -> Result<Conn> {
        match &self.endpoint {
            PgEndpoint::Managed {
                socket_dir,
                port,
                user,
                password,
                ..
            } => Ok(Conn {
                args: vec![
                    "-h".into(),
                    socket_dir.display().to_string(),
                    "-p".into(),
                    port.to_string(),
                    "-U".into(),
                    user.clone(),
                ],
                database: "forgebase".to_owned(),
                user: user.clone(),
                password: password.expose().to_owned(),
            }),
            PgEndpoint::External { url } => {
                let u = url::Url::parse(url.expose())
                    .map_err(|e| BackupError::Invalid(format!("the database URL is not usable: {e}")))?;
                let user = percent_decode(u.username());
                let password = percent_decode(u.password().unwrap_or_default());
                let mut args = Vec::new();
                let query = |key: &str| u.query_pairs().find(|(k, _)| k == key).map(|(_, v)| v.into_owned());
                match (query("host"), u.host_str()) {
                    (Some(dir), _) => args.extend(["-h".to_owned(), dir]),
                    (None, Some(h)) if !h.is_empty() => {
                        args.extend(["-h".to_owned(), h.trim_matches(['[', ']']).to_owned()]);
                    }
                    _ => {}
                }
                if let Some(p) = query("port").and_then(|v| v.parse::<u16>().ok()).or_else(|| u.port()) {
                    args.extend(["-p".to_owned(), p.to_string()]);
                }
                if !user.is_empty() {
                    args.extend(["-U".to_owned(), user.clone()]);
                }
                let db = percent_decode(u.path().trim_start_matches('/'));
                Ok(Conn {
                    args,
                    database: if db.is_empty() { user.clone() } else { db },
                    user,
                    password,
                })
            }
        }
    }

    /// Runs `tool args`, with the password in a private passfile under
    /// `tmp` when `login` is set. Returns stdout.
    async fn run(&self, tool: &str, args: Vec<String>, tmp: &Path, login: bool) -> Result<String> {
        let bin = self.tool(tool);
        if let Some(root) = &self.engine_root
            && !bin.is_file()
        {
            return Err(BackupError::Invalid(format!(
                "{} is missing: the engine at {} has no {tool}",
                bin.display(),
                root.display()
            )));
        }
        let pass = if login {
            let c = self.connection()?;
            Some(PassFile::create(tmp, &c.user, &c.password)?)
        } else {
            None
        };
        let mut cmd = tokio::process::Command::new(&bin);
        cmd.args(&args)
            .env("PGAPPNAME", "forgebase-backup")
            .env_remove("PGPASSWORD")
            .env_remove("PGSERVICE")
            .env_remove("PGHOSTADDR")
            .env_remove("PGDATABASE")
            .env_remove("PGUSER")
            .env_remove("PGHOST")
            .env_remove("PGPORT")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(p) = &pass {
            cmd.env("PGPASSFILE", p.path());
        }
        let out = cmd.output().await.map_err(|e| BackupError::Io {
            path: bin.clone(),
            source: e,
        })?;
        drop(pass);
        if out.status.success() {
            return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        let mut tail: Vec<&str> = stderr.lines().rev().take(8).collect();
        tail.reverse();
        Err(BackupError::Internal(format!(
            "{tool} failed ({}): {}",
            out.status.code().map_or("signal".to_owned(), |c| format!("exit {c}")),
            tail.join("\n").trim()
        )))
    }

    /// `pg_basebackup` into `dest` (which must not exist): a plain copy of
    /// `PGDATA` with its WAL, checksummed (`backup_manifest`).
    ///
    /// # Errors
    /// The tool is missing or fails; this source is not the bundled engine.
    pub async fn basebackup(&self, dest: &Path, tmp: &Path, label: &str) -> Result<()> {
        if !self.is_managed() {
            return Err(BackupError::Invalid(
                "a physical backup needs the bundled engine; external servers are backed up logically (pg_dump)".into(),
            ));
        }
        let mut args = self.connection()?.args;
        args.extend(
            [
                "-D",
                &dest.display().to_string(),
                "-Fp",
                "-X",
                "stream",
                "--checkpoint=fast",
                "--manifest-checksums=SHA256",
                "--no-password",
                "-l",
                label,
            ]
            .map(str::to_owned),
        );
        self.run("pg_basebackup", args, tmp, true).await.map(|_| ())
    }

    /// `pg_verifybackup` on a physical backup directory (the `postgres/`
    /// directory of a backup). Returns the tool's complaint, empty when the
    /// backup is sound.
    ///
    /// # Errors
    /// The tool cannot be run.
    pub async fn verifybackup(&self, dir: &Path, tmp: &Path) -> Result<Vec<String>> {
        let args = vec!["--exit-on-error".to_owned(), dir.display().to_string()];
        match self.run("pg_verifybackup", args, tmp, false).await {
            Ok(_) => Ok(Vec::new()),
            Err(BackupError::Internal(msg)) if msg.starts_with("pg_verifybackup failed") => Ok(vec![msg]),
            Err(e) => Err(e),
        }
    }

    /// `pg_dump` into `dest`.
    ///
    /// # Errors
    /// The tool is missing or fails.
    pub async fn dump(&self, dest: &Path, tmp: &Path, plan: &DumpPlan) -> Result<()> {
        let conn = self.connection()?;
        let mut args = conn.args;
        args.extend(
            [
                if plan.plain { "-Fp" } else { "-Fc" },
                "--no-owner",
                "--no-password",
                "-d",
                &conn.database,
                "-f",
                &dest.display().to_string(),
            ]
            .map(str::to_owned),
        );
        if plan.schema_only {
            args.push("--schema-only".into());
        }
        if plan.portable {
            args.push("--no-acl".into());
        }
        if plan.forgebase_schema_only {
            args.extend(["-n".to_owned(), FORGEBASE_SCHEMA.to_owned()]);
        }
        self.run("pg_dump", args, tmp, true).await.map(|_| ())
    }

    /// `pg_restore --list`: proves the dump file is readable.
    ///
    /// # Errors
    /// The tool is missing or the file is not a dump.
    pub async fn list_dump(&self, file: &Path, tmp: &Path) -> Result<()> {
        self.run("pg_restore", vec!["--list".to_owned(), file.display().to_string()], tmp, false)
            .await
            .map(|_| ())
    }

    /// `pg_restore` of a custom-format dump of the ForgeBase schema into the
    /// database: the schema's objects are dropped and recreated, in one
    /// transaction (a failure changes nothing).
    ///
    /// # Errors
    /// The tool is missing or the load fails.
    pub async fn restore_dump(&self, file: &Path, tmp: &Path) -> Result<()> {
        let conn = self.connection()?;
        let mut args = conn.args;
        args.extend(
            [
                "--clean",
                "--if-exists",
                "--no-owner",
                "--no-password",
                "--single-transaction",
                "--exit-on-error",
                "-d",
                &conn.database,
                "-n",
                FORGEBASE_SCHEMA,
            ]
            .map(str::to_owned),
        );
        args.push(file.display().to_string());
        self.run("pg_restore", args, tmp, true).await.map(|_| ())
    }
}

struct Conn {
    args: Vec<String>,
    database: String,
    user: String,
    password: String,
}

/// What `pg_dump` writes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DumpPlan {
    /// Only the `forgebase` schema (backups of an external server share
    /// their database with other applications); `forgebase db dump` takes
    /// everything.
    pub forgebase_schema_only: bool,
    /// Plain SQL text instead of the custom format.
    pub plain: bool,
    /// Leave ownership and privileges out, so any PostgreSQL can load it
    /// (`forgebase db dump`); a backup keeps them.
    pub portable: bool,
    /// No rows.
    pub schema_only: bool,
}

/// A private `.pgpass` file, removed on drop.
struct PassFile(PathBuf);

impl PassFile {
    fn create(dir: &Path, user: &str, password: &str) -> Result<Self> {
        use std::io::Write;
        mkdir_private(dir).map_err(io(dir))?;
        let path = dir.join(format!(
            ".pgpass-{}-{}",
            std::process::id(),
            forgebase_core::now_rfc3339().replace([':', '-', '.'], "")
        ));
        let _ = std::fs::remove_file(&path);
        let mut f = create_private(&path).map_err(io(&path))?;
        writeln!(f, "*:*:*:{}:{}", pgpass_field(user), pgpass_field(password)).map_err(io(&path))?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for PassFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn pgpass_field(s: &str) -> String {
    s.replace('\\', "\\\\").replace(':', "\\:")
}

fn percent_decode(s: &str) -> String {
    url::form_urlencoded::parse(format!("x={}", s.replace('+', "%2B")).as_bytes())
        .next()
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default()
}

/// `18` for `18.3`, `17` for `17.0`.
pub fn major_of(version: &str) -> Option<u32> {
    version.trim().split('.').next()?.parse().ok()
}

/// The pid of a live postmaster of the cluster at `pgdata`, from `postmaster.pid`.
pub fn running_postmaster(pgdata: &Path) -> Option<u32> {
    let text = std::fs::read_to_string(pgdata.join("postmaster.pid")).ok()?;
    let pid: u32 = text.lines().next()?.trim().parse().ok()?;
    #[cfg(unix)]
    {
        let alive = std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        alive.then_some(pid)
    }
    #[cfg(not(unix))]
    {
        Some(pid)
    }
}

/// Refuses a cluster with more than the primary's single data set
/// (ADR-0029: replicas and shards live under `postgres/nodes/`).
///
/// # Errors
/// [`BackupError::Invalid`] naming FB-179.
pub fn ensure_single_set(data_dir: &Path) -> Result<()> {
    let nodes = data_dir.join("postgres").join("nodes");
    let any = std::fs::read_dir(&nodes).is_ok_and(|mut d| d.next().is_some());
    if any {
        return Err(BackupError::Invalid(format!(
            "{} holds more than one PostgreSQL cluster (replicas or shard sets): backing up and restoring \
             them is FB-179's work and arrives as `forgebase backup --cluster` (one backup per set under one \
             directory with a combined manifest); this build backs up a single set only",
            nodes.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn major_versions_parse() {
        assert_eq!(major_of("18.3"), Some(18));
        assert_eq!(major_of(" 17 "), Some(17));
        assert_eq!(major_of("x"), None);
    }

    #[test]
    fn pgpass_fields_are_escaped() {
        assert_eq!(pgpass_field("a:b\\c"), "a\\:b\\\\c");
    }

    #[test]
    fn external_urls_become_flags() {
        let s = PgSource::external(None, "18.3", "postgres://fb:p%40ss@db.example.com:6432/app");
        let c = s.connection().unwrap();
        assert_eq!((c.user.as_str(), c.password.as_str(), c.database.as_str()), ("fb", "p@ss", "app"));
        assert_eq!(c.args, ["-h", "db.example.com", "-p", "6432", "-U", "fb"].map(str::to_owned));
        assert!(!format!("{s:?}").contains("p%40ss"));
    }

    #[test]
    fn socket_urls_become_flags() {
        let s = PgSource::external(None, "18.3", "postgres://u:pw@localhost/db?host=/tmp/run&port=5433");
        let c = s.connection().unwrap();
        assert_eq!(c.args[..4], ["-h", "/tmp/run", "-p", "5433"].map(str::to_owned));
    }

    #[test]
    fn a_node_directory_is_refused_with_fb_179() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(ensure_single_set(tmp.path()).is_ok());
        std::fs::create_dir_all(tmp.path().join("postgres/nodes/replica1")).unwrap();
        let e = ensure_single_set(tmp.path()).unwrap_err().to_string();
        assert!(e.contains("FB-179") && e.contains("backup --cluster"), "{e}");
    }
}
