//! Rebuild when the built web app changes, and make a release build carry one.
//!
//! The app is embedded with `rust-embed`, which reads `web/dist` at compile
//! time. Cargo would only notice the files it has already seen, so this script
//! watches the directory itself: a checkout that had no build when it was first
//! compiled, and gets one later, must embed it on the next `cargo build` rather
//! than keep serving the "no web build" page.
//!
//! A debug build may go without `web/dist` (the embed is `allow_missing`, so
//! the service still serves its API). A **release** build may not: it is what
//! people run, and one without the app serves a page saying so. So in release
//! this script builds the app first (`npm ci` when `node_modules` is absent,
//! then `npm run build`) and fails the build if `web/dist/index.html` is still
//! missing.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let web = manifest.join("..").join("..").join("web");
    let dist = web.join("dist");
    println!("cargo:rerun-if-changed={}", dist.display());
    // The build's own folder too: when `dist` is created or removed, only the
    // parent's mtime changes, and watching just the missing path would miss it.
    println!("cargo:rerun-if-changed={}", web.display());

    let release = std::env::var("PROFILE").is_ok_and(|profile| profile == "release");
    if release && !dist.join("index.html").is_file() {
        build_web(&web);
        assert!(
            dist.join("index.html").is_file(),
            "a release build must embed the web app, but {} is missing: \
             run `npm ci && npm run build` in web/ (Node 24+), then build again",
            dist.join("index.html").display()
        );
    }
}

/// Builds the web app, panicking with the reason when it cannot.
fn build_web(web: &Path) {
    if !web.join("node_modules").is_dir() {
        npm(web, &["ci"]);
    }
    npm(web, &["run", "build"]);
}

/// Runs `npm <args>` in `web`, failing the build when it does not succeed.
fn npm(web: &Path, args: &[&str]) {
    let status = Command::new("npm").args(args).current_dir(web).status();
    match status {
        Ok(status) if status.success() => {}
        Ok(status) => panic!("`npm {}` in web/ failed ({status})", args.join(" ")),
        Err(error) => panic!(
            "a release build needs Node to build the web app, but `npm` did not start ({error}): \
             install Node 24+ or run `npm ci && npm run build` in web/ first"
        ),
    }
}
