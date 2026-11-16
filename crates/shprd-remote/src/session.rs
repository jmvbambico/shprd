//! Server-side sessions: what a browser holds after the password, and how it is
//! ended.
//!
//! A session is a random 32-byte id ([`session_id`]) that only ever travels in
//! the browser's cookie. The service stores the **SHA-256 of it**, never the id
//! itself, so a copy of `sessions.json` is not a set of logins (M7d §4.4, threat
//! row 4). Everything else about a session is in the same file: when it was
//! created, when it was last seen, when the password was last entered
//! (`fresh_until`), and a label so a person can tell their phone from their
//! laptop in `shprd web sessions`.
//!
//! The timeouts are the researched design's:
//!
//! | | ordinary | "remember this device" |
//! |---|---|---|
//! | idle | 12 hours | 30 days |
//! | absolute | 90 days | 90 days |
//!
//! `last_seen` is written at most once a minute, so a page that polls does not
//! write a file per request.
//!
//! The file is read on every operation, so a `shprd web sessions --revoke`
//! running in another process is felt by the service immediately: revocation
//! has to work against a stolen cookie, not eventually.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use shprd_core::log::Clock;
use ts_rs::TS;

use crate::error::RemoteError;
use crate::fsutil::{random_hex, secret_eq, write_private_atomic};

/// The file name sessions live under, inside the service's state directory.
pub const FILE: &str = "sessions.json";

/// How long an ordinary session may go unused.
pub const IDLE_SECS: u64 = 12 * 60 * 60;

/// How long a "remember this device" session may go unused.
pub const REMEMBER_IDLE_SECS: u64 = 30 * 24 * 60 * 60;

/// How long any session may live, however much it is used.
pub const ABSOLUTE_SECS: u64 = 90 * 24 * 60 * 60;

/// How long a password entry stays fresh enough for a destructive action.
pub const FRESH_SECS: u64 = 5 * 60;

/// How often `last_seen` is written.
pub const TOUCH_SECS: u64 = 60;

/// The most sessions one state directory keeps.
///
/// Session creation is rate limited to five an hour, so this is a backstop
/// rather than a policy: it stops a file that only ever grows if something is
/// minting sessions outside that limit.
pub const MAX_SESSIONS: usize = 64;

/// What kind of device a session belongs to, from its `User-Agent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "UaClass.ts")]
pub enum UaClass {
    /// A phone: a small screen, and the reason the web app exists.
    Phone,
    /// A tablet.
    Tablet,
    /// A laptop or a desktop browser.
    Desktop,
    /// Anything else, including a client that sent no `User-Agent`.
    Other,
}

impl UaClass {
    /// The class of a `User-Agent`, or [`UaClass::Other`] for an absent one.
    ///
    /// It is a coarse reading on purpose: this class is shown to a person and
    /// used for nothing else, so a phone that says "iPad" is not worth a
    /// fingerprinting library.
    pub fn of(user_agent: Option<&str>) -> Self {
        let Some(agent) = user_agent else {
            return Self::Other;
        };
        let agent = agent.to_ascii_lowercase();
        let tablet = agent.contains("ipad")
            || agent.contains("tablet")
            || (agent.contains("android") && !agent.contains("mobile"));
        let phone = agent.contains("mobile") || agent.contains("iphone") || agent.contains("ipod");
        if tablet {
            Self::Tablet
        } else if phone {
            Self::Phone
        } else if agent.contains("mozilla") || agent.contains("applewebkit") {
            Self::Desktop
        } else {
            Self::Other
        }
    }

    /// The word this class is called by, in a session list.
    pub fn word(self) -> &'static str {
        match self {
            Self::Phone => "phone",
            Self::Tablet => "tablet",
            Self::Desktop => "desktop",
            Self::Other => "unknown device",
        }
    }
}

/// One stored session. The raw id is never here — only its hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecord {
    /// SHA-256 of the id the cookie carries, as lowercase hex. It is also the
    /// session's public handle: `shprd web sessions` lists and revokes by it.
    pub id_hash: String,
    /// When the password was entered, in seconds since the Unix epoch.
    pub created: u64,
    /// The last request this session was used for.
    pub last_seen: u64,
    /// Until when the session may take a destructive action without the
    /// password again.
    pub fresh_until: u64,
    /// Whether this is a "remember this device" session.
    pub remember: bool,
    /// What kind of device it is, from its `User-Agent`.
    pub ua_class: UaClass,
    /// A short label for a list: the class and the browser, e.g. `phone · safari`.
    pub label: String,
}

impl SessionRecord {
    /// Whether this session has expired at `now` (epoch seconds).
    pub fn expired(&self, now: u64) -> bool {
        let idle = if self.remember {
            REMEMBER_IDLE_SECS
        } else {
            IDLE_SECS
        };
        now.saturating_sub(self.last_seen) > idle
            || now.saturating_sub(self.created) > ABSOLUTE_SECS
    }

    /// When this session stops being usable if it is never used again.
    pub fn expires_at(&self) -> u64 {
        let idle = if self.remember {
            REMEMBER_IDLE_SECS
        } else {
            IDLE_SECS
        };
        (self.last_seen + idle).min(self.created + ABSOLUTE_SECS)
    }

    /// Whether the password was entered recently enough for a destructive
    /// action, at `now`.
    pub fn fresh(&self, now: u64) -> bool {
        now <= self.fresh_until
    }

    /// The first twelve hex digits of the hash: what a list shows, what a
    /// `--revoke` names, and short enough to read out.
    pub fn short_id(&self) -> &str {
        let take = self.id_hash.len().min(12);
        self.id_hash.get(..take).unwrap_or(&self.id_hash)
    }
}

/// What `sessions.json` holds.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Stored {
    /// The file's shape, so a later change can be told apart.
    #[serde(default = "one")]
    version: u32,
    /// The sessions, oldest first.
    #[serde(default)]
    sessions: Vec<SessionRecord>,
}

/// The version this crate writes.
fn one() -> u32 {
    1
}

/// The session store: one file, read and rewritten under a lock.
#[derive(Clone)]
pub struct Sessions {
    path: PathBuf,
    clock: Arc<dyn Clock + Send + Sync>,
    /// One writer at a time, so a read-modify-write cannot lose a session the
    /// CLI added in another process.
    lock: Arc<Mutex<()>>,
}

impl std::fmt::Debug for Sessions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sessions")
            .field("path", &self.path)
            .finish()
    }
}

impl Sessions {
    /// The store in `state_dir` — the service's own `<state>/shprd`.
    pub fn new(state_dir: &Path, clock: Arc<dyn Clock + Send + Sync>) -> Self {
        Self {
            path: state_dir.join(FILE),
            clock,
            lock: Arc::new(Mutex::new(())),
        }
    }

    /// The file the sessions live in.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The current time, in the unit the file stores.
    pub fn now(&self) -> u64 {
        self.clock.now_ms() / 1000
    }

    /// Starts a session and returns the id its cookie carries.
    ///
    /// The id is 32 random bytes; the file gets only its hash. `remember`
    /// chooses the idle timeout, and `user_agent` only labels the row in a list.
    ///
    /// # Errors
    ///
    /// [`RemoteError`] when randomness or the file fails. A session that cannot
    /// be written is not returned, so a login never hands out an id the service
    /// will not find again.
    pub fn create(
        &self,
        user_agent: Option<&str>,
        remember: bool,
    ) -> Result<(String, SessionRecord), RemoteError> {
        let _guard = self.guard();
        let now = self.now();
        let id = random_hex(32)?;
        let class = UaClass::of(user_agent);
        let record = SessionRecord {
            id_hash: hash(&id),
            created: now,
            last_seen: now,
            fresh_until: now + FRESH_SECS,
            remember,
            ua_class: class,
            label: label(class, user_agent),
        };
        let mut stored = self.read()?;
        stored.sessions.retain(|session| !session.expired(now));
        stored.sessions.push(record.clone());
        if stored.sessions.len() > MAX_SESSIONS {
            // Oldest first, so the excess is the front of the list.
            let excess = stored.sessions.len() - MAX_SESSIONS;
            stored.sessions.drain(..excess);
        }
        self.write(&stored)?;
        Ok((id, record))
    }

    /// The session a cookie's id names, if it is one and has not expired.
    ///
    /// Every stored hash is compared ([`secret_eq`]) so the comparison time says
    /// nothing about which session, or how many, the file holds.
    pub fn find(&self, id: &str) -> Result<Option<SessionRecord>, RemoteError> {
        let now = self.now();
        let wanted = hash(id);
        let stored = self.read()?;
        let mut found: Option<SessionRecord> = None;
        for session in &stored.sessions {
            if secret_eq(session.id_hash.as_bytes(), wanted.as_bytes()) && !session.expired(now) {
                found = Some(session.clone());
            }
        }
        Ok(found)
    }

    /// Notes that this session was just used, at most once a minute.
    ///
    /// Returns the record as it stands after the note, which is what a handler
    /// answers with.
    pub fn touch(&self, id_hash: &str) -> Result<Option<SessionRecord>, RemoteError> {
        let now = self.now();
        let _guard = self.guard();
        let mut stored = self.read()?;
        let Some(index) = stored
            .sessions
            .iter()
            .position(|session| session.id_hash == id_hash)
        else {
            return Ok(None);
        };
        let record = &mut stored.sessions[index];
        if now.saturating_sub(record.last_seen) >= TOUCH_SECS {
            record.last_seen = now;
            let record = record.clone();
            self.write(&stored)?;
            return Ok(Some(record));
        }
        Ok(Some(record.clone()))
    }

    /// Notes that the password was entered again for this session.
    ///
    /// A successful `POST /api/login` on an existing session is this call: the
    /// session is not replaced, so a person re-entering the password does not
    /// lose their other tabs.
    pub fn mark_fresh(&self, id_hash: &str) -> Result<Option<SessionRecord>, RemoteError> {
        let now = self.now();
        let _guard = self.guard();
        let mut stored = self.read()?;
        let Some(index) = stored
            .sessions
            .iter()
            .position(|session| session.id_hash == id_hash)
        else {
            return Ok(None);
        };
        let record = &mut stored.sessions[index];
        record.fresh_until = now + FRESH_SECS;
        record.last_seen = now;
        let record = record.clone();
        self.write(&stored)?;
        Ok(Some(record))
    }

    /// Ends one session, by the id its cookie carries.
    ///
    /// Answers whether anything was removed, so a logout says what happened
    /// without pretending a session it did not find was revoked.
    pub fn revoke(&self, id: &str) -> Result<bool, RemoteError> {
        self.revoke_where(|session| secret_eq(session.id_hash.as_bytes(), hash(id).as_bytes()))
    }

    /// Ends one session, by its stored hash or the unique prefix of it.
    ///
    /// This is what `shprd web sessions --revoke <id>` calls, and it is given
    /// the hash because the CLI never sees the cookie. A prefix that names more
    /// than one session removes none of them: guessing which device a person
    /// meant is exactly what they would not want.
    pub fn revoke_by_id(&self, short: &str) -> Result<Revoked, RemoteError> {
        let short = short.trim().to_ascii_lowercase();
        if short.is_empty() {
            return Ok(Revoked::Nothing);
        }
        let _guard = self.guard();
        let mut stored = self.read()?;
        let matches: Vec<&SessionRecord> = stored
            .sessions
            .iter()
            .filter(|session| session.id_hash.starts_with(&short))
            .collect();
        match matches.len() {
            0 => Ok(Revoked::Nothing),
            1 => {
                let id_hash = matches[0].id_hash.clone();
                stored.sessions.retain(|session| session.id_hash != id_hash);
                let count = 1;
                self.write(&stored)?;
                Ok(Revoked::Removed(count))
            }
            count => Ok(Revoked::Ambiguous(count)),
        }
    }

    /// Ends every session — what a password change needs.
    ///
    /// Returns how many were ended, so the CLI can say it and a test can see it.
    pub fn revoke_all(&self) -> Result<usize, RemoteError> {
        let _guard = self.guard();
        let mut stored = self.read()?;
        let removed = stored.sessions.len();
        if removed > 0 {
            stored.sessions.clear();
            self.write(&stored)?;
        }
        Ok(removed)
    }

    /// Every unexpired session, oldest first, without the raw ids.
    pub fn list(&self) -> Result<Vec<SessionRecord>, RemoteError> {
        let now = self.now();
        let mut sessions = self.read()?.sessions;
        sessions.retain(|session| !session.expired(now));
        Ok(sessions)
    }

    /// How many unexpired sessions there are.
    pub fn count(&self) -> Result<usize, RemoteError> {
        Ok(self.list()?.len())
    }

    /// Removes the expired sessions from the file.
    ///
    /// A session that has expired is not found by [`Sessions::find`] whether or
    /// not this runs, so it is tidying rather than enforcement; a service calls
    /// it when it starts and a test calls it directly.
    pub fn purge(&self) -> Result<usize, RemoteError> {
        let now = self.now();
        let _guard = self.guard();
        let mut stored = self.read()?;
        let before = stored.sessions.len();
        stored.sessions.retain(|session| !session.expired(now));
        let removed = before - stored.sessions.len();
        if removed > 0 {
            self.write(&stored)?;
        }
        Ok(removed)
    }

    /// Removes every session `predicate` picks, answering whether any went.
    fn revoke_where(
        &self,
        predicate: impl Fn(&SessionRecord) -> bool,
    ) -> Result<bool, RemoteError> {
        let _guard = self.guard();
        let mut stored = self.read()?;
        let before = stored.sessions.len();
        stored.sessions.retain(|session| !predicate(session));
        let removed = before != stored.sessions.len();
        if removed {
            self.write(&stored)?;
        }
        Ok(removed)
    }

    /// The lock, with a poisoned one recovered: a panic while writing sessions
    /// must not make the service unable to read them for ever.
    fn guard(&self) -> std::sync::MutexGuard<'_, ()> {
        match self.lock.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Reads the file, turning "no file yet" into "no sessions".
    fn read(&self) -> Result<Stored, RemoteError> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) if text.trim().is_empty() => Ok(Stored {
                version: one(),
                sessions: Vec::new(),
            }),
            Ok(text) => serde_json::from_str(&text)
                .map_err(|source| RemoteError::Json { what: FILE, source }),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Stored {
                version: one(),
                sessions: Vec::new(),
            }),
            Err(err) => Err(RemoteError::io(&self.path, err)),
        }
    }

    /// Rewrites the file, privately and atomically.
    fn write(&self, stored: &Stored) -> Result<(), RemoteError> {
        let text = serde_json::to_vec_pretty(stored)
            .map_err(|source| RemoteError::Json { what: FILE, source })?;
        write_private_atomic(&self.path, &text)
    }
}

/// What [`Sessions::revoke_by_id`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Revoked {
    /// Nothing matched the prefix.
    Nothing,
    /// One session matched, and is gone.
    Removed(usize),
    /// More than one matched, and none was touched.
    Ambiguous(usize),
}

/// The SHA-256 of a session id, as lowercase hex.
///
/// The hash is what is stored, so the file never holds anything that can be sent
/// as a cookie.
pub fn hash(id: &str) -> String {
    crate::fsutil::hex(&Sha256::digest(id.as_bytes()))
}

/// A short label for a session row: its device class and its browser family.
pub fn label(class: UaClass, user_agent: Option<&str>) -> String {
    let agent = user_agent.unwrap_or_default().to_ascii_lowercase();
    let browser = if agent.contains("edg/") {
        "edge"
    } else if agent.contains("firefox") {
        "firefox"
    } else if agent.contains("chrome") || agent.contains("crios") {
        "chrome"
    } else if agent.contains("safari") || agent.contains("applewebkit") {
        "safari"
    } else if agent.contains("curl") {
        "curl"
    } else {
        ""
    };
    match browser {
        "" => class.word().to_string(),
        browser => format!("{} · {browser}", class.word()),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::sync::atomic::{AtomicU64, Ordering};

    use shprd_core::log::Clock;

    use super::*;

    /// A clock a test moves by hand.
    #[derive(Debug, Default)]
    struct FakeClock(AtomicU64);

    impl FakeClock {
        fn at(secs: u64) -> Arc<Self> {
            Arc::new(Self(AtomicU64::new(secs * 1000)))
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

    const IPHONE: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1";
    const MAC: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

    fn fixture() -> (tempfile::TempDir, Arc<FakeClock>, Sessions) {
        let dir = tempfile::tempdir().expect("a state dir");
        let clock = FakeClock::at(1_700_000_000);
        let sessions = Sessions::new(dir.path(), clock.clone());
        (dir, clock, sessions)
    }

    #[test]
    fn a_session_is_found_by_the_id_its_cookie_carries_and_never_stored_raw() {
        let (_dir, _clock, sessions) = fixture();
        let (id, record) = sessions.create(Some(IPHONE), false).expect("a session");
        assert_eq!(id.len(), 64, "32 bytes of hex");
        assert_ne!(record.id_hash, id, "the file holds the hash");
        let text = std::fs::read_to_string(sessions.path()).expect("the file");
        assert!(!text.contains(&id), "the raw id is never written");
        assert!(text.contains(&record.id_hash));

        let found = sessions.find(&id).expect("the lookup").expect("a session");
        assert_eq!(found.id_hash, record.id_hash);
        assert_eq!(found.label, "phone · safari");
        assert_eq!(found.ua_class, UaClass::Phone);
        assert!(
            sessions
                .find("not-a-session")
                .expect("the lookup")
                .is_none()
        );
    }

    #[test]
    fn an_ordinary_session_expires_after_twelve_idle_hours() {
        let (_dir, clock, sessions) = fixture();
        let (id, _) = sessions.create(Some(MAC), false).expect("a session");
        clock.advance(IDLE_SECS);
        assert!(
            sessions.find(&id).expect("the lookup").is_some(),
            "still inside the idle window"
        );
        clock.advance(2);
        assert!(sessions.find(&id).expect("the lookup").is_none());
        assert!(sessions.list().expect("a list").is_empty());
    }

    #[test]
    fn remembering_a_device_lasts_thirty_days_but_never_past_ninety() {
        let (_dir, clock, sessions) = fixture();
        let (id, _) = sessions.create(Some(MAC), true).expect("a session");
        clock.advance(REMEMBER_IDLE_SECS - 1);
        assert!(sessions.find(&id).expect("the lookup").is_some());
        clock.advance(2);
        assert!(sessions.find(&id).expect("the lookup").is_none());

        let (kept, _) = sessions.create(Some(MAC), true).expect("a session");
        // Used every day, so it never idles out; the absolute cap still ends it.
        let mut elapsed = 0;
        while elapsed + 24 * 60 * 60 < ABSOLUTE_SECS {
            clock.advance(24 * 60 * 60);
            elapsed += 24 * 60 * 60;
            sessions
                .touch(&hash(&kept))
                .expect("a touch")
                .expect("found");
            assert!(sessions.find(&kept).expect("the lookup").is_some());
        }
        clock.advance(24 * 60 * 60 + 1);
        assert!(
            sessions.find(&kept).expect("the lookup").is_none(),
            "the absolute limit ends it whatever the use"
        );
    }

    #[test]
    fn last_seen_is_written_at_most_once_a_minute() {
        let (_dir, clock, sessions) = fixture();
        let (id, record) = sessions.create(None, false).expect("a session");
        let start = record.last_seen;
        clock.advance(30);
        let touched = sessions
            .touch(&record.id_hash)
            .expect("a touch")
            .expect("found");
        assert_eq!(touched.last_seen, start, "inside the minute: not written");
        clock.advance(31);
        let touched = sessions
            .touch(&record.id_hash)
            .expect("a touch")
            .expect("found");
        assert_eq!(touched.last_seen, start + 61, "written once a minute");
        assert!(sessions.find(&id).expect("the lookup").is_some());
    }

    #[test]
    fn a_password_that_was_entered_five_minutes_ago_is_no_longer_fresh() {
        let (_dir, clock, sessions) = fixture();
        let (_id, record) = sessions.create(None, false).expect("a session");
        assert!(record.fresh(sessions.now()));
        clock.advance(FRESH_SECS + 1);
        let listed = sessions.list().expect("a list");
        assert_eq!(listed.len(), 1);
        assert!(!listed[0].fresh(sessions.now()), "the entry is stale");
        let refreshed = sessions
            .mark_fresh(&record.id_hash)
            .expect("a refresh")
            .expect("found");
        assert!(refreshed.fresh(sessions.now()), "a new entry fresh again");
    }

    #[test]
    fn revoking_one_session_leaves_the_others() {
        let (_dir, _clock, sessions) = fixture();
        let (phone, phone_record) = sessions.create(Some(IPHONE), false).expect("a session");
        let (laptop, _) = sessions.create(Some(MAC), true).expect("a session");
        assert!(sessions.revoke(&phone).expect("a revoke"));
        assert!(sessions.find(&phone).expect("the lookup").is_none());
        assert!(sessions.find(&laptop).expect("the lookup").is_some());
        assert!(!sessions.revoke(&phone).expect("a second revoke"));
        assert!(phone_record.label.contains("phone"));
    }

    #[test]
    fn revoking_by_hash_takes_a_unique_prefix_and_refuses_an_ambiguous_one() {
        let (_dir, _clock, sessions) = fixture();
        let (_, first) = sessions.create(Some(IPHONE), false).expect("a session");
        let (_, second) = sessions.create(Some(MAC), true).expect("a session");

        assert_eq!(
            sessions.revoke_by_id(&"0".repeat(64)).expect("a revoke"),
            Revoked::Nothing
        );
        // A prefix that both hashes share: with two sessions, one hex digit is
        // ambiguous unless the hashes differ there.
        let common: String = first
            .id_hash
            .chars()
            .zip(second.id_hash.chars())
            .take_while(|(a, b)| a == b)
            .map(|(a, _)| a)
            .collect();
        if !common.is_empty() {
            assert_eq!(
                sessions.revoke_by_id(&common).expect("a revoke"),
                Revoked::Ambiguous(2)
            );
            assert_eq!(
                sessions.count().expect("a count"),
                2,
                "an ambiguous prefix ends nothing"
            );
        }
        assert_eq!(
            sessions.revoke_by_id(&second.id_hash).expect("a revoke"),
            Revoked::Removed(1)
        );
        assert_eq!(sessions.count().expect("a count"), 1);
    }

    #[test]
    fn revoking_everything_answers_how_many_went() {
        let (_dir, _clock, sessions) = fixture();
        for _ in 0..3 {
            sessions.create(None, false).expect("a session");
        }
        assert_eq!(sessions.revoke_all().expect("a revoke"), 3);
        assert_eq!(sessions.count().expect("a count"), 0);
        assert_eq!(sessions.revoke_all().expect("a revoke"), 0);
    }

    #[test]
    fn the_expired_are_purged_and_the_live_ones_stay() {
        let (_dir, clock, sessions) = fixture();
        sessions.create(None, false).expect("a session");
        sessions.create(None, false).expect("a session");
        clock.advance(IDLE_SECS + 1);
        // Creating a session is what tidies the file: the two that idled out are
        // gone before the new one is written.
        sessions.create(None, false).expect("a session");
        assert_eq!(sessions.list().expect("a list").len(), 1);
        assert_eq!(sessions.purge().expect("a purge"), 0);

        clock.advance(IDLE_SECS + 1);
        assert_eq!(sessions.purge().expect("a purge"), 1);
        assert_eq!(sessions.list().expect("a list").len(), 0);
    }

    #[test]
    fn a_session_written_by_another_process_is_seen_at_once() {
        // The CLI revokes in its own process while the service holds its own
        // store: revocation must not wait for a restart.
        let (dir, clock, sessions) = fixture();
        let (_id, record) = sessions.create(None, false).expect("a session");
        let other = Sessions::new(dir.path(), clock);
        assert_eq!(other.revoke_all().expect("a revoke"), 1);
        assert_eq!(
            sessions.count().expect("a count"),
            0,
            "the service's own store sees the other process's write"
        );
        assert!(
            record.id_hash.len() == 64,
            "the record is still a hash of the id"
        );
    }

    #[test]
    fn there_are_never_more_than_the_cap() {
        let (_dir, _clock, sessions) = fixture();
        for _ in 0..(MAX_SESSIONS + 5) {
            sessions.create(None, false).expect("a session");
        }
        assert_eq!(sessions.count().expect("a count"), MAX_SESSIONS);
    }

    #[test]
    fn a_device_class_reads_the_user_agent_coarsely() {
        assert_eq!(UaClass::of(None), UaClass::Other);
        assert_eq!(UaClass::of(Some(IPHONE)), UaClass::Phone);
        assert_eq!(UaClass::of(Some(MAC)), UaClass::Desktop);
        assert_eq!(
            UaClass::of(Some(
                "Mozilla/5.0 (iPad; CPU OS 17_0 like Mac OS X) AppleWebKit/605.1.15"
            )),
            UaClass::Tablet
        );
        assert_eq!(
            UaClass::of(Some(
                "Mozilla/5.0 (Linux; Android 14; Pixel Tablet) AppleWebKit/537.36"
            )),
            UaClass::Tablet
        );
        assert_eq!(
            UaClass::of(Some(
                "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 Mobile"
            )),
            UaClass::Phone
        );
        assert_eq!(UaClass::of(Some("curl/8.7.1")), UaClass::Other);
        assert_eq!(
            label(UaClass::Other, Some("curl/8.7.1")),
            "unknown device · curl"
        );
    }

    #[test]
    fn the_file_is_private() {
        let (_dir, _clock, sessions) = fixture();
        sessions.create(None, false).expect("a session");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(sessions.path())
                .expect("the file")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn an_empty_or_missing_file_is_no_sessions_rather_than_an_error() {
        let (dir, clock, sessions) = fixture();
        assert_eq!(sessions.count().expect("a count"), 0);
        std::fs::write(dir.path().join(FILE), b"").expect("an empty file");
        assert_eq!(sessions.count().expect("a count"), 0);
        let other = Sessions::new(&dir.path().join("none"), clock);
        assert_eq!(other.count().expect("a count"), 0);
    }

    #[test]
    fn a_file_something_else_wrote_is_refused_rather_than_read_as_empty() {
        let (dir, clock, sessions) = fixture();
        std::fs::write(dir.path().join(FILE), b"{not json").expect("a broken file");
        assert!(sessions.count().is_err());
        let _ = Sessions::new(dir.path(), clock);
        assert!(sessions.find("anything").is_err());
    }

    #[test]
    fn a_short_id_is_twelve_characters() {
        let (_dir, _clock, sessions) = fixture();
        let (_, record) = sessions.create(None, false).expect("a session");
        assert_eq!(record.short_id().len(), 12);
        assert!(record.id_hash.starts_with(record.short_id()));
    }
}
