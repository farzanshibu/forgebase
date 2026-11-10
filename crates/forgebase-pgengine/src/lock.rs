//! `engine.lock` lookup shared by `build.rs` and the library tests.
//!
//! `engine.lock` (FB-130, repository root) pins the engine release. The part
//! this crate reads is one table per target triple:
//!
//! ```toml
//! [engine]
//! postgres = "18.6"
//! build = "r1"
//!
//! [archives."aarch64-apple-darwin"]
//! file = "forgebase-pg18.6-r1-aarch64-apple-darwin.tar.zst"
//! sha256 = "<64 hex>"
//! ```
//!
//! Every other table (source tarballs, extensions) is ignored here.

/// The SHA-256 (lowercase hex) `engine.lock` pins for `target`, checked
/// against `actual` (the archive being embedded).
///
/// # Errors
/// A message for the build log: unreadable lock, no entry for the target, or
/// a different hash (naming the target the hash belongs to, when known).
pub fn check_archive(lock_text: &str, target: &str, actual: &str) -> Result<(), String> {
    let doc: toml::Table = lock_text
        .parse()
        .map_err(|e| format!("engine.lock is not valid TOML: {e}"))?;
    let archives = doc
        .get("archives")
        .and_then(toml::Value::as_table)
        .ok_or("engine.lock has no [archives] table")?;
    let sha_of = |entry: &toml::Value| {
        entry
            .get("sha256")
            .and_then(toml::Value::as_str)
            .map(|s| s.trim().to_ascii_lowercase())
    };
    let actual = actual.to_ascii_lowercase();
    match archives.get(target).and_then(sha_of) {
        Some(pinned) if pinned == actual => Ok(()),
        Some(pinned) => Err(format!(
            "engine archive SHA-256 {actual} does not match engine.lock ({pinned}) for target {target}"
        )),
        None => {
            let other = archives
                .iter()
                .find(|(_, v)| sha_of(v).as_deref() == Some(actual.as_str()))
                .map(|(k, _)| k.clone());
            Err(match other {
                Some(t) => format!("engine archive is pinned for target {t}, but this build is for {target}"),
                None => format!("engine.lock has no archive entry for target {target}"),
            })
        }
    }
}
