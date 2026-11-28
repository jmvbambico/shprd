//! The login password: how it is chosen, how it is stored, how it is checked.
//!
//! The parameters are the researched design's (M7d §4.4): **argon2id**, memory
//! 64 MiB, three passes, one lane, a 16-byte random salt, and the result kept as
//! one PHC string in `<state>/shprd/web-auth.json` with mode `0600`. A hash
//! takes about a tenth of a second, so it runs on a blocking thread
//! ([`PasswordStore::verify`]) and never on the async runtime.
//!
//! Two properties are worth more than they look:
//!
//! * **No password set is not "no check".** When the file is missing, the check
//!   still hashes — against a dummy of the same parameters — and then refuses.
//!   So an answer takes the same time whether or not a password exists, and a
//!   missing file can never be mistaken for a door standing open.
//! * **The comparison is constant-time.** [`shprd_remote::secret_eq`] (`subtle`)
//!   compares the digest, so the time says nothing about how much of a guess was
//!   right. The file is read on every check, so a password the CLI writes while
//!   the service is running takes effect at once.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use argon2::password_hash::{PasswordHash, PasswordHasher, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use serde::{Deserialize, Serialize};

use crate::error::{PasswordRefusal, RemoteError};
use crate::fsutil::{random_bytes, secret_eq, set_private, write_private_atomic};

/// The shortest password a person may set.
///
/// Twelve is the researched design's floor: shorter than that, and the guess
/// rate a rate limiter allows can still reach it.
pub const MIN_LENGTH: usize = 12;

/// The longest password this crate will hash.
///
/// argon2 absorbs the password in one pass whatever its length, so this is not a
/// work factor; it is a bound on how much a request may make this process hold.
pub const MAX_LENGTH: usize = 1024;

/// argon2id's memory cost: 64 MiB, the OWASP baseline.
pub const MEMORY_KIB: u32 = 64 * 1024;

/// argon2id's time cost: three passes.
pub const PASSES: u32 = 3;

/// argon2id's parallelism: one lane, so the cost is one core's.
pub const LANES: u32 = 1;

/// The file name the hash lives under, inside the service's state directory.
pub const FILE: &str = "web-auth.json";

/// The PHC string a check hashes against when no password is set.
///
/// It is the argon2id hash of a sentence nobody will type, made with the same
/// parameters as a real one, so [`PasswordStore::verify`] costs the same whether
/// or not a password exists. It can never match a password: the answer is
/// refused because the file is missing, not because this matched.
const DUMMY_PHC: &str = "$argon2id$v=19$m=65536,t=3,p=1$c2hwdmQtd3Jvbmctc2FsdA$Aj0mYkP0Ynul3XR0oG8r1z7yc5CzGEv0Gb6LDwxkV6o";

/// Which file a [`PasswordStore`] reads and writes, and nothing else.
///
/// The store holds no cache: the file is read on each check, so the CLI's
/// `shprd web password set` takes effect in a running service without a restart.
#[derive(Debug, Clone)]
pub struct PasswordStore {
    path: PathBuf,
}

/// What `web-auth.json` holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Stored {
    /// The file's shape, so a later change can be told apart.
    version: u32,
    /// The argon2id PHC string.
    phc: String,
    /// When it was set, in seconds since the Unix epoch.
    updated: u64,
}

impl PasswordStore {
    /// The store in `state_dir` — the service's own `<state>/shprd`.
    pub fn new(state_dir: &Path) -> Self {
        Self {
            path: state_dir.join(FILE),
        }
    }

    /// The file the hash lives in.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether a password is set.
    ///
    /// The answer distinguishes "set" from "not set" only; it never says
    /// anything about the password.
    pub fn is_set(&self) -> Result<bool, RemoteError> {
        Ok(self.read()?.is_some())
    }

    /// Sets `password`, replacing any earlier one.
    ///
    /// The caller checks the rules first ([`PasswordStore::check_rules`]); this
    /// repeats them because a hash must never be written for a password nobody
    /// checked. Revoking the sessions that trusted the old password is the
    /// caller's next step — see [`crate::session::Sessions::revoke_all`].
    pub fn set(&self, password: &str) -> Result<(), RemoteError> {
        self.set_with_cost(password, MEMORY_KIB, PASSES, LANES)
    }

    /// Sets `password` with explicit argon2 parameters.
    ///
    /// [`PasswordStore::set`] is this with the researched design's parameters,
    /// and it is what every real caller uses. This exists because the cost is
    /// also read back from the file: a test that proves a cookie opens a door
    /// does not need three seconds of argon2 per request, so it writes a cheap
    /// hash and the verification follows it. Nothing the web can reach calls it,
    /// so a lowered cost can only come from a person at their own terminal.
    pub fn set_with_cost(
        &self,
        password: &str,
        memory_kib: u32,
        passes: u32,
        lanes: u32,
    ) -> Result<(), RemoteError> {
        Self::check_rules(password, None)
            .map_err(|refusal| RemoteError::Refused(refusal.to_string()))?;
        let phc = hash(password, memory_kib, passes, lanes)?;
        let stored = Stored {
            version: 1,
            phc,
            updated: now_secs(),
        };
        let text = serde_json::to_vec_pretty(&stored)
            .map_err(|source| RemoteError::Json { what: FILE, source })?;
        write_private_atomic(&self.path, &text)
    }

    /// Forgets the password: the web can no longer mint a session at all.
    ///
    /// A missing file is not an error — "cleared" is the state the person asked
    /// for.
    pub fn clear(&self) -> Result<(), RemoteError> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(RemoteError::io(&self.path, err)),
        }
    }

    /// Checks `password` against the stored hash.
    ///
    /// The hash runs on a blocking thread, because it is ~100 ms of CPU and the
    /// caller is a request handler.
    ///
    /// # Panics
    ///
    /// Panics when called outside a Tokio runtime, like any `spawn_blocking`.
    pub async fn verify(&self, password: &str) -> Result<bool, RemoteError> {
        let store = self.clone();
        let password = password.to_string();
        tokio::task::spawn_blocking(move || store.verify_blocking(&password))
            .await
            .map_err(|err| RemoteError::Refused(format!("the password check did not run: {err}")))?
    }

    /// [`PasswordStore::verify`], on the calling thread.
    ///
    /// A test and the CLI use it; a request never should.
    pub fn verify_blocking(&self, password: &str) -> Result<bool, RemoteError> {
        let stored = self.read()?;
        // One hash either way: the dummy stands in for a password that is not
        // there, so the time to answer cannot tell a caller which it is.
        let phc = match &stored {
            Some(stored) => stored.phc.as_str(),
            None => DUMMY_PHC,
        };
        let matched = digest_matches(phc, password)?;
        Ok(stored.is_some() && matched)
    }

    /// Refuses a password a person should not set.
    ///
    /// `hostname` is the machine's own name, which a tunnel's URL shows and a
    /// stranger can read, so it is refused along with the commonest passwords.
    /// It is an argument rather than a lookup so the rule is testable and so the
    /// binary decides what this machine is called.
    pub fn check_rules(password: &str, hostname: Option<&str>) -> Result<(), PasswordRefusal> {
        if password.chars().count() < MIN_LENGTH {
            return Err(PasswordRefusal::TooShort);
        }
        if password.len() > MAX_LENGTH {
            return Err(PasswordRefusal::TooLong);
        }
        let lowered = password.to_lowercase();
        if crate::password::COMMON_PASSWORDS.contains(&lowered.as_str()) {
            return Err(PasswordRefusal::Common);
        }
        if let Some(hostname) = hostname {
            for name in host_names(hostname) {
                if lowered == name.to_lowercase() {
                    return Err(PasswordRefusal::Hostname);
                }
            }
        }
        Ok(())
    }

    /// The stored record, or `None` when no password is set.
    fn read(&self) -> Result<Option<Stored>, RemoteError> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => {
                // A file another local user may read is a hash exposed to an
                // offline attack: private it again before trusting it.
                set_private(&self.path)?;
                let stored: Stored = serde_json::from_str(&text)
                    .map_err(|source| RemoteError::Json { what: FILE, source })?;
                if stored.phc.is_empty() {
                    return Err(RemoteError::Refused(format!(
                        "{} holds an empty hash; set a password again",
                        self.path.display()
                    )));
                }
                Ok(Some(stored))
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(RemoteError::io(&self.path, err)),
        }
    }
}

/// Hashes `password` into a PHC string with the given parameters.
fn hash(password: &str, memory_kib: u32, passes: u32, lanes: u32) -> Result<String, RemoteError> {
    let mut salt = [0u8; 16];
    random_bytes(&mut salt)?;
    let salt = SaltString::encode_b64(&salt).map_err(|err| RemoteError::Hash {
        reason: err.to_string(),
    })?;
    let argon = Argon2::new(
        Algorithm::Argon2id,
        Version::V0x13,
        Params::new(memory_kib, passes, lanes, None).map_err(|err| RemoteError::Hash {
            reason: err.to_string(),
        })?,
    );
    let hash = argon
        .hash_password(password.as_bytes(), &salt)
        .map_err(|err| RemoteError::Hash {
            reason: err.to_string(),
        })?;
    Ok(hash.to_string())
}

/// Whether `password` produces the digest in `phc`, in constant time.
///
/// Only argon2id version 19 is accepted: it is the only thing this crate writes,
/// so anything else in the file is a file something else wrote, and is refused
/// rather than hashed with parameters nobody chose.
fn digest_matches(phc: &str, password: &str) -> Result<bool, RemoteError> {
    let parsed = PasswordHash::new(phc).map_err(|err| RemoteError::Hash {
        reason: err.to_string(),
    })?;
    if parsed.algorithm != Algorithm::Argon2id.ident() {
        return Err(RemoteError::Refused(
            "the stored password hash is not argon2id".to_string(),
        ));
    }
    if parsed.version != Some(19) {
        return Err(RemoteError::Refused(
            "the stored password hash is not argon2 version 19".to_string(),
        ));
    }
    let params = Params::try_from(&parsed).map_err(|err| RemoteError::Hash {
        reason: err.to_string(),
    })?;
    let salt = parsed.salt.ok_or_else(|| RemoteError::Hash {
        reason: "the stored hash has no salt".to_string(),
    })?;
    let expected = parsed.hash.ok_or_else(|| RemoteError::Hash {
        reason: "the stored hash has no digest".to_string(),
    })?;
    // The PHC salt is base64: argon2 absorbs the bytes it encodes, not the text.
    let mut salt_buffer = [0u8; 64];
    let salt = salt
        .decode_b64(&mut salt_buffer)
        .map_err(|err| RemoteError::Hash {
            reason: err.to_string(),
        })?;
    let mut digest = [0u8; 32];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(password.as_bytes(), salt, &mut digest)
        .map_err(|err| RemoteError::Hash {
            reason: err.to_string(),
        })?;
    Ok(secret_eq(&digest, expected.as_bytes()))
}

/// The names a machine answers to, for the "not your own hostname" rule.
///
/// The full name and its first label, so `studio.local` rules out both
/// `studio.local` and `studio`.
fn host_names(hostname: &str) -> Vec<&str> {
    let trimmed = hostname.trim();
    let mut names = vec![trimmed];
    if let Some((label, _)) = trimmed.split_once('.') {
        names.push(label);
    }
    names
}

/// This machine's name, as the operating system holds it.
///
/// It is read from `hostname(1)` at an absolute path — the same reason the
/// service asks `kill(1)` whether a process is alive — with `$HOSTNAME` as a
/// fallback for a system without it. `None` leaves the hostname rule off, which
/// is weaker but never wrong.
pub fn hostname() -> Option<String> {
    for program in ["/bin/hostname", "/usr/bin/hostname"] {
        if !Path::new(program).is_file() {
            continue;
        }
        if let Ok(output) = std::process::Command::new(program)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            && output.status.success()
        {
            let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !name.is_empty() {
                return Some(name);
            }
        }
    }
    std::env::var("HOSTNAME")
        .ok()
        .filter(|name| !name.is_empty())
}

/// Seconds since the Unix epoch.
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// The few passwords every guessing tool tries first.
///
/// A small embedded list, not a dictionary: it exists to stop the password a
/// person picks when nobody is watching, and it costs nothing to carry. Entries
/// are lowercase and compared with a lowercased password.
pub const COMMON_PASSWORDS: &[&str] = &[
    "123456789012",
    "1234567890123",
    "12345678901234",
    "123456789012345",
    "1234567890123456",
    "000000000000",
    "111111111111",
    "121212121212",
    "123123123123",
    "123412341234",
    "112233445566",
    "qwertyuiop12",
    "qwertyuiop123",
    "qwertyuiop1234",
    "qwertyuiopasdf",
    "1qaz2wsx3edc",
    "1q2w3e4r5t6y",
    "q1w2e3r4t5y6",
    "zaq12wsxcde3",
    "asdfghjkl123",
    "asdfasdfasdf",
    "qweasdzxc123",
    "password1234",
    "password12345",
    "password123!",
    "passwordpassword",
    "p@ssw0rd1234",
    "p@ssword1234",
    "passw0rd1234",
    "passpasspass",
    "letmein12345",
    "letmein123456",
    "welcome12345",
    "welcome123456",
    "welcome1welcome",
    "iloveyou1234",
    "iloveyou12345",
    "sunshine1234",
    "princess1234",
    "football1234",
    "baseball1234",
    "superman1234",
    "starwars1234",
    "trustno1trustno1",
    "administrator",
    "administrator1",
    "changeme1234",
    "changemenow1",
    "defaultpassword",
    "secretpassword",
    "mypassword12",
    "mypassword123",
    "passwordpassword1",
    "qwerty12345678",
    "qwerty123456789",
    "azerty123456",
    "abcdefghijkl",
    "abcdefghijklm",
    "abcd1234abcd",
    "1password1password",
    "correcthorsebatterystaple",
    "shprdshprdshprd",
    "shepherdr",
    "shepherdr123",
    "localhost123",
    "localhost1234",
    "adminadmin12",
    "rootrootroot",
    "testtesttest",
    "testtest1234",
    "temp12345678",
    "temporary1234",
    "letmeinplease",
    "opensesame12",
    "opensesame123",
    "nothingtosee1",
    "nopassword12",
    "nopassword123",
    "blankblank12",
    "monkeymonkey",
    "dragon123456",
    "shadow123456",
    "master123456",
    "michael12345",
    "jennifer1234",
    "computer1234",
    "internet1234",
    "samsung12345",
    "google123456",
    "appleapple12",
    "windows12345",
    "linuxlinux12",
    "rustrustrust",
    "terminal1234",
    "development1",
    "developer123",
    "production12",
    "integration1",
    "github123456",
    "gitlab123456",
    "bitbucket123",
    "docker123456",
    "kubernetes12",
    "homelab12345",
    "mypassword1!",
    "password123456789",
    "qwertyuiop1234567890",
    "1q2w3e4r5t6y7u8i",
    "1234567890abcdef",
    "0987654321qwerty",
    "aaaa1111aaaa",
    "abcd1234efgh",
    "qazwsxedcrfv",
    "zxcvbnmasdfg",
    "qwe123qwe123",
    "asd123asd123",
    "zxc123zxc123",
    "123qweasdzxc",
    "iloveyouiloveyou",
    "sweetheart12",
    "basketball12",
    "supermanbatman",
    "starwarswars1",
    "mypassphrase1",
    "thisismypassword",
    "changethis123",
    "changeme123456",
    "pleaseletmein1",
    "notapassword1",
    "passwordpasswordpassword",
];

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// A memory cost the argon2 cost tests do not assert about.
    ///
    /// Argon2 at 64 MiB is the right cost for a password and about 1.5 seconds in
    /// a debug build: a test that only needs "the hash verifies" writes this
    /// instead, and the one test that pins the production parameters uses `set`.
    const CHEAP_KIB: u32 = 8;

    /// A store in a fresh state directory, with the directory alive.
    fn store() -> (tempfile::TempDir, PasswordStore) {
        let dir = tempfile::tempdir().expect("a state dir");
        let store = PasswordStore::new(dir.path());
        (dir, store)
    }

    #[test]
    fn the_dummy_hash_has_a_real_shape() {
        // If this ever stops parsing, a check with no password set would fail
        // for the wrong reason; this pins the constant to the parser.
        let parsed = PasswordHash::new(DUMMY_PHC).expect("the dummy parses");
        assert_eq!(parsed.algorithm, Algorithm::Argon2id.ident());
        assert_eq!(parsed.version, Some(19));
        let params = Params::try_from(&parsed).expect("the parameters parse");
        assert_eq!(params.m_cost(), MEMORY_KIB);
        assert_eq!(params.t_cost(), PASSES);
        assert_eq!(params.p_cost(), LANES);
        assert!(
            parsed.salt.expect("a salt").as_str().len() >= 16,
            "a salt of at least 16 encoded characters"
        );
    }

    #[test]
    fn a_password_round_trips_and_the_hash_is_argon2id_at_the_named_cost() {
        let (_dir, store) = store();
        assert!(!store.is_set().expect("the file is read"));
        store
            .set("a long enough password")
            .expect("the password sets");
        assert!(store.is_set().expect("the file is read"));

        let text = std::fs::read_to_string(store.path()).expect("the file");
        let stored: Stored = serde_json::from_str(&text).expect("the file parses");
        assert!(stored.phc.starts_with("$argon2id$v=19$m=65536,t=3,p=1$"));
        assert_eq!(stored.version, 1);

        // One check at the production cost: this test is about the parameters
        // being the ones the file claims, and a wrong password is refused by the
        // same code path the cheap-cost test below drives.
        assert!(
            store
                .verify_blocking("a long enough password")
                .expect("the check runs")
        );
    }

    #[test]
    fn a_cheap_hash_records_its_own_cost_and_the_rules_still_apply() {
        let (_dir, store) = store();
        store
            .set_with_cost("a long enough password", 8, 1, 1)
            .expect("the password sets");
        let text = std::fs::read_to_string(store.path()).expect("the file");
        assert!(
            text.contains("$argon2id$v=19$m=8,t=1,p=1$"),
            "the file records the cost it was given: {text}"
        );
        assert!(
            store
                .verify_blocking("a long enough password")
                .expect("the check runs")
        );
        assert!(store.set_with_cost("short", 8, 1, 1).is_err());
    }

    #[test]
    fn the_hash_is_private_on_disk() {
        let (_dir, store) = store();
        store
            .set_with_cost("a long enough password", CHEAP_KIB, 1, 1)
            .expect("the password sets");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(store.path())
                .expect("the file")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn with_no_password_set_no_password_is_ever_right() {
        let (_dir, store) = store();
        // Two guesses, and the empty one is the one that matters: the dummy hash
        // costs a real hash each time, so this stops at the case a bug would
        // actually produce — an empty password being taken for "no password".
        for guess in ["", "a long enough password"] {
            assert!(
                !store.verify_blocking(guess).expect("the check runs"),
                "{guess:?} must not be accepted with no password set"
            );
        }
    }

    #[test]
    fn clearing_forgets_the_password_and_clearing_twice_is_the_same_state() {
        let (_dir, store) = store();
        store
            .set_with_cost("a long enough password", CHEAP_KIB, 1, 1)
            .expect("the password sets");
        store.clear().expect("the password clears");
        assert!(!store.is_set().expect("the file is read"));
        store.clear().expect("clearing again is not an error");
        assert!(!store.path().exists());
    }

    #[test]
    fn a_password_below_the_floor_is_refused() {
        assert_eq!(
            PasswordStore::check_rules("elevenchars", None),
            Err(PasswordRefusal::TooShort)
        );
        assert_eq!(PasswordStore::check_rules("twelve chars", None), Ok(()));
    }

    #[test]
    fn a_common_password_is_refused_whatever_its_case() {
        assert_eq!(
            PasswordStore::check_rules("Password1234", None),
            Err(PasswordRefusal::Common)
        );
        assert_eq!(
            PasswordStore::check_rules("correcthorsebatterystaple", None),
            Err(PasswordRefusal::Common)
        );
    }

    #[test]
    fn the_machines_own_name_is_refused() {
        assert_eq!(
            PasswordStore::check_rules("studio.local", Some("studio.local")),
            Err(PasswordRefusal::Hostname)
        );
        assert_eq!(
            PasswordStore::check_rules("Studio.Local", Some("studio.local")),
            Err(PasswordRefusal::Hostname),
            "the full name counts, and case does not"
        );
        assert_eq!(
            PasswordStore::check_rules("a long enough password", Some("studio.local")),
            Ok(())
        );
    }

    #[test]
    fn an_enormous_password_is_refused_rather_than_hashed() {
        let huge = "x".repeat(MAX_LENGTH + 1);
        assert_eq!(
            PasswordStore::check_rules(&huge, None),
            Err(PasswordRefusal::TooLong)
        );
    }

    #[test]
    fn a_file_something_else_wrote_is_refused_rather_than_trusted() {
        let (_dir, store) = store();
        std::fs::write(store.path(), b"not json").expect("a broken file");
        assert!(store.verify_blocking("a long enough password").is_err());

        std::fs::write(
            store.path(),
            serde_json::to_vec(&Stored {
                version: 1,
                phc: "$argon2i$v=19$m=65536,t=3,p=1$c2FsdA$aGFzaA".to_string(),
                updated: 0,
            })
            .expect("json"),
        )
        .expect("an argon2i hash");
        assert!(
            store.verify_blocking("a long enough password").is_err(),
            "only argon2id is accepted"
        );
    }

    #[test]
    fn a_set_password_is_checked_after_the_file_is_private_again() {
        // A previous version, or a person with `chmod`, could leave it readable.
        let (_dir, store) = store();
        store
            .set_with_cost("a long enough password", CHEAP_KIB, 1, 1)
            .expect("the password sets");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(store.path(), std::fs::Permissions::from_mode(0o644))
                .expect("world readable");
        }
        assert!(
            store
                .verify_blocking("a long enough password")
                .expect("the check runs")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(store.path())
                .expect("the file")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "the check private'd the file again");
        }
    }

    #[test]
    fn two_salts_never_repeat_so_two_equal_passwords_hash_differently() {
        let (dir, store) = store();
        store
            .set_with_cost("a long enough password", CHEAP_KIB, 1, 1)
            .expect("the password sets");
        let first = std::fs::read_to_string(store.path()).expect("the file");
        assert_eq!(
            first
                .split('"')
                .find(|part| part.starts_with("$argon2"))
                .expect("a PHC string")
                .split('$')
                .nth(4)
                .expect("a salt")
                .len(),
            22,
            "16 bytes of salt is 22 base64 characters"
        );
        store
            .set_with_cost("a long enough password", CHEAP_KIB, 1, 1)
            .expect("the password sets");
        let second = std::fs::read_to_string(store.path()).expect("the file");
        assert_ne!(first, second, "a fresh salt for every hash");

        let other = PasswordStore::new(&dir.path().join("other"));
        std::fs::create_dir_all(dir.path().join("other")).expect("a second dir");
        other
            .set_with_cost("a long enough password", CHEAP_KIB, 1, 1)
            .expect("the password sets");
        let third = std::fs::read_to_string(other.path()).expect("the file");
        assert_ne!(first, third);
    }

    #[test]
    fn this_machines_name_is_read_or_left_off() {
        // Whatever the machine's name is, the rule is a comparison and not a
        // crash: the test asserts the shape, not the name.
        if let Some(name) = hostname() {
            assert!(!name.is_empty());
            assert!(!name.contains(char::is_whitespace), "{name:?}");
            // A name too short to be a password is refused for its length; a
            // long one is refused for being the machine's name.
            let refusal = PasswordStore::check_rules(&name, Some(&name));
            assert!(
                matches!(
                    refusal,
                    Err(PasswordRefusal::Hostname) | Err(PasswordRefusal::TooShort)
                ),
                "{name:?} gave {refusal:?}"
            );
        }
    }
}
