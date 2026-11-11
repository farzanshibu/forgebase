//! Read replicas and read-your-writes (ADR-0029 §2.1, §2.4–§2.7, FB-167).
//!
//! A database set is one primary and zero or more replicas. Replica reads
//! are **opt-in per call**: [`crate::Backend::fetch_routed`] takes a
//! [`ReadIntent`] and, on every backend except a [`crate::SetHandle`] that
//! has a [`ReplicaSet`], runs on the primary exactly as
//! [`crate::Backend::fetch_all`]. Without replicas nothing below changes
//! behaviour.
//!
//! * [`ReplicaSet`]: the replicas of one set, each a read-only
//!   [`crate::Database`] on the standby, with a health monitor
//!   ([`ReplicaOptions::health_interval`]), lag limits with hysteresis and
//!   the choice among eligible replicas (weighted, fewest in-flight reads).
//! * [`check_replica`]: the checks an external replica passes before use
//!   (`pg_is_in_recovery()`, `system_identifier`, version, schema access).
//! * Read-your-writes: every commit through a routed set records its WAL
//!   position ([`CommitToken`], `<set>/<lsn>`) in the request's
//!   [`RequestScope`]; the API sends it as `X-ForgeBase-Commit` and takes
//!   `X-ForgeBase-Read-After` / `X-ForgeBase-Consistency` back
//!   (`forgebase-api` `consistency`). A read needing an LSN goes to a replica
//!   that has replayed it, waits for one up to [`ReplicaOptions::lsn_wait`],
//!   or reads the primary.
//! * A replica read that fails because the replica is unavailable or cancels
//!   the statement for a recovery conflict is retried once on the primary.
//!
//! The FB-158 seams ([`ReadRouter`], [`ReadTarget`], [`PrimaryOnly`]) stay:
//! the PostgreSQL backend's own read pool holds its server only.

mod check;
mod health;
mod replicas;
pub mod scope;
#[cfg(all(feature = "testing", unix))]
pub mod testing;
mod token;

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::backend::Statement;

pub use check::{ReplicaCheckError, ReplicaFacts, check_replica, primary_facts};
pub use replicas::{FallbackReason, ReplicaNode, ReplicaSet, ReplicaStatus};
pub use scope::RequestScope;
pub use token::{CommitToken, ReadAfter};

/// A PostgreSQL write-ahead-log position (`pg_lsn`), printed `16/B374D848`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct Lsn(pub u64);

impl fmt::Display for Lsn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:X}/{:X}", self.0 >> 32, self.0 & 0xffff_ffff)
    }
}

impl FromStr for Lsn {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (hi, lo) = s.trim().split_once('/').ok_or_else(|| format!("{s:?} is not an LSN"))?;
        let hi = u64::from_str_radix(hi, 16).map_err(|_| format!("{s:?} is not an LSN"))?;
        let lo = u64::from_str_radix(lo, 16).map_err(|_| format!("{s:?} is not an LSN"))?;
        if hi > u64::from(u32::MAX) || lo > u64::from(u32::MAX) {
            return Err(format!("{s:?} is not an LSN"));
        }
        Ok(Lsn((hi << 32) | lo))
    }
}

/// How fresh a read must be (`X-ForgeBase-Consistency`, SDK option
/// `consistency`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Consistency {
    /// Read-your-writes: a replica may serve the read once it has replayed
    /// the caller's commits (the default).
    #[default]
    Session,
    /// Every read of the request runs on the primary.
    Strong,
}

impl Consistency {
    /// `session` or `strong`.
    pub fn as_str(self) -> &'static str {
        match self {
            Consistency::Session => "session",
            Consistency::Strong => "strong",
        }
    }

    /// Parses a header value (case-insensitive); `None` for anything else.
    pub fn parse(value: &str) -> Option<Self> {
        let v = value.trim();
        if v.eq_ignore_ascii_case("strong") {
            Some(Consistency::Strong)
        } else if v.eq_ignore_ascii_case("session") {
            Some(Consistency::Session)
        } else {
            None
        }
    }
}

/// What a read is and whether a replica may serve it (ADR-0029 §2.5). Only
/// the call sites of the §2.5 table pass [`ReadIntent::replica`]; everything
/// else reads the primary.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadIntent {
    /// What the read is, for logs and metrics (`record.list`, `vector.search`).
    pub what: &'static str,
    /// The call site allows a replica. `false` (the default): the primary.
    pub replica: bool,
    /// `Strong` sends the read to the primary even when `replica` is set.
    pub consistency: Consistency,
    /// Commits the read must see, beyond the request's
    /// ([`RequestScope::read_after`]): the higher LSN per set wins.
    pub read_after: ReadAfter,
}

impl ReadIntent {
    /// A read that must run on the primary.
    pub fn primary(what: &'static str) -> Self {
        Self {
            what,
            ..Self::default()
        }
    }

    /// A read a replica may serve once it has replayed the caller's commits.
    pub fn replica(what: &'static str) -> Self {
        Self {
            what,
            replica: true,
            ..Self::default()
        }
    }

    /// Same intent with `consistency`.
    #[must_use]
    pub fn with_consistency(mut self, consistency: Consistency) -> Self {
        self.consistency = consistency;
        self
    }

    /// Same intent, also waiting for `token`.
    #[must_use]
    pub fn after(mut self, token: &CommitToken) -> Self {
        self.read_after.note(&token.set, token.lsn);
        self
    }

    /// Same intent, also waiting for every token of `read_after`.
    #[must_use]
    pub fn after_all(mut self, read_after: &ReadAfter) -> Self {
        self.read_after.merge(read_after);
        self
    }
}

/// Where replica reads go (`[database.replicas] read_routing`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadRouting {
    /// Every read on the primary; replicas are not opened.
    Primary,
    /// Reads that allow it go to healthy replicas.
    #[default]
    Replicas,
}

/// `[database.replicas]` (ADR-0029 §4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplicaOptions {
    /// `primary` or `replicas`.
    pub read_routing: ReadRouting,
    /// Honour commit tokens and the per-principal commit map. `false`: a
    /// replica serves any read it is healthy for, however stale.
    pub read_your_writes: bool,
    /// How long a read waits for a replica to replay its LSN before it
    /// reads the primary (`lsn_wait_ms`, 50).
    pub lsn_wait: Duration,
    /// How often the wait asks the replica (5 ms) on engines without
    /// `WAIT FOR LSN`.
    pub lsn_poll: Duration,
    /// A replica further behind than this leaves rotation (`max_lag_ms`, 2 000).
    pub max_lag: Duration,
    /// Or more bytes of WAL behind than this (`max_lag_bytes`, 64 MiB).
    pub max_lag_bytes: u64,
    /// Health sample period (`health_interval_ms`, 500).
    pub health_interval: Duration,
    /// Consecutive samples under half the limits that bring a lagging or
    /// unreachable replica back (3).
    pub return_after: u32,
}

impl Default for ReplicaOptions {
    fn default() -> Self {
        Self {
            read_routing: ReadRouting::Replicas,
            read_your_writes: true,
            lsn_wait: Duration::from_millis(50),
            lsn_poll: Duration::from_millis(5),
            max_lag: Duration::from_millis(2_000),
            max_lag_bytes: 64 * 1024 * 1024,
            health_interval: Duration::from_millis(500),
            return_after: 3,
        }
    }
}

/// Outcome of waiting for a standby to replay a position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LsnWait {
    /// Replayed.
    Reached,
    /// Not within the timeout.
    TimedOut,
    /// The engine has no `WAIT FOR LSN` (PostgreSQL 18 and older, SQLite).
    Unsupported,
}

/// Where one read runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadTarget {
    /// The primary server's read pool.
    Primary,
    /// Replica number `n` (0-based) of the read pool.
    Replica(usize),
}

/// What a [`ReadRouter`] decides on.
#[derive(Debug, Clone, Copy)]
pub struct ReadRequest<'a> {
    /// The statement about to run.
    pub statement: &'a Statement,
    /// Number of replicas configured in the backend's own read pool (0:
    /// replicas live in [`ReplicaSet`]s).
    pub replicas: usize,
}

/// Chooses the read target of each statement inside one PostgreSQL
/// backend. A target that does not exist falls back to the primary.
pub trait ReadRouter: Send + Sync + fmt::Debug {
    /// The target for `request`.
    fn route(&self, request: &ReadRequest<'_>) -> ReadTarget;
}

/// Every read on the primary (the backend's own router).
#[derive(Debug, Clone, Copy, Default)]
pub struct PrimaryOnly;

impl ReadRouter for PrimaryOnly {
    fn route(&self, _request: &ReadRequest<'_>) -> ReadTarget {
        ReadTarget::Primary
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lsn_text_round_trip() {
        let lsn: Lsn = "16/B374D848".parse().unwrap();
        assert_eq!(lsn.0, (0x16 << 32) | 0xB374_D848);
        assert_eq!(lsn.to_string(), "16/B374D848");
        assert!("0/1".parse::<Lsn>().unwrap() < "1/0".parse::<Lsn>().unwrap());
        assert!("x".parse::<Lsn>().is_err());
        assert!("1/100000000".parse::<Lsn>().is_err());
    }

    #[test]
    fn consistency_header_values() {
        assert_eq!(Consistency::parse(" Strong "), Some(Consistency::Strong));
        assert_eq!(Consistency::parse("session"), Some(Consistency::Session));
        assert_eq!(Consistency::parse("eventual"), None);
        assert_eq!(Consistency::default().as_str(), "session");
    }

    #[test]
    fn intents_collect_tokens() {
        let a: CommitToken = "control/0/10".parse().unwrap();
        let b: CommitToken = "control/0/20".parse().unwrap();
        let intent = ReadIntent::replica("record.list").after(&b).after(&a);
        assert!(intent.replica);
        assert_eq!(intent.read_after.get("control"), Some(Lsn(0x20)));
        assert!(!ReadIntent::primary("auth").replica);
        let defaults = ReplicaOptions::default();
        assert_eq!(defaults.lsn_wait, Duration::from_millis(50));
        assert_eq!(defaults.max_lag_bytes, 67_108_864);
    }
}
