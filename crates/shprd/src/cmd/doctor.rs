//! `shprd doctor`: what works on this machine, and what to fix.
//!
//! A read-only page of sections: the host shprd runs in, the harness data it
//! loads, the config, the installed agents, their quota, and the skills they
//! see. A probe that fails becomes a line rather than an error, so one run
//! shows every problem. The verb exits 1 when any line is a [`Level::Fail`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::anyhow;
use shprd_core::config::{self, Config};
use shprd_roster::detect::{Detection, SignIn, detect_all, find_binary, vendor_name};
use shprd_roster::env::{self, EnvContext, LaunchEnv, Role};
use shprd_roster::skills::{CompatList, VisibilityRules};
use shprd_roster::spec::{HarnessSpec, Vendor, load_dir};

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
    let report = build(home);
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
fn build(home_flag: Option<&Path>) -> Report {
    let facts = Facts::resolve(home_flag);
    let mut report = Report::default();
    host(&mut report, &facts);
    let specs = data(&mut report, &facts);
    let config = config_section(&mut report, &facts);
    agents(&mut report, &facts, &specs, config.as_ref());
    review(&mut report, &facts, &specs, config.as_ref());
    report
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
    /// The data directory holding `harness/` and `skills/`.
    data: Option<PathBuf>,
}

impl Facts {
    /// Collects the environment, home, repo and data directory.
    fn resolve(home_flag: Option<&Path>) -> Self {
        Self {
            current: std::env::vars().collect(),
            path_env: std::env::var("PATH").unwrap_or_default(),
            home: resolve_home(home_flag).ok(),
            repo: crate::ctx::resolve_root(None).ok(),
            data: crate::cmd::roster::resolve_data_dir().ok(),
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

/// Section 1: the multiplexer we are inside and the binaries we would drive.
fn host(report: &mut Report, facts: &Facts) {
    let section = report.section("Host");
    if marked(&facts.current, "TUIOS_ENV") {
        section.add(Level::Ok, "inside tuios");
    } else if marked(&facts.current, "HERDR_ENV") {
        section.add(Level::Ok, "inside herdr");
    } else {
        section.add(Level::Warn, "not inside herdr or tuios");
    }
    for binary in ["herdr", "tuios"] {
        match find_binary(binary, &facts.path_env) {
            Some(path) => section.add(Level::Ok, format!("{binary} {}", path.display())),
            None => section.add(Level::Warn, format!("{binary} not on PATH")),
        }
    }
}

/// Section 2: the harness table and the curated skills tables in `data/`.
///
/// Returns the specs it loaded, so the agent section does not load them again.
fn data(report: &mut Report, facts: &Facts) -> Vec<HarnessSpec> {
    let section = report.section("Data");
    let Some(dir) = &facts.data else {
        section.add(
            Level::Fail,
            "no data directory: set SHPRD_DATA_DIR to a directory holding `harness/`",
        );
        return Vec::new();
    };
    section.add(Level::Info, format!("data directory {}", dir.display()));
    let specs = match load_dir(&dir.join("harness")) {
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
    let skills_dir = dir.join("skills");
    match CompatList::load(&skills_dir.join("compat.toml")) {
        Ok(list) => section.add(
            Level::Ok,
            format!("compat.toml: {} entries", list.skills.len()),
        ),
        Err(err) => section.add(Level::Fail, format!("compat.toml does not load: {err}")),
    }
    match VisibilityRules::load(&skills_dir.join("visibility.toml")) {
        Ok(rules) => section.add(
            Level::Ok,
            format!("visibility.toml: {} rule(s)", rules.rules.len()),
        ),
        Err(err) => section.add(Level::Fail, format!("visibility.toml does not load: {err}")),
    }
    specs
}

/// Section 3: the layered config and the three role agent strings.
fn config_section(report: &mut Report, facts: &Facts) -> Option<Config> {
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
            return None;
        }
    };
    match (&global, repo_layer.as_deref()) {
        (Some(global), Some(repo)) => section.add(
            Level::Ok,
            format!(
                "loads and validates ({} and {})",
                global.display(),
                repo.display()
            ),
        ),
        _ => section.add(Level::Ok, "loads and validates"),
    }
    let roster = &loaded.roster;
    role(section, "orchestrator", roster.orchestrator.as_deref());
    role(section, "reviewer", roster.reviewer.as_deref());
    if roster.workers.is_empty() {
        section.add(Level::Warn, "workers not set");
    } else {
        section.add(Level::Ok, format!("workers {}", roster.workers.join(", ")));
    }
    Some(loaded)
}

/// Adds one role line: the agent string, or a warning that it is unset.
fn role(section: &mut Section, name: &str, agent: Option<&str>) {
    match agent {
        Some(agent) if !agent.trim().is_empty() => {
            section.add(Level::Ok, format!("{name} {agent}"));
        }
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
    };
    env::launch_env(&facts.current, Role::Worker, &ctx)
}

/// The model the pi sign-in probe needs: from the roster, else the environment.
fn pi_models(
    facts: &Facts,
    specs: &[HarnessSpec],
    config: Option<&Config>,
) -> BTreeMap<String, String> {
    if !specs.iter().any(|spec| spec.id == "pi") {
        return BTreeMap::new();
    }
    let roster = config.map(|config| &config.roster);
    let from_roster = roster.and_then(|roster| {
        roster
            .workers
            .iter()
            .chain(roster.orchestrator.iter())
            .chain(roster.reviewer.iter())
            .find_map(|agent| model_after_pi(agent))
    });
    let model = from_roster.or_else(|| {
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

/// The model of an agent string of the form `pi/<model>`, when it is one.
fn model_after_pi(agent: &str) -> Option<String> {
    let rest = agent.strip_prefix("pi/")?.trim();
    (!rest.is_empty()).then(|| rest.to_owned())
}

/// Section 4: every harness CLI, probed with the environment a worker gets.
fn agents(report: &mut Report, facts: &Facts, specs: &[HarnessSpec], config: Option<&Config>) {
    let section = report.section("Agents");
    if specs.is_empty() {
        section.add(Level::Warn, "no harness spec, so no agent could be probed");
        return;
    }
    let clean = worker_env(facts);
    let pairs = clean.as_pairs();
    let models = pi_models(facts, specs, config);
    for detection in detect_all(specs, &facts.path_env, &pairs, &models) {
        let (level, text) = describe(&detection);
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
        SignIn::Unknown(reason) => (level, format!("unknown ({reason})")),
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

/// The vendor of the harness an agent string names, by its program prefix.
fn vendor_of(specs: &[HarnessSpec], agent: &str) -> Option<Vendor> {
    let program = agent.split('/').next()?.trim();
    specs
        .iter()
        .find(|spec| spec.id == program)
        .map(|spec| spec.vendor)
}

/// Section 5: whether the reviewer comes from another vendor than every worker.
fn review(report: &mut Report, _facts: &Facts, specs: &[HarnessSpec], config: Option<&Config>) {
    let section = report.section("Review");
    let Some(roster) = config.map(|config| &config.roster) else {
        section.add(Level::Warn, "no config loads, so no reviewer to check");
        return;
    };
    let reviewer = roster
        .reviewer
        .as_deref()
        .filter(|agent| !agent.trim().is_empty());
    let Some(reviewer) = reviewer else {
        section.add(Level::Warn, "reviewer not set, so no cross-vendor review");
        return;
    };
    let Some(vendor) = vendor_of(specs, reviewer) else {
        section.add(
            Level::Warn,
            format!("reviewer {reviewer}: no harness spec names its vendor"),
        );
        return;
    };
    let clashes: Vec<&str> = roster
        .workers
        .iter()
        .filter(|worker| vendor_of(specs, worker) == Some(vendor))
        .map(String::as_str)
        .collect();
    if clashes.is_empty() {
        section.add(
            Level::Ok,
            format!(
                "reviewer {reviewer} is {}, independent of every worker",
                vendor_name(vendor)
            ),
        );
    } else {
        section.add(
            Level::Fail,
            format!(
                "reviewer {reviewer} is {}: the same vendor as worker(s) {}",
                vendor_name(vendor),
                clashes.join(", ")
            ),
        );
    }
}
