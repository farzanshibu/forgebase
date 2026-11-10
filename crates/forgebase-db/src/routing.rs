//! Seams for read replicas and tenant shards (ADR-0029, FB-158).
//!
//! Nothing here routes to a replica yet: the PostgreSQL read pool holds the
//! primary only and [`PrimaryOnly`] is the router. The replica packages add
//! read targets and a router without changing the read path:
//!
//! * the read pool is a list of targets (index 0 the primary) chosen per
//!   statement by a [`ReadRouter`];
//! * every commit records its WAL position ([`Lsn`]), so a read that must see
//!   its own writes can wait until a replica has replayed that far
//!   ([`crate::Database::last_commit_lsn`], [`crate::Database::current_wal_lsn`]);
//! * a [`crate::Database`] is one target and cheap to clone, with no global
//!   state, so a shard map holds one per shard.

use std::fmt;
use std::str::FromStr;

use serde::Serialize;

use crate::backend::Statement;

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
    /// Number of replicas configured (0 today).
    pub replicas: usize,
}

/// Chooses the read target of each statement. A target that does not exist
/// falls back to the primary.
pub trait ReadRouter: Send + Sync + fmt::Debug {
    /// The target for `request`.
    fn route(&self, request: &ReadRequest<'_>) -> ReadTarget;
}

/// Every read on the primary (the only router until replicas exist).
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
}
