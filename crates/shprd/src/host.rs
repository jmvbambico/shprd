//! Which terminal host shprd drives, and where that is decided.
//!
//! shprd can drive three hosts (see [docs/plan.md]'s M10 and
//! [`shprd_mux::Host`]): Herdr desktop, which is herdr-core plus the Herdr
//! window; the herdr TUI; and tuios, which speaks herdr's command line through
//! its compatibility layer. Which one a run uses is the `host` setting —
//! `herdr-desktop`, `herdr`, `tuios` or `auto` — and `auto`, the default, asks
//! the machine:
//!
//! - **Herdr desktop** when a Herdr window answers *and* a herdr-core answers for
//!   the session this process is in;
//! - **the herdr TUI** otherwise, because it is what a person running shprd
//!   inside herdr or tuios already has.
//!
//! The choice is made **once, at the wiring edge** — [`select`] — and passed
//! down as a `Box<dyn Host>`, so no library has to know which host it is talking
//! to. [`resolve`] is the same choice without opening a host, which is what
//! `shprd doctor` prints: the host, and why it was the one chosen.
//!
//! # The probe starts nothing it was not asked to
//!
//! Asking a herdr-core anything *starts* it when no core is running (M7ef §2.3),
//! so the probe asks the **window** first with `herdr-app status`, which starts
//! nothing, and asks the core only when a window answered — a desktop without a
//! window has nobody to show a terminal to. The one exception is a person who
//! named `host = "herdr-desktop"`: then the core is asked too, so the answer can
//! say which half is missing.
//!
//! # Decisions this file makes
//!
//! A host the settings name and the machine cannot give falls **back** to the
//! next one that can, with the reason kept in [`Resolution::why`]: a person on
//! the road must not lose every verb because their desktop is not running. A
//! core that answered the probe and then refuses to connect at open time is the
//! same case, reported as a warning and served by the herdr TUI.
//!
//! [docs/plan.md]: ../../../docs/plan.md

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use shprd_core::settings::{HostChoice, Loaded};
use shprd_mux::Host;
use shprd_mux_hc::{Endpoint, HcHost, Window};

/// The environment variable naming the `hc` binary a probe and a host use.
pub const HC_ENV: &str = "SHPRD_HC";

/// The environment variable naming the `herdr-app` binary.
pub const HERDR_APP_ENV: &str = "SHPRD_HERDR_APP";

/// How long one probe may take.
///
/// A core answers `hello` in 15–25 ms once it is up (M7ef §2.3); the margin is
/// for the first call of a session, which starts one.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the one retry of a core probe may take.
///
/// A probe that fails while several builds are running is retried once with a
/// longer wait before the machine is declared to have no desktop: a busy core
/// answers late, and "late" must not read as "not there".
const PROBE_RETRY_TIMEOUT: Duration = Duration::from_secs(20);

/// The hosts shprd has an adapter for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKind {
    /// Herdr desktop: herdr-core for terminals, the window for the layout.
    HerdrDesktop,
    /// The herdr TUI.
    Herdr,
    /// The tuios TUI, through its herdr-compatible shim.
    Tuios,
}

impl HostKind {
    /// The word the setting and the banner use.
    pub fn name(self) -> &'static str {
        match self {
            Self::HerdrDesktop => "herdr-desktop",
            Self::Herdr => "herdr",
            Self::Tuios => "tuios",
        }
    }

    /// The program this host is driven through.
    pub fn binary(self) -> &'static str {
        match self {
            Self::HerdrDesktop => "hc",
            Self::Herdr | Self::Tuios => "herdr",
        }
    }
}

impl fmt::Display for HostKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// What the machine has, as the choice sees it.
///
/// It is plain data, so the choice itself is a pure function of it and a test
/// can say what every combination of facts picks without a core or a window.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Probe {
    /// The herdr-core version that answered, when one did.
    pub core: Option<String>,
    /// Why no core was asked, or answered, when there is no version.
    pub core_problem: Option<String>,
    /// Whether a Herdr window answered `herdr-app status`.
    pub window: bool,
    /// Why the window did not answer, when it did not.
    pub window_problem: Option<String>,
    /// Whether the `herdr` binary is on `PATH`.
    pub herdr: bool,
    /// Whether the `tuios` binary is on `PATH`.
    pub tuios: bool,
    /// Whether a herdr server is running: its socket is there. `auto` falls
    /// back to the herdr TUI only when this is true.
    pub herdr_running: bool,
}

impl Probe {
    /// Whether Herdr desktop is there: a window to show terminals in and a core
    /// to run them.
    pub fn desktop(&self) -> bool {
        self.window && self.core.is_some()
    }
}

/// What the wiring edge chose, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    /// The setting the choice started from.
    pub setting: HostChoice,
    /// The host the run will drive.
    pub kind: HostKind,
    /// One line a person can read: what was chosen, and on what evidence.
    pub why: String,
    /// True when the setting named a host the machine could not give, and the
    /// run fell back to another.
    pub fell_back: bool,
    /// False when `auto` found no host that is running — no desktop and no herdr
    /// server. The run is then not on a host at all: `kind` is the adapter whose
    /// calls will report the error, and `why` says plainly that nothing runs.
    pub running: bool,
    /// The facts the choice was made from.
    pub probe: Probe,
}

impl Resolution {
    /// The banner's line: `host: <kind> — <why>`.
    pub fn line(&self) -> String {
        format!("host: {} — {}", self.kind, self.why)
    }
}

/// The host a setting and a set of facts pick, and the reason in words.
///
/// This is the whole selection rule, and it is pure: nothing here asks the
/// machine anything, so a test can name the facts it wants to see decided.
pub fn choose(setting: HostChoice, probe: Probe) -> Resolution {
    match setting {
        HostChoice::HerdrDesktop => {
            if probe.desktop() {
                Resolution {
                    setting,
                    kind: HostKind::HerdrDesktop,
                    why: desktop_why(&probe),
                    fell_back: false,
                    running: true,
                    probe,
                }
            } else {
                // The herdr TUI is the answer even when `herdr` is not on
                // `PATH`: tuios is reached *through* herdr's command line, so a
                // machine with either has the same adapter, and a machine with
                // neither gets the error the adapter's own calls report.
                let kind = HostKind::Herdr;
                let why = format!(
                    "host = herdr-desktop, but {}; using {kind} instead",
                    desktop_problem(&probe)
                );
                Resolution {
                    setting,
                    kind,
                    why,
                    fell_back: true,
                    running: true,
                    probe,
                }
            }
        }
        HostChoice::Herdr => {
            let why = if probe.herdr {
                "host = herdr: the herdr TUI".to_string()
            } else {
                "host = herdr: the herdr TUI, and `herdr` is not on PATH".to_string()
            };
            Resolution {
                setting,
                kind: HostKind::Herdr,
                why,
                fell_back: false,
                running: true,
                probe,
            }
        }
        HostChoice::Tuios => {
            let why = if probe.tuios {
                "host = tuios: tuios through its herdr-compatible shim".to_string()
            } else {
                "host = tuios: tuios through its herdr-compatible shim, and `tuios` is not on PATH"
                    .to_string()
            };
            Resolution {
                setting,
                kind: HostKind::Tuios,
                why,
                fell_back: false,
                running: true,
                probe,
            }
        }
        HostChoice::Auto => {
            if probe.desktop() {
                Resolution {
                    setting,
                    kind: HostKind::HerdrDesktop,
                    why: format!("auto picked Herdr desktop: {}", desktop_why(&probe)),
                    fell_back: false,
                    running: true,
                    probe,
                }
            } else if probe.herdr_running {
                // `auto` never falls back: it asked the machine and was told.
                // Only a named host the machine could not give is a fallback a
                // person should hear about.
                let kind = HostKind::Herdr;
                Resolution {
                    setting,
                    kind,
                    why: format!("auto picked {kind}: {}", desktop_problem(&probe)),
                    fell_back: false,
                    running: true,
                    probe,
                }
            } else {
                // Neither host is running. Picking the herdr TUI would only
                // postpone the failure to the first call that needs it (and
                // look like a choice), so say it plainly instead.
                let kind = HostKind::Herdr;
                Resolution {
                    setting,
                    kind,
                    why: format!(
                        "auto found no running host: {}, and no herdr server is running",
                        desktop_problem(&probe)
                    ),
                    fell_back: false,
                    running: false,
                    probe,
                }
            }
        }
    }
}

/// The evidence that picked Herdr desktop, in words.
fn desktop_why(probe: &Probe) -> String {
    format!(
        "herdr-core {} answers{}",
        probe.core.as_deref().unwrap_or("?"),
        if probe.window {
            " and a Herdr window is attached"
        } else {
            ""
        }
    )
}

/// What Herdr desktop is missing, in words.
fn desktop_problem(probe: &Probe) -> String {
    match (probe.window, probe.core.as_deref()) {
        (false, _) => format!(
            "no Herdr window is attached ({})",
            probe
                .window_problem
                .as_deref()
                .unwrap_or("`herdr-app status` did not answer")
        ),
        (true, None) => format!(
            "no herdr-core answered ({})",
            probe.core_problem.as_deref().unwrap_or("it did not answer")
        ),
        (true, Some(_)) => "it is available".to_string(),
    }
}

/// Resolve the host for this machine, without opening it.
///
/// The settings are read for this home and repository — `host` is a machine
/// setting, so the repository's committed file cannot set it — and the machine
/// is asked only as much as the setting needs.
pub fn resolve() -> Resolution {
    let home = crate::ctx::resolve_home(None);
    let repo = crate::ctx::resolve_root(None).ok();
    let setting = home
        .as_ref()
        .map(|home| load_setting(home, repo.as_deref()))
        .unwrap_or_default();
    let probe = probe(setting);
    choose(setting, probe)
}

/// Resolve the host and open it: the one place a host is built.
///
/// It answers the [`Resolution`] beside the host, so the service's banner and
/// `shprd doctor` can say which host a run is on without probing a second time.
///
/// # The choice is sticky
///
/// The first healthy resolution of this process is kept, and every later call
/// reuses it without probing: a probe that fails while the machine is busy must
/// not turn a service that found the desktop into one that drives a herdr that
/// is not running. It is dropped when the host it names stops opening, so the
/// next call resolves afresh — that is the "connection lost, and comes back"
/// case. A resolution that found no running host is never kept.
pub fn select() -> (Resolution, Box<dyn Host>) {
    let kept = sticky().clone();
    let mut resolution = kept.clone().unwrap_or_else(resolve);
    let host = open(&mut resolution);
    let mut slot = sticky();
    if resolution.running && !resolution.fell_back {
        *slot = Some(resolution.clone());
    } else if kept.is_some() {
        *slot = None;
    }
    (resolution, host)
}

/// The resolution this process keeps; see [`select`].
static STICKY: Mutex<Option<Resolution>> = Mutex::new(None);

/// The kept resolution's slot, usable even if a holder panicked.
fn sticky() -> std::sync::MutexGuard<'static, Option<Resolution>> {
    STICKY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Open the host `resolution` names.
///
/// A Herdr desktop that answered the probe and then will not connect — the
/// window quit in between, the core was restarted — is served by the herdr TUI
/// with a warning rather than by a run that cannot start a task. The warning is
/// the whole point: a person must not be left believing their terminals are on
/// the desktop when they are not.
fn open(resolution: &mut Resolution) -> Box<dyn Host> {
    if resolution.kind != HostKind::HerdrDesktop {
        return Box::new(shprd_mux_herdr::HerdrMux::new());
    }
    match HcHost::connect(endpoint(), window()) {
        Ok(host) => Box::new(host),
        Err(error) => {
            // The resolution is amended, not just warned about: the banner and
            // `--json` describe the host a run actually drives, and a service
            // that says "herdr-desktop" while typing into the herdr TUI says two
            // different things about the same run.
            fell_back_at_open(resolution, &error.to_string());
            eprintln!("warning: {}", resolution.why);
            Box::new(shprd_mux_herdr::HerdrMux::new())
        }
    }
}

/// Record that the host the setting named did not open, and the run is on the
/// herdr TUI instead.
///
/// It is a separate function so the wording is one thing, and so a test can
/// check what a person then reads without a desktop that refuses to connect.
fn fell_back_at_open(resolution: &mut Resolution, reason: &str) {
    let named = resolution.kind;
    resolution.kind = HostKind::Herdr;
    resolution.fell_back = true;
    resolution.why =
        format!("host = {named}, but herdr-core did not open ({reason}); using herdr instead");
}

/// The `host` setting for this home and repository, `auto` when it cannot be
/// read: a broken settings file is the settings page's business, and it must not
/// stop every verb from running.
fn load_setting(home: &Path, repo: Option<&Path>) -> HostChoice {
    Loaded::read(home, repo)
        .map(|loaded| loaded.resolve(None).host())
        .unwrap_or_default()
}

/// Ask the machine what it has.
///
/// The window first, because `herdr-app status` starts nothing; the core only
/// when a window answered or when a person named Herdr desktop in their
/// settings, so a shprd verb on a TUI-only machine never starts a stray core.
fn probe(setting: HostChoice) -> Probe {
    let window = window();
    // One `herdr-app status` answers both "is a window there" and "why not".
    let status = window.status();
    let attached = status.is_ok();
    let mut probe = Probe {
        window: attached,
        window_problem: status.err().map(|error| error.to_string()),
        core: None,
        core_problem: None,
        herdr: on_path("herdr"),
        tuios: on_path("tuios"),
        herdr_running: herdr_socket().is_some_and(|socket| socket.exists()),
    };
    let ask_core = attached || setting == HostChoice::HerdrDesktop;
    if ask_core {
        let endpoint = endpoint();
        // A core that does not answer in time is asked once more with a longer
        // wait, but only when a window is attached (or the desktop was named):
        // there is then a desktop to be found, and a busy machine is the likely
        // reason it was late.
        match ask_core_with_retry(|timeout| {
            HcHost::core_version(&endpoint, timeout).map_err(|error| error.to_string())
        }) {
            Ok(version) => probe.core = Some(version),
            Err(error) => probe.core_problem = Some(error),
        }
    } else {
        probe.core_problem =
            Some("no Herdr window is attached, so herdr-core was not asked".to_string());
    }
    probe
}

/// Ask for the core's version, and once more with a longer wait when the first
/// try fails. `ask` takes the timeout it may use; the retry is the whole reason
/// this is a function: a probe that fails under load must not decide the host.
fn ask_core_with_retry(
    mut ask: impl FnMut(Duration) -> Result<String, String>,
) -> Result<String, String> {
    ask(PROBE_TIMEOUT).or_else(|_| ask(PROBE_RETRY_TIMEOUT))
}

/// Where the herdr TUI's server socket is: `$HERDR_SOCKET_PATH`, else
/// `~/.config/herdr/herdr.sock`. `None` when neither can be named.
fn herdr_socket() -> Option<PathBuf> {
    if let Some(path) = env_value("HERDR_SOCKET_PATH") {
        return Some(PathBuf::from(path));
    }
    let home = env_value("HOME")?;
    Some(PathBuf::from(home).join(".config/herdr/herdr.sock"))
}

/// Whether a program is on `PATH`.
fn on_path(binary: &str) -> bool {
    let path = std::env::var("PATH").unwrap_or_default();
    shprd_agents::detect::find_binary(binary, &path).is_some()
}

/// One environment variable, with an empty one read as unset.
///
/// The same rule [`crate::ctx`] keeps: the environment is read once, at the edge,
/// and carried in as a value.
fn env_value(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

/// The herdr-core this process's environment names.
///
/// `$SHPRD_HC` picks the binary, `$HC_SESSION` / `$HC_SOCKET` the core, and the
/// five XDG directories where that core keeps its socket and terminals. Nothing
/// is inherited by the client itself: it drops every `HC_*` variable it was not
/// given, so a private session stays private and a default one stays the
/// person's.
pub fn endpoint() -> Endpoint {
    let binary = env_value(HC_ENV).unwrap_or_else(|| "hc".to_string());
    let mut endpoint = Endpoint::with_binary(binary);
    if let Some(session) = env_value("HC_SESSION") {
        endpoint = endpoint.with_session(session);
    }
    if let Some(socket) = env_value("HC_SOCKET") {
        endpoint = endpoint.with_socket(socket);
    }
    let directories: Vec<(String, String)> = Endpoint::DIRECTORIES
        .iter()
        .filter_map(|name| env_value(name).map(|value| ((*name).to_string(), value)))
        .collect();
    endpoint.with_directories(directories)
}

/// The Herdr window this process's environment names.
///
/// The same session and directories as [`endpoint`], so the window and the core
/// are the same pair; `$SHPRD_HERDR_APP` picks the binary and `$HERDR_APP_HOME`
/// the window's home, which is how a test or a sandbox points at its own window.
pub fn window() -> Window {
    let binary = env_value(HERDR_APP_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("herdr-app"));
    let mut window = Window::from_endpoint(&endpoint()).with_bin(binary);
    if let Some(home) = env_value("HERDR_APP_HOME") {
        window = window.with_variable("HERDR_APP_HOME", home);
    }
    window
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{HostKind, Probe, choose};
    use shprd_core::settings::HostChoice;

    /// A machine with nothing on it.
    fn nothing() -> Probe {
        Probe {
            core: None,
            core_problem: Some("no Herdr window is attached, so herdr-core was not asked".into()),
            window: false,
            window_problem: Some("no Herdr window at /tmp/none: No such file or directory".into()),
            herdr: true,
            tuios: false,
            herdr_running: true,
        }
    }

    /// A machine running Herdr desktop: a core answers and a window shows it.
    fn desktop() -> Probe {
        Probe {
            core: Some("0.1.21".to_string()),
            core_problem: None,
            window: true,
            window_problem: None,
            herdr: true,
            tuios: true,
            herdr_running: true,
        }
    }

    #[test]
    fn auto_picks_the_desktop_when_a_core_and_a_window_answer() {
        let chosen = choose(HostChoice::Auto, desktop());
        assert_eq!(chosen.kind, HostKind::HerdrDesktop);
        assert!(!chosen.fell_back, "auto asks; it never falls back");
        assert!(
            chosen.why.starts_with("auto picked Herdr desktop"),
            "{}",
            chosen.why
        );
        assert!(
            chosen.why.contains("0.1.21"),
            "the version is named: {}",
            chosen.why
        );
        assert_eq!(
            chosen.line(),
            "host: herdr-desktop — auto picked Herdr desktop: herdr-core 0.1.21 answers and a Herdr window is attached"
        );
    }

    #[test]
    fn auto_picks_the_tui_when_no_window_is_attached() {
        let chosen = choose(HostChoice::Auto, nothing());
        assert_eq!(chosen.kind, HostKind::Herdr);
        assert!(!chosen.fell_back);
        assert!(
            chosen.why.contains("no Herdr window is attached"),
            "the reason a person can act on: {}",
            chosen.why
        );
        assert!(chosen.why.contains("/tmp/none"), "{}", chosen.why);
    }

    #[test]
    fn auto_never_picks_a_herdr_that_is_not_running() {
        let mut probe = nothing();
        probe.herdr_running = false;
        let chosen = choose(HostChoice::Auto, probe);
        assert!(!chosen.running, "nothing runs, and it says so");
        assert!(
            chosen.why.contains("no running host") && chosen.why.contains("no herdr server"),
            "{}",
            chosen.why
        );
        assert!(!chosen.why.starts_with("auto picked"), "{}", chosen.why);
        let chosen = choose(HostChoice::Auto, nothing());
        assert!(chosen.running, "a running herdr is still the answer");
    }

    #[test]
    fn a_desktop_probe_that_times_out_once_still_uses_the_desktop() {
        let mut asked = Vec::new();
        let answer = super::ask_core_with_retry(|timeout| {
            asked.push(timeout);
            if asked.len() == 1 {
                Err("timed out".to_string())
            } else {
                Ok("0.1.22".to_string())
            }
        });
        assert_eq!(answer.as_deref(), Ok("0.1.22"));
        assert_eq!(asked.len(), 2);
        assert!(asked[1] > asked[0], "the retry waits longer: {asked:?}");
        let mut probe = desktop();
        probe.core = answer.ok();
        assert_eq!(choose(HostChoice::Auto, probe).kind, HostKind::HerdrDesktop);
        // A core that never answers is asked twice, then reported.
        let mut calls = 0;
        let failed = super::ask_core_with_retry(|_| {
            calls += 1;
            Err("no".to_string())
        });
        assert!(failed.is_err());
        assert_eq!(calls, 2);
    }

    #[test]
    fn auto_picks_the_tui_when_the_core_does_not_answer() {
        let mut probe = desktop();
        probe.core = None;
        probe.core_problem = Some("herdr-core needs to be 0.1.21 or newer".to_string());
        let chosen = choose(HostChoice::Auto, probe);
        assert_eq!(chosen.kind, HostKind::Herdr);
        assert!(
            chosen.why.contains("no herdr-core answered"),
            "{}",
            chosen.why
        );
        assert!(chosen.why.contains("0.1.21 or newer"), "{}", chosen.why);
    }

    #[test]
    fn a_window_without_a_core_is_not_a_desktop() {
        let mut probe = desktop();
        probe.core = None;
        probe.core_problem = Some("cannot start `hc connect`".to_string());
        let chosen = choose(HostChoice::Auto, probe);
        assert_eq!(chosen.kind, HostKind::Herdr);
    }

    #[test]
    fn a_named_desktop_that_is_not_there_falls_back_and_says_so() {
        let chosen = choose(HostChoice::HerdrDesktop, nothing());
        assert_eq!(chosen.kind, HostKind::Herdr);
        assert!(
            chosen.fell_back,
            "a named host that is missing is a fallback"
        );
        assert!(
            chosen.why.starts_with("host = herdr-desktop, but"),
            "{}",
            chosen.why
        );
        assert!(
            chosen.why.ends_with("using herdr instead"),
            "{}",
            chosen.why
        );
    }

    #[test]
    fn a_named_desktop_that_is_there_is_used_without_a_guess() {
        let chosen = choose(HostChoice::HerdrDesktop, desktop());
        assert_eq!(chosen.kind, HostKind::HerdrDesktop);
        assert!(!chosen.fell_back);
        assert!(chosen.why.contains("herdr-core 0.1.21"), "{}", chosen.why);
    }

    #[test]
    fn a_named_tui_is_used_even_when_the_desktop_is_there() {
        let chosen = choose(HostChoice::Herdr, desktop());
        assert_eq!(chosen.kind, HostKind::Herdr);
        assert!(!chosen.fell_back, "a person's own choice is not a fallback");
        let chosen = choose(HostChoice::Tuios, desktop());
        assert_eq!(chosen.kind, HostKind::Tuios);
        assert!(chosen.why.contains("tuios"), "{}", chosen.why);
    }

    #[test]
    fn a_tui_that_is_not_on_path_still_names_itself() {
        let mut probe = nothing();
        probe.herdr = false;
        probe.tuios = false;
        let chosen = choose(HostChoice::Herdr, probe.clone());
        assert_eq!(chosen.kind, HostKind::Herdr);
        assert!(chosen.why.contains("not on PATH"), "{}", chosen.why);
        let chosen = choose(HostChoice::Tuios, probe);
        assert!(chosen.why.contains("not on PATH"), "{}", chosen.why);
    }

    #[test]
    fn an_open_time_fallback_amends_what_the_banner_says() {
        // The probe said the desktop was there; opening it failed. What a person
        // reads — and what `--json` carries — must be the host the run is on,
        // not the one the setting named.
        let mut resolution = choose(HostChoice::HerdrDesktop, super::tests::desktop());
        assert_eq!(resolution.kind, HostKind::HerdrDesktop);
        super::fell_back_at_open(&mut resolution, "herdr-core is unreachable: it died");
        assert_eq!(resolution.kind, HostKind::Herdr);
        assert!(resolution.fell_back);
        assert_eq!(
            resolution.line(),
            "host: herdr — host = herdr-desktop, but herdr-core did not open \
             (herdr-core is unreachable: it died); using herdr instead"
        );
        assert_eq!(
            resolution.probe.core.as_deref(),
            Some("0.1.21"),
            "the facts stay"
        );
    }

    #[test]
    fn the_kind_names_the_program_it_is_driven_through() {
        assert_eq!(HostKind::HerdrDesktop.binary(), "hc");
        assert_eq!(HostKind::Herdr.binary(), "herdr");
        assert_eq!(HostKind::Tuios.binary(), "herdr");
        assert_eq!(HostKind::HerdrDesktop.to_string(), "herdr-desktop");
    }
}
