//! Commit tokens: `<set id>/<lsn>` (ADR-0029 §2.6, §3.11).

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::Serialize;

use super::Lsn;

/// Most tokens one header value carries (one per set; more is a client bug).
const MAX_TOKENS: usize = 64;

/// One set's commit position, as `X-ForgeBase-Commit` and
/// `X-ForgeBase-Read-After` carry it: `control/16/B374D848`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct CommitToken {
    /// Set id.
    pub set: String,
    /// WAL position at or after the commit.
    pub lsn: Lsn,
}

impl fmt::Display for CommitToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.set, self.lsn)
    }
}

/// Set ids are lower-case words (`control`, `s1a2b3`); anything else is not
/// a token.
fn valid_set_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.')
}

impl FromStr for CommitToken {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        let (set, lsn) = s
            .split_once('/')
            .ok_or_else(|| format!("{s:?} is not a commit token"))?;
        if !valid_set_id(set) {
            return Err(format!("{s:?} is not a commit token"));
        }
        Ok(CommitToken {
            set: set.to_owned(),
            lsn: lsn.parse()?,
        })
    }
}

/// The highest commit position per set a read must see. Merging keeps the
/// higher LSN of each set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ReadAfter(BTreeMap<String, Lsn>);

impl ReadAfter {
    /// No positions.
    pub fn new() -> Self {
        Self::default()
    }

    /// Keeps `lsn` for `set` when it is higher than the one held.
    pub fn note(&mut self, set: &str, lsn: Lsn) {
        match self.0.get_mut(set) {
            Some(held) => *held = (*held).max(lsn),
            None => {
                self.0.insert(set.to_owned(), lsn);
            }
        }
    }

    /// Merges `other` (higher LSN per set wins).
    pub fn merge(&mut self, other: &ReadAfter) {
        for (set, lsn) in &other.0 {
            self.note(set, *lsn);
        }
    }

    /// The position for `set`.
    pub fn get(&self, set: &str) -> Option<Lsn> {
        self.0.get(set).copied()
    }

    /// Whether no set has a position.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Number of sets.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// The tokens, by set id.
    pub fn tokens(&self) -> impl Iterator<Item = CommitToken> + '_ {
        self.0.iter().map(|(set, lsn)| CommitToken {
            set: set.clone(),
            lsn: *lsn,
        })
    }

    /// Adds every well-formed token of a header value (`a/0/1, b/2/3`).
    /// Malformed tokens are skipped: the header only ever makes reads
    /// fresher, so a bad one costs freshness, never an error.
    pub fn parse_into(&mut self, header: &str) {
        for part in header.split(',').take(MAX_TOKENS) {
            if let Ok(t) = part.parse::<CommitToken>() {
                self.note(&t.set, t.lsn);
            }
        }
    }

    /// Tokens of a header value.
    pub fn parse_header(header: &str) -> Self {
        let mut r = Self::new();
        r.parse_into(header);
        r
    }

    /// The header value (`a/0/1, b/2/3`), or `None` when empty.
    pub fn to_header(&self) -> Option<String> {
        if self.0.is_empty() {
            return None;
        }
        Some(self.tokens().map(|t| t.to_string()).collect::<Vec<_>>().join(", "))
    }
}

impl FromIterator<CommitToken> for ReadAfter {
    fn from_iter<I: IntoIterator<Item = CommitToken>>(iter: I) -> Self {
        let mut r = Self::new();
        for t in iter {
            r.note(&t.set, t.lsn);
        }
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_round_trip() {
        let t: CommitToken = "control/16/B374D848".parse().unwrap();
        assert_eq!(t.set, "control");
        assert_eq!(t.lsn, "16/B374D848".parse().unwrap());
        assert_eq!(t.to_string(), "control/16/B374D848");
        for bad in [
            "",
            "control",
            "control/",
            "/0/1",
            "con trol/0/1",
            "control/0/x",
            "a/b/c/d",
        ] {
            assert!(bad.parse::<CommitToken>().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn read_after_keeps_the_highest_per_set() {
        let mut r = ReadAfter::parse_header("control/0/20, s1/1/0, control/0/10, junk, s1/0/FF");
        assert_eq!(r.len(), 2);
        assert_eq!(r.get("control"), Some(Lsn(0x20)));
        assert_eq!(r.get("s1"), Some(Lsn(1 << 32)));
        r.merge(&ReadAfter::parse_header("control/0/30"));
        assert_eq!(r.to_header().as_deref(), Some("control/0/30, s1/1/0"));
        assert_eq!(ReadAfter::new().to_header(), None);
        let many = (0..200).map(|i| format!("s{i}/0/1")).collect::<Vec<_>>().join(",");
        assert_eq!(ReadAfter::parse_header(&many).len(), MAX_TOKENS);
    }
}
