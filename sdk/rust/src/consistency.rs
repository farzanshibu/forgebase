//! Read-your-writes with read replicas.
//!
//! The server answers a write with `X-ForgeBase-Commit: <set>/<lsn>[, …]`. The client keeps the
//! highest position per set ([`CommitTokens`]) and sends them back as `X-ForgeBase-Read-After`, so a
//! replica serves a read only once it has replayed your writes (else the server reads the
//! primary). [`Consistency::Strong`] sends `X-ForgeBase-Consistency: strong` and the server reads
//! the primary. A server without replicas sends no tokens, and the client then sends nothing extra.

use std::collections::BTreeMap;
use std::sync::Mutex;

/// Response header with the commit positions of a write.
pub const COMMIT_HEADER: &str = "x-forgebase-commit";
/// Request header with the positions reads must see.
pub const READ_AFTER_HEADER: &str = "x-forgebase-read-after";
/// Request header asking for primary reads.
pub const CONSISTENCY_HEADER: &str = "x-forgebase-consistency";

/// Most sets tracked.
const MAX_SETS: usize = 64;

/// How fresh reads must be.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Consistency {
    /// Read your own writes (the default): replicas serve a read once they have replayed the
    /// client's commits.
    #[default]
    Session,
    /// Every read on the primary.
    Strong,
}

/// The highest commit position per set the client has seen. Shared by clones of a client.
#[derive(Debug, Default)]
pub struct CommitTokens {
    by_set: Mutex<BTreeMap<String, u64>>,
}

fn valid_set(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

fn parse_token(token: &str) -> Option<(&str, u64)> {
    let (set, lsn) = token.trim().split_once('/')?;
    let (hi, lo) = lsn.split_once('/')?;
    if !valid_set(set) || hi.is_empty() || lo.is_empty() || hi.len() > 8 || lo.len() > 8 {
        return None;
    }
    let hi = u64::from_str_radix(hi, 16).ok()?;
    let lo = u64::from_str_radix(lo, 16).ok()?;
    Some((set, (hi << 32) | lo))
}

fn format_lsn(lsn: u64) -> String {
    format!("{:X}/{:X}", lsn >> 32, lsn & 0xffff_ffff)
}

impl CommitTokens {
    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, u64>> {
        self.by_set.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Takes the tokens of a header value (`control/16/B374D848, s1/0/9`); malformed ones are
    /// ignored. Realtime events that carry a position feed it too.
    pub fn note(&self, header: &str) {
        let mut by_set = self.lock();
        for part in header.split(',') {
            let Some((set, lsn)) = parse_token(part) else {
                continue;
            };
            if let Some(held) = by_set.get_mut(set) {
                *held = (*held).max(lsn);
            } else if by_set.len() < MAX_SETS {
                by_set.insert(set.to_owned(), lsn);
            }
        }
    }

    /// The position held for one set (`16/B374D848`).
    pub fn get(&self, set: &str) -> Option<String> {
        self.lock().get(set).map(|l| format_lsn(*l))
    }

    /// The `X-ForgeBase-Read-After` value, or `None` before the first token.
    pub fn header(&self) -> Option<String> {
        let by_set = self.lock();
        if by_set.is_empty() {
            return None;
        }
        Some(
            by_set
                .iter()
                .map(|(set, lsn)| format!("{set}/{}", format_lsn(*lsn)))
                .collect::<Vec<_>>()
                .join(", "),
        )
    }

    /// Forgets every token.
    pub fn clear(&self) {
        self.lock().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_highest_position_per_set() {
        let t = CommitTokens::default();
        assert_eq!(t.header(), None);
        t.note("control/0/20, s1/1/0, junk, control/0/1F, /0/1, s2/x/1, s3/0/1/2, s4/123456789/0");
        t.note("control/0/10");
        assert_eq!(t.get("control").as_deref(), Some("0/20"));
        assert_eq!(t.header().as_deref(), Some("control/0/20, s1/1/0"));
        t.note("control/9/FFFFFFFF");
        t.note("control/a/0");
        assert_eq!(t.get("control").as_deref(), Some("A/0"));
        t.clear();
        assert_eq!(t.header(), None);
    }
}
