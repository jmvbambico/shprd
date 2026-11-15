//! A private `herdr server` for the `#[ignore]` live tests.
//!
//! Two test binaries in two crates need one: `crates/shprd-mux-herdr/tests/
//! live_herdr.rs` drives the adapter, and `crates/shprd/tests/spawn_live.rs`
//! drives the binary. Both include this file with `#[path]`, so there is one
//! socket-path rule, one readiness loop and one cleanup to keep honest.
//!
//! The server is isolated twice over: `HOME` and `HERDR_SOCKET_PATH` both point
//! into a fresh temporary directory, and every `herdr` command this helper runs
//! goes through `isolate` (in `support/herdr_env.rs`) first, so a live test
//! cannot see, or touch, the multiplexer the human is working in. The `HERDR_ENV`,
//! `HERDR_PANE_ID` and `HC_SESSION` a herdr pane exports to the test process are
//! removed as well: the socket alone would not keep a `herdr server` started
//! here from inheriting the pane it was started in.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output};
use std::time::{Duration, Instant};

use tempfile::TempDir;

/// The isolation rule, re-exported so a test that includes this file gets it
/// without loading `herdr_env.rs` as a second module of its own.
#[path = "herdr_env.rs"]
pub mod herdr_env;

use herdr_env::isolate;

/// How long a `herdr server` gets to answer before the test fails.
const READY_TIMEOUT: Duration = Duration::from_secs(20);

/// The length a unix socket path must stay under, terminator included.
///
/// `sockaddr_un::sun_path` is 104 bytes on macOS and 108 on Linux; anything
/// longer fails with "local socket name length exceeds capacity of sun_path".
/// `std::env::temp_dir()` on macOS is already about half of that, and an agent
/// sandbox or a nested `TMPDIR` adds more, so the path is checked before a
/// server is started rather than after it fails to.
const SUN_PATH_MAX: usize = if cfg!(target_os = "macos") { 104 } else { 108 };

/// A private `herdr server` that stops and cleans up when it is dropped.
pub struct LiveServer {
    /// The temp directory holding `HOME` and the socket; removed on drop.
    temp: TempDir,
    /// The socket `herdr` listens on, at the top of the temp directory.
    socket: PathBuf,
    /// The server process, while it is running.
    child: Option<Child>,
}

impl LiveServer {
    /// Starts a server, or returns `None` with a one-line reason to skip.
    ///
    /// A skip means the machine cannot host this test, not that the test
    /// failed: `herdr` is not installed, or `TMPDIR` is too long for a unix
    /// socket path. A server that is installed but does not answer is a
    /// failure, and it is stopped before the panic so no orphan is left behind.
    pub fn start() -> Option<LiveServer> {
        let temp = match TempDir::new() {
            Ok(temp) => temp,
            Err(err) => {
                eprintln!("skipping the live test: no temp dir ({err})");
                return None;
            }
        };
        // The socket sits at the top of the temp directory, not in a per-test
        // subdirectory: the path is already close to `sun_path`'s limit.
        let socket = temp.path().join("h.sock");
        let length = socket.as_os_str().as_encoded_bytes().len();
        if length >= SUN_PATH_MAX {
            eprintln!(
                "skipping the live test: TMPDIR is too long for a unix socket \
                 ({length} bytes, {} allowed): {}",
                SUN_PATH_MAX - 1,
                socket.display()
            );
            return None;
        }
        let mut server = LiveServer {
            temp,
            socket,
            child: None,
        };
        let log = match std::fs::File::create(server.home().join("server.log")) {
            Ok(file) => file,
            Err(err) => {
                eprintln!("skipping the live test: cannot write the server log ({err})");
                return None;
            }
        };
        let mut command = Command::new("herdr");
        isolate(&mut command, server.home());
        let child = match command
            .arg("server")
            .env("HOME", server.home())
            .env("HERDR_SOCKET_PATH", &server.socket)
            .stdout(std::process::Stdio::null())
            .stderr(log)
            .spawn()
        {
            Ok(child) => child,
            Err(err) => {
                eprintln!("skipping the live test: herdr is not installed ({err})");
                return None;
            }
        };
        // The process is recorded before the readiness loop starts, so a
        // server that never answers is stopped and reaped on the way out.
        server.child = Some(child);
        let started = Instant::now();
        loop {
            match server.herdr(&["workspace", "list"]) {
                Ok(_) => return Some(server),
                Err(answer) => {
                    if started.elapsed() < READY_TIMEOUT {
                        std::thread::sleep(Duration::from_millis(250));
                        continue;
                    }
                    let socket = server.socket.display().to_string();
                    let log = std::fs::read_to_string(server.home().join("server.log"))
                        .unwrap_or_default();
                    drop(server);
                    panic!("herdr server did not answer on {socket}: {answer}\n{log}");
                }
            }
        }
    }

    /// The temporary `HOME` the server runs under.
    pub fn home(&self) -> &Path {
        self.temp.path()
    }

    /// The unix socket the server listens on.
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Runs one `herdr` command against this server.
    pub fn herdr(&self, args: &[&str]) -> Result<Output, String> {
        let mut command = Command::new("herdr");
        isolate(&mut command, self.home());
        let output = command
            .args(args)
            .env("HOME", self.home())
            .env("HERDR_SOCKET_PATH", &self.socket)
            .output()
            .map_err(|err| err.to_string())?;
        if output.status.success() {
            Ok(output)
        } else {
            Err(String::from_utf8_lossy(&output.stderr).to_string())
        }
    }
}

impl Drop for LiveServer {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let mut command = Command::new("herdr");
            isolate(&mut command, self.home());
            let _ = command
                .args(["server", "stop"])
                .env("HOME", self.home())
                .env("HERDR_SOCKET_PATH", &self.socket)
                .status();
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
