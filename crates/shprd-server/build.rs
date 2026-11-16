//! Rebuild when the built web app changes.
//!
//! The app is embedded with `rust-embed`, which reads `web/dist` at compile
//! time. Cargo would only notice the files it has already seen, so this script
//! watches the directory itself: a checkout that had no build when it was first
//! compiled, and gets one later, must embed it on the next `cargo build` rather
//! than keep serving the "no web build" page.

use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dist = manifest.join("..").join("..").join("web").join("dist");
    println!("cargo:rerun-if-changed={}", dist.display());
    // The build's own folder too: when `dist` is created or removed, only the
    // parent's mtime changes, and watching just the missing path would miss it.
    println!(
        "cargo:rerun-if-changed={}",
        manifest.join("..").join("..").join("web").display()
    );
}
