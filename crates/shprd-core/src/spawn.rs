//! Spawning an external program, retrying the `ETXTBSY` the kernel reports.
//!
//! `execve` fails with `ETXTBSY` ("Text file busy", os error 26) while any
//! process still holds a descriptor open for writing on the file being
//! executed, even a descriptor it merely inherited and never writes to. A
//! program that was written moments ago, and a child forked by another thread
//! in the same process between that fork and its own `execve`, are enough to
//! have a spawn refused for a few milliseconds. macOS never reports the error,
//! Linux does, and it shows up as a one-in-a-hundred failure in CI.
//!
//! The refusal is transient, so every spawn shprd makes goes through this
//! module: a spawn that fails with `ETXTBSY` is tried again after a short
//! backoff, up to [`BUSY_RETRY_DELAYS`] times. Any other error is returned at
//! once, and so is `ETXTBSY` once the retries are spent. This is the same
//! treatment Go's `os/exec` and cargo give the race.

use std::io;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::thread;
use std::time::Duration;

/// `ETXTBSY`, as the kernel reports it in [`io::Error::raw_os_error`].
///
/// It is 26 on both Linux and macOS; only Linux reports it for a busy
/// executable, which is why the race is invisible on a developer's machine.
#[cfg(unix)]
const ETXTBSY: i32 = 26;

/// How long to wait before each retry of a spawn the kernel refused with
/// `ETXTBSY`, one delay per retry after the first attempt.
///
/// The list has four entries, so a busy program is tried five times in all and
/// a spawn that stays busy costs about 150 ms before it fails.
pub const BUSY_RETRY_DELAYS: [Duration; 4] = [
    Duration::from_millis(10),
    Duration::from_millis(20),
    Duration::from_millis(40),
    Duration::from_millis(80),
];

/// Whether `error` is the kernel refusing to execute a file that is open for
/// writing.
fn is_busy(error: &io::Error) -> bool {
    if error.kind() == io::ErrorKind::ExecutableFileBusy {
        return true;
    }
    #[cfg(unix)]
    {
        // Belt and braces: a std that did not map ETXTBSY to its own kind still
        // reports the raw number, which is the same on Linux and macOS.
        error.raw_os_error() == Some(ETXTBSY)
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// Runs `attempt` until it no longer fails with `ETXTBSY`.
///
/// The first attempt is immediate; each retry waits one more entry of
/// [`BUSY_RETRY_DELAYS`]. Any error other than `ETXTBSY` is returned at once,
/// and a spawn that is still busy after the last retry returns the last error,
/// so a caller can report it exactly as if no retry had happened.
///
/// The attempt is a closure rather than a [`Command`] so the policy can be
/// tested without a real race: a test hands in a spawn that fails with
/// `ETXTBSY` a fixed number of times and counts the calls.
pub fn retry_busy<T>(mut attempt: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let mut delays = BUSY_RETRY_DELAYS.iter();
    loop {
        match attempt() {
            Ok(value) => return Ok(value),
            Err(error) => {
                if !is_busy(&error) {
                    return Err(error);
                }
                match delays.next() {
                    Some(delay) => thread::sleep(*delay),
                    None => return Err(error),
                }
            }
        }
    }
}

/// [`Command::spawn`], retried while the kernel refuses with `ETXTBSY`.
pub fn child(command: &mut Command) -> io::Result<Child> {
    retry_busy(|| command.spawn())
}

/// [`Command::status`], retried while the kernel refuses with `ETXTBSY`.
pub fn status(command: &mut Command) -> io::Result<ExitStatus> {
    retry_busy(|| command.status())
}

/// [`Command::output`], retried while the kernel refuses with `ETXTBSY`.
///
/// Only the spawn is retried: a command that ran is never run a second time.
/// Both streams are captured and stdin is `/dev/null`, which is what
/// [`Command::output`] does, so a caller that wants anything else builds the
/// child with [`child`] instead.
pub fn output(command: &mut Command) -> io::Result<Output> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let spawned = child(command)?;
    spawned.wait_with_output()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::cell::Cell;

    /// The error the kernel returns for a file that is open for writing.
    fn busy() -> io::Error {
        io::Error::from_raw_os_error(26)
    }

    #[test]
    fn a_busy_spawn_is_retried_until_it_succeeds() {
        let attempts = Cell::new(0_u32);
        let outcome = retry_busy(|| {
            attempts.set(attempts.get() + 1);
            if attempts.get() < 3 {
                Err(busy())
            } else {
                Ok("ran")
            }
        });
        assert_eq!(outcome.unwrap(), "ran");
        assert_eq!(attempts.get(), 3, "two busy refusals, then the run");
    }

    #[test]
    fn a_spawn_that_stays_busy_returns_the_last_busy_error() {
        let attempts = Cell::new(0_u32);
        let outcome: io::Result<()> = retry_busy(|| {
            attempts.set(attempts.get() + 1);
            Err(busy())
        });
        let error = outcome.expect_err("a spawn that never stops being busy must fail");
        assert_eq!(error.raw_os_error(), Some(26), "{error}");
        assert_eq!(
            attempts.get(),
            u32::try_from(BUSY_RETRY_DELAYS.len()).unwrap() + 1,
            "one attempt plus one per backoff"
        );
    }

    #[test]
    fn another_error_is_not_retried() {
        let attempts = Cell::new(0_u32);
        let outcome: io::Result<()> = retry_busy(|| {
            attempts.set(attempts.get() + 1);
            Err(io::Error::from(io::ErrorKind::NotFound))
        });
        assert_eq!(
            outcome.expect_err("a missing program must fail").kind(),
            io::ErrorKind::NotFound
        );
        assert_eq!(attempts.get(), 1, "only a busy file is worth retrying");
    }

    #[test]
    fn a_first_try_that_works_is_not_attempted_twice() {
        let attempts = Cell::new(0_u32);
        let outcome = retry_busy(|| {
            attempts.set(attempts.get() + 1);
            Ok(7)
        });
        assert_eq!(outcome.unwrap(), 7);
        assert_eq!(attempts.get(), 1, "a spawn that works must not be retried");
    }

    #[test]
    fn the_retry_budget_is_the_documented_five_tries() {
        assert_eq!(
            BUSY_RETRY_DELAYS,
            [
                Duration::from_millis(10),
                Duration::from_millis(20),
                Duration::from_millis(40),
                Duration::from_millis(80),
            ]
        );
    }

    /// A real spawn through the helper: both streams are captured and stdin is
    /// closed, so a program that reads stdin sees end-of-file at once.
    #[cfg(unix)]
    #[test]
    fn output_captures_both_streams_and_closes_stdin() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "echo out; echo err >&2; cat"]);
        let output = output(&mut command).expect("a real shell must run");
        assert!(output.status.success(), "{output:?}");
        assert_eq!(String::from_utf8_lossy(&output.stdout), "out\n");
        assert_eq!(String::from_utf8_lossy(&output.stderr), "err\n");
    }
}
