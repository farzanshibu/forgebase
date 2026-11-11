//! FB-173: collection DDL and managed indexes on every shard (ADR-0029
//! §3.7): the catalog replay of a new shard, create / update / delete
//! collection as one two-phase transaction, a failure on one shard rolling
//! back everywhere, and per-set index builds. Real, separate PostgreSQL
//! clusters; skipped without an engine.

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use forgebase_data::db::shard_testing::Sharded;
use forgebase_data::db::shards::{self, ShardSetup};
use forgebase_data::db::{Backend, Database, Statement, TxMode, Value};
use forgebase_data::indexes::IndexDefinition;
use forgebase_data::{AccessContext, DataService, DefaultAuthorizer};
use forgebase_schema::Collection;
use serde_json::{Value as Json, json};

async fn rows(db: &Database, sql: &str) -> Vec<String> {
    db.fetch_all(&Statement::new(sql, vec![]))
        .await
        .unwrap()
        .rows
        .iter()
        .filter_map(|r| r.first().and_then(Value::as_str).map(str::to_owned))
        .collect()
}

async fn tables(db: &Database) -> Vec<String> {
    rows(
        db,
        "SELECT c.relname::text FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = 'public' AND c.relkind IN ('r', 'v') ORDER BY 1",
    )
    .await
}

async fn columns(db: &Database, table: &str) -> Vec<String> {
    rows(
        db,
        &format!(
            "SELECT column_name::text FROM information_schema.columns WHERE table_schema = 'public' \
             AND table_name = '{table}' ORDER BY column_name"
        ),
    )
    .await
}

async fn indexes(db: &Database, table: &str) -> Vec<String> {
    rows(
        db,
        &format!("SELECT indexname::text FROM pg_catalog.pg_indexes WHERE schemaname = 'public' AND tablename = '{table}' ORDER BY 1"),
    )
    .await
}

async fn exec(db: &Database, sql: &str) {
    let mut tx = db.begin(TxMode::Normal).await.unwrap();
    tx.execute(&Statement::raw(sql)).await.unwrap();
    tx.commit().await.unwrap();
}

fn coll(def: Json) -> Collection {
    serde_json::from_value(def).unwrap()
}

#[tokio::test]
async fn collection_changes_and_indexes_reach_every_shard() {
    let Some(mut s) = Sharded::start().await else {
        eprintln!("skipped: no PostgreSQL engine configured");
        return;
    };
    let backend: Arc<dyn Backend> = Arc::new(s.control.clone());
    let data = DataService::new(backend, Arc::new(DefaultAuthorizer)).await.unwrap();
    let sys = AccessContext::system();

    // A catalog made before any shard: a relation cycle, a tenant-scoped
    // collection, a view and a manual index.
    data.create_collection(&sys, coll(json!({"id": "authors_c", "name": "authors", "fields": [
        {"name": "name", "type": "text"}
    ]})))
    .await
    .unwrap();
    data.create_collection(&sys, coll(json!({"id": "posts_c", "name": "posts", "fields": [
        {"name": "title", "type": "text"},
        {"name": "author", "type": "relation", "collection_id": "authors_c"}
    ]})))
    .await
    .unwrap();
    let mut authors = data.get_collection(&sys, "authors").await.unwrap();
    authors.fields.push(coll(json!({"name": "x", "fields": [
        {"name": "best", "type": "relation", "collection_id": "posts_c"}
    ]})).fields.remove(0));
    data.update_collection(&sys, "authors", authors).await.unwrap();
    data.create_collection(&sys, coll(json!({"name": "notes", "options": {"tenant_scoped": true}, "fields": [
        {"name": "body", "type": "text"}
    ]})))
    .await
    .unwrap();
    data.create_collection(&sys, coll(json!({"name": "post_titles", "type": "view",
        "view_query": "SELECT id, title FROM posts"})))
    .await
    .unwrap();
    let mut ix = IndexDefinition::btree("posts", &["title"]);
    ix.name = "posts_title".into();
    data.create_index(&sys, ix).await.unwrap();

    // Two shards replay it.
    let mut shard_dbs = Vec::new();
    for name in ["s1", "s2"] {
        let (set, db) = s.provision(name).await;
        shards::activate_shard(
            &s.router,
            &set,
            &db,
            ShardSetup {
                backups_dir: None,
                replay: Some(&data),
                control_conninfo: None,
            },
        )
        .await
        .unwrap();
        shard_dbs.push(db);
    }
    assert!(data.attach_shards(s.router.clone()));
    let all: Vec<Database> = std::iter::once(s.control.clone()).chain(shard_dbs.clone()).collect();
    let want = ["authors", "notes", "post_titles", "posts"];
    for db in &all {
        assert_eq!(tables(db).await, want);
        assert_eq!(columns(db, "authors").await, columns(&s.control, "authors").await);
        assert_eq!(indexes(db, "posts").await, indexes(&s.control, "posts").await);
        assert!(indexes(db, "posts").await.contains(&"posts_title".to_owned()));
        assert!(columns(db, "notes").await.contains(&"_tenant".to_owned()));
    }

    // Create, update and delete run on every node.
    data.create_collection(&sys, coll(json!({"name": "tags", "fields": [{"name": "label", "type": "text"}]})))
        .await
        .unwrap();
    for db in &all {
        assert!(tables(db).await.contains(&"tags".to_owned()));
    }
    let mut posts = data.get_collection(&sys, "posts").await.unwrap();
    posts.fields.iter_mut().find(|f| f.name == "title").unwrap().name = "headline".into();
    posts.fields.push(coll(json!({"name": "x", "fields": [{"name": "body", "type": "text"}]})).fields.remove(0));
    // The view reads `title`: it follows the rename.
    data.update_collection(&sys, "posts", posts).await.unwrap();
    for db in &all {
        let cols = columns(db, "posts").await;
        assert!(cols.contains(&"headline".to_owned()) && cols.contains(&"body".to_owned()), "{cols:?}");
        assert!(!cols.contains(&"title".to_owned()));
    }
    data.delete_collection(&sys, "tags").await.unwrap();
    for db in &all {
        assert!(!tables(db).await.contains(&"tags".to_owned()));
    }
    // Every change was one committed two-phase transaction, nothing in doubt.
    let journal = rows(&s.control, "SELECT state FROM _forgebase_xact_journal").await;
    assert_eq!(journal, ["committed"; 3]);
    for db in &all {
        assert!(db.prepared_transactions().await.unwrap().is_empty());
    }

    // A failure on one shard changes nothing anywhere.
    exec(&shard_dbs[1], "CREATE TABLE public.clash (x int)").await;
    let err = data
        .create_collection(&sys, coll(json!({"name": "clash", "fields": [{"name": "a", "type": "text"}]})))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("clash"), "{err}");
    assert!(data.get_collection(&sys, "clash").await.is_err());
    assert!(!tables(&s.control).await.contains(&"clash".to_owned()));
    assert!(!tables(&shard_dbs[0]).await.contains(&"clash".to_owned()));
    assert_eq!(columns(&shard_dbs[1], "clash").await, ["x"]);
    for db in &all {
        assert!(db.prepared_transactions().await.unwrap().is_empty());
    }

    // Managed indexes build per set; a failure is recorded for that set.
    exec(
        &shard_dbs[1],
        "INSERT INTO public.posts (id, created, updated, headline, body) VALUES \
         ('p1', '', '', 'a', 'same'), ('p2', '', '', 'b', 'same')",
    )
    .await;
    let mut unique = IndexDefinition::btree("posts", &["body"]);
    unique.name = "posts_body".into();
    unique.unique = true;
    data.create_index(&sys, unique).await.unwrap();
    let builds = data.index_builds(&sys, "posts_body").await.unwrap();
    assert_eq!(builds.len(), 2, "{builds:?}");
    let s1 = s.router.sets()[1].id().to_owned();
    for b in &builds {
        if b.set_id == s1 {
            assert_eq!(b.state, "ready");
        } else {
            assert_eq!(b.state, "failed");
            assert!(b.error.as_deref().unwrap_or_default().to_ascii_lowercase().contains("unique"), "{b:?}");
        }
    }
    assert!(indexes(&shard_dbs[0], "posts").await.contains(&"posts_body".to_owned()));
    assert!(!indexes(&shard_dbs[1], "posts").await.contains(&"posts_body".to_owned()));
    data.drop_index(&sys, "posts_body").await.unwrap();
    assert!(!indexes(&shard_dbs[0], "posts").await.contains(&"posts_body".to_owned()));
    assert!(data.index_builds(&sys, "posts_body").await.is_err());
}
