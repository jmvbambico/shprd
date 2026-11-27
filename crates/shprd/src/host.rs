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
    /// Whether a herdr server is running: the herdr CLI got an answer from it. `auto` falls
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
/// Private on purpose: every production caller goes through [`select`], which
/// keeps the choice sticky.
///
/// The settings are read for this home and repository — `host` is a machine
/// setting, so the repository's committed file cannot set it — and the machine
/// is asked only as much as the setting needs.
fn resolve() -> Resolution {
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
    #[cfg(test)]
    if let Some(result) = test_select::run() {
        return result;
    }
    select_in(&STICKY, resolve, open)
}

/// A test's own slot, resolver and opener in place of the machine's, so a test
/// can drive a verb that calls [`select`] and count what it asked.
#[cfg(test)]
pub(crate) mod test_select {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    /// What the fake machine was asked.
    #[derive(Default)]
    pub(crate) struct Counts {
        /// Times the machine was probed.
        pub resolves: Cell<u32>,
        /// Times a host was opened.
        pub opens: Cell<u32>,
    }

    struct Installed {
        slot: Mutex<Option<Resolution>>,
        counts: Rc<Counts>,
        probe: Probe,
    }

    thread_local! {
        static INSTALLED: RefCell<Option<Installed>> = const { RefCell::new(None) };
    }

    /// Replace the machine, on this thread, with a desktop that answers; the
    /// returned counts say how often it was probed and opened.
    pub(crate) fn install(probe: Probe) -> Rc<Counts> {
        let counts = Rc::new(Counts::default());
        INSTALLED.with(|cell| {
            *cell.borrow_mut() = Some(Installed {
                slot: Mutex::new(None),
                counts: counts.clone(),
                probe,
            });
        });
        counts
    }

    /// Put the real machine back.
    pub(crate) fn uninstall() {
        INSTALLED.with(|cell| *cell.borrow_mut() = None);
    }

    pub(super) fn run() -> Option<(Resolution, Box<dyn Host>)> {
        INSTALLED.with(|cell| {
            let cell = cell.borrow();
            let installed = cell.as_ref()?;
            Some(select_in(
                &installed.slot,
                || {
                    installed
                        .counts
                        .resolves
                        .set(installed.counts.resolves.get() + 1);
                    choose(HostChoice::Auto, installed.probe.clone())
                },
                |_| {
                    installed.counts.opens.set(installed.counts.opens.get() + 1);
                    Box::new(shprd_mux_herdr::HerdrMux::with_bin("/usr/bin/true"))
                },
            ))
        })
    }
}

/// [`select`] over a slot, a resolver and an opener, so the keeping can be
/// tested without a machine: `resolve` is called only when the slot is empty.
fn select_in(
    slot: &Mutex<Option<Resolution>>,
    resolve: impl FnOnce() -> Resolution,
    open: impl FnOnce(&mut Resolution) -> Box<dyn Host>,
) -> (Resolution, Box<dyn Host>) {
    // One select at a time: two callers in the same tick must not both probe.
    let _one_at_a_time = SELECTING.lock().unwrap_or_else(|p| p.into_inner());
    let kept = kept_slot(slot).clone();
    let mut resolution = kept.clone().unwrap_or_else(resolve);
    if kept.is_none()
        && !resolution.running
        && !NOTHING_WARNED.swap(true, std::sync::atomic::Ordering::Relaxed)
    {
        eprintln!("warning: {}", resolution.why);
    }
    let host = open(&mut resolution);
    let mut held = kept_slot(slot);
    if resolution.running && !resolution.fell_back {
        *held = Some(resolution.clone());
    } else if kept.is_some() {
        *held = None;
    }
    (resolution, host)
}

/// Serialises [`select_in`] so the read-resolve-write is one step.
static SELECTING: Mutex<()> = Mutex::new(());

/// Set once the "nothing is running" warning has been printed, so a hostless
/// machine is told once, not on every spawn and review.
static NOTHING_WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The resolution this process keeps; see [`select`].
static STICKY: Mutex<Option<Resolution>> = Mutex::new(None);

/// The kept resolution's slot, usable even if a holder panicked.
fn kept_slot(slot: &Mutex<Option<Resolution>>) -> std::sync::MutexGuard<'_, Option<Resolution>> {
    slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
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
        herdr_running: on_path("herdr")
            && herdr_server_answers(shprd_mux_herdr::HerdrMux::new(), HERDR_PROBE_TIMEOUT),
    };
    let ask_core = attached || setting == HostChoice::HerdrDesktop;
    if ask_core {
        let endpoint = endpoint();
        // A core that does not answer in time is asked once more with a longer
        // wait: a busy machine answers late, and "late" must not read as "not
        // there". This branch only runs when a window is attached or the desktop
        // was named, so no ordinary resolve pays for the longer wait.
        match ask_core_with_retry(|timeout| {
            HcHost::core_version(&endpoint, timeout).map_err(probe_error)
        }) {
            Ok(version) => probe.core = Some(version),
            Err(error) => probe.core_problem = Some(error.message),
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
///
/// Only a timeout is retried: a core that refuses the connection outright is not
/// there, and waiting longer for it would stall every resolve on a machine
/// where the desktop is genuinely absent.
fn ask_core_with_retry(
    mut ask: impl FnMut(Duration) -> Result<String, ProbeError>,
) -> Result<String, ProbeError> {
    match ask(PROBE_TIMEOUT) {
        Err(error) if error.timed_out => ask(PROBE_RETRY_TIMEOUT),
        other => other,
    }
}

/// Classify a client error: only [`shprd_mux_hc::Error::Timeout`] is "slow".
fn probe_error(error: shprd_mux_hc::Error) -> ProbeError {
    ProbeError {
        timed_out: matches!(error, shprd_mux_hc::Error::Timeout { .. }),
        message: error.to_string(),
    }
}

/// Why a core probe failed, and whether it was only slow.
#[derive(Debug, PartialEq, Eq)]
struct ProbeError {
    /// The core did not answer in time (as opposed to refusing or erroring).
    timed_out: bool,
    /// What to tell a person.
    message: String,
}

/// Whether a herdr TUI server is running: the `herdr` adapter lists its
/// workspaces, the very call a spawn or a review makes first.
///
/// A socket that merely accepts a connection is not proof: `HERDR_SOCKET_PATH`
/// in an hc child names the Herdr desktop's own socket, which listens but is not
/// a herdr server, and a leftover socket file from a herdr that exited is not one
/// either. Only a server that answers the herdr CLI counts.
///
/// The herdr CLI is asked on its own thread and given `timeout`: a socket that
/// accepts and never answers would otherwise hold the serialized [`select`], and
/// with it every spawn and review. A call that does not answer in time counts as
/// "not running".
fn herdr_server_answers(mux: shprd_mux_herdr::HerdrMux, timeout: Duration) -> bool {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(mux.list_spaces().is_ok());
    });
    rx.recv_timeout(timeout).unwrap_or(false)
}

/// How long the herdr CLI may take to answer the "is a server running" probe.
const HERDR_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

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

    use super::{Host, HostKind, Probe, Resolution, choose};
    use shprd_core::settings::HostChoice;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

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
    fn spawns_and_reviews_reuse_the_service_resolution() {
        let counts = super::test_select::install(desktop());
        // The service resolves at start; spawns start later through `spawn::real_mux`, and
        // none probes again; the review path is tested in `cmd::review`.
        let (first, _) = super::select();
        assert_eq!(first.kind, HostKind::HerdrDesktop);
        let _spawn = crate::cmd::spawn::real_mux();
        let _spawn_again = crate::cmd::spawn::real_mux();
        super::test_select::uninstall();
        assert_eq!(counts.resolves.get(), 1, "the machine was probed once");
        assert_eq!(counts.opens.get(), 3, "every caller still gets a host");
    }

    #[test]
    fn a_herdr_that_never_answers_counts_as_not_running() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("shprd-fake-herdr-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("herdr");
        std::fs::write(&bin, "#!/bin/sh\nexec sleep 3\n").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let started = std::time::Instant::now();
        let answered = super::herdr_server_answers(
            shprd_mux_herdr::HerdrMux::with_bin(&bin),
            std::time::Duration::from_millis(300),
        );
        assert!(!answered, "a silent herdr is not a running one");
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_a_timeout_is_classified_as_slow() {
        let slow = super::probe_error(shprd_mux_hc::Error::Timeout {
            method: "core.version".into(),
            after: std::time::Duration::from_secs(1),
        });
        assert!(slow.timed_out);
        let refused = super::probe_error(shprd_mux_hc::Error::Unreachable("no core".into()));
        assert!(!refused.timed_out);
    }

    #[test]
    fn a_desktop_probe_that_times_out_once_still_uses_the_desktop() {
        let mut asked = Vec::new();
        let answer = super::ask_core_with_retry(|timeout| {
            asked.push(timeout);
            if asked.len() == 1 {
                Err(super::ProbeError {
                    timed_out: true,
                    message: "timed out".to_string(),
                })
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
        // A core that keeps timing out is asked twice, then reported.
        let mut calls = 0;
        let failed = super::ask_core_with_retry(|_| {
            calls += 1;
            Err(super::ProbeError {
                timed_out: true,
                message: "no".to_string(),
            })
        });
        assert!(failed.is_err());
        assert_eq!(calls, 2);
        // A core that refuses outright is not waited for.
        let mut calls = 0;
        let refused = super::ask_core_with_retry(|_| {
            calls += 1;
            Err(super::ProbeError {
                timed_out: false,
                message: "refused".to_string(),
            })
        });
        assert!(refused.is_err());
        assert_eq!(calls, 1);
    }

    /// A host that does nothing: the tests below care about the keeping.
    fn inert(_: &mut Resolution) -> Box<dyn Host> {
        Box::new(shprd_mux_herdr::HerdrMux::with_bin("/usr/bin/true"))
    }

    #[test]
    fn a_service_that_found_the_desktop_resolves_once() {
        let slot = Mutex::new(None);
        let mut resolved = 0;
        for _ in 0..3 {
            let (resolution, _) = super::select_in(
                &slot,
                || {
                    resolved += 1;
                    choose(HostChoice::Auto, desktop())
                },
                inert,
            );
            assert_eq!(resolution.kind, HostKind::HerdrDesktop);
        }
        assert_eq!(
            resolved, 1,
            "later reviews and spawns reuse the first answer"
        );
    }

    #[test]
    fn a_host_that_stops_opening_is_resolved_afresh() {
        let slot = Mutex::new(None);
        let _ = super::select_in(&slot, || choose(HostChoice::Auto, desktop()), inert);
        // The desktop is lost: opening falls back, so the kept answer is dropped.
        let _ = super::select_in(
            &slot,
            || unreachable!("the kept answer is used first"),
            |resolution| {
                resolution.fell_back = true;
                inert(resolution)
            },
        );
        assert!(slot.lock().unwrap().is_none());
        let mut resolved = 0;
        let _ = super::select_in(
            &slot,
            || {
                resolved += 1;
                choose(HostChoice::Auto, desktop())
            },
            inert,
        );
        assert_eq!(resolved, 1, "it comes back and is kept again");
        assert!(slot.lock().unwrap().is_some());
    }

    #[test]
    fn a_resolution_with_no_running_host_is_never_kept() {
        let slot = Mutex::new(None);
        let mut probe = nothing();
        probe.herdr_running = false;
        let mut resolved = 0;
        for _ in 0..2 {
            let _ = super::select_in(
                &slot,
                || {
                    resolved += 1;
                    choose(HostChoice::Auto, probe.clone())
                },
                inert,
            );
        }
        assert_eq!(
            resolved, 2,
            "nothing runs, so each call asks the machine again"
        );
        assert!(slot.lock().unwrap().is_none());
    }

    /// A `herdr` stand-in that answers `workspace list` as the real CLI does: a
    /// result from a herdr server's socket (`herdr.sock`), `server_not_running`
    /// from anything else, and from no listener at all.
    fn fake_herdr(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let bin = dir.join("herdr");
        std::fs::write(
            &bin,
            r#"#!/bin/sh
case "$HERDR_SOCKET_PATH" in
  */herdr.sock) [ -S "$HERDR_SOCKET_PATH" ] && python3 -c 'import socket,sys; socket.socket(socket.AF_UNIX).connect(sys.argv[1])' "$HERDR_SOCKET_PATH" 2>/dev/null && { echo '{"id":"cli:workspace:list","result":{"type":"workspace_list","workspaces":[]}}'; exit 0; } ;;
esac
echo '{"id":"cli:workspace:list","error":{"code":"server_not_running","message":"no herdr server is running"}}'
exit 1
"#,
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        bin
    }

    #[test]
    fn herdr_running_is_true_only_for_a_live_herdr_server_socket() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_herdr(dir.path());
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let mux = |socket: &Path| {
            shprd_mux_herdr::HerdrMux::with_bin(&bin).with_environment(socket, &home)
        };

        // The desktop's socket, as an hc child sees it in HERDR_SOCKET_PATH: it
        // listens, so the old "does it accept a connection" check said running.
        let desktop = dir.path().join("desktop-core.sock");
        let _desktop = std::os::unix::net::UnixListener::bind(&desktop).unwrap();
        assert!(
            std::os::unix::net::UnixStream::connect(&desktop).is_ok(),
            "the desktop socket accepts connections"
        );
        assert!(
            !super::herdr_server_answers(mux(&desktop), std::time::Duration::from_secs(5)),
            "a listening desktop socket is not a herdr server"
        );

        // A leftover herdr.sock whose server exited.
        let tui = dir.path().join("herdr.sock");
        let listener = std::os::unix::net::UnixListener::bind(&tui).unwrap();
        assert!(
            super::herdr_server_answers(mux(&tui), std::time::Duration::from_secs(5)),
            "a live herdr server"
        );
        drop(listener);
        assert!(tui.exists());
        assert!(
            !super::herdr_server_answers(mux(&tui), std::time::Duration::from_secs(5)),
            "a stale socket file is not a server"
        );
        // And no herdr binary at all.
        let missing = shprd_mux_herdr::HerdrMux::with_bin(dir.path().join("none"));
        assert!(!super::herdr_server_answers(
            missing,
            std::time::Duration::from_secs(5)
        ));
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

/// How long a host that was not running is trusted before the service probes
/// for a running one again.
const RESELECT_AFTER: Duration = Duration::from_secs(5);

/// The adapter held, whether it was a running one, and when it was picked.
type Held = (std::sync::Arc<dyn Host>, bool, std::time::Instant);

type Picker = Box<dyn Fn() -> (Resolution, std::sync::Arc<dyn Host>) + Send + Sync>;

/// The host a long-lived service holds.
///
/// A service that came up while the Herdr window did not answer (it was in the
/// background, or not open yet) resolved to a host that is not running, and used
/// to keep that adapter for its whole life: a spawn, a review, a review again and
/// a stop then failed until it was restarted. This one keeps the adapter it was
/// given while that adapter is a running one, and asks [`select`] again (at most
/// every few seconds) whenever the one it holds is not, so the first call after a
/// desktop comes up reaches it.
pub struct Reselecting {
    pick: Picker,
    slot: Mutex<Option<Held>>,
    retry: Duration,
}

impl Reselecting {
    /// Holds `host` (whose resolution said `running`), and picks again with `pick`.
    pub fn new(
        resolution: &Resolution,
        host: std::sync::Arc<dyn Host>,
        pick: Picker,
        retry: Duration,
    ) -> Self {
        let good = resolution.running && !resolution.fell_back;
        Self {
            pick,
            slot: Mutex::new(Some((host, good, std::time::Instant::now()))),
            retry,
        }
    }

    /// The service's host: the machine's own [`select`], asked again when needed.
    pub fn of_machine(resolution: &Resolution, host: std::sync::Arc<dyn Host>) -> Self {
        let pick: Picker = Box::new(|| {
            let (resolution, host) = select();
            (resolution, std::sync::Arc::from(host))
        });
        Self::new(resolution, host, pick, RESELECT_AFTER)
    }

    /// The adapter to use for this call.
    fn current(&self) -> std::sync::Arc<dyn Host> {
        let mut slot = self.slot.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((host, good, at)) = slot.as_ref()
            && (*good || at.elapsed() < self.retry)
        {
            return host.clone();
        }
        let (resolution, host) = (self.pick)();
        let good = resolution.running && !resolution.fell_back;
        *slot = Some((host.clone(), good, std::time::Instant::now()));
        host
    }
}

macro_rules! forward {
    ($($name:ident(&self $(, $arg:ident: $ty:ty)*) -> $ret:ty;)*) => {
        $(fn $name(&self $(, $arg: $ty)*) -> $ret {
            self.current().$name($($arg),*)
        })*
    };
}

impl Host for Reselecting {
    forward! {
        capabilities(&self) -> shprd_mux::Capabilities;
        find_space(&self, name: &str) -> shprd_mux::Result<Option<shprd_mux::Space>>;
        marked_space(&self, mark: &str) -> shprd_mux::Result<Option<shprd_mux::Space>>;
        mark_space(&self, space: &shprd_mux::SpaceId, mark: &str) -> shprd_mux::Result<()>;
        rename_space(&self, space: &shprd_mux::SpaceId, name: &str) -> shprd_mux::Result<()>;
        spaces_in_folder(&self, folder: &Path) -> shprd_mux::Result<Vec<shprd_mux::Space>>;
        list_coordinators(&self) -> shprd_mux::Result<Vec<shprd_mux::LiveAgent>>;
        is_foreign_agent(&self, pane: &shprd_mux::PaneId) -> bool;
        foreign_agent_tabs(&self) -> Vec<shprd_mux::TabId>;
        list_spaces(&self) -> shprd_mux::Result<Vec<shprd_mux::Space>>;
        ensure_space(&self, name: &str, cwd: &Path) -> shprd_mux::Result<shprd_mux::Space>;
        create_space(&self, name: &str, cwd: &Path) -> shprd_mux::Result<shprd_mux::Space>;
        close_space(&self, space: &shprd_mux::SpaceId) -> shprd_mux::Result<()>;
        focus_space(&self, space: &shprd_mux::SpaceId) -> shprd_mux::Result<()>;
        list_tabs(&self, space: &shprd_mux::SpaceId) -> shprd_mux::Result<Vec<shprd_mux::Tab>>;
        rename_tab(&self, tab: &shprd_mux::TabId, title: &str) -> shprd_mux::Result<()>;
        focus_tab(&self, tab: &shprd_mux::TabId) -> shprd_mux::Result<()>;
        close_tab(&self, tab: &shprd_mux::TabId) -> shprd_mux::Result<()>;
        spawn(
            &self,
            space: &shprd_mux::SpaceId,
            title: &str,
            argv: &[String],
            env: &[(String, Option<String>)],
            cwd: &Path
        ) -> shprd_mux::Result<shprd_mux::Tab>;
        spawn_agent(&self, launch: &shprd_mux::AgentLaunch<'_>) -> shprd_mux::Result<shprd_mux::Tab>;
        list_agents(&self) -> shprd_mux::Result<Vec<shprd_mux::LiveAgent>>;
        split(
            &self,
            pane: &shprd_mux::PaneId,
            direction: shprd_mux::Direction,
            ratio: f32,
            cwd: &Path
        ) -> shprd_mux::Result<shprd_mux::PaneId>;
        close_pane(&self, pane: &shprd_mux::PaneId) -> shprd_mux::Result<()>;
        tab_names(&self, space: &shprd_mux::SpaceId) -> shprd_mux::Result<Vec<(shprd_mux::TabId, String)>>;
        show_chat(&self, pane: &shprd_mux::PaneId, chat_space: &str) -> shprd_mux::Result<()>;
        write(&self, pane: &shprd_mux::PaneId, text: &str, submit: bool) -> shprd_mux::Result<()>;
        keys(&self, pane: &shprd_mux::PaneId, keys: &[shprd_mux::Key]) -> shprd_mux::Result<()>;
        read(&self, pane: &shprd_mux::PaneId, lines: usize) -> shprd_mux::Result<String>;
        wait(
            &self,
            pane: &shprd_mux::PaneId,
            wanted: &shprd_mux::Waited,
            timeout: Duration
        ) -> shprd_mux::Result<shprd_mux::Outcome>;
        kill(&self, pane: &shprd_mux::PaneId) -> shprd_mux::Result<()>;
        agent_state(&self, pane: &shprd_mux::PaneId) -> shprd_mux::Result<shprd_mux::AgentState>;
        exit_of(&self, pane: &shprd_mux::PaneId) -> shprd_mux::Result<Option<shprd_mux::Outcome>>;
        pane_process_group(&self, pane: &shprd_mux::PaneId) -> shprd_mux::Result<Option<u32>>;
        subscribe(&self, timeout: Duration) -> shprd_mux::Result<Vec<shprd_mux::MuxEvent>>;
        doc(&self, space: &shprd_mux::SpaceId, name: &str, body: &str) -> shprd_mux::Result<String>;
        exec(&self, argv: &[String], cwd: &Path) -> shprd_mux::Result<shprd_mux::Outcome>;
    }
}

#[cfg(test)]
mod reselect_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use shprd_mux::fake::FakeMux;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn res(running: bool) -> Resolution {
        Resolution {
            setting: HostChoice::default(),
            kind: HostKind::Herdr,
            why: String::new(),
            fell_back: !running,
            running,
            probe: Probe::default(),
        }
    }

    #[test]
    fn a_hostless_service_probes_again_and_a_running_host_is_kept() {
        let picks = Arc::new(AtomicU32::new(0));
        let seen = picks.clone();
        let pick: Picker = Box::new(move || {
            seen.fetch_add(1, Ordering::Relaxed);
            (
                res(true),
                Arc::new(FakeMux::new(shprd_mux::Capabilities::minimal())) as Arc<dyn Host>,
            )
        });
        // Started hostless: the first call after the retry window picks again.
        let held = Reselecting::new(
            &res(false),
            Arc::new(FakeMux::new(shprd_mux::Capabilities::minimal())),
            pick,
            Duration::ZERO,
        );
        let _ = held.capabilities();
        assert_eq!(picks.load(Ordering::Relaxed), 1, "probed for a host again");
        let _ = held.capabilities();
        let _ = held.capabilities();
        assert_eq!(picks.load(Ordering::Relaxed), 1, "a running host is kept");
    }

    #[test]
    fn a_host_that_was_running_at_start_is_never_probed_again() {
        let picks = Arc::new(AtomicU32::new(0));
        let seen = picks.clone();
        let pick: Picker = Box::new(move || {
            seen.fetch_add(1, Ordering::Relaxed);
            (
                res(true),
                Arc::new(FakeMux::new(shprd_mux::Capabilities::minimal())) as Arc<dyn Host>,
            )
        });
        let held = Reselecting::new(
            &res(true),
            Arc::new(FakeMux::new(shprd_mux::Capabilities::minimal())),
            pick,
            Duration::ZERO,
        );
        let _ = held.capabilities();
        assert_eq!(picks.load(Ordering::Relaxed), 0);
    }
}
