//! The client against a fake `hc connect`: real pipes, the recorded framing.
//!
//! Every test here spawns [`support::Fake`] — a Python script speaking the bytes
//! M7ef recorded (plain `hello`, then `channel | kind | length | payload`
//! frames) — so the client is exercised over a real child process and a real
//! stdin/stdout pair without a herdr-core, a session or a person's desktop.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::time::Duration;

use serde_json::json;
use shprd_mux_hc::{AgentState, CORE_MINIMUM, Endpoint, Error, HcClient, LAYOUT_DOCUMENT};
use support::Fake;

/// The shorter timeout the timeout test uses, so the suite stays quick.
const QUICK: Duration = Duration::from_millis(150);

/// An endpoint for a fake, with a timeout long enough for a loaded machine.
fn endpoint(fake: &Fake) -> Endpoint {
    Endpoint::with_binary(fake.binary())
        .with_timeout(Duration::from_secs(5))
        .with_reconnect(false)
}

/// Connect to a fake that answers everything.
async fn open(fake: &Fake) -> HcClient {
    HcClient::connect(endpoint(fake)).await.expect("connects")
}

/// Wait for the fold to satisfy `until`, or fail the test after a few seconds.
async fn wait_until(client: &HcClient, mut until: impl FnMut(&shprd_mux_hc::State) -> bool) {
    for _ in 0..200 {
        if until(&client.state()) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!(
        "the fold never reached the expected state: {:?}",
        client.state()
    );
}

#[tokio::test]
async fn a_call_gets_its_answer_and_the_snapshot_lands_in_the_fold() {
    let fake = Fake::new("normal");
    let client = open(&fake).await;

    let hello = client.hello().await.expect("hello");
    assert_eq!(hello.version, "0.1.21");
    assert_eq!(hello.generation, 1);
    assert_eq!(hello.core_id, "06f3e992");

    let status = client
        .call("server.status", json!({}))
        .await
        .expect("status");
    assert_eq!(status["pid"], 2391);
    assert_eq!(status["version"], "0.1.21");
}

#[tokio::test]
async fn a_refusal_keeps_the_cores_own_kind_not_its_message() {
    let fake = Fake::new("normal");
    let client = open(&fake).await;

    let error = client
        .call("terminal.wobble", json!({}))
        .await
        .expect_err("refused");
    match &error {
        Error::Refused {
            method, kind, hint, ..
        } => {
            assert_eq!(method, "terminal.wobble");
            assert_eq!(kind, "invalid_params");
            assert_eq!(hint.as_deref(), Some("see hc schema"));
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(error.refusal_kind(), Some("invalid_params"));
    assert!(!error.is_unreachable());
}

#[tokio::test]
async fn the_snapshot_from_subscribing_is_in_the_fold_before_connect_returns() {
    let fake = Fake::new("normal");
    let client = open(&fake).await;

    let state = client.state();
    // The fake sends one event straight after the snapshot, so the watermark
    // is already at that event by the time connect returns, or about to be.
    assert!(state.seq >= 300, "{:?}", state.seq);
    let addresses: Vec<&str> = state
        .terminals()
        .map(|terminal| terminal.address.as_str())
        .collect();
    assert_eq!(addresses, vec!["t1y", "t20"]);
    // A snapshot never carries a document, so the fold is waiting for one.
    assert!(state.is_stale());
    assert!(state.pending.contains(LAYOUT_DOCUMENT));
}

#[tokio::test]
async fn a_server_event_reaches_the_fold_without_being_asked() {
    let fake = Fake::new("normal");
    let client = open(&fake).await;

    // The fake sends one `status` event straight after the snapshot.
    wait_until(&client, |state| {
        state
            .terminal("t20")
            .and_then(|terminal| terminal.status.as_ref())
            .is_some_and(|status| status.state.get() == &AgentState::Working)
    })
    .await;

    let state = client.state();
    let terminal = state.terminal("t20").expect("t20 folded");
    assert_eq!(
        terminal.status.as_ref().expect("status").app.as_deref(),
        Some("shprd-probe")
    );
    assert_eq!(state.seq, 301);
}

#[tokio::test]
async fn a_doc_event_sends_the_caller_back_to_the_document_it_names() {
    let fake = Fake::new("doc");
    let client = open(&fake).await;

    wait_until(&client, |state| state.pending.contains(LAYOUT_DOCUMENT)).await;
    let read = client.read_pending_documents().await.expect("reads");
    assert_eq!(read, vec![LAYOUT_DOCUMENT.to_string()]);

    let state = client.state();
    assert!(!state.is_stale());
    assert_eq!(state.spaces.len(), 1);
    assert_eq!(state.spaces[0].id, "spc3f2a");
    let tab = state.tab("spc3f2a", "probe").expect("the probe tab");
    assert_eq!(tab.terminals, vec!["t20"]);
}

#[tokio::test]
async fn a_missed_notice_replaces_the_set_and_re_reads_the_layout() {
    let fake = Fake::new("missed");
    let client = open(&fake).await;

    wait_until(&client, |state| {
        state.seq == 350 && state.terminals.contains_key("t23")
    })
    .await;

    let state = client.state();
    // The stream's own view was replaced, not merged: t1y is gone.
    assert!(!state.terminals.contains_key("t1y"));
    assert!(state.terminals.contains_key("t20"));
    let t23 = state.terminal("t23").expect("t23");
    assert_eq!(t23.agent_state(), AgentState::Blocked);
    assert_eq!(t23.tab_label(), Some("tbprobe"));
    // And the layout is waiting to be read again.
    assert!(state.is_stale());
    assert!(state.pending.contains(LAYOUT_DOCUMENT));
    client.read_pending_documents().await.expect("re-reads");
    assert!(!client.state().is_stale());
}

#[tokio::test]
async fn a_call_that_is_never_answered_times_out_instead_of_hanging() {
    let fake = Fake::new("quiet");
    // Connecting gets the normal budget (a Python child takes a moment to
    // start); the call that is never answered gets the short one.
    let client = open(&fake).await;

    let started = std::time::Instant::now();
    let error = client
        .call_with("terminal.list", json!({}), QUICK)
        .await
        .expect_err("a quiet core times out");
    assert!(matches!(error, Error::Timeout { .. }), "{error}");
    assert!(started.elapsed() >= QUICK, "waited at least the timeout");
    assert!(started.elapsed() < Duration::from_secs(2), "and no longer");
    // The link is still usable: only that call failed.
    assert!(client.generation() >= 1);
}

#[tokio::test]
async fn an_old_core_is_refused_naming_both_versions() {
    let fake = Fake::new("old");
    let error = HcClient::connect(endpoint(&fake))
        .await
        .expect_err("an old core is refused");
    match &error {
        Error::VersionTooOld { found, needed } => {
            assert_eq!(found, "0.1.14");
            assert_eq!(*needed, CORE_MINIMUM);
        }
        other => panic!("expected a version refusal, got {other:?}"),
    }
    let text = error.to_string();
    assert!(text.contains("0.1.14"), "{text}");
    assert!(text.contains(CORE_MINIMUM), "{text}");
    assert!(text.contains("hc update"), "{text}");
}

#[tokio::test]
async fn another_generation_is_refused() {
    let fake = Fake::new("gen2");
    let error = HcClient::connect(endpoint(&fake))
        .await
        .expect_err("refused");
    assert!(
        matches!(
            error,
            Error::Generation {
                found: 2,
                needed: 1
            }
        ),
        "{error}"
    );
}

#[tokio::test]
async fn a_child_that_dies_fails_the_call_and_a_new_one_answers() {
    let fake = Fake::new("die-once");
    let endpoint = endpoint(&fake).with_reconnect(true);
    let client = HcClient::connect(endpoint).await.expect("connects");
    assert_eq!(client.generation(), 1);

    // The first list is answered; the second kills the child mid-call.
    client.terminals().await.expect("the first list");
    let error = client
        .terminals()
        .await
        .expect_err("the child died mid-call");
    assert!(error.is_unreachable(), "{error}");

    // The keeper spawns a new child by itself and the call lands again.
    let mut terminals = None;
    for _ in 0..100 {
        match client.terminals().await {
            Ok(found) => {
                terminals = Some(found);
                break;
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    }
    let terminals = terminals.expect("the call lands after reconnecting");
    assert_eq!(terminals.len(), 2);
    assert!(fake.spawns() >= 2, "a second child was spawned");
    assert!(client.generation() >= 2, "the connection was replaced");
}

#[tokio::test]
async fn a_reconnect_forgets_what_the_dead_connection_said() {
    let fake = Fake::new("die-once");
    let endpoint = endpoint(&fake).with_reconnect(true);
    let client = HcClient::connect(endpoint).await.expect("connects");
    client.terminals().await.expect("the first list");
    let _ = client.terminals().await;

    wait_until(&client, |state| {
        state.seq >= 300 && !state.terminals.is_empty()
    })
    .await;
    assert!(client.state().terminals.contains_key("t20"));
}

#[tokio::test]
async fn a_client_that_never_connects_fails_a_call_instead_of_hanging() {
    // No such binary: the supervisor keeps trying, and a call says so.
    let endpoint = Endpoint::with_binary("/nonexistent/hc-for-shprd-tests").with_timeout(QUICK);
    let client = HcClient::lazy(endpoint);
    let error = client
        .call("terminal.list", json!({}))
        .await
        .expect_err("no core");
    assert!(matches!(error, Error::Unreachable(_)), "{error}");
    client.close().await;
}

#[tokio::test]
async fn the_endpoint_never_hands_the_child_a_stray_core() {
    let fake = Fake::new("normal");
    // A session the endpoint names is passed; the terminal and core ids the
    // environment has are not, or the child would address another core.
    let endpoint = endpoint(&fake).with_session("shprd-test");
    assert_eq!(
        endpoint.environment(),
        vec![("HC_SESSION", "shprd-test".to_string())]
    );
    for dropped in ["HC_SOCKET", "HC_TERMINAL_ID", "HC_CORE_ID"] {
        assert!(Endpoint::DROPPED.contains(&dropped), "{dropped}");
    }
    let client = HcClient::connect(endpoint).await.expect("connects");
    assert_eq!(client.endpoint().session.as_deref(), Some("shprd-test"));
}

#[tokio::test]
async fn dropping_the_client_kills_its_child_and_stops_the_keeper() {
    let fake = Fake::new("normal");
    let endpoint = endpoint(&fake).with_reconnect(true);
    let client = HcClient::connect(endpoint).await.expect("connects");
    assert_eq!(fake.spawns(), 1);
    drop(client);

    // The child is killed with the link, so the fake sees its stdin close and
    // exits; the keeper holds only a weak handle and so stops retrying.
    for _ in 0..60 {
        if fake.children_finished() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(fake.spawns(), 1, "no reconnect after the last client went");
}
