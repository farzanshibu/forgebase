//! KV data survives a restart: flush to SQLite, reopen, read back.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use forgebase_data::{AccessContext, DefaultAuthorizer};
use forgebase_kv::{KvConfig, KvError, KvService, Reply};

fn cmd(s: &str) -> Vec<Vec<u8>> {
    s.split_whitespace().map(|p| p.as_bytes().to_vec()).collect()
}

#[tokio::test]
async fn data_ttl_and_tenants_survive_restart_and_expired_keys_do_not() {
    let test = common::test_db().await;
    let db = test.database().clone();
    let clock = Arc::new(std::sync::atomic::AtomicU64::new(1_000_000));
    let mk = |db: forgebase_db::Database, c: Arc<std::sync::atomic::AtomicU64>| async move {
        KvService::open_with_clock(
            KvConfig::default(),
            Arc::new(DefaultAuthorizer),
            Some(Arc::new(db)),
            Arc::new(move || c.load(std::sync::atomic::Ordering::SeqCst)),
        )
        .await
        .unwrap()
    };
    let kv = mk(db.clone(), clock.clone()).await;
    let mut a = AccessContext::admin("root");
    a.tenant = Some("acme".into());
    let d = AccessContext::admin("root");
    for c in [
        "SET s hello",
        "HSET h f v",
        "RPUSH l a b c",
        "SADD st x y",
        "ZADD z 1.5 m +inf top",
        "XADD x 5-1 f v",
        "SET short v PX 500",
        "SET long v EX 100",
    ] {
        kv.exec(&a, "app", &cmd(c)).unwrap();
    }
    kv.exec(&d, "app", &cmd("SET s default-tenant")).unwrap();
    kv.exec(&a, "app", &cmd("DEL h")).unwrap();
    assert_eq!(
        kv.flush().await.unwrap(),
        9,
        "9 keys changed (h was created then deleted: a delete row)"
    );
    assert_eq!(kv.flush().await.unwrap(), 0, "nothing left unsaved");

    // restart 1 s later: `short` has expired
    clock.fetch_add(1_000, std::sync::atomic::Ordering::SeqCst);
    let kv2 = mk(db.clone(), clock.clone()).await;
    let get = |c: &str| kv2.exec(&a, "app", &cmd(c)).unwrap();
    assert_eq!(get("GET s"), Reply::bulk("hello"));
    assert_eq!(
        kv2.exec(&d, "app", &cmd("GET s")).unwrap(),
        Reply::bulk("default-tenant")
    );
    assert_eq!(get("EXISTS h"), Reply::Int(0), "deleted stays deleted");
    assert_eq!(
        get("LRANGE l 0 -1"),
        Reply::Array(vec![Reply::bulk("a"), Reply::bulk("b"), Reply::bulk("c")])
    );
    assert_eq!(get("SCARD st"), Reply::Int(2));
    assert_eq!(get("ZSCORE z top"), Reply::bulk("inf"));
    assert_eq!(get("XLEN x"), Reply::Int(1));
    assert_eq!(get("EXISTS short"), Reply::Int(0), "expired while down");
    assert_eq!(get("PTTL long"), Reply::Int(99_000));
    // the stream keeps its last id across the restart
    assert!(kv2.exec(&a, "app", &cmd("XADD x 5-1 f v")).is_err());
    // expired key's row is removed from SQLite on the next flush
    kv2.flush().await.unwrap();
    let rows = forgebase_db::Backend::fetch_all(
        &db,
        &forgebase_db::Statement::new("SELECT count(*) FROM _forgebase_kv_entries WHERE key = 'short'", vec![]),
    )
    .await
    .unwrap();
    assert_eq!(rows.scalar().and_then(forgebase_db::Value::as_i64), Some(0));
}

#[tokio::test]
async fn shutdown_flushes_unsaved_writes() {
    let test = common::test_db().await;
    let db = test.database().clone();
    let kv = KvService::open(
        KvConfig::default(),
        Arc::new(DefaultAuthorizer),
        Some(Arc::new(db.clone())),
    )
    .await
    .unwrap();
    kv.start_background();
    kv.exec(&AccessContext::admin("r"), "n", &cmd("SET k v")).unwrap();
    assert_eq!(kv.shutdown().await.unwrap(), 1);
    let kv2 = KvService::open(KvConfig::default(), Arc::new(DefaultAuthorizer), Some(Arc::new(db)))
        .await
        .unwrap();
    assert_eq!(
        kv2.exec(&AccessContext::admin("r"), "n", &cmd("GET k")).unwrap(),
        Reply::bulk("v")
    );
}

/// Shutdown aborts the background flusher, which may be in the middle of a
/// write. A flush dropped before it finished must hand its keys back, or the
/// final flush finds nothing to write and recent keys are lost on restart.
#[tokio::test]
async fn a_flush_cancelled_mid_write_keeps_its_keys_for_the_next_flush() {
    use std::future::Future;
    use std::task::{Context, Poll, Waker};

    let test = common::test_db().await;
    let db = test.database().clone();
    let kv = KvService::open(KvConfig::default(), Arc::new(DefaultAuthorizer), Some(Arc::new(db)))
        .await
        .unwrap();
    let a = AccessContext::admin("root");
    kv.exec(&a, "app", &cmd("SET greeting hello")).unwrap();

    // Poll one flush until its first suspension (the SQLite write), then drop it.
    {
        let mut fut = Box::pin(kv.flush());
        let mut cx = Context::from_waker(Waker::noop());
        assert!(
            matches!(fut.as_mut().poll(&mut cx), Poll::Pending),
            "the write needs I/O, so the first poll suspends"
        );
    }
    assert_eq!(
        kv.stats(&a).unwrap().unsaved_keys,
        1,
        "the cancelled flush returned its key"
    );
    assert_eq!(kv.shutdown().await.unwrap(), 1, "the final flush writes it");
}

/// A key with U+0000 is refused on every backend: PostgreSQL `text` cannot
/// store it, and a key the flusher cannot write used to block every save.
#[tokio::test]
async fn a_key_with_a_nul_character_is_refused() {
    let kv = KvService::open(KvConfig::default(), Arc::new(DefaultAuthorizer), None)
        .await
        .unwrap();
    let a = AccessContext::admin("root");
    let set = vec![b"SET".to_vec(), b"bad\0key".to_vec(), b"v".to_vec()];
    let err = kv.exec(&a, "app", &set).unwrap_err();
    assert!(matches!(err, KvError::InvalidKey(_)), "{err}");
    let mset = vec![
        b"MSET".to_vec(),
        b"x".to_vec(),
        b"1".to_vec(),
        b"y\0".to_vec(),
        b"2".to_vec(),
    ];
    let err = kv.exec(&a, "app", &mset).unwrap_err();
    assert!(matches!(err, KvError::InvalidKey(_)), "{err}");
    assert_eq!(
        kv.exec(&a, "app", &cmd("GET x")).unwrap(),
        Reply::Nil,
        "MSET is all or nothing"
    );
}

/// One row the database refuses does not hold back the other keys of its
/// set: they are saved, and only the refused key stays unsaved.
#[tokio::test]
async fn a_key_the_database_refuses_does_not_block_the_others() {
    use forgebase_db::{Backend, BackendKind, Statement, TxMode};

    let test = common::test_db().await;
    let db = test.database().clone();
    let poison = match db.kind() {
        BackendKind::Sqlite => {
            "CREATE TRIGGER kv_poison BEFORE INSERT ON _forgebase_kv_entries \
             WHEN NEW.key = 'poison' BEGIN SELECT RAISE(ABORT, 'refused'); END"
        }
        BackendKind::Postgres => {
            "CREATE FUNCTION kv_poison() RETURNS trigger LANGUAGE plpgsql AS $$ \
             BEGIN IF NEW.key = 'poison' THEN RAISE EXCEPTION 'refused'; END IF; RETURN NEW; END $$; \
             CREATE TRIGGER kv_poison BEFORE INSERT ON _forgebase_kv_entries \
             FOR EACH ROW EXECUTE FUNCTION kv_poison();"
        }
    };
    let mut tx = db.begin(TxMode::SchemaChange).await.unwrap();
    tx.execute(&Statement::raw(poison)).await.unwrap();
    tx.commit().await.unwrap();

    let kv = KvService::open(
        KvConfig::default(),
        Arc::new(DefaultAuthorizer),
        Some(Arc::new(db.clone())),
    )
    .await
    .unwrap();
    let a = AccessContext::admin("root");
    for c in ["SET good1 a", "SET poison b", "SET good2 c"] {
        kv.exec(&a, "app", &cmd(c)).unwrap();
    }
    assert_eq!(kv.flush().await.unwrap(), 2, "the two good keys are saved");
    assert_eq!(kv.stats(&a).unwrap().unsaved_keys, 1, "the refused key stays unsaved");
    assert!(kv.flush().await.is_err(), "the refused key alone still fails");

    let kv2 = KvService::open(KvConfig::default(), Arc::new(DefaultAuthorizer), Some(Arc::new(db)))
        .await
        .unwrap();
    assert_eq!(kv2.exec(&a, "app", &cmd("GET good1")).unwrap(), Reply::bulk("a"));
    assert_eq!(kv2.exec(&a, "app", &cmd("GET good2")).unwrap(), Reply::bulk("c"));
    assert_eq!(kv2.exec(&a, "app", &cmd("GET poison")).unwrap(), Reply::Nil);
}
