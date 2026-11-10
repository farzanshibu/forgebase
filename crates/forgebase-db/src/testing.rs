//! Test databases on both backends (ADR-0028 §7.7). Feature `testing`, for
//! dev-dependencies only.
//!
//! [`test_backend`] reads `FORGEBASE_TEST_BACKEND` (`sqlite`, the default, or
//! `postgres`). For PostgreSQL it starts **one** engine per test binary, from
//! `FORGEBASE_TEST_PG_ENGINE` (an extracted engine directory, e.g. FB-130's
//! archive) or `FORGEBASE_PG_ENGINE_DIR` (any PostgreSQL 16+ install such as
//! Homebrew's): `initdb` into a temporary directory, a Unix socket only, no
//! TCP port. A watchdog shell stops the engine and deletes its directory once
//! the test binary exits. Each test gets `CREATE DATABASE t_<uuid> TEMPLATE
//! fb_template` (the template is migrated once), dropped when the
//! [`TestDb`] is dropped.
//!
//! `FORGEBASE_TEST_PG_URL` (an administrator URL of a running server) is used
//! instead of starting an engine when set; each test database is then
//! created empty and migrated on its own.

use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sqlx::{AssertSqlSafe, Connection};
use tokio::sync::OnceCell;

use crate::dialect::{Dialect, PgDialect};
use crate::{BackendKind, Database, DbOptions, DbTarget, SecretString};

/// Selects the backend of [`test_backend`]: `sqlite` (default) or `postgres`.
pub const BACKEND_ENV: &str = "FORGEBASE_TEST_BACKEND";
/// Extracted PostgreSQL engine directory (contains `bin/postgres`).
pub const ENGINE_ENV: &str = "FORGEBASE_TEST_PG_ENGINE";
/// Fallback engine directory (the local engine of the build machine).
pub const ENGINE_DIR_ENV: &str = "FORGEBASE_PG_ENGINE_DIR";
/// Administrator URL of an existing server, used instead of an engine.
pub const URL_ENV: &str = "FORGEBASE_TEST_PG_URL";
/// Port number of the test engine's Unix socket (no TCP listener).
pub const SOCKET_PORT: u16 = 18_200;

/// The backend `FORGEBASE_TEST_BACKEND` asks for.
pub fn selected_backend() -> BackendKind {
    match std::env::var(BACKEND_ENV).ok().as_deref().map(str::trim) {
        Some("postgres" | "postgresql" | "pg") => BackendKind::Postgres,
        _ => BackendKind::Sqlite,
    }
}

/// Whether a PostgreSQL engine or server is configured for tests.
pub fn postgres_available() -> bool {
    [URL_ENV, ENGINE_ENV, ENGINE_DIR_ENV]
        .iter()
        .any(|k| std::env::var_os(k).is_some_and(|v| !v.is_empty()))
}

/// A database for one test. Derefs to [`Database`].
#[derive(Debug)]
pub struct TestDb {
    db: Database,
    dir: tempfile::TempDir,
    pg: Option<PgDrop>,
}

#[derive(Debug)]
struct PgDrop {
    admin_url: String,
    name: String,
}

impl TestDb {
    /// The database.
    pub fn database(&self) -> &Database {
        &self.db
    }

    /// A private temporary directory for the test (the SQLite file lives here).
    pub fn data_dir(&self) -> &Path {
        self.dir.path()
    }

    /// The engine.
    pub fn kind(&self) -> BackendKind {
        self.db.kind()
    }

    /// Options that reopen this test's database.
    pub fn options(&self) -> DbOptions {
        let target = match &self.pg {
            None => DbTarget::sqlite(self.dir.path().join("forgebase.db")),
            Some(pg) => DbTarget::postgres(database_url(&pg.admin_url, &pg.name), false),
        };
        default_options(target)
    }
}

impl Deref for TestDb {
    type Target = Database;

    fn deref(&self) -> &Database {
        &self.db
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        let Some(pg) = self.pg.take() else {
            return;
        };
        // Async work cannot run in Drop: drop the database from a helper
        // thread with its own runtime. FORCE ends this test's open sessions.
        let handle = std::thread::spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
                return;
            };
            rt.block_on(async move {
                if let Ok(mut admin) = sqlx::PgConnection::connect(&pg.admin_url).await {
                    let sql = format!(
                        "DROP DATABASE IF EXISTS {} WITH (FORCE)",
                        PgDialect.quote_ident(&pg.name)
                    );
                    let _ = sqlx::raw_sql(AssertSqlSafe(sql)).execute(&mut admin).await;
                    let _ = admin.close().await;
                }
            });
        });
        let _ = handle.join();
    }
}

fn default_options(target: DbTarget) -> DbOptions {
    DbOptions {
        target,
        synchronous: forgebase_core::Synchronous::Full,
        busy_timeout: Duration::from_millis(5000),
        read_pool_size: 4,
        create_if_missing: true,
        read_only: false,
    }
}

/// A migrated database on the backend `FORGEBASE_TEST_BACKEND` selects.
///
/// # Panics
/// When the database cannot be set up (a broken test environment), or when
/// PostgreSQL is selected but no engine is configured.
pub async fn test_backend() -> TestDb {
    test_backend_of(selected_backend()).await
}

/// A migrated database on `kind`. Panics like [`test_backend`].
pub async fn test_backend_of(kind: BackendKind) -> TestDb {
    match kind {
        BackendKind::Sqlite => sqlite_test_db().await,
        BackendKind::Postgres => match postgres_test_db().await {
            Some(db) => db,
            None => {
                panic!("{BACKEND_ENV}=postgres needs {ENGINE_ENV}, {ENGINE_DIR_ENV} (an engine directory) or {URL_ENV}")
            }
        },
    }
}

async fn sqlite_test_db() -> TestDb {
    let dir = tempfile::tempdir().expect("temporary directory");
    let db = Database::open(default_options(DbTarget::sqlite(dir.path().join("forgebase.db"))))
        .await
        .expect("open the SQLite test database");
    db.migrate(None).await.expect("migrate the SQLite test database");
    TestDb { db, dir, pg: None }
}

/// A fresh PostgreSQL test database at the newest system migration, or
/// `None` when no engine or server is configured (the caller skips).
///
/// # Panics
/// When an engine is configured but cannot be started.
pub async fn postgres_test_db() -> Option<TestDb> {
    if !postgres_available() {
        return None;
    }
    let server = server()
        .await
        .as_ref()
        .unwrap_or_else(|e| panic!("PostgreSQL test engine: {e}"));
    let name = format!("t_{}", uuid::Uuid::new_v4().simple());
    let mut admin = sqlx::PgConnection::connect(&server.admin_url)
        .await
        .unwrap_or_else(|e| panic!("connect to the PostgreSQL test engine: {e}"));
    let create = match &server.template {
        // FILE_COPY: the migrated template has ~450 relations; copying files
        // is several times faster than WAL-logging every block (PG 15+).
        Some(t) => format!(
            "CREATE DATABASE {} TEMPLATE {} STRATEGY FILE_COPY",
            PgDialect.quote_ident(&name),
            PgDialect.quote_ident(t)
        ),
        None => format!("CREATE DATABASE {}", PgDialect.quote_ident(&name)),
    };
    sqlx::raw_sql(AssertSqlSafe(create))
        .execute(&mut admin)
        .await
        .unwrap_or_else(|e| panic!("create test database {name}: {e}"));
    let _ = admin.close().await;
    let dir = tempfile::tempdir().expect("temporary directory");
    let db = Database::open(default_options(DbTarget::postgres(
        database_url(&server.admin_url, &name),
        false,
    )))
    .await
    .unwrap_or_else(|e| panic!("open test database {name}: {e}"));
    if server.template.is_none() {
        migrate_or_skip(&db).await;
    }
    Some(TestDb {
        db,
        dir,
        pg: Some(PgDrop {
            admin_url: server.admin_url.clone(),
            name,
        }),
    })
}

async fn migrate_or_skip(db: &Database) {
    if let Err(e) = db.migrate(None).await {
        panic!("migrate the PostgreSQL test database: {e}");
    }
}

/// `admin_url` with its database replaced by `name`.
fn database_url(admin_url: &str, name: &str) -> SecretString {
    let (base, query) = admin_url
        .split_once('?')
        .map_or((admin_url, None), |(b, q)| (b, Some(q)));
    // postgres://user[:pw]@host[:port]/db : replace the path.
    let scheme_end = base.find("://").map_or(0, |p| p + 3);
    let path_start = base[scheme_end..].find('/').map_or(base.len(), |p| p + scheme_end);
    let mut url = format!("{}/{name}", &base[..path_start]);
    if let Some(q) = query {
        url.push('?');
        url.push_str(q);
    }
    SecretString::new(url)
}

#[derive(Debug)]
struct Server {
    admin_url: String,
    /// Migrated template database, when this binary owns the engine.
    template: Option<String>,
}

static SERVER: OnceCell<Result<Server, String>> = OnceCell::const_new();

async fn server() -> &'static Result<Server, String> {
    SERVER.get_or_init(start_server).await
}

async fn start_server() -> Result<Server, String> {
    if let Some(url) = std::env::var(URL_ENV).ok().filter(|u| !u.is_empty()) {
        return Ok(Server {
            admin_url: url,
            template: None,
        });
    }
    let engine = [ENGINE_ENV, ENGINE_DIR_ENV]
        .iter()
        .find_map(|k| std::env::var_os(k).filter(|v| !v.is_empty()))
        .map(PathBuf::from)
        .ok_or("no engine directory configured")?;
    let admin_url = start_engine(&engine)?;
    wait_ready(&admin_url).await?;
    // The template: migrated once, then copied for every test.
    let template = "fb_template".to_owned();
    let mut admin = sqlx::PgConnection::connect(&admin_url)
        .await
        .map_err(|e| e.to_string())?;
    sqlx::raw_sql(AssertSqlSafe(format!("CREATE DATABASE {template}")))
        .execute(&mut admin)
        .await
        .map_err(|e| format!("create the template database: {e}"))?;
    // The role carries the search_path, as the bundled engine's roles do
    // (ADR-0028 §7.6): copies of the template do not inherit per-database
    // settings, role settings apply everywhere.
    sqlx::raw_sql(AssertSqlSafe(format!(
        "ALTER ROLE CURRENT_USER SET search_path = {}",
        forgebase_migrations::postgres::SEARCH_PATH
    )))
    .execute(&mut admin)
    .await
    .map_err(|e| format!("set the test role's search_path: {e}"))?;
    let _ = admin.close().await;
    let db = Database::open(default_options(DbTarget::postgres(
        database_url(&admin_url, &template),
        true,
    )))
    .await
    .map_err(|e| format!("open the template database: {e}"))?;
    migrate_or_skip(&db).await;
    db.close().await;
    Ok(Server {
        admin_url,
        template: Some(template),
    })
}

/// `initdb` and start the engine under a temporary directory; returns the
/// administrator URL (Unix socket).
#[cfg(unix)]
fn start_engine(engine: &Path) -> Result<String, String> {
    use std::process::{Command, Stdio};

    let bin = engine.join("bin");
    let base = std::env::var_os("FORGEBASE_TEST_PG_TMP")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let root = base.join(format!(
        "fbpg-{}-{}",
        std::process::id(),
        &uuid::Uuid::new_v4().simple().to_string()[..6]
    ));
    let data = root.join("data");
    std::fs::create_dir_all(&root).map_err(|e| format!("create {}: {e}", root.display()))?;
    let out = Command::new(bin.join("initdb"))
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
        .arg(&data)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("run {}: {e}", bin.join("initdb").display()))?;
    if !out.status.success() {
        return Err(format!("initdb failed: {}", String::from_utf8_lossy(&out.stderr)));
    }
    // The watchdog lives as long as this test binary: when it exits, the
    // engine gets a fast shutdown and its directory is removed.
    let script = r#"
"$FB_PG_BIN/postgres" -D "$FB_PG_DATA" -k "$FB_PG_ROOT" -p "$FB_PG_PORT" -c listen_addresses='' \
  -c fsync=off -c synchronous_commit=off -c full_page_writes=off -c max_connections=300 \
  -c shared_buffers=32MB >"$FB_PG_ROOT/postgres.log" 2>&1 &
PGPID=$!
while kill -0 "$FB_PG_PARENT" 2>/dev/null && kill -0 "$PGPID" 2>/dev/null; do sleep 1; done
kill -INT "$PGPID" 2>/dev/null
wait "$PGPID" 2>/dev/null
rm -rf "$FB_PG_ROOT"
"#;
    Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .env("FB_PG_BIN", &bin)
        .env("FB_PG_DATA", &data)
        .env("FB_PG_ROOT", &root)
        .env("FB_PG_PORT", SOCKET_PORT.to_string())
        .env("FB_PG_PARENT", std::process::id().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("start postgres: {e}"))?;
    Ok(format!(
        "postgres://postgres@localhost/postgres?host={}&port={SOCKET_PORT}",
        root.display()
    ))
}

#[cfg(not(unix))]
fn start_engine(_engine: &Path) -> Result<String, String> {
    Err(format!(
        "starting a test engine needs Unix sockets; set {URL_ENV} on this platform"
    ))
}

async fn wait_ready(url: &str) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match sqlx::PgConnection::connect(url).await {
            Ok(conn) => {
                let _ = conn.close().await;
                return Ok(());
            }
            Err(e) if Instant::now() > deadline => return Err(format!("engine did not become ready: {e}")),
            Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn database_urls() {
        assert_eq!(
            database_url("postgres://postgres@localhost/postgres?host=/tmp/x&port=18200", "t_1").expose(),
            "postgres://postgres@localhost/t_1?host=/tmp/x&port=18200"
        );
        assert_eq!(
            database_url("postgres://u:p@h:5432/admin", "t_2").expose(),
            "postgres://u:p@h:5432/t_2"
        );
        assert_eq!(database_url("postgres://u@h", "t_3").expose(), "postgres://u@h/t_3");
    }
}
