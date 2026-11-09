//! Vector validation, storage encoding and exact distances.
//!
//! Distances use the ANN engine's definitions so exact and approximate
//! results are comparable: cosine `1 - a·b / (|a||b|)`, inner product
//! `1 - a·b`, and squared Euclidean distance for `l2` (reported to callers as
//! the Euclidean distance, see [`public_distance`]).

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use crate::error::VectorError;
use crate::model::Metric;

/// Converts request numbers to float32, checking dimension and finiteness.
/// Cosine vectors must not be all zero (their direction is undefined).
pub(crate) fn validate(field: &str, dimension: u32, metric: Metric, v: &[f64]) -> Result<Vec<f32>, VectorError> {
    let expected = dimension as usize;
    if v.len() != expected {
        return Err(VectorError::DimensionMismatch {
            field: field.to_owned(),
            expected,
            got: v.len(),
        });
    }
    let mut out = Vec::with_capacity(v.len());
    for (i, x) in v.iter().enumerate() {
        #[allow(clippy::cast_possible_truncation)]
        let f = *x as f32;
        if !x.is_finite() || !f.is_finite() {
            return Err(VectorError::invalid(
                "invalid_vector",
                format!("{field}[{i}] is not a finite 32-bit float"),
            ));
        }
        out.push(f);
    }
    if metric == Metric::Cosine && out.iter().all(|x| *x == 0.0) {
        return Err(VectorError::invalid(
            "invalid_vector",
            format!("{field}: a cosine vector must not be all zeros"),
        ));
    }
    Ok(out)
}

/// Little-endian float32 bytes.
pub(crate) fn encode(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for x in v {
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

/// Inverse of [`encode`]; fails when the length does not fit the dimension.
pub(crate) fn decode(bytes: &[u8], dimension: u32) -> Result<Vec<f32>, VectorError> {
    if bytes.len() != dimension as usize * 4 {
        return Err(VectorError::Internal(format!(
            "stored vector has {} bytes, expected {}",
            bytes.len(),
            dimension as usize * 4
        )));
    }
    let (chunks, _) = bytes.as_chunks::<4>();
    Ok(chunks.iter().map(|c| f32::from_le_bytes(*c)).collect())
}

/// Raw distance in the engine's definition (lower is closer).
pub(crate) fn distance(metric: Metric, a: &[f32], b: &[f32]) -> f32 {
    match metric {
        Metric::L2 => a.iter().zip(b).map(|(x, y)| (x - y) * (x - y)).sum(),
        Metric::Dot => 1.0 - dot(a, b),
        Metric::Cosine => {
            let (mut ab, mut aa, mut bb) = (0.0f32, 0.0f32, 0.0f32);
            for (x, y) in a.iter().zip(b) {
                ab += x * y;
                aa += x * x;
                bb += y * y;
            }
            if aa == 0.0 || bb == 0.0 {
                return 1.0;
            }
            1.0 - ab / (aa.sqrt() * bb.sqrt())
        }
    }
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Distance shown to callers: Euclidean (not squared) for `l2`.
pub(crate) fn public_distance(metric: Metric, raw: f32) -> f32 {
    match metric {
        Metric::L2 => raw.max(0.0).sqrt(),
        Metric::Cosine | Metric::Dot => raw,
    }
}

/// Similarity from a public distance (higher is closer).
pub(crate) fn score(metric: Metric, public: f32) -> f32 {
    match metric {
        Metric::Cosine | Metric::Dot => 1.0 - public,
        Metric::L2 => 1.0 / (1.0 + public),
    }
}

/// A candidate ordered by distance, then key (deterministic ties).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Scored {
    pub dist: f32,
    pub key: u64,
}

impl PartialEq for Scored {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Scored {}
impl PartialOrd for Scored {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Scored {
    fn cmp(&self, other: &Self) -> Ordering {
        self.dist.total_cmp(&other.dist).then(self.key.cmp(&other.key))
    }
}

/// Keeps the `k` closest candidates seen so far.
#[derive(Debug)]
pub(crate) struct TopK {
    k: usize,
    heap: BinaryHeap<Scored>,
}

impl TopK {
    pub(crate) fn new(k: usize) -> Self {
        Self {
            k,
            heap: BinaryHeap::with_capacity(k + 1),
        }
    }

    pub(crate) fn push(&mut self, key: u64, dist: f32) {
        let s = Scored { dist, key };
        if self.heap.len() < self.k {
            self.heap.push(s);
        } else if let Some(worst) = self.heap.peek()
            && s < *worst
        {
            self.heap.pop();
            self.heap.push(s);
        }
    }

    /// Closest first.
    pub(crate) fn into_sorted(self) -> Vec<Scored> {
        self.heap.into_sorted_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_rejects_wrong_shapes_and_values() {
        assert!(validate("e", 3, Metric::L2, &[1.0, 2.0, 3.0]).is_ok());
        assert!(matches!(
            validate("e", 3, Metric::L2, &[1.0, 2.0]),
            Err(VectorError::DimensionMismatch {
                expected: 3,
                got: 2,
                ..
            })
        ));
        assert!(validate("e", 2, Metric::L2, &[f64::NAN, 0.0]).is_err());
        assert!(validate("e", 2, Metric::L2, &[1e39, 0.0]).is_err(), "overflows f32");
        assert!(validate("e", 2, Metric::Cosine, &[0.0, 0.0]).is_err());
        assert!(validate("e", 2, Metric::Dot, &[0.0, 0.0]).is_ok());
    }

    #[test]
    fn encoding_round_trips() {
        let v = vec![0.5f32, -1.25, 3.0e-8, f32::MAX];
        assert_eq!(decode(&encode(&v), 4).unwrap(), v);
        assert!(decode(&encode(&v), 3).is_err());
    }

    #[test]
    fn distances_follow_the_engine_definitions() {
        let a = [1.0f32, 0.0];
        let b = [0.0f32, 2.0];
        assert!((distance(Metric::Cosine, &a, &b) - 1.0).abs() < 1e-6);
        assert!(distance(Metric::Cosine, &a, &[3.0, 0.0]).abs() < 1e-6);
        assert!((distance(Metric::L2, &a, &b) - 5.0).abs() < 1e-6);
        assert!((public_distance(Metric::L2, 5.0) - 5f32.sqrt()).abs() < 1e-6);
        assert!((distance(Metric::Dot, &[2.0, 1.0], &[3.0, 4.0]) - (1.0 - 10.0)).abs() < 1e-6);
        assert!((score(Metric::Dot, -9.0) - 10.0).abs() < 1e-6);
    }

    #[test]
    fn top_k_keeps_the_closest_with_stable_ties() {
        let mut t = TopK::new(3);
        for (k, d) in [(1, 0.5), (2, 0.1), (3, 0.9), (4, 0.1), (5, 0.3)] {
            t.push(k, d);
        }
        let keys: Vec<u64> = t.into_sorted().iter().map(|s| s.key).collect();
        assert_eq!(keys, vec![2, 4, 5]);
    }
}
