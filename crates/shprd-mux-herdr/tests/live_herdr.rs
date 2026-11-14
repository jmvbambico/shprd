//! One live test against a private herdr server.
//!
//! `#[ignore]` because it starts a real `herdr server`, which CI and the unit
//! gate must never do. Run it by hand with
//!
//! ```text
//! cargo test -p shprd-mux-herdr -- --ignored
//! ```
//!
//! The server it starts is isolated twice over: `HOME` and `HERDR_SOCKET_PATH`
//! both point into a fresh temporary directory, so this test cannot see, or
//! touch, the multiplexer the human is working in.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

#[path = "support/live_server.rs"]
mod live_server;

use live_server::LiveServer;
use shprd_mux::{Direction, Mux, WorkspaceId};
use shprd_mux_herdr::HerdrMux;

/// The whole adapter, against one private server, once.
#[test]
#[ignore = "starts a real herdr server; run by hand"]
fn the_whole_adapter_works_against_an_isolated_server() {
    // A skip is the machine's answer, not a failure: no `herdr`, or a `TMPDIR`
    // too long for a unix socket path. The helper says which, in one line.
    let Some(server) = LiveServer::start() else {
        return;
    };
    let mux = HerdrMux::new().with_environment(server.socket(), server.home());
    let cwd = server.home().to_path_buf();
    let capabilities = mux.capabilities();
    assert!(capabilities.split_ratio);
    assert!(capabilities.agent_state);
    assert!(!capabilities.resumable_events);

    // A workspace named after the repo shprd is driving.
    let name = "shprd \u{b7} scratch";
    assert!(mux.find_workspace(name).unwrap().is_none());
    let workspace = mux.create_workspace(name, &cwd).unwrap();
    assert_eq!(workspace.name, name);
    assert_eq!(mux.find_workspace(name).unwrap().as_ref(), Some(&workspace));
    mux.focus_workspace(&workspace.id).unwrap();

    // A tab, then a pane split off its root pane.
    let tab = mux.create_tab(&workspace.id, "planner", &cwd).unwrap();
    // `workspace create` already made a starter tab, so the new one is found
    // among several rather than being the only tab.
    let listed = mux.list_tabs(&workspace.id).unwrap();
    assert_eq!(
        listed.iter().find(|listed| listed.id == tab.id),
        Some(&tab),
        "new tab is missing from {listed:?}"
    );
    mux.focus_tab(&tab.id).unwrap();
    let pane = mux
        .split(&tab.root_pane, Direction::Down, 0.5, &cwd)
        .unwrap();
    assert_ne!(pane, tab.root_pane);

    // The point of `run`: a program, quoted by us, starts in that pane.
    mux.run(
        &pane,
        &["sh".to_string(), "-c".to_string(), "echo hello".to_string()],
        &[],
    )
    .unwrap();
    let mut seen = String::new();
    for _ in 0..40 {
        seen = mux.read(&pane, 20).unwrap();
        if seen.contains("hello") {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    assert!(seen.contains("hello"), "pane output was {seen:?}");

    // Retitle, and see the new title through a listing.
    mux.rename_tab(&tab.id, "planner (renamed)").unwrap();
    let titles: Vec<String> = mux
        .list_tabs(&workspace.id)
        .unwrap()
        .into_iter()
        .map(|tab| tab.title)
        .collect();
    assert!(
        titles.iter().any(|title| title == "planner (renamed)"),
        "renamed tab is missing from {titles:?}"
    );
    assert!(!titles.iter().any(|title| title == "planner"), "{titles:?}");

    // A pane that runs no agent has no state, which is not an error.
    assert_eq!(
        mux.agent_state(&pane).unwrap(),
        shprd_mux::AgentState::Unknown
    );

    // Events: at least a baseline, and an empty or non-empty answer, never an
    // error, from a server that just answered everything else.
    let events = mux.next_events(Duration::from_millis(1)).unwrap();
    assert!(events.len() < 100, "a one millisecond poll saw {events:?}");

    // Closing is not a failure either, and closing twice is `NotFound`.
    mux.close_tab(&tab.id).unwrap();
    assert!(mux.close_tab(&tab.id).is_err());

    // The workspace the test made is the only one on this server.
    mux.focus_workspace(&WorkspaceId::new(workspace.id.as_str()))
        .unwrap();
    assert!(mux.find_workspace(name).unwrap().is_some());
}
