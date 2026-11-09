//! The ANN engine abstraction and its USearch (HNSW) implementation.
//!
//! The service only talks to [`AnnIndex`] and [`AnnEngine`], so another
//! engine can be added later without touching storage, search strategies or
//! the API (SPEC §6). Index files name their engine and engine version in
//! the manifest; a file written by another engine or version is rebuilt.
//!
//! All methods block (CPU work, file I/O); the service calls them from
//! `spawn_blocking`.

use std::fmt;
use std::path::Path;
use std::sync::RwLock;

use usearch::ffi::{IndexOptions, MetricKind, ScalarKind};

use crate::error::VectorError;
use crate::model::{Metric, Quantization};

/// Parameters an index is built with (from the field definition).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexParams {
    /// Number of components.
    pub dimension: u32,
    /// Distance function.
    pub metric: Metric,
    /// Element type.
    pub quantization: Quantization,
    /// HNSW links per node.
    pub connectivity: u32,
    /// HNSW insertion candidate list.
    pub expansion_add: u32,
    /// HNSW search candidate list.
    pub expansion_search: u32,
}

/// Size and memory figures of an index.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EngineStats {
    /// Vectors present.
    pub len: u64,
    /// Reserved slots.
    pub capacity: u64,
    /// Bytes in memory.
    pub memory_bytes: u64,
    /// Bytes when saved.
    pub serialized_bytes: u64,
}

/// One approximate nearest-neighbour index over `u64` keys.
pub trait AnnIndex: Send + Sync + fmt::Debug {
    /// Vectors present.
    fn len(&self) -> usize;
    /// Whether the index holds no vectors.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Inserts or replaces the vector of `key`.
    fn upsert(&self, key: u64, vector: &[f32]) -> Result<(), VectorError>;
    /// Removes `key` (no-op when absent).
    fn remove(&self, key: u64) -> Result<(), VectorError>;
    /// Up to `k` nearest keys with raw distances, closest first. With a
    /// filter, only keys it accepts are returned.
    fn search(
        &self,
        query: &[f32],
        k: usize,
        filter: Option<&(dyn Fn(u64) -> bool + Sync)>,
    ) -> Result<Vec<(u64, f32)>, VectorError>;
    /// Writes the index to `path`.
    fn save(&self, path: &Path) -> Result<(), VectorError>;
    /// Prepares room for `n` vectors in total.
    fn reserve(&self, n: usize) -> Result<(), VectorError>;
    /// Size and memory figures.
    fn stats(&self) -> EngineStats;
}

/// Creates and loads indexes of one engine.
pub trait AnnEngine: Send + Sync + fmt::Debug {
    /// Engine name recorded in manifests.
    fn name(&self) -> &'static str;
    /// Engine version recorded in manifests.
    fn version(&self) -> String;
    /// An empty index.
    fn create(&self, params: &IndexParams) -> Result<Box<dyn AnnIndex>, VectorError>;
    /// Loads a file written by [`AnnIndex::save`] of the same engine.
    fn load(&self, params: &IndexParams, path: &Path) -> Result<Box<dyn AnnIndex>, VectorError>;
}

/// USearch: HNSW over dense vectors (C++, Apache-2.0).
#[derive(Debug, Clone, Copy, Default)]
pub struct Usearch;

fn options(p: &IndexParams) -> IndexOptions {
    IndexOptions {
        dimensions: p.dimension as usize,
        metric: match p.metric {
            Metric::Cosine => MetricKind::Cos,
            Metric::L2 => MetricKind::L2sq,
            Metric::Dot => MetricKind::IP,
        },
        quantization: match p.quantization {
            Quantization::F32 => ScalarKind::F32,
            Quantization::F16 => ScalarKind::F16,
            Quantization::I8 => ScalarKind::I8,
        },
        connectivity: p.connectivity as usize,
        expansion_add: p.expansion_add as usize,
        expansion_search: p.expansion_search as usize,
        multi: false,
    }
}

fn engine_err(e: impl fmt::Display) -> VectorError {
    VectorError::Index(e.to_string())
}

fn path_str(path: &Path) -> Result<&str, VectorError> {
    path.to_str()
        .ok_or_else(|| VectorError::Io(format!("index path {} is not UTF-8", path.display())))
}

impl AnnEngine for Usearch {
    fn name(&self) -> &'static str {
        "usearch"
    }

    fn version(&self) -> String {
        usearch::version().to_owned()
    }

    fn create(&self, params: &IndexParams) -> Result<Box<dyn AnnIndex>, VectorError> {
        let index = usearch::Index::new(&options(params)).map_err(engine_err)?;
        Ok(Box::new(UsearchIndex {
            index: RwLock::new(index),
            dimension: params.dimension as usize,
        }))
    }

    fn load(&self, params: &IndexParams, path: &Path) -> Result<Box<dyn AnnIndex>, VectorError> {
        let index = usearch::Index::new(&options(params)).map_err(engine_err)?;
        index.load(path_str(path)?).map_err(engine_err)?;
        if index.dimensions() != params.dimension as usize {
            return Err(VectorError::Index(format!(
                "index file has {} dimensions, the field {}",
                index.dimensions(),
                params.dimension
            )));
        }
        // Searches made with the stored expansion would ignore a changed
        // field setting; the field definition wins.
        index.change_expansion_search(params.expansion_search as usize);
        Ok(Box::new(UsearchIndex {
            index: RwLock::new(index),
            dimension: params.dimension as usize,
        }))
    }
}

/// A USearch index behind a lock: searches share it, changes (which may
/// reallocate) are exclusive.
pub struct UsearchIndex {
    index: RwLock<usearch::Index>,
    dimension: usize,
}

impl fmt::Debug for UsearchIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UsearchIndex")
            .field("dimension", &self.dimension)
            .field("len", &self.len())
            .finish()
    }
}

impl UsearchIndex {
    fn read(&self) -> std::sync::RwLockReadGuard<'_, usearch::Index> {
        self.index.read().unwrap_or_else(|p| p.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, usearch::Index> {
        self.index.write().unwrap_or_else(|p| p.into_inner())
    }

    fn check(&self, v: &[f32]) -> Result<(), VectorError> {
        if v.len() == self.dimension {
            Ok(())
        } else {
            Err(VectorError::Internal(format!(
                "vector of {} components for a {}-dimensional index",
                v.len(),
                self.dimension
            )))
        }
    }
}

impl AnnIndex for UsearchIndex {
    fn len(&self) -> usize {
        self.read().size()
    }

    fn upsert(&self, key: u64, vector: &[f32]) -> Result<(), VectorError> {
        self.check(vector)?;
        let index = self.write();
        if index.contains(key) {
            index.remove(key).map_err(engine_err)?;
        }
        if index.size() + 1 > index.capacity() {
            let grow = (index.capacity() * 2).max(index.size() + 1).max(1024);
            index.reserve(grow).map_err(engine_err)?;
        }
        index.add(key, vector).map_err(engine_err)
    }

    fn remove(&self, key: u64) -> Result<(), VectorError> {
        let index = self.write();
        if index.contains(key) {
            index.remove(key).map_err(engine_err)?;
        }
        Ok(())
    }

    fn search(
        &self,
        query: &[f32],
        k: usize,
        filter: Option<&(dyn Fn(u64) -> bool + Sync)>,
    ) -> Result<Vec<(u64, f32)>, VectorError> {
        self.check(query)?;
        let index = self.read();
        if k == 0 || index.size() == 0 {
            return Ok(Vec::new());
        }
        let matches = match filter {
            None => index.search(query, k),
            Some(f) => index.filtered_search(query, k, f),
        }
        .map_err(engine_err)?;
        Ok(matches.keys.into_iter().zip(matches.distances).collect())
    }

    fn save(&self, path: &Path) -> Result<(), VectorError> {
        let index = self.write();
        index.save(path_str(path)?).map_err(engine_err)
    }

    fn reserve(&self, n: usize) -> Result<(), VectorError> {
        let index = self.write();
        if n > index.capacity() {
            index.reserve(n).map_err(engine_err)?;
        }
        Ok(())
    }

    fn stats(&self) -> EngineStats {
        let index = self.read();
        EngineStats {
            len: index.size() as u64,
            capacity: index.capacity() as u64,
            memory_bytes: index.memory_usage() as u64,
            serialized_bytes: index.serialized_length() as u64,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(metric: Metric) -> IndexParams {
        IndexParams {
            dimension: 3,
            metric,
            quantization: Quantization::F32,
            connectivity: 16,
            expansion_add: 128,
            expansion_search: 64,
        }
    }

    #[test]
    fn upsert_search_remove_save_load() {
        let dir = tempfile::tempdir().unwrap();
        let idx = Usearch.create(&params(Metric::L2)).unwrap();
        for k in 0..100u64 {
            #[allow(clippy::cast_precision_loss)]
            idx.upsert(k, &[k as f32, 0.0, 0.0]).unwrap();
        }
        idx.upsert(5, &[500.0, 0.0, 0.0]).unwrap();
        assert_eq!(idx.len(), 100);
        let hits = idx.search(&[4.9, 0.0, 0.0], 2, None).unwrap();
        assert_eq!(hits[0].0, 4);
        assert!(hits.iter().all(|(k, _)| *k != 5), "replaced vector moved away");

        let even = |k: u64| k.is_multiple_of(2);
        let hits = idx.search(&[7.0, 0.0, 0.0], 3, Some(&even)).unwrap();
        assert!(hits.iter().all(|(k, _)| k.is_multiple_of(2)));
        assert_eq!(hits[0].0, 6);

        idx.remove(6).unwrap();
        idx.remove(6).unwrap();
        assert_eq!(idx.len(), 99);

        let path = dir.path().join("x.usearch");
        idx.save(&path).unwrap();
        let back = Usearch.load(&params(Metric::L2), &path).unwrap();
        assert_eq!(back.len(), 99);
        assert_eq!(
            back.search(&[7.0, 0.0, 0.0], 1, None).unwrap()[0].0,
            idx.search(&[7.0, 0.0, 0.0], 1, None).unwrap()[0].0
        );
        let wrong = IndexParams {
            dimension: 4,
            ..params(Metric::L2)
        };
        assert!(Usearch.load(&wrong, &path).is_err());
        assert!(idx.search(&[1.0, 2.0], 1, None).is_err(), "wrong query dimension");
    }
}
