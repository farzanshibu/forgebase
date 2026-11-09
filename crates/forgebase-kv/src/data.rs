//! The value types a key can hold, with incremental memory accounting.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

/// Estimated bookkeeping bytes per key (map node, LRU and expiry index entries).
pub(crate) const ENTRY_OVERHEAD: usize = 128;
/// Estimated bookkeeping bytes per collection member.
const ITEM_OVERHEAD: usize = 32;

fn item(len: usize) -> usize {
    len + ITEM_OVERHEAD
}

/// What kind of value a key holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Str,
    Hash,
    List,
    Set,
    ZSet,
    Stream,
}

impl Kind {
    /// The name `TYPE` reports and the SQLite `type` column stores.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Kind::Str => "string",
            Kind::Hash => "hash",
            Kind::List => "list",
            Kind::Set => "set",
            Kind::ZSet => "zset",
            Kind::Stream => "stream",
        }
    }

    pub(crate) fn from_name(s: &str) -> Option<Self> {
        Some(match s {
            "string" => Kind::Str,
            "hash" => Kind::Hash,
            "list" => Kind::List,
            "set" => Kind::Set,
            "zset" => Kind::ZSet,
            "stream" => Kind::Stream,
            _ => return None,
        })
    }
}

/// A value.
#[derive(Debug, Clone)]
pub(crate) enum Data {
    Str(Vec<u8>),
    Hash(HashV),
    List(ListV),
    Set(SetV),
    ZSet(ZSetV),
    Stream(StreamV),
}

impl Data {
    pub(crate) fn empty(kind: Kind) -> Self {
        match kind {
            Kind::Str => Data::Str(Vec::new()),
            Kind::Hash => Data::Hash(HashV::default()),
            Kind::List => Data::List(ListV::default()),
            Kind::Set => Data::Set(SetV::default()),
            Kind::ZSet => Data::ZSet(ZSetV::default()),
            Kind::Stream => Data::Stream(StreamV::default()),
        }
    }

    pub(crate) fn kind(&self) -> Kind {
        match self {
            Data::Str(_) => Kind::Str,
            Data::Hash(_) => Kind::Hash,
            Data::List(_) => Kind::List,
            Data::Set(_) => Kind::Set,
            Data::ZSet(_) => Kind::ZSet,
            Data::Stream(_) => Kind::Stream,
        }
    }

    /// Estimated bytes of the value.
    pub(crate) fn mem(&self) -> usize {
        match self {
            Data::Str(s) => s.len(),
            Data::Hash(h) => h.bytes,
            Data::List(l) => l.bytes,
            Data::Set(s) => s.bytes,
            Data::ZSet(z) => z.bytes,
            Data::Stream(s) => s.bytes,
        }
    }

    /// Collections with no members are removed (as Redis does); strings never are.
    pub(crate) fn is_empty(&self) -> bool {
        match self {
            Data::Str(_) => false,
            Data::Hash(h) => h.map.is_empty(),
            Data::List(l) => l.items.is_empty(),
            Data::Set(s) => s.set.is_empty(),
            Data::ZSet(z) => z.scores.is_empty(),
            // A stream keeps its last id even when empty, like Redis.
            Data::Stream(_) => false,
        }
    }
}

/// Hash: field to value.
#[derive(Debug, Clone, Default)]
pub(crate) struct HashV {
    pub(crate) map: BTreeMap<Vec<u8>, Vec<u8>>,
    bytes: usize,
}

impl HashV {
    /// Sets a field; true when it is new.
    pub(crate) fn set(&mut self, field: Vec<u8>, value: Vec<u8>) -> bool {
        let add = item(field.len() + value.len());
        match self.map.get_mut(&field) {
            Some(old) => {
                self.bytes = self.bytes - old.len() + value.len();
                *old = value;
                false
            }
            None => {
                self.bytes += add;
                self.map.insert(field, value);
                true
            }
        }
    }

    pub(crate) fn remove(&mut self, field: &[u8]) -> bool {
        match self.map.remove(field) {
            Some(v) => {
                self.bytes -= item(field.len() + v.len());
                true
            }
            None => false,
        }
    }
}

/// List.
#[derive(Debug, Clone, Default)]
pub(crate) struct ListV {
    pub(crate) items: VecDeque<Vec<u8>>,
    bytes: usize,
}

impl ListV {
    pub(crate) fn push_front(&mut self, v: Vec<u8>) {
        self.bytes += item(v.len());
        self.items.push_front(v);
    }

    pub(crate) fn push_back(&mut self, v: Vec<u8>) {
        self.bytes += item(v.len());
        self.items.push_back(v);
    }

    pub(crate) fn pop_front(&mut self) -> Option<Vec<u8>> {
        let v = self.items.pop_front()?;
        self.bytes -= item(v.len());
        Some(v)
    }

    pub(crate) fn pop_back(&mut self) -> Option<Vec<u8>> {
        let v = self.items.pop_back()?;
        self.bytes -= item(v.len());
        Some(v)
    }

    pub(crate) fn set(&mut self, idx: usize, v: Vec<u8>) {
        if let Some(slot) = self.items.get_mut(idx) {
            self.bytes = self.bytes - slot.len() + v.len();
            *slot = v;
        }
    }

    /// Replaces the items (LREM, LTRIM); recounts.
    pub(crate) fn replace(&mut self, items: VecDeque<Vec<u8>>) {
        self.bytes = items.iter().map(|i| item(i.len())).sum();
        self.items = items;
    }
}

/// Set of byte strings.
#[derive(Debug, Clone, Default)]
pub(crate) struct SetV {
    pub(crate) set: BTreeSet<Vec<u8>>,
    bytes: usize,
}

impl SetV {
    pub(crate) fn insert(&mut self, m: Vec<u8>) -> bool {
        let n = item(m.len());
        if self.set.insert(m) {
            self.bytes += n;
            true
        } else {
            false
        }
    }

    pub(crate) fn remove(&mut self, m: &[u8]) -> bool {
        if self.set.remove(m) {
            self.bytes -= item(m.len());
            true
        } else {
            false
        }
    }
}

/// A score ordered with `total_cmp` (no NaN is ever stored; `-0.0` is stored as `0.0`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Score(pub(crate) f64);

impl Eq for Score {}

impl PartialOrd for Score {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Score {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.total_cmp(&other.0)
    }
}

/// Sorted set: member to score, ordered by (score, member).
#[derive(Debug, Clone, Default)]
pub(crate) struct ZSetV {
    pub(crate) scores: HashMap<Vec<u8>, f64>,
    pub(crate) order: BTreeSet<(Score, Vec<u8>)>,
    bytes: usize,
}

impl ZSetV {
    /// Sets a member's score; true when the member is new.
    pub(crate) fn set(&mut self, member: Vec<u8>, score: f64) -> bool {
        let score = if score == 0.0 { 0.0 } else { score };
        match self.scores.get_mut(&member) {
            Some(old) => {
                self.order.remove(&(Score(*old), member.clone()));
                *old = score;
                self.order.insert((Score(score), member));
                false
            }
            None => {
                self.bytes += item(member.len() + 8);
                self.scores.insert(member.clone(), score);
                self.order.insert((Score(score), member));
                true
            }
        }
    }

    pub(crate) fn remove(&mut self, member: &[u8]) -> bool {
        match self.scores.remove(member) {
            Some(s) => {
                self.order.remove(&(Score(s), member.to_vec()));
                self.bytes -= item(member.len() + 8);
                true
            }
            None => false,
        }
    }
}

/// A stream entry id: milliseconds and a sequence number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub(crate) struct StreamId {
    pub(crate) ms: u64,
    pub(crate) seq: u64,
}

impl StreamId {
    pub(crate) const MIN: StreamId = StreamId { ms: 0, seq: 0 };
    pub(crate) const MAX: StreamId = StreamId {
        ms: u64::MAX,
        seq: u64::MAX,
    };

    pub(crate) fn render(self) -> String {
        format!("{}-{}", self.ms, self.seq)
    }

    /// The id after this one.
    pub(crate) fn next(self) -> Option<StreamId> {
        if self.seq < u64::MAX {
            Some(StreamId {
                ms: self.ms,
                seq: self.seq + 1,
            })
        } else if self.ms < u64::MAX {
            Some(StreamId {
                ms: self.ms + 1,
                seq: 0,
            })
        } else {
            None
        }
    }
}

/// Field/value pairs of one stream entry.
pub(crate) type Fields = Vec<(Vec<u8>, Vec<u8>)>;

/// Append-only stream.
#[derive(Debug, Clone, Default)]
pub(crate) struct StreamV {
    pub(crate) entries: BTreeMap<StreamId, Fields>,
    pub(crate) last: StreamId,
    bytes: usize,
}

fn fields_bytes(f: &Fields) -> usize {
    item(16 + f.iter().map(|(k, v)| k.len() + v.len() + 8).sum::<usize>())
}

impl StreamV {
    pub(crate) fn insert(&mut self, id: StreamId, fields: Fields) {
        self.bytes += fields_bytes(&fields);
        self.entries.insert(id, fields);
        self.last = id;
    }

    pub(crate) fn remove(&mut self, id: &StreamId) -> bool {
        match self.entries.remove(id) {
            Some(f) => {
                self.bytes -= fields_bytes(&f);
                true
            }
            None => false,
        }
    }

    /// Keeps the newest `max` entries; returns how many were removed.
    pub(crate) fn trim(&mut self, max: usize) -> usize {
        let mut removed = 0;
        while self.entries.len() > max {
            let Some((id, f)) = self.entries.pop_first() else { break };
            let _ = id;
            self.bytes -= fields_bytes(&f);
            removed += 1;
        }
        removed
    }
}
