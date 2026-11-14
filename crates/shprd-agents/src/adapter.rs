//! Per-program adapters: the exact argv, environment and brief route for one
//! role's pick.
//!
//! A role chooses `{program, model, effort, fast, perm}` (Shepherdr's pick, M7c
//! 2.2). This module turns that choice plus a brief into what the program is
//! started with — never by branching on the program's name, always from the
//! harness spec in `data/harness/`, so a new program is a table, not code:
//!
//! - [`launch`] gives the argv, in the order Shepherdr's `argvFor` writes it
//!   (base, model, effort, fast, access mode, brief), the environment diff the
//!   launch gets, and where the brief went.
//! - [`Pick::validate`] refuses a model, effort, fast or access mode the program
//!   does not take, naming the ones that work, so a wrong setting is a message
//!   and never a CLI that starts odd or not at all.
//! - [`catalog_from_output`] and [`catalog_from_static`] read a program's model
//!   catalog into models and effort levels for the picker.
//!
//! The adapter facts are borrowed from Shepherdr (MIT), tag v3.1.34, commit
//! a3f327a: `core/agents.js` (`argvFor`, the fast values and the argument
//! order), `mods/tasks/bin/herdr_task/settings.py` (`argv_for`) and
//! `mods/tasks/bin/herdr_task/launch.py` (`CLEAN_ENV`, the parent-session strip
//! that lives in [`crate::env`]). https://github.com/eliasstravik/shepherdr
//!
//! What a launch does not decide: a resume (the caller that resumes appends
//! `launch.resume_args` itself), the reviewer's `launch.reviewer_args` and
//! `launch.auto_approve` (both stay for the older spawn path; here the access
//! mode is the one thing that decides approvals), and the words typed into the
//! pane (a kickoff when the brief went in its argv, the brief itself otherwise).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::env::{self, EnvContext, LaunchEnv, Role as EnvRole};
use crate::harness::Harness;
use crate::models::{self, Catalog, ModelError};
use crate::spec::{BriefDelivery, CatalogSource, EffortSource, SpecError, Values, render_args};

/// One role's choice of program settings: `{program, model, effort, fast, perm}`.
///
/// Every field is what a settings row or a task override may say. A model or
/// effort left out means whatever the program does by itself, `fast` is off
/// unless a role asks for it, and `perm` falls back to the program's own
/// default mode.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pick {
    /// The harness id, e.g. `claude`.
    pub program: String,
    /// The model to pass, in the program's own words, if the role names one.
    pub model: Option<String>,
    /// The effort or thinking level, if the role names one.
    pub effort: Option<String>,
    /// Whether the role asked for the program's fast (higher-priority) tier.
    pub fast: bool,
    /// The program's own access-mode id; the program's default when absent.
    pub perm: Option<String>,
}

/// The brief a role hands its program.
///
/// Both forms travel together: a program with a system-prompt flag is given the
/// file, and one without is typed the words, so the caller always writes the
/// file and the adapter decides which form this program takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Brief {
    /// The brief's words, as they would be typed into the pane.
    pub text: String,
    /// The file the caller writes the words to.
    pub file: PathBuf,
}

/// What the launched program inherits, and who it acts as.
#[derive(Debug, Clone)]
pub struct Host<'a> {
    /// The environment the program would inherit before shprd's rules.
    pub base_env: &'a BTreeMap<String, String>,
    /// The role the CLI acts as, which decides how much the environment allows.
    pub role: EnvRole,
    /// shprd's own identity variables for this launch.
    pub env: &'a EnvContext,
}

/// One launch, ready for a terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    /// The full command line, the program first.
    pub argv: Vec<String>,
    /// The environment diff to apply on top of the base environment.
    pub env: LaunchEnv,
    /// Where the brief goes: in this argv, or typed into the pane.
    pub brief: BriefRoute,
}

/// Where a brief reaches the program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BriefRoute {
    /// The brief is already in [`Launch::argv`]; the pane gets a one-line
    /// kickoff so the program takes its first turn.
    InArgs,
    /// The brief is typed into the pane as the program's first turn.
    Typed,
}

/// Build one program launch for `pick` and `brief`.
///
/// The argv is the harness's base command, then the model, the effort, the fast
/// switch when the program has one, the access mode (the named one, or the
/// program's default), and the system-prompt flag when the program takes the
/// brief as a file. The environment is [`crate::env::launch_env`] for `host`.
pub fn launch(
    harness: &Harness,
    pick: &Pick,
    brief: &Brief,
    host: &Host<'_>,
) -> Result<Launch, AdapterError> {
    pick.validate(harness)?;
    let spec = harness.spec();
    let mut argv = spec.launch.interactive.clone();

    if let Some(model) = &pick.model {
        let values = Values {
            model: Some(model.clone()),
            ..Values::default()
        };
        argv.extend(render_args(&spec.launch.model_args, &values)?);
    }
    if let Some(effort) = &pick.effort
        && let Some(template) = &spec.launch.effort_args
    {
        let values = Values {
            effort: Some(effort.clone()),
            ..Values::default()
        };
        argv.extend(render_args(template, &values)?);
    }
    if harness.fast().available() {
        argv.extend(harness.fast().args(pick.fast).iter().cloned());
    }
    if let Some(mode) = selected_mode(harness, pick.perm.as_deref()) {
        argv.extend(mode.args.iter().cloned());
    }

    let mut route = BriefRoute::Typed;
    if harness.brief().delivery == BriefDelivery::SystemPromptFile {
        let Some(template) = harness.brief_args() else {
            return Err(AdapterError::BriefWithoutFlag {
                program: harness.id().to_owned(),
            });
        };
        let values = Values {
            prompt: Some(brief.text.clone()),
            prompt_file: Some(brief.file.display().to_string()),
            ..Values::default()
        };
        argv.extend(render_args(template, &values)?);
        route = BriefRoute::InArgs;
    }

    Ok(Launch {
        argv,
        env: env::launch_env(host.base_env, host.role, host.env),
        brief: route,
    })
}

/// The access mode `pick` gets: the one it names, else the program's default.
fn selected_mode<'a>(
    harness: &'a Harness,
    perm: Option<&str>,
) -> Option<&'a crate::spec::PermissionMode> {
    match perm {
        Some(id) => harness.permission(id),
        None => harness.default_permission(),
    }
}

/// Read a program's catalog from its own list command's output.
///
/// A program with no list command has no output to read; use
/// [`catalog_from_static`] for the static table it names instead.
pub fn catalog_from_output(harness: &Harness, output: &str) -> Result<Catalog, AdapterError> {
    if harness.catalog_source() != CatalogSource::Command {
        return Err(AdapterError::NoCatalog {
            program: harness.id().to_owned(),
        });
    }
    Ok(models::parse_catalog(harness.spec(), output)?)
}

/// Read the static catalog a program names, from a `data/models` directory.
///
/// [`None`] when the program has no static catalog.
pub fn catalog_from_static(
    harness: &Harness,
    models_dir: &Path,
) -> Result<Option<Catalog>, AdapterError> {
    Ok(models::static_catalog(harness.spec(), models_dir)?)
}

impl Pick {
    /// Refuse a choice the program cannot take, naming what it can.
    ///
    /// Checked here rather than at launch: a wrong mode, effort or fast flag is
    /// a settings question with a list of working values for an answer, and a
    /// CLI started with one only fails later and less clearly. Levels are
    /// checked against the program's own list, not a model's, because the model
    /// catalog is read over the network and may be stale or absent.
    pub fn validate(&self, harness: &Harness) -> Result<(), AdapterError> {
        if !self.program.is_empty() && self.program != harness.id() {
            return Err(AdapterError::WrongProgram {
                pick: self.program.clone(),
                harness: harness.id().to_owned(),
            });
        }
        if let Some(model) = &self.model
            && model.trim().is_empty()
        {
            return Err(AdapterError::EmptyModel);
        }
        if let Some(perm) = &self.perm
            && harness.permission(perm).is_none()
        {
            let known: Vec<&str> = harness
                .permissions()
                .modes
                .iter()
                .map(|mode| mode.id.as_str())
                .collect();
            return Err(AdapterError::UnknownPermission {
                program: harness.id().to_owned(),
                mode: perm.clone(),
                known: known.join(", "),
            });
        }
        if let Some(effort) = &self.effort {
            let levels = harness.efforts();
            if levels.source == EffortSource::None || levels.levels.is_empty() {
                return Err(AdapterError::UnsupportedEffort {
                    program: harness.id().to_owned(),
                    effort: effort.clone(),
                });
            }
            if !levels.levels.contains(effort) {
                return Err(AdapterError::UnknownEffort {
                    program: harness.id().to_owned(),
                    effort: effort.clone(),
                    allowed: levels.levels.join(", "),
                });
            }
        }
        if self.fast && !harness.fast().available() {
            return Err(AdapterError::UnsupportedFast {
                program: harness.id().to_owned(),
            });
        }
        Ok(())
    }
}

/// Why a pick could not become a launch, or a catalog could not be read.
#[derive(Debug, Error)]
pub enum AdapterError {
    /// The pick names one program and the harness is another.
    #[error("the pick is for `{pick}`, but the harness is `{harness}`")]
    WrongProgram {
        /// The program the pick names.
        pick: String,
        /// The harness that was handed in.
        harness: String,
    },
    /// The pick names an access mode the program does not have.
    #[error("`{program}` has no access mode `{mode}`: it takes {known}")]
    UnknownPermission {
        /// The harness id.
        program: String,
        /// The mode id that was written.
        mode: String,
        /// The mode ids that work, comma separated.
        known: String,
    },
    /// The pick names an effort level but the program takes none.
    #[error("`{program}` takes no effort level, so `{effort}` cannot be set")]
    UnsupportedEffort {
        /// The harness id.
        program: String,
        /// The effort that was written.
        effort: String,
    },
    /// The pick names an effort level the program does not list.
    #[error("`{program}` has no effort `{effort}`: it takes {allowed}")]
    UnknownEffort {
        /// The harness id.
        program: String,
        /// The level that was written.
        effort: String,
        /// The levels that work, comma separated.
        allowed: String,
    },
    /// The pick asks for fast on a program that has no fast switch.
    #[error("`{program}` has no fast mode, so `fast` cannot be set")]
    UnsupportedFast {
        /// The harness id.
        program: String,
    },
    /// A model name is blank.
    #[error("a model name is blank")]
    EmptyModel,
    /// A program takes its brief as a file but its harness names no flag.
    #[error("`{program}` takes its brief as a system-prompt file but names no flag")]
    BriefWithoutFlag {
        /// The harness id.
        program: String,
    },
    /// A catalog was asked for from a program that has none.
    #[error("`{program}` has no catalog to read")]
    NoCatalog {
        /// The harness id.
        program: String,
    },
    /// A launch template could not be filled.
    #[error(transparent)]
    Spec(#[from] SpecError),
    /// A catalog could not be read.
    #[error(transparent)]
    Models(#[from] ModelError),
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::spec::{BriefDelivery, CatalogParser, EffortSource, PermissionMode};

    /// The harness `id` from `data/harness/`, as a role's launch would load it.
    fn harness(id: &str) -> Harness {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../data/harness")
            .join(format!("{id}.toml"));
        match Harness::load(&path) {
            Ok(harness) => harness,
            Err(error) => panic!("{id} should load: {error}"),
        }
    }

    fn brief() -> Brief {
        Brief {
            text: "line one\nline two\n".to_owned(),
            file: PathBuf::from("/tmp/shprd/brief.md"),
        }
    }

    fn base_env() -> BTreeMap<String, String> {
        let mut base = BTreeMap::new();
        for (name, value) in [
            ("PATH", "/usr/bin:/bin"),
            ("HOME", "/home/dev"),
            ("CLAUDECODE", "1"),
            ("CLAUDE_CODE_SESSION_ID", "parent-session"),
        ] {
            base.insert(name.to_owned(), value.to_owned());
        }
        base
    }

    fn ctx() -> EnvContext {
        EnvContext {
            actor: "agent:claude".to_owned(),
            task: Some("M9.2".to_owned()),
            run: Some("run-1".to_owned()),
            repo: PathBuf::from("/repo"),
        }
    }

    fn host<'a>(base: &'a BTreeMap<String, String>, env: &'a EnvContext) -> Host<'a> {
        Host {
            base_env: base,
            role: EnvRole::Worker,
            env,
        }
    }

    fn pick(program: &str) -> Pick {
        Pick {
            program: program.to_owned(),
            ..Pick::default()
        }
    }

    /// The launch for `pick` against the shipped harness `id`.
    fn plan(id: &str, pick: Pick) -> Launch {
        let base = base_env();
        let env = ctx();
        match launch(&harness(id), &pick, &brief(), &host(&base, &env)) {
            Ok(launch) => launch,
            Err(error) => panic!("{id} should launch: {error}"),
        }
    }

    /// The words of an argv, for a readable assertion.
    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn claude_gets_model_effort_fast_mode_and_its_brief_file() {
        let launch = plan(
            "claude",
            Pick {
                model: Some("opus".to_owned()),
                effort: Some("high".to_owned()),
                fast: true,
                perm: Some("acceptEdits".to_owned()),
                ..pick("claude")
            },
        );
        assert_eq!(
            launch.argv,
            argv(&[
                "claude",
                "--model",
                "opus",
                "--effort",
                "high",
                "--settings",
                "{\"fastMode\":true}",
                "--permission-mode",
                "acceptEdits",
                "--append-system-prompt-file",
                "/tmp/shprd/brief.md",
            ])
        );
        assert_eq!(launch.brief, BriefRoute::InArgs);
        assert!(
            !launch.argv.iter().any(|arg| arg.contains("line one")),
            "the brief's words never travel as an argument: {:?}",
            launch.argv
        );
    }

    #[test]
    fn fast_off_writes_the_settings_that_turn_it_off() {
        let launch = plan("claude", pick("claude"));
        assert!(
            launch
                .argv
                .windows(2)
                .any(|pair| pair == argv(&["--settings", "{\"fastMode\":false}"]).as_slice()),
            "{:?}",
            launch.argv
        );
    }

    #[test]
    fn codex_gets_its_model_effort_and_mode_and_types_the_brief() {
        let launch = plan(
            "codex",
            Pick {
                model: Some("gpt-5.6-terra".to_owned()),
                effort: Some("xhigh".to_owned()),
                perm: Some("auto".to_owned()),
                ..pick("codex")
            },
        );
        assert_eq!(
            launch.argv,
            argv(&[
                "codex",
                "--no-daemon",
                "-c",
                "check_for_update_on_startup=false",
                "-m",
                "gpt-5.6-terra",
                "-c",
                "model_reasoning_effort=xhigh",
                "-s",
                "workspace-write",
                "-a",
                "on-request",
            ])
        );
        assert_eq!(launch.brief, BriefRoute::Typed);
    }

    #[test]
    fn pi_gets_its_provider_qualified_model_thinking_and_tools() {
        let launch = plan(
            "pi",
            Pick {
                model: Some("Command Code/deepseek/deepseek-v4.1-flash".to_owned()),
                effort: Some("high".to_owned()),
                perm: Some("read-only".to_owned()),
                ..pick("pi")
            },
        );
        assert_eq!(
            launch.argv,
            argv(&[
                "pi",
                "--model",
                "Command Code/deepseek/deepseek-v4.1-flash",
                "--thinking",
                "high",
                "--tools",
                "read,grep,find,ls",
                "--append-system-prompt",
                "/tmp/shprd/brief.md",
            ])
        );
        assert_eq!(launch.brief, BriefRoute::InArgs);
    }

    #[test]
    fn opencode_gets_its_model_and_mode_and_types_the_brief() {
        let launch = plan(
            "opencode",
            Pick {
                model: Some("opencode/claude-opus-5".to_owned()),
                perm: Some("auto".to_owned()),
                ..pick("opencode")
            },
        );
        assert_eq!(
            launch.argv,
            argv(&["opencode", "-m", "opencode/claude-opus-5", "--auto"])
        );
        assert_eq!(launch.brief, BriefRoute::Typed);
    }

    #[test]
    fn a_role_that_names_no_mode_gets_the_programs_default() {
        let claude = plan("claude", pick("claude"));
        assert_eq!(
            &claude.argv[1..],
            argv(&[
                "--settings",
                "{\"fastMode\":false}",
                "--permission-mode",
                "manual",
                "--append-system-prompt-file",
                "/tmp/shprd/brief.md",
            ])
            .as_slice()
        );
        let codex = plan("codex", pick("codex"));
        assert!(
            codex
                .argv
                .windows(4)
                .any(|window| window == argv(&["-s", "read-only", "-a", "never"]).as_slice()),
            "codex should start in its read-only default: {:?}",
            codex.argv
        );
    }

    #[test]
    fn every_mode_of_every_program_reaches_the_argv() {
        for id in ["claude", "codex", "pi", "opencode", "kilo"] {
            let harness = harness(id);
            let modes: Vec<PermissionMode> = harness.permissions().modes.clone();
            assert!(!modes.is_empty(), "{id} should have modes");
            for mode in &modes {
                let launch = plan(
                    id,
                    Pick {
                        perm: Some(mode.id.clone()),
                        ..pick(id)
                    },
                );
                if mode.args.is_empty() {
                    continue;
                }
                let found = launch
                    .argv
                    .windows(mode.args.len())
                    .any(|window| window == mode.args.as_slice());
                assert!(
                    found,
                    "{id}: mode {} should put {:?} in {:?}",
                    mode.id, mode.args, launch.argv
                );
            }
        }
    }

    #[test]
    fn every_program_takes_its_brief_the_way_its_harness_says() {
        for id in ["claude", "codex", "pi", "opencode", "kilo"] {
            let harness = harness(id);
            let launch = plan(id, pick(id));
            let wanted = match harness.brief().delivery {
                BriefDelivery::FirstMessage => BriefRoute::Typed,
                BriefDelivery::SystemPromptFile => BriefRoute::InArgs,
            };
            assert_eq!(launch.brief, wanted, "{id}");
            if harness.brief().delivery == BriefDelivery::SystemPromptFile {
                assert!(
                    launch.argv.iter().any(|arg| arg == "/tmp/shprd/brief.md"),
                    "{id} should be pointed at the brief file: {:?}",
                    launch.argv
                );
            } else {
                assert!(
                    !launch.argv.iter().any(|arg| arg == "/tmp/shprd/brief.md"),
                    "{id} should not name the brief file: {:?}",
                    launch.argv
                );
            }
        }
    }

    #[test]
    fn an_unknown_mode_is_refused_with_the_ones_that_work() {
        let harness = harness("claude");
        let choice = Pick {
            perm: Some("yolo".to_owned()),
            ..pick("claude")
        };
        let error = match choice.validate(&harness) {
            Ok(()) => panic!("an unknown mode should be refused"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            AdapterError::UnknownPermission { ref mode, .. } if mode == "yolo"
        ));
        assert!(error.to_string().contains("acceptEdits"), "{error}");
    }

    #[test]
    fn a_choice_the_program_cannot_take_is_refused() {
        let mut choice = pick("opencode");
        choice.effort = Some("high".to_owned());
        assert!(matches!(
            choice.validate(&harness("opencode")),
            Err(AdapterError::UnsupportedEffort { .. })
        ));

        let mut choice = pick("pi");
        choice.effort = Some("turbo".to_owned());
        match choice.validate(&harness("pi")) {
            Err(AdapterError::UnknownEffort { allowed, .. }) => {
                assert!(allowed.contains("xhigh"), "{allowed}");
            }
            other => panic!("expected UnknownEffort, got {other:?}"),
        }

        let mut choice = pick("codex");
        choice.fast = true;
        assert!(matches!(
            choice.validate(&harness("codex")),
            Err(AdapterError::UnsupportedFast { .. })
        ));

        let mut choice = pick("claude");
        choice.model = Some("  ".to_owned());
        assert!(matches!(
            choice.validate(&harness("claude")),
            Err(AdapterError::EmptyModel)
        ));

        let choice = Pick {
            program: "codex".to_owned(),
            ..Pick::default()
        };
        assert!(matches!(
            choice.validate(&harness("claude")),
            Err(AdapterError::WrongProgram { .. })
        ));
    }

    #[test]
    fn a_valid_pick_passes_for_every_program() {
        for id in ["claude", "codex", "pi", "opencode", "kilo"] {
            let harness = harness(id);
            let modes = harness.permissions().modes.clone();
            let pick = Pick {
                perm: modes.first().map(|mode| mode.id.clone()),
                effort: harness.efforts().levels.first().cloned(),
                ..pick(id)
            };
            assert!(
                pick.validate(&harness).is_ok(),
                "{id}: {pick:?} should validate"
            );
        }
    }

    #[test]
    fn a_launch_drops_a_parent_claude_session() {
        let launch = plan("claude", pick("claude"));
        let base = base_env();
        let applied = launch.env.apply_to(&base);
        assert!(!applied.contains_key("CLAUDECODE"));
        assert!(!applied.contains_key("CLAUDE_CODE_SESSION_ID"));
        assert_eq!(
            applied.get("SHPRD_ACTOR").map(String::as_str),
            Some("agent:claude")
        );
        assert_eq!(applied.get("SHPRD_TASK").map(String::as_str), Some("M9.2"));
    }

    #[test]
    fn a_brief_that_travels_as_contents_lands_in_the_argv() {
        let text = r#"
id = "fake"
name = "Fake"
vendor = "pi"
verified_version = "1.0.0"
dry_patterns = []

[detect]
binary = "fake"
version_args = ["--version"]
signin_hint = "fake"

[launch]
interactive = ["fake"]
model_args = ["--model", "{model}"]
system_prompt_args = ["--system-prompt", "{prompt}"]
verified = false

[models]

[brief]
delivery = "system-prompt-file"
file = "contents"

[records]
kind = "jsonl"
location = "~/.fake/{session}.jsonl"
errors = "error"
verified = false

[quota]
source = "none"

[skills]
home_dirs = []
project_dirs = []
"#;
        let dir = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(error) => panic!("tempdir failed: {error}"),
        };
        let path = dir.path().join("fake.toml");
        if let Err(error) = std::fs::write(&path, text) {
            panic!("the fixture should be written: {error}");
        }
        let harness = match Harness::load(&path) {
            Ok(harness) => harness,
            Err(error) => panic!("the fixture should load: {error}"),
        };
        let base = base_env();
        let env = ctx();
        let launch = match launch(
            &harness,
            &pick("fake"),
            &Brief {
                text: "do the thing".to_owned(),
                file: PathBuf::from("/tmp/shprd/brief.md"),
            },
            &host(&base, &env),
        ) {
            Ok(launch) => launch,
            Err(error) => panic!("the fake should launch: {error}"),
        };
        assert_eq!(
            launch.argv,
            argv(&["fake", "--system-prompt", "do the thing"])
        );
        assert_eq!(launch.brief, BriefRoute::InArgs);
    }

    #[test]
    fn a_catalog_is_read_the_way_its_harness_says() {
        let models_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/models");
        let claude = harness("claude");
        let catalog = match catalog_from_static(&claude, &models_dir) {
            Ok(Some(catalog)) => catalog,
            other => panic!("claude should have a static catalog: {other:?}"),
        };
        assert!(catalog.names().contains(&"opus"));

        let codex = harness("codex");
        let output = fixture("codex-models.json");
        let catalog = match catalog_from_output(&codex, &output) {
            Ok(catalog) => catalog,
            Err(error) => panic!("codex's catalog should read: {error}"),
        };
        assert_eq!(
            catalog.names(),
            ["gpt-6-luna", "gpt-5.6-terra", "gpt-5.6-luna"]
        );

        let pi = harness("pi");
        let catalog = match catalog_from_output(&pi, &fixture("pi-models.txt")) {
            Ok(catalog) => catalog,
            Err(error) => panic!("pi's catalog should read: {error}"),
        };
        assert_eq!(
            catalog.names().first().copied(),
            Some("Command Code/deepseek/deepseek-v4.1-flash")
        );

        let opencode = harness("opencode");
        let catalog = match catalog_from_output(&opencode, &fixture("opencode-models.txt")) {
            Ok(catalog) => catalog,
            Err(error) => panic!("opencode's catalog should read: {error}"),
        };
        assert_eq!(catalog.names().first().copied(), Some("opencode/beam-free"));
    }

    #[test]
    fn a_catalog_is_read_only_from_a_program_that_lists_one() {
        let claude = harness("claude");
        assert!(matches!(
            catalog_from_output(&claude, ""),
            Err(AdapterError::NoCatalog { .. })
        ));
        let opencode = harness("opencode");
        let models_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/models");
        assert!(matches!(
            catalog_from_static(&opencode, &models_dir),
            Ok(None)
        ));
    }

    #[test]
    fn each_program_names_its_catalog_parser_and_effort_source() {
        for (id, parser, source) in [
            ("codex", CatalogParser::CodexCatalog, EffortSource::Catalog),
            ("pi", CatalogParser::PiTable, EffortSource::Static),
            ("opencode", CatalogParser::FirstToken, EffortSource::None),
        ] {
            let harness = harness(id);
            assert_eq!(harness.models().parser, parser, "{id}");
            assert_eq!(harness.efforts().source, source, "{id}");
        }
    }

    /// A captured command output the catalog tests read.
    fn fixture(name: &str) -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) => panic!("{} should be readable: {error}", path.display()),
        }
    }
}
