//! The tunnel manager: one tunnel at a time, behind the password rule.
//!
//! It is the only code that starts a tunnel, and it holds every rule M7d §4.3
//! gives the lifecycle in one place, so the CLI, the HTTP routes and a test all
//! get them whichever door they came in by:
//!
//! * **fail closed** — a provider on the public internet does not start unless a
//!   login password is set, and a password file that cannot be read counts as
//!   none (there is no `--force`, and nothing here takes a flag that bypasses
//!   it). The check runs before anything is spawned, and again once the tunnel
//!   is up, because the password may have been cleared while it started;
//! * **one at a time** — a second start is refused, not queued;
//! * **the allowlist follows the tunnel** — the endpoint's host is added to the
//!   `Host` allowlist when the tunnel comes up and removed when it goes down,
//!   however it goes down;
//! * **a storm stops it** — a supervisor stops the tunnel when the sign-in
//!   limits that mean "someone is guessing" trip ([`StormSignal`]), and when the
//!   tunnel's own process dies;
//! * **its record is truthful** — `web.json` says on only while a tunnel is.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use shprd_core::log::Clock;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use super::parse::mask_url;
use super::state::{WebFile, qr_svg};
use super::{Edge, Options, Provider, Tunnel, TunnelError, TunnelFactory, TunnelState};
use crate::hosts::AllowedHosts;
use crate::limits::StormSignal;
use crate::password::PasswordStore;

/// How often the supervisor looks at the tunnel and the storm signal.
const POLL: Duration = Duration::from_secs(1);

/// What `web up` asks for.
#[derive(Debug, Clone)]
pub struct StartRequest {
    /// Which provider.
    pub provider: Provider,
    /// The settings that shape it.
    pub options: Options,
}

/// What the manager holds while a tunnel is up.
struct Slot {
    /// The running tunnel.
    tunnel: Option<Box<dyn Tunnel>>,
    /// The host it added to the allowlist.
    host: Option<String>,
    /// The supervisor watching it.
    supervisor: Option<JoinHandle<()>>,
    /// Which tunnel the supervisor belongs to: a supervisor that finds a newer
    /// generation than its own does nothing.
    generation: u64,
}

/// What a status call reads without waiting for a start that is in progress.
#[derive(Default)]
struct View {
    file: WebFile,
    edge: Option<Edge>,
    starting: bool,
}

struct Inner {
    state_dir: PathBuf,
    hosts: AllowedHosts,
    password: PasswordStore,
    storm: StormSignal,
    clock: Arc<dyn Clock + Send + Sync>,
    factory: Arc<dyn TunnelFactory>,
    poll: Duration,
    slot: Mutex<Slot>,
    view: RwLock<View>,
}

/// Starts, watches and stops the one tunnel.
#[derive(Clone)]
pub struct TunnelManager {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for TunnelManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TunnelManager")
    }
}

impl TunnelManager {
    /// A manager over `gateway`'s allowlist, password and storm signal, writing
    /// `web.json` in `state_dir`, building tunnels with `factory`.
    pub fn new(
        state_dir: &std::path::Path,
        gateway: &crate::Gateway,
        clock: Arc<dyn Clock + Send + Sync>,
        factory: Arc<dyn TunnelFactory>,
    ) -> Self {
        Self::with_poll(state_dir, gateway, clock, factory, POLL)
    }

    /// [`TunnelManager::new`] with another supervisor interval, so a test does
    /// not wait a second per look.
    pub fn with_poll(
        state_dir: &std::path::Path,
        gateway: &crate::Gateway,
        clock: Arc<dyn Clock + Send + Sync>,
        factory: Arc<dyn TunnelFactory>,
        poll: Duration,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                state_dir: state_dir.to_path_buf(),
                hosts: gateway.hosts(),
                password: gateway.password().clone(),
                storm: gateway.storm(),
                clock,
                factory,
                poll,
                slot: Mutex::new(Slot {
                    tunnel: None,
                    host: None,
                    supervisor: None,
                    generation: 0,
                }),
                view: RwLock::new(View::default()),
            }),
        }
    }

    /// Forgets what a service that did not stop cleanly left in `web.json`.
    ///
    /// A new service has no tunnel, whatever the file says; leaving "on" there
    /// would show a link that goes nowhere.
    pub fn reset(&self) {
        let was = WebFile::read(&self.inner.state_dir);
        let why = was
            .on
            .then(|| "the service restarted, so its tunnel ended".to_owned());
        self.inner.record_off(why);
    }

    /// The record, as `web.json` would hold it right now.
    pub fn file(&self) -> WebFile {
        self.inner.view().file.clone()
    }

    /// The endpoint the gate's side needs, while a tunnel is up.
    pub fn edge(&self) -> Option<Edge> {
        self.inner.view().edge.clone()
    }

    /// Whether a tunnel is being started.
    pub fn starting(&self) -> bool {
        self.inner.view().starting
    }

    /// Starts the tunnel to `local`, or says why it did not.
    ///
    /// Every refusal is a [`TunnelError`] whose words carry no address, and every
    /// refusal leaves `web.json` saying off, with the reason.
    pub async fn start(
        &self,
        request: StartRequest,
        local: SocketAddr,
    ) -> Result<WebFile, TunnelError> {
        let inner = &self.inner;
        let mut slot = inner
            .slot
            .try_lock()
            .map_err(|_| TunnelError::Busy("starting or stopping"))?;
        if slot.tunnel.is_some() {
            return Err(TunnelError::Busy("up"));
        }
        // Fail closed before anything is looked for or spawned. The refusal is
        // recorded, so the remote box can show why there is no tunnel.
        if request.provider.public_internet() {
            inner
                .require_password()
                .inspect_err(|err| inner.record_off(Some(err.to_string())))?;
        }
        let mut tunnel = inner
            .factory
            .create(request.provider, &request.options)
            .inspect_err(|err| inner.record_off(Some(err.to_string())))?;
        // The tunnel's own claim is checked too: a provider that says it is
        // public is held to the rule whatever the table above said about it.
        if tunnel.public_internet() {
            inner
                .require_password()
                .inspect_err(|err| inner.record_off(Some(err.to_string())))?;
        }

        inner.set_starting(true);
        let version = tunnel.version().await;
        tracing::info!(
            provider = request.provider.word(),
            version = version.as_deref().unwrap_or("unknown"),
            "starting a tunnel"
        );
        let started = tunnel.start(local).await;
        inner.set_starting(false);
        let endpoint = match started {
            Ok(endpoint) => endpoint,
            Err(err) => {
                let _ = tunnel.stop().await;
                inner.record_off(Some(err.to_string()));
                return Err(err);
            }
        };

        // The password may have been cleared while the tunnel came up; a public
        // address with no password behind it must not survive that.
        if endpoint.public_internet
            && let Err(err) = inner.require_password()
        {
            let _ = tunnel.stop().await;
            inner.record_off(Some(err.to_string()));
            return Err(err);
        }
        if let Err(err) = inner.hosts.add(&endpoint.host) {
            let _ = tunnel.stop().await;
            let err = TunnelError::Failed(format!("the tunnel's host cannot be allowed: {err}"));
            inner.record_off(Some(err.to_string()));
            return Err(err);
        }
        let file = WebFile {
            on: true,
            provider: Some(request.provider.word().to_owned()),
            link: Some(endpoint.url().to_owned()),
            qr_svg: qr_svg(endpoint.url()),
            why: None,
            at: inner.now(),
            version,
        };
        if let Err(err) = file.write(&inner.state_dir) {
            inner.hosts.remove(&endpoint.host);
            let _ = tunnel.stop().await;
            let err = TunnelError::Failed(format!("could not write web.json: {err}"));
            inner.record_off(Some(err.to_string()));
            return Err(err);
        }
        tracing::info!(
            provider = request.provider.word(),
            link = %mask_url(endpoint.url()),
            public_internet = endpoint.public_internet,
            "the tunnel is up"
        );

        slot.generation += 1;
        let generation = slot.generation;
        slot.host = Some(endpoint.host.clone());
        slot.tunnel = Some(tunnel);
        slot.supervisor = Some(tokio::spawn(supervise(
            inner.clone(),
            generation,
            inner.storm.count(),
        )));
        {
            let mut view = inner.view_mut();
            view.file = file.clone();
            view.edge = Some(Edge {
                provider: request.provider,
                host: endpoint.host,
            });
        }
        Ok(file)
    }

    /// Stops the tunnel, removes its host from the allowlist and records why.
    ///
    /// Stopping when nothing is up is not an error. A provider that fails to
    /// stop cleanly (Tailscale's `serve off`) still has its host removed and
    /// its record written, and the error is returned after.
    pub async fn stop(&self, why: Option<&str>) -> Result<WebFile, TunnelError> {
        let mut slot = self.inner.slot.lock().await;
        self.inner
            .end(&mut slot, why.map(str::to_owned), false)
            .await?;
        Ok(self.file())
    }

    /// Stops the tunnel on the service's way out. Errors are logged, not
    /// returned: there is nothing left to tell.
    pub async fn shutdown(&self) {
        if let Err(err) = self.stop(None).await {
            tracing::warn!(%err, "the tunnel did not stop cleanly");
        }
    }
}

impl Inner {
    fn view(&self) -> std::sync::RwLockReadGuard<'_, View> {
        match self.view.read() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn view_mut(&self) -> std::sync::RwLockWriteGuard<'_, View> {
        match self.view.write() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn set_starting(&self, starting: bool) {
        self.view_mut().starting = starting;
    }

    fn now(&self) -> u64 {
        self.clock.now_ms() / 1000
    }

    /// Refuses unless a password is set. A file that cannot be read is not
    /// "set": the reason says so, because the person has to fix it.
    fn require_password(&self) -> Result<(), TunnelError> {
        match self.password.is_set() {
            Ok(true) => Ok(()),
            Ok(false) => Err(TunnelError::NeedsPassword(
                "this tunnel is on the public internet, and no login password is set: \
                 run `shprd web password set` first (there is no way around this)"
                    .to_owned(),
            )),
            Err(_) => Err(TunnelError::NeedsPassword(
                "this tunnel is on the public internet, and the login password could not be \
                 read, which counts as none: run `shprd web password set` to set it again"
                    .to_owned(),
            )),
        }
    }

    /// Writes the off record, with `why`, and clears the view.
    fn record_off(&self, why: Option<String>) {
        let file = WebFile {
            on: false,
            why,
            at: self.now(),
            ..WebFile::default()
        };
        if let Err(err) = file.write(&self.state_dir) {
            tracing::warn!(%err, "could not write web.json");
        }
        let mut view = self.view_mut();
        view.file = file;
        view.edge = None;
    }

    /// Ends the tunnel in `slot`. `own` is true when the caller is the
    /// supervisor itself, which must not abort its own task.
    async fn end(
        &self,
        slot: &mut Slot,
        why: Option<String>,
        own: bool,
    ) -> Result<(), TunnelError> {
        if let Some(handle) = slot.supervisor.take()
            && !own
        {
            handle.abort();
        }
        slot.generation += 1;
        let stopped = match slot.tunnel.take() {
            Some(mut tunnel) => tunnel.stop().await,
            None => Ok(()),
        };
        if let Some(host) = slot.host.take() {
            self.hosts.remove(&host);
        }
        let why = match (&stopped, why) {
            (Err(err), _) => Some(err.to_string()),
            (Ok(()), why) => why,
        };
        self.record_off(why);
        stopped
    }
}

/// Watches one tunnel until it ends, and ends it when it must.
async fn supervise(inner: Arc<Inner>, generation: u64, baseline: u64) {
    loop {
        tokio::time::sleep(inner.poll).await;
        let mut slot = inner.slot.lock().await;
        if slot.generation != generation {
            return;
        }
        if inner.storm.count() > baseline {
            tracing::warn!("a burst of failed sign-ins: stopping the tunnel");
            let _ = inner
                .end(
                    &mut slot,
                    Some("stopped after a burst of failed sign-ins".to_owned()),
                    true,
                )
                .await;
            return;
        }
        let Some(tunnel) = slot.tunnel.as_mut() else {
            return;
        };
        if let TunnelState::Failed { why } = tunnel.status().await {
            tracing::warn!("the tunnel ended on its own");
            let _ = inner.end(&mut slot, Some(why), true).await;
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use shprd_core::log::SystemClock;

    use super::*;
    use crate::tunnel::{BoxFuture, PublicEndpoint};

    /// What the fake tunnels were asked, shared with the test.
    #[derive(Default)]
    struct Log {
        created: AtomicUsize,
        started: AtomicUsize,
        stopped: AtomicUsize,
        /// What the next tunnel's `status` answers.
        dead: StdMutex<Option<String>>,
        /// Run just before the tunnel reports its address.
        during_start: StdMutex<Option<Box<dyn Fn() + Send>>>,
        /// The name the next tunnel is reached at.
        host: StdMutex<String>,
    }

    struct Fake {
        provider: Provider,
        log: Arc<Log>,
        claims_public: bool,
    }

    impl Tunnel for Fake {
        fn provider(&self) -> Provider {
            self.provider
        }
        fn public_internet(&self) -> bool {
            self.claims_public
        }
        fn version(&self) -> BoxFuture<'_, Option<String>> {
            Box::pin(async { Some("fake 1.0".to_owned()) })
        }
        fn start(
            &mut self,
            _local: SocketAddr,
        ) -> BoxFuture<'_, Result<PublicEndpoint, TunnelError>> {
            Box::pin(async move {
                self.log.started.fetch_add(1, Ordering::SeqCst);
                if let Some(hook) = self.log.during_start.lock().unwrap().take() {
                    hook();
                }
                let host = self.log.host.lock().unwrap().clone();
                Ok(PublicEndpoint::new(&format!("https://{host}"), self.provider).unwrap())
            })
        }
        fn status(&mut self) -> BoxFuture<'_, TunnelState> {
            Box::pin(async move {
                match self.log.dead.lock().unwrap().clone() {
                    Some(why) => TunnelState::Failed { why },
                    None => TunnelState::Starting,
                }
            })
        }
        fn stop(&mut self) -> BoxFuture<'_, Result<(), TunnelError>> {
            Box::pin(async move {
                self.log.stopped.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        }
    }

    struct FakeFactory {
        log: Arc<Log>,
        /// Whether every tunnel claims to be public whatever its provider says.
        claims_public: bool,
    }

    impl TunnelFactory for FakeFactory {
        fn create(
            &self,
            provider: Provider,
            _options: &Options,
        ) -> Result<Box<dyn Tunnel>, TunnelError> {
            self.log.created.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(Fake {
                provider,
                log: self.log.clone(),
                claims_public: self.claims_public || provider.public_internet(),
            }))
        }
    }

    struct Rig {
        dir: tempfile::TempDir,
        gateway: crate::Gateway,
        manager: TunnelManager,
        log: Arc<Log>,
    }

    fn rig_with(claims_public: bool) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let clock: Arc<dyn Clock + Send + Sync> = Arc::new(SystemClock);
        let gateway = crate::Gateway::new(dir.path(), clock.clone());
        let log = Arc::new(Log::default());
        *log.host.lock().unwrap() = "first-name.trycloudflare.com".to_owned();
        let factory = Arc::new(FakeFactory {
            log: log.clone(),
            claims_public,
        });
        let manager = TunnelManager::with_poll(
            dir.path(),
            &gateway,
            clock,
            factory,
            Duration::from_millis(20),
        );
        Rig {
            dir,
            gateway,
            manager,
            log,
        }
    }

    fn rig() -> Rig {
        rig_with(false)
    }

    fn set_password(rig: &Rig) {
        rig.gateway
            .password()
            .set_with_cost("correct horse battery staple", 8, 1, 1)
            .unwrap();
    }

    fn request(provider: Provider) -> StartRequest {
        StartRequest {
            provider,
            options: Options::default(),
        }
    }

    fn local() -> SocketAddr {
        "127.0.0.1:18080".parse().unwrap()
    }

    async fn until(mut condition: impl FnMut() -> bool) {
        for _ in 0..200 {
            if condition() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("the condition never held");
    }

    #[tokio::test]
    async fn a_public_tunnel_does_not_start_without_a_password() {
        let rig = rig();
        for provider in [
            Provider::CloudflaredQuick,
            Provider::CloudflaredNamed,
            Provider::Ngrok,
            Provider::Tunnl,
        ] {
            let err = rig
                .manager
                .start(request(provider), local())
                .await
                .unwrap_err();
            assert!(
                matches!(err, TunnelError::NeedsPassword(_)),
                "{provider:?}: {err}"
            );
        }
        // Nothing was even built, let alone run, and nothing was allowed.
        assert_eq!(rig.log.created.load(Ordering::SeqCst), 0);
        assert_eq!(rig.log.started.load(Ordering::SeqCst), 0);
        assert!(rig.gateway.hosts().list().is_empty());
        let file = WebFile::read(rig.dir.path());
        assert!(!file.on);
        assert!(
            file.why
                .unwrap_or_default()
                .contains("no login password is set")
        );
    }

    #[tokio::test]
    async fn an_unreadable_password_file_counts_as_no_password() {
        let rig = rig();
        std::fs::write(rig.gateway.password().path(), "{ this is not json").unwrap();
        let err = rig
            .manager
            .start(request(Provider::Tunnl), local())
            .await
            .unwrap_err();
        assert!(matches!(err, TunnelError::NeedsPassword(_)), "{err}");
        assert!(err.to_string().contains("could not be read"), "{err}");
        assert_eq!(rig.log.created.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn tailscale_may_run_without_a_password() {
        let rig = rig();
        *rig.log.host.lock().unwrap() = "studio.tail1234.ts.net".to_owned();
        let file = rig
            .manager
            .start(request(Provider::Tailscale), local())
            .await
            .unwrap();
        assert!(file.on);
        assert_eq!(rig.gateway.hosts().list(), ["studio.tail1234.ts.net"]);
        rig.manager.stop(None).await.unwrap();
    }

    #[tokio::test]
    async fn a_tunnel_that_claims_to_be_public_is_held_to_the_rule_whatever_its_provider() {
        let rig = rig_with(true);
        let err = rig
            .manager
            .start(request(Provider::Tailscale), local())
            .await
            .unwrap_err();
        assert!(matches!(err, TunnelError::NeedsPassword(_)), "{err}");
        assert_eq!(rig.log.started.load(Ordering::SeqCst), 0, "nothing ran");
    }

    #[tokio::test]
    async fn start_allows_the_host_and_writes_a_private_record_and_stop_undoes_both() {
        use std::os::unix::fs::PermissionsExt;
        let rig = rig();
        set_password(&rig);
        let file = rig
            .manager
            .start(request(Provider::CloudflaredQuick), local())
            .await
            .unwrap();
        assert!(file.on);
        assert_eq!(file.provider.as_deref(), Some("cloudflared"));
        assert_eq!(
            file.link.as_deref(),
            Some("https://first-name.trycloudflare.com")
        );
        assert!(file.qr_svg.as_deref().unwrap_or("").contains("<svg"));
        assert_eq!(file.version.as_deref(), Some("fake 1.0"));
        assert_eq!(rig.gateway.hosts().list(), ["first-name.trycloudflare.com"]);
        assert!(
            rig.gateway
                .host_allowed(Some("first-name.trycloudflare.com"))
        );
        assert!(!rig.gateway.host_allowed(Some("other.trycloudflare.com")));
        assert_eq!(WebFile::read(rig.dir.path()), file);
        let mode = std::fs::metadata(WebFile::path(rig.dir.path()))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(
            rig.manager.edge().map(|edge| edge.host),
            Some("first-name.trycloudflare.com".to_owned())
        );

        let stopped = rig.manager.stop(Some("web down")).await.unwrap();
        assert!(!stopped.on);
        assert!(rig.gateway.hosts().list().is_empty());
        assert!(
            !rig.gateway
                .host_allowed(Some("first-name.trycloudflare.com"))
        );
        assert_eq!(rig.log.stopped.load(Ordering::SeqCst), 1);
        let after = WebFile::read(rig.dir.path());
        assert!(!after.on);
        assert_eq!(after.link, None, "the off record keeps no address");
        assert_eq!(after.why.as_deref(), Some("web down"));
        assert!(rig.manager.edge().is_none());
        // A second stop is not an error and stops nothing more.
        rig.manager.stop(None).await.unwrap();
        assert_eq!(rig.log.stopped.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn one_tunnel_at_a_time_and_a_restart_replaces_the_host() {
        let rig = rig();
        set_password(&rig);
        rig.manager
            .start(request(Provider::CloudflaredQuick), local())
            .await
            .unwrap();
        let err = rig
            .manager
            .start(request(Provider::Tunnl), local())
            .await
            .unwrap_err();
        assert!(matches!(err, TunnelError::Busy(_)), "{err}");
        assert_eq!(rig.gateway.hosts().list(), ["first-name.trycloudflare.com"]);

        rig.manager.stop(None).await.unwrap();
        *rig.log.host.lock().unwrap() = "second-name.trycloudflare.com".to_owned();
        rig.manager
            .start(request(Provider::CloudflaredQuick), local())
            .await
            .unwrap();
        assert_eq!(
            rig.gateway.hosts().list(),
            ["second-name.trycloudflare.com"],
            "the old name is gone, the new one is allowed"
        );
        rig.manager.stop(None).await.unwrap();
    }

    #[tokio::test]
    async fn a_password_cleared_while_the_tunnel_starts_stops_it_again() {
        let rig = rig();
        set_password(&rig);
        let store = rig.gateway.password().clone();
        *rig.log.during_start.lock().unwrap() = Some(Box::new(move || {
            store.clear().unwrap();
        }));
        let err = rig
            .manager
            .start(request(Provider::CloudflaredQuick), local())
            .await
            .unwrap_err();
        assert!(matches!(err, TunnelError::NeedsPassword(_)), "{err}");
        assert_eq!(rig.log.stopped.load(Ordering::SeqCst), 1, "it was stopped");
        assert!(rig.gateway.hosts().list().is_empty());
        assert!(!WebFile::read(rig.dir.path()).on);
    }

    #[tokio::test]
    async fn a_storm_of_failed_sign_ins_stops_the_tunnel_and_closes_the_host() {
        let rig = rig();
        set_password(&rig);
        rig.manager
            .start(request(Provider::CloudflaredQuick), local())
            .await
            .unwrap();
        // The global login limit trips: the same signal the real limiter sets.
        rig.gateway.storm().trip();
        until(|| !rig.manager.file().on).await;
        assert!(rig.gateway.hosts().list().is_empty());
        let file = WebFile::read(rig.dir.path());
        assert!(!file.on);
        assert!(file.why.unwrap_or_default().contains("failed sign-ins"));
        assert_eq!(rig.log.stopped.load(Ordering::SeqCst), 1);
        // And a new tunnel can be started afterwards.
        rig.manager
            .start(request(Provider::CloudflaredQuick), local())
            .await
            .unwrap();
        rig.manager.stop(None).await.unwrap();
    }

    #[tokio::test]
    async fn the_real_global_limit_trips_the_storm_that_stops_it() {
        let rig = rig();
        set_password(&rig);
        rig.manager
            .start(request(Provider::CloudflaredQuick), local())
            .await
            .unwrap();
        // Thirty attempts from thirty addresses, then the thirty-first.
        for n in 0..crate::limits::LOGIN_GLOBAL {
            let ip = std::net::IpAddr::from([10, 0, 1, u8::try_from(n).unwrap()]);
            rig.gateway.limits().login(ip).unwrap();
        }
        rig.gateway
            .limits()
            .login(std::net::IpAddr::from([10, 0, 2, 1]))
            .unwrap_err();
        until(|| !rig.manager.file().on).await;
        assert!(rig.gateway.hosts().list().is_empty());
    }

    #[tokio::test]
    async fn a_tunnel_that_dies_on_its_own_is_cleaned_up() {
        let rig = rig();
        set_password(&rig);
        rig.manager
            .start(request(Provider::CloudflaredQuick), local())
            .await
            .unwrap();
        *rig.log.dead.lock().unwrap() = Some("the cloudflared process ended (exit 1)".into());
        until(|| !rig.manager.file().on).await;
        assert!(rig.gateway.hosts().list().is_empty());
        assert_eq!(
            WebFile::read(rig.dir.path()).why.as_deref(),
            Some("the cloudflared process ended (exit 1)")
        );
    }

    #[tokio::test]
    async fn reset_clears_a_record_a_dead_service_left_on() {
        let rig = rig();
        WebFile {
            on: true,
            link: Some("https://old-one.trycloudflare.com".into()),
            ..WebFile::default()
        }
        .write(rig.dir.path())
        .unwrap();
        rig.manager.reset();
        let file = WebFile::read(rig.dir.path());
        assert!(!file.on);
        assert_eq!(file.link, None);
        assert!(file.why.unwrap().contains("restarted"));
    }

    #[tokio::test]
    async fn a_failed_start_is_recorded_off_with_the_reason() {
        struct Broken;
        impl TunnelFactory for Broken {
            fn create(
                &self,
                provider: Provider,
                _: &Options,
            ) -> Result<Box<dyn Tunnel>, TunnelError> {
                Err(TunnelError::NotInstalled {
                    program: provider.program(),
                })
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let clock: Arc<dyn Clock + Send + Sync> = Arc::new(SystemClock);
        let gateway = crate::Gateway::new(dir.path(), clock.clone());
        gateway
            .password()
            .set_with_cost("correct horse battery staple", 8, 1, 1)
            .unwrap();
        let manager = TunnelManager::new(dir.path(), &gateway, clock, Arc::new(Broken));
        let err = manager
            .start(request(Provider::Ngrok), local())
            .await
            .unwrap_err();
        assert!(matches!(err, TunnelError::NotInstalled { .. }));
        let file = WebFile::read(dir.path());
        assert!(!file.on);
        assert!(file.why.unwrap().contains("ngrok is not installed"));
    }
}
