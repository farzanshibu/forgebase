//! Shard primaries on this host (ADR-0029 §3.9, FB-174).
//!
//! A local shard is a writable cluster in a node layout
//! (`<data_dir>/postgres/nodes/<name>/`): `initdb` on first start, the same
//! bootstrap and role passwords as the primary (so the server reaches it
//! with the same roles), its own socket directory, never on TCP. A file
//! `shard` in the node directory holds the set id and tells it from a
//! replica's directory.
//!
//! Removing a shard keeps its data: the node directory moves to
//! `<data_dir>/postgres/retired/<name>-<time>/` until an operator deletes
//! it ([`LocalShards::delete_retired`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::error::{EngineError, Result};
use crate::{Engine, EngineConfig, EngineHandle, Layout, NodeKind, fsutil};

/// The marker file of a shard node directory (contents: the set id).
pub const SHARD_MARKER: &str = "shard";

/// Directory of retired shard data under `<data_dir>/postgres`.
pub const RETIRED_DIR: &str = "retired";

/// A retired shard's data kept on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetiredData {
    /// `<name>-<time>` (the directory name; give it to
    /// [`LocalShards::delete_retired`]).
    pub id: String,
    /// The shard node's name.
    pub name: String,
    /// The set id it held, when recorded.
    pub set_id: Option<String>,
    /// Where it is.
    pub path: PathBuf,
    /// Bytes on disk.
    pub bytes: u64,
}

/// The shard primaries on this host. The server keeps one and calls it from
/// the `database.shard.*` jobs.
#[derive(Debug)]
pub struct LocalShards {
    data_dir: PathBuf,
    cfg: EngineConfig,
    nodes: Mutex<BTreeMap<String, Arc<EngineHandle>>>,
}

impl LocalShards {
    /// Shards of the server in `data_dir`, started with `cfg` (the
    /// primary's engine configuration; shards never listen on TCP).
    pub fn new(data_dir: impl Into<PathBuf>, cfg: &EngineConfig) -> Self {
        let mut cfg = cfg.clone();
        cfg.listen = None;
        cfg.remove_old_engines = false;
        Self {
            data_dir: data_dir.into(),
            cfg,
            nodes: Mutex::new(BTreeMap::new()),
        }
    }

    /// The layout of shard node `name`.
    ///
    /// # Errors
    /// An invalid node name.
    pub fn layout(&self, name: &str) -> Result<Layout> {
        Layout::node(&self.data_dir, name, NodeKind::Primary)
    }

    fn marker(&self, name: &str) -> Result<PathBuf> {
        Ok(self.layout(name)?.node_dir().join(SHARD_MARKER))
    }

    fn retired_root(&self) -> PathBuf {
        self.data_dir.join("postgres").join(RETIRED_DIR)
    }

    /// Creates and starts shard node `name` for set `set_id`: `initdb`,
    /// bootstrap (roles, database `forgebase`), supervised. Refuses a
    /// directory that exists and is not this set's shard (a replica, or
    /// another set's shard).
    ///
    /// # Errors
    /// The name is taken, or a step fails (a half-made cluster is removed).
    pub async fn add(&self, name: &str, set_id: &str) -> Result<Arc<EngineHandle>> {
        let layout = self.layout(name)?;
        if self.nodes.lock().await.contains_key(name) {
            return Err(EngineError::State(format!("shard node {name} is already running")));
        }
        let dir = layout.node_dir();
        let marker = self.marker(name)?;
        if dir.exists() {
            match std::fs::read_to_string(&marker) {
                Ok(s) if s.trim() == set_id => {}
                _ => {
                    return Err(EngineError::State(format!(
                        "{} exists and is not a shard of {set_id}; pick another node name",
                        dir.display()
                    )));
                }
            }
        }
        fsutil::private_dir(layout.postgres_dir())?;
        fsutil::private_dir(&dir)?;
        std::fs::write(&marker, set_id).map_err(|e| EngineError::io("write", &marker, e))?;
        let fresh = !layout.pgdata().exists();
        let handle = match Engine::prepare(&layout, &self.cfg).await {
            Ok(h) => Arc::new(h),
            Err(e) => {
                if fresh {
                    let _ = fsutil::remove_tree(&dir);
                }
                return Err(e);
            }
        };
        self.nodes.lock().await.insert(name.to_owned(), handle.clone());
        Ok(handle)
    }

    /// Starts every shard node on disk that is not running (server start).
    /// Returns each node tried with its set id and outcome.
    pub async fn start_existing(&self) -> Vec<(String, String, Result<()>)> {
        let mut out = Vec::new();
        for (name, set_id) in self.on_disk() {
            if self.nodes.lock().await.contains_key(&name) {
                continue;
            }
            let r = self.start(&name).await.map(|_| ());
            out.push((name, set_id, r));
        }
        out
    }

    /// Shard node directories on disk: `(name, set id)`.
    pub fn on_disk(&self) -> Vec<(String, String)> {
        let dir = self.data_dir.join("postgres").join("nodes");
        let mut found: Vec<(String, String)> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let set = std::fs::read_to_string(e.path().join(SHARD_MARKER)).ok()?;
                Some((e.file_name().to_str()?.to_owned(), set.trim().to_owned()))
            })
            .collect();
        found.sort();
        found
    }

    /// Starts the provisioned shard node `name`.
    ///
    /// # Errors
    /// Not a shard node, or the engine fails to start.
    pub async fn start(&self, name: &str) -> Result<Arc<EngineHandle>> {
        if let Some(h) = self.get(name).await {
            return Ok(h);
        }
        if !self.marker(name)?.is_file() {
            return Err(EngineError::State(format!("{name} is not a shard node on this host")));
        }
        let layout = self.layout(name)?;
        let h = Arc::new(Engine::prepare(&layout, &self.cfg).await?);
        self.nodes.lock().await.insert(name.to_owned(), h.clone());
        Ok(h)
    }

    /// The running shard node `name`.
    pub async fn get(&self, name: &str) -> Option<Arc<EngineHandle>> {
        self.nodes.lock().await.get(name).cloned()
    }

    /// Names of the running shard nodes.
    pub async fn names(&self) -> Vec<String> {
        self.nodes.lock().await.keys().cloned().collect()
    }

    /// Stops shard node `name` (fast shutdown); its data stays.
    ///
    /// # Errors
    /// Never today.
    pub async fn stop(&self, name: &str) -> Result<()> {
        let h = self.nodes.lock().await.remove(name);
        if let Some(h) = h {
            h.stop().await?;
        }
        Ok(())
    }

    /// Stops every running shard node (server shutdown).
    pub async fn stop_all(&self) {
        let all: Vec<Arc<EngineHandle>> = std::mem::take(&mut *self.nodes.lock().await).into_values().collect();
        for h in all {
            let _ = h.stop().await;
        }
    }

    /// Stops shard node `name` and moves its directory to
    /// `retired/<name>-<time>` (ADR-0029 §3.9: data kept until "Delete
    /// data"). Returns where it went.
    ///
    /// # Errors
    /// Not a shard node, or the rename fails.
    pub async fn retire(&self, name: &str) -> Result<RetiredData> {
        let layout = self.layout(name)?;
        let dir = layout.node_dir();
        let marker = self.marker(name)?;
        let set_id = std::fs::read_to_string(&marker)
            .map(|s| s.trim().to_owned())
            .map_err(|_| EngineError::State(format!("{name} is not a shard node on this host")))?;
        self.stop(name).await?;
        let root = self.retired_root();
        fsutil::private_dir(&root)?;
        let stamp = time::OffsetDateTime::now_utc().unix_timestamp();
        let id = format!("{name}-{stamp}");
        let to = root.join(&id);
        std::fs::rename(&dir, &to).map_err(|e| EngineError::io("move", &dir, e))?;
        Ok(RetiredData {
            id,
            name: name.to_owned(),
            set_id: Some(set_id),
            bytes: dir_size(&to),
            path: to,
        })
    }

    /// Retired shard data on disk, oldest first.
    pub fn retired(&self) -> Vec<RetiredData> {
        let mut out: Vec<RetiredData> = std::fs::read_dir(self.retired_root())
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let id = e.file_name().to_str()?.to_owned();
                let (name, stamp) = id.rsplit_once('-')?;
                stamp.parse::<i64>().ok()?;
                Some(RetiredData {
                    name: name.to_owned(),
                    set_id: std::fs::read_to_string(e.path().join(SHARD_MARKER))
                        .ok()
                        .map(|s| s.trim().to_owned()),
                    bytes: dir_size(&e.path()),
                    path: e.path(),
                    id,
                })
            })
            .collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    /// Deletes retired data `id` (a `<name>-<time>` from [`LocalShards::retired`]).
    ///
    /// # Errors
    /// Unknown id, or the deletion fails.
    pub fn delete_retired(&self, id: &str) -> Result<()> {
        let found = self
            .retired()
            .into_iter()
            .find(|r| r.id == id)
            .ok_or_else(|| EngineError::State(format!("no retired shard data {id}")))?;
        fsutil::remove_tree(&found.path).map_err(|e| EngineError::io("remove", &found.path, e))
    }
}

fn dir_size(p: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(p) else {
        return 0;
    };
    if meta.is_file() {
        return meta.len();
    }
    if !meta.is_dir() {
        return 0;
    }
    std::fs::read_dir(p)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| dir_size(&e.path()))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retired_data_is_listed_and_deleted_by_id() {
        let tmp = tempfile::tempdir().unwrap();
        let shards = LocalShards::new(tmp.path(), &EngineConfig::default());
        let node = shards.layout("s-1").unwrap().node_dir();
        std::fs::create_dir_all(node.join("data")).unwrap();
        std::fs::write(node.join(SHARD_MARKER), "set1").unwrap();
        std::fs::write(node.join("data/x"), b"12345").unwrap();
        assert_eq!(shards.on_disk(), vec![("s-1".to_owned(), "set1".to_owned())]);
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let r = rt.block_on(shards.retire("s-1")).unwrap();
        assert!(r.id.starts_with("s-1-"));
        assert_eq!(r.set_id.as_deref(), Some("set1"));
        assert!(r.bytes >= 5);
        assert!(!node.exists());
        assert!(shards.on_disk().is_empty());
        let listed = shards.retired();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "s-1");
        assert!(shards.delete_retired("nope").is_err());
        shards.delete_retired(&r.id).unwrap();
        assert!(shards.retired().is_empty());
        // Not a shard: refused.
        assert!(rt.block_on(shards.retire("other")).is_err());
        assert!(shards.layout("Bad Name").is_err());
    }
}
