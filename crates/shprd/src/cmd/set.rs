//! `shprd set`: write a roster role into a config file.
//!
//! The file is edited rather than rewritten, so comments and keys the verb does
//! not own survive. The agent strings are checked against the harness table
//! before anything is written. The edited text is then validated in memory,
//! merged with the other config layer: a change that would make the
//! config unusable (a reviewer equal to the orchestrator, say) exits 2 with the
//! reason and writes nothing at all. A write that survives that check goes to a
//! unique temp file, is fsynced, and is renamed over the target, so the config
//! is never seen half-written and no restore step is needed. The file is read
//! again once the temp file is on disk and just before the rename, so a file
//! that changed at any point between the first read and the swap is refused
//! instead of replaced.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::anyhow;
use shprd_core::config;
use shprd_roster::models::{AgentRef, list_models, parse_effort};
use shprd_roster::spec::{HarnessSpec, load_dir};
use toml_edit::{Array, DocumentMut, Item, Table, value};

use crate::cli::{SetArgs, SetRole};
use crate::cmd::roster;
use crate::ctx::{CliError, resolve_home, resolve_root};

/// How long a model listing may take; the listing only feeds a warning.
const LIST_TIMEOUT: Duration = Duration::from_secs(10);

/// Runs `shprd set`, writing the role the command line named.
/// `repo` is the `--repo` root: giving it writes that repository's file
/// instead of the global one.
pub fn run(args: &SetArgs, home: Option<&Path>, repo: Option<&Path>) -> Result<(), CliError> {
    let local = repo.is_some();
    execute(args, home, repo, local)
}

/// [`run`], with the repository root already resolved and the scope chosen.
fn execute(
    args: &SetArgs,
    home: Option<&Path>,
    root: Option<&Path>,
    local: bool,
) -> Result<(), CliError> {
    let dir = roster::resolve_data_dir()?;
    let specs = load_dir(&dir.join("harness")).map_err(|err| {
        CliError::other(anyhow!(err).context(format!("loading {}", dir.display())))
    })?;
    let change = change_of(args, &specs)?;
    let home = resolve_home(home)?;
    let root = match root {
        Some(root) => Some(root.to_path_buf()),
        None => resolve_root(None).ok(),
    };
    let path = target_path(local, &home, root.as_deref())?;

    let (global, repo) = layers(&home, root.as_deref());

    // Read and edit in memory: the file stays untouched until the edited text
    // is known to be good.
    let previous = previous_text(&path)?;
    let mut document = match &previous {
        Some(text) => text.parse::<DocumentMut>().map_err(|err| {
            CliError::other(anyhow!(err).context(format!("parsing {}", path.display())))
        })?,
        None => DocumentMut::new(),
    };
    apply(&mut document, &change)?;
    let edited = document.to_string();
    validate_edited(&path, &edited, &global, repo.as_deref())?;

    // A file someone else saved in the meantime wins: replacing it would
    // silently drop their edit. The check runs again just before the swap, so
    // an edit that lands while the temp file is written is caught too.
    replace_checked(&path, &edited, previous.as_deref(), || {})?;

    let path_env = std::env::var("PATH").unwrap_or_default();
    for agent in &change.agents {
        warn_unknown_model(agent, &specs, &path_env);
    }
    println!("ok {} = {}", change.role, change.agents.join(" "));
    Ok(())
}

/// The validated change a command line asks for.
struct Change {
    /// The `[roster]` key to write, e.g. `orchestrator` or `workers`.
    role: &'static str,
    /// The agent strings, in fallback order.
    agents: Vec<String>,
    /// The effort for the single-agent roles, already checked.
    effort: Option<String>,
}

/// Check every agent string and effort, and normalize the strings.
fn change_of(args: &SetArgs, specs: &[HarnessSpec]) -> Result<Change, CliError> {
    let (role, agents, effort) = match &args.role {
        SetRole::Orchestrator(one) => ("orchestrator", vec![one.agent.clone()], one.effort.clone()),
        SetRole::Reviewer(one) => ("reviewer", vec![one.agent.clone()], one.effort.clone()),
        SetRole::Workers(many) => ("workers", many.agents.clone(), None),
    };
    let mut parsed = Vec::with_capacity(agents.len());
    for agent in &agents {
        let found = AgentRef::parse(agent, specs).map_err(|err| {
            CliError::Invalid(anyhow!(err).context(format!("`{agent}` is not a usable agent")))
        })?;
        parsed.push(found.to_string());
    }
    let effort = match effort {
        Some(effort) => Some(parse_effort(&effort).map_err(|err| {
            CliError::Invalid(anyhow!(err).context("checking the effort".to_owned()))
        })?),
        None => None,
    };
    Ok(Change {
        role,
        agents: parsed,
        effort,
    })
}

/// The file to write: the global one, or the repo's with `--repo`.
fn target_path(local: bool, home: &Path, root: Option<&Path>) -> Result<PathBuf, CliError> {
    if local {
        let root = root.ok_or_else(|| {
            CliError::Invalid(anyhow!(
                "`--repo` writes the repository's file, so run it inside a repository"
            ))
        })?;
        return Ok(root.join(".shprd").join("config.toml"));
    }
    Ok(home.join(".config").join("shprd").join("config.toml"))
}

/// The two config layers the merged config is read from, repo file optional.
fn layers(home: &Path, root: Option<&Path>) -> (PathBuf, Option<PathBuf>) {
    let global = home.join(".config").join("shprd").join("config.toml");
    let repo = root.map(|root| root.join(".shprd").join("config.toml"));
    (global, repo)
}

/// The file's current text, or [`None`] when it does not exist yet.
fn previous_text(path: &Path) -> Result<Option<String>, CliError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(CliError::other(
            anyhow!(error).context(format!("reading {}", path.display())),
        )),
    }
}

/// Check the edited text without writing it: parse it as a config layer, merge
/// it with the other layer the way [`config::load`] does, and validate the
/// result. A refusal leaves the file exactly as it was.
fn validate_edited(
    path: &Path,
    edited: &str,
    global: &Path,
    repo: Option<&Path>,
) -> Result<(), CliError> {
    let mut parsed = Vec::new();
    for layer_path in [Some(global), repo].into_iter().flatten() {
        let text = if layer_path == path {
            Some(edited.to_owned())
        } else {
            previous_text(layer_path)?
        };
        let Some(text) = text else { continue };
        let layer: config::ConfigLayer =
            toml::from_str(&text).map_err(|err| unusable(path, anyhow!(err)))?;
        parsed.push(layer);
    }
    config::from_layers(&parsed)
        .validate()
        .map_err(|err| unusable(path, anyhow!(err)))
}

/// The refusal a change that would make the config unusable produces.
fn unusable(path: &Path, err: anyhow::Error) -> CliError {
    CliError::Invalid(err.context(format!(
        "{} would be invalid, so nothing was changed",
        path.display()
    )))
}

/// Refuse when the file no longer holds the text that was read from it.
fn ensure_unchanged(path: &Path, before: Option<&str>) -> Result<(), CliError> {
    if previous_text(path)?.as_deref() == before {
        return Ok(());
    }
    Err(CliError::Invalid(anyhow!(
        "config changed while editing; try again"
    )))
}

/// Write `text` to `path` through a temp file beside it, then swap it in.
///
/// The temp file is on disk and fsynced before the target is checked again, so
/// an edit that landed while the temp file was being written is refused instead
/// of overwritten. `before_rename` runs between the two: production passes a
/// no-op, a test uses it to change the target at the last possible moment.
fn replace_checked(
    path: &Path,
    text: &str,
    before: Option<&str>,
    before_rename: impl FnOnce(),
) -> Result<(), CliError> {
    let temp = write_temp(path, text)?;
    before_rename();
    if let Err(refusal) = ensure_unchanged(path, before) {
        let _ = fs::remove_file(&temp);
        return Err(refusal);
    }
    fs::rename(&temp, path)
        .inspect_err(|_| {
            let _ = fs::remove_file(&temp);
        })
        .map_err(|error| {
            CliError::other(anyhow!(error).context(format!("replacing {}", path.display())))
        })
}

/// Write `text` to a unique, fsynced temp file beside `path`, returning its path.
///
/// The temp file shares the target's directory, so a later rename stays inside
/// one filesystem, and it is fsynced, so a crash cannot leave a half-written
/// config behind. The caller must rename or remove the file.
fn write_temp(path: &Path, text: &str) -> Result<PathBuf, CliError> {
    let parent = path
        .parent()
        .map_or_else(|| Path::new("."), |parent| parent);
    fs::create_dir_all(parent).map_err(|error| {
        CliError::other(anyhow!(error).context(format!("creating {}", parent.display())))
    })?;
    let temp = temp_path(path);
    let written = fs::File::create(&temp).and_then(|mut file| {
        file.write_all(text.as_bytes())?;
        file.sync_all()
    });
    if let Err(error) = written {
        let _ = fs::remove_file(&temp);
        return Err(CliError::other(
            anyhow!(error).context(format!("writing {}", temp.display())),
        ));
    }
    Ok(temp)
}

/// A temp path beside `path` that no two writers can pick at the same time.
fn temp_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let parent = path
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    parent.join(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        unique_nanos()
    ))
}

/// The largest nanosecond timestamp this process has handed out so far.
static LAST_NANOS: AtomicU64 = AtomicU64::new(0);

/// Nanoseconds since the Unix epoch, at least one more than the previous call's
/// value within this process.
fn unique_nanos() -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or(0);
    let mut previous = LAST_NANOS.load(Ordering::Relaxed);
    loop {
        let candidate = now.max(previous.saturating_add(1));
        match LAST_NANOS.compare_exchange_weak(
            previous,
            candidate,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return candidate,
            Err(actual) => previous = actual,
        }
    }
}

/// Write the change into the document, touching nothing else.
fn apply(document: &mut DocumentMut, change: &Change) -> Result<(), CliError> {
    let roster = document["roster"].or_insert(Item::Table(Table::new()));
    let table = roster.as_table_mut().ok_or_else(|| {
        CliError::other(anyhow!(
            "the config's `roster` key is not a table, so it cannot be set"
        ))
    })?;
    if change.role == "workers" {
        let mut array = Array::new();
        for agent in &change.agents {
            array.push(agent.as_str());
        }
        table["workers"] = value(array);
    } else if let Some(agent) = change.agents.first() {
        table[change.role] = value(agent.as_str());
    }
    if let (Some(effort), Some(agent)) = (&change.effort, change.agents.first()) {
        let effort_table = table["effort"].or_insert(Item::Table(Table::new()));
        let effort_table = effort_table.as_table_mut().ok_or_else(|| {
            CliError::other(anyhow!("the config's `roster.effort` key is not a table"))
        })?;
        effort_table[agent] = value(effort.as_str());
    }
    Ok(())
}

/// Warn when the harness lists models and the chosen one is not among them.
fn warn_unknown_model(agent: &str, specs: &[HarnessSpec], path_env: &str) {
    let Ok(parsed) = AgentRef::parse(agent, specs) else {
        return;
    };
    let (Some(spec), Some(model)) = (parsed.spec(specs), parsed.model.as_deref()) else {
        return;
    };
    match list_models(spec, path_env, LIST_TIMEOUT) {
        Ok(Some(models)) if !models.contains(&model.to_owned()) => {
            eprintln!(
                "warning: `{model}` is not in what `{}` lists; setting it anyway",
                spec.id
            );
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::cli::OneAgent;

    /// A temp home and repo, so nothing in the real checkout is ever written.
    struct Fixture {
        home: TempDir,
        root: TempDir,
    }

    impl Fixture {
        fn new() -> Fixture {
            Fixture {
                home: temp("home"),
                root: temp("root"),
            }
        }

        /// The global config file the fixture's home holds.
        fn global(&self) -> PathBuf {
            self.home
                .path()
                .join(".config")
                .join("shprd")
                .join("config.toml")
        }

        /// The config file under the fixture's repo.
        fn repo_file(&self) -> PathBuf {
            self.root.path().join(".shprd").join("config.toml")
        }

        /// Run `set` with this fixture's home, as a global write.
        fn run(&self, args: &SetArgs) -> Result<(), CliError> {
            execute(args, Some(self.home.path()), Some(self.root.path()), false)
        }

        /// Run `set` with this fixture's home and repo, as a repo write.
        fn run_in_repo(&self, args: &SetArgs) -> Result<(), CliError> {
            execute(args, Some(self.home.path()), Some(self.root.path()), true)
        }
    }

    fn temp(label: &str) -> TempDir {
        match TempDir::new() {
            Ok(dir) => dir,
            Err(error) => panic!("{label} tempdir failed: {error}"),
        }
    }

    /// The text of `path`, or an empty string when it does not exist.
    fn text_of(path: &Path) -> String {
        fs::read_to_string(path).unwrap_or_default()
    }

    /// `set orchestrator <agent>` with no effort.
    fn orchestrator(agent: &str) -> SetArgs {
        SetArgs {
            role: SetRole::Orchestrator(OneAgent {
                agent: agent.to_owned(),
                effort: None,
            }),
        }
    }

    /// `set reviewer <agent>` with no effort.
    fn reviewer(agent: &str) -> SetArgs {
        SetArgs {
            role: SetRole::Reviewer(OneAgent {
                agent: agent.to_owned(),
                effort: None,
            }),
        }
    }

    #[test]
    fn writes_the_orchestrator_to_the_global_file() {
        let fixture = Fixture::new();
        assert!(fixture.run(&orchestrator("claude/claude-opus-5-5")).is_ok());
        let text = text_of(&fixture.global());
        assert!(text.contains("[roster]"), "{text}");
        assert!(
            text.contains("orchestrator = \"claude/claude-opus-5-5\""),
            "{text}"
        );
        assert!(!fixture.repo_file().exists());
    }

    #[test]
    fn keeps_the_comments_and_keys_of_an_existing_file() {
        let fixture = Fixture::new();
        let path = fixture.global();
        let parent = path.parent().unwrap_or(Path::new("."));
        assert!(fs::create_dir_all(parent).is_ok());
        let before = "# written by hand\n[roster]\nreviewer = \"codex/gpt-6\"\n\n[janitor]\narchive_days = 30\n";
        assert!(fs::write(&path, before).is_ok());

        assert!(fixture.run(&orchestrator("claude/claude-opus-5-5")).is_ok());
        let text = text_of(&path);
        assert!(text.contains("# written by hand"), "{text}");
        assert!(text.contains("reviewer = \"codex/gpt-6\""), "{text}");
        assert!(text.contains("archive_days = 30"), "{text}");
        assert!(text.contains("claude/claude-opus-5-5"), "{text}");
    }

    #[test]
    fn refuses_a_reviewer_equal_to_the_orchestrator() {
        let fixture = Fixture::new();
        assert!(fixture.run(&orchestrator("claude/claude-opus-5-5")).is_ok());
        let before = text_of(&fixture.global());

        let refused = fixture.run(&reviewer("claude/claude-opus-5-5"));
        match refused {
            Err(err @ CliError::Invalid(_)) => assert_eq!(err.exit_code(), 2),
            other => panic!("expected Invalid, got {other:?}"),
        }
        assert_eq!(text_of(&fixture.global()), before);
    }

    /// Every refusal of a user's input exits 2, like the other verbs.
    fn refusal_code(result: Result<(), CliError>) -> u8 {
        match result {
            Err(err @ CliError::Invalid(_)) => err.exit_code(),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn refuses_a_pi_model_without_its_provider() {
        let fixture = Fixture::new();
        let refused = fixture.run(&orchestrator("pi/deepseek-v4.1-flash"));
        match refused {
            Err(ref err) => assert!(format!("{err}").contains("<provider>"), "{err}"),
            Ok(()) => panic!("a bare pi model should be refused"),
        }
        assert_eq!(
            refusal_code(fixture.run(&orchestrator("pi/deepseek-v4.1-flash"))),
            2
        );
        assert!(!fixture.global().exists());
    }

    #[test]
    fn every_refused_set_exits_2() {
        let fixture = Fixture::new();

        assert_eq!(refusal_code(fixture.run(&orchestrator("nosuch/model"))), 2);
        let at_turbo = SetArgs {
            role: SetRole::Orchestrator(OneAgent {
                agent: "claude/claude-opus-5-5".to_owned(),
                effort: Some("turbo".to_owned()),
            }),
        };
        assert_eq!(refusal_code(fixture.run(&at_turbo)), 2);

        let outside_a_repo = target_path(true, fixture.home.path(), None);
        assert_eq!(refusal_code(outside_a_repo.map(|_| ())), 2);
        assert!(!fixture.global().exists());
    }

    #[test]
    fn writes_the_repo_file_with_repo() {
        let fixture = Fixture::new();
        assert!(
            fixture
                .run_in_repo(&orchestrator("claude/claude-opus-5-5"))
                .is_ok()
        );
        let text = text_of(&fixture.repo_file());
        assert!(text.contains("claude/claude-opus-5-5"), "{text}");
        assert!(!fixture.global().exists());
    }

    #[test]
    fn writes_workers_as_an_ordered_list_and_effort_per_agent() {
        let fixture = Fixture::new();
        let workers = SetArgs {
            role: SetRole::Workers(crate::cli::ManyAgents {
                agents: vec![
                    "claude/claude-opus-5-5".to_owned(),
                    "opencode/opencode/deepseek-v4.1-flash-free".to_owned(),
                ],
            }),
        };
        assert!(fixture.run(&workers).is_ok());
        let text = text_of(&fixture.global());
        assert!(
            text.contains(
                "\"claude/claude-opus-5-5\", \"opencode/opencode/deepseek-v4.1-flash-free\""
            ),
            "{text}"
        );

        let at_high = SetArgs {
            role: SetRole::Orchestrator(OneAgent {
                agent: "claude/claude-opus-5-5".to_owned(),
                effort: Some("high".to_owned()),
            }),
        };
        assert!(fixture.run(&at_high).is_ok());
        let text = text_of(&fixture.global());
        assert!(text.contains("[roster.effort]"), "{text}");
        assert!(
            text.contains("\"claude/claude-opus-5-5\" = \"high\""),
            "{text}"
        );
    }

    /// Every file beside `path` other than the config itself, so a refused or
    /// failed write can prove it left no temp debris.
    fn other_files_beside(path: &Path) -> Vec<String> {
        let parent = path.parent().unwrap_or(Path::new("."));
        match fs::read_dir(parent) {
            Ok(entries) => entries
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| name != "config.toml")
                .collect(),
            Err(error) => panic!("should read {}: {error}", parent.display()),
        }
    }

    #[test]
    fn an_invalid_change_leaves_the_file_untouched() {
        let fixture = Fixture::new();
        assert!(fixture.run(&orchestrator("claude/claude-opus-5-5")).is_ok());
        let path = fixture.global();
        let before = fs::read(&path).unwrap_or_default();

        let refused = fixture.run(&reviewer("claude/claude-opus-5-5"));
        assert_eq!(refusal_code(refused), 2);
        assert_eq!(fs::read(&path).unwrap_or_default(), before);
        let left = other_files_beside(&path);
        assert!(left.is_empty(), "left behind: {left:?}");
    }

    #[test]
    fn replaces_the_file_and_leaves_no_temp_file() {
        let fixture = Fixture::new();
        let path = fixture.global();
        let parent = path.parent().unwrap_or(Path::new("."));
        assert!(fs::create_dir_all(parent).is_ok());
        let before = "# keep me\n[janitor]\narchive_days = 30\n";
        assert!(fs::write(&path, before).is_ok());

        assert!(fixture.run(&orchestrator("claude/claude-opus-5-5")).is_ok());
        let text = text_of(&path);
        assert!(text.contains("# keep me"), "{text}");
        assert!(text.contains("archive_days = 30"), "{text}");
        let left = other_files_beside(&path);
        assert!(left.is_empty(), "left behind: {left:?}");
    }

    #[test]
    fn refuses_a_file_that_changed_while_it_was_edited() {
        let home = temp("guard");
        let path = home.path().join("config.toml");
        assert!(fs::write(&path, "mux = \"herdr\"\n").is_ok());
        let before = previous_text(&path).unwrap_or_default();

        assert!(ensure_unchanged(&path, before.as_deref()).is_ok());
        assert!(fs::write(&path, "mux = \"tuios\"\n").is_ok());
        match ensure_unchanged(&path, before.as_deref()) {
            Err(ref err) => assert!(
                format!("{err}").contains("config changed while editing; try again"),
                "{err}"
            ),
            Ok(()) => panic!("a file edited underneath should be refused"),
        }
        assert_eq!(refusal_code(ensure_unchanged(&path, before.as_deref())), 2);
        assert_eq!(text_of(&path), "mux = \"tuios\"\n");
    }

    #[test]
    fn refuses_a_change_that_lands_while_the_temp_file_is_written() {
        let home = temp("hook");
        let path = home.path().join("config.toml");
        let before = "mux = \"herdr\"\n";
        assert!(fs::write(&path, before).is_ok());
        let hook_text = "mux = \"tuios\"\n";

        let refused = replace_checked(&path, "mux = \"herdr\"\n[roster]\n", Some(before), || {
            assert!(fs::write(&path, hook_text).is_ok());
        });
        assert_eq!(refusal_code(refused), 2);
        assert_eq!(text_of(&path), hook_text);
        let left = other_files_beside(&path);
        assert!(left.is_empty(), "left behind: {left:?}");
    }
}
