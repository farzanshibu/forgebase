//! Small file-system helpers: private directories and files, hex, SHA-256.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::error::{EngineError, Result};

/// Lowercase hex.
pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// SHA-256 of a byte slice, hex.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let d: [u8; 32] = Sha256::digest(bytes).into();
    hex(&d)
}

/// SHA-256 of a file, hex (streamed).
pub(crate) fn sha256_file(path: &Path) -> Result<String> {
    let mut f = File::open(path).map_err(|e| EngineError::io("open", path, e))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf).map_err(|e| EngineError::io("read", path, e))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    let d: [u8; 32] = h.finalize().into();
    Ok(hex(&d))
}

/// Creates `dir` (and parents) and makes it private to the user (0700 on Unix).
pub(crate) fn private_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).map_err(|e| EngineError::io("create directory", dir, e))?;
    set_mode(dir, 0o700)
}

/// Creates (truncates) a file readable only by the user (0600 on Unix).
pub(crate) fn create_private_file(path: &Path) -> Result<File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let f = opts.open(path).map_err(|e| EngineError::io("create", path, e))?;
    set_mode(path, 0o600)?;
    Ok(f)
}

/// Fails unless `path` is private to the user (no group or other bits on Unix).
pub(crate) fn check_private_file(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path)
            .map_err(|e| EngineError::io("stat", path, e))?
            .permissions()
            .mode();
        if mode & 0o077 != 0 {
            return Err(EngineError::Config(format!(
                "{} must be readable by its owner only (chmod 0600), it is {:04o}",
                path.display(),
                mode & 0o777
            )));
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Sets Unix permission bits (no-op elsewhere).
pub(crate) fn set_mode(path: &Path, mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .map_err(|e| EngineError::io("chmod", path, e))?;
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
    Ok(())
}

/// Whether extraction must sync each file as it writes it. Only platforms
/// without the single barrier of [`sync_tree`] do (Windows: `FlushFileBuffers`
/// needs a writable handle, and the files are read-only once finished).
pub(crate) const SYNC_EACH_FILE: bool = !cfg!(any(target_os = "linux", target_os = "android", target_vendor = "apple"));

/// Makes a freshly written tree durable before the caller renames it into
/// place (FB-148): `files` are its regular files, relative to `root`.
///
/// One barrier for the whole tree instead of `sync_all` per file, which on
/// macOS is `F_FULLFSYNC` (a full flush of the drive's cache) and took about
/// 4 minutes for the 938 files of an engine on an external SSD:
/// - Linux: one `syncfs` of the file system holding `root`;
/// - macOS: a plain `fsync` of every file and directory (data reaches the
///   drive), then one `F_FULLFSYNC` (the drive's cache reaches the medium);
/// - elsewhere: nothing here, the files were synced as they were written
///   ([`SYNC_EACH_FILE`]).
pub(crate) fn sync_tree(root: &Path, files: &[&str]) -> Result<()> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let _ = files;
        let dir = File::open(root).map_err(|e| EngineError::io("open", root, e))?;
        rustix::fs::syncfs(&dir).map_err(|e| EngineError::io("sync", root, e.into()))?;
    }
    #[cfg(target_vendor = "apple")]
    {
        let fsync = |path: &Path| -> Result<()> {
            let f = File::open(path).map_err(|e| EngineError::io("open", path, e))?;
            rustix::fs::fsync(&f).map_err(|e| EngineError::io("sync", path, e.into()))
        };
        let mut dirs = std::collections::BTreeSet::new();
        for rel in files {
            let path = root.join(rel);
            fsync(&path)?;
            let mut parent = path.parent();
            while let Some(p) = parent.filter(|p| p.starts_with(root)) {
                if !dirs.insert(p.to_path_buf()) {
                    break;
                }
                parent = p.parent();
            }
        }
        for d in dirs.iter().rev() {
            fsync(d)?;
        }
        let f = File::open(root).map_err(|e| EngineError::io("open", root, e))?;
        rustix::fs::fcntl_fullfsync(&f).map_err(|e| EngineError::io("sync", root, e.into()))?;
    }
    #[cfg(not(any(target_os = "linux", target_os = "android", target_vendor = "apple")))]
    let _ = (root, files);
    Ok(())
}

/// Syncs a directory's entries (after a rename into it). Best effort where
/// directories cannot be opened (Windows).
pub(crate) fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// Removes a directory tree, clearing read-only flags first where the
/// platform needs it (Windows refuses to delete read-only files).
pub(crate) fn remove_tree(dir: &Path) -> io::Result<()> {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(first) => {
            clear_readonly(dir)?;
            std::fs::remove_dir_all(dir).map_err(|_| first)
        }
    }
}

fn clear_readonly(dir: &Path) -> io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let ft = entry.file_type()?;
        if ft.is_dir() {
            clear_readonly(&path)?;
        }
        if !ft.is_symlink() {
            let mut perm = std::fs::metadata(&path)?.permissions();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                perm.set_mode(perm.mode() | 0o700);
            }
            #[cfg(not(unix))]
            #[allow(clippy::permissions_set_readonly_false)]
            perm.set_readonly(false);
            std::fs::set_permissions(&path, perm)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_and_sha() {
        assert_eq!(hex(&[0, 15, 255]), "000fff");
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("f");
        std::fs::write(&p, b"abc").expect("write");
        assert_eq!(sha256_file(&p).expect("sha"), sha256_hex(b"abc"));
    }

    #[test]
    fn remove_tree_handles_read_only_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tree = dir.path().join("t");
        std::fs::create_dir_all(tree.join("a")).expect("mkdir");
        let f = tree.join("a/f");
        std::fs::write(&f, b"x").expect("write");
        set_mode(&f, 0o444).expect("chmod");
        remove_tree(&tree).expect("remove");
        assert!(!tree.exists());
        remove_tree(&tree).expect("missing is fine");
    }
}
