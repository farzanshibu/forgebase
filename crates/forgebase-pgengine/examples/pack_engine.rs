//! Packs an installed engine directory into a ForgeBase engine archive
//! (`tar.zst` with its manifest), for development and engine tooling.
//!
//! ```text
//! cargo run -p forgebase-pgengine --example pack_engine -- \
//!     <engine dir> <out.tar.zst> <postgres version> <build> <target> [paths…]
//! ```
//!
//! `paths` default to `bin lib share`. Prints the archive's SHA-256, the
//! value `engine.lock` pins for the target.

use std::path::PathBuf;

use forgebase_pgengine::archive::{PackInfo, pack_dir};
use sha2::{Digest, Sha256};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 5 {
        eprintln!("usage: pack_engine <engine dir> <out.tar.zst> <postgres version> <build> <target> [paths...]");
        std::process::exit(2);
    }
    let src = PathBuf::from(&args[0]);
    let out = PathBuf::from(&args[1]);
    let paths: Vec<&str> = if args.len() > 5 {
        args[5..].iter().map(String::as_str).collect()
    } else {
        vec!["bin", "lib", "share"]
    };
    let info = PackInfo {
        postgres: args[2].clone(),
        build: args[3].clone(),
        target: args[4].clone(),
        ..PackInfo::default()
    };
    let file = match std::fs::File::create(&out) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("create {}: {e}", out.display());
            std::process::exit(1);
        }
    };
    if let Err(e) = pack_dir(&src, &paths, &info, file, 3) {
        eprintln!("pack: {e}");
        std::process::exit(1);
    }
    let bytes = match std::fs::read(&out) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("read {}: {e}", out.display());
            std::process::exit(1);
        }
    };
    let sha: [u8; 32] = Sha256::digest(&bytes).into();
    let hex: String = sha.iter().map(|b| format!("{b:02x}")).collect();
    println!("{hex}  {}", out.display());
}
