//! Backups and restores of a cluster with replicas and shards (FB-179,
//! ADR-0029 §2.9).
//!
//! A cluster backup is **one directory with one backup per set**:
//!
//! ```text
//! <dest>/
//! ├── CLUSTER.json        the combined manifest (topology epoch, placements, one entry per set)
//! ├── control/            a normal backup of the control set (database, storage, indexes, key)
//! │   └── MANIFEST.json
//! └── sets/<number>/      database-only backup of each shard set
//!     └── MANIFEST.json
//! ```
//!
//! * **Source.** `[backup] source = "replica"` copies a set from a healthy
//!   replica (`pg_basebackup` as the replicator over the standby's socket),
//!   so the backup does not load the primary; a set without one is copied
//!   from its primary and the manifest says why. Every entry records the
//!   node, the timeline and the end LSN.
//! * **Restore** puts every set's data back, offline, in one journaled swap
//!   (a crash rolls all of it back), and moves every other cluster of the
//!   data directory aside: the replicas and old primaries follow a history
//!   that no longer exists. It writes `restore-pending.json`.
//! * **After the restore**, with the restored control database running,
//!   [`complete_restore`] marks every replica `needs_rebuild`, migrates every
//!   set to the schema head (the schema reconcile), rewrites `topology.json`
//!   and rebuilds the managed replicas through a [`ReplicaRebuilder`].
//!
//! This crate does not depend on the engine: the caller describes each node
//! in a [`NodeSource`] and supplies the rebuilder.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use forgebase_core::{BackupSource, DataLayout};
use forgebase_db::migrations::multi::{MultiOptions, NodeOutcome};
use forgebase_db::topology::{
    self, DbNode, DbSet, NodeKind, NodeRole, NodeState, Placement, SetKind, TOPOLOGY_FILE, Topology,
};
use forgebase_db::{BackendKind, Database, DbRouter};
use serde::{Deserialize, Serialize};

use crate::backup::{self, BackupOptions, spawn};
use crate::error::{BackupError, Result, io};
use crate::fsutil::{self, copy_hashed, copy_hashed_unsynced, join_rel, mkdir_private, safe_rel, sha256_file};
use crate::manifest::{FileKind, KEY_FILE, MANIFEST, Manifest, Method, PG_DIR, ROLES_FILE};
use crate::pg::{self, PgSource};
use crate::restore::{self, KeySource, RestoreOptions};
use crate::verify::{VerifyOptions, VerifyReport, verify_with};

/// The combined manifest's file name.
pub const CLUSTER_MANIFEST: &str = "CLUSTER.json";
/// Format version of [`ClusterManifest`].
pub const CLUSTER_FORMAT: u32 = 1;
/// Directory of the control set's backup.
pub const CONTROL_DIR: &str = "control";
/// Directory holding the shard sets' backups (`sets/<number>`).
pub const SETS_DIR: &str = "sets";
/// What a restore leaves for [`complete_restore`] in the data directory.
pub const RESTORE_PENDING: &str = "restore-pending.json";
/// Where the control primary lives unless a replica was promoted.
pub const PRIMARY_DATA: &str = "postgres/data";

// ---------------------------------------------------------------------------
// What the caller describes.
// ---------------------------------------------------------------------------

/// One PostgreSQL server of a set the backup can read from.
#[derive(Debug, Clone)]
pub struct NodeSource {
    /// Node name (`postgres/nodes/<name>` for local nodes).
    pub name: String,
    /// How to reach it. A replica is reached as the replicator over its own
    /// socket; the cluster directory is the one holding `roles.secret`.
    pub pg: PgSource,
    /// Where its data directory lives in the data directory, relative:
    /// `postgres/data` or `postgres/nodes/<name>/data`. Only a primary's
    /// matters (it is where a restore puts the set back).
    pub data_dir: Option<String>,
}

/// One set of the cluster: its row, a database handle (the schema version
/// is read from it) and the servers to copy from.
#[derive(Debug, Clone)]
pub struct SetSource {
    /// The set row.
    pub set: DbSet,
    /// An open database on the set's primary (read for the schema version).
    pub db: Database,
    /// The set's primary.
    pub primary: NodeSource,
    /// Healthy replicas, best first. The caller filters: a lagging, needs
    /// rebuild or unreachable one is not offered.
    pub replicas: Vec<NodeSource>,
}

/// What [`create_cluster`] needs.
#[derive(Debug, Clone)]
pub struct ClusterBackupOptions {
    /// Indexes, master key, label (`pg` and `database_only` are set per set).
    pub base: BackupOptions,
    /// `[backup] source`.
    pub source: BackupSource,
    /// One entry per set the topology holds (not retired).
    pub sets: Vec<SetSource>,
}

// ---------------------------------------------------------------------------
// The combined manifest.
// ---------------------------------------------------------------------------

/// Where tenants are placed, at the time of the backup.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlacementSummary {
    /// Number of placement rows.
    pub total: u64,
    /// Highest placement epoch.
    pub max_epoch: u64,
    /// Placement rows per set id.
    pub per_set: BTreeMap<String, u64>,
}

/// One set's entry of the combined manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetEntry {
    /// Set id.
    pub id: String,
    /// Set name.
    pub name: String,
    /// Set number.
    pub number: u16,
    /// `control` or `shard`.
    pub kind: SetKind,
    /// The set's backup directory, relative to the cluster backup.
    pub dir: String,
    /// How the set was copied.
    pub method: Method,
    /// Schema version of the copied database.
    pub schema_version: i64,
    /// The node the data was copied from.
    pub node: String,
    /// Whether that node was the set's primary or a replica.
    pub role: NodeRole,
    /// Where the set's primary keeps its data directory (where a restore
    /// puts the set back).
    pub data_dir: Option<String>,
    /// Timeline of the copy (physical backups).
    pub timeline: Option<u32>,
    /// WAL position the copy starts at.
    pub start_lsn: Option<String>,
    /// WAL position the copy ends at: the backup is consistent here.
    pub end_lsn: Option<String>,
    /// Why a replica was asked for and the primary used.
    pub fallback: Option<String>,
    /// SHA-256 of the set's `MANIFEST.json`.
    pub manifest_sha256: String,
    /// Files in the set's backup.
    pub files: usize,
    /// Bytes in the set's backup.
    pub bytes: u64,
}

/// `CLUSTER.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClusterManifest {
    /// [`CLUSTER_FORMAT`].
    pub format: u32,
    /// Backup id (the directory name).
    pub id: String,
    /// RFC 3339 creation time (UTC).
    pub created: String,
    /// ForgeBase version that wrote it.
    pub forgebase_version: String,
    /// Free-form label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// What `[backup] source` asked for.
    pub source: BackupSource,
    /// The placement epoch: the topology epoch when the backup started.
    pub topology_epoch: u64,
    /// Placements when the backup started.
    pub placements: PlacementSummary,
    /// Sets and nodes as they were (a restore rewrites `topology.json` from it).
    pub topology: Topology,
    /// One entry per set, control first.
    pub sets: Vec<SetEntry>,
    /// Things worth knowing: a tenant move in flight, a topology change
    /// during the backup, a replica that was not used.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

impl ClusterManifest {
    /// Reads `<dir>/CLUSTER.json`.
    ///
    /// # Errors
    /// [`BackupError::Verify`] when it is missing, unreadable or of a
    /// format this build does not know.
    pub fn read(dir: &Path) -> Result<Self> {
        let path = dir.join(CLUSTER_MANIFEST);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(BackupError::Verify(vec![format!(
                    "{CLUSTER_MANIFEST} is missing: the backup is unfinished or not a cluster backup"
                )]));
            }
            Err(e) => return Err(io(&path)(e)),
        };
        let m: Self = serde_json::from_str(&text)
            .map_err(|e| BackupError::Verify(vec![format!("{CLUSTER_MANIFEST} is invalid: {e}")]))?;
        if m.format != CLUSTER_FORMAT {
            return Err(BackupError::Verify(vec![format!(
                "unsupported cluster manifest format {} (this build reads format {CLUSTER_FORMAT})",
                m.format
            )]));
        }
        Ok(m)
    }

    /// Total bytes of every set.
    pub fn total_bytes(&self) -> u64 {
        self.sets.iter().map(|s| s.bytes).sum()
    }

    /// Total files of every set.
    pub fn total_files(&self) -> usize {
        self.sets.iter().map(|s| s.files).sum()
    }

    /// The control set's entry.
    pub fn control(&self) -> Option<&SetEntry> {
        self.sets.iter().find(|s| s.kind == SetKind::Control)
    }
}

/// Whether `dir` is a cluster backup (it holds `CLUSTER.json`).
pub fn is_cluster_backup(dir: &Path) -> bool {
    dir.join(CLUSTER_MANIFEST).is_file()
}

// ---------------------------------------------------------------------------
// Backup.
// ---------------------------------------------------------------------------

/// A valid data directory path of a PostgreSQL cluster inside the data
/// directory: `postgres/data` or `postgres/nodes/<name>/data`.
fn valid_data_dir(rel: &str) -> bool {
    if rel == PRIMARY_DATA {
        return true;
    }
    let Some(rest) = rel.strip_prefix("postgres/nodes/") else {
        return false;
    };
    let Some(name) = rest.strip_suffix("/data") else {
        return false;
    };
    safe_rel(rel)
        && (1..=64).contains(&name.len())
        && !name.contains('/')
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_' || b == b'~')
}

fn member_dir(set: &DbSet) -> String {
    if set.is_control() {
        CONTROL_DIR.to_owned()
    } else {
        format!("{SETS_DIR}/{}", set.number)
    }
}

/// Timeline and LSN range of a physical backup, from PostgreSQL's own
/// `backup_manifest` (`WAL-Ranges`): the last range is the one the backup
/// ends in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalRange {
    /// Timeline id.
    pub timeline: u32,
    /// First WAL position needed.
    pub start_lsn: String,
    /// Last WAL position needed: consistent once replayed to here.
    pub end_lsn: String,
}

/// Reads the WAL range of a `pg_basebackup` directory.
pub fn wal_range(pgdir: &Path) -> Option<WalRange> {
    let text = std::fs::read_to_string(pgdir.join("backup_manifest")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let last = v.get("WAL-Ranges")?.as_array()?.last()?;
    Some(WalRange {
        timeline: u32::try_from(last.get("Timeline")?.as_u64()?).ok()?,
        start_lsn: last.get("Start-LSN")?.as_str()?.to_owned(),
        end_lsn: last.get("End-LSN")?.as_str()?.to_owned(),
    })
}

async fn placement_summary(control: &Database) -> Result<(PlacementSummary, Vec<Placement>)> {
    let placements = topology::load_placements(control).await?;
    let mut s = PlacementSummary {
        total: placements.len() as u64,
        ..PlacementSummary::default()
    };
    for p in &placements {
        s.max_epoch = s.max_epoch.max(p.epoch);
        *s.per_set.entry(p.set_id.clone()).or_default() += 1;
    }
    Ok((s, placements))
}

/// Tenant moves not finished (`_forgebase_db_moves`): their rows are split
/// between two sets, so a backup taken meanwhile needs a second look.
async fn moves_in_flight(control: &Database) -> u64 {
    use forgebase_db::backend::{Backend, Statement};
    control
        .fetch_all(&Statement::raw(
            "SELECT COUNT(*) FROM _forgebase_db_moves WHERE state NOT IN ('done', 'aborted')",
        ))
        .await
        .ok()
        .and_then(|r| r.scalar().and_then(forgebase_db::backend::Value::as_i64))
        .map_or(0, |n| u64::try_from(n).unwrap_or(0))
}

/// Backs the whole cluster up into the new directory `dest`: control, then
/// every shard, each from its replica or primary as `opts.source` says,
/// and the combined manifest last (a directory without it is unfinished).
///
/// # Errors
/// The database is not PostgreSQL; a set of the topology has no source (or
/// the reverse); a data directory path is not one a restore can use; a
/// `pg_basebackup` fails.
pub async fn create_cluster(
    control: &Database,
    layout: &DataLayout,
    dest: &Path,
    opts: ClusterBackupOptions,
) -> Result<ClusterManifest> {
    if control.kind() != BackendKind::Postgres {
        return Err(BackupError::Invalid(
            "a cluster backup needs PostgreSQL: SQLite deployments have one set, use `forgebase backup`".into(),
        ));
    }
    if dest.exists() {
        return Err(BackupError::Conflict(format!("{} already exists", dest.display())));
    }
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| BackupError::Invalid(format!("{} is not a usable backup path", dest.display())))?;

    let topo = topology::load(control).await?;
    let (placements, _) = placement_summary(control).await?;
    let live: Vec<&DbSet> = topo
        .sets
        .iter()
        .filter(|s| s.state != topology::SetState::Retired)
        .collect();
    let missing: Vec<&str> = live
        .iter()
        .filter(|s| !opts.sets.iter().any(|x| x.set.id == s.id))
        .map(|s| s.name.as_str())
        .collect();
    if !missing.is_empty() {
        return Err(BackupError::Invalid(format!(
            "no way to reach the primary of set(s) {}: a cluster backup copies every set (start the node here, \
             or back it up on its host)",
            missing.join(", ")
        )));
    }
    if let Some(extra) = opts.sets.iter().find(|x| !live.iter().any(|s| s.id == x.set.id)) {
        return Err(BackupError::Invalid(format!(
            "set {} is not in the topology (or is retired)",
            extra.set.name
        )));
    }
    for s in &opts.sets {
        for node in std::iter::once(&s.primary).chain(&s.replicas) {
            if let Some(d) = &node.data_dir
                && !valid_data_dir(d)
            {
                return Err(BackupError::Invalid(format!(
                    "{d:?} is not a PostgreSQL data directory of this data directory \
                     (postgres/data or postgres/nodes/<name>/data)"
                )));
            }
        }
        if s.primary.data_dir.is_none() {
            return Err(BackupError::Invalid(format!(
                "the primary of set {} has no data directory: a restore would not know where to put it",
                s.set.name
            )));
        }
    }

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

    let mut warnings = Vec::new();
    let in_flight = moves_in_flight(control).await;
    if in_flight > 0 {
        warnings.push(format!(
            "{in_flight} tenant move(s) were not finished when the backup started: their rows may be on two \
             sets, and the placement says which one is right; check after a restore"
        ));
    }

    // Control first, then shards by number.
    let mut sources = opts.sets.clone();
    sources.sort_by_key(|s| (!s.set.is_control(), s.set.number));
    let mut entries = Vec::with_capacity(sources.len());
    for s in &sources {
        let (node, role, fallback) = match (opts.source, s.replicas.first()) {
            (BackupSource::Replica, Some(r)) => (r, NodeRole::Replica, None),
            (BackupSource::Replica, None) => (
                &s.primary,
                NodeRole::Primary,
                Some(format!(
                    "no healthy replica of set {}: copied from its primary {}",
                    s.set.name, s.primary.name
                )),
            ),
            (BackupSource::Primary, _) => (&s.primary, NodeRole::Primary, None),
        };
        if let Some(f) = &fallback {
            warnings.push(f.clone());
        }
        let rel = member_dir(&s.set);
        let member_opts = BackupOptions {
            pg: Some(node.pg.clone()),
            database_only: !s.set.is_control(),
            label: opts.base.label.clone(),
            note: Some(format!("cluster backup {name}, set {}", s.set.name)),
            ..opts.base.clone()
        };
        let member_path = join_rel(&partial, &rel);
        let m = backup::create_member(&s.db, layout, &member_path, member_opts).await?;
        let wal = wal_range(&member_path.join(PG_DIR));
        let (manifest_sha256, _) = sha256_file(&member_path.join(MANIFEST))?;
        tracing::info!(set = %s.set.name, node = %node.name, files = m.files.len(), "set backed up");
        entries.push(SetEntry {
            id: s.set.id.clone(),
            name: s.set.name.clone(),
            number: s.set.number,
            kind: s.set.kind,
            dir: rel,
            method: m.engine_info().method,
            schema_version: m.schema_version,
            node: node.name.clone(),
            role,
            data_dir: s.primary.data_dir.clone(),
            timeline: wal.as_ref().map(|w| w.timeline),
            start_lsn: wal.as_ref().map(|w| w.start_lsn.clone()),
            end_lsn: wal.map(|w| w.end_lsn),
            fallback,
            manifest_sha256,
            files: m.files.len(),
            bytes: m.total_bytes(),
        });
    }

    let after = topology::load(control).await?;
    if after.epoch != topo.epoch {
        warnings.push(format!(
            "the topology changed while the backup ran (epoch {} to {}); the manifest records the state at the start",
            topo.epoch, after.epoch
        ));
    }
    let manifest = ClusterManifest {
        format: CLUSTER_FORMAT,
        id: name.clone(),
        created: forgebase_core::now_rfc3339(),
        forgebase_version: env!("CARGO_PKG_VERSION").to_owned(),
        label: opts.base.label.clone(),
        source: opts.source,
        topology_epoch: topo.epoch,
        placements,
        topology: topo,
        sets: entries,
        warnings,
    };
    let json = serde_json::to_vec_pretty(&manifest).map_err(|e| BackupError::Internal(e.to_string()))?;
    let (p2, final_dir) = (partial.clone(), dest.to_path_buf());
    spawn(move || {
        fsutil::write_atomic(&p2.join(CLUSTER_MANIFEST), &json)?;
        std::fs::rename(&p2, &final_dir).map_err(io(&p2))?;
        if let Some(parent) = final_dir.parent() {
            fsutil::sync_dir(parent);
        }
        Ok(())
    })
    .await?;
    tracing::info!(id = %manifest.id, sets = manifest.sets.len(), bytes = manifest.total_bytes(), "cluster backup written");
    Ok(manifest)
}

// ---------------------------------------------------------------------------
// Verify.
// ---------------------------------------------------------------------------

/// One set's result inside [`ClusterVerifyReport`].
#[derive(Debug, Clone, Serialize)]
pub struct SetVerify {
    /// Set id.
    pub id: String,
    /// Set name.
    pub name: String,
    /// The set's own report.
    pub report: VerifyReport,
}

/// Outcome of [`verify_cluster`].
#[derive(Debug, Clone, Serialize)]
pub struct ClusterVerifyReport {
    /// No problems in the combined manifest or in any set.
    pub ok: bool,
    /// The combined manifest, when it could be read.
    pub manifest: Option<ClusterManifest>,
    /// One report per set.
    pub sets: Vec<SetVerify>,
    /// Files checked in every set.
    pub files_checked: usize,
    /// Bytes hashed in every set.
    pub bytes_checked: u64,
    /// Reasons the backup cannot be restored.
    pub problems: Vec<String>,
    /// Things worth knowing that do not block a restore.
    pub warnings: Vec<String>,
}

/// Verifies a cluster backup: the combined manifest, every set's backup
/// (hashes, `pg_verifybackup`) and that the two agree. Never fails:
/// problems are reported.
pub async fn verify_cluster(dir: &Path, opts: &VerifyOptions) -> ClusterVerifyReport {
    let mut report = ClusterVerifyReport {
        ok: false,
        manifest: None,
        sets: Vec::new(),
        files_checked: 0,
        bytes_checked: 0,
        problems: Vec::new(),
        warnings: Vec::new(),
    };
    let manifest = match ClusterManifest::read(dir) {
        Ok(m) => m,
        Err(BackupError::Verify(p)) => {
            report.problems = p;
            return report;
        }
        Err(e) => {
            report.problems.push(e.to_string());
            return report;
        }
    };
    let controls = manifest.sets.iter().filter(|s| s.kind == SetKind::Control).count();
    if controls != 1 {
        report.problems.push(format!(
            "the manifest lists {controls} control sets, it needs exactly one"
        ));
    }
    let mut ids = BTreeSet::new();
    let mut numbers = BTreeSet::new();
    for s in &manifest.sets {
        if !ids.insert(s.id.as_str()) || !numbers.insert(s.number) {
            report
                .problems
                .push(format!("set {} is listed twice (id or number)", s.name));
        }
        if !safe_rel(&s.dir) {
            report.problems.push(format!("unsafe path in manifest: {:?}", s.dir));
        }
    }
    if !report.problems.is_empty() {
        report.manifest = Some(manifest);
        return report;
    }

    for s in &manifest.sets {
        let member = join_rel(dir, &s.dir);
        let r = verify_with(&member, opts).await;
        let mut set_problems = Vec::new();
        match sha256_file(&member.join(MANIFEST)) {
            Ok((sha, _)) if sha != s.manifest_sha256 => set_problems.push(format!(
                "{}: its {MANIFEST} is not the one {CLUSTER_MANIFEST} recorded (SHA-256 differs)",
                s.name
            )),
            Ok(_) => {}
            Err(_) => {} // verify_with reports the missing manifest
        }
        if let Some(m) = &r.manifest
            && m.schema_version != s.schema_version
        {
            set_problems.push(format!(
                "{}: its backup is at schema v{} but {CLUSTER_MANIFEST} says v{}",
                s.name, m.schema_version, s.schema_version
            ));
        }
        report
            .problems
            .extend(r.problems.iter().map(|p| format!("{}: {p}", s.name)));
        report.problems.extend(set_problems);
        report.warnings.extend(
            r.warnings
                .iter()
                .filter(|w| s.kind == SetKind::Control || !w.contains("master key"))
                .map(|w| format!("{}: {w}", s.name)),
        );
        report.files_checked += r.files_checked;
        report.bytes_checked += r.bytes_checked;
        report.sets.push(SetVerify {
            id: s.id.clone(),
            name: s.name.clone(),
            report: r,
        });
    }

    // Nothing else may sit in the directory.
    let known: BTreeSet<String> = manifest.sets.iter().map(|s| s.dir.clone()).collect();
    match std::fs::read_dir(dir) {
        Ok(rd) => {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().into_owned();
                if n == CLUSTER_MANIFEST || n == CONTROL_DIR || n == SETS_DIR {
                    continue;
                }
                report.problems.push(format!("{n} is not part of the cluster backup"));
            }
        }
        Err(e) => report.problems.push(e.to_string()),
    }
    if let Ok(rd) = std::fs::read_dir(dir.join(SETS_DIR)) {
        for e in rd.flatten() {
            let rel = format!("{SETS_DIR}/{}", e.file_name().to_string_lossy());
            if !known.contains(&rel) {
                report
                    .problems
                    .push(format!("{rel} is not listed in {CLUSTER_MANIFEST}"));
            }
        }
    }
    report.warnings.extend(manifest.warnings.iter().cloned());
    report.ok = report.problems.is_empty();
    report.manifest = Some(manifest);
    report
}

// ---------------------------------------------------------------------------
// Restore.
// ---------------------------------------------------------------------------

/// What [`restore_cluster`] may use.
#[derive(Debug, Clone, Default)]
pub struct ClusterRestoreOptions {
    /// Force, master key, indexes and the engine's tools, as for one set.
    pub base: RestoreOptions,
    /// Where to load a set that was copied logically (`pg_dump`), by set id.
    pub external: BTreeMap<String, PgSource>,
}

/// A replica a restore invalidated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplicaRef {
    /// Set id.
    pub set_id: String,
    /// Node id.
    pub node_id: String,
    /// Node name.
    pub node: String,
    /// Where it runs: local ones are rebuilt here, remote ones by their
    /// agent, external ones by the operator.
    pub kind: NodeKind,
}

/// One set after a restore.
#[derive(Debug, Clone, Serialize)]
pub struct SetRestored {
    /// Set id.
    pub id: String,
    /// Set name.
    pub name: String,
    /// Where its data went (`None` for a set loaded from a dump).
    pub data_dir: Option<String>,
    /// Schema version restored.
    pub schema_version: i64,
}

/// Result of [`restore_cluster`].
#[derive(Debug, Clone, Serialize)]
pub struct ClusterRestoreReport {
    /// Backup id.
    pub id: String,
    /// The placement epoch of the backup.
    pub topology_epoch: u64,
    /// Every set restored.
    pub sets: Vec<SetRestored>,
    /// Replicas that must be rebuilt before they serve again.
    pub replicas: Vec<ReplicaRef>,
    /// Clusters of the data directory that were moved aside, not restored
    /// (replicas, an old primary).
    pub moved_aside: Vec<String>,
    /// Files restored.
    pub files: usize,
    /// Bytes restored.
    pub bytes: u64,
    /// Where the replaced data went.
    pub previous_data: Option<PathBuf>,
    /// What happened to the master key.
    pub master_key: String,
    /// Findings that do not block.
    pub warnings: Vec<String>,
    /// The file [`complete_restore`] reads.
    pub pending_file: PathBuf,
}

/// What a restore leaves for [`complete_restore`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingRestore {
    /// RFC 3339 time of the restore.
    pub restored_at: String,
    /// Backup id.
    pub backup_id: String,
    /// The placement epoch of the backup.
    pub topology_epoch: u64,
    /// Replicas to mark `needs_rebuild` and rebuild.
    pub replicas: Vec<ReplicaRef>,
    /// Set ids to bring to the schema head.
    pub sets: Vec<String>,
}

/// Where the pending file of a data directory is.
pub fn pending_path(layout: &DataLayout) -> PathBuf {
    layout.root().join(RESTORE_PENDING)
}

/// The pending work of an earlier [`restore_cluster`], if any.
///
/// # Errors
/// The file exists but is unreadable.
pub fn read_pending(layout: &DataLayout) -> Result<Option<PendingRestore>> {
    let path = pending_path(layout);
    match std::fs::read_to_string(&path) {
        Ok(t) => serde_json::from_str(&t)
            .map(Some)
            .map_err(|e| BackupError::Conflict(format!("{} is unreadable: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io(&path)(e)),
    }
}

/// Every PostgreSQL cluster directory of the data directory, relative:
/// `postgres/data` and `postgres/nodes/*/data`.
fn clusters_present(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if root.join(PRIMARY_DATA).join("PG_VERSION").is_file() {
        out.push(PRIMARY_DATA.to_owned());
    }
    if let Ok(rd) = std::fs::read_dir(root.join("postgres").join("nodes")) {
        let mut names: Vec<String> = rd
            .flatten()
            .filter(|e| e.path().join("data").is_dir())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        out.extend(names.into_iter().map(|n| format!("postgres/nodes/{n}/data")));
    }
    out
}

/// Makes a copy of a standby's data directory start as a primary: removes
/// `standby.signal` and the follow settings of `postgresql.auto.conf`.
/// A node that was copied from a replica needs this; the files change only
/// after `pg_verifybackup` and the hashes were checked.
fn defollow(pgdata: &Path) -> Result<()> {
    let signal = pgdata.join("standby.signal");
    if signal.exists() {
        std::fs::remove_file(&signal).map_err(io(&signal))?;
    }
    let conf = pgdata.join("postgresql.auto.conf");
    let Ok(text) = std::fs::read_to_string(&conf) else {
        return Ok(());
    };
    let kept: Vec<&str> = text
        .lines()
        .filter(|l| {
            let key = l.split('=').next().unwrap_or("").trim().to_ascii_lowercase();
            key != "primary_conninfo" && key != "primary_slot_name"
        })
        .collect();
    let mut out = kept.join("\n");
    out.push('\n');
    fsutil::write_atomic(&conf, out.as_bytes())
}

/// Copies one set's files into the staging area. `data` is where the set's
/// cluster goes; the control set also brings storage, indexes, the key and
/// the role passwords.
fn stage_member(
    backup: &Path,
    m: &Manifest,
    staging: &Path,
    data: Option<&str>,
    control: bool,
    skip_indexes: bool,
) -> Result<(usize, u64)> {
    let (mut files, mut bytes) = (0usize, 0u64);
    for f in &m.files {
        if f.kind == FileKind::Index && skip_indexes {
            continue;
        }
        let target = if let Some(rest) = f.path.strip_prefix(&format!("{PG_DIR}/")) {
            match data {
                Some(d) => format!("{d}/{rest}"),
                None => continue,
            }
        } else if f.path == ROLES_FILE {
            if !control {
                continue;
            }
            format!("postgres/{ROLES_FILE}")
        } else if f.kind == FileKind::Dump {
            continue;
        } else {
            f.path.clone()
        };
        let (sha, size) = copy_hashed_unsynced(&join_rel(backup, &f.path), &join_rel(staging, &target))?;
        if sha != f.sha256 || size != f.size {
            return Err(BackupError::Verify(vec![format!(
                "{} changed while it was being restored",
                f.path
            )]));
        }
        files += 1;
        bytes += size;
    }
    if let Some(d) = data {
        // PostgreSQL needs its (often empty) directories: pg_wal/archive_status, pg_tblspc, ...
        copy_dir_tree(&backup.join(PG_DIR), &join_rel(staging, d))?;
    }
    Ok((files, bytes))
}

fn copy_dir_tree(src: &Path, dst: &Path) -> Result<()> {
    mkdir_private(dst).map_err(io(dst))?;
    for entry in std::fs::read_dir(src).map_err(io(src))? {
        let entry = entry.map_err(io(src))?;
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            copy_dir_tree(&entry.path(), &dst.join(entry.file_name()))?;
        }
    }
    Ok(())
}

/// Restores a cluster backup into a stopped data directory: every set's
/// data, in one journaled swap (an interrupted restore is rolled back by
/// [`restore::recover`], which the next restore runs first). The replicas
/// and old primaries of the directory are moved aside, `topology.json` is
/// rewritten from the backup, and `restore-pending.json` records what
/// [`complete_restore`] must do once the control database runs.
///
/// # Errors
/// The backup does not verify; a cluster of the directory still runs; the
/// directory holds data and `force` is off; another PostgreSQL major; the
/// master key does not match; a set copied logically has no server to load
/// into (`external`).
pub async fn restore_cluster(
    backup: &Path,
    layout: &DataLayout,
    opts: ClusterRestoreOptions,
) -> Result<ClusterRestoreReport> {
    layout.ensure().map_err(io(layout.root()))?;
    restore::recover(layout)?;
    let root = layout.root().to_path_buf();

    let report = verify_cluster(
        backup,
        &VerifyOptions {
            pg: opts.base.pg.clone(),
            tmp: Some(layout.tmp_dir()),
        },
    )
    .await;
    if !report.ok {
        return Err(BackupError::Verify(report.problems));
    }
    let Some(cm) = report.manifest else {
        return Err(BackupError::Internal("verified backup has no manifest".into()));
    };
    let mut warnings = report.warnings;

    // What goes where.
    struct Plan {
        entry: SetEntry,
        manifest: Manifest,
        dir: PathBuf,
        data: Option<String>,
    }
    let mut plans = Vec::new();
    for e in &cm.sets {
        let dir = join_rel(backup, &e.dir);
        let manifest = Manifest::read(&dir)?;
        let physical = manifest.is_physical_postgres();
        let data = physical.then(|| e.data_dir.clone()).flatten();
        if physical && data.is_none() {
            return Err(BackupError::Invalid(format!(
                "set {} has no data directory recorded; the backup cannot be restored",
                e.name
            )));
        }
        if let Some(d) = &data
            && !valid_data_dir(d)
        {
            return Err(BackupError::Invalid(format!(
                "set {} names an unusable data directory {d:?}",
                e.name
            )));
        }
        if !physical && !opts.external.contains_key(&e.id) {
            return Err(BackupError::Invalid(format!(
                "set {} was copied logically (pg_dump): name the server to load it into",
                e.name
            )));
        }
        plans.push(Plan {
            entry: e.clone(),
            manifest,
            dir,
            data,
        });
    }
    let targets: Vec<&str> = plans.iter().filter_map(|p| p.data.as_deref()).collect();
    let mut seen = BTreeSet::new();
    if let Some(d) = targets.iter().find(|d| !seen.insert(**d)) {
        return Err(BackupError::Invalid(format!("two sets restore into {d}")));
    }

    // The target must be quiet, empty or forced.
    if layout.database_path().exists() {
        return Err(BackupError::Conflict(format!(
            "{} holds a SQLite database but the backup is PostgreSQL: a restore does not change the backend; \
             move the project with `forgebase db convert` (FB-151) or restore into an empty data directory",
            root.display()
        )));
    }
    let present = clusters_present(&root);
    for rel in &present {
        if let Some(pid) = pg::running_postmaster(&root.join(rel)) {
            return Err(BackupError::Conflict(format!(
                "a PostgreSQL server (pid {pid}) is still running on {}; stop ForgeBase, the replicas and the shard \
                 nodes of this host and wait for them to exit",
                root.join(rel).display()
            )));
        }
    }
    let logical_sets = plans.iter().any(|p| p.data.is_none());
    if (!present.is_empty() || logical_sets) && !opts.base.force {
        return Err(BackupError::Conflict(format!(
            "{} already holds a database; use --force to replace it (the current data, replicas included, is kept \
             in backups/pre-restore-*)",
            root.display()
        )));
    }
    let here = opts.base.pg.as_ref().and_then(PgSource::major);
    for p in &plans {
        let engine = p.manifest.engine_info();
        match (engine.major, here) {
            (Some(backup), Some(here)) if backup != here => {
                return Err(BackupError::Invalid(format!(
                    "set {} is {}, this engine is PostgreSQL {here}: a physical backup restores on the same major \
                     only; restore it with an engine of major {backup}, or move the data with a logical export",
                    p.entry.name,
                    engine.describe()
                )));
            }
            (_, None) if p.data.is_some() => warnings.push(format!(
                "the engine's major version is unknown here; set {} is checked when its server starts",
                p.entry.name
            )),
            _ => {}
        }
    }
    warnings.dedup();

    // The control set carries the key, storage and indexes.
    let Some(control) = plans.iter().find(|p| p.entry.kind == SetKind::Control) else {
        return Err(BackupError::Internal("verified backup has no control set".into()));
    };
    let key_path = backup::master_key_path(layout);
    let source = if control.manifest.master_key.included {
        KeySource::Backup
    } else if let Some(p) = &opts.base.master_key {
        KeySource::Provided(p.clone())
    } else if key_path.is_file() {
        KeySource::Existing
    } else {
        KeySource::None
    };
    let key_note = restore::resolve_key(&control.manifest, &source, &key_path, opts.base.allow_missing_key)?;
    let provided = if let KeySource::Provided(p) = &source {
        Some(p.clone())
    } else {
        None
    };

    // Stage everything.
    let stamp = forgebase_core::now_rfc3339().replace([':', '-'], "");
    let staging = layout.tmp_dir().join(format!("restore-{stamp}-{}", std::process::id()));
    let aside = layout.backups_dir().join(format!("pre-restore-{stamp}"));
    let staged: Vec<(Manifest, PathBuf, Option<String>, bool, bool)> = plans
        .iter()
        .map(|p| {
            (
                p.manifest.clone(),
                p.dir.clone(),
                p.data.clone(),
                p.entry.kind == SetKind::Control,
                p.entry.role == NodeRole::Replica,
            )
        })
        .collect();
    let (st, skip_indexes) = (staging.clone(), opts.base.skip_indexes);
    let (files, bytes) = spawn(move || {
        let _ = std::fs::remove_dir_all(&st);
        mkdir_private(&st).map_err(io(&st))?;
        let (mut files, mut bytes) = (0usize, 0u64);
        for (m, dir, data, control, from_replica) in &staged {
            let (f, b) = stage_member(dir, m, &st, data.as_deref(), *control, skip_indexes)?;
            files += f;
            bytes += b;
            if *from_replica && let Some(d) = data {
                defollow(&join_rel(&st, d))?;
            }
        }
        if let Some(k) = provided {
            copy_hashed(&k, &join_rel(&st, KEY_FILE))?;
        }
        // One flush for the staged tree, before the journaled swap makes it live.
        fsutil::sync_all_files();
        Ok((files, bytes))
    })
    .await?;

    // A set copied logically is loaded first: one transaction, nothing changes if it fails.
    for p in plans.iter().filter(|p| p.data.is_none()) {
        let src = &opts.external[&p.entry.id];
        let dump = join_rel(&staging, crate::manifest::DUMP_FILE);
        std::fs::copy(p.dir.join(crate::manifest::DUMP_FILE), &dump).map_err(io(&dump))?;
        src.restore_dump(&dump, &layout.tmp_dir()).await?;
    }

    // Items: what the backup brings, and every other cluster, moved aside.
    let mut items: Vec<String> = vec!["storage".into(), "indexes".into(), format!("postgres/{ROLES_FILE}")];
    if matches!(source, KeySource::Backup | KeySource::Provided(_)) {
        items.push(KEY_FILE.to_owned());
    }
    items.extend(targets.iter().map(|d| (*d).to_owned()));
    let moved_aside: Vec<String> = present
        .iter()
        .filter(|rel| !targets.contains(&rel.as_str()))
        .cloned()
        .collect();
    items.extend(moved_aside.iter().cloned());

    let (lay, st, ad, crash_after) = (layout.clone(), staging.clone(), aside.clone(), opts.base.crash_after);
    let swapped = spawn(move || restore::swap(&lay, &st, &ad, items, crash_after)).await;
    let moved_any = match swapped {
        Ok(v) => v,
        Err(e) => {
            if crash_after.is_none() {
                let _ = restore::recover(layout);
            }
            return Err(e);
        }
    };

    // The topology as it was, every replica needing a rebuild.
    let mut topo = cm.topology.clone();
    let mut replicas = Vec::new();
    for n in topo
        .nodes
        .iter_mut()
        .filter(|n| n.role == NodeRole::Replica && n.state != NodeState::Retired)
    {
        replicas.push(ReplicaRef {
            set_id: n.set_id.clone(),
            node_id: n.id.clone(),
            node: n.name.clone(),
            kind: n.kind,
        });
        n.state = NodeState::NeedsRebuild;
    }
    topology::write_topology_file(&root.join(TOPOLOGY_FILE), &topo)?;

    let pending = PendingRestore {
        restored_at: forgebase_core::now_rfc3339(),
        backup_id: cm.id.clone(),
        topology_epoch: cm.topology_epoch,
        replicas: replicas.clone(),
        sets: cm.sets.iter().map(|s| s.id.clone()).collect(),
    };
    let pending_file = pending_path(layout);
    let json = serde_json::to_vec_pretty(&pending).map_err(|e| BackupError::Internal(e.to_string()))?;
    fsutil::write_atomic(&pending_file, &json)?;
    tracing::info!(id = %cm.id, sets = cm.sets.len(), replicas = replicas.len(), "cluster restore complete");

    Ok(ClusterRestoreReport {
        id: cm.id,
        topology_epoch: cm.topology_epoch,
        sets: plans
            .iter()
            .map(|p| SetRestored {
                id: p.entry.id.clone(),
                name: p.entry.name.clone(),
                data_dir: p.data.clone(),
                schema_version: p.entry.schema_version,
            })
            .collect(),
        replicas,
        moved_aside,
        files,
        bytes,
        previous_data: moved_any.then_some(aside),
        master_key: key_note,
        warnings,
        pending_file,
    })
}

// ---------------------------------------------------------------------------
// After the restore.
// ---------------------------------------------------------------------------

/// What happened to a replica [`complete_restore`] was asked to rebuild.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RebuildOutcome {
    /// Re-cloned from the restored primary and running again.
    Rebuilt,
    /// A remote node's agent was told to rebuild; it reports when done.
    Queued,
    /// Left to the operator, with the reason (an external replica).
    Manual(String),
}

/// Rebuilds a replica from the restored primary. The server implements it
/// with its replica service (`forgebase_pgengine::replication::service::rebuild_replica`).
pub trait ReplicaRebuilder: Send + Sync {
    /// Rebuilds `node` (state `needs_rebuild`).
    fn rebuild<'a>(
        &'a self,
        node: &'a DbNode,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<RebuildOutcome>> + Send + 'a>>;
}

/// What [`complete_restore`] may use.
#[derive(Clone, Copy, Default)]
pub struct CompleteOptions<'a> {
    /// The router over control and every shard: its sets are migrated to
    /// the schema head (the schema reconcile). Without it the reconcile is
    /// left to `forgebase migrate` / the server's start.
    pub router: Option<&'a DbRouter>,
    /// Where pre-migration snapshots of the sets go.
    pub backups_dir: Option<&'a Path>,
    /// Rebuilds the replicas. Without it they stay `needs_rebuild`.
    pub rebuilder: Option<&'a dyn ReplicaRebuilder>,
}

/// Result of [`complete_restore`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CompleteReport {
    /// Whether a restore was pending.
    pub pending: bool,
    /// Replicas marked `needs_rebuild`.
    pub marked: Vec<String>,
    /// Replicas re-cloned and running.
    pub rebuilt: Vec<String>,
    /// Replicas a remote agent was told to rebuild.
    pub queued: Vec<String>,
    /// Replicas left to the operator, with the reason.
    pub manual: Vec<(String, String)>,
    /// Replicas whose rebuild failed, with the error.
    pub failed: Vec<(String, String)>,
    /// Sets migrated to the schema head, with the number of migrations applied.
    pub migrated: Vec<(String, usize)>,
    /// Sets already at the head.
    pub up_to_date: Vec<String>,
    /// Sets that could not be migrated, with the error.
    pub schema_failed: Vec<(String, String)>,
    /// Where `topology.json` was rewritten.
    pub topology_file: Option<PathBuf>,
}

impl CompleteReport {
    /// Everything done: no failure and nothing left for the operator.
    pub fn is_complete(&self) -> bool {
        self.failed.is_empty() && self.schema_failed.is_empty() && self.manual.is_empty() && self.queued.is_empty()
    }
}

/// Finishes a cluster restore with the restored control database running
/// (ADR-0029 §2.9): brings every set to the schema head, marks every
/// replica of the restored topology `needs_rebuild` (the restore rewrote
/// the primary's history), rewrites `topology.json` and rebuilds the
/// replicas it can. Idempotent: the pending file is removed only when
/// nothing is left to do, so a failed rebuild is retried by calling this
/// again.
///
/// # Errors
/// The control database cannot be read or written.
pub async fn complete_restore(
    control: &Database,
    layout: &DataLayout,
    opts: CompleteOptions<'_>,
) -> Result<CompleteReport> {
    let mut report = CompleteReport::default();
    let Some(pending) = read_pending(layout)? else {
        return Ok(report);
    };
    report.pending = true;

    // 1. Schema reconcile: every set to the head, control first. Replicas
    //    are cloned from the primary afterwards, so they get the head.
    if let Some(router) = opts.router {
        let multi = forgebase_db::shards::migrate_sets(router, opts.backups_dir, MultiOptions::default()).await;
        for r in std::iter::once(&multi.control).chain(&multi.shards) {
            match &r.outcome {
                NodeOutcome::UpToDate => report.up_to_date.push(r.id.clone()),
                NodeOutcome::Applied(a) => report.migrated.push((r.id.clone(), a.len())),
                NodeOutcome::Failed { message, .. } => report.schema_failed.push((r.id.clone(), message.clone())),
                NodeOutcome::Skipped => report
                    .schema_failed
                    .push((r.id.clone(), "skipped: control did not migrate".into())),
            }
        }
    }

    // 2. Every replica of the restored topology needs a rebuild.
    let topo = topology::load(control).await?;
    let replicas: Vec<DbNode> = topo
        .nodes
        .iter()
        .filter(|n| n.role == NodeRole::Replica && n.state != NodeState::Retired)
        .cloned()
        .collect();
    for n in &replicas {
        if n.state != NodeState::NeedsRebuild {
            topology::update_node_state(control, &n.id, NodeState::NeedsRebuild).await?;
        }
        report.marked.push(n.name.clone());
    }

    // 3. topology.json follows the control tables.
    let file = layout.root().join(TOPOLOGY_FILE);
    topology::write_topology_file(&file, &topology::load(control).await?)?;
    report.topology_file = Some(file.clone());

    // 4. Rebuild what we can.
    let mut left: Vec<ReplicaRef> = Vec::new();
    for n in &replicas {
        let this = ReplicaRef {
            set_id: n.set_id.clone(),
            node_id: n.id.clone(),
            node: n.name.clone(),
            kind: n.kind,
        };
        let Some(rebuilder) = opts.rebuilder else {
            left.push(this);
            continue;
        };
        let fresh = topology::load(control)
            .await?
            .nodes
            .into_iter()
            .find(|x| x.id == n.id)
            .unwrap_or_else(|| n.clone());
        match rebuilder.rebuild(&fresh).await {
            Ok(RebuildOutcome::Rebuilt) => report.rebuilt.push(n.name.clone()),
            Ok(RebuildOutcome::Queued) => report.queued.push(n.name.clone()),
            Ok(RebuildOutcome::Manual(why)) => {
                report.manual.push((n.name.clone(), why));
                left.push(this);
            }
            Err(e) => {
                report.failed.push((n.name.clone(), e.to_string()));
                left.push(this);
            }
        }
    }
    topology::write_topology_file(&file, &topology::load(control).await?)?;

    // 5. Done when nothing is left and every set is at the head.
    let path = pending_path(layout);
    if left.is_empty() && report.schema_failed.is_empty() && opts.router.is_some() {
        std::fs::remove_file(&path).map_err(io(&path))?;
    } else {
        let rest = PendingRestore {
            replicas: left,
            ..pending
        };
        let json = serde_json::to_vec_pretty(&rest).map_err(|e| BackupError::Internal(e.to_string()))?;
        fsutil::write_atomic(&path, &json)?;
    }
    Ok(report)
}

/// Listing helper for callers that look for files a restore may leave
/// behind: the data directory's top-level pending file and journal.
pub fn restore_in_progress(layout: &DataLayout) -> bool {
    restore::interrupted(layout) || pending_path(layout).is_file()
}

// ---------------------------------------------------------------------------
// Tests of the pure parts.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_directories_are_checked() {
        assert!(valid_data_dir("postgres/data"));
        assert!(valid_data_dir("postgres/nodes/r1/data"));
        assert!(valid_data_dir("postgres/nodes/shard-2_a/data"));
        assert!(!valid_data_dir("postgres/nodes/../data"));
        assert!(!valid_data_dir("postgres/nodes/a/b/data"));
        assert!(!valid_data_dir("postgres/nodes/A/data"));
        assert!(!valid_data_dir("/etc/data"));
        assert!(!valid_data_dir("postgres"));
        assert!(!valid_data_dir("storage"));
    }

    #[test]
    fn the_wal_range_comes_from_the_backup_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("backup_manifest"),
            r#"{"PostgreSQL-Backup-Manifest-Version":2,"Files":[],
                "WAL-Ranges":[{"Timeline":1,"Start-LSN":"0/3000028","End-LSN":"0/3000100"},
                              {"Timeline":2,"Start-LSN":"0/4000028","End-LSN":"0/4000100"}]}"#,
        )
        .unwrap();
        assert_eq!(
            wal_range(tmp.path()),
            Some(WalRange {
                timeline: 2,
                start_lsn: "0/4000028".into(),
                end_lsn: "0/4000100".into()
            })
        );
        assert_eq!(wal_range(&tmp.path().join("nope")), None);
    }

    #[test]
    fn a_standby_copy_loses_its_follow_settings() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("standby.signal"), "").unwrap();
        std::fs::write(
            tmp.path().join("postgresql.auto.conf"),
            "# Do not edit\nwork_mem = '8MB'\nprimary_conninfo = 'host=x'\nprimary_slot_name = 'fbr_r1'\n",
        )
        .unwrap();
        defollow(tmp.path()).unwrap();
        assert!(!tmp.path().join("standby.signal").exists());
        let conf = std::fs::read_to_string(tmp.path().join("postgresql.auto.conf")).unwrap();
        assert!(conf.contains("work_mem") && !conf.contains("primary_"), "{conf}");
        defollow(tmp.path()).unwrap();
    }

    #[test]
    fn clusters_present_lists_primary_and_nodes() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(clusters_present(tmp.path()).is_empty());
        std::fs::create_dir_all(tmp.path().join("postgres/data")).unwrap();
        std::fs::write(tmp.path().join("postgres/data/PG_VERSION"), "18\n").unwrap();
        std::fs::create_dir_all(tmp.path().join("postgres/nodes/r1/data")).unwrap();
        std::fs::create_dir_all(tmp.path().join("postgres/nodes/s2/data")).unwrap();
        assert_eq!(
            clusters_present(tmp.path()),
            ["postgres/data", "postgres/nodes/r1/data", "postgres/nodes/s2/data"]
        );
    }

    #[test]
    fn member_directories_follow_the_set_number() {
        let mut set = DbSet::control();
        assert_eq!(member_dir(&set), "control");
        set.kind = SetKind::Shard;
        set.number = 7;
        assert_eq!(member_dir(&set), "sets/7");
    }
}
