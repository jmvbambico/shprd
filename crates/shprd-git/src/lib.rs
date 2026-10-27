//! Worktrees, scopes, integration branch, archive refs, janitor.
//!
//! Everything here is real git, driven through [`std::process::Command`] and
//! never a shell, so a branch name or a path can never turn into a command.

pub mod changed;
pub mod git;
pub mod worktree;
