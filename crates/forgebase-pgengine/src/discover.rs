//! Finding the engine: embedded archive, archive file or installed directory.

use std::path::{Path, PathBuf};

use crate::archive::{self, ArchiveSource, BytesSource, FileSource};
use crate::config::{EngineConfig, EngineSource, ENGINE_DIR_ENV};
use crate::error::{EngineError, Result};
use crate::layout::Layout;

/// Oldest major the managed engine accepts: the `builtin` C.UTF-8 locale
/// (ADR-0028 §4.2) needs PostgreSQL 17.
pub const MIN_MAJOR: u32 = 17;

/// Whether this binary embeds an engine archive (feature `bundled-postgres`).
pub const HAS_EMBEDDED: bool = cfg!(feature = "bundled-postgres");

#[cfg(feature = "bundled-postgres")]
static EMBEDDED_ARCHIVE: &[u8] = include_bytes!(env!("FORGEBASE_PG_ENGINE_EMBED"));
#[cfg(feature = "bundled-postgres")]
const EMBEDDED_SHA256: &str = env!("FORGEBASE_PG_ENGINE_SHA256");

/// The embedded archive, when this binary has one.
pub fn embedded_archive() -> Option<BytesSource<'static>> {
    #[cfg(feature = "bundled-postgres")]
    {
        Some(BytesSource {
            bytes: EMBEDDED_ARCHIVE,
            sha256: Some(EMBEDDED_SHA256),
        })
    }
    #[cfg(not(feature = "bundled-postgres"))]
    {
        None
    }
}

/// A PostgreSQL version as `postgres --version` prints it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PgVersion {
    /// Major (18).
    pub major: u32,
    /// Minor (3); 0 for pre-releases.
    pub minor: u32,
    /// The version token (`18.3`, `19beta1`).
    pub text: String,
}

impl PgVersion {
    /// Parses `postgres (PostgreSQL) 18.3 (Homebrew)` or a bare `18.3`.
    pub fn parse(output: &str) -> Option<Self> {
        let token = output
            .split_whitespace()
            .skip_while(|t| !t.starts_with(|c: char| c.is_ascii_digit()))
            .next()?;
        let digits = |s: &str| s.chars().take_while(char::is_ascii_digit).collect::<String>();
        let major: u32 = digits(token).parse().ok()?;
        let rest = &token[digits(token).len()..];
        let minor = rest
            .strip_prefix('.')
            .and_then(|r| digits(r).parse().ok())
            .unwrap_or(0);
        Some(Self {
            major,
            minor,
            text: token.to_owned(),
        })
    }
}

/// Where a resolved engine came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineOrigin {
    /// An installed directory used in place.
    Dir,
    /// Extracted from an archive (embedded or file).
    Extracted {
        /// SHA-256 of the archive.
        sha256: String,
        /// Whether the long-path fallback put it in the cache directory.
        in_cache: bool,
        /// Whether this start extracted it (`false`: reused).
        fresh: bool,
    },
}

/// A usable engine directory.
#[derive(Debug, Clone)]
pub struct EngineDir {
    /// Root holding `bin/`, `lib/`, `share/`.
    pub root: PathBuf,
    /// Version of its `postgres`.
    pub version: PgVersion,
    /// Where it came from.
    pub origin: EngineOrigin,
    /// Directories searched for loadable modules (`$libdir`).
    pub pkglibdirs: Vec<PathBuf>,
}

impl EngineDir {
    /// Path of a program in `bin/`.
    pub fn bin(&self, name: &str) -> PathBuf {
        self.root.join("bin").join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
    }

    /// Whether a loadable module (`pg_cron`, `pg_stat_statements`) exists.
    pub fn has_module(&self, name: &str) -> bool {
        self.pkglibdirs.iter().any(|d| {
            ["so", "dylib", "dll"]
                .iter()
                .any(|ext| d.join(format!("{name}.{ext}")).is_file())
        })
    }

    /// Opens `root` as an engine: checks its programs and asks `postgres`
    /// for its version (which also proves the binary runs on this host).
    ///
    /// # Errors
    /// Missing programs, a binary that does not run, or a major below
    /// [`MIN_MAJOR`].
    pub async fn open(root: &Path, origin: EngineOrigin) -> Result<Self> {
        let bad = |reason: String| EngineError::Engine {
            dir: root.to_path_buf(),
            reason,
        };
        let mut dir = Self {
            root: root.to_path_buf(),
            version: PgVersion {
                major: 0,
                minor: 0,
                text: String::new(),
            },
            origin,
            pkglibdirs: Vec::new(),
        };
        for prog in ["postgres", "initdb"] {
            if !dir.bin(prog).is_file() {
                return Err(bad(format!("bin/{prog} is missing")));
            }
        }
        let out = tokio::process::Command::new(dir.bin("postgres"))
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .output()
            .await
            .map_err(|e| {
                bad(format!(
                    "bin/postgres does not run on this host ({e}); on Linux this usually means the \
                     engine was not relocated or needs a newer C library"
                ))
            })?;
        let text = String::from_utf8_lossy(&out.stdout);
        if !out.status.success() {
            return Err(bad(format!(
                "bin/postgres --version failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        dir.version = PgVersion::parse(&text).ok_or_else(|| bad(format!("unexpected version output {text:?}")))?;
        if dir.version.major < MIN_MAJOR {
            return Err(bad(format!(
                "PostgreSQL {} is too old for the managed engine (needs {MIN_MAJOR} or later)",
                dir.version.text
            )));
        }
        dir.pkglibdirs = pkglibdirs(&dir).await;
        Ok(dir)
    }
}

/// `pg_config --pkglibdir` when the engine has `pg_config`, then the two
/// layouts PostgreSQL installs use (`lib/postgresql/`, `lib/`).
async fn pkglibdirs(dir: &EngineDir) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let pg_config = dir.bin("pg_config");
    if pg_config.is_file() {
        if let Ok(o) = tokio::process::Command::new(&pg_config)
            .arg("--pkglibdir")
            .stdin(std::process::Stdio::null())
            .output()
            .await
        {
            let p = String::from_utf8_lossy(&o.stdout).trim().to_owned();
            if o.status.success() && !p.is_empty() {
                out.push(PathBuf::from(p));
            }
        }
    }
    out.push(dir.root.join("lib").join("postgresql"));
    out.push(dir.root.join("lib"));
    out
}

/// Resolves the configured source to a usable engine directory, extracting
/// an archive when needed.
pub(crate) async fn resolve(cfg: &EngineConfig, layout: &Layout) -> Result<EngineDir> {
    match &cfg.source {
        EngineSource::Dir(d) => EngineDir::open(d, EngineOrigin::Dir).await,
        EngineSource::Archive(p) => extract_and_open(Box::new(FileSource(p.clone())), cfg, layout).await,
        EngineSource::Embedded => match embedded_archive() {
            Some(src) => extract_and_open(Box::new(src), cfg, layout).await,
            None => Err(no_engine()),
        },
        EngineSource::Auto => {
            if let Some(src) = embedded_archive() {
                return extract_and_open(Box::new(src), cfg, layout).await;
            }
            match std::env::var_os(ENGINE_DIR_ENV).filter(|v| !v.is_empty()) {
                Some(d) => EngineDir::open(Path::new(&d), EngineOrigin::Dir).await,
                None => Err(no_engine()),
            }
        }
    }
}

fn no_engine() -> EngineError {
    EngineError::NoEngine(format!(
        "this build has no bundled PostgreSQL; set [database.postgres] engine_dir (or {ENGINE_DIR_ENV}) \
         to a PostgreSQL {MIN_MAJOR}+ installation, use [database.postgres] mode = \"external\" with a URL, \
         or use the standard release"
    ))
}

async fn extract_and_open(
    source: Box<dyn ArchiveSource + Send + Sync>,
    cfg: &EngineConfig,
    layout: &Layout,
) -> Result<EngineDir> {
    let engines = layout.engines_dir();
    let cache = cfg.cache_dir.clone().or_else(crate::config::default_cache_dir);
    let ex = tokio::task::spawn_blocking(move || {
        let sha = source.sha256()?;
        archive::extract(source.as_ref(), &engines, cache.as_deref()).map(|ex| (ex, sha))
    })
    .await
    .map_err(|e| EngineError::Archive(format!("extraction task failed: {e}")))?;
    let (ex, sha256) = ex?;
    let dir = EngineDir::open(
        &ex.dir,
        EngineOrigin::Extracted {
            sha256,
            in_cache: ex.in_cache,
            fresh: !ex.reused,
        },
    )
    .await?;
    if PgVersion::parse(&ex.manifest.postgres).is_some_and(|m| m.major != dir.version.major) {
        return Err(EngineError::Engine {
            dir: ex.dir,
            reason: format!(
                "manifest says PostgreSQL {}, bin/postgres is {}",
                ex.manifest.postgres, dir.version.text
            ),
        });
    }
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parsing() {
        let v = PgVersion::parse("postgres (PostgreSQL) 18.3 (Homebrew)\n").expect("parse");
        assert_eq!((v.major, v.minor, v.text.as_str()), (18, 3, "18.3"));
        let v = PgVersion::parse("postgres (PostgreSQL) 19beta1").expect("parse");
        assert_eq!((v.major, v.minor), (19, 0));
        let v = PgVersion::parse("18.6").expect("bare");
        assert_eq!((v.major, v.minor), (18, 6));
        assert!(PgVersion::parse("postgres (PostgreSQL)").is_none());
    }

    #[tokio::test]
    async fn missing_programs_are_reported() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = EngineDir::open(dir.path(), EngineOrigin::Dir).await.expect_err("empty");
        assert!(err.to_string().contains("bin/postgres is missing"), "{err}");
    }

    #[test]
    fn no_engine_message_says_what_to_do() {
        let m = no_engine().to_string();
        assert!(m.contains("engine_dir") && m.contains("external"), "{m}");
        assert_eq!(embedded_archive().is_some(), HAS_EMBEDDED);
    }
}
