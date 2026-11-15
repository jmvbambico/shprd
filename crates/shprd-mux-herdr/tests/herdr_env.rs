//! The shared isolation rule every spawning test follows, tested once.
//!
//! `support/herdr_env.rs` is included by `crates/shprd/tests/support/mod.rs`
//! with `#[path]`, exactly as `spawn_live.rs` includes the live-server helper,
//! so both crates follow one rule and this file proves what that rule does.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;

#[path = "support/herdr_env.rs"]
mod herdr_env;

use herdr_env::{PRIVATE_HC_SESSION, absent_socket, is_a_host_variable, isolate};

/// The value `command` set for `name`, or `None` when it left it alone.
fn value<'a>(command: &'a Command, name: &str) -> Option<&'a OsStr> {
    command
        .get_envs()
        .find(|(key, _)| *key == OsStr::new(name))
        .map(|(_, value)| value.unwrap_or_default())
}

/// Every name herdr, hc or tuios hands a pane is recognised; a name of a
/// fixture's own is not.
#[test]
fn the_names_that_point_at_a_host_are_recognised() {
    for name in [
        "HERDR_SOCKET_PATH",
        "HERDR_ENV",
        "HERDR_PANE_ID",
        "HERDR_TAB_ID",
        "HERDR_WORKSPACE_ID",
        "HERDR_BIN_PATH",
        "HERDR_SESSION",
        "HERDR_APP_HOME",
        "HC_SESSION",
        "HC_SOCKET",
        "HC_TERMINAL_ID",
        "TUIOS_ENV",
        "TUIOS_SOCKET",
    ] {
        assert!(is_a_host_variable(name), "{name} points at a host");
    }
    for name in ["PATH", "HOME", "SHPRD_HOME", "XDG_STATE_HOME", "HERDRISH"] {
        assert!(!is_a_host_variable(name), "{name} is the test's own");
    }
}

/// A child is left with no host to reach: the socket cannot exist, the desktop
/// home is inside the test's folder, and the session is nobody's.
#[test]
fn a_child_is_left_with_no_host_to_reach() {
    let anchor = Path::new("/tmp/a-test/anchor");
    let mut command = Command::new("/usr/bin/true");
    isolate(&mut command, anchor);

    assert_eq!(
        value(&command, "HERDR_SOCKET_PATH"),
        Some(OsStr::new("/tmp/a-test/anchor/no-herdr-server.sock")),
        "the socket is absent, and inside the test's own folder"
    );
    assert_eq!(
        value(&command, "HC_SESSION"),
        Some(OsStr::new(PRIVATE_HC_SESSION)),
        "the desktop's session is one nobody runs"
    );
    assert_eq!(
        value(&command, "HERDR_APP_HOME"),
        Some(OsStr::new("/tmp/a-test/anchor/herdr-app"))
    );
}

/// Every host variable the test process itself holds is removed from the
/// child, whatever its name: the removals are recorded in the child's own
/// environment map as empty values.
#[test]
fn every_inherited_host_variable_is_removed_by_name() {
    let mut command = Command::new("/usr/bin/true");
    isolate(&mut command, Path::new("/tmp/a-test/anchor"));
    let mut checked = 0;
    for (name, _) in std::env::vars_os() {
        let Some(text) = name.to_str() else { continue };
        if !is_a_host_variable(text) {
            continue;
        }
        if matches!(text, "HERDR_SOCKET_PATH" | "HC_SESSION" | "HERDR_APP_HOME") {
            continue;
        }
        assert_eq!(
            value(&command, text),
            Some(OsStr::new("")),
            "{text} must be removed, not inherited"
        );
        checked += 1;
    }
    // Not an assertion about the machine, only a note: outside herdr there is
    // nothing of hers to remove, and the test above still proves the rule.
    if checked == 0 {
        eprintln!("no inherited host variable to remove on this machine");
    }
}

/// The absent socket is a path nothing creates, so a call against it is "no
/// server" and never the person's session.
#[test]
fn the_absent_socket_is_a_path_nothing_creates() {
    let socket = absent_socket(Path::new("/tmp/a-test/anchor"));
    assert!(socket.starts_with("/tmp/a-test/anchor"));
    assert!(!socket.exists());
}
