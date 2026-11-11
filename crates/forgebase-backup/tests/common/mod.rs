//! A real, migrated data directory with a data service on top.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use forgebase_core::DataLayout;
use forgebase_data::schema::{Collection, Field, FieldKind};
use forgebase_data::{AccessContext, DataService, DefaultAuthorizer};
use forgebase_db::{Database, DbOptions};
use serde_json::json;

pub struct Instance {
    pub layout: DataLayout,
    pub db: Database,
    pub data: DataService,
}

pub fn admin() -> AccessContext {
    AccessContext::admin("root")
}

pub fn text() -> FieldKind {
    FieldKind::Text {
        min_length: None,
        max_length: None,
    }
}

pub async fn open(root: &Path) -> Instance {
    let layout = DataLayout::new(root);
    layout.ensure().unwrap();
    let cfg = forgebase_core::Config::defaults(root);
    let db = Database::open(DbOptions::from_config(&cfg.database, layout.database_path()))
        .await
        .unwrap();
    db.migrate(Some(&layout.backups_dir())).await.unwrap();
    let data = DataService::new(Arc::new(db.clone()), Arc::new(DefaultAuthorizer))
        .await
        .unwrap();
    Instance { layout, db, data }
}

impl Instance {
    /// `authors` and `books` (books relate to authors) with a few records.
    pub async fn seed(&self, books: usize) -> (String, String) {
        let ctx = admin();
        let authors = self
            .data
            .create_collection(
                &ctx,
                Collection::base("authors").with_field(Field::new("name", text()).required()),
            )
            .await
            .unwrap();
        // Created first on purpose: a relation target must be imported first.
        let books_c = self
            .data
            .create_collection(
                &ctx,
                Collection::base("books")
                    .with_field(Field::new("title", text()).required())
                    .with_field(Field::new(
                        "author",
                        FieldKind::Relation {
                            collection_id: authors.id.clone(),
                            max_select: 1,
                            cascade_delete: false,
                        },
                    )),
            )
            .await
            .unwrap();
        self.data
            .create_record(&ctx, "authors", json!({"id": "author00000001", "name": "Ursula"}))
            .await
            .unwrap();
        for i in 0..books {
            self.data
                .create_record(
                    &ctx,
                    "books",
                    json!({"id": format!("book{i:010}"), "title": format!("Book {i}"), "author": "author00000001"}),
                )
                .await
                .unwrap();
        }
        (authors.id, books_c.id)
    }

    pub fn put_blob(&self, rel: &str, bytes: &[u8]) {
        let p = self.layout.storage_dir().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, bytes).unwrap();
    }

    pub fn put_index(&self, rel: &str, bytes: &[u8]) {
        let p = self.layout.indexes_dir().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, bytes).unwrap();
    }

    pub fn put_key(&self, bytes: &[u8]) {
        let p = forgebase_backup::backup::master_key_path(&self.layout);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, bytes).unwrap();
    }

    pub async fn count(&self, collection: &str) -> u64 {
        self.data
            .list_records(
                &admin(),
                collection,
                &forgebase_data::ListQuery {
                    per_page: Some(1),
                    ..Default::default()
                },
            )
            .await
            .unwrap()
            .total_items
            .unwrap()
    }
}

pub fn read(p: PathBuf) -> Vec<u8> {
    std::fs::read(p).unwrap()
}

// ------------------------------------------------------------- PostgreSQL

use forgebase_backup::pre_migration::PgPreMigration;
use forgebase_backup::{Password, PgSource};
use forgebase_pgengine::{Engine, EngineConfig, EngineHandle, EngineSource, Layout, Role, RoleSecrets};

/// The engine directory the PostgreSQL tests use, when they should run:
/// `FORGEBASE_TEST_BACKEND=postgres` and `FORGEBASE_PG_ENGINE_DIR` (or
/// `FORGEBASE_TEST_PG_ENGINE`, an extracted FB-130 archive).
pub fn pg_engine_dir() -> Option<PathBuf> {
    if std::env::var("FORGEBASE_TEST_BACKEND").as_deref() != Ok("postgres") {
        eprintln!("skipped: FORGEBASE_TEST_BACKEND is not postgres");
        return None;
    }
    let dir = ["FORGEBASE_TEST_PG_ENGINE", "FORGEBASE_PG_ENGINE_DIR"]
        .iter()
        .find_map(|k| std::env::var_os(k).filter(|v| !v.is_empty()));
    if dir.is_none() {
        eprintln!("skipped: no PostgreSQL engine (FORGEBASE_PG_ENGINE_DIR)");
    }
    dir.map(PathBuf::from)
}

/// Short, private temp directories: a Unix socket path holds 103 bytes.
pub fn short_tempdir() -> tempfile::TempDir {
    let base = std::env::var_os("FORGEBASE_TEST_PG_TMP")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&base).unwrap();
    tempfile::Builder::new().prefix("fb150-").tempdir_in(base).unwrap()
}

/// A data directory on the bundled engine.
pub struct PgInstance {
    pub inst: Instance,
    pub engine: EngineHandle,
    pub src: PgSource,
    pub root: PathBuf,
}

pub fn engine_config(dir: &Path) -> EngineConfig {
    EngineConfig {
        source: EngineSource::Dir(dir.to_path_buf()),
        unsafe_fast: true,
        ready_timeout: std::time::Duration::from_secs(60),
        stop_timeout: std::time::Duration::from_secs(20),
        ..EngineConfig::default()
    }
}

pub fn pg_source_of(engine: &EngineHandle, root: &Path) -> PgSource {
    let layout = Layout::primary(root);
    let secrets = RoleSecrets::load(&layout.secrets_file()).unwrap().unwrap();
    PgSource::managed(
        Some(engine.engine().root.clone()),
        engine.version().text.clone(),
        engine.socket_dir().unwrap(),
        engine.port(),
        Role::Super.name(),
        Password::new(secrets.password(Role::Super).unwrap()),
        layout.postgres_dir(),
    )
}

/// Starts the engine of `root` and opens, migrates and wires the database
/// the way the server does (pre-migration hook included).
pub async fn open_pg(engine_dir: &Path, root: &Path) -> PgInstance {
    open_pg_with(engine_dir, root, true).await
}

pub async fn open_pg_with(engine_dir: &Path, root: &Path, migrate: bool) -> PgInstance {
    let layout = DataLayout::new(root);
    layout.ensure().unwrap();
    let engine = Engine::prepare(&Layout::primary(root), &engine_config(engine_dir))
        .await
        .unwrap();
    let cfg = forgebase_core::Config::defaults(root);
    let opts = DbOptions::from_config_target(
        &cfg.database,
        forgebase_db::DbTarget::postgres(engine.url(Role::Server).expose(), true),
    );
    let db = Database::open(opts).await.unwrap();
    let src = pg_source_of(&engine, root);
    db.set_pre_migration(PgPreMigration::shared(layout.clone(), src.clone()));
    if migrate {
        db.migrate(Some(&layout.backups_dir())).await.unwrap();
    }
    let data = DataService::new(Arc::new(db.clone()), Arc::new(DefaultAuthorizer))
        .await
        .unwrap();
    PgInstance {
        inst: Instance { layout, db, data },
        engine,
        src,
        root: root.to_path_buf(),
    }
}

impl PgInstance {
    pub fn opts(&self) -> forgebase_backup::BackupOptions {
        forgebase_backup::BackupOptions {
            pg: Some(self.src.clone()),
            ..Default::default()
        }
    }

    pub async fn close(self) {
        self.inst.db.close().await;
        self.engine.stop().await.unwrap();
    }
}
