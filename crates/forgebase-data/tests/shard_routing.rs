//! FB-171: shard routing in the data core (ADR-0029 §3.5–§3.6).
//!
//! * On one set (both backends, `FORGEBASE_TEST_BACKEND`): an attached
//!   one-set router changes nothing (a privileged caller without a tenant
//!   still lists every tenant's rows; relations keep their foreign keys),
//!   and the pre-check lists stored rules that would cross shards.
//! * Sharded (real, separate PostgreSQL clusters; skipped without an
//!   engine): records of a tenant-scoped collection go to the tenant's set,
//!   shared collections stay on control, `tenant_required_sharded`,
//!   relations from tenant data to shared collections (existence check on
//!   control, no foreign key, two-step `expand`, `ON DELETE` cascade / set
//!   null / multi-value removal on every set, `RESTRICT` across sets), and
//!   `relation_crosses_shards` for rules, filters, sort keys and saved rules.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use forgebase_core::ErrorCode;
use forgebase_data::db::{Backend, Database, DbRouter, Statement, TxMode, Value};
use forgebase_data::{AccessContext, DataService, DefaultAuthorizer, GetOptions, ListQuery, Predicate};
use forgebase_schema::Collection;
use serde_json::{Value as Json, json};

fn coll(def: Json) -> Collection {
    serde_json::from_value(def).unwrap()
}

fn admin_in(t: &str) -> AccessContext {
    AccessContext::admin("root").with_tenant(t)
}

async fn count(db: &dyn Backend, sql: &str) -> i64 {
    db.fetch_all(&Statement::new(sql, vec![]))
        .await
        .unwrap()
        .scalar()
        .and_then(Value::as_i64)
        .unwrap_or(-1)
}

/// `users` (shared), `posts` (tenant-scoped, `author` → users).
async fn base_collections(data: &DataService) {
    let sys = AccessContext::system();
    data.create_collection(
        &sys,
        coll(json!({"id": "users_c", "name": "users", "fields": [{"name": "name", "type": "text"}]})),
    )
    .await
    .unwrap();
    data.create_collection(
        &sys,
        coll(json!({"id": "posts_c", "name": "posts", "options": {"tenant_scoped": true}, "fields": [
            {"name": "title", "type": "text"},
            {"name": "author", "type": "relation", "collection_id": "users_c"}
        ]})),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn one_set_behaves_as_before() {
    let tdb = common::test_db().await;
    let db: &Database = &tdb;
    let data = common::service(db).await;
    assert!(data.attach_shards(DbRouter::single(db.clone())));
    assert!(!data.is_sharded());
    base_collections(&data).await;
    let u = data
        .create_record(&common::admin(), "users", json!({"name": "ann"}))
        .await
        .unwrap();
    let uid = u["id"].as_str().unwrap().to_owned();
    for t in ["t1", "t2"] {
        data.create_record(&admin_in(t), "posts", json!({"title": t, "author": uid}))
            .await
            .unwrap();
    }
    // A privileged caller without a tenant still sees every tenant's rows.
    let all = data
        .list_records(&common::admin(), "posts", &ListQuery::default())
        .await
        .unwrap();
    assert_eq!(all.items.len(), 2);
    // Rules and filters may follow the relation on one set.
    let q = ListQuery {
        filter: Some("author.name = 'ann'".into()),
        ..ListQuery::default()
    };
    assert_eq!(data.list_records(&admin_in("t1"), "posts", &q).await.unwrap().items.len(), 1);
    let rule = Predicate::parse("author.name = 'ann'", Default::default()).unwrap();
    let posts = data.get_collection(&common::admin(), "posts").await.unwrap();
    data.validate_predicate(&posts, &rule).unwrap();
    // Deleting the shared record still clears the reference in place.
    data.delete_record(&common::admin(), "users", &uid, None).await.unwrap();
    let left = data
        .list_records(&admin_in("t1"), "posts", &ListQuery::default())
        .await
        .unwrap();
    assert!(left.items[0]["author"].is_null() || left.items[0].get("author").is_none());
    // Without shard sets no foreign key is dropped.
    assert_eq!(data.drop_cross_placement_keys().await.unwrap(), 0);
}

#[tokio::test]
async fn precheck_lists_rules_that_would_cross_shards() {
    let tdb = common::test_db().await;
    let db: &Database = &tdb;
    let data = common::service(db).await;
    base_collections(&data).await;
    let mut tx = db.begin(TxMode::Normal).await.unwrap();
    tx.execute(&Statement::new(
        "INSERT INTO _forgebase_collection_policies (collection_id, list_rule, view_rule, create_rule, update_rule, delete_rule, fields, updated_by, updated_at) \
         VALUES ('posts_c', 'author.name = @request.auth.id', 'title = \"author.name\"', NULL, '', NULL, '{}', 'test', 1)",
        vec![],
    ))
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let offenders = data.cross_shard_offenders().await.unwrap();
    assert_eq!(offenders.len(), 1, "{offenders:?}");
    assert_eq!(offenders[0].collection, "posts");
    assert_eq!(offenders[0].rule, "list_rule");
    assert_eq!(offenders[0].path, "author.name");
}

#[cfg(unix)]
mod sharded {
    use super::*;
    use forgebase_data::db::shard_testing::Sharded;
    use forgebase_data::db::shards::{self, ShardSetup};
    use forgebase_data::db::topology::PlacementState;

    async fn fk_count(db: &Database, table: &str) -> i64 {
        count(
            db,
            &format!(
                "SELECT count(*) FROM pg_catalog.pg_constraint WHERE contype = 'f' \
                 AND conrelid = to_regclass('public.\"{table}\"')"
            ),
        )
        .await
    }

    fn code(e: &forgebase_data::DataError) -> &'static str {
        e.code()
    }

    #[tokio::test]
    async fn records_route_to_the_tenants_set_and_relations_cross_by_rule() {
        let Some(mut s) = Sharded::start().await else {
            eprintln!("skipped: no PostgreSQL engine configured");
            return;
        };
        let backend: Arc<dyn Backend> = Arc::new(s.control.clone());
        let data = DataService::new(backend, Arc::new(DefaultAuthorizer)).await.unwrap();
        let sys = AccessContext::system();
        base_collections(&data).await;
        // Cascade, required (RESTRICT) and multi-value relations to users.
        for def in [
            json!({"name": "likes", "options": {"tenant_scoped": true}, "fields": [
                {"name": "user", "type": "relation", "collection_id": "users_c", "cascade_delete": true}]}),
            json!({"name": "duties", "options": {"tenant_scoped": true}, "fields": [
                {"name": "owner", "type": "relation", "collection_id": "users_c", "required": true}]}),
            json!({"name": "teams", "options": {"tenant_scoped": true}, "fields": [
                {"name": "members", "type": "relation", "collection_id": "users_c", "max_select": 5}]}),
        ] {
            data.create_collection(&sys, coll(def)).await.unwrap();
        }
        assert!(fk_count(&s.control, "posts").await >= 1, "control keeps its keys before a shard");

        let (set, shard) = s.provision("s1").await;
        shards::activate_shard(
            &s.router,
            &set,
            &shard,
            ShardSetup {
                backups_dir: None,
                replay: Some(&data),
                control_conninfo: None,
            },
        )
        .await
        .unwrap();
        assert!(data.attach_shards(s.router.clone()));
        assert!(data.is_sharded());
        data.reload_catalog().await.unwrap();
        // The replayed shard has no key from tenant data to shared tables;
        // control's are dropped once sharded.
        for t in ["posts", "likes", "duties"] {
            assert_eq!(fk_count(&shard, t).await, 0, "{t} on the shard");
        }
        assert!(data.drop_cross_placement_keys().await.unwrap() >= 3);
        assert_eq!(fk_count(&s.control, "posts").await, 0);
        assert_eq!(data.drop_cross_placement_keys().await.unwrap(), 0, "idempotent");

        // t1 lives on s1; t0 has no placement row (control).
        s.router.place("t1", &set.id, PlacementState::Active).await.unwrap();
        let ann = data
            .create_record(&common::admin(), "users", json!({"name": "ann"}))
            .await
            .unwrap();
        let ann = ann["id"].as_str().unwrap().to_owned();
        assert_eq!(count(&shard, "SELECT count(*) FROM users").await, 0, "shared rows stay on control");

        // Existence check of a shared target on control.
        let e = data
            .create_record(&admin_in("t1"), "posts", json!({"title": "x", "author": "missing"}))
            .await
            .unwrap_err();
        assert_eq!(code(&e), "validation_failed", "{e}");
        let p1 = data
            .create_record(&admin_in("t1"), "posts", json!({"title": "on s1", "author": ann}))
            .await
            .unwrap();
        let p1 = p1["id"].as_str().unwrap().to_owned();
        data.create_record(&admin_in("t0"), "posts", json!({"title": "on control", "author": ann}))
            .await
            .unwrap();
        assert_eq!(count(&shard, "SELECT count(*) FROM posts").await, 1);
        assert_eq!(count(&s.control, "SELECT count(*) FROM posts").await, 1);

        // Reads, updates and versions route per call.
        let got = data
            .get_record(
                &admin_in("t1"),
                "posts",
                &p1,
                &GetOptions {
                    expand: vec!["author".into()],
                    ..GetOptions::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(got["title"], "on s1");
        assert_eq!(got["expand"]["author"]["name"], "ann", "two-step expand from control");
        assert!(data
            .get_record(&admin_in("t0"), "posts", &p1, &GetOptions::default())
            .await
            .is_err());
        data.update_record(&admin_in("t1"), "posts", &p1, json!({"title": "edited"}), Default::default())
            .await
            .unwrap();
        let list = data
            .list_records(&admin_in("t1"), "posts", &ListQuery::default())
            .await
            .unwrap();
        assert_eq!(list.items.len(), 1);
        assert_eq!(list.items[0]["title"], "edited");
        assert_eq!(data.count_records(&admin_in("t1"), "posts", None, Default::default()).await.unwrap(), 1);

        // tenant_required_sharded: tenant data without a tenant.
        let e = data
            .list_records(&common::admin(), "posts", &ListQuery::default())
            .await
            .unwrap_err();
        assert_eq!(code(&e), "tenant_required_sharded");
        let e = data
            .create_record(&common::admin(), "posts", json!({"title": "?"}))
            .await
            .unwrap_err();
        assert_eq!(code(&e), "tenant_required_sharded");
        // Shared collections need no tenant.
        assert_eq!(
            data.list_records(&common::admin(), "users", &ListQuery::default())
                .await
                .unwrap()
                .items
                .len(),
            1
        );

        // relation_crosses_shards: filters, sort keys, saved rules.
        let crossing = ListQuery {
            filter: Some("author.name = 'ann'".into()),
            ..ListQuery::default()
        };
        let e = data.list_records(&admin_in("t1"), "posts", &crossing).await.unwrap_err();
        assert_eq!(code(&e), "relation_crosses_shards", "{e}");
        let e = data
            .count_records(&admin_in("t1"), "posts", Some("author.name = 'ann'"), Default::default())
            .await
            .unwrap_err();
        assert_eq!(code(&e), "relation_crosses_shards");
        let sorted = ListQuery {
            sort: Some("-author.name".into()),
            ..ListQuery::default()
        };
        let e = data.list_records(&admin_in("t1"), "posts", &sorted).await.unwrap_err();
        assert_eq!(code(&e), "relation_crosses_shards");
        let posts = data.get_collection(&common::admin(), "posts").await.unwrap();
        let rule = Predicate::parse("author.name = 'ann'", Default::default()).unwrap();
        let e = data.validate_predicate(&posts, &rule).unwrap_err();
        assert_eq!(code(&e), "relation_crosses_shards");
        // The relation's own column is not a crossing.
        let own = ListQuery {
            filter: Some(format!("author = '{ann}'")),
            ..ListQuery::default()
        };
        assert_eq!(data.list_records(&admin_in("t1"), "posts", &own).await.unwrap().items.len(), 1);

        // ON DELETE across sets: RESTRICT first.
        let bob = data
            .create_record(&common::admin(), "users", json!({"name": "bob"}))
            .await
            .unwrap();
        let bob = bob["id"].as_str().unwrap().to_owned();
        data.create_record(&admin_in("t1"), "duties", json!({"owner": bob}))
            .await
            .unwrap();
        let e = data.delete_record(&common::admin(), "users", &bob, None).await.unwrap_err();
        assert_eq!(code(&e), "record_referenced", "{e}");
        assert!(data
            .get_record(&common::admin(), "users", &bob, &GetOptions::default())
            .await
            .is_ok());

        // Cascade, set null and multi-value removal run on every set.
        for t in ["t1", "t0"] {
            data.create_record(&admin_in(t), "likes", json!({"user": ann}))
                .await
                .unwrap();
            data.create_record(&admin_in(t), "teams", json!({"members": [ann, bob]}))
                .await
                .unwrap();
        }
        assert_eq!(count(&shard, "SELECT count(*) FROM likes").await, 1);
        data.delete_record(&common::admin(), "users", &ann, None).await.unwrap();
        for db in [&shard, &s.control] {
            assert_eq!(count(db, "SELECT count(*) FROM likes").await, 0, "cascade");
            assert_eq!(count(db, "SELECT count(*) FROM posts WHERE author IS NOT NULL").await, 0, "set null");
            let sql = format!(
                "SELECT count(*) FROM teams WHERE CAST(members AS text) NOT LIKE '%{ann}%' AND CAST(members AS text) LIKE '%{bob}%'"
            );
            assert_eq!(count(db, &sql).await, 1, "id pulled out of the multi-value relation");
        }
        // Idempotent: a second run finds nothing to change.
        let again = data
            .apply_relation_deletes(&forgebase_data::RelationDeletes {
                collection_id: "users_c".into(),
                ids: vec![ann.clone()],
            })
            .await
            .unwrap();
        assert!(again.values().all(|n| *n == 0), "{again:?}");
    }
}
