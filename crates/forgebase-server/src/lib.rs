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
//!    limits),
//! 6. start the job scheduler and in-process worker pool, and the vector
//!    engine's index warm-up and saver.
//!
//! [`RunningServer::shutdown`] stops accepting connections, waits up to
//! `shutdown_timeout_secs` for in-flight requests, stops the job pool (running
//! jobs get the same time to finish, the rest are released back to the queue),
//! writes changed vector indexes to disk, checkpoints the WAL into the main database file, closes the pools and
//! releases the lock.

mod lock;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use forgebase_api::{AppState, RouterOptions};
use forgebase_core::{Config, DataLayout, ErrorCode, ErrorKind};
use forgebase_data::Authorizer;
use forgebase_db::{Database, DbError, DbOptions, InstanceInfo, MigrateReport};
use forgebase_entitlements::{EntitlementService, SystemClock};
use forgebase_kv::{KvConfig, KvError, KvService};
use forgebase_vector::{VectorConfig, VectorError, VectorService};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

pub use forgebase_jobs::{JobRegistry, JobService, PoolConfig};
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
    vector: VectorService,
    entitlements: EntitlementService,
    shutdown_timeout: Duration,
    lock: Option<DataDirLock>,
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

/// Like [`start`], with in-process handlers for job kinds. The worker pool
/// claims only the kinds registered in `registry`.
pub async fn start_with_jobs(
    config: &Config,
    registry: JobRegistry,
    pool: PoolConfig,
) -> Result<RunningServer, ServerError> {
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

    let (auth, policy) = match open_auth(&db, &layout, config).await {
        Ok(a) => a,
        Err(e) => {
            db.close().await;
            return Err(e);
        }
    };
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
    // One authorizer — the policy engine — for the data core and the KV engine.
    let authorizer: Arc<dyn Authorizer> = policy.authorizer();
    let kv = match KvService::open(KvConfig::default(), authorizer, Some(Arc::new(db.clone()))).await {
        Ok(kv) => kv,
        Err(e) => {
            db.close().await;
            return Err(e.into());
        }
    };
    kv.start_background();
    let opts = RouterOptions {
        request_timeout: Duration::from_secs(config.server.request_timeout_secs),
        ..RouterOptions::default()
    };
    // One data service, built by the auth service with the Argon2id hasher
    // and the policy engine as its authorizer; the auth layer turns
    // credentials into each request's AccessContext, the tenancy layer adds
    // realm, organization, tenant, roles and attributes.
    let state = AppState::from_data(db.clone(), layout.clone(), auth.data().clone())
        .with_auth(auth)
        .with_policy(policy);
    let jobs = JobService::new(state.data().clone());
    entitlements.attach_jobs(jobs.clone());
    if let Err(e) = entitlements.ensure_schedule().await {
        tracing::warn!(error = %e, "entitlement maintenance schedule not created");
    }
    let registry = forgebase_entitlements::register_jobs(registry, entitlements.clone());
    let vector = match VectorService::open(state.data().clone(), layout.indexes_dir(), VectorConfig::default()).await {
        Ok(v) => v,
        Err(e) => {
            db.close().await;
            return Err(e.into());
        }
    };
    let opts = RouterOptions {
        extensions: vec![
            forgebase_jobs::extension(jobs.clone()),
            forgebase_vector::extension(vector.clone()),
            forgebase_entitlements::extension(entitlements.clone()),
        ],
        gates: vec![entitlements.rate_gate()],
        ..opts
    };
    let kv_routes = forgebase_kv::http::router(kv.clone(), usize::try_from(opts.max_body_bytes).unwrap_or(usize::MAX));
    let app = forgebase_api::router_with(state, opts, kv_routes, Some(forgebase_kv::http::openapi()));

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
    if !addr.ip().is_loopback() {
        tracing::warn!(%addr, "listening on a non-loopback address; /api/system/* and /metrics are not behind admin authentication yet (FB-23)");
    }
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
    vector.start_background();

    tracing::info!(
        %addr,
        data_dir = %layout.root().display(),
        instance_id = %instance.instance_id,
        applied_migrations = migrations.applied.len(),
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
        entitlements,
        shutdown_timeout: Duration::from_secs(config.server.shutdown_timeout_secs),
        lock: Some(lock),
    })
}

/// Opens the policy service (FB-21), then the auth service (FB-20) and the
/// data service they share, with the policy engine as the authorizer.
async fn open_auth(
    db: &Database,
    layout: &DataLayout,
    config: &Config,
) -> Result<(forgebase_auth::AuthService, forgebase_policy::PolicyService), ServerError> {
    let mut cfg = forgebase_auth::AuthConfig::for_data_dir(layout.root());
    cfg.public_url = format!("http://{}:{}", config.server.host, config.server.port);
    let public_url = cfg.public_url.clone();
    let backend: Arc<dyn forgebase_db::Backend> = Arc::new(db.clone());
    let policy = forgebase_policy::PolicyService::open(backend.clone())
        .await
        .map_err(|e| ServerError::Policy(e.to_string()))?;
    let auth =
        forgebase_auth::AuthService::open_with_data(backend, policy.authorizer(), layout.root(), cfg, None).await?;
    policy.attach_data(auth.data().clone());
    policy.attach_mailer(auth.mailer().clone(), public_url);
    Ok((auth, policy))
}

impl RunningServer {
    /// The bound address (useful with port 0).
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// The primary database.
    pub fn database(&self) -> &Database {
        &self.db
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

    /// The vector engine of this instance.
    pub fn vector(&self) -> &VectorService {
        &self.vector
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
        self.upkeep.abort();
        if let Some(pool) = self.job_pool.take() {
            pool.shutdown().await;
        }
        let saved = self.vector.shutdown().await;
        tracing::debug!(files = saved, "vector indexes saved");
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
    /// The entitlement service failed to load its catalog.
    #[error("entitlements: {0}")]
    Entitlements(String),
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
            ServerError::Entitlements(_) => "entitlements_unavailable",
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
            ServerError::Policy(_) | ServerError::Entitlements(_) => ErrorKind::Internal,
        }
    }
}
