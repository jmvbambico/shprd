//! A private Herdr desktop for the `#[ignore]` live test.
//!
//! M7ef §2.1 is the recipe, and the reason for every piece:
//!
//! - `HC_SESSION` names a core of this test's own, so nothing here can address
//!   the person's core (whose session is `default`).
//! - `HERDR_APP_HOME` gives the window a home of its own, so it reads and
//!   writes no mods, layouts or settings of the person's.
//! - The XDG directories point at a **short symlink** under `/tmp`: on macOS
//!   the core's socket path must fit in 103 bytes
//!   (`$XDG_STATE_HOME/herdr-core/<session>/<session>.sock`), and a temporary
//!   folder under `$TMPDIR` is deeper than that. The symlink is the spike's own
//!   trick, and it lives in the test's temporary folder, not loose in `/tmp`.
//!
//! Everything this creates — the core, the window, the symlink — is removed by
//! [`Live::drop`], including the core's terminals.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// A private core session and (when asked) a private window over it.
#[derive(Debug)]
pub struct Live {
    root: tempfile::TempDir,
    /// The short path the XDG variables point at, `/tmp/shprd-live-<pid>`.
    short: PathBuf,
    session: String,
    window: Option<Child>,
}

impl Live {
    /// Set up a private session, and start a window over it.
    ///
    /// Returns an error rather than panicking so a caller can skip the test when
    /// `hc` is not installed on this machine.
    pub fn new() -> Result<Self, String> {
        let root = tempfile::Builder::new()
            .prefix("shprd-live-")
            .tempdir()
            .map_err(|error| format!("no temporary folder: {error}"))?;
        let short = PathBuf::from(format!("/tmp/shprd-live-{}", std::process::id()));
        std::fs::create_dir_all(&short)
            .map_err(|error| format!("cannot make {}: {error}", short.display()))?;
        for name in ["cfg", "state", "data", "run", "cache", "app"] {
            std::fs::create_dir_all(short.join(name))
                .map_err(|error| format!("cannot make {}: {error}", short.join(name).display()))?;
        }
        let session = format!("shprd-live-{}", std::process::id());
        Ok(Self {
            root,
            short,
            session,
            window: None,
        })
    }

    /// The root of everything this test made, for the record.
    pub fn root(&self) -> &Path {
        self.root.path()
    }

    /// The `hc` on `PATH`, or `SHPRD_HC` when it names one.
    pub fn hc(&self) -> PathBuf {
        std::env::var_os("SHPRD_HC")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("hc"))
    }

    /// The session name, unique to this run.
    pub fn session(&self) -> String {
        self.session.clone()
    }

    /// The XDG directories, which are what make the session private in the
    /// filesystem as well as in the core: `HC_SESSION` alone names a core, and
    /// where that core keeps its socket comes from these.
    pub fn directories(&self) -> Vec<(String, String)> {
        let at = |name: &str| self.short.join(name).to_string_lossy().into_owned();
        vec![
            ("XDG_CONFIG_HOME".to_string(), at("cfg")),
            ("XDG_STATE_HOME".to_string(), at("state")),
            ("XDG_DATA_HOME".to_string(), at("data")),
            ("XDG_RUNTIME_DIR".to_string(), at("run")),
            ("XDG_CACHE_HOME".to_string(), at("cache")),
        ]
    }

    /// The environment every command here runs with, as M7ef §2.1 lists it.
    pub fn environment(&self) -> Vec<(String, String)> {
        let mut environment = self.directories();
        environment.push(("HC_SESSION".to_string(), self.session.clone()));
        environment.push((
            "HERDR_APP_HOME".to_string(),
            self.short.join("app").to_string_lossy().into_owned(),
        ));
        environment
    }

    /// Run one `hc` command in the private session and return its stdout.
    pub fn hc_call(&self, args: &[&str]) -> Result<String, String> {
        let mut command = Command::new(self.hc());
        command.args(args).stdin(Stdio::null());
        for (key, value) in self.environment() {
            command.env(key, value);
        }
        let output = command
            .output()
            .map_err(|error| format!("cannot run hc: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "hc {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Run one `herdr-app` command in the private home and return its stdout.
    pub fn app_call(&self, args: &[&str]) -> Result<String, String> {
        let binary = std::env::var_os("SHPRD_HERDR_APP")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/Applications/Herdr.app/Contents/MacOS/Herdr"));
        let mut command = Command::new(binary);
        command.args(args).stdin(Stdio::null());
        for (key, value) in self.environment() {
            command.env(key, value);
        }
        let output = command
            .output()
            .map_err(|error| format!("cannot run herdr-app: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "herdr-app {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Start a window over the private home, and wait for it to answer.
    pub fn start_window(&mut self) -> Result<(), String> {
        let binary = std::env::var_os("SHPRD_HERDR_APP")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/Applications/Herdr.app/Contents/MacOS/Herdr"));
        let log = self.short.join("window.log");
        let out = std::fs::File::create(&log)
            .map_err(|error| format!("cannot write {}: {error}", log.display()))?;
        let err = out
            .try_clone()
            .map_err(|error| format!("cannot copy the log handle: {error}"))?;
        let mut command = Command::new(&binary);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::from(out))
            .stderr(Stdio::from(err));
        for (key, value) in self.environment() {
            command.env(key, value);
        }
        let child = command
            .spawn()
            .map_err(|error| format!("cannot start {}: {error}", binary.display()))?;
        self.window = Some(child);
        for _ in 0..100 {
            if self.app_call(&["status"]).is_ok() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Err(format!("the window never answered (see {})", log.display()))
    }

    /// Stop the core and the window this test made.
    pub fn stop(&mut self) {
        if let Some(mut window) = self.window.take() {
            let _ = self.app_call(&["do", "app.quit"]);
            std::thread::sleep(Duration::from_millis(300));
            let _ = window.kill();
            let _ = window.wait();
        }
        let _ = self.hc_call(&["server", "stop"]);
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        self.stop();
        // The symlink is a path of our own; the temp dir goes with `root`.
        let _ = std::fs::remove_dir_all(&self.short);
    }
}
