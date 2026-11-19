//! The roster route: the programs a role may run, with their models, effort
//! levels and access modes.
//!
//! A settings page draws three pickers per role — the program, the model and the
//! effort level — and every choice it may offer comes from one of two tables the
//! binary already carries: the harness table (`data/harness/*.toml`) says which
//! programs exist and what each one takes, and the model catalogs
//! (`data/models/*.toml`, or a program asking itself) say which models a program
//! runs. A Herdr mod has neither, so the service answers both here rather than
//! leaving the page to invent them.
//!
//! Two things this route deliberately does not do:
//!
//! - It never checks anything the person wrote. That is [`crate::settings`]'s
//!   job, with [`shprd_agents::roles`]; this is the menu, not the refusal.
//! - It does not hide a program whose binary is missing. A machine with no
//!   `codex` still has the harness entry, and the person may be about to install
//!   it; a menu that changed shape with `$PATH` would be a menu whose absence
//!   says nothing. The models are what a missing binary costs: a program that
//!   lists its own models answers nothing, which `models_note` says.
//!
//! A program that lists its models itself is asked, with a short bound, because
//! a picker with no models is a text box. Which `$PATH` that call runs with is
//! the binary's to resolve and pass in ([`crate::api::Paths::path_env`]): a
//! service prepared without one reads only the shipped static catalogs, so a
//! test never starts a program on this machine.

use std::time::Duration;

use shprd_agents::data::DataDir;
use shprd_agents::detect::{find_binary, vendor_name};
use shprd_agents::models::{self, Catalog};
use shprd_agents::spec::{CatalogSource, HarnessSpec};

use crate::dto::{ModelChoiceDto, ModelsSourceDto, PermissionDto, ProgramDto, RosterDto};

/// How long one program's own model listing may take.
///
/// The catalogue feeds a menu, so a slow or hung CLI must not hold the page: a
/// program that does not answer in this long is offered with a text box and the
/// note saying so. The listings run at once, so this is the whole answer's
/// bound, not one program's share of it.
const LIST_TIMEOUT: Duration = Duration::from_secs(3);

/// Every program this machine's harness table defines, in the table's order.
///
/// `path_env` is the `PATH` the binary resolved, when it resolved one. It is
/// `None` for a service that was built by hand (a test, for one), and then a
/// program's own list command is not run at all: only the shipped catalogs are
/// read.
pub(crate) fn read(data: Option<&DataDir>, path_env: Option<&str>) -> RosterDto {
    let Some(data) = data else {
        return RosterDto {
            programs: Vec::new(),
            note: Some(format!(
                "this service was not told where the harness table is, so it has no programs to \
                 offer: start `shprd serve` from an install with a data directory (or set {})",
                shprd_agents::harness::DATA_ENV
            )),
        };
    };
    let specs = match data.harness_specs() {
        Ok(specs) => specs,
        Err(error) => {
            return RosterDto {
                programs: Vec::new(),
                note: Some(format!("reading {}: {error}", data.label())),
            };
        }
    };
    // A program that lists its own models is a process, and four of them in a
    // row would make a picker wait four times over. They are started together
    // and read in the table's order, so the answer takes the slowest listing's
    // time rather than their sum.
    let catalogs = listings(&specs, data, path_env);
    RosterDto {
        programs: specs
            .iter()
            .zip(catalogs)
            .map(|(spec, catalog)| program(spec, catalog, installed(spec, path_env)))
            .collect(),
        note: None,
    }
}

/// Whether `spec`'s CLI is on the `PATH` the binary resolved.
///
/// It is a path lookup, not a probe: the tab bar's "+" menu dims a program it
/// cannot start and nothing else here needs the version or the sign-in (that is
/// `shprd roster`'s job, which runs the CLIs). A service that was told no `PATH`
/// was built by hand, so nothing is claimed to be installed.
fn installed(spec: &HarnessSpec, path_env: Option<&str>) -> bool {
    path_env.is_some_and(|env| find_binary(&spec.detect.binary, env).is_some())
}

/// Every program's catalog, in the specs' order, listed at once.
///
/// The static ones are read here (a file each); the ones a program lists itself
/// run together, each under [`LIST_TIMEOUT`]. `None` is "no list": a program
/// with no catalog, or one whose list failed.
fn listings(specs: &[HarnessSpec], data: &DataDir, path_env: Option<&str>) -> Vec<Option<Catalog>> {
    std::thread::scope(|scope| {
        let running: Vec<_> = specs
            .iter()
            .map(|spec| {
                scope.spawn(move || match (spec.models.catalog_source(), path_env) {
                    // A request may read the shipped data, but it starts a
                    // program only when the binary said which `$PATH` to find it
                    // with.
                    (CatalogSource::Static, _) => models::static_catalog(spec, data).ok().flatten(),
                    (CatalogSource::Command, Some(env)) => data.catalog(spec, env, LIST_TIMEOUT),
                    (CatalogSource::Command, None) | (CatalogSource::None, _) => None,
                })
            })
            .collect();
        running
            .into_iter()
            .map(|handle| handle.join().ok().flatten())
            .collect()
    })
}

/// One program's entry: its identity and everything a picker offers for it.
fn program(spec: &HarnessSpec, catalog: Option<Catalog>, installed: bool) -> ProgramDto {
    let source = spec.models.catalog_source();
    let model_choices = catalog.as_ref().map(models_of).unwrap_or_default();
    let efforts = catalog
        .as_ref()
        .map(|catalog| catalog.efforts.clone())
        // A program whose levels come from `--help` or from its catalog has none
        // in the spec, so an unread catalog leaves them unknown rather than
        // claiming the program takes nothing.
        .filter(|levels| !levels.is_empty())
        .unwrap_or_else(|| spec.models.efforts.levels.clone());
    ProgramDto {
        id: spec.id.clone(),
        name: spec.name.clone(),
        installed,
        vendor: vendor_name(spec.vendor).to_string(),
        multi_model: spec.multi_model,
        needs_provider: spec.models.provider_qualified,
        models_note: note_for(spec, source, &model_choices),
        models: model_choices,
        models_source: match source {
            CatalogSource::Static => ModelsSourceDto::Static,
            CatalogSource::Command => ModelsSourceDto::Command,
            CatalogSource::None => ModelsSourceDto::None,
        },
        efforts,
        fast: spec.fast.available(),
        permissions: spec
            .permissions
            .modes
            .iter()
            .map(|mode| PermissionDto {
                id: mode.id.clone(),
                label: mode.label.clone(),
            })
            .collect(),
        default_permission: spec.permissions.default.clone(),
    }
}

/// A catalog's models, as the picker draws them.
fn models_of(catalog: &Catalog) -> Vec<ModelChoiceDto> {
    catalog
        .models
        .iter()
        .map(|model| ModelChoiceDto {
            id: model.id.clone(),
            name: model.name.clone(),
            efforts: model.efforts.clone(),
            default_effort: model.default_effort.clone(),
            fast: model.fast,
        })
        .collect()
}

/// Why there are no models, when there are none.
///
/// A list that is there is not explained: the picker shows the models, and a
/// sentence about where they came from would be one line of noise per program.
fn note_for(
    spec: &HarnessSpec,
    source: CatalogSource,
    models: &[ModelChoiceDto],
) -> Option<String> {
    if !models.is_empty() {
        return None;
    }
    match source {
        CatalogSource::Static => Some(String::from(
            "its catalog in the shipped data lists no model, so type one",
        )),
        CatalogSource::Command => {
            let command = spec
                .models
                .list_command
                .as_ref()
                .map(|parts| parts.join(" "))
                .unwrap_or_else(|| spec.id.clone());
            Some(format!(
                "`{command}` gave no model list, so type a model id"
            ))
        }
        CatalogSource::None => Some(String::from("it names no model list, so type a model id")),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::path::Path;

    use super::*;

    /// The shipped `data/` directory: the real harness and family tables.
    fn shipped() -> DataDir {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../data")
            .canonicalize()
            .expect("the shipped data directory");
        DataDir::resolve(Some(&path), None)
    }

    /// The entry called `id`.
    fn program_of(roster: &RosterDto, id: &str) -> ProgramDto {
        roster
            .programs
            .iter()
            .find(|program| program.id == id)
            .unwrap_or_else(|| panic!("no program `{id}` in {}", roster.programs.len()))
            .clone()
    }

    #[test]
    fn every_program_in_the_harness_table_is_offered() {
        let roster = read(Some(&shipped()), None);
        let ids: Vec<&str> = roster
            .programs
            .iter()
            .map(|program| program.id.as_str())
            .collect();
        assert_eq!(ids, ["claude", "codex", "kilo", "opencode", "pi"]);
        assert_eq!(roster.note, None);
    }

    #[test]
    fn a_static_catalog_is_read_without_running_anything() {
        // No `$PATH`: the service was prepared by hand, so no program may be
        // started. Claude Code's models are in the shipped data, so it still
        // has a full picker.
        let roster = read(Some(&shipped()), None);
        let claude = program_of(&roster, "claude");
        assert_eq!(claude.models_source, ModelsSourceDto::Static);
        let ids: Vec<&str> = claude
            .models
            .iter()
            .map(|model| model.id.as_str())
            .collect();
        assert_eq!(
            ids,
            [
                "fable",
                "fable[1m]",
                "claude-fable-5-1",
                "opus",
                "opus[1m]",
                "sonnet",
                "sonnet[1m]",
                "haiku"
            ],
            "Haiku and the one-million-token variants are listed, in the ids the CLI takes"
        );
        let names: Vec<&str> = claude
            .models
            .iter()
            .map(|model| model.name.as_str())
            .collect();
        assert_eq!(
            names,
            [
                "Fable 5.2",
                "Fable 5.2 · 1M",
                "Fable 5.1",
                "Opus 5.5",
                "Opus 5.5 · 1M",
                "Sonnet 5.5",
                "Sonnet 5.5 · 1M",
                "Haiku 5.5"
            ]
        );
        assert!(
            claude.models[3].fast && !claude.models[7].fast,
            "fast is Opus's"
        );
        assert_eq!(claude.efforts, ["low", "medium", "high", "xhigh", "max"]);
        assert_eq!(claude.default_permission.as_deref(), Some("manual"));
        let modes: Vec<&str> = claude
            .permissions
            .iter()
            .map(|mode| mode.id.as_str())
            .collect();
        assert!(modes.contains(&"auto"), "{modes:?}");
        assert!(claude.fast, "Claude Code has a fast mode");
        assert_eq!(claude.models_note, None);
    }

    #[test]
    fn a_program_that_lists_its_models_is_not_asked_without_a_path() {
        // pi lists its models itself; with no `$PATH` passed in it is offered
        // with a note and no models, never with a list this machine might have.
        let roster = read(Some(&shipped()), None);
        let pi = program_of(&roster, "pi");
        assert_eq!(pi.models_source, ModelsSourceDto::Command);
        assert!(pi.models.is_empty());
        assert!(pi.needs_provider, "pi names its provider in the model id");
        let note = pi.models_note.unwrap_or_default();
        assert!(note.contains("--list-models"), "{note}");
        assert!(note.contains("type a model id"), "{note}");
    }

    /// Codex's own catalog is read even when it is bigger than a pipe buffer: the
    /// real `codex debug models` prints about 260 KB, which a probe that does not
    /// drain its pipe reads as a timeout, so the picker had no Codex models.
    #[cfg(unix)]
    #[test]
    fn codex_models_come_from_codex_even_when_its_catalog_is_large() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::TempDir::new().expect("a temp dir");
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../shprd-agents/tests/fixtures/codex-models.json");
        let script = dir.path().join("codex");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\ncat '{}'\nhead -c 300000 /dev/zero | tr '\\0' ' '\n",
                fixture.display()
            ),
        )
        .expect("a fake codex");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .expect("executable");
        let path = format!("{}:/usr/bin:/bin", dir.path().display());
        let roster = read(Some(&shipped()), Some(&path));
        let codex = program_of(&roster, "codex");
        assert!(codex.installed);
        assert!(!codex.models.is_empty(), "{:?}", codex.models_note);
        assert!(
            codex.models.iter().all(|model| !model.id.is_empty()),
            "every listed model has the id codex takes"
        );
    }

    #[test]
    fn a_service_with_no_data_directory_says_so() {
        let roster = read(None, None);
        assert!(roster.programs.is_empty());
        let note = roster.note.unwrap_or_default();
        assert!(note.contains("SHPRD_DATA_DIR"), "{note}");
    }

    #[test]
    fn a_broken_harness_table_is_a_note_and_no_programs() {
        let dir = tempfile::TempDir::new().expect("a temp dir");
        let harness = dir.path().join("harness");
        std::fs::create_dir_all(&harness).expect("the harness dir");
        std::fs::write(harness.join("claude.toml"), "id = \"claude\"\n").expect("a broken spec");
        let data = DataDir::resolve(Some(dir.path()), Some(Path::new("/nonexistent-data-home")));
        let roster = read(Some(&data), None);
        assert!(roster.programs.is_empty());
        assert!(
            roster.note.unwrap_or_default().contains("harness"),
            "the note names what could not be read"
        );
    }
}
