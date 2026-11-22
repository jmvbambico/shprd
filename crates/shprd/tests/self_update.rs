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
import json, os, sys, uuid
from http.server import BaseHTTPRequestHandler, HTTPServer

state, version = sys.argv[1], sys.argv[2]
ident = uuid.uuid4().hex

class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        body = json.dumps({"ok": True, "id": ident, "version": version}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass

server = HTTPServer(("127.0.0.1", 0), Handler)
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

/// Starts `<install> serve` whenever none is running, as the layout's footer does.
struct Supervisor {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Supervisor {
    fn start(install: PathBuf, env: Vec<(String, String)>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let handle = std::thread::spawn(move || {
            let mut children: Vec<Child> = Vec::new();
            while !flag.load(Ordering::SeqCst) {
                // Reaping is what lets the helper see the old service as gone.
                children.retain_mut(|child| matches!(child.try_wait(), Ok(None)));
                if children.is_empty() {
                    let mut command = Command::new(&install);
                    command
                        .arg("serve")
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null());
                    for (key, value) in &env {
                        command.env(key, value);
                    }
                    if let Ok(child) = command.spawn() {
                        children.push(child);
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
        let state = root.join("state");
        let work = root.join("work");
        let app_home = root.join("app-home");
        for dir in [&bin_dir, &state, &work, &app_home] {
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
        ];
        let supervisor = Supervisor::start(install.clone(), env);
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
            "the service never reported {version}; it reports {:?}",
            self.service_version()
        );
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
    assert!(out.status.success(), "{out:?}");
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
