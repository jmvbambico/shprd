//! Roles checked against the harness table, and the two rules a review obeys.
//!
//! [`shprd_core::settings`] resolves *what* a role is set to; this module says
//! whether the answer is one the harnesses on this machine can run, and whether
//! the review pair makes sense:
//!
//! 1. **Only a review role may review.** A role reviews when its kind is
//!    `reviewer` or when it is marked `readonly`, so the engine can apply the
//!    program's read-only mode and never auto-approve on the reviewer's behalf.
//!    A refusal names the roles that can review.
//! 2. **The reviewer must be another vendor than the worker.** The vendor is the
//!    *model's* family ([`crate::families`]), not the CLI's: a pi worker running
//!    DeepSeek may be reviewed by a pi reviewer running Claude, and a pi worker
//!    running Claude may not. A refusal names both roles and both families.
//!
//! The rest is the pick: the program must be one `data/harness/` defines, the
//! model must be one the program can take (its provider, when the program needs
//! one), the access mode must be one of that program's `[permissions]` ids, the
//! effort level one of its levels, and every fallback program must exist.
//!
//! A role that names no program is checked no further: the person has not chosen
//! yet, and the program's own defaults run it. A role that names a program but
//! no access mode runs that program's `[permissions] default` — `manual` for
//! Claude Code, `read-only` for Codex — because the harness table is the one
//! place that says what a program's own default is; the settings model never
//! substitutes a mode of its own.
//!
//! Borrowed behaviour: the reviewer capability check and the cross-vendor
//! default come from Shepherdr (`eliasstravik/shepherdr`, `core/settings.js`
//! `otherCli()` and `roleSel`, commit a3f327a, MIT). Shepherdr never enforces
//! cross-vendor; shprd's rule is its own (`docs/roles.md`), reusing Shepherdr's
//! mechanism for naming the pair.

use std::collections::BTreeMap;
use std::path::Path;

use shprd_core::settings::{Resolved, ResolvedRole};
use thiserror::Error;

use crate::families::{Families, FamilyError, agent_vendor};
use crate::models::{AgentRef, Catalog};
use crate::spec::{HarnessSpec, SpecError, load_dir};

/// Which two roles a review compares.
///
/// The engine asks for the roles by id: the built-in `worker` and `reviewer`
/// unless the caller says otherwise, because a custom role may be the one
/// working a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Review<'a> {
    /// The role that did the work.
    pub worker: &'a str,
    /// The role that reads it.
    pub reviewer: &'a str,
}

impl Default for Review<'_> {
    fn default() -> Self {
        Self {
            worker: "worker",
            reviewer: "reviewer",
        }
    }
}

/// The harness table and the model-family table, loaded together.
///
/// Every check in this module needs both: the specs say what a program can be
/// asked for, the families say whose model it would be running.
#[derive(Debug, Clone, Default)]
pub struct Table {
    /// One entry per `data/harness/*.toml`.
    pub specs: Vec<HarnessSpec>,
    /// The model families, from `data/models/families.toml`.
    pub families: Families,
}

impl Table {
    /// Load a data directory: its `harness/` specs and its `models/families.toml`.
    pub fn load(dir: &Path) -> Result<Table, TableError> {
        let specs = load_dir(&dir.join("harness"))?;
        let families = Families::load(&dir.join("models").join("families.toml"))?;
        Ok(Table { specs, families })
    }
}

/// Why a data directory could not be loaded.
#[derive(Debug, Error)]
pub enum TableError {
    /// The harness specs are missing or malformed.
    #[error("loading the harness table: {0}")]
    Specs(#[from] SpecError),
    /// The model family table is missing or malformed.
    #[error("loading the model families: {0}")]
    Families(#[from] FamilyError),
}

/// Everything wrong with a resolved set of roles, in the order it was found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Problems {
    messages: Vec<String>,
}

impl Problems {
    /// The refusals, one line each.
    pub fn messages(&self) -> &[String] {
        &self.messages
    }

    /// Whether anything is wrong.
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Record one refusal.
    fn push(&mut self, message: impl Into<String>) {
        self.messages.push(message.into());
    }
}

impl std::fmt::Display for Problems {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.messages.join("; "))
    }
}

impl std::error::Error for Problems {}

/// A loaded harness table, or the reason this machine has none.
///
/// The reason is carried rather than swallowed: a caller that cannot check the
/// roles must say so, and must never store a role nothing looked at. A machine
/// without a data directory can still read and write the workflow keys, so
/// building a checker never fails — only the checks do.
#[derive(Debug, Clone)]
pub struct Checker {
    table: Result<Table, String>,
    /// The catalogs the checks may consult, by program id.
    ///
    /// Only the effort check needs them: a program's own levels are in the
    /// harness table, but whether a *model* can think is in its catalog. An
    /// empty map means nothing is known, and the check says nothing rather than
    /// guessing.
    catalogs: BTreeMap<String, Catalog>,
}

impl Default for Checker {
    /// A checker with no table: every check reports that it was skipped.
    fn default() -> Self {
        Checker::load(None)
    }
}

impl Checker {
    /// Load `data`'s harness table and model families, keeping the reason when
    /// there is nothing to load.
    ///
    /// `data` is [`crate::harness::data_dir`]'s answer for this machine and
    /// repository.
    pub fn load(data: Option<&Path>) -> Checker {
        let Some(dir) = data else {
            return Checker {
                table: Err(format!(
                    "no harness data directory: set {} to a directory holding `harness/` and \
                     `models/`, or run `shprd doctor`",
                    crate::harness::DATA_ENV
                )),
                catalogs: BTreeMap::new(),
            };
        };
        Checker {
            table: Table::load(dir).map_err(|error| format!("reading {}: {error}", dir.display())),
            catalogs: BTreeMap::new(),
        }
    }

    /// A checker over a table the caller already holds.
    pub fn with(table: Table) -> Checker {
        Checker {
            table: Ok(table),
            catalogs: BTreeMap::new(),
        }
    }

    /// The same checker, with the model catalogs its effort check may consult.
    ///
    /// A caller that has read a program's catalog (a static list, or its own
    /// listing) hands it in here, so a model that cannot think is told apart
    /// from one that can. Catalogs are hints: absent is not an error.
    pub fn with_catalogs(mut self, catalogs: BTreeMap<String, Catalog>) -> Checker {
        self.catalogs = catalogs;
        self
    }

    /// The harness specs, when this machine has a table.
    pub fn specs(&self) -> Option<&[HarnessSpec]> {
        self.table.as_ref().ok().map(|table| table.specs.as_slice())
    }

    /// Whether the roles can be checked at all on this machine.
    pub fn is_available(&self) -> bool {
        self.table.is_ok()
    }

    /// Why the roles cannot be checked, when they cannot.
    pub fn reason(&self) -> Option<&str> {
        self.table.as_ref().err().map(String::as_str)
    }

    /// The refusals these settings have, or why they could not be checked.
    pub fn check(&self, resolved: &Resolved, review: Review<'_>) -> Checked {
        match &self.table {
            Err(reason) => Checked {
                problems: Problems::default(),
                skipped: Some(reason.clone()),
                warnings: Vec::new(),
            },
            Ok(table) => {
                let mut warnings = Vec::new();
                check_effort_notes(resolved, &self.catalogs, &mut warnings);
                match check(resolved, review, table) {
                    Ok(()) => Checked {
                        warnings,
                        ..Checked::default()
                    },
                    Err(problems) => Checked {
                        problems,
                        skipped: None,
                        warnings,
                    },
                }
            }
        }
    }
}

/// What checking one set of settings produced.
///
/// A clean answer and an unchecked one must not look alike, so the reason the
/// table is missing travels with the (empty) refusals.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Checked {
    /// The refusals the resolved roles have.
    problems: Problems,
    /// Why the roles could not be checked at all, when they could not.
    skipped: Option<String>,
    /// The settings that work but do not do what they look like: an effort
    /// level on a model whose catalog says it cannot think, for one.
    warnings: Vec<String>,
}

impl Checked {
    /// The refusals, one line each.
    pub fn messages(&self) -> &[String] {
        self.problems.messages()
    }

    /// The warnings, one line each: nothing here blocks a write.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Whether nothing is wrong.
    pub fn is_empty(&self) -> bool {
        self.problems.is_empty()
    }

    /// Why the roles could not be checked, when they could not.
    pub fn skipped(&self) -> Option<&str> {
        self.skipped.as_deref()
    }

    /// The refusals `self` has that `before` did not.
    ///
    /// A file someone else left broken is not this change's to answer, so a
    /// write only answers for what it introduces.
    pub fn introduced(&self, before: &Checked) -> Vec<&str> {
        self.messages()
            .iter()
            .map(String::as_str)
            .filter(|message| !before.messages().iter().any(|seen| seen == message))
            .collect()
    }
}

/// Check every resolved role against the harness table, then the review pair.
///
/// The roles are checked first, so a refusal about a program comes before one
/// about the review: a role that cannot run at all is the more useful thing to
/// say. A set with nothing wrong returns `Ok(())`.
pub fn check(resolved: &Resolved, review: Review<'_>, table: &Table) -> Result<(), Problems> {
    let mut problems = Problems::default();
    for role in resolved.roles() {
        check_role(&role, table, &mut problems);
    }
    check_review(resolved, review, table, &mut problems);
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems)
    }
}

/// Check one role's pick against the program it names.
fn check_role(role: &ResolvedRole, table: &Table, problems: &mut Problems) {
    let Some(agent) = &role.agent else {
        return;
    };
    let program = agent.value.as_str();
    let known = table
        .specs
        .iter()
        .map(|spec| spec.id.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let Some(spec) = table.specs.iter().find(|spec| spec.id == program) else {
        problems.push(format!(
            "role `{}` names the program `{program}`, which no harness defines: known programs are {known}",
            role.id
        ));
        return;
    };
    check_model(role, spec, table, problems);
    check_perm(role, spec, problems);
    check_effort(role, spec, problems);
    for fallback in &role.fallback.value {
        if !table.specs.iter().any(|spec| spec.id == *fallback) {
            problems.push(format!(
                "role `{}` falls back to `{fallback}`, which no harness defines: known programs are {known}",
                role.id
            ));
        }
    }
}

/// Check one role's effort level against the model catalog, when the caller has
/// read it.
///
/// A program's own levels say what the CLI accepts; its catalog says which of
/// them a *model* can use. The difference matters for `pi`, whose listing marks
/// some models `thinking: no`: the level is accepted by the program and does
/// nothing for that model, so it is a warning rather than a refusal.
fn check_effort_notes(
    resolved: &Resolved,
    catalogs: &BTreeMap<String, Catalog>,
    warnings: &mut Vec<String>,
) {
    for role in resolved.roles() {
        let (Some(program), Some(effort)) = (&role.agent, &role.effort) else {
            continue;
        };
        let Some(catalog) = catalogs.get(&program.value) else {
            continue;
        };
        let model = role.model.as_ref().map(|model| model.value.as_str());
        if let Some(note) = catalog.effort_warning(&program.value, model, &effort.value) {
            warnings.push(format!("role `{}`: {note}", role.id));
        }
    }
}

/// Check the model against the program that would run it.
fn check_model(role: &ResolvedRole, spec: &HarnessSpec, table: &Table, problems: &mut Problems) {
    let Some(model) = &role.model else {
        return;
    };
    let written = format!("{}/{}", spec.id, model.value);
    // Only the shape of the string is checked: a program's model catalogue can
    // be stale, so a model it does not list is a hint rather than a refusal
    // (`shprd set` is where that hint is printed).
    if let Err(error) = AgentRef::parse(&written, &table.specs) {
        problems.push(format!(
            "role `{}` names the model `{written}`: {error}",
            role.id
        ));
    }
}

/// Check the access mode against the program's own modes.
fn check_perm(role: &ResolvedRole, spec: &HarnessSpec, problems: &mut Problems) {
    let offers = spec
        .permissions
        .modes
        .iter()
        .map(|mode| mode.id.as_str())
        .collect::<Vec<_>>();
    let Some(perm) = &role.perm else {
        return;
    };
    if !offers.contains(&perm.value.as_str()) {
        problems.push(format!(
            "role `{}` names the access mode `{}`, which `{}` does not offer: it offers {}",
            role.id,
            perm.value,
            spec.id,
            if offers.is_empty() {
                "none".to_owned()
            } else {
                offers.join(", ")
            }
        ));
        return;
    }
    // A role that reviews never runs with full access: the engine starts it in
    // the read-only mode, and a person who set something stronger by hand is
    // told rather than silently overruled.
    if role.may_review() && spec.permissions.full.as_deref() == Some(perm.value.as_str()) {
        let readonly = spec
            .permissions
            .readonly
            .as_deref()
            .unwrap_or("its read-only mode");
        problems.push(format!(
            "role `{}` reviews, so it may not run in `{}`, which is full access: it runs in `{readonly}`",
            role.id, perm.value
        ));
    }
}

/// Check the effort level against the program's levels, when it states them.
fn check_effort(role: &ResolvedRole, spec: &HarnessSpec, problems: &mut Problems) {
    let Some(effort) = &role.effort else {
        return;
    };
    let levels = &spec.models.efforts.levels;
    // An empty list means the program states no levels (or reads them from a
    // catalogue nobody has read yet), so there is nothing to compare with.
    if levels.is_empty() || levels.contains(&effort.value) {
        return;
    }
    problems.push(format!(
        "role `{}` names the effort `{}`, which `{}` does not list: it lists {}",
        role.id,
        effort.value,
        spec.id,
        levels.join(", ")
    ));
}

/// Check the two review rules: who may review, and whether the vendors differ.
fn check_review(resolved: &Resolved, review: Review<'_>, table: &Table, problems: &mut Problems) {
    let Some(reviewer) = resolved.role(review.reviewer) else {
        problems.push(format!(
            "no role `{}` reviews: the roles are {}",
            review.reviewer,
            resolved.order().join(", ")
        ));
        return;
    };
    if !reviewer.may_review() {
        let roles = resolved.roles();
        let can: Vec<&str> = roles
            .iter()
            .filter(|role| role.may_review())
            .map(|role| role.id.as_str())
            .collect();
        problems.push(format!(
            "role `{}` may not review: only a role of kind `reviewer`, or one marked `readonly`, may review ({})",
            reviewer.id,
            if can.is_empty() {
                "none does".to_owned()
            } else {
                can.join(", ")
            }
        ));
        return;
    }
    if !resolved.cross_vendor() {
        return;
    }
    let Some(worker) = resolved.role(review.worker) else {
        problems.push(format!(
            "no worker role `{}`: the roles are {}",
            review.worker,
            resolved.order().join(", ")
        ));
        return;
    };
    let reviewer_vendor = vendor_of(&reviewer, table);
    let worker_vendor = vendor_of(&worker, table);
    // A role that names no program has no vendor yet: the person has not
    // chosen, and the check cannot be made rather than failed.
    let (Some(reviewer_vendor), Some(worker_vendor)) = (reviewer_vendor, worker_vendor) else {
        return;
    };
    if reviewer_vendor == worker_vendor {
        problems.push(format!(
            "the reviewer `{}` and the worker `{}` are both {reviewer_vendor}, so the review is not cross-vendor: set roles.{}.agent to a program from another family",
            reviewer.id, worker.id, reviewer.id
        ));
    }
}

/// The model family a role would run, when both its program and its model name
/// one.
fn vendor_of(role: &ResolvedRole, table: &Table) -> Option<String> {
    let agent = role.agent.as_ref()?;
    let parsed = match role.model.as_ref() {
        Some(model) => format!("{}/{}", agent.value, model.value),
        None => agent.value.clone(),
    };
    let agent = AgentRef::parse(&parsed, &table.specs).ok()?;
    agent_vendor(&agent, &table.specs, &table.families)
}

/// The vendor a role's model belongs to, in the words a settings page shows.
///
/// [`None`] when the role names no program, or one no family claims (a router
/// such as `kilo/kilo-auto/free`).
pub fn vendor(role: &ResolvedRole, table: &Table) -> Option<String> {
    vendor_of(role, table)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::path::Path;

    use shprd_core::settings::{File, Request, Resolved, resolve};

    use super::{Checker, Review, Table, check};

    /// The shipped `data/` directory, which the harness and family tables load
    /// from: the roles are checked against exactly the table shprd ships.
    fn table() -> Table {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data");
        match Table::load(&dir) {
            Ok(table) => table,
            Err(error) => panic!("the shipped data directory must load: {error}"),
        }
    }

    /// The settings one test document resolves to.
    fn resolved(text: &str) -> Resolved {
        let file = match File::parse(text) {
            Ok(file) => file,
            Err(error) => panic!("the test settings must parse: {error}"),
        };
        resolve(&file, &Request::default())
    }

    /// A captured pi listing read with the shipped pi spec, as the settings
    /// check sees it. No CLI runs: the table is the fixture.
    fn pi_catalog() -> crate::models::Catalog {
        let text = match std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pi-models-thinking.txt"),
        ) {
            Ok(text) => text,
            Err(error) => panic!("the pi fixture should be readable: {error}"),
        };
        let spec = match table().specs.iter().find(|spec| spec.id == "pi") {
            Some(spec) => spec.clone(),
            None => panic!("the shipped table defines pi"),
        };
        match crate::models::parse_catalog(&spec, &text) {
            Ok(catalog) => catalog,
            Err(error) => panic!("the pi fixture should read: {error}"),
        }
    }

    #[test]
    fn an_effort_a_model_cannot_use_is_a_warning_not_a_refusal() {
        let catalogs = std::collections::BTreeMap::from([("pi".to_owned(), pi_catalog())]);
        let flat = resolved(
            r#"
            [roles.worker]
            agent = "pi"
            model = "Command Code/deepseek/deepseek-v4.1-flash"
            effort = "low"
            "#,
        );
        let checked = Checker::with(table())
            .with_catalogs(catalogs.clone())
            .check(&flat, Review::default());
        assert!(
            checked.is_empty(),
            "the level is one pi accepts, so nothing is refused: {:?}",
            checked.messages()
        );
        assert_eq!(checked.warnings().len(), 1, "{:?}", checked.warnings());
        assert!(
            checked.warnings()[0].contains("role `worker`")
                && checked.warnings()[0].contains("not thinking"),
            "{:?}",
            checked.warnings()
        );

        let thinking = resolved(
            r#"
            [roles.worker]
            agent = "pi"
            model = "anthropic/claude-opus-4-8"
            effort = "high"
            "#,
        );
        let checked = Checker::with(table())
            .with_catalogs(catalogs)
            .check(&thinking, Review::default());
        assert!(
            checked.warnings().is_empty(),
            "a model that thinks keeps its level: {:?}",
            checked.warnings()
        );
    }

    /// The refusal a check produced, or a panic when it passed.
    fn refused(resolved: &Resolved) -> String {
        match check(resolved, Review::default(), &table()) {
            Err(problems) => problems.to_string(),
            Ok(()) => panic!("the settings should have been refused: {resolved:#?}"),
        }
    }

    #[test]
    fn the_built_in_roles_check_out_against_the_shipped_table() {
        assert!(
            check(&resolved(""), Review::default(), &table()).is_ok(),
            "a role nobody picked runs the program's own defaults, so it checks"
        );
    }

    #[test]
    fn a_picked_role_that_the_table_can_run_checks_out() {
        let settings = resolved(
            r#"
            [roles.worker]
            agent = "claude"
            model = "claude-opus-5-5"
            effort = "high"
            perm = "auto"
            fallback = ["codex", "pi"]

            [roles.reviewer]
            agent = "codex"
            model = "gpt-5-codex"
            "#,
        );
        assert!(check(&settings, Review::default(), &table()).is_ok());
    }

    #[test]
    fn a_program_no_harness_defines_is_refused_by_name() {
        let settings = resolved("[roles.worker]\nagent = \"gemini\"\n");
        let message = refused(&settings);
        assert!(message.contains("role `worker`"), "{message}");
        assert!(message.contains("`gemini`"), "{message}");
        assert!(message.contains("known programs are"), "{message}");
    }

    #[test]
    fn a_fallback_program_must_exist_too() {
        let settings = resolved("[roles.worker]\nagent = \"claude\"\nfallback = [\"nope\"]\n");
        let message = refused(&settings);
        assert!(message.contains("falls back to `nope`"), "{message}");
    }

    #[test]
    fn an_access_mode_the_program_does_not_offer_is_refused() {
        // `auto` is Claude's word for it; codex calls its own `auto` too, but
        // claude has no `read-only`.
        let settings = resolved("[roles.worker]\nagent = \"claude\"\nperm = \"read-only\"\n");
        let message = refused(&settings);
        assert!(message.contains("access mode `read-only`"), "{message}");
        assert!(message.contains("`claude` does not offer"), "{message}");
        assert!(message.contains("manual"), "{message}");
    }

    #[test]
    fn a_role_that_reviews_may_not_run_in_full_access() {
        let settings = resolved(
            r#"
            [roles.reviewer]
            agent = "claude"
            perm = "bypassPermissions"
            "#,
        );
        let message = refused(&settings);
        assert!(
            message.contains("may not run in `bypassPermissions`"),
            "{message}"
        );
        assert!(message.contains("`plan`"), "{message}");
    }

    #[test]
    fn an_effort_the_program_does_not_list_is_refused() {
        let settings = resolved("[roles.worker]\nagent = \"claude\"\neffort = \"turbo\"\n");
        let message = refused(&settings);
        assert!(message.contains("effort `turbo`"), "{message}");
        assert!(message.contains("low, medium, high"), "{message}");
    }

    #[test]
    fn a_pi_model_without_its_provider_is_refused() {
        let settings =
            resolved("[roles.worker]\nagent = \"pi\"\nmodel = \"deepseek-v4.1-flash\"\n");
        let message = refused(&settings);
        assert!(message.contains("<provider>"), "{message}");
    }

    #[test]
    fn a_same_vendor_review_is_refused_naming_both_roles() {
        let settings = resolved(
            r#"
            [roles.worker]
            agent = "claude"
            model = "claude-opus-5-5"

            [roles.reviewer]
            agent = "claude"
            model = "claude-sonnet-5-5"
            "#,
        );
        let message = refused(&settings);
        assert!(
            message.contains("the reviewer `reviewer` and the worker `worker` are both anthropic"),
            "{message}"
        );
        assert!(
            message.contains("roles.reviewer.agent"),
            "the refusal should say what to change: {message}"
        );
    }

    #[test]
    fn two_programs_are_not_enough_when_they_run_one_vendors_models() {
        // pi and opencode are multi-model CLIs: the same vendor through two
        // programs is still one vendor.
        let settings = resolved(
            r#"
            [roles.worker]
            agent = "pi"
            model = "Command Code/deepseek/deepseek-v4.1-flash"

            [roles.reviewer]
            agent = "opencode"
            model = "deepseek-v4.1-flash-free"
            "#,
        );
        let message = refused(&settings);
        assert!(message.contains("are both deepseek"), "{message}");
    }

    #[test]
    fn one_program_with_two_vendors_is_cross_vendor() {
        // The same CLI running another family's model is a different vendor.
        let settings = resolved(
            r#"
            [roles.worker]
            agent = "pi"
            model = "Command Code/deepseek/deepseek-v4.1-flash"

            [roles.reviewer]
            agent = "pi"
            model = "Claude Code/claude-sonnet-4"
            "#,
        );
        assert!(
            check(&settings, Review::default(), &table()).is_ok(),
            "deepseek work under an anthropic review is cross-vendor"
        );
    }

    #[test]
    fn only_a_review_role_may_review() {
        let settings = resolved(
            r#"
            [roles.reviewer]
            kind = "worker"
            readonly = false
            agent = "codex"

            [roles.audit]
            kind = "reviewer"
            agent = "codex"
            "#,
        );
        let message = refused(&settings);
        assert!(
            message.contains("role `reviewer` may not review"),
            "{message}"
        );
        assert!(
            message.contains("may review (audit)"),
            "the refusal names the roles that can review: {message}"
        );
    }

    #[test]
    fn a_readonly_role_may_review() {
        let settings = resolved(
            r#"
            [roles.reviewer]
            kind = "worker"
            readonly = true
            agent = "codex"
            "#,
        );
        assert!(check(&settings, Review::default(), &table()).is_ok());
    }

    #[test]
    fn cross_vendor_can_be_turned_off_and_is_on_by_default() {
        let same = r#"
            [roles.worker]
            agent = "claude"

            [roles.reviewer]
            agent = "claude"
            "#;
        let message = refused(&resolved(same));
        assert!(message.contains("not cross-vendor"), "{message}");

        let off = format!("{same}\n[review]\ncross_vendor = false\n");
        assert!(
            check(&resolved(&off), Review::default(), &table()).is_ok(),
            "a person may decide one vendor is enough"
        );
    }

    #[test]
    fn an_unknown_review_role_is_refused_with_the_ones_that_exist() {
        let settings = resolved("[roles.reviewer]\nagent = \"codex\"\n");
        match check(
            &settings,
            Review {
                worker: "worker",
                reviewer: "auditor",
            },
            &table(),
        ) {
            Err(problems) => {
                let message = problems.to_string();
                assert!(message.contains("no role `auditor`"), "{message}");
                assert!(message.contains("reviewer"), "{message}");
            }
            Ok(()) => panic!("an unknown review role must be refused"),
        }
    }

    #[test]
    fn a_review_compares_the_roles_the_caller_names() {
        let settings = resolved(
            r#"
            [roles.docs]
            kind = "worker"
            agent = "pi"
            model = "Claude Code/claude-sonnet-4"

            [roles.audit]
            kind = "reviewer"
            agent = "claude"
            "#,
        );
        let review = Review {
            worker: "docs",
            reviewer: "audit",
        };
        let message = match check(&settings, review, &table()) {
            Err(problems) => problems.to_string(),
            Ok(()) => panic!("both roles run anthropic models"),
        };
        assert!(
            message.contains("the reviewer `audit` and the worker `docs`"),
            "{message}"
        );
    }

    #[test]
    fn a_machine_without_a_table_says_so_instead_of_passing() {
        let settings = resolved(
            r#"
            [roles.worker]
            agent = "claude"

            [roles.reviewer]
            agent = "claude"
            "#,
        );
        let checker = Checker::load(None);
        assert!(!checker.is_available());
        let reason = checker.reason().unwrap_or_default();
        assert!(reason.contains("SHPRD_DATA_DIR"), "{reason}");
        let checked = checker.check(&settings, Review::default());
        assert!(
            checked.is_empty(),
            "nothing was found, because nothing was asked"
        );
        assert_eq!(checked.skipped(), Some(reason));
    }

    #[test]
    fn only_the_refusals_a_change_introduces_are_new() {
        let broken = resolved(
            r#"
            [roles.worker]
            agent = "claude"

            [roles.reviewer]
            agent = "claude"
            "#,
        );
        let clean = resolved("[roles.worker]\nagent = \"claude\"\n");
        let checker = Checker::with(table());
        let before = checker.check(&broken, Review::default());
        let after = checker.check(&broken, Review::default());
        assert!(
            after.introduced(&before).is_empty(),
            "a refusal already in the file is not a new one"
        );
        let fixed = checker.check(&clean, Review::default());
        assert!(
            fixed.introduced(&before).is_empty(),
            "clearing a refusal introduces nothing"
        );
        assert_eq!(
            before.introduced(&fixed).len(),
            1,
            "the pair is the one refusal"
        );
    }

    #[test]
    fn the_table_loads_from_a_data_directory() {
        let table = table();
        assert_eq!(table.specs.len(), 5, "the five shipped harnesses");
        assert_eq!(table.specs[0].id, "claude", "in id order");
        assert!(table.families.family_of("claude-opus-5-5").is_some());
    }
}
