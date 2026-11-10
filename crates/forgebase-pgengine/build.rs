//! Embeds the engine archive when the `bundled-postgres` feature is on
//! (ADR-0028 §3.1). The archive comes from `FORGEBASE_PG_ENGINE_ARCHIVE`; its
//! SHA-256 must match `engine.lock` (or `FORGEBASE_PG_ENGINE_LOCK`). Nothing is
//! downloaded.

use std::path::PathBuf;

use sha2::{Digest, Sha256};

#[path = "src/lock.rs"]
mod lock;

fn main() {
    println!("cargo:rerun-if-env-changed=FORGEBASE_PG_ENGINE_ARCHIVE");
    println!("cargo:rerun-if-env-changed=FORGEBASE_PG_ENGINE_LOCK");
    if std::env::var_os("CARGO_FEATURE_BUNDLED_POSTGRES").is_none() {
        return;
    }
    if let Err(e) = embed() {
        panic!("forgebase-pgengine (feature bundled-postgres): {e}");
    }
}

fn embed() -> Result<(), String> {
    let archive = std::env::var_os("FORGEBASE_PG_ENGINE_ARCHIVE")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .ok_or(
            "set FORGEBASE_PG_ENGINE_ARCHIVE to the engine archive of this target \
             (a forgebase-pg*.tar.zst from the engine release), or build without the feature",
        )?;
    let archive = std::fs::canonicalize(&archive).map_err(|e| format!("{}: {e}", archive.display()))?;
    println!("cargo:rerun-if-changed={}", archive.display());
    let bytes = std::fs::read(&archive).map_err(|e| format!("read {}: {e}", archive.display()))?;
    let sha: [u8; 32] = Sha256::digest(&bytes).into();
    let sha: String = sha.iter().map(|b| format!("{b:02x}")).collect();

    let lock_path = match std::env::var_os("FORGEBASE_PG_ENGINE_LOCK").filter(|v| !v.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => {
            let manifest = std::env::var_os("CARGO_MANIFEST_DIR").ok_or("CARGO_MANIFEST_DIR is not set")?;
            PathBuf::from(manifest).join("../../engine.lock")
        }
    };
    println!("cargo:rerun-if-changed={}", lock_path.display());
    let lock_text = std::fs::read_to_string(&lock_path)
        .map_err(|e| format!("read {} (needed to verify the archive): {e}", lock_path.display()))?;
    let target = std::env::var("TARGET").map_err(|e| format!("TARGET: {e}"))?;
    lock::check_archive(&lock_text, &target, &sha)?;

    println!("cargo:rustc-env=FORGEBASE_PG_ENGINE_EMBED={}", archive.display());
    println!("cargo:rustc-env=FORGEBASE_PG_ENGINE_SHA256={sha}");
    Ok(())
}
