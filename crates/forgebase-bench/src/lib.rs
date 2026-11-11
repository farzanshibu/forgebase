//! Shared rig for the ForgeBase benchmarks (FB-81).
//!
//! Everything here is dev tooling: it opens real databases in a temp
//! directory with the production settings and migrations, so the numbers are
//! the engine's, not a mock's. Nothing in the server depends on this crate.
//!
//! The backend follows `FORGEBASE_TEST_BACKEND` (FB-148): `sqlite` (the
//! default) or `postgres`, the bundled engine started the way the server
//! starts it (`forgebase_server::database::open_owned`), from
//! `FORGEBASE_PG_ENGINE_DIR` in a build without the embedded engine.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use forgebase_core::config::{DatabaseBackend, PostgresMode};
use forgebase_core::{Config, DataLayout};
use forgebase_data::{AccessContext, DataService, DefaultAuthorizer};
use forgebase_db::{Backend, Database};
use forgebase_server::database::{Create, PrimaryDatabase, open_owned};

/// The backend the benchmarks run on: `FORGEBASE_TEST_BACKEND=postgres`
/// selects the bundled PostgreSQL, anything else SQLite.
pub fn backend() -> DatabaseBackend {
    match std::env::var("FORGEBASE_TEST_BACKEND").as_deref() {
        Ok("postgres") => DatabaseBackend::Postgres,
        _ => DatabaseBackend::Sqlite,
    }
}

/// Default configuration under `dir` on [`backend`] (the server benchmarks
/// pass it to `forgebase_server::start`).
pub fn config(dir: &std::path::Path) -> Config {
    let mut cfg = Config::defaults(dir);
    cfg.database.backend = backend();
    cfg.database.postgres.mode = PostgresMode::Managed;
    cfg
}

/// A migrated database in a temp directory that lives as long as the rig.
pub struct Rig {
    /// Keeps the directory alive.
    pub tmp: tempfile::TempDir,
    /// The database.
    pub db: Database,
    /// The opened database with the engine it started (PostgreSQL).
    primary: Option<PrimaryDatabase>,
}

impl Rig {
    /// Opens and migrates a database with the default configuration on
    /// [`backend`].
    pub async fn open() -> Self {
        let tmp = tempfile::tempdir_in(std::env::var_os("TMPDIR").unwrap_or_else(|| "/tmp".into())).unwrap();
        let cfg = config(tmp.path());
        let layout = DataLayout::new(&cfg.data_dir);
        layout.ensure().unwrap();
        let primary = open_owned(&cfg, true, &layout, Create::Yes).await.unwrap();
        let db = primary.db().clone();
        db.migrate(Some(&layout.backups_dir())).await.unwrap();
        Self {
            tmp,
            db,
            primary: Some(primary),
        }
    }

    /// Closes the database and stops the engine (PostgreSQL). Dropping the
    /// rig stops the engine too, without waiting.
    pub async fn close(mut self) {
        if let Some(p) = self.primary.take() {
            p.close().await;
        }
    }

    /// The database as a backend trait object.
    pub fn backend(&self) -> Arc<dyn Backend> {
        Arc::new(self.db.clone())
    }

    /// A data service with the default (admin-or-system) authorizer.
    pub async fn data(&self) -> DataService {
        DataService::new(self.backend(), Arc::new(DefaultAuthorizer))
            .await
            .unwrap()
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The rig opens, migrates and answers on the selected backend (run with
    /// `FORGEBASE_TEST_BACKEND=postgres` and `FORGEBASE_PG_ENGINE_DIR` for
    /// PostgreSQL).
    #[tokio::test]
    async fn rig_opens_on_the_selected_backend() {
        let rig = Rig::open().await;
        let want = match backend() {
            DatabaseBackend::Sqlite => forgebase_db::BackendKind::Sqlite,
            DatabaseBackend::Postgres => forgebase_db::BackendKind::Postgres,
        };
        assert_eq!(rig.db.kind(), want);
        let rows = rig
            .db
            .fetch_all(&forgebase_db::Statement::raw(
                "SELECT count(*) FROM _forgebase_migrations",
            ))
            .await
            .unwrap();
        assert!(rows.scalar().and_then(forgebase_db::Value::as_i64).unwrap_or(0) > 0);
        let data = rig.data().await;
        drop(data);
        rig.close().await;
    }

    #[test]
    fn percentiles() {
        let mut s = vec![5, 1, 3, 2, 4];
        assert_eq!(percentile(&mut s, 0.5), 3);
        assert_eq!(percentile(&mut [], 0.5), 0);
    }
}
