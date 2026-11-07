//! Runs a ForgeBase instance.
//!
//! [`start`] performs the startup sequence and returns a [`RunningServer`]:
//!
//! 1. create the data-directory layout,
//! 2. take the data-directory lock (one server per data directory),
//! 3. open the primary database (WAL, pragmas) and apply pending system
//!    migrations, snapshotting first if the database already existed,
//! 4. record the instance id on first start,
//! 5. bind the listener and serve the API.
//!
//! [`RunningServer::shutdown`] stops accepting connections, waits up to
//! `shutdown_timeout_secs` for in-flight requests, checkpoints the WAL into
//! the main database file, closes the pools and releases the lock.

mod lock;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use forgebase_api::{AppState, RouterOptions};
use forgebase_core::{Config, DataLayout, ErrorCode, ErrorKind};
use forgebase_db::{Database, DbError, DbOptions, InstanceInfo, MigrateReport};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

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
    shutdown_timeout: Duration,
    lock: Option<DataDirLock>,
}

/// Starts a server for `config`. Returns once the listener is bound.
pub async fn start(config: &Config) -> Result<RunningServer, ServerError> {
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

    let auth = match open_auth(&db, &layout, config).await {
        Ok(a) => a,
        Err(e) => {
            db.close().await;
            return Err(e.into());
        }
    };
    let state = AppState::new(db.clone(), layout.clone()).with_auth(auth);
    let app = forgebase_api::router(
        state,
        RouterOptions {
            request_timeout: Duration::from_secs(config.server.request_timeout_secs),
        },
    );

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
        tracing::warn!(%addr, "listening on a non-loopback address; admin endpoints have no authentication yet");
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
        shutdown_timeout: Duration::from_secs(config.server.shutdown_timeout_secs),
        lock: Some(lock),
    })
}

/// Opens the auth service (FB-20) and the data service it shares, with the
/// fail-closed default authorizer until the policy engine (FB-21) lands.
async fn open_auth(
    db: &Database,
    layout: &DataLayout,
    config: &Config,
) -> Result<forgebase_auth::AuthService, forgebase_auth::AuthError> {
    let mut cfg = forgebase_auth::AuthConfig::for_data_dir(layout.root());
    cfg.public_url = format!("http://{}:{}", config.server.host, config.server.port);
    let backend: Arc<dyn forgebase_db::Backend> = Arc::new(db.clone());
    forgebase_auth::AuthService::open_with_data(
        backend,
        Arc::new(forgebase_auth::data::DefaultAuthorizer),
        layout.root(),
        cfg,
        None,
    )
    .await
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
}

impl ErrorCode for ServerError {
    fn code(&self) -> &'static str {
        match self {
            ServerError::Locked { .. } => "data_dir_locked",
            ServerError::Bind { source, .. } if source.kind() == std::io::ErrorKind::AddrInUse => "port_in_use",
            ServerError::Bind { .. } => "bind_failed",
            ServerError::Database(e) => e.code(),
            ServerError::Io { .. } => "io_error",
            ServerError::Serve(_) => "server_error",
            ServerError::Auth(e) => e.code(),
        }
    }

    fn kind(&self) -> ErrorKind {
        match self {
            ServerError::Locked { .. } | ServerError::Bind { .. } => ErrorKind::Conflict,
            ServerError::Database(e) => e.kind(),
            ServerError::Io { .. } | ServerError::Serve(_) => ErrorKind::Internal,
            ServerError::Auth(e) => e.kind(),
        }
    }
}
