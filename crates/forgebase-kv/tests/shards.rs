//! KV persistence over database sets (FB-172, ADR-0029 §3.5 "KV: memory as
//! today; the flusher groups dirty keys by set; start-up loads every set").
//!
//! One set (SQLite and PostgreSQL): `open_routed` over a one-set router is
//! `open`. Several sets (PostgreSQL clusters from
//! `forgebase_db::shard_testing`, skipped without an engine): keys are
//! written on their tenant's set, a restart loads every set and takes a
//! tenant's keys only from its owner set, a frozen tenant's keys wait, and
//! `flush_tenant` writes them during the freeze.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use forgebase_data::{AccessContext, DefaultAuthorizer};
use forgebase_db::shard_testing::Sharded;
use forgebase_db::topology::PlacementState;
use forgebase_db::{Backend, Database, DbRouter, RouterOptions, Statement, TxMode, Value, shards};
use forgebase_kv::{KvConfig, KvService, Reply};

fn cmd(s: &str) -> Vec<Vec<u8>> {
    s.split_whitespace().map(|p| p.as_bytes().to_vec()).collect()
}

fn tenant(t: &str) -> AccessContext {
    AccessContext::admin("root").with_tenant(t)
}

async fn count(db: &Database, sql: &str) -> i64 {
    db.fetch_all(&Statement::raw(sql))
        .await
        .unwrap()
        .rows
        .first()
        .and_then(|r| r.first())
        .and_then(Value::as_i64)
        .unwrap_or(-1)
}

async fn exec(db: &Database, sql: &str) {
    let mut tx = db.begin(TxMode::Normal).await.unwrap();
    tx.execute(&Statement::raw(sql)).await.unwrap();
    tx.commit().await.unwrap();
}

/// Rows of environment `t` (stored as `t:<env>`); `""`: the instance-wide `default`.
fn rows_of(t: &str) -> String {
    let stored = if t.is_empty() { "default".to_owned() } else { format!("t:{t}") };
    format!("SELECT count(*) FROM _forgebase_kv_entries WHERE tenant = '{stored}'")
}

#[tokio::test]
async fn one_set_router_persists_as_before() {
    let test = common::test_db().await;
    let db = test.database().clone();
    let open = || KvService::open_routed(KvConfig::default(), Arc::new(DefaultAuthorizer), DbRouter::single(db.clone()));
    let kv = open().await.unwrap();
    kv.exec(&tenant("acme"), "app", &cmd("SET a 1")).unwrap();
    kv.exec(&AccessContext::admin("root"), "app", &cmd("SET b 2")).unwrap();
    assert_eq!(kv.flush().await.unwrap(), 2);
    assert_eq!(kv.flush().await.unwrap(), 0);
    let again = open().await.unwrap();
    assert_eq!(again.exec(&tenant("acme"), "app", &cmd("GET a")).unwrap(), Reply::bulk("1"));
    assert_eq!(
        again.exec(&AccessContext::admin("root"), "app", &cmd("GET b")).unwrap(),
        Reply::bulk("2")
    );
    // flush_tenant writes only that tenant's keys.
    again.exec(&tenant("acme"), "app", &cmd("SET c 3")).unwrap();
    again.exec(&AccessContext::admin("root"), "app", &cmd("SET d 4")).unwrap();
    assert_eq!(again.flush_tenant("acme").await.unwrap(), 1);
    assert_eq!(again.flush().await.unwrap(), 1);
}

#[tokio::test]
async fn keys_are_written_on_their_tenant_set_and_loaded_from_every_set() {
    let Some(mut sh) = Sharded::start().await else {
        eprintln!("skipped: no PostgreSQL engine");
        return;
    };
    let (set, s1) = sh.provision("s1").await;
    shards::activate_shard(
        &sh.router,
        &set,
        &s1,
        shards::ShardSetup {
            backups_dir: None,
            replay: None,
            control_conninfo: None,
        },
    )
    .await
    .unwrap();
    let router = DbRouter::open(
        sh.control.clone(),
        RouterOptions {
            connector: Some(sh.connector.clone()),
            freeze_timeout: Duration::from_millis(200),
            ..RouterOptions::default()
        },
    )
    .await
    .unwrap();
    let control = sh.control.clone();
    router.place("tb", &set.id, PlacementState::Active).await.unwrap();
    let open = || KvService::open_routed(KvConfig::default(), Arc::new(DefaultAuthorizer), router.clone());
    let kv = open().await.unwrap();
    kv.exec(&tenant("ta"), "app", &cmd("SET k a")).unwrap();
    kv.exec(&tenant("tb"), "app", &cmd("SET k b")).unwrap();
    kv.exec(&AccessContext::admin("root"), "app", &cmd("SET k i")).unwrap();
    assert_eq!(kv.flush().await.unwrap(), 3);
    assert_eq!((count(&control, &rows_of("ta")).await, count(&s1, &rows_of("ta")).await), (1, 0));
    assert_eq!((count(&control, &rows_of("tb")).await, count(&s1, &rows_of("tb")).await), (0, 1));
    assert_eq!((count(&control, &rows_of("")).await, count(&s1, &rows_of("")).await), (1, 0));

    // A delete goes to the tenant's set too.
    kv.exec(&tenant("tb"), "app", &cmd("SET gone x")).unwrap();
    kv.flush().await.unwrap();
    kv.exec(&tenant("tb"), "app", &cmd("DEL gone")).unwrap();
    kv.flush().await.unwrap();
    assert_eq!(count(&s1, &rows_of("tb")).await, 1);

    // A restart loads every set; a stale copy on a set that does not own the
    // tenant (a move's source until cleanup) is ignored.
    exec(
        &control,
        "INSERT INTO _forgebase_kv_entries (tenant, namespace, key, type, data, expires_at, updated_at) \
         VALUES ('t:tb', 'app', 'k', 'string', '{\"t\":\"Str\",\"v\":\"c3RhbGU=\"}', NULL, 1)",
    )
    .await;
    let back = open().await.unwrap();
    assert_eq!(back.exec(&tenant("ta"), "app", &cmd("GET k")).unwrap(), Reply::bulk("a"));
    assert_eq!(back.exec(&tenant("tb"), "app", &cmd("GET k")).unwrap(), Reply::bulk("b"));
    assert_eq!(
        back.exec(&AccessContext::admin("root"), "app", &cmd("GET k")).unwrap(),
        Reply::bulk("i")
    );

    // A frozen tenant's keys wait in memory; flush_tenant writes them on the
    // set its placement names (the source) during the freeze.
    router.place("tb", &set.id, PlacementState::Frozen).await.unwrap();
    back.exec(&tenant("tb"), "app", &cmd("SET k b2")).unwrap();
    back.exec(&tenant("ta"), "app", &cmd("SET k a2")).unwrap();
    assert_eq!(back.flush().await.unwrap(), 1, "ta written, tb held back");
    assert_eq!(back.stats(&tenant("tb")).unwrap().unsaved_keys, 1);
    assert_eq!(back.flush_tenant("tb").await.unwrap(), 1);
    assert_eq!(
        count(
            &s1,
            "SELECT count(*) FROM _forgebase_kv_entries WHERE tenant = 't:tb' AND key = 'k' AND data LIKE '%YjI=%'"
        )
        .await,
        1
    );

    // After the switch (tb now on control) new writes land on control.
    router.place("tb", "control", PlacementState::Cleanup).await.unwrap();
    back.exec(&tenant("tb"), "app", &cmd("SET moved yes")).unwrap();
    assert_eq!(back.flush().await.unwrap(), 1);
    assert_eq!(
        count(
            &control,
            "SELECT count(*) FROM _forgebase_kv_entries WHERE tenant = 't:tb' AND key = 'moved'"
        )
        .await,
        1
    );
    drop(sh);
}
