//! Build and version information baked into the binary.

/// Product name, as reported by `/api/system/info` and `forgebase version`.
pub const NAME: &str = "forgebase";

/// Product version (the workspace version).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Target operating system of this build.
pub const OS: &str = std::env::consts::OS;

/// Target CPU architecture of this build.
pub const ARCH: &str = std::env::consts::ARCH;
