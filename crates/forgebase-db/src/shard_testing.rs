//! Several PostgreSQL clusters for shard tests (FB-173). Feature `testing`,
//! for dev-dependencies only.
//!
//! [`TestCluster::start`] runs `initdb` and `pg_ctl start` from the engine
//! of `FORGEBASE_TEST_PG_ENGINE` or `FORGEBASE_PG_ENGINE_DIR` (the same
//! variables as [`crate::testing`]) in a temporary directory: a Unix socket
//! in that directory only (no TCP), `max_prepared_transactions = 16` and
//! `wal_level = logical`, so two-phase commit and logical replication work
//! between clusters. [`Drop`] stops the cluster and removes the directory;
//! a watchdog shell does the same if the test binary dies first.
//! [`TestCluster::crash`] is `pg_ctl stop -m immediate` (a crash: prepared
//! transactions survive it), [`TestCluster::restart`] starts it again.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use sqlx::{AssertSqlSafe, Connection};

use crate::dialect::{Dialect, PgDialect};
use crate::testing::{ENGINE_DIR_ENV, ENGINE_ENV};
use crate::{Database, DbOptions, DbTarget, SecretString};

/// Port number of every test cluster's socket (each has its own directory).
pub const PORT: u16 = 5432;

/// The engine directory configured for tests, if any.
pub fn engine_dir() -> Option<PathBuf> {
    [ENGINE_ENV, ENGINE_DIR_ENV]
        .iter()
        .find_map(|k| std::env::var_os(k).filter(|v| !v.is_empty()))
        .map(PathBuf::from)
}

/// One local PostgreSQL cluster in a temporary directory.
#[derive(Debug)]
pub struct TestCluster {
    name: String,
    bin: PathBuf,
    root: tempfile::TempDir,
}

impl TestCluster {
    /// Starts a cluster named `name` (for messages) with the default test
    /// settings plus `extra` (`name = value` lines of `postgresql.conf`).
    /// `None` when no engine is configured (the test skips).
    ///
    /// # Panics
    /// When an engine is configured but the cluster cannot start.
    pub async fn start(name: &str, extra: &[(&str, &str)]) -> Option<Self> {
        let engine = engine_dir()?;
        let bin = engine.join("bin");
        let base = std::env::var_os("FORGEBASE_TEST_PG_TMP")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        std::fs::create_dir_all(&base).unwrap_or_else(|e| panic!("create {}: {e}", base.display()));
        let root = tempfile::Builder::new()
            .prefix("fbc-")
            .tempdir_in(&base)
            .unwrap_or_else(|e| panic!("temporary directory under {}: {e}", base.display()));
        let cluster = Self {
            name: name.to_owned(),
            bin,
            root,
        };
        cluster.init(extra);
        cluster.pg_ctl_start();
        cluster.watchdog();
        cluster.wait_ready().await;
        Some(cluster)
    }

    fn data(&self) -> PathBuf {
        self.root.path().join("data")
    }

    /// The socket directory.
    pub fn socket_dir(&self) -> &Path {
        self.root.path()
    }

    /// Name given at start.
    pub fn name(&self) -> &str {
        &self.name
    }

    fn init(&self, extra: &[(&str, &str)]) {
        let out = Command::new(self.bin.join("initdb"))
            .args([
                "-U",
                "postgres",
                "-A",
                "trust",
                "-E",
                "UTF8",
                "--locale=C",
                "--no-sync",
                "--no-instructions",
                "-D",
            ])
            .arg(self.data())
            .stdin(Stdio::null())
            .output()
            .unwrap_or_else(|e| panic!("run initdb for {}: {e}", self.name));
        assert!(
            out.status.success(),
            "initdb for {} failed: {}",
            self.name,
            String::from_utf8_lossy(&out.stderr)
        );
        let mut conf = format!(
            "\n# FB-173 test cluster\nlisten_addresses = ''\nunix_socket_directories = '{}'\nport = {PORT}\n\
             fsync = off\nsynchronous_commit = off\nfull_page_writes = off\nshared_buffers = 32MB\n\
             max_connections = 120\nmax_prepared_transactions = 16\nwal_level = logical\n\
             max_wal_senders = 16\nmax_replication_slots = 16\nmax_logical_replication_workers = 8\n\
             wal_sender_timeout = 0\n",
            self.root.path().display()
        );
        for (k, v) in extra {
            conf.push_str(&format!("{k} = {v}\n"));
        }
        let path = self.data().join("postgresql.conf");
        let mut text = std::fs::read_to_string(&path).unwrap_or_default();
        text.push_str(&conf);
        std::fs::write(&path, text).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    }

    fn pg_ctl(&self, args: &[&str]) -> std::process::Output {
        Command::new(self.bin.join("pg_ctl"))
            .args(args)
            .arg("-D")
            .arg(self.data())
            .stdin(Stdio::null())
            .output()
            .unwrap_or_else(|e| panic!("run pg_ctl for {}: {e}", self.name))
    }

    fn pg_ctl_start(&self) {
        let log = self.root.path().join("postgres.log");
        let log = log.to_string_lossy().into_owned();
        let out = self.pg_ctl(&["start", "-w", "-t", "60", "-l", &log]);
        assert!(
            out.status.success(),
            "start {}: {}\n{}",
            self.name,
            String::from_utf8_lossy(&out.stderr),
            std::fs::read_to_string(&log).unwrap_or_default()
        );
    }

    /// Stops the cluster when this test binary exits without dropping it.
    fn watchdog(&self) {
        let script = r#"
while kill -0 "$FB_PARENT" 2>/dev/null; do sleep 1; done
"$FB_BIN/pg_ctl" stop -m immediate -D "$FB_DATA" >/dev/null 2>&1
rm -rf "$FB_ROOT"
"#;
        let _ = Command::new("/bin/sh")
            .arg("-c")
            .arg(script)
            .env("FB_BIN", &self.bin)
            .env("FB_DATA", self.data())
            .env("FB_ROOT", self.root.path())
            .env("FB_PARENT", std::process::id().to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }

    async fn wait_ready(&self) {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            match sqlx::PgConnection::connect(&self.admin_url()).await {
                Ok(c) => {
                    let _ = c.close().await;
                    return;
                }
                Err(e) if Instant::now() > deadline => panic!("{} did not become ready: {e}", self.name),
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
    }

    /// Superuser URL of database `postgres`.
    pub fn admin_url(&self) -> String {
        self.url_of("postgres")
    }

    /// Superuser URL of database `db`.
    pub fn url_of(&self, db: &str) -> String {
        format!(
            "postgres://postgres@localhost/{db}?host={}&port={PORT}",
            self.root.path().display()
        )
    }

    /// libpq connection string of database `db` (for `CREATE SUBSCRIPTION`).
    pub fn conninfo(&self, db: &str) -> String {
        format!(
            "host={} port={PORT} user=postgres dbname={db}",
            self.root.path().display()
        )
    }

    /// Creates database `db` (if missing), opens it and migrates it to head.
    ///
    /// # Panics
    /// When the database cannot be created, opened or migrated.
    pub async fn database(&self, db: &str) -> Database {
        let mut admin = sqlx::PgConnection::connect(&self.admin_url())
            .await
            .unwrap_or_else(|e| panic!("connect to {}: {e}", self.name));
        let exists: Option<(i32,)> = sqlx::query_as("SELECT 1 FROM pg_database WHERE datname = $1")
            .bind(db)
            .fetch_optional(&mut admin)
            .await
            .unwrap_or_else(|e| panic!("list databases of {}: {e}", self.name));
        if exists.is_none() {
            sqlx::raw_sql(AssertSqlSafe(format!("CREATE DATABASE {}", PgDialect.quote_ident(db))))
                .execute(&mut admin)
                .await
                .unwrap_or_else(|e| panic!("create database {db} on {}: {e}", self.name));
        }
        let _ = admin.close().await;
        let database = self.open(db).await;
        database
            .migrate(None)
            .await
            .unwrap_or_else(|e| panic!("migrate {db} on {}: {e}", self.name));
        database
    }

    /// Opens database `db` without migrating.
    ///
    /// # Panics
    /// When it cannot be opened.
    pub async fn open(&self, db: &str) -> Database {
        Database::open(self.options(db))
            .await
            .unwrap_or_else(|e| panic!("open {db} on {}: {e}", self.name))
    }

    /// Options that open database `db`.
    pub fn options(&self, db: &str) -> DbOptions {
        DbOptions {
            target: DbTarget::postgres(SecretString::new(self.url_of(db)), false),
            synchronous: forgebase_core::Synchronous::Full,
            busy_timeout: Duration::from_millis(5000),
            read_pool_size: 2,
            create_if_missing: true,
            read_only: false,
        }
    }

    /// Runs one statement as the superuser in database `db`.
    ///
    /// # Panics
    /// When it fails.
    pub async fn exec(&self, db: &str, sql: &str) {
        let mut c = sqlx::PgConnection::connect(&self.url_of(db))
            .await
            .unwrap_or_else(|e| panic!("connect to {db} on {}: {e}", self.name));
        sqlx::raw_sql(AssertSqlSafe(sql.to_owned()))
            .execute(&mut c)
            .await
            .unwrap_or_else(|e| panic!("{sql} on {}: {e}", self.name));
        let _ = c.close().await;
    }

    /// A crash: `pg_ctl stop -m immediate` (no shutdown checkpoint).
    pub fn crash(&self) {
        let out = self.pg_ctl(&["stop", "-m", "immediate", "-w"]);
        assert!(
            out.status.success(),
            "crash {}: {}",
            self.name,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Starts the cluster again after [`TestCluster::crash`].
    pub async fn restart(&self) {
        self.pg_ctl_start();
        self.wait_ready().await;
    }
}

impl Drop for TestCluster {
    fn drop(&mut self) {
        let _ = Command::new(self.bin.join("pg_ctl"))
            .args(["stop", "-m", "fast", "-w", "-D"])
            .arg(self.data())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// Opens shards from a map of node name to database.
#[derive(Debug, Default)]
pub struct MapConnector(std::sync::Mutex<std::collections::HashMap<String, Database>>);

impl MapConnector {
    /// Node `name` is `db`.
    pub fn insert(&self, name: &str, db: Database) {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(name.to_owned(), db);
    }
}

impl crate::router::SetConnector for MapConnector {
    fn connect<'a>(
        &'a self,
        _set: &'a crate::topology::DbSet,
        primary: &'a crate::topology::DbNode,
    ) -> crate::BoxFuture<'a, Result<Database, crate::DbError>> {
        Box::pin(async move {
            self.0
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get(&primary.name)
                .cloned()
                .ok_or_else(|| crate::DbError::Unavailable(format!("no database for node {}", primary.name)))
        })
    }
}

/// A control cluster with a router, and shard clusters added one by one.
#[derive(Debug)]
pub struct Sharded {
    /// Control's cluster first, then one per shard.
    pub clusters: Vec<TestCluster>,
    /// Control's database (migrated).
    pub control: Database,
    /// The router over control and every active shard.
    pub router: crate::DbRouter,
    /// Its connector (node `<set name>-primary` → the shard's database).
    pub connector: std::sync::Arc<MapConnector>,
}

impl Sharded {
    /// Starts control (database `forgebase`, migrated) and opens a router.
    /// `None` without an engine.
    ///
    /// # Panics
    /// When the cluster or router cannot be set up.
    pub async fn start() -> Option<Self> {
        let cluster = TestCluster::start("control", &[]).await?;
        let control = cluster.database("forgebase").await;
        let connector = std::sync::Arc::new(MapConnector::default());
        let router = crate::DbRouter::open(
            control.clone(),
            crate::RouterOptions {
                connector: Some(connector.clone()),
                ..crate::RouterOptions::default()
            },
        )
        .await
        .unwrap_or_else(|e| panic!("open the router: {e}"));
        Some(Self {
            clusters: vec![cluster],
            control,
            router,
            connector,
        })
    }

    /// Starts a cluster for a new shard `name` with an empty, unmigrated
    /// database, and records the set (`provisioning`) and its primary
    /// (`ready`) on control. Activate it with
    /// [`crate::shards::activate_shard`].
    ///
    /// # Panics
    /// When the cluster cannot start or the rows cannot be written.
    pub async fn provision(&mut self, name: &str) -> (crate::topology::DbSet, Database) {
        self.provision_with(name, &[]).await
    }

    /// [`Sharded::provision`] with extra `postgresql.conf` settings.
    ///
    /// # Panics
    /// Like [`Sharded::provision`].
    pub async fn provision_with(&mut self, name: &str, extra: &[(&str, &str)]) -> (crate::topology::DbSet, Database) {
        use crate::topology::{self, NewNode, NewSet, NodeKind, NodeOrigin, NodeRole, NodeState};
        let cluster = TestCluster::start(name, extra)
            .await
            .unwrap_or_else(|| panic!("no engine for shard {name}"));
        cluster.exec("postgres", "CREATE DATABASE forgebase").await;
        let db = cluster.open("forgebase").await;
        let set = topology::insert_set(
            &self.control,
            &NewSet {
                name: name.into(),
                weight: 1,
            },
        )
        .await
        .unwrap_or_else(|e| panic!("insert set {name}: {e}"));
        let node = format!("{name}-primary");
        topology::insert_node(
            &self.control,
            &NewNode {
                set_id: set.id.clone(),
                name: node.clone(),
                kind: NodeKind::External,
                role: NodeRole::Primary,
                url_sealed: Some("sealed".into()),
                state: NodeState::Ready,
                weight: 1,
                origin: NodeOrigin::Api,
            },
        )
        .await
        .unwrap_or_else(|e| panic!("insert node {node}: {e}"));
        self.connector.insert(&node, db.clone());
        self.clusters.push(cluster);
        (set, db)
    }

    /// How a shard reaches control (`CREATE SUBSCRIPTION … CONNECTION`).
    pub fn control_conninfo(&self) -> SecretString {
        SecretString::new(self.clusters[0].conninfo("forgebase"))
    }
}
