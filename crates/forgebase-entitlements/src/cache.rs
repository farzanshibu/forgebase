//! Usage state in memory for the synchronous pipeline hooks.
//!
//! The database is the source of truth; every metering transaction reads the
//! state of the key it changed before committing and publishes it here after
//! the commit. Entries carry the order of their transaction (`seq`), so a
//! late publisher never overwrites a newer state. This process is the only
//! writer (ADR-0008 §7 has the same limitation), so the cache is exact for
//! calendar, lifetime and gauge counters and leases; a stale or missing
//! entry makes the hook admit and the metering call decide exactly.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// A counter: feature, subject, scope key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct Key {
    pub feature: String,
    pub subject: String,
    pub scope_key: String,
}

impl Key {
    pub fn new(feature: &str, subject: &str, scope_key: &str) -> Self {
        Self {
            feature: feature.to_owned(),
            subject: subject.to_owned(),
            scope_key: scope_key.to_owned(),
        }
    }
}

/// Cached state of one key.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Entry {
    /// Window the numbers belong to.
    pub window_start: i64,
    /// Used plus reserved (gauges: the level).
    pub consumed: i64,
    /// Rolled over into the window.
    pub carried: i64,
    /// Expiry times of active leases (concurrency).
    pub leases: Vec<i64>,
    /// The numbers hold until then (rolling windows: the oldest event ages out).
    pub valid_until: i64,
    pub(crate) seq: u64,
}

const MAX_ENTRIES: usize = 200_000;

#[derive(Debug, Default)]
pub(crate) struct UsageCache {
    seq: AtomicU64,
    keys: Mutex<HashMap<Key, Entry>>,
    /// Usage-pack units left by (feature, subject).
    packs: Mutex<HashMap<(String, String), (i64, u64)>>,
}

impl UsageCache {
    /// The order of the current transaction; take it while holding the
    /// writer so it follows commit order.
    pub fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::SeqCst) + 1
    }

    pub fn put(&self, key: Key, mut entry: Entry, seq: u64) {
        entry.seq = seq;
        let mut map = self.keys.lock().unwrap_or_else(|p| p.into_inner());
        if map.len() >= MAX_ENTRIES && !map.contains_key(&key) {
            map.clear();
        }
        match map.get(&key) {
            Some(old) if old.seq > seq => {}
            _ => {
                map.insert(key, entry);
            }
        }
    }

    pub fn get(&self, key: &Key) -> Option<Entry> {
        self.keys.lock().unwrap_or_else(|p| p.into_inner()).get(key).cloned()
    }

    pub fn put_packs(&self, feature: &str, subject: &str, remaining: i64, seq: u64) {
        let mut map = self.packs.lock().unwrap_or_else(|p| p.into_inner());
        if map.len() >= MAX_ENTRIES {
            map.clear();
        }
        let k = (feature.to_owned(), subject.to_owned());
        match map.get(&k) {
            Some((_, s)) if *s > seq => {}
            _ => {
                map.insert(k, (remaining, seq));
            }
        }
    }

    pub fn packs(&self, feature: &str, subject: &str) -> i64 {
        self.packs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&(feature.to_owned(), subject.to_owned()))
            .map_or(0, |(r, _)| *r)
    }
}
