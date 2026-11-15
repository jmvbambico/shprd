//! Helpers every integration test in this crate shares.
//!
//! Nothing here is a test target of its own: `tests/support/` is a directory
//! module, so each test file that spawns the binary says `mod support;` and gets
//! the same rule rather than its own copy of it.

#![allow(dead_code)]

/// The one rule for keeping a spawned child away from the person's own herdr
/// session and Herdr desktop.
///
/// It lives in `shprd-mux-herdr`'s test support rather than here because the
/// adapter's own live tests need the same rule, and two copies would drift.
#[path = "../../../shprd-mux-herdr/tests/support/herdr_env.rs"]
pub mod herdr;
