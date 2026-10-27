//! Turning an argv plus an environment delta into one shell line.
//!
//! herdr's `pane send-text` types a string into a pane's shell and Enter runs
//! it, so [`crate::Mux::run`] has to build a command line — the one place where
//! shprd's process-shaped input meets a shell. Every element is POSIX
//! single-quoted, so nothing in a branch name, an env value or a title can end
//! the quoting and become syntax. Values containing a newline are refused
//! outright: a newline is how the line would end early.

use shprd_mux::{MuxError, Result};

/// One argv element or environment assignment, single-quoted for `sh`.
///
/// Inside single quotes every byte is literal except `'` itself, which becomes
/// `'\''` (close, escaped quote, reopen).
pub(crate) fn quote(element: &str) -> Result<String> {
    if element.contains('\n') || element.contains('\r') {
        return Err(MuxError::command(
            "run",
            "refusing to send a command containing a newline",
        ));
    }
    let mut quoted = String::with_capacity(element.len() + 2);
    quoted.push('\'');
    quoted.push_str(&element.replace('\'', "'\\''"));
    quoted.push('\'');
    Ok(quoted)
}

/// The shell line that runs `argv` with `env` applied.
///
/// `env` is expressed with `env -u NAME` for removals and `NAME=VALUE` for
/// sets, in the order given, so a later entry for a key wins. `exec` replaces
/// the pane's shell with the program, so the pane's process is the program and
/// not a shell waiting behind it.
pub(crate) fn line(argv: &[String], env: &[(String, Option<String>)]) -> Result<String> {
    if argv.is_empty() {
        return Err(MuxError::command("run", "refusing to run an empty argv"));
    }
    let mut line = String::from("exec env");
    for (name, value) in env {
        line.push(' ');
        match value {
            Some(value) => {
                line.push_str(&quote(&format!("{name}={value}"))?);
            }
            None => {
                line.push_str("-u ");
                line.push_str(&quote(name)?);
            }
        }
    }
    for element in argv {
        line.push(' ');
        line.push_str(&quote(element)?);
    }
    Ok(line)
}
