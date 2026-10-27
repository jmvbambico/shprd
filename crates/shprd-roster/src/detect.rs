//! Finding the agent CLIs on `PATH`, and probing their version and sign-in.
//!
//! A detection never trusts its own output: the version is a token scraped out
//! of `stdout`/`stderr`, and a probe's exit status is reduced to signed in,
//! signed out or unknown. Probe output is never logged, and no failure message
//! carries it, so a CLI that prints a token cannot leak it into a log.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::spec::{HarnessSpec, Values, Vendor, render_args};

/// How long the version probe may run.
const VERSION_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the sign-in probe may run.
const SIGNIN_TIMEOUT: Duration = Duration::from_secs(10);

/// How often a running probe is asked whether it has exited.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The placeholder a sign-in probe may use for the model it depends on.
const MODEL_PLACEHOLDER: &str = "{model}";

/// Why a probe command produced no exit status.
///
/// Probe output is deliberately absent: a sign-in command may print a
/// credential, so a message carries the program name, never what it said.
#[derive(Debug, thiserror::Error)]
pub enum DetectError {
    /// The program could not be started.
    #[error("failed to run {program}: {source}")]
    Spawn {
        /// The program that failed to start.
        program: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// The program was still running at the timeout and was killed.
    #[error("{program} did not finish within {timeout:?}")]
    Timeout {
        /// The program that timed out.
        program: PathBuf,
        /// The timeout it exceeded.
        timeout: Duration,
    },
    /// Waiting on the program failed.
    #[error("failed to wait for {program}: {source}")]
    Wait {
        /// The program that was being waited on.
        program: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },
}

/// Whether a CLI believes the user is signed in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignIn {
    /// The probe exited 0.
    SignedIn,
    /// The probe exited non-zero.
    SignedOut,
    /// The CLI cannot be asked, so the answer is unknown and the reason says why.
    Unknown(String),
}

/// What one harness lookup found on this machine.
#[derive(Debug, Clone)]
pub struct Detection {
    /// The spec id, e.g. `claude`.
    pub id: String,
    /// The human-readable name, e.g. `Claude Code`.
    pub name: String,
    /// The vendor that ships the CLI.
    pub vendor: Vendor,
    /// The resolved binary, or [`None`] when it is not installed.
    pub binary: Option<PathBuf>,
    /// The CLI version, when one could be scraped out of its output.
    pub version: Option<String>,
    /// Whether the user is signed in.
    pub signin: SignIn,
}

/// The spec name of a vendor, as it appears in the TOML files.
pub fn vendor_name(vendor: Vendor) -> &'static str {
    match vendor {
        Vendor::Anthropic => "anthropic",
        Vendor::Openai => "openai",
        Vendor::Opencode => "opencode",
        Vendor::Kilo => "kilo",
        Vendor::Pi => "pi",
    }
}

/// The first executable file called `name` in the `:`-separated `path_env`.
///
/// An empty entry means the current directory, as it does for the shell.
pub fn find_binary(name: &str, path_env: &str) -> Option<PathBuf> {
    std::env::split_paths(path_env)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
}

/// Whether `path` is a regular file this user may execute.
fn is_executable(path: &Path) -> bool {
    let Ok(meta) = path.metadata() else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Runs `program` with `args` and a timeout, and returns what it printed.
///
/// `env` starts from the current process environment: a [`Some`] entry sets a
/// variable and a [`None`] entry removes one. The child's stdin is `/dev/null`
/// so a CLI that asks a question fails instead of hanging. At `timeout` the
/// probe and the whole process group it leads are killed, so a CLI that started
/// children of its own leaves nothing running. Output is captured for the
/// caller and never logged.
pub fn run_with_timeout(
    program: &Path,
    args: &[String],
    env: &[(String, Option<String>)],
    timeout: Duration,
) -> Result<Output, DetectError> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in env {
        match value {
            Some(value) => command.env(key, value),
            None => command.env_remove(key),
        };
    }

    // The probe leads its own process group, so a timeout can take down the
    // children a CLI started for itself as well as the CLI.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }

    let mut child = command.spawn().map_err(|source| DetectError::Spawn {
        program: program.to_owned(),
        source,
    })?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(source) => {
                kill(&mut child);
                return Err(DetectError::Wait {
                    program: program.to_owned(),
                    source,
                });
            }
        }
        if Instant::now() >= deadline {
            kill(&mut child);
            return Err(DetectError::Timeout {
                program: program.to_owned(),
                timeout,
            });
        }
        thread::sleep(POLL_INTERVAL);
    }
    child
        .wait_with_output()
        .map_err(|source| DetectError::Wait {
            program: program.to_owned(),
            source,
        })
}

/// Kills a probe that timed out or failed, and every process it started, then
/// reaps it.
///
/// The child leads its own process group (see [`run_with_timeout`]), so a group
/// kill reaches the grandchildren too: a CLI that shelled out to its own child
/// would otherwise leave it running after the probe is gone. `libc` is not a
/// dependency, so the group is signalled with `kill -KILL -<pgid>`.
fn kill(child: &mut std::process::Child) {
    #[cfg(unix)]
    kill_group(child.id());
    let _ = child.kill();
    let _ = child.wait();
}

/// Kill the process group led by `leader`, ignoring a group that is already
/// gone.
#[cfg(unix)]
fn kill_group(leader: u32) {
    let _ = Command::new("kill")
        .arg("-KILL")
        .arg(format!("-{leader}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Runs the version probe and scrapes a version number out of its output.
fn detect_version(
    binary: &Path,
    args: &[String],
    env: &[(String, Option<String>)],
    timeout: Duration,
) -> Option<String> {
    let output = run_with_timeout(binary, args, env, timeout).ok()?;
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    first_version(&text)
}

/// The first `major.minor[.patch]` number in `text`, as a token boundary allows.
fn first_version(text: &str) -> Option<String> {
    text.split_whitespace()
        .find_map(version_in)
        .map(str::to_owned)
}

/// The first version number embedded in one whitespace-delimited token.
///
/// The number has to start and end on a boundary, so a longer dotted run like
/// `1.2.3.4` is rejected rather than truncated.
fn version_in(token: &str) -> Option<&str> {
    let bytes = token.as_bytes();
    for start in 0..bytes.len() {
        if !bytes[start].is_ascii_digit() {
            continue;
        }
        if start > 0 && (bytes[start - 1].is_ascii_digit() || bytes[start - 1] == b'.') {
            continue;
        }
        if let Some(end) = version_end(bytes, start) {
            return token.get(start..end);
        }
    }
    None
}

/// The end index of a `major.minor[.patch]` number starting at `start`.
fn version_end(bytes: &[u8], start: usize) -> Option<usize> {
    let after_major = skip_digits(bytes, start);
    if after_major == start || bytes.get(after_major) != Some(&b'.') {
        return None;
    }
    let after_minor = skip_digits(bytes, after_major + 1);
    if after_minor == after_major + 1 {
        return None;
    }
    let mut end = after_minor;
    if bytes.get(end) == Some(&b'.') {
        let after_patch = skip_digits(bytes, end + 1);
        if after_patch > end + 1 {
            end = after_patch;
        }
    }
    match bytes.get(end) {
        Some(b'.') => None,
        Some(byte) if byte.is_ascii_digit() => None,
        _ => Some(end),
    }
}

/// The index of the first non-digit at or after `start`.
fn skip_digits(bytes: &[u8], start: usize) -> usize {
    let mut index = start;
    while bytes.get(index).is_some_and(u8::is_ascii_digit) {
        index += 1;
    }
    index
}

/// Runs the spec's sign-in probe and reduces its exit status to a [`SignIn`].
fn detect_signin(
    spec: &HarnessSpec,
    binary: &Path,
    env: &[(String, Option<String>)],
    model: Option<&str>,
    timeout: Duration,
) -> SignIn {
    let Some(args) = &spec.detect.signin_args else {
        return SignIn::Unknown("no probe".to_owned());
    };
    if model.is_none() && args.iter().any(|arg| arg.contains(MODEL_PLACEHOLDER)) {
        return SignIn::Unknown("needs a model".to_owned());
    }
    let values = Values {
        model: model.map(str::to_owned),
        ..Values::default()
    };
    let args = match render_args(args, &values) {
        Ok(args) => args,
        Err(error) => return SignIn::Unknown(error.to_string()),
    };
    match run_with_timeout(binary, &args, env, timeout) {
        Ok(output) if output.status.success() => SignIn::SignedIn,
        Ok(_) => SignIn::SignedOut,
        Err(error) => SignIn::Unknown(error.to_string()),
    }
}

/// Detects one harness: where its binary is, its version and its sign-in state.
///
/// `model` is only needed by a sign-in probe whose answer depends on the model
/// (see [`DetectSpec::signin_args`](crate::spec::DetectSpec::signin_args)); the
/// other probes run whatever it is.
pub fn detect(
    spec: &HarnessSpec,
    path_env: &str,
    env: &[(String, Option<String>)],
    model: Option<&str>,
) -> Detection {
    let binary = find_binary(&spec.detect.binary, path_env);
    let Some(binary) = binary else {
        return Detection {
            id: spec.id.clone(),
            name: spec.name.clone(),
            vendor: spec.vendor,
            binary: None,
            version: None,
            signin: SignIn::Unknown("not installed".to_owned()),
        };
    };
    let version = detect_version(&binary, &spec.detect.version_args, env, VERSION_TIMEOUT);
    let signin = detect_signin(spec, &binary, env, model, SIGNIN_TIMEOUT);
    Detection {
        id: spec.id.clone(),
        name: spec.name.clone(),
        vendor: spec.vendor,
        binary: Some(binary),
        version,
        signin,
    }
}

/// Detects every spec, probing them in parallel because each CLI is a process
/// launch and they do not depend on one another.
///
/// `models` maps a spec id to the model to probe with, for a CLI whose sign-in
/// state depends on the model.
pub fn detect_all(
    specs: &[HarnessSpec],
    path_env: &str,
    env: &[(String, Option<String>)],
    models: &BTreeMap<String, String>,
) -> Vec<Detection> {
    thread::scope(|scope| {
        let handles: Vec<_> = specs
            .iter()
            .map(|spec| {
                let model = models.get(&spec.id).map(String::as_str);
                scope.spawn(move || detect(spec, path_env, env, model))
            })
            .collect();
        handles
            .into_iter()
            .filter_map(|handle| handle.join().ok())
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    /// A script interactive shell can run, standing in for a real agent CLI.
    fn write_script(dir: &Path, name: &str, body: &str) {
        let path = dir.join(name);
        if let Err(error) = fs::write(&path, format!("#!/bin/sh\n{body}\n")) {
            panic!("writing {name} failed: {error}");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::Permissions::from_mode(0o755);
            if let Err(error) = fs::set_permissions(&path, mode) {
                panic!("chmod {name} failed: {error}");
            }
        }
    }

    /// A spec whose `[detect]` block and templates the caller chooses.
    fn spec(binary: &str, version_args: &str, signin_args: &str) -> HarnessSpec {
        let text = format!(
            r#"
id = "{binary}"
name = "Fake {binary}"
vendor = "pi"
verified_version = "1.0.0"
dry_patterns = []

[detect]
binary = "{binary}"
version_args = {version_args}
signin_hint = "fake sign in"
signin_args = {signin_args}

[launch]
interactive = ["{binary}"]
model_args = ["--model", "{{model}}"]
verified = false

[models]

[records]
kind = "jsonl"
location = "~/.fake/{{session}}.jsonl"
errors = "error"
verified = true

[quota]
source = "none"

[skills]
home_dirs = []
project_dirs = []
"#
        );
        match toml::from_str::<HarnessSpec>(&text) {
            Ok(spec) => spec,
            Err(error) => panic!("fixture should parse: {error}"),
        }
    }

    /// The `PATH` value that points only at `dir`, so the host's real CLIs stay
    /// out of the test.
    fn path_of(dir: &Path) -> String {
        dir.display().to_string()
    }

    #[test]
    fn finds_the_binary_on_the_path() {
        let dir = match TempDir::new() {
            Ok(dir) => dir,
            Err(error) => panic!("tempdir failed: {error}"),
        };
        write_script(dir.path(), "fake", "exit 0");
        let found = find_binary("fake", &path_of(dir.path()));
        assert_eq!(found, Some(dir.path().join("fake")));
        assert_eq!(find_binary("missing", &path_of(dir.path())), None);
    }

    #[test]
    fn skips_a_file_that_is_not_executable() {
        let dir = match TempDir::new() {
            Ok(dir) => dir,
            Err(error) => panic!("tempdir failed: {error}"),
        };
        if let Err(error) = fs::write(dir.path().join("fake"), "not a program") {
            panic!("writing fake failed: {error}");
        }
        assert_eq!(find_binary("fake", &path_of(dir.path())), None);
    }

    #[test]
    fn detects_version_and_a_signed_in_cli() {
        let dir = match TempDir::new() {
            Ok(dir) => dir,
            Err(error) => panic!("tempdir failed: {error}"),
        };
        write_script(
            dir.path(),
            "fake",
            "case \"$1\" in\n  --version) echo 'fake 1.2.3' ;;\n  auth) exit 0 ;;\nesac",
        );
        let spec = spec("fake", r#"["--version"]"#, r#"["auth", "status"]"#);
        let found = detect(&spec, &path_of(dir.path()), &[], None);
        assert_eq!(found.binary, Some(dir.path().join("fake")));
        assert_eq!(found.version.as_deref(), Some("1.2.3"));
        assert_eq!(found.signin, SignIn::SignedIn);
        assert_eq!(found.vendor, Vendor::Pi);
    }

    #[test]
    fn detects_a_signed_out_cli() {
        let dir = match TempDir::new() {
            Ok(dir) => dir,
            Err(error) => panic!("tempdir failed: {error}"),
        };
        write_script(
            dir.path(),
            "fake",
            "case \"$1\" in\n  auth) exit 1 ;;\nesac",
        );
        let spec = spec("fake", r#"["--version"]"#, r#"["auth", "status"]"#);
        let found = detect(&spec, &path_of(dir.path()), &[], None);
        assert_eq!(found.version, None);
        assert_eq!(found.signin, SignIn::SignedOut);
    }

    #[test]
    fn a_probe_that_hangs_is_unknown() {
        let dir = match TempDir::new() {
            Ok(dir) => dir,
            Err(error) => panic!("tempdir failed: {error}"),
        };
        write_script(dir.path(), "fake", "sleep 30");
        let spec = spec("fake", r#"["--version"]"#, r#"["auth", "status"]"#);
        let binary = dir.path().join("fake");
        let signin = detect_signin(&spec, &binary, &[], None, Duration::from_millis(250));
        match signin {
            SignIn::Unknown(reason) => {
                assert!(reason.contains("did not finish"), "reason: {reason}");
            }
            other => panic!("expected unknown, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_binary_is_not_installed() {
        let dir = match TempDir::new() {
            Ok(dir) => dir,
            Err(error) => panic!("tempdir failed: {error}"),
        };
        let spec = spec("fake", r#"["--version"]"#, r#"["auth", "status"]"#);
        let found = detect(&spec, &path_of(dir.path()), &[], None);
        assert_eq!(found.binary, None);
        assert_eq!(found.version, None);
        assert_eq!(found.signin, SignIn::Unknown("not installed".to_owned()));
    }

    #[test]
    fn a_model_probe_without_a_model_is_unknown() {
        let dir = match TempDir::new() {
            Ok(dir) => dir,
            Err(error) => panic!("tempdir failed: {error}"),
        };
        write_script(dir.path(), "fake", "exit 0");
        let signin = r#"["auth", "check", "--model", "{model}"]"#;
        let spec = spec("fake", r#"["--version"]"#, signin);
        let found = detect(&spec, &path_of(dir.path()), &[], None);
        assert_eq!(found.signin, SignIn::Unknown("needs a model".to_owned()));

        let found = detect(&spec, &path_of(dir.path()), &[], Some("m/one"));
        assert_eq!(found.signin, SignIn::SignedIn);
    }

    #[test]
    fn a_spec_without_a_probe_is_unknown() {
        let dir = match TempDir::new() {
            Ok(dir) => dir,
            Err(error) => panic!("tempdir failed: {error}"),
        };
        write_script(dir.path(), "fake", "exit 0");
        let mut spec = spec("fake", r#"["--version"]"#, r#"["auth"]"#);
        spec.detect.signin_args = None;
        let found = detect(&spec, &path_of(dir.path()), &[], None);
        assert_eq!(found.signin, SignIn::Unknown("no probe".to_owned()));
    }

    #[test]
    fn scrapes_a_version_out_of_noisy_output() {
        assert_eq!(
            first_version("claude 2.1.311 (Claude Code)"),
            Some("2.1.311".to_owned())
        );
        assert_eq!(first_version("codex-cli/0.9"), Some("0.9".to_owned()));
        assert_eq!(first_version("v1.2.3"), Some("1.2.3".to_owned()));
        assert_eq!(first_version("1.2.3.4"), None);
        assert_eq!(first_version("no version here"), None);
    }

    #[test]
    fn runs_a_command_with_an_adjusted_environment() {
        let dir = match TempDir::new() {
            Ok(dir) => dir,
            Err(error) => panic!("tempdir failed: {error}"),
        };
        write_script(dir.path(), "fake", "echo \"$FAKE_KEPT-$FAKE_DROPPED\"");
        let env = vec![
            ("FAKE_KEPT".to_owned(), Some("kept".to_owned())),
            ("FAKE_DROPPED".to_owned(), None),
        ];
        let binary = dir.path().join("fake");
        let output = run_with_timeout(&binary, &[], &env, Duration::from_secs(5));
        match output {
            Ok(output) => {
                let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
                assert_eq!(text, "kept-");
                assert!(output.status.success());
            }
            Err(error) => panic!("running the fake failed: {error}"),
        }
    }

    #[test]
    fn detects_every_spec_in_parallel() {
        let dir = match TempDir::new() {
            Ok(dir) => dir,
            Err(error) => panic!("tempdir failed: {error}"),
        };
        write_script(
            dir.path(),
            "one",
            "case \"$1\" in\n  --version) echo 'one 1.0' ;;\nesac",
        );
        write_script(
            dir.path(),
            "two",
            "case \"$1\" in\n  --version) echo 'two 2.0' ;;\nesac",
        );
        let specs = vec![
            spec("one", r#"["--version"]"#, r#"["auth"]"#),
            spec("two", r#"["--version"]"#, r#"["auth"]"#),
        ];
        let found = detect_all(&specs, &path_of(dir.path()), &[], &BTreeMap::new());
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].version.as_deref(), Some("1.0"));
        assert_eq!(found[1].version.as_deref(), Some("2.0"));
        assert_eq!(found[0].signin, SignIn::SignedIn);
    }

    /// Whether `pid` is still a live process.
    fn alive(pid: &str) -> bool {
        std::process::Command::new("kill")
            .args(["-0", pid])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    #[test]
    fn a_timeout_kills_what_the_probe_started() {
        let dir = match TempDir::new() {
            Ok(dir) => dir,
            Err(error) => panic!("tempdir failed: {error}"),
        };
        let pidfile = dir.path().join("child.pid");
        let body = format!("sleep 30 &\necho $! > {}\nsleep 30", pidfile.display());
        write_script(dir.path(), "forker", &body);
        let binary = dir.path().join("forker");

        // Generous enough that the script reaches its second line before the
        // timeout fires, even on a machine where a fork is slow.
        let out = run_with_timeout(&binary, &[], &[], Duration::from_secs(2));
        assert!(
            matches!(out, Err(DetectError::Timeout { .. })),
            "expected a timeout, got {out:?}"
        );
        let pid = match fs::read_to_string(&pidfile) {
            Ok(pid) => pid.trim().to_owned(),
            Err(error) => panic!("reading {}: {error}", pidfile.display()),
        };
        assert!(
            !pid.is_empty(),
            "the script should have written its child pid"
        );

        let deadline = Instant::now() + Duration::from_secs(5);
        while alive(&pid) && Instant::now() < deadline {
            thread::sleep(POLL_INTERVAL);
        }
        assert!(
            !alive(&pid),
            "the background child {pid} survived the timeout"
        );
    }
}
