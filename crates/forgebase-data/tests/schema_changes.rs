//! Collection schema changes: alter as transactional DDL with history, and
//! rollback when a change fails part-way. Also auth/system/view collections.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::HashMap;
use std::sync::Arc;

use common::{TestHasher, admin, open_db, service, service_with};
use forgebase_core::ErrorCode;
use forgebase_data::schema::{Collection, CollectionType, Field, FieldKind};
use forgebase_data::{
    AccessContext, DataError, DefaultAuthorizer, IndexDefinition, IndexKind, ListQuery, SqlStatement, UpdateOptions,
};
use serde_json::json;

fn text() -> FieldKind {
    FieldKind::Text {
        min_length: None,
        max_length: None,
    }
}

async fn columns(svc: &forgebase_data::DataService, table: &str) -> Vec<(String, String)> {
    svc.describe_table(&admin(), table)
        .await
        .unwrap()
        .columns
        .into_iter()
        .map(|c| (c.name, c.declared_type))
        .collect()
}

#[tokio::test]
async fn alter_adds_renames_changes_and_drops_fields_keeping_data() {
    let tmp = tempfile::tempdir().unwrap();
    let db = open_db(tmp.path()).await;
    let svc = service(&db).await;
    let ctx = admin();
    let c = svc
        .create_collection(
            &ctx,
            Collection::base("items")
                .with_field(Field::new("name", text()).required())
                .with_field(Field::new("qty", text()))
                .with_field(Field::new("note", text())),
        )
        .await
        .unwrap();
    for i in 0..20 {
        svc.create_record(
            &ctx,
            "items",
            json!({"name": format!("item {i}"), "qty": format!("{i}"), "note": "n"}),
        )
        .await
        .unwrap();
    }
    svc.create_index(
        &ctx,
        IndexDefinition {
            name: "idx_items_name".into(),
            ..IndexDefinition::btree("items", &["name:lower"])
        },
    )
    .await
    .unwrap();
    svc.create_index(
        &ctx,
        IndexDefinition {
            name: "items_fts".into(),
            kind: IndexKind::Fulltext,
            ..IndexDefinition::btree("items", &["name"])
        },
    )
    .await
    .unwrap();
    svc.execute_sql(
        &ctx,
        SqlStatement {
            sql: "CREATE INDEX items_custom ON items (created)".into(),
            params: vec![],
        },
    )
    .await
    .unwrap();

    // 1) Add a field and rename one: in-place ALTERs, no rebuild.
    let mut def = c.clone();
    def.fields[0].name = "title".into();
    def.fields.push(Field::new("sku", text()).unique());
    let v2 = svc.update_collection(&ctx, "items", def).await.unwrap();
    assert_eq!(v2.version, 2);
    let rec = svc
        .list_records(
            &ctx,
            "items",
            &ListQuery {
                filter: Some("title = 'item 3'".into()),
                ..ListQuery::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(rec.items.len(), 1);
    let idx = svc.get_index(&ctx, "idx_items_name").await.unwrap();
    assert_eq!(
        idx.definition.unwrap().keys[0].path,
        "title:lower",
        "managed definitions follow renames"
    );

    // 2) Change a type (text -> integer) and drop a field: table rebuild.
    let mut def = v2.clone();
    def.fields.retain(|f| f.name != "note");
    for f in &mut def.fields {
        if f.name == "qty" {
            f.kind = FieldKind::Number {
                min: None,
                max: None,
                integer: true,
            };
        }
    }
    let v3 = svc.update_collection(&ctx, "items", def).await.unwrap();
    assert_eq!(v3.version, 3);
    let cols = columns(&svc, "items").await;
    assert!(cols.contains(&("qty".into(), "INTEGER".into())));
    assert!(!cols.iter().any(|(n, _)| n == "note"));
    let big = svc
        .count_records(&ctx, "items", Some("qty >= 10"), HashMap::new())
        .await
        .unwrap();
    assert_eq!(big, 10, "values were converted, not lost");
    let indexes: Vec<String> = svc
        .list_indexes(&ctx, Some("items"))
        .await
        .unwrap()
        .into_iter()
        .map(|i| i.name)
        .collect();
    for n in ["idx_items_name", "items_fts", "items_custom"] {
        assert!(
            indexes.contains(&n.to_string()),
            "{n} survived the rebuild: {indexes:?}"
        );
    }
    assert!(
        indexes.iter().any(|n| n.starts_with("auto_") && n.ends_with("_u")),
        "auto unique index recreated"
    );
    // Full-text triggers were recreated and rowids preserved.
    let found = svc
        .list_records(
            &ctx,
            "items",
            &ListQuery {
                search: Some("item".into()),
                ..ListQuery::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(found.total_items, Some(20));
    svc.create_record(&ctx, "items", json!({"title": "fresh thing", "qty": 1}))
        .await
        .unwrap();
    let found = svc
        .list_records(
            &ctx,
            "items",
            &ListQuery {
                search: Some("fresh".into()),
                ..ListQuery::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(found.total_items, Some(1));

    // 3) Rename the collection; relations from other collections follow.
    let other = svc
        .create_collection(
            &ctx,
            Collection::base("orders").with_field(Field::new(
                "item",
                FieldKind::Relation {
                    collection_id: c.id.clone(),
                    max_select: 1,
                    cascade_delete: false,
                },
            )),
        )
        .await
        .unwrap();
    let any = svc
        .list_records(
            &ctx,
            "items",
            // Not the default order: record order is not guaranteed, and the
            // "fresh thing" record would not match the title filter below.
            &ListQuery {
                filter: Some("title = 'item 3'".into()),
                ..ListQuery::default()
            },
        )
        .await
        .unwrap()
        .items[0]["id"]
        .clone();
    svc.create_record(&ctx, "orders", json!({"item": any})).await.unwrap();
    let mut def = v3.clone();
    def.name = "products".into();
    svc.update_collection(&ctx, "items", def).await.unwrap();
    let n = svc
        .count_records(&ctx, "orders", Some("item.title ~ 'item'"), HashMap::new())
        .await
        .unwrap();
    assert_eq!(n, 1);
    let err = svc.delete_collection(&ctx, "products").await.unwrap_err();
    assert_eq!(err.code(), "collection_referenced");
    svc.delete_collection(&ctx, &other.id).await.unwrap();

    let history = svc.schema_history(&ctx, Some(&c.id)).await.unwrap();
    let actions: Vec<&str> = history.iter().map(|h| h.action.as_str()).collect();
    assert_eq!(
        actions,
        vec!["create", "index_create", "index_create", "alter", "alter", "alter"]
    );
    assert!(history[3].statements.iter().any(|s| s.contains("RENAME COLUMN")));
    assert!(
        history[4]
            .statements
            .iter()
            .any(|s| s.starts_with("CREATE TABLE \"_forgebase_tmp_"))
    );
    assert_eq!(history[5].before.as_ref().unwrap()["name"], json!("items"));
    assert_eq!(history[5].after.as_ref().unwrap()["name"], json!("products"));
    assert!(db.integrity_check().await.unwrap().is_empty());
    assert_eq!(db.foreign_key_violations().await.unwrap(), 0);
    db.close().await;
}

#[tokio::test]
async fn failed_schema_changes_roll_back_completely() {
    let tmp = tempfile::tempdir().unwrap();
    let db = open_db(tmp.path()).await;
    let svc = service(&db).await;
    let ctx = admin();
    let c = svc
        .create_collection(
            &ctx,
            Collection::base("things")
                .with_field(Field::new("code", text()))
                .with_field(Field::new("label", text())),
        )
        .await
        .unwrap();
    for code in ["1", "2", "two", "2"] {
        svc.create_record(&ctx, "things", json!({"code": code, "label": "x"}))
            .await
            .unwrap();
    }
    svc.create_collection(
        &ctx,
        Collection {
            kind: CollectionType::View,
            view_query: Some("SELECT id, label FROM things".into()),
            ..Collection::base("thing_labels")
        },
    )
    .await
    .unwrap();
    let before_cols = columns(&svc, "things").await;
    let before_history = svc.schema_history(&ctx, None).await.unwrap().len();

    let check_unchanged = |svc: forgebase_data::DataService| {
        let before_cols = before_cols.clone();
        async move {
            assert_eq!(columns(&svc, "things").await, before_cols);
            let cur = svc.get_collection(&admin(), "things").await.unwrap();
            assert_eq!(cur.version, 1);
            assert_eq!(cur.fields.len(), 2);
            assert_eq!(
                svc.count_records(&admin(), "things", None, HashMap::new())
                    .await
                    .unwrap(),
                4
            );
            assert_eq!(svc.schema_history(&admin(), None).await.unwrap().len(), before_history);
            assert_eq!(
                svc.count_records(&admin(), "thing_labels", None, HashMap::new())
                    .await
                    .unwrap(),
                4
            );
        }
    };

    // a) Type change that existing data does not fit ("two" is not a number).
    let mut def = c.clone();
    def.fields[0].kind = FieldKind::Number {
        min: None,
        max: None,
        integer: true,
    };
    let err = svc.update_collection(&ctx, "things", def).await.unwrap_err();
    assert_eq!(err.code(), "incompatible_data", "{err:?}");
    check_unchanged(svc.clone()).await;

    // b) Making a field unique over duplicates.
    let mut def = c.clone();
    def.fields[0].unique = true;
    let err = svc.update_collection(&ctx, "things", def).await.unwrap_err();
    assert_eq!(err.code(), "duplicate_values", "{err:?}");
    check_unchanged(svc.clone()).await;

    // c) Dropping a field a view depends on.
    let mut def = c.clone();
    def.fields.retain(|f| f.name != "label");
    let err = svc.update_collection(&ctx, "things", def).await.unwrap_err();
    assert_eq!(err.code(), "schema_conflict", "{err:?}");
    check_unchanged(svc.clone()).await;

    // d) Dropping a collection a view depends on.
    let err = svc.delete_collection(&ctx, "things").await.unwrap_err();
    assert_eq!(err.code(), "schema_conflict", "{err:?}");
    check_unchanged(svc.clone()).await;

    // e) A raw DDL batch failing half-way.
    let err = svc
        .execute_sql_batch(
            &ctx,
            vec![
                SqlStatement {
                    sql: "CREATE TABLE side (x INTEGER)".into(),
                    params: vec![],
                },
                SqlStatement {
                    sql: "ALTER TABLE things DROP COLUMN code".into(),
                    params: vec![],
                },
            ],
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "schema_conflict");
    assert!(
        svc.describe_table(&ctx, "side").await.is_err(),
        "first statement rolled back too"
    );
    check_unchanged(svc.clone()).await;

    // The writer is healthy afterwards: foreign keys back on, writes work.
    assert_eq!(db.effective_pragmas(false).await.unwrap().foreign_keys, 1);
    svc.create_record(&ctx, "things", json!({"code": "3"})).await.unwrap();
    // A valid change still applies after the failures.
    let mut def = c.clone();
    def.fields.push(Field::new("extra", FieldKind::Bool));
    assert_eq!(svc.update_collection(&ctx, "things", def).await.unwrap().version, 2);
    assert!(db.integrity_check().await.unwrap().is_empty());
    db.close().await;
}

#[tokio::test]
async fn auth_view_and_system_collections() {
    let tmp = tempfile::tempdir().unwrap();
    let db = open_db(tmp.path()).await;
    let svc = service(&db).await;
    let ctx = admin();

    // Auth collection: schema only; system fields are added and protected.
    let users = svc
        .create_collection(
            &ctx,
            Collection {
                kind: CollectionType::Auth,
                ..Collection::base("users").with_field(Field::new("display_name", text()))
            },
        )
        .await
        .unwrap();
    let names: Vec<&str> = users.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "display_name",
            "email",
            "email_visibility",
            "verified",
            "password",
            "token_key"
        ]
    );
    let mut def = users.clone();
    def.fields.retain(|f| f.name != "password");
    assert!(matches!(
        svc.update_collection(&ctx, "users", def).await.unwrap_err(),
        DataError::Forbidden(_)
    ));
    // Without a secret hasher, password writes fail closed.
    let err = svc
        .create_record(
            &ctx,
            "users",
            json!({"email": "a@example.com", "password": "correct horse"}),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "password_hasher_unavailable");
    let svc = service_with(&db, Arc::new(DefaultAuthorizer), Some(Arc::new(TestHasher))).await;
    let u = svc
        .create_record(
            &ctx,
            "users",
            json!({"email": "a@example.com", "password": "correct horse", "token_key": "k"}),
        )
        .await
        .unwrap();
    assert!(u.get("password").is_none(), "passwords are never returned");
    assert_eq!(u["verified"], json!(false));
    // Secrets are masked in the SQL editor unless asked for (FB-135, G11).
    let stored = svc
        .execute_sql_with(
            &ctx,
            SqlStatement {
                sql: "SELECT password FROM users".into(),
                params: vec![],
            },
            forgebase_data::SqlOptions { show_secrets: true },
        )
        .await
        .unwrap();
    assert_eq!(stored.rows[0][0], json!("test$esroh tcerroc"));
    assert!(
        svc.count_records(&ctx, "users", Some("password = 'x'"), HashMap::new())
            .await
            .is_err()
    );
    let short = svc
        .create_record(&ctx, "users", json!({"email": "b@example.com", "password": "short"}))
        .await
        .unwrap_err();
    assert!(matches!(short, DataError::Validation(_)));
    // Updating without a password keeps the hash.
    svc.update_record(
        &ctx,
        "users",
        u["id"].as_str().unwrap(),
        json!({"verified": true}),
        UpdateOptions::default(),
    )
    .await
    .unwrap();
    let stored = svc
        .execute_sql_with(
            &ctx,
            SqlStatement {
                sql: "SELECT password, verified FROM users".into(),
                params: vec![],
            },
            forgebase_data::SqlOptions { show_secrets: true },
        )
        .await
        .unwrap();
    assert_eq!(stored.rows[0], vec![json!("test$esroh tcerroc"), json!(1)]);

    // View collection: read-only records with inferred fields.
    let view = svc
        .create_collection(
            &ctx,
            Collection {
                kind: CollectionType::View,
                view_query: Some("SELECT id, email, verified FROM users WHERE verified = 1".into()),
                ..Collection::base("verified_users")
            },
        )
        .await
        .unwrap();
    assert_eq!(
        view.fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
        vec!["email", "verified"]
    );
    let page = svc
        .list_records(
            &ctx,
            "verified_users",
            &ListQuery {
                filter: Some("email ~ 'example'".into()),
                ..ListQuery::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(page.total_items, Some(1));
    assert!(svc.create_record(&ctx, "verified_users", json!({})).await.is_err());
    for bad in [
        "DELETE FROM users",
        "SELECT * FROM _forgebase_meta",
        "SELECT 1; SELECT 2",
        "SELECT email FROM users",
    ] {
        let r = svc
            .create_collection(
                &ctx,
                Collection {
                    kind: CollectionType::View,
                    view_query: Some(bad.into()),
                    ..Collection::base("v2")
                },
            )
            .await;
        assert!(r.is_err(), "{bad}");
    }

    // System collections: only ForgeBase creates or changes them.
    let sys = Collection {
        system: true,
        ..Collection::base("_superusers").with_field(Field::new("note", text()))
    };
    assert!(matches!(
        svc.create_collection(&ctx, sys.clone()).await.unwrap_err(),
        DataError::Forbidden(_)
    ));
    let created = svc.create_collection(&AccessContext::system(), sys).await.unwrap();
    assert!(matches!(
        svc.delete_collection(&ctx, &created.id).await.unwrap_err(),
        DataError::Forbidden(_)
    ));
    assert!(matches!(
        svc.update_collection(&ctx, &created.id, created.clone())
            .await
            .unwrap_err(),
        DataError::Forbidden(_)
    ));
    svc.create_record(&ctx, "_superusers", json!({"note": "records are ordinary"}))
        .await
        .unwrap();
    svc.delete_collection(&AccessContext::system(), &created.id)
        .await
        .unwrap();

    // Definitions survive a restart.
    db.close().await;
    let db = open_db(tmp.path()).await;
    let svc = service(&db).await;
    let all: Vec<String> = svc
        .list_collections(&ctx)
        .await
        .unwrap()
        .into_iter()
        .map(|c| c.name)
        .collect();
    assert_eq!(all, vec!["users", "verified_users"]);
    db.close().await;
}
