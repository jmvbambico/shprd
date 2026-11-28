//! The worker brief's ask route comes from the program's harness.
//!
//! `[asks] supported` in `data/harness/<program>.toml` is the only fact that
//! decides whether a worker is told to ask with its CLI's own question tool and
//! let the hook raise the card, or to raise the card itself with
//! `shprd ask raise`. These tests read the shipped harness files rather than a
//! list of program names, so the two never drift apart: a program whose hook
//! support changes gets the other wording with no code edit.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use shprd_agents::harness::Harness;
use shprd_core::state::{Task, TaskStatus};
use shprd_views::brief::{AskRoute, BriefInput, task_brief};

/// The shipped `data/harness` directory.
fn harness_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/harness")
}

/// One shipped program's harness, loaded the way the engine loads it.
fn harness(program: &str) -> Harness {
    let path = harness_dir().join(format!("{program}.toml"));
    match Harness::load(&path) {
        Ok(harness) => harness,
        Err(err) => panic!("{} should load: {err}", path.display()),
    }
}

/// Every shipped program, with whether its harness names an ask hook.
fn shipped_programs() -> Vec<(String, bool)> {
    let entries = match std::fs::read_dir(harness_dir()) {
        Ok(entries) => entries,
        Err(err) => panic!("{} should be readable: {err}", harness_dir().display()),
    };
    let mut programs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
            continue;
        }
        let harness = match Harness::load(&path) {
            Ok(harness) => harness,
            Err(err) => panic!("{} should load: {err}", path.display()),
        };
        programs.push((harness.id().to_string(), harness.asks().supported));
    }
    programs.sort();
    programs
}

/// A task with nothing in it that would make the brief longer than it needs to
/// be: the route is the only thing these tests read.
fn task() -> Task {
    Task {
        id: "T1".to_string(),
        title: "Raise a card when a decision is the person's".to_string(),
        scope: vec!["crates/shprd".to_string()],
        acceptance: vec!["the person sees the question".to_string()],
        tags: Vec::new(),
        skills: Vec::new(),
        deps: Vec::new(),
        status: TaskStatus::Queued,
        attempts: Vec::new(),
        overrides: Vec::new(),
        dropped_reason: None,
        last_status: None,
        pr: None,
        approved_head: None,
        nothing_to_merge: None,
        fallbacks: Vec::new(),
        added_at_ms: 0,
        touched_at_ms: 0,
    }
}

/// The brief a worker on `program` is handed, with the route its harness says.
fn brief_for(program: &str) -> String {
    let harness = harness(program);
    let plan = task();
    task_brief(&BriefInput {
        task: &plan,
        run: "m9",
        attempt: 1,
        agent: harness.id(),
        model: None,
        branch: "shprd/m9/T1",
        worktree: "/home/me/.shprd/wt/repo/m9/T1",
        skills: &[],
        gates: &[],
        archive: None,
        prompt: None,
        ask: AskRoute::of_harness(harness.asks().supported),
        proof: true,
        settings: None,
        role: None,
        warnings: &[],
        program_line: None,
    })
}

#[test]
fn a_program_with_an_ask_hook_keeps_the_question_tool_wording() {
    let claude = harness("claude");
    assert!(
        claude.asks().supported,
        "claude ships an ask hook, so this test needs another hook program"
    );
    let text = brief_for("claude");
    assert!(
        text.contains("This program's CLI has an ask hook: ask with your CLI's own question tool"),
        "{text}"
    );
    assert!(
        !text.contains("shprd ask raise"),
        "a program with a hook must not be sent to the verb its hook runs: {text}"
    );
}

#[test]
fn a_program_without_an_ask_hook_is_given_the_raise_verb() {
    let pi = harness("pi");
    assert!(
        !pi.asks().supported,
        "pi ships no ask hook, so this test needs another bare program"
    );
    let text = brief_for("pi");
    assert!(
        text.contains("This program's CLI has no ask hook"),
        "{text}"
    );
    assert!(
        text.contains("shprd ask raise --kind question"),
        "the brief names the verb to run: {text}"
    );
    assert!(
        text.contains("--option"),
        "the brief shows how to offer choices: {text}"
    );
    assert!(
        !text.contains("ask with your CLI's own question tool"),
        "a program with no hook must not be told to use its own tool: {text}"
    );
}

/// Every shipped program gets the route its own harness names, so the brief and
/// the harness files cannot drift apart.
#[test]
fn every_shipped_program_gets_the_route_its_harness_names() {
    let programs = shipped_programs();
    let with_hook = programs.iter().filter(|(_, supported)| *supported).count();
    let without_hook = programs.len() - with_hook;
    assert!(with_hook > 0, "some shipped program names an ask hook");
    assert!(without_hook > 0, "some shipped program names no ask hook");
    for (program, supported) in programs {
        let text = brief_for(&program);
        if supported {
            assert!(
                text.contains("has an ask hook"),
                "{program} names a hook, so its brief must say so: {text}"
            );
            assert!(!text.contains("shprd ask raise"), "{program}: {text}");
        } else {
            assert!(
                text.contains("has no ask hook"),
                "{program} names no hook, so its brief must say so: {text}"
            );
            assert!(
                text.contains("shprd ask raise --kind question"),
                "{program} raises its card with the verb: {text}"
            );
        }
    }
}
