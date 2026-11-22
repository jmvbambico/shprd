//! `shprd self-update run`: the helper that applies an accepted change to shprd.
//!
//! The service starts this (detached, so it outlives the service it restarts)
//! when a task lands and `tasks.self_update` is on. It is hidden: nobody types
//! it. All the sequencing is `shprd_server::self_update::Pipeline`'s; this file
//! is the machine's half of it ([`Machine`]): git, the build, the file swap, the
//! desktop install, the pid-recorded stop and the health check.
//!
//! Every path is an explicit argument and none has a default, so a missing one is
//! a usage error rather than the person's real install. This module reads no
//! settings and resolves no home of its own; the one environment variable it
//! reads is `CARGO_HOME`/`HOME`, to find rustup's `cargo`, at the edge.

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::anyhow;
use shprd_core::event::{Actor, EventKind};
use shprd_git::git::Git;
use shprd_server::ServiceRecord;
use shprd_server::self_update::{Dirs, Job, Outcome, Pipeline, Steps};

use crate::cli::SelfUpdateArgs;
use crate::ctx::{CliError, Ctx};

/// How long the old service has to exit after it is asked to.
const STOP_WAIT: Duration = Duration::from_secs(20);

/// Runs the pipeline for one repository and prints one line per pass.
pub fn run(args: &SelfUpdateArgs) -> Result<(), CliError> {
    let machine = Machine::new(args);
    let dirs = Dirs::at(&args.work_dir);
    let repo = args.repo.to_string_lossy().into_owned();
    let outcomes = Pipeline {
        dirs: &dirs,
        steps: &machine,
        repo: &repo,
    }
    .run();
    let mut failed = None;
    for outcome in &outcomes {
        println!("{}", describe(outcome));
        if let Outcome::Stuck(why) = outcome {
            failed = Some(why.clone());
        }
    }
    match failed {
        Some(why) => Err(CliError::other(anyhow!("self-update is stuck: {why}"))),
        None => Ok(()),
    }
}

/// One plain line for an outcome.
fn describe(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Busy => "another update is running; it will see this change".into(),
        Outcome::UpToDate => "shprd is already built from the accepted change".into(),
        Outcome::NoCodeChange => "the accepted change touches nothing shprd is built from".into(),
        Outcome::Unknown(why) => format!("could not tell what to build: {why}"),
        Outcome::BuildFailed(why) => format!("the build failed; nothing changed: {why}"),
        Outcome::Updated { head, version } => format!("updated to {version} at {head}"),
        Outcome::RolledBack(why) => format!("rolled back to the previous shprd: {why}"),
        Outcome::Stuck(why) => format!("stuck: {why}"),
    }
}

/// The detached start of the helper for `job`, as the service runs it.
///
/// `install_path` is the running binary's own file, captured once at start.
/// It refuses a binary inside a cargo `target/` folder: that is a developer's
/// build, and swapping it would rewrite their build output.
pub fn spawn_for(
    install_path: &Path,
    home: &Path,
    state_dir: &Path,
    job: &Job,
) -> Result<(), String> {
    use std::os::unix::process::CommandExt;

    if install_path
        .components()
        .any(|part| part.as_os_str() == "target")
    {
        return Err(format!(
            "{} is a build output, not an install, so it is not updated",
            install_path.display()
        ));
    }
    let dirs = Dirs::under_home(home);
    fs::create_dir_all(dirs.root()).map_err(|err| err.to_string())?;
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dirs.root().join("helper.log"))
        .map_err(|err| err.to_string())?;
    let err_log = log.try_clone().map_err(|err| err.to_string())?;
    let mut command = Command::new(install_path);
    command
        .args(["self-update", "run", "--work-dir"])
        .arg(dirs.root())
        .arg("--state-dir")
        .arg(state_dir)
        .arg("--repo")
        .arg(&job.repo)
        .args(["--base", &job.base])
        .arg("--install-path")
        .arg(install_path)
        .arg("--home-dir")
        .arg(home);
    if job.local {
        command.arg("--local");
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(err_log))
        // Its own process group, so the service's stop (and the supervisor that
        // restarts it) does not take the helper with it.
        .process_group(0)
        .spawn()
        .map(|_| ())
        .map_err(|err| format!("{}: {err}", install_path.display()))
}

/// The machine, as the pipeline drives it.
struct Machine<'a> {
    args: &'a SelfUpdateArgs,
    /// The id of the service that was running before the restart, so the health
    /// check knows the answer comes from a new one.
    old_id: RefCell<Option<String>>,
}

impl<'a> Machine<'a> {
    fn new(args: &'a SelfUpdateArgs) -> Self {
        Self {
            args,
            old_id: RefCell::new(None),
        }
    }

    fn work(&self) -> &Path {
        &self.args.work_dir
    }

    fn worktree(&self) -> PathBuf {
        self.work().join("wt")
    }

    fn target(&self) -> PathBuf {
        self.work().join("target")
    }

    fn sibling(&self, suffix: &str) -> PathBuf {
        let mut name = self
            .args
            .install_path
            .file_name()
            .map(|name| name.to_os_string())
            .unwrap_or_default();
        name.push(suffix);
        self.args.install_path.with_file_name(name)
    }

    fn record(&self) -> Option<ServiceRecord> {
        let text = fs::read_to_string(self.args.state_dir.join("service.json")).ok()?;
        serde_json::from_str(&text).ok()
    }

    fn repo_git(&self) -> Git {
        Git::new(&self.args.repo)
    }

    /// Removes the build worktree, if there is one.
    fn clear_worktree(&self) {
        let wt = self.worktree();
        if wt.exists() {
            let _ = self
                .repo_git()
                .run(&["worktree", "remove", "--force", &wt.to_string_lossy()]);
            let _ = fs::remove_dir_all(&wt);
        }
        let _ = self.repo_git().run(&["worktree", "prune"]);
    }

    /// The command that builds shprd: the override the caller gave, else rustup's
    /// own `cargo` (never whichever `cargo` is first on `PATH`: Homebrew's ignores
    /// `rust-toolchain.toml`).
    fn build_command(&self) -> Vec<String> {
        if !self.args.build.is_empty() {
            return self.args.build.clone();
        }
        let cargo_home = std::env::var_os("CARGO_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")));
        let rustup_cargo = cargo_home
            .map(|dir| dir.join("bin").join("cargo"))
            .filter(|path| path.is_file());
        let program = rustup_cargo.map_or_else(
            || String::from("cargo"),
            |path| path.to_string_lossy().into_owned(),
        );
        vec![
            program,
            "build".into(),
            "--release".into(),
            "-p".into(),
            "shprd".into(),
        ]
    }
}

/// The last lines of a command's output, as one short reason.
fn tail(stderr: &[u8], stdout: &[u8]) -> String {
    let mut text = String::from_utf8_lossy(stderr).into_owned();
    if text.trim().is_empty() {
        text = String::from_utf8_lossy(stdout).into_owned();
    }
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let from = lines.len().saturating_sub(6);
    let joined = lines[from..].join(" / ");
    if joined.chars().count() > 500 {
        let cut: String = joined.chars().take(500).collect();
        format!("{cut}…")
    } else {
        joined
    }
}

fn alive(pid: u32) -> bool {
    Command::new("/bin/kill")
        .args(["-0", "--", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Whether `pid` still looks like a shprd service: a recorded pid can outlive
/// its process and be handed to another, and that one is not ours to stop.
fn looks_like_service(pid: u32) -> bool {
    Command::new("/bin/ps")
        .args(["-o", "command=", "-p", &pid.to_string()])
        .stdin(Stdio::null())
        .output()
        .is_ok_and(|output| {
            let line = String::from_utf8_lossy(&output.stdout);
            line.contains("shprd") && line.contains("serve")
        })
}

fn signal(name: &str, pid: u32) {
    let _ = Command::new("/bin/kill")
        .args([name, "--", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// `GET /api/health` on `port`, and the version it reports when it says ok.
fn health(port: u16) -> Option<String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(3)))
        .http_status_as_error(false)
        .build()
        .into();
    let mut response = agent
        .get(&format!("http://127.0.0.1:{port}/api/health"))
        .call()
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let text = response.body_mut().read_to_string().ok()?;
    let body: serde_json::Value = serde_json::from_str(&text).ok()?;
    body.get("ok")
        .and_then(serde_json::Value::as_bool)
        .filter(|ok| *ok)?;
    body.get("version")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

impl Machine<'_> {
    /// Waits for a service other than the one that was asked to stop to answer.
    fn wait_for_new_service(&self) -> Result<String, String> {
        let old = self.old_id.borrow().clone();
        let deadline = Instant::now() + Duration::from_secs(self.args.health_timeout_secs);
        loop {
            if let Some(record) = self.record()
                && old.as_deref() != Some(record.id.as_str())
                && alive(record.pid)
                && let Some(version) = health(record.port)
            {
                return Ok(version);
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "no healthy service answered within {} seconds",
                    self.args.health_timeout_secs
                ));
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
}

impl Steps for Machine<'_> {
    fn head(&self) -> Result<String, String> {
        let git = self.repo_git();
        let base = self.args.base.as_str();
        if !self.args.local {
            // The forge merged it on the remote: look at the remote's branch.
            let _ = git.run(&["fetch", "--quiet", "origin", base]);
            if let Ok(sha) = git.rev_parse(&format!("refs/remotes/origin/{base}")) {
                return Ok(sha);
            }
        }
        git.rev_parse(&format!("refs/heads/{base}"))
            .map_err(|err| format!("cannot read {base}: {err}"))
    }

    fn baseline(&self, head: &str) -> Option<String> {
        let modified = fs::metadata(&self.args.install_path)
            .ok()?
            .modified()
            .ok()?;
        let secs = modified
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs();
        let found = self
            .repo_git()
            .run(&["rev-list", "-1", &format!("--before={secs}"), head])
            .ok()?;
        let found = found.trim();
        (!found.is_empty()).then(|| found.to_string())
    }

    fn changed(&self, from: &str, to: &str) -> Result<Vec<String>, String> {
        shprd_git::changed::changed_paths(&self.args.repo, from, to).map_err(|err| err.to_string())
    }

    fn build(&self, head: &str) -> Result<PathBuf, String> {
        self.clear_worktree();
        fs::create_dir_all(self.work()).map_err(|err| err.to_string())?;
        let wt = self.worktree();
        self.repo_git()
            .run(&["worktree", "add", "--detach", &wt.to_string_lossy(), head])
            .map_err(|err| format!("could not check out {head}: {err}"))?;
        let command = self.build_command();
        let Some((program, rest)) = command.split_first() else {
            return Err(String::from("no build command"));
        };
        let result = Command::new(program)
            .args(rest)
            .current_dir(&wt)
            .env("CARGO_TARGET_DIR", self.target())
            .stdin(Stdio::null())
            .output();
        self.clear_worktree();
        let output = result.map_err(|err| format!("{program}: {err}"))?;
        if !output.status.success() {
            return Err(tail(&output.stderr, &output.stdout));
        }
        let built = self.target().join("release").join("shprd");
        if built.is_file() {
            Ok(built)
        } else {
            Err(format!("the build left no binary at {}", built.display()))
        }
    }

    fn swap(&self, built: &Path) -> Result<(), String> {
        let live = &self.args.install_path;
        let fresh = self.sibling(".new");
        let prev = self.sibling(".prev");
        fs::copy(built, &fresh).map_err(|err| format!("{}: {err}", fresh.display()))?;
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&fresh, fs::Permissions::from_mode(0o755))
                .map_err(|err| format!("{}: {err}", fresh.display()))?;
        }
        if live.exists() {
            fs::rename(live, &prev).map_err(|err| format!("{}: {err}", prev.display()))?;
        }
        if let Err(err) = fs::rename(&fresh, live) {
            // The old one goes straight back: the live path is never left empty.
            if prev.exists() {
                let _ = fs::rename(&prev, live);
            }
            return Err(format!("{}: {err}", live.display()));
        }
        Ok(())
    }

    fn install_desktop(&self) -> Result<(), String> {
        let mut command = Command::new(&self.args.install_path);
        command.arg("--home").arg(&self.args.home_dir).args([
            "install",
            "herdr-desktop",
            "--allow-agent",
        ]);
        if let Some(app_home) = &self.args.app_home {
            command.arg("--app-home").arg(app_home);
        }
        // The new binary installs the desktop it carries, not a tree an
        // environment variable names.
        let output = command
            .env_remove("SHPRD_HOST_DIR")
            .stdin(Stdio::null())
            .output()
            .map_err(|err| format!("{}: {err}", self.args.install_path.display()))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(format!(
                "the desktop install failed: {}",
                tail(&output.stderr, &output.stdout)
            ))
        }
    }

    fn say(&self, line: &str) {
        println!("{line}");
        if self.args.no_news {
            return;
        }
        let ctx = Ctx::new(self.args.repo.clone(), Actor::Engine, true, true)
            .with_home(Some(self.args.home_dir.clone()));
        let Ok(state) = ctx.state() else {
            return;
        };
        let (_, host) = crate::host::select();
        let news = crate::cmd::live_news::LiveNews::new(host, &self.args.home_dir, &self.args.repo);
        use shprd_server::Coordinator;
        if let Some(pane) = news.pane(&state)
            && let Err(why) = news.tell(&pane, line)
        {
            eprintln!("the coordinator was not told: {why}");
        }
    }

    fn escalate(&self, body: &str) {
        let ctx = Ctx::new(self.args.repo.clone(), Actor::Engine, true, true)
            .with_home(Some(self.args.home_dir.clone()));
        let kind = EventKind::Escalated {
            task: None,
            body: body.to_string(),
            default_action: String::from("nothing needs doing; shprd is as it was"),
        };
        if let Err(why) = ctx.commit(kind) {
            eprintln!("the person was not told: {why}");
        }
    }

    fn restart(&self) -> Result<(), String> {
        let Some(record) = self.record() else {
            // No service is running, so nothing stops; whatever starts one gets
            // the new binary.
            return Ok(());
        };
        *self.old_id.borrow_mut() = Some(record.id.clone());
        if !alive(record.pid) {
            return Ok(());
        }
        if !looks_like_service(record.pid) {
            return Err(format!(
                "pid {} no longer looks like shprd, so it was not stopped",
                record.pid
            ));
        }
        signal("-TERM", record.pid);
        let deadline = Instant::now() + STOP_WAIT;
        while alive(record.pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        if alive(record.pid) {
            // It was asked, politely, and did not go: the same pid is stopped.
            signal("-KILL", record.pid);
            std::thread::sleep(Duration::from_millis(300));
        }
        if alive(record.pid) {
            return Err(format!(
                "the running service (pid {}) did not stop",
                record.pid
            ));
        }
        Ok(())
    }

    fn healthy(&self) -> Result<String, String> {
        self.wait_for_new_service()
    }

    fn rollback(&self) -> Result<(), String> {
        let live = &self.args.install_path;
        let prev = self.sibling(".prev");
        let bad = self.sibling(".bad");
        if !prev.exists() {
            return Err(format!("there is no {} to go back to", prev.display()));
        }
        // Whatever service is up now runs the bad binary: it is stopped by its
        // recorded pid first, then the old file goes back.
        self.restart()?;
        if live.exists() {
            fs::rename(live, &bad).map_err(|err| format!("{}: {err}", bad.display()))?;
        }
        fs::rename(&prev, live).map_err(|err| format!("{}: {err}", live.display()))?;
        let _ = fs::remove_file(&bad);
        let installed = self.install_desktop();
        let up = self.wait_for_new_service();
        installed?;
        up.map(|_| ())
    }
}
