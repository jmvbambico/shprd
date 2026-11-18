//! The one test that drives a whole task's space on a real Herdr desktop.
//!
//! It is `#[ignore]` because it needs `hc`, Herdr.app and a few seconds: the
//! milestone gate runs it by hand. It never touches the person's own desktop —
//! it builds its own `HC_SESSION` and `HERDR_APP_HOME` under the test's
//! temporary folder and tears both down afterwards, exactly as M7ef §2.1
//! records. M10.1's live test proves the labels reach the window; this one is
//! the host trait over the same desktop, which is what an M10 task actually
//! uses:
//!
//! 1. a task's space is made and carries the name shprd gave it;
//! 2. a terminal started in it with `sh -c 'echo hi; sleep 30'` is read back,
//!    output and all, through `Host::read` and `Host::wait`;
//! 3. the host lists the space and the tab it just made, from the layout;
//! 4. killing the program and closing the space are the two steps a landing
//!    takes, and neither leaves anything behind.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::time::Duration;

use shprd_mux::{AgentLaunch, AgentRole, Host, Waited};
use shprd_mux_hc::{Endpoint, HcHost, Window};

use support::live::Live;

#[test]
#[ignore = "needs a real hc and a private Herdr desktop; run at the milestone gate"]
fn a_task_space_is_made_filled_read_and_closed_on_a_private_window() {
    let mut live = match Live::new() {
        Ok(live) => live,
        Err(reason) => {
            eprintln!("skipped: {reason}");
            return;
        }
    };
    live.start_window()
        .expect("a private Herdr window over this session");

    let endpoint = Endpoint::with_binary(live.hc().to_string_lossy())
        .with_session(live.session())
        .with_directories(live.directories())
        .with_timeout(Duration::from_secs(30));
    // The window must be the one over this session: same core, same home.
    let mut window = Window::from_endpoint(&endpoint);
    for (key, value) in live.environment() {
        window = window.with_variable(key, value);
    }
    let host = HcHost::connect(endpoint, window).expect("the private core answers");
    assert!(host.window_attached(), "the private window answers too");
    assert!(
        host.capabilities().close_space,
        "a desktop host can close a space whole"
    );

    // 1. The space a task works in, named as shprd names it.
    let name = format!("shprd m10.9 {}", std::process::id());
    let space = host
        .create_space(&name, live.root())
        .expect("the window makes the space");
    assert!(!space.id.as_str().is_empty(), "the window gave it an id");
    assert_eq!(space.name, name);
    let found = wait_for_space(&host, &name);
    assert_eq!(found.id, space.id, "the layout holds the space just made");

    // 2. Its worker tab: an hc terminal in that space, labelled into a tab.
    let tab = host
        .spawn(
            &space.id,
            "sh·probe",
            &[
                "/bin/sh".to_string(),
                "-c".to_string(),
                "echo hi; sleep 30".to_string(),
            ],
            &[],
            live.root(),
        )
        .expect("the core starts the terminal");

    // The output is read back both ways a caller uses: as a screen, and as the
    // thing a wait was waiting for.
    let waited = host
        .wait(
            &tab.root_pane,
            &Waited::Output("hi".to_string()),
            Duration::from_secs(20),
        )
        .expect("the core waits");
    assert!(
        waited
            .output
            .as_deref()
            .is_some_and(|text| text.contains("hi")),
        "the wait saw the output: {waited:?}"
    );
    let screen = host
        .read(&tab.root_pane, 50)
        .expect("the core reads the screen");
    assert!(
        screen.contains("hi"),
        "the screen holds the output: {screen:?}"
    );

    // 3. The tab the spawn made is in the layout, found by the label the host
    //    wrote; the window draws a terminal about two seconds after it starts.
    let tabs = wait_for_tab(&host, &space.id, &tab.id);
    assert!(
        tabs.iter().any(|listed| listed.id == tab.id),
        "the layout lists the spawned tab: {tabs:?}"
    );

    // 4. What a landing does: end the program, then close the space whole.
    host.kill(&tab.root_pane).expect("the core kills it");
    host.close_space(&space.id).expect("the window closes it");
    assert!(
        host.find_space(&name)
            .expect("the layout is readable")
            .is_none(),
        "the space is gone from the layout"
    );
}

/// SHPR-23 on a real desktop: a worker started with `spawn_agent` is a labelled
/// terminal with **no tab**: the window draws nothing for it, the host lists it by
/// label, it can be read, and closing its handle stops it. The coordinator is the
/// one agent that keeps a tab, in the repository's space.
#[test]
#[ignore = "needs a real hc and a private Herdr desktop; run at the milestone gate"]
fn a_headless_worker_leaves_no_tab_on_a_private_window() {
    let mut live = match Live::new() {
        Ok(live) => live,
        Err(reason) => {
            eprintln!("skipped: {reason}");
            return;
        }
    };
    live.start_window()
        .expect("a private Herdr window over this session");
    let endpoint = Endpoint::with_binary(live.hc().to_string_lossy())
        .with_session(live.session())
        .with_directories(live.directories())
        .with_timeout(Duration::from_secs(30));
    let mut window = Window::from_endpoint(&endpoint);
    for (key, value) in live.environment() {
        window = window.with_variable(key, value);
    }
    let host = HcHost::connect(endpoint, window).expect("the private core answers");
    assert!(host.capabilities().headless_agents);

    let name = format!("shprd · live {}", std::process::id());
    let space = host
        .create_space(&name, live.root())
        .expect("the window makes the repository's space");
    wait_for_space(&host, &name);
    let argv = |line: &str| vec!["/bin/sh".to_string(), "-c".to_string(), line.to_string()];

    // The coordinator keeps a tab of the space.
    let coordinator = host
        .spawn_agent(&AgentLaunch {
            space: &space.id,
            title: "coordinator",
            argv: &argv("echo coordinator-ready; sleep 60"),
            env: &[],
            cwd: live.root(),
            role: AgentRole::Coordinator,
            task: None,
            program: "sh",
            parent: None,
        })
        .expect("the coordinator starts");
    wait_for_tab(&host, &space.id, &coordinator.id);
    let tabs_before = host.list_tabs(&space.id).expect("tabs").len();
    let spaces_before = host.list_spaces().expect("spaces").len();

    // The worker is the coordinator's child and has no tab.
    let worker = host
        .spawn_agent(&AgentLaunch {
            space: &space.id,
            title: "sh·T1",
            argv: &argv("echo worker-hi; sleep 60"),
            env: &[],
            cwd: live.root(),
            role: AgentRole::Worker,
            task: Some("T1"),
            program: "sh",
            parent: Some(&coordinator.root_pane),
        })
        .expect("the worker starts");
    let waited = host
        .wait(
            &worker.root_pane,
            &Waited::Output("worker-hi".to_string()),
            Duration::from_secs(20),
        )
        .expect("the core waits");
    assert!(waited.output.is_some(), "the worker's output is readable");

    // The window draws a labelled terminal about two seconds after it starts, so
    // give it that long to draw a tab it should not draw.
    std::thread::sleep(Duration::from_secs(4));
    assert_eq!(
        host.list_tabs(&space.id).expect("tabs").len(),
        tabs_before,
        "the worker made no tab"
    );
    assert_eq!(
        host.list_spaces().expect("spaces").len(),
        spaces_before,
        "and no space"
    );
    let listed = host.list_agents().expect("the host lists agents by label");
    let ours = listed
        .iter()
        .find(|agent| agent.tab.root_pane == worker.root_pane)
        .unwrap_or_else(|| panic!("the worker is not listed: {listed:?}"));
    assert_eq!(ours.task.as_deref(), Some("T1"));
    assert_eq!(ours.role, AgentRole::Worker);
    assert_eq!(ours.program, "sh");
    assert!(ours.live);

    // Stopping the handle ends the program. The core keeps a finished terminal's
    // record, so the agent is still listed — as finished, with its output still
    // readable, which is what the Agents pane shows for a while — and the
    // coordinator is untouched.
    host.close_tab(&worker.id).expect("the handle stops it");
    let after = host.list_agents().expect("agents");
    let stopped = after
        .iter()
        .find(|agent| agent.tab.root_pane == worker.root_pane)
        .unwrap_or_else(|| panic!("a finished agent stays listed: {after:?}"));
    assert!(!stopped.live, "the worker's program ended");
    assert!(stopped.ended_ms.is_some());
    assert!(host.read(&worker.root_pane, 5).is_ok());
    assert!(host.read(&coordinator.root_pane, 5).is_ok());
}

/// Wait for the layout to hold the space called `name`.
fn wait_for_space(host: &HcHost, name: &str) -> shprd_mux::Space {
    for _ in 0..100 {
        if let Some(space) = host.find_space(name).expect("the layout is readable") {
            return space;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("the layout never held the space {name:?}");
}

/// Wait for the layout to list the tab `id`, and answer every tab it lists.
fn wait_for_tab(
    host: &HcHost,
    space: &shprd_mux::SpaceId,
    id: &shprd_mux::TabId,
) -> Vec<shprd_mux::Tab> {
    let mut tabs = Vec::new();
    for _ in 0..100 {
        tabs = host.list_tabs(space).expect("the layout is readable");
        if tabs.iter().any(|tab| tab.id == *id) {
            return tabs;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    tabs
}
