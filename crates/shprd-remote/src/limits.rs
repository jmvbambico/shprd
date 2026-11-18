//! The rate limits in front of the login and the API.
//!
//! The table is the researched design's (M7d §4.4), and every row is enforced
//! here rather than in the browser:
//!
//! | Limit | Value | On breach |
//! |---|---|---|
//! | login attempts per IP | 5 / 15 min, then 15 min → 1 h → 6 h | `429` + `Retry-After` |
//! | login attempts globally | 60 / hour | `429` |
//! | wrong passwords for the account | 30 / hour, then locked for 15 min | `429` |
//! | session creation | 5 / hour per IP | `429` ("too many devices") |
//! | `/api/*` writes | 60 / min per session | `429` |
//! | unauthenticated requests | 120 / min per IP | `429` |
//!
//! The keys are the **client address** the gate worked out
//! (`tunnel::client_address`): the socket's peer for a direct request, and over
//! the active tunnel the address the provider's edge wrote in its own header,
//! because every tunnelled request's peer is the local agent and would put every
//! visitor in one bucket. The account is a single one, so its key is the table
//! itself.
//!
//! The per-address and global rows count *failed* logins: an attempt is counted
//! when it arrives (so a burst of parallel guesses cannot all slip in before
//! the first is judged) and handed back by [`RateLimiter::login_succeeded`] when
//! the password was right. A right password never counts against anyone. The account row counts **wrong
//! passwords**, which is what "someone is guessing" means.
//!
//! **The trade-off.** Any limit shared by every visitor is a lever an attacker
//! can pull to lock the owner out, and a limit that is not shared is one a
//! spoofed address walks around. The per-address row is where one guesser is
//! stopped, so it is the tight one (5 per 15 minutes), and on a tunnel whose
//! edge reports addresses it is keyed on the visitor, not the owner. The two
//! shared rows are the ceiling for a *spread* guess and are loose on purpose:
//! one address cannot reach either (it is blocked after 5, and at most about 10
//! an hour once its blocks escalate, against 30 for the account), so it takes
//! a crowd to lock the owner out, and the lock lasts 15 minutes, not an hour.
//! A tunnel whose edge reports nothing we believe (`tunnl`) leaves every
//! visitor in one bucket: there, one guesser does lock the owner out until the
//! block ends, and the local CLI is the way back in.
//!
//! A blocked IP is a *failing closed* state that only time ends; the account
//! lock is what stops a distributed guess, and the local CLI is untouched by
//! both — it is the person's way back in.
//!
//! The clock is injected, so every row is tested by moving time rather than by
//! waiting for it.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use shprd_core::log::Clock;

/// The most a single address may try the password in one window.
pub const LOGIN_PER_IP: u32 = 5;

/// The window that limit is counted over.
pub const LOGIN_WINDOW: Duration = Duration::from_secs(15 * 60);

/// How long each escalation of a blocked address lasts.
///
/// The first block is the window's own length; a second offence in the same hour
/// is an hour, and after that every block is six hours.
pub const BACKOFF: [Duration; 3] = [
    Duration::from_secs(15 * 60),
    Duration::from_secs(60 * 60),
    Duration::from_secs(6 * 60 * 60),
];

/// Every address together may try the password this often in an hour.
pub const LOGIN_GLOBAL: u32 = 60;

/// The account may be given this many wrong passwords in an hour before it
/// locks.
pub const LOGIN_ACCOUNT: u32 = 30;

/// How long the account stays locked.
pub const ACCOUNT_LOCK: Duration = Duration::from_secs(15 * 60);

/// How many sessions one address may mint in an hour.
pub const SESSION_CREATION: u32 = 5;

/// How many writes one session may make a minute.
pub const WRITES_PER_MINUTE: u32 = 60;

/// How many unauthenticated requests one address may make a minute.
pub const ANONYMOUS_PER_MINUTE: u32 = 120;

/// How long one of those two windows is.
const MINUTE: Duration = Duration::from_secs(60);

/// How long a "n an hour" window is.
const HOUR: Duration = Duration::from_secs(60 * 60);

/// When a refused request may be retried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retry {
    /// How long the caller must wait, in milliseconds.
    pub after_ms: u64,
}

impl Retry {
    /// The wait in whole seconds, rounded up: what `Retry-After` carries.
    pub fn after_secs(&self) -> u64 {
        self.after_ms.div_ceil(1000).max(1)
    }
}

/// One escalating block on an address.
#[derive(Debug, Clone, Copy)]
struct Block {
    /// When it ends, in epoch milliseconds.
    until: u64,
    /// How many times this address has been blocked, which picks the length.
    level: usize,
}

/// Everything the limiter counts.
#[derive(Debug, Default)]
struct Buckets {
    /// Login attempts per address, in the last window.
    login_ip: HashMap<IpAddr, Vec<u64>>,
    /// Blocks, by the address that earned them.
    blocked: HashMap<IpAddr, Block>,
    /// Login attempts from everywhere.
    login_all: Vec<u64>,
    /// Wrong passwords for the one account, in the last hour.
    failures: Vec<u64>,
    /// Until when the account is locked.
    account_locked_until: u64,
    /// Session creations per address.
    sessions: HashMap<IpAddr, Vec<u64>>,
    /// Writes per session, keyed by the stored session hash.
    writes: HashMap<String, Vec<u64>>,
    /// Requests that carried no valid credential, per address.
    anonymous: HashMap<IpAddr, Vec<u64>>,
}

/// A count of how often the sign-in limits that mean "someone is guessing" have
/// tripped: the global attempt limit and the account lock.
///
/// A tunnel's supervisor watches it and stops the tunnel when it moves (M7d
/// §4.3 rule 4, the auth-failure circuit breaker). It is its own shared handle,
/// not a view of the limiter, because a gateway is rebuilt when a test swaps its
/// clock and a tunnel started before that must still see the same counter.
#[derive(Debug, Clone, Default)]
pub struct StormSignal(Arc<AtomicU64>);

impl StormSignal {
    /// A signal that has not tripped.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one trip.
    pub fn trip(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }

    /// How many times it has tripped since the service started.
    pub fn count(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// The attempt a [`RateLimiter::login`] counted, handed back to
/// [`RateLimiter::login_succeeded`] when the password turns out to be right.
#[derive(Debug, Clone, Copy)]
pub struct LoginTicket(u64);

/// The limiter: one instance per service, shared by every route.
#[derive(Clone)]
pub struct RateLimiter {
    clock: Arc<dyn Clock + Send + Sync>,
    inner: Arc<Mutex<Buckets>>,
    storm: StormSignal,
}

impl std::fmt::Debug for RateLimiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RateLimiter")
    }
}

impl RateLimiter {
    /// A limiter that counts against `clock`.
    pub fn new(clock: Arc<dyn Clock + Send + Sync>) -> Self {
        Self {
            clock,
            inner: Arc::new(Mutex::new(Buckets::default())),
            storm: StormSignal::new(),
        }
    }

    /// Reports the limits that mean "someone is guessing" to `storm`.
    pub fn with_storm(mut self, storm: StormSignal) -> Self {
        self.storm = storm;
        self
    }

    /// The signal those limits report to.
    pub fn storm(&self) -> StormSignal {
        self.storm.clone()
    }

    /// Counts one login attempt from `ip`, or refuses it.
    ///
    /// The attempt is counted up front so parallel guesses are bounded, and
    /// [`RateLimiter::login_succeeded`] takes it back when the password was
    /// right. A wrong password is counted a second time, against the account,
    /// by [`RateLimiter::failure`].
    pub fn login(&self, ip: IpAddr) -> Result<LoginTicket, Retry> {
        let now = self.now();
        let mut buckets = self.lock();

        // An address in a block is refused until the block ends. The block is
        // not extended by attempts inside it, or a client that retries would
        // never get back in.
        if let Some(block) = buckets.blocked.get(&ip).copied()
            && now < block.until
        {
            return Err(Retry {
                after_ms: block.until - now,
            });
        }

        if now < buckets.account_locked_until {
            return Err(Retry {
                after_ms: buckets.account_locked_until - now,
            });
        }

        // Per address, in its window.
        let window = LOGIN_WINDOW.as_millis() as u64;
        let recent = buckets.login_ip.entry(ip).or_default();
        recent.retain(|at| now.saturating_sub(*at) < window);
        if recent.len() as u32 >= LOGIN_PER_IP {
            let previous = buckets
                .blocked
                .get(&ip)
                .map(|block| block.level)
                .unwrap_or(0);
            let level = previous + 1;
            let length = BACKOFF[level.saturating_sub(1).min(BACKOFF.len() - 1)];
            let until = now + length.as_millis() as u64;
            buckets.blocked.insert(ip, Block { until, level });
            return Err(Retry {
                after_ms: length.as_millis() as u64,
            });
        }
        buckets.login_ip.entry(ip).or_default().push(now);

        // Everywhere, in the last hour.
        let hour = HOUR.as_millis() as u64;
        buckets
            .login_all
            .retain(|at| now.saturating_sub(*at) < hour);
        if buckets.login_all.len() as u32 >= LOGIN_GLOBAL {
            // A refused attempt is not one this address made against the
            // per-address limit.
            if let Some(recent) = buckets.login_ip.get_mut(&ip) {
                recent.pop();
            }
            self.storm.trip();
            let after = buckets
                .login_all
                .first()
                .map(|at| at + hour - now)
                .unwrap_or(hour);
            return Err(Retry { after_ms: after });
        }
        buckets.login_all.push(now);

        self.sweep(&mut buckets, now);
        Ok(LoginTicket(now))
    }

    /// Takes back the attempt [`RateLimiter::login`] counted for `ip` (the one
    /// `ticket` names), because the password was right. Only this attempt's own entry leaves each count, so a
    /// right password racing a wrong one never hands the wrong one back.
    pub fn login_succeeded(&self, ip: IpAddr, ticket: LoginTicket) {
        let mut buckets = self.lock();
        // Only this attempt's own entries leave, not the address's other
        // in-flight attempts: a right password racing wrong ones must not wipe
        // them. (Earlier failures therefore age out with the window.)
        if let Some(recent) = buckets.login_ip.get_mut(&ip)
            && let Some(at) = recent.iter().rposition(|at| *at == ticket.0)
        {
            recent.remove(at);
        }
        if let Some(at) = buckets.login_all.iter().rposition(|at| *at == ticket.0) {
            buckets.login_all.remove(at);
        }
    }

    /// Counts one wrong password, and locks the account when there are too many.
    ///
    /// The lock is the row that stops a distributed guess: a hundred addresses
    /// that each stay under their own limit still show up here. The local CLI
    /// never asks this limiter anything, so it is how the person gets back in.
    pub fn failure(&self) -> Result<(), Retry> {
        let now = self.now();
        let mut buckets = self.lock();
        let hour = HOUR.as_millis() as u64;
        buckets.failures.retain(|at| now.saturating_sub(*at) < hour);
        buckets.failures.push(now);
        if buckets.failures.len() as u32 >= LOGIN_ACCOUNT {
            buckets.account_locked_until = now + ACCOUNT_LOCK.as_millis() as u64;
            self.storm.trip();
            return Err(Retry {
                after_ms: ACCOUNT_LOCK.as_millis() as u64,
            });
        }
        Ok(())
    }

    /// Counts one new session from `ip`, or refuses it ("too many devices").
    pub fn session_creation(&self, ip: IpAddr) -> Result<(), Retry> {
        let now = self.now();
        let mut buckets = self.lock();
        let window = HOUR.as_millis() as u64;
        let recent = buckets.sessions.entry(ip).or_default();
        recent.retain(|at| now.saturating_sub(*at) < window);
        if recent.len() as u32 >= SESSION_CREATION {
            let after = recent.first().map(|at| at + window - now).unwrap_or(window);
            return Err(Retry { after_ms: after });
        }
        recent.push(now);
        Ok(())
    }

    /// Counts one write by `session`, or refuses it.
    pub fn write(&self, session: &str) -> Result<(), Retry> {
        let now = self.now();
        let mut buckets = self.lock();
        take(
            &mut buckets.writes,
            session.to_string(),
            WRITES_PER_MINUTE,
            MINUTE,
            now,
        )
    }

    /// Counts one request that carried no valid credential, or refuses it.
    pub fn anonymous(&self, ip: IpAddr) -> Result<(), Retry> {
        let now = self.now();
        let mut buckets = self.lock();
        take(
            &mut buckets.anonymous,
            ip,
            ANONYMOUS_PER_MINUTE,
            MINUTE,
            now,
        )
    }

    /// Whether `ip` is inside a login block right now.
    pub fn blocked(&self, ip: IpAddr) -> Option<Retry> {
        let now = self.now();
        let buckets = self.lock();
        buckets.blocked.get(&ip).and_then(|block| {
            (now < block.until).then_some(Retry {
                after_ms: block.until - now,
            })
        })
    }

    /// Drops keys that can no longer be refused anything, so a long-lived
    /// service does not remember every address that ever knocked.
    fn sweep(&self, buckets: &mut Buckets, now: u64) {
        const STALE: u64 = 6 * 60 * 60 * 1000;
        if buckets.login_ip.len() > 1024 {
            buckets
                .login_ip
                .retain(|_, ats| ats.last().is_some_and(|at| now.saturating_sub(*at) < STALE));
        }
        buckets
            .blocked
            .retain(|_, block| now.saturating_sub(block.until) < STALE);
        if buckets.sessions.len() > 1024 {
            buckets
                .sessions
                .retain(|_, ats| ats.last().is_some_and(|at| now.saturating_sub(*at) < STALE));
        }
        if buckets.writes.len() > 1024 {
            buckets
                .writes
                .retain(|_, ats| ats.last().is_some_and(|at| now.saturating_sub(*at) < STALE));
        }
        if buckets.anonymous.len() > 1024 {
            buckets
                .anonymous
                .retain(|_, ats| ats.last().is_some_and(|at| now.saturating_sub(*at) < STALE));
        }
        if now >= buckets.account_locked_until {
            buckets.account_locked_until = 0;
        }
    }

    /// The clock, in milliseconds.
    fn now(&self) -> u64 {
        self.clock.now_ms()
    }

    /// The counters, with a poisoned lock recovered rather than panicking.
    fn lock(&self) -> std::sync::MutexGuard<'_, Buckets> {
        match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

/// One sliding window: `limit` hits per `window` for `key`, or a [`Retry`].
///
/// It is a free function because both maps in [`Buckets`] that use it hold a
/// different key type, and a caller with the lock already held is the simplest
/// shape that keeps the count and the record atomic.
fn take<K: std::hash::Hash + Eq>(
    map: &mut HashMap<K, Vec<u64>>,
    key: K,
    limit: u32,
    window: Duration,
    now: u64,
) -> Result<(), Retry> {
    let window = window.as_millis() as u64;
    let recent = map.entry(key).or_default();
    recent.retain(|at| now.saturating_sub(*at) < window);
    if recent.len() as u32 >= limit {
        let after = recent.first().map(|at| at + window - now).unwrap_or(window);
        return Err(Retry { after_ms: after });
    }
    recent.push(now);
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::net::Ipv4Addr;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    /// A clock a test moves by hand.
    #[derive(Debug, Default)]
    struct FakeClock(AtomicU64);

    impl FakeClock {
        fn at(ms: u64) -> Arc<Self> {
            Arc::new(Self(AtomicU64::new(ms)))
        }

        fn advance(&self, by: Duration) {
            self.0.fetch_add(by.as_millis() as u64, Ordering::SeqCst);
        }
    }

    impl Clock for FakeClock {
        fn now_ms(&self) -> u64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    fn ip(last: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(10, 0, 0, last))
    }

    fn limiter() -> (Arc<FakeClock>, RateLimiter) {
        let clock = FakeClock::at(1_700_000_000_000);
        (clock.clone(), RateLimiter::new(clock))
    }

    #[test]
    fn five_login_attempts_a_quarter_of_an_hour_and_then_a_block() {
        let (clock, limits) = limiter();
        for _ in 0..LOGIN_PER_IP {
            limits.login(ip(1)).expect("inside the limit");
        }
        let refused = limits.login(ip(1)).expect_err("the sixth is refused");
        assert_eq!(refused.after_secs(), 15 * 60);
        assert!(limits.blocked(ip(1)).is_some());
        assert!(limits.login(ip(2)).is_ok(), "another address is unaffected");

        // Inside the block, the answer is a retry and the wait counts down; the
        // block is not extended by trying.
        clock.advance(Duration::from_secs(60));
        assert_eq!(
            limits.login(ip(1)).expect_err("still blocked").after_secs(),
            14 * 60
        );

        // Once it ends, the window is empty and attempts work again.
        clock.advance(Duration::from_secs(15 * 60));
        limits.login(ip(1)).expect("the block ended");
    }

    #[test]
    fn the_backoff_escalates_fifteen_minutes_then_an_hour_then_six() {
        let (clock, limits) = limiter();
        let mut waits = Vec::new();
        for _ in 0..3 {
            for _ in 0..LOGIN_PER_IP {
                let _ = limits.login(ip(1));
            }
            let refused = limits.login(ip(1)).expect_err("blocked");
            waits.push(refused.after_secs());
            clock.advance(Duration::from_secs(refused.after_secs()));
        }
        assert_eq!(waits, vec![15 * 60, 60 * 60, 6 * 60 * 60]);
    }

    #[test]
    fn the_window_rolls_so_an_old_attempt_stops_counting() {
        let (clock, limits) = limiter();
        for _ in 0..LOGIN_PER_IP {
            limits.login(ip(1)).expect("inside the limit");
        }
        clock.advance(LOGIN_WINDOW);
        clock.advance(Duration::from_secs(1));
        limits.login(ip(1)).expect("the window rolled over");
    }

    #[test]
    fn every_address_together_may_only_fail_sixty_times_an_hour() {
        let (clock, limits) = limiter();
        for n in 0..LOGIN_GLOBAL {
            // A fresh address for each, so no per-address block can be what
            // refuses the next one.
            limits
                .login(ip(u8::try_from(n).expect("small")))
                .expect("inside");
        }
        let refused = limits
            .login(ip(200))
            .expect_err("one more than the ceiling");
        assert!(refused.after_secs() > 0);
        clock.advance(Duration::from_secs(60 * 60 + 1));
        limits.login(ip(200)).expect("the hour is over");
    }

    #[test]
    fn thirty_wrong_passwords_lock_the_account_for_a_quarter_hour_even_from_many_addresses() {
        let (clock, limits) = limiter();
        for n in 0..(LOGIN_ACCOUNT - 1) {
            limits
                .login(ip(u8::try_from(n).expect("small")))
                .expect("inside");
            limits.failure().expect("a wrong password");
        }
        let locking = limits.failure().expect_err("the last locks the account");
        assert_eq!(locking.after_secs(), 15 * 60);
        // A brand-new address is refused, because the account is what is locked.
        let refused = limits.login(ip(99)).expect_err("the account is locked");
        assert_eq!(refused.after_secs(), 15 * 60);
        clock.advance(Duration::from_secs(10 * 60));
        assert!(limits.login(ip(99)).is_err(), "still locked");
        clock.advance(Duration::from_secs(6 * 60));
        limits.login(ip(99)).expect("the lock ended");
    }

    #[test]
    fn a_successful_login_is_not_a_wrong_password_so_the_account_stays_open() {
        let (clock, limits) = limiter();
        for n in 0..20 {
            // Two addresses, each under its own limit, and no failures.
            let address = ip(n % 2);
            if limits.login(address).is_err() {
                clock.advance(Duration::from_secs(16 * 60));
                limits.login(address).expect("after the block");
            }
        }
        // The account is not locked: nothing was guessed wrong.
        limits.login(ip(50)).expect("a fresh address still gets in");
    }

    #[test]
    fn five_sessions_an_hour_per_address() {
        let (clock, limits) = limiter();
        for _ in 0..SESSION_CREATION {
            limits.session_creation(ip(1)).expect("inside the limit");
        }
        let refused = limits
            .session_creation(ip(1))
            .expect_err("the sixth is refused");
        assert_eq!(refused.after_secs(), 60 * 60);
        limits.session_creation(ip(2)).expect("another address");
        clock.advance(Duration::from_secs(60 * 60));
        limits.session_creation(ip(1)).expect("the window rolled");
    }

    #[test]
    fn sixty_writes_a_minute_per_session() {
        let (clock, limits) = limiter();
        for _ in 0..WRITES_PER_MINUTE {
            limits.write("session-a").expect("inside the limit");
        }
        let refused = limits.write("session-a").expect_err("the sixty-first");
        assert_eq!(refused.after_secs(), 60);
        limits.write("session-b").expect("another session");
        clock.advance(Duration::from_secs(60));
        limits.write("session-a").expect("the minute rolled");
    }

    #[test]
    fn an_unauthenticated_address_gets_a_hundred_and_twenty_a_minute() {
        let (clock, limits) = limiter();
        for _ in 0..ANONYMOUS_PER_MINUTE {
            limits.anonymous(ip(1)).expect("inside the limit");
        }
        let refused = limits.anonymous(ip(1)).expect_err("the next is");
        assert_eq!(refused.after_secs(), 60);
        limits.anonymous(ip(2)).expect("another address");
        clock.advance(Duration::from_secs(60));
        limits.anonymous(ip(1)).expect("the minute rolled");
    }

    #[test]
    fn a_retry_after_is_whole_seconds_rounded_up_and_never_zero() {
        assert_eq!(Retry { after_ms: 1 }.after_secs(), 1);
        assert_eq!(Retry { after_ms: 999 }.after_secs(), 1);
        assert_eq!(Retry { after_ms: 1000 }.after_secs(), 1);
        assert_eq!(Retry { after_ms: 1001 }.after_secs(), 2);
    }

    #[test]
    fn the_counters_do_not_grow_for_ever() {
        let (clock, limits) = limiter();
        for n in 0..1200u32 {
            let third = u8::try_from(n / 250).expect("small");
            let last = u8::try_from(n % 250).expect("small");
            let _ = limits.anonymous(IpAddr::V4(Ipv4Addr::new(10, 1, third, last)));
        }
        clock.advance(Duration::from_secs(7 * 60 * 60));
        limits.login(ip(1)).expect("the sweep runs");
        let buckets = limits.lock();
        assert!(
            buckets.anonymous.is_empty(),
            "addresses that can be refused nothing are forgotten"
        );
    }

    #[test]
    fn the_storm_signal_trips_on_the_global_limit_and_the_account_lock_only() {
        let (clock, limits) = limiter();
        let storm = limits.storm();
        // One address blocked on its own is not a storm.
        for _ in 0..=LOGIN_PER_IP {
            let _ = limits.login(ip(1));
        }
        assert_eq!(storm.count(), 0, "a per-address block is not a storm");

        clock.advance(Duration::from_secs(60 * 60 * 7));
        for n in 0..LOGIN_GLOBAL {
            limits
                .login(ip(u8::try_from(n + 10).expect("small")))
                .expect("inside");
        }
        assert_eq!(storm.count(), 0);
        limits
            .login(ip(250))
            .expect_err("one more than the ceiling");
        assert_eq!(storm.count(), 1, "the global limit tripped it");

        clock.advance(Duration::from_secs(60 * 60 * 7));
        for _ in 0..(LOGIN_ACCOUNT - 1) {
            limits.failure().expect("a wrong password");
        }
        assert_eq!(storm.count(), 1);
        limits.failure().expect_err("the last locks the account");
        assert_eq!(storm.count(), 2, "the account lock tripped it");
    }

    #[test]
    fn a_signal_handed_in_is_the_one_the_limits_report_to() {
        let signal = StormSignal::new();
        let limits = RateLimiter::new(FakeClock::at(1_700_000_000_000)).with_storm(signal.clone());
        for _ in 0..LOGIN_ACCOUNT {
            let _ = limits.failure();
        }
        assert_eq!(signal.count(), 1);
    }

    #[test]
    fn a_right_password_is_not_counted_against_anyone() {
        let (_, limits) = limiter();
        // Far more right logins than any limit, from one address: never refused.
        for _ in 0..(LOGIN_GLOBAL * 2) {
            let ticket = limits.login(ip(1)).expect("a right login is never refused");
            limits.login_succeeded(ip(1), ticket);
        }
        // Four wrong passwords, then the owner gets in: the success took back
        // only its own attempt, so exactly one more is allowed.
        for _ in 0..(LOGIN_PER_IP - 1) {
            limits.login(ip(2)).expect("inside");
        }
        let ticket = limits.login(ip(2)).expect("the fifth is allowed");
        limits.login_succeeded(ip(2), ticket);
        limits.login(ip(2)).expect("the success was not counted");
        limits.login(ip(2)).expect_err("but the four failures were");
    }

    #[test]
    fn a_success_hands_back_only_its_own_attempt_from_the_global_count() {
        let (clock, limits) = limiter();
        let mine = limits.login(ip(1)).expect("inside");
        clock.advance(Duration::from_secs(1));
        limits.login(ip(2)).expect("another guess, still in flight");
        limits.login_succeeded(ip(1), mine);
        assert_eq!(limits.lock().login_all.len(), 1, "the other attempt stays");
    }

    #[test]
    fn one_address_cannot_lock_the_account_or_use_up_the_global_ceiling() {
        let (clock, limits) = limiter();
        // Wrong passwords from one address, as fast as it is allowed, for a day.
        for _ in 0..96 {
            for _ in 0..(LOGIN_PER_IP * 2) {
                if limits.login(ip(1)).is_ok() {
                    let _ = limits.failure();
                }
            }
            clock.advance(Duration::from_secs(15 * 60));
        }
        let locked_until = limits.lock().account_locked_until;
        assert_eq!(locked_until, 0, "one address never locked the account");
        limits
            .login(ip(2))
            .expect("the owner's own address is fine");
    }
}
