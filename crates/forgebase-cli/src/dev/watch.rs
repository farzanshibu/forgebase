//! Change detection for `forgebase dev --watch`: the project directory is
//! scanned every 250 ms (path, modification time, size) and a change is
//! reported once the files have been quiet for a short while, so a save that
//! touches several files, or a formatter or build writing after it, produces
//! one re-apply. Polling needs no platform file-watching dependency and
//! behaves the same on every filesystem.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Time between scans.
const POLL: Duration = Duration::from_millis(250);
/// The files must be unchanged this long before a change is reported.
const QUIET: Duration = Duration::from_millis(400);

/// Directories never scanned.
const SKIP: &[&str] = &[".git", "node_modules", ".schemas", "target", "dist", ".DS_Store"];

type Snapshot = BTreeMap<PathBuf, (Option<SystemTime>, u64)>;

fn scan(root: &Path) -> Snapshot {
    let mut out = Snapshot::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.filter_map(Result::ok) {
            let name = e.file_name();
            if SKIP.iter().any(|s| name.to_str() == Some(s)) {
                continue;
            }
            let path = e.path();
            let Ok(meta) = e.metadata() else { continue };
            if meta.is_dir() {
                stack.push(path);
            } else {
                out.insert(path, (meta.modified().ok(), meta.len()));
            }
        }
    }
    out
}

/// Watches a project directory.
pub struct Watcher {
    root: PathBuf,
    last: Snapshot,
}

impl Watcher {
    /// Starts from the directory's current state.
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            last: scan(root),
        }
    }

    /// Takes the current state as the baseline (after an apply wrote files, such as a function build).
    pub fn rebase(&mut self) {
        self.last = scan(&self.root);
    }

    /// Resolves with the paths (relative to the directory) that changed, after the files were quiet.
    pub async fn next_change(&mut self) -> Vec<String> {
        loop {
            tokio::time::sleep(POLL).await;
            let now = scan(&self.root);
            if now == self.last {
                continue;
            }
            // Wait until the directory stops changing.
            let mut current = now;
            loop {
                tokio::time::sleep(QUIET).await;
                let again = scan(&self.root);
                if again == current {
                    break;
                }
                current = again;
            }
            let mut changed: Vec<String> = Vec::new();
            for (p, v) in &current {
                if self.last.get(p) != Some(v) {
                    changed.push(rel(&self.root, p));
                }
            }
            for p in self.last.keys() {
                if !current.contains_key(p) {
                    changed.push(rel(&self.root, p));
                }
            }
            self.last = current;
            if !changed.is_empty() {
                changed.sort();
                return changed;
            }
        }
    }
}

fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root).unwrap_or(p).display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reports_new_changed_and_removed_files_once_quiet() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("collections")).unwrap();
        std::fs::write(dir.path().join("collections/a.json"), "{}").unwrap();
        let mut w = Watcher::new(dir.path());
        let root = dir.path().to_path_buf();
        let writer = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            std::fs::write(root.join("collections/b.json"), "{}").unwrap();
            std::fs::write(root.join("collections/c.json"), "{}").unwrap();
        });
        let changed = tokio::time::timeout(Duration::from_secs(10), w.next_change())
            .await
            .unwrap();
        writer.await.unwrap();
        assert_eq!(changed, vec!["collections/b.json", "collections/c.json"]);
        std::fs::remove_file(dir.path().join("collections/a.json")).unwrap();
        let changed = tokio::time::timeout(Duration::from_secs(10), w.next_change())
            .await
            .unwrap();
        assert_eq!(changed, vec!["collections/a.json"]);
    }

    #[tokio::test]
    async fn the_skipped_directories_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = Watcher::new(dir.path());
        std::fs::create_dir_all(dir.path().join(".schemas")).unwrap();
        std::fs::write(dir.path().join(".schemas/x.json"), "{}").unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(900), w.next_change())
                .await
                .is_err()
        );
    }
}
