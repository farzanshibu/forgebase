//! Configuration model and loading.
//!
//! Precedence, highest first:
//!
//! 1. command-line flags ([`ConfigOverrides`] built by the CLI),
//! 2. `FORGEBASE_*` environment variables ([`ConfigOverrides::from_env`]),
//! 3. the TOML file (`--config`, else `FORGEBASE_CONFIG`, else `./forgebase.toml` if present),
//! 4. built-in defaults.
//!
//! Relative `data_dir` values resolve against the directory of the file that
//! set them: the config file's directory for the TOML value, the current
//! working directory for flags and environment variables. The loaded
//! [`Config`] always holds an absolute `data_dir`.

use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::{ErrorCode, ErrorKind};

/// Prefix of every environment variable ForgeBase reads.
pub const ENV_PREFIX: &str = "FORGEBASE_";
/// Environment variable naming the config file.
pub const ENV_CONFIG_FILE: &str = "FORGEBASE_CONFIG";
/// Config file looked up in the working directory when none is given.
pub const DEFAULT_CONFIG_FILE: &str = "forgebase.toml";
/// Default data directory name.
pub const DEFAULT_DATA_DIR: &str = "forgebase-data";
/// Default bind host: loopback only until admin authentication lands.
pub const DEFAULT_HOST: &str = "127.0.0.1";
/// Default HTTP port.
pub const DEFAULT_PORT: u16 = 8090;
/// Default port of the optional RESP (Redis protocol) listener.
pub const DEFAULT_RESP_PORT: u16 = 6380;

/// Contents written by `forgebase init` when no config file exists.
pub const CONFIG_TEMPLATE: &str = r#"# ForgeBase configuration.
# Precedence: command-line flags > FORGEBASE_* environment variables > this file > defaults.

# Data directory (primary database, storage/, indexes/, backups/, tmp/).
# Relative paths are resolved against the directory of this file.
data_dir = "forgebase-data"

[server]
# Loopback by default: admin endpoints are unauthenticated until admin login ships.
host = "127.0.0.1"          # FORGEBASE_HOST
port = 8090                 # FORGEBASE_PORT
request_timeout_secs = 30   # FORGEBASE_REQUEST_TIMEOUT_SECS
shutdown_timeout_secs = 30  # FORGEBASE_SHUTDOWN_TIMEOUT_SECS

[database]
# "full" fsyncs the WAL on every commit (durable across power loss).
# "normal" is faster but a power cut can drop the last committed transactions.
synchronous = "full"        # FORGEBASE_DB_SYNCHRONOUS
busy_timeout_ms = 5000      # FORGEBASE_DB_BUSY_TIMEOUT_MS
read_pool_size = 8          # FORGEBASE_DB_READ_POOL_SIZE

[log]
level = "info"              # FORGEBASE_LOG_LEVEL (tracing filter syntax)
format = "pretty"           # FORGEBASE_LOG_FORMAT: "pretty" or "json"

# Optional Redis protocol (RESP2/RESP3) listener for the built-in KV engine.
# Off by default; every connection must AUTH with an API key or PAT.
[resp]
enabled = false             # FORGEBASE_RESP_ENABLED, flag --resp
host = "127.0.0.1"          # FORGEBASE_RESP_HOST, flag --resp-host
port = 6380                 # FORGEBASE_RESP_PORT, flag --resp-port
max_connections = 1024      # FORGEBASE_RESP_MAX_CONNECTIONS
max_bulk_bytes = 16777216   # FORGEBASE_RESP_MAX_BULK_BYTES
idle_timeout_secs = 300     # FORGEBASE_RESP_IDLE_TIMEOUT_SECS (0 = never)
"#;

/// SQLite `synchronous` setting for the primary database.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Synchronous {
    /// fsync the WAL on every commit: committed data survives power loss.
    Full,
    /// fsync only at checkpoints: survives process crashes, may lose the most
    /// recent commits on power loss or OS crash.
    Normal,
}

impl Synchronous {
    /// Lowercase name as used in config files.
    pub fn as_str(self) -> &'static str {
        match self {
            Synchronous::Full => "full",
            Synchronous::Normal => "normal",
        }
    }
}

impl fmt::Display for Synchronous {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Synchronous {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "full" => Ok(Synchronous::Full),
            "normal" => Ok(Synchronous::Normal),
            other => Err(format!("expected \"full\" or \"normal\", got {other:?}")),
        }
    }
}

/// Log output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    /// Human-readable lines.
    Pretty,
    /// One JSON object per line.
    Json,
}

impl LogFormat {
    /// Lowercase name as used in config files.
    pub fn as_str(self) -> &'static str {
        match self {
            LogFormat::Pretty => "pretty",
            LogFormat::Json => "json",
        }
    }
}

impl fmt::Display for LogFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for LogFormat {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "pretty" => Ok(LogFormat::Pretty),
            "json" => Ok(LogFormat::Json),
            other => Err(format!("expected \"pretty\" or \"json\", got {other:?}")),
        }
    }
}

/// Fully resolved configuration.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Config {
    /// Absolute path of the data directory.
    pub data_dir: PathBuf,
    /// HTTP server settings.
    pub server: ServerConfig,
    /// Primary database settings.
    pub database: DatabaseConfig,
    /// Logging settings.
    pub log: LogConfig,
    /// Redis protocol listener settings.
    pub resp: RespConfig,
}

/// Settings of the optional RESP (Redis protocol) listener (FB-32, ADR-0013).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RespConfig {
    /// Whether the listener runs. Off by default.
    pub enabled: bool,
    /// Bind host. Loopback by default: RESP has no TLS.
    pub host: String,
    /// Bind port; `0` picks a free port.
    pub port: u16,
    /// Most simultaneous connections; further ones are refused.
    pub max_connections: u32,
    /// Largest single bulk string (an argument such as a value) in bytes.
    pub max_bulk_bytes: u64,
    /// Seconds a connection may sit idle before it is closed; `0` = never.
    /// Subscribed connections are exempt.
    pub idle_timeout_secs: u64,
}

/// HTTP server settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ServerConfig {
    /// Bind host (IP address or hostname).
    pub host: String,
    /// Bind port; `0` picks a free port.
    pub port: u16,
    /// Per-request timeout in seconds.
    pub request_timeout_secs: u64,
    /// How long graceful shutdown waits for in-flight requests.
    pub shutdown_timeout_secs: u64,
}

/// Primary database settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DatabaseConfig {
    /// SQLite `synchronous` mode.
    pub synchronous: Synchronous,
    /// SQLite `busy_timeout` in milliseconds.
    pub busy_timeout_ms: u64,
    /// Number of read-only connections in the read pool.
    pub read_pool_size: u32,
}

/// Logging settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LogConfig {
    /// `tracing` filter directive, e.g. `info` or `forgebase=debug,tower_http=info`.
    pub level: String,
    /// Output format.
    pub format: LogFormat,
}

impl Config {
    /// Defaults with `data_dir` resolved against `base_dir`.
    pub fn defaults(base_dir: &Path) -> Self {
        Self {
            data_dir: normalize(&base_dir.join(DEFAULT_DATA_DIR)),
            server: ServerConfig {
                host: DEFAULT_HOST.to_owned(),
                port: DEFAULT_PORT,
                request_timeout_secs: 30,
                shutdown_timeout_secs: 30,
            },
            database: DatabaseConfig {
                synchronous: Synchronous::Full,
                busy_timeout_ms: 5_000,
                read_pool_size: 8,
            },
            log: LogConfig {
                level: "info".to_owned(),
                format: LogFormat::Pretty,
            },
            resp: RespConfig {
                enabled: false,
                host: DEFAULT_HOST.to_owned(),
                port: DEFAULT_RESP_PORT,
                max_connections: 1024,
                max_bulk_bytes: 16 * 1024 * 1024,
                idle_timeout_secs: 300,
            },
        }
    }

    /// Checks value ranges. Called by the loader; call it again after
    /// mutating a config by hand.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let invalid = |field: &'static str, message: String| ConfigError::Invalid { field, message };
        if self.server.host.trim().is_empty() || self.server.host.contains(char::is_whitespace) {
            return Err(invalid("server.host", format!("invalid host {:?}", self.server.host)));
        }
        if !(1..=3600).contains(&self.server.request_timeout_secs) {
            return Err(invalid(
                "server.request_timeout_secs",
                "must be between 1 and 3600".into(),
            ));
        }
        if self.server.shutdown_timeout_secs > 3600 {
            return Err(invalid("server.shutdown_timeout_secs", "must be at most 3600".into()));
        }
        if !(1..=64).contains(&self.database.read_pool_size) {
            return Err(invalid("database.read_pool_size", "must be between 1 and 64".into()));
        }
        if self.database.busy_timeout_ms > 600_000 {
            return Err(invalid("database.busy_timeout_ms", "must be at most 600000".into()));
        }
        if self.log.level.trim().is_empty() {
            return Err(invalid("log.level", "must not be empty".into()));
        }
        if self.resp.host.trim().is_empty() || self.resp.host.contains(char::is_whitespace) {
            return Err(invalid("resp.host", format!("invalid host {:?}", self.resp.host)));
        }
        if !(1..=1_000_000).contains(&self.resp.max_connections) {
            return Err(invalid("resp.max_connections", "must be between 1 and 1000000".into()));
        }
        if !(1..=512 * 1024 * 1024).contains(&self.resp.max_bulk_bytes) {
            return Err(invalid("resp.max_bulk_bytes", "must be between 1 and 536870912".into()));
        }
        if self.resp.idle_timeout_secs > 86_400 {
            return Err(invalid("resp.idle_timeout_secs", "must be at most 86400".into()));
        }
        Ok(())
    }

    fn apply(&mut self, o: &ConfigOverrides, base_dir: &Path) {
        if let Some(v) = &o.data_dir {
            self.data_dir = normalize(&base_dir.join(v));
        }
        if let Some(v) = &o.host {
            self.server.host.clone_from(v);
        }
        if let Some(v) = o.port {
            self.server.port = v;
        }
        if let Some(v) = o.request_timeout_secs {
            self.server.request_timeout_secs = v;
        }
        if let Some(v) = o.shutdown_timeout_secs {
            self.server.shutdown_timeout_secs = v;
        }
        if let Some(v) = o.synchronous {
            self.database.synchronous = v;
        }
        if let Some(v) = o.busy_timeout_ms {
            self.database.busy_timeout_ms = v;
        }
        if let Some(v) = o.read_pool_size {
            self.database.read_pool_size = v;
        }
        if let Some(v) = &o.log_level {
            self.log.level.clone_from(v);
        }
        if let Some(v) = o.log_format {
            self.log.format = v;
        }
        if let Some(v) = o.resp_enabled {
            self.resp.enabled = v;
        }
        if let Some(v) = &o.resp_host {
            self.resp.host.clone_from(v);
        }
        if let Some(v) = o.resp_port {
            self.resp.port = v;
        }
        if let Some(v) = o.resp_max_connections {
            self.resp.max_connections = v;
        }
        if let Some(v) = o.resp_max_bulk_bytes {
            self.resp.max_bulk_bytes = v;
        }
        if let Some(v) = o.resp_idle_timeout_secs {
            self.resp.idle_timeout_secs = v;
        }
    }
}

/// A flat set of optional values from one source (environment or CLI).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigOverrides {
    /// Config file to load (only meaningful for CLI and environment).
    pub config_file: Option<PathBuf>,
    /// Data directory (relative to the working directory).
    pub data_dir: Option<PathBuf>,
    /// Bind host.
    pub host: Option<String>,
    /// Bind port.
    pub port: Option<u16>,
    /// Per-request timeout in seconds.
    pub request_timeout_secs: Option<u64>,
    /// Graceful shutdown timeout in seconds.
    pub shutdown_timeout_secs: Option<u64>,
    /// SQLite `synchronous` mode.
    pub synchronous: Option<Synchronous>,
    /// SQLite busy timeout in milliseconds.
    pub busy_timeout_ms: Option<u64>,
    /// Read pool size.
    pub read_pool_size: Option<u32>,
    /// Log filter.
    pub log_level: Option<String>,
    /// Log format.
    pub log_format: Option<LogFormat>,
    /// Run the RESP listener.
    pub resp_enabled: Option<bool>,
    /// RESP bind host.
    pub resp_host: Option<String>,
    /// RESP bind port.
    pub resp_port: Option<u16>,
    /// Most RESP connections.
    pub resp_max_connections: Option<u32>,
    /// Largest RESP bulk string in bytes.
    pub resp_max_bulk_bytes: Option<u64>,
    /// RESP idle timeout in seconds.
    pub resp_idle_timeout_secs: Option<u64>,
}

impl ConfigOverrides {
    /// Reads `FORGEBASE_*` variables from `vars` (pass `std::env::vars()`).
    /// Unknown `FORGEBASE_*` names are rejected so typos do not pass silently.
    pub fn from_env<I>(vars: I) -> Result<Self, ConfigError>
    where
        I: IntoIterator<Item = (String, String)>,
    {
        let mut o = ConfigOverrides::default();
        for (name, value) in vars {
            let Some(key) = name.strip_prefix(ENV_PREFIX) else {
                continue;
            };
            let bad = |message: String| ConfigError::InvalidEnv {
                var: name.clone(),
                message,
            };
            match key {
                "CONFIG" => o.config_file = Some(PathBuf::from(&value)),
                "DATA_DIR" => o.data_dir = Some(PathBuf::from(&value)),
                "HOST" => o.host = Some(value),
                "PORT" => o.port = Some(parse_num(&value).map_err(bad)?),
                "REQUEST_TIMEOUT_SECS" => o.request_timeout_secs = Some(parse_num(&value).map_err(bad)?),
                "SHUTDOWN_TIMEOUT_SECS" => o.shutdown_timeout_secs = Some(parse_num(&value).map_err(bad)?),
                "DB_SYNCHRONOUS" => o.synchronous = Some(value.parse().map_err(bad)?),
                "DB_BUSY_TIMEOUT_MS" => o.busy_timeout_ms = Some(parse_num(&value).map_err(bad)?),
                "DB_READ_POOL_SIZE" => o.read_pool_size = Some(parse_num(&value).map_err(bad)?),
                "LOG_LEVEL" => o.log_level = Some(value),
                "LOG_FORMAT" => o.log_format = Some(value.parse().map_err(bad)?),
                "RESP_ENABLED" => o.resp_enabled = Some(parse_bool(&value).map_err(bad)?),
                "RESP_HOST" => o.resp_host = Some(value),
                "RESP_PORT" => o.resp_port = Some(parse_num(&value).map_err(bad)?),
                "RESP_MAX_CONNECTIONS" => o.resp_max_connections = Some(parse_num(&value).map_err(bad)?),
                "RESP_MAX_BULK_BYTES" => o.resp_max_bulk_bytes = Some(parse_num(&value).map_err(bad)?),
                "RESP_IDLE_TIMEOUT_SECS" => o.resp_idle_timeout_secs = Some(parse_num(&value).map_err(bad)?),
                _ => return Err(bad("unknown ForgeBase environment variable".into())),
            }
        }
        Ok(o)
    }
}

fn parse_bool(value: &str) -> Result<bool, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(format!("{value:?}: expected true or false")),
    }
}

fn parse_num<T: FromStr>(value: &str) -> Result<T, String>
where
    T::Err: fmt::Display,
{
    value.trim().parse::<T>().map_err(|e| format!("{value:?}: {e}"))
}

/// The TOML file shape. Every field is optional; unknown keys are errors.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    data_dir: Option<PathBuf>,
    #[serde(default)]
    server: FileServer,
    #[serde(default)]
    database: FileDatabase,
    #[serde(default)]
    log: FileLog,
    #[serde(default)]
    resp: FileResp,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileResp {
    enabled: Option<bool>,
    host: Option<String>,
    port: Option<u16>,
    max_connections: Option<u32>,
    max_bulk_bytes: Option<u64>,
    idle_timeout_secs: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileServer {
    host: Option<String>,
    port: Option<u16>,
    request_timeout_secs: Option<u64>,
    shutdown_timeout_secs: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileDatabase {
    synchronous: Option<Synchronous>,
    busy_timeout_ms: Option<u64>,
    read_pool_size: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileLog {
    level: Option<String>,
    format: Option<LogFormat>,
}

impl From<FileConfig> for ConfigOverrides {
    fn from(f: FileConfig) -> Self {
        ConfigOverrides {
            config_file: None,
            data_dir: f.data_dir,
            host: f.server.host,
            port: f.server.port,
            request_timeout_secs: f.server.request_timeout_secs,
            shutdown_timeout_secs: f.server.shutdown_timeout_secs,
            synchronous: f.database.synchronous,
            busy_timeout_ms: f.database.busy_timeout_ms,
            read_pool_size: f.database.read_pool_size,
            log_level: f.log.level,
            log_format: f.log.format,
            resp_enabled: f.resp.enabled,
            resp_host: f.resp.host,
            resp_port: f.resp.port,
            resp_max_connections: f.resp.max_connections,
            resp_max_bulk_bytes: f.resp.max_bulk_bytes,
            resp_idle_timeout_secs: f.resp.idle_timeout_secs,
        }
    }
}

/// Result of loading: the config plus the file it came from, if any.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedConfig {
    /// The resolved configuration.
    pub config: Config,
    /// Absolute path of the TOML file that was read, if one was.
    pub file: Option<PathBuf>,
}

/// Loads configuration from all sources with the documented precedence.
#[derive(Debug, Clone)]
pub struct ConfigLoader {
    cwd: PathBuf,
    env: ConfigOverrides,
    cli: ConfigOverrides,
}

impl ConfigLoader {
    /// Loader resolving relative paths against `cwd`, with no env or CLI values.
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            env: ConfigOverrides::default(),
            cli: ConfigOverrides::default(),
        }
    }

    /// Sets the environment layer.
    pub fn env(mut self, env: ConfigOverrides) -> Self {
        self.env = env;
        self
    }

    /// Sets the command-line layer.
    pub fn cli(mut self, cli: ConfigOverrides) -> Self {
        self.cli = cli;
        self
    }

    /// Path of the config file that [`ConfigLoader::load`] will read, and
    /// whether it was explicitly requested (an explicit file must exist).
    pub fn config_file(&self) -> Option<(PathBuf, bool)> {
        if let Some(p) = &self.cli.config_file {
            return Some((normalize(&self.cwd.join(p)), true));
        }
        if let Some(p) = &self.env.config_file {
            return Some((normalize(&self.cwd.join(p)), true));
        }
        let implicit = self.cwd.join(DEFAULT_CONFIG_FILE);
        implicit.is_file().then(|| (normalize(&implicit), false))
    }

    /// Resolves the final configuration.
    pub fn load(self) -> Result<LoadedConfig, ConfigError> {
        let file = match self.config_file() {
            Some((path, explicit)) => {
                if !path.is_file() {
                    if explicit {
                        return Err(ConfigError::MissingFile(path));
                    }
                    None
                } else {
                    Some(path)
                }
            }
            None => None,
        };

        let (mut config, file_layer) = match &file {
            Some(path) => {
                let dir = path.parent().map(Path::to_path_buf).unwrap_or_else(|| self.cwd.clone());
                let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
                    path: path.clone(),
                    source,
                })?;
                let parsed: FileConfig = toml::from_str(&text).map_err(|e| ConfigError::Parse {
                    path: path.clone(),
                    message: e.to_string(),
                })?;
                (Config::defaults(&dir), Some((ConfigOverrides::from(parsed), dir)))
            }
            None => (Config::defaults(&self.cwd), None),
        };

        if let Some((layer, dir)) = &file_layer {
            config.apply(layer, dir);
        }
        config.apply(&self.env, &self.cwd);
        config.apply(&self.cli, &self.cwd);
        config.validate()?;
        Ok(LoadedConfig { config, file })
    }
}

/// Lexically normalises a path: drops `.` components and folds `..` where
/// possible. Does not touch the filesystem (the path may not exist yet).
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Configuration errors.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// An explicitly requested config file does not exist.
    #[error("config file {0} does not exist")]
    MissingFile(PathBuf),
    /// The config file could not be read.
    #[error("cannot read config file {path}: {source}")]
    Read {
        /// File path.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The config file is not valid TOML or has unknown keys.
    #[error("invalid config file {path}: {message}")]
    Parse {
        /// File path.
        path: PathBuf,
        /// Parser message.
        message: String,
    },
    /// An environment variable has an invalid value or unknown name.
    #[error("invalid environment variable {var}: {message}")]
    InvalidEnv {
        /// Variable name.
        var: String,
        /// What is wrong.
        message: String,
    },
    /// A value is out of range.
    #[error("invalid configuration value {field}: {message}")]
    Invalid {
        /// Dotted field name.
        field: &'static str,
        /// What is wrong.
        message: String,
    },
}

impl ErrorCode for ConfigError {
    fn code(&self) -> &'static str {
        match self {
            ConfigError::MissingFile(_) => "config_file_missing",
            ConfigError::Read { .. } => "config_file_unreadable",
            ConfigError::Parse { .. } => "config_file_invalid",
            ConfigError::InvalidEnv { .. } => "config_env_invalid",
            ConfigError::Invalid { .. } => "config_value_invalid",
        }
    }

    fn kind(&self) -> ErrorKind {
        ErrorKind::InvalidInput
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> ConfigOverrides {
        ConfigOverrides::from_env(pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))).unwrap()
    }

    #[test]
    fn defaults_without_any_source() {
        let tmp = tempfile::tempdir().unwrap();
        let loaded = ConfigLoader::new(tmp.path()).load().unwrap();
        assert_eq!(loaded.file, None);
        assert_eq!(loaded.config, Config::defaults(tmp.path()));
        assert_eq!(loaded.config.server.host, "127.0.0.1");
        assert_eq!(loaded.config.server.port, 8090);
        assert_eq!(loaded.config.data_dir, tmp.path().join("forgebase-data"));
    }

    #[test]
    fn precedence_is_cli_then_env_then_file_then_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("forgebase.toml"),
            "data_dir = \"from-file\"\n[server]\nport = 1111\nhost = \"0.0.0.0\"\n[database]\nread_pool_size = 3\n[log]\nlevel = \"debug\"\n",
        )
        .unwrap();

        // file only
        let c = ConfigLoader::new(tmp.path()).load().unwrap().config;
        assert_eq!(c.server.port, 1111);
        assert_eq!(c.server.host, "0.0.0.0");
        assert_eq!(c.database.read_pool_size, 3);
        assert_eq!(c.data_dir, tmp.path().join("from-file"));
        assert_eq!(c.database.synchronous, Synchronous::Full, "default kept");

        // env beats file
        let e = env(&[
            ("FORGEBASE_PORT", "2222"),
            ("FORGEBASE_LOG_LEVEL", "warn"),
            ("HOME", "/x"),
        ]);
        let c = ConfigLoader::new(tmp.path()).env(e.clone()).load().unwrap().config;
        assert_eq!(c.server.port, 2222);
        assert_eq!(c.log.level, "warn");
        assert_eq!(c.server.host, "0.0.0.0", "file value kept where env is silent");

        // cli beats env
        let cli = ConfigOverrides {
            port: Some(3333),
            data_dir: Some("cli-data".into()),
            ..Default::default()
        };
        let c = ConfigLoader::new(tmp.path()).env(e).cli(cli).load().unwrap().config;
        assert_eq!(c.server.port, 3333);
        assert_eq!(c.log.level, "warn");
        assert_eq!(c.data_dir, tmp.path().join("cli-data"));
    }

    #[test]
    fn resp_listener_is_off_by_default_and_configurable_from_every_source() {
        let tmp = tempfile::tempdir().unwrap();
        let d = ConfigLoader::new(tmp.path()).load().unwrap().config.resp;
        assert!(!d.enabled);
        assert_eq!((d.host.as_str(), d.port), ("127.0.0.1", 6380));
        std::fs::write(
            tmp.path().join("forgebase.toml"),
            "[resp]\nenabled = true\nport = 7000\nmax_connections = 5\n",
        )
        .unwrap();
        let c = ConfigLoader::new(tmp.path()).load().unwrap().config.resp;
        assert!(c.enabled);
        assert_eq!((c.port, c.max_connections), (7000, 5));
        let e = env(&[
            ("FORGEBASE_RESP_PORT", "7001"),
            ("FORGEBASE_RESP_IDLE_TIMEOUT_SECS", "9"),
        ]);
        let cli = ConfigOverrides {
            resp_port: Some(7002),
            resp_host: Some("::1".into()),
            ..Default::default()
        };
        let c = ConfigLoader::new(tmp.path())
            .env(e)
            .cli(cli)
            .load()
            .unwrap()
            .config
            .resp;
        assert!(c.enabled, "file value kept");
        assert_eq!((c.port, c.host.as_str(), c.idle_timeout_secs), (7002, "::1", 9));
        assert_eq!(env(&[("FORGEBASE_RESP_ENABLED", "on")]).resp_enabled, Some(true));
        assert!(ConfigOverrides::from_env([("FORGEBASE_RESP_ENABLED".to_owned(), "maybe".to_owned())]).is_err());
        let mut bad = Config::defaults(tmp.path());
        bad.resp.max_bulk_bytes = 0;
        assert_eq!(bad.validate().unwrap_err().code(), "config_value_invalid");
    }

    #[test]
    fn file_data_dir_is_relative_to_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("proj");
        std::fs::create_dir(&proj).unwrap();
        std::fs::write(proj.join("forgebase.toml"), "data_dir = \"./data\"\n").unwrap();
        let cli = ConfigOverrides {
            config_file: Some("proj/forgebase.toml".into()),
            ..Default::default()
        };
        let loaded = ConfigLoader::new(tmp.path()).cli(cli).load().unwrap();
        assert_eq!(loaded.config.data_dir, proj.join("data"));
        assert_eq!(loaded.file, Some(proj.join("forgebase.toml")));
    }

    #[test]
    fn explicit_missing_file_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let e = env(&[("FORGEBASE_CONFIG", "nope.toml")]);
        let err = ConfigLoader::new(tmp.path()).env(e).load().unwrap_err();
        assert_eq!(err.code(), "config_file_missing");
    }

    #[test]
    fn unknown_keys_and_bad_values_are_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("forgebase.toml"), "[server]\nprot = 1\n").unwrap();
        let err = ConfigLoader::new(tmp.path()).load().unwrap_err();
        assert_eq!(err.code(), "config_file_invalid");

        let err = ConfigOverrides::from_env([("FORGEBASE_PORT".to_owned(), "99999".to_owned())]).unwrap_err();
        assert_eq!(err.code(), "config_env_invalid");
        let err = ConfigOverrides::from_env([("FORGEBASE_PROT".to_owned(), "1".to_owned())]).unwrap_err();
        assert_eq!(err.code(), "config_env_invalid");

        let cli = ConfigOverrides {
            read_pool_size: Some(0),
            ..Default::default()
        };
        let err = ConfigLoader::new(tmp.path().join("elsewhere"))
            .cli(cli)
            .load()
            .unwrap_err();
        assert_eq!(err.code(), "config_value_invalid");
    }

    #[test]
    fn template_parses_and_matches_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("forgebase.toml"), CONFIG_TEMPLATE).unwrap();
        let c = ConfigLoader::new(tmp.path()).load().unwrap().config;
        assert_eq!(c, Config::defaults(tmp.path()));
    }

    #[test]
    fn normalize_folds_dots() {
        assert_eq!(normalize(Path::new("/a/./b/../c")), PathBuf::from("/a/c"));
    }
}
