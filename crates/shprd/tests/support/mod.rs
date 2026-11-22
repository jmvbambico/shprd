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

/// Shprd's own session variables: a test run from inside a shprd session (a
/// worker, the coordinator) inherits them, and the binary under test would
/// then read the person's repo, actor or home instead of its fixture.
const SESSION_VARS: &[&str] = &[
    "SHPRD_REPO",
    "SHPRD_ACTOR",
    "SHPRD_BIN",
    "SHPRD_HOME",
    "SHPRD_TASK",
];

/// [`herdr::isolate`], plus removal of shprd's own session variables. `SHPRD_HOME`
/// is then set to `anchor` by the herdr rule; a test that sets any of these on
/// purpose does so after this call, and wins.
pub fn isolate(command: &mut std::process::Command, anchor: &std::path::Path) {
    for name in SESSION_VARS {
        command.env_remove(name);
    }
    herdr::isolate(command, anchor);
}
