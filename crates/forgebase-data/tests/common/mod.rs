//! Helpers for the data-core integration tests: a migrated database on the
//! backend `FORGEBASE_TEST_BACKEND` selects (`sqlite`, the default, or
//! `postgres`; ADR-0028 §7.7), behind the backend trait.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use forgebase_core::Synchronous;
use forgebase_data::db::testing::{TestDb, test_backend};
use forgebase_data::db::{Backend, BackendKind, Database, DbOptions};
use forgebase_data::{AccessContext, Authorizer, DataService, DefaultAuthorizer, SecretHasher};

pub fn admin() -> AccessContext {
    AccessContext::admin("root")
}

/// A fresh, migrated test database (dropped with the [`TestDb`]).
pub async fn test_db() -> TestDb {
    test_backend().await
}

/// Opens the test's database again (a restart).
pub async fn reopen(db: &TestDb) -> Database {
    let reopened = Database::open(db.options()).await.unwrap();
    reopened.migrate(Some(&db.data_dir().join("backups"))).await.unwrap();
    reopened
}

/// Whether the test runs on PostgreSQL.
pub fn is_postgres(db: &Database) -> bool {
    db.kind() == BackendKind::Postgres
}

/// Skips the rest of a test on PostgreSQL until the package named in
/// `why` ports what it needs (prints why). `true`: return now.
pub fn skip_on_postgres(db: &Database, why: &str) -> bool {
    if is_postgres(db) {
        eprintln!("skipped on PostgreSQL: {why}");
        return true;
    }
    false
}

/// A SQLite database file in `dir` (reopening `dir` reopens it).
/// SQLite only: kept for `schema_changes.rs` until FB-144 moves it to
/// [`test_db`].
pub async fn open_db(dir: &Path) -> Database {
    let opts = DbOptions {
        target: forgebase_data::db::DbTarget::sqlite(dir.join("forgebase.db")),
        synchronous: Synchronous::Full,
        busy_timeout: Duration::from_millis(5000),
        read_pool_size: 8,
        create_if_missing: true,
        read_only: false,
    };
    let db = Database::open(opts).await.unwrap();
    db.migrate(Some(&dir.join("backups"))).await.unwrap();
    db
}

pub async fn service(db: &Database) -> DataService {
    service_with(db, Arc::new(DefaultAuthorizer), None).await
}

pub async fn service_with(
    db: &Database,
    authorizer: Arc<dyn Authorizer>,
    hasher: Option<Arc<dyn SecretHasher>>,
) -> DataService {
    let backend: Arc<dyn Backend> = Arc::new(db.clone());
    DataService::with_hasher(backend, authorizer, hasher).await.unwrap()
}

/// Test-only hasher (FB-20 provides Argon2id).
#[derive(Debug)]
pub struct TestHasher;

impl SecretHasher for TestHasher {
    fn hash(&self, secret: &str) -> Result<String, String> {
        Ok(format!("test${}", secret.chars().rev().collect::<String>()))
    }
}
