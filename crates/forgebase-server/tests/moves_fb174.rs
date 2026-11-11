//! FB-174 on a running server: a tenant moves from control to a shard
//! while writers create records, a realtime subscriber listens and a queue
//! consumer pulls and acknowledges, with the server's move hooks
//! (`forgebase_server::moves`). Asserted: every acknowledged record is on
//! the shard exactly once, the subscriber saw each record's event exactly
//! once, every published message was delivered and acknowledged exactly
//! once. PostgreSQL clusters only (skips without an engine); SQLite: the
//! refusal.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use forgebase_core::Config;
use forgebase_core::ErrorCode;
use forgebase_core::config::{DatabaseBackend, DatabaseUrl, PostgresMode};
use forgebase_data::AccessContext;
use forgebase_db::moves::{self, MoveOptions, Quiet};
use forgebase_db::{Backend, Database, Statement, Value};
use serde_json::{Value as Json, json};

fn base_config(dir: &Path) -> Config {
    let mut cfg = Config::defaults(dir);
    cfg.server.host = "127.0.0.1".into();
    cfg.server.port = 0;
    cfg.server.shutdown_timeout_secs = 5;
    cfg.queue.enabled = true;
    cfg
}

async fn scalar(db: &Database, sql: &str) -> i64 {
    db.fetch_all(&Statement::new(sql, vec![]))
        .await
        .unwrap()
        .scalar()
        .and_then(Value::as_i64)
        .unwrap_or(-1)
}

fn options() -> MoveOptions {
    MoveOptions {
        poll: Duration::from_millis(50),
        ..MoveOptions::default()
    }
}

#[tokio::test]
async fn sqlite_servers_refuse_moves() {
    if std::env::var("FORGEBASE_TEST_BACKEND").as_deref() == Ok("postgres") {
        eprintln!("skipped: the refusal runs on SQLite");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = base_config(dir.path());
    cfg.database.backend = DatabaseBackend::Sqlite;
    let server = forgebase_server::start(&cfg).await.unwrap();
    let keyring = forgebase_auth::crypto::Keyring::load_or_create(
        &forgebase_auth::AuthConfig::for_data_dir(dir.path()).master_key_path,
    )
    .unwrap();
    let m = forgebase_server::moves::Moves::new(forgebase_server::moves::MovesConfig {
        router: server.router().clone(),
        jobs: server.jobs().clone(),
        kv: None,
        queue: server.queue().cloned(),
        keyring,
        database: cfg.database.clone(),
        control_url: None,
        shards: None,
        options: options(),
    });
    let err = m
        .start_move(&AccessContext::system(), "t1", "s1", None)
        .await
        .unwrap_err();
    assert_eq!(err.code(), "postgres_required");
    let err = m.plan(&moves::PlanOptions::default()).await.unwrap_err();
    assert_eq!(err.code(), "postgres_required");
    server.shutdown().await.unwrap();
}

#[cfg(unix)]
mod sharded {
    use super::*;
    use forgebase_db::shard_testing::Sharded;
    use forgebase_db::shards::{self, ShardSetup};
    use forgebase_db::topology::{MoveState, PlacementState};
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message as WsMessage;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    /// Realtime routes of the server's own service behind a test identity
    /// (an administrator in the tenant of `x-test-tenant`).
    async fn realtime_app(rt: forgebase_realtime::RealtimeService) -> std::net::SocketAddr {
        async fn as_admin(mut req: axum::extract::Request, next: axum::middleware::Next) -> axum::response::Response {
            let mut ctx = AccessContext::admin("root");
            if let Some(t) = req.headers().get("x-test-tenant").and_then(|v| v.to_str().ok()) {
                ctx = ctx.with_tenant(t);
            }
            req.extensions_mut().insert(ctx);
            next.run(req).await
        }
        let app = axum::Router::new()
            .nest_service("/api/v1/realtime", forgebase_realtime::router(rt))
            .layer(axum::middleware::from_fn(as_admin));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>())
                .await
                .unwrap();
        });
        addr
    }

    /// A realtime subscriber of `posts` in tenant `t1`: every record id of a
    /// `record.create` event, and every frame type.
    struct Subscriber {
        created: Arc<Mutex<Vec<String>>>,
        frames: Arc<Mutex<BTreeMap<String, usize>>>,
        task: tokio::task::JoinHandle<()>,
    }

    async fn subscribe(addr: std::net::SocketAddr) -> Subscriber {
        let mut req = format!("ws://{addr}/api/v1/realtime/ws").into_client_request().unwrap();
        req.headers_mut().insert("x-test-tenant", "t1".parse().unwrap());
        let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
        ws.send(WsMessage::Text(
            json!({"type": "subscribe", "id": "p", "collection": "posts"}).to_string().into(),
        ))
        .await
        .unwrap();
        let created = Arc::new(Mutex::new(Vec::new()));
        let frames = Arc::new(Mutex::new(BTreeMap::new()));
        let (c, f) = (created.clone(), frames.clone());
        let (tx, rx) = tokio::sync::oneshot::channel();
        let mut tx = Some(tx);
        let task = tokio::spawn(async move {
            while let Some(Ok(m)) = ws.next().await {
                let WsMessage::Text(t) = m else { continue };
                let j: Json = serde_json::from_str(t.as_str()).unwrap();
                let ty = j["type"].as_str().unwrap_or("").to_owned();
                if ty == "subscribed" {
                    if let Some(tx) = tx.take() {
                        let _ = tx.send(());
                    }
                }
                if ty == "record.create" {
                    c.lock().unwrap().push(j["data"]["id"].as_str().unwrap_or("").to_owned());
                }
                *f.lock().unwrap().entry(ty).or_insert(0) += 1;
            }
        });
        tokio::time::timeout(Duration::from_secs(10), rx).await.unwrap().unwrap();
        Subscriber { created, frames, task }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 6)]
    async fn a_tenant_moves_under_load_with_no_lost_or_duplicated_record_event_or_delivery() {
        let Some(mut s) = Sharded::start().await else {
            eprintln!("skipped: no PostgreSQL engine configured");
            return;
        };
        let (set, shard) = s.provision("s1").await;
        shards::activate_shard(
            &s.router,
            &set,
            &shard,
            ShardSetup {
                backups_dir: None,
                replay: None,
                control_conninfo: None,
            },
        )
        .await
        .unwrap();
        let dir = tempfile::Builder::new()
            .prefix("fb174-")
            .tempdir_in(std::env::var_os("FORGEBASE_TEST_PG_TMP").unwrap_or_else(|| std::env::temp_dir().into()))
            .unwrap();
        let key = forgebase_auth::AuthConfig::for_data_dir(dir.path().join("data")).master_key_path;
        std::fs::create_dir_all(key.parent().unwrap()).unwrap();
        let keyring = forgebase_auth::crypto::Keyring::load_or_create(&key).unwrap();
        let node = s.router.topology().nodes_of(&set.id).next().unwrap().clone();
        let sealed =
            forgebase_server::shards::seal_node_url(&keyring, &node.id, &s.clusters[1].url_of("forgebase")).unwrap();
        let mut tx = s.control.begin(forgebase_db::TxMode::Normal).await.unwrap();
        tx.execute(&Statement::new(
            "UPDATE _forgebase_db_nodes SET url_sealed = ?1 WHERE id = ?2",
            vec![sealed.into(), node.id.clone().into()],
        ))
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let mut cfg = base_config(&dir.path().join("data"));
        cfg.data_dir = dir.path().join("data");
        cfg.database.backend = DatabaseBackend::Postgres;
        cfg.database.postgres.mode = PostgresMode::External;
        cfg.database.postgres.url = Some(DatabaseUrl::new(s.clusters[0].url_of("forgebase")));
        let server = forgebase_server::start(&cfg).await.unwrap();
        let router = server.router().clone();
        assert!(router.is_sharded());
        let data = server.realtime().data().clone();
        let sys = AccessContext::system();
        data.create_collection(
            &sys,
            serde_json::from_value(json!({"name": "posts", "options": {"tenant_scoped": true}, "fields": [
                {"name": "title", "type": "text"}]}))
            .unwrap(),
        )
        .await
        .unwrap();
        router.place("t1", "control", PlacementState::Active).await.unwrap();
        let t1 = AccessContext::admin("root").with_tenant("t1");
        let queue = server.queue().unwrap().clone();
        queue
            .create_topic(&t1, serde_json::from_value(json!({"name": "t1-orders"})).unwrap())
            .await
            .unwrap();
        queue
            .create_subscription(
                &t1,
                serde_json::from_value(json!({"name": "t1-worker", "topic": "t1-orders", "ack_deadline_ms": 600000}))
                    .unwrap(),
            )
            .await
            .unwrap();
        for i in 0..50 {
            data.create_record(&t1, "posts", json!({"title": format!("seed {i}")}))
                .await
                .unwrap();
        }
        let rt_addr = realtime_app(server.realtime().clone()).await;
        let sub = subscribe(rt_addr).await;

        // Load: a record writer, a publisher and a consumer, all as tenant t1.
        let stop = Arc::new(AtomicBool::new(false));
        let records = Arc::new(Mutex::new(Vec::<String>::new()));
        let published = Arc::new(Mutex::new(Vec::<String>::new()));
        let acked = Arc::new(Mutex::new(Vec::<String>::new()));
        let errors = Arc::new(Mutex::new(Vec::<String>::new()));
        let mut tasks = Vec::new();
        {
            let (stop, records, errors, data, t1) = (stop.clone(), records.clone(), errors.clone(), data.clone(), t1.clone());
            tasks.push(tokio::spawn(async move {
                let mut i = 0;
                while !stop.load(Ordering::SeqCst) {
                    i += 1;
                    match data.create_record(&t1, "posts", json!({"title": format!("live {i}")})).await {
                        Ok(r) => records.lock().unwrap().push(r["id"].as_str().unwrap().to_owned()),
                        Err(e) => errors.lock().unwrap().push(format!("record: {e}")),
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            }));
        }
        {
            let (stop, published, errors, queue, t1) = (stop.clone(), published.clone(), errors.clone(), queue.clone(), t1.clone());
            tasks.push(tokio::spawn(async move {
                let mut i = 0;
                while !stop.load(Ordering::SeqCst) {
                    i += 1;
                    let msg = serde_json::from_value(json!({"data": {"n": i}})).unwrap();
                    match queue.publish(&t1, "t1-orders", vec![msg]).await {
                        Ok(p) => published.lock().unwrap().extend(p.into_iter().map(|m| m.message_id)),
                        Err(e) => errors.lock().unwrap().push(format!("publish: {e}")),
                    }
                    tokio::time::sleep(Duration::from_millis(7)).await;
                }
            }));
        }
        let consumer_stop = Arc::new(AtomicBool::new(false));
        {
            let (stop, acked, errors, queue, t1) = (consumer_stop.clone(), acked.clone(), errors.clone(), queue.clone(), t1.clone());
            tasks.push(tokio::spawn(async move {
                while !stop.load(Ordering::SeqCst) {
                    let req = serde_json::from_value(json!({"max_messages": 50, "wait_ms": 100})).unwrap();
                    match queue.pull(&t1, "t1-worker", req).await {
                        Ok(got) => {
                            if got.is_empty() {
                                continue;
                            }
                            let ids: Vec<String> = got.iter().map(|m| m.ack_id.clone()).collect();
                            match queue.ack(&t1, "t1-worker", &ids).await {
                                Ok(_) => acked.lock().unwrap().extend(got.into_iter().map(|m| m.message.id)),
                                Err(e) => errors.lock().unwrap().push(format!("ack: {e}")),
                            }
                        }
                        Err(e) => {
                            errors.lock().unwrap().push(format!("pull: {e}"));
                            tokio::time::sleep(Duration::from_millis(20)).await;
                        }
                    }
                }
            }));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;

        // The move, with the server's hooks.
        let m = forgebase_server::moves::Moves::new(forgebase_server::moves::MovesConfig {
            router: router.clone(),
            jobs: server.jobs().clone(),
            kv: None,
            queue: Some(queue.clone()),
            keyring: keyring.clone(),
            database: cfg.database.clone(),
            control_url: forgebase_server::moves::external_control_url(&cfg),
            shards: None,
            options: options(),
        });
        let rec = moves::begin(
            &router,
            &m,
            m.options(),
            moves::BeginMove {
                tenant: "t1".into(),
                to: set.id.clone(),
                ..moves::BeginMove::default()
            },
        )
        .await
        .unwrap();
        let done = moves::run(&router, &m, &Quiet, m.options(), &rec.id).await.unwrap();
        assert_eq!(done.state, MoveState::Cleanup, "{:?}", done.error);
        eprintln!(
            "FREEZE server tenant=t1 freeze_ms={:?} parts={:?} rows={}",
            done.detail.freeze_ms,
            done.detail.freeze_parts,
            done.detail.rows.values().sum::<u64>()
        );
        tokio::time::sleep(Duration::from_millis(700)).await;
        stop.store(true, Ordering::SeqCst);
        // Let the consumer drain what was published.
        let want_published = {
            for t in tasks.drain(..2) {
                t.await.unwrap();
            }
            published.lock().unwrap().clone()
        };
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        while acked.lock().unwrap().len() < want_published.len() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        consumer_stop.store(true, Ordering::SeqCst);
        for t in tasks {
            t.await.unwrap();
        }
        let errors = errors.lock().unwrap().clone();
        assert!(errors.is_empty(), "calls failed during the move: {errors:?}");

        // Records: every acknowledged one on the shard, once; none left behind on control as live data.
        let records = records.lock().unwrap().clone();
        assert!(records.len() > 20, "the writer ran ({})", records.len());
        let on_shard = scalar(&shard, "SELECT count(*) FROM posts WHERE _tenant = 't1'").await;
        assert_eq!(usize::try_from(on_shard).unwrap(), 50 + records.len());
        let after = data
            .create_record(&t1, "posts", json!({"title": "after"}))
            .await
            .unwrap();
        assert_eq!(
            scalar(&shard, &format!("SELECT count(*) FROM posts WHERE id = '{}'", after["id"].as_str().unwrap())).await,
            1
        );

        // Realtime: one create event per record, none twice.
        tokio::time::sleep(Duration::from_millis(800)).await;
        let created = sub.created.lock().unwrap().clone();
        let frames = sub.frames.lock().unwrap().clone();
        sub.task.abort();
        let distinct: BTreeSet<&String> = created.iter().collect();
        assert_eq!(distinct.len(), created.len(), "duplicated events; frames {frames:?}");
        let after_id = after["id"].as_str().unwrap().to_owned();
        let want: BTreeSet<&String> = records.iter().chain(std::iter::once(&after_id)).collect();
        let missing: Vec<&&String> = want.iter().filter(|r| !distinct.contains(**r)).collect();
        assert!(missing.is_empty(), "{} events missing (frames {frames:?}): {missing:?}", missing.len());

        // Queue: every published message delivered and acknowledged once.
        let acked = acked.lock().unwrap().clone();
        let acked_set: BTreeSet<&String> = acked.iter().collect();
        assert_eq!(acked_set.len(), acked.len(), "a message was delivered twice");
        let want_set: BTreeSet<&String> = want_published.iter().collect();
        assert_eq!(acked_set, want_set, "published {} acked {}", want_published.len(), acked.len());
        assert!(
            scalar(&shard, "SELECT count(*) FROM _forgebase_queue_messages WHERE topic = 't1-orders'").await > 0
        );

        // Cleanup through the server's hooks.
        let cleaned = moves::cleanup(&router, &m, m.options(), &rec.id).await.unwrap();
        assert_eq!(cleaned.state, MoveState::Done);
        assert_eq!(scalar(&s.control, "SELECT count(*) FROM posts WHERE _tenant = 't1'").await, 0);
        assert_eq!(router.placement("t1").unwrap().state, PlacementState::Active);
        server.shutdown().await.unwrap();
    }
}
