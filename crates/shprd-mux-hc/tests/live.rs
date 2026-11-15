//! The one test that talks to a real herdr-core, and therefore Herdr desktop.
//!
//! It is `#[ignore]` because it needs `hc`, a window and a few seconds: the
//! milestone gate runs it by hand. It never touches the person's own desktop —
//! it builds its own `HC_SESSION` and `HERDR_APP_HOME` under the test's
//! temporary folder, and tears both down afterwards, exactly as M7ef §2.1
//! records.
//!
//! Its case is the one M7ef proved by hand (§2.8): a terminal labelled
//! `herdr.space` / `herdr.tab` started outside the window appears in that space
//! and tab. So the test lists the real spaces, takes one terminal carrying the
//! shprd labels and reads the layout the window keeps, which is what M10.2's
//! placement needs before it can trust the labels.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::time::Duration;

use shprd_mux_hc::{Endpoint, HcClient, LAYOUT_DOCUMENT, Placement, State};

use support::live::Live;

#[tokio::test]
#[ignore = "needs a real hc and a private Herdr desktop; run at the milestone gate"]
async fn a_private_core_lists_its_spaces_and_places_a_labelled_terminal() {
    let mut live = Live::new().expect("a private HC_SESSION and HERDR_APP_HOME");
    live.start_window()
        .expect("a private Herdr window over this session");

    // `server.status` is the health check, and it also starts the core for a
    // fresh session on macOS (M7ef §2.3), which is what makes this test
    // self-contained.
    let client = HcClient::connect(
        Endpoint::with_binary(live.hc().to_string_lossy())
            .with_session(live.session())
            // The same XDG dirs the window uses, so this core keeps its socket
            // and its terminals in this test's folder — not the person's
            // `~/.local/state/herdr-core`.
            .with_directories(live.directories())
            .with_timeout(Duration::from_secs(30)),
    )
    .await
    .expect("connects to the private core");

    let hello = client.hello().await.expect("hello");
    assert!(
        shprd_mux_hc::version_at_least(&hello.version, shprd_mux_hc::CORE_MINIMUM),
        "the real core is {} and the floor is {}",
        hello.version,
        shprd_mux_hc::CORE_MINIMUM
    );
    assert_eq!(hello.generation, 1);

    let status = client
        .call("server.status", serde_json::json!({}))
        .await
        .expect("server.status");
    assert!(status.get("pid").is_some(), "the core answers: {status}");

    // A terminal in a space and tab this test made up, exactly as shprd places
    // a task's agent.
    let space = format!("shprd-m10-1-{}", std::process::id());
    let placement = Placement::new(&space, "worker");
    let labels = placement.labels();
    let run = client
        .call(
            "terminal.run",
            serde_json::json!({
                "argv": ["/bin/sh", "-c", "sleep 30"],
                "cwd": live.root().to_string_lossy(),
                "name": format!("shprd-m10-1-{}", std::process::id()),
                "labels": labels,
                "keep": false,
            }),
        )
        .await
        .expect("terminal.run");
    let address = run
        .get("address")
        .and_then(|address| address.as_str())
        .expect("the terminal's address")
        .to_string();

    // The core reports it with the labels shprd wrote, which is what the
    // window reads to place it (M7ef §2.8).
    let terminal = client
        .call("terminal.get", serde_json::json!({"terminal": address}))
        .await
        .expect("terminal.get");
    assert_eq!(
        terminal
            .get("labels")
            .and_then(|labels| labels.get("herdr.space"))
            .and_then(|space| space.as_str()),
        Some(placement.space.as_str())
    );
    assert_eq!(
        terminal
            .get("labels")
            .and_then(|labels| labels.get("herdr.tab"))
            .and_then(|tab| tab.as_str()),
        Some(placement.tab.as_str())
    );

    // And the fold knows it without being told: the event stream carries it.
    let state = wait_for(&client, |state: &State| {
        state
            .terminal(&address)
            .is_some_and(|terminal| terminal.space_label() == Some(placement.space.as_str()))
    })
    .await;
    let space_terminals = state
        .terminals()
        .filter(|terminal| terminal.space_label() == Some(placement.space.as_str()))
        .count();
    assert_eq!(space_terminals, 1);

    // The layout document is readable, and a space the window made is in it.
    client.read_layout().await.expect("doc.get herdr-app");
    let state = client.state();
    assert!(!state.is_stale(), "the layout has been read");
    assert!(
        state.doc_versions.contains_key(LAYOUT_DOCUMENT),
        "the layout's version is known"
    );

    // Clean up: kill and remove the terminal this test made.
    let _ = client
        .call("terminal.kill", serde_json::json!({"terminal": address}))
        .await;
    let removed = client
        .call("terminal.rm", serde_json::json!({"terminal": address}))
        .await;
    assert!(removed.is_ok(), "the terminal is removed: {removed:?}");
    client.close().await;
}

/// Wait for the fold, or give up after a few seconds.
async fn wait_for(client: &HcClient, mut until: impl FnMut(&State) -> bool) -> State {
    for _ in 0..200 {
        let state = client.state();
        if until(&state) {
            return state;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the fold never saw the terminal: {:?}", client.state());
}
