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
use forgebase_db::{BackendKind, Database};

use crate::error::{BackupError, Result, io};
use crate::fsutil::{self, copy_hashed, join_rel, mkdir_private, sha256_file, walk_files};
use crate::manifest::{
    DB_FILE, DUMP_FILE, EngineInfo, FORMAT, FORMAT_PG, FileEntry, FileKind, KEY_FILE, KeyInfo, MANIFEST, Manifest,
    Method, PG_DIR, ROLES_FILE,
};
use crate::pg::{self, DumpPlan, PgEndpoint, PgSource};

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
    /// How to reach a PostgreSQL cluster (required when the database is
    /// PostgreSQL: the engine's tools do the copy). Ignored on SQLite.
    pub pg: Option<PgSource>,
    /// Back up the database only: no storage, indexes or master key. A
    /// restore of such a backup leaves those alone (pre-migration snapshots).
    pub database_only: bool,
    /// Recorded in the manifest.
    pub note: Option<String>,
}

impl Default for BackupOptions {
    fn default() -> Self {
        Self {
            include_indexes: true,
            include_master_key: false,
            label: None,
            kind: "backup".to_owned(),
            pg: None,
            database_only: false,
            note: None,
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
    create_with(Some(db), db.kind(), None, layout, dest, opts, false).await
}

/// One set of a cluster backup (FB-179): like [`create`], but the data
/// directory may hold replicas and other sets' clusters (the single-set
/// guard is the cluster backup's own job).
pub(crate) async fn create_member(
    db: &Database,
    layout: &DataLayout,
    dest: &Path,
    opts: BackupOptions,
) -> Result<Manifest> {
    create_with(Some(db), db.kind(), None, layout, dest, opts, true).await
}

/// [`create`] without a database handle, for a PostgreSQL cluster whose
/// schema version the caller already knows (the pre-migration snapshot runs
/// inside `Database::migrate`, which holds the only writer connection).
///
/// # Errors
/// As [`create`]; SQLite needs a handle and is refused.
pub async fn create_physical(
    layout: &DataLayout,
    dest: &Path,
    schema_version: i64,
    opts: BackupOptions,
) -> Result<Manifest> {
    create_with(
        None,
        BackendKind::Postgres,
        Some(schema_version),
        layout,
        dest,
        opts,
        false,
    )
    .await
}

async fn create_with(
    db: Option<&Database>,
    kind: BackendKind,
    known_version: Option<i64>,
    layout: &DataLayout,
    dest: &Path,
    opts: BackupOptions,
    member: bool,
) -> Result<Manifest> {
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

    let (db_files, engine) = copy_database(db, kind, layout, &partial, &opts, &name, member).await?;
    let schema_version = match (known_version, db) {
        (Some(v), _) => v,
        (None, Some(db)) => db.schema_version().await?,
        (None, None) => {
            return Err(BackupError::Internal(
                "no database to read the schema version from".into(),
            ));
        }
    };
    let format = if engine.is_postgres() { FORMAT_PG } else { FORMAT };

    let layout = layout.clone();
    let id = name.clone();
    let manifest = spawn(move || -> Result<Manifest> {
        let mut files = db_files;
        if !opts.database_only {
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
        }
        let key_src = master_key_path(&layout);
        let fingerprint = key_fingerprint(&key_src)?;
        let included = !opts.database_only && opts.include_master_key && fingerprint.is_some();
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
            format,
            id,
            kind: opts.kind,
            label: opts.label,
            created: forgebase_core::now_rfc3339(),
            forgebase_version: env!("CARGO_PKG_VERSION").to_owned(),
            schema_version,
            includes_indexes: opts.include_indexes && !opts.database_only,
            master_key: KeyInfo { included, fingerprint },
            files,
            engine: Some(engine),
            database_only: opts.database_only,
            note: opts.note,
        };
        let json = serde_json::to_vec_pretty(&manifest).map_err(|e| BackupError::Internal(e.to_string()))?;
        fsutil::write_atomic(&partial.join(MANIFEST), &json)?;
        std::fs::rename(&partial, dest_of(&partial)).map_err(io(&partial))?;
        if let Some(parent) = partial.parent() {
            fsutil::sync_dir(parent);
        }
        Ok(manifest)
    })
    .await?;
    tracing::info!(id = %manifest.id, files = manifest.files.len(), bytes = manifest.total_bytes(), "backup written");
    Ok(manifest)
}

/// Copies the database into `partial` with the engine's own method and
/// returns its manifest entries and the engine description.
async fn copy_database(
    db: Option<&Database>,
    kind: BackendKind,
    layout: &DataLayout,
    partial: &Path,
    opts: &BackupOptions,
    id: &str,
    member: bool,
) -> Result<(Vec<FileEntry>, EngineInfo)> {
    if kind == BackendKind::Sqlite {
        let db = db.ok_or_else(|| BackupError::Internal("a SQLite backup needs a database handle".into()))?;
        let db_copy = partial.join(DB_FILE);
        db.snapshot_to(&db_copy).await?;
        let files = spawn(move || -> Result<Vec<FileEntry>> {
            make_private(&db_copy)?;
            let (sha, size) = sha256_file(&db_copy)?;
            Ok(vec![FileEntry {
                path: DB_FILE.to_owned(),
                size,
                sha256: sha,
                kind: FileKind::Database,
            }])
        })
        .await?;
        return Ok((files, EngineInfo::sqlite()));
    }

    let src = opts.pg.clone().ok_or_else(|| {
        BackupError::Invalid(
            "this database is PostgreSQL: the backup needs the engine's tools (PgSource); \
             use `forgebase backup`, or the server's snapshot API"
                .into(),
        )
    })?;
    let tmp = layout.tmp_dir();
    let engine = EngineInfo {
        kind: "postgres".to_owned(),
        mode: Some(src.mode().to_owned()),
        version: Some(src.version.clone()),
        major: src.major(),
        method: if src.is_managed() {
            Method::Basebackup
        } else {
            Method::Dump
        },
    };
    match &src.endpoint {
        PgEndpoint::Managed { cluster_dir, .. } => {
            if !member {
                pg::ensure_single_set(layout.root())?;
            }
            let dest = partial.join(PG_DIR);
            src.basebackup(&dest, &tmp, &format!("forgebase {id}")).await?;
            let (dest2, secret_src, partial2) = (dest, cluster_dir.join(ROLES_FILE), partial.to_path_buf());
            let files = spawn(move || -> Result<Vec<FileEntry>> {
                let mut out = Vec::new();
                for rel in walk_files(&dest2, &[])? {
                    let (sha, size) = sha256_file(&join_rel(&dest2, &rel))?;
                    out.push(FileEntry {
                        path: format!("{PG_DIR}/{rel}"),
                        size,
                        sha256: sha,
                        kind: FileKind::Postgres,
                    });
                }
                if secret_src.is_file() {
                    let (sha, size) = copy_hashed(&secret_src, &partial2.join(ROLES_FILE))?;
                    out.push(FileEntry {
                        path: ROLES_FILE.to_owned(),
                        size,
                        sha256: sha,
                        kind: FileKind::Postgres,
                    });
                }
                Ok(out)
            })
            .await?;
            Ok((files, engine))
        }
        PgEndpoint::External { .. } => {
            let file = partial.join(DUMP_FILE);
            src.dump(
                &file,
                &tmp,
                &DumpPlan {
                    forgebase_schema_only: true,
                    ..DumpPlan::default()
                },
            )
            .await?;
            let files = spawn(move || -> Result<Vec<FileEntry>> {
                make_private(&file)?;
                let (sha, size) = sha256_file(&file)?;
                Ok(vec![FileEntry {
                    path: DUMP_FILE.to_owned(),
                    size,
                    sha256: sha,
                    kind: FileKind::Dump,
                }])
            })
            .await?;
            Ok((files, engine))
        }
    }
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
