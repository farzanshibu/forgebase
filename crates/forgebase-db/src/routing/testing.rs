//! A real streaming replica for tests (feature `testing`, Unix only).
//!
//! [`ReplicaCluster::start`] runs `initdb` for a primary from the engine in
//! `FORGEBASE_TEST_PG_ENGINE` or `FORGEBASE_PG_ENGINE_DIR`, migrates a
//! database `fb` on it, clones it with `pg_basebackup -R -X stream` and
//! starts the standby, each server on its own Unix socket directory under
//! a temporary directory (no TCP). A watchdog shell stops each server when
//! the test binary exits; dropping the cluster stops them at once.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use sqlx::{AssertSqlSafe, Connection, Row};

use super::{Lsn, ReplicaNode, ReplicaOptions, ReplicaSet};
use crate::router::SetHandle;
use crate::topology::{DbNode, DbSet, NodeKind, NodeOrigin, NodeRole, NodeState};
use crate::Backend;
use crate::testing::{ENGINE_DIR_ENV, ENGINE_ENV};
use crate::{Database, DbError, DbOptions, DbTarget};

/// Port number of every test server's Unix socket (each has its own directory).
const PORT: u16 = 18_300;
/// The migrated database on the primary.
pub const DATABASE: &str = "fb";

/// The engine directory tests use, when one is configured.
pub fn engine_dir() -> Option<PathBuf> {
    [ENGINE_ENV, ENGINE_DIR_ENV]
        .iter()
        .find_map(|k| std::env::var_os(k).filter(|v| !v.is_empty()))
        .map(PathBuf::from)
}

/// A primary and one streaming standby. Stopped and deleted on drop.
#[derive(Debug)]
pub struct ReplicaCluster {
    root: PathBuf,
    bin: PathBuf,
    nodes: Vec<PathBuf>,
}

fn url(dir: &Path, database: &str) -> String {
    format!(
        "postgres://postgres@localhost/{database}?host={}&port={PORT}",
        dir.display()
    )
}

fn options(url: String) -> DbOptions {
    DbOptions {
        target: DbTarget::postgres(url, false),
        synchronous: forgebase_core::Synchronous::Full,
        busy_timeout: Duration::from_millis(5_000),
        read_pool_size: 4,
        create_if_missing: false,
        read_only: false,
    }
}

impl ReplicaCluster {
    /// Starts a primary (database [`DATABASE`], not migrated) and a
    /// standby with extra `standby_settings` (`max_standby_streaming_delay`,
    /// ...). `None` when no engine is configured.
    pub async fn start(standby_settings: &[(&str, &str)]) -> Option<Result<Self, String>> {
        let engine = engine_dir()?;
        Some(Self::start_with(&engine, standby_settings, false).await)
    }

    /// [`ReplicaCluster::start`] with [`DATABASE`] migrated to head before
    /// the standby is cloned (the `forgebase` schema, topology tables).
    pub async fn start_migrated(standby_settings: &[(&str, &str)]) -> Option<Result<Self, String>> {
        let engine = engine_dir()?;
        Some(Self::start_with(&engine, standby_settings, true).await)
    }

    async fn start_with(engine: &Path, standby_settings: &[(&str, &str)], migrate: bool) -> Result<Self, String> {
        let base = std::env::var_os("FORGEBASE_TEST_PG_TMP")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let root = base.join(format!(
            "fbr-{}-{}",
            std::process::id(),
            &uuid::Uuid::new_v4().simple().to_string()[..6]
        ));
        std::fs::create_dir_all(&root).map_err(|e| format!("create {}: {e}", root.display()))?;
        let mut cluster = Self {
            root: root.clone(),
            bin: engine.join("bin"),
            nodes: Vec::new(),
        };
        let primary = root.join("p");
        cluster.initdb(&primary)?;
        cluster.spawn(&primary, &[])?;
        wait_ready(&url(&primary, "postgres")).await?;
        let mut admin = connect(&url(&primary, "postgres")).await?;
        for sql in [
            format!("CREATE DATABASE {DATABASE}"),
            format!(
                "ALTER ROLE CURRENT_USER SET search_path = {}",
                forgebase_migrations::postgres::SEARCH_PATH
            ),
        ] {
            sqlx::raw_sql(AssertSqlSafe(sql))
                .execute(&mut admin)
                .await
                .map_err(|e| e.to_string())?;
        }
        let _ = admin.close().await;
        if migrate {
            let db = Database::open(options(url(&primary, DATABASE)))
                .await
                .map_err(|e| format!("open the primary: {e}"))?;
            db.migrate(None)
                .await
                .map_err(|e| format!("migrate the primary: {e}"))?;
            db.close().await;
        }
        let standby = root.join("s");
        std::fs::create_dir_all(&standby).map_err(|e| e.to_string())?;
        let out = Command::new(cluster.bin.join("pg_basebackup"))
            .arg("-h")
            .arg(&primary)
            .args(["-p", &PORT.to_string(), "-U", "postgres", "-R", "-X", "stream"])
            .args(["--checkpoint=fast", "--no-sync", "-D"])
            .arg(standby.join("data"))
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("run pg_basebackup: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "pg_basebackup failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        cluster.spawn(&standby, standby_settings)?;
        wait_ready(&cluster.standby_url()).await?;
        Ok(cluster)
    }

    fn initdb(&self, node: &Path) -> Result<(), String> {
        std::fs::create_dir_all(node).map_err(|e| format!("create {}: {e}", node.display()))?;
        let out = Command::new(self.bin.join("initdb"))
            .args(["-U", "postgres", "-A", "trust", "-E", "UTF8", "--locale=C", "--no-sync"])
            .args(["--no-instructions", "-D"])
            .arg(node.join("data"))
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("run initdb: {e}"))?;
        if !out.status.success() {
            return Err(format!("initdb failed: {}", String::from_utf8_lossy(&out.stderr)));
        }
        Ok(())
    }

    /// Starts the server of `node` (`<node>/data`, socket in `<node>`)
    /// under a watchdog that stops it when this process exits.
    fn spawn(&mut self, node: &Path, settings: &[(&str, &str)]) -> Result<(), String> {
        let extra = settings
            .iter()
            .map(|(k, v)| format!(" -c {k}='{v}'"))
            .collect::<String>();
        let script = format!(
            r#"
"$FB_PG_BIN/postgres" -D "$FB_PG_NODE/data" -k "$FB_PG_NODE" -p "$FB_PG_PORT" -c listen_addresses='' \
  -c fsync=off -c synchronous_commit=off -c max_connections=100 -c shared_buffers=16MB \
  -c hot_standby=on{extra} >"$FB_PG_NODE/postgres.log" 2>&1 &
PGPID=$!
while kill -0 "$FB_PG_PARENT" 2>/dev/null && kill -0 "$PGPID" 2>/dev/null; do sleep 1; done
kill -QUIT "$PGPID" 2>/dev/null
wait "$PGPID" 2>/dev/null
"#
        );
        Command::new("/bin/sh")
            .arg("-c")
            .arg(script)
            .env("FB_PG_BIN", &self.bin)
            .env("FB_PG_NODE", node)
            .env("FB_PG_PORT", PORT.to_string())
            .env("FB_PG_PARENT", std::process::id().to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("start postgres: {e}"))?;
        self.nodes.push(node.to_path_buf());
        Ok(())
    }

    /// Superuser URL of the primary's database [`DATABASE`].
    pub fn primary_url(&self) -> String {
        url(&self.root.join("p"), DATABASE)
    }

    /// Superuser URL of the standby's database [`DATABASE`].
    pub fn standby_url(&self) -> String {
        url(&self.root.join("s"), DATABASE)
    }

    /// The primary, read-write.
    pub async fn open_primary(&self) -> Result<Database, DbError> {
        Database::open(options(self.primary_url())).await
    }

    /// The standby, read-only (as a set connector opens replicas).
    pub async fn open_standby(&self) -> Result<Database, DbError> {
        Database::open(options(self.standby_url()).read_only()).await
    }

    /// Runs `sql` on the primary as the superuser.
    pub async fn primary_sql(&self, sql: &str) -> Result<(), String> {
        run(&self.primary_url(), sql).await
    }

    /// Runs `sql` on the standby as the superuser.
    pub async fn standby_sql(&self, sql: &str) -> Result<(), String> {
        run(&self.standby_url(), sql).await
    }

    /// Pauses WAL replay on the standby (it keeps receiving).
    pub async fn pause_replay(&self) -> Result<(), String> {
        let mut conn = connect(&self.standby_url()).await?;
        sqlx::raw_sql("SELECT pg_wal_replay_pause()")
            .execute(&mut conn)
            .await
            .map_err(|e| e.to_string())?;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let state: String = sqlx::query_scalar("SELECT pg_get_wal_replay_pause_state()")
                .fetch_one(&mut conn)
                .await
                .map_err(|e| e.to_string())?;
            if state == "paused" {
                return Ok(());
            }
            if Instant::now() > deadline {
                return Err(format!("replay did not pause ({state})"));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Resumes WAL replay on the standby.
    pub async fn resume_replay(&self) -> Result<(), String> {
        self.standby_sql("SELECT pg_wal_replay_resume()").await
    }

    /// The primary's current WAL insert position.
    pub async fn primary_lsn(&self) -> Result<Lsn, String> {
        let mut conn = connect(&self.primary_url()).await?;
        let text: String = sqlx::query_scalar("SELECT pg_current_wal_insert_lsn()::text")
            .fetch_one(&mut conn)
            .await
            .map_err(|e| e.to_string())?;
        text.parse()
    }

    /// Waits until the standby has replayed everything the primary wrote.
    pub async fn wait_caught_up(&self) -> Result<(), String> {
        let target = self.primary_lsn().await?;
        let mut conn = connect(&self.standby_url()).await?;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let row = sqlx::query("SELECT COALESCE(pg_last_wal_replay_lsn()::text, '0/0')")
                .fetch_one(&mut conn)
                .await
                .map_err(|e| e.to_string())?;
            let replay: Lsn = row.try_get::<String, _>(0).map_err(|e| e.to_string())?.parse()?;
            if replay >= target {
                return Ok(());
            }
            if Instant::now() > deadline {
                return Err(format!("standby stuck at {replay}, primary at {target}"));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Starts an unrelated server that is in recovery (its own `initdb`
    /// plus `standby.signal`, no primary): a replica of another cluster.
    /// Returns its superuser URL (database `postgres`).
    pub async fn start_unrelated_standby(&mut self) -> Result<String, String> {
        let node = self.root.join("u");
        self.initdb(&node)?;
        std::fs::write(node.join("data").join("standby.signal"), b"").map_err(|e| e.to_string())?;
        self.spawn(&node, &[])?;
        let u = url(&node, "postgres");
        wait_ready(&u).await?;
        Ok(u)
    }
}

impl Drop for ReplicaCluster {
    fn drop(&mut self) {
        for node in self.nodes.iter().rev() {
            let _ = Command::new(self.bin.join("pg_ctl"))
                .args(["stop", "-m", "immediate", "-w", "-D"])
                .arg(node.join("data"))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A migrated primary, one streaming standby and a control [`SetHandle`]
/// routing over it (FB-169). The services under test run on
/// [`RoutedCluster::backend`]; [`RoutedCluster::lag`] pauses replay so that
/// a routed read shows what the standby had before, and
/// [`RoutedCluster::replica_reads`] counts the reads it served.
#[derive(Debug)]
pub struct RoutedCluster {
    /// The cluster (primary and standby).
    pub cluster: ReplicaCluster,
    /// The routing over the standby.
    pub replicas: ReplicaSet,
    /// The control set's handle: its reads route, its writes go to the primary.
    pub handle: SetHandle,
}

impl RoutedCluster {
    /// Starts the cluster. `None` when no engine is configured.
    pub async fn start() -> Option<Self> {
        let cluster = match ReplicaCluster::start_migrated(&[]).await? {
            Ok(c) => c,
            Err(e) => panic!("replica cluster: {e}"),
        };
        Some(Self::over(cluster).await)
    }

    async fn over(cluster: ReplicaCluster) -> Self {
        let primary = cluster.open_primary().await.expect("open the primary");
        cluster.wait_caught_up().await.expect("standby catches up");
        let standby = cluster.open_standby().await.expect("open the standby");
        let node = DbNode {
            id: "n-r1".into(),
            set_id: "control".into(),
            name: "r1".into(),
            kind: NodeKind::External,
            role: NodeRole::Replica,
            url_sealed: None,
            state: NodeState::Ready,
            weight: 1,
            engine_version: None,
            system_identifier: None,
            timeline: None,
            origin: NodeOrigin::Api,
            created_at: String::new(),
            updated_at: String::new(),
        };
        let replicas = ReplicaSet::open(
            "control",
            primary.clone(),
            vec![ReplicaNode { node, db: standby }],
            ReplicaOptions {
                health_interval: Duration::ZERO,
                lsn_wait: Duration::from_millis(20),
                ..ReplicaOptions::default()
            },
            None,
        )
        .await;
        let handle = SetHandle::new(DbSet::control(), primary).with_replicas(replicas.clone());
        Self {
            cluster,
            replicas,
            handle,
        }
    }

    /// The handle as the backend services take.
    pub fn backend(&self) -> Arc<dyn Backend> {
        Arc::new(self.handle.clone())
    }

    /// Reads the standby has served so far.
    pub fn replica_reads(&self) -> u64 {
        self.replicas.status().iter().map(|s| s.reads).sum()
    }

    /// Reads that failed on the standby and were retried on the primary.
    pub fn fallbacks(&self) -> u64 {
        self.replicas.status().iter().map(|s| s.fallbacks).sum()
    }

    /// Lets the standby catch up (DDL a service ran at open must reach it
    /// before the first routed read), then samples its health.
    pub async fn sync(&self) {
        self.cluster.wait_caught_up().await.expect("standby catches up");
        self.replicas.refresh().await;
    }

    /// Pauses replay: the standby keeps what it has.
    pub async fn lag(&self) {
        self.cluster.pause_replay().await.expect("pause replay");
    }

    /// Resumes replay and waits for it to finish.
    pub async fn catch_up(&self) {
        self.cluster.resume_replay().await.expect("resume replay");
        self.sync().await;
    }
}

/// Opens `url` read-only (a replica, as a set connector would).
pub async fn open_read_only(url: String) -> Result<Database, DbError> {
    Database::open(options(url).read_only()).await
}

async fn connect(url: &str) -> Result<sqlx::PgConnection, String> {
    sqlx::PgConnection::connect(url).await.map_err(|e| e.to_string())
}

async fn run(url: &str, sql: &str) -> Result<(), String> {
    let mut conn = connect(url).await?;
    sqlx::raw_sql(AssertSqlSafe(sql.to_owned()))
        .execute(&mut conn)
        .await
        .map_err(|e| e.to_string())?;
    let _ = conn.close().await;
    Ok(())
}

async fn wait_ready(url: &str) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match sqlx::PgConnection::connect(url).await {
            Ok(conn) => {
                let _ = conn.close().await;
                return Ok(());
            }
            Err(e) if Instant::now() > deadline => return Err(format!("server did not become ready: {e}")),
            Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    }
}
