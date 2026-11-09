//! `MANIFEST.json`: the last file a backup writes, listing every file with its
//! size and SHA-256. A directory without it is an unfinished backup.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{BackupError, Result, io};

/// Manifest file name.
pub const MANIFEST: &str = "MANIFEST.json";
/// Current manifest format.
pub const FORMAT: u32 = 1;
/// Database file name inside a backup.
pub const DB_FILE: &str = "forgebase.db";
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
}

impl Manifest {
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
        if m.format != FORMAT {
            return Err(BackupError::Verify(vec![format!(
                "unsupported manifest format {} (this build reads format {FORMAT})",
                m.format
            )]));
        }
        Ok(m)
    }
}
