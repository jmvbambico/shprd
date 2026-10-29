//! Event types: the durable vocabulary of the append-only run log.
//!
//! Every state change in a run is one [`Event`]. Current state is a fold over
//! the log, and every view is rendered from the log, so these types are the
//! only way anything about a run is recorded.

use serde::{Deserialize, Serialize};

/// Identifies a task inside a run.
pub type TaskId = String;

/// Identifies an agent (a worker slot in the roster).
pub type AgentId = String;

/// Who caused an event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Actor {
    /// The engine itself: spawning, gating, integrating.
    Engine,
    /// The orchestrator process driving the run.
    Orchestrator,
    /// A specific agent.
    Agent {
        /// The agent's id.
        id: AgentId,
    },
    /// A human operator.
    Human,
}

/// One line of the append-only run log.
///
/// `seq` starts at 1 and increases by exactly one per event. `at_ms` is the
/// wall-clock time the event was appended, in milliseconds since the Unix
/// epoch. The variant payload is flattened into the same JSON object as
/// `seq`, `at_ms` and `actor`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// Monotonic position of this event in the log, starting at 1.
    pub seq: u64,
    /// Wall-clock time the event was appended, ms since the Unix epoch.
    pub at_ms: u64,
    /// Who caused the event.
    pub actor: Actor,
    /// What happened.
    #[serde(flatten)]
    pub kind: EventKind,
}

/// Outcome of a single task attempt, as reported by the agent that ran it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportStatus {
    /// The attempt finished its scope and is ready to integrate.
    Completed,
    /// The attempt stopped on something it could not resolve.
    Blocked,
    /// The attempt failed.
    Failed,
}

/// How bad a gate finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Must be fixed before integration.
    Critical,
    /// Should be fixed before integration.
    Major,
    /// Worth fixing, not blocking.
    Minor,
    /// Cosmetic.
    Trivial,
}

/// One finding reported by a gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    /// How bad it is.
    pub severity: Severity,
    /// One-line title.
    pub title: String,
    /// Where it is, e.g. `crates/shprd-core/src/log.rs:42`.
    pub location: Option<String>,
    /// The full detail, including what to do about it.
    pub detail: String,
}

/// One command of the run-wide gate, and how it went.
///
/// The step is recorded whole so a reviewer can see what ran and why it failed
/// without the machine that ran it: the command string, its exit code (`None`
/// when a timeout killed it) and the tail of what it printed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GateStep {
    /// The command, as the config spelled it.
    pub command: String,
    /// Its exit code, or `None` when a timeout killed it.
    pub exit_code: Option<i32>,
    /// How long it took, in milliseconds.
    pub duration_ms: u64,
    /// The last lines it printed, capped at [`GATE_OUTPUT_TAIL_CHARS`].
    pub output_tail: String,
}

/// How many characters of a gate step's output the event keeps.
///
/// A failing build can print megabytes and the log is folded on every command,
/// so the tail is capped rather than kept whole.
pub const GATE_OUTPUT_TAIL_CHARS: usize = 4000;

/// What happened to a resource shprd owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceAction {
    /// shprd created it.
    Created,
    /// shprd kept it but took it out of service.
    Archived,
    /// shprd deleted it.
    Removed,
}

/// A kind of resource shprd creates and is therefore allowed to clean up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    /// A git worktree.
    Worktree,
    /// A local branch.
    Branch,
    /// A branch on a remote.
    RemoteBranch,
    /// An archive ref holding a run's history.
    ArchiveRef,
    /// An exported bundle.
    Bundle,
}

/// Where a review verdict came from: the reviewer, and the findings it decided.
///
/// A verdict is only a verdict when something proves a reviewer wrote it, so the
/// gate result carries the reviewer the dispatch named and the SHA-256 of the
/// exact findings bytes the engine read. A caller that records a review gate
/// without this is refused, and a verdict recorded before this field existed
/// folds with `None` and reads as unproven.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewSource {
    /// The agent the review was dispatched to, as [`EventKind::ReviewRequested`]
    /// recorded it.
    pub reviewer: AgentId,
    /// SHA-256 of the findings file's bytes, as lowercase hex.
    pub findings_sha256: String,
    /// The `seq` of the [`EventKind::ReviewRequested`] this verdict answers.
    ///
    /// A restarted review records a fresh request for the same attempt, and it
    /// may name the same reviewer, so who reviewed does not tell the two
    /// requests apart. The request's position does: a verdict written for a
    /// request that has since been superseded is refused instead of being read
    /// as the current review's own.
    ///
    /// Defaulted to `0` so a log written before this field existed still
    /// folds; no request has that seq, so a verdict recorded now names one.
    #[serde(default)]
    pub request_seq: u64,
}

/// What happened. Serialized with a `kind` tag alongside the [`Event`] fields.
///
/// Unknown-field rejection is not enabled here: `deny_unknown_fields` cannot
/// be combined with the internally tagged representation used by this enum
/// (nor with the flattened field on [`Event`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventKind {
    /// A run was started.
    RunStarted {
        /// Run identifier.
        run_id: String,
        /// Repository the run works on.
        repo: String,
        /// Branch the run was cut from.
        base_branch: String,
        /// Branch landed work is integrated on.
        integration_branch: String,
    },
    /// A task was added to the run's plan.
    TaskAdded {
        /// The task.
        task: TaskId,
        /// Human-readable title.
        title: String,
        /// What the task must change.
        scope: Vec<String>,
        /// What must be true when it is done.
        acceptance: Vec<String>,
        /// Free-form tags.
        tags: Vec<String>,
        /// Skills the agent is told to load.
        skills: Vec<String>,
        /// Tasks that must land before this one.
        deps: Vec<TaskId>,
    },
    /// An agent was spawned for a task attempt.
    Spawned {
        /// The task.
        task: TaskId,
        /// Attempt number, starting at 1.
        attempt: u32,
        /// The agent that was spawned.
        agent: AgentId,
        /// Model the agent runs on.
        model: Option<String>,
        /// Worktree the agent works in.
        worktree: String,
        /// Branch the agent commits on.
        branch: String,
    },
    /// An agent reported the outcome of an attempt.
    Reported {
        /// The task.
        task: TaskId,
        /// Attempt number.
        attempt: u32,
        /// Outcome.
        status: ReportStatus,
        /// What the agent says it did.
        summary: String,
        /// Test commands the agent ran, with their result.
        tests: Vec<String>,
        /// Paths the agent changed.
        changed_paths: Vec<String>,
        /// Commit the agent made, if any.
        commit: Option<String>,
        /// Skills the agent used.
        skills_used: Vec<String>,
    },
    /// A gate passed for an attempt.
    GatePassed {
        /// The task.
        task: TaskId,
        /// Attempt number.
        attempt: u32,
        /// Gate name.
        gate: String,
        /// Findings the gate raised, if any.
        findings: Vec<Finding>,
        /// Provenance, for a review gate only.
        ///
        /// Defaulted so a log written before provenance existed still folds.
        #[serde(default)]
        source: Option<ReviewSource>,
    },
    /// A gate failed for an attempt.
    GateFailed {
        /// The task.
        task: TaskId,
        /// Attempt number.
        attempt: u32,
        /// Gate name.
        gate: String,
        /// Why it failed.
        findings: Vec<Finding>,
        /// Provenance, for a review gate only.
        ///
        /// Defaulted so a log written before provenance existed still folds.
        #[serde(default)]
        source: Option<ReviewSource>,
    },
    /// A task's work was integrated.
    Integrated {
        /// The task.
        task: TaskId,
        /// Commits that were landed.
        commits: Vec<String>,
    },
    /// A review was handed to a reviewer for an attempt.
    ///
    /// `degraded` is the fact a landing decision needs: it records that the
    /// reviewer shares the author's vendor, so no independent review happened.
    /// Recording it here keeps `shprd land` from guessing it out of prose.
    ReviewRequested {
        /// The task.
        task: TaskId,
        /// Attempt number.
        attempt: u32,
        /// The agent string the review runs on.
        reviewer: String,
        /// True when the reviewer shares the author's vendor.
        degraded: bool,
    },
    /// An agent was put on cooldown (rate limit, quota, auth).
    Dry {
        /// The agent.
        agent: AgentId,
        /// Why it is dry.
        reason: String,
        /// When it is expected back, ms since the Unix epoch.
        reset_at_ms: Option<u64>,
    },
    /// A dry agent was re-admitted (its cooldown was cleared).
    Available {
        /// The agent.
        agent: AgentId,
        /// Why it is usable again.
        reason: String,
    },
    /// A task was handed from one agent to another.
    HandedOff {
        /// The task.
        task: TaskId,
        /// The agent handing it over.
        from_agent: AgentId,
        /// The agent taking it over.
        to_agent: AgentId,
        /// The checkpoint the new agent should resume from.
        checkpoint: String,
        /// Why it was handed over, in the words of the verdict that caused it.
        ///
        /// A worker process exiting hands the attempt on without drying the
        /// agent, so the reason is the only record that the attempt's worker
        /// was lost. Defaulted so a log written before it existed still folds.
        #[serde(default)]
        reason: Option<String>,
    },
    /// Something needs a human decision.
    Escalated {
        /// The task it concerns, when it is not run-wide.
        task: Option<TaskId>,
        /// What needs deciding.
        body: String,
        /// The action taken if nobody answers.
        default_action: String,
    },
    /// A human overrode a rule.
    Override {
        /// The task, or run-wide rule when empty.
        task: TaskId,
        /// The rule that was overridden.
        rule: String,
        /// Why it was overridden.
        reason: String,
    },
    /// A resource was created, archived or removed.
    Resource {
        /// What happened to it.
        action: ResourceAction,
        /// What sort of resource it is.
        ///
        /// Serialized as `resource`, because `kind` is taken by the tag that
        /// names the event variant.
        #[serde(rename = "resource")]
        kind: ResourceKind,
        /// Its name: a worktree path, a branch name, an archive ref.
        name: String,
        /// The task it belongs to, when it belongs to one.
        task: Option<TaskId>,
    },
    /// The run-wide gate ran over the integration branch.
    ///
    /// One event per `shprd gate` run, holding every step it reached: the
    /// whole verdict is one log line, so a reader never has to join steps.
    FullGate {
        /// The integration branch's head the gate ran on.
        head: String,
        /// True when every step it ran passed.
        passed: bool,
        /// The steps it ran, in order.
        steps: Vec<GateStep>,
    },
    /// A pull request was opened, or its body refreshed, for the run's
    /// integration branch.
    PrOpened {
        /// The forge's number for the pull request.
        number: u64,
        /// The URL a human opens to read, approve or merge it.
        url: String,
        /// The branch the pull request proposes to merge.
        head: String,
    },
    /// The run's pull request was merged.
    PrMerged {
        /// The forge's number for the pull request.
        number: u64,
        /// The head that was merged.
        head: String,
    },
    /// A human approved the run's pull request in shprd itself.
    ///
    /// A solo maintainer cannot approve their own pull request on GitHub, so
    /// the approval a merge policy waits for can also be recorded here. It
    /// names the head it was given for: an approval of an older head says
    /// nothing about the commits now on top of it.
    Approved {
        /// The forge's number for the pull request.
        number: u64,
        /// The integration head the human approved.
        head_sha: String,
    },
    /// shprd started the orchestrator in a pane.
    ///
    /// The process group is the orchestrator's identity for the rest of the
    /// run: a host that restores the tab and relaunches the CLI itself gives
    /// the pane a different group, which is how `up` tells a session it
    /// started from one it did not.
    OrchestratorLaunched {
        /// The title of the orchestrator's tab.
        title: String,
        /// The pane the orchestrator runs in.
        pane: String,
        /// The pane's foreground process group at launch.
        process_group: u32,
    },
    /// shprd started the watchdog in a pane.
    ///
    /// The watchdog is the process that turns a reviewer's findings into a
    /// verdict and hands off a dry worker, so `up` starts it the way it starts
    /// the orchestrator: the pane and its process group are what a later `up`
    /// checks before it trusts the pane, and what run end closes.
    WatchLaunched {
        /// The pane the watchdog runs in.
        pane: String,
        /// The pane's foreground process group at launch.
        ///
        /// `None` is a host that cannot tell. It is not evidence against the
        /// pane, so a later `up` leaves it alone rather than splitting a second
        /// watchdog off the chat tab.
        process_group: Option<u32>,
    },
    /// The watchdog told the orchestrator about the log up to this point.
    ///
    /// The orchestrator is an interactive CLI: it only acts when it is given
    /// input, so the watchdog prompts its pane. Two workers can report and the
    /// orchestrator sits idle until a human notices, unless something tells it.
    /// This event is that delivery's watermark: it names the last sequence the
    /// orchestrator was told about, so a restarted watchdog neither replays old
    /// notices nor drops new ones.
    OrchestratorNotified {
        /// The last log sequence the orchestrator was told about.
        up_to_seq: u64,
    },
    /// The run finished.
    RunFinished {
        /// The tasks the run ended with unfinished, when a human abandoned
        /// them.
        ///
        /// Empty for a run that ended with its work landed. Non-empty is the
        /// one explicit choice to end a run anyway (`--abandon`): the tasks
        /// keep the branches their work sits on, and `up` says so when it
        /// archives the run. Defaulted, so a log written before the field
        /// existed still folds.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        abandoned: Vec<TaskId>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn to_json<T: Serialize>(value: &T) -> String {
        match serde_json::to_string(value) {
            Ok(text) => text,
            Err(err) => panic!("serialize failed: {err}"),
        }
    }

    fn from_json<T: serde::de::DeserializeOwned>(text: &str) -> T {
        match serde_json::from_str(text) {
            Ok(value) => value,
            Err(err) => panic!("deserialize failed: {err}\n{text}"),
        }
    }

    fn finding() -> Finding {
        Finding {
            severity: Severity::Major,
            title: "clippy warning".to_string(),
            location: Some("crates/shprd-core/src/log.rs:42".to_string()),
            detail: "unwrap used".to_string(),
        }
    }

    /// Every kind, with non-trivial field values.
    fn every_kind() -> Vec<EventKind> {
        vec![
            EventKind::RunStarted {
                run_id: "run-1".to_string(),
                repo: "/tmp/repo".to_string(),
                base_branch: "main".to_string(),
                integration_branch: "integration/m1".to_string(),
            },
            EventKind::TaskAdded {
                task: "M1.1".to_string(),
                title: "event log".to_string(),
                scope: vec!["crates/shprd-core".to_string()],
                acceptance: vec!["round trip".to_string()],
                tags: vec!["core".to_string()],
                skills: vec!["tdd".to_string()],
                deps: vec!["M0.2".to_string()],
            },
            EventKind::Spawned {
                task: "M1.1".to_string(),
                attempt: 1,
                agent: "claude".to_string(),
                model: Some("opus".to_string()),
                worktree: "/tmp/wt".to_string(),
                branch: "feature/m1-events".to_string(),
            },
            EventKind::Reported {
                task: "M1.1".to_string(),
                attempt: 1,
                status: ReportStatus::Completed,
                summary: "done".to_string(),
                tests: vec!["cargo test".to_string()],
                changed_paths: vec!["crates/shprd-core/src/event.rs".to_string()],
                commit: Some("abc123".to_string()),
                skills_used: vec!["tdd".to_string()],
            },
            EventKind::GatePassed {
                task: "M1.1".to_string(),
                attempt: 1,
                gate: "review".to_string(),
                findings: vec![],
                source: Some(ReviewSource {
                    reviewer: "codex/m1".to_string(),
                    findings_sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb41"
                        .to_string(),
                    request_seq: 5,
                }),
            },
            EventKind::GateFailed {
                task: "M1.1".to_string(),
                attempt: 2,
                gate: "tests".to_string(),
                findings: vec![finding()],
                source: None,
            },
            EventKind::Integrated {
                task: "M1.1".to_string(),
                commits: vec!["abc123".to_string(), "def456".to_string()],
            },
            EventKind::ReviewRequested {
                task: "M1.1".to_string(),
                attempt: 1,
                reviewer: "codex".to_string(),
                degraded: true,
            },
            EventKind::Dry {
                agent: "codex".to_string(),
                reason: "rate limited".to_string(),
                reset_at_ms: Some(1_700_000_000_000),
            },
            EventKind::HandedOff {
                task: "M1.1".to_string(),
                from_agent: "claude".to_string(),
                to_agent: "codex".to_string(),
                checkpoint: "after log.rs".to_string(),
                reason: Some("agent process exited".to_string()),
            },
            EventKind::Escalated {
                task: None,
                body: "two gates disagree".to_string(),
                default_action: "stop the run".to_string(),
            },
            EventKind::Override {
                task: "M1.1".to_string(),
                rule: "no_main_push".to_string(),
                reason: "release branch".to_string(),
            },
            EventKind::Resource {
                action: ResourceAction::Created,
                kind: ResourceKind::Worktree,
                name: "/tmp/wt/M1.1".to_string(),
                task: Some("M1.1".to_string()),
            },
            EventKind::FullGate {
                head: "abc1234".to_string(),
                passed: false,
                steps: vec![
                    GateStep {
                        command: "cargo test --workspace".to_string(),
                        exit_code: Some(101),
                        duration_ms: 1200,
                        output_tail: "test result: FAILED".to_string(),
                    },
                    GateStep {
                        command: "cargo fmt --all --check".to_string(),
                        exit_code: None,
                        duration_ms: 30_000,
                        output_tail: "timed out".to_string(),
                    },
                ],
            },
            EventKind::PrOpened {
                number: 7,
                url: "https://example.invalid/pr/7".to_string(),
                head: "shprd/r1/int".to_string(),
            },
            EventKind::PrMerged {
                number: 7,
                head: "abc1234".to_string(),
            },
            EventKind::Approved {
                number: 7,
                head_sha: "abc1234".to_string(),
            },
            EventKind::OrchestratorLaunched {
                title: "★ claude·orchestrator".to_string(),
                pane: "w1:p2".to_string(),
                process_group: 88158,
            },
            EventKind::OrchestratorNotified { up_to_seq: 42 },
            EventKind::RunFinished {
                abandoned: Vec::new(),
            },
        ]
    }

    #[test]
    fn every_kind_round_trips() {
        let kinds = every_kind();
        assert_eq!(kinds.len(), 20, "one case per EventKind variant");

        for (index, kind) in kinds.into_iter().enumerate() {
            let event = Event {
                seq: index as u64 + 1,
                at_ms: 1_700_000_000_000 + index as u64,
                actor: Actor::Engine,
                kind,
            };
            let text = to_json(&event);
            assert!(
                !text.contains('\n'),
                "an event must be a single JSON line: {text}"
            );
            let back: Event = from_json(&text);
            assert_eq!(event, back, "round trip changed the event: {text}");
        }
    }

    #[test]
    fn a_gate_recorded_before_provenance_still_parses() {
        // Old logs predate `source`: the field defaults to None, and a review
        // gate from then is an unproven verdict, not a parse failure.
        let text = r#"{"seq":7,"at_ms":5,"actor":{"type":"engine"},"kind":"gate_passed","task":"M1.1","attempt":1,"gate":"review","findings":[]}"#;
        let event: Event = from_json(text);
        match &event.kind {
            EventKind::GatePassed { source, gate, .. } => {
                assert_eq!(gate, "review");
                assert_eq!(*source, None);
            }
            other => panic!("expected a passed gate, got {other:?}"),
        }
    }

    #[test]
    fn kind_is_flattened_into_the_event_object() {
        let event = Event {
            seq: 1,
            at_ms: 5,
            actor: Actor::Agent {
                id: "claude".to_string(),
            },
            kind: EventKind::RunFinished {
                abandoned: Vec::new(),
            },
        };
        let text = to_json(&event);
        assert!(text.contains("\"kind\":\"run_finished\""), "{text}");
        assert!(
            text.contains("\"actor\":{\"type\":\"agent\",\"id\":"),
            "{text}"
        );
        assert!(text.contains("\"seq\":1"), "{text}");
        assert!(text.contains("\"at_ms\":5"), "{text}");
        assert!(
            !text.contains("\"kind\":{"),
            "kind must not be nested: {text}"
        );
    }

    #[test]
    fn actor_variants_round_trip() {
        let actors = [
            Actor::Engine,
            Actor::Orchestrator,
            Actor::Agent {
                id: "claude".to_string(),
            },
            Actor::Human,
        ];
        for actor in actors {
            let text = to_json(&actor);
            let back: Actor = from_json(&text);
            assert_eq!(actor, back, "{text}");
        }
        assert_eq!(to_json(&Actor::Engine), "{\"type\":\"engine\"}");
    }

    #[test]
    fn report_status_and_severity_are_snake_case() {
        assert_eq!(to_json(&ReportStatus::Completed), "\"completed\"");
        assert_eq!(to_json(&ReportStatus::Blocked), "\"blocked\"");
        assert_eq!(to_json(&ReportStatus::Failed), "\"failed\"");
        assert_eq!(to_json(&Severity::Critical), "\"critical\"");
        assert_eq!(to_json(&Severity::Major), "\"major\"");
        assert_eq!(to_json(&Severity::Minor), "\"minor\"");
        assert_eq!(to_json(&Severity::Trivial), "\"trivial\"");
    }

    #[test]
    fn finding_rejects_unknown_fields() {
        let text = "{\"severity\":\"minor\",\"title\":\"t\",\"location\":null,\
                    \"detail\":\"d\",\"extra\":1}";
        assert!(serde_json::from_str::<Finding>(text).is_err());
    }
}
