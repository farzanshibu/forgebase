//! Engine archives: `tar` + `zstd` with an inner file manifest (ADR-0028
//! §3.1), extraction with verification, relocation and read-only files.
//!
//! An archive holds `bin/`, `lib/`, `share/` and, preferably as its first
//! entry, [`MANIFEST_NAME`]: a JSON [`Manifest`] listing every regular file
//! (path, size, SHA-256, mode) and every symbolic link. Everything may sit
//! under one top-level directory. Extraction refuses entries the manifest
//! does not list, files whose size or hash differ, links that leave the
//! engine directory, and any other entry type.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{EngineError, Result};
use crate::fsutil;
use crate::interp;

/// File name of the manifest inside an archive and an extracted engine.
pub const MANIFEST_NAME: &str = "forgebase-engine.json";

/// Manifest format this crate reads and writes.
pub const MANIFEST_FORMAT: u32 = 1;

/// The manifest inside an engine archive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Always [`MANIFEST_FORMAT`].
    pub format: u32,
    /// PostgreSQL version, `18.6`.
    pub postgres: String,
    /// Engine build of that version, `r1`.
    pub build: String,
    /// Rust target triple the engine is for.
    #[serde(default)]
    pub target: String,
    /// Linux: the musl loader shipped in the archive (`lib/ld-musl-x86_64.so.1`).
    /// When set, every ELF file in `bin/` is relocated to it (ADR-0028 §2.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loader: Option<String>,
    /// Prefix of the placeholder interpreter; default
    /// [`interp::PLACEHOLDER_PREFIX`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interp_placeholder: Option<String>,
    /// Feature flags of the build (`graph_cypher`, extension versions …).
    #[serde(default)]
    pub features: BTreeMap<String, serde_json::Value>,
    /// Every regular file.
    pub files: Vec<FileEntry>,
    /// Every symbolic link.
    #[serde(default)]
    pub links: Vec<LinkEntry>,
}

/// One regular file of the engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    /// Relative path with `/` separators.
    pub path: String,
    /// Size in bytes.
    pub size: u64,
    /// SHA-256, lowercase hex.
    pub sha256: String,
    /// Unix permission bits (write bits are dropped at extraction).
    pub mode: u32,
}

/// One symbolic link of the engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkEntry {
    /// Relative path with `/` separators.
    pub path: String,
    /// Relative link target, which must stay inside the engine.
    pub target: String,
}

impl Manifest {
    /// Directory name of the extracted engine: `<postgres>-<build>-<sha12>`.
    pub fn dir_name(&self, archive_sha256: &str) -> String {
        format!("{}-{}-{}", self.postgres, self.build, sha12(archive_sha256))
    }

    fn validate(&self) -> Result<()> {
        if self.format != MANIFEST_FORMAT {
            return Err(EngineError::Archive(format!(
                "manifest format {} is not supported (expected {MANIFEST_FORMAT}); \
                 this ForgeBase is older than the engine archive",
                self.format
            )));
        }
        let simple = |s: &str| {
            !s.is_empty() && s.len() <= 32 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_')
        };
        if !simple(&self.postgres) || !simple(&self.build) {
            return Err(EngineError::Archive(format!(
                "manifest version {:?} / build {:?} must be short and alphanumeric",
                self.postgres, self.build
            )));
        }
        let mut seen = BTreeSet::new();
        for f in &self.files {
            let p = rel_key(&f.path)?;
            if !seen.insert(p) {
                return Err(EngineError::Archive(format!("manifest lists {} twice", f.path)));
            }
            if f.sha256.len() != 64 || !f.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(EngineError::Archive(format!(
                    "manifest hash of {} is not SHA-256 hex",
                    f.path
                )));
            }
        }
        for l in &self.links {
            let p = rel_key(&l.path)?;
            if !seen.insert(p.clone()) {
                return Err(EngineError::Archive(format!("manifest lists {} twice", l.path)));
            }
            link_stays_inside(&p, &l.target)?;
        }
        if let Some(loader) = &self.loader {
            let key = rel_key(loader)?;
            if !self.files.iter().any(|f| rel_key(&f.path).is_ok_and(|p| p == key)) {
                return Err(EngineError::Archive(format!("loader {loader} is not in the manifest")));
            }
        }
        Ok(())
    }
}

/// First 12 hex characters of a SHA-256.
pub fn sha12(sha256: &str) -> &str {
    sha256.get(..12).unwrap_or(sha256)
}

/// A normalised relative path key (`a/b/c`), refusing absolute paths and `..`.
fn rel_key(p: &str) -> Result<String> {
    let mut parts = Vec::new();
    for c in Path::new(p).components() {
        match c {
            Component::Normal(x) => parts.push(
                x.to_str()
                    .ok_or_else(|| EngineError::Archive(format!("path {p:?} is not UTF-8")))?
                    .to_owned(),
            ),
            Component::CurDir => {}
            _ => {
                return Err(EngineError::Archive(format!(
                    "path {p:?} must be relative and stay inside"
                )));
            }
        }
    }
    if parts.is_empty() {
        return Err(EngineError::Archive(format!("empty path {p:?}")));
    }
    Ok(parts.join("/"))
}

/// Refuses a link whose target, resolved from the link's directory, leaves
/// the engine root.
fn link_stays_inside(link_key: &str, target: &str) -> Result<()> {
    if Path::new(target).is_absolute() || target.starts_with('/') || target.starts_with('\\') {
        return Err(EngineError::Archive(format!(
            "link {link_key} -> {target}: absolute target"
        )));
    }
    let mut depth: Vec<&str> = link_key.split('/').collect();
    depth.pop();
    for part in target.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => {
                if depth.pop().is_none() {
                    return Err(EngineError::Archive(format!(
                        "link {link_key} -> {target} leaves the engine directory"
                    )));
                }
            }
            p => depth.push(p),
        }
    }
    Ok(())
}

/// Where archive bytes come from (opened once per pass).
pub trait ArchiveSource {
    /// A fresh reader over the compressed archive.
    ///
    /// # Errors
    /// When the archive cannot be opened.
    fn open(&self) -> Result<Box<dyn Read + '_>>;
    /// SHA-256 of the compressed archive, lowercase hex.
    ///
    /// # Errors
    /// When the archive cannot be read.
    fn sha256(&self) -> Result<String>;
    /// For messages.
    fn describe(&self) -> String;
}

/// An archive in memory (the embedded engine).
#[derive(Debug, Clone, Copy)]
pub struct BytesSource<'a> {
    /// Compressed bytes.
    pub bytes: &'a [u8],
    /// Known SHA-256 (computed when `None`).
    pub sha256: Option<&'a str>,
}

impl ArchiveSource for BytesSource<'_> {
    fn open(&self) -> Result<Box<dyn Read + '_>> {
        Ok(Box::new(self.bytes))
    }
    fn sha256(&self) -> Result<String> {
        Ok(match self.sha256 {
            Some(s) => s.to_ascii_lowercase(),
            None => fsutil::sha256_hex(self.bytes),
        })
    }
    fn describe(&self) -> String {
        "the embedded engine archive".into()
    }
}

/// An archive file.
#[derive(Debug, Clone)]
pub struct FileSource(pub PathBuf);

impl ArchiveSource for FileSource {
    fn open(&self) -> Result<Box<dyn Read + '_>> {
        let f = std::fs::File::open(&self.0).map_err(|e| EngineError::io("open", &self.0, e))?;
        Ok(Box::new(std::io::BufReader::new(f)))
    }
    fn sha256(&self) -> Result<String> {
        fsutil::sha256_file(&self.0)
    }
    fn describe(&self) -> String {
        self.0.display().to_string()
    }
}

/// An extracted, verified engine directory.
#[derive(Debug, Clone)]
pub struct Extracted {
    /// The engine root (`bin/`, `lib/`, `share/`).
    pub dir: PathBuf,
    /// Its manifest.
    pub manifest: Manifest,
    /// Whether an earlier extraction was reused.
    pub reused: bool,
    /// Whether the long-path fallback put it under the cache directory.
    pub in_cache: bool,
}

/// Extracts `source` into `engines_dir/<postgres>-<build>-<sha12>/` unless an
/// extraction of the same archive exists there (or in the cache fallback).
///
/// Steps (ADR-0028 §3.1, §2.3): delete leftover `*.partial` directories;
/// read the manifest; choose the directory (the cache fallback when the
/// relocated loader path would exceed [`interp::MAX_INTERP_LEN`] bytes);
/// extract into `<dir>.partial`, verifying every entry; relocate; drop write
/// bits; rename into place.
///
/// # Errors
/// Malformed archives, verification failures, file-system errors.
pub fn extract(source: &dyn ArchiveSource, engines_dir: &Path, cache_dir: Option<&Path>) -> Result<Extracted> {
    let sha = source.sha256()?;
    let cache_engines = cache_dir.map(|c| c.join("engines"));
    let bases = std::iter::once((engines_dir, false)).chain(cache_engines.as_deref().map(|c| (c, true)));
    for (base, in_cache) in bases {
        if let Some(found) = find_extracted(base, &sha, in_cache)? {
            return Ok(found);
        }
    }
    fsutil::private_dir(engines_dir)?;
    clean_partials(engines_dir);

    let (manifest, manifest_bytes, prefix) = read_manifest(source)?;
    let name = manifest.dir_name(&sha);
    let mut dest = engines_dir.join(&name);
    let mut in_cache = false;
    if let Some(loader) = &manifest.loader
        && dest.join(loader).as_os_str().len() > interp::MAX_INTERP_LEN
    {
        let cache = cache_engines.ok_or_else(|| {
            EngineError::Archive(format!(
                "the engine path {} is too long for the loader and no cache directory is known (set HOME)",
                dest.display()
            ))
        })?;
        fsutil::private_dir(&cache)?;
        clean_partials(&cache);
        dest = cache.join(&name);
        if dest.join(loader).as_os_str().len() > interp::MAX_INTERP_LEN {
            return Err(EngineError::Archive(format!(
                "both {} and the cache path {} are too long for the loader (254 bytes)",
                engines_dir.display(),
                dest.display()
            )));
        }
        tracing::warn!(
            target: "forgebase::postgres",
            engine = %dest.display(),
            "data directory path is too long for the relocated engine; extracting it under the cache directory"
        );
        in_cache = true;
    }

    let partial = partial_path(&dest);
    let _ = fsutil::remove_tree(&partial);
    fsutil::private_dir(&partial)?;
    let result = unpack_verified(source, &manifest, &manifest_bytes, &prefix, &partial)
        .and_then(|()| relocate(&manifest, &partial, &dest))
        .and_then(|()| finish_modes(&manifest, &partial))
        .and_then(|()| {
            std::fs::write(partial.join(SHA_FILE), format!("{sha}\n"))
                .map_err(|e| EngineError::io("write", &partial.join(SHA_FILE), e))
        });
    if let Err(e) = result {
        let _ = fsutil::remove_tree(&partial);
        return Err(e);
    }
    match std::fs::rename(&partial, &dest) {
        Ok(()) => {}
        Err(_) if dest.join(MANIFEST_NAME).exists() => {
            // Extracted concurrently by someone else: keep theirs.
            let _ = fsutil::remove_tree(&partial);
        }
        Err(e) => {
            let _ = fsutil::remove_tree(&partial);
            return Err(EngineError::io("rename into place", &dest, e));
        }
    }
    tracing::info!(target: "forgebase::postgres", engine = %dest.display(), version = %manifest.postgres, "extracted PostgreSQL engine");
    Ok(Extracted {
        dir: dest,
        manifest,
        reused: false,
        in_cache,
    })
}

/// File inside an extracted engine recording the archive's SHA-256.
pub const SHA_FILE: &str = "forgebase-engine.sha256";

fn partial_path(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_owned();
    s.push(".partial");
    PathBuf::from(s)
}

fn clean_partials(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        if e.file_name().to_string_lossy().ends_with(".partial") {
            tracing::info!(target: "forgebase::postgres", path = %e.path().display(), "removing an interrupted engine extraction");
            let _ = fsutil::remove_tree(&e.path());
        }
    }
}

/// An earlier extraction of the archive with `sha` under `base`.
fn find_extracted(base: &Path, sha: &str, in_cache: bool) -> Result<Option<Extracted>> {
    let Ok(rd) = std::fs::read_dir(base) else {
        return Ok(None);
    };
    let suffix = format!("-{}", sha12(sha));
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.ends_with(&suffix) {
            continue;
        }
        let dir = e.path();
        let recorded = std::fs::read_to_string(dir.join(SHA_FILE)).unwrap_or_default();
        if recorded.trim() != sha {
            continue;
        }
        let text = std::fs::read(dir.join(MANIFEST_NAME)).map_err(|e| EngineError::io("read", &dir, e))?;
        let manifest: Manifest = serde_json::from_slice(&text)
            .map_err(|e| EngineError::Archive(format!("{}: {e}", dir.join(MANIFEST_NAME).display())))?;
        return Ok(Some(Extracted {
            in_cache,
            dir,
            manifest,
            reused: true,
        }));
    }
    Ok(None)
}

/// Pass 1: the manifest, its raw bytes, and the top-level prefix it sits under.
fn read_manifest(source: &dyn ArchiveSource) -> Result<(Manifest, Vec<u8>, String)> {
    let reader = source.open()?;
    let decoder = zstd::stream::read::Decoder::new(reader)
        .map_err(|e| EngineError::Archive(format!("{}: not a zstd stream: {e}", source.describe())))?;
    let mut tar = tar::Archive::new(decoder);
    let entries = tar
        .entries()
        .map_err(|e| EngineError::Archive(format!("{}: {e}", source.describe())))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| EngineError::Archive(format!("{}: {e}", source.describe())))?;
        let path = entry
            .path()
            .map_err(|e| EngineError::Archive(format!("entry path: {e}")))?
            .to_string_lossy()
            .into_owned();
        let key = rel_key(&path)?;
        let prefix = match key.rsplit_once('/') {
            Some((p, MANIFEST_NAME)) if !p.contains('/') => format!("{p}/"),
            None if key == MANIFEST_NAME => String::new(),
            _ => continue,
        };
        let mut bytes = Vec::new();
        entry
            .read_to_end(&mut bytes)
            .map_err(|e| EngineError::Archive(format!("read the manifest: {e}")))?;
        let manifest: Manifest =
            serde_json::from_slice(&bytes).map_err(|e| EngineError::Archive(format!("manifest: {e}")))?;
        manifest.validate()?;
        return Ok((manifest, bytes, prefix));
    }
    Err(EngineError::Archive(format!(
        "{} has no {MANIFEST_NAME}",
        source.describe()
    )))
}

/// Pass 2: every entry, checked against the manifest.
fn unpack_verified(
    source: &dyn ArchiveSource,
    manifest: &Manifest,
    manifest_bytes: &[u8],
    prefix: &str,
    root: &Path,
) -> Result<()> {
    let files: BTreeMap<String, &FileEntry> = manifest
        .files
        .iter()
        .map(|f| rel_key(&f.path).map(|k| (k, f)))
        .collect::<Result<_>>()?;
    let links: BTreeMap<String, &LinkEntry> = manifest
        .links
        .iter()
        .map(|l| rel_key(&l.path).map(|k| (k, l)))
        .collect::<Result<_>>()?;
    let mut seen = BTreeSet::new();

    let reader = source.open()?;
    let decoder = zstd::stream::read::Decoder::new(reader).map_err(|e| EngineError::Archive(e.to_string()))?;
    let mut tar = tar::Archive::new(decoder);
    for entry in tar.entries().map_err(|e| EngineError::Archive(e.to_string()))? {
        let mut entry = entry.map_err(|e| EngineError::Archive(e.to_string()))?;
        let raw = entry
            .path()
            .map_err(|e| EngineError::Archive(format!("entry path: {e}")))?
            .to_string_lossy()
            .into_owned();
        let full = rel_key(&raw)?;
        let key = if prefix.is_empty() {
            full
        } else if let Some(rest) = full.strip_prefix(prefix) {
            rest.to_owned()
        } else if format!("{full}/") == prefix {
            continue; // the top-level directory itself
        } else {
            return Err(EngineError::Archive(format!("{raw} lies outside {prefix}")));
        };
        let out = root.join(&key);
        match entry.header().entry_type() {
            tar::EntryType::Directory => {
                std::fs::create_dir_all(&out).map_err(|e| EngineError::io("create directory", &out, e))?;
            }
            tar::EntryType::Regular | tar::EntryType::Continuous => {
                if key == MANIFEST_NAME {
                    let mut bytes = Vec::new();
                    entry
                        .read_to_end(&mut bytes)
                        .map_err(|e| EngineError::Archive(format!("read the manifest: {e}")))?;
                    if bytes != manifest_bytes {
                        return Err(EngineError::Archive("the archive holds two different manifests".into()));
                    }
                    std::fs::write(&out, &bytes).map_err(|e| EngineError::io("write", &out, e))?;
                    continue;
                }
                let want = files
                    .get(&key)
                    .ok_or_else(|| EngineError::Archive(format!("{key} is not in the manifest")))?;
                if !seen.insert(key.clone()) {
                    return Err(EngineError::Archive(format!("{key} appears twice")));
                }
                if let Some(parent) = out.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| EngineError::io("create directory", parent, e))?;
                }
                let mut file = std::fs::File::create(&out).map_err(|e| EngineError::io("create", &out, e))?;
                let mut hasher = Sha256::new();
                let mut size = 0u64;
                let mut buf = vec![0u8; 1 << 16];
                loop {
                    let n = entry
                        .read(&mut buf)
                        .map_err(|e| EngineError::Archive(format!("read {key}: {e}")))?;
                    if n == 0 {
                        break;
                    }
                    size += n as u64;
                    hasher.update(&buf[..n]);
                    file.write_all(&buf[..n])
                        .map_err(|e| EngineError::io("write", &out, e))?;
                }
                file.sync_all().map_err(|e| EngineError::io("sync", &out, e))?;
                let digest: [u8; 32] = hasher.finalize().into();
                let got = fsutil::hex(&digest);
                if size != want.size || !got.eq_ignore_ascii_case(&want.sha256) {
                    return Err(EngineError::Archive(format!(
                        "{key} does not match the manifest (size {size} vs {}, sha256 {got} vs {})",
                        want.size, want.sha256
                    )));
                }
            }
            tar::EntryType::Symlink => {
                let want = links
                    .get(&key)
                    .ok_or_else(|| EngineError::Archive(format!("link {key} is not in the manifest")))?;
                let target = entry
                    .link_name()
                    .map_err(|e| EngineError::Archive(format!("link {key}: {e}")))?
                    .ok_or_else(|| EngineError::Archive(format!("link {key} has no target")))?
                    .to_string_lossy()
                    .into_owned();
                if target != want.target {
                    return Err(EngineError::Archive(format!(
                        "link {key} -> {target}, the manifest says {}",
                        want.target
                    )));
                }
                link_stays_inside(&key, &target)?;
                if !seen.insert(key.clone()) {
                    return Err(EngineError::Archive(format!("{key} appears twice")));
                }
                if let Some(parent) = out.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| EngineError::io("create directory", parent, e))?;
                }
                make_symlink(&target, &out)?;
            }
            other => {
                return Err(EngineError::Archive(format!(
                    "{key}: entry type {other:?} is not allowed in an engine archive"
                )));
            }
        }
    }
    let missing: Vec<&str> = files
        .keys()
        .chain(links.keys())
        .filter(|k| !seen.contains(*k))
        .map(String::as_str)
        .collect();
    if !missing.is_empty() {
        return Err(EngineError::Archive(format!(
            "the archive lacks files its manifest lists: {}",
            missing.join(", ")
        )));
    }
    if !root.join(MANIFEST_NAME).exists() {
        std::fs::write(root.join(MANIFEST_NAME), manifest_bytes)
            .map_err(|e| EngineError::io("write", &root.join(MANIFEST_NAME), e))?;
    }
    Ok(())
}

#[cfg(unix)]
fn make_symlink(target: &str, link: &Path) -> Result<()> {
    std::os::unix::fs::symlink(target, link).map_err(|e| EngineError::io("create link", link, e))
}

#[cfg(not(unix))]
fn make_symlink(target: &str, link: &Path) -> Result<()> {
    Err(EngineError::Archive(format!(
        "{} -> {target}: engine archives for this platform must not contain links",
        link.display()
    )))
}

/// Points every ELF executable in `bin/` at the extracted loader.
fn relocate(manifest: &Manifest, partial: &Path, dest: &Path) -> Result<()> {
    let Some(loader) = &manifest.loader else {
        return Ok(());
    };
    let loader_abs = dest.join(rel_key(loader)?);
    let loader_abs = loader_abs
        .to_str()
        .ok_or_else(|| EngineError::Archive(format!("loader path {} is not UTF-8", loader_abs.display())))?;
    let placeholder = manifest
        .interp_placeholder
        .as_deref()
        .unwrap_or(interp::PLACEHOLDER_PREFIX);
    let mut patched = 0;
    for f in &manifest.files {
        let key = rel_key(&f.path)?;
        if !key.starts_with("bin/") {
            continue;
        }
        if interp::patch_file(&partial.join(&key), loader_abs, placeholder)? == interp::Patched::Yes {
            patched += 1;
        }
    }
    tracing::debug!(target: "forgebase::postgres", patched, loader = loader_abs, "relocated engine executables");
    Ok(())
}

/// Drops write bits: files become read-only (ADR-0028 §3.1).
fn finish_modes(manifest: &Manifest, root: &Path) -> Result<()> {
    for f in &manifest.files {
        let path = root.join(rel_key(&f.path)?);
        #[cfg(unix)]
        fsutil::set_mode(&path, (f.mode & 0o777 & !0o222) | 0o400)?;
        #[cfg(not(unix))]
        {
            let mut perm = std::fs::metadata(&path)
                .map_err(|e| EngineError::io("stat", &path, e))?
                .permissions();
            perm.set_readonly(true);
            std::fs::set_permissions(&path, perm).map_err(|e| EngineError::io("chmod", &path, e))?;
        }
    }
    let m = root.join(MANIFEST_NAME);
    fsutil::set_mode(&m, 0o444)
}

/// Re-checks an extracted engine against its manifest (for `doctor`).
/// Relocated executables are checked by size only, since relocation changed
/// their bytes.
///
/// # Errors
/// The first difference found, or I/O errors.
pub fn verify_extracted(dir: &Path) -> Result<Manifest> {
    let text = std::fs::read(dir.join(MANIFEST_NAME)).map_err(|e| EngineError::io("read", dir, e))?;
    let manifest: Manifest =
        serde_json::from_slice(&text).map_err(|e| EngineError::Archive(format!("manifest: {e}")))?;
    manifest.validate()?;
    for f in &manifest.files {
        let key = rel_key(&f.path)?;
        let path = dir.join(&key);
        let meta = std::fs::metadata(&path).map_err(|e| EngineError::io("stat", &path, e))?;
        if meta.len() != f.size {
            return Err(EngineError::Archive(format!(
                "{key}: size {} vs {}",
                meta.len(),
                f.size
            )));
        }
        if manifest.loader.is_some() && key.starts_with("bin/") {
            continue;
        }
        let got = fsutil::sha256_file(&path)?;
        if !got.eq_ignore_ascii_case(&f.sha256) {
            return Err(EngineError::Archive(format!("{key}: sha256 {got} vs {}", f.sha256)));
        }
    }
    Ok(manifest)
}

/// What [`pack_dir`] records about the engine.
#[derive(Debug, Clone, Default)]
pub struct PackInfo {
    /// PostgreSQL version.
    pub postgres: String,
    /// Build of that version.
    pub build: String,
    /// Target triple.
    pub target: String,
    /// Relative path of the musl loader (Linux).
    pub loader: Option<String>,
    /// Feature flags.
    pub features: BTreeMap<String, serde_json::Value>,
}

/// Packs `paths` (files or directories, relative to `src`) into a `tar.zst`
/// engine archive with its manifest first. Symbolic links are kept as links.
/// Used by tests and engine tooling.
///
/// # Errors
/// I/O errors, paths outside `src`, links that leave the engine.
pub fn pack_dir(src: &Path, paths: &[&str], info: &PackInfo, out: impl Write, level: i32) -> Result<Manifest> {
    let mut items: Vec<(String, PathBuf)> = Vec::new();
    for p in paths {
        collect(src, &rel_key(p)?, &mut items)?;
    }
    items.sort();
    items.dedup();
    let mut files = Vec::new();
    let mut links = Vec::new();
    for (key, path) in &items {
        let meta = std::fs::symlink_metadata(path).map_err(|e| EngineError::io("stat", path, e))?;
        if meta.file_type().is_symlink() {
            let target = std::fs::read_link(path).map_err(|e| EngineError::io("read link", path, e))?;
            let target = target.to_string_lossy().into_owned();
            link_stays_inside(key, &target)?;
            links.push(LinkEntry {
                path: key.clone(),
                target,
            });
        } else {
            files.push(FileEntry {
                path: key.clone(),
                size: meta.len(),
                sha256: fsutil::sha256_file(path)?,
                mode: file_mode(&meta),
            });
        }
    }
    let manifest = Manifest {
        format: MANIFEST_FORMAT,
        postgres: info.postgres.clone(),
        build: info.build.clone(),
        target: info.target.clone(),
        loader: info.loader.clone(),
        interp_placeholder: None,
        features: info.features.clone(),
        files,
        links,
    };
    manifest.validate()?;
    let manifest_bytes =
        serde_json::to_vec_pretty(&manifest).map_err(|e| EngineError::Archive(format!("manifest: {e}")))?;

    let io_err = |e: std::io::Error| EngineError::Archive(format!("write archive: {e}"));
    let enc = zstd::stream::write::Encoder::new(out, level).map_err(io_err)?;
    let mut tar = tar::Builder::new(enc);
    let mut h = tar::Header::new_gnu();
    h.set_size(manifest_bytes.len() as u64);
    h.set_mode(0o644);
    h.set_entry_type(tar::EntryType::Regular);
    tar.append_data(&mut h, MANIFEST_NAME, manifest_bytes.as_slice())
        .map_err(io_err)?;
    for f in &manifest.files {
        let path = src.join(&f.path);
        let file = std::fs::File::open(&path).map_err(|e| EngineError::io("open", &path, e))?;
        let mut h = tar::Header::new_gnu();
        h.set_size(f.size);
        h.set_mode(f.mode);
        h.set_entry_type(tar::EntryType::Regular);
        tar.append_data(&mut h, &f.path, file).map_err(io_err)?;
    }
    for l in &manifest.links {
        let mut h = tar::Header::new_gnu();
        h.set_size(0);
        h.set_mode(0o777);
        h.set_entry_type(tar::EntryType::Symlink);
        tar.append_link(&mut h, &l.path, &l.target).map_err(io_err)?;
    }
    let enc = tar.into_inner().map_err(io_err)?;
    enc.finish().map_err(io_err)?;
    Ok(manifest)
}

fn collect(src: &Path, key: &str, out: &mut Vec<(String, PathBuf)>) -> Result<()> {
    let path = src.join(key);
    let meta = std::fs::symlink_metadata(&path).map_err(|e| EngineError::io("stat", &path, e))?;
    if meta.is_dir() {
        let rd = std::fs::read_dir(&path).map_err(|e| EngineError::io("list", &path, e))?;
        for e in rd {
            let e = e.map_err(|e| EngineError::io("list", &path, e))?;
            let name = e.file_name();
            let name = name
                .to_str()
                .ok_or_else(|| EngineError::Archive(format!("{} is not UTF-8", e.path().display())))?;
            collect(src, &format!("{key}/{name}"), out)?;
        }
    } else {
        out.push((key.to_owned(), path));
    }
    Ok(())
}

#[cfg(unix)]
fn file_mode(meta: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o777
}

#[cfg(not(unix))]
fn file_mode(_meta: &std::fs::Metadata) -> u32 {
    0o755
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(dir: &Path) -> PathBuf {
        let src = dir.join("src");
        std::fs::create_dir_all(src.join("bin")).expect("mkdir");
        std::fs::create_dir_all(src.join("lib")).expect("mkdir");
        std::fs::write(src.join("bin/postgres"), b"#!/bin/sh\necho postgres\n").expect("write");
        std::fs::write(src.join("lib/libpq.so.5.18"), vec![7u8; 100_000]).expect("write");
        fsutil::set_mode(&src.join("bin/postgres"), 0o755).expect("chmod");
        #[cfg(unix)]
        std::os::unix::fs::symlink("libpq.so.5.18", src.join("lib/libpq.so.5")).expect("link");
        src
    }

    fn info() -> PackInfo {
        PackInfo {
            postgres: "18.3".into(),
            build: "t1".into(),
            target: "test".into(),
            ..PackInfo::default()
        }
    }

    fn pack(src: &Path) -> Vec<u8> {
        let mut out = Vec::new();
        pack_dir(src, &["bin", "lib"], &info(), &mut out, 1).expect("pack");
        out
    }

    #[test]
    fn pack_extract_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let src = fixture(dir.path());
        let bytes = pack(&src);
        let engines = dir.path().join("engine");
        let src_bytes = BytesSource {
            bytes: &bytes,
            sha256: None,
        };
        let ex = extract(&src_bytes, &engines, None).expect("extract");
        assert!(!ex.reused);
        let sha = fsutil::sha256_hex(&bytes);
        assert_eq!(ex.dir, engines.join(format!("18.3-t1-{}", &sha[..12])));
        assert_eq!(
            std::fs::read(ex.dir.join("bin/postgres")).expect("read"),
            b"#!/bin/sh\necho postgres\n"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &str| std::fs::metadata(ex.dir.join(p)).expect("meta").permissions().mode() & 0o777;
            assert_eq!(mode("bin/postgres"), 0o555);
            assert_eq!(mode("lib/libpq.so.5.18") & 0o222, 0);
            assert_eq!(
                std::fs::read_link(ex.dir.join("lib/libpq.so.5")).expect("link"),
                Path::new("libpq.so.5.18")
            );
        }
        verify_extracted(&ex.dir).expect("verify");
        let again = extract(&src_bytes, &engines, None).expect("reuse");
        assert!(again.reused);
        assert_eq!(again.dir, ex.dir);
        assert!(
            std::fs::read_dir(&engines)
                .expect("list")
                .flatten()
                .all(|e| !e.file_name().to_string_lossy().ends_with(".partial"))
        );
    }

    #[test]
    fn leftover_partial_is_removed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let src = fixture(dir.path());
        let bytes = pack(&src);
        let engines = dir.path().join("engine");
        std::fs::create_dir_all(engines.join("18.2-r1-000000000000.partial/bin")).expect("mkdir");
        let ex = extract(
            &BytesSource {
                bytes: &bytes,
                sha256: None,
            },
            &engines,
            None,
        )
        .expect("extract");
        assert!(!engines.join("18.2-r1-000000000000.partial").exists());
        assert!(ex.dir.exists());
    }

    /// Rewrites an archive with `edit` applied to each (path, data) entry.
    fn rewrite(bytes: &[u8], edit: impl Fn(&str, Vec<u8>) -> Option<Vec<u8>>, extra: Option<(&str, &[u8])>) -> Vec<u8> {
        let dec = zstd::stream::read::Decoder::new(bytes).expect("decoder");
        let mut ar = tar::Archive::new(dec);
        let mut out = Vec::new();
        {
            let enc = zstd::stream::write::Encoder::new(&mut out, 1).expect("enc");
            let mut b = tar::Builder::new(enc);
            for e in ar.entries().expect("entries") {
                let mut e = e.expect("entry");
                let path = e.path().expect("path").to_string_lossy().into_owned();
                let mut h = e.header().clone();
                if h.entry_type() == tar::EntryType::Symlink {
                    let target = e.link_name().expect("ln").expect("target").into_owned();
                    b.append_link(&mut h, &path, target).expect("link");
                    continue;
                }
                let mut data = Vec::new();
                e.read_to_end(&mut data).expect("read");
                if let Some(data) = edit(&path, data) {
                    h.set_size(data.len() as u64);
                    h.set_cksum();
                    b.append_data(&mut h, &path, data.as_slice()).expect("append");
                }
            }
            if let Some((p, d)) = extra {
                let mut h = tar::Header::new_gnu();
                h.set_size(d.len() as u64);
                h.set_mode(0o644);
                b.append_data(&mut h, p, d).expect("append");
            }
            b.into_inner().expect("inner").finish().expect("finish");
        }
        out
    }

    fn extract_err(bytes: &[u8]) -> String {
        let dir = tempfile::tempdir().expect("tempdir");
        let engines = dir.path().join("engine");
        let err = extract(&BytesSource { bytes, sha256: None }, &engines, None).expect_err("must fail");
        // Nothing half-extracted stays behind.
        let left: Vec<_> = std::fs::read_dir(&engines)
            .map(|rd| rd.flatten().map(|e| e.file_name()).collect())
            .unwrap_or_default();
        assert!(left.is_empty(), "left behind: {left:?}");
        err.to_string()
    }

    #[test]
    fn tampered_file_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bytes = pack(&fixture(dir.path()));
        let bad = rewrite(
            &bytes,
            |p, mut d| {
                if p == "bin/postgres" {
                    d[0] = b'X';
                }
                Some(d)
            },
            None,
        );
        assert!(extract_err(&bad).contains("does not match the manifest"));
    }

    #[test]
    fn missing_and_extra_files_are_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bytes = pack(&fixture(dir.path()));
        let missing = rewrite(&bytes, |p, d| (p != "lib/libpq.so.5.18").then_some(d), None);
        assert!(extract_err(&missing).contains("lacks files"));
        let extra = rewrite(&bytes, |_, d| Some(d), Some(("bin/evil", b"x")));
        assert!(extract_err(&extra).contains("not in the manifest"));
    }

    #[test]
    fn manifest_must_exist_and_be_valid() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bytes = pack(&fixture(dir.path()));
        let none = rewrite(&bytes, |p, d| (p != MANIFEST_NAME).then_some(d), None);
        assert!(extract_err(&none).contains("has no"));
        let future = rewrite(
            &bytes,
            |p, d| {
                if p == MANIFEST_NAME {
                    let mut m: serde_json::Value = serde_json::from_slice(&d).expect("json");
                    m["format"] = 2.into();
                    Some(serde_json::to_vec(&m).expect("json"))
                } else {
                    Some(d)
                }
            },
            None,
        );
        assert!(extract_err(&future).contains("format 2"));
    }

    #[test]
    fn paths_and_links_must_stay_inside() {
        assert!(rel_key("../x").is_err());
        assert!(rel_key("/etc/passwd").is_err());
        assert!(rel_key("a/../../x").is_err());
        assert_eq!(rel_key("./bin/postgres").expect("ok"), "bin/postgres");
        assert!(link_stays_inside("lib/libpq.so", "libpq.so.5").is_ok());
        assert!(link_stays_inside("lib/a/b", "../c").is_ok());
        assert!(link_stays_inside("lib/x", "../../etc/passwd").is_err());
        assert!(link_stays_inside("lib/x", "/etc/passwd").is_err());
    }

    #[test]
    fn top_level_directory_is_stripped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bytes = pack(&fixture(dir.path()));
        let nested = rewrite(&bytes, |_, d| Some(d), None);
        // Re-pack everything under "engine-root/".
        let dec = zstd::stream::read::Decoder::new(nested.as_slice()).expect("dec");
        let mut ar = tar::Archive::new(dec);
        let mut out = Vec::new();
        {
            let enc = zstd::stream::write::Encoder::new(&mut out, 1).expect("enc");
            let mut b = tar::Builder::new(enc);
            for e in ar.entries().expect("entries") {
                let mut e = e.expect("entry");
                let path = format!("engine-root/{}", e.path().expect("path").to_string_lossy());
                let mut h = e.header().clone();
                if h.entry_type() == tar::EntryType::Symlink {
                    let t = e.link_name().expect("ln").expect("t").into_owned();
                    b.append_link(&mut h, &path, t).expect("link");
                } else {
                    let mut d = Vec::new();
                    e.read_to_end(&mut d).expect("read");
                    b.append_data(&mut h, &path, d.as_slice()).expect("append");
                }
            }
            b.into_inner().expect("inner").finish().expect("finish");
        }
        let engines = dir.path().join("engine");
        let ex = extract(
            &BytesSource {
                bytes: &out,
                sha256: None,
            },
            &engines,
            None,
        )
        .expect("extract");
        assert!(ex.dir.join("bin/postgres").exists());
    }

    #[test]
    fn linux_engine_is_relocated_and_long_paths_fall_back() {
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("src");
        std::fs::create_dir_all(src.join("bin")).expect("mkdir");
        std::fs::create_dir_all(src.join("lib")).expect("mkdir");
        std::fs::write(
            src.join("bin/postgres"),
            crate::interp::tests::fake_elf(&crate::interp::tests::placeholder()),
        )
        .expect("write");
        std::fs::write(src.join("bin/initdb"), b"not an elf").expect("write");
        std::fs::write(src.join("lib/ld-musl-x86_64.so.1"), b"loader").expect("write");
        let mut out = Vec::new();
        let info = PackInfo {
            loader: Some("lib/ld-musl-x86_64.so.1".into()),
            ..info()
        };
        pack_dir(&src, &["bin", "lib"], &info, &mut out, 1).expect("pack");
        let source = BytesSource {
            bytes: &out,
            sha256: None,
        };

        let engines = dir.path().join("short/engine");
        let ex = extract(&source, &engines, None).expect("extract");
        let data = std::fs::read(ex.dir.join("bin/postgres")).expect("read");
        let want = ex.dir.join("lib/ld-musl-x86_64.so.1");
        assert_eq!(
            crate::interp::read_interp(&data).expect("interp").as_deref(),
            want.to_str()
        );
        assert!(!ex.in_cache);
        verify_extracted(&ex.dir).expect("verify after relocation");

        // A data directory so deep that the loader path cannot fit.
        let deep = dir.path().join("d".repeat(200)).join("engine");
        assert!(extract(&source, &deep, None).is_err());
        let cache = dir.path().join("cache");
        let ex = extract(&source, &deep, Some(&cache)).expect("fallback");
        assert!(ex.in_cache);
        assert!(ex.dir.starts_with(cache.join("engines")));
        let data = std::fs::read(ex.dir.join("bin/postgres")).expect("read");
        assert!(
            crate::interp::read_interp(&data)
                .expect("interp")
                .is_some_and(|i| i.starts_with(cache.to_str().expect("utf8")))
        );
        // Found again on the next start.
        let again = extract(&source, &deep, Some(&cache)).expect("reuse");
        assert!(again.reused && again.in_cache);
    }
}
