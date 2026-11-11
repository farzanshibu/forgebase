//! FB-171: after-hooks on a sharded cluster (ADR-0029 §3.2, §3.5): every
//! set has its own outbox and its own `after_hooks` cursor (the per-set
//! table `_forgebase_function_cursors`), so hooks run for changes on every
//! set, once. Real, separate PostgreSQL clusters; skipped without an
//! engine (one set: `hooks.rs` on both backends).
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use common::{admin, constant, req};
use forgebase_auth::crypto::Keyring;
use forgebase_data::db::shard_testing::Sharded;
use forgebase_data::db::shards::{self, ShardSetup};
use forgebase_data::db::topology::{PlacementState, set_number_of};
use forgebase_data::db::{Backend, Database, Statement, Value};
use forgebase_data::{AccessContext, Authorizer, DataService};
use forgebase_functions::{FunctionService, FunctionsConfig, HookEvent, HookSpec};
use forgebase_policy::PolicyService;
use forgebase_realtime::{RealtimeConfig, RealtimeService};
use serde_json::json;

async fn cursor(db: &Database) -> Option<i64> {
    db.fetch_all(&Statement::new(
        "SELECT seq FROM _forgebase_function_cursors WHERE name = 'after_hooks'",
        vec![],
    ))
    .await
    .unwrap()
    .rows
    .first()
    .and_then(|r| r.first())
    .and_then(Value::as_i64)
}

#[tokio::test]
async fn after_hooks_follow_every_sets_outbox_with_its_own_cursor() {
    let Some(mut s) = Sharded::start().await else {
        eprintln!("skipped: no PostgreSQL engine configured");
        return;
    };
    let backend: Arc<dyn Backend> = Arc::new(s.control.clone());
    let policy = PolicyService::open(backend.clone()).await.unwrap();
    let authorizer: Arc<dyn Authorizer> = policy.authorizer();
    let data = DataService::new(backend, authorizer).await.unwrap();
    policy.attach_data(data.clone());
    let c = serde_json::from_value(json!({"name": "posts", "options": {"tenant_scoped": true},
        "fields": [{"name": "title", "type": "text"}]}))
    .unwrap();
    data.create_collection(&admin(), c).await.unwrap();
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
    s.router.place("t1", &set.id, PlacementState::Active).await.unwrap();
    let rt = RealtimeService::start(data.clone(), None, RealtimeConfig::default(), None)
        .await
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let keyring = Keyring::load_or_create(&dir.path().join("master.key")).unwrap();
    let svc = FunctionService::open(data.clone(), None, keyring, FunctionsConfig::default())
        .await
        .unwrap();
    // Opening starts every set's cursor at its head.
    assert_eq!(cursor(&s.control).await, Some(0));
    assert_eq!(cursor(&shard).await, Some(0));

    let mut r = req("noop", &constant("{}"), &[]);
    r.hooks = vec![HookSpec {
        collection: "posts".into(),
        events: vec![HookEvent::AfterCreate],
    }];
    svc.deploy(&admin(), r).await.unwrap();
    for t in ["t1", "t0"] {
        data.create_record(
            &AccessContext::admin("root").with_tenant(t),
            "posts",
            json!({"title": t}),
        )
        .await
        .unwrap();
    }
    assert_eq!(
        svc.run_after_hooks().await.unwrap(),
        2,
        "one event on s1, one on control"
    );
    assert_eq!(svc.run_after_hooks().await.unwrap(), 0, "each set's cursor persisted");
    let on_shard = cursor(&shard).await.unwrap();
    assert_eq!(set_number_of(on_shard), set.number, "s1's cursor is in s1's range");
    assert_eq!(set_number_of(cursor(&s.control).await.unwrap()), 0);
    svc.shutdown();
    rt.shutdown();
}
