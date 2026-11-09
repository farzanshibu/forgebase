//! Point-in-time snapshots: managed backups under `<data_dir>/backups/snapshots/`
//! named by their UTC creation time, with a retention policy.

use std::path::{Path, PathBuf};

use forgebase_core::DataLayout;
use forgebase_db::Database;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::backup::{self, BackupOptions, spawn};
use crate::error::{BackupError, Result, io};
use crate::manifest::{MANIFEST, Manifest};

/// Which snapshots to keep.
///
/// `keep_last` is the number of newest snapshots that are always kept;
/// `max_age_days` deletes older ones beyond that. With only `keep_last`, the
/// rest is deleted; with only `max_age_days`, nothing younger is deleted;
/// with neither, nothing is deleted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Retention {
    /// Newest snapshots always kept.
    #[serde(default)]
    pub keep_last: Option<usize>,
    /// Snapshots older than this many days are deleted (beyond `keep_last`).
    #[serde(default)]
    pub max_age_days: Option<u32>,
}

/// A snapshot on disk.
#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    /// Id (directory name).
    pub id: String,
    /// Directory.
    pub path: PathBuf,
    /// Its manifest.
    pub manifest: Manifest,
}

/// The snapshot directory of one data directory.
#[derive(Debug, Clone)]
pub struct SnapshotStore {
    dir: PathBuf,
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 80
        && !id.starts_with('.')
        && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !id.ends_with(".partial")
}

fn parse_time(s: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(s, &Rfc3339).ok()
}

impl SnapshotStore {
    /// The store of `layout`.
    pub fn new(layout: &DataLayout) -> Self {
        Self {
            dir: layout.backups_dir().join("snapshots"),
        }
    }

    /// The directory holding the snapshots.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Directory of snapshot `id`.
    pub fn path_of(&self, id: &str) -> Result<PathBuf> {
        if !valid_id(id) {
            return Err(BackupError::Invalid(format!("{id:?} is not a snapshot id")));
        }
        Ok(self.dir.join(id))
    }

    /// Creates a snapshot named by the current UTC time and applies `retention`.
    pub async fn create(
        &self,
        db: &Database,
        layout: &DataLayout,
        mut opts: BackupOptions,
        retention: Option<Retention>,
    ) -> Result<Manifest> {
        opts.kind = "snapshot".to_owned();
        let now = OffsetDateTime::now_utc();
        let base = format!(
            "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
            now.year(),
            u8::from(now.month()),
            now.day(),
            now.hour(),
            now.minute(),
            now.second()
        );
        let mut id = base.clone();
        for n in 1..1000 {
            if !self.dir.join(&id).exists() && !self.dir.join(format!("{id}.partial")).exists() {
                break;
            }
            id = format!("{base}-{n}");
        }
        let manifest = backup::create(db, layout, &self.dir.join(&id), opts).await?;
        if let Some(r) = retention {
            self.prune(r).await?;
        }
        Ok(manifest)
    }

    /// All complete snapshots, newest first.
    pub fn list(&self) -> Result<Vec<Snapshot>> {
        let mut out = Vec::new();
        if !self.dir.is_dir() {
            return Ok(out);
        }
        for entry in std::fs::read_dir(&self.dir).map_err(io(&self.dir))? {
            let entry = entry.map_err(io(&self.dir))?;
            let id = entry.file_name().to_string_lossy().into_owned();
            if !valid_id(&id) || !entry.path().is_dir() {
                continue;
            }
            if let Ok(manifest) = Manifest::read(&entry.path()) {
                out.push(Snapshot {
                    id,
                    path: entry.path(),
                    manifest,
                });
            }
        }
        out.sort_by(|a, b| b.manifest.created.cmp(&a.manifest.created).then_with(|| b.id.cmp(&a.id)));
        Ok(out)
    }

    /// One snapshot.
    pub fn get(&self, id: &str) -> Result<Snapshot> {
        let path = self.path_of(id)?;
        if !path.is_dir() {
            return Err(BackupError::NotFound(id.to_owned()));
        }
        let manifest = Manifest::read(&path)?;
        Ok(Snapshot {
            id: id.to_owned(),
            path,
            manifest,
        })
    }

    /// Resolves `latest`, a snapshot id, or an RFC 3339 time (the newest
    /// snapshot taken at or before it: point-in-time restore).
    pub fn resolve(&self, spec: &str) -> Result<Snapshot> {
        let all = self.list()?;
        if spec == "latest" {
            return all.into_iter().next().ok_or_else(|| BackupError::NotFound("latest".into()));
        }
        if let Some(at) = parse_time(spec) {
            return all
                .into_iter()
                .find(|s| parse_time(&s.manifest.created).is_some_and(|c| c <= at))
                .ok_or_else(|| BackupError::NotFound(format!("a snapshot taken at or before {spec}")));
        }
        self.get(spec)
    }

    /// Deletes snapshot `id`.
    pub async fn delete(&self, id: &str) -> Result<()> {
        let path = self.path_of(id)?;
        if !path.is_dir() {
            return Err(BackupError::NotFound(id.to_owned()));
        }
        spawn(move || {
            // Take the manifest away first so a half-deleted directory is never listed.
            let _ = std::fs::remove_file(path.join(MANIFEST));
            std::fs::remove_dir_all(&path).map_err(io(&path))
        })
        .await
    }

    /// Applies `retention`; returns the ids deleted. Also removes abandoned
    /// `*.partial` directories older than one hour.
    pub async fn prune(&self, retention: Retention) -> Result<Vec<String>> {
        let all = self.list()?;
        let now = OffsetDateTime::now_utc();
        let mut doomed = Vec::new();
        for (i, s) in all.iter().enumerate() {
            let protected = retention.keep_last.is_some_and(|n| i < n);
            let young = retention.max_age_days.is_some_and(|d| {
                parse_time(&s.manifest.created).is_some_and(|c| now - c < time::Duration::days(i64::from(d)))
            });
            let keep = match (retention.keep_last, retention.max_age_days) {
                (None, None) => true,
                (Some(_), None) => protected,
                (None, Some(_)) => young,
                (Some(_), Some(_)) => protected || young,
            };
            if !keep {
                doomed.push(s.id.clone());
            }
        }
        for id in &doomed {
            self.delete(id).await?;
        }
        self.remove_stale_partials();
        Ok(doomed)
    }

    fn remove_stale_partials(&self) {
        let Ok(rd) = std::fs::read_dir(&self.dir) else { return };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let old = e
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > std::time::Duration::from_secs(3600));
            if name.ends_with(".partial") && old {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
}
