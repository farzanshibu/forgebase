//! Online tenant moves (ADR-0029 §3.8–§3.9, FB-174) on real, separate
//! PostgreSQL clusters (`shard_testing::Sharded`), and the refusal on
//! SQLite. The PostgreSQL tests skip without an engine.

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use forgebase_core::ErrorCode;
use forgebase_db::moves::{self, BeginMove, MoveControl, MoveError, MoveHooks, MoveOptions, PlanOptions, Quiet};
use forgebase_db::router::SetHandle;
use forgebase_db::shard_testing::Sharded;
use forgebase_db::shards::{self, ShardSetup};
use forgebase_db::topology::{self, DbSet, MoveState, PlacementState, SetState};
use forgebase_db::{Backend, BoxFuture, Database, DbRouter, SecretString, Statement, TenantRef, TxMode, Value};

const A: &str = "env-a";
const B: &str = "env-b";

async fn exec(db: &dyn Backend, sql: &str) {
    let mut tx = db.begin(TxMode::Normal).await.unwrap();
    tx.execute(&Statement::new(sql, vec![])).await.unwrap();
    tx.commit().await.unwrap();
}

async fn int(db: &dyn Backend, sql: &str) -> i64 {
    db.fetch_all(&Statement::new(sql, vec![]))
        .await
        .unwrap()
        .scalar()
        .and_then(Value::as_i64)
        .unwrap_or(-1)
}

async fn texts(db: &dyn Backend, sql: &str) -> Vec<String> {
    db.fetch_all(&Statement::new(sql, vec![]))
        .await
        .unwrap()
        .rows
        .iter()
        .map(|r| match r.first() {
            Some(Value::Text(s)) => s.clone(),
            Some(Value::Integer(i)) => i.to_string(),
            other => format!("{other:?}"),
        })
        .collect()
}

/// Hooks of the tests: connection strings of the clusters, a count of
/// calls, and an optional pause in the freeze.
#[derive(Debug, Default)]
struct Hooks {
    conninfo: Mutex<BTreeMap<String, String>>,
    calls: Mutex<Vec<String>>,
    freeze_pause: Mutex<Option<Duration>>,
}

impl Hooks {
    fn new(s: &Sharded, sets: &[&DbSet]) -> Arc<Self> {
        let h = Self::default();
        h.conninfo
            .lock()
            .unwrap()
            .insert("control".into(), s.clusters[0].conninfo("forgebase"));
        for (i, set) in sets.iter().enumerate() {
            h.conninfo
                .lock()
                .unwrap()
                .insert(set.id.clone(), s.clusters[i + 1].conninfo("forgebase"));
        }
        Arc::new(h)
    }

    fn called(&self, what: &str) -> usize {
        self.calls.lock().unwrap().iter().filter(|c| c.starts_with(what)).count()
    }
}

impl MoveHooks for Hooks {
    fn source_conninfo<'a>(&'a self, source: &'a DbSet) -> BoxFuture<'a, Result<SecretString, MoveError>> {
        Box::pin(async move {
            self.conninfo
                .lock()
                .unwrap()
                .get(&source.id)
                .cloned()
                .map(SecretString::new)
                .ok_or_else(|| MoveError::Hook(format!("no conninfo for {}", source.id)))
        })
    }

    fn frozen<'a>(&'a self, tenant: &'a str) -> BoxFuture<'a, Result<(), MoveError>> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(format!("frozen {tenant}"));
            let pause = *self.freeze_pause.lock().unwrap();
            if let Some(p) = pause {
                tokio::time::sleep(p).await;
            }
            Ok(())
        })
    }

    fn drain<'a>(&'a self, tenant: &'a str, source: &'a SetHandle) -> BoxFuture<'a, Result<(), MoveError>> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(format!("drain {tenant} {}", source.id()));
            Ok(())
        })
    }

    fn switched<'a>(
        &'a self,
        tenant: &'a str,
        source: &'a SetHandle,
        target: &'a SetHandle,
    ) -> BoxFuture<'a, Result<(), MoveError>> {
        Box::pin(async move {
            self.calls
                .lock()
                .unwrap()
                .push(format!("switched {tenant} {} {}", source.id(), target.id()));
            Ok(())
        })
    }

    fn unfrozen<'a>(&'a self, tenant: &'a str) -> BoxFuture<'a, Result<(), MoveError>> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(format!("unfrozen {tenant}"));
            Ok(())
        })
    }
}

/// Cancels when a stage is reported; can run a callback at a stage.
struct Ctl {
    cancel_at: Option<&'static str>,
    cancelled: AtomicBool,
    stages: Mutex<Vec<String>>,
}

impl Ctl {
    fn new(cancel_at: Option<&'static str>) -> Self {
        Self {
            cancel_at,
            cancelled: AtomicBool::new(false),
            stages: Mutex::new(Vec::new()),
        }
    }
}

impl MoveControl for Ctl {
    fn report<'a>(&'a self, stage: &'a str, _percent: f64, _message: String) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            self.stages.lock().unwrap().push(stage.to_owned());
            if self.cancel_at == Some(stage) {
                self.cancelled.store(true, Ordering::SeqCst);
            }
        })
    }

    fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

fn options() -> MoveOptions {
    MoveOptions {
        poll: Duration::from_millis(50),
        delete_batch: 7,
        ..MoveOptions::default()
    }
}

/// A tenant-scoped collection table, as catalog replay creates it on every set.
const POSTS: &str = "CREATE TABLE public.posts (id text PRIMARY KEY, _tenant text, title text NOT NULL, n bigint, \
                     doc jsonb, blob bytea)";

/// Control and one active shard `s1`, both with the `posts` table.
async fn two_sets() -> Option<(Sharded, DbSet, Database)> {
    let mut s = Sharded::start().await?;
    let (set, db) = s.provision("s1").await;
    let conninfo = s.control_conninfo();
    let set = shards::activate_shard(
        &s.router,
        &set,
        &db,
        ShardSetup {
            backups_dir: None,
            replay: None,
            control_conninfo: Some(&conninfo),
        },
    )
    .await
    .unwrap();
    exec(&s.control, POSTS).await;
    exec(&db, POSTS).await;
    for t in [A, B] {
        s.router.place(t, "control", PlacementState::Active).await.unwrap();
    }
    Some((s, set, db))
}

/// Rows of every kind for `tenant` on `db`: posts, KV, a job with events and
/// attempts (staged, renumbered), a queue topic with messages, a
/// subscription with deliveries and a cursor (renumbered, remapped).
async fn seed(db: &Database, tenant: &str, n: usize) {
    let mut tx = db.begin(TxMode::Normal).await.unwrap();
    for i in 0..n {
        tx.execute(&Statement::new(
            "INSERT INTO public.posts (id, _tenant, title, n, doc, blob) VALUES (?1, ?2, ?3, ?4, \
             CAST(CAST(?5 AS text) AS jsonb), decode('00ff10', 'hex'))",
            vec![
                format!("{tenant}-p{i}").into(),
                tenant.into(),
                format!("post {i} of {tenant}").into(),
                i64::try_from(i).unwrap().into(),
                format!(r#"{{"i": {i}, "tags": ["a", "b"]}}"#).into(),
            ],
        ))
        .await
        .unwrap();
        tx.execute(&Statement::new(
            "INSERT INTO _forgebase_kv_entries (tenant, namespace, key, type, data, updated_at) \
             VALUES (?1, 'app', ?2, 'string', CAST(CAST('\"v\"' AS text) AS jsonb), 1)",
            vec![format!("t:{tenant}").into(), format!("k{i}").into()],
        ))
        .await
        .unwrap();
    }
    let job = format!("job-{tenant}");
    tx.execute(&Statement::new(
        "INSERT INTO _forgebase_jobs (id, kind, status, run_at, created_by, created_at, updated_at, tenant) \
         VALUES (?1, 'test.kind', 'queued', 1, 'test', 1, 1, ?2)",
        vec![job.clone().into(), tenant.into()],
    ))
    .await
    .unwrap();
    for (i, e) in ["enqueued", "claimed", "released", "claimed"].iter().enumerate() {
        tx.execute(&Statement::new(
            "INSERT INTO _forgebase_job_events (job_id, at, event, attempt) VALUES (?1, ?2, ?3, ?4)",
            vec![job.clone().into(), i64::try_from(i).unwrap().into(), (*e).into(), 1.into()],
        ))
        .await
        .unwrap();
    }
    tx.execute(&Statement::new(
        "INSERT INTO _forgebase_job_attempts (job_id, attempt, started_at) VALUES (?1, 1, 5)",
        vec![job.into()],
    ))
    .await
    .unwrap();
    let topic = format!("{tenant}/orders");
    tx.execute(&Statement::new(
        "INSERT INTO _forgebase_queue_topics (name, retention_ms, max_message_bytes, dedup_window_ms, created_by, \
         created_at, updated_at, tenant) VALUES (?1, 86400000, 65536, 0, 'test', 1, 1, ?2)",
        vec![topic.clone().into(), tenant.into()],
    ))
    .await
    .unwrap();
    tx.execute(&Statement::new(
        "INSERT INTO _forgebase_queue_subscriptions (name, topic, ack_deadline_ms, backoff_base_ms, backoff_max_ms, \
         cursor_seq, created_by, created_at, updated_at, tenant) VALUES (?1, ?2, 30000, 100, 1000, 0, 'test', 1, 1, ?3)",
        vec![format!("{topic}/worker").into(), topic.clone().into(), tenant.into()],
    ))
    .await
    .unwrap();
    tx.commit().await.unwrap();
    for i in 0..n {
        publish(db, tenant, &format!("seed-{i}")).await;
    }
}

/// One queue message and its delivery, and the subscription cursor moved
/// past it, in one transaction (what publish + fan-out write).
async fn publish_in(tx: &mut dyn forgebase_db::WriteTx, tenant: &str, id: &str) {
    let topic = format!("{tenant}/orders");
    let rows = tx
        .fetch_all(&Statement::new(
            "INSERT INTO _forgebase_queue_messages (topic, id, data, bytes, publisher, published_at) \
             VALUES (?1, ?2, 'payload', 7, 'test', 1) RETURNING seq",
            vec![topic.clone().into(), format!("{tenant}:{id}").into()],
        ))
        .await
        .unwrap();
    let seq = rows.scalar().and_then(Value::as_i64).unwrap();
    tx.execute(&Statement::new(
        "INSERT INTO _forgebase_queue_deliveries (subscription, seq, state, available_at) VALUES (?1, ?2, 'ready', 1)",
        vec![format!("{topic}/worker").into(), seq.into()],
    ))
    .await
    .unwrap();
    tx.execute(&Statement::new(
        "UPDATE _forgebase_queue_subscriptions SET cursor_seq = ?2 WHERE name = ?1",
        vec![format!("{topic}/worker").into(), seq.into()],
    ))
    .await
    .unwrap();
}

async fn publish(db: &Database, tenant: &str, id: &str) {
    let mut tx = db.begin(TxMode::Normal).await.unwrap();
    publish_in(tx.as_mut(), tenant, id).await;
    tx.commit().await.unwrap();
}

/// A writer that keeps writing posts and queue messages for `tenant`
/// through the router (as the services do), recording what was
/// acknowledged (committed) and where.
struct Writer {
    stop: Arc<AtomicBool>,
    acked: Arc<Mutex<Vec<(String, String)>>>,
    failed: Arc<AtomicU64>,
    task: tokio::task::JoinHandle<()>,
}

impl Writer {
    fn start(router: DbRouter, tenant: &'static str) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let acked = Arc::new(Mutex::new(Vec::new()));
        let failed = Arc::new(AtomicU64::new(0));
        let (s, a, f) = (stop.clone(), acked.clone(), failed.clone());
        let task = tokio::spawn(async move {
            let mut i = 0u64;
            while !s.load(Ordering::SeqCst) {
                i += 1;
                let id = format!("{tenant}-w{i}");
                let r: Result<String, String> = async {
                    let h = router
                        .for_tenant(TenantRef::Tenant(tenant))
                        .await
                        .map_err(|e| e.to_string())?;
                    let mut tx = h.begin(TxMode::Normal).await.map_err(|e| e.to_string())?;
                    tx.execute(&Statement::new(
                        "INSERT INTO public.posts (id, _tenant, title, n) VALUES (?1, ?2, 'live', ?3)",
                        vec![id.clone().into(), tenant.into(), i64::try_from(i).unwrap().into()],
                    ))
                    .await
                    .map_err(|e| e.to_string())?;
                    publish_in(tx.as_mut(), tenant, &format!("w{i}")).await;
                    tx.commit().await.map_err(|e| e.to_string())?;
                    Ok(h.id().to_owned())
                }
                .await;
                match r {
                    Ok(set) => a.lock().unwrap().push((id, set)),
                    Err(e) => {
                        eprintln!("writer: {e}");
                        f.fetch_add(1, Ordering::SeqCst);
                    }
                }
                tokio::time::sleep(Duration::from_millis(3)).await;
            }
        });
        Self {
            stop,
            acked,
            failed,
            task,
        }
    }

    async fn stop(self) -> (Vec<(String, String)>, u64) {
        self.stop.store(true, Ordering::SeqCst);
        self.task.await.unwrap();
        let acked = self.acked.lock().unwrap().clone();
        (acked, self.failed.load(Ordering::SeqCst))
    }
}

async fn publication_and_slot(db: &Database) -> (i64, i64) {
    (
        int(db, "SELECT count(*) FROM pg_publication WHERE pubname LIKE 'fb_move_%'").await,
        int(db, "SELECT count(*) FROM pg_replication_slots WHERE slot_name LIKE 'fb_move_%'").await,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_tenant_moves_under_load_with_no_lost_or_duplicated_row_then_cleans_up_and_moves_back() {
    let Some((s, set, shard)) = two_sets().await else {
        eprintln!("skipped: no PostgreSQL engine configured");
        return;
    };
    seed(&s.control, A, 300).await;
    seed(&s.control, B, 20).await;
    let hooks = Hooks::new(&s, &[&set]);
    let writer = Writer::start(s.router.clone(), A);
    tokio::time::sleep(Duration::from_millis(300)).await;

    let rec = moves::begin(
        &s.router,
        hooks.as_ref(),
        &options(),
        BeginMove {
            tenant: A.into(),
            to: set.id.clone(),
            created_by: Some("admin".into()),
            ..BeginMove::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(rec.state, MoveState::Preparing);
    assert_eq!(rec.from_set, "control");
    // A second move of the tenant is refused while this one is open.
    let again = moves::begin(
        &s.router,
        hooks.as_ref(),
        &options(),
        BeginMove {
            tenant: A.into(),
            to: set.id.clone(),
            ..BeginMove::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(again.code(), "tenant_moving");

    let ctl = Ctl::new(None);
    let done = moves::run(&s.router, hooks.as_ref(), &ctl, &options(), &rec.id).await.unwrap();
    assert_eq!(done.state, MoveState::Cleanup, "{:?}", done.error);
    let freeze = done.detail.freeze_ms.unwrap();
    eprintln!(
        "FREEZE tenant={A} rows={:?} freeze_ms={freeze} parts={:?}",
        done.detail.rows, done.detail.freeze_parts
    );
    assert!(freeze < 10_000);
    assert!(done.detail.replication_dropped);
    assert_eq!(hooks.called("frozen"), 1);
    assert_eq!(hooks.called("drain"), 1);
    assert_eq!(hooks.called("switched"), 1);
    // Keep writing on the target for a moment, then stop.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (acked, failed) = writer.stop().await;
    assert_eq!(failed, 0, "no write failed during the move");
    assert!(acked.iter().any(|(_, set_id)| set_id == "control"));
    assert!(acked.iter().any(|(_, set_id)| *set_id == set.id), "writes reached the target after the switch");

    // Routing: the tenant is on the shard, B stays on control.
    let p = s.router.placement(A).unwrap();
    assert_eq!((p.set_id.as_str(), p.state), (set.id.as_str(), PlacementState::Cleanup));
    assert_eq!(moves::routed_set(&s.router, A).await.unwrap(), set.id);
    assert_eq!(moves::routed_set(&s.router, B).await.unwrap(), "control");

    // Every acknowledged post is on the shard exactly once (the primary key
    // forbids duplicates), with the seeded ones: nothing lost.
    let on_shard: BTreeSet<String> = texts(&shard, &format!("SELECT id FROM public.posts WHERE _tenant = '{A}'"))
        .await
        .into_iter()
        .collect();
    let want: BTreeSet<String> = (0..300)
        .map(|i| format!("{A}-p{i}"))
        .chain(acked.iter().map(|(id, _)| id.clone()))
        .collect();
    assert_eq!(on_shard, want);
    assert_eq!(int(&shard, &format!("SELECT count(*) FROM public.posts WHERE _tenant = '{B}'")).await, 0);
    // Columns survive (jsonb, bytea).
    assert_eq!(
        texts(&shard, &format!("SELECT doc->>'i' || encode(blob, 'hex') FROM public.posts WHERE id = '{A}-p7'")).await,
        vec!["700ff10".to_owned()]
    );
    // KV under the tenant's scope.
    assert_eq!(
        int(&shard, &format!("SELECT count(*) FROM _forgebase_kv_entries WHERE tenant = 't:{A}'")).await,
        300
    );
    // Job events and attempts: staged, renumbered into the shard's range, in source order.
    let events = texts(
        &shard,
        &format!("SELECT event FROM _forgebase_job_events WHERE job_id = 'job-{A}' ORDER BY id"),
    )
    .await;
    assert_eq!(events, ["enqueued", "claimed", "released", "claimed"]);
    let min_id = int(&shard, &format!("SELECT min(id) FROM _forgebase_job_events WHERE job_id = 'job-{A}'")).await;
    assert_eq!(topology::set_number_of(min_id), set.number);
    assert_eq!(
        int(&shard, &format!("SELECT count(*) FROM _forgebase_job_attempts WHERE job_id = 'job-{A}'")).await,
        1
    );
    // Queue: one message and one delivery per publish (seeded + acknowledged),
    // messages renumbered into the shard's range in order, deliveries remapped,
    // the cursor at the last message.
    let msgs = int(&shard, &format!("SELECT count(*) FROM _forgebase_queue_messages WHERE topic = '{A}/orders'")).await;
    assert_eq!(usize::try_from(msgs).unwrap(), 300 + acked.len());
    let ids = texts(
        &shard,
        &format!("SELECT id FROM _forgebase_queue_messages WHERE topic = '{A}/orders' ORDER BY seq"),
    )
    .await;
    let distinct: BTreeSet<&String> = ids.iter().collect();
    assert_eq!(distinct.len(), ids.len(), "no duplicated message");
    let seeded: Vec<String> = ids.iter().filter(|i| i.contains(":seed-")).cloned().collect();
    assert_eq!(seeded.first().unwrap(), &format!("{A}:seed-0"));
    assert_eq!(seeded.last().unwrap(), &format!("{A}:seed-299"));
    let min_seq = int(&shard, &format!("SELECT min(seq) FROM _forgebase_queue_messages WHERE topic = '{A}/orders'")).await;
    assert_eq!(topology::set_number_of(min_seq), set.number);
    assert_eq!(
        int(
            &shard,
            &format!(
                "SELECT count(*) FROM _forgebase_queue_deliveries d JOIN _forgebase_queue_messages m ON m.seq = d.seq \
                 WHERE d.subscription = '{A}/orders/worker'"
            )
        )
        .await,
        msgs,
        "every delivery points at its renumbered message"
    );
    assert_eq!(
        int(&shard, &format!("SELECT cursor_seq FROM _forgebase_queue_subscriptions WHERE name = '{A}/orders/worker'")).await,
        int(&shard, &format!("SELECT max(seq) FROM _forgebase_queue_messages WHERE topic = '{A}/orders'")).await
    );
    // New messages on the shard come after the moved ones.
    publish(&shard, A, "after").await;
    assert_eq!(
        texts(
            &shard,
            &format!("SELECT id FROM _forgebase_queue_messages WHERE topic = '{A}/orders' ORDER BY seq DESC LIMIT 1")
        )
        .await,
        vec![format!("{A}:after")]
    );
    // Replication objects are gone; the source keeps the rows until the cleanup.
    assert_eq!(publication_and_slot(&s.control).await, (0, 0));
    assert_eq!(int(&s.control, &format!("SELECT count(*) FROM public.posts WHERE _tenant = '{A}'")).await > 0, true);
    assert!(moves::cleanup_due(&done, &options()).is_some());

    // Cleanup: the source rows go, B's stay; the placement is active.
    let cleaned = moves::cleanup(&s.router, hooks.as_ref(), &options(), &rec.id).await.unwrap();
    assert_eq!(cleaned.state, MoveState::Done);
    assert!(cleaned.detail.cleaned_rows.unwrap() > 600);
    for (table, col, t) in [
        ("public.posts", "_tenant", A.to_owned()),
        ("_forgebase_kv_entries", "tenant", format!("t:{A}")),
        ("_forgebase_jobs", "tenant", A.to_owned()),
        ("_forgebase_queue_topics", "tenant", A.to_owned()),
    ] {
        assert_eq!(int(&s.control, &format!("SELECT count(*) FROM {table} WHERE {col} = '{t}'")).await, 0, "{table}");
    }
    assert_eq!(int(&s.control, &format!("SELECT count(*) FROM _forgebase_job_events WHERE job_id = 'job-{A}'")).await, 0);
    assert_eq!(int(&s.control, &format!("SELECT count(*) FROM _forgebase_queue_messages WHERE topic = '{A}/orders'")).await, 0);
    assert_eq!(int(&s.control, &format!("SELECT count(*) FROM public.posts WHERE _tenant = '{B}'")).await, 20);
    assert_eq!(int(&s.control, &format!("SELECT count(*) FROM _forgebase_queue_messages WHERE topic = '{B}/orders'")).await, 20);
    assert_eq!(s.router.placement(A).unwrap().state, PlacementState::Active);

    // Undo: a move back to control.
    let back = moves::undo(&s.router, hooks.as_ref(), &options(), &rec.id, Some("admin".into())).await.unwrap();
    assert_eq!(back.to_set, "control");
    assert_eq!(back.detail.undo_of.as_deref(), Some(rec.id.as_str()));
    let back = moves::run(&s.router, hooks.as_ref(), &Quiet, &options(), &back.id).await.unwrap();
    assert_eq!(back.state, MoveState::Cleanup, "{:?}", back.error);
    assert_eq!(moves::routed_set(&s.router, A).await.unwrap(), "control");
    let back_posts = int(&s.control, &format!("SELECT count(*) FROM public.posts WHERE _tenant = '{A}'")).await;
    assert_eq!(usize::try_from(back_posts).unwrap(), want.len());
    let seq = int(&s.control, &format!("SELECT min(seq) FROM _forgebase_queue_messages WHERE topic = '{A}/orders'")).await;
    assert_eq!(topology::set_number_of(seq), 0, "renumbered into control's range");
    moves::cleanup(&s.router, hooks.as_ref(), &options(), &back.id).await.unwrap();
    assert_eq!(int(&shard, &format!("SELECT count(*) FROM public.posts WHERE _tenant = '{A}'")).await, 0);
    let all = moves::list(&s.control, &moves::MoveQuery { tenant: Some(A.into()), ..Default::default() })
        .await
        .unwrap();
    assert_eq!(all.len(), 2);
    assert!(all.iter().all(|m| m.state == MoveState::Done));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_and_freeze_timeout_abort_with_nothing_changed_and_refusals() {
    let Some((s, set, shard)) = two_sets().await else {
        eprintln!("skipped: no PostgreSQL engine configured");
        return;
    };
    seed(&s.control, A, 50).await;
    let hooks = Hooks::new(&s, &[&set]);
    let opts = options();

    // Refusals.
    let same = moves::begin(&s.router, hooks.as_ref(), &opts, BeginMove { tenant: A.into(), to: "control".into(), ..Default::default() })
        .await
        .unwrap_err();
    assert_eq!(same.code(), "invalid_request");
    let unknown = moves::begin(&s.router, hooks.as_ref(), &opts, BeginMove { tenant: "nobody".into(), to: set.id.clone(), ..Default::default() })
        .await
        .unwrap_err();
    assert_eq!(unknown.code(), "invalid_request");
    let nowhere = moves::begin(&s.router, hooks.as_ref(), &opts, BeginMove { tenant: A.into(), to: "no-such-set".into(), ..Default::default() })
        .await
        .unwrap_err();
    assert_eq!(nowhere.code(), "shard_not_found");

    // Cancel while copying: aborted, nothing on the target, A active on control.
    let rec = moves::begin(&s.router, hooks.as_ref(), &opts, BeginMove { tenant: A.into(), to: set.id.clone(), ..Default::default() })
        .await
        .unwrap();
    let ctl = Ctl::new(Some("copying"));
    let err = moves::run(&s.router, hooks.as_ref(), &ctl, &opts, &rec.id).await.unwrap_err();
    assert_eq!(err.code(), "cancelled");
    let aborted = moves::get(&s.control, &rec.id).await.unwrap();
    assert_eq!(aborted.state, MoveState::Aborted);
    assert!(aborted.error.unwrap().contains("cancelled"));
    assert_eq!(s.router.placement(A).unwrap().state, PlacementState::Active);
    assert_eq!(s.router.placement(A).unwrap().set_id, "control");
    assert_eq!(int(&shard, &format!("SELECT count(*) FROM public.posts WHERE _tenant = '{A}'")).await, 0);
    assert_eq!(int(&shard, &format!("SELECT count(*) FROM _forgebase_kv_entries WHERE tenant = 't:{A}'")).await, 0);
    assert_eq!(int(&shard, "SELECT count(*) FROM pg_subscription WHERE subname LIKE 'fb_move_%'").await, 0);
    assert_eq!(int(&shard, "SELECT count(*) FROM pg_tables WHERE schemaname = 'fb_move' AND tablename <> 'barrier'").await, 0);
    assert_eq!(publication_and_slot(&s.control).await, (0, 0));
    assert_eq!(
        texts(&s.control, "SELECT CAST(relreplident AS text) FROM pg_class WHERE oid = 'forgebase._forgebase_jobs'::regclass").await,
        vec!["d".to_owned()],
        "replica identity restored"
    );
    assert_eq!(hooks.called("unfrozen"), 1);

    // A freeze over its limit aborts and unfreezes.
    *hooks.freeze_pause.lock().unwrap() = Some(Duration::from_millis(400));
    let short = MoveOptions {
        freeze_timeout: Duration::from_millis(200),
        ..opts
    };
    let rec = moves::begin(&s.router, hooks.as_ref(), &short, BeginMove { tenant: A.into(), to: set.id.clone(), ..Default::default() })
        .await
        .unwrap();
    let err = moves::run(&s.router, hooks.as_ref(), &Quiet, &short, &rec.id).await.unwrap_err();
    assert_eq!(err.code(), "move_freeze_timeout");
    let aborted = moves::get(&s.control, &rec.id).await.unwrap();
    assert_eq!(aborted.state, MoveState::Aborted);
    assert!(aborted.detail.freeze_ms.unwrap() >= 200);
    assert_eq!(moves::routed_set(&s.router, A).await.unwrap(), "control");
    assert_eq!(int(&s.control, &format!("SELECT count(*) FROM public.posts WHERE _tenant = '{A}'")).await, 50);
    assert_eq!(int(&shard, &format!("SELECT count(*) FROM public.posts WHERE _tenant = '{A}'")).await, 0);
    assert_eq!(publication_and_slot(&s.control).await, (0, 0));

    // A switched move cannot be aborted, an unswitched one cannot be undone or cleaned up.
    *hooks.freeze_pause.lock().unwrap() = None;
    let rec = moves::begin(&s.router, hooks.as_ref(), &opts, BeginMove { tenant: A.into(), to: set.id.clone(), ..Default::default() })
        .await
        .unwrap();
    assert_eq!(moves::undo(&s.router, hooks.as_ref(), &opts, &rec.id, None).await.unwrap_err().code(), "move_state");
    assert_eq!(moves::cleanup(&s.router, hooks.as_ref(), &opts, &rec.id).await.unwrap_err().code(), "move_state");
    let done = moves::run(&s.router, hooks.as_ref(), &Quiet, &opts, &rec.id).await.unwrap();
    assert_eq!(done.state, MoveState::Cleanup, "{:?}", done.error);
    assert_eq!(
        moves::abort(&s.router, hooks.as_ref(), &opts, &rec.id, "no").await.unwrap_err().code(),
        "move_state"
    );
    assert_eq!(int(&shard, &format!("SELECT count(*) FROM public.posts WHERE _tenant = '{A}'")).await, 50);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn containers_created_during_a_move_are_copied_in_the_freeze() {
    let Some((s, set, shard)) = two_sets().await else {
        eprintln!("skipped: no PostgreSQL engine configured");
        return;
    };
    seed(&s.control, A, 5).await;
    let hooks = Hooks::new(&s, &[&set]);
    let rec = moves::begin(&s.router, hooks.as_ref(), &options(), BeginMove { tenant: A.into(), to: set.id.clone(), ..Default::default() })
        .await
        .unwrap();
    // Run the prepare step only, by cancelling the copy … no: start the move,
    // and create a topic while it copies (a hook-free stand-in for an API call).
    struct AtCopy<'a> {
        db: &'a Database,
        done: AtomicBool,
    }
    impl MoveControl for AtCopy<'_> {
        fn report<'a>(&'a self, stage: &'a str, _p: f64, _m: String) -> BoxFuture<'a, ()> {
            Box::pin(async move {
                if stage == "copying" && !self.done.swap(true, Ordering::SeqCst) {
                    exec(
                        self.db,
                        &format!(
                            "INSERT INTO _forgebase_queue_topics (name, retention_ms, max_message_bytes, dedup_window_ms, \
                             created_by, created_at, updated_at, tenant) VALUES ('{A}/late', 1000, 65536, 0, 't', 1, 1, '{A}')"
                        ),
                    )
                    .await;
                    for i in 0..3 {
                        exec(
                            self.db,
                            &format!(
                                "INSERT INTO _forgebase_queue_messages (topic, id, data, bytes, publisher, published_at) \
                                 VALUES ('{A}/late', '{A}:late-{i}', 'x', 1, 't', 1)"
                            ),
                        )
                        .await;
                    }
                }
            })
        }
        fn cancelled(&self) -> bool {
            false
        }
    }
    let ctl = AtCopy {
        db: &s.control,
        done: AtomicBool::new(false),
    };
    let done = moves::run(&s.router, hooks.as_ref(), &ctl, &options(), &rec.id).await.unwrap();
    assert_eq!(done.state, MoveState::Cleanup, "{:?}", done.error);
    assert!(ctl.done.load(Ordering::SeqCst), "the topic was created during the copy");
    assert_eq!(
        texts(&shard, &format!("SELECT id FROM _forgebase_queue_messages WHERE topic = '{A}/late' ORDER BY seq")).await,
        vec![format!("{A}:late-0"), format!("{A}:late-1"), format!("{A}:late-2")]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rebalance_plans_run_one_move_at_a_time_and_a_drain_empties_a_shard() {
    let Some((s, set, shard)) = two_sets().await else {
        eprintln!("skipped: no PostgreSQL engine configured");
        return;
    };
    seed(&s.control, A, 40).await;
    seed(&s.control, B, 10).await;
    let hooks = Hooks::new(&s, &[&set]);
    assert_eq!(moves::rebalance::refresh_stats(&s.router).await.unwrap(), 2);
    let a = topology::load_placement(&s.control, A).await.unwrap().unwrap();
    assert!(a.est_rows >= 80 && a.est_bytes > 0, "{a:?}");
    let loads = moves::rebalance::tenant_loads(&s.router).await.unwrap();
    let plan = moves::plan_rebalance(&s.router.topology().sets, &loads, &PlanOptions::default());
    // Moving the smaller tenant (B) narrows the spread; moving A as well would not.
    assert_eq!(plan.moves.len(), 1, "{plan:?}");
    assert_eq!(plan.moves[0].tenant, B);
    assert!(plan.spread_after < plan.spread_before);
    moves::rebalance::save_plan(&s.control, &plan).await.unwrap();
    let stored = moves::rebalance::load_plan(&s.control, &plan.id).await.unwrap();
    assert_eq!(stored, plan);
    let out = moves::run_plan(&s.router, hooks.as_ref(), &Quiet, &options(), &stored, Some("admin".into()))
        .await
        .unwrap();
    assert!(out.failed.is_none(), "{out:?}");
    assert_eq!(out.moves.len(), 1);
    assert_eq!(out.moves[0].rebalance_id.as_deref(), Some(plan.id.as_str()));
    assert_eq!(moves::routed_set(&s.router, B).await.unwrap(), set.id);
    // The plan again: B is on the shard now, so it is skipped.
    let again = moves::run_plan(&s.router, hooks.as_ref(), &Quiet, &options(), &stored, None).await.unwrap();
    assert_eq!(again.skipped, vec![B.to_owned()]);

    // Removing the shard is refused while it holds B; draining moves B back.
    assert_eq!(moves::check_removable(&s.router, &set.id).await.unwrap_err().code(), "move_state");
    let drain = moves::drain_set(&s.router, &set.id).await.unwrap();
    assert_eq!(s.router.topology().set(&set.id).unwrap().state, SetState::Draining);
    assert_eq!(drain.moves.len(), 1);
    assert_eq!(drain.moves[0].to, "control");
    let out = moves::run_plan(&s.router, hooks.as_ref(), &Quiet, &options(), &drain, None).await.unwrap();
    assert!(out.failed.is_none(), "{out:?}");
    assert_eq!(moves::routed_set(&s.router, B).await.unwrap(), "control");
    // The moves are switched but keep their source rows: still not removable.
    assert_eq!(moves::check_removable(&s.router, &set.id).await.unwrap_err().code(), "operation_in_progress");
    for m in moves::list(&s.control, &moves::MoveQuery { open: true, ..Default::default() }).await.unwrap() {
        moves::cleanup(&s.router, hooks.as_ref(), &options(), &m.id).await.unwrap();
    }
    moves::check_removable(&s.router, &set.id).await.unwrap();
    assert_eq!(int(&shard, &format!("SELECT count(*) FROM public.posts WHERE _tenant = '{B}'")).await, 0);
    // Draining control is refused.
    assert_eq!(moves::drain_set(&s.router, "control").await.unwrap_err().code(), "invalid_request");
}

/// SQLite (and one-set PostgreSQL): moves are refused before anything
/// changes.
#[tokio::test]
async fn moves_need_postgresql_and_shards() {
    let db = forgebase_db::testing::test_backend().await;
    let router = DbRouter::single(db.database().clone());
    let hooks = Hooks::default();
    let err = moves::begin(&router, &hooks, &options(), BeginMove { tenant: A.into(), to: "s1".into(), ..Default::default() })
        .await
        .unwrap_err();
    match db.kind() {
        forgebase_db::BackendKind::Sqlite => assert_eq!(err.code(), "postgres_required"),
        forgebase_db::BackendKind::Postgres => assert_eq!(err.code(), "shard_not_found"),
    }
    assert!(moves::list(db.database(), &moves::MoveQuery::default()).await.unwrap().is_empty());
}
