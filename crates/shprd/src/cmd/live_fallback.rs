//! The fallback ladder's live host: what a transcript says, and the two
//! effects the ladder may have.
//!
//! The ladder itself is pure (`shprd_watch::fallback`); this is the binary's
//! side of it, the only place that reads a CLI's record, probes a sign-in,
//! types into a pane or starts a backup. `shprd tick` and `shprd serve` hand it
//! to the service's tick as a [`FallbackSource`].
//!
//! Two facts are deliberately quiet:
//!
//! - a record that cannot be found or read is not a cut, so the ladder leaves
//!   that agent alone (`records::locate`'s rule);
//! - the sign-in check is cached for fifteen minutes, as Shepherdr's
//!   `cli-check.json` is (`takeover.py:14-96`), because a probe is a process
//!   launch and a pass runs every fifteen seconds.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use shprd_agents::detect::{self, SignIn};
use shprd_core::event::Actor;
use shprd_core::log::{Clock, SystemClock};
use shprd_core::state::{RunState, Task, TaskStatus};
use shprd_watch::fallback::{AgentFacts, Handover, Role};
use shprd_watch::records;

use crate::cmd::handoff::{HandoffRequest, handoff};
use crate::cmd::{review, spawn};
use crate::ctx::Ctx;

/// How long a sign-in check is reused, as Shepherdr's `CHECK_EVERY`
/// (`takeover.py:23`).
const CHECK_EVERY_MS: u64 = 15 * 60_000;

/// The programs this machine can run, and which are signed in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Check {
    /// When the check ran, in epoch milliseconds.
    at_ms: u64,
    /// Programs with a binary on `PATH`.
    installed: BTreeSet<String>,
    /// Programs whose own sign-in probe answered `SignedIn`.
    signed_in: BTreeSet<String>,
}

/// The live fallback host for one repository.
pub struct LiveFallback {
    /// The repository whose run is advanced.
    repo: PathBuf,
    /// The home shprd's own files live under.
    home: PathBuf,
    /// The run's id.
    run: String,
    /// The repository's directory name, which names its workspace.
    repo_name: String,
    /// The multiplexer the task's tabs live in.
    mux: Arc<dyn shprd_mux::Mux>,
    /// The last sign-in check, kept across passes.
    check: Mutex<Option<Check>>,
}

impl LiveFallback {
    /// A host over one repository, its home and its run.
    pub fn new(repo: &Path, home: &Path, run: &str, repo_name: &str) -> Self {
        Self {
            repo: repo.to_path_buf(),
            home: home.to_path_buf(),
            run: run.to_string(),
            repo_name: repo_name.to_string(),
            mux: Arc::new(spawn::real_mux()),
            check: Mutex::new(None),
        }
    }

    /// The directory briefs and tab records are written under.
    fn briefs(&self) -> PathBuf {
        self.home.join(".shprd").join("briefs")
    }

    /// The pane a task's worker or reviewer runs in, when its record names one
    /// and the host still knows the tab.
    fn pane(
        &self,
        tabs: &[shprd_mux::Tab],
        task: &str,
        attempt: u32,
        review: bool,
    ) -> Option<shprd_mux::PaneId> {
        let record = if review {
            self.briefs()
                .join(&self.run)
                .join(format!("{task}-a{attempt}{}", spawn::REVIEW_TAB_SUFFIX))
        } else {
            spawn::tab_path(&self.briefs(), &self.run, task, attempt)
        };
        let wanted = std::fs::read_to_string(record).ok()?;
        let wanted = wanted.trim();
        tabs.iter()
            .find(|tab| tab.id.as_str() == wanted)
            .map(|tab| tab.root_pane.clone())
    }

    /// The workspace every task's tab lives in, when the host has one.
    fn workspace(&self) -> Option<shprd_mux::Workspace> {
        self.mux
            .find_workspace(&spawn::workspace_name(&self.repo_name))
            .ok()
            .flatten()
    }

    /// What a backup needs to know about the work so far.
    fn handover(&self, task: &Task, worktree: &Path, branch: &str) -> Handover {
        let git = shprd_git::git::Git::new(worktree);
        Handover {
            title: task.title.clone(),
            where_: worktree.display().to_string(),
            branch: Some(branch.to_string()),
            commits: git
                .run(&["log", "--oneline", "-20"])
                .unwrap_or_default()
                .trim()
                .to_string(),
            changes: git
                .run(&["status", "--short"])
                .unwrap_or_default()
                .trim()
                .to_string(),
            chat_tail: records::chat_tail(&task_agent(task), &self.home, worktree)
                .unwrap_or_default(),
        }
    }

    /// This machine's installed and signed-in programs, checked at most every
    /// fifteen minutes.
    fn check(&self) -> Check {
        let mut guard = match self.check.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(check) = guard.as_ref()
            && SystemClock.now_ms().saturating_sub(check.at_ms) < CHECK_EVERY_MS
        {
            return check.clone();
        }
        let specs = crate::cmd::roster::resolve_data_dir()
            .ok()
            .and_then(|data| shprd_agents::spec::load_dir(&data.join("harness")).ok())
            .unwrap_or_default();
        let path_env = std::env::var("PATH").unwrap_or_default();
        let env: Vec<(String, Option<String>)> = std::env::vars()
            .map(|(key, value)| (key, Some(value)))
            .collect();
        let detections = detect::detect_all(&specs, &path_env, &env, &BTreeMap::new());
        let mut check = Check {
            at_ms: SystemClock.now_ms(),
            ..Check::default()
        };
        for found in detections {
            if found.binary.is_none() {
                continue;
            }
            check.installed.insert(found.id.clone());
            if found.signin == SignIn::SignedIn {
                check.signed_in.insert(found.id);
            }
        }
        *guard = Some(check.clone());
        check
    }
}

/// The program a task's latest attempt runs, or its first worker.
fn task_agent(task: &Task) -> String {
    task.latest_attempt()
        .map(|attempt| attempt.agent.clone())
        .unwrap_or_else(|| String::from("claude"))
}

impl shprd_server::FallbackSource for LiveFallback {
    fn agents(&self, state: &RunState) -> Vec<AgentFacts> {
        let Some(workspace) = self.workspace() else {
            return Vec::new();
        };
        let Ok(tabs) = self.mux.list_tabs(&workspace.id) else {
            return Vec::new();
        };
        let mut facts = Vec::new();
        for task in state.tasks.values() {
            if task.status != TaskStatus::Working {
                continue;
            }
            let Some(attempt) = task.latest_attempt() else {
                continue;
            };
            if let Some(pane) = self.pane(&tabs, &task.id, attempt.n, false) {
                let worktree = PathBuf::from(&attempt.worktree);
                let cut = records::locate::cut(&attempt.agent, &self.home, &worktree);
                // The handover reads git and the whole transcript, so it is
                // only built for the one case that could need it.
                let handover = if cut.is_some() {
                    self.handover(task, &worktree, &attempt.branch)
                } else {
                    Handover::default()
                };
                facts.push(AgentFacts {
                    task: task.id.clone(),
                    attempt: attempt.n,
                    agent: attempt.agent.clone(),
                    role: Role::Worker,
                    working: matches!(
                        self.mux.agent_state(&pane),
                        Ok(shprd_mux::AgentState::Working)
                    ),
                    cut,
                    handover,
                });
            }
            // A reviewer's own cut is never retried, but it is still a fact
            // the planner gets: that is what makes "never a reviewer" a rule
            // of the ladder rather than a hope about what this host reports.
            let Some(review) = attempt.reviews.last() else {
                continue;
            };
            let Some(pane) = self.pane(&tabs, &task.id, attempt.n, true) else {
                continue;
            };
            let packet = review::packet_dir(
                &self.home.join(".shprd").join("reviews"),
                &self.run,
                &task.id,
                attempt.n,
            );
            facts.push(AgentFacts {
                task: task.id.clone(),
                attempt: attempt.n,
                agent: review.reviewer.clone(),
                role: Role::Reviewer,
                working: matches!(
                    self.mux.agent_state(&pane),
                    Ok(shprd_mux::AgentState::Working)
                ),
                cut: records::locate::cut(&review.reviewer, &self.home, &packet),
                handover: Handover::default(),
            });
        }
        facts
    }

    fn installed(&self) -> BTreeSet<String> {
        self.check().installed
    }

    fn signed_in(&self) -> BTreeSet<String> {
        self.check().signed_in
    }

    fn nudge(&self, task: &str, agent: &str, text: &str) -> Result<String, String> {
        let Some(workspace) = self.workspace() else {
            return Err(format!("no workspace for {}", self.repo.display()));
        };
        let tabs = self
            .mux
            .list_tabs(&workspace.id)
            .map_err(|err| err.to_string())?;
        let record = spawn::latest_tab_record(&self.briefs(), &self.run, task)
            .ok_or_else(|| format!("no tab record for {task}"))?;
        let wanted = std::fs::read_to_string(record).map_err(|err| err.to_string())?;
        let wanted = wanted.trim();
        let tab = tabs
            .iter()
            .find(|tab| tab.id.as_str() == wanted)
            .ok_or_else(|| format!("{task}'s tab is gone"))?;
        self.mux
            .prompt(&tab.root_pane, text)
            .map_err(|err| err.to_string())?;
        Ok(format!("told {agent} to carry on"))
    }

    fn backup(
        &self,
        task: &str,
        attempt: u32,
        from: &str,
        to: &str,
        brief: &str,
    ) -> Result<String, String> {
        let ctx = Ctx::new(self.repo.clone(), Actor::Engine, false, false);
        let env = spawn::build_env(&ctx, Some(&self.home)).map_err(|err| err.to_string())?;
        let request = HandoffRequest {
            task: task.to_string(),
            reason: format!("the provider cut {from}'s turn off"),
            to: Some(to.to_string()),
            role: Some("backup".to_string()),
            same_worker: false,
            opening: Some(brief.to_string()),
        };
        let report = handoff(self.mux.as_ref(), &env, &request).map_err(|err| err.to_string())?;
        if report.attempt != attempt {
            return Err(format!(
                "{to} took over attempt {}, not {attempt}",
                report.attempt
            ));
        }
        Ok(format!(
            "{to} took over {task} in {}",
            report.brief.display()
        ))
    }
}
