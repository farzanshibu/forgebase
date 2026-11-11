//! Extracts an engine archive the way the server's first start does, and
//! prints how long it took (FB-148: first-start time on a real archive).
//!
//! ```text
//! cargo run -p forgebase-pgengine --example extract_engine -- <archive.tar.zst> <engines dir>
//! ```
//!
//! The engines directory must not hold an earlier extraction of the same
//! archive, or the run only finds it again.

use std::path::PathBuf;
use std::time::Instant;

use forgebase_pgengine::archive::{FileSource, extract};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 2 {
        eprintln!("usage: extract_engine <archive.tar.zst> <engines dir>");
        std::process::exit(2);
    }
    let source = FileSource(PathBuf::from(&args[0]));
    let dir = PathBuf::from(&args[1]);
    let started = Instant::now();
    match extract(&source, &dir, None) {
        Ok(e) => {
            let files = e.manifest.files.len();
            println!(
                "extracted {files} files into {} in {:.2} s (reused: {})",
                e.dir.display(),
                started.elapsed().as_secs_f64(),
                e.reused
            );
        }
        Err(e) => {
            eprintln!("extract failed after {:.2} s: {e}", started.elapsed().as_secs_f64());
            std::process::exit(1);
        }
    }
}
