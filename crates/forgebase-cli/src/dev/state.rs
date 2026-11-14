//! What a running emulator leaves behind so `forgebase dev seed|reset` can
//! find it: a registry file in the temp directory, keyed by the project
//! directory, and (with `--persist`) the generated credentials in the data
//! directory so a restart signs in as the same administrator.

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Credentials of the bootstrapped administrator and the dev API key.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Creds {
    /// Administrator email.
    pub admin_email: String,
    /// Administrator password (generated; for local development only).
    pub admin_password: String,
    /// API key of an `admin` service account (`fbk_...`).
    #[serde(default)]
    pub api_key: Option<String>,
}

/// File of the generated credentials inside a persisted data directory.
pub const CREDENTIALS_FILE: &str = "dev-credentials.json";

/// A running emulator, as `dev seed|reset` finds it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Running {
    /// Process id of `forgebase dev`.
    pub pid: u32,
    /// Base URL.
    pub url: String,
    /// The project directory the emulator serves.
    pub project_dir: String,
    /// Administrator and API key.
    #[serde(flatten)]
    pub creds: Creds,
}

fn write_private(path: &Path, text: &str) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(text.as_bytes())?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, text)?;
        Ok(())
    }
}

/// Reads the credentials of a persisted data directory, if it has them.
pub fn read_credentials(root: &Path) -> anyhow::Result<Option<Creds>> {
    let p = root.join(CREDENTIALS_FILE);
    if !p.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&p).with_context(|| format!("cannot read {}", p.display()))?;
    Ok(Some(
        serde_json::from_str(&text).with_context(|| format!("{} is not valid", p.display()))?,
    ))
}

/// Writes the credentials of a persisted data directory (mode 0600).
pub fn write_credentials(root: &Path, c: &Creds) -> anyhow::Result<()> {
    let p = root.join(CREDENTIALS_FILE);
    write_private(&p, &(serde_json::to_string_pretty(c)? + "\n"))
        .with_context(|| format!("cannot write {}", p.display()))
}

fn registry_dir() -> PathBuf {
    std::env::temp_dir().join("forgebase-dev")
}

/// The registry file of a project directory.
pub fn registry_path(project_dir: &Path) -> PathBuf {
    let canon = std::fs::canonicalize(project_dir).unwrap_or_else(|_| project_dir.to_path_buf());
    let digest = Sha256::digest(canon.to_string_lossy().as_bytes());
    let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    registry_dir().join(format!("{hex}.json"))
}

/// Records a running emulator.
pub fn register(project_dir: &Path, running: &Running) -> anyhow::Result<PathBuf> {
    let dir = registry_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let p = registry_path(project_dir);
    write_private(&p, &(serde_json::to_string_pretty(running)? + "\n"))
        .with_context(|| format!("cannot write {}", p.display()))?;
    Ok(p)
}

/// Forgets a registry file (best effort).
pub fn unregister(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// The emulator serving a project directory, if one is registered.
pub fn lookup(project_dir: &Path) -> Option<Running> {
    let text = std::fs::read_to_string(registry_path(project_dir)).ok()?;
    serde_json::from_str(&text).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_paths_differ_per_project_and_credentials_round_trip() {
        let a = registry_path(Path::new("/nonexistent/a"));
        let b = registry_path(Path::new("/nonexistent/b"));
        assert_ne!(a, b);
        assert_eq!(a, registry_path(Path::new("/nonexistent/a")));
        let dir = tempfile::tempdir().unwrap();
        assert!(read_credentials(dir.path()).unwrap().is_none());
        let c = Creds {
            admin_email: "a@b.c".into(),
            admin_password: "pw".into(),
            api_key: Some("fbk_x".into()),
        };
        write_credentials(dir.path(), &c).unwrap();
        let back = read_credentials(dir.path()).unwrap().unwrap();
        assert_eq!(back.api_key.as_deref(), Some("fbk_x"));
    }
}
