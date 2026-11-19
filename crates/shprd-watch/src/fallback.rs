//! The provider fallback ladder: nudges at 1, 2, 4 and 8 minutes, then one
//! backup handover.
//!
//! Ported from Shepherdr's provider retry and backup agent:
//!
//! - Source: <https://github.com/eliasstravik/shepherdr>
//! - Paths: `mods/tasks/bin/herdr_task/provider.py` (`PROVIDER_WAITS`,
//!   `NUDGE`, `provider_error`, `provider_cut`) and
//!   `mods/tasks/bin/herdr_task/takeover.py` (`backup_for`, `handover`,
//!   `takes_over`, `head`)
//! - Commit: `a3f327a` (tag `v3.1.34`)
//! - License: MIT — Copyright (c) 2026 Elias Stravik
//!
//! Nothing here reads a clock, a record or a terminal: the caller hands in the
//! transcript-derived facts ([`AgentFacts`]), the ledger's memory of what was
//! already done ([`State`]), the clock reading and the machine's sign-in table
//! ([`Config`]), and gets back the [`Action`]s to take. The tick executes them,
//! which is what keeps the timing testable with plain numbers and stops a pass
//! from sleeping through a wait.
//!
//! The rules, in Shepherdr's order:
//!
//! - a turn its provider cut off is retried after 1, 2, 4 and 8 minutes; the
//!   fifth cut, or a reason waiting cannot fix, is the end of the ladder;
//! - a reviewer is never nudged (nor handed a backup): it is not the task's
//!   worker;
//! - nothing is sent while the agent is working, and nothing is sent twice for
//!   the same cut;
//! - a cut older than [`STALE_CUT_MS`] when first seen is left to the person;
//! - when the ladder is over, the task is handed to the next untried program
//!   line (the worker's chain): one handover per line, never to a program the
//!   task already ran on, and only to a program installed and signed in on this
//!   machine. When the program that took over is cut off too, the next untried
//!   line takes over; when every line was tried (or none can start), the task is
//!   a human's again.

use std::collections::{BTreeMap, BTreeSet};

use crate::records::{Cut, ErrorKind, Verdict};

/// The waits between nudges, in milliseconds: Shepherdr's
/// `PROVIDER_WAITS = [1, 2, 4, 8]` minutes (`provider.py:15`).
pub const PROVIDER_WAITS_MS: [u64; 4] = [60_000, 120_000, 240_000, 480_000];

/// How old a cut may be when first seen before it is left to the person, ms
/// (Shepherdr's `STALE_CUT_MS`, 30 minutes; `tick.py:409-438`).
pub const STALE_CUT_MS: u64 = 30 * 60_000;

/// The words shprd sends to tell an agent to carry on, with the provider's
/// reason in the parentheses. Word for word Shepherdr's `NUDGE`
/// (`provider.py:59-61`).
pub const NUDGE: &str = "Your last turn was cut off by the model's provider ({}). \
Carry on where you left off within your existing authorization.";

/// The role an agent fills. Only a worker's or coordinator's cut is retried.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Role {
    /// A task's worker, the role a backup may replace.
    #[default]
    Worker,
    /// The space's coordinator: retried, but never replaced.
    Coordinator,
    /// A reviewer: never nudged, never backed up.
    Reviewer,
}

impl Role {
    /// The key a fallback list is stored under, `None` for a reviewer.
    pub fn key(self) -> Option<&'static str> {
        match self {
            Self::Worker => Some("worker"),
            Self::Coordinator => Some("coordinator"),
            Self::Reviewer => None,
        }
    }
}

/// The branch of the role's fallback list a backup is chosen from.
///
/// This is the seam M9.3 fills in: today the tick builds `worker` from
/// `roster.workers`, and a role with its own `fallback` list will pass it
/// under the same key.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    /// Waits between nudges, in milliseconds. Defaults to
    /// [`PROVIDER_WAITS_MS`].
    pub waits_ms: Vec<u64>,
    /// Programs to hand a task over to, in order, by role key (`worker`,
    /// `coordinator`). Entries may be `program/model` strings; only the
    /// program is read here.
    pub fallbacks: BTreeMap<String, Vec<String>>,
    /// How many leading entries of a role's list are its own program lines (the
    /// rest are the configured workers, appended). A task moves to the line after
    /// the one it runs on only when it runs on one of those lines; a role missing
    /// here counts every entry as a line.
    pub lines: BTreeMap<String, usize>,
    /// Every program the machine has a harness for and a binary for.
    pub installed: BTreeSet<String>,
    /// Every program that answered its own sign-in probe.
    pub signed_in: BTreeSet<String>,
}

impl Config {
    /// The waits in force, falling back to Shepherdr's schedule.
    pub fn waits(&self) -> &[u64] {
        if self.waits_ms.is_empty() {
            &PROVIDER_WAITS_MS
        } else {
            &self.waits_ms
        }
    }

    /// The fallback programs configured for `role`.
    pub fn fallbacks_for(&self, role: Role) -> &[String] {
        role.key()
            .and_then(|key| self.fallbacks.get(key))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }
}

/// What the ledger already recorded about one agent's cuts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentState {
    /// Cut ids a nudge was already sent for.
    pub nudged: BTreeSet<String>,
    /// Cut ids already given up on, so a pass does not repeat itself.
    pub gave_up: BTreeSet<String>,
}

/// A handover the ledger already recorded, successful or not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupDone {
    /// The program the task was running.
    pub from: String,
    /// The program chosen to take over.
    pub to: String,
}

/// What the ledger already recorded about one task.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskState {
    /// The ladder's memory per agent, keyed by program id.
    pub agents: BTreeMap<String, AgentState>,
    /// The handovers this task already had, oldest first: one per program line.
    pub backups: Vec<BackupDone>,
}

/// Everything the ledger already recorded, keyed by task.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct State {
    /// Per-task memory.
    pub tasks: BTreeMap<String, TaskState>,
}

/// The text a backup handover carries, as data so it can be rendered without
/// a terminal, a file or a clock.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Handover {
    /// The task's title, so the newcomer knows what it is finishing.
    pub title: String,
    /// The worktree or folder the task runs in.
    pub where_: String,
    /// The branch the task commits on, when it has one.
    pub branch: Option<String>,
    /// The branch's own commits so far, one per line.
    pub commits: String,
    /// The uncommitted changes, as `git status --short` prints them.
    pub changes: String,
    /// The conversation so far, latest last.
    pub chat_tail: String,
}

/// Transcript-derived facts about one live agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentFacts {
    /// The task it works on.
    pub task: String,
    /// The attempt number it continues.
    pub attempt: u32,
    /// The program it runs, e.g. `claude`.
    pub agent: String,
    /// The role it fills.
    pub role: Role,
    /// Whether the host says it is working right now.
    pub working: bool,
    /// The cut its record ends on, if one does.
    pub cut: Option<Cut>,
    /// What a backup would need to take over.
    pub handover: Handover,
}

/// What a pass decided to do about one cut.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Tell the agent its turn was cut off and it should carry on.
    Nudge {
        /// The task.
        task: String,
        /// The attempt it continues.
        attempt: u32,
        /// The program to write to.
        agent: String,
        /// The message to send.
        text: String,
        /// The cut this answers.
        cut: String,
        /// What kind of provider trouble it was.
        kind: ErrorKind,
        /// Why, in a few words.
        reason: String,
        /// Which try this is.
        tries: u32,
    },
    /// Hand the task to another program, once.
    Backup {
        /// The task.
        task: String,
        /// The attempt that continues in the same worktree.
        attempt: u32,
        /// The program handing it over.
        from: String,
        /// The program taking over.
        to: String,
        /// The cut that ended the ladder.
        cut: String,
        /// What kind of provider trouble it was.
        kind: ErrorKind,
        /// Why the handover happens.
        reason: String,
        /// The brief the newcomer reads.
        handover: Handover,
    },
    /// Stop retrying and ask a person.
    GiveUp {
        /// The task.
        task: String,
        /// The attempt involved.
        attempt: u32,
        /// The program that cannot go on.
        agent: String,
        /// The cut that ended the ladder.
        cut: String,
        /// What kind of provider trouble it was.
        kind: ErrorKind,
        /// Why it needs the person, in one line.
        reason: String,
        /// Which try ended the ladder.
        tries: u32,
    },
}

impl Action {
    /// The task this action is about.
    pub fn task(&self) -> &str {
        match self {
            Self::Nudge { task, .. } | Self::Backup { task, .. } | Self::GiveUp { task, .. } => {
                task
            }
        }
    }

    /// The action's name, as a tick's result line spells it.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Nudge { .. } => "nudge",
            Self::Backup { .. } => "backup",
            Self::GiveUp { .. } => "give_up",
        }
    }
}

/// How one cut reads on the ladder: whether it is over, why, and when the next
/// try is due while waiting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retry {
    /// Whether waiting can no longer fix it.
    pub over: bool,
    /// Why, in a few words: the provider reason, with ", still after N tries"
    /// when a passing error exhausted the schedule.
    pub reason: String,
    /// When the next nudge is due, in epoch milliseconds, while waiting.
    pub due_ms: Option<u64>,
}

/// Reads one cut against a wait schedule, the way Shepherdr's `retry_one`
/// does.
pub fn retry(cut: &Cut, waits: &[u64]) -> Retry {
    let passing = cut.verdict() == Verdict::Passing;
    let waiting = passing && (cut.tries as usize) <= waits.len();
    let base = head(cut.reason());
    if !waiting {
        let reason = if passing {
            format!("{base}, still after {} tries", cut.tries)
        } else {
            base.to_string()
        };
        return Retry {
            over: true,
            reason,
            due_ms: None,
        };
    }
    let wait = waits
        .get((cut.tries as usize).saturating_sub(1))
        .copied()
        .unwrap_or_default();
    Retry {
        over: false,
        reason: base.to_string(),
        due_ms: Some(cut.at_ms.saturating_add(wait)),
    }
}

/// Plans every ladder action for one pass, in task then agent order.
///
/// The same facts and state always produce the same actions, and an action the
/// ledger already holds is not planned again.
pub fn plan(state: &State, facts: &[AgentFacts], now_ms: u64, config: &Config) -> Vec<Action> {
    let mut ordered: Vec<&AgentFacts> = facts.iter().collect();
    ordered.sort_by(|a, b| {
        a.task
            .cmp(&b.task)
            .then_with(|| a.agent.cmp(&b.agent))
            .then_with(|| a.attempt.cmp(&b.attempt))
    });
    let mut actions = Vec::new();
    for fact in ordered {
        let Some(cut) = &fact.cut else {
            continue;
        };
        // A reviewer is not the task's worker: no nudge, no backup.
        if fact.role == Role::Reviewer {
            continue;
        }
        let task = state.tasks.get(&fact.task);
        let agent = task.and_then(|task| task.agents.get(&fact.agent));
        let backups: &[BackupDone] = task.map_or(&[], |task| task.backups.as_slice());
        // The agent a handover already replaced is on its way out, and its cut
        // is the old program's problem, not the task's.
        if backups.iter().any(|backup| backup.from == fact.agent) {
            continue;
        }
        if agent.is_some_and(|agent| agent.gave_up.contains(&cut.id)) {
            continue;
        }
        let outcome = retry(cut, config.waits());
        // A cut older than the stale window, first seen now, is old news.
        if !outcome.over
            && now_ms.saturating_sub(cut.at_ms) > STALE_CUT_MS
            && !agent.is_some_and(|agent| agent.nudged.contains(&cut.id))
        {
            actions.push(give_up(fact, cut, outcome.reason, "stale", config));
            continue;
        }
        if outcome.over {
            if takes_over(cut) {
                // Every program the task already ran on, or was handed to
                // (even when the handover failed to start), is tried.
                let tried: BTreeSet<&str> = backups
                    .iter()
                    .flat_map(|backup| [backup.from.as_str(), backup.to.as_str()])
                    .collect();
                match choose_backup(config, fact.role, &fact.agent, &tried) {
                    Some(to) => actions.push(Action::Backup {
                        task: fact.task.clone(),
                        attempt: fact.attempt,
                        from: fact.agent.clone(),
                        to,
                        cut: cut.id.clone(),
                        kind: cut.kind,
                        reason: outcome.reason,
                        handover: fact.handover.clone(),
                    }),
                    None => {
                        let why = match backups.last() {
                            Some(last) if !has_untried(config, fact.role, &fact.agent, &tried) => {
                                format!(
                                    "every line was tried: {} took over from {}",
                                    last.to, last.from
                                )
                            }
                            _ => no_backup_why(config, fact.role, &fact.agent, &tried),
                        };
                        actions.push(give_up(fact, cut, outcome.reason, &why, config));
                    }
                }
            } else {
                actions.push(give_up(fact, cut, outcome.reason, "", config));
            }
            continue;
        }
        // Never into words the agent is producing, and never the same cut
        // twice.
        if fact.working || agent.is_some_and(|agent| agent.nudged.contains(&cut.id)) {
            continue;
        }
        let Some(due) = outcome.due_ms else {
            continue;
        };
        if now_ms < due {
            continue;
        }
        actions.push(Action::Nudge {
            task: fact.task.clone(),
            attempt: fact.attempt,
            agent: fact.agent.clone(),
            text: nudge_text(&outcome.reason),
            cut: cut.id.clone(),
            kind: cut.kind,
            reason: outcome.reason,
            tries: cut.tries,
        });
    }
    actions
}

/// The program that takes over from `from`, if one may.
///
/// The role's list is the worker's program lines, in order. A task that was
/// running line N moves to line N+1, and when no later line can take it, to an
/// earlier one. A program in `tried` (the lines the task already ran on or was
/// handed to) and the program the task runs are skipped, so the chain walks to
/// each line once; only a program signed in on this machine is chosen.
pub fn choose_backup(
    config: &Config,
    role: Role,
    from: &str,
    tried: &BTreeSet<&str>,
) -> Option<String> {
    let list = config.fallbacks_for(role);
    let lines = role
        .key()
        .and_then(|key| config.lines.get(key))
        .copied()
        .unwrap_or(list.len())
        .min(list.len());
    let start = list[..lines]
        .iter()
        .position(|raw| program_of(raw) == from)
        .map_or(0, |at| at + 1);
    for raw in list[start..].iter().chain(&list[..start]) {
        let program = program_of(raw);
        if program != from
            && !tried.contains(program)
            && config.installed.contains(program)
            && config.signed_in.contains(program)
        {
            return Some(program.to_string());
        }
    }
    None
}

/// Whether any line is left that was not tried (installed or not).
fn has_untried(config: &Config, role: Role, from: &str, tried: &BTreeSet<&str>) -> bool {
    config
        .fallbacks_for(role)
        .iter()
        .map(|raw| program_of(raw))
        .any(|program| program != from && !tried.contains(program))
}

/// Whether a cut is one only another program gets past.
///
/// Shepherdr's `takes_over`: an exhausted allowance, a lost login, or a
/// passing error whose tries ran out ("still after N tries", `takeover.py:125-140`).
pub fn takes_over(cut: &Cut) -> bool {
    matches!(cut.kind, ErrorKind::Quota | ErrorKind::Auth) || cut.verdict() == Verdict::Passing
}

/// Why no backup could be chosen, in Shepherdr's few words.
fn no_backup_why(config: &Config, role: Role, from: &str, tried: &BTreeSet<&str>) -> String {
    let mut why = String::from("no backup");
    for raw in config.fallbacks_for(role) {
        let program = program_of(raw);
        if program == from || tried.contains(program) {
            continue;
        }
        why = if !config.installed.contains(program) {
            String::from("backup not installed")
        } else if !config.signed_in.contains(program) {
            String::from("backup signed out")
        } else {
            // Installed and signed in would have been chosen.
            String::from("no backup")
        };
    }
    why
}

/// The program part of a `program` or `program/model` agent string.
fn program_of(raw: &str) -> &str {
    raw.split('/').next().unwrap_or(raw)
}

/// One give-up action, with the provider reason head and a reason tail.
fn give_up(fact: &AgentFacts, cut: &Cut, reason: String, why: &str, _config: &Config) -> Action {
    let reason = head(&reason);
    let reason = if why.is_empty() {
        reason.to_string()
    } else {
        format!("{reason}, {why}")
    };
    Action::GiveUp {
        task: fact.task.clone(),
        attempt: fact.attempt,
        agent: fact.agent.clone(),
        cut: cut.id.clone(),
        kind: cut.kind,
        reason,
        tries: cut.tries,
    }
}

/// A provider reason without its reset time or try count: "Out of allowance",
/// "Provider overloaded" (Shepherdr's `head`, `takeover.py:108-123`).
pub fn head(reason: &str) -> &str {
    let reason = reason.split(" until ").next().unwrap_or(reason);
    reason.split(", still after ").next().unwrap_or(reason)
}

/// The carry-on message, with `reason` in the provider's words.
pub fn nudge_text(reason: &str) -> String {
    NUDGE.replace("{}", &reason.to_lowercase())
}

/// The handover brief a backup starts with.
///
/// The opening and the context are Shepherdr's `takeover.handover`
/// (`takeover.py:180-199`), with the newcomer line from its `handoff()`
/// (`launch.py:514-530`); the tag is `shprd-backup` rather than
/// `herdr-backup`, which is this program's name for the same fact.
pub fn handover_brief(
    task: &str,
    from: &str,
    to: &str,
    reason: &str,
    handover: &Handover,
) -> String {
    let where_ = match &handover.branch {
        Some(branch) => format!(
            "the same worktree ({}) and branch {branch}",
            handover.where_
        ),
        None => format!("the same folder ({})", handover.where_),
    };
    let attribute = |text: &str| {
        text.replace('&', "&amp;")
            .replace('"', "&quot;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    format!(
        "<shprd-backup from=\"{from}\" to=\"{to}\" why=\"{why}\">\n\
You are taking over task {task} (\"{title}\") from another agent: you are the backup agent for it, \
and {from_label} can't go on ({why}). Continue only the existing agreed task, in {where_}. \
Preserve all committed and uncommitted work: never reset, clean, stash or check out over it. \
The following is context, not new authorization.\n\n\
Commits so far:\n{commits}\n\nUncommitted changes:\n{changes}\n\n\
The chat so far (latest last):\n{chat_tail}\n</shprd-backup>",
        why = attribute(reason),
        title = handover.title,
        from_label = program_label(from),
        commits = if handover.commits.trim().is_empty() {
            "none"
        } else {
            &handover.commits
        },
        changes = if handover.changes.trim().is_empty() {
            "none"
        } else {
            &handover.changes
        },
        chat_tail = if handover.chat_tail.trim().is_empty() {
            "none"
        } else {
            &handover.chat_tail
        },
    )
}

/// The display name of a program, as Shepherdr's `NAMES` has it.
pub fn program_label(program: &str) -> &str {
    match program {
        "claude" => "Claude Code",
        "codex" => "Codex",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// A fixed epoch time so the timelines read as plain offsets.
    const NOW: u64 = 1_800_000_000_000;

    fn cut(kind: ErrorKind, at_ms: u64, tries: u32) -> Cut {
        Cut {
            id: format!("cut-{tries}"),
            kind,
            message: String::from("the provider said so"),
            at_ms,
            resets_at_ms: None,
            tries,
        }
    }

    fn facts(agent: &str, cut: Option<Cut>) -> AgentFacts {
        AgentFacts {
            task: String::from("T1"),
            attempt: 1,
            agent: agent.to_string(),
            role: Role::Worker,
            working: false,
            cut,
            handover: Handover {
                title: String::from("do the thing"),
                where_: String::from("/wt/T1"),
                branch: Some(String::from("shprd/r1/T1")),
                commits: String::from("abc123 first"),
                changes: String::from(" M src/x.rs"),
                chat_tail: String::from("agent: I am halfway"),
            },
        }
    }

    fn config() -> Config {
        Config {
            waits_ms: PROVIDER_WAITS_MS.to_vec(),
            fallbacks: BTreeMap::from([(
                String::from("worker"),
                vec![String::from("claude"), String::from("codex")],
            )]),
            lines: BTreeMap::new(),
            installed: BTreeSet::from([String::from("claude"), String::from("codex")]),
            signed_in: BTreeSet::from([String::from("claude"), String::from("codex")]),
        }
    }

    #[test]
    fn the_nudge_waits_one_two_four_and_eight_minutes() {
        let mut state = State::default();
        let mut seen = Vec::new();
        for (try_number, wait) in [(1u32, 60_000u64), (2, 120_000), (3, 240_000), (4, 480_000)] {
            let at = NOW;
            let fact = facts("claude", Some(cut(ErrorKind::Overloaded, at, try_number)));
            // Just before the wait is up nothing is sent.
            assert!(
                plan(
                    &state,
                    std::slice::from_ref(&fact),
                    at + wait - 1,
                    &config()
                )
                .is_empty()
            );
            let actions = plan(&state, std::slice::from_ref(&fact), at + wait, &config());
            assert_eq!(actions.len(), 1, "try {try_number}");
            assert_eq!(actions[0].name(), "nudge");
            seen.push(wait);
            // Recording the nudge is the ledger's job; the next pass sends
            // nothing more for the same cut.
            let task = state.tasks.entry(fact.task.clone()).or_default();
            task.agents
                .entry(fact.agent.clone())
                .or_default()
                .nudged
                .insert(fact.cut.as_ref().unwrap().id.clone());
        }
        assert_eq!(seen, PROVIDER_WAITS_MS.to_vec());
    }

    #[test]
    fn a_passing_error_is_over_after_the_fifth_cut_and_backs_up() {
        let cfg = config();
        let fact = facts("claude", Some(cut(ErrorKind::RateLimit, NOW, 5)));
        let actions = plan(&State::default(), &[fact], NOW, &cfg);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            Action::Backup {
                from, to, reason, ..
            } => {
                assert_eq!(from, "claude");
                assert_eq!(to, "codex");
                assert!(reason.contains("still after 5 tries"), "{reason}");
            }
            other => panic!("expected a backup, got {other:?}"),
        }
    }

    #[test]
    fn an_allowance_or_a_logout_backs_up_at_once() {
        for kind in [ErrorKind::Quota, ErrorKind::Auth] {
            let fact = facts("claude", Some(cut(kind, NOW, 1)));
            let actions = plan(&State::default(), &[fact], NOW, &config());
            assert_eq!(actions[0].name(), "backup", "{kind:?}");
        }
    }

    #[test]
    fn another_kind_asks_the_person_without_a_backup() {
        let fact = facts("claude", Some(cut(ErrorKind::Other, NOW, 1)));
        let actions = plan(&State::default(), &[fact], NOW, &config());
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].name(), "give_up");
        if let Action::GiveUp { reason, .. } = &actions[0] {
            assert_eq!(reason, "Provider error");
        }
    }

    #[test]
    fn a_reviewer_is_never_nudged() {
        let mut fact = facts("claude", Some(cut(ErrorKind::Overloaded, NOW, 1)));
        fact.role = Role::Reviewer;
        let actions = plan(
            &State::default(),
            &[fact],
            NOW + PROVIDER_WAITS_MS[0],
            &config(),
        );
        assert!(actions.is_empty());
    }

    #[test]
    fn a_working_agent_is_not_told_to_carry_on() {
        let mut fact = facts("claude", Some(cut(ErrorKind::Overloaded, NOW, 1)));
        fact.working = true;
        let actions = plan(
            &State::default(),
            &[fact],
            NOW + PROVIDER_WAITS_MS[0],
            &config(),
        );
        assert!(actions.is_empty());
    }

    #[test]
    fn a_cut_older_than_thirty_minutes_is_left_to_the_person() {
        let fact = facts("claude", Some(cut(ErrorKind::Overloaded, NOW, 1)));
        let actions = plan(
            &State::default(),
            &[fact],
            NOW + STALE_CUT_MS + 1,
            &config(),
        );
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].name(), "give_up");
    }

    #[test]
    fn never_the_same_program_and_a_replaced_agents_cut_is_left_alone() {
        let cfg = config();
        // The task runs codex: the fallback list's first entry is the same
        // program and is skipped for claude.
        let fact = facts("codex", Some(cut(ErrorKind::Quota, NOW, 1)));
        let actions = plan(&State::default(), std::slice::from_ref(&fact), NOW, &cfg);
        assert!(matches!(&actions[0], Action::Backup { to, .. } if to == "claude"));

        // The old program's cut is not acted on once a handover replaced it.
        let mut state = State::default();
        state.tasks.insert(
            String::from("T1"),
            TaskState {
                agents: BTreeMap::new(),
                backups: vec![BackupDone {
                    from: String::from("codex"),
                    to: String::from("claude"),
                }],
            },
        );
        let actions = plan(&state, &[fact], NOW, &cfg);
        assert!(actions.is_empty());
    }

    fn chain_of_three() -> Config {
        let mut cfg = config();
        cfg.fallbacks = BTreeMap::from([(
            String::from("worker"),
            ["claude", "codex", "pi"].map(String::from).to_vec(),
        )]);
        cfg.installed = ["claude", "codex", "pi"].map(String::from).into();
        cfg.signed_in = cfg.installed.clone();
        cfg
    }

    fn done(from: &str, to: &str) -> BackupDone {
        BackupDone {
            from: from.to_string(),
            to: to.to_string(),
        }
    }

    fn state_with(backups: Vec<BackupDone>) -> State {
        let mut state = State::default();
        state.tasks.insert(
            String::from("T1"),
            TaskState {
                agents: BTreeMap::new(),
                backups,
            },
        );
        state
    }

    #[test]
    fn the_chain_walks_one_handover_per_line_then_stops() {
        let cfg = chain_of_three();
        // Line 1 is cut: line 2 takes over.
        let first = facts("claude", Some(cut(ErrorKind::Quota, NOW, 1)));
        let actions = plan(&State::default(), &[first], NOW, &cfg);
        assert!(matches!(&actions[0], Action::Backup { to, .. } if to == "codex"));

        // Line 2 (which took over) is cut as well: line 3 takes over, never
        // back to line 1.
        let state = state_with(vec![done("claude", "codex")]);
        let second = facts("codex", Some(cut(ErrorKind::Quota, NOW, 1)));
        let actions = plan(&state, &[second], NOW, &cfg);
        assert!(
            matches!(&actions[0], Action::Backup { to, from, .. } if to == "pi" && from == "codex")
        );

        // Line 3 is cut too: every line was tried, so the person decides.
        let state = state_with(vec![done("claude", "codex"), done("codex", "pi")]);
        let third = facts("pi", Some(cut(ErrorKind::Quota, NOW, 1)));
        let actions = plan(&state, &[third], NOW, &cfg);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            Action::GiveUp { reason, .. } => {
                assert!(reason.contains("every line was tried"), "{reason}")
            }
            other => panic!("expected a give-up, got {other:?}"),
        }

        // The agents already replaced stay quiet.
        let old = facts("claude", Some(cut(ErrorKind::Quota, NOW, 1)));
        assert!(plan(&state, &[old], NOW, &cfg).is_empty());
    }

    #[test]
    fn a_line_that_failed_to_start_still_counts_as_tried() {
        let cfg = chain_of_three();
        // claude was handed to codex (which failed to start) and the task is
        // back on claude's pane: codex is not tried again, pi is.
        let state = state_with(vec![done("claude", "codex")]);
        let again = facts("pi", Some(cut(ErrorKind::Auth, NOW, 1)));
        let actions = plan(&state, &[again], NOW, &cfg);
        assert!(matches!(&actions[0], Action::GiveUp { .. }));
        let tried: BTreeSet<&str> = BTreeSet::from(["claude", "codex"]);
        assert_eq!(choose_backup(&cfg, Role::Worker, "pi", &tried), None);
    }

    #[test]
    fn a_cut_task_moves_to_the_next_line_then_wraps_to_an_earlier_one() {
        let mut cfg = config();
        cfg.fallbacks = BTreeMap::from([(
            String::from("worker"),
            ["claude", "codex", "pi", "opencode"]
                .map(String::from)
                .to_vec(),
        )]);
        cfg.installed = ["claude", "codex", "pi", "opencode"]
            .map(String::from)
            .into();
        cfg.signed_in = cfg.installed.clone();
        assert_eq!(
            choose_backup(&cfg, Role::Worker, "codex", &BTreeSet::new()).as_deref(),
            Some("pi"),
            "line 2 moves to line 3"
        );
        assert_eq!(
            choose_backup(&cfg, Role::Worker, "claude", &BTreeSet::new()).as_deref(),
            Some("codex")
        );
        // The next line is signed out: the one after it takes over.
        cfg.signed_in.remove("pi");
        assert_eq!(
            choose_backup(&cfg, Role::Worker, "codex", &BTreeSet::new()).as_deref(),
            Some("opencode")
        );
        // The last line has nothing after it: an earlier line takes over.
        assert_eq!(
            choose_backup(&cfg, Role::Worker, "opencode", &BTreeSet::new()).as_deref(),
            Some("claude")
        );
    }

    #[test]
    fn only_a_signed_in_program_can_take_over() {
        let mut cfg = config();
        cfg.signed_in = BTreeSet::from([String::from("claude")]);
        let fact = facts("claude", Some(cut(ErrorKind::Quota, NOW, 1)));
        let actions = plan(&State::default(), &[fact], NOW, &cfg);
        assert_eq!(actions[0].name(), "give_up");
        if let Action::GiveUp { reason, .. } = &actions[0] {
            assert!(reason.contains("backup signed out"), "{reason}");
        }

        let mut cfg = config();
        cfg.installed = BTreeSet::from([String::from("claude")]);
        let fact = facts("claude", Some(cut(ErrorKind::Quota, NOW, 1)));
        let actions = plan(&State::default(), &[fact], NOW, &cfg);
        if let Action::GiveUp { reason, .. } = &actions[0] {
            assert!(reason.contains("backup not installed"), "{reason}");
        } else {
            panic!("expected a give-up");
        }
    }

    #[test]
    fn the_handover_brief_carries_the_chat_tail_and_the_work() {
        let fact = facts("claude", Some(cut(ErrorKind::Quota, NOW, 1)));
        let text = handover_brief("T1", "claude", "codex", "Out of allowance", &fact.handover);
        assert!(
            text.starts_with(
                "<shprd-backup from=\"claude\" to=\"codex\" why=\"Out of allowance\">"
            )
        );
        assert!(text.contains("abc123 first"));
        assert!(text.contains("M src/x.rs"));
        assert!(text.contains("agent: I am halfway"));
        assert!(text.contains("same worktree (/wt/T1) and branch shprd/r1/T1"));
        assert!(text.ends_with("</shprd-backup>"));
    }
}
