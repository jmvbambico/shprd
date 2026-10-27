//! A thin, shell-free runner for the git commands shprd needs.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use thiserror::Error;

/// Why a git invocation did not produce usable output.
#[derive(Debug, Error)]
pub enum GitError {
    /// git could not be started at all.
    #[error("could not run git: {source}")]
    Spawn {
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// git ran and exited non-zero.
    #[error("git {} failed (exit {code:?}): {stderr}", args.join(" "))]
    Failed {
        /// The arguments, without the leading `git`.
        args: Vec<String>,
        /// The exit code, when the process reported one.
        code: Option<i32>,
        /// Whatever git wrote to stderr, trimmed.
        stderr: String,
    },
}

/// git in one directory.
///
/// Every call runs `git` directly, with prompts disabled and messages forced
/// to the C locale, so output is parseable no matter who runs shprd.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Git {
    /// The directory git runs in.
    dir: PathBuf,
}

impl Git {
    /// A runner for the repository (or worktree) at `dir`.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The directory git runs in.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Runs `git` with `args` and returns its stdout, less one trailing
    /// newline.
    pub fn run(&self, args: &[&str]) -> Result<String, GitError> {
        let output = Command::new("git")
            .args(args)
            .current_dir(&self.dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .output()
            .map_err(|source| GitError::Spawn { source })?;
        if !output.status.success() {
            return Err(GitError::Failed {
                args: args.iter().map(|arg| (*arg).to_string()).collect(),
                code: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }
        Ok(trim_one_newline(
            String::from_utf8_lossy(&output.stdout).into_owned(),
        ))
    }

    /// The commit HEAD points at.
    pub fn head_sha(&self) -> Result<String, GitError> {
        self.rev_parse("HEAD")
    }

    /// The checked-out branch, or `None` when HEAD is detached.
    pub fn current_branch(&self) -> Result<Option<String>, GitError> {
        let name = self.run(&["rev-parse", "--abbrev-ref", "HEAD"])?;
        if name == "HEAD" || name.is_empty() {
            return Ok(None);
        }
        Ok(Some(name))
    }

    /// True when the working tree has no staged or unstaged changes and no
    /// untracked files.
    pub fn is_clean(&self) -> Result<bool, GitError> {
        Ok(self.run(&["status", "--porcelain"])?.is_empty())
    }

    /// True when a local branch called `branch` exists.
    pub fn branch_exists(&self, branch: &str) -> Result<bool, GitError> {
        // `branch --list` exits zero and prints nothing when the branch is
        // absent, so a missing branch is not an error here.
        Ok(!self.run(&["branch", "--list", branch])?.is_empty())
    }

    /// Resolves `rev` to a commit id.
    pub fn rev_parse(&self, rev: &str) -> Result<String, GitError> {
        self.run(&["rev-parse", "--verify", rev])
    }
}

/// Drops one trailing newline, and the carriage return before it, if any.
fn trim_one_newline(mut text: String) -> String {
    if text.ends_with('\n') {
        text.pop();
        if text.ends_with('\r') {
            text.pop();
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_trailing_newline_goes() {
        assert_eq!(trim_one_newline("a\n".to_string()), "a");
        assert_eq!(trim_one_newline("a\n\n".to_string()), "a\n");
        assert_eq!(trim_one_newline("a".to_string()), "a");
        assert_eq!(trim_one_newline("a\r\n".to_string()), "a");
    }

    #[test]
    fn a_failing_command_reports_its_stderr() {
        let dir = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(err) => panic!("tempdir: {err}"),
        };
        let git = Git::new(dir.path());
        let err = match git.run(&["rev-parse", "HEAD"]) {
            Ok(out) => panic!("a non-repository must fail, got {out:?}"),
            Err(err) => err,
        };
        assert!(matches!(err, GitError::Failed { .. }), "{err:?}");
        assert!(!err.to_string().is_empty());
    }
}
