//! Shared rig for the ForgeBase benchmarks (FB-81).
//!
//! Everything here is dev tooling: it opens real SQLite databases in a temp
//! directory with the production pragmas and migrations, so the numbers are
//! the engine's, not a mock's. Nothing in the server depends on this crate.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use forgebase_core::{Config, DataLayout};
use forgebase_data::{AccessContext, DataService, DefaultAuthorizer};
use forgebase_db::{Backend, Database, DbOptions};

/// A migrated database in a temp directory that lives as long as the rig.
pub struct Rig {
    /// Keeps the directory alive.
    pub tmp: tempfile::TempDir,
    /// The database.
    pub db: Database,
}

impl Rig {
    /// Opens and migrates a database with the default configuration.
    pub async fn open() -> Self {
        let tmp = tempfile::tempdir_in(std::env::var_os("TMPDIR").unwrap_or_else(|| "/tmp".into())).unwrap();
        let layout = DataLayout::new(tmp.path().join("data"));
        layout.ensure().unwrap();
        let cfg = Config::defaults(tmp.path());
        let db = Database::open(DbOptions::from_config(&cfg.database, layout.database_path()))
            .await
            .unwrap();
        db.migrate(Some(&layout.backups_dir())).await.unwrap();
        Self { tmp, db }
    }

    /// The database as a backend trait object.
    pub fn backend(&self) -> Arc<dyn Backend> {
        Arc::new(self.db.clone())
    }

    /// A data service with the default (admin-or-system) authorizer.
    pub async fn data(&self) -> DataService {
        DataService::new(self.backend(), Arc::new(DefaultAuthorizer)).await.unwrap()
    }

    /// The system access context.
    pub fn system() -> AccessContext {
        AccessContext::system()
    }
}

/// Returns the `q`-quantile (0..=1) of `samples` (sorted in place), or zero when empty.
pub fn percentile(samples: &mut [u64], q: f64) -> u64 {
    if samples.is_empty() {
        return 0;
    }
    samples.sort_unstable();
    let idx = ((samples.len() as f64 - 1.0) * q).round() as usize;
    samples[idx.min(samples.len() - 1)]
}
