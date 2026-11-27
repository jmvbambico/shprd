//! What the person's task verbs need from the binary: a spawn and a close.
//!
//! Two of the actions on `POST /api/action` are not an event the log can hold:
//! starting an attempt (which writes a brief, makes a worktree and opens a tab)
//! and stopping an agent (which closes the space it runs in). Both belong to the
//! binary, exactly as the tick's own steps do, so the service calls the same
//! functions `shprd agent spawn` and a landing's close call, passed in here the
//! way [`crate::service::AfterWrite`] is.
//!
//! A service prepared without one — every test that drives the router, and any
//! caller that only wants to read the log — refuses those actions rather than
//! pretending it did something: an agent that was never started is a lie a
//! person would find later, in a tab that does not exist.
//!
//! Nothing here reaches a host or a model by itself: a test wires closures that
//! record what they were asked for.

use std::path::Path;

use crate::tick::{Starter, WorkerCloser};

/// Types a task's latest brief into its agent again; answers one line.
pub type Resender = std::sync::Arc<dyn Fn(&Path, &str) -> Result<String, String> + Send + Sync>;

/// The binary's spawn path, and its way of closing a task's space.
#[derive(Clone)]
pub struct Engine {
    /// Starts an attempt on a task; returns one line about what it did.
    start: Starter,
    /// Closes the space a task's agent runs in, when the caller has a host.
    ///
    /// It is the same closure a landing uses to close a task's space, so a
    /// person's stop and the engine's own close cannot disagree about what a
    /// task's space is.
    stop: Option<WorkerCloser>,
    /// Delivers a task's brief to its agent again, when the caller has a host.
    resend: Option<Resender>,
    /// Closes a task's reviewer and starts a new review, when the caller has
    /// a host.
    review_again: Option<Resender>,
    /// Runs one of the run's own verbs as the person, when the caller has a
    /// binary to run it with.
    ///
    /// `sync` and `run close` are the run's rather than a task's, so they are not
    /// an event this service can append: the binary's own verb asks the rules,
    /// writes the event and does the filesystem work. It is one closure for both
    /// so a service and a caller can never wire one and forget the other.
    run_verb: Option<RunVerb>,
}

/// Runs one of the run's verbs as the person (`run close`, `sync`), by naming it;
/// answers what it said, or why it refused.
pub type RunVerb = std::sync::Arc<dyn Fn(&Path, &str) -> Result<(), String> + Send + Sync>;

impl Engine {
    /// An engine whose only verb is a spawn.
    ///
    /// It is what a caller without a host passes: a start works and a stop is
    /// refused, because there is no tab to close.
    pub fn new(start: Starter) -> Self {
        Self {
            start,
            stop: None,
            resend: None,
            review_again: None,
            run_verb: None,
        }
    }

    /// The same engine, also able to run the run's own verbs.
    pub fn with_run_verb(mut self, run_verb: RunVerb) -> Self {
        self.run_verb = Some(run_verb);
        self
    }

    /// Whether this engine can run the run's own verbs (`run close`, `sync`).
    pub fn can_run_verb(&self) -> bool {
        self.run_verb.is_some()
    }

    /// Runs `verb` (`run close`, `sync`) as the person in `repo`.
    ///
    /// A failure is the verb's own words, which is what a person reads on the
    /// card that asked for it.
    pub fn run_verb(&self, repo: &Path, verb: &str) -> Result<(), String> {
        let run = self.run_verb.as_ref().ok_or_else(|| {
            format!("this service was started without a binary, so it cannot run `shprd {verb}`")
        })?;
        run(repo, verb)
    }

    /// The same engine, also able to close a task's space.
    pub fn with_stop(mut self, stop: WorkerCloser) -> Self {
        self.stop = Some(stop);
        self
    }

    /// The same engine, also able to deliver a brief again.
    pub fn with_resend(mut self, resend: Resender) -> Self {
        self.resend = Some(resend);
        self
    }

    /// The same engine, also able to start a review again.
    pub fn with_review_again(mut self, review_again: Resender) -> Self {
        self.review_again = Some(review_again);
        self
    }

    /// Whether this engine can start a review again.
    pub fn can_review_again(&self) -> bool {
        self.review_again.is_some()
    }

    /// Closes `task`'s reviewer and starts a new review of the same attempt, as
    /// `shprd agent review --again` does.
    pub fn review_again(&self, repo: &Path, task: &str) -> Result<String, String> {
        let again = self.review_again.as_ref().ok_or_else(|| {
            "this service was started without a host, so it cannot start a review".to_string()
        })?;
        again(repo, task)
    }

    /// Delivers `task`'s latest brief to its agent again.
    ///
    /// A failure is the host's or the brief's own words: no agent to type into,
    /// no brief on disk.
    pub fn resend(&self, repo: &Path, task: &str) -> Result<String, String> {
        let resend = self.resend.as_ref().ok_or_else(|| {
            "this service was started without a host, so it cannot deliver a brief".to_string()
        })?;
        resend(repo, task)
    }

    /// Starts an attempt at `task` in `repo`, as `shprd agent spawn --task` does.
    ///
    /// The failure is the spawn's own words: it is what a person reads when a
    /// start is refused, so it is passed through rather than wrapped.
    pub fn start(&self, repo: &Path, task: &str) -> Result<String, String> {
        (self.start)(repo, task)
    }

    /// Stops whatever is running on `task`, by closing its space.
    ///
    /// A failure means the host refused the close, and is the host's words.
    pub fn stop(&self, repo: &Path, task: &str) -> Result<(), String> {
        let stop = self.stop.as_ref().ok_or_else(|| {
            "this service was started without a host, so it cannot close a task's space".to_string()
        })?;
        stop(repo, task)
    }
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("stop", &self.stop.is_some())
            .field("resend", &self.resend.is_some())
            .field("review_again", &self.review_again.is_some())
            .field("run_verb", &self.run_verb.is_some())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::sync::{Arc, Mutex};

    use super::*;

    #[test]
    fn an_engine_without_a_host_refuses_a_stop_and_says_why() {
        let started: Arc<Mutex<Vec<String>>> = Arc::default();
        let seen = Arc::clone(&started);
        let engine = Engine::new(Arc::new(move |_repo: &Path, task: &str| {
            seen.lock().expect("the lock").push(task.to_string());
            Ok(format!("started {task}"))
        }));
        assert_eq!(
            engine.start(Path::new("/repo"), "T1").expect("a start"),
            "started T1"
        );
        assert_eq!(
            started.lock().expect("the lock").as_slice(),
            ["T1".to_string()]
        );
        let refused = engine
            .stop(Path::new("/repo"), "T1")
            .expect_err("a refusal");
        assert!(refused.contains("without a host"), "{refused}");
    }

    #[test]
    fn a_wired_stop_is_called_and_its_failure_passes_through() {
        let engine = Engine::new(Arc::new(|_repo: &Path, task: &str| Ok(task.to_string())))
            .with_stop(Arc::new(|_repo: &Path, _task: &str| {
                Err("no space labelled T1".to_string())
            }));
        assert_eq!(engine.start(Path::new("/repo"), "T1"), Ok("T1".to_string()));
        assert_eq!(
            engine.stop(Path::new("/repo"), "T1"),
            Err("no space labelled T1".to_string())
        );
    }
}
