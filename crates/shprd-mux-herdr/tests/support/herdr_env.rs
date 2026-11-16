//! Keeping every test away from the person's real herdr session and Herdr
//! desktop.
//!
//! # Why this exists
//!
//! A test run from a herdr pane inherits `HERDR_SOCKET_PATH` and the rest of
//! herdr's pane variables, because herdr exports them to everything it starts —
//! a shell, a `cargo test`, and therefore every child those tests make. The
//! binary under test wires its multiplexer to whatever that variable points at,
//! and `shprd serve` prepares its workspace at startup (M9 fix J), so one
//! `cargo test --workspace` launched from a pane created twenty workspaces
//! labelled `shprd · .tmpXXXXXX` in the person's own session. CI never saw it:
//! CI is not inside herdr.
//!
//! So there is exactly one rule, here, and every test that spawns the binary
//! goes through it before it adds anything of its own:
//!
//! - every `HERDR_*`, `HC_*` and `TUIOS_*` variable of the test process is
//!   removed from the child, by name as the environment actually holds it, so
//!   whatever herdr exported this time is exactly what the child cannot inherit;
//! - `HERDR_SOCKET_PATH` is then set to a **nonexistent** socket inside the
//!   test's own temporary folder, so a child that tries to reach a multiplexer
//!   finds none and fails visibly instead of finding the person's;
//! - `HC_SESSION` names a private session. It is set rather than left unset
//!   because an unset `HC_SESSION` means the *default* session — the person's —
//!   while a set one names a session that is nobody's;
//! - `HERDR_APP_HOME` points into the temporary folder too, so `herdr-app` can
//!   never find the person's window;
//! - `SHPRD_HOME` points into the temporary folder as well, so a child that
//!   resolves the home itself (the dagr projection reads `graph.dagr` from that
//!   home and looks in its `herdr/plugins/` folder) reads the test's own files
//!   and not the developer's. A test that passes `--home` still wins: the flag
//!   is read before the variable.
//!
//! A fixture that needs a different value applies the rule first and overrides
//! that one variable afterwards: an `.env` call after [`isolate`] wins, which is
//! how the `#[ignore]` live tests point a child at the private `herdr server`
//! they started themselves. The rest of the rule still holds for them.
//!
//! This file is shared, not copied: `crates/shprd/tests/support/mod.rs` includes
//! it with `#[path]`, the same way `spawn_live.rs` includes the live-server
//! helper, so there is one rule and not one per test crate.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

/// The session name a spawned child sees, so nothing it runs can fall back to
/// the person's Herdr core. Nobody's session is called this.
pub const PRIVATE_HC_SESSION: &str = "shprd-test";

/// Variables removed from every child whatever their value: names that are not
/// `HERDR_*` but still point at some host.
const HOST_VARS: &[&str] = &[
    "HC_SESSION",
    "HC_SOCKET",
    "HC_TERMINAL_ID",
    "HC_ASK_ID",
    "HERDR_APP_HOME",
];

/// Prefixes whose variables are all removed from every child.
///
/// `HERDR_` covers `HERDR_SOCKET_PATH`, `HERDR_ENV`, `HERDR_PANE_ID`,
/// `HERDR_TAB_ID`, `HERDR_WORKSPACE_ID`, `HERDR_BIN_PATH`, `HERDR_SESSION` and
/// whatever herdr adds next, without this file having to learn each new name.
const HOST_PREFIXES: &[&str] = &["HERDR_", "HC_", "TUIOS_"];

/// The socket path a child is given: inside the test's folder, and never
/// created, so a multiplexer call against it fails with "no server" rather than
/// reaching a session that is running.
pub fn absent_socket(anchor: &Path) -> PathBuf {
    anchor.join("no-herdr-server.sock")
}

/// Points `command` away from every real host: see the module documentation.
///
/// `anchor` is any path inside the test's own temporary folder — usually the
/// fixture's root, home or state directory. The absent socket and the
/// `HERDR_APP_HOME` are put beside it, so a child that writes something anyway
/// writes it inside the test's temp directory.
pub fn isolate(command: &mut Command, anchor: &Path) {
    // The parent's own environment is the leak, and herdr's variable names are
    // not ours to enumerate ahead of time, so the rule is applied to the names
    // that are actually there.
    for (name, _) in std::env::vars_os() {
        if let Some(text) = name.to_str()
            && is_a_host_variable(text)
        {
            command.env_remove(text);
        }
    }
    // Named a second time so the rule holds even if the listing above came back
    // empty for a name the child would otherwise still inherit.
    for name in HOST_VARS {
        command.env_remove(name);
    }
    command
        .env("HERDR_SOCKET_PATH", absent_socket(anchor))
        .env("HERDR_APP_HOME", anchor.join("herdr-app"))
        // The home shprd's own files live under, so `graph.dagr` and the herdr
        // plugin folder a child probes are the test's, never the person's.
        .env("SHPRD_HOME", anchor)
        .env("HC_SESSION", PRIVATE_HC_SESSION);
}

/// Whether `name` points a child at some host rather than at its own work.
pub fn is_a_host_variable(name: &str) -> bool {
    HOST_VARS.contains(&name) || HOST_PREFIXES.iter().any(|prefix| name.starts_with(prefix))
}
