//! The ask-hook installers.
//!
//! An ask is a question, a permission or a plan a program waits on: until it is
//! answered the agent reads no brief and answers nothing, so the person has to
//! be shown it as a card (M9.5). Surfacing one means writing a hook into the
//! program's own config file, which only an adapter for that program can do.
//!
//! The schema in [`crate::spec::AskHookSpec`] says *what* a program needs
//! (which config file, which shape, which events); this module is the trait the
//! adapters implement and the two installers that write the hook's config.
//!
//! # Never the person's global config
//!
//! Shepherdr installs its hooks into `~/.claude/settings.json` and
//! `~/.codex/hooks.json` (v3.1.34, commit `a3f327a`). shprd does not: a task's
//! hook belongs to the task, so it goes into the task's worktree — Claude Code
//! reads `.claude/settings.local.json` from the project it runs in, and Codex
//! reads a trusted project's `.codex/hooks.json` plus its `.codex/config.toml`.
//! The harness files name those worktree-relative paths, and every installer
//! refuses a `~/...` target rather than touching the person's own setup.
//!
//! # Borrowed
//!
//! The hook's config shape is adapted from Shepherdr (MIT):
//! <https://github.com/eliasstravik/shepherdr>, `bin/shepherdr` (`HOOKS`) and
//! `mods/chat/ask-hook.sh`, tag v3.1.34, commit `a3f327a`. Copyright (c) 2026
//! Elias Stravik. See `THIRD_PARTY_NOTICES.md`.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Map, Value, json};

use crate::spec::{AskHookConfig, AskHookSpec};

/// Where the ask hook lives on this machine, and where the program runs.
#[derive(Debug, Clone, Copy)]
pub struct AskHookPaths<'a> {
    /// The ask hook executable the installer writes.
    pub hook: &'a Path,
    /// The task's worktree: the only tree an installer may write into.
    pub worktree: &'a Path,
    /// The shprd binary the hook script runs.
    pub binary: &'a Path,
}

impl<'a> AskHookPaths<'a> {
    /// The paths an install runs with, for a hook executable at `hook`.
    pub fn new(hook: &'a Path, worktree: &'a Path, binary: &'a Path) -> Self {
        Self {
            hook,
            worktree,
            binary,
        }
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

/// Install a program's ask hook into its own worktree-local config file.
///
/// An installer is handed the hook's path, the worktree and the binary to call:
/// the program's config file holds one command per event, and the adapter
/// answers the file's exact shape from [`crate::spec::AskHookConfig`]. The
/// default [`Self::install`] touches nothing and reports
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

/// The installer every harness without one gets: it reads no file and writes
/// none. Pi and OpenCode have no hook shprd can install yet, so they fall back
/// to screen detection.
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

/// Claude Code's project-local settings file (`.claude/settings.local.json`).
#[derive(Debug, Clone, Copy)]
pub struct ClaudeAskHook<'a> {
    spec: &'a AskHookSpec,
    program: &'a str,
}

impl<'a> ClaudeAskHook<'a> {
    /// The installer for Claude Code's ask hook.
    pub fn new(spec: &'a AskHookSpec, program: &'a str) -> Self {
        Self { spec, program }
    }
}

impl AskHookInstaller for ClaudeAskHook<'_> {
    fn spec(&self) -> &AskHookSpec {
        self.spec
    }

    fn install(&self, paths: &AskHookPaths<'_>) -> Result<AskHookInstall, AskHookError> {
        if !self.available() {
            return Ok(AskHookInstall::Unsupported);
        }
        let command = self.command(paths).ok_or(AskHookError::Incomplete)?;
        // The config target is checked before anything is written, so a spec
        // that names the person's global config leaves the worktree untouched.
        let config = config_path(self.spec, paths.worktree)?;
        write_hook_script(paths, self.program)?;
        install_into_json(&config, self.spec, &command)
    }
}

/// Codex's project hooks file (`.codex/hooks.json`) and its project config.
#[derive(Debug, Clone, Copy)]
pub struct CodexAskHook<'a> {
    spec: &'a AskHookSpec,
    program: &'a str,
}

impl<'a> CodexAskHook<'a> {
    /// The installer for Codex's ask hook.
    pub fn new(spec: &'a AskHookSpec, program: &'a str) -> Self {
        Self { spec, program }
    }
}

impl AskHookInstaller for CodexAskHook<'_> {
    fn spec(&self) -> &AskHookSpec {
        self.spec
    }

    fn install(&self, paths: &AskHookPaths<'_>) -> Result<AskHookInstall, AskHookError> {
        if !self.available() {
            return Ok(AskHookInstall::Unsupported);
        }
        let command = self.command(paths).ok_or(AskHookError::Incomplete)?;
        let config = config_path(self.spec, paths.worktree)?;
        write_hook_script(paths, self.program)?;
        let installed = install_into_json(&config, self.spec, &command)?;
        // Codex only reads hooks when `hooks = true` is in its config. The
        // project file is the worktree's own copy, never the person's.
        let enabled = enable_codex_hooks(paths.worktree)?;
        if installed == AskHookInstall::AlreadyThere && !enabled {
            return Ok(AskHookInstall::AlreadyThere);
        }
        Ok(AskHookInstall::Added)
    }
}

/// The installer a loaded harness hands out, chosen by its config format.
pub enum HarnessAskHook<'a> {
    /// A program with no hook.
    None(NoAskHook<'a>),
    /// Claude Code's project settings.
    Claude(ClaudeAskHook<'a>),
    /// Codex's project hooks.
    Codex(CodexAskHook<'a>),
}

impl<'a> HarnessAskHook<'a> {
    /// The installer for `spec`, whose program id is `program`.
    pub fn for_spec(spec: &'a AskHookSpec, program: &'a str) -> Self {
        match spec.format {
            AskHookConfig::ClaudeSettings => Self::Claude(ClaudeAskHook::new(spec, program)),
            AskHookConfig::CodexHooks => Self::Codex(CodexAskHook::new(spec, program)),
            AskHookConfig::Unsupported => Self::None(NoAskHook::new(spec)),
        }
    }
}

impl AskHookInstaller for HarnessAskHook<'_> {
    fn spec(&self) -> &AskHookSpec {
        match self {
            Self::None(installer) => installer.spec(),
            Self::Claude(installer) => installer.spec(),
            Self::Codex(installer) => installer.spec(),
        }
    }

    fn install(&self, paths: &AskHookPaths<'_>) -> Result<AskHookInstall, AskHookError> {
        match self {
            Self::None(installer) => installer.install(paths),
            Self::Claude(installer) => installer.install(paths),
            Self::Codex(installer) => installer.install(paths),
        }
    }
}

/// An error while installing a program's ask hook.
#[derive(Debug, thiserror::Error)]
pub enum AskHookError {
    /// A file could not be read.
    #[error("failed to read the ask hook config {path}: {source}")]
    Read {
        /// The file that failed.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// A file could not be written.
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
    /// The harness says it supports a hook but not how to install it.
    #[error("the harness names no ask hook command")]
    Incomplete,
    /// The harness names a config file outside the task's worktree.
    #[error(
        "the ask hook config {config:?} is not inside the task's worktree; shprd never writes a person's global config"
    )]
    OutsideWorktree {
        /// The path the harness named.
        config: String,
    },
}

/// The hooks a task's worktree-local config file gets.
///
/// The shape is Shepherdr's `HOOKS` entry (v3.1.34): one array per event, each
/// entry a list of commands with a type and a timeout.
fn install_into_json(
    path: &Path,
    spec: &AskHookSpec,
    command: &str,
) -> Result<AskHookInstall, AskHookError> {
    let mut root = match fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Value::Object(Map::new()),
        Ok(text) => serde_json::from_str::<Value>(&text).map_err(|err| AskHookError::Invalid {
            path: path.to_path_buf(),
            format: spec.format_name(),
            reason: err.to_string(),
        })?,
        Err(err) if err.kind() == io::ErrorKind::NotFound => Value::Object(Map::new()),
        Err(source) => {
            return Err(AskHookError::Read {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let Some(root) = root.as_object_mut() else {
        return Err(AskHookError::Invalid {
            path: path.to_path_buf(),
            format: spec.format_name(),
            reason: "the config is not a JSON object".to_string(),
        });
    };
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()));
    let Some(hooks) = hooks.as_object_mut() else {
        return Err(AskHookError::Invalid {
            path: path.to_path_buf(),
            format: spec.format_name(),
            reason: "\"hooks\" is not an object".to_string(),
        });
    };
    let timeout = spec.timeout_seconds.unwrap_or(1800);
    let mut changed = false;
    for event in &spec.events {
        let entries = hooks
            .entry(event.event.clone())
            .or_insert_with(|| Value::Array(Vec::new()));
        let Some(entries) = entries.as_array_mut() else {
            return Err(AskHookError::Invalid {
                path: path.to_path_buf(),
                format: spec.format_name(),
                reason: format!("hooks.{} is not an array", event.event),
            });
        };
        let already = entries.iter().any(|entry| {
            entry
                .get("hooks")
                .and_then(Value::as_array)
                .is_some_and(|list| {
                    list.iter()
                        .any(|hook| hook.get("command").and_then(Value::as_str) == Some(command))
                })
        });
        if already {
            continue;
        }
        let mut entry = Map::new();
        if let Some(matcher) = &event.matcher {
            entry.insert("matcher".to_string(), json!(matcher));
        }
        entry.insert(
            "hooks".to_string(),
            json!([{
                "type": "command",
                "command": command,
                "timeout": timeout,
            }]),
        );
        entries.push(Value::Object(entry));
        changed = true;
    }
    if !changed {
        return Ok(AskHookInstall::AlreadyThere);
    }
    let bytes = serde_json::to_vec_pretty(&Value::Object(root.clone())).map_err(|err| {
        AskHookError::Invalid {
            path: path.to_path_buf(),
            format: spec.format_name(),
            reason: err.to_string(),
        }
    })?;
    write_atomic(path, &bytes)?;
    Ok(AskHookInstall::Added)
}

/// Writes the hook script beside the program's config, unless it is there.
///
/// The script is what the config's command runs: `sh '<script>'`. It gives the
/// subcommand the program name and leaves the payload on stdin untouched.
fn write_hook_script(paths: &AskHookPaths<'_>, program: &str) -> Result<(), AskHookError> {
    if paths.hook.starts_with("~") || !paths.hook.starts_with(paths.worktree) {
        return Err(AskHookError::OutsideWorktree {
            config: paths.hook.display().to_string(),
        });
    }
    let script = format!(
        "#!/bin/sh\n# shprd's ask hook for {program} (M9.5): writes the ask and waits for an answer.\nexec {} ask hook --program {} \"$@\"\n",
        shell_quote(&paths.binary.display().to_string()),
        shell_quote(program),
    );
    let wanted = script.as_bytes();
    match fs::read(paths.hook) {
        Ok(existing) if existing == wanted => return Ok(()),
        Ok(_) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(AskHookError::Read {
                path: paths.hook.to_path_buf(),
                source,
            });
        }
    }
    write_atomic(paths.hook, wanted)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::Permissions::from_mode(0o755);
        fs::set_permissions(paths.hook, mode).map_err(|source| AskHookError::Write {
            path: paths.hook.to_path_buf(),
            source,
        })?;
    }
    Ok(())
}

/// Codex reads hooks only when its config says so; make the worktree's own copy
/// say it, and nothing outside the worktree.
fn enable_codex_hooks(worktree: &Path) -> Result<bool, AskHookError> {
    let path = worktree.join(".codex").join("config.toml");
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => String::new(),
        Err(source) => return Err(AskHookError::Read { path, source }),
    };
    let enabled = text.lines().any(|line| {
        let line = line.trim();
        line == "hooks = true" || line == "hooks=true"
    });
    if enabled {
        return Ok(false);
    }
    let mut next = text;
    if !next.is_empty() && !next.ends_with('\n') {
        next.push('\n');
    }
    next.push_str("hooks = true\n");
    write_atomic(&path, next.as_bytes())?;
    Ok(true)
}

/// The config file a harness names, joined to the worktree.
///
/// A home-relative path is refused: the person's global config is not a task's
/// to edit. So is anything absolute or climbing out with `..`.
fn config_path(spec: &AskHookSpec, worktree: &Path) -> Result<PathBuf, AskHookError> {
    let raw = spec.config.as_deref().ok_or(AskHookError::Incomplete)?;
    if raw.starts_with('~') {
        return Err(AskHookError::OutsideWorktree {
            config: raw.to_string(),
        });
    }
    let relative = Path::new(raw);
    if relative.is_absolute()
        || relative.components().any(|part| {
            matches!(
                part,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(AskHookError::OutsideWorktree {
            config: raw.to_string(),
        });
    }
    Ok(worktree.join(relative))
}

/// A path as one shell word.
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// Writes `bytes` to `path` through a temporary file and a rename.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), AskHookError> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir).map_err(|source| AskHookError::Write {
        path: dir.to_path_buf(),
        source,
    })?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("ask");
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let temporary = path.with_file_name(format!(".{name}.{}.{n}.tmp", std::process::id()));
    let wrote = (|| -> io::Result<()> {
        fs::write(&temporary, bytes)?;
        fs::rename(&temporary, path)
    })();
    if let Err(source) = wrote {
        let _ = fs::remove_file(&temporary);
        return Err(AskHookError::Write {
            path: path.to_path_buf(),
            source,
        });
    }
    Ok(())
}

impl AskHookSpec {
    /// The config shape's name, for an error.
    fn format_name(&self) -> &'static str {
        match self.format {
            AskHookConfig::Unsupported => "unsupported",
            AskHookConfig::ClaudeSettings => "claude_settings",
            AskHookConfig::CodexHooks => "codex_hooks",
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::path::{Path, PathBuf};

    use super::{
        AskHookInstall, AskHookInstaller, AskHookPaths, ClaudeAskHook, CodexAskHook, NoAskHook,
    };
    use crate::spec::{AskEvent, AskHookConfig, AskHookSpec, AskKind};

    fn spec(format: AskHookConfig, config: &str) -> AskHookSpec {
        AskHookSpec {
            supported: true,
            config: Some(config.to_owned()),
            format,
            command: Some("sh '{hook}'".to_owned()),
            events: vec![AskEvent {
                kind: AskKind::Permission,
                event: "PermissionRequest".to_owned(),
                matcher: None,
            }],
            timeout_seconds: Some(1800),
        }
    }

    fn claude_spec() -> AskHookSpec {
        spec(AskHookConfig::ClaudeSettings, ".claude/settings.local.json")
    }

    fn codex_spec() -> AskHookSpec {
        spec(AskHookConfig::CodexHooks, ".codex/hooks.json")
    }

    fn paths(worktree: &Path) -> (PathBuf, PathBuf) {
        let hook = worktree.join(".shprd").join("ask-hook.sh");
        let binary = PathBuf::from("/opt/shprd/bin/shprd");
        (hook.clone(), binary)
    }

    #[test]
    fn the_default_installer_touches_nothing() {
        let spec = claude_spec();
        let installer = NoAskHook::new(&spec);
        let dir = tempfile::tempdir().expect("temp dir");
        let (hook, binary) = paths(dir.path());
        let paths = AskHookPaths::new(&hook, dir.path(), &binary);
        assert!(installer.available());
        assert!(matches!(
            installer.install(&paths),
            Ok(AskHookInstall::Unsupported)
        ));
        assert!(!hook.exists());
        assert!(!dir.path().join(".claude").exists());
    }

    #[test]
    fn the_command_fills_the_hook_path_in() {
        let spec = claude_spec();
        let installer = NoAskHook::new(&spec);
        let dir = tempfile::tempdir().expect("temp dir");
        let (hook, binary) = paths(dir.path());
        let paths = AskHookPaths::new(&hook, dir.path(), &binary);
        assert_eq!(
            installer.command(&paths).as_deref(),
            Some(format!("sh '{}'", hook.display()).as_str())
        );
    }

    #[test]
    fn a_program_with_no_hook_has_no_command() {
        let spec = AskHookSpec::default();
        let installer = NoAskHook::new(&spec);
        let dir = tempfile::tempdir().expect("temp dir");
        let (hook, binary) = paths(dir.path());
        let paths = AskHookPaths::new(&hook, dir.path(), &binary);
        assert!(!installer.available());
        assert_eq!(installer.command(&paths), None);
    }

    #[test]
    fn claude_gets_the_hook_in_its_project_settings() {
        let dir = tempfile::tempdir().expect("temp dir");
        let spec = claude_spec();
        let installer = ClaudeAskHook::new(&spec, "claude");
        let (hook, binary) = paths(dir.path());
        let paths = AskHookPaths::new(&hook, dir.path(), &binary);
        assert_eq!(
            installer.install(&paths).expect("install"),
            AskHookInstall::Added
        );
        let settings = dir.path().join(".claude/settings.local.json");
        let text = std::fs::read_to_string(&settings).expect("read settings");
        let value: serde_json::Value = serde_json::from_str(&text).expect("parse");
        let entry = &value["hooks"]["PermissionRequest"][0]["hooks"][0];
        assert_eq!(entry["type"], "command");
        assert_eq!(entry["timeout"], 1800);
        let command = entry["command"].as_str().expect("command");
        assert!(
            command.contains(hook.to_str().expect("hook path")),
            "{command}"
        );
        let script = std::fs::read_to_string(&hook).expect("read hook script");
        assert!(script.contains("ask hook --program 'claude'"), "{script}");
        assert!(script.contains("/opt/shprd/bin/shprd"), "{script}");
        // Installing twice leaves one entry, not two.
        assert_eq!(
            installer.install(&paths).expect("install again"),
            AskHookInstall::AlreadyThere
        );
        let text = std::fs::read_to_string(&settings).expect("read settings");
        assert_eq!(
            text.matches("PermissionRequest").count(),
            1,
            "one event entry: {text}"
        );
        // Nothing outside the worktree was written.
        assert!(!dir.path().join("home").exists());
    }

    #[test]
    fn claude_keeps_what_else_the_settings_file_held() {
        let dir = tempfile::tempdir().expect("temp dir");
        let settings = dir.path().join(".claude/settings.local.json");
        std::fs::create_dir_all(settings.parent().expect("parent")).expect("mkdir");
        std::fs::write(
            &settings,
            r#"{"permissions":{"allow":["Bash(cargo test:*)"]},"hooks":{"PostToolUse":[]}}"#,
        )
        .expect("seed");
        let spec = claude_spec();
        let installer = ClaudeAskHook::new(&spec, "claude");
        let (hook, binary) = paths(dir.path());
        let paths = AskHookPaths::new(&hook, dir.path(), &binary);
        installer.install(&paths).expect("install");
        let text = std::fs::read_to_string(&settings).expect("read");
        let value: serde_json::Value = serde_json::from_str(&text).expect("parse");
        assert_eq!(
            value["permissions"]["allow"][0], "Bash(cargo test:*)",
            "{text}"
        );
        assert!(value["hooks"]["PostToolUse"].is_array(), "{text}");
        assert!(value["hooks"]["PermissionRequest"].is_array(), "{text}");
    }

    #[test]
    fn codex_gets_the_hook_and_its_project_config_turns_hooks_on() {
        let dir = tempfile::tempdir().expect("temp dir");
        let spec = codex_spec();
        let installer = CodexAskHook::new(&spec, "codex");
        let (hook, binary) = paths(dir.path());
        let paths = AskHookPaths::new(&hook, dir.path(), &binary);
        assert_eq!(
            installer.install(&paths).expect("install"),
            AskHookInstall::Added
        );
        let hooks = dir.path().join(".codex/hooks.json");
        let text = std::fs::read_to_string(&hooks).expect("read hooks");
        let value: serde_json::Value = serde_json::from_str(&text).expect("parse");
        let command = value["hooks"]["PermissionRequest"][0]["hooks"][0]["command"]
            .as_str()
            .expect("command");
        assert!(
            command.contains(hook.to_str().expect("hook path")),
            "{command}"
        );
        let config =
            std::fs::read_to_string(dir.path().join(".codex/config.toml")).expect("read config");
        assert!(config.contains("hooks = true"), "{config}");
        // A second install changes nothing.
        assert_eq!(
            installer.install(&paths).expect("install again"),
            AskHookInstall::AlreadyThere
        );
    }

    #[test]
    fn codex_keeps_the_rest_of_a_project_config() {
        let dir = tempfile::tempdir().expect("temp dir");
        let config = dir.path().join(".codex/config.toml");
        std::fs::create_dir_all(config.parent().expect("parent")).expect("mkdir");
        std::fs::write(&config, "model = \"gpt-6\"\n").expect("seed");
        let spec = codex_spec();
        let installer = CodexAskHook::new(&spec, "codex");
        let (hook, binary) = paths(dir.path());
        let paths = AskHookPaths::new(&hook, dir.path(), &binary);
        installer.install(&paths).expect("install");
        let text = std::fs::read_to_string(&config).expect("read");
        assert!(text.contains("model = \"gpt-6\""), "{text}");
        assert!(text.contains("hooks = true"), "{text}");
    }

    #[test]
    fn an_installer_never_writes_the_persons_global_config() {
        let dir = tempfile::tempdir().expect("temp dir");
        let worktree = dir.path().join("worktree");
        std::fs::create_dir_all(&worktree).expect("worktree");
        for spec in [
            spec(AskHookConfig::ClaudeSettings, "~/.claude/settings.json"),
            spec(AskHookConfig::CodexHooks, "~/.codex/hooks.json"),
            spec(AskHookConfig::ClaudeSettings, "/tmp/elsewhere.json"),
            spec(AskHookConfig::ClaudeSettings, "../escape.json"),
        ] {
            let (hook, binary) = paths(&worktree);
            let paths = AskHookPaths::new(&hook, &worktree, &binary);
            match super::HarnessAskHook::for_spec(&spec, "claude").install(&paths) {
                Err(super::AskHookError::OutsideWorktree { .. }) => {}
                other => panic!("expected OutsideWorktree, got {other:?}"),
            }
            // A refused target writes nothing at all, not even the script.
            assert!(!worktree.join(".shprd").exists());
        }
        assert!(!worktree.join(".claude").exists());
        assert!(!worktree.join(".codex").exists());
    }
}
