//! The service's own files, its listener and its lifecycle.
//!
//! A client finds the service the way Shepherdr's window does: the state
//! directory holds `<state>/shprd/service.json` (`{port, id, pid, version}`) and
//! a token file only the owner can read. The client reads the port, confirms the
//! `id` at `GET /api/health`, and only then sends the token.

use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use shprd_agents::data::DataDir;
use tokio::net::TcpListener;

use crate::api::{App, Paths};
use crate::error::ServerError;

/// The version every answer reports.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Where this service keeps its discovery files: `<state_home>/shprd`.
///
/// The state home itself comes from [`state_home`], resolved once by the binary,
/// so the service never reads `$XDG_STATE_HOME` or `$HOME` for itself.
pub fn state_dir(state_home: &Path) -> PathBuf {
    state_home.join("shprd")
}

/// The state home for `home`: `xdg_state_home` (the `$XDG_STATE_HOME` the caller
/// read) when it names a directory, else `<home>/.local/state`.
///
/// The value is an argument rather than an environment lookup so the one place
/// that reads `$XDG_STATE_HOME` is the binary's wiring, and a test that names its
/// own home can never write into the developer's state directory. An empty value
/// is unset, which is what the XDG specification says.
pub fn state_home(home: &Path, xdg_state_home: Option<&OsStr>) -> PathBuf {
    match xdg_state_home.filter(|value| !value.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => home.join(".local").join("state"),
    }
}

/// What `service.json` holds.
///
/// It is written on every start, so a client that found a stale file at least
/// sees a token check fail (`GET /api/health`'s `id`) instead of sending the
/// token to whatever now holds the port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceRecord {
    /// The port the service is listening on.
    pub port: u16,
    /// The service's identity for this process's lifetime.
    pub id: String,
    /// The process that wrote it.
    pub pid: u32,
    /// The service's version.
    pub version: String,
}

/// A step the binary runs after an action appended an event.
///
/// The CLI's write path projects `<root>/.dagr/run.json` after every write. The
/// projection is the binary's, not the service's, so `shprd serve` passes that
/// same step in here rather than the server crate growing a second copy of it.
/// A failure is the step's own business: it must not turn a recorded write into
/// an error the client sees.
#[derive(Clone)]
pub struct AfterWrite(std::sync::Arc<dyn Fn(&Path) + Send + Sync>);

impl AfterWrite {
    /// Wraps `step`, which receives the repository the action wrote to.
    pub fn new(step: impl Fn(&Path) + Send + Sync + 'static) -> Self {
        Self(std::sync::Arc::new(step))
    }

    /// Runs the step for `repo`.
    pub fn call(&self, repo: &Path) {
        (self.0)(repo);
    }
}

impl std::fmt::Debug for AfterWrite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AfterWrite")
    }
}

/// The discovery files in one state directory.
#[derive(Debug)]
pub struct ServiceFiles {
    dir: PathBuf,
    token: String,
    id: String,
    /// The lock this service holds for its state directory, for its whole life.
    ///
    /// Only one service can be found through one `service.json`, so only one is
    /// allowed to serve one state directory at a time. The lock is what makes
    /// that true rather than hoped for: without it a second start would take the
    /// record over, and stopping either one would leave the other running and
    /// undiscoverable.
    _lock: fs::File,
}

impl ServiceFiles {
    /// Opens (or creates) the discovery files in `dir`, taking its lock.
    ///
    /// The directory is made private (`0700`), the token is reused when a
    /// previous start made one, and a new token is written `0600`. The identity
    /// is new on every start, because it is what a client uses to tell this
    /// process from whatever held the port before it.
    ///
    /// # Errors
    ///
    /// [`ServerError::AlreadyRunning`] when a service already holds this state
    /// directory: a second one could only fight over its `service.json`.
    pub fn prepare(dir: &Path) -> Result<Self, ServerError> {
        create_private_dir(dir)?;
        let lock = take_lock(dir)?;
        let token_path = dir.join("token");
        let token = match read_token(&token_path)? {
            Some(token) => token,
            None => {
                let token = random_hex(32)?;
                write_private(&token_path, token.as_bytes())?;
                token
            }
        };
        Ok(Self {
            dir: dir.to_path_buf(),
            token,
            id: random_hex(16)?,
            _lock: lock,
        })
    }

    /// The directory the files live in.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Where the bearer token is kept.
    pub fn token_path(&self) -> PathBuf {
        self.dir.join("token")
    }

    /// Where the discovery record is kept.
    pub fn service_json_path(&self) -> PathBuf {
        self.dir.join("service.json")
    }

    /// The bearer token this service accepts.
    pub fn token(&self) -> &str {
        &self.token
    }

    /// This start's identity.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Writes `service.json` for `port`, replacing any earlier one.
    pub fn write_record(&self, port: u16) -> Result<ServiceRecord, ServerError> {
        let record = ServiceRecord {
            port,
            id: self.id.clone(),
            pid: std::process::id(),
            version: VERSION.to_string(),
        };
        let text = serde_json::to_string(&record).map_err(|source| ServerError::Json {
            what: "service.json",
            source,
        })?;
        let path = self.service_json_path();
        let temporary = self.dir.join("service.json.tmp");
        write_private(&temporary, text.as_bytes())?;
        fs::rename(&temporary, &path).map_err(|source| ServerError::io(path, source))?;
        Ok(record)
    }

    /// Removes `service.json`, if this process is still the one it names.
    ///
    /// A second service started against the same state directory overwrites the
    /// record with its own; when the first one then stops, removing the file
    /// blindly would make the service that is still running undiscoverable, so
    /// the file is read first and only removed when its `id` is this start's.
    /// A missing or unreadable file is not a failure: it is the state a
    /// shutdown in the middle of another one leaves behind.
    pub fn remove_record(&self) -> Result<(), ServerError> {
        let path = self.service_json_path();
        let ours = match fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str::<ServiceRecord>(&text)
                .map(|record| record.id == self.id)
                // A record that will not parse is not proof of ownership, but
                // nothing else can find a service through it either: leave it
                // for the next start to overwrite.
                .unwrap_or(false),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(ServerError::io(path, err)),
        };
        if !ours {
            return Ok(());
        }
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(ServerError::io(path, err)),
        }
    }

    /// Whether `service.json` currently names this process.
    pub fn owns_record(&self) -> bool {
        match fs::read_to_string(self.service_json_path()) {
            Ok(text) => serde_json::from_str::<ServiceRecord>(&text)
                .map(|record| record.id == self.id)
                .unwrap_or(false),
            Err(_) => false,
        }
    }
}

/// How long a start waits for a dying service to drop its lock, before it
/// decides the lock is not going anywhere.
///
/// It has to be long enough for the kernel to reap a killed process and release
/// its flock, which is usually milliseconds and, on a busy machine, can be a
/// second or two. The wait only ever costs a start that finds a lock nobody can
/// explain; a live service's record names a live pid, so that refusal is
/// immediate.
const SHUTDOWN_WAIT: std::time::Duration = std::time::Duration::from_secs(3);

/// How often that wait looks at the lock again.
const SHUTDOWN_POLL: std::time::Duration = std::time::Duration::from_millis(50);

/// Takes this state directory's exclusive lock, or says who holds it.
///
/// The lock lives as long as the [`File`] does — for the service's whole
/// lifetime — and the kernel drops it when the process ends, however it ends,
/// so a crashed service never leaves the directory locked against its successor.
///
/// A kill is not instant: the kernel drops the lock when the process is reaped,
/// which can be a few hundred milliseconds after `SIGTERM`/`SIGKILL`, and a
/// `service.json` still naming that process makes a start in that window refuse
/// with "already running" about a service nobody can reach (M10 gate bug 7). So
/// a held lock whose recorded pid is gone is waited out briefly and the lock
/// retried; only a lock a live process still holds is [`ServerError::AlreadyRunning`].
fn take_lock(dir: &Path) -> Result<fs::File, ServerError> {
    let path = dir.join("service.lock");
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(&path)
        .map_err(|err| ServerError::io(&path, err))?;
    let deadline = std::time::Instant::now() + SHUTDOWN_WAIT;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(fs::TryLockError::WouldBlock) => {
                let running = read_record(dir);
                // Two cases are worth waiting out, and both are bounded by
                // `SHUTDOWN_WAIT`:
                //
                // * the record names a process that is gone, which is a service
                //   that was just killed and whose flock the kernel has not
                //   released yet (M10 gate bug 7);
                // * there is no record at all. Either the holder has taken the
                //   lock and not yet written its record, or this process has just
                //   released the same lock and macOS is still cleaning the vnode
                //   up — which reads as held for a moment although nobody holds
                //   it.
                //
                // The retry happens on the handle already open rather than by
                // reopening the file, so the wait cannot be defeated by a fresh
                // handle racing the same cleanup.
                let unclear = running
                    .as_ref()
                    .is_none_or(|record| !process_alive(record.pid));
                if unclear && std::time::Instant::now() < deadline {
                    std::thread::sleep(SHUTDOWN_POLL);
                    continue;
                }
                return Err(match running {
                    Some(record) if !process_alive(record.pid) => ServerError::ShuttingDown {
                        dir: dir.to_path_buf(),
                        port: Some(record.port),
                        pid: Some(record.pid),
                    },
                    Some(record) => ServerError::AlreadyRunning {
                        dir: dir.to_path_buf(),
                        port: Some(record.port),
                        pid: Some(record.pid),
                    },
                    // A lock nobody's record names and that nothing explains:
                    // it is either a service still starting or one whose record
                    // went away, and either way "try again in a moment" is the
                    // truth and "already running (pid ?)" is not.
                    None => ServerError::ShuttingDown {
                        dir: dir.to_path_buf(),
                        port: None,
                        pid: None,
                    },
                });
            }
            Err(fs::TryLockError::Error(err)) => return Err(ServerError::io(&path, err)),
        }
    }
}

/// The discovery record in `dir`, when one is there and parses.
///
/// A half-written or unreadable record is no record: the next start overwrites
/// it, and a reader that could not parse it cannot name a port anyway.
fn read_record(dir: &Path) -> Option<ServiceRecord> {
    let text = fs::read_to_string(dir.join("service.json")).ok()?;
    serde_json::from_str(&text).ok()
}

/// Whether the process `pid` is still there.
///
/// It is asked of the kernel with signal `0`, which checks for the process
/// without sending it anything. `libc` is not a dependency — the same reason
/// [`crate::service`] waves `fs::File::try_lock` instead of `flock` — so this
/// goes through the system `kill(1)`, at an absolute path so nothing planted on
/// `PATH` can decide whether a service is running. A machine with no `kill`, or
/// a `kill` that cannot tell, answers "still alive": a start that refused when
/// it could have taken over is a retry, while one that took over a live
/// service's directory is two services over one log.
fn process_alive(pid: u32) -> bool {
    let Some(program) = ["/bin/kill", "/usr/bin/kill"]
        .into_iter()
        .map(Path::new)
        .find(|path| path.is_file())
    else {
        return true;
    };
    // `kill -0 -- <pid>`: the `--` ends option parsing, so a pid is never read as
    // a signal, and the status says whether the process exists.
    match std::process::Command::new(program)
        .args(["-0", "--", &pid.to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
    {
        Ok(status) => status.success(),
        Err(_) => true,
    }
}

/// Reads a token a previous start wrote, if it is usable.
///
/// An empty file is not a secret, so it counts as no token and is replaced; a
/// file that is there is re-asserted private before its contents are trusted.
fn read_token(path: &Path) -> Result<Option<String>, ServerError> {
    match fs::read_to_string(path) {
        Ok(text) => {
            let token = text.trim().to_string();
            if token.is_empty() {
                return Ok(None);
            }
            set_private(path)?;
            Ok(Some(token))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(ServerError::io(path, err)),
    }
}

/// Creates `dir` and makes it private.
fn create_private_dir(dir: &Path) -> Result<(), ServerError> {
    fs::create_dir_all(dir).map_err(|err| ServerError::io(dir, err))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::Permissions::from_mode(0o700);
        fs::set_permissions(dir, mode).map_err(|err| ServerError::io(dir, err))?;
    }
    Ok(())
}

/// Writes `bytes` to `path`, creating it private and truncating it.
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), ServerError> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|err| ServerError::io(path, err))?;
    file.write_all(bytes)
        .map_err(|err| ServerError::io(path, err))?;
    file.sync_all().map_err(|err| ServerError::io(path, err))?;
    set_private(path)
}

/// Re-asserts a private file mode.
fn set_private(path: &Path) -> Result<(), ServerError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::Permissions::from_mode(0o600);
        fs::set_permissions(path, mode).map_err(|err| ServerError::io(path, err))?;
    }
    let _ = path;
    Ok(())
}

/// `bytes` bytes of randomness, as lowercase hex.
///
/// `/dev/urandom` is the one source every unix has; the token guards a loopback
/// port, not a cipher, and it is 256 bits of it.
fn random_hex(bytes: usize) -> Result<String, ServerError> {
    let mut buffer = vec![0u8; bytes];
    let mut source = fs::File::open("/dev/urandom").map_err(|err| ServerError::Secret {
        reason: err.to_string(),
    })?;
    source
        .read_exact(&mut buffer)
        .map_err(|err| ServerError::Secret {
            reason: err.to_string(),
        })?;
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes * 2);
    for byte in buffer {
        text.push(char::from(HEX[usize::from(byte >> 4)]));
        text.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Ok(text)
}

/// What the service tells the binary (and a client) about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceInfo {
    /// The port the socket is bound to.
    pub port: u16,
    /// This start's identity.
    pub id: String,
    /// The service's version.
    pub version: String,
    /// The process that is serving.
    pub pid: u32,
    /// Where the bearer token is.
    pub token_path: PathBuf,
    /// Where the discovery record is.
    pub service_json: PathBuf,
}

/// Everything a service is started with.
#[derive(Debug, Clone)]
pub struct Config {
    /// The repository whose run log the service reads and writes.
    pub repo: PathBuf,
    /// The state directory holding `service.json` and the token.
    pub state_dir: PathBuf,
    /// The loopback port to listen on; `0` picks a free one.
    pub port: u16,
    /// Who the service's actions act as.
    pub actor: shprd_core::event::Actor,
    /// A step to run after every action that appended an event.
    pub after_write: Option<AfterWrite>,
    /// The change counter the API and the tick share.
    ///
    /// Both write the one log, so both wake the one set of waiters: a task the
    /// tick started reaches an open page exactly as a task added through the API
    /// does.
    pub hub: crate::hub::Hub,
    /// The tick the service runs beside the API, when the binary supplies one.
    ///
    /// The service's job is to advance the world by itself, so this is what
    /// makes `shprd serve` more than an HTTP front end. A service prepared
    /// without it — every test that only drives the router — answers requests
    /// and advances nothing.
    pub tick: Option<TickConfig>,
    /// The home directory the settings routes read and write, when the caller
    /// resolved one.
    ///
    /// The binary resolves it once (`--home`, then `$SHPRD_HOME`, then `$HOME`)
    /// and a test passes its own, so the service never reads the environment to
    /// find it. `None` leaves the settings routes refusing.
    pub home: Option<PathBuf>,
    /// The prompts directory the API reads and writes, when the caller resolved
    /// one.
    ///
    /// The binary resolves it from the configuration root once
    /// (`$XDG_CONFIG_HOME`, else `<home>/.config`) and a test passes its own, so
    /// the service never reads `$XDG_CONFIG_HOME` itself.
    pub prompts_dir: Option<PathBuf>,
    /// The shipped data the settings checks read the harness table from, when
    /// the caller resolved one.
    ///
    /// The binary resolves `$SHPRD_DATA_DIR`, the person's overrides and the
    /// embedded copy into this once, so the service reads no directory variable
    /// of its own.
    pub data: Option<DataDir>,
    /// The `PATH` a program's own model listing runs with, when the caller
    /// resolved one.
    ///
    /// The roster's pickers ask a program for the models it runs, and a service
    /// that read `$PATH` itself would start whatever the environment it was
    /// launched with names. The binary passes the `PATH` it was started with;
    /// `None` leaves every listing off, so a service prepared by hand reads only
    /// the shipped catalogs.
    pub path_env: Option<String>,
    /// Where the graph's program is, as the binary found it.
    ///
    /// The binary locates dagr once ([`crate::graph::locate`]) and passes the
    /// answer across, so the service answers `GET /api/graph` without reading a
    /// `PATH` or a settings file of its own. `None` is a service nobody told,
    /// which the route says rather than pretending there is no dagr.
    pub dagr: Option<crate::graph::Dagr>,
    /// The binary's spawn and close, so the person's start, retry and stop
    /// actions reach an agent.
    ///
    /// It is the same two closures the tick's steps use: a start writes a brief,
    /// makes a worktree and opens a tab, a stop closes the space the agent runs
    /// in. A service started without them — every test that only drives the
    /// router — refuses those three actions and reads the log exactly the same.
    pub engine: Option<crate::engine::Engine>,
    /// What types a chat line into a space's agent, when the binary has a host.
    ///
    /// Typing into a pane is the host's verb, so the binary passes the same
    /// adapter it drives everywhere else ([`crate::ChatWriter`]). A service
    /// started without one answers `POST /api/chat` with a refusal rather than
    /// pretending the line went somewhere.
    pub chat: crate::chat::ChatHost,
    /// What the terminal socket watches and types through, when the binary has a
    /// host. A service started without one refuses `GET /api/terminal`.
    pub terminal: crate::terminal::TerminalPort,
    /// What starts a program in a new tab, when the binary has a host.
    ///
    /// "Start an agent" is a tab and a CLI, so the binary passes the adapter that
    /// owns the harness table and the host ([`crate::AgentOpener`]). A service
    /// started without one answers `POST /api/agents` with a refusal rather than
    /// pretending a tab exists.
    pub agents: crate::agents::Agents,
    /// The home and data home the agent CLIs run with, when the binary resolved
    /// them.
    ///
    /// The binary passes the environment its launches will inherit, so the chat
    /// reads a CLI's own records where that CLI wrote them
    /// ([`shprd_agents::transcript::Roots`]) and never under the engine's
    /// `SHPRD_HOME`. `None` is a service nobody told, which the chat's route
    /// says rather than looking in the wrong directory.
    pub transcript_roots: Option<shprd_agents::transcript::Roots>,
    /// The hostnames this service answers to besides loopback.
    ///
    /// The tunnel task adds its hostname here when a tunnel starts and removes it
    /// when it stops ([`shprd_remote::AllowedHosts`]); the gate reads the set on
    /// every request, and an unknown `Host` is a `421`. The default is loopback
    /// only, so a service nothing put on the internet answers to nothing else.
    pub allowed_hosts: shprd_remote::AllowedHosts,
}

/// Told what a pass did, after it did it, for the caller's own output.
///
/// `shprd serve` uses it to say the run exists when the service was started
/// before `shprd run start`: the banner is printed once, so without a notice a
/// service started too early would look like an empty run for ever. The service
/// itself prints nothing; whoever wants to say something passes the words.
pub type PassNotice = std::sync::Arc<dyn Fn(&crate::tick::TickReport) + Send + Sync>;

/// How the service runs its tick.
#[derive(Clone)]
pub struct TickConfig {
    /// The worker one pass runs against.
    pub worker: std::sync::Arc<crate::tick::Worker>,
    /// How long to wait between passes.
    pub interval: std::time::Duration,
    /// How many tasks of this repository may run at once, for the startup line.
    pub max_running: u32,
    /// Told what every pass did, when the caller wants to say something about
    /// it.
    pub notice: Option<PassNotice>,
}

impl std::fmt::Debug for TickConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TickConfig")
            .field("interval", &self.interval)
            .finish_non_exhaustive()
    }
}

impl Config {
    /// A config for `repo`, with the discovery files in `state_dir`.
    ///
    /// The hub it starts with touches `<state_dir>/changes`, so everything this
    /// service writes from then on is visible to a Herdr mod watching that
    /// folder. A caller that builds its own hub keeps its own file (or none).
    pub fn new(repo: impl Into<PathBuf>, state_dir: impl Into<PathBuf>) -> Self {
        let state_dir = state_dir.into();
        Self {
            repo: repo.into(),
            hub: crate::hub::Hub::touching(state_dir.clone()),
            state_dir,
            port: 0,
            actor: shprd_core::event::Actor::Human,
            after_write: None,
            tick: None,
            home: None,
            prompts_dir: None,
            data: None,
            path_env: None,
            dagr: None,
            engine: None,
            chat: crate::chat::ChatHost::new(),
            terminal: crate::terminal::TerminalPort::new(),
            agents: crate::agents::Agents::new(),
            transcript_roots: None,
            allowed_hosts: shprd_remote::AllowedHosts::loopback_only(),
        }
    }

    /// The names this service answers to, as a shared handle.
    ///
    /// The tunnel task keeps the handle and adds or removes its hostname; the
    /// running service reads the set on every request.
    pub fn allowed_hosts(&self) -> shprd_remote::AllowedHosts {
        self.allowed_hosts.clone()
    }
}

impl Service {
    /// The names this running service answers to, as a shared handle.
    pub fn allowed_hosts(&self) -> shprd_remote::AllowedHosts {
        self.app.allowed_hosts()
    }
}

/// A service whose socket is bound and whose discovery files are written.
#[derive(Debug)]
pub struct Service {
    app: App,
    listener: TcpListener,
    info: ServiceInfo,
    tick: Option<TickConfig>,
}

impl Service {
    /// Binds the listener and writes the discovery files.
    ///
    /// `service.json` records the port the kernel chose, so it is written after
    /// the bind, never before it.
    pub async fn start(config: Config) -> Result<Self, ServerError> {
        let address = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, config.port));
        let listener = TcpListener::bind(address)
            .await
            .map_err(|source| ServerError::Bind { address, source })?;
        let port = listener
            .local_addr()
            .map_err(ServerError::NoAddress)?
            .port();
        let app = App::prepare_all(
            &config.repo,
            port,
            &config.actor,
            &config.state_dir,
            config.after_write.clone(),
            config.hub.clone(),
            // Everything the service reads from the machine was resolved by the
            // binary and carried here, so the service never consults the
            // environment for a path of its own.
            Paths {
                home: config.home.clone(),
                prompts_dir: config.prompts_dir.clone(),
                data: config.data.clone(),
                path_env: config.path_env.clone(),
                dagr: config.dagr.clone(),
                transcript_roots: config.transcript_roots.clone(),
                allowed_hosts: config.allowed_hosts.clone(),
            },
        )?
        // The spawn and the close are the binary's, wired exactly as the tick's
        // steps are; a service without them refuses the three actions that need
        // a host.
        .with_engine_opt(config.engine.clone())
        // The chat's writer is that same host. Attaching it rebuilds the router,
        // which a client never sees: the record is written before either is
        // wired, and a request cannot arrive until the listener is serving.
        .with_chat_opt(config.chat.get().cloned())
        .with_terminal_opt(config.terminal.get().cloned())
        // The "+" menu's *Start an agent* is that host too: one adapter, so a
        // program the tab bar starts and a chat line type into the same window.
        .with_agent_opener_opt(config.agents.get().cloned());
        let info = app.info();
        let tick = config.tick.clone();
        tracing::info!(port, id = %info.id, "shprd service listening on loopback");
        // The page is served from this same port. Saying whether this binary
        // embedded a build is the one thing a person opening it needs to know
        // when there is no app to see.
        if crate::web::built() {
            tracing::info!(port, "the web app is embedded");
        } else {
            tracing::warn!(
                port,
                "this build has no web app; run `npm ci && npm run build` in web/, then build shprd again"
            );
        }
        Ok(Self {
            app,
            listener,
            info,
            tick,
        })
    }

    /// What the service tells the world about itself.
    pub fn info(&self) -> &ServiceInfo {
        &self.info
    }

    /// Serves until `shutdown` resolves, then removes `service.json`.
    ///
    /// The discovery file is removed on a clean shutdown only; a crashed service
    /// leaves it behind, and the next start overwrites it.
    pub async fn run(
        self,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> Result<(), ServerError> {
        let tick = self.tick.clone();
        self.run_with_tick(shutdown, tick).await
    }

    /// Serves, running `tick` beside the API until `shutdown`.
    ///
    /// The tick is one more task in the same runtime, so stopping the service
    /// stops it: a pass in flight finishes, and no pass starts after the
    /// shutdown resolves.
    pub async fn run_with_tick(
        self,
        shutdown: impl Future<Output = ()> + Send + 'static,
        tick: Option<TickConfig>,
    ) -> Result<(), ServerError> {
        let Self {
            app,
            listener,
            info,
            tick: _,
        } = self;
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        // Writers outside this process — a program's ask hook, the CLI — append
        // to the same log. Watching its stamp is what wakes a long poll for an
        // ask raised that way, so a card arrives without waiting for a client
        // to poll the file.
        let watcher = tokio::spawn(watch_log(app.repo().to_path_buf(), app.hub()));
        let ticker = tick.map(|tick| {
            let worker = std::sync::Arc::clone(&tick.worker);
            let interval = tick.interval;
            let notice = tick.notice.clone();
            tokio::spawn(async move {
                crate::tick::run(worker, interval, notice, async {
                    // The service's own shutdown is awaited here, so the
                    // loop ends with it rather than with the socket.
                    let _ = stopped.await;
                })
                .await;
            })
        });
        // The peer address is the kernel's, and it is what every rate limit is
        // keyed on: `X-Forwarded-For` is a header a client writes, and is ignored
        // until the tunnel task decides when it may be trusted.
        let served = axum::serve(
            listener,
            app.router()
                .into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(shutdown)
        .await;
        let _ = stop.send(());
        watcher.abort();
        // The tunnel dies with the service: stopped here, so a Tailscale mapping
        // is removed too (a child process would also die with the runtime, but
        // `tailscale serve` outlives its command).
        app.tunnels().shutdown().await;
        if let Some(ticker) = ticker {
            let _ = ticker.await;
        }
        let removed = app.remove_record();
        tracing::info!(port = info.port, "shprd service stopped");
        served.map_err(ServerError::Serve)?;
        removed
    }
}

/// Publishes every log change a writer outside this process makes.
///
/// The log is re-read only when its stamp changes, so a quiet run costs a
/// `stat` per tick; the publish is what wakes long polls and sockets.
async fn watch_log(repo: std::path::PathBuf, hub: crate::hub::Hub) {
    let mut seen: Option<crate::ledger::Stamp> = None;
    let mut last = 0;
    loop {
        let current = crate::ledger::stamp(&repo).ok().flatten();
        if current != seen {
            seen = current;
            if let Ok(seq) = crate::ledger::last_seq(&repo)
                && seq > last
            {
                last = seq;
                hub.publish(seq);
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

/// Starts a service, serves it until `shutdown`, and reports what it was.
pub async fn serve(
    config: Config,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<ServiceInfo, ServerError> {
    let service = Service::start(config).await?;
    let info = service.info().clone();
    service.run(shutdown).await?;
    Ok(info)
}

/// Resolves when the process is asked to stop: `SIGINT` or `SIGTERM`.
///
/// The handlers are installed when this is *called*, not when the future is
/// first polled, so a caller that takes the future before it announces itself
/// (writes its record) can be signalled at once and still stop quietly rather
/// than die of the default action. It must be called inside a Tokio runtime.
pub fn shutdown_signal() -> impl Future<Output = ()> + Send + 'static {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let interrupt = signal(SignalKind::interrupt());
        let terminate = signal(SignalKind::terminate());
        async move {
            let wait = |listener: std::io::Result<tokio::signal::unix::Signal>,
                        name: &'static str| async move {
                match listener {
                    Ok(mut signal) => {
                        signal.recv().await;
                    }
                    Err(err) => {
                        tracing::warn!(%err, "could not listen for {name}");
                        std::future::pending::<()>().await;
                    }
                }
            };
            tokio::select! {
                _ = wait(interrupt, "SIGINT") => {}
                _ = wait(terminate, "SIGTERM") => {}
            }
        }
    }
    #[cfg(not(unix))]
    {
        async {
            let _ = tokio::signal::ctrl_c().await;
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn a_token_is_made_once_and_kept_private() {
        let dir = tempfile::tempdir().expect("temp dir");
        let first = ServiceFiles::prepare(dir.path()).expect("prepare");
        let token = first.token().to_string();
        let id = first.id().to_string();
        // A restart is the next start after this one stopped: it keeps the
        // token, and takes a new identity.
        drop(first);
        let second = ServiceFiles::prepare(dir.path()).expect("prepare again");
        assert_eq!(token, second.token(), "the token is reused");
        assert_eq!(second.token().len(), 64);
        assert_ne!(id, second.id(), "the identity is per start");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(second.token_path())
                .expect("token metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn the_record_round_trips_and_is_removed() {
        let dir = tempfile::tempdir().expect("temp dir");
        let files = ServiceFiles::prepare(dir.path()).expect("prepare");
        let record = files.write_record(4321).expect("write");
        assert_eq!(record.port, 4321);
        assert_eq!(record.id, files.id());
        assert_eq!(record.version, VERSION);
        let text = fs::read_to_string(files.service_json_path()).expect("read record");
        let parsed: ServiceRecord = serde_json::from_str(&text).expect("parse record");
        assert_eq!(parsed, record);
        files.remove_record().expect("remove");
        assert!(!files.service_json_path().exists());
        files.remove_record().expect("removing twice is fine");
    }

    #[test]
    fn a_second_service_cannot_share_a_state_directory() {
        let dir = tempfile::tempdir().expect("temp dir");
        let first = ServiceFiles::prepare(dir.path()).expect("the first start takes the lock");
        // The lock-holder writes its record, as a start does after it binds: the
        // record is what names the process the second start refuses on.
        first.write_record(1234).expect("write the record");
        match ServiceFiles::prepare(dir.path()) {
            Err(ServerError::AlreadyRunning { dir: reported, .. }) => {
                assert_eq!(reported, dir.path());
            }
            Err(other) => panic!("expected AlreadyRunning, got {other}"),
            Ok(_) => panic!("a second service must not take a held state directory"),
        }
        // The first one still owns it, and dropping it frees it for the next.
        assert!(first.owns_record() || !first.service_json_path().exists());
        drop(first);
        ServiceFiles::prepare(dir.path()).expect("the lock is released when the service ends");
    }

    #[test]
    fn a_record_naming_a_dead_process_is_not_a_reason_to_refuse_a_start() {
        // Bug 7 of the M10 gate: after `pkill -f "shprd serve"` a start within a
        // second or two was refused with "already running (pid 57685)" — the
        // flock is still held by the dying process, and `service.json` still
        // names it. The rule is to look at the process, not just the lock.
        let dir = tempfile::tempdir().expect("temp dir");
        let dead = dead_pid();
        let record = ServiceRecord {
            port: 59229,
            id: String::from("a-start-that-died"),
            pid: dead,
            version: VERSION.to_string(),
        };
        let path = dir.path().join("service.json");
        fs::write(
            &path,
            serde_json::to_string(&record).expect("serialize the record"),
        )
        .expect("write the stale record");

        // The lock the dying process still holds is modelled by holding it here
        // in the same process: the record is what says whose it is, and that pid
        // is gone, so the start waits the lock out instead of believing it.
        let held = take_lock_holding(dir.path());
        let started = std::thread::spawn({
            let dir = dir.path().to_path_buf();
            move || ServiceFiles::prepare(&dir)
        });
        std::thread::sleep(std::time::Duration::from_millis(120));
        drop(held);
        let taken = started.join().expect("the start finished");
        let taken = taken.expect("a start takes a directory whose service died");
        assert!(!taken.owns_record(), "the stale record is not this start's");
    }

    #[test]
    fn a_start_whose_service_is_still_shutting_down_says_so() {
        // The other half of bug 7: the process is gone and the lock has not been
        // released yet, so the message must say a stop is in flight rather than
        // claim a service is running. The wait is bounded, so the refusal comes.
        let dir = tempfile::tempdir().expect("temp dir");
        let record = ServiceRecord {
            port: 59229,
            id: String::from("a-start-that-is-dying"),
            pid: dead_pid(),
            version: VERSION.to_string(),
        };
        fs::write(
            dir.path().join("service.json"),
            serde_json::to_string(&record).expect("serialize the record"),
        )
        .expect("write the record");
        let held = take_lock_holding(dir.path());
        match ServiceFiles::prepare(dir.path()) {
            Err(ServerError::ShuttingDown {
                dir: reported, pid, ..
            }) => {
                assert_eq!(reported, dir.path());
                assert_eq!(pid, Some(record.pid));
            }
            Err(other) => panic!("expected ShuttingDown, got {other}"),
            Ok(_) => panic!("a held lock is not a free directory"),
        }
        drop(held);
        ServiceFiles::prepare(dir.path()).expect("once the lock is gone the start works");
    }

    #[test]
    fn a_record_naming_a_live_process_still_refuses_a_start() {
        // The guard on the takeover: a live pid is a service that is running, and
        // a start must not take its directory out from under it.
        let dir = tempfile::tempdir().expect("temp dir");
        let record = ServiceRecord {
            port: 4321,
            id: String::from("a-live-start"),
            pid: std::process::id(),
            version: VERSION.to_string(),
        };
        fs::write(
            dir.path().join("service.json"),
            serde_json::to_string(&record).expect("serialize the record"),
        )
        .expect("write the record");
        let held = take_lock_holding(dir.path());
        match ServiceFiles::prepare(dir.path()) {
            Err(ServerError::AlreadyRunning { pid, .. }) => {
                assert_eq!(pid, Some(record.pid), "the refusal names the live pid");
            }
            Err(other) => panic!("expected AlreadyRunning, got {other}"),
            Ok(_) => panic!("a live service's directory must not be taken"),
        }
        drop(held);
    }

    /// A pid no process has: the largest one plus one is not one the kernel
    /// hands out, and asking the kernel about it says so.
    fn dead_pid() -> u32 {
        // A pid this process cannot have (it is one process), so nothing here can
        // be mistaken for the live case.
        let candidate = std::process::id().wrapping_add(1_000_000) | 1;
        assert!(!process_alive(candidate), "{candidate} must look dead");
        candidate
    }

    /// Takes the state directory's lock, and answers the file holding it.
    fn take_lock_holding(dir: &Path) -> fs::File {
        let path = dir.join("service.lock");
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        let file = options.open(&path).expect("open the lock file");
        file.try_lock().expect("take the lock");
        file
    }

    #[test]
    fn a_stop_only_removes_a_record_that_names_this_start() {
        let dir = tempfile::tempdir().expect("temp dir");
        let files = ServiceFiles::prepare(dir.path()).expect("prepare");
        files.write_record(1234).expect("write");
        // Another start takes the record over, as a second service would.
        let other = ServiceFiles::prepare(dir.path()).expect_err("the lock is held");
        assert!(matches!(other, ServerError::AlreadyRunning { .. }));

        // A record another process wrote is left alone.
        let text = serde_json::json!({
            "port": 9999, "id": "not-this-start", "pid": 1, "version": VERSION,
        });
        fs::write(files.service_json_path(), text.to_string()).expect("overwrite");
        assert!(!files.owns_record());
        files
            .remove_record()
            .expect("a foreign record is not a failure");
        assert!(
            files.service_json_path().exists(),
            "another service's record must survive this one's stop"
        );

        // Its own record goes.
        files.write_record(1234).expect("write again");
        assert!(files.owns_record());
        files.remove_record().expect("remove");
        assert!(!files.service_json_path().exists());
    }

    #[test]
    fn the_state_home_honours_the_xdg_setting_it_is_given() {
        // The production rule, with the environment's value handed in: no test
        // reads the process's own `$XDG_STATE_HOME`.
        let home = Path::new("/home/example");
        assert_eq!(
            state_dir(&state_home(home, None)),
            home.join(".local").join("state").join("shprd")
        );
        assert_eq!(
            state_dir(&state_home(home, Some(std::ffi::OsStr::new("/xdg/state")))),
            Path::new("/xdg/state").join("shprd")
        );
        assert_eq!(
            state_dir(&state_home(home, Some(std::ffi::OsStr::new("")))),
            home.join(".local").join("state").join("shprd"),
            "an empty setting is unset"
        );
    }

    #[tokio::test]
    async fn the_service_binds_loopback_and_removes_its_record_on_shutdown() {
        let state = tempfile::tempdir().expect("state dir");
        let repo = tempfile::tempdir().expect("repo");
        let config = Config::new(repo.path(), state.path());
        let service = Service::start(config).await.expect("start");
        let info = service.info().clone();
        assert!(info.port > 0);
        assert!(info.token_path.exists());
        assert!(info.service_json.exists());
        let record: ServiceRecord =
            serde_json::from_str(&fs::read_to_string(&info.service_json).expect("read"))
                .expect("parse");
        assert_eq!(record.port, info.port);
        assert_eq!(record.pid, std::process::id());
        service.run(async {}).await.expect("serve and stop");
        assert!(
            !info.service_json.exists(),
            "service.json is removed on a clean shutdown"
        );
    }
}
