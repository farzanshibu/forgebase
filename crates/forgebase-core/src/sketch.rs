//! A compact latency sketch (ADR-0036 §2).
//!
//! A log-linear histogram of non-negative integers (microseconds in the
//! callers): four buckets per power of two, 160 buckets covering `0 ..= 2^40`
//! (about 12.7 days). A bucket spans at most 25 % of its lower bound, so a
//! quantile read at the bucket midpoint is within about ±11 % of the true
//! value, whatever the distribution. Sketches merge by adding counts, which is
//! what lets ForgeBase store one per time bucket and answer any time range
//! afterwards.
//!
//! [`AtomicSketch`] is the hot-path form: recording is one relaxed atomic
//! add, no lock and no allocation. [`Sketch`] is the plain, mergeable and
//! serialisable form (sparse JSON, `[[bucket, count], ...]`).

use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

/// Number of buckets.
pub const BUCKETS: usize = 160;

/// The bucket of `v`.
#[inline]
pub fn bucket_of(v: u64) -> usize {
    if v < 4 {
        return v as usize;
    }
    let msb = 63 - v.leading_zeros() as usize;
    let frac = ((v >> (msb - 2)) & 3) as usize;
    ((msb - 1) * 4 + frac).min(BUCKETS - 1)
}

/// Lower bound (inclusive) of bucket `i`.
pub fn bucket_low(i: usize) -> u64 {
    if i < 4 {
        return i as u64;
    }
    let msb = i / 4 + 1;
    let frac = (i % 4) as u64;
    (4 + frac) << (msb - 2)
}

/// Upper bound (exclusive) of bucket `i`.
pub fn bucket_high(i: usize) -> u64 {
    if i + 1 >= BUCKETS {
        return u64::MAX;
    }
    bucket_low(i + 1)
}

/// The plain sketch: counts per bucket.
#[derive(Clone, PartialEq, Eq)]
pub struct Sketch {
    counts: Box<[u64; BUCKETS]>,
}

impl Default for Sketch {
    fn default() -> Self {
        Self {
            counts: Box::new([0; BUCKETS]),
        }
    }
}

impl std::fmt::Debug for Sketch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sketch").field("count", &self.count()).finish()
    }
}

impl Sketch {
    /// An empty sketch.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one value.
    pub fn record(&mut self, v: u64) {
        self.counts[bucket_of(v)] += 1;
    }

    /// Adds `n` to bucket `i` (out-of-range buckets are ignored).
    pub fn add_bucket(&mut self, i: usize, n: u64) {
        if let Some(c) = self.counts.get_mut(i) {
            *c = c.saturating_add(n);
        }
    }

    /// Number of recorded values.
    pub fn count(&self) -> u64 {
        self.counts.iter().sum()
    }

    /// Whether nothing was recorded.
    pub fn is_empty(&self) -> bool {
        self.counts.iter().all(|c| *c == 0)
    }

    /// Adds every count of `other`.
    pub fn merge(&mut self, other: &Sketch) {
        for (a, b) in self.counts.iter_mut().zip(other.counts.iter()) {
            *a = a.saturating_add(*b);
        }
    }

    /// The value at quantile `q` (0..=1): the midpoint of the bucket holding
    /// the `ceil(q * count)`-th smallest value; `None` when empty. The
    /// result is clamped into `[min, max]` when those are known.
    pub fn quantile(&self, q: f64) -> Option<u64> {
        let total = self.count();
        if total == 0 {
            return None;
        }
        let rank = ((q.clamp(0.0, 1.0) * total as f64).ceil() as u64).clamp(1, total);
        let mut seen = 0u64;
        for (i, c) in self.counts.iter().enumerate() {
            seen += c;
            if seen >= rank {
                let lo = bucket_low(i);
                let hi = bucket_high(i);
                return Some(if hi == u64::MAX { lo } else { lo + (hi - lo) / 2 });
            }
        }
        None
    }

    /// Like [`Sketch::quantile`], clamped into `[lo, hi]` (the observed
    /// minimum and maximum), so p100 is the true maximum.
    pub fn quantile_within(&self, q: f64, lo: u64, hi: u64) -> Option<u64> {
        self.quantile(q).map(|v| v.clamp(lo.min(hi), hi))
    }

    /// Sparse JSON text: `[[bucket, count], ...]`.
    pub fn to_json(&self) -> String {
        let pairs: Vec<(usize, u64)> = self
            .counts
            .iter()
            .enumerate()
            .filter(|(_, c)| **c > 0)
            .map(|(i, c)| (i, *c))
            .collect();
        serde_json::to_string(&pairs).unwrap_or_else(|_| "[]".into())
    }

    /// Reads [`Sketch::to_json`] text; unreadable input is an empty sketch.
    pub fn from_json(text: &str) -> Self {
        let mut s = Self::new();
        if let Ok(pairs) = serde_json::from_str::<Vec<(usize, u64)>>(text) {
            for (i, n) in pairs {
                s.add_bucket(i, n);
            }
        }
        s
    }
}

impl Serialize for Sketch {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let pairs: Vec<(usize, u64)> = self
            .counts
            .iter()
            .enumerate()
            .filter(|(_, c)| **c > 0)
            .map(|(i, c)| (i, *c))
            .collect();
        pairs.serialize(s)
    }
}

impl<'de> Deserialize<'de> for Sketch {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let pairs = Vec::<(usize, u64)>::deserialize(d)?;
        let mut s = Sketch::new();
        for (i, n) in pairs {
            s.add_bucket(i, n);
        }
        Ok(s)
    }
}

/// The lock-free recording form of [`Sketch`].
#[derive(Debug)]
pub struct AtomicSketch {
    counts: [AtomicU64; BUCKETS],
}

impl Default for AtomicSketch {
    fn default() -> Self {
        Self {
            counts: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }
}

impl AtomicSketch {
    /// An empty sketch.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one value: one relaxed atomic add.
    #[inline]
    pub fn record(&self, v: u64) {
        self.counts[bucket_of(v)].fetch_add(1, Ordering::Relaxed);
    }

    /// Moves every count into a plain sketch and zeroes the buckets. Values
    /// recorded concurrently land either in the returned sketch or in the
    /// next drain, never in both and never nowhere.
    pub fn drain(&self) -> Sketch {
        let mut s = Sketch::new();
        for (i, c) in self.counts.iter().enumerate() {
            let n = c.swap(0, Ordering::Relaxed);
            if n > 0 {
                s.add_bucket(i, n);
            }
        }
        s
    }

    /// A copy of the counts without clearing them.
    pub fn peek(&self) -> Sketch {
        let mut s = Sketch::new();
        for (i, c) in self.counts.iter().enumerate() {
            let n = c.load(Ordering::Relaxed);
            if n > 0 {
                s.add_bucket(i, n);
            }
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_are_monotonic_and_contiguous() {
        let mut prev = 0;
        for i in 0..BUCKETS {
            assert_eq!(bucket_of(bucket_low(i)), i, "low bound of {i}");
            if i > 0 {
                assert_eq!(bucket_low(i), prev, "bucket {i} starts where {} ends", i - 1);
            }
            prev = bucket_high(i);
        }
        assert_eq!(bucket_of(u64::MAX), BUCKETS - 1);
        for v in [0u64, 1, 3, 4, 5, 7, 8, 100, 12345, 1 << 30] {
            let b = bucket_of(v);
            assert!(bucket_low(b) <= v && v < bucket_high(b), "{v} in bucket {b}");
        }
    }

    #[test]
    fn quantiles_are_within_the_bucket_error() {
        let mut s = Sketch::new();
        for v in 1..=10_000u64 {
            s.record(v * 10);
        }
        for (q, want) in [(0.5, 50_000.0), (0.95, 95_000.0), (0.99, 99_000.0)] {
            let got = s.quantile(q).unwrap() as f64;
            assert!((got - want).abs() / want < 0.13, "q{q}: {got} vs {want}");
        }
        assert_eq!(Sketch::new().quantile(0.5), None);
    }

    #[test]
    fn merge_and_json_round_trip() {
        let (mut a, mut b) = (Sketch::new(), Sketch::new());
        for v in [10, 20, 30] {
            a.record(v);
        }
        for v in [1000, 2000] {
            b.record(v);
        }
        a.merge(&b);
        assert_eq!(a.count(), 5);
        let back = Sketch::from_json(&a.to_json());
        assert_eq!(back, a);
        assert!(Sketch::from_json("not json").is_empty());
    }

    #[test]
    fn atomic_drain_loses_nothing() {
        let s = AtomicSketch::new();
        s.record(5);
        s.record(500);
        assert_eq!(s.peek().count(), 2);
        assert_eq!(s.drain().count(), 2);
        assert_eq!(s.drain().count(), 0);
    }
}
