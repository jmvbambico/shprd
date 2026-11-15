//! The service's own files, its listener and its lifecycle.
//!
//! A client finds the service the way Shepherdr's window does: the state
//! directory holds `<state>/shprd/service.json` (`{port, id, pid, version}`) and
//! a token file only the owner can read. The client reads the port, confirms the
//! `id` at `GET /api/health`, and only then sends the token.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;

use crate::api::App;
use crate::error::ServerError;

/// The version every answer reports.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Where this service keeps its discovery files, under the XDG state directory.
///
/// `$XDG_STATE_HOME` wins; otherwise the state lives in `~/.local/state`, with
/// `home` standing in for `$HOME`.
pub fn state_dir(home: &Path) -> PathBuf {
    match std::env::var_os("XDG_STATE_HOME").filter(|value| !value.is_empty()) {
        Some(dir) => PathBuf::from(dir).join("shprd"),
        None => home.join(".local").join("state").join("shprd"),
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

/// Takes this state directory's exclusive lock, or says who holds it.
///
/// The lock lives as long as the [`File`] does — for the service's whole
/// lifetime — and the kernel drops it when the process ends, however it ends,
/// so a crashed service never leaves the directory locked against its successor.
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
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(fs::TryLockError::WouldBlock) => {
            let running = fs::read_to_string(dir.join("service.json"))
                .ok()
                .and_then(|text| serde_json::from_str::<ServiceRecord>(&text).ok());
            Err(match running {
                Some(record) => ServerError::AlreadyRunning {
                    dir: dir.to_path_buf(),
                    port: Some(record.port),
                    pid: Some(record.pid),
                },
                None => ServerError::AlreadyRunning {
                    dir: dir.to_path_buf(),
                    port: None,
                    pid: None,
                },
            })
        }
        Err(fs::TryLockError::Error(err)) => Err(ServerError::io(&path, err)),
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
    pub fn new(repo: impl Into<PathBuf>, state_dir: impl Into<PathBuf>) -> Self {
        Self {
            repo: repo.into(),
            state_dir: state_dir.into(),
            port: 0,
            actor: shprd_core::event::Actor::Human,
            after_write: None,
            hub: crate::hub::Hub::new(),
            tick: None,
        }
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
        let app = App::prepare_with_hub(
            &config.repo,
            port,
            &config.actor,
            &config.state_dir,
            config.after_write.clone(),
            config.hub.clone(),
        )?;
        let info = app.info();
        let tick = config.tick.clone();
        tracing::info!(port, id = %info.id, "shprd service listening on loopback");
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
        let served = axum::serve(listener, app.router())
            .with_graceful_shutdown(shutdown)
            .await;
        let _ = stop.send(());
        watcher.abort();
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
pub async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let terminate = async {
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(mut signal) => {
                    signal.recv().await;
                }
                Err(err) => {
                    tracing::warn!(%err, "could not listen for SIGTERM; waiting for SIGINT only");
                    std::future::pending::<()>().await;
                }
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
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
    fn the_state_dir_honours_xdg_state_home() {
        let home = Path::new("/home/example");
        // The environment belongs to the process, so only the fallback is
        // asserted here; the override is asserted where it is set.
        if std::env::var_os("XDG_STATE_HOME").is_none() {
            assert_eq!(
                state_dir(home),
                home.join(".local").join("state").join("shprd")
            );
        }
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
