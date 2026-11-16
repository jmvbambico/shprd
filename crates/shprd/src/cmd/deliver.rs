//! Handing a brief to an agent CLI shprd has just started.
//!
//! `spawn` (a task's agent), `review` (a reviewer) and `watch` (a notice typed
//! into an orchestrator) all start a process and then have to get text into it
//! once the host has detected an agent there. The decisions that are not
//! verb-specific live here and are made once: the argv-flag rules, how long to
//! wait, and whether a prompt of the CLI's own is in the way.
//!
use std::time::{Duration, Instant};

use shprd_agents::spec::{BlockingPrompt, LaunchSpec};
use shprd_mux::{AgentState, Host, PaneId};

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

/// How many lines of a pane the readiness check reads.
///
/// Every program draws its input box and its status footer at the bottom of the
/// screen, so the line that says "I can take a turn" is in the last few rows —
/// but how far up depends on the program and the pane's height, and a line of
/// scrollback above it may still belong to the start-up banner.
const READY_LINES: usize = 40;

/// How often the wait re-asks the host whether the agent is up yet.
const READY_POLL: Duration = Duration::from_millis(250);

/// How many polls a program that names no ready line gets before a pane with
/// anything on it counts as ready.
///
/// A TUI that has painted is a TUI that can take a turn, so the wait only has to
/// outlast the paint itself. That is a few [`READY_POLL`]s at most.
const READY_SETTLE_POLLS: usize = 6;

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
    /// The agent took the brief as its first turn, and this is what said it was
    /// ready for it.
    Delivered {
        /// Where the readiness came from.
        ready: ReadyBy,
    },
    /// The brief never arrived; the string says why, for the caller's warning.
    NotDelivered(String),
    /// The CLI is showing a prompt of its own and waits for a human to answer
    /// it; the string names that prompt. Nothing was typed into the pane.
    Blocked {
        /// The [`BlockingPrompt::name`] the screen matched.
        prompt: String,
    },
}

/// What said a program was ready for its brief.
///
/// A launch records this beside the attempt, because the two are different
/// facts about the run: a host that reported the agent is a host shprd can
/// follow, and a program read off its own ready line is one whose status the
/// host cannot give (M10 gate, bug 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadyBy {
    /// The host reported the program's own agent state.
    HostStatus,
    /// The host cannot report this program, so the line its harness names was
    /// read off the bottom of its pane.
    ReadyLine,
    /// The host cannot report this program and its harness names no line, so a
    /// pane that had painted anything at all was taken as ready.
    ScreenPainted,
}

impl ReadyBy {
    /// The word the launch record carries.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HostStatus => "host-status",
            Self::ReadyLine => "ready-line",
            Self::ScreenPainted => "screen-painted",
        }
    }
}

impl Delivery {
    /// The reason the brief did not arrive, or `None` when it did.
    ///
    /// A blocked agent gets a sentence, not just the prompt's name: the human
    /// reads it as a warning, and it says what to do about it.
    pub fn reason(&self) -> Option<String> {
        match self {
            Self::Delivered { .. } => None,
            Self::NotDelivered(reason) => Some(reason.clone()),
            Self::Blocked { prompt } => Some(format!(
                "waiting on its \"{prompt}\" prompt: answer it in that tab, then resend the brief"
            )),
        }
    }

    /// How the agent was ready, when the brief arrived.
    pub fn ready(&self) -> Option<ReadyBy> {
        match self {
            Self::Delivered { ready } => Some(*ready),
            Self::NotDelivered(_) | Self::Blocked { .. } => None,
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

/// What a freshly launched program's pane says is ready for the brief.
///
/// The harness file's facts, as the delivery needs them: the lines the program
/// prints when its TUI can take a turn, the prompts it may be waiting on a
/// human to answer, and whether the host can report this program's status at
/// all.
#[derive(Debug, Clone, Copy, Default)]
pub struct Readiness<'a> {
    /// The lines the program's TUI prints once it accepts a turn
    /// ([`shprd_agents::harness::Harness::ready_lines`]).
    pub lines: &'a [String],
    /// The prompts the program raises and waits on a human to answer.
    pub prompts: &'a [BlockingPrompt],
    /// Whether this program has any status source that is not a plugin or hook
    /// the host carries, from its `[status] sources`
    /// ([`shprd_agents::spec::StatusSpec::needs_the_host`]).
    ///
    /// `false` is the OpenCode/Kilo case: every real source the harness names is
    /// one a plugin or hook provides, so a host without the report method has no
    /// way to hear from the program at all.
    pub own_status: bool,
}

/// Types `text` into `pane` as the agent's first turn, submitted, once the
/// program is ready for it.
///
/// Nothing a `Host` reports tells shprd that a terminal UI is *reading input*:
/// `agent_state` only says a process was recognised as an agent, and both herdr
/// and a herdr-core say that while the TUI is still painting. A brief typed then
/// is pasted into a program that has not started listening, and it is lost with
/// no error — which is exactly what the M10 gate saw with opencode.
///
/// So which evidence is asked for first depends on the program, and the first
/// that answers wins ([`await_ready`] carries the argument):
///
/// 1. **a program that reports itself** (`pi`'s OSC 7501): the host's status is
///    the program's own word, so it is asked for first
///    ([`ReadyBy::HostStatus`]);
/// 2. **a program only a plugin reports** (`opencode`, `kilo`, `codex`,
///    `claude`'s hooks): the host's status is the *host's* reading of a process
///    it merely recognised, so the program's own ready line is asked for first
///    ([`ReadyBy::ReadyLine`]) — the only evidence its TUI has finished starting;
/// 3. a program that names no ready line is ready when the host reports its
///    agent state, or, when the host cannot, once its pane has painted anything
///    ([`ReadyBy::ScreenPainted`]);
/// 4. a named ready line that never appeared is still answered by the host's
///    own word once the wait runs out on a painted pane: a prompt whose wording
///    changed in a newer build must not cost the run its agent.
///
/// Nothing here is fatal — a brief that never lands is reported to the human,
/// not turned into a failed launch, because the process itself is up either
/// way.
pub fn deliver_brief(
    mux: &dyn Host,
    pane: &PaneId,
    text: &str,
    readiness: &Readiness<'_>,
    wait: Duration,
) -> Delivery {
    let ready = match await_ready(mux, pane, readiness, wait) {
        Ok(Ready::Ready(ready)) => ready,
        Ok(Ready::Blocked(prompt)) => return Delivery::Blocked { prompt },
        Err(reason) => return Delivery::NotDelivered(reason),
    };
    match mux.write(pane, text, true) {
        Ok(()) => Delivery::Delivered { ready },
        Err(err) => Delivery::NotDelivered(format!("the host refused the prompt: {err}")),
    }
}

/// What the wait found: ready (and how), or a prompt waiting on a human.
enum Ready {
    /// The program can take its brief.
    Ready(ReadyBy),
    /// The program is waiting on its own prompt, named here.
    Blocked(String),
}

/// Polls until the program is ready for its brief, or `wait` runs out.
///
/// Which evidence is asked for first depends on whether the program reports
/// itself, because the two are not equally trustworthy:
///
/// - **a program that reports for itself** (`pi`'s OSC 7501) has a host status
///   that *is* the program's own word, so it is asked for first and is enough.
///   That keeps a program whose file names a line fast when the line happens to
///   be missing from a resumed session's footer;
/// - **a program that only a plugin reports** (`opencode`, `kilo`, `codex`,
///   `claude`'s hooks) has a host status that is the *host's* word, and both
///   herdr and a herdr-core call a program an idle agent the moment they see its
///   process — before its TUI reads input. So its own ready line is asked for
///   first, and the host's word is the last resort once the wait has run out on
///   a pane that has plainly painted something.
fn await_ready(
    mux: &dyn Host,
    pane: &PaneId,
    readiness: &Readiness<'_>,
    wait: Duration,
) -> Result<Ready, String> {
    // Whether the host can report *this program* at all. `own_status` is the
    // program's own reporter (OSC 7501); a program whose sources are all
    // plugins additionally needs the host to carry a report
    // (`Capabilities::agent_reports`), because that is the method its plugin
    // calls. A core without one is where the plugin silently no-ops, and
    // waiting for a status nobody sends is the bug this whole path exists for.
    let caps = mux.capabilities();
    let host_reports = caps.agent_state && (readiness.own_status || caps.agent_reports);
    // The program's own word is the status only when it reports itself; a
    // plugin's status is the host's reading of a program it merely recognises.
    let own_word = host_reports && readiness.own_status;
    let named = !readiness.lines.is_empty();
    let start = Instant::now();
    let mut polls: usize = 0;
    loop {
        // The CLI may ask its own user something before it ever becomes an
        // agent, and wait there silently: report that instead of timing out.
        // This is checked first on every poll, because a blocking prompt is the
        // one screen shprd must never type over.
        if let Some(prompt) = blocking_prompt(mux, pane, readiness.prompts) {
            return Ok(Ready::Blocked(prompt));
        }
        if own_word {
            if host_is_up(mux, pane) {
                return Ok(Ready::Ready(ReadyBy::HostStatus));
            }
            // A program that reports itself and does not, on a host that can
            // hear it, is still given the chance its screen offers: a build that
            // stops reporting must not lose its brief.
            if let Some(ready) = screen_ready(mux, pane, readiness.lines, polls) {
                return Ok(Ready::Ready(ready));
            }
        } else if named {
            // A plugin-only program: its own line is the only evidence that its
            // TUI has finished starting, so it is asked for on every poll.
            if let Some(ready) = screen_ready(mux, pane, readiness.lines, polls) {
                return Ok(Ready::Ready(ready));
            }
        } else if host_reports && host_is_up(mux, pane) {
            return Ok(Ready::Ready(ReadyBy::HostStatus));
        } else if let Some(ready) = screen_ready(mux, pane, readiness.lines, polls) {
            // No named line, and no host report: a painted pane after the
            // settle is the last resort.
            return Ok(Ready::Ready(ready));
        }
        polls += 1;
        let waited = start.elapsed();
        if waited >= wait {
            // The wait ran out. A named line that never appeared is answered by
            // the host's own word when it has one and the pane has painted
            // something, so a program whose prompt wording changed still gets
            // its brief rather than costing the run its agent. Anything else is
            // a lost brief, reported with why.
            let painted = mux
                .read(pane, READY_LINES)
                .map(|screen| !screen.trim().is_empty())
                .unwrap_or(false);
            if named && painted && host_reports && host_is_up(mux, pane) {
                return Ok(Ready::Ready(ReadyBy::HostStatus));
            }
            return Err(not_ready_reason(mux, pane, readiness, host_reports, wait));
        }
        std::thread::sleep(READY_POLL.min(wait - waited));
    }
}

/// Whether the host reports an agent in `pane` at all: any state but
/// [`AgentState::Unknown`], and not one that says the process has ended.
fn host_is_up(mux: &dyn Host, pane: &PaneId) -> bool {
    matches!(
        mux.agent_state(pane),
        Ok(AgentState::Working | AgentState::Idle | AgentState::Blocked)
    )
}

/// What the pane's screen says about readiness, when a ready line is asked for.
///
/// `Some(ReadyBy::ReadyLine)` the first time one of the harness's lines is on
/// the screen. A program that names no line at all is ready once the pane has
/// painted anything, but only after [`READY_SETTLE_POLLS`], so a pane whose text
/// is still the shell's `$ ` line is not mistaken for a TUI.
fn screen_ready(mux: &dyn Host, pane: &PaneId, lines: &[String], polls: usize) -> Option<ReadyBy> {
    let screen = mux.read(pane, READY_LINES).ok()?;
    if lines.is_empty() {
        return (polls >= READY_SETTLE_POLLS && !screen.trim().is_empty())
            .then_some(ReadyBy::ScreenPainted);
    }
    let screen = screen.to_lowercase();
    lines
        .iter()
        .any(|line| screen.contains(&line.to_lowercase()))
        .then_some(ReadyBy::ReadyLine)
}

/// The reason a wait that never saw a ready program gives the human.
///
/// It names what was waited for, because the two failures are different: a
/// program the host reports and never did is a CLI that did not start, and one
/// whose ready line never appeared is a CLI that started and never drew its
/// prompt — a login screen or a crash, which the human has to look at.
fn not_ready_reason(
    mux: &dyn Host,
    pane: &PaneId,
    readiness: &Readiness<'_>,
    host_reports: bool,
    wait: Duration,
) -> String {
    let millis = wait.as_millis();
    if host_reports {
        return format!("the agent was not detected within {millis} ms");
    }
    let blank = mux
        .read(pane, READY_LINES)
        .map(|screen| screen.trim().is_empty())
        .unwrap_or(true);
    if blank {
        return format!("the program drew nothing within {millis} ms: it may not have started");
    }
    if readiness.lines.is_empty() {
        return format!("the program never drew a ready screen within {millis} ms");
    }
    let what = readiness.lines.first().map_or_else(
        || "the ready line".to_string(),
        |line| format!("its \"{line}\" line"),
    );
    format!("{what} never showed within {millis} ms: look at the tab, it may be asking something")
}

/// The name of the first blocking prompt the pane's screen is showing, if any.
///
/// The watchdog asks the same question of the orchestrator's pane before it
/// types a notice: a CLI waiting on its own prompt reads the next line as the
/// answer to that prompt.
///
/// `Some` means a pattern actually matched, and is the only answer that counts
/// as blocked. A screen the host handed over with no prompt on it (the normal
/// case), a screen that cannot be read at all, and a spec with no patterns are
/// every one of them `None`: a pane with nothing waiting on it is a pane a
/// brief or a notice may be typed into. The return type says that much, because
/// a `Result` let "the host answered" read like "a prompt is waiting".
///
/// The match is case-insensitive, like [`shprd_agents::spec::HarnessSpec::dry_patterns`],
/// and reads only the last [`BLOCKING_PROMPT_LINES`] lines: a prompt still
/// waiting is on screen, and the scrollback above it may mention one long gone.
pub(crate) fn blocking_prompt(
    mux: &dyn Host,
    pane: &PaneId,
    patterns: &[BlockingPrompt],
) -> Option<String> {
    if patterns.is_empty() {
        return None;
    }
    let screen = mux.read(pane, BLOCKING_PROMPT_LINES).ok()?;
    let screen = screen.to_lowercase();
    patterns
        .iter()
        .find(|pattern| screen.contains(&pattern.pattern.to_lowercase()))
        .map(|pattern| pattern.name.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use shprd_agents::spec::LaunchSpec;

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

    /// A one-tab workspace over `mux`, ready to have a brief typed into its root
    /// pane.
    fn with_pane(mux: shprd_mux::fake::FakeMux) -> (shprd_mux::fake::FakeMux, PaneId) {
        use shprd_mux::Host as _;
        let workspace =
            match mux.create_space("shprd \u{b7} repo", std::path::Path::new("/tmp/repo")) {
                Ok(workspace) => workspace,
                Err(error) => panic!("{error}"),
            };
        let tab = match mux.spawn(
            &workspace.id,
            "sh\u{b7}w1",
            &[],
            &[],
            std::path::Path::new("/tmp"),
        ) {
            Ok(tab) => tab,
            Err(error) => panic!("{error}"),
        };
        (mux, tab.root_pane)
    }

    /// [`with_pane`] over the default fake: every capability on.
    fn fixture() -> (shprd_mux::fake::FakeMux, PaneId) {
        with_pane(shprd_mux::fake::FakeMux::default())
    }

    fn prompts(mux: &shprd_mux::fake::FakeMux) -> Vec<String> {
        use shprd_mux::fake::Call;
        mux.calls()
            .into_iter()
            .filter_map(|call| match call {
                Call::Write(_, text, _) => Some(text),
                _ => None,
            })
            .collect()
    }

    /// A readiness that needs nothing of the host: the program reports its own
    /// status, as `pi` does over OSC 7501.
    fn own_status() -> Readiness<'static> {
        Readiness {
            lines: &[],
            prompts: &[],
            own_status: true,
        }
    }

    /// A host that carries a plugin's report: herdr 0.9.5 has
    /// `pane.report-agent`, so a program whose only sources are plugins is
    /// reported here.
    fn reports_plugins() -> shprd_mux::fake::FakeMux {
        let mut caps = shprd_mux::Capabilities::all();
        caps.agent_reports = true;
        // The host's status is what says the agent is up: this fake reports an
        // idle agent for every pane, which is what `pane.report-agent` looks
        // like once the plugin has sent its first report.
        shprd_mux::fake::FakeMux::new(caps).with_ready_agent()
    }

    /// A host with a core that has no `pane.report_agent`: herdr-core 0.1.21,
    /// where a plugin's report goes nowhere.
    fn no_plugin_reports() -> shprd_mux::fake::FakeMux {
        let mut caps = shprd_mux::Capabilities::all();
        caps.agent_reports = false;
        shprd_mux::fake::FakeMux::new(caps)
    }

    #[test]
    fn a_brief_waits_for_a_late_agent_then_lands() {
        let (mux, pane) = fixture();
        // Not an agent for two polls, an idle agent from the third on.
        mux.set_state_after(&pane, 2, AgentState::Idle);
        let delivery = deliver_brief(
            &mux,
            &pane,
            "the brief",
            &own_status(),
            Duration::from_secs(5),
        );
        assert_eq!(
            delivery,
            Delivery::Delivered {
                ready: ReadyBy::HostStatus
            }
        );
        assert_eq!(prompts(&mux), ["the brief".to_string()]);
    }

    #[test]
    fn an_agent_that_never_shows_up_is_a_warning_not_a_failure() {
        let (mux, pane) = fixture();
        // A zero wait: one poll, no sleep, and the agent is still undetected.
        let delivery = deliver_brief(&mux, &pane, "the brief", &own_status(), Duration::ZERO);
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
        let readiness = Readiness {
            lines: &[],
            prompts: &patterns,
            own_status: true,
        };
        let start = std::time::Instant::now();
        // The screen answers before the first poll finds no agent, and long
        // before the wait would run out.
        let delivery = deliver_brief(
            &mux,
            &pane,
            "the brief",
            &readiness,
            Duration::from_secs(30),
        );
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
        let readiness = Readiness {
            lines: &[],
            prompts: &patterns,
            own_status: true,
        };
        let delivery = deliver_brief(&mux, &pane, "the brief", &readiness, Duration::from_secs(5));
        assert_eq!(
            delivery,
            Delivery::Delivered {
                ready: ReadyBy::HostStatus
            }
        );
        assert_eq!(prompts(&mux), ["the brief".to_string()]);
    }

    // ---- the three readiness paths, one test each ----

    /// The real opencode pane, as its TUI draws it once it can take a turn.
    fn opencode_screen() -> &'static str {
        "                                 ┃\n\
         \u{2503}  Ask anything\u{2026} \"Fix broken tests\"\n\
         \u{2503}\n\
         \u{2503}  Build \u{b7} DeepSeek V4.1 Flash Free OpenCode\n"
    }

    /// What a pane shows while the TUI is still painting.
    fn painting_screen() -> &'static str {
        "\u{2584}\n\u{2588}\u{2580}\u{2580}\u{2588} \u{2588}\u{2580}\u{2580}\u{2588}"
    }

    /// opencode's readiness: no source of its own, and the TUI's own ready line.
    fn opencode_readiness(lines: &[String]) -> Readiness<'_> {
        Readiness {
            lines,
            prompts: &[],
            own_status: false,
        }
    }

    #[test]
    fn a_program_the_host_cannot_report_is_ready_when_its_own_line_is_on_screen() {
        // The bug the M10 gate found: a core without `pane.report_agent` makes
        // opencode's plugin a no-op, so `agent_state` stays Unknown for ever and
        // the brief used to time out into a warning. The TUI's own line is what
        // says it is up.
        let mux = no_plugin_reports();
        let (mux, pane) = with_pane(mux);
        mux.set_screen(&pane, opencode_screen());
        let lines: Vec<String> = vec!["Ask anything".to_string()];
        let delivery = deliver_brief(
            &mux,
            &pane,
            "the brief",
            &opencode_readiness(&lines),
            Duration::from_secs(5),
        );
        assert_eq!(
            delivery,
            Delivery::Delivered {
                ready: ReadyBy::ReadyLine
            },
            "the ready line is what delivers the brief"
        );
        assert_eq!(prompts(&mux), ["the brief".to_string()]);
    }

    #[test]
    fn a_program_the_host_cannot_report_waits_while_its_tui_is_still_painting() {
        // The line has not been drawn yet, so the brief must not be typed into a
        // pane that is not reading. A zero wait is one poll: nothing arrives.
        let mux = no_plugin_reports();
        let (mux, pane) = with_pane(mux);
        mux.set_screen(&pane, painting_screen());
        let lines: Vec<String> = vec!["Ask anything".to_string()];
        let delivery = deliver_brief(
            &mux,
            &pane,
            "the brief",
            &opencode_readiness(&lines),
            Duration::ZERO,
        );
        assert!(
            matches!(delivery, Delivery::NotDelivered(_)),
            "{delivery:?}"
        );
        assert_eq!(
            delivery.reason().as_deref(),
            Some(
                "its \"Ask anything\" line never showed within 0 ms: look at the tab, it may be \
                 asking something"
            )
        );
        assert!(prompts(&mux).is_empty(), "nothing may be typed in yet");
    }

    #[test]
    fn the_ready_line_lands_once_the_tui_finishes_painting() {
        // The same bug, waited out: the first polls see only paint, and once the
        // prompt is drawn the brief goes in.
        let mux = no_plugin_reports();
        let (mux, pane) = with_pane(mux);
        // Three reads see paint, and the read after them sees the prompt: a TUI
        // that is painting when the wait starts and can take a turn a poll later.
        mux.set_screen(&pane, opencode_screen());
        mux.set_screen_after(&pane, 3, painting_screen());
        let lines: Vec<String> = vec!["Ask anything".to_string()];
        let delivery = deliver_brief(
            &mux,
            &pane,
            "the brief",
            &opencode_readiness(&lines),
            Duration::from_secs(10),
        );
        assert_eq!(
            delivery,
            Delivery::Delivered {
                ready: ReadyBy::ReadyLine
            }
        );
        assert_eq!(prompts(&mux), ["the brief".to_string()]);
    }

    #[test]
    fn a_program_with_no_ready_line_is_ready_once_its_pane_has_painted() {
        // The harness names no line, so after the settle a painted pane is a TUI
        // that can take a turn.
        let mux = no_plugin_reports();
        let (mux, pane) = with_pane(mux);
        mux.set_screen(&pane, painting_screen());
        let delivery = deliver_brief(
            &mux,
            &pane,
            "the brief",
            &Readiness {
                lines: &[],
                prompts: &[],
                own_status: false,
            },
            Duration::from_secs(10),
        );
        assert_eq!(
            delivery,
            Delivery::Delivered {
                ready: ReadyBy::ScreenPainted
            }
        );
        assert_eq!(prompts(&mux), ["the brief".to_string()]);
    }

    #[test]
    fn a_pane_that_never_painted_anything_is_a_start_failure_not_a_ready_screen() {
        // The real "the CLI did not start": no host status, no ready line and an
        // empty screen. That stays a warning, and its wording says which of the
        // three failed.
        let mux = no_plugin_reports();
        let (mux, pane) = with_pane(mux);
        let delivery = deliver_brief(
            &mux,
            &pane,
            "the brief",
            &Readiness {
                lines: &[],
                prompts: &[],
                own_status: false,
            },
            Duration::ZERO,
        );
        assert_eq!(
            delivery.reason().as_deref(),
            Some("the program drew nothing within 0 ms: it may not have started")
        );
        assert!(prompts(&mux).is_empty(), "nothing may be typed in");
    }

    #[test]
    fn a_program_that_reports_itself_is_ready_on_its_own_word() {
        // `pi` answers the 7501 query itself, so a host status about it is the
        // program's own word. A ready line the file also names is not waited for:
        // a resumed session whose footer moved must not cost the run its agent.
        let (mux, pane) = fixture();
        mux.set_state(&pane, AgentState::Idle);
        // Its screen shows neither the ready line nor anything the file names.
        mux.set_screen(&pane, "a resumed pi session with a moved footer");
        let lines: Vec<String> = vec!["0.0%/".to_string()];
        let delivery = deliver_brief(
            &mux,
            &pane,
            "the brief",
            &Readiness {
                lines: &lines,
                prompts: &[],
                own_status: true,
            },
            Duration::from_secs(5),
        );
        assert_eq!(
            delivery,
            Delivery::Delivered {
                ready: ReadyBy::HostStatus
            }
        );
    }

    #[test]
    fn a_named_ready_line_is_the_evidence_even_when_the_host_could_report() {
        // The host's word is not enough on its own: herdr detects opencode the
        // moment the process starts and calls it `idle` while its TUI is still
        // painting, and a brief typed then is pasted into a program not yet
        // reading. So the program's own line wins wherever the file names one.
        let mux = reports_plugins();
        let (mux, pane) = with_pane(mux);
        mux.set_screen(&pane, opencode_screen());
        let lines: Vec<String> = vec!["Ask anything".to_string()];
        let delivery = deliver_brief(
            &mux,
            &pane,
            "the brief",
            &opencode_readiness(&lines),
            Duration::from_secs(5),
        );
        assert_eq!(
            delivery,
            Delivery::Delivered {
                ready: ReadyBy::ReadyLine
            }
        );
    }

    #[test]
    fn a_host_that_carries_plugin_reports_delivers_a_program_with_no_ready_line() {
        // A program whose file names no line, on a host that does report it:
        // the status is the only evidence there is, and it delivers.
        let mux = reports_plugins();
        let (mux, pane) = with_pane(mux);
        mux.set_screen(&pane, opencode_screen());
        let delivery = deliver_brief(
            &mux,
            &pane,
            "the brief",
            &Readiness {
                lines: &[],
                prompts: &[],
                own_status: false,
            },
            Duration::from_secs(5),
        );
        assert_eq!(
            delivery,
            Delivery::Delivered {
                ready: ReadyBy::HostStatus
            }
        );
    }

    #[test]
    fn a_ready_line_that_never_shows_still_ends_in_the_hosts_word() {
        // A newer build that reworded its prompt must not cost the run its
        // agent: when the wait runs out and the host does report an agent in a
        // painted pane, the brief goes in after all.
        let mux = reports_plugins();
        let (mux, pane) = with_pane(mux);
        // A painted pane whose wording is not the one the file names.
        mux.set_screen(&pane, "a TUI, painted, with different words");
        let lines: Vec<String> = vec!["Ask anything".to_string()];
        let delivery = deliver_brief(
            &mux,
            &pane,
            "the brief",
            &opencode_readiness(&lines),
            Duration::ZERO,
        );
        assert_eq!(
            delivery,
            Delivery::Delivered {
                ready: ReadyBy::HostStatus
            },
            "the host's own word is the last resort rather than a lost brief"
        );
        assert_eq!(prompts(&mux), ["the brief".to_string()]);
    }

    #[test]
    fn a_host_without_agent_state_falls_back_to_the_ready_line() {
        // The capability flags are the host's own answer, and they win over the
        // harness: a host that says it cannot report state cannot report this
        // program either, whatever its file claims.
        let mut caps = shprd_mux::Capabilities::all();
        caps.agent_state = false;
        caps.agent_reports = false;
        let mux = shprd_mux::fake::FakeMux::new(caps);
        let (mux, pane) = with_pane(mux);
        mux.set_screen(&pane, opencode_screen());
        let lines: Vec<String> = vec!["Ask anything".to_string()];
        let delivery = deliver_brief(
            &mux,
            &pane,
            "the brief",
            &Readiness {
                lines: &lines,
                prompts: &[],
                own_status: true,
            },
            Duration::from_secs(5),
        );
        assert_eq!(
            delivery,
            Delivery::Delivered {
                ready: ReadyBy::ReadyLine
            }
        );
    }

    #[test]
    fn a_blocking_prompt_wins_over_a_ready_line() {
        // A TUI that is up but asking its own question must never be typed over,
        // even when its ready line is also on screen.
        let mux = no_plugin_reports();
        let (mux, pane) = with_pane(mux);
        mux.set_screen(
            &pane,
            "Ask anything\nIs this a project you created or one you trust?",
        );
        let patterns = trust_patterns();
        let lines: Vec<String> = vec!["Ask anything".to_string()];
        let delivery = deliver_brief(
            &mux,
            &pane,
            "the brief",
            &Readiness {
                lines: &lines,
                prompts: &patterns,
                own_status: false,
            },
            Duration::from_secs(30),
        );
        assert_eq!(
            delivery,
            Delivery::Blocked {
                prompt: "folder trust".to_string()
            }
        );
        assert!(
            prompts(&mux).is_empty(),
            "nothing may be typed over a prompt"
        );
    }
}
