//! `shprd self-update run`, end to end, against a fake build command and a fake
//! service.
//!
//! Nothing here touches the machine the test runs on: the repository, the
//! "installed" binary, the service's state directory, the Herdr app home and the
//! updater's own folder are all temporary, and the child gets the isolation rule
//! every spawning test applies. The "build" is a shell script that copies a fake
//! binary into place; the "binary" is a shell script that, asked to `serve`,
//! becomes a tiny Python HTTP server answering `/api/health` and writing its own
//! `service.json`; the supervisor the desktop's footer plays is a thread that
//! starts `<install>/shprd serve` whenever no service is running.
//!
//! What it proves: a docs-only landing builds nothing; a code landing builds once
//! in a detached worktree outside the repository and restarts the service once;
//! a landing that arrives during a build causes exactly one more build; a failed
//! build leaves the binary untouched; a new binary that fails its health check is
//! rolled back.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tempfile::TempDir;

const FAKE_SERVICE: &str = r#"
import json, os, socketserver, sys, uuid
from http.server import BaseHTTPRequestHandler, HTTPServer

state, version = sys.argv[1], sys.argv[2]
ident = uuid.uuid4().hex

class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/api/health/repos":
            body = json.dumps({"ok": True, "checked": 0, "failed": []}).encode()
        else:
            body = json.dumps({"ok": True, "id": ident, "version": version}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass

class Server(HTTPServer):
    # HTTPServer.server_bind looks the host's full name up (socket.getfqdn),
    # which can stall for many seconds on a CI Mac; a loopback test server
    # needs no name.
    def server_bind(self):
        socketserver.TCPServer.server_bind(self)
        self.server_name = "127.0.0.1"
        self.server_port = self.server_address[1]

server = Server(("127.0.0.1", 0), Handler)
record = {"port": server.server_address[1], "id": ident, "pid": os.getpid(), "version": version, "root": ""}
tmp = os.path.join(state, "service.json.tmp")
with open(tmp, "w") as out:
    out.write(json.dumps(record))
os.replace(tmp, os.path.join(state, "service.json"))
server.serve_forever()
"#;

/// A fake `shprd`: `install` logs and succeeds, `serve` becomes the fake
/// service (or dies at once when this is the "bad" build).
fn fake_binary(version: &str) -> String {
    format!(
        r#"#!/bin/sh
VERSION={version}
for a in "$@"; do
  if [ "$a" = install ]; then echo "install $VERSION" >> "$FAKE_LOG"; exit 0; fi
done
[ "$1" = serve ] || exit 0
[ "$VERSION" = bad ] && exit 1
# The unfoldable build is the REAL shprd, serving a repository whose log it
# cannot fold: it comes up once (its own check catches it), and a supervisor
# that restarts it before the rollback's file swap finds it gone, as it would a
# service that cannot start.
if [ "$VERSION" = unfoldable ]; then
  [ -f "$FAKE_STATE/unfoldable-served" ] && exit 1
  touch "$FAKE_STATE/unfoldable-served"
  "$FAKE_REAL" --repo "$FAKE_BAD_REPO" --home "$FAKE_HOME" serve --port 0 &
  real=$!
  trap 'kill $real 2>/dev/null' TERM INT
  # The repository was readable when the service started (the real service
  # refuses a log it cannot read at start-up); its log goes bad once the
  # service is up, which is what an old log under a newer fold looks like to a
  # service that already holds the repository. The health check folds it again.
  until grep -q "$real" "$FAKE_STATE/service.json" 2>/dev/null; do sleep 0.02; done
  printf '{{"this": "is not an event"}}\n' > "$FAKE_BAD_REPO/.shprd/events.jsonl"
  wait $real
  exit $?
fi
exec python3 "$FAKE_PY" "$FAKE_STATE" "$VERSION"
"#
    )
}

/// The fake build. `BUILD_MODE=fail` fails; `BUILD_MODE=land` makes a new commit
/// on the integration branch during the first build, and bumps the request
/// counter the way a landing does.
const FAKE_BUILD: &str = r#"#!/bin/sh
echo "build pwd=$(pwd) head=$(git rev-parse HEAD)" >> "$FAKE_LOG"
if [ "$BUILD_MODE" = fail ]; then
  echo "error[E0308]: mismatched types" >&2
  exit 1
fi
if [ "$BUILD_MODE" = land ] && [ ! -f "$FAKE_STATE/landed-once" ]; then
  touch "$FAKE_STATE/landed-once"
  (cd "$FAKE_REPO" && echo more >> crates/x.rs && git commit -qam "land two")
  echo 99999999999999999999 > "$FAKE_REQUEST"
fi
mkdir -p "$CARGO_TARGET_DIR/release"
cp "$NEW_BINARY" "$CARGO_TARGET_DIR/release/shprd"
"#;

fn have_python() -> bool {
    Command::new("python3")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn write_script(path: &Path, text: &str) {
    use std::os::unix::fs::PermissionsExt;
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// Appends one line to the supervisor's error file, best effort.
fn note(path: &Path, line: &str) {
    use std::io::Write;
    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{line}");
    }
}

/// Starts `<install> serve` whenever none is running, as the layout's footer does.
struct Supervisor {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Supervisor {
    fn start(install: PathBuf, env: Vec<(String, String)>, errors: PathBuf) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let handle = std::thread::spawn(move || {
            let mut children: Vec<Child> = Vec::new();
            while !flag.load(Ordering::SeqCst) {
                // Reaping is what lets the helper see the old service as gone.
                children.retain_mut(|child| match child.try_wait() {
                    Ok(None) => true,
                    Ok(Some(status)) => {
                        note(&errors, &format!("serve {} exited: {status}", child.id()));
                        false
                    }
                    Err(err) => {
                        note(&errors, &format!("serve {} unreadable: {err}", child.id()));
                        false
                    }
                });
                if children.is_empty() {
                    let mut command = Command::new(&install);
                    // The fake service's own errors are kept, so a test that
                    // never sees it answer can say why.
                    let stderr = fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&errors)
                        .map_or_else(|_| Stdio::null(), Stdio::from);
                    support::isolate(&mut command, errors.parent().unwrap_or(Path::new("/")));
                    command
                        .arg("serve")
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(stderr);
                    for (key, value) in &env {
                        command.env(key, value);
                    }
                    match command.spawn() {
                        Ok(child) => children.push(child),
                        Err(err) => note(&errors, &format!("serve did not start: {err}")),
                    }
                    std::thread::sleep(Duration::from_millis(300));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            for mut child in children {
                let _ = child.kill();
                let _ = child.wait();
            }
        });
        Self {
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

struct World {
    _tmp: TempDir,
    root: PathBuf,
    repo: PathBuf,
    install: PathBuf,
    state: PathBuf,
    work: PathBuf,
    app_home: PathBuf,
    log: PathBuf,
    base_sha: String,
    head_sha: String,
    _supervisor: Supervisor,
}

impl World {
    /// A repository with a base commit and one landed commit that changes
    /// `landed_path`, an "installed" v1 with its service up.
    fn new(landed_path: &str, new_version: &str) -> Self {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let repo = root.join("repo");
        fs::create_dir_all(repo.join("crates")).unwrap();
        fs::create_dir_all(repo.join("docs")).unwrap();
        git(&root, &["init", "-q", "-b", "integ", "repo"]);
        fs::write(repo.join("crates/x.rs"), "fn main() {}\n").unwrap();
        fs::write(repo.join("docs/plan.md"), "plan\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "base"]);
        let base_sha = git(&repo, &["rev-parse", "HEAD"]);
        fs::write(repo.join(landed_path), "changed\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "landed"]);
        let head_sha = git(&repo, &["rev-parse", "HEAD"]);

        let bin_dir = root.join("bin");
        let state_home = root.join("xdg-state");
        let state = shprd_server::state_dir(&state_home);
        let work = root.join("work");
        let app_home = root.join("app-home");
        let bad_repo = root.join("bad-repo");
        fs::create_dir_all(bad_repo.join(".shprd")).unwrap();
        for dir in [&bin_dir, &state, &work, &app_home, &root.join("fake-home")] {
            fs::create_dir_all(dir).unwrap();
        }
        let install = bin_dir.join("shprd");
        write_script(&install, &fake_binary("v1"));
        fs::write(root.join("shprd_serve.py"), FAKE_SERVICE).unwrap();
        write_script(&root.join("build.sh"), FAKE_BUILD);
        write_script(&root.join("new-binary"), &fake_binary(new_version));
        let log = root.join("log");
        fs::write(&log, "").unwrap();
        // What the running shprd was built from: the base commit.
        fs::write(
            work.join("installed.json"),
            format!("{{\"{}\":\"{base_sha}\"}}", repo.display()),
        )
        .unwrap();

        let env = vec![
            ("FAKE_LOG".to_string(), log.display().to_string()),
            (
                "FAKE_PY".to_string(),
                root.join("shprd_serve.py").display().to_string(),
            ),
            ("FAKE_STATE".to_string(), state.display().to_string()),
            (
                "FAKE_REAL".to_string(),
                env!("CARGO_BIN_EXE_shprd").to_string(),
            ),
            ("FAKE_BAD_REPO".to_string(), bad_repo.display().to_string()),
            (
                "FAKE_HOME".to_string(),
                root.join("fake-home").display().to_string(),
            ),
            (
                "XDG_STATE_HOME".to_string(),
                state_home.display().to_string(),
            ),
            (
                "XDG_CONFIG_HOME".to_string(),
                root.join("fake-home/.config").display().to_string(),
            ),
            (
                "XDG_DATA_HOME".to_string(),
                root.join("fake-home/.local/share").display().to_string(),
            ),
        ];
        let supervisor = Supervisor::start(install.clone(), env, root.join("serve.err"));
        let world = Self {
            _tmp: tmp,
            root,
            repo,
            install,
            state,
            work,
            app_home,
            log,
            base_sha,
            head_sha,
            _supervisor: supervisor,
        };
        world.wait_for_version("v1", Duration::from_secs(20));
        world
    }

    fn service_version(&self) -> Option<String> {
        let text = fs::read_to_string(self.state.join("service.json")).ok()?;
        let record: serde_json::Value = serde_json::from_str(&text).ok()?;
        let port = record.get("port")?.as_u64()?;
        let out = Command::new("curl")
            .args([
                "-s",
                "--max-time",
                "2",
                &format!("http://127.0.0.1:{port}/api/health"),
            ])
            .output()
            .ok()?;
        let body: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
        body.get("version")?.as_str().map(str::to_string)
    }

    fn wait_for_version(&self, version: &str, within: Duration) {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            if self.service_version().as_deref() == Some(version) {
                return;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        panic!(
            "the service never reported {version}; it reports {:?}\n{}",
            self.service_version(),
            self.diagnosis()
        );
    }

    /// What a test that never saw the service answer can say about why: the
    /// service record, the fake service's errors and curl's own answer.
    fn diagnosis(&self) -> String {
        let record = fs::read_to_string(self.state.join("service.json"))
            .unwrap_or_else(|err| format!("<no service.json: {err}>"));
        let errors = fs::read_to_string(self.root.join("serve.err")).unwrap_or_default();
        let tail: String = errors
            .lines()
            .rev()
            .take(20)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");
        let port = serde_json::from_str::<serde_json::Value>(&record)
            .ok()
            .and_then(|value| value.get("port").and_then(serde_json::Value::as_u64));
        let curl = port.map_or_else(String::new, |port| {
            Command::new("curl")
                .args([
                    "-sS",
                    "--max-time",
                    "2",
                    &format!("http://127.0.0.1:{port}/api/health"),
                ])
                .output()
                .map_or_else(
                    |err| format!("<curl did not run: {err}>"),
                    |out| {
                        format!(
                            "status {:?}, stdout {:?}, stderr {:?}",
                            out.status.code(),
                            String::from_utf8_lossy(&out.stdout),
                            String::from_utf8_lossy(&out.stderr)
                        )
                    },
                )
        });
        let python = Command::new("python3")
            .args(["-c", "import sys; print(sys.executable, sys.version)"])
            .output()
            .map_or_else(
                |err| format!("<python3 did not run: {err}>"),
                |out| String::from_utf8_lossy(&out.stdout).trim().to_string(),
            );
        format!("service.json: {record}\ncurl: {curl}\npython3: {python}\nserve errors:\n{tail}")
    }

    fn pid(&self) -> u64 {
        let text = fs::read_to_string(self.state.join("service.json")).unwrap();
        serde_json::from_str::<serde_json::Value>(&text).unwrap()["pid"]
            .as_u64()
            .unwrap()
    }

    /// Runs the helper once, as the service would start it.
    fn helper(&self, mode: &str, new_version: &str) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_shprd"));
        support::isolate(&mut command, &self.root);
        command
            .args(["self-update", "run", "--work-dir"])
            .arg(&self.work)
            .arg("--state-dir")
            .arg(&self.state)
            .arg("--repo")
            .arg(&self.repo)
            .args(["--base", "integ", "--local", "--install-path"])
            .arg(&self.install)
            .arg("--home-dir")
            .arg(self.root.join("home"))
            .arg("--app-home")
            .arg(&self.app_home)
            .args(["--health-timeout-secs", "8", "--no-news", "--"])
            .arg(self.root.join("build.sh"))
            .env("FAKE_LOG", &self.log)
            .env("FAKE_STATE", &self.state)
            .env("FAKE_WORK", &self.work)
            .env(
                "FAKE_REQUEST",
                shprd_server::self_update::Dirs::at(&self.work)
                    .request(&self.repo.display().to_string()),
            )
            .env("FAKE_REPO", &self.repo)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .env("BUILD_MODE", mode)
            .env("NEW_BINARY", self.root.join("new-binary"));
        let _ = new_version;
        command.output().unwrap()
    }

    /// Starts the run the repository's own log needs before it can hold a card.
    fn start_run(&self) {
        let mut command = Command::new(env!("CARGO_BIN_EXE_shprd"));
        support::isolate(&mut command, &self.root);
        let out = command
            .arg("--home")
            .arg(self.root.join("home"))
            .arg("--repo")
            .arg(&self.repo)
            .args([
                "run",
                "start",
                "--run-id",
                "t1",
                "--base",
                "integ",
                "--integration",
                "integ-m",
            ])
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
    }

    fn log_lines(&self, prefix: &str) -> Vec<String> {
        fs::read_to_string(&self.log)
            .unwrap()
            .lines()
            .filter(|line| line.starts_with(prefix))
            .map(str::to_string)
            .collect()
    }

    fn installed(&self) -> serde_json::Value {
        serde_json::from_str(&fs::read_to_string(self.work.join("installed.json")).unwrap())
            .unwrap()
    }

    fn status(&self) -> Option<serde_json::Value> {
        serde_json::from_str(&fs::read_to_string(self.work.join("status.json")).ok()?).ok()
    }

    fn binary_text(&self) -> String {
        fs::read_to_string(&self.install).unwrap()
    }
}

#[test]
fn a_landing_that_only_touches_docs_builds_nothing() {
    if !have_python() {
        return;
    }
    let world = World::new("docs/plan.md", "v2");
    let out = world.helper("ok", "v2");
    assert!(out.status.success(), "{out:?}");
    assert!(world.log_lines("build").is_empty(), "no build for docs");
    assert!(world.binary_text().contains("VERSION=v1"));
    assert!(!world.install.with_file_name("shprd.prev").exists());
    assert_eq!(world.service_version().as_deref(), Some("v1"));
}

#[test]
fn a_code_landing_builds_once_outside_the_repo_and_restarts_once() {
    if !have_python() {
        return;
    }
    let world = World::new("crates/x.rs", "v2");
    let before = world.pid();
    let out = world.helper("ok", "v2");
    assert!(out.status.success(), "{out:?}\n{}", world.diagnosis());
    let builds = world.log_lines("build");
    assert_eq!(builds.len(), 1, "{builds:?}");
    assert!(
        builds[0].contains(&format!("head={}", world.head_sha)),
        "the build is of the landed head: {builds:?}"
    );
    assert!(
        !builds[0].contains(&format!("pwd={}", world.repo.display())),
        "the build runs in its own worktree, not the checkout: {builds:?}"
    );
    assert!(
        world.binary_text().contains("VERSION=v2"),
        "the new binary is in place"
    );
    let prev = fs::read_to_string(world.install.with_file_name("shprd.prev")).unwrap();
    assert!(prev.contains("VERSION=v1"), "the previous binary is kept");
    assert_eq!(
        world.log_lines("install v2").len(),
        1,
        "the desktop is installed once"
    );
    world.wait_for_version("v2", Duration::from_secs(10));
    assert_ne!(world.pid(), before, "the service restarted");
    assert_eq!(
        world.installed()[world.repo.display().to_string()],
        world.head_sha.as_str()
    );
    assert_eq!(world.status().unwrap()["ok"], true);
    assert!(
        git(&world.repo, &["worktree", "list"]).lines().count() == 1,
        "the build worktree is gone"
    );
    // The same landing again: nothing to do.
    let again = world.helper("ok", "v2");
    assert!(again.status.success(), "{again:?}");
    assert_eq!(world.log_lines("build").len(), 1);
    let _ = &world.base_sha;
}

#[test]
fn a_landing_during_a_build_causes_exactly_one_more_build() {
    if !have_python() {
        return;
    }
    let world = World::new("crates/x.rs", "v2");
    let out = world.helper("land", "v2");
    assert!(out.status.success(), "{out:?}");
    let builds = world.log_lines("build");
    assert_eq!(
        builds.len(),
        2,
        "one build, then one more for what landed: {builds:?}"
    );
    let newest = git(&world.repo, &["rev-parse", "HEAD"]);
    assert_ne!(newest, world.head_sha);
    assert!(builds[1].contains(&format!("head={newest}")), "{builds:?}");
    assert_eq!(
        world.installed()[world.repo.display().to_string()],
        newest.as_str()
    );
}

#[test]
fn a_failed_build_leaves_the_binary_untouched() {
    if !have_python() {
        return;
    }
    let world = World::new("crates/x.rs", "v2");
    let before = world.pid();
    let out = world.helper("fail", "v2");
    assert!(
        out.status.success(),
        "a failed build is reported, not a crash: {out:?}"
    );
    assert_eq!(world.log_lines("build").len(), 1);
    assert!(world.binary_text().contains("VERSION=v1"));
    assert!(!world.install.with_file_name("shprd.prev").exists());
    assert_eq!(world.pid(), before, "the service was not restarted");
    assert_eq!(
        world.installed()[world.repo.display().to_string()],
        world.base_sha.as_str()
    );
    let status = world.status().unwrap();
    assert_eq!(status["ok"], false);
    let line = status["line"].as_str().unwrap();
    assert!(line.contains("still running the version it had"), "{line}");
    assert!(line.contains("mismatched types"), "{line}");
}

#[test]
fn a_new_binary_that_fails_its_health_check_is_rolled_back() {
    if !have_python() {
        return;
    }
    let world = World::new("crates/x.rs", "bad");
    let out = world.helper("ok", "bad");
    assert!(
        out.status.success(),
        "a rollback is reported, not a crash: {out:?}"
    );
    assert!(
        world.binary_text().contains("VERSION=v1"),
        "the previous binary is back: {}",
        world.binary_text()
    );
    world.wait_for_version("v1", Duration::from_secs(15));
    assert_eq!(
        world.installed()[world.repo.display().to_string()],
        world.base_sha.as_str()
    );
    let status = world.status().unwrap();
    assert_eq!(status["ok"], false);
    assert!(
        status["line"]
            .as_str()
            .unwrap()
            .contains("went back to the version it had"),
        "{status}"
    );
    assert!(
        world.log_lines("install v1").len() == 1,
        "the old desktop is put back too"
    );
}

#[test]
fn a_new_binary_that_cannot_fold_a_repository_log_is_rolled_back() {
    if !have_python() {
        return;
    }
    // The "unfoldable" build is the real shprd serving a repository whose log it
    // cannot fold; its own /api/health/repos says so.
    let world = World::new("crates/x.rs", "unfoldable");
    let out = world.helper("ok", "unfoldable");
    assert!(out.status.success(), "a rollback is reported: {out:?}");
    assert!(
        world.binary_text().contains("VERSION=v1"),
        "the previous binary is back: {}",
        world.binary_text()
    );
    world.wait_for_version("v1", Duration::from_secs(15));
    let status = world.status().unwrap();
    assert_eq!(status["ok"], false);
    let line = status["line"].as_str().unwrap();
    assert!(line.contains("went back to the version it had"), "{line}");
    assert!(line.contains("cannot read a repository's log"), "{line}");
}

/// The cards a repository's own log draws, as the chat and board show them.
fn open_cards(repo: &Path) -> Vec<String> {
    let state = ctx_state(repo);
    state
        .escalations
        .iter()
        .filter_map(|escalation| shprd_views::cards::card(&state, escalation))
        .map(|card| card.body)
        .collect()
}

#[test]
fn a_failed_update_card_clears_once_a_later_update_succeeds() {
    if !have_python() {
        return;
    }
    let world = World::new("crates/x.rs", "v2");
    world.start_run();
    let out = world.helper("fail", "v2");
    assert!(out.status.success(), "{out:?}");
    // The card the real failed build raised is one the settle logic recognises.
    let state = ctx_state(&world.repo);
    assert!(
        shprd_core::gate::has_open_update_failure(&state, &world.repo.display().to_string()),
        "{:?}",
        state.escalations
    );
    assert_eq!(open_cards(&world.repo).len(), 1, "one card for the failure");
    // A later update succeeds: the card leaves.
    let ok = world.helper("ok", "v2");
    assert!(ok.status.success(), "{ok:?}");
    assert!(world.binary_text().contains("VERSION=v2"));
    assert!(open_cards(&world.repo).is_empty(), "the card is settled");
    // A success with nothing to settle records nothing more.
    let before = ctx_state(&world.repo).escalations.len();
    let again = world.helper("ok", "v2");
    assert!(again.status.success(), "{again:?}");
    assert_eq!(ctx_state(&world.repo).escalations.len(), before);
}

#[test]
fn a_failure_after_a_success_stays_open() {
    if !have_python() {
        return;
    }
    let world = World::new("crates/x.rs", "v2");
    world.start_run();
    world.helper("fail", "v2");
    world.helper("ok", "v2");
    assert!(open_cards(&world.repo).is_empty());
    // A new landing whose build fails raises a card newer than the success.
    fs::write(world.repo.join("crates/y.rs"), "fn y() {}\n").unwrap();
    git(&world.repo, &["add", "."]);
    git(&world.repo, &["commit", "-qm", "later landing"]);
    world.helper("fail", "v3");
    assert_eq!(open_cards(&world.repo).len(), 1, "the new failure is open");
}

fn ctx_state(repo: &Path) -> shprd_core::state::RunState {
    let log = repo.join(".shprd").join("events.jsonl");
    let read = shprd_core::log::EventLog::read_all(&log).unwrap();
    shprd_core::state::RunState::fold(&read.events).unwrap()
}

#[test]
fn another_repositorys_success_leaves_a_failure_open() {
    if !have_python() {
        return;
    }
    let (a, b) = (
        World::new("crates/x.rs", "v2"),
        World::new("crates/x.rs", "v2"),
    );
    a.start_run();
    b.start_run();
    b.helper("fail", "v2");
    a.helper("ok", "v2");
    assert!(a.binary_text().contains("VERSION=v2"));
    assert_eq!(open_cards(&b.repo).len(), 1, "B's failure is still open");
    assert!(shprd_core::gate::has_open_update_failure(
        &ctx_state(&b.repo),
        &b.repo.display().to_string()
    ));
    // A's own log never held B's failure, and a success there records nothing.
    assert!(open_cards(&a.repo).is_empty());
}
