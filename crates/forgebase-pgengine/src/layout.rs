//! Where a cluster lives inside the data directory (ADR-0028 §4.1, ADR-0029
//! §2.2).
//!
//! ```text
//! <data_dir>/postgres/
//! ├── engine/<version>-<build>-<sha12>/   extracted engines (shared by every node)
//! ├── roles.secret                        bootstrap role passwords (shared, 0600)
//! ├── data/  run/  forgebase-postgres.conf          the primary
//! └── nodes/<name>/data/  run/  forgebase-postgres.conf   other clusters
//! ```
//!
//! The primary is the cluster of a single-server deployment. Other clusters
//! on the same host (managed replicas, FB-168; shard primaries, FB-174) use a
//! node layout: their own data, socket directory and settings file, the same
//! extracted engine and the same role passwords (a replica copies the
//! primary's roles; a shard is bootstrapped with the same ones).

use std::path::{Path, PathBuf};

use crate::error::{EngineError, Result};

/// What kind of cluster a layout holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    /// A writable cluster: `initdb` on first start, roles bootstrapped on
    /// every start. The primary, and every shard primary.
    Primary,
    /// A hot standby. Its data directory is provisioned by the caller
    /// (`pg_basebackup -R`, FB-168) before [`crate::Engine::prepare`]; no
    /// `initdb`, no bootstrap (it is read-only), `hot_standby` settings.
    Standby,
}

/// Paths of one cluster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    postgres: PathBuf,
    node: Option<String>,
    kind: NodeKind,
}

impl Layout {
    /// The primary cluster of `data_dir` (`<data_dir>/postgres/data`).
    pub fn primary(data_dir: impl AsRef<Path>) -> Self {
        Self {
            postgres: data_dir.as_ref().join("postgres"),
            node: None,
            kind: NodeKind::Primary,
        }
    }

    /// Another cluster on this host: `<data_dir>/postgres/nodes/<name>/`.
    ///
    /// # Errors
    /// When `name` is not 1–32 characters of `a-z`, `0-9`, `-` or `_`
    /// starting with a letter or digit, or is `primary`.
    pub fn node(data_dir: impl AsRef<Path>, name: &str, kind: NodeKind) -> Result<Self> {
        let valid = (1..=32).contains(&name.len())
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
            && name.as_bytes()[0].is_ascii_alphanumeric()
            && name != "primary";
        if !valid {
            return Err(EngineError::Config(format!(
                "node name {name:?} must be 1-32 characters of a-z, 0-9, '-' or '_', start with a \
                 letter or digit, and not be \"primary\""
            )));
        }
        Ok(Self {
            postgres: data_dir.as_ref().join("postgres"),
            node: Some(name.to_owned()),
            kind,
        })
    }

    /// The same paths holding another kind of cluster (FB-168: a promoted
    /// replica is a primary; a rejoined old primary is a standby).
    #[must_use]
    pub fn with_kind(mut self, kind: NodeKind) -> Self {
        self.kind = kind;
        self
    }

    /// `<data_dir>/postgres`.
    pub fn postgres_dir(&self) -> &Path {
        &self.postgres
    }

    /// The node name: `primary`, or the name given to [`Layout::node`].
    pub fn name(&self) -> &str {
        self.node.as_deref().unwrap_or("primary")
    }

    /// Whether this is the primary layout (not a node layout).
    pub fn is_primary_layout(&self) -> bool {
        self.node.is_none()
    }

    /// What the cluster is.
    pub fn kind(&self) -> NodeKind {
        self.kind
    }

    /// The directory holding this cluster's data, socket and settings.
    pub fn node_dir(&self) -> PathBuf {
        match &self.node {
            None => self.postgres.clone(),
            Some(n) => self.postgres.join("nodes").join(n),
        }
    }

    /// PGDATA (mode 0700).
    pub fn pgdata(&self) -> PathBuf {
        self.node_dir().join("data")
    }

    /// The preferred Unix socket directory (mode 0700). The engine falls back
    /// to a short private directory when the socket path would be too long.
    pub fn run_dir(&self) -> PathBuf {
        self.node_dir().join("run")
    }

    /// `forgebase-postgres.conf`, written on every start and included from
    /// `postgresql.conf` as `../forgebase-postgres.conf`.
    pub fn conf_file(&self) -> PathBuf {
        self.node_dir().join(CONF_FILE)
    }

    /// `roles.secret`, shared by every node.
    pub fn secrets_file(&self) -> PathBuf {
        self.postgres.join("roles.secret")
    }

    /// Extracted engines, shared by every node.
    pub fn engines_dir(&self) -> PathBuf {
        self.postgres.join("engine")
    }
}

/// Name of the settings file ForgeBase writes.
pub const CONF_FILE: &str = "forgebase-postgres.conf";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primary_paths() {
        let l = Layout::primary("/d");
        assert_eq!(l.name(), "primary");
        assert_eq!(l.pgdata(), Path::new("/d/postgres/data"));
        assert_eq!(l.run_dir(), Path::new("/d/postgres/run"));
        assert_eq!(l.conf_file(), Path::new("/d/postgres/forgebase-postgres.conf"));
        assert_eq!(l.secrets_file(), Path::new("/d/postgres/roles.secret"));
        assert_eq!(l.engines_dir(), Path::new("/d/postgres/engine"));
        assert_eq!(l.kind(), NodeKind::Primary);
        assert!(l.is_primary_layout());
    }

    #[test]
    fn node_paths_share_engine_and_secrets() {
        let l = Layout::node("/d", "replica-1", NodeKind::Standby).expect("valid");
        assert_eq!(l.pgdata(), Path::new("/d/postgres/nodes/replica-1/data"));
        assert_eq!(l.run_dir(), Path::new("/d/postgres/nodes/replica-1/run"));
        assert_eq!(
            l.conf_file(),
            Path::new("/d/postgres/nodes/replica-1/forgebase-postgres.conf")
        );
        assert_eq!(l.secrets_file(), Path::new("/d/postgres/roles.secret"));
        assert_eq!(l.engines_dir(), Path::new("/d/postgres/engine"));
        assert!(!l.is_primary_layout());
    }

    #[test]
    fn node_names_are_checked() {
        for bad in ["", "primary", "-x", "A", "a/b", "..", &"x".repeat(33)] {
            assert!(Layout::node("/d", bad, NodeKind::Primary).is_err(), "{bad:?}");
        }
        for good in ["shard-1", "r1", "eu_read_1"] {
            assert!(Layout::node("/d", good, NodeKind::Primary).is_ok(), "{good:?}");
        }
    }
}
