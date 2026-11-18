//! One link to herdr-core: `hc connect` as a child, calls over it, and a keeper
//! that brings it back when it dies.
//!
//! Everything goes through a process rather than the core's Unix socket
//! (M7ef §2.13): `hc connect` is the documented way in on every platform, it
//! works the same over ssh, and it hides the socket path — which on macOS has a
//! 103-byte limit that a deep `XDG_STATE_HOME` breaks with a hard error.
//!
//! The child's environment is filtered, not inherited: a stray `HC_SOCKET`,
//! `HC_TERMINAL_ID` or `HC_CORE_ID` would silently address a different core —
//! possibly the person's own — so only what the endpoint names is passed on.
//!
//! Three rules keep a dead core from hanging a caller:
//!
//! - every call has a timeout, and a timed-out call fails rather than waiting;
//! - when the child dies, every call in flight fails at once with
//!   [`Error::Unreachable`], and the keeper reconnects by itself;
//! - a mutating call is never retried blind: a timeout leaves the core's state
//!   unknown, so the caller decides.

use std::collections::BTreeMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::sync::{Notify, mpsc, oneshot, watch};

use crate::error::{CORE_GENERATION, CORE_MINIMUM, Error, Result};
use crate::events::{Fold, State};
use crate::model::Notice;
use crate::wire::{self, FrameKind, Incoming, Parser};

/// How long a call waits by default before it is declared uncertain.
///
/// herdr-core answers every method in milliseconds except `terminal.send`,
/// which waits out a draft hold of about 2.2 s by design (M7ef §2.3); a caller
/// that sends text passes its own, longer timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);

/// The most calls that may wait on one core at once.
///
/// A reader that stops reading must not let the pending map grow without bound;
/// the number is Shepherdr's (`client.ts:103-127`).
pub const MAX_PENDING: usize = 256;

/// The most bytes one `data` frame of typed input carries.
const TYPE_CHUNK: usize = 4096;

/// The delay before each reconnect attempt: 0.5 s, 1 s, 2 s, then
/// [`RECONNECT_CEILING`] forever.
///
/// A person restarting `hc` should not have to restart shprd, and a core that is
/// gone for a minute should not cost a minute of retries (M7ef §2.10,
/// `hub.ts:57-62`).
pub const RECONNECT_DELAYS: [Duration; 4] = [
    Duration::from_millis(500),
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(5),
];

/// The delay every attempt uses after [`RECONNECT_DELAYS`] runs out.
pub const RECONNECT_CEILING: Duration = Duration::from_secs(5);

/// What `hello` must list for this crate to work at all.
///
/// `hello` names every method a core has, so a core that predates one is
/// refused up front instead of failing the first time a task runs.
pub const REQUIRED_METHODS: [&str; 12] = [
    "hello",
    "server.status",
    "terminal.run",
    "terminal.list",
    "terminal.get",
    "terminal.label",
    "terminal.kill",
    "terminal.rm",
    "events.subscribe",
    "doc.get",
    "doc.set",
    "rpc.discover",
];

/// How to reach a core, and how patient to be about it.
#[derive(Debug, Clone)]
pub struct Endpoint {
    /// The `hc` binary to spawn (`SHPRD_HC` overrides this at the wiring edge).
    pub binary: String,
    /// The core's session name (`HC_SESSION`); `None` keeps whatever the
    /// environment names, which for shprd is normally nothing.
    pub session: Option<String>,
    /// The core's socket path (`HC_SOCKET`), which skips session lookup.
    pub socket: Option<String>,
    /// How long a call waits when its caller names no timeout.
    pub timeout: Duration,
    /// Whether a lost connection is retried.
    pub reconnect: bool,
    /// The directories `hc` resolves its own state in. Empty means "whatever
    /// the environment already says", which is right for a desktop install and
    /// wrong for a test or a sandbox.
    pub directories: Vec<(String, String)>,
}

impl Default for Endpoint {
    fn default() -> Self {
        Self {
            binary: "hc".to_string(),
            session: None,
            socket: None,
            timeout: DEFAULT_TIMEOUT,
            reconnect: true,
            directories: Vec::new(),
        }
    }
}

impl Endpoint {
    /// An endpoint for the `hc` on `PATH`.
    pub fn new() -> Self {
        Self::default()
    }

    /// An endpoint for a specific `hc` binary.
    pub fn with_binary(binary: impl Into<String>) -> Self {
        Self {
            binary: binary.into(),
            ..Self::default()
        }
    }

    /// Use a named core session (`HC_SESSION`), which is how a test or a
    /// sandbox keeps away from the person's own core.
    pub fn with_session(mut self, session: impl Into<String>) -> Self {
        self.session = Some(session.into());
        self
    }

    /// Address a core by socket path (`HC_SOCKET`).
    pub fn with_socket(mut self, socket: impl Into<String>) -> Self {
        self.socket = Some(socket.into());
        self
    }

    /// The default timeout for calls made through this endpoint.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Whether to reconnect after the child dies.
    pub fn with_reconnect(mut self, reconnect: bool) -> Self {
        self.reconnect = reconnect;
        self
    }

    /// Point `hc` at its own XDG directories, so a test or a sandbox never
    /// shares a person's `~/.local/state`.
    ///
    /// This is what makes a session really private: `HC_SESSION` alone names a
    /// core, but *where* that core keeps its socket and its terminals comes
    /// from `XDG_STATE_HOME`. Only the five directories `hc` reads are accepted
    /// (Shepherdr restricts them the same way, `client.ts:19`), and a value
    /// with a NUL in it or longer than 4 KiB is refused, because it would be
    /// truncated or ignored by the child rather than used.
    pub fn with_directories<I, K, V>(mut self, directories: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.directories = directories
            .into_iter()
            .map(|(key, value)| (key.into(), value.into()))
            .collect();
        self
    }

    /// The directory variables `hc` is given, checked.
    pub fn checked_directories(&self) -> Result<Vec<(&str, &str)>> {
        let mut checked = Vec::new();
        for (key, value) in &self.directories {
            if !Self::DIRECTORIES.contains(&key.as_str()) {
                return Err(Error::Output(format!(
                    "{key} is not one of the directories `hc` reads"
                )));
            }
            if value.contains('\0') || value.len() > 4096 {
                return Err(Error::Output(format!("{key} is not a usable path")));
            }
            let known = Self::DIRECTORIES
                .iter()
                .find(|known| **known == key.as_str())
                .copied()
                .ok_or_else(|| Error::Output(format!("{key} is not a directory")))?;
            checked.push((known, value.as_str()));
        }
        Ok(checked)
    }

    /// The directories `hc` resolves its state in, and nothing else.
    pub const DIRECTORIES: [&'static str; 5] = [
        "XDG_RUNTIME_DIR",
        "XDG_STATE_HOME",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_CACHE_HOME",
    ];

    /// The environment variables `hc connect` is given.
    ///
    /// `HC_SOCKET` beats `HC_SESSION`, as it does for `hc` itself, and neither
    /// is passed when the endpoint names neither — the core's own default then
    /// applies.
    pub fn environment(&self) -> Vec<(&'static str, String)> {
        if let Some(socket) = &self.socket {
            vec![("HC_SOCKET", socket.clone())]
        } else if let Some(session) = &self.session {
            vec![("HC_SESSION", session.clone())]
        } else {
            Vec::new()
        }
    }

    /// The variables a spawned `hc connect` must not inherit, because they name
    /// a terminal or a core rather than the session this endpoint chose.
    pub const DROPPED: [&'static str; 5] = [
        "HC_SOCKET",
        "HC_SESSION",
        "HC_TERMINAL_ID",
        "HC_CORE_ID",
        "HC_PARENT",
    ];
}

/// What `hello` answered, as much of it as this crate reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hello {
    /// The core's version (`0.1.21` on the core the M7ef spike tested).
    pub version: String,
    /// The protocol generation.
    pub generation: u32,
    /// Whether the core took the framed upgrade.
    pub framed: bool,
    /// The core's id.
    pub core_id: String,
    /// The core's name.
    pub core_name: String,
    /// Every method the core has.
    pub methods: Vec<String>,
}

impl Hello {
    /// Read a `hello` result, refusing anything this crate cannot use.
    ///
    /// The order matters: generation first, because a core of another
    /// generation may not even parse the rest; then the version floor; then the
    /// framed upgrade and the methods.
    pub fn from_result(result: &Value) -> Result<Self> {
        let generation = result
            .get("generation")
            .and_then(Value::as_u64)
            .and_then(|generation| u32::try_from(generation).ok())
            .ok_or_else(|| Error::Output("`hello` answered without a generation".into()))?;
        if generation != CORE_GENERATION {
            return Err(Error::Generation {
                found: generation,
                needed: CORE_GENERATION,
            });
        }
        let version = result
            .get("version")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Output("`hello` answered without a version".into()))?
            .to_string();
        if !version_at_least(&version, CORE_MINIMUM) {
            return Err(Error::VersionTooOld {
                found: version,
                needed: CORE_MINIMUM,
            });
        }
        if !result
            .get("framed")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return Err(Error::Incompatible {
                version,
                what: "framed connections".into(),
            });
        }
        let methods: Vec<String> = result
            .get("methods")
            .and_then(Value::as_array)
            .map(|methods| {
                methods
                    .iter()
                    .filter_map(|method| method.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let missing: Vec<&str> = REQUIRED_METHODS
            .iter()
            .copied()
            .filter(|wanted| !methods.iter().any(|method| method == wanted))
            .collect();
        if !missing.is_empty() {
            return Err(Error::Incompatible {
                version,
                what: missing.join(", "),
            });
        }
        Ok(Self {
            version,
            generation,
            framed: true,
            core_id: result
                .get("core")
                .and_then(|core| core.get("id"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            core_name: result
                .get("core")
                .and_then(|core| core.get("name"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            methods,
        })
    }
}

/// Whether `found` is `needed` or newer, comparing `major.minor.patch` part by
/// part so `0.1.100` beats `0.1.21`.
///
/// Anything that is not a dotted triple is refused: a version shprd cannot read
/// is a version it cannot vouch for.
pub fn version_at_least(found: &str, needed: &str) -> bool {
    fn parts(text: &str) -> Option<(u64, u64, u64)> {
        let core = text
            .split(['-', '+'])
            .next()
            .filter(|core| !core.is_empty())?;
        let mut pieces = core.split('.');
        let major = pieces.next()?.parse().ok()?;
        let minor = pieces.next()?.parse().ok()?;
        let patch = pieces.next()?.parse().ok()?;
        if pieces.next().is_some() {
            return None;
        }
        Some((major, minor, patch))
    }
    match (parts(found), parts(needed)) {
        (Some(found), Some(needed)) => found >= needed,
        _ => false,
    }
}

/// One call waiting for its reply.
struct Pending {
    method: String,
    reply: oneshot::Sender<std::result::Result<Value, Error>>,
}

/// A live connection: what a call needs, and how to notice it died.
struct Link {
    hello: Hello,
    /// Outgoing bytes, already framed once the upgrade happened.
    outgoing: mpsc::Sender<Vec<u8>>,
    pending: Arc<Mutex<BTreeMap<u64, Pending>>>,
    /// Set once when the reader or the keeper sees the child go.
    dead: watch::Sender<bool>,
    /// Kept so the child dies with the link.
    _child: Child,
}

impl std::fmt::Debug for Link {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Link")
            .field("hello", &self.hello)
            .field("dead", &self.dead.borrow())
            .finish_non_exhaustive()
    }
}

impl Link {
    /// Whether the child is gone.
    fn is_dead(&self) -> bool {
        *self.dead.borrow()
    }

    /// Mark the link dead, once, and fail every call in flight.
    fn bury(&self, reason: &str) {
        self.dead.send_replace(true);
        if let Ok(mut pending) = self.pending.lock() {
            for (_, call) in std::mem::take(&mut *pending) {
                let _ = call.reply.send(Err(Error::Unreachable(reason.to_string())));
            }
        }
    }
}

/// A client for one herdr-core: calls, the event fold and reconnects.
///
/// Cloning is cheap and shares the connection, so one client is handed to every
/// consumer of a core. Dropping the last clone closes the child.
#[derive(Clone)]
pub struct HcClient {
    inner: Arc<Shared>,
}

/// What every clone of a client shares.
struct Shared {
    endpoint: Endpoint,
    next_id: AtomicU64,
    link: tokio::sync::Mutex<Option<Arc<Link>>>,
    /// Bumped on every successful connection, so a caller can tell that the
    /// core it is talking to was replaced.
    generation: AtomicU64,
    /// Set by [`HcClient::close`], so the keeper stops reconnecting.
    closed: AtomicBool,
    fold: Mutex<Fold>,
    /// Fires when a connection is (re)established, so a waiting call wakes.
    connected: Notify,
    /// The folded state, published on every change.
    changed: watch::Sender<State>,
}

impl std::fmt::Debug for HcClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HcClient")
            .field("endpoint", &self.inner.endpoint)
            .field("generation", &self.generation())
            .finish_non_exhaustive()
    }
}

impl HcClient {
    /// Connect, handshake and subscribe.
    pub async fn connect(endpoint: Endpoint) -> Result<Self> {
        let client = Self::unconnected(endpoint);
        let link = open(&client.inner).await?;
        *client.inner.link.lock().await = Some(Arc::clone(&link));
        client.publish();
        client.spawn_keeper(Some(link));
        Ok(client)
    }

    /// A client that connects in the background, so a caller can start before
    /// the core does. Every call made meanwhile fails with
    /// [`Error::Unreachable`], and lands once the core is up.
    pub fn lazy(endpoint: Endpoint) -> Self {
        let client = Self::unconnected(endpoint);
        client.spawn_keeper(None);
        client
    }

    /// Build a client without touching the network.
    fn unconnected(endpoint: Endpoint) -> Self {
        Self {
            inner: Arc::new(Shared {
                endpoint,
                next_id: AtomicU64::new(0),
                link: tokio::sync::Mutex::new(None),
                generation: AtomicU64::new(0),
                closed: AtomicBool::new(false),
                fold: Mutex::new(Fold::new()),
                connected: Notify::new(),
                changed: watch::channel(State::default()).0,
            }),
        }
    }

    /// The store the fold keeps, as of now.
    pub fn state(&self) -> State {
        self.inner.changed.borrow().clone()
    }

    /// A watch channel carrying the fold; `changed()` on it resolves whenever
    /// the core said something that moved the model.
    pub fn changes(&self) -> watch::Receiver<State> {
        self.inner.changed.subscribe()
    }

    /// What `hello` said about the core this client is talking to.
    pub async fn hello(&self) -> Result<Hello> {
        Ok(self.link_now().await?.hello.clone())
    }

    /// How many connections this client has made; 0 before the first.
    pub fn generation(&self) -> u64 {
        self.inner.generation.load(Ordering::SeqCst)
    }

    /// The endpoint this client talks to.
    pub fn endpoint(&self) -> &Endpoint {
        &self.inner.endpoint
    }

    /// Call a method, with the endpoint's default timeout.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.call_with(method, params, self.inner.endpoint.timeout)
            .await
    }

    /// Call a method, waiting at most `timeout`.
    ///
    /// A timeout fails the call with [`Error::Timeout`] and leaves the core's
    /// state unknown: the request may still be running there, so a mutating
    /// call is the caller's to retry, not this client's.
    pub async fn call_with(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let link = self.link_for(timeout).await?;
        let id = self.next_id();
        let (reply, answer) = oneshot::channel();
        {
            let mut pending = link
                .pending
                .lock()
                .map_err(|_| Error::Output("the pending map is poisoned".into()))?;
            if pending.len() >= MAX_PENDING {
                return Err(Error::Busy(pending.len()));
            }
            pending.insert(
                id,
                Pending {
                    method: method.to_string(),
                    reply,
                },
            );
        }
        let message = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        if let Err(error) = self.write(&link, &message).await {
            if let Ok(mut pending) = link.pending.lock() {
                pending.remove(&id);
            }
            return Err(error);
        }
        match tokio::time::timeout(timeout, answer).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(Error::Unreachable(
                "herdr-core answered nothing for this call".into(),
            )),
            Err(_) => {
                if let Ok(mut pending) = link.pending.lock() {
                    pending.remove(&id);
                }
                Err(Error::Timeout {
                    method: method.to_string(),
                    after: timeout,
                })
            }
        }
    }

    /// Type raw `bytes` into `terminal` through a short attach stream.
    ///
    /// `terminal.send` pastes and presses enter and `terminal.keys` cannot spell
    /// most punctuation, so text left at the cursor goes the way Shepherdr's
    /// own web terminal does (M7ef §2.6): a full attach at the terminal's own
    /// size, the bytes as `data` frames on its channel, then `close`. The
    /// stream is credited by the read loop like any other, so its first paint
    /// does not wedge the connection.
    pub async fn type_raw(
        &self,
        terminal: &str,
        cols: u32,
        rows: u32,
        bytes: &[u8],
        timeout: Duration,
    ) -> Result<()> {
        let attached = self
            .call_with(
                "terminal.attach",
                json!({"terminal": terminal, "cols": cols, "rows": rows, "history": 0}),
                timeout,
            )
            .await?;
        let channel = attached
            .get("channel")
            .and_then(Value::as_u64)
            .and_then(|channel| u32::try_from(channel).ok())
            .ok_or_else(|| Error::Output("terminal.attach answered no channel".into()))?;
        let link = self.link_for(timeout).await?;
        let sent = async {
            for chunk in bytes.chunks(TYPE_CHUNK) {
                let frame = wire::encode_frame(channel, FrameKind::Data, chunk)?;
                link.outgoing.send(frame).await.map_err(|_| {
                    link.bury("the core connection is gone");
                    Error::Unreachable("the core connection is gone".into())
                })?;
            }
            Ok(())
        }
        .await;
        // Close even when the write failed: an attach left open keeps a viewer
        // on the terminal.
        if let Ok(frame) = wire::encode_frame(channel, FrameKind::Close, &[]) {
            let _ = link.outgoing.send(frame).await;
        }
        sent
    }

    /// Read every document the fold is waiting for, and take the values.
    ///
    /// This is the re-read discipline M7ef insists on: a `missed` notice, a
    /// fresh subscription and a reconnect all name documents here, because a
    /// terminal snapshot never carries one (M7ef §2.4, §2.7).
    pub async fn read_pending_documents(&self) -> Result<Vec<String>> {
        let names: Vec<String> = {
            let fold = self
                .inner
                .fold
                .lock()
                .map_err(|_| Error::Output("the fold is poisoned".into()))?;
            fold.state().pending.iter().cloned().collect()
        };
        let mut read = Vec::new();
        for name in names {
            let result = self.call("doc.get", json!({"name": name})).await?;
            self.take_document(&name, &result);
            read.push(name);
        }
        Ok(read)
    }

    /// Read the layout document into the fold.
    pub async fn read_layout(&self) -> Result<()> {
        self.layout().await.map(|_| ())
    }

    /// Read the layout document into the fold, and answer its value.
    ///
    /// A caller that needs a part of the layout the fold does not model — the
    /// pane id behind a terminal, say — reads it here rather than asking the
    /// core twice: the value and the fold come from the same `doc.get`, so they
    /// cannot disagree.
    pub async fn layout(&self) -> Result<Value> {
        let result = self
            .call("doc.get", json!({"name": crate::events::LAYOUT_DOCUMENT}))
            .await?;
        let value = result.get("value").cloned().unwrap_or(Value::Null);
        self.take_document(crate::events::LAYOUT_DOCUMENT, &result);
        Ok(value)
    }

    /// Every terminal the core has, from the core rather than the fold.
    pub async fn terminals(&self) -> Result<Vec<crate::model::Terminal>> {
        let result = self.call("terminal.list", json!({})).await?;
        Ok(crate::model::terminals_from(result.get("terminals")))
    }

    /// Close the connection and stop reconnecting.
    pub async fn close(&self) {
        self.inner.closed.store(true, Ordering::SeqCst);
        if let Some(link) = self.inner.link.lock().await.take() {
            link.bury("the client was closed");
        }
    }

    /// Take a `doc.get` result into the fold.
    fn take_document(&self, name: &str, result: &Value) {
        let value = result.get("value").cloned().unwrap_or(Value::Null);
        let version = result
            .get("version")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        if let Ok(mut fold) = self.inner.fold.lock() {
            fold.set_document(name, &value, version);
        }
        self.publish();
    }

    /// The next request id.
    fn next_id(&self) -> u64 {
        self.inner.next_id.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Publish the fold as it stands.
    fn publish(&self) {
        let state = self
            .inner
            .fold
            .lock()
            .map(|fold| fold.state().clone())
            .unwrap_or_default();
        self.inner.changed.send_replace(state);
    }

    /// Apply one notice to the fold and publish.
    fn apply(&self, notice: Notice) {
        if let Ok(mut fold) = self.inner.fold.lock() {
            fold.apply(notice);
        }
        self.publish();
    }

    /// Forget the fold: the connection it described is gone.
    fn forget(&self) {
        if let Ok(mut fold) = self.inner.fold.lock() {
            fold.reset();
        }
        self.publish();
    }

    /// The live link without waiting.
    async fn link_now(&self) -> Result<Arc<Link>> {
        if let Some(link) = self.inner.link.lock().await.as_ref()
            && !link.is_dead()
        {
            return Ok(Arc::clone(link));
        }
        Err(Error::Unreachable(
            "there is no connection to herdr-core".into(),
        ))
    }

    /// The live link, waiting up to `timeout` for a reconnect to land.
    async fn link_for(&self, timeout: Duration) -> Result<Arc<Link>> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            // Register for the wake-up before looking, so a connection that
            // lands between the two is not missed.
            let woken = self.inner.connected.notified();
            let mut woken = std::pin::pin!(woken);
            woken.as_mut().enable();
            if let Ok(link) = self.link_now().await {
                return Ok(link);
            }
            if tokio::time::timeout_at(deadline, woken).await.is_err() {
                return Err(Error::Unreachable(
                    "herdr-core did not come back in time".into(),
                ));
            }
        }
    }

    /// Write one framed message on `link`.
    async fn write(&self, link: &Link, message: &Value) -> Result<()> {
        let body = serde_json::to_vec(message)
            .map_err(|error| Error::Output(format!("cannot encode a call: {error}")))?;
        let frame = wire::encode_frame(0, FrameKind::Data, &body)?;
        link.outgoing.send(frame).await.map_err(|_| {
            link.bury("the core connection is gone");
            Error::Unreachable("the core connection is gone".into())
        })
    }

    /// Start the keeper: it waits for the current link to die (if there is
    /// one), forgets the fold, and reconnects until the endpoint says stop.
    fn spawn_keeper(&self, first: Option<Arc<Link>>) {
        // A weak handle, so the keeper stops when the last client is dropped:
        // a strong one would keep the child alive forever.
        let shared = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            let mut current = first;
            loop {
                if let Some(link) = current.take() {
                    let mut dead = link.dead.subscribe();
                    if !link.is_dead() {
                        let _ = dead.wait_for(|dead| *dead).await;
                    }
                    drop(dead);
                    let Some(inner) = shared.upgrade() else {
                        return;
                    };
                    {
                        let mut slot = inner.link.lock().await;
                        if slot.as_ref().is_some_and(|held| Arc::ptr_eq(held, &link)) {
                            *slot = None;
                        }
                    }
                    // Nothing the old connection said is trusted any more.
                    HcClient { inner }.forget();
                }
                match reconnect(&shared).await {
                    Some((inner, link)) => {
                        let client = HcClient {
                            inner: Arc::clone(&inner),
                        };
                        *inner.link.lock().await = Some(Arc::clone(&link));
                        inner.connected.notify_waiters();
                        client.publish();
                        // Documents are never in a snapshot: read them again.
                        if let Err(error) = client.read_pending_documents().await {
                            tracing::debug!(%error, "re-read after reconnect failed");
                        }
                        current = Some(link);
                    }
                    None => return,
                }
            }
        });
    }
}

/// Try to open a connection, retrying until one lands or the endpoint says no.
///
/// The first attempt is immediate — the child that just died may be replaced in
/// milliseconds — and the rest follow [`RECONNECT_DELAYS`] and then
/// [`RECONNECT_CEILING`] forever. Waiting on a `Weak` is what lets a dropped
/// client stop the retries: a shared state nobody holds is a link nobody wants.
async fn reconnect(shared: &Weak<Shared>) -> Option<(Arc<Shared>, Arc<Link>)> {
    let mut attempt = 0usize;
    loop {
        if attempt > 0 {
            let delay = RECONNECT_DELAYS
                .get(attempt - 1)
                .copied()
                .unwrap_or(RECONNECT_CEILING);
            tokio::time::sleep(delay).await;
        }
        attempt += 1;
        let inner = shared.upgrade()?;
        if inner.closed.load(Ordering::SeqCst) {
            return None;
        }
        match open(&inner).await {
            Ok(link) => return Some((inner, link)),
            Err(error) => {
                tracing::debug!(%error, "herdr-core reconnect failed");
                if !inner.endpoint.reconnect {
                    return None;
                }
            }
        }
    }
}

/// Spawn `hc connect`, say `hello`, subscribe, and read one event line more.
///
/// `hello` is the only plain message: its reply switches the core to framed
/// mode as the reply is parsed, and [`Parser`] is where that switch happens.
async fn open(inner: &Arc<Shared>) -> Result<Arc<Link>> {
    let endpoint = &inner.endpoint;
    let mut command = Command::new(&endpoint.binary);
    command
        .arg("connect")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for variable in Endpoint::DROPPED {
        command.env_remove(variable);
    }
    for (key, value) in endpoint.environment() {
        command.env(key, value);
    }
    for (key, value) in endpoint.checked_directories()? {
        command.env(key, value);
    }
    let mut child = command.spawn().map_err(|error| {
        Error::Unreachable(format!(
            "cannot start `{} connect`: {error}",
            endpoint.binary
        ))
    })?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| Error::Unreachable("`hc connect` has no stdin".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::Unreachable("`hc connect` has no stdout".into()))?;
    if let Some(mut stderr) = child.stderr.take() {
        // The child's stderr is its own log. Draining it keeps a chatty core
        // from blocking on a full pipe, and the tail goes to the log at debug.
        tokio::spawn(async move {
            let mut sink = Vec::new();
            let _ = stderr.read_to_end(&mut sink).await;
            if !sink.is_empty() {
                tracing::debug!(
                    target: "shprd_mux_hc",
                    "hc connect: {}",
                    String::from_utf8_lossy(&sink)
                );
            }
        });
    }

    let (outgoing, mut queued) = mpsc::channel::<Vec<u8>>(256);
    tokio::spawn(async move {
        while let Some(bytes) = queued.recv().await {
            if stdin.write_all(&bytes).await.is_err() || stdin.flush().await.is_err() {
                break;
            }
        }
    });

    let pending: Arc<Mutex<BTreeMap<u64, Pending>>> = Arc::new(Mutex::new(BTreeMap::new()));
    let event_channel: Arc<Mutex<Option<u32>>> = Arc::new(Mutex::new(None));
    let event_buffer: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let dead = watch::channel(false).0;

    let hello_id = inner.next_id.fetch_add(1, Ordering::SeqCst) + 1;
    let (hello_reply, hello_answer) = oneshot::channel();
    if let Ok(mut calls) = pending.lock() {
        calls.insert(
            hello_id,
            Pending {
                method: "hello".to_string(),
                reply: hello_reply,
            },
        );
    }

    let reader = Arc::downgrade(inner);
    let reader_pending = Arc::clone(&pending);
    let reader_events = Arc::clone(&event_channel);
    let reader_buffer = Arc::clone(&event_buffer);
    let reader_outgoing = outgoing.clone();
    let reader_dead = dead.clone();
    let subscribe_id = inner.next_id.fetch_add(1, Ordering::SeqCst) + 1;
    tokio::spawn(async move {
        read_loop(
            stdout,
            hello_id,
            subscribe_id,
            reader,
            reader_pending,
            reader_events,
            reader_buffer,
            reader_outgoing,
            reader_dead,
        )
        .await;
    });

    let hello_bytes = wire::encode_plain(&json!({
        "jsonrpc": "2.0",
        "id": hello_id,
        "method": "hello",
        "params": {"generation": CORE_GENERATION, "framed": true},
    }))?;
    if outgoing.send(hello_bytes).await.is_err() {
        return Err(Error::Unreachable(
            "`hc connect` closed its input before hello".into(),
        ));
    }

    let hello = match tokio::time::timeout(endpoint.timeout, hello_answer).await {
        Ok(Ok(result)) => result.and_then(|result| Hello::from_result(&result)),
        Ok(Err(_)) => Err(Error::Unreachable("`hc connect` gave no greeting".into())),
        Err(_) => Err(Error::Timeout {
            method: "hello".to_string(),
            after: endpoint.timeout,
        }),
    }?;

    // Everything from here is framed. The reader learns the event channel from
    // this reply itself, so the first event — which may arrive in the same
    // read as the reply — is not lost.
    let (subscribe_reply, subscribe_answer) = oneshot::channel();
    if let Ok(mut calls) = pending.lock() {
        calls.insert(
            subscribe_id,
            Pending {
                method: "events.subscribe".to_string(),
                reply: subscribe_reply,
            },
        );
    }
    let body = serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "id": subscribe_id,
        "method": "events.subscribe",
        "params": {},
    }))
    .map_err(|error| Error::Output(format!("cannot encode the subscribe: {error}")))?;
    if outgoing
        .send(wire::encode_frame(0, FrameKind::Data, &body)?)
        .await
        .is_err()
    {
        return Err(Error::Unreachable(
            "`hc connect` closed its input before subscribing".into(),
        ));
    }
    let snapshot = match tokio::time::timeout(endpoint.timeout, subscribe_answer).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err(Error::Unreachable(
            "`events.subscribe` was never answered".into(),
        )),
        Err(_) => Err(Error::Timeout {
            method: "events.subscribe".to_string(),
            after: endpoint.timeout,
        }),
    }?;

    if snapshot
        .get("channel")
        .and_then(Value::as_u64)
        .and_then(|channel| u32::try_from(channel).ok())
        .is_none()
    {
        return Err(Error::Incompatible {
            version: hello.version.clone(),
            what: "an event channel".into(),
        });
    }
    // The reader folded the snapshot into the fold before it woke this call up
    // (see `read_loop`), so a caller sees the core's terminals the moment
    // `connect` returns.
    inner.generation.fetch_add(1, Ordering::SeqCst);

    Ok(Arc::new(Link {
        hello,
        outgoing,
        pending,
        dead,
        _child: child,
    }))
}

/// Read the core's stdout until it ends, routing replies and event lines.
#[allow(clippy::too_many_arguments)]
async fn read_loop(
    mut stdout: tokio::process::ChildStdout,
    hello_id: u64,
    subscribe_id: u64,
    shared: Weak<Shared>,
    pending: Arc<Mutex<BTreeMap<u64, Pending>>>,
    event_channel: Arc<Mutex<Option<u32>>>,
    event_buffer: Arc<Mutex<Vec<u8>>>,
    outgoing: mpsc::Sender<Vec<u8>>,
    dead: watch::Sender<bool>,
) {
    let mut parser = Parser::new(hello_id);
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        let read = match stdout.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        let incoming = match parser.push(&chunk[..read]) {
            Ok(incoming) => incoming,
            Err(error) => {
                tracing::debug!(%error, "herdr-core broke the protocol");
                break;
            }
        };
        for message in incoming {
            match message {
                Incoming::Upgraded => {}
                Incoming::Control(value) => {
                    if value.get("id").is_none() {
                        // A notification with no id rides on channel 0 only
                        // before the event stream exists; treat it as an event.
                        if let Some(inner) = shared.upgrade() {
                            handle_line(&inner, value.to_string().as_bytes());
                        }
                    } else {
                        if value.get("id").and_then(Value::as_u64) == Some(subscribe_id) {
                            // Learn the event channel and fold the snapshot
                            // here, in stream order, so an event that arrives
                            // in the same read as this reply cannot be lost or
                            // overwritten by it.
                            if let Some(result) = value.get("result") {
                                let channel = result
                                    .get("channel")
                                    .and_then(Value::as_u64)
                                    .and_then(|channel| u32::try_from(channel).ok());
                                if let (Some(channel), Ok(mut slot)) =
                                    (channel, event_channel.lock())
                                {
                                    *slot = Some(channel);
                                }
                                let Some(inner) = shared.upgrade() else {
                                    break;
                                };
                                let client = HcClient { inner };
                                client.apply(Notice::Synced {
                                    seq: result
                                        .get("seq")
                                        .and_then(Value::as_u64)
                                        .unwrap_or_default(),
                                    terminals: crate::model::terminals_from(
                                        result.get("terminals"),
                                    ),
                                });
                            }
                        }
                        reply(&pending, &value);
                    }
                }
                Incoming::Data {
                    channel: stream,
                    payload,
                } => {
                    let channel = event_channel.lock().ok().and_then(|slot| *slot);
                    if Some(stream) == channel {
                        if let Some(inner) = shared.upgrade() {
                            split_events(&inner, &event_buffer, &payload);
                        }
                    } else {
                        // A stream nobody reads still has to be credited, or
                        // the core stops sending on it (M7ef §2.6).
                        if let Ok(credit) = wire::credit_frame(stream, payload.len()) {
                            let _ = outgoing.try_send(credit);
                        }
                    }
                }
                Incoming::Credit { .. } | Incoming::Close { .. } | Incoming::Other { .. } => {}
            }
        }
    }
    dead.send_replace(true);
    if let Ok(mut calls) = pending.lock() {
        for (_, call) in std::mem::take(&mut *calls) {
            let _ = call.reply.send(Err(Error::Unreachable(
                "herdr-core closed the connection".into(),
            )));
        }
    }
}

/// Hand one reply to the call that waits for it.
fn reply(pending: &Mutex<BTreeMap<u64, Pending>>, value: &Value) {
    let Some(id) = value.get("id").and_then(Value::as_u64) else {
        return;
    };
    let Ok(mut calls) = pending.lock() else {
        return;
    };
    let Some(call) = calls.remove(&id) else {
        return;
    };
    let outcome = match value.get("error") {
        Some(error) => Err(Error::Refused {
            method: call.method,
            kind: error
                .get("data")
                .and_then(|data| data.get("kind"))
                .and_then(Value::as_str)
                .unwrap_or("rpc")
                .to_string(),
            message: error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            hint: error
                .get("data")
                .and_then(|data| data.get("hint"))
                .and_then(Value::as_str)
                .map(str::to_string),
        }),
        None => Ok(value.get("result").cloned().unwrap_or(Value::Null)),
    };
    let _ = call.reply.send(outcome);
}

/// Split one chunk of the event stream into lines and fold each.
fn split_events(shared: &Arc<Shared>, buffer: &Mutex<Vec<u8>>, chunk: &[u8]) {
    let Ok(mut buffer) = buffer.lock() else {
        return;
    };
    buffer.extend_from_slice(chunk);
    while let Some(end) = buffer.iter().position(|byte| *byte == b'\n') {
        let line: Vec<u8> = buffer.drain(..=end).collect();
        handle_line(shared, &line[..line.len() - 1]);
    }
}

/// Fold one JSON notification line, when it is one this crate knows.
fn handle_line(shared: &Arc<Shared>, line: &[u8]) {
    if line.is_empty() {
        return;
    }
    let Ok(value) = serde_json::from_slice::<Value>(line) else {
        return;
    };
    let Some(notice) = crate::model::notice_from_message(&value) else {
        return;
    };
    let client = HcClient {
        inner: Arc::clone(shared),
    };
    client.apply(notice);
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn the_floor_allows_a_newer_version() {
        assert!(version_at_least("0.1.21", CORE_MINIMUM));
        assert!(version_at_least("0.1.100", CORE_MINIMUM));
        assert!(version_at_least("0.2.0", CORE_MINIMUM));
        assert!(version_at_least("1.0.0", CORE_MINIMUM));
        assert!(version_at_least("0.1.21-beta.1", CORE_MINIMUM));
    }

    #[test]
    fn the_floor_refuses_an_older_version() {
        assert!(!version_at_least("0.1.20", CORE_MINIMUM));
        assert!(!version_at_least("0.0.99", CORE_MINIMUM));
        assert!(!version_at_least("", CORE_MINIMUM));
        assert!(!version_at_least("0.1", CORE_MINIMUM));
        assert!(!version_at_least("0.1.21.1", CORE_MINIMUM));
        assert!(!version_at_least("newest", CORE_MINIMUM));
    }

    /// The `hello` result M7ef recorded on herdr-core 0.1.21.
    fn recorded_hello(version: &str, generation: u32) -> Value {
        json!({
            "core": {"id": "06f3e992", "name": "mac"},
            "generation": generation,
            "framed": true,
            "version": version,
            "features": ["framed", "attach", "events", "status", "shell",
                         "resize_notifications", "automatic_input", "restart_environment"],
            "methods": ["hello", "server.status", "server.stop", "server.restart",
                        "terminal.run", "templates.list", "terminal.list", "terminal.get",
                        "terminal.kill", "terminal.rm", "terminal.keep", "terminal.label",
                        "terminal.read", "terminal.attach", "terminal.send", "terminal.wait",
                        "terminal.keys", "terminal.interrupt", "terminal.put",
                        "machine.exec", "file.read", "file.write", "file.edit",
                        "doc.get", "doc.set", "events.subscribe", "machine.add",
                        "machine.list", "machine.connect", "machine.rename", "machine.rm",
                        "rpc.discover"],
            "login_session": {"gui": true, "id": 100020, "user": 501},
            "keeps_running_after_logout": null,
            "logout_linger_user": null
        })
    }

    #[test]
    fn the_recorded_hello_is_accepted() {
        let hello = Hello::from_result(&recorded_hello("0.1.21", 1)).unwrap();
        assert_eq!(hello.version, "0.1.21");
        assert_eq!(hello.generation, 1);
        assert!(hello.framed);
        assert_eq!(hello.core_id, "06f3e992");
        assert_eq!(hello.core_name, "mac");
        assert_eq!(hello.methods.len(), 32);
    }

    #[test]
    fn an_older_core_is_refused_naming_both_versions() {
        let error = Hello::from_result(&recorded_hello("0.1.14", 1)).unwrap_err();
        assert!(matches!(error, Error::VersionTooOld { .. }), "{error}");
        let text = error.to_string();
        assert!(text.contains("0.1.14"), "{text}");
        assert!(text.contains(CORE_MINIMUM), "{text}");
    }

    #[test]
    fn another_generation_is_refused_before_the_version_is_read() {
        let error = Hello::from_result(&recorded_hello("0.0.1", 2)).unwrap_err();
        assert!(
            matches!(
                error,
                Error::Generation {
                    found: 2,
                    needed: 1
                }
            ),
            "{error}"
        );
    }

    #[test]
    fn a_core_without_a_required_method_is_refused() {
        let mut result = recorded_hello("0.1.21", 1);
        result["methods"] = json!(["hello", "doc.get"]);
        let error = Hello::from_result(&result).unwrap_err();
        assert!(matches!(error, Error::Incompatible { .. }), "{error}");
        assert!(error.to_string().contains("terminal.run"), "{error}");
    }

    #[test]
    fn a_core_that_will_not_frame_is_refused() {
        let mut result = recorded_hello("0.1.21", 1);
        result["framed"] = json!(false);
        let error = Hello::from_result(&result).unwrap_err();
        assert!(error.to_string().contains("framed connections"), "{error}");
    }

    #[test]
    fn an_endpoint_refuses_a_variable_hc_does_not_read() {
        let endpoint = Endpoint::new().with_directories([("PATH", "/tmp")]);
        assert!(endpoint.checked_directories().is_err());

        let endpoint = Endpoint::new().with_directories([("XDG_STATE_HOME", "/tmp/s")]);
        assert_eq!(
            endpoint.checked_directories().unwrap(),
            vec![("XDG_STATE_HOME", "/tmp/s")]
        );

        let endpoint = Endpoint::new().with_directories([("XDG_STATE_HOME", "/tmp/a\0b")]);
        assert!(endpoint.checked_directories().is_err());
    }

    #[test]
    fn an_endpoint_passes_only_what_it_names() {
        let plain = Endpoint::new();
        assert!(plain.environment().is_empty());
        let session = Endpoint::new().with_session("shprd-test");
        assert_eq!(
            session.environment(),
            vec![("HC_SESSION", "shprd-test".to_string())]
        );
        let socket = session.clone().with_socket("/tmp/s.sock");
        assert_eq!(
            socket.environment(),
            vec![("HC_SOCKET", "/tmp/s.sock".to_string())]
        );
        assert_eq!(Endpoint::with_binary("hc-0.1.21").binary, "hc-0.1.21");
        assert_eq!(Endpoint::new().timeout, DEFAULT_TIMEOUT);
        assert!(Endpoint::new().reconnect);
    }
}
