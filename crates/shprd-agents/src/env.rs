//! The clean launch environment shprd hands to an agent CLI (M3.5).
//!
//! Two things must be true of every CLI shprd starts:
//!
//! 1. A worker or reviewer cannot authenticate to a remote, so it cannot push
//!    or fetch credentials on a routine path. Only the shprd engine pushes.
//! 2. Leftovers from another orchestrator (omnigent, `og`) cannot change how
//!    the CLI behaves, because shprd owns the whole environment it starts.
//!
//! The result is a [`LaunchEnv`]: a small diff over the caller's base
//! environment, which a supervisor applies with [`LaunchEnv::apply_to`] or
//! translates into `Command` calls with [`LaunchEnv::as_pairs`].
//!
//! The rule that an agent never inherits a parent Claude Code session is
//! borrowed from Shepherdr (MIT), `mods/tasks/bin/herdr_task/launch.py`
//! (`CLEAN_ENV`), tag v3.1.34, commit a3f327a.
//! https://github.com/eliasstravik/shepherdr

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

/// Who a launched CLI acts as, which decides how much the environment allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The engine itself, in a pane of its own: the watchdog that turns a
    /// reviewer's findings into a verdict and hands off a dry worker.
    ///
    /// It is not an agent and holds no session, but it acts as the engine, so
    /// it gets the engine's environment rather than an agent's.
    Engine,
    /// The engine's own agent: may push, so it keeps remote credentials.
    Orchestrator,
    /// A worker on one task: no remote credentials, no interactive prompts.
    Worker,
    /// A read-only reviewer: no remote credentials, no interactive prompts.
    Reviewer,
}

impl Role {
    /// Whether this role may hold remote (push) credentials.
    fn may_push(self) -> bool {
        matches!(self, Self::Engine | Self::Orchestrator)
    }
}

/// Variables that let a CLI authenticate to, or prompt for, a remote.
///
/// Removed for every role except the engine's own processes
/// ([`Role::Engine`] and [`Role::Orchestrator`]).
const CREDENTIAL_VARS: &[&str] = &[
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
    "GIT_ASKPASS",
    "SSH_ASKPASS",
    "SSH_AUTH_SOCK",
];

/// Exact variable names left behind by another orchestrator.
///
/// Removed for every role. The `OPENCODE_*` entries are named exactly rather
/// than by prefix because the rest of the `OPENCODE_` namespace is the CLI's
/// own business.
const FOREIGN_VARS: &[&str] = &[
    "OPENCODE_DISABLE_EXTERNAL_SKILLS",
    "OPENCODE_DISABLE_CLAUDE_CODE",
    "OPENCODE_CONFIG_DIR",
    "OPENCODE_CONFIG",
    "OPENCODE_CONFIG_CONTENT",
];

/// Prefixes reserved by other orchestrators; anything under them is removed.
const FOREIGN_PREFIXES: &[&str] = &["OMNIGENT_", "OG_"];

/// Environment variable naming the home directory shprd keeps its config,
/// briefs and worktrees in, as the engine resolved it.
const HOME_ENV: &str = "SHPRD_HOME";

/// Environment variable naming the data directory holding `harness/` and
/// `skills/`, as the engine resolved it.
const DATA_ENV: &str = "SHPRD_DATA_DIR";

/// Names a parent Claude Code session leaves in the environment, exactly.
///
/// A terminal holder started from inside Claude Code hands every CLI it starts
/// these: with them Claude Code saves no transcript and skips its folder-trust
/// prompt, so an agent shprd starts never inherits them (Shepherdr's
/// `CLEAN_ENV`, `mods/tasks/bin/herdr_task/launch.py`, SHEP-86).
const PARENT_CLAUDE_EXACT: &[&str] = &["CLAUDECODE", "CLAUDE_PID"];

/// The prefix a parent Claude Code session's own variables carry.
const PARENT_CLAUDE_PREFIX: &str = "CLAUDE_CODE_";

/// `CLAUDE_CODE_*` suffixes that are the person's own configuration, kept.
///
/// `CLAUDE_CODE_USE_BEDROCK`, `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC` and
/// friends set how Claude Code behaves, not which session it is in.
const PARENT_CLAUDE_KEEP: &[&str] = &[
    "USE_", "SKIP_", "DISABLE_", "ENABLE_", "MAX_", "OAUTH_", "CLIENT_", "API_",
];

/// What shprd knows about the launch and wants the CLI to inherit.
#[derive(Debug, Clone)]
pub struct EnvContext {
    /// The agent identity, exposed as `SHPRD_ACTOR`.
    pub actor: String,
    /// The task id, exposed as `SHPRD_TASK` when the launch is for a task.
    pub task: Option<String>,
    /// The run id, exposed as `SHPRD_RUN` when the launch is part of a run.
    pub run: Option<String>,
    /// The checkout the CLI works in, exposed as `SHPRD_REPO`.
    pub repo: PathBuf,
    /// The home directory the engine keeps its config, briefs and worktrees in,
    /// exposed as `SHPRD_HOME` when the engine has one resolved.
    pub home: Option<PathBuf>,
    /// The data directory holding `harness/` and `skills/`, exposed as
    /// `SHPRD_DATA_DIR` when the engine resolved one.
    pub data: Option<PathBuf>,
}

/// A diff over a base environment: names to set and names to drop.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchEnv {
    /// Variables to set, with the value to set them to.
    pub set: BTreeMap<String, String>,
    /// Variables to drop from the inherited environment.
    pub remove: BTreeSet<String>,
}

impl LaunchEnv {
    /// Apply this diff to a base environment, returning the environment to
    /// hand the CLI.
    ///
    /// `set` wins over `remove`: a name in both lists is set, not dropped.
    pub fn apply_to(&self, base: &BTreeMap<String, String>) -> BTreeMap<String, String> {
        let mut env = base.clone();
        for name in &self.remove {
            env.remove(name);
        }
        for (name, value) in &self.set {
            env.insert(name.clone(), value.clone());
        }
        env
    }

    /// The diff as pairs for `std::process::Command`: `Some` means set the
    /// variable to that value, `None` means remove it from the environment.
    ///
    /// Pairs are sorted by name so call sites and tests see a stable order.
    pub fn as_pairs(&self) -> Vec<(String, Option<String>)> {
        let mut pairs: Vec<(String, Option<String>)> = Vec::new();
        for (name, value) in &self.set {
            pairs.push((name.clone(), Some(value.clone())));
        }
        for name in &self.remove {
            if !self.set.contains_key(name) {
                pairs.push((name.clone(), None));
            }
        }
        pairs.sort();
        pairs
    }

    /// Names dropped by this diff, for doctor output.
    ///
    /// Names only: values are never reported, because they may be secrets.
    pub fn removed_names(&self) -> Vec<String> {
        self.remove
            .iter()
            .filter(|name| !self.set.contains_key(*name))
            .cloned()
            .collect()
    }
}

/// Build the launch environment diff for one CLI launch.
///
/// Rules, in order: workers and reviewers lose every remote credential and
/// get git's interactive prompts and credential helper switched off; every
/// role loses another orchestrator's leftovers; every role gains the
/// `SHPRD_*` identity variables. A variable shprd sets is never also removed.
pub fn launch_env(base: &BTreeMap<String, String>, role: Role, ctx: &EnvContext) -> LaunchEnv {
    let mut set: BTreeMap<String, String> = BTreeMap::new();
    let mut remove: BTreeSet<String> = BTreeSet::new();

    if !role.may_push() {
        for name in CREDENTIAL_VARS {
            remove.insert((*name).to_string());
        }
        set.insert("GIT_TERMINAL_PROMPT".to_string(), "0".to_string());
        blank_credential_helper(base, &mut set);
    }

    for name in FOREIGN_VARS {
        remove.insert((*name).to_string());
    }
    for name in base.keys() {
        if FOREIGN_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
        {
            remove.insert(name.clone());
        }
    }
    for name in base.keys() {
        if is_parent_claude_marker(name) {
            remove.insert(name.clone());
        }
    }
    remove.retain(|name| !is_protected(name));

    set.insert("SHPRD_ACTOR".to_string(), ctx.actor.clone());
    set.insert("SHPRD_REPO".to_string(), ctx.repo.display().to_string());
    if let Some(task) = &ctx.task {
        set.insert("SHPRD_TASK".to_string(), task.clone());
    }
    if let Some(run) = &ctx.run {
        set.insert("SHPRD_RUN".to_string(), run.clone());
    }
    set_engine_dirs(base, ctx, &mut set);

    LaunchEnv { set, remove }
}

/// Hand the engine's home and data directory to the CLI it starts.
///
/// An agent runs `shprd` itself — `task status`, the ask hook, the hand-in — and
/// has to resolve the same config, briefs and harness table the service does. A
/// GUI-started service or a `--home` flag leaves no `SHPRD_HOME` in the child's
/// inherited environment, so the engine's resolved directory is set explicitly;
/// a home or data directory the base environment already carries is passed
/// through, so a caller that resolves neither still forwards the engine's. A
/// launch whose engine has neither set gets neither, and the CLI falls back to
/// `$HOME` as before.
fn set_engine_dirs(
    base: &BTreeMap<String, String>,
    ctx: &EnvContext,
    set: &mut BTreeMap<String, String>,
) {
    for (name, resolved) in [
        (HOME_ENV, ctx.home.as_deref()),
        (DATA_ENV, ctx.data.as_deref()),
    ] {
        let value = match resolved {
            Some(dir) => Some(dir.display().to_string()),
            None => base.get(name).filter(|value| !value.is_empty()).cloned(),
        };
        if let Some(value) = value {
            set.insert(name.to_string(), value);
        }
    }
}

/// Append a `credential.helper = ""` entry to git's command-scoped config.
///
/// The empty value resets the helper list, so no helper from the user's git
/// config or from the inherited `GIT_CONFIG_*` entries can supply a password.
/// Existing entries are kept and the new one is appended after them, because
/// the last helper entry wins and the base env may carry its own config.
fn blank_credential_helper(base: &BTreeMap<String, String>, set: &mut BTreeMap<String, String>) {
    let existing = base
        .get("GIT_CONFIG_COUNT")
        .and_then(|count| count.parse::<usize>().ok())
        .unwrap_or(0);
    set.insert(
        format!("GIT_CONFIG_KEY_{existing}"),
        "credential.helper".to_string(),
    );
    set.insert(format!("GIT_CONFIG_VALUE_{existing}"), String::new());
    set.insert("GIT_CONFIG_COUNT".to_string(), (existing + 1).to_string());
}

/// Whether a name marks the session a parent Claude Code runs.
///
/// `CLAUDECODE`, `CLAUDE_PID` and `CLAUDE_CODE_*` are markers; a `CLAUDE_CODE_*`
/// name under one of [`PARENT_CLAUDE_KEEP`] is configuration and stays, and
/// `CLAUDE_CONFIG_DIR` is neither.
fn is_parent_claude_marker(name: &str) -> bool {
    if PARENT_CLAUDE_EXACT.contains(&name) {
        return true;
    }
    match name.strip_prefix(PARENT_CLAUDE_PREFIX) {
        Some(rest) => !PARENT_CLAUDE_KEEP.iter().any(|keep| rest.starts_with(keep)),
        None => false,
    }
}

/// Whether a name is off limits for removal regardless of any prefix rule.
fn is_protected(name: &str) -> bool {
    const PROTECTED: &[&str] = &["HOME", "PATH", "USER", "SHELL", "TERM", "LANG"];
    PROTECTED.contains(&name)
        || name.starts_with("LC_")
        || name.starts_with("HERDR_")
        || name.starts_with("TUIOS_")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A base environment that carries every hazard the rules care about.
    fn base_env() -> BTreeMap<String, String> {
        let mut base = BTreeMap::new();
        for (name, value) in [
            ("PATH", "/usr/bin:/bin"),
            ("HOME", "/home/dev"),
            ("USER", "dev"),
            ("SHELL", "/bin/zsh"),
            ("TERM", "xterm-256color"),
            ("LANG", "en_US.UTF-8"),
            ("LC_ALL", "en_US.UTF-8"),
            ("GH_TOKEN", "gh-secret"),
            ("GITHUB_TOKEN", "github-secret"),
            ("GH_ENTERPRISE_TOKEN", "gh-enterprise"),
            ("GITHUB_ENTERPRISE_TOKEN", "github-enterprise"),
            ("GIT_ASKPASS", "/usr/bin/askpass"),
            ("SSH_ASKPASS", "/usr/bin/ssh-askpass"),
            ("SSH_AUTH_SOCK", "/tmp/ssh-agent.sock"),
            ("OMNIGENT_SESSION", "conv_123"),
            ("OG_PROFILE", "og"),
            ("OPENCODE_CONFIG", "/tmp/opencode.json"),
            ("OPENCODE_CONFIG_CONTENT", "{}"),
            ("OPENCODE_CONFIG_DIR", "/tmp/opencode"),
            ("OPENCODE_DISABLE_EXTERNAL_SKILLS", "1"),
            ("OPENCODE_DISABLE_CLAUDE_CODE", "1"),
            ("CLAUDECODE", "1"),
            ("CLAUDE_PID", "4242"),
            ("CLAUDE_CODE_CHILD_SESSION", "1"),
            ("CLAUDE_CODE_SESSION_ID", "fake-session"),
            ("CLAUDE_CODE_MESSAGING_TOKEN", "secret"),
            ("CLAUDE_CODE_USE_BEDROCK", "1"),
            ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1"),
            ("CLAUDE_CONFIG_DIR", "/home/dev/.claude"),
            ("HERDR_SESSION", "s1"),
            ("TUIOS_SOCKET", "/tmp/tuios.sock"),
        ] {
            base.insert(name.to_string(), value.to_string());
        }
        base
    }

    fn ctx() -> EnvContext {
        EnvContext {
            actor: "worker-7".to_string(),
            task: Some("M3.5".to_string()),
            run: Some("run-2".to_string()),
            repo: PathBuf::from("/repo"),
            home: None,
            data: None,
        }
    }

    const CREDENTIALS: &[&str] = CREDENTIAL_VARS;

    /// Test-local stand-in for `unwrap`, which the workspace lints deny.
    fn unpack<T, E: std::fmt::Debug>(value: Result<T, E>) -> T {
        match value {
            Ok(value) => value,
            Err(error) => panic!("command failed: {error:?}"),
        }
    }

    #[test]
    fn workers_and_reviewers_lose_remote_credentials() {
        let base = base_env();
        for role in [Role::Worker, Role::Reviewer] {
            let env = launch_env(&base, role, &ctx());
            let applied = env.apply_to(&base);
            for name in CREDENTIALS {
                assert!(!applied.contains_key(*name), "{role:?} kept {name}");
                assert!(env.removed_names().contains(&(*name).to_string()));
            }
            assert_eq!(
                applied.get("GIT_TERMINAL_PROMPT").map(String::as_str),
                Some("0")
            );
        }
    }

    #[test]
    fn orchestrator_keeps_credentials_but_loses_foreign_vars() {
        let base = base_env();
        let env = launch_env(&base, Role::Orchestrator, &ctx());
        let applied = env.apply_to(&base);
        for name in CREDENTIALS {
            assert_eq!(
                applied.get(*name),
                base.get(*name),
                "orchestrator lost {name}"
            );
        }
        assert!(!applied.contains_key("GIT_TERMINAL_PROMPT"));
        assert!(!applied.contains_key("GIT_CONFIG_COUNT"));
        for name in FOREIGN_VARS {
            assert!(!applied.contains_key(*name), "kept {name}");
        }
        assert!(!applied.contains_key("OMNIGENT_SESSION"));
        assert!(!applied.contains_key("OG_PROFILE"));
    }

    #[test]
    fn the_watchdog_gets_the_engines_environment() {
        let base = base_env();
        let engine = EnvContext {
            actor: "engine".to_string(),
            ..ctx()
        };
        let applied = launch_env(&base, Role::Engine, &engine).apply_to(&base);
        for name in CREDENTIALS {
            assert_eq!(
                applied.get(*name),
                base.get(*name),
                "the engine lost {name}"
            );
        }
        assert!(!applied.contains_key("GIT_TERMINAL_PROMPT"));
        assert_eq!(
            applied.get("SHPRD_ACTOR").map(String::as_str),
            Some("engine")
        );
    }

    #[test]
    fn a_parent_claude_code_session_is_not_inherited() {
        let base = base_env();
        for role in [
            Role::Engine,
            Role::Orchestrator,
            Role::Worker,
            Role::Reviewer,
        ] {
            let env = launch_env(&base, role, &ctx());
            let applied = env.apply_to(&base);
            for name in [
                "CLAUDECODE",
                "CLAUDE_PID",
                "CLAUDE_CODE_CHILD_SESSION",
                "CLAUDE_CODE_SESSION_ID",
                "CLAUDE_CODE_MESSAGING_TOKEN",
            ] {
                assert!(!applied.contains_key(name), "{role:?} kept {name}");
            }
            for name in [
                "CLAUDE_CODE_USE_BEDROCK",
                "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
                "CLAUDE_CONFIG_DIR",
            ] {
                assert_eq!(
                    applied.get(name),
                    base.get(name),
                    "{role:?} dropped the person's own {name}"
                );
            }
        }
    }

    #[test]
    fn foreign_vars_go_for_every_role() {
        let base = base_env();
        for role in [Role::Orchestrator, Role::Worker, Role::Reviewer] {
            let env = launch_env(&base, role, &ctx());
            for name in FOREIGN_VARS {
                assert!(
                    env.removed_names().contains(&(*name).to_string()),
                    "{role:?}: {name}"
                );
            }
            assert!(
                env.removed_names()
                    .contains(&"OMNIGENT_SESSION".to_string())
            );
            assert!(env.removed_names().contains(&"OG_PROFILE".to_string()));
        }
    }

    #[test]
    fn protected_and_multiplexer_vars_are_kept() {
        let base = base_env();
        for role in [Role::Orchestrator, Role::Worker, Role::Reviewer] {
            let env = launch_env(&base, role, &ctx());
            let applied = env.apply_to(&base);
            for name in ["HOME", "PATH", "USER", "SHELL", "TERM", "LANG", "LC_ALL"] {
                assert_eq!(applied.get(name), base.get(name), "{role:?} dropped {name}");
                assert!(!env.removed_names().contains(&name.to_string()));
            }
            for name in ["HERDR_SESSION", "TUIOS_SOCKET"] {
                assert_eq!(applied.get(name), base.get(name), "{role:?} dropped {name}");
            }
        }
    }

    #[test]
    fn identity_vars_are_set_for_every_role() {
        let base = base_env();
        for role in [Role::Orchestrator, Role::Worker, Role::Reviewer] {
            let applied = launch_env(&base, role, &ctx()).apply_to(&base);
            assert_eq!(
                applied.get("SHPRD_ACTOR").map(String::as_str),
                Some("worker-7")
            );
            assert_eq!(applied.get("SHPRD_REPO").map(String::as_str), Some("/repo"));
            assert_eq!(applied.get("SHPRD_TASK").map(String::as_str), Some("M3.5"));
            assert_eq!(applied.get("SHPRD_RUN").map(String::as_str), Some("run-2"));
        }
    }

    #[test]
    fn task_and_run_are_omitted_when_absent() {
        let base = base_env();
        let bare = EnvContext {
            actor: "orchestrator".to_string(),
            task: None,
            run: None,
            repo: PathBuf::from("/repo"),
            home: None,
            data: None,
        };
        let env = launch_env(&base, Role::Orchestrator, &bare);
        assert!(!env.set.contains_key("SHPRD_TASK"));
        assert!(!env.set.contains_key("SHPRD_RUN"));
    }

    #[test]
    fn the_engines_home_and_data_directory_are_handed_to_the_cli() {
        let base = base_env();
        let engine = EnvContext {
            home: Some(PathBuf::from("/srv/shprd-home")),
            data: Some(PathBuf::from("/srv/shprd-data")),
            ..ctx()
        };
        for role in [Role::Worker, Role::Reviewer] {
            let env = launch_env(&base, role, &engine);
            let applied = env.apply_to(&base);
            assert_eq!(
                applied.get("SHPRD_HOME").map(String::as_str),
                Some("/srv/shprd-home"),
                "{role:?}: a worker's own shprd must read the engine's config"
            );
            assert_eq!(
                applied.get("SHPRD_DATA_DIR").map(String::as_str),
                Some("/srv/shprd-data"),
                "{role:?}: a worker's own shprd must find the engine's harnesses"
            );
        }
    }

    #[test]
    fn a_home_the_engine_does_not_set_is_not_invented() {
        let base = base_env();
        let env = launch_env(&base, Role::Worker, &ctx());
        assert!(!env.set.contains_key("SHPRD_HOME"), "{:?}", env.set);
        assert!(!env.set.contains_key("SHPRD_DATA_DIR"), "{:?}", env.set);
        let applied = env.apply_to(&base);
        assert!(!applied.contains_key("SHPRD_HOME"));
        assert!(!applied.contains_key("SHPRD_DATA_DIR"));
    }

    #[test]
    fn an_inherited_home_is_passed_through() {
        // A caller that resolves neither still forwards what the engine's own
        // environment carries, so `SHPRD_HOME=<dir> shprd agent spawn` works the
        // way the variable means.
        let mut base = base_env();
        base.insert("SHPRD_HOME".to_string(), "/env/shprd-home".to_string());
        base.insert("SHPRD_DATA_DIR".to_string(), "/env/shprd-data".to_string());
        let applied = launch_env(&base, Role::Worker, &ctx()).apply_to(&base);
        assert_eq!(
            applied.get("SHPRD_HOME").map(String::as_str),
            Some("/env/shprd-home")
        );
        assert_eq!(
            applied.get("SHPRD_DATA_DIR").map(String::as_str),
            Some("/env/shprd-data")
        );
    }

    #[test]
    fn git_config_count_appends_after_existing_entries() {
        let mut base = base_env();
        base.insert("GIT_CONFIG_COUNT".to_string(), "1".to_string());
        base.insert(
            "GIT_CONFIG_KEY_0".to_string(),
            "credential.helper".to_string(),
        );
        base.insert("GIT_CONFIG_VALUE_0".to_string(), "osxkeychain".to_string());
        let env = launch_env(&base, Role::Worker, &ctx());
        assert_eq!(
            env.set.get("GIT_CONFIG_COUNT").map(String::as_str),
            Some("2")
        );
        assert_eq!(
            env.set.get("GIT_CONFIG_KEY_1").map(String::as_str),
            Some("credential.helper")
        );
        assert_eq!(
            env.set.get("GIT_CONFIG_VALUE_1").map(String::as_str),
            Some("")
        );
        assert_eq!(
            env.set.get("GIT_CONFIG_KEY_0").map(String::as_str),
            None,
            "the existing entry must not be rewritten"
        );
        let applied = env.apply_to(&base);
        assert_eq!(
            applied.get("GIT_CONFIG_COUNT").map(String::as_str),
            Some("2")
        );
        assert_eq!(
            applied.get("GIT_CONFIG_VALUE_0").map(String::as_str),
            Some("osxkeychain")
        );
    }

    #[test]
    fn git_config_count_starts_at_zero_without_base_entries() {
        let base = base_env();
        let env = launch_env(&base, Role::Reviewer, &ctx());
        assert_eq!(
            env.set.get("GIT_CONFIG_COUNT").map(String::as_str),
            Some("1")
        );
        assert_eq!(
            env.set.get("GIT_CONFIG_KEY_0").map(String::as_str),
            Some("credential.helper")
        );
        assert_eq!(
            env.set.get("GIT_CONFIG_VALUE_0").map(String::as_str),
            Some("")
        );
    }

    #[test]
    fn as_pairs_marks_sets_and_removals() {
        let base = base_env();
        let env = launch_env(&base, Role::Worker, &ctx());
        let pairs = env.as_pairs();
        assert!(
            pairs.windows(2).all(|w| w[0].0 <= w[1].0),
            "pairs are sorted"
        );
        assert!(pairs.contains(&("SHPRD_ACTOR".to_string(), Some("worker-7".to_string()))));
        assert!(pairs.contains(&("GH_TOKEN".to_string(), None)));
        assert!(pairs.contains(&("GIT_CONFIG_VALUE_0".to_string(), Some(String::new()))));
        assert_eq!(pairs.len(), env.set.len() + env.remove.len());
        assert_eq!(
            env.removed_names(),
            env.remove.iter().cloned().collect::<Vec<_>>()
        );
    }

    /// Real-git check: the applied worker environment leaves git with no
    /// credential helper, so no helper can answer a push or fetch prompt.
    #[test]
    fn worker_env_blanks_the_git_credential_helper() {
        use std::process::Command;

        let git_works = Command::new("git")
            .arg("--version")
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false);
        if !git_works {
            eprintln!("skipped: git is not on PATH");
            return;
        }

        let dir = unpack(tempfile::tempdir());
        let repo = dir.path();
        let init = Command::new("git")
            .arg("init")
            .arg("--quiet")
            .arg(repo)
            .output();
        assert!(init.map(|out| out.status.success()).unwrap_or(false));

        let mut base: BTreeMap<String, String> = BTreeMap::new();
        base.insert(
            "PATH".to_string(),
            "/usr/bin:/bin:/usr/local/bin".to_string(),
        );
        base.insert("HOME".to_string(), repo.display().to_string());
        base.insert("GIT_CONFIG_COUNT".to_string(), "1".to_string());
        base.insert(
            "GIT_CONFIG_KEY_0".to_string(),
            "credential.helper".to_string(),
        );
        base.insert("GIT_CONFIG_VALUE_0".to_string(), "osxkeychain".to_string());

        let worker_ctx = EnvContext {
            actor: "worker-7".to_string(),
            task: None,
            run: None,
            repo: repo.to_path_buf(),
            home: None,
            data: None,
        };
        let applied = launch_env(&base, Role::Worker, &worker_ctx).apply_to(&base);

        let all = Command::new("git")
            .args(["config", "--get-all", "credential.helper"])
            .current_dir(repo)
            .env_clear()
            .envs(&applied)
            .output();
        assert!(
            all.as_ref()
                .map(|out| out.status.success())
                .unwrap_or(false),
            "git config --get-all failed"
        );
        let all = unpack(all);
        let printed = String::from_utf8_lossy(&all.stdout).to_string();
        let helpers: Vec<&str> = printed.lines().collect();
        assert!(
            helpers.len() >= 2,
            "expected base + blank helper, got {printed:?}"
        );
        assert_eq!(helpers[0], "osxkeychain", "base helper must survive");
        assert_eq!(helpers[helpers.len() - 1], "", "last helper must be blank");

        let one = Command::new("git")
            .args(["config", "--get", "credential.helper"])
            .current_dir(repo)
            .env_clear()
            .envs(&applied)
            .output();
        let one = unpack(one);
        assert!(
            !one.status.success() || String::from_utf8_lossy(&one.stdout).trim().is_empty(),
            "effective credential.helper must be empty: {:?}",
            String::from_utf8_lossy(&one.stdout)
        );
    }
}
