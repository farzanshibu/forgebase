//! Bootstrap role passwords (`postgres/roles.secret`, ADR-0028 §4.2) and a
//! redacting wrapper for connection URLs.

use std::collections::BTreeMap;
use std::fmt;
use std::io::Write;
use std::path::Path;

use crate::error::{EngineError, Result};
use crate::fsutil;

/// A string that never shows in `Debug` or `Display` (URLs with passwords).
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Wraps `value`.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The secret value. Do not log it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// A login role the engine bootstraps (rls-coverage §3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Role {
    /// `forgebase_super`: the `initdb` superuser (socket only): bootstrap,
    /// backups, upgrades, extensions.
    Super,
    /// `forgebase_server`: the server's writer pool. `BYPASSRLS`, member of
    /// `forgebase_owner` and `forgebase_rls`, not a superuser.
    Server,
    /// `forgebase_reader`: the read pool. `BYPASSRLS`, `SELECT` only,
    /// `default_transaction_read_only = on`.
    Reader,
}

impl Role {
    /// Every login role, in bootstrap order.
    pub const ALL: [Role; 3] = [Role::Super, Role::Server, Role::Reader];

    /// The PostgreSQL role name.
    pub fn name(self) -> &'static str {
        match self {
            Role::Super => "forgebase_super",
            Role::Server => "forgebase_server",
            Role::Reader => "forgebase_reader",
        }
    }
}

/// Generated passwords of the login roles, stored in `roles.secret` (0600).
#[derive(Clone, PartialEq, Eq)]
pub struct RoleSecrets {
    passwords: BTreeMap<String, String>,
}

impl fmt::Debug for RoleSecrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RoleSecrets")
            .field("roles", &self.passwords.keys().collect::<Vec<_>>())
            .finish()
    }
}

const HEADER: &str = "# ForgeBase bootstrap role passwords (ADR-0028 §4.2). Never share or log this file.\n";

impl RoleSecrets {
    /// Fresh random passwords (32 random bytes each, hex) for every login role.
    ///
    /// # Errors
    /// When the system random generator fails.
    pub fn generate() -> Result<Self> {
        let mut passwords = BTreeMap::new();
        for role in Role::ALL {
            passwords.insert(role.name().to_owned(), random_password()?);
        }
        Ok(Self { passwords })
    }

    /// The password of `role`.
    pub fn password(&self, role: Role) -> Option<&str> {
        self.passwords.get(role.name()).map(String::as_str)
    }

    /// Adds a password for every login role that has none (a later ForgeBase
    /// adding a role). Returns whether anything was added.
    ///
    /// # Errors
    /// When the system random generator fails.
    pub fn fill_missing(&mut self) -> Result<bool> {
        let mut changed = false;
        for role in Role::ALL {
            if !self.passwords.contains_key(role.name()) {
                self.passwords.insert(role.name().to_owned(), random_password()?);
                changed = true;
            }
        }
        Ok(changed)
    }

    /// Reads `path`; `Ok(None)` when it does not exist.
    ///
    /// # Errors
    /// Unreadable or malformed file, or (Unix) a file other users can read.
    pub fn load(path: &Path) -> Result<Option<Self>> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(EngineError::io("read", path, e)),
        };
        fsutil::check_private_file(path)?;
        let mut passwords = BTreeMap::new();
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (role, pw) = line.split_once('=').ok_or_else(|| {
                EngineError::Config(format!("{} line {}: expected role=password", path.display(), n + 1))
            })?;
            let (role, pw) = (role.trim(), pw.trim());
            if role.is_empty() || pw.is_empty() {
                return Err(EngineError::Config(format!(
                    "{} line {}: empty role or password",
                    path.display(),
                    n + 1
                )));
            }
            passwords.insert(role.to_owned(), pw.to_owned());
        }
        Ok(Some(Self { passwords }))
    }

    /// Writes `path` atomically with mode 0600.
    ///
    /// # Errors
    /// File-system errors.
    pub fn save(&self, path: &Path) -> Result<()> {
        let mut text = String::from(HEADER);
        for (role, pw) in &self.passwords {
            text.push_str(role);
            text.push('=');
            text.push_str(pw);
            text.push('\n');
        }
        let tmp = path.with_extension("secret.tmp");
        {
            let mut f = fsutil::create_private_file(&tmp)?;
            f.write_all(text.as_bytes())
                .and_then(|()| f.sync_all())
                .map_err(|e| EngineError::io("write", &tmp, e))?;
        }
        std::fs::rename(&tmp, path).map_err(|e| EngineError::io("rename to", path, e))
    }
}

/// 32 random bytes as 64 lowercase hex characters (safe in URLs and SQL).
fn random_password() -> Result<String> {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).map_err(|e| EngineError::Config(format!("system random generator failed: {e}")))?;
    Ok(fsutil::hex(&buf))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_is_redacted() {
        let s = Secret::new("postgres://u:pw@localhost/db");
        assert_eq!(format!("{s:?}"), "Secret(<redacted>)");
        assert_eq!(s.to_string(), "<redacted>");
        assert_eq!(s.expose(), "postgres://u:pw@localhost/db");
    }

    #[test]
    fn roles_secret_round_trip_and_mode() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("roles.secret");
        assert!(RoleSecrets::load(&path).expect("load").is_none());
        let s = RoleSecrets::generate().expect("generate");
        for role in Role::ALL {
            let pw = s.password(role).expect("password");
            assert_eq!(pw.len(), 64);
            assert!(pw.bytes().all(|b| b.is_ascii_hexdigit()));
        }
        assert_ne!(s.password(Role::Server), s.password(Role::Reader));
        s.save(&path).expect("save");
        let back = RoleSecrets::load(&path).expect("load").expect("present");
        assert_eq!(back, s);
        assert!(!format!("{back:?}").contains(s.password(Role::Super).expect("pw")));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).expect("meta").permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn fill_missing_adds_new_roles_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("roles.secret");
        std::fs::write(&path, "# x\nforgebase_super = abc\n").expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
        }
        let mut s = RoleSecrets::load(&path).expect("load").expect("present");
        assert_eq!(s.password(Role::Super), Some("abc"));
        assert!(s.fill_missing().expect("fill"));
        assert_eq!(s.password(Role::Super), Some("abc"));
        assert!(s.password(Role::Reader).is_some());
        assert!(!s.fill_missing().expect("fill"));
    }

    #[cfg(unix)]
    #[test]
    fn world_readable_secret_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("roles.secret");
        std::fs::write(&path, "forgebase_super=abc\n").expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        let err = RoleSecrets::load(&path).expect_err("refused");
        assert!(err.to_string().contains("0600"), "{err}");
    }

    #[test]
    fn malformed_secret_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("roles.secret");
        std::fs::write(&path, "nonsense\n").expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
        }
        assert!(RoleSecrets::load(&path).is_err());
    }
}
