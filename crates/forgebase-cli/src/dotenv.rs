//! Development `.env` support (SPEC §19).
//!
//! `forgebase` reads `./.env` from the working directory, only when the file
//! exists. Variables already set in the process environment are never
//! overridden, so a real environment always wins over the file. Values in the
//! file then feed the normal precedence (flags > `FORGEBASE_*` > config file).

use std::path::{Path, PathBuf};

use anyhow::Context;

/// File name looked up in the directory.
pub const DOTENV_FILE: &str = ".env";

/// Loads `dir/.env` into the process environment if it exists.
///
/// Returns the path that was loaded, or `None` when there is no file.
/// Existing variables are kept (dotenvy never overrides them).
pub fn load_from(dir: &Path) -> anyhow::Result<Option<PathBuf>> {
    let path = dir.join(DOTENV_FILE);
    if !path.is_file() {
        return Ok(None);
    }
    dotenvy::from_path(&path).with_context(|| format!("cannot load {}", path.display()))?;
    Ok(Some(path))
}

/// Loads `./.env` from the current working directory, if present.
pub fn load_from_cwd() -> anyhow::Result<Option<PathBuf>> {
    load_from(&std::env::current_dir().context("cannot read the working directory")?)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SET_KEY: &str = "FB03_DOTENV_PROBE_SET";
    const NEW_KEY: &str = "FB03_DOTENV_PROBE_NEW";
    const CHILD_MARK: &str = "FB03_DOTENV_PROBE_CHILD";

    #[test]
    fn missing_file_is_a_no_op() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(load_from(dir.path()).expect("no error"), None);
    }

    #[test]
    fn a_directory_named_dotenv_is_ignored() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join(DOTENV_FILE)).expect("mkdir");
        assert_eq!(load_from(dir.path()).expect("no error"), None);
    }

    #[test]
    fn malformed_file_is_reported_with_its_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        // dotenvy accepts most input; an unterminated quote is a hard parse error.
        std::fs::write(dir.path().join(DOTENV_FILE), "FB03_BROKEN=\"open\n").expect("write");
        let err = load_from(dir.path()).expect_err("unterminated quote must fail");
        assert!(format!("{err:#}").contains(DOTENV_FILE), "{err:#}");
    }

    /// The file loads, and a variable already in the environment is kept.
    /// Runs in a child process: this crate forbids `unsafe`, and setting
    /// environment variables in edition 2024 needs `unsafe`. The child gets its
    /// environment from the parent, which is the real-world case.
    #[test]
    fn env_file_fills_gaps_and_never_overrides_the_process() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join(DOTENV_FILE),
            format!("{SET_KEY}=from_file\n{NEW_KEY}=from_file\n"),
        )
        .expect("write");
        let exe = std::env::current_exe().expect("test binary");
        let out = std::process::Command::new(exe)
            .args(["--exact", "dotenv::tests::probe_child", "--nocapture"])
            .current_dir(dir.path())
            .env(CHILD_MARK, "1")
            .env(SET_KEY, "from_process")
            .env_remove(NEW_KEY)
            .output()
            .expect("spawn child");
        assert!(
            out.status.success(),
            "child failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Body of the child process above; a no-op when run on its own.
    #[test]
    fn probe_child() {
        if std::env::var_os(CHILD_MARK).is_none() {
            return;
        }
        load_from_cwd().expect("load").expect("file present");
        assert_eq!(std::env::var(SET_KEY).expect("set"), "from_process");
        assert_eq!(std::env::var(NEW_KEY).expect("filled from file"), "from_file");
    }
}
