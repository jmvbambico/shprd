//! What shprd tells a launched CLI about the folder it starts in, and what it
//! keeps a coordinator from doing there.
//!
//! Two launch facts live here, both per program:
//!
//! * **Folder trust.** Claude Code asks "Is this a project you created or one
//!   you trust" in every folder it has not seen, and the person had to answer by
//!   hand in the coordinator's pane. shprd starts the program in a folder the
//!   person chose (a repository) or made itself (a worktree), so the folder is
//!   already theirs: [`trust_folder`] says so up front. Claude has no flag for
//!   it, so the one thing it reads is its own per-project entry,
//!   `projects["<folder>"].hasTrustDialogAccepted` in `~/.claude.json` (or
//!   `$CLAUDE_CONFIG_DIR/.claude.json`). Codex takes the same fact as a `-c`
//!   argument, which writes nothing; pi takes `--approve`; opencode has no
//!   first-run prompt for a folder.
//! * **The coordinator's hands.** The coordinator turns asks into tasks and
//!   never changes a repository ([`coordinator_guard`]): where a CLI can refuse
//!   its own edit tools at launch, it is launched that way. The shell stays,
//!   because every `shprd` verb runs through it, so this narrows the program and
//!   the brief does the rest.
//!
//! Nothing here reads the process environment: callers pass the environment the
//! CLI will inherit, so a test never reaches the real `~/.claude.json`.

use std::collections::BTreeMap;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

/// The environment variable opencode reads inline permission rules from.
pub(crate) const OPENCODE_PERMISSION: &str = "OPENCODE_PERMISSION";

/// opencode's rules for a coordinator: file edits are denied, and `--auto`
/// approves only what is not explicitly denied.
const OPENCODE_COORDINATOR_RULES: &str = r#"{"edit":"deny"}"#;

/// Tools claude refuses a coordinator: the file editors, and the git verbs that
/// would turn an edit into history. The shell itself stays.
const CLAUDE_COORDINATOR_DENIED: &[&str] = &[
    "Edit",
    "Write",
    "NotebookEdit",
    "Bash(git add *)",
    "Bash(git commit *)",
    "Bash(git push *)",
    "Bash(git merge *)",
    "Bash(git rebase *)",
    "Bash(git reset *)",
    "Bash(git checkout *)",
    "Bash(git apply *)",
    "Bash(git stash *)",
    "Bash(rm *)",
];

/// What a coordinator's launch adds so the program cannot edit the repository.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CoordinatorGuard {
    /// Arguments appended to the program's command line.
    pub(crate) args: Vec<String>,
    /// Environment variables set for the program.
    pub(crate) env: Vec<(String, String)>,
}

/// The launch additions that keep `program`'s coordinator from editing the
/// repository, or none where the program has no way to say so (codex's sandbox
/// would also stop `shprd` writing its own log, so it is the brief alone).
pub(crate) fn coordinator_guard(program: &str) -> CoordinatorGuard {
    match program {
        // `--disallowedTools` is variadic, so the list is one comma-joined
        // argument: a following flag can never be read as another tool name.
        "claude" => CoordinatorGuard {
            args: vec![
                "--disallowedTools".to_string(),
                CLAUDE_COORDINATOR_DENIED.join(","),
            ],
            env: Vec::new(),
        },
        "pi" => CoordinatorGuard {
            args: vec!["--exclude-tools".to_string(), "edit,write".to_string()],
            env: Vec::new(),
        },
        "opencode" => CoordinatorGuard {
            args: Vec::new(),
            env: vec![(
                OPENCODE_PERMISSION.to_string(),
                OPENCODE_COORDINATOR_RULES.to_string(),
            )],
        },
        _ => CoordinatorGuard::default(),
    }
}

/// Whether `value` is the permission rule shprd gives a coordinator.
///
/// A worker started by the coordinator's own `shprd tick` inherits the
/// coordinator's shell environment, so its launch removes exactly this value and
/// never a rule the person set themselves.
pub(crate) fn is_coordinator_rule(value: &str) -> bool {
    value == OPENCODE_COORDINATOR_RULES
}

/// The launch arguments that mark `folder` as trusted for `program`, for the
/// programs that take that as an argument (nothing is written for them).
pub(crate) fn trust_args(program: &str, folder: &Path) -> Vec<String> {
    match program {
        // `-c projects={"<folder>"={trust_level="trusted"}}`: the same override a
        // reviewer's own command line uses; a TOML inline table, so the path is
        // a quoted key.
        "codex" => vec![
            "-c".to_string(),
            format!(
                "projects={{\"{}\"={{trust_level=\"trusted\"}}}}",
                toml_escape(&folder.display().to_string())
            ),
        ],
        // "Trust project-local files for this run".
        "pi" => vec!["--approve".to_string()],
        _ => Vec::new(),
    }
}

/// A string escaped for a TOML basic string.
fn toml_escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

/// What [`trust_folder`] did, for the caller's one line of log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Trusted {
    /// Nothing needed saying: the program has no prompt, or says it as an
    /// argument that was appended.
    Argument,
    /// The folder was already trusted; nothing was written.
    Already,
    /// The entry was written to this file.
    Wrote(PathBuf),
    /// No file could be named from the environment, so nothing was written and
    /// the program's own dialog (answered by the pane watcher) stays the way.
    Unknown,
}

/// Makes `folder` trusted for `program` before it starts there.
///
/// Arguments are appended to `argv`; for claude, the per-project entry is
/// written to its config file (see the module docs). `base_env` is the
/// environment the program will inherit: its `CLAUDE_CONFIG_DIR` and `HOME`
/// name the file, and with neither nothing is written.
pub(crate) fn trust_folder(
    program: &str,
    folder: &Path,
    base_env: &BTreeMap<String, String>,
    argv: &mut Vec<String>,
) -> io::Result<Trusted> {
    if program != "claude" {
        argv.extend(trust_args(program, folder));
        return Ok(Trusted::Argument);
    }
    let Some(config) = claude_config_file(base_env) else {
        return Ok(Trusted::Unknown);
    };
    // Claude records the folder it runs in, which the system resolves: on macOS
    // `/tmp` is `/private/tmp`. Both spellings are trusted so neither is asked.
    let mut folders = vec![folder.to_path_buf()];
    if let Ok(real) = folder.canonicalize()
        && real != folder
    {
        folders.push(real);
    }
    match trust_claude_projects(&config, &folders)? {
        true => Ok(Trusted::Wrote(config)),
        false => Ok(Trusted::Already),
    }
}

/// The file claude keeps its per-project state in for this environment:
/// `$CLAUDE_CONFIG_DIR/.claude.json`, else `$HOME/.claude.json`.
pub(crate) fn claude_config_file(base_env: &BTreeMap<String, String>) -> Option<PathBuf> {
    let set = |name: &str| base_env.get(name).filter(|value| !value.is_empty());
    match set("CLAUDE_CONFIG_DIR") {
        Some(dir) => Some(Path::new(dir).join(".claude.json")),
        None => set("HOME").map(|home| Path::new(home).join(".claude.json")),
    }
}

/// Sets `projects["<folder>"].hasTrustDialogAccepted = true` for each folder in
/// `config`, touching nothing else. Returns whether the file was written.
///
/// Running claude sessions rewrite this file all the time, so the write is as
/// small as it can be: nothing is written when every folder is trusted already,
/// the file is read again just before the write, the new text goes to a sibling
/// temp file with the old file's permissions and replaces the old one by rename,
/// and a file that is missing is created while one that is not a JSON object is
/// left alone (an error, and claude's own dialog stays the way).
fn trust_claude_projects(config: &Path, folders: &[PathBuf]) -> io::Result<bool> {
    let mut document = read_document(config)?;
    if !add_trust(&mut document, folders) {
        return Ok(false);
    }
    // A session may have written between the first read and now: start from the
    // newest text, and let a second look say whether there is still work.
    let mut document = read_document(config)?;
    if !add_trust(&mut document, folders) {
        return Ok(false);
    }
    let mut text = serde_json::to_string_pretty(&document).map_err(io::Error::other)?;
    text.push('\n');
    write_replacing(config, text.as_bytes())?;
    Ok(true)
}

/// The config document: an empty object when the file is not there.
fn read_document(config: &Path) -> io::Result<serde_json::Value> {
    match std::fs::read_to_string(config) {
        Ok(text) => {
            let value: serde_json::Value = serde_json::from_str(&text).map_err(|err| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{} is not valid JSON: {err}", config.display()),
                )
            })?;
            if value.is_object() {
                Ok(value)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{} is not a JSON object", config.display()),
                ))
            }
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(serde_json::json!({})),
        Err(err) => Err(err),
    }
}

/// Marks each folder trusted in `document`. Returns whether anything changed.
fn add_trust(document: &mut serde_json::Value, folders: &[PathBuf]) -> bool {
    let mut changed = false;
    let Some(root) = document.as_object_mut() else {
        return false;
    };
    let projects = root
        .entry("projects")
        .or_insert_with(|| serde_json::json!({}));
    let Some(projects) = projects.as_object_mut() else {
        return false;
    };
    for folder in folders {
        let entry = projects
            .entry(folder.display().to_string())
            .or_insert_with(|| serde_json::json!({}));
        let Some(entry) = entry.as_object_mut() else {
            continue;
        };
        if entry.get("hasTrustDialogAccepted") != Some(&serde_json::Value::Bool(true)) {
            entry.insert(
                "hasTrustDialogAccepted".to_string(),
                serde_json::Value::Bool(true),
            );
            changed = true;
        }
    }
    changed
}

/// Writes `bytes` to a temp file beside `target` and renames it over `target`,
/// keeping the target's permissions (a new file is private to its owner).
fn write_replacing(target: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = target.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let temp = dir.join(format!(
        ".{}.shprd-{}",
        target
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default(),
        std::process::id()
    ));
    let result = (|| {
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        match std::fs::metadata(target) {
            Ok(meta) => std::fs::set_permissions(&temp, meta.permissions())?,
            Err(_) => make_private(&temp)?,
        }
        std::fs::rename(&temp, target)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// Makes a file this module created private to its owner.
#[cfg(unix)]
fn make_private(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

/// Makes a file this module created private to its owner, where there is no
/// owner-only mode to ask for.
#[cfg(not(unix))]
fn make_private(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn env_with(pairs: &[(&str, &Path)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_string(), value.display().to_string()))
            .collect()
    }

    fn read(path: &Path) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(path).expect("the file is there"))
            .expect("the file is JSON")
    }

    #[test]
    fn a_new_folder_gets_a_trust_entry_and_every_other_key_stays() {
        let home = TempDir::new().expect("temp home");
        let config = home.path().join(".claude.json");
        std::fs::write(
            &config,
            r#"{"numStartups":7,"projects":{"/old":{"hasTrustDialogAccepted":true,"lastCost":1}}}"#,
        )
        .expect("seed");
        let folder = home.path().join("fresh");
        std::fs::create_dir(&folder).expect("folder");
        let mut argv = vec!["claude".to_string()];
        let env = env_with(&[("HOME", home.path())]);
        let done = trust_folder("claude", &folder, &env, &mut argv).expect("trust");
        assert_eq!(done, Trusted::Wrote(config.clone()));
        assert_eq!(argv, ["claude"], "claude takes no argument for it");
        let doc = read(&config);
        assert_eq!(doc["numStartups"], 7);
        assert_eq!(doc["projects"]["/old"]["lastCost"], 1);
        let key = folder.display().to_string();
        assert_eq!(doc["projects"][&key]["hasTrustDialogAccepted"], true);
        let real = folder.canonicalize().expect("real").display().to_string();
        assert_eq!(doc["projects"][&real]["hasTrustDialogAccepted"], true);
    }

    #[test]
    fn a_folder_already_trusted_writes_nothing() {
        let home = TempDir::new().expect("temp home");
        let folder = home.path().join("known");
        std::fs::create_dir(&folder).expect("folder");
        let real = folder.canonicalize().expect("real");
        let config = home.path().join(".claude.json");
        let seed = json!({"projects": {
            folder.display().to_string(): {"hasTrustDialogAccepted": true},
            real.display().to_string(): {"hasTrustDialogAccepted": true},
        }})
        .to_string();
        std::fs::write(&config, &seed).expect("seed");
        let mut argv = Vec::new();
        let env = env_with(&[("HOME", home.path())]);
        let done = trust_folder("claude", &folder, &env, &mut argv).expect("trust");
        assert_eq!(done, Trusted::Already);
        assert_eq!(std::fs::read_to_string(&config).expect("read"), seed);
    }

    #[test]
    fn a_declined_folder_is_trusted_and_its_other_fields_stay() {
        let home = TempDir::new().expect("temp home");
        let folder = home.path().join("declined");
        std::fs::create_dir(&folder).expect("folder");
        let key = folder.display().to_string();
        let config = home.path().join(".claude.json");
        std::fs::write(
            &config,
            json!({"projects": {key.clone(): {"hasTrustDialogAccepted": false, "lastCost": 2}}})
                .to_string(),
        )
        .expect("seed");
        let env = env_with(&[("HOME", home.path())]);
        trust_folder("claude", &folder, &env, &mut Vec::new()).expect("trust");
        let doc = read(&config);
        assert_eq!(doc["projects"][&key]["hasTrustDialogAccepted"], true);
        assert_eq!(doc["projects"][&key]["lastCost"], 2);
    }

    #[test]
    fn claude_config_dir_wins_over_home() {
        let home = TempDir::new().expect("temp home");
        let dir = TempDir::new().expect("config dir");
        let env = env_with(&[("HOME", home.path()), ("CLAUDE_CONFIG_DIR", dir.path())]);
        assert_eq!(
            claude_config_file(&env),
            Some(dir.path().join(".claude.json"))
        );
        let folder = home.path().join("f");
        std::fs::create_dir(&folder).expect("folder");
        trust_folder("claude", &folder, &env, &mut Vec::new()).expect("trust");
        assert!(dir.path().join(".claude.json").exists());
        assert!(!home.path().join(".claude.json").exists());
    }

    #[test]
    fn with_neither_home_nor_config_dir_nothing_is_written() {
        let folder = TempDir::new().expect("folder");
        let done = trust_folder("claude", folder.path(), &BTreeMap::new(), &mut Vec::new())
            .expect("no error");
        assert_eq!(done, Trusted::Unknown);
    }

    #[test]
    fn a_file_that_is_not_json_is_left_alone() {
        let home = TempDir::new().expect("temp home");
        let config = home.path().join(".claude.json");
        std::fs::write(&config, "{ not json").expect("seed");
        let folder = home.path().join("f");
        std::fs::create_dir(&folder).expect("folder");
        let env = env_with(&[("HOME", home.path())]);
        assert!(trust_folder("claude", &folder, &env, &mut Vec::new()).is_err());
        assert_eq!(
            std::fs::read_to_string(&config).expect("read"),
            "{ not json"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_replaced_file_keeps_its_permissions_and_no_temp_file_is_left() {
        use std::os::unix::fs::PermissionsExt;
        let home = TempDir::new().expect("temp home");
        let config = home.path().join(".claude.json");
        std::fs::write(&config, "{}").expect("seed");
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).expect("mode");
        let folder = home.path().join("f");
        std::fs::create_dir(&folder).expect("folder");
        let env = env_with(&[("HOME", home.path())]);
        trust_folder("claude", &folder, &env, &mut Vec::new()).expect("trust");
        let mode = std::fs::metadata(&config)
            .expect("meta")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        let names: Vec<_> = std::fs::read_dir(home.path())
            .expect("dir")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect();
        assert!(
            !names.iter().any(|name| name.contains("shprd-")),
            "{names:?}"
        );
    }

    #[test]
    fn codex_is_told_by_argument_and_nothing_is_written() {
        let args = trust_args("codex", Path::new("/work/shop"));
        assert_eq!(
            args,
            ["-c", "projects={\"/work/shop\"={trust_level=\"trusted\"}}"]
        );
        let mut argv = vec!["codex".to_string()];
        let done = trust_folder(
            "codex",
            Path::new("/work/shop"),
            &BTreeMap::new(),
            &mut argv,
        )
        .expect("trust");
        assert_eq!(done, Trusted::Argument);
        assert_eq!(&argv[1..], args.as_slice());
    }

    #[test]
    fn a_quote_in_a_folder_name_cannot_break_the_codex_override() {
        let args = trust_args("codex", Path::new("/work/a\"b"));
        assert_eq!(
            args[1],
            "projects={\"/work/a\\\"b\"={trust_level=\"trusted\"}}"
        );
    }

    #[test]
    fn pi_is_approved_and_opencode_has_nothing_to_say() {
        assert_eq!(trust_args("pi", Path::new("/w")), ["--approve"]);
        assert!(trust_args("opencode", Path::new("/w")).is_empty());
    }

    #[test]
    fn the_claude_coordinator_cannot_edit_or_commit_but_keeps_its_shell() {
        let guard = coordinator_guard("claude");
        assert_eq!(guard.args[0], "--disallowedTools");
        assert_eq!(guard.args.len(), 2, "one comma-joined argument");
        let list: Vec<&str> = guard.args[1].split(',').collect();
        for tool in ["Edit", "Write", "NotebookEdit", "Bash(git commit *)"] {
            assert!(list.contains(&tool), "{tool} is denied: {list:?}");
        }
        assert!(
            !list.contains(&"Bash"),
            "the shell stays, every shprd verb runs through it"
        );
    }

    #[test]
    fn pi_and_opencode_coordinators_lose_their_edit_tools_and_codex_has_no_flag() {
        assert_eq!(
            coordinator_guard("pi").args,
            ["--exclude-tools", "edit,write"]
        );
        let opencode = coordinator_guard("opencode");
        assert!(opencode.args.is_empty());
        assert_eq!(
            opencode.env,
            [(
                "OPENCODE_PERMISSION".to_string(),
                "{\"edit\":\"deny\"}".to_string()
            )]
        );
        assert!(is_coordinator_rule(&opencode.env[0].1));
        assert!(!is_coordinator_rule("{\"edit\":\"ask\"}"));
        assert_eq!(coordinator_guard("codex"), CoordinatorGuard::default());
    }
}
