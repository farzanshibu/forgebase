//! Backup verification: manifest, every file's size and SHA-256, no extra
//! files, and the copied database opened read-only (`integrity_check`,
//! `foreign_key_check`, migration state).

use std::collections::HashSet;
use std::path::Path;

use forgebase_db::{Database, DbOptions};
use serde::Serialize;

use crate::backup::spawn;
use crate::error::BackupError;
use crate::fsutil::{join_rel, safe_rel, sha256_file, walk_files};
use crate::manifest::{DB_FILE, FileKind, MANIFEST, Manifest};

/// Outcome of [`verify`].
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct VerifyReport {
    /// No problems found.
    pub ok: bool,
    /// The manifest, when it could be read.
    pub manifest: Option<Manifest>,
    /// Files whose size and hash were checked.
    pub files_checked: usize,
    /// Bytes hashed.
    pub bytes_checked: u64,
    /// Reasons the backup cannot be restored.
    pub problems: Vec<String>,
    /// Things worth knowing that do not block a restore.
    pub warnings: Vec<String>,
}

/// Verifies the backup directory `dir`. Never fails: problems are reported.
pub async fn verify(dir: &Path) -> VerifyReport {
    let mut report = VerifyReport {
        ok: false,
        manifest: None,
        files_checked: 0,
        bytes_checked: 0,
        problems: Vec::new(),
        warnings: Vec::new(),
    };
    if !dir.is_dir() {
        report.problems.push(format!("{} is not a directory", dir.display()));
        return report;
    }
    let manifest = match Manifest::read(dir) {
        Ok(m) => m,
        Err(BackupError::Verify(p)) => {
            report.problems.extend(p);
            return report;
        }
        Err(e) => {
            report.problems.push(e.to_string());
            return report;
        }
    };

    let (d, m) = (dir.to_path_buf(), manifest.clone());
    match spawn(move || Ok(check_files(&d, &m))).await {
        Ok((problems, files, bytes)) => {
            report.problems.extend(problems);
            report.files_checked = files;
            report.bytes_checked = bytes;
        }
        Err(e) => report.problems.push(e.to_string()),
    }

    if manifest.files.iter().filter(|f| f.kind == FileKind::Database).count() != 1
        || !manifest.files.iter().any(|f| f.path == DB_FILE)
    {
        report
            .problems
            .push(format!("manifest does not list exactly one {DB_FILE}"));
    } else if report.problems.is_empty() {
        check_database(dir, &manifest, &mut report).await;
    }
    if !manifest.includes_indexes {
        report
            .warnings
            .push("indexes are not part of this backup; they are rebuilt after restore".into());
    }
    if !manifest.master_key.included {
        report.warnings.push(
            "the master key is not part of this backup; restore needs the original key (--master-key) or an existing one"
                .into(),
        );
    }
    report.ok = report.problems.is_empty();
    report.manifest = Some(manifest);
    report
}

fn check_files(dir: &Path, m: &Manifest) -> (Vec<String>, usize, u64) {
    let mut problems = Vec::new();
    let mut listed: HashSet<&str> = HashSet::new();
    let (mut files, mut bytes) = (0usize, 0u64);
    for f in &m.files {
        if !safe_rel(&f.path) {
            problems.push(format!("unsafe path in manifest: {:?}", f.path));
            continue;
        }
        if !listed.insert(f.path.as_str()) {
            problems.push(format!("{} is listed twice", f.path));
            continue;
        }
        let path = join_rel(dir, &f.path);
        match sha256_file(&path) {
            Err(BackupError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
                problems.push(format!("{} is missing", f.path));
            }
            Err(e) => problems.push(format!("{}: {e}", f.path)),
            Ok((sha, size)) => {
                files += 1;
                bytes += size;
                if size != f.size {
                    problems.push(format!("{} has {size} bytes, manifest says {}", f.path, f.size));
                } else if sha != f.sha256 {
                    problems.push(format!("{} is corrupt (SHA-256 differs from the manifest)", f.path));
                }
            }
        }
    }
    match walk_files(dir, &[]) {
        Ok(found) => {
            for rel in found {
                if rel != MANIFEST && !listed.contains(rel.as_str()) {
                    problems.push(format!("{rel} is not listed in the manifest"));
                }
            }
        }
        Err(e) => problems.push(e.to_string()),
    }
    (problems, files, bytes)
}

async fn check_database(dir: &Path, m: &Manifest, report: &mut VerifyReport) {
    let cfg = forgebase_core::Config::defaults(dir);
    let opts = DbOptions::from_config(&cfg.database, dir.join(DB_FILE)).read_only();
    let db = match Database::open(opts).await {
        Ok(db) => db,
        Err(e) => {
            report
                .problems
                .push(format!("the copied database cannot be opened: {e}"));
            return;
        }
    };
    match db.integrity_check().await {
        Ok(p) if p.is_empty() => {}
        Ok(p) => report.problems.push(format!(
            "integrity_check: {}",
            p.into_iter().take(5).collect::<Vec<_>>().join("; ")
        )),
        Err(e) => report.problems.push(format!("integrity_check failed: {e}")),
    }
    match db.foreign_key_violations().await {
        Ok(0) => {}
        Ok(n) => report
            .problems
            .push(format!("{n} foreign-key violation(s) in the copied database")),
        Err(e) => report.problems.push(format!("foreign_key_check failed: {e}")),
    }
    match db.migration_status().await {
        Err(e) => report.problems.push(format!("migration state unreadable: {e}")),
        Ok(s) if !s.problems.is_empty() => report.problems.push(format!("migration problems: {:?}", s.problems)),
        Ok(s) => {
            if s.current_version != m.schema_version {
                report.problems.push(format!(
                    "database is at schema v{} but the manifest says v{}",
                    s.current_version, m.schema_version
                ));
            }
            if !s.pending.is_empty() {
                report.warnings.push(format!(
                    "schema v{} is older than this build (v{}); `serve` or `migrate` upgrades it after restore",
                    s.current_version, s.latest_version
                ));
            }
        }
    }
    db.close().await;
}
