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
/// `env` is expressed as `env -u NAME ... NAME=VALUE ...`: every `env` option
/// has to come before the assignments, so removals are emitted first. Within
/// each group the caller's order is kept, and a removal then a set for the same
/// key ends up set, which is what "a later entry wins" means for a pair that
/// crosses the two groups. `exec` replaces the pane's shell with the program,
/// so the pane's process is the program and not a shell waiting behind it.
pub(crate) fn line(argv: &[String], env: &[(String, Option<String>)]) -> Result<String> {
    if argv.is_empty() {
        return Err(MuxError::command("run", "refusing to run an empty argv"));
    }
    let mut line = String::from("exec env");
    for name in env
        .iter()
        .filter_map(|(name, value)| value.is_none().then_some(name))
    {
        line.push_str(" -u ");
        line.push_str(&quote(name)?);
    }
    for (name, value) in env {
        if let Some(value) = value {
            line.push(' ');
            line.push_str(&quote(&format!("{name}={value}"))?);
        }
    }
    for element in argv {
        line.push(' ');
        line.push_str(&quote(element)?);
    }
    Ok(line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn line_of(argv: &[String], env: &[(String, Option<String>)]) -> String {
        super::line(argv, env).unwrap()
    }

    fn argv(elements: &[&str]) -> Vec<String> {
        elements.iter().map(|element| element.to_string()).collect()
    }

    #[test]
    fn a_plain_command_is_quoted_element_by_element() {
        let line = line(&argv(&["sh", "-c", "echo hello"]), &[]).unwrap();
        assert_eq!(line, "exec env 'sh' '-c' 'echo hello'");
    }

    #[test]
    fn a_single_quote_is_escaped_not_ended() {
        assert_eq!(quote("it's").unwrap(), "'it'\\''s'");
        assert_eq!(quote("'").unwrap(), "''\\'''");
        assert_eq!(quote("a''b").unwrap(), "'a'\\'''\\''b'");
    }

    #[test]
    fn dollars_backticks_and_globs_stay_literal() {
        for hostile in ["$HOME", "`id`", "*", "$(rm -rf /)", "~", "\\n"] {
            let quoted = quote(hostile).unwrap();
            assert!(quoted.starts_with('\''));
            assert!(quoted.ends_with('\''));
            assert_eq!(quoted[1..quoted.len() - 1], *hostile);
        }
    }

    #[test]
    fn a_newline_is_refused_because_it_would_end_the_line() {
        let error = quote("echo\nrm -rf /").unwrap_err();
        assert!(error.to_string().contains("newline"));
        assert!(quote("a\rb").is_err());
        assert!(line(&argv(&["echo", "a\nb"]), &[]).is_err());
    }

    #[test]
    fn an_empty_argv_is_refused() {
        let error = line(&[], &[]).unwrap_err();
        assert!(error.to_string().contains("empty argv"));
    }

    #[test]
    fn environment_sets_and_removals_keep_their_order() {
        let env = vec![
            ("A".to_string(), None),
            ("B".to_string(), Some("two words".to_string())),
            ("C".to_string(), Some(String::new())),
        ];
        let line = line(&argv(&["true"]), &env).unwrap();
        assert_eq!(line, "exec env -u 'A' 'B=two words' 'C=' 'true'");
        let reversed = vec![
            ("B".to_string(), Some("set".to_string())),
            ("B".to_string(), None),
        ];
        // A removal always precedes the sets, so the set wins here.
        assert_eq!(
            line_of(&argv(&["true"]), &reversed),
            "exec env -u 'B' 'B=set' 'true'"
        );
    }

    #[test]
    fn a_quote_in_an_environment_value_cannot_break_out() {
        let env = vec![("X".to_string(), Some("a'b".to_string()))];
        let line = line(&argv(&["true"]), &env).unwrap();
        assert_eq!(line, "exec env 'X=a'\\''b' 'true'");
    }

    /// The only real proof: hand the line to a shell and check that what ran is
    /// the argv we meant, with the environment we meant.
    #[test]
    fn a_shell_runs_the_line_as_one_program_with_our_arguments() {
        let env = vec![
            ("SHPRD_SET".to_string(), Some("va'lue $x `y`".to_string())),
            ("SHPRD_GONE".to_string(), None),
        ];
        let line = line(
            &argv(&[
                "sh",
                "-c",
                "printf '%s|%s' \"$SHPRD_SET\" \"${SHPRD_GONE-unset}\"",
            ]),
            &env,
        )
        .unwrap();
        let output = Command::new("sh")
            .arg("-c")
            .arg(&line)
            .env("SHPRD_GONE", "inherited")
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "va'lue $x `y`|unset"
        );
        assert!(output.stderr.is_empty());
    }
}
