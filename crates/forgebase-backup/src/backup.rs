//! Online consistent backup of a data directory.
//!
//! The system database is copied with `VACUUM INTO` (one read snapshot of the
//! committed state, no pause of readers or of the server). Storage blobs and
//! index files are copied afterwards. Blobs are content-addressed and
//! immutable, so a blob written after the database snapshot is only an
//! orphan in the copy (the reconciler removes it); a blob deleted in between
//! is reported by `verify`/`doctor` as missing metadata target. The backup is
//! written to `<dest>.partial` and renamed once `MANIFEST.json` is on disk,
//! so a crash never leaves something that looks complete.

use std::path::{Path, PathBuf};

use forgebase_core::DataLayout;
use forgebase_db::Database;

use crate::error::{BackupError, Result, io};
use crate::fsutil::{self, copy_hashed, join_rel, mkdir_private, sha256_file, walk_files};
use crate::manifest::{DB_FILE, FORMAT, FileEntry, FileKind, KEY_FILE, KeyInfo, MANIFEST, Manifest};

/// Top-level entries of the storage directory that are never backed up
/// (in-flight temporary files of the local driver).
const STORAGE_SKIP: &[&str] = &[".tmp"];

/// What goes into a backup.
#[derive(Debug, Clone)]
pub struct BackupOptions {
    /// Copy derived index files too (faster restore; they are rebuildable).
    pub include_indexes: bool,
    /// Copy `auth/master.key` into the backup. Off by default: a backup that
    /// holds the key next to the data it protects defeats key separation.
    pub include_master_key: bool,
    /// Free-form label.
    pub label: Option<String>,
    /// `backup` or `snapshot`.
    pub kind: String,
}

impl Default for BackupOptions {
    fn default() -> Self {
        Self {
            include_indexes: true,
            include_master_key: false,
            label: None,
            kind: "backup".to_owned(),
        }
    }
}

#[cfg(unix)]
fn make_private(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(io(path))
}

#[cfg(not(unix))]
fn make_private(_path: &Path) -> Result<()> {
    Ok(())
}

/// Fingerprint of a key file: first 16 hex characters of its SHA-256.
pub fn key_fingerprint(path: &Path) -> Result<Option<String>> {
    if !path.is_file() {
        return Ok(None);
    }
    let (sha, _) = sha256_file(path)?;
    Ok(Some(sha[..16].to_owned()))
}

/// Path of the master key inside a data directory.
pub fn master_key_path(layout: &DataLayout) -> PathBuf {
    join_rel(layout.root(), KEY_FILE)
}

/// Creates a backup at `dest` (which must not exist) while `db` stays in use.
pub async fn create(db: &Database, layout: &DataLayout, dest: &Path, opts: BackupOptions) -> Result<Manifest> {
    if dest.exists() {
        return Err(BackupError::Conflict(format!("{} already exists", dest.display())));
    }
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| BackupError::Invalid(format!("{} is not a usable backup path", dest.display())))?;
    let partial = dest.with_file_name(format!("{name}.partial"));
    {
        let (partial, dest) = (partial.clone(), dest.to_path_buf());
        spawn(move || {
            if let Some(parent) = dest.parent() {
                mkdir_private(parent).map_err(io(parent))?;
            }
            if partial.exists() {
                std::fs::remove_dir_all(&partial).map_err(io(&partial))?;
            }
            mkdir_private(&partial).map_err(io(&partial))
        })
        .await?;
    }

    let db_copy = partial.join(DB_FILE);
    db.snapshot_to(&db_copy).await?;
    let schema_version = db.schema_version().await?;

    let layout = layout.clone();
    let id = name.clone();
    let manifest = spawn(move || -> Result<Manifest> {
        make_private(&db_copy)?;
        let (sha, size) = sha256_file(&db_copy)?;
        let mut files = vec![FileEntry {
            path: DB_FILE.to_owned(),
            size,
            sha256: sha,
            kind: FileKind::Database,
        }];
        copy_tree(
            &layout.storage_dir(),
            &partial,
            "storage",
            FileKind::Storage,
            STORAGE_SKIP,
            &mut files,
        )?;
        if opts.include_indexes {
            copy_tree(
                &layout.indexes_dir(),
                &partial,
                "indexes",
                FileKind::Index,
                &[],
                &mut files,
            )?;
        }
        let key_src = master_key_path(&layout);
        let fingerprint = key_fingerprint(&key_src)?;
        let included = opts.include_master_key && fingerprint.is_some();
        if included {
            let dst = join_rel(&partial, KEY_FILE);
            let (sha, size) = copy_hashed(&key_src, &dst)?;
            files.push(FileEntry {
                path: KEY_FILE.to_owned(),
                size,
                sha256: sha,
                kind: FileKind::Key,
            });
        }
        let manifest = Manifest {
            format: FORMAT,
            id,
            kind: opts.kind,
            label: opts.label,
            created: forgebase_core::now_rfc3339(),
            forgebase_version: env!("CARGO_PKG_VERSION").to_owned(),
            schema_version,
            includes_indexes: opts.include_indexes,
            master_key: KeyInfo { included, fingerprint },
            files,
        };
        let json = serde_json::to_vec_pretty(&manifest).map_err(|e| BackupError::Internal(e.to_string()))?;
        fsutil::write_atomic(&partial.join(MANIFEST), &json)?;
        std::fs::rename(&partial, &dest_of(&partial)).map_err(io(&partial))?;
        if let Some(parent) = partial.parent() {
            fsutil::sync_dir(parent);
        }
        Ok(manifest)
    })
    .await?;
    tracing::info!(id = %manifest.id, files = manifest.files.len(), bytes = manifest.total_bytes(), "backup written");
    Ok(manifest)
}

/// `x.partial` → `x`.
fn dest_of(partial: &Path) -> PathBuf {
    let name = partial
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    partial.with_file_name(name.trim_end_matches(".partial"))
}

fn copy_tree(
    src_root: &Path,
    dst_root: &Path,
    prefix: &str,
    kind: FileKind,
    skip: &[&str],
    out: &mut Vec<FileEntry>,
) -> Result<()> {
    for rel in walk_files(src_root, skip)? {
        let src = join_rel(src_root, &rel);
        let dst = join_rel(&join_rel(dst_root, prefix), &rel);
        match copy_hashed(&src, &dst) {
            Ok((sha, size)) => out.push(FileEntry {
                path: format!("{prefix}/{rel}"),
                size,
                sha256: sha,
                kind,
            }),
            // Removed by the server while we walked (reconciler, index saver).
            Err(BackupError::Io { path, source }) if source.kind() == std::io::ErrorKind::NotFound && path == src => {
                let _ = std::fs::remove_file(&dst);
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

pub(crate) async fn spawn<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| BackupError::Internal(format!("task failed: {e}")))?
}
