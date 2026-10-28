//! The fixture log the projector tests fold.
//!
//! One run, three tasks (one integrated, one failed at a gate, one queued),
//! one dry agent, two escalations and two overrides.

use shprd_core::event::{Actor, Event, EventKind, Finding, ReportStatus, Severity};
use shprd_core::state::RunState;

/// 2023-11-14 22:13:20 UTC: the fixed clock every fixture event hangs off.
pub const NOW: u64 = 1_700_000_000_000;

/// Ten minutes after [`NOW`]: when the dry agent comes back (22:23 UTC).
pub const DRY_UNTIL: u64 = NOW + 600_000;

/// One event, stamped `at_ms` from its sequence number.
fn ev(seq: u64, kind: EventKind) -> Event {
    Event {
        seq,
        at_ms: NOW + seq * 1_000,
        actor: Actor::Engine,
        kind,
    }
}

/// A task with a valid, non-overlapping scope.
fn task(seq: u64, id: &str, title: &str, path: &str) -> Event {
    ev(
        seq,
        EventKind::TaskAdded {
            task: id.to_string(),
            title: title.to_string(),
            scope: vec![path.to_string()],
            acceptance: vec![format!("{id} is done")],
            tags: vec![],
            skills: vec!["tdd".to_string()],
            deps: vec![],
        },
    )
}

/// The whole fixture log.
pub fn events() -> Vec<Event> {
    vec![
        ev(
            1,
            EventKind::RunStarted {
                run_id: "run-7".to_string(),
                repo: "/tmp/repo".to_string(),
                base_branch: "main".to_string(),
                integration_branch: "integration/m1".to_string(),
            },
        ),
        task(2, "M1.1", "event log", "crates/shprd-core"),
        task(3, "M1.2", "views", "crates/shprd-views"),
        task(4, "M1.3", "roster", "crates/shprd-roster"),
        ev(
            5,
            EventKind::Spawned {
                task: "M1.1".to_string(),
                attempt: 1,
                agent: "claude".to_string(),
                model: Some("opus".to_string()),
                worktree: "/tmp/wt1".to_string(),
                branch: "feature/m1-events".to_string(),
            },
        ),
        ev(
            6,
            EventKind::Reported {
                task: "M1.1".to_string(),
                attempt: 1,
                status: ReportStatus::Completed,
                summary: "wrote the log".to_string(),
                tests: vec!["cargo test -p shprd-core: ok".to_string()],
                changed_paths: vec!["crates/shprd-core/src/event.rs".to_string()],
                commit: Some("abc123".to_string()),
                skills_used: vec!["tdd".to_string()],
            },
        ),
        ev(
            7,
            EventKind::GatePassed {
                task: "M1.1".to_string(),
                attempt: 1,
                gate: "lint".to_string(),
                findings: vec![],
                source: None,
            },
        ),
        ev(
            8,
            EventKind::Integrated {
                task: "M1.1".to_string(),
                commits: vec!["abc123".to_string(), "def456".to_string()],
            },
        ),
        ev(
            9,
            EventKind::Spawned {
                task: "M1.2".to_string(),
                attempt: 1,
                agent: "codex".to_string(),
                model: Some("gpt".to_string()),
                worktree: "/tmp/wt2".to_string(),
                branch: "feature/m1-text".to_string(),
            },
        ),
        ev(
            10,
            EventKind::Reported {
                task: "M1.2".to_string(),
                attempt: 1,
                status: ReportStatus::Completed,
                summary: "wrote the views".to_string(),
                tests: vec!["cargo test -p shprd-views: ok".to_string()],
                changed_paths: vec!["crates/shprd-views/src/brief.rs".to_string()],
                commit: Some("aaa111".to_string()),
                skills_used: vec!["tdd".to_string(), "simplify".to_string()],
            },
        ),
        ev(
            11,
            EventKind::GateFailed {
                task: "M1.2".to_string(),
                attempt: 1,
                gate: "clippy".to_string(),
                findings: vec![Finding {
                    severity: Severity::Major,
                    title: "unwrap used".to_string(),
                    location: Some("crates/shprd-views/src/brief.rs:9".to_string()),
                    detail: "replace the unwrap with a match".to_string(),
                }],
                source: None,
            },
        ),
        ev(
            12,
            EventKind::Dry {
                agent: "codex".to_string(),
                reason: "rate limited".to_string(),
                reset_at_ms: Some(DRY_UNTIL),
            },
        ),
        ev(
            13,
            EventKind::Escalated {
                task: None,
                body: "two gates disagree on the panic policy".to_string(),
                default_action: "stop the run".to_string(),
            },
        ),
        ev(
            14,
            EventKind::Escalated {
                task: Some("M1.2".to_string()),
                body: "the clippy finding is cosmetic, integrate anyway?".to_string(),
                default_action: "hold M1.2".to_string(),
            },
        ),
        ev(
            15,
            EventKind::Override {
                task: "M1.2".to_string(),
                rule: "integrate_after_failed_gate".to_string(),
                reason: "the finding is cosmetic".to_string(),
            },
        ),
        ev(
            16,
            EventKind::Override {
                task: String::new(),
                rule: "no_main_push".to_string(),
                reason: "release branch".to_string(),
            },
        ),
    ]
}

/// Folds [`events`] into the state the projectors read.
pub fn state() -> RunState {
    match RunState::fold(&events()) {
        Ok(state) => state,
        Err(err) => panic!("the fixture log must fold: {err}"),
    }
}

/// A state with `count` queued tasks and nothing else, for the length tests.
pub fn many_tasks(count: u64) -> RunState {
    let mut events = vec![ev(
        1,
        EventKind::RunStarted {
            run_id: "run-many".to_string(),
            repo: "/tmp/repo".to_string(),
            base_branch: "main".to_string(),
            integration_branch: "integration/m1".to_string(),
        },
    )];
    for index in 0..count {
        let id = format!("T{index:02}");
        events.push(task(
            index + 2,
            &id,
            "a task with a long enough title",
            "crates/shprd-core",
        ));
    }
    match RunState::fold(&events) {
        Ok(state) => state,
        Err(err) => panic!("the fixture log must fold: {err}"),
    }
}
