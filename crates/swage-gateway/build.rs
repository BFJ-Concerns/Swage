//! Build script: ensures the frontend dist directory exists before compile.
//!
//! `rust-embed` requires the embedded folder to exist at compile time. When
//! the frontend hasn't been built yet (e.g. a fresh checkout), the directory
//! is missing and the crate fails to compile. Creating an empty directory here
//! lets the crate compile with no embedded assets; the `dev_fallback` handler
//! in `assets.rs` then returns a helpful message at runtime instead of the SPA.

use std::path::Path;

fn main() {
    let dist = Path::new("../../frontend/dist");

    if !dist.exists() {
        std::fs::create_dir_all(dist)
            .expect("failed to create frontend/dist directory for rust-embed");
    }

    // Re-run this script only when the dist directory itself changes.
    // Individual asset changes are picked up by rust-embed's own change
    // detection on the embedded files.
    println!("cargo:rerun-if-changed=../../frontend/dist");
}
