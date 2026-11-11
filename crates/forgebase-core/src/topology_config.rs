//! `[database.replicas]`, `[database.sharding]` and `[[database.nodes]]`
//! (FB-175, ADR-0029 §4.1).
//!
//! Only how replicas and shards behave is file configuration. The topology
//! itself (sets, nodes, placements) lives in control tables and changes
//! through `/api/system/database/*`; nodes declared here are seeded into
//! those tables at start as `origin = config` (read-only in the dashboard).
//! A declared node's connection URL is never in the file: `url_env` names
//! the environment variable that holds it, and the server stores it sealed.
//!
//! Every key is in the FB-91 registry ([`crate::features`]) and has an
//! environment variable `FORGEBASE_DATABASE_REPLICAS_<KEY>` /
//! `FORGEBASE_DATABASE_SHARDING_<KEY>`. Nodes are a list of tables and come
//! from the file only.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use super::{ConfigError, parse_bool, parse_num, whole};

/// Which server answers reads that may go to a replica (`read_routing`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadRouting {
    /// Every read goes to the primary.
    Primary,
    /// Opt-in reads go to a healthy replica when the set has one (default).
    #[default]
    Replicas,
}

impl ReadRouting {
    /// The configuration value.
    pub fn as_str(self) -> &'static str {
        match self {
            ReadRouting::Primary => "primary",
            ReadRouting::Replicas => "replicas",
        }
    }
}

impl FromStr for ReadRouting {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim() {
            "primary" => Ok(ReadRouting::Primary),
            "replicas" => Ok(ReadRouting::Replicas),
            other => Err(format!("{other:?}: expected primary or replicas")),
        }
    }
}

impl fmt::Display for ReadRouting {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where new tenants go (`[database.sharding] placement`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TenantPlacement {
    /// The set with the lowest load score (default).
    #[default]
    LeastLoaded,
    /// New tenants stay on control.
    Control,
}

impl TenantPlacement {
    /// The configuration value.
    pub fn as_str(self) -> &'static str {
        match self {
            TenantPlacement::LeastLoaded => "least_loaded",
            TenantPlacement::Control => "control",
        }
    }
}

impl FromStr for TenantPlacement {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim() {
            "least_loaded" => Ok(TenantPlacement::LeastLoaded),
            "control" => Ok(TenantPlacement::Control),
            other => Err(format!("{other:?}: expected least_loaded or control")),
        }
    }
}

/// Role of a declared node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeclaredRole {
    /// Streams from its set's primary and serves opt-in reads (default).
    #[default]
    Replica,
    /// The primary of a shard that the operator runs (not of control).
    Primary,
}

impl DeclaredRole {
    /// The configuration value.
    pub fn as_str(self) -> &'static str {
        match self {
            DeclaredRole::Replica => "replica",
            DeclaredRole::Primary => "primary",
        }
    }
}

/// `[database.replicas]`: replica reads, lag limits and slot limits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReplicasConfig {
    /// `primary` or `replicas`.
    pub read_routing: ReadRouting,
    /// A caller's reads see its own writes (commit tokens, ADR-0029 §2.6).
    pub read_your_writes: bool,
    /// How long a read waits for a replica to catch up before it reads the primary.
    pub lsn_wait_ms: u64,
    /// A replica further behind than this leaves rotation.
    pub max_lag_ms: u64,
    /// Or more bytes of write-ahead log behind than this.
    pub max_lag_bytes: u64,
    /// How often replica health is sampled.
    pub health_interval_ms: u64,
    /// Most write-ahead log a primary keeps for a replica that falls behind
    /// (`max_slot_wal_keep_size`, a PostgreSQL size such as `8GB`).
    pub slot_wal_keep_size: String,
}

impl Default for ReplicasConfig {
    fn default() -> Self {
        Self {
            read_routing: ReadRouting::Replicas,
            read_your_writes: true,
            lsn_wait_ms: 50,
            max_lag_ms: 2_000,
            max_lag_bytes: 64 * 1024 * 1024,
            health_interval_ms: 500,
            slot_wal_keep_size: "8GB".to_owned(),
        }
    }
}

/// `[database.sharding]`: placement of new tenants and online moves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShardingConfig {
    /// `least_loaded` or `control`.
    pub placement: TenantPlacement,
    /// New environments of a project go where the project's first one is.
    pub project_affinity: bool,
    /// Longest pause of a tenant's requests while a move switches over.
    pub move_freeze_timeout_ms: u64,
    /// Hours a moved tenant's rows stay on the old set (undo window).
    pub move_source_retention_hours: u64,
    /// Tenant moves running at the same time.
    pub max_parallel_moves: u32,
}

impl Default for ShardingConfig {
    fn default() -> Self {
        Self {
            placement: TenantPlacement::LeastLoaded,
            project_affinity: true,
            move_freeze_timeout_ms: 10_000,
            move_source_retention_hours: 24,
            max_parallel_moves: 1,
        }
    }
}

/// One `[[database.nodes]]` entry: an external server declared in the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeDecl {
    /// Node name (`a-z0-9-_`, 1 to 32, not `primary`).
    pub name: String,
    /// `control` or a shard name.
    #[serde(default = "control_set")]
    pub set: String,
    /// `replica` (default) or `primary` (a shard's primary only).
    #[serde(default)]
    pub role: DeclaredRole,
    /// Environment variable that holds the connection URL (never the URL itself).
    pub url_env: String,
    /// Share of replica reads (0 takes it out of rotation).
    #[serde(default = "one")]
    pub weight: u32,
}

fn control_set() -> String {
    "control".to_owned()
}

fn one() -> u32 {
    1
}

/// The three sections together: one field of
/// [`super::DatabaseConfig`], flattened so the registry keys read
/// `database.replicas.*`, `database.sharding.*` and `database.nodes`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct TopologyConfig {
    /// `[database.replicas]`.
    pub replicas: ReplicasConfig,
    /// `[database.sharding]`.
    pub sharding: ShardingConfig,
    /// `[[database.nodes]]`.
    pub nodes: Vec<NodeDecl>,
}

/// `[database.replicas]` from the file or the environment.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplicasOverrides {
    /// See [`ReplicasConfig::read_routing`].
    pub read_routing: Option<ReadRouting>,
    /// See [`ReplicasConfig::read_your_writes`].
    pub read_your_writes: Option<bool>,
    /// See [`ReplicasConfig::lsn_wait_ms`].
    pub lsn_wait_ms: Option<u64>,
    /// See [`ReplicasConfig::max_lag_ms`].
    pub max_lag_ms: Option<u64>,
    /// See [`ReplicasConfig::max_lag_bytes`].
    pub max_lag_bytes: Option<u64>,
    /// See [`ReplicasConfig::health_interval_ms`].
    pub health_interval_ms: Option<u64>,
    /// See [`ReplicasConfig::slot_wal_keep_size`].
    pub slot_wal_keep_size: Option<String>,
}

/// `[database.sharding]` from the file or the environment.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShardingOverrides {
    /// See [`ShardingConfig::placement`].
    pub placement: Option<TenantPlacement>,
    /// See [`ShardingConfig::project_affinity`].
    pub project_affinity: Option<bool>,
    /// See [`ShardingConfig::move_freeze_timeout_ms`].
    pub move_freeze_timeout_ms: Option<u64>,
    /// See [`ShardingConfig::move_source_retention_hours`].
    pub move_source_retention_hours: Option<u64>,
    /// See [`ShardingConfig::max_parallel_moves`].
    pub max_parallel_moves: Option<u32>,
}

/// The topology layer of one configuration source.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TopologyOverrides {
    /// `[database.replicas]`.
    pub replicas: Option<ReplicasOverrides>,
    /// `[database.sharding]`.
    pub sharding: Option<ShardingOverrides>,
    /// `[[database.nodes]]` (file only; replaces the list).
    pub nodes: Option<Vec<NodeDecl>>,
}

impl TopologyOverrides {
    fn replicas(&mut self) -> &mut ReplicasOverrides {
        self.replicas.get_or_insert_with(ReplicasOverrides::default)
    }

    fn sharding(&mut self) -> &mut ShardingOverrides {
        self.sharding.get_or_insert_with(ShardingOverrides::default)
    }

    /// Reads one environment variable: `key` is the name after
    /// `FORGEBASE_`. `Ok(false)` when it is not a topology variable.
    ///
    /// # Errors
    /// A value that does not parse (the message says what to enter).
    pub fn env(&mut self, key: &str, value: &str) -> Result<bool, String> {
        if let Some(k) = key.strip_prefix("DATABASE_REPLICAS_") {
            let r = self.replicas();
            match k {
                "READ_ROUTING" => r.read_routing = Some(value.parse()?),
                "READ_YOUR_WRITES" => r.read_your_writes = Some(parse_bool(value)?),
                "LSN_WAIT_MS" => r.lsn_wait_ms = Some(parse_num(value)?),
                "MAX_LAG_MS" => r.max_lag_ms = Some(parse_num(value)?),
                "MAX_LAG_BYTES" => r.max_lag_bytes = Some(parse_num(value)?),
                "HEALTH_INTERVAL_MS" => r.health_interval_ms = Some(parse_num(value)?),
                "SLOT_WAL_KEEP_SIZE" => r.slot_wal_keep_size = Some(value.trim().to_owned()),
                _ => return Ok(false),
            }
            return Ok(true);
        }
        if let Some(k) = key.strip_prefix("DATABASE_SHARDING_") {
            let s = self.sharding();
            match k {
                "PLACEMENT" => s.placement = Some(value.parse()?),
                "PROJECT_AFFINITY" => s.project_affinity = Some(parse_bool(value)?),
                "MOVE_FREEZE_TIMEOUT_MS" => s.move_freeze_timeout_ms = Some(parse_num(value)?),
                "MOVE_SOURCE_RETENTION_HOURS" => s.move_source_retention_hours = Some(parse_num(value)?),
                "MAX_PARALLEL_MOVES" => s.max_parallel_moves = Some(parse_num(value)?),
                _ => return Ok(false),
            }
            return Ok(true);
        }
        Ok(false)
    }

    /// Dotted registry keys this layer sets.
    pub fn keys(&self, put: &mut dyn FnMut(bool, &'static str)) {
        if let Some(r) = &self.replicas {
            put(r.read_routing.is_some(), "database.replicas.read_routing");
            put(r.read_your_writes.is_some(), "database.replicas.read_your_writes");
            put(r.lsn_wait_ms.is_some(), "database.replicas.lsn_wait_ms");
            put(r.max_lag_ms.is_some(), "database.replicas.max_lag_ms");
            put(r.max_lag_bytes.is_some(), "database.replicas.max_lag_bytes");
            put(r.health_interval_ms.is_some(), "database.replicas.health_interval_ms");
            put(r.slot_wal_keep_size.is_some(), "database.replicas.slot_wal_keep_size");
        }
        if let Some(s) = &self.sharding {
            put(s.placement.is_some(), "database.sharding.placement");
            put(s.project_affinity.is_some(), "database.sharding.project_affinity");
            put(
                s.move_freeze_timeout_ms.is_some(),
                "database.sharding.move_freeze_timeout_ms",
            );
            put(
                s.move_source_retention_hours.is_some(),
                "database.sharding.move_source_retention_hours",
            );
            put(s.max_parallel_moves.is_some(), "database.sharding.max_parallel_moves");
        }
        put(self.nodes.is_some(), "database.nodes");
    }
}

impl TopologyConfig {
    /// Applies one layer.
    pub fn apply(&mut self, o: &TopologyOverrides) {
        if let Some(r) = &o.replicas {
            let c = &mut self.replicas;
            if let Some(v) = r.read_routing {
                c.read_routing = v;
            }
            if let Some(v) = r.read_your_writes {
                c.read_your_writes = v;
            }
            if let Some(v) = r.lsn_wait_ms {
                c.lsn_wait_ms = v;
            }
            if let Some(v) = r.max_lag_ms {
                c.max_lag_ms = v;
            }
            if let Some(v) = r.max_lag_bytes {
                c.max_lag_bytes = v;
            }
            if let Some(v) = r.health_interval_ms {
                c.health_interval_ms = v;
            }
            if let Some(v) = &r.slot_wal_keep_size {
                v.trim().clone_into(&mut c.slot_wal_keep_size);
            }
        }
        if let Some(s) = &o.sharding {
            let c = &mut self.sharding;
            if let Some(v) = s.placement {
                c.placement = v;
            }
            if let Some(v) = s.project_affinity {
                c.project_affinity = v;
            }
            if let Some(v) = s.move_freeze_timeout_ms {
                c.move_freeze_timeout_ms = v;
            }
            if let Some(v) = s.move_source_retention_hours {
                c.move_source_retention_hours = v;
            }
            if let Some(v) = s.max_parallel_moves {
                c.max_parallel_moves = v;
            }
        }
        if let Some(n) = &o.nodes {
            self.nodes.clone_from(n);
        }
    }

    /// Problems with the values (each says what to enter instead).
    pub fn problems(&self, out: &mut Vec<ConfigError>) {
        let d = TopologyConfig::default();
        let mut invalid = |field: &'static str, message: String| out.push(ConfigError::Invalid { field, message });
        let r = &self.replicas;
        if r.lsn_wait_ms > 10_000 {
            invalid(
                "database.replicas.lsn_wait_ms",
                whole(0, 10_000, "milliseconds", d.replicas.lsn_wait_ms),
            );
        }
        if !(1..=3_600_000).contains(&r.max_lag_ms) {
            invalid(
                "database.replicas.max_lag_ms",
                whole(1, 3_600_000, "milliseconds", d.replicas.max_lag_ms),
            );
        }
        if r.max_lag_bytes == 0 {
            invalid(
                "database.replicas.max_lag_bytes",
                whole(1, u64::MAX, "bytes", d.replicas.max_lag_bytes),
            );
        }
        if !(50..=60_000).contains(&r.health_interval_ms) {
            invalid(
                "database.replicas.health_interval_ms",
                whole(50, 60_000, "milliseconds", d.replicas.health_interval_ms),
            );
        }
        if !is_pg_size(&r.slot_wal_keep_size) {
            invalid(
                "database.replicas.slot_wal_keep_size",
                "must be a size with a unit, such as 8GB, 512MB or 1TB, or -1 for no limit".into(),
            );
        }
        let s = &self.sharding;
        if !(100..=600_000).contains(&s.move_freeze_timeout_ms) {
            invalid(
                "database.sharding.move_freeze_timeout_ms",
                whole(100, 600_000, "milliseconds", d.sharding.move_freeze_timeout_ms),
            );
        }
        if s.move_source_retention_hours > 720 {
            invalid(
                "database.sharding.move_source_retention_hours",
                whole(0, 720, "hours", d.sharding.move_source_retention_hours),
            );
        }
        if !(1..=16).contains(&s.max_parallel_moves) {
            invalid(
                "database.sharding.max_parallel_moves",
                whole(1, 16, "", u64::from(d.sharding.max_parallel_moves)),
            );
        }
        let mut seen: Vec<String> = Vec::new();
        for n in &self.nodes {
            let name = n.name.trim();
            if !is_node_name(name) {
                invalid(
                    "database.nodes",
                    format!(
                        "node name {name:?} must be 1 to 32 lowercase letters, digits, - or _, start with a letter or digit, and not be \"primary\", such as eu-read-1"
                    ),
                );
            } else if seen.iter().any(|s| s.eq_ignore_ascii_case(name)) {
                invalid("database.nodes", format!("node name {name:?} is used twice"));
            } else {
                seen.push(name.to_owned());
            }
            if n.set.trim().is_empty() {
                invalid(
                    "database.nodes",
                    format!("node {name:?}: set must be control or a shard name"),
                );
            }
            if n.role == DeclaredRole::Primary && n.set.trim() == "control" {
                invalid(
                    "database.nodes",
                    format!(
                        "node {name:?}: control's primary is this server's database; declare it as a replica, or as the primary of a shard"
                    ),
                );
            }
            let env = n.url_env.trim();
            if env.is_empty()
                || !env
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
            {
                invalid(
                    "database.nodes",
                    format!(
                        "node {name:?}: url_env must name an environment variable (capital letters, digits and _), such as FORGEBASE_DB_NODE_EU_READ_1_URL; the URL itself never goes in the file"
                    ),
                );
            }
            if n.weight > 1_000 {
                invalid(
                    "database.nodes",
                    format!("node {name:?}: weight must be from 0 to 1000"),
                );
            }
        }
    }
}

/// A node name as [`forgebase_pgengine`]'s layout accepts it (it names
/// directories and replication slots).
pub fn is_node_name(name: &str) -> bool {
    (1..=32).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && name != "primary"
}

/// A PostgreSQL memory size (`8GB`, `512MB`, `1024kB`, `1TB`) or `-1`.
pub fn is_pg_size(s: &str) -> bool {
    let s = s.trim();
    if s == "-1" {
        return true;
    }
    let digits = s.bytes().take_while(u8::is_ascii_digit).count();
    digits > 0 && matches!(&s[digits..], "kB" | "MB" | "GB" | "TB")
}

/// Environment variables that hold declared node URLs
/// (`FORGEBASE_DB_NODE_<NAME>_URL`): secrets the loader never reads.
pub fn is_node_url_env(key: &str) -> bool {
    key.starts_with("DB_NODE_") && key.ends_with("_URL") && key.len() > "DB_NODE__URL".len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_variables_map_to_keys() {
        let mut o = TopologyOverrides::default();
        assert!(o.env("DATABASE_REPLICAS_MAX_LAG_MS", "1500").unwrap());
        assert!(o.env("DATABASE_REPLICAS_READ_ROUTING", "primary").unwrap());
        assert!(o.env("DATABASE_SHARDING_PLACEMENT", "control").unwrap());
        assert!(o.env("DATABASE_SHARDING_PROJECT_AFFINITY", "off").unwrap());
        assert!(!o.env("DATABASE_REPLICAS_NOPE", "1").unwrap());
        assert!(!o.env("DATABASE_BACKEND", "sqlite").unwrap());
        assert!(o.env("DATABASE_REPLICAS_LSN_WAIT_MS", "soon").is_err());
        assert!(o.env("DATABASE_SHARDING_PLACEMENT", "random").is_err());
        let mut keys = Vec::new();
        o.keys(&mut |set, k| {
            if set {
                keys.push(k);
            }
        });
        assert_eq!(
            keys,
            [
                "database.replicas.read_routing",
                "database.replicas.max_lag_ms",
                "database.sharding.placement",
                "database.sharding.project_affinity"
            ]
        );
        let mut c = TopologyConfig::default();
        c.apply(&o);
        assert_eq!(c.replicas.max_lag_ms, 1500);
        assert_eq!(c.replicas.read_routing, ReadRouting::Primary);
        assert_eq!(c.sharding.placement, TenantPlacement::Control);
        assert!(!c.sharding.project_affinity);
        assert_eq!(c.replicas.lsn_wait_ms, 50, "unset keys keep their default");
    }

    #[test]
    fn validation_says_what_to_enter() {
        let mut c = TopologyConfig::default();
        let mut out = Vec::new();
        c.problems(&mut out);
        assert!(out.is_empty(), "defaults are valid: {out:?}");
        c.replicas.max_lag_ms = 0;
        c.replicas.health_interval_ms = 10;
        c.replicas.slot_wal_keep_size = "lots".into();
        c.sharding.max_parallel_moves = 0;
        c.nodes = vec![
            NodeDecl {
                name: "Primary".into(),
                set: "control".into(),
                role: DeclaredRole::Primary,
                url_env: "postgres://u:p@h/db".into(),
                weight: 1,
            },
            NodeDecl {
                name: "eu-1".into(),
                set: "control".into(),
                role: DeclaredRole::Replica,
                url_env: "FORGEBASE_DB_NODE_EU_1_URL".into(),
                weight: 1,
            },
            NodeDecl {
                name: "eu-1".into(),
                set: "control".into(),
                role: DeclaredRole::Replica,
                url_env: "FORGEBASE_DB_NODE_EU_1_URL".into(),
                weight: 1,
            },
        ];
        c.problems(&mut out);
        let text: Vec<String> = out.iter().map(ToString::to_string).collect();
        let all = text.join("\n");
        for needle in [
            "max_lag_ms",
            "health_interval_ms",
            "such as 8GB",
            "max_parallel_moves",
            "\"Primary\" must be 1 to 32",
            "used twice",
            "control's primary",
            "the URL itself never goes in the file",
        ] {
            assert!(all.contains(needle), "{needle} in:\n{all}");
        }
    }

    #[test]
    fn sizes_and_names() {
        for ok in ["8GB", "512MB", "1024kB", "1TB", "-1"] {
            assert!(is_pg_size(ok), "{ok}");
        }
        for bad in ["", "8", "8gb", "GB", "8 GB"] {
            assert!(!is_pg_size(bad), "{bad}");
        }
        assert!(is_node_name("eu-read-1"));
        assert!(!is_node_name("primary"));
        assert!(!is_node_name("-x"));
        assert!(is_node_url_env("DB_NODE_EU_READ_1_URL"));
        assert!(!is_node_url_env("DB_NODE__URL"));
        assert!(!is_node_url_env("DATABASE_URL"));
    }
}
