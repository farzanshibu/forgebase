//! Errors of the engine crate.

use std::io;
use std::path::{Path, PathBuf};

/// Everything that can stop the engine from being prepared or run.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// ForgeBase refuses to run a managed engine as root (ADR-0028 §4.2).
    #[error(
        "the bundled PostgreSQL cannot run as root: run ForgeBase as an unprivileged user, \
         or use [database.postgres] mode = \"external\""
    )]
    Root,
    /// No engine is available (no embedded archive, directory or archive path).
    #[error("{0}")]
    NoEngine(String),
    /// The engine directory is unusable (missing programs, wrong version).
    #[error("PostgreSQL engine {dir}: {reason}")]
    Engine {
        /// Engine directory.
        dir: PathBuf,
        /// What is wrong.
        reason: String,
    },
    /// An engine archive is malformed or does not match its manifest.
    #[error("engine archive: {0}")]
    Archive(String),
    /// Invalid configuration (deny-listed or malformed settings, bad listen address).
    #[error("database.postgres: {0}")]
    Config(String),
    /// The cluster on disk cannot be used with this engine or layout.
    #[error("PostgreSQL cluster {dir}: {reason}")]
    Cluster {
        /// Data directory (PGDATA).
        dir: PathBuf,
        /// What is wrong.
        reason: String,
    },
    /// `initdb` failed.
    #[error("initdb failed: {0}")]
    Initdb(String),
    /// The engine did not start or did not become ready.
    #[error("PostgreSQL did not start: {reason}{}", format_log(log))]
    Start {
        /// What happened.
        reason: String,
        /// The engine's last log lines.
        log: Vec<String>,
    },
    /// A bootstrap or readiness SQL step failed.
    #[error("PostgreSQL bootstrap: {0}")]
    Sql(String),
    /// The engine was stopped or failed and cannot serve the request.
    #[error("PostgreSQL engine is {0}")]
    State(String),
    /// A file-system step failed.
    #[error("{context}: {source}")]
    Io {
        /// What was being done, with the path.
        context: String,
        /// The I/O error.
        #[source]
        source: io::Error,
    },
}

fn format_log(log: &[String]) -> String {
    if log.is_empty() {
        String::new()
    } else {
        format!("\nlast engine log lines:\n  {}", log.join("\n  "))
    }
}

impl EngineError {
    /// An I/O error with what was being done to `path`.
    pub(crate) fn io(what: &str, path: &Path, source: io::Error) -> Self {
        Self::Io {
            context: format!("{what} {}", path.display()),
            source,
        }
    }
}

/// Result alias.
pub type Result<T, E = EngineError> = std::result::Result<T, E>;
