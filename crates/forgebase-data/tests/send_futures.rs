//! Every `DataService` future must be `Send`, so the HTTP layer can call it
//! from handlers running on a multi-threaded runtime. (FB-11 found
//! `aggregate` was not: its query compiler held a non-`Sync` `&dyn Catalog`
//! across an await.)
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::HashMap;

use common::{admin, service, test_db};
use forgebase_data::db::Database;
use forgebase_data::{AggregateQuery, ListQuery, SqlStatement};

fn assert_send<T: Send>(_: &T) {}

#[tokio::test]
async fn service_futures_are_send() {
    let test = test_db().await;
    let db: &Database = &test;
    let svc = service(db).await;
    let ctx = admin();
    let q = ListQuery::default();
    let agg = AggregateQuery::default();

    assert_send(&svc.list_collections(&ctx));
    assert_send(&svc.list_records(&ctx, "c", &q));
    assert_send(&svc.count_records(&ctx, "c", None, HashMap::new()));
    assert_send(&svc.aggregate(&ctx, "c", &agg));
    assert_send(&svc.explain_list(&ctx, "c", &q));
    assert_send(&svc.recommend_indexes(&ctx));
    assert_send(&svc.execute_sql(&ctx, SqlStatement::default()));
    assert_send(&svc.execute_sql_batch(&ctx, vec![]));
    assert_send(&svc.explain_sql(&ctx, SqlStatement::default()));
    assert_send(&svc.list_indexes(&ctx, None));
    assert_send(&svc.record_versions(&ctx, "c", "id"));
    assert_send(&svc.schema_history(&ctx, None));
}
