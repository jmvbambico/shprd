//! The ask-hook installer seam.
//!
//! An ask is a question, a permission or a plan a program waits on: until it is
//! answered the agent reads no brief and answers nothing, so the person has to
//! be shown it as a card (M9.5). Surfacing one means writing a hook into the
//! program's own config file, which only an adapter for that program can do.
//!
//! The schema in [`crate::spec::AskHookSpec`] says *what* a program needs
//! (which config file, which shape, which events, the command pattern); this
//! module is the trait the adapters implement and the no-op every harness gets
//! until M9.5 ships them, so a caller can wire the seam now and never touch a
//! person's files by accident.

use std::io;
use std::path::{Path, PathBuf};

use crate::spec::AskHookSpec;

/// Where the ask hook lives on this machine.
#[derive(Debug, Clone, Copy)]
pub struct AskHookPaths<'a> {
    /// The ask hook executable the adapter installs.
    pub hook: &'a Path,
}

impl<'a> AskHookPaths<'a> {
    /// The paths an install runs with, for a hook executable at `hook`.
    pub fn new(hook: &'a Path) -> Self {
        Self { hook }
    }
}

/// What an install did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskHookInstall {
    /// The hook was not in the config and is now.
    Added,
    /// The hook was already in the config, so the file stayed as it was.
    AlreadyThere,
    /// The program takes no ask hook, or the harness does not say how to
    /// install one, so nothing was touched.
    Unsupported,
}

/// Install a program's ask hook into its own config file.
///
/// An installer is handed the hook executable's path, not the hook's words: the
/// program's config file holds one command per event, and the adapter answers
/// the file's exact shape from [`crate::spec::AskHookConfig`]. The default
/// [`Self::install`] touches nothing and reports
/// [`AskHookInstall::Unsupported`], so a type that only knows its spec is
/// always safe to call.
pub trait AskHookInstaller {
    /// The ask-hook facts from the harness spec.
    fn spec(&self) -> &AskHookSpec;

    /// Whether this installer can put a hook in place at all.
    fn available(&self) -> bool {
        self.spec().supported
    }

    /// The command a config entry gets, with the hook's path filled in.
    ///
    /// The harness's pattern quotes `{hook}` where a program's config file
    /// needs it; a program that takes no hook gets [`None`].
    fn command(&self, paths: &AskHookPaths<'_>) -> Option<String> {
        let spec = self.spec();
        if !spec.supported {
            return None;
        }
        let pattern = spec.command.as_deref()?;
        Some(pattern.replace("{hook}", &paths.hook.display().to_string()))
    }

    /// Put the hook in place, or leave the file as it was.
    ///
    /// The default is a no-op: it reports [`AskHookInstall::Unsupported`] and
    /// writes nothing.
    fn install(&self, _paths: &AskHookPaths<'_>) -> Result<AskHookInstall, AskHookError> {
        Ok(AskHookInstall::Unsupported)
    }
}

/// The installer every harness gets until M9.5 ships the real ones: it reads no
/// file and writes none.
#[derive(Debug, Clone, Copy)]
pub struct NoAskHook<'a> {
    spec: &'a AskHookSpec,
}

impl<'a> NoAskHook<'a> {
    /// The no-op installer for one harness's ask-hook facts.
    pub fn new(spec: &'a AskHookSpec) -> Self {
        Self { spec }
    }
}

impl AskHookInstaller for NoAskHook<'_> {
    fn spec(&self) -> &AskHookSpec {
        self.spec
    }
}

/// An error while installing a program's ask hook.
#[derive(Debug, thiserror::Error)]
pub enum AskHookError {
    /// The program's config file could not be read.
    #[error("failed to read the ask hook config {path}: {source}")]
    Read {
        /// The file that failed.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// The program's config file could not be written.
    #[error("failed to write the ask hook config {path}: {source}")]
    Write {
        /// The file that failed.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// The program's config file does not have the shape the harness names.
    #[error("the ask hook config {path} is not valid {format}: {reason}")]
    Invalid {
        /// The file that could not be read.
        path: PathBuf,
        /// The shape the harness expected, e.g. `claude_settings`.
        format: &'static str,
        /// What is wrong with the file.
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{AskHookInstall, AskHookInstaller, AskHookPaths, NoAskHook};
    use crate::spec::{AskEvent, AskHookConfig, AskHookSpec, AskKind};

    fn supported() -> AskHookSpec {
        AskHookSpec {
            supported: true,
            config: Some("~/.claude/settings.json".to_owned()),
            format: AskHookConfig::ClaudeSettings,
            command: Some("sh '{hook}'".to_owned()),
            events: vec![AskEvent {
                kind: AskKind::Permission,
                event: "PermissionRequest".to_owned(),
                matcher: None,
            }],
            timeout_seconds: Some(1800),
        }
    }

    #[test]
    fn the_default_installer_touches_nothing() {
        let spec = supported();
        let installer = NoAskHook::new(&spec);
        let paths = AskHookPaths::new(Path::new("/opt/shprd/libexec/ask-hook.sh"));
        assert!(installer.available());
        assert!(matches!(
            installer.install(&paths),
            Ok(AskHookInstall::Unsupported)
        ));
    }

    #[test]
    fn the_command_fills_the_hook_path_in() {
        let spec = supported();
        let installer = NoAskHook::new(&spec);
        let paths = AskHookPaths::new(Path::new("/opt/shprd/ask-hook.sh"));
        assert_eq!(
            installer.command(&paths).as_deref(),
            Some("sh '/opt/shprd/ask-hook.sh'")
        );
    }

    #[test]
    fn a_program_with_no_hook_has_no_command() {
        let spec = AskHookSpec::default();
        let installer = NoAskHook::new(&spec);
        let paths = AskHookPaths::new(Path::new("/opt/shprd/ask-hook.sh"));
        assert!(!installer.available());
        assert_eq!(installer.command(&paths), None);
    }
}
