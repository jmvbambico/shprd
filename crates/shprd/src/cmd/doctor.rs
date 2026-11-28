//! `shprd doctor`: what works on this machine, and what to fix.
//!
//! A read-only page of sections: the host shprd runs in, the harness data it
//! loads, the config, the installed agents, their quota, and the skills they
//! see. A probe that fails becomes a line rather than an error, so one run
//! shows every problem. The verb exits 1 when any line is a [`Level::Fail`].

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::anyhow;
use shprd_agents::data::{DataDir, Kind};
use shprd_agents::detect::{
    Detection, PREFLIGHT_TIMEOUT, SignIn, detect_all, find_binary, model_reads_images,
    preflight_probe, vendor_name,
};
use shprd_agents::env::{self, EnvContext, LaunchEnv, Role};
use shprd_agents::families::{Families, agent_vendor};
use shprd_agents::models::AgentRef;
use shprd_agents::quota::{self, ProbeContext, QuotaReport, QuotaState};
use shprd_agents::skills::{self, CompatList, ReviewState, Verdict, VisibilityRules};
use shprd_agents::spec::HarnessSpec;
use shprd_core::config::{self, Config, Roster};
use shprd_core::settings::{Loaded, Resolved, ResolvedRole, RoleKind, Source};

use crate::cli::DoctorArgs;
use crate::ctx::{CliError, resolve_home};

/// How much attention one line wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// Working as intended.
    Ok,
    /// Suspicious, worth a look.
    Warn,
    /// Broken: the verb exits 1.
    Fail,
    /// Neither good nor bad, just worth showing.
    Info,
}

impl Level {
    /// The glyph the text report puts before a line.
    fn glyph(self) -> char {
        match self {
            Self::Ok => '✓',
            Self::Warn => '!',
            Self::Fail => '✗',
            Self::Info => '·',
        }
    }

    /// The lower-case name the JSON report uses.
    fn name(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Fail => "fail",
            Self::Info => "info",
        }
    }
}

/// One line of the report, without its glyph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// How much attention the line wants.
    pub level: Level,
    /// What the line says.
    pub text: String,
}

/// One titled block of lines.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Section {
    /// The heading, e.g. `Host`.
    pub title: String,
    /// The lines under it, in the order they were found.
    pub lines: Vec<Line>,
}

/// The whole page, in display order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Report {
    /// The sections.
    pub sections: Vec<Section>,
}

impl Report {
    /// Starts a titled section and returns it for filling.
    fn section(&mut self, title: &str) -> &mut Section {
        self.sections.push(Section {
            title: title.to_owned(),
            lines: Vec::new(),
        });
        let index = self.sections.len() - 1;
        &mut self.sections[index]
    }

    /// Whether any line failed, which decides the exit code.
    pub fn has_fail(&self) -> bool {
        self.sections
            .iter()
            .flat_map(|section| section.lines.iter())
            .any(|line| line.level == Level::Fail)
    }
}

impl Section {
    /// Appends one line.
    fn add(&mut self, level: Level, text: impl Into<String>) {
        self.lines.push(Line {
            level,
            text: text.into(),
        });
    }
}

/// Runs `shprd doctor`, printing the page and failing when a check failed.
pub fn run(args: &DoctorArgs, json: bool, home: Option<&Path>) -> Result<(), CliError> {
    let report = build(home, args.no_quota);
    if json || args.json {
        print_json(&report)?;
    } else {
        print!("{}", render(&report));
    }
    if report.has_fail() {
        return Err(CliError::other(anyhow!(
            "a check failed: fix the lines marked with ✗"
        )));
    }
    Ok(())
}

/// Builds the whole page, one section at a time.
fn build(home_flag: Option<&Path>, no_quota: bool) -> Report {
    let facts = Facts::resolve(home_flag);
    let mut report = Report::default();
    host(&mut report, &facts);
    let specs = data(&mut report, &facts);
    let roles = config_section(&mut report, &facts);
    graph_section(&mut report, &facts);
    forge_section(&mut report, &facts, &RealGh::new(facts.repo.as_deref()));
    let clean = worker_env(&facts);
    let detections = detect_every(&facts, &specs, &roles);
    agents(&mut report, &detections);
    preflight_section(&mut report, &facts, &specs, &detections, &clean);
    review(&mut report, &facts, &specs, &roles);
    if !no_quota {
        quota_section(&mut report, &facts, &specs, &roles);
    }
    skills_section(&mut report, &facts, &specs, &clean, &detections);
    launch_env_section(&mut report, &facts, &clean);
    news_section(&mut report, &facts);
    report
}

/// The news section: how many stop alarms the task later contradicted, a
/// measure of how trustworthy "Agent stopped" has been.
fn news_section(report: &mut Report, facts: &Facts) {
    let section = report.section("News");
    let Some(repo) = facts.repo.as_deref() else {
        section.add(Level::Warn, "not inside a repository, so no news memory");
        return;
    };
    let count = crate::cmd::live_news::stops_contradicted(repo);
    let (level, text) = contradicted_stops_line(count);
    section.add(level, text);
}

/// The doctor line for the contradicted-stop count: a warning when a stop alarm
/// has been wrong, since that is a false "Agent stopped" the person saw.
fn contradicted_stops_line(count: u32) -> (Level, String) {
    if count == 0 {
        (Level::Ok, String::from("0 stop alarms contradicted"))
    } else {
        (
            Level::Warn,
            format!("{count} stop alarm(s) contradicted by the task itself"),
        )
    }
}

/// What `run start` warns about: the programs that fail to start, and whether
/// the reviewer's model reads images. Only warnings, never a refusal.
pub fn start_warnings(home: Option<&Path>) -> Vec<String> {
    let facts = Facts::resolve(home);
    let mut scratch = Report::default();
    let specs = data(&mut scratch, &facts);
    let roles = config_section(&mut scratch, &facts);
    let clean = worker_env(&facts);
    let detections = detect_every(&facts, &specs, &roles);
    let mut report = Report::default();
    preflight_section(&mut report, &facts, &specs, &detections, &clean);
    if let Some(reviewer) = roles.reviewer.as_deref() {
        image_line(report.section("Review"), &specs_model(&specs, reviewer));
    } else {
        report
            .section("Review")
            .add(Level::Warn, "reviewer not set, so no cross-vendor review");
    }
    warnings_of(&report)
}

/// The text of every warning line on the page.
fn warnings_of(report: &Report) -> Vec<String> {
    report
        .sections
        .iter()
        .flat_map(|section| section.lines.iter())
        .filter(|line| line.level == Level::Warn)
        .map(|line| line.text.clone())
        .collect()
}

/// The machine facts every section reads, resolved once.
struct Facts {
    /// The environment the verb started in, values included.
    current: BTreeMap<String, String>,
    /// `PATH`, in the form the detection helpers take it.
    path_env: String,
    /// The user's home directory, when one resolved.
    home: Option<PathBuf>,
    /// The repository root, when the verb runs inside one.
    repo: Option<PathBuf>,
    /// The data the binary reads: `$SHPRD_DATA_DIR`, the person's overrides,
    /// or the copy embedded in the binary.
    data: DataDir,
}

impl Facts {
    /// Collects the environment, home, repo and data directory.
    fn resolve(home_flag: Option<&Path>) -> Self {
        Self {
            current: std::env::vars().collect(),
            path_env: std::env::var("PATH").unwrap_or_default(),
            home: resolve_home(home_flag).ok(),
            repo: crate::ctx::resolve_root(None).ok(),
            data: crate::cmd::roster::resolve_data_dir(),
        }
    }
}

/// The page as text: a heading per section, then one glyphed line each.
fn render(report: &Report) -> String {
    let mut out = String::new();
    for section in &report.sections {
        out.push_str(&section.title);
        out.push('\n');
        for line in &section.lines {
            out.push_str(&format!("  {} {}\n", line.level.glyph(), line.text));
        }
    }
    out
}

/// The page as JSON: one object per section, one object per line.
fn print_json(report: &Report) -> Result<(), CliError> {
    let sections: Vec<serde_json::Value> = report
        .sections
        .iter()
        .map(|section| {
            let lines: Vec<serde_json::Value> = section
                .lines
                .iter()
                .map(|line| {
                    serde_json::json!({
                        "level": line.level.name(),
                        "text": line.text,
                    })
                })
                .collect();
            serde_json::json!({"title": section.title, "lines": lines})
        })
        .collect();
    let text = serde_json::to_string_pretty(&serde_json::json!({"sections": sections}))
        .map_err(|err| CliError::other(anyhow!(err).context("serializing the report")))?;
    println!("{text}");
    Ok(())
}

/// Whether the environment marks this as inside a multiplexer.
///
/// Tuios is checked first: it can also set `HERDR_ENV=1` in some panes, so a
/// herdr-first test would name the wrong multiplexer.
fn marked(current: &BTreeMap<String, String>, var: &str) -> bool {
    current.get(var).map(String::as_str) == Some("1")
}

/// Section 1: the multiplexer we are inside, the host a run would drive, and the
/// binaries behind it.
fn host(report: &mut Report, facts: &Facts) {
    let section = report.section("Host");
    if marked(&facts.current, "TUIOS_ENV") {
        section.add(Level::Ok, "inside tuios");
    } else if marked(&facts.current, "HERDR_ENV") {
        section.add(Level::Ok, "inside herdr");
    } else {
        section.add(Level::Warn, "not inside herdr or tuios");
    }
    // The host the verbs would drive, and why: the same resolution the run does
    // (M10.9), so a person asking "which multiplexer is shprd using?" gets the
    // answer the next spawn will act on rather than a second opinion.
    // Through the sticky select, like every other caller, so doctor reports the
    // host a spawn would drive rather than a second probe.
    let (resolution, _host) = crate::host::select();
    let level = if resolution.fell_back {
        Level::Warn
    } else {
        Level::Ok
    };
    section.add(level, resolution.line());
    for binary in ["herdr", "tuios", "hc", "herdr-app"] {
        match find_binary(binary, &facts.path_env) {
            Some(path) => section.add(Level::Ok, format!("{binary} {}", path.display())),
            None => section.add(Level::Warn, format!("{binary} not on PATH")),
        }
    }
}

/// Section 2: where the shipped data came from, what it holds and whether it
/// loads.
///
/// Every kind is named with its own source, because a person's overrides are
/// merged file by file: their `harness/` may hold one file while the rest still
/// comes from the binary. Returns the specs it loaded, so the agent section
/// does not load them again.
fn data(report: &mut Report, facts: &Facts) -> Vec<HarnessSpec> {
    let section = report.section("Data");
    section.add(Level::Info, format!("data from {}", facts.data.label()));
    for kind in Kind::ALL {
        let files = facts.data.summary(kind);
        let level = if files.total == 0 {
            Level::Warn
        } else {
            Level::Ok
        };
        section.add(level, files.describe(kind));
    }
    let specs = match facts.data.harness_specs() {
        Ok(specs) => {
            section.add(
                Level::Ok,
                format!("{} harness spec(s) load and validate", specs.len()),
            );
            specs
        }
        Err(err) => {
            section.add(Level::Fail, format!("harness specs do not load: {err}"));
            Vec::new()
        }
    };
    match facts.data.compat() {
        Ok(list) => section.add(
            Level::Ok,
            format!("compat.toml: {} entries", list.skills.len()),
        ),
        Err(err) => section.add(Level::Fail, format!("compat.toml does not load: {err}")),
    }
    match facts.data.visibility() {
        Ok(rules) => section.add(
            Level::Ok,
            format!("visibility.toml: {} rule(s)", rules.rules.len()),
        ),
        Err(err) => section.add(Level::Fail, format!("visibility.toml does not load: {err}")),
    }
    specs
}

/// The roles this machine would actually run, and where each came from.
///
/// The settings are what the engine dispatches from: `shprd spawn` reads
/// `roles.worker.*` and `shprd agent review` reads `roles.reviewer.*`, so the
/// page a person runs to check their setup must read the same resolution. The
/// legacy `[roster]` layer is the fallback, and only that: a person who has
/// migrated sees what the engine will do, and a person who has not still sees
/// their old choice working.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Roles {
    /// The coordinator: the role of kind `coordinator`, or the roster's.
    orchestrator: Option<String>,
    /// The reviewer: the role of kind `reviewer` that may review, or the
    /// roster's.
    reviewer: Option<String>,
    /// The programs a task may be dispatched to, in the order a spawn tries
    /// them: the worker role's agent, then the roster's workers.
    workers: Vec<String>,
    /// The layer each role's answer came from, for the line that prints it.
    sources: BTreeMap<&'static str, Source>,
    /// Whether the old `[roster] orchestrator_auto_approve` switch is on.
    auto_approve: bool,
}

impl Roles {
    /// The roles a legacy `[roster]` alone names.
    ///
    /// It is the layer this machine had before M9, and it stays the fallback:
    /// [`Roles::resolve`] starts here and lets the settings replace what they
    /// name, so a person who has not migrated sees their old choice and a
    /// person who has sees the engine's.
    fn from_roster(roster: Option<&Roster>) -> Roles {
        let Some(roster) = roster else {
            return Roles::default();
        };
        let text = |agent: &String| agent.trim().to_owned();
        let mut roles = Roles {
            auto_approve: roster.orchestrator_auto_approve,
            ..Roles::default()
        };
        if let Some(agent) = roster
            .orchestrator
            .as_deref()
            .map(str::trim)
            .filter(|a| !a.is_empty())
        {
            roles.orchestrator = Some(agent.to_owned());
            roles.sources.insert("orchestrator", Source::Legacy);
        }
        if let Some(agent) = roster
            .reviewer
            .as_deref()
            .map(str::trim)
            .filter(|a| !a.is_empty())
        {
            roles.reviewer = Some(agent.to_owned());
            roles.sources.insert("reviewer", Source::Legacy);
        }
        roles.workers = roster
            .workers
            .iter()
            .map(text)
            .filter(|agent| !agent.is_empty())
            .collect();
        roles
    }

    /// Resolve the roles for this `home` and repository.
    ///
    /// The settings win where they name a program, because they are what the
    /// engine dispatches from; the legacy roster is what is left when they do
    /// not.
    fn resolve(facts: &Facts, config: Option<&Config>) -> Roles {
        let mut roles = Roles::from_roster(config.map(|config| &config.roster));
        let resolved = facts
            .home
            .as_ref()
            .and_then(|home| Loaded::read(home, facts.repo.as_deref()).ok())
            .map(|loaded| loaded.resolve(None));
        let Some(resolved) = resolved else {
            return roles;
        };
        // The coordinator is a role of kind `coordinator`; the built-in one is
        // `coordinator`, so a settings-only home finds it there.
        if let Some((agent, source)) = kind_role(&resolved, RoleKind::Coordinator)
            .as_ref()
            .and_then(role_agent)
        {
            roles.orchestrator = Some(agent);
            roles.sources.insert("orchestrator", source);
        }
        // The reviewer is the role a review actually uses: one that may review,
        // which is a `reviewer` or a role that only reads.
        if let Some((agent, source)) = resolved
            .roles()
            .into_iter()
            .find(|role| role.may_review())
            .as_ref()
            .and_then(role_agent)
        {
            roles.reviewer = Some(agent);
            roles.sources.insert("reviewer", source);
        }
        // Every program the run could dispatch to: the worker role first, then
        // the machine's list, exactly as `role_candidates` orders them.
        if let Some((agent, source)) = resolved.role("worker").as_ref().and_then(role_agent) {
            roles.workers.retain(|known| *known != agent);
            roles.workers.insert(0, agent);
            roles.sources.insert("worker", source);
        }
        roles
    }

    /// The layer one role's answer came from, when one has an answer.
    fn source(&self, name: &str) -> Option<Source> {
        self.sources.get(name).copied()
    }

    /// Every agent string the roles name, coordinator first.
    ///
    /// It is what the piped-in checks read: a sign-in probe that needs a model,
    /// a quota line that asks whether anything dispatches this program, and the
    /// free-pin test all ask the same "what does this machine run" question.
    fn every_agent(&self) -> impl Iterator<Item = &String> {
        self.orchestrator
            .iter()
            .chain(self.reviewer.iter())
            .chain(self.workers.iter())
    }
}

/// The first role of `kind`, in the order a settings page lists them.
fn kind_role(resolved: &Resolved, kind: RoleKind) -> Option<ResolvedRole> {
    resolved
        .roles()
        .into_iter()
        .find(|role| role.kind.value == kind)
}

/// A role's program and model as one agent string, and the layer that named it.
///
/// It is the same `program/model` the legacy roster writes, so a line and a
/// vendor check read the two layers alike.
fn role_agent(role: &ResolvedRole) -> Option<(String, Source)> {
    let agent = role.agent.as_ref()?;
    let program = agent.value.trim();
    if program.is_empty() {
        return None;
    }
    let text = match role.model.as_ref().map(|model| model.value.trim()) {
        Some(model) if !model.is_empty() => format!("{program}/{model}"),
        _ => program.to_owned(),
    };
    // The nearer layer wins: a program from `you` and a model from `built in`
    // is the person's own choice, so the program's layer is the one to show.
    Some((text, agent.source))
}

/// Section 3: the layered config, then the roles it resolves to.
///
/// The role lines read the settings ([`Roles::resolve`]), which is what the
/// engine dispatches from, so a person who set a role the M9 way sees that
/// choice here rather than a legacy roster that says nothing. The returned
/// `Config` is still the file's own answer: `[gates]`, `[skills]` and the
/// auto-approve switch have no settings key and are read straight from it.
fn config_section(report: &mut Report, facts: &Facts) -> Roles {
    let section = report.section("Config");
    let global = facts
        .home
        .as_ref()
        .map(|home| home.join(".config").join("shprd").join("config.toml"));
    let repo_layer = facts
        .repo
        .as_ref()
        .map(|root| root.join(".shprd").join("config.toml"));
    let loaded = match config::load(global.as_deref(), repo_layer.as_deref()) {
        Ok(loaded) => loaded,
        Err(err) => {
            section.add(Level::Fail, format!("does not load: {err}"));
            // The settings are a separate file: a broken `config.toml` must
            // not hide a reviewer the person set the M9 way.
            return Roles::resolve(facts, None);
        }
    };
    // Only the files that are there: naming one shprd never writes sends a
    // person looking for a file that does not exist, and an M9 install keeps
    // everything in `settings.toml`.
    let present = config_files(facts);
    match present.as_slice() {
        [] => section.add(
            Level::Ok,
            "loads and validates (no config.toml: the settings file is the only layer)",
        ),
        [one] => section.add(Level::Ok, format!("loads and validates ({one})")),
        [one, two, ..] => section.add(Level::Ok, format!("loads and validates ({one} and {two})")),
    }
    let roles = Roles::resolve(facts, Some(&loaded));
    roles_section(section, &roles);
    roles
}

/// The `config.toml` files this machine actually has, as `path`s.
///
/// The person's file first, then the repository's, which is the order the layers
/// are read in. A file that is not there is left out: a page that named it would
/// send a person looking for something shprd never wrote.
fn config_files(facts: &Facts) -> Vec<String> {
    let global = facts
        .home
        .as_ref()
        .map(|home| home.join(".config").join("shprd").join("config.toml"));
    let repo = facts
        .repo
        .as_ref()
        .map(|root| root.join(".shprd").join("config.toml"));
    [global, repo]
        .into_iter()
        .flatten()
        .filter(|path| path.exists())
        .map(|path| path.display().to_string())
        .collect()
}

/// Prints the three role lines the engine would act on, and where each came
/// from.
fn roles_section(section: &mut Section, roles: &Roles) {
    role(
        section,
        "orchestrator",
        roles.orchestrator.as_deref(),
        roles,
    );
    if roles.auto_approve {
        section.add(
            Level::Warn,
            "orchestrator runs with auto-approve: the PR gate relies on its brief",
        );
    }
    role(section, "reviewer", roles.reviewer.as_deref(), roles);
    if roles.workers.is_empty() {
        section.add(Level::Warn, "workers not set");
    } else {
        let list = roles.workers.join(", ");
        match roles.source("worker") {
            Some(source) => section.add(Level::Ok, format!("workers {list} ({})", source.label())),
            None => section.add(Level::Ok, format!("workers {list}")),
        }
    }
}

/// Adds one role line: the agent string and its layer, or a warning that it is
/// unset.
fn role(section: &mut Section, name: &str, agent: Option<&str>, roles: &Roles) {
    match agent {
        Some(agent) if !agent.trim().is_empty() => match roles.source(name) {
            Some(source) => section.add(Level::Ok, format!("{name} {agent} ({})", source.label())),
            None => section.add(Level::Ok, format!("{name} {agent}")),
        },
        _ => section.add(Level::Warn, format!("{name} not set")),
    }
}

/// The clean environment a worker CLI is launched with.
fn worker_env(facts: &Facts) -> LaunchEnv {
    let ctx = EnvContext {
        actor: "worker".to_owned(),
        task: None,
        run: None,
        repo: std::env::current_dir().unwrap_or_default(),
        home: facts.home.clone(),
        bin: shprd_agents::env::service_bin(),
        data: facts.data.env_value().map(Path::to_path_buf),
    };
    env::launch_env(&facts.current, Role::Worker, &ctx)
}

/// The model the pi sign-in probe needs: from the roles, else the environment.
fn pi_models(facts: &Facts, specs: &[HarnessSpec], roles: &Roles) -> BTreeMap<String, String> {
    if !specs.iter().any(|spec| spec.id == "pi") {
        return BTreeMap::new();
    }
    let from_roles = roles
        .every_agent()
        .find_map(|agent| model_after_pi(specs, agent));
    let model = from_roles.or_else(|| {
        facts
            .current
            .get("SHPRD_PI_MODEL")
            .map(|model| model.trim().to_owned())
            .filter(|model| !model.is_empty())
    });
    let mut models = BTreeMap::new();
    if let Some(model) = model {
        models.insert("pi".to_owned(), model);
    }
    models
}

/// The model of an agent string that names the pi harness, when it has one.
fn model_after_pi(specs: &[HarnessSpec], agent: &str) -> Option<String> {
    let parsed = AgentRef::parse(agent, specs).ok()?;
    if parsed.program != "pi" {
        return None;
    }
    parsed.model.filter(|model| !model.trim().is_empty())
}

/// Probes every harness CLI with the environment a worker is launched with.
fn detect_every(facts: &Facts, specs: &[HarnessSpec], roles: &Roles) -> Vec<Detection> {
    if specs.is_empty() {
        return Vec::new();
    }
    let pairs = worker_env(facts).as_pairs();
    let models = pi_models(facts, specs, roles);
    detect_all(specs, &facts.path_env, &pairs, &models)
}

/// Section 3b: the graph's program and the document it draws.
///
/// Two questions a person cannot answer from anywhere else: which dagr shprd
/// will run (the setting, `PATH` or the herdr plugin, and it matters because a
/// projection is only checked when one is found), and whether the run's
/// `run.json` is there and younger than the log it was projected from. A missing
/// program is a warning and not a failure — a run without a graph still runs —
/// but it is said in the words that name the key to set.
fn graph_section(report: &mut Report, facts: &Facts) {
    let section = report.section("Graph");
    match crate::cmd::dagr::locate(facts.home.as_deref()) {
        Ok(found) => section.add(
            Level::Ok,
            format!(
                "dagr {} (from {})",
                found.path.display(),
                found.origin.label()
            ),
        ),
        Err(missing) => section.add(Level::Warn, missing.message()),
    }
    let Some(root) = facts.repo.as_deref() else {
        section.add(
            Level::Info,
            "no repository here, so there is no run to draw",
        );
        return;
    };
    let run_json = crate::cmd::dagr::run_json_path(root);
    let log = root.join(".shprd").join("events.jsonl");
    let modified = |path: &Path| fs::metadata(path).and_then(|meta| meta.modified()).ok();
    match (modified(&run_json), modified(&log)) {
        (Some(document), Some(log)) if document >= log => section.add(
            Level::Ok,
            format!("{} is as new as the log", run_json.display()),
        ),
        (Some(_), _) => section.add(
            Level::Warn,
            format!(
                "{} is older than the log: every ledger write rewrites it, so a service that is not running leaves it behind",
                run_json.display()
            ),
        ),
        (None, Some(_)) => section.add(
            Level::Warn,
            format!(
                "{} is missing while the run has a log: `shprd dagr sync` writes it",
                run_json.display()
            ),
        ),
        (None, None) => section.add(
            Level::Info,
            "no run yet, so there is no graph to draw",
        ),
    }
}

/// How the doctor asks `gh`: a seam so the section's decisions are tested with
/// a script instead of a login and a network.
trait Gh {
    /// Runs `gh <args>`; `token`, when given, goes in as `GH_TOKEN`. `Err` is
    /// the reason it could not even start.
    fn run(&self, args: &[&str], token: Option<&str>) -> Result<GhOut, String>;
}

/// What one `gh` call answered.
struct GhOut {
    /// True when it exited zero.
    ok: bool,
    /// Its stdout, trimmed.
    stdout: String,
    /// Its stderr, trimmed.
    stderr: String,
}

/// The real `gh`, run in the repository the doctor was started in.
struct RealGh {
    /// Where `gh` runs, so it names this repository's remote.
    dir: Option<PathBuf>,
}

impl RealGh {
    /// A `gh` that runs in `dir`.
    fn new(dir: Option<&Path>) -> Self {
        Self {
            dir: dir.map(Path::to_path_buf),
        }
    }
}

impl Gh for RealGh {
    fn run(&self, args: &[&str], token: Option<&str>) -> Result<GhOut, String> {
        let mut command = std::process::Command::new("gh");
        command
            .args(args)
            .env("GH_PROMPT_DISABLED", "1")
            .env("LC_ALL", "C")
            .stdin(std::process::Stdio::null());
        if let Some(dir) = &self.dir {
            command.current_dir(dir);
        }
        if let Some(token) = token {
            command.env("GH_TOKEN", token);
        }
        let output = command.output().map_err(|err| err.to_string())?;
        Ok(GhOut {
            ok: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).trim().to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    }
}

/// The `gh` accounts `gh auth status` lists, the active one flagged.
fn gh_accounts(status: &str) -> Vec<(String, bool)> {
    let mut accounts: Vec<(String, bool)> = Vec::new();
    for line in status.lines().map(str::trim) {
        let line = line.trim_start_matches(['\u{2713}', '-', ' ']);
        if let Some(rest) = line
            .split(" account ")
            .nth(1)
            .filter(|_| line.starts_with("Logged in to"))
        {
            let name = rest.split_whitespace().next().unwrap_or_default();
            accounts.push((name.to_string(), false));
        } else if let Some(active) = line.strip_prefix("Active account:")
            && let Some(last) = accounts.last_mut()
        {
            last.1 = active.trim() == "true";
        }
    }
    accounts
}

/// What one account can do to the repository.
enum Access {
    /// It can push and open pull requests.
    Write,
    /// It sees the repository but may not write (`READ`, `TRIAGE`).
    ReadOnly(String),
    /// It cannot see the repository; `gh`'s own words say why.
    Hidden(String),
    /// The repository has no GitHub remote.
    NoRemote,
    /// It could not be asked (no network, no token): not a verdict.
    Unknown(String),
}

/// The last line of a `gh` error, which is the one that says what happened.
fn gh_words(stderr: &str) -> String {
    stderr
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .unwrap_or("no reason given")
        .to_string()
}

/// Whether `account` (or the active one, for `None`) can write to the
/// repository `gh` runs in. Never prints a token.
fn gh_access(gh: &dyn Gh, account: Option<&str>) -> Access {
    let token = match account {
        None => None,
        Some(name) => match gh.run(&["auth", "token", "-u", name], None) {
            Ok(out) if out.ok && !out.stdout.is_empty() => Some(out.stdout),
            Ok(out) => {
                return Access::Unknown(format!(
                    "no token for the gh account {name}: {}",
                    gh_words(&out.stderr)
                ));
            }
            Err(err) => return Access::Unknown(err),
        },
    };
    let out = match gh.run(
        &["repo", "view", "--json", "nameWithOwner,viewerPermission"],
        token.as_deref(),
    ) {
        Ok(out) => out,
        Err(err) => return Access::Unknown(err),
    };
    if !out.ok {
        let said = out.stderr.to_ascii_lowercase();
        let words = gh_words(&out.stderr);
        return if said.contains("known github host") || said.contains("no git remotes") {
            Access::NoRemote
        } else if said.contains("could not resolve")
            || said.contains("not found")
            || said.contains("404")
        {
            Access::Hidden(words)
        } else {
            Access::Unknown(words)
        };
    }
    let value: serde_json::Value = serde_json::from_str(&out.stdout).unwrap_or_default();
    let permission = value
        .get("viewerPermission")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("UNKNOWN");
    match permission {
        "ADMIN" | "MAINTAIN" | "WRITE" => Access::Write,
        other => Access::ReadOnly(other.to_string()),
    }
}

/// The Forge section: which `gh` account landing will use, and whether it can
/// see the repository's remote and write to it. When it cannot, says which
/// logged-in account can, and the setting that pins it.
fn forge_section(report: &mut Report, facts: &Facts, gh: &dyn Gh) {
    let Some(repo) = facts.repo.as_deref() else {
        return;
    };
    let section = report.section("Forge");
    let status = match gh.run(&["auth", "status"], None) {
        Ok(out) => format!("{}\n{}", out.stdout, out.stderr),
        Err(_) => {
            section.add(
                Level::Info,
                "gh is not installed here, so work lands locally and nothing reaches GitHub",
            );
            return;
        }
    };
    let accounts = gh_accounts(&status);
    let active = accounts
        .iter()
        .find(|(_, active)| *active)
        .map(|(name, _)| name.clone());
    let pinned = facts
        .home
        .as_deref()
        .map(|home| crate::cmd::land::gh_user(home, repo))
        .filter(|name| !name.is_empty());
    if accounts.is_empty() {
        section.add(
            Level::Info,
            "gh is not signed in, so work lands locally; `gh auth login` signs it in",
        );
        return;
    }
    let names: Vec<String> = accounts
        .iter()
        .map(|(name, active)| {
            if *active {
                format!("{name} (active)")
            } else {
                name.clone()
            }
        })
        .collect();
    section.add(Level::Info, format!("gh accounts: {}", names.join(", ")));
    let used = pinned.clone().or(active.clone());
    let label = match (&pinned, &active) {
        (Some(name), _) => format!("{name} (pinned by tasks.gh_user)"),
        (None, Some(name)) => format!("{name} (the active account)"),
        (None, None) => String::from("the active account"),
    };
    let denied = match gh_access(gh, pinned.as_deref()) {
        Access::Write => {
            section.add(
                Level::Ok,
                format!("{label} can see this repository's remote and has write access"),
            );
            return;
        }
        Access::NoRemote => {
            section.add(
                Level::Info,
                "this repository has no GitHub remote, so work lands locally",
            );
            return;
        }
        Access::Unknown(why) => {
            section.add(
                Level::Warn,
                format!("could not check {label} against the remote: {why}"),
            );
            return;
        }
        Access::ReadOnly(permission) => {
            format!("{label} has only {permission} access to this repository and needs WRITE")
        }
        Access::Hidden(words) => {
            format!("{label} cannot see this repository's remote: {words}")
        }
    };
    let can: Vec<&str> = accounts
        .iter()
        .map(|(name, _)| name.as_str())
        .filter(|name| Some(*name) != used.as_deref())
        .filter(|name| matches!(gh_access(gh, Some(name)), Access::Write))
        .collect();
    match can.first() {
        Some(name) => section.add(
            Level::Fail,
            format!(
                "{denied}; logged-in account {name} can: `shprd settings set tasks.gh_user {name}` pins it"
            ),
        ),
        None => section.add(
            Level::Fail,
            format!("{denied}; no other logged-in account can write to it"),
        ),
    }
}

/// Section 4: every harness CLI, as a worker would find it.
fn agents(report: &mut Report, detections: &[Detection]) {
    let section = report.section("Agents");
    if detections.is_empty() {
        section.add(Level::Warn, "no harness spec, so no agent could be probed");
        return;
    }
    for detection in detections {
        let (level, text) = describe(detection);
        section.add(level, text);
    }
}

/// One detection as a level and a line.
fn describe(detection: &Detection) -> (Level, String) {
    let (level, binary) = match &detection.binary {
        Some(path) => (Level::Ok, path.display().to_string()),
        None => (Level::Warn, "not installed".to_owned()),
    };
    let (level, signin) = match &detection.signin {
        SignIn::SignedIn => (level, "signed in".to_owned()),
        SignIn::SignedOut => (Level::Fail, "signed out".to_owned()),
        SignIn::Unknown(reason) => (Level::Warn, format!("unknown ({reason})")),
    };
    let version = detection.version.as_deref().unwrap_or("-");
    let text = format!(
        "{}  {}  {}  {}  {}",
        detection.id,
        binary,
        version,
        signin,
        vendor_name(detection.vendor)
    );
    (level, text)
}

/// The model family `agent` runs, or why nothing proves one.
///
/// The family, not the CLI's vendor: a `pi` worker on DeepSeek and a `pi`
/// reviewer on Claude are different families, and a router that names no model
/// family proves nothing at all.
fn vendor_or_reason(
    specs: &[HarnessSpec],
    families: &Families,
    agent: &str,
) -> Result<String, String> {
    let named = AgentRef::parse(agent, specs).map_err(|error| error.to_string())?;
    agent_vendor(&named, specs, families).ok_or_else(|| match &named.model {
        Some(model) => format!("`{model}` names no known model family"),
        None => "it names no model".to_owned(),
    })
}

/// The model family table the data directory ships, or an empty one.
///
/// An empty table knows no family, so every line about a family is a warning
/// rather than a pass.
fn families_table(facts: &Facts) -> Families {
    facts.data.families().unwrap_or_default()
}

/// Section 5: whether the reviewer comes from another vendor than every worker.
///
/// An agent string that names no vendor is reported as a warning line rather
/// than skipped, and a review with any such agent never claims independence:
/// an unchecked worker could share the reviewer's vendor.
fn review(report: &mut Report, facts: &Facts, specs: &[HarnessSpec], roles: &Roles) {
    let section = report.section("Review");
    let families = families_table(facts);
    let Some(reviewer) = roles.reviewer.as_deref() else {
        section.add(Level::Warn, "reviewer not set, so no cross-vendor review");
        return;
    };
    let mut skipped = 0usize;
    let vendor = match vendor_or_reason(specs, &families, reviewer) {
        Ok(vendor) => Some(vendor),
        Err(reason) => {
            skipped += 1;
            section.add(
                Level::Warn,
                format!("cannot check vendor of {reviewer}: {reason}"),
            );
            None
        }
    };
    let mut clashes: Vec<&str> = Vec::new();
    for worker in &roles.workers {
        match vendor_or_reason(specs, &families, worker) {
            Ok(worker_vendor) => {
                if vendor == Some(worker_vendor) {
                    clashes.push(worker.as_str());
                }
            }
            Err(reason) => {
                skipped += 1;
                section.add(
                    Level::Warn,
                    format!("cannot check vendor of {worker}: {reason}"),
                );
            }
        }
    }
    match vendor {
        Some(vendor) if clashes.is_empty() && skipped == 0 => section.add(
            Level::Ok,
            format!("reviewer {reviewer} runs the {vendor} family, independent of every worker"),
        ),
        Some(vendor) if !clashes.is_empty() => section.add(
            Level::Fail,
            format!(
                "reviewer {reviewer} runs the {vendor} family: the same as worker(s) {}",
                clashes.join(", ")
            ),
        ),
        _ => {}
    }
    image_line(section, &specs_model(specs, reviewer));
}

/// Section 4b: does every program a run starts actually start?
///
/// `node --version` and each installed agent CLI's version command are run, so a
/// CLI that is on `PATH` but dies at launch (a missing dynamic library, say)
/// shows its first error line here, instead of as a silent "version unknown"
/// and a reviewer that never writes findings.
fn preflight_section(
    report: &mut Report,
    facts: &Facts,
    specs: &[HarnessSpec],
    detections: &[Detection],
    clean: &LaunchEnv,
) {
    let probes = preflight_probes(&facts.path_env, specs, detections, &clean.as_pairs());
    preflight_lines(report.section("Preflight"), &probes);
}

/// Runs `node --version` and each installed agent's version command, returning
/// one entry per program: `None` when it is not installed, else how it started.
fn preflight_probes(
    path_env: &str,
    specs: &[HarnessSpec],
    detections: &[Detection],
    pairs: &[(String, Option<String>)],
) -> Vec<(String, Option<Result<(), String>>)> {
    let mut probes: Vec<(String, Option<Result<(), String>>)> = Vec::new();
    let node = find_binary("node", path_env)
        .map(|path| preflight_probe(&path, &["--version".to_owned()], pairs, PREFLIGHT_TIMEOUT));
    probes.push(("node".to_owned(), node));
    for detection in detections {
        let Some(binary) = &detection.binary else {
            continue;
        };
        let Some(spec) = specs.iter().find(|spec| spec.id == detection.id) else {
            continue;
        };
        let result = preflight_probe(binary, &spec.detect.version_args, pairs, PREFLIGHT_TIMEOUT);
        probes.push((detection.id.clone(), Some(result)));
    }
    probes
}

/// One line per probed program: started cleanly, not installed, or why not.
fn preflight_lines(section: &mut Section, probes: &[(String, Option<Result<(), String>>)]) {
    for (name, result) in probes {
        match result {
            Some(Ok(())) => section.add(Level::Ok, format!("{name} starts")),
            Some(Err(why)) => section.add(Level::Warn, format!("{name} fails to start: {why}")),
            None => section.add(Level::Warn, format!("{name} not on PATH")),
        }
    }
}

/// The model an agent string names, or the string itself when it names none.
fn specs_model(specs: &[HarnessSpec], agent: &str) -> String {
    match AgentRef::parse(agent, specs) {
        Ok(named) => named.model.unwrap_or_else(|| named.program.clone()),
        Err(_) => agent.to_owned(),
    }
}

/// Whether the reviewer's model can read images, which decides whether image
/// proof can be judged by it.
fn image_line(section: &mut Section, model: &str) {
    match model_reads_images(model) {
        Some(true) => section.add(Level::Ok, format!("reviewer model {model} can read images")),
        Some(false) => section.add(
            Level::Warn,
            format!(
                "reviewer model {model} cannot read images: a review with image proof is degraded and its acceptance lines stay yours to check"
            ),
        ),
        None => section.add(
            Level::Warn,
            format!("cannot tell whether reviewer model {model} reads images"),
        ),
    }
}

/// Section 6: what quota each agent reports, never naming a credential.
fn quota_section(report: &mut Report, facts: &Facts, specs: &[HarnessSpec], roles: &Roles) {
    let section = report.section("Quota");
    let Some(home) = facts.home.clone() else {
        section.add(
            Level::Warn,
            "no home directory, so no credential store to probe",
        );
        return;
    };
    let mut ctx = ProbeContext::new(home.clone());
    ctx.free_pin = free_pin(roles, &home);
    for agent in ["claude", "codex", "kilo"] {
        if !specs.iter().any(|spec| spec.id == agent) {
            section.add(Level::Info, format!("{agent}  not in the harness table"));
            continue;
        }
        let in_role = fills_a_role(roles, specs, agent);
        let (level, text) = quota_line(&quota::probe(agent, &ctx), in_role);
        section.add(level, text);
    }
}

/// Whether kilo runs on its free tier: shprd's own pick, else kilo's default.
///
/// A configured free model wins, because it is the one a dispatch would use. A
/// machine with no kilo pick at all still counts when kilo's own config names
/// a free router: the paid balance is zero there and does not bind.
fn free_pin(roles: &Roles, home: &Path) -> bool {
    if configured_free_pin(roles) {
        return true;
    }
    quota::kilo::free_pin(quota::kilo::default_model(home).as_deref())
}

/// Whether a configured kilo agent string pins the free tier.
fn configured_free_pin(roles: &Roles) -> bool {
    roles
        .every_agent()
        .any(|agent| agent.starts_with("kilo/") && quota::kilo::free_pin(Some(agent.as_str())))
}

/// Whether `agent`'s program fills at least one role.
///
/// Every configured string is parsed against the harness table, so a bare
/// `kilo` in a role counts exactly like `kilo/kilo-auto/free` does.
fn fills_a_role(roles: &Roles, specs: &[HarnessSpec], agent: &str) -> bool {
    roles.every_agent().any(|pinned| {
        AgentRef::parse(pinned, specs)
            .map(|named| named.program == agent)
            .unwrap_or(false)
    })
}

/// One quota report as a level and a line: state, binding window, reset.
///
/// A dry or unknown agent fails the verb only when a role actually dispatches
/// it; an agent nothing uses is a warning, so nobody is blocked by a probe for
/// a program they never run.
fn quota_line(report: &QuotaReport, in_role: bool) -> (Level, String) {
    let (level, state) = match (report.state, in_role) {
        (QuotaState::Ok, _) => (Level::Ok, "ok"),
        (QuotaState::Dry, true) => (Level::Fail, "dry"),
        (QuotaState::Dry, false) => (Level::Warn, "dry"),
        (QuotaState::Unknown, true) => (Level::Fail, "unknown"),
        (QuotaState::Unknown, false) => (Level::Warn, "unknown"),
    };
    let mut text = format!("{}  {state}", report.agent);
    match report.windows.first() {
        Some(window) => text.push_str(&format!(
            "  {} {:.0}% used",
            window.name, window.used_percent
        )),
        None => text.push_str("  no window reported"),
    }
    if let Some(reset) = report.reset_at_ms {
        text.push_str(&format!("  reset {} UTC", clock(reset)));
    }
    if report.state == QuotaState::Unknown {
        text.push_str(&format!("  ({})", report.detail));
    }
    (level, text)
}

/// A reset instant as `HH:MM` in UTC, so the line never needs a locale or a
/// timezone crate. The line labels it, so the reader is never misled.
fn clock(ms: u64) -> String {
    let of_day = (ms / 1000) % 86_400;
    format!("{:02}:{:02}", of_day / 3600, (of_day % 3600) / 60)
}

/// Section 7: the skills each installed CLI sees, and the curated review.
fn skills_section(
    report: &mut Report,
    facts: &Facts,
    specs: &[HarnessSpec],
    clean: &LaunchEnv,
    detections: &[Detection],
) {
    let section = report.section("Skills");
    let Some(home) = facts.home.as_ref() else {
        section.add(Level::Warn, "no home directory, so no skill was scanned");
        return;
    };
    let (Ok(compat), Ok(rules)) = (facts.data.compat(), facts.data.visibility()) else {
        section.add(Level::Warn, "the skill tables do not load");
        return;
    };
    let ignore = rules.ignore_patterns();
    let clean_env = clean.apply_to(&facts.current);
    let lock = skills::read_lock(home).unwrap_or_default();
    for spec in specs {
        if !detections
            .iter()
            .any(|found| found.id == spec.id && found.binary.is_some())
        {
            continue;
        }
        let seen = skills::visible(
            spec,
            home,
            facts.repo.as_deref(),
            &clean_env,
            &rules,
            &ignore,
        );
        section.add(
            Level::Ok,
            format!("{}  {} skill(s) visible", spec.id, seen.skills.len()),
        );
        hidden_now(section, &spec.id, &rules, &facts.current, &clean_env);
        for junk in top_junk(&seen.junk) {
            section.add(
                Level::Warn,
                format!("{}  junk folder {}", spec.id, tilde(junk, home)),
            );
        }
    }
    compat_review(section, &compat, &lock);
}

/// Warns for each variable that hides a directory from this shell but not
/// from a launch.
fn hidden_now(
    section: &mut Section,
    cli: &str,
    rules: &VisibilityRules,
    current: &BTreeMap<String, String>,
    clean_env: &BTreeMap<String, String>,
) {
    let still_shown = rules.hidden(cli, clean_env);
    for (dir, var) in rules.hidden(cli, current) {
        if still_shown.iter().any(|(shown, _)| *shown == dir) {
            continue;
        }
        section.add(
            Level::Warn,
            format!("{var} hides {dir} from this shell; unset {var} to see it"),
        );
    }
}

/// Warns about skills the curated list wants looked at.
fn compat_review(
    section: &mut Section,
    compat: &CompatList,
    lock: &BTreeMap<String, skills::LockEntry>,
) {
    for row in skills::review(compat, lock) {
        match &row.state {
            ReviewState::Reviewed => {}
            ReviewState::ChangedUpstream { reviewed, current } => section.add(
                Level::Warn,
                format!(
                    "{}  changed upstream ({reviewed} -> {current}): re-review",
                    row.name
                ),
            ),
            ReviewState::NotInCompatList => section.add(
                Level::Warn,
                format!("{}  installed but not in compat.toml", row.name),
            ),
            ReviewState::MissingLocally => {}
        }
        if row.verdict == Some(Verdict::Deny) && row.state != ReviewState::MissingLocally {
            section.add(
                Level::Info,
                format!("{}  denied by compat.toml but installed", row.name),
            );
        }
    }
}

/// Section 8: the variables a worker launch drops, by name only.
///
/// Values are never printed: the variables dropped include credentials.
fn launch_env_section(report: &mut Report, facts: &Facts, clean: &LaunchEnv) {
    let section = report.section("Launch env");
    let dropped: Vec<String> = clean
        .removed_names()
        .into_iter()
        .filter(|name| facts.current.contains_key(name))
        .collect();
    if dropped.is_empty() {
        section.add(
            Level::Ok,
            "no variable a worker launch drops is set in this shell",
        );
    } else {
        section.add(
            Level::Info,
            format!("worker launches drop {}", dropped.join(", ")),
        );
    }
}

/// The junk entries that are not inside another junk entry.
///
/// The scanner reports an ignored folder and everything under it; a report
/// wants the folder once, so a nested path is dropped.
fn top_junk(junk: &[PathBuf]) -> Vec<&Path> {
    junk.iter()
        .map(PathBuf::as_path)
        .filter(|path| {
            !junk
                .iter()
                .any(|other| other.as_path() != *path && path.starts_with(other))
        })
        .collect()
}

/// A path spelled with `~` for the home directory, so a line stays short.
fn tilde(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use shprd_agents::quota::Tier;

    /// A report in `state`, as a probe would return it.
    fn report(agent: &str, state: QuotaState) -> QuotaReport {
        QuotaReport {
            agent: agent.to_owned(),
            state,
            tier: Tier::Measured,
            windows: Vec::new(),
            reset_at_ms: None,
            source: "kilo-profile".to_owned(),
            detail: "a zero balance".to_owned(),
            checked_at_ms: 0,
        }
    }

    /// A config whose roles are the given agents.
    fn config(orchestrator: Option<&str>, workers: &[&str]) -> Config {
        let mut config = Config::default();
        config.roster.orchestrator = orchestrator.map(str::to_owned);
        config.roster.workers = workers.iter().map(|agent| (*agent).to_owned()).collect();
        config
    }

    #[test]
    fn a_program_that_fails_to_start_is_named_with_its_error() {
        let mut page = Report::default();
        preflight_lines(
            page.section("Preflight"),
            &[
                ("node".to_owned(), Some(Ok(()))),
                (
                    "claude".to_owned(),
                    Some(Err("exited 134: dyld: Library not loaded".to_owned())),
                ),
            ],
        );
        let text = render(&page);
        assert!(text.contains("✓ node starts"), "{text}");
        assert!(
            text.contains("! claude fails to start: exited 134: dyld"),
            "{text}"
        );
        assert!(!page.has_fail());
    }

    #[test]
    fn preflight_runs_node_and_names_its_first_error_line() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap_or_else(|err| panic!("{err}"));
        let node = dir.path().join("node");
        std::fs::write(
            &node,
            "#!/bin/sh\necho 'dyld: Library not loaded: libnode' >&2\nexit 134\n",
        )
        .unwrap_or_else(|err| panic!("{err}"));
        std::fs::set_permissions(&node, std::fs::Permissions::from_mode(0o755))
            .unwrap_or_else(|err| panic!("{err}"));
        let path_env = dir.path().display().to_string();
        let probes = preflight_probes(&path_env, &[], &[], &[]);
        let [(name, Some(Err(why)))] = probes.as_slice() else {
            panic!("node must be probed and fail: {probes:?}");
        };
        assert_eq!(name, "node");
        assert!(why.contains("Library not loaded"), "{why}");
        let none = preflight_probes("", &[], &[], &[]);
        assert!(matches!(none.as_slice(), [(_, None)]), "{none:?}");
    }

    #[test]
    fn run_start_warns_of_a_failing_cli_and_a_blind_reviewer() {
        let mut page = Report::default();
        preflight_lines(
            page.section("Preflight"),
            &[
                ("node".to_owned(), Some(Ok(()))),
                (
                    "claude".to_owned(),
                    Some(Err("dyld: Library not loaded".to_owned())),
                ),
            ],
        );
        image_line(page.section("Review"), "deepseek-v4");
        let warnings = warnings_of(&page);
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(
            warnings[0].contains("claude fails to start"),
            "{warnings:?}"
        );
        assert!(warnings[1].contains("cannot read images"), "{warnings:?}");
    }

    #[test]
    fn a_blind_reviewer_model_is_said_to_degrade_image_review() {
        let mut page = Report::default();
        image_line(page.section("Review"), "deepseek-v4");
        let text = render(&page);
        assert!(text.contains("cannot read images"), "{text}");
        assert!(text.contains("degraded"), "{text}");
        let mut page = Report::default();
        image_line(page.section("Review"), "claude-opus");
        assert!(render(&page).contains("can read images"));
    }

    #[test]
    fn a_worker_whose_vendor_cannot_be_checked_is_reported() {
        let specs = harness_specs();
        let mut config = config(None, &["kilo", "gemini/pro"]);
        config.roster.reviewer = Some("claude/opus".to_owned());
        let mut page = Report::default();
        review(
            &mut page,
            &Facts::resolve(None),
            &specs,
            &Roles::from_roster(Some(&config.roster)),
        );
        let text = render(&page);
        assert!(text.contains("cannot check vendor of gemini/pro"), "{text}");
        assert!(!text.contains("independent of every worker"), "{text}");
    }

    #[test]
    fn a_reviewer_whose_vendor_cannot_be_checked_is_reported() {
        let specs = harness_specs();
        let mut config = config(None, &["claude/opus"]);
        config.roster.reviewer = Some("gemini/pro".to_owned());
        let mut page = Report::default();
        review(
            &mut page,
            &Facts::resolve(None),
            &specs,
            &Roles::from_roster(Some(&config.roster)),
        );
        let text = render(&page);
        assert!(text.contains("cannot check vendor of gemini/pro"), "{text}");
        assert!(!text.contains("independent of every worker"), "{text}");
    }

    #[test]
    fn a_pi_reviewer_on_claude_is_independent_of_a_pi_worker_on_deepseek() {
        // The CLI is the same on both sides; the model families are not.
        let specs = harness_specs();
        let mut config = config(None, &["pi/Command Code/deepseek/deepseek-v4.1-flash"]);
        config.roster.reviewer = Some("pi/openrouter/anthropic/claude-opus-5-5".to_owned());
        let mut page = Report::default();
        review(
            &mut page,
            &Facts::resolve(None),
            &specs,
            &Roles::from_roster(Some(&config.roster)),
        );
        let text = render(&page);
        assert!(
            text.contains("runs the anthropic family, independent of every worker"),
            "{text}"
        );
    }

    #[test]
    fn a_reviewer_from_the_workers_model_family_fails_the_section() {
        // A pi worker on Claude and Claude Code are the same family, so the
        // section fails even though the two CLIs differ.
        let specs = harness_specs();
        let mut config = config(None, &["pi/claude/claude-opus-5-5"]);
        config.roster.reviewer = Some("claude/claude-opus-5-5".to_owned());
        let mut page = Report::default();
        review(
            &mut page,
            &Facts::resolve(None),
            &specs,
            &Roles::from_roster(Some(&config.roster)),
        );
        let text = render(&page);
        assert!(
            text.contains("✗ reviewer claude/claude-opus-5-5 runs the anthropic family"),
            "{text}"
        );
        assert!(
            text.contains("the same as worker(s) pi/claude/claude-opus-5-5"),
            "{text}"
        );
        assert!(!text.contains("independent of every worker"), "{text}");
    }

    #[test]
    fn a_worker_on_a_router_is_reported_and_never_called_independent() {
        let specs = harness_specs();
        let mut config = config(None, &["kilo/kilo-auto/free"]);
        config.roster.reviewer = Some("codex/gpt-5-codex".to_owned());
        let mut page = Report::default();
        review(
            &mut page,
            &Facts::resolve(None),
            &specs,
            &Roles::from_roster(Some(&config.roster)),
        );
        let text = render(&page);
        assert!(
            text.contains("! cannot check vendor of kilo/kilo-auto/free"),
            "{text}"
        );
        assert!(text.contains("names no known model family"), "{text}");
        assert!(!text.contains("independent of every worker"), "{text}");
    }

    /// The shipped harness table, which names every program.
    fn harness_specs() -> Vec<HarnessSpec> {
        match DataDir::resolve(None, None).harness_specs() {
            Ok(specs) => specs,
            Err(error) => panic!("the shipped harness table must load: {error}"),
        }
    }

    /// A report holding only this one quota line, and the level it carries.
    fn line_level(config: Option<&Config>, agent: &str, state: QuotaState) -> (Level, bool) {
        let specs = harness_specs();
        let roles = Roles::from_roster(config.map(|config| &config.roster));
        let in_role = fills_a_role(&roles, &specs, agent);
        let (level, text) = quota_line(&report(agent, state), in_role);
        let mut report = Report::default();
        report.section("Quota").add(level, text);
        (level, report.has_fail())
    }

    /// A Config section read from a temp home holding `text` as its config.
    fn config_section_text(text: &str) -> String {
        let home = match TempDir::new() {
            Ok(home) => home,
            Err(error) => panic!("tempdir failed: {error}"),
        };
        let dir = home.path().join(".config").join("shprd");
        if let Err(error) = std::fs::create_dir_all(&dir) {
            panic!("creating {} failed: {error}", dir.display());
        }
        if let Err(error) = std::fs::write(dir.join("config.toml"), text) {
            panic!("writing the config failed: {error}");
        }
        let facts = Facts {
            current: BTreeMap::new(),
            path_env: String::new(),
            home: Some(home.path().to_path_buf()),
            repo: None,
            data: DataDir::resolve(None, None),
        };
        let mut report = Report::default();
        config_section(&mut report, &facts);
        render(&report)
    }

    #[test]
    fn an_auto_approving_orchestrator_is_named_in_the_config_section() {
        let text = config_section_text(
            "[roster]\norchestrator = \"claude/opus\"\norchestrator_auto_approve = true\n",
        );
        assert!(
            text.contains("! orchestrator runs with auto-approve: the PR gate relies on its brief"),
            "{text}"
        );
    }

    #[test]
    fn an_orchestrator_with_prompts_has_no_auto_approve_line() {
        let text = config_section_text("[roster]\norchestrator = \"claude/opus\"\n");
        assert!(!text.contains("auto-approve"), "{text}");
    }

    #[test]
    fn a_dry_agent_no_role_uses_is_a_warning_and_the_verb_succeeds() {
        let (level, fails) = line_level(None, "kilo", QuotaState::Dry);
        assert_eq!(level, Level::Warn);
        assert_eq!(level.glyph(), '!');
        assert!(!fails, "an unused dry agent must not fail the verb");
    }

    #[test]
    fn a_dry_agent_a_worker_uses_fails_the_verb() {
        let config = config(Some("claude/opus"), &["kilo/kilo-auto/free"]);
        let (level, fails) = line_level(Some(&config), "kilo", QuotaState::Dry);
        assert_eq!(level, Level::Fail);
        assert_eq!(level.glyph(), '✗');
        assert!(fails, "a dry worker must fail the verb");
    }

    #[test]
    fn an_unknown_agent_outside_every_role_only_warns() {
        let config = config(Some("claude/opus"), &["claude/opus"]);
        let (level, fails) = line_level(Some(&config), "codex", QuotaState::Unknown);
        assert_eq!(level, Level::Warn);
        assert!(!fails);
    }

    #[test]
    fn an_ok_agent_is_ok_whatever_the_roster_says() {
        assert_eq!(
            line_level(None, "codex", QuotaState::Ok),
            (Level::Ok, false)
        );
        let config = config(None, &["codex/gpt-6"]);
        assert_eq!(
            line_level(Some(&config), "codex", QuotaState::Ok),
            (Level::Ok, false)
        );
    }

    #[test]
    fn a_bare_agent_name_a_worker_uses_is_still_in_a_role() {
        let config = config(Some("claude/opus"), &["kilo"]);
        let (level, fails) = line_level(Some(&config), "kilo", QuotaState::Dry);
        assert_eq!(level, Level::Fail);
        assert_eq!(level.glyph(), '✗');
        assert!(fails, "a dry agent a bare-name role dispatches must fail");
    }

    #[test]
    fn a_free_pin_is_taken_from_kilos_own_config_when_shprd_names_none() {
        let home = home_with_kilo(Some(
            "{\n  // picked by hand\n  \"model\": \"ohmyproxy/kilo-auto/free\",\n}\n",
        ));
        assert!(free_pin(&Roles::default(), home.path()));

        let paid = home_with_kilo(Some("{\"model\": \"kilo/openai/gpt-5\"}"));
        assert!(!free_pin(&Roles::default(), paid.path()));
    }

    #[test]
    fn a_model_merely_named_free_is_not_the_free_tier() {
        let paid = config(None, &["kilo/vendor/free-lunch"]);
        assert!(!configured_free_pin(&Roles::from_roster(Some(
            &paid.roster
        ))));
        let free = config(None, &["kilo/kilo/kilo-auto/free"]);
        assert!(configured_free_pin(&Roles::from_roster(Some(&free.roster))));
    }

    #[test]
    fn a_configured_free_pin_wins_over_kilos_own_model() {
        let home = home_with_kilo(Some("{\"model\": \"kilo/openai/gpt-5\"}"));
        let config = config(None, &["kilo/kilo-auto/free"]);
        assert!(free_pin(
            &Roles::from_roster(Some(&config.roster)),
            home.path()
        ));
    }

    /// A temp home, holding a kilo jsonc with `body` when one is given.
    fn home_with_kilo(body: Option<&str>) -> TempDir {
        let home = match TempDir::new() {
            Ok(home) => home,
            Err(error) => panic!("temp home failed: {error}"),
        };
        if let Some(body) = body {
            let dir = home.path().join(".config").join("kilo");
            assert!(std::fs::create_dir_all(&dir).is_ok());
            assert!(std::fs::write(dir.join("kilo.jsonc"), body).is_ok());
        }
        home
    }

    /// A scripted `gh`: answers by account (the token it is handed), so a test
    /// states which account can do what.
    struct ScriptGh {
        status: &'static str,
        /// (account, `repo view` stdout or stderr, ok)
        views: Vec<(&'static str, &'static str, bool)>,
    }

    impl Gh for ScriptGh {
        fn run(&self, args: &[&str], token: Option<&str>) -> Result<GhOut, String> {
            let out = |ok: bool, stdout: &str, stderr: &str| {
                Ok(GhOut {
                    ok,
                    stdout: stdout.to_string(),
                    stderr: stderr.to_string(),
                })
            };
            match args {
                ["auth", "status"] => out(true, self.status, ""),
                ["auth", "token", "-u", name] => out(true, &format!("tok-{name}"), ""),
                ["repo", "view", ..] => {
                    let who = token.map_or("alice", |t| t.trim_start_matches("tok-"));
                    match self.views.iter().find(|(name, _, _)| *name == who) {
                        Some((_, text, true)) => out(true, text, ""),
                        Some((_, text, false)) => out(false, "", text),
                        None => out(false, "", "no such account"),
                    }
                }
                _ => out(false, "", "unexpected"),
            }
        }
    }

    const TWO: &str = "github.com\n  \u{2713} Logged in to github.com account alice (keyring)\n  - Active account: true\n\n  \u{2713} Logged in to github.com account bob (keyring)\n  - Active account: false\n";

    fn forge_text(gh: &ScriptGh) -> (String, bool) {
        let facts = Facts {
            current: BTreeMap::new(),
            path_env: String::new(),
            home: None,
            repo: Some(PathBuf::from("/nonexistent-repo")),
            data: DataDir::resolve(None, None),
        };
        let mut report = Report::default();
        forge_section(&mut report, &facts, gh);
        (render(&report), report.has_fail())
    }

    #[test]
    fn the_doctor_names_the_active_account_and_the_one_that_can_write() {
        let gh = ScriptGh {
            status: TWO,
            views: vec![
                (
                    "alice",
                    "{\"nameWithOwner\":\"o/r\",\"viewerPermission\":\"READ\"}",
                    true,
                ),
                (
                    "bob",
                    "{\"nameWithOwner\":\"o/r\",\"viewerPermission\":\"WRITE\"}",
                    true,
                ),
            ],
        };
        let (text, failed) = forge_text(&gh);
        assert!(failed, "{text}");
        assert!(text.contains("alice (active)"), "{text}");
        assert!(
            text.contains("alice (the active account) has only READ"),
            "{text}"
        );
        assert!(text.contains("logged-in account bob can"), "{text}");
        assert!(text.contains("tasks.gh_user bob"), "{text}");
        assert!(!text.contains("tok-"), "no token is printed: {text}");
    }

    #[test]
    fn the_doctor_flags_the_wrong_active_account_and_names_the_right_one() {
        // The 2026-11-23 incident: the active account cannot see the repository
        // ("Could not resolve to a Repository"), another logged-in one can write.
        let gh = ScriptGh {
            status: TWO,
            views: vec![
                (
                    "alice",
                    "GraphQL: Could not resolve to a Repository with the name 'o/r'.",
                    false,
                ),
                (
                    "bob",
                    "{\"nameWithOwner\":\"o/r\",\"viewerPermission\":\"WRITE\"}",
                    true,
                ),
            ],
        };
        let (text, failed) = forge_text(&gh);
        assert!(failed, "{text}");
        assert!(
            text.contains("alice (the active account) cannot see this repository's remote"),
            "{text}"
        );
        assert!(text.contains("tasks.gh_user bob"), "{text}");
    }

    #[test]
    fn the_doctor_shows_what_github_said_when_the_repo_is_hidden() {
        let gh = ScriptGh {
            status: TWO,
            views: vec![
                (
                    "alice",
                    "GraphQL: Could not resolve to a Repository with the name 'o/r'.",
                    false,
                ),
                (
                    "bob",
                    "GraphQL: Could not resolve to a Repository with the name 'o/r'.",
                    false,
                ),
            ],
        };
        let (text, failed) = forge_text(&gh);
        assert!(failed, "{text}");
        assert!(text.contains("Could not resolve to a Repository"), "{text}");
        assert!(
            text.contains("no other logged-in account can write"),
            "{text}"
        );
    }

    #[test]
    fn an_account_with_write_access_passes() {
        let gh = ScriptGh {
            status: TWO,
            views: vec![("alice", "{\"viewerPermission\":\"ADMIN\"}", true)],
        };
        let (text, failed) = forge_text(&gh);
        assert!(!failed, "{text}");
        assert!(text.contains("has write access"), "{text}");
    }

    /// The contradicted-stop count shows on the page, warning when non-zero.
    #[test]
    fn the_contradicted_stop_count_is_shown() {
        assert_eq!(contradicted_stops_line(0).0, Level::Ok);
        let (level, text) = contradicted_stops_line(3);
        assert_eq!(level, Level::Warn);
        assert!(text.contains('3'));
    }
}
