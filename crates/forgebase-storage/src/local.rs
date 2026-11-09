//! The local filesystem driver: blobs are files under a root directory.
//!
//! Writes go to `<root>/.tmp/<id>`, are flushed and synced, then renamed into
//! place, so a crash or a dropped connection never leaves a partial blob under
//! its key. Keys are internal and validated: no empty, `.` or `..` segments.

use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use axum::body::Bytes;
use futures_util::StreamExt;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use crate::driver::{BoxFuture, ByteStream, DriverObject, StorageDriver};
use crate::error::{Result, StorageError};
use crate::util::new_id;

const TMP_DIR: &str = ".tmp";
const CHUNK: usize = 64 * 1024;

/// Stores blobs as files under `root`.
#[derive(Debug, Clone)]
pub struct LocalDriver {
    root: PathBuf,
}

impl LocalDriver {
    /// A driver rooted at `root` (created on first use).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn path(&self, key: &str) -> Result<PathBuf> {
        if key.is_empty()
            || key
                .split('/')
                .any(|s| s.is_empty() || s == "." || s == ".." || s.contains('\\') || s.contains('\0'))
        {
            return Err(StorageError::Internal(format!("invalid driver key {key:?}")));
        }
        Ok(self.root.join(key))
    }

    async fn tmp_path(&self) -> Result<PathBuf> {
        let dir = self.root.join(TMP_DIR);
        tokio::fs::create_dir_all(&dir).await?;
        Ok(dir.join(new_id()))
    }

    async fn install(&self, tmp: &Path, key: &str) -> Result<()> {
        let dest = self.path(key)?;
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::rename(tmp, &dest).await?;
        Ok(())
    }
}

fn modified_ms(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

impl StorageDriver for LocalDriver {
    fn name(&self) -> &'static str {
        "local"
    }

    fn put(&self, key: &str, mut body: ByteStream) -> BoxFuture<'_, Result<u64>> {
        let key = key.to_owned();
        Box::pin(async move {
            let tmp = self.tmp_path().await?;
            let mut file = tokio::fs::File::create(&tmp).await?;
            let mut written = 0u64;
            let result: io::Result<()> = async {
                while let Some(chunk) = body.next().await {
                    let chunk = chunk?;
                    file.write_all(&chunk).await?;
                    written += chunk.len() as u64;
                }
                file.flush().await?;
                file.sync_all().await
            }
            .await;
            drop(file);
            if let Err(e) = result {
                let _ = tokio::fs::remove_file(&tmp).await;
                return Err(e.into());
            }
            if let Err(e) = self.install(&tmp, &key).await {
                let _ = tokio::fs::remove_file(&tmp).await;
                return Err(e);
            }
            Ok(written)
        })
    }

    fn get(&self, key: &str, range: Option<(u64, u64)>) -> BoxFuture<'_, Result<ByteStream>> {
        let path = self.path(key);
        Box::pin(async move {
            let path = path?;
            let mut file = tokio::fs::File::open(&path).await.map_err(|e| {
                if e.kind() == io::ErrorKind::NotFound {
                    StorageError::NotFound("stored blob".into())
                } else {
                    e.into()
                }
            })?;
            let size = file.metadata().await?.len();
            let (start, end) = range.map_or((0, size), |(s, e)| (s.min(size), e.min(size)));
            if start > 0 {
                file.seek(io::SeekFrom::Start(start)).await?;
            }
            let remaining = end.saturating_sub(start);
            let stream = futures_util::stream::unfold((file, remaining), |(mut file, left)| async move {
                if left == 0 {
                    return None;
                }
                let want = usize::try_from(left.min(CHUNK as u64)).unwrap_or(CHUNK);
                let mut buf = vec![0u8; want];
                match file.read(&mut buf).await {
                    Ok(0) => Some((
                        Err(io::Error::new(io::ErrorKind::UnexpectedEof, "stored blob is shorter than recorded")),
                        (file, 0),
                    )),
                    Ok(n) => {
                        buf.truncate(n);
                        Some((Ok(Bytes::from(buf)), (file, left - n as u64)))
                    }
                    Err(e) => Some((Err(e), (file, 0))),
                }
            });
            Ok(Box::pin(stream) as ByteStream)
        })
    }

    fn head(&self, key: &str) -> BoxFuture<'_, Result<Option<DriverObject>>> {
        let path = self.path(key);
        let key = key.to_owned();
        Box::pin(async move {
            match tokio::fs::metadata(path?).await {
                Ok(m) if m.is_file() => Ok(Some(DriverObject {
                    key,
                    size: m.len(),
                    modified_ms: modified_ms(&m),
                })),
                Ok(_) => Ok(None),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e.into()),
            }
        })
    }

    fn delete(&self, key: &str) -> BoxFuture<'_, Result<()>> {
        let path = self.path(key);
        Box::pin(async move {
            match tokio::fs::remove_file(path?).await {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e.into()),
            }
        })
    }

    fn copy(&self, from: &str, to: &str) -> BoxFuture<'_, Result<()>> {
        let src = self.path(from);
        let to = to.to_owned();
        Box::pin(async move {
            let tmp = self.tmp_path().await?;
            if let Err(e) = tokio::fs::copy(src?, &tmp).await {
                let _ = tokio::fs::remove_file(&tmp).await;
                return Err(if e.kind() == io::ErrorKind::NotFound {
                    StorageError::NotFound("stored blob".into())
                } else {
                    e.into()
                });
            }
            if let Err(e) = self.install(&tmp, &to).await {
                let _ = tokio::fs::remove_file(&tmp).await;
                return Err(e);
            }
            Ok(())
        })
    }

    fn rename<'a>(&'a self, from: &'a str, to: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let src = self.path(from)?;
            let dest = self.path(to)?;
            if let Some(parent) = dest.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::rename(&src, &dest).await.map_err(|e| {
                if e.kind() == io::ErrorKind::NotFound {
                    StorageError::NotFound("stored blob".into())
                } else {
                    e.into()
                }
            })
        })
    }

    fn list(&self, prefix: &str) -> BoxFuture<'_, Result<Vec<DriverObject>>> {
        let prefix = prefix.to_owned();
        Box::pin(async move {
            // Walk the directory part of the prefix, then filter by the full prefix.
            let dir_part = prefix.rsplit_once('/').map_or("", |(d, _)| d);
            let start = if dir_part.is_empty() {
                self.root.clone()
            } else {
                self.path(dir_part)?
            };
            let mut out = Vec::new();
            let mut stack = vec![start];
            while let Some(dir) = stack.pop() {
                let mut rd = match tokio::fs::read_dir(&dir).await {
                    Ok(rd) => rd,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                    Err(e) => return Err(e.into()),
                };
                while let Some(entry) = rd.next_entry().await? {
                    let meta = entry.metadata().await?;
                    let path = entry.path();
                    if meta.is_dir() {
                        stack.push(path);
                    } else if meta.is_file() {
                        let Ok(rel) = path.strip_prefix(&self.root) else { continue };
                        let key = rel.to_string_lossy().replace('\\', "/");
                        if key.starts_with(&prefix) {
                            out.push(DriverObject {
                                key,
                                size: meta.len(),
                                modified_ms: modified_ms(&meta),
                            });
                        }
                    }
                }
            }
            out.sort_by(|a, b| a.key.cmp(&b.key));
            Ok(out)
        })
    }

    fn ping(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            tokio::fs::create_dir_all(&self.root).await?;
            let probe = self.tmp_path().await?;
            tokio::fs::write(&probe, b"ok").await?;
            tokio::fs::remove_file(&probe).await?;
            Ok(())
        })
    }
}
