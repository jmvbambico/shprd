//! shprd updates itself after the person accepts a task that changes shprd.
//!
//! The whole of it is two halves, because the service cannot swap its own
//! binary, restart itself and judge whether the new one came up:
//!
//! - **The trigger** ([`landed`]) runs inside the service, on the one path every
//!   landing is recorded through. When `tasks.self_update` is on for the
//!   repository it bumps a request counter and starts a detached helper through
//!   a [`Spawn`] the binary supplies. It never builds, and never waits.
//! - **The pipeline** ([`Pipeline`]) is what the helper runs. Everything it does
//!   to the machine goes through [`Steps`], so the sequence — is the diff worth a
//!   build, build, swap, install the desktop, restart once, check health, roll
//!   back — is one tested piece of logic, and the binary supplies the real
//!   effects while the tests supply fakes.
//!
//! Why it is safe:
//!
//! - One update at a time: the pipeline holds a lock file. A second helper finds
//!   it held and leaves.
//! - A landing that arrives during a build is not lost and is not a pile: it
//!   bumps the request counter, and the holder runs exactly one more pass when
//!   the counter moved. That pass builds only if the integration head is not the
//!   one already installed.
//! - What is installed is recorded per repository ([`Installed`]), so a landing
//!   that only touched docs builds nothing, a failed build leaves the next
//!   landing to try again, and a restart never rebuilds the commit it came from.
//! - A failed build changes nothing. A new binary that does not come up healthy
//!   is rolled back to `shprd.prev` and the person is told.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use serde::{Deserialize, Serialize};

/// The paths that make a landing worth a rebuild: shprd's own code, its shipped
/// data, the desktop's layout and mods, and the workspace manifests.
pub const WATCHED: [&str; 4] = ["crates/", "data/", "host/herdr-desktop/", "Cargo."];

const NOTHING_TO_DO: &str = "nothing needs doing; shprd is as it was";
const BACK_ON_OLD: &str = "nothing needs doing; shprd is back on the version it had";
const START_BY_HAND: &str = "start shprd again by hand: it may not be running";

/// The most passes one run of the pipeline makes, so a request counter that
/// keeps moving cannot hold the helper for ever.
pub const MAX_PASSES: usize = 5;

/// Whether any of `paths` is one a rebuild of shprd would pick up.
pub fn touches_binary(paths: &[String]) -> bool {
    paths
        .iter()
        .any(|path| WATCHED.iter().any(|prefix| path.starts_with(prefix)))
}

/// Where the updater keeps its files: the lock, the request counter, what is
/// installed, and the last outcome.
#[derive(Debug, Clone)]
pub struct Dirs {
    root: PathBuf,
}

impl Dirs {
    /// The updater's folder under `home`: `<home>/.shprd/self-update`.
    pub fn under_home(home: &Path) -> Self {
        Self {
            root: home.join(".shprd").join("self-update"),
        }
    }

    /// The updater's folder, named outright.
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The folder itself.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The machine-wide lock: one swap and restart at a time, whichever
    /// repository asked for it.
    fn lock(&self) -> PathBuf {
        self.root.join("lock")
    }

    /// One repository's own folder. Its lock and request counter live here, so
    /// a landing in one repository is never read as another's.
    fn repo_dir(&self, repo: &str) -> PathBuf {
        // FNV-1a: the same key in the service and in the helper, whatever
        // version of Rust each was built with.
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in repo.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
        self.root.join("repos").join(format!("{hash:016x}"))
    }

    fn repo_lock(&self, repo: &str) -> PathBuf {
        self.repo_dir(repo).join("lock")
    }

    /// The file holding `repo`'s request counter.
    pub fn request(&self, repo: &str) -> PathBuf {
        self.repo_dir(repo).join("request")
    }

    fn installed(&self) -> PathBuf {
        self.root.join("installed.json")
    }

    fn status(&self) -> PathBuf {
        self.root.join("status.json")
    }
}

/// The commit each repository's running shprd was last built from.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Installed(pub BTreeMap<String, String>);

impl Installed {
    /// What `dirs` holds, or nothing when it holds no readable record.
    pub fn read(dirs: &Dirs) -> Self {
        fs::read_to_string(dirs.installed())
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    fn write(&self, dirs: &Dirs) -> Result<(), String> {
        let text = serde_json::to_string_pretty(self).map_err(|err| err.to_string())?;
        write_file(&dirs.installed(), text.as_bytes())
    }
}

/// How the last pass ended, in the words a person reads.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Status {
    /// When it ended, ms since the epoch.
    pub at_ms: u64,
    /// Whether the machine is running what the pass meant it to.
    pub ok: bool,
    /// One plain line.
    pub line: String,
}

/// The last pass's outcome, when one was recorded.
pub fn read_status(dirs: &Dirs) -> Option<Status> {
    serde_json::from_str(&fs::read_to_string(dirs.status()).ok()?).ok()
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| format!("{}: {err}", parent.display()))?;
    }
    let tmp = path.with_extension("tmp");
    let mut file = fs::File::create(&tmp).map_err(|err| format!("{}: {err}", tmp.display()))?;
    file.write_all(bytes)
        .map_err(|err| format!("{}: {err}", tmp.display()))?;
    drop(file);
    fs::rename(&tmp, path).map_err(|err| format!("{}: {err}", path.display()))
}

fn open_lock(path: &Path) -> Option<fs::File> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).ok()?;
    }
    fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .ok()
}

fn now_ms() -> u64 {
    use shprd_core::log::{Clock, SystemClock};
    SystemClock.now_ms()
}

/// Notes that something landed in `repo`: the holder of its lock, or the next
/// one, will look again. Returns the counter's new value.
pub fn bump(dirs: &Dirs, repo: &str) -> Result<u128, String> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    // Two bumps in one clock tick must still differ, or the holder would read
    // them as one.
    let next = nanos.max(read_request(dirs, repo).saturating_add(1));
    write_file(&dirs.request(repo), next.to_string().as_bytes())?;
    Ok(next)
}

fn read_request(dirs: &Dirs, repo: &str) -> u128 {
    fs::read_to_string(dirs.request(repo))
        .ok()
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(0)
}

/// What the machine does for the pipeline. Every method is an effect; the
/// sequence that uses them is [`Pipeline::run`].
pub trait Steps {
    /// The commit of the branch accepted work lands on, as of now.
    fn head(&self) -> Result<String, String>;
    /// The commit the running binary most likely came from, when nothing was
    /// recorded for it: the newest commit older than the binary itself.
    fn baseline(&self, head: &str) -> Option<String>;
    /// The paths that differ between two commits.
    fn changed(&self, from: &str, to: &str) -> Result<Vec<String>, String>;
    /// Builds `head` in an engine-owned detached worktree and returns the
    /// binary it made. Leaves the running shprd alone.
    fn build(&self, head: &str) -> Result<PathBuf, String>;
    /// Puts the built binary in place of the running one, keeping the previous
    /// as `shprd.prev`.
    fn swap(&self, built: &Path) -> Result<(), String>;
    /// Installs the new binary's desktop layout and mods.
    fn install_desktop(&self) -> Result<(), String>;
    /// Tells the coordinator, in a line.
    fn say(&self, line: &str);
    /// Puts a card in front of the person: something went wrong and they should
    /// know, in plain words. `default_action` is what happens if they do nothing.
    fn escalate(&self, body: &str, default_action: &str);
    /// Stops the service by its recorded pid. Whatever restarts the service
    /// then starts it from the swapped binary.
    fn restart(&self) -> Result<(), String>;
    /// Waits for the restarted service to answer its health check, and returns
    /// the version it reports.
    fn healthy(&self) -> Result<String, String>;
    /// Puts `shprd.prev` back, reinstalls its desktop and restarts the service.
    fn rollback(&self) -> Result<(), String>;
}

/// How one pass ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Another helper for this repository holds its lock; it will see this
    /// landing.
    Busy,
    /// The installed commit is the head: nothing to do.
    UpToDate,
    /// What landed since touches nothing a rebuild would pick up.
    NoCodeChange,
    /// The head could not be read, so nothing was decided.
    Unknown(String),
    /// The build failed; nothing on the machine changed.
    BuildFailed(String),
    /// The new version is running.
    Updated {
        /// The commit it was built from.
        head: String,
        /// The version it reports.
        version: String,
    },
    /// The new version did not come up and the previous one was put back.
    RolledBack(String),
    /// The swap or the rollback itself failed: a person must look.
    Stuck(String),
}

/// One repository's update, over some [`Steps`].
pub struct Pipeline<'a, S: Steps> {
    /// The updater's files.
    pub dirs: &'a Dirs,
    /// The effects.
    pub steps: &'a S,
    /// The repository whose landings are being applied (a key of [`Installed`]).
    pub repo: &'a str,
}

impl<S: Steps> Pipeline<'_, S> {
    /// Runs until nothing new has landed, one update at a time.
    ///
    /// Returns one [`Outcome`] per pass. A second helper that finds the lock
    /// held returns [`Outcome::Busy`] and does nothing.
    pub fn run(&self) -> Vec<Outcome> {
        if let Err(err) = fs::create_dir_all(self.dirs.root()) {
            return vec![Outcome::Unknown(err.to_string())];
        }
        self.locked()
    }

    fn locked(&self) -> Vec<Outcome> {
        // This repository's own lock first: a second helper for the same
        // repository leaves, because the holder sees its landing (the counter).
        let Some(mine) = open_lock(&self.dirs.repo_lock(self.repo)) else {
            return vec![Outcome::Unknown(String::from(
                "the lock could not be opened",
            ))];
        };
        if mine.try_lock().is_err() {
            return vec![Outcome::Busy];
        }
        // Then the machine's: another repository's update is one binary being
        // swapped, so this one waits its turn rather than being dropped.
        let Some(machine) = open_lock(&self.dirs.lock()) else {
            return vec![Outcome::Unknown(String::from(
                "the lock could not be opened",
            ))];
        };
        if let Err(err) = machine.lock() {
            return vec![Outcome::Unknown(err.to_string())];
        }
        let mut outcomes = Vec::new();
        for _ in 0..MAX_PASSES {
            let seen = read_request(self.dirs, self.repo);
            let outcome = self.once();
            let finished = matches!(outcome, Outcome::Stuck(_));
            outcomes.push(outcome);
            if finished || read_request(self.dirs, self.repo) == seen {
                break;
            }
        }
        // Dropping the files releases the locks.
        drop(machine);
        drop(mine);
        outcomes
    }

    /// One look at the head, and one update when it is due.
    pub fn once(&self) -> Outcome {
        let head = match self.steps.head() {
            Ok(head) => head,
            Err(why) => return Outcome::Unknown(why),
        };
        let mut installed = Installed::read(self.dirs);
        let from = installed
            .0
            .get(self.repo)
            .cloned()
            .or_else(|| self.steps.baseline(&head));
        if from.as_deref() == Some(head.as_str()) {
            return Outcome::UpToDate;
        }
        if let Some(from) = &from {
            match self.steps.changed(from, &head) {
                Ok(paths) if !touches_binary(&paths) => return Outcome::NoCodeChange,
                Ok(_) => {}
                // A diff that cannot be read is not a reason to leave the
                // machine behind: build.
                Err(why) => {
                    tracing::warn!(%why, "the landed diff could not be read; building anyway")
                }
            }
        }
        let short = head.chars().take(9).collect::<String>();
        let built = match self.steps.build(&head) {
            Ok(built) => built,
            Err(why) => {
                let line = could_not_update(&short, &why);
                self.finish(false, &line);
                self.steps.escalate(&line, NOTHING_TO_DO);
                return Outcome::BuildFailed(why);
            }
        };
        self.steps.say(&format!(
            "shprd will restart once to apply the accepted change ({short}). Nothing is lost: the work on the board stays."
        ));
        if let Err(why) = self.steps.swap(&built) {
            let line = could_not_update(&short, &why);
            self.finish(false, &line);
            self.steps.escalate(&line, NOTHING_TO_DO);
            return Outcome::Stuck(why);
        }
        let came_up = self
            .steps
            .install_desktop()
            .and_then(|()| self.steps.restart())
            .and_then(|()| self.steps.healthy());
        match came_up {
            Ok(version) => {
                installed.0.insert(self.repo.to_string(), head.clone());
                if let Err(why) = installed.write(self.dirs) {
                    tracing::warn!(%why, "what is installed could not be recorded");
                }
                let line = format!(
                    "shprd restarted and is now running version {version} (commit {short})."
                );
                self.finish(true, &line);
                self.steps.say(&line);
                Outcome::Updated { head, version }
            }
            Err(why) => match self.steps.rollback() {
                Ok(()) => {
                    let line = format!(
                        "The new shprd ({short}) did not start properly, so shprd went back to the version it had before. Reason: {why}"
                    );
                    self.finish(false, &line);
                    self.steps.escalate(&line, BACK_ON_OLD);
                    self.steps.say(&line);
                    Outcome::RolledBack(why)
                }
                Err(also) => {
                    let line = format!(
                        "The new shprd ({short}) did not start properly and putting the old one back failed too ({also}). shprd may not be running: start it again by hand. Reason: {why}"
                    );
                    self.finish(false, &line);
                    self.steps.escalate(&line, START_BY_HAND);
                    Outcome::Stuck(also)
                }
            },
        }
    }

    fn finish(&self, ok: bool, line: &str) {
        let status = Status {
            at_ms: now_ms(),
            ok,
            line: line.to_string(),
        };
        if let Ok(text) = serde_json::to_string_pretty(&status)
            && let Err(why) = write_file(&self.dirs.status(), text.as_bytes())
        {
            tracing::warn!(%why, "the update's outcome could not be recorded");
        }
    }
}

/// What a person is told when an update could not be built or put in place:
/// shprd is as it was, and why. It holds no command and no backtick.
fn could_not_update(short: &str, why: &str) -> String {
    let why = why.replace('`', "'");
    let why = why.trim().trim_end_matches('.');
    format!(
        "shprd could not update itself to {short} and is still running the version it had. Nothing was changed. Why: {why}."
    )
}

/// Starts a detached helper that runs the pipeline for one repository.
pub type Spawn = Arc<dyn Fn(&Job) -> Result<(), String> + Send + Sync>;

/// What the helper is started for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    /// The repository a task landed in.
    pub repo: PathBuf,
    /// The branch it landed on.
    pub base: String,
    /// Whether the landing was local (no forge), so the head is the local ref.
    pub local: bool,
}

/// What runs when a task lands: reads the setting, notes the landing, and
/// starts the helper.
pub struct Trigger {
    home: PathBuf,
    dirs: Dirs,
    spawn: Spawn,
}

impl Trigger {
    /// A trigger that keeps its files under `home` and starts helpers with `spawn`.
    pub fn new(home: &Path, spawn: Spawn) -> Self {
        Self {
            home: home.to_path_buf(),
            dirs: Dirs::under_home(home),
            spawn,
        }
    }

    /// Whether `tasks.self_update` is on for `repo`. A settings file that cannot
    /// be read is off: nothing rebuilds the person's shprd on a guess.
    pub fn enabled(&self, repo: &Path) -> bool {
        shprd_core::settings::Loaded::read(&self.home, Some(repo))
            .map(|loaded| loaded.resolve(None).tasks().self_update)
            .unwrap_or(false)
    }

    /// A task landed on `base` in `repo`.
    pub fn landed(&self, repo: &Path, base: &str, local: bool) {
        if !self.enabled(repo) {
            return;
        }
        if let Err(why) = bump(&self.dirs, &repo.to_string_lossy()) {
            tracing::warn!(%why, "the self-update request could not be recorded");
        }
        let job = Job {
            repo: repo.to_path_buf(),
            base: base.to_string(),
            local,
        };
        if let Err(why) = (self.spawn)(&job) {
            tracing::warn!(%why, "the self-update helper did not start");
        }
    }
}

static TRIGGER: OnceLock<Trigger> = OnceLock::new();

/// Installs the service's trigger, once. `false` when one is already installed.
pub fn install(trigger: Trigger) -> bool {
    TRIGGER.set(trigger).is_ok()
}

/// A task landed on `base` in `repo`; a no-op where no trigger is installed (a
/// test, a record-only pass).
pub fn landed(repo: &Path, base: &str, local: bool) {
    if let Some(trigger) = TRIGGER.get() {
        trigger.landed(repo, base, local);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::cell::RefCell;
    use std::sync::Mutex;
    use tempfile::TempDir;

    /// A machine that records what the pipeline asked of it.
    struct Fake<'a> {
        dirs: &'a Dirs,
        heads: RefCell<Vec<String>>,
        paths: Vec<String>,
        build_fails: bool,
        healthy: bool,
        /// Landings that arrive while a build runs: each moves the head and
        /// bumps the request counter, as a real landing does.
        landings_during_build: RefCell<Vec<String>>,
        calls: RefCell<Vec<String>>,
        said: RefCell<Vec<String>>,
        escalated: RefCell<Vec<String>>,
    }

    impl<'a> Fake<'a> {
        fn new(dirs: &'a Dirs, head: &str, paths: &[&str]) -> Self {
            Self {
                dirs,
                heads: RefCell::new(vec![head.to_string()]),
                paths: paths.iter().map(|path| (*path).to_string()).collect(),
                build_fails: false,
                healthy: true,
                landings_during_build: RefCell::new(Vec::new()),
                calls: RefCell::new(Vec::new()),
                said: RefCell::new(Vec::new()),
                escalated: RefCell::new(Vec::new()),
            }
        }

        fn count(&self, what: &str) -> usize {
            self.calls
                .borrow()
                .iter()
                .filter(|call| call.starts_with(what))
                .count()
        }
    }

    impl Steps for Fake<'_> {
        fn head(&self) -> Result<String, String> {
            Ok(self.heads.borrow().last().cloned().expect("a head"))
        }
        fn baseline(&self, _head: &str) -> Option<String> {
            Some(String::from("base0"))
        }
        fn changed(&self, _from: &str, _to: &str) -> Result<Vec<String>, String> {
            Ok(self.paths.clone())
        }
        fn build(&self, head: &str) -> Result<PathBuf, String> {
            self.calls.borrow_mut().push(format!("build {head}"));
            for next in self.landings_during_build.borrow_mut().drain(..) {
                self.heads.borrow_mut().push(next);
                bump(self.dirs, "/repo").expect("a bump");
            }
            if self.build_fails {
                Err(String::from("error[E0308]: mismatched types"))
            } else {
                Ok(PathBuf::from("/built/shprd"))
            }
        }
        fn swap(&self, _built: &Path) -> Result<(), String> {
            self.calls.borrow_mut().push(String::from("swap"));
            Ok(())
        }
        fn install_desktop(&self) -> Result<(), String> {
            self.calls.borrow_mut().push(String::from("install"));
            Ok(())
        }
        fn say(&self, line: &str) {
            self.said.borrow_mut().push(line.to_string());
        }
        fn escalate(&self, body: &str, default_action: &str) {
            self.escalated
                .borrow_mut()
                .push(format!("{body} || {default_action}"));
        }
        fn restart(&self) -> Result<(), String> {
            self.calls.borrow_mut().push(String::from("restart"));
            Ok(())
        }
        fn healthy(&self) -> Result<String, String> {
            if self.healthy {
                Ok(String::from("0.0.1"))
            } else {
                Err(String::from("the service never answered"))
            }
        }
        fn rollback(&self) -> Result<(), String> {
            self.calls.borrow_mut().push(String::from("rollback"));
            Ok(())
        }
    }

    fn run(fake: &Fake<'_>, dirs: &Dirs) -> Vec<Outcome> {
        Pipeline {
            dirs,
            steps: fake,
            repo: "/repo",
        }
        .run()
    }

    #[test]
    fn only_shprds_own_paths_are_worth_a_build() {
        let paths = |list: &[&str]| list.iter().map(|p| (*p).to_string()).collect::<Vec<_>>();
        assert!(touches_binary(&paths(&[
            "docs/plan.md",
            "crates/shprd/src/main.rs"
        ])));
        assert!(touches_binary(&paths(&["data/skills/compat.toml"])));
        assert!(touches_binary(&paths(&[
            "host/herdr-desktop/mods/lib/x.js"
        ])));
        assert!(touches_binary(&paths(&["Cargo.lock"])));
        assert!(!touches_binary(&paths(&[
            "docs/plan.md",
            "README.md",
            "web/src/a.ts"
        ])));
        assert!(!touches_binary(&paths(&[])));
    }

    #[test]
    fn a_landing_that_only_touches_docs_builds_nothing() {
        let tmp = TempDir::new().unwrap();
        let dirs = Dirs::at(tmp.path());
        let fake = Fake::new(&dirs, "h1", &["docs/plan.md", "README.md"]);
        assert_eq!(run(&fake, &dirs), vec![Outcome::NoCodeChange]);
        assert_eq!(fake.count("build"), 0);
        assert_eq!(fake.count("restart"), 0);
    }

    #[test]
    fn a_code_landing_builds_swaps_and_restarts_once() {
        let tmp = TempDir::new().unwrap();
        let dirs = Dirs::at(tmp.path());
        let fake = Fake::new(&dirs, "h1", &["crates/shprd/src/main.rs"]);
        let out = run(&fake, &dirs);
        assert_eq!(
            out,
            vec![Outcome::Updated {
                head: "h1".into(),
                version: "0.0.1".into()
            }]
        );
        assert_eq!(fake.count("build"), 1);
        assert_eq!(fake.count("swap"), 1);
        assert_eq!(fake.count("install"), 1);
        assert_eq!(fake.count("restart"), 1);
        let said = fake.said.borrow();
        assert!(said[0].contains("restart once"), "{said:?}");
        assert!(said[1].contains("now running version 0.0.1"), "{said:?}");
        assert_eq!(
            Installed::read(&dirs).0.get("/repo").map(String::as_str),
            Some("h1")
        );
        assert!(read_status(&dirs).unwrap().ok);
        // Run again with the same head: it is installed, so nothing builds.
        assert_eq!(run(&fake, &dirs), vec![Outcome::UpToDate]);
        assert_eq!(fake.count("build"), 1);
    }

    #[test]
    fn two_landings_during_one_build_cause_exactly_one_more_build() {
        let tmp = TempDir::new().unwrap();
        let dirs = Dirs::at(tmp.path());
        let fake = Fake::new(&dirs, "h1", &["crates/a.rs"]);
        // Two landings arrive while the first build runs.
        fake.landings_during_build
            .borrow_mut()
            .extend(["h2".to_string(), "h3".to_string()]);
        let out = run(&fake, &dirs);
        let builds: Vec<String> = fake
            .calls
            .borrow()
            .iter()
            .filter(|call| call.starts_with("build"))
            .cloned()
            .collect();
        assert_eq!(
            builds,
            vec!["build h1".to_string(), "build h3".to_string()],
            "{out:?}"
        );
        assert_eq!(out.len(), 2);
        assert_eq!(
            Installed::read(&dirs).0.get("/repo").map(String::as_str),
            Some("h3")
        );
    }

    #[test]
    fn a_failed_build_changes_nothing_and_says_so_in_plain_words() {
        let tmp = TempDir::new().unwrap();
        let dirs = Dirs::at(tmp.path());
        let mut fake = Fake::new(&dirs, "h1", &["crates/a.rs"]);
        fake.build_fails = true;
        let out = run(&fake, &dirs);
        assert!(
            matches!(out.as_slice(), [Outcome::BuildFailed(_)]),
            "{out:?}"
        );
        assert_eq!(fake.count("swap"), 0);
        assert_eq!(fake.count("restart"), 0);
        assert!(Installed::read(&dirs).0.is_empty());
        let card = fake.escalated.borrow();
        assert_eq!(card.len(), 1);
        assert!(
            card[0].contains("still running the version it had"),
            "{card:?}"
        );
        assert!(
            card[0].starts_with("shprd could not update itself to ")
                && card[0].contains(" Nothing was changed. Why: ")
                && !card[0].contains('`'),
            "{card:?}"
        );
        assert!(!read_status(&dirs).unwrap().ok);
    }

    #[test]
    fn a_new_binary_that_fails_its_health_check_is_rolled_back() {
        let tmp = TempDir::new().unwrap();
        let dirs = Dirs::at(tmp.path());
        let mut fake = Fake::new(&dirs, "h1", &["crates/a.rs"]);
        fake.healthy = false;
        let out = run(&fake, &dirs);
        assert!(
            matches!(out.as_slice(), [Outcome::RolledBack(_)]),
            "{out:?}"
        );
        assert_eq!(fake.count("rollback"), 1);
        assert!(
            Installed::read(&dirs).0.is_empty(),
            "the bad commit is not recorded as installed"
        );
        let card = fake.escalated.borrow();
        assert!(card[0].contains("went back to the version it had"));
        assert!(card[0].contains(BACK_ON_OLD), "{card:?}");
    }

    #[test]
    fn another_repos_update_holding_the_machine_does_not_drop_this_landing() {
        let tmp = TempDir::new().unwrap();
        let dirs = Dirs::at(tmp.path());
        fs::create_dir_all(dirs.root()).unwrap();
        // Repository A's helper holds the machine-wide lock.
        let held = open_lock(&dirs.lock()).unwrap();
        held.lock().unwrap();
        std::thread::scope(|scope| {
            let handle = scope.spawn(|| {
                let fake = Fake::new(&dirs, "h1", &["crates/a.rs"]);
                let out = Pipeline {
                    dirs: &dirs,
                    steps: &fake,
                    repo: "/repo-b",
                }
                .run();
                (out, fake.count("build"))
            });
            std::thread::sleep(std::time::Duration::from_millis(200));
            assert!(!handle.is_finished(), "B waits for A rather than leaving");
            drop(held);
            let (out, builds) = handle.join().unwrap();
            assert!(
                matches!(out.as_slice(), [Outcome::Updated { .. }]),
                "{out:?}"
            );
            assert_eq!(builds, 1);
        });
        assert_eq!(
            Installed::read(&dirs).0.get("/repo-b").map(String::as_str),
            Some("h1")
        );
    }

    #[test]
    fn a_second_helper_finds_the_lock_held_and_leaves() {
        let tmp = TempDir::new().unwrap();
        let dirs = Dirs::at(tmp.path());
        fs::create_dir_all(dirs.root()).unwrap();
        let held = open_lock(&dirs.repo_lock("/repo")).unwrap();
        held.try_lock().unwrap();
        let fake = Fake::new(&dirs, "h1", &["crates/a.rs"]);
        assert_eq!(run(&fake, &dirs), vec![Outcome::Busy]);
        assert_eq!(fake.count("build"), 0);
    }

    #[test]
    fn the_trigger_does_nothing_unless_the_setting_is_on() {
        let home = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let jobs: Arc<Mutex<Vec<Job>>> = Arc::new(Mutex::new(Vec::new()));
        let spawn: Spawn = {
            let jobs = jobs.clone();
            Arc::new(move |job: &Job| {
                jobs.lock().unwrap().push(job.clone());
                Ok(())
            })
        };
        let trigger = Trigger::new(home.path(), spawn);
        trigger.landed(repo.path(), "integration/m1", true);
        assert!(jobs.lock().unwrap().is_empty(), "off by default");
        let dir = home.path().join(".config").join("shprd");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("settings.toml"),
            "version = 1\n[tasks]\nself_update = true\n",
        )
        .unwrap();
        assert!(trigger.enabled(repo.path()));
        trigger.landed(repo.path(), "integration/m1", true);
        assert_eq!(jobs.lock().unwrap().len(), 1);
        assert!(
            read_request(
                &Dirs::under_home(home.path()),
                &repo.path().to_string_lossy()
            ) > 0
        );
    }
}
