//! `shprd set`: write a roster role into a config file.
//!
//! The file is edited rather than rewritten, so comments and keys the verb does
//! not own survive. The agent strings are checked against the harness table
//! before anything is written, and the merged config is loaded and validated
//! afterwards: a change that would make it unusable (a reviewer equal to the
//! orchestrator, say) is undone and the verb exits 2 with the reason.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

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

    let previous = previous_text(&path)?;
    let mut document = match &previous {
        Some(text) => text.parse::<DocumentMut>().map_err(|err| {
            CliError::other(anyhow!(err).context(format!("parsing {}", path.display())))
        })?,
        None => DocumentMut::new(),
    };
    apply(&mut document, &change)?;
    write(&path, document.to_string().as_str())?;

    let (global, repo) = layers(&home, root.as_deref());
    if let Err(err) = config::load(Some(&global), repo.as_deref()) {
        restore(&path, previous.as_deref())?;
        return Err(CliError::Invalid(anyhow!(err).context(format!(
            "{} would be invalid, so nothing was changed",
            path.display()
        ))));
    }

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
            CliError::other(anyhow!(err).context(format!("`{agent}` is not a usable agent")))
        })?;
        parsed.push(found.to_string());
    }
    let effort = match effort {
        Some(effort) => Some(parse_effort(&effort).map_err(|err| {
            CliError::other(anyhow!(err).context("checking the effort".to_owned()))
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
            CliError::other(anyhow!(
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

/// Create the parent directory and write `text` to `path`.
fn write(path: &Path, text: &str) -> Result<(), CliError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            CliError::other(anyhow!(error).context(format!("creating {}", parent.display())))
        })?;
    }
    fs::write(path, text).map_err(|error| {
        CliError::other(anyhow!(error).context(format!("writing {}", path.display())))
    })
}

/// Put `previous` back, deleting the file when there was none.
fn restore(path: &Path, previous: Option<&str>) -> Result<(), CliError> {
    match previous {
        Some(text) => write(path, text),
        None => match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(CliError::other(
                anyhow!(error).context(format!("removing {}", path.display())),
            )),
        },
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

    #[test]
    fn refuses_a_pi_model_without_its_provider() {
        let fixture = Fixture::new();
        let refused = fixture.run(&orchestrator("pi/deepseek-v4.1-flash"));
        match refused {
            Err(err) => assert!(format!("{err}").contains("<provider>"), "{err}"),
            Ok(()) => panic!("a bare pi model should be refused"),
        }
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
}
