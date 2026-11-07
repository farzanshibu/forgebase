//! Helpers for the data-core integration tests: a real SQLite file in a
//! temporary directory, migrated, behind the backend trait.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use forgebase_core::Synchronous;
use forgebase_data::db::{Backend, Database, DbOptions};
use forgebase_data::{AccessContext, Authorizer, DataService, DefaultAuthorizer, SecretHasher};

pub fn admin() -> AccessContext {
    AccessContext::admin("root")
}

pub async fn open_db(dir: &Path) -> Database {
    let opts = DbOptions {
        path: dir.join("forgebase.db"),
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
