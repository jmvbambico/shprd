//! Live tests against a private herdr server.
//!
//! `#[ignore]` because they start a real `herdr server`, which CI and the unit
//! gate must never do. Run them by hand with
//!
//! ```text
//! cargo test -p shprd-mux-herdr -- --ignored
//! ```
//!
//! The servers they start are isolated twice over: `HOME` and
//! `HERDR_SOCKET_PATH` both point into a fresh temporary directory, and every
//! `herdr` command goes through `isolate` first, so a live test cannot see, or
//! touch, the multiplexer the human is working in.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

#[path = "support/live_server.rs"]
mod live_server;

use live_server::LiveServer;
use shprd_mux::{Direction, Host, Key, Outcome, SpaceId, Waited};
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
    assert!(capabilities.spawn);
    assert!(capabilities.split_ratio);
    assert!(capabilities.agent_state);
    assert!(capabilities.close_space);
    assert!(!capabilities.resumable_events);
    assert!(!capabilities.doc);

    // A space named after the repo shprd is driving.
    let name = "shprd \u{b7} scratch";
    assert!(mux.find_space(name).unwrap().is_none());
    let space = mux.create_space(name, &cwd).unwrap();
    assert_eq!(space.name, name);
    assert_eq!(mux.find_space(name).unwrap().as_ref(), Some(&space));
    mux.focus_space(&space.id).unwrap();
    // `ensure_space` is the same space, not a second one.
    assert_eq!(mux.ensure_space(name, &cwd).unwrap(), space);

    // A tab, then a pane split off its root pane.
    let tab = mux.spawn(&space.id, "planner", &[], &[], &cwd).unwrap();
    // `workspace create` already made a starter tab, so the new one is found
    // among several rather than being the only tab.
    let listed = mux.list_tabs(&space.id).unwrap();
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

    // `write` types, `keys` presses: together they run a command and submit it.
    let echo = "echo shprd-live-check";
    mux.write(&pane, echo, false).unwrap();
    mux.keys(&pane, &[Key::Enter]).unwrap();
    let mut seen = String::new();
    for _ in 0..40 {
        seen = mux.read(&pane, 20).unwrap();
        if seen.contains("shprd-live-check") {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    assert!(
        seen.contains("shprd-live-check"),
        "pane output was {seen:?}"
    );

    // `wait` for output the screen already holds answers at once.
    let waited = mux
        .wait(
            &pane,
            &Waited::Output("shprd-live-check".to_string()),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(waited.output.as_deref(), Some("shprd-live-check"));

    // `write` with submit is one turn for whatever runs in the pane.
    mux.write(&pane, "echo shprd-live-submit", true).unwrap();

    // Retitle, and see the new title through a listing.
    mux.rename_tab(&tab.id, "planner (renamed)").unwrap();
    let titles: Vec<String> = mux
        .list_tabs(&space.id)
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
    let events = mux.subscribe(Duration::from_millis(1)).unwrap();
    assert!(events.len() < 100, "a one millisecond poll saw {events:?}");

    // Closing is not a failure either, and closing twice is `NotFound`.
    mux.close_tab(&tab.id).unwrap();
    assert!(mux.close_tab(&tab.id).is_err());

    // The space the test made is the only one the host still has, and closing
    // it is one call that takes whatever a task left in it.
    mux.focus_space(&SpaceId::new(space.id.as_str())).unwrap();
    assert!(mux.find_space(name).unwrap().is_some());
    mux.close_space(&space.id).unwrap();
    assert!(
        mux.find_space(name).unwrap().is_none(),
        "a closed space stays listed"
    );
}

/// One task's space, from spawn to close: the layout M10.2 gives every task.
#[test]
#[ignore = "starts a real herdr server; run by hand"]
fn a_task_space_is_spawned_into_and_closed_as_one() {
    let Some(server) = LiveServer::start() else {
        return;
    };
    let mux = HerdrMux::new().with_environment(server.socket(), server.home());
    let cwd = server.home().to_path_buf();

    // The repository's own space, where the coordinator's tabs live, outlives
    // the task: it is made once and never closed here.
    let repo = mux
        .ensure_space(&shprd_mux::repo_space_name("scratch"), &cwd)
        .unwrap();
    // A second space, to prove the adapter's space verbs (shprd itself makes only
    // the repository's now).
    let name = String::from("shprd \u{b7} scratch \u{b7} r1 \u{b7} T1");
    let task = mux.ensure_space(&name, &cwd).unwrap();
    assert_ne!(task.id, repo.id);
    assert_eq!(task.name, name);

    // The worker is a program started in a tab of the task's space.
    let worker = mux
        .spawn(
            &task.id,
            "sh\u{b7}T1",
            &[
                "sh".to_string(),
                "-c".to_string(),
                "echo worker-ready; exec sh".to_string(),
            ],
            &[("SHPRD_LIVE".to_string(), Some("1".to_string()))],
            &cwd,
        )
        .unwrap();
    let mut seen = String::new();
    for _ in 0..40 {
        seen = mux.read(&worker.root_pane, 20).unwrap();
        if seen.contains("worker-ready") {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    assert!(seen.contains("worker-ready"), "pane output was {seen:?}");

    // A reviewer is a second tab in the same space, told apart by its title.
    let reviewer = mux
        .spawn(&task.id, "codex\u{b7}T1\u{b7}review", &[], &[], &cwd)
        .unwrap();
    let titles: Vec<String> = mux
        .list_tabs(&task.id)
        .unwrap()
        .into_iter()
        .map(|tab| tab.title)
        .collect();
    assert!(
        titles.iter().any(|title| title == "sh\u{b7}T1"),
        "{titles:?}"
    );
    assert!(
        titles
            .iter()
            .any(|title| title == "codex\u{b7}T1\u{b7}review"),
        "{titles:?}"
    );

    // Closing the space takes both tabs, and leaves the repository's alone.
    mux.close_space(&task.id).unwrap();
    assert!(mux.find_space(&name).unwrap().is_none());
    assert!(
        mux.find_space(&shprd_mux::repo_space_name("scratch"))
            .unwrap()
            .is_some(),
        "closing a task's space must not close the repository's"
    );
    assert!(mux.read(&reviewer.root_pane, 1).is_err());
    mux.close_space(&repo.id).unwrap();
}

/// `kill` leaves the pane in place; `wait` for the exit sees it end.
#[test]
#[ignore = "starts a real herdr server; run by hand"]
fn a_killed_program_ends_while_its_pane_stays() {
    let Some(server) = LiveServer::start() else {
        return;
    };
    let mux = HerdrMux::new().with_environment(server.socket(), server.home());
    let cwd = server.home().to_path_buf();
    let space = mux.create_space("kill probe", &cwd).unwrap();
    let tab = mux
        .spawn(
            &space.id,
            "sleeper",
            &["sh".to_string(), "-c".to_string(), "sleep 120".to_string()],
            &[],
            &cwd,
        )
        .unwrap();

    // A program that has not ended is not an exit, and waiting for one times
    // out running rather than erroring.
    let running = mux
        .wait(&tab.root_pane, &Waited::Exit, Duration::from_millis(300))
        .unwrap();
    assert_eq!(running, Outcome::running());

    mux.kill(&tab.root_pane).unwrap();
    let mut ended = false;
    for _ in 0..40 {
        match mux.wait(&tab.root_pane, &Waited::Exit, Duration::from_millis(500)) {
            Ok(outcome) if outcome.exited => {
                ended = true;
                break;
            }
            // herdr's interrupt only reaches an agent, so the fallback closes
            // the pane: a pane the host no longer knows is a program that ended.
            Err(error) if error.is_gone() => {
                ended = true;
                break;
            }
            Ok(_) => std::thread::sleep(Duration::from_millis(250)),
            Err(error) => panic!("waiting for the kill: {error}"),
        }
    }
    assert!(ended, "the program in the pane never ended");
    let _ = mux.close_space(&space.id);
}
