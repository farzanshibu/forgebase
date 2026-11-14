//! Runs a ForgeBase instance.
//!
//! [`start`] performs the startup sequence and returns a [`RunningServer`]:
//!
//! 1. create the data-directory layout,
//! 2. take the data-directory lock (one server per data directory),
//! 3. decide the backend ([`database::resolve`]: the backend marker, else
//!    what is on disk, else `[database] backend`), start the bundled
//!    PostgreSQL engine when the backend is `postgres` in `managed` mode
//!    (`forgebase-pgengine`, ADR-0028 §4.3), open the primary database and
//!    apply pending system migrations, snapshotting first if the database
//!    already existed (SQLite),
//! 4. record the instance id on first start,
//! 5. bind the listener and serve the API (with the jobs API of
//!    `forgebase-jobs` mounted under `/api/v1/jobs` and the vector API of
//!    `forgebase-vector` under `/api/v1/vector`, and the entitlement and
//!    quota API of `forgebase-entitlements`, whose hooks run in steps 4 and
//!    6 of the policy pipeline and whose gate applies plan-based API rate
//!    limits, and the credit ledger of `forgebase-credits` under
//!    `/api/v1/credits`, which charges usage events of priced features, and
//!    the topics and queues API of `forgebase-queue` under `/api/v1/queues`
//!    unless `[queue] enabled = false`, and the snapshot API of
//!    `forgebase-backup` under `/api/v1/system/backups`),
//! 6. start the job scheduler and in-process worker pool, the vector
//!    engine's index warm-up and saver, and the queue's maintenance and push
//!    task,
//! 7. when `[resp] enabled = true`, bind the Redis protocol listener
//!    (`forgebase-resp`, ADR-0013) in front of the built-in KV engine.
//!
//! Native TLS (ADR-0033, [`tls`]): with `[tls] mode = "files"` or `"acme"`
//! the HTTP listener serves HTTPS (and RESP too with `[resp] tls`). The
//! certificate is prepared before the database opens, so a missing file or a
//! key that does not match refuses to start; certificate files are reloaded
//! and ACME certificates renewed in the background.
//!
//! Switchable modules (FB-91, ADR-0026): a module with `[<module>] enabled =
//! false` (storage, functions, queue, realtime, vector, graph, identity,
//! access, resp, billing) is not opened, its routes answer `404
//! feature_disabled`, its job handlers are not registered, its system
//! schedules are paused and its background tasks do not start. Realtime is
//! the exception that keeps its record-change outbox (functions and queues
//! read it) and drops only the client routes and the dispatcher.
//!
//! [`RunningServer::shutdown`] stops accepting connections, waits up to
//! `shutdown_timeout_secs` for in-flight requests, stops the job pool (running
//! jobs get the same time to finish, the rest are released back to the queue),
//! writes changed vector indexes to disk, checkpoints the WAL into the main database file, closes the pools,
//! stops the bundled PostgreSQL engine (fast shutdown) and releases the lock.

pub mod database;
mod function_services;
mod job_results;
mod lock;
pub mod moves;
pub mod replicas;
pub mod shards;
pub mod tls;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use forgebase_api::{AppState, RouterOptions};
use forgebase_core::{Config, ConfigOrigin, ConfigSources, DataLayout, ErrorCode, ErrorKind, LoadedConfig};
use forgebase_credits::CreditService;
use forgebase_customize::{CustomizeConfig, CustomizeService};
use forgebase_data::Authorizer;
use forgebase_db::{Database, DbError, InstanceInfo, MigrateReport};
use forgebase_entitlements::{EntitlementService, SystemClock};
use forgebase_functions::{FunctionService, FunctionsConfig};
use forgebase_kv::{KvConfig, KvError, KvService};
use forgebase_vector::{VectorConfig, VectorError, VectorService};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

pub use database::BackendMarker;
pub use forgebase_jobs::{JobRegistry, JobService, PoolConfig};
pub use job_results::{RESULTS_BUCKET, StorageResults};
pub use lock::{DataDirLock, LockOwner};

/// A started server.
#[derive(Debug)]
pub struct RunningServer {
    addr: SocketAddr,
    db: Database,
    instance: InstanceInfo,
    migrations: MigrateReport,
    shutdown_tx: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<std::io::Result<()>>>,
    upkeep: JoinHandle<()>,
    jobs: JobService,
    job_pool: Option<forgebase_jobs::PoolHandle>,
    /// pg_cron schedule sync and `LISTEN` wake-up (FB-165); dropping it stops both.
    pgcron: Option<forgebase_jobs::PgCronHandle>,
    kv: KvService,
    vector: Option<VectorService>,
    resp: Option<forgebase_resp::RespHandle>,
    realtime: forgebase_realtime::RealtimeService,
    entitlements: EntitlementService,
    functions: Option<FunctionService>,
    credits: CreditService,
    billing: Option<forgebase_billing::BillingService>,
    customize: CustomizeService,
    queue: Option<forgebase_queue::QueueService>,
    /// The alerting engine and SLOs (ADR-0035), unless `[alerts] enabled = false`.
    alerts: Option<forgebase_alerts::AlertService>,
    /// Operations insights (ADR-0036): flushed once more at shutdown.
    insights: forgebase_insights::InsightsService,
    shutdown_timeout: Duration,
    lock: Option<DataDirLock>,
    /// First-run setup token (FB-90), while no administrator exists.
    setup_token: Option<String>,
    /// The backend this data directory holds (FB-148).
    backend: BackendMarker,
    /// The bundled PostgreSQL engine, when this server runs it.
    engine: Option<Arc<forgebase_pgengine::EngineHandle>>,
    /// The database sets (FB-171): control and, once sharded, every shard.
    router: forgebase_db::DbRouter,
    /// Two-phase commit recovery at start and every minute (FB-173,
    /// PostgreSQL); dropping it stops the loop.
    recovery: Option<forgebase_db::twophase::RecoveryTask>,
    /// Managed replicas and the topology service (FB-175).
    replicas: Arc<replicas::Replicas>,
    /// Native TLS (ADR-0033), when `[tls] mode` is not `off`.
    tls: Option<Arc<tls::TlsSetup>>,
    /// The certificate file watcher or the ACME renewer.
    tls_tasks: Option<tls::TlsTasks>,
    /// The plain HTTP listener (redirects, `http-01`): address, stop, task.
    plain: Option<(SocketAddr, oneshot::Sender<()>, JoinHandle<std::io::Result<()>>)>,
}

/// Starts a server for `config` with no in-process job handlers (jobs are
/// still stored, scheduled and served to external workers). Returns once the
/// listener is bound.
pub async fn start(config: &Config) -> Result<RunningServer, ServerError> {
    let pool = PoolConfig {
        shutdown_grace: Duration::from_secs(config.server.shutdown_timeout_secs),
        ..PoolConfig::default()
    };
    start_with_jobs(config, JobRegistry::new(), pool).await
}

/// Like [`start`] for a configuration from the loader: the feature registry
/// (`GET /api/system/features`) reports where each setting came from, and
/// `PATCH /api/system/config` writes the loaded file. `forgebase serve` uses it.
pub async fn start_loaded(loaded: &LoadedConfig) -> Result<RunningServer, ServerError> {
    let config = &loaded.config;
    let pool = PoolConfig {
        shutdown_grace: Duration::from_secs(config.server.shutdown_timeout_secs),
        ..PoolConfig::default()
    };
    let origin = (loaded.origin.clone(), loaded.sources.clone());
    start_full(config, Some(origin), JobRegistry::new(), pool, false).await
}

/// Like [`start`], with in-process handlers for job kinds. The worker pool
/// claims only the kinds registered in `registry`.
pub async fn start_with_jobs(
    config: &Config,
    registry: JobRegistry,
    pool: PoolConfig,
) -> Result<RunningServer, ServerError> {
    start_full(config, None, registry, pool, false).await
}

/// Starts a **quiescent** server (ADR-0041): the database, the API and the
/// listener, but nothing that acts on its own. No job worker pool and no
/// scheduler (so no scheduled job and no cron fires, and no job is claimed),
/// no queue push delivery, no outbound webhooks or alert notifications, no
/// function after-hooks, no realtime dispatch, no vector warm-up, no TLS
/// renewal, no scheduled drill. A restore drill uses it to look at restored
/// data without the data doing anything in the world. The caller picks a
/// loopback address and port `0`, and turns TLS and RESP off.
pub async fn start_quiescent(config: &Config) -> Result<RunningServer, ServerError> {
    start_quiescent_with_jobs(config, JobRegistry::new()).await
}

/// [`start_quiescent`] with handlers registered (they never run: a test
/// registers one to prove it).
pub async fn start_quiescent_with_jobs(config: &Config, registry: JobRegistry) -> Result<RunningServer, ServerError> {
    start_full(config, None, registry, PoolConfig::default(), true).await
}

/// The startup sequence. Without `origin`, `config` counts as built in code:
/// every setting reports `default` and `PATCH` writes `forgebase.toml` next
/// to the data directory.
async fn start_full(
    config: &Config,
    origin: Option<(ConfigOrigin, ConfigSources)>,
    registry: JobRegistry,
    pool: PoolConfig,
    quiescent: bool,
) -> Result<RunningServer, ServerError> {
    // A configuration built in code states its backend on purpose; from the
    // loader, only a layer other than the defaults does (FB-148).
    let explicit = origin
        .as_ref()
        .is_none_or(|(_, sources)| database::backend_is_explicit(sources));
    let (origin, sources) = origin.unwrap_or_else(|| (ConfigOrigin::embedded(config), ConfigSources::default()));
    let system_config = Arc::new(forgebase_api::features::SystemConfig::new(
        config.clone(),
        origin,
        sources,
    ));
    let layout = DataLayout::new(&config.data_dir);
    layout.ensure().map_err(|source| ServerError::Io {
        path: layout.root().to_path_buf(),
        source,
    })?;
    let lock = DataDirLock::acquire(&layout)?;
    // ADR-0033: certificate files are read and checked (key matches) before
    // anything else opens; ACME loads its sealed state.
    let tls = tls::TlsSetup::prepare(config, &layout)?;

    let primary = database::open_owned(config, explicit, &layout, database::Create::Yes).await?;
    let backend = primary.marker();
    let (db, engine) = primary.into_parts();
    let started = start_on(
        config,
        system_config,
        &layout,
        &lock,
        (db.clone(), backend, engine.clone()),
        registry,
        pool,
        tls,
        quiescent,
    )
    .await;
    match started {
        Ok(mut server) => {
            server.lock = Some(lock);
            Ok(server)
        }
        Err(e) => {
            db.close().await;
            if let Some(engine) = &engine
                && let Err(stop) = engine.stop().await
            {
                tracing::warn!(error = %stop, "stopping PostgreSQL failed");
            }
            drop(lock);
            Err(e)
        }
    }
}

/// What [`start_on`] reports about the database to the API.
fn database_runtime(
    config: &Config,
    layout: &DataLayout,
    backend: BackendMarker,
    engine: Option<&Arc<forgebase_pgengine::EngineHandle>>,
    db: &Database,
) -> forgebase_api::DatabaseRuntime {
    let pg = &config.database.postgres;
    let location = match backend {
        BackendMarker::Sqlite => None,
        BackendMarker::PostgresManaged => Some(database::engine_layout(layout).pgdata().display().to_string()),
        BackendMarker::PostgresExternal => Some(db.location()),
    };
    let report = engine.cloned().map(|engine| {
        let f: Arc<dyn Fn() -> forgebase_api::EngineReport + Send + Sync> = Arc::new(move || {
            let h = engine.health();
            forgebase_api::EngineReport {
                state: h.state.as_str(),
                version: engine.version().text.clone(),
                pid: h.pid,
                restarts: h.restarts,
                last_error: h.last_error,
                socket_dir: engine.socket_dir().map(|d| d.display().to_string()),
                tcp: engine.tcp_addr().map(|a| a.to_string()),
                warnings: engine.warnings().to_vec(),
            }
        });
        f
    });
    forgebase_api::DatabaseRuntime {
        mode: backend.mode().map(forgebase_core::config::PostgresMode::as_str),
        location,
        extensions: if backend == BackendMarker::Sqlite {
            Vec::new()
        } else {
            pg.extensions
                .list()
                .iter()
                .map(|(f, e, m)| (*f, *e, m.as_str()))
                .collect()
        },
        engine: report,
    }
}

/// The startup sequence after the database is open (steps 3 to 7). The
/// caller closes the database and stops the engine when this fails.
#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
async fn start_on(
    config: &Config,
    system_config: Arc<forgebase_api::features::SystemConfig>,
    layout: &DataLayout,
    lock: &DataDirLock,
    (db, backend, engine): (Database, BackendMarker, Option<Arc<forgebase_pgengine::EngineHandle>>),
    registry: JobRegistry,
    pool: PoolConfig,
    tls: Option<tls::TlsSetup>,
    quiescent: bool,
) -> Result<RunningServer, ServerError> {
    let tls = tls.map(Arc::new);
    let migrations = match db.migrate(Some(&layout.backups_dir())).await {
        Ok(report) => report,
        Err(e) => {
            db.close().await;
            return Err(e.into());
        }
    };
    // FB-153: role and login management and the extensions API run as the
    // bundled engine's superuser; an external server's own role otherwise.
    db.set_admin_url(
        engine
            .as_ref()
            .map(|e| forgebase_db::SecretString::new(e.url(forgebase_pgengine::Role::Super).expose())),
    );
    // FB-171 (ADR-0029 §3.5): the database sets. The router reads the
    // topology from the migrated control database and opens every other
    // set and the replicas; SQLite has one fixed set.
    // FB-175: local replicas start after the primary; the router opens them.
    let replicas = match replicas::Replicas::open(config, layout.root(), engine.clone()).await {
        Ok(r) => r,
        Err(e) => {
            db.close().await;
            return Err(e);
        }
    };
    let router = match open_sets(&db, &replicas, layout).await {
        Ok(r) => r,
        Err(e) => {
            replicas.stop().await;
            db.close().await;
            return Err(e);
        }
    };
    // Extensions after the migrations (ADR-0029 §1.1): the bundled engine
    // creates them as forgebase_super, an external server as its own role.
    let extensions = forgebase_db::ExtensionSetup {
        modes: config.database.postgres.extensions,
        admin_url: engine
            .as_ref()
            .map(|e| forgebase_db::SecretString::new(e.url(forgebase_pgengine::Role::Super).expose())),
        reader_role: engine
            .as_ref()
            .map(|_| forgebase_pgengine::Role::Reader.name().to_owned()),
    };
    if let Err(e) = db.setup_extensions(&extensions).await {
        db.close().await;
        return Err(e.into());
    }
    let instance = db.ensure_instance().await?;

    let (auth, policy, job_access) = match open_auth(&db, &router, layout, config).await {
        Ok(a) => a,
        Err(e) => {
            db.close().await;
            return Err(e);
        }
    };
    // FB-173: in-doubt two-phase transactions are resolved at start and
    // every minute (PostgreSQL only; SQLite never prepares).
    let recovery = (db.kind() == forgebase_db::BackendKind::Postgres)
        .then(|| forgebase_db::twophase::spawn_recovery(router.clone(), forgebase_db::twophase::RECOVERY_INTERVAL));
    shards::prepare_data(auth.data()).await;
    // FB-179 (ADR-0029 §2.9): finish a cluster restore: schema reconcile of
    // every set, every replica marked `needs_rebuild`. Replica rebuilds go
    // through the replicas service (FB-168), not here.
    if let Ok(Some(_)) = forgebase_backup::topology::read_pending(layout) {
        let backups = layout.backups_dir();
        match forgebase_backup::topology::complete_restore(
            &db,
            layout,
            forgebase_backup::topology::CompleteOptions {
                router: Some(&router),
                backups_dir: Some(&backups),
                rebuilder: None,
            },
        )
        .await
        {
            Ok(r) => tracing::info!(report = ?r, "cluster restore completed"),
            Err(e) => {
                tracing::warn!(error = %e, "cluster restore could not be completed; it is retried at the next start")
            }
        }
    }
    // First-run setup (FB-90): a one-time token while no administrator
    // exists. The caller prints it; it never goes to the log.
    let setup_token = match auth.begin_setup().await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(error = %e, "cannot check for an administrator; setup link not issued");
            None
        }
    };
    if setup_token.is_some() {
        tracing::info!("setup mode: no administrator yet; the setup link is printed to standard output");
    }
    // Entitlements and quotas (FB-24): pipeline steps 4 and 6.
    let entitlements = match EntitlementService::open(Arc::new(db.clone()), policy.clone(), Arc::new(SystemClock)).await
    {
        Ok(e) => e,
        Err(e) => {
            db.close().await;
            return Err(ServerError::Entitlements(e.to_string()));
        }
    };
    policy.set_hooks(entitlements.hooks());
    // Credits (FB-73): prepaid balances that pay for usage events.
    let credits = match CreditService::open(
        Arc::new(db.clone()),
        policy.clone(),
        entitlements.clone(),
        Arc::new(SystemClock),
    )
    .await
    {
        Ok(c) => c,
        Err(e) => {
            db.close().await;
            return Err(ServerError::Credits(e.to_string()));
        }
    };
    // Product customization (FB-74): domains, branding, email, agreements.
    let customize = match CustomizeService::open(
        Arc::new(db.clone()),
        policy.clone(),
        Arc::new(forgebase_auth::SystemClock),
        CustomizeConfig::default(),
    )
    .await
    {
        Ok(c) => c,
        Err(e) => {
            db.close().await;
            return Err(ServerError::Customize(e.to_string()));
        }
    };
    // One authorizer — the policy engine — for the data core and the KV engine.
    let authorizer: Arc<dyn Authorizer> = policy.authorizer();
    let kv_cfg = KvConfig {
        tenant_max_bytes: usize::try_from(config.kv.tenant_max_bytes).unwrap_or(usize::MAX),
        ..KvConfig::default()
    };
    let kv = match KvService::open_routed(kv_cfg, authorizer, router.clone()).await {
        Ok(kv) => kv,
        Err(e) => {
            db.close().await;
            return Err(e.into());
        }
    };
    kv.start_background();
    // The config file a restore drill's child process loads (ADR-0041).
    let system_config_file = system_config.target_file();
    let opts = RouterOptions {
        request_timeout: Duration::from_secs(config.server.request_timeout_secs),
        ops_access: config.server.ops_access,
        system_config: Some(system_config),
        ..RouterOptions::default()
    };
    // One data service, built by the auth service with the Argon2id hasher
    // and the policy engine as its authorizer; the auth layer turns
    // credentials into each request's AccessContext, the tenancy layer adds
    // realm, organization, tenant, roles and attributes.
    let realtime = match forgebase_realtime::RealtimeService::start(
        auth.data().clone(),
        Some(auth.clone()),
        forgebase_realtime::RealtimeConfig {
            max_connections_per_principal: usize::try_from(config.realtime.max_connections_per_principal)
                .unwrap_or(usize::MAX),
            // FB-91: off, the outbox is still written and pruned, nothing is delivered.
            dispatch: config.realtime.enabled && !quiescent,
            ..forgebase_realtime::RealtimeConfig::default()
        },
        None,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            db.close().await;
            return Err(e.into());
        }
    };
    // Server functions (FB-60), unless `[functions] enabled = false`: then
    // no data hooks, no after-hook dispatcher, no function jobs.
    let functions = if config.functions.enabled {
        match open_functions(&auth, &kv).await {
            Ok(f) => Some(f),
            Err(e) => {
                realtime.shutdown();
                db.close().await;
                return Err(e);
            }
        }
    } else {
        None
    };
    // Billing (FB-70): on by default, `[billing] enabled = false` turns it off.
    let billing = if config.billing.enabled {
        match open_billing(&db, &auth, &policy, &entitlements, &credits).await {
            Ok(b) => {
                // The inbound webhook inspector (ADR-0032).
                b.set_webhook_log(forgebase_billing::WebhookLogConfig {
                    retention_days: config.billing.webhook_log_retention_days.max(1),
                    trust_forwarded_for: config.auth.trust_forwarded_for,
                    ..forgebase_billing::WebhookLogConfig::default()
                });
                Some(b)
            }
            Err(e) => {
                realtime.shutdown();
                db.close().await;
                return Err(e);
            }
        }
    } else {
        None
    };
    // Durable topics and queues (FB-63), unless `[queue] enabled = false`.
    let queue = if config.queue.enabled {
        match open_queue(&auth, &router, config, &entitlements, functions.as_ref()).await {
            Ok(q) => Some(q),
            Err(e) => {
                realtime.shutdown();
                if let Some(f) = &functions {
                    f.shutdown();
                }
                db.close().await;
                return Err(e);
            }
        }
    } else {
        None
    };
    let resp_auth = forgebase_resp::KeyAuthenticator::new(auth.clone());
    let (fn_auth, fn_policy) = (auth.clone(), policy.clone());
    let master_key_path = auth.config().master_key_path.clone();
    // Kept for the mail providers below (the auth service moves into the state).
    let (mail_http, mail_key_path) = (auth.http_client(), auth.config().master_key_path.clone());
    let state = AppState::from_data(db.clone(), layout.clone(), auth.data().clone())
        .with_auth(auth)
        .with_policy(policy)
        .with_database_runtime(database_runtime(config, layout, backend, engine.as_ref(), &db));
    let state = if config.realtime.enabled {
        state.with_realtime(realtime.clone())
    } else {
        state
    };
    let jobs = JobService::new(state.data().clone());
    // FB-172: jobs resolve the tenant's set; one claim loop per set.
    jobs.attach_router(router.clone());
    // FB-171: `ON DELETE` actions of shared records on every set, as jobs.
    state
        .data()
        .set_relation_fanout(Arc::new(shards::JobFanout::new(jobs.clone())));
    let registry = shards::register_jobs(registry, state.data().clone());
    // FB-175: the `database.*` topology operations.
    let registry = replicas::register_jobs(registry, replicas.clone());
    // FB-174: tenant moves, shard drains and rebalancing.
    let moves = moves::Moves::new(moves::MovesConfig {
        router: router.clone(),
        jobs: jobs.clone(),
        kv: Some(kv.clone()),
        queue: queue.clone(),
        keyring: replicas.keyring().clone(),
        database: config.database.clone(),
        control_url: replicas.control_admin_url(),
        shards: Some(replicas.shards().clone()),
        options: moves::options_from(config),
    });
    let registry = moves::register_jobs(registry, moves.clone());
    // ADR-0041: restore drills run as jobs, in a child process of this executable.
    let drill_runner = forgebase_backup::drill::DrillRunner::for_current_exe(
        layout.root(),
        Some(system_config_file.as_path()).filter(|p| p.is_file()),
    );
    let registry = match &drill_runner {
        Some(runner) => forgebase_backup::drill::register_jobs(
            registry,
            runner.clone(),
            forgebase_backup::drill::DrillStore::new(layout),
        ),
        None => registry,
    };
    replicas
        .attach(router.clone(), jobs.clone(), state.data().clone())
        .await;
    replicas.attach_moves(moves.clone());
    if router.control().database().kind() == forgebase_db::BackendKind::Postgres {
        moves.spawn_stats();
    }
    // FB-142: job visibility (channels included) and `engine:read_jobs:all`.
    jobs.attach_access(job_access);
    let jp = fn_policy.clone();
    jobs.attach_permissions(Arc::new(move |ctx, key| jp.has(ctx, key, ctx.org_id.as_deref())));
    // FB-64: live progress and state on the realtime channels `jobs:<id>`.
    if config.realtime.enabled {
        jobs.attach_realtime(realtime.clone());
    }
    entitlements.attach_jobs(jobs.clone());
    if let Err(e) = entitlements.ensure_schedule().await {
        tracing::warn!(error = %e, "entitlement maintenance schedule not created");
    }
    let registry = forgebase_entitlements::register_jobs(registry, entitlements.clone());
    let registry = match &functions {
        Some(f) => forgebase_functions::register_jobs(registry, f.clone()),
        None => registry,
    };
    // FB-164: `queue.convert` moves deliveries between pgmq and portable rows.
    let registry = match &queue {
        Some(q) => forgebase_queue::register_jobs(registry, q.clone()),
        None => registry,
    };
    // Object storage (FB-30): local directory or S3-compatible driver, unless
    // `[storage] enabled = false`.
    let storage = if config.storage.enabled {
        match open_storage(
            &db,
            &router,
            layout,
            config,
            state.data().clone(),
            &entitlements,
            master_key_path,
        )
        .await
        {
            Ok(s) => Some(s),
            Err(e) => {
                db.close().await;
                return Err(e);
            }
        }
    } else {
        None
    };
    let registry = match &storage {
        Some(storage) => {
            storage.attach_jobs(jobs.clone());
            // FB-64: results too large for the job row go to object storage.
            match StorageResults::open(storage.clone()).await {
                Ok(store) => jobs.attach_result_store(Arc::new(store)),
                Err(e) => tracing::warn!(error = %e, "job results bucket not available; large results are rejected"),
            }
            if let Err(e) = storage.ensure_schedule().await {
                tracing::warn!(error = %e, "storage reconcile schedule not created");
            }
            forgebase_storage::register_jobs(registry, storage.clone())
        }
        None => registry,
    };
    // FB-91: a disabled module's system schedule is paused (nothing would
    // claim its jobs), and resumed when the module is on again.
    set_schedule_enabled(&jobs, forgebase_storage::RECONCILE_SCHEDULE, storage.is_some()).await;
    credits.attach_jobs(jobs.clone());
    if let Err(e) = credits.ensure_schedule().await {
        tracing::warn!(error = %e, "credit maintenance schedule not created");
    }
    let registry = forgebase_credits::register_jobs(registry, credits.clone());
    let registry = match &billing {
        Some(b) => {
            b.attach_jobs(jobs.clone());
            if let Err(e) = b.ensure_schedule().await {
                tracing::warn!(error = %e, "billing maintenance schedule not created");
            }
            forgebase_billing::register_jobs(registry, b.clone())
        }
        None => registry,
    };
    set_schedule_enabled(&jobs, forgebase_billing::MAINTENANCE_SCHEDULE, billing.is_some()).await;
    customize.attach_jobs(jobs.clone());
    // Mail providers (ADR-0039): the HTTPS client (SSRF-guarded) and the master
    // key their stored, sealed settings need. A failure leaves the log provider.
    {
        let path = mail_key_path;
        match tokio::task::spawn_blocking(move || forgebase_auth::crypto::Keyring::load_or_create(&path)).await {
            Ok(Ok(keyring)) => {
                if let Err(e) = customize.attach_mail_runtime(mail_http, keyring).await {
                    tracing::warn!(error = %e, "mail providers not loaded");
                }
            }
            Ok(Err(e)) => tracing::warn!(error = %e, "mail providers not loaded: master key unavailable"),
            Err(e) => tracing::warn!(error = %e, "mail providers not loaded"),
        }
    }
    let registry = forgebase_customize::register_jobs(registry, customize.clone());
    // Vector search (FB-50), unless `[vector] enabled = false`.
    let vector = if config.vector.enabled {
        match VectorService::open_routed(
            state.data().clone(),
            router.clone(),
            layout.indexes_dir(),
            VectorConfig::default(),
        )
        .await
        {
            Ok(v) => Some(v),
            Err(e) => {
                db.close().await;
                return Err(e.into());
            }
        }
    } else {
        None
    };
    // FB-162: `vector.convert` moves vectors between pgvector and portable rows.
    let registry = match &vector {
        Some(v) => forgebase_vector::register_jobs(registry, v.clone()),
        None => registry,
    };
    // FB-163: the graph service the API mounts, routed per tenant set, with
    // the `graph.materialise` job (ADR-0029 §1.5).
    let graph = forgebase_graph::GraphService::new(state.data().clone()).with_jobs(Arc::new(GraphJobs(jobs.clone())));
    graph.attach_router(router.clone());
    let registry = registry.register(forgebase_graph::MATERIALISE_JOB, {
        let graph = graph.clone();
        move |ctx: forgebase_jobs::JobContext| {
            let graph = graph.clone();
            async move {
                let tenant = ctx
                    .payload()
                    .get("tenant")
                    .and_then(|t| t.as_str())
                    .unwrap_or_default()
                    .to_owned();
                match graph.materialise(&tenant).await {
                    Ok(r) => Ok(serde_json::to_value(r).unwrap_or_default()),
                    Err(e @ forgebase_graph::GraphError::CypherUnsupported) => {
                        Err(forgebase_jobs::HandlerError::fatal(e.to_string()))
                    }
                    Err(e) => Err(forgebase_jobs::HandlerError::retry(e.to_string())),
                }
            }
        }
    });
    // FB-93: the engines JavaScript functions reach through `ctx`. A module
    // switched off (FB-91) leaves its `ctx` service unset.
    if let Some(f) = &functions {
        f.attach_services(function_services::services(
            vector.as_ref(),
            storage.as_ref(),
            queue.as_ref(),
            &jobs,
            &realtime,
            &fn_auth,
            &fn_policy,
        ));
    }
    let mut extensions = vec![
        forgebase_jobs::extension(jobs.clone()),
        forgebase_entitlements::extension(entitlements.clone()),
        forgebase_credits::extension(credits.clone()),
        forgebase_customize::extension(customize.clone()),
        forgebase_backup::extension({
            let service = forgebase_backup::BackupService::new(db.clone(), layout.clone())
                .with_drills(jobs.clone(), drill_runner.is_some() && !quiescent)
                .with_tls(config.backup.includes_tls(config.tls.mode));
            match database::pg_source(config, layout, backend, engine.as_deref(), None) {
                Some(src) => service.with_pg(src),
                None => service,
            }
        }),
    ];
    if let Some(v) = &vector {
        extensions.push(forgebase_vector::extension(v.clone()));
    }
    if let Some(f) = &functions {
        extensions.push(forgebase_functions::extension(f.clone()));
    }
    if let Some(s) = &storage {
        extensions.push(forgebase_storage::extension(s.clone()));
    }
    if let Some(b) = &billing {
        extensions.push(forgebase_billing::extension(b.clone()));
    }
    if let Some(q) = &queue {
        extensions.push(forgebase_queue::extension(q.clone()));
    }
    // ADR-0035: alerting engine and SLOs, unless `[alerts] enabled = false`.
    let alerts = if config.alerts.enabled {
        match open_alerts(&fn_auth, layout, config, &customize, queue.as_ref()).await {
            Ok(a) => Some(a),
            Err(e) => {
                db.close().await;
                return Err(e);
            }
        }
    } else {
        None
    };
    if let Some(a) = &alerts {
        extensions.push(forgebase_alerts::extension(a.clone()));
    }
    // ADR-0036: operations insights (slow statements, usage, function
    // profiles, audit search). The slow threshold applies to every set.
    let slow = Duration::from_millis(config.observability.insights.slow_query_ms);
    for set in router.sets() {
        forgebase_db::Backend::set_slow_query_threshold(set.database(), slow);
    }
    let insights = forgebase_insights::InsightsService::new(
        forgebase_insights::InsightsDeps {
            db: Arc::new(db.clone()),
            router: Some(router.clone()),
            data: state.data().clone(),
            auth: fn_auth.clone(),
            functions: functions.clone(),
            billing: billing.clone(),
            credits: Some(credits.clone()),
        },
        config.observability.insights.clone(),
    );
    extensions.push(forgebase_insights::extension(insights.clone()));
    let opts = RouterOptions {
        // SLO request counting.
        http_observers: alerts
            .iter()
            .map(forgebase_alerts::AlertService::http_observer)
            .collect(),
        extensions,
        graph: Some(graph.clone()),
        gates: vec![
            entitlements.rate_gate(),
            Arc::new(forgebase_customize::AgreementGate::new(customize.clone())),
        ],
        database_topology: replicas.ops(),
        ..opts
    };
    // ADR-0041: the drill gauges come from the history files on every scrape.
    let state = {
        let store = forgebase_backup::drill::DrillStore::new(layout);
        state.with_metrics_collector(move || {
            let store = store.clone();
            async move {
                let _ = tokio::task::spawn_blocking(move || forgebase_backup::drill::publish_metrics(&store)).await;
            }
        })
    };
    let state = state.with_metrics_collector(subsystem_gauges(
        jobs.clone(),
        storage.clone(),
        kv.clone(),
        vector.clone(),
        queue.clone(),
    ));
    // ADR-0033: the `tls` entry of /api/system/info and the expiry gauge.
    let state = match &tls {
        Some(t) => {
            let (status, gauge) = (t.clone(), t.clone());
            state
                .with_status_section("tls", move || status.status_json())
                .with_metrics_collector(move || {
                    let t = gauge.clone();
                    async move { t.refresh_gauge() }
                })
        }
        None => state.with_status_section("tls", || serde_json::json!({ "mode": "off" })),
    };
    let kv_routes = forgebase_kv::http::router(kv.clone(), usize::try_from(opts.max_body_bytes).unwrap_or(usize::MAX));
    let app = forgebase_api::router_with(state, opts, kv_routes, Some(forgebase_kv::http::openapi()));
    // Host-header routing of verified custom domains: outermost, before auth and tenancy.
    let app = app.layer(axum::middleware::from_fn_with_state(
        customize.clone(),
        forgebase_customize::host_router,
    ));

    // ADR-0033: HSTS on every HTTPS response, when configured.
    let app = match (
        &tls,
        tls::serve::hsts_value(config.tls.hsts_max_age_secs, config.tls.hsts_include_subdomains),
    ) {
        (Some(_), Some(v)) => tls::serve::with_hsts(app, v),
        _ => app,
    };
    let tls_configs = match &tls {
        Some(t) => match (t.http_server_config(), t.resp_server_config()) {
            (Ok(h), Ok(r)) => Some((h, r)),
            (Err(e), _) | (_, Err(e)) => {
                db.close().await;
                return Err(e.into());
            }
        },
        None => None,
    };
    if tls.is_some() && config.auth.trust_forwarded_for {
        tracing::warn!(
            "[auth] trust_forwarded_for is on with native TLS: correct only when every request still comes through a proxy you run that appends X-Forwarded-For; clients that connect directly can choose their own address"
        );
    }

    let bind = (config.server.host.as_str(), config.server.port);
    let listener = match TcpListener::bind(bind).await {
        Ok(l) => l,
        Err(source) => {
            db.close().await;
            return Err(ServerError::Bind {
                addr: format!("{}:{}", config.server.host, config.server.port),
                source,
            });
        }
    };
    let addr = listener.local_addr().map_err(ServerError::Serve)?;
    if !addr.ip().is_loopback() && config.server.ops_access == forgebase_core::OpsAccess::Public {
        tracing::warn!(%addr, "listening on a non-loopback address with ops_access = \"public\": anyone who reaches it can read /metrics and /api/system/*");
    }
    let resp = if config.resp.enabled {
        let r = &config.resp;
        let cfg = forgebase_resp::RespConfig {
            host: r.host.clone(),
            port: r.port,
            max_connections: usize::try_from(r.max_connections).unwrap_or(usize::MAX),
            max_bulk_bytes: usize::try_from(r.max_bulk_bytes).unwrap_or(usize::MAX),
            max_request_bytes: usize::try_from(r.max_bulk_bytes.saturating_mul(4)).unwrap_or(usize::MAX),
            idle_timeout: Duration::from_secs(r.idle_timeout_secs),
            shutdown_grace: Duration::from_secs(config.server.shutdown_timeout_secs),
            tls: match (&tls_configs, r.tls) {
                (Some((_, resp_tls)), true) => Some(resp_tls.clone()),
                _ => None,
            },
            ..forgebase_resp::RespConfig::default()
        };
        match forgebase_resp::start(cfg, Arc::new(forgebase_resp::LocalKv(kv.clone())), Arc::new(resp_auth)).await {
            Ok(h) => Some(h),
            Err(source) => {
                db.close().await;
                return Err(ServerError::Bind {
                    addr: format!("{}:{} (resp)", r.host, r.port),
                    source,
                });
            }
        }
    } else {
        None
    };
    // ADR-0033: the plain HTTP listener (redirects to HTTPS, http-01 answers).
    let plain = match &tls {
        Some(t) if t.config().plain_listener() => {
            let port = t.config().http_port;
            let pl = match TcpListener::bind((config.server.host.as_str(), port)).await {
                Ok(l) => l,
                Err(source) => {
                    if let Some(r) = resp {
                        r.shutdown().await;
                    }
                    db.close().await;
                    return Err(ServerError::Bind {
                        addr: format!("{}:{port} (http redirect)", config.server.host),
                        source,
                    });
                }
            };
            let plain_addr = pl.local_addr().map_err(ServerError::Serve)?;
            let target = tls::serve::RedirectTarget {
                public_url: config.server.public_url.clone().filter(|u| u.starts_with("https://")),
                https_port: addr.port(),
                domains: if config.tls.mode == forgebase_core::config::tls_config::TlsMode::Acme {
                    config.tls.acme.domains.clone()
                } else {
                    Vec::new()
                },
            };
            let router = tls::serve::plain_router(t.store().clone(), target);
            let (stop_tx, stop_rx) = oneshot::channel::<()>();
            let task = tokio::spawn(tls::serve::serve_plain(pl, router, stop_rx));
            t.status().set_http_listener(Some(plain_addr.to_string()));
            tracing::info!(%plain_addr, "plain HTTP listener started (redirects to HTTPS)");
            Some((plain_addr, stop_tx, task))
        }
        _ => None,
    };
    if let Some(t) = &tls {
        t.status().set_resp(resp.is_some() && config.resp.tls);
    }
    lock.record_owner(&LockOwner {
        pid: std::process::id(),
        addr: addr.to_string(),
        started_at: forgebase_core::now_rfc3339(),
    });

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let task = match &tls_configs {
        Some((http_tls, _)) => tokio::spawn(tls::serve::serve_tls(
            listener,
            tokio_rustls::TlsAcceptor::from(http_tls.clone()),
            app,
            shutdown_rx,
        )),
        None => tokio::spawn(async move {
            axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                })
                .await
        }),
    };
    let tls_tasks = tls.as_ref().filter(|_| !quiescent).map(|t| t.start_background());
    let upkeep = tokio::spawn(async {
        let handle = forgebase_observability::metrics_handle();
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        loop {
            tick.tick().await;
            handle.run_upkeep();
        }
    });

    // ADR-0041: a quiescent server starts none of the workers below. Nothing
    // claims a job, fires a schedule, delivers a push or a webhook, sends an
    // alert, runs a function hook or renews a certificate.
    let (job_pool, pgcron) = if quiescent {
        (None, None)
    } else {
        let job_pool = forgebase_jobs::start_pool(jobs.clone(), registry, pool);
        // FB-165: schedules pg_cron can express fire from the database; the rest
        // (and everything on SQLite or without pg_cron) stay on the Rust tick.
        let pgcron = forgebase_jobs::attach_pgcron_sets(&jobs, &db).await;
        if drill_runner.is_some() {
            forgebase_backup::drill::sync_schedule(&jobs, config.backup.drill_schedule.as_deref()).await;
        } else if config.backup.drill_schedule.is_some() {
            tracing::warn!(
                "[backup] drill_schedule is set but this server is not the forgebase command: no drill is scheduled"
            );
        }
        if let Some(v) = &vector {
            v.start_background();
        }
        if let Some(f) = &functions {
            f.start_background();
        }
        if let Some(q) = &queue {
            q.start_background();
        }
        if let Some(a) = &alerts {
            a.start_background();
        }
        insights.start_background();
        (Some(job_pool), pgcron)
    };

    tracing::info!(
        %addr,
        tls = config.tls.mode.as_str(),
        data_dir = %layout.root().display(),
        backend = backend.as_str(),
        instance_id = %instance.instance_id,
        applied_migrations = migrations.applied.len(),
        disabled = ?forgebase_core::features::disabled(config).iter().map(|f| f.id).collect::<Vec<_>>(),
        quiescent,
        "forgebase started"
    );
    Ok(RunningServer {
        addr,
        db,
        instance,
        migrations,
        shutdown_tx: Some(shutdown_tx),
        task: Some(task),
        upkeep,
        jobs,
        job_pool,
        pgcron,
        kv,
        vector,
        resp,
        realtime,
        entitlements,
        functions,
        credits,
        billing,
        customize,
        queue,
        alerts,
        insights,
        shutdown_timeout: Duration::from_secs(config.server.shutdown_timeout_secs),
        lock: None,
        setup_token,
        backend,
        engine,
        router,
        recovery,
        replicas,
        tls,
        tls_tasks,
        plain,
    })
}

/// Opens the database sets on the migrated control database (FB-171), then
/// migrates every shard (FB-173: control first, at most four shards at a
/// time; a shard that fails stays `migrating` and its tenants get `503
/// shard_migrating`).
async fn open_sets(
    db: &Database,
    replicas: &Arc<replicas::Replicas>,
    layout: &DataLayout,
) -> Result<forgebase_db::DbRouter, ServerError> {
    // FB-175: the connector opens local replicas too; [database.replicas]
    // and [database.sharding] become the router's options.
    let router = replicas.open_router(db).await?;
    if router.is_sharded() {
        let report = forgebase_db::shards::migrate_sets(
            &router,
            Some(&layout.backups_dir()),
            forgebase_db::migrations::multi::MultiOptions::default(),
        )
        .await;
        for f in report.failed() {
            tracing::warn!(set = %f.id, outcome = ?f.outcome, "database set not migrated; its tenants get 503 shard_migrating");
        }
    }
    Ok(router)
}

/// Opens the policy service (FB-21), then the auth service (FB-20) and the
/// data service they share, with the policy engine as the authorizer.
/// The functions engine on the auth service's data service: secrets sealed
/// with the data directory's master key, fetch over FB-25's TLS settings.
/// The queue service on the auth service's data service: webhook signing
/// secrets sealed with the data directory's master key, metering through the
/// quota engine, push to server functions and SSRF-safe webhooks.
/// A `/metrics` collector for the subsystem gauges (jobs, storage, vector, KV,
/// queue). Labels are fixed vocabularies only: no per-record, per-user or
/// per-queue label. A subsystem whose read fails keeps its last values.
fn subsystem_gauges(
    jobs: JobService,
    storage: Option<forgebase_storage::StorageService>,
    kv: KvService,
    vector: Option<VectorService>,
    queue: Option<forgebase_queue::QueueService>,
) -> impl Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync + 'static {
    use forgebase_data::AccessContext;
    use forgebase_observability::names;
    move || {
        let (jobs, storage, kv, vector, queue) =
            (jobs.clone(), storage.clone(), kv.clone(), vector.clone(), queue.clone());
        Box::pin(async move {
            let sys = AccessContext::system();
            match jobs.stats(&sys).await {
                Ok(s) => {
                    for (status, n) in &s.totals {
                        metrics::gauge!(names::JOBS, "status" => status.clone()).set(*n as f64);
                    }
                }
                Err(e) => tracing::debug!(error = %e, "job gauges not refreshed"),
            }
            if let Some(storage) = storage {
                match storage.stats(&sys).await {
                    Ok(s) => {
                        metrics::gauge!(names::STORAGE_BYTES, "kind" => "logical").set(s.logical_bytes as f64);
                        metrics::gauge!(names::STORAGE_BYTES, "kind" => "stored").set(s.stored_bytes as f64);
                        metrics::gauge!(names::STORAGE_OBJECTS, "kind" => "current").set(s.objects as f64);
                        metrics::gauge!(names::STORAGE_OBJECTS, "kind" => "versions").set(s.versions as f64);
                    }
                    Err(e) => tracing::debug!(error = %e, "storage gauges not refreshed"),
                }
            }
            if let Some(vector) = vector {
                let v = vector.totals();
                metrics::gauge!(names::VECTOR_INDEXES, "state" => "ready")
                    .set(v.indexes.saturating_sub(v.building) as f64);
                metrics::gauge!(names::VECTOR_INDEXES, "state" => "building").set(v.building as f64);
                metrics::gauge!(names::VECTOR_INDEXED_VECTORS).set(v.indexed_vectors as f64);
                metrics::gauge!(names::VECTOR_INDEX_MEMORY_BYTES).set(v.memory_bytes as f64);
            }
            match kv.stats(&sys) {
                Ok(s) => {
                    metrics::gauge!(names::KV_KEYS).set(s.keys as f64);
                    metrics::gauge!(names::KV_MEMORY_BYTES).set(s.used_memory_bytes as f64);
                }
                Err(e) => tracing::debug!(error = %e, "kv gauges not refreshed"),
            }
            if let Some(q) = queue {
                match q.backlog().await {
                    Ok(b) => {
                        metrics::gauge!(names::QUEUE_DELIVERIES, "state" => "ready").set(b.ready as f64);
                        metrics::gauge!(names::QUEUE_DELIVERIES, "state" => "delayed").set(b.delayed as f64);
                        metrics::gauge!(names::QUEUE_DELIVERIES, "state" => "in_flight").set(b.in_flight as f64);
                        metrics::gauge!(names::QUEUE_MESSAGES).set(b.messages as f64);
                    }
                    Err(e) => tracing::debug!(error = %e, "queue gauges not refreshed"),
                }
            }
        })
    }
}

/// Pauses or resumes the system schedule `name` (FB-91): a disabled module's
/// schedule would enqueue jobs nobody claims. A missing schedule is fine.
async fn set_schedule_enabled(jobs: &JobService, name: &str, enabled: bool) {
    let sys = forgebase_data::AccessContext::system();
    let list = match jobs.schedules(&sys).await {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(error = %e, schedule = name, "schedules not readable");
            return;
        }
    };
    for s in list.iter().filter(|s| s.name == name && s.enabled != enabled) {
        let patch = forgebase_jobs::SchedulePatch {
            enabled: Some(enabled),
            ..forgebase_jobs::SchedulePatch::default()
        };
        match jobs.update_schedule(&sys, &s.id, patch).await {
            Ok(_) => tracing::info!(schedule = name, enabled, "module schedule updated"),
            Err(e) => tracing::warn!(error = %e, schedule = name, "module schedule not updated"),
        }
    }
}

async fn open_queue(
    auth: &forgebase_auth::AuthService,
    router: &forgebase_db::DbRouter,
    config: &Config,
    entitlements: &EntitlementService,
    functions: Option<&FunctionService>,
) -> Result<forgebase_queue::QueueService, ServerError> {
    let qc = &config.queue;
    let path = auth.config().master_key_path.clone();
    let keyring = tokio::task::spawn_blocking(move || forgebase_auth::crypto::Keyring::load_or_create(&path))
        .await
        .map_err(|e| ServerError::Queue(e.to_string()))?
        .map_err(|e| ServerError::Queue(e.to_string()))?;
    let secs = |s: u64| i64::try_from(s.saturating_mul(1000)).unwrap_or(i64::MAX);
    let settings = forgebase_queue::QueueSettings {
        max_message_bytes: i64::try_from(qc.max_message_bytes).unwrap_or(i64::MAX),
        default_retention_ms: secs(qc.default_retention_secs),
        max_retention_ms: secs(qc.max_retention_secs),
        default_dedup_window_ms: secs(qc.dedup_window_secs),
        max_pull_wait_ms: i64::try_from(qc.max_pull_wait_ms).unwrap_or(i64::MAX),
        poll_interval: Duration::from_millis(qc.poll_interval_ms),
        push_timeout: Duration::from_secs(qc.push_timeout_secs),
        push_concurrency: usize::try_from(qc.push_concurrency).unwrap_or(8),
        allow_http_push: qc.allow_http_push,
        dead_letter_retention_ms: secs(qc.dead_letter_retention_days.saturating_mul(86_400)),
        // ADR-0031: the push attempt log and test sends.
        push_response_body_bytes: usize::try_from(qc.push_response_body_bytes).unwrap_or(4096),
        push_attempts_per_subscription: i64::try_from(qc.push_attempts_per_subscription).unwrap_or(1000),
        push_attempts_retention_ms: secs(qc.push_attempts_retention_secs),
        push_test_sends_per_minute: u32::try_from(qc.push_test_sends_per_minute).unwrap_or(10),
        ..forgebase_queue::QueueSettings::default()
    };
    let tls = auth.tls_settings().map_err(|e| ServerError::Queue(e.to_string()))?;
    let fetcher = forgebase_functions::Fetcher::new(
        &tls,
        forgebase_functions::FetchConfig {
            timeout: settings.push_timeout,
            max_request_bytes: usize::try_from(qc.max_message_bytes).unwrap_or(usize::MAX) * 2 + 64 * 1024,
            ..forgebase_functions::FetchConfig::default()
        },
    )
    .map_err(|e| ServerError::Queue(e.to_string()))?;
    let queue = forgebase_queue::QueueService::new(auth.data().clone(), keyring, settings);
    // FB-172: topics and their deliveries live on the tenant's set.
    queue.attach_router(router.clone());
    queue.attach_entitlements(entitlements.clone());
    // Without functions, push subscriptions to a function fail delivery.
    if let Some(f) = functions {
        queue.attach_functions(f.clone());
    }
    queue.set_fetcher(fetcher);
    Ok(queue)
}

/// The alerting engine (ADR-0035): notifier secrets sealed with the data
/// directory's master key, webhook/Slack/PagerDuty calls through the
/// SSRF-safe client, email through customization, queue notifiers and
/// signals through the queue service.
async fn open_alerts(
    auth: &forgebase_auth::AuthService,
    layout: &DataLayout,
    config: &Config,
    customize: &CustomizeService,
    queue: Option<&forgebase_queue::QueueService>,
) -> Result<forgebase_alerts::AlertService, ServerError> {
    let path = auth.config().master_key_path.clone();
    let keyring = tokio::task::spawn_blocking(move || forgebase_auth::crypto::Keyring::load_or_create(&path))
        .await
        .map_err(|e| ServerError::Alerts(e.to_string()))?
        .map_err(|e| ServerError::Alerts(e.to_string()))?;
    let tls = auth.tls_settings().map_err(|e| ServerError::Alerts(e.to_string()))?;
    let fetcher = forgebase_functions::Fetcher::new(
        &tls,
        forgebase_functions::FetchConfig {
            timeout: Duration::from_secs(15),
            ..forgebase_functions::FetchConfig::default()
        },
    )
    .map_err(|e| ServerError::Alerts(e.to_string()))?;
    forgebase_alerts::AlertService::open(forgebase_alerts::AlertDeps {
        db: auth.data().backend().clone(),
        keyring,
        clock: Arc::new(forgebase_jobs::SystemClock),
        fetcher: Some(fetcher),
        customize: Some(customize.clone()),
        queue: queue.cloned(),
        layout: Some(layout.clone()),
        metrics: Arc::new(forgebase_alerts::ProcessMetrics),
        settings: forgebase_alerts::AlertSettings::from_config(&config.alerts),
        public_url: config.server.public_url.clone(),
    })
    .await
    .map_err(|e| ServerError::Alerts(e.to_string()))
}

async fn open_functions(auth: &forgebase_auth::AuthService, kv: &KvService) -> Result<FunctionService, ServerError> {
    let path = auth.config().master_key_path.clone();
    let keyring = tokio::task::spawn_blocking(move || forgebase_auth::crypto::Keyring::load_or_create(&path))
        .await
        .map_err(|e| ServerError::Functions(e.to_string()))??;
    let tls = auth.tls_settings()?;
    let fetcher = forgebase_functions::Fetcher::new(&tls, forgebase_functions::FetchConfig::default())
        .map_err(|e| ServerError::Functions(e.to_string()))?;
    let cfg = FunctionsConfig {
        fetcher: Some(fetcher),
        ..FunctionsConfig::default()
    };
    FunctionService::open(auth.data().clone(), Some(kv.clone()), keyring, cfg)
        .await
        .map_err(|e| ServerError::Functions(e.to_string()))
}

/// The billing service (FB-70): provider secrets sealed with the data
/// directory's master key, provider APIs over FB-25's TLS client.
async fn open_billing(
    db: &Database,
    auth: &forgebase_auth::AuthService,
    policy: &forgebase_policy::PolicyService,
    entitlements: &EntitlementService,
    credits: &CreditService,
) -> Result<forgebase_billing::BillingService, ServerError> {
    let path = auth.config().master_key_path.clone();
    let keyring = tokio::task::spawn_blocking(move || forgebase_auth::crypto::Keyring::load_or_create(&path))
        .await
        .map_err(|e| ServerError::Billing(e.to_string()))??;
    let http = forgebase_auth::net::HttpsClient::new(&auth.tls_settings()?)?;
    forgebase_billing::BillingService::open(
        Arc::new(db.clone()),
        forgebase_billing::BillingDeps {
            policy: policy.clone(),
            entitlements: entitlements.clone(),
            credits: Arc::new(credits.clone()),
            clock: Arc::new(SystemClock),
            keyring,
            http: Arc::new(http),
        },
    )
    .await
    .map_err(|e| ServerError::Billing(e.to_string()))
}

async fn open_auth(
    db: &Database,
    router: &forgebase_db::DbRouter,
    layout: &DataLayout,
    config: &Config,
) -> Result<
    (
        forgebase_auth::AuthService,
        forgebase_policy::PolicyService,
        forgebase_jobs::JobAccess,
    ),
    ServerError,
> {
    let mut cfg = forgebase_auth::AuthConfig::for_data_dir(layout.root());
    let scheme = if config.tls.mode.is_on() { "https" } else { "http" };
    cfg.public_url = format!("{scheme}://{}:{}", config.server.host, config.server.port);
    // FB-80: `[auth]` deployment settings.
    cfg.trust_forwarded_for = config.auth.trust_forwarded_for;
    cfg.allow_private_outbound = config.auth.allow_private_outbound;
    let public_url = cfg.public_url.clone();
    // FB-171: control's set handle (its replica reads route per §2.5).
    // FIX-15: the live one, so replicas added or removed while the server
    // runs are used (or dropped) by auth, policy and the data core at once.
    let backend: Arc<dyn forgebase_db::Backend> = Arc::new(router.control_backend());
    let policy = forgebase_policy::PolicyService::open(backend.clone())
        .await
        .map_err(|e| ServerError::Policy(e.to_string()))?;
    // FB-64: subscribing to the job channels (`jobs:<id>`) needs permission
    // to read jobs, and only the system publishes there.
    let channels = forgebase_jobs::JobChannelAuthorizer::new(policy.authorizer());
    let job_access = channels.access();
    let authorizer: Arc<dyn Authorizer> = Arc::new(channels);
    let auth = forgebase_auth::AuthService::open_with_data(backend, authorizer, layout.root(), cfg, None).await?;
    policy.attach_data(auth.data().clone());
    // FIX-15: new environments are placed on a set (ADR-0029 §3.4).
    policy.attach_router(router.clone());
    // FB-173: schema changes on every set, and the native-policy startup
    // sync below covers every shard. FB-171: records route per call.
    auth.data().attach_shards(router.clone());
    policy.attach_mailer(auth.mailer().clone(), public_url);
    // FB-25: identities act in their own realm; suspensions set through the
    // auth API reach the policy engine's cached model.
    policy
        .attach_realms(auth.realms().clone())
        .await
        .map_err(|e| ServerError::Policy(e.to_string()))?;
    // FB-152: ForgeBase rules also become native PostgreSQL policies. Only
    // an explicit `enforce` refuses to start without them.
    if db.kind() == forgebase_db::BackendKind::Postgres {
        use forgebase_core::config::NativeRls;
        use forgebase_policy::native::NativeMode;
        let mode = match config.database.postgres.native_rls {
            NativeRls::Off => NativeMode::Off,
            NativeRls::Logins => NativeMode::Logins,
            NativeRls::Enforce => NativeMode::Enforce,
        };
        if let Err(e) = policy.enable_native_rls(mode).await {
            if mode == NativeMode::Enforce {
                return Err(ServerError::Policy(e.to_string()));
            }
            tracing::warn!(error = %e, "native row-level security is not available");
        }
    }
    let p = policy.clone();
    auth.set_change_hook(Arc::new(move || {
        let p = p.clone();
        tokio::spawn(async move {
            if let Err(e) = p.reload().await {
                tracing::warn!(error = %e, "policy reload after an auth change failed");
            }
        });
    }));
    Ok((auth, policy, job_access))
}

impl RunningServer {
    /// The bound address (useful with port 0).
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// The one-time first-run setup token (FB-90), when the server started
    /// with no administrator. Print it to standard output only.
    pub fn setup_token(&self) -> Option<&str> {
        self.setup_token.as_deref()
    }

    /// The address of the RESP (Redis protocol) listener, when it is enabled
    /// (useful with port 0).
    pub fn resp_addr(&self) -> Option<SocketAddr> {
        self.resp.as_ref().map(forgebase_resp::RespHandle::local_addr)
    }

    /// Native TLS (ADR-0033), when `[tls] mode` is not `off`.
    pub fn tls(&self) -> Option<&Arc<tls::TlsSetup>> {
        self.tls.as_ref()
    }

    /// The plain HTTP listener's address (redirects to HTTPS and `http-01`
    /// answers), when it runs.
    pub fn http_redirect_addr(&self) -> Option<SocketAddr> {
        self.plain.as_ref().map(|p| p.0)
    }

    /// `https://<addr>` with native TLS, `http://<addr>` otherwise.
    pub fn url(&self) -> String {
        let scheme = if self.tls.is_some() { "https" } else { "http" };
        format!("{scheme}://{}", self.addr)
    }

    /// The primary database.
    pub fn database(&self) -> &Database {
        &self.db
    }

    /// The realtime service. With `[realtime] enabled = false` it only
    /// writes and prunes the record-change outbox (no dispatcher, no routes).
    pub fn realtime(&self) -> &forgebase_realtime::RealtimeService {
        &self.realtime
    }

    /// The job queue of this instance (enqueue, inspect, claim as the system
    /// principal).
    pub fn jobs(&self) -> &JobService {
        &self.jobs
    }

    /// The entitlement and quota service of this instance.
    pub fn entitlements(&self) -> &EntitlementService {
        &self.entitlements
    }

    /// The server-functions engine (FB-60), unless `[functions] enabled = false`.
    pub fn functions(&self) -> Option<&FunctionService> {
        self.functions.as_ref()
    }

    /// The key-value engine of this instance.
    pub fn kv(&self) -> &KvService {
        &self.kv
    }

    /// The operations insights service (ADR-0036).
    pub fn insights(&self) -> &forgebase_insights::InsightsService {
        &self.insights
    }

    /// The credit ledger of this instance.
    pub fn credits(&self) -> &CreditService {
        &self.credits
    }

    /// The billing application (`None` unless `[billing] enabled`).
    pub fn billing(&self) -> Option<&forgebase_billing::BillingService> {
        self.billing.as_ref()
    }

    /// The customization service (domains, branding, email, agreements).
    pub fn customize(&self) -> &CustomizeService {
        &self.customize
    }

    /// The alerting engine (ADR-0035), unless `[alerts] enabled = false`.
    pub fn alerts(&self) -> Option<&forgebase_alerts::AlertService> {
        self.alerts.as_ref()
    }

    /// The topics and queues service (FB-63), unless `[queue] enabled = false`.
    pub fn queue(&self) -> Option<&forgebase_queue::QueueService> {
        self.queue.as_ref()
    }

    /// The vector engine of this instance, unless `[vector] enabled = false`.
    pub fn vector(&self) -> Option<&VectorService> {
        self.vector.as_ref()
    }

    /// This instance's identity.
    pub fn instance(&self) -> &InstanceInfo {
        &self.instance
    }

    /// Migrations applied during startup.
    pub fn startup_migrations(&self) -> &MigrateReport {
        &self.migrations
    }

    /// The primary database.
    pub fn db(&self) -> &Database {
        &self.db
    }

    /// The database sets (FB-171): control and every open shard.
    pub fn router(&self) -> &forgebase_db::DbRouter {
        &self.router
    }

    /// The backend of this data directory (FB-148).
    pub fn backend(&self) -> BackendMarker {
        self.backend
    }

    /// The bundled PostgreSQL engine, when this server runs it.
    pub fn engine(&self) -> Option<&forgebase_pgengine::EngineHandle> {
        self.engine.as_deref()
    }

    /// Serves until SIGINT/SIGTERM (or until the server fails), then shuts
    /// down gracefully.
    pub async fn run_until_signal(self) -> Result<(), ServerError> {
        self.run_until_signals(ShutdownSignals::install()).await
    }

    /// Like [`run_until_signal`](Self::run_until_signal) with signal streams
    /// installed earlier (see [`ShutdownSignals::install`]): a signal that
    /// arrived during startup is delivered immediately and ends in a graceful
    /// shutdown instead of the default action killing the process.
    pub async fn run_until_signals(mut self, mut signals: ShutdownSignals) -> Result<(), ServerError> {
        let Some(mut task) = self.task.take() else {
            return self.shutdown().await;
        };
        tokio::select! {
            () = signals.recv() => {
                tracing::info!("shutdown signal received");
                self.task = Some(task);
                self.shutdown().await
            }
            result = &mut task => {
                tracing::error!("server stopped unexpectedly");
                self.finish().await;
                match result {
                    Ok(Ok(())) => Ok(()),
                    Ok(Err(e)) => Err(ServerError::Serve(e)),
                    Err(e) => Err(ServerError::Serve(std::io::Error::other(e))),
                }
            }
        }
    }

    /// Stops accepting connections, drains in-flight requests (bounded by the
    /// shutdown timeout), checkpoints and closes the database, releases the lock.
    pub async fn shutdown(mut self) -> Result<(), ServerError> {
        // Realtime connections (upgraded sockets, SSE streams) are not
        // in-flight requests: close them first so graceful shutdown can finish.
        self.realtime.shutdown();
        if let Some(q) = &self.queue {
            q.close_streams();
        }
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        let mut result = Ok(());
        if let Some(mut task) = self.task.take() {
            match tokio::time::timeout(self.shutdown_timeout, &mut task).await {
                Ok(Ok(Ok(()))) => {}
                Ok(Ok(Err(e))) => result = Err(ServerError::Serve(e)),
                Ok(Err(e)) => result = Err(ServerError::Serve(std::io::Error::other(e))),
                Err(_) => {
                    tracing::warn!(
                        timeout_secs = self.shutdown_timeout.as_secs(),
                        "in-flight requests did not finish in time; aborting them"
                    );
                    task.abort();
                }
            }
        }
        self.finish().await;
        tracing::info!("forgebase stopped");
        result
    }

    async fn finish(&mut self) {
        self.realtime.shutdown();
        if let Some((_, stop, mut task)) = self.plain.take() {
            let _ = stop.send(());
            if tokio::time::timeout(self.shutdown_timeout, &mut task).await.is_err() {
                task.abort();
            }
        }
        if let Some(t) = self.tls_tasks.take() {
            t.stop().await;
        }
        if let Some(a) = &self.alerts {
            a.shutdown(self.shutdown_timeout.min(Duration::from_secs(5))).await;
        }
        if let Some(q) = &self.queue {
            q.close_streams();
            q.shutdown(self.shutdown_timeout).await;
        }
        if let Some(f) = &self.functions {
            f.shutdown();
        }
        self.insights.shutdown().await;
        self.upkeep.abort();
        if let Some(resp) = self.resp.take() {
            resp.shutdown().await;
        }
        drop(self.pgcron.take());
        if let Some(pool) = self.job_pool.take() {
            pool.shutdown().await;
        }
        if let Some(v) = &self.vector {
            let saved = v.shutdown().await;
            tracing::debug!(files = saved, "vector indexes saved");
        }
        match self.kv.shutdown().await {
            Ok(n) => tracing::debug!(rows = n, "kv snapshot written"),
            Err(e) => tracing::warn!(error = %e, "final kv snapshot failed; recent kv writes are lost"),
        }
        if let Err(e) = database::checkpoint(&self.db).await {
            tracing::warn!(error = %e, "final WAL checkpoint failed; the WAL will be replayed on next start");
        }
        drop(self.recovery.take());
        for set in self.router.sets().into_iter().filter(|s| !s.is_control()) {
            set.database().close().await;
        }
        self.db.close().await;
        // FB-175: local replicas (and a promoted primary) before the engine.
        self.replicas.stop().await;
        if let Some(engine) = self.engine.take()
            && let Err(e) = engine.stop().await
        {
            tracing::warn!(error = %e, "stopping PostgreSQL failed");
        }
        self.lock.take();
    }
}

/// SIGINT and SIGTERM streams. Installing them registers the handlers, so
/// create them before startup: a signal received while the server is still
/// starting is remembered and delivered by [`recv`](Self::recv) instead of
/// killing the process (exit 143, bundled PostgreSQL left running).
pub struct ShutdownSignals {
    #[cfg(unix)]
    int: Option<tokio::signal::unix::Signal>,
    #[cfg(unix)]
    term: Option<tokio::signal::unix::Signal>,
}

impl ShutdownSignals {
    /// Registers the handlers now. Must run inside a tokio runtime.
    pub fn install() -> Self {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            Self {
                int: signal(SignalKind::interrupt()).ok(),
                term: signal(SignalKind::terminate()).ok(),
            }
        }
        #[cfg(not(unix))]
        Self {}
    }

    /// Resolves on Ctrl-C, or SIGTERM on Unix.
    pub async fn recv(&mut self) {
        #[cfg(unix)]
        {
            async fn one(s: &mut Option<tokio::signal::unix::Signal>) {
                match s {
                    Some(s) => {
                        s.recv().await;
                    }
                    None => std::future::pending::<()>().await,
                }
            }
            tokio::select! {
                () = one(&mut self.int) => {}
                () = one(&mut self.term) => {}
            }
        }
        #[cfg(not(unix))]
        {
            if tokio::signal::ctrl_c().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }
}

/// Resolves on Ctrl-C, or SIGTERM on Unix.
pub async fn shutdown_signal() {
    ShutdownSignals::install().recv().await;
}

/// Server errors.
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    /// Another process holds the data-directory lock.
    #[error("data directory {path} is in use by another ForgeBase process{}", owner.as_ref().map(|o| format!(" (pid {}, {})", o.pid, o.addr)).unwrap_or_default())]
    Locked {
        /// Data directory.
        path: PathBuf,
        /// The holder, if it recorded itself.
        owner: Option<LockOwner>,
    },
    /// The listener could not be bound.
    #[error("cannot listen on {addr}: {source}")]
    Bind {
        /// Requested address.
        addr: String,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
    /// The database failed to open or migrate.
    #[error(transparent)]
    Database(#[from] DbError),
    /// The data service could not load its catalog.
    #[error(transparent)]
    Data(#[from] forgebase_data::DataError),
    /// The key-value engine could not load its snapshot.
    #[error(transparent)]
    Kv(#[from] KvError),
    /// The vector engine could not load its catalog.
    #[error(transparent)]
    Vector(#[from] VectorError),
    /// A filesystem error.
    #[error("i/o error at {path}: {source}")]
    Io {
        /// Path involved.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
    /// The HTTP server failed.
    #[error("http server error: {0}")]
    Serve(#[source] std::io::Error),
    /// The auth service failed to start (master key, signing keys).
    #[error("auth: {0}")]
    Auth(#[from] forgebase_auth::AuthError),
    /// The policy service failed to load its model.
    #[error("policy: {0}")]
    Policy(String),
    /// The realtime service failed to start.
    #[error("realtime: {0}")]
    Realtime(#[from] forgebase_realtime::RealtimeError),
    /// The entitlement service failed to load its catalog.
    #[error("entitlements: {0}")]
    Entitlements(String),
    /// The functions engine failed to start.
    #[error("functions: {0}")]
    Functions(String),
    /// Object storage could not start.
    #[error("storage: {0}")]
    Storage(String),
    /// The queue service could not start.
    #[error("queue: {0}")]
    Queue(String),
    /// The alerting engine could not start (ADR-0035): an invalid
    /// `[[alerts.rules]]` entry, or its tables.
    #[error("alerts: {0}")]
    Alerts(String),
    /// The credit service failed to open.
    #[error("credits: {0}")]
    Credits(String),
    /// The billing service failed to open.
    #[error("billing: {0}")]
    Billing(String),
    /// The customization service failed to open.
    #[error("customize: {0}")]
    Customize(String),
    /// The bundled PostgreSQL engine failed (FB-148).
    #[error(transparent)]
    Engine(#[from] forgebase_pgengine::EngineError),
    /// The configured backend contradicts what the data directory holds
    /// (ADR-0028 §6.3).
    #[error("{}", mismatch_message(configured, *found, data_dir))]
    BackendMismatch {
        /// The marker the configuration asks for (`postgres-managed`).
        configured: &'static str,
        /// What the data directory holds.
        found: BackendMarker,
        /// The data directory.
        data_dir: PathBuf,
    },
    /// The backend cannot be used as configured (no engine in this build, an
    /// external server without a URL, an unknown marker).
    #[error("{0}")]
    Backend(String),
    /// A PostgreSQL extension a feature needs cannot be used (ADR-0029 §1.1).
    #[error(transparent)]
    Extensions(#[from] forgebase_db::ExtensionError),
    /// The data directory holds no database (`forgebase init` creates it).
    #[error("no database in {} (run `forgebase init`)", .0.display())]
    NotInitialized(PathBuf),
    /// Native TLS cannot start: a certificate or key file, a mismatched key,
    /// the ACME settings (ADR-0033).
    #[error(transparent)]
    Tls(#[from] tls::TlsError),
}

fn mismatch_message(configured: &str, found: BackendMarker, data_dir: &std::path::Path) -> String {
    let want = BackendMarker::parse(configured).unwrap_or(BackendMarker::Sqlite);
    let dir = data_dir.display();
    if want.backend() == found.backend() {
        let mode = want.mode().map_or("", forgebase_core::config::PostgresMode::as_str);
        format!(
            "database.postgres.mode is {mode}, but the data directory {dir} holds {}; \
             moving it needs forgebase db convert, or set [database.postgres] mode = \"{}\"",
            found.describe(),
            found.mode().map_or("", forgebase_core::config::PostgresMode::as_str)
        )
    } else {
        format!(
            "database.backend is {}, but the data directory {dir} holds {}; run forgebase db convert --to {}, \
             or set [database] backend = \"{}\"",
            want.backend(),
            found.describe(),
            want.backend(),
            found.backend()
        )
    }
}

impl ErrorCode for ServerError {
    fn code(&self) -> &'static str {
        match self {
            ServerError::Locked { .. } => "data_dir_locked",
            ServerError::Bind { source, .. } if source.kind() == std::io::ErrorKind::AddrInUse => "port_in_use",
            ServerError::Bind { .. } => "bind_failed",
            ServerError::Database(e) => e.code(),
            ServerError::Data(e) => e.code(),
            ServerError::Kv(e) => e.code(),
            ServerError::Vector(e) => e.code(),
            ServerError::Io { .. } => "io_error",
            ServerError::Serve(_) => "server_error",
            ServerError::Auth(e) => e.code(),
            ServerError::Policy(_) => "policy_unavailable",
            ServerError::Realtime(e) => e.code(),
            ServerError::Entitlements(_) => "entitlements_unavailable",
            ServerError::Functions(_) => "functions_unavailable",
            ServerError::Storage(_) => "storage_unavailable",
            ServerError::Queue(_) => "queue_unavailable",
            ServerError::Alerts(_) => "alerts_unavailable",
            ServerError::Credits(_) => "credits_unavailable",
            ServerError::Billing(_) => "billing_unavailable",
            ServerError::Customize(_) => "customize_unavailable",
            ServerError::Engine(_) => "database_engine_failed",
            ServerError::BackendMismatch { .. } => "backend_mismatch",
            ServerError::Backend(_) => "backend_unavailable",
            ServerError::Extensions(e) => e.code(),
            ServerError::NotInitialized(_) => "not_initialized",
            ServerError::Tls(e) => e.code(),
        }
    }

    fn kind(&self) -> ErrorKind {
        match self {
            ServerError::Locked { .. } | ServerError::Bind { .. } => ErrorKind::Conflict,
            ServerError::Database(e) => e.kind(),
            ServerError::Data(e) => e.kind(),
            ServerError::Kv(e) => e.kind(),
            ServerError::Vector(e) => e.kind(),
            ServerError::Io { .. } | ServerError::Serve(_) => ErrorKind::Internal,
            ServerError::Auth(e) => e.kind(),
            ServerError::Policy(_)
            | ServerError::Entitlements(_)
            | ServerError::Functions(_)
            | ServerError::Storage(_)
            | ServerError::Queue(_)
            | ServerError::Alerts(_)
            | ServerError::Credits(_)
            | ServerError::Customize(_)
            | ServerError::Billing(_) => ErrorKind::Internal,
            ServerError::Realtime(e) => e.kind(),
            ServerError::Engine(_) | ServerError::Backend(_) => ErrorKind::Unavailable,
            ServerError::Extensions(e) => e.kind(),
            ServerError::BackendMismatch { .. } => ErrorKind::Conflict,
            ServerError::NotInitialized(_) => ErrorKind::NotFound,
            ServerError::Tls(e) => e.kind(),
        }
    }
}

/// Builds the storage service for the configured driver.
async fn open_storage(
    db: &Database,
    router: &forgebase_db::DbRouter,
    layout: &DataLayout,
    config: &Config,
    data: forgebase_data::DataService,
    entitlements: &EntitlementService,
    master_key_path: std::path::PathBuf,
) -> Result<forgebase_storage::StorageService, ServerError> {
    use forgebase_storage::{LocalDriver, S3Config, S3Driver, StorageDriver, StorageService, StorageSettings};
    let sc = &config.storage;
    let driver: Arc<dyn StorageDriver> = match sc.driver {
        forgebase_core::StorageDriverKind::Local => Arc::new(LocalDriver::new(layout.storage_dir())),
        forgebase_core::StorageDriverKind::S3 => {
            let env = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
            let (Some(access), Some(secret)) = (
                env("FORGEBASE_STORAGE_S3_ACCESS_KEY"),
                env("FORGEBASE_STORAGE_S3_SECRET_KEY"),
            ) else {
                return Err(ServerError::Storage(
                    "the s3 driver needs FORGEBASE_STORAGE_S3_ACCESS_KEY and FORGEBASE_STORAGE_S3_SECRET_KEY".into(),
                ));
            };
            let mut cfg = S3Config::new(&sc.s3_endpoint, &sc.s3_bucket, access, secret);
            cfg.region.clone_from(&sc.s3_region);
            cfg.prefix.clone_from(&sc.s3_prefix);
            cfg.session_token = env("FORGEBASE_STORAGE_S3_SESSION_TOKEN");
            cfg.part_size = usize::try_from(sc.s3_part_size).unwrap_or(8 * 1024 * 1024);
            let tls = forgebase_auth::net::TlsSettings::new();
            let client = forgebase_auth::net::HttpsClient::new(&tls)
                .map_err(|e| ServerError::Storage(e.to_string()))?
                .with_timeout(std::time::Duration::from_secs(120))
                .with_max_body(cfg.read_chunk.max(cfg.part_size) + 1024 * 1024);
            Arc::new(S3Driver::new(cfg, Arc::new(client)).map_err(|e| ServerError::Storage(e.to_string()))?)
        }
        forgebase_core::StorageDriverKind::Gcs => Arc::new(open_gcs_driver(sc).await?),
        forgebase_core::StorageDriverKind::Azure => Arc::new(open_azure_driver(sc)?),
    };
    let settings = StorageSettings {
        max_object_bytes: sc.max_object_bytes,
        upload_ttl_secs: sc.upload_ttl_secs,
        reconcile_grace_secs: sc.reconcile_grace_secs,
        signed_urls: sc.signed_urls,
        signed_urls_buckets: sc.signed_urls_buckets.clone(),
        ..StorageSettings::default()
    };
    // The URL signing key is sealed with the data directory's master key
    // (FB-140, rls-coverage G11).
    let keyring =
        tokio::task::spawn_blocking(move || forgebase_auth::crypto::Keyring::load_or_create(&master_key_path))
            .await
            .map_err(|e| ServerError::Storage(e.to_string()))?
            .map_err(|e| ServerError::Storage(e.to_string()))?;
    let svc = StorageService::open(
        Arc::new(db.clone()),
        data,
        driver,
        Arc::new(SystemClock),
        settings,
        keyring,
    )
    .await
    .map_err(|e| ServerError::Storage(e.to_string()))?;
    // FB-172: buckets and objects live on the bucket's set.
    svc.attach_router(router.clone());
    svc.attach_entitlements(entitlements.clone());
    Ok(svc)
}

/// The outbound client of the native cloud storage drivers (ADR-0040).
fn cloud_http_client(
    part_size: usize,
    read_chunk: usize,
) -> Result<Arc<forgebase_auth::net::HttpsClient>, ServerError> {
    let tls = forgebase_auth::net::TlsSettings::new();
    let client = forgebase_auth::net::HttpsClient::new(&tls)
        .map_err(|e| ServerError::Storage(e.to_string()))?
        .with_timeout(std::time::Duration::from_secs(120))
        .with_max_body(read_chunk.max(part_size) + 1024 * 1024);
    Ok(Arc::new(client))
}

fn storage_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// The Google Cloud Storage driver: a service-account key from
/// `FORGEBASE_STORAGE_GCS_CREDENTIALS_JSON`, or the file named by
/// `FORGEBASE_STORAGE_GCS_CREDENTIALS_FILE` / `GOOGLE_APPLICATION_CREDENTIALS`,
/// else a fixed `FORGEBASE_STORAGE_GCS_ACCESS_TOKEN`, else the metadata server
/// (workload identity); with a custom endpoint and none of these, no
/// credentials (an emulator).
async fn open_gcs_driver(sc: &forgebase_core::StorageConfig) -> Result<forgebase_storage::GcsDriver, ServerError> {
    use forgebase_storage::{GcsConfig, GcsDriver};
    let fail = |m: String| ServerError::Storage(m);
    let key_text = match storage_env("FORGEBASE_STORAGE_GCS_CREDENTIALS_JSON") {
        Some(json) => Some(json),
        None => match storage_env("FORGEBASE_STORAGE_GCS_CREDENTIALS_FILE")
            .or_else(|| storage_env("GOOGLE_APPLICATION_CREDENTIALS"))
        {
            Some(path) => Some(
                tokio::fs::read_to_string(&path)
                    .await
                    .map_err(|e| fail(format!("cannot read the GCS credentials file {path}: {e}")))?,
            ),
            None => None,
        },
    };
    let custom_endpoint = !sc.gcs_endpoint.trim().is_empty();
    let auth = forgebase_storage::gcs::auth_from_env(&storage_env, key_text, custom_endpoint)
        .map_err(|e| fail(e.to_string()))?;
    let mut cfg = GcsConfig::new(&sc.gcs_bucket, auth);
    if custom_endpoint {
        cfg.endpoint.clone_from(&sc.gcs_endpoint);
    }
    cfg.prefix.clone_from(&sc.gcs_prefix);
    cfg.part_size = usize::try_from(sc.s3_part_size).unwrap_or(8 * 1024 * 1024);
    let client = cloud_http_client(cfg.part_size, cfg.read_chunk)?;
    GcsDriver::new(cfg, client).map_err(|e| fail(e.to_string()))
}

/// The Azure Blob driver: the account key from
/// `FORGEBASE_STORAGE_AZURE_ACCOUNT_KEY`, else a SAS token
/// (`..._SAS_TOKEN`), else Microsoft Entra client credentials
/// (`..._TENANT_ID`, `..._CLIENT_ID`, `..._CLIENT_SECRET`), else the
/// managed identity of the host (`..._CLIENT_ID` picks a user-assigned one).
fn open_azure_driver(sc: &forgebase_core::StorageConfig) -> Result<forgebase_storage::AzureDriver, ServerError> {
    use forgebase_storage::{AzureConfig, AzureDriver};
    let fail = |m: String| ServerError::Storage(m);
    let auth = forgebase_storage::azure::auth_from_env(&storage_env).map_err(|e| fail(e.to_string()))?;
    let mut cfg = AzureConfig::new(&sc.azure_account, &sc.azure_container, auth);
    cfg.endpoint = Some(sc.azure_endpoint.clone()).filter(|e| !e.trim().is_empty());
    cfg.prefix.clone_from(&sc.azure_prefix);
    cfg.part_size = usize::try_from(sc.s3_part_size).unwrap_or(8 * 1024 * 1024);
    let client = cloud_http_client(cfg.part_size, cfg.read_chunk)?;
    AzureDriver::new(cfg, client).map_err(|e| fail(e.to_string()))
}

/// FB-163: enqueues `graph.materialise` jobs for the graph service.
#[derive(Debug)]
struct GraphJobs(JobService);

impl forgebase_graph::MaterialiseJobs for GraphJobs {
    fn enqueue<'a>(&'a self, tenant: &'a str) -> forgebase_db::BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let job = forgebase_jobs::NewJob::new(
                forgebase_graph::MATERIALISE_JOB,
                serde_json::json!({ "tenant": tenant }),
            );
            self.0
                .enqueue(&forgebase_data::AccessContext::system(), job)
                .await
                .map(|(job, _)| job.id)
                .map_err(|e| e.to_string())
        })
    }
}
