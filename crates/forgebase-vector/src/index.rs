//! One field's derived ANN index: its life cycle, files and statistics.
//!
//! Files (ADR-0001 §8, derived data): `<data_dir>/indexes/vector/<collection
//! id>/<field>.usearch` and `<field>.manifest.json`. The manifest records the
//! format, engine and version, build parameters, vector count, the
//! collection's change sequence the file reflects, and the file's size and
//! SHA-256. A file is used only when every one of them matches; otherwise it
//! is rebuilt from the canonical vectors in SQLite. The checksum is verified
//! before the engine parses the file, so a damaged file never reaches it.
//!
//! Writes are atomic: temporary file, rename; the manifest last. Files are
//! not fsynced (on macOS that is a full device flush, seconds per file on
//! some disks): derived data needs no durability, only detection. Whatever a
//! crash or power loss leaves behind (a torn file, a new manifest with an old
//! file, or the reverse) fails the size, checksum or sequence check and is
//! rebuilt from SQLite, which is durable.
//!
//! Every method here blocks (file I/O, engine calls).

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::engine::{AnnEngine, AnnIndex, IndexParams};
use crate::error::VectorError;
use crate::model::{IndexState, Progress, RebuildInfo, RecallReport, VectorField};

/// Version of the file layout. Changing it makes every existing index file
/// stale (rebuilt on next use).
pub(crate) const FORMAT: u32 = 1;

/// Why an index file was not used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Unusable {
    /// `missing`, `corrupt`, `stale`, `version_mismatch`, `params_changed`.
    pub reason: &'static str,
    /// Human-readable detail.
    pub detail: String,
}

impl Unusable {
    fn new(reason: &'static str, detail: impl Into<String>) -> Self {
        Self {
            reason,
            detail: detail.into(),
        }
    }
}

/// Contents of `<field>.manifest.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Manifest {
    pub format: u32,
    pub engine: String,
    pub engine_version: String,
    pub dimension: u32,
    pub metric: String,
    pub quantization: String,
    pub connectivity: u32,
    pub expansion_add: u32,
    pub count: u64,
    pub change_seq: i64,
    pub file_bytes: u64,
    pub sha256: String,
    pub saved: String,
}

/// Mutable state, behind one mutex (never held across an await).
#[derive(Debug)]
pub(crate) struct Status {
    pub state: IndexState,
    pub reason: Option<String>,
    /// Keys written while a rebuild runs; replayed onto the new index.
    pub pending: Option<HashSet<u64>>,
    /// Incremented when a rebuild starts; a finishing rebuild with an older
    /// generation is discarded.
    pub generation: u64,
    /// The serving index differs from its file.
    pub dirty: bool,
    /// Manifest on disk, if the serving index was loaded from or saved to it.
    pub manifest: Option<Manifest>,
    pub last_saved: Option<String>,
    pub last_rebuild: Option<RebuildInfo>,
    pub last_recall: Option<RecallReport>,
}

/// Search counters.
#[derive(Debug, Default)]
pub(crate) struct Counters {
    pub searches: AtomicU64,
    pub ann: AtomicU64,
    pub exact: AtomicU64,
    pub micros: AtomicU64,
}

/// One vector field's index.
#[derive(Debug)]
pub(crate) struct FieldIndex {
    pub def: VectorField,
    pub params: IndexParams,
    index_path: PathBuf,
    manifest_path: PathBuf,
    live: RwLock<Option<Arc<dyn AnnIndex>>>,
    status: Mutex<Status>,
    pub done: AtomicU64,
    pub total: AtomicU64,
    pub counters: Counters,
    /// pgvector engine (FB-162): the field's HNSW index exists and is valid.
    pub pg_valid: AtomicBool,
    /// pgvector engine: size of that index (last seen).
    pub pg_bytes: AtomicU64,
    /// pgvector engine: vectors it covers (last seen).
    pub pg_indexed: AtomicU64,
}

/// Build parameters of a field.
pub(crate) fn params_of(f: &VectorField) -> IndexParams {
    IndexParams {
        dimension: f.dimension,
        metric: f.metric,
        quantization: f.quantization,
        connectivity: f.connectivity,
        expansion_add: f.expansion_add,
        expansion_search: f.expansion_search,
    }
}

fn sha256_file(path: &Path) -> std::io::Result<(u64, String)> {
    let mut f = File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut n = 0u64;
    loop {
        let r = f.read(&mut buf)?;
        if r == 0 {
            break;
        }
        h.update(&buf[..r]);
        n += r as u64;
    }
    let hex = h.finalize().iter().map(|b| format!("{b:02x}")).collect::<String>();
    Ok((n, hex))
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    {
        let mut f = File::create(&tmp)?;
        f.write_all(bytes)?;
    }
    fs::rename(&tmp, path)
}

fn major_minor(v: &str) -> &str {
    match v.match_indices('.').nth(1) {
        Some((i, _)) => &v[..i],
        None => v,
    }
}

impl FieldIndex {
    /// An index that is not loaded yet.
    pub(crate) fn unloaded(def: VectorField, dir: &Path) -> Self {
        let params = params_of(&def);
        Self {
            index_path: dir.join(format!("{}.usearch", def.name)),
            manifest_path: dir.join(format!("{}.manifest.json", def.name)),
            def,
            params,
            live: RwLock::new(None),
            status: Mutex::new(Status {
                state: IndexState::Unloaded,
                reason: None,
                pending: None,
                generation: 0,
                dirty: false,
                manifest: None,
                last_saved: None,
                last_rebuild: None,
                last_recall: None,
            }),
            done: AtomicU64::new(0),
            total: AtomicU64::new(0),
            counters: Counters::default(),
            pg_valid: AtomicBool::new(false),
            pg_bytes: AtomicU64::new(0),
            pg_indexed: AtomicU64::new(0),
        }
    }

    /// A field of the pgvector engine: no file, no in-memory index; its HNSW
    /// index lives in PostgreSQL (`valid` once built).
    pub(crate) fn on_postgres(def: VectorField, dir: &Path, valid: bool) -> Self {
        let idx = Self::unloaded(def, dir);
        idx.status().state = IndexState::Ready;
        idx.pg_valid.store(valid, Ordering::Relaxed);
        idx
    }

    /// A new, empty, serving index (a field that has no vectors yet).
    pub(crate) fn empty(def: VectorField, dir: &Path, engine: &dyn AnnEngine) -> Result<Self, VectorError> {
        let idx = Self::unloaded(def, dir);
        let live = engine.create(&idx.params)?;
        *idx.live.write().unwrap_or_else(|p| p.into_inner()) = Some(Arc::from(live));
        {
            let mut st = idx.status();
            st.state = IndexState::Ready;
            st.dirty = true;
        }
        Ok(idx)
    }

    pub(crate) fn status(&self) -> MutexGuard<'_, Status> {
        self.status.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The serving index, if any (also while a rebuild runs).
    pub(crate) fn live(&self) -> Option<Arc<dyn AnnIndex>> {
        self.live.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub(crate) fn set_live(&self, idx: Option<Arc<dyn AnnIndex>>) {
        *self.live.write().unwrap_or_else(|p| p.into_inner()) = idx;
    }

    pub(crate) fn progress(&self) -> Progress {
        // Rows inserted while the rebuild scans are picked up too.
        let done = self.done.load(Ordering::Relaxed);
        Progress {
            done,
            total: self.total.load(Ordering::Relaxed).max(done),
        }
    }

    /// Loads the file if it matches the canonical data at `change_seq` with
    /// `count` vectors. On success the index is `Ready`.
    pub(crate) fn load(&self, engine: &dyn AnnEngine, change_seq: i64, count: u64) -> Result<(), Unusable> {
        let text = match fs::read_to_string(&self.manifest_path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(Unusable::new("missing", "no index file"));
            }
            Err(e) => return Err(Unusable::new("corrupt", format!("manifest unreadable: {e}"))),
        };
        let m: Manifest = serde_json::from_str(&text)
            .map_err(|e| Unusable::new("corrupt", format!("manifest does not parse: {e}")))?;
        if m.format != FORMAT || m.engine != engine.name() {
            return Err(Unusable::new(
                "version_mismatch",
                format!(
                    "file format {} of {} (expected {FORMAT} of {})",
                    m.format,
                    m.engine,
                    engine.name()
                ),
            ));
        }
        let current = engine.version();
        if major_minor(&m.engine_version) != major_minor(&current) {
            return Err(Unusable::new(
                "version_mismatch",
                format!("written by {} {}, running {current}", m.engine, m.engine_version),
            ));
        }
        let p = &self.params;
        if m.dimension != p.dimension
            || m.metric != p.metric.as_str()
            || m.quantization != p.quantization.as_str()
            || m.connectivity != p.connectivity
            || m.expansion_add != p.expansion_add
        {
            return Err(Unusable::new(
                "params_changed",
                "build parameters differ from the field",
            ));
        }
        if m.change_seq != change_seq || m.count != count {
            return Err(Unusable::new(
                "stale",
                format!(
                    "file reflects change {} with {} vectors, data is at change {change_seq} with {count}",
                    m.change_seq, m.count
                ),
            ));
        }
        let (bytes, sum) = match sha256_file(&self.index_path) {
            Ok(v) => v,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(Unusable::new("missing", "index file is missing"));
            }
            Err(e) => return Err(Unusable::new("corrupt", format!("index file unreadable: {e}"))),
        };
        if bytes != m.file_bytes || sum != m.sha256 {
            return Err(Unusable::new(
                "corrupt",
                "index file checksum does not match its manifest",
            ));
        }
        let idx = engine
            .load(p, &self.index_path)
            .map_err(|e| Unusable::new("corrupt", e.to_string()))?;
        if idx.len() as u64 != count {
            return Err(Unusable::new(
                "corrupt",
                format!("index holds {} vectors, expected {count}", idx.len()),
            ));
        }
        self.set_live(Some(Arc::from(idx)));
        let mut st = self.status();
        st.state = IndexState::Ready;
        st.reason = None;
        st.dirty = false;
        st.manifest = Some(m);
        Ok(())
    }

    /// Writes the serving index (when it changed) and a manifest for
    /// `change_seq`. Callers hold the collection's write lock, so no change is
    /// applied meanwhile and every committed change up to `change_seq` is in
    /// the serving index.
    pub(crate) fn save(&self, engine: &dyn AnnEngine, change_seq: i64) -> Result<bool, VectorError> {
        let Some(live) = self.live() else {
            return Ok(false);
        };
        let (dirty, manifest) = {
            let st = self.status();
            (st.dirty, st.manifest.clone())
        };
        if let Some(dir) = self.index_path.parent() {
            fs::create_dir_all(dir)?;
        }
        let now = forgebase_data::schema::now_timestamp();
        let manifest = match (dirty, manifest) {
            (false, Some(m)) if m.change_seq == change_seq => return Ok(false),
            (false, Some(mut m)) => {
                // Index unchanged since its file was written; only the
                // sequence it reflects moved (writes to other fields).
                m.change_seq = change_seq;
                m.saved = now.clone();
                m
            }
            _ => {
                let tmp = self.index_path.with_extension("usearch.tmp");
                live.save(&tmp)?;
                let (file_bytes, sha256) = sha256_file(&tmp)?;
                fs::rename(&tmp, &self.index_path)?;
                Manifest {
                    format: FORMAT,
                    engine: engine.name().to_owned(),
                    engine_version: engine.version(),
                    dimension: self.params.dimension,
                    metric: self.params.metric.as_str().to_owned(),
                    quantization: self.params.quantization.as_str().to_owned(),
                    connectivity: self.params.connectivity,
                    expansion_add: self.params.expansion_add,
                    count: live.len() as u64,
                    change_seq,
                    file_bytes,
                    sha256,
                    saved: now.clone(),
                }
            }
        };
        let json = serde_json::to_vec_pretty(&manifest).map_err(|e| VectorError::Internal(e.to_string()))?;
        write_atomic(&self.manifest_path, &json)?;
        let mut st = self.status();
        st.dirty = false;
        st.manifest = Some(manifest);
        st.last_saved = Some(now);
        Ok(true)
    }

    /// Applies committed changes (`None` = no vector any more) to the
    /// serving index and records them for a running rebuild.
    pub(crate) fn apply(&self, changes: &[(u64, Option<&[f32]>)]) -> Result<(), VectorError> {
        {
            let mut st = self.status();
            if let Some(p) = st.pending.as_mut() {
                p.extend(changes.iter().map(|(k, _)| *k));
            }
            st.dirty = true;
        }
        if let Some(live) = self.live() {
            for (k, v) in changes {
                match v {
                    Some(v) => live.upsert(*k, v)?,
                    None => live.remove(*k)?,
                }
            }
        }
        Ok(())
    }

    /// Deletes the files (field or collection dropped).
    pub(crate) fn remove_files(&self) {
        for p in [&self.index_path, &self.manifest_path] {
            if let Err(e) = fs::remove_file(p)
                && e.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(path = %p.display(), error = %e, "could not delete vector index file");
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn paths(&self) -> (&Path, &Path) {
        (&self.index_path, &self.manifest_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Usearch;
    use crate::model::{Metric, Quantization};

    fn def() -> VectorField {
        VectorField {
            name: "emb".into(),
            dimension: 2,
            metric: Metric::L2,
            quantization: Quantization::F32,
            connectivity: 16,
            expansion_add: 64,
            expansion_search: 64,
            created: String::new(),
        }
    }

    #[test]
    fn save_then_load_and_every_mismatch_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let idx = FieldIndex::empty(def(), dir.path(), &Usearch).unwrap();
        idx.apply(&[(1, Some(&[1.0, 0.0])), (2, Some(&[0.0, 1.0]))]).unwrap();
        assert!(idx.save(&Usearch, 7).unwrap());
        assert!(!idx.save(&Usearch, 7).unwrap(), "nothing to do");

        let fresh = FieldIndex::unloaded(def(), dir.path());
        assert_eq!(fresh.load(&Usearch, 8, 2).unwrap_err().reason, "stale");
        assert_eq!(fresh.load(&Usearch, 7, 3).unwrap_err().reason, "stale");
        fresh.load(&Usearch, 7, 2).unwrap();
        assert_eq!(fresh.live().unwrap().len(), 2);

        // Manifest-only refresh keeps the checksum valid.
        assert!(idx.save(&Usearch, 9).unwrap());
        FieldIndex::unloaded(def(), dir.path()).load(&Usearch, 9, 2).unwrap();

        let (file, manifest) = idx.paths();
        let mut bytes = fs::read(file).unwrap();
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0xff;
        fs::write(file, &bytes).unwrap();
        assert_eq!(
            FieldIndex::unloaded(def(), dir.path())
                .load(&Usearch, 9, 2)
                .unwrap_err()
                .reason,
            "corrupt"
        );

        let mut m: Manifest = serde_json::from_str(&fs::read_to_string(manifest).unwrap()).unwrap();
        m.engine_version = "0.1.0".into();
        fs::write(manifest, serde_json::to_vec(&m).unwrap()).unwrap();
        assert_eq!(
            FieldIndex::unloaded(def(), dir.path())
                .load(&Usearch, 9, 2)
                .unwrap_err()
                .reason,
            "version_mismatch"
        );

        let other = VectorField {
            connectivity: 8,
            ..def()
        };
        m.engine_version = Usearch.version();
        fs::write(manifest, serde_json::to_vec(&m).unwrap()).unwrap();
        assert_eq!(
            FieldIndex::unloaded(other, dir.path())
                .load(&Usearch, 9, 2)
                .unwrap_err()
                .reason,
            "params_changed"
        );

        idx.remove_files();
        assert_eq!(
            FieldIndex::unloaded(def(), dir.path())
                .load(&Usearch, 9, 2)
                .unwrap_err()
                .reason,
            "missing"
        );
    }

    #[test]
    fn version_prefix() {
        assert_eq!(major_minor("2.26.6"), "2.26");
        assert_eq!(major_minor("2"), "2");
    }
}
