//! Running a repository's gate commands in a worktree.
//!
//! The commands are strings the repository's own config defines, so each one
//! runs through `/bin/sh -c` in the worktree: they are shell by nature (`&&`,
//! pipes). Everything else is the care a probe gets too — a step leads its own
//! process group, so a timeout can take down the children it started, and only
//! the tail of the output is kept, because a failing build prints megabytes and
//! the tail is what the event can carry.

use std::fs::{File, OpenOptions, remove_file};
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use shprd_core::event::{GATE_OUTPUT_TAIL_CHARS, GateStep};

/// The shell every gate command runs in.
const SHELL: &str = "/bin/sh";

/// How often a running step is asked whether it has exited.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// How many lines of a step's output are kept.
const TAIL_LINES: usize = 40;

/// How many bytes of a step's capture file are read back.
const TAIL_BYTES: u64 = 64 * 1024;

/// What running a gate's commands produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateRun {
    /// True when every command that ran passed.
    pub passed: bool,
    /// The commands that ran, in order, stopping at the first failure.
    pub steps: Vec<GateStep>,
}

/// Runs `commands` in `worktree`, in order, stopping at the first failure.
///
/// Each command is run with `/bin/sh -c` and `timeout_per_step` to finish. A
/// step that takes longer is killed, with the whole process group it leads, and
/// counts as a failure: its `exit_code` is `None`. `env` starts from this
/// process's environment, where a [`Some`] entry sets a variable and a [`None`]
/// entry removes one.
pub fn run_gate(
    worktree: &Path,
    commands: &[String],
    timeout_per_step: Duration,
    env: &[(String, Option<String>)],
) -> GateRun {
    let mut steps = Vec::new();
    let mut passed = true;
    for command in commands {
        let step = run_step(worktree, command, timeout_per_step, env);
        let ok = step.exit_code == Some(0);
        steps.push(step);
        if !ok {
            passed = false;
            break;
        }
    }
    GateRun { passed, steps }
}

/// Runs one command, and captures the tail of what it printed.
fn run_step(
    worktree: &Path,
    command: &str,
    timeout: Duration,
    env: &[(String, Option<String>)],
) -> GateStep {
    let started = Instant::now();
    let capture = Capture::new();
    let mut shell = Command::new(SHELL);
    shell
        .arg("-c")
        .arg(command)
        .current_dir(worktree)
        .stdin(Stdio::null());
    match capture.streams() {
        Some((out, err)) => {
            shell.stdout(Stdio::from(out)).stderr(Stdio::from(err));
        }
        None => {
            shell.stdout(Stdio::null()).stderr(Stdio::null());
        }
    }
    for (key, value) in env {
        match value {
            Some(value) => shell.env(key, value),
            None => shell.env_remove(key),
        };
    }
    // The step leads its own process group, so a timeout can take down the
    // children it started as well as the shell that started them.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        shell.process_group(0);
    }

    let mut child = match shell.spawn() {
        Ok(child) => child,
        Err(source) => {
            return GateStep {
                command: command.to_string(),
                exit_code: None,
                duration_ms: 0,
                output_tail: format!("could not start {SHELL}: {source}"),
            };
        }
    };
    let deadline = started + timeout;
    let exit_code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) => {}
            Err(_) => {
                kill(&mut child);
                break None;
            }
        }
        if Instant::now() >= deadline {
            kill(&mut child);
            break None;
        }
        thread::sleep(POLL_INTERVAL);
    };
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let output_tail = capture.tail();
    GateStep {
        command: command.to_string(),
        exit_code,
        duration_ms,
        output_tail,
    }
}

/// The file a step's combined stdout and stderr is written to.
///
/// A file rather than a pipe: nothing has to drain it while the step runs, so a
/// step that prints far more than a pipe buffer holds cannot deadlock the gate.
/// The name carries the process id and a counter, so concurrent gates in one
/// process never share one.
#[derive(Debug)]
struct Capture {
    /// Where the output goes, when a file could be made.
    path: Option<PathBuf>,
    /// The open file both streams are duplicated from.
    file: Option<File>,
}

impl Capture {
    /// Creates the capture file for one step.
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let name = format!(
            "shprd-gate-{}-{}.log",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let path = std::env::temp_dir().join(name);
        match OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&path)
        {
            Ok(file) => Self {
                path: Some(path),
                file: Some(file),
            },
            Err(_) => Self {
                path: None,
                file: None,
            },
        }
    }

    /// Two write handles for the child's stdout and stderr, or `None`.
    ///
    /// A duplication that fails drops both streams rather than half the output:
    /// no gate verdict depends on the tail, so losing it is safe.
    fn streams(&self) -> Option<(File, File)> {
        let file = self.file.as_ref()?;
        let out = file.try_clone().ok()?;
        let err = file.try_clone().ok()?;
        Some((out, err))
    }

    /// The last [`TAIL_LINES`] lines, capped at [`GATE_OUTPUT_TAIL_CHARS`].
    ///
    /// The file is read, never waited on, so a grandchild that inherited the
    /// descriptor and stays alive cannot hold the gate open.
    fn tail(&self) -> String {
        let Some(path) = &self.path else {
            return String::new();
        };
        let text = read_back(path);
        let _ = remove_file(path);
        text
    }
}

/// Reads the tail of `path` back as text, without following a file that grew.
fn read_back(path: &Path) -> String {
    let Ok(mut file) = File::open(path) else {
        return String::new();
    };
    let len = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    if len > TAIL_BYTES && file.seek(SeekFrom::Start(len - TAIL_BYTES)).is_err() {
        return String::new();
    }
    let mut buf = Vec::new();
    if file.read_to_end(&mut buf).is_err() {
        return String::new();
    }
    let text = String::from_utf8_lossy(&buf).to_string();
    tail_lines(&text)
}

/// The last [`TAIL_LINES`] lines of `text`, capped at [`GATE_OUTPUT_TAIL_CHARS`].
fn tail_lines(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(TAIL_LINES);
    let joined = lines[start..].join("\n");
    let count = joined.chars().count();
    if count > GATE_OUTPUT_TAIL_CHARS {
        return joined
            .chars()
            .skip(count - GATE_OUTPUT_TAIL_CHARS)
            .collect();
    }
    joined
}

/// Kills a step that timed out or failed, and every process it started, then
/// reaps it.
fn kill(child: &mut Child) {
    // A group kill that fails is not fatal: the direct child kill and the reap
    // below always run, so at worst the step's grandchildren outlive it.
    #[cfg(unix)]
    let _group_signalled = kill_group(child.id());
    let _ = child.kill();
    let _ = child.wait();
}

/// The absolute path of the system `kill`, when one of its standard locations
/// exists. It is never resolved through `PATH`.
#[cfg(unix)]
fn kill_program() -> Option<&'static Path> {
    ["/bin/kill", "/usr/bin/kill"]
        .into_iter()
        .map(Path::new)
        .find(|path| path.is_file())
}

/// Kill the process group led by `leader`, ignoring a group that is already
/// gone.
///
/// The target is `--`-separated from the signal because procps-ng `kill` on
/// Linux parses a leading `-` as an option and never sees the negative
/// process-group id. Returns whether `kill` ran and reported success.
#[cfg(unix)]
fn kill_group(leader: u32) -> bool {
    let Some(program) = kill_program() else {
        return false;
    };
    let group = format!("-{leader}");
    Command::new(program)
        .args(["-KILL", "--", group.as_str()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The commands a test runs, as config strings.
    fn commands(list: &[&str]) -> Vec<String> {
        list.iter().map(|item| item.to_string()).collect()
    }

    /// A directory that stands in for a worktree.
    fn worktree() -> tempfile::TempDir {
        match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(err) => panic!("temp dir failed: {err}"),
        }
    }

    #[test]
    fn every_passing_command_runs_in_order() {
        let dir = worktree();
        let run = run_gate(
            dir.path(),
            &commands(&["true", "echo hi"]),
            Duration::from_secs(10),
            &[],
        );
        assert!(run.passed, "{run:?}");
        assert_eq!(run.steps.len(), 2);
        assert_eq!(run.steps[0].command, "true");
        assert_eq!(run.steps[0].exit_code, Some(0));
        assert_eq!(run.steps[1].exit_code, Some(0));
        assert!(
            run.steps[1].output_tail.contains("hi"),
            "the tail keeps what the step printed: {run:?}"
        );
    }

    #[test]
    fn the_first_failure_stops_the_gate() {
        let dir = worktree();
        let run = run_gate(
            dir.path(),
            &commands(&["true", "false", "echo unreachable"]),
            Duration::from_secs(10),
            &[],
        );
        assert!(!run.passed);
        assert_eq!(run.steps.len(), 2, "the step after the failure never runs");
        assert_eq!(run.steps[1].exit_code, Some(1));
    }

    #[test]
    fn a_step_that_outruns_its_timeout_is_killed_and_fails() {
        let dir = worktree();
        let run = run_gate(
            dir.path(),
            &commands(&["sleep 5"]),
            Duration::from_millis(300),
            &[],
        );
        assert!(!run.passed);
        assert_eq!(run.steps.len(), 1);
        assert_eq!(
            run.steps[0].exit_code, None,
            "a killed step has no exit code"
        );
        assert!(
            run.steps[0].duration_ms < 3_000,
            "the gate must not wait for the sleep: {run:?}"
        );
    }

    #[test]
    fn commands_run_in_the_worktree_with_the_given_env() {
        let dir = worktree();
        if let Err(err) = std::fs::write(dir.path().join("marker"), "x") {
            panic!("write failed: {err}");
        }
        let env = vec![
            ("SHPRD_GATE_TEST".to_string(), Some("yes".to_string())),
            ("SHPRD_GATE_GONE".to_string(), None),
        ];
        let run = run_gate(
            dir.path(),
            &commands(&[
                "test -f marker",
                "test \"$SHPRD_GATE_TEST\" = yes",
                "test -z \"$SHPRD_GATE_GONE\"",
            ]),
            Duration::from_secs(10),
            &env,
        );
        assert!(run.passed, "{run:?}");
    }

    #[test]
    fn the_tail_keeps_the_last_forty_lines() {
        let mut text = String::new();
        for number in 1..=100 {
            text.push_str(&format!("line {number}\n"));
        }
        let tail = tail_lines(&text);
        assert_eq!(tail.lines().count(), TAIL_LINES);
        assert!(tail.starts_with("line 61"), "{tail}");
        assert!(tail.ends_with("line 100"), "{tail}");
    }

    #[test]
    fn the_tail_is_capped_in_characters() {
        let huge = "x".repeat(10_000);
        let tail = tail_lines(&huge);
        assert_eq!(tail.chars().count(), GATE_OUTPUT_TAIL_CHARS);
    }
}
