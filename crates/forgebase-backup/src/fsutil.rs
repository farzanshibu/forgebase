//! Blocking file helpers: hashed copies, tree walks, private creation, fsync.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::{Result, io};

pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// SHA-256 and size of a file.
pub fn sha256_file(path: &Path) -> Result<(String, u64)> {
    let mut f = File::open(path).map_err(io(path))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut size = 0u64;
    loop {
        let n = f.read(&mut buf).map_err(io(path))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        size += n as u64;
    }
    Ok((hex(&h.finalize()), size))
}

#[cfg(unix)]
pub(crate) fn create_private(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)
}

#[cfg(not(unix))]
pub(crate) fn create_private(path: &Path) -> std::io::Result<File> {
    fs::OpenOptions::new().write(true).create_new(true).open(path)
}

#[cfg(unix)]
pub(crate) fn mkdir_private(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().recursive(true).mode(0o700).create(path)
}

#[cfg(not(unix))]
pub(crate) fn mkdir_private(path: &Path) -> std::io::Result<()> {
    fs::create_dir_all(path)
}

/// Copies `src` to a new file `dst` (private mode), returning its hash and
/// size, and syncs it to disk.
pub(crate) fn copy_hashed(src: &Path, dst: &Path) -> Result<(String, u64)> {
    if let Some(parent) = dst.parent() {
        mkdir_private(parent).map_err(io(parent))?;
    }
    let mut r = File::open(src).map_err(io(src))?;
    let mut w = create_private(dst).map_err(io(dst))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut size = 0u64;
    loop {
        let n = r.read(&mut buf).map_err(io(src))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        w.write_all(&buf[..n]).map_err(io(dst))?;
        size += n as u64;
    }
    w.sync_all().map_err(io(dst))?;
    Ok((hex(&h.finalize()), size))
}

/// Regular files below `root`, as `/`-separated relative paths, sorted.
/// Symlinks are not followed or listed. Top-level entries named in `skip_top`
/// are left out. A missing `root` yields nothing.
pub(crate) fn walk_files(root: &Path, skip_top: &[&str]) -> Result<Vec<String>> {
    let mut out = Vec::new();
    if !root.is_dir() {
        return Ok(out);
    }
    let mut stack: Vec<(PathBuf, String)> = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, rel)) = stack.pop() {
        for entry in fs::read_dir(&dir).map_err(io(&dir))? {
            let entry = entry.map_err(io(&dir))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if rel.is_empty() && skip_top.contains(&name.as_str()) {
                continue;
            }
            let Ok(ty) = entry.file_type() else { continue };
            let child = if rel.is_empty() { name } else { format!("{rel}/{name}") };
            if ty.is_dir() {
                stack.push((entry.path(), child));
            } else if ty.is_file() {
                out.push(child);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// `a/b/c` joined onto `base` using the platform separator.
pub(crate) fn join_rel(base: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(base.to_path_buf(), |p, s| p.join(s))
}

/// Whether a manifest path is safe to join onto a directory.
pub(crate) fn safe_rel(rel: &str) -> bool {
    !rel.is_empty() && !rel.starts_with('/') && rel.split('/').all(|s| !s.is_empty() && s != "." && s != ".." && !s.contains('\\'))
}

/// Flushes a directory entry change to disk (best effort off Unix).
pub(crate) fn sync_dir(path: &Path) {
    if let Ok(d) = File::open(path) {
        let _ = d.sync_all();
    }
}

/// Writes `bytes` to `path` atomically: temp file, fsync, rename.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    let _ = fs::remove_file(&tmp);
    let mut f = create_private(&tmp).map_err(io(&tmp))?;
    f.write_all(bytes).map_err(io(&tmp))?;
    f.sync_all().map_err(io(&tmp))?;
    fs::rename(&tmp, path).map_err(io(path))?;
    if let Some(parent) = path.parent() {
        sync_dir(parent);
    }
    Ok(())
}
