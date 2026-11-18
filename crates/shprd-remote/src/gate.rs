//! The gate: one decision in front of every route that is not health or login.
//!
//! A request is allowed by **either** door —
//!
//! 1. the bearer token in the state directory, which is the mods' and the CLI's
//!    door and is unchanged by this milestone; or
//! 2. a valid session cookie, which is a browser that entered the password.
//!
//! — and then, for a write or a socket upgrade, only if:
//!
//! * its `Origin` is the page's own origin, which is to say the `Host` this
//!   service is being reached under, so a page from anywhere else cannot post a
//!   form here (threat row 5);
//! * it carries `x-shprd: 1`, a header a form post cannot set — except on a
//!   socket upgrade, where a browser sets the handshake's headers itself and the
//!   `Origin` check above is the rule;
//! * the rate limits for that door allow it.
//!
//! Before any of that, the `Host` must be one this service answers to — a
//! loopback name, or a host a tunnel added ([`crate::hosts::AllowedHosts`]).
//! An unknown `Host` is a `421`, which is what DNS rebinding looks like from
//! inside the process (threat row 6).
//!
//! Two differences between the doors are deliberate:
//!
//! * the token door's `Origin`, when it sends one, must be loopback: a browser
//!   page is never the token's business;
//! * a destructive action needs a **fresh** password through the session door
//!   (five minutes) and is exempt on the token door, because the token is
//!   already local and physical (threat rows 2 and 4).
//!
//! Every refusal is a variant of [`GateError`], which the server turns into the
//! status and body a client sees. Nothing in this module is allowed to fall
//! through to "allowed".

use std::net::IpAddr;
use std::sync::Arc;

use shprd_core::log::Clock;

use crate::cookie;
use crate::dto::SessionInfoDto;
use crate::error::RemoteError;
use crate::hosts::{self, AllowedHosts};
use crate::limits::{RateLimiter, Retry};
use crate::password::PasswordStore;
use crate::session::{SessionRecord, Sessions};

/// Why a request was refused.
///
/// The server maps each variant to a status: `421` for a wrong `Host`, `401` for
/// a missing credential, `403` for a wrong `Origin` or a missing header, `429`
/// with `Retry-After` for a rate limit, `401 {"code":"reauth"}` for a stale
/// password, and `500` for a state file that could not be read — which is a
/// refusal, never a pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateError {
    /// The `Host` is not one this service answers to.
    WrongHost {
        /// The host the request carried, when it carried one.
        host: Option<String>,
    },
    /// A write's `Origin` is not this page's own origin.
    WrongOrigin {
        /// The origin the request carried, when it carried one.
        origin: Option<String>,
    },
    /// A write did not carry `x-shprd: 1`.
    MissingHeader,
    /// No credential, or one that is not valid any more.
    Unauthorized,
    /// A destructive action, and the password was not entered in the last five
    /// minutes through this session.
    Reauth,
    /// A rate limit refused the request.
    TooManyRequests {
        /// How long the client must wait, in milliseconds.
        after_ms: u64,
    },
    /// A state file could not be read or written, so the request is refused
    /// rather than guessed at.
    Failed(String),
}

/// Which door a request came through, once it is allowed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Door {
    /// The loopback bearer token.
    Token,
    /// A browser's session, as it stood when the request was allowed.
    Session(SessionRecord),
}

impl Door {
    /// Whether this door is a browser session.
    pub fn is_session(&self) -> bool {
        matches!(self, Self::Session(_))
    }

    /// The session this door names, if it is one.
    pub fn session(&self) -> Option<&SessionRecord> {
        match self {
            Self::Token => None,
            Self::Session(record) => Some(record),
        }
    }
}

/// Everything the gate needs to know about one request.
///
/// It is a plain struct rather than an HTTP type so the decision table is a unit
/// test: `shprd-server` fills it from headers and an extension, and this crate
/// never imports a web framework.
#[derive(Debug, Clone, Copy)]
pub struct GateRequest<'a> {
    /// The `Host` header.
    pub host: Option<&'a str>,
    /// The method, as the request spells it.
    pub method: &'a str,
    /// The `Origin` header, when there is one.
    pub origin: Option<&'a str>,
    /// The bearer token, when there is one.
    pub bearer: Option<&'a str>,
    /// The `Cookie` header, when there is one.
    pub cookies: Option<&'a str>,
    /// Whether the request carries `x-shprd: 1`.
    pub write_header: bool,
    /// Whether the request asks to upgrade to a WebSocket.
    pub upgrade: bool,
    /// The client's address, which is what a rate limit is keyed on: the
    /// socket's peer, or over the active tunnel the address its edge reported.
    pub peer: IpAddr,
}

impl<'a> GateRequest<'a> {
    /// A read request with no credential: the base a test or a caller varies.
    pub fn read(peer: IpAddr) -> Self {
        Self {
            host: None,
            method: "GET",
            origin: None,
            bearer: None,
            cookies: None,
            write_header: false,
            upgrade: false,
            peer,
        }
    }

    /// Whether this request changes something: a write method, or a socket.
    pub fn is_write(&self) -> bool {
        self.upgrade || !matches!(self.method, "GET" | "HEAD" | "OPTIONS")
    }

    /// The session id this request's cookie carries.
    pub fn session_id(&self) -> Option<&'a str> {
        cookie::session_id(self.cookies)
    }
}

/// What a successful login produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginOutcome {
    /// The id the cookie must carry. It is returned once and never stored.
    pub id: String,
    /// The session it names, as it now stands.
    pub record: SessionRecord,
    /// True when an existing session was refreshed rather than a new one made.
    pub refreshed: bool,
}

/// The gate and the things behind it: the password, the sessions, the limits and
/// the hostnames this service answers to.
#[derive(Clone)]
pub struct Gateway {
    password: PasswordStore,
    sessions: Sessions,
    limits: RateLimiter,
    hosts: AllowedHosts,
    clock: Arc<dyn Clock + Send + Sync>,
    token: Option<String>,
}

impl std::fmt::Debug for Gateway {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Gateway")
            .field("password", &self.password)
            .field("sessions", &self.sessions)
            .finish_non_exhaustive()
    }
}

impl Gateway {
    /// A gateway over `state_dir`, counting against `clock`.
    ///
    /// The token door is closed until [`Gateway::with_token`] names the token:
    /// a gateway nobody gave one to can only allow sessions.
    pub fn new(state_dir: &std::path::Path, clock: Arc<dyn Clock + Send + Sync>) -> Self {
        Self {
            password: PasswordStore::new(state_dir),
            sessions: Sessions::new(state_dir, clock.clone()),
            limits: RateLimiter::new(clock.clone()),
            hosts: AllowedHosts::loopback_only(),
            clock,
            token: None,
        }
    }

    /// Accepts `token` as the loopback door, as the service's own token file
    /// does.
    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    /// Answers to the names in `hosts` as well as loopback.
    ///
    /// The handle is shared: the tunnel task adds and removes a quick tunnel's
    /// hostname through it, and the gate sees the change at once.
    pub fn with_hosts(mut self, hosts: AllowedHosts) -> Self {
        self.hosts = hosts;
        self
    }

    /// The password store.
    pub fn password(&self) -> &PasswordStore {
        &self.password
    }

    /// The session store.
    pub fn sessions(&self) -> &Sessions {
        &self.sessions
    }

    /// Reports the sign-in limits that mean "someone is guessing" to `storm`.
    ///
    /// The tunnel manager holds the same handle and stops the tunnel when it
    /// moves.
    pub fn with_storm(mut self, storm: crate::limits::StormSignal) -> Self {
        self.limits = self.limits.with_storm(storm);
        self
    }

    /// The signal the sign-in limits report to.
    pub fn storm(&self) -> crate::limits::StormSignal {
        self.limits.storm()
    }

    /// The rate limits.
    pub fn limits(&self) -> &RateLimiter {
        &self.limits
    }

    /// The names this service answers to, as a shared handle.
    pub fn hosts(&self) -> AllowedHosts {
        self.hosts.clone()
    }

    /// The current time, in the unit every timestamp here uses.
    pub fn now(&self) -> u64 {
        self.clock.now_ms() / 1000
    }

    /// Whether a request's `Host` may be answered at all.
    pub fn host_allowed(&self, host: Option<&str>) -> bool {
        self.hosts.allows(host)
    }

    /// The decision: allow the request through one door, or refuse it.
    pub fn decide(&self, request: &GateRequest<'_>) -> Result<Door, GateError> {
        if !self.hosts.allows(request.host) {
            return Err(GateError::WrongHost {
                host: request.host.map(str::to_string),
            });
        }

        // Door A: the loopback token. A token is compared whole, in constant
        // time, and an absent one is not "the empty token".
        let door = match (&self.token, request.bearer) {
            (Some(expected), Some(presented))
                if crate::secret_eq(expected.as_bytes(), presented.as_bytes()) =>
            {
                Door::Token
            }
            _ => match request.session_id() {
                Some(id) => match self.sessions.find(id) {
                    Ok(Some(record)) => Door::Session(record),
                    Ok(None) => return Err(self.anonymous(request)),
                    Err(err) => return Err(GateError::Failed(err.to_string())),
                },
                None => return Err(self.anonymous(request)),
            },
        };

        // An `Origin` that is present is checked on a read too: a page from
        // another site asking this service anything is refused, which is what
        // the service did before there was a login (M7d §2.3, "a cross-site API
        // read is still refused"). A write additionally *needs* one through a
        // session door, because only a browser carries a session.
        let origin_ok = match request.origin {
            Some(_) => origin_allowed(request.host, request.origin, door.is_session()),
            None => !(request.is_write() && door.is_session()),
        };
        if !origin_ok {
            return Err(GateError::WrongOrigin {
                origin: request.origin.map(str::to_string),
            });
        }

        if request.is_write() {
            // A socket upgrade cannot carry a custom header — a browser sets the
            // handshake's headers itself — so the `Origin` check above is its
            // rule, exactly as the researched design has it (M7d §2.3 step 6,
            // threat row 12).
            if !request.upgrade && !request.write_header {
                return Err(GateError::MissingHeader);
            }
            if let Door::Session(record) = &door {
                self.limits
                    .write(&record.id_hash)
                    .map_err(|retry| GateError::TooManyRequests {
                        after_ms: retry.after_ms,
                    })?;
            }
        }

        // A session's last use is noted at most once a minute, so a page that
        // polls does not write a file a request. A session that went away
        // between the lookup and the note — expired, or revoked by the CLI — is
        // refused, and so is one the file would not rewrite: this is the last
        // place a request could be let through on a stale answer.
        if let Door::Session(record) = &door {
            return match self.sessions.touch(&record.id_hash) {
                Ok(Some(touched)) => Ok(Door::Session(touched)),
                Ok(None) => Err(self.anonymous(request)),
                Err(err) => Err(GateError::Failed(err.to_string())),
            };
        }
        Ok(door)
    }

    /// Refuses a destructive action the session is not fresh enough for.
    ///
    /// The token door is exempt: it is local and physical, and the researched
    /// design says so (`M7d` §4.4 rule 6).
    pub fn require_fresh(&self, door: &Door) -> Result<(), GateError> {
        match door {
            Door::Token => Ok(()),
            Door::Session(record) if record.fresh(self.now()) => Ok(()),
            Door::Session(_) => Err(GateError::Reauth),
        }
    }

    /// Counts one more write against this door's session.
    ///
    /// [`Gateway::decide`] already counts a write it lets through; this is for a
    /// caller that does more than one write's worth of work in one request and
    /// wants it counted.
    pub fn count_write(&self, door: &Door) -> Result<(), GateError> {
        match door {
            Door::Token => Ok(()),
            Door::Session(record) => {
                self.limits
                    .write(&record.id_hash)
                    .map_err(|retry| GateError::TooManyRequests {
                        after_ms: retry.after_ms,
                    })
            }
        }
    }

    /// Enters the password: the only way a session is minted.
    ///
    /// `current` is the session the request already has, when it has one: a
    /// successful login on an existing session **refreshes** it instead of
    /// starting another, which is how a destructive action's "enter your
    /// password again" works without signing the other tabs out.
    ///
    /// The check runs on a blocking thread, always — including when no password
    /// is set, where it hashes a dummy and then refuses — so the time to answer
    /// says nothing about whether a password exists.
    pub async fn login(
        &self,
        ip: IpAddr,
        password: &str,
        remember: bool,
        user_agent: Option<&str>,
        current: Option<&str>,
    ) -> Result<LoginOutcome, GateError> {
        let ticket = self
            .limits
            .login(ip)
            .map_err(|retry| GateError::TooManyRequests {
                after_ms: retry.after_ms,
            })?;

        let matched = self
            .password
            .verify(password)
            .await
            .map_err(|err| GateError::Failed(err.to_string()))?;
        if !matched {
            // A wrong password is what "guessing" means, and it is the account
            // row that stops a guess spread over many addresses.
            return match self.limits.failure() {
                Ok(()) => Err(GateError::Unauthorized),
                Err(retry) => Err(GateError::TooManyRequests {
                    after_ms: retry.after_ms,
                }),
            };
        }

        // A right password is not an attempt to guess: hand the attempt back.
        self.limits.login_succeeded(ip, ticket);

        if let Some(current) = current
            && let Some(record) = self
                .sessions
                .find(current)
                .map_err(|err| GateError::Failed(err.to_string()))?
            && let Some(record) = self
                .sessions
                .mark_fresh(&record.id_hash)
                .map_err(|err| GateError::Failed(err.to_string()))?
        {
            return Ok(LoginOutcome {
                id: current.to_string(),
                record,
                refreshed: true,
            });
        }

        self.limits
            .session_creation(ip)
            .map_err(|retry| GateError::TooManyRequests {
                after_ms: retry.after_ms,
            })?;
        let (id, record) = self
            .sessions
            .create(user_agent, remember)
            .map_err(|err| GateError::Failed(err.to_string()))?;
        Ok(LoginOutcome {
            id,
            record,
            refreshed: false,
        })
    }

    /// Ends the session a request's cookie names.
    ///
    /// Answers whether a session was ended; a logout of a session that is
    /// already gone is not a failure.
    pub fn logout(&self, current: Option<&str>) -> Result<bool, RemoteError> {
        match current {
            Some(id) => self.sessions.revoke(id),
            None => Ok(false),
        }
    }

    /// The view of a door for `GET /api/session`.
    pub fn view(&self, door: &Door, password_set: bool) -> crate::dto::SessionDto {
        let now = self.now();
        match door {
            Door::Token => crate::dto::SessionDto {
                ok: true,
                via: crate::dto::ViaDto::Token,
                fresh: true,
                password: password_set,
                session: None,
            },
            Door::Session(record) => crate::dto::SessionDto {
                ok: true,
                via: crate::dto::ViaDto::Session,
                fresh: record.fresh(now),
                password: password_set,
                session: Some(SessionInfoDto::new(record, now)),
            },
        }
    }

    /// Counts a request that carried no valid credential, and refuses it once an
    /// address has had too many.
    fn anonymous(&self, request: &GateRequest<'_>) -> GateError {
        match self.limits.anonymous(request.peer) {
            Ok(()) => GateError::Unauthorized,
            Err(Retry { after_ms }) => GateError::TooManyRequests { after_ms },
        }
    }
}

/// Whether a write's `Origin` is allowed through `door`, for a request
/// addressed to `host`.
///
/// A browser sends `Origin` on every write it makes, so a session door requires
/// one and requires its authority to be the `Host` this request was addressed
/// to: that is the whole CSRF rule, and it is why a tunnel's page may write to
/// the tunnel's host and not to any other. The token door's `Origin`, when it
/// sends one, must be loopback — a browser page is never the token's business.
///
/// A request with no `Origin` at all is a program, not a page: allowed on the
/// token door (a mod's `fetch` may not set one), refused on a session door,
/// because a session is a cookie and only a browser carries it.
fn origin_allowed(host: Option<&str>, origin: Option<&str>, session: bool) -> bool {
    match (host, origin) {
        // No `Origin` at all: a program, which may only be the token door.
        (_, None) => !session,
        (Some(host), Some(origin)) => match hosts::origin_host(Some(origin)) {
            // A session may write from any host the service answers to (the
            // tunnel's); the token door only from loopback.
            Some(authority) => {
                hosts::same_host(authority, host) && (session || hosts::is_loopback(authority))
            }
            None => false,
        },
        (None, Some(_)) => false,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::limits::{ANONYMOUS_PER_MINUTE, WRITES_PER_MINUTE};
    use crate::session::{FRESH_SECS, IDLE_SECS};

    /// A clock a test moves by hand.
    #[derive(Debug, Default)]
    struct FakeClock(AtomicU64);

    impl FakeClock {
        fn at(ms: u64) -> Arc<Self> {
            Arc::new(Self(AtomicU64::new(ms)))
        }

        fn advance(&self, secs: u64) {
            self.0.fetch_add(secs * 1000, Ordering::SeqCst);
        }
    }

    impl Clock for FakeClock {
        fn now_ms(&self) -> u64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    /// The bearer token every fixture uses.
    const TOKEN: &str = "the-loopback-token";

    /// A tunnel hostname the fixture allowed, as M11.2 will.
    const TUNNEL: &str = "abc.trycloudflare.com";

    /// The loopback host a local client uses.
    const LOOPBACK: &str = "127.0.0.1:18080";

    /// The password the fixture's session was minted with.
    const PASSWORD: &str = "a long enough password";

    /// A memory cost a test can afford. Argon2 at 64 MiB is the right cost for a
    /// password and a pointless one for a unit test; the parameters are read back
    /// from the file, so a cheap hash still proves every branch.
    const CHEAP_KIB: u32 = 8;

    const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

    /// One gateway with a token, a tunnel host, and one live session.
    struct Fixture {
        _dir: tempfile::TempDir,
        clock: Arc<FakeClock>,
        gateway: Gateway,
        /// The raw id the session cookie carries.
        session: String,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("a state dir");
            let clock = FakeClock::at(1_700_000_000_000);
            let hosts = AllowedHosts::loopback_only();
            hosts.add(TUNNEL).expect("a tunnel host");
            let gateway = Gateway::new(dir.path(), clock.clone())
                .with_token(TOKEN)
                .with_hosts(hosts);
            let (session, _) = gateway
                .sessions()
                .create(Some(UA), false)
                .expect("a session");
            Self {
                _dir: dir,
                clock,
                gateway,
                session,
            }
        }

        /// A read request carrying this fixture's session cookie.
        fn read(&self) -> GateRequest<'_> {
            GateRequest {
                host: Some(LOOPBACK),
                cookies: Some("__Host-shprd=the-token-nobody-set"),
                ..GateRequest::read(peer())
            }
        }

        /// A request whose cookie names the live session.
        fn with_session<'a>(&self, mut request: GateRequest<'a>) -> GateRequest<'a> {
            request.cookies = Some(Box::leak(
                format!("__Host-shprd={}", self.session).into_boxed_str(),
            ));
            request
        }

        /// A request carrying the bearer token.
        fn with_token<'a>(&self, mut request: GateRequest<'a>) -> GateRequest<'a> {
            request.bearer = Some(TOKEN);
            request
        }

        /// A write with everything a browser sends.
        fn write<'a>(&self, host: &'a str) -> GateRequest<'a> {
            GateRequest {
                host: Some(host),
                method: "POST",
                origin: Some(Box::leak(format!("http://{host}").into_boxed_str())),
                write_header: true,
                ..GateRequest::read(peer())
            }
        }
    }

    fn peer() -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))
    }

    /// The decision for one request, as `Ok(door)` or `Err(error)`.
    fn decide(fixture: &Fixture, request: &GateRequest<'_>) -> Result<Door, GateError> {
        fixture.gateway.decide(request)
    }

    #[test]
    fn an_unknown_host_is_refused_before_any_credential_is_looked_at() {
        let fixture = Fixture::new();
        let mut token = fixture.with_token(fixture.read());
        token.host = Some("evil.example");
        assert_eq!(
            decide(&fixture, &token),
            Err(GateError::WrongHost {
                host: Some("evil.example".to_string())
            })
        );

        let mut session = fixture.with_session(GateRequest::read(peer()));
        session.host = Some("evil.example");
        assert!(matches!(
            decide(&fixture, &session),
            Err(GateError::WrongHost { .. })
        ));

        let mut missing = fixture.read();
        missing.host = None;
        assert!(matches!(
            decide(&fixture, &missing),
            Err(GateError::WrongHost { host: None })
        ));

        // A read from a tunnel host that was added is a read like any other.
        let mut tunnel = fixture.read();
        tunnel.host = Some(TUNNEL);
        assert_eq!(decide(&fixture, &tunnel), Err(GateError::Unauthorized));
    }

    #[test]
    fn the_token_door_allows_reads_and_refuses_nothing_it_used_to_allow() {
        let fixture = Fixture::new();
        let read = fixture.with_token(fixture.read());
        assert_eq!(decide(&fixture, &read), Ok(Door::Token));

        // A write with a loopback Origin and the header is what a mod sends.
        let write = fixture.with_token(fixture.write(LOOPBACK));
        assert_eq!(decide(&fixture, &write), Ok(Door::Token));

        // A mod's fetch may send no Origin; that is still the token door.
        let mut no_origin = fixture.with_token(fixture.write(LOOPBACK));
        no_origin.origin = None;
        assert_eq!(decide(&fixture, &no_origin), Ok(Door::Token));
    }

    #[test]
    fn the_token_door_keeps_its_loopback_origin_rule() {
        let fixture = Fixture::new();
        // Another site's page, with the token: refused.
        let mut other = fixture.with_token(fixture.write(LOOPBACK));
        other.origin = Some("https://evil.example");
        assert_eq!(
            decide(&fixture, &other),
            Err(GateError::WrongOrigin {
                origin: Some("https://evil.example".to_string())
            })
        );

        // The token is local, and a tunnel page is not local: the same write
        // from the tunnel's own origin is refused through this door.
        let mut tunnel = fixture.with_token(fixture.write(TUNNEL));
        tunnel.origin = Some(Box::leak(format!("https://{TUNNEL}").into_boxed_str()));
        assert!(matches!(
            decide(&fixture, &tunnel),
            Err(GateError::WrongOrigin { .. })
        ));

        // An Origin for a different loopback port is not this page either.
        let mut wrong_port = fixture.with_token(fixture.write(LOOPBACK));
        wrong_port.origin = Some("http://127.0.0.1:9999");
        assert!(matches!(
            decide(&fixture, &wrong_port),
            Err(GateError::WrongOrigin { .. })
        ));
    }

    #[test]
    fn a_write_through_either_door_needs_the_custom_header() {
        let fixture = Fixture::new();
        let mut token = fixture.with_token(fixture.write(LOOPBACK));
        token.write_header = false;
        assert_eq!(decide(&fixture, &token), Err(GateError::MissingHeader));

        let mut session = fixture.with_session(fixture.write(LOOPBACK));
        session.write_header = false;
        assert_eq!(decide(&fixture, &session), Err(GateError::MissingHeader));
    }

    #[test]
    fn the_session_door_allows_reads_and_shows_the_session() {
        let fixture = Fixture::new();
        let request = fixture.with_session(fixture.read());
        match decide(&fixture, &request) {
            Ok(Door::Session(record)) => {
                assert!(record.fresh(fixture.gateway.now()));
                assert!(record.label.contains("desktop"));
            }
            other => panic!("expected a session door, got {other:?}"),
        }
    }

    #[test]
    fn a_session_write_needs_an_origin_equal_to_the_request_host() {
        let fixture = Fixture::new();

        let loopback = fixture.with_session(fixture.write(LOOPBACK));
        assert!(matches!(decide(&fixture, &loopback), Ok(Door::Session(_))));

        let tunnel = fixture.with_session(fixture.write(TUNNEL));
        assert!(
            matches!(decide(&fixture, &tunnel), Ok(Door::Session(_))),
            "the tunnel's own page may write to the tunnel's host"
        );

        let mut no_origin = fixture.with_session(fixture.write(LOOPBACK));
        no_origin.origin = None;
        assert_eq!(
            decide(&fixture, &no_origin),
            Err(GateError::WrongOrigin { origin: None }),
            "a session is a cookie, and only a browser carries it"
        );

        let mut other = fixture.with_session(fixture.write(LOOPBACK));
        other.origin = Some("https://evil.example");
        assert!(matches!(
            decide(&fixture, &other),
            Err(GateError::WrongOrigin { .. })
        ));

        // The tunnel's page writing to the loopback host: a different origin.
        let mut crossed = fixture.with_session(fixture.write(LOOPBACK));
        crossed.origin = Some(Box::leak(format!("https://{TUNNEL}").into_boxed_str()));
        assert!(matches!(
            decide(&fixture, &crossed),
            Err(GateError::WrongOrigin { .. })
        ));

        // `Origin: null` — a sandboxed frame — is never the page's own.
        let mut null = fixture.with_session(fixture.write(LOOPBACK));
        null.origin = Some("null");
        assert!(matches!(
            decide(&fixture, &null),
            Err(GateError::WrongOrigin { .. })
        ));
    }

    #[test]
    fn a_socket_upgrade_needs_the_pages_origin_and_no_custom_header() {
        let fixture = Fixture::new();
        let mut upgrade = fixture.with_session(GateRequest {
            method: "GET",
            upgrade: true,
            ..fixture.write(LOOPBACK)
        });
        // A browser cannot set `x-shprd` on a handshake, so the Origin is the
        // whole rule there and the header is not asked for.
        upgrade.write_header = false;
        assert!(matches!(decide(&fixture, &upgrade), Ok(Door::Session(_))));

        // Without an Origin, a session socket is refused: a cross-site page can
        // open one, and the browser sends its own origin when it does.
        let mut no_origin = fixture.with_session(GateRequest {
            method: "GET",
            upgrade: true,
            ..fixture.write(LOOPBACK)
        });
        no_origin.origin = None;
        assert!(matches!(
            decide(&fixture, &no_origin),
            Err(GateError::WrongOrigin { .. })
        ));

        // A foreign origin is refused.
        let mut foreign = fixture.with_session(GateRequest {
            method: "GET",
            upgrade: true,
            ..fixture.write(LOOPBACK)
        });
        foreign.origin = Some("https://evil.example");
        assert!(matches!(
            decide(&fixture, &foreign),
            Err(GateError::WrongOrigin { .. })
        ));

        // The token door still needs a loopback origin when it sends one.
        let mut token = fixture.with_token(fixture.write(TUNNEL));
        token.origin = Some(Box::leak(format!("https://{TUNNEL}").into_boxed_str()));
        assert!(matches!(
            decide(&fixture, &token),
            Err(GateError::WrongOrigin { .. })
        ));

        // A mod's handshake may send no Origin at all, and is then allowed.
        let mut mods = fixture.with_token(GateRequest {
            method: "GET",
            upgrade: true,
            ..fixture.write(LOOPBACK)
        });
        mods.origin = None;
        assert_eq!(decide(&fixture, &mods), Ok(Door::Token));
    }

    #[test]
    fn a_read_carrying_another_sites_origin_is_refused_through_either_door() {
        let fixture = Fixture::new();
        let mut token = fixture.with_token(fixture.read());
        token.origin = Some("https://evil.example");
        assert!(matches!(
            decide(&fixture, &token),
            Err(GateError::WrongOrigin { .. })
        ));

        let mut session = fixture.with_session(fixture.read());
        session.origin = Some("https://evil.example");
        assert!(matches!(
            decide(&fixture, &session),
            Err(GateError::WrongOrigin { .. })
        ));

        // The service's own origin on a read is fine through either door.
        let mut own = fixture.with_session(fixture.read());
        own.origin = Some(Box::leak(format!("http://{LOOPBACK}").into_boxed_str()));
        assert!(matches!(decide(&fixture, &own), Ok(Door::Session(_))));

        let mut own_token = fixture.with_token(fixture.read());
        own_token.origin = Some(Box::leak(format!("http://{LOOPBACK}").into_boxed_str()));
        assert_eq!(
            decide(&fixture, &token),
            Err(GateError::WrongOrigin {
                origin: Some("https://evil.example".to_string())
            })
        );
        assert_eq!(decide(&fixture, &own_token), Ok(Door::Token));
    }

    #[test]
    fn no_credential_is_unauthorized_and_a_wrong_token_is_no_better() {
        let fixture = Fixture::new();
        let none = fixture.read();
        assert_eq!(decide(&fixture, &none), Err(GateError::Unauthorized));

        let mut wrong = fixture.with_token(fixture.read());
        wrong.bearer = Some("not-the-token");
        assert_eq!(decide(&fixture, &wrong), Err(GateError::Unauthorized));

        let mut write = fixture.write(LOOPBACK);
        write.bearer = Some("not-the-token");
        assert_eq!(decide(&fixture, &write), Err(GateError::Unauthorized));
    }

    #[test]
    fn an_empty_token_presented_with_no_token_configured_is_not_a_pass() {
        // A gateway nobody gave a token to has no token door at all.
        let dir = tempfile::tempdir().expect("a state dir");
        let gateway = Gateway::new(dir.path(), FakeClock::at(1_700_000_000_000));
        let mut request = GateRequest::read(peer());
        request.host = Some(LOOPBACK);
        request.bearer = Some("");
        assert_eq!(gateway.decide(&request), Err(GateError::Unauthorized));
    }

    #[test]
    fn an_expired_session_is_unauthorized() {
        let fixture = Fixture::new();
        let request = fixture.with_session(fixture.read());
        assert!(decide(&fixture, &request).is_ok());
        fixture.clock.advance(IDLE_SECS + 1);
        let request = fixture.with_session(fixture.read());
        assert_eq!(decide(&fixture, &request), Err(GateError::Unauthorized));
    }

    #[test]
    fn a_token_door_is_always_fresh_and_a_session_is_fresh_for_five_minutes() {
        let fixture = Fixture::new();
        assert!(fixture.gateway.require_fresh(&Door::Token).is_ok());

        let session = Door::Session(
            fixture
                .gateway
                .sessions()
                .list()
                .expect("a list")
                .into_iter()
                .next()
                .expect("a session"),
        );
        assert!(fixture.gateway.require_fresh(&session).is_ok());
        fixture.clock.advance(FRESH_SECS + 1);
        assert_eq!(
            fixture.gateway.require_fresh(&session),
            Err(GateError::Reauth)
        );
    }

    #[test]
    fn sixty_writes_a_minute_per_session_and_no_limit_on_the_token() {
        let fixture = Fixture::new();
        let request = fixture.with_session(fixture.write(LOOPBACK));
        for _ in 0..WRITES_PER_MINUTE {
            assert!(matches!(decide(&fixture, &request), Ok(Door::Session(_))));
        }
        assert!(matches!(
            decide(&fixture, &request),
            Err(GateError::TooManyRequests { .. })
        ));

        let token = fixture.with_token(fixture.write(LOOPBACK));
        for _ in 0..(WRITES_PER_MINUTE * 2) {
            assert_eq!(decide(&fixture, &token), Ok(Door::Token));
        }
    }

    #[test]
    fn an_address_gets_a_hundred_and_twenty_anonymous_requests_a_minute() {
        let fixture = Fixture::new();
        let request = fixture.read();
        for _ in 0..ANONYMOUS_PER_MINUTE {
            assert_eq!(decide(&fixture, &request), Err(GateError::Unauthorized));
        }
        match decide(&fixture, &request) {
            Err(GateError::TooManyRequests { after_ms }) => assert!(after_ms > 0),
            other => panic!("expected a rate limit, got {other:?}"),
        }
        fixture.clock.advance(60);
        assert_eq!(decide(&fixture, &request), Err(GateError::Unauthorized));
    }

    #[test]
    fn a_login_mints_a_session_and_a_second_login_refreshes_it() {
        let fixture = Fixture::new();
        fixture
            .gateway
            .password()
            .set_with_cost(PASSWORD, CHEAP_KIB, 1, 1)
            .expect("the password sets");

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime");
        let before = fixture.gateway.sessions().count().expect("a count");
        let first = runtime
            .block_on(
                fixture
                    .gateway
                    .login(peer(), PASSWORD, false, Some(UA), None),
            )
            .expect("the login");
        assert!(!first.refreshed);
        assert_eq!(
            fixture.gateway.sessions().count().expect("a count"),
            before + 1
        );

        let read = GateRequest {
            host: Some(LOOPBACK),
            cookies: Some(Box::leak(
                format!("__Host-shprd={}", first.id).into_boxed_str(),
            )),
            ..GateRequest::read(peer())
        };
        assert!(matches!(decide(&fixture, &read), Ok(Door::Session(_))));

        fixture.clock.advance(FRESH_SECS + 1);
        let again = runtime
            .block_on(
                fixture
                    .gateway
                    .login(peer(), PASSWORD, true, Some(UA), Some(&first.id)),
            )
            .expect("the second login");
        assert!(again.refreshed, "an existing session is refreshed");
        assert_eq!(again.id, first.id);
        assert!(
            again.record.fresh(fixture.gateway.now()),
            "and is fresh again"
        );
        assert_eq!(
            fixture.gateway.sessions().count().expect("a count"),
            before + 1,
            "no second session was made"
        );
    }

    #[test]
    fn a_wrong_password_and_no_password_at_all_are_the_same_refusal() {
        let fixture = Fixture::new();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime");

        // No password set yet: nothing is accepted, however plausible.
        assert_eq!(
            runtime
                .block_on(fixture.gateway.login(peer(), PASSWORD, false, None, None))
                .unwrap_err(),
            GateError::Unauthorized
        );

        fixture
            .gateway
            .password()
            .set_with_cost(PASSWORD, CHEAP_KIB, 1, 1)
            .expect("the password sets");
        assert_eq!(
            runtime
                .block_on(
                    fixture
                        .gateway
                        .login(peer(), "a different long one", false, None, None)
                )
                .unwrap_err(),
            GateError::Unauthorized
        );
    }

    #[test]
    fn five_logins_an_hour_per_address_after_the_first_sessions() {
        let fixture = Fixture::new();
        fixture
            .gateway
            .password()
            .set_with_cost(PASSWORD, CHEAP_KIB, 1, 1)
            .expect("the password sets");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime");
        let mut minted = 0;
        for _ in 0..5 {
            match runtime.block_on(fixture.gateway.login(peer(), PASSWORD, false, None, None)) {
                Ok(_) => minted += 1,
                Err(GateError::TooManyRequests { .. }) => break,
                Err(other) => panic!("unexpected refusal: {other:?}"),
            }
        }
        assert_eq!(
            minted, 5,
            "five sessions an hour, counted from the fixture's own"
        );
        match runtime
            .block_on(fixture.gateway.login(peer(), PASSWORD, false, None, None))
            .unwrap_err()
        {
            // Either row can be the one that refuses it: five attempts from one
            // address in a quarter of an hour, or five sessions in an hour.
            GateError::TooManyRequests { after_ms } => assert!(after_ms > 0),
            other => panic!("expected a rate limit, got {other:?}"),
        }
    }

    #[test]
    fn a_session_the_cli_revoked_is_refused_on_the_next_request() {
        // The person runs `shprd web sessions --revoke` in another process: the
        // service must stop trusting the cookie at once, not at its next start.
        let fixture = Fixture::new();
        let request = fixture.with_session(fixture.read());
        assert!(matches!(decide(&fixture, &request), Ok(Door::Session(_))));

        let elsewhere = Sessions::new(
            fixture
                .gateway
                .sessions()
                .path()
                .parent()
                .expect("the state dir"),
            fixture.clock.clone(),
        );
        assert_eq!(elsewhere.revoke_all().expect("a revoke"), 1);

        let request = fixture.with_session(fixture.read());
        assert_eq!(decide(&fixture, &request), Err(GateError::Unauthorized));
    }

    #[test]
    fn a_logout_ends_the_session_the_cookie_names() {
        let fixture = Fixture::new();
        assert!(
            fixture
                .gateway
                .logout(Some(&fixture.session))
                .expect("a logout")
        );
        assert_eq!(
            decide(&fixture, &fixture.with_session(fixture.read())),
            Err(GateError::Unauthorized)
        );
        assert!(
            !fixture
                .gateway
                .logout(Some(&fixture.session))
                .expect("a logout"),
            "logging out twice is not an error, and revokes nothing"
        );
    }

    #[test]
    fn the_session_view_says_which_door_and_how_fresh() {
        let fixture = Fixture::new();
        let view = fixture.gateway.view(&Door::Token, true);
        assert_eq!(view.via, crate::dto::ViaDto::Token);
        assert!(view.session.is_none());
        assert!(view.password);

        let record = fixture
            .gateway
            .sessions()
            .find(&fixture.session)
            .expect("the lookup")
            .expect("a session");
        let view = fixture.gateway.view(&Door::Session(record), true);
        assert_eq!(view.via, crate::dto::ViaDto::Session);
        let session = view.session.expect("a session");
        assert_eq!(session.id.len(), 12);
        assert!(session.fresh);
        assert!(session.expires_at > session.created);
    }
}
