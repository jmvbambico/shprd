//! The rules every verb that starts an agent CLI shares.
//!
//! Only two verbs launch an agent: `up` (the orchestrator) and `spawn` (a
//! task). They build their argv from the same [`LaunchSpec`], so the decisions
//! that are not verb-specific live here and are made once.

use std::time::{Duration, Instant};

use shprd_mux::{AgentState, Mux, PaneId};
use shprd_roster::spec::{BlockingPrompt, LaunchSpec};

/// The model-argument template to render, empty when no model is chosen.
///
/// A spec's `model_args` use the `{model}` placeholder, so rendering them for
/// an agent line that names no model fails on a placeholder nobody filled.
/// An orchestrator configured as plain `sh` is the normal case: it has no
/// `/model` segment and its CLI needs no model flag at all.
pub fn model_args<'a>(launch: &'a LaunchSpec, model: Option<&str>) -> &'a [String] {
    if model.is_some() {
        &launch.model_args
    } else {
        &[]
    }
}

/// How many lines of a pane the blocking-prompt check reads.
const BLOCKING_PROMPT_LINES: usize = 40;

/// How often the wait re-asks the host whether the agent is up yet.
const READY_POLL: Duration = Duration::from_millis(250);

/// How long a launched CLI gets to be detected as an agent before shprd gives
/// up on delivering its brief.
const DEFAULT_READY_WAIT: Duration = Duration::from_secs(60);

/// The environment variable that overrides [`DEFAULT_READY_WAIT`], in
/// milliseconds. Tests set it to `0` (deliver nothing, warn) or a small value.
const READY_WAIT_ENV: &str = "SHPRD_AGENT_READY_TIMEOUT_MS";

/// The one-line message an agent gets when its brief already arrived as an
/// argument (a system prompt).
///
/// Such a CLI starts idle, waiting for a first turn, so the brief alone would
/// leave it sitting there. One line, because it travels through the same
/// prompt path a typed brief does.
pub const KICKOFF: &str = "Start now: follow the brief in your system prompt.";

/// How handing a brief to a freshly launched agent ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    /// The agent took the brief as its first turn.
    Delivered,
    /// The brief never arrived; the string says why, for the caller's warning.
    NotDelivered(String),
    /// The CLI is showing a prompt of its own and waits for a human to answer
    /// it; the string names that prompt. Nothing was typed into the pane.
    Blocked {
        /// The [`BlockingPrompt::name`] the screen matched.
        prompt: String,
    },
}

impl Delivery {
    /// The reason the brief did not arrive, or `None` when it did.
    ///
    /// A blocked agent gets a sentence, not just the prompt's name: the human
    /// reads it as a warning, and it says what to do about it.
    pub fn reason(&self) -> Option<String> {
        match self {
            Self::Delivered => None,
            Self::NotDelivered(reason) => Some(reason.clone()),
            Self::Blocked { prompt } => Some(format!(
                "waiting on its \"{prompt}\" prompt: answer it in that tab, then resend the brief"
            )),
        }
    }
}

/// How long to wait for an agent to be detected, from the environment.
pub fn ready_wait() -> Duration {
    parse_wait(std::env::var(READY_WAIT_ENV).ok().as_deref())
}

/// [`ready_wait`] without the environment, so a test can prove the parse.
fn parse_wait(raw: Option<&str>) -> Duration {
    raw.and_then(|raw| raw.trim().parse::<u64>().ok())
        .map_or(DEFAULT_READY_WAIT, Duration::from_millis)
}

/// Types `text` into `pane` as the agent's first turn, once the host has
/// detected an agent there.
///
/// A host only accepts `prompt` for a pane it knows to be running an agent, and
/// detection lags the launch: a CLI that has just been started is not an agent
/// yet. So the wait polls [`Mux::agent_state`] until it is no longer
/// [`AgentState::Unknown`]. Nothing here is fatal — a brief that never lands is
/// reported to the human, not turned into a failed launch, because the process
/// itself is up either way.
pub fn deliver_brief(
    mux: &dyn Mux,
    pane: &PaneId,
    text: &str,
    patterns: &[BlockingPrompt],
    wait: Duration,
) -> Delivery {
    match await_agent(mux, pane, patterns, wait) {
        Ok(None) => {}
        Ok(Some(prompt)) => return Delivery::Blocked { prompt },
        Err(reason) => return Delivery::NotDelivered(reason),
    }
    match mux.prompt(pane, text) {
        Ok(()) => Delivery::Delivered,
        Err(err) => Delivery::NotDelivered(format!("the host refused the prompt: {err}")),
    }
}

/// Polls until `pane` reports a detected agent, or `wait` runs out.
///
/// `Ok(None)` means the agent is up and ready for its brief; `Ok(Some(name))`
/// means the screen is showing a prompt that agent waits on a human to answer,
/// named by the matching [`BlockingPrompt`].
fn await_agent(
    mux: &dyn Mux,
    pane: &PaneId,
    patterns: &[BlockingPrompt],
    wait: Duration,
) -> Result<Option<String>, String> {
    let start = Instant::now();
    loop {
        // The CLI may ask its own user something before it ever becomes an
        // agent, and wait there silently: report that instead of timing out.
        if let Ok(prompt) = blocking_prompt(mux, pane, patterns) {
            return Ok(Some(prompt));
        }
        // A host that cannot report state answers `Unknown` forever; that is
        // "no signal" rather than a failure, so the wait alone decides.
        if matches!(mux.agent_state(pane), Ok(state) if state != AgentState::Unknown) {
            return Ok(None);
        }
        let waited = start.elapsed();
        if waited >= wait {
            return Err(format!(
                "the agent was not detected within {} ms",
                wait.as_millis()
            ));
        }
        std::thread::sleep(READY_POLL.min(wait - waited));
    }
}

/// The name of the first blocking prompt the pane's screen is showing, if any.
///
/// The watchdog asks the same question of the orchestrator's pane before it
/// types a notice: a CLI waiting on its own prompt reads the next line as the
/// answer to that prompt.
///
/// The match is case-insensitive, like [`shprd_roster::spec::HarnessSpec::dry_patterns`],
/// and reads only the last [`BLOCKING_PROMPT_LINES`] lines: a prompt still
/// waiting is on screen, and the scrollback above it may mention one long gone.
pub(crate) fn blocking_prompt(
    mux: &dyn Mux,
    pane: &PaneId,
    patterns: &[BlockingPrompt],
) -> Result<String, ()> {
    if patterns.is_empty() {
        return Err(());
    }
    let Ok(screen) = mux.read(pane, BLOCKING_PROMPT_LINES) else {
        return Err(());
    };
    let screen = screen.to_lowercase();
    patterns
        .iter()
        .find(|pattern| screen.contains(&pattern.pattern.to_lowercase()))
        .map(|pattern| pattern.name.clone())
        .ok_or(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use shprd_roster::spec::LaunchSpec;

    fn launch() -> LaunchSpec {
        LaunchSpec {
            interactive: vec!["claude".to_string()],
            model_args: vec!["--model".to_string(), "{model}".to_string()],
            effort_args: None,
            auto_approve: None,
            reviewer_args: None,
            system_prompt_args: None,
            resume_args: None,
            verified: true,
        }
    }

    #[test]
    fn a_model_renders_its_flags() {
        let launch = launch();
        let args = model_args(&launch, Some("claude-opus-5-5"));
        assert_eq!(args, ["--model".to_string(), "{model}".to_string()]);
    }

    #[test]
    fn no_model_renders_nothing() {
        let launch = launch();
        assert!(model_args(&launch, None).is_empty());
    }

    #[test]
    fn the_wait_comes_from_the_environment_with_a_default() {
        assert_eq!(parse_wait(Some("1500")), Duration::from_millis(1500));
        assert_eq!(parse_wait(Some(" 0 ")), Duration::ZERO);
        assert_eq!(parse_wait(None), DEFAULT_READY_WAIT);
        assert_eq!(parse_wait(Some("soon")), DEFAULT_READY_WAIT);
    }

    /// A one-tab workspace, ready to have a brief typed into its root pane.
    fn fixture() -> (shprd_mux::fake::FakeMux, PaneId) {
        use shprd_mux::Mux as _;
        let mux = shprd_mux::fake::FakeMux::default();
        let workspace =
            match mux.create_workspace("shprd · repo", std::path::Path::new("/tmp/repo")) {
                Ok(workspace) => workspace,
                Err(error) => panic!("{error}"),
            };
        let tab = match mux.create_tab(&workspace.id, "● sh·w1", std::path::Path::new("/tmp")) {
            Ok(tab) => tab,
            Err(error) => panic!("{error}"),
        };
        (mux, tab.root_pane)
    }

    fn prompts(mux: &shprd_mux::fake::FakeMux) -> Vec<String> {
        use shprd_mux::fake::Call;
        mux.calls()
            .into_iter()
            .filter_map(|call| match call {
                Call::Prompt(_, text) => Some(text),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_brief_waits_for_a_late_agent_then_lands() {
        let (mux, pane) = fixture();
        // Not an agent for two polls, an idle agent from the third on.
        mux.set_state_after(&pane, 2, AgentState::Idle);
        let delivery = deliver_brief(&mux, &pane, "the brief", &[], Duration::from_secs(5));
        assert_eq!(delivery, Delivery::Delivered);
        assert_eq!(prompts(&mux), ["the brief".to_string()]);
    }

    #[test]
    fn an_agent_that_never_shows_up_is_a_warning_not_a_failure() {
        let (mux, pane) = fixture();
        // A zero wait: one poll, no sleep, and the agent is still undetected.
        let delivery = deliver_brief(&mux, &pane, "the brief", &[], Duration::ZERO);
        assert_eq!(
            delivery.reason().as_deref(),
            Some("the agent was not detected within 0 ms")
        );
        assert!(
            matches!(delivery, Delivery::NotDelivered(_)),
            "{delivery:?}"
        );
        assert!(prompts(&mux).is_empty(), "nothing may be typed in");
    }

    /// The real Claude Code first-run text, as it appears on screen.
    fn trust_patterns() -> Vec<BlockingPrompt> {
        vec![BlockingPrompt {
            name: "folder trust".to_string(),
            pattern: "Is this a project you created or one you trust".to_string(),
        }]
    }

    #[test]
    fn a_folder_trust_prompt_is_reported_at_once() {
        let (mux, pane) = fixture();
        mux.set_screen(
            &pane,
            "Is this a project you created or one you trust?\n  > 1. Yes, I trust\n    2. No",
        );
        let patterns = trust_patterns();
        let start = std::time::Instant::now();
        // The screen answers before the first poll finds no agent, and long
        // before the wait would run out.
        let delivery = deliver_brief(&mux, &pane, "the brief", &patterns, Duration::from_secs(30));
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "the wait must not run out"
        );
        assert_eq!(
            delivery,
            Delivery::Blocked {
                prompt: "folder trust".to_string()
            }
        );
        assert_eq!(
            delivery.reason().as_deref(),
            Some(
                "waiting on its \"folder trust\" prompt: answer it in that tab, then resend the brief"
            )
        );
        assert!(
            prompts(&mux).is_empty(),
            "a prompt waiting on a human is never answered, and the brief is never typed over it"
        );
    }

    #[test]
    fn a_plain_screen_leaves_the_delivery_alone() {
        let (mux, pane) = fixture();
        mux.set_screen(&pane, "claude ready\n> ");
        mux.set_state(&pane, AgentState::Idle);
        let patterns = trust_patterns();
        let delivery = deliver_brief(&mux, &pane, "the brief", &patterns, Duration::from_secs(5));
        assert_eq!(delivery, Delivery::Delivered);
        assert_eq!(prompts(&mux), ["the brief".to_string()]);
    }
}
