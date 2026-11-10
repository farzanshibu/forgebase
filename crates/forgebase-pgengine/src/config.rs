//! Engine configuration: what FB-148 maps `[database.postgres]` onto.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use crate::error::{EngineError, Result};

/// Development fallback engine directory (ADR-0028 §2.2, plan rule 7).
pub const ENGINE_DIR_ENV: &str = "FORGEBASE_PG_ENGINE_DIR";

/// Where the engine binaries come from.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum EngineSource {
    /// The archive embedded in this binary (feature `bundled-postgres`);
    /// without it, `FORGEBASE_PG_ENGINE_DIR`; otherwise an error that says
    /// how to get an engine.
    #[default]
    Auto,
    /// An installed engine directory used in place (`[database.postgres]
    /// engine_dir`, a shared read-only engine, Homebrew or apt). Never
    /// modified.
    Dir(PathBuf),
    /// An engine archive (`forgebase-pg*.tar.zst`) on disk, extracted and
    /// verified like the embedded one.
    Archive(PathBuf),
    /// The embedded archive only.
    Embedded,
}

/// `auto` / `on` / `off` for an extension ForgeBase preloads
/// (`[database.postgres.extensions]`, ADR-0029 §1.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExtensionMode {
    /// Preload when the engine ships it; otherwise warn and continue.
    #[default]
    Auto,
    /// Refuse to start without it.
    On,
    /// Never preload it.
    Off,
}

/// Crash-restart backoff (ADR-0028 §4.3): 1 s, 2 s, 4 s … 30 s; after
/// `max_failures` failures within `window` the engine is marked failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestartPolicy {
    /// First delay.
    pub initial: Duration,
    /// Largest delay.
    pub max: Duration,
    /// Failures within `window` after which ForgeBase stops trying.
    pub max_failures: u32,
    /// Window for counting failures.
    pub window: Duration,
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self {
            initial: Duration::from_secs(1),
            max: Duration::from_secs(30),
            max_failures: 5,
            window: Duration::from_secs(600),
        }
    }
}

impl RestartPolicy {
    /// Delay before restart number `attempt` (1-based).
    pub fn delay(&self, attempt: u32) -> Duration {
        let factor = 1u32.checked_shl(attempt.saturating_sub(1)).unwrap_or(u32::MAX);
        self.initial.saturating_mul(factor).min(self.max)
    }
}

/// How to run one cluster.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Where the binaries come from.
    pub source: EngineSource,
    /// `None` (default): Unix socket only (`listen_addresses = ''`). `Some`:
    /// also listen on this loopback TCP address (`[database.postgres]
    /// listen`). Addresses off loopback need TLS and are refused until
    /// FB-153 generates certificates. On Windows the engine always listens
    /// on loopback (a free port when this is `None`).
    pub listen: Option<SocketAddr>,
    /// Port number naming the Unix socket (`.s.PGSQL.<port>`) when `listen`
    /// is `None`. Default 5432; every node has its own socket directory.
    pub socket_port: u16,
    /// Extra `postgresql.conf` parameters (`[database.postgres.settings]`),
    /// checked against the deny list.
    pub settings: BTreeMap<String, String>,
    /// pg_cron preload (`[database.postgres.extensions] cron`).
    pub cron: ExtensionMode,
    /// `log_min_duration_statement` (the slow-query threshold); `None` = off.
    pub slow_query: Option<Duration>,
    /// How long to wait for the engine to accept connections.
    pub ready_timeout: Duration,
    /// How long a fast shutdown may take before an immediate one.
    pub stop_timeout: Duration,
    /// Crash-restart backoff.
    pub restart: RestartPolicy,
    /// Base directory for the long-path fallback of extracted engines
    /// (default `$XDG_CACHE_HOME/forgebase` or `$HOME/.cache/forgebase`).
    pub cache_dir: Option<PathBuf>,
    /// After a clean start from an extracted engine, delete older extracted
    /// engines of the same major (minor upgrade by engine swap, §4.5).
    pub remove_old_engines: bool,
    /// Tests only: `fsync = off`, `synchronous_commit = off`,
    /// `full_page_writes = off` and `initdb --no-sync`. Never for real data.
    pub unsafe_fast: bool,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            source: EngineSource::Auto,
            listen: None,
            socket_port: 5432,
            settings: BTreeMap::new(),
            cron: ExtensionMode::Auto,
            slow_query: None,
            ready_timeout: Duration::from_secs(30),
            stop_timeout: Duration::from_secs(30),
            restart: RestartPolicy::default(),
            cache_dir: None,
            remove_old_engines: true,
            unsafe_fast: false,
        }
    }
}

impl EngineConfig {
    /// Parses `[database.postgres] listen`: `""` = socket only, otherwise
    /// `host:port` on loopback.
    ///
    /// # Errors
    /// Not an address, or not loopback.
    pub fn parse_listen(value: &str) -> Result<Option<SocketAddr>> {
        let value = value.trim();
        if value.is_empty() {
            return Ok(None);
        }
        let addr: SocketAddr = value.parse().map_err(|_| {
            EngineError::Config(format!(
                "listen {value:?} must be \"\" or an address such as 127.0.0.1:5433"
            ))
        })?;
        check_listen(addr)?;
        Ok(Some(addr))
    }
}

pub(crate) fn check_listen(addr: SocketAddr) -> Result<()> {
    if !addr.ip().is_loopback() {
        return Err(EngineError::Config(format!(
            "listen {addr}: logins from other hosts need TLS, which this version does not set up; \
             use a loopback address (127.0.0.1 or ::1) or leave listen empty"
        )));
    }
    if addr.port() == 0 {
        return Err(EngineError::Config(format!("listen {addr}: choose a port")));
    }
    Ok(())
}

/// The default cache base for the long-path fallback.
pub(crate) fn default_cache_dir() -> Option<PathBuf> {
    if let Some(x) = std::env::var_os("XDG_CACHE_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(x).join("forgebase"));
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".cache").join("forgebase"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_to_the_cap() {
        let p = RestartPolicy::default();
        let secs: Vec<u64> = (1..=7).map(|n| p.delay(n).as_secs()).collect();
        assert_eq!(secs, [1, 2, 4, 8, 16, 30, 30]);
        assert_eq!(p.delay(100), Duration::from_secs(30));
    }

    #[test]
    fn listen_parsing() {
        assert_eq!(EngineConfig::parse_listen("").expect("empty"), None);
        assert_eq!(
            EngineConfig::parse_listen("127.0.0.1:5433").expect("loopback"),
            Some("127.0.0.1:5433".parse().expect("addr"))
        );
        assert!(EngineConfig::parse_listen("[::1]:5433").expect("v6").is_some());
        assert!(EngineConfig::parse_listen("0.0.0.0:5433").is_err());
        assert!(EngineConfig::parse_listen("10.0.0.1:5433").is_err());
        assert!(EngineConfig::parse_listen("127.0.0.1:0").is_err());
        assert!(EngineConfig::parse_listen("localhost").is_err());
    }
}
