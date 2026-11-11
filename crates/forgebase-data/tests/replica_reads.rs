//! Replica reads in the data core (ADR-0029 §2.5, FB-169): each read call
//! site of the table reads a lagging standby, only when the request allows
//! it; every write and catalogue path reads the primary.
//!
//! Needs a PostgreSQL engine (`FORGEBASE_PG_ENGINE_DIR` or
//! `FORGEBASE_TEST_PG_ENGINE`), else each test prints a skip. SQLite has no
//! replicas, so a routed read there is the plain read (the other suites).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::HashMap;
use std::sync::Arc;

use forgebase_data::db::routing::testing::RoutedCluster;
use forgebase_data::db::routing::{Consistency, ReadAfter, RequestScope, scope};
use forgebase_data::schema::{Collection, Field, FieldKind};
use forgebase_data::{
    AccessContext, AggregateQuery, DataService, DefaultAuthorizer, GetOptions, IndexDefinition, IndexKind, ListQuery,
    SqlOptions, SqlStatement, UpdateOptions,
};
use serde_json::json;

fn text() -> FieldKind {
    FieldKind::Text {
        min_length: None,
        max_length: None,
    }
}

fn admin() -> AccessContext {
    common::admin()
}

struct Env {
    rc: RoutedCluster,
    svc: DataService,
    ctx: AccessContext,
}

/// Two collections (`authors`, and `notes` with a relation and versions), one
/// note replayed on the standby; then replay is paused and a second note is
/// written: the standby holds `n1`, the primary `n1` and `n2`.
async fn env() -> Option<Env> {
    let Some(rc) = RoutedCluster::start().await else {
        eprintln!("skipped: no PostgreSQL engine configured");
        return None;
    };
    let ctx = admin();
    let svc = DataService::with_hasher(rc.backend(), Arc::new(DefaultAuthorizer), None)
        .await
        .unwrap();
    let authors = svc
        .create_collection(&ctx, Collection::base("authors").with_field(Field::new("name", text())))
        .await
        .unwrap();
    let mut notes = Collection::base("notes")
        .with_field(Field::new("title", text()))
        .with_field(Field::new(
            "author",
            FieldKind::Relation {
                collection_id: authors.id.clone(),
                max_select: 1,
                cascade_delete: false,
            },
        ));
    notes.versioned = true;
    svc.create_collection(&ctx, notes).await.unwrap();
    svc.create_index(
        &ctx,
        IndexDefinition {
            name: "notes_title_fts".into(),
            kind: IndexKind::Fulltext,
            ..IndexDefinition::btree("notes", &["title"])
        },
    )
    .await
    .unwrap();
    svc.create_record(&ctx, "authors", json!({"id": "a1", "name": "Ada"}))
        .await
        .unwrap();
    svc.create_record(&ctx, "notes", json!({"id": "n1", "title": "one", "author": "a1"}))
        .await
        .unwrap();
    rc.sync().await;
    rc.lag().await;
    svc.create_record(&ctx, "notes", json!({"id": "n2", "title": "two", "author": "a1"}))
        .await
        .unwrap();
    Some(Env { rc, svc, ctx })
}

fn ids(r: &forgebase_data::ListResult) -> Vec<String> {
    r.items.iter().map(|i| i["id"].as_str().unwrap().to_owned()).collect()
}

fn strong() -> Arc<RequestScope> {
    RequestScope::new(Consistency::Strong, ReadAfter::new())
}

#[tokio::test]
async fn record_reads_use_a_lagging_replica_when_allowed() {
    let Some(e) = env().await else { return };
    let (svc, ctx, rc) = (&e.svc, &e.ctx, &e.rc);
    let q = ListQuery::default();

    // list (and its count): the standby has one note.
    let before = rc.replica_reads();
    let list = svc.list_records(ctx, "notes", &q).await.unwrap();
    assert_eq!(ids(&list), ["n1"], "list read the lagging replica");
    assert!(rc.replica_reads() > before);

    // search is a list read too.
    let before = rc.replica_reads();
    let search = ListQuery {
        search: Some("two".into()),
        ..ListQuery::default()
    };
    let found = svc.list_records(ctx, "notes", &search).await.unwrap();
    assert!(found.items.is_empty(), "n2 is not on the standby yet");
    assert!(rc.replica_reads() > before, "search read the replica");

    // view
    let before = rc.replica_reads();
    assert!(svc.get_record(ctx, "notes", "n1", &GetOptions::default()).await.is_ok());
    assert!(
        svc.get_record(ctx, "notes", "n2", &GetOptions::default()).await.is_err(),
        "view read the lagging replica: n2 is not there yet"
    );
    assert_eq!(rc.replica_reads(), before + 2);

    // count and aggregate
    let before = rc.replica_reads();
    assert_eq!(svc.count_records(ctx, "notes", None, HashMap::new()).await.unwrap(), 1);
    let agg = svc.aggregate(ctx, "notes", &AggregateQuery::default()).await.unwrap();
    assert_eq!(agg[0]["count"], json!(1));
    assert!(rc.replica_reads() >= before + 2);

    // expand: the list reads the replica and so do the relation targets.
    let before = rc.replica_reads();
    let expanded = svc
        .list_records(
            ctx,
            "notes",
            &ListQuery {
                expand: vec!["author".into()],
                ..ListQuery::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(expanded.items[0]["expand"]["author"]["name"], json!("Ada"));
    assert!(rc.replica_reads() >= before + 2, "list and expand both read the replica");

    // versions: n1 has one version on the standby; n2 none yet.
    let before = rc.replica_reads();
    assert_eq!(svc.record_versions(ctx, "notes", "n1").await.unwrap().len(), 1);
    assert!(svc.record_versions(ctx, "notes", "n2").await.is_err());
    assert!(rc.replica_reads() > before);
    assert_eq!(rc.fallbacks(), 0);
}

#[tokio::test]
async fn strong_consistency_and_tokens_keep_reads_off_the_lagging_replica() {
    let Some(e) = env().await else { return };
    let (svc, ctx, rc) = (&e.svc, &e.ctx, &e.rc);
    let q = ListQuery::default();

    // Strong: the primary, at once.
    let before = rc.replica_reads();
    let list = scope::scoped(strong(), svc.list_records(ctx, "notes", &q))
        .await
        .unwrap();
    assert_eq!(ids(&list), ["n1", "n2"]);
    assert_eq!(rc.replica_reads(), before, "a strong read never touches the replica");

    // A request that wrote reads its own write: the replica is behind its
    // commit token, so the read waits, then the primary serves it.
    let own = RequestScope::new(Consistency::Session, ReadAfter::new());
    let list = scope::scoped(own, async {
        svc.create_record(ctx, "notes", json!({"id": "n3", "title": "three", "author": "a1"}))
            .await
            .unwrap();
        svc.list_records(ctx, "notes", &q).await.unwrap()
    })
    .await;
    assert_eq!(ids(&list), ["n1", "n2", "n3"], "read-your-writes");

    // Replayed: the replica serves the same read.
    e.rc.catch_up().await;
    let before = rc.replica_reads();
    let list = svc.list_records(ctx, "notes", &q).await.unwrap();
    assert_eq!(ids(&list), ["n1", "n2", "n3"]);
    assert!(rc.replica_reads() > before);
}

#[tokio::test]
async fn the_sql_editor_reads_a_replica_only_when_asked() {
    let Some(e) = env().await else { return };
    let (svc, ctx, rc) = (&e.svc, &e.ctx, &e.rc);
    let stmt = || SqlStatement {
        sql: "SELECT count(*) AS n FROM notes".into(),
        params: Vec::new(),
    };
    let count = |r: forgebase_data::SqlResult| r.rows[0][0].as_i64().unwrap();

    let before = rc.replica_reads();
    let plain = svc.execute_sql_with(ctx, stmt(), SqlOptions::default()).await.unwrap();
    assert_eq!(count(plain), 2, "default: the primary");
    assert_eq!(rc.replica_reads(), before);

    let ticked = SqlOptions {
        read_from_replica: true,
        ..SqlOptions::default()
    };
    let on_replica = svc.execute_sql_with(ctx, stmt(), ticked).await.unwrap();
    assert_eq!(count(on_replica), 1, "\"Read from replica\": the lagging replica");
    assert_eq!(rc.replica_reads(), before + 1);

    // Revealing secrets is audited on the primary: never from a replica.
    let secrets = SqlOptions {
        read_from_replica: true,
        show_secrets: true,
    };
    let before = rc.replica_reads();
    let r = svc.execute_sql_with(ctx, stmt(), secrets).await.unwrap();
    assert_eq!(count(r), 2);
    assert_eq!(rc.replica_reads(), before);

    // Writes and batches are the primary's, whatever the flag says.
    let write = SqlStatement {
        sql: "UPDATE authors SET name = 'Ada L' WHERE id = 'a1'".into(),
        params: Vec::new(),
    };
    svc.execute_sql_with(ctx, write, ticked).await.unwrap();
    assert_eq!(rc.replica_reads(), before);
}

#[tokio::test]
async fn writes_and_the_catalogue_never_read_a_replica() {
    let Some(e) = env().await else { return };
    let (svc, ctx, rc) = (&e.svc, &e.ctx, &e.rc);
    let before = rc.replica_reads();

    // Record writes (their reads are inside the write transaction).
    svc.create_record(ctx, "notes", json!({"id": "n4", "title": "four", "author": "a1"}))
        .await
        .unwrap();
    svc.update_record(ctx, "notes", "n2", json!({"title": "two!"}), UpdateOptions::default())
        .await
        .unwrap();
    svc.delete_record(ctx, "notes", "n4", None).await.unwrap();
    // Catalogue: DDL and the reload after it.
    svc.create_collection(ctx, Collection::base("tags").with_field(Field::new("name", text())))
        .await
        .unwrap();
    svc.create_record(ctx, "tags", json!({"id": "t1", "name": "x"}))
        .await
        .unwrap();
    // The write path's own visibility checks read the lagging replica nowhere:
    // n2 exists on the primary only, and the update found it.
    assert_eq!(rc.replica_reads(), before, "writes and DDL read the primary");
    assert_eq!(rc.fallbacks(), 0);
}
