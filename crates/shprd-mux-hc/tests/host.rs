//! `HcHost` over the fake `hc connect` and a stand-in for the window.
//!
//! Every verb of the host trait is driven here against
//! [`tests/support/fake_hc.py`](support/fake_hc.py) — a real child process
//! speaking the frames and shapes M7ef recorded — and against
//! [`support::Window`], which answers the window's `herdr-app` commands. So the
//! two halves of Herdr desktop are exercised without a core, a window, a session
//! or the person's desktop. What a verb sent is read back from the fake's
//! request log, which is the only way to prove a label, an environment or a
//! missing `caller` arrives as the trait promises.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::path::Path;
use std::time::Duration;

use serde_json::Value;
use shprd_mux::{
    AgentState, Direction, Host, Key, MuxError, MuxEvent, Outcome, PaneId, SpaceId, TabId, Waited,
};
use shprd_mux_hc::{Endpoint, HcHost, Window};

use support::Fake;

/// A host over a fake core, with no window: the terminals-only case.
fn host(mode: &str) -> (Fake, HcHost) {
    let fake = Fake::new(mode);
    let endpoint = Endpoint::with_binary(fake.binary())
        .with_timeout(Duration::from_secs(5))
        .with_reconnect(false);
    let window = Window::with_binary("/nonexistent/herdr-app").with_timeout(Duration::from_secs(1));
    let host = HcHost::connect(endpoint, window).expect("the fake core answers");
    (fake, host)
}

/// A host over a fake core and a fake window: the whole desktop case.
fn host_with_window(mode: &str) -> (Fake, support::Window, HcHost) {
    let fake = Fake::new(mode);
    let window = support::Window::new();
    let endpoint = Endpoint::with_binary(fake.binary())
        .with_timeout(Duration::from_secs(5))
        .with_reconnect(false);
    let host = HcHost::connect(endpoint, window.window()).expect("the fake core answers");
    (fake, window, host)
}

/// Every request the fake was asked, whole.
fn requests(fake: &Fake) -> Vec<Value> {
    std::fs::read_to_string(fake.dir().join("requests.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).expect("a request line"))
        .collect()
}

/// The one request whose method is `method`, or a panic naming what was asked.
fn request(fake: &Fake, method: &str) -> Value {
    requests(fake)
        .into_iter()
        .find(|request| request.get("method").and_then(Value::as_str) == Some(method))
        .unwrap_or_else(|| panic!("the host never called {method}"))
}

/// Write what the fake's `terminal.read` should answer.
fn screen(fake: &Fake, text: &str) {
    std::fs::write(fake.dir().join("screen"), text).expect("writes the screen");
}

const SPACE: &str = "spc3f2a";

#[test]
fn without_a_window_the_flags_say_exactly_what_hc_has() {
    let (_fake, host) = host("normal");
    let caps = host.capabilities();
    assert!(caps.spawn, "a terminal can be started");
    assert!(caps.keys);
    assert!(caps.wait, "terminal.wait answers in one call");
    assert!(caps.kill);
    assert!(caps.doc, "herdr-core has documents");
    assert!(caps.exec, "machine.exec is a one-shot command");
    assert!(caps.agent_state);
    assert!(caps.list_spaces);
    assert!(caps.resumable_events, "the fold keeps reading");
    // A space or a tab is the window's, and there is no window here.
    assert!(!caps.split);
    assert!(!caps.close_pane);
    assert!(!caps.close_space);
    assert!(!host.window_attached());
    // Neither herdr-core nor the window has these.
    assert!(!caps.pane_grants);
    assert!(!caps.human_ask);
    assert!(!caps.split_ratio);
}

#[test]
fn a_window_that_answers_adds_the_layout_verbs() {
    let (_fake, _window, host) = host_with_window("normal");
    let caps = host.capabilities();
    assert!(host.window_attached());
    assert!(caps.split);
    assert!(caps.close_pane);
    assert!(caps.close_space);
    assert!(!caps.split_ratio, "pane.split has no ratio");
}

#[test]
fn a_space_and_its_tabs_are_read_from_the_layout_document() {
    let (_fake, host) = host("normal");

    let space = host
        .find_space("shprd probe")
        .expect("the layout is readable")
        .expect("the window's space is in it");
    assert_eq!(space.id.as_str(), SPACE);
    assert_eq!(space.name, "shprd probe");

    // The id is the label a space can be found by too, which is how a space
    // shprd made is found again.
    assert_eq!(
        host.find_space(SPACE)
            .expect("the layout is readable")
            .expect("the id finds it")
            .id
            .as_str(),
        SPACE
    );
    assert!(
        host.find_space("nobody")
            .expect("the layout is readable")
            .is_none()
    );

    let listed = host.list_spaces().expect("the layout is readable");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "shprd probe");

    let tabs = host
        .list_tabs(&SpaceId::new(SPACE))
        .expect("the layout is readable");
    assert_eq!(tabs.len(), 2);
    assert_eq!(tabs[0].root_pane.as_str(), "t1y");
    assert_eq!(tabs[1].id.as_str(), "tbprobe");
    assert_eq!(tabs[1].title, "probe");
    assert_eq!(tabs[1].root_pane.as_str(), "t20");
}

#[test]
fn a_space_verb_without_a_window_is_unsupported_rather_than_faked() {
    let (_fake, host) = host("normal");
    let error = host
        .create_space("shprd · repo", Path::new("/tmp"))
        .expect_err("no window makes no space");
    assert!(
        matches!(error, MuxError::Unsupported("making a space")),
        "{error}"
    );
    assert!(matches!(
        host.close_space(&SpaceId::new(SPACE)),
        Err(MuxError::Unsupported("closing a space"))
    ));
}

#[test]
fn a_space_the_window_makes_is_named_where_the_person_sees_it() {
    let (fake, window, host) = host_with_window("normal");
    let space = host
        .create_space("shprd · repo · r1 · T1", Path::new("/work/t1"))
        .expect("the window makes a space");
    assert_eq!(space.id.as_str(), "spcnew");
    assert_eq!(space.name, "shprd · repo · r1 · T1");

    let calls = window.calls();
    assert!(
        calls.iter().any(|call| call.starts_with("do space.new")
            && call.contains("--name shprd · repo · r1 · T1")
            && call.contains("--folder /work/t1")),
        "the window was asked for a named space: {calls:?}"
    );
    // The window's answer is the id the space is found by afterwards.
    assert!(fake.requests().contains(&"doc.get".to_string()));
}

#[test]
fn a_space_the_layout_does_not_hold_is_not_found() {
    let (_fake, _window, host) = host_with_window("normal");
    assert!(matches!(
        host.close_space(&SpaceId::new("spgone")),
        Err(MuxError::NotFound { .. })
    ));
    assert!(matches!(
        host.close_tab(&TabId::new("tbgone")),
        Err(MuxError::NotFound { .. })
    ));
}

#[test]
fn the_layout_verbs_ask_the_window_by_the_id_the_layout_knows() {
    let (_fake, window, host) = host_with_window("normal");
    host.close_space(&SpaceId::new(SPACE)).expect("closes");
    host.focus_space(&SpaceId::new(SPACE)).expect("shows");
    host.rename_tab(&TabId::new("tbprobe"), "reviewer")
        .expect("renames");
    host.focus_tab(&TabId::new("tbprobe")).expect("shows");
    host.close_tab(&TabId::new("tbprobe")).expect("closes");

    let calls = window.calls();
    for wanted in [
        "do space.close --id spc3f2a --confirmed true",
        "do space.select --id spc3f2a",
        "do tab.rename --id tbprobe --name reviewer",
        "do tab.select --id tbprobe",
        "do tab.close --id tbprobe --confirmed true",
    ] {
        assert!(
            calls.iter().any(|call| call == wanted),
            "{wanted}: {calls:?}"
        );
    }
}

#[test]
fn a_spawn_starts_a_labelled_terminal_and_answers_its_tab() {
    let (fake, host) = host("normal");
    let argv = vec!["pi".to_string(), "--model".to_string(), "x".to_string()];
    let env = vec![
        ("SHPRD_TASK".to_string(), Some("T1".to_string())),
        ("HERDR_PANE_ID".to_string(), Some("p7".to_string())),
    ];
    let tab = host
        .spawn(
            &SpaceId::new(SPACE),
            "pi·T1",
            &argv,
            &env,
            Path::new("/work/t1"),
        )
        .expect("the core starts it");

    assert_eq!(tab.id.as_str(), "pi·T1", "the tab label is the title");
    assert_eq!(tab.title, "pi·T1");
    assert_eq!(tab.root_pane.as_str(), "t100");

    let sent = request(&fake, "terminal.run");
    let params = &sent["params"];
    // The labels are what place the terminal in the task's space and tab
    // (M7ef §2.8): the space by id, the tab by the label.
    assert_eq!(params["labels"]["herdr.space"], SPACE);
    assert_eq!(params["labels"]["herdr.tab"], "pi·T1");
    assert_eq!(params["cwd"], "/work/t1");
    assert_eq!(params["argv"][0], "pi");
    assert_eq!(params["env"]["SHPRD_TASK"], "T1");
}

#[test]
fn a_spawn_gives_its_tab_the_name_a_person_reads() {
    let (_fake, window, host) = host_with_window("normal");
    host.spawn(
        &SpaceId::new(SPACE),
        "claude·T2",
        &["claude".to_string()],
        &[],
        Path::new("/work/t2"),
    )
    .expect("the core starts it");
    let calls = window.calls();
    assert!(
        calls
            .iter()
            .any(|call| call == "do tab.rename --id claude·T2 --name claude·T2"),
        "{calls:?}"
    );
}

#[test]
fn a_launch_that_drops_a_variable_goes_through_env() {
    // herdr-core's `env` cannot unset a variable, so a launch that must drop one
    // is wrapped in `/usr/bin/env`, which does. Nothing is re-parsed as a shell:
    // the program is still the last element of a vector.
    let (fake, host) = host("normal");
    let tab = host
        .spawn(
            &SpaceId::new(SPACE),
            "pi·T1",
            &["pi".to_string(), "--model".to_string(), "a b".to_string()],
            &[
                ("HERDR_PANE_ID".to_string(), None),
                ("SHPRD_TASK".to_string(), Some("T1".to_string())),
            ],
            Path::new("/work/t1"),
        )
        .expect("the core starts it");
    assert_eq!(tab.root_pane.as_str(), "t100");
    let sent = request(&fake, "terminal.run");
    let argv: Vec<&str> = sent["params"]["argv"]
        .as_array()
        .expect("an argv array")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(
        argv,
        vec![
            "/usr/bin/env",
            "-u",
            "HERDR_PANE_ID",
            "SHPRD_TASK=T1",
            "pi",
            "--model",
            "a b"
        ],
        "the drop comes first, then the sets, then the program"
    );
    assert!(
        sent["params"].get("env").is_none(),
        "the wrapper carries the environment: {sent}"
    );
    // The labels still place the terminal in the task's tab.
    assert_eq!(sent["params"]["labels"]["herdr.tab"], "pi·T1");
}

#[test]
fn text_is_sent_as_the_persons_own_typing() {
    let (fake, host) = host("normal");
    host.write(&PaneId::new("t20"), "hello there", true)
        .expect("the core takes the text");
    let sent = request(&fake, "terminal.send");
    assert_eq!(sent["params"]["terminal"], "t20");
    assert_eq!(sent["params"]["text"], "hello there");
    // No `caller`: the text is the person's (M7ef §2.10).
    assert!(
        sent["params"].get("caller").is_none_or(Value::is_null),
        "{sent}"
    );

    // herdr-core's send always presses enter, so typing without submitting is
    // refused rather than quietly submitted — and the flags say so first, which
    // is what a caller plans from.
    assert!(!host.capabilities().type_without_submit);
    let error = host
        .write(&PaneId::new("t20"), "half a line", false)
        .expect_err("no half-sent text on this host");
    assert!(matches!(error, MuxError::Unsupported(_)), "{error}");
}

#[test]
fn keys_are_pressed_with_the_words_every_host_accepts() {
    let (fake, host) = host("normal");
    host.keys(
        &PaneId::new("t20"),
        &[Key::Enter, Key::CtrlC, Key::Char('a')],
    )
    .expect("the core presses them");
    let sent = request(&fake, "terminal.keys");
    assert_eq!(
        sent["params"]["keys"],
        serde_json::json!(["enter", "ctrl+c", "a"])
    );
}

#[test]
fn a_screen_is_read_back_as_the_core_renders_it() {
    let (fake, host) = host("normal");
    screen(&fake, "hi\nthere\n");
    let text = host.read(&PaneId::new("t20"), 10).expect("the core reads");
    assert!(text.starts_with("hi"), "{text}");
    assert_eq!(request(&fake, "terminal.read")["params"]["lines"], 10);
}

#[test]
fn a_wait_for_output_answers_with_what_matched() {
    let (fake, host) = host("normal");
    screen(&fake, "hi\n");
    let outcome = host
        .wait(
            &PaneId::new("t20"),
            &Waited::Output("hi".to_string()),
            Duration::from_secs(2),
        )
        .expect("the core waits");
    assert_eq!(
        outcome.output.as_deref(),
        Some("hi\n"),
        "the screen came back"
    );
    assert!(!outcome.exited);

    // Output that never appears is a timeout, not an error, and the program is
    // still running.
    let outcome = host
        .wait(
            &PaneId::new("t20"),
            &Waited::Output("later".to_string()),
            Duration::from_millis(500),
        )
        .expect("a timeout is normal");
    assert!(!outcome.exited);
    assert!(outcome.output.is_none());
}

#[test]
fn a_wait_for_exit_reads_the_cores_own_end() {
    let (_fake, host) = host("normal");
    let outcome = host
        .wait(
            &PaneId::new("t20"),
            &Waited::Exit,
            Duration::from_millis(300),
        )
        .expect("the core waits");
    assert!(!outcome.exited, "a live terminal keeps running");

    host.kill(&PaneId::new("t20")).expect("the core kills it");
    let outcome = host
        .wait(
            &PaneId::new("t20"),
            &Waited::Exit,
            Duration::from_millis(300),
        )
        .expect("the core waits");
    assert!(outcome.exited, "a finished terminal has ended");
}

#[test]
fn a_killed_terminal_reads_as_done_and_ended() {
    let (_fake, host) = host("normal");
    host.kill(&PaneId::new("t20")).expect("the core kills it");
    assert_eq!(
        host.agent_state(&PaneId::new("t20")).expect("the state"),
        AgentState::Done
    );
    let ended = host
        .exit_of(&PaneId::new("t20"))
        .expect("the core answers")
        .expect("it ended");
    assert!(ended.exited);
}

#[test]
fn a_terminal_the_core_never_had_is_not_found() {
    let (_fake, host) = host("normal");
    assert!(matches!(
        host.read(&PaneId::new("t999"), 5),
        Err(MuxError::NotFound { .. })
    ));
    assert!(matches!(
        host.kill(&PaneId::new("t999")),
        Err(MuxError::NotFound { .. })
    ));
}

#[test]
fn a_state_the_host_has_no_opinion_about_is_unknown_not_idle() {
    let (_fake, host) = host("normal");
    // t1y is live and never reported a status.
    assert_eq!(
        host.agent_state(&PaneId::new("t1y")).expect("the state"),
        AgentState::Unknown
    );
    // t20 reported `working` in the stream the subscription folded.
    assert_eq!(
        host.agent_state(&PaneId::new("t20")).expect("the state"),
        AgentState::Working
    );
    assert!(
        host.pane_process_group(&PaneId::new("t20"))
            .expect("a pid")
            .is_some()
    );
}

#[test]
fn a_document_is_written_compare_and_set() {
    let (fake, host) = host("normal");
    let name = host
        .doc(&SpaceId::new(SPACE), "brief", "# the brief")
        .expect("the core stores it");
    assert_eq!(name, "shprd.spc3f2a.brief");
    let written = request(&fake, "doc.set");
    assert_eq!(written["params"]["name"], "shprd.spc3f2a.brief");
    assert_eq!(written["params"]["value"], "# the brief");
    // Compare-and-set: the write names the version it read (zero: never set).
    assert_eq!(written["params"]["version"], 0);
}

#[test]
fn a_one_shot_command_runs_in_the_core() {
    let (fake, host) = host("normal");
    let outcome = host
        .exec(&["echo".to_string(), "ok".to_string()], Path::new("/tmp"))
        .expect("the core runs it");
    assert!(outcome.exited);
    assert_eq!(outcome.exit_code, Some(0));
    assert_eq!(outcome.output.as_deref(), Some("exec ok"));
    assert_eq!(request(&fake, "machine.exec")["params"]["argv"][0], "echo");
}

#[test]
fn a_split_is_asked_of_the_window_by_the_pane_a_terminal_sits_in() {
    let (_fake, window, host) = host_with_window("normal");
    // `pane.split` is answered with a pane the layout never learns about, so
    // the fallback is the pane id the window gave.
    let made = host
        .split(
            &PaneId::new("t20"),
            Direction::Right,
            0.5,
            Path::new("/work/t2"),
        )
        .expect("the window splits");
    assert_eq!(made.as_str(), "pnew");
    let calls = window.calls();
    assert!(
        calls.iter().any(|call| call.starts_with("do pane.split")
            && call.contains("--direction right")
            && call.contains("--pane p4")),
        "the layout's pane for t20 is p4: {calls:?}"
    );
}

#[test]
fn a_subscription_reports_a_status_that_changed_after_the_host_was_built() {
    let (_fake, host) = host("status-later");
    // Asking for the terminal is what makes the fake report the change; the
    // fold then holds it before anybody subscribes to it.
    assert_eq!(
        host.agent_state(&PaneId::new("t20")).expect("the state"),
        AgentState::Blocked
    );
    let events = host
        .subscribe(Duration::from_secs(2))
        .expect("the fold answers");
    assert!(
        events.iter().any(|event| matches!(
            event,
            MuxEvent::AgentState { pane, state }
                if pane.as_str() == "t20" && *state == AgentState::Blocked
        )),
        "{events:?}"
    );
    // Reported once: a second subscription has nothing new to say.
    let again = host
        .subscribe(Duration::from_millis(200))
        .expect("the fold answers");
    assert!(again.is_empty(), "{again:?}");
}

#[test]
fn a_subscription_reports_a_terminal_that_ended() {
    let (_fake, host) = host("normal");
    host.kill(&PaneId::new("t20")).expect("the core kills it");
    let events = host.subscribe(Duration::from_secs(2)).expect("answers");
    assert!(
        events.iter().any(|event| matches!(
            event,
            MuxEvent::PaneExited { pane } if pane.as_str() == "t20"
        )),
        "{events:?}"
    );
}

#[test]
fn a_core_that_is_not_there_is_an_error_a_person_can_read() {
    let endpoint = Endpoint::with_binary("/nonexistent/hc")
        .with_timeout(Duration::from_secs(1))
        .with_reconnect(false);
    let error = HcHost::connect(endpoint, Window::with_binary("/nonexistent/herdr-app"))
        .expect_err("there is no core");
    let text = error.to_string();
    assert!(
        text.contains("cannot start `/nonexistent/hc connect`"),
        "{text}"
    );
    assert!(matches!(error, MuxError::Command { .. }), "{error}");
}

#[test]
fn the_probe_names_the_cores_version() {
    let fake = Fake::new("normal");
    let endpoint = Endpoint::with_binary(fake.binary()).with_reconnect(false);
    let version = HcHost::core_version(&endpoint, Duration::from_secs(5)).expect("it answers");
    assert_eq!(version, "0.1.21");
    assert!(Outcome::running().exit_code.is_none());
}
