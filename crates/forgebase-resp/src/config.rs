//! Listener settings.

use std::time::Duration;

/// Settings of the RESP listener. [`RespConfig::default`] is the production
/// default; `forgebase-server` fills the first six fields from `[resp]` in
/// `forgebase.toml`.
#[derive(Debug, Clone)]
pub struct RespConfig {
    /// Bind host.
    pub host: String,
    /// Bind port; `0` picks a free port.
    pub port: u16,
    /// Most simultaneous connections; further ones get `-ERR max number of clients reached`.
    pub max_connections: usize,
    /// Largest single bulk string (one argument) in bytes.
    pub max_bulk_bytes: usize,
    /// Idle connections are closed after this long; `Duration::ZERO` = never.
    /// Subscribed connections are exempt (like Redis).
    pub idle_timeout: Duration,
    /// An unauthenticated connection is closed after this long.
    pub auth_timeout: Duration,
    /// A write that makes no progress for this long closes the connection
    /// (a client that does not read its replies or its pub/sub messages).
    pub write_timeout: Duration,
    /// Most arguments in one command.
    pub max_args: usize,
    /// Most bytes of arguments in one command (sum of its bulk strings).
    pub max_request_bytes: usize,
    /// Most channels plus patterns one connection may subscribe to.
    pub max_subscriptions: usize,
    /// Failed `AUTH`s after which a connection is closed.
    pub max_auth_failures: u32,
    /// Pause after each failed `AUTH`.
    pub auth_failure_delay: Duration,
    /// Failed `AUTH`s per client address within [`RespConfig::auth_window`]
    /// before that address is refused for the rest of the window.
    pub max_auth_failures_per_ip: u32,
    /// Window of the per-address failure count.
    pub auth_window: Duration,
    /// How often a live connection's credential is checked again, so a
    /// revoked key stops working on open connections. `ZERO` = never.
    pub revalidate_interval: Duration,
    /// How long shutdown lets connections finish before it drops them.
    pub shutdown_grace: Duration,
}

impl Default for RespConfig {
    fn default() -> Self {
        let max_bulk_bytes = 16 * 1024 * 1024;
        Self {
            host: "127.0.0.1".into(),
            port: 6380,
            max_connections: 1024,
            max_bulk_bytes,
            idle_timeout: Duration::from_secs(300),
            auth_timeout: Duration::from_secs(10),
            write_timeout: Duration::from_secs(30),
            max_args: 1_048_576,
            max_request_bytes: max_bulk_bytes.saturating_mul(4),
            max_subscriptions: 1024,
            max_auth_failures: 5,
            auth_failure_delay: Duration::from_millis(100),
            max_auth_failures_per_ip: 20,
            auth_window: Duration::from_secs(60),
            revalidate_interval: Duration::from_secs(30),
            shutdown_grace: Duration::from_secs(5),
        }
    }
}

/// Before a connection authenticates it may send at most this many arguments
/// per command and this many bytes per argument (an `AUTH`/`HELLO` fits easily).
pub(crate) const PREAUTH_MAX_ARGS: usize = 16;
pub(crate) const PREAUTH_MAX_BULK: usize = 4096;
/// Longest inline command line.
pub(crate) const MAX_INLINE: usize = 64 * 1024;
