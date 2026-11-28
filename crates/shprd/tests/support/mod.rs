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

/// Appends the engine's review of `task`'s attempt 1: a request, then a
/// passing verdict that answers it, as `agent review` and `review-verdict`
/// would record them.
///
/// A worker's own gates never pass a task: only a review does, and a review
/// needs a reviewer tab no test here can open, so the log gets the two events
/// directly.
pub fn pass_review(root: &std::path::Path, task: &str) {
    append_review(root, task, None);
}

/// [`pass_review`], with the worker's gate `failed_gate` failing between the
/// request and the verdict: the task ends Passed with a failed gate behind it.
pub fn pass_review_after_failed_gate(root: &std::path::Path, task: &str, failed_gate: &str) {
    append_review(root, task, Some(failed_gate));
}

#[allow(clippy::expect_used)]
fn append_review(root: &std::path::Path, task: &str, failed_gate: Option<&str>) {
    use shprd_core::event::{Actor, Event, EventKind, ReviewSource};
    let path = root.join(".shprd").join("events.jsonl");
    let mut text = std::fs::read_to_string(&path).expect("read the log");
    let request_seq = text.lines().count() as u64 + 1;
    let mut kinds = vec![EventKind::ReviewRequested {
        task: task.to_string(),
        attempt: 1,
        reviewer: "c1".to_string(),
        degraded: false,
        note: None,
    }];
    if let Some(gate) = failed_gate {
        kinds.push(EventKind::GateFailed {
            task: task.to_string(),
            attempt: 1,
            gate: gate.to_string(),
            findings: Vec::new(),
            source: None,
        });
    }
    kinds.push(EventKind::GatePassed {
        task: task.to_string(),
        attempt: 1,
        gate: "review".to_string(),
        findings: Vec::new(),
        source: Some(ReviewSource {
            reviewer: "c1".to_string(),
            findings_sha256: "ba7816bf8f01cfea".to_string(),
            request_seq,
        }),
    });
    for (offset, kind) in kinds.into_iter().enumerate() {
        let seq = request_seq + offset as u64;
        let event = Event {
            seq,
            at_ms: 1_700_000_000_000 + seq,
            actor: Actor::Engine,
            kind,
        };
        text.push_str(&serde_json::to_string(&event).expect("serialize"));
        text.push('\n');
    }
    std::fs::write(&path, text).expect("append the review");
}
