//! ForgeBase's RESP listener (FB-32, ADR-0013): Redis clients (`redis-cli`,
//! `redis-rs`, `ioredis`, `redis-py`, ...) talk to the built-in key-value
//! engine over the Redis protocol, RESP2 and RESP3.
//!
//! * **Off by default.** [`start`] binds a TCP listener (`[resp]` in
//!   `forgebase.toml`, default `127.0.0.1:6380`).
//! * **Authenticated.** A connection may only `PING`, `HELLO`, `AUTH` and
//!   `QUIT` until `AUTH <token>` or `AUTH <user> <token>` succeeds with a
//!   ForgeBase API key or personal access token ([`KeyAuthenticator`]). Every
//!   command then runs through the same `Authorizer` actions as the REST KV
//!   API, plus the key's `kv:read` / `kv:write` scopes.
//! * **Namespaces.** `SELECT n` picks the KV namespace (`0` = `default`,
//!   `n` = `db<n>`, or a namespace name); keys of different tenants and
//!   namespaces never meet.
//! * **A subset, stated.** [`compat`] lists what is supported, what is refused
//!   and with which error. This is not a Redis replacement.
//! * **Backends.** [`KvBackend`]: the built-in engine ([`LocalKv`]) or, with
//!   the `external-redis` feature, an external Redis/Valkey.

mod auth;
mod backend;
pub mod compat;
mod config;
mod conn;
mod errors;
#[cfg(feature = "external-redis")]
pub mod external;
mod protocol;

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{Instant, sleep, timeout};

pub use auth::{AuthFailure, Authenticator, Identity, KeyAuthenticator};
pub use backend::{BoxFuture, KvBackend, LocalKv};
pub use config::RespConfig;

use auth::AuthLimiter;
use conn::Shared;

/// A running listener.
#[derive(Debug)]
pub struct RespHandle {
    addr: SocketAddr,
    stop: watch::Sender<bool>,
    task: Option<JoinHandle<()>>,
    shared: Arc<Shared>,
}

impl RespHandle {
    /// The bound address (useful with port 0).
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Connections open right now.
    pub fn connections(&self) -> usize {
        self.shared.active.load(Ordering::SeqCst)
    }

    /// Stops accepting, lets open connections finish their current command
    /// (up to `shutdown_grace`), then drops them.
    pub async fn shutdown(mut self) {
        let _ = self.stop.send(true);
        if let Some(t) = self.task.take() {
            let _ = t.await;
        }
    }
}

impl Drop for RespHandle {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
    }
}

/// Binds the listener and serves until [`RespHandle::shutdown`].
///
/// # Errors
/// The bind error (address in use, bad host, no permission).
pub async fn start(
    cfg: RespConfig,
    backend: Arc<dyn KvBackend>,
    auth: Arc<dyn Authenticator>,
) -> std::io::Result<RespHandle> {
    let listener = TcpListener::bind((cfg.host.as_str(), cfg.port)).await?;
    let addr = listener.local_addr()?;
    if !addr.ip().is_loopback() {
        tracing::warn!(%addr, "RESP listener on a non-loopback address; the protocol has no TLS, so credentials and data cross the network in clear text");
    }
    let shared = Arc::new(Shared {
        limiter: AuthLimiter::new(cfg.max_auth_failures_per_ip, cfg.auth_window),
        cfg,
        backend,
        auth,
        active: AtomicUsize::new(0),
        next_id: AtomicU64::new(1),
        started: Instant::now(),
    });
    let (stop, stop_rx) = watch::channel(false);
    let task = tokio::spawn(accept_loop(listener, Arc::clone(&shared), stop_rx));
    tracing::info!(%addr, "RESP listener started");
    Ok(RespHandle {
        addr,
        stop,
        task: Some(task),
        shared,
    })
}

async fn accept_loop(listener: TcpListener, sh: Arc<Shared>, mut stop: watch::Receiver<bool>) {
    let mut conns: JoinSet<()> = JoinSet::new();
    loop {
        tokio::select! {
            biased;
            _ = stop.changed() => break,
            r = listener.accept() => match r {
                Ok((sock, peer)) => {
                    while conns.try_join_next().is_some() {}
                    if sh.active.load(Ordering::SeqCst) >= sh.cfg.max_connections {
                        metrics::counter!("forgebase_resp_connections_refused_total").increment(1);
                        let _ = sock.try_write(b"-ERR max number of clients reached\r\n");
                        continue;
                    }
                    let n = sh.active.fetch_add(1, Ordering::SeqCst) + 1;
                    metrics::gauge!("forgebase_resp_connections").set(n as f64);
                    let id = sh.next_id.fetch_add(1, Ordering::SeqCst);
                    conns.spawn(conn::serve(Arc::clone(&sh), sock, peer, id, stop.clone()));
                }
                Err(e) => {
                    // Typically too many open files: back off instead of spinning.
                    tracing::warn!(error = %e, "RESP accept failed");
                    sleep(std::time::Duration::from_millis(50)).await;
                }
            },
        }
    }
    drop(listener);
    let drain = async { while conns.join_next().await.is_some() {} };
    if timeout(sh.cfg.shutdown_grace, drain).await.is_err() {
        tracing::warn!("RESP connections did not close in time; dropping them");
        conns.abort_all();
        while conns.join_next().await.is_some() {}
    }
    tracing::info!("RESP listener stopped");
}
