//! The [`shprd_mux::Host`] every shprd verb drives on Herdr desktop.
//!
//! Ported from Shepherdr.
//!
//! - Source: <https://github.com/eliasstravik/shepherdr>
//! - Path: `service/hub.ts` (terminal input sent with no `caller`, the one link
//!   that is spawned and reconnected, the request ids and per-call timeout),
//!   `service/spaces.ts` (a space made through the window, and the labels a
//!   space's terminals carry), `service/terminal.ts` (the wait-then-send
//!   discipline)
//! - Commit: `a3f327a` (tag `v3.1.34`)
//! - License: MIT — Copyright (c) 2026 Elias Stravik
//!
//! Herdr desktop is two programs, and this adapter is the one place that knows
//! it: **herdr-core** (`hc`) owns the terminals and their status, and the
//! **window** owns the layout — the spaces, the tabs, and which pane shows which
//! terminal. `hc` has no verb that makes a space; the window's own command line
//! (`herdr-app do …`, M7ef §2.12) is the only way, and the M7ef spike records
//! exactly that split: `hc run` starts a program, `herdr-app do space.new`
//! makes the space it then appears in.
//!
//! | verb | how |
//! |---|---|
//! | [`Host::find_space`], [`Host::list_spaces`], [`Host::list_tabs`] | the `herdr-app` document, read with `doc.get` |
//! | [`Host::create_space`], [`Host::close_space`], [`Host::focus_space`] | the window: `space.new`, `space.close`, `space.select` |
//! | [`Host::spawn`] | `terminal.run` with the `herdr.space` / `herdr.tab` labels, then the window's `tab.rename` |
//! | [`Host::rename_tab`], [`Host::focus_tab`], [`Host::close_tab`] | the window: `tab.rename`, `tab.select`, `tab.close` |
//! | [`Host::split`], [`Host::close_pane`] | the window: `pane.split`, `pane.close`, the pane found by its terminal in the layout |
//! | [`Host::write`], [`Host::keys`], [`Host::read`], [`Host::wait`], [`Host::kill`] | `terminal.send`, `terminal.keys`, `terminal.read`, `terminal.wait`, `terminal.kill` |
//! | [`Host::subscribe`] | the fold, diffed: the client's event stream never stops reading |
//! | [`Host::doc`], [`Host::exec`] | `doc.set` (compare-and-set) and `machine.exec` |
//!
//! # Spaces and tabs are found by their labels, and made by the window
//!
//! A task's space and each of its tabs are **found by label** in the `herdr-app`
//! document: `create_space` asks the window for a space named as shprd names it,
//! and `spawn` starts a terminal labelled `herdr.space=<space id>` /
//! `herdr.tab=<tab label>`, which is how the window puts a terminal in the tab a
//! task works in (M7ef §2.8, verified live in M10.1's test). A terminal started
//! with a `herdr.tab` label the window does not know makes that tab, so a spawn
//! never needs a second call to place its program — only one to give the tab the
//! name a person reads.
//!
//! # What this host cannot do, and says so
//!
//! [`Capabilities`] reports the truth at construction: `pane_grants` and
//! `human_ask` are always `false` (herdr-core has neither on 0.1.21), and the
//! layout verbs — `split`, `close_pane`, `close_space`, and panes' ratio — are
//! `false` when [no window answers](Window::attached), because without a window
//! there is nothing to make a space or a tab in. Every verb without its flag
//! answers [`MuxError::Unsupported`] rather than pretending.
//!
//! # Threading
//!
//! [`shprd_mux::Host`] is synchronous and herdr-core's client is not, so the host
//! owns a small tokio runtime of its own and blocks on it per call. That is
//! legal from any thread that is not itself running a runtime, which is where
//! every caller of this trait lives: the verbs, and the service's tick, which
//! runs its passes inside `spawn_blocking`. Dropping the host drops the runtime
//! and the `hc connect` child with it.
//!
//! When a core is not running, asking it anything *starts* it (M7ef §2.3: only
//! Windows is documented as never starting one), which is the same thing `hc ls`
//! does and what a person on Herdr desktop already has.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Map, Value, json};
use shprd_mux::{
    AgentState, Capabilities, Direction, Host, Key, MuxError, MuxEvent, Outcome, PaneId, Result,
    Space, SpaceId, Tab, TabId, Waited,
};
use tokio::sync::watch;

use crate::client::{Endpoint, HcClient};
use crate::error::Error;
use crate::events::State;
use crate::model::{self, Terminal};
use crate::placement::{self, Placement};

/// How long one window command may take before the call gives up on it.
///
/// Every `herdr-app do …` call measured in M7ef §2.12 answered in milliseconds;
/// this is the bound that keeps a window that has stopped drawing from hanging a
/// verb for ever.
pub const WINDOW_TIMEOUT: Duration = Duration::from_secs(20);

/// How long [`Host::write`] waits for `terminal.send`.
///
/// The core holds a draft for about 2.2 s by design and refuses a `blocked`
/// program, so the trait's 15 s default is not enough (M7ef §2.3, §2.10's 120 s).
const SEND_TIMEOUT: Duration = Duration::from_secs(120);

/// The margin a blocking core call is given over what it waits for.
const CALL_MARGIN: Duration = Duration::from_secs(15);

/// How long one core-side wait runs before the screen is looked at again.
///
/// `terminal.wait --output` only matches output printed after the call starts,
/// so a wait for output is cut into slices and the screen is read between them.
const WAIT_SLICE: Duration = Duration::from_secs(2);

/// How long `machine.exec` may take: the core's own ceiling for it is 600 s and
/// its default 60 s, and the trait's `exec` names no timeout of its own.
const EXEC_TIMEOUT: Duration = Duration::from_secs(120);

/// How long a freshly started terminal's tab may take to appear in the layout
/// before [`Host::spawn`] stops asking the window to name it.
///
/// The window draws a labelled terminal about two seconds after it starts
/// (M7ef §2.8), so this is a handful of times the measured delay.
const PLACE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long [`Host::spawn`] waits between two attempts to name its tab.
const PLACE_POLL: Duration = Duration::from_millis(100);

/// How many times a compare-and-set write is retried after a `conflict`.
const DOC_RETRIES: usize = 5;

/// How many screen lines a `wait` for output reads back once it matched.
const SCREEN_LINES: usize = 200;

/// What one herdr-core or window call answers, in the client's own error type.
///
/// The adapter's own verbs answer [`shprd_mux::Result`]; this is the layer under
/// them, where a refusal keeps the core's `kind` long enough for the verb to
/// decide whether it is "not found", "forbidden" or a plain failure.
type CoreResult<T> = crate::error::Result<T>;

/// The `herdr-app` command line: the window's half of the host.
///
/// The client's half is [`HcClient`], and this is the other one — spaces, tabs
/// and panes, which the window owns and `hc` does not. Its environment is
/// deliberate rather than inherited: the variables `hc` resolves a session from
/// (and the pane variables that name a caller) are removed and then the ones the
/// endpoint names are set, so a call can never address a different window or
/// terminal than the one the endpoint chose.
#[derive(Debug, Clone)]
pub struct Window {
    /// The binary to run (`herdr-app`; `$SHPRD_HERDR_APP` overrides it).
    binary: PathBuf,
    /// The environment the child gets, as set/removed pairs.
    environment: Vec<(String, Option<String>)>,
    /// How long one call may take.
    timeout: Duration,
}

impl Default for Window {
    fn default() -> Self {
        Self::new()
    }
}

impl Window {
    /// The `herdr-app` on `PATH`, inheriting this process's environment.
    pub fn new() -> Self {
        Self {
            binary: PathBuf::from("herdr-app"),
            environment: Vec::new(),
            timeout: WINDOW_TIMEOUT,
        }
    }

    /// A window command line with a specific binary.
    pub fn with_binary(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
            ..Self::new()
        }
    }

    /// The same command line running another binary, environment and all.
    pub fn with_bin(mut self, binary: impl Into<PathBuf>) -> Self {
        self.binary = binary.into();
        self
    }

    /// A window command line pointed at the same core and directories as
    /// `endpoint`.
    ///
    /// The window itself resolves its core from `HC_SESSION` (and its state from
    /// the XDG directories), so it must be told the same ones `hc` was, or the
    /// layout it would change belongs to another core.
    pub fn from_endpoint(endpoint: &Endpoint) -> Self {
        let mut window = Self::new();
        for variable in Endpoint::DROPPED {
            window.environment.push((variable.to_string(), None));
        }
        for (key, value) in endpoint.environment() {
            window.environment.push((key.to_string(), Some(value)));
        }
        for (key, value) in endpoint.directories.clone() {
            window.environment.push((key, Some(value)));
        }
        window
    }

    /// Set one variable on the window's environment.
    pub fn with_variable(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.environment.push((key.into(), Some(value.into())));
        self
    }

    /// How long one call may take.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The binary this command line runs.
    pub fn binary(&self) -> &Path {
        &self.binary
    }

    /// `herdr-app status`: what the window is showing, or why it did not answer.
    pub fn status(&self) -> CoreResult<Value> {
        self.call_value(&["status".to_string()])
    }

    /// Whether a window answers at all.
    ///
    /// This is the one fact [`Capabilities`] is built from, and it is a fact
    /// about the host rather than a verb tried to find out what the host can do:
    /// a window that is not running has no layout to change.
    pub fn attached(&self) -> bool {
        self.status().is_ok()
    }

    /// Run one window command (`herdr-app do <command> --arg value …`).
    ///
    /// Returns the JSON object the command printed. A command that refuses says
    /// so in its own words, which come back as [`Error::Output`] rather than
    /// being guessed at.
    pub fn call(&self, command: &str, args: &[(&str, &str)]) -> CoreResult<Value> {
        let mut argv: Vec<String> = vec!["do".to_string(), command.to_string()];
        for (flag, value) in args {
            argv.push((*flag).to_string());
            argv.push((*value).to_string());
        }
        self.call_value(&argv)
    }

    /// Run `herdr-app <argv>` and read the JSON object it printed.
    fn call_value(&self, argv: &[String]) -> CoreResult<Value> {
        let output = self.run(argv)?;
        let trimmed = output.trim();
        if trimmed.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(trimmed).map_err(|error| {
            Error::Window(format!(
                "`{} {}` did not print JSON ({error}): {trimmed}",
                self.binary.display(),
                argv.join(" ")
            ))
        })
    }

    /// Run `herdr-app <argv>` and answer its stdout, bounded by the timeout.
    ///
    /// The child is reaped on its own thread, because a window that stops
    /// drawing would otherwise block the verb for ever; a call that times out
    /// leaves that thread to collect the process rather than leaking a zombie.
    fn run(&self, argv: &[String]) -> CoreResult<String> {
        let mut command = Command::new(&self.binary);
        command
            .args(argv)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in &self.environment {
            match value {
                Some(value) => command.env(key, value),
                None => command.env_remove(key),
            };
        }
        let child = command.spawn().map_err(|error| {
            Error::Window(format!(
                "cannot start `{}`: {error} (is Herdr desktop installed, and `herdr-app` on PATH?)",
                self.binary.display()
            ))
        })?;
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(child.wait_with_output());
        });
        let method = format!("{} {}", self.binary.display(), argv.join(" "));
        match receiver.recv_timeout(self.timeout) {
            Ok(Ok(output)) if output.status.success() => {
                Ok(String::from_utf8_lossy(&output.stdout).into_owned())
            }
            Ok(Ok(output)) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let stdout = String::from_utf8_lossy(&output.stdout);
                let detail = if stderr.trim().is_empty() {
                    stdout.trim().to_string()
                } else {
                    stderr.trim().to_string()
                };
                Err(Error::Refused {
                    method,
                    kind: "window".to_string(),
                    message: format!("({}) {detail}", output.status),
                    hint: None,
                })
            }
            Ok(Err(error)) => Err(Error::Window(format!("cannot read `{method}`: {error}"))),
            Err(_) => Err(Error::Timeout {
                method,
                after: self.timeout,
            }),
        }
    }
}

/// The shprd host for Herdr desktop: `hc` for terminals, the window for layout.
///
/// Cloning is cheap and shares one connection, one runtime and one fold, so the
/// scheduler and the event pump can hold the same host. The last clone to go
/// closes the `hc connect` child.
#[derive(Clone)]
pub struct HcHost {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for HcHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HcHost")
            .field("window", &self.inner.window.binary())
            .field("window_attached", &self.inner.capabilities.close_space)
            .finish_non_exhaustive()
    }
}

/// What every clone of a host shares.
struct Inner {
    /// The runtime the client's tasks run on. Declared first so it outlives the
    /// client it drives while `Inner` is dropped.
    runtime: tokio::runtime::Runtime,
    /// The link to herdr-core and its fold.
    client: HcClient,
    /// The window's command line.
    window: Window,
    /// What the host can do, decided once at construction.
    capabilities: Capabilities,
    /// The fold's published states; `subscribe` waits on it.
    changes: Mutex<watch::Receiver<State>>,
    /// What `subscribe` has already reported, so it reports each change once.
    seen: Mutex<Seen>,
    /// The `herdr-app` document's value as last read, for the pane ids the
    /// parsed layout does not keep.
    layout: Mutex<Value>,
}

/// The last state `subscribe` reported, so a diff is never reported twice.
#[derive(Debug, Default)]
struct Seen {
    /// Every terminal by bare address: its shprd state and whether it was live.
    terminals: BTreeMap<String, (AgentState, bool)>,
    /// Every space id the layout held.
    spaces: BTreeSet<String>,
}

impl HcHost {
    /// Connect to the core `endpoint` names, and to `window` for the layout.
    ///
    /// The window is asked whether it answers, once, because [`Capabilities`]
    /// has to say before any verb is tried whether a space or a tab can be made.
    /// A window that does not answer is not an error: the terminals still work
    /// and are simply invisible, which is what the flags then report.
    ///
    /// Panics if called from inside a tokio runtime: the host owns one, and a
    /// runtime cannot be started from an async context. Every caller in shprd
    /// is synchronous.
    pub fn connect(endpoint: Endpoint, window: Window) -> Result<Self> {
        let attached = window.attached();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|error| {
                MuxError::command(
                    "starting the runtime herdr-core talks on",
                    error.to_string(),
                )
            })?;
        let client = runtime
            .block_on(HcClient::connect(endpoint))
            .map_err(|error| MuxError::command("connecting to herdr-core", error.to_string()))?;
        let mut changes = client.changes();
        // The snapshot the subscription folded is the baseline: `subscribe`
        // reports what happens *after* the host exists, not the whole host.
        changes.borrow_and_update();
        let capabilities = Capabilities {
            spawn: true,
            split: attached,
            // The window's `pane.split` has no ratio.
            split_ratio: false,
            close_pane: attached,
            // herdr-core has neither on 0.1.21: a terminal's program is started
            // with the permissions it already has, and nothing in a terminal can
            // hold for a person's answer.
            pane_grants: false,
            human_ask: false,
            keys: true,
            // `terminal.wait` answers in one call.
            wait: true,
            kill: true,
            // The client folds the stream whether or not a caller is reading,
            // so nothing that happened while nobody called `subscribe` is lost.
            resumable_events: true,
            doc: true,
            exec: true,
            agent_state: true,
            close_space: attached,
            list_spaces: true,
        };
        let host = Self {
            inner: Arc::new(Inner {
                runtime,
                client,
                window,
                capabilities,
                changes: Mutex::new(changes),
                seen: Mutex::new(Seen::default()),
                layout: Mutex::new(Value::Null),
            }),
        };
        // The baseline the first `subscribe` diffs against, read now so a
        // terminal that was already there is not reported as new.
        host.refresh_layout()?;
        host.reset_seen()?;
        Ok(host)
    }

    /// The version of the core `endpoint` names, for choosing a host.
    ///
    /// This is the probe the wiring edge uses: a core that answers `hello` with a
    /// version this crate accepts is a core shprd can drive. An endpoint that
    /// names no running core *starts* one, which is what `hc` itself does
    /// (M7ef §2.3) and what a person on Herdr desktop already has.
    pub fn core_version(endpoint: &Endpoint, timeout: Duration) -> CoreResult<String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| {
                Error::Output(format!(
                    "cannot start the runtime herdr-core needs: {error}"
                ))
            })?;
        let endpoint = endpoint.clone().with_timeout(timeout);
        runtime.block_on(async move {
            let client = HcClient::connect(endpoint).await?;
            let hello = client.hello().await?;
            // The client answers the core's own error type here; a caller
            // choosing a host wants the message, and a version that is too old
            // says so in it.
            client.close().await;
            Ok(hello.version)
        })
    }

    /// What this host's window command line is, for a caller that wants to ask
    /// the window something shprd does not model.
    pub fn window(&self) -> &Window {
        &self.inner.window
    }

    /// Whether the window answered when the host was built, and so whether the
    /// layout verbs are promised.
    pub fn window_attached(&self) -> bool {
        self.inner.capabilities.close_space
    }

    /// One call, as the client's own error.
    fn core(&self, method: &str, params: Value) -> CoreResult<Value> {
        self.inner
            .runtime
            .block_on(self.inner.client.call(method, params))
    }

    /// One call with a timeout, as the client's own error.
    fn core_with(&self, method: &str, params: Value, timeout: Duration) -> CoreResult<Value> {
        self.inner
            .runtime
            .block_on(self.inner.client.call_with(method, params, timeout))
    }

    /// One call, as the trait's error, for a verb that cannot be "not found".
    fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.core(method, params)
            .map_err(|error| core_error(method, error))
    }

    /// One call that finds `what` by `id`, mapping the core's `not_found`.
    fn call_object(
        &self,
        method: &str,
        params: Value,
        what: &'static str,
        id: &str,
    ) -> Result<Value> {
        self.call_object_with(method, params, None, what, id)
    }

    /// [`HcHost::call_object`] with a timeout of its own, for a verb that blocks
    /// in the core (`terminal.wait`, `machine.exec`): the connection's default
    /// is shorter than those calls answer in.
    fn call_object_with(
        &self,
        method: &str,
        params: Value,
        timeout: Option<Duration>,
        what: &'static str,
        id: &str,
    ) -> Result<Value> {
        let outcome = match timeout {
            Some(timeout) => self.core_with(method, params, timeout),
            None => self.core(method, params),
        };
        outcome.map_err(|error| {
            if error.refusal_kind() == Some("not_found") {
                MuxError::not_found(what, id)
            } else {
                core_error(method, error)
            }
        })
    }

    /// Read the `herdr-app` document, take it into the fold, and answer it.
    fn refresh_layout(&self) -> Result<State> {
        let value = self
            .inner
            .runtime
            .block_on(self.inner.client.layout())
            .map_err(|error| core_error("doc.get", error))?;
        if let Ok(mut layout) = self.inner.layout.lock() {
            *layout = value;
        }
        Ok(self.inner.client.state())
    }

    /// The layout document as last read.
    fn layout(&self) -> Value {
        self.inner
            .layout
            .lock()
            .map(|layout| layout.clone())
            .unwrap_or(Value::Null)
    }

    /// Every space the layout holds, as the full model.
    fn spaces(&self) -> Result<Vec<placement::Space>> {
        Ok(self.refresh_layout()?.spaces)
    }

    /// The space with `id`, from a fresh read.
    fn space_with(&self, id: &str, what: &'static str) -> Result<placement::Space> {
        let spaces = self.spaces()?;
        placement::find_space(&spaces, id)
            .cloned()
            .ok_or_else(|| MuxError::not_found(what, id))
    }

    /// The tab with `id`, from a fresh read, and the space it sits in.
    fn tab_with(&self, id: &str) -> Result<(placement::Space, placement::Tab)> {
        let wanted = model::bare(id);
        for space in self.spaces()? {
            if let Some(tab) = space.tabs.iter().find(|tab| model::bare(&tab.id) == wanted) {
                return Ok((space.clone(), tab.clone()));
            }
        }
        Err(MuxError::not_found("tab", id))
    }

    /// The window's answer, or the trait's "this host cannot do that" when no
    /// window is attached.
    fn window_call(
        &self,
        command: &str,
        args: &[(&str, &str)],
        what: &'static str,
    ) -> Result<Value> {
        if !self.window_attached() {
            return Err(MuxError::unsupported(what));
        }
        self.inner
            .window
            .call(command, args)
            .map_err(|error| window_error(command, error))
    }

    /// The terminal `address`, from the core rather than the fold: a verb that
    /// acts on one terminal wants the current record, not the last event.
    fn terminal(&self, pane: &PaneId) -> Result<Terminal> {
        let value = self.call_object(
            "terminal.get",
            json!({"terminal": pane.as_str()}),
            "pane",
            pane.as_str(),
        )?;
        serde_json::from_value(value).map_err(|error| {
            MuxError::command(
                format!("reading terminal {}", pane.as_str()),
                error.to_string(),
            )
        })
    }

    /// Record the current fold as already reported.
    fn reset_seen(&self) -> Result<()> {
        let state = self.inner.client.state();
        let mut seen = self
            .inner
            .seen
            .lock()
            .map_err(|_| MuxError::command("reading the event fold", "it is poisoned"))?;
        *seen = Seen::default();
        for terminal in state.terminals() {
            seen.terminals.insert(
                terminal.bare_address().to_string(),
                (reported_state(terminal), terminal.is_live()),
            );
        }
        for space in &state.spaces {
            seen.spaces.insert(space.id.clone());
        }
        Ok(())
    }

    /// Ask the window to give the tab labelled `label` the name `title`.
    ///
    /// The window draws a terminal's tab a couple of seconds after the terminal
    /// starts (M7ef §2.8), so this retries until the tab exists. A window that
    /// never catches up leaves the tab named by its number, which is what a
    /// person sees for any unnamed tab: the terminal is placed either way, so
    /// this is not worth failing a spawn over.
    fn name_spawned_tab(&self, label: &str, title: &str) {
        if !self.window_attached() {
            return;
        }
        let started = std::time::Instant::now();
        loop {
            if self
                .inner
                .window
                .call("tab.rename", &[("--id", label), ("--name", title)])
                .is_ok()
            {
                return;
            }
            if started.elapsed() >= PLACE_TIMEOUT {
                tracing::debug!(label, title, "the window never showed the spawned tab");
                return;
            }
            std::thread::sleep(PLACE_POLL);
        }
    }

    /// The pane (layout block) that shows terminal `address`, for the verbs the
    /// window addresses by pane.
    fn pane_of(&self, address: &str) -> Option<String> {
        let wanted = model::bare(address);
        fn walk(node: &Value, wanted: &str) -> Option<String> {
            if node
                .get("content")
                .and_then(|content| content.get("terminal"))
                .and_then(Value::as_str)
                .map(model::bare)
                == Some(wanted)
            {
                return node.get("id").and_then(Value::as_str).map(str::to_string);
            }
            node.get("children")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .find_map(|child| walk(child, wanted))
        }
        self.layout()
            .get("spaces")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .flat_map(|space| {
                space
                    .get("tabs")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
            })
            .find_map(|tab| tab.get("root").and_then(|root| walk(root, wanted)))
    }
}

impl Host for HcHost {
    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities
    }

    /// The space shprd calls `name`: matched by the id the window gave it (a
    /// label-made space's id *is* the label) or by the name a person sees.
    fn find_space(&self, name: &str) -> Result<Option<Space>> {
        let spaces = self.spaces()?;
        Ok(match_space(&spaces, name).map(space_of))
    }

    fn list_spaces(&self) -> Result<Vec<Space>> {
        Ok(self.spaces()?.iter().map(space_of).collect())
    }

    /// A space the window makes: named as shprd names it, with its first pane in
    /// `cwd`.
    ///
    /// `hc` has no verb for this — a space is the window's own object, and
    /// `herdr-app do space.new` is how Shepherdr makes one too (`service/spaces.ts`
    /// in v3.1.34). The window's answer names the space, the tab and the
    /// terminal it made, which is what a person then sees on the sidebar.
    fn create_space(&self, name: &str, cwd: &Path) -> Result<Space> {
        let folder = cwd.display().to_string();
        let made = self.window_call(
            "space.new",
            &[("--name", name), ("--folder", folder.as_str())],
            "making a space",
        )?;
        let id = made
            .get("space")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                MuxError::command(
                    "making a space",
                    format!("the window answered without a space: {made}"),
                )
            })?
            .to_string();
        Ok(Space {
            id: SpaceId::new(id),
            name: name.to_string(),
        })
    }

    /// Close a space, and with it every terminal in it.
    ///
    /// A space the layout no longer holds is [`MuxError::NotFound`] — a landing
    /// that races a person closing the space has nothing left to do.
    fn close_space(&self, space: &SpaceId) -> Result<()> {
        let found = self.space_with(space.as_str(), "space")?;
        self.window_call(
            "space.close",
            &[("--id", &found.id), ("--confirmed", "true")],
            "closing a space",
        )?;
        Ok(())
    }

    fn focus_space(&self, space: &SpaceId) -> Result<()> {
        let found = self.space_with(space.as_str(), "space")?;
        self.window_call("space.select", &[("--id", &found.id)], "showing a space")?;
        Ok(())
    }

    /// The tabs of a space, from the layout.
    ///
    /// Only tabs that show a terminal are listed: a tab of mods alone has no
    /// pane a program runs in, and every consumer of this trait addresses a tab
    /// by the program it started.
    fn list_tabs(&self, space: &SpaceId) -> Result<Vec<Tab>> {
        let found = self.space_with(space.as_str(), "space")?;
        Ok(found
            .tabs
            .iter()
            .filter_map(|tab| {
                let root = tab.terminals.first()?;
                Some(Tab {
                    id: TabId::new(tab.id.clone()),
                    title: tab_title(tab),
                    root_pane: PaneId::new(root.clone()),
                })
            })
            .collect())
    }

    fn rename_tab(&self, tab: &TabId, title: &str) -> Result<()> {
        let (_, found) = self.tab_with(tab.as_str())?;
        self.window_call(
            "tab.rename",
            &[("--id", &found.id), ("--name", title)],
            "renaming a tab",
        )?;
        Ok(())
    }

    fn focus_tab(&self, tab: &TabId) -> Result<()> {
        let (_, found) = self.tab_with(tab.as_str())?;
        self.window_call("tab.select", &[("--id", &found.id)], "showing a tab")?;
        Ok(())
    }

    fn close_tab(&self, tab: &TabId) -> Result<()> {
        let (_, found) = self.tab_with(tab.as_str())?;
        self.window_call(
            "tab.close",
            &[("--id", &found.id), ("--confirmed", "true")],
            "closing a tab",
        )?;
        Ok(())
    }

    /// Start `argv` as a terminal labelled into `space`'s tab `title`.
    ///
    /// One `terminal.run` makes the terminal and places it: the window puts a
    /// terminal carrying `herdr.space` / `herdr.tab` into that space, and makes
    /// the tab when it does not have one yet (M7ef §2.8). Naming the tab is the
    /// window's business and follows as its own call, because `terminal.run` has
    /// no field a person's tab name would fit in.
    fn spawn(
        &self,
        space: &SpaceId,
        title: &str,
        argv: &[String],
        env: &[(String, Option<String>)],
        cwd: &Path,
    ) -> Result<Tab> {
        let label = tab_label(title);
        let placement = Placement::new(space.as_str(), &label);
        let launch = launch_argv(argv, env);
        let mut params = json!({
            "argv": launch.argv,
            "cwd": cwd.display().to_string(),
            "labels": placement.labels(),
            "keep": false,
        });
        if !launch.variables.is_empty() {
            params["env"] = Value::Object(launch.variables);
        }
        let started = self.call_object("terminal.run", params, "terminal", title)?;
        let address = started
            .get("address")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                MuxError::command(
                    "starting a terminal",
                    format!("herdr-core answered without an address: {started}"),
                )
            })?
            .to_string();
        self.name_spawned_tab(&label, title);
        Ok(Tab {
            id: TabId::new(label),
            title: title.to_string(),
            root_pane: PaneId::new(address),
        })
    }

    /// Split the pane showing `pane` with the window's `pane.split`.
    ///
    /// The trait addresses a pane by the terminal it runs; the window addresses
    /// it by its layout id, so the two are joined through the layout document
    /// and the new pane is read back the same way — the window answers with the
    /// pane it made, not the terminal in it.
    fn split(
        &self,
        pane: &PaneId,
        direction: Direction,
        _ratio: f32,
        cwd: &Path,
    ) -> Result<PaneId> {
        if !self.window_attached() {
            return Err(MuxError::unsupported("splitting a pane"));
        }
        self.refresh_layout()?;
        let target = self
            .pane_of(pane.as_str())
            .ok_or_else(|| MuxError::not_found("pane", pane.as_str()))?;
        let way = match direction {
            Direction::Right => "right",
            Direction::Down => "down",
        };
        let folder = cwd.display().to_string();
        let made = self.window_call(
            "pane.split",
            &[
                ("--direction", way),
                ("--pane", &target),
                ("--folder", folder.as_str()),
            ],
            "splitting a pane",
        )?;
        let made = made
            .get("pane")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                MuxError::command(
                    "splitting a pane",
                    format!("the window answered without a pane: {made}"),
                )
            })?;
        // The window's layout is written after the pane exists, so the new
        // pane's terminal is read back rather than guessed.
        let started = std::time::Instant::now();
        loop {
            self.refresh_layout()?;
            if let Some(address) = terminal_of_pane(&self.layout(), &made) {
                return Ok(PaneId::new(address));
            }
            if started.elapsed() >= PLACE_TIMEOUT {
                return Ok(PaneId::new(made));
            }
            std::thread::sleep(PLACE_POLL);
        }
    }

    fn close_pane(&self, pane: &PaneId) -> Result<()> {
        if !self.window_attached() {
            return Err(MuxError::unsupported("closing a pane"));
        }
        self.refresh_layout()?;
        let target = self
            .pane_of(pane.as_str())
            .ok_or_else(|| MuxError::not_found("pane", pane.as_str()))?;
        self.window_call("pane.close", &[("--pane", &target)], "closing a pane")?;
        Ok(())
    }

    /// Type `text` and submit it, as the person would.
    ///
    /// `terminal.send` carries no `caller`: the text must arrive as the person's
    /// own, never wrapped in a from-another-machine note (M7ef §2.10,
    /// `hub.ts:30-42`). herdr-core's send always presses enter, so a caller that
    /// wants the text left at the cursor is refused rather than lied to.
    fn write(&self, pane: &PaneId, text: &str, submit: bool) -> Result<()> {
        if !submit {
            return Err(MuxError::unsupported("typing text without submitting it"));
        }
        self.core_with(
            "terminal.send",
            json!({"terminal": pane.as_str(), "text": text}),
            SEND_TIMEOUT,
        )
        .map_err(|error| {
            if error.refusal_kind() == Some("blocked") {
                MuxError::forbidden(
                    "send",
                    format!(
                        "{error}; answer the program's dialog first, or send with `force` from \
                         the core's own CLI"
                    ),
                )
            } else {
                core_error("terminal.send", error)
            }
        })?;
        Ok(())
    }

    fn keys(&self, pane: &PaneId, keys: &[Key]) -> Result<()> {
        let words: Vec<String> = keys.iter().map(Key::word).collect();
        self.call(
            "terminal.keys",
            json!({"terminal": pane.as_str(), "keys": words}),
        )?;
        Ok(())
    }

    fn read(&self, pane: &PaneId, lines: usize) -> Result<String> {
        let result = self.call_object(
            "terminal.read",
            json!({"terminal": pane.as_str(), "lines": lines}),
            "pane",
            pane.as_str(),
        )?;
        Ok(result
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string())
    }

    /// Wait until the program ends, the screen shows the output, or the time is
    /// up — with the core doing the waiting.
    ///
    /// `terminal.wait --output` only matches output the program prints *after*
    /// the call, which a fast program has usually already printed. So the screen
    /// is read first, and re-read after each short core-side wait: an agent that
    /// printed its prompt before anybody asked still matches, and one that
    /// prints it a minute later is caught by the wait rather than by a poll.
    fn wait(&self, pane: &PaneId, wanted: &Waited, timeout: Duration) -> Result<Outcome> {
        let started = std::time::Instant::now();
        let needle = match wanted {
            Waited::Exit => None,
            Waited::Output(text) | Waited::Either(text) => Some(text.as_str()),
        };
        loop {
            if let Some(needle) = needle {
                let screen = self.read(pane, SCREEN_LINES)?;
                if screen.contains(needle) {
                    return Ok(Outcome::matched(screen));
                }
            }
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Ok(Outcome::running());
            }
            // With output to look for, the core is asked for a slice at a time so
            // the screen is looked at again: the core's own answer is "printed
            // after the call".
            let slice = match needle {
                Some(_) => remaining.min(WAIT_SLICE),
                None => remaining,
            };
            let mut params = json!({
                "terminal": pane.as_str(),
                "timeout": slice.as_secs_f64().max(0.001),
            });
            if let Some(needle) = needle {
                params["output"] = json!(needle);
            }
            let result = self.call_object_with(
                "terminal.wait",
                params,
                Some(slice + CALL_MARGIN),
                "pane",
                pane.as_str(),
            )?;
            let ended = result
                .get("ended")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if ended {
                return Ok(Outcome::exited(
                    result
                        .get("exit_code")
                        .and_then(Value::as_i64)
                        .and_then(|code| i32::try_from(code).ok()),
                ));
            }
            let reason = result
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or_default();
            // `output` means the screen has it now; the next pass reads it.
            // `timeout` means the slice ran out; the loop gives up when the
            // caller's whole wait has.
            if reason != "output" && slice == remaining {
                return Ok(Outcome::running());
            }
        }
    }

    fn kill(&self, pane: &PaneId) -> Result<()> {
        self.call_object(
            "terminal.kill",
            json!({"terminal": pane.as_str()}),
            "pane",
            pane.as_str(),
        )?;
        Ok(())
    }

    fn agent_state(&self, pane: &PaneId) -> Result<AgentState> {
        let terminal = self.terminal(pane)?;
        Ok(reported_state(&terminal))
    }

    /// Whether the terminal's program ended, from the core's own `state`.
    fn exit_of(&self, pane: &PaneId) -> Result<Option<Outcome>> {
        let terminal = self.terminal(pane)?;
        if terminal.is_live() {
            return Ok(None);
        }
        Ok(Some(Outcome::exited(
            terminal.exit_code.and_then(|code| i32::try_from(code).ok()),
        )))
    }

    /// The program in front of the terminal: the core reports its pid, which is
    /// the identity shprd needs to tell its own launch from somebody else's.
    fn pane_process_group(&self, pane: &PaneId) -> Result<Option<u32>> {
        let terminal = self.terminal(pane)?;
        Ok(terminal
            .foreground
            .and_then(|foreground| foreground.pid)
            .or(terminal.pid))
    }

    /// Wait for the fold to move, and answer what moved.
    ///
    /// herdr-core's stream is folded by the client whether or not anybody is
    /// waiting, so the events that happened during an earlier verb are still
    /// here: the diff against what was last reported is the queue.
    fn subscribe(&self, timeout: Duration) -> Result<Vec<MuxEvent>> {
        let seen_at_call = {
            let mut changes = self.inner.changes.lock().map_err(|_| {
                MuxError::command("listening for changes", "the channel is poisoned")
            })?;
            let _ = self
                .inner
                .runtime
                .block_on(async { tokio::time::timeout(timeout, changes.changed()).await });
            changes.borrow_and_update().clone()
        };
        self.diff(seen_at_call)
    }

    /// Store `body` as the document `shprd.<space>.<name>`, and answer its name.
    ///
    /// herdr-core's `doc.set` is compare-and-set: a write names the version it
    /// read, so a document the window or another shprd writes is never silently
    /// replaced (M7ef §2.7). A `conflict` means somebody else wrote in between,
    /// and the write is re-read and retried.
    fn doc(&self, space: &SpaceId, name: &str, body: &str) -> Result<String> {
        let document = format!("shprd.{}.{name}", model::bare(space.as_str()));
        for _ in 0..DOC_RETRIES {
            let current = self
                .core("doc.get", json!({"name": document}))
                .map_err(|error| core_error("doc.get", error))?;
            let version = current.get("version").and_then(Value::as_u64).unwrap_or(0);
            let write = self.core(
                "doc.set",
                json!({"name": document, "value": body, "version": version}),
            );
            match write {
                Ok(_) => return Ok(document),
                Err(error) if error.refusal_kind() == Some("conflict") => continue,
                Err(error) => return Err(core_error("doc.set", error)),
            }
        }
        Err(MuxError::command(
            "writing a document",
            format!("herdr-core kept refusing {document} as changed"),
        ))
    }

    /// Run `argv` once with `machine.exec` and answer its exit code and output.
    fn exec(&self, argv: &[String], cwd: &Path) -> Result<Outcome> {
        let result = self
            .core_with(
                "machine.exec",
                json!({"argv": argv, "cwd": cwd.display().to_string()}),
                EXEC_TIMEOUT,
            )
            .map_err(|error| core_error("machine.exec", error))?;
        let code = result
            .get("code")
            .and_then(Value::as_i64)
            .and_then(|code| i32::try_from(code).ok());
        let stdout = result
            .get("stdout")
            .and_then(Value::as_str)
            .map(decode_base64)
            .transpose()?
            .unwrap_or_default();
        Ok(Outcome {
            exited: true,
            exit_code: code,
            output: Some(stdout),
        })
    }
}

/// What changed between the last report and `now`.
impl HcHost {
    fn diff(&self, now: State) -> Result<Vec<MuxEvent>> {
        let mut seen = self
            .inner
            .seen
            .lock()
            .map_err(|_| MuxError::command("reading the event fold", "it is poisoned"))?;
        let mut events = Vec::new();
        for terminal in now.terminals() {
            let address = terminal.bare_address().to_string();
            let state = reported_state(terminal);
            let previous = seen.terminals.get(&address).cloned();
            if previous.is_none() || previous.as_ref().map(|(seen, _)| *seen) != Some(state) {
                events.push(MuxEvent::AgentState {
                    pane: PaneId::new(terminal.address.clone()),
                    state,
                });
            }
            if previous.is_some_and(|(_, live)| live) && !terminal.is_live() {
                events.push(MuxEvent::PaneExited {
                    pane: PaneId::new(terminal.address.clone()),
                });
            }
            seen.terminals.insert(address, (state, terminal.is_live()));
        }
        let gone: Vec<String> = seen
            .terminals
            .keys()
            .filter(|address| now.terminal(address).is_none())
            .cloned()
            .collect();
        for address in gone {
            seen.terminals.remove(&address);
            events.push(MuxEvent::PaneExited {
                pane: PaneId::new(address),
            });
        }
        for space in &now.spaces {
            if seen.spaces.insert(space.id.clone()) {
                events.push(MuxEvent::Space {
                    space: SpaceId::new(space.id.clone()),
                    created: true,
                });
            }
        }
        let closed: Vec<String> = seen
            .spaces
            .iter()
            .filter(|id| !now.spaces.iter().any(|space| &space.id == *id))
            .cloned()
            .collect();
        for id in closed {
            seen.spaces.remove(&id);
            events.push(MuxEvent::Space {
                space: SpaceId::new(id),
                created: false,
            });
        }
        Ok(events)
    }
}

/// What a launch's argv and environment become on herdr-core.
struct Launch {
    /// The argv the terminal runs: the caller's, or `/usr/bin/env` wrapping it
    /// when the launch must drop a variable.
    argv: Vec<String>,
    /// The variables herdr-core is given, when it can carry all of them.
    variables: Map<String, Value>,
}

/// Build the argv and environment variables one spawn sends.
///
/// herdr-core's `env` is a map of strings: it can add or change a variable and
/// has no spelling for "unset". A launch that must drop one — a parent session's
/// marker, a credential a worker may not hold (M3.5) — is therefore wrapped in
/// `/usr/bin/env`, which does exactly that and nothing else. The argv stays a
/// vector and the program is still the last element: nothing is re-parsed by a
/// shell, and a branch name or a title can never become a command.
fn launch_argv(argv: &[String], env: &[(String, Option<String>)]) -> Launch {
    let drops = env.iter().any(|(_, value)| value.is_none());
    if !drops {
        let mut variables = Map::new();
        for (key, value) in env {
            if let Some(value) = value {
                variables.insert(key.clone(), Value::String(value.clone()));
            }
        }
        return Launch {
            argv: argv.to_vec(),
            variables,
        };
    }
    let mut wrapped = vec!["/usr/bin/env".to_string()];
    for (key, value) in env {
        if value.is_none() {
            // `env -u NAME`: the one spelling for "this variable is not set".
            wrapped.push("-u".to_string());
            wrapped.push(key.clone());
        }
    }
    for (key, value) in env {
        if let Some(value) = value {
            wrapped.push(format!("{key}={value}"));
        }
    }
    if let Some((program, rest)) = argv.split_first() {
        // BSD `env` has no `--`, so the program follows the assignments: a
        // variable assignment is the only thing `env` reads as a word of its own.
        wrapped.push(program.clone());
        wrapped.extend(rest.iter().cloned());
    }
    Launch {
        argv: wrapped,
        variables: Map::new(),
    }
}

/// The state shprd reports for a terminal.
///
/// The core's own word wins; a terminal that never reported one has no opinion
/// while it runs, and is `done` once it is not. `error` is a word shprd does not
/// model — reading it as `done` would make a spawn's check believe the program
/// had exited — so it is the trait's `unknown`, the conservative answer.
fn reported_state(terminal: &Terminal) -> AgentState {
    match &terminal.status {
        Some(status) => match status.state.get() {
            model::AgentState::Idle => AgentState::Idle,
            model::AgentState::Working => AgentState::Working,
            model::AgentState::Blocked => AgentState::Blocked,
            model::AgentState::Done => AgentState::Done,
            model::AgentState::Error | model::AgentState::Other(_) => AgentState::Unknown,
        },
        None if !terminal.is_live() => AgentState::Done,
        None => AgentState::Unknown,
    }
}

/// The space of a layout entry, named as a person sees it.
fn space_of(space: &placement::Space) -> Space {
    Space {
        id: SpaceId::new(space.id.clone()),
        name: if space.name.is_empty() {
            space.id.clone()
        } else {
            space.name.clone()
        },
    }
}

/// The tab's title as a person sees it: its name, or its id when the window
/// names it by its number.
fn tab_title(tab: &placement::Tab) -> String {
    if tab.name.is_empty() {
        tab.id.clone()
    } else {
        tab.name.clone()
    }
}

/// The space `name` matches: by id (a label-made space's id is its label), then
/// by the name a person typed, then case-insensitively.
fn match_space<'a>(spaces: &'a [placement::Space], name: &str) -> Option<&'a placement::Space> {
    let wanted = model::bare(name);
    spaces
        .iter()
        .find(|space| model::bare(&space.id) == wanted)
        .or_else(|| spaces.iter().find(|space| space.name == name))
        .or_else(|| {
            spaces
                .iter()
                .find(|space| space.name.eq_ignore_ascii_case(name))
        })
}

/// The `herdr.tab` label a spawn places its terminal under, derived from the tab
/// title.
///
/// The window makes a tab whose id is exactly this label, so the label has to be
/// stable for a title, short enough to read in a sidebar, and free of characters
/// that would confuse a document key.
pub fn tab_label(title: &str) -> String {
    let cleaned: String = title
        .chars()
        .map(|letter| {
            if letter.is_alphanumeric() || matches!(letter, '-' | '_' | '.' | '·' | ' ') {
                letter
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim();
    let capped: String = trimmed.chars().take(48).collect();
    if capped.is_empty() {
        "shprd".to_string()
    } else {
        capped
    }
}

/// The terminal a layout pane shows, from the raw document.
fn terminal_of_pane(layout: &Value, pane: &str) -> Option<String> {
    fn walk(node: &Value, wanted: &str) -> Option<String> {
        if node.get("id").and_then(Value::as_str) == Some(wanted) {
            return node
                .get("content")
                .and_then(|content| content.get("terminal"))
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        node.get("children")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find_map(|child| walk(child, wanted))
    }
    layout
        .get("spaces")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|space| {
            space
                .get("tabs")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .find_map(|tab| tab.get("root").and_then(|root| walk(root, pane)))
}

/// The trait's error for one herdr-core refusal.
fn core_error(method: &str, error: Error) -> MuxError {
    match error {
        Error::Refused {
            kind,
            message,
            hint,
            ..
        } => {
            let detail = match hint {
                Some(hint) => format!("{message} ({hint})"),
                None => message,
            };
            if kind == "blocked" {
                MuxError::forbidden(verb_of(method), detail)
            } else {
                MuxError::command(format!("herdr-core `{method}` ({kind})"), detail)
            }
        }
        other => MuxError::command(format!("herdr-core `{method}`"), other.to_string()),
    }
}

/// The window's error for one refused command.
fn window_error(command: &str, error: Error) -> MuxError {
    match error {
        Error::Refused { message, .. } => {
            MuxError::command(format!("the Herdr window's `{command}`"), message)
        }
        other => MuxError::command(format!("the Herdr window's `{command}`"), other.to_string()),
    }
}

/// The trait's verb name for a core method, for a refusal's message.
fn verb_of(method: &str) -> &'static str {
    match method {
        "terminal.send" => "send",
        "terminal.keys" => "keys",
        "terminal.run" => "spawn",
        "terminal.kill" => "kill",
        _ => "call",
    }
}

/// Decode the base64 `machine.exec` answers its output in.
///
/// A tiny decoder rather than a dependency: this is the one place in the
/// workspace that reads base64, and the alphabet is the standard one with `=`
/// padding (herdr-core's own encoder).
fn decode_base64(text: &str) -> Result<String> {
    fn value(byte: u8) -> Option<u32> {
        Some(match byte {
            b'A'..=b'Z' => u32::from(byte - b'A'),
            b'a'..=b'z' => u32::from(byte - b'a') + 26,
            b'0'..=b'9' => u32::from(byte - b'0') + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        })
    }
    let mut bytes = Vec::with_capacity(text.len() / 4 * 3);
    let mut chunk = [0u32; 4];
    let mut held = 0usize;
    let mut padding = 0usize;
    for byte in text.bytes() {
        if byte.is_ascii_whitespace() {
            continue;
        }
        if byte == b'=' {
            padding += 1;
            continue;
        }
        let Some(part) = value(byte) else {
            return Err(MuxError::command(
                "reading a command's output",
                format!("{byte:?} is not base64"),
            ));
        };
        chunk[held] = part;
        held += 1;
        if held == 4 {
            let triple = (chunk[0] << 18) | (chunk[1] << 12) | (chunk[2] << 6) | chunk[3];
            bytes.push((triple >> 16) as u8);
            bytes.push((triple >> 8) as u8);
            bytes.push(triple as u8);
            held = 0;
        }
    }
    // `=` only ever ends the last group: a whole group, two characters and
    // `==`, or three and `=`. Anything else is not base64.
    let whole = matches!((held, padding), (0, 0) | (2, 2) | (3, 1));
    if !whole {
        return Err(MuxError::command(
            "reading a command's output",
            "base64 that does not end on a boundary".to_string(),
        ));
    }
    if held != 0 {
        // The last group carries `held - 1` real bytes; the padding is what the
        // `=` characters stood in for, and is not data.
        let mut triple = chunk[0] << 18;
        if held > 1 {
            triple |= chunk[1] << 12;
        }
        if held > 2 {
            triple |= chunk[2] << 6;
        }
        bytes.push((triple >> 16) as u8);
        if held > 2 {
            bytes.push((triple >> 8) as u8);
        }
    }
    String::from_utf8(bytes).map_err(|error| {
        MuxError::command(
            "reading a command's output",
            format!("its output is not UTF-8: {error}"),
        )
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{match_space, tab_label, terminal_of_pane};
    use crate::placement::spaces_from_document;
    use serde_json::json;

    #[test]
    fn a_tab_label_is_the_title_a_person_reads() {
        assert_eq!(tab_label("pi·T1"), "pi·T1");
        assert_eq!(tab_label("codex·T1-review"), "codex·T1-review");
        // A character a document key has no business carrying becomes `-`.
        assert_eq!(tab_label("a/b\"c"), "a-b-c");
        assert_eq!(tab_label("   "), "shprd");
        assert_eq!(tab_label(""), "shprd");
        assert_eq!(tab_label(&"x".repeat(80)).chars().count(), 48);
    }

    #[test]
    fn a_space_is_matched_by_its_id_then_its_name() {
        let spaces = spaces_from_document(&json!({"spaces": [
            {"id": "sp1", "name": "shprd · repo", "tabs": []},
            {"id": "shprd · repo · r1 · T1", "name": "", "tabs": []},
        ]}));
        assert_eq!(match_space(&spaces, "sp1").unwrap().id, "sp1");
        assert_eq!(
            match_space(&spaces, "shprd · repo").unwrap().id,
            "sp1",
            "the name a person sees"
        );
        assert_eq!(
            match_space(&spaces, "shprd · repo · r1 · T1").unwrap().id,
            "shprd · repo · r1 · T1",
            "a label-made space's id is its label"
        );
        assert!(match_space(&spaces, "nobody").is_none());
    }

    #[test]
    fn base64_output_decodes_whatever_its_padding() {
        // The three shapes `machine.exec` can answer in: no padding, one `=`,
        // two, and the empty case.
        assert_eq!(super::decode_base64("").expect("empty"), "");
        assert_eq!(super::decode_base64("aGk=").expect("padded once"), "hi");
        assert_eq!(
            super::decode_base64("ZXhlYyBvaw==").expect("padded twice"),
            "exec ok"
        );
        assert_eq!(
            super::decode_base64("ZXhlYw==").expect("one whole group"),
            "exec"
        );
        assert!(
            super::decode_base64("aGk").is_err(),
            "a short group is refused"
        );
        assert!(
            super::decode_base64("a===").is_err(),
            "padding alone is nothing"
        );
        assert!(super::decode_base64("a*b=").is_err(), "not base64");
        assert!(
            super::decode_base64("/w==").is_err(),
            "a byte that is not UTF-8 is refused rather than mangled"
        );
    }

    #[test]
    fn a_pane_is_found_by_the_terminal_it_shows() {
        let layout = json!({"spaces": [{"id": "sp1", "tabs": [
            {"id": "tb1", "root": {"type": "row", "children": [
                {"type": "block", "id": "p1", "content": {"terminal": "t1"}},
                {"type": "column", "children": [
                    {"type": "block", "id": "p2", "content": {"terminal": "box/t9"}}]}
            ]}}
        ]}]});
        assert_eq!(terminal_of_pane(&layout, "p2").as_deref(), Some("box/t9"));
        assert!(terminal_of_pane(&layout, "p9").is_none());
    }
}
