//! Every repository one service serves.
//!
//! One `shprd serve` per user (SHPR-77) holds a **slot** per repository: its own
//! run log, change file, hub, tick and host adapters, laid over the state the
//! whole service shares (the discovery files, the gate and the tunnel). A
//! repo-scoped request names its repository with `?repo=<id or path>` (or the
//! `x-shprd-repo` header, for a client that can set one) and is answered by that
//! slot's own router, so a handler never knows there is more than one.
//!
//! Only a **registered** repository resolves, and the parameter is never opened
//! as a path: it is matched against the roots the service was told about. A
//! repository is registered by `POST /api/repos {folder}` (idempotent, and it
//! answers at once: the repository's space and coordinator are prepared in the
//! background), by `PUT /api/repos {folders}` (the layout's whole set: those
//! folders are registered and every other unpinned repository stops being
//! ticked and is dropped), or by naming it when the service starts. The three
//! registration routes take the token door only, never a web session.
//!
//! A request that names no repository is answered by the only one registered
//! (so a service started for one repository, and a client that does not know
//! about the others, behave as they always did); with none or several
//! registered it is refused in words instead of guessed at.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use axum::extract::{Query, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Json, Router, middleware};
use serde::{Deserialize, Serialize};
use tower::ServiceExt;

use crate::api::{AppState, router};
use crate::auth;
use crate::dto::ErrorCode;
use crate::error::ApiReject;
use crate::hub::Hub;
use crate::service::{AfterWrite, TickConfig};

/// The header a client that cannot set a query may name its repository in.
pub const REPO_HEADER: &str = "x-shprd-repo";

/// What the binary supplies to make one repository's slot.
///
/// The server cannot build a host adapter, a tick worker or a coordinator, so
/// the binary passes this in ([`crate::Config::factory`]): the service asks it
/// for a repository's parts when that repository is registered.
pub trait RepoFactory: std::fmt::Debug + Send + Sync {
    /// The repository root a folder belongs to: a folder inside a repository
    /// serves that repository, never a second one; a folder that is not one yet
    /// is its own root (it becomes a repository at the first chat message).
    fn root_of(&self, folder: &Path) -> PathBuf;

    /// Everything the slot for `root` is made of. `folder` is the folder the
    /// person added (the one a space may already be adopted for), `hub` the
    /// slot's change counter and `dir` its data directory. Nothing slow runs
    /// here: whatever has to touch the host goes in [`RepoParts::prepare`].
    ///
    /// # Errors
    ///
    /// One plain sentence when the repository cannot be served.
    fn build(&self, root: &Path, folder: &Path, hub: Hub, dir: &Path) -> Result<RepoParts, String>;
}

/// One repository's collaborators, as the factory made them.
#[derive(Default)]
pub struct RepoParts {
    /// The step after a write (the dagr projection).
    pub after_write: Option<AfterWrite>,
    /// The person's start, retry and stop actions.
    pub engine: Option<crate::engine::Engine>,
    /// What types a chat line into a space's agent.
    pub chat: crate::chat::ChatHost,
    /// What the terminal socket watches.
    pub terminal: crate::terminal::TerminalPort,
    /// What the "+" menu starts.
    pub agents: crate::agents::Agents,
    /// The agents the host lists.
    pub live: crate::live::LiveAgents,
    /// This repository's tick.
    pub tick: Option<TickConfig>,
    /// Preparing the repository's own space and its coordinator, which asks the
    /// host and so can take seconds: it runs on its own thread after the slot
    /// answers, never in a request.
    pub prepare: Option<Box<dyn FnOnce() + Send>>,
}

impl std::fmt::Debug for RepoParts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RepoParts").finish_non_exhaustive()
    }
}

/// One registered repository.
pub(crate) struct Slot {
    /// The stable id: `<name>-<hash>`, the name of its data directory.
    pub(crate) id: String,
    /// The repository root, as its own tools name it.
    pub(crate) root: PathBuf,
    /// The same root with every symlink resolved, for comparing paths.
    canon: PathBuf,
    /// Where its change file and last pass live.
    pub(crate) dir: PathBuf,
    /// The state its handlers see.
    pub(crate) state: AppState,
    /// Its router.
    pub(crate) router: Router,
    /// A repository the service was started for (or built by hand): a layout's
    /// sync never drops it.
    pub(crate) pinned: bool,
    /// Its tick, until the registry starts it.
    tick: Option<TickConfig>,
    /// Ends the tick.
    stop: tokio::sync::watch::Sender<bool>,
    /// The log watcher and the tick.
    tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    /// Preparing the repository's space and coordinator, until it has run. A
    /// repository nobody has opened and that has no run of its own is not
    /// prepared at all: a window that lists it does not start an agent for it.
    prepare: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl Slot {
    /// Starts the log watcher and (once) the tick.
    fn start(&self, tick_override: Option<TickConfig>) {
        let Ok(mut tasks) = self.tasks.lock() else {
            return;
        };
        if tasks.is_empty() {
            tasks.push(tokio::spawn(crate::service::watch_log(
                self.root.clone(),
                self.state.hub.clone(),
            )));
        }
        if let Some(tick) = tick_override.or_else(|| self.tick.clone()) {
            let worker = Arc::clone(&tick.worker);
            let notice = tick.notice.clone();
            let mut stopped = self.stop.subscribe();
            tasks.push(tokio::spawn(async move {
                crate::tick::run(worker, tick.interval, notice, async move {
                    let _ = stopped.wait_for(|stop| *stop).await;
                })
                .await;
            }));
        }
    }

    /// Runs the repository's preparation, once; on this thread when `wait`.
    fn run_prepare(&self, wait: bool) {
        let step = self.prepare.lock().ok().and_then(|mut step| step.take());
        match step {
            Some(step) if wait => step(),
            Some(step) => {
                std::thread::spawn(step);
            }
            None => {}
        }
    }

    /// Stops what [`Slot::start`] started; the pass in flight finishes.
    fn stop(&self) -> Vec<tokio::task::JoinHandle<()>> {
        let _ = self.stop.send(true);
        let mut tasks = self
            .tasks
            .lock()
            .map(|mut t| std::mem::take(&mut *t))
            .unwrap_or_default();
        // The watcher has no state worth finishing.
        if !tasks.is_empty() {
            tasks.remove(0).abort();
        }
        tasks
    }
}

impl std::fmt::Debug for Slot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Slot")
            .field("id", &self.id)
            .field("root", &self.root)
            .finish()
    }
}

/// The repositories one service serves, and how a request finds its own.
pub struct Registry {
    base: AppState,
    dir: PathBuf,
    factory: Option<Arc<dyn RepoFactory>>,
    slots: RwLock<BTreeMap<String, Arc<Slot>>>,
    /// Serialises registration, so two requests for one folder make one slot.
    registering: Mutex<()>,
    /// Whether the service is running its ticks yet.
    ticking: AtomicBool,
    global: Router,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry").finish_non_exhaustive()
    }
}

/// One repository as the routes answer it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepoInfo {
    /// The stable id.
    pub id: String,
    /// The repository root.
    pub root: String,
    /// The directory whose `changes` file a mod watches.
    pub dir: String,
}

/// The id and data directory for the repository at `root`, in `state_dir`.
fn slot_dir(state_dir: &Path, root: &Path) -> (String, PathBuf) {
    // The same rule the per-repository directories always followed, so a
    // repository keeps its last-pass file across the move to one service.
    let dir = crate::service::repo_dir_in(state_dir, root);
    let id = dir
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("repo")
        .to_string();
    (id, dir)
}

impl Registry {
    /// A registry over the shared `base` state, making slots with `factory`.
    pub(crate) fn new(base: AppState, factory: Option<Arc<dyn RepoFactory>>) -> Arc<Self> {
        let global = router(base.clone());
        Arc::new(Self {
            dir: base.state_dir.clone(),
            base,
            factory,
            slots: RwLock::new(BTreeMap::new()),
            registering: Mutex::new(()),
            ticking: AtomicBool::new(false),
            global,
        })
    }

    /// The state every slot shares.
    pub(crate) fn base(&self) -> &AppState {
        &self.base
    }

    /// Every registered repository, by id.
    pub fn list(&self) -> Vec<RepoInfo> {
        self.slots
            .read()
            .map(|slots| slots.values().map(|slot| info(slot)).collect())
            .unwrap_or_default()
    }

    /// Registers a repository built by hand (a library caller or a test): its
    /// state is `state` with the repository's own pieces already laid on.
    pub(crate) fn register_state(&self, state: AppState, tick: Option<TickConfig>) -> Arc<Slot> {
        let (id, dir) = slot_dir(&self.dir, &state.repo);
        let (stop, _) = tokio::sync::watch::channel(false);
        let slot = Arc::new(Slot {
            id: id.clone(),
            canon: std::fs::canonicalize(&state.repo).unwrap_or_else(|_| state.repo.clone()),
            root: state.repo.clone(),
            dir,
            router: router(state.clone()),
            state,
            pinned: true,
            tick,
            stop,
            tasks: Mutex::new(Vec::new()),
            prepare: Mutex::new(None),
        });
        if let Ok(mut slots) = self.slots.write() {
            slots.insert(id, Arc::clone(&slot));
        }
        slot
    }

    /// Registers the repository `folder` belongs to, through the factory.
    ///
    /// Idempotent and quick: a repository that is registered is answered from
    /// the table, and a new one is built (nothing slow) and started; its space
    /// and coordinator are prepared on a thread of their own.
    ///
    /// # Errors
    ///
    /// One plain sentence: no factory, a folder that is not there, or the
    /// factory's own refusal.
    pub fn register_folder(&self, folder: &Path, pinned: bool) -> Result<RepoInfo, String> {
        self.register_folder_wait(folder, pinned, false)
    }

    /// [`Registry::register_folder`], optionally running the repository's
    /// preparation (its space and coordinator) before it answers: the service's
    /// own start does, so its banner can say what was prepared.
    ///
    /// # Errors
    ///
    /// As [`Registry::register_folder`].
    pub fn register_folder_wait(
        &self,
        folder: &Path,
        pinned: bool,
        wait: bool,
    ) -> Result<RepoInfo, String> {
        let factory = self.factory.as_ref().ok_or_else(|| {
            "this service was started without a way to serve a repository".to_string()
        })?;
        if !folder.is_dir() {
            return Err(format!(
                "{} is not a folder this computer has",
                folder.display()
            ));
        }
        let folder = folder.to_path_buf();
        let _one_at_a_time = self
            .registering
            .lock()
            .map_err(|_| "registration failed".to_string())?;
        // The root is what the repository's own tools (git, the CLI's `--repo`)
        // call it; the id and the lookups below compare canonical paths.
        let root = factory.root_of(&folder);
        let (id, dir) = slot_dir(&self.dir, &root);
        if let Some(found) = self
            .slots
            .read()
            .ok()
            .and_then(|slots| slots.get(&id).cloned())
        {
            return Ok(info(&found));
        }
        std::fs::create_dir_all(&dir)
            .map_err(|err| format!("could not make {}: {err}", dir.display()))?;
        let hub = Hub::touching(dir.clone());
        // The change file names the log as it stands, so a panel's first look
        // at it is a real answer rather than a missing file.
        let _ = crate::changes::touch(&dir, crate::ledger::last_seq(&root).unwrap_or(0));
        let parts = factory.build(&root, &folder, hub.clone(), &dir)?;
        let state = AppState {
            repo: root.clone(),
            state_dir: dir.clone(),
            hub,
            after_write: parts.after_write,
            engine: parts.engine,
            chat: parts.chat,
            terminal: parts.terminal,
            agents: parts.agents,
            live: parts.live,
            ..self.base.clone()
        };
        let (stop, _) = tokio::sync::watch::channel(false);
        let slot = Arc::new(Slot {
            id: id.clone(),
            canon: std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone()),
            root,
            dir,
            router: router(state.clone()),
            state,
            pinned,
            tick: parts.tick,
            stop,
            tasks: Mutex::new(Vec::new()),
            prepare: Mutex::new(parts.prepare),
        });
        if let Ok(mut slots) = self.slots.write() {
            slots.insert(id, Arc::clone(&slot));
        }
        if self.ticking.load(Ordering::SeqCst) {
            slot.start(None);
        }
        // A repository with a run of its own is prepared at once (its tick may
        // have a task to start); any other waits until it is opened.
        if wait || crate::ledger::last_seq(&slot.root).unwrap_or(0) > 0 {
            slot.run_prepare(wait);
        }
        Ok(info(&slot))
    }

    /// Registers the repository `folder` belongs to and prepares it now: the
    /// window shows its space (or someone sent it a message), so its space and
    /// coordinator are made if they are not yet. Quick like registering: the
    /// preparation runs on its own thread.
    ///
    /// # Errors
    ///
    /// As [`Registry::register_folder`].
    pub fn show(&self, folder: &Path) -> Result<RepoInfo, String> {
        let found = self.register_folder(folder, false)?;
        if let Some(slot) = self
            .slots
            .read()
            .ok()
            .and_then(|slots| slots.get(&found.id).cloned())
        {
            slot.run_prepare(false);
        }
        Ok(found)
    }

    /// Makes `folders` exactly the set of repositories being served, apart from
    /// the pinned ones: new ones are registered, and every other repository
    /// stops being ticked and is dropped (its coordinator keeps its space and
    /// sits idle: nothing is closed).
    ///
    /// Answers the repositories now served and, per folder that failed, why.
    pub fn sync(&self, folders: &[PathBuf]) -> (Vec<RepoInfo>, Vec<(PathBuf, String)>) {
        let mut wanted = Vec::new();
        let mut failed = Vec::new();
        for folder in folders {
            match self.register_folder(folder, false) {
                Ok(found) => wanted.push(found.id),
                Err(why) => failed.push((folder.clone(), why)),
            }
        }
        let stale: Vec<Arc<Slot>> = self
            .slots
            .read()
            .map(|slots| {
                slots
                    .values()
                    .filter(|slot| !slot.pinned && !wanted.contains(&slot.id))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        for slot in stale {
            self.remove(&slot.id);
        }
        (self.list(), failed)
    }

    /// Stops serving the repository `key` names; whether there was one.
    pub fn unregister(&self, key: &str) -> bool {
        match self.find(key) {
            Some(slot) => self.remove(&slot.id),
            None => false,
        }
    }

    fn remove(&self, id: &str) -> bool {
        let removed = self
            .slots
            .write()
            .ok()
            .and_then(|mut slots| slots.remove(id));
        match removed {
            Some(slot) => {
                slot.stop();
                true
            }
            None => false,
        }
    }

    /// The registered repository `key` (an id, a root, or a folder inside one)
    /// names. A path is only compared with registered roots, never opened.
    fn find(&self, key: &str) -> Option<Arc<Slot>> {
        let slots = self.slots.read().ok()?;
        if let Some(slot) = slots.get(key) {
            return Some(Arc::clone(slot));
        }
        if !key.starts_with('/') {
            return None;
        }
        let key_path = std::fs::canonicalize(key).unwrap_or_else(|_| PathBuf::from(key));
        slots
            .values()
            .filter(|slot| key_path.starts_with(&slot.canon) || key_path.starts_with(&slot.root))
            .max_by_key(|slot| slot.root.as_os_str().len())
            .cloned()
    }

    /// The slot a request is for.
    fn resolve(&self, key: Option<&str>) -> Result<Arc<Slot>, ApiReject> {
        match key.filter(|key| !key.is_empty()) {
            Some(key) => self.find(key).ok_or_else(|| {
                ApiReject::not_found(format!(
                    "unknown repo {key:?}: this service serves the repositories its window registered (POST /api/repos adds one)"
                ))
            }),
            None => {
                let slots = self.slots.read().map_err(|_| ApiReject::internal("the repository table is poisoned"))?;
                let mut all = slots.values();
                match (all.next(), all.next()) {
                    (Some(only), None) => Ok(Arc::clone(only)),
                    (None, _) => Err(ApiReject::not_found(
                        "no repository is registered yet: POST /api/repos {\"folder\": …} adds one",
                    )),
                    _ => Err(ApiReject::bad_request(
                        "several repositories are served: name one with ?repo=<id or path>",
                    )),
                }
            }
        }
    }

    /// Starts every slot's log watcher and tick, and every later slot's as it
    /// is registered. `first` replaces the tick of the only repository when a
    /// caller runs one of its own (`Service::run_with_tick`).
    pub(crate) fn start_ticks(&self, first: Option<TickConfig>) {
        self.ticking.store(true, Ordering::SeqCst);
        let slots: Vec<Arc<Slot>> = self
            .slots
            .read()
            .map(|s| s.values().cloned().collect())
            .unwrap_or_default();
        let only = slots.len() == 1;
        for slot in slots {
            slot.start(if only { first.clone() } else { None });
        }
    }

    /// Stops every slot, and waits for the passes in flight to finish.
    pub(crate) async fn stop_all(&self) {
        let slots: Vec<Arc<Slot>> = self
            .slots
            .read()
            .map(|s| s.values().cloned().collect())
            .unwrap_or_default();
        let mut handles = Vec::new();
        for slot in slots {
            handles.extend(slot.stop());
        }
        for handle in handles {
            let _ = handle.await;
        }
    }

    /// The whole API: the registration routes, then the global routes, then a
    /// repository's own router for everything repo-scoped.
    pub fn router(self: &Arc<Self>) -> Router {
        let base = self.base.clone();
        let repos = Router::new()
            .route(
                "/api/repos",
                get(list_repos)
                    .post(add_repo)
                    .put(sync_repos)
                    .delete(drop_repo),
            )
            .layer(middleware::from_fn_with_state(base.clone(), auth::require))
            .layer(middleware::from_fn_with_state(base.clone(), auth::host))
            .layer(middleware::from_fn_with_state(base.clone(), auth::peer))
            .with_state(Arc::clone(self));
        // A request for a repository that does not resolve still passes the
        // gate first, so an unauthenticated caller learns nothing about which
        // repositories are served.
        Router::new()
            .merge(repos)
            .fallback(dispatch)
            .with_state(Arc::clone(self))
    }
}

fn info(slot: &Slot) -> RepoInfo {
    RepoInfo {
        id: slot.id.clone(),
        root: slot.root.display().to_string(),
        dir: slot.dir.display().to_string(),
    }
}

/// The repo-scoped routes: everything a repository's own router answers by
/// itself. Everything else (`health`, `login`, `session`, `tunnels`, `roster`,
/// `prompts`, the web app) is global.
fn is_scoped(path: &str) -> bool {
    const SCOPED: [&str; 12] = [
        "/api/state",
        "/api/changes",
        "/api/asks",
        "/api/chat",
        "/api/terminal",
        "/api/action",
        "/api/graph",
        "/api/tasks",
        "/api/settings",
        "/api/coordinator",
        "/api/agents",
        "/api/ws",
    ];
    SCOPED.iter().any(|prefix| {
        path == *prefix
            || path
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.starts_with('/'))
    })
}

/// The repository a request names: `?repo=`, else the header.
fn repo_key(request: &Request) -> Option<String> {
    let query = Query::<HashMap<String, String>>::try_from_uri(request.uri())
        .ok()
        .and_then(|Query(map)| map.get("repo").cloned());
    query.or_else(|| {
        request
            .headers()
            .get(REPO_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    })
}

async fn call(router: Router, request: Request) -> Response {
    match router.oneshot(request).await {
        Ok(response) => response,
        Err(never) => match never {},
    }
}

/// What a request that resolved no repository carries to the gate.
#[derive(Clone)]
struct Refusal(ApiReject);

async fn dispatch(State(registry): State<Arc<Registry>>, mut request: Request) -> Response {
    if !is_scoped(request.uri().path()) {
        return call(registry.global.clone(), request).await;
    }
    match registry.resolve(repo_key(&request).as_deref()) {
        Ok(slot) => call(slot.router.clone(), request).await,
        Err(reject) => {
            request.extensions_mut().insert(Refusal(reject));
            call(refusal_router(registry.base().clone()), request).await
        }
    }
}

/// A router whose only answer is the refusal in the request, behind the gate.
fn refusal_router(base: AppState) -> Router {
    async fn refuse(Extension(Refusal(reject)): Extension<Refusal>) -> Response {
        reject.into_response()
    }
    Router::new()
        .fallback(refuse)
        .layer(middleware::from_fn_with_state(base.clone(), auth::require))
        .layer(middleware::from_fn_with_state(base.clone(), auth::host))
        .layer(middleware::from_fn_with_state(base, auth::peer))
}

/// A registration needs the token door: it starts an agent in a named folder.
fn token_only(door: &shprd_remote::Door) -> Result<(), ApiReject> {
    if door.is_session() {
        return Err(ApiReject::new(
            StatusCode::FORBIDDEN,
            ErrorCode::Unauthorized,
            "registering a repository is the desktop's door only, not a web session's",
        ));
    }
    Ok(())
}

#[derive(Serialize)]
struct ReposDto {
    repos: Vec<RepoInfo>,
}

async fn list_repos(State(registry): State<Arc<Registry>>) -> Json<ReposDto> {
    Json(ReposDto {
        repos: registry.list(),
    })
}

#[derive(Deserialize)]
struct FolderBody {
    folder: String,
}

async fn add_repo(
    State(registry): State<Arc<Registry>>,
    Extension(door): Extension<shprd_remote::Door>,
    Json(body): Json<FolderBody>,
) -> Result<Json<RepoInfo>, ApiReject> {
    token_only(&door)?;
    let found = tokio::task::spawn_blocking(move || registry.show(Path::new(&body.folder)))
        .await
        .map_err(|err| ApiReject::internal(err.to_string()))?
        .map_err(ApiReject::bad_request)?;
    Ok(Json(found))
}

#[derive(Deserialize)]
struct FoldersBody {
    folders: Vec<String>,
}

#[derive(Serialize)]
struct SyncDto {
    repos: Vec<RepoInfo>,
    failed: Vec<FailedFolder>,
}

#[derive(Serialize)]
struct FailedFolder {
    folder: String,
    reason: String,
}

async fn sync_repos(
    State(registry): State<Arc<Registry>>,
    Extension(door): Extension<shprd_remote::Door>,
    Json(body): Json<FoldersBody>,
) -> Result<Json<SyncDto>, ApiReject> {
    token_only(&door)?;
    let folders: Vec<PathBuf> = body.folders.iter().map(PathBuf::from).collect();
    let (repos, failed) = tokio::task::spawn_blocking(move || registry.sync(&folders))
        .await
        .map_err(|err| ApiReject::internal(err.to_string()))?;
    Ok(Json(SyncDto {
        repos,
        failed: failed
            .into_iter()
            .map(|(folder, reason)| FailedFolder {
                folder: folder.display().to_string(),
                reason,
            })
            .collect(),
    }))
}

async fn drop_repo(
    State(registry): State<Arc<Registry>>,
    Extension(door): Extension<shprd_remote::Door>,
    Json(body): Json<FolderBody>,
) -> Result<StatusCode, ApiReject> {
    token_only(&door)?;
    if registry.unregister(&body.folder) {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiReject::not_found(format!(
            "no repository {:?} is registered",
            body.folder
        )))
    }
}
