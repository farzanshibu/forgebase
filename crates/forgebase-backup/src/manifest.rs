//! `MANIFEST.json`: the last file a backup writes, listing every file with its
//! size and SHA-256. A directory without it is an unfinished backup.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{BackupError, Result, io};

/// Manifest file name.
pub const MANIFEST: &str = "MANIFEST.json";
/// Manifest format of SQLite backups (and of backups written before the
/// `engine` field existed).
pub const FORMAT: u32 = 1;
/// Manifest format of PostgreSQL backups (FB-150). A build that predates it
/// refuses them with "unsupported manifest format 2".
pub const FORMAT_PG: u32 = 2;
/// Database file name inside a backup.
pub const DB_FILE: &str = "forgebase.db";
/// `pg_dump -Fc` file of a logical (external-mode) backup.
pub const DUMP_FILE: &str = "postgres.dump";
/// Directory of a physical PostgreSQL backup: the contents of `PGDATA`
/// (`pg_basebackup -Fp`), with PostgreSQL's own `backup_manifest` at its root.
pub const PG_DIR: &str = "postgres";
/// The bundled engine's login passwords, copied next to the data they unlock.
pub const ROLES_FILE: &str = "roles.secret";
/// Master key path inside a backup and inside the data directory.
pub const KEY_FILE: &str = "auth/master.key";

/// What a file is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum FileKind {
    /// The system database.
    Database,
    /// An object-storage blob or staging file.
    Storage,
    /// A derived index file (rebuildable).
    Index,
    /// The auth master key.
    Key,
    /// A file of the PostgreSQL cluster (`postgres/**`, `roles.secret`).
    Postgres,
    /// A `pg_dump` custom-format file.
    Dump,
}

/// How the database was copied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Method {
    /// SQLite `VACUUM INTO`.
    Vacuum,
    /// `pg_basebackup`: a physical copy of the bundled cluster.
    Basebackup,
    /// `pg_dump -Fc`: a logical copy (external servers).
    Dump,
}

/// The database engine that wrote a backup (ADR-0028 §5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct EngineInfo {
    /// `sqlite` or `postgres`.
    pub kind: String,
    /// PostgreSQL: `managed` (the bundled engine) or `external`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// PostgreSQL: server version, such as `18.3`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// PostgreSQL: major version. A physical backup restores on this major only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub major: Option<u32>,
    /// How the database was copied.
    pub method: Method,
}

impl EngineInfo {
    /// The engine of SQLite backups.
    pub fn sqlite() -> Self {
        Self {
            kind: "sqlite".to_owned(),
            mode: None,
            version: None,
            major: None,
            method: Method::Vacuum,
        }
    }

    /// Whether this is a PostgreSQL backup.
    pub fn is_postgres(&self) -> bool {
        self.kind == "postgres"
    }

    /// Short text for messages: `sqlite`, `postgres 18.3 (managed)`.
    pub fn describe(&self) -> String {
        if !self.is_postgres() {
            return self.kind.clone();
        }
        format!(
            "postgres {} ({})",
            self.version.as_deref().unwrap_or("?"),
            self.mode.as_deref().unwrap_or("?")
        )
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// One file of a backup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct FileEntry {
    /// `/`-separated path relative to the backup directory.
    pub path: String,
    /// Size in bytes.
    pub size: u64,
    /// Lower-case hex SHA-256.
    pub sha256: String,
    /// Role of the file.
    pub kind: FileKind,
}

/// Master key handling recorded in a backup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct KeyInfo {
    /// Whether `auth/master.key` is part of the backup.
    pub included: bool,
    /// First 16 hex characters of the key file's SHA-256 (`None` when the
    /// data directory had no key). Restore refuses a different key.
    pub fingerprint: Option<String>,
}

/// The manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Manifest {
    /// Manifest format version.
    pub format: u32,
    /// Backup id (the directory name for snapshots).
    pub id: String,
    /// `backup` (explicit destination) or `snapshot` (managed, with retention).
    pub kind: String,
    /// Free-form label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// RFC 3339 creation time (UTC).
    pub created: String,
    /// ForgeBase version that wrote it.
    pub forgebase_version: String,
    /// Highest applied system migration in the copied database.
    pub schema_version: i64,
    /// Whether derived index files are included.
    pub includes_indexes: bool,
    /// Master key handling.
    pub master_key: KeyInfo,
    /// Every file with size and hash.
    pub files: Vec<FileEntry>,
    /// The engine that wrote the database copy (absent in backups made
    /// before FB-150: SQLite).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<EngineInfo>,
    /// Only the database is in this backup (pre-migration snapshots): a
    /// restore leaves storage, indexes and the master key alone.
    #[serde(default, skip_serializing_if = "is_false")]
    pub database_only: bool,
    /// Why this backup was taken, for managed ones (`pre-migration`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Manifest {
    /// The engine of this backup (`sqlite` when the manifest does not say).
    pub fn engine_info(&self) -> EngineInfo {
        self.engine.clone().unwrap_or_else(EngineInfo::sqlite)
    }

    /// Whether the database copy is a physical PostgreSQL cluster.
    pub fn is_physical_postgres(&self) -> bool {
        self.engine
            .as_ref()
            .is_some_and(|e| e.is_postgres() && e.method == Method::Basebackup)
    }

    /// Total bytes of all files.
    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }

    /// Reads `<dir>/MANIFEST.json`.
    pub fn read(dir: &Path) -> Result<Self> {
        let path = dir.join(MANIFEST);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(BackupError::Verify(vec![format!(
                    "{MANIFEST} is missing: the backup is unfinished or not a ForgeBase backup"
                )]));
            }
            Err(e) => return Err(io(&path)(e)),
        };
        let m: Manifest = serde_json::from_str(&text)
            .map_err(|e| BackupError::Verify(vec![format!("{MANIFEST} is invalid: {e}")]))?;
        if m.format != FORMAT && m.format != FORMAT_PG {
            return Err(BackupError::Verify(vec![format!(
                "unsupported manifest format {} (this build reads formats {FORMAT} and {FORMAT_PG})",
                m.format
            )]));
        }
        Ok(m)
    }
}
