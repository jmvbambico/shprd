//! Running the person's own tunnel programs.
//!
//! Three things every provider needs and none should get wrong: finding the
//! program on the `PATH` it was handed (this crate reads no environment
//! variable), running a short command with a time limit, and starting a long
//! one whose output is read for its address and then drained so its pipes never
//! fill and stall it.
//!
//! A child is started with `kill_on_drop`, so a tunnel dies with the service
//! even when the service does not get to stop it (M7d §4.3 rule 4).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use super::parse::{redact, strip_ansi};

/// How long a short command (a version, a status) may take.
pub(crate) const SHORT: Duration = Duration::from_secs(10);

/// The first executable named `name` in the `PATH` string `path`.
///
/// Only absolute directories count: a relative entry (`.`, an empty one) would
/// make "the person's own binary" mean "whatever is in the current folder".
pub(crate) fn find_program(name: &str, path: &str) -> Option<PathBuf> {
    std::env::split_paths(path)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
}

/// Whether `path` is a regular file this user can run.
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// What a short command printed.
pub(crate) struct Captured {
    /// Whether it exited with status zero.
    pub ok: bool,
    /// Standard output.
    pub out: String,
    /// Standard error.
    pub err: String,
}

/// Runs `program args…` to completion, giving up after `limit`.
///
/// It reads nothing from stdin and is killed if the time runs out. An error to
/// start it, or a timeout, is a failed [`Captured`] with the reason in `err`.
pub(crate) async fn capture(program: &Path, args: &[&str], limit: Duration) -> Captured {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            return Captured {
                ok: false,
                out: String::new(),
                err: err.to_string(),
            };
        }
    };
    match tokio::time::timeout(limit, child.wait_with_output()).await {
        Ok(Ok(output)) => Captured {
            ok: output.status.success(),
            out: String::from_utf8_lossy(&output.stdout).into_owned(),
            err: String::from_utf8_lossy(&output.stderr).into_owned(),
        },
        Ok(Err(err)) => Captured {
            ok: false,
            out: String::new(),
            err: err.to_string(),
        },
        Err(_) => Captured {
            ok: false,
            out: String::new(),
            err: format!("timed out after {}s", limit.as_secs()),
        },
    }
}

/// The first non-empty line of `program args…`, for the "which one is this"
/// banner. `ssh -V` and some others print it on stderr, so both are read.
pub(crate) async fn version_line(program: &Path, args: &[&str]) -> Option<String> {
    let captured = capture(program, args, SHORT).await;
    let text = if captured.out.trim().is_empty() {
        captured.err
    } else {
        captured.out
    };
    text.lines()
        .map(|line| strip_ansi(line).trim().to_owned())
        .find(|line| !line.is_empty())
        .map(|line| redact(&line))
}

/// A long-running child and the lines it prints.
pub(crate) struct Running {
    /// The process. Dropping it kills it.
    pub child: tokio::process::Child,
    /// Lines from stdout and stderr, in the order they arrive.
    pub lines: mpsc::Receiver<String>,
}

/// Starts `program args…` with its output piped through [`Running::lines`].
///
/// Both streams are read to the end by their own tasks whether or not anyone
/// reads the channel, so a quiet reader cannot fill a pipe and stall the tunnel.
pub(crate) fn spawn(program: &Path, args: &[String]) -> std::io::Result<Running> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn()?;
    let (sender, lines) = mpsc::channel(256);
    if let Some(out) = child.stdout.take() {
        pump(out, sender.clone());
    }
    if let Some(err) = child.stderr.take() {
        pump(err, sender);
    }
    Ok(Running { child, lines })
}

/// Forwards `reader`'s lines until it ends, dropping them if nobody listens.
fn pump(reader: impl AsyncRead + Unpin + Send + 'static, sender: mpsc::Sender<String>) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).split(b'\n');
        // `split` on bytes: a program that prints something that is not UTF-8
        // is lossy-decoded rather than ending the stream.
        while let Ok(Some(bytes)) = lines.next_segment().await {
            let line = String::from_utf8_lossy(&bytes).into_owned();
            // A closed channel is not a reason to stop reading.
            let _ = sender.send(line).await;
        }
    });
}

/// Reads and discards `lines` until the child's streams end, so the pump tasks
/// are never blocked on a full channel.
pub(crate) fn drain(mut lines: mpsc::Receiver<String>) {
    tokio::spawn(async move { while lines.recv().await.is_some() {} });
}

/// Ends `child` and waits briefly for it to be gone.
pub(crate) async fn kill(child: &mut tokio::process::Child) {
    let _ = child.start_kill();
    let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn program_is_found_on_the_given_path_only() {
        let dir = tempfile::tempdir().unwrap();
        script(dir.path(), "faketun", "true");
        let path = format!("/nonexistent:{}", dir.path().display());
        assert!(find_program("faketun", &path).is_some());
        assert!(find_program("faketun", "/nonexistent").is_none());
        // A relative entry is not "the person's own binary".
        assert!(find_program("faketun", ".").is_none());
        assert!(find_program("faketun", "").is_none());
    }

    #[test]
    fn a_file_that_is_not_executable_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("plain"), "x").unwrap();
        assert!(find_program("plain", &dir.path().display().to_string()).is_none());
    }

    #[tokio::test]
    async fn version_is_the_first_line_of_either_stream() {
        let dir = tempfile::tempdir().unwrap();
        let out = script(dir.path(), "v-out", "echo 'tun 1.2.3'; echo second");
        let err = script(dir.path(), "v-err", "echo 'OpenSSH_9' >&2");
        assert_eq!(version_line(&out, &[]).await.as_deref(), Some("tun 1.2.3"));
        assert_eq!(version_line(&err, &[]).await.as_deref(), Some("OpenSSH_9"));
    }

    #[tokio::test]
    async fn capture_times_out_instead_of_hanging() {
        let dir = tempfile::tempdir().unwrap();
        let slow = script(dir.path(), "slow", "exec sleep 30");
        let started = std::time::Instant::now();
        let got = capture(&slow, &[], Duration::from_millis(300)).await;
        assert!(!got.ok);
        assert!(got.err.contains("timed out"), "{}", got.err);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn lines_from_both_streams_arrive() {
        let dir = tempfile::tempdir().unwrap();
        let both = script(dir.path(), "both", "echo out-line; echo err-line >&2");
        let mut running = spawn(&both, &[]).unwrap();
        let mut got = Vec::new();
        while let Some(line) = running.lines.recv().await {
            got.push(line);
        }
        got.sort();
        assert_eq!(got, ["err-line", "out-line"]);
    }
}
