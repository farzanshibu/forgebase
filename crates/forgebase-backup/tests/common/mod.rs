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
            .create_collection(&ctx, Collection::base("authors").with_field(Field::new("name", text()).required()))
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
