//! Collection schema changes: alter as transactional DDL with history, and
//! rollback when a change fails part-way. Also auth/system/view collections,
//! the index manager and full text. Every test runs on the backend
//! `FORGEBASE_TEST_BACKEND` selects (`sqlite`, the default, or `postgres`;
//! ADR-0028 §7.7): SQLite rebuilds tables, PostgreSQL alters them in place,
//! and the observable behaviour is the same.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::HashMap;
use std::sync::Arc;

use common::{TestHasher, admin, service, service_with};
use forgebase_core::ErrorCode;
use forgebase_data::db::testing::{TestDb, test_backend};
use forgebase_data::db::{Backend, BackendKind, Database, Statement, Value};
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

/// A fresh, migrated database on the selected backend.
async fn test_db() -> TestDb {
    test_backend().await
}

fn is_pg(db: &Database) -> bool {
    db.kind() == BackendKind::Postgres
}

/// Opens the test's database again (a restart).
async fn reopen(db: &TestDb) -> Database {
    let reopened = Database::open(db.options()).await.unwrap();
    reopened.migrate(Some(&db.data_dir().join("backups"))).await.unwrap();
    reopened
}

/// Columns of `table` and their declared types, from the engine's catalog.
async fn columns(svc: &forgebase_data::DataService, table: &str) -> Vec<(String, String)> {
    let b = svc.backend();
    let rows = b.fetch_all(&b.dialect().introspect_columns(table)).await.unwrap();
    let (n, t) = (rows.column("name").unwrap(), rows.column("type").unwrap());
    rows.rows
        .iter()
        .map(|r| {
            (
                r[n].as_str().unwrap_or_default().to_owned(),
                r[t].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

/// One value from the database, as the system sees it.
async fn scalar(db: &Database, sql: &str, params: Vec<Value>) -> Value {
    db.fetch_all(&Statement::new(sql, params))
        .await
        .unwrap()
        .scalar()
        .cloned()
        .unwrap_or(Value::Null)
}

#[tokio::test]
async fn alter_adds_renames_changes_and_drops_fields_keeping_data() {
    let db = test_db().await;
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
    let int_type = if is_pg(&db) { "bigint" } else { "INTEGER" };
    assert!(cols.contains(&("qty".into(), int_type.into())), "{cols:?}");
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
    if is_pg(&db) {
        // PostgreSQL changes the column in place: no rebuild, no row copy.
        let alter = &history[4].statements;
        assert!(
            alter.iter().any(|s| s.contains("ALTER COLUMN \"qty\" TYPE bigint")),
            "{alter:?}"
        );
        assert!(alter.iter().any(|s| s.contains("DROP COLUMN \"note\"")), "{alter:?}");
        assert!(!alter.iter().any(|s| s.contains("_forgebase_tmp_")), "{alter:?}");
    } else {
        assert!(
            history[4]
                .statements
                .iter()
                .any(|s| s.starts_with("CREATE TABLE \"_forgebase_tmp_"))
        );
    }
    assert_eq!(history[5].before.as_ref().unwrap()["name"], json!("items"));
    assert_eq!(history[5].after.as_ref().unwrap()["name"], json!("products"));
    assert!(db.integrity_check().await.unwrap().is_empty());
    assert_eq!(db.foreign_key_violations().await.unwrap(), 0);
    db.close().await;
}

#[tokio::test]
async fn failed_schema_changes_roll_back_completely() {
    let db = test_db().await;
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
    if !is_pg(&db) {
        assert_eq!(db.effective_pragmas(false).await.unwrap().foreign_keys, 1);
    }
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
    let db = test_db().await;
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
    let db2 = reopen(&db).await;
    let svc = service(&db2).await;
    let all: Vec<String> = svc
        .list_collections(&ctx)
        .await
        .unwrap()
        .into_iter()
        .map(|c| c.name)
        .collect();
    assert_eq!(all, vec!["users", "verified_users"]);
    db2.close().await;
}

fn int() -> FieldKind {
    FieldKind::Number {
        min: None,
        max: None,
        integer: true,
    }
}

fn real() -> FieldKind {
    FieldKind::Number {
        min: None,
        max: None,
        integer: false,
    }
}

fn json_kind() -> FieldKind {
    FieldKind::Json { max_size: None }
}

/// Records of `coll` sorted by `sort`.
async fn sorted(svc: &forgebase_data::DataService, coll: &str, sort: &str) -> Vec<serde_json::Value> {
    svc.list_records(
        &admin(),
        coll,
        &ListQuery {
            sort: Some(sort.into()),
            ..ListQuery::default()
        },
    )
    .await
    .unwrap()
    .items
    .into_iter()
    .map(serde_json::Value::Object)
    .collect()
}

#[tokio::test]
async fn type_changes_convert_values_or_refuse_them() {
    let db = test_db().await;
    let svc = service(&db).await;
    let ctx = admin();
    let c = svc
        .create_collection(
            &ctx,
            Collection::base("vals")
                .with_field(Field::new("a", int()))
                .with_field(Field::new("b", real()))
                .with_field(Field::new("flag", FieldKind::Bool))
                .with_field(Field::new("doc", text()))
                .with_field(Field::new("j", json_kind())),
        )
        .await
        .unwrap();
    svc.create_records(
        &ctx,
        "vals",
        vec![
            json!({"a": 3, "b": 2.5, "flag": true, "doc": "{\"k\": 1}", "j": {"x": [1, 2]}}),
            json!({"a": 4, "b": 3.0, "flag": false, "doc": "[1]", "j": "s"}),
        ],
    )
    .await
    .unwrap();

    // int -> real, real -> text, bool -> int, text -> json, json -> text.
    let mut def = c.clone();
    for f in &mut def.fields {
        f.kind = match f.name.as_str() {
            "a" => real(),
            "b" | "j" => text(),
            "flag" => int(),
            "doc" => json_kind(),
            other => unreachable!("{other}"),
        };
    }
    let v2 = svc.update_collection(&ctx, "vals", def).await.unwrap();
    let rows = sorted(&svc, "vals", "a").await;
    assert_eq!(rows[0]["a"], json!(3.0));
    assert_eq!(rows[0]["b"], json!("2.5"));
    assert_eq!(rows[1]["b"], json!("3.0"), "integral reals keep SQLite's text form");
    assert_eq!((rows[0]["flag"].clone(), rows[1]["flag"].clone()), (json!(1), json!(0)));
    assert_eq!(rows[0]["doc"], json!({"k": 1}));
    assert_eq!(rows[1]["doc"], json!([1]));
    // JSON as text: the same document (PostgreSQL's jsonb text adds spaces).
    let j: serde_json::Value = serde_json::from_str(rows[0]["j"].as_str().unwrap()).unwrap();
    assert_eq!(j, json!({"x": [1, 2]}));
    assert_eq!(rows[1]["j"], json!("\"s\""));

    // A fraction does not fit an integer: refused, nothing changes.
    let mut def = v2.clone();
    def.fields.iter_mut().find(|f| f.name == "b").unwrap().kind = int();
    let err = svc.update_collection(&ctx, "vals", def).await.unwrap_err();
    assert_eq!(err.code(), "incompatible_data", "{err:?}");
    assert_eq!(svc.get_collection(&ctx, "vals").await.unwrap().version, 2);
    assert_eq!(sorted(&svc, "vals", "a").await, rows);

    // Integral reals do: real -> int keeps the values.
    let mut def = v2.clone();
    def.fields.iter_mut().find(|f| f.name == "a").unwrap().kind = int();
    svc.update_collection(&ctx, "vals", def).await.unwrap();
    let rows = sorted(&svc, "vals", "a").await;
    assert_eq!((rows[0]["a"].clone(), rows[1]["a"].clone()), (json!(3), json!(4)));
    let int_type = if is_pg(&db) { "bigint" } else { "INTEGER" };
    assert!(columns(&svc, "vals").await.contains(&("a".into(), int_type.into())));
    assert!(db.integrity_check().await.unwrap().is_empty());
    db.close().await;
}

#[tokio::test]
async fn views_survive_column_changes_and_run_as_the_caller() {
    let db = test_db().await;
    let svc = service(&db).await;
    let ctx = admin();
    let src = svc
        .create_collection(
            &ctx,
            Collection::base("src")
                .with_field(Field::new("n", text()))
                .with_field(Field::new("label", text())),
        )
        .await
        .unwrap();
    svc.create_record(&ctx, "src", json!({"n": "5", "label": "five"}))
        .await
        .unwrap();
    for (name, query) in [
        ("src_view", "SELECT id, n, label FROM src"),
        ("src_view2", "SELECT id, label FROM src_view"),
    ] {
        svc.create_collection(
            &ctx,
            Collection {
                kind: CollectionType::View,
                view_query: Some(query.into()),
                ..Collection::base(name)
            },
        )
        .await
        .unwrap();
    }
    let invoker = "SELECT array_to_string(reloptions, ',') FROM pg_catalog.pg_class WHERE relname = ?1";
    if is_pg(&db) {
        assert_eq!(
            scalar(&db, invoker, vec!["src_view".into()]).await,
            Value::Text("security_invoker=true".into())
        );
    }

    // A type change under two layers of views: they are rebuilt on
    // PostgreSQL (which refuses to change a column a view reads).
    let mut def = src.clone();
    def.fields[0].kind = int();
    let v2 = svc.update_collection(&ctx, "src", def).await.unwrap();
    let rows = sorted(&svc, "src_view", "n").await;
    assert_eq!(rows[0]["n"], json!(5));
    assert_eq!(
        svc.count_records(&ctx, "src_view2", Some("label = 'five'"), HashMap::new())
            .await
            .unwrap(),
        1
    );
    if is_pg(&db) {
        for v in ["src_view", "src_view2"] {
            assert_eq!(
                scalar(&db, invoker, vec![v.into()]).await,
                Value::Text("security_invoker=true".into()),
                "{v} keeps security_invoker"
            );
        }
    }

    // Dropping a column the views read is refused, and so is dropping the
    // table or a view another view reads; nothing changes.
    let mut def = v2.clone();
    def.fields.retain(|f| f.name != "label");
    let err = svc.update_collection(&ctx, "src", def).await.unwrap_err();
    assert_eq!(err.code(), "schema_conflict", "{err:?}");
    let err = svc.delete_collection(&ctx, "src").await.unwrap_err();
    assert_eq!(err.code(), "schema_conflict", "{err:?}");
    let err = svc.delete_collection(&ctx, "src_view").await.unwrap_err();
    assert_eq!(err.code(), "schema_conflict", "{err:?}");
    assert_eq!(svc.get_collection(&ctx, "src").await.unwrap().version, 2);
    assert_eq!(
        svc.count_records(&ctx, "src_view2", None, HashMap::new()).await.unwrap(),
        1
    );

    // Changing a view's query rebuilds the views reading it.
    let view = svc.get_collection(&ctx, "src_view").await.unwrap();
    let changed = Collection {
        view_query: Some("SELECT id, n, label, label AS again FROM src".into()),
        ..view
    };
    let changed = svc.update_collection(&ctx, "src_view", changed).await.unwrap();
    assert!(changed.fields.iter().any(|f| f.name == "again"));
    assert_eq!(
        svc.count_records(&ctx, "src_view2", None, HashMap::new()).await.unwrap(),
        1
    );
    svc.delete_collection(&ctx, "src_view2").await.unwrap();
    svc.delete_collection(&ctx, "src_view").await.unwrap();
    svc.delete_collection(&ctx, "src").await.unwrap();
    db.close().await;
}

#[tokio::test]
async fn long_derived_names_are_fitted_to_the_identifier_limit() {
    let db = test_db().await;
    let svc = service(&db).await;
    let ctx = admin();
    let max = if is_pg(&db) { 63 } else { 1000 };
    let coll = svc
        .create_collection(
            &ctx,
            Collection {
                id: "c".repeat(64),
                ..Collection::base("longnames")
                    .with_field(Field::new("code", text()).unique())
                    .with_field(Field::new("body", text()))
            },
        )
        .await
        .unwrap();
    let fts_name = format!("fts_{}", "x".repeat(59));
    assert_eq!(fts_name.len(), 63);
    svc.create_index(
        &ctx,
        IndexDefinition {
            name: fts_name.clone(),
            kind: IndexKind::Fulltext,
            ..IndexDefinition::btree("longnames", &["code", "body"])
        },
    )
    .await
    .unwrap();
    svc.create_record(&ctx, "longnames", json!({"code": "a1", "body": "Crème brûlée recipe"}))
        .await
        .unwrap();
    assert!(
        svc.create_record(&ctx, "longnames", json!({"code": "a1"})).await.is_err(),
        "the auto unique index works under its fitted name"
    );
    let infos = svc.list_indexes(&ctx, Some("longnames")).await.unwrap();
    let auto = infos
        .iter()
        .find(|i| i.name.starts_with("auto_") && i.unique)
        .unwrap_or_else(|| panic!("{infos:?}"));
    assert!(auto.name.len() <= max, "{}", auto.name);
    if is_pg(&db) {
        assert_eq!(auto.name.len(), 63, "hashed, not cut short: {}", auto.name);
        assert!(infos.iter().all(|i| !i.name.starts_with("_forgebase_")), "{infos:?}");
    }
    let fts = infos.iter().find(|i| i.name == fts_name).unwrap();
    assert_eq!(fts.kind, IndexKind::Fulltext);
    assert!(fts.stats.size_bytes.unwrap_or_default() > 0, "{fts:?}");
    // Full text ignores case and accents on both engines.
    for (q, n) in [("creme", 1), ("BRULEE recipe", 1), ("a1", 1), ("pudding", 0)] {
        let found = svc
            .list_records(
                &ctx,
                "longnames",
                &ListQuery {
                    search: Some(q.into()),
                    ..ListQuery::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(found.total_items, Some(n), "{q}");
    }
    svc.drop_index(&ctx, &fts_name).await.unwrap();
    let err = svc
        .list_records(
            &ctx,
            "longnames",
            &ListQuery {
                search: Some("creme".into()),
                ..ListQuery::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "no_fulltext_index");
    // Dropping the unique flag drops the fitted automatic index.
    let mut def = coll.clone();
    def.fields[0].unique = false;
    svc.update_collection(&ctx, "longnames", def).await.unwrap();
    svc.create_record(&ctx, "longnames", json!({"code": "a1"})).await.unwrap();
    db.close().await;
}

#[tokio::test]
async fn names_postgres_refuses_are_refused_for_new_definitions() {
    let db = test_db().await;
    let svc = service(&db).await;
    let ctx = admin();
    let err = svc
        .create_collection(&ctx, Collection::base("pg_stats").with_field(Field::new("x", text())))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "reserved_name", "{err:?}");
    let err = svc
        .create_collection(&ctx, Collection::base("notes").with_field(Field::new("xmin", text())))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "reserved_name", "{err:?}");
    let notes = svc
        .create_collection(&ctx, Collection::base("notes").with_field(Field::new("body", text())))
        .await
        .unwrap();
    let mut def = notes.clone();
    def.fields[0].name = "ctid".into();
    let err = svc.update_collection(&ctx, "notes", def).await.unwrap_err();
    assert_eq!(err.code(), "reserved_name", "{err:?}");
    let err = svc
        .create_index(
            &ctx,
            IndexDefinition {
                name: "pg_body".into(),
                ..IndexDefinition::btree("notes", &["body"])
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "invalid_index_name", "{err:?}");
    db.close().await;
}

#[tokio::test]
async fn index_manager_reports_kinds_statistics_and_plans() {
    let db = test_db().await;
    let svc = service(&db).await;
    let ctx = admin();
    svc.create_collection(
        &ctx,
        Collection::base("events")
            .with_field(Field::new("code", text()))
            .with_field(Field::new("n", int()))
            .with_field(Field::new("other", int()))
            .with_field(Field::new("meta", json_kind())),
    )
    .await
    .unwrap();
    let batch: Vec<serde_json::Value> = (0..1000)
        .map(|i| json!({"code": format!("c{i}"), "n": i, "other": i % 7, "meta": {"brand": format!("b{i}")}}))
        .collect();
    svc.create_records(&ctx, "events", batch).await.unwrap();
    let defs = [
        IndexDefinition {
            name: "idx_events_code".into(),
            ..IndexDefinition::btree("events", &["code"])
        },
        IndexDefinition {
            name: "idx_events_n".into(),
            unique: true,
            ..IndexDefinition::btree("events", &["n"])
        },
        IndexDefinition {
            name: "idx_events_low".into(),
            partial: Some("n < 100".into()),
            ..IndexDefinition::btree("events", &["code:lower"])
        },
        IndexDefinition {
            name: "idx_events_brand".into(),
            ..IndexDefinition::btree("events", &["meta.brand"])
        },
    ];
    for def in defs {
        svc.create_index(&ctx, def).await.unwrap();
    }
    svc.analyze(&ctx, Some("events")).await.unwrap();

    let code = svc.get_index(&ctx, "idx_events_code").await.unwrap();
    assert_eq!(code.columns, vec!["code"]);
    assert_eq!(code.table, "events");
    assert_eq!(code.stats.rows_estimate, Some(1000), "{code:?}");
    assert!(code.stats.size_bytes.unwrap_or_default() > 0, "{code:?}");
    let n = svc.get_index(&ctx, "idx_events_n").await.unwrap();
    assert!(n.unique && !n.partial);
    let low = svc.get_index(&ctx, "idx_events_low").await.unwrap();
    assert!(low.partial && !low.unique);
    assert_eq!(low.columns, vec!["<expr>"]);
    let brand = svc.get_index(&ctx, "idx_events_brand").await.unwrap();
    assert_eq!(brand.columns, vec!["<expr>"]);
    let all = svc.list_indexes(&ctx, Some("events")).await.unwrap();
    assert!(
        all.iter().any(|i| i.origin == forgebase_data::indexes::IndexOrigin::Engine),
        "the primary key index is listed: {all:?}"
    );

    // Plans: equality on an indexed column, a JSON path, and a full scan.
    for (filter, index) in [
        ("code = 'c7'", Some("idx_events_code")),
        ("meta.brand = 'b42'", Some("idx_events_brand")),
        ("other = 3", None),
    ] {
        let plan = svc
            .explain_list(
                &ctx,
                "events",
                &ListQuery {
                    filter: Some(filter.into()),
                    ..ListQuery::default()
                },
            )
            .await
            .unwrap();
        match index {
            Some(i) => assert!(plan.indexes_used.iter().any(|u| u == i), "{filter}: {plan:?}"),
            None => assert_eq!(plan.full_scans, vec!["t".to_string()], "{filter}: {plan:?}"),
        }
    }
    // A slow listing that scans the whole table gets a recommendation.
    svc.set_slow_query_threshold(&ctx, std::time::Duration::ZERO).unwrap();
    svc.list_records(
        &ctx,
        "events",
        &ListQuery {
            filter: Some("other = 3".into()),
            ..ListQuery::default()
        },
    )
    .await
    .unwrap();
    let recs = svc.recommend_indexes(&ctx).await.unwrap();
    assert!(
        recs.iter()
            .any(|r| r.definition.collection == "events" && r.definition.keys[0].path == "other"),
        "{recs:?}"
    );

    let rebuilt = svc.rebuild_index(&ctx, "idx_events_code").await.unwrap();
    assert_eq!(rebuilt.stats.rows_estimate, Some(1000));
    svc.drop_index(&ctx, "idx_events_low").await.unwrap();
    assert!(svc.get_index(&ctx, "idx_events_low").await.is_err());
    // A unique index over duplicates is refused and leaves nothing behind.
    let err = svc
        .create_index(
            &ctx,
            IndexDefinition {
                name: "idx_events_other".into(),
                unique: true,
                ..IndexDefinition::btree("events", &["other"])
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "duplicate_values", "{err:?}");
    assert!(svc.get_index(&ctx, "idx_events_other").await.is_err());
    db.close().await;
}

#[tokio::test]
async fn relation_changes_add_change_and_drop_foreign_keys() {
    let db = test_db().await;
    let svc = service(&db).await;
    let ctx = admin();
    let authors = svc
        .create_collection(&ctx, Collection::base("authors").with_field(Field::new("name", text())))
        .await
        .unwrap();
    let a = svc.create_record(&ctx, "authors", json!({"name": "ann"})).await.unwrap();
    let b = svc.create_record(&ctx, "authors", json!({"name": "bob"})).await.unwrap();
    let posts = svc
        .create_collection(&ctx, Collection::base("posts").with_field(Field::new("author", text())))
        .await
        .unwrap();
    for author in [&a, &b] {
        svc.create_record(&ctx, "posts", json!({"author": author["id"]}))
            .await
            .unwrap();
    }
    let relation = |cascade_delete| FieldKind::Relation {
        collection_id: authors.id.clone(),
        max_select: 1,
        cascade_delete,
    };
    let count = |svc: forgebase_data::DataService| async move {
        svc.count_records(&admin(), "posts", None, HashMap::new()).await.unwrap()
    };

    // Text -> relation with cascade: the key is added over existing rows.
    let mut def = posts.clone();
    def.fields[0].kind = relation(true);
    let v2 = svc.update_collection(&ctx, "posts", def).await.unwrap();
    svc.delete_record(&ctx, "authors", a["id"].as_str().unwrap(), None).await.unwrap();
    assert_eq!(count(svc.clone()).await, 1, "cascade");
    let n = svc
        .count_records(&ctx, "posts", Some("author.name = 'bob'"), HashMap::new())
        .await
        .unwrap();
    assert_eq!(n, 1);

    // Cascade -> set null: the key's action changes.
    let mut def = v2.clone();
    def.fields[0].kind = relation(false);
    let v3 = svc.update_collection(&ctx, "posts", def).await.unwrap();
    svc.delete_record(&ctx, "authors", b["id"].as_str().unwrap(), None).await.unwrap();
    assert_eq!(count(svc.clone()).await, 1, "set null keeps the post");
    let orphans = svc
        .count_records(&ctx, "posts", Some("author = null"), HashMap::new())
        .await
        .unwrap();
    assert_eq!(orphans, 1);

    // Relation -> text: the key is gone, any value fits.
    let mut def = v3.clone();
    def.fields[0].kind = text();
    let v4 = svc.update_collection(&ctx, "posts", def).await.unwrap();
    svc.create_record(&ctx, "posts", json!({"author": "nobody"})).await.unwrap();

    // Text -> relation over a value with no target: refused, nothing changes.
    let mut def = v4.clone();
    def.fields[0].kind = relation(false);
    let err = svc.update_collection(&ctx, "posts", def).await.unwrap_err();
    assert_eq!(err.code(), "foreign_key_violation", "{err:?}");
    assert_eq!(svc.get_collection(&ctx, "posts").await.unwrap().version, 4);
    assert_eq!(count(svc.clone()).await, 2);
    assert_eq!(db.foreign_key_violations().await.unwrap(), 0);
    db.close().await;
}
