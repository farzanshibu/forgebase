//! Verified restore of a backup into a data directory.
//!
//! The server must be stopped (the caller holds the data-directory lock).
//! Order of work, chosen so a crash at any point can be undone:
//!
//! 1. an interrupted earlier restore is rolled back ([`recover`]);
//! 2. the backup is verified (manifest, hashes, database integrity);
//! 3. the master key is resolved and compared with the backup's fingerprint;
//! 4. everything is copied into `tmp/restore-*` and hashed again;
//! 5. a journal is written, the current data is moved to
//!    `backups/pre-restore-*` (never deleted), the staged data is moved in
//!    (database last), and the journal is marked committed and removed.
//!
//! A crash before the commit mark leaves the journal; the next restore (or
//! `forgebase restore --recover`) moves the previous data back.

use std::path::{Path, PathBuf};

use forgebase_core::DataLayout;
use serde::{Deserialize, Serialize};

use crate::backup::{key_fingerprint, master_key_path, spawn};
use crate::error::{BackupError, Result, io};
use crate::fsutil::{self, copy_hashed, join_rel, mkdir_private};
use crate::manifest::{DB_FILE, DUMP_FILE, FileKind, KEY_FILE, Manifest, Method, PG_DIR, ROLES_FILE};
use crate::pg::{self, PgSource};
use crate::verify::{VerifyOptions, verify_with};

const JOURNAL: &str = ".restore-journal.json";
/// Items of the data directory a SQLite restore replaces, in the order they
/// are moved aside. The database is moved in last.
const ITEMS: &[&str] = &[
    "forgebase.db-wal",
    "forgebase.db-shm",
    "forgebase.db",
    "storage",
    "indexes",
];
/// The database items of a restore of a physical PostgreSQL backup.
const PG_ITEMS: &[&str] = &["postgres/roles.secret", "postgres/data"];
/// `PGDATA` and the login passwords inside a data directory.
const PG_DATA: &str = "postgres/data";

/// Restore settings.
#[derive(Debug, Clone, Default)]
pub struct RestoreOptions {
    /// Replace an existing database (the old data is kept in `backups/pre-restore-*`).
    pub force: bool,
    /// Master key file to install when the backup does not hold one.
    pub master_key: Option<PathBuf>,
    /// Go on without a master key (secrets sealed with it stay unreadable).
    pub allow_missing_key: bool,
    /// Do not restore index files; they are rebuilt at startup.
    pub skip_indexes: bool,
    /// The engine's tools and, for an external server, the way in. Needed to
    /// check a PostgreSQL backup with `pg_verifybackup`, to refuse a
    /// different major version, and to load a logical backup.
    pub pg: Option<PgSource>,
    /// Test hook: stop with an error (no rollback, as after a crash) after
    /// this many items were moved or installed.
    #[doc(hidden)]
    pub crash_after: Option<usize>,
}

/// Result of a restore.
#[derive(Debug, Clone, Serialize)]
pub struct RestoreReport {
    /// Backup id.
    pub id: String,
    /// Schema version restored.
    pub schema_version: i64,
    /// Files restored.
    pub files: usize,
    /// Bytes restored.
    pub bytes: u64,
    /// Where the replaced data went (`None` for an empty target).
    pub previous_data: Option<PathBuf>,
    /// What happened to the master key.
    pub master_key: String,
    /// The engine of the restored database (`sqlite`, `postgres 18.3 (managed)`).
    pub engine: String,
    /// Non-blocking findings of the verification.
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Journal {
    aside: PathBuf,
    staging: PathBuf,
    items: Vec<String>,
    /// Items whose staged copy is being (or was) moved in. Written before each move.
    #[serde(default)]
    installed: Vec<String>,
    committed: bool,
}

pub(crate) fn journal_path(layout: &DataLayout) -> PathBuf {
    layout.root().join(JOURNAL)
}

pub(crate) fn remove_any(p: &Path) -> Result<()> {
    match std::fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(p).map_err(io(p)),
        Ok(_) => std::fs::remove_file(p).map_err(io(p)),
        Err(_) => Ok(()),
    }
}

pub(crate) fn move_to(from: &Path, to: &Path) -> Result<()> {
    if let Some(parent) = to.parent() {
        mkdir_private(parent).map_err(io(parent))?;
    }
    std::fs::rename(from, to).map_err(io(from))
}

/// Whether an interrupted restore left a journal in the data directory.
pub fn interrupted(layout: &DataLayout) -> bool {
    journal_path(layout).is_file()
}

/// Rolls back an interrupted restore. Returns whether there was one.
pub fn recover(layout: &DataLayout) -> Result<bool> {
    let jp = journal_path(layout);
    let Ok(text) = std::fs::read_to_string(&jp) else {
        return Ok(false);
    };
    let j: Journal = serde_json::from_str(&text).map_err(|e| {
        BackupError::Conflict(format!(
            "{} is unreadable ({e}); inspect the data directory by hand",
            jp.display()
        ))
    })?;
    if !j.committed {
        // 1. Remove what the restore moved in (its staged copy is gone from staging).
        for item in &j.installed {
            if !join_rel(&j.staging, item).exists() {
                remove_any(&join_rel(layout.root(), item))?;
            }
        }
        // 2. Put the previous data back: whatever sits in the aside directory was moved by the restore.
        for item in &j.items {
            let old = join_rel(&j.aside, item);
            if old.exists() {
                let cur = join_rel(layout.root(), item);
                remove_any(&cur)?;
                move_to(&old, &cur)?;
            }
        }
        tracing::warn!(aside = %j.aside.display(), "rolled back an interrupted restore");
    }
    remove_any(&j.staging)?;
    std::fs::remove_file(&jp).map_err(io(&jp))?;
    let _ = layout.ensure();
    Ok(!j.committed)
}

pub(crate) enum KeySource {
    Backup,
    Provided(PathBuf),
    Existing,
    None,
}

/// Restores `backup` into `layout`.
pub async fn restore(backup: &Path, layout: &DataLayout, opts: RestoreOptions) -> Result<RestoreReport> {
    layout.ensure().map_err(io(layout.root()))?;
    recover(layout)?;

    let report = verify_with(
        backup,
        &VerifyOptions {
            pg: opts.pg.clone(),
            tmp: Some(layout.tmp_dir()),
        },
    )
    .await;
    if !report.ok {
        return Err(BackupError::Verify(report.problems));
    }
    let Some(manifest) = report.manifest else {
        return Err(BackupError::Internal("verified backup has no manifest".into()));
    };
    let engine = manifest.engine_info();
    let physical = manifest.is_physical_postgres();
    let logical = engine.is_postgres() && engine.method == Method::Dump;
    let mut warnings = report.warnings;
    let existing = if logical {
        true
    } else if physical {
        pg_cluster_exists(layout)
    } else {
        layout.database_path().exists()
    };
    check_target(layout, &manifest, &opts, &mut warnings)?;
    if existing && !opts.force {
        return Err(BackupError::Conflict(if logical {
            "restoring a logical backup replaces the objects of the forgebase schema in the external database; \
             use --force to go on (take a fresh backup of it first)"
                .to_owned()
        } else {
            format!(
                "{} already holds a database; use --force to replace it (the current data is kept in backups/pre-restore-*)",
                layout.root().display()
            )
        }));
    }

    // A database-only backup (pre-migration snapshot) leaves files and keys alone.
    let key_path = master_key_path(layout);
    let source = if manifest.database_only {
        KeySource::Existing
    } else if manifest.master_key.included {
        KeySource::Backup
    } else if let Some(p) = &opts.master_key {
        KeySource::Provided(p.clone())
    } else if key_path.is_file() {
        KeySource::Existing
    } else {
        KeySource::None
    };
    let key_note = if manifest.database_only {
        "unchanged (database-only backup)".to_owned()
    } else {
        resolve_key(&manifest, &source, &key_path, opts.allow_missing_key)?
    };

    let stamp = forgebase_core::now_rfc3339().replace([':', '-'], "");
    let staging = layout.tmp_dir().join(format!("restore-{stamp}-{}", std::process::id()));
    let aside = layout.backups_dir().join(format!("pre-restore-{stamp}"));
    let (backup_dir, m, skip_indexes, st) = (
        backup.to_path_buf(),
        manifest.clone(),
        opts.skip_indexes,
        staging.clone(),
    );
    let provided = if let KeySource::Provided(p) = &source {
        Some(p.clone())
    } else {
        None
    };
    let (files, bytes) = spawn(move || stage(&backup_dir, &m, &st, skip_indexes, provided.as_deref())).await?;

    // A logical backup is loaded first: one transaction, so a failure changes nothing.
    if logical {
        let src = opts.pg.as_ref().ok_or_else(|| {
            BackupError::Invalid("restoring a logical backup needs the external server's connection (PgSource)".into())
        })?;
        src.restore_dump(&staging.join(DUMP_FILE), &layout.tmp_dir()).await?;
    }

    let items = items_of(&manifest, matches!(source, KeySource::Backup | KeySource::Provided(_)));
    let (lay, st, ad, crash_after) = (layout.clone(), staging.clone(), aside.clone(), opts.crash_after);
    let swapped = spawn(move || swap(&lay, &st, &ad, items, crash_after)).await;
    match swapped {
        Ok(moved_any) => {
            if !engine.is_postgres() {
                enable_wal(layout).await?;
            }
            tracing::info!(id = %manifest.id, files, "restore complete");
            Ok(RestoreReport {
                id: manifest.id,
                schema_version: manifest.schema_version,
                files,
                bytes,
                previous_data: moved_any.then_some(aside),
                master_key: key_note,
                engine: engine.describe(),
                warnings,
            })
        }
        Err(e) => {
            if crash_after.is_none() {
                let _ = recover(layout);
            }
            Err(e)
        }
    }
}

/// Whether the data directory holds a bundled PostgreSQL cluster.
fn pg_cluster_exists(layout: &DataLayout) -> bool {
    layout.root().join(PG_DATA).join("PG_VERSION").is_file()
}

/// Refuses a backup the target cannot take: another engine, another
/// PostgreSQL major, a running cluster, more than one data set.
fn check_target(layout: &DataLayout, m: &Manifest, opts: &RestoreOptions, warnings: &mut Vec<String>) -> Result<()> {
    let engine = m.engine_info();
    let sqlite_here = layout.database_path().exists();
    let pg_here = pg_cluster_exists(layout);
    if engine.is_postgres() {
        if sqlite_here && !m.database_only {
            return Err(BackupError::Conflict(format!(
                "{} holds a SQLite database but the backup is {}: a restore does not change the backend; \
                 move the project with `forgebase db convert` (FB-151) or restore into an empty data directory",
                layout.root().display(),
                engine.describe()
            )));
        }
    } else if pg_here {
        return Err(BackupError::Conflict(format!(
            "{} holds a PostgreSQL cluster but the backup is SQLite: a restore does not change the backend; \
             move the project with `forgebase db convert` (FB-151) or restore into an empty data directory",
            layout.root().display()
        )));
    }
    if m.is_physical_postgres() {
        pg::ensure_single_set(layout.root())?;
        if let Some(pid) = pg::running_postmaster(&layout.root().join(PG_DATA)) {
            return Err(BackupError::Conflict(format!(
                "a PostgreSQL server (pid {pid}) is still running on {}; stop ForgeBase and wait for it to exit",
                layout.root().join(PG_DATA).display()
            )));
        }
        match (engine.major, opts.pg.as_ref().and_then(PgSource::major)) {
            (Some(backup), Some(here)) if backup != here => {
                return Err(BackupError::Invalid(format!(
                    "the backup is {}, this engine is PostgreSQL {here}: a physical backup restores on the same major \
                     only; restore it with an engine of major {backup}, or move the data with a logical export \
                     (`forgebase export` and `forgebase import`)",
                    engine.describe()
                )));
            }
            (_, None) => warnings.push(
                "the engine's major version is unknown here; the cluster is checked when the server starts".into(),
            ),
            _ => {}
        }
    }
    Ok(())
}

/// The data-directory items a restore of `m` replaces.
fn items_of(m: &Manifest, replace_key: bool) -> Vec<String> {
    let mut items: Vec<String> = if m.is_physical_postgres() {
        let mut v: Vec<String> = vec!["storage".into(), "indexes".into()];
        v.extend(PG_ITEMS.iter().map(|s| (*s).to_owned()));
        v
    } else if m.engine_info().is_postgres() {
        vec!["storage".into(), "indexes".into()]
    } else {
        ITEMS.iter().map(|s| (*s).to_owned()).collect()
    };
    if m.database_only {
        items.retain(|i| i != "storage" && i != "indexes");
    }
    if replace_key {
        items.push(KEY_FILE.to_owned());
    }
    items
}

/// Where a manifest path lands in the data directory.
fn target_path(m: &Manifest, rel: &str) -> String {
    if m.is_physical_postgres() {
        if let Some(rest) = rel.strip_prefix(&format!("{PG_DIR}/")) {
            return format!("{PG_DATA}/{rest}");
        }
        if rel == ROLES_FILE {
            return format!("postgres/{ROLES_FILE}");
        }
    }
    rel.to_owned()
}

pub(crate) fn resolve_key(m: &Manifest, source: &KeySource, existing: &Path, allow_missing: bool) -> Result<String> {
    let (path, label): (Option<PathBuf>, &str) = match source {
        KeySource::Backup => (None, "restored from the backup"),
        KeySource::Provided(p) => (Some(p.clone()), "installed from --master-key"),
        KeySource::Existing => (
            Some(existing.to_path_buf()),
            "kept the key already in the data directory",
        ),
        KeySource::None => (None, "missing"),
    };
    if let (Some(p), Some(want)) = (&path, &m.master_key.fingerprint) {
        match key_fingerprint(p)? {
            Some(got) if &got != want => {
                return Err(BackupError::Invalid(format!(
                    "master key {} does not match the backup (fingerprint {got}, backup expects {want}); secrets sealed with the original key would be unreadable",
                    p.display()
                )));
            }
            None => {
                return Err(BackupError::Invalid(format!(
                    "master key {} does not exist",
                    p.display()
                )));
            }
            _ => {}
        }
    }
    if matches!(source, KeySource::None) && m.master_key.fingerprint.is_some() && !allow_missing {
        return Err(BackupError::Invalid(
            "the backup does not contain the master key and none is available: pass --master-key FILE (the original auth/master.key), or --allow-missing-key to start with a new key and lose sealed secrets".into(),
        ));
    }
    Ok(label.to_owned())
}

fn stage(
    backup: &Path,
    m: &Manifest,
    staging: &Path,
    skip_indexes: bool,
    provided_key: Option<&Path>,
) -> Result<(usize, u64)> {
    let _ = std::fs::remove_dir_all(staging);
    mkdir_private(staging).map_err(io(staging))?;
    let (mut files, mut bytes) = (0usize, 0u64);
    for f in &m.files {
        if f.kind == FileKind::Index && skip_indexes {
            continue;
        }
        let (sha, size) = copy_hashed(&join_rel(backup, &f.path), &join_rel(staging, &target_path(m, &f.path)))?;
        if sha != f.sha256 || size != f.size {
            return Err(BackupError::Verify(vec![format!(
                "{} changed while it was being restored",
                f.path
            )]));
        }
        files += 1;
        bytes += size;
    }
    if let Some(p) = provided_key {
        copy_hashed(p, &join_rel(staging, KEY_FILE))?;
    }
    if m.is_physical_postgres() {
        // PostgreSQL needs its (often empty) directories: pg_wal/archive_status, pg_tblspc, ...
        copy_dirs(&backup.join(PG_DIR), &join_rel(staging, PG_DATA))?;
    }
    Ok((files, bytes))
}

/// Creates every directory of `src` below `dst`.
fn copy_dirs(src: &Path, dst: &Path) -> Result<()> {
    mkdir_private(dst).map_err(io(dst))?;
    for entry in std::fs::read_dir(src).map_err(io(src))? {
        let entry = entry.map_err(io(src))?;
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            copy_dirs(&entry.path(), &dst.join(entry.file_name()))?;
        }
    }
    Ok(())
}

/// Moves the current data aside and the staged data in. Returns whether any
/// current data was moved aside.
pub(crate) fn swap(
    layout: &DataLayout,
    staging: &Path,
    aside: &Path,
    items: Vec<String>,
    crash_after: Option<usize>,
) -> Result<bool> {
    let root = layout.root();
    mkdir_private(aside).map_err(io(aside))?;
    let mut journal = Journal {
        aside: aside.to_path_buf(),
        staging: staging.to_path_buf(),
        items: items.clone(),
        installed: Vec::new(),
        committed: false,
    };
    let jp = journal_path(layout);
    let write_journal = |j: &Journal| -> Result<()> {
        let bytes = serde_json::to_vec(j).map_err(|e| BackupError::Internal(e.to_string()))?;
        fsutil::write_atomic(&jp, &bytes)
    };
    write_journal(&journal)?;

    let mut steps = 0usize;
    let crash = |steps: usize| match crash_after {
        Some(n) if steps >= n => Err(BackupError::Internal("simulated crash during restore".into())),
        _ => Ok(()),
    };
    let mut moved_any = false;
    for item in &items {
        let cur = join_rel(root, item);
        if cur.is_dir() && std::fs::read_dir(&cur).map_err(io(&cur))?.next().is_none() {
            // An empty directory (fresh layout) holds no data worth keeping.
            std::fs::remove_dir(&cur).map_err(io(&cur))?;
        } else if cur.exists() {
            move_to(&cur, &join_rel(aside, item))?;
            moved_any = true;
            steps += 1;
            crash(steps)?;
        }
    }
    // Staged items: database last, so its presence marks a complete restore.
    let is_db = |i: &&String| {
        i.as_str() == DB_FILE || i.as_str() == PG_DATA || (i.starts_with("postgres/nodes/") && i.ends_with("/data"))
    };
    let order = items.iter().filter(|i| !is_db(i)).chain(items.iter().filter(is_db));
    for item in order {
        let from = join_rel(staging, item);
        if from.exists() {
            let to = join_rel(root, item);
            journal.installed.push(item.clone());
            write_journal(&journal)?;
            remove_any(&to)?;
            move_to(&from, &to)?;
            steps += 1;
            crash(steps)?;
        }
    }
    layout.ensure().map_err(io(root))?;
    journal.committed = true;
    write_journal(&journal)?;
    remove_any(staging)?;
    std::fs::remove_file(&jp).map_err(io(&jp))?;
    fsutil::sync_dir(root);
    if !moved_any {
        let _ = std::fs::remove_dir(aside);
    }
    Ok(moved_any)
}

/// `VACUUM INTO` writes a rollback-journal database. Opening it once
/// read-write switches it to WAL (a persistent file property), so the
/// restored directory looks exactly like one the server wrote.
async fn enable_wal(layout: &DataLayout) -> Result<()> {
    let cfg = forgebase_core::Config::defaults(layout.root());
    let opts = forgebase_db::DbOptions::from_config(&cfg.database, layout.database_path()).create_if_missing(false);
    let db = forgebase_db::Database::open(opts).await?;
    db.checkpoint().await?;
    db.close().await;
    Ok(())
}
