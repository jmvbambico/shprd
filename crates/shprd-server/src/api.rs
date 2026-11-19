//! The HTTP surface: one route per read, write and notice, one of them a socket.
//!
//! `GET /api/health` is the only unauthenticated one — it is the handshake, not
//! a secret. Everything else is behind the gate in `auth`. Reads answer from the
//! fold (a snapshot or a change notice), except `GET /api/settings`, which
//! answers the settings files as they resolve (`crate::settings`), and
//! `GET /api/roster`, which answers the programs a role may run
//! (`crate::roster`), and `GET /api/tasks/{id}/changes`, which reads git
//! (`crate::tasks`). The writes are `POST /api/action`, which hands one
//! [`Action`] to `actions`, `POST /api/tasks`, which adds one task,
//! `PUT /api/settings`, which writes one settings key, and the prompt routes,
//! which store or reset one template's words.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Bytes;
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade, close_code};
use axum::extract::{DefaultBodyLimit, Path as UrlPath, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Extension, Json, middleware};
use serde::Deserialize;
use serde_json::Value;
use shprd_agents::data::DataDir;
use shprd_core::event::Actor;
use shprd_core::log::{Clock, SystemClock};
use tower_http::trace::TraceLayer;

use crate::actions;
use crate::agents::AgentOpener;
use crate::auth;
use crate::chat::{self, ChatHost, ChatWriter, Space};
use crate::dto::{
    Action, ActionReply, AgentTabDto, AnswerAsk, AskListDto, AskReply, ChangeDto, ChatDto,
    ChatSendReply, ClientMessage, ErrorCode, GraphDto, HealthDto, PromptDto, PromptListDto,
    RosterDto, SettingsDto, SettingsWriteDto, SnapshotDto, WsMessage,
};
use crate::engine::Engine;
use crate::error::{ApiReject, ServerError};
use crate::hub::Hub;
use crate::ledger::{self, Stamp};
use crate::prompts as prompts_api;
use crate::remote as login;
use crate::roster as roster_api;
use crate::service::{AfterWrite, ServiceFiles, ServiceInfo, VERSION};
use crate::settings as settings_api;
use crate::tasks;
use crate::tunnels;

/// The largest body any route accepts, in bytes.
///
/// A JSON request is small; a file upload will get its own, larger limit when it
/// exists. The bound is here because the same listener may be on a tunnel: a
/// request with no limit is a request that can fill memory (M7d §4.4, "body
/// size").
pub const BODY_LIMIT: usize = 1024 * 1024;

/// How long a change request waits when it does not say.
const DEFAULT_WAIT: Duration = Duration::from_secs(10);

/// The longest a change request may wait, so a client cannot pin a task for
/// ever.
const MAX_WAIT: Duration = Duration::from_secs(30);

/// How often a waiter re-checks the log on its own, in case a wake-up was
/// missed or another process appended.
const TICK: Duration = Duration::from_millis(200);

/// How long a socket may live, and how often the server pings it.
///
/// A socket is long-lived by design, so it is bounded instead: a page that goes
/// away without a close (a phone losing signal) must not hold a task for hours,
/// and a probe run against the route by hand must end rather than wait for
/// ever. Every frame the client sends — a text message, a ping, the pong to our
/// own ping — counts as activity, so a live page never trips [`WsLimits::idle`];
/// the session bound is the backstop for a client that keeps answering.
///
/// The defaults are a bounded version of the researched design's ([Shepherdr's
/// `idleTimeout: 600`][sh]). A browser answers a ping itself, within
/// milliseconds, so a page that is open is heard from constantly and never
/// trips [`WsLimits::idle`]; a client that is *not* a WebSocket — a probe with
/// hand-made upgrade headers, a page that went away without closing — never
/// answers, and is gone after thirty seconds of silence rather than holding a
/// connection for ever. No socket lives past five minutes either, so a
/// connection is never permanent; a client that reconnects (the web app does)
/// sees nothing but a short gap.
///
/// [sh]: https://github.com/eliasstravik/shepherdr/blob/a3f327a52354cd2880cdacc29716dd07bcd11efb/service/main.ts#L930
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WsLimits {
    /// How often the server sends a ping on a quiet socket.
    pub ping: Duration,
    /// How long a socket may go without a client frame before it is closed.
    pub idle: Duration,
    /// How long a socket may live at all.
    pub session: Duration,
}

impl Default for WsLimits {
    fn default() -> Self {
        Self {
            ping: Duration::from_secs(10),
            idle: Duration::from_secs(30),
            session: Duration::from_secs(300),
        }
    }
}

/// What every handler sees.
#[derive(Debug, Clone)]
pub(crate) struct AppState {
    /// The repository whose run log this service serves.
    pub(crate) repo: PathBuf,
    /// The port the listener bound, which is what "same origin" means.
    pub(crate) port: u16,
    /// The bearer token every route behind the gate accepts.
    pub(crate) token: String,
    /// This start's identity.
    pub(crate) id: String,
    /// The service's version.
    pub(crate) version: String,
    /// Who the actions act as.
    pub(crate) actor: Actor,
    /// The change counter and its wake-up.
    pub(crate) hub: Hub,
    /// The binary's own step after a write, if it supplied one.
    pub(crate) after_write: Option<AfterWrite>,
    /// The bounds a socket lives under.
    pub(crate) ws: WsLimits,
    /// The home directory the settings files live in, when the caller set one.
    ///
    /// Nothing here reads `$SHPRD_HOME` or `$HOME`: the binary resolves the home
    /// once ([`App::with_home`]) and a test passes its own, so the service can
    /// never read or write a directory it was not pointed at.
    pub(crate) home: Option<PathBuf>,
    /// The state directory holding `service.json`, the token and the last pass's
    /// outcome ([`crate::pass`]).
    pub(crate) state_dir: PathBuf,
    /// The shipped data the harness table is read from, when this service was
    /// given one. Only the settings checks need it.
    pub(crate) data: Option<DataDir>,
    /// The binary's spawn and close, when this service was given them.
    ///
    /// Three of the person's actions — a start, a retry and a stop — are not an
    /// event but the host's business, so the binary passes the same closures a
    /// tick's steps use ([`crate::engine`]). A service without them refuses
    /// those actions rather than pretending it started an agent.
    pub(crate) engine: Option<Engine>,
    /// The prompts directory, when the caller set one.
    ///
    /// The binary resolves the configuration root once (where `$XDG_CONFIG_HOME`
    /// wins over `<home>/.config`) and sets it ([`App::with_prompts_dir`]), and a
    /// test always sets it, so neither the service nor a test ever resolves a
    /// path from the process's own XDG settings.
    pub(crate) prompts_dir: Option<PathBuf>,
    /// The `PATH` a program's own model listing runs with, when the caller
    /// resolved one.
    ///
    /// The binary passes the `PATH` it was started with, because the roster's
    /// pickers ask a program for the models it runs. A service built by hand — a
    /// test, for one — leaves it unset, and then only the shipped catalogs are
    /// read: a request never starts a program this service was not told how to
    /// find.
    pub(crate) path_env: Option<String>,
    /// Where the graph's program is, as the binary found it.
    ///
    /// The binary is the one place that reads `graph.dagr`, `PATH` and the
    /// herdr plugin folder ([`crate::graph::locate`]), so the service answers
    /// `GET /api/graph` from a value rather than probing a machine it was not
    /// pointed at. `None` is "nobody told this service", which the route says
    /// out loud.
    pub(crate) dagr: Option<crate::graph::Dagr>,
    /// What types a chat line into a space's agent, when the caller has a host.
    ///
    /// The service reads a transcript by itself (it is a file), but typing into
    /// a pane is the host's verb, so the binary passes it in
    /// ([`App::with_chat_writer`]) exactly as it passes the tick's spawn path.
    /// A service without one answers `POST /api/chat` with a refusal rather
    /// than pretending the line went somewhere.
    pub(crate) chat: ChatHost,
    /// What watches and types into a pane for the terminal socket, when the
    /// caller has a host ([`App::with_terminal_host`]).
    pub(crate) terminal: crate::terminal::TerminalPort,
    /// The login, the sessions and the gate that stands in front of a tunnel.
    ///
    /// It is always present: the gate is what decides whether a request has a
    /// credential at all, and a service with no [`shprd_remote::Gateway`] would
    /// be one with no rule. A test that wants a fake clock replaces it
    /// ([`App::with_clock`]).
    pub(crate) remote: Arc<shprd_remote::Gateway>,
    /// What starts a program in a new tab, when the caller has a host.
    ///
    /// "Start an agent" in the tab bar's "+" menu is a host verb — a terminal is
    /// opened and a CLI started in it — so the binary passes its adapter in
    /// ([`App::with_agent_opener`]) exactly as it passes the chat writer and the
    /// tick's spawn path. A service without one refuses `POST /api/agents`
    /// rather than pretending a tab exists.
    pub(crate) agents: crate::agents::Agents,
    /// The agents the host lists, headless, for `live_agents`.
    pub(crate) live: crate::live::LiveAgents,
    /// The home and data home the agent CLIs run with, when the caller resolved
    /// them.
    ///
    /// A transcript is the CLI's own file, so a chat reads it under the roots
    /// *that process* had ([`shprd_agents::transcript::Roots`]): the real `HOME`
    /// it inherits and its XDG data home, never the engine's own `SHPRD_HOME`,
    /// which no CLI writes a session under. The empty [`Paths::default`] leaves
    /// it unset, and a chat then says it was not told rather than looking in a
    /// directory that can never hold a session.
    pub(crate) transcript_roots: Option<shprd_agents::transcript::Roots>,
    /// Where the built web app's files come from.
    ///
    /// Production serves the build embedded in this binary ([`crate::web`]); a
    /// test points this at a directory of its own ([`App::with_web_dir`]) so the
    /// SPA fallback and the cache headers are proven without needing `web/dist`
    /// to have been built.
    pub(crate) web: crate::web::Source,
    /// The one tunnel, if there is one: started, watched and stopped here.
    pub(crate) tunnels: shprd_remote::tunnel::TunnelManager,
    /// How a tunnel is built, kept so the manager can be rebuilt when the `PATH`
    /// it looks for programs on is replaced.
    pub(crate) tunnel_factory: Arc<dyn shprd_remote::tunnel::TunnelFactory>,
}

/// The directories the binary resolved for the service.
///
/// They are carried in rather than looked up, so the service reads no
/// environment variable of its own: the binary resolves `--home`/`$SHPRD_HOME`,
/// the configuration root (`$XDG_CONFIG_HOME`, else `<home>/.config`) and the
/// data directory once, and passes them here.
#[derive(Debug, Clone, Default)]
pub(crate) struct Paths {
    /// The home directory the settings files live in, when there is one.
    pub(crate) home: Option<PathBuf>,
    /// The prompts directory, when there is one.
    pub(crate) prompts_dir: Option<PathBuf>,
    /// The shipped data the harness table is read from, when there is one.
    pub(crate) data: Option<DataDir>,
    /// The `PATH` a program's own model listing runs with, when there is one.
    pub(crate) path_env: Option<String>,
    /// Where the graph's program is, as the binary found it.
    pub(crate) dagr: Option<crate::graph::Dagr>,
    /// The home and data home the agent CLIs run with, when there is one.
    pub(crate) transcript_roots: Option<shprd_agents::transcript::Roots>,
    /// The hostnames this service answers to besides loopback.
    ///
    /// The binary passes the set a tunnel added; a test passes its own. The
    /// empty set is loopback only, which is a service nobody put on the internet.
    pub(crate) allowed_hosts: shprd_remote::AllowedHosts,
}

/// One prepared service: its state, its router and its discovery files.
///
/// It is the seam a test drives without a socket, and what [`crate::service`]
/// serves. Preparing it writes `service.json` and the token, because a client
/// that cannot find the port cannot ask anything.
#[derive(Debug)]
pub struct App {
    state: AppState,
    router: Router,
    files: ServiceFiles,
}

impl App {
    /// Writes the discovery files for `port` and builds the router.
    ///
    /// `after_write` is the binary's own step after a write (its dagr
    /// projection); a test passes `None`.
    pub fn prepare(
        repo: &Path,
        port: u16,
        actor: &Actor,
        state_dir: &Path,
        after_write: Option<AfterWrite>,
    ) -> Result<Self, ServerError> {
        Self::prepare_with_hub(repo, port, actor, state_dir, after_write, Hub::new())
    }

    /// [`App::prepare`], publishing writes on `hub`.
    ///
    /// The service's tick writes the same log the API does, so the two must
    /// share one change counter: a task the tick started has to wake the long
    /// polls and sockets an open page is waiting on, exactly as a write through
    /// `POST /api/action` does.
    pub fn prepare_with_hub(
        repo: &Path,
        port: u16,
        actor: &Actor,
        state_dir: &Path,
        after_write: Option<AfterWrite>,
        hub: Hub,
    ) -> Result<Self, ServerError> {
        Self::prepare_all(
            repo,
            port,
            actor,
            state_dir,
            after_write,
            hub,
            Paths::default(),
        )
    }

    /// [`App::prepare_with_hub`], pointed at the directories the binary resolved
    /// for the service.
    ///
    /// The paths are set before the router is built, so a start builds it once
    /// and a client that finds the discovery record at once finds a service
    /// ready to answer it.
    pub(crate) fn prepare_all(
        repo: &Path,
        port: u16,
        actor: &Actor,
        state_dir: &Path,
        after_write: Option<AfterWrite>,
        hub: Hub,
        paths: Paths,
    ) -> Result<Self, ServerError> {
        let files = ServiceFiles::prepare(state_dir)?;
        // A mod that starts before the run moves has nothing to watch for: the
        // change file is written once here, naming the log as it stands, so a
        // panel's first look at it is a real answer rather than a missing file.
        // It goes before the record, so a client that polls for `service.json`
        // never finds a record whose change file is still missing.
        let _ = crate::changes::touch(state_dir, ledger::last_seq(repo).unwrap_or(0));
        // The gate is prepared before the record, so a client that finds the
        // record finds a service that can already answer it.
        let gateway = shprd_remote::Gateway::new(state_dir, Arc::new(SystemClock))
            .with_token(files.token())
            .with_hosts(paths.allowed_hosts.clone());
        // A session that has expired is not found whatever happens, so this is
        // tidying: the file a start leaves behind never holds dead rows.
        if let Err(err) = gateway.sessions().purge() {
            tracing::warn!(%err, "could not tidy the expired sessions");
        }
        // A new service has no tunnel, whatever a service that was killed left in
        // `web.json`; the manager says so before anything can read it.
        let tunnel_factory: Arc<dyn shprd_remote::tunnel::TunnelFactory> = Arc::new(
            shprd_remote::tunnel::SystemTunnels::new(paths.path_env.clone().unwrap_or_default()),
        );
        let tunnels = shprd_remote::tunnel::TunnelManager::new(
            state_dir,
            &gateway,
            Arc::new(SystemClock),
            tunnel_factory.clone(),
        );
        tunnels.reset();
        files.write_record(port)?;
        let state = AppState {
            repo: repo.to_path_buf(),
            port,
            token: files.token().to_string(),
            id: files.id().to_string(),
            version: VERSION.to_string(),
            actor: actor.clone(),
            hub,
            after_write,
            ws: WsLimits::default(),
            // Everything the service reads from the machine is resolved by the
            // binary and carried in ([`crate::service::Config`]); a service
            // prepared by hand reads nothing of the process's environment.
            remote: Arc::new(gateway),
            home: paths.home,
            state_dir: state_dir.to_path_buf(),
            data: paths.data,
            engine: None,
            prompts_dir: paths.prompts_dir,
            path_env: paths.path_env,
            dagr: paths.dagr,
            chat: ChatHost::new(),
            terminal: crate::terminal::TerminalPort::new(),
            agents: crate::agents::Agents::new(),
            live: crate::live::LiveAgents::new(),
            transcript_roots: paths.transcript_roots,
            web: crate::web::Source::Embedded,
            tunnels,
            tunnel_factory,
        };
        let router = router(state.clone());
        Ok(Self {
            state,
            router,
            files,
        })
    }

    /// The router, ready to be served or driven by `tower::ServiceExt`.
    pub fn router(&self) -> Router {
        self.router.clone()
    }

    /// The change counter this service publishes on.
    pub fn hub(&self) -> Hub {
        self.state.hub.clone()
    }

    /// The repository whose log this service serves.
    pub fn repo(&self) -> &Path {
        &self.state.repo
    }

    /// What the service tells a client about itself.
    pub fn info(&self) -> ServiceInfo {
        ServiceInfo {
            port: self.state.port,
            id: self.state.id.clone(),
            version: self.state.version.clone(),
            pid: std::process::id(),
            token_path: self.files.token_path(),
            service_json: self.files.service_json_path(),
        }
    }

    /// Removes `service.json`, on a clean shutdown.
    pub fn remove_record(&self) -> Result<(), ServerError> {
        self.files.remove_record()
    }

    /// Serves sockets under `limits` instead of the defaults.
    ///
    /// The defaults are what a UI gets; a test uses short ones so a bound is
    /// observable without waiting minutes for it.
    pub fn with_ws_limits(mut self, limits: WsLimits) -> Self {
        self.state.ws = limits;
        self.router = router(self.state.clone());
        self
    }
}

impl App {
    /// Read and write the settings in `home` instead of the environment's home.
    ///
    /// A test that drives the settings routes passes its own home, so it can
    /// never read or write the developer's own settings file.
    pub fn with_home(mut self, home: PathBuf) -> Self {
        self.state.home = Some(home);
        self.router = router(self.state.clone());
        self
    }

    /// Look for the agent CLIs' session records under `roots`.
    ///
    /// A transcript is the CLI's own file, written under the home and data home
    /// *that* process resolved, so the binary passes them here
    /// ([`shprd_agents::transcript::Roots`]) rather than letting the chat guess
    /// from the settings home. A test points them at its own fixture home, which
    /// is the only way a chat reads a transcript a test wrote.
    pub fn with_transcript_roots(mut self, roots: shprd_agents::transcript::Roots) -> Self {
        self.state.transcript_roots = Some(roots);
        self.router = router(self.state.clone());
        self
    }

    /// Check the settings' roles against the harness table in `data`.
    ///
    /// A service without a data directory still resolves and writes the workflow
    /// keys; it refuses a role, because a role nothing looked at must not be
    /// stored.
    pub fn with_data(mut self, data: PathBuf) -> Self {
        self.state.data = Some(DataDir::resolve(Some(&data), None));
        self.router = router(self.state.clone());
        self
    }

    /// Read the harness table from `data`, however its origin resolved.
    ///
    /// [`App::with_data`] names a directory; this takes the
    /// [`DataDir`](shprd_agents::data::DataDir) the binary resolved, which is
    /// how a test drives a service with only the copy embedded in the binary.
    pub fn with_data_dir(mut self, data: DataDir) -> Self {
        self.state.data = Some(data);
        self.router = router(self.state.clone());
        self
    }

    /// Read and write the prompts in `dir` instead of under the home directory.
    ///
    /// A test passes its own directory, so it can never read or write the
    /// developer's own prompt files.
    pub fn with_prompts_dir(mut self, dir: PathBuf) -> Self {
        self.state.prompts_dir = Some(dir);
        self.router = router(self.state.clone());
        self
    }

    /// Read a program's own model listing with `path_env`, as `shprd serve`
    /// does with the `PATH` it was started with.
    ///
    /// A test passes a directory of its own — or nothing at all, which keeps
    /// every listing off the machine.
    pub fn with_path_env(mut self, path_env: Option<String>) -> Self {
        // The tunnel programs are looked up on this same `PATH`, so the factory
        // follows it. A service that has already started a tunnel is not
        // re-pathed: this is a builder, used before the service serves.
        self.state.tunnel_factory = Arc::new(shprd_remote::tunnel::SystemTunnels::new(
            path_env.clone().unwrap_or_default(),
        ));
        self.state.tunnels = self.new_tunnels();
        self.state.path_env = path_env;
        self.router = router(self.state.clone());
        self
    }

    /// Answer `GET /api/graph` with the program the binary found.
    ///
    /// A test passes its own answer, so the route is proven without probing the
    /// machine the test runs on.
    pub fn with_dagr(mut self, dagr: crate::graph::Dagr) -> Self {
        self.state.dagr = Some(dagr);
        self.router = router(self.state.clone());
        self
    }

    /// Give the service the binary's spawn and close, so the person's start,
    /// retry and stop actions reach a host.
    ///
    /// The service cannot make either itself — a spawn writes a brief and opens
    /// a tab, a stop closes a space — so they are passed in exactly as the
    /// tick's own steps are.
    pub fn with_engine(self, engine: Engine) -> Self {
        self.with_engine_opt(Some(engine))
    }

    /// [`App::with_engine`], for a caller that may not have one.
    ///
    /// The service's config carries an `Option`, and a builder that turned
    /// `None` into a refusal at the wrong place would be one more branch to
    /// keep in step.
    pub(crate) fn with_engine_opt(mut self, engine: Option<Engine>) -> Self {
        self.state.engine = engine;
        self.router = router(self.state.clone());
        self
    }

    /// Type chat lines into a space's agent through `writer`.
    ///
    /// The binary passes the host it drives; a test passes a recorder, so the
    /// API is proved without a terminal and a person's window is never written
    /// to by a test.
    pub fn with_chat_writer(self, writer: Arc<dyn ChatWriter>) -> Self {
        self.with_chat_opt(Some(writer))
    }

    /// [`App::with_chat_writer`], for a caller that may not have one.
    ///
    /// The service's config carries an `Option`, and this is the same shape as
    /// [`App::with_engine_opt`]: a service started with no host still reads a
    /// conversation, and refuses only the send.
    pub(crate) fn with_chat_opt(mut self, writer: Option<Arc<dyn ChatWriter>>) -> Self {
        self.state.chat = match writer {
            Some(writer) => ChatHost::with(writer),
            None => ChatHost::new(),
        };
        self.router = router(self.state.clone());
        self
    }

    /// Watch and type into panes through `host`, for `GET /api/terminal`.
    ///
    /// The binary passes the host it drives; a test passes a fake pane, so no
    /// terminal is ever reached by a test.
    pub fn with_terminal_host(self, host: Arc<dyn crate::terminal::TerminalHost>) -> Self {
        self.with_terminal_opt(Some(host))
    }

    /// [`App::with_terminal_host`], for a caller that may not have one.
    pub(crate) fn with_terminal_opt(
        mut self,
        host: Option<Arc<dyn crate::terminal::TerminalHost>>,
    ) -> Self {
        self.state.terminal = match host {
            Some(host) => crate::terminal::TerminalPort::with(host),
            None => crate::terminal::TerminalPort::new(),
        };
        self.router = router(self.state.clone());
        self
    }

    /// Count the login's rate limits and session clocks against `clock`.
    ///
    /// A test moves time rather than waiting for it; the service itself always
    /// uses [`shprd_core::log::SystemClock`]. The gateway is rebuilt, so the
    /// password, sessions and hostnames this service already had are kept.
    pub fn with_clock(mut self, clock: Arc<dyn Clock + Send + Sync>) -> Self {
        let hosts = self.state.remote.hosts();
        // The storm signal is carried over: the tunnel manager holds the same
        // handle, and a rebuilt gateway that started its own would leave it
        // watching a counter nothing moves.
        let storm = self.state.remote.storm();
        self.state.remote = Arc::new(
            shprd_remote::Gateway::new(&self.state.state_dir, clock)
                .with_token(self.state.token.clone())
                .with_hosts(hosts)
                .with_storm(storm),
        );
        self.router = router(self.state.clone());
        self
    }

    /// Start programs in new tabs through `opener`.
    ///
    /// The binary passes the host it drives; a test passes a recorder, so the
    /// route is proved without a terminal and a person's window is never opened
    /// by a test.
    pub fn with_agent_opener(self, opener: Arc<dyn crate::agents::AgentOpener>) -> Self {
        self.with_agent_opener_opt(Some(opener))
    }

    /// [`App::with_agent_opener`], for a caller that may not have one.
    ///
    /// It is the same shape as [`App::with_chat_opt`]: a service started with no
    /// host still answers every read, and refuses only the start.
    pub(crate) fn with_agent_opener_opt(mut self, opener: Option<Arc<dyn AgentOpener>>) -> Self {
        self.state.agents = match opener {
            Some(opener) => crate::agents::Agents::with(opener),
            None => crate::agents::Agents::new(),
        };
        self.router = router(self.state.clone());
        self
    }

    /// List agents through `source`, for `live_agents` and the agent routes.
    pub fn with_live_source(self, source: Arc<dyn crate::live::LiveSource>) -> Self {
        self.with_live_opt(Some(source))
    }

    /// [`App::with_live_source`], for a caller that may not have one.
    pub(crate) fn with_live_opt(
        mut self,
        source: Option<Arc<dyn crate::live::LiveSource>>,
    ) -> Self {
        self.state.live = match source {
            Some(source) => crate::live::LiveAgents::with(source),
            None => crate::live::LiveAgents::new(),
        };
        self.router = router(self.state.clone());
        self
    }

    /// Serve the web app from `dir` instead of the build embedded in the binary.
    ///
    /// Production always embeds `web/dist`; a test passes a temp directory
    /// holding its own `index.html` and assets, so the SPA fallback, the cache
    /// headers and the `/api` split are proven in CI, which does not build the
    /// web app before the cargo job.
    pub fn with_web_dir(mut self, dir: PathBuf) -> Self {
        self.state.web = crate::web::Source::Dir(dir);
        self.router = router(self.state.clone());
        self
    }

    /// Build tunnels with `factory` instead of the person's own programs.
    ///
    /// A test hands in a fake provider so the routes, the fail-closed rule and
    /// the allowlist are proven without a network; production never calls it.
    pub fn with_tunnel_factory(
        mut self,
        factory: Arc<dyn shprd_remote::tunnel::TunnelFactory>,
    ) -> Self {
        self.state.tunnel_factory = factory;
        self.state.tunnels = self.new_tunnels();
        self.router = router(self.state.clone());
        self
    }

    /// A manager over this service's gate, with the factory it holds now.
    fn new_tunnels(&self) -> shprd_remote::tunnel::TunnelManager {
        let manager = shprd_remote::tunnel::TunnelManager::new(
            &self.state.state_dir,
            &self.state.remote,
            Arc::new(SystemClock),
            self.state.tunnel_factory.clone(),
        );
        manager.reset();
        manager
    }

    /// The tunnel manager, so the service can stop the tunnel on its way out.
    pub fn tunnels(&self) -> shprd_remote::tunnel::TunnelManager {
        self.state.tunnels.clone()
    }

    /// The names this service answers to, as a shared handle.
    ///
    /// A tunnel task adds its hostname here when it starts and removes it when it
    /// stops ([`shprd_remote::AllowedHosts`]); the gate sees the change at once,
    /// and an unknown `Host` is a `421` either way.
    pub fn allowed_hosts(&self) -> shprd_remote::AllowedHosts {
        self.state.remote.hosts()
    }

    /// Serve with no harness table at all, whatever the environment would find.
    ///
    /// A test uses it to prove what a machine without a data directory does,
    /// without depending on whether the developer happens to have one installed.
    pub fn without_data(mut self) -> Self {
        self.state.data = None;
        self.router = router(self.state.clone());
        self
    }
}

/// The whole API, with the gate in front of everything but health and login.
///
/// The layers, innermost first: the body limit every route shares, the trace,
/// the `Host` allowlist, and the peer address the rate limits are keyed on. The
/// gate itself is a layer on the guarded routes only, because a login is how a
/// session comes to exist and health is the handshake.
pub(crate) fn router(state: AppState) -> Router {
    let guarded = Router::new()
        .route("/api/state", get(snapshot_handler))
        .route("/api/changes", get(changes))
        .route("/api/asks", get(asks))
        .route("/api/asks/answer", post(answer))
        .route("/api/chat", get(chat_page).post(chat_send))
        .route("/api/terminal", get(crate::terminal::socket))
        .route("/api/action", post(action))
        .route("/api/graph", get(graph))
        .route("/api/graph/run", get(graph_run))
        .route("/api/tasks", post(add_task))
        .route("/api/tasks/{id}/changes", get(task_changes))
        .route("/api/settings", get(settings).put(put_settings))
        .route("/api/coordinator/restart", post(restart_coordinator))
        .route("/api/roster", get(roster))
        .route("/api/agents", post(open_agent))
        .route("/api/agents/{id}/output", get(agent_output))
        .route("/api/agents/{id}/attach", post(agent_attach))
        .route("/api/prompts", get(list_prompts))
        .route(
            "/api/prompts/{id}",
            get(read_prompt).put(put_prompt).delete(reset_prompt),
        )
        .route("/api/ws", get(socket))
        .route("/api/session", get(login::session))
        .route("/api/logout", post(login::logout))
        .route("/api/sessions", get(login::sessions))
        .route("/api/sessions/{id}", delete(login::revoke))
        .route("/api/tunnels", get(tunnels::list))
        .route("/api/tunnels/start", post(tunnels::start))
        .route("/api/tunnels/stop", post(tunnels::stop))
        .layer(middleware::from_fn_with_state(state.clone(), auth::require));
    Router::new()
        .route("/api/health", get(health))
        .route("/api/login", post(login::login))
        .merge(guarded)
        // Everything else is the built web app: an asset, an SPA route, or the
        // page a binary without a web build serves. An `/api…` miss keeps the
        // API's typed 404, which `web` asks `not_found` for.
        .fallback(crate::web::serve)
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        // The path only, never the query: a socket's query carries the bearer
        // token, because a browser cannot send one as a header, and a log is not
        // a place for a secret (M7d §4.4, threat row 15).
        .layer(
            TraceLayer::new_for_http().make_span_with(|request: &axum::http::Request<_>| {
                tracing::debug_span!(
                    "request",
                    method = %request.method(),
                    path = %request.uri().path()
                )
            }),
        )
        .layer(middleware::from_fn_with_state(state.clone(), auth::host))
        .layer(middleware::from_fn_with_state(state.clone(), auth::peer))
        .with_state(state)
}

/// `GET /api/health`: who this is, for the handshake.
///
/// The service's `id` is the local handshake's (the CLI and the desktop mods
/// check it before they send the token), so it is answered to a loopback caller
/// only: anyone reaching the service by its tunnel's name gets `ok` and the
/// version.
async fn health(
    State(state): State<AppState>,
    Extension(auth::Peer(peer)): Extension<auth::Peer>,
    headers: HeaderMap,
) -> Json<HealthDto> {
    Json(HealthDto {
        ok: true,
        id: auth::is_local_caller(peer, &headers).then(|| state.id.clone()),
        version: state.version.clone(),
    })
}

/// `GET /api/state`: the whole board, folded now.
///
/// The last pass's outcome comes from the state directory beside the snapshot,
/// so a step that failed is visible on the board as well as in the service's own
/// output: `shprd status` reads the same file.
async fn snapshot_handler(State(state): State<AppState>) -> Result<Json<SnapshotDto>, ApiReject> {
    let repo = state.repo.clone();
    let dir = state.state_dir.clone();
    let loaded = blocking(move || {
        let folded = ledger::load(&repo);
        let last = crate::pass::LastPass::read(&dir, &repo);
        (folded, last)
    })
    .await?;
    let (folded, last) = loaded;
    let (folded, _) = folded.map_err(|err| ApiReject::internal(err.to_string()))?;
    let mut snapshot = SnapshotDto::from_state(&folded, SystemClock.now_ms());
    snapshot.last_tick = last.as_ref().map(crate::dto::LastTickDto::from);
    // The host is asked off the runtime: a host call blocks on its own.
    let live = state.live.clone();
    let repo = state.repo.clone();
    let (rows, host_space, branch) = blocking(move || {
        let branch = shprd_git::git::Git::new(&repo)
            .current_branch()
            .ok()
            .flatten();
        (live.rows(), live.space(), branch)
    })
    .await?;
    crate::live::decorate(&mut snapshot, &state.repo, rows, host_space, branch);
    let titles_state = state.clone();
    snapshot.role_titles = blocking(move || settings_api::role_titles(&titles_state)).await?;
    Ok(Json(snapshot))
}

/// `GET /api/graph`: where the graph's program is, and the command that draws
/// the run.
///
/// The block in the Herdr window is a terminal running dagr, so this answers the
/// command and the reason when there is none; the run's identity is read from
/// the same log the board is folded from, so a graph and a board never disagree
/// about which run they show.
async fn graph(State(state): State<AppState>) -> Result<Json<GraphDto>, ApiReject> {
    let repo = state.repo.clone();
    let dagr = state.dagr.clone();
    let run = blocking(move || ledger::load(&repo).map(|(state, _)| state.run))
        .await?
        .map_err(|err| ApiReject::internal(err.to_string()))?
        .map(|run| run.run_id);
    Ok(Json(GraphDto::new(
        &state.repo,
        run.as_deref(),
        dagr.as_ref(),
    )))
}

/// `GET /api/graph/run`: the run document the window's graph block draws.
///
/// The desktop's block is a terminal running `dagr view <repo>/.dagr/run.json`,
/// which the service rewrites after every ledger write. The web cannot run dagr,
/// so it draws that same file itself: this answers the file, verbatim, so a graph
/// in a browser and a graph in the window can never show different runs. A run
/// that has not written a document yet is a `404` with the words that say so,
/// and a file that is not a run document is a `500`: the page draws neither.
async fn graph_run(State(state): State<AppState>) -> Result<Json<Value>, ApiReject> {
    let repo = state.repo.clone();
    let document = blocking(move || crate::graph::read_document(&repo)).await?;
    Ok(Json(document?))
}

/// `GET /api/asks`: every ask still waiting for the person.
async fn asks(State(state): State<AppState>) -> Result<Json<AskListDto>, ApiReject> {
    let repo = state.repo.clone();
    let now_ms = SystemClock.now_ms();
    let list = blocking(move || actions::list_asks(&repo, now_ms)).await?;
    Ok(Json(list?))
}

/// `POST /api/asks/answer`: answer one ask, exactly once.
async fn answer(State(state): State<AppState>, body: Bytes) -> Result<Json<AskReply>, ApiReject> {
    let body: AnswerAsk = serde_json::from_slice(&body)
        .map_err(|err| ApiReject::bad_request(format!("the body is not an answer: {err}")))?;
    let repo = state.repo.clone();
    let actor = state.actor.clone();
    let hub = state.hub.clone();
    let after_write = state.after_write.clone();
    let reply = blocking(move || {
        let ctx = actions::Context {
            repo: &repo,
            actor: &actor,
            hub: &hub,
            after_write: after_write.as_ref(),
            engine: None,
            merge: None,
        };
        actions::answer_ask(&ctx, &body.task, &body.ask, &body.answer)
    })
    .await?;
    Ok(Json(reply?))
}

/// The query `GET /api/chat` takes.
#[derive(Debug, Deserialize)]
struct ChatQuery {
    /// `coordinator`, or a task's id. Absent means the coordinator.
    space: Option<String>,
    /// The cursor the client already has.
    since: Option<String>,
}

/// `GET /api/chat`: what a space's agent record gained since `since`.
///
/// The answer is a page and a cursor; a client that asks again with the cursor
/// sees only what was added. Nothing is written, and the agent's own file is the
/// only copy read.
async fn chat_page(
    State(state): State<AppState>,
    Query(query): Query<ChatQuery>,
) -> Result<Json<ChatDto>, ApiReject> {
    let space = Space::parse(query.space.as_deref());
    let since = query.since.unwrap_or_default();
    let answer = blocking(move || chat::page(&state, &space, &since)).await?;
    Ok(Json(answer?))
}

/// `POST /api/chat`: type one turn into a space's agent, as the person.
///
/// The write needs the custom header every other write needs (`auth`), because
/// it reaches a terminal rather than the ledger.
async fn chat_send(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Json<ChatSendReply>, ApiReject> {
    let answer = blocking(move || chat::send(&state, &body)).await?;
    Ok(Json(answer?))
}

/// The query `GET /api/changes` takes.
#[derive(Debug, Deserialize)]
struct ChangesQuery {
    /// The last sequence the client already has.
    since: Option<u64>,
    /// How long to wait for more, in seconds.
    timeout: Option<u64>,
}

/// `GET /api/changes`: long-poll for events after `since`.
///
/// It answers the events themselves (a snapshot's worth of information in one
/// call, which is what a Herdr mod can afford), or `204` when the wait runs out.
async fn changes(
    State(state): State<AppState>,
    Query(query): Query<ChangesQuery>,
) -> Result<Response, ApiReject> {
    let since = query.since.unwrap_or(0);
    let wait = Duration::from_secs(query.timeout.unwrap_or(DEFAULT_WAIT.as_secs()));
    let wait = wait.min(MAX_WAIT);
    let deadline = Instant::now() + wait;
    let mut seen: Option<Option<Stamp>> = None;

    loop {
        let current = {
            let repo = state.repo.clone();
            let stamp = blocking(move || ledger::stamp(&repo)).await?;
            stamp.map_err(|err| {
                ApiReject::internal(format!("could not look at the run log: {err}"))
            })?
        };
        let changed = match &seen {
            None => true,
            Some(previous) => previous != &current,
        };
        if changed {
            let repo = state.repo.clone();
            let events = blocking(move || ledger::read_since(&repo, since)).await?;
            let events = events.map_err(|err| ApiReject::internal(err.to_string()))?;
            seen = Some(current);
            // An empty answer means the log is behind the client, or has no
            // event past `since`: wait for one rather than answer nothing.
            if !events.is_empty() {
                let seq = events.last().map(|event| event.seq).unwrap_or(since);
                let notices = ledger::notices(&events).map_err(|err| {
                    ApiReject::internal(format!("could not project events: {err}"))
                })?;
                return Ok(Json(ChangeDto {
                    seq,
                    events: notices,
                })
                .into_response());
            }
        }

        let now = Instant::now();
        if now >= deadline {
            return Ok(StatusCode::NO_CONTENT.into_response());
        }
        let remaining = (deadline - now).min(TICK);
        tokio::select! {
            _ = state.hub.wait() => {}
            _ = tokio::time::sleep(remaining) => {}
        }
    }
}

/// A sentence for an action body serde could not read.
///
/// A missing field is the common slip, and serde's own text for it
/// (`missing field `reason``) reads as an internal error to the person whose
/// button sent it, so it is said in words; anything else keeps serde's detail.
fn malformed_action(kind: &str, detail: &str) -> String {
    let missing = detail
        .strip_prefix("missing field `")
        .and_then(|rest| rest.split_once('`'))
        .map(|(field, _)| field);
    match (kind, missing) {
        ("task.drop" | "task_drop" | "task.drop_many" | "task_drop_many", Some("reason")) => {
            "dropping a task needs a reason: say why it is not wanted".to_string()
        }
        (_, Some(field)) => format!("a {kind:?} action needs a {field:?}"),
        (_, None) => format!("a {kind:?} action is malformed: {detail}"),
    }
}

/// `POST /api/action`: one envelope, run against the same log the CLI writes.
async fn action(
    State(state): State<AppState>,
    Extension(door): Extension<shprd_remote::Door>,
    body: Bytes,
) -> Result<Json<ActionReply>, ApiReject> {
    let value: Value = serde_json::from_slice(&body)
        .map_err(|err| ApiReject::bad_request(format!("the body is not JSON: {err}")))?;
    let kind = value
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ApiReject::bad_request(format!(
                "the body needs a \"kind\": one of {}",
                Action::KINDS.join(", ")
            ))
        })?
        .to_string();
    if !Action::KINDS.contains(&kind.as_str()) {
        return Err(ApiReject::unknown_action(&kind));
    }
    let action: Action = Action::from_value(value)
        .map_err(|err| ApiReject::bad_request(malformed_action(&kind, &err.to_string())))?;

    // A destructive action needs a password entered in the last five minutes
    // through a session. The loopback token door is exempt: the token is already
    // local, and the researched design says so (M7d §4.4 rule 6).
    if action.is_destructive() {
        state.remote.require_fresh(&door).map_err(login::reject)?;
    }

    // The context borrows the service, and the work runs on a blocking thread,
    // so the pieces it needs are moved in and rebuilt there.
    let repo = state.repo.clone();
    let actor = state.actor.clone();
    let hub = state.hub.clone();
    let after_write = state.after_write.clone();
    let engine = state.engine.clone();
    let merge = settings_api::merge_policy(&state);
    let reply = blocking(move || {
        let ctx = actions::Context {
            repo: &repo,
            actor: &actor,
            hub: &hub,
            after_write: after_write.as_ref(),
            engine: engine.as_ref(),
            merge,
        };
        actions::run(&ctx, action)
    })
    .await?;
    Ok(Json(reply?))
}

/// `POST /api/tasks`: add one task, as `shprd agent task add` does.
///
/// The person's own verb: the service's actor is the person who started it, so a
/// task added from a board is the orchestrator's peer rather than its
/// subordinate, and the same rules check it either way.
async fn add_task(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Json<ActionReply>, ApiReject> {
    let hub = state.hub.clone();
    let after_write = state.after_write.clone();
    let reply = blocking(move || tasks::add(&state, &hub, after_write.as_ref(), &body)).await?;
    Ok(Json(reply?))
}

/// `GET /api/tasks/{id}/changes`: the task's branch against its base.
///
/// The accept card's Changes section: the files the attempt changed and how much.
/// It reads git — a fold says an attempt exists, and only the repository says
/// what it did — so it answers the count line and the paths the card draws.
async fn task_changes(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<crate::dto::TaskChangesDto>, ApiReject> {
    let answer = blocking(move || tasks::changes(&state, &id)).await?;
    answer.map(Json)
}

/// `GET /api/settings`: every key, its value and the layer it came from.
async fn settings(State(state): State<AppState>) -> Result<Json<SettingsDto>, ApiReject> {
    let answer = blocking(move || settings_api::read(&state)).await?;
    answer.map(Json)
}

/// `PUT /api/settings`: write one key, and answer the settings as they now are.
async fn put_settings(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Json<SettingsWriteDto>, ApiReject> {
    let answer = blocking(move || settings_api::write(&state, &body)).await?;
    answer.map(Json)
}

/// `POST /api/coordinator/restart`: end the coordinator and start it again from
/// its role, in the same tab. It is how a new program or model reaches a
/// coordinator that is already running.
async fn restart_coordinator(State(state): State<AppState>) -> Result<StatusCode, ApiReject> {
    let agents = state.agents.clone();
    blocking(move || agents.restart_coordinator()).await??;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/agents`: start a program in a new tab, in the run's space.
///
/// It is the tab bar's "+" menu's *Start an agent*: a plain CLI in a tab, with
/// nothing to work on — a task's agent is a spawn, and that is `POST /api/action`.
/// The service holds no harness table and no host, so the work is the binary's
/// ([`crate::agents::AgentOpener`]); this route parses the request and hands it
/// over, and a service without one refuses rather than pretending.
async fn open_agent(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Json<AgentTabDto>, ApiReject> {
    let agents = state.agents.clone();
    let opened = blocking(move || agents.open(&body)).await?;
    Ok(Json(AgentTabDto::from(opened?)))
}

/// `GET /api/agents/{id}/output?lines=N`: one agent's recent screen.
async fn agent_output(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<crate::dto::AgentOutputDto>, ApiReject> {
    let lines = query
        .get("lines")
        .and_then(|lines| lines.parse::<usize>().ok())
        .unwrap_or(120);
    let live = state.live.clone();
    Ok(Json(blocking(move || live.output(&id, lines)).await??))
}

/// `POST /api/agents/{id}/attach`: what to bind a real terminal to.
async fn agent_attach(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<crate::dto::AgentAttachDto>, ApiReject> {
    let live = state.live.clone();
    Ok(Json(blocking(move || live.attach(&id)).await??))
}

/// `GET /api/roster`: the programs a role may run, with their models and levels.
///
/// It is a read of the shipped tables, except for a program that lists its own
/// models: that listing is a process, so it runs on a blocking thread and with
/// the `PATH` the binary passed in ([`AppState::path_env`]).
async fn roster(State(state): State<AppState>) -> Result<Json<RosterDto>, ApiReject> {
    let answer =
        blocking(move || roster_api::read(state.data.as_ref(), state.path_env.as_deref())).await?;
    Ok(Json(answer))
}

/// `GET /api/prompts`: every template this machine has, without its words.
async fn list_prompts(State(state): State<AppState>) -> Result<Json<PromptListDto>, ApiReject> {
    let answer = blocking(move || prompts_api::list(&state)).await?;
    answer.map(Json)
}

/// `GET /api/prompts/:id`: one prompt's words, its file and its state.
async fn read_prompt(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<PromptDto>, ApiReject> {
    let answer = blocking(move || prompts_api::read(&state, &id)).await?;
    answer.map(Json)
}

/// `PUT /api/prompts/:id`: store the person's words over the file.
async fn put_prompt(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
    body: Bytes,
) -> Result<Json<PromptDto>, ApiReject> {
    let answer = blocking(move || prompts_api::write(&state, &id, &body)).await?;
    answer.map(Json)
}

/// `DELETE /api/prompts/:id`: put the shipped words back over an edit.
///
/// It is the templates page's Reset, and the command line's `shprd prompts
/// reset`: the only route that replaces the person's words, and it does so with
/// the words this build ships rather than with anything a client sent.
async fn reset_prompt(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<PromptDto>, ApiReject> {
    let answer = blocking(move || prompts_api::reset(&state, &id)).await?;
    answer.map(Json)
}

/// `GET /api/ws`: the same change notices, pushed.
///
/// A request that is not a WebSocket upgrade is refused with the typed body
/// every other refusal has, and pointed at the long poll, so a plain `GET` (or
/// a probe that forgot its upgrade headers) gets an answer instead of a stream
/// it cannot read.
async fn socket(
    State(state): State<AppState>,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    match upgrade {
        Ok(upgrade) => upgrade.on_upgrade(move |socket| pump(socket, state)),
        Err(rejection) => ApiReject::new(
            rejection.status(),
            ErrorCode::BadRequest,
            format!(
                "{}: this route is a WebSocket and stays open; poll GET /api/changes for changes instead",
                rejection.body_text()
            ),
        )
        .into_response(),
    }
}

/// Pushes a hello, then one change per batch of new events, until the client
/// goes away.
///
/// The loop's tick is what makes a missed wake-up harmless: the log is the truth
/// and it is re-read regardless.
async fn pump(mut socket: WebSocket, state: AppState) {
    let limits = state.ws;
    let seq = {
        let repo = state.repo.clone();
        match blocking(move || ledger::last_seq(&repo)).await {
            Ok(Ok(seq)) => seq,
            // A log that will not read yet is an empty one: hello says so and
            // the tick below will catch up.
            Ok(Err(_)) | Err(_) => 0,
        }
    };
    let hello = WsMessage::Hello {
        id: state.id.clone(),
        version: state.version.clone(),
        seq,
    };
    if send(&mut socket, &hello).await.is_err() {
        return;
    }

    // Anything published between reading the log and the hello above is still
    // delivered: the socket starts from the furthest of the two watermarks.
    let mut since = seq.max(state.hub.last());
    let opened = Instant::now();
    let mut heard = opened;
    let mut ping_at = opened + limits.ping;
    loop {
        let tick = tokio::time::sleep(TICK);
        tokio::select! {
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    heard = Instant::now();
                    if let Ok(ClientMessage::Ping { at }) = serde_json::from_str(&text)
                        && send(&mut socket, &WsMessage::Pong { at }).await.is_err()
                    {
                        return;
                    }
                }
                // The library answers a ping on its own; the frame still
                // counts as the client being there.
                Some(Ok(Message::Ping(_))) => heard = Instant::now(),
                Some(Ok(Message::Pong(_) | Message::Binary(_))) => heard = Instant::now(),
                Some(Ok(Message::Close(_)) | Err(_)) | None => return,
            },
            _ = state.hub.wait() => {}
            _ = tick => {}
        }

        // A socket is bounded: the client's silence closes it, and no socket
        // outlives the session limit. Both answers are a clean `1001`, so a
        // client sees a close and reconnects rather than a dead connection.
        let now = Instant::now();
        if now.duration_since(opened) >= limits.session {
            close(&mut socket, "the session limit was reached; reconnect").await;
            return;
        }
        if now.duration_since(heard) >= limits.idle {
            close(
                &mut socket,
                "idle; reconnect when there is something to see",
            )
            .await;
            return;
        }
        if now >= ping_at {
            if socket.send(Message::Ping(Bytes::new())).await.is_err() {
                return;
            }
            ping_at = now + limits.ping;
        }

        let repo = state.repo.clone();
        let events = match blocking(move || ledger::read_since(&repo, since)).await {
            Ok(Ok(events)) => events,
            // A log that will not read is not a reason to drop the socket; the
            // next tick tries again.
            Ok(Err(_)) | Err(_) => continue,
        };
        if events.is_empty() {
            continue;
        }
        let seq = events.last().map(|event| event.seq).unwrap_or(since);
        let Ok(notices) = ledger::notices(&events) else {
            continue;
        };
        let message = WsMessage::Change {
            change: ChangeDto {
                seq,
                events: notices,
            },
        };
        if send(&mut socket, &message).await.is_err() {
            return;
        }
        since = seq;
    }
}

/// Closes the socket with `1001 going away` and `reason`.
///
/// The code is the protocol's own "going away" rather than an error: the client
/// should reconnect, and every WebSocket client treats `1001` as that.
async fn close(socket: &mut WebSocket, reason: &'static str) {
    let frame = CloseFrame {
        code: close_code::AWAY,
        reason: reason.into(),
    };
    let _ = socket.send(Message::Close(Some(frame))).await;
}

/// Sends one message as JSON text.
async fn send(socket: &mut WebSocket, message: &WsMessage) -> Result<(), ()> {
    let text = serde_json::to_string(message).map_err(|_| ())?;
    socket
        .send(Message::Text(text.into()))
        .await
        .map_err(|_| ())
}

/// Always a typed body, so a client never has to parse HTML to learn why a path
/// it guessed did not exist.
pub(crate) fn not_found() -> ApiReject {
    ApiReject::new(StatusCode::NOT_FOUND, ErrorCode::NotFound, "no such route")
}

/// Runs a blocking ledger operation off the async runtime.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, ApiReject> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|err| ApiReject::internal(format!("the ledger task did not finish: {err}")))
}

#[cfg(test)]
mod malformed_action_tests {
    use super::malformed_action;

    #[test]
    fn a_missing_field_is_said_in_words() {
        assert_eq!(
            malformed_action("task.drop", "missing field `reason` at line 1 column 30"),
            "dropping a task needs a reason: say why it is not wanted"
        );
        assert_eq!(
            malformed_action("task.stop", "missing field `task` at line 1 column 9"),
            "a \"task.stop\" action needs a \"task\""
        );
        let other = malformed_action("task.add", "invalid type: string, expected a sequence");
        assert!(other.contains("is malformed"), "{other}");
    }
}
