//! Re-embeds the admin dashboard when `dashboard/dist` changes.
//!
//! `rust-embed` reads `dashboard/dist` at compile time, but Cargo only
//! recompiles when a tracked path changes:
//!
//! * `dashboard/dist` exists → watch it (new, changed or deleted assets).
//! * otherwise → watch `dashboard/`, so a later `npm run build` that creates
//!   `dist/` is picked up without `cargo clean`. While `dashboard/` itself is
//!   missing Cargo treats it as changed and re-checks this crate on every
//!   build; that is the price of never shipping a stale embed.

use std::path::Path;

fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into());
    let dashboard = Path::new(&manifest).join("../../dashboard");
    let dist = dashboard.join("dist");
    let watched = if dist.is_dir() { dist } else { dashboard };
    println!("cargo::rerun-if-changed={}", watched.display());
    println!("cargo::rerun-if-changed=build.rs");
}
