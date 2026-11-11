//! Runs a ForgeBase instance.
//!
//! [`start`] performs the startup sequence and returns a [`RunningServer`]:
//!
//! 1. create the data-directory layout,
//! 2. take the data-directory lock (one server per data directory),
//! 3. open the primary database (WAL, pragmas) and apply pending system
//!    migrations, snapshotting first if the database already existed,
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
//! writes changed vector indexes to disk, checkpoints the WAL into the main database file, closes the pools and
//! releases the lock.

mod function_services;
mod job_results;
mod lock;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use forgebase_api::{AppState, RouterOptions};
use forgebase_core::{Config, ConfigOrigin, ConfigSources, DataLayout, ErrorCode, ErrorKind, LoadedConfig};
use forgebase_credits::CreditService;
use forgebase_customize::{CustomizeConfig, CustomizeService};
use forgebase_data::Authorizer;
use forgebase_db::{Database, DbError, DbOptions, InstanceInfo, MigrateReport};
use forgebase_entitlements::{EntitlementService, SystemClock};
use forgebase_functions::{FunctionService, FunctionsConfig};
use forgebase_kv::{KvConfig, KvError, KvService};
use forgebase_vector::{VectorConfig, VectorError, VectorService};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

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
    shutdown_timeout: Duration,
    lock: Option<DataDirLock>,
    /// First-run setup token (FB-90), while no administrator exists.
    setup_token: Option<String>,
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
    start_full(config, Some(origin), JobRegistry::new(), pool).await
}

/// Like [`start`], with in-process handlers for job kinds. The worker pool
/// claims only the kinds registered in `registry`.
pub async fn start_with_jobs(
    config: &Config,
    registry: JobRegistry,
    pool: PoolConfig,
) -> Result<RunningServer, ServerError> {
    start_full(config, None, registry, pool).await
}

/// The startup sequence. Without `origin`, `config` counts as built in code:
/// every setting reports `default` and `PATCH` writes `forgebase.toml` next
/// to the data directory.
async fn start_full(
    config: &Config,
    origin: Option<(ConfigOrigin, ConfigSources)>,
    registry: JobRegistry,
    pool: PoolConfig,
) -> Result<RunningServer, ServerError> {
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

    let db = Database::open(DbOptions::from_config(&config.database, layout.database_path())).await?;
    let migrations = match db.migrate(Some(&layout.backups_dir())).await {
        Ok(report) => report,
        Err(e) => {
            db.close().await;
            return Err(e.into());
        }
    };
    let instance = db.ensure_instance().await?;

    let (auth, policy, job_access) = match open_auth(&db, &layout, config).await {
        Ok(a) => a,
        Err(e) => {
            db.close().await;
            return Err(e);
        }
    };
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
    let kv = match KvService::open(kv_cfg, authorizer, Some(Arc::new(db.clone()))).await {
        Ok(kv) => kv,
        Err(e) => {
            db.close().await;
            return Err(e.into());
        }
    };
    kv.start_background();
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
            dispatch: config.realtime.enabled,
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
            Ok(b) => Some(b),
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
        match open_queue(&auth, config, &entitlements, functions.as_ref()).await {
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
    let state = AppState::from_data(db.clone(), layout.clone(), auth.data().clone())
        .with_auth(auth)
        .with_policy(policy);
    let state = if config.realtime.enabled {
        state.with_realtime(realtime.clone())
    } else {
        state
    };
    let jobs = JobService::new(state.data().clone());
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
    // Object storage (FB-30): local directory or S3-compatible driver, unless
    // `[storage] enabled = false`.
    let storage = if config.storage.enabled {
        match open_storage(&db, &layout, config, state.data().clone(), &entitlements).await {
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
    let registry = forgebase_customize::register_jobs(registry, customize.clone());
    // Vector search (FB-50), unless `[vector] enabled = false`.
    let vector = if config.vector.enabled {
        match VectorService::open(state.data().clone(), layout.indexes_dir(), VectorConfig::default()).await {
            Ok(v) => Some(v),
            Err(e) => {
                db.close().await;
                return Err(e.into());
            }
        }
    } else {
        None
    };
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
        forgebase_backup::extension(forgebase_backup::BackupService::new(db.clone(), layout.clone())),
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
    let opts = RouterOptions {
        extensions,
        gates: vec![
            entitlements.rate_gate(),
            Arc::new(forgebase_customize::AgreementGate::new(customize.clone())),
        ],
        ..opts
    };
    let state = state.with_metrics_collector(subsystem_gauges(
        jobs.clone(),
        storage.clone(),
        kv.clone(),
        vector.clone(),
        queue.clone(),
    ));
    let kv_routes = forgebase_kv::http::router(kv.clone(), usize::try_from(opts.max_body_bytes).unwrap_or(usize::MAX));
    let app = forgebase_api::router_with(state, opts, kv_routes, Some(forgebase_kv::http::openapi()));
    // Host-header routing of verified custom domains: outermost, before auth and tenancy.
    let app = app.layer(axum::middleware::from_fn_with_state(
        customize.clone(),
        forgebase_customize::host_router,
    ));

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
    lock.record_owner(&LockOwner {
        pid: std::process::id(),
        addr: addr.to_string(),
        started_at: forgebase_core::now_rfc3339(),
    });

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await
    });
    let upkeep = tokio::spawn(async {
        let handle = forgebase_observability::metrics_handle();
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        loop {
            tick.tick().await;
            handle.run_upkeep();
        }
    });

    let job_pool = forgebase_jobs::start_pool(jobs.clone(), registry, pool);
    if let Some(v) = &vector {
        v.start_background();
    }
    if let Some(f) = &functions {
        f.start_background();
    }
    if let Some(q) = &queue {
        q.start_background();
    }

    tracing::info!(
        %addr,
        data_dir = %layout.root().display(),
        instance_id = %instance.instance_id,
        applied_migrations = migrations.applied.len(),
        disabled = ?forgebase_core::features::disabled(config).iter().map(|f| f.id).collect::<Vec<_>>(),
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
        job_pool: Some(job_pool),
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
        shutdown_timeout: Duration::from_secs(config.server.shutdown_timeout_secs),
        lock: Some(lock),
        setup_token,
    })
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
    queue.attach_entitlements(entitlements.clone());
    // Without functions, push subscriptions to a function fail delivery.
    if let Some(f) = functions {
        queue.attach_functions(f.clone());
    }
    queue.set_fetcher(fetcher);
    Ok(queue)
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
    cfg.public_url = format!("http://{}:{}", config.server.host, config.server.port);
    // FB-80: `[auth]` deployment settings.
    cfg.trust_forwarded_for = config.auth.trust_forwarded_for;
    cfg.allow_private_outbound = config.auth.allow_private_outbound;
    let public_url = cfg.public_url.clone();
    let backend: Arc<dyn forgebase_db::Backend> = Arc::new(db.clone());
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
    policy.attach_mailer(auth.mailer().clone(), public_url);
    // FB-25: identities act in their own realm; suspensions set through the
    // auth API reach the policy engine's cached model.
    policy
        .attach_realms(auth.realms().clone())
        .await
        .map_err(|e| ServerError::Policy(e.to_string()))?;
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

    /// Serves until SIGINT/SIGTERM (or until the server fails), then shuts
    /// down gracefully.
    pub async fn run_until_signal(mut self) -> Result<(), ServerError> {
        let Some(mut task) = self.task.take() else {
            return self.shutdown().await;
        };
        tokio::select! {
            () = shutdown_signal() => {
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
        if let Some(q) = &self.queue {
            q.close_streams();
            q.shutdown(self.shutdown_timeout).await;
        }
        if let Some(f) = &self.functions {
            f.shutdown();
        }
        self.upkeep.abort();
        if let Some(resp) = self.resp.take() {
            resp.shutdown().await;
        }
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
        if let Err(e) = self.db.checkpoint().await {
            tracing::warn!(error = %e, "final WAL checkpoint failed; the WAL will be replayed on next start");
        }
        self.db.close().await;
        self.lock.take();
    }
}

/// Resolves on Ctrl-C, or SIGTERM on Unix.
pub async fn shutdown_signal() {
    let ctrl_c = async {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {}
        () = term => {}
    }
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
    /// The credit service failed to open.
    #[error("credits: {0}")]
    Credits(String),
    /// The billing service failed to open.
    #[error("billing: {0}")]
    Billing(String),
    /// The customization service failed to open.
    #[error("customize: {0}")]
    Customize(String),
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
            ServerError::Credits(_) => "credits_unavailable",
            ServerError::Billing(_) => "billing_unavailable",
            ServerError::Customize(_) => "customize_unavailable",
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
            | ServerError::Credits(_)
            | ServerError::Customize(_)
            | ServerError::Billing(_) => ErrorKind::Internal,
            ServerError::Realtime(e) => e.kind(),
        }
    }
}

/// Builds the storage service for the configured driver.
async fn open_storage(
    db: &Database,
    layout: &DataLayout,
    config: &Config,
    data: forgebase_data::DataService,
    entitlements: &EntitlementService,
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
    };
    let settings = StorageSettings {
        max_object_bytes: sc.max_object_bytes,
        upload_ttl_secs: sc.upload_ttl_secs,
        reconcile_grace_secs: sc.reconcile_grace_secs,
        ..StorageSettings::default()
    };
    let svc = StorageService::open(Arc::new(db.clone()), data, driver, Arc::new(SystemClock), settings)
        .await
        .map_err(|e| ServerError::Storage(e.to_string()))?;
    svc.attach_entitlements(entitlements.clone());
    Ok(svc)
}
