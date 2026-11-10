//! Malicious inputs (SPEC §27 #25, data-core part): filter-language and SQL
//! injection attempts, SQL-editor restrictions, and fail-closed access.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::HashMap;
use std::sync::Arc;

use common::{admin, is_postgres, service, service_with, skip_on_postgres, test_db};
use forgebase_data::db::testing::TestDb;
use forgebase_core::ErrorCode;
use forgebase_data::schema::{Collection, Field, FieldKind};
use forgebase_data::{
    AccessContext, Action, Authorizer, DataError, Decision, GetOptions, IndexDefinition, ListQuery, Predicate,
    SqlStatement, UpdateOptions,
};
use serde_json::{Value as Json, json};

fn text() -> FieldKind {
    FieldKind::Text {
        min_length: None,
        max_length: None,
    }
}

async fn seeded() -> (TestDb, forgebase_data::DataService) {
    let db = test_db().await;
    let svc = service(&db).await;
    let ctx = admin();
    svc.create_collection(
        &ctx,
        Collection::documents("notes")
            .with_field(Field::new("title", text()))
            .with_field(Field::new("owner", text()))
            .with_field(Field {
                hidden: true,
                ..Field::new("secret", text())
            }),
    )
    .await
    .unwrap();
    for (title, owner) in [
        ("alpha", "u1"),
        ("beta", "u1"),
        ("gamma", "u2"),
        ("100% off_sale", "u2"),
        ("it's", "u3"),
    ] {
        svc.create_record(
            &ctx,
            "notes",
            json!({"title": title, "owner": owner, "secret": format!("s-{title}"), "meta": {"k": 1}}),
        )
        .await
        .unwrap();
    }
    (db, svc)
}

async fn count(
    svc: &forgebase_data::DataService,
    filter: &str,
    params: HashMap<String, Json>,
) -> Result<u64, DataError> {
    svc.count_records(&admin(), "notes", Some(filter), params).await
}

#[tokio::test]
async fn filter_injection_attempts_are_rejected_or_inert() {
    let (db, svc) = seeded().await;

    // Syntax that would change the SQL if it were concatenated is refused.
    for f in [
        "title = 'x' OR 1=1",
        "title = 'x'; DROP TABLE notes; --",
        "title = 'x' -- ",
        "title = 'x' /* */",
        "title = \"x\" UNION SELECT * FROM _forgebase_collections",
        "title = (SELECT 1)",
        "title IN ('a')",
        "title = `x`",
        "sqlite_master.name = 'x' && 1 = (SELECT 1)",
        "title = 'a' || owner = 'u1' ;",
        "meta.k') OR ('1'='1 = 1",
        "title = @request.auth.id",
        "title = x'00'",
    ] {
        let err = count(&svc, f, HashMap::new()).await.unwrap_err();
        assert!(matches!(err, DataError::Query(_)), "{f}: {err:?}");
    }

    // Quotes inside strings are data, not syntax.
    assert_eq!(
        count(&svc, r"title = 'x\' OR \'1\'=\'1'", HashMap::new())
            .await
            .unwrap(),
        0
    );
    assert_eq!(count(&svc, r"title = 'it\'s'", HashMap::new()).await.unwrap(), 1);
    assert_eq!(
        count(&svc, r#"title = "x\" OR \"1\"=\"1""#, HashMap::new())
            .await
            .unwrap(),
        0
    );

    // Parameters are bound, whatever they contain.
    for evil in ["' OR '1'='1", "x'); DROP TABLE notes; --", "\" OR 1=1 --", "%", "_"] {
        let n = count(
            &svc,
            "title = {:t} || owner = {:t}",
            HashMap::from([("t".into(), json!(evil))]),
        )
        .await
        .unwrap();
        assert_eq!(n, 0, "{evil}");
    }
    // `~` treats % and _ literally (LIKE metacharacters are escaped).
    assert_eq!(count(&svc, "title ~ '%'", HashMap::new()).await.unwrap(), 1);
    assert_eq!(count(&svc, "title ~ '_'", HashMap::new()).await.unwrap(), 1);
    assert_eq!(count(&svc, "title ~ 'off_s'", HashMap::new()).await.unwrap(), 1);
    assert_eq!(count(&svc, "title ~ 'a'", HashMap::new()).await.unwrap(), 4);

    // Unknown, hidden and system names.
    for f in ["_data = 1", "expand.x = 1", "id.x = 1", "title.x = 1"] {
        assert!(count(&svc, f, HashMap::new()).await.is_err(), "{f}");
    }
    // Resource limits.
    let deep = format!("{}title = 'a'{}", "(".repeat(100), ")".repeat(100));
    assert!(matches!(
        count(&svc, &deep, HashMap::new()).await.unwrap_err(),
        DataError::Query(_)
    ));
    let wide = vec!["title = 'a'"; 400].join(" || ");
    assert!(count(&svc, &wide, HashMap::new()).await.is_err());

    // Sort, projection and expand inputs are validated the same way.
    for sort in [
        "title; DROP TABLE notes",
        "title DESC",
        "(SELECT 1)",
        "-_data",
        "title,,,",
    ] {
        let r = svc
            .list_records(
                &admin(),
                "notes",
                &ListQuery {
                    sort: Some(sort.into()),
                    ..ListQuery::default()
                },
            )
            .await;
        if sort == "title,,," {
            assert!(r.is_ok());
        } else {
            assert!(r.is_err(), "{sort}");
        }
    }
    assert!(
        svc.list_records(
            &admin(),
            "notes",
            &ListQuery {
                expand: vec!["owner; DROP".into()],
                ..ListQuery::default()
            }
        )
        .await
        .is_err()
    );
    let r = svc
        .list_records(
            &admin(),
            "notes",
            &ListQuery {
                fields: Some(vec!["title'); DROP TABLE notes; --".into()]),
                ..ListQuery::default()
            },
        )
        .await
        .unwrap();
    assert!(
        r.items.iter().all(|i| i.is_empty()),
        "unknown projections select nothing"
    );

    // Everything is still there.
    assert_eq!(
        svc.count_records(&admin(), "notes", None, HashMap::new())
            .await
            .unwrap(),
        5
    );
    assert!(db.integrity_check().await.unwrap().is_empty());
    db.close().await;
}

#[tokio::test]
async fn names_and_index_definitions_cannot_inject() {
    let (db, svc) = seeded().await;
    let ctx = admin();
    for name in [
        "x\"; DROP TABLE notes; --",
        "_forgebase_meta",
        "sqlite_master",
        "a b",
        "notes",
    ] {
        assert!(
            svc.create_collection(&ctx, Collection::base(name)).await.is_err(),
            "{name}"
        );
    }
    for field in ["id", "x\" TEXT); DROP TABLE notes; --", "_data", "expand"] {
        assert!(
            svc.create_collection(&ctx, Collection::base("ok").with_field(Field::new(field, text())))
                .await
                .is_err(),
            "{field}"
        );
    }
    if skip_on_postgres(&db, "the index manager on PostgreSQL is FB-144's") {
        return;
    }
    // Partial-index conditions are compiled with escaped literals.
    let ix = svc
        .create_index(
            &ctx,
            IndexDefinition {
                name: "idx_q".into(),
                partial: Some(r"title != 'x\'); DROP TABLE notes; --'".into()),
                ..IndexDefinition::btree("notes", &["title"])
            },
        )
        .await
        .unwrap();
    assert!(ix.sql.unwrap().contains("'x''); DROP TABLE notes; --'"));
    for p in ["title = {:p}", "created < @now", "(SELECT 1) = 1"] {
        let r = svc
            .create_index(
                &ctx,
                IndexDefinition {
                    name: "idx_bad".into(),
                    partial: Some(p.into()),
                    ..IndexDefinition::btree("notes", &["title"])
                },
            )
            .await;
        assert!(r.is_err(), "{p}");
    }
    for name in ["idx\"; DROP", "auto_x", "sqlite_x", "1abc"] {
        let r = svc
            .create_index(
                &ctx,
                IndexDefinition {
                    name: name.into(),
                    ..IndexDefinition::btree("notes", &["title"])
                },
            )
            .await;
        assert!(r.is_err(), "{name}");
    }
    assert_eq!(svc.count_records(&ctx, "notes", None, HashMap::new()).await.unwrap(), 5);
    db.close().await;
}

#[tokio::test]
async fn sql_editor_restrictions() {
    let (db, svc) = seeded().await;
    let ctx = admin();
    let run = |s: &str| SqlStatement {
        sql: s.into(),
        params: vec![],
    };

    let mut bads = vec![
        "SELECT 1; DROP TABLE notes",
        "ATTACH DATABASE 'x.db' AS x",
        "PRAGMA writable_schema = ON",
        "PRAGMA foreign_keys = OFF",
        "PRAGMA database_list",
        "BEGIN; DELETE FROM notes; COMMIT",
        "VACUUM INTO 'copy.db'",
        "CREATE TEMP TABLE t (x)",
        "DELETE FROM _forgebase_collections",
        "UPDATE \"_forgebase_meta\" SET value = 'x'",
        "INSERT INTO _forgebase_schema_history (action, statements, actor, applied_at) VALUES ('sql', '[]', 'x', 'x')",
        "DROP TABLE _forgebase_indexes",
    ];
    if is_postgres(&db) {
        // ADR-0028 §7.5: PostgreSQL's deny list (the editor's role is the
        // boundary, FB-153).
        bads.extend([
            "SELECT pg_read_file('/etc/passwd')",
            "COPY notes TO '/tmp/notes.csv'",
            "COPY notes FROM PROGRAM 'id'",
            "SET ROLE postgres",
            "SET search_path = public",
            "RESET ROLE",
            "ALTER SYSTEM SET work_mem = '1GB'",
            "LOAD 'auto_explain'",
            "CREATE EXTENSION dblink",
            "CREATE FUNCTION f() RETURNS int LANGUAGE c AS 'lib', 'f'",
            "CREATE FUNCTION f() RETURNS text LANGUAGE plpgsql AS $$ BEGIN RETURN pg_read_file('x'); END $$",
            "LISTEN x",
            "CHECKPOINT",
            "SELECT pg_terminate_backend(pg_backend_pid())",
            "SELECT set_config('search_path', 'public', false)",
            "SELECT pg_advisory_lock(1)",
            "DELETE FROM forgebase._forgebase_meta",
            "UPDATE pg_catalog.pg_class SET relname = 'x' WHERE false",
            "EXPLAIN ANALYZE DELETE FROM _forgebase_meta",
            "SELECT 1 INTO _forgebase_x",
            "DO $$ BEGIN DELETE FROM _forgebase_meta; END $$",
        ]);
    } else {
        bads.push("SELECT load_extension('evil')");
    }
    for bad in bads {
        let err = svc.execute_sql(&ctx, run(bad)).await.unwrap_err();
        assert!(
            matches!(
                err,
                DataError::Query(_) | DataError::Forbidden(_) | DataError::Invalid { .. }
            ),
            "{bad}: {err:?}"
        );
    }
    // Administrators may read system tables, not write them.
    let r = svc
        .execute_sql(&ctx, run("SELECT count(*) FROM _forgebase_collections"))
        .await
        .unwrap();
    assert_eq!(r.rows[0][0], json!(1));

    // A write disguised as a read is caught by the engine and needs write rights.
    let r = svc
        .execute_sql(
            &ctx,
            run("WITH gone AS (SELECT 1) DELETE FROM notes WHERE title = 'alpha'"),
        )
        .await
        .unwrap();
    assert_eq!(r.kind, forgebase_data::query::StatementKind::Write);
    assert_eq!(r.rows_affected, 1);

    // Raw DDL that would break a collection is rolled back.
    for ddl in [
        "DROP TABLE notes",
        "ALTER TABLE notes RENAME TO other",
        "ALTER TABLE notes DROP COLUMN owner",
        "ALTER TABLE notes RENAME COLUMN title TO t2",
    ] {
        let err = svc.execute_sql(&ctx, run(ddl)).await.unwrap_err();
        assert_eq!(err.code(), "schema_conflict", "{ddl}: {err:?}");
    }
    assert_eq!(
        svc.count_records(&ctx, "notes", Some("title != null"), HashMap::new())
            .await
            .unwrap(),
        4
    );

    // Parameters are bound in raw SQL too.
    let r = svc
        .execute_sql(
            &ctx,
            SqlStatement {
                sql: "SELECT count(*) FROM notes WHERE title = ?1".into(),
                params: vec![json!("x' OR '1'='1")],
            },
        )
        .await
        .unwrap();
    assert_eq!(r.rows[0][0], json!(0));

    // Non-administrators get nothing (fail closed).
    let user = AccessContext::user("users", "u1");
    assert!(matches!(
        svc.execute_sql(&user, run("SELECT 1")).await.unwrap_err(),
        DataError::Forbidden(_)
    ));
    assert!(matches!(
        svc.execute_sql(&AccessContext::anonymous(), run("SELECT 1"))
            .await
            .unwrap_err(),
        DataError::Unauthenticated
    ));
    db.close().await;
}

/// A policy like FB-21's: users see and change only their own notes.
#[derive(Debug)]
struct OwnerOnly;

impl Authorizer for OwnerOnly {
    fn authorize(&self, ctx: &AccessContext, action: Action, _c: Option<&Collection>) -> Decision {
        match &ctx.principal {
            forgebase_data::Principal::Admin { .. } => Decision::Allow,
            forgebase_data::Principal::User { id, .. } => match action {
                Action::List | Action::View | Action::Create | Action::Update | Action::Delete => Decision::Filter(
                    Predicate::parse("owner = {:me}", HashMap::from([("me".to_string(), json!(id))])).unwrap(),
                ),
                _ => Decision::Deny,
            },
            _ => Decision::Deny,
        }
    }
}

#[tokio::test]
async fn access_context_predicates_are_enforced_in_sql() {
    let (db, _) = seeded().await;
    let svc = service_with(&db, Arc::new(OwnerOnly), None).await;
    let u1 = AccessContext::user("users", "u1");

    let mine = svc.list_records(&u1, "notes", &ListQuery::default()).await.unwrap();
    assert_eq!(mine.total_items, Some(2));
    assert!(mine.items.iter().all(|r| r["owner"] == json!("u1")));
    assert!(
        mine.items.iter().all(|r| r.get("secret").is_none()),
        "hidden fields stay hidden"
    );
    // An `||` in the user's filter cannot widen the predicate.
    let widened = svc
        .list_records(
            &u1,
            "notes",
            &ListQuery {
                filter: Some("owner = 'u2' || owner != 'nobody'".into()),
                ..ListQuery::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(widened.total_items, Some(2));
    // Hidden fields cannot be probed through filters either.
    assert!(
        svc.list_records(
            &u1,
            "notes",
            &ListQuery {
                filter: Some("secret = 's-gamma'".into()),
                ..ListQuery::default()
            }
        )
        .await
        .is_err()
    );

    let all = svc
        .list_records(
            &admin(),
            "notes",
            &ListQuery {
                per_page: Some(10),
                ..ListQuery::default()
            },
        )
        .await
        .unwrap();
    let theirs = all.items.iter().find(|r| r["owner"] == json!("u2")).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let err = svc
        .get_record(&u1, "notes", &theirs, &GetOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(err, DataError::RecordNotFound { .. }));
    let err = svc
        .update_record(
            &u1,
            "notes",
            &theirs,
            json!({"title": "pwned"}),
            UpdateOptions::default(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DataError::RecordNotFound { .. }));
    let err = svc.delete_record(&u1, "notes", &theirs, None).await.unwrap_err();
    assert!(matches!(err, DataError::RecordNotFound { .. }));
    // Creating or moving a record outside one's scope is refused before commit.
    let err = svc
        .create_record(&u1, "notes", json!({"title": "x", "owner": "u2"}))
        .await
        .unwrap_err();
    assert!(matches!(err, DataError::Forbidden(_)), "{err:?}");
    let own = svc
        .create_record(&u1, "notes", json!({"title": "x", "owner": "u1"}))
        .await
        .unwrap();
    let err = svc
        .update_record(
            &u1,
            "notes",
            own["id"].as_str().unwrap(),
            json!({"owner": "u2"}),
            UpdateOptions::default(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DataError::Forbidden(_)), "{err:?}");
    assert_eq!(
        svc.count_records(&admin(), "notes", Some("owner = 'u2'"), HashMap::new())
            .await
            .unwrap(),
        2
    );
    // Schema actions are denied.
    assert!(matches!(
        svc.create_collection(&u1, Collection::base("x")).await.unwrap_err(),
        DataError::Forbidden(_)
    ));
    assert!(matches!(
        svc.list_collections(&AccessContext::anonymous()).await.unwrap_err(),
        DataError::Unauthenticated
    ));
    db.close().await;
}
